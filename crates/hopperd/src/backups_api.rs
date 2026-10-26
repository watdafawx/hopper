//! Backups over the API: a server's backups (make, browse, restore,
//! download, delete, keep rules, off-site copies, bringing one back from a
//! copy), the panel's off-site targets, and the receiving side that lets
//! another Hopper Server copy its backups here.

use crate::App;
use crate::api::{ApiError, ApiResult, bad, not_found, require, require_owner};
use crate::offsite::{self, Target};
use crate::servers::Entry;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use hopper_core::hosting::backup::{self, Keep, Kind, Scope};
use hopper_core::hosting::chunkstore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/servers/{id}/backups", get(list).post(make))
        .route("/api/v1/servers/{id}/backups/keep", put(set_keep))
        .route("/api/v1/servers/{id}/backups/copies", put(set_copies))
        .route("/api/v1/servers/{id}/backups/remote", get(remote))
        .route("/api/v1/servers/{id}/backups/remote/bring", post(bring))
        .route("/api/v1/servers/{id}/backups/{name}", axum::routing::delete(delete))
        .route("/api/v1/servers/{id}/backups/{name}/entries", get(entries))
        .route("/api/v1/servers/{id}/backups/{name}/restore", post(restore))
        .route("/api/v1/servers/{id}/backups/{name}/download", get(download))
        .route("/api/v1/servers/{id}/backups/{name}/convert", post(convert))
        .route("/api/v1/backup-targets", get(targets).post(add_target))
        .route("/api/v1/backup-targets/test", post(test_target))
        .route("/api/v1/backup-targets/key", post(import_key))
        .route("/api/v1/backup-targets/{tid}", put(edit_target).delete(remove_target))
        .route("/api/v1/backup-receive", get(receive_keys).post(add_receive_key))
        .route("/api/v1/backup-receive/{rid}", axum::routing::delete(remove_receive_key))
        .route("/api/v1/backup-store/{op}", post(store_op).layer(DefaultBodyLimit::max(64 << 20)))
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

// ---- a server's backups ------------------------------------------------------------

async fn list(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let dir = app.data_dir.clone();
    let owner = require_owner(&app, &headers, &method).is_ok();
    let (root, sid) = (e.root.clone(), e.id.clone());
    let v = blocking(move || {
        let list = backup::list(&root);
        let used = backup::usage(&root);
        let as_zips: u64 = list.iter().map(|b| b.size).sum();
        let zips: u64 = list.iter().filter(|b| b.is_zip()).map(|b| b.size).sum();
        let keep = backup::keep_rules(&root);
        let sched = hopper_core::hosting::Schedule::load(&root);
        let on: Vec<String> = offsite::load_copies(&dir).get(&sid).cloned().unwrap_or_default();
        let targets: Vec<Value> = offsite::load_targets(&dir)
            .into_iter()
            .map(|t| {
                let st = offsite::status(&dir, &t.id, &sid);
                json!({ "id": t.id, "name": t.name, "kind": t.kind, "enabled": t.enabled, "on": on.contains(&t.id),
                        "done": st.done, "last_ok": st.last_ok, "last_error": st.last_error, "pending": st.pending, "working": st.working })
            })
            .collect();
        Ok(json!({
            "backups": list, "total_size": used, "as_zips": as_zips, "zips_size": zips,
            "keep": keep, "every_hours": sched.backup_every_hours,
            "targets": targets, "can_edit_targets": owner,
            "key_set": offsite::has_key(&dir),
        }))
    })
    .await?;
    let free = app.host.read().unwrap().disk_free;
    let mut v = v;
    v["disk_free"] = json!(free);
    v["folder"] = json!(backup::DIR);
    Ok(Json(v))
}

#[derive(Deserialize)]
struct MakeBody {
    #[serde(default)]
    worlds_only: bool,
    #[serde(default)]
    note: String,
}

async fn make(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<MakeBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let scope = if b.worlds_only { Scope::Worlds } else { Scope::Full };
    let a = app.clone();
    let job = crate::pack_api::spawn(&app, format!("Backing up {}", e.name), move |job| {
        let r = backup::make(&e.root, scope, Kind::Manual, b.note.trim(), Some(&e.server), job)?;
        let text = format!("{} backed up ({})", e.name, r.name);
        let ev = crate::webhooks::Event { event: "backup", server_id: e.id.clone(), server: e.name.clone(), text, at: crate::webhooks::now() };
        crate::webhooks::emit(&a, vec![ev]);
        offsite::kick();
        Ok(r)
    });
    Ok(Json(json!({ "job": job })))
}

async fn delete(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path((id, name)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    log::info!("{}: deleting backup {name}", e.name);
    blocking(move || backup::delete(&e.root, &name)).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn entries(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path((id, name)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let list = blocking(move || backup::entries(&e.root, &name)).await?;
    Ok(Json(json!({ "entries": list })))
}

#[derive(Deserialize)]
struct RestoreBody {
    #[serde(default)]
    paths: Vec<String>,
}

async fn restore(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path((id, name)): Path<(String, String)>, Json(b): Json<RestoreBody>) -> ApiResult<Json<Value>> {
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

async fn convert(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path((id, name)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    log::info!("{}: converting {name}", e.name);
    let job = crate::pack_api::spawn(&app, format!("Converting a backup of {}", e.name), move |job| {
        let b = backup::convert(&e.root, &name, job)?;
        offsite::kick();
        Ok(b)
    });
    Ok(Json(json!({ "job": job })))
}

/// A backup as a zip: an old zip as it is, a snapshot built on the fly.
async fn download(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path((id, name)): Path<(String, String)>) -> ApiResult<Response> {
    let e = entry(&app, &headers, &method, &id)?;
    backup::check(&name).map_err(bad)?;
    let root = e.root.clone();
    let zip_name = format!("{}-{}.zip", e.id, name.trim_end_matches(".zip").trim_end_matches(chunkstore::EXT));
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let m = if name.ends_with(chunkstore::EXT) {
        let r = root.clone();
        let n = name.clone();
        Some(blocking(move || backup::store(&r).manifest(&n)).await?)
    } else {
        if !root.join(backup::DIR).join(&name).is_file() {
            return Err(not_found("backup"));
        }
        None
    };
    tokio::task::spawn_blocking(move || {
        let mut w = crate::fs_api::ChanWriter { tx: tx.clone(), buf: Vec::with_capacity(256 * 1024) };
        let res = (|| -> std::io::Result<()> {
            match m {
                Some(m) => {
                    let st = backup::store(&root);
                    let mut z = crate::zipstream::ZipStream::new(&mut w);
                    let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(m.at);
                    for f in &m.files {
                        z.add_reader(&f.path, f.size, Some(at), &mut st.reader(f))?;
                    }
                    z.finish()?;
                }
                None => {
                    let mut f = std::fs::File::open(root.join(backup::DIR).join(&name))?;
                    std::io::copy(&mut f, &mut w)?;
                }
            }
            std::io::Write::flush(&mut w)
        })();
        if let Err(err) = res
            && err.kind() != std::io::ErrorKind::BrokenPipe
        {
            log::warn!("backup download: {err}");
            let _ = tx.blocking_send(Err(err));
        }
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "application/zip")
        .header(header::CONTENT_DISPOSITION, crate::fs_api::disposition(&zip_name, false))
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx)))
        .map_err(bad)
}

async fn set_keep(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(k): Json<Keep>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    log::info!("{}: keep {} · {} days · {} weeks", e.name, k.last, k.daily_days, k.weekly_weeks);
    let k = blocking(move || backup::set_keep(&e.root, k)).await?;
    Ok(Json(json!({ "keep": k })))
}

#[derive(Deserialize)]
struct CopiesBody {
    targets: Vec<String>,
}

async fn set_copies(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<CopiesBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let dir = app.data_dir.clone();
    let _g = lock();
    let known: Vec<String> = offsite::load_targets(&dir).into_iter().map(|t| t.id).collect();
    let mut c = offsite::load_copies(&dir);
    let want: Vec<String> = b.targets.into_iter().filter(|t| known.contains(t)).collect();
    log::info!("{}: backup copies to {want:?}", e.name);
    if want.is_empty() {
        c.remove(&e.id);
    } else {
        c.insert(e.id.clone(), want);
    }
    offsite::save_copies(&dir, &c).map_err(bad)?;
    offsite::kick();
    Ok(Json(json!({ "ok": true })))
}

fn find_target(app: &App, tid: &str) -> ApiResult<Target> {
    offsite::load_targets(&app.data_dir).into_iter().find(|t| t.id == tid).ok_or_else(|| not_found("target"))
}

#[derive(Deserialize)]
struct RemoteQ {
    target: String,
    #[serde(default)]
    from: String,
}

/// The servers a copy holds and the snapshots of one of them.
async fn remote(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Query(q): Query<RemoteQ>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    require_owner(&app, &headers, &method)?;
    let t = find_target(&app, &q.target)?;
    let dir = app.data_dir.clone();
    let from = if q.from.is_empty() { e.id.clone() } else { q.from };
    let root = e.root.clone();
    let v = blocking(move || {
        let servers = offsite::remote_servers(&dir, &t)?;
        let snaps = if servers.contains(&from) { offsite::remote_snaps(&dir, &t, &from, &root)? } else { Vec::new() };
        Ok(json!({ "servers": servers, "from": from, "snapshots": snaps }))
    })
    .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct BringBody {
    target: String,
    from: String,
    name: String,
}

async fn bring(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<BringBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    require_owner(&app, &headers, &method)?;
    let t = find_target(&app, &b.target)?;
    let dir = app.data_dir.clone();
    log::info!("{}: bringing back {} from {} ({})", e.name, b.name, t.name, b.from);
    let job = crate::pack_api::spawn(&app, format!("Bringing back a backup of {}", e.name), move |job| offsite::bring_back(&dir, &t, &b.from, &b.name, &e.root, job));
    Ok(Json(json!({ "job": job })))
}

// ---- the panel's targets -------------------------------------------------------------

fn lock() -> std::sync::MutexGuard<'static, ()> {
    static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
    L.lock().unwrap_or_else(|e| e.into_inner())
}

async fn targets(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let dir = app.data_dir.clone();
    let names: HashMap<String, String> = app.servers.list().iter().map(|e| (e.id.clone(), e.name.clone())).collect();
    let v = blocking(move || {
        let copies = offsite::load_copies(&dir);
        let list: Vec<Value> = offsite::load_targets(&dir)
            .into_iter()
            .map(|t| {
                let servers: Vec<Value> = copies
                    .iter()
                    .filter(|(_, ts)| ts.contains(&t.id))
                    .map(|(sid, _)| {
                        let st = offsite::status(&dir, &t.id, sid);
                        json!({ "id": sid, "name": names.get(sid).cloned().unwrap_or_else(|| sid.clone()),
                                "copies": st.done.len(), "last_ok": st.last_ok, "last_error": st.last_error, "pending": st.pending, "working": st.working })
                    })
                    .collect();
                let mut v = t.view();
                v["place"] = json!(t.place());
                v["servers"] = json!(servers);
                v
            })
            .collect();
        Ok(json!({ "targets": list, "key_set": offsite::has_key(&dir), "receive": receivers(&dir) }))
    })
    .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct TargetBody {
    #[serde(flatten)]
    t: Target,
}

fn new_id() -> String {
    let mut b = [0u8; 6];
    let _ = getrandom::fill(&mut b);
    format!("t{}", chunkstore::hex(&b))
}

async fn add_target(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<TargetBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let dir = app.data_dir.clone();
    let mut t = b.t;
    t.id = new_id();
    if t.name.trim().is_empty() {
        t.name = t.place();
    }
    log::info!("backup target added: {} ({}, {})", t.name, t.kind, t.place());
    let v = blocking(move || {
        let _g = lock();
        let (key, fresh) = offsite::key(&dir)?;
        let mut all = offsite::load_targets(&dir);
        all.push(t.clone());
        offsite::save_targets(&dir, &all)?;
        Ok(json!({ "target": t.view(), "recovery_key": fresh.then(|| chunkstore::hex(&key)) }))
    })
    .await?;
    Ok(Json(v))
}

async fn edit_target(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(tid): Path<String>, Json(b): Json<TargetBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let dir = app.data_dir.clone();
    let v = blocking(move || {
        let _g = lock();
        let mut all = offsite::load_targets(&dir);
        let Some(old) = all.iter_mut().find(|t| t.id == tid) else { anyhow::bail!("No target with that id.") };
        let mut t = b.t;
        t.id = old.id.clone();
        // An empty secret keeps the saved one; the browser never has it.
        if t.secret.is_empty() {
            t.secret = std::mem::take(&mut old.secret);
        }
        // A new host or port means a new SSH key to learn.
        if t.kind == "sftp" && t.host_key.is_empty() && old.host == t.host && old.port == t.port {
            t.host_key = old.host_key.clone();
        }
        if t.name.trim().is_empty() {
            t.name = t.place();
        }
        *old = t.clone();
        offsite::save_targets(&dir, &all)?;
        log::info!("backup target changed: {} ({})", t.name, t.kind);
        Ok(json!({ "target": t.view() }))
    })
    .await?;
    offsite::kick();
    Ok(Json(v))
}

async fn remove_target(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(tid): Path<String>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let dir = app.data_dir.clone();
    blocking(move || {
        let _g = lock();
        let mut all = offsite::load_targets(&dir);
        all.retain(|t| t.id != tid);
        offsite::save_targets(&dir, &all)?;
        let mut c = offsite::load_copies(&dir);
        for v in c.values_mut() {
            v.retain(|t| t != &tid);
        }
        c.retain(|_, v| !v.is_empty());
        offsite::save_copies(&dir, &c)?;
        offsite::forget(&dir, &tid);
        log::info!("backup target {tid} removed (its copies stay where they are)");
        Ok(())
    })
    .await?;
    Ok(Json(json!({ "ok": true })))
}

/// Tries a target (saved or not). For a saved one, an empty secret uses
/// the saved secret. An SFTP server's key is remembered on success.
async fn test_target(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<TargetBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let dir = app.data_dir.clone();
    let v = blocking(move || {
        let mut t = b.t;
        let saved = offsite::load_targets(&dir).into_iter().find(|x| !t.id.is_empty() && x.id == t.id);
        if let Some(s) = &saved {
            if t.secret.is_empty() {
                t.secret = s.secret.clone();
            }
            if t.kind == "sftp" && s.host == t.host && s.port == t.port {
                t.host_key = s.host_key.clone();
            }
        }
        let host_key = offsite::test(&t)?;
        if let (Some(k), Some(_)) = (&host_key, &saved) {
            let _g = lock();
            let mut all = offsite::load_targets(&dir);
            if let Some(x) = all.iter_mut().find(|x| x.id == t.id) {
                x.host_key = k.clone();
            }
            offsite::save_targets(&dir, &all)?;
        }
        Ok(json!({ "ok": true, "host_key": host_key }))
    })
    .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct KeyBody {
    key: String,
}

async fn import_key(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<KeyBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let dir = app.data_dir.clone();
    blocking(move || offsite::import_key(&dir, &b.key)).await?;
    log::info!("backup key put in from another panel");
    Ok(Json(json!({ "ok": true })))
}

// ---- receiving another panel's copies ---------------------------------------------

const RECEIVE: &str = "backup-receive.json";
const RECEIVED: &str = "received-backups";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Receiver {
    id: String,
    name: String,
    /// SHA-256 of the key; the key itself is shown once.
    hash: String,
    at: u64,
}

fn load_receivers(dir: &std::path::Path) -> Vec<Receiver> {
    std::fs::read(dir.join(RECEIVE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn receivers(dir: &std::path::Path) -> Vec<Value> {
    use offsite::Backend as _;
    load_receivers(dir)
        .into_iter()
        .map(|r| {
            let used = crate::offsite::Folder(dir.join(RECEIVED).join(&r.id)).list("").map(|l| l.len()).unwrap_or(0);
            json!({ "id": r.id, "name": r.name, "at": r.at, "files": used })
        })
        .collect()
}

fn sha(s: &str) -> String {
    use sha2::Digest as _;
    chunkstore::hex(&sha2::Sha256::digest(s.as_bytes()))
}

async fn receive_keys(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let dir = app.data_dir.clone();
    Ok(Json(json!({ "receive": blocking(move || Ok(receivers(&dir))).await? })))
}

#[derive(Deserialize)]
struct NameBody {
    #[serde(default)]
    name: String,
}

async fn add_receive_key(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<NameBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let dir = app.data_dir.clone();
    let mut raw = [0u8; 24];
    getrandom::fill(&mut raw).map_err(bad)?;
    let key = format!("hbr_{}", chunkstore::hex(&raw));
    let r = Receiver { id: new_id(), name: if b.name.trim().is_empty() { "Another panel".into() } else { b.name.trim().to_string() }, hash: sha(&key), at: crate::webhooks::now() };
    log::info!("receive key made: {}", r.name);
    let id = r.id.clone();
    blocking(move || {
        let _g = lock();
        let mut all = load_receivers(&dir);
        all.push(r);
        std::fs::write(dir.join(RECEIVE), serde_json::to_vec_pretty(&all)?)?;
        Ok(())
    })
    .await?;
    Ok(Json(json!({ "id": id, "key": key })))
}

async fn remove_receive_key(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(rid): Path<String>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let dir = app.data_dir.clone();
    blocking(move || {
        let _g = lock();
        let mut all = load_receivers(&dir);
        all.retain(|r| r.id != rid);
        std::fs::write(dir.join(RECEIVE), serde_json::to_vec_pretty(&all)?)?;
        log::info!("receive key {rid} removed (what it sent stays in {RECEIVED})");
        Ok(())
    })
    .await?;
    Ok(Json(json!({ "ok": true })))
}

/// The folder a receive key's copies go to, if the request's key is good.
fn receiver_dir(app: &App, headers: &HeaderMap) -> ApiResult<PathBuf> {
    let key = headers.get("x-backup-key").and_then(|v| v.to_str().ok()).unwrap_or("");
    let h = sha(key);
    let r = (!key.is_empty())
        .then(|| load_receivers(&app.data_dir).into_iter().find(|r| r.hash == h))
        .flatten()
        .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "That receive key isn't known here.".into()))?;
    Ok(app.data_dir.join(RECEIVED).join(r.id))
}

#[derive(Deserialize, Default)]
struct StoreBody {
    #[serde(default)]
    keys: Vec<String>,
    #[serde(default)]
    prefix: String,
}

/// put (packed body), get / list / delete (JSON body).
async fn store_op(State(app): State<Arc<App>>, headers: HeaderMap, Path(op): Path<String>, body: Bytes) -> ApiResult<Response> {
    let dir = receiver_dir(&app, &headers)?;
    let r = tokio::task::spawn_blocking(move || -> anyhow::Result<Response> {
        let mut f = offsite::Folder(dir);
        use offsite::Backend as _;
        let req = || -> anyhow::Result<StoreBody> { Ok(if op == "put" { StoreBody::default() } else { serde_json::from_slice(&body)? }) };
        Ok(match op.as_str() {
            "put" => {
                let items = offsite::unpack(&body)?;
                f.put_many(&items)?;
                Json(json!({ "ok": true, "stored": items.len() })).into_response()
            }
            "get" => {
                let keys = req()?.keys;
                let vals = f.get_many(&keys)?;
                let items: Vec<(String, Vec<u8>)> = keys.into_iter().zip(vals).collect();
                ([(header::CONTENT_TYPE, "application/octet-stream")], offsite::pack(&items)).into_response()
            }
            "list" => Json(json!({ "keys": f.list(&req()?.prefix)? })).into_response(),
            "delete" => {
                f.delete_many(&req()?.keys)?;
                Json(json!({ "ok": true })).into_response()
            }
            _ => anyhow::bail!("unknown store operation"),
        })
    })
    .await
    .map_err(bad)?;
    r.map_err(|e| bad(format!("{e:#}")))
}
