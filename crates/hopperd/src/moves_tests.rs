//! Moving and copying servers between a main and its node, both through
//! their real routers over an in-memory tunnel (design/multi-panel.md, A5):
//! a cut link resumes with only the missing chunks, links and modes
//! survive, links out of the folder are refused, a failed move leaves
//! nothing on the target, both Histories get the move, and only people
//! allowed both ends (under Manage) can move.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::auth::Role;
use crate::cross_origin_tests::{OWN, call, user_cookie};
use crate::nodes::Ceiling;
use crate::relay_tests::{Pair, other_node, pair, send};

fn random(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).unwrap();
    b
}

/// Fills a server with a few megabytes of chunks, one file twice.
fn fill(srv: &Path) {
    std::fs::create_dir_all(srv.join("world/region")).unwrap();
    for i in 0..4 {
        std::fs::write(srv.join(format!("world/region/r.{i}.0.mca")), random(700 * 1024)).unwrap();
    }
    let same = random(300 * 1024);
    std::fs::write(srv.join("mods-a.jar"), &same).unwrap();
    std::fs::write(srv.join("mods-b.jar"), &same).unwrap();
    std::fs::write(srv.join("empty.txt"), b"").unwrap();
    // Backups stay unless asked for.
    std::fs::create_dir_all(srv.join(hopper_core::hosting::backup::DIR)).unwrap();
    std::fs::write(srv.join(hopper_core::hosting::backup::DIR).join("b.zip"), random(10_000)).unwrap();
}

async fn wait_end(p: &Pair, mid: &str) -> Value {
    for _ in 0..600 {
        let (_, m) = send(&p.main, &p.main_cookie, "GET", &format!("/api/v1/moves/{mid}"), None).await;
        if matches!(m["state"].as_str(), Some("done" | "failed" | "paused" | "cancelled")) {
            return m;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the move didn't end");
}

/// Every file under `root` (relative, `/`) with its bytes, leaving out
/// the folders a move doesn't carry or writes itself.
fn files(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let rel = e.path().strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            // server.properties gets a free port on the target.
            if rel.starts_with(".hopper-") || rel == "server.properties" {
                continue;
            }
            let m = std::fs::symlink_metadata(e.path()).unwrap();
            if m.is_dir() {
                stack.push(e.path());
            } else if m.is_file() {
                out.push((rel, std::fs::read(e.path()).unwrap()));
            }
        }
    }
    out.sort();
    out
}

fn history_notes(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(root.join(".hopper-backups")).into_iter().flatten().flatten() {
        if let Ok(b) = std::fs::read(e.path().join(".records.json")) {
            let v: Value = serde_json::from_slice(&b).unwrap();
            for r in v.as_array().unwrap() {
                out.push(format!("{} {}", r["did"].as_str().unwrap_or(""), r["note"].as_str().unwrap_or("")));
            }
        }
    }
    out
}

fn main_srv(p: &Pair) -> PathBuf {
    p.main.dir.join("servers").join("Test Server")
}

fn node_servers(p: &Pair) -> PathBuf {
    p.node.dir.join("servers")
}

/// Main to node, with the link cut partway: it goes on by itself and sends
/// only what was missing; the files arrive the same; both Histories say
/// so; the old copy is kept, stopped, marked, and won't start until kept.
/// Then a copy back: the source stays as it was.
#[tokio::test]
async fn a_move_survives_a_cut_link_and_sends_only_whats_missing() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let src = main_srv(&p);
    fill(&src);
    let before = files(&src);
    let (s, c) = send(&p.main, &p.main_cookie, "POST", "/api/v1/moves/precheck", Some(json!({ "server": p.main.server, "to": p.id, "mode": "move" }))).await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["ok"], true, "{c}");
    let keys: Vec<&str> = c["rows"].as_array().unwrap().iter().filter_map(|r| r["key"].as_str()).collect();
    assert!(keys.contains(&"disk") && keys.contains(&"state") && keys.contains(&"backups"), "{keys:?}");
    // The link breaks after about 300 KB of the first batch.
    crate::moves::CUT.lock().unwrap().push((p.main.app.data_dir.clone(), 300 * 1024));
    let (s, m) = send(&p.main, &p.main_cookie, "POST", "/api/v1/moves", Some(json!({ "server": p.main.server, "to": p.id, "mode": "move" }))).await;
    assert_eq!(s, StatusCode::OK, "{m}");
    let m = wait_end(&p, m["id"].as_str().unwrap()).await;
    assert_eq!(m["state"], "done", "{m}");
    assert!(m["resumed"].as_u64().unwrap() >= 1, "{m}");
    // Sent: every distinct chunk once, plus at most the one cut off midway.
    let (unique, chunks, sent) = (m["total"].as_u64().unwrap(), m["chunks"].as_u64().unwrap(), m["sent"].as_u64().unwrap());
    let frames = unique + 68 * chunks;
    assert!(sent >= frames && sent <= frames + hopper_core::hosting::chunkstore::MAX_CHUNK as u64 + 68, "sent {sent}, chunks {frames}");
    // The same file twice went once.
    let all: u64 = before.iter().map(|(_, b)| b.len() as u64).sum();
    assert!(unique < all, "{unique} < {all}");
    // On the node, under a free name; backups stayed behind.
    let dest = node_servers(&p).join("Test Server 2");
    assert_eq!(files(&dest), before);
    assert!(!dest.join(hopper_core::hosting::backup::DIR).exists());
    assert!(!node_servers(&p).join(crate::transfer::STAGING).read_dir().is_ok_and(|mut d| d.next().is_some()));
    // Its port was taken on the node (the node's own test server): it got another.
    let port = |d: &Path| hopper_core::hosting::Properties::load(&d.join("server.properties")).unwrap().port();
    assert_eq!(m["new_port"].as_u64().unwrap() as u16, port(&dest));
    assert_ne!(port(&dest), port(&src));
    let new_id = m["new_id"].as_str().unwrap();
    assert!(new_id.starts_with(&format!("{}:", p.id)), "{new_id}");
    // Both Histories.
    let there = history_notes(&dest);
    assert!(there.iter().any(|n| n.starts_with("Moved Here from")), "{there:?}");
    let here = history_notes(&src);
    assert!(here.iter().any(|n| n.starts_with("Moved To vm.")), "{here:?}");
    // The History lists say Moved, with no Undo.
    let (_, h) = send(&p.main, &p.main_cookie, "GET", &format!("/api/v1/servers/{}/mod-backups", p.main.server), None).await;
    let text = h.to_string();
    assert!(text.contains("\"Moved\"") && text.contains("\"undo\":false"), "{text}");
    // The old copy: marked, and it won't start until kept.
    let mark = crate::transfer::moved_mark(&src).expect("marked");
    assert_eq!(mark.to, "vm");
    let (s, b) = send(&p.main, &p.main_cookie, "POST", &format!("/api/v1/servers/{}/power", p.main.server), Some(json!({ "action": "start" }))).await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert!(b["error"].as_str().unwrap().contains("was moved to vm"), "{b}");
    let (s, _) = send(&p.main, &p.main_cookie, "DELETE", &format!("/api/v1/servers/{}/moved", p.main.server), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(crate::transfer::moved_mark(&src).is_none());

    // Copy it back from the node: the node's copy stays as it was.
    crate::nodes::took_for_test(&p.main.app, &p.id, { p.node.app.rescan(); p.node.app.servers.list().iter().map(|e| json!(p.node.app.servers.summary(e))).collect() });
    let (s, m) = send(&p.main, &p.main_cookie, "POST", "/api/v1/moves", Some(json!({ "server": new_id, "to": "main", "mode": "copy" }))).await;
    assert_eq!(s, StatusCode::OK, "{m}");
    let m = wait_end(&p, m["id"].as_str().unwrap()).await;
    assert_eq!(m["state"], "done", "{m}");
    let back = p.main.dir.join("servers").join("Test Server 2");
    assert_eq!(files(&back), before);
    assert!(crate::transfer::moved_mark(&dest).is_none(), "a copy leaves its source unmarked");
    assert!(history_notes(&back).iter().any(|n| n.starts_with("Copied Copied here from vm")));
    assert!(!history_notes(&dest).iter().any(|n| n.starts_with("Copied") || n.starts_with("Moved To")));
}

/// Links inside the folder and file modes arrive as they were; a link
/// that points outside stops the move before anything is sent.
#[tokio::test]
async fn links_and_modes_survive_and_links_out_are_refused() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let src = main_srv(&p);
    std::fs::write(src.join("start.sh"), b"#!/bin/sh\necho hi\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(src.join("start.sh"), std::fs::Permissions::from_mode(0o750)).unwrap();
    }
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink("hello.txt", src.join("link.txt")).is_ok();
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_file("hello.txt", src.join("link.txt")).is_ok();
    // One outside first.
    let outside = p.main.dir.join("outside.txt");
    std::fs::write(&outside, b"secret").unwrap();
    #[cfg(unix)]
    let out_link = std::os::unix::fs::symlink("../../outside.txt", src.join("out.txt")).is_ok();
    #[cfg(windows)]
    let out_link = std::os::windows::fs::symlink_file("..\\..\\outside.txt", src.join("out.txt")).is_ok();
    if out_link {
        let (_, c) = send(&p.main, &p.main_cookie, "POST", "/api/v1/moves/precheck", Some(json!({ "server": p.main.server, "to": p.id }))).await;
        assert_eq!(c["ok"], false, "{c}");
        assert!(c["rows"].as_array().unwrap().iter().any(|r| r["key"] == "links" && r["level"] == "fail"), "{c}");
        let (s, b) = send(&p.main, &p.main_cookie, "POST", "/api/v1/moves", Some(json!({ "server": p.main.server, "to": p.id }))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
        std::fs::remove_file(src.join("out.txt")).unwrap();
    }
    let (s, m) = send(&p.main, &p.main_cookie, "POST", "/api/v1/moves", Some(json!({ "server": p.main.server, "to": p.id, "mode": "copy" }))).await;
    assert_eq!(s, StatusCode::OK, "{m}");
    let m = wait_end(&p, m["id"].as_str().unwrap()).await;
    assert_eq!(m["state"], "done", "{m}");
    let dest = node_servers(&p).join("Test Server 2");
    assert_eq!(std::fs::read(dest.join("start.sh")).unwrap(), b"#!/bin/sh\necho hi\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(std::fs::metadata(dest.join("start.sh")).unwrap().permissions().mode() & 0o7777, 0o750);
    }
    if linked {
        let l = std::fs::symlink_metadata(dest.join("link.txt")).unwrap();
        assert!(l.file_type().is_symlink());
        assert_eq!(std::fs::read_link(dest.join("link.txt")).unwrap().to_string_lossy().replace('\\', "/"), "hello.txt");
    }
    assert!(!dest.join("out.txt").exists());
    println!("links checked: {linked}, link out checked: {out_link}");
}

/// A move whose source can't be read any more fails, and the node keeps
/// nothing: no staging folder, no new server. The source may start again.
#[tokio::test]
async fn a_failed_move_leaves_nothing_on_the_target() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let src = main_srv(&p);
    fill(&src);
    crate::transfer::TAMPER.lock().unwrap().push(src.canonicalize().unwrap());
    let (s, m) = send(&p.main, &p.main_cookie, "POST", "/api/v1/moves", Some(json!({ "server": p.main.server, "to": p.id }))).await;
    assert_eq!(s, StatusCode::OK, "{m}");
    let m = wait_end(&p, m["id"].as_str().unwrap()).await;
    crate::transfer::TAMPER.lock().unwrap().retain(|t| !t.starts_with(src.canonicalize().unwrap()));
    assert_eq!(m["state"], "failed", "{m}");
    assert!(m["error"].as_str().unwrap().contains("changed while it was being sent"), "{m}");
    let staged: Vec<_> = std::fs::read_dir(node_servers(&p).join(crate::transfer::STAGING)).map(|d| d.flatten().collect()).unwrap_or_default();
    assert!(staged.is_empty(), "{staged:?}");
    assert!(!node_servers(&p).join("Test Server 2").exists());
    assert!(!crate::transfer::is_sending(&src) && crate::transfer::moved_mark(&src).is_none());
    assert!(!p.main.app.data_dir.join("transfers").read_dir().is_ok_and(|mut d| d.next().is_some()), "the source's list is dropped");
}

/// Only the owner or a manager who sees both ends, under Manage on every
/// node it touches.
#[tokio::test]
async fn only_people_allowed_both_ends_can_move() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let y = other_node(&p);
    let ns = p.ns();
    // Someone with Manage on node X only (managers are never limited):
    // making and removing servers there is the owner's and managers', so
    // moving is too, even between servers of their own node.
    let (s, u) = send(&p.main, &p.main_cookie, "POST", "/api/v1/users", Some(json!({ "name": "max", "password": "maxpass123", "role": "admin", "servers": [format!("node:{}", p.id)] }))).await;
    assert_eq!(s, StatusCode::OK, "{u}");
    let max = user_cookie(&p.main, "max", "maxpass123").await;
    let pre = |cookie: String, server: String, to: String| {
        let main = &p.main;
        async move { send(main, &cookie, "POST", "/api/v1/moves/precheck", Some(json!({ "server": server, "to": to }))).await }
    };
    assert_eq!(pre(max.clone(), p.main.server.clone(), p.id.clone()).await.0, StatusCode::FORBIDDEN);
    assert_eq!(pre(max.clone(), ns.clone(), "main".into()).await.0, StatusCode::FORBIDDEN);
    assert_eq!(pre(max.clone(), ns.clone(), y.clone()).await.0, StatusCode::FORBIDDEN);
    let (s, _) = send(&p.main, &max, "POST", "/api/v1/moves", Some(json!({ "server": ns, "to": y }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = send(&p.main, &max, "GET", &format!("/api/v1/moves/places?server={ns}"), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // The owner may: node Y is listed, offline (it never connected).
    let (_, places) = send(&p.main, &p.main_cookie, "GET", &format!("/api/v1/moves/places?server={ns}"), None).await;
    let y_row = places.as_array().unwrap().iter().find(|x| x["id"] == y.as_str()).expect("node Y listed");
    assert_eq!(y_row["ok"], false, "{places}");
    assert_eq!(pre(p.main_cookie.clone(), ns.clone(), y.clone()).await.0, StatusCode::SERVICE_UNAVAILABLE);
    // An operator (all servers) and a token: no moves at all.
    let (_, _) = send(&p.main, &p.main_cookie, "POST", "/api/v1/users", Some(json!({ "name": "ola", "password": "olapass123", "role": "operator" }))).await;
    let ola = user_cookie(&p.main, "ola", "olapass123").await;
    assert_eq!(pre(ola, p.main.server.clone(), p.id.clone()).await.0, StatusCode::FORBIDDEN);
    let (_, b) = send(&p.main, &p.main_cookie, "POST", "/api/v1/tokens", Some(json!({ "name": "agent" }))).await;
    let (s, _, _) = call("POST", "/api/v1/moves/precheck").bearer(b["token"].as_str().unwrap()).json(json!({ "server": p.main.server, "to": p.id })).send(&p.main).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // People calling a move's steps themselves are refused on both panels.
    let (s, _) = send(&p.main, &p.main_cookie, "POST", &format!("/api/v1/servers/{ns}/transfer/check"), Some(json!({}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = send(&p.main, &p.main_cookie, "POST", &format!("/api/v1/servers/{}/transfer/check", p.main.server), Some(json!({}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = send(&p.node, &p.node_cookie, "POST", "/api/v1/receive/precheck", Some(json!({ "size": 1 }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

/// A Look ceiling refuses moves: on the main's copy, and on the node's own
/// when the main's copy still says Manage.
#[tokio::test]
async fn a_look_ceiling_refuses_moves() {
    let p = pair(Ceiling::Look, Some(Role::Viewer)).await;
    let (s, b) = send(&p.main, &p.main_cookie, "POST", "/api/v1/moves/precheck", Some(json!({ "server": p.main.server, "to": p.id }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{b}");
    assert!(b["error"].as_str().unwrap().contains("Manage"), "{b}");
    let q = pair(Ceiling::Manage, Some(Role::Viewer)).await;
    let (s, b) = send(&q.main, &q.main_cookie, "POST", "/api/v1/moves/precheck", Some(json!({ "server": q.main.server, "to": q.id }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{b}");
    // Nothing was made there.
    assert!(!q.node.dir.join("servers").join(crate::transfer::STAGING).exists());
}

/// The node checks a file list itself: a path that climbs out, a link
/// that points out, or a file under a link is refused before anything is
/// written.
#[tokio::test]
async fn the_node_refuses_lists_that_climb_out() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let r = crate::api::router(p.main.app.clone());
    let h = "a".repeat(64);
    let bad = [
        json!({ "name": "x", "size": 1, "unique": 1, "chunks": 1, "items": [{ "p": "../escape.txt", "k": "f", "s": 1, "c": [[h, 1]] }] }),
        json!({ "name": "x", "size": 0, "unique": 0, "chunks": 0, "items": [{ "p": "w", "k": "d" }, { "p": "w/../../escape.txt", "k": "f" }] }),
        json!({ "name": "x", "size": 0, "unique": 0, "chunks": 0, "items": [{ "p": "l", "k": "l", "l": "../../etc" }] }),
        json!({ "name": "x", "size": 1, "unique": 1, "chunks": 1, "items": [{ "p": "l", "k": "l", "l": "." }, { "p": "l/f", "k": "f", "s": 1, "c": [[h, 1]] }] }),
        json!({ "name": "x", "size": 0, "unique": 0, "chunks": 0, "items": [{ "p": "C:/Windows", "k": "d" }] }),
    ];
    for (i, plan) in bad.iter().enumerate() {
        let (st, body) = crate::relay::engine_call(&p.main.app, &r, Some(&p.id), "owner", Method::PUT, &format!("/api/v1/receive/t{i}/manifest?mode=move&from=x"), axum::body::Body::from(plan.to_string())).await.unwrap();
        let b = axum::body::to_bytes(body, 1 << 20).await.unwrap();
        assert_eq!(st, StatusCode::BAD_REQUEST, "{i}: {}", String::from_utf8_lossy(&b));
        assert!(!node_servers(&p).join(crate::transfer::STAGING).join(format!("t{i}")).exists(), "{i}");
    }
    assert!(!p.node.dir.join("escape.txt").exists() && !p.node.dir.join("servers").join("escape.txt").exists());
    // And a move's step from a browser through the main never reaches it.
    let (s, _, _) = call("PUT", format!("/api/v1/servers/{}/transfer/x/manifest", p.ns())).origin(OWN).h("cookie", format!("hopperd_session={}", p.main_cookie)).h("x-hopperd", "1").send(&p.main).await;
    assert_ne!(s, StatusCode::OK);
}
