//! Adding and removing servers, beyond a fresh one (manage_api's create):
//! a server folder on this machine (run where it is, or copied in), a
//! server pack or client pack zip (uploaded, or already on this machine),
//! a CurseForge or Modrinth pack; removing a server from the panel or
//! deleting it (to `.hopper-deleted` in the servers folder, kept 30 days);
//! and choosing the servers folder, adding what's already in it.
//!
//! Browsing this machine's folders, the servers folder and deleting are
//! the owner's.

use crate::App;
use crate::api::{ApiResult, bad, not_found, require, require_owner};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use hopper_core::hosting::zip_import;
use hopper_core::hosting::{CreateOptions, ServerLoader};
use hopper_core::{browse, cf, modrinth, source};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

/// Deleted servers wait here, inside the servers folder (a rename on the
/// same drive, so instant), until restored or 30 days pass.
pub const DELETED_DIR: &str = ".hopper-deleted";
const KEEP_DELETED_DAYS: u64 = 30;
const INCOMING: &str = "incoming";

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/host/browse", get(browse_dirs))
        .route("/api/v1/host/scan", get(scan))
        .route("/api/v1/settings/servers-folder", get(folder_get).put(folder_set))
        .route("/api/v1/servers/import-folder", post(import_folder))
        .route("/api/v1/incoming/uploads", post(upload_start))
        .route("/api/v1/incoming/uploads/{up}", get(upload_status).put(upload_chunk).delete(upload_cancel))
        .route("/api/v1/servers/zip-info", get(zip_info))
        .route("/api/v1/servers/import-zip", post(import_zip))
        .route("/api/v1/packs/search", get(pack_search))
        .route("/api/v1/packs/versions", get(pack_versions))
        .route("/api/v1/packs/check", get(pack_check))
        .route("/api/v1/servers/create-from-pack", post(create_from_pack))
        .route("/api/v1/servers/{id}/remove", post(remove))
        .route("/api/v1/deleted/{name}/restore", put(restore))
        .route("/api/v1/deleted/{name}", axum::routing::delete(purge))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(bad)?.map_err(|e| bad(format!("{e:#}")))
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn servers_root(app: &App) -> ApiResult<PathBuf> {
    app.cfg
        .read()
        .unwrap()
        .servers_root
        .clone()
        .ok_or_else(|| bad("Pick a servers folder first (Settings › Panel folder)."))
}

fn save_cfg(app: &App) -> ApiResult<()> {
    let c = app.cfg.read().unwrap().clone();
    c.save(&app.data_dir).map_err(|e| bad(format!("saving settings: {e:#}")))
}

fn same_path(a: &FsPath, b: &FsPath) -> bool {
    let n = |p: &FsPath| p.to_string_lossy().trim_end_matches(['\\', '/']).replace('\\', "/").to_lowercase();
    n(a) == n(b)
}

fn is_zip(name: &str) -> bool {
    let l = name.to_lowercase();
    l.ends_with(".zip") || l.ends_with(".mrpack")
}

// ---- browsing this machine ------------------------------------------------------

/// Folders (and zips) in `path`; with no path, the drives (Windows) or `/`.
async fn browse_dirs(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    // With a panel folder, nothing outside it is listed: it's where
    // browsing starts and Up stops.
    let home = crate::home::home(&app);
    let mut path = q.get("path").cloned().unwrap_or_default();
    if let Some(h) = &home {
        if path.trim().is_empty() {
            path = h.to_string_lossy().into_owned();
        }
        crate::home::check(&app, FsPath::new(&path))?;
    }
    let v = blocking(move || {
        if path.trim().is_empty() {
            let roots: Vec<Value> = if cfg!(windows) {
                (b'A'..=b'Z')
                    .map(|c| format!("{}:\\", c as char))
                    .filter(|d| FsPath::new(d).is_dir())
                    .map(|d| json!({ "name": d, "path": d, "dir": true }))
                    .collect()
            } else {
                vec![json!({ "name": "/", "path": "/", "dir": true })]
            };
            return Ok(json!({ "path": "", "parent": null, "entries": roots }));
        }
        let dir = PathBuf::from(&path);
        anyhow::ensure!(dir.is_dir(), "{path} isn't a folder on this machine.");
        let mut entries: Vec<Value> = Vec::new();
        for e in std::fs::read_dir(&dir)?.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('$') || name.starts_with('.') {
                continue;
            }
            let p = e.path();
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                entries.push(json!({ "name": name, "path": p, "dir": true, "server": p.join("server.properties").is_file() }));
            } else if is_zip(&name) {
                entries.push(json!({ "name": name, "path": p, "dir": false, "size": e.metadata().map(|m| m.len()).unwrap_or(0) }));
            }
        }
        entries.sort_by_key(|e| (!e["dir"].as_bool().unwrap_or(false), e["name"].as_str().unwrap_or("").to_lowercase()));
        let parent = dir
            .parent()
            .filter(|_| home.as_deref().is_none_or(|h| !crate::home::inside(&dir, h)))
            .map(|p| p.to_string_lossy().into_owned());
        Ok(json!({ "path": dir, "parent": parent, "entries": entries }))
    })
    .await?;
    Ok(Json(v))
}

/// What `path` holds for the panel: server folders (with their loader and
/// whether the panel has them already), server or client pack zips, and
/// folders that aren't servers.
fn scan_folder(app: &App, dir: &FsPath) -> anyhow::Result<Value> {
    anyhow::ensure!(dir.is_dir(), "{} isn't a folder on this machine.", dir.display());
    let known: Vec<PathBuf> = app.servers.list().iter().map(|e| e.root.clone()).collect();
    let (mut servers, mut zips, mut other) = (Vec::new(), Vec::new(), Vec::new());
    for e in std::fs::read_dir(dir)?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name.starts_with('$') {
            continue;
        }
        let p = e.path();
        if p.is_dir() {
            if p.join("server.properties").is_file() {
                let info = hopper_core::server::load(&p);
                servers.push(json!({
                    "name": name, "path": p, "loader": info.loader,
                    "already": known.iter().any(|k| same_path(k, &p)),
                    "managed_by": match info.manager { hopper_core::server::Manager::Crafty => Some("Crafty"), hopper_core::server::Manager::Mcss => Some("MCSS"), _ => None },
                }));
            } else {
                other.push(name);
            }
        } else if is_zip(&name) {
            match zip_import::inspect(&p) {
                Ok(i) => zips.push(json!({
                    "name": name, "path": p, "kind": i.kind.key(), "server_name": i.name,
                    "mc_version": i.target.as_ref().map(|t| t.mc_version.clone()),
                    "loader": i.target.as_ref().map(|t| t.loader.clone()),
                })),
                Err(err) => zips.push(json!({ "name": name, "path": p, "kind": null, "error": format!("{err:#}") })),
            }
        }
    }
    let by_name = |a: &Value, b: &Value| a["name"].as_str().unwrap_or("").to_lowercase().cmp(&b["name"].as_str().unwrap_or("").to_lowercase());
    servers.sort_by(by_name);
    zips.sort_by(by_name);
    other.sort_by_key(|n| n.to_lowercase());
    Ok(json!({ "path": dir, "servers": servers, "zips": zips, "other": other }))
}

async fn scan(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let dir = PathBuf::from(q.get("path").cloned().unwrap_or_default());
    crate::home::check(&app, &dir)?;
    let a = app.clone();
    Ok(Json(blocking(move || scan_folder(&a, &dir)).await?))
}

// ---- the servers folder ---------------------------------------------------------------

fn deleted_list(root: &FsPath) -> Vec<Value> {
    let mut v: Vec<Value> = std::fs::read_dir(root.join(DELETED_DIR))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| {
            let p = e.path();
            let meta: Value = std::fs::read(p.join(".hopper-deleted.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
            let at = meta["at"].as_u64().unwrap_or(0);
            json!({
                "name": e.file_name().to_string_lossy(), "server": meta["name"], "from": meta["from"], "at": at,
                "expires": at + KEEP_DELETED_DAYS * 86400,
            })
        })
        .collect();
    v.sort_by_key(|d| std::cmp::Reverse(d["at"].as_u64().unwrap_or(0)));
    v
}

/// Deleted servers past their 30 days go for good (at start).
pub fn purge_old(root: Option<&FsPath>) {
    let Some(root) = root else { return };
    for d in deleted_list(root) {
        if d["expires"].as_u64().is_some_and(|e| e < now())
            && let Some(n) = d["name"].as_str()
        {
            let p = root.join(DELETED_DIR).join(n);
            log::info!("removing {} (deleted over {KEEP_DELETED_DAYS} days ago)", p.display());
            let _ = std::fs::remove_dir_all(p);
        }
    }
}

async fn folder_get(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let c = app.cfg.read().unwrap().clone();
    let deleted = c.servers_root.as_deref().map(deleted_list).unwrap_or_default();
    Ok(Json(json!({ "home": c.home, "path": c.servers_root, "extra": c.servers, "hidden": c.hidden, "deleted": deleted })))
}

#[derive(Deserialize)]
struct FolderBody {
    path: String,
    /// Server folders in it to leave out of the panel.
    #[serde(default)]
    skip: Vec<String>,
    /// Zips in it to make servers from.
    #[serde(default)]
    zips: Vec<String>,
    #[serde(default)]
    accept_eula: bool,
}

/// Makes `path` the servers folder: every server folder in it shows up
/// (except `skip`), the zips picked become servers, and the servers of the
/// old folder stay listed where they are.
async fn folder_set(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<FolderBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let dir = crate::absolute(FsPath::new(b.path.trim()));
    crate::home::check(&app, &dir)?;
    if !dir.is_dir() {
        std::fs::create_dir_all(&dir).map_err(|e| bad(format!("creating {}: {e}", dir.display())))?;
    }
    if !b.zips.is_empty() && !b.accept_eula {
        return Err(bad("Agree to the Minecraft EULA to make servers from the zips."));
    }
    {
        let mut c = app.cfg.write().unwrap();
        if let Some(old) = c.servers_root.clone().filter(|o| !same_path(o, &dir)) {
            // Keep the old folder's servers: they run where they are now.
            for e in app.servers.list() {
                if e.root.starts_with(&old) && !c.servers.iter().any(|s| same_path(s, &e.root)) {
                    c.servers.push(e.root.clone());
                }
            }
        }
        c.servers_root = Some(dir.clone());
        c.servers.retain(|s| !s.starts_with(&dir) || s.parent().is_some_and(|p| !same_path(p, &dir)));
        for s in &b.skip {
            let p = PathBuf::from(s);
            if !c.hidden.iter().any(|h| same_path(h, &p)) {
                c.hidden.push(p);
            }
        }
    }
    save_cfg(&app)?;
    log::info!("servers folder is now {}", dir.display());
    app.rescan();
    app.refresh.notify_one();
    let mut jobs = Vec::new();
    for z in b.zips {
        crate::home::check(&app, FsPath::new(&z))?;
        let body = ZipBody { path: Some(z), accept_eula: true, ..Default::default() };
        jobs.push(start_zip_job(&app, body)?);
    }
    Ok(Json(json!({ "ok": true, "servers": app.servers.list().len(), "jobs": jobs })))
}

// ---- a folder -----------------------------------------------------------------------------

#[derive(Deserialize)]
struct ImportFolderBody {
    path: String,
    /// Copy it into the servers folder instead of running it where it is.
    #[serde(default)]
    copy: bool,
    #[serde(default)]
    name: String,
}

async fn import_folder(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<ImportFolderBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let src = crate::absolute(FsPath::new(b.path.trim()));
    crate::home::check(&app, &src)?;
    if !src.join("server.properties").is_file() {
        return Err(bad(format!("{} has no server.properties, so it isn't a server folder.", src.display())));
    }
    if app.servers.list().iter().any(|e| same_path(&e.root, &src)) {
        return Err(bad("The panel has that server already."));
    }
    if !b.copy {
        {
            let mut c = app.cfg.write().unwrap();
            c.hidden.retain(|h| !same_path(h, &src));
            if !c.servers_root.as_deref().is_some_and(|r| src.parent().is_some_and(|p| same_path(p, r))) {
                c.servers.push(src.clone());
            }
        }
        save_cfg(&app)?;
        app.rescan();
        app.refresh.notify_one();
        let id = app.servers.list().into_iter().find(|e| same_path(&e.root, &src)).map(|e| e.id.clone());
        log::info!("added {} (runs where it is)", src.display());
        return Ok(Json(json!({ "id": id })));
    }
    let root = servers_root(&app)?;
    let name = if b.name.trim().is_empty() { src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() } else { b.name.trim().to_string() };
    let dest = root.join(hopper_core::instances::Instance::folder_name(&name));
    if dest.exists() {
        return Err(bad(format!("{} already exists.", dest.display())));
    }
    let a = app.clone();
    let job = crate::pack_api::spawn(&app, format!("Copying {name}"), move |job| {
        job.detail(format!("Copying {} to {}", src.display(), dest.display()));
        let r = hopper_core::hosting::create::copy_tree(&src, &dest);
        if let Err(e) = r {
            let _ = std::fs::remove_dir_all(&dest);
            return Err(e);
        }
        a.rescan();
        a.refresh.notify_one();
        let id = a.servers.list().into_iter().find(|e| e.root == dest).map(|e| e.id.clone());
        Ok(json!({ "id": id, "path": dest }))
    });
    Ok(Json(json!({ "job": job })))
}

// ---- zips --------------------------------------------------------------------------------

fn incoming(app: &App) -> PathBuf {
    let d = app.data_dir.join(INCOMING);
    let _ = std::fs::create_dir_all(&d);
    d
}

#[derive(Deserialize)]
struct StartBody {
    path: String,
    size: u64,
}

async fn upload_start(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<StartBody>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    if !is_zip(&b.path) || b.path.contains(['/', '\\']) {
        return Err(bad("Upload a .zip or .mrpack."));
    }
    let dir = incoming(&app);
    let up = blocking(move || crate::files::upload_start(&dir, &b.path, b.size, true)).await?;
    Ok(Json(json!({ "id": up.id, "offset": 0, "path": up.path })))
}

async fn upload_status(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(up): Path<String>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let dir = incoming(&app);
    let (u, have) = blocking(move || crate::files::upload_status(&dir, &up)).await?;
    Ok(Json(json!({ "id": u.id, "path": u.path, "size": u.size, "offset": have })))
}

async fn upload_chunk(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(up): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<Response> {
    require(&app, &headers, &method)?;
    let dir = incoming(&app);
    let offset: u64 = q.get("offset").and_then(|o| o.parse().ok()).ok_or_else(|| bad("offset is required"))?;
    match blocking(move || crate::files::upload_chunk(&dir, &up, offset, &body)).await? {
        Ok(r) => Ok(Json(r).into_response()),
        Err(have) => Ok((StatusCode::CONFLICT, Json(json!({ "error": "offset mismatch", "offset": have }))).into_response()),
    }
}

async fn upload_cancel(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(up): Path<String>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let dir = incoming(&app);
    blocking(move || crate::files::upload_cancel(&dir, &up)).await?;
    Ok(Json(json!({ "ok": true })))
}

/// The zip a request means: an upload (by its file name in the incoming
/// folder) or a path on this machine (the owner's).
fn zip_path(app: &App, headers: &HeaderMap, method: &Method, upload: Option<&str>, path: Option<&str>) -> ApiResult<PathBuf> {
    match (upload.filter(|u| !u.is_empty()), path.filter(|p| !p.is_empty())) {
        (Some(u), _) => {
            require(app, headers, method)?;
            if u.contains(['/', '\\']) || u.contains("..") {
                return Err(bad("Which upload?"));
            }
            let p = incoming(app).join(u);
            p.is_file().then_some(p).ok_or_else(|| not_found("upload"))
        }
        (None, Some(p)) => {
            require_owner(app, headers, method)?;
            let p = PathBuf::from(p);
            crate::home::check(app, &p)?;
            if !p.is_file() || !is_zip(&p.to_string_lossy()) {
                return Err(bad(format!("{} isn't a zip on this machine.", p.display())));
            }
            Ok(p)
        }
        _ => Err(bad("Which zip?")),
    }
}

async fn zip_info(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let p = zip_path(&app, &headers, &method, q.get("upload").map(String::as_str), q.get("path").map(String::as_str))?;
    let i = blocking(move || zip_import::inspect(&p)).await?;
    Ok(Json(json!({
        "kind": i.kind.key(), "name": i.name,
        "mc_version": i.target.as_ref().map(|t| t.mc_version.clone()),
        "loader": i.target.as_ref().map(|t| t.loader.clone()),
        "recommended_mb": i.target.as_ref().map(|t| t.recommended_mb).filter(|m| *m > 0),
    })))
}

#[derive(Deserialize, Default)]
struct ZipBody {
    #[serde(default)]
    upload: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    name: String,
    memory_mb: Option<u32>,
    port: Option<u16>,
    #[serde(default)]
    address: String,
    #[serde(default)]
    accept_eula: bool,
    /// For a server pack that doesn't say what it runs on.
    #[serde(default)]
    mc_version: String,
    /// Hopper's loader id ("neoforge-21.1.219", "fabric-0.16.9", "" vanilla).
    #[serde(default)]
    loader: Option<String>,
}

/// What every way of making a server shares: the address (checked now)
/// and the port to give it (one the router doesn't use).
struct Common {
    root: PathBuf,
    address: Option<String>,
    auto_port: Option<u16>,
}

fn common(app: &App, address: &str, port: Option<u16>, accept_eula: bool) -> ApiResult<Common> {
    if !accept_eula {
        return Err(bad("Agree to the Minecraft EULA to make a server."));
    }
    let root = servers_root(app)?;
    let address = match address.trim() {
        "" => None,
        a => Some(hopper_core::hosting::router::clean_address(a).map_err(bad)?),
    };
    if let Some(a) = &address
        && let Some(other) = crate::router::taken_by(&app, a, None)
    {
        return Err(bad(format!("{other} already has {a}.")));
    }
    let router_port = {
        let c = app.cfg.read().unwrap();
        c.router.enabled.then_some(c.router.port)
    };
    let auto_port = match (port, router_port) {
        (None, Some(rp)) => crate::router::free_port(app, None).filter(|_| hopper_core::hosting::suggest_port(&root) == rp),
        _ => None,
    };
    Ok(Common { root, address, auto_port })
}

fn options(c: &Common, name: &str, mc: &str, loader: ServerLoader, java: &FsPath, memory_mb: Option<u32>, port: Option<u16>) -> CreateOptions {
    let mut o = CreateOptions::new(name.trim(), &c.root, mc, loader, java);
    if let Some(m) = memory_mb {
        o.memory_mb = m;
    }
    if let Some(p) = port.or(c.auto_port) {
        o.port = p;
    }
    o.accept_eula = true;
    o
}

/// After any create: the address, the client-only mods left out, the list.
fn finish(app: &App, folder: &FsPath, address: Option<String>, skipped: &[(String, String)]) -> anyhow::Result<Value> {
    if let Some(addr) = address {
        hopper_core::hosting::router::RouteSettings { addresses: vec![addr], ..Default::default() }.save(folder)?;
        app.router.changed();
    }
    if !skipped.is_empty() {
        let lines: Vec<String> = skipped.iter().map(|(f, why)| format!("{f}: {why}")).collect();
        let _ = std::fs::write(
            folder.join("left-out-client-mods.txt"),
            format!("Hopper left these mods out of the server as client-only. If the server needs one, copy it from the pack into mods.\n\n{}\n", lines.join("\n")),
        );
    }
    app.rescan();
    app.refresh.notify_one();
    let id = app.servers.list().into_iter().find(|e| e.root == folder).map(|e| e.id.clone());
    log::info!("made {}", folder.display());
    Ok(json!({ "id": id, "path": folder, "left_out": skipped.len() }))
}

fn start_zip_job(app: &Arc<App>, b: ZipBody) -> ApiResult<u64> {
    let zip = match (b.upload.as_deref(), b.path.as_deref()) {
        (Some(u), _) if !u.is_empty() => incoming(app).join(u),
        (_, Some(p)) => PathBuf::from(p),
        _ => return Err(bad("Which zip?")),
    };
    let c = common(app, &b.address, b.port, b.accept_eula)?;
    let a = app.clone();
    let title = format!("Making a server from {}", zip.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    Ok(crate::pack_api::spawn(app, title, move |job| {
        job.detail("Reading the zip");
        let info = zip_import::inspect(&zip)?;
        let (mc, loader) = match (&info.target, b.mc_version.trim(), &b.loader) {
            (_, mc, Some(l)) if !mc.is_empty() => (mc.to_string(), l.clone()),
            (Some(t), _, _) => (t.mc_version.clone(), t.loader.clone()),
            _ => anyhow::bail!("Hopper can't tell which Minecraft and loader this zip is for: pick them."),
        };
        let name = if b.name.trim().is_empty() { info.name.clone() } else { b.name.clone() };
        let java = crate::manage_api::java_for(&mc, job)?;
        let memory = b.memory_mb.or(info.target.as_ref().map(|t| t.recommended_mb).filter(|m| *m >= 2048));
        let o = options(&c, &name, &mc, ServerLoader::from_instance(&loader)?, &java, memory, b.port);
        let (r, _used, note) = zip_import::create_from_local_zip(&zip, &o, job)?;
        if let Some(n) = note {
            job.note(n);
        }
        for n in r.missing_notes() {
            job.note(n);
        }
        // An upload is done with once it's a server.
        if zip.starts_with(a.data_dir.join(INCOMING)) {
            let _ = std::fs::remove_file(&zip);
        }
        finish(&a, &r.root, c.address, &r.skipped)
    }))
}

async fn import_zip(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<ZipBody>) -> ApiResult<Json<Value>> {
    // Checks who may use it (a path on this machine is the owner's).
    zip_path(&app, &headers, &method, b.upload.as_deref(), b.path.as_deref())?;
    let job = start_zip_job(&app, b)?;
    Ok(Json(json!({ "job": job })))
}

// ---- CurseForge and Modrinth packs --------------------------------------------------------

async fn pack_search(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let text = q.get("q").cloned().unwrap_or_default();
    let use_mr = q.get("source").is_none_or(|s| s != "curseforge");
    let page: u32 = q.get("page").and_then(|p| p.parse().ok()).unwrap_or(0);
    let v = blocking(move || {
        if !use_mr && cf::api_key().is_none() {
            anyhow::bail!("CurseForge needs an API key: add one in Access & devices, or search Modrinth.");
        }
        let s = cf::Search { class_id: cf::CLASS_MODPACKS, text, index: page * 20, ..Default::default() };
        let (packs, _) = source::search(&s, use_mr)?;
        Ok(json!(packs
            .iter()
            .map(|m| json!({
                "id": m.id.to_string(), "name": m.name, "summary": m.summary, "downloads": m.download_count,
                "icon": m.icon().unwrap_or_default(),
                "author": m.authors.first().map(|a| a.name.clone()).unwrap_or_default(),
                "source": if use_mr { "modrinth" } else { "curseforge" },
                "server_files": if use_mr { None } else { Some(browse::curse_server_files(m).is_some()) },
            }))
            .collect::<Vec<_>>()))
    })
    .await?;
    Ok(Json(json!({ "results": v })))
}

/// Minecraft from a file's game versions ("1.21.1" among "NeoForge", ...).
fn mc_of(game_versions: &[String]) -> String {
    game_versions
        .iter()
        .find(|g| g.starts_with(|c: char| c.is_ascii_digit()) && g.contains('.'))
        .cloned()
        .unwrap_or_default()
}

async fn pack_versions(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let project: u64 = q.get("project").and_then(|p| p.parse().ok()).ok_or_else(|| bad("Which pack?"))?;
    let use_mr = q.get("source").is_some_and(|s| s == "modrinth");
    let v = blocking(move || {
        let rows: Vec<Value> = if use_mr {
            modrinth::versions(&modrinth::real_id(project)?, "", cf::Loader::Any)?
                .iter()
                .take(50)
                .map(|v| json!({
                    "id": modrinth::num_id(&v.id).to_string(), "name": v.version_number, "date": v.date_published,
                    "mc_version": mc_of(&v.game_versions), "loaders": v.loaders, "type": v.version_type,
                    "server_files": browse::modrinth_server_files(v).is_some(),
                }))
                .collect()
        } else {
            source::files(project, "", cf::Loader::Any)?
                .iter()
                .filter(|f| f.is_server_pack != Some(true))
                .take(50)
                .map(|f| json!({
                    "id": f.id.to_string(), "name": f.display_name, "date": f.file_date,
                    "mc_version": mc_of(&f.game_versions), "type": f.release_name(),
                    "server_files": f.server_pack_file_id.is_some_and(|i| i != 0),
                }))
                .collect()
        };
        Ok(rows)
    })
    .await?;
    Ok(Json(json!({ "versions": v })))
}

/// Which of a CurseForge pack version and its server files Hopper may
/// download, before the player starts a build: `choice` is "server_files",
/// "client_pack" or "needs_zip"; `pack` / `server_files` give the page of a
/// website-only file.
async fn pack_check(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let project: u64 = q.get("project").and_then(|p| p.parse().ok()).ok_or_else(|| bad("Which pack?"))?;
    let file: u64 = q.get("file").and_then(|p| p.parse().ok()).ok_or_else(|| bad("Which version?"))?;
    if q.get("source").is_some_and(|s| s == "modrinth") {
        return Ok(Json(json!({ "choice": "any", "pack": null, "server_files": null })));
    }
    let c = blocking(move || {
        let m = cf::get_mod(project)?;
        let f = cf::file(project, file)?;
        hopper_core::hosting::check_pack(&m, &f)
    })
    .await?;
    let page = |w: &Option<hopper_core::hosting::WebsiteOnly>| w.as_ref().map(|w| json!({ "file_name": w.file_name, "url": w.url }));
    let choice = match c.choice() {
        hopper_core::hosting::BuildChoice::ServerFiles => "server_files",
        hopper_core::hosting::BuildChoice::ClientPack => "client_pack",
        hopper_core::hosting::BuildChoice::NeedsZip => "needs_zip",
    };
    Ok(Json(json!({ "choice": choice, "pack": page(&c.pack), "server_files": page(&c.server_files) })))
}

#[derive(Deserialize)]
struct PackBody {
    source: String,
    project: String,
    file: String,
    name: String,
    memory_mb: Option<u32>,
    port: Option<u16>,
    #[serde(default)]
    address: String,
    #[serde(default)]
    accept_eula: bool,
}

/// A server from a pack version: its server files when it has them (the
/// mods the author picked for servers), else built from the client pack
/// with client-only mods left out, as desktop does.
async fn create_from_pack(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<PackBody>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let project: u64 = b.project.trim().parse().map_err(|_| bad("Which pack?"))?;
    let file_id: u64 = b.file.trim().parse().map_err(|_| bad("Which version?"))?;
    if b.name.trim().is_empty() {
        return Err(bad("Give the server a name."));
    }
    let c = common(&app, &b.address, b.port, b.accept_eula)?;
    let mr = b.source == "modrinth";
    let a = app.clone();
    let job = crate::pack_api::spawn(&app, format!("Making {}", b.name.trim()), move |job| {
        job.detail("Looking the pack up");
        let (files, client): (Option<browse::ServerFiles>, Option<(cf::Mod, cf::File)>) = if mr {
            let v = modrinth::version(&modrinth::real_id(file_id)?)?;
            match browse::modrinth_server_files(&v) {
                Some(sf) => (Some(sf), None),
                None => (None, Some((source::get_mod(project)?, modrinth::version_to_file(&v)))),
            }
        } else {
            let f = cf::file(project, file_id)?;
            match f.server_pack_file_id.filter(|i| *i != 0) {
                Some(s) => (Some(browse::ServerFiles { project, pack_file: file_id, server_file: s, file: None }), None),
                None => (None, Some((source::get_mod(project)?, f))),
            }
        };
        if let Some(files) = files {
            let t = hopper_core::hosting::pack_target(&files, job)?;
            let java = crate::manage_api::java_for(&t.mc_version, job)?;
            let memory = b.memory_mb.or(Some(t.recommended_mb).filter(|m| *m >= 2048));
            let o = options(&c, &b.name, &t.mc_version, ServerLoader::from_instance(&t.loader)?, &java, memory, b.port);
            let r = hopper_core::hosting::create_from_server_files(&files, &o, job)?;
            if r.website_only.is_some() {
                job.note(hopper_core::hosting::BUILT_FROM_PACK);
                job.note(format!("{} mods copied, {} left out as client-only.", r.create.mods_copied, r.create.skipped.len()));
            } else {
                job.note(format!("From {}: {} mods", r.server_pack, r.create.mods_copied));
            }
            for n in r.create.missing_notes() {
                job.note(n);
            }
            crate::pack_api::link_new_server(&r.create.root, mr, project, file_id);
            let mut v = finish(&a, &r.create.root, c.address, &r.create.skipped)?;
            v["built_from_pack"] = json!(r.website_only.is_some());
            v["missing"] = json!(r.create.missing.len());
            return Ok(v);
        }
        let (pack, file) = client.expect("one of the two");
        // Before finding Java: a website-only pack zip can't be built from.
        if cf::website_only(&file)? {
            return Err(cf::WebsiteOnly::of(&pack, &file).into());
        }
        let mc = mc_of(&file.game_versions);
        let java = crate::manage_api::java_for(&mc, job)?;
        // The pack's own Minecraft and loader replace these.
        let o = options(&c, &b.name, &mc, ServerLoader::Vanilla, &java, b.memory_mb, b.port);
        let (r, _used) = hopper_core::hosting::create_from_client_pack(&pack, &file, &o, job)?;
        job.note(format!("Built from the client pack: {} mods copied, {} left out as client-only.", r.mods_copied, r.skipped.len()));
        for n in r.missing_notes() {
            job.note(n);
        }
        crate::pack_api::link_new_server(&r.root, mr, project, file_id);
        let mut v = finish(&a, &r.root, c.address, &r.skipped)?;
        v["missing"] = json!(r.missing.len());
        Ok(v)
    });
    Ok(Json(json!({ "job": job })))
}

// ---- removing and deleting -------------------------------------------------------------------

#[derive(Deserialize)]
struct RemoveBody {
    /// "remove" (the panel forgets it, the folder stays) or "delete".
    mode: String,
    /// Delete only: the server's name, typed to confirm.
    #[serde(default)]
    confirm: String,
}

async fn remove(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<RemoveBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    if e.server.state().is_alive() {
        return Err(bad(format!("Stop {} first.", e.name)));
    }
    let root_dir = app.cfg.read().unwrap().servers_root.clone();
    let in_root = root_dir.as_deref().is_some_and(|r| e.root.parent().is_some_and(|p| same_path(p, r)));
    match b.mode.as_str() {
        "remove" => {
            {
                let mut c = app.cfg.write().unwrap();
                c.servers.retain(|s| !same_path(s, &e.root));
                if in_root && !c.hidden.iter().any(|h| same_path(h, &e.root)) {
                    c.hidden.push(e.root.clone());
                }
            }
            save_cfg(&app)?;
            log::info!("{} removed from the panel ({} stays)", e.name, e.root.display());
        }
        "delete" => {
            if !in_root {
                return Err(bad(format!(
                    "{} runs where it is ({}), maybe for another panel too: remove it from the panel instead. Hopper only deletes servers in its servers folder.",
                    e.name,
                    e.root.display()
                )));
            }
            if b.confirm.trim() != e.name {
                return Err(bad(format!("Type {} to delete it.", e.name)));
            }
            let root = root_dir.expect("in_root");
            let folder = e.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let dest = root.join(DELETED_DIR).join(format!("{folder}-{}", now()));
            let (src, name) = (e.root.clone(), e.name.clone());
            blocking(move || {
                std::fs::create_dir_all(dest.parent().unwrap())?;
                std::fs::rename(&src, &dest).map_err(|err| anyhow::anyhow!("moving {} to {}: {err}", src.display(), dest.display()))?;
                std::fs::write(dest.join(".hopper-deleted.json"), serde_json::to_vec(&json!({ "name": name, "from": src, "at": now() }))?)?;
                Ok(())
            })
            .await?;
            log::info!("{} deleted (kept {KEEP_DELETED_DAYS} days in {DELETED_DIR})", e.name);
        }
        _ => return Err(bad("remove or delete?")),
    }
    app.rescan();
    app.refresh.notify_one();
    Ok(Json(json!({ "ok": true })))
}

fn deleted_dir(app: &App, name: &str) -> ApiResult<PathBuf> {
    if name.contains(['/', '\\']) || name.contains("..") || name.is_empty() {
        return Err(bad("Which one?"));
    }
    let p = servers_root(app)?.join(DELETED_DIR).join(name);
    p.is_dir().then_some(p).ok_or_else(|| not_found("deleted server"))
}

async fn restore(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(name): Path<String>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let p = deleted_dir(&app, &name)?;
    let meta: Value = std::fs::read(p.join(".hopper-deleted.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let back = meta["from"].as_str().map(PathBuf::from).ok_or_else(|| bad("Hopper doesn't know where it came from."))?;
    if back.exists() {
        return Err(bad(format!("{} exists again: move it away first.", back.display())));
    }
    std::fs::rename(&p, &back).map_err(|e| bad(format!("moving it back: {e}")))?;
    let _ = std::fs::remove_file(back.join(".hopper-deleted.json"));
    {
        let mut c = app.cfg.write().unwrap();
        c.hidden.retain(|h| !same_path(h, &back));
    }
    save_cfg(&app)?;
    app.rescan();
    app.refresh.notify_one();
    let id = app.servers.list().into_iter().find(|e| same_path(&e.root, &back)).map(|e| e.id.clone());
    Ok(Json(json!({ "id": id })))
}

async fn purge(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(name): Path<String>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let p = deleted_dir(&app, &name)?;
    log::info!("deleting {} for good", p.display());
    blocking(move || Ok(std::fs::remove_dir_all(&p)?)).await?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    #[test]
    fn pack_minecraft_from_game_versions() {
        let g = |v: &[&str]| super::mc_of(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(g(&["NeoForge", "1.21.1", "Server"]), "1.21.1");
        assert_eq!(g(&["Fabric"]), "");
        assert!(super::same_path(std::path::Path::new("E:\\a\\B\\"), std::path::Path::new("e:/a/b")));
    }
}
