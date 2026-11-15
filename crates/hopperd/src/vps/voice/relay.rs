//! The relay's end of voice (see the module above): one UDP port on the VPS
//! that takes players' voice packets and home's sealed ones.
//!
//! - A player's packet (the voice mod's shape, at most [`MAX_PACKET`]) gets
//!   a flow id for its address (at most [`MAX_FLOWS`], 8 per address) and
//!   goes home sealed, or over the voice stream while UDP to home doesn't
//!   work. Each flow has a rate limit; packets over it are dropped.
//! - A sealed packet is checked against the current connection's keys
//!   (nothing else is read before the tag matched). Keepalives are
//!   answered; voice packets go out to the flow's player.
//! - Nothing is passed on while home isn't connected or carries no server
//!   with voice.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use super::{Bucket, FLOW_IDLE, Frames, Keys, Kind, MAX_FLOWS, MAX_FLOWS_PER_IP, MAX_PACKET, MAX_SEALED, Opener, Rejected, STALE, Sealer};
use crate::tunnel::mux;

/// What the relay's voice port has seen, for its log and the tests.
#[derive(Default)]
pub struct Counts {
    pub from_players: AtomicU64,
    pub to_home_udp: AtomicU64,
    pub to_home_tcp: AtomicU64,
    pub from_home: AtomicU64,
    pub to_players: AtomicU64,
    /// Not a voice packet, too big, or no flow for it.
    pub garbage: AtomicU64,
    /// Over a flow's rate, over the flow caps, or the sealed-packet budget.
    pub limited: AtomicU64,
    /// Sealed packets that failed the check (forged, replayed, old session).
    pub refused: AtomicU64,
    /// Home isn't there (or carries no voice): nowhere to send it.
    pub no_home: AtomicU64,
}

fn bump(c: &AtomicU64) {
    c.fetch_add(1, Ordering::Relaxed);
}

pub struct RelayVoice {
    sock: Arc<UdpSocket>,
    pub port: u16,
    st: Mutex<State>,
    pub counts: Counts,
}

struct State {
    link: Option<Link>,
    flows: HashMap<SocketAddr, Flow>,
    ids: HashMap<u32, SocketAddr>,
    per_ip: HashMap<IpAddr, usize>,
    next: u32,
    /// Sealed packets checked per second, from anyone: each costs a tag
    /// check, so a flood of fakes can't take the CPU.
    sealed: Bucket,
}

struct Link {
    epoch: u64,
    tx: Sealer,
    rx: Opener,
    /// Home's address as its last good packet came from (NAT may change it).
    home: Option<SocketAddr>,
    last: Option<Instant>,
    /// Home said it hears the relay's answers: UDP works both ways.
    home_hears: bool,
    /// The voice stream on the TCP tunnel, while UDP doesn't work.
    stream: Option<mpsc::Sender<Vec<u8>>>,
    /// Home carries a server with voice.
    enabled: bool,
}

impl Link {
    fn udp(&self) -> Option<SocketAddr> {
        self.home.filter(|_| self.home_hears && self.last.is_some_and(|t| t.elapsed() < STALE))
    }
}

struct Flow {
    id: u32,
    last: Instant,
    up: Bucket,
    down: Bucket,
}

impl RelayVoice {
    pub async fn bind(bind: &str, port: u16) -> std::io::Result<Arc<RelayVoice>> {
        let sock = UdpSocket::bind((bind, port)).await?;
        let port = sock.local_addr()?.port();
        Ok(Arc::new(RelayVoice {
            sock: Arc::new(sock),
            port,
            st: Mutex::new(State { link: None, flows: HashMap::new(), ids: HashMap::new(), per_ip: HashMap::new(), next: 1, sealed: Bucket::new(5000.0, 10000.0) }),
            counts: Counts::default(),
        }))
    }

    /// A new home connection's keys; the old connection's stop working.
    pub fn attach(&self, epoch: u64, keys: &Keys) {
        let (tx, rx) = keys.relay();
        let mut st = self.st.lock().unwrap();
        st.link = Some(Link { epoch, tx, rx, home: None, last: None, home_hears: false, stream: None, enabled: false });
    }

    pub fn detach(&self, epoch: u64) {
        let mut st = self.st.lock().unwrap();
        if st.link.as_ref().is_some_and(|l| l.epoch == epoch) {
            st.link = None;
        }
    }

    pub fn set_enabled(&self, epoch: u64, on: bool) {
        if let Some(l) = self.st.lock().unwrap().link.as_mut().filter(|l| l.epoch == epoch) {
            l.enabled = on;
        }
    }

    /// Home and UDP both ways work.
    #[cfg(test)]
    pub fn udp_up(&self) -> bool {
        self.st.lock().unwrap().link.as_ref().is_some_and(|l| l.udp().is_some())
    }

    #[cfg(test)]
    pub fn flows(&self) -> usize {
        self.st.lock().unwrap().flows.len()
    }

    /// Home's voice stream on the TCP tunnel: packets both ways while UDP
    /// doesn't get through.
    pub async fn stream(self: Arc<Self>, epoch: u64, s: mux::Stream) {
        let mux::Stream { mut tx, mut rx, .. } = s;
        // Voice is lossy anyway: a full queue drops a packet rather than
        // holding the port's loop up.
        let (q, mut out) = mpsc::channel::<Vec<u8>>(256);
        match self.st.lock().unwrap().link.as_mut().filter(|l| l.epoch == epoch) {
            Some(l) => l.stream = Some(q),
            None => return,
        }
        let writer = tokio::spawn(async move {
            while let Some(f) = out.recv().await {
                if tx.send(&f).await.is_err() {
                    return;
                }
            }
        });
        let mut frames = Frames::default();
        while let Ok(Some(chunk)) = rx.recv().await {
            let Ok(got) = frames.push(&chunk) else { break };
            for (flow, pkt) in got {
                bump(&self.counts.from_home);
                self.to_player(flow, &pkt);
            }
        }
        writer.abort();
        if let Some(l) = self.st.lock().unwrap().link.as_mut().filter(|l| l.epoch == epoch) {
            l.stream = None;
        }
    }

    /// Reads the port until the process ends; old flows go every few seconds.
    pub async fn run(self: Arc<Self>) {
        let mut buf = vec![0u8; MAX_SEALED + 64];
        let mut sweep = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                r = self.sock.recv_from(&mut buf) => match r {
                    Ok((n, from)) => self.datagram(from, &buf[..n]),
                    // An ICMP "port unreachable" from a player that went away
                    // shows up as an error on some systems; keep reading.
                    Err(_) => tokio::time::sleep(Duration::from_millis(1)).await,
                },
                _ = sweep.tick() => self.sweep(),
            }
        }
    }

    fn sweep(&self) {
        let mut st = self.st.lock().unwrap();
        let old: Vec<SocketAddr> = st.flows.iter().filter(|(_, f)| f.last.elapsed() > FLOW_IDLE).map(|(a, _)| *a).collect();
        for a in old {
            forget(&mut st, a);
        }
    }

    /// One datagram on the voice port.
    pub fn datagram(&self, from: SocketAddr, pkt: &[u8]) {
        if pkt.len() > MAX_SEALED {
            bump(&self.counts.garbage);
            return;
        }
        if super::is_sealed(pkt) {
            self.sealed(from, pkt);
        } else {
            self.from_player(from, pkt);
        }
    }

    fn sealed(&self, from: SocketAddr, pkt: &[u8]) {
        let mut guard = self.st.lock().unwrap();
        let st = &mut *guard;
        let flows = st_flows(&st.flows);
        let Some(l) = st.link.as_mut() else {
            bump(&self.counts.refused);
            return;
        };
        match l.rx.plausible(pkt) {
            Ok(()) => {}
            Err(Rejected::Garbage) => return bump(&self.counts.garbage),
            Err(_) => return bump(&self.counts.refused),
        }
        // Only the tag check costs. Home's own address doesn't share the
        // budget, so fakes sent from elsewhere can't crowd its packets out
        // (a flood with the wrong session already stopped above).
        if l.home != Some(from) && !st.sealed.take() {
            bump(&self.counts.limited);
            return;
        }
        let (kind, body, counter) = match l.rx.open(pkt) {
            Ok(x) => x,
            Err(Rejected::Garbage) => return bump(&self.counts.garbage),
            Err(_) => return bump(&self.counts.refused),
        };
        // Follow home's address only on its newest packet, so a packet
        // replayed late from somewhere else can't move it.
        if l.rx.newest() == Some(counter) {
            l.home = Some(from);
        }
        l.last = Some(Instant::now());
        match kind {
            Kind::Keepalive => {
                // Home's send time (echoed for its round trip) and whether it
                // hears these answers.
                if body.len() >= 9 {
                    l.home_hears = body[8] & 1 != 0;
                }
                let mut ack = body.get(..8).map(<[u8]>::to_vec).unwrap_or_default();
                ack.extend_from_slice(&(flows as u16).to_be_bytes());
                let out = l.tx.seal(Kind::Ack, &ack);
                let _ = self.sock.try_send_to(&out, from);
            }
            Kind::Data => {
                drop(guard);
                bump(&self.counts.from_home);
                match super::split_data(&body) {
                    Some((flow, p)) => self.to_player(flow, p),
                    None => bump(&self.counts.garbage),
                }
            }
            Kind::Ack => {}
        }
    }

    fn to_player(&self, flow: u32, pkt: &[u8]) {
        if pkt.len() > MAX_PACKET {
            return bump(&self.counts.garbage);
        }
        let mut st = self.st.lock().unwrap();
        let Some(addr) = st.ids.get(&flow).copied() else { return bump(&self.counts.garbage) };
        let Some(f) = st.flows.get_mut(&addr) else { return };
        if !f.down.take() {
            return bump(&self.counts.limited);
        }
        f.last = Instant::now();
        drop(st);
        if self.sock.try_send_to(pkt, addr).is_ok() {
            bump(&self.counts.to_players);
        }
    }

    fn from_player(&self, from: SocketAddr, pkt: &[u8]) {
        if !super::player_packet_ok(pkt) {
            return bump(&self.counts.garbage);
        }
        let mut st = self.st.lock().unwrap();
        let ready = st.link.as_ref().is_some_and(|l| l.enabled && (l.udp().is_some() || l.stream.is_some()));
        if !ready {
            return bump(&self.counts.no_home);
        }
        let id = match st.flows.get_mut(&from) {
            Some(f) => {
                if !f.up.take() {
                    return bump(&self.counts.limited);
                }
                f.last = Instant::now();
                f.id
            }
            None => {
                let n = st.per_ip.get(&from.ip()).copied().unwrap_or(0);
                if st.flows.len() >= MAX_FLOWS || n >= MAX_FLOWS_PER_IP {
                    return bump(&self.counts.limited);
                }
                // Ids aren't reused while a flow has them; 0 is never one.
                let mut id = st.next;
                while id == 0 || st.ids.contains_key(&id) {
                    id = id.wrapping_add(1);
                }
                st.next = id.wrapping_add(1);
                let mut f = Flow { id, last: Instant::now(), up: super::player_bucket(), down: super::listener_bucket() };
                f.up.take();
                st.flows.insert(from, f);
                st.ids.insert(id, from);
                *st.per_ip.entry(from.ip()).or_default() += 1;
                id
            }
        };
        bump(&self.counts.from_players);
        let l = st.link.as_mut().expect("ready");
        if let Some(home) = l.udp() {
            let out = l.tx.data(id, pkt);
            drop(st);
            if self.sock.try_send_to(&out, home).is_ok() {
                bump(&self.counts.to_home_udp);
            }
        } else if let Some(q) = &l.stream {
            if q.try_send(super::frame(id, pkt)).is_ok() {
                bump(&self.counts.to_home_tcp);
            } else {
                bump(&self.counts.limited);
            }
        }
    }
}

fn st_flows(f: &HashMap<SocketAddr, Flow>) -> usize {
    f.len().min(u16::MAX as usize)
}

fn forget(st: &mut State, a: SocketAddr) {
    if let Some(f) = st.flows.remove(&a) {
        st.ids.remove(&f.id);
        if let Some(n) = st.per_ip.get_mut(&a.ip()) {
            *n -= 1;
            if *n == 0 {
                st.per_ip.remove(&a.ip());
            }
        }
    }
}
