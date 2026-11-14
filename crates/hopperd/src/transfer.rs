//! The two ends of moving or copying a server between panels
//! (design/multi-panel.md, A5). Every panel has both: the source end reads
//! a stopped server's files into chunks, the target end puts them back
//! together in a staging folder and makes a server of it at the end.
//!
//! Only a main's move engine (`crate::moves`) calls these routes, through
//! the node tunnel or, for its own servers, straight into its own router.
//! Both set the relayed caller (`crate::relay::relayed`), which nothing from
//! the network can; a person calling a route here directly is refused. The
//! main never sees a server's files as a whole: it passes chunk streams from
//! one end to the other, one tunnel credit window at a time.
//!
//! Paths are checked on both ends. The source never follows a link and
//! refuses links that point outside the server folder; the target checks
//! every path in the list it gets (no `..`, no drive or root, no links out)
//! and writes only inside its staging folder, which it renames into the
//! servers folder once every chunk is there and checked.

use crate::App;
use crate::api::{ApiError, ApiResult, bad, not_found};
use crate::auth::{Role, Who};
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use hopper_core::hosting::chunkstore::{self, MAX_CHUNK};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};

/// Incoming servers wait here, inside the servers folder (so the last step
/// is a rename on the same drive), until every chunk is in.
pub const STAGING: &str = ".hopper-incoming";
/// Written into a server's folder once it has been moved elsewhere: where
/// to, when, by whom. Never copied along.
pub const MOVED_FILE: &str = ".hopper-moved.json";
/// A staging folder nobody came back for in this long is removed.
const STALE_SECS: u64 = 7 * 24 * 3600;
/// The most hashes one chunk request may ask for.
const MAX_ASK: usize = 4096;
/// A file list bigger than this is refused (about four million files).
const MAX_PLAN: usize = 512 * 1024 * 1024;
/// The frame a source sends instead of a chunk it couldn't read.
const ERR_HASH: &[u8; 64] = b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/servers/{id}/transfer/check", post(check))
        .route("/api/v1/servers/{id}/transfer/{tid}/prepare", post(prepare))
        .route("/api/v1/servers/{id}/transfer/{tid}/manifest", get(manifest))
        .route("/api/v1/servers/{id}/transfer/{tid}/chunks", post(chunks))
        .route("/api/v1/servers/{id}/transfer/{tid}/finish", post(finish))
        .route("/api/v1/servers/{id}/moved", delete(clear_moved))
        .route("/api/v1/receive/precheck", post(precheck))
        .route("/api/v1/receive/{tid}/manifest", put(receive_manifest))
        .route("/api/v1/receive/{tid}", get(receive_state).delete(receive_abort))
        .route("/api/v1/receive/{tid}/chunks", put(receive_chunks))
        .route("/api/v1/receive/{tid}/commit", post(commit))
}

// ---- the list of what a server holds ---------------------------------------

/// One folder, file or link in a server, by its path inside the folder.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Item {
    /// Path inside the server folder, `/`-separated.
    pub p: String,
    /// `d` folder, `f` file, `l` link.
    pub k: String,
    /// Unix permission bits (0 when the source can't tell).
    #[serde(default)]
    pub m: u32,
    #[serde(default)]
    pub s: u64,
    /// Modified time, Unix seconds.
    #[serde(default)]
    pub t: i64,
    /// A file's chunks in order: hash and length.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub c: Vec<(String, u32)>,
    /// A link's target, as written (relative, inside the folder).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub l: String,
    /// A link to a folder (Windows makes the two kinds differently).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ld: bool,
}

/// Everything a move sends: the server's name and its items.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Plan {
    pub name: String,
    #[serde(default)]
    pub backups: bool,
    /// Bytes in every file.
    pub size: u64,
    /// Bytes in the distinct chunks (what goes over the wire).
    pub unique: u64,
    pub chunks: usize,
    pub items: Vec<Item>,
}

/// `rel` names something inside a folder: plain parts only, no `..`, no
/// root, drive or NUL, no empty part.
pub fn rel_ok(rel: &str) -> bool {
    if rel.is_empty() || rel.len() > 4096 || rel.contains(['\\', '\0', ':']) || rel.starts_with('/') {
        return false;
    }
    rel.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
}

/// A link at `rel` pointing at `target` stays inside the folder: relative,
/// and its `..` never climbs above the folder from where the link is.
pub fn link_ok(rel: &str, target: &str) -> bool {
    if target.is_empty() || target.len() > 4096 || target.contains('\0') || target.starts_with(['/', '\\']) || target.contains(':') {
        return false;
    }
    let mut depth: Vec<&str> = rel.split('/').collect();
    depth.pop();
    for part in target.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                if depth.pop().is_none() {
                    return false;
                }
            }
            p => depth.push(p),
        }
    }
    true
}

/// Checks a list a target got: every path plain and unique, every link
/// inside, sizes adding up, chunk hashes well formed.
pub fn plan_ok(p: &Plan) -> Result<(), String> {
    let mut seen = HashSet::new();
    let mut kinds: HashMap<&str, &str> = HashMap::new();
    for i in &p.items {
        if !rel_ok(&i.p) {
            return Err(format!("\"{}\" isn't a path inside the server folder.", crate::relay::clean_text(&i.p)));
        }
        if !seen.insert(i.p.to_lowercase()) {
            return Err(format!("\"{}\" is listed twice.", crate::relay::clean_text(&i.p)));
        }
        // A parent must be a folder listed before it, never a file or link:
        // writing through a link would land outside the staging folder.
        if let Some((parent, _)) = i.p.rsplit_once('/')
            && kinds.get(parent) != Some(&"d")
        {
            return Err(format!("\"{}\" isn't inside a listed folder.", crate::relay::clean_text(&i.p)));
        }
        match i.k.as_str() {
            "d" => {}
            "f" => {
                let sum: u64 = i.c.iter().map(|(_, n)| *n as u64).sum();
                if sum != i.s || i.c.iter().any(|(h, n)| !chunkstore::valid_hash(h) || *n == 0 || *n as usize > MAX_CHUNK) {
                    return Err(format!("\"{}\" has a broken chunk list.", crate::relay::clean_text(&i.p)));
                }
            }
            "l" => {
                if !link_ok(&i.p, &i.l) {
                    return Err(format!("The link \"{}\" points outside the server folder.", crate::relay::clean_text(&i.p)));
                }
            }
            _ => return Err("The file list has something that isn't a folder, file or link.".into()),
        }
        kinds.insert(&i.p, i.k.as_str());
    }
    Ok(())
}

fn mtime_of(m: &std::fs::Metadata) -> i64 {
    m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64)
}

#[cfg(unix)]
fn mode_of(m: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    m.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn mode_of(m: &std::fs::Metadata) -> u32 {
    if m.permissions().readonly() { 0o444 } else { 0 }
}

/// What a walk of a server folder found.
#[derive(Default)]
struct Walk {
    items: Vec<Item>,
    size: u64,
    files: usize,
    /// Bytes in its backups folder (sent only with "Bring backups").
    backups: u64,
    /// Links that point outside the folder, or that can't be read.
    bad_links: Vec<String>,
}

/// Lists a server folder without following links. Leaves out the moved
/// mark, and its backups unless `backups`.
fn walk(root: &FsPath, backups: bool) -> anyhow::Result<Walk> {
    let mut w = Walk::default();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&dir)?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            if rel.is_empty() && name == MOVED_FILE {
                continue;
            }
            let m = std::fs::symlink_metadata(e.path())?;
            if rel.is_empty() && name == hopper_core::hosting::backup::DIR {
                if !backups {
                    w.backups += dir_size(&e.path());
                    continue;
                }
            }
            if !rel_ok(&r) {
                w.bad_links.push(r);
                continue;
            }
            let ft = m.file_type();
            if ft.is_symlink() || is_junction(&e.path(), &m) {
                let target = std::fs::read_link(e.path()).map(|t| t.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                if !link_ok(&r, &target) {
                    w.bad_links.push(r);
                    continue;
                }
                let ld = std::fs::metadata(e.path()).is_ok_and(|t| t.is_dir());
                w.items.push(Item { p: r, k: "l".into(), m: 0, s: 0, t: mtime_of(&m), c: Vec::new(), l: target, ld });
            } else if ft.is_dir() {
                w.items.push(Item { p: r.clone(), k: "d".into(), m: mode_of(&m), t: mtime_of(&m), ..Default::default() });
                stack.push((e.path(), r));
            } else if ft.is_file() {
                w.size += m.len();
                w.files += 1;
                w.items.push(Item { p: r, k: "f".into(), m: mode_of(&m), s: m.len(), t: mtime_of(&m), ..Default::default() });
            }
            // Sockets, pipes and devices aren't part of a server: left out.
        }
    }
    // Folders before what's in them (the stack walks depth first).
    w.items.sort_by(|a, b| a.p.split('/').count().cmp(&b.p.split('/').count()).then_with(|| a.p.cmp(&b.p)));
    Ok(w)
}

#[cfg(windows)]
fn is_junction(_p: &FsPath, m: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    // A reparse point that isn't a plain symlink (a junction): treated as
    // a link, never walked into.
    m.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_junction(_p: &FsPath, _m: &std::fs::Metadata) -> bool {
    false
}

fn dir_size(p: &FsPath) -> u64 {
    let mut n = 0;
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let Ok(m) = std::fs::symlink_metadata(e.path()) else { continue };
            if m.is_dir() {
                stack.push(e.path());
            } else if m.is_file() {
                n += m.len();
            }
        }
    }
    n
}

/// Cuts every file of `w` into chunks (the backup store's cut, so the same
/// file always gives the same chunks), calling `tick` with bytes done.
fn chunk_all(root: &FsPath, w: &mut Walk, mut tick: impl FnMut(u64) -> bool) -> anyhow::Result<(u64, usize)> {
    let mut seen = HashSet::new();
    let mut unique = 0u64;
    let mut done = 0u64;
    let mut buf: Vec<u8> = Vec::with_capacity(2 * MAX_CHUNK);
    for item in w.items.iter_mut().filter(|i| i.k == "f") {
        let path = root.join(&item.p);
        let mut f = std::fs::File::open(&path)?;
        buf.clear();
        let mut eof = false;
        let mut total = 0u64;
        loop {
            while !eof && buf.len() < MAX_CHUNK {
                let start = buf.len();
                buf.resize(start + MAX_CHUNK, 0);
                let n = f.read(&mut buf[start..])?;
                buf.truncate(start + n);
                eof = n == 0;
            }
            if buf.is_empty() {
                break;
            }
            let n = chunkstore::next_cut(&buf, eof);
            let hash = chunkstore::hex(&Sha256::digest(&buf[..n]));
            if seen.insert(hash.clone()) {
                unique += n as u64;
            }
            item.c.push((hash, n as u32));
            total += n as u64;
            done += n as u64;
            buf.drain(..n);
            if !tick(done) {
                anyhow::bail!("Cancelled");
            }
        }
        // Sized as read.
        item.s = total;
    }
    Ok((unique, seen.len()))
}

// ---- who may call ----------------------------------------------------------

/// Only a main's move engine, as someone who may make and remove servers
/// here (the owner or a manager there, under a Manage ceiling here).
fn engine() -> ApiResult<Who> {
    match crate::relay::relayed() {
        Some(w @ Who::Main { lead: true, role: Role::Admin, .. }) => Ok(w),
        Some(_) => Err(ApiError(StatusCode::FORBIDDEN, "Moving servers needs the owner or a manager on the main, and Manage on this panel.".into())),
        None => Err(ApiError(StatusCode::FORBIDDEN, "Only a main panel moves servers here. Use Move or copy to… on the server's page.".into())),
    }
}

fn tid_ok(tid: &str) -> ApiResult<()> {
    if (1..=64).contains(&tid.len()) && tid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        Ok(())
    } else {
        Err(bad("Bad move id."))
    }
}

fn plans_dir(app: &App) -> PathBuf {
    app.data_dir.join("transfers")
}

fn out_plan(app: &App, tid: &str) -> PathBuf {
    plans_dir(app).join(format!("out-{tid}.json"))
}

// ---- the source end --------------------------------------------------------

/// Servers being sent right now (their folder), so nothing starts them.
static SENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn sending(root: &FsPath, on: bool) {
    let mut s = SENDING.lock().unwrap_or_else(|e| e.into_inner());
    s.retain(|r| r != root);
    if on {
        s.push(root.to_path_buf());
    }
}

/// Where a moved server went, from its folder's mark.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MovedMark {
    /// The panel it went to, as named there.
    pub to: String,
    /// That panel's node id on the main ("" for the main itself).
    #[serde(default)]
    pub to_node: String,
    /// The new server's id there.
    #[serde(default)]
    pub to_id: String,
    pub at: u64,
    #[serde(default)]
    pub by: String,
}

pub fn moved_mark(root: &FsPath) -> Option<MovedMark> {
    serde_json::from_slice(&std::fs::read(root.join(MOVED_FILE)).ok()?).ok()
}

pub fn is_sending(root: &FsPath) -> bool {
    SENDING.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|r| r == root)
}

/// Why `e` can't start now: it's being sent, or it was moved away.
pub fn start_blocked(e: &crate::servers::Entry) -> Option<String> {
    if is_sending(&e.root) {
        return Some(format!("{} is being moved or copied. Start it once that's done.", e.name));
    }
    moved_mark(&e.root).map(|m| format!("{} was moved to {}. Start it there, or choose Keep using this copy first.", e.name, m.to))
}

#[derive(Deserialize, Default)]
struct BackupsBody {
    #[serde(default)]
    backups: bool,
}

fn entry(app: &App, id: &str) -> ApiResult<Arc<crate::servers::Entry>> {
    app.servers.get(id).ok_or_else(|| not_found("server"))
}

/// What the source has: size, files, backups, state and what it runs.
async fn check(State(app): State<Arc<App>>, Path(id): Path<String>, Json(b): Json<BackupsBody>) -> ApiResult<Json<Value>> {
    engine()?;
    let e = entry(&app, &id)?;
    let s = app.servers.summary(&e);
    let root = e.root.clone();
    let w = tokio::task::spawn_blocking(move || walk(&root, b.backups)).await.map_err(bad)?.map_err(|e| bad(format!("Couldn't read the server's files: {e:#}")))?;
    let launch = hopper_core::hosting::script::read_launch(&e.root).ok().map(|l| l.0.java.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(Json(json!({
        "name": e.name,
        "state": s.state,
        "size": w.size,
        "files": w.files,
        "backups_size": w.backups,
        "bad_links": w.bad_links.iter().take(20).collect::<Vec<_>>(),
        "loader": s.loader,
        "minecraft": s.version,
        "port": s.port,
        "max_memory": s.max_memory,
        "java": launch,
        "os": std::env::consts::OS,
        "moved": moved_mark(&e.root),
        "sending": is_sending(&e.root),
    })))
}

fn ndjson(rx: tokio::sync::mpsc::Receiver<Value>) -> Response {
    let s = futures_util::stream::unfold(rx, |mut rx| async move {
        let v = rx.recv().await?;
        let mut line = v.to_string().into_bytes();
        line.push(b'\n');
        Some((Ok::<_, std::io::Error>(axum::body::Bytes::from(line)), rx))
    });
    ([(header::CONTENT_TYPE, "application/x-ndjson")], Body::from_stream(s)).into_response()
}

/// Lists and cuts the server's files into chunks, saving the list for the
/// chunk requests. Answers with progress lines, then `{"ok":..}` or
/// `{"error":..}`. The server must be stopped, and stays unstartable until
/// [`finish`].
async fn prepare(State(app): State<Arc<App>>, Path((id, tid)): Path<(String, String)>, Json(b): Json<BackupsBody>) -> ApiResult<Response> {
    engine()?;
    tid_ok(&tid)?;
    let e = entry(&app, &id)?;
    if e.server.state().is_alive() {
        return Err(bad(format!("Stop {} first.", e.name)));
    }
    sending(&e.root, true);
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let app2 = app.clone();
    tokio::task::spawn_blocking(move || {
        let r = (|| -> anyhow::Result<Plan> {
            let mut w = walk(&e.root, b.backups)?;
            if let Some(l) = w.bad_links.first() {
                anyhow::bail!("\"{l}\" is a link to somewhere outside the server folder. Remove it or replace it with the real files first.");
            }
            let total = w.size;
            let mut last = std::time::Instant::now();
            let (unique, chunks) = chunk_all(&e.root, &mut w, |done| {
                if last.elapsed().as_millis() >= 500 {
                    last = std::time::Instant::now();
                    // A main that stopped listening stops the reading too.
                    return tx.blocking_send(json!({ "done": done, "total": total })).is_ok();
                }
                true
            })?;
            let plan = Plan { name: e.name.clone(), backups: b.backups, size: w.items.iter().map(|i| i.s).sum(), unique, chunks, items: w.items };
            std::fs::create_dir_all(plans_dir(&app2))?;
            let path = out_plan(&app2, &tid);
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, serde_json::to_vec(&plan)?)?;
            std::fs::rename(&tmp, &path)?;
            Ok(plan)
        })();
        let _ = match r {
            Ok(p) => tx.blocking_send(json!({ "ok": true, "size": p.size, "unique": p.unique, "chunks": p.chunks, "items": p.items.len() })),
            Err(err) => {
                sending(&e.root, false);
                tx.blocking_send(json!({ "error": format!("{err:#}") }))
            }
        };
    });
    Ok(ndjson(rx))
}

fn load_out(app: &App, tid: &str) -> ApiResult<Plan> {
    let bytes = std::fs::read(out_plan(app, tid)).map_err(|_| ApiError(StatusCode::NOT_FOUND, "This move isn't ready on the source any more. Start it again.".into()))?;
    serde_json::from_slice(&bytes).map_err(bad)
}

/// The saved file list, as is.
async fn manifest(State(app): State<Arc<App>>, Path((id, tid)): Path<(String, String)>) -> ApiResult<Response> {
    engine()?;
    tid_ok(&tid)?;
    let e = entry(&app, &id)?;
    let bytes = std::fs::read(out_plan(&app, &tid)).map_err(|_| not_found("move"))?;
    sending(&e.root, true);
    Ok(([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response())
}

#[derive(Deserialize)]
struct AskBody {
    hashes: Vec<String>,
}

/// Reads one chunk back from the server folder: never through a link, and
/// only when it still hashes to what the list says.
fn read_chunk(root: &FsPath, croot: &FsPath, rel: &str, off: u64, len: u32, hash: &str) -> Result<Vec<u8>, String> {
    let path = root.join(rel);
    let m = std::fs::symlink_metadata(&path).map_err(|_| format!("{rel} is gone."))?;
    if !m.is_file() {
        return Err(format!("{rel} isn't a file any more."));
    }
    // Every folder on the way must still be inside the server folder.
    let real = path.canonicalize().map_err(|e| format!("{rel}: {e}"))?;
    if !real.starts_with(croot) {
        return Err(format!("{rel} is outside the server folder now."));
    }
    #[cfg(test)]
    if TAMPER.lock().unwrap().iter().any(|t| real.starts_with(t)) {
        return Err(format!("{rel} changed while it was being sent."));
    }
    let mut f = std::fs::File::open(&real).map_err(|e| format!("{rel}: {e}"))?;
    f.seek(SeekFrom::Start(off)).map_err(|e| format!("{rel}: {e}"))?;
    let mut buf = vec![0u8; len as usize];
    f.read_exact(&mut buf).map_err(|_| format!("{rel} changed while it was being sent."))?;
    if chunkstore::hex(&Sha256::digest(&buf)) != hash {
        return Err(format!("{rel} changed while it was being sent."));
    }
    Ok(buf)
}

/// Tests: server folders (canonical) whose chunks read as changed.
#[cfg(test)]
pub static TAMPER: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn frame(hash: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(68 + body.len());
    out.extend_from_slice(hash);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// The chunks asked for, as frames: 64 hex characters, a 4-byte length,
/// the bytes. A chunk that can't be read ends the answer with an error
/// frame (the hash all `x`, then the reason).
async fn chunks(State(app): State<Arc<App>>, Path((id, tid)): Path<(String, String)>, Json(b): Json<AskBody>) -> ApiResult<Response> {
    engine()?;
    tid_ok(&tid)?;
    if b.hashes.len() > MAX_ASK || !b.hashes.iter().all(|h| chunkstore::valid_hash(h)) {
        return Err(bad("Bad chunk list."));
    }
    let e = entry(&app, &id)?;
    if e.server.state().is_alive() {
        return Err(bad(format!("{} was started. Stop it to go on.", e.name)));
    }
    sending(&e.root, true);
    let plan = load_out(&app, &tid)?;
    let want: HashSet<&str> = b.hashes.iter().map(String::as_str).collect();
    let mut at: HashMap<String, (String, u64, u32)> = HashMap::new();
    for i in plan.items.iter().filter(|i| i.k == "f") {
        let mut off = 0u64;
        for (h, n) in &i.c {
            if want.contains(h.as_str()) && !at.contains_key(h) {
                at.insert(h.clone(), (i.p.clone(), off, *n));
            }
            off += *n as u64;
        }
    }
    if let Some(h) = b.hashes.iter().find(|h| !at.contains_key(*h)) {
        return Err(bad(format!("Chunk {h} isn't part of this move.")));
    }
    let croot = e.root.canonicalize().map_err(bad)?;
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(2);
    let root = e.root.clone();
    tokio::task::spawn_blocking(move || {
        for h in &b.hashes {
            let (rel, off, len) = &at[h];
            let f = match read_chunk(&root, &croot, rel, *off, *len, h) {
                Ok(buf) => frame(h.as_bytes(), &buf),
                Err(msg) => {
                    let _ = tx.blocking_send(frame(ERR_HASH, msg.as_bytes()));
                    return;
                }
            };
            if tx.blocking_send(f).is_err() {
                return;
            }
        }
    });
    let s = futures_util::stream::unfold(rx, |mut rx| async move { Some((Ok::<_, std::io::Error>(axum::body::Bytes::from(rx.recv().await?)), rx)) });
    Ok(([(header::CONTENT_TYPE, "application/octet-stream")], Body::from_stream(s)).into_response())
}

#[derive(Deserialize)]
struct FinishBody {
    /// moved, copied or cancelled.
    outcome: String,
    /// The panel it went to, as named there.
    #[serde(default)]
    dest_name: String,
    #[serde(default)]
    dest_node: String,
    #[serde(default)]
    dest_id: String,
}

/// Ends the source's part: lets the server start again (or marks it moved
/// and writes its History), and drops the saved list.
async fn finish(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path((id, tid)): Path<(String, String)>, Json(b): Json<FinishBody>) -> ApiResult<Json<Value>> {
    let who = engine()?;
    tid_ok(&tid)?;
    let e = entry(&app, &id)?;
    let _ = std::fs::remove_file(out_plan(&app, &tid));
    sending(&e.root, false);
    let dest = crate::relay::clean(&b.dest_name);
    if b.outcome == "moved" {
        let by = match &who {
            Who::Main { user, .. } => crate::relay::clean(user),
            _ => String::new(),
        };
        let mark = MovedMark { to: dest.clone(), to_node: crate::relay::clean(&b.dest_node), to_id: crate::relay::clean(&b.dest_id), at: crate::security::now(), by };
        std::fs::write(e.root.join(MOVED_FILE), serde_json::to_vec_pretty(&mark).map_err(bad)?).map_err(|err| bad(format!("Couldn't mark {} as moved: {err}", e.name)))?;
        let note = format!("To {dest}. This copy is stopped and kept.");
        if let Err(err) = hopper_core::actor::note_move(&e.root, hopper_core::actor::Did::Moved, &e.name, &note, &crate::mods_api::changer(&app, &headers, &method)) {
            log::warn!("{}: couldn't write the move to its History: {err}", e.name);
        }
        log::info!("{} moved to {dest}; this copy is kept, stopped", e.name);
        crate::changes::emit(&app, Some(&e.id), &["history"]);
    }
    crate::changes::emit(&app, None, &["list"]);
    app.refresh.notify_one();
    Ok(Json(json!({ "ok": true })))
}

/// "Keep using this copy": drops the moved mark so it starts again.
async fn clear_moved(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let who = crate::api::require_owner_who(&app, &headers, &method)?;
    if !who.sees(&id) {
        return Err(not_found("server"));
    }
    let e = entry(&app, &id)?;
    match std::fs::remove_file(e.root.join(MOVED_FILE)) {
        Ok(()) => log::info!("{}: moved mark cleared by {}", e.name, who.name()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(bad(format!("Couldn't change {}: {err}", e.name))),
    }
    crate::changes::emit(&app, None, &["list"]);
    app.refresh.notify_one();
    Ok(Json(json!({ "ok": true })))
}

// ---- the target end --------------------------------------------------------

fn servers_root(app: &App) -> ApiResult<PathBuf> {
    app.cfg.read().unwrap().servers_root.clone().ok_or_else(|| bad("This panel has no servers folder yet (Settings › Panel folder)."))
}

fn staging(app: &App, tid: &str) -> ApiResult<PathBuf> {
    tid_ok(tid)?;
    Ok(servers_root(app)?.join(STAGING).join(tid))
}

/// Removes staging folders nobody came back for in a week.
pub fn purge_stale(root: Option<&FsPath>) {
    let Some(dir) = root.map(|r| r.join(STAGING)) else { return };
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|a| a.as_secs() > STALE_SECS);
        if old {
            log::info!("removing an unfinished incoming server ({})", e.path().display());
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

#[derive(Deserialize)]
struct PrecheckBody {
    #[serde(default)]
    name: String,
    size: u64,
    #[serde(default)]
    port: u16,
    #[serde(default)]
    minecraft: String,
}

/// Whether this panel can take a server of that size, port and version.
async fn precheck(State(app): State<Arc<App>>, Json(b): Json<PrecheckBody>) -> ApiResult<Json<Value>> {
    engine()?;
    let root = servers_root(&app)?;
    let list = app.servers.list();
    let mut used: Vec<(String, u16)> = list.iter().map(|e| (e.name.clone(), crate::router::port_of(e))).collect();
    {
        let c = app.cfg.read().unwrap();
        if c.router.enabled {
            used.push(("The address router".into(), c.router.port));
        }
    }
    let port = b.port;
    let mc = b.minecraft.clone();
    let name = crate::relay::clean(&b.name);
    let r2 = root.clone();
    let (free, port_c, java, folder) = tokio::task::spawn_blocking(move || {
        let free = hopper_core::hosting::create::disk_free(&r2);
        let pc = (port > 0).then(|| hopper_core::hosting::ports::check_here(port, &used));
        let java = if mc.is_empty() {
            None
        } else {
            let installs = hopper_core::java::detect(std::path::Path::new(""));
            Some(hopper_core::java::best(&installs, &mc, hopper_core::java::Side::Server).map(|r| r.install.exe.to_string_lossy().into_owned()))
        };
        let folder = hopper_core::hosting::create::free_server_name(&r2, if name.is_empty() { "Server" } else { &name });
        (free, pc, java, folder)
    })
    .await
    .map_err(bad)?;
    // Room for the files plus a little to run.
    let need = b.size + 512 * 1024 * 1024;
    let port_v = match port_c {
        Some(c) if !c.taken_by.is_empty() || c.in_use => json!({ "ok": false, "taken_by": c.taken_by, "suggest": c.suggest }),
        Some(_) => json!({ "ok": true }),
        None => json!({ "ok": true }),
    };
    Ok(Json(json!({
        "host": crate::config::hostname(),
        "os": std::env::consts::OS,
        "free": free,
        "need": need,
        "disk_ok": free == 0 || free >= need,
        "port": port_v,
        "java": match java {
            None => json!({ "ok": true }),
            Some(Some(exe)) => json!({ "ok": true, "exe": exe }),
            Some(None) => json!({ "ok": false, "major": hopper_core::java::recommended_major(&b.minecraft) }),
        },
        "name": folder,
    })))
}

/// What a staging folder holds besides the tree.
#[derive(Serialize, Deserialize, Default)]
struct Meta {
    mode: String,
    from: String,
    at: u64,
}

#[derive(Deserialize)]
struct ManifestQuery {
    #[serde(default)]
    mode: String,
    #[serde(default)]
    from: String,
}

fn read_plan(dir: &FsPath) -> ApiResult<Plan> {
    let bytes = std::fs::read(dir.join("plan.json")).map_err(|_| not_found("incoming server"))?;
    serde_json::from_slice(&bytes).map_err(bad)
}

fn have_set(dir: &FsPath) -> HashSet<String> {
    std::fs::read_to_string(dir.join("have")).unwrap_or_default().lines().filter(|l| chunkstore::valid_hash(l)).map(str::to_string).collect()
}

fn missing(plan: &Plan, have: &HashSet<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for i in plan.items.iter().filter(|i| i.k == "f") {
        for (h, _) in &i.c {
            if !have.contains(h) && seen.insert(h.as_str()) {
                out.push(h.clone());
            }
        }
    }
    out
}

/// Takes the file list and makes the staging folder: every folder, and
/// every file at its full size, ready for chunks. The same list again
/// (a resumed move) keeps what's already there.
async fn receive_manifest(State(app): State<Arc<App>>, Path(tid): Path<String>, Query(q): Query<ManifestQuery>, body: Body) -> ApiResult<Json<Value>> {
    engine()?;
    let dir = staging(&app, &tid)?;
    let bytes = axum::body::to_bytes(body, MAX_PLAN).await.map_err(|_| bad("The file list didn't arrive whole."))?;
    let plan: Plan = serde_json::from_slice(&bytes).map_err(|_| bad("The file list can't be read."))?;
    plan_ok(&plan).map_err(bad)?;
    let mode = if q.mode == "copy" { "copy" } else { "move" };
    let meta = Meta { mode: mode.into(), from: crate::relay::clean(&q.from), at: crate::security::now() };
    let got = tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
        let same = std::fs::read(dir.join("plan.json")).is_ok_and(|old| old == bytes);
        if !same {
            if dir.exists() {
                std::fs::remove_dir_all(&dir)?;
            }
            std::fs::create_dir_all(dir.join("tree"))?;
            let tree = dir.join("tree");
            for i in &plan.items {
                let p = tree.join(&i.p);
                match i.k.as_str() {
                    "d" => std::fs::create_dir_all(&p)?,
                    "f" => std::fs::File::create(&p)?.set_len(i.s)?,
                    _ => {}
                }
            }
            std::fs::write(dir.join("meta.json"), serde_json::to_vec(&meta)?)?;
            std::fs::write(dir.join("have"), b"")?;
            std::fs::write(dir.join("plan.json"), &bytes)?;
        }
        Ok(missing(&plan, &have_set(&dir)).len())
    })
    .await
    .map_err(bad)?
    .map_err(|e| bad(format!("Couldn't make room for the server: {e:#}")))?;
    Ok(Json(json!({ "missing": got })))
}

/// The chunks still missing.
async fn receive_state(State(app): State<Arc<App>>, Path(tid): Path<String>) -> ApiResult<Json<Value>> {
    engine()?;
    let dir = staging(&app, &tid)?;
    let plan = read_plan(&dir)?;
    let m = missing(&plan, &have_set(&dir));
    Ok(Json(json!({ "missing": m, "chunks": plan.chunks })))
}

/// Where each chunk goes: every file and offset that holds it.
fn positions(plan: &Plan) -> HashMap<String, Vec<(usize, u64)>> {
    let mut at: HashMap<String, Vec<(usize, u64)>> = HashMap::new();
    for (n, i) in plan.items.iter().enumerate().filter(|(_, i)| i.k == "f") {
        let mut off = 0u64;
        for (h, len) in &i.c {
            at.entry(h.clone()).or_default().push((n, off));
            off += *len as u64;
        }
    }
    at
}

/// Writes one checked chunk everywhere it goes and notes it as had.
fn put_chunk(tree: &FsPath, plan: &Plan, at: &HashMap<String, Vec<(usize, u64)>>, have: &mut std::fs::File, hash: &str, data: &[u8]) -> Result<(), String> {
    let Some(places) = at.get(hash) else {
        return Err("A chunk came that isn't part of this move.".into());
    };
    if chunkstore::hex(&Sha256::digest(data)) != hash {
        return Err("A chunk arrived damaged.".into());
    }
    for (n, off) in places {
        let p = tree.join(&plan.items[*n].p);
        // Only plain files the list made; never through a link.
        if !std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_file()) {
            return Err(format!("{} isn't where it should be.", plan.items[*n].p));
        }
        let mut f = std::fs::OpenOptions::new().write(true).open(&p).map_err(|e| e.to_string())?;
        f.seek(SeekFrom::Start(*off)).map_err(|e| e.to_string())?;
        f.write_all(data).map_err(|e| format!("Couldn't write {}: {e}", plan.items[*n].p))?;
    }
    have.write_all(format!("{hash}\n").as_bytes()).map_err(|e| e.to_string())?;
    Ok(())
}

/// Takes chunk frames as they stream in. Each is checked against its hash
/// and written; one that breaks off midway is simply missing next time.
async fn receive_chunks(State(app): State<Arc<App>>, Path(tid): Path<String>, body: Body) -> ApiResult<Json<Value>> {
    use futures_util::StreamExt as _;
    engine()?;
    let dir = staging(&app, &tid)?;
    let plan = Arc::new(read_plan(&dir)?);
    let at = Arc::new(positions(&plan));
    let tree = dir.join("tree");
    let mut have = std::fs::OpenOptions::new().append(true).open(dir.join("have")).map_err(|_| not_found("incoming server"))?;
    let mut data = body.into_data_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut got = 0usize;
    loop {
        // Whole frames first.
        while buf.len() >= 68 {
            let len = u32::from_be_bytes([buf[64], buf[65], buf[66], buf[67]]) as usize;
            if len > MAX_CHUNK {
                return Err(bad("A chunk is too big."));
            }
            if buf.len() < 68 + len {
                break;
            }
            let hash = String::from_utf8_lossy(&buf[..64]).into_owned();
            if hash.as_bytes() == ERR_HASH {
                let msg = String::from_utf8_lossy(&buf[68..68 + len]).into_owned();
                return Err(ApiError(StatusCode::CONFLICT, crate::relay::clean_text(&msg)));
            }
            let chunk: Vec<u8> = buf.drain(..68 + len).skip(68).collect();
            let (tree, plan, at) = (tree.clone(), plan.clone(), at.clone());
            let mut h2 = have.try_clone().map_err(bad)?;
            tokio::task::spawn_blocking(move || put_chunk(&tree, &plan, &at, &mut h2, &hash, &chunk)).await.map_err(bad)?.map_err(|m| ApiError(StatusCode::CONFLICT, m))?;
            got += 1;
        }
        match data.next().await {
            Some(Ok(c)) => buf.extend_from_slice(&c),
            Some(Err(_)) => return Err(ApiError(StatusCode::BAD_GATEWAY, format!("The link broke after {got} chunks; the rest come when it's back."))),
            None => break,
        }
    }
    let _ = have.flush();
    if !buf.is_empty() {
        return Err(ApiError(StatusCode::BAD_GATEWAY, "The last chunk broke off.".into()));
    }
    Ok(Json(json!({ "got": got })))
}

#[derive(Deserialize)]
struct CommitBody {
    /// The port it gets here (another when its own is taken); 0 keeps it.
    #[serde(default)]
    port: u16,
}

#[cfg(unix)]
fn set_mode(p: &FsPath, m: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    if m != 0 {
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(m));
    }
}

#[cfg(not(unix))]
fn set_mode(p: &FsPath, m: u32) {
    if m != 0 && m & 0o200 == 0
        && let Ok(meta) = std::fs::metadata(p)
    {
        let mut perm = meta.permissions();
        perm.set_readonly(true);
        let _ = std::fs::set_permissions(p, perm);
    }
}

#[cfg(unix)]
fn make_link(target: &str, at: &FsPath, _dir: bool) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

#[cfg(windows)]
fn make_link(target: &str, at: &FsPath, dir: bool) -> std::io::Result<()> {
    let t = target.replace('/', "\\");
    if dir { std::os::windows::fs::symlink_dir(t, at) } else { std::os::windows::fs::symlink_file(t, at) }
}

/// Puts the finishing touches on a complete staging tree (links, modes,
/// times, Java) and renames it into the servers folder.
fn commit_tree(app: &App, dir: &FsPath, plan: &Plan, port: u16) -> anyhow::Result<PathBuf> {
    let tree = dir.join("tree");
    for i in &plan.items {
        let p = tree.join(&i.p);
        match i.k.as_str() {
            "f" => {
                let len = std::fs::symlink_metadata(&p)?.len();
                anyhow::ensure!(len == i.s, "{} came out the wrong size", i.p);
                if i.t > 0
                    && let Ok(f) = std::fs::OpenOptions::new().write(true).open(&p)
                {
                    let _ = f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(i.t as u64));
                }
            }
            "l" => {
                anyhow::ensure!(link_ok(&i.p, &i.l), "the link {} points outside the server folder", i.p);
                make_link(&i.l, &p, i.ld).map_err(|e| anyhow::anyhow!("couldn't make the link {}: {e}", i.p))?;
            }
            _ => {}
        }
    }
    // Modes last, deepest first, so a read-only folder doesn't stop the rest.
    for i in plan.items.iter().rev().filter(|i| i.k != "l") {
        set_mode(&tree.join(&i.p), i.m);
    }
    if port > 0 {
        let props = tree.join("server.properties");
        let mut p = hopper_core::hosting::Properties::load(&props).unwrap_or_default();
        p.set("server-port", port.to_string());
        p.save(&props)?;
    }
    // A Java path from the other machine: this machine's own instead.
    if let Ok((launch, _)) = hopper_core::hosting::script::read_launch(&tree)
        && launch.java.is_absolute()
        && !launch.java.exists()
    {
        let mc = hopper_core::hosting::script::start_info(&tree).minecraft.unwrap_or_default();
        let job = app.jobs.start_quiet(format!("Java for {}", plan.name));
        match crate::manage_api::java_for(&mc, &job) {
            Ok(j) => {
                if let Err(e) = hopper_core::hosting::version_change::repoint_java(&tree, &j) {
                    log::warn!("{}: couldn't point its start script at {}: {e:#}", plan.name, j.display());
                }
            }
            Err(e) => log::warn!("{}: no Java for Minecraft {mc} here: {e:#}", plan.name),
        }
    }
    let root = servers_root(app).map_err(|e| anyhow::anyhow!(e.1))?;
    let name = hopper_core::hosting::create::free_server_name(&root, &plan.name);
    let dest = root.join(hopper_core::instances::Instance::folder_name(&name));
    std::fs::rename(&tree, &dest)?;
    Ok(dest)
}

/// Makes the server once every chunk is in.
async fn commit(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(tid): Path<String>, Json(b): Json<CommitBody>) -> ApiResult<Json<Value>> {
    engine()?;
    let dir = staging(&app, &tid)?;
    let plan = read_plan(&dir)?;
    let left = missing(&plan, &have_set(&dir)).len();
    if left > 0 {
        return Err(bad(format!("{left} chunks are still missing.")));
    }
    let meta: Meta = std::fs::read(dir.join("meta.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let who = crate::mods_api::changer(&app, &headers, &method);
    let (app2, dir2, plan2) = (app.clone(), dir.clone(), plan.clone());
    let dest = tokio::task::spawn_blocking(move || commit_tree(&app2, &dir2, &plan2, b.port)).await.map_err(bad)?.map_err(|e| bad(format!("Couldn't finish the server: {e:#}")))?;
    let _ = std::fs::remove_dir_all(&dir);
    let from = if meta.from.is_empty() { "another panel".to_string() } else { meta.from };
    let (did, note) = if meta.mode == "copy" {
        (hopper_core::actor::Did::Copied, format!("Copied here from {from}."))
    } else {
        (hopper_core::actor::Did::Moved, format!("Here from {from}."))
    };
    if let Err(err) = hopper_core::actor::note_move(&dest, did, &plan.name, &note, &who) {
        log::warn!("{}: couldn't write the move to its History: {err}", plan.name);
    }
    app.rescan();
    crate::changes::emit(&app, None, &["list"]);
    app.refresh.notify_one();
    let id = app.servers.list().into_iter().find(|e| e.root == dest).map(|e| e.id.clone());
    log::info!("{} arrived from {from} ({})", plan.name, dest.display());
    Ok(Json(json!({ "id": id, "name": dest.file_name().map(|n| n.to_string_lossy().into_owned()) })))
}

/// Drops an incoming server: nothing of it stays.
async fn receive_abort(State(app): State<Arc<App>>, Path(tid): Path<String>) -> ApiResult<Json<Value>> {
    engine()?;
    let dir = staging(&app, &tid)?;
    if dir.exists() {
        tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&dir)).await.map_err(bad)?.map_err(|e| bad(format!("Couldn't remove the unfinished copy: {e}")))?;
    }
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_links() {
        assert!(rel_ok("world/region/r.0.0.mca"));
        for bad in ["", "/etc/passwd", "../x", "a/../../x", "a//b", "C:/x", "a\\b", "a/./b", "a\0"] {
            assert!(!rel_ok(bad), "{bad:?}");
        }
        assert!(link_ok("mods/a.jar", "../libs/a.jar"));
        assert!(link_ok("a/b/c", "../../d"));
        assert!(!link_ok("a/b", "../../d"));
        assert!(!link_ok("a", "/etc/passwd"));
        assert!(!link_ok("a", "C:\\Windows"));
        assert!(!link_ok("a", "..\\..\\x"));
    }

    #[test]
    fn plan_checks_parents_and_chunks() {
        let h = "a".repeat(64);
        let ok = Plan {
            items: vec![
                Item { p: "w".into(), k: "d".into(), ..Default::default() },
                Item { p: "w/f".into(), k: "f".into(), s: 3, c: vec![(h.clone(), 3)], ..Default::default() },
            ],
            ..Default::default()
        };
        assert!(plan_ok(&ok).is_ok());
        let mut through_link = ok.clone();
        through_link.items[0].k = "l".into();
        through_link.items[0].l = "x".into();
        assert!(plan_ok(&through_link).is_err());
        let mut wrong_size = ok.clone();
        wrong_size.items[1].s = 4;
        assert!(plan_ok(&wrong_size).is_err());
        let mut climb = ok;
        climb.items[1].p = "w/../../f".into();
        assert!(plan_ok(&climb).is_err());
    }
}
