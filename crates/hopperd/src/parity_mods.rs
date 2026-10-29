//! Routes added to bring the panel's mods area closer to Hopper desktop.
//!
//! - `POST /servers/{id}/mods/version-lock`: keeps one offered file out
//!   (desktop's "Lock 1.2.3 (keep 1.2.2)"): the update stays listed, but the
//!   table and the review leave it alone unless asked with Shift+click.
//! - `POST /servers/{id}/mods/orphans`: the mods nothing else needs once
//!   these go (desktop's "Also remove these mods? Nothing else needs them").
//! - `POST /servers/{id}/mods/undo-remove`: the Undo on "Removed X": puts the
//!   jars a remove moved to `.hopper-backups` back, with their entries.

use crate::App;
use crate::api::{ApiResult, bad, not_found, require};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method};
use axum::routing::post;
use axum::{Json, Router};
use hopper_core::instances::{Addon, Instance};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/servers/{id}/mods/version-lock", post(version_lock))
        .route("/api/v1/servers/{id}/mods/orphans", post(orphans))
        .route("/api/v1/servers/{id}/mods/undo-remove", post(undo_remove))
}

#[derive(Deserialize)]
struct VersionLockBody {
    /// The installed jar.
    file: String,
    /// The offered file's name to keep out, or none to lift the lock.
    #[serde(default)]
    version: Option<String>,
}

async fn version_lock(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<VersionLockBody>,
) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let root = e.root.clone();
    let found = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
        crate::mods_api::instance_of(&e)?;
        let want = b.file.trim_end_matches(".disabled").to_string();
        let mut found = false;
        Instance::update_on_disk(&root, |inst| {
            if let Some(a) = inst
                .addons
                .iter_mut()
                .find(|a| a.file_name.trim_end_matches(".disabled").eq_ignore_ascii_case(&want))
            {
                a.locked_version = b.version.clone().map(|v| v.trim().to_string()).unwrap_or_default();
                found = true;
            }
        })?;
        Ok(found)
    })
    .await
    .map_err(bad)?
    .map_err(|e| bad(format!("{e:#}")))?;
    if !found {
        return Err(not_found("mod"));
    }
    Ok(Json(json!({ "ok": true })))
}

fn same_file(a: &str, b: &str) -> bool {
    a.trim_end_matches(".disabled").eq_ignore_ascii_case(b.trim_end_matches(".disabled"))
}

#[derive(Deserialize)]
struct FilesBody {
    files: Vec<String>,
}

/// The mods nothing else needs once `files` go (libraries they pulled in).
async fn orphans(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<FilesBody>,
) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let inst = crate::mods_api::instance_of(&e)?;
        let files: Vec<String> = b
            .files
            .iter()
            .filter_map(|f| inst.addons.iter().find(|a| same_file(&a.file_name, f)).map(|a| a.file_name.clone()))
            .collect();
        let list: Vec<Value> = inst
            .orphans_of(&files)
            .into_iter()
            .filter(|a| a.folder() == "mods")
            .map(|a| {
                json!({
                    "file": a.file_name,
                    "name": if a.name.is_empty() { a.file_name.clone() } else { a.name.clone() },
                    "icon": a.icon,
                })
            })
            .collect();
        Ok(json!({ "mods": list }))
    })
    .await
    .map_err(bad)?
    .map_err(|e| bad(format!("{e:#}")))?;
    Ok(Json(v))
}

// ---- Undo after remove ------------------------------------------------------------

/// A removed jar: where it was, where its backup is, and its entry.
#[derive(Clone)]
struct Removed {
    original: PathBuf,
    backup: PathBuf,
    addon: Option<Addon>,
}

type Undo = (String, String, Vec<Removed>);

/// Removes that can still be undone: (token, server id, what went).
fn undos() -> std::sync::MutexGuard<'static, Vec<Undo>> {
    static U: Mutex<Vec<Undo>> = Mutex::new(Vec::new());
    U.lock().unwrap_or_else(|e| e.into_inner())
}

/// The newest backup of `original` made at `since` or later (backup
/// folders are named by the second; a name taken gets a "2-" prefix), as
/// desktop's find_backup.
fn find_backup(root: &FsPath, original: &FsPath, since: u64) -> Option<PathBuf> {
    hopper_core::backups::find(root, original, since)
}

/// Seconds now, less one (a backup folder made this tick may be named
/// for the second before).
pub(crate) fn undo_since() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        .saturating_sub(1)
}

/// After a remove: finds each moved jar's backup and keeps them for Undo.
/// `before` is each file's path before it went and its entry. The token,
/// or none when nothing can be undone (a jar the running server holds open
/// is only queued to go).
pub(crate) fn remember_removed(server: &str, root: &FsPath, before: Vec<(PathBuf, Addon)>, since: u64) -> Option<String> {
    let items: Vec<Removed> = before
        .into_iter()
        .filter(|(original, _)| !original.exists())
        .filter_map(|(original, addon)| {
            let backup = find_backup(root, &original, since)?;
            // A jar Hopper didn't know comes back as one it doesn't know.
            Some(Removed { original, backup, addon: (addon.project_id != 0).then_some(addon) })
        })
        .collect();
    if items.is_empty() {
        return None;
    }
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let token = format!("{since}-{}", N.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let mut u = undos();
    u.push((token.clone(), server.to_string(), items));
    // Only the last few: the toast that offers it goes in seconds.
    let extra = u.len().saturating_sub(16);
    u.drain(..extra);
    Some(token)
}

#[derive(Deserialize)]
struct UndoBody {
    token: String,
}

async fn undo_remove(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<UndoBody>,
) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let running = e.server.state().is_alive();
    let items = {
        let mut u = undos();
        let at = u
            .iter()
            .position(|(t, s, _)| *t == b.token && *s == id)
            .ok_or_else(|| bad("That remove can't be undone any more. Put the mods back from Mods › Backups."))?;
        u.remove(at).2
    };
    let root = e.root.clone();
    let back = tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
        let mut inst = crate::mods_api::instance_of(&e)?;
        let sets = hopper_core::backups::list(&root);
        let mut back = 0;
        for it in &items {
            // Put back some other way already: leave the backup alone.
            if it.original.exists() || !hopper_core::backups::exists(&it.backup) {
                continue;
            }
            // Kept as a note of where to get it: download it again first.
            hopper_core::backups::materialize(&it.backup)?;
            match sets.iter().flat_map(|s| s.files.iter()).find(|f| f.path == it.backup) {
                // Also tidies the emptied backup folder away.
                Some(f) if root.join(&f.restore_to) == it.original => {
                    hopper_core::backups::restore(&root, f)?;
                }
                _ => {
                    if let Some(p) = it.original.parent() {
                        std::fs::create_dir_all(p)?;
                    }
                    if std::fs::rename(&it.backup, &it.original).is_err() {
                        std::fs::copy(&it.backup, &it.original)?;
                        std::fs::remove_file(&it.backup)?;
                    }
                }
            }
            back += 1;
            if let Some(a) = &it.addon
                && !inst.addons.iter().any(|x| same_file(&x.file_name, &a.file_name))
            {
                inst.addons.push(a.clone());
            }
        }
        inst.save()?;
        Ok(back)
    })
    .await
    .map_err(bad)?
    .map_err(|e| bad(format!("{e:#}")))?;
    crate::mods_api::forget_updates(&id);
    Ok(Json(json!({ "restored": back, "restart_needed": running && back > 0 })))
}
