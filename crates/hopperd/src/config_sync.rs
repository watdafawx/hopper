//! Players' copies of a server's configs: a paired Hopper sends the
//! settings files both sides read (common and server ones) as its snapshot,
//! the Configs page compares them with the server's live copies, and the
//! owner can push the server's values to paired Hopper clients. A push
//! waits per device until that Hopper applies or dismisses it; nothing
//! changes on a player's PC without their click there.
//!
//! Kept per server in the panel's data folder (`config-sync/<id>.json`).

use crate::App;
use crate::api::{ApiError, ApiResult, bad, not_found, require};
use crate::auth::{Role, Who};
use crate::configs::{MASK, secret};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use hopper_core::configcompare;
use hopper_core::configedit::{self, Format};
use hopper_core::configindex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// A snapshot's limits: files, each file, and all of them.
const MAX_FILES: usize = 600;
const MAX_TOTAL: usize = 12 * 1024 * 1024;
/// Finished pushes kept for the list (pending ones always stay).
const KEEP_DONE: usize = 200;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/servers/{id}/configs/client-snapshot", put(put_snapshot).layer(DefaultBodyLimit::max(MAX_TOTAL + 1024 * 1024)))
        .route("/api/v1/servers/{id}/configs/players", get(players))
        .route("/api/v1/servers/{id}/configs/push", post(push))
        .route("/api/v1/servers/{id}/configs/pushes", get(pushes))
        .route("/api/v1/servers/{id}/configs/pushes/{pid}", post(push_done))
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Snapshot {
    device: String,
    name: String,
    at: u64,
    /// Path as on the player's PC (`config/x-common.toml`) -> text.
    files: BTreeMap<String, String>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PushEdit {
    pub path: Vec<String>,
    pub key: String,
    /// As typed: the setting's value text.
    pub value: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Push {
    id: String,
    device: String,
    device_name: String,
    /// The file as on the player's PC.
    rel: String,
    edits: Vec<PushEdit>,
    /// Who pushed it.
    from: String,
    at: u64,
    /// pending, applied or dismissed.
    state: String,
    note: String,
    done_at: u64,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Kept {
    /// Device id -> its latest snapshot.
    snapshots: BTreeMap<String, Snapshot>,
    pushes: Vec<Push>,
}

static LOCK: Mutex<()> = Mutex::new(());

fn file(app: &App, id: &str) -> std::path::PathBuf {
    let safe: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    app.data_dir.join("config-sync").join(format!("{safe}.json"))
}

fn load(path: &std::path::Path) -> Kept {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save(path: &std::path::Path, k: &Kept) -> anyhow::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(k)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Reads, changes and writes a server's record under the one lock.
fn edit<T>(app: &App, id: &str, f: impl FnOnce(&mut Kept) -> T) -> ApiResult<T> {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = file(app, id);
    let mut k = load(&path);
    let out = f(&mut k);
    save(&path, &k).map_err(|e| bad(format!("{e:#}")))?;
    Ok(out)
}

fn read(app: &App, id: &str) -> Kept {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    load(&file(app, id))
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Who may push and see every push: the owner, managers, and admins of
/// this server (the people who may change its files).
fn manages(who: &Who) -> bool {
    who.owner_level() || matches!(who, Who::User { role: Role::Admin, .. })
}

fn forbidden(msg: &str) -> ApiError {
    ApiError(StatusCode::FORBIDDEN, msg.into())
}

fn server_of(app: &App, who: &Who, id: &str) -> ApiResult<Arc<crate::servers::Entry>> {
    if !who.sees(id) {
        return Err(not_found("server"));
    }
    app.servers.get(id).ok_or_else(|| not_found("server"))
}

/// A path a snapshot or push may name: a settings file both sides read.
fn shared_rel(rel: &str) -> bool {
    hopper_core::configdiff::safe_rel(rel) && configcompare::compared(rel) && configedit::editable(rel)
}

// ---- the snapshot ----------------------------------------------------------------

#[derive(Deserialize)]
struct SnapshotBody {
    files: BTreeMap<String, String>,
}

/// A paired Hopper's copy of its common and server settings files for this
/// server (the latest replaces the one before). `{ ok, files }`.
async fn put_snapshot(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<SnapshotBody>,
) -> ApiResult<Json<Value>> {
    let who = require(&app, &headers, &method)?;
    let Who::Device { id: device, name, .. } = &who else { return Err(forbidden("Only a paired Hopper sends its copy of the configs.")) };
    server_of(&app, &who, &id)?;
    let mut files = BTreeMap::new();
    let mut total = 0;
    for (rel, text) in b.files {
        let rel = rel.replace('\\', "/");
        if !shared_rel(&rel) || text.len() as u64 > configindex::MAX_BYTES {
            continue;
        }
        total += text.len();
        if files.len() >= MAX_FILES || total > MAX_TOTAL {
            return Err(bad("That's more config than the panel keeps for players' copies."));
        }
        files.insert(rel, text);
    }
    let n = files.len();
    let snap = Snapshot { device: device.clone(), name: name.clone(), at: now(), files };
    let app2 = app.clone();
    let id2 = id.clone();
    tokio::task::spawn_blocking(move || edit(&app2, &id2, |k| {
        k.snapshots.insert(snap.device.clone(), snap);
    }))
    .await
    .map_err(bad)??;
    Ok(Json(json!({ "ok": true, "files": n })))
}

/// The settings that differ between `client` (the player's text) and the
/// server's, secrets left out.
fn diffs(f: Format, client: &str, server: &str) -> Vec<Value> {
    configcompare::pairs(f, client, server)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.differs() && !secret(&p.entry.key))
        .map(|p| json!({ "path": p.entry.path, "key": p.entry.key, "label": p.entry.label(), "client": p.client.text(), "server": p.server.text() }))
        .collect()
}

/// `{ snapshot: { device, name, at } | null, files: [{ rel, client_rel,
/// diffs: [{ path, key, label, client, server }] }], differs, devices: [{ id,
/// name, last_seen, snapshot_at }] }`: the newest players' copy beside the
/// server's live copies, and the paired devices a push can go to.
async fn players(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let who = require(&app, &headers, &method)?;
    let e = server_of(&app, &who, &id)?;
    let kept = read(&app, &id);
    let devices: Vec<Value> = app
        .auth
        .devices()
        .iter()
        .map(|d| json!({ "id": d.id, "name": d.name, "last_seen": d.last_seen, "snapshot_at": kept.snapshots.get(&d.id).map(|s| s.at) }))
        .collect();
    let newest = kept.snapshots.values().max_by_key(|s| s.at).cloned();
    let v = tokio::task::spawn_blocking(move || {
        let root = &e.root;
        let level = crate::configs::level_name(root);
        let has = |r: &str| crate::files::resolve(root, r).is_ok_and(|p| p.is_file());
        let mut files = Vec::new();
        let mut differs = 0;
        if let Some(s) = &newest {
            for (crel, ctext) in &s.files {
                let Some(f) = Format::of(crel) else { continue };
                let Some(srel) = configcompare::server_rel(crel, &level, has) else { continue };
                let Ok((stext, _)) = &*configindex::read(root, &srel) else { continue };
                let d = diffs(f, ctext, stext);
                differs += d.len();
                files.push(json!({ "rel": srel, "client_rel": crel, "diffs": d }));
            }
        }
        json!({
            "snapshot": newest.as_ref().map(|s| json!({ "device": s.device, "name": s.name, "at": s.at })),
            "files": files, "differs": differs,
        })
    })
    .await
    .map_err(bad)?;
    let mut v = v;
    v["devices"] = Value::Array(devices);
    Ok(Json(v))
}

// ---- pushes ------------------------------------------------------------------------

#[derive(Deserialize)]
struct PushBody {
    devices: Vec<String>,
    /// The file as on the player's PC.
    rel: String,
    edits: Vec<PushEdit>,
}

/// Queues the server's values for paired Hopper clients: one push per
/// device, which that Hopper shows and applies on one click. The owner,
/// a manager or an admin. `{ ok, ids }`.
async fn push(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<PushBody>) -> ApiResult<Json<Value>> {
    let who = require(&app, &headers, &method)?;
    if !manages(&who) {
        return Err(forbidden("Only the owner, a manager or an admin can push settings to players."));
    }
    server_of(&app, &who, &id)?;
    let rel = b.rel.replace('\\', "/");
    if !shared_rel(&rel) {
        return Err(bad("That isn't a config file players' Hopper can take."));
    }
    // A masked secret never travels.
    let edits: Vec<PushEdit> = b.edits.into_iter().filter(|e| e.value != MASK && !e.key.is_empty() && !secret(&e.key)).collect();
    if edits.is_empty() {
        return Err(bad("Nothing to send."));
    }
    let known = app.auth.devices();
    let mut to = Vec::new();
    for d in &b.devices {
        let dev = known.iter().find(|k| &k.id == d).ok_or_else(|| bad("That device isn't paired any more."))?;
        to.push((dev.id.clone(), dev.name.clone()));
    }
    if to.is_empty() {
        return Err(bad("Pick at least one Hopper."));
    }
    let from = who.name();
    let at = now();
    let ids = edit(&app, &id, |k| {
        let mut ids = Vec::new();
        for (n, (dev, name)) in to.into_iter().enumerate() {
            let pid = format!("{at:x}{n:02x}{:04x}", k.pushes.len() % 0xffff);
            // A newer push of the same file to the same device replaces a pending one.
            k.pushes.retain(|p| !(p.state == "pending" && p.device == dev && p.rel == rel));
            k.pushes.push(Push { id: pid.clone(), device: dev, device_name: name, rel: rel.clone(), edits: edits.clone(), from: from.clone(), at, state: "pending".into(), ..Default::default() });
            ids.push(pid);
        }
        let done = k.pushes.iter().filter(|p| p.state != "pending").count();
        if done > KEEP_DONE {
            let mut drop = done - KEEP_DONE;
            k.pushes.retain(|p| {
                if drop > 0 && p.state != "pending" {
                    drop -= 1;
                    false
                } else {
                    true
                }
            });
        }
        ids
    })?;
    Ok(Json(json!({ "ok": true, "ids": ids })))
}

/// `{ pushes: [...] }`: a paired Hopper gets its pending ones; the owner and
/// managers (and admins) get every push with its state. Others may not look.
async fn pushes(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let who = require(&app, &headers, &method)?;
    server_of(&app, &who, &id)?;
    let kept = read(&app, &id);
    let list: Vec<&Push> = match &who {
        Who::Device { id: dev, .. } => kept.pushes.iter().filter(|p| &p.device == dev && p.state == "pending").collect(),
        w if manages(w) => kept.pushes.iter().rev().collect(),
        _ => return Err(forbidden("Only the owner, a manager or an admin can see pushes.")),
    };
    Ok(Json(json!({ "pushes": list })))
}

#[derive(Deserialize)]
struct DoneBody {
    state: String,
    #[serde(default)]
    note: String,
}

/// Marks a push applied or dismissed: by the Hopper it was for, or (to
/// withdraw one) by the owner, a manager or an admin.
async fn push_done(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path((id, pid)): Path<(String, String)>,
    Json(b): Json<DoneBody>,
) -> ApiResult<Json<Value>> {
    let who = require(&app, &headers, &method)?;
    server_of(&app, &who, &id)?;
    if b.state != "applied" && b.state != "dismissed" {
        return Err(bad("The state is applied or dismissed."));
    }
    let mine = |p: &Push| match &who {
        Who::Device { id: dev, .. } => &p.device == dev,
        w => manages(w),
    };
    let note: String = b.note.chars().take(500).collect();
    let found = edit(&app, &id, |k| {
        let Some(p) = k.pushes.iter_mut().find(|p| p.id == pid) else { return Err(not_found("push")) };
        if !mine(p) {
            return Err(forbidden("That push is for another Hopper."));
        }
        p.state = b.state.clone();
        p.note = note;
        p.done_at = now();
        Ok(())
    })?;
    found?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diffs_leave_secrets_out() {
        let c = "a = 1\nbotToken = \"mine\"\nb = true\n";
        let s = "a = 2\nbotToken = \"theirs\"\nb = true\n";
        let d = diffs(Format::Toml, c, s);
        assert_eq!(d.len(), 1);
        assert_eq!((d[0]["key"].as_str(), d[0]["client"].as_str(), d[0]["server"].as_str()), (Some("a"), Some("1"), Some("2")));
        assert!(shared_rel("config/x-common.toml") && shared_rel("config/x-server.toml"));
        assert!(!shared_rel("config/x-client.toml") && !shared_rel("../x-common.toml") && !shared_rel("mods/a.jar"));
    }

    #[test]
    fn kept_round_trips() {
        let dir = std::env::temp_dir().join(format!("hopperd-cfgsync-{}", std::process::id()));
        let path = dir.join("s.json");
        let mut k = Kept::default();
        k.pushes.push(Push { id: "1".into(), state: "pending".into(), edits: vec![PushEdit { path: vec![], key: "a".into(), value: "2".into() }], ..Default::default() });
        save(&path, &k).unwrap();
        let back = load(&path);
        assert_eq!(back.pushes.len(), 1);
        assert_eq!(back.pushes[0].edits[0].value, "2");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
