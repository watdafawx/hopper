//! Requests to sources, with a disk cache that asks "changed since?"
//! (ETag / If-None-Match). GitHub lets a computer without a token make 60
//! API calls an hour; an unchanged answer (304) costs nothing, so most
//! update checks are free. When GitHub says the limit is used up (403 or
//! 429), checks pause until it resets and the cached answer stands in.

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
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
/// GitHub API requests that went out (not the ones the cache answered).
static API_CALLS: AtomicU64 = AtomicU64::new(0);
/// Tests point GitHub at a local fake server instead.
static TEST_BASE: Mutex<Option<String>> = Mutex::new(None);

const API: &str = "https://api.github.com";
const WEB: &str = "https://github.com";

/// How long a releases list is used as it is before GitHub is asked again
/// (and even then an unchanged list costs nothing).
pub const LIST_FRESH: u64 = 10 * 60;
/// One release by its tag rarely changes once out.
pub const TAG_FRESH: u64 = 24 * 3600;

/// GitHub said its limit for this PC is used up: no GitHub calls before
/// `until` (unix seconds).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateLimited {
    pub until: u64,
}

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GitHub is limiting downloads for now. Hopper tries again at {}.", hhmm(self.until))
    }
}

impl std::error::Error for RateLimited {}

impl RateLimited {
    /// When it ends, "14:32" (this PC's time).
    pub fn at(&self) -> String {
        hhmm(self.until)
    }

    /// How long until it ends (a second at least).
    pub fn wait(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.until.saturating_sub(now()).max(1))
    }
}

/// The rate limit behind `e`, if that's why it failed.
pub fn rate_limited(e: &anyhow::Error) -> Option<RateLimited> {
    e.chain().find_map(|c| c.downcast_ref::<RateLimited>()).copied()
}

/// "14:32", this PC's time.
fn hhmm(unix: u64) -> String {
    let c = crate::time::clock(unix as i64);
    c.get(..5).unwrap_or(&c).to_string()
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn cache_file() -> Option<std::path::PathBuf> {
    // Tests never read or write the player's cache.
    if cfg!(test) {
        return None;
    }
    crate::paths::dirs().map(|d| d.cache_dir().join("source-cache.json"))
}

/// The local fake GitHub tests run against: `HOPPER_TEST_GITHUB`, and only
/// on this PC (a loopback address), so nobody can be pointed elsewhere.
fn test_base() -> Option<String> {
    if let Some(b) = TEST_BASE.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return Some(b);
    }
    std::env::var("HOPPER_TEST_GITHUB")
        .ok()
        .map(|b| b.trim_end_matches('/').to_string())
        .filter(|b| b.starts_with("http://127.0.0.1:"))
}

/// Points GitHub at `base` (a fake server; `None` for the real one).
#[doc(hidden)]
pub fn set_test_base(base: Option<&str>) {
    *TEST_BASE.lock().unwrap_or_else(|e| e.into_inner()) = base.map(str::to_string);
}

/// Where a request for `url` really goes: the fake server under a test.
fn routed(url: &str) -> String {
    match test_base() {
        Some(b) if url.starts_with(API) => format!("{b}/api{}", &url[API.len()..]),
        Some(b) if url.starts_with(WEB) => format!("{b}/web{}", &url[WEB.len()..]),
        _ => url.to_string(),
    }
}

/// GitHub API requests made this run (cache answers don't count).
pub fn github_calls() -> u64 {
    API_CALLS.load(Ordering::Relaxed)
}

/// The website link of a release's file. Downloads from it don't count
/// against the API limit.
pub fn release_download_url(repo: &str, tag: &str, file: &str) -> String {
    routed(&format!("{WEB}/{repo}/releases/download/{}/{}", super::enc(tag), super::enc(file)))
}

/// When GitHub's answer says the limit is used up: the time calls may
/// start again (unix seconds). `remaining`, `reset` and `retry_after` are
/// the `x-ratelimit-remaining`, `x-ratelimit-reset` and `retry-after`
/// headers. A 403 for anything else (a private repo) is not a limit.
pub fn limit_reset(status: u16, remaining: Option<&str>, reset: Option<&str>, retry_after: Option<&str>, now: u64) -> Option<u64> {
    let used_up = remaining.map(str::trim) == Some("0");
    let reset = reset.and_then(|r| r.trim().parse::<u64>().ok()).filter(|r| *r > now);
    let after = retry_after.and_then(|r| r.trim().parse::<u64>().ok()).map(|s| now + s.max(1));
    if !(status == 429 || (status == 403 && (used_up || after.is_some()))) {
        return None;
    }
    let at = if used_up { reset.or(after) } else { after.or(reset) };
    // A missing or odd header: wait a while, never longer than GitHub's
    // own hour-long window.
    Some(at.unwrap_or(now + if used_up { 15 * 60 } else { 60 }).min(now + 3600))
}

#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Forgets the cache, the pause and the count (tests only).
#[cfg(test)]
pub(crate) fn reset_for_test() {
    with_cache(|m| m.clear());
    *PAUSED_UNTIL.lock().unwrap_or_else(|e| e.into_inner()) = 0;
    API_CALLS.store(0, Ordering::Relaxed);
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
    get_text_fresh(url, github, 0)
}

/// [`get_text`], taking a cached answer younger than `fresh` seconds as
/// it is, without asking.
pub fn get_text_fresh(url: &str, github: bool, fresh: u64) -> anyhow::Result<String> {
    let cached = with_cache(|m| m.get(url).cloned());
    if let Some(c) = cached.as_ref().filter(|c| fresh > 0 && c.at + fresh > now()) {
        return Ok(c.body.clone());
    }
    if github && let Some(until) = paused_until() {
        if let Some(c) = cached {
            return Ok(c.body);
        }
        return Err(RateLimited { until }.into());
    }
    let target = routed(url);
    let mut req = crate::cf::agent()
        .get(&target)
        .config()
        .http_status_as_error(false)
        .build();
    if github {
        API_CALLS.fetch_add(1, Ordering::Relaxed);
        req = req.header("Accept", "application/vnd.github+json").header("X-GitHub-Api-Version", "2022-11-28");
        // The token goes to GitHub's API and nowhere else.
        if target.starts_with(API)
            && let Some(t) = github_token()
        {
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
    if github
        && let Some(until) = limit_reset(
            status,
            header("x-ratelimit-remaining").as_deref(),
            header("x-ratelimit-reset").as_deref(),
            header("retry-after").as_deref(),
            now(),
        )
    {
        log::warn!("GitHub's rate limit is used up; no GitHub calls until {}", hhmm(until));
        {
            let mut p = PAUSED_UNTIL.lock().unwrap_or_else(|e| e.into_inner());
            *p = (*p).max(until);
        }
        if let Some(c) = cached {
            return Ok(c.body);
        }
        return Err(RateLimited { until }.into());
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

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
