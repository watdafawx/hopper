//! Files streamed through the main (design/multi-panel.md, A3): a body over
//! the old one-go cap gets through, a node that reads slowly holds the main
//! to one window instead of the whole body, and a cancelled upload leaves
//! nothing on the node.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt as _;

use super::{Fake, TLS, pair};
use crate::auth::Role;
use crate::cross_origin_tests::{OWN, T, body, cookie_login, panel_with};
use crate::nodes::{Ceiling, Follow};
use crate::tunnel::mux::{self, Config, Side};

/// Sends `b` as a raw body to `path` on `t`, signed in with `cookie`.
async fn send_body(t: &T, cookie: &str, method: &str, path: &str, b: Body) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("host", crate::cross_origin_tests::HOST)
        .header("origin", OWN)
        .header("cookie", format!("hopperd_session={cookie}"))
        .header("x-hopperd", "1")
        .header("content-type", "application/octet-stream")
        .body(b)
        .unwrap();
    req.extensions_mut().insert(ConnectInfo(std::net::SocketAddr::from(([192, 168, 50, 21], 50000))));
    let res = crate::api::router(t.app.clone()).oneshot(req).await.unwrap();
    let (parts, b) = res.into_parts();
    let bytes = axum::body::to_bytes(b, 64 * 1024 * 1024).await.unwrap_or_default();
    (parts.status, body(&bytes))
}

/// A jar's bytes: a zip's first four, then `n` more.
fn jar(n: usize) -> Vec<u8> {
    let mut v = b"PK\x03\x04".to_vec();
    v.resize(4 + n, 7);
    v
}

/// A jar bigger than the main used to take in one go (32 MB) reaches the
/// node whole: the main streams it instead of reading it first.
#[tokio::test]
async fn a_body_over_the_old_cap_gets_through() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let size = crate::relay::MAX_BODY + 8 * 1024 * 1024;
    let data = jar(size);
    let path = format!("/api/v1/servers/{}/mods/upload?name=big.jar&conflict=replace", p.ns());
    let (s, v) = send_body(&p.main, &p.main_cookie, "POST", &path, Body::from(data)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let landed = p.node.dir.join("servers").join("Test Server").join("mods").join("big.jar");
    assert_eq!(std::fs::metadata(&landed).unwrap().len() as usize, size + 4);
    // An upload chunk still streams too, and lands where the node puts it.
    let (s, v) = super::send(&p.main, &p.main_cookie, "POST", &format!("/api/v1/fs/{}/uploads", p.ns()), Some(serde_json::json!({ "path": "up.bin", "size": 3 }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let up = v["id"].as_str().unwrap().to_string();
    let (s, v) = send_body(&p.main, &p.main_cookie, "PUT", &format!("/api/v1/fs/{}/uploads/{up}?offset=0", p.ns()), Body::from(b"abc".to_vec())).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(std::fs::read(p.node.dir.join("servers").join("Test Server").join("up.bin")).unwrap(), b"abc");
}

/// A body made of `chunk`-sized pieces, as many as are asked for, counting
/// the bytes the main has taken.
fn endless(chunk: usize, taken: Arc<AtomicUsize>, fail_after: Option<usize>) -> Body {
    let mut first = true;
    let s = futures_util::stream::unfold(0usize, move |n| {
        let taken = taken.clone();
        let piece = if std::mem::take(&mut first) { jar(chunk - 4) } else { vec![7u8; chunk] };
        async move {
            if fail_after.is_some_and(|f| n >= f) {
                // Give the node a moment to write what it has, then break off.
                tokio::time::sleep(Duration::from_millis(100)).await;
                return Some((Err(std::io::Error::other("the browser went away")), n + 1));
            }
            taken.fetch_add(piece.len(), Ordering::SeqCst);
            Some((Ok::<_, std::io::Error>(axum::body::Bytes::from(piece)), n + 1))
        }
    });
    Body::from_stream(s)
}

/// A node that takes `read` bytes of each request body and then stops
/// reading (a slow disk), never answering.
async fn slow_node(read: usize) -> (Fake, Arc<AtomicUsize>) {
    let main = panel_with(TLS());
    let (_, key, _) = main.app.nodes.make_key(Ceiling::Manage, "owner");
    let (n, _) = main.app.nodes.use_key(&key, "vm", "10.0.0.9".parse().unwrap(), "").unwrap();
    let (a, b) = mux::mem::pair(None);
    let (m, d) = tokio::join!(mux::start(Side::Main, Config::default(), a.0, a.1), mux::start(Side::Node, Config::default(), b.0, b.1));
    let (m, mut d) = (m.unwrap(), d.unwrap());
    let mut incoming = std::mem::replace(&mut d.incoming, tokio::sync::mpsc::channel(1).1);
    let got = Arc::new(AtomicUsize::new(0));
    let g = got.clone();
    let _node = Follow(tokio::spawn(async move {
        while let Some(mut s) = incoming.recv().await {
            let head: Value = serde_json::from_slice(&s.head).unwrap_or_default();
            let g = g.clone();
            tokio::spawn(async move {
                if head["k"] == "req" {
                    let mut have = 0;
                    while have < read {
                        match s.rx.recv().await {
                            Ok(Some(d)) => have += d.len(),
                            _ => break,
                        }
                    }
                    g.store(have, Ordering::SeqCst);
                }
                std::future::pending::<()>().await;
            });
        }
    }));
    let _feed = crate::nodes::attach_for_test(&main.app, &n, m.handle.clone());
    let cookie = cookie_login(&main).await;
    let handle = d.handle.clone();
    (Fake { main, id: n.id.clone(), cookie, _feed, _node, _tunnels: (m, handle) }, got)
}

/// The main reads from the browser only as fast as the node takes it: with
/// a node that stopped reading after 1 MiB of an endless upload, the main
/// has taken at most that plus one window and its small send queue.
#[tokio::test]
async fn a_slow_node_holds_the_main_to_one_window() {
    let (f, node_read) = slow_node(1024 * 1024).await;
    let taken = Arc::new(AtomicUsize::new(0));
    let path = format!("/api/v1/servers/{}/mods/upload?name=a.jar", f.ns("srv"));
    let b = endless(64 * 1024, taken.clone(), None);
    let main = f.main.app.clone();
    let cookie = f.cookie.clone();
    let req = tokio::spawn(async move {
        let t = T { app: main, dir: std::env::temp_dir().join("hopperd-a3-none"), server: String::new() };
        let r = send_body(&t, &cookie, "POST", &path, b).await;
        std::mem::forget(t);
        r
    });
    // Let it run until nothing moves any more.
    let mut last = usize::MAX;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let now = taken.load(Ordering::SeqCst);
        if now == last && node_read.load(Ordering::SeqCst) > 0 {
            break;
        }
        last = now;
    }
    let took = taken.load(Ordering::SeqCst);
    let read = node_read.load(Ordering::SeqCst);
    assert!(read >= 1024 * 1024, "the node read {read}");
    // One window (512 KiB) waiting on the node, eight 64 KiB frames queued
    // to the socket, one chunk in the pump's hand and one being made.
    let bound = read + 512 * 1024 + 8 * 64 * 1024 + 2 * 64 * 1024;
    assert!(took <= bound, "the main took {took} bytes from the browser with the node at {read} (bound {bound})");
    req.abort();
}

/// An upload cut off on its way (the browser went away) resets the stream:
/// the node's handler sees an error and keeps nothing, not a cut-off jar.
#[tokio::test]
async fn a_cancelled_upload_leaves_nothing_on_the_node() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let taken = Arc::new(AtomicUsize::new(0));
    let path = format!("/api/v1/servers/{}/mods/upload?name=cut.jar&conflict=replace", p.ns());
    let (s, _) = send_body(&p.main, &p.main_cookie, "POST", &path, endless(256 * 1024, taken.clone(), Some(12))).await;
    assert_ne!(s, StatusCode::OK);
    let mods = p.node.dir.join("servers").join("Test Server").join("mods");
    let mut left = Vec::new();
    for _ in 0..50 {
        left = std::fs::read_dir(&mods).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
        if left.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(left.is_empty(), "left on the node: {left:?}");
    assert!(taken.load(Ordering::SeqCst) >= 12 * 256 * 1024);
}

/// Read bodies (JSON the main checks) are still read whole and capped: a
/// text save over the cap is refused on the main with the old message.
#[tokio::test]
async fn json_bodies_keep_their_cap() {
    let p = pair(Ceiling::Manage, Some(Role::Admin)).await;
    let big = vec![b'x'; crate::relay::MAX_BODY + 1];
    let (s, v) = send_body(&p.main, &p.main_cookie, "PUT", &format!("/api/v1/fs/{}/text", p.ns()), Body::from(big)).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "{v}");
}
