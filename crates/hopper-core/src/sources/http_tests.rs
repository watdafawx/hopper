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
