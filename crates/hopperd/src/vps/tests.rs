//! Tests for "Relay through a VPS" (B2): the key check, the stream protocol
//! over the codec, the relay's own answers, and the whole path from a
//! player on the relay's port to home's router. Everything runs on
//! loopback or in memory.

use std::sync::Arc;
use std::time::Duration;

use hopper_core::hosting::router::{self as core, Read};
use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use super::link::{self, LinkError, Live};
use super::relay_end::{self, Lines, Relay};
use super::wire::Codec;
use super::{Head, home};
use crate::tunnel::mux::{self, StreamError};

pub(super) fn key() -> String {
    format!("hrt_{}", "ab".repeat(32))
}

/// A small busy direction, cache on, so the tests exercise it cheaply.
fn small() -> Codec {
    Codec { level: 3, window_log: 20, long_distance: false, cache_bytes: 1 << 20 }
}

/// Pings often (so a round trip is measured early), but gives a quiet link
/// the normal time before calling it dead: every test that cuts the link
/// does it by closing the connection, which ends it at once, and a short
/// "dead" time only made a busy test machine drop tunnels mid-test.
pub(super) fn quick() -> mux::Config {
    mux::Config { ping_every: Duration::from_millis(300), ..link::mux_config() }
}

fn head(kind: &str, server: &str) -> Vec<u8> {
    serde_json::to_vec(&Head { kind: kind.into(), server: server.into(), peer: "203.0.113.9:50000".into() }).unwrap()
}

/// Relay and home over an in-memory connection the test can cut.
pub(super) struct Pair {
    pub(super) relay: Live,
    pub(super) home: Live,
    pub(super) cut: JoinHandle<()>,
}

pub(super) async fn pair(cfg: mux::Config) -> Pair {
    let (r_io, r_far) = tokio::io::duplex(256 * 1024);
    let (h_io, h_far) = tokio::io::duplex(256 * 1024);
    let cut = tokio::spawn(async move {
        let (mut a, mut b) = (r_far, h_far);
        let _ = tokio::io::copy_bidirectional(&mut a, &mut b).await;
    });
    let (hash, k) = (link::key_hash(&key()), key());
    let (relay, home) = tokio::join!(link::relay(r_io, &hash, small(), (25565, 0), cfg.clone()), link::home(h_io, &k, Codec::quiet(), cfg));
    Pair { relay: relay.expect("relay end"), home: home.expect("home end").0, cut }
}

fn pattern(seed: u8, len: usize) -> Vec<u8> {
    (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
}

// ---- the stream protocol ----------------------------------------------------------------

/// A stream opens with its head, each side can finish on its own and the
/// other still sends (half-close), and both see the end.
#[tokio::test]
async fn stream_opens_half_closes_and_closes() {
    let mut p = pair(quick()).await;
    let mut s = p.relay.tunnel.handle.open(head("game", "srv")).await.unwrap();
    s.tx.send(b"hello").await.unwrap();
    s.tx.finish().await.unwrap();
    let mut h = p.home.tunnel.incoming.recv().await.expect("a stream at home");
    let got: Head = serde_json::from_slice(&h.head).unwrap();
    assert_eq!((got.kind.as_str(), got.server.as_str()), ("game", "srv"));
    assert_eq!(h.rx.read_all().await.unwrap(), b"hello");
    // The relay finished sending; home can still answer.
    h.tx.send(b"world").await.unwrap();
    h.tx.finish().await.unwrap();
    assert_eq!(s.rx.read_all().await.unwrap(), b"world");
}

/// A hundred streams at once, each echoed back unchanged through the
/// codec both ways.
#[tokio::test]
async fn many_streams_echo_unchanged() {
    let p = pair(quick()).await;
    let mut incoming = p.home.tunnel.incoming;
    let echo = tokio::spawn(async move {
        while let Some(mut s) = incoming.recv().await {
            tokio::spawn(async move {
                while let Ok(Some(d)) = s.rx.recv().await {
                    s.tx.send(&d).await.unwrap();
                }
                s.tx.finish().await.unwrap();
            });
        }
    });
    let mut runs = Vec::new();
    for i in 0..100u8 {
        let handle = p.relay.tunnel.handle.clone();
        runs.push(tokio::spawn(async move {
            let data = pattern(i, 40_000 + i as usize * 97);
            let s = handle.open(head("game", "srv")).await.unwrap();
            let (mut tx, mut rx) = (s.tx, s.rx);
            let sent = data.clone();
            let w = tokio::spawn(async move {
                for c in sent.chunks(7_000) {
                    tx.send(c).await.unwrap();
                }
                tx.finish().await.unwrap();
            });
            let back = rx.read_all().await.unwrap();
            w.await.unwrap();
            assert_eq!(back, data, "stream {i}");
        }));
    }
    for r in runs {
        tokio::time::timeout(Duration::from_secs(30), r).await.expect("in time").unwrap();
    }
    // Repeated data went through the cache or zstd: far less on the wire.
    let c = p.relay.counters.get();
    assert!(c.received.raw > 4_000_000 && c.received.wire < c.received.raw / 4, "{c:?}");
    echo.abort();
}

/// A stream whose reader stops holds its sender at the window, and other
/// streams keep flowing meanwhile.
#[tokio::test]
async fn a_stalled_stream_waits_and_others_flow() {
    let mut p = pair(quick()).await;
    let mut stalled = p.relay.tunnel.handle.open(head("game", "a")).await.unwrap();
    let _held = p.home.tunnel.incoming.recv().await.unwrap();
    let big = vec![7u8; 4 << 20];
    let push = async {
        for c in big.chunks(32 * 1024) {
            stalled.tx.send(c).await.unwrap();
        }
    };
    assert!(tokio::time::timeout(Duration::from_millis(1500), push).await.is_err(), "4 MB went out with nobody reading");
    let mut other = p.relay.tunnel.handle.open(head("game", "b")).await.unwrap();
    other.tx.send(b"still moving").await.unwrap();
    other.tx.finish().await.unwrap();
    let mut h = tokio::time::timeout(Duration::from_secs(5), p.home.tunnel.incoming.recv()).await.unwrap().unwrap();
    assert_eq!(tokio::time::timeout(Duration::from_secs(5), h.rx.read_all()).await.unwrap().unwrap(), b"still moving");
}

/// Cutting the connection ends every stream on both sides (no stream
/// hangs) and both tunnels report that they're done.
#[tokio::test]
async fn a_dead_connection_drops_streams() {
    let mut p = pair(quick()).await;
    let mut s = p.relay.tunnel.handle.open(head("game", "srv")).await.unwrap();
    s.tx.send(b"x").await.unwrap();
    let mut h = p.home.tunnel.incoming.recv().await.unwrap();
    assert_eq!(h.rx.recv().await.unwrap().unwrap(), b"x");
    p.cut.abort();
    let relay_end = tokio::time::timeout(Duration::from_secs(5), s.rx.recv()).await.expect("relay stream ended");
    let home_end = tokio::time::timeout(Duration::from_secs(5), h.rx.recv()).await.expect("home stream ended");
    assert!(matches!(relay_end, Err(StreamError::Closed) | Err(StreamError::Reset(_))), "{relay_end:?}");
    assert!(matches!(home_end, Err(StreamError::Closed) | Err(StreamError::Reset(_))), "{home_end:?}");
    tokio::time::timeout(Duration::from_secs(5), p.relay.tunnel.done).await.expect("relay tunnel done").unwrap();
    tokio::time::timeout(Duration::from_secs(5), p.home.tunnel.done).await.expect("home tunnel done").unwrap();
}

// ---- the key ---------------------------------------------------------------------------

#[tokio::test]
async fn a_wrong_key_is_refused() {
    let (r_io, h_io) = tokio::io::duplex(64 * 1024);
    let other = link::key_hash(&format!("hrt_{}", "cd".repeat(32)));
    let k = key();
    let (relay, home) = tokio::join!(link::relay(r_io, &other, small(), (25565, 0), quick()), link::home(h_io, &k, Codec::quiet(), quick()));
    assert!(matches!(relay, Err(LinkError::Refused(_))));
    match home {
        Err(LinkError::Refused(m)) => assert_eq!(m, "The relay key is wrong."),
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("a wrong key was let in"),
    }
}

/// Before the key, the relay reads one hello and nothing else: frames sent
/// after a wrong key aren't read as frames, and the connection closes after
/// one "no". A broken hello gets no answer at all.
#[tokio::test]
async fn nothing_is_read_before_the_key() {
    let (r_io, mut c) = tokio::io::duplex(64 * 1024);
    let relay = tokio::spawn(async move { link::relay(r_io, &link::key_hash(&key()), small(), (25565, 0), quick()).await });
    let hello = serde_json::to_vec(&serde_json::json!({ "key": "hrt_wrong", "tunnel": quick().tunnel, "version": "x", "recv": Codec::quiet() })).unwrap();
    c.write_all(&(hello.len() as u32).to_be_bytes()).await.unwrap();
    c.write_all(&hello).await.unwrap();
    // What a mux would send next: never read.
    c.write_all(&[0, 0, 0, 9, 1, 0, 0, 0, 2, 0, 0, 0, 0]).await.unwrap();
    assert!(matches!(relay.await.unwrap(), Err(LinkError::Refused(_))));
    let mut answer = Vec::new();
    c.read_to_end(&mut answer).await.unwrap();
    let len = u32::from_be_bytes(answer[..4].try_into().unwrap()) as usize;
    assert_eq!(answer.len(), 4 + len, "one answer and then the end");
    let no: Value = serde_json::from_slice(&answer[4..]).unwrap();
    assert_eq!(no["ok"], false);

    let (r_io, mut c) = tokio::io::duplex(64 * 1024);
    let relay = tokio::spawn(async move { link::relay(r_io, &link::key_hash(&key()), small(), (25565, 0), quick()).await });
    c.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
    assert!(matches!(relay.await.unwrap(), Err(LinkError::Protocol(_))));
    let mut rest = Vec::new();
    c.read_to_end(&mut rest).await.unwrap();
    assert!(rest.is_empty(), "answered a broken hello: {rest:?}");
}

#[test]
fn keys_and_hashes() {
    let k = link::new_key();
    assert!(k.starts_with("hrt_") && k.len() == 68, "{k}");
    let h = link::key_hash(&k);
    assert_eq!(link::hash_from_hex(&link::hash_hex(&h)), Some(h));
    assert_eq!(link::key_hash(&format!(" {k}\n")), h, "pasted with spaces");
    assert_eq!(link::hash_from_hex("abc"), None);
    assert_ne!(link::key_hash(&link::new_key()), h);
}

#[test]
fn control_lines_split_and_cap() {
    let mut l = Lines::default();
    assert_eq!(l.push(b"{\"a\":1}\n{\"b\"").unwrap(), vec![b"{\"a\":1}".to_vec()]);
    assert_eq!(l.push(b":2}\n").unwrap(), vec![b"{\"b\":2}".to_vec()]);
    assert!(l.push(&vec![b'x'; super::MAX_CONTROL_LINE + 1]).is_err(), "an endless line");
}

#[test]
fn relay_addresses() {
    assert_eq!(home::relay_target("relay.example.com").unwrap(), ("relay.example.com".into(), 25566));
    assert_eq!(home::relay_target("203.0.113.5:4000").unwrap(), ("203.0.113.5".into(), 4000));
    assert_eq!(home::relay_target("[2001:db8::1]").unwrap().1, 25566);
    assert_eq!(home::relay_target("[2001:db8::1]:7").unwrap().1, 7);
}

// ---- the relay process, over TLS ---------------------------------------------------------

struct RelayRun {
    relay: Arc<Relay>,
    game: u16,
    /// Velocity's first backend port (0: none).
    backend: u16,
    tunnel: u16,
    fingerprint: String,
    task: JoinHandle<()>,
    dir: std::path::PathBuf,
    /// The port just above the first backend port, kept bound so another
    /// test can't take it (the relay hands out the ports above the first
    /// one). A test that wants the relay to listen there gives it up with
    /// [`RelayRun::free_next_backend`].
    spare: Option<TcpListener>,
}

impl RelayRun {
    fn free_next_backend(&mut self) {
        self.spare = None;
    }
}

impl Drop for RelayRun {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn run_relay() -> RelayRun {
    run_relay_with(false).await
}

/// `backends`: also listen for Velocity on a loopback port (B7).
async fn run_relay_with(backends: bool) -> RelayRun {
    let dir = std::env::temp_dir().join(format!("hopperd-vps-{}-{}", std::process::id(), crate::auth::random_hex(4)));
    std::fs::create_dir_all(&dir).unwrap();
    let (tls, fingerprint) = relay_end::tls_config(&dir).unwrap();
    let mut relay = Relay::new(link::key_hash(&key()), small(), 0);
    Arc::get_mut(&mut relay).unwrap().mux = quick();
    let game = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tunnel = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (gp, tp) = (game.local_addr().unwrap().port(), tunnel.local_addr().unwrap().port());
    // The second backend port is the next one up, so it has to be free
    // too: with the other tests binding ports at the same time, take a
    // pair or try again.
    let (backend, spare) = if backends {
        let mut got = None;
        for _ in 0..50 {
            let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let next = l.local_addr().unwrap().port().checked_add(1);
            if let Some(Ok(n)) = next.map(|p| std::net::TcpListener::bind(("127.0.0.1", p))) {
                n.set_nonblocking(true).unwrap();
                got = Some((Some(l), Some(TcpListener::from_std(n).unwrap())));
                break;
            }
        }
        got.expect("two neighbouring free ports")
    } else {
        (None, None)
    };
    let bp = backend.as_ref().map_or(0, |l| l.local_addr().unwrap().port());
    let task = tokio::spawn(relay_end::serve(relay.clone(), game, tunnel, tls, backend));
    RelayRun { relay, game: gp, backend: bp, tunnel: tp, fingerprint, task, dir, spare }
}

fn home_cfg(r: &RelayRun, port: u16, server: &str) -> home::HomeConfig {
    home::HomeConfig { enabled: true, address: format!("127.0.0.1:{port}"), fingerprint: r.fingerprint.clone(), key: key(), servers: vec![server.into()], ..Default::default() }
}

/// A server list ping (status, then ping): the status JSON and whether the
/// pong came back with the same payload.
async fn list_ping(port: u16) -> (Value, bool) {
    list_ping_as(port, "localhost").await
}

/// [`list_ping`] for the address `host`.
async fn list_ping_as(port: u16, host: &str) -> (Value, bool) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut out = core::handshake_packet(767, host, port, 1);
    out.extend(core::packet(0, &[]));
    s.write_all(&out).await.unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let status = loop {
        if let Read::Done((id, body), used) = core::read_packet(&buf, 1 << 20).unwrap() {
            assert_eq!(id, 0);
            let (text, _) = core::read_string(body, 1 << 20).unwrap();
            buf.drain(..used);
            break serde_json::from_str::<Value>(&text).unwrap();
        }
        let n = tokio::time::timeout(Duration::from_secs(10), s.read(&mut chunk)).await.expect("status in time").unwrap();
        assert!(n > 0, "closed before the status");
        buf.extend_from_slice(&chunk[..n]);
    };
    s.write_all(&core::packet(1, &42i64.to_be_bytes())).await.unwrap();
    let pong = loop {
        if let Read::Done((id, body), _) = core::read_packet(&buf, 64).unwrap() {
            break id == 1 && body == 42i64.to_be_bytes();
        }
        let Ok(Ok(n)) = tokio::time::timeout(Duration::from_secs(10), s.read(&mut chunk)).await else { break false };
        if n == 0 {
            break false;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    (status, pong)
}

/// With no home connected the relay answers the server list itself and
/// tells a joining player; nothing goes anywhere else.
#[tokio::test(flavor = "multi_thread")]
async fn relay_answers_alone_while_home_is_away() {
    let r = run_relay().await;
    let (status, pong) = list_ping(r.game).await;
    assert_eq!(status["description"]["text"], "Home server is offline");
    assert!(pong);
    let mut s = TcpStream::connect(("127.0.0.1", r.game)).await.unwrap();
    s.write_all(&core::handshake_packet(767, "localhost", r.game, 2)).await.unwrap();
    let mut got = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut got)).await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&got).contains("The home server is offline"));
}

/// TLS pins the relay's CA and the key decides the rest.
#[tokio::test(flavor = "multi_thread")]
async fn home_needs_the_fingerprint_and_the_key() {
    let r = run_relay().await;
    let mut c = home_cfg(&r, r.tunnel, "srv");
    let ok = home::connect(&c).await;
    assert!(ok.is_ok(), "{:?}", ok.err());
    c.key = format!("hrt_{}", "cd".repeat(32));
    assert_eq!(home::connect(&c).await.err().as_deref(), Some("The relay key is wrong."));
    c.key = key();
    c.fingerprint = ["00"; 32].join(":");
    let e = home::connect(&c).await.err().unwrap();
    assert!(e.contains("fingerprint"), "{e}");
}

/// Ten failed tries from one address and the relay stops reading it.
#[tokio::test(flavor = "multi_thread")]
async fn failed_tries_are_limited() {
    let r = run_relay().await;
    let mut c = home_cfg(&r, r.tunnel, "srv");
    c.key = format!("hrt_{}", "cd".repeat(32));
    for _ in 0..10 {
        let _ = home::connect(&c).await;
    }
    c.key = key();
    assert!(home::connect(&c).await.is_err(), "still let in after 10 failures");
}

// ---- home: the router and the whole path -----------------------------------------------

/// Home takes only game streams for a server set to use the relay, and
/// that server's router answers them (tagged by the relay, not guessed from
/// the address the player typed).
#[tokio::test(flavor = "multi_thread")]
async fn home_routes_by_tag_and_refuses_others() {
    let t = crate::cross_origin_tests::panel_with(crate::TlsInfo { fingerprint: "AA:BB".into(), ca_pem: String::new() });
    let r = run_relay().await;
    t.app.vps.set_config(home_cfg(&r, r.tunnel, &t.server)).unwrap();
    let p = pair(quick()).await;
    let (relay, mut incoming) = (p.relay.tunnel.handle.clone(), p.home.tunnel.incoming);
    let app = t.app.clone();
    tokio::spawn(async move {
        while let Some(s) = incoming.recv().await {
            let app = app.clone();
            tokio::spawn(async move { home::stream(&app, s).await });
        }
    });
    let ask = |server: &'static str, kind: &'static str| {
        let h = relay.clone();
        async move {
            let mut s = h.open(head(kind, server)).await.unwrap();
            // The address names no server: the tag decides.
            let mut out = core::handshake_packet(767, "nothing.example", 25565, 1);
            out.extend(core::packet(0, &[]));
            // A rejected stream can reset before DATA is sent.
            s.tx.send(&out).await?;
            // The first answer, or the stream's end.
            tokio::time::timeout(Duration::from_secs(30), s.rx.recv()).await.expect("an answer or the end")
        }
    };
    let id: &'static str = Box::leak(t.server.clone().into_boxed_str());
    let answered = ask(id, "game").await.unwrap().expect("the router answered the server list");
    let (status, _) = match core::read_packet(&answered, 1 << 20).unwrap() {
        Read::Done((0, body), _) => core::read_string(body, 1 << 20).unwrap(),
        _ => panic!("not a status packet: {answered:?}"),
    };
    assert!(serde_json::from_str::<Value>(&status).unwrap().get("description").is_some(), "{status}");
    assert!(!matches!(ask("someone-else", "game").await, Ok(Some(_))), "a server not set to use the relay");
    assert!(!matches!(ask(id, "control").await, Ok(Some(_))), "a non-game stream from the relay");
}

/// A TCP proxy in front of the relay's tunnel port that the test can switch
/// off (standing in for the VPS going away: open connections are cut and
/// new ones closed at once) and on again. It keeps its port the whole time:
/// a port handed back to the OS and bound again later can be taken by
/// another socket in between, which made the reconnect test fail now and
/// then on a busy machine.
struct Proxy {
    port: u16,
    on: Arc<tokio::sync::watch::Sender<bool>>,
    task: JoinHandle<()>,
}

impl Proxy {
    fn set(&self, on: bool) {
        self.on.send_replace(on);
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn proxy(to: u16) -> Proxy {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let on = Arc::new(tokio::sync::watch::channel(true).0);
    let o = on.clone();
    let task = tokio::spawn(async move {
        let mut set = tokio::task::JoinSet::new();
        loop {
            let Ok((mut a, _)) = l.accept().await else { continue };
            if !*o.borrow() {
                continue;
            }
            let mut off = o.subscribe();
            set.spawn(async move {
                if let Ok(mut b) = TcpStream::connect(("127.0.0.1", to)).await {
                    tokio::select! {
                        _ = tokio::io::copy_bidirectional(&mut a, &mut b) => {}
                        _ = off.wait_for(|on| !*on) => {}
                    }
                }
            });
        }
    });
    Proxy { port, on, task }
}

async fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let until = std::time::Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(std::time::Instant::now() < until, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// End to end: home's supervisor connects, a server list ping on the
/// relay's port is answered by home's router for the tagged server, the
/// counters move; the relay going away shows "down" and the relay answers
/// alone; it coming back reconnects by itself.
#[tokio::test(flavor = "multi_thread")]
async fn whole_path_and_reconnect() {
    let t = crate::cross_origin_tests::panel_with(crate::TlsInfo { fingerprint: "AA:BB".into(), ca_pem: String::new() });
    let r = run_relay().await;
    let px = proxy(r.tunnel).await;
    // Players' address: the relay's game port here (the health test pings it).
    let cfg = home::HomeConfig { public_address: format!("127.0.0.1:{}", r.game), ..home_cfg(&r, px.port, &t.server) };
    t.app.vps.set_config(cfg).unwrap();
    home::start(t.app.clone());
    wait_for("the link", 15, || r.relay.ready()).await;
    assert_eq!(t.app.vps.status().state, "up");
    // The health test: connected, and a player's server list ping on the
    // players' address is answered by home through the relay.
    let h = super::health::test(&t.app, &t.server).await;
    assert_eq!(h["ok"], true, "{h}");
    assert_eq!(h["rows"][0]["id"], "link");
    assert_eq!(h["rows"][1]["state"], "ok", "{h}");
    assert_eq!(t.app.vps.server_status(&t.server)["test"]["ok"], true);

    let (status, pong) = list_ping(r.game).await;
    assert_ne!(status["description"]["text"], "Home server is offline", "{status}");
    assert_ne!(status["version"]["name"], "Hopper relay", "answered by the relay, not home: {status}");
    assert!(pong, "the ping went through and back");
    t.app.vps.tick();
    let s = t.app.vps.server_status(&t.server);
    assert_eq!(s["relayed"], true);
    assert_eq!(s["state"], "up");
    assert!(s["connection"]["raw"].as_u64().unwrap() > 0, "{s}");
    assert!(s["total"]["streams"].as_u64().unwrap() >= 1, "{s}");
    // A server list ping isn't a player.
    assert_eq!(s["players"], 0, "{s}");

    // The VPS goes away.
    px.set(false);
    wait_for("down", 20, || t.app.vps.status().state == "down" && !r.relay.ready()).await;
    let (status, _) = list_ping(r.game).await;
    assert_eq!(status["description"]["text"], "Home server is offline");
    let s = t.app.vps.server_status(&t.server);
    assert_eq!(s["state"], "down");
    assert!(!s["error"].as_str().unwrap().is_empty());
    // The test tries once more, then says the relay is down and players
    // reach only the relay's "Home server is offline".
    let h = super::health::test(&t.app, &t.server).await;
    assert_eq!(h["ok"], false, "{h}");
    assert_eq!(h["rows"][0]["state"], "fail", "{h}");
    assert_eq!(h["rows"][1]["state"], "fail", "{h}");
    assert!(h["rows"][1]["text"].as_str().unwrap().contains("Home server is offline"), "{h}");

    // And comes back on the same address. "Try now" (the card's button)
    // instead of waiting out the backoff, which may have grown on a slow
    // machine while the relay was away.
    px.set(true);
    t.app.vps.wake();
    wait_for("up again", 30, || t.app.vps.status().state == "up" && r.relay.ready()).await;
    let (status, pong) = list_ping(r.game).await;
    assert_ne!(status["version"]["name"], "Hopper relay");
    assert!(pong);
    // Totals carry over the reconnect.
    t.app.vps.tick();
    assert!(t.app.vps.server_status(&t.server)["total"]["streams"].as_u64().unwrap() >= 2);
}

#[test]
fn proxy_header_carries_the_players_address() {
    let sig = [0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A];
    let h = relay_end::proxy_v2("203.0.113.7:51234".parse().unwrap(), "127.0.0.1:25565".parse().unwrap());
    assert_eq!(&h[..12], &sig);
    assert_eq!(&h[12..16], &[0x21, 0x11, 0, 12]);
    assert_eq!(&h[16..20], &[203, 0, 113, 7]);
    assert_eq!(&h[20..24], &[127, 0, 0, 1]);
    assert_eq!(&h[24..28], &[0xC8, 0x22, 0x63, 0xDD]);
    assert_eq!(h.len(), 28);
    // A v6 player on a v4 listener: both as IPv6, the v4 one mapped.
    let h = relay_end::proxy_v2("[2001:db8::1]:40000".parse().unwrap(), "10.0.0.2:25565".parse().unwrap());
    assert_eq!(&h[12..16], &[0x21, 0x21, 0, 36]);
    assert_eq!(&h[16..18], &[0x20, 0x01]);
    assert_eq!(&h[32..48], &"::ffff:10.0.0.2".parse::<std::net::Ipv6Addr>().unwrap().octets());
    assert_eq!(h.len(), 16 + 36);
}

// ---- B5: players' ping from the relay's front port ----------------------------------------

/// The ping ask still carries the routes, so a relay from before B5 (which
/// reads every line as routes) changes nothing; a routes line doesn't ask.
#[test]
fn ping_ask_keeps_the_routes() {
    let routes = vec![super::Route { server: "s1".into(), name: "One".into(), velocity: true, ..Default::default() }];

    let ask = serde_json::to_value(super::Control { routes: routes.clone(), pings: true }).unwrap();
    assert_eq!(ask["pings"], true);
    #[derive(serde::Deserialize)]
    struct Before {
        routes: Vec<super::Route>,
    }
    let old: Before = serde_json::from_value(ask).unwrap();
    assert_eq!(old.routes, routes);
    let plain = serde_json::to_string(&super::Control { routes, pings: false }).unwrap();
    assert!(!plain.contains("pings"), "{plain}");
}

/// Home asks over the control stream and the relay answers with what its
/// front port lists for that server's players. (Both ends run in this test
/// process, so the player's socket is listed here directly; the relay
/// reading Login Start is checked against a real client on the test VPS.)
#[tokio::test(flavor = "multi_thread")]
async fn home_gets_the_relays_ping_readings() {
    let t = crate::cross_origin_tests::panel_with(crate::TlsInfo { fingerprint: "AA:BB".into(), ca_pem: String::new() });
    let r = run_relay().await;
    t.app.vps.set_config(home_cfg(&r, r.tunnel, &t.server)).unwrap();
    home::start(t.app.clone());
    wait_for("the link", 15, || r.relay.ready()).await;
    // A player's connection, with some traffic so the OS has a round trip.
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut player = TcpStream::connect(l.local_addr().unwrap()).await.unwrap();
    let (mut front, _) = l.accept().await.unwrap();
    for _ in 0..20 {
        front.write_all(&[1; 64]).await.unwrap();
        let mut b = [0u8; 64];
        player.read_exact(&mut b).await.unwrap();
        player.write_all(&b).await.unwrap();
        front.read_exact(&mut b).await.unwrap();
    }
    let _listed = crate::player_ping::listen(&t.server, "Steve", &front);
    let (pings, _) = t.app.vps.relay_pings(&t.server).await.expect("the server uses the relay and it's up");
    assert!(t.app.vps.has_relay_answer(), "the relay answered the ask");
    // Loopback can round-trip in under a microsecond, which reads as no
    // value; when there is one it's under the name, lowercase.
    if let Some(ms) = pings.get("steve") {
        assert!(*ms < 1000, "{ms}");
    }
    assert!(t.app.vps.relay_pings("not-relayed").await.is_none());
}

// ---- B7: several servers behind one relay ------------------------------------------------

fn route(server: &str, hosts: &[&str], velocity: bool, port: u16) -> super::Route {
    super::Route { server: server.into(), name: server.into(), velocity, hosts: hosts.iter().map(|h| h.to_string()).collect(), port, ..Default::default() }
}

/// The address a player typed picks the server; one nobody lists is
/// refused. One server, or a home from before B7, takes every address.
#[test]
fn the_address_picks_the_server() {
    use super::pick_route;
    let both = [route("a", &["dg.example.com"], false, 0), route("b", &["build.example.com"], false, 0)];
    assert_eq!(pick_route(&both, "dg.example.com").unwrap().server, "a");
    assert_eq!(pick_route(&both, "BUILD.example.com.").unwrap().server, "b");
    assert_eq!(pick_route(&both, "build.example.com\0FML3\0").unwrap().server, "b");
    assert!(pick_route(&both, "other.example.com").is_none());
    assert!(pick_route(&both, "203.0.113.5").is_none());
    assert_eq!(pick_route(&both[..1], "203.0.113.5").unwrap().server, "a");
    let old = [route("a", &[], false, 0), route("b", &[], false, 0)];
    assert_eq!(pick_route(&old, "anything").unwrap().server, "a");
    assert!(pick_route(&[], "x").is_none());
}

/// Each Velocity port carries one server; a port nobody has, or a server
/// Velocity doesn't know yet, goes nowhere.
#[test]
fn velocity_ports_tag_their_server() {
    use super::{backend_route, velocity_knows};
    let base = 25580;
    let rs = [route("a", &["a.x"], true, base), route("b", &["b.x"], true, base + 1), route("c", &["c.x"], false, 0)];
    assert_eq!(backend_route(&rs, base, base).unwrap().server, "a");
    assert_eq!(backend_route(&rs, base + 1, base).unwrap().server, "b");
    assert!(backend_route(&rs, base + 2, base).is_none());
    assert!(velocity_knows(&rs, &rs[1]) && !velocity_knows(&rs, &rs[2]));
    // Set to check accounts through the relay, but not in Velocity yet.
    let new = [route("a", &["a.x"], true, base), route("d", &["d.x"], true, 0)];
    assert!(!velocity_knows(&new, &new[1]));
    // Before B7: one Velocity server, no ports, and the first port is its.
    let old = [route("a", &[], true, 0), route("c", &[], false, 0)];
    assert!(velocity_knows(&old, &old[0]));
    assert_eq!(backend_route(&old, base, base).unwrap().server, "a");
    assert!(backend_route(&old, base + 1, base).is_none());
    // Two Velocity servers and no ports: nothing tells them apart.
    let two = [route("a", &[], true, 0), route("b", &[], true, 0)];
    assert!(!velocity_knows(&two, &two[0]) && backend_route(&two, base, base).is_none());
}

/// Exact names only, in the form Velocity compares; the address states.
#[test]
fn relay_addresses_and_their_state() {
    let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(home::exact_hosts(&a(&["DG.example.com.", "*.mc.example.com", "dg.example.com", "x\"y", "a:1", ""])), a(&["dg.example.com"]));
    let mut cfg = home::HomeConfig { servers: a(&["s1"]), ..Default::default() };
    assert_eq!(home::address_state(&cfg, "s1", true, &[]), "ok", "one server takes every address");
    cfg.servers = a(&["s1", "s2"]);
    assert_eq!(home::address_state(&cfg, "s1", false, &[]), "none");
    assert_eq!(home::address_state(&cfg, "s1", false, &a(&["a.x"])), "ok");
    assert_eq!(home::address_state(&cfg, "s1", true, &a(&["a.x"])), "stale", "not in Velocity yet");
    cfg.backends = vec![home::Backend { server: "s1".into(), name: "s1".into(), port: 25580, hosts: a(&["a.x"]) }];
    assert_eq!(home::address_state(&cfg, "s1", true, &a(&["a.x"])), "ok");
    assert_eq!(home::address_state(&cfg, "s1", true, &a(&["b.x"])), "stale", "the address changed since");
}

/// Another server in the test panel: its id.
fn add_server(t: &crate::cross_origin_tests::T, name: &str) -> String {
    let root = t.dir.join("servers").join(name);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("server.properties"), format!("motd={name}\n")).unwrap();
    t.app.rescan();
    t.app.servers.list().iter().find(|e| e.root == root).expect("the new server").id.clone()
}

fn set_addresses(t: &crate::cross_origin_tests::T, id: &str, hosts: &[&str]) {
    let e = t.app.servers.get(id).unwrap();
    let mut s = crate::router::settings(&e);
    s.addresses = hosts.iter().map(|h| h.to_string()).collect();
    s.save(&e.root).unwrap();
}

/// Streams home has had for `id` so far.
fn streams(t: &crate::cross_origin_tests::T, id: &str) -> u64 {
    t.app.vps.tick();
    t.app.vps.server_status(id)["total"]["streams"].as_u64().unwrap_or(0)
}

/// A join on the relay's port: the text it's turned away with.
async fn join_refused(port: u16, host: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(&core::handshake_packet(767, host, port, 2)).await.unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Read::Done((id, body), _) = core::read_packet(&buf, 1 << 20).unwrap() {
            assert_eq!(id, 0, "a login disconnect");
            let (text, _) = core::read_string(body, 1 << 20).unwrap();
            return serde_json::from_str::<Value>(&text).unwrap()["text"].as_str().unwrap_or_default().to_string();
        }
        let n = tokio::time::timeout(Duration::from_secs(10), s.read(&mut chunk)).await.expect("an answer in time").unwrap();
        assert!(n > 0, "closed without a reason");
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Two servers on one relay: each player reaches the server whose address
/// they typed, an address neither has is refused at the relay, and taking
/// one server off leaves the other as it was.
#[tokio::test(flavor = "multi_thread")]
async fn two_servers_behind_one_relay() {
    let t = crate::cross_origin_tests::panel_with(crate::TlsInfo { fingerprint: "AA:BB".into(), ca_pem: String::new() });
    let a = t.server.clone();
    let b = add_server(&t, "Creative");
    set_addresses(&t, &a, &["survival.example.test"]);
    set_addresses(&t, &b, &["creative.example.test"]);
    let r = run_relay().await;
    t.app.vps.set_config(home::HomeConfig { servers: vec![a.clone(), b.clone()], ..home_cfg(&r, r.tunnel, &a) }).unwrap();
    home::start(t.app.clone());
    wait_for("both servers on the relay", 15, || r.relay.route_servers() == [a.clone(), b.clone()]).await;

    // Streams reach the right server.
    let (status, pong) = list_ping_as(r.game, "Survival.Example.test").await;
    assert_ne!(status["version"]["name"], "Hopper relay", "answered by home: {status}");
    assert!(pong);
    wait_for("a stream for the first server", 10, || streams(&t, &a) == 1).await;
    assert_eq!(streams(&t, &b), 0);
    let (status, _) = list_ping_as(r.game, "creative.example.test").await;
    assert_ne!(status["version"]["name"], "Hopper relay", "{status}");
    wait_for("a stream for the second server", 10, || streams(&t, &b) == 1).await;
    assert_eq!(streams(&t, &a), 1);

    // An address neither has: the relay answers, home never sees it.
    let (status, _) = list_ping_as(r.game, "nobody.example.test").await;
    assert_eq!(status["version"]["name"], "Hopper relay");
    assert_eq!(status["description"]["text"], "No server goes by this address");
    assert!(join_refused(r.game, "nobody.example.test").await.contains("No server here goes by this address"));
    assert_eq!((streams(&t, &a), streams(&t, &b)), (1, 1));

    // The cards give each server its own address.
    t.app.vps.set_config(home::HomeConfig { backends: vec![home::Backend { server: b.clone(), name: "creative".into(), port: 25581, hosts: vec!["creative.example.test".into()] }], ..t.app.vps.config() }).unwrap();
    assert!(t.app.vps.players_address_for(&b, &[]).starts_with("creative.example.test"));
    assert!(!t.app.vps.players_address_for(&a, &[]).starts_with("creative"));

    // The first server comes off: the second keeps working as before.
    t.app.vps.set_config(home::HomeConfig { servers: vec![b.clone()], ..t.app.vps.config() }).unwrap();
    wait_for("one server left", 15, || r.relay.route_servers() == [b.clone()]).await;
    let (status, _) = list_ping_as(r.game, "creative.example.test").await;
    assert_ne!(status["version"]["name"], "Hopper relay", "{status}");
    wait_for("the second server's next stream", 10, || streams(&t, &b) == 2).await;
    assert_eq!(t.app.vps.server_status(&a)["relayed"], false);

}

/// Velocity's way on (B7): each server has its own loopback port on the
/// relay, opened when home names it, and a connection there reaches only
/// that server. A server set to check accounts through the relay that
/// Velocity doesn't have yet is refused at the front port, not let in
/// without the check.
#[tokio::test(flavor = "multi_thread")]
async fn velocity_ports_reach_their_own_server() {
    use hopper_core::hosting::proxy_forward::{Mode, Record};
    let t = crate::cross_origin_tests::panel_with(crate::TlsInfo { fingerprint: "AA:BB".into(), ca_pem: String::new() });
    let a = t.server.clone();
    let b = add_server(&t, "Creative");
    for id in [&a, &b] {
        t.app.vps_forward.set(id, Some(Record { mode: Mode::Relay, ..Default::default() })).unwrap();
    }
    set_addresses(&t, &a, &["survival.example.test"]);
    set_addresses(&t, &b, &["creative.example.test"]);

    let mut r = run_relay_with(true).await;
    let base = r.backend;
    let backends = vec![
        home::Backend { server: a.clone(), name: "survival".into(), port: base, hosts: vec!["survival.example.test".into()] },
        home::Backend { server: b.clone(), name: "creative".into(), port: base + 1, hosts: vec!["creative.example.test".into()] },
    ];
    t.app.vps.set_config(home::HomeConfig { servers: vec![a.clone(), b.clone()], backends, ..home_cfg(&r, r.tunnel, &a) }).unwrap();
    let ctl = home::routes(&t.app);
    assert_eq!(ctl.routes.iter().map(|r| (r.port, r.hosts.clone())).collect::<Vec<_>>(), [(base, vec!["survival.example.test".to_string()]), (base + 1, vec!["creative.example.test".to_string()])]);
    // The relay may listen on the second port from here on.
    r.free_next_backend();
    home::start(t.app.clone());
    wait_for("both servers on the relay", 30, || r.relay.route_servers().len() == 2).await;
    let mut open = false;
    for _ in 0..150 {
        if TcpStream::connect(("127.0.0.1", base + 1)).await.is_ok() {
            open = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(open, "the second backend port opened");
    // Velocity's own address in the handshake doesn't matter here: the port does.
    let (status, _) = list_ping_as(base + 1, "127.0.0.1").await;
    assert_ne!(status["version"]["name"], "Hopper relay", "{status}");
    wait_for("the second server's stream", 30, || streams(&t, &b) >= 1).await;
    let before_a = streams(&t, &a);
    let (status, _) = list_ping_as(base, "127.0.0.1").await;
    assert_ne!(status["version"]["name"], "Hopper relay", "{status}");
    wait_for("the first server's stream", 30, || streams(&t, &a) > before_a).await;

    // The second server's Velocity line is gone (taken out by hand): its
    // address is refused at the front port and its old port goes nowhere.
    let first_only = t.app.vps.config().backends[..1].to_vec();
    t.app.vps.set_config(home::HomeConfig { backends: first_only, ..t.app.vps.config() }).unwrap();
    wait_for("the second server without a port", 30, || home::routes(&t.app).routes[1].port == 0).await;
    let mut refused = String::new();
    for _ in 0..100 {
        refused = join_refused(r.game, "creative.example.test").await;
        if refused.contains("isn't set up on the relay yet") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(refused.contains("isn't set up on the relay yet"), "{refused}");
    let gone = streams(&t, &b);
    let mut s = TcpStream::connect(("127.0.0.1", base + 1)).await.unwrap();
    let _ = s.write_all(&core::handshake_packet(767, "127.0.0.1", base + 1, 1)).await;
    let _ = s.write_all(&core::packet(0, &[])).await;
    let mut chunk = [0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(10), s.read(&mut chunk)).await.expect("closed in time").unwrap_or(0);
    assert_eq!(n, 0, "nothing answers on a port no server has");
    assert_eq!(streams(&t, &b), gone);
}

/// Taking a server off the relay through the API: refused while it still
/// checks accounts through the relay, and its Velocity line goes with it.
#[tokio::test(flavor = "multi_thread")]
async fn taking_a_server_off_needs_stop_first() {
    use crate::cross_origin_tests::{OWN, body, call, cookie_login};
    use hopper_core::hosting::proxy_forward::{Mode, Record};
    let t = crate::cross_origin_tests::panel_with(crate::TlsInfo { fingerprint: "AA:BB".into(), ca_pem: String::new() });
    let a = t.server.clone();
    let b = add_server(&t, "Creative");
    let backends = vec![home::Backend { server: a.clone(), name: "a".into(), port: 25580, hosts: vec![] }, home::Backend { server: b.clone(), name: "b".into(), port: 25581, hosts: vec![] }];
    t.app.vps.set_config(home::HomeConfig { servers: vec![a.clone(), b.clone()], backends, ..Default::default() }).unwrap();
    t.app.vps_forward.set(&b, Some(Record { mode: Mode::Relay, ..Default::default() })).unwrap();
    let me = cookie_login(&t).await;
    let put = |servers: Vec<String>| call("PUT", "/api/v1/vps-relay").origin(OWN).h("cookie", format!("hopperd_session={me}")).h("x-hopperd", "1").json(serde_json::json!({ "servers": servers }));
    let (s, _, got) = put(vec![a.clone()]).send(&t).await;
    assert_eq!(s, axum::http::StatusCode::BAD_REQUEST);
    assert!(body(&got)["error"].as_str().unwrap().contains("Stop using the relay for Creative first"), "{}", body(&got));
    assert_eq!(t.app.vps.config().servers.len(), 2);
    let (s, _, _) = put(vec![b.clone()]).send(&t).await;
    assert_eq!(s, axum::http::StatusCode::OK, "a server on its own comes off");
    let c = t.app.vps.config();
    assert_eq!(c.servers, [b.clone()]);
    assert_eq!(c.backends.iter().map(|x| x.server.clone()).collect::<Vec<_>>(), [b.clone()]);
    // The list in Settings › Relays: owner only.
    let (s, _, got) = call("GET", "/api/v1/vps-relays").origin(OWN).h("cookie", format!("hopperd_session={me}")).h("x-hopperd", "1").send(&t).await;
    assert_eq!(s, axum::http::StatusCode::OK);
    assert_eq!(body(&got)["relays"], serde_json::json!([]), "no relay address set");
    let (s, _, _) = call("GET", "/api/v1/vps-relays").origin(OWN).send(&t).await;
    assert_eq!(s, axum::http::StatusCode::UNAUTHORIZED);
}
