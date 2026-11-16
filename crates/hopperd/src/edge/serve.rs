//! The edge itself: holds the router port and every player's connection,
//! and lives on while hopperd restarts.
//!
//! Each player who connects gets a *leg* to hopperd over the local channel:
//! their bytes go there as data frames, and what hopperd writes comes back.
//! hopperd decides everything (which server, wake, the server list answer,
//! the lobby island, holds). When it has a player ready for a running
//! server, it sends `Splice`; the edge connects to that server on
//! 127.0.0.1, replays what hopperd would have sent it, then the player's
//! bytes hopperd hadn't read (from its own record, so nothing in flight is
//! lost), and from then on copies bytes by itself. A spliced player no
//! longer depends on hopperd.
//!
//! While no hopperd is connected, new joins are answered here (see
//! [`fallback`]): the server list gets the last answer hopperd gave with a
//! "restarting" message, and a login waits up to 20 s for hopperd to come
//! back before it is told to join again.

use super::local::{self, Stream};
use super::proto::{self, Answer, CHANNEL_BUDGET, CHUNK, CONN_QUEUE, Frame, MAX_FRAME, MAX_HELLO, Msg, PROTO, PipeInfo, SPLICE_TIMEOUT};
use hopper_core::hosting::router::{self as core, Read};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, Semaphore, mpsc, watch};

/// How long a login waits for hopperd to come back.
pub const HOLD_FOR: Duration = Duration::from_secs(20);
/// What a player hears when hopperd didn't come back in time.
pub const RESTART_KICK: &str = "Hopper Server is restarting. Join again in a few seconds.";
pub const RESTART_MOTD: &str = "Hopper Server is restarting, back in a moment";
/// What the edge keeps of a player's bytes before a splice. A handshake and
/// login are well under 1 KB; past this a hand-over can't replay them.
const MAX_RECORD: usize = 64 * 1024;
/// Connections waiting for hopperd at once (each costs a socket before
/// anyone has proved who they are).
const MAX_HELD: usize = 64;
/// Connections not yet spliced (on a leg or being answered).
const MAX_OPEN: usize = 512;
/// With no hopperd for this long and nobody on, the edge has nothing to do.
const IDLE_EXIT: Duration = Duration::from_secs(10 * 60);
/// Wrong tokens in a minute before the channel stops listening for a minute.
const MAX_FAILS: usize = 10;

enum Cmd {
    Data(Vec<u8>),
    Close,
    Splice { port: u16, replay: Vec<u8>, consumed: u64, server: String, player: String },
}

struct Conn {
    tx: mpsc::UnboundedSender<Cmd>,
    /// Bytes from hopperd in `tx` not yet written to the player (capped at
    /// [`CONN_QUEUE`]).
    queued: Arc<AtomicUsize>,
    pipe: Option<PipeInfo>,
    task: Option<tokio::task::AbortHandle>,
}

/// A connection's orders from hopperd, and its share of [`Conn::queued`].
struct Inbox {
    rx: mpsc::UnboundedReceiver<Cmd>,
    queued: Arc<AtomicUsize>,
}

struct Ctl {
    epoch: u64,
    tx: mpsc::UnboundedSender<Frame>,
    /// Player bytes in `tx` may total [`CHANNEL_BUDGET`]; past that a leg
    /// stops reading its player until the channel catches up.
    budget: Arc<Semaphore>,
}

struct Listen {
    bind: String,
    port: u16,
    task: tokio::task::JoinHandle<()>,
}

pub struct Edge {
    token: String,
    ctl: Mutex<Option<Ctl>>,
    /// The current hopperd's generation; 0 while none is connected.
    epoch: watch::Sender<u64>,
    next_gen: AtomicU64,
    conns: Mutex<HashMap<u64, Conn>>,
    next_conn: AtomicU64,
    /// Fallback server list answers by address, and the one for any other.
    answers: RwLock<(Vec<(Value, Vec<String>)>, Option<Value>)>,
    listen: Mutex<Option<Listen>>,
    held: AtomicUsize,
    draining: AtomicBool,
    fails: Mutex<VecDeque<Instant>>,
    alone_since: Mutex<Option<Instant>>,
    quit: Notify,
    /// How long a join waits for hopperd ([`HOLD_FOR`]; shorter in tests).
    hold: Duration,
}

impl Edge {
    pub fn new(token: String) -> Arc<Edge> {
        Edge::with_hold(token, HOLD_FOR)
    }

    pub fn with_hold(token: String, hold: Duration) -> Arc<Edge> {
        Arc::new(Edge {
            hold,
            token,
            ctl: Mutex::new(None),
            epoch: watch::channel(0).0,
            next_gen: AtomicU64::new(1),
            conns: Mutex::new(HashMap::new()),
            next_conn: AtomicU64::new(1),
            answers: RwLock::new((Vec::new(), None)),
            listen: Mutex::new(None),
            held: AtomicUsize::new(0),
            draining: AtomicBool::new(false),
            fails: Mutex::new(VecDeque::new()),
            alone_since: Mutex::new(Some(Instant::now())),
            quit: Notify::new(),
        })
    }

    /// Sends to hopperd generation `epoch`, if that one is still connected.
    fn send(&self, epoch: u64, f: Frame) -> bool {
        match &*self.ctl.lock().unwrap() {
            Some(c) if c.epoch == epoch => c.tx.send(f).is_ok(),
            _ => false,
        }
    }

    /// A player's bytes to hopperd generation `epoch`, waiting while the
    /// channel has [`CHANNEL_BUDGET`] of them on the way. False when that
    /// hopperd is gone.
    async fn send_data(&self, epoch: u64, conn: u64, b: Vec<u8>) -> bool {
        let budget = match &*self.ctl.lock().unwrap() {
            Some(c) if c.epoch == epoch => c.budget.clone(),
            _ => return false,
        };
        // Closed when that hopperd goes.
        let Ok(p) = budget.acquire_many_owned(b.len().min(CHANNEL_BUDGET) as u32).await else { return false };
        p.forget();
        self.send(epoch, Frame::Data(conn, b))
    }

    fn send_now(&self, f: Frame) {
        if let Some(c) = &*self.ctl.lock().unwrap() {
            let _ = c.tx.send(f);
        }
    }

    /// Whether a hopperd is on the channel now (tests wait on this instead
    /// of guessing how long the edge takes to notice one go).
    #[cfg(test)]
    pub(super) fn hopperd_connected(&self) -> bool {
        self.ctl.lock().unwrap().is_some()
    }

    pub fn pipes(&self) -> Vec<PipeInfo> {
        self.conns.lock().unwrap().values().filter_map(|c| c.pipe.clone()).collect()
    }

    fn own_port(&self) -> Option<u16> {
        self.listen.lock().unwrap().as_ref().map(|l| l.port)
    }

    fn listening(&self) -> Msg {
        Msg::Listening { port: self.own_port(), error: None }
    }

    /// Too many wrong tokens lately: the channel waits a minute.
    fn blocked(&self) -> bool {
        let mut f = self.fails.lock().unwrap();
        while f.front().is_some_and(|t| t.elapsed() > Duration::from_secs(60)) {
            f.pop_front();
        }
        f.len() >= MAX_FAILS
    }

    fn failed(&self) {
        self.fails.lock().unwrap().push_back(Instant::now());
    }

    /// Runs until told to stop (or idle long enough): the channel and the
    /// housekeeping. The player port is opened when hopperd says which.
    pub async fn run(self: Arc<Self>, mut channel: local::Listener) {
        let me = self.clone();
        let tick = tokio::spawn(async move { me.housekeeping().await });
        let me = self.clone();
        let accept = tokio::spawn(async move {
            loop {
                while me.blocked() {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
                match channel.accept().await {
                    // The limit can be reached while this accept waits.
                    Ok(_) if me.blocked() => {}
                    Ok((s, peer)) => {
                        let e = me.clone();
                        tokio::spawn(async move { e.session(s, peer).await });
                    }
                    Err(e) => {
                        log::warn!("router: channel: {e:#}");
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                }
            }
        });
        self.quit.notified().await;
        accept.abort();
        tick.abort();
        if let Some(l) = self.listen.lock().unwrap().take() {
            l.task.abort();
        }
        for c in self.conns.lock().unwrap().values() {
            if let Some(t) = &c.task {
                t.abort();
            }
        }
        *self.ctl.lock().unwrap() = None;
        self.epoch.send_replace(0);
    }

    pub fn stop(&self, drain: bool) {
        if let Some(l) = self.listen.lock().unwrap().take() {
            l.task.abort();
            log::info!("router: stopped listening on port {}", l.port);
        }
        if drain && !self.conns.lock().unwrap().is_empty() {
            log::info!("router: stopping when the last player leaves");
            self.draining.store(true, Ordering::SeqCst);
        } else {
            log::info!("router: stopping");
            self.quit.notify_one();
        }
    }

    async fn housekeeping(self: Arc<Self>) {
        let mut every = tokio::time::interval(Duration::from_secs(5));
        loop {
            every.tick().await;
            let empty = self.conns.lock().unwrap().is_empty();
            if self.draining.load(Ordering::SeqCst) && empty {
                log::info!("router: the last player left; stopping");
                self.quit.notify_one();
            }
            let alone = *self.alone_since.lock().unwrap();
            if empty && alone.is_some_and(|t| t.elapsed() > IDLE_EXIT) {
                log::info!("router: no Hopper Server for {} minutes and nobody on; stopping", IDLE_EXIT.as_secs() / 60);
                self.quit.notify_one();
            }
            // The Players tab's ping, read from each pipe's own socket.
            let piped: Vec<u64> = self.conns.lock().unwrap().iter().filter(|(_, c)| c.pipe.is_some()).map(|(id, _)| *id).collect();
            if !piped.is_empty() {
                let rtts: HashMap<String, Option<u32>> = crate::player_ping::rtt_all().into_iter().map(|(k, _, ms)| (k, ms)).collect();
                let list = piped.into_iter().map(|id| (id, rtts.get(&id.to_string()).copied().flatten())).collect();
                self.send_now(Frame::Msg(Msg::Pings { list }));
            }
        }
    }

    /// One hopperd connection: the token first, then its orders, until it
    /// goes. A newer one takes over from an older one.
    async fn session(self: Arc<Self>, mut s: Stream, peer: local::Peer) {
        let hello = tokio::time::timeout(Duration::from_secs(5), proto::read(&mut s, MAX_HELLO)).await;
        let proto_v = match hello {
            Ok(Ok(Some(Frame::Msg(Msg::Hello { token, proto })))) if proto::same(&token, &self.token) => proto,
            _ => {
                self.failed();
                log::warn!("router: refused a channel connection without the right token (process {:?})", peer.pid);
                return;
            }
        };
        let welcome = Msg::Welcome {
            proto: PROTO,
            version: env!("CARGO_PKG_VERSION").into(),
            pid: std::process::id(),
            listening: self.own_port(),
            error: None,
            pipes: self.pipes(),
        };
        if proto_v != PROTO {
            // Says what it is, so hopperd can plan around it, and takes no orders.
            let _ = proto::write(&mut s, &Frame::Msg(welcome)).await;
            log::warn!("router: Hopper Server speaks channel version {proto_v}, this router {PROTO}");
            return;
        }
        let (mut r, mut w) = tokio::io::split(s);
        let (tx, mut rx) = mpsc::unbounded_channel::<Frame>();
        let _ = tx.send(Frame::Msg(welcome));
        let epoch = self.next_gen.fetch_add(1, Ordering::SeqCst);
        let budget = Arc::new(Semaphore::new(CHANNEL_BUDGET));
        // Queued after the welcome, so legs opened from now on follow it.
        *self.ctl.lock().unwrap() = Some(Ctl { epoch, tx, budget: budget.clone() });
        *self.alone_since.lock().unwrap() = None;
        self.epoch.send_replace(epoch);
        log::info!("router: Hopper Server connected (process {:?})", peer.pid);
        let b = budget.clone();
        let writer = tokio::spawn(async move {
            while let Some(f) = rx.recv().await {
                if proto::write(&mut w, &f).await.is_err() {
                    break;
                }
                if let Frame::Data(_, d) = &f {
                    b.add_permits(d.len().min(CHANNEL_BUDGET));
                }
            }
        });
        loop {
            match proto::read(&mut r, MAX_FRAME).await {
                Ok(Some(f)) => self.order(epoch, f).await,
                Ok(None) => break,
                Err(e) => {
                    log::warn!("router: channel: {e:#}");
                    break;
                }
            }
        }
        writer.abort();
        // Legs waiting for room on this channel stop waiting.
        budget.close();
        let mut ctl = self.ctl.lock().unwrap();
        if ctl.as_ref().is_some_and(|c| c.epoch == epoch) {
            *ctl = None;
            *self.alone_since.lock().unwrap() = Some(Instant::now());
            self.epoch.send_replace(0);
            log::info!("router: Hopper Server went away; players on servers stay connected");
        }
    }

    async fn order(self: &Arc<Self>, epoch: u64, f: Frame) {
        let to = |conn: u64, c: Cmd| self.conns.lock().unwrap().get(&conn).is_some_and(|x| x.tx.send(c).is_ok());
        match f {
            Frame::Data(conn, b) => {
                if let Some(x) = self.conns.lock().unwrap().get(&conn) {
                    // A player who doesn't read what hopperd writes is let go
                    // rather than kept in memory.
                    let n = b.len();
                    if x.queued.fetch_add(n, Ordering::SeqCst) + n > CONN_QUEUE {
                        log::debug!("router: connection {conn} isn't reading; closing it");
                        let _ = x.tx.send(Cmd::Close);
                    } else {
                        let _ = x.tx.send(Cmd::Data(b));
                    }
                }
            }
            Frame::Msg(Msg::Close { conn }) => {
                to(conn, Cmd::Close);
            }
            Frame::Msg(Msg::Splice { conn, port, replay, consumed, server, player }) => {
                let replay = proto::unb64(&replay).unwrap_or_default();
                if !to(conn, Cmd::Splice { port, replay, consumed, server, player }) {
                    self.send(epoch, Frame::Msg(Msg::Spliced { conn, ok: false, error: Some("the player left".into()), pipe: None }));
                }
            }
            Frame::Msg(Msg::Listen { bind, port }) => {
                let r = self.set_listen(bind, port).await;
                self.send(epoch, Frame::Msg(r));
            }
            Frame::Msg(Msg::Answers { list, unknown }) => {
                let list = list.into_iter().map(|Answer { addresses, status }| (status, addresses)).collect();
                *self.answers.write().unwrap() = (list, Some(unknown));
            }
            Frame::Msg(Msg::Stop { drain }) => self.stop(drain),
            Frame::Msg(other) => log::debug!("router: ignored {other:?}"),
        }
    }

    async fn set_listen(self: &Arc<Self>, bind: String, port: Option<u16>) -> Msg {
        let same = self.listen.lock().unwrap().as_ref().is_some_and(|l| Some(l.port) == port && l.bind == bind);
        if same {
            return self.listening();
        }
        if let Some(l) = self.listen.lock().unwrap().take() {
            l.task.abort();
            log::info!("router: stopped listening on port {}", l.port);
        }
        let Some(port) = port else { return self.listening() };
        self.draining.store(false, Ordering::SeqCst);
        // The old listener's socket closes as its task ends; a moment to let go.
        let mut tries = 0;
        let l = loop {
            match TcpListener::bind((bind.as_str(), port)).await {
                Ok(l) => break l,
                Err(_) if tries < 5 => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => return Msg::Listening { port: None, error: Some(e.to_string()) },
            }
        };
        log::info!("router: listening for Minecraft on {bind}:{port}");
        let me = self.clone();
        let task = tokio::spawn(async move { me.accept(l).await });
        *self.listen.lock().unwrap() = Some(Listen { bind, port, task });
        self.listening()
    }

    async fn accept(self: Arc<Self>, l: TcpListener) {
        loop {
            let Ok((sock, peer)) = l.accept().await else {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            };
            let open = self.conns.lock().unwrap().values().filter(|c| c.pipe.is_none()).count();
            if open >= MAX_OPEN {
                continue;
            }
            let _ = sock.set_nodelay(true);
            let id = self.next_conn.fetch_add(1, Ordering::SeqCst);
            let (tx, rx) = mpsc::unbounded_channel();
            let queued = Arc::new(AtomicUsize::new(0));
            self.conns.lock().unwrap().insert(id, Conn { tx, queued: queued.clone(), pipe: None, task: None });
            let me = self.clone();
            let task = tokio::spawn(async move {
                me.clone().conn(id, sock, peer, Inbox { rx, queued }).await;
                me.conns.lock().unwrap().remove(&id);
            });
            if let Some(c) = self.conns.lock().unwrap().get_mut(&id) {
                c.task = Some(task.abort_handle());
            }
        }
    }

    async fn conn(self: Arc<Self>, id: u64, mut sock: TcpStream, peer: SocketAddr, rx: Inbox) {
        let mut gens = self.epoch.subscribe();
        let mut record = Vec::new();
        let mut epoch = *gens.borrow_and_update();
        if epoch == 0 {
            match self.fallback(&mut sock, &mut record, &mut gens).await {
                Some(g) => epoch = g,
                None => return,
            }
        }
        if !self.send(epoch, Frame::Msg(Msg::Open { conn: id, peer: peer.to_string() })) {
            return;
        }
        if !record.is_empty() && !self.send_data(epoch, id, record.clone()).await {
            return;
        }
        self.leg(id, sock, record, epoch, rx, gens).await;
    }

    /// A connection on hopperd's leg, until hopperd is done with it or
    /// splices it.
    async fn leg(self: &Arc<Self>, id: u64, mut sock: TcpStream, mut record: Vec<u8>, epoch: u64, mut rx: Inbox, mut gens: watch::Receiver<u64>) {
        let mut overflow = record.len() > MAX_RECORD;
        let mut chunk = vec![0u8; CHUNK];
        loop {
            tokio::select! {
                r = sock.read(&mut chunk) => {
                    let n = match r { Ok(n) => n, Err(_) => 0 };
                    if n == 0 {
                        self.send(epoch, Frame::Msg(Msg::Close { conn: id }));
                        return;
                    }
                    if !overflow {
                        record.extend_from_slice(&chunk[..n]);
                        overflow = record.len() > MAX_RECORD;
                        if overflow {
                            record = Vec::new();
                        }
                    }
                    if !self.send_data(epoch, id, chunk[..n].to_vec()).await {
                        return;
                    }
                }
                c = rx.rx.recv() => match c {
                    Some(Cmd::Data(b)) => {
                        rx.queued.fetch_sub(b.len(), Ordering::SeqCst);
                        if !matches!(tokio::time::timeout(Duration::from_secs(10), sock.write_all(&b)).await, Ok(Ok(()))) {
                            self.send(epoch, Frame::Msg(Msg::Close { conn: id }));
                            return;
                        }
                    }
                    Some(Cmd::Splice { port, replay, consumed, server, player }) => {
                        // All of it within the limit hopperd waits for, so the
                        // answer always gets there before hopperd gives up.
                        let r = tokio::time::timeout(SPLICE_TIMEOUT, self.splice(&record, overflow, port, &replay, consumed))
                            .await
                            .unwrap_or_else(|_| Err(anyhow::anyhow!("the server didn't take the connection in time")));
                        match r {
                            Ok(up) => {
                                let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                                let info = PipeInfo { conn: id, server, player, since };
                                self.send(epoch, Frame::Msg(Msg::Spliced { conn: id, ok: true, error: None, pipe: Some(info.clone()) }));
                                return self.pipe(sock, up, info).await;
                            }
                            Err(e) => {
                                log::debug!("router: couldn't hand connection {id} to port {port}: {e:#}");
                                self.send(epoch, Frame::Msg(Msg::Spliced { conn: id, ok: false, error: Some(format!("{e:#}")), pipe: None }));
                            }
                        }
                    }
                    Some(Cmd::Close) | None => {
                        let _ = sock.shutdown().await;
                        return;
                    }
                },
                // hopperd went (or another took over): this leg is gone with it.
                _ = gens.changed() => {
                    if *gens.borrow() != epoch {
                        return;
                    }
                }
            }
        }
    }

    /// Connects to the server and replays: what hopperd would have written,
    /// then the player's bytes past what hopperd read.
    async fn splice(&self, record: &[u8], overflow: bool, port: u16, replay: &[u8], consumed: u64) -> anyhow::Result<TcpStream> {
        // Only this machine, and never back into the router itself: a
        // confused or hostile channel can't make the edge a proxy to
        // anywhere else.
        if port == 0 || Some(port) == self.own_port() {
            anyhow::bail!("port {port} isn't a server's");
        }
        if overflow {
            anyhow::bail!("too much was sent before the hand-over");
        }
        let consumed = usize::try_from(consumed).unwrap_or(usize::MAX);
        if consumed > record.len() || replay.len() > MAX_RECORD {
            anyhow::bail!("hand-over doesn't match what the player sent");
        }
        // (The caller limits all of this to SPLICE_TIMEOUT.)
        let mut up = TcpStream::connect(("127.0.0.1", port)).await?;
        let _ = up.set_nodelay(true);
        up.write_all(replay).await?;
        up.write_all(&record[consumed..]).await?;
        Ok(up)
    }

    async fn pipe(self: &Arc<Self>, mut sock: TcpStream, mut up: TcpStream, info: PipeInfo) {
        let id = info.conn;
        let player = info.player.clone();
        if let Some(c) = self.conns.lock().unwrap().get_mut(&id) {
            c.pipe = Some(info);
        }
        {
            // Listed by connection number for the ping readings; dropped
            // before the socket (see player_ping).
            let _listed = crate::player_ping::listen(&id.to_string(), &player, &sock);
            let _ = tokio::io::copy_bidirectional(&mut sock, &mut up).await;
        }
        self.conns.lock().unwrap().remove(&id);
        self.send_now(Frame::Msg(Msg::Ended { conn: id }));
    }

    /// No hopperd: answer the server list from what it last gave, hold a
    /// login until it's back (up to [`HOLD_FOR`]). The generation of the
    /// hopperd that came back, to hand the login to.
    async fn fallback(&self, sock: &mut TcpStream, record: &mut Vec<u8>, gens: &mut watch::Receiver<u64>) -> Option<u64> {
        let r = async {
            let (hs, used) = read_until(sock, record, 0, |b| {
                if core::is_legacy_ping(b) {
                    anyhow::bail!("pre-1.7 ping");
                }
                core::read_handshake(b)
            })
            .await?;
            anyhow::Ok((hs, used))
        };
        // hopperd may be back before the handshake is in: then it's its.
        let (hs, used) = tokio::select! {
            r = r => r.ok()?,
            _ = wait_for_gen(gens) => return Some(*gens.borrow()),
        };
        let now = *gens.borrow();
        if now != 0 {
            return Some(now);
        }
        if !hs.is_login() {
            let answer = self.answer_for(&hs);
            let _ = status(sock, record, used, &answer).await;
            return None;
        }
        if self.held.fetch_add(1, Ordering::SeqCst) >= MAX_HELD {
            self.held.fetch_sub(1, Ordering::SeqCst);
            let _ = kick(sock, RESTART_KICK).await;
            return None;
        }
        struct Held<'a>(&'a AtomicUsize);
        impl Drop for Held<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let _held = Held(&self.held);
        let deadline = tokio::time::Instant::now() + self.hold;
        let mut chunk = [0u8; 1024];
        loop {
            tokio::select! {
                _ = wait_for_gen(gens) => return Some(*gens.borrow()),
                n = sock.read(&mut chunk) => {
                    let n = n.unwrap_or(0);
                    if n == 0 || record.len() + n > MAX_RECORD {
                        return None;
                    }
                    record.extend_from_slice(&chunk[..n]);
                }
                _ = tokio::time::sleep_until(deadline) => {
                    let _ = kick(sock, RESTART_KICK).await;
                    return None;
                }
            }
        }
    }

    fn answer_for(&self, hs: &core::Handshake) -> Value {
        let a = self.answers.read().unwrap();
        let mut v = core::pick(&hs.host, &a.0).cloned().or_else(|| a.1.clone()).unwrap_or_else(|| {
            json!({ "version": { "name": "Hopper", "protocol": hs.protocol }, "players": { "max": 0, "online": 0 } })
        });
        if v.is_object() {
            v["description"] = json!({ "text": RESTART_MOTD, "color": "gold" });
            // Never seen the server answer: no version to show, so not "incompatible" either.
            if v["version"]["protocol"].as_i64() == Some(-1) {
                v["version"]["protocol"] = json!(hs.protocol);
            }
        }
        v
    }
}

/// Resolves when a hopperd is connected (now or later).
async fn wait_for_gen(gens: &mut watch::Receiver<u64>) {
    loop {
        if *gens.borrow_and_update() != 0 {
            return;
        }
        if gens.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

async fn read_until<T>(sock: &mut TcpStream, buf: &mut Vec<u8>, from: usize, parse: impl Fn(&[u8]) -> anyhow::Result<Read<T>>) -> anyhow::Result<(T, usize)> {
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

fn packet(b: &[u8], max: usize) -> anyhow::Result<Read<(i32, Vec<u8>)>> {
    Ok(match core::read_packet(b, max)? {
        Read::Done((id, body), n) => Read::Done((id, body.to_vec()), n),
        Read::More => Read::More,
    })
}

async fn status(sock: &mut TcpStream, buf: &mut Vec<u8>, used: usize, answer: &Value) -> anyhow::Result<()> {
    let (_, after) = read_until(sock, buf, used, |b| packet(b, 16)).await?;
    sock.write_all(&core::status_packet(answer)).await?;
    if let Ok(((1, payload), _)) = read_until(sock, buf, after, |b| packet(b, 64)).await {
        sock.write_all(&core::pong_packet(&payload)).await?;
    }
    Ok(())
}

async fn kick(sock: &mut TcpStream, text: &str) -> anyhow::Result<()> {
    sock.write_all(&core::login_disconnect_packet(&json!({ "text": text }))).await?;
    sock.flush().await?;
    Ok(())
}
