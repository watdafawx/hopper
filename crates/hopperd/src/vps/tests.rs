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

fn key() -> String {
    format!("hrt_{}", "ab".repeat(32))
}

/// A small busy direction, cache on, so the tests exercise it cheaply.
fn small() -> Codec {
    Codec { level: 3, window_log: 20, long_distance: false, cache_bytes: 1 << 20 }
}

fn quick() -> mux::Config {
    mux::Config { ping_every: Duration::from_millis(300), dead_after: Duration::from_secs(2), ..link::mux_config() }
}

fn head(kind: &str, server: &str) -> Vec<u8> {
    serde_json::to_vec(&Head { kind: kind.into(), server: server.into(), peer: "203.0.113.9:50000".into() }).unwrap()
}

/// Relay and home over an in-memory connection the test can cut.
struct Pair {
    relay: Live,
    home: Live,
    cut: JoinHandle<()>,
}

async fn pair(cfg: mux::Config) -> Pair {
    let (r_io, r_far) = tokio::io::duplex(256 * 1024);
    let (h_io, h_far) = tokio::io::duplex(256 * 1024);
    let cut = tokio::spawn(async move {
        let (mut a, mut b) = (r_far, h_far);
        let _ = tokio::io::copy_bidirectional(&mut a, &mut b).await;
    });
    let (hash, k) = (link::key_hash(&key()), key());
    let (relay, home) = tokio::join!(link::relay(r_io, &hash, small(), 25565, cfg.clone()), link::home(h_io, &k, Codec::quiet(), cfg));
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
    let (relay, home) = tokio::join!(link::relay(r_io, &other, small(), 25565, quick()), link::home(h_io, &k, Codec::quiet(), quick()));
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
    let relay = tokio::spawn(async move { link::relay(r_io, &link::key_hash(&key()), small(), 25565, quick()).await });
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
    let relay = tokio::spawn(async move { link::relay(r_io, &link::key_hash(&key()), small(), 25565, quick()).await });
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
    tunnel: u16,
    fingerprint: String,
    task: JoinHandle<()>,
    dir: std::path::PathBuf,
}

impl Drop for RelayRun {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn run_relay() -> RelayRun {
    let dir = std::env::temp_dir().join(format!("hopperd-vps-{}-{}", std::process::id(), crate::auth::random_hex(4)));
    std::fs::create_dir_all(&dir).unwrap();
    let (tls, fingerprint) = relay_end::tls_config(&dir).unwrap();
    let mut relay = Relay::new(link::key_hash(&key()), small(), 0);
    Arc::get_mut(&mut relay).unwrap().mux = quick();
    let game = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tunnel = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (gp, tp) = (game.local_addr().unwrap().port(), tunnel.local_addr().unwrap().port());
    let task = tokio::spawn(relay_end::serve(relay.clone(), game, tunnel, tls, None));
    RelayRun { relay, game: gp, tunnel: tp, fingerprint, task, dir }
}

fn home_cfg(r: &RelayRun, port: u16, server: &str) -> home::HomeConfig {
    home::HomeConfig { enabled: true, address: format!("127.0.0.1:{port}"), fingerprint: r.fingerprint.clone(), key: key(), servers: vec![server.into()], public_address: String::new() }
}

/// A server list ping (status, then ping): the status JSON and whether the
/// pong came back with the same payload.
async fn list_ping(port: u16) -> (Value, bool) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut out = core::handshake_packet(767, "localhost", port, 1);
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
            s.tx.send(&out).await.unwrap();
            // The first answer, or the stream's end.
            tokio::time::timeout(Duration::from_secs(10), s.rx.recv()).await.expect("an answer or the end")
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

/// A TCP proxy in front of the relay's tunnel port that the test can stop
/// (standing in for the VPS going away) and start again on the same port.
async fn proxy(listen: Option<u16>, to: u16) -> (u16, JoinHandle<()>) {
    let l = TcpListener::bind(("127.0.0.1", listen.unwrap_or(0))).await.unwrap();
    let port = l.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let mut set = tokio::task::JoinSet::new();
        loop {
            let Ok((mut a, _)) = l.accept().await else { continue };
            set.spawn(async move {
                if let Ok(mut b) = TcpStream::connect(("127.0.0.1", to)).await {
                    let _ = tokio::io::copy_bidirectional(&mut a, &mut b).await;
                }
            });
        }
    });
    (port, task)
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
    let (pport, ptask) = proxy(None, r.tunnel).await;
    t.app.vps.set_config(home_cfg(&r, pport, &t.server)).unwrap();
    home::start(t.app.clone());
    wait_for("the link", 15, || r.relay.ready()).await;
    assert_eq!(t.app.vps.status().state, "up");

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

    // The VPS goes away.
    ptask.abort();
    let _ = ptask.await;
    wait_for("down", 20, || t.app.vps.status().state == "down" && !r.relay.ready()).await;
    let (status, _) = list_ping(r.game).await;
    assert_eq!(status["description"]["text"], "Home server is offline");
    let s = t.app.vps.server_status(&t.server);
    assert_eq!(s["state"], "down");
    assert!(!s["error"].as_str().unwrap().is_empty());

    // And comes back on the same address.
    let (_, ptask) = proxy(Some(pport), r.tunnel).await;
    wait_for("up again", 30, || t.app.vps.status().state == "up" && r.relay.ready()).await;
    let (status, pong) = list_ping(r.game).await;
    assert_ne!(status["version"]["name"], "Hopper relay");
    assert!(pong);
    // Totals carry over the reconnect.
    t.app.vps.tick();
    assert!(t.app.vps.server_status(&t.server)["total"]["streams"].as_u64().unwrap() >= 2);
    ptask.abort();
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
