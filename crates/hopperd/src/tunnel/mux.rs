//! Streams over one tunnel connection, with per-stream credit windows and a
//! keepalive. Transport-agnostic: anything that moves whole frames
//! ([`FrameTx`], [`FrameRx`]) works, so tests run it over channels and the
//! real thing runs it over a WebSocket (`ws.rs`).
//!
//! Flow control: each side says in its HELLO how many bytes the other may
//! send per stream before waiting (`window`). A sender takes credit before
//! every DATA frame and waits when it has none; a receiver gives credit
//! back (CREDIT) once the application has *read* half a window. So a stream
//! never has more than one window buffered on the receiving side, whatever
//! the sender does; sending past it is a protocol error that closes the
//! connection.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, Notify};

use super::frame::{code, Frame, Hello, MAX_DATA, MAX_HEAD, TUNNEL_VERSION};

/// Sends one whole frame.
pub trait FrameTx: Send + 'static {
    fn send(&mut self, frame: Vec<u8>) -> impl Future<Output = io::Result<()>> + Send;
}

/// Receives one whole frame; `None` when the connection closed.
pub trait FrameRx: Send + 'static {
    fn recv(&mut self) -> impl Future<Output = Option<io::Result<Vec<u8>>>> + Send;
}

/// Which end this is. Streams the main opens have even ids, the node's odd,
/// so both can open streams without agreeing on ids first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Main,
    Node,
}

impl Side {
    fn first_id(self) -> u32 {
        match self {
            Side::Main => 2,
            Side::Node => 1,
        }
    }
    fn owns(self, id: u32) -> bool {
        id % 2 == self.first_id() % 2
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    /// Bytes the peer may send on one stream before it waits for credit.
    pub window: u32,
    /// Open streams (both directions) before new ones are refused.
    pub max_streams: usize,
    pub ping_every: Duration,
    /// Nothing at all received for this long: the connection is dead.
    pub dead_after: Duration,
    pub hello_timeout: Duration,
    pub version: String,
    pub capabilities: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            // 512 KiB: ~5 MB/s per stream at 100 ms round trip, and at most
            // 64 x 512 KiB = 32 MiB buffered per connection in the worst case.
            window: 512 * 1024,
            max_streams: 64,
            ping_every: Duration::from_secs(20),
            dead_after: Duration::from_secs(60),
            hello_timeout: Duration::from_secs(10),
            version: env!("CARGO_PKG_VERSION").into(),
            capabilities: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub enum MuxError {
    Io(io::Error),
    /// The peer broke the protocol (bad frame, past its window, bad id).
    Protocol(String),
    /// Nothing received for `dead_after`.
    Timeout,
    /// The peer said goodbye.
    GoAway(u16, String),
    Version { ours: u16, theirs: u16 },
    /// The connection closed without a goodbye.
    Closed,
}

impl std::fmt::Display for MuxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MuxError::Io(e) => write!(f, "connection error: {e}"),
            MuxError::Protocol(m) => write!(f, "protocol error: {m}"),
            MuxError::Timeout => write!(f, "no answer for too long"),
            MuxError::GoAway(c, r) => write!(f, "closed by the other side ({c}{}{r})", if r.is_empty() { "" } else { ": " }),
            MuxError::Version { ours, theirs } => write!(f, "tunnel versions too far apart (ours {ours}, theirs {theirs})"),
            MuxError::Closed => write!(f, "connection closed"),
        }
    }
}

impl std::error::Error for MuxError {}

#[derive(Debug, PartialEq, Eq)]
pub enum StreamError {
    /// The other side reset the stream (with its code), or this side did.
    Reset(u16),
    /// The whole connection is gone.
    Closed,
}

enum Inbound {
    Data(Vec<u8>),
    Fin,
    Reset(u16),
}

struct Entry {
    /// Bytes this side may still send.
    credit: u32,
    notify: Arc<Notify>,
    inbound: mpsc::UnboundedSender<Inbound>,
    /// Bytes the peer may still send before it needs more credit.
    window_left: u32,
    local_fin: bool,
    remote_fin: bool,
}

struct State {
    streams: HashMap<u32, Entry>,
    next_id: u32,
    last_peer_id: u32,
    dead: bool,
    rtt: Option<Duration>,
    ping: Option<(u64, Instant)>,
}

struct Shared {
    side: Side,
    my_window: u32,
    peer_window: u32,
    state: Mutex<State>,
    /// OPEN, DATA and CLOSE, in order. Bounded: a busy stream waits here
    /// when the socket is slow.
    data_tx: mpsc::Sender<Frame>,
    /// CREDIT, PING, PONG, RESET, GOAWAY: small, sent ahead of data so a
    /// full data queue never holds back the credit the peer is waiting for.
    ctl_tx: mpsc::Sender<Frame>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// An id this connection has used, so a late frame for it is ignored
    /// rather than treated as an error.
    fn known(&self, st: &State, id: u32) -> bool {
        if self.side.owns(id) { id < st.next_id } else { id <= st.last_peer_id }
    }
    fn new_entry(&self) -> (Entry, Arc<Notify>, mpsc::UnboundedReceiver<Inbound>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let notify = Arc::new(Notify::new());
        let e = Entry {
            credit: self.peer_window,
            notify: notify.clone(),
            inbound: tx,
            window_left: self.my_window,
            local_fin: false,
            remote_fin: false,
        };
        (e, notify, rx)
    }
    fn stream(self: &Arc<Self>, id: u32, head: Vec<u8>, notify: Arc<Notify>, rx: mpsc::UnboundedReceiver<Inbound>) -> Stream {
        let guard = Arc::new(Guard { id, shared: self.clone() });
        Stream {
            id,
            head,
            tx: SendHalf { id, shared: self.clone(), notify, finished: false, _guard: guard.clone() },
            rx: RecvHalf { id, shared: self.clone(), inbound: rx, consumed: 0, done: false, _guard: guard },
        }
    }
}

/// A live connection.
pub struct Tunnel {
    pub handle: Handle,
    /// Streams the peer opens.
    pub incoming: mpsc::Receiver<Stream>,
    /// What the peer said in its HELLO.
    pub peer: Hello,
    /// Ends with why the connection ended.
    pub done: tokio::task::JoinHandle<MuxError>,
}

#[derive(Clone)]
pub struct Handle(Arc<Shared>);

impl Handle {
    /// Opens a stream; `head` says what it is for (JSON, at most 16 KiB).
    pub async fn open(&self, head: Vec<u8>) -> Result<Stream, StreamError> {
        if head.len() > MAX_HEAD {
            return Err(StreamError::Reset(code::REFUSED));
        }
        let (id, notify, rx) = {
            let mut st = self.0.lock();
            if st.dead {
                return Err(StreamError::Closed);
            }
            if st.streams.len() >= MAX_LOCAL_STREAMS || st.next_id > u32::MAX - 2 {
                return Err(StreamError::Reset(code::REFUSED));
            }
            let id = st.next_id;
            st.next_id += 2;
            let (e, notify, rx) = self.0.new_entry();
            st.streams.insert(id, e);
            (id, notify, rx)
        };
        let stream = self.0.stream(id, Vec::new(), notify, rx);
        self.0.data_tx.send(Frame::Open { id, head }).await.map_err(|_| StreamError::Closed)?;
        Ok(stream)
    }

    /// The last measured ping round trip.
    pub fn rtt(&self) -> Option<Duration> {
        self.0.lock().rtt
    }

    pub fn open_streams(&self) -> usize {
        self.0.lock().streams.len()
    }

    pub fn is_dead(&self) -> bool {
        self.0.lock().dead
    }

    /// Ends the connection with a goodbye the peer can show.
    pub async fn close(&self, reason: &str) {
        let _ = self.0.ctl_tx.send(Frame::GoAway { code: code::BYE, reason: reason.into() }).await;
    }
}

/// Our own streams are limited only so a runaway caller can't grow the map
/// without end; the peer's limit (`max_streams`) refuses them first.
const MAX_LOCAL_STREAMS: usize = 4096;

pub struct Stream {
    pub id: u32,
    /// The head the peer sent with OPEN (empty on streams this side opened).
    pub head: Vec<u8>,
    pub tx: SendHalf,
    pub rx: RecvHalf,
}

/// Resets the stream when both halves are dropped before it finished both
/// ways.
struct Guard {
    id: u32,
    shared: Arc<Shared>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let removed = self.shared.lock().streams.remove(&self.id);
        if let Some(e) = removed {
            if !(e.local_fin && e.remote_fin) {
                let _ = self.shared.ctl_tx.try_send(Frame::Reset { id: self.id, code: code::CANCEL, reason: String::new() });
            }
        }
    }
}

pub struct SendHalf {
    id: u32,
    shared: Arc<Shared>,
    notify: Arc<Notify>,
    finished: bool,
    _guard: Arc<Guard>,
}

impl SendHalf {
    /// Sends `data`, waiting for credit whenever the window is used up.
    pub async fn send(&mut self, data: &[u8]) -> Result<(), StreamError> {
        let mut off = 0;
        while off < data.len() {
            let n = loop {
                {
                    let mut st = self.shared.lock();
                    if st.dead {
                        return Err(StreamError::Closed);
                    }
                    let Some(e) = st.streams.get_mut(&self.id) else { return Err(StreamError::Reset(code::CANCEL)) };
                    if e.local_fin {
                        return Err(StreamError::Reset(code::CANCEL));
                    }
                    if e.credit > 0 {
                        let n = (e.credit as usize).min(data.len() - off).min(MAX_DATA);
                        e.credit -= n as u32;
                        break n;
                    }
                }
                // A CREDIT between the check and here leaves a permit, so
                // this can't miss it.
                self.notify.notified().await;
            };
            let frame = Frame::Data { id: self.id, data: data[off..off + n].to_vec() };
            self.shared.data_tx.send(frame).await.map_err(|_| StreamError::Closed)?;
            off += n;
        }
        Ok(())
    }

    /// Says this side has nothing more to send (the peer's `recv` ends).
    pub async fn finish(&mut self) -> Result<(), StreamError> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        {
            let mut st = self.shared.lock();
            let Some(e) = st.streams.get_mut(&self.id) else { return Err(StreamError::Reset(code::CANCEL)) };
            e.local_fin = true;
            if e.remote_fin {
                st.streams.remove(&self.id);
            }
        }
        self.shared.data_tx.send(Frame::Close { id: self.id }).await.map_err(|_| StreamError::Closed)
    }

    /// Kills the stream both ways.
    pub fn reset(&mut self, code: u16) {
        if self.shared.lock().streams.remove(&self.id).is_some() {
            let _ = self.shared.ctl_tx.try_send(Frame::Reset { id: self.id, code, reason: String::new() });
        }
    }

    /// Credit left (tests).
    #[cfg(test)]
    pub fn credit(&self) -> Option<u32> {
        self.shared.lock().streams.get(&self.id).map(|e| e.credit)
    }
}

pub struct RecvHalf {
    id: u32,
    shared: Arc<Shared>,
    inbound: mpsc::UnboundedReceiver<Inbound>,
    consumed: u32,
    done: bool,
    _guard: Arc<Guard>,
}

impl RecvHalf {
    /// The next chunk; `Ok(None)` once the peer has finished.
    pub async fn recv(&mut self) -> Result<Option<Vec<u8>>, StreamError> {
        if self.done {
            return Ok(None);
        }
        match self.inbound.recv().await {
            Some(Inbound::Data(d)) => {
                self.consumed += d.len() as u32;
                if self.consumed >= self.shared.my_window / 2 {
                    let n = std::mem::take(&mut self.consumed);
                    // Raise our own limit before telling the peer, so data
                    // sent on that credit is never counted as over the window.
                    let alive = match self.shared.lock().streams.get_mut(&self.id) {
                        Some(e) if !e.remote_fin => {
                            e.window_left += n;
                            true
                        }
                        _ => false,
                    };
                    if alive {
                        let _ = self.shared.ctl_tx.send(Frame::Credit { id: self.id, n }).await;
                    }
                }
                Ok(Some(d))
            }
            Some(Inbound::Fin) => {
                self.done = true;
                Ok(None)
            }
            Some(Inbound::Reset(c)) => {
                self.done = true;
                Err(StreamError::Reset(c))
            }
            None => Err(StreamError::Closed),
        }
    }

    /// Reads everything to the end.
    #[cfg(test)]
    pub async fn read_all(&mut self) -> Result<Vec<u8>, StreamError> {
        let mut out = Vec::new();
        while let Some(d) = self.recv().await? {
            out.extend_from_slice(&d);
        }
        Ok(out)
    }
}

/// Says hello, checks the peer's, and starts the connection's task.
pub async fn start<T: FrameTx, R: FrameRx>(side: Side, cfg: Config, mut tx: T, mut rx: R) -> Result<Tunnel, MuxError> {
    let hello = Hello { tunnel: TUNNEL_VERSION, version: cfg.version.clone(), capabilities: cfg.capabilities.clone(), window: cfg.window };
    let body = serde_json::to_vec(&hello).expect("hello");
    tx.send(Frame::Hello(body).encode()).await.map_err(MuxError::Io)?;
    let first = match tokio::time::timeout(cfg.hello_timeout, rx.recv()).await {
        Err(_) => return Err(MuxError::Timeout),
        Ok(None) => return Err(MuxError::Closed),
        Ok(Some(Err(e))) => return Err(MuxError::Io(e)),
        Ok(Some(Ok(b))) => b,
    };
    let peer: Hello = match Frame::decode(&first) {
        Ok(Frame::Hello(b)) => serde_json::from_slice(&b).map_err(|e| MuxError::Protocol(format!("hello: {e}")))?,
        Ok(Frame::GoAway { code, reason }) => return Err(MuxError::GoAway(code, reason)),
        Ok(f) => return Err(MuxError::Protocol(format!("expected hello, got {f:?}"))),
        Err(e) => return Err(MuxError::Protocol(format!("hello: {e}"))),
    };
    if peer.tunnel.abs_diff(TUNNEL_VERSION) > 1 {
        let reason = format!("tunnel {TUNNEL_VERSION} can't talk to tunnel {}", peer.tunnel);
        let _ = tx.send(Frame::GoAway { code: code::VERSION, reason }.encode()).await;
        return Err(MuxError::Version { ours: TUNNEL_VERSION, theirs: peer.tunnel });
    }
    if !(4096..=16 * 1024 * 1024).contains(&peer.window) {
        return Err(MuxError::Protocol(format!("window {} out of range", peer.window)));
    }

    // 8 frames (at most 512 KiB) waiting for the socket: enough to keep it
    // busy, small enough that a fast transfer doesn't leave the process
    // holding megabytes it no longer needs (64 frames left a node ~15 MiB
    // bigger after a 200 MB echo).
    let (data_tx, data_rx) = mpsc::channel(8);
    let (ctl_tx, ctl_rx) = mpsc::channel(256);
    let shared = Arc::new(Shared {
        side,
        my_window: cfg.window,
        peer_window: peer.window,
        state: Mutex::new(State {
            streams: HashMap::new(),
            next_id: side.first_id(),
            last_peer_id: 0,
            dead: false,
            rtt: None,
            ping: None,
        }),
        data_tx,
        ctl_tx,
    });
    let (incoming_tx, incoming) = mpsc::channel(cfg.max_streams.max(1));
    let s = shared.clone();
    let done = tokio::spawn(async move {
        let err = tokio::select! {
            e = read_loop(&s, &cfg, rx, incoming_tx) => e,
            e = write_loop(tx, ctl_rx, data_rx) => e,
        };
        // Everyone waiting on a stream learns the connection is gone.
        let streams = {
            let mut st = s.lock();
            st.dead = true;
            std::mem::take(&mut st.streams)
        };
        for e in streams.values() {
            e.notify.notify_one();
        }
        drop(streams);
        err
    });
    Ok(Tunnel { handle: Handle(shared), incoming, peer, done })
}

async fn write_loop<T: FrameTx>(mut tx: T, mut ctl: mpsc::Receiver<Frame>, mut data: mpsc::Receiver<Frame>) -> MuxError {
    loop {
        let f = tokio::select! {
            biased;
            Some(f) = ctl.recv() => f,
            Some(f) = data.recv() => f,
            else => return MuxError::Closed,
        };
        let bye = match &f {
            Frame::GoAway { code, reason } => Some(MuxError::GoAway(*code, reason.clone())),
            _ => None,
        };
        if let Err(e) = tx.send(f.encode()).await {
            return MuxError::Io(e);
        }
        if let Some(b) = bye {
            return b;
        }
    }
}

async fn read_loop<R: FrameRx>(shared: &Arc<Shared>, cfg: &Config, mut rx: R, incoming: mpsc::Sender<Stream>) -> MuxError {
    let mut last_seen = Instant::now();
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + cfg.ping_every, cfg.ping_every);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut nonce = 0u64;
    loop {
        let deadline = tokio::time::Instant::from_std(last_seen + cfg.dead_after);
        tokio::select! {
            r = rx.recv() => {
                let bytes = match r {
                    None => return MuxError::Closed,
                    Some(Err(e)) => return MuxError::Io(e),
                    Some(Ok(b)) => b,
                };
                last_seen = Instant::now();
                let frame = match Frame::decode(&bytes) {
                    Ok(f) => f,
                    Err(e) => return protocol(shared, format!("bad frame: {e}")),
                };
                if let Err(e) = on_frame(shared, cfg, frame, &incoming) {
                    return e;
                }
            }
            _ = ping.tick() => {
                nonce += 1;
                shared.lock().ping = Some((nonce, Instant::now()));
                if shared.ctl_tx.try_send(Frame::Ping(nonce)).is_err() {
                    return protocol(shared, "control queue full".into());
                }
            }
            _ = tokio::time::sleep_until(deadline) => return MuxError::Timeout,
        }
    }
}

/// Tells the peer why before the connection drops.
fn protocol(shared: &Shared, msg: String) -> MuxError {
    let _ = shared.ctl_tx.try_send(Frame::GoAway { code: code::PROTOCOL, reason: msg.clone() });
    MuxError::Protocol(msg)
}

fn on_frame(shared: &Arc<Shared>, cfg: &Config, frame: Frame, incoming: &mpsc::Sender<Stream>) -> Result<(), MuxError> {
    match frame {
        Frame::Hello(_) => return Err(protocol(shared, "second hello".into())),
        Frame::Ping(n) => {
            if shared.ctl_tx.try_send(Frame::Pong(n)).is_err() {
                return Err(protocol(shared, "control queue full".into()));
            }
        }
        Frame::Pong(n) => {
            let mut st = shared.lock();
            if let Some((sent, at)) = st.ping {
                if sent == n {
                    st.rtt = Some(at.elapsed());
                    st.ping = None;
                }
            }
        }
        Frame::GoAway { code, reason } => return Err(MuxError::GoAway(code, reason)),
        Frame::Open { id, head } => {
            let accepted = {
                let mut st = shared.lock();
                if shared.side.owns(id) || id <= st.last_peer_id {
                    drop(st);
                    return Err(protocol(shared, format!("stream id {id} not allowed")));
                }
                st.last_peer_id = id;
                if st.streams.len() >= cfg.max_streams {
                    None
                } else {
                    let (e, notify, rx) = shared.new_entry();
                    st.streams.insert(id, e);
                    Some((notify, rx))
                }
            };
            match accepted {
                None => {
                    let _ = shared.ctl_tx.try_send(Frame::Reset { id, code: code::REFUSED, reason: "too many streams".into() });
                }
                // Built outside the lock: dropping a stream (if nobody is
                // accepting) takes the lock to reset it.
                Some((notify, rx)) => {
                    let _ = incoming.try_send(shared.stream(id, head, notify, rx));
                }
            }
        }
        Frame::Data { id, data } => {
            let mut st = shared.lock();
            let known = shared.known(&st, id);
            let Some(e) = st.streams.get_mut(&id) else {
                drop(st);
                return if known { Ok(()) } else { Err(protocol(shared, format!("data on unknown stream {id}"))) };
            };
            if e.remote_fin {
                drop(st);
                return Err(protocol(shared, format!("data after close on stream {id}")));
            }
            if data.len() as u64 > e.window_left as u64 {
                drop(st);
                return Err(protocol(shared, format!("stream {id} sent past its window")));
            }
            e.window_left -= data.len() as u32;
            let _ = e.inbound.send(Inbound::Data(data));
        }
        Frame::Credit { id, n } => {
            let mut st = shared.lock();
            let known = shared.known(&st, id);
            let Some(e) = st.streams.get_mut(&id) else {
                drop(st);
                return if known { Ok(()) } else { Err(protocol(shared, format!("credit on unknown stream {id}"))) };
            };
            match e.credit.checked_add(n) {
                Some(c) if c <= 1 << 31 => {
                    e.credit = c;
                    e.notify.notify_one();
                }
                _ => {
                    drop(st);
                    return Err(protocol(shared, format!("credit overflow on stream {id}")));
                }
            }
        }
        Frame::Close { id } => {
            let mut st = shared.lock();
            let known = shared.known(&st, id);
            let Some(e) = st.streams.get_mut(&id) else {
                drop(st);
                return if known { Ok(()) } else { Err(protocol(shared, format!("close on unknown stream {id}"))) };
            };
            e.remote_fin = true;
            let _ = e.inbound.send(Inbound::Fin);
            if e.local_fin {
                st.streams.remove(&id);
            }
        }
        Frame::Reset { id, code, .. } => {
            let removed = shared.lock().streams.remove(&id);
            if let Some(e) = removed {
                let _ = e.inbound.send(Inbound::Reset(code));
                e.notify.notify_one();
            }
        }
    }
    Ok(())
}

/// Frames over a pair of channels, for tests.
#[cfg(test)]
pub mod mem {
    use super::*;

    pub struct Tx(pub mpsc::Sender<Vec<u8>>, pub Option<Arc<std::sync::atomic::AtomicUsize>>);
    pub struct Rx(pub mpsc::Receiver<Vec<u8>>);

    impl FrameTx for Tx {
        async fn send(&mut self, frame: Vec<u8>) -> io::Result<()> {
            if let (Some(count), Ok(Frame::Data { data, .. })) = (&self.1, Frame::decode(&frame)) {
                count.fetch_add(data.len(), std::sync::atomic::Ordering::SeqCst);
            }
            self.0.send(frame).await.map_err(|_| io::ErrorKind::BrokenPipe.into())
        }
    }
    impl FrameRx for Rx {
        async fn recv(&mut self) -> Option<io::Result<Vec<u8>>> {
            self.0.recv().await.map(Ok)
        }
    }

    /// Two connected ends; `a_data` counts DATA bytes a sends.
    pub fn pair(a_data: Option<Arc<std::sync::atomic::AtomicUsize>>) -> ((Tx, Rx), (Tx, Rx)) {
        let (a_tx, b_rx) = mpsc::channel(1024);
        let (b_tx, a_rx) = mpsc::channel(1024);
        ((Tx(a_tx, a_data), Rx(a_rx)), (Tx(b_tx, None), Rx(b_rx)))
    }
}

#[cfg(test)]
mod tests {
    use super::mem::*;
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn cfg(window: u32) -> Config {
        Config { window, ..Config::default() }
    }

    async fn connect(main_cfg: Config, node_cfg: Config, count: Option<Arc<AtomicUsize>>) -> (Tunnel, Tunnel) {
        let ((mt, mr), (nt, nr)) = pair(count);
        let (m, n) = tokio::join!(start(Side::Main, main_cfg, mt, mr), start(Side::Node, node_cfg, nt, nr));
        (m.unwrap(), n.unwrap())
    }

    #[tokio::test]
    async fn echo_and_clean_close() {
        let (main, mut node) = connect(cfg(64 * 1024), cfg(64 * 1024), None).await;
        tokio::spawn(async move {
            while let Some(mut s) = node.incoming.recv().await {
                tokio::spawn(async move {
                    while let Some(d) = s.rx.recv().await.unwrap() {
                        s.tx.send(&d).await.unwrap();
                    }
                    s.tx.finish().await.unwrap();
                });
            }
        });
        let s = main.handle.open(br#"{"kind":"echo"}"#.to_vec()).await.unwrap();
        assert_eq!(s.id, 2, "the main's streams are even");
        // 1 MiB through a 64 KiB window both ways: the sender has to wait for
        // credit many times, and reading at the same time keeps it moving.
        let payload: Vec<u8> = (0..1024 * 1024u32).map(|i| (i * 7 % 251) as u8).collect();
        let (mut tx, mut rx) = (s.tx, s.rx);
        let p = payload.clone();
        let sender = tokio::spawn(async move {
            tx.send(&p).await.unwrap();
            tx.finish().await.unwrap();
            tx
        });
        let back = rx.read_all().await.unwrap();
        let _tx = sender.await.unwrap();
        assert_eq!(back.len(), payload.len());
        assert!(back == payload);
        drop((rx, _tx));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(main.handle.open_streams(), 0, "closed both ways: forgotten");
    }

    #[tokio::test]
    async fn sender_stops_at_the_window_until_the_reader_reads() {
        let sent = Arc::new(AtomicUsize::new(0));
        let window = 256 * 1024;
        let (main, mut node) = connect(cfg(window), cfg(window), Some(sent.clone())).await;
        let mut s = main.handle.open(b"{}".to_vec()).await.unwrap();
        let mut peer = node.incoming.recv().await.unwrap();
        let total = 5 * window as usize + 123;
        let sender = tokio::spawn(async move {
            s.tx.send(&vec![1u8; total]).await.unwrap();
            s.tx.finish().await.unwrap();
            s
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!sender.is_finished(), "sender must be waiting for credit");
        assert_eq!(sent.load(Ordering::SeqCst), window as usize, "exactly one window on the wire, no more");
        // Read less than half a window: no credit yet, still stuck.
        let mut got = peer.rx.recv().await.unwrap().unwrap().len();
        assert!(got < window as usize / 2);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(sent.load(Ordering::SeqCst), window as usize);
        // Read the rest: credit flows and everything arrives.
        while let Some(d) = peer.rx.recv().await.unwrap() {
            got += d.len();
        }
        assert_eq!(got, total);
        let s = sender.await.unwrap();
        assert_eq!(sent.load(Ordering::SeqCst), total);
        // The peer gave back credit only for what it read.
        assert!(s.tx.credit().unwrap() <= window);
    }

    #[tokio::test]
    async fn dropping_a_stream_resets_it_on_the_other_side() {
        let (main, mut node) = connect(cfg(64 * 1024), cfg(64 * 1024), None).await;
        let mut s = main.handle.open(b"{}".to_vec()).await.unwrap();
        s.tx.send(b"hi").await.unwrap();
        let peer = node.incoming.recv().await.unwrap();
        drop(peer);
        assert_eq!(s.rx.recv().await, Err(StreamError::Reset(code::CANCEL)));
        // Sending on a reset stream fails instead of waiting forever.
        assert!(s.tx.send(&vec![0; 200 * 1024]).await.is_err());
        assert_eq!(main.handle.open_streams(), 0);
    }

    #[tokio::test]
    async fn explicit_reset_carries_its_code() {
        let (main, mut node) = connect(cfg(64 * 1024), cfg(64 * 1024), None).await;
        let mut s = main.handle.open(b"{}".to_vec()).await.unwrap();
        let mut peer = node.incoming.recv().await.unwrap();
        s.tx.reset(code::INTERNAL);
        assert_eq!(peer.rx.recv().await, Err(StreamError::Reset(code::INTERNAL)));
    }

    #[tokio::test]
    async fn half_close_lets_the_other_side_keep_sending() {
        let (main, mut node) = connect(cfg(64 * 1024), cfg(64 * 1024), None).await;
        let mut s = main.handle.open(b"{}".to_vec()).await.unwrap();
        s.tx.send(b"request").await.unwrap();
        s.tx.finish().await.unwrap();
        let mut peer = node.incoming.recv().await.unwrap();
        assert_eq!(peer.rx.read_all().await.unwrap(), b"request");
        peer.tx.send(b"answer").await.unwrap();
        peer.tx.finish().await.unwrap();
        assert_eq!(s.rx.read_all().await.unwrap(), b"answer");
        assert_eq!(main.handle.open_streams(), 0);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(node.handle.open_streams(), 0);
    }

    #[tokio::test]
    async fn too_many_streams_are_refused() {
        let node_cfg = Config { max_streams: 2, ..cfg(64 * 1024) };
        let (main, mut node) = connect(cfg(64 * 1024), node_cfg, None).await;
        let _a = main.handle.open(b"{}".to_vec()).await.unwrap();
        let _b = main.handle.open(b"{}".to_vec()).await.unwrap();
        let mut c = main.handle.open(b"{}".to_vec()).await.unwrap();
        assert_eq!(c.rx.recv().await, Err(StreamError::Reset(code::REFUSED)));
        assert!(node.incoming.recv().await.is_some());
        assert!(node.incoming.recv().await.is_some());
    }

    /// A raw peer: says hello, then whatever the test sends.
    async fn raw_peer(cfg_main: Config, window: u32, tunnel: u16) -> (Result<Tunnel, MuxError>, Tx, Rx) {
        let ((mt, mr), (mut pt, pr)) = pair(None);
        let hello = Hello { tunnel, version: "x".into(), capabilities: vec![], window };
        pt.send(Frame::Hello(serde_json::to_vec(&hello).unwrap()).encode()).await.unwrap();
        let t = start(Side::Main, cfg_main, mt, mr).await;
        (t, pt, pr)
    }

    #[tokio::test]
    async fn sending_past_the_window_closes_the_connection() {
        let (t, mut pt, _pr) = raw_peer(cfg(8192), 8192, TUNNEL_VERSION).await;
        let t = t.unwrap();
        pt.send(Frame::Open { id: 1, head: b"{}".to_vec() }.encode()).await.unwrap();
        pt.send(Frame::Data { id: 1, data: vec![0; 8192] }.encode()).await.unwrap();
        pt.send(Frame::Data { id: 1, data: vec![0; 1] }.encode()).await.unwrap();
        let err = t.done.await.unwrap();
        assert!(matches!(err, MuxError::Protocol(ref m) if m.contains("window")), "{err}");
    }

    #[tokio::test]
    async fn a_peer_cannot_open_with_our_ids_or_reuse_one() {
        let (t, mut pt, _pr) = raw_peer(cfg(8192), 8192, TUNNEL_VERSION).await;
        pt.send(Frame::Open { id: 2, head: vec![] }.encode()).await.unwrap();
        assert!(matches!(t.unwrap().done.await.unwrap(), MuxError::Protocol(_)));
        let (t, mut pt, _pr) = raw_peer(cfg(8192), 8192, TUNNEL_VERSION).await;
        pt.send(Frame::Open { id: 3, head: vec![] }.encode()).await.unwrap();
        pt.send(Frame::Open { id: 1, head: vec![] }.encode()).await.unwrap();
        assert!(matches!(t.unwrap().done.await.unwrap(), MuxError::Protocol(_)));
    }

    #[tokio::test]
    async fn versions_two_apart_are_refused_one_apart_accepted() {
        let (t, _pt, _pr) = raw_peer(cfg(8192), 8192, TUNNEL_VERSION + 2).await;
        assert!(matches!(t, Err(MuxError::Version { .. })));
        let (t, _pt, _pr) = raw_peer(cfg(8192), 8192, TUNNEL_VERSION + 1).await;
        assert!(t.is_ok());
    }

    #[tokio::test]
    async fn a_silent_peer_times_out() {
        let c = Config { ping_every: Duration::from_millis(50), dead_after: Duration::from_millis(200), ..cfg(8192) };
        // The raw peer says hello and then never answers a ping.
        let (t, _pt, _pr) = raw_peer(c, 8192, TUNNEL_VERSION).await;
        let at = Instant::now();
        let err = t.unwrap().done.await.unwrap();
        assert!(matches!(err, MuxError::Timeout), "{err}");
        let took = at.elapsed();
        assert!(took >= Duration::from_millis(190) && took < Duration::from_millis(600), "{took:?}");
    }

    #[tokio::test]
    async fn pings_keep_an_idle_connection_alive_and_measure_rtt() {
        let c = Config { ping_every: Duration::from_millis(30), dead_after: Duration::from_millis(100), ..cfg(8192) };
        let (main, node) = connect(c.clone(), c, None).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(!main.handle.is_dead() && !node.handle.is_dead());
        assert!(main.handle.rtt().is_some());
        // And once one side goes away, streams waiting on it fail.
        let mut s = main.handle.open(b"{}".to_vec()).await.unwrap();
        node.done.abort();
        drop(node.incoming);
        drop(node.handle);
        let err = main.done.await.unwrap();
        assert!(matches!(err, MuxError::Closed | MuxError::Io(_) | MuxError::Timeout), "{err}");
        assert_eq!(s.rx.recv().await, Err(StreamError::Closed));
    }

    #[tokio::test]
    async fn goaway_ends_both_sides() {
        let (main, node) = connect(cfg(8192), cfg(8192), None).await;
        main.handle.close("maintenance").await;
        let e = node.done.await.unwrap();
        assert!(matches!(e, MuxError::GoAway(code::BYE, ref r) if r == "maintenance"), "{e}");
    }
}
