//! GitHub requests against a fake GitHub on a free loopback port: rate
//! limits, the ETag cache, where the token goes, and how many API calls a
//! group pack's GitHub mods cost.

use super::*;
use crate::sources::{Kind, ModSource};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

type Answer = (u16, Vec<(&'static str, String)>, Vec<u8>);

/// A fake GitHub: `answer` gets the path and the If-None-Match header.
/// Counts the API and website requests it got.
struct Fake {
    base: String,
    api: Arc<AtomicUsize>,
    web: Arc<AtomicUsize>,
}

fn fake(answer: impl Fn(&str, Option<&str>) -> Answer + Send + 'static) -> Fake {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}", server.server_addr().to_ip().unwrap());
    let (api, web) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (a2, w2) = (api.clone(), web.clone());
    std::thread::spawn(move || {
        for req in server.incoming_requests() {
            let url = req.url().to_string();
            if url.starts_with("/api/") {
                a2.fetch_add(1, Ordering::SeqCst);
            } else {
                w2.fetch_add(1, Ordering::SeqCst);
            }
            let inm = req.headers().iter().find(|h| h.field.equiv("If-None-Match")).map(|h| h.value.to_string());
            let (status, headers, body) = answer(&url, inm.as_deref());
            let mut resp = tiny_http::Response::from_data(body).with_status_code(status);
            for (k, v) in headers {
                resp.add_header(tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap());
            }
            let _ = req.respond(resp);
        }
    });
    set_test_base(Some(&base));
    Fake { base, api, web }
}

/// These tests share the cache, the pause and the fake's address.
fn locked() -> std::sync::MutexGuard<'static, ()> {
    let g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset_for_test();
    g
}

#[test]
fn rate_limit_headers_say_when_github_can_be_asked_again() {
    let now = 1_000_000;
    // The primary limit: 403 or 429 with nothing left, until the reset.
    assert_eq!(limit_reset(403, Some("0"), Some("1000600"), None, now), Some(1_000_600));
    assert_eq!(limit_reset(429, Some("0"), Some("1000600"), None, now), Some(1_000_600));
    // A secondary limit: retry-after.
    assert_eq!(limit_reset(403, Some("12"), None, Some("90"), now), Some(1_000_090));
    assert_eq!(limit_reset(429, None, None, Some("30"), now), Some(1_000_030));
    // 429 without headers: a minute. Used up without a reset: 15.
    assert_eq!(limit_reset(429, None, None, None, now), Some(1_000_060));
    assert_eq!(limit_reset(403, Some("0"), None, None, now), Some(now + 900));
    // A reset in the past or far off doesn't wait wrongly.
    assert_eq!(limit_reset(403, Some("0"), Some("5"), None, now), Some(now + 900));
    assert_eq!(limit_reset(403, Some("0"), Some("99999999999"), None, now), Some(now + 3600));
    // Not a limit: a private repo's 403, a 404, a 200.
    assert_eq!(limit_reset(403, Some("42"), Some("1000600"), None, now), None);
    assert_eq!(limit_reset(404, Some("0"), None, None, now), None);
    assert_eq!(limit_reset(200, Some("0"), None, None, now), None);
}

#[test]
fn a_rate_limit_pauses_github_calls_until_it_resets() {
    let _g = locked();
    let reset = now() + 600;
    let f = fake(move |_, _| (403, vec![("x-ratelimit-remaining", "0".into()), ("x-ratelimit-reset", reset.to_string())], b"{}".to_vec()));
    let e = get_text("https://api.github.com/repos/o/limited/releases?per_page=30", true).unwrap_err();
    assert_eq!(rate_limited(&e), Some(RateLimited { until: reset }));
    assert!(e.to_string().starts_with("GitHub is limiting downloads for now. Hopper tries again at "), "{e}");
    assert_eq!(paused_until(), Some(reset));
    // Every later call waits without asking.
    for i in 0..20 {
        let e = get_text(&format!("https://api.github.com/repos/o/r{i}/releases/tags/v1"), true).unwrap_err();
        assert_eq!(rate_limited(&e).map(|r| r.until), Some(reset));
    }
    assert_eq!(f.api.load(Ordering::SeqCst), 1);
    assert_eq!(github_calls(), 1);
    set_test_base(None);
}

#[test]
fn unchanged_answers_come_from_the_cache() {
    let _g = locked();
    let f = fake(|_, inm| match inm {
        Some("\"v1\"") => (304, vec![("etag", "\"v1\"".into())], Vec::new()),
        _ => (200, vec![("etag", "\"v1\"".into())], b"[]".to_vec()),
    });
    let url = "https://api.github.com/repos/o/etag/releases?per_page=30";
    assert_eq!(get_text(url, true).unwrap(), "[]");
    // Asked again: "changed since?", answered 304 (free), the cached body.
    assert_eq!(get_text(url, true).unwrap(), "[]");
    assert_eq!(f.api.load(Ordering::SeqCst), 2);
    // Within the fresh window nothing goes out at all.
    for _ in 0..5 {
        assert_eq!(get_text_fresh(url, true, LIST_FRESH).unwrap(), "[]");
    }
    assert_eq!(f.api.load(Ordering::SeqCst), 2);
    set_test_base(None);
}

/// A fake GitHub whose answer the test changes between requests: one
/// (etag, body) at a time. Records each request's If-None-Match and what it
/// answered (200 or 304).
type Seen = Arc<Mutex<Vec<(Option<String>, u16)>>>;

fn changing(first: (&str, &str)) -> (Fake, Arc<Mutex<(String, String)>>, Seen) {
    let now = Arc::new(Mutex::new((first.0.to_string(), first.1.to_string())));
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let (n2, s2) = (now.clone(), seen.clone());
    let f = fake(move |_, inm| {
        let (etag, body) = n2.lock().unwrap().clone();
        let status = if inm == Some(etag.as_str()) { 304 } else { 200 };
        s2.lock().unwrap().push((inm.map(str::to_string), status));
        (status, vec![("etag", etag)], if status == 304 { Vec::new() } else { body.into_bytes() })
    });
    (f, now, seen)
}

#[test]
fn a_check_by_hand_asks_github_inside_the_fresh_window() {
    let _g = locked();
    let (f, now_answer, seen) = changing(("\"v1\"", "[]"));
    let url = "https://api.github.com/repos/o/manual/releases?per_page=30";
    let auto = |u| get_text_fresh(u, true, Check::Auto.fresh(LIST_FRESH)).unwrap();
    let hand = |u| get_text_fresh(u, true, Check::Manual.fresh(LIST_FRESH)).unwrap();
    assert_eq!(auto(url), "[]");
    // A new release lands; the app's own look a moment later still uses the
    // answer it has (nothing goes out)...
    *now_answer.lock().unwrap() = ("\"v2\"".into(), "[1]".into());
    assert_eq!(auto(url), "[]");
    assert_eq!(f.api.load(Ordering::SeqCst), 1);
    // ...but the player's own check asks, with the cached ETag, and gets it.
    assert_eq!(hand(url), "[1]");
    assert_eq!(f.api.load(Ordering::SeqCst), 2);
    assert_eq!(seen.lock().unwrap()[1], (Some("\"v1\"".to_string()), 200));
    // The new answer is what the next automatic look uses too.
    assert_eq!(auto(url), "[1]");
    assert_eq!(f.api.load(Ordering::SeqCst), 2);
    set_test_base(None);
}

#[test]
fn a_check_by_hand_that_gets_304_keeps_the_cached_answer() {
    let _g = locked();
    let (f, _, seen) = changing(("\"v1\"", "[1]"));
    let url = "https://api.github.com/repos/o/same/releases?per_page=30";
    let hand = || get_text_fresh(url, true, Check::Manual.fresh(LIST_FRESH)).unwrap();
    assert_eq!(hand(), "[1]");
    for _ in 0..3 {
        assert_eq!(hand(), "[1]");
    }
    // Every look went out, and all but the first were answered 304: free.
    assert_eq!(f.api.load(Ordering::SeqCst), 4);
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0], (None, 200));
    assert!(seen[1..].iter().all(|s| *s == (Some("\"v1\"".to_string()), 304)), "{seen:?}");
    set_test_base(None);
}

#[test]
fn a_pause_holds_back_a_check_by_hand_and_says_so() {
    let _g = locked();
    let reset = now() + 600;
    let f = fake(move |url, _| {
        if url.contains("/limited/") {
            return (403, vec![("x-ratelimit-remaining", "0".into()), ("x-ratelimit-reset", reset.to_string())], b"{}".to_vec());
        }
        (200, vec![("etag", "\"v1\"".into())], b"[]".to_vec())
    });
    let url = "https://api.github.com/repos/o/cached/releases?per_page=30";
    assert_eq!(get_text(url, true).unwrap(), "[]");
    assert!(pause().is_none());
    // GitHub says stop (another repo's check hit the limit).
    assert!(get_text("https://api.github.com/repos/o/limited/releases?per_page=30", true).is_err());
    let calls = f.api.load(Ordering::SeqCst);
    let p = pause().expect("paused");
    assert_eq!(p.until, reset);
    assert!(p.paused_note().starts_with("GitHub is limiting checks for now. Mods from GitHub are checked again at "), "{}", p.paused_note());
    // The player's check can't ask: the cached answer stands in, nothing goes out.
    assert_eq!(get_text_fresh(url, true, Check::Manual.fresh(LIST_FRESH)).unwrap(), "[]");
    assert_eq!(f.api.load(Ordering::SeqCst), calls);
    set_test_base(None);
}

/// The reported bug end to end: a release made after the last look was
/// hidden from "check for updates" for ten minutes.
#[test]
fn a_new_release_shows_in_a_check_by_hand_right_away() {
    let _g = locked();
    let list = |tags: &[&str]| {
        serde_json::Value::Array(
            tags.iter()
                .map(|t| {
                    serde_json::json!({
                        "tag_name": t, "html_url": format!("https://github.com/o/mc-mcp/releases/tag/{t}"),
                        "assets": [{ "name": format!("mc-mcp-{t}.jar"), "size": 4, "browser_download_url": format!("https://github.com/o/mc-mcp/releases/download/{t}/mc-mcp-{t}.jar") }]
                    })
                })
                .collect(),
        )
        .to_string()
    };
    let (f, now_answer, _) = changing(("\"a\"", &list(&["v0.1.1"])));
    let src = ModSource { kind: Kind::GitHub, url: "o/mc-mcp".into(), ..Default::default() };
    let newest = |check| crate::sources::github::candidates(&src, check).unwrap()[0].version.clone();
    assert_eq!(newest(Check::Auto), "v0.1.1");
    *now_answer.lock().unwrap() = ("\"b\"".into(), list(&["v0.2.0", "v0.1.1"]));
    assert_eq!(newest(Check::Auto), "v0.1.1", "an automatic look inside the window uses the cache");
    assert_eq!(newest(Check::Manual), "v0.2.0");
    assert_eq!(f.api.load(Ordering::SeqCst), 2);
    set_test_base(None);
}

#[test]
fn the_token_goes_only_to_githubs_api() {
    // A fake server under a test isn't GitHub's API: no token header.
    let _g = locked();
    set_session_github_token("ghp_test_only");
    let seen = Arc::new(Mutex::new(None));
    let s2 = seen.clone();
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}", server.server_addr().to_ip().unwrap());
    std::thread::spawn(move || {
        if let Some(req) = server.incoming_requests().next() {
            *s2.lock().unwrap() = Some(req.headers().iter().any(|h| h.field.equiv("Authorization")));
            let _ = req.respond(tiny_http::Response::from_string("[]"));
        }
    });
    set_test_base(Some(&base));
    get_text("https://api.github.com/repos/o/tok/releases?per_page=30", true).unwrap();
    assert_eq!(*seen.lock().unwrap(), Some(false));
    set_session_github_token("");
    set_test_base(None);
}

fn release(repo: &str, base: &str, sha: &str) -> String {
    let name = format!("{}-1.0.jar", repo.rsplit('/').next().unwrap());
    serde_json::json!({
        "tag_name": "v1",
        "html_url": format!("https://github.com/{repo}/releases/tag/v1"),
        "assets": [{
            "name": name,
            "size": 4,
            "browser_download_url": format!("{base}/web/{repo}/releases/download/v1/{name}"),
            "digest": format!("sha256:{sha}"),
        }]
    })
    .to_string()
}

/// A group pack with 10 GitHub mods arriving at a member: the old way asks
/// the API once per mod; the pin's own tag, file name and sha256 now
/// download straight from the release link, with no API call.
#[test]
fn ten_github_mods_from_a_group_need_no_api_calls() {
    let _g = locked();
    let body = b"jar!".to_vec();
    let sha = {
        use sha2::{Digest, Sha256};
        Sha256::digest(&body).iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    let base = Arc::new(Mutex::new(String::new()));
    let (b2, s2, body2) = (base.clone(), sha.clone(), body.clone());
    let f = fake(move |url, _| {
        if let Some(rest) = url.strip_prefix("/api/repos/") {
            let repo: Vec<&str> = rest.split('/').take(2).collect();
            return (200, vec![], release(&repo.join("/"), &b2.lock().unwrap(), &s2).into_bytes());
        }
        (200, vec![], body2.clone())
    });
    *base.lock().unwrap() = f.base.clone();
    let pins: Vec<(ModSource, String)> = (0..10)
        .map(|i| {
            let src = ModSource {
                kind: Kind::GitHub,
                url: format!("dev/mod{i}"),
                publisher: format!("dev/mod{i}"),
                seen: "v1".into(),
                sha256: sha.clone(),
                ..Default::default()
            };
            (src, format!("mod{i}-1.0.jar"))
        })
        .collect();

    // Before: one API call per mod.
    for (src, file) in &pins {
        let c = crate::sources::pinned_candidate(src, file).unwrap();
        assert_eq!(c.digest, sha);
    }
    assert_eq!(f.api.load(Ordering::SeqCst), 10);

    // After: none; the downloads come from the website's links.
    reset_for_test();
    f.api.store(0, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("hopper-ghdirect-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    for (src, file) in &pins {
        let c = crate::sources::github_direct(src, file).unwrap();
        assert_eq!(c.digest, sha);
        assert!(c.url.starts_with(&format!("{}/web/{}/releases/download/v1/", f.base, src.url)), "{}", c.url);
        let dest = dir.join(file);
        crate::cf::download_checked(&c.url, &dest, &Default::default(), None, |_| {}).unwrap();
        assert_eq!(crate::sources::sha256_file(&dest).unwrap(), c.digest);
    }
    assert_eq!(f.api.load(Ordering::SeqCst), 0);
    assert_eq!(github_calls(), 0);
    assert_eq!(f.web.load(Ordering::SeqCst), 10);

    // A download that isn't the pinned file is not kept.
    let wrong = crate::sources::SrcFile { digest: "0".repeat(64), ..Default::default() };
    let e = crate::sources::verify_download(&dir.join("mod0-1.0.jar"), &wrong).unwrap_err();
    assert!(e.to_string().contains("checksum"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
    set_test_base(None);
}

#[test]
fn pins_without_what_a_download_needs_still_look_it_up() {
    let full = ModSource { kind: Kind::GitHub, url: "o/r".into(), seen: "v1.2+mc1.21".into(), sha256: "a".repeat(64), ..Default::default() };
    let c = crate::sources::github_direct(&full, "r 1.2.jar").unwrap();
    assert!(c.url.ends_with("/o/r/releases/download/v1.2%2Bmc1.21/r%201.2.jar"), "{}", c.url);
    // An older pin or an odd one: the API lookup as before.
    for src in [
        ModSource { sha256: String::new(), ..full.clone() },
        ModSource { seen: String::new(), ..full.clone() },
        ModSource { seen: "neoforge/v1".into(), ..full.clone() },
        ModSource { kind: Kind::Maven, ..full.clone() },
    ] {
        assert!(crate::sources::github_direct(&src, "r.jar").is_none());
    }
    assert!(crate::sources::github_direct(&full, "").is_none());
}
