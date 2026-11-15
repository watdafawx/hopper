//! Each online player's ping, for the Players tab.
//!
//! Players who join through the router: the router holds their own TCP
//! connection, so the OS's round-trip time for it is their ping (Windows
//! `SIO_TCP_INFO`, Linux `TCP_INFO`). It works with online-mode encryption,
//! sends nothing extra and needs no mod. Others: spark's `spark ping
//! --player` over RCON, when spark is installed and RCON is on. Nothing is
//! polled in the background: readings are taken when the Players tab asks.

use crate::servers::Entry;
use hopper_core::hosting::rcon;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
type Raw = std::os::windows::io::RawSocket;
#[cfg(unix)]
type Raw = std::os::fd::RawFd;
#[cfg(not(any(windows, unix)))]
type Raw = ();

/// A player connected through the router.
struct Conn {
    key: u64,
    server: String,
    name: String,
    raw: Raw,
    since: SystemTime,
}

fn conns() -> &'static Mutex<Vec<Conn>> {
    static C: OnceLock<Mutex<Vec<Conn>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Keeps a router connection listed while it lives. Drop it before the
/// socket: a reading takes the same lock, so it never uses a closed socket.
pub struct Listed(u64);

impl Drop for Listed {
    fn drop(&mut self) {
        conns().lock().unwrap().retain(|c| c.key != self.0);
    }
}

/// Lists `name`'s router connection to server `server`.
pub fn listen(server: &str, name: &str, sock: &tokio::net::TcpStream) -> Listed {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let key = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    #[cfg(windows)]
    let raw = std::os::windows::io::AsRawSocket::as_raw_socket(sock);
    #[cfg(unix)]
    let raw = std::os::fd::AsRawFd::as_raw_fd(sock);
    #[cfg(not(any(windows, unix)))]
    let raw = {
        let _ = sock;
    };
    conns().lock().unwrap().push(Conn { key, server: server.to_string(), name: name.to_string(), raw, since: SystemTime::now() });
    Listed(key)
}

/// The smoothed round-trip time the OS keeps for a connection, in ms.
#[cfg(windows)]
fn rtt_ms(raw: Raw) -> Option<u32> {
    use windows::Win32::Networking::WinSock::{SIO_TCP_INFO, SOCKET, TCP_INFO_v0, WSAIoctl};
    let version: u32 = 0;
    let mut info = TCP_INFO_v0::default();
    let mut got = 0u32;
    // SAFETY: the socket stays open while the caller holds the list's lock;
    // the buffers are sized for version 0 of the answer.
    let r = unsafe {
        WSAIoctl(
            SOCKET(raw as usize),
            SIO_TCP_INFO,
            Some(&version as *const u32 as *const _),
            4,
            Some(&mut info as *mut TCP_INFO_v0 as *mut _),
            std::mem::size_of::<TCP_INFO_v0>() as u32,
            &mut got,
            None,
            None,
        )
    };
    (r == 0 && info.RttUs > 0).then(|| info.RttUs.div_ceil(1000))
}

#[cfg(target_os = "linux")]
fn rtt_ms(raw: Raw) -> Option<u32> {
    let mut info: libc::tcp_info = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::tcp_info>() as libc::socklen_t;
    // SAFETY: as above; `info` and `len` describe a buffer of the right size.
    let r = unsafe { libc::getsockopt(raw, libc::IPPROTO_TCP, libc::TCP_INFO, &mut info as *mut _ as *mut libc::c_void, &mut len) };
    (r == 0 && info.tcpi_rtt > 0).then(|| info.tcpi_rtt.div_ceil(1000))
}

#[cfg(not(any(windows, target_os = "linux")))]
fn rtt_ms(_: Raw) -> Option<u32> {
    None
}

/// Router readings for a server's players: lowercase name -> (ms, since).
/// With two connections under one name, the older one counts: a second
/// login under a name already online is the one that hasn't been let in
/// (the server or the relay's Velocity refuses it, or it replaces the
/// first, which then closes).
fn router(server: &str) -> HashMap<String, (Option<u32>, SystemTime)> {
    let g = conns().lock().unwrap();
    let mut out: HashMap<String, (Option<u32>, SystemTime)> = HashMap::new();
    for c in g.iter().filter(|c| c.server == server) {
        let low = c.name.to_lowercase();
        if out.get(&low).is_some_and(|(_, since)| *since <= c.since) {
            continue;
        }
        out.insert(low, (rtt_ms(c.raw), c.since));
    }
    out
}

/// What the relay's front port measures for `server`'s players, sent home
/// on the control stream (design/vps-relay.md, "B5 as built"): lowercase
/// name -> ms. The relay runs no servers, so every connection listed there
/// is a player's socket on the VPS.
pub fn readings(server: &str) -> HashMap<String, u32> {
    router(server).into_iter().filter_map(|(n, (ms, _))| ms.map(|ms| (n, ms))).collect()
}

/// spark readings, kept a few seconds so a tab open in two places doesn't
/// ask twice: server id -> (when, lowercase name -> ms).
fn spark_cache() -> &'static Mutex<HashMap<String, (Instant, HashMap<String, u32>)>> {
    static C: OnceLock<Mutex<HashMap<String, (Instant, HashMap<String, u32>)>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Asks spark for `names`' pings, one RCON command each.
fn spark(e: &Entry, names: &[String]) -> HashMap<String, u32> {
    if let Some((at, m)) = spark_cache().lock().unwrap().get(&e.id)
        && at.elapsed() < Duration::from_secs(4)
    {
        return m.clone();
    }
    let mut out = HashMap::new();
    for n in names {
        match crate::alerts::rcon_once(e, &format!("spark ping --player {n}")) {
            Some(text) => {
                if let Some(ms) = rcon::parse_spark_ping(&text) {
                    out.insert(n.to_lowercase(), ms);
                }
            }
            // RCON is down or the server is stopping: don't try the rest.
            None => break,
        }
    }
    spark_cache().lock().unwrap().insert(e.id.clone(), (Instant::now(), out.clone()));
    out
}

fn unix(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Each online player with their ping (`ms`, `null` with `why` when there's
/// no way to read it) and when they joined (unix seconds, when known).
pub async fn people(app: &crate::App, e: &Arc<Entry>) -> Value {
    let online = e.server.players();
    if online.is_empty() {
        return json!([]);
    }
    let joined = e.server.joined_at();
    let routed = router(&e.id);
    // Players who came through a relay on a VPS: the relay's front port
    // holds their own connection, so it measures them there (to the VPS),
    // and the tunnel's round trip adds the way from the VPS to here.
    let relay = if online.iter().any(|n| !matches!(routed.get(&n.to_lowercase()), Some((Some(_), _)))) { app.vps.relay_pings(&e.id).await } else { None };
    let relayed = |low: &str| relay.as_ref().and_then(|(m, _)| m.get(low).copied());
    let missing: Vec<String> = online.iter().filter(|n| !matches!(routed.get(&n.to_lowercase()), Some((Some(_), _))) && relayed(&n.to_lowercase()).is_none()).cloned().collect();
    let has_spark = crate::alerts::has_spark(&e.root);
    let rcon_on = rcon::settings(&e.root).is_some();
    let mut sparked = HashMap::new();
    if !missing.is_empty() && has_spark && rcon_on && e.server.rcon_allowed() {
        static ASKING: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
        let asking = ASKING.get_or_init(Default::default);
        if asking.lock().unwrap().insert(e.id.clone()) {
            let (e2, names) = (e.clone(), missing.clone());
            let job = tokio::task::spawn_blocking(move || {
                let m = spark(&e2, &names);
                asking.lock().unwrap().remove(&e2.id);
                m
            });
            // A lagging server answers late: show the rest now, spark's
            // reading next time (it's cached when it lands).
            if let Ok(Ok(m)) = tokio::time::timeout(Duration::from_millis(1500), job).await {
                sparked = m;
            }
        } else if let Some((_, m)) = spark_cache().lock().unwrap().get(&e.id) {
            sparked = m.clone();
        }
    }
    let why = if has_spark && !rcon_on {
        "Turn on RCON to see ping from spark."
    } else if has_spark {
        "spark has no ping for them yet."
    } else {
        "Connect through Hopper's address to see ping, or install spark."
    };
    let list: Vec<Value> = online
        .iter()
        .map(|n| {
            let low = n.to_lowercase();
            let r = routed.get(&low);
            let front = relayed(&low);
            let home = relay.as_ref().and_then(|(_, h)| *h);
            let (ms, via) = match (r.and_then(|r| r.0), front, sparked.get(&low)) {
                (Some(ms), _, _) => (Some(ms), "router"),
                // What the player feels: to the VPS, then on to here.
                (None, Some(ms), _) => (Some(ms + home.unwrap_or(0)), "relay"),
                (None, None, Some(ms)) => (Some(*ms), "spark"),
                _ => (None, ""),
            };
            let since = joined.get(n).copied().or(r.map(|r| r.1));
            let mut v = json!({
                "name": n,
                "ms": ms,
                "via": via,
                "why": if ms.is_none() { Some(why) } else { None },
                "joined": since.map(unix),
            });
            // Both parts, for the tooltip.
            if via == "relay" {
                v["relay_ms"] = json!(front);
                v["home_ms"] = json!(home);
            }
            v
        })
        .collect();
    json!(list)
}
