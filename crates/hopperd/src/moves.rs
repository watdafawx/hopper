//! Moving or copying a server from one panel of a main to another: the
//! main itself or any of its nodes, either way (design/multi-panel.md, A5).
//!
//! The main runs the move. It asks the source for a file list cut into
//! chunks and the target for what it's still missing, then pipes chunk
//! streams from one to the other in batches; it never holds more than a
//! tunnel window of a server's files, and writes none of them to disk. A
//! cut link waits for the node to come back and goes on with what's still
//! missing; a move that can't go on stops at "paused" (Resume) or
//! "failed", and a failed one leaves nothing on the target.
//!
//! The ends are `crate::transfer`'s routes. A move needs the owner or a
//! manager here, both ends visible to them, and Manage on every node it
//! touches; each end checks the same again.

use crate::App;
use crate::api::{ApiError, ApiResult, bad, not_found, require_owner_who};
use crate::auth::{Role, Who};
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Chunks asked for in one go: about 16 MB at the average chunk size.
const BATCH: usize = if cfg!(test) { 4 } else { 64 };
/// How long a move waits for a node that dropped off before it pauses.
const LINK_WAIT: Duration = Duration::from_secs(120);
/// Finished moves kept in the list.
const KEEP_DONE: usize = 50;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/moves", get(list).post(start))
        .route("/api/v1/moves/places", get(places))
        .route("/api/v1/moves/precheck", post(precheck))
        .route("/api/v1/moves/{mid}", get(one).delete(dismiss))
        .route("/api/v1/moves/{mid}/resume", post(resume))
        .route("/api/v1/moves/{mid}/cancel", post(cancel))
}

/// An error as the start of a sentence for people (some come lower-case
/// from the link).
fn sentence(e: &str) -> String {
    let mut c = e.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// One move or copy, as the dialog shows it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Move {
    pub id: String,
    /// The server as this panel lists it ("tnp", or "vm-a:tnp").
    pub server: String,
    pub name: String,
    /// The node it comes from ("" for this panel) and its name.
    pub from: String,
    pub from_name: String,
    pub to: String,
    pub to_name: String,
    /// move or copy.
    pub mode: String,
    pub backups: bool,
    /// Who started it.
    pub by: String,
    /// preparing, sending, waiting (link down), finishing, paused, failed,
    /// cancelled, done.
    pub state: String,
    /// What it's doing, for people.
    pub step: String,
    /// Bytes read on the source (preparing) or sent (sending).
    pub done: u64,
    pub total: u64,
    /// Bytes that went over the wire, every try counted.
    pub sent: u64,
    pub chunks: usize,
    pub chunks_left: usize,
    /// Times it went on after the link broke.
    pub resumed: u32,
    pub error: String,
    /// The new server, as this panel lists it.
    pub new_id: String,
    pub new_port: u16,
    pub started: u64,
    pub updated: u64,
    #[serde(default)]
    prepared: bool,
    #[serde(default)]
    listed: bool,
    #[serde(default)]
    committed: bool,
}

impl Move {
    fn active(&self) -> bool {
        matches!(self.state.as_str(), "preparing" | "sending" | "waiting" | "finishing")
    }
}

/// The main's moves, saved in `moves.json` so a restart shows them (an
/// unfinished one as paused).
pub struct Moves {
    list: Mutex<Vec<Move>>,
    /// Moves asked to stop.
    stop: Mutex<Vec<String>>,
    /// Bytes sent by running moves, counted as they stream.
    counters: Mutex<HashMap<String, Arc<AtomicU64>>>,
    router: OnceLock<axum::Router>,
    path: std::path::PathBuf,
}

impl Moves {
    pub fn new(data_dir: &std::path::Path) -> Self {
        let path = data_dir.join("moves.json");
        let mut list: Vec<Move> = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        for m in list.iter_mut().filter(|m| m.active()) {
            m.state = "paused".into();
            m.step = "Hopper Server restarted. Resume to go on.".into();
        }
        Moves { list: Mutex::new(list), stop: Mutex::new(Vec::new()), counters: Mutex::new(HashMap::new()), router: OnceLock::new(), path }
    }

    fn save(&self, list: &[Move]) {
        if let Ok(b) = serde_json::to_vec_pretty(list) {
            let tmp = self.path.with_extension("tmp");
            if std::fs::write(&tmp, b).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }

    fn get(&self, id: &str) -> Option<Move> {
        let mut m = self.list.lock().unwrap().iter().find(|m| m.id == id).cloned()?;
        if let Some(c) = self.counters.lock().unwrap().get(id) {
            m.sent = c.load(Ordering::Relaxed);
        }
        Some(m)
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut Move)) {
        let mut list = self.list.lock().unwrap();
        if let Some(m) = list.iter_mut().find(|m| m.id == id) {
            f(m);
            m.updated = crate::security::now();
        }
        self.save(&list);
    }

    fn stopping(&self, id: &str) -> bool {
        self.stop.lock().unwrap().iter().any(|s| s == id)
    }
}

/// One end of a move: this panel (`node` None) or a node, and the server's
/// id there.
#[derive(Clone, Debug)]
struct End {
    node: Option<String>,
    name: String,
}

fn end(node: &str) -> End {
    End { node: (!node.is_empty()).then(|| node.to_string()), name: String::new() }
}

fn router(app: &Arc<App>) -> &axum::Router {
    app.moves.router.get_or_init(|| crate::api::router(app.clone()))
}

/// Answer of one call to an end: status and JSON.
async fn call_json(app: &Arc<App>, e: &End, user: &str, method: Method, path: &str, body: Value) -> Result<(StatusCode, Value), String> {
    let (st, b) = crate::relay::engine_call(app, router(app), e.node.as_deref(), user, method, path, Body::from(body.to_string())).await?;
    let bytes = axum::body::to_bytes(b, 64 * 1024 * 1024).await.map_err(|_| "the answer broke off".to_string())?;
    Ok((st, serde_json::from_slice(&bytes).unwrap_or(Value::Null)))
}

fn error_of(v: &Value, st: StatusCode) -> String {
    v.get("error").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("It answered {st}."))
}

/// A link problem (offline, a gateway error) rather than the end's own no.
fn link_status(st: StatusCode) -> bool {
    matches!(st, StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT)
}

fn panel_name() -> String {
    crate::config::hostname()
}

/// Checks a person may move `server` to `to`, and finds both ends.
fn resolve(app: &App, who: &Who, server: &str, to: &str) -> ApiResult<(End, String, End)> {
    if !who.sees(server) {
        return Err(not_found("server"));
    }
    let (from_node, sid) = match crate::relay::split(server) {
        Some((n, s)) => (n.to_string(), s.to_string()),
        None => (String::new(), server.to_string()),
    };
    let to_node = if to == "main" || to.is_empty() { String::new() } else { to.to_string() };
    if from_node == to_node {
        return Err(bad("It's already there. Pick another place."));
    }
    // A person limited to some servers or nodes moves only between nodes
    // they can see: never onto this panel itself.
    if to_node.is_empty() && who.limited() {
        return Err(ApiError(StatusCode::FORBIDDEN, "You can only move servers between nodes you can see.".into()));
    }
    if !to_node.is_empty() && !who.sees_node(&to_node) {
        return Err(not_found("node"));
    }
    let mut ends = Vec::new();
    for n in [&from_node, &to_node] {
        let mut e = end(n);
        if n.is_empty() {
            e.name = panel_name();
        } else {
            let info = app.nodes.relay_info(n).ok_or_else(|| not_found("node"))?;
            if info.ceiling != Role::Admin {
                return Err(ApiError(
                    StatusCode::FORBIDDEN,
                    format!("{} lets this panel {}. Moving servers needs Manage there.", info.name, crate::relay::ceiling_text(info.ceiling)),
                ));
            }
            e.name = info.name;
        }
        ends.push(e);
    }
    let to_end = ends.pop().unwrap();
    let from_end = ends.pop().unwrap();
    Ok((from_end, sid, to_end))
}

fn who_label(who: &Who) -> String {
    match who {
        Who::Owner => "Owner".into(),
        Who::User { name, .. } | Who::Device { name, .. } | Who::Token { name, .. } => name.clone(),
        Who::Main { user, .. } => user.clone(),
    }
}

/// Where a server may go: this panel and each node the person can see,
/// with why one can't take it.
async fn places(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let who = require_owner_who(&app, &headers, &method)?;
    let server = q.get("server").cloned().unwrap_or_default();
    let from = crate::relay::split(&server).map(|(n, _)| n.to_string()).unwrap_or_default();
    let mut out = Vec::new();
    if !who.limited() {
        out.push(json!({ "id": "main", "name": panel_name(), "here": from.is_empty(), "ok": true, "why": "" }));
    }
    for (id, _) in app.nodes.states() {
        if !who.sees_node(&id) {
            continue;
        }
        let Some(info) = app.nodes.relay_info(&id) else { continue };
        let why = if info.ceiling != Role::Admin {
            format!("Lets this panel {}; moving needs Manage.", crate::relay::ceiling_text(info.ceiling))
        } else if info.handle.is_none() {
            "Offline.".to_string()
        } else if info.maintenance.is_some() {
            "Under maintenance.".to_string()
        } else {
            String::new()
        };
        out.push(json!({ "id": id, "name": info.name, "here": id == from, "ok": why.is_empty(), "why": why }));
    }
    Ok(Json(json!(out)))
}

#[derive(Deserialize)]
struct MoveBody {
    server: String,
    to: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    backups: bool,
}

fn human(n: u64) -> String {
    hopper_core::instance_ops::human_bytes(n)
}

/// What the dialog shows before a move: one row per check, ok / warn /
/// fail, and whether it can go ahead.
async fn check_rows(app: &Arc<App>, who: &Who, b: &MoveBody) -> ApiResult<Value> {
    let (from, sid, to) = resolve(app, who, &b.server, &b.to)?;
    let user = who_label(who);
    if let Some(n) = &to.node
        && app.nodes.relay_info(n).is_some_and(|i| i.maintenance.is_some())
    {
        return Err(bad(format!("{} is under maintenance. End maintenance first.", to.name)));
    }
    let link = |e: String| ApiError(StatusCode::SERVICE_UNAVAILABLE, e);
    let path = format!("/api/v1/servers/{}/transfer/check", crate::relay::decode_seg(&sid));
    let (st, src) = call_json(app, &from, &user, Method::POST, &path, json!({ "backups": b.backups })).await.map_err(link)?;
    if st == StatusCode::NOT_FOUND && src.get("error").is_none() {
        return Err(bad(format!("Update {} to move servers from it.", from.name)));
    }
    if !st.is_success() {
        return Err(ApiError(st, error_of(&src, st)));
    }
    let size = src["size"].as_u64().unwrap_or(0);
    let port = src["port"].as_u64().unwrap_or(0) as u16;
    let name = src["name"].as_str().unwrap_or("").to_string();
    let (st, tgt) = call_json(app, &to, &user, Method::POST, "/api/v1/receive/precheck", json!({ "name": name, "size": size, "port": port, "minecraft": src["minecraft"] })).await.map_err(link)?;
    if st == StatusCode::NOT_FOUND {
        return Err(bad(format!("Update {} to move servers there.", to.name)));
    }
    if !st.is_success() {
        return Err(ApiError(st, error_of(&tgt, st)));
    }
    let mut rows = Vec::new();
    let mut row = |key: &str, level: &str, text: String| rows.push(json!({ "key": key, "level": level, "text": text }));
    let state = src["state"].as_str().unwrap_or("stopped");
    let running = state != "stopped" && state != "crashed";
    if src["sending"].as_bool() == Some(true) {
        row("busy", "fail", format!("{name} is being moved or copied already."));
    }
    if running {
        row("state", "fail", format!("{name} is running. Stop it first: its files are sent while it's stopped."));
    } else {
        row("state", "ok", format!("{name} is stopped."));
    }
    if let Some(l) = src["bad_links"].as_array().and_then(|a| a.first()).and_then(Value::as_str) {
        row("links", "fail", format!("\"{l}\" links to somewhere outside the server folder. Replace it with the real files first."));
    }
    let (free, need) = (tgt["free"].as_u64().unwrap_or(0), tgt["need"].as_u64().unwrap_or(0));
    if tgt["disk_ok"].as_bool() == Some(false) {
        row("disk", "fail", format!("Not enough space on {}: it needs {}, and {} is free.", to.name, human(need), human(free)));
    } else if free > 0 {
        row("disk", "ok", format!("{} to send; {} free on {}.", human(size), human(free), to.name));
    } else {
        row("disk", "ok", format!("{} to send.", human(size)));
    }
    let mut new_port = 0u16;
    if port > 0 {
        if tgt["port"]["ok"].as_bool() == Some(false) {
            let by = tgt["port"]["taken_by"].as_str().filter(|s| !s.is_empty()).map(|s| format!(" by {s}")).unwrap_or_default();
            match tgt["port"]["suggest"].as_u64() {
                Some(p) => {
                    new_port = p as u16;
                    row("port", "warn", format!("Port {port} is taken on {}{by}. It gets port {p} there.", to.name));
                }
                None => row("port", "warn", format!("Port {port} is taken on {}{by}. Change its port there before you start it.", to.name)),
            }
        } else {
            row("port", "ok", format!("Port {port} is free on {}.", to.name));
        }
    }
    let mc = src["minecraft"].as_str().unwrap_or("");
    if tgt["java"]["ok"].as_bool() == Some(false) {
        let major = tgt["java"]["major"].as_u64().unwrap_or(21);
        row("java", "warn", format!("{} has no Java {major} yet. It downloads it when the server arrives.", to.name));
    } else if !mc.is_empty() {
        row("java", "ok", format!("{} has a Java for Minecraft {mc}.", to.name));
    }
    let loader = src["loader"].as_str().filter(|l| !l.is_empty()).unwrap_or("Vanilla");
    row("loader", "ok", format!("{loader}{}: its files go as they are; nothing is installed again.", if mc.is_empty() { String::new() } else { format!(" {mc}") }));
    let bk = src["backups_size"].as_u64().unwrap_or(0);
    if !b.backups && bk > 0 {
        row("backups", "ok", format!("Its backups ({}) stay on {}.", human(bk), from.name));
    }
    if b.mode != "copy" {
        row("source", "ok", format!("The copy on {} is kept, stopped, and marked as moved.", from.name));
    }
    let ok = rows.iter().all(|r| r["level"] != "fail");
    Ok(json!({
        "ok": ok, "rows": rows, "running": running, "size": size, "name": name, "new_port": new_port,
        "from": from.node.clone().unwrap_or_default(), "from_name": from.name, "to": to.node.clone().unwrap_or_default(), "to_name": to.name,
    }))
}

async fn precheck(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<MoveBody>) -> ApiResult<Json<Value>> {
    let who = require_owner_who(&app, &headers, &method)?;
    Ok(Json(check_rows(&app, &who, &b).await?))
}

fn new_id() -> String {
    let mut b = [0u8; 8];
    let _ = getrandom::fill(&mut b);
    format!("m{}", hopper_core::hosting::chunkstore::hex(&b))
}

async fn start(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<MoveBody>) -> ApiResult<Json<Value>> {
    let who = require_owner_who(&app, &headers, &method)?;
    let c = check_rows(&app, &who, &b).await?;
    if c["ok"] != true {
        let why = c["rows"].as_array().and_then(|r| r.iter().find(|r| r["level"] == "fail")).and_then(|r| r["text"].as_str()).unwrap_or("It can't go yet.").to_string();
        return Err(bad(why));
    }
    let busy = app.moves.list.lock().unwrap().iter().any(|m| m.server == b.server && (m.active() || m.state == "paused"));
    if busy {
        return Err(bad("That server has a move under way. Resume or cancel it first."));
    }
    let now = crate::security::now();
    let m = Move {
        id: new_id(),
        server: b.server.clone(),
        name: c["name"].as_str().unwrap_or("").into(),
        from: c["from"].as_str().unwrap_or("").into(),
        from_name: c["from_name"].as_str().unwrap_or("").into(),
        to: c["to"].as_str().unwrap_or("").into(),
        to_name: c["to_name"].as_str().unwrap_or("").into(),
        mode: if b.mode == "copy" { "copy".into() } else { "move".into() },
        backups: b.backups,
        by: who_label(&who),
        state: "preparing".into(),
        step: "Reading its files".into(),
        total: c["size"].as_u64().unwrap_or(0),
        new_port: c["new_port"].as_u64().unwrap_or(0) as u16,
        started: now,
        updated: now,
        ..Default::default()
    };
    log::info!("{} {} {} from {} to {}", m.by, if m.mode == "copy" { "copies" } else { "moves" }, m.name, m.from_name, m.to_name);
    {
        let mut list = app.moves.list.lock().unwrap();
        list.push(m.clone());
        let done: Vec<usize> = list.iter().enumerate().filter(|(_, m)| !m.active() && m.state != "paused").map(|(i, _)| i).collect();
        if done.len() > KEEP_DONE {
            let drop = done.len() - KEEP_DONE;
            let ids: Vec<String> = done.iter().take(drop).map(|i| list[*i].id.clone()).collect();
            list.retain(|m| !ids.contains(&m.id));
        }
        app.moves.save(&list);
    }
    spawn(app.clone(), m.id.clone());
    Ok(Json(json!(m)))
}

fn spawn(app: Arc<App>, id: String) {
    tokio::spawn(async move {
        run(&app, &id).await;
        app.moves.counters.lock().unwrap().remove(&id);
        app.moves.stop.lock().unwrap().retain(|s| s != &id);
    });
}

/// Why a step didn't work: the link (wait and go on) or the end said no.
enum Stop {
    Link(String),
    Hard(String),
    Cancelled,
}

fn sees_move(who: &Who, m: &Move) -> bool {
    who.sees(&m.server) && (m.to.is_empty() || who.sees_node(&m.to))
}

async fn list(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    let who = require_owner_who(&app, &headers, &method)?;
    let ids: Vec<String> = app.moves.list.lock().unwrap().iter().filter(|m| sees_move(&who, m)).map(|m| m.id.clone()).collect();
    let out: Vec<Move> = ids.iter().filter_map(|id| app.moves.get(id)).collect();
    Ok(Json(json!(out)))
}

fn visible(app: &App, who: &Who, mid: &str) -> ApiResult<Move> {
    app.moves.get(mid).filter(|m| sees_move(who, m)).ok_or_else(|| not_found("move"))
}

async fn one(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(mid): Path<String>) -> ApiResult<Json<Value>> {
    let who = require_owner_who(&app, &headers, &method)?;
    Ok(Json(json!(visible(&app, &who, &mid)?)))
}

async fn resume(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(mid): Path<String>) -> ApiResult<Json<Value>> {
    let who = require_owner_who(&app, &headers, &method)?;
    let m = visible(&app, &who, &mid)?;
    if m.state != "paused" {
        return Err(bad("Only a paused move can go on."));
    }
    // The person resuming must still be allowed both ends.
    resolve(&app, &who, &m.server, if m.to.is_empty() { "main" } else { &m.to })?;
    app.moves.update(&mid, |m| {
        m.state = "sending".into();
        m.step = "Going on".into();
        m.error.clear();
        m.resumed += 1;
    });
    spawn(app.clone(), mid.clone());
    Ok(Json(json!(app.moves.get(&mid))))
}

async fn cancel(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(mid): Path<String>) -> ApiResult<Json<Value>> {
    let who = require_owner_who(&app, &headers, &method)?;
    let m = visible(&app, &who, &mid)?;
    if m.active() {
        app.moves.stop.lock().unwrap().push(mid.clone());
    } else if m.state == "paused" {
        let app2 = app.clone();
        tokio::spawn(async move { undo(&app2, &m, "cancelled", "Cancelled. Nothing was left on the target.").await });
    } else {
        return Err(bad("It has already ended."));
    }
    Ok(Json(json!({ "ok": true })))
}

async fn dismiss(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(mid): Path<String>) -> ApiResult<Json<Value>> {
    let who = require_owner_who(&app, &headers, &method)?;
    let m = visible(&app, &who, &mid)?;
    if m.active() || m.state == "paused" {
        return Err(bad("Cancel it first."));
    }
    let mut list = app.moves.list.lock().unwrap();
    list.retain(|x| x.id != mid);
    app.moves.save(&list);
    Ok(Json(json!({ "ok": true })))
}

fn ends_of(m: &Move) -> (End, String, End) {
    let sid = crate::relay::split(&m.server).map(|(_, s)| s.to_string()).unwrap_or_else(|| m.server.clone());
    let mut from = end(&m.from);
    from.name = m.from_name.clone();
    let mut to = end(&m.to);
    to.name = m.to_name.clone();
    (from, sid, to)
}

/// Ends a move that won't finish: the target drops what it got, the
/// source may start again.
async fn undo(app: &Arc<App>, m: &Move, state: &str, text: &str) {
    let (from, sid, to) = ends_of(m);
    let mut left = String::new();
    match call_json(app, &to, &m.by, Method::DELETE, &format!("/api/v1/receive/{}", m.id), Value::Null).await {
        Ok((st, _)) if st.is_success() => {}
        _ => left = format!(" {} couldn't be reached to remove the part it got; it removes it within a week.", to.name),
    }
    let path = format!("/api/v1/servers/{sid}/transfer/{}/finish", m.id);
    let _ = call_json(app, &from, &m.by, Method::POST, &path, json!({ "outcome": "cancelled" })).await;
    app.moves.update(&m.id, |x| {
        x.state = state.into();
        x.step = format!("{text}{left}");
    });
}

async fn run(app: &Arc<App>, id: &str) {
    let counter = Arc::new(AtomicU64::new(app.moves.get(id).map_or(0, |m| m.sent)));
    app.moves.counters.lock().unwrap().insert(id.to_string(), counter.clone());
    loop {
        let Some(m) = app.moves.get(id) else { return };
        let r = steps(app, &m, &counter).await;
        let m = app.moves.get(id).unwrap_or(m);
        match r {
            Ok(()) => return,
            Err(Stop::Cancelled) => return undo(app, &m, "cancelled", "Cancelled. Nothing was left on the target.").await,
            Err(Stop::Hard(e)) => {
                log::warn!("moving {} to {} failed: {e}", m.name, m.to_name);
                app.moves.update(id, |x| x.error = e.clone());
                return undo(app, &m, "failed", "It didn't work. Nothing was left on the target.").await;
            }
            Err(Stop::Link(e)) => {
                log::info!("moving {}: {e}; waiting for the link", m.name);
                app.moves.update(id, |x| {
                    x.state = "waiting".into();
                    x.step = format!("{} Waiting for it to come back…", sentence(&e));
                });
                if !wait_link(app, &m).await {
                    app.moves.update(id, |x| {
                        x.state = "paused".into();
                        x.step = "The link didn't come back. Resume when it's back; only what's missing is sent.".into();
                        x.error = e;
                    });
                    return;
                }
                app.moves.update(id, |x| {
                    x.resumed += 1;
                    x.state = "sending".into();
                });
            }
        }
    }
}

/// Waits for every node of the move to be connected again.
async fn wait_link(app: &Arc<App>, m: &Move) -> bool {
    let until = tokio::time::Instant::now() + LINK_WAIT;
    while tokio::time::Instant::now() < until {
        if app.moves.stopping(&m.id) {
            return true;
        }
        let up = [&m.from, &m.to].iter().filter(|n| !n.is_empty()).all(|n| app.nodes.relay_info(n).is_some_and(|i| i.handle.is_some()));
        if up {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

fn hard_or_link(st: StatusCode, v: &Value) -> Stop {
    if link_status(st) { Stop::Link(error_of(v, st)) } else { Stop::Hard(error_of(v, st)) }
}

async fn steps(app: &Arc<App>, m: &Move, counter: &Arc<AtomicU64>) -> Result<(), Stop> {
    let (from, sid, to) = ends_of(m);
    let user = m.by.clone();
    let base = format!("/api/v1/servers/{sid}/transfer/{}", m.id);
    let check = || if app.moves.stopping(&m.id) { Err(Stop::Cancelled) } else { Ok(()) };
    let r = router(app);
    if !m.prepared {
        app.moves.update(&m.id, |x| {
            x.state = "preparing".into();
            x.step = "Reading its files".into();
        });
        let (st, body) = crate::relay::engine_call(app, r, from.node.as_deref(), &user, Method::POST, &format!("{base}/prepare"), Body::from(json!({ "backups": m.backups }).to_string()))
            .await
            .map_err(Stop::Link)?;
        if !st.is_success() {
            let b = axum::body::to_bytes(body, 1 << 20).await.unwrap_or_default();
            return Err(hard_or_link(st, &serde_json::from_slice(&b).unwrap_or(Value::Null)));
        }
        let mut lines = BodyLines { s: body.into_data_stream(), buf: Vec::new() };
        loop {
            check()?;
            let Some(line) = lines.next().await.map_err(Stop::Link)? else {
                return Err(Stop::Link(format!("{} stopped while reading the files.", from.name)));
            };
            let v: Value = serde_json::from_slice(&line).unwrap_or(Value::Null);
            if let Some(e) = v["error"].as_str() {
                return Err(Stop::Hard(e.to_string()));
            }
            if v["ok"] == true {
                app.moves.update(&m.id, |x| {
                    x.prepared = true;
                    x.total = v["unique"].as_u64().unwrap_or(0);
                    x.chunks = v["chunks"].as_u64().unwrap_or(0) as usize;
                    x.done = 0;
                });
                break;
            }
            app.moves.update(&m.id, |x| {
                x.done = v["done"].as_u64().unwrap_or(0);
                x.total = v["total"].as_u64().unwrap_or(x.total);
            });
        }
    }
    let m = app.moves.get(&m.id).ok_or(Stop::Cancelled)?;
    if !m.listed {
        app.moves.update(&m.id, |x| {
            x.state = "sending".into();
            x.step = format!("Sending the file list to {}", to.name);
        });
        let (st, list) = crate::relay::engine_call(app, r, from.node.as_deref(), &user, Method::GET, &format!("{base}/manifest"), Body::empty()).await.map_err(Stop::Link)?;
        if !st.is_success() {
            let b = axum::body::to_bytes(list, 1 << 20).await.unwrap_or_default();
            return Err(hard_or_link(st, &serde_json::from_slice(&b).unwrap_or(Value::Null)));
        }
        let from_q = percent(&from.name);
        let path = format!("/api/v1/receive/{}/manifest?mode={}&from={from_q}", m.id, m.mode);
        let (st, ans) = crate::relay::engine_call(app, r, to.node.as_deref(), &user, Method::PUT, &path, list).await.map_err(Stop::Link)?;
        let b = axum::body::to_bytes(ans, 1 << 20).await.map_err(|_| Stop::Link(format!("{} stopped answering.", to.name)))?;
        if !st.is_success() {
            return Err(hard_or_link(st, &serde_json::from_slice(&b).unwrap_or(Value::Null)));
        }
        app.moves.update(&m.id, |x| x.listed = true);
    }
    if !m.committed {
        loop {
            check()?;
            let (st, v) = call_json(app, &to, &user, Method::GET, &format!("/api/v1/receive/{}", m.id), Value::Null).await.map_err(Stop::Link)?;
            if !st.is_success() {
                return Err(hard_or_link(st, &v));
            }
            let missing: Vec<String> = v["missing"].as_array().map(|a| a.iter().filter_map(|h| h.as_str().map(str::to_string)).collect()).unwrap_or_default();
            let all = v["chunks"].as_u64().unwrap_or(0) as usize;
            app.moves.update(&m.id, |x| {
                x.chunks_left = missing.len();
                x.chunks = all;
                // Bytes in place, from the chunks still missing (sizes vary;
                // a count is close enough for the bar).
                x.done = if all == 0 { x.total } else { x.total.saturating_mul((all - missing.len()) as u64) / all as u64 };
                x.state = "sending".into();
                x.step = format!("Sending to {}", to.name);
            });
            if missing.is_empty() {
                break;
            }
            for batch in missing.chunks(BATCH) {
                check()?;
                let path = format!("{base}/chunks");
                let (st, src) = crate::relay::engine_call(app, r, from.node.as_deref(), &user, Method::POST, &path, Body::from(json!({ "hashes": batch }).to_string())).await.map_err(Stop::Link)?;
                if !st.is_success() {
                    let b = axum::body::to_bytes(src, 1 << 20).await.unwrap_or_default();
                    return Err(hard_or_link(st, &serde_json::from_slice(&b).unwrap_or(Value::Null)));
                }
                let counted = count(src, counter.clone(), cut_at(app));
                let (st, ans) = crate::relay::engine_call(app, r, to.node.as_deref(), &user, Method::PUT, &format!("/api/v1/receive/{}/chunks", m.id), counted).await.map_err(Stop::Link)?;
                let b = axum::body::to_bytes(ans, 1 << 20).await.map_err(|_| Stop::Link(format!("{} stopped answering.", to.name)))?;
                if !st.is_success() {
                    return Err(hard_or_link(st, &serde_json::from_slice(&b).unwrap_or(Value::Null)));
                }
                let left = app.moves.get(&m.id).map_or(0, |x| x.chunks_left).saturating_sub(batch.len());
                app.moves.update(&m.id, |x| {
                    x.chunks_left = left;
                    x.done = if x.chunks == 0 { x.total } else { x.total.saturating_mul((x.chunks - left) as u64) / x.chunks as u64 };
                    x.sent = counter.load(Ordering::Relaxed);
                });
            }
        }
        check()?;
        app.moves.update(&m.id, |x| {
            x.state = "finishing".into();
            x.step = format!("Making the server on {}", to.name);
        });
        let (st, v) = call_json(app, &to, &user, Method::POST, &format!("/api/v1/receive/{}/commit", m.id), json!({ "port": m.new_port })).await.map_err(Stop::Link)?;
        if !st.is_success() {
            return Err(hard_or_link(st, &v));
        }
        let new = v["id"].as_str().unwrap_or("").to_string();
        let new_id = if m.to.is_empty() || new.is_empty() { new.clone() } else { crate::relay::join(&m.to, &new) };
        app.moves.update(&m.id, |x| {
            x.committed = true;
            x.new_id = new_id;
        });
    }
    // The server is on the target now; what's left is the source's mark.
    let m = app.moves.get(&m.id).ok_or(Stop::Cancelled)?;
    let outcome = if m.mode == "copy" { "copied" } else { "moved" };
    let new_local = crate::relay::split(&m.new_id).map(|(_, s)| s.to_string()).unwrap_or_else(|| m.new_id.clone());
    let body = json!({ "outcome": outcome, "dest_name": to.name, "dest_node": m.to, "dest_id": new_local });
    let (st, v) = call_json(app, &from, &user, Method::POST, &format!("{base}/finish"), body).await.map_err(Stop::Link)?;
    let note = if st.is_success() {
        String::new()
    } else {
        format!(" {} couldn't mark the old copy as moved ({}).", from.name, error_of(&v, st))
    };
    let text = if m.mode == "copy" { format!("Copied to {}.", to.name) } else { format!("Moved to {}. The old copy on {} is kept, stopped.", to.name, from.name) };
    app.moves.update(&m.id, |x| {
        x.state = "done".into();
        x.step = format!("{text}{note}");
        x.done = x.total;
        x.chunks_left = 0;
        x.sent = counter.load(Ordering::Relaxed);
    });
    log::info!("{} {} {}", m.name, if m.mode == "copy" { "copied to" } else { "moved to" }, to.name);
    crate::changes::emit(app, None, &["list"]);
    app.refresh.notify_one();
    Ok(())
}

/// Newline-separated JSON from an answer body, one line per call.
struct BodyLines {
    s: axum::body::BodyDataStream,
    buf: Vec<u8>,
}

impl BodyLines {
    /// The next line; `None` at the end. `Err` when the body broke off.
    async fn next(&mut self) -> Result<Option<Vec<u8>>, String> {
        use futures_util::StreamExt as _;
        loop {
            if let Some(i) = self.buf.iter().position(|b| *b == b'\n') {
                let mut line: Vec<u8> = self.buf.drain(..=i).collect();
                line.pop();
                return Ok(Some(line));
            }
            if self.buf.len() > crate::relay::MAX_LINE {
                return Err("a line was too long".into());
            }
            match self.s.next().await {
                Some(Ok(d)) => self.buf.extend_from_slice(&d),
                Some(Err(_)) => return Err("The link broke while reading the files.".into()),
                None => return Ok(None),
            }
        }
    }
}

/// A body that counts its bytes as they pass. `cut` (tests) breaks it off
/// once that many bytes went, as a dropped link would.
fn count(b: Body, c: Arc<AtomicU64>, cut: u64) -> Body {
    use futures_util::StreamExt as _;
    let s = b.into_data_stream().map(move |r| {
        if let Ok(d) = &r {
            let n = c.fetch_add(d.len() as u64, Ordering::Relaxed) + d.len() as u64;
            if cut > 0 && n >= cut {
                return Err(std::io::Error::other("the link was cut (test)"));
            }
        }
        r.map_err(std::io::Error::other)
    });
    Body::from_stream(s)
}

/// Tests: cut the link once after this many bytes, per main panel.
#[cfg(test)]
pub static CUT: Mutex<Vec<(std::path::PathBuf, u64)>> = Mutex::new(Vec::new());

#[cfg(test)]
fn cut_at(app: &App) -> u64 {
    let mut c = CUT.lock().unwrap();
    match c.iter().position(|(d, _)| *d == app.data_dir) {
        Some(i) => {
            let at = c[i].1;
            // Only while this batch is under way; the next one goes through.
            c.remove(i);
            at
        }
        None => 0,
    }
}

#[cfg(not(test))]
fn cut_at(_app: &App) -> u64 {
    0
}

fn percent(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}
