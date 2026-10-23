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

#[derive(Default)]
pub struct Limiter {
    seen: Mutex<HashMap<IpAddr, (Instant, u32, u32)>>,
}

impl Limiter {
    /// Counts a request; false when the address is over its limit.
    pub fn allow(&self, ip: IpAddr, auth: bool) -> bool {
        let mut m = self.seen.lock().unwrap();
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
        e.1 <= LIMIT_ALL && (!auth || e.2 <= LIMIT_AUTH)
    }
}

/// Who a sign-in became, set on the response for the audit log.
#[derive(Clone)]
pub struct SignedIn(pub String);

/// A line of the audit log.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    /// Unix seconds.
    pub at: u64,
    pub who: String,
    pub ip: String,
    pub net: Net,
    pub method: String,
    pub path: String,
    pub status: u16,
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

    /// The newest `limit` events, newest first.
    pub fn recent(&self, limit: usize) -> Vec<Event> {
        let _g = self.file.lock().unwrap();
        let mut out: Vec<Event> = Vec::new();
        for p in [self.path.with_extension("1.log"), self.path.clone()] {
            if let Ok(f) = std::fs::File::open(&p) {
                out.extend(std::io::BufReader::new(f).lines().map_while(Result::ok).filter_map(|l| serde_json::from_str(&l).ok()));
            }
        }
        out.reverse();
        out.truncate(limit);
        out
    }
}

/// The server id a path is about (`/api/v1/servers/{id}/…`, `/api/v1/fs/{id}/…`).
fn server_of(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/v1/servers/").or_else(|| path.strip_prefix("/api/v1/fs/"))?;
    let id = rest.split('/').next()?;
    (!id.is_empty() && !matches!(id, "infos" | "rescan" | "create")).then_some(id)
}

/// What someone could guess at: passwords, codes, setup. These get the
/// tight limit. (Pairing status needs the request's long secret, and a
/// device asks every second while it waits.)
fn is_guessable(path: &str) -> bool {
    matches!(path, "/api/v1/auth/login" | "/api/v1/auth/setup" | "/api/v1/auth/2fa/enable" | "/api/v1/auth/2fa/disable" | "/api/v1/pair/request")
}

/// Why a scoped token or a user may not make this request, if they may not.
/// (Owner-only routes check that themselves.)
pub(crate) fn refusal(who: &crate::auth::Who, method: &Method, path: &str) -> Option<&'static str> {
    use crate::auth::{Role, Who};
    let reading = method == Method::GET || method == Method::HEAD;
    match who {
        Who::Token { read_only, servers, .. } => {
            if *read_only && !reading && path != "/api/v1/mcp" {
                Some("This token can only read.")
            } else if !servers.is_empty() && server_of(path).is_some_and(|id| !servers.iter().any(|s| s == id)) {
                Some("This token isn't for that server.")
            } else {
                None
            }
        }
        Who::User { role, servers, .. } => {
            if server_of(path).is_some_and(|id| !who.sees(id)) {
                return Some("You don't have access to that server.");
            }
            // MCP checks each tool call itself.
            if path == "/api/v1/mcp" {
                return None;
            }
            // Your own sign-in: sign out, password, two-factor.
            if reading || path == "/api/v1/auth/logout" || path == "/api/v1/auth/password" || path.starts_with("/api/v1/auth/2fa/") {
                return None;
            }
            match role {
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
        _ => None,
    }
}

fn deny(status: StatusCode, msg: &str) -> Response {
    (status, axum::Json(serde_json::json!({ "error": msg }))).into_response()
}

pub(crate) fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The middleware.
pub async fn guard(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
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

    if api && !app.limiter.allow(ip, is_guessable(&path)) {
        return deny(StatusCode::TOO_MANY_REQUESTS, "Too many requests. Wait a minute.");
    }

    // Who's asking, for scopes and the log.
    let token = crate::api::token(req.headers()).0;
    let who = if token.is_empty() { None } else { app.auth.check(&token) };
    if let Some(w) = &who {
        if let Some(msg) = refusal(w, &method, &path) {
            app.audit.record(&Event { at: now(), who: w.name(), ip: ip.to_string(), net, method: method.to_string(), path, status: 403 });
            return deny(StatusCode::FORBIDDEN, msg);
        }
    }

    let res = next.run(req).await;

    // Changes, sign-ins and pairing go in the audit log.
    // Reads aren't logged (the UI checks who's signed in all the time).
    let changes = method != Method::GET && method != Method::HEAD;
    if api && changes && path != "/api/v1/pair/status" {
        let status = res.status().as_u16();
        // A sign-in names who it became; a failed one stays anonymous.
        let who_name = who.as_ref().map(|w| w.name()).unwrap_or_else(|| {
            res.extensions().get::<SignedIn>().map(|s| s.0.clone()).unwrap_or_else(|| "-".into())
        });
        app.audit.record(&Event { at: now(), who: who_name, ip: ip.to_string(), net, method: method.to_string(), path, status });
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_ids_in_paths() {
        assert_eq!(server_of("/api/v1/servers/tnp/power"), Some("tnp"));
        assert_eq!(server_of("/api/v1/fs/tnp/list"), Some("tnp"));
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
    }

    #[test]
    fn limits() {
        let l = Limiter::default();
        let ip: IpAddr = "8.8.8.8".parse().unwrap();
        for _ in 0..LIMIT_AUTH {
            assert!(l.allow(ip, true));
        }
        assert!(!l.allow(ip, true), "sign-ins are limited");
        assert!(l.allow("8.8.4.4".parse().unwrap(), true), "per address");
    }

    #[test]
    fn audit_round_trip() {
        let dir = std::env::temp_dir().join(format!("hopperd-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = Audit::new(&dir);
        for i in 0..3 {
            a.record(&Event { at: i, who: "owner".into(), ip: "1.2.3.4".into(), net: Net::Internet, method: "POST".into(), path: format!("/p{i}"), status: 200 });
        }
        let r = a.recent(2);
        assert_eq!(r.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(), ["/p2", "/p1"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
