//! Mod profiles: named sets of mods that are off, to switch between, as in
//! Hopper desktop's Mods tab.
//!
//! This was also where a server ran "Find the mod that breaks it", removed
//! in 0.13.5: turning mods off in rounds broke worlds, KubeJS scripts and
//! data packs on big packs. A hunt an older Hopper Server left running is
//! put back when it starts ([`resume_note`]).

use crate::App;
use crate::api::{ApiResult, bad, not_found, require};
use crate::servers::Entry;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post};
use axum::{Json, Router};
use hopper_core::bisect;
use hopper_core::hosting::Properties;
use hopper_core::install;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/servers/{id}/mods/profiles", get(profiles))
        .route("/api/v1/servers/{id}/mods/profiles/save", post(save_profile))
        .route("/api/v1/servers/{id}/mods/profiles/use", post(use_profile))
        .route("/api/v1/servers/{id}/mods/profiles/delete", post(delete_profile))
}

fn entry(app: &App, headers: &HeaderMap, method: &Method, id: &str) -> ApiResult<Arc<Entry>> {
    require(app, headers, method)?;
    app.servers.get(id).ok_or_else(|| not_found("server"))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(bad)?.map_err(|e| bad(format!("{e:#}")))
}

// ---- profiles ---------------------------------------------------------------

fn names(list: &[&hopper_core::instances::Addon], cap: usize) -> Vec<String> {
    list.iter()
        .take(cap)
        .map(|a| if a.name.is_empty() { a.file_name.trim_end_matches(".jar").to_string() } else { a.name.clone() })
        .collect()
}

/// Each profile with what switching to it would turn on and off now.
async fn profiles(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let v = blocking(move || {
        let inst = crate::mods_api::instance_of(&e)?;
        let list: Vec<Value> = inst
            .mod_profiles
            .iter()
            .map(|p| {
                let (on, off) = install::profile_changes(&inst, p);
                json!({
                    "name": p.name, "disabled": p.disabled.len(),
                    "turns_on": on.len(), "turns_off": off.len(),
                    "on_names": names(&on, 4), "off_names": names(&off, 4),
                })
            })
            .collect();
        Ok(json!({ "profiles": list }))
    })
    .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct NameBody {
    name: String,
}

async fn save_profile(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<NameBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let name = b.name.trim().to_string();
    if name.is_empty() {
        return Err(bad("Give the profile a name."));
    }
    blocking(move || {
        let mut inst = crate::mods_api::instance_of(&e)?;
        install::save_profile(&mut inst, &name)
    })
    .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn use_profile(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<NameBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let running = e.server.state().is_alive();
    log::info!("{}: switching to mod profile {}", e.name, b.name);
    let n = blocking(move || {
        let mut inst = crate::mods_api::instance_of(&e)?;
        install::apply_profile(&mut inst, &b.name).map_err(install::explain_locked)
    })
    .await?;
    Ok(Json(json!({ "changed": n, "restart_needed": running && n > 0 })))
}

async fn delete_profile(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<NameBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let gone = blocking(move || {
        let mut inst = crate::mods_api::instance_of(&e)?;
        install::delete_profile(&mut inst, &b.name)
    })
    .await?;
    Ok(Json(json!({ "deleted": gone })))
}

// ---- a hunt left running by an older Hopper Server ----------------------------

/// What an old server hunt kept beside its journal.
const SIDE: &str = "hopper-bisect-server.json";
/// The throwaway world its rounds played on.
const SCRATCH: &str = "hopper-hunt-world";

#[derive(Deserialize, Default)]
#[serde(default)]
struct Side {
    /// `level-name` before the hunt, when rounds played on [`SCRATCH`].
    world: Option<String>,
}

/// At start: every server with a hunt an older Hopper Server left running
/// gets its mods back on or off as before it, and its real world back.
pub fn resume_note(app: &App) {
    for e in app.servers.list() {
        if !bisect::path(&e.root).is_file() {
            continue;
        }
        let side: Side =
            std::fs::read_to_string(e.root.join(SIDE)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let r = (|| -> anyhow::Result<usize> {
            let mut inst = crate::mods_api::instance_of(&e)?;
            let n = bisect::restore_leftover(&mut inst)?.unwrap_or(0);
            if let Some(w) = &side.world {
                let props = e.root.join("server.properties");
                let mut p = Properties::load(&props)?;
                p.set("level-name", w);
                p.save(&props)?;
                let _ = std::fs::remove_dir_all(e.root.join(SCRATCH));
            }
            let _ = std::fs::remove_file(e.root.join(SIDE));
            Ok(n)
        })();
        match r {
            Ok(n) => log::info!("{}: put back {n} mods from an unfinished mod hunt", e.name),
            Err(err) => log::warn!("{}: putting back the mod hunt's mods: {err:#}", e.name),
        }
    }
}
