//! Version skew and updating nodes from the main (design/multi-panel.md,
//! A3): what both sides can do is what both say they can, a node two
//! tunnel versions apart is shown read-only with the reason, "Update" asks
//! the node to run its own update, "Update all" stops at the first failure,
//! and a node takes an update only at the Manage level from a lead.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::{TLS, pair, send};
use crate::auth::{Role, Who};
use crate::cross_origin_tests::{T, cookie_login, panel_with};
use crate::nodes::{Ceiling, Follow, Node, Skew};
use crate::tunnel::frame::TUNNEL_VERSION;
use crate::tunnel::mux::{self, Config, MuxError, Side};

/// A fake node that answers `update` streams with `lines` and counts how
/// often it was asked.
struct UpNode {
    node: Node,
    asked: Arc<AtomicUsize>,
    _feed: Follow,
    _serve: Follow,
    _tunnels: (mux::Tunnel, mux::Handle),
}

async fn up_node(main: &T, name: &str, ceiling: Ceiling, version: &str, caps: Vec<String>, lines: Vec<Value>) -> UpNode {
    let (_, key, _) = main.app.nodes.make_key(ceiling, "owner");
    let (node, _) = main.app.nodes.use_key(&key, name, "10.0.0.9".parse().unwrap(), version).unwrap();
    let (a, b) = mux::mem::pair(None);
    let (m, d) = tokio::join!(mux::start(Side::Main, Config::default(), a.0, a.1), mux::start(Side::Node, Config::default(), b.0, b.1));
    let (m, mut d) = (m.unwrap(), d.unwrap());
    let mut incoming = std::mem::replace(&mut d.incoming, tokio::sync::mpsc::channel(1).1);
    let asked = Arc::new(AtomicUsize::new(0));
    let n = asked.clone();
    let _serve = Follow(tokio::spawn(async move {
        while let Some(mut s) = incoming.recv().await {
            let head: Value = serde_json::from_slice(&s.head).unwrap_or_default();
            if head["k"] != "update" {
                s.tx.reset(crate::tunnel::frame::code::REFUSED);
                continue;
            }
            n.fetch_add(1, Ordering::SeqCst);
            let lines = lines.clone();
            tokio::spawn(async move {
                for l in lines {
                    let mut b = l.to_string().into_bytes();
                    b.push(b'\n');
                    let _ = s.tx.send(&b).await;
                }
                let _ = s.tx.finish().await;
            });
        }
    }));
    let _feed = crate::nodes::attach_for_test_as(&main.app, &node, m.handle.clone(), version, caps);
    let handle = d.handle.clone();
    UpNode { node, asked, _feed, _serve, _tunnels: (m, handle) }
}

fn all_caps() -> Vec<String> {
    crate::api::CAPABILITIES.iter().map(|c| c.to_string()).collect()
}

/// The node's row in the main's list.
fn row(main: &T, id: &str) -> Value {
    let l = main.app.nodes.list("");
    l["nodes"].as_array().unwrap().iter().find(|n| n["id"] == id).cloned().unwrap_or_default()
}

/// Waits until the node's update is no longer running; its record.
async fn settled(main: &T, id: &str) -> Value {
    for _ in 0..100 {
        let u = row(main, id)["update"].clone();
        if !u.is_null() && u["state"] != "updating" {
            return u;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the update didn't settle: {}", row(main, id));
}

#[test]
fn both_sides_do_what_both_have() {
    let theirs = vec!["files".to_string(), "nodes.update".to_string(), "later.thing".to_string()];
    assert_eq!(crate::nodes::intersect(&["files", "mods", "nodes.update"], &theirs), vec!["files", "nodes.update"]);
    assert!(crate::nodes::intersect(&["files"], &[]).is_empty());
}

/// A node one tunnel version behind still talks; two apart is refused at
/// the hello, both ways.
#[tokio::test]
async fn one_tunnel_version_apart_talks_two_do_not() {
    async fn try_pair(main: u16, node: u16) -> Result<(), MuxError> {
        let (a, b) = mux::mem::pair(None);
        let (m, d) = tokio::join!(
            mux::start(Side::Main, Config { tunnel: main, ..Config::default() }, a.0, a.1),
            mux::start(Side::Node, Config { tunnel: node, version: "0.1.0".into(), ..Config::default() }, b.0, b.1)
        );
        // The main's side names the versions; the node's sees a goodbye.
        m.map(|_| ())?;
        d.map(|_| ())
    }
    let v = TUNNEL_VERSION;
    assert!(try_pair(v + 1, v).await.is_ok());
    assert!(try_pair(v, v + 1).await.is_ok());
    match try_pair(v + 2, v).await {
        Err(MuxError::Version { ours, theirs, peer }) => assert_eq!((ours, theirs, peer.as_str()), (v + 2, v, "0.1.0")),
        r => panic!("expected a version refusal, got {:?}", r.err()),
    }
}

/// Two behind: the row says "Update needed" with what to do, requests are
/// refused with the same reason (reads too: nothing reaches it), and the
/// main can't update it from here.
#[tokio::test]
async fn two_behind_is_shown_read_only() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let ns = p.ns();
    // The connection drops and the node comes back too old.
    p.main.app.nodes.too_far(&p.id, Skew { theirs: TUNNEL_VERSION, ours: TUNNEL_VERSION + 2, version: "0.40.0".into() });
    crate::nodes::detach_for_test(&p.main.app, &p.id);
    let r = row(&p.main, &p.id);
    assert_eq!(r["state"], "update_needed", "{r}");
    assert!(r["skew"].as_str().unwrap().contains("too old for this panel"), "{r}");
    assert_eq!(r["can_update"], false);
    let (_, heads) = p.main.app.nodes.live_view(&Who::Owner);
    assert_eq!(heads[0]["state"], "update_needed");
    for (m, path) in [("GET", format!("/api/v1/fs/{ns}/list?path=")), ("POST", format!("/api/v1/servers/{ns}/power"))] {
        let (s, b) = send(&p.main, &p.main_cookie, m, &path, Some(json!({ "action": "start" }))).await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{m} {path}: {b}");
        assert!(b["error"].as_str().unwrap().contains("Update it on the node itself"), "{b}");
    }
    let (s, _) = send(&p.main, &p.main_cookie, "POST", &format!("/api/v1/nodes/{}/update", p.id), None).await;
    assert_eq!(s, StatusCode::CONFLICT);
}

/// "Update" waits for the node's own update and records how it went.
#[tokio::test]
async fn update_runs_the_nodes_own_update() {
    let main = panel_with(TLS());
    let cookie = cookie_login(&main).await;
    let lines = vec![json!({ "state": "downloading" }), json!({ "state": "ready" })];
    let n = up_node(&main, "vm", Ceiling::Manage, "0.1.0", all_caps(), lines).await;
    assert_eq!(row(&main, &n.node.id)["can_update"], true);
    let (s, b) = send(&main, &cookie, "POST", &format!("/api/v1/nodes/{}/update", n.node.id), None).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let u = settled(&main, &n.node.id).await;
    assert_eq!(u["state"], "done", "{u}");
    assert!(u["message"].as_str().unwrap().contains("Installed hopperd"), "{u}");
    assert_eq!(u["target"], crate::nodes::main_version());
    assert_eq!(n.asked.load(Ordering::SeqCst), 1);
}

/// A node that restarts into the new version counts as done once it is
/// back on that version.
#[tokio::test]
async fn update_is_done_when_the_node_is_back_on_the_new_version() {
    let main = panel_with(TLS());
    let cookie = cookie_login(&main).await;
    let lines = vec![json!({ "state": "downloading" }), json!({ "state": "restarting" })];
    let n = up_node(&main, "vm", Ceiling::Manage, "0.1.0", all_caps(), lines).await;
    let (s, _) = send(&main, &cookie, "POST", &format!("/api/v1/nodes/{}/update", n.node.id), None).await;
    assert_eq!(s, StatusCode::OK);
    for _ in 0..50 {
        if row(&main, &n.node.id)["update"]["message"] == "Restarting" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(row(&main, &n.node.id)["update"]["state"], "updating");
    // It comes back over a new connection, on the main's version.
    let (a, b) = mux::mem::pair(None);
    let (m, d) = tokio::join!(mux::start(Side::Main, Config::default(), a.0, a.1), mux::start(Side::Node, Config::default(), b.0, b.1));
    let (m, d) = (m.unwrap(), d.unwrap());
    let _feed = crate::nodes::attach_for_test_as(&main.app, &n.node, m.handle.clone(), &crate::nodes::main_version(), all_caps());
    let u = settled(&main, &n.node.id).await;
    assert_eq!(u["state"], "done", "{u}");
    assert!(u["message"].as_str().unwrap().contains("runs hopperd"), "{u}");
    drop((m, d));
}

/// "Update all" goes one node at a time and stops at the first failure:
/// the node after it is never asked.
#[tokio::test]
async fn update_all_stops_at_the_first_failure() {
    let main = panel_with(TLS());
    let cookie = cookie_login(&main).await;
    let bad = up_node(&main, "first", Ceiling::Manage, "0.1.0", all_caps(), vec![json!({ "state": "error", "error": "the download failed" })]).await;
    let good = up_node(&main, "second", Ceiling::Manage, "0.1.0", all_caps(), vec![json!({ "state": "ready" })]).await;
    let (s, b) = send(&main, &cookie, "POST", "/api/v1/nodes/update-all", None).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["count"], 2);
    let mut all = Value::Null;
    for _ in 0..100 {
        all = main.app.nodes.list("")["update_all"].clone();
        if all["state"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(all["state"], "stopped", "{all}");
    assert!(all["message"].as_str().unwrap().contains("the download failed"), "{all}");
    assert!(all["message"].as_str().unwrap().contains("1 more not updated"), "{all}");
    assert_eq!(bad.asked.load(Ordering::SeqCst), 1);
    assert_eq!(good.asked.load(Ordering::SeqCst), 0);
    assert_eq!(settled(&main, &bad.node.id).await["state"], "failed");
}

/// Update only what both can do (an older node without "nodes.update"),
/// only under Manage, and only a node that is behind.
#[tokio::test]
async fn update_is_offered_only_where_it_can_work() {
    let main = panel_with(TLS());
    let cookie = cookie_login(&main).await;
    let old = up_node(&main, "old", Ceiling::Manage, "0.1.0", vec!["files".into()], vec![]).await;
    let low = up_node(&main, "low", Ceiling::Operate, "0.1.0", all_caps(), vec![]).await;
    let same = up_node(&main, "same", Ceiling::Manage, &crate::nodes::main_version(), all_caps(), vec![]).await;
    for n in [&old, &low, &same] {
        assert_eq!(row(&main, &n.node.id)["can_update"], false, "{}", n.node.name);
        let (s, b) = send(&main, &cookie, "POST", &format!("/api/v1/nodes/{}/update", n.node.id), None).await;
        assert_eq!(s, StatusCode::CONFLICT, "{}: {b}", n.node.name);
        assert_eq!(n.asked.load(Ordering::SeqCst), 0);
    }
    assert!(!row(&main, &old.node.id)["capabilities"].as_array().unwrap().iter().any(|c| c == "nodes.update"));
    let (s, _) = send(&main, &cookie, "POST", "/api/v1/nodes/update-all", None).await;
    assert_eq!(s, StatusCode::CONFLICT);
}

/// The node's side: an update from the main needs the Manage level and a
/// lead; with both, the node runs its own checks (a development build
/// doesn't update itself, so nothing is downloaded here).
#[tokio::test]
async fn the_node_takes_an_update_only_at_manage_from_a_lead() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    async fn ask(p: &super::Pair, lead: bool, role: &str) -> Value {
        let head = crate::relay::Head { k: "update".into(), user: "owner".into(), role: role.into(), lead, version: "99.0.0".into(), ..Default::default() };
        let mut s = p._tunnels.0.handle.open(serde_json::to_vec(&head).unwrap()).await.unwrap();
        let _ = s.tx.finish().await;
        crate::relay::Lines::new(s.rx).next().await.unwrap_or_default()
    }
    let v = ask(&p, false, "admin").await;
    assert_eq!(v["state"], "refused", "{v}");
    assert!(v["error"].as_str().unwrap().contains("Manage level"), "{v}");
    *p.ceiling.lock().unwrap() = Some(Role::Operator);
    let v = ask(&p, true, "admin").await;
    assert_eq!(v["state"], "refused", "{v}");
    *p.ceiling.lock().unwrap() = Some(Role::Admin);
    let v = ask(&p, true, "admin").await;
    assert_eq!(v["state"], "refused", "{v}");
    assert!(v["error"].as_str().unwrap().contains("development build"), "{v}");
    assert_eq!(crate::autoupdate::status().state, "", "nothing started");
}
