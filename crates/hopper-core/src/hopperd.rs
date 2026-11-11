//! Talking to Hopper Server (`hopperd`, crates/hopperd): finding it on the
//! LAN, pairing, and the calls the desktop app makes for servers it runs:
//! their details and mods, power and console, and file transfers for sync.
//!
//! A server on a Hopper Server shows up in Hopper as a [`ServerInfo`] whose
//! `remote` is set. Its `root` is a stand-in (`hopperd:\<folder>`) that
//! keeps the folder name for matching and pairing; no local file exists
//! there, so anything that reads it directly finds nothing, quickly.

use crate::model::{ModInfo, SyncOp, SyncResult};
use crate::server::{Manager, ServerInfo};
use anyhow::{Context as _, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The panel's default port.
pub const PORT: u16 = 8790;
/// mDNS service hopperd announces.
pub const SERVICE: &str = "_hopperd._tcp.local.";
/// Upload chunk size (hopperd takes up to 32 MB).
const CHUNK: usize = 8 * 1024 * 1024;
/// Upload chunks for the panel's incoming folder (zips to make servers from).
const INCOMING_CHUNK: usize = 2 * 1024 * 1024 - 64 * 1024;
/// One chunk's limits. Sending it may take 5 minutes (8 MB at under
/// 30 KB/s), the reply then a minute (the panel answers as soon as the
/// chunk is written), the whole chunk 7 minutes. A connection that dies
/// quietly mid-chunk then ends in an error the upload recovers from,
/// instead of a wait of up to an hour that looks like a stuck task.
const CHUNK_SEND: Duration = Duration::from_secs(300);
const CHUNK_REPLY: Duration = Duration::from_secs(60);
const CHUNK_WAIT: Duration = Duration::from_secs(420);
/// Chunks in a row that may fail before an upload gives up.
const CHUNK_TRIES: u32 = 5;

/// Which Hopper Server a server is on, and its id there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    /// "https://192.168.50.3:8790"
    pub base: String,
    pub id: String,
    /// The panel's certificate authority (PEM), pinned at pairing.
    pub ca: String,
    /// Which paired panel ([`crate::settings::HopperPanel::id`]).
    pub panel: String,
}

/// Stand-in roots and the panels they're on, so code given only a path
/// (config compare, file copies, crash scan) can reach the server.
static REMOTES: std::sync::LazyLock<std::sync::Mutex<Vec<(PathBuf, Remote)>>> = std::sync::LazyLock::new(Default::default);

pub fn register(root: &Path, remote: &Remote) {
    let mut r = REMOTES.lock().unwrap_or_else(|e| e.into_inner());
    r.retain(|(p, _)| p != root);
    r.push((root.to_path_buf(), remote.clone()));
}

/// The server root `p` is in (or is), and its panel, when it's remote.
pub fn remote_of(p: &Path) -> Option<(PathBuf, Remote)> {
    if !is_remote_root(p) {
        return None;
    }
    REMOTES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(root, _)| p.starts_with(root))
        .cloned()
}

/// `p` inside a remote server: the client, the server's id, and the path
/// relative to its folder.
pub fn locate(p: &Path) -> Option<anyhow::Result<(Client, String, String)>> {
    let (root, r) = remote_of(p)?;
    let rel = remote_rel(&root, p).unwrap_or_default();
    Some(Client::for_remote(&r).map(|c| (c, r.id.clone(), rel)))
}

/// Where the panel's web UI shows `p` (a file opens in its editor, a
/// folder in its file manager), when `p` is on a remote server.
pub fn web_url(p: &Path) -> Option<String> {
    let (root, r) = remote_of(p)?;
    let rel = remote_rel(&root, p).unwrap_or_default();
    let segs: Vec<String> = rel.split('/').filter(|s| !s.is_empty()).map(enc).collect();
    let file = segs.last().is_some_and(|l| l.contains('.'));
    let tab = if file { "edit" } else { "files" };
    let mut url = format!("{}/#/server/{}/{tab}", r.base, enc(&r.id));
    for s in segs {
        url.push('/');
        url.push_str(&s);
    }
    Some(url)
}

/// The panel's Settings > Mod sources page (where "mods from links" is
/// turned on), for the panel a remote server is on.
pub fn mod_sources_url(p: &Path) -> Option<String> {
    let (_, r) = remote_of(p)?;
    Some(format!("{}/#/settings/content", r.base))
}

/// The stand-in root for a server on a Hopper Server. The first panel's
/// servers keep `hopperd:\<folder>` (pairings are saved by it); others get
/// their panel's id in between (see [`panel_root`]).
pub fn remote_root(folder: &str) -> PathBuf {
    PathBuf::from(format!("hopperd:\\{folder}"))
}

/// [`remote_root`] for a server on panel `panel`.
pub fn panel_root(panel: &str, folder: &str) -> PathBuf {
    if panel.is_empty() { remote_root(folder) } else { PathBuf::from(format!("hopperd:\\@{panel}\\{folder}")) }
}

/// "rig" from "https://rig:8790", for a panel's default name.
pub fn host_name(base: &str) -> String {
    let host = base.split("://").nth(1).unwrap_or(base).split('/').next().unwrap_or("");
    let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
    host.trim_matches(['[', ']']).to_string()
}

/// Whether the panel's `/auth/me` answer says this PC may delete or remove
/// servers there: the owner, a manager, or a paired PC the owner let manage
/// the panel. An answer without a role (an old panel) counts as yes; the
/// panel still checks.
pub fn may_manage(me: &Value) -> bool {
    match me["role"].as_str() {
        Some("owner" | "manager") | None => true,
        Some("device") => me["manage"].as_bool().unwrap_or(false),
        Some(_) => false,
    }
}

/// What a plain paired PC is told when it can't delete or remove servers
/// (`verb`: "delete" or "remove"). The panel says the same when asked.
pub fn cant_manage_text(host: &str, verb: &str) -> String {
    format!(
        "This PC can't {verb} servers on {host}. In {host}'s panel, open Settings › Access & devices and allow this PC to manage it, or {verb} the server there."
    )
}

pub fn is_remote_root(p: &Path) -> bool {
    p.to_string_lossy().starts_with("hopperd:")
}

/// `https://host:8790` from what was typed ("rig", "rig:8790", a URL).
/// An old `http://` address is moved to HTTPS.
pub fn base_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let url = match url.split_once("://") {
        Some(("http", rest)) | Some(("https", rest)) => format!("https://{rest}"),
        Some(_) => url.to_string(),
        None => format!("https://{url}"),
    };
    let after = url.split("://").nth(1).unwrap_or("");
    let host = after.split('/').next().unwrap_or("");
    // No port given: hopperd's.
    let has_port = host.rsplit_once(':').is_some_and(|(_, p)| p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty());
    if has_port || host.is_empty() { url } else { format!("{url}:{PORT}") }
}

/// The addresses to try, in order, for what was typed. A web address with
/// no port (`https://hopper.example.com`) is most likely a panel behind a
/// reverse proxy on 443, so that comes first, then hopperd's own port.
/// Everything else has one answer: [`base_url`]. An `http://` address with
/// no port stays on hopperd's port, as before: the panel only speaks HTTPS,
/// and a proxy's port 80 just redirects. Each address names its port, so
/// what pairing saves never needs probing again.
pub fn candidates(url: &str) -> Vec<String> {
    let base = base_url(url);
    let t = url.trim();
    let Some(rest) = t.strip_prefix("https://") else { return vec![base] };
    let host = rest.split('/').next().unwrap_or("");
    let has_port = host.rsplit_once(':').is_some_and(|(_, p)| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    if host.is_empty() || has_port {
        return vec![base];
    }
    vec![format!("https://{host}:443"), base]
}

/// Whether a saved panel address and a typed one are the same panel.
pub fn same_panel(saved: &str, typed: &str) -> bool {
    let saved = base_url(saved);
    candidates(typed).iter().any(|c| *c == saved)
}

/// The first of [`candidates`] that `check` accepts, with what it gave.
/// `check` gets how long to wait for the connection to open: short on the
/// first of several tries ([`PROBE_WAIT`]), so a wrong guess doesn't hold
/// pairing up; the last (or only) one gets the normal [`CONNECT_WAIT`]. When
/// all fail, the error names every address.
fn first_reachable<T>(url: &str, check: impl Fn(&str, Duration) -> anyhow::Result<T>) -> anyhow::Result<(String, T)> {
    let tries = candidates(url);
    if tries.len() == 1 {
        let base = tries.into_iter().next().unwrap_or_default();
        return check(&base, CONNECT_WAIT).map(|v| (base, v));
    }
    let mut errors = Vec::new();
    for (i, base) in tries.iter().enumerate() {
        let wait = if i + 1 < tries.len() { PROBE_WAIT } else { CONNECT_WAIT };
        match check(base, wait) {
            Ok(v) => return Ok((base.clone(), v)),
            Err(e) => errors.push(format!("{base}: {e:#}")),
        }
    }
    bail!("No Hopper Server found at {}. Tried {}", tries.join(" or "), errors.join("; "))
}

/// Whether `base`'s host is a LAN, Tailscale or local name: the panel's
/// own certificate covers those. A public domain has a Let's Encrypt one.
fn private_host(base: &str) -> bool {
    let host = base.split("://").nth(1).unwrap_or("").split('/').next().unwrap_or("");
    let host = host.rsplit_once(':').map_or(host, |(h, p)| if p.chars().all(|c| c.is_ascii_digit()) { h } else { host });
    let host = host.trim_matches(['[', ']']);
    host.parse::<std::net::IpAddr>().is_ok() || !host.contains('.') || host.ends_with(".local") || host.ends_with(".ts.net")
}

/// How to trust the panel's certificate.
#[derive(Clone)]
enum Trust {
    /// Its own CA (PEM), pinned at pairing.
    Pinned(String),
    /// The normal web roots (Let's Encrypt on a public domain).
    Web,
    /// Anything: only to fetch the CA before pairing.
    Unverified,
}

fn trust_for(base: &str, ca: &str) -> Trust {
    if !ca.trim().is_empty() && private_host(base) { Trust::Pinned(ca.to_string()) } else { Trust::Web }
}

/// How long to wait for a connection to open (name lookup, then TCP and
/// the TLS handshake), for the reply to start, and for a whole request. A
/// panel on the other side of the world, behind a proxy, can take a few
/// seconds just to open a connection; each step gets its own limit so an
/// error can say which step it was.
const CONNECT_WAIT: Duration = Duration::from_secs(10);
const REPLY_WAIT: Duration = Duration::from_secs(20);
const CALL_WAIT: Duration = Duration::from_secs(30);
/// How long the first of several guessed addresses may take to connect,
/// so a wrong guess doesn't hold pairing up.
const PROBE_WAIT: Duration = Duration::from_secs(5);
/// Pauses before opening a connection again after one didn't open.
const RETRY_PAUSES: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(3)];

/// An agent for normal calls to `base`.
fn agent_with(base: &str, trust: &Trust) -> ureq::Agent {
    agent_timed(base, CONNECT_WAIT, Some(REPLY_WAIT), CALL_WAIT, trust)
}

/// `connect`: how long the name lookup and opening the connection may each
/// take. `reply`: how long to wait for the reply to start. `total`: the
/// whole request.
fn agent_timed(base: &str, connect: Duration, reply: Option<Duration>, total: Duration, trust: &Trust) -> ureq::Agent {
    agent_limits(base, connect, None, reply, total, trust)
}

/// An agent for upload chunks (see [`CHUNK_SEND`]).
fn agent_chunk(base: &str, trust: &Trust) -> ureq::Agent {
    agent_limits(base, CONNECT_WAIT, Some(CHUNK_SEND), Some(CHUNK_REPLY), CHUNK_WAIT, trust)
}

/// [`agent_timed`], with `send` limiting how long the request body may
/// take to go out.
fn agent_limits(base: &str, connect: Duration, send: Option<Duration>, reply: Option<Duration>, total: Duration, trust: &Trust) -> ureq::Agent {
    use ureq::tls::{Certificate, RootCerts, TlsConfig};
    let tls = match trust {
        Trust::Pinned(pem) => match Certificate::from_pem(pem.as_bytes()) {
            Ok(c) => TlsConfig::builder().root_certs(RootCerts::Specific(std::sync::Arc::new(vec![c]))).build(),
            // A broken pin trusts nothing rather than everything.
            Err(_) => TlsConfig::builder().root_certs(RootCerts::Specific(std::sync::Arc::new(Vec::new()))).build(),
        },
        Trust::Web => TlsConfig::builder().root_certs(RootCerts::WebPki).build(),
        Trust::Unverified => TlsConfig::builder().disable_verification(true).build(),
    };
    // A public address goes through the PC's proxy when it has one, as the
    // browser does; a LAN or Tailscale panel is always reached directly.
    let proxy = if private_host(base) { None } else { ureq::Proxy::try_from_env() };
    ureq::Agent::config_builder()
        .timeout_global(Some(total))
        .timeout_resolve(Some(connect))
        .timeout_connect(Some(connect))
        .timeout_send_body(send)
        .timeout_recv_response(reply)
        .user_agent(concat!("Hopper/", env!("CARGO_PKG_VERSION")))
        .proxy(proxy)
        .http_status_as_error(false)
        .tls_config(tls)
        .build()
        .into()
}

/// Which step of a request failed.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    /// Looking up the address.
    Lookup,
    /// Opening the connection: TCP, then the TLS handshake.
    Connect,
    /// The certificate or the TLS handshake was refused.
    Tls,
    /// Sending the request.
    Send,
    /// Waiting for or reading the reply.
    Reply,
    /// Something else (the whole request ran out of time, a broken reply).
    Other,
}

impl Step {
    fn of(e: &ureq::Error) -> Step {
        use std::io::ErrorKind as K;
        use ureq::Timeout as T;
        match e {
            ureq::Error::HostNotFound | ureq::Error::Timeout(T::Resolve) => Step::Lookup,
            // Windows' "No such host is known" and friends (WSAHOST_NOT_FOUND
            // to WSANO_DATA); elsewhere getaddrinfo's own message.
            ureq::Error::Io(io)
                if matches!(io.raw_os_error(), Some(11001..=11004))
                    || io.to_string().contains("failed to lookup address") =>
            {
                Step::Lookup
            }
            ureq::Error::Timeout(T::Connect) | ureq::Error::ConnectionFailed | ureq::Error::ConnectProxyFailed(_) => {
                Step::Connect
            }
            ureq::Error::Io(io)
                if matches!(
                    io.kind(),
                    K::ConnectionRefused | K::HostUnreachable | K::NetworkUnreachable | K::AddrNotAvailable
                ) =>
            {
                Step::Connect
            }
            ureq::Error::Tls(_) | ureq::Error::Rustls(_) => Step::Tls,
            ureq::Error::Timeout(T::SendRequest | T::SendBody) => Step::Send,
            ureq::Error::Timeout(T::RecvResponse | T::RecvBody) | ureq::Error::BodyStalled => Step::Reply,
            _ => Step::Other,
        }
    }

    /// Whether nothing was sent yet, so trying again can't run a request
    /// twice, and a second try may get through.
    fn retry(self) -> bool {
        matches!(self, Step::Lookup | Step::Connect)
    }

    fn says(self) -> &'static str {
        match self {
            Step::Lookup => "looking up the address failed",
            Step::Connect => "the connection didn't open",
            Step::Tls => "the secure connection was refused",
            Step::Send => "sending the request stalled",
            Step::Reply => "no reply came",
            Step::Other => "the request failed",
        }
    }
}

/// Sends a request made by `f`, opening the connection again after a short
/// pause when it couldn't be opened: a far-away panel on a lossy link
/// sometimes loses a new connection that the next try gets through. Only
/// failures before anything was sent are tried again, so a POST never runs
/// twice. The error says which step failed and how long it took, and for a
/// connection that didn't open, how long the lookup and TCP took on their
/// own, so a report shows whether DNS, TCP or the TLS handshake stalled.
fn send(
    base: &str,
    retry: bool,
    f: impl FnMut() -> Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> anyhow::Result<ureq::http::Response<ureq::Body>> {
    send_with(base, retry, f, std::thread::sleep, probe)
}

/// [`send`] with the pause and the connection check passed in.
fn send_with<R>(
    base: &str,
    retry: bool,
    mut f: impl FnMut() -> Result<R, ureq::Error>,
    mut sleep: impl FnMut(Duration),
    check: impl Fn(&str) -> String,
) -> anyhow::Result<R> {
    let first = Instant::now();
    let mut tries = 0;
    loop {
        let start = Instant::now();
        let e = match f() {
            Ok(r) => return Ok(r),
            Err(e) => e,
        };
        let took = start.elapsed();
        let step = Step::of(&e);
        // A try that ran out of time is tried once more, not twice, so a
        // panel that's switched off doesn't keep the answer waiting long.
        let most = if took >= CONNECT_WAIT / 2 { 1 } else { RETRY_PAUSES.len() };
        if retry && step.retry() && tries < most {
            sleep(RETRY_PAUSES[tries]);
            tries += 1;
            continue;
        }
        let mut msg = format!("{} after {:.1} s ({e})", step.says(), took.as_secs_f32());
        if tries > 0 {
            msg.push_str(&format!(", tried {} times in {:.0} s", tries + 1, first.elapsed().as_secs_f32()));
        }
        if step == Step::Connect {
            msg.push_str(&format!("; {}", check(base)));
        }
        return Err(anyhow::anyhow!(msg).context(format!("can't reach Hopper Server at {base}")));
    }
}

/// Looks up `base`'s host and opens a plain TCP connection to it, timing
/// each, to tell apart a slow lookup, a blocked port and a TLS handshake
/// that stalls after TCP opened. Skipped behind a proxy (it would test
/// another path than the one Hopper uses).
fn probe(base: &str) -> String {
    use std::net::ToSocketAddrs as _;
    if !private_host(base) && ureq::Proxy::try_from_env().is_some() {
        return "sent through the PC's proxy".into();
    }
    let after = base.split("://").nth(1).unwrap_or(base);
    let authority = after.split('/').next().unwrap_or("").to_string();
    let start = Instant::now();
    let (tx, rx) = std::sync::mpsc::channel();
    let lookup = authority.clone();
    std::thread::spawn(move || {
        let _ = tx.send(lookup.to_socket_addrs().map(|a| a.collect::<Vec<_>>()));
    });
    let addrs = match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(a)) if !a.is_empty() => a,
        Ok(Ok(_)) | Ok(Err(_)) => return format!("checked again: the address lookup found nothing ({:.1} s)", start.elapsed().as_secs_f32()),
        Err(_) => return "checked again: the address lookup took over 5 s".into(),
    };
    let looked = start.elapsed();
    let ips = addrs.iter().map(|a| a.ip().to_string()).collect::<Vec<_>>().join(", ");
    let start = Instant::now();
    for a in &addrs {
        if std::net::TcpStream::connect_timeout(a, Duration::from_secs(5)).is_ok() {
            return format!(
                "checked again: lookup {:.1} s ({ips}), TCP {:.1} s, so the TLS handshake is what stalled",
                looked.as_secs_f32(),
                start.elapsed().as_secs_f32()
            );
        }
    }
    format!(
        "checked again: lookup {:.1} s ({ips}), TCP didn't open in {:.1} s",
        looked.as_secs_f32(),
        start.elapsed().as_secs_f32()
    )
}

/// "AB:CD:…" of a PEM certificate, worked out here (never taken from what
/// the panel says about itself).
pub fn fingerprint_of(pem: &str) -> Option<String> {
    use base64::Engine as _;
    let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let der = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let hex = crate::javadl::sha256_hex(&der);
    Some(hex.as_bytes().chunks(2).map(|c| String::from_utf8_lossy(c).to_uppercase()).collect::<Vec<_>>().join(":"))
}

/// A paired Hopper Server.
pub struct Client {
    pub base: String,
    /// Which paired panel ([`crate::settings::HopperPanel::id`]).
    pub panel: String,
    token: String,
    ca: String,
    agent: ureq::Agent,
    /// For file transfers, which take as long as they take.
    slow: ureq::Agent,
    /// For upload chunks, each with its own limits.
    chunk: ureq::Agent,
    /// With a pinned CA: the same agents checking with the web roots,
    /// for when the panel answers with a Let's Encrypt certificate (its
    /// automatic certificate, served to the internet by IP or name).
    web: Option<std::sync::OnceLock<(ureq::Agent, ureq::Agent, ureq::Agent)>>,
    /// The last answer came with the web roots, so start with them.
    on_web: std::sync::atomic::AtomicBool,
    /// What the panel says it can do (its hello), asked once.
    caps: std::sync::OnceLock<Vec<String>>,
}

/// An answer from `GET /changes` ([`Client::changes`]).
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
pub struct PanelChanges {
    /// The panel's run: a new one means start over.
    pub boot: String,
    /// The place to ask from next time.
    pub rev: u64,
    /// Changes were missed: reload everything shown.
    pub reset: bool,
    pub changes: Vec<PanelChange>,
}

/// One change: what about which server (`None`: the panel, or its list).
#[derive(Debug, Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct PanelChange {
    pub server: Option<String>,
    /// `list`, `server`, `mods`, `configs`, `files`, `backups`, `schedule`,
    /// `players`, `worlds`, `panel`, or `*` for anything.
    pub areas: Vec<String>,
}

impl PanelChange {
    pub fn touches(&self, area: &str) -> bool {
        self.areas.iter().any(|a| a == area || a == "*")
    }
}

/// The reply's JSON, or its `error` as the error.
fn reply(resp: anyhow::Result<ureq::http::Response<ureq::Body>>) -> anyhow::Result<Value> {
    let mut resp = resp?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    match status {
        200..=299 => Ok(v),
        401 => bail!("Hopper Server doesn't know this PC any more. Pair again in Settings → Server panel."),
        _ => bail!("{}", v["error"].as_str().map(str::to_string).unwrap_or_else(|| format!("Hopper Server answered {status}"))),
    }
}

/// Which of a client's agents a request goes through.
#[derive(Clone, Copy)]
enum Lane {
    /// A normal call, answered within seconds.
    Call,
    /// A whole file going one way, as long as it takes.
    Slow,
    /// One upload chunk.
    Chunk,
}

impl Client {
    /// `ca`: the panel's certificate authority, pinned at pairing.
    pub fn new(url: &str, token: &str, ca: &str) -> Self {
        let base = base_url(url);
        let trust = trust_for(&base, ca);
        Client {
            agent: agent_with(&base, &trust),
            // No limit on the reply: a download takes as long as it takes.
            slow: agent_timed(&base, CONNECT_WAIT, None, Duration::from_secs(3600), &trust),
            chunk: agent_chunk(&base, &trust),
            web: matches!(trust, Trust::Pinned(_)).then(std::sync::OnceLock::new),
            on_web: Default::default(),
            caps: Default::default(),
            base,
            panel: String::new(),
            token: token.to_string(),
            ca: ca.to_string(),
        }
    }

    /// A paired panel, when its token is saved.
    pub fn for_panel(p: &crate::settings::HopperPanel) -> Option<Client> {
        let url = if p.id.is_empty() {
            std::env::var("HOPPER_PANEL_URL").ok().filter(|u| !u.trim().is_empty()).unwrap_or_else(|| p.url.clone())
        } else {
            p.url.clone()
        };
        if url.trim().is_empty() {
            return None;
        }
        let mut c = Client::new(&url, &crate::panel::hopperd_token_for(&p.id)?, &p.ca);
        c.panel = p.id.clone();
        Some(c)
    }

    /// The token, to make a [`crate::panel::HopperServer`] from this client.
    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn ca(&self) -> &str {
        &self.ca
    }

    /// The first paired panel.
    pub fn from_settings(s: &crate::settings::Settings) -> Option<Client> {
        s.hopper_panels.first().and_then(Client::for_panel)
    }

    /// For a server on this panel.
    pub fn for_remote(r: &Remote) -> anyhow::Result<Client> {
        let token =
            crate::panel::hopperd_token_for(&r.panel).context("Pair with the Hopper Server first (Settings → Servers).")?;
        let mut c = Client::new(&r.base, &token, &r.ca);
        c.panel = r.panel.clone();
        Ok(c)
    }

    fn url(&self, path: &str) -> String {
        // Paths are relative to /api/v1; passing the prefix too 404s.
        debug_assert!(!path.starts_with("/api/"), "hopperd path {path} already has the /api/v1 prefix");
        format!("{}/api/v1{path}", self.base)
    }

    fn auth(&self) -> String {
        format!("Bearer {}", self.token)
    }

    /// [`send`] with this panel's agent (`slow` for file transfers). A
    /// pinned panel may answer with its Let's Encrypt certificate instead
    /// of its own (from the internet, by its public IP or name), or the
    /// other way round when that's switched off: a certificate the one
    /// check refuses is tried with the other, which is kept for next time.
    fn send_via(
        &self,
        lane: Lane,
        f: impl Fn(&ureq::Agent) -> Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> anyhow::Result<ureq::http::Response<ureq::Body>> {
        use std::sync::atomic::Ordering::Relaxed;
        let pick = |web: bool| -> &ureq::Agent {
            let (a, s, c) = match (&self.web, web) {
                (Some(w), true) => {
                    let (a, s, c) = w.get_or_init(|| {
                        (
                            agent_with(&self.base, &Trust::Web),
                            agent_timed(&self.base, CONNECT_WAIT, None, Duration::from_secs(3600), &Trust::Web),
                            agent_chunk(&self.base, &Trust::Web),
                        )
                    });
                    (a, s, c)
                }
                _ => (&self.agent, &self.slow, &self.chunk),
            };
            match lane {
                Lane::Call => a,
                Lane::Slow => s,
                Lane::Chunk => c,
            }
        };
        let web = self.on_web.load(Relaxed);
        match send(&self.base, true, || f(pick(web))) {
            Err(e) if self.web.is_some() && cert_rejected(&e) => match send(&self.base, true, || f(pick(!web))) {
                Ok(r) => {
                    self.on_web.store(!web, Relaxed);
                    Ok(r)
                }
                Err(_) => Err(e),
            },
            r => r,
        }
    }

    pub fn get(&self, path: &str) -> anyhow::Result<Value> {
        reply(self.send_via(Lane::Call, |a| a.get(&self.url(path)).header("Authorization", &self.auth()).call()))
    }

    pub fn post(&self, path: &str, body: Value) -> anyhow::Result<Value> {
        reply(self.send_via(Lane::Call, |a| a.post(&self.url(path)).header("Authorization", &self.auth()).send_json(&body)))
    }

    pub fn put(&self, path: &str, body: Value) -> anyhow::Result<Value> {
        reply(self.send_via(Lane::Call, |a| a.put(&self.url(path)).header("Authorization", &self.auth()).send_json(&body)))
    }

    pub fn delete(&self, path: &str) -> anyhow::Result<Value> {
        reply(self.send_via(Lane::Call, |a| a.delete(&self.url(path)).header("Authorization", &self.auth()).call()))
    }

    /// Unpairs this PC in the panel, so it leaves Access & devices and the
    /// token stops working there. Panels before this route answer 403.
    pub fn unpair(&self) -> anyhow::Result<()> {
        self.delete("/devices/me").map(|_| ())
    }

    /// Starts a job with a POST and follows it to the end (see [`Client::follow`]).
    pub fn run_job(&self, path: &str, body: Value, job: &crate::jobs::Job) -> anyhow::Result<Value> {
        let v = self.post(path, body)?;
        let id = v["job"].as_u64().context("the panel didn't start a job")?;
        self.follow(id, job)
    }

    /// `/servers/{id}` + `rest`, with the id encoded.
    pub fn server_path(id: &str, rest: &str) -> String {
        format!("/servers/{}{rest}", enc(id))
    }

    /// `/fs/{id}` + `rest`, with the id encoded.
    pub fn fs_path(id: &str, rest: &str) -> String {
        format!("/fs/{}{rest}", enc(id))
    }

    // ---- servers -------------------------------------------------------------

    /// Every server on the panel, as Hopper's [`ServerInfo`].
    pub fn server_infos(&self) -> anyhow::Result<Vec<ServerInfo>> {
        let list = self.get("/servers/infos")?;
        Ok(list
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| info_from(v, &self.base, &self.ca, &self.panel))
            .collect())
    }

    /// What changed on the panel after `at` (its run and place, from the
    /// last answer), waiting up to `wait` seconds for something. Without
    /// `at`: where to start from. See hopperd's `changes.rs`.
    pub fn changes(&self, at: Option<(&str, u64)>, wait: u64) -> anyhow::Result<PanelChanges> {
        let q = match at {
            Some((boot, since)) => format!("/changes?boot={}&since={since}&wait={wait}", enc(boot)),
            None => "/changes".to_string(),
        };
        Ok(serde_json::from_value(self.get(&q)?)?)
    }

    /// A server's mods, read by the panel itself (no jar crosses the wire).
    pub fn mods(&self, id: &str) -> anyhow::Result<Vec<ModInfo>> {
        let v = self.get(&format!("/servers/{}/mods", enc(id)))?;
        Ok(serde_json::from_value(v["mods"].clone())?)
    }

    /// [`Client::mods`] with the jars turned off (`off` set).
    pub fn mods_with_off(&self, id: &str) -> anyhow::Result<Vec<ModInfo>> {
        let v = self.get(&format!("/servers/{}/mods?off=1", enc(id)))?;
        Ok(serde_json::from_value(v["mods"].clone())?)
    }

    /// The server's last crash, read by the panel from its logs.
    pub fn crash(&self, id: &str) -> anyhow::Result<Option<crate::crashdetect::Crash>> {
        let v = self.get(&format!("/servers/{}/crash", enc(id)))?["crash"].clone();
        // Hopper Server before 0.14 sends its old scan: say it crashed, with
        // its headline, but no mods (that scan's guesses were often wrong).
        if v.get("hits").is_some() {
            return Ok(Some(crate::crashdetect::Crash {
                headline: v["headline"].as_str().unwrap_or_default().to_string(),
                server: true,
                ..Default::default()
            }));
        }
        Ok(serde_json::from_value(v)?)
    }

    /// Puts back the older copy of the mod the server's last crash names
    /// (`file`: the jar there now), as [`crate::crashdetect::put_back`].
    /// The server's crash reports, newest first: (path in the server
    /// folder, modified unix seconds, Hopper's one-line verdict when it read
    /// that one).
    pub fn crashes(&self, id: &str) -> anyhow::Result<Vec<(String, u64, Option<String>, bool)>> {
        let v = self.get(&format!("/servers/{}/crashes", enc(id)))?;
        Ok(v["crashes"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|c| {
                        let verdict = c["verdict"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
                        (c["path"].as_str().unwrap_or_default().to_string(), c["modified"].as_u64().unwrap_or(0), verdict, c["follow"].as_bool().unwrap_or(false))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// The server's older logs: (path in the server folder, size, modified
    /// unix seconds), newest first, without the current latest.log / debug.log.
    pub fn old_logs(&self, id: &str) -> anyhow::Result<Vec<(String, u64, i64)>> {
        let v = self.get(&format!("/servers/{}/logs", enc(id)))?;
        let mut out: Vec<(String, u64, i64)> = v["logs"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|f| !f["crash"].as_bool().unwrap_or(false))
                    .map(|f| (f["path"].as_str().unwrap_or_default().to_string(), f["size"].as_u64().unwrap_or(0), f["modified"].as_i64().unwrap_or(0)))
                    .filter(|(p, _, _)| p != "logs/latest.log" && p != "logs/debug.log")
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by(|a, b| b.2.cmp(&a.2).then(b.0.cmp(&a.0)));
        Ok(out)
    }

    /// Deletes crash reports / logs (paths under the server folder) to the
    /// panel's trash. Returns how many went.
    pub fn delete_logs(&self, id: &str, paths: &[String]) -> anyhow::Result<usize> {
        let v = self.post(&format!("/servers/{}/logs/delete", enc(id)), serde_json::json!({ "paths": paths }))?;
        Ok(v["deleted"].as_u64().unwrap_or(0) as usize)
    }

    /// "Clear all" for `what` ("crashes" or "logs").
    pub fn clear_logs(&self, id: &str, what: &str) -> anyhow::Result<usize> {
        let v = self.post(&format!("/servers/{}/logs/clear", enc(id)), serde_json::json!({ "what": what }))?;
        Ok(v["deleted"].as_u64().unwrap_or(0) as usize)
    }

    /// One crash report read like the crash card reads the last one, with
    /// each named jar's name for people.
    pub fn crash_read(&self, id: &str, path: &str) -> anyhow::Result<(Option<crate::crashdetect::Crash>, std::collections::HashMap<String, String>)> {
        let v = self.get(&format!("/servers/{}/crashes/read?path={}", enc(id), enc(path)))?;
        let c = serde_json::from_value(v["crash"].clone())?;
        let names = serde_json::from_value(v["names"].clone()).unwrap_or_default();
        Ok((c, names))
    }

    /// A log's text: its end (`from` 0), or what was written since `from`.
    /// (text, the offset to read from next, whether it started over).
    pub fn log_text(&self, id: &str, path: &str, from: u64) -> anyhow::Result<(String, u64, bool)> {
        let v = self.get(&format!("/servers/{}/logs/text?path={}&from={from}", enc(id), enc(path)))?;
        Ok((v["text"].as_str().unwrap_or_default().to_string(), v["to"].as_u64().unwrap_or(0), v["reset"].as_bool().unwrap_or(true)))
    }

    pub fn crash_put_back(&self, id: &str, file: &str) -> anyhow::Result<()> {
        self.post(&format!("/servers/{}/crash/put-back", enc(id)), serde_json::json!({ "file": file }))?;
        Ok(())
    }

    /// The NeoForge version the server's run.bat starts.
    pub fn neoforge(&self, id: &str) -> anyhow::Result<Option<String>> {
        Ok(self.get(&format!("/servers/{}/neoforge", enc(id)))?["version"].as_str().map(str::to_string))
    }

    /// Every file under `folder` (relative to the server folder) as (path
    /// inside the folder, size, modified time). Empty when it's missing.
    pub fn walk(&self, id: &str, folder: &str) -> anyhow::Result<Vec<(String, u64, u64)>> {
        let v = self.get(&format!("/fs/{}/walk?path={}", enc(id), enc(folder)))?;
        Ok(serde_json::from_value(v["files"].clone())?)
    }

    /// Content hashes of files (paths relative to the server folder).
    pub fn hashes(&self, id: &str, rels: &[String]) -> anyhow::Result<std::collections::HashMap<String, String>> {
        let v = self.post(&format!("/fs/{}/hashes", enc(id)), json!({ "paths": rels }))?;
        Ok(serde_json::from_value(v["hashes"].clone())?)
    }

    /// A text file's content.
    pub fn read_text(&self, id: &str, rel: &str) -> anyhow::Result<String> {
        let v = self.get(&format!("/fs/{}/text?path={}", enc(id), enc(rel)))?;
        Ok(v["content"].as_str().unwrap_or("").to_string())
    }

    /// Which of `names` are folders in the server folder.
    pub fn dirs(&self, id: &str, names: &[&str]) -> anyhow::Result<Vec<String>> {
        let v = self.get(&format!("/fs/{}/dirs?names={}", enc(id), enc(&names.join(","))))?;
        Ok(serde_json::from_value(v["dirs"].clone())?)
    }

    // ---- configs -------------------------------------------------------------------

    /// The server's config files with their mods, sides and changed counts
    /// (the panel's Configs page reads the same).
    pub fn configs_index(&self, id: &str) -> anyhow::Result<Value> {
        self.get(&Self::server_path(id, "/configs/index"))
    }

    /// Settings in the server's config files whose name or key (and with
    /// `desc` their description) hold every word of `q`: `{ hits: [{ rel,
    /// owner, entry }] }`, as the panel's Configs page searches.
    pub fn configs_search(&self, id: &str, q: &str, desc: bool, limit: usize) -> anyhow::Result<Value> {
        self.get(&format!("{}?q={}&desc={}&limit={limit}", Self::server_path(id, "/configs/search"), enc(q), if desc { 1 } else { 0 }))
    }

    /// Config files' text, read by the panel: per file the text, or why it
    /// couldn't be read as settings.
    pub fn configs_parse(&self, id: &str, rels: &[String]) -> anyhow::Result<std::collections::HashMap<String, Result<String, String>>> {
        let v = self.post(&Self::server_path(id, "/configs/parse"), json!({ "rels": rels }))?;
        let mut out = std::collections::HashMap::new();
        for (rel, f) in v["files"].as_object().into_iter().flatten() {
            let r = if f["ok"].as_bool() == Some(true) {
                Ok(f["text"].as_str().unwrap_or("").to_string())
            } else {
                Err(f["error"].as_str().unwrap_or("The panel couldn't read it.").to_string())
            };
            out.insert(rel.clone(), r);
        }
        Ok(out)
    }

    /// Saves settings in one of the server's config files, checked together
    /// on the panel; `base` is the text they were made on (when the file
    /// changed since, the panel puts them onto the newer copy). The old copy
    /// goes to the server's `.hopper-backups`.
    pub fn configs_batch(&self, id: &str, rel: &str, base: &str, edits: &[crate::configedit::Edit]) -> anyhow::Result<crate::configedit::Saved> {
        let edits: Vec<Value> = edits.iter().map(|e| json!({ "path": e.path, "key": e.key, "value": e.value.text() })).collect();
        let v = self.post(&Self::server_path(id, "/configs/batch"), json!({ "rel": rel, "base": base, "edits": edits }))?;
        if v["ok"].as_bool() != Some(true) {
            let errs: Vec<String> = v["errors"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|e| {
                    let (k, m) = (e["key"].as_str().unwrap_or(""), e["error"].as_str().unwrap_or(""));
                    if k.is_empty() { m.to_string() } else { format!("{k}: {m}") }
                })
                .collect();
            anyhow::bail!("{}", if errs.is_empty() { "The panel didn't save it.".to_string() } else { errs.join(" ") });
        }
        Ok(crate::configedit::Saved { text: v["text"].as_str().unwrap_or("").to_string(), reapplied: v["reapplied"].as_bool().unwrap_or(false) })
    }

    /// Sends this PC's copy of the settings files both sides read (paths as
    /// here, `config/x-common.toml` -> text) so the panel can show players'
    /// copies beside the server's. The latest replaces the one before.
    pub fn configs_snapshot(&self, id: &str, files: &BTreeMap<String, String>) -> anyhow::Result<()> {
        self.put(&Self::server_path(id, "/configs/client-snapshot"), json!({ "files": files }))?;
        Ok(())
    }

    /// The settings the server's owner pushed to this Hopper and it hasn't
    /// applied or dismissed yet.
    pub fn configs_pushes(&self, id: &str) -> anyhow::Result<Vec<ConfigPush>> {
        let v = self.get(&Self::server_path(id, "/configs/pushes"))?;
        Ok(v["pushes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["state"].as_str().is_none_or(|s| s == "pending"))
            .map(|p| ConfigPush {
                id: p["id"].as_str().unwrap_or("").to_string(),
                rel: p["rel"].as_str().unwrap_or("").to_string(),
                edits: p["edits"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|e| crate::configedit::Edit {
                        path: serde_json::from_value(e["path"].clone()).unwrap_or_default(),
                        key: e["key"].as_str().unwrap_or("").to_string(),
                        value: crate::configedit::Value::Str(e["value"].as_str().unwrap_or("").to_string()),
                    })
                    .collect(),
                from: p["from"].as_str().unwrap_or("").to_string(),
                at: p["at"].as_u64().unwrap_or(0),
            })
            .filter(|p| !p.id.is_empty() && !p.edits.is_empty())
            .collect())
    }

    /// Tells the panel a push was applied or dismissed (`state`), with a
    /// note the owner sees ("Applied", "Create isn't installed here").
    pub fn configs_push_done(&self, id: &str, pid: &str, state: &str, note: &str) -> anyhow::Result<()> {
        self.post(&Self::server_path(id, &format!("/configs/pushes/{}", enc(pid))), json!({ "state": state, "note": note }))?;
        Ok(())
    }

    // ---- jobs on the panel ---------------------------------------------------------

    /// Follows a job the panel runs until it ends, showing its detail and
    /// progress in `job`; cancelling `job` cancels it there. Returns its result.
    pub fn follow(&self, remote_job: u64, job: &crate::jobs::Job) -> anyhow::Result<Value> {
        let mut cancelled = false;
        loop {
            std::thread::sleep(Duration::from_millis(400));
            if job.is_cancelled() && !cancelled {
                cancelled = true;
                let _ = self.post(&format!("/jobs/{remote_job}/cancel"), json!({}));
            }
            let v = self.get(&format!("/jobs/{remote_job}"))?;
            if let Some(d) = v["detail"].as_str().filter(|d| !d.is_empty()) {
                job.detail(d);
            }
            if let Some(p) = v["progress"].as_f64() {
                job.progress((p * 1000.0) as usize, 1000);
            }
            if v["done"].as_bool() == Some(true) {
                if let Some(e) = v["error"].as_str() {
                    bail!("{e}");
                }
                return Ok(v["result"].clone());
            }
        }
    }

    /// Plans a server pack update on the panel (it downloads the server
    /// pack and compares it with the server's files there).
    pub fn pack_plan(
        &self,
        id: &str,
        inst: &crate::instances::Instance,
        pack: &crate::cf::File,
        compare: bool,
        job: &crate::jobs::Job,
    ) -> anyhow::Result<crate::server_pack_update::ServerPlan> {
        let v = self.post(
            &format!("/servers/{}/pack/plan", enc(id)),
            json!({ "inst": inst, "pack": pack, "compare": compare, "cf_key": crate::cf::api_key().unwrap_or(""), "keep_days": keep_days() }),
        )?;
        let result = self.follow(v["job"].as_u64().context("no job id")?, job)?;
        Ok(serde_json::from_value(result)?)
    }

    /// Applies a server pack update on the panel.
    pub fn pack_apply(
        &self,
        id: &str,
        plan: &crate::server_pack_update::ServerPlan,
        take: &std::collections::HashSet<String>,
        job: &crate::jobs::Job,
    ) -> anyhow::Result<crate::server_pack_update::Report> {
        let v = self.post(
            &format!("/servers/{}/pack/apply", enc(id)),
            json!({ "plan": plan, "take": take, "cf_key": crate::cf::api_key().unwrap_or(""), "keep_days": keep_days() }),
        )?;
        let result = self.follow(v["job"].as_u64().context("no job id")?, job)?;
        Ok(serde_json::from_value(result)?)
    }

    // ---- files -----------------------------------------------------------------

    /// Uploads `local` to `rel` in the server folder, replacing what is
    /// there (the panel keeps the old file in its trash).
    pub fn upload(&self, id: &str, local: &Path, rel: &str) -> anyhow::Result<()> {
        self.upload_with(id, local, rel, &|_| {})
    }

    /// [`Client::upload`], telling `progress` the bytes the panel has so far.
    pub fn upload_with(&self, id: &str, local: &Path, rel: &str, progress: &dyn Fn(u64)) -> anyhow::Result<()> {
        let size = std::fs::metadata(local).with_context(|| format!("opening {}", local.display()))?.len();
        let link = ClientLink {
            client: self,
            uploads: format!("/fs/{}/uploads", enc(id)),
            start: json!({ "path": rel, "size": size, "overwrite": true }),
        };
        self.push(&link, local, size, CHUNK, progress).map(|_| ())
    }

    /// Adds jar `local` to the server's mods, as a jar dropped on its Mods
    /// tab: another version of a mod it has replaces that one (kept in the
    /// panel's backups). Answers whether one was replaced.
    pub fn upload_mod(&self, id: &str, local: &Path) -> anyhow::Result<bool> {
        self.upload_mod_with(id, local, &|_| {})
    }

    /// [`Client::upload_mod`], telling `progress` the bytes sent so far.
    /// The jar goes in resumable chunks (as [`Client::upload`]) to where
    /// the panel's mods route stages it, then one small call adds it; a
    /// panel from before that gets the whole jar in one request, with
    /// limits and a few tries. Either way it ends, with the panel's error
    /// when it failed.
    pub fn upload_mod_with(&self, id: &str, local: &Path, progress: &dyn Fn(u64)) -> anyhow::Result<bool> {
        let name = local.file_name().map(|n| n.to_string_lossy().into_owned()).context("no file name")?;
        let size = std::fs::metadata(local).with_context(|| format!("opening {}", local.display()))?.len();
        let add = format!("/servers/{}/mods/upload?name={}&conflict=replace", enc(id), enc(&name));
        let v = if self.can("mods.staged") {
            self.upload_with(id, local, &format!("mods/.{name}.upload"), progress)?;
            let url = self.url(&format!("{add}&staged=1"));
            with_tries(FINISH_TRIES, &mut std::thread::sleep, |n| {
                // A lost reply: the panel may have added it already.
                if n > 1 && self.mod_landed(id, &name) {
                    return Try::Done(json!({}));
                }
                Try::of(self.send_via(Lane::Chunk, |a| a.post(&url).header("Authorization", &self.auth()).send_empty()))
            })
        } else {
            let body = std::fs::read(local).with_context(|| format!("opening {}", local.display()))?;
            let url = self.url(&add);
            // 32 KB/s at least, and never under the chunk's own limits.
            let send = CHUNK_SEND.max(Duration::from_secs(size / (32 * 1024)));
            let v = with_tries(FINISH_TRIES, &mut std::thread::sleep, |_| {
                Try::of(self.send_via(Lane::Slow, |slow| {
                    slow.post(&url)
                        .config()
                        .timeout_send_body(Some(send))
                        .timeout_recv_response(Some(CHUNK_REPLY))
                        .timeout_global(Some(send + CHUNK_WAIT))
                        .build()
                        .header("Authorization", &self.auth())
                        .content_type("application/octet-stream")
                        .send(&body[..])
                }))
            });
            if v.is_ok() {
                progress(size);
            }
            v
        };
        let v = v.with_context(|| format!("adding {name}"))?;
        Ok(v["replaced"].as_bool().unwrap_or(false))
    }

    /// Whether the panel says it has `cap` (its hello's capabilities).
    fn can(&self, cap: &str) -> bool {
        self.caps
            .get_or_init(|| {
                self.get("/hello")
                    .ok()
                    .and_then(|v| serde_json::from_value::<Vec<String>>(v["capabilities"].clone()).ok())
                    .unwrap_or_default()
            })
            .iter()
            .any(|c| c == cap)
    }

    /// The jar `name` is in the server's mods and its staged copy is gone:
    /// a call that adds it went through.
    fn mod_landed(&self, id: &str, name: &str) -> bool {
        let Ok(v) = self.get(&format!("/fs/{}/list?path=mods", enc(id))) else { return false };
        let names: Vec<&str> = v["entries"].as_array().into_iter().flatten().filter_map(|e| e["name"].as_str()).collect();
        names.contains(&name) && !names.contains(&format!(".{name}.upload").as_str())
    }

    /// Uploads zip `local` to the panel's incoming folder, where its zip
    /// import (`/servers/import-zip`) finds it; returns its name there.
    /// `progress` gets the bytes sent so far.
    pub fn upload_incoming(&self, local: &Path, progress: impl Fn(u64)) -> anyhow::Result<String> {
        let size = std::fs::metadata(local).with_context(|| format!("opening {}", local.display()))?.len();
        let name = local.file_name().map(|n| n.to_string_lossy().into_owned()).context("no file name")?;
        let link = ClientLink { client: self, uploads: "/incoming/uploads".into(), start: json!({ "path": name, "size": size }) };
        // Older panels take at most 2 MB a chunk here.
        let start = self.push(&link, local, size, INCOMING_CHUNK, &progress)?;
        Ok(start["path"].as_str().filter(|p| !p.is_empty()).unwrap_or(&name).to_string())
    }

    /// Sends `local` through `link` in chunks of `chunk` bytes; the reply
    /// that started the upload.
    fn push(&self, link: &ClientLink, local: &Path, size: u64, chunk: usize, progress: &dyn Fn(u64)) -> anyhow::Result<Value> {
        let mut f = std::fs::File::open(local).with_context(|| format!("opening {}", local.display()))?;
        push_chunks(link, &mut f, size, chunk, progress, &mut std::thread::sleep)
            .with_context(|| format!("uploading {}", local.file_name().unwrap_or_default().to_string_lossy()))
    }

    /// Downloads `rel` from the server folder to `local`.
    pub fn download(&self, id: &str, rel: &str, local: &Path) -> anyhow::Result<()> {
        self.save_to(&format!("/fs/{}/file?path={}", enc(id), enc(rel)), local)
    }

    /// Saves what a GET of `path` answers (a file, a backup's zip) to
    /// `local`, through a temporary file so a cut-off transfer leaves nothing
    /// that looks whole.
    pub fn save_to(&self, path: &str, local: &Path) -> anyhow::Result<()> {
        let url = self.url(path);
        let mut resp = self.send_via(Lane::Slow, |slow| slow.get(&url).header("Authorization", &self.auth()).call())?;
        if !resp.status().is_success() {
            let text = resp.body_mut().read_to_string().unwrap_or_default();
            let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            bail!("{}", v["error"].as_str().unwrap_or("download failed"));
        }
        if let Some(p) = local.parent() {
            std::fs::create_dir_all(p)?;
        }
        let tmp = local.with_extension("hopper-download");
        let mut out = std::fs::File::create(&tmp)?;
        let mut body = resp.body_mut().with_config().limit(u64::MAX).reader();
        std::io::copy(&mut body, &mut out)?;
        out.flush()?;
        drop(out);
        std::fs::rename(&tmp, local)?;
        Ok(())
    }

    /// Saves the server's `server-icon.png` to `local` (any role may read
    /// it, unlike the file routes).
    pub fn icon(&self, id: &str, local: &Path) -> anyhow::Result<()> {
        let url = self.url(&Self::server_path(id, "/icon"));
        let mut resp = self.send_via(Lane::Call, |a| a.get(&url).header("Authorization", &self.auth()).call())?;
        if !resp.status().is_success() {
            bail!("no icon ({})", resp.status());
        }
        let png = resp.body_mut().with_config().limit(4 << 20).read_to_vec()?;
        if let Some(p) = local.parent() {
            std::fs::create_dir_all(p)?;
        }
        let tmp = local.with_extension("hopper-download");
        std::fs::write(&tmp, png)?;
        std::fs::rename(&tmp, local)?;
        Ok(())
    }

    /// Moves files on the server to its trash.
    pub fn trash(&self, id: &str, rels: &[String]) -> anyhow::Result<()> {
        self.post(&format!("/fs/{}/ops", enc(id)), json!({ "ops": [{ "op": "trash", "paths": rels }] }))?;
        Ok(())
    }
}

fn read_full(f: &mut impl std::io::Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let r = f.read(&mut buf[n..])?;
        if r == 0 {
            break;
        }
        n += r;
    }
    Ok(n)
}

// ---- resumable chunked uploads -------------------------------------------------------

/// How many times a single call that ends an upload is tried.
const FINISH_TRIES: u32 = 3;

/// What one try of a call came to.
enum Try<T> {
    Done(T),
    /// Worth another try: no answer, or a busy or broken-off one.
    Again(anyhow::Error),
    /// A refusal: trying again won't help.
    Stop(anyhow::Error),
}

impl Try<Value> {
    /// A reply's JSON as a try: signed out, refused, missing or too big
    /// stop; everything else is tried again.
    fn of(resp: anyhow::Result<ureq::http::Response<ureq::Body>>) -> Try<Value> {
        let resp = match resp {
            Ok(r) => r,
            Err(e) => return Try::Again(e),
        };
        let status = resp.status().as_u16();
        let refused = matches!(status, 401 | 403 | 404 | 409 | 413);
        match reply(Ok(resp)) {
            Ok(v) => Try::Done(v),
            Err(e) if refused => Try::Stop(e),
            Err(e) => Try::Again(e),
        }
    }
}

/// Runs `f` (given the try's number, from 1) up to `max` times, pausing
/// 2, 4, 8 s… between; the last error once they're used up.
fn with_tries<T>(max: u32, pause: &mut dyn FnMut(Duration), mut f: impl FnMut(u32) -> Try<T>) -> anyhow::Result<T> {
    let mut n = 1;
    loop {
        match f(n) {
            Try::Done(v) => return Ok(v),
            Try::Stop(e) => return Err(e),
            Try::Again(e) if n >= max => return Err(e.context(format!("gave up after {n} tries"))),
            Try::Again(e) => {
                log::warn!("try {n} of {max} failed: {e:#}");
                pause(Duration::from_secs(1 << n));
                n += 1;
            }
        }
    }
}

/// What a chunk's PUT came back with.
enum Put {
    /// Written: where the upload stands now, and whether the file is in place.
    Took { offset: u64, done: bool },
    /// The panel has a different number of bytes than the chunk assumed
    /// (an older panel after a reply was lost): carry on from there.
    Moved(u64),
    /// No answer, or one that says it failed. `again`: worth another try
    /// (a dropped connection, a cut-short chunk, a busy panel), as opposed
    /// to a refusal (signed out, no such server).
    Failed { error: anyhow::Error, again: bool },
}

/// Where an upload stands, asked after a chunk failed.
enum Where {
    At { offset: u64, done: bool },
    /// The panel no longer knows it (it expired): start that file again.
    Gone,
    /// Couldn't ask (the connection is still down).
    Unknown,
}

/// One upload's three calls, so [`push_chunks`] can be tested without a
/// panel.
trait ChunkLink {
    /// Starts the upload; the reply, with its `id`.
    fn start(&self) -> anyhow::Result<Value>;
    fn put(&self, up: &str, offset: u64, data: &[u8]) -> Put;
    fn status(&self, up: &str) -> Where;
    /// Drops an upload that won't be finished, so its part file doesn't
    /// sit on the panel for a day. Best effort.
    fn cancel(&self, up: &str);
}

/// Sends `file` (`size` bytes) through `link`. A chunk that fails is never
/// just sent again blind: the panel is asked where the upload stands (its
/// reply may have been lost after it wrote the chunk) and sending carries
/// on from there. [`CHUNK_TRIES`] failures in a row end it with the last
/// error, and an upload the panel forgot starts once more from the
/// beginning. `pause` waits between tries. Returns the reply that started
/// the upload.
fn push_chunks(
    link: &dyn ChunkLink,
    file: &mut (impl std::io::Read + std::io::Seek),
    size: u64,
    chunk: usize,
    progress: &dyn Fn(u64),
    pause: &mut dyn FnMut(Duration),
) -> anyhow::Result<Value> {
    let mut up = String::new();
    let r = push_chunks_to(link, file, size, chunk, progress, pause, &mut up);
    if r.is_err() && !up.is_empty() {
        link.cancel(&up);
    }
    r
}

/// [`push_chunks`]; `up` holds the upload's id once it started.
fn push_chunks_to(
    link: &dyn ChunkLink,
    file: &mut (impl std::io::Read + std::io::Seek),
    size: u64,
    chunk: usize,
    progress: &dyn Fn(u64),
    pause: &mut dyn FnMut(Duration),
    up: &mut String,
) -> anyhow::Result<Value> {
    use std::io::SeekFrom;
    let id_of = |v: &Value| v["id"].as_str().map(str::to_string).context("Hopper Server didn't start the upload");
    let mut start = link.start()?;
    *up = id_of(&start)?;
    let mut offset = 0u64;
    let mut fails = 0u32;
    let mut restarted = false;
    let mut buf = vec![0u8; chunk];
    loop {
        file.seek(SeekFrom::Start(offset))?;
        let want = (size - offset).min(chunk as u64) as usize;
        let n = read_full(file, &mut buf[..want])?;
        if n < want {
            bail!("the file got shorter while it was being sent");
        }
        let error = match link.put(up, offset, &buf[..n]) {
            Put::Took { done: true, .. } => {
                progress(size);
                return Ok(start);
            }
            Put::Took { offset: o, .. } if o > offset && o <= size => {
                offset = o;
                fails = 0;
                progress(o);
                continue;
            }
            Put::Took { offset: o, .. } => anyhow::anyhow!("Hopper Server didn't take the chunk at {offset} (it has {o} bytes)"),
            Put::Moved(o) if o <= size => {
                let e = anyhow::anyhow!("Hopper Server had {o} bytes, not {offset}");
                offset = o;
                progress(o);
                e
            }
            Put::Moved(o) => bail!("Hopper Server has more of the file than it is ({o} of {size} bytes)"),
            Put::Failed { error, again: false } => return Err(error),
            Put::Failed { error, .. } => error,
        };
        fails += 1;
        if fails >= CHUNK_TRIES {
            return Err(error.context(format!("gave up after {fails} tries in a row")));
        }
        log::warn!("upload chunk at {offset} failed (try {fails} of {CHUNK_TRIES}): {error:#}");
        pause(Duration::from_secs(1 << fails));
        match link.status(up) {
            Where::At { done: true, .. } => {
                progress(size);
                return Ok(start);
            }
            Where::At { offset: o, .. } if o <= size => {
                offset = o;
                progress(o);
            }
            Where::At { .. } | Where::Unknown => {}
            Where::Gone if restarted => return Err(error.context("Hopper Server forgot the upload twice")),
            Where::Gone => {
                restarted = true;
                start = link.start()?;
                *up = id_of(&start)?;
                offset = 0;
                progress(0);
            }
        }
    }
}

/// [`ChunkLink`] over a panel's upload routes (`uploads`: `/fs/{id}/uploads`
/// or `/incoming/uploads`).
struct ClientLink<'a> {
    client: &'a Client,
    uploads: String,
    start: Value,
}

impl ChunkLink for ClientLink<'_> {
    fn start(&self) -> anyhow::Result<Value> {
        self.client.post(&self.uploads, self.start.clone())
    }

    fn put(&self, up: &str, offset: u64, data: &[u8]) -> Put {
        let c = self.client;
        let url = c.url(&format!("{}/{up}?offset={offset}", self.uploads));
        let resp = c.send_via(Lane::Chunk, |a| {
            a.put(&url).header("Authorization", &c.auth()).content_type("application/octet-stream").send(data)
        });
        let mut resp = match resp {
            Ok(r) => r,
            Err(error) => return Put::Failed { error, again: true },
        };
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string();
        let v: Value = text.as_deref().ok().and_then(|t| serde_json::from_str(t).ok()).unwrap_or(Value::Null);
        match status {
            200..=299 => match v["offset"].as_u64() {
                Some(offset) => Put::Took { offset, done: v["done"].as_bool() == Some(true) },
                // The reply was cut off: ask where it stands.
                None => Put::Failed { error: anyhow::anyhow!("the reply to a chunk was cut off"), again: true },
            },
            409 if v["offset"].is_u64() => Put::Moved(v["offset"].as_u64().unwrap_or(0)),
            _ => {
                let error = match v["error"].as_str() {
                    Some(e) => anyhow::anyhow!("{e}"),
                    None if status == 401 => anyhow::anyhow!("Hopper Server doesn't know this PC any more. Pair again in Settings â†’ Server panel."),
                    // Older panels answer a chunk that didn't arrive whole
                    // with a bare 400.
                    None => anyhow::anyhow!("Hopper Server answered {status}"),
                };
                Put::Failed { error, again: !matches!(status, 401 | 403 | 404 | 413) }
            }
        }
    }

    fn cancel(&self, up: &str) {
        let _ = self.client.delete(&format!("{}/{up}", self.uploads));
    }

    fn status(&self, up: &str) -> Where {
        let c = self.client;
        let url = c.url(&format!("{}/{up}", self.uploads));
        let Ok(mut resp) = c.send_via(Lane::Call, |a| a.get(&url).header("Authorization", &c.auth()).call()) else {
            return Where::Unknown;
        };
        let status = resp.status().as_u16();
        let v: Value = resp.body_mut().read_to_string().ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
        match (status, v["offset"].as_u64()) {
            (200..=299, Some(offset)) => Where::At { offset, done: v["done"].as_bool() == Some(true) },
            // Its record is gone ("That upload has expired").
            (400, _) => Where::Gone,
            _ => Where::Unknown,
        }
    }
}

/// `s` percent-encoded for a URL path segment or query value.
/// Settings a server's owner pushed to this Hopper ([`Client::configs_pushes`]).
#[derive(Clone, Debug)]
pub struct ConfigPush {
    pub id: String,
    /// The file as on this PC (`config/x-common.toml`).
    pub rel: String,
    /// Values as the panel typed them (each a [`crate::configedit::Value::Str`]
    /// of the value's text): [`ConfigPush::typed`] reads them as each
    /// setting's kind in this PC's file.
    pub edits: Vec<crate::configedit::Edit>,
    /// Who pushed them.
    pub from: String,
    /// Unix seconds.
    pub at: u64,
}

impl ConfigPush {
    /// The edits read as the settings in `text` (this PC's copy of the file)
    /// take them; a setting the file doesn't have, or a value it can't take,
    /// says which.
    pub fn typed(&self, text: &str) -> Result<Vec<crate::configedit::Edit>, String> {
        let f = crate::configedit::Format::of(&self.rel).ok_or("Hopper can't read this kind of file as settings.")?;
        let sections = crate::configedit::parse_as(f, text)?;
        self.edits
            .iter()
            .map(|e| {
                let entry = sections
                    .iter()
                    .flat_map(|s| &s.entries)
                    .find(|x| x.path == e.path && x.key == e.key)
                    .ok_or_else(|| format!("{} isn't in this copy of the file.", e.key))?;
                let value = entry.value.parse_like(&e.value.text()).map_err(|m| format!("{}: {m}", e.key))?;
                Ok(crate::configedit::Edit { path: e.path.clone(), key: e.key.clone(), value })
            })
            .collect()
    }
}

/// This PC's settings files the server reads too (common and server
/// side), for [`Client::configs_snapshot`]: path -> text, the big and
/// unreadable ones left out, at most what the panel keeps.
pub fn snapshot_files(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut total = 0usize;
    for (rel, size) in crate::configindex::list(root) {
        // defaultconfigs/ only seeds a missing file: nothing a player's game reads.
        if !crate::configcompare::compared(&rel) || rel.starts_with("defaultconfigs/") || size > crate::configindex::MAX_BYTES {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(&rel)) else { continue };
        total += text.len();
        if out.len() >= 500 || total > 10 * 1024 * 1024 {
            break;
        }
        out.insert(rel, text);
    }
    out
}

/// A server's panel API as Hopper's server tabs use it: a Hopper Server
/// over HTTP ([`Client`]), or a server folder on this PC answered in
/// process ([`crate::hosting::local_api::LocalApi`]) with the same routes
/// and replies, so one view serves both.
pub trait PanelApi: Send + Sync {
    fn get(&self, path: &str) -> anyhow::Result<Value>;
    fn post(&self, path: &str, body: Value) -> anyhow::Result<Value>;
    fn put(&self, path: &str, body: Value) -> anyhow::Result<Value>;
    fn delete(&self, path: &str) -> anyhow::Result<Value>;
    /// A POST that starts a job, followed until it ends; its result.
    fn run_job(&self, path: &str, body: Value, job: &crate::jobs::Job) -> anyhow::Result<Value>;
    /// Copies `local` to `rel` in the server folder, replacing what's there.
    fn upload(&self, id: &str, local: &Path, rel: &str) -> anyhow::Result<()>;
    /// Copies `rel` in the server folder to `local`.
    fn download(&self, id: &str, rel: &str, local: &Path) -> anyhow::Result<()>;
    /// Saves what a GET of `path` answers (e.g. a backup as a zip) to `local`.
    fn save(&self, path: &str, _local: &Path) -> anyhow::Result<()> {
        bail!("Hopper can't save {path} here.")
    }
    /// The HTTP client, for what only a Hopper Server does.
    fn client(&self) -> Option<&Client> {
        None
    }
}

impl PanelApi for Client {
    fn get(&self, path: &str) -> anyhow::Result<Value> {
        Client::get(self, path)
    }
    fn post(&self, path: &str, body: Value) -> anyhow::Result<Value> {
        Client::post(self, path, body)
    }
    fn put(&self, path: &str, body: Value) -> anyhow::Result<Value> {
        Client::put(self, path, body)
    }
    fn delete(&self, path: &str) -> anyhow::Result<Value> {
        Client::delete(self, path)
    }
    fn run_job(&self, path: &str, body: Value, job: &crate::jobs::Job) -> anyhow::Result<Value> {
        Client::run_job(self, path, body, job)
    }
    fn upload(&self, id: &str, local: &Path, rel: &str) -> anyhow::Result<()> {
        Client::upload(self, id, local, rel)
    }
    fn download(&self, id: &str, rel: &str, local: &Path) -> anyhow::Result<()> {
        Client::download(self, id, rel, local)
    }
    fn save(&self, path: &str, local: &Path) -> anyhow::Result<()> {
        Client::save_to(self, path, local)
    }
    fn client(&self) -> Option<&Client> {
        Some(self)
    }
}

pub fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The desktop's "keep downloaded packs" days, sent with pack jobs so the
/// panel prunes its own cache the same way.
fn keep_days() -> u32 {
    crate::settings::Settings::load().map_or(30, |s| s.pack_keep_days)
}

/// Twins found so far: a local (share) server root and the same folder on a
/// Hopper Server.
static TWINS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<PathBuf, ServerInfo>>> =
    std::sync::LazyLock::new(Default::default);

/// `server` reached through a share (like `Q:\Minecraft\packs\atm10`) as
/// the same folder on one of `panels`, so work that downloads (server pack
/// updates) runs on the server's PC instead of over the share: same folder
/// name and the same `server.properties`. `None` when it isn't on one, or
/// no panel answers.
pub fn twin(server: &ServerInfo, panels: &[crate::settings::HopperPanel]) -> Option<ServerInfo> {
    if server.remote.is_some() || panels.is_empty() {
        return None;
    }
    if let Some(t) = TWINS.lock().unwrap_or_else(|e| e.into_inner()).get(&server.root) {
        return Some(t.clone());
    }
    let folder = server.root.file_name()?.to_string_lossy().to_lowercase();
    let props = "server.properties".to_string();
    let ours = crate::configdiff::file_hash(&server.root.join(&props))?;
    for p in panels {
        let Some(c) = Client::for_panel(p) else { continue };
        let Ok(infos) = c.server_infos() else { continue };
        for info in infos {
            let same_name = info.root.file_name().is_some_and(|f| f.to_string_lossy().to_lowercase() == folder);
            let Some(r) = info.remote.as_ref().filter(|_| same_name) else { continue };
            let theirs = c.hashes(&r.id, std::slice::from_ref(&props)).ok().and_then(|h| h.get(&props).cloned());
            if theirs.as_deref() == Some(ours.as_str()) {
                TWINS.lock().unwrap_or_else(|e| e.into_inner()).insert(server.root.clone(), info.clone());
                return Some(info);
            }
        }
    }
    None
}

fn info_from(v: &Value, base: &str, ca: &str, panel: &str) -> ServerInfo {
    let s = |k: &str| v[k].as_str().map(str::to_string);
    let name = s("name").unwrap_or_default();
    let folder = s("path")
        .and_then(|p| p.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().map(str::to_string))
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| name.clone());
    let properties: BTreeMap<String, String> = serde_json::from_value(v["properties"].clone()).unwrap_or_default();
    let root = panel_root(panel, &folder);
    let remote = Remote { base: base.to_string(), id: s("id").unwrap_or_default(), ca: ca.to_string(), panel: panel.to_string() };
    register(&root, &remote);
    ServerInfo {
        name,
        root,
        manager: match v["managed_by"].as_str() {
            Some("Crafty") => Manager::Crafty,
            Some("MCSS") => Manager::Mcss,
            _ => Manager::Unknown,
        },
        properties,
        loader: s("loader"),
        java: s("java"),
        max_memory: s("max_memory"),
        remote: Some(remote),
    }
}

// ---- running sync operations on a remote server --------------------------------

/// `p` relative to the stand-in root, with forward slashes.
fn remote_rel(root: &Path, p: &Path) -> Option<String> {
    let rel = p.strip_prefix(root).ok()?;
    Some(rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/"))
}

/// Runs sync operations where one side is a remote server: copies to it
/// upload, copies from it download (through the local side's backups),
/// deletes on it go to the panel's trash.
/// `progress` gets (done, total) in [`crate::sync::op_weight`]s, during
/// each upload too.
pub fn execute(ops: Vec<SyncOp>, remote: &Remote, root: &Path, local_root: &Path, progress: &dyn Fn(u64, u64)) -> Vec<SyncResult> {
    let client = match Client::for_remote(remote) {
        Ok(c) => c,
        Err(e) => {
            return ops
                .into_iter()
                .map(|op| SyncResult { op, ok: false, error: Some(format!("{e:#}")), backup: None })
                .collect();
        }
    };
    let mut out = Vec::new();
    let total: u64 = ops.iter().map(crate::sync::op_weight).sum();
    let mut before = 0u64;
    for op in ops {
        let weight = crate::sync::op_weight(&op);
        let at = before;
        before += weight;
        progress(at, total);
        let r: anyhow::Result<()> = match &op {
            SyncOp::Copy { src, dst } if is_remote_root(dst) => match remote_rel(root, dst) {
                Some(rel) => client.upload_with(&remote.id, src, &rel, &|sent| progress(at + sent.min(weight), total)),
                None => Err(anyhow::anyhow!("{} isn't on this server", dst.display())),
            },
            SyncOp::Copy { src, dst } if is_remote_root(src) => match remote_rel(root, src) {
                // Through a temp file and the local backups, like a copy.
                Some(rel) => {
                    let tmp = std::env::temp_dir().join(format!("hopper-pull-{}", std::process::id()));
                    let staged = tmp.join(src.file_name().unwrap_or_default());
                    let r = client.download(&remote.id, &rel, &staged).and_then(|_| {
                        let res = crate::sync::execute_local(vec![SyncOp::Copy { src: staged.clone(), dst: dst.clone() }], local_root);
                        match res.into_iter().next() {
                            Some(r) if r.ok => Ok(()),
                            Some(r) => Err(anyhow::anyhow!(r.error.unwrap_or_default())),
                            None => Ok(()),
                        }
                    });
                    let _ = std::fs::remove_dir_all(&tmp);
                    r
                }
                None => Err(anyhow::anyhow!("{} isn't on this server", src.display())),
            },
            SyncOp::Delete { path } if is_remote_root(path) => match remote_rel(root, path) {
                Some(rel) => client.trash(&remote.id, &[rel]),
                None => Err(anyhow::anyhow!("{} isn't on this server", path.display())),
            },
            // Through the panel's own switch, so its mod list stays right.
            SyncOp::Disable { path } | SyncOp::Enable { path } if is_remote_root(path) => {
                let enabled = matches!(op, SyncOp::Enable { .. });
                match remote_rel(root, path).as_deref().and_then(|r| r.strip_prefix("mods/")) {
                    Some(file) if !file.contains('/') => client
                        .post(&Client::server_path(&remote.id, "/mods/toggle"), serde_json::json!({ "file": file, "enabled": enabled }))
                        .map(|_| ()),
                    _ => Err(anyhow::anyhow!("{} isn't in the server's mods folder", path.display())),
                }
            }
            // Local-only operations (removing the client's old jar on a pull).
            _ => {
                let res = crate::sync::execute_local(vec![op.clone()], local_root);
                out.extend(res);
                continue;
            }
        };
        out.push(match r {
            Ok(()) => SyncResult { op, ok: true, error: None, backup: None },
            Err(e) => SyncResult { op, ok: false, error: Some(format!("{e:#}")), backup: None },
        });
    }
    progress(total, total);
    out
}

// ---- finding and pairing ----------------------------------------------------------

/// A panel found on the LAN.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub name: String,
    pub url: String,
    pub version: String,
}

/// Looks for Hopper Servers on the LAN for up to `wait`.
pub fn discover(wait: Duration) -> Vec<Found> {
    // Test copies skip the multicast socket: Windows asks for firewall
    // access for it.
    if crate::paths::loopback_only() {
        return Vec::new();
    }
    let Ok(mdns) = mdns_sd::ServiceDaemon::new() else { return Vec::new() };
    let Ok(rx) = mdns.browse(SERVICE) else { return Vec::new() };
    let end = std::time::Instant::now() + wait;
    let mut out: Vec<Found> = Vec::new();
    while let Some(left) = end.checked_duration_since(std::time::Instant::now()) {
        if let Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) = rx.recv_timeout(left) {
            // One entry per panel, on its likeliest LAN address.
            let mut addrs: Vec<std::net::IpAddr> = info.get_addresses().iter().copied().filter(|a| a.is_ipv4()).collect();
            addrs.sort_by_key(addr_rank);
            if let Some(ip) = addrs.first() {
                let name = info.get_fullname().trim_end_matches(SERVICE).trim_end_matches('.').to_string();
                let found = Found {
                    name: name.clone(),
                    url: format!("http://{ip}:{}", info.get_port()),
                    version: info.get_property_val_str("version").unwrap_or("").to_string(),
                };
                match out.iter_mut().find(|f| f.name == name) {
                    Some(f) if addr_rank(ip) < url_rank(&f.url) => *f = found,
                    Some(_) => {}
                    None => out.push(found),
                }
            }
        }
    }
    let _ = mdns.shutdown();
    out
}

/// Home LAN first, then other private ranges, Tailscale, the rest.
fn addr_rank(a: &std::net::IpAddr) -> u8 {
    match a {
        std::net::IpAddr::V4(v) => match v.octets() {
            [192, 168, ..] => 0,
            [10, ..] => 1,
            [172, 16..=31, ..] => 2,
            [100, 64..=127, ..] => 3,
            _ => 4,
        },
        std::net::IpAddr::V6(_) => 5,
    }
}

fn url_rank(url: &str) -> u8 {
    url.split("://")
        .nth(1)
        .and_then(|h| h.split(':').next())
        .and_then(|h| h.parse::<std::net::IpAddr>().ok())
        .map_or(9, |a| addr_rank(&a))
}

#[derive(Debug, Deserialize)]
pub struct Hello {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub tls: Option<HelloTls>,
}

#[derive(Debug, Deserialize)]
pub struct HelloTls {
    pub fingerprint: String,
    pub ca: String,
    pub domain: Option<String>,
    /// The panel's address with a Let's Encrypt certificate (`https://…`,
    /// port 443), while it has one.
    #[serde(default)]
    pub trusted: Option<String>,
}

/// The panel's name, version and certificate authority. Unverified: only
/// to learn the CA, which pairing then pins and the codes confirm.
pub fn hello(url: &str) -> anyhow::Result<Hello> {
    hello_at(&base_url(url), CONNECT_WAIT)
}

/// `connect`: how long to wait for the connection to open. A short wait is
/// a guess at one of several addresses, so it isn't tried again.
fn hello_at(base: &str, connect: Duration) -> anyhow::Result<Hello> {
    let agent = agent_timed(base, connect, Some(REPLY_WAIT), CALL_WAIT, &Trust::Unverified);
    let v = reply(send(base, connect >= CONNECT_WAIT, || agent.get(&format!("{base}/api/v1/hello")).call()))?;
    if v["panel"].as_str() != Some("hopperd") {
        bail!("{base} isn't a Hopper Server.");
    }
    Ok(serde_json::from_value(v)?)
}

/// [`hello`] for a typed address, trying each of [`candidates`]: the
/// address that answered (port included) and what it said.
pub fn find(url: &str) -> anyhow::Result<(String, Hello)> {
    let (base, h) = first_reachable(url, hello_at)?;
    // Reached by a public IP while the panel has a trusted name for it: use
    // that name when it answers, so what's saved works like any website.
    if let Some(t) = h.tls.as_ref().and_then(|t| t.trusted.as_deref()).filter(|t| prefer_trusted(&base, t)) {
        let t = trusted_base(t);
        if let Ok(h2) = hello_at(&t, PROBE_WAIT)
            && h2.tls.as_ref().map(|x| &x.fingerprint) == h.tls.as_ref().map(|x| &x.fingerprint)
        {
            return Ok((t, h2));
        }
    }
    Ok((base, h))
}

/// Whether to move from `base` to the panel's trusted address: only from a
/// public IP (a LAN, Tailscale or named address stays as typed).
fn prefer_trusted(base: &str, trusted: &str) -> bool {
    let host = base.split("://").nth(1).unwrap_or("").split('/').next().unwrap_or("");
    let host = host.rsplit_once(':').map_or(host, |(h, p)| if p.chars().all(|c| c.is_ascii_digit()) { h } else { host });
    let Ok(ip) = host.trim_matches(['[', ']']).parse::<std::net::IpAddr>() else { return false };
    let public = match ip {
        std::net::IpAddr::V4(v) => !(v.is_private() || v.is_loopback() || v.is_link_local() || matches!(v.octets(), [100, 64..=127, ..])),
        std::net::IpAddr::V6(v) => (v.segments()[0] & 0xe000) == 0x2000,
    };
    public && trusted_base(trusted) != base_url(base)
}

/// The trusted address names no port: it's 443, where Let's Encrypt checked it.
fn trusted_base(trusted: &str) -> String {
    let t = trusted.trim().trim_end_matches('/');
    let host = t.split("://").nth(1).unwrap_or(t);
    let has_port = host.rsplit_once(':').is_some_and(|(h, p)| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) && (!h.contains(':') || h.ends_with(']')));
    base_url(&if has_port { t.to_string() } else { format!("{t}:443") })
}

/// A pairing request waiting for the owner.
#[derive(Clone, Debug)]
pub struct Pairing {
    pub base: String,
    pub id: String,
    pub secret: String,
    /// The 6-digit code, worked out here from the panel's certificate; the
    /// panel shows its own, and they only match for the real panel.
    pub code: String,
    /// The panel's CA (PEM), to keep once paired; empty when a LAN name's
    /// certificate turned out to be a proxy's public one, so the saved
    /// panel is checked with the web roots instead of a pin.
    pub ca: String,
    pub fingerprint: String,
    /// One agent for the whole wait, so the polls reuse one connection
    /// instead of opening a new one through the proxy every second.
    agent: ureq::Agent,
}

/// Whether an error is the certificate being refused (rather than the
/// panel being slow or away).
fn cert_rejected(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        let s = c.to_string().to_lowercase();
        s.contains("certificate") || s.contains("unknownissuer") || s.contains("notvalidforname")
    })
}

/// Asks the panel to pair this PC, named `device`. Everything after
/// learning the CA goes over a checked connection: pinned to that CA on a
/// LAN address, the web roots on a public name. A LAN or Tailscale name
/// whose certificate isn't the panel's own (a proxy in front of it, such as
/// `tailscale serve`) is checked with the web roots instead; if those don't
/// accept it either, pairing stops.
pub fn pair_request(url: &str, device: &str) -> anyhow::Result<Pairing> {
    let (base, h) = find(url)?;
    let tls = h.tls.context("This Hopper Server is too old for encrypted pairing; update it.")?;
    let fingerprint = fingerprint_of(&tls.ca).context("The panel sent a broken certificate.")?;
    let ask = |trust: &Trust| {
        let agent = agent_with(&base, trust);
        let v = reply(send(&base, true, || {
            agent.post(&format!("{base}/api/v1/pair/request")).send_json(json!({ "device": device }))
        }));
        v.map(|v| (v, agent))
    };
    let first = trust_for(&base, &tls.ca);
    let (v, agent, ca) = match ask(&first) {
        Ok((v, a)) => (v, a, tls.ca),
        Err(e) if matches!(first, Trust::Pinned(_)) && cert_rejected(&e) => match ask(&Trust::Web) {
            Ok((v, a)) => (v, a, String::new()),
            Err(_) => return Err(e.context(format!("{base} answered with a certificate that isn't the panel's"))),
        },
        Err(e) if cert_rejected(&e) => return Err(e.context(format!("{base}'s certificate didn't check out"))),
        Err(e) => return Err(e.context("Couldn't send the pairing request")),
    };
    let (id, secret) = (v["id"].as_str().unwrap_or("").to_string(), v["secret"].as_str().unwrap_or("").to_string());
    if id.is_empty() || secret.is_empty() {
        bail!("{base} didn't take the pairing request.");
    }
    Ok(Pairing { code: pair_code(&fingerprint, &id, &secret), base, id, secret, ca, fingerprint, agent })
}

#[derive(Debug, PartialEq)]
pub enum PairState {
    Waiting,
    /// The token to keep.
    Approved(String),
    Denied,
    Expired,
}

/// Where a pairing request stands.
pub fn pair_status(p: &Pairing) -> anyhow::Result<PairState> {
    let v = reply(send(&p.base, true, || {
        p.agent.post(&format!("{}/api/v1/pair/status", p.base)).send_json(json!({ "id": p.id, "secret": p.secret }))
    }))?;
    Ok(match v["status"].as_str() {
        Some("waiting") => PairState::Waiting,
        Some("approved") => PairState::Approved(v["token"].as_str().unwrap_or("").into()),
        Some("denied") => PairState::Denied,
        _ => PairState::Expired,
    })
}

/// How long the owner has to allow a request (the panel forgets it then).
pub const PAIR_WAIT: Duration = Duration::from_secs(300);
/// How long the panel may stay out of reach while waiting before pairing
/// gives up. One lost poll on a slow link or through a proxy is normal.
const PAIR_OUT_OF_REACH: Duration = Duration::from_secs(60);

/// Waits for the owner to answer `p`, asking every second or so. Polls that
/// fail are tried again; pairing only stops when the panel has been out of
/// reach for a minute. Returns `Approved`, `Denied` or `Expired`.
pub fn wait_for_answer(p: &Pairing) -> anyhow::Result<PairState> {
    wait_with(|| pair_status(p), std::thread::sleep, Instant::now, PAIR_WAIT)
}

/// [`wait_for_answer`] with the poll, sleep and clock passed in.
fn wait_with(
    mut poll: impl FnMut() -> anyhow::Result<PairState>,
    mut sleep: impl FnMut(Duration),
    now: impl Fn() -> Instant,
    wait: Duration,
) -> anyhow::Result<PairState> {
    let start = now();
    let mut reached = start;
    let mut failed: Option<anyhow::Error> = None;
    loop {
        sleep(if failed.is_some() { Duration::from_secs(3) } else { Duration::from_secs(1) });
        match poll() {
            Ok(PairState::Waiting) if now().duration_since(start) < wait => {
                reached = now();
                failed = None;
            }
            Ok(PairState::Waiting) => return Ok(PairState::Expired),
            // The panel forgot the request right after a lost answer: it
            // may have been the approval (panels before 0.13.14 hand the
            // token out only once).
            Ok(PairState::Expired) if failed.is_some() => bail!(
                "Lost the panel's answer: it may have allowed this PC already. Remove this PC from Access & devices in the panel, then pair again."
            ),
            Ok(s) => return Ok(s),
            Err(e) => {
                if now().duration_since(reached) >= PAIR_OUT_OF_REACH || now().duration_since(start) >= wait {
                    return Err(e.context("Lost the connection to the panel while waiting for it to allow this PC"));
                }
                failed = Some(e);
            }
        }
    }
}

/// The 6-digit pairing code: from the panel's certificate fingerprint and
/// the request, so both sides only show the same code when the desktop
/// talks to the real panel (a machine in the middle has another
/// certificate, so the codes differ).
pub fn pair_code(fingerprint: &str, id: &str, secret: &str) -> String {
    let h = crate::javadl::sha256_hex(format!("{}|{id}|{secret}", fingerprint.to_uppercase()).as_bytes());
    let n = u32::from_str_radix(&h[..8], 16).unwrap_or(0);
    format!("{:06}", n % 1_000_000)
}

/// "Hopper on FOX".
pub fn device_name() -> String {
    let host = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "this PC".into());
    format!("Hopper on {host}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn who_may_manage_a_panel() {
        let me = |v: Value| may_manage(&v);
        assert!(me(json!({ "role": "owner" })) && me(json!({ "role": "manager" })));
        assert!(me(json!({ "role": "device", "manage": true })));
        // A plain device (and an old panel's answer without the flag) can't.
        assert!(!me(json!({ "role": "device" })) && !me(json!({ "role": "device", "manage": false })));
        assert!(!me(json!({ "role": "admin" })) && !me(json!({ "role": "token" })));
        // Nothing known (an old panel): offered; the panel still checks.
        assert!(me(json!({})));
        assert_eq!(
            cant_manage_text("rig", "delete"),
            "This PC can't delete servers on rig. In rig's panel, open Settings › Access & devices and allow this PC to manage it, or delete the server there."
        );
    }

    /// Against the real panel and share named in `HOPPER_TWIN_ROOT`: read
    /// only (lists servers, hashes server.properties).
    #[test]
    #[ignore]
    fn twin_live() {
        let Ok(root) = std::env::var("HOPPER_TWIN_ROOT") else { return };
        let s = crate::settings::Settings::load().expect("settings");
        let server = crate::server::load(Path::new(&root));
        let t = twin(&server, &s.hopper_panels);
        println!("twin of {root}: {:?}", t.as_ref().map(|t| (t.root.clone(), t.remote.as_ref().map(|r| r.id.clone()))));
        assert!(t.is_some());
    }

    #[test]
    fn urls() {
        assert_eq!(base_url("rig"), "https://rig:8790");
        assert_eq!(base_url("192.168.50.3:9000/"), "https://192.168.50.3:9000");
        assert_eq!(base_url("https://panel.example.com"), "https://panel.example.com:8790");
        assert_eq!(base_url("http://rig:8790"), "https://rig:8790");
        assert_eq!(base_url("https://panel.example.com:443"), "https://panel.example.com:443");
        for (b, private) in [
            ("https://rig:8790", true),
            ("https://192.168.50.3:8790", true),
            ("https://rig.tail3c9e.ts.net:8790", true),
            ("https://fox.local:8790", true),
            ("https://panel.example.com", false),
        ] {
            assert_eq!(private_host(b), private, "{b}");
        }
        assert_eq!(pair_code("aa:bb", "1", "2"), pair_code("AA:BB", "1", "2"));
        assert_ne!(pair_code("AA:BB", "1", "2"), pair_code("AA:BC", "1", "2"));
        assert_eq!(pair_code("AA", "1", "2").len(), 6);
        assert_eq!(enc("config/a b.toml"), "config%2Fa%20b.toml");
    }

    #[test]
    fn address_candidates() {
        // A web address with no port: the proxy's 443 first, then hopperd's.
        for typed in ["https://hopper.example.com", "https://hopper.example.com/", " https://hopper.example.com/ "] {
            assert_eq!(candidates(typed), ["https://hopper.example.com:443", "https://hopper.example.com:8790"], "{typed:?}");
        }
        // Everything else: one address, as before.
        assert_eq!(candidates("rig"), ["https://rig:8790"]);
        assert_eq!(candidates("rig:9000"), ["https://rig:9000"]);
        assert_eq!(candidates("192.168.50.3"), ["https://192.168.50.3:8790"]);
        assert_eq!(candidates("https://hopper.example.com:8443"), ["https://hopper.example.com:8443"]);
        assert_eq!(candidates("http://hopper.example.com"), ["https://hopper.example.com:8790"]);
        assert_eq!(candidates("http://rig:8791"), ["https://rig:8791"]);

        assert!(same_panel("https://hopper.example.com:443", "https://hopper.example.com"));
        assert!(same_panel("https://hopper.example.com:8790", "https://hopper.example.com/"));
        assert!(same_panel("https://rig:8790", "rig"));
        assert!(!same_panel("https://rig:8791", "rig"));
    }

    #[test]
    fn probe_falls_back_to_panel_port() {
        use std::cell::RefCell;
        let tried = RefCell::new(Vec::new());
        // 443 answers: no second try, and the saved address names the port.
        let r = first_reachable("https://hopper.example.com", |b, t| {
            tried.borrow_mut().push((b.to_string(), t));
            Ok(1)
        });
        assert_eq!(r.unwrap(), ("https://hopper.example.com:443".to_string(), 1));
        assert_eq!(tried.borrow().len(), 1);
        assert_eq!(tried.borrow()[0].1, PROBE_WAIT);

        // 443 times out: 8790 is tried and kept.
        tried.borrow_mut().clear();
        let r = first_reachable("https://hopper.example.com", |b, t| {
            tried.borrow_mut().push((b.to_string(), t));
            if b.ends_with(":443") { bail!("timeout: global") } else { Ok(2) }
        });
        assert_eq!(r.unwrap(), ("https://hopper.example.com:8790".to_string(), 2));
        assert_eq!(tried.borrow().len(), 2);
        assert_eq!(tried.borrow()[1].1, CONNECT_WAIT);

        // Both fail: the error names both addresses.
        let e = first_reachable("https://hopper.example.com", |b, _| -> anyhow::Result<()> { bail!("{b} isn't a Hopper Server.") })
            .unwrap_err()
            .to_string();
        assert!(e.contains("https://hopper.example.com:443") && e.contains("https://hopper.example.com:8790"), "{e}");

        // One address: one try, its error unchanged.
        tried.borrow_mut().clear();
        let e = first_reachable("rig", |b, t| -> anyhow::Result<()> {
            tried.borrow_mut().push((b.to_string(), t));
            bail!("timeout: global")
        })
        .unwrap_err();
        assert_eq!(e.to_string(), "timeout: global");
        assert_eq!(*tried.borrow(), [("https://rig:8790".to_string(), CONNECT_WAIT)]);
    }

    /// Runs [`wait_with`] over scripted poll results on a fake clock; each
    /// poll takes `poll_takes`. Returns the outcome and how many polls ran.
    fn wait_script(script: Vec<anyhow::Result<PairState>>, poll_takes: Duration) -> (anyhow::Result<PairState>, usize) {
        use std::cell::{Cell, RefCell};
        let t0 = Instant::now();
        let clock = Cell::new(t0);
        let script = RefCell::new(script.into_iter());
        let polls = Cell::new(0);
        let out = wait_with(
            || {
                polls.set(polls.get() + 1);
                clock.set(clock.get() + poll_takes);
                // Past the script: the panel stays out of reach.
                script.borrow_mut().next().unwrap_or_else(|| Err(anyhow::anyhow!("timeout: global")))
            },
            |d| clock.set(clock.get() + d),
            || clock.get(),
            PAIR_WAIT,
        );
        (out, polls.get())
    }

    #[test]
    fn waiting_for_approval_survives_lost_polls() {
        use PairState::*;
        let lost = || Err(anyhow::anyhow!("can't reach Hopper Server at https://x:443: timeout: global"));
        let slow = Duration::from_secs(15);
        // A poll lost through the proxy, then the approval: paired.
        let (out, polls) = wait_script(vec![Ok(Waiting), lost(), lost(), Ok(Approved("tok".into()))], slow);
        assert_eq!(out.unwrap(), Approved("tok".into()));
        assert_eq!(polls, 4);
        // Denied and expired come straight back.
        assert_eq!(wait_script(vec![Ok(Waiting), Ok(Denied)], slow).0.unwrap(), Denied);
        assert_eq!(wait_script(vec![Ok(Expired)], slow).0.unwrap(), Expired);
        // Out of reach for a minute: gives up, and says at which step.
        let (out, polls) = wait_script(vec![Ok(Waiting)], slow);
        let e = format!("{:#}", out.unwrap_err());
        assert!(e.starts_with("Lost the connection to the panel while waiting") && e.contains("timeout: global"), "{e}");
        assert!((3..=6).contains(&polls), "{polls}");
        // An old panel that forgot the request after a lost reply: it may
        // have been the approval, so say what to do instead of "expired".
        let e = format!("{:#}", wait_script(vec![Ok(Waiting), lost(), Ok(Expired)], slow).0.unwrap_err());
        assert!(e.contains("Remove this PC"), "{e}");
        // Nobody answers for the whole wait: expired.
        let fast = Duration::from_millis(50);
        let (out, polls) = wait_script((0..1000).map(|_| Ok(Waiting)).collect(), fast);
        assert_eq!(out.unwrap(), Expired);
        assert!(polls > 200 && polls < 320, "{polls}");
    }

    /// Runs [`send_with`] over scripted results; returns the outcome, how
    /// many tries ran and the pauses taken.
    fn send_script(retry: bool, script: Vec<Result<u8, ureq::Error>>) -> (anyhow::Result<u8>, usize, Vec<Duration>) {
        use std::cell::{Cell, RefCell};
        let script = RefCell::new(script.into_iter());
        let tries = Cell::new(0);
        let mut pauses = Vec::new();
        let out = send_with(
            "https://hopper.example.com:443",
            retry,
            || {
                tries.set(tries.get() + 1);
                script.borrow_mut().next().unwrap_or(Err(ureq::Error::Timeout(ureq::Timeout::Connect)))
            },
            |d| pauses.push(d),
            |_| "checked again: TCP 0.1 s".into(),
        );
        (out, tries.get(), pauses)
    }

    #[test]
    fn connections_that_dont_open_are_tried_again() {
        use ureq::{Error as E, Timeout as T};
        // A lost connection, then it opens: one pause, no error.
        let (out, tries, pauses) = send_script(true, vec![Err(E::Timeout(T::Connect)), Ok(7)]);
        assert_eq!(out.unwrap(), 7);
        assert_eq!((tries, pauses), (2, vec![RETRY_PAUSES[0]]));
        // Never opens: three tries, and the error says which step, how
        // many tries and what the TCP check found.
        let (out, tries, pauses) = send_script(true, vec![]);
        let e = format!("{:#}", out.unwrap_err());
        assert_eq!(tries, 3);
        assert_eq!(pauses, RETRY_PAUSES.to_vec());
        assert!(e.starts_with("can't reach Hopper Server at https://hopper.example.com:443: the connection didn't open after"), "{e}");
        assert!(e.contains("timeout: connect") && e.contains("tried 3 times") && e.contains("TCP 0.1 s"), "{e}");
        // A lookup that fails is tried again too.
        let (out, tries, _) = send_script(true, vec![Err(E::HostNotFound), Ok(1)]);
        assert_eq!((out.unwrap(), tries), (1, 2));
        // Once the request may have been sent, it isn't sent again.
        for e in [E::Timeout(T::RecvResponse), E::Timeout(T::Global), E::Timeout(T::SendBody)] {
            let (out, tries, pauses) = send_script(true, vec![Err(e), Ok(1)]);
            assert!(out.is_err());
            assert_eq!((tries, pauses.len()), (1, 0));
        }
        let (out, _, _) = send_script(true, vec![Err(E::Timeout(T::RecvResponse))]);
        let e = format!("{:#}", out.unwrap_err());
        assert!(e.contains("no reply came after") && !e.contains("checked again"), "{e}");
        // A guess at one of several addresses isn't tried again.
        let (out, tries, _) = send_script(false, vec![Err(E::Timeout(T::Connect)), Ok(1)]);
        assert!(out.is_err());
        assert_eq!(tries, 1);
    }

    #[test]
    fn certificate_errors_told_apart() {
        let cert = anyhow::anyhow!("invalid peer certificate: UnknownIssuer").context("can't reach Hopper Server at https://rig:8790");
        assert!(cert_rejected(&cert));
        assert!(cert_rejected(&anyhow::anyhow!("invalid peer certificate: NotValidForName")));
        let slow = anyhow::anyhow!("timeout: global").context("can't reach Hopper Server at https://rig:8790");
        assert!(!cert_rejected(&slow));
    }

    #[test]
    fn web_urls() {
        let root = remote_root("Web Test");
        register(&root, &Remote { base: "http://rig:8790".into(), id: "web-test".into(), ca: String::new(), panel: String::new() });
        assert_eq!(
            web_url(&root.join("config").join("a b.toml")).unwrap(),
            "http://rig:8790/#/server/web-test/edit/config/a%20b.toml"
        );
        assert_eq!(web_url(&root.join("mods")).unwrap(), "http://rig:8790/#/server/web-test/files/mods");
        assert_eq!(web_url(&root).unwrap(), "http://rig:8790/#/server/web-test/files");
        assert!(web_url(Path::new(r"C:\x")).is_none());
    }

    #[test]
    fn a_public_ip_moves_to_the_trusted_address() {
        assert!(prefer_trusted("https://81.2.69.160:8790", "https://81-2-69-160.sslip.io"));
        assert!(prefer_trusted("https://81.2.69.160:8790", "https://81.2.69.160"));
        assert!(!prefer_trusted("https://192.168.50.3:8790", "https://81-2-69-160.sslip.io"));
        assert!(!prefer_trusted("https://100.101.102.103:8790", "https://81-2-69-160.sslip.io"));
        assert!(!prefer_trusted("https://hopper.example.com", "https://81-2-69-160.sslip.io"));
        assert!(!prefer_trusted("https://81.2.69.160:443", "https://81.2.69.160"));
        assert_eq!(trusted_base("https://81-2-69-160.sslip.io"), "https://81-2-69-160.sslip.io:443");
        assert_eq!(trusted_base("https://[2a01:4f8::1]"), "https://[2a01:4f8::1]:443");
    }

    #[test]
    fn lan_addresses_rank_first() {
        assert!(url_rank("http://192.168.50.2:8790") < url_rank("http://10.66.0.2:8790"));
        assert!(url_rank("http://10.66.0.2:8790") < url_rank("http://100.101.2.3:8790"));
    }

    #[test]
    #[cfg_attr(not(windows), ignore = "uses Windows paths and tools")]
    fn remote_roots() {
        let root = remote_root("TNP Limitless 8");
        assert!(is_remote_root(&root));
        assert_eq!(root.file_name().unwrap(), "TNP Limitless 8");
        assert_eq!(remote_rel(&root, &root.join("mods").join("a.jar")).as_deref(), Some("mods/a.jar"));
        assert!(!is_remote_root(Path::new(r"Q:\Minecraft\packs\TNP")));
        // Reading it fails at once instead of waiting on the network.
        let t = std::time::Instant::now();
        assert!(std::fs::read_dir(root.join("mods")).is_err());
        assert!(t.elapsed() < Duration::from_millis(500));
    }

    #[test]
    #[cfg_attr(not(windows), ignore = "uses Windows paths and tools")]
    fn server_info_from_panel_json() {
        let v = json!({
            "id": "tnp-limitless-8", "name": "TNP Limitless 8", "path": "G:\\Minecraft\\packs\\TNP Limitless 8",
            "managed_by": null, "loader": "neoforge 21.1.172", "java": null, "max_memory": "12G",
            "properties": { "server-port": "25565", "motd": "hi" }
        });
        let i = info_from(&v, "https://rig:8790", "", "");
        assert_eq!(i.root, remote_root("TNP Limitless 8"));
        let j = info_from(&v, "https://vps:8790", "", "p2");
        assert_eq!(j.root, panel_root("p2", "TNP Limitless 8"));
        assert_eq!(j.root.file_name().unwrap(), "TNP Limitless 8");
        assert_eq!(host_name("https://rig:8790"), "rig");
        assert_eq!(i.port(), 25565);
        assert_eq!(i.remote.as_ref().unwrap().id, "tnp-limitless-8");
        assert_eq!(i.max_memory.as_deref(), Some("12G"));
        assert_eq!(i.manager, Manager::Unknown);
    }
}

#[cfg(test)]
mod chunk_tests {
    use super::*;
    use std::cell::RefCell;

    /// What goes wrong with one PUT.
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Fault {
        None,
        /// The panel writes the chunk, the reply never arrives.
        LoseReply,
        /// The connection drops mid-chunk: nothing written, a 400.
        CutShort,
        /// Signed out.
        Refused,
        /// The panel forgets the upload (it expired) before this chunk.
        Forget,
    }

    /// A panel's upload routes in memory. `old`: a panel from before
    /// chunks could be sent twice (a 409 for any other offset, no record
    /// of finished uploads).
    struct Fake {
        old: bool,
        size: u64,
        faults: RefCell<Vec<Fault>>,
        state: RefCell<FakeState>,
    }

    #[derive(Default)]
    struct FakeState {
        id: u32,
        data: Vec<u8>,
        done: bool,
        known: bool,
        puts: Vec<(u64, usize)>,
        starts: u32,
        /// Copies of the file put in place.
        placed: Vec<Vec<u8>>,
        cancelled: Vec<String>,
    }

    impl Fake {
        fn new(old: bool, size: u64, faults: &[Fault]) -> Fake {
            let mut f = faults.to_vec();
            f.reverse();
            Fake { old, size, faults: RefCell::new(f), state: RefCell::default() }
        }
    }

    impl ChunkLink for Fake {
        fn start(&self) -> anyhow::Result<Value> {
            let mut s = self.state.borrow_mut();
            s.id += 1;
            s.starts += 1;
            s.data.clear();
            s.done = false;
            s.known = true;
            Ok(json!({ "id": format!("{:x}", s.id), "path": "mods/a.jar" }))
        }

        fn put(&self, up: &str, offset: u64, data: &[u8]) -> Put {
            let fault = self.faults.borrow_mut().pop().unwrap_or(Fault::None);
            let mut s = self.state.borrow_mut();
            s.puts.push((offset, data.len()));
            if fault == Fault::Forget {
                s.known = false;
            }
            if up != format!("{:x}", s.id) || (!s.known && !(s.done && !self.old)) {
                return Put::Failed { error: anyhow::anyhow!("That upload has expired. Start it again."), again: true };
            }
            match fault {
                Fault::CutShort => return Put::Failed { error: anyhow::anyhow!("Hopper Server answered 400"), again: true },
                Fault::Refused => return Put::Failed { error: anyhow::anyhow!("signed out"), again: false },
                _ => {}
            }
            let have = s.data.len() as u64;
            if s.done {
                return Put::Took { offset: self.size, done: true };
            }
            if offset > have || (self.old && offset != have) {
                return Put::Moved(have);
            }
            let skip = (have - offset) as usize;
            if skip < data.len() {
                s.data.extend_from_slice(&data[skip..]);
            }
            let now = s.data.len() as u64;
            let done = now == self.size;
            if done {
                s.done = true;
                let d = s.data.clone();
                s.placed.push(d);
                if self.old {
                    s.known = false;
                }
            }
            if fault == Fault::LoseReply {
                return Put::Failed { error: anyhow::anyhow!("no reply came after 60.0 s (timeout: recv_response)"), again: true };
            }
            Put::Took { offset: now, done }
        }

        fn cancel(&self, up: &str) {
            self.state.borrow_mut().cancelled.push(up.to_string());
        }

        fn status(&self, up: &str) -> Where {
            let s = self.state.borrow();
            if up != format!("{:x}", s.id) {
                return Where::Gone;
            }
            if s.done && !self.old {
                return Where::At { offset: self.size, done: true };
            }
            if !s.known {
                return Where::Gone;
            }
            Where::At { offset: s.data.len() as u64, done: false }
        }
    }

    fn file(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 % 251) as u8).collect()
    }

    /// Sends a 10-byte file in 3-byte chunks; the result, the pauses taken
    /// and the progress reported.
    fn run(fake: &Fake) -> (anyhow::Result<Value>, Vec<Duration>, Vec<u64>) {
        let bytes = file(fake.size as usize);
        let mut cur = std::io::Cursor::new(bytes);
        let mut pauses = Vec::new();
        let seen = RefCell::new(Vec::new());
        let r = push_chunks(fake, &mut cur, fake.size, 3, &|o| seen.borrow_mut().push(o), &mut |d| pauses.push(d));
        (r, pauses, seen.into_inner())
    }

    #[test]
    fn a_clean_upload_sends_each_chunk_once() {
        let fake = Fake::new(false, 10, &[]);
        let (r, pauses, seen) = run(&fake);
        r.unwrap();
        let s = fake.state.borrow();
        assert_eq!(s.puts, vec![(0, 3), (3, 3), (6, 3), (9, 1)]);
        assert_eq!(s.placed, vec![file(10)]);
        assert!(pauses.is_empty() && s.cancelled.is_empty());
        assert_eq!(seen, vec![3, 6, 9, 10]);
    }

    #[test]
    fn a_lost_reply_asks_where_it_stands_and_goes_on() {
        // The panel wrote chunk 2 but the reply was lost (the case behind
        // a sync stuck at "Syncing 1 files").
        let fake = Fake::new(false, 10, &[Fault::None, Fault::LoseReply]);
        let (r, pauses, _) = run(&fake);
        r.unwrap();
        let s = fake.state.borrow();
        assert_eq!(s.puts, vec![(0, 3), (3, 3), (6, 3), (9, 1)], "nothing sent twice");
        assert_eq!(s.placed, vec![file(10)]);
        assert_eq!(pauses, vec![Duration::from_secs(2)]);
    }

    #[test]
    fn a_chunk_cut_short_is_sent_again() {
        let fake = Fake::new(false, 10, &[Fault::None, Fault::None, Fault::CutShort]);
        let (r, _, _) = run(&fake);
        r.unwrap();
        let s = fake.state.borrow();
        assert_eq!(s.puts, vec![(0, 3), (3, 3), (6, 3), (6, 3), (9, 1)]);
        assert_eq!(s.placed, vec![file(10)]);
    }

    #[test]
    fn a_lost_reply_to_the_last_chunk_finishes_without_sending_it_again() {
        let fake = Fake::new(false, 10, &[Fault::None, Fault::None, Fault::None, Fault::LoseReply]);
        let (r, _, seen) = run(&fake);
        r.unwrap();
        let s = fake.state.borrow();
        assert_eq!(s.puts.len(), 4);
        assert_eq!(s.placed.len(), 1);
        assert_eq!(seen.last(), Some(&10));
    }

    #[test]
    fn an_older_panel_after_a_lost_reply_still_finishes() {
        // An older panel answers the repeated chunk with 409 and its offset.
        let fake = Fake::new(true, 10, &[Fault::None, Fault::LoseReply]);
        let (r, _, _) = run(&fake);
        r.unwrap();
        assert_eq!(fake.state.borrow().placed, vec![file(10)]);
        // Its last chunk's reply lost: it has no record, so the file goes again.
        let fake = Fake::new(true, 10, &[Fault::None, Fault::None, Fault::None, Fault::LoseReply]);
        let (r, _, _) = run(&fake);
        r.unwrap();
        let s = fake.state.borrow();
        assert_eq!(s.starts, 2);
        assert_eq!(s.placed, vec![file(10), file(10)]);
    }

    #[test]
    fn a_forgotten_upload_starts_again_once() {
        let fake = Fake::new(false, 10, &[Fault::None, Fault::Forget]);
        let (r, _, seen) = run(&fake);
        r.unwrap();
        let s = fake.state.borrow();
        assert_eq!(s.starts, 2);
        assert_eq!(s.placed, vec![file(10)]);
        assert!(seen.contains(&0), "the bar goes back when it starts over");
        drop(s);
        let fake = Fake::new(false, 10, &[Fault::Forget, Fault::None, Fault::Forget]);
        let (r, _, _) = run(&fake);
        let e = format!("{:#}", r.unwrap_err());
        assert!(e.contains("forgot the upload twice"), "{e}");
    }

    #[test]
    fn a_panel_that_keeps_failing_ends_with_its_error_never_hangs() {
        let fake = Fake::new(false, 10, &[Fault::CutShort; 20]);
        let (r, pauses, _) = run(&fake);
        let e = format!("{:#}", r.unwrap_err());
        assert!(e.contains("gave up after 5 tries") && e.contains("Hopper Server answered 400"), "{e}");
        assert_eq!(fake.state.borrow().puts.len(), CHUNK_TRIES as usize);
        assert_eq!(fake.state.borrow().cancelled, vec!["1"], "the panel drops the part file");
        assert_eq!(pauses.len(), CHUNK_TRIES as usize - 1);
        // Failures that come and go don't add up: each chunk gets its tries.
        let mut f = Vec::new();
        for _ in 0..4 {
            f.extend([Fault::CutShort, Fault::CutShort, Fault::None]);
        }
        let fake = Fake::new(false, 10, &f);
        run(&fake).0.unwrap();
    }

    #[test]
    fn a_refusal_ends_at_once() {
        let fake = Fake::new(false, 10, &[Fault::None, Fault::Refused]);
        let (r, pauses, _) = run(&fake);
        assert_eq!(r.unwrap_err().to_string(), "signed out");
        assert!(pauses.is_empty());
        assert!(fake.state.borrow().placed.is_empty());
    }

    #[test]
    fn single_calls_get_a_few_tries_then_a_plain_error() {
        // Fails twice, then works.
        let mut pauses = Vec::new();
        let mut calls = 0;
        let v = with_tries(3, &mut |d| pauses.push(d), |n| {
            calls += 1;
            if n < 3 { Try::Again(anyhow::anyhow!("no reply came")) } else { Try::Done(7) }
        });
        assert_eq!((v.unwrap(), calls), (7, 3));
        assert_eq!(pauses, vec![Duration::from_secs(2), Duration::from_secs(4)]);
        // Never works: the last error, after exactly the tries.
        let mut calls = 0;
        let e = with_tries::<()>(3, &mut |_| {}, |_| {
            calls += 1;
            Try::Again(anyhow::anyhow!("Hopper Server answered 502"))
        })
        .unwrap_err();
        assert_eq!(calls, 3);
        assert_eq!(format!("{e:#}"), "gave up after 3 tries: Hopper Server answered 502");
        // A refusal ends at once.
        let mut calls = 0;
        let e = with_tries::<()>(3, &mut |_| panic!("no pause"), |_| {
            calls += 1;
            Try::Stop(anyhow::anyhow!("signed out"))
        })
        .unwrap_err();
        assert_eq!((calls, e.to_string().as_str()), (1, "signed out"));
    }

    #[test]
    fn an_empty_file_is_one_empty_chunk() {
        let fake = Fake::new(false, 0, &[]);
        let (r, _, seen) = run(&fake);
        r.unwrap();
        assert_eq!(fake.state.borrow().puts, vec![(0, 0)]);
        assert_eq!(seen, vec![0]);
    }
}

/// Against a scratch Hopper Server (never a real one): uploads a 20 MB file
/// through a proxy that loses the reply to one chunk and cuts the next one
/// short (the panel answers that with a 400), then through one that cuts
/// every chunk short. The first must arrive whole, the second must fail
/// with a plain error, and neither may hang.
/// `HOPPER_UPLOAD_LIVE="<base>|<server id>|<that server's folder>"`, the
/// token in `HOPPER_PANEL_TOKEN`.
/// `cargo test -p hopper-core --lib upload_over_a_flaky_link_live -- --ignored --nocapture`
#[cfg(test)]
mod chunk_live {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[derive(Clone, Copy, PartialEq)]
    enum Cut {
        Pass,
        /// Forward the chunk, then close on the client instead of replying.
        LoseReply,
        /// Forward half the chunk and close towards the panel; pass on its answer.
        Half,
        /// Forward the chunk, then hold the connection open without a reply
        /// (a link that died quietly: what used to wait for an hour).
        Stall,
    }

    fn read_head(r: &mut impl BufRead) -> Option<Vec<u8>> {
        let mut head = Vec::new();
        loop {
            let mut line = Vec::new();
            if r.read_until(b'\n', &mut line).ok()? == 0 {
                return None;
            }
            head.extend_from_slice(&line);
            if line == b"\r\n" {
                return Some(head);
            }
        }
    }

    fn header(head: &[u8], name: &str) -> Option<String> {
        String::from_utf8_lossy(head).lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_ascii_lowercase())
        })
    }

    /// A reply's head and body, as it came (Content-Length or chunked).
    fn read_reply(r: &mut impl BufRead) -> Option<Vec<u8>> {
        let mut out = read_head(r)?;
        if let Some(n) = header(&out, "content-length").and_then(|n| n.parse::<usize>().ok()) {
            let mut b = vec![0; n];
            r.read_exact(&mut b).ok()?;
            out.extend(b);
        } else if header(&out, "transfer-encoding").is_some_and(|t| t.contains("chunked")) {
            loop {
                let mut line = Vec::new();
                r.read_until(b'\n', &mut line).ok()?;
                out.extend_from_slice(&line);
                let n = usize::from_str_radix(String::from_utf8_lossy(&line).trim(), 16).ok()?;
                let mut b = vec![0; n + 2];
                r.read_exact(&mut b).ok()?;
                out.extend(b);
                if n == 0 {
                    break;
                }
            }
        }
        Some(out)
    }

    /// A TLS proxy (its CA pinned by the client, as at pairing) to the
    /// panel's plain HTTP at `to`; `plan(n)` decides what happens to the
    /// n-th chunk. Returns its address and CA.
    fn proxy(to: SocketAddr, plan: Arc<dyn Fn(u32) -> Cut + Send + Sync>) -> (SocketAddr, String) {
        use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
        let ca_key = KeyPair::generate().unwrap();
        let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = ca.self_signed(&ca_key).unwrap();
        let leaf_key = KeyPair::generate().unwrap();
        let leaf = CertificateParams::new(vec!["127.0.0.1".to_string()]).unwrap().signed_by(&leaf_key, &ca, &ca_key).unwrap();
        let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![leaf.der().clone()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into()),
            )
            .unwrap();
        let cfg = Arc::new(cfg);
        let ca_pem = ca.pem();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = l.local_addr().unwrap();
        let count = Arc::new(AtomicU32::new(0));
        std::thread::spawn(move || {
            for c in l.incoming().flatten() {
                let (plan, count, cfg) = (plan.clone(), count.clone(), cfg.clone());
                std::thread::spawn(move || {
                    let Ok(s) = TcpStream::connect(to) else { return };
                    let Ok(conn) = rustls::ServerConnection::new(cfg) else { return };
                    let mut cr = BufReader::new(rustls::StreamOwned::new(conn, c));
                    let mut sr = BufReader::new(s.try_clone().unwrap());
                    let mut s = s;
                    while let Some(head) = read_head(&mut cr) {
                        let len = header(&head, "content-length").and_then(|n| n.parse().ok()).unwrap_or(0);
                        let mut body = vec![0u8; len];
                        if cr.read_exact(&mut body).is_err() {
                            return;
                        }
                        let line = String::from_utf8_lossy(&head[..head.iter().position(|&b| b == b'\r').unwrap_or(0)]).to_string();
                        let counted = (line.starts_with("PUT ") && line.contains("/uploads/")) || (line.starts_with("POST ") && line.contains("/mods/upload"));
                        let cut = if counted {
                            plan(count.fetch_add(1, Ordering::SeqCst) + 1)
                        } else {
                            Cut::Pass
                        };
                        let sent = if cut == Cut::Half { len / 2 } else { len };
                        if s.write_all(&head).and_then(|_| s.write_all(&body[..sent])).is_err() {
                            return;
                        }
                        if cut == Cut::Half {
                            let _ = s.shutdown(Shutdown::Write);
                        }
                        let Some(reply) = read_reply(&mut sr) else { return };
                        let c = cr.get_mut();
                        if cut == Cut::Stall {
                            std::thread::sleep(Duration::from_secs(150));
                            return;
                        }
                        if cut == Cut::LoseReply {
                            let _ = c.sock.shutdown(Shutdown::Both);
                            return;
                        }
                        if c.write_all(&reply).and_then(|_| c.flush()).is_err() || cut == Cut::Half {
                            c.conn.send_close_notify();
                            let _ = c.flush();
                            let _ = c.sock.shutdown(Shutdown::Both);
                            return;
                        }
                    }
                });
            }
        });
        (at, ca_pem)
    }

    #[test]
    #[ignore]
    fn upload_over_a_flaky_link_live() {
        let spec = std::env::var("HOPPER_UPLOAD_LIVE").expect("HOPPER_UPLOAD_LIVE=<base>|<server id>|<server folder>");
        let token = std::env::var("HOPPER_PANEL_TOKEN").expect("HOPPER_PANEL_TOKEN");
        let mut parts = spec.split('|');
        let (base, id, folder) = (parts.next().unwrap(), parts.next().unwrap(), PathBuf::from(parts.next().unwrap()));
        let to: SocketAddr = base.trim_start_matches("http://").trim_end_matches('/').parse().expect("http://127.0.0.1:<port>");
        let size = 20 * 1024 * 1024 + 12345;
        let mut x = 0x2545_f491_u32;
        let data: Vec<u8> = (0..size)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        let local = std::env::temp_dir().join(format!("hopper-flaky-{}.jar", std::process::id()));
        std::fs::write(&local, &data).unwrap();

        // 1: the reply to chunk 2 is lost, chunk 3 is cut short.
        let (at, ca) = proxy(to, Arc::new(|n| match n {
            2 => Cut::LoseReply,
            3 => Cut::Half,
            _ => Cut::Pass,
        }));
        let c = Client::new(&format!("https://{at}"), &token, &ca);
        let seen = std::sync::Mutex::new(Vec::new());
        let t = Instant::now();
        c.upload_with(id, &local, "mods/flaky-test.jar", &|o| seen.lock().unwrap().push(o)).expect("the upload should recover");
        println!("recovered in {:.1} s; progress {:?}", t.elapsed().as_secs_f32(), seen.lock().unwrap());
        assert!(t.elapsed() < Duration::from_secs(60));
        assert_eq!(seen.lock().unwrap().last(), Some(&(size as u64)));
        let landed = std::fs::read(folder.join("mods/flaky-test.jar")).unwrap();
        assert!(landed == data, "the file arrived changed ({} bytes)", landed.len());

        // 2: every chunk cut short: a plain error, after the tries.
        let (at, ca) = proxy(to, Arc::new(|_| Cut::Half));
        let c = Client::new(&format!("https://{at}"), &token, &ca);
        let t = Instant::now();
        let e = format!("{:#}", c.upload(id, &local, "mods/flaky-test-2.jar").unwrap_err());
        println!("failed in {:.1} s: {e}", t.elapsed().as_secs_f32());
        assert!(e.contains("gave up after 5 tries") && e.contains("didn't arrive whole"), "{e}");
        assert!(t.elapsed() < Duration::from_secs(90));
        assert!(!folder.join("mods/flaky-test-2.jar").exists());
        let left = std::fs::read_dir(folder.join(".hopper-uploads")).unwrap().flatten().filter(|e| e.path().extension().is_some_and(|x| x == "part")).count();
        assert_eq!(left, 0, "the failed upload's part file is dropped");

        // 3: the reply to the first chunk never comes and the connection
        // stays open: the chunk's reply limit ends it, and it goes on.
        let (at, ca) = proxy(to, Arc::new(|n| if n == 1 { Cut::Stall } else { Cut::Pass }));
        let c = Client::new(&format!("https://{at}"), &token, &ca);
        let t = Instant::now();
        c.upload(id, &local, "mods/flaky-test-3.jar").expect("the upload should go on after the stall");
        println!("went on after a stall in {:.1} s", t.elapsed().as_secs_f32());
        assert!(t.elapsed() < Duration::from_secs(120));
        assert!(std::fs::read(folder.join("mods/flaky-test-3.jar")).unwrap() == data);

        // 4: a jar added like one dropped on the Mods tab: the reply to a
        // chunk and the reply to the call that adds it are both lost.
        let mut jar = data.clone();
        jar[..4].copy_from_slice(b"PK\x03\x04");
        let local_jar = local.with_file_name(format!("flaky-mod-{}.jar", std::process::id()));
        std::fs::write(&local_jar, &jar).unwrap();
        let (at, ca) = proxy(to, Arc::new(|n| if n == 2 || n == 4 { Cut::LoseReply } else { Cut::Pass }));
        let c = Client::new(&format!("https://{at}"), &token, &ca);
        let t = Instant::now();
        let seen = std::sync::Mutex::new(Vec::new());
        c.upload_mod_with(id, &local_jar, &|o| seen.lock().unwrap().push(o)).expect("the mod should be added");
        println!("mod added in {:.1} s; progress {:?}", t.elapsed().as_secs_f32(), seen.lock().unwrap());
        let name = local_jar.file_name().unwrap().to_string_lossy().into_owned();
        assert!(std::fs::read(folder.join("mods").join(&name)).unwrap() == jar);
        assert!(!folder.join("mods").join(format!(".{name}.upload")).exists());
        let _ = std::fs::remove_file(folder.join("mods").join(&name));
        let _ = std::fs::remove_file(&local_jar);

        let _ = std::fs::remove_file(&local);
        for f in ["flaky-test.jar", "flaky-test-3.jar"] {
            let _ = std::fs::remove_file(folder.join("mods").join(f));
        }
    }
}
