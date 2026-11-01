//! Requests to sources, with a disk cache that asks "changed since?"
//! (ETag / If-None-Match). GitHub lets a computer without a token make 60
//! API calls an hour; an unchanged answer (304) costs nothing, so most
//! update checks are free. When GitHub says the limit is used up (403 or
//! 429), checks pause until it resets and the cached answer stands in.

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Entry {
    etag: String,
    body: String,
    /// When it was fetched (unix seconds).
    at: u64,
}

static CACHE: Mutex<Option<HashMap<String, Entry>>> = Mutex::new(None);
/// No GitHub calls before this time (unix seconds), after a rate limit.
static PAUSED_UNTIL: Mutex<u64> = Mutex::new(0);
/// The GitHub token, read once from the credential store.
static TOKEN: Mutex<Option<Option<String>>> = Mutex::new(None);

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn cache_file() -> Option<std::path::PathBuf> {
    crate::paths::dirs().map(|d| d.cache_dir().join("source-cache.json"))
}

fn with_cache<T>(f: impl FnOnce(&mut HashMap<String, Entry>) -> T) -> T {
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.get_or_insert_with(|| {
        cache_file()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    });
    f(map)
}

fn save_cache() {
    let Some(p) = cache_file() else { return };
    let body = with_cache(|m| {
        // Keep the cache small: answers older than a month go.
        let cutoff = now().saturating_sub(30 * 86400);
        m.retain(|_, e| e.at >= cutoff);
        serde_json::to_vec(m).ok()
    });
    if let Some(b) = body {
        let _ = std::fs::create_dir_all(p.parent().unwrap_or(&p));
        let _ = std::fs::write(p, b);
    }
}

/// The GitHub token the player saved in Settings, if any.
pub fn github_token() -> Option<String> {
    let mut g = TOKEN.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(|| {
        std::env::var("HOPPER_GITHUB_TOKEN")
            .ok()
            .or_else(|| crate::secrets::get(super::GITHUB_TOKEN))
            .filter(|t| !t.trim().is_empty())
    })
    .clone()
}

/// Saves (or with "" removes) the GitHub token.
pub fn set_github_token(token: &str) -> anyhow::Result<()> {
    crate::secrets::set(super::GITHUB_TOKEN, token)?;
    *TOKEN.lock().unwrap_or_else(|e| e.into_inner()) = Some(Some(token.trim().to_string()).filter(|t| !t.is_empty()));
    Ok(())
}

/// Uses `token` for this run without saving it (Hopper Server keeps its
/// own in its config).
pub fn set_session_github_token(token: &str) {
    *TOKEN.lock().unwrap_or_else(|e| e.into_inner()) = Some(Some(token.trim().to_string()).filter(|t| !t.is_empty()));
}

/// Whether a GitHub token is saved (never the token itself).
pub fn has_github_token() -> bool {
    github_token().is_some()
}

/// When GitHub checks may run again, if they're paused (unix seconds).
pub fn paused_until() -> Option<u64> {
    let t = *PAUSED_UNTIL.lock().unwrap_or_else(|e| e.into_inner());
    (t > now()).then_some(t)
}

/// GETs `url` as text. `github`: an API call (token, rate limit, cache).
/// Other hosts are cached too, with their ETag when they send one.
pub fn get_text(url: &str, github: bool) -> anyhow::Result<String> {
    let cached = with_cache(|m| m.get(url).cloned());
    if github && let Some(until) = paused_until() {
        if let Some(c) = cached {
            return Ok(c.body);
        }
        bail!("GitHub's limit for checks is used up; Hopper tries again {}", clock(until));
    }
    let mut req = crate::cf::agent()
        .get(url)
        .config()
        .http_status_as_error(false)
        .build();
    if github {
        req = req.header("Accept", "application/vnd.github+json").header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(t) = github_token() {
            req = req.header("Authorization", &format!("Bearer {t}"));
        }
    }
    if let Some(c) = cached.as_ref().filter(|c| !c.etag.is_empty()) {
        req = req.header("If-None-Match", &c.etag);
    }
    let resp = match req.call() {
        Ok(r) => r,
        // Offline: the last answer is better than none.
        Err(e) => match cached {
            Some(c) => {
                log::warn!("{url}: {e}; using the cached answer");
                return Ok(c.body);
            }
            None => return Err(anyhow::Error::new(e).context(format!("GET {url}"))),
        },
    };
    let status = resp.status().as_u16();
    let header = |n: &str| resp.headers().get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
    if status == 304 {
        let c = cached.context("304 without a cached answer")?;
        with_cache(|m| {
            if let Some(e) = m.get_mut(url) {
                e.at = now();
            }
        });
        return Ok(c.body);
    }
    if github && (status == 429 || (status == 403 && header("x-ratelimit-remaining").as_deref() == Some("0"))) {
        let reset = header("x-ratelimit-reset")
            .and_then(|r| r.parse::<u64>().ok())
            .or_else(|| header("retry-after").and_then(|r| r.parse::<u64>().ok()).map(|s| now() + s))
            .unwrap_or(now() + 15 * 60);
        *PAUSED_UNTIL.lock().unwrap_or_else(|e| e.into_inner()) = reset;
        if let Some(c) = cached {
            return Ok(c.body);
        }
        bail!(
            "GitHub's limit for checks is used up; Hopper tries again {}. A GitHub token in Settings raises the limit.",
            clock(reset)
        );
    }
    if status == 404 {
        bail!("Not found (404). Check the link; a private repo can't be read.");
    }
    if !(200..300).contains(&status) {
        bail!("{} answered {status}", super::host_of(url));
    }
    let etag = header("etag").unwrap_or_default();
    let mut resp = resp;
    let body = resp
        .body_mut()
        .with_config()
        .limit(16 << 20)
        .read_to_string()
        .with_context(|| format!("reading {url}"))?;
    with_cache(|m| m.insert(url.to_string(), Entry { etag, body: body.clone(), at: now() }));
    save_cache();
    Ok(body)
}

/// What a HEAD of a download link says: (ETag or Last-Modified, size,
/// file name from Content-Disposition).
pub fn head(url: &str) -> anyhow::Result<(String, u64, Option<String>)> {
    let resp = crate::cf::agent()
        .head(url)
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .with_context(|| format!("HEAD {url}"))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        bail!("{} answered {status}", super::host_of(url));
    }
    let header = |n: &str| resp.headers().get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
    let tag = header("etag").or_else(|| header("last-modified")).unwrap_or_default();
    let size = header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let name = header("content-disposition").and_then(|d| filename_in(&d));
    Ok((tag, size, name))
}

/// The file name in a Content-Disposition header.
pub fn filename_in(d: &str) -> Option<String> {
    let i = d.find("filename=")?;
    let v = d[i + 9..].split(';').next()?.trim().trim_matches('"');
    let v = v.rsplit(['/', '\\']).next()?;
    (!v.is_empty()).then(|| v.to_string())
}

fn clock(unix: u64) -> String {
    let mins = unix.saturating_sub(now()).div_ceil(60).max(1);
    if mins == 1 { "in a minute".into() } else { format!("in about {mins} minutes") }
}
