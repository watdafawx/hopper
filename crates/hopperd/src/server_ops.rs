//! Copying a server and changing its version (Settings › General and
//! Settings › Version):
//! - Clone: the stopped server's folder copied into the servers folder,
//!   with or without its world, on a free port; the address and the
//!   schedule stay with the original (an address can be given).
//! - Templates: a server saved without world or players under the panel's
//!   data folder; New server › Template makes a server from one.
//! - Version: what the server runs, a check of another Minecraft / loader
//!   version (Java, mods on CurseForge / Modrinth, the loader's changelog),
//!   the change itself (full backup first) and Undo.

use crate::App;
use crate::api::{ApiResult, bad, not_found, require, require_owner};
use crate::servers::Entry;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use hopper_core::hosting::clone;
use hopper_core::hosting::version_change::{self as vc, ModFit};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/servers/{id}/clone-info", get(clone_info))
        .route("/api/v1/servers/{id}/clone", post(clone_server))
        .route("/api/v1/servers/{id}/template", post(save_template))
        .route("/api/v1/templates", get(templates))
        .route("/api/v1/templates/{tid}", delete(delete_template))
        .route("/api/v1/servers/from-template", post(from_template))
        .route("/api/v1/servers/{id}/version", get(version))
        .route("/api/v1/servers/{id}/version/loader-log", get(loader_log))
        .route("/api/v1/servers/{id}/version/check", post(version_check))
        .route("/api/v1/servers/{id}/version/change", post(version_change))
        .route("/api/v1/servers/{id}/version/undo", post(version_undo))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(bad)?.map_err(|e| bad(format!("{e:#}")))
}

fn entry(app: &App, headers: &HeaderMap, method: &Method, id: &str) -> ApiResult<Arc<Entry>> {
    require(app, headers, method)?;
    app.servers.get(id).ok_or_else(|| not_found("server"))
}

fn stopped(e: &Entry) -> ApiResult<()> {
    if e.server.state().is_alive() {
        return Err(bad(format!("Stop {} first.", e.name)));
    }
    Ok(())
}

fn servers_root(app: &App) -> ApiResult<PathBuf> {
    app.cfg
        .read()
        .unwrap()
        .servers_root
        .clone()
        .ok_or_else(|| bad("Pick a servers folder first (Settings › Panel folder)."))
}

fn templates_dir(app: &App) -> PathBuf {
    app.data_dir.join("templates")
}

/// A free port for a new server (one the router doesn't use).
fn new_port(app: &App, root: &FsPath) -> u16 {
    let router_on = app.cfg.read().unwrap().router.enabled;
    if router_on {
        crate::router::free_port(app, None).unwrap_or_else(|| hopper_core::hosting::suggest_port(root))
    } else {
        hopper_core::hosting::suggest_port(root)
    }
}

/// A cleaned address no other server has; None for "".
fn address(app: &App, a: &str) -> ApiResult<Option<String>> {
    let a = match a.trim() {
        "" => return Ok(None),
        a => hopper_core::hosting::router::clean_address(a).map_err(bad)?,
    };
    if let Some(other) = app.servers.list().into_iter().find(|e| crate::router::settings(e).addresses.contains(&a)) {
        return Err(bad(format!("{} already has {a}.", other.name)));
    }
    Ok(Some(a))
}

/// Puts a made server on the list (with its address); its id.
fn listed(app: &App, folder: &FsPath, address: Option<String>) -> anyhow::Result<Value> {
    if let Some(addr) = address {
        hopper_core::hosting::router::RouteSettings { addresses: vec![addr], ..Default::default() }.save(folder)?;
        app.router.changed();
    }
    app.rescan();
    app.refresh.notify_one();
    let id = app.servers.list().into_iter().find(|e| e.root == folder).map(|e| e.id.clone());
    Ok(json!({ "id": id, "path": folder }))
}

fn new_folder(root: &FsPath, name: &str) -> ApiResult<PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        return Err(bad("Give it a name."));
    }
    let dest = root.join(hopper_core::instances::Instance::folder_name(name));
    if dest.exists() {
        return Err(bad(format!("{} already exists.", dest.display())));
    }
    Ok(dest)
}

// ---- clone and templates -----------------------------------------------------------------

async fn clone_info(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let root = servers_root(&app)?;
    let src = e.root.clone();
    let sizes = blocking(move || Ok(clone::sizes(&src))).await?;
    let port = new_port(&app, &root);
    let domain = { let c = app.cfg.read().unwrap(); c.router.enabled.then(|| c.router.domain.clone()) };
    Ok(Json(json!({ "sizes": sizes, "port": port, "name": format!("{} copy", e.name), "domain": domain, "servers_root": root })))
}

#[derive(Deserialize)]
struct CloneBody {
    name: String,
    #[serde(default = "yes")]
    with_world: bool,
    port: Option<u16>,
    #[serde(default)]
    address: String,
}

fn yes() -> bool {
    true
}

async fn clone_server(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<CloneBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    stopped(&e)?;
    let root = servers_root(&app)?;
    let dest = new_folder(&root, &b.name)?;
    let address = address(&app, &b.address)?;
    let port = b.port.unwrap_or_else(|| new_port(&app, &root));
    let (src, name, a) = (e.root.clone(), e.name.clone(), app.clone());
    let job = crate::pack_api::spawn(&app, format!("Cloning {name}"), move |job| {
        let n = clone::clone_server(&src, &dest, b.with_world, port, job)?;
        log::info!("cloned {} to {} ({n} files, world: {})", src.display(), dest.display(), b.with_world);
        listed(&a, &dest, address)
    });
    Ok(Json(json!({ "job": job })))
}

#[derive(Deserialize)]
struct TemplateBody {
    name: String,
}

async fn save_template(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<TemplateBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    stopped(&e)?;
    let (src, from, dir) = (e.root.clone(), e.name.clone(), templates_dir(&app));
    let job = crate::pack_api::spawn(&app, format!("Saving {} as a template", e.name), move |job| {
        let t = clone::save_template(&src, &from, &dir, &b.name, job)?;
        log::info!("saved {} as template {}", src.display(), t.name);
        Ok(t)
    });
    Ok(Json(json!({ "job": job })))
}

async fn templates(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let dir = templates_dir(&app);
    let list = blocking(move || Ok(clone::templates(&dir))).await?;
    Ok(Json(json!({ "templates": list })))
}

async fn delete_template(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(tid): Path<String>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let dir = templates_dir(&app);
    blocking(move || clone::delete_template(&dir, &tid)).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct FromTemplateBody {
    template: String,
    name: String,
    memory_mb: Option<u32>,
    port: Option<u16>,
    #[serde(default)]
    address: String,
    accept_eula: bool,
}

async fn from_template(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<FromTemplateBody>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    if !b.accept_eula {
        return Err(bad("Agree to the Minecraft EULA to make a server."));
    }
    let root = servers_root(&app)?;
    let dest = new_folder(&root, &b.name)?;
    let address = address(&app, &b.address)?;
    let port = b.port.unwrap_or_else(|| new_port(&app, &root));
    let (dir, a) = (templates_dir(&app), app.clone());
    let job = crate::pack_api::spawn(&app, format!("Making {}", b.name.trim()), move |job| {
        clone::from_template(&dir, &b.template, &dest, port, b.name.trim(), true, job)?;
        if let Some(mb) = b.memory_mb {
            let mut args = hopper_core::hosting::JvmArgs::load(&dest)?;
            args.set_memory(mb);
            args.save(&dest)?;
        }
        log::info!("made {} from template {}", dest.display(), b.template);
        listed(&a, &dest, address)
    });
    Ok(Json(json!({ "job": job })))
}

// ---- version ------------------------------------------------------------------------------

async fn version(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let root = e.root.clone();
    let v = blocking(move || {
        let have = vc::installed(&root);
        let java = hopper_core::hosting::script::read_launch(&root).ok().map(|(l, _)| l.java);
        let major = java.as_deref().and_then(hopper_core::java::inspect).map(|j| j.major);
        let record = vc::Record::load(&root).map(|r| {
            let ran = r.ran_since(&root);
            let mut v = serde_json::to_value(&r).unwrap_or_default();
            v["ran_since"] = json!(ran);
            v["ok"] = json!(r.ok());
            v
        });
        Ok(json!({
            "installed": have,
            "label": have.label(),
            "pack": hopper_core::server_pack_update::recorded(&root).is_some(),
            "java": java.map(|j| j.to_string_lossy().into_owned()),
            "java_major": major,
            "record": record,
        }))
    })
    .await?;
    Ok(Json(v))
}

/// The loader's builds for the server's Minecraft (newest first) and what
/// each changed, for the loader's versions window: `{ kind, minecraft,
/// installed, offered: [version], log: [{ version, text }] }`.
async fn loader_log(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let root = e.root.clone();
    let v = blocking(move || {
        let have = vc::installed(&root);
        if have.kind.is_empty() || have.kind == "vanilla" {
            anyhow::bail!("This server has no mod loader to change.");
        }
        let prefix = format!("{}-", have.kind);
        let offered: Vec<String> = hopper_core::cf::loader_versions(&have.minecraft)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| v.name.strip_prefix(&prefix).map(str::to_string))
            .collect();
        let newest = offered.first().cloned().unwrap_or_else(|| have.version.clone());
        let log = if matches!(have.kind.as_str(), "neoforge" | "forge" | "fabric") && !newest.is_empty() {
            hopper_core::loader_changes::history(&have.kind, &have.minecraft, &newest)
                .map(|v| v.into_iter().take(80).map(|c| json!({ "version": c.version, "text": c.text })).collect::<Vec<_>>())
                .map_err(|e| format!("{e:#}"))
        } else {
            Ok(offered.iter().map(|v| json!({ "version": v, "text": "" })).collect())
        };
        Ok(json!({
            "kind": have.kind, "minecraft": have.minecraft, "installed": have.version,
            "pack": hopper_core::server_pack_update::recorded(&root).is_some(),
            "offered": offered,
            "log": log.as_ref().ok(),
            "error": log.as_ref().err(),
        }))
    })
    .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct VersionBody {
    minecraft: String,
    #[serde(default)]
    loader_version: String,
}

fn target(have: &vc::Installed, b: &VersionBody) -> ApiResult<hopper_core::hosting::ServerLoader> {
    if !b.loader_version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
        || !b.minecraft.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(bad("That isn't a version."));
    }
    if have.kind.is_empty() {
        return Err(bad("Hopper can't tell what this server runs, so it can't change its version."));
    }
    vc::loader_for(&have.kind, b.loader_version.trim()).map_err(|e| bad(format!("{e:#}")))
}

/// What changing would mean: Java, the loader's changelog, mods.
async fn version_check(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<VersionBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let have = vc::installed(&e.root);
    target(&have, &b)?;
    let pack = hopper_core::server_pack_update::recorded(&e.root).is_some();
    if pack && b.minecraft != have.minecraft {
        return Err(bad("This server came from a modpack's server pack: only its loader build can change here. A new Minecraft version comes with a pack update."));
    }
    let instance = crate::mods_api::instance_of(&e).ok();
    let root = e.root.clone();
    let job = crate::pack_api::spawn(&app, format!("Checking {} on Minecraft {}", e.name, b.minecraft), move |job| {
        let need = hopper_core::java::recommended_major(&b.minecraft);
        let java = hopper_core::hosting::script::read_launch(&root).ok().map(|(l, _)| l.java);
        let bare = java.as_deref().is_some_and(hopper_core::hosting::script::is_bare_java);
        let major = java.as_deref().and_then(hopper_core::java::inspect).map(|j| j.major);
        let installed_java = hopper_core::java::best(&hopper_core::java::detect(FsPath::new("")), &b.minecraft, hopper_core::java::Side::Server)
            .map(|r| r.install.major);
        let to_version = b.loader_version.trim().to_string();
        let changes = if b.minecraft == have.minecraft && !have.version.is_empty() && !to_version.is_empty() && matches!(have.kind.as_str(), "neoforge" | "forge" | "fabric") {
            job.detail("Reading the loader's changelog");
            hopper_core::loader_changes::between(&have.kind, &have.minecraft, &have.version, &to_version)
                .map(|v| v.into_iter().map(|c| json!({ "version": c.version, "text": c.text })).collect::<Vec<_>>())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mods: Vec<ModFit> = match &instance {
            Some(inst) if b.minecraft != have.minecraft => {
                job.detail("Looking up mods for the new version");
                let to = vc::Installed { minecraft: b.minecraft.clone(), kind: have.kind.clone(), version: to_version.clone() };
                vc::mod_check(inst, &b.minecraft, &to.instance_loader())
            }
            _ => Vec::new(),
        };
        let jars = std::fs::read_dir(root.join("mods")).into_iter().flatten().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".jar")).count();
        let sizes = hopper_core::hosting::clone::sizes(&root);
        Ok(json!({
            "same_minecraft": b.minecraft == have.minecraft,
            "java": { "need": need, "have": major, "bare": bare, "ok": bare || major.is_some_and(|m| m >= need), "installed": installed_java },
            "changes": changes,
            "mods": mods,
            "mod_count": jars,
            "backup_size": sizes.total,
        }))
    });
    Ok(Json(json!({ "job": job })))
}

#[derive(Deserialize)]
struct ChangeBody {
    minecraft: String,
    #[serde(default)]
    loader_version: String,
    /// The mod check's answer (Minecraft changes).
    #[serde(default)]
    mods: Vec<ModFit>,
    #[serde(default)]
    update_mods: bool,
    #[serde(default)]
    off_none: bool,
    #[serde(default)]
    off_unknown: bool,
}

async fn version_change(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<ChangeBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    stopped(&e)?;
    let have = vc::installed(&e.root);
    let vb = VersionBody { minecraft: b.minecraft.clone(), loader_version: b.loader_version.clone() };
    let loader = target(&have, &vb)?;
    if hopper_core::server_pack_update::recorded(&e.root).is_some() && b.minecraft != have.minecraft {
        return Err(bad("This server came from a modpack's server pack: only its loader build can change here."));
    }
    let (root, sid, name, a) = (e.root.clone(), e.id.clone(), e.name.clone(), app.clone());
    let same_mc = b.minecraft == have.minecraft;
    let job = crate::pack_api::spawn(&app, format!("Changing {name}'s version"), move |job| {
        let java = crate::manage_api::java_for(&b.minecraft, job)?;
        let change = vc::Change { minecraft: b.minecraft.clone(), loader, version: b.loader_version.trim().to_string(), java };
        let mut r = vc::change(&root, &change, job)?;
        if !same_mc && !b.mods.is_empty() {
            let notes = vc::apply_mods(&root, &b.mods, b.update_mods, b.off_none, b.off_unknown, job);
            r.notes.extend(notes);
            r.save(&root)?;
        }
        crate::mods_api::forget_updates(&sid);
        a.servers.reload_info(&sid);
        a.refresh.notify_one();
        log::info!("{name}: {} on {} → {} on {}", r.from.label(), r.from.minecraft, r.to.label(), r.to.minecraft);
        Ok(r)
    });
    Ok(Json(json!({ "job": job })))
}

async fn version_undo(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    stopped(&e)?;
    if vc::Record::load(&e.root).is_none() {
        return Err(bad("There's no version change to undo."));
    }
    let (root, sid, name, a) = (e.root.clone(), e.id.clone(), e.name.clone(), app.clone());
    let job = crate::pack_api::spawn(&app, format!("Undoing {name}'s version change"), move |job| {
        let r = vc::undo(&root, job)?;
        crate::mods_api::forget_updates(&sid);
        a.servers.reload_info(&sid);
        a.refresh.notify_one();
        log::info!("{name}: back to {} on {}", r.from.label(), r.from.minecraft);
        Ok(json!({ "label": r.from.label(), "minecraft": r.from.minecraft }))
    });
    Ok(Json(json!({ "job": job })))
}
