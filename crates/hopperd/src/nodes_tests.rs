//! Node mode through the real router (design/multi-panel.md, A1): who may
//! manage nodes, join keys, code requests, the wrong-key limit, the audit
//! log, and a real node connecting over TLS with the CA pinned.

use crate::cross_origin_tests::{OWN, T, body, call, cookie_login, panel_with, user_cookie};
use crate::tunnel::dial::{self, ConnectError, Pin, Target};
use crate::tunnel::mux::{self, Config, MuxError, Side};
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::time::Duration;

/// A test panel with a real CA, so a node can connect to it.
fn main_panel() -> (T, crate::tls::Ca) {
    let dir = std::env::temp_dir().join(format!("hopperd-a1-ca-{}-{}", std::process::id(), crate::auth::random_hex(4)));
    std::fs::create_dir_all(&dir).unwrap();
    let ca = crate::tls::load_or_create_ca(&dir, "main").unwrap();
    let t = panel_with(crate::TlsInfo { fingerprint: crate::tls::fingerprint(&ca.der), ca_pem: ca.pem.clone() });
    let _ = std::fs::remove_dir_all(&dir);
    (t, ca)
}

async fn owner(t: &T) -> String {
    cookie_login(t).await
}

fn as_cookie(c: &str) -> String {
    format!("hopperd_session={c}")
}

async fn owner_call(t: &T, cookie: &str, method: &'static str, path: &str, b: Option<Value>) -> (StatusCode, Value) {
    let mut c = call(method, path).origin(OWN).h("cookie", as_cookie(cookie)).h("x-hopperd", "1");
    if let Some(b) = b {
        c = c.json(b);
    }
    let (s, _, b) = c.send(t).await;
    (s, body(&b))
}

fn audit_paths(t: &T) -> Vec<(String, u16)> {
    t.app.audit.recent(200).into_iter().map(|e| (e.path, e.status)).collect()
}

/// Only the true owner manages nodes: not a manager, not a paired PC that
/// may manage the panel, not a token, not anonymous.
#[tokio::test]
async fn only_the_owner_manages_nodes() {
    let (t, _) = main_panel();
    t.app.auth.user_create("mia", "miapass12", crate::auth::Role::Manager, vec![]).unwrap();
    let mia = user_cookie(&t, "mia", "miapass12").await;
    let routes: [(&'static str, &str, Option<Value>); 5] = [
        ("GET", "/api/v1/nodes", None),
        ("POST", "/api/v1/nodes/keys", Some(json!({ "url": "https://main:8790", "ceiling": "manage" }))),
        ("DELETE", "/api/v1/nodes/keys/abcd", None),
        ("POST", "/api/v1/nodes/pending/abcd/answer", Some(json!({ "allow": true }))),
        ("DELETE", "/api/v1/nodes/abcd", None),
    ];
    for (m, p, b) in &routes {
        let (s, _) = owner_call(&t, &mia, m, p, b.clone()).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "manager: {m} {p}");
        let mut c = call(m, *p);
        if let Some(b) = b {
            c = c.json(b.clone());
        }
        assert_eq!(c.send(&t).await.0, StatusCode::UNAUTHORIZED, "anonymous: {m} {p}");
    }
    // A paired PC allowed to manage the panel still can't.
    let (id, secret, _) = t.app.auth.pair_request("Hopper on FOX", "192.168.50.2".parse().unwrap(), "AA").unwrap();
    t.app.auth.answer(&id, true, None, true).unwrap();
    let dev = match t.app.auth.pair_status(&id, &secret) {
        crate::auth::PairStatus::Approved { token, .. } => token,
        other => panic!("{other:?}"),
    };
    let (s, _, _) = call("GET", "/api/v1/nodes").bearer(&dev).send(&t).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "a device that may manage");
    let me = owner(&t).await;
    assert_eq!(owner_call(&t, &me, "GET", "/api/v1/nodes", None).await.0, StatusCode::OK);
    // The node's own page too: only its owner sees or leaves it.
    assert_eq!(owner_call(&t, &mia, "GET", "/api/v1/this-node", None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(owner_call(&t, &mia, "DELETE", "/api/v1/this-node", None).await.0, StatusCode::FORBIDDEN);
    let (s, v) = owner_call(&t, &me, "GET", "/api/v1/this-node", None).await;
    assert_eq!((s, v["joined"].as_bool()), (StatusCode::OK, Some(false)));
}

/// A join key works once and runs out; refusals are logged and the node
/// learns nothing about why.
#[tokio::test]
async fn join_keys_are_single_use_and_logged() {
    let (t, _) = main_panel();
    let me = owner(&t).await;
    let (s, k) = owner_call(&t, &me, "POST", "/api/v1/nodes/keys", Some(json!({ "url": "https://main.example:8790/x", "ceiling": "operate" }))).await;
    assert_eq!(s, StatusCode::OK, "{k}");
    let key = k["key"].as_str().unwrap().to_string();
    assert!(k["linux"].as_str().unwrap().contains(&format!("node join https://main.example:8790 --key {key} --ca {}", t.app.tls.fingerprint)));
    assert!(k["windows"].as_str().unwrap().contains(&format!("--key {key}")));
    let (_, list) = owner_call(&t, &me, "GET", "/api/v1/nodes", None).await;
    assert_eq!(list["keys"].as_array().unwrap().len(), 1);
    assert!(!list.to_string().contains(&key), "the key is shown only when made");

    let join = |key: String| call("POST", "/api/v1/node/join").from("10.1.1.5").json(json!({ "key": key, "name": "vm-1", "version": "0.62.0" }));
    let (s, _, b) = join(key.clone()).send(&t).await;
    assert_eq!(s, StatusCode::OK);
    let v = body(&b);
    assert_eq!(v["ceiling"], "operate");
    assert!(v["token"].as_str().unwrap().starts_with("hnt_"));
    let (s, _, b) = join(key.clone()).send(&t).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "used");
    let (_, _, unknown) = join("hn_unknown".into()).send(&t).await;
    assert_eq!(b, unknown, "the same answer for every refusal");

    // An expired one.
    let (_, k2) = owner_call(&t, &me, "POST", "/api/v1/nodes/keys", Some(json!({ "url": "https://main.example:8790" }))).await;
    let key2 = k2["key"].as_str().unwrap().to_string();
    let path = t.app.data_dir.join("nodes.json");
    let mut f: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for k in f["keys"].as_array_mut().unwrap() {
        k["expires"] = json!(1);
    }
    std::fs::write(&path, f.to_string()).unwrap();
    let reloaded = crate::nodes::Nodes::new(&t.app.data_dir);
    assert_eq!(reloaded.use_key(&key2, "late", "10.1.1.5".parse().unwrap(), "").unwrap_err(), crate::nodes::KeyRefusal::Expired);

    let log = audit_paths(&t);
    assert!(log.iter().any(|(p, s)| p.contains("node \"vm-1\" joined with a join key, may: operate") && *s == 200), "{log:?}");
    assert!(log.iter().any(|(p, s)| p.contains("refused: join key already used") && *s == 401), "{log:?}");
    assert!(log.iter().any(|(p, _)| p.contains("join key") && p.contains("made, may: operate")), "{log:?}");

    // A key for an address a shell could misread is never made.
    let (s, _) = owner_call(&t, &me, "POST", "/api/v1/nodes/keys", Some(json!({ "url": "https://main$(id):8790" }))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

/// Ten wrong keys from one address, then that address is refused for a
/// while (even with a right key); other addresses aren't.
#[tokio::test]
async fn wrong_keys_are_rate_limited_per_address() {
    let (t, _) = main_panel();
    let me = owner(&t).await;
    for _ in 0..10 {
        let (s, _, _) = call("POST", "/api/v1/node/join").from("10.2.2.2").json(json!({ "key": "hn_guess", "name": "x" })).send(&t).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    let (_, k) = owner_call(&t, &me, "POST", "/api/v1/nodes/keys", Some(json!({ "url": "https://main:8790" }))).await;
    let key = k["key"].as_str().unwrap().to_string();
    let (s, _, _) = call("POST", "/api/v1/node/join").from("10.2.2.2").json(json!({ "key": key, "name": "x" })).send(&t).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    // Wrong node tokens count too, on every node route.
    let (s, _, _) = call("GET", "/api/v1/node/check").from("10.2.2.2").bearer("hnt_x").send(&t).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    let (s, _, _) = call("POST", "/api/v1/node/join").from("10.2.2.3").json(json!({ "key": key, "name": "x" })).send(&t).await;
    assert_eq!(s, StatusCode::OK, "another address");
    assert!(audit_paths(&t).iter().any(|(p, s)| p.contains("too many wrong keys") && *s == 429));
}

/// A code request: the main shows the code the node computes, the owner
/// allows it, the node gets its token once.
#[tokio::test]
async fn code_pairing_through_the_routes() {
    let (t, _) = main_panel();
    let me = owner(&t).await;
    let (s, _, b) = call("POST", "/api/v1/node/request").from("10.3.3.3").json(json!({ "name": "oracle-a1", "version": "0.62.0" })).send(&t).await;
    assert_eq!(s, StatusCode::OK);
    let r = body(&b);
    let (id, secret) = (r["id"].as_str().unwrap().to_string(), r["secret"].as_str().unwrap().to_string());
    assert!(r.get("code").is_none(), "the code is never sent");
    let (_, list) = owner_call(&t, &me, "GET", "/api/v1/nodes", None).await;
    let p = &list["pending"][0];
    assert_eq!(p["code"].as_str().unwrap(), hopper_core::hopperd::pair_code(&t.app.tls.fingerprint, &id, &secret));
    assert_eq!((p["name"].as_str(), p["ip"].as_str()), (Some("oracle-a1"), Some("10.3.3.3")));
    let status = || call("POST", "/api/v1/node/request/status").from("10.3.3.3").json(json!({ "id": id, "secret": secret }));
    assert_eq!(body(&status().send(&t).await.2)["state"], "waiting");
    let (s, _) = owner_call(&t, &me, "POST", &format!("/api/v1/nodes/pending/{id}/answer"), Some(json!({ "allow": true, "ceiling": "look" }))).await;
    assert_eq!(s, StatusCode::OK);
    let v = body(&status().send(&t).await.2);
    assert_eq!((v["state"].as_str(), v["ceiling"].as_str()), (Some("allowed"), Some("look")));
    assert!(v["main_name"].is_string(), "the node learns the main's name: {v}");
    let token = v["token"].as_str().unwrap().to_string();
    assert_eq!(body(&status().send(&t).await.2)["state"], "unknown", "handed over once");
    let (s, _, b) = call("GET", "/api/v1/node/check").bearer(&token).send(&t).await;
    assert_eq!((s, body(&b)["name"].as_str()), (StatusCode::OK, Some("oracle-a1")));
    let log = audit_paths(&t);
    assert!(log.iter().any(|(p, _)| p.contains("node \"oracle-a1\" allowed, may: look")), "{log:?}");
    assert!(!log.iter().any(|(p, _)| p.starts_with("/api/v1/node/request/status")), "polling isn't logged");
}

/// Serves `t`'s router over real TLS on 127.0.0.1; returns the address.
async fn serve_tls(t: &T, ca: &crate::tls::Ca) -> Target {
    let own: crate::tls::OwnCert = std::sync::Arc::new(std::sync::RwLock::new(std::sync::Arc::new(
        crate::tls::issue(ca, &["localhost".into()], &["127.0.0.1".parse().unwrap()]).unwrap(),
    )));
    let resolver = |internet| crate::tls::Resolver { own: own.clone(), acme: t.app.acme.clone(), internet };
    let tls = crate::net::Tls { lan: crate::tls::server_config(resolver(false)), internet: crate::tls::server_config(resolver(true)) };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let router = crate::api::router(t.app.clone());
    tokio::spawn(crate::net::serve(listener, router, tls, std::future::pending()));
    Target { host: "127.0.0.1".into(), port }
}

async fn state_of(t: &T, me: &str, id: &str) -> String {
    let (_, list) = owner_call(t, me, "GET", "/api/v1/nodes", None).await;
    list["nodes"].as_array().unwrap().iter().find(|n| n["id"] == id).map(|n| n["state"].as_str().unwrap().to_string()).unwrap_or_default()
}

/// A real node joins over TLS with the fingerprint from the command,
/// connects with the CA pinned, shows as connected, and stops when the
/// owner removes it. Another CA is refused as a certificate problem.
#[tokio::test]
async fn a_node_joins_connects_and_is_removed() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (t, ca) = main_panel();
    let me = owner(&t).await;
    let target = serve_tls(&t, &ca).await;
    let (_, k) = owner_call(&t, &me, "POST", "/api/v1/nodes/keys", Some(json!({ "url": target.url() }))).await;
    let key = k["key"].as_str().unwrap();

    // A wrong fingerprint stops before anything is sent.
    let wrong = "00:".repeat(31) + "00";
    let e = dial::request(&target, &Pin::Fingerprint(wrong), "POST", "/api/v1/node/join", None, Some(&json!({ "key": key }))).await.unwrap_err();
    assert!(matches!(e, ConnectError::Certificate(_)), "{e}");

    let (s, v, seen) = dial::request(&target, &Pin::Fingerprint(t.app.tls.fingerprint.clone()), "POST", "/api/v1/node/join", None, Some(&json!({ "key": key, "name": "vm" })))
        .await
        .unwrap();
    assert_eq!(s, 200, "{v}");
    assert_eq!(seen.as_ref(), ca.der.as_ref(), "the CA it pins is the main's");
    let (node_id, token) = (v["node_id"].as_str().unwrap().to_string(), v["token"].as_str().unwrap().to_string());
    assert_eq!(state_of(&t, &me, &node_id).await, "new");

    let pin = Pin::Ca(seen);
    let (tx, rx) = crate::tunnel::ws::connect(&target, &pin, &token).await.unwrap();
    let tun = mux::start(Side::Node, Config::default(), tx, rx).await.unwrap();
    // The main records the connection once its HELLO is in.
    let mut ok = false;
    for _ in 0..50 {
        if state_of(&t, &me, &node_id).await == "connected" {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(ok, "connected within 2.5 s");
    assert!(audit_paths(&t).iter().any(|(p, s)| p.contains("/api/v1/node/connect (connected") && *s == 101));

    // A wrong token never gets a WebSocket.
    match crate::tunnel::ws::connect(&target, &pin, "hnt_wrong").await {
        Err(ConnectError::Refused(401)) => {}
        other => panic!("{:?}", other.map(|_| ())),
    }

    // Another panel's CA: the certificate doesn't match.
    let dir = std::env::temp_dir().join(format!("hopperd-a1-other-{}", crate::auth::random_hex(4)));
    std::fs::create_dir_all(&dir).unwrap();
    let other = crate::tls::load_or_create_ca(&dir, "other").unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    match crate::tunnel::ws::connect(&target, &Pin::Ca(other.der.clone()), &token).await {
        Err(e @ ConnectError::Certificate(_)) => assert_eq!(e.step(), "certificate"),
        other => panic!("{:?}", other.map(|_| ())),
    }

    // Removing it closes the connection with a goodbye the node stops on.
    let (s, _) = owner_call(&t, &me, "DELETE", &format!("/api/v1/nodes/{node_id}"), None).await;
    assert_eq!(s, StatusCode::OK);
    let why = tokio::time::timeout(Duration::from_secs(5), tun.done).await.unwrap().unwrap();
    assert!(matches!(&why, MuxError::GoAway(crate::tunnel::frame::code::BYE, r) if r == "removed"), "{why}");
    match crate::tunnel::ws::connect(&target, &pin, &token).await {
        Err(ConnectError::Refused(401)) => {}
        other => panic!("{:?}", other.map(|_| ())),
    }
    assert!(audit_paths(&t).iter().any(|(p, _)| p.contains("node \"vm\" removed")));
}

/// `hopperd node leave` from the node: the main forgets it.
#[tokio::test]
async fn a_node_can_leave() {
    let (t, _) = main_panel();
    let me = owner(&t).await;
    let (_, k) = owner_call(&t, &me, "POST", "/api/v1/nodes/keys", Some(json!({ "url": "https://main:8790" }))).await;
    let (_, _, b) = call("POST", "/api/v1/node/join").json(json!({ "key": k["key"], "name": "bye" })).send(&t).await;
    let token = body(&b)["token"].as_str().unwrap().to_string();
    let (s, _, _) = call("POST", "/api/v1/node/leave").bearer(&token).send(&t).await;
    assert_eq!(s, StatusCode::OK);
    let (_, list) = owner_call(&t, &me, "GET", "/api/v1/nodes", None).await;
    assert!(list["nodes"].as_array().unwrap().is_empty());
    assert_eq!(call("POST", "/api/v1/node/leave").bearer(&token).send(&t).await.0, StatusCode::UNAUTHORIZED);
    assert!(audit_paths(&t).iter().any(|(p, _)| p.contains("node \"bye\" left")));
}
