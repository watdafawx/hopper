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
