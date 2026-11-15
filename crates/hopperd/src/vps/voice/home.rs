//! Home's end of voice (see the module above): one UDP socket that dials
//! out to the relay's voice port and keeps the path open, and one local
//! socket per player that talks to the server's voice port on this machine.
//!
//! Home never listens for the internet: the socket to the relay is
//! `connect`ed, so the system only lets the relay's address in, and each
//! player socket is connected to the server's voice port.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use super::{ACTIVE, FLOW_IDLE, Frames, KEEPALIVE, Keys, Kind, MAX_FLOWS, MAX_PACKET, MAX_SEALED, Opener, STALE, Sealer};
use crate::tunnel::mux;

/// Where voice stands on the current connection, for the card.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    /// "udp" (straight, as it should be), "tcp" (UDP doesn't get through;
    /// over the tunnel), "checking" (the first answers aren't in yet).
    pub via: &'static str,
    /// Players who sent voice lately.
    pub players: usize,
    pub rtt_ms: Option<u32>,
    /// The relay's voice port.
    pub port: u16,
}

/// Home's voice link, while the relay connection is up.
#[derive(Default)]
pub struct Slot(Mutex<Option<Arc<Link>>>);

impl Slot {
    pub fn status(&self) -> Option<Status> {
        self.0.lock().unwrap().as_ref().map(|l| l.status())
    }

    pub fn current(&self) -> Option<Arc<Link>> {
        self.0.lock().unwrap().clone()
    }
}

#[derive(Default)]
pub struct Counts {
    pub to_relay_udp: AtomicU64,
    pub to_relay_tcp: AtomicU64,
    pub from_relay: AtomicU64,
    pub to_server: AtomicU64,
    pub refused: AtomicU64,
}

pub struct Link {
    port: u16,
    sock: Option<Arc<UdpSocket>>,
    tx: Mutex<Sealer>,
    rx: Mutex<Opener>,
    stream: mpsc::Sender<Vec<u8>>,
    started: Instant,
    last_ack: Mutex<Option<Instant>>,
    /// u32::MAX: none yet.
    rtt: AtomicU32,
    flows: Mutex<HashMap<u32, Local>>,
    target: Mutex<Option<SocketAddr>>,
    pub counts: Counts,
}

/// One player's socket towards the server's voice port.
struct Local {
    sock: Arc<UdpSocket>,
    last: Instant,
    /// The last packet from the player (the card counts these).
    heard: Instant,
    reader: tokio::task::JoinHandle<()>,
}

impl Drop for Local {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl Link {
    /// The relay answered a keepalive lately: UDP works both ways.
    pub fn udp_ok(&self) -> bool {
        self.sock.is_some() && self.last_ack.lock().unwrap().is_some_and(|t| t.elapsed() < STALE)
    }

    pub fn status(&self) -> Status {
        let via = if self.udp_ok() {
            "udp"
        } else if self.started.elapsed() < STALE {
            "checking"
        } else {
            "tcp"
        };
        let players = self.flows.lock().unwrap().values().filter(|f| f.heard.elapsed() < ACTIVE).count();
        let rtt = self.rtt.load(Ordering::Relaxed);
        Status { via, players, rtt_ms: (rtt != u32::MAX && via == "udp").then_some(rtt), port: self.port }
    }

    /// A player's packet from the relay: to the server, from that player's
    /// own socket here.
    fn deliver(self: &Arc<Self>, flow: u32, pkt: &[u8]) {
        let Some(target) = *self.target.lock().unwrap() else { return };
        let mut flows = self.flows.lock().unwrap();
        if let Some(l) = flows.get_mut(&flow) {
            l.last = Instant::now();
            l.heard = l.last;
            if l.sock.try_send(pkt).is_ok() {
                self.counts.to_server.fetch_add(1, Ordering::Relaxed);
            }
            return;
        }
        if flows.len() >= MAX_FLOWS {
            return;
        }
        let sock = match local_socket(target, pkt) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                log::debug!("relay voice: a socket to the server's voice port: {e}");
                return;
            }
        };
        let me: Weak<Link> = Arc::downgrade(self);
        let rs = sock.clone();
        let reader = tokio::spawn(async move {
            let mut buf = vec![0u8; MAX_PACKET + 64];
            while let Ok(n) = rs.recv(&mut buf).await {
                let Some(me) = me.upgrade() else { return };
                me.back(flow, &buf[..n]);
            }
        });
        self.counts.to_server.fetch_add(1, Ordering::Relaxed);
        let now = Instant::now();
        flows.insert(flow, Local { sock, last: now, heard: now, reader });
    }

    /// The server's answer for a player: to the relay, straight when UDP
    /// works, else over the tunnel.
    fn back(&self, flow: u32, pkt: &[u8]) {
        if pkt.len() > MAX_PACKET {
            return;
        }
        if let Some(l) = self.flows.lock().unwrap().get_mut(&flow) {
            l.last = Instant::now();
        }
        if self.udp_ok()
            && let Some(s) = &self.sock
        {
            let out = self.tx.lock().unwrap().data(flow, pkt);
            if s.try_send(&out).is_ok() {
                self.counts.to_relay_udp.fetch_add(1, Ordering::Relaxed);
            }
        } else if self.stream.try_send(super::frame(flow, pkt)).is_ok() {
            self.counts.to_relay_tcp.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn keepalive(&self) {
        let Some(s) = &self.sock else { return };
        let mut body = super::now_ms().to_be_bytes().to_vec();
        // Tells the relay whether its answers arrive, so it only sends
        // voice this way when they do.
        body.push(self.udp_ok() as u8);
        let out = self.tx.lock().unwrap().seal(Kind::Keepalive, &body);
        let _ = s.try_send(&out);
    }

    fn sealed(self: &Arc<Self>, pkt: &[u8]) {
        let opened = self.rx.lock().unwrap().open(pkt);
        let Ok((kind, body, _)) = opened else {
            self.counts.refused.fetch_add(1, Ordering::Relaxed);
            return;
        };
        match kind {
            Kind::Ack => {
                *self.last_ack.lock().unwrap() = Some(Instant::now());
                if let Some(sent) = body.get(..8).map(|b| u64::from_be_bytes(b.try_into().expect("8"))) {
                    let ms = super::now_ms().saturating_sub(sent).min(60_000) as u32;
                    self.rtt.store(ms, Ordering::Relaxed);
                }
            }
            Kind::Data => {
                self.counts.from_relay.fetch_add(1, Ordering::Relaxed);
                if let Some((flow, p)) = super::split_data(&body) {
                    self.deliver(flow, p);
                }
            }
            Kind::Keepalive => {}
        }
    }

    fn sweep(&self) {
        self.flows.lock().unwrap().retain(|_, f| f.last.elapsed() < FLOW_IDLE);
    }

    fn set_target(&self, t: Option<SocketAddr>) {
        let mut cur = self.target.lock().unwrap();
        if *cur != t {
            // The server's voice port moved (or went): the players' sockets
            // pointed at the old one.
            self.flows.lock().unwrap().clear();
            *cur = t;
        }
    }
}

/// A socket connected to `target`, so only its answers come in, with the
/// player's `first` packet already sent: a new tokio socket refuses
/// `try_send` until the reactor has seen it writable, which would drop it.
fn local_socket(target: SocketAddr, first: &[u8]) -> std::io::Result<UdpSocket> {
    let bind: SocketAddr = match target {
        SocketAddr::V4(a) if a.ip().is_loopback() => ([127, 0, 0, 1], 0).into(),
        SocketAddr::V4(_) => ([0, 0, 0, 0], 0).into(),
        SocketAddr::V6(a) if a.ip().is_loopback() => (std::net::Ipv6Addr::LOCALHOST, 0).into(),
        SocketAddr::V6(_) => (std::net::Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    let s = std::net::UdpSocket::bind(bind)?;
    s.connect(target)?;
    s.send(first)?;
    s.set_nonblocking(true)?;
    UdpSocket::from_std(s)
}

/// Ends a task when its owner goes.
struct Abort(tokio::task::JoinHandle<()>);

impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// How home finds the server's voice port (reads its files, so it runs off
/// the async threads): `None` while no relayed server has voice.
pub type Target = Arc<dyn Fn() -> Option<SocketAddr> + Send + Sync>;

/// Runs voice for one relay connection until the future is dropped (the
/// connection ended). `relay` is the relay's host and voice port.
pub async fn run(slot: Arc<Slot>, handle: mux::Handle, keys: Keys, relay: (String, u16), loopback: bool, target: Target) {
    let head = serde_json::to_vec(&super::super::Head { kind: "voice".into(), server: String::new(), peer: String::new() }).expect("head");
    let Ok(s) = handle.open(head).await else { return std::future::pending().await };
    let mux::Stream { tx: mut stx, rx: mut srx, .. } = s;
    let (q, mut out) = mpsc::channel::<Vec<u8>>(256);
    let _writer = Abort(tokio::spawn(async move {
        while let Some(f) = out.recv().await {
            if stx.send(&f).await.is_err() {
                return;
            }
        }
    }));
    let sock = match udp_to(&relay, loopback).await {
        Ok(s) => Some(Arc::new(s)),
        Err(e) => {
            log::info!("relay voice: no UDP to the relay ({e}); voice goes over the tunnel");
            None
        }
    };
    let (tx, rx) = keys.home();
    let link = Arc::new(Link {
        port: relay.1,
        sock: sock.clone(),
        tx: Mutex::new(tx),
        rx: Mutex::new(rx),
        stream: q,
        started: Instant::now(),
        last_ack: Mutex::new(None),
        rtt: AtomicU32::new(u32::MAX),
        flows: Mutex::new(HashMap::new()),
        target: Mutex::new(None),
        counts: Counts::default(),
    });
    *slot.0.lock().unwrap() = Some(link.clone());
    let _clear = Clear(slot.clone(), link.clone());
    let l = link.clone();
    let _reader = Abort(tokio::spawn(async move {
        let mut frames = Frames::default();
        while let Ok(Some(chunk)) = srx.recv().await {
            let Ok(got) = frames.push(&chunk) else { return };
            for (flow, pkt) in got {
                l.counts.from_relay.fetch_add(1, Ordering::Relaxed);
                l.deliver(flow, &pkt);
            }
        }
    }));
    let t = target.clone();
    link.set_target(tokio::task::spawn_blocking(move || t()).await.ok().flatten());
    let mut buf = vec![0u8; MAX_SEALED + 64];
    let mut n = 0u32;
    // Quick keepalives at first, so the card knows within seconds.
    let mut ka = tokio::time::interval(Duration::from_secs(1));
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        let recv = async {
            match &sock {
                Some(s) => s.recv(&mut buf).await.map(|k| buf[..k].to_vec()),
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            r = recv => match r {
                Ok(p) if super::is_sealed(&p) => link.sealed(&p),
                Ok(_) => {}
                // "Port unreachable" from a relay without voice; keep trying.
                Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
            },
            _ = ka.tick() => {
                // Every second for the first few, then every KEEPALIVE.
                if n < 3 || n % KEEPALIVE.as_secs().max(1) as u32 == 0 {
                    link.keepalive();
                }
                n = n.wrapping_add(1);
            }
            _ = tick.tick() => {
                link.sweep();
                let t = target.clone();
                link.set_target(tokio::task::spawn_blocking(move || t()).await.ok().flatten());
            }
        }
    }
}

struct Clear(Arc<Slot>, Arc<Link>);

impl Drop for Clear {
    fn drop(&mut self) {
        let mut s = self.0.0.lock().unwrap();
        if s.as_ref().is_some_and(|l| Arc::ptr_eq(l, &self.1)) {
            *s = None;
        }
    }
}

/// A UDP socket connected to the relay's voice port.
async fn udp_to(relay: &(String, u16), loopback: bool) -> std::io::Result<UdpSocket> {
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((relay.0.as_str(), relay.1)).await?.collect();
    let to = addrs.iter().find(|a| a.is_ipv4()).or(addrs.first()).copied().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no address"))?;
    let loopback = loopback || to.ip().is_loopback();
    let bind: SocketAddr = match (to, loopback) {
        (SocketAddr::V4(_), true) => ([127, 0, 0, 1], 0).into(),
        (SocketAddr::V4(_), false) => ([0, 0, 0, 0], 0).into(),
        (SocketAddr::V6(_), true) => (std::net::Ipv6Addr::LOCALHOST, 0).into(),
        (SocketAddr::V6(_), false) => (std::net::Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    let s = UdpSocket::bind(bind).await?;
    s.connect(to).await?;
    Ok(s)
}
