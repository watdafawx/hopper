//! The Mods tab's details and backups, like Hopper desktop's mod peek and
//! Backups view: a mod's versions and changelogs, changing or rolling back
//! its version, locking it, resetting its config, and the jars Hopper set
//! aside in `.hopper-backups` (by Hopper desktop or this panel).

use crate::App;
use crate::api::{ApiResult, bad, not_found, require, require_owner};
use crate::servers::Entry;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post};
use axum::{Json, Router};
use hopper_core::instances::Instance;
use hopper_core::{backups, install, modconfig, richtext, source};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/servers/{id}/mods/versions", get(versions))
        .route("/api/v1/servers/{id}/mods/changelog", get(changelog))
        .route("/api/v1/servers/{id}/mods/version", post(set_version))
        .route("/api/v1/servers/{id}/mods/lock", post(lock))
        .route("/api/v1/servers/{id}/mods/config", get(config_files))
        .route("/api/v1/servers/{id}/mods/config-reset", post(config_reset))
        .route("/api/v1/servers/{id}/mod-backups", get(backup_list))
        .route("/api/v1/servers/{id}/mod-backups/restore", post(backup_restore))
        .route("/api/v1/servers/{id}/mod-backups/delete", post(backup_delete))
        .route("/api/v1/servers/{id}/mod-backups/clean", post(backup_clean))
        .route("/api/v1/settings/backup-retention", get(get_retention).put(put_retention))
}

fn entry(app: &App, headers: &HeaderMap, method: &Method, id: &str) -> ApiResult<Arc<Entry>> {
    require(app, headers, method)?;
    app.servers.get(id).ok_or_else(|| not_found("server"))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(bad)?.map_err(|e| bad(format!("{e:#}")))
}

/// Mod ids go to the browser as strings (Modrinth's are 64-bit).
fn id_str(id: u64) -> String {
    id.to_string()
}

fn addon_index(inst: &Instance, file: &str) -> anyhow::Result<usize> {
    let f = file.trim_end_matches(".disabled");
    inst.addons
        .iter()
        .position(|a| a.file_name.trim_end_matches(".disabled").eq_ignore_ascii_case(f))
        .ok_or_else(|| anyhow::anyhow!("Hopper doesn't know where {file} is from yet. Identify it first."))
}

// ---- versions and changelogs --------------------------------------------------

/// A mod's files for this server's Minecraft version and loader, newest
/// first, marking the installed one.
async fn versions(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let file = q.get("file").cloned().unwrap_or_default();
    let v = blocking(move || {
        let inst = crate::mods_api::instance_of(&e)?;
        let a = inst.addons[addon_index(&inst, &file)?].clone();
        let files = source::files(a.project_id, &inst.game_version, install::loader_of(&inst))?;
        let rows: Vec<Value> = files
            .iter()
            .take(40)
            .map(|f| {
                json!({
                    "id": id_str(f.id), "name": f.display_name, "file_name": f.file_name, "date": f.file_date,
                    // Just the version ("1.1.0-beta.54"), as Hopper desktop shows it.
                    "version": hopper_core::scan::short_version(&f.file_name),
                    "type": match f.release_type { 2 => "beta", 3 => "alpha", _ => "release" },
                    "installed": f.id == a.file_id,
                })
            })
            .collect();
        Ok(json!({
            "project_id": id_str(a.project_id), "file_id": id_str(a.file_id), "locked": a.locked,
            "page": a.page, "name": a.name, "versions": rows,
        }))
    })
    .await?;
    Ok(Json(v))
}

/// One file's changelog, as Markdown.
async fn changelog(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    entry(&app, &headers, &method, &id)?;
    let project: u64 = q.get("project").and_then(|p| p.parse().ok()).ok_or_else(|| bad("Which mod?"))?;
    let file: u64 = q.get("file").and_then(|p| p.parse().ok()).ok_or_else(|| bad("Which file?"))?;
    let md = blocking(move || {
        let f = source::file(project, file)?;
        Ok(richtext::to_markdown(&source::changelog(&f)?))
    })
    .await?;
    Ok(Json(json!({ "markdown": md })))
}

#[derive(Deserialize)]
struct VersionBody {
    file: String,
    file_id: String,
}

/// Installs another version of a mod (an update or a roll back); the jar it
/// replaces goes to `.hopper-backups`.
async fn set_version(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<VersionBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let file_id: u64 = b.file_id.trim().parse().map_err(|_| bad("Which version?"))?;
    let job = crate::pack_api::spawn(&app, format!("Changing a mod on {}", e.name), move |job| {
        let mut inst = crate::mods_api::instance_of(&e)?;
        let a = inst.addons[addon_index(&inst, &b.file)?].clone();
        let name = install::install_file(&mut inst, a.project_id, file_id, job)?;
        log::info!("{}: {} is now {name}", e.name, a.name);
        Ok(json!({ "file": name }))
    });
    Ok(Json(json!({ "job": job })))
}

#[derive(Deserialize)]
struct LockBody {
    file: String,
    locked: bool,
}

/// Keeps a mod on its version: update checks skip it.
async fn lock(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<LockBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    blocking(move || {
        let inst = crate::mods_api::instance_of(&e)?;
        let i = addon_index(&inst, &b.file)?;
        Instance::update_on_disk(&e.root, |inst| {
            if let Some(a) = inst.addons.get_mut(i) {
                a.locked = b.locked;
            }
        })?;
        Ok(())
    })
    .await?;
    Ok(Json(json!({ "ok": true })))
}

// ---- config reset --------------------------------------------------------------

async fn config_files(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let file = q.get("file").cloned().unwrap_or_default();
    let files = blocking(move || {
        let inst = crate::mods_api::instance_of(&e)?;
        let a = inst.addons[addon_index(&inst, &file)?].clone();
        Ok(modconfig::files(&inst, &a))
    })
    .await?;
    Ok(Json(json!({ "files": files })))
}

#[derive(Deserialize)]
struct FileBody {
    file: String,
}

/// Moves a mod's config files to `.hopper-backups`; the mod writes fresh
/// ones on the next start.
async fn config_reset(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<FileBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let running = e.server.state().is_alive();
    let n = blocking(move || {
        let inst = crate::mods_api::instance_of(&e)?;
        let a = inst.addons[addon_index(&inst, &b.file)?].clone();
        let files = modconfig::files(&inst, &a);
        modconfig::reset(&e.root, &files)
    })
    .await?;
    Ok(Json(json!({ "reset": n, "restart_needed": running })))
}

// ---- .hopper-backups ----------------------------------------------------------------

async fn backup_list(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let (sets, usage) = blocking(move || {
        let r = hopper_core::retention::policy();
        Ok((backups::list(&e.root), (hopper_core::retention::usage(&e.root), hopper_core::retention::preview(&e.root), r)))
    })
    .await?;
    let (u, would, r) = usage;
    let total: u64 = sets.iter().map(|s| s.size()).sum();
    let rows: Vec<Value> = sets
        .iter()
        .map(|s| {
            json!({
                "when": s.when, "size": s.size(),
                "files": s.files.iter().map(|f| json!({ "name": f.name, "size": f.size, "restore_to": f.restore_to, "download": f.download })).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(Json(json!({
        "sets": rows, "total_size": total,
        // The whole folder (snapshots and world zips too) and the clean up rules.
        "usage": { "total": u.total(), "limited": u.limited(), "frees": would.freed, "removes": would.count(),
                   "keep_actions": r.keep_actions, "keep_days": r.keep_days, "max_mb": r.max_mb },
    })))
}

/// Where old backups go when they're cleaned up: the server's trash (Files
/// › Trash), which empties itself after its usual time.
pub fn to_server_trash(root: &std::path::Path, paths: &[std::path::PathBuf]) -> anyhow::Result<()> {
    for p in paths {
        let rel = p.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
        crate::files::trash(root, &rel)?;
    }
    Ok(())
}

/// How long every server's `.hopper-backups` keeps old files.
async fn get_retention(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    Ok(Json(json!(app.cfg.read().unwrap().backup_retention)))
}

async fn put_retention(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(r): Json<hopper_core::retention::Retention>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    {
        let mut cfg = app.cfg.write().unwrap();
        cfg.backup_retention = r;
        cfg.save(&app.data_dir).map_err(|e| bad(format!("{e:#}")))?;
    }
    // Applied from the next backup on; older ones wait for Clean up.
    hopper_core::retention::set_policy(r);
    Ok(Json(json!(r)))
}

/// Removes old backups now, by the panel's clean up rules.
async fn backup_clean(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let p = blocking(move || Ok(hopper_core::retention::clean_up(&e.root))).await?;
    Ok(Json(json!({ "removed": p.folders + p.snapshots, "freed": p.freed })))
}

#[derive(Deserialize)]
struct RestoreBody {
    when: i64,
    /// One file of the set; none: all of them.
    #[serde(default)]
    name: Option<String>,
}

/// Puts files back from a backup. A jar coming back to `mods/` first moves
/// the jar of the same mod that's there now into a new backup, so the
/// server never loads a mod twice and the restore can be undone the same
/// way.
async fn backup_restore(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<RestoreBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let running = e.server.state().is_alive();
    let done = blocking(move || {
        let set = backups::list(&e.root).into_iter().find(|s| s.when == b.when).ok_or_else(|| anyhow::anyhow!("That backup is gone."))?;
        let files: Vec<_> = set.files.iter().filter(|f| b.name.as_ref().is_none_or(|n| *n == f.name)).cloned().collect();
        anyhow::ensure!(!files.is_empty(), "Nothing to restore.");
        let mut inst = crate::mods_api::instance_of(&e)?;
        let mut out = Vec::new();
        for f in &files {
            // Kept as a note of where to get it: download it again first
            // (its mod ids are read from the jar).
            backups::materialize(&f.path)?;
            if f.restore_to.starts_with("mods/") && f.name.to_lowercase().contains(".jar") {
                let (ids, _) = hopper_core::deps::own_ids(&f.path);
                if !ids.is_empty() {
                    set_aside_same_mod(&mut inst, &ids)?;
                }
            }
            let to = backups::restore(&e.root, f)?;
            out.push(to.file_name().unwrap_or_default().to_string_lossy().into_owned());
        }
        Ok(out)
    })
    .await?;
    crate::mods_api::forget_updates(&id);
    Ok(Json(json!({ "restored": done, "restart_needed": running })))
}

/// Moves the jars in `mods/` that are the mod `ids` (by their mod ids) to
/// `.hopper-backups`.
fn set_aside_same_mod(inst: &mut Instance, ids: &[String]) -> anyhow::Result<()> {
    let dir = inst.mods_dir();
    let mut same = Vec::new();
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let lower = name.to_lowercase();
        if !(lower.ends_with(".jar") || lower.ends_with(".jar.disabled")) {
            continue;
        }
        let (own, _) = hopper_core::deps::own_ids(&e.path());
        if own.iter().any(|o| ids.contains(o)) {
            same.push(name);
        }
    }
    for name in same {
        match addon_index(inst, &name) {
            Ok(i) => install::remove(inst, i).map_err(install::explain_locked)?,
            Err(_) => install::remove_loose_file(inst, &name).map_err(install::explain_locked)?,
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct DeleteBody {
    when: i64,
}

/// Deletes a backup (to the Recycle Bin where there is one).
async fn backup_delete(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<DeleteBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    blocking(move || {
        let set = backups::list(&e.root).into_iter().find(|s| s.when == b.when).ok_or_else(|| anyhow::anyhow!("That backup is gone."))?;
        backups::delete(&set, false)
    })
    .await?;
    Ok(Json(json!({ "ok": true })))
}
