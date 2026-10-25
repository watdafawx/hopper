//! A server's mods, managed like an instance's: Hopper keeps a `hopper.json`
//! in the server folder (as in an instance), so identifying jars, searching,
//! installing with dependencies, turning mods on and off, removing and
//! updating all use hopper-core's instance code.
//!
//! Pack mods stay with the pack: jars that were there when the server was
//! first identified (on a server made from a server pack) are marked as the
//! pack's and only change through "Update from server pack". Single-mod
//! updates are for mods added on top.

use crate::api::{ApiResult, bad, not_found, require};
use crate::servers::Entry;
use crate::App;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use hopper_core::cf::{self, Loader};
use hopper_core::instances::{self, Addon, Instance};
use hopper_core::{install, modrinth};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

/// Mod ids go to the browser as strings: Modrinth ids are mapped to 64-bit
/// numbers, which JavaScript rounds (it's exact only up to 2^53).
fn id_str(id: u64) -> String {
    id.to_string()
}

/// Takes a mod id as a string or a number.
fn de_id<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Id {
        N(u64),
        S(String),
    }
    match Id::deserialize(d)? {
        Id::N(n) => Ok(n),
        Id::S(s) => s.trim().parse().map_err(serde::de::Error::custom),
    }
}

fn de_ids<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u64>, D::Error> {
    #[derive(Deserialize)]
    struct W(#[serde(deserialize_with = "de_id")] u64);
    Ok(Vec::<W>::deserialize(d)?.into_iter().map(|w| w.0).collect())
}

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/servers/{id}/mods/list", get(list))
        .route("/api/v1/servers/{id}/mods/meta", put(set_meta))
        .route("/api/v1/servers/{id}/mods/identify", post(identify))
        .route("/api/v1/servers/{id}/mods/toggle", post(toggle))
        .route("/api/v1/servers/{id}/mods/remove", post(remove))
        .route("/api/v1/servers/{id}/mods/search", get(search))
        .route("/api/v1/servers/{id}/mods/install", post(install_mod))
        .route("/api/v1/servers/{id}/mods/updates", get(updates))
        .route("/api/v1/servers/{id}/mods/update", post(update))
        .route("/api/v1/settings/curseforge", get(cf_key_status).put(set_cf_key))
}

fn entry(app: &App, headers: &HeaderMap, method: &Method, id: &str) -> ApiResult<Arc<Entry>> {
    require(app, headers, method)?;
    app.servers.get(id).ok_or_else(|| not_found("server"))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(bad)?.map_err(|e| bad(format!("{e:#}")))
}

/// Minecraft version for a NeoForge version: 21.1.259 → 1.21.1,
/// 21.0.167 → 1.21, 26.3.0.95 → 26.3.
pub fn mc_for_neoforge(v: &str) -> Option<String> {
    let p: Vec<&str> = v.split('.').collect();
    match p.as_slice() {
        [a, b, _, _] => Some(if *b == "0" { a.to_string() } else { format!("{a}.{b}") }).map(|s| {
            // 26.1.2.x: the third part is Minecraft's patch.
            if p[2] != "0" { format!("{}.{}.{}", p[0], p[1], p[2]) } else { s }
        }),
        [a, b, _] => Some(if *b == "0" { format!("1.{a}") } else { format!("1.{a}.{b}") }),
        _ => None,
    }
}

/// What the server runs, from what Hopper recorded when making it, else
/// its run script and console.
fn detect(e: &Entry) -> (String, String) {
    if let Some(m) = hopper_core::hosting::HostedMeta::load(&e.root) {
        let l = m.loader.to_lowercase();
        let loader = if let Some(v) = l.strip_prefix("neoforge ") {
            format!("neoforge-{v}")
        } else if l.starts_with("fabric") {
            "fabric".into()
        } else if l.starts_with("quilt") {
            "quilt".into()
        } else {
            String::new()
        };
        if !m.minecraft.is_empty() {
            return (m.minecraft, loader);
        }
    }
    if let Some(v) = hopper_core::server_update::server_neoforge(&e.root) {
        return (mc_for_neoforge(&v).unwrap_or_default(), format!("neoforge-{v}"));
    }
    let console_version = e.server.tracker().version;
    let loader = if e.root.join("fabric-server-launch.jar").is_file() {
        "fabric"
    } else if e.root.join("quilt-server-launch.jar").is_file() {
        "quilt"
    } else {
        ""
    };
    (console_version, loader.into())
}

/// The server as an instance, made the first time.
/// Mod files are picked by Minecraft version; without one the newest file
/// would be picked, whatever version it's for.
fn known_version(inst: &Instance) -> anyhow::Result<()> {
    anyhow::ensure!(
        !inst.game_version.is_empty(),
        "Hopper doesn't know this server's Minecraft version yet. Start it once, or set the version in the Mods tab."
    );
    Ok(())
}

fn instance(e: &Entry) -> anyhow::Result<Instance> {
    if e.root.join(instances::HOPPER_FILE).is_file()
        && let Some(mut i) = instances::load(&e.root)
    {
        // Saved before the version was known (a new server that hadn't run):
        // fill it in now if it can be found.
        if i.game_version.is_empty() {
            let (game_version, loader) = detect(e);
            if !game_version.is_empty() {
                i.game_version = game_version;
                if i.loader.is_empty() {
                    i.loader = loader;
                }
                i.save()?;
            }
        }
        return Ok(i);
    }
    let (game_version, loader) = detect(e);
    let pack = hopper_core::server_pack_update::recorded(&e.root).map(|m| instances::PackRef {
        project_id: m.project_id,
        file_id: m.pack_file_id,
        name: m.server_pack.clone(),
        server_pack_file_id: m.server_pack_file_id,
        ..Default::default()
    });
    let i = Instance {
        name: e.name.clone(),
        root: e.root.clone(),
        game_version,
        loader,
        pack,
        format: instances::FORMAT,
        ..Default::default()
    };
    i.save()?;
    Ok(i)
}

fn side_label(env: &str) -> &str {
    match env {
        "client" => "client",
        "server" => "server",
        "both" => "both",
        _ => "",
    }
}

async fn list(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let v = blocking(move || {
        let inst = instance(&e)?;
        let dir = inst.mods_dir();
        // What the jars say about themselves (name, version, side).
        let scanned: HashMap<String, hopper_core::model::ModInfo> = if dir.is_dir() {
            hopper_core::scan::scan_folder(&dir)?.into_iter().map(|m| (m.file_name.to_lowercase(), m)).collect()
        } else {
            HashMap::new()
        };
        let mut rows: Vec<Value> = Vec::new();
        let mut known = std::collections::HashSet::new();
        for a in &inst.addons {
            if a.folder() != "mods" {
                continue;
            }
            known.insert(a.file_name.to_lowercase());
            let s = scanned.get(&a.file_name.to_lowercase());
            rows.push(json!({
                "file": a.file_name, "name": if a.name.is_empty() { s.map(|s| s.mod_name.clone()).unwrap_or_default() } else { a.name.clone() },
                "version": s.map(|s| s.version.clone()).unwrap_or_default(), "icon": a.icon, "enabled": a.enabled,
                "side": if a.env.is_empty() { s.map(|s| s.side.clone()).unwrap_or_default() } else { side_label(&a.env).to_string() },
                "project_id": id_str(a.project_id), "source": if a.project_id == 0 { "" } else if modrinth::is_mr(a.project_id) { "modrinth" } else { "curseforge" },
                "from_pack": a.from_pack, "page": a.page, "author": a.author,
            }));
        }
        // Jars Hopper doesn't know yet.
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let lower = name.to_lowercase();
            let enabled = lower.ends_with(".jar");
            if !(enabled || lower.ends_with(".jar.disabled")) {
                continue;
            }
            let base = name.trim_end_matches(".disabled").to_string();
            if known.contains(&base.to_lowercase()) {
                continue;
            }
            let s = scanned.get(&base.to_lowercase());
            rows.push(json!({
                "file": base, "name": s.map(|s| s.mod_name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| base.clone()),
                "version": s.map(|s| s.version.clone()).unwrap_or_default(), "icon": "", "enabled": enabled,
                "side": s.map(|s| s.side.clone()).unwrap_or_default(), "project_id": "0", "source": "", "from_pack": false, "page": "", "author": "",
            }));
        }
        rows.sort_by_key(|r| r["name"].as_str().unwrap_or("").to_lowercase());
        Ok(json!({
            "game_version": inst.game_version, "loader": inst.loader,
            "pack": inst.pack.as_ref().map(|p| p.name.clone()),
            "curseforge": cf::api_key().is_some(),
            "mods": rows,
        }))
    })
    .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct MetaBody {
    game_version: String,
    loader: String,
}

async fn set_meta(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<MetaBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    blocking(move || {
        instance(&e)?;
        Instance::update_on_disk(&e.root, |i| {
            i.game_version = b.game_version.trim().to_string();
            i.loader = b.loader.trim().to_lowercase();
        })?;
        Ok(())
    })
    .await?;
    Ok(Json(json!({ "ok": true })))
}

/// Names jars by their CurseForge fingerprint (with a key), then Modrinth's
/// SHA-1 for the rest. On a server made from a pack, the jars found the
/// first time are the pack's.
async fn identify(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let job = crate::pack_api::spawn(&app, format!("Identifying {}'s mods", e.name), move |job| {
        let mut inst = instance(&e)?;
        let first = inst.addons.is_empty();
        let before = inst.addons.len();
        if cf::api_key().is_some() {
            if let Err(err) = install::identify_loose(&mut inst, job) {
                log::warn!("CurseForge identify: {err:#}");
            }
            inst.save()?;
        }
        // Modrinth, by SHA-1, for what's left.
        let loose = install::loose_jars(&inst);
        if !loose.is_empty() {
            job.detail("Asking Modrinth");
            let mut by_hash = HashMap::new();
            for name in &loose {
                let bytes = std::fs::read(inst.mods_dir().join(name))?;
                by_hash.insert(modrinth::sha1(&bytes), name.clone());
            }
            let hashes: Vec<String> = by_hash.keys().cloned().collect();
            let versions = modrinth::versions_by_sha1(&hashes)?;
            let ids: Vec<String> = versions.values().map(|v| v.project_id.clone()).collect();
            let projects = modrinth::projects(&ids)?;
            for (hash, v) in versions {
                let (Some(name), Some(p)) = (by_hash.get(&hash), projects.iter().find(|p| p.id == v.project_id)) else { continue };
                let m = modrinth::project_to_mod(p, "");
                let mut f = modrinth::version_to_file(&v);
                f.file_name = name.clone();
                let mut a = install::addon_from(&m, &f, false);
                a.enabled = true;
                inst.addons.push(a);
            }
        }
        if first && inst.pack.is_some() {
            for a in &mut inst.addons {
                a.from_pack = true;
            }
        }
        inst.save()?;
        Ok(json!({ "identified": inst.addons.len() - before, "unknown": install::loose_jars(&inst).len() }))
    });
    Ok(Json(json!({ "job": job })))
}

#[derive(Deserialize)]
struct FileBody {
    file: String,
    #[serde(default)]
    enabled: bool,
}

/// The mod's index in the instance, adding an unknown jar as a bare entry
/// so it can be turned off or removed too.
fn index_of(inst: &mut Instance, file: &str) -> anyhow::Result<usize> {
    if let Some(i) = inst.addons.iter().position(|a| a.file_name.eq_ignore_ascii_case(file) && a.folder() == "mods") {
        return Ok(i);
    }
    let on = inst.mods_dir().join(file).is_file();
    let off = inst.mods_dir().join(format!("{file}.disabled")).is_file();
    anyhow::ensure!(on || off, "{file} isn't in mods/");
    inst.addons.push(Addon {
        file_name: file.to_string(),
        name: file.to_string(),
        enabled: on,
        class_id: cf::CLASS_MODS,
        ..Default::default()
    });
    Ok(inst.addons.len() - 1)
}

async fn toggle(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<FileBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let running = e.server.state().is_alive();
    blocking(move || {
        let mut inst = instance(&e)?;
        let i = index_of(&mut inst, &b.file)?;
        install::set_enabled(&mut inst, i, b.enabled).map_err(install::explain_locked)
    })
    .await?;
    Ok(Json(json!({ "ok": true, "restart_needed": running })))
}

async fn remove(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<FileBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let running = e.server.state().is_alive();
    log::info!("{}: removing {}", e.name, b.file);
    blocking(move || {
        let mut inst = instance(&e)?;
        let i = index_of(&mut inst, &b.file)?;
        install::remove(&mut inst, i).map_err(install::explain_locked)
    })
    .await?;
    Ok(Json(json!({ "ok": true, "restart_needed": running })))
}

fn loader_of(s: &str) -> Loader {
    let l = s.to_lowercase();
    if l.starts_with("neoforge") {
        Loader::NeoForge
    } else if l.starts_with("forge") {
        Loader::Forge
    } else if l.starts_with("fabric") {
        Loader::Fabric
    } else if l.starts_with("quilt") {
        Loader::Quilt
    } else {
        Loader::Any
    }
}

async fn search(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let text = q.get("q").cloned().unwrap_or_default();
    let use_mr = q.get("source").is_none_or(|s| s != "curseforge");
    let page: u32 = q.get("page").and_then(|p| p.parse().ok()).unwrap_or(0);
    let v = blocking(move || {
        if !use_mr && cf::api_key().is_none() {
            anyhow::bail!("CurseForge needs an API key: add one in Access & devices, or search Modrinth.");
        }
        let inst = instance(&e)?;
        let s = cf::Search {
            class_id: cf::CLASS_MODS,
            text,
            game_version: inst.game_version.clone(),
            loader: loader_of(&inst.loader),
            index: page * 20,
            ..Default::default()
        };
        let (mods, _) = hopper_core::source::search(&s, use_mr)?;
        let have: std::collections::HashSet<u64> = inst.addons.iter().map(|a| a.project_id).collect();
        Ok(json!(mods
            .iter()
            .map(|m| json!({
                "id": id_str(m.id), "name": m.name, "summary": m.summary, "downloads": m.download_count,
                "icon": m.logo.as_ref().map(|l| l.thumbnail_url.clone()).unwrap_or_default(),
                "author": m.authors.first().map(|a| a.name.clone()).unwrap_or_default(),
                "installed": have.contains(&m.id),
            }))
            .collect::<Vec<_>>()))
    })
    .await?;
    Ok(Json(json!({ "results": v })))
}

#[derive(Deserialize)]
struct InstallBody {
    #[serde(deserialize_with = "de_id")]
    project_id: u64,
}

async fn install_mod(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<InstallBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let job = crate::pack_api::spawn(&app, format!("Adding a mod to {}", e.name), move |job| {
        let mut inst = instance(&e)?;
        known_version(&inst)?;
        let m = hopper_core::source::get_mod(b.project_id)?;
        job.detail(format!("Finding {} for Minecraft {}", m.name, inst.game_version));
        let file = install::best_file(m.id, m.class_id, &inst)?
            .ok_or_else(|| anyhow::anyhow!("{} has no file for Minecraft {} with this loader.", m.name, inst.game_version))?;
        let added = install::install(&mut inst, &m, &file, job)?;
        log::info!("{}: added {}", e.name, added.join(", "));
        Ok(json!({ "added": added }))
    });
    Ok(Json(json!({ "job": job })))
}

async fn updates(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let v = blocking(move || {
        if cf::api_key().is_none() {
            anyhow::bail!("Checking updates needs a CurseForge API key (Access & devices).");
        }
        let inst = instance(&e)?;
        let ups = install::check_updates(&inst)?;
        Ok(json!(ups
            .iter()
            .filter(|u| inst.addons.iter().any(|a| a.project_id == u.project_id && !a.from_pack))
            .map(|u| {
                let (from, to) = u.versions();
                json!({ "project_id": id_str(u.project_id), "name": u.name, "from": from, "to": to, "file": u.file.file_name })
            })
            .collect::<Vec<_>>()))
    })
    .await?;
    Ok(Json(json!({ "updates": v })))
}

#[derive(Deserialize)]
struct UpdateBody {
    #[serde(deserialize_with = "de_ids")]
    project_ids: Vec<u64>,
}

async fn update(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let job = crate::pack_api::spawn(&app, format!("Updating {}'s mods", e.name), move |job| {
        let mut inst = instance(&e)?;
        let ups: Vec<install::Update> = install::check_updates(&inst)?
            .into_iter()
            .filter(|u| b.project_ids.contains(&u.project_id))
            .filter(|u| inst.addons.iter().any(|a| a.project_id == u.project_id && !a.from_pack))
            .collect();
        let done = install::apply_updates(&mut inst, &ups, job)?;
        Ok(json!({ "updated": done }))
    });
    Ok(Json(json!({ "job": job })))
}

#[derive(Deserialize)]
struct KeyBody {
    key: String,
}

/// The CurseForge key the panel uses (kept in hopperd.toml).
/// Whether the panel has a CurseForge key (never the key itself).
async fn cf_key_status(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    crate::api::require(&app, &headers, &method)?;
    let saved = !app.cfg.read().unwrap().curseforge_key.trim().is_empty();
    Ok(Json(json!({ "set": cf::api_key().is_some(), "saved": saved })))
}

async fn set_cf_key(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<KeyBody>) -> ApiResult<Json<Value>> {
    crate::api::require_owner(&app, &headers, &method)?;
    let key = b.key.trim().to_string();
    if !key.is_empty() {
        blocking({
            let k = key.clone();
            move || cf::test_key(&k)
        })
        .await?;
    }
    {
        let mut cfg = app.cfg.write().unwrap();
        cfg.curseforge_key = key.clone();
        cfg.save(&app.data_dir).map_err(|e| bad(format!("{e:#}")))?;
    }
    cf::set_session_key(&key);
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::mc_for_neoforge;

    #[test]
    fn neoforge_to_minecraft() {
        assert_eq!(mc_for_neoforge("21.1.259").as_deref(), Some("1.21.1"));
        assert_eq!(mc_for_neoforge("21.0.167").as_deref(), Some("1.21"));
        assert_eq!(mc_for_neoforge("20.4.237").as_deref(), Some("1.20.4"));
        assert_eq!(mc_for_neoforge("26.3.0.95").as_deref(), Some("26.3"));
        assert_eq!(mc_for_neoforge("26.1.2.5").as_deref(), Some("26.1.2"));
        assert_eq!(mc_for_neoforge("x"), None);
    }
}
