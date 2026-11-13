//! Requests the main relays to a node over the tunnel (design/multi-panel.md,
//! A2), and the rules both sides apply to them.
//!
//! A node's servers show on the main with namespaced ids, `<node>:<server>`
//! (local ids are slugs, so they never hold a `:`). A request on the main for
//! `/api/v1/servers/<node>:<id>/...` or `/api/v1/fs/<node>:<id>/...` passes
//! the main's own sign-in, role and server checks (`security::guard`), then
//! goes over the tunnel as one stream: an OPEN head naming the method, the
//! node's own path, the person and their role, then the body. The node runs
//! it through its own router as [`Who::Main`] with that role capped by the
//! ceiling it stored when it joined, so every handler, path check and History
//! entry is the node's own (one set of handlers, no thinner copy for nodes).
//!
//! Both sides check the route against [`allowed`]: route families that are
//! not listed are never relayed, and new routes stay local until they're
//! added. What is never reachable through a main, whatever the ceiling: the
//! node's users, devices, tokens, TLS, update switch, node settings, SFTP,
//! the panel folder, browsing host folders and the allowed pages.

use crate::App;
use crate::auth::{Role, Who};
use axum::body::Body;
use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::tunnel::frame::code;
use crate::tunnel::mux::{RecvHalf, Stream};

/// Between a node's id and its server's id.
pub const SEP: char = ':';
/// A relayed request body (an upload chunk is 8 MB in the web UI; the
/// node's own routes accept 32 MB).
pub const MAX_BODY: usize = crate::fs_api::MAX_CHUNK;
/// A JSON answer the main reads whole (to put the node's id into job ids).
const MAX_JSON: usize = 32 * 1024 * 1024;
/// One line of the live feed or a console stream.
pub const MAX_LINE: usize = 8 * 1024 * 1024;
/// How long the main waits for a node to start answering a request. Long
/// work runs as a job on the node and answers at once.
const ANSWER_WAIT: Duration = Duration::from_secs(120);
/// Where the node's log and handlers see a relayed request come from: a
/// documentation address, so nothing treats it as this PC or the LAN.
const RELAY_ADDR: ([u8; 4], u16) = ([192, 0, 2, 1], 0);

tokio::task_local! {
    static RELAYED: Who;
}

/// Who a relayed request is from, while the node's router runs it. Set only
/// by [`serve_request`]; nothing from the network can set it.
pub fn relayed() -> Option<Who> {
    RELAYED.try_with(Who::clone).ok()
}

// ---- ids -------------------------------------------------------------------

/// `"rigvm:tnp"` → `("rigvm", "tnp")`.
pub fn split(id: &str) -> Option<(&str, &str)> {
    let (n, s) = id.split_once(SEP)?;
    (!n.is_empty() && !s.is_empty() && !s.contains(SEP) && !n.contains('/') && !s.contains('/')).then_some((n, s))
}

pub fn join(node: &str, id: &str) -> String {
    format!("{node}{SEP}{id}")
}

/// A path segment with an encoded `:` read back.
pub fn decode_seg(seg: &str) -> String {
    seg.replace("%3A", ":").replace("%3a", ":")
}

/// A request on the main that belongs to a node.
#[derive(Debug, PartialEq)]
pub struct Target {
    pub node: String,
    /// The path and query on the node.
    pub path: String,
    /// The node's path without the query, for the allowlist.
    pub route: String,
}

/// Which node a request on the main is for, and its path there: a
/// namespaced server or job id, or `?node=<id>` on making a server.
pub fn target(path: &str, query: Option<&str>) -> Option<Target> {
    let q = query.map(|q| format!("?{q}")).unwrap_or_default();
    for prefix in ["/api/v1/servers/", "/api/v1/fs/", "/api/v1/jobs/"] {
        let Some(rest) = path.strip_prefix(prefix) else { continue };
        let (seg, tail) = rest.split_once('/').map_or((rest, ""), |(a, b)| (a, b));
        let seg = decode_seg(seg);
        let Some((node, local)) = split(&seg) else { continue };
        let route = if tail.is_empty() { format!("{prefix}{local}") } else { format!("{prefix}{local}/{tail}") };
        return Some(Target { node: node.to_string(), path: format!("{route}{q}"), route });
    }
    // Making a server on a node: the node is named in the query.
    if matches!(path, "/api/v1/servers/create" | "/api/v1/servers/port-check") {
        let query = query?;
        let mut node = None;
        let mut kept = Vec::new();
        for kv in query.split('&') {
            match kv.strip_prefix("node=") {
                Some(n) if node.is_none() => node = Some(decode_seg(n)),
                Some(_) => return None,
                None => kept.push(kv),
            }
        }
        let node = node.filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric()))?;
        let q = if kept.is_empty() { String::new() } else { format!("?{}", kept.join("&")) };
        return Some(Target { node, path: format!("{path}{q}"), route: path.to_string() });
    }
    None
}

// ---- what may be relayed ---------------------------------------------------

/// Parts of a server's routes (`/api/v1/servers/{id}/<part>/...`) a main may
/// use on a node. Everything else under a server stays on the node: copying
/// a server or saving it as a template (they write outside its folder), the
/// off-site backup copies (the node's own targets and keys), sharing a log
/// to the web.
const SERVER_PARTS: &[&str] = &[
    "power", "command", "console", "players", "crash", "crashes", "stuck", "icon", "mods", "mod-backups", "neoforge", "logs", "properties", "jvm", "schedule",
    "backups", "worlds", "datapacks", "configs", "version", "pack", "packs", "address", "rcon", "remove",
];

/// File routes (`/api/v1/fs/{id}/<part>`) a main may use. Not `link` (the
/// main issues its own one-use links) and not `fetch` (it makes the node
/// download from a URL, so from the node's own network).
const FS_PARTS: &[&str] = &[
    "list", "file", "zip", "text", "ops", "walk", "hashes", "dirs", "trash", "history", "diff", "search", "uploads", "version-text", "zip-entry", "zip-list",
    "config-owners", "config",
];

/// Whether the main may relay `method route` (the node's own path, no
/// query). Checked on both sides; the node's check is the one that counts.
pub fn allowed(method: &Method, route: &str) -> bool {
    let parts: Vec<&str> = route.split('/').collect();
    // "", "api", "v1", family, id, rest...
    if parts.len() < 4 || parts[0] != "" || parts[1] != "api" || parts[2] != "v1" || parts[1..].iter().any(|p| p.is_empty() || *p == "." || *p == "..") {
        return false;
    }
    match (parts[3], &parts[4..]) {
        ("servers", ["create"]) => *method == Method::POST,
        ("servers", ["port-check"]) => *method == Method::GET,
        ("servers", [id]) => *method == Method::GET && !matches!(*id, "infos" | "rescan" | "create"),
        ("servers", [_, part, rest @ ..]) => {
            if !SERVER_PARTS.contains(part) {
                return false;
            }
            !matches!((*part, rest), ("backups", ["remote", ..]) | ("backups", ["copies"]) | ("logs", ["share"]))
        }
        ("fs", [_, part, rest @ ..]) => FS_PARTS.contains(part) && match *part {
            "uploads" => rest.len() <= 1,
            "trash" => rest.is_empty() || matches!(rest, ["restore"] | ["purge"]),
            "config" => matches!(rest, ["parse"] | ["set"]),
            _ => rest.is_empty(),
        },
        ("jobs", [_]) => *method == Method::GET,
        ("jobs", [_, "cancel" | "dismiss"]) => *method == Method::POST,
        _ => false,
    }
}

/// Query values a relayed request may not carry: a path above the server
/// folder, an absolute path or a drive. The node's handlers refuse these
/// too (`files::resolve`, which also follows symlinks); this is a second
/// look before any handler runs.
pub fn query_paths_ok(path_and_query: &str) -> bool {
    let Some((_, q)) = path_and_query.split_once('?') else { return true };
    q.split('&').all(|kv| {
        let v = kv.split_once('=').map_or("", |(_, v)| v);
        let v = percent_decode(v);
        rel_ok(&v)
    })
}

/// A relative path that stays inside wherever it is read from.
fn rel_ok(v: &str) -> bool {
    let v = v.replace('\\', "/");
    if v.starts_with('/') || v.as_bytes().get(1) == Some(&b':') {
        return false;
    }
    !v.split('/').any(|p| p == "..")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => match std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                Some(v) => {
                    out.push(v);
                    i += 2;
                }
                None => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Path-like fields in a JSON body (`ops`, `trash/restore`, `config/set`,
/// mods): the same rule as [`query_paths_ok`], at any depth.
pub fn body_paths_ok(body: &[u8]) -> bool {
    fn walk(v: &Value, key: &str, depth: u32) -> bool {
        if depth > 32 {
            return false;
        }
        match v {
            Value::String(s) => !is_path_key(key) || rel_ok(s),
            Value::Array(a) => a.iter().all(|x| walk(x, key, depth + 1)),
            Value::Object(o) => o.iter().all(|(k, x)| walk(x, k, depth + 1)),
            _ => true,
        }
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(v) => walk(&v, "", 0),
        // Not JSON (an upload chunk, a jar): nothing to read paths from.
        Err(_) => true,
    }
}

fn is_path_key(k: &str) -> bool {
    matches!(k, "path" | "paths" | "from" | "to" | "dest" | "dir" | "folder" | "file" | "files" | "target" | "rel" | "src")
}

// ---- roles -----------------------------------------------------------------

fn rank(r: Role) -> u8 {
    match r {
        Role::Viewer => 0,
        Role::Operator => 1,
        Role::Admin => 2,
        Role::Manager => 3,
    }
}

/// The lower of two roles.
pub fn cap(a: Role, b: Role) -> Role {
    if rank(a) <= rank(b) { a } else { b }
}

/// A ceiling as stored (`look`, `operate`, `manage`) as the role it allows.
/// Anything else is `None`, and a node with an unreadable ceiling refuses
/// everything (A1 review: "A2 must read the stored ceiling strictly").
pub fn ceiling_role(s: &str) -> Option<Role> {
    match s {
        "look" => Some(Role::Viewer),
        "operate" => Some(Role::Operator),
        "manage" => Some(Role::Admin),
        _ => None,
    }
}

/// The role a person on the main gets on a node's servers, before the
/// ceiling: at most Admin, which can't reach owner-only routes.
pub fn role_on_node(who: &Who) -> Role {
    match who {
        Who::Owner | Who::Device { .. } => Role::Admin,
        Who::Token { read_only: true, .. } => Role::Viewer,
        Who::Token { .. } => Role::Admin,
        Who::User { role, .. } => cap(*role, Role::Admin),
        Who::Main { role, .. } => cap(*role, Role::Admin),
    }
}

/// What a ceiling lets through, for the message.
pub fn ceiling_text(c: Role) -> &'static str {
    match c {
        Role::Viewer => "only look at its servers",
        Role::Operator => "look at, start and stop its servers and use their consoles",
        _ => "manage its servers",
    }
}

/// A name from the other side, as shown and logged: one line, short.
pub fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(60).collect::<String>().trim().to_string()
}

// ---- the stream head -------------------------------------------------------

/// What a stream from the main is for (its OPEN head, JSON).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Head {
    /// `req` (one request), `live` (the node's server list and changes) or
    /// `console` (one server's console lines).
    pub k: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub m: String,
    /// The node's path and query.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub p: String,
    /// Request headers that matter to the handlers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub h: Vec<(String, String)>,
    /// Who asked on the main, and the role they get on this node there.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
    /// The server, for `console`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub server: String,
    /// The person is the owner or a manager on the main. The node honours
    /// it only under a Manage ceiling: then owner-only routes on the
    /// allowlist (removing a server) work for them, as on their own panel.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lead: bool,
}

/// Request headers passed on to the node.
const PASS_IN: &[&str] = &["content-type", "range", "accept", "if-none-match", "if-modified-since", "x-hopperd-client"];
/// Answer headers passed back.
const PASS_OUT: &[&str] = &[
    "content-type", "content-length", "content-disposition", "content-range", "accept-ranges", "etag", "last-modified", "cache-control", "x-content-type-options",
    "content-security-policy",
];

/// The answer head the node writes first on a request stream.
#[derive(Debug, Serialize, Deserialize)]
struct Answer {
    s: u16,
    #[serde(default)]
    h: Vec<(String, String)>,
}

// ---- the main's side -------------------------------------------------------

fn refuse(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, axum::Json(json!({ "error": msg.into() }))).into_response()
}

/// Sends a request for a node's server there and streams back its answer.
/// The guard has already applied the main's roles and server lists.
pub async fn forward(app: Arc<App>, t: Target, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let who = match crate::api::require(&app, &parts.headers, &parts.method) {
        Ok(w) => w,
        Err(e) => return e.into_response(),
    };
    let method = parts.method.clone();
    let Some(info) = app.nodes.relay_info(&t.node) else {
        return refuse(StatusCode::NOT_FOUND, "No node with that id.");
    };
    if !allowed(&method, &t.route) {
        return refuse(StatusCode::FORBIDDEN, format!("That can't be done on {} from this panel. Open its own page for it.", info.name));
    }
    // Making or removing a server on a node: owners and managers only.
    let making = t.route == "/api/v1/servers/create";
    let removing = t.route.ends_with("/remove") && t.route.starts_with("/api/v1/servers/");
    if (making || removing) && !who.owner_level() {
        return refuse(StatusCode::FORBIDDEN, "Only the owner or a manager can add or remove servers on a node.");
    }
    // The main's copy of the ceiling answers early; the node checks again.
    let role = cap(role_on_node(&who), info.ceiling);
    let as_node = Who::Main { main: String::new(), user: String::new(), role, lead: false };
    if let Some(_msg) = crate::security::refusal(&as_node, &method, &t.route) {
        return refuse(StatusCode::FORBIDDEN, format!("{} lets this panel {}.", info.name, ceiling_text(info.ceiling)));
    }
    let body = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(_) => return refuse(StatusCode::PAYLOAD_TOO_LARGE, "That's too big to send to a node in one go."),
    };
    if info.maintenance.is_some() {
        let starting = making || (t.route.ends_with("/power") && serde_json::from_slice::<Value>(&body).ok().and_then(|v| v["action"].as_str().map(str::to_string)).is_some_and(|a| a == "start" || a == "restart"));
        if starting {
            return refuse(StatusCode::CONFLICT, format!("{} is under maintenance, so nothing starts there. End maintenance first.", info.name));
        }
    }
    let Some(handle) = info.handle else {
        let when = if info.last_seen > 0 { format!(" (last heard from {} ago)", crate::nodes::how_long(crate::security::now().saturating_sub(info.last_seen))) } else { String::new() };
        return refuse(StatusCode::SERVICE_UNAVAILABLE, format!("{} is offline{when}.", info.name));
    };
    let head = Head {
        k: "req".into(),
        m: method.to_string(),
        p: t.path.clone(),
        h: PASS_IN
            .iter()
            .filter_map(|k| parts.headers.get(*k).and_then(|v| v.to_str().ok()).map(|v| (k.to_string(), v.chars().take(512).collect())))
            .collect(),
        user: who_label(&who),
        role: role.id().into(),
        server: String::new(),
        lead: who.owner_level() && info.ceiling == Role::Admin,
    };
    let mut s = match handle.open(serde_json::to_vec(&head).unwrap_or_default()).await {
        Ok(s) => s,
        Err(_) => return refuse(StatusCode::SERVICE_UNAVAILABLE, format!("{} is busy or just went offline. Try again.", info.name)),
    };
    if !body.is_empty() && s.tx.send(&body).await.is_err() {
        return refuse(StatusCode::BAD_GATEWAY, format!("{} stopped answering.", info.name));
    }
    let _ = s.tx.finish().await;
    let (answer, first) = match tokio::time::timeout(ANSWER_WAIT, read_answer(&mut s.rx)).await {
        Ok(Ok(a)) => a,
        Ok(Err(e)) => return refuse(StatusCode::BAD_GATEWAY, format!("{} sent an answer this panel can't read ({e}).", info.name)),
        Err(_) => return refuse(StatusCode::GATEWAY_TIMEOUT, format!("{} didn't answer in time.", info.name)),
    };
    let status = StatusCode::from_u16(answer.s).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut b = Response::builder().status(status);
    let mut json_answer = false;
    for (k, v) in &answer.h {
        let k = k.to_ascii_lowercase();
        if !PASS_OUT.contains(&k.as_str()) {
            continue;
        }
        if k == "content-type" && v.starts_with("application/json") {
            json_answer = true;
        }
        // A JSON answer is rewritten (job ids), so its length is set below.
        if k == "content-length" && answer.h.iter().any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v.starts_with("application/json")) {
            continue;
        }
        if let Ok(v) = HeaderValue::from_str(v) {
            b = b.header(k, v);
        }
    }
    if json_answer {
        // Read whole, so job ids and made servers carry the node's id.
        let mut all = first;
        let mut rx = s.rx;
        loop {
            match rx.recv().await {
                Ok(Some(d)) => {
                    if all.len() + d.len() > MAX_JSON {
                        return refuse(StatusCode::BAD_GATEWAY, format!("{} sent too much.", info.name));
                    }
                    all.extend_from_slice(&d);
                }
                Ok(None) => break,
                Err(_) => return refuse(StatusCode::BAD_GATEWAY, format!("{} stopped answering.", info.name)),
            }
        }
        let all = namespace_answer(&t.node, &t.route, all);
        let mut r = b.header(header::CONTENT_LENGTH, all.len()).body(Body::from(all)).unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
        r.headers_mut().remove(header::CONTENT_ENCODING);
        return r;
    }
    let stream = futures_util::stream::unfold((Some(first), s.rx), |(first, mut rx)| async move {
        if let Some(f) = first.filter(|f| !f.is_empty()) {
            return Some((Ok::<_, std::io::Error>(axum::body::Bytes::from(f)), (None, rx)));
        }
        match rx.recv().await {
            Ok(Some(d)) => Some((Ok(axum::body::Bytes::from(d)), (None, rx))),
            Ok(None) => None,
            Err(_) => Some((Err(std::io::Error::other("the node stopped answering")), (None, rx))),
        }
    });
    b.body(Body::from_stream(stream)).unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

/// "mikey (admin)" as the node's History and log show it.
fn who_label(who: &Who) -> String {
    match who {
        Who::Owner => "Owner".into(),
        Who::User { name, .. } | Who::Device { name, .. } => clean(name),
        Who::Token { name, .. } => format!("token {}", clean(name)),
        Who::Main { user, .. } => clean(user),
    }
}

/// Reads the answer head: a 4-byte length, then JSON. Returns what came
/// after it in the same chunk.
async fn read_answer(rx: &mut RecvHalf) -> Result<(Answer, Vec<u8>), String> {
    let mut buf = Vec::new();
    loop {
        if buf.len() >= 4 {
            let n = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
            if n > 16 * 1024 {
                return Err("head too big".into());
            }
            if buf.len() >= 4 + n {
                let a: Answer = serde_json::from_slice(&buf[4..4 + n]).map_err(|e| e.to_string())?;
                return Ok((a, buf[4 + n..].to_vec()));
            }
        }
        match rx.recv().await {
            Ok(Some(d)) => buf.extend_from_slice(&d),
            Ok(None) => return Err("it ended early".into()),
            Err(_) => return Err("it stopped".into()),
        }
    }
}

/// Puts the node's id into what the browser uses later: a job (`{"job": 7}`
/// → `"rigvm:7"`), and the server a finished job made (`result.id`).
fn namespace_answer(node: &str, route: &str, body: Vec<u8>) -> Vec<u8> {
    let Ok(mut v) = serde_json::from_slice::<Value>(&body) else { return body };
    let mut changed = false;
    if let Some(j) = v.get_mut("job") {
        let id = match &*j {
            Value::Number(n) => Some(n.to_string()),
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            _ => None,
        };
        if let Some(id) = id {
            *j = Value::String(join(node, &id));
            changed = true;
        }
    }
    if route.starts_with("/api/v1/jobs/")
        && let Some(r) = v.get_mut("result")
        && r.get("path").is_some()
        && let Some(Value::String(id)) = r.get_mut("id")
    {
        *id = join(node, id);
        changed = true;
    }
    if changed { serde_json::to_vec(&v).unwrap_or(body) } else { body }
}

/// Reads newline-separated JSON from a stream, one value per call.
pub struct Lines {
    rx: RecvHalf,
    buf: Vec<u8>,
}

impl Lines {
    pub fn new(rx: RecvHalf) -> Self {
        Lines { rx, buf: Vec::new() }
    }

    /// The next value; `None` when the stream ends, breaks or a line is
    /// too long or not JSON.
    pub async fn next(&mut self) -> Option<Value> {
        loop {
            if let Some(i) = self.buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=i).collect();
                return serde_json::from_slice(&line[..line.len() - 1]).ok();
            }
            if self.buf.len() > MAX_LINE {
                return None;
            }
            match self.rx.recv().await {
                Ok(Some(d)) => self.buf.extend_from_slice(&d),
                _ => return None,
            }
        }
    }
}

// ---- the node's side -------------------------------------------------------

/// Serves the streams the main opens, until the connection ends. `ceiling`
/// is what `node.json` says, read strictly: `None` refuses everything.
pub async fn serve(app: Arc<App>, ceiling: Option<Role>, main: String, mut incoming: tokio::sync::mpsc::Receiver<Stream>) {
    let router = crate::api::router(app.clone());
    let main = clean(&main);
    while let Some(s) = incoming.recv().await {
        let (app, router, main) = (app.clone(), router.clone(), main.clone());
        tokio::spawn(async move { serve_stream(app, router, ceiling, main, s).await });
    }
}

async fn serve_stream(app: Arc<App>, router: axum::Router, ceiling: Option<Role>, main: String, mut s: Stream) {
    let Ok(head) = serde_json::from_slice::<Head>(&s.head) else {
        s.tx.reset(code::REFUSED);
        return;
    };
    let Some(ceiling) = ceiling else {
        log::warn!("node: refused a request from the main: this node's ceiling in node.json can't be read");
        if head.k == "req" {
            answer(&mut s, 403, "This node can't read what its main may do here (node.json). Join again to fix it.").await;
        } else {
            s.tx.reset(code::REFUSED);
        }
        return;
    };
    match head.k.as_str() {
        "req" => serve_request(app, router, ceiling, main, head, s).await,
        "live" => serve_live(app, s).await,
        "console" => serve_console(app, head, s).await,
        _ => s.tx.reset(code::REFUSED),
    }
}

/// A refusal written as an answer.
async fn answer(s: &mut Stream, status: u16, msg: &str) {
    let body = json!({ "error": msg }).to_string().into_bytes();
    let head = Answer { s: status, h: vec![("content-type".into(), "application/json".into()), ("content-length".into(), body.len().to_string())] };
    let _ = write_answer(s, &head).await;
    let _ = s.tx.send(&body).await;
    let _ = s.tx.finish().await;
}

async fn write_answer(s: &mut Stream, a: &Answer) -> Result<(), crate::tunnel::mux::StreamError> {
    let h = serde_json::to_vec(a).unwrap_or_default();
    let mut out = (h.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&h);
    s.tx.send(&out).await
}

async fn serve_request(app: Arc<App>, router: axum::Router, ceiling: Role, main: String, head: Head, mut s: Stream) {
    use tower::ServiceExt as _;
    let Ok(method) = Method::from_bytes(head.m.as_bytes()) else {
        return answer(&mut s, 400, "Bad request.").await;
    };
    // The role the main says, strictly, capped by this node's ceiling.
    let Some(role) = Role::parse(&head.role).filter(|r| *r != Role::Manager) else {
        return answer(&mut s, 403, "The main sent a role this node doesn't know.").await;
    };
    let role = cap(role, ceiling);
    let route = head.p.split('?').next().unwrap_or("").to_string();
    if !allowed(&method, &route) || !query_paths_ok(&head.p) {
        log::warn!("node: refused {method} {route} from the main (not something a main may do here)");
        let who = Who::Main { main: main.clone(), user: clean(&head.user), role, lead: false };
        record_refusal(&app, &who, &method, &route);
        return answer(&mut s, 403, "This node doesn't let its main do that. Open the node's own page for it.").await;
    }
    // The body, bounded.
    let mut body = Vec::new();
    loop {
        match s.rx.recv().await {
            Ok(Some(d)) => {
                if body.len() + d.len() > MAX_BODY {
                    return answer(&mut s, 413, "That's too big.").await;
                }
                body.extend_from_slice(&d);
            }
            Ok(None) => break,
            Err(_) => return,
        }
    }
    // Owner-level only when this node's own ceiling is Manage.
    let lead = head.lead && ceiling == Role::Admin && role == Role::Admin;
    let who = Who::Main { main, user: clean(&head.user), role, lead };
    if !body_paths_ok(&body) {
        record_refusal(&app, &who, &method, &route);
        return answer(&mut s, 400, "Paths can't go above the server folder.").await;
    }
    let mut b = Request::builder().method(method).uri(&head.p).header(header::HOST, "node");
    for (k, v) in &head.h {
        if PASS_IN.contains(&k.as_str()) {
            b = b.header(k.as_str(), v.as_str());
        }
    }
    let Ok(mut req) = b.body(Body::from(body)) else {
        return answer(&mut s, 400, "Bad request.").await;
    };
    req.extensions_mut().insert(ConnectInfo(SocketAddr::from(RELAY_ADDR)));
    let res = match RELAYED.scope(who, router.oneshot(req)).await {
        Ok(r) => r,
        Err(e) => match e {},
    };
    let (parts, body) = res.into_parts();
    let h = parts.headers.iter().filter(|(k, _)| PASS_OUT.contains(&k.as_str())).filter_map(|(k, v)| v.to_str().ok().map(|v| (k.to_string(), v.to_string()))).collect();
    if write_answer(&mut s, &Answer { s: parts.status.as_u16(), h }).await.is_err() {
        return;
    }
    let mut data = body.into_data_stream();
    use futures_util::StreamExt as _;
    while let Some(chunk) = data.next().await {
        match chunk {
            Ok(c) => {
                if s.tx.send(&c).await.is_err() {
                    return;
                }
            }
            Err(_) => {
                s.tx.reset(code::CANCEL);
                return;
            }
        }
    }
    let _ = s.tx.finish().await;
}

fn record_refusal(app: &App, who: &Who, method: &Method, route: &str) {
    app.audit.record(&crate::security::Event {
        at: crate::security::now(),
        who: who.name(),
        role: who.role_name().into(),
        ip: "main".into(),
        proxy: String::new(),
        net: crate::net::Net::Internet,
        method: method.to_string(),
        path: format!("{route} (refused: not relayed)"),
        status: 403,
    });
}

/// RAM a server may use, from its `max_memory` ("8G", "4096M").
pub fn memory_bytes(m: &str) -> u64 {
    let m = m.trim();
    let (num, unit) = m.split_at(m.find(|c: char| !c.is_ascii_digit()).unwrap_or(m.len()));
    let n: u64 = num.parse().unwrap_or(0);
    match unit.trim().to_ascii_lowercase().as_str() {
        "g" | "gb" => n << 30,
        "m" | "mb" => n << 20,
        "k" | "kb" => n << 10,
        "t" | "tb" => n << 40,
        "" => n,
        _ => 0,
    }
}

/// The node's server list (when it changes, at most once a second) and its
/// changes, as lines of JSON: what the main merges into its own list.
async fn serve_live(app: Arc<App>, mut s: Stream) {
    let mut snap_rx = app.snapshot.subscribe();
    let mut change_rx = app.changes.subscribe();
    let mut sent_rev = app.changes.rev();
    let mut last = Vec::new();
    // Sent even when it didn't change, so a quiet node still proves it's there.
    let mut every = tokio::time::interval(Duration::from_secs(30));
    every.tick().await;
    loop {
        let snap = snap_rx.borrow_and_update().clone();
        let line = json!({ "t": "snap", "servers": snap.servers, "host": snap.host }).to_string().into_bytes();
        if line != last {
            let mut out = line.clone();
            out.push(b'\n');
            if s.tx.send(&out).await.is_err() {
                return;
            }
            last = line;
        }
        tokio::select! {
            c = snap_rx.changed() => if c.is_err() { return },
            c = change_rx.changed() => {
                if c.is_err() { return }
                change_rx.borrow_and_update();
                let list = app.changes.since(sent_rev);
                sent_rev = app.changes.rev();
                let lines: Vec<Value> = match list {
                    Some(l) => l.into_iter().map(|c| json!({ "t": "changed", "server": c.server, "areas": c.areas, "by": c.by })).collect(),
                    None => vec![json!({ "t": "changed", "server": null, "areas": ["*"] })],
                };
                for l in lines {
                    let mut out = l.to_string().into_bytes();
                    out.push(b'\n');
                    if s.tx.send(&out).await.is_err() { return }
                }
            }
            _ = every.tick() => last.clear(),
        }
    }
}

/// One server's console: everything first, then new lines every 150 ms.
async fn serve_console(app: Arc<App>, head: Head, mut s: Stream) {
    // Reading a console is Look; the role still has to be one this node knows.
    if Role::parse(&head.role).is_none_or(|r| r == Role::Manager) {
        s.tx.reset(code::REFUSED);
        return;
    }
    let mut seen = u64::MAX;
    let mut tick = tokio::time::interval(Duration::from_millis(150));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let Some(e) = app.servers.get(&head.server) else {
            let _ = s.tx.finish().await;
            return;
        };
        let (lines, total) = e.server.console_since(seen);
        let reset = total < seen || seen == u64::MAX;
        if lines.is_empty() && !reset {
            continue;
        }
        seen = total;
        let mut out = json!({ "lines": lines, "reset": reset }).to_string().into_bytes();
        out.push(b'\n');
        if s.tx.send(&out).await.is_err() {
            return;
        }
    }
}

/// Runs `f` as a request relayed from a main (tests).
#[cfg(test)]
pub(crate) async fn as_relayed<F: std::future::Future>(who: Who, f: F) -> F::Output {
    RELAYED.scope(who, f).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_targets() {
        assert_eq!(split("ab12:tnp"), Some(("ab12", "tnp")));
        assert_eq!(split("tnp"), None);
        assert_eq!(split(":tnp"), None);
        assert_eq!(split("a:b:c"), None);
        let t = target("/api/v1/servers/ab12:tnp/power", None).unwrap();
        assert_eq!((t.node.as_str(), t.path.as_str()), ("ab12", "/api/v1/servers/tnp/power"));
        let t = target("/api/v1/fs/ab12%3Atnp/list", Some("path=config")).unwrap();
        assert_eq!(t.path, "/api/v1/fs/tnp/list?path=config");
        assert_eq!(t.route, "/api/v1/fs/tnp/list");
        assert_eq!(target("/api/v1/servers/tnp/power", None), None);
        let t = target("/api/v1/servers/create", Some("node=ab12")).unwrap();
        assert_eq!((t.node.as_str(), t.path.as_str()), ("ab12", "/api/v1/servers/create"));
        assert!(target("/api/v1/servers/create", Some("node=ab12&node=cd34")).is_none());
        assert!(target("/api/v1/servers/create", None).is_none());
        let t = target("/api/v1/jobs/ab12:7/cancel", None).unwrap();
        assert_eq!(t.path, "/api/v1/jobs/7/cancel");
    }

    #[test]
    fn the_allowlist() {
        let (g, p, put, d) = (Method::GET, Method::POST, Method::PUT, Method::DELETE);
        for (m, r) in [
            (&g, "/api/v1/servers/tnp"),
            (&p, "/api/v1/servers/tnp/power"),
            (&p, "/api/v1/servers/tnp/command"),
            (&g, "/api/v1/servers/tnp/console"),
            (&p, "/api/v1/servers/tnp/mods/toggle"),
            (&d, "/api/v1/servers/tnp/backups/a.zip"),
            (&g, "/api/v1/fs/tnp/list"),
            (&put, "/api/v1/fs/tnp/text"),
            (&put, "/api/v1/fs/tnp/uploads/u1"),
            (&p, "/api/v1/fs/tnp/trash/restore"),
            (&p, "/api/v1/servers/create"),
            (&g, "/api/v1/jobs/7"),
            (&p, "/api/v1/jobs/7/cancel"),
        ] {
            assert!(allowed(m, r), "{m} {r}");
        }
        for (m, r) in [
            (&g, "/api/v1/users"),
            (&p, "/api/v1/tokens"),
            (&g, "/api/v1/settings/certificate"),
            (&p, "/api/v1/settings/panel-folder/move"),
            (&g, "/api/v1/host/browse"),
            (&p, "/api/v1/panel/update/now"),
            (&g, "/api/v1/nodes"),
            (&g, "/api/v1/this-node"),
            (&p, "/api/v1/servers/import-folder"),
            (&p, "/api/v1/servers/rescan"),
            (&p, "/api/v1/servers/tnp/clone"),
            (&p, "/api/v1/servers/tnp/template"),
            (&p, "/api/v1/servers/tnp/backups/remote/bring"),
            (&put, "/api/v1/servers/tnp/backups/copies"),
            (&p, "/api/v1/servers/tnp/logs/share"),
            (&p, "/api/v1/fs/tnp/link"),
            (&p, "/api/v1/fs/tnp/fetch"),
            (&g, "/api/v1/fs/tnp/../../users"),
            (&g, "/api/v1/servers/tnp/../../../users"),
            (&g, "/api/v1/servers//power"),
            (&g, "/api/v1/ws"),
            (&p, "/api/v1/mcp"),
            (&g, "/api/v1/jobs"),
        ] {
            assert!(!allowed(m, r), "{m} {r}");
        }
    }

    #[test]
    fn paths_that_climb_out_are_refused() {
        assert!(query_paths_ok("/api/v1/fs/tnp/list?path=config/x"));
        assert!(!query_paths_ok("/api/v1/fs/tnp/list?path=../.."));
        assert!(!query_paths_ok("/api/v1/fs/tnp/list?path=..%2F..%2Fusers"));
        assert!(!query_paths_ok("/api/v1/fs/tnp/list?path=%2Fetc"));
        assert!(!query_paths_ok("/api/v1/fs/tnp/list?path=C:%5CWindows"));
        assert!(!query_paths_ok("/api/v1/fs/tnp/list?path=a%5C..%5C..%5Cb"));
        assert!(body_paths_ok(br#"{"ops":[{"op":"move","from":"a","to":"b/c"}]}"#));
        assert!(!body_paths_ok(br#"{"ops":[{"op":"move","from":"a","to":"../../b"}]}"#));
        assert!(!body_paths_ok(br#"{"path":"/etc/passwd"}"#));
        assert!(body_paths_ok(b"\x00\x01 not json"));
    }

    #[test]
    fn roles_and_ceilings() {
        assert_eq!(ceiling_role("look"), Some(Role::Viewer));
        assert_eq!(ceiling_role("operate"), Some(Role::Operator));
        assert_eq!(ceiling_role("manage"), Some(Role::Admin));
        for bad in ["", "Manage", "admin", "manager", "all"] {
            assert_eq!(ceiling_role(bad), None, "{bad}");
        }
        assert_eq!(role_on_node(&Who::Owner), Role::Admin);
        let m = Who::User { id: "u".into(), name: "u".into(), role: Role::Manager, servers: vec![] };
        assert_eq!(role_on_node(&m), Role::Admin, "never owner-level on a node");
        assert_eq!(cap(Role::Admin, Role::Viewer), Role::Viewer);
        assert_eq!(cap(Role::Operator, Role::Admin), Role::Operator);
        assert_eq!(memory_bytes("8G"), 8 << 30);
        assert_eq!(memory_bytes("4096M"), 4096 << 20);
        assert_eq!(memory_bytes("x"), 0);
    }

    #[test]
    fn job_ids_carry_the_node() {
        let b = namespace_answer("ab12", "/api/v1/servers/create", br#"{"job":7}"#.to_vec());
        assert_eq!(serde_json::from_slice::<Value>(&b).unwrap()["job"], "ab12:7");
        let b = namespace_answer("ab12", "/api/v1/jobs/7", br#"{"done":true,"result":{"id":"new","path":"/x"}}"#.to_vec());
        assert_eq!(serde_json::from_slice::<Value>(&b).unwrap()["result"]["id"], "ab12:new");
    }
}
