//! The home panel's side of "Relay through a VPS" (design/vps-relay.md,
//! B2): one outbound connection to the relay, kept up for as long as the
//! panel runs (reconnecting with back-off), and every player stream it
//! brings handed to the router for the server it names.
//!
//! The pair is set by hand in B2 (`PUT /api/v1/vps-relay`: the relay's
//! address, its certificate fingerprint and key, which servers go through
//! it); the ssh setup that fills these in is B3. Kept in `vps-relay.json`
//! in the panel's data folder, owner-only; the key never leaves it (no API
//! answer, no log line).
//!
//! Counters: the codec's own (raw, sent, cache hits, both directions) for
//! the connection, and per server what its players were sent (raw bytes,
//! and what vanilla would have sent for them, `VanillaEstimate`). One
//! compressed stream carries every server, so a server's share of what was
//! sent and of the cache hits is its share of the raw bytes. Totals and an
//! hour of 10-second samples are kept in `vps-relay-stats.json`.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post};
use axum::{Json, Router};
use hopper_core::hosting::relay_codec::VanillaEstimate;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::Notify;

use super::link::{self, LinkError, Live};
use super::wire::{Codec, Counters};
use super::{Control, Head, Route};
use crate::App;
use crate::api::{ApiResult, bad, not_found, require, require_owner};
use crate::tunnel::dial::{self, Pin};
use crate::tunnel::mux;

const FILE: &str = "vps-relay.json";
const STATS_FILE: &str = "vps-relay-stats.json";
/// The relay's tunnel port when the address names none.
pub const TUNNEL_PORT: u16 = 25566;
/// How often the counters are sampled, and how many samples are kept (an hour).
const TICK: Duration = Duration::from_secs(10);
const SAMPLES: usize = 360;
/// The longest wait between tries.
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const SETTINGS_CHANGED: &str = "The relay settings changed.";

/// Writes a file only this user can read (0600 on Linux and macOS; on
/// Windows the panel's data folder is already the user's own).
pub fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write as _;
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let mut f = o.open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// `vps-relay.json`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct HomeConfig {
    pub enabled: bool,
    /// The relay's tunnel port, `host:port` (`:25566` when left out).
    pub address: String,
    /// SHA-256 of the relay's CA, "AB:CD:…" (`hopperd relay init` prints it).
    pub fingerprint: String,
    /// The relay key (`hrt_…`). Never sent back by the API.
    pub key: String,
    /// Servers whose players come through the relay.
    pub servers: Vec<String>,
    /// What players type to join; empty: the relay's host and game port.
    pub public_address: String,
}

impl HomeConfig {
    fn complete(&self) -> bool {
        self.enabled && !self.address.is_empty() && !self.fingerprint.is_empty() && !self.key.is_empty() && !self.servers.is_empty()
    }
}

/// `host:port` of the relay, with the default tunnel port filled in.
pub fn relay_target(address: &str) -> Result<(String, u16), String> {
    let a = address.trim();
    let with_port = if a.starts_with('[') && !a.contains("]:") || !a.starts_with('[') && !a.contains(':') { format!("{a}:{TUNNEL_PORT}") } else { a.to_string() };
    dial::host_port(&with_port).map_err(|e| e.to_string())
}

/// Counters one server's streams add to while they run.
#[derive(Default)]
pub struct ServerLive {
    /// What its players were sent (before the tunnel compressed it).
    pub raw_down: AtomicU64,
    /// What they sent.
    pub raw_up: AtomicU64,
    /// What vanilla would have sent them.
    pub vanilla: AtomicU64,
    /// Player connections (and server list pings) open now.
    pub open: AtomicUsize,
    /// Of those, the ones open for a while: players in the game, not the
    /// server list asking how many there are.
    pub players: AtomicUsize,
    pub streams: AtomicU64,
}

/// A stream open this long is a player, not a server list ping (those
/// close within a second).
const PLAYER_AFTER: Duration = Duration::from_secs(2);

/// Counts one player while it lives.
struct Player(Option<Arc<ServerLive>>);

impl Drop for Player {
    fn drop(&mut self) {
        if let Some(l) = self.0.take() {
            l.players.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Totals {
    /// Raw bytes to players.
    pub raw: u64,
    /// Their share of what the tunnel sent (estimated, see the module).
    pub sent: u64,
    pub vanilla: u64,
    /// Their share of the bytes the cache sent as references.
    pub hit_bytes: u64,
    /// Raw bytes from players.
    pub raw_up: u64,
    pub streams: u64,
}

impl Totals {
    fn add(&mut self, o: &Totals) {
        self.raw += o.raw;
        self.sent += o.sent;
        self.vanilla += o.vanilla;
        self.hit_bytes += o.hit_bytes;
        self.raw_up += o.raw_up;
        self.streams += o.streams;
    }
}

/// Ten seconds of one server: unix time, sent, vanilla, raw.
pub type Sample = (u64, u64, u64, u64);

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Saved {
    totals: HashMap<String, Totals>,
    samples: HashMap<String, VecDeque<Sample>>,
    /// Whole tunnel, every connection: raw, sent and cache-hit bytes home
    /// sent, and how many connections there were.
    link: Totals,
    connections: u64,
}

#[derive(Default)]
struct Stats {
    saved: Saved,
    /// This connection, per server.
    conn: HashMap<String, Totals>,
    live: HashMap<String, Arc<ServerLive>>,
    /// What the counters said at the last tick.
    seen: HashMap<String, (u64, u64, u64, u64)>,
    seen_link: (u64, u64, u64),
    /// The last tick's rate, per server: sent and vanilla bytes per second.
    now: HashMap<String, (u64, u64)>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct LinkStatus {
    /// "off" (not set up or turned off), "connecting", "up", "down".
    pub state: &'static str,
    /// Unix seconds: up since, or down since.
    pub since: Option<u64>,
    /// Why it's down, in words a player can act on.
    pub error: String,
    /// Unix seconds of the next try.
    pub retry_at: Option<u64>,
    pub relay_version: String,
    pub game_port: u16,
    /// The relay's voice port (B6); 0: the relay has none.
    pub voice_port: u16,
}

pub struct Home {
    dir: PathBuf,
    cfg: Mutex<HomeConfig>,
    status: Mutex<LinkStatus>,
    link: Mutex<Option<(mux::Handle, Arc<Counters>)>>,
    stats: Mutex<Stats>,
    /// Try now (or the settings changed).
    wake: Notify,
    /// The Players tab wants the relay's pings: the control loop asks.
    ask_pings: Notify,
    /// The relay's last answer: when, and server -> lowercase name -> ms.
    pings: Mutex<Option<(Instant, HashMap<String, HashMap<String, u32>>)>>,
    /// Bumped on every answer, so a waiting reader wakes.
    pings_in: tokio::sync::watch::Sender<u64>,
    /// Voice through the relay while connected (B6).
    pub voice: Arc<super::voice::home::Slot>,
    /// Each server's last health check ("Test again"), for the card.
    last_test: Mutex<HashMap<String, Value>>,
}

pub(super) fn unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Home {
    pub fn new(dir: &Path) -> Home {
        let cfg = std::fs::read(dir.join(FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let saved = std::fs::read(dir.join(STATS_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Home {
            dir: dir.to_path_buf(),
            cfg: Mutex::new(cfg),
            status: Mutex::new(LinkStatus { state: "off", ..Default::default() }),
            link: Mutex::new(None),
            stats: Mutex::new(Stats { saved, ..Default::default() }),
            wake: Notify::new(),
            ask_pings: Notify::new(),
            pings: Mutex::new(None),
            pings_in: tokio::sync::watch::channel(0).0,
            voice: Arc::default(),
            last_test: Mutex::default(),
        }
    }

    pub(super) fn set_last_test(&self, id: &str, res: Value) {
        self.last_test.lock().unwrap().insert(id.to_string(), res);
    }

    pub fn config(&self) -> HomeConfig {
        self.cfg.lock().unwrap().clone()
    }

    pub(super) fn set_config(&self, c: HomeConfig) -> anyhow::Result<()> {
        write_private(&self.dir.join(FILE), &serde_json::to_vec_pretty(&c)?)?;
        *self.cfg.lock().unwrap() = c;
        self.wake.notify_one();
        Ok(())
    }

    pub fn status(&self) -> LinkStatus {
        self.status.lock().unwrap().clone()
    }

    /// Sends the routes again now (a server's relay settings changed).
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// The relay's ping readings for `server`'s players (lowercase name ->
    /// ms, measured on each player's own connection to the VPS) and the
    /// tunnel's round trip from the VPS to here. `None` when the server
    /// doesn't use the relay or it isn't connected. Asked for only while
    /// the Players tab is open; an answer is reused for a few seconds, and
    /// a slow one is waited for at most 800 ms (the next refresh has it).
    pub async fn relay_pings(&self, server: &str) -> Option<(HashMap<String, u32>, Option<u32>)> {
        if !self.config().servers.iter().any(|s| s == server) {
            return None;
        }
        let rtt = {
            let l = self.link.lock().unwrap();
            let (h, _) = l.as_ref().filter(|(h, _)| !h.is_dead())?;
            h.rtt().map(|d| d.as_millis().min(u32::MAX as u128) as u32)
        };
        let fresh = |max: Duration| self.pings.lock().unwrap().as_ref().filter(|(at, _)| at.elapsed() < max).map(|(_, m)| m.get(server).cloned().unwrap_or_default());
        if let Some(m) = fresh(Duration::from_secs(3)) {
            return Some((m, rtt));
        }
        let mut seen = self.pings_in.subscribe();
        self.ask_pings.notify_one();
        let _ = tokio::time::timeout(Duration::from_millis(800), seen.changed()).await;
        Some((fresh(Duration::from_secs(15)).unwrap_or_default(), rtt))
    }

    #[cfg(test)]
    pub fn has_relay_answer(&self) -> bool {
        self.pings.lock().unwrap().is_some()
    }

    /// An answer from the relay.
    fn got_pings(&self, line: super::RelayLine) {
        let mut by: HashMap<String, HashMap<String, u32>> = HashMap::new();
        for p in line.pings {
            by.entry(p.server).or_default().insert(p.name.to_lowercase(), p.ms);
        }
        *self.pings.lock().unwrap() = Some((Instant::now(), by));
        self.pings_in.send_modify(|n| *n += 1);
    }

    /// What players type to join through the relay.
    pub fn players_address(&self) -> String {
        let cfg = self.config();
        if !cfg.public_address.trim().is_empty() {
            return cfg.public_address.trim().to_string();
        }
        let host = relay_target(&cfg.address).map(|(h, _)| h).unwrap_or_default();
        match self.status().game_port {
            0 | 25565 => host,
            p => format!("{host}:{p}"),
        }
    }

    /// Where a server's voice mod tells players to send voice (B6): the
    /// players' host and the relay's voice port. `None` while the connected
    /// relay has no voice port (one from before B6 or set up without it);
    /// before any connection, the voice mods' usual port.
    pub fn voice_to(&self) -> Option<(String, u16)> {
        let cfg = self.config();
        let a = if cfg.public_address.trim().is_empty() { cfg.address.trim() } else { cfg.public_address.trim() };
        let host = relay_target(a).map(|(h, _)| h).ok().filter(|h| !h.is_empty())?;
        let s = self.status();
        let port = match s.voice_port {
            0 if s.relay_version.is_empty() => hopper_core::hosting::voice_mods::SVC_DEFAULT_PORT,
            0 => return None,
            p => p,
        };
        Some((host, port))
    }

    fn set_status(&self, f: impl FnOnce(&mut LinkStatus)) {
        f(&mut self.status.lock().unwrap());
    }

    /// The counters a new stream for `server` adds to.
    pub fn server_live(&self, server: &str) -> Arc<ServerLive> {
        self.stats.lock().unwrap().live.entry(server.to_string()).or_default().clone()
    }

    /// Takes the counters' change since the last tick into the totals and
    /// the samples. A server's share of what the tunnel sent (and of the
    /// cache hits) is its share of the raw bytes in this tick.
    pub(super) fn tick(&self) {
        let counters = self.link.lock().unwrap().as_ref().map(|(_, c)| c.get());
        let mut st = self.stats.lock().unwrap();
        let link = counters.map(|c| (c.sent.raw, c.sent.wire, c.sent.hit_bytes)).unwrap_or(st.seen_link);
        let (d_raw, d_wire, d_hits) = (link.0.saturating_sub(st.seen_link.0), link.1.saturating_sub(st.seen_link.1), link.2.saturating_sub(st.seen_link.2));
        st.seen_link = link;
        st.saved.link.raw += d_raw;
        st.saved.link.sent += d_wire;
        st.saved.link.hit_bytes += d_hits;
        let now = unix();
        let live: Vec<(String, Arc<ServerLive>)> = st.live.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (id, l) in live {
            let cur = (l.raw_down.load(Ordering::Relaxed), l.vanilla.load(Ordering::Relaxed), l.raw_up.load(Ordering::Relaxed), l.streams.load(Ordering::Relaxed));
            let was = st.seen.insert(id.clone(), cur).unwrap_or_default();
            let raw = cur.0.saturating_sub(was.0);
            let share = |x: u64| if d_raw == 0 { 0 } else { (x as u128 * raw.min(d_raw) as u128 / d_raw as u128) as u64 };
            let d = Totals { raw, sent: share(d_wire), vanilla: cur.1.saturating_sub(was.1), hit_bytes: share(d_hits), raw_up: cur.2.saturating_sub(was.2), streams: cur.3.saturating_sub(was.3) };
            let secs = TICK.as_secs().max(1);
            st.now.insert(id.clone(), (d.sent / secs, d.vanilla / secs));
            st.conn.entry(id.clone()).or_default().add(&d);
            st.saved.totals.entry(id.clone()).or_default().add(&d);
            let q = st.saved.samples.entry(id).or_default();
            q.push_back((now, d.sent, d.vanilla, d.raw));
            while q.len() > SAMPLES {
                q.pop_front();
            }
        }
    }

    fn save_stats(&self) {
        let body = serde_json::to_vec(&self.stats.lock().unwrap().saved);
        if let Ok(b) = body
            && let Err(e) = write_private(&self.dir.join(STATS_FILE), &b)
        {
            log::warn!("relay: saving its counters: {e:#}");
        }
    }

    /// A new connection: its own counters start at zero.
    fn connected(&self, handle: mux::Handle, counters: Arc<Counters>) {
        let mut st = self.stats.lock().unwrap();
        st.conn.clear();
        st.now.clear();
        st.seen_link = (0, 0, 0);
        st.saved.connections += 1;
        // The per-server counters keep counting; only what this tick sees
        // as new is taken (so a stream still open across a reconnect is
        // not counted twice).
        drop(st);
        *self.link.lock().unwrap() = Some((handle, counters));
    }

    fn disconnected(&self) {
        self.tick();
        *self.link.lock().unwrap() = None;
        let mut st = self.stats.lock().unwrap();
        st.now.clear();
        drop(st);
        self.save_stats();
    }

    /// The status card's numbers for one server.
    pub fn server_status(&self, id: &str) -> Value {
        let cfg = self.config();
        if !cfg.servers.iter().any(|s| s == id) {
            return json!({ "relayed": false });
        }
        let s = self.status();
        let rtt = self.link.lock().unwrap().as_ref().and_then(|(h, _)| h.rtt()).map(|d| d.as_millis() as u64);
        let st = self.stats.lock().unwrap();
        let conn = st.conn.get(id).cloned().unwrap_or_default();
        let total = st.saved.totals.get(id).cloned().unwrap_or_default();
        let (sent_bps, vanilla_bps) = st.now.get(id).copied().unwrap_or_default();
        let samples: Vec<Sample> = st.saved.samples.get(id).map(|q| q.iter().copied().collect()).unwrap_or_default();
        let open = st.live.get(id).map_or(0, |l| l.open.load(Ordering::Relaxed));
        let players = st.live.get(id).map_or(0, |l| l.players.load(Ordering::Relaxed));
        drop(st);
        let test = self.last_test.lock().unwrap().get(id).cloned();
        let address = self.players_address();
        let now = unix();
        json!({
            "relayed": true,
            "enabled": cfg.enabled,
            "state": s.state,
            "since": s.since,
            "error": s.error,
            "retry_in": s.retry_at.map(|t| t.saturating_sub(now)),
            "rtt_ms": rtt,
            "relay_version": s.relay_version,
            "address": address,
            "open": open,
            "players": players,
            "test": test,
            "connection": conn,
            "total": total,
            "now": { "sent": sent_bps, "vanilla": vanilla_bps },
            "samples": samples,
        })
    }

    /// The card's voice row (B6): `null` when the server has no voice mod.
    /// `state`: "on" (UDP straight through), "tcp" (UDP doesn't get through
    /// the VPS, voice rides the tunnel), "checking", "down" (no relay
    /// connection), "old" (the relay has no voice port), "not-set" (the mod
    /// still points players at home: "Use the relay" sets it).
    pub fn voice_json(&self, root: &Path, set: bool) -> Value {
        let Some(d) = hopper_core::hosting::voice_mods::detect(root, server_port(root)) else { return Value::Null };
        let s = self.status();
        let live = self.voice.status();
        let state = if !set {
            "not-set"
        } else if s.state != "up" {
            "down"
        } else if s.voice_port == 0 {
            "old"
        } else {
            match live.as_ref().map(|l| l.via) {
                Some("udp") => "on",
                Some("tcp") => "tcp",
                _ => "checking",
            }
        };
        json!({
            "mod": d.kind.name(),
            "state": state,
            "players": live.as_ref().map_or(0, |l| l.players),
            "rtt_ms": live.as_ref().and_then(|l| l.rtt_ms),
            "port": if s.voice_port != 0 { s.voice_port } else { hopper_core::hosting::voice_mods::SVC_DEFAULT_PORT },
        })
    }
}

/// Keeps the connection up while the panel runs.
pub fn start(app: Arc<App>) {
    tokio::spawn(async move {
        let mut backoff = Duration::from_secs(1);
        loop {
            let cfg = app.vps.config();
            if !cfg.complete() {
                app.vps.set_status(|s| *s = LinkStatus { state: "off", ..Default::default() });
                tokio::select! {
                    _ = app.vps.wake.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                }
                continue;
            }
            app.vps.set_status(|s| {
                s.state = "connecting";
                s.retry_at = None;
            });
            let began = Instant::now();
            let why = match connect(&cfg).await {
                Ok((live, hello)) => {
                    backoff = Duration::from_secs(1);
                    log::info!("relay: connected to {} (Hopper Server {})", cfg.address, hello.version);
                    app.vps.set_status(|s| {
                        *s = LinkStatus { state: "up", since: Some(unix()), relay_version: hello.version.clone(), game_port: hello.game_port, voice_port: hello.voice_port, ..Default::default() };
                    });
                    let why = run(&app, live, &cfg).await;
                    log::warn!("relay: connection to {} ended ({why})", cfg.address);
                    why
                }
                Err(e) => {
                    log::info!("relay: can't connect to {}: {e}", cfg.address);
                    e
                }
            };
            // New settings: the new connection starts now.
            if why == SETTINGS_CHANGED {
                backoff = Duration::from_secs(1);
                continue;
            }
            if began.elapsed() > Duration::from_secs(60) {
                backoff = Duration::from_secs(1);
            }
            // A little jitter, so a relay that comes back isn't hit by every
            // panel at the same moment.
            let jitter = Duration::from_millis(u64::from_str_radix(&crate::auth::random_hex(2), 16).unwrap_or(0) % 1000);
            let wait = backoff + jitter;
            let down_since = app.vps.status().since.filter(|_| app.vps.status().state == "down");
            app.vps.set_status(|s| {
                *s = LinkStatus { state: "down", since: down_since.or(Some(unix())), error: why, retry_at: Some(unix() + wait.as_secs().max(1)), game_port: s.game_port, voice_port: s.voice_port, ..Default::default() };
            });
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = app.vps.wake.notified() => {}
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    });
}

/// One try: TCP, TLS pinned to the relay's CA, the key, the tunnel.
pub(super) async fn connect(cfg: &HomeConfig) -> Result<(Live, link::RelayHello), String> {
    let (host, port) = relay_target(&cfg.address)?;
    let tcp = match tokio::time::timeout(Duration::from_secs(10), TcpStream::connect((host.as_str(), port))).await {
        Ok(Ok(s)) => s,
        // In words an owner can act on, not the system's error text.
        Ok(Err(e)) => return Err(format!("Can't reach the VPS at {host}:{port}: {}", super::health::connect_words(&e, port))),
        Err(_) => return Err(format!("Can't reach the VPS at {host}:{port}: {}", super::health::no_answer(port))),
    };
    let _ = tcp.set_nodelay(true);
    let (tls, _) = dial::pinned_config(&Pin::Fingerprint(cfg.fingerprint.clone()), vec![link::ALPN.to_vec()]);
    let name = rustls::pki_types::ServerName::try_from(host.clone()).map_err(|e| format!("The relay's address isn't usable: {e}"))?;
    let tls = match tokio::time::timeout(Duration::from_secs(15), tokio_rustls::TlsConnector::from(tls).connect(name, tcp)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            let cert = e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>()).is_some_and(|r| matches!(r, rustls::Error::InvalidCertificate(_)));
            return Err(if cert {
                "The relay's certificate doesn't match the fingerprint saved here.".into()
            } else {
                format!("The VPS answered on port {port}, but the secure connection to the relay failed. Open the relay setup and press Run again to repair it.")
            });
        }
        Err(_) => return Err(format!("The port {port} is open but the relay didn't answer.")),
    };
    // Voice keys (B6) come from this TLS connection's exported secret:
    // only its two ends know it, so the UDP path needs no secret of its own.
    let exported: Option<super::voice::Exported> = tls.get_ref().1.export_keying_material([0u8; 64], super::voice::EXPORTER_LABEL, None).ok();
    let (mut live, hello) = link::home(tls, &cfg.key, Codec::quiet(), link::mux_config()).await.map_err(|e| match e {
        LinkError::Refused(m) => m,
        LinkError::Mux(e) => super::health::mux_words(&e),
        _ => "The relay didn't finish connecting. Open the relay setup and press Run again to repair it.".into(),
    })?;
    live.exported = exported;
    Ok((live, hello))
}

/// The routes home sends on its control stream.
fn routes(app: &App) -> Control {
    let cfg = app.vps.config();
    let velocity = |id: &str| app.vps_forward.mode(id) == hopper_core::hosting::proxy_forward::Mode::Relay;
    Control { routes: cfg.servers.iter().filter_map(|id| app.servers.get(id).map(|e| Route { server: id.clone(), name: e.name.clone(), velocity: velocity(id), voice: has_voice(app, id) })).collect(), pings: false }
}

/// The server's voice mod points players at the relay (set with "Use the
/// relay", B6).
pub(super) fn has_voice(app: &App, id: &str) -> bool {
    let r = app.vps_forward.record(id);
    r.mode == hopper_core::hosting::proxy_forward::Mode::Relay && r.voice.as_ref().is_some_and(|v| v.kind.is_some())
}

/// The game port in a server's `server.properties` (25565 when unset).
pub fn server_port(root: &Path) -> u16 {
    hopper_core::hosting::properties::Properties::load(&root.join("server.properties"))
        .ok()
        .and_then(|p| p.get("server-port").and_then(|v| v.trim().parse().ok()))
        .unwrap_or(25565)
}

/// Where home sends voice packets: the voice mod of the first server on
/// the relay that has voice, on this PC. The relay passes voice for one
/// server, like the game port in B2 (per-server ports are B7).
fn voice_target(app: Arc<App>) -> super::voice::home::Target {
    Arc::new(move || {
        let cfg = app.vps.config();
        let id = cfg.servers.iter().find(|id| has_voice(&app, id))?;
        let e = app.servers.get(id)?;
        let d = hopper_core::hosting::voice_mods::detect(&e.root, server_port(&e.root))?;
        let ip = d.bind.filter(|b| !b.is_unspecified()).unwrap_or(std::net::IpAddr::from([127, 0, 0, 1]));
        Some(SocketAddr::new(ip, d.port))
    })
}

/// Runs a connection until it ends; says why.
async fn run(app: &Arc<App>, live: Live, cfg: &HomeConfig) -> String {
    let Live { tunnel, counters, exported } = live;
    let mux::Tunnel { handle, mut incoming, done, .. } = tunnel;
    app.vps.connected(handle.clone(), counters);
    let control = async {
        let Ok(s) = handle.open(serde_json::to_vec(&Head { kind: "control".into(), server: String::new(), peer: String::new() }).expect("head")).await else {
            return "Lost the connection to the relay.".to_string();
        };
        let mux::Stream { mut tx, rx, .. } = s;
        // The relay's answers (pings), read beside the loop below; aborted
        // with it.
        let answers = tokio::spawn(read_answers(app.clone(), rx));
        let _stop = AbortOnDrop(answers);
        let mut sent: Option<Control> = None;
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut n = 0u64;
        loop {
            // Turned off, or pointed at another relay: this connection ends
            // (the supervisor starts the new one).
            let now = app.vps.config();
            if !now.complete() || now.address != cfg.address || now.fingerprint != cfg.fingerprint || now.key != cfg.key {
                return SETTINGS_CHANGED.to_string();
            }
            // The servers it carries, again whenever they change.
            let r = routes(app);
            if sent.as_ref() != Some(&r) {
                let mut line = serde_json::to_vec(&r).expect("routes");
                line.push(b'\n');
                if tx.send(&line).await.is_err() {
                    return "Lost the connection to the relay.".to_string();
                }
                sent = Some(r);
            }
            tokio::select! {
                _ = app.vps.ask_pings.notified() => {
                    // The routes ride along, so a relay from before B5 reads
                    // this as the same routes again and changes nothing.
                    let ask = Control { pings: true, ..sent.clone().unwrap_or_default() };
                    let mut line = serde_json::to_vec(&ask).expect("routes");
                    line.push(b'\n');
                    if tx.send(&line).await.is_err() {
                        return "Lost the connection to the relay.".to_string();
                    }
                }
                _ = tick.tick() => {
                    n += 1;
                    app.vps.tick();
                    if n % 6 == 0 {
                        let a = app.clone();
                        let _ = tokio::task::spawn_blocking(move || a.vps.save_stats()).await;
                    }
                }
                _ = app.vps.wake.notified() => {}
            }
        }
    };
    // Voice beside the game streams (B6), while the relay has a voice port.
    let voice = async {
        let port = app.vps.status().voice_port;
        if let (Some(e), true) = (exported, port != 0) {
            let keys = super::voice::Keys::derive(&e, &link::key_hash(&cfg.key));
            let host = relay_target(&cfg.address).map(|(h, _)| h).unwrap_or_default();
            let loopback = hopper_core::paths::loopback_env() == Some(true);
            super::voice::home::run(app.vps.voice.clone(), handle.clone(), keys, (host, port), loopback, voice_target(app.clone())).await;
        }
        std::future::pending::<()>().await
    };
    let streams = async {
        while let Some(s) = incoming.recv().await {
            let app = app.clone();
            tokio::spawn(async move { stream(&app, s).await });
        }
    };
    let why = tokio::select! {
        r = done => match r {
            Ok(e) => super::health::mux_words(&e),
            Err(_) => "Lost the connection to the relay.".into(),
        },
        why = control => why,
        _ = streams => "Lost the connection to the relay.".into(),
        _ = voice => "Lost the connection to the relay.".into(),
    };
    handle.close("home is reconnecting").await;
    app.vps.disconnected();
    why
}

/// Ends a task when its owner goes.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Reads the relay's control lines (answers to ping asks) until the stream
/// ends. A relay from before B5 sends none.
async fn read_answers(app: Arc<App>, mut rx: mux::RecvHalf) {
    let mut lines = super::relay_end::Lines::default();
    while let Ok(Some(chunk)) = rx.recv().await {
        let Ok(got) = lines.push(&chunk) else { return };
        for line in got {
            if let Ok(l) = serde_json::from_slice::<super::RelayLine>(&line) {
                app.vps.got_pings(l);
            }
        }
    }
}

/// A player stream: checked against the servers set to use the relay, then
/// into the router for that server.
pub(super) async fn stream(app: &Arc<App>, s: mux::Stream) {
    let head: Option<Head> = serde_json::from_slice(&s.head).ok();
    let Some(head) = head.filter(|h| h.kind == "game") else { return };
    // Only servers set to use the relay: the relay can't reach any other.
    if !app.vps.config().servers.iter().any(|id| *id == head.server) || app.servers.get(&head.server).is_none() {
        log::info!("relay: refused a stream for {:?}, which isn't set to use the relay", head.server);
        return;
    }
    let live = app.vps.server_live(&head.server);
    live.open.fetch_add(1, Ordering::Relaxed);
    live.streams.fetch_add(1, Ordering::Relaxed);
    let peer: SocketAddr = head.peer.parse().unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
    let (mut near, far) = tokio::io::duplex(64 * 1024);
    let (a, id) = (app.clone(), head.server.clone());
    let routed = tokio::spawn(async move {
        if let Err(e) = crate::router::relayed(&a, &mut near, peer, &id).await {
            log::debug!("relay: {peer}: {e:#}");
        }
    });
    let mut van = VanillaEstimate::new();
    let (l1, l2) = (live.clone(), live.clone());
    let pipe = super::pipe(
        far,
        s,
        move |d| {
            l1.raw_down.fetch_add(d.len() as u64, Ordering::Relaxed);
            let before = van.vanilla();
            van.feed(d);
            l1.vanilla.fetch_add(van.vanilla() - before, Ordering::Relaxed);
        },
        move |d| {
            l2.raw_up.fetch_add(d.len() as u64, Ordering::Relaxed);
        },
    );
    tokio::pin!(pipe);
    let mut _player = Player(None);
    let r = tokio::select! {
        r = &mut pipe => r,
        _ = tokio::time::sleep(PLAYER_AFTER) => {
            live.players.fetch_add(1, Ordering::Relaxed);
            _player = Player(Some(live.clone()));
            pipe.await
        }
    };
    drop(_player);
    if let Err(e) = r {
        log::debug!("relay: stream from {peer}: {e}");
    }
    routed.abort();
    live.open.fetch_sub(1, Ordering::Relaxed);
}

// ---- API ---------------------------------------------------------------------------------

pub fn routes_api() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/vps-relay", get(get_config).put(put_config))
        .route("/api/v1/vps-relay/retry", post(retry))
        .route("/api/v1/servers/{id}/vps-relay", get(server_status))
        .route("/api/v1/servers/{id}/vps-relay/test", post(super::health::test_route))
}

fn config_json(app: &App) -> Value {
    let c = app.vps.config();
    json!({
        "enabled": c.enabled, "address": c.address, "fingerprint": c.fingerprint,
        // The key stays here: only whether there is one.
        "has_key": !c.key.is_empty(),
        "servers": c.servers, "public_address": c.public_address,
        "status": app.vps.status(),
    })
}

async fn get_config(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    Ok(Json(config_json(&app)))
}

#[derive(Deserialize)]
struct ConfigBody {
    enabled: Option<bool>,
    address: Option<String>,
    fingerprint: Option<String>,
    key: Option<String>,
    servers: Option<Vec<String>>,
    public_address: Option<String>,
}

fn clean_fingerprint(f: &str) -> Option<String> {
    let hex: String = f.chars().filter(|c| c.is_ascii_hexdigit()).map(|c| c.to_ascii_uppercase()).collect();
    let only_hex = f.chars().all(|c| c.is_ascii_hexdigit() || matches!(c, ':' | ' '));
    (hex.len() == 64 && only_hex).then(|| hex.as_bytes().chunks(2).map(|p| String::from_utf8_lossy(p).into_owned()).collect::<Vec<_>>().join(":"))
}

fn key_ok(k: &str) -> bool {
    k.strip_prefix(link::KEY_PREFIX).is_some_and(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Sets the pair by hand (B2; the ssh setup fills these in from B3).
/// Owner and managers only: it decides where players' traffic goes.
async fn put_config(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<ConfigBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let mut c = app.vps.config();
    if let Some(a) = b.address {
        let a = a.trim().to_string();
        if !a.is_empty() {
            relay_target(&a).map_err(|e| bad(format!("The relay's address: {e}")))?;
        }
        c.address = a;
    }
    if let Some(f) = b.fingerprint {
        c.fingerprint = if f.trim().is_empty() { String::new() } else { clean_fingerprint(&f).ok_or_else(|| bad("The fingerprint is 64 hex digits (AB:CD:…), as hopperd relay init prints it."))? };
    }
    if let Some(k) = b.key {
        let k = k.trim().to_string();
        if !k.is_empty() && !key_ok(&k) {
            return Err(bad("The relay key starts with hrt_ and has 64 hex digits after it."));
        }
        c.key = k;
    }
    if let Some(s) = b.servers {
        let mut clean = Vec::new();
        for id in s {
            if app.servers.get(&id).is_none() {
                return Err(not_found("server"));
            }
            if !clean.contains(&id) {
                clean.push(id);
            }
        }
        c.servers = clean;
    }
    if let Some(p) = b.public_address {
        let p = p.trim().to_string();
        if p.chars().count() > 255 || p.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
            return Err(bad("The players' address is one name or IP, with a port if it isn't 25565."));
        }
        c.public_address = p;
    }
    if let Some(e) = b.enabled {
        c.enabled = e;
    }
    app.vps.set_config(c).map_err(|e| bad(format!("{e:#}")))?;
    log::info!("relay: settings changed (address {:?}, {} server(s), {})", app.vps.config().address, app.vps.config().servers.len(), if app.vps.config().enabled { "on" } else { "off" });
    Ok(Json(config_json(&app)))
}

async fn retry(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    app.vps.wake.notify_one();
    Ok(Json(json!({ "ok": true })))
}

async fn server_status(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, UrlPath(id): UrlPath<String>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let mut v = app.vps.server_status(&id);
    if v["relayed"] == true {
        let (a, e2, set) = (app.clone(), e.clone(), has_voice(&app, &id));
        v["voice"] = tokio::task::spawn_blocking(move || a.vps.voice_json(&e2.root, set)).await.unwrap_or_default();
        let a = app.clone();
        v["forward"] = tokio::task::spawn_blocking(move || super::forward::status_json(&a, &e)).await.unwrap_or_default();
    }
    Ok(Json(v))
}
