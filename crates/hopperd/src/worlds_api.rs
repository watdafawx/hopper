//! A server's worlds over the API: the list (size, last played, seed,
//! which one is active), new worlds, switching, reset with a backup first,
//! delete, upload, and pre-generation with Chunky (installed when missing,
//! progress read from the console).

use crate::App;
use crate::api::{ApiError, ApiResult, bad, not_found, require};
use crate::servers::Entry;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use hopper_core::hosting::ServerState;
use hopper_core::hosting::backup::Kind;
use hopper_core::hosting::worlds::{self, ChunkyKind, SeedChoice};
use hopper_core::panel::PanelAction;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/servers/{id}/worlds", get(list).post(create))
        .route("/api/v1/servers/{id}/worlds/import", post(import))
        .route("/api/v1/servers/{id}/worlds/pregen", post(pregen))
        .route("/api/v1/servers/{id}/worlds/pregen/{op}", post(pregen_op))
        .route("/api/v1/servers/{id}/worlds/{name}", delete(remove))
        .route("/api/v1/servers/{id}/worlds/{name}/activate", post(activate))
        .route("/api/v1/servers/{id}/worlds/{name}/backup", post(backup))
        .route("/api/v1/servers/{id}/worlds/{name}/reset", post(reset))
}

fn entry(app: &App, headers: &HeaderMap, method: &Method, id: &str) -> ApiResult<Arc<Entry>> {
    require(app, headers, method)?;
    app.servers.get(id).ok_or_else(|| not_found("server"))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(bad)?.map_err(|e| bad(format!("{e:#}")))
}

fn conflict(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::CONFLICT, msg.into())
}

fn stopped(e: &Entry, what: &str) -> ApiResult<()> {
    if e.server.state().is_alive() {
        return Err(conflict(format!("Stop {} before {what}.", e.name)));
    }
    Ok(())
}

/// The server's Minecraft version, from what's known about it.
fn mc_of(e: &Entry) -> String {
    e.minecraft
        .clone()
        .filter(|v| !v.is_empty())
        .or_else(|| hopper_core::hosting::HostedMeta::load(&e.root).map(|m| m.minecraft).filter(|v| !v.is_empty()))
        .unwrap_or_else(|| e.server.tracker().version)
}

fn chunky_kind(app: &App, e: &Entry) -> Option<ChunkyKind> {
    let loader = app.servers.summary(e).loader.unwrap_or_default();
    ChunkyKind::of(&loader, &e.root)
}

fn trash_in(root: &std::path::Path) -> impl Fn(&str) -> anyhow::Result<()> + '_ {
    move |rel| crate::files::trash(root, rel).map(|_| ())
}

async fn list(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let kind = chunky_kind(&app, &e);
    let t = e.server.tracker();
    let lines: Vec<String> = t.lines.iter().cloned().collect();
    let running = e.server.state() == ServerState::Running;
    let players = t.players.len();
    let root = e.root.clone();
    let mc = mc_of(&e);
    let v = blocking(move || {
        let ws = worlds::list(&root);
        let total: u64 = ws.iter().map(|w| w.size).sum();
        // As desktop's world cards: the world's icon.png and its data packs.
        let ws: Vec<Value> = ws
            .into_iter()
            .map(|w| {
                let dir = root.join(&w.name);
                let packs = crate::parity_worlds::datapacks(&dir.join("datapacks"));
                let mut v = json!(w);
                v["icon"] = json!(dir.join("icon.png").is_file());
                v["datapacks"] = json!(packs);
                v
            })
            .collect();
        Ok(json!({
            "worlds": ws,
            "total_size": total,
            "level": worlds::level_name(&root),
            "chunky": {
                "kind": kind,
                "jar": worlds::chunky_jar(&root),
                "minecraft": mc,
            },
            "pregen": if running { worlds::chunky_progress(&lines) } else { None },
            "last_pregen": worlds::last_pregen(&root),
        }))
    })
    .await?;
    let mut v = v;
    v["running"] = json!(running);
    v["players"] = json!(players);
    Ok(Json(v))
}

#[derive(Deserialize)]
struct CreateBody {
    name: String,
    #[serde(default)]
    seed: String,
    #[serde(default)]
    activate: bool,
}

async fn create(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<CreateBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    if b.activate {
        stopped(&e, "switching worlds")?;
    }
    log::info!("{}: new world {} (active: {})", e.name, b.name.trim(), b.activate);
    let root = e.root.clone();
    blocking(move || worlds::create(&root, b.name.trim(), &b.seed, b.activate)).await?;
    app.servers.reload_info(&id);
    Ok(Json(json!({ "ok": true })))
}

async fn activate(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path((id, name)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    stopped(&e, "switching worlds")?;
    log::info!("{}: active world -> {name}", e.name);
    let root = e.root.clone();
    blocking(move || worlds::set_active(&root, &name)).await?;
    app.servers.reload_info(&id);
    Ok(Json(json!({ "ok": true })))
}

async fn backup(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path((id, name)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    worlds::check_name(&name).map_err(bad)?;
    let job = crate::pack_api::spawn(&app, format!("Backing up {name} on {}", e.name), move |job| {
        let b = worlds::backup_world(&e.root, &name, Kind::Manual, &format!("World {name}"), Some(&e.server), job)?;
        crate::offsite::kick();
        Ok(b)
    });
    Ok(Json(json!({ "job": job })))
}

#[derive(Deserialize)]
struct ResetBody {
    seed: SeedChoice,
    #[serde(default)]
    keep_dims: bool,
    /// Back the world up first (the "Back up first" tick; off unless the
    /// person asks: backups are theirs to make).
    #[serde(default)]
    backup_first: bool,
}

/// `?backup_first=true` on a world delete.
#[derive(Deserialize, Default)]
struct RemoveQuery {
    #[serde(default)]
    backup_first: bool,
}

async fn reset(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path((id, name)): Path<(String, String)>, Json(b): Json<ResetBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    stopped(&e, "resetting a world")?;
    worlds::check_name(&name).map_err(bad)?;
    log::info!("{}: resetting world {name} ({:?}, keep dimensions: {})", e.name, b.seed, b.keep_dims);
    let job = crate::pack_api::spawn(&app, format!("Resetting {name} on {}", e.name), move |job| {
        if b.backup_first && e.root.join(&name).join("level.dat").is_file() {
            job.detail(format!("Backing up {name}"));
            worlds::backup_world(&e.root, &name, Kind::Manual, &format!("Before resetting {name}"), None, job)?;
            crate::offsite::kick();
        }
        job.detail(format!("Moving {name} to the trash"));
        let seed = worlds::reset(&e.root, &name, &b.seed, b.keep_dims, &trash_in(&e.root))?;
        Ok(json!({ "seed": seed }))
    });
    Ok(Json(json!({ "job": job })))
}

async fn remove(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path((id, name)): Path<(String, String)>,
    Query(q): Query<RemoveQuery>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    worlds::check_name(&name).map_err(bad)?;
    if worlds::level_name(&e.root) == name {
        return Err(conflict(format!("\"{name}\" is the active world. Make another one active first.")));
    }
    log::info!("{}: deleting world {name}", e.name);
    let job = crate::pack_api::spawn(&app, format!("Deleting {name} on {}", e.name), move |job| {
        if q.backup_first && e.root.join(&name).join("level.dat").is_file() {
            job.detail(format!("Backing up {name}"));
            worlds::backup_world(&e.root, &name, Kind::Manual, &format!("Before deleting {name}"), None, job)?;
            crate::offsite::kick();
        }
        worlds::delete(&e.root, &name, &trash_in(&e.root))?;
        Ok(json!({ "ok": true }))
    });
    Ok(Json(json!({ "job": job })))
}

#[derive(Deserialize)]
struct ImportBody {
    /// The uploaded zip, relative to the server folder.
    zip: String,
    name: String,
}

async fn import(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<ImportBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    worlds::check_name(b.name.trim()).map_err(bad)?;
    crate::files::resolve(&e.root, &b.zip).map_err(bad)?;
    log::info!("{}: importing world {} from {}", e.name, b.name.trim(), b.zip);
    let job = crate::pack_api::spawn(&app, format!("Adding world {} to {}", b.name.trim(), e.name), move |job| {
        let r = worlds::import_zip(&e.root, &b.zip, b.name.trim(), job);
        // The Worlds tab's upload is only a carrier: don't leave it in the
        // server folder when it couldn't be unpacked (on success it's gone).
        if r.is_err() && is_world_upload(&b.zip) {
            let _ = std::fs::remove_file(e.root.join(&b.zip));
        }
        r?;
        Ok(json!({ "ok": true }))
    });
    Ok(Json(json!({ "job": job })))
}

/// The Worlds tab's upload name: `world-upload-<ms>.zip` in the server folder.
fn is_world_upload(rel: &str) -> bool {
    rel.strip_prefix("world-upload-").and_then(|r| r.strip_suffix(".zip")).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

#[derive(Deserialize)]
struct PregenBody {
    radius: u32,
    /// Block x, z; spawn when left out.
    #[serde(default)]
    center: Option<(i64, i64)>,
}

/// Waits for the server to come up (after a start or restart), giving up
/// after `limit`. `was_up`: it was running before, so it must go down first.
fn wait_running(e: &Entry, was_up: bool, limit: Duration, job: &hopper_core::jobs::Job) -> anyhow::Result<()> {
    let end = Instant::now() + limit;
    let mut went_down = !was_up;
    let mut stopped_since: Option<Instant> = None;
    while Instant::now() < end {
        if job.is_cancelled() {
            anyhow::bail!("Cancelled");
        }
        let st = e.server.state();
        match st {
            ServerState::Running if went_down => return Ok(()),
            ServerState::Running => {}
            ServerState::Crashed(_) => anyhow::bail!("{} crashed while starting. See the console.", e.name),
            _ => went_down = true,
        }
        if st == ServerState::Stopped {
            let since = *stopped_since.get_or_insert_with(Instant::now);
            if since.elapsed() > Duration::from_secs(90) {
                anyhow::bail!("{} didn't start. See the console.", e.name);
            }
        } else {
            stopped_since = None;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    anyhow::bail!("{} didn't finish starting in {} minutes.", e.name, limit.as_secs() / 60)
}

async fn pregen(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<PregenBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    if !(16..=100_000).contains(&b.radius) {
        return Err(bad("Pick a radius between 16 and 100,000 blocks."));
    }
    let kind = chunky_kind(&app, &e).ok_or_else(|| bad("Chunky needs a modded (Fabric, NeoForge, Forge, Quilt) or plugin (Paper) server. Vanilla can't load it."))?;
    let installed = worlds::chunky_jar(&e.root).is_some();
    let state = e.server.state();
    let up = state == ServerState::Running;
    if state.is_alive() && !up {
        return Err(conflict(format!("Wait until {} has finished starting or stopping.", e.name)));
    }
    if !installed && up && !e.server.tracker().players.is_empty() {
        return Err(conflict("Players are online. Installing Chunky needs a restart; try again when they've left."));
    }
    let mc = mc_of(&e);
    let a = app.clone();
    log::info!("{}: pre-generating radius {} (Chunky installed: {installed})", e.name, b.radius);
    let job = crate::pack_api::spawn(&app, format!("Pre-generating {}", e.name), move |job| {
        let mut added: Vec<String> = Vec::new();
        if !installed {
            added = add_chunky(&e, &mc, &kind, job)?;
            log::info!("{}: added {}", e.name, added.join(", "));
            crate::mods_api::forget_updates(&e.id);
        }
        if !installed || !up {
            job.detail(if up { format!("Restarting {} to load Chunky", e.name) } else { format!("Starting {}", e.name) });
            a.servers.act(&e, if up { PanelAction::Restart } else { PanelAction::Start })?;
            if let Err(err) = wait_running(&e, up, Duration::from_secs(15 * 60), job) {
                if added.is_empty() {
                    return Err(err);
                }
                // Chunky (or what it pulled in) broke the start: take it out
                // again and bring the server back as it was.
                log::warn!("{}: didn't start with Chunky ({err:#}); removing {}", e.name, added.join(", "));
                job.detail("Chunky didn't load; removing it again");
                remove_added(&e, &added);
                if up {
                    let _ = a.servers.act(&e, PanelAction::Start);
                }
                anyhow::bail!("{} didn't start with Chunky, so Hopper removed it again{}. See the console for why.", e.name, if up { " and started the server" } else { "" });
            }
        }
        job.detail("Starting Chunky");
        let level = worlds::level_name(&e.root);
        let seen = e.server.console_since(0).1;
        for c in worlds::chunky_commands(b.radius, kind == ChunkyKind::Plugin, &level, b.center) {
            a.servers.command(&e, &c)?;
            std::thread::sleep(Duration::from_millis(250));
        }
        // A saved task for this world asks to confirm a fresh start.
        let end = Instant::now() + Duration::from_secs(4);
        while Instant::now() < end {
            let (lines, _) = e.server.console_since(seen);
            if lines.iter().any(|l| l.contains("chunky confirm")) {
                a.servers.command(&e, "chunky confirm")?;
                break;
            }
            if lines.iter().any(|l| l.contains("Unknown or incomplete command") || l.contains("Unknown command")) {
                anyhow::bail!("The server doesn't know the chunky command: Chunky didn't load. Check the console.");
            }
            if lines.iter().any(|l| l.contains("[Chunky] Task started") || l.contains("[Chunky] Task running")) {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        worlds::note_pregen(&e.root, &level, b.radius)?;
        Ok(json!({ "ok": true }))
    });
    Ok(Json(json!({ "job": job })))
}

/// The jars in mods/ and plugins/ (relative paths).
fn jars(root: &std::path::Path) -> std::collections::BTreeSet<String> {
    ["mods", "plugins"]
        .iter()
        .flat_map(|d| std::fs::read_dir(root.join(d)).into_iter().flatten().flatten().map(move |e| format!("{d}/{}", e.file_name().to_string_lossy())))
        .filter(|n| n.ends_with(".jar"))
        .collect()
}

/// Adds Chunky: a mod goes in through the Mods tab's install (with what it
/// needs, e.g. Fabric API), a plugin is downloaded into plugins/. Returns
/// the jars that are new.
fn add_chunky(e: &Entry, mc: &str, kind: &ChunkyKind, job: &hopper_core::jobs::Job) -> anyhow::Result<Vec<String>> {
    let before = jars(&e.root);
    match kind {
        ChunkyKind::Mod(_) => {
            let mut inst = crate::mods_api::instance_of(e)?;
            anyhow::ensure!(!inst.game_version.is_empty(), "Hopper doesn't know this server's Minecraft version yet. Start it once first.");
            job.detail(format!("Finding Chunky for Minecraft {}", inst.game_version));
            let id = hopper_core::modrinth::num_id(&hopper_core::modrinth::project("chunky")?.id);
            let m = hopper_core::source::get_mod(id)?;
            let file = hopper_core::install::best_file(m.id, m.class_id, &inst)?
                .ok_or_else(|| anyhow::anyhow!("Chunky has no file for Minecraft {} with this loader.", inst.game_version))?;
            hopper_core::install::install(&mut inst, &m, &file, job)?;
        }
        ChunkyKind::Plugin => {
            worlds::install_chunky(&e.root, mc, kind, job)?;
        }
    }
    Ok(jars(&e.root).difference(&before).cloned().collect())
}

/// Takes jars [`add_chunky`] added out again (mods through the Mods tab's
/// remove, so its list stays right).
fn remove_added(e: &Entry, added: &[String]) {
    let mut inst = crate::mods_api::instance_of(e).ok();
    for rel in added {
        if let (Some(file), Some(inst)) = (rel.strip_prefix("mods/"), inst.as_mut()) {
            match crate::mods_api::index_of(inst, file).and_then(|i| hopper_core::install::remove(inst, i)) {
                Ok(()) => continue,
                Err(err) => log::warn!("{}: removing {rel}: {err:#}", e.name),
            }
        }
        let _ = std::fs::remove_file(e.root.join(rel));
    }
    if let Some(inst) = inst {
        let _ = inst.save();
    }
    crate::mods_api::forget_updates(&e.id);
}

async fn pregen_op(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path((id, op)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let cmd = match op.as_str() {
        "cancel" => "chunky cancel",
        "pause" => "chunky pause",
        "continue" => "chunky continue",
        _ => return Err(not_found("action")),
    };
    if e.server.state() != ServerState::Running {
        return Err(conflict(format!("{} isn't running.", e.name)));
    }
    app.servers.command(&e, cmd).map_err(|err| bad(format!("{err:#}")))?;
    if op == "cancel" {
        // Chunky asks to confirm a cancel.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let _ = app.servers.command(&e, "chunky confirm");
    }
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod upload_tests {
    use super::is_world_upload;

    #[test]
    fn world_upload_names() {
        assert!(is_world_upload("world-upload-1761650000000.zip"));
        for no in ["world-upload-.zip", "world.zip", "mods/world-upload-1.zip", "world-upload-1.zip.bak", "world-upload-../x.zip"] {
            assert!(!is_world_upload(no), "{no}");
        }
    }
}
