//! A main and a node over an in-memory tunnel, both through their real
//! routers (design/multi-panel.md, A2): the same requests answer the same
//! way direct and through the main, the node's ceiling is read strictly,
//! paths that climb out of the server folder are refused by the node, and
//! maintenance stops starts.

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::auth::{Role, Who};
use crate::cross_origin_tests::{OWN, T, body, call, cookie_login, panel_with};
use crate::nodes::{Ceiling, Follow};
use crate::tunnel::mux::{self, Config, Side};

const TLS: fn() -> crate::TlsInfo = || crate::TlsInfo { fingerprint: "AA:BB".into(), ca_pem: "-----BEGIN CERTIFICATE-----\nTEST\n-----END CERTIFICATE-----\n".into() };

struct Pair {
    main: T,
    node: T,
    /// The node's id on the main.
    id: String,
    main_cookie: String,
    node_cookie: String,
    /// What the node's node.json says, as the node reads it per stream.
    ceiling: Arc<std::sync::Mutex<Option<Role>>>,
    _feed: Follow,
    _serve: Follow,
    _tunnels: (mux::Tunnel, mux::Handle),
}

impl Pair {
    /// The node's test server as the main names it.
    fn ns(&self) -> String {
        crate::relay::join(&self.id, &self.node.server)
    }
}

/// `main_copy`: the ceiling the main stored; `node_has`: what the node's
/// node.json says (None: unreadable).
async fn pair(main_copy: Ceiling, node_has: Option<Role>) -> Pair {
    let main = panel_with(TLS());
    let node = panel_with(TLS());
    let (_, key, _) = main.app.nodes.make_key(main_copy, "owner");
    let (n, _) = main.app.nodes.use_key(&key, "vm", "10.0.0.9".parse().unwrap(), "").unwrap();
    let (a, b) = mux::mem::pair(None);
    let (m, d) = tokio::join!(mux::start(Side::Main, Config::default(), a.0, a.1), mux::start(Side::Node, Config::default(), b.0, b.1));
    let (m, mut d) = (m.unwrap(), d.unwrap());
    let incoming = std::mem::replace(&mut d.incoming, tokio::sync::mpsc::channel(1).1);
    let cell = Arc::new(std::sync::Mutex::new(node_has));
    let c = cell.clone();
    let _serve = Follow(tokio::spawn(crate::relay::serve(node.app.clone(), Arc::new(move || *c.lock().unwrap()), "rig".into(), incoming)));
    // The node's server list, as its once-a-second loop would make it.
    for t in [&main, &node] {
        let snap = t.app.take_snapshot();
        t.app.snapshot.send_replace(Arc::new(snap));
    }
    let _feed = crate::nodes::attach_for_test(&main.app, &n, m.handle.clone());
    let main_cookie = cookie_login(&main).await;
    let node_cookie = cookie_login(&node).await;
    let id = n.id.clone();
    let handle = d.handle.clone();
    let p = Pair { main, node, id, main_cookie, node_cookie, ceiling: cell, _feed, _serve, _tunnels: (m, handle) };
    // Wait for the live feed's first list.
    for _ in 0..100 {
        if !p.main.app.nodes.servers_for(&Who::Owner).is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    p
}

async fn send(t: &T, cookie: &str, method: &'static str, path: &str, b: Option<Value>) -> (StatusCode, Value) {
    let mut c = call(method, path).origin(OWN).h("cookie", format!("hopperd_session={cookie}")).h("x-hopperd", "1");
    if let Some(b) = b {
        c = c.json(b);
    }
    let (s, _, b) = c.send(t).await;
    (s, body(&b))
}

/// The same requests give the same answers on the node and through the
/// main: the shared handlers run, the main only carries them.
#[tokio::test]
async fn the_same_requests_answer_the_same_through_the_main() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let (local, ns) = (p.node.server.clone(), p.ns());
    let reads = [
        "/api/v1/fs/{}/list?path=",
        "/api/v1/fs/{}/list?path=world",
        "/api/v1/fs/{}/text?path=hello.txt",
        "/api/v1/fs/{}/text?path=server.properties",
        "/api/v1/fs/{}/text?path=missing.txt",
    ];
    for r in reads {
        let direct = send(&p.node, &p.node_cookie, "GET", &r.replace("{}", &local), None).await;
        let relayed = send(&p.main, &p.main_cookie, "GET", &r.replace("{}", &ns), None).await;
        assert_eq!(direct, relayed, "{r}");
    }
    // One length, the true one, on a relayed JSON answer (a browser drops
    // a body whose two lengths disagree).
    let (_, h, raw) = call("GET", format!("/api/v1/fs/{ns}/list?path=")).origin(OWN).h("cookie", format!("hopperd_session={}", p.main_cookie)).send(&p.main).await;
    let lens: Vec<&str> = h.get_all("content-length").iter().filter_map(|v| v.to_str().ok()).collect();
    assert_eq!(lens, vec![raw.len().to_string().as_str()]);
    // A server page: the same server, its own id kept on the node.
    let (s, direct) = send(&p.node, &p.node_cookie, "GET", &format!("/api/v1/servers/{local}"), None).await;
    let (s2, relayed) = send(&p.main, &p.main_cookie, "GET", &format!("/api/v1/servers/{ns}"), None).await;
    assert_eq!((s, s2), (StatusCode::OK, StatusCode::OK));
    assert_eq!(direct["name"], relayed["name"]);
    // A write lands in the node's server folder, and its log names who on
    // which main did it.
    let (s, _) = send(&p.main, &p.main_cookie, "PUT", &format!("/api/v1/fs/{ns}/text"), Some(json!({ "path": "hello.txt", "content": "from the main", "force": true }))).await;
    assert_eq!(s, StatusCode::OK);
    let srv = p.node.dir.join("servers").join("Test Server");
    assert_eq!(std::fs::read_to_string(srv.join("hello.txt")).unwrap(), "from the main");
    let who: Vec<String> = p.node.app.audit.recent(50).into_iter().filter(|e| e.path.contains("/text")).map(|e| format!("{} {}", e.who, e.ip)).collect();
    assert_eq!(who, vec!["Owner via rig (admin) main".to_string()]);
    // The merged list: the node's server under its namespaced id.
    let (_, list) = send(&p.main, &p.main_cookie, "GET", "/api/v1/servers", None).await;
    let ids: Vec<&str> = list.as_array().unwrap().iter().filter_map(|s| s["id"].as_str()).collect();
    assert!(ids.contains(&ns.as_str()) && ids.contains(&p.main.server.as_str()), "{ids:?}");
    let row = list.as_array().unwrap().iter().find(|s| s["id"] == ns.as_str()).unwrap();
    assert_eq!(row["node"], p.id.as_str());
}

/// History on the node names the person and the main.
#[tokio::test]
async fn history_names_who_on_which_main() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let who = Who::Main { main: "rig".into(), user: "mia".into(), role: Role::Operator, lead: false };
    let by = crate::relay::as_relayed(who, async { crate::mods_api::changer(&p.node.app, &Default::default(), &axum::http::Method::POST) }).await;
    assert_eq!(serde_json::to_value(&by).unwrap().to_string().contains("mia via rig"), true, "{:?}", serde_json::to_value(&by));
}

/// A Look node refuses a start even when the main's copy says Manage; the
/// main refuses early when its copy says Look. Reading still works.
#[tokio::test]
async fn a_look_ceiling_refuses_start() {
    let p = pair(Ceiling::Manage, Some(Role::Viewer)).await;
    let ns = p.ns();
    let (s, b) = send(&p.main, &p.main_cookie, "POST", &format!("/api/v1/servers/{ns}/power"), Some(json!({ "action": "start" }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{b}");
    assert!(p.node.app.servers.get(&p.node.server).is_some_and(|e| e.server.state() == hopper_core::hosting::process::ServerState::Stopped));
    let (s, _) = send(&p.main, &p.main_cookie, "PUT", &format!("/api/v1/fs/{ns}/text"), Some(json!({ "path": "hello.txt", "content": "x", "force": true }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = send(&p.main, &p.main_cookie, "GET", &format!("/api/v1/fs/{ns}/text?path=hello.txt"), None).await;
    assert_eq!(s, StatusCode::OK);

    let p = pair(Ceiling::Look, Some(Role::Admin)).await;
    let (s, b) = send(&p.main, &p.main_cookie, "POST", &format!("/api/v1/servers/{}/power", p.ns()), Some(json!({ "action": "start" }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(b["error"].as_str().unwrap().contains("vm lets this panel"), "{b}");
}

/// Lowering the ceiling on the node applies to the connection that is
/// already open (security review, medium).
#[tokio::test]
async fn a_lowered_ceiling_applies_at_once() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let path = format!("/api/v1/fs/{}/text", p.ns());
    let (s, _) = send(&p.main, &p.main_cookie, "PUT", &path, Some(json!({ "path": "hello.txt", "content": "a", "force": true }))).await;
    assert_eq!(s, StatusCode::OK);
    *p.ceiling.lock().unwrap() = Some(Role::Viewer);
    let (s, _) = send(&p.main, &p.main_cookie, "PUT", &path, Some(json!({ "path": "hello.txt", "content": "b", "force": true }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    *p.ceiling.lock().unwrap() = None;
    let (s, _) = send(&p.main, &p.main_cookie, "GET", &format!("/api/v1/fs/{}/list?path=", p.ns()), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(std::fs::read_to_string(p.node.dir.join("servers").join("Test Server").join("hello.txt")).unwrap(), "a");
}

/// A node whose node.json ceiling can't be read refuses everything.
#[tokio::test]
async fn an_unknown_ceiling_refuses_everything() {
    let p = pair(Ceiling::Manage, None).await;
    let (s, _) = send(&p.main, &p.main_cookie, "GET", &format!("/api/v1/fs/{}/list?path=", p.ns()), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(p.main.app.nodes.servers_for(&Who::Owner).is_empty(), "no live feed either");
}

/// `..`, absolute paths and links out of the folder are refused by the node.
#[tokio::test]
async fn paths_out_of_the_server_folder_are_refused() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let ns = p.ns();
    for q in ["../outside.txt", "..%2F..%2Foutside.txt", "world/../../outside.txt", "%2Fetc%2Fpasswd", "C:%5CWindows%5Cwin.ini"] {
        let (s, b) = send(&p.main, &p.main_cookie, "GET", &format!("/api/v1/fs/{ns}/text?path={q}"), None).await;
        assert!(!s.is_success(), "{q}: {s}");
        assert!(!b.to_string().contains("not yours"), "{q}");
    }
    let (s, _) = send(&p.main, &p.main_cookie, "PUT", &format!("/api/v1/fs/{ns}/text"), Some(json!({ "path": "../escaped.txt", "content": "x", "force": true }))).await;
    assert!(!s.is_success());
    assert!(!p.node.dir.join("escaped.txt").exists());
    let (s, _) = send(&p.main, &p.main_cookie, "POST", &format!("/api/v1/fs/{ns}/ops"), Some(json!({ "ops": [{ "op": "move", "from": "hello.txt", "to": "../../moved.txt" }] }))).await;
    assert!(!s.is_success());
    assert!(p.node.dir.join("servers").join("Test Server").join("hello.txt").exists());
    // Refusals are in the node's log, as the main's person.
    assert!(p.node.app.audit.recent(50).iter().any(|e| e.ip == "main" && e.status >= 400));
    // A link inside the folder that points out of it (needs the right to
    // make links on Windows; skipped without it).
    let srv = p.node.dir.join("servers").join("Test Server");
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(p.node.dir.join("outside.txt"), srv.join("link.txt")).is_ok();
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(p.node.dir.join("outside.txt"), srv.join("link.txt")).is_ok();
    if made {
        let (s, b) = send(&p.main, &p.main_cookie, "GET", &format!("/api/v1/fs/{ns}/text?path=link.txt"), None).await;
        assert!(!s.is_success(), "{s} {b}");
        assert!(!b.to_string().contains("not yours"));
    } else {
        eprintln!("skipped the link part: this account can't make links");
    }
}

/// The owner on the main may remove a server on a Manage node (an
/// owner-only route there); not when the node's own ceiling is lower, and
/// not an admin who isn't owner-level on the main.
#[tokio::test]
async fn removing_a_server_needs_manage_and_the_owner() {
    let p = pair(Ceiling::Manage, Some(Role::Operator)).await;
    let (s, _) = send(&p.main, &p.main_cookie, "POST", &format!("/api/v1/servers/{}/remove", p.ns()), Some(json!({ "mode": "remove" }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(p.node.app.servers.get(&p.node.server).is_some());

    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    p.main.app.auth.user_create("ada", "adapass12", Role::Admin, vec![]).unwrap();
    let ada = crate::cross_origin_tests::user_cookie(&p.main, "ada", "adapass12").await;
    let (s, _) = send(&p.main, &ada, "POST", &format!("/api/v1/servers/{}/remove", p.ns()), Some(json!({ "mode": "remove" }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "an admin on the main isn't owner-level");
    let (s, b) = send(&p.main, &p.main_cookie, "POST", &format!("/api/v1/servers/{}/remove", p.ns()), Some(json!({ "mode": "remove" }))).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert!(p.node.app.servers.get(&p.node.server).is_none());
}

/// Routes that aren't on the allowlist never reach the node's handlers.
#[tokio::test]
async fn only_listed_routes_are_relayed() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let ns = p.ns();
    let (s, _) = send(&p.main, &p.main_cookie, "POST", &format!("/api/v1/fs/{ns}/fetch"), Some(json!({ "url": "https://example.com/x", "path": "x" }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = send(&p.main, &p.main_cookie, "POST", &format!("/api/v1/servers/{ns}/clone"), Some(json!({}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

/// Maintenance: nothing starts there, and the main says why.
#[tokio::test]
async fn maintenance_blocks_starts() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let ns = p.ns();
    let (s, _) = send(&p.main, &p.main_cookie, "PUT", &format!("/api/v1/nodes/{}/maintenance", p.id), Some(json!({ "on": true }))).await;
    assert_eq!(s, StatusCode::OK);
    let (s, b) = send(&p.main, &p.main_cookie, "POST", &format!("/api/v1/servers/{ns}/power"), Some(json!({ "action": "start" }))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(b["error"].as_str().unwrap().contains("under maintenance"), "{b}");
    let (s, _) = send(&p.main, &p.main_cookie, "POST", &format!("/api/v1/servers/create?node={}", p.id), Some(json!({ "name": "x" }))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    // Pre-generating starts a stopped server too.
    let (s, b) = send(&p.main, &p.main_cookie, "POST", &format!("/api/v1/servers/{ns}/worlds/pregen"), Some(json!({ "radius": 100 }))).await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    // Reading and other work go on.
    let (s, _) = send(&p.main, &p.main_cookie, "GET", &format!("/api/v1/fs/{ns}/list?path="), None).await;
    assert_eq!(s, StatusCode::OK);
    let (_, heads) = p.main.app.nodes.live_view(&Who::Owner);
    assert_eq!(heads[0]["maintenance"]["by"], "owner");
    let (s, _) = send(&p.main, &p.main_cookie, "PUT", &format!("/api/v1/nodes/{}/maintenance", p.id), Some(json!({ "on": false }))).await;
    assert_eq!(s, StatusCode::OK);
    let (_, heads) = p.main.app.nodes.live_view(&Who::Owner);
    assert!(heads[0]["maintenance"].is_null());
}

/// Someone limited to some servers sees only those, and only the nodes
/// that hold one.
#[tokio::test]
async fn limited_people_see_only_their_node_servers() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let ns = p.ns();
    let other = Who::User { id: "u".into(), name: "u".into(), role: Role::Operator, servers: vec![p.main.server.clone()] };
    let (servers, heads) = p.main.app.nodes.live_view(&other);
    assert!(servers.is_empty() && heads.is_empty());
    let mine = Who::User { id: "u".into(), name: "u".into(), role: Role::Operator, servers: vec![ns.clone()] };
    let (servers, heads) = p.main.app.nodes.live_view(&mine);
    assert_eq!((servers.len(), heads.len()), (1, 1));
    assert_eq!(servers[0]["id"], ns.as_str());
}

// ---- a node that isn't honest (A2 security review) ----

/// What the fake node answers every request with: status, headers, body.
type Script = (u16, Vec<(&'static str, String)>, Vec<u8>);

struct Fake {
    main: T,
    id: String,
    cookie: String,
    _feed: Follow,
    _node: Follow,
    _tunnels: (mux::Tunnel, mux::Handle),
}

impl Fake {
    fn ns(&self, local: &str) -> String {
        crate::relay::join(&self.id, local)
    }

    /// Waits for the live feed's first list; the node's servers on the main.
    async fn listed(&self) -> Vec<String> {
        for _ in 0..100 {
            let l = self.main.app.nodes.servers_for(&Who::Owner);
            if !l.is_empty() {
                return l.iter().filter_map(|s| s["id"].as_str().map(str::to_string)).collect();
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        Vec::new()
    }
}

/// A main joined by a node that sends `live` as its live feed and answers
/// every request with `answer`, whatever it was asked: a compromised node.
async fn fake_node(live: Vec<Value>, answer: Script) -> Fake {
    let main = panel_with(TLS());
    let (_, key, _) = main.app.nodes.make_key(Ceiling::Manage, "owner");
    let (n, _) = main.app.nodes.use_key(&key, "vm", "10.0.0.9".parse().unwrap(), "").unwrap();
    let (a, b) = mux::mem::pair(None);
    let (m, d) = tokio::join!(mux::start(Side::Main, Config::default(), a.0, a.1), mux::start(Side::Node, Config::default(), b.0, b.1));
    let (m, mut d) = (m.unwrap(), d.unwrap());
    let mut incoming = std::mem::replace(&mut d.incoming, tokio::sync::mpsc::channel(1).1);
    let _node = Follow(tokio::spawn(async move {
        while let Some(mut s) = incoming.recv().await {
            let head: Value = serde_json::from_slice(&s.head).unwrap_or_default();
            let (live, answer) = (live.clone(), answer.clone());
            tokio::spawn(async move {
                match head["k"].as_str() {
                    Some("live") => {
                        for l in live {
                            let mut b = l.to_string().into_bytes();
                            b.push(b'\n');
                            if s.tx.send(&b).await.is_err() {
                                return;
                            }
                        }
                        std::future::pending::<()>().await;
                    }
                    // Status 0: a node that never answers.
                    Some("req") if answer.0 == 0 => std::future::pending::<()>().await,
                    Some("req") => {
                        while let Ok(Some(_)) = s.rx.recv().await {}
                        let h = serde_json::to_vec(&json!({ "s": answer.0, "h": answer.1 })).unwrap();
                        let mut out = (h.len() as u32).to_be_bytes().to_vec();
                        out.extend_from_slice(&h);
                        out.extend_from_slice(&answer.2);
                        let _ = s.tx.send(&out).await;
                        let _ = s.tx.finish().await;
                    }
                    _ => s.tx.reset(crate::tunnel::frame::code::REFUSED),
                }
            });
        }
    }));
    let _feed = crate::nodes::attach_for_test(&main.app, &n, m.handle.clone());
    let cookie = cookie_login(&main).await;
    let handle = d.handle.clone();
    Fake { main, id: n.id.clone(), cookie, _feed, _node, _tunnels: (m, handle) }
}

/// A node can't make the main serve its own page: whatever type and
/// headers it sends, a relayed answer never runs as a page on the main's
/// address, sets a cookie there, or carries two lengths.
#[tokio::test]
async fn a_node_cannot_put_a_page_on_the_mains_address() {
    let page = b"<script>fetch('/api/v1/tokens',{method:'POST',headers:{'x-hopperd':'1'}})</script>".to_vec();
    let headers = vec![
        ("content-type", "text/html; charset=utf-8".to_string()),
        ("content-security-policy", "script-src *".to_string()),
        ("content-length", page.len().to_string()),
        ("content-length", "1".to_string()),
        ("set-cookie", "hopperd_session=planted; Path=/".to_string()),
        ("content-disposition", "inline".to_string()),
    ];
    let f = fake_node(Vec::new(), (200, headers, page)).await;
    let path = format!("/api/v1/fs/{}/file?path=page.html&inline=1", f.ns("srv"));
    let (s, h, _) = call("GET", &path).origin(OWN).h("cookie", format!("hopperd_session={}", f.cookie)).send(&f.main).await;
    assert_eq!(s, StatusCode::OK);
    let all = |k: &str| h.get_all(k).iter().filter_map(|v| v.to_str().ok()).map(str::to_string).collect::<Vec<_>>();
    let types = all("content-type");
    assert_eq!(types.len(), 1, "{types:?}");
    assert!(!types[0].contains("html"), "a node's page would run on the main's address: {types:?}");
    let csp = all("content-security-policy");
    assert_eq!(csp.len(), 1, "{csp:?}");
    assert!(csp[0].starts_with("sandbox") && csp[0].contains("default-src 'none'"), "{csp:?}");
    assert_eq!(all("x-content-type-options"), vec!["nosniff"]);
    assert!(all("set-cookie").is_empty());
    assert_eq!(all("content-length").len(), 1, "{:?}", all("content-length"));

    // A node that answers "sign in again" or with no real status gets a
    // 502: only the main itself says who is signed in.
    for status in [401u16, 407, 101, 999] {
        let f = fake_node(Vec::new(), (status, vec![("content-type", "application/json".into())], b"{}".to_vec())).await;
        let (s, _) = send(&f.main, &f.cookie, "GET", &format!("/api/v1/servers/{}", f.ns("srv")), None).await;
        assert_eq!(s, StatusCode::BAD_GATEWAY, "{status}");
    }
}

/// Server and job ids from a node are plain names: one with a `/`, `..`,
/// `?` or `:` in it would make the browser call a different address on the
/// main when it builds a URL from it.
#[tokio::test]
async fn ids_from_a_node_must_be_plain() {
    let ids = ["ok-1", "x/../../users", "a:b", "..", "a?b", "a#b", "", "A_b", "a%2F..", r"a\b"];
    let servers: Vec<Value> = ids.iter().map(|id| json!({ "id": id, "name": id })).collect();
    let f = fake_node(vec![json!({ "t": "snap", "servers": servers, "host": {} })], (200, vec![], Vec::new())).await;
    let mut listed = f.listed().await;
    listed.sort();
    assert_eq!(listed, vec![f.ns("A_b"), f.ns("ok-1")]);

    // A job id or a made server's id that isn't plain doesn't reach the browser.
    let f = fake_node(Vec::new(), (200, vec![("content-type", "application/json".into())], br#"{"job":"../../users"}"#.to_vec())).await;
    let (s, b) = send(&f.main, &f.cookie, "POST", &format!("/api/v1/servers/create?node={}", f.id), Some(json!({ "name": "x" }))).await;
    assert_eq!(s, StatusCode::OK);
    assert!(b["job"].is_null(), "{b}");
    let answer = br#"{"done":true,"result":{"id":"x/../../users","path":"/srv"}}"#.to_vec();
    let f = fake_node(Vec::new(), (200, vec![("content-type", "application/json".into())], answer)).await;
    let (_, b) = send(&f.main, &f.cookie, "GET", &format!("/api/v1/jobs/{}", f.ns("7")), None).await;
    assert!(b["result"]["id"].is_null(), "{b}");
}

/// A node can't flood the main's list: it keeps at most so many of a
/// node's servers.
#[tokio::test]
async fn a_node_lists_a_bounded_number_of_servers() {
    let servers: Vec<Value> = (0..5000).map(|i| json!({ "id": format!("s{i}"), "name": "x" })).collect();
    let f = fake_node(vec![json!({ "t": "snap", "servers": servers, "host": {} })], (200, vec![], Vec::new())).await;
    let n = f.listed().await.len();
    assert!(n > 0 && n <= crate::nodes::MAX_NODE_SERVERS, "{n}");
}

// ---- a node's TPS and alerts on the main ----

/// A node server's TPS reading and its low-TPS alert show in the main's
/// `/alerts` under the namespaced id, like a local server's; Look is
/// enough for the main to read them.
#[tokio::test]
async fn a_nodes_tps_and_alerts_show_on_the_main() {
    for (main_copy, node_has) in [(Ceiling::Manage, Role::Admin), (Ceiling::Look, Role::Viewer)] {
        let p = pair(main_copy, Some(node_has)).await;
        let ns = p.ns();
        crate::alerts::low_tps_for_test(&p.node.server, "dg", 14.56, 68.7);
        let (s, b) = send(&p.main, &p.main_cookie, "GET", "/api/v1/alerts", None).await;
        assert_eq!(s, StatusCode::OK);
        let t = &b["tps"][&ns];
        assert!((t["tps"].as_f64().unwrap_or(0.0) - 14.56).abs() < 0.01, "{b}");
        assert_eq!(t["source"], "rcon");
        let a = b["active"].as_array().unwrap().iter().find(|a| a["server_id"] == ns.as_str()).unwrap_or_else(|| panic!("{b}"));
        assert_eq!((a["kind"].as_str(), a["node"].as_str(), a["dismissed"].as_bool()), (Some("tps"), Some(p.id.as_str()), Some(false)));
        assert!(b["settings"].is_object(), "the main's own settings stay");
    }
}

/// Dismissing a node server's alert on the main hides it there only: the
/// node's own alert stays up for the node's people.
#[tokio::test]
async fn dismissing_a_node_alert_hides_it_on_the_main_only() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let ns = p.ns();
    crate::alerts::low_tps_for_test(&p.node.server, "dg", 9.0, 110.0);
    let (_, b) = send(&p.main, &p.main_cookie, "GET", "/api/v1/alerts", None).await;
    assert!(b["active"].as_array().unwrap().iter().any(|a| a["server_id"] == ns.as_str()), "{b}");
    let (s, _) = send(&p.main, &p.main_cookie, "POST", "/api/v1/alerts/dismiss", Some(json!({ "server_id": ns, "kind": "tps", "node": p.id }))).await;
    assert_eq!(s, StatusCode::OK);
    let (_, b) = send(&p.main, &p.main_cookie, "GET", "/api/v1/alerts", None).await;
    let a = b["active"].as_array().unwrap().iter().find(|a| a["server_id"] == ns.as_str()).unwrap();
    assert_eq!(a["dismissed"], true, "{b}");
    let (_, b) = send(&p.node, &p.node_cookie, "GET", "/api/v1/alerts", None).await;
    let a = b["active"].as_array().unwrap().iter().find(|a| a["server_id"] == p.node.server.as_str()).unwrap();
    assert_eq!(a["dismissed"], false, "the node's own alert stays: {b}");
}

/// A node's alerts are read as untrusted: ids must be plain, kinds and
/// reasons known ones, numbers numbers. Its machine's alerts (a drive) show
/// as about that node, never as the main's own.
#[tokio::test]
async fn a_nodes_alerts_are_checked() {
    let answer = json!({
        "tps": { "ok": { "tps": 12.5, "mspt": 80.0, "at": 1, "source": "console" }, "x/../y": { "tps": 1.0 }, "a:b": { "tps": 1.0 }, "nan": { "tps": "fast" } },
        "no_tps": { "quiet": "rcon_off", "odd": "<script>" },
        "active": [
            { "server_id": "ok", "server": "OK", "kind": "crash", "text": "**OK** crashed", "since": 5 },
            { "server_id": "../users", "server": "x", "kind": "tps", "text": "x", "since": 5 },
            { "server_id": "ok", "server": "OK", "kind": "node", "text": "x", "since": 5 },
            { "server_id": "ok", "server": "OK", "kind": "disk", "text": "x", "since": 5 },
            { "server_id": "", "server": "", "kind": "disk", "text": "Drive / is nearly full: 3.0 GB free", "since": 6 },
            { "server_id": "", "server": "", "kind": "tps", "text": "x", "since": 6 },
        ],
    });
    let f = fake_node(Vec::new(), (200, vec![("content-type", "application/json".into())], answer.to_string().into_bytes())).await;
    let (s, b) = send(&f.main, &f.cookie, "GET", "/api/v1/alerts", None).await;
    assert_eq!(s, StatusCode::OK);
    let prefix = format!("{}:", f.id);
    let mine = |m: &Value| m.as_object().unwrap().keys().filter(|k| k.starts_with(&prefix)).cloned().collect::<Vec<_>>();
    assert_eq!(mine(&b["tps"]), vec![f.ns("ok")], "{b}");
    assert_eq!(b["tps"][f.ns("ok")]["source"], "console");
    assert_eq!(mine(&b["no_tps"]), vec![f.ns("quiet")], "{b}");
    let rows: Vec<&Value> = b["active"].as_array().unwrap().iter().filter(|a| a["node"] == f.id.as_str()).collect();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!((rows[0]["server_id"].as_str(), rows[0]["kind"].as_str()), (Some(f.ns("ok").as_str()), Some("crash")));
    assert_eq!((rows[1]["server_id"].as_str(), rows[1]["kind"].as_str()), (Some(""), Some("disk")));
    assert_eq!(rows[1]["text"], "**vm**: Drive / is nearly full: 3.0 GB free");
}

/// A node that doesn't answer doesn't hold up the main's `/alerts`.
#[tokio::test]
async fn a_slow_node_does_not_hold_up_alerts() {
    let f = fake_node(Vec::new(), (0, vec![], Vec::new())).await;
    let t = std::time::Instant::now();
    let (s, b) = send(&f.main, &f.cookie, "GET", "/api/v1/alerts", None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(t.elapsed() < std::time::Duration::from_secs(3), "{:?}", t.elapsed());
    assert!(b["tps"].as_object().unwrap().keys().all(|k| !k.starts_with(&f.id)), "{b}");
    // Asked again while that ask still runs: an answer at once.
    let t = std::time::Instant::now();
    let _ = send(&f.main, &f.cookie, "GET", "/api/v1/alerts", None).await;
    assert!(t.elapsed() < std::time::Duration::from_millis(500), "{:?}", t.elapsed());
}

/// Someone limited to some servers gets none of a node's alerts for
/// servers they can't see.
#[tokio::test]
async fn limited_people_get_only_their_node_alerts() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let other = Who::User { id: "u".into(), name: "u".into(), role: Role::Operator, servers: vec![p.main.server.clone()] };
    let mut tps = serde_json::Map::new();
    tps.insert(p.node.server.clone(), json!({ "tps": 9.0 }));
    let v = json!({ "tps": tps, "no_tps": {}, "active": [{ "server_id": p.node.server, "kind": "tps", "text": "x", "since": 1 }] });
    let mut out = json!({ "active": [], "tps": {}, "no_tps": {} });
    crate::alerts::merge_node_for_test(&mut out, &p.id, "vm", &v, &other);
    assert_eq!(out, json!({ "active": [], "tps": {}, "no_tps": {} }));
    let mine = Who::User { id: "u".into(), name: "u".into(), role: Role::Operator, servers: vec![p.ns()] };
    crate::alerts::merge_node_for_test(&mut out, &p.id, "vm", &v, &mine);
    assert!(out["tps"].get(p.ns()).is_some() && out["active"].as_array().unwrap().len() == 1, "{out}");
}

/// A node at the Look ceiling never sends its passwords: not in
/// `/servers/infos`, the properties page or as a file. At Manage the
/// main's owner is an Admin there and sees them (design/server-secrets.md).
#[tokio::test]
async fn a_look_node_sends_no_secret() {
    let props = "motd=on a node\nrcon.password=hunter22\nmanagement-server-secret=s3cretkey\n";
    let p = pair(Ceiling::Look, Some(Role::Viewer)).await;
    std::fs::write(p.node.dir.join("servers").join("Test Server").join("server.properties"), props).unwrap();
    let ns = p.ns();
    let (s, v) = send(&p.main, &p.main_cookie, "GET", "/api/v1/servers/infos", None).await;
    assert_eq!(s, StatusCode::OK);
    let row = v.as_array().unwrap().iter().find(|s| s["id"] == ns.as_str()).expect("the node's server").clone();
    assert_eq!(row["properties"]["motd"], "on a node", "{row}");
    assert!(!v.to_string().contains("hunter22") && !v.to_string().contains("s3cretkey"), "{row}");
    let (s, v) = send(&p.main, &p.main_cookie, "GET", &format!("/api/v1/servers/{ns}/properties"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["values"].get("rcon.password").is_none() && v["values"].get("management-server-secret").is_none(), "{v}");
    for r in ["text?path=server.properties", "file?path=server.properties"] {
        let (s, v) = send(&p.main, &p.main_cookie, "GET", &format!("/api/v1/fs/{ns}/{r}"), None).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{r}");
        assert!(!v.to_string().contains("hunter22"), "{r}");
    }

    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    std::fs::write(p.node.dir.join("servers").join("Test Server").join("server.properties"), props).unwrap();
    let ns = p.ns();
    let (_, v) = send(&p.main, &p.main_cookie, "GET", &format!("/api/v1/servers/{ns}/properties"), None).await;
    assert_eq!(v["values"]["rcon.password"], "hunter22");
    // Infos never carry them, at any ceiling.
    let (_, v) = send(&p.main, &p.main_cookie, "GET", "/api/v1/servers/infos", None).await;
    assert!(!v.to_string().contains("hunter22"));
}

#[path = "relay_stream_tests.rs"]
mod stream;

#[path = "relay_skew_tests.rs"]
mod skew;

// ---- node-scoped access, bulk and MCP (A4) ----

/// A second node joined to the main but never connected: another node's id
/// to aim at.
fn other_node(p: &Pair) -> String {
    let (_, key, _) = p.main.app.nodes.make_key(Ceiling::Manage, "owner");
    p.main.app.nodes.use_key(&key, "vm2", "10.0.0.8".parse().unwrap(), "").unwrap().0.id
}

/// A new server folder on the node, as if made there after the scope was
/// given; waits until the main lists it.
async fn new_node_server(p: &Pair, folder: &str) -> String {
    let srv = p.node.dir.join("servers").join(folder);
    std::fs::create_dir_all(&srv).unwrap();
    std::fs::write(srv.join("server.properties"), "motd=later\n").unwrap();
    p.node.app.rescan();
    let snap = p.node.app.take_snapshot();
    p.node.app.snapshot.send_replace(Arc::new(snap));
    let local = p.node.app.servers.list().into_iter().find(|e| e.name == folder).expect("the new server").id.clone();
    let ns = crate::relay::join(&p.id, &local);
    for _ in 0..150 {
        if p.main.app.nodes.servers_for(&Who::Owner).iter().any(|s| s["id"] == ns.as_str()) {
            return ns;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // The live loop didn't send it yet: hand the list over as it would.
    let list: Vec<Value> = p.node.app.snapshot.borrow().servers.iter().map(|s| json!(s)).collect();
    crate::nodes::took_for_test(&p.main.app, &p.id, list);
    ns
}

/// Someone given "all servers on node X" sees a server made there later,
/// and nothing on another node or on the main.
#[tokio::test]
async fn a_node_scope_covers_servers_made_later() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let y = other_node(&p);
    let (s, u) = send(&p.main, &p.main_cookie, "POST", "/api/v1/users", Some(json!({ "name": "nia", "password": "niapass12", "role": "operator", "servers": [format!("node:{}", p.id)] }))).await;
    assert_eq!(s, StatusCode::OK, "{u}");
    let later = new_node_server(&p, "Later Server").await;
    let nia = crate::cross_origin_tests::user_cookie(&p.main, "nia", "niapass12").await;
    let (s, list) = send(&p.main, &nia, "GET", "/api/v1/servers", None).await;
    assert_eq!(s, StatusCode::OK);
    let ids: Vec<&str> = list.as_array().unwrap().iter().filter_map(|s| s["id"].as_str()).collect();
    assert!(ids.contains(&later.as_str()) && ids.contains(&p.ns().as_str()), "{ids:?}");
    assert!(!ids.contains(&p.main.server.as_str()), "{ids:?}");
    // Their own role on it: an Operator can look, not open secrets.
    let (s, b) = send(&p.main, &nia, "GET", &format!("/api/v1/fs/{later}/list?path="), None).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let (s, _) = send(&p.main, &nia, "GET", &format!("/api/v1/servers/{}/console", p.main.server), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = send(&p.main, &nia, "GET", &format!("/api/v1/servers/{y}:x/console"), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // The live feed's heads: node X only, even before Y has servers.
    let who = Who::User { id: "u".into(), name: "nia".into(), role: Role::Operator, servers: vec![format!("node:{}", p.id)] };
    let (servers, heads) = p.main.app.nodes.live_view(&who);
    assert_eq!(heads.iter().map(|h| h["id"].as_str().unwrap()).collect::<Vec<_>>(), vec![p.id.as_str()]);
    assert!(servers.iter().all(|s| s["node"] == p.id.as_str()));
    let only_y = Who::User { id: "u".into(), name: "nia".into(), role: Role::Operator, servers: vec![format!("node:{y}")] };
    let (servers, heads) = p.main.app.nodes.live_view(&only_y);
    assert!(servers.is_empty() && heads.len() == 1 && heads[0]["id"] == y.as_str());
}

/// A token for node X is refused on node Y, on the main's own servers and
/// on node Y's jobs; it works on X.
#[tokio::test]
async fn a_node_token_is_refused_elsewhere() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let y = other_node(&p);
    let (s, b) = send(&p.main, &p.main_cookie, "POST", "/api/v1/tokens", Some(json!({ "name": "x only", "servers": [format!("node:{}", p.id)] }))).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let token = b["token"].as_str().unwrap().to_string();
    let get = |path: String| {
        let token = token.clone();
        let main = &p.main;
        async move {
            let (s, _, _) = call("GET", path).bearer(&token).send(main).await;
            s
        }
    };
    assert_eq!(get(format!("/api/v1/fs/{}/text?path=hello.txt", p.ns())).await, StatusCode::OK);
    assert_eq!(get(format!("/api/v1/servers/{}/console", p.main.server)).await, StatusCode::FORBIDDEN);
    assert_eq!(get(format!("/api/v1/servers/{y}:x/console")).await, StatusCode::FORBIDDEN);
    assert_eq!(get(format!("/api/v1/jobs/{y}:1")).await, StatusCode::FORBIDDEN);
    // A scope for a node that isn't joined is refused when it's given.
    let (s, b) = send(&p.main, &p.main_cookie, "POST", "/api/v1/tokens", Some(json!({ "name": "gone", "servers": ["node:abcdefabcdef"] }))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
    let (s, _) = send(&p.main, &p.main_cookie, "POST", "/api/v1/tokens", Some(json!({ "name": "bare", "servers": ["node:"] }))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

/// Removing a node drops its scope entries; a list left empty matches
/// nothing rather than widening to every server.
#[tokio::test]
async fn removing_a_node_drops_its_scope() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let y = other_node(&p);
    let mixed = vec![format!("node:{y}"), p.main.server.clone(), format!("{y}:tnp")];
    let only_y = vec![format!("node:{y}")];
    p.main.app.auth.user_create("mixed", "mixedpass1", Role::Operator, mixed).unwrap();
    p.main.app.auth.user_create("onlyy", "onlyypass1", Role::Operator, only_y.clone()).unwrap();
    p.main.app.auth.token_create("y token", false, only_y, None).unwrap();
    let (s, _) = send(&p.main, &p.main_cookie, "DELETE", &format!("/api/v1/nodes/{y}"), None).await;
    assert_eq!(s, StatusCode::OK);
    let users = p.main.app.auth.users();
    let of = |n: &str| users.iter().find(|u| u.name == n).unwrap().servers.clone();
    assert_eq!(of("mixed"), vec![p.main.server.clone()]);
    assert_eq!(of("onlyy"), vec![crate::auth::NO_SERVERS.to_string()]);
    let t = p.main.app.auth.tokens();
    assert_eq!(t.iter().find(|t| t.name == "y token").unwrap().servers, vec![crate::auth::NO_SERVERS.to_string()]);
    let nobody = Who::User { id: "u".into(), name: "onlyy".into(), role: Role::Operator, servers: of("onlyy") };
    assert!(!nobody.sees(&p.main.server) && !nobody.sees(&p.ns()));
    let (servers, heads) = p.main.app.nodes.live_view(&nobody);
    assert!(servers.is_empty() && heads.is_empty());
}

/// A bulk restart across the main and a node (one request per server, as
/// the web UI sends them) lands in the main's one audit log, the node
/// named; the node filter shows only that node's lines.
#[tokio::test]
async fn a_bulk_restart_is_audited_with_the_node_named() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    for id in [p.main.server.clone(), p.ns()] {
        // The test servers have nothing to run: the answer may be a refusal
        // to start, but the attempt is what the log must show.
        send(&p.main, &p.main_cookie, "POST", &format!("/api/v1/servers/{id}/power"), Some(json!({ "action": "restart" }))).await;
    }
    let (s, all) = send(&p.main, &p.main_cookie, "GET", "/api/v1/audit?limit=50", None).await;
    assert_eq!(s, StatusCode::OK);
    let power: Vec<&Value> = all.as_array().unwrap().iter().filter(|e| e["path"].as_str().is_some_and(|x| x.ends_with("/power"))).collect();
    assert_eq!(power.len(), 2, "{all}");
    let on_node = power.iter().find(|e| e["path"].as_str().unwrap().contains(&p.ns())).unwrap();
    assert_eq!((on_node["node"].as_str(), on_node["node_name"].as_str()), (Some(p.id.as_str()), Some("vm")));
    let local = power.iter().find(|e| e["path"].as_str().unwrap().contains(&format!("/servers/{}/", p.main.server))).unwrap();
    assert!(local.get("node").is_none(), "{local}");
    let (_, only) = send(&p.main, &p.main_cookie, "GET", &format!("/api/v1/audit?node={}", p.id), None).await;
    let only = only.as_array().unwrap();
    assert!(!only.is_empty() && only.iter().all(|e| e["node"] == p.id.as_str()), "{only:?}");
    // A refusal on a node is logged with the node named too.
    p.main.app.auth.user_create("vic", "vicpass123", Role::Viewer, vec![]).unwrap();
    let vic = crate::cross_origin_tests::user_cookie(&p.main, "vic", "vicpass123").await;
    let (s, _) = send(&p.main, &vic, "POST", &format!("/api/v1/servers/{}/power", p.ns()), Some(json!({ "action": "restart" }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (_, only) = send(&p.main, &p.main_cookie, "GET", &format!("/api/v1/audit?node={}", p.id), None).await;
    assert!(only.as_array().unwrap().iter().any(|e| e["who"].as_str().is_some_and(|w| w.contains("vic")) && e["status"] == 403), "{only}");
}

async fn mcp(p: &Pair, token: &str, tool: &str, args: Value) -> Value {
    let rpc = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": tool, "arguments": args } });
    let (s, _, b) = call("POST", "/api/v1/mcp").bearer(token).json(rpc).send(&p.main).await;
    assert_eq!(s, StatusCode::OK);
    body(&b)["result"].clone()
}

/// The MCP tools list and act on node servers in scope, and refuse the
/// rest with the same checks as the web UI.
#[tokio::test]
async fn mcp_works_on_node_servers_in_scope_only() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let y = other_node(&p);
    let (_, b) = send(&p.main, &p.main_cookie, "POST", "/api/v1/tokens", Some(json!({ "name": "agent", "servers": [format!("node:{}", p.id)] }))).await;
    let token = b["token"].as_str().unwrap().to_string();
    let r = mcp(&p, &token, "list_servers", json!({})).await;
    let listed = r["content"][0]["text"].as_str().unwrap();
    assert!(listed.contains(&p.ns()) && !listed.contains(&format!("\"{}\"", p.main.server)), "{listed}");
    let r = mcp(&p, &token, "read_file", json!({ "server": p.ns(), "path": "hello.txt" })).await;
    assert_eq!(r["content"][0]["text"], "hello from the server", "{r}");
    let r = mcp(&p, &token, "write_file", json!({ "server": p.ns(), "path": "hello.txt", "content": "from mcp" })).await;
    assert!(r.get("isError").is_none(), "{r}");
    assert_eq!(std::fs::read_to_string(p.node.dir.join("servers").join("Test Server").join("hello.txt")).unwrap(), "from mcp");
    for server in [p.main.server.clone(), format!("{y}:x")] {
        let r = mcp(&p, &token, "read_file", json!({ "server": server, "path": "hello.txt" })).await;
        assert_eq!(r["isError"], true, "{server}: {r}");
    }
    // Path checks still apply on the node.
    let r = mcp(&p, &token, "read_file", json!({ "server": p.ns(), "path": "../outside.txt" })).await;
    assert_eq!(r["isError"], true, "{r}");
    // A read-only token can't write through MCP either.
    let (_, b) = send(&p.main, &p.main_cookie, "POST", "/api/v1/tokens", Some(json!({ "name": "look", "read_only": true, "servers": [format!("node:{}", p.id)] }))).await;
    let look = b["token"].as_str().unwrap().to_string();
    let r = mcp(&p, &look, "write_file", json!({ "server": p.ns(), "path": "hello.txt", "content": "no" })).await;
    assert_eq!(r["isError"], true, "{r}");
    // A node at Look refuses MCP writes too (the ceiling caps everything).
    *p.ceiling.lock().unwrap() = Some(Role::Viewer);
    let r = mcp(&p, &token, "write_file", json!({ "server": p.ns(), "path": "hello.txt", "content": "no" })).await;
    assert_eq!(r["isError"], true, "{r}");
    assert_eq!(std::fs::read_to_string(p.node.dir.join("servers").join("Test Server").join("hello.txt")).unwrap(), "from mcp");
}

/// The ceiling route only lowers, and only the owner may use it.
#[tokio::test]
async fn the_ceiling_route_only_lowers() {
    let p = pair(Ceiling::Operate, Some(Role::Operator)).await;
    let path = format!("/api/v1/nodes/{}/ceiling", p.id);
    let (s, b) = send(&p.main, &p.main_cookie, "PUT", &path, Some(json!({ "ceiling": "manage" }))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
    assert!(b["error"].as_str().unwrap().contains("join again"), "{b}");
    p.main.app.auth.user_create("ada", "adapass123", Role::Admin, vec![]).unwrap();
    let ada = crate::cross_origin_tests::user_cookie(&p.main, "ada", "adapass123").await;
    let (s, _) = send(&p.main, &ada, "PUT", &path, Some(json!({ "ceiling": "look" }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, b) = send(&p.main, &p.main_cookie, "PUT", &path, Some(json!({ "ceiling": "look" }))).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let (_, heads) = p.main.app.nodes.live_view(&Who::Owner);
    assert_eq!(heads[0]["ceiling"], "look");
    let (s, _) = send(&p.main, &p.main_cookie, "POST", &format!("/api/v1/servers/{}/power", p.ns()), Some(json!({ "action": "start" }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

/// A failed node update is still shown after the main restarts; a later
/// success clears it.
#[tokio::test]
async fn a_failed_update_survives_a_restart_of_the_main() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    crate::nodes::update_ended_for_test(&p.main.app, &p.id, "failed");
    let again = crate::nodes::Nodes::new(&p.main.app.data_dir);
    let (_, heads) = again.live_view(&Who::Owner);
    assert_eq!(heads[0]["update"]["state"], "failed", "{}", heads[0]);
    assert_eq!(heads[0]["update"]["message"], "it didn't start");
    crate::nodes::update_ended_for_test(&p.main.app, &p.id, "done");
    let again = crate::nodes::Nodes::new(&p.main.app.data_dir);
    let (_, heads) = again.live_view(&Who::Owner);
    assert!(heads[0]["update"].is_null(), "{}", heads[0]);
}
