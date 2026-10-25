//! Running a server day to day: its server.properties, Java memory and
//! flags, players, backups and schedule; and making new servers.

use crate::api::{ApiError, ApiResult, bad, not_found, require};
use crate::servers::Entry;
use crate::App;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use hopper_core::hosting::backup::{self, Kind, Scope};
use hopper_core::hosting::properties::{self, PropKind, Properties};
use hopper_core::hosting::{JvmArgs, JvmPreset, Schedule};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/servers/{id}/properties", get(get_props).put(put_props))
        .route("/api/v1/servers/{id}/jvm", get(get_jvm).put(put_jvm))
        .route("/api/v1/servers/{id}/players", get(players).post(player_action))
        .route("/api/v1/servers/{id}/backups", get(backups).post(make_backup))
        .route("/api/v1/servers/{id}/backups/{name}", axum::routing::delete(delete_backup))
        .route("/api/v1/servers/{id}/backups/{name}/entries", get(backup_entries))
        .route("/api/v1/servers/{id}/backups/{name}/restore", post(restore_backup))
        .route("/api/v1/servers/{id}/schedule", get(get_schedule).put(put_schedule))
        .route("/api/v1/servers/create", post(create))
        .route("/api/v1/meta/minecraft", get(mc_versions))
        .route("/api/v1/meta/loaders/{mc}", get(loader_versions))
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

// ---- server.properties --------------------------------------------------------

fn spec_json(s: &properties::PropSpec) -> Value {
    let (kind, options, min, max) = match s.kind {
        PropKind::Bool => ("bool", None, None, None),
        PropKind::Int { min, max } => ("int", None, Some(min), Some(max)),
        PropKind::Enum(o) => ("enum", Some(o), None, None),
        PropKind::String => ("string", None, None, None),
    };
    json!({ "key": s.key, "label": s.label, "kind": kind, "options": options, "min": min, "max": max,
            "description": s.description, "default": s.default })
}

async fn get_props(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let p = blocking(move || Properties::load(&e.root.join("server.properties"))).await?;
    let values: BTreeMap<&str, &str> = p.entries().collect();
    Ok(Json(json!({
        "values": values,
        "specs": properties::COMMON.iter().map(spec_json).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
struct PropsBody {
    values: BTreeMap<String, String>,
}

/// Changes keys (others stay as they are, comments too). Known keys are
/// checked first; nothing is written when one is wrong.
async fn put_props(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<PropsBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    for (k, v) in &b.values {
        if k.trim().is_empty() || k.contains(['=', '\n', '\r']) || v.contains(['\n', '\r']) {
            return Err(bad(format!("\"{k}\" can't be saved")));
        }
        if let Some(s) = properties::spec(k) {
            s.validate(v).map_err(bad)?;
        }
    }
    let running = e.server.state().is_alive();
    let name = e.name.clone();
    blocking(move || {
        let path = e.root.join("server.properties");
        let mut p = Properties::load(&path)?;
        for (k, v) in b.values {
            p.set(&k, v.trim());
        }
        p.save(&path)
    })
    .await?;
    log::info!("{name}: server.properties saved");
    app.refresh.notify_one();
    Ok(Json(json!({ "ok": true, "restart_needed": running })))
}

// ---- Java memory and flags -------------------------------------------------------

/// The Java the server starts with (what the start script says, or the
/// one Hopper puts in place of a plain `java`), and its major version.
fn java_of(e: &Entry) -> (Option<String>, Option<u32>) {
    let java = hopper_core::hosting::script::launch_command(&e.root)
        .ok()
        .map(|l| l.java.to_string_lossy().into_owned())
        .or_else(|| hopper_core::server::load(&e.root).java);
    let major = java.as_deref().and_then(|j| hopper_core::java::inspect(std::path::Path::new(j))).map(|j| j.major);
    (java, major)
}

/// The files that decide how the server starts and that exist: start
/// scripts and the JVM arguments file.
fn start_files(e: &Entry) -> Vec<&'static str> {
    hopper_core::hosting::script::SCRIPTS
        .into_iter()
        .chain([hopper_core::hosting::jvm::ARGS_FILE])
        .filter(|f| e.root.join(f).is_file())
        .collect()
}

async fn get_jvm(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let total = app.host.read().unwrap().mem_total;
    let v = blocking(move || {
        let a = JvmArgs::load(&e.root)?;
        let (java, major) = java_of(&e);
        let files = start_files(&e);
        let major_n = major.unwrap_or(21);
        let presets: Vec<Value> = [JvmPreset::Aikar, JvmPreset::Zgc, JvmPreset::Plain]
            .into_iter()
            .map(|p| json!({ "id": format!("{p:?}").to_lowercase(), "label": p.label(), "supported": p.supported(major_n) }))
            .collect();
        Ok(json!({
            "memory_mb": a.xmx_mb, "preset": format!("{:?}", a.preset()).to_lowercase(), "flags": a.flags,
            "presets": presets, "java": java, "java_major": major, "host_memory_mb": total / (1024 * 1024),
            "files": files,
        }))
    })
    .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct JvmBody {
    memory_mb: Option<u32>,
    preset: Option<String>,
}

async fn put_jvm(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<JvmBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let running = e.server.state().is_alive();
    blocking(move || {
        let mut a = JvmArgs::load(&e.root)?;
        if let Some(mb) = b.memory_mb {
            anyhow::ensure!((512..=262_144).contains(&mb), "Memory must be between 512 MB and 256 GB.");
            a.set_memory(mb);
        }
        if let Some(p) = b.preset {
            let preset = match p.as_str() {
                "aikar" => JvmPreset::Aikar,
                "zgc" => JvmPreset::Zgc,
                "plain" => JvmPreset::Plain,
                other => anyhow::bail!("Unknown preset {other}"),
            };
            let major = java_of(&e).1.unwrap_or(21);
            anyhow::ensure!(preset.supported(major), "{} needs a newer Java than this server's (Java {major}).", preset.label());
            a.set_preset(preset, major);
        }
        a.save(&e.root)
    })
    .await?;
    Ok(Json(json!({ "ok": true, "restart_needed": running })))
}

// ---- players ------------------------------------------------------------------------

fn names_in(root: &std::path::Path, file: &str) -> Vec<Value> {
    std::fs::read(root.join(file))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<Value>>(&b).ok())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| {
            let name = v["name"].as_str()?.to_string();
            Some(json!({ "name": name, "uuid": v["uuid"], "reason": v["reason"], "level": v["level"] }))
        })
        .collect()
}

async fn players(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let props = Properties::load(&e.root.join("server.properties")).unwrap_or_default();
    Ok(Json(json!({
        "online": e.server.players(),
        "running": e.server.state().is_alive(),
        "ops": names_in(&e.root, "ops.json"),
        "whitelist": names_in(&e.root, "whitelist.json"),
        "banned": names_in(&e.root, "banned-players.json"),
        "whitelist_on": props.get("white-list").is_some_and(|v| v.trim() == "true"),
    })))
}

#[derive(Deserialize)]
struct PlayerBody {
    action: String,
    name: String,
    #[serde(default)]
    reason: String,
}

/// Player name rules: 3-16 letters, digits, underscores.
fn valid_name(n: &str) -> bool {
    (1..=16).contains(&n.len()) && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Ops, kicks, bans and the whitelist go through the server's own
/// commands, so it looks names up and writes its files itself.
async fn player_action(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<PlayerBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let name = b.name.trim();
    if !valid_name(name) {
        return Err(bad(format!("\"{name}\" isn't a Minecraft name.")));
    }
    if !e.server.state().is_alive() {
        return Err(conflict("Start the server to change players: it looks names up and saves the lists itself."));
    }
    let reason = b.reason.replace(['\n', '\r'], " ");
    let cmd = match b.action.as_str() {
        "op" => format!("op {name}"),
        "deop" => format!("deop {name}"),
        "kick" => format!("kick {name} {reason}"),
        "ban" => format!("ban {name} {reason}"),
        "pardon" => format!("pardon {name}"),
        "whitelist_add" => format!("whitelist add {name}"),
        "whitelist_remove" => format!("whitelist remove {name}"),
        other => return Err(bad(format!("Unknown action {other}"))),
    };
    e.server.command(cmd.trim()).map_err(|e| conflict(format!("{e:#}")))?;
    log::info!("{}: {}", e.name, cmd.trim());
    Ok(Json(json!({ "ok": true })))
}

// ---- backups -------------------------------------------------------------------------

async fn backups(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let root = e.root.clone();
    let list = blocking(move || Ok(backup::list(&root))).await?;
    let total: u64 = list.iter().map(|b| b.size).sum();
    let free = app.host.read().unwrap().disk_free;
    Ok(Json(json!({ "backups": list, "total_size": total, "disk_free": free, "folder": backup::DIR })))
}

#[derive(Deserialize)]
struct MakeBody {
    #[serde(default)]
    worlds_only: bool,
    #[serde(default)]
    note: String,
}

async fn make_backup(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<MakeBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let scope = if b.worlds_only { Scope::Worlds } else { Scope::Full };
    let a = app.clone();
    let job = crate::pack_api::spawn(&app, format!("Backing up {}", e.name), move |job| {
        let r = backup::make(&e.root, scope, Kind::Manual, b.note.trim(), Some(&e.server), job)?;
        let text = format!("{} backed up ({})", e.name, r.name);
        let ev = crate::webhooks::Event { event: "backup", server_id: e.id.clone(), server: e.name.clone(), text, at: crate::webhooks::now() };
        crate::webhooks::emit(&a, vec![ev]);
        Ok(r)
    });
    Ok(Json(json!({ "job": job })))
}

async fn delete_backup(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path((id, name)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    log::info!("{}: deleting backup {name}", e.name);
    blocking(move || backup::delete(&e.root, &name)).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn backup_entries(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path((id, name)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let list = blocking(move || backup::entries(&e.root, &name)).await?;
    Ok(Json(json!({ "entries": list })))
}

#[derive(Deserialize)]
struct RestoreBody {
    #[serde(default)]
    paths: Vec<String>,
}

async fn restore_backup(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path((id, name)): Path<(String, String)>,
    Json(b): Json<RestoreBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    if e.server.state().is_alive() {
        return Err(conflict(format!("Stop {} before restoring a backup.", e.name)));
    }
    log::info!("{}: restoring {name} ({} paths)", e.name, if b.paths.is_empty() { "all".into() } else { b.paths.len().to_string() });
    let job = crate::pack_api::spawn(&app, format!("Restoring {}", e.name), move |job| {
        let n = backup::restore(&e.root, &name, &b.paths, job)?;
        Ok(json!({ "files": n }))
    });
    Ok(Json(json!({ "job": job })))
}

// ---- schedule ---------------------------------------------------------------------------

async fn get_schedule(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let s = Schedule::load(&e.root);
    let next = hopper_core::hosting::schedule::describe_next_restart(&s, hopper_core::hosting::schedule::LocalTime::now());
    let gave_up = app.scheduler.lock().unwrap().as_ref().and_then(|sc| sc.gave_up(&e.root));
    Ok(Json(json!({ "schedule": s, "next_restart": next, "gave_up": gave_up,
                    "last_backup": backup::last_scheduled(&e.root) })))
}

async fn put_schedule(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(mut s): Json<Schedule>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    if s.daily_restart.as_deref().is_some_and(|t| t.trim().is_empty()) {
        s.daily_restart = None;
    }
    s.validate().map_err(bad)?;
    s.warn_minutes.sort_unstable_by(|a, b| b.cmp(a));
    s.warn_minutes.dedup();
    s.save(&e.root).map_err(|e| bad(format!("{e:#}")))?;
    log::info!("{}: schedule saved", e.name);
    Ok(Json(json!({ "ok": true })))
}

// ---- new servers ----------------------------------------------------------------------------

async fn mc_versions(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let v = blocking(hopper_core::hosting::create::release_versions).await?;
    Ok(Json(json!({ "versions": v })))
}

async fn loader_versions(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(mc): Path<String>,
) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let list = blocking(move || hopper_core::cf::loader_versions(&mc)).await?;
    let mut by: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for l in list {
        if let Some((kind, v)) = l.name.split_once('-') {
            by.entry(kind.to_string()).or_default().push(v.to_string());
        }
    }
    Ok(Json(json!({ "loaders": by })))
}

#[derive(Deserialize)]
struct CreateBody {
    name: String,
    mc_version: String,
    /// vanilla, fabric, quilt, neoforge.
    loader: String,
    #[serde(default)]
    loader_version: String,
    memory_mb: Option<u32>,
    port: Option<u16>,
    #[serde(default)]
    motd: String,
    /// Address players join it by ("survival.mc.example.com"); empty: none.
    #[serde(default)]
    address: String,
    /// The person creating it agreed to the Minecraft EULA in the form.
    accept_eula: bool,
}

/// A Java that runs `mc`'s server: an installed one, else Temurin downloaded.
pub(crate) fn java_for(mc: &str, job: &hopper_core::jobs::Job) -> anyhow::Result<PathBuf> {
    use hopper_core::java;
    job.detail("Finding Java");
    let installs = java::detect(std::path::Path::new(""));
    if let Some(r) = java::best(&installs, mc, java::Side::Server) {
        return Ok(hopper_core::hosting::script::console_java(&r.install.exe));
    }
    let need = java::recommended_major(mc);
    hopper_core::javadl::install_temurin(need, hopper_core::javadl::Package::Jdk, job)
}

async fn create(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<CreateBody>) -> ApiResult<Json<Value>> {
    use hopper_core::hosting::{CreateOptions, ServerLoader};
    require(&app, &headers, &method)?;
    if !b.accept_eula {
        return Err(bad("Agree to the Minecraft EULA to make a server."));
    }
    let Some(root) = app.cfg.read().unwrap().servers_root.clone() else {
        return Err(bad("Set the servers folder first (hopperd --servers-root)."));
    };
    let loader = match b.loader.as_str() {
        "vanilla" => ServerLoader::Vanilla,
        "fabric" => ServerLoader::Fabric((!b.loader_version.is_empty()).then(|| b.loader_version.clone())),
        "quilt" => ServerLoader::Quilt((!b.loader_version.is_empty()).then(|| b.loader_version.clone())),
        "neoforge" if !b.loader_version.is_empty() => ServerLoader::NeoForge(b.loader_version.clone()),
        "neoforge" => return Err(bad("Pick a NeoForge version.")),
        other => return Err(bad(format!("Hopper Server can't make {other} servers yet."))),
    };
    let address = match b.address.trim() {
        "" => None,
        a => Some(hopper_core::hosting::router::clean_address(a).map_err(bad)?),
    };
    if let Some(a) = &address
        && let Some(other) = app.servers.list().into_iter().find(|e| crate::router::settings(e).addresses.contains(a))
    {
        return Err(bad(format!("{} already has {a}.", other.name)));
    }
    // Behind the router, a server needs a port other than the router's.
    let router_port = { let c = app.cfg.read().unwrap(); c.router.enabled.then_some(c.router.port) };
    let auto_port = match (b.port, router_port) {
        (None, Some(rp)) => crate::router::free_port(&app, None).filter(|_| hopper_core::hosting::suggest_port(&root) == rp),
        _ => None,
    };
    let title = format!("Making {}", b.name.trim());
    let a = app.clone();
    let job = crate::pack_api::spawn(&app, title, move |job| {
        let java = java_for(&b.mc_version, job)?;
        let mut o = CreateOptions::new(b.name.trim(), &root, &b.mc_version, loader, &java);
        if let Some(m) = b.memory_mb {
            o.memory_mb = m;
        }
        if let Some(p) = b.port.or(auto_port) {
            o.port = p;
        }
        if !b.motd.trim().is_empty() {
            o.motd = b.motd.trim().to_string();
        }
        o.accept_eula = true;
        let folder = hopper_core::hosting::create_server(&o, job)?;
        if let Some(addr) = address {
            hopper_core::hosting::router::RouteSettings { addresses: vec![addr], ..Default::default() }.save(&folder)?;
            a.router.changed();
        }
        a.rescan();
        a.refresh.notify_one();
        let id = a.servers.list().into_iter().find(|e| e.root == folder).map(|e| e.id.clone());
        log::info!("made {}", folder.display());
        Ok(json!({ "id": id, "path": folder }))
    });
    Ok(Json(json!({ "job": job })))
}

#[cfg(test)]
mod tests {
    #[test]
    fn names() {
        assert!(super::valid_name("Steve") && super::valid_name("a_b_9"));
        assert!(!super::valid_name("x y") && !super::valid_name("Steve;op me") && !super::valid_name(""));
    }
}
