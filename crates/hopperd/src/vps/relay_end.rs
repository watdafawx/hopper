//! `hopperd relay`: the VPS end of "Relay through a VPS"
//! (design/vps-relay.md, B2). No web UI and no servers: two listeners.
//!
//! - The **tunnel port** (25566): TLS with the relay's own CA (home pins
//!   it), ALPN `hopper-relay`, then the relay key before anything else
//!   ([`super::link`]). One home connection at a time; a newer one that
//!   passes the key check takes over. Failed attempts are counted per
//!   address: 10 in 10 minutes and that address is dropped unread until
//!   they age out.
//! - The **game port** (25565): players. Each connection becomes one
//!   stream to home, tagged with the server home named on its control
//!   stream. With no home connected the relay answers by itself (the
//!   server list says "Home server is offline"; a join is told so), and
//!   nothing is passed anywhere else.
//!
//! ```text
//! hopperd relay init [--data <dir>] [--key-stdin]   make the relay key (shown once) and CA
//! hopperd relay status [--data <dir>]   the certificate fingerprint and settings, as JSON
//! hopperd relay [run] [--data <dir>] [--bind <addr>] [--game-port <n>] [--tunnel-port <n>]
//! ```
//!
//! The key is printed once, to the console, and never logged; the relay
//! keeps only its SHA-256 in `relay.json` (owner-only, like the folder).

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use hopper_core::hosting::relay_codec::CodecConfig;
use hopper_core::hosting::router::{self as core, Read};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

use super::link::{self, LinkError};
use super::wire::Codec;
use super::{Control, Head, Ping, Route, RelayLine};
use crate::tunnel::mux;

const FILE: &str = "relay.json";

/// Failed tunnel attempts from one address before it is dropped unread.
const MAX_FAILS: usize = 10;
const FAIL_WINDOW: Duration = Duration::from_secs(600);
/// Player connections at once from one address (a stream each, out of the
/// tunnel's 256).
const MAX_PER_IP: usize = 8;

/// `relay.json` in the relay's data folder.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct RelayConfig {
    /// SHA-256 of the relay key, hex. Empty until `hopperd relay init`.
    pub key_sha256: String,
    /// Where both ports listen. A VPS takes players from everywhere.
    pub bind: String,
    pub game_port: u16,
    pub tunnel_port: u16,
    /// The piece cache for what home sends, in MB (default: by the
    /// machine's memory, 512 MB under 4 GB of RAM, else 1 GB).
    pub cache_mb: Option<u64>,
    /// The zstd window for what home sends, as a power of two (default 28,
    /// 256 MB).
    pub window_log: Option<u32>,
    /// Velocity on this VPS (B4), on 127.0.0.1: players of a server that
    /// checks accounts through the relay go there (0: no Velocity).
    pub velocity_port: u16,
    /// Where Velocity sends them on, on 127.0.0.1: each connection there
    /// becomes a stream to home (0: none).
    pub backend_port: u16,
    /// UDP port for voice chat (B6); 0: no voice. Players' voice mods send
    /// here, and so does home (sealed), from the connection it dialled.
    pub voice_port: u16,
}

impl Default for RelayConfig {
    fn default() -> Self {
        RelayConfig { key_sha256: String::new(), bind: "0.0.0.0".into(), game_port: core::DEFAULT_PORT, tunnel_port: 25566, cache_mb: None, window_log: None, velocity_port: 0, backend_port: 0, voice_port: hopper_core::hosting::voice_mods::SVC_DEFAULT_PORT }
    }
}

impl RelayConfig {
    pub fn load(dir: &Path) -> RelayConfig {
        std::fs::read(dir.join(FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        super::home::write_private(&dir.join(FILE), &serde_json::to_vec_pretty(self)?)
    }

    /// How the relay wants the busy direction (home to players) compressed.
    pub fn recv_codec(&self) -> Codec {
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        let mut c = CodecConfig::for_memory(sys.total_memory());
        if let Some(mb) = self.cache_mb {
            c.cache_bytes = mb << 20;
        }
        if let Some(w) = self.window_log {
            c.window_log = w;
        }
        c.into()
    }
}

/// The relay's state while it runs.
pub struct Relay {
    key: [u8; 32],
    recv: Codec,
    game_port: u16,
    /// Velocity's port on 127.0.0.1 (0: none).
    pub velocity_port: u16,
    /// The voice port (B6), when it could be opened.
    pub voice: Option<Arc<super::voice::relay::RelayVoice>>,
    home: Mutex<Option<HomeLink>>,
    epoch: AtomicU64,
    fails: Mutex<HashMap<IpAddr, VecDeque<Instant>>>,
    players: Mutex<HashMap<IpAddr, usize>>,
    /// Mux settings (tests shorten the timers).
    pub mux: mux::Config,
}

struct HomeLink {
    epoch: u64,
    handle: mux::Handle,
    routes: Vec<Route>,
}

impl Relay {
    pub fn new(key: [u8; 32], recv: Codec, game_port: u16) -> Arc<Relay> {
        Arc::new(Relay {
            key,
            recv,
            game_port,
            velocity_port: 0,
            voice: None,
            home: Mutex::new(None),
            epoch: AtomicU64::new(0),
            fails: Mutex::new(HashMap::new()),
            players: Mutex::new(HashMap::new()),
            mux: link::mux_config(),
        })
    }

    /// Home is connected and has named a server.
    #[cfg(test)]
    pub fn ready(&self) -> bool {
        self.home.lock().unwrap().as_ref().is_some_and(|h| !h.routes.is_empty() && !h.handle.is_dead())
    }

    fn blocked(&self, ip: IpAddr) -> bool {
        let mut f = self.fails.lock().unwrap();
        let Some(q) = f.get_mut(&ip) else { return false };
        while q.front().is_some_and(|t| t.elapsed() > FAIL_WINDOW) {
            q.pop_front();
        }
        if q.is_empty() {
            f.remove(&ip);
            return false;
        }
        q.len() >= MAX_FAILS
    }

    fn failed(&self, ip: IpAddr) {
        let mut f = self.fails.lock().unwrap();
        // Bounded: a flood of addresses can't grow this without end.
        if f.len() > 10_000 {
            f.retain(|_, q| q.back().is_some_and(|t| t.elapsed() < FAIL_WINDOW));
        }
        let q = f.entry(ip).or_default();
        q.push_back(Instant::now());
        while q.len() > MAX_FAILS {
            q.pop_front();
        }
    }
}

/// The TLS settings: the relay's own certificate, and only `hopper-relay`.
pub fn tls_config(dir: &Path) -> anyhow::Result<(Arc<rustls::ServerConfig>, String)> {
    let ca = crate::tls::load_or_create_ca(dir, "relay")?;
    let fingerprint = crate::tls::fingerprint(&ca.der);
    // Home pins the CA, not a name, so one name is enough.
    let cert = Arc::new(crate::tls::issue(&ca, &["hopper-relay".into()], &[[127, 0, 0, 1].into()])?);
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut c = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("TLS versions")
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(Fixed(cert)));
    c.alpn_protocols = vec![link::ALPN.to_vec()];
    Ok((Arc::new(c), fingerprint))
}

#[derive(Debug)]
struct Fixed(Arc<rustls::sign::CertifiedKey>);

impl rustls::server::ResolvesServerCert for Fixed {
    fn resolve(&self, _: rustls::server::ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(self.0.clone())
    }
}

/// Runs both listeners until the process ends.
pub async fn serve(relay: Arc<Relay>, game: TcpListener, tunnel: TcpListener, tls: Arc<rustls::ServerConfig>, backend: Option<TcpListener>) {
    let r = relay.clone();
    let rb = relay.clone();
    // Velocity's connections on to the server: each is a stream to home.
    let backends = async move {
        let Some(l) = backend else { return std::future::pending::<()>().await };
        loop {
            let Ok((sock, peer)) = l.accept().await else {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            };
            let r = rb.clone();
            tokio::spawn(async move {
                if let Err(e) = backend_conn(&r, sock).await {
                    log::debug!("relay: Velocity's connection {peer}: {e:#}");
                }
            });
        }
    };
    let voice = relay.voice.clone();
    let voice = async move {
        match voice {
            Some(v) => v.run().await,
            None => std::future::pending::<()>().await,
        }
    };
    let acceptor = tokio_rustls::TlsAcceptor::from(tls);
    let homes = async move {
        loop {
            let Ok((sock, peer)) = tunnel.accept().await else {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            };
            if r.blocked(peer.ip()) {
                continue;
            }
            let (r, acceptor) = (r.clone(), acceptor.clone());
            tokio::spawn(async move { home_conn(r, acceptor, sock, peer).await });
        }
    };
    let players = async move {
        loop {
            let Ok((sock, peer)) = game.accept().await else {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            };
            let r = relay.clone();
            tokio::spawn(async move {
                if let Err(e) = player_conn(&r, sock, peer).await {
                    log::debug!("relay: player {peer}: {e:#}");
                }
            });
        }
    };
    tokio::join!(homes, players, backends, voice);
}

async fn home_conn(r: Arc<Relay>, acceptor: tokio_rustls::TlsAcceptor, sock: TcpStream, peer: SocketAddr) {
    let _ = sock.set_nodelay(true);
    let tls = match tokio::time::timeout(Duration::from_secs(15), acceptor.accept(sock)).await {
        Ok(Ok(t)) => t,
        _ => {
            r.failed(peer.ip());
            return;
        }
    };
    if tls.get_ref().1.alpn_protocol() != Some(link::ALPN) {
        r.failed(peer.ip());
        return;
    }
    // The voice keys come from this TLS session (see `voice`), taken before
    // the link owns the connection.
    let exported: Option<super::voice::Exported> = tls.get_ref().1.export_keying_material([0u8; 64], super::voice::EXPORTER_LABEL, None).ok();
    let voice_port = r.voice.as_ref().map_or(0, |v| v.port);
    let live = match link::relay(tls, &r.key, r.recv, (r.game_port, voice_port), r.mux.clone()).await {
        Ok(l) => l,
        Err(e) => {
            r.failed(peer.ip());
            match e {
                LinkError::Refused(m) => log::warn!("relay: refused a home connection from {}: {m}", peer.ip()),
                e => log::info!("relay: a home connection from {} failed before it started: {e}", peer.ip()),
            }
            return;
        }
    };
    let mux::Tunnel { handle, mut incoming, done, .. } = live.tunnel;
    let epoch = r.epoch.fetch_add(1, Ordering::SeqCst) + 1;
    let old = r.home.lock().unwrap().replace(HomeLink { epoch, handle: handle.clone(), routes: Vec::new() });
    if let Some(old) = old {
        old.handle.close("Another home connection took over this relay.").await;
    }
    log::info!("relay: home panel connected from {}", peer.ip());
    if let (Some(v), Some(e)) = (&r.voice, &exported) {
        v.attach(epoch, &super::voice::Keys::derive(e, &r.key));
    }
    // Home opens only control and voice streams; anything else is dropped
    // (reset).
    let rc = r.clone();
    let streams = tokio::spawn(async move {
        while let Some(s) = incoming.recv().await {
            let head: Option<Head> = serde_json::from_slice(&s.head).ok();
            match head.map(|h| h.kind).as_deref() {
                Some("control") => {
                    let rc = rc.clone();
                    tokio::spawn(async move { control(&rc, epoch, s).await });
                }
                Some("voice") if rc.voice.is_some() => {
                    tokio::spawn(rc.voice.clone().expect("voice").stream(epoch, s));
                }
                _ => {}
            }
        }
    });
    let why = done.await.map(|e| e.to_string()).unwrap_or_else(|e| e.to_string());
    streams.abort();
    if let Some(v) = &r.voice {
        v.detach(epoch);
    }
    let mut h = r.home.lock().unwrap();
    if h.as_ref().is_some_and(|h| h.epoch == epoch) {
        *h = None;
    }
    drop(h);
    log::info!("relay: home panel disconnected ({why})");
}

/// Reads home's control lines: which servers it carries, and asks for the
/// players' pings (answered on the same stream).
async fn control(r: &Relay, epoch: u64, s: mux::Stream) {
    let mux::Stream { mut rx, mut tx, .. } = s;
    let mut lines = Lines::default();
    while let Ok(Some(chunk)) = rx.recv().await {
        let Ok(got) = lines.push(&chunk) else { return };
        for line in got {
            let Ok(c) = serde_json::from_slice::<Control>(&line) else { continue };
            let servers: Vec<String> = c.routes.iter().map(|rt| rt.server.clone()).collect();
            {
                let mut h = r.home.lock().unwrap();
                let Some(h) = h.as_mut().filter(|h| h.epoch == epoch) else { return };
                if h.routes != c.routes {
                    log::info!("relay: home carries {}", c.routes.iter().map(|r| if r.name.is_empty() { r.server.as_str() } else { r.name.as_str() }).collect::<Vec<_>>().join(", "));
                    h.routes = c.routes;
                }
                if let Some(v) = &r.voice {
                    v.set_enabled(epoch, h.routes.iter().any(|rt| rt.voice));
                }
            }
            if c.pings {
                let mut out = RelayLine::default();
                for server in servers {
                    for (name, ms) in crate::player_ping::readings(&server) {
                        out.pings.push(Ping { server: server.clone(), name, ms });
                    }
                }
                let mut line = serde_json::to_vec(&out).expect("pings");
                line.push(b'\n');
                if line.len() <= super::MAX_CONTROL_LINE && tx.send(&line).await.is_err() {
                    return;
                }
            }
        }
    }
}

/// Cuts a byte stream into lines of at most [`super::MAX_CONTROL_LINE`].
#[derive(Default)]
pub(super) struct Lines(Vec<u8>);

impl Lines {
    /// The whole lines so far; an error once a line is too long.
    pub(super) fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, ()> {
        self.0.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(i) = self.0.iter().position(|b| *b == b'\n') {
            let rest = self.0.split_off(i + 1);
            let mut line = std::mem::replace(&mut self.0, rest);
            line.pop();
            out.push(line);
        }
        if self.0.len() > super::MAX_CONTROL_LINE {
            return Err(());
        }
        Ok(out)
    }
}

/// Counts a player's connection against its address while it lasts.
struct PlayerSlot<'a>(&'a Relay, IpAddr);

impl Drop for PlayerSlot<'_> {
    fn drop(&mut self) {
        let mut p = self.0.players.lock().unwrap();
        if let Some(n) = p.get_mut(&self.1) {
            *n -= 1;
            if *n == 0 {
                p.remove(&self.1);
            }
        }
    }
}

async fn player_conn(r: &Relay, mut sock: TcpStream, peer: SocketAddr) -> anyhow::Result<()> {
    let _slot = {
        let mut p = r.players.lock().unwrap();
        let n = p.entry(peer.ip()).or_default();
        if *n >= MAX_PER_IP {
            anyhow::bail!("too many connections from this address");
        }
        *n += 1;
        PlayerSlot(r, peer.ip())
    };
    let _ = sock.set_nodelay(true);
    // The handshake says whether it's the server list or a join; the
    // bytes go on to home unchanged.
    let mut buf = Vec::new();
    let (hs, used) = read_until(&mut sock, &mut buf, |b| {
        if core::is_legacy_ping(b) {
            anyhow::bail!("pre-1.7 ping");
        }
        core::read_handshake(b)
    })
    .await?;
    let target = {
        let h = r.home.lock().unwrap();
        h.as_ref().filter(|h| !h.handle.is_dead()).and_then(|h| h.routes.first().map(|rt| (h.handle.clone(), rt.server.clone(), rt.velocity)))
    };
    // A joining player: list their socket here, so home's Players tab can
    // show the ping this end measures (B5). Their name is the next packet,
    // sent in the clear before any encryption; the bytes still go on as
    // they came. Declared after `sock`, so it's dropped first: a reading
    // never touches a closed socket.
    let mut _listed = None;
    if hs.is_login()
        && let Some((_, server, _)) = &target
        && let Ok((name, _)) = read_until(&mut sock, &mut buf, |b| core::read_login_name(&b[used..])).await
    {
        _listed = Some(crate::player_ping::listen(server, &name, &sock));
    }
    // A server that checks accounts through the relay: Velocity takes the
    // player (it ends their encryption, so what goes home compresses), told
    // who they are with a PROXY header. With home away the relay answers
    // as before.
    if let Some((_, _, true)) = &target
        && r.velocity_port != 0
    {
        let mut v = TcpStream::connect(("127.0.0.1", r.velocity_port)).await.context("connecting to Velocity")?;
        let _ = v.set_nodelay(true);
        let local = sock.local_addr().unwrap_or(peer);
        v.write_all(&proxy_v2(peer, local)).await?;
        v.write_all(&buf).await?;
        tokio::io::copy_bidirectional(&mut sock, &mut v).await?;
        return Ok(());
    }
    if let Some((handle, server, _)) = target {
        let head = Head { kind: "game".into(), server, peer: peer.to_string() };
        if let Ok(mut stream) = handle.open(serde_json::to_vec(&head).expect("head")).await {
            stream.tx.send(&buf).await.map_err(|e| anyhow::anyhow!("tunnel: {e:?}"))?;
            // Borrowed, so the socket outlives `_listed`.
            return super::pipe(&mut sock, stream, |_| {}, |_| {}).await.map_err(anyhow::Error::msg);
        }
    }
    offline(&mut sock, buf, &hs).await
}

/// One of Velocity's connections on to the server: a stream to home for
/// the server that checks accounts through the relay. Velocity is only
/// sent players while home is connected; if home went away since, the
/// connection ends and Velocity tells the player.
async fn backend_conn(r: &Relay, sock: TcpStream) -> anyhow::Result<()> {
    let _ = sock.set_nodelay(true);
    let target = {
        let h = r.home.lock().unwrap();
        h.as_ref().filter(|h| !h.handle.is_dead()).and_then(|h| h.routes.iter().find(|rt| rt.velocity).map(|rt| (h.handle.clone(), rt.server.clone())))
    };
    let (handle, server) = target.context("home isn't connected")?;
    let head = Head { kind: "game".into(), server, peer: "velocity".into() };
    let stream = handle.open(serde_json::to_vec(&head).expect("head")).await.map_err(|e| anyhow::anyhow!("tunnel: {e:?}"))?;
    super::pipe(sock, stream, |_| {}, |_| {}).await.map_err(anyhow::Error::msg)
}

/// A PROXY protocol v2 header (TCP, `src` the player): Velocity reads the
/// player's real address from it for its logs, limits and forwarding.
pub fn proxy_v2(src: SocketAddr, dst: SocketAddr) -> Vec<u8> {
    let v6 = |a: SocketAddr| match a {
        SocketAddr::V4(s) => std::net::SocketAddrV6::new(s.ip().to_ipv6_mapped(), s.port(), 0, 0),
        SocketAddr::V6(s) => s,
    };
    let mut h = b"\r\n\r\n\0\r\nQUIT\n".to_vec();
    h.push(0x21); // version 2, PROXY
    match (src, dst) {
        (SocketAddr::V4(s), SocketAddr::V4(d)) => {
            h.push(0x11); // TCP over IPv4
            h.extend_from_slice(&12u16.to_be_bytes());
            h.extend_from_slice(&s.ip().octets());
            h.extend_from_slice(&d.ip().octets());
            h.extend_from_slice(&s.port().to_be_bytes());
            h.extend_from_slice(&d.port().to_be_bytes());
        }
        (s, d) => {
            let (s, d) = (v6(s), v6(d));
            h.push(0x21); // TCP over IPv6
            h.extend_from_slice(&36u16.to_be_bytes());
            h.extend_from_slice(&s.ip().octets());
            h.extend_from_slice(&d.ip().octets());
            h.extend_from_slice(&s.port().to_be_bytes());
            h.extend_from_slice(&d.port().to_be_bytes());
        }
    }
    h
}

/// Reads until `parse` has a whole packet (at most 8 KB, 10 s): the
/// packet and the bytes it used.
async fn read_until<T>(sock: &mut TcpStream, buf: &mut Vec<u8>, parse: impl Fn(&[u8]) -> anyhow::Result<Read<T>>) -> anyhow::Result<(T, usize)> {
    let mut chunk = [0u8; 1024];
    loop {
        if let Read::Done(t, n) = parse(buf)? {
            return Ok((t, n));
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

/// The relay's own answer while home isn't connected.
async fn offline(sock: &mut TcpStream, buf: Vec<u8>, hs: &core::Handshake) -> anyhow::Result<()> {
    if hs.is_login() {
        let text = json!({ "text": "The home server is offline. Try again in a little while." });
        sock.write_all(&core::login_disconnect_packet(&text)).await?;
        sock.flush().await?;
        return Ok(());
    }
    // Server list: the status request, then the ping. The handshake is
    // the first packet in `buf`; what follows may already be there.
    let used = match core::read_packet(&buf, 8 * 1024)? {
        Read::Done(_, n) => n,
        Read::More => buf.len(),
    };
    let mut rest = buf[used..].to_vec();
    let packet = |b: &[u8]| -> anyhow::Result<Read<(i32, Vec<u8>)>> {
        Ok(match core::read_packet(b, 64)? {
            Read::Done((id, body), n) => Read::Done((id, body.to_vec()), n),
            Read::More => Read::More,
        })
    };
    let (id, _) = take_packet(sock, &mut rest, packet).await?;
    if id != 0 {
        return Ok(());
    }
    let status = json!({
        "version": { "name": "Hopper relay", "protocol": hs.protocol },
        "players": { "max": 0, "online": 0 },
        "description": { "text": "Home server is offline" },
    });
    sock.write_all(&core::status_packet(&status)).await?;
    if let Ok((1, payload)) = take_packet(sock, &mut rest, packet).await {
        sock.write_all(&core::pong_packet(&payload)).await?;
    }
    Ok(())
}

async fn take_packet(sock: &mut TcpStream, buf: &mut Vec<u8>, parse: impl Fn(&[u8]) -> anyhow::Result<Read<(i32, Vec<u8>)>>) -> anyhow::Result<(i32, Vec<u8>)> {
    let mut chunk = [0u8; 1024];
    loop {
        if let Read::Done(p, used) = parse(buf)? {
            buf.drain(..used);
            return Ok(p);
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

// ---- command line ------------------------------------------------------------------------

const USAGE: &str = "hopperd relay init [--data <dir>] [--key-stdin]   make the relay key (shown once) and CA
hopperd relay status [--data <dir>]   the certificate fingerprint and settings, as JSON
hopperd relay [run] [--data <dir>] [--bind <addr>] [--game-port <n>] [--tunnel-port <n>] [--velocity-port <n>] [--backend-port <n>] [--voice-port <n> (UDP, 0: none)]";

struct Args {
    data: PathBuf,
    bind: Option<String>,
    game_port: Option<u16>,
    tunnel_port: Option<u16>,
    velocity_port: Option<u16>,
    backend_port: Option<u16>,
    voice_port: Option<u16>,
    /// `init` takes the key from stdin instead of making one: the home
    /// panel's ssh setup makes it, so it never shows on a console.
    key_stdin: bool,
}

fn parse(args: &[String]) -> anyhow::Result<(String, Args)> {
    let mut a = Args { data: crate::config::data_dir().join("relay"), bind: None, game_port: None, tunnel_port: None, velocity_port: None, backend_port: None, voice_port: None, key_stdin: false };
    let mut cmd = String::from("run");
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut val = || it.next().cloned().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "init" | "run" | "status" => cmd = arg.clone(),
            "--key-stdin" => a.key_stdin = true,
            "--data" => a.data = val()?.into(),
            "--bind" => a.bind = Some(val()?),
            "--game-port" => a.game_port = Some(val()?.parse().context("--game-port takes a number")?),
            "--tunnel-port" => a.tunnel_port = Some(val()?.parse().context("--tunnel-port takes a number")?),
            "--velocity-port" => a.velocity_port = Some(val()?.parse().context("--velocity-port takes a number")?),
            "--backend-port" => a.backend_port = Some(val()?.parse().context("--backend-port takes a number")?),
            "--voice-port" => a.voice_port = Some(val()?.parse().context("--voice-port takes a number")?),
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => anyhow::bail!("Unknown argument {other}. Try hopperd relay --help."),
        }
    }
    Ok((cmd, a))
}

/// `hopperd relay ...`.
pub fn command(args: &[String]) -> anyhow::Result<()> {
    let (cmd, a) = parse(args)?;
    std::fs::create_dir_all(&a.data).with_context(|| format!("creating {}", a.data.display()))?;
    crate::private_dir(&a.data);
    let mut cfg = RelayConfig::load(&a.data);
    if let Some(b) = &a.bind {
        cfg.bind = b.clone();
    }
    if let Some(p) = a.game_port {
        cfg.game_port = p;
    }
    if let Some(p) = a.tunnel_port {
        cfg.tunnel_port = p;
    }
    if let Some(p) = a.velocity_port {
        cfg.velocity_port = p;
    }
    if let Some(p) = a.backend_port {
        cfg.backend_port = p;
    }
    if let Some(p) = a.voice_port {
        cfg.voice_port = p;
    }
    if cmd == "status" {
        // Read by the home panel's setup over ssh to check what is there;
        // the key itself isn't kept here, only its hash.
        let (_, fingerprint) = tls_config(&a.data)?;
        let out = json!({
            "version": env!("CARGO_PKG_VERSION"), "fingerprint": fingerprint, "key_sha256": cfg.key_sha256,
            "bind": cfg.bind, "game_port": cfg.game_port, "tunnel_port": cfg.tunnel_port,
            "velocity_port": cfg.velocity_port, "backend_port": cfg.backend_port, "voice_port": cfg.voice_port,
        });
        println!("{out}");
        return Ok(());
    }
    if cmd == "init" {
        let key = if a.key_stdin {
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).context("reading the key")?;
            let k = line.trim().to_string();
            anyhow::ensure!(k.strip_prefix(link::KEY_PREFIX).is_some_and(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())), "the key on stdin isn't a relay key");
            k
        } else {
            link::new_key()
        };
        cfg.key_sha256 = link::hash_hex(&link::key_hash(&key));
        cfg.save(&a.data)?;
        let (_, fingerprint) = tls_config(&a.data)?;
        if a.key_stdin {
            // The panel that sent the key has it; only the fingerprint goes back.
            println!("{}", json!({ "fingerprint": fingerprint, "key_sha256": cfg.key_sha256 }));
            return Ok(());
        }
        // To the person at the console only: the key is never logged and
        // the relay keeps only its hash, so this is the one time it shows.
        println!("Relay key (shown once): {key}");
        println!("Relay certificate fingerprint: {fingerprint}");
        println!("Tunnel port {}, game port {}.", cfg.tunnel_port, cfg.game_port);
        println!("On the home panel, set the relay's address, this key and this fingerprint (PUT /api/v1/vps-relay; see design/vps-relay.md, \"Pairing by hand\").");
        return Ok(());
    }
    if a.bind.is_some() || a.game_port.is_some() || a.tunnel_port.is_some() || a.velocity_port.is_some() || a.backend_port.is_some() || a.voice_port.is_some() {
        cfg.save(&a.data)?;
    }
    let key = link::hash_from_hex(&cfg.key_sha256).context("No relay key yet. Run hopperd relay init first.")?;
    // A scratch run listens on this PC only, like the panel.
    if a.bind.is_none() && hopper_core::paths::loopback_env() == Some(true) {
        cfg.bind = "127.0.0.1".into();
    }
    let (tls, fingerprint) = tls_config(&a.data)?;
    let recv = cfg.recv_codec();
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(async move {
        let game = TcpListener::bind((cfg.bind.as_str(), cfg.game_port)).await.with_context(|| format!("listening for players on {}:{}", cfg.bind, cfg.game_port))?;
        let tunnel = TcpListener::bind((cfg.bind.as_str(), cfg.tunnel_port)).await.with_context(|| format!("listening for home on {}:{}", cfg.bind, cfg.tunnel_port))?;
        log::info!("hopperd {} relay: players on {}:{}, home on {}:{} (certificate {fingerprint}); cache {} MB, window 2^{}", env!("CARGO_PKG_VERSION"), cfg.bind, cfg.game_port, cfg.bind, cfg.tunnel_port, recv.cache_bytes >> 20, recv.window_log);
        // Velocity's side is on this machine only.
        let backend = if cfg.backend_port != 0 { Some(TcpListener::bind(("127.0.0.1", cfg.backend_port)).await.with_context(|| format!("listening for Velocity on 127.0.0.1:{}", cfg.backend_port))?) } else { None };
        // Voice is extra: a port someone else holds leaves the relay
        // running without it (home's card says voice isn't there).
        let voice = if cfg.voice_port != 0 {
            match super::voice::relay::RelayVoice::bind(&cfg.bind, cfg.voice_port).await {
                Ok(v) => {
                    log::info!("relay: voice on {}:{}/udp", cfg.bind, v.port);
                    Some(v)
                }
                Err(e) => {
                    log::warn!("relay: no voice: can't open {}:{}/udp ({e})", cfg.bind, cfg.voice_port);
                    None
                }
            }
        } else {
            None
        };
        let mut relay = Relay::new(key, recv, cfg.game_port);
        if let Some(r) = Arc::get_mut(&mut relay) {
            r.velocity_port = cfg.velocity_port;
            r.voice = voice;
        }
        tokio::select! {
            _ = serve(relay, game, tunnel, tls, backend) => {}
            _ = tokio::signal::ctrl_c() => log::info!("relay: stopping"),
        }
        Ok(())
    })
}
