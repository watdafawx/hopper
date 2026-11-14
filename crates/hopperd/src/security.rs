//! Checks every request goes through, before its handler:
//! - plain HTTP only where the network is private (see [`Net`]);
//! - rate limits per address (tighter on sign-in and pairing);
//! - API tokens and users only do what they're scoped to.
//!
//! Afterwards, changes and sign-ins go to the audit log.

use crate::App;
use crate::net::{Net, Transport};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead as _, Write as _};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Requests per minute from one address.
const LIMIT_ALL: u32 = 1200;
/// Sign-in, setup and pairing requests per minute from one address.
const LIMIT_AUTH: u32 = 20;
/// A reverse proxy on this PC or the LAN (Nginx Proxy Manager, Caddy)
/// sends everyone's requests from its own address, so each person is
/// counted by the address it passes on. The proxy as a whole still gets
/// a ceiling, because a LAN device can make that header up to look like
/// many people: requests per minute through one proxy address...
const PROXY_ALL: u32 = LIMIT_ALL * 10;
/// ...and sign-in, setup and pairing requests through it.
const PROXY_AUTH: u32 = LIMIT_AUTH * 3;

/// When a minute's count started, all requests, guessable ones.
type Count = (Instant, u32, u32);

#[derive(Default)]
pub struct Limiter {
    seen: Mutex<HashMap<IpAddr, Count>>,
    proxies: Mutex<HashMap<IpAddr, Count>>,
    busy: Mutex<HashMap<IpAddr, Tally>>,
}

/// Where an address stands at 80% of its limit: a loop in a page shows in
/// the log before it locks the person out. Paths only, never queries,
/// tokens or bodies.
const WARN_AT: u32 = LIMIT_ALL / 10 * 8;
/// Most distinct paths kept per address and minute.
const TALLY_PATHS: usize = 200;

#[derive(Default)]
struct Tally {
    since: Option<Instant>,
    total: u32,
    warned: bool,
    paths: HashMap<String, u32>,
}

impl Tally {
    /// Counts one request; returns the warning the first time this minute's
    /// count reaches [`WARN_AT`], naming the three busiest paths.
    fn add(&mut self, now: Instant, path: &str) -> Option<String> {
        if self.since.is_none_or(|s| now.duration_since(s) >= Duration::from_secs(60)) {
            *self = Tally { since: Some(now), ..Default::default() };
        }
        self.total += 1;
        if let Some(n) = self.paths.get_mut(path) {
            *n += 1;
        } else if self.paths.len() < TALLY_PATHS {
            self.paths.insert(path.to_string(), 1);
        }
        if self.warned || self.total < WARN_AT {
            return None;
        }
        self.warned = true;
        let mut top: Vec<_> = self.paths.iter().collect();
        top.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let top: Vec<String> = top.iter().take(3).map(|(p, n)| format!("{p} x{n}")).collect();
        Some(format!("{} requests in under a minute (limit {LIMIT_ALL}); busiest: {}", self.total, top.join(", ")))
    }
}

/// Adds a request to `ip`'s count for this minute and returns the count.
fn bump(m: &mut HashMap<IpAddr, Count>, ip: IpAddr, auth: bool) -> (u32, u32) {
    if m.len() > 10_000 {
        m.retain(|_, (at, _, _)| at.elapsed() < Duration::from_secs(60));
    }
    let e = m.entry(ip).or_insert((Instant::now(), 0, 0));
    if e.0.elapsed() >= Duration::from_secs(60) {
        *e = (Instant::now(), 0, 0);
    }
    e.1 += 1;
    if auth {
        e.2 += 1;
    }
    (e.1, e.2)
}

impl Limiter {
    /// Counts a request from `client` (the address a private proxy passed
    /// on, else the peer); false when it's over its limit, or the proxy
    /// it came through is over the proxy's.
    pub fn allow(&self, peer: IpAddr, client: IpAddr, auth: bool) -> bool {
        let (all, guess) = bump(&mut self.seen.lock().unwrap(), client, auth);
        let mut ok = all <= LIMIT_ALL && (!auth || guess <= LIMIT_AUTH);
        if client != peer {
            let (all, guess) = bump(&mut self.proxies.lock().unwrap(), peer, auth);
            ok &= all <= PROXY_ALL && (!auth || guess <= PROXY_AUTH);
        }
        ok
    }

    /// Notes an allowed API request; logs one warning per minute when this
    /// address is close to its limit.
    pub fn note(&self, client: IpAddr, path: &str) {
        let warning = {
            let mut m = self.busy.lock().unwrap();
            if m.len() > 10_000 {
                m.retain(|_, t| t.since.is_some_and(|s| s.elapsed() < Duration::from_secs(60)));
            }
            m.entry(client).or_default().add(Instant::now(), path)
        };
        if let Some(w) = warning {
            log::warn!("{client}: {w}. A page may be asking in a loop.");
        }
    }
}

/// Who a sign-in became, set on the response for the audit log.
#[derive(Clone)]
pub struct SignedIn(pub String, pub String);

/// A handler adds this to its response to say what a change did when the
/// path alone doesn't (shown after the path in the audit log).
#[derive(Clone)]
pub struct AuditNote(pub String);

/// A line of the audit log.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    /// Unix seconds.
    pub at: u64,
    pub who: String,
    /// The actor's role: owner, manager, admin, operator, viewer, device or
    /// token (empty for anonymous requests and older lines).
    #[serde(default)]
    pub role: String,
    /// The person's address: the one a reverse proxy on this PC or the LAN
    /// passed on, else the connection's.
    pub ip: String,
    /// The proxy the request came through, when it came through one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub proxy: String,
    pub net: Net,
    pub method: String,
    pub path: String,
    pub status: u16,
    /// The node a relayed request went to (design/multi-panel.md, A4), so
    /// the main's log names it and can be read for one node.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub node: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub node_name: String,
}

pub struct Audit {
    path: PathBuf,
    file: Mutex<()>,
}

const AUDIT_MAX: u64 = 5 * 1024 * 1024;

impl Audit {
    pub fn new(dir: &std::path::Path) -> Self {
        Audit { path: dir.join("audit.log"), file: Mutex::new(()) }
    }

    pub fn record(&self, e: &Event) {
        let _g = self.file.lock().unwrap();
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() > AUDIT_MAX) {
            let _ = std::fs::rename(&self.path, self.path.with_extension("1.log"));
        }
        let line = serde_json::to_string(e).unwrap_or_default();
        let r = std::fs::OpenOptions::new().create(true).append(true).open(&self.path).and_then(|mut f| writeln!(f, "{line}"));
        if let Err(err) = r {
            log::warn!("audit log: {err}");
        }
    }

    /// The newest `limit` events about node `node`, newest first. Lines from
    /// before A4 have no `node`: their path still names it.
    pub fn recent_for_node(&self, node: &str, limit: usize) -> Vec<Event> {
        // A line that doesn't name the node anywhere can't be about it, so
        // most lines are skipped without being parsed.
        self.recent_where(limit, |l| l.contains(node), |e| e.node == node || (e.node.is_empty() && node_of(e.path.split(' ').next().unwrap_or(""), None).as_deref() == Some(node)))
    }

    /// The newest `limit` events, newest first.
    pub fn recent(&self, limit: usize) -> Vec<Event> {
        self.recent_where(limit, |_| true, |_| true)
    }

    /// The newest `limit` events that pass `line` (on the raw line) and
    /// `keep`, newest first. Reads the two log files once, holding at most
    /// `limit` events (the files are up to 10 MB together).
    fn recent_where(&self, limit: usize, line: impl Fn(&str) -> bool, keep: impl Fn(&Event) -> bool) -> Vec<Event> {
        let _g = self.file.lock().unwrap();
        let mut out: std::collections::VecDeque<Event> = std::collections::VecDeque::new();
        if limit == 0 {
            return Vec::new();
        }
        for p in [self.path.with_extension("1.log"), self.path.clone()] {
            if let Ok(f) = std::fs::File::open(&p) {
                for l in std::io::BufReader::new(f).lines().map_while(Result::ok) {
                    if !line(&l) {
                        continue;
                    }
                    let Ok(e) = serde_json::from_str::<Event>(&l) else { continue };
                    if keep(&e) {
                        if out.len() == limit {
                            out.pop_front();
                        }
                        out.push_back(e);
                    }
                }
            }
        }
        out.into_iter().rev().collect()
    }
}

/// The server id a path is about (`/api/v1/servers/{id}/…`, `/api/v1/fs/{id}/…`);
/// a node's server keeps its namespaced id (`node:id`, `:` maybe encoded).
pub(crate) fn server_of(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/api/v1/servers/").or_else(|| path.strip_prefix("/api/v1/fs/"))?;
    let id = rest.split('/').next()?;
    (!id.is_empty() && !matches!(id, "infos" | "rescan" | "create")).then(|| crate::relay::decode_seg(id))
}

/// The job a path is about (`/api/v1/jobs/{id}/...`, `{node}:{n}` for a
/// node's job).
fn job_of(path: &str) -> Option<String> {
    let id = crate::relay::decode_seg(path.strip_prefix("/api/v1/jobs/")?.split('/').next()?);
    (!id.is_empty()).then_some(id)
}

/// Someone limited to some servers follows or cancels only the jobs of
/// those servers: a job's title, steps and result name its server, and
/// cancelling one stops someone else's update. A job started by no
/// server's request (a new server, the panel's own work) is for people
/// with every server only.
pub(crate) fn job_refusal(app: &App, who: &crate::auth::Who, path: &str) -> Option<&'static str> {
    use crate::auth::Who;
    let limited = match who {
        Who::Token { servers, .. } | Who::User { servers, .. } => !servers.is_empty(),
        _ => false,
    };
    let hidden = limited && job_of(path).is_some_and(|j| !app.job_servers.of(&j).is_some_and(|s| who.sees(&s)));
    hidden.then_some("You don't have access to that job's server.")
}

tokio::task_local! {
    /// The server the request being handled is about, so a job it starts
    /// remembers whose it is (`crate::pack_api::spawn`).
    pub(crate) static REQUEST_SERVER: Option<String>;
    /// Where the request being handled came from, as its audit line says
    /// it, for audit lines a handler writes itself (MCP tool calls).
    static CALLER: Caller;
}

/// Where a request came from, for the audit log.
#[derive(Clone)]
pub(crate) struct Caller {
    pub ip: String,
    pub proxy: String,
    pub net: Net,
}

/// The request's own address and network. Outside a request (never for
/// a route) it says so plainly instead of claiming this PC.
pub(crate) fn caller() -> Caller {
    CALLER.try_with(Clone::clone).unwrap_or_else(|_| Caller { ip: "unknown".into(), proxy: String::new(), net: Net::Internet })
}

/// What someone could guess at: passwords, codes, setup. These get the
/// tight limit. (Pairing status needs the request's long secret, and a
/// device asks every second while it waits.)
fn is_guessable(path: &str) -> bool {
    matches!(
        path,
        "/api/v1/auth/login" | "/api/v1/auth/setup" | "/api/v1/auth/2fa/enable" | "/api/v1/auth/2fa/disable" | "/api/v1/pair/request" | "/api/v1/node/join" | "/api/v1/node/request"
    )
}

/// Asking for a one-use live ticket or a file link (design/multi-panel.md,
/// B1). These POSTs change nothing, so roles treat them as reads and the
/// audit log leaves them out like other reads.
/// A move's chunk batches (`crate::transfer`): thousands of calls for one
/// server, which would push everything else out of the audit log. The
/// move's start, its finish on each end and its record in History say
/// who moved what where.
pub(crate) fn is_move_chunks(method: &Method, path: &str) -> bool {
    let p: Vec<&str> = path.split('/').collect();
    match p.as_slice() {
        ["", "api", "v1", "servers", _, "transfer", _, "chunks"] => *method == Method::POST,
        ["", "api", "v1", "receive", _, "chunks"] => *method == Method::PUT,
        _ => false,
    }
}

pub(crate) fn is_grant_request(method: &Method, path: &str) -> bool {
    *method == Method::POST
        && (path == "/api/v1/ws/ticket"
            || path.strip_prefix("/api/v1/fs/").and_then(|r| r.split_once('/')).is_some_and(|(id, rest)| !id.is_empty() && rest == "link"))
}

/// Why a scoped token or a user may not make this request, if they may not.
/// (Owner-only routes check that themselves.)
pub(crate) fn refusal(who: &crate::auth::Who, method: &Method, path: &str) -> Option<&'static str> {
    use crate::auth::{Role, Who};
    // A one-use ticket or file link only reads; using it is checked as a
    // read of what it names (`grants`).
    let reading = method == Method::GET || method == Method::HEAD || is_grant_request(method, path);
    match who {
        Who::Token { read_only, servers, .. } => {
            if *read_only && !reading && path != "/api/v1/mcp" {
                Some("This token can only read.")
            } else if !servers.is_empty() && server_of(path).is_some_and(|id| !who.sees(&id)) {
                Some("This token isn't for that server.")
            } else {
                None
            }
        }
        Who::User { role, servers, .. } => {
            if server_of(path).is_some_and(|id| !who.sees(&id)) {
                return Some("You don't have access to that server.");
            }
            // MCP checks each tool call itself.
            if path == "/api/v1/mcp" {
                return None;
            }
            // Your own sign-in: sign out, password, two-factor, your sessions.
            if reading || path == "/api/v1/auth/logout" || path == "/api/v1/auth/password" || path.starts_with("/api/v1/auth/2fa/") || path.starts_with("/api/v1/auth/sessions/") {
                return None;
            }
            match role {
                // Owner-only routes accept managers; the rest is open to them.
                Role::Manager => None,
                Role::Viewer => Some("Your account can only look."),
                Role::Operator => {
                    let rest = path.strip_prefix("/api/v1/servers/").map(|r| r.splitn(2, '/').nth(1).unwrap_or(""));
                    match rest {
                        Some("power" | "command" | "players" | "backups") => None,
                        _ => Some("Your account can start and stop servers, use the console, manage players and make backups."),
                    }
                }
                Role::Admin => {
                    if !servers.is_empty() && (path == "/api/v1/servers/create" || path == "/api/v1/servers/rescan") {
                        Some("Only accounts with every server can add servers.")
                    } else {
                        None
                    }
                }
            }
        }
        // A request the main relayed: the node's servers, with the role the
        // main gave capped by this node's ceiling (never Manager).
        Who::Main { role, .. } => {
            let as_user = Who::User { id: String::new(), name: String::new(), role: crate::relay::cap(*role, Role::Admin), servers: Vec::new() };
            refusal(&as_user, method, path)
        }
        _ => None,
    }
}

/// `crate::secrets::refusal` for a request about one of this panel's own
/// servers (a node's are checked on the node, which has the files).
pub(crate) fn secret_refusal(app: &App, who: &crate::auth::Who, method: &Method, path: &str, query: Option<&str>) -> Option<&'static str> {
    let root = server_of(path).and_then(|id| app.servers.get(&id)).map(|e| e.root.clone());
    crate::secrets::refusal(who, method, path, query, root.as_deref())
}

fn deny(status: StatusCode, msg: &str) -> Response {
    (status, axum::Json(serde_json::json!({ "error": msg }))).into_response()
}

/// The node a request on this panel is about, for the log: one relayed to
/// it (a node's server or job, a server made there), or one of its own
/// routes here (`/api/v1/nodes/{id}/...`: maintenance, update, removal).
pub(crate) fn node_of(path: &str, query: Option<&str>) -> Option<String> {
    if let Some(t) = crate::relay::target(path, query) {
        return Some(t.node);
    }
    let id = path.strip_prefix("/api/v1/nodes/")?.split('/').next()?;
    (!id.is_empty() && !matches!(id, "keys" | "pending" | "live" | "update-all") && id.chars().all(|c| c.is_ascii_alphanumeric())).then(|| id.to_string())
}

/// [`node_of`] and the node's name now (kept in the log line, so it still
/// reads after the node is removed).
pub(crate) fn node_named(app: &App, path: &str, query: Option<&str>) -> (String, String) {
    // Only a node this panel has: the id comes from the URL, and anyone
    // signed in could otherwise put any text in a log row's node.
    match node_of(path, query).and_then(|id| app.nodes.name_of(&id).map(|name| (id, name))) {
        Some(found) => found,
        None => (String::new(), String::new()),
    }
}

pub(crate) fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The middleware. API answers to a page on another origin the owner listed
/// get CORS headers (see [`crate::web_access`]); every other answer is as
/// before.
pub async fn guard(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    let api = req.uri().path().starts_with("/api/");
    let (origins, manage_from_others) = {
        let cfg = app.cfg.read().unwrap();
        (cfg.web_origins.clone(), cfg.web_manage_access)
    };
    let listed = if api { crate::web_access::listed_other(req.headers(), &origins).map(str::to_string) } else { None };
    let had_origin = api && crate::web_access::has_origin(req.headers());
    // A preflight is answered whole by `web_access::before`.
    let preflight = req.method() == Method::OPTIONS && req.headers().contains_key(axum::http::header::ACCESS_CONTROL_REQUEST_METHOD) && had_origin;
    let res = checks(app, req, next, &origins, manage_from_others).await;
    if api && !preflight { crate::web_access::after(res, listed.as_deref(), had_origin) } else { res }
}

async fn checks(app: Arc<App>, mut req: Request, next: Next, origins: &[String], manage_from_others: bool) -> Response {
    // A request this node's main relayed (`crate::relay`): the main did its
    // own sign-in and limits; here the role and ceiling apply, then the log.
    if let Some(w) = crate::relay::relayed() {
        return relayed_checks(app, w, req, next).await;
    }
    let ip = req.extensions().get::<ConnectInfo<SocketAddr>>().map_or(IpAddr::from([127, 0, 0, 1]), |c| c.0.ip());
    let tls = req.extensions().get::<Transport>().is_none_or(|t| t.tls);
    let net = Net::of(ip);
    let path = req.uri().path().to_string();
    let method = req.method().clone();
    let api = path.starts_with("/api/");

    // Plain HTTP only on private networks (or nowhere, when TLS is required).
    let require_tls = app.cfg.read().unwrap().require_tls;
    if !tls && (!net.private() || (require_tls && net != Net::Local)) {
        let host = req.headers().get("host").and_then(|h| h.to_str().ok()).unwrap_or("this-panel");
        return deny(
            StatusCode::FORBIDDEN,
            &format!("This panel needs an encrypted connection from here. Use https://{host}{path}"),
        );
    }

    let from = crate::net::client_ip(ip, req.headers());
    // A CORS preflight counts towards the address's limit but is never a
    // sign-in attempt, even when it's for the sign-in route.
    if api && !app.limiter.allow(ip, from, method != Method::OPTIONS && is_guessable(&path)) {
        return deny(StatusCode::TOO_MANY_REQUESTS, "Too many requests. Wait a minute.");
    }
    if api {
        app.limiter.note(from, &path);
        // Preflights are answered here, before any sign-in check.
        if let crate::web_access::Before::Answer(r) = crate::web_access::before(&mut req, origins) {
            return r;
        }
    }

    // Where the log says a request came from: the person, and the proxy
    // in between when there is one.
    let (log_ip, log_net, proxy) = (from.to_string(), Net::of(from), if from == ip { String::new() } else { ip.to_string() });

    // Who's asking, for scopes and the log. A session made for another
    // page only counts when that page sent the request (`Auth::check_from`).
    let token = crate::api::token(req.headers()).0;
    let origin = req.headers().get(axum::http::header::ORIGIN).and_then(|v| v.to_str().ok()).map(str::to_string);
    let who = if token.is_empty() { None } else { app.auth.check_from(&token, origin.as_deref()) };
    let remote = who.is_some() && app.auth.remote_origin(&token).is_some();
    // Who can get in is managed on the panel's own page (unless the owner
    // lets listed pages do it too).
    if api && let Some(msg) = crate::web_access::access_refusal(req.headers(), origins, manage_from_others, remote, &method, &path) {
        let (name, role) = who.as_ref().map_or_else(|| ("-".to_string(), String::new()), |w| (w.name(), w.role_name().to_string()));
        app.audit.record(&Event { at: now(), who: name, role, ip: log_ip, proxy, net: log_net, method: method.to_string(), path, status: 403, node: String::new(), node_name: String::new() });
        return deny(StatusCode::FORBIDDEN, msg);
    }
    if let Some(w) = &who {
        if let Some(msg) = refusal(w, &method, &path).or_else(|| job_refusal(&app, w, &path)).or_else(|| secret_refusal(&app, w, &method, &path, req.uri().query())) {
            let (node, node_name) = node_named(&app, &path, req.uri().query());
            app.audit.record(&Event { at: now(), who: w.name(), role: w.role_name().into(), ip: log_ip, proxy, net: log_net, method: method.to_string(), path, status: 403, node, node_name });
            return deny(StatusCode::FORBIDDEN, msg);
        }
    }

    let client = req.headers().get("x-hopperd-client").and_then(|v| v.to_str().ok()).map(|v| v.chars().take(64).collect::<String>());
    // A node's server: the request goes to that node over its tunnel. Its
    // changes come back on the node's live feed, so none is sent here.
    let node = if api { crate::relay::target(&path, req.uri().query()) } else { None };
    let relayed_to_node = node.is_some();
    let (node_id, node_name) = if api { node_named(&app, &path, req.uri().query()) } else { (String::new(), String::new()) };
    let res = match node {
        Some(t) => crate::relay::forward(app.clone(), t, req).await,
        None => {
            let caller = Caller { ip: log_ip.clone(), proxy: proxy.clone(), net: log_net };
            REQUEST_SERVER.scope(server_of(&path), CALLER.scope(caller, next.run(req))).await
        }
    };

    // A change went through: tell every open client what to show again.
    if api && !relayed_to_node && res.status().is_success()
        && let Some((server, areas)) = crate::changes::from_request(&method, &path)
    {
        // The server list and headers come with the live snapshot: send it now.
        if areas.iter().any(|a| matches!(*a, "list" | "server")) {
            app.refresh.notify_one();
        }
        app.changes.emit(server.as_deref(), &areas, client);
    }

    // Changes, sign-ins and pairing go in the audit log.
    // Reads aren't logged (the UI checks who's signed in all the time).
    let changes = method != Method::GET && method != Method::HEAD;
    // A waiting node asks every 2 s, like a waiting PC.
    if api && changes && path != "/api/v1/pair/status" && path != "/api/v1/node/request/status" && !is_grant_request(&method, &path) {
        let status = res.status().as_u16();
        // A sign-in names who it became; a failed one stays anonymous.
        let (who_name, role) = who.as_ref().map(|w| (w.name(), w.role_name().to_string())).unwrap_or_else(|| {
            res.extensions().get::<SignedIn>().map(|s| (s.0.clone(), s.1.clone())).unwrap_or_else(|| ("-".into(), String::new()))
        });
        let path = match res.extensions().get::<AuditNote>() {
            Some(n) => format!("{path} ({})", n.0),
            None => path,
        };
        app.audit.record(&Event { at: now(), who: who_name, role, ip: log_ip, proxy, net: log_net, method: method.to_string(), path, status, node: node_id, node_name });
    }
    res
}

/// The guard for a request the main relayed: role and ceiling, the
/// change feed and the audit log, as for any other request. No sign-in
/// (the main did it), no address limits (one main stands for many people).
async fn relayed_checks(app: Arc<App>, who: crate::auth::Who, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().clone();
    let log = |status: u16, path: String| Event {
        at: now(),
        who: who.name(),
        role: who.role_name().into(),
        ip: "main".into(),
        proxy: String::new(),
        net: Net::Internet,
        method: method.to_string(),
        path,
        status,
        node: String::new(),
        node_name: String::new(),
    };
    if let Some(msg) = refusal(&who, &method, &path).or_else(|| secret_refusal(&app, &who, &method, &path, req.uri().query())) {
        app.audit.record(&log(403, path));
        return deny(StatusCode::FORBIDDEN, msg);
    }
    let client = req.headers().get("x-hopperd-client").and_then(|v| v.to_str().ok()).map(|v| v.chars().take(64).collect::<String>());
    let res = REQUEST_SERVER.scope(server_of(&path), next.run(req)).await;
    if res.status().is_success()
        && let Some((server, areas)) = crate::changes::from_request(&method, &path)
    {
        if areas.iter().any(|a| matches!(*a, "list" | "server")) {
            app.refresh.notify_one();
        }
        app.changes.emit(server.as_deref(), &areas, client);
    }
    // A chunk batch that failed is still logged.
    let quiet = is_move_chunks(&method, &path) && res.status().is_success();
    if method != Method::GET && method != Method::HEAD && !is_grant_request(&method, &path) && !quiet {
        let path = match res.extensions().get::<AuditNote>() {
            Some(n) => format!("{path} ({})", n.0),
            None => path,
        };
        app.audit.record(&log(res.status().as_u16(), path));
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_ids_in_paths() {
        assert_eq!(server_of("/api/v1/servers/tnp/power").as_deref(), Some("tnp"));
        assert_eq!(server_of("/api/v1/fs/tnp/list").as_deref(), Some("tnp"));
        assert_eq!(server_of("/api/v1/fs/ab12%3Atnp/list").as_deref(), Some("ab12:tnp"));
        assert_eq!(server_of("/api/v1/servers"), None);
        assert_eq!(server_of("/api/v1/servers/infos"), None);
        assert_eq!(server_of("/api/v1/host"), None);
    }

    #[test]
    fn user_roles() {
        use crate::auth::{Role, Who};
        let user = |role, servers: &[&str]| Who::User { id: "u".into(), name: "u".into(), role, servers: servers.iter().map(|s| s.to_string()).collect() };
        let (get, post) = (Method::GET, Method::POST);
        let v = user(Role::Viewer, &[]);
        assert!(refusal(&v, &get, "/api/v1/fs/tnp/list").is_none());
        assert!(refusal(&v, &post, "/api/v1/servers/tnp/power").is_some());
        assert!(refusal(&v, &post, "/api/v1/auth/logout").is_none());
        assert!(refusal(&v, &post, "/api/v1/auth/2fa/begin").is_none());
        let o = user(Role::Operator, &["tnp"]);
        assert!(refusal(&o, &post, "/api/v1/servers/tnp/power").is_none());
        assert!(refusal(&o, &post, "/api/v1/servers/tnp/command").is_none());
        assert!(refusal(&o, &post, "/api/v1/servers/tnp/backups").is_none());
        assert!(refusal(&o, &post, "/api/v1/servers/tnp/backups/x.zip/restore").is_some());
        assert!(refusal(&o, &Method::PUT, "/api/v1/fs/tnp/text").is_some());
        assert!(refusal(&o, &post, "/api/v1/servers/other/power").is_some(), "not their server");
        assert!(refusal(&o, &get, "/api/v1/fs/other/list").is_some(), "can't even look");
        let a = user(Role::Admin, &["tnp"]);
        assert!(refusal(&a, &Method::PUT, "/api/v1/fs/tnp/text").is_none());
        assert!(refusal(&a, &post, "/api/v1/servers/create").is_some());
        assert!(refusal(&user(Role::Admin, &[]), &post, "/api/v1/servers/create").is_none());
        let c = user(Role::Manager, &[]);
        assert!(refusal(&c, &post, "/api/v1/servers/create").is_none());
        assert!(refusal(&c, &Method::PUT, "/api/v1/fs/other/text").is_none());
    }

    #[test]
    fn limits() {
        let l = Limiter::default();
        let ip: IpAddr = "8.8.8.8".parse().unwrap();
        for _ in 0..LIMIT_AUTH {
            assert!(l.allow(ip, ip, true));
        }
        assert!(!l.allow(ip, ip, true), "sign-ins are limited");
        let other: IpAddr = "8.8.4.4".parse().unwrap();
        assert!(l.allow(other, other, true), "per address");
    }

    #[test]
    fn warns_once_a_minute_at_80_percent() {
        let mut t = Tally::default();
        let start = Instant::now();
        let mut warnings = vec![];
        for i in 0..LIMIT_ALL {
            let path = if i % 4 == 0 { "/api/v1/hello" } else { "/api/v1/servers/a/address" };
            warnings.extend(t.add(start, path));
        }
        assert_eq!(warnings.len(), 1, "one line, not one per request");
        assert!(warnings[0].contains("/api/v1/servers/a/address x"), "{}", warnings[0]);
        assert!(!warnings[0].contains('?'), "paths only");
        // Next minute: counts start over and it can warn again.
        let later = start + Duration::from_secs(61);
        assert!(t.add(later, "/api/v1/hello").is_none());
        let again = (0..WARN_AT).filter_map(|_| t.add(later, "/api/v1/hello")).count();
        assert_eq!(again, 1);
        // Below the line: silent.
        let mut quiet = Tally::default();
        assert_eq!((0..WARN_AT - 1).filter_map(|_| quiet.add(start, "/x")).count(), 0);
    }

    #[test]
    fn limits_behind_a_proxy() {
        let l = Limiter::default();
        let proxy: IpAddr = "192.168.50.3".parse().unwrap();
        let (fox, kimiko): (IpAddr, IpAddr) = ("192.168.50.2".parse().unwrap(), "84.196.98.52".parse().unwrap());
        // One busy person doesn't use up everyone else's share.
        for _ in 0..LIMIT_ALL {
            assert!(l.allow(proxy, fox, false));
        }
        assert!(!l.allow(proxy, fox, false), "each person still has a limit");
        assert!(l.allow(proxy, kimiko, false), "someone else through the same proxy");
        // Something on the proxy's own PC talking to the panel directly
        // keeps its own share too.
        assert!(l.allow(proxy, proxy, false));

        // Made-up addresses don't buy endless sign-in tries.
        let l = Limiter::default();
        let mut allowed = 0;
        for i in 0..PROXY_AUTH * 2 {
            let fake = IpAddr::from([10, 9, (i / 256) as u8, (i % 256) as u8]);
            allowed += l.allow(proxy, fake, true) as u32;
        }
        assert_eq!(allowed, PROXY_AUTH);
    }

    #[test]
    fn audit_round_trip() {
        let dir = std::env::temp_dir().join(format!("hopperd-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = Audit::new(&dir);
        for i in 0..3 {
            a.record(&Event { at: i, who: "owner".into(), role: "owner".into(), ip: "1.2.3.4".into(), proxy: String::new(), net: Net::Internet, method: "POST".into(), path: format!("/p{i}"), status: 200, node: String::new(), node_name: String::new() });
        }
        let r = a.recent(2);
        assert_eq!(r.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(), ["/p2", "/p1"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Reading one node's history keeps only the newest it asked for, from
    /// both log files, newest first; other nodes' lines don't count.
    #[test]
    fn audit_for_a_node_is_bounded_and_newest_first() {
        let dir = std::env::temp_dir().join(format!("hopperd-audit-node-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = Audit::new(&dir);
        let ev = |i: u64, node: &str| Event { at: i, who: "owner".into(), role: "owner".into(), ip: "1.2.3.4".into(), proxy: String::new(), net: Net::Local, method: "GET".into(), path: format!("/p{i}"), status: 200, node: node.into(), node_name: String::new() };
        for i in 0..50 {
            a.record(&ev(i, if i % 2 == 0 { "aaaa" } else { "bbbb" }));
        }
        // The older half in the rotated file.
        std::fs::rename(dir.join("audit.log"), dir.join("audit.1.log")).unwrap();
        for i in 50..60 {
            a.record(&ev(i, "aaaa"));
        }
        let r = a.recent_for_node("aaaa", 12);
        assert_eq!(r.iter().map(|e| e.at).collect::<Vec<_>>(), [59, 58, 57, 56, 55, 54, 53, 52, 51, 50, 48, 46]);
        assert!(a.recent_for_node("cccc", 5).is_empty());
        assert!(a.recent_for_node("aaaa", 0).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn job_ids_in_paths() {
        assert_eq!(job_of("/api/v1/jobs/7").as_deref(), Some("7"));
        assert_eq!(job_of("/api/v1/jobs/ab12%3A7/cancel").as_deref(), Some("ab12:7"));
        assert_eq!(job_of("/api/v1/jobs"), None);
        assert_eq!(job_of("/api/v1/jobs/"), None);
    }
}
