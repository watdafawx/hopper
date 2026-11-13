//! Phase A0's spike: a throwaway main and node that talk over the tunnel,
//! to measure it. Not a feature: hidden subcommand, scratch folders only.
//!
//! ```text
//! hopperd tunnel-spike main --data <scratch> --port <p> [--echo-mb 200] [--stats-every 60] [--hold-secs 0]
//! hopperd tunnel-spike node --from <scratch>\tunnel-spike [--url wss://...]
//! HOPPERD_TUNNEL_SPIKE_NODE=<scratch>\tunnel-spike hopperd --data ... (a full panel that also dials the main)
//! ```
//! The main always listens on 127.0.0.1 (reach it from a VM through an SSH
//! reverse forward); it writes `ca.pem`, `node-token` and `url` into
//! `<data>/tunnel-spike` for the node to read.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

use super::frame::{code, MAX_FRAME};
use super::mux::{self, Config, MuxError, Side, Stream};
use super::ws;

const PATH: &str = "/api/v1/node/connect";

struct Opts {
    data: Option<PathBuf>,
    from: Option<PathBuf>,
    url: Option<String>,
    port: u16,
    echo_mb: u64,
    stats_every: u64,
    hold_secs: u64,
    window_kib: u32,
}

fn opts(args: &[String]) -> anyhow::Result<Opts> {
    let mut o = Opts { data: None, from: None, url: None, port: 18790, echo_mb: 0, stats_every: 60, hold_secs: 0, window_kib: 512 };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut v = || it.next().cloned().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--data" => o.data = Some(v()?.into()),
            "--from" => o.from = Some(v()?.into()),
            "--url" => o.url = Some(v()?),
            "--port" => o.port = v()?.parse()?,
            "--echo-mb" => o.echo_mb = v()?.parse()?,
            "--stats-every" => o.stats_every = v()?.parse()?,
            "--hold-secs" => o.hold_secs = v()?.parse()?,
            "--window-kib" => o.window_kib = v()?.parse()?,
            // Accepted so the usual scratch command line works unchanged.
            "--portable" => {}
            other => anyhow::bail!("unknown tunnel-spike argument {other}"),
        }
    }
    Ok(o)
}

pub fn command(args: &[String]) -> anyhow::Result<()> {
    let (role, rest) = args.split_first().context("tunnel-spike main|node")?;
    let o = opts(rest)?;
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    match role.as_str() {
        "main" => rt.block_on(run_main(o)),
        "node" => {
            let dir = o.from.clone().context("--from <folder with ca.pem, node-token, url>")?;
            rt.block_on(run_node(dir, o.url.clone(), o.window_kib))
        }
        _ => anyhow::bail!("tunnel-spike main|node"),
    }
}

/// A full panel that also dials a spike main (`HOPPERD_TUNNEL_SPIKE_NODE`),
/// to measure the node's real idle cost: panel plus tunnel in one process.
pub fn from_env() {
    let Some(dir) = std::env::var_os("HOPPERD_TUNNEL_SPIKE_NODE") else { return };
    tokio::spawn(async move {
        if let Err(e) = run_node(PathBuf::from(dir), None, 512).await {
            log::warn!("tunnel spike: {e:#}");
        }
    });
}

// ---- main ----

struct MainState {
    token_hash: [u8; 32],
    cfg: Config,
    echo_mb: u64,
    stats_every: u64,
}

async fn run_main(o: Opts) -> anyhow::Result<()> {
    let data = o.data.context("--data <scratch folder>")?;
    let dir = data.join("tunnel-spike");
    std::fs::create_dir_all(&dir)?;
    let ca = crate::tls::load_or_create_ca(&dir, "tunnel-spike")?;
    let cert = crate::tls::issue(&ca, &["localhost".into()], &[[127, 0, 0, 1].into()])?;
    let token = crate::auth::random_hex(32);
    std::fs::write(dir.join("node-token"), &token)?;
    let url = format!("wss://127.0.0.1:{}{PATH}", o.port);
    std::fs::write(dir.join("url"), &url)?;
    log::info!("tunnel spike main: {url}, CA {}", crate::tls::fingerprint(&ca.der));
    log::info!("node files in {}", dir.display());

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut sc = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(One(Arc::new(cert))));
    sc.alpn_protocols = vec![b"http/1.1".to_vec()];
    let sc = Arc::new(sc);

    let state = Arc::new(MainState {
        token_hash: Sha256::digest(token.as_bytes()).into(),
        cfg: Config { window: o.window_kib * 1024, ..Config::default() },
        echo_mb: o.echo_mb,
        stats_every: o.stats_every,
    });
    let router = axum::Router::new().route(PATH, axum::routing::get(upgrade)).with_state(state);
    // Loopback only, always: this is a test tool.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", o.port)).await.with_context(|| format!("port {}", o.port))?;
    let hold = o.hold_secs;
    let stop = async move {
        if hold == 0 {
            let _ = tokio::signal::ctrl_c().await;
        } else {
            tokio::time::sleep(Duration::from_secs(hold)).await;
        }
    };
    crate::net::serve(listener, router, crate::net::Tls { lan: sc.clone(), internet: sc }, stop).await;
    log::info!("tunnel spike main: done");
    Ok(())
}

#[derive(Debug)]
struct One(Arc<rustls::sign::CertifiedKey>);
impl rustls::server::ResolvesServerCert for One {
    fn resolve(&self, _: rustls::server::ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(self.0.clone())
    }
}

async fn upgrade(State(st): State<Arc<MainState>>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let given = headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
    let h: [u8; 32] = Sha256::digest(given.as_bytes()).into();
    // Compare hashes in constant time: the token never sits in memory here.
    if h.iter().zip(st.token_hash.iter()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) != 0 {
        log::warn!("tunnel spike: refused a node key");
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.read_buffer_size(16 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(2 * MAX_FRAME)
        .max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_upgrade(move |socket| async move {
            let (tx, rx) = ws::split_axum(socket);
            match mux::start(Side::Main, st.cfg.clone(), tx, rx).await {
                Ok(t) => main_session(st, t).await,
                Err(e) => log::warn!("tunnel spike: hello failed: {e}"),
            }
        })
}

async fn main_session(st: Arc<MainState>, t: mux::Tunnel) {
    log::info!("node connected: hopperd {} tunnel {} window {} KiB", t.peer.version, t.peer.tunnel, t.peer.window / 1024);
    let h = t.handle.clone();
    let checks = tokio::spawn(async move {
        // Small echo: one round trip through a stream.
        let at = Instant::now();
        match echo(&h, b"hello through the tunnel".to_vec()).await {
            Ok(back) if back == b"hello through the tunnel" => log::info!("echo ok in {:?}", at.elapsed()),
            other => log::warn!("echo failed: {other:?}"),
        }
        if st.echo_mb > 0 {
            match big_echo(&h, st.echo_mb << 20).await {
                Ok(r) => log::info!("{r}"),
                Err(e) => log::warn!("big echo failed: {e}"),
            }
        }
        loop {
            match stats(&h).await {
                Ok(s) => log::info!("node stats: {s} rtt {:?} streams {}", h.rtt(), h.open_streams()),
                Err(e) => log::warn!("stats: {e:?}"),
            }
            tokio::time::sleep(Duration::from_secs(st.stats_every)).await;
        }
    });
    let why = t.done.await.unwrap_or(MuxError::Closed);
    checks.abort();
    log::info!("node gone: {why}");
}

async fn echo(h: &mux::Handle, payload: Vec<u8>) -> Result<Vec<u8>, mux::StreamError> {
    let mut s = h.open(br#"{"kind":"echo"}"#.to_vec()).await?;
    s.tx.send(&payload).await?;
    s.tx.finish().await?;
    let mut out = Vec::new();
    while let Some(d) = s.rx.recv().await? {
        out.extend_from_slice(&d);
    }
    Ok(out)
}

/// Sends `total` bytes and reads them back at the same time, so both
/// directions run on credit; checks a CRC of what came back.
async fn big_echo(h: &mux::Handle, total: u64) -> Result<String, String> {
    let s = h.open(br#"{"kind":"echo"}"#.to_vec()).await.map_err(|e| format!("{e:?}"))?;
    let (mut tx, mut rx) = (s.tx, s.rx);
    let at = Instant::now();
    let sender = tokio::spawn(async move {
        let mut crc = crc32fast::Hasher::new();
        let mut chunk = vec![0u8; 256 * 1024];
        let mut sent = 0u64;
        let mut x = 0x9e37_79b9u32;
        while sent < total {
            for b in chunk.iter_mut() {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                *b = x as u8;
            }
            let n = (total - sent).min(chunk.len() as u64) as usize;
            crc.update(&chunk[..n]);
            tx.send(&chunk[..n]).await.map_err(|e| format!("{e:?}"))?;
            sent += n as u64;
        }
        tx.finish().await.map_err(|e| format!("{e:?}"))?;
        Ok::<_, String>(crc.finalize())
    });
    let mut crc = crc32fast::Hasher::new();
    let mut got = 0u64;
    while let Some(d) = rx.recv().await.map_err(|e| format!("{e:?}"))? {
        crc.update(&d);
        got += d.len() as u64;
    }
    let sent_crc = sender.await.map_err(|e| e.to_string())??;
    let secs = at.elapsed().as_secs_f64();
    let back = crc.finalize();
    if got != total || back != sent_crc {
        return Err(format!("sent {total} bytes crc {sent_crc:08x}, got {got} crc {back:08x}"));
    }
    Ok(format!("big echo ok: {} MiB each way in {secs:.1} s ({:.1} MiB/s each way), crc {back:08x}", total >> 20, (total >> 20) as f64 / secs))
}

async fn stats(h: &mux::Handle) -> Result<String, mux::StreamError> {
    let mut s = h.open(br#"{"kind":"stats"}"#.to_vec()).await?;
    s.tx.finish().await?;
    let mut out = Vec::new();
    while let Some(d) = s.rx.recv().await? {
        out.extend_from_slice(&d);
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

// ---- node ----

async fn run_node(dir: PathBuf, url: Option<String>, window_kib: u32) -> anyhow::Result<()> {
    let url = match url {
        Some(u) => u,
        None => std::fs::read_to_string(dir.join("url")).context("url")?.trim().to_string(),
    };
    let token = std::fs::read_to_string(dir.join("node-token")).context("node-token")?.trim().to_string();
    let ca = read_ca(&dir.join("ca.pem"))?;
    log::info!("tunnel spike node: dialing {url}, pinned CA {}", crate::tls::fingerprint(&ca));
    let tls = ws::pinned_tls(&ca)?;
    let cfg = Config { window: window_kib * 1024, ..Config::default() };
    let mut backoff = Duration::from_secs(1);
    loop {
        match ws::connect(&url, tls.clone(), &token).await {
            Ok((tx, rx)) => match mux::start(Side::Node, cfg.clone(), tx, rx).await {
                Ok(t) => {
                    log::info!("connected to the main (hopperd {})", t.peer.version);
                    let at = Instant::now();
                    let why = node_session(t).await;
                    // Start the backoff again only after a connection that
                    // lasted: a main that accepts and drops at once would
                    // otherwise get every node back each second.
                    if at.elapsed() >= Duration::from_secs(60) {
                        backoff = Duration::from_secs(1);
                    }
                    log::info!("tunnel ended: {why}");
                    if matches!(why, MuxError::GoAway(code::BYE, _)) {
                        return Ok(());
                    }
                }
                Err(e) => log::warn!("hello failed: {e}"),
            },
            Err(ws::ConnectError::Refused(s)) => {
                // A revoked key stays revoked: hammering the main would only
                // trip the rate limit for everyone behind this address.
                anyhow::bail!("the main refused this node's key (HTTP {s}); stopped");
            }
            Err(e) => log::warn!("connecting: {e}"),
        }
        let jitter = Duration::from_millis(u64::from(rand_u16() % 1000));
        tokio::time::sleep(backoff + jitter).await;
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

fn rand_u16() -> u16 {
    let mut b = [0u8; 2];
    let _ = getrandom::fill(&mut b);
    u16::from_le_bytes(b)
}

fn read_ca(p: &Path) -> anyhow::Result<rustls::pki_types::CertificateDer<'static>> {
    let pem = std::fs::read_to_string(p).with_context(|| p.display().to_string())?;
    let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    use base64::Engine as _;
    Ok(base64::engine::general_purpose::STANDARD.decode(b64)?.into())
}

async fn node_session(mut t: mux::Tunnel) -> MuxError {
    loop {
        tokio::select! {
            s = t.incoming.recv() => match s {
                Some(s) => { tokio::spawn(serve_stream(s)); }
                None => break,
            },
            r = &mut t.done => return r.unwrap_or(MuxError::Closed),
        }
    }
    t.done.await.unwrap_or(MuxError::Closed)
}

async fn serve_stream(mut s: Stream) {
    let kind = serde_json::from_slice::<serde_json::Value>(&s.head).ok().and_then(|v| v["kind"].as_str().map(String::from));
    match kind.as_deref() {
        Some("echo") => {
            loop {
                match s.rx.recv().await {
                    Ok(Some(d)) => {
                        if s.tx.send(&d).await.is_err() {
                            return;
                        }
                    }
                    Ok(None) => break,
                    Err(_) => return,
                }
            }
            let _ = s.tx.finish().await;
        }
        Some("stats") => {
            let body = own_stats().to_string();
            let _ = s.tx.send(body.as_bytes()).await;
            let _ = s.tx.finish().await;
            while let Ok(Some(_)) = s.rx.recv().await {}
        }
        _ => s.tx.reset(code::REFUSED),
    }
}

/// This process's memory and CPU, for the main to log.
fn own_stats() -> serde_json::Value {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    static SYS: Mutex<Option<(System, Instant, u64)>> = Mutex::new(None);
    let pid = Pid::from_u32(std::process::id());
    let mut g = SYS.lock().unwrap_or_else(|e| e.into_inner());
    let (sys, last_at, last_cpu) = g.get_or_insert_with(|| (System::new(), Instant::now(), 0));
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, ProcessRefreshKind::nothing().with_memory().with_cpu());
    let Some(p) = sys.process(pid) else { return serde_json::json!({}) };
    let cpu_ms = p.accumulated_cpu_time();
    let wall = last_at.elapsed().as_millis().max(1) as u64;
    let avg = (cpu_ms.saturating_sub(*last_cpu)) as f64 / wall as f64 * 100.0;
    *last_at = Instant::now();
    *last_cpu = cpu_ms;
    serde_json::json!({
        "rss_mib": (p.memory() as f64 / 1048576.0 * 10.0).round() / 10.0,
        "cpu_ms_total": cpu_ms,
        "cpu_pct_since_last": (avg * 100.0).round() / 100.0,
    })
}
