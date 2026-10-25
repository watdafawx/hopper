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
    &["servers", "power", "console", "console.ws", "stats", "host", "pairing", "files", "files.upload", "files.zip", "files.trash", "files.text", "mods.scan", "servers.infos", "configs", "crash", "neoforge", "pack.update", "jobs", "properties", "jvm", "players", "backups", "schedule", "create", "tls", "2fa", "tokens", "audit", "mods", "files.history", "files.search", "webhooks", "users", "mcp", "sftp"];

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
pub(crate) fn token(headers: &HeaderMap) -> (String, bool) {
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

pub(crate) fn require_owner(app: &App, headers: &HeaderMap, method: &Method) -> ApiResult<()> {
    match require(app, headers, method)? {
        Who::Owner => Ok(()),
        _ => Err(ApiError(StatusCode::FORBIDDEN, "Only the owner can do that.".into())),
    }
}

/// The session cookie; `Secure` over HTTPS, so it's never sent in the clear.
fn session_cookie(token: &str, secure: bool) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}{}",
        30 * 24 * 3600,
        if secure { "; Secure" } else { "" }
    ))
    .expect("cookie header")
}

fn secure(t: &Option<axum::Extension<crate::net::Transport>>) -> bool {
    t.as_ref().is_some_and(|t| t.tls)
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/api/v1/hello", get(hello))
        .route("/api/v1/auth/setup", post(setup))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/me", get(me))
        .route("/api/v1/auth/2fa", get(two_factor_status))
        .route("/api/v1/auth/2fa/begin", post(two_factor_begin))
        .route("/api/v1/auth/2fa/enable", post(two_factor_enable))
        .route("/api/v1/auth/2fa/disable", post(two_factor_disable))
        .route("/api/v1/auth/password", post(change_password))
        .route("/api/v1/users", get(users).post(user_create))
        .route("/api/v1/users/{id}", axum::routing::put(user_update).delete(user_delete))
        .route("/api/v1/users/{id}/reset-2fa", post(user_reset_2fa))
        .route("/api/v1/tokens", get(tokens).post(token_create))
        .route("/api/v1/tokens/{id}", axum::routing::delete(token_revoke))
        .route("/api/v1/audit", get(audit))
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
        .route("/api/v1/servers/{id}/icon", get(icon))
        .route("/api/v1/ws", get(ws))
        .merge(crate::fs_api::routes())
        .merge(crate::pack_api::routes())
        .merge(crate::manage_api::routes())
        .merge(crate::mods_api::routes())
        .merge(crate::webhooks::routes())
        .merge(crate::mcp::routes())
        .merge(crate::router::routes())
        .merge(crate::mods_more::routes())
        .merge(crate::mods_table::routes())
        .merge(crate::fs_zip::routes())
        .fallback(crate::ui::serve)
        .layer(axum::middleware::from_fn_with_state(app.clone(), crate::security::guard))
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
        "tls": { "fingerprint": app.tls.fingerprint, "ca": app.tls.ca_pem, "domain": cfg.acme_domain },
        "sftp": (cfg.sftp_port != 0).then(|| json!({ "port": cfg.sftp_port, "fingerprint": app.sftp_fingerprint })),
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
    t: Option<axum::Extension<crate::net::Transport>>,
    Json(b): Json<SetupBody>,
) -> ApiResult<Response> {
    if !crate::net::Net::of(addr.ip()).private() {
        return Err(ApiError(StatusCode::FORBIDDEN, "Set the panel up from its own LAN first.".into()));
    }
    let token = app.auth.setup(&b.code, &b.password, addr.ip()).map_err(bad)?;
    let _ = std::fs::remove_file(app.data_dir.join(crate::SETUP_FILE));
    log::info!("owner set up from {}", addr.ip());
    let mut r = ([(header::SET_COOKIE, session_cookie(&token, secure(&t)))], Json(json!({ "ok": true }))).into_response();
    r.extensions_mut().insert(crate::security::SignedIn("owner".into()));
    Ok(r)
}

#[derive(Deserialize)]
struct LoginBody {
    #[serde(default)]
    name: String,
    password: String,
    #[serde(default)]
    code: Option<String>,
}

async fn login(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    t: Option<axum::Extension<crate::net::Transport>>,
    Json(b): Json<LoginBody>,
) -> Response {
    use crate::auth::LoginError;
    let private = crate::net::Net::of(addr.ip()).private();
    match app.auth.login_as(&b.name, &b.password, b.code.as_deref(), addr.ip(), private) {
        Ok(token) => {
            let who = app.auth.check(&token).map_or_else(|| "owner".into(), |w| w.name());
            let mut r = ([(header::SET_COOKIE, session_cookie(&token, secure(&t)))], Json(json!({ "ok": true }))).into_response();
            r.extensions_mut().insert(crate::security::SignedIn(who));
            r
        }
        Err(e) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": e.to_string(), "need_code": e == LoginError::NeedCode, "need_two_factor": e == LoginError::NeedTwoFactor })),
        )
            .into_response(),
    }
}

// ---- two-factor sign-in and passwords (your own), tokens, audit (owner) -----------

/// The signed-in person (owner or user), for their own password and two-factor.
fn account(app: &App, headers: &HeaderMap, method: &Method) -> ApiResult<Who> {
    let who = require(app, headers, method)?;
    if who.account().is_none() {
        return Err(ApiError(StatusCode::FORBIDDEN, "Only people signed in with a password can do that.".into()));
    }
    Ok(who)
}

async fn two_factor_status(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    let who = account(&app, &headers, &method)?;
    Ok(Json(json!({ "on": app.auth.two_factor_on(who.account().flatten()) })))
}

async fn two_factor_begin(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    let who = account(&app, &headers, &method)?;
    let panel = app.cfg.read().unwrap().name.clone();
    let label = match &who {
        Who::User { name, .. } => format!("{name}-{panel}"),
        _ => panel,
    };
    let (secret, uri) = app.auth.two_factor_begin(who.account().flatten(), &label);
    let qr = qrcode::QrCode::new(uri.as_bytes())
        .map(|c| c.render::<qrcode::render::svg::Color>().min_dimensions(200, 200).quiet_zone(true).build())
        .unwrap_or_default();
    Ok(Json(json!({ "secret": secret, "uri": uri, "qr_svg": qr })))
}

#[derive(Deserialize)]
struct CodeBody {
    code: String,
    #[serde(default)]
    password: String,
}

async fn two_factor_enable(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<CodeBody>) -> ApiResult<Json<Value>> {
    let who = account(&app, &headers, &method)?;
    app.auth.two_factor_enable(who.account().flatten(), &b.code).map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

async fn two_factor_disable(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(b): Json<CodeBody>,
) -> ApiResult<Json<Value>> {
    let who = account(&app, &headers, &method)?;
    app.auth.two_factor_disable(who.account().flatten(), &b.password, &b.code, addr.ip()).map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct PasswordBody {
    current: String,
    new: String,
}

async fn change_password(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(b): Json<PasswordBody>,
) -> ApiResult<Json<Value>> {
    let who = account(&app, &headers, &method)?;
    app.auth.change_password(who.account().flatten(), &b.current, &b.new, addr.ip()).map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

// ---- users (owner only) -----------------------------------------------------------

fn user_json(u: &crate::auth::User) -> Value {
    json!({ "id": u.id, "name": u.name, "role": u.role, "servers": u.servers, "created": u.created, "last_seen": u.last_seen, "two_factor": u.two_factor() })
}

async fn users(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    Ok(Json(json!(app.auth.users().iter().map(user_json).collect::<Vec<_>>())))
}

#[derive(Deserialize)]
struct UserBody {
    name: String,
    #[serde(default)]
    password: String,
    role: String,
    #[serde(default)]
    servers: Vec<String>,
}

fn role_of(s: &str) -> ApiResult<crate::auth::Role> {
    crate::auth::Role::parse(s).ok_or_else(|| bad("The role is admin, operator or viewer."))
}

async fn user_create(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<UserBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let u = app.auth.user_create(&b.name, &b.password, role_of(&b.role)?, b.servers).map_err(bad)?;
    Ok(Json(user_json(&u)))
}

async fn user_update(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<UserBody>,
) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    app.auth.user_update(&id, &b.name, role_of(&b.role)?, b.servers, Some(b.password.as_str())).map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

async fn user_delete(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    if !app.auth.user_delete(&id) {
        return Err(not_found("user"));
    }
    Ok(Json(json!({ "ok": true })))
}

async fn user_reset_2fa(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    if !app.auth.user_reset_2fa(&id) {
        return Err(not_found("user"));
    }
    Ok(Json(json!({ "ok": true })))
}

async fn tokens(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    Ok(Json(json!(app.auth.tokens())))
}

#[derive(Deserialize)]
struct TokenBody {
    name: String,
    #[serde(default)]
    read_only: bool,
    #[serde(default)]
    servers: Vec<String>,
    days: Option<u64>,
}

async fn token_create(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<TokenBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let (token, rec) = app.auth.token_create(&b.name, b.read_only, b.servers, b.days).map_err(bad)?;
    Ok(Json(json!({ "token": token, "info": rec })))
}

async fn token_revoke(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    if !app.auth.token_revoke(&id) {
        return Err(not_found("token"));
    }
    Ok(Json(json!({ "ok": true })))
}

async fn audit(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let limit = q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(200).min(2000);
    Ok(Json(json!(app.audit.recent(limit))))
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
        Who::Token { id, name, read_only, servers } => json!({ "role": "token", "id": id, "name": name, "read_only": read_only, "servers": servers }),
        Who::User { id, name, role, servers } => json!({ "role": role, "user": true, "id": id, "name": name, "servers": servers, "min_password": MIN_PASSWORD }),
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
    let (id, secret, code) = app.auth.pair_request(&b.device, addr.ip(), &app.tls.fingerprint).map_err(bad)?;
    log::info!("pairing request from {} ({}): code {code}", b.device.trim(), addr.ip());
    // The code isn't sent: the device works it out from the certificate it
    // sees, so a machine in the middle can't show the right one.
    Ok(Json(json!({ "id": id, "secret": secret })))
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
    let who = require(&app, &headers, &method)?;
    let snap = app.snapshot.borrow().clone();
    Ok(Json(json!(snap.servers.iter().filter(|s| who.sees(&s.id)).collect::<Vec<_>>())))
}

/// Every server as Hopper desktop describes one: folder, manager, loader,
/// Java, memory and server.properties.
async fn infos(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    let who = require(&app, &headers, &method)?;
    let a = app.clone();
    let list = tokio::task::spawn_blocking(move || {
        a.servers
            .list()
            .iter()
            .filter(|e| who.sees(&e.id))
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
    w.name()
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

/// The server's `server-icon.png` (what the multiplayer list shows).
async fn icon(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Response> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let png = tokio::fs::read(e.root.join("server-icon.png")).await.map_err(|_| not_found("icon"))?;
    // The URL carries the file's time, so it can be kept for long.
    Ok(([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "private, max-age=604800")], png).into_response())
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
fn ws_allowed(app: &App, headers: &HeaderMap) -> Option<Who> {
    let (tok, from_cookie) = token(headers);
    let who = app.auth.check(&tok)?;
    if !from_cookie {
        return Some(who);
    }
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()).unwrap_or("");
    let origin_host = origin.split("://").nth(1).unwrap_or("");
    (!host.is_empty() && origin_host == host).then_some(who)
}

async fn ws(State(app): State<Arc<App>>, headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    let Some(who) = ws_allowed(&app, &headers) else {
        return ApiError(StatusCode::UNAUTHORIZED, "Sign in first.".into()).into_response();
    };
    upgrade.on_upgrade(move |socket| live(app, who, socket))
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
async fn live(app: Arc<App>, who: Who, mut socket: WebSocket) {
    let mut snap_rx = app.snapshot.subscribe();
    let mut follows: HashMap<String, u64> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_millis(150));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let first = snap_rx.borrow_and_update().clone();
    if send_snapshot(&mut socket, &first, &who).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            changed = snap_rx.changed() => {
                if changed.is_err() { break; }
                let snap = snap_rx.borrow_and_update().clone();
                if send_snapshot(&mut socket, &snap, &who).await.is_err() { break; }
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
                    Ok(ClientMsg::Console { server }) if who.sees(&server) => { follows.insert(server, u64::MAX); }
                    Ok(ClientMsg::Console { .. }) => {}
                    Ok(ClientMsg::Unconsole { server }) => { follows.remove(&server); }
                    Err(e) => log::debug!("ws: {e}"),
                }
            }
        }
    }
}

async fn send_snapshot(socket: &mut WebSocket, snap: &Snapshot, who: &Who) -> Result<(), axum::Error> {
    let servers: Vec<&Summary> = snap.servers.iter().filter(|s| who.sees(&s.id)).collect();
    let msg = json!({ "t": "snapshot", "servers": servers, "host": snap.host });
    socket.send(Message::Text(msg.to_string().into())).await
}
