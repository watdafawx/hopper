//! Server addresses: one Minecraft port (25565) for every server here.
//! Players join `survival.mc.example.com`; the router reads that from the
//! handshake and passes the connection to the server's own port. A
//! stopped server with sleep on answers "Sleeping · join to wake it" and
//! starts when someone joins; with nobody on for its set time it's
//! stopped again. Protocol and matching: hopper_core::hosting::router.

use crate::App;
use crate::api::{ApiResult, bad, not_found, require, require_owner};
use crate::servers::Entry;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post};
use axum::{Json, Router as Routes};
use hopper_core::hosting::router::{self as core, Read, RouteSettings, Standin};
use hopper_core::hosting::{Properties, ServerState};
use hopper_core::panel::PanelAction;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

/// The router's part of hopperd.toml (`[router]`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RouterConfig {
    /// Off: nothing listens, whatever addresses servers have.
    pub enabled: bool,
    /// The port players connect to.
    pub port: u16,
    /// Servers' addresses are `name.<domain>` by default ("mc.example.com").
    pub domain: String,
    /// Where players with an address no server has go: a server id, or
    /// empty for "No server at this address".
    pub unknown_server: String,
}

impl Default for RouterConfig {
    fn default() -> Self {
        RouterConfig { enabled: true, port: core::DEFAULT_PORT, domain: String::new(), unknown_server: String::new() }
    }
}

/// The address the setup check pings the public port with.
const CHECK_HOST: &str = "hopper-check.invalid";

#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    /// The port it listens on, when it does.
    pub listening: Option<u16>,
    /// Why it doesn't, when it should.
    pub error: Option<String>,
}

#[derive(Default)]
pub struct Router {
    pub status: Mutex<Status>,
    /// (server id, addresses), refreshed every few seconds and on changes.
    routes: RwLock<Vec<(String, Vec<String>)>>,
    /// When a sleeping server last had someone on (or started).
    active: Mutex<HashMap<String, Instant>>,
    /// When a server was first seen starting, for the next wake estimate.
    starting: Mutex<HashMap<String, Instant>>,
    reload: tokio::sync::Notify,
}

impl Router {
    /// Re-reads addresses and the config now.
    pub fn changed(&self) {
        self.reload.notify_one();
    }
}

/// A server's addresses and sleep setting.
pub fn settings(e: &Entry) -> RouteSettings {
    RouteSettings::load(&e.root)
}

fn refresh_routes(app: &App) {
    let routes = app
        .servers
        .list()
        .iter()
        .map(|e| (e.id.clone(), settings(e).addresses))
        .filter(|(_, a)| !a.is_empty())
        .collect();
    *app.router.routes.write().unwrap() = routes;
}

/// Runs the router for as long as the panel runs: (re)binds when the
/// config or addresses change, and puts servers to sleep.
pub fn start(app: Arc<App>) {
    tokio::spawn(async move {
        let mut current: Option<(u16, tokio::task::JoinHandle<()>)> = None;
        loop {
            let a = app.clone();
            let _ = tokio::task::spawn_blocking(move || refresh_routes(&a)).await;
            let cfg = app.cfg.read().unwrap().router.clone();
            let bind = app.cfg.read().unwrap().bind.clone();
            let wanted = (cfg.enabled && !app.router.routes.read().unwrap().is_empty()).then_some(cfg.port);
            if current.as_ref().map(|c| c.0) != wanted {
                if let Some((_, h)) = current.take() {
                    h.abort();
                }
                // (A port that couldn't be had is tried again every round.)
                let mut st = Status::default();
                if let Some(port) = wanted {
                    match TcpListener::bind((bind.as_str(), port)).await {
                        Ok(l) => {
                            log::info!("server addresses: listening for Minecraft on port {port}");
                            st.listening = Some(port);
                            current = Some((port, tokio::spawn(accept(app.clone(), l))));
                        }
                        Err(e) => {
                            let who = app.servers.list().into_iter().find(|e| port_of(e) == port).map(|e| e.name.clone());
                            st.error = Some(match who {
                                Some(n) => format!("Port {port} is {n}'s own port. Move {n} to another port so the router can have {port}."),
                                None => format!("Port {port} is in use by another program ({e})."),
                            });
                            log::warn!("server addresses: {}", st.error.as_deref().unwrap_or(""));
                        }
                    }
                }
                *app.router.status.lock().unwrap() = st;
            }
            let a = app.clone();
            let _ = tokio::task::spawn_blocking(move || sleep_and_time(&a)).await;
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                _ = app.router.reload.notified() => {}
            }
        }
    });
}

fn port_of(e: &Entry) -> u16 {
    Properties::load(&e.root.join("server.properties")).unwrap_or_default().port()
}

/// Stops servers that have been empty for their sleep time, and notes how
/// long starts take.
fn sleep_and_time(app: &Arc<App>) {
    let now = Instant::now();
    let mut events = Vec::new();
    for e in app.servers.list() {
        let state = e.server.state();
        // How long starts take.
        {
            let mut starting = app.router.starting.lock().unwrap();
            match state {
                ServerState::Starting => {
                    starting.entry(e.id.clone()).or_insert(now);
                }
                ServerState::Running => {
                    if let Some(at) = starting.remove(&e.id) {
                        let mut s = settings(&e);
                        if !s.addresses.is_empty() {
                            s.last_start_secs = Some(now.duration_since(at).as_secs() as u32);
                            let _ = s.save(&e.root);
                        }
                    }
                }
                _ => {
                    starting.remove(&e.id);
                }
            }
        }
        // Sleep.
        let s = settings(&e);
        let mut active = app.router.active.lock().unwrap();
        if s.sleep_after_minutes == 0 || state != ServerState::Running {
            active.remove(&e.id);
            continue;
        }
        if !e.server.players().is_empty() {
            active.insert(e.id.clone(), now);
            continue;
        }
        let since = *active.entry(e.id.clone()).or_insert(now);
        if now.duration_since(since) >= Duration::from_secs(s.sleep_after_minutes as u64 * 60) {
            active.remove(&e.id);
            drop(active);
            match app.servers.act(&e, PanelAction::Stop) {
                Ok(()) => events.push(event(&e, "sleep", format!("**{}** went to sleep (nobody on for {} min)", e.name, s.sleep_after_minutes))),
                Err(err) => events.push(event(&e, "problem", format!("{}: couldn't put it to sleep: {err:#}", e.name))),
            }
        }
    }
    crate::webhooks::emit(app, events);
}

fn event(e: &Entry, kind: &'static str, text: String) -> crate::webhooks::Event {
    crate::webhooks::Event { event: kind, server_id: e.id.clone(), server: e.name.clone(), text, at: crate::webhooks::now() }
}

async fn accept(app: Arc<App>, l: TcpListener) {
    loop {
        let Ok((sock, peer)) = l.accept().await else {
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        };
        let app = app.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(&app, sock, peer).await {
                log::debug!("router: {peer}: {e:#}");
            }
        });
    }
}

/// Reads from `sock` into `buf` until `parse` has a whole packet.
async fn read_until<T>(
    sock: &mut TcpStream,
    buf: &mut Vec<u8>,
    from: usize,
    parse: impl Fn(&[u8]) -> anyhow::Result<Read<T>>,
) -> anyhow::Result<(T, usize)> {
    let mut chunk = [0u8; 1024];
    loop {
        if let Read::Done(t, used) = parse(&buf[from..])? {
            return Ok((t, from + used));
        }
        if buf.len() > 8 * 1024 {
            anyhow::bail!("too much before the packet ended");
        }
        let n = tokio::time::timeout(Duration::from_secs(10), sock.read(&mut chunk)).await??;
        if n == 0 {
            anyhow::bail!("closed");
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

async fn handle(app: &Arc<App>, mut sock: TcpStream, peer: SocketAddr) -> anyhow::Result<()> {
    let _ = sock.set_nodelay(true);
    let mut buf = Vec::new();
    let (hs, used) = read_until(&mut sock, &mut buf, 0, |b| {
        if core::is_legacy_ping(b) {
            anyhow::bail!("pre-1.7 ping");
        }
        core::read_handshake(b)
    })
    .await?;

    // The setup check's own address: always answered here.
    if hs.host == CHECK_HOST {
        return standin(app, &mut sock, &buf, used, &hs, Standin::Unknown, None).await;
    }
    let entry = {
        let routes = app.router.routes.read().unwrap();
        core::pick(&hs.host, &routes).cloned()
    }
    .or_else(|| Some(app.cfg.read().unwrap().router.unknown_server.clone()).filter(|s| !s.is_empty()))
    .and_then(|id| app.servers.get(&id));

    let Some(e) = entry else {
        log::debug!("router: {peer} asked for {:?}, which no server has", hs.host);
        return standin(app, &mut sock, &buf, used, &hs, Standin::Unknown, None).await;
    };
    let state = e.server.state();
    if state == ServerState::Running {
        let port = port_of(&e);
        match tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(("127.0.0.1", port))).await {
            Ok(Ok(mut up)) => {
                let _ = up.set_nodelay(true);
                up.write_all(&buf).await?;
                tokio::io::copy_bidirectional(&mut sock, &mut up).await?;
                return Ok(());
            }
            _ => return standin(app, &mut sock, &buf, used, &hs, Standin::Starting, Some(&e)).await,
        }
    }
    let s = settings(&e);
    let kind = match state {
        ServerState::Starting => Standin::Starting,
        ServerState::Stopping => Standin::Stopping,
        ServerState::Crashed(_) => Standin::Crashed,
        _ if s.sleep_after_minutes > 0 => Standin::Sleeping,
        _ => Standin::Stopped,
    };
    standin(app, &mut sock, &buf, used, &hs, kind, Some(&e)).await
}

/// Answers for a server that isn't up (or an address no server has):
/// its server list entry, or the message a joining player sees. Joining a
/// sleeping server starts it.
async fn standin(app: &Arc<App>, sock: &mut TcpStream, buf: &[u8], used: usize, hs: &core::Handshake, kind: Standin, e: Option<&Arc<Entry>>) -> anyhow::Result<()> {
    let mut buf = buf.to_vec();
    let s = e.map(|e| settings(e)).unwrap_or_default();
    let name = e.map(|e| e.name.clone()).unwrap_or_default();
    if hs.is_login() {
        let player = read_until(sock, &mut buf, used, core::read_login_name).await.map(|(n, _)| n).unwrap_or_default();
        if kind == Standin::Sleeping
            && let Some(e) = e
        {
            match e.server.state() {
                ServerState::Stopped => {}
                _ => return kick(sock, &core::standin_kick(Standin::Starting, &name, None)).await,
            }
            match app.servers.act(e, PanelAction::Start) {
                Ok(()) => {
                    let who = if player.is_empty() { "a player".to_string() } else { player.clone() };
                    log::info!("router: {who} woke {}", e.name);
                    // Counts as busy from now: it sleeps again only after its
                    // sleep time with nobody on.
                    app.router.active.lock().unwrap().insert(e.id.clone(), Instant::now());
                    crate::webhooks::emit(app, vec![event(e, "wake", format!("**{}** woke up for {who}", e.name))]);
                    app.refresh.notify_one();
                }
                Err(err) => {
                    log::warn!("router: couldn't wake {}: {err:#}", e.name);
                    return kick(sock, &json!({ "text": format!("Couldn't start {name}: {err:#}"), "color": "red" })).await;
                }
            }
        }
        return kick(sock, &core::standin_kick(kind, &name, s.last_start_secs)).await;
    }
    // Server list: status request, then ping.
    let (_, after) = read_until(sock, &mut buf, used, |b| packet(b, 16)).await?;
    let (motd, max) = match e {
        Some(e) => {
            let p = Properties::load(&e.root.join("server.properties")).unwrap_or_default();
            (p.get("motd").unwrap_or_default().to_string(), p.get("max-players").and_then(|m| m.trim().parse().ok()).unwrap_or(20))
        }
        None => (String::new(), 0),
    };
    let icon = e.and_then(|e| core::favicon(&e.root));
    sock.write_all(&core::status_packet(&core::standin_status(kind, &motd, s.last_start_secs, max, icon.as_deref()))).await?;
    if let Ok(((1, payload), _)) = read_until(sock, &mut buf, after, |b| packet(b, 64)).await {
        sock.write_all(&core::pong_packet(&payload)).await?;
    }
    Ok(())
}

/// Any packet: (id, body).
fn packet(b: &[u8], max: usize) -> anyhow::Result<Read<(i32, Vec<u8>)>> {
    Ok(match core::read_packet(b, max)? {
        Read::Done((id, body), n) => Read::Done((id, body.to_vec()), n),
        Read::More => Read::More,
    })
}

async fn kick(sock: &mut TcpStream, text: &Value) -> anyhow::Result<()> {
    sock.write_all(&core::login_disconnect_packet(text)).await?;
    sock.flush().await?;
    Ok(())
}

/// A port for a server behind the router: free now, not the router's, not
/// another server's.
pub fn free_port(app: &App, skip: Option<&str>) -> Option<u16> {
    let router_port = app.cfg.read().unwrap().router.port;
    let used: Vec<u16> = app.servers.list().iter().filter(|e| Some(e.id.as_str()) != skip).map(|e| port_of(e)).collect();
    (25566..=25700).find(|p| *p != router_port && !used.contains(p) && hopper_core::hosting::port_free(*p))
}

// ---- API ---------------------------------------------------------------------------------

pub fn routes() -> Routes<Arc<App>> {
    Routes::new()
        .route("/api/v1/router", get(get_router).put(put_router))
        .route("/api/v1/router/check", post(check))
        .route("/api/v1/servers/{id}/address", get(get_address).put(put_address))
        .route("/api/v1/servers/{id}/address/move-port", post(move_port))
}

fn router_json(app: &App) -> Value {
    let cfg = app.cfg.read().unwrap().router.clone();
    let st = app.router.status.lock().unwrap().clone();
    let clash = app.servers.list().into_iter().find(|e| port_of(e) == cfg.port).map(|e| json!({ "id": e.id, "name": e.name }));
    json!({ "config": cfg, "status": st, "clash": clash })
}

async fn get_router(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    Ok(Json(router_json(&app)))
}

async fn put_router(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(mut b): Json<RouterConfig>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    b.domain = b.domain.trim().trim_start_matches("*.").to_string();
    if !b.domain.is_empty() {
        b.domain = core::clean_address(&b.domain).map_err(bad)?;
    }
    if b.port == 0 {
        return Err(bad("Pick a port for the router."));
    }
    if !b.unknown_server.is_empty() && app.servers.get(&b.unknown_server).is_none() {
        return Err(bad("That server isn't here."));
    }
    {
        let mut cfg = app.cfg.write().unwrap();
        cfg.router = b;
        cfg.save(&app.data_dir).map_err(|e| bad(format!("{e:#}")))?;
    }
    app.router.changed();
    // Let it bind before answering, so the reply says how it went.
    tokio::time::sleep(Duration::from_millis(300)).await;
    Ok(Json(router_json(&app)))
}

#[derive(Deserialize)]
struct AddressBody {
    addresses: Vec<String>,
    #[serde(default)]
    sleep_after_minutes: u32,
}

async fn get_address(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let s = settings(&e);
    let router_port = app.cfg.read().unwrap().router.port;
    Ok(Json(json!({
        "addresses": s.addresses, "sleep_after_minutes": s.sleep_after_minutes, "last_start_secs": s.last_start_secs,
        "port": port_of(&e), "router_port": router_port, "managed_by": e.managed_by,
    })))
}

async fn put_address(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<AddressBody>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let mut clean = Vec::new();
    for a in b.addresses.iter().filter(|a| !a.trim().is_empty()) {
        let a = core::clean_address(a).map_err(bad)?;
        if !clean.contains(&a) {
            clean.push(a);
        }
    }
    for other in app.servers.list().iter().filter(|o| o.id != e.id) {
        if let Some(a) = settings(other).addresses.iter().find(|a| clean.contains(a)) {
            return Err(bad(format!("{} already has {a}.", other.name)));
        }
    }
    if b.sleep_after_minutes > 24 * 60 {
        return Err(bad("Sleep after at most a day (1440 minutes)."));
    }
    let mut s = settings(&e);
    s.addresses = clean;
    s.sleep_after_minutes = b.sleep_after_minutes;
    s.save(&e.root).map_err(|e| bad(format!("{e:#}")))?;
    log::info!("{}: addresses {:?}, sleep {} min", e.name, s.addresses, s.sleep_after_minutes);
    app.router.changed();
    app.refresh.notify_one();
    Ok(Json(json!({ "ok": true, "addresses": s.addresses })))
}

/// Gives a server a port of its own, away from the router's.
async fn move_port(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    if e.server.state().is_alive() {
        return Err(bad(format!("Stop {} first: its port changes when it starts again.", e.name)));
    }
    let port = free_port(&app, Some(&e.id)).ok_or_else(|| bad("No free port between 25566 and 25700."))?;
    let path = e.root.join("server.properties");
    let mut p = Properties::load(&path).unwrap_or_default();
    p.set("server-port", &port.to_string());
    p.save(&path).map_err(|e| bad(format!("{e:#}")))?;
    log::info!("{}: moved to port {port}", e.name);
    app.router.changed();
    app.refresh.notify_one();
    Ok(Json(json!({ "port": port })))
}

/// Checks the setup from outside the panel's view: this network's public
/// address, what each server address points at, and whether the router's
/// port answers on the public address.
async fn check(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    let who = require(&app, &headers, &method)?;
    let cfg = app.cfg.read().unwrap().router.clone();
    let public: Option<IpAddr> = tokio::task::spawn_blocking(public_ip).await.ok().flatten();
    let lan: Vec<IpAddr> = crate::discovery::lan_addresses();
    let mut names: Vec<(String, String)> = app
        .servers
        .list()
        .iter()
        .filter(|e| who.sees(&e.id))
        .flat_map(|e| settings(e).addresses.into_iter().map(move |a| (e.name.clone(), a)))
        .collect();
    if !cfg.domain.is_empty() {
        names.insert(0, (String::new(), format!("*.{}", cfg.domain)));
    }
    let mut dns = Vec::new();
    for (server, name) in names {
        // A wildcard is checked through a name under it.
        let probe = name.strip_prefix("*.").map(|d| format!("hopper-check.{d}")).unwrap_or_else(|| name.clone());
        let ips: Vec<IpAddr> = match tokio::time::timeout(Duration::from_secs(5), tokio::net::lookup_host((probe.as_str(), 25565))).await {
            Ok(Ok(it)) => it.map(|a| a.ip()).collect(),
            _ => Vec::new(),
        };
        let here = ips.iter().any(|ip| Some(*ip) == public || lan.contains(ip) || ip.is_loopback());
        dns.push(json!({ "server": server, "name": name, "ips": ips, "ok": here,
            "note": if ips.is_empty() { "doesn't resolve yet" } else if here { "points here" } else { "points somewhere else" } }));
    }
    let listening = app.router.status.lock().unwrap().listening;
    let port = match (public, listening) {
        (Some(ip), Some(p)) => {
            let ok = tokio::time::timeout(Duration::from_secs(5), answers_as_router(ip, p)).await.is_ok_and(|r| r);
            json!({ "port": p, "reachable": ok,
                "note": if ok { "answers on your public address" } else { "no answer on your public address: forward it on your router (some routers can't test this from inside)" } })
        }
        (_, None) => json!({ "port": cfg.port, "reachable": null, "note": "the router isn't listening yet" }),
        (None, _) => json!({ "port": cfg.port, "reachable": null, "note": "couldn't find your public address" }),
    };
    Ok(Json(json!({ "public_ip": public, "lan": lan, "dns": dns, "port": port })))
}

/// Whether `ip:port` is this router: a server list ping for an address
/// nobody has gets the router's own "No server at this address" back
/// (a bare connection could be the home router answering).
async fn answers_as_router(ip: IpAddr, port: u16) -> bool {
    async fn go(ip: IpAddr, port: u16) -> anyhow::Result<bool> {
        let mut s = TcpStream::connect((ip, port)).await?;
        let mut out = core::handshake_packet(767, CHECK_HOST, port, 1);
        out.extend_from_slice(&[1, 0]); // status request
        s.write_all(&out).await?;
        let mut buf = Vec::new();
        let (reply, _) = read_until(&mut s, &mut buf, 0, |b| packet(b, 64 * 1024)).await?;
        Ok(reply.0 == 0 && String::from_utf8_lossy(&reply.1).contains("No server at this address"))
    }
    go(ip, port).await.unwrap_or(false)
}

/// This network's public IPv4 address, asked of a public service.
fn public_ip() -> Option<IpAddr> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(5))).build().into();
    let text = agent.get("https://api.ipify.org").call().ok()?.body_mut().read_to_string().ok()?;
    text.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn config_defaults() {
        let c: crate::config::Config = toml::from_str("port = 9000\n").unwrap();
        assert!(c.router.enabled);
        assert_eq!(c.router.port, 25565);
        let c: crate::config::Config = toml::from_str("[router]\ndomain = \"mc.example.com\"\n").unwrap();
        assert_eq!((c.router.domain.as_str(), c.router.port), ("mc.example.com", 25565));
    }
}
