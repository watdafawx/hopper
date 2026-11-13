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
    &["servers", "power", "console", "console.ws", "stats", "host", "pairing", "files", "files.upload", "files.zip", "files.trash", "files.text", "mods.scan", "servers.infos", "configs", "crash", "neoforge", "pack.update", "jobs", "properties", "jvm", "jvm.preview", "players", "backups", "schedule", "create", "tls", "2fa", "tokens", "audit", "mods", "files.history", "files.search", "webhooks", "rules", "users", "mcp", "sftp", "mods.profiles", "browse.install", "pack.check", "configs.batch", "configs.sync", "mods.records", "web.origins", "mods.staged", "files.fetch", "web.remote"];

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
        .check_from(&tok, origin_of(headers))
        .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "Sign in first.".into()))?;
    if from_cookie && method != Method::GET && headers.get("x-hopperd").is_none() {
        return Err(ApiError(StatusCode::FORBIDDEN, "Missing X-Hopperd header.".into()));
    }
    Ok(who)
}

/// The request's `Origin`, when it sent one.
pub(crate) fn origin_of(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()).filter(|o| !o.is_empty())
}

/// The owner or a manager: panel-wide settings, users, devices and the
/// rest. The few things only the true owner may do check `Who::may_*`.
pub(crate) fn require_owner(app: &App, headers: &HeaderMap, method: &Method) -> ApiResult<()> {
    require_owner_who(app, headers, method).map(|_| ())
}

pub(crate) fn require_owner_who(app: &App, headers: &HeaderMap, method: &Method) -> ApiResult<Who> {
    let who = require(app, headers, method)?;
    if who.owner_level() {
        Ok(who)
    } else {
        let msg = if matches!(who, Who::Device { .. }) {
            "This PC can't do that. In the panel, open Settings › Access & devices and allow this PC to manage it, or do it there."
        } else {
            "Only the owner or a manager can do that."
        };
        Err(ApiError(StatusCode::FORBIDDEN, msg.into()))
    }
}

fn only_owner(msg: &str) -> ApiError {
    ApiError(StatusCode::FORBIDDEN, msg.into())
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
        .route("/api/v1/auth/sessions", get(sessions))
        .route("/api/v1/auth/sessions/{id}", axum::routing::delete(session_revoke))
        .route("/api/v1/ws/ticket", post(ws_ticket))
        .route("/api/v1/link", get(crate::fs_api::redeem_link))
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
        .route("/api/v1/devices/me", axum::routing::delete(unpair_self))
        .route("/api/v1/devices/{id}/manage", axum::routing::put(device_manage))
        .route("/api/v1/devices/{id}", axum::routing::delete(forget_device))
        .route("/api/v1/host", get(host))
        .route("/api/v1/servers", get(servers))
        .route("/api/v1/servers/rescan", post(rescan))
        .route("/api/v1/servers/infos", get(infos))
        .route("/api/v1/servers/{id}/mods", get(mods))
        .route("/api/v1/servers/{id}/crash", get(crash))
        .route("/api/v1/servers/{id}/crash/put-back", post(crash_put_back))
        .route("/api/v1/servers/{id}/crash/switch-off", post(crash_switch_off))
        .route("/api/v1/servers/{id}/crash/switch-on", post(crash_switch_on))
        .route("/api/v1/servers/{id}/stuck/snooze", post(stuck_snooze))
        .route("/api/v1/servers/{id}/neoforge", get(neoforge))
        .route("/api/v1/servers/{id}", get(server))
        .route("/api/v1/servers/{id}/power", post(power))
        .route("/api/v1/servers/{id}/command", post(command))
        .route("/api/v1/servers/{id}/console", get(console))
        .route("/api/v1/servers/{id}/icon", get(icon))
        .route("/api/v1/ws", get(ws))
        .merge(crate::changes::routes())
        .merge(crate::fs_api::routes())
        .merge(crate::pack_api::routes())
        .merge(crate::manage_api::routes())
        .merge(crate::mods_api::routes())
        .merge(crate::mods_upload::routes())
        .merge(crate::sources_api::routes())
        .merge(crate::webhooks::routes())
        .merge(crate::mcp::routes())
        .merge(crate::router::routes())
        .merge(crate::mods_more::routes())
        .merge(crate::mods_table::routes())
        .merge(crate::servers_admin::routes())
        .merge(crate::server_ops::routes())
        .merge(crate::browse_api::routes())
        .merge(crate::home::routes())
        .merge(crate::mods_hunt::routes())
        .merge(crate::fs_zip::routes())
        .merge(crate::control::routes())
        .merge(crate::web_panels::routes())
        .merge(crate::worlds_api::routes())
        .merge(crate::packs_where_api::routes())
        .merge(crate::autoupdate::routes())
        .merge(crate::parity_overview::routes())
        .merge(crate::parity_mods::routes())
        .merge(crate::parity_files::routes())
        .merge(crate::configs::routes())
        .merge(crate::config_sync::routes())
        .merge(crate::parity_worlds::routes())
        .merge(crate::parity_console::routes())
        .merge(crate::parity_browse::routes())
        .merge(crate::acme::routes())
        .merge(crate::web_access::routes())
        .fallback(crate::ui::serve)
        .layer(axum::middleware::from_fn_with_state(app.clone(), crate::security::guard))
        .merge(crate::acme::challenge_route())
        .with_state(app)
}

// ---- hello and auth ----------------------------------------------------

async fn hello(State(app): State<Arc<App>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Json<Value> {
    let cfg = app.cfg.read().unwrap();
    let setup_needed = app.auth.needs_setup();
    Json(json!({
        "panel": "hopperd",
        "name": cfg.name,
        "version": env!("CARGO_PKG_VERSION"),
        "api": API_VERSION,
        // The web UI this exe serves and this run of the panel: an open page
        // compares them to its own to know it should reload.
        "build": crate::ui::build_id(),
        "boot": crate::ui::boot_id(),
        "setup_needed": setup_needed,
        // How this browser reaches the panel (local, lan, tailscale, internet).
        "via": crate::net::Net::client(addr.ip(), &headers),
        // A browser on this PC sets the owner password without the code.
        "local_setup": setup_needed && crate::auth::trusted_local(cfg.trust_local_setup, addr.ip(), &headers),
        "addresses": *app.addresses.read().unwrap(),
        "port": cfg.port,
        "capabilities": CAPABILITIES,
        // `trusted`: the address with a Let's Encrypt certificate, when there is one.
        "tls": { "fingerprint": app.tls.fingerprint, "ca": app.tls.ca_pem, "domain": cfg.acme_domain, "trusted": app.acme.trusted_url() },
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
    headers: HeaderMap,
    Json(b): Json<SetupBody>,
) -> ApiResult<Response> {
    if !crate::net::Net::of(addr.ip()).private() {
        return Err(ApiError(StatusCode::FORBIDDEN, "Set the panel up from its own LAN first.".into()));
    }
    let local = crate::auth::trusted_local(app.cfg.read().unwrap().trust_local_setup, addr.ip(), &headers);
    let token = app.auth.setup_as(&b.code, &b.password, addr.ip(), local).map_err(bad)?;
    let _ = std::fs::remove_file(app.data_dir.join(crate::SETUP_FILE));
    log::info!("owner set up from {}{}", addr.ip(), if local && b.code.trim().is_empty() { " (this PC, no setup code)" } else { "" });
    let mut r = ([(header::SET_COOKIE, session_cookie(&token, secure(&t)))], Json(json!({ "ok": true }))).into_response();
    r.extensions_mut().insert(crate::security::SignedIn("owner".into(), "owner".into()));
    Ok(r)
}

#[derive(Deserialize)]
struct LoginBody {
    #[serde(default)]
    name: String,
    password: String,
    #[serde(default)]
    code: Option<String>,
    /// A page on another origin signing in (B1): the token comes back in
    /// the body instead of a cookie, for that page only.
    #[serde(default)]
    bearer: bool,
}

async fn login(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    t: Option<axum::Extension<crate::net::Transport>>,
    headers: HeaderMap,
    Json(b): Json<LoginBody>,
) -> Response {
    use crate::auth::LoginError;
    let private = crate::net::Net::of(addr.ip()).private();
    // A bearer session only for a page the owner listed, never for the
    // panel's own page (it has its cookie) or a client without `Origin`
    // (scripts use API tokens). Checked before the password, so a refused
    // page learns nothing about it and uses up no try.
    let remote = if b.bearer {
        let list = app.cfg.read().unwrap().web_origins.clone();
        match crate::web_access::listed_other(&headers, &list) {
            Some(o) => Some((o.to_string(), crate::auth::agent_label(headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("")))),
            None => {
                let mut r = ApiError(StatusCode::FORBIDDEN, "This page can't sign in to this panel. Its owner can add it in Settings › Access & devices.".into()).into_response();
                // Refused before the password, but still worth a line: who
                // is trying to sign in from where. Only the (capped) origin.
                r.extensions_mut().insert(crate::security::AuditNote(refused_origin_note(origin_of(&headers))));
                return r;
            }
        }
    } else {
        None
    };
    let page = remote.as_ref().map(|(o, _)| o.as_str());
    let mut r = match app.auth.login_with(&b.name, &b.password, b.code.as_deref(), addr.ip(), private, remote.as_ref().map(|(o, a)| (o.as_str(), a.as_str()))) {
        Ok(token) => {
            let (who, role) = app.auth.check_from(&token, page).map_or_else(|| ("owner".into(), "owner"), |w| (w.name(), w.role_name()));
            let mut r = if remote.is_some() {
                Json(json!({ "ok": true, "token": token, "expires_in": crate::auth::REMOTE_IDLE })).into_response()
            } else {
                ([(header::SET_COOKIE, session_cookie(&token, secure(&t)))], Json(json!({ "ok": true }))).into_response()
            };
            r.extensions_mut().insert(crate::security::SignedIn(who, role.into()));
            r
        }
        Err(e) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": e.to_string(), "need_code": e == LoginError::NeedCode, "need_two_factor": e == LoginError::NeedTwoFactor })),
        )
            .into_response(),
    };
    // The audit log names the page a sign-in came from, right or wrong.
    if let Some(o) = page {
        r.extensions_mut().insert(crate::security::AuditNote(format!("from {o}")));
    }
    r
}

/// "from <origin>" for the audit log line of a refused bearer sign-in. The
/// header is whatever the client sent: keep the start, printable characters
/// only, so it can't flood or forge the log.
fn refused_origin_note(origin: Option<&str>) -> String {
    match origin {
        Some(o) => {
            let shown: String = o.chars().filter(|c| !c.is_control()).take(100).collect();
            format!("from {shown}{}", if o.chars().count() > 100 { "…" } else { "" })
        }
        None => "from no Origin".into(),
    }
}

// ---- sessions (your own; every one for the owner and managers) ------------------------

async fn sessions(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    let who = account(&app, &headers, &method)?;
    let current = crate::auth::Auth::session_id(&token(&headers).0);
    let users = app.auth.users();
    let names: HashMap<String, String> = users.iter().map(|u| (u.id.clone(), u.name.clone())).collect();
    let me = who.account().flatten();
    let list: Vec<Value> = app
        .auth
        .sessions(who.owner_level(), me)
        .into_iter()
        // A manager sees (and can end) their own and the users' sessions, not
        // the owner's or another manager's: the same rule as ending one, so
        // their IP, page and browser stay private.
        .filter(|s| match s.user.as_deref() {
            None => who.is_owner(),
            Some(uid) => me == Some(uid) || who.is_owner() || !users.iter().any(|u| u.id == uid && u.role == crate::auth::Role::Manager),
        })
        .map(|s| {
            let mut v = serde_json::to_value(&s).unwrap_or_default();
            v["current"] = json!(s.id == current);
            v["name"] = json!(s.user.as_ref().map_or_else(|| "owner".to_string(), |u| names.get(u).cloned().unwrap_or_default()));
            v
        })
        .collect();
    Ok(Json(json!(list)))
}

/// Ends a session: your own, or (owner and managers) someone else's. A
/// manager can't end the owner's or another manager's sessions.
async fn session_revoke(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let who = account(&app, &headers, &method)?;
    let whose = app.auth.session_owner(&id).ok_or_else(|| not_found("session"))?;
    let mine = who.account().flatten().map(str::to_string) == whose;
    let allowed = mine
        || match &whose {
            None => who.is_owner(),
            Some(uid) => {
                let role = app.auth.users().into_iter().find(|u| &u.id == uid).map(|u| u.role);
                who.owner_level() && who.may_manage_user(role, role)
            }
        };
    if !allowed {
        return Err(only_owner("Only the owner can end this session."));
    }
    app.auth.session_revoke(&id).ok_or_else(|| not_found("session"))?;
    Ok(Json(json!({ "ok": true })))
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
    // Tokens other pages held may have been copied: they end with the old password.
    app.auth.end_browser_sessions(who.account().flatten(), &token(&headers).0);
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
    crate::auth::Role::parse(s).ok_or_else(|| bad("The role is manager, admin, operator or viewer."))
}

/// A user's role now, and whether `who` may change that user to `after`.
fn check_manage(app: &App, who: &Who, id: &str, after: Option<crate::auth::Role>) -> ApiResult<()> {
    let before = app.auth.users().into_iter().find(|u| u.id == id).ok_or_else(|| not_found("user"))?.role;
    if who.may_manage_user(Some(before), after) {
        Ok(())
    } else {
        Err(only_owner("Only the owner can change or remove managers."))
    }
}

async fn user_create(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<UserBody>) -> ApiResult<Json<Value>> {
    let who = require_owner_who(&app, &headers, &method)?;
    let role = role_of(&b.role)?;
    if !who.may_manage_user(None, Some(role)) {
        return Err(only_owner("Only the owner can add managers."));
    }
    let u = app.auth.user_create(&b.name, &b.password, role, b.servers).map_err(bad)?;
    Ok(Json(user_json(&u)))
}

async fn user_update(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<UserBody>,
) -> ApiResult<Json<Value>> {
    let who = require_owner_who(&app, &headers, &method)?;
    let role = role_of(&b.role)?;
    check_manage(&app, &who, &id, Some(role))?;
    app.auth.user_update(&id, &b.name, role, b.servers, Some(b.password.as_str())).map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

async fn user_delete(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let who = require_owner_who(&app, &headers, &method)?;
    check_manage(&app, &who, &id, None)?;
    if !app.auth.user_delete(&id) {
        return Err(not_found("user"));
    }
    Ok(Json(json!({ "ok": true })))
}

async fn user_reset_2fa(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let who = require_owner_who(&app, &headers, &method)?;
    let role = app.auth.users().into_iter().find(|u| u.id == id).map(|u| u.role);
    check_manage(&app, &who, &id, role)?;
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
        Who::Owner => {
            let c = app.cfg.read().unwrap();
            json!({ "role": "owner", "min_password": MIN_PASSWORD, "panel_folder": c.home, "servers_folder": c.servers_root })
        }
        Who::Device { id, name, manage } => json!({ "role": "device", "id": id, "name": name, "manage": manage }),
        Who::Token { id, name, read_only, servers } => json!({ "role": "token", "id": id, "name": name, "read_only": read_only, "servers": servers }),
        Who::User { id, name, role: crate::auth::Role::Manager, servers } => json!({
            "role": "manager", "user": true, "id": id, "name": name, "servers": servers,
            "min_password": MIN_PASSWORD, "panel_folder": app.cfg.read().unwrap().home,
            "servers_folder": app.cfg.read().unwrap().servers_root,
        }),
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
    /// Let the new device manage the panel. Only the true owner may ask.
    #[serde(default)]
    manage: bool,
}

async fn pair_answer(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<AnswerBody>,
) -> ApiResult<Response> {
    let who = require_owner_who(&app, &headers, &method)?;
    // A paired PC that may manage the panel still can't let other devices in.
    if matches!(who, Who::Device { .. }) {
        return Err(only_owner("Only the owner or a manager can allow a PC."));
    }
    let manage = b.allow && b.manage;
    if manage && !who.is_owner() {
        return Err(only_owner(MANAGE_OWNER_ONLY));
    }
    app.auth.answer(&id, b.allow, who.account().flatten(), manage).map_err(bad)?;
    let mut res = Json(json!({ "ok": true })).into_response();
    if manage {
        res.extensions_mut().insert(crate::security::AuditNote("allowed to manage the panel".into()));
    }
    Ok(res)
}

const MANAGE_OWNER_ONLY: &str = "Only the owner can let a PC manage the panel.";

#[derive(Deserialize)]
struct ManageBody {
    manage: bool,
}

/// The owner lets a paired PC manage the panel, or takes that away again.
/// Only the true owner: not a manager, and not a device (not even one that
/// may manage), so a device can never widen its own or another's rights.
async fn device_manage(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<ManageBody>,
) -> ApiResult<Response> {
    let who = require(&app, &headers, &method)?;
    if !who.is_owner() {
        return Err(only_owner(MANAGE_OWNER_ONLY));
    }
    if !app.auth.set_device_manage(&id, b.manage) {
        return Err(not_found("device"));
    }
    let mut res = Json(json!({ "ok": true })).into_response();
    let note = if b.manage { "allowed to manage the panel" } else { "no longer allowed to manage the panel" };
    res.extensions_mut().insert(crate::security::AuditNote(note.into()));
    Ok(res)
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
    let who = require_owner_who(&app, &headers, &method)?;
    let by = app.auth.devices().into_iter().find(|d| d.id == id).ok_or_else(|| not_found("device"))?.by;
    if !who.may_forget_device(by.as_deref()) {
        return Err(only_owner("Only the owner can remove the owner's devices."));
    }
    if !app.auth.forget_device(&id) {
        return Err(not_found("device"));
    }
    Ok(Json(json!({ "ok": true })))
}

/// A paired PC unpairs itself (Remove in Hopper's settings). Only a
/// device's own token can do this, and it only removes that device.
async fn unpair_self(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    let Who::Device { id, .. } = require(&app, &headers, &method)? else {
        return Err(bad("Only a paired device can unpair itself."));
    };
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
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let dir = e.root.join("mods");
    // `off=1`: the jars turned off too (marked `off`), for Hopper's Sync.
    let off = q.get("off").is_some_and(|v| v == "1");
    let mods = tokio::task::spawn_blocking(move || {
        // Sync's request: one line in this panel's log with where the time
        // went (counts and times only), to set beside the desktop's line.
        let timed = off;
        if timed {
            hopper_core::synctiming::begin();
        }
        let t = std::time::Instant::now();
        let r = if !dir.is_dir() {
            Ok(Vec::new())
        } else if off {
            hopper_core::rigsync::mods_with_off(&dir)
        } else {
            hopper_core::scan::scan_folder(&dir)
        };
        if timed {
            let n = r.as_ref().map_or(0, Vec::len);
            if let Some(line) = hopper_core::synctiming::finish() {
                log::info!("{} (server side, {n} jars, {} ms)", line.replacen("sync compare", "sync mods read", 1), t.elapsed().as_millis());
            }
        }
        r
    })
    .await
    .map_err(bad)?
    .map_err(|e| bad(format!("{e:#}")))?;
    Ok(Json(json!({ "mods": mods })))
}

/// The server's last crash, read like Hopper reads a game's: the mods it
/// names with how sure, what happened in order, and what changed lately.
/// `report` is the crash file inside the server folder, to open or share.
async fn crash(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let root = e.root.clone();
    // The last update check: an "Update X" step shows only while a newer X is out.
    let ups: Option<std::collections::HashSet<u64>> = crate::mods_api::cached_updates(&id)
        .map(|v| v.iter().filter_map(|u| u["project_id"].as_str()?.parse().ok()).collect());
    let c = tokio::task::spawn_blocking(move || {
        let mut c = hopper_core::crashdetect::server(&root)?;
        let inst = hopper_core::instances::load(&root);
        let mods = root.join("mods");
        for s in &mut c.suspects {
            let pid = inst
                .as_ref()
                .and_then(|i| i.addons.iter().find(|a| a.file_name.eq_ignore_ascii_case(s.file_name.trim_end_matches(".disabled"))))
                .map_or(0, |a| a.project_id);
            let newer = ups.as_ref().and_then(|u| (pid != 0).then(|| u.contains(&pid)));
            s.steps = hopper_core::crashdetect::steps_shown(s, &mods, newer);
        }
        Some(c)
    })
    .await
    .map_err(bad)?;
    let report = c
        .as_ref()
        .and_then(|c| c.source.strip_prefix(&e.root).ok())
        .map(|p| p.to_string_lossy().replace('\\', "/"));
    // Java kept running after the crash until the watchdog ended it.
    let lingered = c.as_ref().and_then(|c| c.lingered_note());
    // What Hopper changed for each setting turned off since: the card's diff.
    let changes: Vec<Vec<(bool, String)>> =
        c.as_ref().map(|c| c.switched_off.iter().map(|s| hopper_core::configswitch::applied_diff(&e.root, s)).collect()).unwrap_or_default();
    // What each mod the crash says is missing is now (installed since, the game or its loader, still missing).
    let needs = c.as_ref().map(|c| hopper_core::crashdetect::need_statuses(&e.root.join("mods"), c)).unwrap_or_default();
    Ok(Json(json!({ "crash": c, "report": report, "lingered": lingered, "changes": changes, "needs": needs })))
}

#[derive(serde::Deserialize)]
struct SwitchBody {
    file: String,
    key: String,
}

/// Changes the setting the last crash points at (an item's `fe_seed = true`
/// → false, a mixin option, a mod's support for another mod, a limit),
/// keeping the mod. The config as it was goes to `.hopper-backups` first.
/// Read again here, so only a setting this crash names can be changed this
/// way. `{ ok, diff }`: the lines that changed.
async fn crash_switch_off(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<SwitchBody>,
) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    if e.server.state().is_alive() {
        return Err(bad("Stop the server first: it may write its config back when it stops."));
    }
    let root = e.root.clone();
    let what = format!("{} in {}", b.key, b.file);
    let diff = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<(bool, String)>> {
        let c = hopper_core::crashdetect::server(&root).ok_or_else(|| anyhow::anyhow!("The server's last start didn't crash."))?;
        let sw = c
            .switches
            .iter()
            .find(|s| s.file == b.file && s.key == b.key)
            .ok_or_else(|| anyhow::anyhow!("The crash doesn't name {} in {}.", b.key, b.file))?;
        let (before, after) = hopper_core::configswitch::turn_off(&root, sw)?;
        Ok(hopper_core::configswitch::diff(&before, &after))
    })
    .await
    .map_err(bad)?
    .map_err(|e| bad(format!("{e:#}")))?;
    log::info!("{}: changed {what} after a crash", e.name);
    Ok(Json(json!({ "ok": true, "diff": diff })))
}

/// Undoes [`crash_switch_off`]: the setting goes back to what it was at the
/// crash (the config as it is now goes to `.hopper-backups` first). Only a
/// setting this crash names, changed since, can be put back this way.
async fn crash_switch_on(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<SwitchBody>,
) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    if e.server.state().is_alive() {
        return Err(bad("Stop the server first: it may write its config back when it stops."));
    }
    let root = e.root.clone();
    let what = format!("{} in {}", b.key, b.file);
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let c = hopper_core::crashdetect::server(&root).ok_or_else(|| anyhow::anyhow!("The server's last start didn't crash."))?;
        let sw = c
            .switched_off
            .iter()
            .find(|s| s.file == b.file && s.key == b.key)
            .ok_or_else(|| anyhow::anyhow!("{} in {} isn't changed from the crash.", b.key, b.file))?;
        hopper_core::configswitch::turn_on(&root, sw).map(|_| ())
    })
    .await
    .map_err(bad)?
    .map_err(|e| bad(format!("{e:#}")))?;
    log::info!("{}: put {what} back as it was at the crash", e.name);
    Ok(Json(json!({ "ok": true })))
}

#[derive(serde::Deserialize)]
struct PutBackBody {
    /// The jar in the mods folder now (the newer one).
    file: String,
}

/// Puts back the older copy of the mod the last crash's error names: the
/// jar there now goes to `.hopper-backups` first. Read again here, so only
/// a change this crash names can be undone this way.
/// "Wait": hides a stuck start's notice and asks again after 5 more quiet
/// minutes. The server keeps running either way.
async fn stuck_snooze(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    e.server.snooze_stuck(300);
    Ok(Json(json!({ "ok": true })))
}

async fn crash_put_back(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<PutBackBody>,
) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    if e.server.state().is_alive() {
        return Err(bad("Stop the server first."));
    }
    let root = e.root.clone();
    let back = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let c = hopper_core::crashdetect::server(&root).ok_or_else(|| anyhow::anyhow!("The server's last start didn't crash."))?;
        let ch = c
            .suspects
            .iter()
            .filter_map(|s| s.changed.as_ref())
            .find(|ch| ch.file_name == b.file)
            .ok_or_else(|| anyhow::anyhow!("The crash doesn't name a change to {}.", b.file))?;
        let back = hopper_core::crashdetect::put_back(&root, ch)?;
        let name = back.file_name().unwrap_or_default().to_string_lossy().into_owned();
        // The mod list's entry follows the jar that's back.
        if let Some(mut inst) = hopper_core::instances::load(&root)
            && let Some(a) = inst.addons.iter_mut().find(|a| a.file_name == ch.file_name)
        {
            a.file_name = name.clone();
            inst.save()?;
        }
        Ok(name)
    })
    .await
    .map_err(bad)?
    .map_err(|e| bad(format!("{e:#}")))?;
    log::info!("{}: put back {back} after a crash", e.name);
    crate::mods_api::forget_updates(&id);
    Ok(Json(json!({ "back": back })))
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
    tokio::task::spawn_blocking(move || Json(crate::router::fill_summary(a.servers.summary(&e), &mut crate::router::effective(&a)))).await.map_err(bad)
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
    // A restore is writing the server's files: starting now would load a half-written world.
    if matches!(action, PanelAction::Start | PanelAction::Restart) && crate::backups_api::is_restoring(&id) {
        return Err(ApiError(StatusCode::CONFLICT, format!("{} is being restored from a backup. Start it once that's done.", e.name)));
    }
    log::info!("{} {} by {}", b.action, e.name, who_name(&who));
    if matches!(action, PanelAction::Start | PanelAction::Restart) {
        crate::router::started_by_hand(&e);
    }
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
/// Devices: a bearer token. A page on another origin the owner listed: a
/// one-use ticket in `Sec-WebSocket-Protocol` (see [`ws_ticket`]).
pub(crate) fn ws_allowed(app: &App, headers: &HeaderMap) -> Option<Who> {
    ws_access(app, headers).map(|(who, _)| who)
}

/// What an open live socket signed in with, so it can be checked again
/// while it stays open (see [`live_still_allowed`]).
pub(crate) struct LiveCred {
    hash: String,
    origin: Option<String>,
}

/// [`ws_allowed`], with the sign-in the socket rests on.
pub(crate) fn ws_access(app: &App, headers: &HeaderMap) -> Option<(Who, LiveCred)> {
    if let Some(ticket) = crate::grants::ticket_in(headers) {
        return ws_ticket_who(app, headers, &ticket);
    }
    let (tok, from_cookie) = token(headers);
    let origin = origin_of(headers);
    let who = app.auth.check_from(&tok, origin)?;
    let cred = LiveCred { hash: crate::auth::hash_token(&tok), origin: origin.map(str::to_string) };
    if !from_cookie {
        return Some((who, cred));
    }
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()).unwrap_or("");
    let origin_host = origin.split("://").nth(1).unwrap_or("");
    (!host.is_empty() && origin_host == host).then_some((who, cred))
}

/// A ticket opens the socket only from a listed page, the same page that
/// asked for it, while the session that asked is still signed in. The
/// first try uses it up, right or wrong.
fn ws_ticket_who(app: &App, headers: &HeaderMap, ticket: &str) -> Option<(Who, LiveCred)> {
    let g = app.grants.take(ticket)?;
    let list = app.cfg.read().unwrap().web_origins.clone();
    let page = crate::web_access::listed_other(headers, &list)?;
    if g.purpose != crate::grants::Purpose::Live || g.origin.as_deref() != Some(page) {
        return None;
    }
    let who = app.auth.check_hash(&g.cred, Some(page))?;
    Some((who, LiveCred { hash: g.cred, origin: Some(page.to_string()) }))
}

/// Whether an open live socket may stay open: its session is still signed
/// in (not signed out, revoked, its page taken off the list, its user
/// removed) and still the same person with the same role and servers.
pub(crate) fn live_still_allowed(app: &App, cred: &LiveCred, who: &Who) -> bool {
    app.auth.check_hash(&cred.hash, cred.origin.as_deref()).as_ref() == Some(who)
}

/// How often an open live socket checks its sign-in again.
const LIVE_RECHECK: Duration = Duration::from_secs(5);

/// A one-use ticket for the live WebSocket, for a page on another origin
/// the owner listed (browsers can't send `Authorization` on a WebSocket).
async fn ws_ticket(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let list = app.cfg.read().unwrap().web_origins.clone();
    let Some(page) = crate::web_access::listed_other(&headers, &list) else {
        return Err(ApiError(StatusCode::FORBIDDEN, "Tickets are only for pages on another address that this panel lists.".into()));
    };
    let cred = crate::auth::hash_token(&token(&headers).0);
    let acct = app.auth.account_key(&cred).unwrap_or_else(|| cred.clone());
    let ticket = app.grants.issue(crate::grants::Purpose::Live, &cred, &acct, Some(page)).map_err(|m| ApiError(StatusCode::TOO_MANY_REQUESTS, m.into()))?;
    Ok(Json(json!({
        "ticket": ticket,
        "expires_in": crate::grants::TICKET_TTL.as_secs(),
        "protocols": [crate::grants::WS_PROTOCOL, format!("{}{ticket}", crate::grants::WS_TICKET_PREFIX)],
    })))
}

async fn ws(State(app): State<Arc<App>>, headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    let Some((who, cred)) = ws_access(&app, &headers) else {
        return ApiError(StatusCode::UNAUTHORIZED, "Sign in first.".into()).into_response();
    };
    // A browser that offered subprotocols drops the socket unless the
    // panel picks one; the ticket's own entry is never echoed back.
    let upgrade = if crate::grants::ticket_in(&headers).is_some() { upgrade.protocols([crate::grants::WS_PROTOCOL]) } else { upgrade };
    upgrade.on_upgrade(move |socket| live(app, who, cred, socket))
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
async fn live(app: Arc<App>, who: Who, cred: LiveCred, mut socket: WebSocket) {
    let mut snap_rx = app.snapshot.subscribe();
    let mut change_rx = app.changes.subscribe();
    let mut sent_rev = *change_rx.borrow_and_update();
    let mut follows: HashMap<String, u64> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_millis(150));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Signing out, Revoke or taking a page off the list ends the feed too,
    // not only the next request; a changed role reconnects with the new one.
    let mut recheck = tokio::time::interval(LIVE_RECHECK);
    recheck.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    recheck.tick().await;
    let first = snap_rx.borrow_and_update().clone();
    if send_snapshot(&mut socket, &first, &who).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            _ = recheck.tick() => {
                if !live_still_allowed(&app, &cred, &who) {
                    let _ = socket.send(Message::Close(None)).await;
                    break;
                }
            }
            changed = snap_rx.changed() => {
                if changed.is_err() { break; }
                let snap = snap_rx.borrow_and_update().clone();
                if send_snapshot(&mut socket, &snap, &who).await.is_err() { break; }
            }
            changed = change_rx.changed() => {
                if changed.is_err() { break; }
                change_rx.borrow_and_update();
                if send_changes(&mut socket, &app, &who, &mut sent_rev).await.is_err() { break; }
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

/// What changed since `sent` that this client may see (see
/// [`crate::changes`]); after a gap too long to replay, one `*` change.
async fn send_changes(socket: &mut WebSocket, app: &App, who: &Who, sent: &mut u64) -> Result<(), axum::Error> {
    let list = match app.changes.since(*sent) {
        Some(list) => list,
        None => {
            *sent = app.changes.rev();
            let msg = json!({ "t": "changed", "rev": *sent, "server": null, "areas": ["*"] });
            return socket.send(Message::Text(msg.to_string().into())).await;
        }
    };
    for c in list {
        *sent = c.rev;
        if c.server.as_deref().is_some_and(|s| !who.sees(s)) {
            continue;
        }
        let mut msg = serde_json::to_value(&c).unwrap_or_default();
        msg["t"] = json!("changed");
        socket.send(Message::Text(msg.to_string().into())).await?;
    }
    Ok(())
}

async fn send_snapshot(socket: &mut WebSocket, snap: &Snapshot, who: &Who) -> Result<(), axum::Error> {
    let servers: Vec<&Summary> = snap.servers.iter().filter(|s| who.sees(&s.id)).collect();
    let msg = json!({ "t": "snapshot", "servers": servers, "host": snap.host });
    socket.send(Message::Text(msg.to_string().into())).await
}
