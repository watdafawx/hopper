//! The main's side of node mode (`design/multi-panel.md`, A1): which nodes
//! may connect, how they join (a join command with a one-use key, or a
//! 6-digit code the owner compares), the connections they keep open, and
//! Settings › Nodes. The node's side is [`crate::tunnel::node`].
//!
//! Kept in `nodes.json` in the panel's data folder: each node's id, name,
//! ceiling and the SHA-256 of its node token (never the token), and the
//! join keys' hashes with when they run out and who used them.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::App;
use crate::api::{ApiError, ApiResult, bad, not_found};
use crate::auth::{hash_token, random_hex};
use crate::security::{AuditNote, Event, now};
use crate::tunnel::frame::{MAX_FRAME, code};
use crate::tunnel::mux::{self, Config, Handle, MuxError, Side};

const FILE: &str = "nodes.json";
/// A join key works once, for this long.
pub const KEY_TTL: u64 = 30 * 60;
/// Used or expired keys stay listed this long, then go.
const KEY_KEEP: u64 = 24 * 3600;
/// A code request waits this long for the owner (as desktop pairing).
const REQUEST_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_PENDING: usize = 8;
/// Wrong keys and node tokens from one address: this many per window, then
/// it's refused until the window passes.
const MAX_FAILS: u32 = 10;
const FAIL_WINDOW: Duration = Duration::from_secs(10 * 60);
/// No frame for this long: "Reconnecting" (a ping is every 20 s).
const QUIET_RECONNECTING: Duration = Duration::from_secs(30);
/// And this long: "Offline" (the tunnel gives up at 60 s too).
const QUIET_OFFLINE: Duration = Duration::from_secs(60);
/// Offline this long: one alert (and one when it's back).
const ALERT_AFTER: Duration = Duration::from_secs(120);

/// What the main may do on a node (enforced by the node from A2 on).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Ceiling {
    Look,
    Operate,
    #[default]
    Manage,
}

impl Ceiling {
    pub fn id(self) -> &'static str {
        match self {
            Ceiling::Look => "look",
            Ceiling::Operate => "operate",
            Ceiling::Manage => "manage",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub name: String,
    /// SHA-256 of the node token, hex.
    pub token_hash: String,
    pub ceiling: Ceiling,
    pub created: u64,
    /// Who let it in.
    pub by: String,
    /// "key" or "code".
    pub via: String,
    #[serde(default)]
    pub last_ip: String,
    #[serde(default)]
    pub version: String,
    /// The last time it was connected (unix seconds; 0 = never).
    #[serde(default)]
    pub last_seen: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JoinKey {
    id: String,
    hash: String,
    ceiling: Ceiling,
    created: u64,
    expires: u64,
    by: String,
    #[serde(default)]
    used_by: Option<String>,
    #[serde(default)]
    used_at: Option<u64>,
}

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    nodes: Vec<Node>,
    #[serde(default)]
    keys: Vec<JoinKey>,
}

/// A node asking with a code, waiting for the owner.
struct Pending {
    id: String,
    secret_hash: [u8; 32],
    /// SHA-256 of the nonce the node chose before it learnt the id and
    /// secret (see [`node_code`]).
    commit: [u8; 32],
    /// This panel's fingerprint when it was asked.
    fingerprint: String,
    /// Set once the node revealed its nonce; only then is it shown.
    code: Option<String>,
    name: String,
    ip: IpAddr,
    version: String,
    at: Instant,
    /// Once answered: the new node's id and token (handed over once), or None for refused.
    answer: Option<Option<(String, String, Ceiling)>>,
}

struct Live {
    handle: Handle,
    /// Unix seconds it connected.
    since: u64,
    /// Which connection this is, so an old one ending can't remove a newer one.
    conn: u64,
}

#[derive(Default)]
struct Inner {
    stored: Stored,
    pending: Vec<Pending>,
    live: HashMap<String, Live>,
    /// When each node was last heard from (this run).
    heard: HashMap<String, Instant>,
    /// Why the last connection ended, for the node's row.
    ended: HashMap<String, String>,
    fails: HashMap<IpAddr, (Instant, u32)>,
    conns: u64,
}

pub struct Nodes {
    path: PathBuf,
    inner: Mutex<Inner>,
    started: Instant,
}

/// Why a join key was refused (for the audit log; the node is told only
/// "refused").
#[derive(Debug, PartialEq)]
pub enum KeyRefusal {
    Unknown,
    Used,
    Expired,
}

impl KeyRefusal {
    fn note(&self) -> &'static str {
        match self {
            KeyRefusal::Unknown => "refused: unknown join key",
            KeyRefusal::Used => "refused: join key already used",
            KeyRefusal::Expired => "refused: join key expired",
        }
    }
}

/// A node's state as the main shows it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeState {
    /// Joined but never connected.
    New,
    Connected,
    Reconnecting,
    Offline,
}

fn hash32(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}

fn same(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The 6-digit code both sides show for a code request: the same commit and
/// reveal as desktop pairing ([`hopper_core::hopperd::committed_pair_code`]).
/// The node picks `nonce` and sends only its hash with the request; the main
/// then picks `id` and `secret`; only then does the node reveal the nonce.
pub fn node_code(fingerprint: &str, id: &str, secret: &str, nonce: &str) -> String {
    hopper_core::hopperd::committed_pair_code(fingerprint, id, secret, nonce)
}

/// Wrong guesses from IPv6 count per /64: one machine usually holds a whole
/// network of addresses.
fn guess_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) if v6.to_ipv4_mapped().is_none() => {
            let s = v6.segments();
            IpAddr::V6(std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
        IpAddr::V6(v6) => IpAddr::V4(v6.to_ipv4_mapped().unwrap_or(std::net::Ipv4Addr::UNSPECIFIED)),
        v4 => v4,
    }
}

/// At most this many addresses are tracked for wrong guesses; past it the
/// oldest is forgotten, so a flood of addresses can't use up memory.
const MAX_TRACKED: usize = 4096;

/// A node name as shown: one line, no control characters, at most 60 chars.
fn clean_name(s: &str) -> String {
    let n: String = s.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(60).collect();
    if n.is_empty() { "node".into() } else { n }
}

fn clean_version(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+')).take(32).collect()
}

impl Nodes {
    pub fn new(data_dir: &std::path::Path) -> Self {
        let path = data_dir.join(FILE);
        let stored = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Nodes { path, inner: Mutex::new(Inner { stored, ..Default::default() }), started: Instant::now() }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, inner: &Inner) {
        let body = serde_json::to_vec_pretty(&inner.stored).unwrap_or_default();
        let tmp = self.path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, &self.path)) {
            log::warn!("saving {}: {e}", self.path.display());
        }
    }

    // ---- wrong keys ----

    /// Whether this address made too many wrong guesses lately.
    pub fn blocked(&self, ip: IpAddr) -> bool {
        let ip = guess_key(ip);
        let mut g = self.lock();
        match g.fails.get(&ip) {
            Some((at, _)) if at.elapsed() >= FAIL_WINDOW => {
                g.fails.remove(&ip);
                false
            }
            Some((_, n)) => *n >= MAX_FAILS,
            None => false,
        }
    }

    pub fn failed(&self, ip: IpAddr) {
        let ip = guess_key(ip);
        let mut g = self.lock();
        if g.fails.len() >= MAX_TRACKED && !g.fails.contains_key(&ip) {
            g.fails.retain(|_, (at, _)| at.elapsed() < FAIL_WINDOW);
            if g.fails.len() >= MAX_TRACKED {
                let oldest = g.fails.iter().min_by_key(|(_, (at, _))| *at).map(|(k, _)| *k);
                if let Some(k) = oldest {
                    g.fails.remove(&k);
                }
            }
        }
        let e = g.fails.entry(ip).or_insert((Instant::now(), 0));
        if e.0.elapsed() >= FAIL_WINDOW {
            *e = (Instant::now(), 0);
        }
        e.1 += 1;
    }

    // ---- join keys ----

    /// A new join key; only its hash is kept.
    pub fn make_key(&self, ceiling: Ceiling, by: &str) -> (String, String, u64) {
        let key = format!("hn_{}", random_hex(24));
        let id = random_hex(4);
        let t = now();
        let mut g = self.lock();
        g.stored.keys.retain(|k| t < k.expires + KEY_KEEP);
        g.stored.keys.push(JoinKey { id: id.clone(), hash: hash_token(&key), ceiling, created: t, expires: t + KEY_TTL, by: by.into(), used_by: None, used_at: None });
        self.save(&g);
        (id, key, t + KEY_TTL)
    }

    pub fn remove_key(&self, id: &str) -> bool {
        let mut g = self.lock();
        let n = g.stored.keys.len();
        g.stored.keys.retain(|k| k.id != id);
        let removed = g.stored.keys.len() != n;
        if removed {
            self.save(&g);
        }
        removed
    }

    /// Uses a join key: a new node with the key's ceiling, and its token.
    pub fn use_key(&self, key: &str, name: &str, ip: IpAddr, version: &str) -> Result<(Node, String), KeyRefusal> {
        let h = hash32(key);
        let t = now();
        let mut g = self.lock();
        let k = g.stored.keys.iter_mut().find(|k| hex32(&k.hash).is_some_and(|x| same(&x, &h))).ok_or(KeyRefusal::Unknown)?;
        if k.used_by.is_some() {
            return Err(KeyRefusal::Used);
        }
        if t >= k.expires {
            return Err(KeyRefusal::Expired);
        }
        let (token, node) = new_node(name, k.ceiling, &format!("join key {} ({})", k.id, k.by), "key", ip, version);
        k.used_by = Some(node.id.clone());
        k.used_at = Some(t);
        g.stored.nodes.push(node.clone());
        self.save(&g);
        Ok((node, token))
    }

    // ---- code requests ----

    /// A code request; `commit` is the SHA-256 of the node's nonce. The code
    /// exists only once the node reveals it ([`Nodes::request_status`]).
    pub fn request(&self, name: &str, ip: IpAddr, version: &str, fingerprint: &str, commit: [u8; 32]) -> Result<(String, String), &'static str> {
        let mut g = self.lock();
        g.pending.retain(|p| p.at.elapsed() < REQUEST_TTL);
        if g.pending.iter().filter(|p| p.answer.is_none()).count() >= MAX_PENDING {
            return Err("Too many nodes are waiting. Answer them in Settings › Nodes first.");
        }
        let (id, secret) = (random_hex(8), random_hex(32));
        g.pending.push(Pending {
            id: id.clone(),
            secret_hash: hash32(&secret),
            commit,
            fingerprint: fingerprint.into(),
            code: None,
            name: clean_name(name),
            ip,
            version: clean_version(version),
            at: Instant::now(),
            answer: None,
        });
        Ok((id, secret))
    }

    /// What a waiting node learns. Its first ask reveals the nonce it
    /// committed to; a nonce that doesn't match ends the request. The token
    /// is handed over once.
    pub fn request_status(&self, id: &str, secret: &str, nonce: &str) -> Option<Value> {
        let mut g = self.lock();
        let i = g.pending.iter().position(|p| p.id == id && same(&p.secret_hash, &hash32(secret)))?;
        if g.pending[i].at.elapsed() >= REQUEST_TTL {
            g.pending.remove(i);
            return Some(json!({ "state": "expired" }));
        }
        if g.pending[i].code.is_none() {
            let p = &mut g.pending[i];
            if nonce.len() < 32 || nonce.len() > 128 || !same(&p.commit, &hash32(nonce)) {
                g.pending.remove(i);
                return None;
            }
            p.code = Some(node_code(&p.fingerprint, &p.id, secret, nonce));
        }
        match &g.pending[i].answer {
            None => Some(json!({ "state": "waiting" })),
            Some(None) => {
                g.pending.remove(i);
                Some(json!({ "state": "denied" }))
            }
            Some(Some((node_id, token, ceiling))) => {
                let v = json!({ "state": "allowed", "node_id": node_id, "token": token, "ceiling": ceiling.id() });
                g.pending.remove(i);
                Some(v)
            }
        }
    }

    /// The owner's answer to a code request. Returns the node's name.
    pub fn answer(&self, id: &str, allow: bool, ceiling: Ceiling, by: &str) -> Option<String> {
        let mut g = self.lock();
        // Only a request whose code the owner could compare.
        let p = g.pending.iter_mut().find(|p| p.id == id && p.code.is_some() && p.answer.is_none() && p.at.elapsed() < REQUEST_TTL)?;
        let name = p.name.clone();
        if !allow {
            p.answer = Some(None);
            return Some(name);
        }
        let (token, node) = new_node(&p.name, ceiling, by, "code", p.ip, &p.version);
        p.answer = Some(Some((node.id.clone(), token, ceiling)));
        g.stored.nodes.push(node);
        self.save(&g);
        Some(name)
    }

    // ---- nodes ----

    /// The node a token belongs to.
    pub fn by_token(&self, token: &str) -> Option<Node> {
        if token.is_empty() {
            return None;
        }
        let h = hash32(token);
        self.lock().stored.nodes.iter().find(|n| hex32(&n.token_hash).is_some_and(|t| same(&t, &h))).cloned()
    }

    /// Removes a node; its connection (if any) is returned to be closed.
    pub fn remove(&self, id: &str) -> Option<(Node, Option<Handle>)> {
        let mut g = self.lock();
        let i = g.stored.nodes.iter().position(|n| n.id == id)?;
        let node = g.stored.nodes.remove(i);
        let live = g.live.remove(id).map(|l| l.handle);
        g.heard.remove(id);
        g.ended.remove(id);
        self.save(&g);
        Some((node, live))
    }

    /// A connection came up; returns its number and the one it replaces, or
    /// None when the node was removed (or its token changed) since its token
    /// was checked: the upgrade and the hello take a while, and a removal in
    /// between must not leave a live tunnel nobody can see.
    fn attach(&self, node: &Node, handle: Handle, ip: IpAddr, version: &str) -> Option<(u64, Option<Handle>)> {
        let id = node.id.as_str();
        let mut g = self.lock();
        if !g.stored.nodes.iter().any(|n| n.id == id && n.token_hash == node.token_hash) {
            return None;
        }
        g.conns += 1;
        let conn = g.conns;
        let old = g.live.insert(id.into(), Live { handle, since: now(), conn }).map(|l| l.handle);
        g.heard.insert(id.into(), Instant::now());
        g.ended.remove(id);
        if let Some(n) = g.stored.nodes.iter_mut().find(|n| n.id == id) {
            n.last_ip = ip.to_string();
            n.version = clean_version(version);
            n.last_seen = now();
        }
        self.save(&g);
        Some((conn, old))
    }

    /// A connection ended (`quiet`: how long it had been silent).
    fn detach(&self, id: &str, conn: u64, quiet: Duration, why: String) {
        let mut g = self.lock();
        if g.live.get(id).is_some_and(|l| l.conn == conn) {
            g.live.remove(id);
            let heard = Instant::now().checked_sub(quiet).unwrap_or_else(Instant::now);
            g.heard.insert(id.into(), heard);
            g.ended.insert(id.into(), why);
            let secs = now().saturating_sub(quiet.as_secs());
            if let Some(n) = g.stored.nodes.iter_mut().find(|n| n.id == id) {
                n.last_seen = secs;
            }
            self.save(&g);
        }
    }

    fn state_of(&self, g: &Inner, n: &Node) -> (NodeState, Option<Duration>) {
        if let Some(l) = g.live.get(&n.id) {
            let q = l.handle.quiet_for();
            let s = if q < QUIET_RECONNECTING {
                NodeState::Connected
            } else if q < QUIET_OFFLINE {
                NodeState::Reconnecting
            } else {
                NodeState::Offline
            };
            return (s, Some(q));
        }
        match g.heard.get(&n.id) {
            Some(h) if h.elapsed() < QUIET_OFFLINE => (NodeState::Reconnecting, Some(h.elapsed())),
            Some(h) => (NodeState::Offline, Some(h.elapsed())),
            None if n.last_seen == 0 => (NodeState::New, None),
            None => (NodeState::Offline, None),
        }
    }

    /// Settings › Nodes.
    pub fn list(&self, fingerprint: &str) -> Value {
        let t = now();
        let mut g = self.lock();
        g.pending.retain(|p| p.at.elapsed() < REQUEST_TTL);
        let nodes: Vec<Value> = g
            .stored
            .nodes
            .iter()
            .map(|n| {
                let (state, quiet) = self.state_of(&g, n);
                let live = g.live.get(&n.id);
                json!({
                    "id": n.id,
                    "name": n.name,
                    "ceiling": n.ceiling.id(),
                    "state": state,
                    "quiet_secs": quiet.map(|q| q.as_secs()),
                    "since": live.map(|l| l.since),
                    "last_seen": if live.is_some() { t } else { n.last_seen },
                    "rtt_ms": live.and_then(|l| l.handle.rtt()).map(|d| d.as_millis() as u64),
                    "version": n.version,
                    "ip": n.last_ip,
                    "via": n.via,
                    "created": n.created,
                    "ended": g.ended.get(&n.id),
                })
            })
            .collect();
        let pending: Vec<Value> = g
            .pending
            .iter()
            .filter(|p| p.answer.is_none() && p.code.is_some())
            .map(|p| json!({ "id": p.id, "name": p.name, "ip": p.ip.to_string(), "code": p.code, "version": p.version, "expires_in": REQUEST_TTL.saturating_sub(p.at.elapsed()).as_secs() }))
            .collect();
        let keys: Vec<Value> = g
            .stored
            .keys
            .iter()
            .filter(|k| k.used_by.is_none() && t < k.expires)
            .map(|k| json!({ "id": k.id, "ceiling": k.ceiling.id(), "created": k.created, "expires": k.expires, "by": k.by }))
            .collect();
        json!({ "nodes": nodes, "pending": pending, "keys": keys, "fingerprint": fingerprint, "version": env!("CARGO_PKG_VERSION") })
    }

    /// Nodes gone long enough to alert about, and ones back after one.
    fn health(&self) -> Vec<(String, String, bool)> {
        let g = self.lock();
        g.stored
            .nodes
            .iter()
            .filter_map(|n| {
                let (state, _) = self.state_of(&g, n);
                match state {
                    NodeState::New => None,
                    NodeState::Connected => Some((n.id.clone(), n.name.clone(), false)),
                    NodeState::Reconnecting | NodeState::Offline => {
                        // Not heard from this run: count from the start, so a
                        // restart of the main doesn't alert about every node.
                        let gone = g.heard.get(&n.id).map_or(self.started.elapsed(), |h| h.elapsed());
                        (gone >= ALERT_AFTER).then(|| (n.id.clone(), n.name.clone(), true))
                    }
                }
            })
            .collect()
    }
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

fn new_node(name: &str, ceiling: Ceiling, by: &str, via: &str, ip: IpAddr, version: &str) -> (String, Node) {
    let token = format!("hnt_{}", random_hex(32));
    let node = Node {
        id: random_hex(6),
        name: clean_name(name),
        token_hash: hash_token(&token),
        ceiling,
        created: now(),
        by: by.into(),
        via: via.into(),
        last_ip: ip.to_string(),
        version: clean_version(version),
        last_seen: 0,
    };
    (token, node)
}

// ---- the join command ----

/// Only characters that are safe inside a shell or PowerShell argument
/// without quoting surprises (`design/multi-panel.md`, A1: the command is
/// pasted into a root shell).
fn shell_safe(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '[' | ']' | '/'))
}

/// The one-line commands that install hopperd as a service and join.
pub fn commands(url: &str, key: &str, fingerprint: &str, version: &str) -> Option<(String, String)> {
    if ![url.trim_start_matches("https://"), key, fingerprint, version].iter().all(|s| shell_safe(s)) || !url.starts_with("https://") {
        return None;
    }
    let base = format!("https://github.com/watdafawx/hopper/releases/download/hopperd-v{version}/hopperd-{version}");
    let join = format!("node join {url} --key {key} --ca {fingerprint} --service");
    let linux = format!(
        "curl -fsSL {base}-linux-$(uname -m | sed 's/x86_64/x64/;s/aarch64/arm64/').tar.gz | sudo tar xz -C /usr/local/bin hopperd && sudo hopperd {join}"
    );
    let windows = format!(
        "$d = \"$env:TEMP\\hopperd-{version}\"; iwr -UseBasicParsing {base}-windows-x64.zip -OutFile \"$d.zip\"; Expand-Archive \"$d.zip\" $d -Force; & \"$d\\hopperd.exe\" {join}"
    );
    Some((linux, windows))
}

// ---- routes ----

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/nodes", get(list))
        .route("/api/v1/nodes/keys", post(key_create))
        .route("/api/v1/nodes/keys/{id}", delete(key_remove))
        .route("/api/v1/nodes/pending/{id}/answer", post(pending_answer))
        .route("/api/v1/nodes/{id}", delete(node_remove))
        // What nodes call (no session: a join key, a code request or a node token).
        .route("/api/v1/node/join", post(join))
        .route("/api/v1/node/request", post(request))
        .route("/api/v1/node/request/status", post(request_status))
        .route("/api/v1/node/check", get(check))
        .route("/api/v1/node/leave", post(leave))
        .route(crate::tunnel::ws::PATH, get(connect))
}

/// Only the true owner manages nodes: a node is another machine whose
/// servers the main can run code on (`design/multi-panel.md`, pairing 3).
/// Not a manager, and not a paired PC, even one that may manage.
fn owner_only(app: &App, headers: &HeaderMap, method: &Method) -> ApiResult<crate::auth::Who> {
    let who = crate::api::require_owner_who(app, headers, method)?;
    if !who.is_owner() {
        return Err(ApiError(StatusCode::FORBIDDEN, "Only the owner can manage nodes.".into()));
    }
    Ok(who)
}

fn noted(v: Value, note: String) -> Response {
    let mut r = Json(v).into_response();
    r.extensions_mut().insert(AuditNote(note));
    r
}

fn refused(status: StatusCode, msg: &str, note: String) -> Response {
    let mut r = (status, Json(json!({ "error": msg }))).into_response();
    r.extensions_mut().insert(AuditNote(note));
    r
}

async fn list(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    owner_only(&app, &headers, &method)?;
    Ok(Json(app.nodes.list(&app.tls.fingerprint)))
}

#[derive(Deserialize)]
struct KeyBody {
    #[serde(default)]
    ceiling: Ceiling,
    /// The main's address as the node should use it.
    url: String,
}

async fn key_create(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<KeyBody>) -> ApiResult<Response> {
    let who = owner_only(&app, &headers, &method)?;
    let target = crate::tunnel::dial::Target::parse(&b.url).map_err(|e| bad(format!("This panel's address: {e}")))?;
    let url = target.url();
    // Checked before the key is made, so a refused address leaves no key.
    if commands(&url, "hn_0", &app.tls.fingerprint, env!("CARGO_PKG_VERSION")).is_none() {
        return Err(bad("This panel's address has characters a command can't carry. Use its IP or plain host name."));
    }
    let (id, key, expires) = app.nodes.make_key(b.ceiling, &who.name());
    let (linux, windows) = commands(&url, &key, &app.tls.fingerprint, env!("CARGO_PKG_VERSION")).ok_or_else(|| bad("address"))?;
    Ok(noted(
        json!({ "id": id, "key": key, "expires": expires, "linux": linux, "windows": windows, "url": url, "fingerprint": app.tls.fingerprint }),
        format!("join key {id} made, may: {}", b.ceiling.id()),
    ))
}

async fn key_remove(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    owner_only(&app, &headers, &method)?;
    if !app.nodes.remove_key(&id) {
        return Err(not_found("join key"));
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct AnswerBody {
    allow: bool,
    #[serde(default)]
    ceiling: Ceiling,
}

async fn pending_answer(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<AnswerBody>,
) -> ApiResult<Response> {
    let who = owner_only(&app, &headers, &method)?;
    let name = app.nodes.answer(&id, b.allow, b.ceiling, &who.name()).ok_or_else(|| not_found("waiting node"))?;
    let note = if b.allow { format!("node \"{name}\" allowed, may: {}", b.ceiling.id()) } else { format!("node \"{name}\" refused") };
    Ok(noted(json!({ "ok": true }), note))
}

async fn node_remove(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Response> {
    owner_only(&app, &headers, &method)?;
    let (node, live) = app.nodes.remove(&id).ok_or_else(|| not_found("node"))?;
    if let Some(h) = live {
        // The node stops when it reads this; its next try would get a 401 anyway.
        h.close("removed").await;
    }
    if let Some(e) = crate::alerts::node_state(&node.id, &node.name, None) {
        crate::webhooks::emit(&app, vec![e]);
    }
    Ok(noted(json!({ "ok": true }), format!("node \"{}\" removed", node.name)))
}

fn client(addr: SocketAddr, headers: &HeaderMap) -> IpAddr {
    crate::net::client_ip(addr.ip(), headers)
}

#[derive(Deserialize)]
struct JoinBody {
    key: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
}

const TOO_MANY: &str = "Too many wrong keys from this address. Try again in 10 minutes.";

async fn join(State(app): State<Arc<App>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap, Json(b): Json<JoinBody>) -> Response {
    let ip = client(addr, &headers);
    if app.nodes.blocked(ip) {
        return refused(StatusCode::TOO_MANY_REQUESTS, TOO_MANY, "refused: too many wrong keys".into());
    }
    match app.nodes.use_key(b.key.trim(), &b.name, ip, &b.version) {
        Ok((node, token)) => {
            log::info!("node \"{}\" joined from {ip} with a join key", node.name);
            noted(
                json!({ "node_id": node.id, "token": token, "ceiling": node.ceiling.id(), "main_name": app.cfg.read().unwrap().name }),
                format!("node \"{}\" joined with a join key, may: {}", node.name, node.ceiling.id()),
            )
        }
        Err(why) => {
            app.nodes.failed(ip);
            log::warn!("node join from {ip}: {}", why.note());
            refused(StatusCode::UNAUTHORIZED, "This join key isn't valid: it was used already, ran out or was removed.", why.note().into())
        }
    }
}

#[derive(Deserialize)]
struct RequestBody {
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
    /// SHA-256 (hex) of the nonce the node reveals on its first status ask.
    #[serde(default)]
    commit: String,
}

async fn request(State(app): State<Arc<App>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap, Json(b): Json<RequestBody>) -> ApiResult<Response> {
    let ip = client(addr, &headers);
    let commit = hex32(b.commit.trim()).ok_or_else(|| bad("This Hopper Server is too old to join with a code. Update it, or use a join command."))?;
    let (id, secret) = app.nodes.request(&b.name, ip, &b.version, &app.tls.fingerprint, commit).map_err(|e| ApiError(StatusCode::TOO_MANY_REQUESTS, e.into()))?;
    log::info!("node \"{}\" asks to join from {ip}", clean_name(&b.name));
    // The code isn't sent: the node works it out from the certificate it
    // sees, so a machine in the middle can't show the right one.
    Ok(noted(json!({ "id": id, "secret": secret }), format!("node \"{}\" asks to join", clean_name(&b.name))))
}

#[derive(Deserialize)]
struct StatusBody {
    id: String,
    secret: String,
    #[serde(default)]
    nonce: String,
}

async fn request_status(State(app): State<Arc<App>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap, Json(b): Json<StatusBody>) -> Response {
    let ip = client(addr, &headers);
    if app.nodes.blocked(ip) {
        return refused(StatusCode::TOO_MANY_REQUESTS, TOO_MANY, String::new());
    }
    match app.nodes.request_status(&b.id, &b.secret, &b.nonce) {
        Some(mut v) => {
            // Allowed: the node shows this panel by name, like a key join does.
            if v.get("token").is_some() {
                v["main_name"] = json!(app.cfg.read().unwrap().name);
            }
            Json(v).into_response()
        }
        None => {
            app.nodes.failed(ip);
            Json(json!({ "state": "unknown" })).into_response()
        }
    }
}

fn bearer(headers: &HeaderMap) -> &str {
    headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).map(str::trim).unwrap_or("")
}

/// Records a node-facing request the guard doesn't log itself (reads, the
/// WebSocket upgrade).
fn audit(app: &App, addr: SocketAddr, headers: &HeaderMap, who: &str, path: String, status: u16) {
    let ip = client(addr, headers);
    let proxy = if ip == addr.ip() { String::new() } else { addr.ip().to_string() };
    app.audit.record(&Event {
        at: now(),
        who: who.into(),
        role: if who == "-" { String::new() } else { "node".into() },
        ip: ip.to_string(),
        proxy,
        net: crate::net::Net::of(ip),
        method: "GET".into(),
        path,
        status,
    });
}

/// A node checks its key (`hopperd node status` while the panel is off).
async fn check(State(app): State<Arc<App>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    let ip = client(addr, &headers);
    if app.nodes.blocked(ip) {
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "error": TOO_MANY }))).into_response();
    }
    match app.nodes.by_token(bearer(&headers)) {
        Some(n) => Json(json!({ "name": n.name, "ceiling": n.ceiling.id() })).into_response(),
        None => {
            app.nodes.failed(ip);
            audit(&app, addr, &headers, "-", "/api/v1/node/check (refused: unknown node key)".into(), 401);
            (StatusCode::UNAUTHORIZED, Json(json!({ "error": "Unknown node key." }))).into_response()
        }
    }
}

/// `hopperd node leave` on the node: it forgets the main, and the main forgets it.
async fn leave(State(app): State<Arc<App>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    let ip = client(addr, &headers);
    if app.nodes.blocked(ip) {
        return refused(StatusCode::TOO_MANY_REQUESTS, TOO_MANY, "refused: too many wrong keys".into());
    }
    let Some(n) = app.nodes.by_token(bearer(&headers)) else {
        app.nodes.failed(ip);
        return refused(StatusCode::UNAUTHORIZED, "Unknown node key.", "refused: unknown node key".into());
    };
    if let Some((node, live)) = app.nodes.remove(&n.id) {
        if let Some(h) = live {
            h.close("left").await;
        }
        if let Some(e) = crate::alerts::node_state(&node.id, &node.name, None) {
            crate::webhooks::emit(&app, vec![e]);
        }
    }
    log::info!("node \"{}\" left", n.name);
    noted(json!({ "ok": true }), format!("node \"{}\" left", n.name))
}

/// The tunnel (`design/multi-panel.md`, "A0 as built"): the node token is
/// checked before the upgrade, so a wrong one never gets a WebSocket.
async fn connect(State(app): State<Arc<App>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let ip = client(addr, &headers);
    if app.nodes.blocked(ip) {
        audit(&app, addr, &headers, "-", format!("{} (refused: too many wrong keys)", crate::tunnel::ws::PATH), 429);
        return (StatusCode::TOO_MANY_REQUESTS, TOO_MANY).into_response();
    }
    let Some(node) = app.nodes.by_token(bearer(&headers)) else {
        app.nodes.failed(ip);
        audit(&app, addr, &headers, "-", format!("{} (refused: unknown node key)", crate::tunnel::ws::PATH), 401);
        return StatusCode::UNAUTHORIZED.into_response();
    };
    ws.read_buffer_size(16 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(2 * MAX_FRAME)
        .max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_upgrade(move |socket| async move {
            let (tx, rx) = crate::tunnel::ws::split_axum(socket);
            let cfg = Config { capabilities: crate::api::CAPABILITIES.iter().map(|c| c.to_string()).collect(), ..Config::default() };
            match mux::start(Side::Main, cfg, tx, rx).await {
                Ok(t) => session(app, node, t, addr, headers).await,
                Err(e) => log::warn!("node \"{}\": the connection failed at hello: {e}", node.name),
            }
        })
}

async fn session(app: Arc<App>, node: Node, mut t: mux::Tunnel, addr: SocketAddr, headers: HeaderMap) {
    let ip = client(addr, &headers);
    let Some((conn, old)) = app.nodes.attach(&node, t.handle.clone(), ip, &t.peer.version) else {
        log::info!("node \"{}\" was removed while it connected; closed", node.name);
        t.handle.close("removed").await;
        return;
    };
    if let Some(old) = old {
        // The node reconnected before the old connection timed out.
        old.close("replaced by a new connection").await;
    }
    log::info!("node \"{}\" connected from {ip} (hopperd {})", node.name, t.peer.version);
    audit(&app, addr, &headers, &format!("node \"{}\"", node.name), format!("{} (connected, hopperd {})", crate::tunnel::ws::PATH, clean_version(&t.peer.version)), 101);
    let why = loop {
        tokio::select! {
            s = t.incoming.recv() => match s {
                // Nodes don't open streams to the main in A1.
                Some(mut s) => s.tx.reset(code::REFUSED),
                None => break t.done.await.unwrap_or(MuxError::Closed),
            },
            r = &mut t.done => break r.unwrap_or(MuxError::Closed),
        }
    };
    let quiet = t.handle.quiet_for();
    log::info!("node \"{}\" disconnected: {why}", node.name);
    let plain = match &why {
        MuxError::Timeout => "stopped answering".to_string(),
        MuxError::Closed | MuxError::Io(_) => "the connection dropped".to_string(),
        MuxError::GoAway(_, r) if r == "left" => "left".to_string(),
        MuxError::Version { .. } => "a different version (update the older one)".to_string(),
        // The detail is in the log line above; the row stays readable.
        MuxError::Protocol(_) | MuxError::GoAway(..) => "the connection broke".to_string(),
    };
    app.nodes.detach(&node.id, conn, quiet, plain);
}

/// Alerts for nodes that are gone (once, after [`ALERT_AFTER`]) and back.
pub fn start(app: Arc<App>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tick.tick().await;
            let events: Vec<_> = app.nodes.health().into_iter().filter_map(|(id, name, gone)| crate::alerts::node_state(&id, &name, Some(gone))).collect();
            crate::webhooks::emit(&app, events);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes() -> (Nodes, PathBuf) {
        let dir = std::env::temp_dir().join(format!("hopperd-nodes-{}-{}", std::process::id(), random_hex(4)));
        std::fs::create_dir_all(&dir).unwrap();
        (Nodes::new(&dir), dir)
    }

    const IP: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 9));

    #[test]
    fn a_join_key_works_once_and_only_its_hash_is_kept() {
        let (n, dir) = nodes();
        let (_, key, expires) = n.make_key(Ceiling::Operate, "owner");
        assert!(expires >= now() + KEY_TTL - 1);
        let file = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(!file.contains(&key), "the key itself is never stored");
        let (node, token) = n.use_key(&key, "vm\u{7}-1", IP, "0.62.0").unwrap();
        assert_eq!((node.name.as_str(), node.ceiling), ("vm-1", Ceiling::Operate));
        assert_eq!(n.use_key(&key, "again", IP, "").unwrap_err(), KeyRefusal::Used);
        assert_eq!(n.use_key("hn_nope", "x", IP, "").unwrap_err(), KeyRefusal::Unknown);
        assert_eq!(n.by_token(&token).unwrap().id, node.id);
        assert!(n.by_token("").is_none() && n.by_token("hnt_wrong").is_none());
        let file = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(!file.contains(&token), "the node token is never stored");
        // A restarted main still knows it.
        assert_eq!(Nodes::new(&dir).by_token(&token).unwrap().id, node.id);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_expired_join_key_is_refused() {
        let (n, dir) = nodes();
        let (id, key, _) = n.make_key(Ceiling::Manage, "owner");
        n.lock().stored.keys.iter_mut().for_each(|k| k.expires = now() - 1);
        assert_eq!(n.use_key(&key, "late", IP, "").unwrap_err(), KeyRefusal::Expired);
        assert!(n.list("FP")["keys"].as_array().unwrap().is_empty(), "no longer listed as usable");
        assert!(n.remove_key(&id));
        assert_eq!(n.use_key(&key, "late", IP, "").unwrap_err(), KeyRefusal::Unknown);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn code_requests_hand_the_token_over_once() {
        let (n, dir) = nodes();
        let fp = "AB:CD";
        let nonce = random_hex(32);
        let commit = hash32(&nonce);
        let (id, secret) = n.request("oracle", IP, "0.62.0", fp, commit).unwrap();
        assert!(n.list(fp)["pending"].as_array().unwrap().is_empty(), "no code before the reveal");
        assert!(n.request_status(&id, "wrong secret", &nonce).is_none());
        assert_eq!(n.request_status(&id, &secret, &nonce).unwrap()["state"], "waiting");
        let listed = n.list(fp);
        let code = listed["pending"][0]["code"].as_str().unwrap().to_string();
        assert_eq!(code, node_code(fp, &id, &secret, &nonce), "the node computes the same code");
        assert_eq!(n.answer(&id, true, Ceiling::Look, "owner").as_deref(), Some("oracle"));
        assert!(n.answer(&id, false, Ceiling::Look, "owner").is_none(), "answered once");
        let v = n.request_status(&id, &secret, "").unwrap();
        assert_eq!((v["state"].as_str(), v["ceiling"].as_str()), (Some("allowed"), Some("look")));
        assert!(n.by_token(v["token"].as_str().unwrap()).is_some());
        assert!(n.request_status(&id, &secret, &nonce).is_none(), "the token is handed over once");
        // Refused requests say so.
        let (id2, s2) = n.request("other", IP, "", fp, commit).unwrap();
        assert_eq!(n.request_status(&id2, &s2, &nonce).unwrap()["state"], "waiting");
        n.answer(&id2, false, Ceiling::Manage, "owner");
        assert_eq!(n.request_status(&id2, &s2, &nonce).unwrap()["state"], "denied");
        // At most MAX_PENDING wait at once.
        for i in 0..MAX_PENDING {
            n.request(&format!("n{i}"), IP, "", fp, commit).unwrap();
        }
        assert!(n.request("one too many", IP, "", fp, commit).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn wrong_guesses_lock_the_address_out() {
        let (n, dir) = nodes();
        let other: IpAddr = "10.0.0.10".parse().unwrap();
        for _ in 0..MAX_FAILS {
            assert!(!n.blocked(IP));
            n.failed(IP);
        }
        assert!(n.blocked(IP));
        assert!(!n.blocked(other), "only that address");
        n.lock().fails.get_mut(&IP).unwrap().0 = Instant::now() - FAIL_WINDOW;
        assert!(!n.blocked(IP), "until the window passes");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Security review M2: the wrong-guess table grew by one entry per new
    /// address for as long as the window lasted, and an IPv6 client (or a
    /// LAN client behind a trusted proxy header) has endless addresses.
    #[test]
    fn wrong_guesses_from_many_addresses_use_bounded_memory() {
        let (n, dir) = nodes();
        let v6 = |a: u16, b: u16, last: u16| IpAddr::V6(std::net::Ipv6Addr::new(0x2001, 0xdb8, a, b, 0, 0, 0, last));
        for i in 0..MAX_FAILS {
            n.failed(v6(1, 2, i as u16 + 1));
        }
        assert!(n.blocked(v6(1, 2, 0xffff)), "one IPv6 network counts as one address");
        assert!(!n.blocked(v6(1, 3, 1)), "the next network doesn't");
        for i in 0..30_000u32 {
            n.failed(v6((i >> 16) as u16 + 10, i as u16, 1));
        }
        assert!(n.lock().fails.len() <= 10_000, "{} entries", n.lock().fails.len());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn states_follow_the_last_frame() {
        let (n, dir) = nodes();
        let (_, key, _) = n.make_key(Ceiling::Manage, "owner");
        let (node, _) = n.use_key(&key, "vm", IP, "").unwrap();
        let state = |n: &Nodes| n.list("")["nodes"][0]["state"].as_str().unwrap().to_string();
        assert_eq!(state(&n), "new");
        assert!(n.health().is_empty(), "a node that never connected doesn't alert");
        n.lock().heard.insert(node.id.clone(), Instant::now() - Duration::from_secs(40));
        assert_eq!(state(&n), "reconnecting");
        n.lock().heard.insert(node.id.clone(), Instant::now() - Duration::from_secs(61));
        assert_eq!(state(&n), "offline");
        assert!(n.health().is_empty(), "no alert before 2 minutes");
        n.lock().heard.insert(node.id.clone(), Instant::now() - ALERT_AFTER);
        assert_eq!(n.health(), vec![(node.id.clone(), "vm".to_string(), true)]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn join_commands_carry_the_address_key_and_fingerprint() {
        let fp = "3A:7F:00:11";
        let (linux, windows) = commands("https://fox.tail.ts.net:8790", "hn_abc123", fp, "0.62.0").unwrap();
        assert!(linux.starts_with("curl -fsSL https://github.com/watdafawx/hopper/releases/download/hopperd-v0.62.0/hopperd-0.62.0-linux-$(uname -m"));
        assert!(linux.contains(".tar.gz | sudo tar xz -C /usr/local/bin hopperd && sudo hopperd node join https://fox.tail.ts.net:8790 --key hn_abc123 --ca 3A:7F:00:11 --service"));
        assert!(windows.contains("hopperd-0.62.0-windows-x64.zip"));
        assert!(windows.ends_with("hopperd.exe\" node join https://fox.tail.ts.net:8790 --key hn_abc123 --ca 3A:7F:00:11 --service"));
        assert!(commands("https://[::1]:8790", "hn_a", fp, "1.0.0").is_some());
        // Nothing that could run something else in a root shell.
        for url in ["https://fox;rm -rf /", "https://fox$(id)", "https://fox`id`", "https://a b", "https://fox'", "http://fox:8790"] {
            assert!(commands(url, "hn_a", fp, "1.0.0").is_none(), "{url}");
        }
        assert!(commands("https://fox", "hn_a;id", fp, "1.0.0").is_none());
    }
}
