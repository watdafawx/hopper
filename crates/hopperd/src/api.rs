//! The HTTP API (`/api/v1`) and the live WebSocket. The web UI and Hopper
//! desktop use the same routes.
//!
//! Auth: browsers carry the `hopperd_session` cookie (HttpOnly, SameSite
//! Strict) and must also send `X-Hopperd: 1` on anything that changes
//! state, which other sites can't add; devices send `Authorization: Bearer`.

use crate::auth::{MIN_PASSWORD, PairStatus, Who};
use crate::servers::Summary;
use crate::{App, Snapshot};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use hopper_core::panel::PanelAction;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

pub const COOKIE: &str = "hopperd_session";
pub const API_VERSION: u32 = 1;

/// Features this panel has, so clients light up only what both sides know.
pub const CAPABILITIES: &[&str] =
    &["servers", "power", "console", "console.ws", "stats", "host", "pairing", "files", "files.upload", "files.zip", "files.trash", "files.text", "mods.scan", "servers.infos", "configs", "crash", "neoforge", "pack.update", "jobs"];

pub struct ApiError(pub StatusCode, pub String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

pub(crate) fn bad(e: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, e.to_string())
}

pub(crate) fn not_found(what: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, format!("No {what} with that id."))
}

pub(crate) type ApiResult<T> = Result<T, ApiError>;

/// The session cookie or bearer token on a request.
fn token(headers: &HeaderMap) -> (String, bool) {
    if let Some(b) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        return (b.trim().to_string(), false);
    }
    let cookie = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| kv.trim().strip_prefix(&format!("{COOKIE}=")).map(str::to_string))
        .unwrap_or_default();
    (cookie, true)
}

/// Who is asking; refuses anonymous requests, and cookie requests that
/// change state without the `X-Hopperd` header.
pub(crate) fn require(app: &App, headers: &HeaderMap, method: &Method) -> ApiResult<Who> {
    let (tok, from_cookie) = token(headers);
    let who = app
        .auth
        .check(&tok)
        .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "Sign in first.".into()))?;
    if from_cookie && method != Method::GET && headers.get("x-hopperd").is_none() {
        return Err(ApiError(StatusCode::FORBIDDEN, "Missing X-Hopperd header.".into()));
    }
    Ok(who)
}

fn require_owner(app: &App, headers: &HeaderMap, method: &Method) -> ApiResult<()> {
    match require(app, headers, method)? {
        Who::Owner => Ok(()),
        Who::Device { .. } => Err(ApiError(StatusCode::FORBIDDEN, "Only the owner can do that.".into())),
    }
}

fn session_cookie(token: &str) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}",
        30 * 24 * 3600
    ))
    .expect("cookie header")
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/api/v1/hello", get(hello))
        .route("/api/v1/auth/setup", post(setup))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/me", get(me))
        .route("/api/v1/pair/request", post(pair_request))
        .route("/api/v1/pair/status", post(pair_status))
        .route("/api/v1/pair/pending", get(pair_pending))
        .route("/api/v1/pair/{id}/answer", post(pair_answer))
        .route("/api/v1/devices", get(devices))
        .route("/api/v1/devices/{id}", axum::routing::delete(forget_device))
        .route("/api/v1/host", get(host))
        .route("/api/v1/servers", get(servers))
        .route("/api/v1/servers/rescan", post(rescan))
        .route("/api/v1/servers/infos", get(infos))
        .route("/api/v1/servers/{id}/mods", get(mods))
        .route("/api/v1/servers/{id}/crash", get(crash))
        .route("/api/v1/servers/{id}/neoforge", get(neoforge))
        .route("/api/v1/servers/{id}", get(server))
        .route("/api/v1/servers/{id}/power", post(power))
        .route("/api/v1/servers/{id}/command", post(command))
        .route("/api/v1/servers/{id}/console", get(console))
        .route("/api/v1/ws", get(ws))
        .merge(crate::fs_api::routes())
        .merge(crate::pack_api::routes())
        .fallback(crate::ui::serve)
        .with_state(app)
}

// ---- hello and auth ----------------------------------------------------

async fn hello(State(app): State<Arc<App>>) -> Json<Value> {
    let cfg = app.cfg.read().unwrap();
    Json(json!({
        "panel": "hopperd",
        "name": cfg.name,
        "version": env!("CARGO_PKG_VERSION"),
        "api": API_VERSION,
        "setup_needed": app.auth.needs_setup(),
        "addresses": app.addresses,
        "port": cfg.port,
        "capabilities": CAPABILITIES,
    }))
}

#[derive(Deserialize)]
struct SetupBody {
    code: String,
    password: String,
}

async fn setup(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(b): Json<SetupBody>,
) -> ApiResult<Response> {
    let token = app.auth.setup(&b.code, &b.password, addr.ip()).map_err(bad)?;
    let _ = std::fs::remove_file(app.data_dir.join(crate::SETUP_FILE));
    log::info!("owner set up from {}", addr.ip());
    Ok(([(header::SET_COOKIE, session_cookie(&token))], Json(json!({ "ok": true }))).into_response())
}

#[derive(Deserialize)]
struct LoginBody {
    password: String,
}

async fn login(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(b): Json<LoginBody>,
) -> ApiResult<Response> {
    let token = app
        .auth
        .login(&b.password, addr.ip())
        .map_err(|e| ApiError(StatusCode::UNAUTHORIZED, e.to_string()))?;
    Ok(([(header::SET_COOKIE, session_cookie(&token))], Json(json!({ "ok": true }))).into_response())
}

async fn logout(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    let (tok, _) = token(&headers);
    app.auth.logout(&tok);
    let clear = HeaderValue::from_static("hopperd_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
    ([(header::SET_COOKIE, clear)], Json(json!({ "ok": true }))).into_response()
}

async fn me(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    Ok(Json(match require(&app, &headers, &method)? {
        Who::Owner => json!({ "role": "owner", "min_password": MIN_PASSWORD }),
        Who::Device { id, name } => json!({ "role": "device", "id": id, "name": name }),
    }))
}

// ---- pairing -------------------------------------------------------------

#[derive(Deserialize)]
struct PairBody {
    device: String,
}

async fn pair_request(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(b): Json<PairBody>,
) -> ApiResult<Json<Value>> {
    let (id, secret, code) = app.auth.pair_request(&b.device, addr.ip()).map_err(bad)?;
    log::info!("pairing request from {} ({}): code {code}", b.device.trim(), addr.ip());
    Ok(Json(json!({ "id": id, "secret": secret, "code": code })))
}

#[derive(Deserialize)]
struct PairStatusBody {
    id: String,
    secret: String,
}

async fn pair_status(State(app): State<Arc<App>>, Json(b): Json<PairStatusBody>) -> Json<PairStatus> {
    Json(app.auth.pair_status(&b.id, &b.secret))
}

async fn pair_pending(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    Ok(Json(json!(app.auth.pending())))
}

#[derive(Deserialize)]
struct AnswerBody {
    allow: bool,
}

async fn pair_answer(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<AnswerBody>,
) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    app.auth.answer(&id, b.allow).map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

async fn devices(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    Ok(Json(json!(app.auth.devices())))
}

async fn forget_device(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    if !app.auth.forget_device(&id) {
        return Err(not_found("device"));
    }
    Ok(Json(json!({ "ok": true })))
}

// ---- servers -------------------------------------------------------------

async fn host(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    Ok(Json(json!(*app.host.read().unwrap())))
}

async fn servers(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let snap = app.snapshot.borrow().clone();
    Ok(Json(json!(snap.servers)))
}

/// Every server as Hopper desktop describes one: folder, manager, loader,
/// Java, memory and server.properties.
async fn infos(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let a = app.clone();
    let list = tokio::task::spawn_blocking(move || {
        a.servers
            .list()
            .iter()
            .map(|e| {
                let i = hopper_core::server::load(&e.root);
                json!({
                    "id": e.id, "name": e.name, "path": e.root.to_string_lossy(), "managed_by": e.managed_by,
                    "loader": i.loader, "java": i.java, "max_memory": i.max_memory, "properties": i.properties,
                })
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(bad)?;
    Ok(Json(json!(list)))
}

/// The server's mods, read from the jars here so only the list travels.
async fn mods(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let dir = e.root.join("mods");
    let mods = tokio::task::spawn_blocking(move || {
        if dir.is_dir() { hopper_core::scan::scan_folder(&dir) } else { Ok(Vec::new()) }
    })
    .await
    .map_err(bad)?
    .map_err(|e| bad(format!("{e:#}")))?;
    Ok(Json(json!({ "mods": mods })))
}

/// The server's last crash, from its logs and crash reports.
async fn crash(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let c = tokio::task::spawn_blocking(move || hopper_core::crashscan::last_crash(&e.root)).await.map_err(bad)?;
    Ok(Json(json!({ "crash": c })))
}

/// The NeoForge version the server's run.bat starts.
async fn neoforge(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let v = hopper_core::server_update::server_neoforge(&e.root);
    Ok(Json(json!({ "version": v })))
}

async fn rescan(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let a = app.clone();
    tokio::task::spawn_blocking(move || a.rescan()).await.map_err(bad)?;
    app.refresh.notify_one();
    Ok(Json(json!({ "servers": app.servers.list().len() })))
}

async fn server(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
) -> ApiResult<Json<Summary>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let a = app.clone();
    tokio::task::spawn_blocking(move || Json(a.servers.summary(&e))).await.map_err(bad)
}

#[derive(Deserialize)]
struct PowerBody {
    action: String,
}

async fn power(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<PowerBody>,
) -> ApiResult<Json<Value>> {
    let who = require(&app, &headers, &method)?;
    let action = match b.action.as_str() {
        "start" => PanelAction::Start,
        "stop" => PanelAction::Stop,
        "restart" => PanelAction::Restart,
        "kill" => PanelAction::Kill,
        other => return Err(bad(format!("Unknown action \"{other}\". Use start, stop, restart or kill."))),
    };
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    log::info!("{} {} by {}", b.action, e.name, who_name(&who));
    let a = app.clone();
    tokio::task::spawn_blocking(move || a.servers.act(&e, action))
        .await
        .map_err(bad)?
        .map_err(|e| ApiError(StatusCode::CONFLICT, format!("{e:#}")))?;
    app.refresh.notify_one();
    Ok(Json(json!({ "ok": true })))
}

fn who_name(w: &Who) -> String {
    match w {
        Who::Owner => "owner".into(),
        Who::Device { name, .. } => name.clone(),
    }
}

#[derive(Deserialize)]
struct CommandBody {
    line: String,
}

async fn command(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<CommandBody>,
) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    app.servers
        .command(&e, &b.line)
        .map_err(|e| ApiError(StatusCode::CONFLICT, format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true })))
}

async fn console(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let since = q.get("since").and_then(|s| s.parse().ok()).unwrap_or(0);
    let (lines, total) = e.server.console_since(since);
    Ok(Json(json!({ "lines": lines, "total": total })))
}

// ---- live updates ----------------------------------------------------------

/// Browsers: the cookie, from a page on this panel (Origin matches Host).
/// Devices: a bearer token.
fn ws_allowed(app: &App, headers: &HeaderMap) -> bool {
    let (tok, from_cookie) = token(headers);
    if app.auth.check(&tok).is_none() {
        return false;
    }
    if !from_cookie {
        return true;
    }
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()).unwrap_or("");
    let origin_host = origin.split("://").nth(1).unwrap_or("");
    !host.is_empty() && origin_host == host
}

async fn ws(State(app): State<Arc<App>>, headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    if !ws_allowed(&app, &headers) {
        return ApiError(StatusCode::UNAUTHORIZED, "Sign in first.".into()).into_response();
    }
    upgrade.on_upgrade(move |socket| live(app, socket))
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum ClientMsg {
    /// Follow a server's console (all lines first, then new ones).
    Console { server: String },
    Unconsole { server: String },
}

/// One client's live feed: the servers and host snapshot whenever it
/// changes (about once a second) and new console lines of the servers it
/// follows (checked every 150 ms).
async fn live(app: Arc<App>, mut socket: WebSocket) {
    let mut snap_rx = app.snapshot.subscribe();
    let mut follows: HashMap<String, u64> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_millis(150));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let first = snap_rx.borrow_and_update().clone();
    if send_snapshot(&mut socket, &first).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            changed = snap_rx.changed() => {
                if changed.is_err() { break; }
                let snap = snap_rx.borrow_and_update().clone();
                if send_snapshot(&mut socket, &snap).await.is_err() { break; }
            }
            _ = tick.tick() => {
                for (id, seen) in follows.iter_mut() {
                    let Some(e) = app.servers.get(id) else { continue };
                    let (lines, total) = e.server.console_since(*seen);
                    let reset = total < *seen || *seen == u64::MAX;
                    if lines.is_empty() && !reset { continue; }
                    *seen = total;
                    let msg = json!({ "t": "console", "server": id, "lines": lines, "reset": reset });
                    if socket.send(Message::Text(msg.to_string().into())).await.is_err() { return; }
                }
            }
            msg = socket.recv() => {
                let Some(Ok(msg)) = msg else { break };
                let Message::Text(text) = msg else { continue };
                match serde_json::from_str::<ClientMsg>(&text) {
                    // u64::MAX: send everything, flagged as a fresh console.
                    Ok(ClientMsg::Console { server }) => { follows.insert(server, u64::MAX); }
                    Ok(ClientMsg::Unconsole { server }) => { follows.remove(&server); }
                    Err(e) => log::debug!("ws: {e}"),
                }
            }
        }
    }
}

async fn send_snapshot(socket: &mut WebSocket, snap: &Snapshot) -> Result<(), axum::Error> {
    let msg = json!({ "t": "snapshot", "servers": snap.servers, "host": snap.host });
    socket.send(Message::Text(msg.to_string().into())).await
}
