//! The Mods table's extras, as in Hopper desktop's Mods tab: every newer
//! version's changelog at once, the mods that need (or are needed by) the
//! ones being switched, bulk on / off / lock / remove, and switching a mod
//! between CurseForge and Modrinth.

use crate::App;
use crate::api::{ApiResult, bad, not_found, require};
use crate::servers::Entry;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post};
use axum::{Json, Router};
use hopper_core::instances::Instance;
use hopper_core::{install, richtext, source};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/servers/{id}/mods/changelogs", get(changelogs))
        .route("/api/v1/servers/{id}/mods/related", post(related))
        .route("/api/v1/servers/{id}/mods/toggle-many", post(toggle_many))
        .route("/api/v1/servers/{id}/mods/remove-many", post(remove_many))
        .route("/api/v1/servers/{id}/mods/lock-many", post(lock_many))
        .route("/api/v1/servers/{id}/mods/switch", post(switch))
}

fn entry(app: &App, headers: &HeaderMap, method: &Method, id: &str) -> ApiResult<Arc<Entry>> {
    require(app, headers, method)?;
    app.servers.get(id).ok_or_else(|| not_found("server"))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(bad)?.map_err(|e| bad(format!("{e:#}")))
}

fn same_file(a: &str, b: &str) -> bool {
    a.trim_end_matches(".disabled").eq_ignore_ascii_case(b.trim_end_matches(".disabled"))
}

// ---- categories ------------------------------------------------------------

static CATS_RUNNING: AtomicBool = AtomicBool::new(false);

/// The categories known for these addons, by file name. Ones not looked up
/// yet are fetched in the background (one lookup at a time); `true` when
/// that started, so the list asks again shortly.
pub fn categories(inst: &Instance) -> (HashMap<String, Vec<String>>, bool) {
    let known = hopper_core::modmeta::load();
    let mut out = HashMap::new();
    let mut want = Vec::new();
    for a in &inst.addons {
        let Some(k) = hopper_core::modmeta::key(a) else { continue };
        match known.get(&k) {
            Some(c) => {
                out.insert(a.file_name.to_lowercase(), c.clone());
            }
            None => want.push(k),
        }
    }
    want.sort_unstable();
    want.dedup();
    let started = !want.is_empty() && !CATS_RUNNING.swap(true, Ordering::SeqCst);
    if started {
        std::thread::spawn(move || {
            for chunk in want.chunks(200) {
                let found = hopper_core::modmeta::fetch(chunk);
                hopper_core::modmeta::save(&found);
            }
            CATS_RUNNING.store(false, Ordering::SeqCst);
        });
    }
    (out, started || CATS_RUNNING.load(Ordering::SeqCst))
}

// ---- versions and changelogs ------------------------------------------------

/// Every version after the installed one up to the update on offer, each
/// with its changelog (newest first, the installed one last), and the
/// other versions for this Minecraft and loader, to update or roll back to.
async fn changelogs(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let file = q.get("file").cloned().unwrap_or_default();
    let sid = id.clone();
    let v = blocking(move || {
        let inst = crate::mods_api::instance_of(&e)?;
        let a = inst
            .addons
            .iter()
            .find(|a| same_file(&a.file_name, &file))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Hopper doesn't know where {file} is from yet. Identify it first."))?;
        anyhow::ensure!(a.project_id != 0, "Hopper doesn't know where {file} is from yet. Identify it first.");
        // The update on offer, from the last check.
        let offer: Option<u64> = crate::mods_api::cached_updates(&sid).and_then(|ups| {
            ups.iter()
                .find(|u| u["project_id"].as_str() == Some(a.project_id.to_string().as_str()))
                .and_then(|u| u["file_id"].as_str()?.parse().ok())
        });
        let update = offer.and_then(|f| source::file(a.project_id, f).ok());
        let (entries, mut all) = install::changelogs(&inst, a.project_id, a.file_id, update.as_ref(), false)?;
        all.retain(|f| f.is_server_pack != Some(true));
        let installed_date = all.iter().find(|f| f.id == a.file_id).map(|f| f.file_date.clone()).unwrap_or_default();
        let shown: HashSet<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        let mut md_seen: Vec<(String, String)> = Vec::new();
        let rows: Vec<Value> = entries
            .iter()
            .map(|en| {
                let md = richtext::to_markdown(&en.blocks);
                // The same text as a newer card: say so, don't repeat it.
                let same = (!md.trim().is_empty())
                    .then(|| md_seen.iter().find(|(_, m)| *m == md).map(|(n, _)| n.clone()))
                    .flatten();
                md_seen.push((en.name.clone(), md.clone()));
                let f = all.iter().find(|f| f.display_name == en.name);
                json!({
                    "name": en.name, "date": en.date, "release": en.release, "installed": en.installed,
                    "markdown": if same.is_some() { String::new() } else { md }, "same_as": same,
                    "file_id": f.map(|f| f.id.to_string()),
                    "version": f.map(|f| hopper_core::scan::short_version(&f.file_name)),
                })
            })
            .collect();
        let others: Vec<Value> = all
            .iter()
            .filter(|f| !shown.contains(f.display_name.as_str()))
            .take(60)
            .map(|f| {
                json!({
                    "file_id": f.id.to_string(), "name": f.display_name, "date": f.file_date, "release": f.release_name(),
                    "version": hopper_core::scan::short_version(&f.file_name),
                    "older": !installed_date.is_empty() && f.file_date < installed_date,
                })
            })
            .collect();
        let behind = entries.iter().filter(|e| !e.installed).count()
            + all
                .iter()
                .filter(|f| !shown.contains(f.display_name.as_str()) && !installed_date.is_empty() && f.file_date > installed_date)
                .count();
        let latest = update
            .as_ref()
            .map(|f| (f.id, hopper_core::scan::short_version(&f.file_name)))
            .or_else(|| {
                all.iter()
                    .filter(|f| !installed_date.is_empty() && f.file_date > installed_date)
                    .max_by(|x, y| x.file_date.cmp(&y.file_date))
                    .map(|f| (f.id, hopper_core::scan::short_version(&f.file_name)))
            });
        Ok(json!({
            "project_id": a.project_id.to_string(), "name": a.name, "icon": a.icon, "page": a.page,
            "locked": a.locked, "from": a.file_name,
            "to": update.as_ref().map(|f| f.file_name.clone()),
            "behind": behind,
            "latest": latest.map(|(id, v)| json!({ "file_id": id.to_string(), "version": v })),
            "entries": rows, "others": others,
        }))
    })
    .await?;
    Ok(Json(v))
}

// ---- related mods -------------------------------------------------------------

#[derive(Deserialize)]
struct RelatedBody {
    files: Vec<String>,
    #[serde(default)]
    on: bool,
}

/// Turning mods off: the mods that need them. Turning on: the mods they
/// need that are off. Asked before switching, like desktop.
async fn related(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<RelatedBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let v = blocking(move || {
        let inst = crate::mods_api::instance_of(&e)?;
        let files: Vec<String> = b
            .files
            .iter()
            .filter_map(|f| inst.addons.iter().find(|a| same_file(&a.file_name, f)).map(|a| a.file_name.clone()))
            .collect();
        let rel = if b.on { inst.missing_deps_of(&files) } else { inst.dependents_of(&files) };
        Ok(json!({ "mods": rel.iter().map(|a| json!({ "file": a.file_name, "name": a.name })).collect::<Vec<_>>() }))
    })
    .await?;
    Ok(Json(v))
}

// ---- bulk -----------------------------------------------------------------------

#[derive(Deserialize)]
struct ManyBody {
    files: Vec<String>,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    locked: bool,
}

async fn toggle_many(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<ManyBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let running = e.server.state().is_alive();
    log::info!("{}: turning {} {} mods", e.name, if b.enabled { "on" } else { "off" }, b.files.len());
    let n = blocking(move || {
        let mut inst = crate::mods_api::instance_of(&e)?;
        let mut n = 0;
        for f in &b.files {
            let i = crate::mods_api::index_of(&mut inst, f)?;
            if inst.addons[i].enabled != b.enabled {
                install::set_enabled(&mut inst, i, b.enabled).map_err(install::explain_locked)?;
                n += 1;
            }
        }
        Ok(n)
    })
    .await?;
    Ok(Json(json!({ "changed": n, "restart_needed": running && n > 0 })))
}

async fn remove_many(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<ManyBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    crate::mods_api::drop_updates_for(&id, &b.files);
    let running = e.server.state().is_alive();
    log::info!("{}: removing {} mods", e.name, b.files.len());
    let n = blocking(move || {
        let mut inst = crate::mods_api::instance_of(&e)?;
        let mut n = 0;
        for f in &b.files {
            // Indexes move as entries go: look each one up again.
            let i = crate::mods_api::index_of(&mut inst, f)?;
            install::remove(&mut inst, i).map_err(install::explain_locked)?;
            n += 1;
        }
        Ok(n)
    })
    .await?;
    Ok(Json(json!({ "removed": n, "restart_needed": running && n > 0 })))
}

async fn lock_many(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<ManyBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let root = e.root.clone();
    let n = blocking(move || {
        crate::mods_api::instance_of(&e)?;
        let mut n = 0;
        Instance::update_on_disk(&root, |inst| {
            for a in inst.addons.iter_mut().filter(|a| a.project_id != 0 && b.files.iter().any(|f| same_file(&a.file_name, f))) {
                if a.locked != b.locked {
                    a.locked = b.locked;
                    n += 1;
                }
            }
        })?;
        Ok(n)
    })
    .await?;
    // Locked mods' offers stay in the check (the table hides them); one
    // unlocked may not have been checked, so that check starts over.
    if !b.locked && n > 0 {
        crate::mods_api::forget_updates(&id);
    }
    Ok(Json(json!({ "changed": n })))
}

// ---- switch source ----------------------------------------------------------------

#[derive(Deserialize)]
struct SwitchBody {
    file: String,
}

/// The same mod from the other site (CurseForge ↔ Modrinth); the old jar
/// goes to `.hopper-backups`.
async fn switch(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<SwitchBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    crate::mods_api::drop_updates_for(&id, std::slice::from_ref(&b.file));
    let job = crate::pack_api::spawn(&app, format!("Switching a mod's source on {}", e.name), move |job| {
        let mut inst = crate::mods_api::instance_of(&e)?;
        let file = inst
            .addons
            .iter()
            .find(|a| same_file(&a.file_name, &b.file))
            .map(|a| a.file_name.clone())
            .ok_or_else(|| anyhow::anyhow!("{} isn't in mods/", b.file))?;
        let msg = install::switch_source(&mut inst, &file, job)?;
        log::info!("{}: {msg}", e.name);
        Ok(json!({ "message": msg }))
    });
    Ok(Json(json!({ "job": job })))
}
