//! Alerts: a server crashed, its TPS is low, it turned a player away for
//! mods or versions that don't match, a drive is nearly full, or the PC is
//! nearly out of memory. Each fires once (to webhooks and the panel),
//! then again only after it has been clear for ten minutes.
//!
//! TPS is read where it's cheap: over RCON every 30 seconds, with the
//! loader's own command (`neoforge tps`, `forge tps`, Paper's `tps`, spark,
//! or vanilla's `tick query`), never typed into the console. Servers with
//! RCON off show no TPS; the panel offers to turn it on. RCON is used only
//! while a server is up: not before "Done", and the connection is closed the
//! moment it starts stopping, so it can't hold the shutdown open.

use crate::App;
use crate::api::{ApiResult, bad, not_found, require, require_owner};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use hopper_core::hosting::rcon;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const FILE: &str = "alerts.json";
/// An alert that cleared fires again only after this long.
const QUIET: u64 = 600;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub crash: bool,
    pub tps_on: bool,
    pub tps_below: f32,
    pub tps_minutes: u32,
    pub disk_on: bool,
    pub disk_gb: u32,
    pub ram_on: bool,
    /// Percent of the PC's memory in use.
    pub ram_pct: u32,
    pub ram_minutes: u32,
    /// Turn RCON on for every server (new ones too) so TPS shows
    /// everywhere. Off unless the owner switches it on.
    pub auto_rcon: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { crash: true, tps_on: true, tps_below: 15.0, tps_minutes: 2, disk_on: true, disk_gb: 20, ram_on: true, ram_pct: 90, ram_minutes: 5, auto_rcon: false }
    }
}

pub fn settings(app: &App) -> Settings {
    std::fs::read(app.data_dir.join(FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

#[derive(Clone, Debug, Serialize)]
pub struct Alert {
    /// Empty for the whole panel (a drive, the PC's memory).
    pub server_id: String,
    pub server: String,
    /// crash, tps, join, disk, ram.
    pub kind: &'static str,
    pub text: String,
    pub since: u64,
    pub dismissed: bool,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Tps {
    pub tps: f32,
    pub mspt: Option<f32>,
    pub at: u64,
    /// Where it came from: "console" (spark's report in the console, free)
    /// or "rcon" (a TPS command).
    pub source: &'static str,
}

/// spark's latest TPS report in a running server's console, while it's
/// recent (spark prints one every 30 s or so when set up to).
fn console_reading(e: &crate::servers::Entry) -> Option<Tps> {
    if e.server.state() != hopper_core::hosting::process::ServerState::Running {
        return None;
    }
    let t = e.server.console_tps()?;
    let at = t.at.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    (now().saturating_sub(at) <= 90).then_some(Tps { tps: t.tps, mspt: t.mspt, at, source: "console" })
}

#[derive(Default)]
struct Tracker {
    active: HashMap<(String, &'static str), Alert>,
    cleared: HashMap<(String, &'static str), u64>,
    /// When a condition started (it must last before it fires).
    over: HashMap<(String, &'static str), u64>,
    tps: HashMap<String, Tps>,
    /// Why a server has no TPS: "rcon_off", "unsupported", or "slow" (it
    /// didn't answer within [`ANSWER_WAIT`]: ticks are taking seconds).
    no_tps: HashMap<String, &'static str>,
    /// Servers whose RCON was turned on while they ran, with when that run
    /// started: RCON comes up at their next start.
    rcon_waits: HashMap<String, Option<std::time::SystemTime>>,
    /// How far each server's `logs/latest.log` was read for players it
    /// turned away.
    log_read: HashMap<String, u64>,
}

fn state() -> &'static Mutex<Tracker> {
    static S: OnceLock<Mutex<Tracker>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn now() -> u64 {
    crate::webhooks::now()
}

/// Raises or clears one alert; returns a webhook event when it newly fires.
fn update(st: &mut Tracker, key: (String, &'static str), server: &str, on: bool, text: impl FnOnce() -> String) -> Option<crate::webhooks::Event> {
    let t = now();
    if !on {
        st.over.remove(&key);
        if st.active.remove(&key).is_some() {
            st.cleared.insert(key, t);
        }
        return None;
    }
    if st.active.contains_key(&key) {
        return None;
    }
    if st.cleared.get(&key).is_some_and(|c| t.saturating_sub(*c) < QUIET) {
        return None;
    }
    let text = text();
    let kind = key.1;
    // Drives and the PC's memory are about the whole panel.
    let sid = if key.0.starts_with("disk:") || key.0 == "host" { String::new() } else { key.0.clone() };
    let a = Alert { server_id: sid.clone(), server: server.to_string(), kind, text: text.clone(), since: t, dismissed: false };
    st.active.insert(key, a);
    log::warn!("alert: {text}");
    // Crashes already go to webhooks as "crash" events.
    (kind != "crash").then(|| crate::webhooks::Event { event: kind, server_id: sid, server: server.to_string(), text, at: t })
}

/// How long a crash the server was restarted from stays up, undismissed.
const RESTARTED_FOR: u64 = 24 * 3600;

/// A crash the scheduler restarted the server from. The server is
/// "crashed" for too short a time for [`check`] to see it, so the alert is
/// made here, and stays until it's dismissed (or for a day).
pub fn note_crash_restart(app: &App, id: &str, name: &str, attempt: u32) {
    if !settings(app).crash {
        return;
    }
    let text = format!("**{name}** crashed and was restarted (attempt {attempt} this hour)");
    log::warn!("alert: {text}");
    let a = Alert { server_id: id.to_string(), server: name.to_string(), kind: "crash", text, since: now(), dismissed: false };
    state().lock().unwrap().active.insert((id.to_string(), "restarted"), a);
}

/// Players the server's log shows it turned away since the last look (mods,
/// registries or versions that don't match the player's): the newest one
/// becomes the server's "join" alert, until dismissed or for a day. A
/// player who just left or whose connection dropped isn't one.
fn turned_away(st: &mut Tracker, id: &str, name: &str, root: &std::path::Path) {
    use std::io::{Read, Seek, SeekFrom};
    let log = root.join("logs").join("latest.log");
    let len = std::fs::metadata(&log).map_or(0, |m| m.len());
    // First look: from the end, so an old run's refusals don't show up.
    let from = *st.log_read.entry(id.to_string()).or_insert(len);
    let from = if len < from { 0 } else { from }; // a new log
    st.log_read.insert(id.to_string(), len);
    if len == from {
        return;
    }
    let Ok(mut f) = std::fs::File::open(&log) else { return };
    let start = from.max(len.saturating_sub(1 << 20));
    let _ = f.seek(SeekFrom::Start(start));
    let mut buf = Vec::new();
    let _ = f.take(len - start).read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let Some(r) = hopper_core::joinfail::server_refusals(&text).pop() else { return };
    let who = if r.player.is_empty() { "A player".to_string() } else { format!("**{}**", r.player) };
    let mods = if r.mods.is_empty() {
        String::new()
    } else {
        let shown: Vec<&str> = r.mods.iter().take(5).map(String::as_str).collect();
        format!(" Mods named: {}.", shown.join(", "))
    };
    let text = format!("{who} couldn't join **{name}**: {}{mods} {} (\"{}\")", r.why, r.fix, r.reason);
    log::warn!("alert: {text}");
    let a = Alert { server_id: id.to_string(), server: name.to_string(), kind: "join", text, since: now(), dismissed: false };
    st.active.insert((id.to_string(), "join"), a);
}

/// True once `cond` has held for `minutes`.
fn lasted(st: &mut Tracker, key: &(String, &'static str), cond: bool, minutes: u32) -> bool {
    if !cond {
        st.over.remove(key);
        return false;
    }
    let since = *st.over.entry(key.clone()).or_insert_with(now);
    now().saturating_sub(since) >= u64::from(minutes) * 60
}

fn gb(b: u64) -> String {
    format!("{:.1} GB", b as f64 / 1e9)
}

/// Free space on the drive holding `p`.
fn free_on(disks: &sysinfo::Disks, p: &std::path::Path) -> Option<(String, u64)> {
    let full = crate::absolute(p);
    disks
        .list()
        .iter()
        .filter(|d| full.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| (d.mount_point().to_string_lossy().trim_end_matches('\\').to_string(), d.available_space()))
}

pub(crate) fn has_spark(root: &std::path::Path) -> bool {
    ["mods", "plugins"].iter().any(|d| {
        std::fs::read_dir(root.join(d))
            .map(|rd| rd.flatten().any(|e| e.file_name().to_string_lossy().to_ascii_lowercase().starts_with("spark")))
            .unwrap_or(false)
    })
}

/// How long a TPS reading waits for its answer. RCON commands run on the
/// server thread between ticks, so a lagging server answers late (TNP-LL8
/// took 10-40 s with single ticks of up to 27 s); a stop still cuts the
/// wait at once (see [`guard`]).
const ANSWER_WAIT: Duration = Duration::from_secs(20);

/// An open RCON connection to one server. `idle` is out while a command
/// runs; `closer` stays so a stop can cut that command short.
struct Link {
    port: u16,
    pass: String,
    closer: rcon::Closer,
    idle: Option<rcon::Rcon>,
}

/// RCON connections stay open between readings: each new one is a
/// "Thread RCON Client started" line in the server's console.
fn links() -> &'static Mutex<HashMap<String, Link>> {
    static C: OnceLock<Mutex<HashMap<String, Link>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Closes a server's RCON connection, ending any command still waiting for
/// its answer (that command then fails quietly, without a retry).
fn cut(id: &str) {
    if let Some(l) = links().lock().unwrap().remove(id) {
        l.closer.close();
    }
}

/// Runs one command over RCON, reusing the open connection. `allowed` is
/// asked before connecting and before sending: a server that has started
/// stopping gets nothing new, and a failed command isn't retried then.
fn ask_rcon(id: &str, port: u16, pass: &str, cmd: &str, allowed: &dyn Fn() -> bool) -> anyhow::Result<String> {
    let reused = {
        let mut m = links().lock().unwrap();
        match m.get_mut(id) {
            Some(l) if l.port == port && l.pass == pass => l.idle.take(),
            Some(_) => {
                if let Some(l) = m.remove(id) {
                    l.closer.close();
                }
                None
            }
            None => None,
        }
    };
    if let Some(mut c) = reused {
        if !allowed() {
            cut(id);
            anyhow::bail!("server is stopping");
        }
        match c.command(cmd) {
            Ok(r) => {
                put_back(id, c);
                return Ok(r);
            }
            // A stale connection (the server restarted): one fresh try below.
            // Not after a timeout: a lagging server would only get a second
            // command waiting behind the first.
            Err(e) => {
                cut(id);
                if e.to_string().contains("didn't answer") {
                    return Err(e);
                }
            }
        }
    }
    if !allowed() {
        anyhow::bail!("server is stopping");
    }
    let mut c = rcon::Rcon::connect(port, pass, ANSWER_WAIT)?;
    let closer = c.closer()?;
    links().lock().unwrap().insert(id.to_string(), Link { port, pass: pass.to_string(), closer, idle: None });
    // The stop may have come while connecting; the guard can't see a
    // connection before it's in the map.
    if !allowed() {
        cut(id);
        anyhow::bail!("server is stopping");
    }
    match c.command(cmd) {
        Ok(r) => {
            put_back(id, c);
            Ok(r)
        }
        Err(e) => {
            cut(id);
            Err(e)
        }
    }
}

/// One command over the server's RCON (sharing the TPS reading's
/// connection), when RCON is on and the server isn't stopping.
pub fn rcon_once(e: &crate::servers::Entry, cmd: &str) -> Option<String> {
    if !e.server.rcon_allowed() {
        return None;
    }
    let (port, pass) = rcon::settings(&e.root)?;
    let server = e.server.clone();
    ask_rcon(&e.id, port, &pass, cmd, &|| server.rcon_allowed()).ok()
}

/// Returns a connection to the map after a command, unless it was cut
/// meanwhile (then it's dropped, which closes it).
fn put_back(id: &str, c: rcon::Rcon) {
    if let Some(l) = links().lock().unwrap().get_mut(id) {
        l.idle = Some(c);
    }
}

/// Cuts RCON to every server that is no longer up and running: asked to
/// stop, restart or be killed (from anywhere), "Stopping server" in its
/// console, or ended. Runs every half second so a stop never waits on it.
fn guard(app: &App) {
    let open: Vec<String> = links().lock().unwrap().keys().cloned().collect();
    if open.is_empty() {
        return;
    }
    let servers = app.servers.list();
    for id in open {
        let ok = servers.iter().find(|e| e.id == id).is_some_and(|e| e.server.rcon_allowed());
        if !ok {
            cut(&id);
        }
    }
}

use hopper_core::hosting::rcon::new_password;

/// Turns RCON on for one server (keeping a setup the owner made). When the
/// server is running, it comes up at its next start; until then the panel
/// says so instead of showing no TPS.
fn turn_on(app: &App, e: &crate::servers::Entry) -> anyhow::Result<rcon::Setup> {
    let others: Vec<std::path::PathBuf> = app.servers.list().iter().filter(|x| x.id != e.id).map(|x| x.root.clone()).collect();
    let s = rcon::enable_in(&e.root, &others, new_password)?;
    if s.changed() {
        if e.server.state().is_alive() {
            state().lock().unwrap().rcon_waits.insert(e.id.clone(), e.server.started_at());
        }
        let how = if matches!(s, rcon::Setup::Adopted { .. }) { "switched on with its own password" } else { "turned on" };
        log::info!("{}: RCON {how} (port {}) so the panel can read TPS", e.name, s.port());
    }
    Ok(s)
}

/// With "RCON for all servers" on, turns it on for every server that has
/// none yet: ones made, copied, downloaded or imported since the last look.
fn auto_rcon(app: &App) {
    if !settings(app).auto_rcon {
        return;
    }
    for e in app.servers.list() {
        if rcon::settings(&e.root).is_none()
            && let Err(err) = turn_on(app, &e)
        {
            log::warn!("{}: couldn't turn RCON on: {err:#}", e.name);
        }
    }
}

/// Reads TPS from every server that is up (past "Done", not stopping) and
/// has RCON.
fn poll_tps(app: &App) {
    for e in app.servers.list() {
        // spark's own report in the console: no RCON or command needed.
        if let Some(t) = console_reading(&e) {
            let mut st = state().lock().unwrap();
            st.no_tps.remove(&e.id);
            st.tps.insert(e.id.clone(), t);
            continue;
        }
        // RCON switched on while it ran: nothing listens until it restarts.
        {
            let mut st = state().lock().unwrap();
            match st.rcon_waits.get(&e.id) {
                Some(at) if *at == e.server.started_at() && e.server.state().is_alive() => {
                    st.tps.remove(&e.id);
                    st.no_tps.insert(e.id.clone(), "restart");
                    continue;
                }
                Some(_) => {
                    st.rcon_waits.remove(&e.id);
                    if st.no_tps.get(&e.id) == Some(&"restart") {
                        st.no_tps.remove(&e.id);
                    }
                }
                None => {}
            }
        }
        if !e.server.rcon_allowed() {
            cut(&e.id);
            let mut st = state().lock().unwrap();
            st.tps.remove(&e.id);
            st.no_tps.remove(&e.id);
            continue;
        }
        let version = e.minecraft.clone().unwrap_or_default();
        let Some(cmd) = rcon::tps_command(e.loader.as_deref(), &version, has_spark(&e.root)) else {
            state().lock().unwrap().no_tps.insert(e.id.clone(), "unsupported");
            continue;
        };
        let Some((port, pass)) = rcon::settings(&e.root) else {
            state().lock().unwrap().no_tps.insert(e.id.clone(), "rcon_off");
            continue;
        };
        let server = e.server.clone();
        let r = ask_rcon(&e.id, port, &pass, cmd, &|| server.rcon_allowed());
        let timed_out = r.as_ref().is_err_and(|e| e.to_string().contains("didn't answer"));
        let mut st = state().lock().unwrap();
        match r.ok().as_deref().and_then(rcon::parse_tps) {
            Some((tps, mspt)) => {
                st.no_tps.remove(&e.id);
                st.tps.insert(e.id.clone(), Tps { tps, mspt, at: now(), source: "rcon" });
            }
            None => {
                // RCON not up yet, or a slow answer: keep the last reading
                // a little while.
                if st.tps.get(&e.id).is_some_and(|t| now() - t.at > 120) {
                    st.tps.remove(&e.id);
                }
                if timed_out && !st.tps.contains_key(&e.id) {
                    st.no_tps.insert(e.id.clone(), "slow");
                } else if cmd == "spark tps" && !st.tps.contains_key(&e.id) {
                    // spark answers RCON after the command returns, so its
                    // reply is lost there; its console report is read instead.
                    st.no_tps.insert(e.id.clone(), "spark_console");
                } else if !timed_out && st.no_tps.get(&e.id) == Some(&"slow") {
                    st.no_tps.remove(&e.id);
                }
            }
        }
    }
}

fn check(app: &Arc<App>, disks: &mut sysinfo::Disks) {
    let s = settings(app);
    let snap = app.snapshot.borrow().clone();
    let mut events = Vec::new();
    disks.refresh(true);
    let mut st = state().lock().unwrap();
    let t = now();
    st.active.retain(|k, a| !matches!(k.1, "restarted" | "join") || t.saturating_sub(a.since) < RESTARTED_FOR);
    for e in app.servers.list() {
        turned_away(&mut st, &e.id, &e.name, &e.root);
    }
    for srv in &snap.servers {
        let id = srv.id.clone();
        let crashed = s.crash && srv.state == "crashed";
        events.extend(update(&mut st, (id.clone(), "crash"), &srv.name, crashed, || {
            format!("**{}** crashed{}", srv.name, srv.exit_code.map(|c| format!(" (exit code {c})")).unwrap_or_default())
        }));
        let reading = st.tps.get(&id).copied().filter(|_| srv.state == "running");
        let key = (id.clone(), "tps");
        let low = s.tps_on && reading.is_some_and(|t| t.tps < s.tps_below);
        let fire = lasted(&mut st, &key, low, s.tps_minutes);
        // Clears once it's back a little above the line.
        let clear = !s.tps_on || reading.is_none_or(|t| t.tps >= s.tps_below + 1.0);
        if fire || clear {
            events.extend(update(&mut st, key, &srv.name, fire, || {
                let t = reading.unwrap();
                format!(
                    "**{}** is running slowly: {:.1} TPS{} for {} min",
                    srv.name,
                    t.tps,
                    t.mspt.map(|m| format!(" ({m:.0} ms a tick)")).unwrap_or_default(),
                    s.tps_minutes
                )
            }));
        }
    }
    // Drives: every server's and the panel's own.
    let mut drives: HashMap<String, (u64, Vec<String>)> = HashMap::new();
    for e in app.servers.list() {
        if let Some((mount, free)) = free_on(disks, &e.root) {
            drives.entry(mount).or_insert((free, Vec::new())).1.push(e.name.clone());
        }
    }
    if let Some((mount, free)) = free_on(disks, &app.data_dir) {
        drives.entry(mount).or_insert((free, Vec::new()));
    }
    let limit = u64::from(s.disk_gb) * 1_000_000_000;
    for (mount, (free, names)) in &drives {
        let low = s.disk_on && *free < limit;
        events.extend(update(&mut st, (format!("disk:{mount}"), "disk"), "", low, || {
            let who = if names.is_empty() { String::new() } else { format!(" ({})", names.join(", ")) };
            format!("Drive {mount} is nearly full: {} free{who}", gb(*free))
        }));
    }
    let h = &snap.host;
    let pct = if h.mem_total > 0 { h.mem_used as f64 * 100.0 / h.mem_total as f64 } else { 0.0 };
    let key = (String::from("host"), "ram");
    let high = s.ram_on && pct >= f64::from(s.ram_pct);
    let fire = lasted(&mut st, &key, high, s.ram_minutes);
    if fire || !high {
        events.extend(update(&mut st, key, "", fire, || format!("{} is nearly out of memory: {pct:.0} % used ({} of {})", h.hostname, gb(h.mem_used), gb(h.mem_total))));
    }
    drop(st);
    crate::webhooks::emit(app, events);
}

/// Watches for alerts for as long as the panel runs.
pub fn start(app: Arc<App>) {
    // RCON for all servers: set up the moment any server starts, so one
    // started right after it was made doesn't wait for the 30 s look.
    let h = app.clone();
    hopper_core::hosting::process::on_before_start(move |root| {
        if !settings(&h).auto_rcon || rcon::settings(root).is_some() {
            return;
        }
        if let Some(e) = h.servers.list().into_iter().find(|e| e.root == root)
            && let Err(err) = turn_on(&h, &e)
        {
            log::warn!("{}: couldn't turn RCON on: {err:#}", e.name);
        }
    });
    let a = app.clone();
    std::thread::Builder::new()
        .name("rcon-guard".into())
        .spawn(move || {
            loop {
                guard(&a);
                std::thread::sleep(Duration::from_millis(500));
            }
        })
        .ok();
    // Its own thread: a lagging server can take a while to answer.
    let p = app.clone();
    std::thread::Builder::new()
        .name("tps".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(15));
            loop {
                let t = Instant::now();
                auto_rcon(&p);
                poll_tps(&p);
                std::thread::sleep(Duration::from_secs(30).saturating_sub(t.elapsed()).max(Duration::from_secs(5)));
            }
        })
        .ok();
    std::thread::Builder::new()
        .name("alerts".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(15));
            let mut disks = sysinfo::Disks::new_with_refreshed_list();
            loop {
                check(&app, &mut disks);
                std::thread::sleep(Duration::from_secs(15));
            }
        })
        .ok();
}

/// The server's last TPS reading, while it's fresh (for automations).
pub fn tps_now(id: &str) -> Option<f32> {
    state().lock().unwrap().tps.get(id).map(|t| t.tps)
}

// ---- API -------------------------------------------------------------------

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/alerts", get(list))
        .route("/api/v1/alerts/settings", put(save))
        .route("/api/v1/alerts/dismiss", post(dismiss))
        .route("/api/v1/servers/{id}/rcon", post(enable_rcon))
}

async fn list(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    let who = require(&app, &headers, &method)?;
    let owner = require_owner(&app, &headers, &method).is_ok();
    // A console report shows the moment it's printed, not at the next poll.
    let fresh: Vec<(String, Tps)> = app.servers.list().iter().filter(|e| who.sees(&e.id)).filter_map(|e| Some((e.id.clone(), console_reading(e)?))).collect();
    let st = state().lock().unwrap();
    let mut active: Vec<&Alert> = st.active.values().filter(|a| a.server_id.is_empty() || who.sees(&a.server_id)).collect();
    active.sort_by_key(|a| a.since);
    let mut tps: HashMap<String, Tps> = st.tps.iter().filter(|(id, _)| who.sees(id)).map(|(k, v)| (k.clone(), *v)).collect();
    let mut no_tps: HashMap<String, &str> = st.no_tps.iter().filter(|(id, _)| who.sees(id)).map(|(k, v)| (k.clone(), *v)).collect();
    for (id, t) in fresh {
        no_tps.remove(&id);
        tps.insert(id, t);
    }
    Ok(Json(json!({ "active": active, "tps": tps, "no_tps": no_tps, "settings": owner.then(|| settings(&app)) })))
}

async fn save(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(s): Json<Settings>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let s = Settings {
        tps_below: s.tps_below.clamp(1.0, 20.0),
        tps_minutes: s.tps_minutes.clamp(1, 60),
        disk_gb: s.disk_gb.clamp(1, 10_000),
        ram_pct: s.ram_pct.clamp(50, 100),
        ram_minutes: s.ram_minutes.clamp(1, 60),
        ..s
    };
    let path = app.data_dir.join(FILE);
    std::fs::write(&path, serde_json::to_vec_pretty(&s).map_err(bad)?).map_err(bad)?;
    log::info!("alert settings changed");
    if s.auto_rcon {
        let a = app.clone();
        tokio::task::spawn_blocking(move || auto_rcon(&a));
    }
    Ok(Json(json!({ "settings": s })))
}

#[derive(Deserialize)]
struct DismissBody {
    server_id: String,
    kind: String,
}

async fn dismiss(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<DismissBody>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let mut st = state().lock().unwrap();
    for a in st.active.values_mut().filter(|a| a.server_id == b.server_id && a.kind == b.kind) {
        a.dismissed = true;
    }
    Ok(Json(json!({ "ok": true })))
}

/// Turns RCON on with a random password and a free port (or switches on
/// the owner's own setup), so the panel can read TPS. It takes effect when
/// the server next starts.
async fn enable_rcon(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let a = app.clone();
    let alive = e.server.state().is_alive();
    let s = tokio::task::spawn_blocking(move || turn_on(&a, &e)).await.map_err(bad)?.map_err(|e| bad(format!("{e:#}")))?;
    Ok(Json(json!({ "port": s.port(), "restart": alive && s.changed() })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// A fake RCON server: accepts any login, answers commands after
    /// `delay`. Returns its port and how many connections it took.
    fn fake(delay: Duration) -> (u16, Arc<AtomicUsize>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                c.fetch_add(1, Ordering::SeqCst);
                std::thread::spawn(move || serve(s, delay));
            }
        });
        (port, count)
    }

    fn serve(mut s: TcpStream, delay: Duration) {
        let mut first = true;
        loop {
            let mut len = [0u8; 4];
            if s.read_exact(&mut len).is_err() {
                return;
            }
            let mut b = vec![0u8; i32::from_le_bytes(len) as usize];
            if s.read_exact(&mut b).is_err() {
                return;
            }
            let id = i32::from_le_bytes(b[0..4].try_into().unwrap());
            if !first {
                std::thread::sleep(delay);
            }
            let (kind, body) = if first { (2, String::new()) } else { (0, "TPS from last 1m, 5m, 15m: 19.5, 20.0, 20.0".to_string()) };
            first = false;
            let mut p = (10 + body.len() as i32).to_le_bytes().to_vec();
            p.extend_from_slice(&id.to_le_bytes());
            p.extend_from_slice(&(kind as i32).to_le_bytes());
            p.extend_from_slice(body.as_bytes());
            p.extend_from_slice(&[0, 0]);
            if s.write_all(&p).is_err() {
                return;
            }
        }
    }

    #[test]
    fn a_turned_away_player_becomes_a_join_alert() {
        let dir = std::env::temp_dir().join(format!("hopperd-join-{}", std::process::id()));
        let logs = dir.join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        let log = logs.join("latest.log");
        std::fs::write(&log, "[21:00:00] [Server thread/INFO] [minecraft/DedicatedServer]: Done (4.1s)!
").unwrap();
        let mut st = Tracker::default();
        // The first look starts at the end.
        turned_away(&mut st, "s1", "Test", &dir);
        assert!(st.active.is_empty());
        let mut more = std::fs::read_to_string(&log).unwrap();
        more.push_str("[21:01:00] [Server thread/INFO] [minecraft/ServerGamePacketListenerImpl]: Jeb lost connection: Disconnected
");
        more.push_str("[21:02:00] [Netty Server IO #4/INFO] [minecraft/ServerConfigurationPacketListenerImpl]: Alex lost connection: Missing registry entries: create:brass_casing
");
        std::fs::write(&log, more).unwrap();
        turned_away(&mut st, "s1", "Test", &dir);
        let a = st.active.get(&("s1".to_string(), "join")).expect("join alert");
        assert!(a.text.contains("**Alex** couldn't join **Test**"), "{}", a.text);
        assert!(a.text.contains("create"), "{}", a.text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reuses_one_connection_while_running() {
        let (port, count) = fake(Duration::ZERO);
        let yes = || true;
        assert!(ask_rcon("t-reuse", port, "pw", "spark tps", &yes).unwrap().contains("19.5"));
        assert!(ask_rcon("t-reuse", port, "pw", "spark tps", &yes).is_ok());
        assert_eq!(count.load(Ordering::SeqCst), 1);
        cut("t-reuse");
    }

    #[test]
    fn nothing_is_sent_to_a_stopping_server() {
        let (port, count) = fake(Duration::ZERO);
        let running = AtomicBool::new(true);
        let allowed = || running.load(Ordering::SeqCst);
        assert!(ask_rcon("t-stopping", port, "pw", "spark tps", &allowed).is_ok());
        running.store(false, Ordering::SeqCst);
        assert!(ask_rcon("t-stopping", port, "pw", "spark tps", &allowed).is_err());
        assert!(!links().lock().unwrap().contains_key("t-stopping"), "the connection is closed");
        assert!(ask_rcon("t-stopping", port, "pw", "spark tps", &allowed).is_err());
        assert_eq!(count.load(Ordering::SeqCst), 1, "no new connection");
    }

    /// The stop comes while a reading waits for its answer: the guard's cut
    /// ends it at once, and it isn't retried.
    #[test]
    fn a_stop_cuts_a_waiting_command_without_retry() {
        let (port, count) = fake(Duration::from_secs(10));
        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();
        let t = Instant::now();
        let h = std::thread::spawn(move || ask_rcon("t-inflight", port, "pw", "spark tps", &|| r.load(Ordering::SeqCst)));
        let end = Instant::now() + Duration::from_secs(3);
        while !links().lock().unwrap().contains_key("t-inflight") && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(100));
        running.store(false, Ordering::SeqCst);
        cut("t-inflight");
        assert!(h.join().unwrap().is_err());
        assert!(t.elapsed() < Duration::from_secs(3), "waited for the read timeout");
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(count.load(Ordering::SeqCst), 1, "no retry");
        assert!(!links().lock().unwrap().contains_key("t-inflight"));
    }
}
