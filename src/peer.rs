//! Peer sync: Hopper on two PCs syncing an instance's folders directly,
//! ported from mod-sync's peer hub. Peers find each other by UDP broadcast
//! on the LAN, by probing Tailscale nodes, or by address. Pairing is trust
//! on first use: the initiator shows a 6-digit code, the other side accepts
//! after checking it, and both keep a shared token that every data request
//! carries. Pulled files go through backups like every other sync.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DISCOVERY_PORT: u16 = 42424;
pub const HTTP_PORT: u16 = 42425;
const PROTO: u32 = 1;
const STALE: Duration = Duration::from_secs(15);

/// Folders a peer may read and write, under an instance.
pub const FOLDERS: &[&str] = &[
    "mods",
    "config",
    "defaultconfigs",
    "kubejs",
    "resourcepacks",
    "shaderpacks",
];

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn data_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "Hopper")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(std::env::temp_dir)
}

/// Fills `buf` from the operating system's secure random generator.
#[cfg(windows)]
fn os_random(buf: &mut [u8]) -> std::io::Result<()> {
    use windows::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };
    // SAFETY: the buffer is a valid, writable slice; no algorithm handle is
    // needed with BCRYPT_USE_SYSTEM_PREFERRED_RNG.
    let status = unsafe { BCryptGenRandom(None, buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status.is_ok() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "BCryptGenRandom failed: {:#x}",
            status.0
        )))
    }
}

#[cfg(not(windows))]
fn os_random(buf: &mut [u8]) -> std::io::Result<()> {
    std::fs::File::open("/dev/urandom")?.read_exact(buf)
}

/// `bytes` random bytes from the OS, as lowercase hex.
fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    // Tokens guard the peer API; never fall back to something guessable.
    os_random(&mut buf).expect("the system random number generator failed");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn code() -> String {
    // 64 random bits keep the modulo bias negligible.
    let n = u64::from_str_radix(&random_hex(8), 16).unwrap_or(0) % 1_000_000;
    format!("{n:06}")
}

/// Compares secrets without stopping at the first difference.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y));
    std::hint::black_box(diff) == 0
}

fn self_id() -> String {
    let p = data_dir().join("peer-id.txt");
    if let Ok(id) = std::fs::read_to_string(&p)
        && id.trim().len() >= 16
    {
        return id.trim().to_string();
    }
    let id = random_hex(16);
    let _ = std::fs::create_dir_all(data_dir());
    let _ = std::fs::write(p, &id);
    id
}

pub fn self_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "This PC".into())
}

// ── trusted peers ───────────────────────────────────────────────────

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Trusted {
    pub peer_id: String,
    pub name: String,
    pub token: String,
    /// Last known "host:port" or http(s) URL.
    pub addr: String,
    pub added: u64,
}

/// Tests point this elsewhere so they never touch the real peer list.
static TRUSTED_FILE: Mutex<Option<PathBuf>> = Mutex::new(None);

fn trusted_path() -> PathBuf {
    TRUSTED_FILE
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| data_dir().join("peers.json"))
}

pub fn trusted() -> Vec<Trusted> {
    std::fs::read(trusted_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_trusted(list: &[Trusted]) {
    if let Some(d) = trusted_path().parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(b) = serde_json::to_vec_pretty(list) {
        let _ = std::fs::write(trusted_path(), b);
    }
}

fn upsert_trusted(t: Trusted) {
    let mut list = trusted();
    list.retain(|p| p.peer_id != t.peer_id);
    list.push(t);
    save_trusted(&list);
}

pub fn forget(peer_id: &str) {
    let mut list = trusted();
    list.retain(|p| p.peer_id != peer_id);
    save_trusted(&list);
}

fn by_token(token: &str) -> Option<Trusted> {
    if token.len() < 32 {
        return None;
    }
    // Check every peer, so timing doesn't tell which one (or how much) matched.
    let mut found = None;
    for p in trusted() {
        if ct_eq(p.token.as_bytes(), token.as_bytes()) && found.is_none() {
            found = Some(p);
        }
    }
    found
}

fn touch_addr(peer_id: &str, addr: &str) {
    let mut list = trusted();
    if let Some(p) = list.iter_mut().find(|p| p.peer_id == peer_id)
        && p.addr != addr
    {
        p.addr = addr.to_string();
        save_trusted(&list);
    }
}

// ── what this PC shares ─────────────────────────────────────────────

/// An instance as peers see it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SharedInst {
    /// The instance folder's name; how peers address it.
    pub key: String,
    pub name: String,
    pub pack_id: u64,
    pub pack_version: String,
    #[serde(skip)]
    pub root: PathBuf,
}

impl SharedInst {
    pub fn of(i: &crate::instances::Instance) -> Self {
        Self {
            key: i
                .root
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            name: i.name.clone(),
            pack_id: i.pack.as_ref().map_or(0, |p| p.project_id),
            pack_version: i.pack.as_ref().map_or(String::new(), |p| p.version.clone()),
            root: i.root.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileMeta {
    pub rel: String,
    pub size: u64,
    pub mtime: u64,
    pub sha1: String,
    /// Mod name for jars the instance knows.
    pub name: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Manifest {
    pub folder: String,
    pub files: Vec<FileMeta>,
}

static HASHES: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn hash_cache_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "Hopper").map(|d| d.cache_dir().join("peerhash.json"))
}

fn file_sha1(path: &Path, size: u64, mtime: u64) -> Option<String> {
    let key = format!("{}|{size}|{mtime}", path.display());
    {
        let mut g = HASHES.lock().unwrap();
        let m = g.get_or_insert_with(|| {
            hash_cache_path()
                .and_then(|p| std::fs::read(p).ok())
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default()
        });
        if let Some(h) = m.get(&key) {
            return Some(h.clone());
        }
    }
    let mut f = std::fs::File::open(path).ok()?;
    let mut h = sha1_smol::Sha1::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    let hex = h.hexdigest();
    HASHES
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(key, hex.clone());
    Some(hex)
}

fn save_hashes() {
    if let (Some(p), Some(m)) = (hash_cache_path(), HASHES.lock().unwrap().as_ref())
        && let Ok(b) = serde_json::to_vec(m)
    {
        let _ = std::fs::write(p, b);
    }
}

/// Every file in `folder` of an instance, with content hashes.
pub fn manifest(inst: &SharedInst, folder: &str, names: &HashMap<String, String>) -> Manifest {
    let root = inst.root.join(folder);
    let mut files = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                stack.push(p);
                continue;
            }
            let Ok(rel) = p.strip_prefix(&root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if crate::configdiff::is_noise(&rel) {
                continue;
            }
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            let Some(sha1) = file_sha1(&p, md.len(), mtime) else {
                continue;
            };
            files.push(FileMeta {
                name: names.get(&rel.to_lowercase()).cloned().unwrap_or_default(),
                rel,
                size: md.len(),
                mtime,
                sha1,
            });
        }
    }
    save_hashes();
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    Manifest {
        folder: folder.to_string(),
        files,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Differ,
    OnlyLocal,
    OnlyRemote,
    Same,
}

#[derive(Clone, Debug)]
pub struct DiffEntry {
    pub rel: String,
    pub status: Status,
    pub local: Option<FileMeta>,
    pub remote: Option<FileMeta>,
}

pub fn diff(local: &Manifest, remote: &Manifest) -> Vec<DiffEntry> {
    let l: HashMap<&str, &FileMeta> = local.files.iter().map(|f| (f.rel.as_str(), f)).collect();
    let r: HashMap<&str, &FileMeta> = remote.files.iter().map(|f| (f.rel.as_str(), f)).collect();
    let mut out: Vec<DiffEntry> = Vec::new();
    for (rel, lf) in &l {
        let rf = r.get(rel);
        out.push(DiffEntry {
            rel: rel.to_string(),
            status: match rf {
                None => Status::OnlyLocal,
                Some(rf) if rf.sha1 == lf.sha1 => Status::Same,
                Some(_) => Status::Differ,
            },
            local: Some((*lf).clone()),
            remote: rf.map(|f| (*f).clone()),
        });
    }
    for (rel, rf) in &r {
        if !l.contains_key(rel) {
            out.push(DiffEntry {
                rel: rel.to_string(),
                status: Status::OnlyRemote,
                local: None,
                remote: Some((*rf).clone()),
            });
        }
    }
    let order = |s: Status| s as u8;
    out.sort_by(|a, b| {
        order(a.status)
            .cmp(&order(b.status))
            .then_with(|| a.rel.cmp(&b.rel))
    });
    out
}

// ── the hub ─────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct PeerInfo {
    pub peer_id: String,
    pub name: String,
    pub addr: String,
    pub last_seen: u64,
    pub trusted: bool,
    /// "lan" | "tailscale" | "address" | "" (trusted, not seen)
    pub source: String,
}

#[derive(Clone, Debug)]
pub struct PairRequest {
    pub peer_id: String,
    pub name: String,
    pub code: String,
    pub addr: String,
    token: String,
    /// Who sent it and when, for the rate limit.
    from: Option<IpAddr>,
    at: Instant,
}

/// Pair requests one address may send per [`PAIR_WINDOW`].
const PAIR_PER_WINDOW: usize = 3;
const PAIR_WINDOW: Duration = Duration::from_secs(60);
/// How long a pending request from one address blocks requests from others.
const PAIR_PENDING_HOLD: Duration = Duration::from_secs(120);

#[derive(Serialize, Deserialize)]
struct Announce {
    proto: u32,
    peer_id: String,
    name: String,
    http_port: u16,
}

#[derive(Default)]
struct State {
    seen: HashMap<String, PeerInfo>,
    pending: Option<PairRequest>,
    shared: Vec<SharedInst>,
    /// file name (lowercase) -> mod name, per instance key.
    names: HashMap<String, HashMap<String, String>>,
    /// Recent `/pair/request`s per address.
    pair_hits: HashMap<Option<IpAddr>, Vec<Instant>>,
}

impl State {
    /// Whether a pair request from `ip` may become the pending one: at most
    /// [`PAIR_PER_WINDOW`] per address a minute, and one pending request at a
    /// time (an address may replace its own, others wait until it is
    /// answered or [`PAIR_PENDING_HOLD`] old). Err is (HTTP status, reason).
    fn admit_pair(&mut self, ip: Option<IpAddr>, now: Instant) -> Result<(), (u16, &'static str)> {
        self.pair_hits.retain(|_, hits| {
            hits.retain(|t| now.saturating_duration_since(*t) < PAIR_WINDOW);
            !hits.is_empty()
        });
        let hits = self.pair_hits.entry(ip).or_default();
        if hits.len() >= PAIR_PER_WINDOW {
            return Err((429, "too many pair requests; wait a minute"));
        }
        hits.push(now);
        if let Some(p) = &self.pending
            && p.from != ip
            && now.saturating_duration_since(p.at) < PAIR_PENDING_HOLD
        {
            return Err((409, "another pair request is waiting for an answer"));
        }
        Ok(())
    }
}

pub struct Hub {
    pub id: String,
    pub port: u16,
    state: Mutex<State>,
    stop: AtomicBool,
    waker: crate::waker::Waker,
    /// UDP discovery could not start (port in use); address and Tailscale
    /// still work.
    pub discovery_error: Option<String>,
}

impl Hub {
    /// Starts the transport, discovery and Tailscale probing.
    pub fn start(ctx: crate::waker::Waker) -> anyhow::Result<Arc<Hub>> {
        Self::start_as(ctx, self_id(), false)
    }

    /// `local_only` (tests) listens on 127.0.0.1 without LAN discovery.
    fn start_as(
        ctx: crate::waker::Waker,
        id: String,
        local_only: bool,
    ) -> anyhow::Result<Arc<Hub>> {
        let host = if local_only { "127.0.0.1" } else { "0.0.0.0" };
        let server = tiny_http::Server::http((host, if local_only { 0 } else { HTTP_PORT }))
            .or_else(|_| tiny_http::Server::http((host, 0)))
            .map_err(|e| anyhow::anyhow!("can't open the peer port: {e}"))?;
        let port = server.server_addr().to_ip().map_or(HTTP_PORT, |a| a.port());
        let udp = if local_only {
            Err(std::io::Error::other("local only"))
        } else {
            UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT))
        };
        let hub = Arc::new(Hub {
            id,
            port,
            state: Mutex::new(State::default()),
            stop: AtomicBool::new(false),
            waker: ctx,
            discovery_error: udp
                .as_ref()
                .err()
                .map(|e| format!("LAN discovery is off: {e}")),
        });
        {
            let hub = hub.clone();
            std::thread::Builder::new()
                .name("peer-http".into())
                .spawn(move || {
                    for req in server.incoming_requests() {
                        if hub.stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let hub = hub.clone();
                        std::thread::spawn(move || hub.handle(req));
                    }
                })?;
        }
        if let Ok(sock) = udp {
            let _ = sock.set_broadcast(true);
            let _ = sock.set_read_timeout(Some(Duration::from_secs(1)));
            let hub2 = hub.clone();
            let send = sock.try_clone()?;
            std::thread::Builder::new()
                .name("peer-announce".into())
                .spawn(move || {
                    while !hub2.stop.load(Ordering::Relaxed) {
                        let msg = serde_json::to_vec(&Announce {
                            proto: PROTO,
                            peer_id: hub2.id.clone(),
                            name: self_name(),
                            http_port: hub2.port,
                        })
                        .unwrap_or_default();
                        let _ = send.send_to(&msg, ("255.255.255.255", DISCOVERY_PORT));
                        hub2.prune();
                        std::thread::sleep(Duration::from_secs(3));
                    }
                })?;
            let hub3 = hub.clone();
            std::thread::Builder::new()
                .name("peer-listen".into())
                .spawn(move || {
                    let mut buf = [0u8; 2048];
                    while !hub3.stop.load(Ordering::Relaxed) {
                        let Ok((n, from)) = sock.recv_from(&mut buf) else {
                            continue;
                        };
                        let Ok(a) = serde_json::from_slice::<Announce>(&buf[..n]) else {
                            continue;
                        };
                        if a.proto != PROTO || a.peer_id == hub3.id {
                            continue;
                        }
                        hub3.seen(PeerInfo {
                            peer_id: a.peer_id,
                            name: a.name,
                            addr: format!("{}:{}", from.ip(), a.http_port),
                            last_seen: now(),
                            trusted: false,
                            source: "lan".into(),
                        });
                    }
                })?;
        }
        {
            let hub = hub.clone();
            std::thread::Builder::new()
                .name("peer-probe".into())
                .spawn(move || {
                    while !hub.stop.load(Ordering::Relaxed) {
                        hub.probe_remote();
                        for _ in 0..20 {
                            if hub.stop.load(Ordering::Relaxed) {
                                return;
                            }
                            std::thread::sleep(Duration::from_secs(1));
                        }
                    }
                })?;
        }
        Ok(hub)
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wake the HTTP loop so it sees the flag.
        let _ = ureq::get(&format!("http://127.0.0.1:{}/hello", self.port)).call();
    }

    pub fn set_shared(
        &self,
        list: Vec<SharedInst>,
        names: HashMap<String, HashMap<String, String>>,
    ) {
        let mut s = self.state.lock().unwrap();
        s.shared = list;
        s.names = names;
    }

    fn seen(&self, mut p: PeerInfo) {
        p.trusted = trusted().iter().any(|t| t.peer_id == p.peer_id);
        if p.trusted {
            touch_addr(&p.peer_id, &p.addr);
        }
        let mut s = self.state.lock().unwrap();
        let changed = s
            .seen
            .get(&p.peer_id)
            .is_none_or(|o| o.addr != p.addr || o.name != p.name);
        if let Some(o) = s.seen.get(&p.peer_id)
            && o.source == "lan"
        {
            p.source = "lan".into();
        }
        s.seen.insert(p.peer_id.clone(), p);
        drop(s);
        if changed {
            (self.waker)();
        }
    }

    fn prune(&self) {
        let cutoff = now().saturating_sub(STALE.as_secs() + 30);
        let mut s = self.state.lock().unwrap();
        let before = s.seen.len();
        s.seen.retain(|_, p| p.last_seen >= cutoff);
        if s.seen.len() != before {
            (self.waker)();
        }
    }

    /// Seen peers plus trusted ones not seen right now.
    pub fn peers(&self) -> Vec<PeerInfo> {
        let s = self.state.lock().unwrap();
        let mut out: Vec<PeerInfo> = s.seen.values().cloned().collect();
        for t in trusted() {
            match out.iter_mut().find(|p| p.peer_id == t.peer_id) {
                Some(p) => p.trusted = true,
                None => out.push(PeerInfo {
                    peer_id: t.peer_id,
                    name: t.name,
                    addr: t.addr,
                    trusted: true,
                    ..Default::default()
                }),
            }
        }
        out.sort_by(|a, b| b.trusted.cmp(&a.trusted).then_with(|| a.name.cmp(&b.name)));
        out
    }

    pub fn pending(&self) -> Option<PairRequest> {
        self.state.lock().unwrap().pending.clone()
    }

    /// The local user accepted or declined the pending request.
    pub fn answer(&self, accept: bool) {
        let Some(req) = self.state.lock().unwrap().pending.take() else {
            return;
        };
        if accept {
            upsert_trusted(Trusted {
                peer_id: req.peer_id,
                name: req.name,
                token: req.token,
                addr: req.addr,
                added: now(),
            });
        }
        (self.waker)();
    }

    fn addr_of(&self, peer_id: &str) -> Option<String> {
        if let Some(p) = self.state.lock().unwrap().seen.get(peer_id)
            && !p.addr.is_empty()
        {
            return Some(p.addr.clone());
        }
        trusted()
            .into_iter()
            .find(|t| t.peer_id == peer_id)
            .map(|t| t.addr)
            .filter(|a| !a.is_empty())
    }

    /// Checks `addr` for a Hopper peer and lists it.
    pub fn add_address(&self, addr: &str) -> anyhow::Result<PeerInfo> {
        let mut p = probe(addr)?;
        anyhow::ensure!(p.peer_id != self.id, "that address is this PC");
        p.source = "address".into();
        self.seen(p.clone());
        Ok(p)
    }

    fn probe_remote(&self) {
        let mut targets: Vec<(String, String, &str)> = tailscale_peers()
            .into_iter()
            .map(|(host, name)| (host, name, "tailscale"))
            .collect();
        for t in trusted() {
            if !t.addr.is_empty() {
                targets.push((t.addr, t.name, "address"));
            }
        }
        for (addr, name, source) in targets {
            if let Ok(mut p) = probe(&addr)
                && p.peer_id != self.id
            {
                if p.name.is_empty() {
                    p.name = name;
                }
                p.source = source.into();
                self.seen(p);
            }
        }
    }

    /// Starts pairing with a peer; returns the code to show the user.
    pub fn pair(&self, peer_id: &str) -> anyhow::Result<String> {
        let addr = self
            .addr_of(peer_id)
            .ok_or_else(|| anyhow::anyhow!("peer not reachable"))?;
        let (code, token) = (code(), random_hex(32));
        let mut r = ureq::post(&format!("{}/pair/request", base_url(&addr)))
            .config()
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .send_json(serde_json::json!({
                "peer_id": self.id, "name": self_name(), "code": code, "token": token, "port": self.port,
            }))?;
        let v: serde_json::Value = r.body_mut().read_json().unwrap_or_default();
        let name = v["name"].as_str().unwrap_or("Peer").to_string();
        upsert_trusted(Trusted {
            peer_id: peer_id.into(),
            name,
            token,
            addr,
            added: now(),
        });
        (self.waker)();
        Ok(code)
    }

    // ── serving ─────────────────────────────────────────────────────

    fn handle(&self, mut req: tiny_http::Request) {
        let url = req.url().to_string();
        let (path, query) = url.split_once('?').unwrap_or((&url, ""));
        let q: HashMap<String, String> = query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (decode(k), decode(v)))
            .collect();
        let remote = req.remote_addr().map(|a| a.ip());
        let json = |v: serde_json::Value| {
            tiny_http::Response::from_string(v.to_string()).with_header(
                "Content-Type: application/json"
                    .parse::<tiny_http::Header>()
                    .unwrap(),
            )
        };
        let status =
            |code: u16, msg: &str| tiny_http::Response::from_string(msg).with_status_code(code);
        match path {
            "/hello" => {
                let _ = req.respond(json(serde_json::json!({
                    "peer_id": self.id, "name": self_name(), "proto": PROTO, "http_port": self.port,
                })));
            }
            "/pair/request" if *req.method() == tiny_http::Method::Post => {
                let admitted = self
                    .state
                    .lock()
                    .unwrap()
                    .admit_pair(remote, Instant::now());
                if let Err((code, msg)) = admitted {
                    let _ = req.respond(status(code, msg));
                    return;
                }
                let mut body = String::new();
                let _ = req.as_reader().take(1 << 16).read_to_string(&mut body);
                let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                let (Some(id), Some(token)) = (v["peer_id"].as_str(), v["token"].as_str()) else {
                    let _ = req.respond(status(400, "bad request"));
                    return;
                };
                let port = v["port"].as_u64().unwrap_or(HTTP_PORT as u64);
                let addr = remote.map_or(String::new(), |ip| format!("{ip}:{port}"));
                self.state.lock().unwrap().pending = Some(PairRequest {
                    peer_id: id.into(),
                    name: v["name"].as_str().unwrap_or("Unknown PC").into(),
                    code: v["code"].as_str().unwrap_or("").into(),
                    addr,
                    token: token.into(),
                    from: remote,
                    at: Instant::now(),
                });
                (self.waker)();
                let _ = req.respond(json(serde_json::json!({ "name": self_name(), "peer_id": self.id, "status": "pending" })));
            }
            _ => {
                let token = req
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Authorization"))
                    .map(|h| h.value.as_str().trim_start_matches("Bearer ").to_string())
                    .unwrap_or_default();
                let Some(peer) = by_token(&token) else {
                    let _ = req.respond(status(401, "not paired"));
                    return;
                };
                if let Some(ip) = remote {
                    let port = trusted()
                        .iter()
                        .find(|t| t.peer_id == peer.peer_id)
                        .and_then(|t| t.addr.rsplit(':').next()?.parse::<u16>().ok())
                        .unwrap_or(HTTP_PORT);
                    if !peer.addr.starts_with("http") {
                        touch_addr(&peer.peer_id, &SocketAddr::new(ip, port).to_string());
                    }
                }
                self.handle_authed(req, path, &q, &peer, json, status);
            }
        }
    }

    fn handle_authed(
        &self,
        mut req: tiny_http::Request,
        path: &str,
        q: &HashMap<String, String>,
        peer: &Trusted,
        json: impl Fn(serde_json::Value) -> tiny_http::Response<std::io::Cursor<Vec<u8>>>,
        status: impl Fn(u16, &str) -> tiny_http::Response<std::io::Cursor<Vec<u8>>>,
    ) {
        let (shared, names) = {
            let s = self.state.lock().unwrap();
            (s.shared.clone(), s.names.clone())
        };
        if path == "/instances" {
            let _ = req.respond(json(serde_json::to_value(&shared).unwrap_or_default()));
            return;
        }
        let inst = q
            .get("inst")
            .and_then(|k| shared.iter().find(|s| &s.key == k));
        let folder = q.get("folder").map(String::as_str).unwrap_or("");
        let (Some(inst), true) = (inst, FOLDERS.contains(&folder)) else {
            let _ = req.respond(status(404, "no such instance or folder"));
            return;
        };
        match path {
            "/manifest" => {
                let m = manifest(
                    inst,
                    folder,
                    names.get(&inst.key).unwrap_or(&HashMap::new()),
                );
                let _ = req.respond(json(serde_json::to_value(m).unwrap_or_default()));
            }
            "/file" => {
                let rel = q.get("rel").cloned().unwrap_or_default();
                if !crate::configdiff::safe_rel(&rel) {
                    let _ = req.respond(status(400, "bad path"));
                    return;
                }
                let full = inst.root.join(folder).join(&rel);
                if *req.method() == tiny_http::Method::Get {
                    match std::fs::File::open(&full) {
                        Ok(f) => {
                            let _ = req.respond(tiny_http::Response::from_file(f));
                        }
                        Err(_) => {
                            let _ = req.respond(status(404, "not found"));
                        }
                    }
                } else if *req.method() == tiny_http::Method::Put {
                    let r = receive(req.as_reader(), &inst.root, &full);
                    crate::history::record(
                        &inst.root,
                        &format!("peer {}", peer.name),
                        &inst.root,
                        std::slice::from_ref(&r),
                    );
                    (self.waker)();
                    let _ = match r.error {
                        None => req.respond(json(serde_json::json!({"status": "ok"}))),
                        Some(e) => req.respond(status(500, &e)),
                    };
                } else {
                    let _ = req.respond(status(405, "GET or PUT"));
                }
            }
            _ => {
                let _ = req.respond(status(404, "unknown route"));
            }
        }
    }

    // ── asking a paired peer ────────────────────────────────────────

    fn request(
        &self,
        peer_id: &str,
        path: &str,
        query: &[(&str, &str)],
    ) -> anyhow::Result<(Trusted, String)> {
        let t = trusted()
            .into_iter()
            .find(|t| t.peer_id == peer_id)
            .ok_or_else(|| anyhow::anyhow!("not paired with this peer"))?;
        let addr = self
            .addr_of(peer_id)
            .ok_or_else(|| anyhow::anyhow!("peer is offline"))?;
        let mut url = format!("{}{path}", base_url(&addr));
        if !query.is_empty() {
            url.push('?');
            url += &query
                .iter()
                .map(|(k, v)| format!("{k}={}", encode(v)))
                .collect::<Vec<_>>()
                .join("&");
        }
        Ok((t, url))
    }

    pub fn instances(&self, peer_id: &str) -> anyhow::Result<Vec<SharedInst>> {
        let (t, url) = self.request(peer_id, "/instances", &[])?;
        Ok(agent()
            .get(&url)
            .header("Authorization", format!("Bearer {}", t.token))
            .call()?
            .body_mut()
            .read_json()?)
    }

    pub fn remote_manifest(
        &self,
        peer_id: &str,
        inst: &str,
        folder: &str,
    ) -> anyhow::Result<Manifest> {
        let (t, url) = self.request(peer_id, "/manifest", &[("inst", inst), ("folder", folder)])?;
        Ok(agent()
            .get(&url)
            .header("Authorization", format!("Bearer {}", t.token))
            .call()?
            .body_mut()
            .with_config()
            .limit(256 << 20)
            .read_json()?)
    }

    /// Downloads files from the peer into `local_root/folder`.
    pub fn pull(
        &self,
        peer_id: &str,
        inst: &str,
        folder: &str,
        local_root: &Path,
        rels: &[String],
    ) -> Vec<crate::model::SyncResult> {
        rels.iter()
            .map(|rel| {
                let dst = local_root.join(folder).join(rel);
                let fail = |e: String| crate::model::SyncResult {
                    op: crate::model::SyncOp::Copy {
                        src: PathBuf::from(format!("peer:{rel}")),
                        dst: dst.clone(),
                    },
                    ok: false,
                    error: Some(e),
                    backup: None,
                };
                if !crate::configdiff::safe_rel(rel) {
                    return fail("bad path".into());
                }
                let r = self
                    .request(
                        peer_id,
                        "/file",
                        &[("inst", inst), ("folder", folder), ("rel", rel)],
                    )
                    .and_then(|(t, url)| {
                        Ok(agent()
                            .get(&url)
                            .header("Authorization", format!("Bearer {}", t.token))
                            .call()?)
                    });
                match r {
                    Ok(mut resp) => receive(resp.body_mut().as_reader(), local_root, &dst),
                    Err(e) => fail(format!("{e:#}")),
                }
            })
            .collect()
    }

    /// Uploads files from `local_root/folder` to the peer, which backs up
    /// what it replaces.
    pub fn push(
        &self,
        peer_id: &str,
        inst: &str,
        folder: &str,
        local_root: &Path,
        rels: &[String],
    ) -> Vec<String> {
        rels.iter()
            .filter_map(|rel| {
                let src = local_root.join(folder).join(rel);
                let r = self
                    .request(
                        peer_id,
                        "/file",
                        &[("inst", inst), ("folder", folder), ("rel", rel)],
                    )
                    .and_then(|(t, url)| {
                        let body = std::fs::read(&src)?;
                        agent()
                            .put(&url)
                            .header("Authorization", format!("Bearer {}", t.token))
                            .send(&body[..])?;
                        Ok(())
                    });
                r.err().map(|e| format!("{rel}: {e:#}"))
            })
            .collect()
    }
}

/// Writes an incoming file through a temp file, backing up what it replaces.
fn receive(mut body: impl Read, backup_root: &Path, dst: &Path) -> crate::model::SyncResult {
    let op = crate::model::SyncOp::Copy {
        src: PathBuf::from("peer"),
        dst: dst.to_path_buf(),
    };
    let r = (|| -> anyhow::Result<Option<PathBuf>> {
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut tmp = dst.as_os_str().to_owned();
        tmp.push(".hopper-tmp");
        let tmp = PathBuf::from(tmp);
        {
            let mut f = std::fs::File::create(&tmp)?;
            std::io::copy(&mut body.by_ref().take(2 << 30), &mut f)?;
        }
        let backup = match crate::sync::backup_into(backup_root, dst) {
            Ok(b) => b,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        };
        if let Err(e) = std::fs::rename(&tmp, dst) {
            let _ = std::fs::remove_file(&tmp);
            if let Some(b) = &backup {
                let _ = std::fs::rename(b, dst);
            }
            return Err(e.into());
        }
        Ok(backup)
    })();
    match r {
        Ok(backup) => crate::model::SyncResult {
            op,
            ok: true,
            error: None,
            backup,
        },
        Err(e) => crate::model::SyncResult {
            op,
            ok: false,
            error: Some(format!("{e:#}")),
            backup: None,
        },
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(8)))
        .timeout_global(Some(Duration::from_secs(600)))
        .build()
        .into()
}

/// "host", "host:port" or a full http(s) URL into a base URL.
pub fn base_url(addr: &str) -> String {
    let a = addr.trim().trim_end_matches('/');
    if a.starts_with("http://") || a.starts_with("https://") {
        return a.to_string();
    }
    if a.parse::<SocketAddr>().is_ok()
        || a.rsplit_once(':')
            .is_some_and(|(_, p)| p.parse::<u16>().is_ok())
    {
        format!("http://{a}")
    } else {
        format!("http://{a}:{HTTP_PORT}")
    }
}

/// Asks `addr` who it is.
pub fn probe(addr: &str) -> anyhow::Result<PeerInfo> {
    let v: serde_json::Value = ureq::get(&format!("{}/hello", base_url(addr)))
        .config()
        .timeout_global(Some(Duration::from_secs(4)))
        .build()
        .call()?
        .body_mut()
        .read_json()?;
    let id = v["peer_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("not a Hopper peer"))?;
    let stored = if addr.starts_with("http") {
        addr.trim_end_matches('/').to_string()
    } else {
        base_url(addr).trim_start_matches("http://").to_string()
    };
    Ok(PeerInfo {
        peer_id: id.into(),
        name: v["name"].as_str().unwrap_or("").into(),
        addr: stored,
        last_seen: now(),
        trusted: false,
        source: String::new(),
    })
}

/// Online Tailscale nodes: (100.x address, host name).
pub fn tailscale_peers() -> Vec<(String, String)> {
    let full = r"C:\Program Files\Tailscale\tailscale.exe";
    let bin = if Path::new(full).exists() {
        full
    } else {
        "tailscale"
    };
    let mut cmd = std::process::Command::new(bin);
    cmd.args(["status", "--json"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let Ok(out) = cmd.output() else {
        return Vec::new();
    };
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    v["Peer"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
        .filter(|p| p["Online"].as_bool() == Some(true))
        .filter_map(|p| {
            let ip = p["TailscaleIPs"]
                .as_array()?
                .iter()
                .filter_map(|i| i.as_str())
                .find(|i| i.parse::<IpAddr>().is_ok_and(|a| a.is_ipv4()))?;
            let name = p["HostName"].as_str().unwrap_or("").to_string();
            Some((ip.to_string(), name))
        })
        .collect()
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(v) => {
                    out.push(v);
                    i += 3;
                    continue;
                }
                Err(_) => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_round_trip() {
        let s = "config/My Mod/a+b&c=d.toml";
        assert_eq!(decode(&encode(s)), s);
        assert_eq!(base_url("100.64.0.2"), "http://100.64.0.2:42425");
        assert_eq!(base_url("192.168.1.5:5000"), "http://192.168.1.5:5000");
        assert_eq!(
            base_url("https://x.trycloudflare.com/"),
            "https://x.trycloudflare.com"
        );
        assert_eq!(code().len(), 6);
    }

    #[test]
    fn tokens_are_random_hex() {
        let (a, b) = (random_hex(32), random_hex(32));
        assert_eq!(a.len(), 64);
        assert!(
            a.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        );
        assert_ne!(a, b);
        assert_eq!(random_hex(16).len(), 32);
        let c = code();
        assert!(c.len() == 6 && c.bytes().all(|b| b.is_ascii_digit()));
        assert!(ct_eq(a.as_bytes(), a.clone().as_bytes()));
        assert!(!ct_eq(a.as_bytes(), b.as_bytes()));
        assert!(!ct_eq(a.as_bytes(), &a.as_bytes()[..63]));
    }

    #[test]
    fn pair_requests_are_rate_limited() {
        let mut s = State::default();
        let (x, y): (IpAddr, IpAddr) = ("10.0.0.5".parse().unwrap(), "10.0.0.6".parse().unwrap());
        let t0 = Instant::now();
        let pending = |ip: IpAddr, at: Instant| PairRequest {
            peer_id: String::new(),
            name: String::new(),
            code: String::new(),
            addr: String::new(),
            token: String::new(),
            from: Some(ip),
            at,
        };
        // Three a minute per address; the fourth is refused.
        for _ in 0..3 {
            assert!(s.admit_pair(Some(x), t0).is_ok());
        }
        assert_eq!(s.admit_pair(Some(x), t0).unwrap_err().0, 429);
        // Another address isn't affected by x's count.
        assert!(s.admit_pair(Some(y), t0).is_ok());
        // A minute later x may ask again.
        let t1 = t0 + PAIR_WINDOW + Duration::from_secs(1);
        assert!(s.admit_pair(Some(x), t1).is_ok());

        // While x's request waits, y can't replace it; x can.
        s.pending = Some(pending(x, t1));
        let t2 = t1 + Duration::from_secs(5);
        assert_eq!(s.admit_pair(Some(y), t2).unwrap_err().0, 409);
        assert!(s.admit_pair(Some(x), t2).is_ok());
        // Once it's old, y may.
        let t3 = t1 + PAIR_PENDING_HOLD + Duration::from_secs(1);
        assert!(s.admit_pair(Some(y), t3).is_ok());
    }

    #[test]
    fn diffs_manifests() {
        let f = |rel: &str, sha: &str| FileMeta {
            rel: rel.into(),
            sha1: sha.into(),
            ..Default::default()
        };
        let l = Manifest {
            folder: "mods".into(),
            files: vec![f("a.jar", "1"), f("b.jar", "2"), f("c.jar", "3")],
        };
        let r = Manifest {
            folder: "mods".into(),
            files: vec![f("a.jar", "1"), f("b.jar", "9"), f("d.jar", "4")],
        };
        let d: Vec<(String, Status)> = diff(&l, &r)
            .into_iter()
            .map(|e| (e.rel, e.status))
            .collect();
        assert_eq!(
            d,
            [
                ("b.jar".to_string(), Status::Differ),
                ("c.jar".to_string(), Status::OnlyLocal),
                ("d.jar".to_string(), Status::OnlyRemote),
                ("a.jar".to_string(), Status::Same),
            ]
        );
    }

    #[test]
    fn two_hubs_pair_and_sync_over_localhost() {
        let root = std::env::temp_dir().join(format!("hopper-peer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        *TRUSTED_FILE.lock().unwrap() = Some(root.join("peers.json"));
        let (ra, rb) = (root.join("A/TNP"), root.join("B/TNP"));
        for (r, body) in [(&ra, "a-version"), (&rb, "b-version")] {
            std::fs::create_dir_all(r.join("mods")).unwrap();
            std::fs::write(r.join("mods/shared.jar"), body).unwrap();
        }
        std::fs::write(ra.join("mods/only-a.jar"), "a").unwrap();
        std::fs::write(rb.join("mods/only-b.jar"), "b").unwrap();
        let share = |r: &Path| SharedInst {
            key: "TNP".into(),
            name: "TNP".into(),
            root: r.to_path_buf(),
            ..Default::default()
        };

        let ctx = crate::waker::noop();
        let a = Hub::start_as(ctx.clone(), "a".repeat(32), true).unwrap();
        let b = Hub::start_as(ctx, "b".repeat(32), true).unwrap();
        a.set_shared(vec![share(&ra)], HashMap::new());
        b.set_shared(vec![share(&rb)], HashMap::new());

        let seen = a.add_address(&format!("127.0.0.1:{}", b.port)).unwrap();
        assert_eq!(seen.peer_id, b.id);
        // Unpaired: data routes refuse.
        assert!(a.instances(&b.id).is_err());
        let code = a.pair(&b.id).unwrap();
        assert_eq!(b.pending().unwrap().code, code);
        b.answer(true);

        let insts = a.instances(&b.id).unwrap();
        assert_eq!(insts[0].key, "TNP");
        let remote = a.remote_manifest(&b.id, "TNP", "mods").unwrap();
        let local = manifest(&share(&ra), "mods", &HashMap::new());
        let d: Vec<(String, Status)> = diff(&local, &remote)
            .into_iter()
            .map(|e| (e.rel, e.status))
            .collect();
        assert_eq!(
            d,
            [
                ("shared.jar".to_string(), Status::Differ),
                ("only-a.jar".to_string(), Status::OnlyLocal),
                ("only-b.jar".to_string(), Status::OnlyRemote),
            ]
        );

        let pulled = a.pull(
            &b.id,
            "TNP",
            "mods",
            &ra,
            &["only-b.jar".into(), "shared.jar".into()],
        );
        assert!(pulled.iter().all(|r| r.ok), "{pulled:?}");
        assert_eq!(
            std::fs::read_to_string(ra.join("mods/shared.jar")).unwrap(),
            "b-version"
        );
        assert_eq!(
            std::fs::read_to_string(pulled[1].backup.as_ref().unwrap()).unwrap(),
            "a-version"
        );

        assert!(
            a.push(&b.id, "TNP", "mods", &ra, &["only-a.jar".into()])
                .is_empty()
        );
        assert_eq!(
            std::fs::read_to_string(rb.join("mods/only-a.jar")).unwrap(),
            "a"
        );
        // Paths can't escape the folder.
        assert!(!a.pull(&b.id, "TNP", "mods", &ra, &["../x".into()])[0].ok);

        a.stop();
        b.stop();
        *TRUSTED_FILE.lock().unwrap() = None;
        let _ = std::fs::remove_dir_all(&root);
    }
}
