//! The tray's line to the panel it runs (portable mode, Windows).
//!
//! The tray starts hopperd as a child with a random token in
//! `HOPPERD_CONTROL_TOKEN`. The child then writes [`Running`] to
//! [`running_file`] (the user's local app data) and answers three requests
//! from this PC that carry the token: status, stop every server, and quit.
//! Without the token (a terminal run, the service) none of this exists.

use crate::App;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use hopper_core::panel::PanelAction;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, OnceLock};

/// Exit code that tells the tray to quit too (another copy is replacing it).
pub const EXIT_TRAY_TOO: i32 = 76;
pub const HEADER: &str = "x-hopper-control";

static TOKEN: OnceLock<Option<String>> = OnceLock::new();
static EXIT: AtomicI32 = AtomicI32::new(0);

fn quit() -> &'static tokio::sync::Notify {
    static QUIT: OnceLock<tokio::sync::Notify> = OnceLock::new();
    QUIT.get_or_init(tokio::sync::Notify::new)
}

/// Takes the token from the environment (so servers hopperd starts don't
/// inherit it). Call first thing in `main`.
pub fn init() {
    let t = std::env::var("HOPPERD_CONTROL_TOKEN").ok().filter(|t| t.len() >= 32);
    if t.is_some() {
        // SAFETY: called at the start of main, before any other thread.
        unsafe { std::env::remove_var("HOPPERD_CONTROL_TOKEN") };
    }
    let _ = TOKEN.set(t);
}

fn token() -> Option<&'static str> {
    TOKEN.get().and_then(|t| t.as_deref())
}

/// Resolves when the tray asks the panel to quit.
pub async fn quitting() {
    if token().is_some() {
        quit().notified().await;
    } else {
        std::future::pending::<()>().await;
    }
}

/// The exit code the tray asked for.
pub fn exit_code() -> i32 {
    EXIT.load(Ordering::Relaxed)
}

/// A panel the tray runs, for the next copy of hopperd that's started.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Running {
    pub pid: u32,
    pub tray_pid: Option<u32>,
    pub port: u16,
    pub token: String,
    pub version: String,
    /// The tray's exe (what an update replaces).
    pub exe: PathBuf,
    /// "portable" or "startup" (Start with Windows).
    pub mode: String,
}

/// `%LOCALAPPDATA%\Hopper Server\running.json` (per service name, so a
/// test copy doesn't find the real one).
pub fn running_file() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map_or_else(std::env::temp_dir, PathBuf::from);
    let name = crate::service::name();
    base.join("Hopper Server").join(if name == "hopperd" { "running.json".to_string() } else { format!("running-{name}.json") })
}

pub fn read_running() -> Option<Running> {
    serde_json::from_str(&std::fs::read_to_string(running_file()).ok()?).ok()
}

/// Writes [`running_file`] once the panel listens.
pub fn announce(port: u16) {
    let Some(token) = token() else { return };
    let r = Running {
        pid: std::process::id(),
        tray_pid: std::env::var("HOPPERD_TRAY_PID").ok().and_then(|p| p.parse().ok()),
        port,
        token: token.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        exe: std::env::var_os("HOPPERD_TRAY_EXE").map(PathBuf::from).or_else(|| std::env::current_exe().ok()).unwrap_or_default(),
        mode: std::env::var("HOPPERD_MODE").unwrap_or_else(|_| "portable".into()),
    };
    let f = running_file();
    let _ = std::fs::create_dir_all(f.parent().unwrap());
    if let Err(e) = std::fs::write(&f, serde_json::to_string_pretty(&r).unwrap_or_default()) {
        log::warn!("writing {}: {e}", f.display());
    }
}

/// Removes [`running_file`] when it's ours.
pub fn withdraw() {
    if token().is_some() && read_running().is_some_and(|r| r.pid == std::process::id()) {
        let _ = std::fs::remove_file(running_file());
    }
}

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/local/status", get(status))
        .route("/api/v1/local/stop-servers", post(stop_servers))
        .route("/api/v1/local/quit", post(quit_now))
}

/// Compares without leaking where they differ.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Only the tray: from this PC, with the token. Anything else sees nothing.
fn check(addr: &SocketAddr, headers: &HeaderMap) -> Result<(), StatusCode> {
    let ok = token().is_some_and(|t| {
        addr.ip().is_loopback() && headers.get(HEADER).and_then(|v| v.to_str().ok()).is_some_and(|h| same(h, t))
    });
    if ok { Ok(()) } else { Err(StatusCode::NOT_FOUND) }
}

async fn status(State(app): State<Arc<App>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Result<Json<Value>, StatusCode> {
    check(&addr, &headers)?;
    let cfg = app.cfg.read().unwrap().clone();
    let up: Vec<String> = app.servers.list().iter().filter(|e| e.server.state().is_alive()).map(|e| e.name.clone()).collect();
    // For other devices on the LAN: plain HTTP works there (see net).
    let lan = crate::discovery::lan_addresses().into_iter().find(|ip| crate::net::Net::of(*ip) == crate::net::Net::Lan).map(|ip| format!("http://{ip}:{}", cfg.port));
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "port": cfg.port,
        "servers_up": up,
        "servers": app.servers.list().len(),
        "setup_code": app.auth.setup_code(),
        "lan": lan,
        "home": cfg.home,
        "data": app.data_dir,
    })))
}

async fn stop_servers(State(app): State<Arc<App>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Result<Json<Value>, StatusCode> {
    check(&addr, &headers)?;
    let up: Vec<_> = app.servers.list().into_iter().filter(|e| e.server.state().is_alive()).collect();
    let n = up.len();
    for e in up {
        let a = app.clone();
        log::info!("stopping {} (quitting from the tray)", e.name);
        tokio::task::spawn_blocking(move || {
            if let Err(err) = a.servers.act(&e, PanelAction::Stop) {
                log::warn!("stopping {}: {err:#}", e.name);
            }
        });
    }
    app.refresh.notify_one();
    Ok(Json(json!({ "ok": true, "stopping": n })))
}

#[derive(Deserialize, Default)]
struct QuitBody {
    /// Quit the tray too (another copy is taking over).
    #[serde(default)]
    tray: bool,
}

async fn quit_now(ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap, body: Option<Json<QuitBody>>) -> Result<Json<Value>, StatusCode> {
    check(&addr, &headers)?;
    let tray = body.is_some_and(|b| b.tray);
    EXIT.store(if tray { EXIT_TRAY_TOO } else { 0 }, Ordering::Relaxed);
    log::info!("quitting (asked by the tray)");
    quit().notify_one();
    Ok(Json(json!({ "ok": true })))
}
