//! hopperd's side of the router edge: finds or starts the edge, keeps the
//! channel to it, tells it where to listen and what to answer while
//! hopperd is away, and turns each player it reports into a [`Leg`] that
//! `crate::router` routes like any socket.

use super::proto::{self, Answer, CHUNK, Frame, MAX_FRAME, Msg, PROTO, PipeInfo};
use super::{RunFile, local};
use crate::App;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{Notify, mpsc, oneshot};

/// The edge as hopperd last heard from it, for the router card.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Info {
    pub connected: bool,
    pub pid: Option<u32>,
    pub version: Option<String>,
    /// Players piped through it right now.
    pub players: usize,
}

struct Live {
    epoch: u64,
    tx: mpsc::UnboundedSender<Frame>,
    legs: HashMap<u64, mpsc::UnboundedSender<Vec<u8>>>,
    splices: HashMap<u64, oneshot::Sender<Result<(), String>>>,
}

#[derive(Default)]
struct Want {
    /// Where it should listen; None: it shouldn't (router off, no addresses).
    listen: Option<(String, u16)>,
    answers: Option<(Vec<Answer>, serde_json::Value)>,
}

#[derive(Default)]
pub struct Client {
    live: Mutex<Option<Live>>,
    want: Mutex<Want>,
    info: Mutex<Info>,
    wake: Notify,
    started: std::sync::atomic::AtomicBool,
    next_gen: AtomicU64,
}

/// Pipes the edge holds: conn -> (server id, player, since, ms). Static, as
/// `player_ping`'s own table is, so the Players tab can read it.
fn pipes() -> &'static Mutex<HashMap<u64, (String, String, SystemTime, Option<u32>)>> {
    static P: OnceLock<Mutex<HashMap<u64, (String, String, SystemTime, Option<u32>)>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

/// Readings for `server`'s players piped by the edge: lowercase name -> (ms, since).
pub fn readings(server: &str) -> HashMap<String, (Option<u32>, SystemTime)> {
    let mut out: HashMap<String, (Option<u32>, SystemTime)> = HashMap::new();
    for (s, name, since, ms) in pipes().lock().unwrap().values() {
        if s != server {
            continue;
        }
        let low = name.to_lowercase();
        if out.get(&low).is_some_and(|(_, t)| t <= since) {
            continue;
        }
        out.insert(low, (*ms, *since));
    }
    out
}

impl Client {
    pub fn info(&self) -> Info {
        let mut i = self.info.lock().unwrap().clone();
        i.players = pipes().lock().unwrap().len();
        i
    }

    /// Players piped through the edge now (for "Stop Hopper Server").
    pub fn players(&self) -> usize {
        pipes().lock().unwrap().len()
    }

    fn send(&self, f: Frame) -> bool {
        self.live.lock().unwrap().as_ref().is_some_and(|l| l.tx.send(f).is_ok())
    }

    /// Where the router should listen (None: nowhere). An edge that runs is
    /// told now; one is started when needed.
    pub fn set_listen(&self, to: Option<(String, u16)>) {
        let mut w = self.want.lock().unwrap();
        if w.listen == to {
            return;
        }
        let was = std::mem::replace(&mut w.listen, to.clone());
        drop(w);
        match to {
            Some((bind, port)) => {
                self.send(Frame::Msg(Msg::Listen { bind, port: Some(port) }));
            }
            // Router off or no addresses: the edge goes once nobody is on.
            None if was.is_some() => {
                self.send(Frame::Msg(Msg::Stop { drain: true }));
            }
            None => {}
        }
        self.wake.notify_one();
    }

    pub fn set_answers(&self, list: Vec<Answer>, unknown: serde_json::Value) {
        let mut w = self.want.lock().unwrap();
        if w.answers.as_ref().is_some_and(|(l, u)| *l == list && *u == unknown) {
            return;
        }
        w.answers = Some((list.clone(), unknown.clone()));
        drop(w);
        self.send(Frame::Msg(Msg::Answers { list, unknown }));
    }

    /// Starts looking after the edge (once).
    pub fn start(app: &Arc<App>) {
        if app.edge.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let app = app.clone();
        tokio::spawn(async move { supervise(app).await });
    }

    /// The panel is stopping for good: the edge stops too and its players
    /// are disconnected. An update or restart leaves it running.
    pub async fn stop_for_good(&self) {
        if self.send(Frame::Msg(Msg::Stop { drain: false })) {
            log::info!("router: stopping the router process (players on it are disconnected)");
            // Let the writer put it on the channel.
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
}

/// Finds the edge from its run file and connects to it.
async fn connect_existing(app: &App) -> anyhow::Result<Option<(local::Stream, RunFile)>> {
    let Some(r) = super::read_run(&app.boot_dir) else { return Ok(None) };
    if !super::is_ours(&r) {
        return Ok(None);
    }
    let s = local::connect(&r.channel, r.pid).await?;
    Ok(Some((s, r)))
}

async fn supervise(app: Arc<App>) {
    let mut fails = 0;
    loop {
        let wanted = app.edge.want.lock().unwrap().listen.is_some();
        let found = match connect_existing(&app).await {
            Ok(f) => f,
            Err(e) => {
                fails += 1;
                log::warn!("router: can't reach the router process: {e:#}");
                // A router that runs but won't answer: after a few tries it's
                // ended and a new one started (its players drop, as they
                // would if it had crashed).
                if fails >= 3
                    && let Some(r) = super::read_run(&app.boot_dir)
                {
                    super::kill(&r);
                    fails = 0;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let (stream, run) = match found {
            Some(f) => f,
            None if wanted => {
                let data = app.boot_dir.clone();
                match tokio::task::spawn_blocking(move || super::spawn(&data)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        log::warn!("router: couldn't start the router process: {e:#}");
                        set_error(&app, &format!("The router couldn't start: {e:#}"));
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                    Err(_) => continue,
                }
                // Its run file appears once its channel is up.
                let mut got = None;
                for _ in 0..50 {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    if let Ok(Some(f)) = connect_existing(&app).await {
                        got = Some(f);
                        break;
                    }
                }
                match got {
                    Some(f) => f,
                    None => {
                        log::warn!("router: the router process didn't come up");
                        continue;
                    }
                }
            }
            None => {
                // Nothing to do until the router is wanted again.
                let _ = tokio::time::timeout(Duration::from_secs(5), app.edge.wake.notified()).await;
                continue;
            }
        };
        fails = 0;
        match session(&app, stream, &run).await {
            Ok(()) => log::warn!("router: lost the router process {}; finding it again", run.pid),
            Err(e) => log::warn!("router: router process {}: {e:#}", run.pid),
        }
        app.edge.info.lock().unwrap().connected = false;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn set_error(app: &App, e: &str) {
    let mut st = app.router.status.lock().unwrap();
    st.listening = None;
    st.error = Some(e.to_string());
}

async fn session(app: &Arc<App>, mut s: local::Stream, run: &RunFile) -> anyhow::Result<()> {
    proto::write(&mut s, &Frame::Msg(Msg::Hello { token: run.token.clone(), proto: PROTO })).await?;
    let first = tokio::time::timeout(Duration::from_secs(5), proto::read(&mut s, MAX_FRAME)).await??;
    let Some(Frame::Msg(Msg::Welcome { proto: v, version, pid, pipes: live, .. })) = first else {
        anyhow::bail!("no welcome");
    };
    if v != PROTO {
        // Phase 4 plans an update here; for now one that can't take orders
        // is stopped when nobody is on it, and a matching one started.
        anyhow::bail!("it speaks channel version {v}, this Hopper Server {PROTO}");
    }
    log::info!("router: connected to the router process {pid} (version {version}, {} player(s) on)", live.len());
    {
        let mut p = pipes().lock().unwrap();
        p.clear();
        for PipeInfo { conn, server, player, since } in live {
            p.insert(conn, (server, player, UNIX_EPOCH + Duration::from_secs(since), None));
        }
    }
    *app.edge.info.lock().unwrap() = Info { connected: true, pid: Some(pid), version: Some(version), players: 0 };
    let (mut r, mut w) = tokio::io::split(s);
    let (tx, mut rx) = mpsc::unbounded_channel::<Frame>();
    let epoch = app.edge.next_gen.fetch_add(1, Ordering::SeqCst);
    {
        let want = app.edge.want.lock().unwrap();
        if let Some((bind, port)) = &want.listen {
            let _ = tx.send(Frame::Msg(Msg::Listen { bind: bind.clone(), port: Some(*port) }));
        } else {
            let _ = tx.send(Frame::Msg(Msg::Stop { drain: true }));
        }
        if let Some((list, unknown)) = &want.answers {
            let _ = tx.send(Frame::Msg(Msg::Answers { list: list.clone(), unknown: unknown.clone() }));
        }
        *app.edge.live.lock().unwrap() = Some(Live { epoch, tx, legs: HashMap::new(), splices: HashMap::new() });
    }
    let writer = tokio::spawn(async move {
        while let Some(f) = rx.recv().await {
            if proto::write(&mut w, &f).await.is_err() {
                break;
            }
        }
    });
    let r = read_loop(app, &mut r, epoch).await;
    writer.abort();
    // Legs end with the channel: their routes read the end of the stream.
    *app.edge.live.lock().unwrap() = None;
    r
}

async fn read_loop<R: AsyncRead + Unpin>(app: &Arc<App>, r: &mut R, epoch: u64) -> anyhow::Result<()> {
    loop {
        let Some(f) = proto::read(r, MAX_FRAME).await? else { return Ok(()) };
        match f {
            Frame::Data(conn, b) => {
                if let Some(l) = app.edge.live.lock().unwrap().as_ref()
                    && let Some(leg) = l.legs.get(&conn)
                {
                    let _ = leg.send(b);
                }
            }
            Frame::Msg(Msg::Open { conn, peer }) => {
                let Some(leg) = Leg::new(app, epoch, conn) else { continue };
                let peer = peer.parse().unwrap_or_else(|_| std::net::SocketAddr::from(([0, 0, 0, 0], 0)));
                let app = app.clone();
                tokio::spawn(async move {
                    let mut leg = leg;
                    if let Err(e) = crate::router::edge_leg(&app, &mut leg, peer).await {
                        log::debug!("router: {peer}: {e:#}");
                    }
                });
            }
            Frame::Msg(Msg::Close { conn }) => {
                if let Some(l) = app.edge.live.lock().unwrap().as_mut() {
                    l.legs.remove(&conn);
                }
            }
            Frame::Msg(Msg::Spliced { conn, ok, error }) => {
                let tx = app.edge.live.lock().unwrap().as_mut().and_then(|l| l.splices.remove(&conn));
                if let Some(tx) = tx {
                    let _ = tx.send(if ok { Ok(()) } else { Err(error.unwrap_or_default()) });
                }
            }
            Frame::Msg(Msg::Listening { port, error }) => {
                let want = app.edge.want.lock().unwrap().listen.clone();
                let mut st = app.router.status.lock().unwrap();
                st.listening = port;
                st.error = match (error, want) {
                    (Some(e), Some((_, p))) => Some(crate::router::bind_error(app, p, &e)),
                    _ => None,
                };
                if let Some(p) = port {
                    log::info!("server addresses: the router listens for Minecraft on port {p}");
                } else if let Some(e) = &st.error {
                    log::warn!("server addresses: {e}");
                }
            }
            Frame::Msg(Msg::Ended { conn }) => {
                pipes().lock().unwrap().remove(&conn);
            }
            Frame::Msg(Msg::Pings { list }) => {
                let mut p = pipes().lock().unwrap();
                let live: std::collections::HashSet<u64> = list.iter().map(|(c, _)| *c).collect();
                // A pipe that ended while hopperd was away is gone from the list.
                p.retain(|c, _| live.contains(c));
                for (c, ms) in list {
                    if let Some(e) = p.get_mut(&c) {
                        e.3 = ms;
                    }
                }
            }
            Frame::Msg(other) => log::debug!("router: ignored {other:?}"),
        }
    }
}

/// A player's connection that the edge holds: reads are their bytes, writes
/// go back to them, and [`Leg::splice`] hands them to a server.
pub struct Leg {
    app: Arc<App>,
    epoch: u64,
    conn: u64,
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
    pending: Vec<u8>,
    pos: usize,
    /// Bytes handed to the router so far: where the edge's replay goes on from.
    read: u64,
    spliced: bool,
}

impl Leg {
    fn new(app: &Arc<App>, epoch: u64, conn: u64) -> Option<Leg> {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut live = app.edge.live.lock().unwrap();
        let l = live.as_mut().filter(|l| l.epoch == epoch)?;
        l.legs.insert(conn, tx);
        Some(Leg { app: app.clone(), epoch, conn, rx, pending: Vec::new(), pos: 0, read: 0, spliced: false })
    }

    fn frame(&self, f: Frame) -> bool {
        self.app.edge.live.lock().unwrap().as_ref().filter(|l| l.epoch == self.epoch).is_some_and(|l| l.tx.send(f).is_ok())
    }

    /// Hands the player to the server on `port`: the edge writes `replay`
    /// there, then whatever the player sent past what was read here. Ok(true)
    /// once it's piped; Ok(false) if the server didn't take the connection.
    pub async fn splice(&mut self, port: u16, replay: &[u8], server: &str, player: &str) -> anyhow::Result<bool> {
        let (tx, rx) = oneshot::channel();
        {
            let mut live = self.app.edge.live.lock().unwrap();
            let l = live.as_mut().filter(|l| l.epoch == self.epoch).ok_or_else(|| anyhow::anyhow!("the router went away"))?;
            l.splices.insert(self.conn, tx);
            l.tx.send(Frame::Msg(Msg::Splice {
                conn: self.conn,
                port,
                replay: proto::b64(replay),
                consumed: self.read,
                server: server.to_string(),
                player: player.to_string(),
            }))
            .map_err(|_| anyhow::anyhow!("the router went away"))?;
        }
        match tokio::time::timeout(Duration::from_secs(10), rx).await {
            Ok(Ok(Ok(()))) => {
                self.spliced = true;
                if !player.is_empty() {
                    pipes().lock().unwrap().insert(self.conn, (server.to_string(), player.to_string(), SystemTime::now(), None));
                }
                Ok(true)
            }
            Ok(Ok(Err(e))) => {
                log::debug!("router: the server on port {port} didn't take the player: {e}");
                Ok(false)
            }
            _ => anyhow::bail!("the router didn't answer the hand-over"),
        }
    }
}

impl Drop for Leg {
    fn drop(&mut self) {
        if !self.spliced {
            self.frame(Frame::Msg(Msg::Close { conn: self.conn }));
        }
        if let Some(l) = self.app.edge.live.lock().unwrap().as_mut().filter(|l| l.epoch == self.epoch) {
            l.legs.remove(&self.conn);
            l.splices.remove(&self.conn);
        }
    }
}

impl AsyncRead for Leg {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        if self.pos >= self.pending.len() {
            match self.rx.poll_recv(cx) {
                Poll::Ready(Some(b)) => {
                    self.pending = b;
                    self.pos = 0;
                }
                // The player left or the channel closed: the end.
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
        let n = (self.pending.len() - self.pos).min(buf.remaining());
        let pos = self.pos;
        buf.put_slice(&self.pending[pos..pos + n]);
        self.pos += n;
        self.read += n as u64;
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for Leg {
    fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        // Queued in order on the channel, ahead of any splice that follows.
        let n = buf.len().min(CHUNK);
        if self.frame(Frame::Data(self.conn, buf[..n].to_vec())) {
            Poll::Ready(Ok(n))
        } else {
            Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
