//! Voice through the relay (B6): the sealed packets, the TCP frames, the
//! limits, and voice going both ways between a player and a server's voice
//! port, over UDP and over the tunnel when UDP doesn't get through.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;

use super::*;
use crate::vps::link;

fn keys(seed: u8) -> Keys {
    Keys::derive(&[seed; 64], &link::key_hash("relay key"))
}

/// A Simple Voice Chat-shaped packet: 0xFF, a 16-byte id, then `body`.
fn svc(body: &[u8]) -> Vec<u8> {
    let mut p = vec![0xFF];
    p.extend_from_slice(&[0x42; 16]);
    p.extend_from_slice(body);
    p
}

/// Small deterministic random numbers, so a failure repeats.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

// ---- keys and sealed packets ------------------------------------------------------------

#[test]
fn both_ends_derive_the_same_keys_and_each_connection_its_own() {
    let (a, b) = (keys(1), keys(1));
    assert_eq!(a.session, b.session);
    let (mut home_tx, mut home_rx) = a.home();
    let (mut relay_tx, mut relay_rx) = b.relay();
    let p = home_tx.seal(Kind::Keepalive, b"hello");
    assert_eq!(relay_rx.open(&p).unwrap().0, Kind::Keepalive);
    let p = relay_tx.data(7, b"voice");
    let (kind, body, _) = home_rx.open(&p).unwrap();
    assert_eq!((kind, split_data(&body)), (Kind::Data, Some((7, &b"voice"[..]))));
    // A direction's key opens only its own direction.
    let p = home_tx.seal(Kind::Keepalive, b"x");
    assert!(home_rx.open(&p).is_err());
    // Another connection (another exported secret) or another relay key: nothing opens.
    let (_, mut other_rx) = keys(2).relay();
    assert!(other_rx.open(&home_tx.seal(Kind::Keepalive, b"x")).is_err());
    let k3 = Keys::derive(&[1; 64], &link::key_hash("another key"));
    let (_, mut k3_rx) = k3.relay();
    assert!(k3_rx.open(&home_tx.seal(Kind::Keepalive, b"x")).is_err());
}

#[test]
fn any_changed_byte_is_refused() {
    let k = keys(3);
    let (mut tx, _) = k.home();
    let p = tx.data(9, &svc(b"some audio"));
    for i in 0..p.len() {
        for bit in [0x01u8, 0x80] {
            let (_, mut rx) = k.relay();
            let mut bad = p.clone();
            bad[i] ^= bit;
            assert!(rx.open(&bad).is_err(), "byte {i} bit {bit:#x} changed and it still opened");
        }
    }
    // Cut short or with a byte added: refused too.
    let (_, mut rx) = k.relay();
    assert!(rx.open(&p[..p.len() - 1]).is_err());
    let mut longer = p.clone();
    longer.push(0);
    assert!(rx.open(&longer).is_err());
    // The real one still opens after all that: a forged packet doesn't use up its counter.
    assert!(rx.open(&p).is_ok());
}

#[test]
fn replays_are_refused_within_and_beyond_the_window() {
    let k = keys(4);
    let (mut tx, _) = k.home();
    let (_, mut rx) = k.relay();
    let ps: Vec<Vec<u8>> = (0..200).map(|i| tx.seal(Kind::Keepalive, &[i as u8])).collect();
    assert!(rx.open(&ps[0]).is_ok());
    assert_eq!(rx.open(&ps[0]).unwrap_err(), Rejected::Replay);
    // Out of order within the window: each once.
    assert!(rx.open(&ps[5]).is_ok());
    assert!(rx.open(&ps[3]).is_ok());
    assert_eq!(rx.open(&ps[3]).unwrap_err(), Rejected::Replay);
    assert!(rx.open(&ps[4]).is_ok());
    // Far ahead: what's more than 64 behind is too old, even unseen.
    assert!(rx.open(&ps[150]).is_ok());
    assert_eq!(rx.open(&ps[50]).unwrap_err(), Rejected::Replay);
    assert!(rx.open(&ps[100]).is_ok(), "63 behind is still inside the window");
    assert_eq!(rx.open(&ps[86]).unwrap_err(), Rejected::Replay, "64 behind is out");
    assert_eq!(rx.newest(), Some(150));
}

#[test]
fn wrong_session_and_cheap_checks_come_before_the_tag() {
    let k = keys(5);
    let (mut tx, _) = k.home();
    let (_, mut rx) = k.relay();
    let mut p = tx.seal(Kind::Keepalive, b"ka");
    p[5] ^= 1;
    assert_eq!(rx.open(&p).unwrap_err(), Rejected::Session);
    let p = tx.seal(Kind::Keepalive, b"ka");
    let mut v = p.clone();
    v[3] = 2;
    assert_eq!(rx.open(&v).unwrap_err(), Rejected::Garbage, "another version");
    let mut v = p.clone();
    v[4] = 9;
    assert_eq!(rx.open(&v).unwrap_err(), Rejected::Garbage, "an unknown kind");
    assert_eq!(rx.open(&p[..HEADER + TAG - 1]).unwrap_err(), Rejected::Garbage, "too short for a tag");
}

#[test]
fn oversize_packets_are_dropped_unread() {
    let k = keys(6);
    let (mut tx, _) = k.home();
    let (_, mut rx) = k.relay();
    // The biggest allowed voice packet goes through; one byte more doesn't.
    let ok = tx.data(1, &vec![0xFF; MAX_PACKET]);
    assert_eq!(ok.len(), MAX_SEALED);
    assert!(rx.open(&ok).is_ok());
    let big = tx.data(1, &vec![0xFF; MAX_PACKET + 1]);
    assert_eq!(rx.open(&big).unwrap_err(), Rejected::Garbage);
    assert!(split_data(&vec![0; 4 + MAX_PACKET + 1]).is_none());
    assert!(!player_packet_ok(&svc(&vec![0; MAX_PACKET])));
    assert!(Frames::default().push(&((4 + MAX_PACKET + 1) as u16).to_be_bytes()).is_err());
}

#[test]
fn only_voice_mods_packets_count_as_players() {
    assert!(player_packet_ok(&svc(b"x")));
    let mut plasmo = vec![0x4E, 0x90, 0x04, 0xE9, 1];
    plasmo.extend_from_slice(&[0; 24]);
    assert!(player_packet_ok(&plasmo));
    assert!(!player_packet_ok(b""));
    assert!(!player_packet_ok(&[0xFF; 17]), "too short for SVC's id");
    assert!(!player_packet_ok(b"GET / HTTP/1.1\r\n\r\n"));
    assert!(!player_packet_ok(&plasmo[..20]));
    // A sealed packet never reads as a player's, and the other way round.
    let (mut tx, _) = keys(7).home();
    let s = tx.seal(Kind::Keepalive, b"");
    assert!(is_sealed(&s) && !player_packet_ok(&s));
    assert!(!is_sealed(&svc(b"x")) && !is_sealed(&plasmo));
}

#[test]
fn random_bytes_never_open_or_panic() {
    let k = keys(8);
    let (_, mut rx) = k.relay();
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    let mut frames = Frames::default();
    for i in 0..20_000 {
        let n = (r.next() % (MAX_SEALED as u64 + 40)) as usize;
        let mut p = r.bytes(n);
        // Half of them look like ours up to the tag, so the deeper checks run.
        if i % 2 == 0 && p.len() >= 5 {
            p[..3].copy_from_slice(MAGIC);
            p[3] = VERSION;
            p[4] = 1 + (r.next() % 3) as u8;
            if p.len() >= 9 && i % 4 == 0 {
                p[5..9].copy_from_slice(&k.session.to_be_bytes());
            }
        }
        assert!(rx.open(&p).is_err());
        let _ = header(&p);
        let _ = is_sealed(&p);
        let _ = split_data(&p);
        let _ = player_packet_ok(&p);
        // The frame reader takes any chunks; once it refuses, it starts over.
        if frames.push(&p).is_err() {
            frames = Frames::default();
        }
    }
}

#[test]
fn frames_split_anywhere_come_back_whole() {
    let packets: Vec<(u32, Vec<u8>)> = (0..50).map(|i| (i, svc(&vec![i as u8; (i * 37 % 1200) as usize]))).collect();
    let all: Vec<u8> = packets.iter().flat_map(|(f, p)| frame(*f, p)).collect();
    let mut r = Rng(42);
    let mut frames = Frames::default();
    let mut got = Vec::new();
    let mut at = 0;
    while at < all.len() {
        let n = ((r.next() % 300) as usize + 1).min(all.len() - at);
        got.extend(frames.push(&all[at..at + n]).unwrap());
        at += n;
    }
    assert_eq!(got, packets);
    // A length that can't be (too short for the flow id) ends the stream.
    assert!(Frames::default().push(&[0, 3, 0, 0, 0]).is_err());
}

#[test]
fn buckets_limit_with_a_burst_and_refill() {
    let t0 = Instant::now();
    let mut b = Bucket::new(10.0, 20.0);
    b.last = t0;
    let took = (0..100).filter(|_| b.take_at(t0)).count();
    assert_eq!(took, 20, "the burst, then nothing");
    assert!(!b.take_at(t0 + Duration::from_millis(50)));
    assert!(b.take_at(t0 + Duration::from_millis(150)));
    // A long pause fills it to the burst, not beyond.
    let later = t0 + Duration::from_secs(60);
    assert_eq!((0..100).filter(|_| b.take_at(later)).count(), 20);
    // A player's normal voice (50 a second) never hits the limit.
    let mut p = player_bucket();
    p.last = t0;
    assert!((0..500).all(|i| p.take_at(t0 + Duration::from_millis(20 * i))));
}

// ---- the whole path -------------------------------------------------------------------------

/// A stand-in for the server's voice port: answers each packet with "re:"
/// and the packet, to whoever sent it.
async fn echo_server() -> SocketAddr {
    let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = s.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        while let Ok((n, from)) = s.recv_from(&mut buf).await {
            let mut out = b"re:".to_vec();
            out.extend_from_slice(&buf[..n]);
            let _ = s.send_to(&out, from).await;
        }
    });
    addr
}

struct Path {
    relay: Arc<relay::RelayVoice>,
    slot: Arc<home::Slot>,
    _pair: crate::vps::tests::Pair,
    _tasks: Vec<tokio::task::JoinHandle<()>>,
}

/// The relay's voice port and home's voice link over a real tunnel, home
/// sending its UDP to `udp_port` (the relay's port, or a dead one to stand
/// in for a VPS whose provider blocks UDP).
async fn path(dead_udp: bool) -> Path {
    let mut p = crate::vps::tests::pair(crate::vps::tests::quick()).await;
    let relay = relay::RelayVoice::bind("127.0.0.1", 0).await.unwrap();
    let k = keys(11);
    relay.attach(1, &k);
    relay.set_enabled(1, true);
    let mut tasks = vec![tokio::spawn(relay.clone().run())];
    // The relay side takes home's voice stream, as relay_end does.
    let mut incoming = std::mem::replace(&mut p.relay.tunnel.incoming, tokio::sync::mpsc::channel(1).1);
    let r = relay.clone();
    tasks.push(tokio::spawn(async move {
        while let Some(s) = incoming.recv().await {
            tokio::spawn(r.clone().stream(1, s));
        }
    }));
    let server = echo_server().await;
    let udp_port = if dead_udp {
        // A port nothing reads: what home sends there is lost.
        let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = s.local_addr().unwrap().port();
        std::mem::forget(s);
        port
    } else {
        relay.port
    };
    let slot: Arc<home::Slot> = Arc::default();
    let target: home::Target = Arc::new(move || Some(server));
    let (s2, h) = (slot.clone(), p.home.tunnel.handle.clone());
    tasks.push(tokio::spawn(async move { home::run(s2, h, keys(11), ("127.0.0.1".into(), udp_port), true, target).await }));
    Path { relay, slot, _pair: p, _tasks: tasks }
}

async fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Waits until the relay's port has read everything sent to it so far: a
/// packet from a new player comes back echoed only after the ones queued
/// before it (one socket reads them in order). Sent again until one does,
/// since a queue that is still full drops it.
async fn drained(relay_port: u16) {
    let me = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    me.connect(("127.0.0.1", relay_port)).await.unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    let mut buf = vec![0u8; 4096];
    loop {
        assert!(Instant::now() < end, "the relay's port never got through its queue");
        me.send(&svc(b"drained?")).await.unwrap();
        if let Ok(Ok(k)) = tokio::time::timeout(Duration::from_millis(250), me.recv(&mut buf)).await
            && buf[..k].starts_with(b"re:")
        {
            return;
        }
    }
}

/// Sends `n` packets about 20 ms apart (one voice frame each, as a talking
/// player does) from a player socket while a reader takes the echoes:
/// (received, median round trip in ms, worst in ms). Each packet carries
/// its send time, so the round trip doesn't include the sender's sleeps.
/// The reader keeps going while the sender does, however slow the machine,
/// and stops once every echo is in or, after the last send, nothing came
/// for a while (a lost packet never comes).
async fn talk(relay_port: u16, n: u32) -> (u32, f64, f64) {
    let me = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    me.connect(("127.0.0.1", relay_port)).await.unwrap();
    let t0 = Instant::now();
    let rx = me.clone();
    let sent_all = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done = sent_all.clone();
    let reader = tokio::spawn(async move {
        let mut rtts = Vec::new();
        let mut seen = Vec::new();
        let mut buf = vec![0u8; 4096];
        loop {
            if seen.len() as u32 >= n {
                break;
            }
            let k = match tokio::time::timeout(Duration::from_secs(2), rx.recv(&mut buf)).await {
                Ok(Ok(k)) => k,
                // Quiet for 2 s after the last send: the rest was lost.
                Ok(Err(_)) | Err(_) if done.load(Ordering::Relaxed) => break,
                Ok(Err(_)) | Err(_) => continue,
            };
            let Some(p) = buf[..k].strip_prefix(b"re:") else { continue };
            assert!(player_packet_ok(p) && p.len() == 17 + 12, "the echo came back changed");
            seen.push(u32::from_be_bytes(p[17..21].try_into().unwrap()));
            let sent = u64::from_be_bytes(p[21..29].try_into().unwrap());
            rtts.push((t0.elapsed().as_micros() as u64 - sent) as f64 / 1000.0);
        }
        (rtts, seen)
    });
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    for i in 0..n {
        tick.tick().await;
        let mut body = i.to_be_bytes().to_vec();
        body.extend_from_slice(&(t0.elapsed().as_micros() as u64).to_be_bytes());
        me.send(&svc(&body)).await.unwrap();
    }
    sent_all.store(true, Ordering::Relaxed);
    let (mut rtts, seen) = reader.await.unwrap();
    let lost: Vec<u32> = (0..n).filter(|i| !seen.contains(i)).collect();
    if !lost.is_empty() {
        eprintln!("lost: {lost:?}");
    }
    rtts.sort_by(f64::total_cmp);
    let med = rtts.get(rtts.len() / 2).copied().unwrap_or(f64::NAN);
    (rtts.len() as u32, med, rtts.last().copied().unwrap_or(f64::NAN))
}

#[tokio::test(flavor = "multi_thread")]
async fn voice_goes_both_ways_over_udp() {
    let p = path(false).await;
    wait_for("UDP both ways", 10, || p.relay.udp_up() && p.slot.status().is_some_and(|s| s.via == "udp")).await;
    let (got, med, worst) = talk(p.relay.port, 400).await;
    eprintln!("voice over UDP: {got}/400 back, median {med:.2} ms, worst {worst:.2} ms");
    assert!(got >= 396, "lost {} of 400", 400 - got);
    let c = &p.relay.counts;
    assert!(c.to_home_udp.load(Ordering::Relaxed) >= 396 && c.to_home_tcp.load(Ordering::Relaxed) == 0);
    assert!(c.to_players.load(Ordering::Relaxed) >= 396);
    let s = p.slot.status().unwrap();
    assert_eq!((s.via, s.players), ("udp", 1));
    assert!(s.rtt_ms.is_some());
    assert_eq!(p.relay.flows(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn voice_rides_the_tunnel_when_udp_is_blocked() {
    let p = path(true).await;
    // Home's voice stream is there before any keepalive answer could be,
    // and the relay has taken it: players' packets have a way home. (Talking
    // before the relay took it lost the first few under load.)
    wait_for("the voice stream", 10, || p.slot.status().is_some() && p.relay.has_stream()).await;
    let (got, med, worst) = talk(p.relay.port, 200).await;
    eprintln!("voice over the tunnel: {got}/200 back, median {med:.2} ms, worst {worst:.2} ms");
    assert!(got >= 198, "lost {} of 200", 200 - got);
    let c = &p.relay.counts;
    assert_eq!(c.to_home_udp.load(Ordering::Relaxed), 0);
    assert!(c.to_home_tcp.load(Ordering::Relaxed) >= 198);
    assert!(!p.relay.udp_up());
    assert_ne!(p.slot.status().unwrap().via, "udp");
}

#[tokio::test(flavor = "multi_thread")]
async fn forged_garbage_and_floods_are_dropped() {
    let p = path(false).await;
    wait_for("UDP both ways", 10, || p.relay.udp_up()).await;
    let bad = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    bad.connect(("127.0.0.1", p.relay.port)).await.unwrap();
    // Sealed-looking packets with the right session but no key: refused,
    // and home's address doesn't move to the sender.
    let mut r = Rng(7);
    for _ in 0..50 {
        let mut f = r.bytes(60);
        f[..3].copy_from_slice(MAGIC);
        f[3] = VERSION;
        f[4] = Kind::Keepalive as u8;
        f[5..9].copy_from_slice(&keys(11).session.to_be_bytes());
        bad.send(&f).await.unwrap();
    }
    // Not a voice mod's packet, and one too big: dropped.
    bad.send(b"\x00scan").await.unwrap();
    bad.send(&vec![0xFF; MAX_SEALED + 1]).await.unwrap();
    wait_for("the refusals", 5, || p.relay.counts.refused.load(Ordering::Relaxed) >= 50 && p.relay.counts.garbage.load(Ordering::Relaxed) >= 2).await;
    assert!(p.relay.udp_up(), "home's path still works");
    // One player sending far over its rate: the rest is dropped.
    let flood = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    flood.connect(("127.0.0.1", p.relay.port)).await.unwrap();
    for i in 0..2000u32 {
        let _ = flood.send(&svc(&i.to_be_bytes())).await;
    }
    wait_for("the limit", 5, || p.relay.counts.limited.load(Ordering::Relaxed) >= 100).await;
    // Whatever of the flood is still queued at the relay's port goes first:
    // talking into a full queue loses packets to the OS, not to the relay.
    drained(p.relay.port).await;
    let (got, _, _) = talk(p.relay.port, 100).await;
    assert!(got >= 98, "another player still gets through ({got}/100)");
}

/// A flood of sealed-looking fakes from elsewhere, with the right session,
/// far over the relay's budget of tag checks: home's own packets don't
/// share that budget, so players keep hearing each other. (Found on the
/// test VPS: before, 92k junk packets a second cost a real player 60 %.)
#[tokio::test(flavor = "multi_thread")]
async fn a_flood_of_fakes_cant_crowd_out_home() {
    let p = path(false).await;
    wait_for("UDP both ways", 10, || p.relay.udp_up()).await;
    // A budget the flood is sure to go over on any machine (the real one is
    // 5000 a second, which a busy test machine's flood may never reach),
    // and a flood paced so the port's queue never fills: what this checks
    // is the budget, not how fast the OS drops packets.
    p.relay.set_sealed_budget(50.0, 50.0);
    let port = p.relay.port;
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flooder = {
        let stop = stop.clone();
        tokio::spawn(async move {
            let bad = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            bad.connect(("127.0.0.1", port)).await.unwrap();
            let mut r = Rng(9);
            let mut n = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let mut f = r.bytes(60);
                f[..3].copy_from_slice(MAGIC);
                f[3] = VERSION;
                f[4] = Kind::Data as u8;
                f[5..9].copy_from_slice(&keys(11).session.to_be_bytes());
                let _ = bad.send(&f).await;
                n += 1;
                if n % 4 == 0 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
            n
        })
    };
    let (got, _, _) = talk(port, 200).await;
    stop.store(true, Ordering::Relaxed);
    let sent = flooder.await.unwrap();
    let limited = p.relay.counts.limited.load(Ordering::Relaxed);
    eprintln!("fakes sent: {sent}, over the budget: {limited}");
    assert!(limited > 0, "the flood went over the budget");
    assert!(got >= 196, "players still hear each other ({got}/200)");
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_passes_while_home_carries_no_voice() {
    let p = path(false).await;
    wait_for("UDP both ways", 10, || p.relay.udp_up()).await;
    p.relay.set_enabled(1, false);
    let (got, _, _) = talk(p.relay.port, 20).await;
    assert_eq!(got, 0);
    assert!(p.relay.counts.no_home.load(Ordering::Relaxed) >= 20);
    // Home went: the old connection's keys stop working too.
    p.relay.detach(1);
    assert!(!p.relay.udp_up());
}
