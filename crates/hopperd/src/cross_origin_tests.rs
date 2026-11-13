//! End to end through the real router: a page on another origin signs in
//! with a bearer session, opens the live feed with a ticket and downloads
//! with a one-use link (design/multi-panel.md, B1). One test per item of
//! the threat list in that file, where it can be tested.

use crate::App;
use crate::auth::{Role, Who};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tower::ServiceExt as _;

const LISTED: &str = "http://fox:8790";
const LISTED_2: &str = "http://tail.example.ts.net:8790";
const UNLISTED: &str = "http://evil.example:8790";
const HOST: &str = "rig:8791";
const OWN: &str = "http://rig:8791";
const PASSWORD: &str = "password1";

struct T {
    app: Arc<App>,
    dir: PathBuf,
    server: String,
}

impl Drop for T {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn panel() -> T {
    let dir = std::env::temp_dir().join(format!("hopperd-b1-{}-{}", std::process::id(), crate::auth::random_hex(4)));
    let srv = dir.join("servers").join("Test Server");
    std::fs::create_dir_all(srv.join("world")).unwrap();
    std::fs::write(srv.join("server.properties"), "motd=test\n").unwrap();
    std::fs::write(srv.join("hello.txt"), "hello from the server").unwrap();
    std::fs::write(srv.join("page.html"), "<script>alert(1)</script>").unwrap();
    std::fs::write(srv.join("world").join("level.dat"), "level").unwrap();
    std::fs::write(dir.join("outside.txt"), "not yours").unwrap();
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let mut cfg = crate::config::Config::default();
    cfg.servers_root = Some(dir.join("servers"));
    cfg.web_origins = vec![LISTED.into(), LISTED_2.into()];
    cfg.discovery = false;
    let auth = crate::auth::Auth::load(&data).unwrap();
    auth.setup(&auth.setup_code().unwrap(), PASSWORD, "192.168.50.2".parse().unwrap()).unwrap();
    let (snapshot, _) = tokio::sync::watch::channel(Arc::new(crate::Snapshot::default()));
    let app = Arc::new(App {
        cfg: RwLock::new(cfg),
        data_dir: data.clone(),
        boot_dir: data.clone(),
        auth,
        servers: crate::servers::Servers::new(),
        host: crate::host::start(None),
        addresses: RwLock::new(Vec::new()),
        snapshot,
        refresh: tokio::sync::Notify::new(),
        jobs: Default::default(),
        results: Default::default(),
        changes: Default::default(),
        grants: Default::default(),
        router: Default::default(),
        scheduler: Default::default(),
        limiter: Default::default(),
        audit: crate::security::Audit::new(&data),
        tls: crate::TlsInfo { fingerprint: "AA:BB".into(), ca_pem: String::new() },
        acme: crate::acme::Acme::new(&data),
        sftp_fingerprint: None,
    });
    app.rescan();
    let server = app.servers.list().first().expect("the test server").id.clone();
    T { app, dir, server }
}

struct Call<'a> {
    method: &'a str,
    path: String,
    headers: Vec<(&'a str, String)>,
    body: Option<Value>,
    ip: &'a str,
}

fn call<'a>(method: &'a str, path: impl Into<String>) -> Call<'a> {
    Call { method, path: path.into(), headers: vec![("host", HOST.into())], body: None, ip: "192.168.50.21" }
}

impl<'a> Call<'a> {
    fn h(mut self, k: &'a str, v: impl Into<String>) -> Self {
        if k == "host" {
            self.headers.retain(|(n, _)| *n != "host");
        }
        self.headers.push((k, v.into()));
        self
    }
    fn origin(self, o: &str) -> Self {
        self.h("origin", o)
    }
    fn bearer(self, t: &str) -> Self {
        self.h("authorization", format!("Bearer {t}"))
    }
    fn json(mut self, v: Value) -> Self {
        self.body = Some(v);
        self
    }
    fn from(mut self, ip: &'a str) -> Self {
        self.ip = ip;
        self
    }
    async fn send(self, t: &T) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut b = Request::builder().method(self.method).uri(&self.path);
        for (k, v) in &self.headers {
            b = b.header(*k, v);
        }
        let body = match &self.body {
            Some(v) => {
                b = b.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let mut req = b.body(body).unwrap();
        let addr: SocketAddr = format!("{}:50000", self.ip).parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(addr));
        let res = crate::api::router(t.app.clone()).oneshot(req).await.unwrap();
        let (parts, body) = res.into_parts();
        let bytes = axum::body::to_bytes(body, 64 * 1024 * 1024).await.unwrap().to_vec();
        (parts.status, parts.headers, bytes)
    }
}

fn body(b: &[u8]) -> Value {
    serde_json::from_slice(b).unwrap_or(Value::Null)
}

fn no_cors(h: &HeaderMap) -> bool {
    h.keys().all(|k| !k.as_str().starts_with("access-control-"))
}

/// Signs a listed page in; returns its bearer token.
async fn remote_login(t: &T, origin: &str) -> String {
    let (s, h, b) = call("POST", "/api/v1/auth/login").origin(origin).json(json!({ "password": PASSWORD, "bearer": true })).send(t).await;
    assert_eq!(s, StatusCode::OK, "{}", String::from_utf8_lossy(&b));
    assert!(!h.contains_key("set-cookie"), "a bearer sign-in sets no cookie");
    body(&b)["token"].as_str().unwrap().to_string()
}

async fn cookie_login(t: &T) -> String {
    let (s, h, _) = call("POST", "/api/v1/auth/login").origin(OWN).json(json!({ "password": PASSWORD })).send(t).await;
    assert_eq!(s, StatusCode::OK);
    let c = h["set-cookie"].to_str().unwrap();
    assert!(c.contains("HttpOnly") && c.contains("SameSite=Strict"), "{c}");
    c.split(';').next().unwrap().strip_prefix("hopperd_session=").unwrap().to_string()
}

// ---- threats from an unlisted origin --------------------------------------------

/// An unlisted page can't sign in, and nothing it gets back is readable
/// (no CORS headers on any answer, refusals included).
#[tokio::test]
async fn unlisted_page_gets_nothing_usable() {
    let t = panel();
    let (s, h, _) = call("POST", "/api/v1/auth/login").origin(UNLISTED).json(json!({ "password": PASSWORD, "bearer": true })).send(&t).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(no_cors(&h));
    // A plain sign-in from that page works on the server's side (same as
    // before B1), but its cookie is SameSite=Strict and the answer is
    // unreadable; a token from it never comes back in a body.
    let (_, h, b) = call("POST", "/api/v1/auth/login").origin(UNLISTED).json(json!({ "password": PASSWORD })).send(&t).await;
    assert!(no_cors(&h));
    assert!(body(&b).get("token").is_none());
    for (m, p) in [("GET", "/api/v1/auth/me"), ("GET", "/api/v1/servers"), ("POST", "/api/v1/ws/ticket"), ("GET", "/api/v1/link?k=x"), ("GET", "/api/v1/hello")] {
        let (_, h, _) = call(m, p).origin(UNLISTED).send(&t).await;
        assert!(no_cors(&h), "{m} {p}");
    }
    // Its preflight is refused before anything runs.
    let (s, h, _) = call("OPTIONS", "/api/v1/auth/login").origin(UNLISTED).h("access-control-request-method", "POST").send(&t).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(no_cors(&h));
}

/// A listed page's token used from an unlisted page (or with no Origin, or
/// from another listed page) is refused: the session is bound to its page.
#[tokio::test]
async fn remote_session_is_bound_to_its_page() {
    let t = panel();
    let tok = remote_login(&t, LISTED).await;
    let me = |o: Option<&'static str>| {
        let c = call("GET", "/api/v1/auth/me").bearer(&tok);
        match o {
            Some(o) => c.origin(o),
            None => c,
        }
    };
    assert_eq!(me(Some(LISTED)).send(&t).await.0, StatusCode::OK);
    assert_eq!(me(None).send(&t).await.0, StatusCode::UNAUTHORIZED, "a script without Origin");
    assert_eq!(me(Some(UNLISTED)).send(&t).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(me(Some(LISTED_2)).send(&t).await.0, StatusCode::UNAUTHORIZED, "another listed page");
    assert_eq!(me(Some(OWN)).send(&t).await.0, StatusCode::UNAUTHORIZED);
    // Not as a cookie either (and a listed page's cookie is dropped anyway).
    let (s, _, _) = call("GET", "/api/v1/auth/me").h("cookie", format!("hopperd_session={tok}")).origin(OWN).send(&t).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // Not for SFTP or anything else that checks a bare token.
    assert!(t.app.auth.check(&tok).is_none());
    assert!(t.app.auth.sftp_login("owner", &tok, "192.168.50.21".parse().unwrap(), true).is_err());
}

/// Bearer sessions only for listed pages, never for the panel's own page or
/// a client without Origin; the cookie sign-in is unchanged.
#[tokio::test]
async fn bearer_sign_in_only_for_listed_pages() {
    let t = panel();
    for o in [None, Some(OWN), Some(UNLISTED), Some("null")] {
        let mut c = call("POST", "/api/v1/auth/login").json(json!({ "password": PASSWORD, "bearer": true }));
        if let Some(o) = o {
            c = c.origin(o);
        }
        let (s, _, b) = c.send(&t).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{o:?}");
        assert!(body(&b).get("token").is_none());
    }
    // Refused before the password is looked at: no try is used up.
    for _ in 0..8 {
        call("POST", "/api/v1/auth/login").origin(UNLISTED).json(json!({ "password": "wrong", "bearer": true })).send(&t).await;
    }
    remote_login(&t, LISTED).await;
    // The panel's own page still gets its cookie, never a token in the body.
    let (s, h, b) = call("POST", "/api/v1/auth/login").origin(OWN).json(json!({ "password": PASSWORD })).send(&t).await;
    assert_eq!(s, StatusCode::OK);
    assert!(h.contains_key("set-cookie") && body(&b).get("token").is_none());
}

// ---- refusals a listed page can read --------------------------------------------

#[tokio::test]
async fn refusals_carry_cors_for_listed_pages() {
    let t = panel();
    // 401: not signed in.
    let (s, h, b) = call("GET", "/api/v1/auth/me").origin(LISTED).send(&t).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(h["access-control-allow-origin"], LISTED);
    assert!(!h.contains_key("access-control-allow-credentials"));
    assert_eq!(body(&b)["error"], "Sign in first.");
    // 401: wrong password, with the two-factor hints.
    let (s, h, b) = call("POST", "/api/v1/auth/login").origin(LISTED).json(json!({ "password": "wrong", "bearer": true })).send(&t).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(h["access-control-allow-origin"], LISTED);
    assert!(body(&b)["need_code"].is_boolean());
    // 403: a viewer starting a server, and managing access from another page.
    t.app.auth.user_create("viewer", "viewerpass", Role::Viewer, vec![]).unwrap();
    let (_, _, b) = call("POST", "/api/v1/auth/login").origin(LISTED).json(json!({ "name": "viewer", "password": "viewerpass", "bearer": true })).send(&t).await;
    let vt = body(&b)["token"].as_str().unwrap().to_string();
    let (s, h, b) = call("POST", format!("/api/v1/servers/{}/power", t.server)).origin(LISTED).bearer(&vt).json(json!({ "action": "start" })).send(&t).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(h["access-control-allow-origin"], LISTED);
    assert_eq!(body(&b)["error"], "Your account can only look.");
    let tok = remote_login(&t, LISTED).await;
    let (s, h, b) = call("POST", "/api/v1/users").origin(LISTED).bearer(&tok).json(json!({ "name": "x", "password": "xxxxxxxx", "role": "viewer" })).send(&t).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(h["access-control-allow-origin"], LISTED);
    assert!(body(&b)["error"].as_str().unwrap().contains("own page"));
    // 429: too many sign-ins from one address.
    let mut last = None;
    for _ in 0..25 {
        last = Some(call("POST", "/api/v1/auth/login").origin(LISTED).from("192.168.50.77").json(json!({ "password": "wrong", "bearer": true })).send(&t).await);
    }
    let (s, h, b) = last.unwrap();
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(h["access-control-allow-origin"], LISTED);
    assert!(body(&b)["error"].as_str().unwrap().contains("Too many"));
    // The same refusals to an unlisted page: no CORS headers on any.
    for (m, p) in [("GET", "/api/v1/auth/me"), ("POST", "/api/v1/users")] {
        let (s, h, _) = call(m, p).origin(UNLISTED).bearer(&tok).json(json!({})).send(&t).await;
        assert!(s.is_client_error());
        assert!(no_cors(&h), "{m} {p}");
    }
    let (s, h, _) = call("POST", "/api/v1/auth/login").origin(UNLISTED).from("192.168.50.77").json(json!({ "password": "x" })).send(&t).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert!(no_cors(&h));
}

// ---- rate limits and lockout --------------------------------------------------------

/// Cross-origin sign-in hits the same tight limit and lockout as any other;
/// preflights never count as tries.
#[tokio::test]
async fn sign_in_limits_apply_across_origins() {
    let t = panel();
    for _ in 0..40 {
        let (s, _, _) = call("OPTIONS", "/api/v1/auth/login").origin(LISTED).from("192.168.50.30").h("access-control-request-method", "POST").send(&t).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
    }
    let (s, _, _) = call("POST", "/api/v1/auth/login").origin(LISTED).from("192.168.50.30").json(json!({ "password": PASSWORD, "bearer": true })).send(&t).await;
    assert_eq!(s, StatusCode::OK, "preflights used no sign-in tries");
    // Five wrong passwords from a listed page lock the address out, the
    // right one included, the same as on the panel's own page.
    for _ in 0..5 {
        call("POST", "/api/v1/auth/login").origin(LISTED).from("192.168.50.31").json(json!({ "password": "wrong", "bearer": true })).send(&t).await;
    }
    let (s, _, b) = call("POST", "/api/v1/auth/login").origin(LISTED).from("192.168.50.31").json(json!({ "password": PASSWORD, "bearer": true })).send(&t).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert!(body(&b)["error"].as_str().unwrap().contains("Too many wrong tries"));
    // From the internet, two-factor is required for bearer sign-in too.
    let (s, _, b) = call("POST", "/api/v1/auth/login").origin(LISTED).from("8.8.8.8").json(json!({ "password": PASSWORD, "bearer": true })).send(&t).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(body(&b)["need_two_factor"], true);
}

// ---- audit -----------------------------------------------------------------------

#[tokio::test]
async fn audit_names_the_page_a_sign_in_came_from() {
    let t = panel();
    remote_login(&t, LISTED).await;
    call("POST", "/api/v1/auth/login").origin(LISTED).json(json!({ "password": "wrong", "bearer": true })).send(&t).await;
    let ev = t.app.audit.recent(10);
    let ok = ev.iter().find(|e| e.status == 200 && e.path.starts_with("/api/v1/auth/login")).unwrap();
    assert_eq!(ok.path, format!("/api/v1/auth/login (from {LISTED})"));
    assert_eq!(ok.who, "owner");
    let bad = ev.iter().find(|e| e.status == 401).unwrap();
    assert_eq!(bad.path, format!("/api/v1/auth/login (from {LISTED})"));
    assert_eq!(bad.who, "-");
    // Tickets and links are reads: not in the audit log.
    let tok = remote_login(&t, LISTED).await;
    call("POST", "/api/v1/ws/ticket").origin(LISTED).bearer(&tok).send(&t).await;
    assert!(t.app.audit.recent(50).iter().all(|e| !e.path.contains("ticket")));
}

// ---- a listed but compromised page ---------------------------------------------------

/// What a stolen remote token can't do: manage who gets in, even when sent
/// from a script that makes `Origin` equal `Host` to look like the panel's
/// own page; change the allowed pages at all.
#[tokio::test]
async fn remote_session_cannot_manage_access() {
    let t = panel();
    let tok = remote_login(&t, LISTED).await;
    let tries = [
        ("POST", "/api/v1/users".to_string(), json!({ "name": "evil", "password": "evilpass1", "role": "manager" })),
        ("POST", "/api/v1/tokens".into(), json!({ "name": "backdoor" })),
        ("POST", "/api/v1/auth/2fa/begin".into(), json!({})),
        ("POST", "/api/v1/auth/password".into(), json!({ "current": PASSWORD, "new": "takenover1" })),
        ("PUT", crate::web_access::SETTINGS_PATH.into(), json!({ "origins": [UNLISTED], "manage": true })),
    ];
    for (m, p, b) in &tries {
        let (s, _, _) = call(m, p.clone()).origin(LISTED).bearer(&tok).json(b.clone()).send(&t).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{m} {p}");
        // Host made up to match the page: still that page's session.
        let (s, _, _) = call(m, p.clone()).h("host", "fox:8790").origin(LISTED).bearer(&tok).json(b.clone()).send(&t).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{m} {p} with a made-up Host");
    }
    assert!(t.app.auth.users().is_empty() && t.app.auth.tokens().is_empty());
    assert_eq!(t.app.cfg.read().unwrap().web_origins, [LISTED, LISTED_2]);
    // Day-to-day work goes through.
    let (s, _, _) = call("GET", "/api/v1/servers").origin(LISTED).bearer(&tok).send(&t).await;
    assert_eq!(s, StatusCode::OK);
    // With the owner's switch on, user management works from the page; the
    // allowed-pages list still doesn't.
    t.app.cfg.write().unwrap().web_manage_access = true;
    let (s, _, _) = call("POST", "/api/v1/users").origin(LISTED).bearer(&tok).json(json!({ "name": "sam", "password": "sampass12", "role": "viewer" })).send(&t).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, _) = call("PUT", crate::web_access::SETTINGS_PATH).origin(LISTED).bearer(&tok).json(json!({ "origins": [], "manage": false })).send(&t).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

/// Taking a page off the list signs its sessions out.
#[tokio::test]
async fn removing_a_page_ends_its_sessions() {
    let t = panel();
    let a = remote_login(&t, LISTED).await;
    let b = remote_login(&t, LISTED_2).await;
    let cookie = cookie_login(&t).await;
    let (s, _, _) = call("PUT", crate::web_access::SETTINGS_PATH)
        .origin(OWN)
        .h("cookie", format!("hopperd_session={cookie}"))
        .h("x-hopperd", "1")
        .json(json!({ "origins": [LISTED_2] }))
        .send(&t)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(t.app.auth.check_from(&a, Some(LISTED)).is_none(), "removed page signed out");
    assert!(t.app.auth.check_from(&b, Some(LISTED_2)).is_some());
    assert!(t.app.auth.check(&cookie).is_some(), "the own page's cookie stays");
    let note = &t.app.audit.recent(5)[0].path;
    assert!(note.contains("removed http://fox:8790") && note.contains("signed out 1"), "{note}");
}

/// Sessions list with the browser label, and revoking one.
#[tokio::test]
async fn sessions_listed_and_revoked() {
    let t = panel();
    let tok = remote_login(&t, LISTED).await;
    let cookie = cookie_login(&t).await;
    let own = |c: Call<'static>| c.origin(OWN).h("cookie", format!("hopperd_session={cookie}")).h("x-hopperd", "1");
    let (s, _, b) = own(call("GET", "/api/v1/auth/sessions")).send(&t).await;
    assert_eq!(s, StatusCode::OK);
    let list = body(&b);
    let remote = list.as_array().unwrap().iter().find(|x| x["kind"] == "browser").unwrap().clone();
    assert_eq!(remote["origin"], LISTED);
    assert_eq!(remote["name"], "owner");
    assert!(remote["expires"].as_u64().unwrap() <= crate::security::now() + crate::auth::REMOTE_IDLE + 1);
    assert!(list.as_array().unwrap().iter().any(|x| x["kind"] == "cookie" && x["current"] == true));
    let id = remote["id"].as_str().unwrap();
    // The remote page itself may not end other sessions (who gets in)...
    let (s, _, _) = call("DELETE", format!("/api/v1/auth/sessions/{id}")).origin(LISTED).bearer(&tok).send(&t).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // ...the panel's own page can.
    let (s, _, _) = own(call("DELETE", format!("/api/v1/auth/sessions/{id}"))).send(&t).await;
    assert_eq!(s, StatusCode::OK);
    let (s, h, _) = call("GET", "/api/v1/auth/me").origin(LISTED).bearer(&tok).send(&t).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(h["access-control-allow-origin"], LISTED, "the page can show Sign in again");
    // A remote page can still sign itself out.
    let tok = remote_login(&t, LISTED).await;
    let (s, _, _) = call("POST", "/api/v1/auth/logout").origin(LISTED).bearer(&tok).send(&t).await;
    assert_eq!(s, StatusCode::OK);
    assert!(t.app.auth.check_from(&tok, Some(LISTED)).is_none());
    // A user sees only their own sessions and can't end the owner's.
    t.app.auth.user_create("sam", "sampass12", Role::Admin, vec![]).unwrap();
    let (_, h, _) = call("POST", "/api/v1/auth/login").origin(OWN).json(json!({ "name": "sam", "password": "sampass12" })).send(&t).await;
    let sam = h["set-cookie"].to_str().unwrap().split(';').next().unwrap().strip_prefix("hopperd_session=").unwrap().to_string();
    let as_sam = |c: Call<'static>| c.origin(OWN).h("cookie", format!("hopperd_session={sam}")).h("x-hopperd", "1");
    let (_, _, b) = as_sam(call("GET", "/api/v1/auth/sessions")).send(&t).await;
    assert!(body(&b).as_array().unwrap().iter().all(|x| x["name"] == "sam"));
    let owner_id = crate::auth::Auth::session_id(&cookie);
    let (s, _, _) = as_sam(call("DELETE", format!("/api/v1/auth/sessions/{owner_id}"))).send(&t).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(t.app.auth.check(&cookie).is_some());
}

// ---- live tickets ----------------------------------------------------------------

async fn ticket(t: &T, tok: &str, origin: &str) -> String {
    let (s, _, b) = call("POST", "/api/v1/ws/ticket").origin(origin).bearer(tok).send(t).await;
    assert_eq!(s, StatusCode::OK, "{}", String::from_utf8_lossy(&b));
    let v = body(&b);
    assert_eq!(v["protocols"][0], "hopperd");
    v["ticket"].as_str().unwrap().to_string()
}

fn ws_headers(origin: &str, ticket: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert("host", HOST.parse().unwrap());
    h.insert("origin", origin.parse().unwrap());
    h.insert("sec-websocket-protocol", format!("hopperd, hopperd-ticket.{ticket}").parse().unwrap());
    h
}

#[tokio::test]
async fn tickets_one_use_bound_and_expiring() {
    let t = panel();
    let tok = remote_login(&t, LISTED).await;
    // Only for listed pages.
    for o in [OWN, UNLISTED] {
        let (s, _, _) = call("POST", "/api/v1/ws/ticket").origin(o).bearer(&tok).send(&t).await;
        assert_ne!(s, StatusCode::OK, "{o}");
    }
    let (s, _, _) = call("POST", "/api/v1/ws/ticket").origin(LISTED).send(&t).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "needs a session");
    // Opens once, from the page that asked.
    let tk = ticket(&t, &tok, LISTED).await;
    assert_eq!(crate::api::ws_allowed(&t.app, &ws_headers(LISTED, &tk)), Some(Who::Owner));
    assert_eq!(crate::api::ws_allowed(&t.app, &ws_headers(LISTED, &tk)), None, "single use");
    // From another page, even a listed one: refused, and used up.
    let tk = ticket(&t, &tok, LISTED).await;
    assert_eq!(crate::api::ws_allowed(&t.app, &ws_headers(LISTED_2, &tk)), None);
    assert_eq!(crate::api::ws_allowed(&t.app, &ws_headers(LISTED, &tk)), None, "burnt by the wrong try");
    let tk = ticket(&t, &tok, LISTED).await;
    assert_eq!(crate::api::ws_allowed(&t.app, &ws_headers(UNLISTED, &tk)), None);
    let mut no_origin = ws_headers(LISTED, &ticket(&t, &tok, LISTED).await);
    no_origin.remove("origin");
    assert_eq!(crate::api::ws_allowed(&t.app, &no_origin), None);
    // Expired after 30 seconds.
    let tk = ticket(&t, &tok, LISTED).await;
    t.app.grants.age(&tk, crate::grants::TICKET_TTL);
    assert_eq!(crate::api::ws_allowed(&t.app, &ws_headers(LISTED, &tk)), None);
    // Signed out after asking: the ticket dies with the session.
    let tk = ticket(&t, &tok, LISTED).await;
    t.app.auth.logout(&tok);
    assert_eq!(crate::api::ws_allowed(&t.app, &ws_headers(LISTED, &tk)), None);
    // A file link is no ticket.
    let tok = remote_login(&t, LISTED).await;
    let (_, _, b) = call("POST", format!("/api/v1/fs/{}/link", t.server)).origin(LISTED).bearer(&tok).json(json!({ "path": "hello.txt" })).send(&t).await;
    let k = body(&b)["url"].as_str().unwrap().rsplit_once("k=").unwrap().1.to_string();
    assert_eq!(crate::api::ws_allowed(&t.app, &ws_headers(LISTED, &k)), None);
    // The role is read when the socket opens: a viewer is a viewer.
    t.app.auth.user_create("vic", "vicpass12", Role::Viewer, vec!["other".into()]).unwrap();
    let (_, _, b) = call("POST", "/api/v1/auth/login").origin(LISTED).json(json!({ "name": "vic", "password": "vicpass12", "bearer": true })).send(&t).await;
    let vt = body(&b)["token"].as_str().unwrap().to_string();
    let tk = ticket(&t, &vt, LISTED).await;
    let who = crate::api::ws_allowed(&t.app, &ws_headers(LISTED, &tk)).unwrap();
    assert!(!who.sees(&t.server), "server scope still applies on the socket");
    // The cookie path is unchanged: own page only.
    let cookie = cookie_login(&t).await;
    let mut h = HeaderMap::new();
    h.insert("host", HOST.parse().unwrap());
    h.insert("cookie", format!("hopperd_session={cookie}").parse().unwrap());
    h.insert("origin", OWN.parse().unwrap());
    assert_eq!(crate::api::ws_allowed(&t.app, &h), Some(Who::Owner));
    h.insert("origin", LISTED.parse().unwrap());
    assert_eq!(crate::api::ws_allowed(&t.app, &h), None, "cookie from another page");
}

// ---- file links --------------------------------------------------------------------

async fn link(t: &T, tok: &str, b: Value) -> (StatusCode, String) {
    let (s, _, r) = call("POST", format!("/api/v1/fs/{}/link", t.server)).origin(LISTED).bearer(tok).json(b).send(t).await;
    (s, body(&r)["url"].as_str().unwrap_or("").to_string())
}

#[tokio::test]
async fn file_links_one_use_bound_and_expiring() {
    let t = panel();
    let tok = remote_login(&t, LISTED).await;
    let (s, url) = link(&t, &tok, json!({ "path": "hello.txt" })).await;
    assert_eq!(s, StatusCode::OK);
    assert!(url.starts_with("/api/v1/link?k="), "the secret is in the query, never the path");
    // A navigation sends no Origin and no token: the link alone is enough, once.
    let (s, h, b) = call("GET", url.clone()).send(&t).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b, b"hello from the server");
    assert!(h["content-disposition"].to_str().unwrap().starts_with("attachment"));
    assert_eq!(h["x-content-type-options"], "nosniff");
    let (s, _, b) = call("GET", url).send(&t).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "single use");
    assert!(body(&b)["error"].as_str().unwrap().contains("expired or was already used"));
    // Expires after 60 seconds.
    let (_, url) = link(&t, &tok, json!({ "path": "hello.txt" })).await;
    t.app.grants.age(url.rsplit_once("k=").unwrap().1, crate::grants::LINK_TTL);
    assert_eq!(call("GET", url).send(&t).await.0, StatusCode::UNAUTHORIZED);
    // Path checks when asking: nothing outside the server folder.
    for p in ["../outside.txt", "..\\outside.txt", "/etc/passwd", "C:\\Windows\\win.ini", "world/../../outside.txt"] {
        let (s, _) = link(&t, &tok, json!({ "path": p })).await;
        assert!(s.is_client_error(), "{p}: {s}");
        let (s, _) = link(&t, &tok, json!({ "paths": [p], "zip": true })).await;
        assert!(s.is_client_error(), "zip {p}: {s}");
    }
    // One file, or a zip; a folder only as a zip.
    assert_eq!(link(&t, &tok, json!({ "path": "world" })).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(link(&t, &tok, json!({ "paths": ["hello.txt", "page.html"] })).await.0, StatusCode::BAD_REQUEST);
    let (s, url) = link(&t, &tok, json!({ "paths": ["world", "hello.txt"], "zip": true })).await;
    assert_eq!(s, StatusCode::OK);
    let (s, h, b) = call("GET", url).send(&t).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h["content-type"], "application/zip");
    assert!(b.starts_with(b"PK"));
    // A preview of an HTML file is shown as text, sandboxed.
    let (_, url) = link(&t, &tok, json!({ "path": "page.html", "inline": true })).await;
    let (_, h, _) = call("GET", url).send(&t).await;
    assert_eq!(h["content-type"], "text/plain; charset=utf-8");
    assert!(h["content-security-policy"].to_str().unwrap().contains("sandbox"));
    // Signed out after asking: the link dies with the session.
    let (_, url) = link(&t, &tok, json!({ "path": "hello.txt" })).await;
    t.app.auth.logout(&tok);
    assert_eq!(call("GET", url).send(&t).await.0, StatusCode::UNAUTHORIZED);
    // A ticket is no file link.
    let tok = remote_login(&t, LISTED).await;
    let tk = ticket(&t, &tok, LISTED).await;
    assert_eq!(call("GET", format!("/api/v1/link?k={tk}")).send(&t).await.0, StatusCode::UNAUTHORIZED);
    // Someone else's server: refused when asking.
    let (s, _, _) = call("POST", "/api/v1/fs/nope/link").origin(LISTED).bearer(&tok).json(json!({ "path": "hello.txt" })).send(&t).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// Server scope and roles are checked when asking and again when the link
/// is used (access taken away in between).
#[tokio::test]
async fn file_links_follow_server_access() {
    let t = panel();
    let u = t.app.auth.user_create("op", "oppass123", Role::Operator, vec![t.server.clone()]).unwrap();
    let (_, _, b) = call("POST", "/api/v1/auth/login").origin(LISTED).json(json!({ "name": "op", "password": "oppass123", "bearer": true })).send(&t).await;
    let tok = body(&b)["token"].as_str().unwrap().to_string();
    // An operator may read files (downloads are reads), so may ask.
    let (s, url) = link(&t, &tok, json!({ "path": "hello.txt" })).await;
    assert_eq!(s, StatusCode::OK);
    // Their access to this server is taken away before they use it.
    t.app.auth.user_update(&u.id, "op", Role::Operator, vec!["other".into()], None).unwrap();
    let (s, _, _) = call("GET", url).send(&t).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = link(&t, &tok, json!({ "path": "hello.txt" })).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "and can't ask for a new one");
    // A read-only API token scoped to another server can't ask either.
    let (api, _) = t.app.auth.token_create("ro", true, vec!["other".into()], None).unwrap();
    let (s, _, _) = call("POST", format!("/api/v1/fs/{}/link", t.server)).bearer(&api).json(json!({ "path": "hello.txt" })).send(&t).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (api, _) = t.app.auth.token_create("ro2", true, vec![], None).unwrap();
    let (s, _, _) = call("POST", format!("/api/v1/fs/{}/link", t.server)).bearer(&api).json(json!({ "path": "hello.txt" })).send(&t).await;
    assert_eq!(s, StatusCode::OK, "asking for a link is a read");
}

// ---- same-origin rules unchanged ------------------------------------------------------

#[tokio::test]
async fn same_origin_cookie_rules_unchanged() {
    let t = panel();
    let cookie = cookie_login(&t).await;
    let c = || call("POST", "/api/v1/servers/rescan").origin(OWN).h("cookie", format!("hopperd_session={cookie}"));
    assert_eq!(c().send(&t).await.0, StatusCode::FORBIDDEN, "no X-Hopperd: CSRF guard");
    assert_eq!(c().h("x-hopperd", "1").send(&t).await.0, StatusCode::OK);
    // The cookie from a listed page is dropped: that page needs its token.
    let (s, h, _) = call("GET", "/api/v1/auth/me").origin(LISTED).h("cookie", format!("hopperd_session={cookie}")).send(&t).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert!(!h.contains_key("access-control-allow-credentials"));
    // The panel's own page gets no CORS headers.
    let (_, h, _) = call("GET", "/api/v1/auth/me").origin(OWN).h("cookie", format!("hopperd_session={cookie}")).send(&t).await;
    assert!(no_cors(&h));
}

/// Browser sessions live 7 days from last use; cookie sessions keep 30.
#[tokio::test]
async fn remote_sessions_live_seven_days() {
    let t = panel();
    let tok = remote_login(&t, LISTED).await;
    let cookie = cookie_login(&t).await;
    t.app.auth.age_sessions(6 * 24 * 3600);
    assert!(t.app.auth.check_from(&tok, Some(LISTED)).is_some(), "used: the 7 days start again");
    t.app.auth.age_sessions(crate::auth::REMOTE_IDLE + 60);
    assert!(t.app.auth.check_from(&tok, Some(LISTED)).is_none());
    assert!(t.app.auth.check(&cookie).is_some(), "a cookie session lasts 30 days");
    assert!(t.app.auth.sessions(true, None).iter().all(|s| s.kind == "cookie"));
}

/// A page signing in in a loop can't pile up sessions.
#[tokio::test]
async fn remote_sessions_are_capped() {
    let t = panel();
    // Spread over addresses so the sign-in limit isn't what stops it.
    let ips: Vec<String> = (0..22).map(|i| format!("192.168.60.{}", i + 1)).collect();
    let mut toks = Vec::new();
    for ip in &ips {
        let (s, _, b) = call("POST", "/api/v1/auth/login").origin(LISTED).from(ip).json(json!({ "password": PASSWORD, "bearer": true })).send(&t).await;
        assert_eq!(s, StatusCode::OK);
        toks.push(body(&b)["token"].as_str().unwrap().to_string());
    }
    let n = t.app.auth.sessions(true, None).iter().filter(|s| s.kind == "browser").count();
    assert_eq!(n, 20);
    assert!(t.app.auth.check_from(&toks[0], Some(LISTED)).is_none(), "the oldest went");
    assert!(t.app.auth.check_from(&toks[21], Some(LISTED)).is_some());
}

/// `hello` advertises the browser-session API only now that it exists.
#[tokio::test]
async fn hello_lists_web_remote() {
    let t = panel();
    let (_, h, b) = call("GET", "/api/v1/hello").origin(LISTED).send(&t).await;
    assert!(body(&b)["capabilities"].as_array().unwrap().iter().any(|c| c == "web.remote"));
    assert_eq!(h["access-control-allow-origin"], LISTED);
}

// ---- found in the B1 security review ----------------------------------------------

/// A link remembers its paths for 60 s, so asking must stay small: a viewer
/// sending one big request (paths padded with `./`, which the folder check
/// skips) must not park megabytes in memory per link, 64 links a session.
#[tokio::test]
async fn file_links_cannot_fill_memory() {
    let t = panel();
    t.app.auth.user_create("vic", "vicpass12", Role::Viewer, vec![]).unwrap();
    let (_, _, b) = call("POST", "/api/v1/auth/login").origin(LISTED).json(json!({ "name": "vic", "password": "vicpass12", "bearer": true })).send(&t).await;
    let vt = body(&b)["token"].as_str().unwrap().to_string();
    let padded = format!("{}hello.txt", "./".repeat(1000));
    let paths: Vec<String> = (0..1000).map(|_| padded.clone()).collect();
    let (s, url) = link(&t, &vt, json!({ "paths": paths, "zip": true })).await;
    assert_ne!(s, StatusCode::OK, "a 2 MB link request is refused");
    assert!(url.is_empty());
    // An ordinary zip of a few files still works, and the padding is gone
    // from what the link keeps.
    let (s, url) = link(&t, &vt, json!({ "paths": [padded, "world"], "zip": true })).await;
    assert_eq!(s, StatusCode::OK);
    let k = url.rsplit_once("k=").unwrap().1;
    let g = t.app.grants.take(k).unwrap();
    let crate::grants::Purpose::File { paths, .. } = g.purpose else { panic!("a file link") };
    assert_eq!(paths, vec!["hello.txt".to_string(), "world".to_string()]);
}

/// An open live feed ends with its session: Revoke, signing out or taking
/// the page off the list must stop the console and server updates too, not
/// only the page's next request.
#[tokio::test]
async fn live_feed_ends_with_its_session() {
    let t = panel();
    let cookie = cookie_login(&t).await;
    let own = |c: Call<'static>| c.origin(OWN).h("cookie", format!("hopperd_session={cookie}")).h("x-hopperd", "1");
    // Revoked from the panel's own page.
    let tok = remote_login(&t, LISTED).await;
    let (who, cred) = crate::api::ws_access(&t.app, &ws_headers(LISTED, &ticket(&t, &tok, LISTED).await)).unwrap();
    assert!(crate::api::live_still_allowed(&t.app, &cred, &who));
    let id = crate::auth::Auth::session_id(&tok);
    assert_eq!(own(call("DELETE", format!("/api/v1/auth/sessions/{id}"))).send(&t).await.0, StatusCode::OK);
    assert!(!crate::api::live_still_allowed(&t.app, &cred, &who), "revoked: the feed closes");
    // The page taken off the list.
    let tok = remote_login(&t, LISTED).await;
    let (who, cred) = crate::api::ws_access(&t.app, &ws_headers(LISTED, &ticket(&t, &tok, LISTED).await)).unwrap();
    let (s, _, _) = own(call("PUT", "/api/v1/settings/web-access")).json(json!({ "origins": [LISTED_2] })).send(&t).await;
    assert_eq!(s, StatusCode::OK);
    assert!(!crate::api::live_still_allowed(&t.app, &cred, &who), "page removed: the feed closes");
    // The panel's own cookie feed, signed out elsewhere.
    let mut h = HeaderMap::new();
    h.insert("host", HOST.parse().unwrap());
    h.insert("cookie", format!("hopperd_session={cookie}").parse().unwrap());
    h.insert("origin", OWN.parse().unwrap());
    let (who, cred) = crate::api::ws_access(&t.app, &h).unwrap();
    assert!(crate::api::live_still_allowed(&t.app, &cred, &who));
    t.app.auth.logout(&cookie);
    assert!(!crate::api::live_still_allowed(&t.app, &cred, &who));
    // A user's role changing ends the old feed (it reconnects as the new role).
    t.app.auth.user_create("sam", "sampass12", Role::Admin, vec![]).unwrap();
    // (Only the second page is still listed here.)
    let (_, _, b) = call("POST", "/api/v1/auth/login").origin(LISTED_2).json(json!({ "name": "sam", "password": "sampass12", "bearer": true })).send(&t).await;
    let st = body(&b)["token"].as_str().unwrap().to_string();
    let (who, cred) = crate::api::ws_access(&t.app, &ws_headers(LISTED_2, &ticket(&t, &st, LISTED_2).await)).unwrap();
    let Who::User { id: uid, .. } = &who else { panic!("a user") };
    t.app.auth.user_update(uid, "sam", Role::Viewer, vec![t.server.clone()], None).unwrap();
    assert!(!crate::api::live_still_allowed(&t.app, &cred, &who), "role changed: the feed closes");
}

/// Changing your password signs out the browser sessions other pages hold
/// (their tokens are readable by the page, so they may have been copied);
/// the session that changed it stays.
#[tokio::test]
async fn password_change_ends_browser_sessions() {
    let t = panel();
    let remote = remote_login(&t, LISTED).await;
    let cookie = cookie_login(&t).await;
    let (s, _, b) = call("POST", "/api/v1/auth/password")
        .origin(OWN)
        .h("cookie", format!("hopperd_session={cookie}"))
        .h("x-hopperd", "1")
        .json(json!({ "current": PASSWORD, "new": "password2" }))
        .send(&t)
        .await;
    assert_eq!(s, StatusCode::OK, "{}", String::from_utf8_lossy(&b));
    assert!(t.app.auth.check_from(&remote, Some(LISTED)).is_none(), "the copied page token is dead");
    assert!(t.app.auth.check(&cookie).is_some(), "the session that changed it stays");
}

// ---- found in the second look at B1 (low findings) ---------------------------------

/// One account with dozens of sessions (two listed pages, 20 each) asking
/// for tickets in a loop can't use up the shared pool: the owner's own
/// newest request still works (its oldest grants go), and another account
/// still gets its live feed and its file links.
#[tokio::test]
async fn one_account_cannot_fill_the_grant_pool() {
    let t = panel();
    let mut sessions = Vec::new();
    for (n, page) in [LISTED, LISTED_2].iter().enumerate() {
        for i in 0..20 {
            // An address each, so the sign-in and request limits aren't what stops it.
            let ip = format!("192.168.{}.{}", 70 + n, i + 1);
            let (s, _, b) = call("POST", "/api/v1/auth/login").origin(page).from(&ip).json(json!({ "password": PASSWORD, "bearer": true })).send(&t).await;
            assert_eq!(s, StatusCode::OK);
            sessions.push((body(&b)["token"].as_str().unwrap().to_string(), *page, ip));
        }
    }
    t.app.auth.user_create("vic", "vicpass12", Role::Viewer, vec![]).unwrap();
    let (_, _, b) = call("POST", "/api/v1/auth/login").origin(LISTED).from("192.168.80.1").json(json!({ "name": "vic", "password": "vicpass12", "bearer": true })).send(&t).await;
    let vic = body(&b)["token"].as_str().unwrap().to_string();
    // 40 sessions x 64 tickets = 2560, more than the 2048 the pool holds.
    for (tok, page, ip) in &sessions {
        for _ in 0..64 {
            let (s, _, b) = call("POST", "/api/v1/ws/ticket").origin(page).bearer(tok).from(ip).send(&t).await;
            assert_eq!(s, StatusCode::OK, "the owner is never refused for asking: {}", String::from_utf8_lossy(&b));
        }
    }
    let (s, _, b) = call("POST", "/api/v1/ws/ticket").origin(LISTED).bearer(&vic).from("192.168.80.1").send(&t).await;
    assert_eq!(s, StatusCode::OK, "someone else's live feed: {}", String::from_utf8_lossy(&b));
    let (s, _) = link(&t, &vic, json!({ "path": "hello.txt" })).await;
    assert_eq!(s, StatusCode::OK, "someone else's file link");
}

/// A browser session ends 30 days after sign-in however often it is used,
/// and the session list says when.
#[tokio::test]
async fn browser_sessions_end_after_thirty_days() {
    let t = panel();
    let tok = remote_login(&t, LISTED).await;
    let cookie = cookie_login(&t).await;
    t.app.auth.age_sign_ins(28 * 24 * 3600);
    // Used just now (so 7 days idle would start again), but only 2 days of its 30 are left.
    assert!(t.app.auth.check_from(&tok, Some(LISTED)).is_some());
    let mine = t.app.auth.sessions(true, None);
    let ends = mine.iter().find(|s| s.kind == "browser").unwrap().expires;
    let now = crate::security::now();
    assert!(ends > now && ends <= now + 2 * 24 * 3600 + 5, "ends in {} s", ends.saturating_sub(now));
    t.app.auth.age_sign_ins(3 * 24 * 3600);
    assert!(t.app.auth.check_from(&tok, Some(LISTED)).is_none(), "30 days after sign-in, used or not");
    assert!(t.app.auth.sessions(true, None).iter().all(|s| s.kind == "cookie"));
    assert!(t.app.auth.check(&cookie).is_some(), "cookie sessions keep their own 30 days from last use");
}

/// A sign-in a listed-page check turns away (403, before the password) is
/// still in the audit log with where it came from; long or odd origins are cut.
#[tokio::test]
async fn refused_bearer_sign_in_is_logged_with_its_origin() {
    let t = panel();
    call("POST", "/api/v1/auth/login").origin(UNLISTED).json(json!({ "password": "secret-pass-123", "bearer": true })).send(&t).await;
    let long = format!("http://{}.example", "a".repeat(500));
    call("POST", "/api/v1/auth/login").origin(&long).json(json!({ "password": "secret-pass-123", "bearer": true })).send(&t).await;
    call("POST", "/api/v1/auth/login").json(json!({ "password": "secret-pass-123", "bearer": true })).send(&t).await;
    let ev = t.app.audit.recent(10);
    let paths: Vec<&str> = ev.iter().filter(|e| e.status == 403).map(|e| e.path.as_str()).collect();
    assert_eq!(paths.len(), 3, "{paths:?}");
    assert!(paths.contains(&format!("/api/v1/auth/login (from {UNLISTED})").as_str()), "{paths:?}");
    assert!(paths.iter().any(|p| p.starts_with("/api/v1/auth/login (from http://aaaa") && p.len() < 200), "{paths:?}");
    assert!(paths.contains(&"/api/v1/auth/login (from no Origin)"), "{paths:?}");
    assert!(ev.iter().all(|e| !e.path.contains("secret-pass-123")));
}

async fn user_cookie(t: &T, name: &str, password: &str) -> String {
    let (s, h, _) = call("POST", "/api/v1/auth/login").origin(OWN).json(json!({ "name": name, "password": password })).send(t).await;
    assert_eq!(s, StatusCode::OK);
    h["set-cookie"].to_str().unwrap().split(';').next().unwrap().strip_prefix("hopperd_session=").unwrap().to_string()
}

/// A manager's session list shows their own and the users' sessions, not
/// the owner's or another manager's (IP, page, browser); the owner sees all.
#[tokio::test]
async fn managers_do_not_see_the_owners_sessions() {
    let t = panel();
    t.app.auth.user_create("mia", "miapass12", Role::Manager, vec![]).unwrap();
    t.app.auth.user_create("moe", "moepass12", Role::Manager, vec![]).unwrap();
    t.app.auth.user_create("sam", "sampass12", Role::Admin, vec![]).unwrap();
    let owner = cookie_login(&t).await;
    remote_login(&t, LISTED).await;
    let mia = user_cookie(&t, "mia", "miapass12").await;
    user_cookie(&t, "moe", "moepass12").await;
    user_cookie(&t, "sam", "sampass12").await;
    let names = |c: &str| {
        let c = c.to_string();
        let t = &t;
        async move {
            let (s, _, b) = call("GET", "/api/v1/auth/sessions").origin(OWN).h("cookie", format!("hopperd_session={c}")).send(t).await;
            assert_eq!(s, StatusCode::OK);
            let mut n: Vec<String> = body(&b).as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap().to_string()).collect();
            n.sort();
            n
        }
    };
    assert_eq!(names(&mia).await, vec!["mia", "sam"], "no owner or other manager sessions");
    assert_eq!(names(&owner).await, vec!["mia", "moe", "owner", "owner", "owner", "sam"], "the owner sees all (setup, cookie, page)");
}
