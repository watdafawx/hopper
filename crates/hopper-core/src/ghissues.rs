//! A mod's GitHub issues, for the mod peek's Issues tab ("is this a known
//! bug?").
//!
//! Only projects whose issue tracker is a GitHub repo get the tab: the
//! tracker link CurseForge or Modrinth gives, or a GitHub-sourced mod's own
//! repo. The list is GitHub's REST `issues` call (pull requests dropped);
//! typing searches with GitHub's issue search. Without a token GitHub allows
//! 60 calls an hour and 10 searches a minute, so every answer is kept for
//! the session and a refusal reads [`BUSY`]; the saved GitHub token (Settings)
//! is sent when there is one, never logged.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Context;
use serde::Deserialize;

/// Issues asked for at a time.
pub const PAGE_SIZE: u32 = 20;

/// Shown when the issues can't be had (not found, issues off, offline).
pub const LOAD_FAILED: &str = "Issues couldn't be loaded.";

/// Shown when GitHub refuses for now (its hourly or per-minute limit).
pub const BUSY: &str = "GitHub is busy, try again in a minute.";

/// GitHub's search goes no further than its first thousand results.
const SEARCH_CAP: u32 = 1000;

/// Which issues: GitHub's own Open / Closed / All.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Filter {
    #[default]
    Open,
    Closed,
    All,
}

impl Filter {
    pub const ALL: [Filter; 3] = [Filter::Open, Filter::Closed, Filter::All];

    /// GitHub's word for it (`state=`), also the server route's.
    pub fn as_str(self) -> &'static str {
        match self {
            Filter::Open => "open",
            Filter::Closed => "closed",
            Filter::All => "all",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Filter::Open => "Open",
            Filter::Closed => "Closed",
            Filter::All => "All",
        }
    }

    pub fn parse(s: &str) -> Option<Filter> {
        Filter::ALL.into_iter().find(|f| f.as_str() == s)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    pub name: String,
    /// The label's colour as GitHub keeps it: six hex digits, no `#`
    /// (empty when it isn't that).
    pub color: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    /// Its page on github.com.
    pub url: String,
    pub open: bool,
    pub labels: Vec<Label>,
    pub comments: u32,
    /// Opened / closed, seconds since the epoch (closed 0 while open).
    pub created: i64,
    pub closed: i64,
    pub author: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Page {
    pub issues: Vec<Issue>,
    /// Whether a later page has more.
    pub more: bool,
    /// How many match, for a search (GitHub's count); the list has none.
    pub total: Option<u32>,
}

/// Why a page couldn't be had: GitHub's limit ([`BUSY`]) or anything else
/// ([`LOAD_FAILED`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Busy,
    Failed,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Error::Busy => BUSY,
            Error::Failed => LOAD_FAILED,
        })
    }
}

impl std::error::Error for Error {}

/// A GitHub owner or repo name: letters, digits, `-`, `_` and `.`, never
/// `.` or `..` alone. Hopper Server takes nothing else into its URLs.
fn valid_part(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s != "."
        && s != ".."
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Whether `repo` is a plain "owner/repo".
pub fn valid_repo(repo: &str) -> bool {
    repo.split_once('/').is_some_and(|(o, r)| valid_part(o) && valid_part(r))
}

/// "owner/repo" for a github.com link to a repo or anything in it
/// (`/issues`, `/issues/12`, `.git`, a trailing slash); None for any other
/// host or a link that isn't a repo.
pub fn repo_of(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let rest = rest.split(['?', '#']).next()?;
    let (host, path) = rest.split_once('/')?;
    let host = host.to_ascii_lowercase();
    if host != "github.com" && host != "www.github.com" {
        return None;
    }
    let mut parts = path.split('/').filter(|p| !p.is_empty());
    let owner = parts.next()?;
    let repo = parts.next()?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    // github.com/orgs/x, /settings, /sponsors/x … aren't repos.
    const NOT_OWNERS: [&str; 9] =
        ["orgs", "users", "settings", "sponsors", "marketplace", "topics", "apps", "features", "login"];
    if NOT_OWNERS.contains(&owner.to_ascii_lowercase().as_str()) {
        return None;
    }
    let r = format!("{owner}/{repo}");
    valid_repo(&r).then_some(r)
}

/// The repo whose issues a project shows: its issue tracker link when that
/// is on GitHub, else (a mod from GitHub) its own repo page.
pub fn repo_for(issues_url: Option<&str>, github_page: Option<&str>) -> Option<String> {
    match issues_url.map(str::trim).filter(|u| !u.is_empty()) {
        // A tracker elsewhere (GitLab, a Discord) means no tab, even for a
        // mod whose code is on GitHub: the authors read the other one.
        Some(u) => repo_of(u),
        None => github_page.and_then(repo_of),
    }
}

/// [`repo_for`] a project: CurseForge's or Modrinth's tracker link, or a
/// mod from GitHub's own repo.
pub fn repo_of_mod(m: &crate::cf::Mod) -> Option<String> {
    let github = crate::sources::is_src(m.id)
        .then(|| crate::sources::page::origin(m.id))
        .flatten()
        .filter(|o| o.kind == crate::sources::Kind::GitHub)
        .map(|o| o.page);
    repo_for(m.links.issues_url.as_deref(), github.as_deref())
}

/// The repo's issues on github.com ("Open issues on GitHub"), with the
/// search and filter the tab shows.
pub fn site_url(repo: &str, filter: Filter, query: &str) -> String {
    let terms = terms(query);
    if terms.is_empty() && filter == Filter::Open {
        return format!("https://github.com/{repo}/issues");
    }
    let mut q = String::from("is:issue");
    if filter != Filter::All {
        q.push_str(&format!(" is:{}", filter.as_str()));
    }
    if !terms.is_empty() {
        q.push(' ');
        q.push_str(&terms);
    }
    format!("https://github.com/{repo}/issues?q={}", encode(&q))
}

/// The player's search words, tidied: one space between words, and no
/// qualifier that would widen it past this repo (`repo:`, `org:`, `user:`).
fn terms(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|w| {
            let w = w.to_ascii_lowercase();
            !["repo:", "org:", "user:", "is:pr", "is:pull-request", "type:pr"].iter().any(|q| w.starts_with(q))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[derive(Deserialize)]
struct RawIssue {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    labels: Vec<RawLabel>,
    #[serde(default)]
    comments: u32,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    closed_at: Option<String>,
    #[serde(default)]
    user: Option<RawUser>,
    /// Present on pull requests, which the issues list mixes in.
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawLabel {
    #[serde(default)]
    name: String,
    #[serde(default)]
    color: String,
}

#[derive(Deserialize)]
struct RawUser {
    #[serde(default)]
    login: String,
}

#[derive(Deserialize)]
struct RawSearch {
    total_count: u32,
    items: Vec<RawIssue>,
}

fn issue(r: RawIssue) -> Option<Issue> {
    if r.pull_request.is_some() {
        return None;
    }
    // Only github.com links are opened from a row.
    let url = Some(r.html_url).filter(|u| u.starts_with("https://github.com/"))?;
    let secs = |s: Option<String>| s.as_deref().and_then(crate::time::parse_iso).unwrap_or(0);
    Some(Issue {
        number: r.number,
        title: r.title.trim().to_string(),
        url,
        open: r.state != "closed",
        labels: r
            .labels
            .into_iter()
            .filter(|l| !l.name.trim().is_empty())
            .map(|l| Label {
                name: l.name.trim().to_string(),
                color: if l.color.len() == 6 && l.color.bytes().all(|b| b.is_ascii_hexdigit()) {
                    l.color.to_ascii_lowercase()
                } else {
                    String::new()
                },
            })
            .collect(),
        comments: r.comments,
        created: secs(r.created_at),
        closed: secs(r.closed_at),
        author: r.user.map(|u| u.login).unwrap_or_default(),
    })
}

/// One page of `GET /repos/{repo}/issues`. A full page means there may be
/// more, counted before pull requests are dropped.
pub fn parse_list(body: &str) -> anyhow::Result<Page> {
    let raw: Vec<RawIssue> = serde_json::from_str(body).context("not an issue list")?;
    let more = raw.len() as u32 >= PAGE_SIZE;
    Ok(Page { issues: raw.into_iter().filter_map(issue).collect(), more, total: None })
}

/// Page `page` (from 1) of `GET /search/issues`.
pub fn parse_search(body: &str, page: u32) -> anyhow::Result<Page> {
    let raw: RawSearch = serde_json::from_str(body).context("not a search result")?;
    let total = raw.total_count;
    let more = !raw.items.is_empty() && page.saturating_mul(PAGE_SIZE) < total.min(SEARCH_CAP);
    Ok(Page { issues: raw.items.into_iter().filter_map(issue).collect(), more, total: Some(total) })
}

/// Whether an answer is GitHub's limit rather than a real failure: 429, or
/// a 403 that says so (no calls left, or its message for the per-minute
/// "secondary" limit).
pub fn rate_limited(status: u16, remaining: Option<&str>, body: &str) -> bool {
    status == 429 || (status == 403 && (remaining == Some("0") || body.to_ascii_lowercase().contains("rate limit")))
}

/// What one page is: which repo, filter, search words and page (from 1).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    repo: String,
    filter: Filter,
    terms: String,
    page: u32,
}

fn key(repo: &str, filter: Filter, query: &str, page: u32) -> Key {
    Key { repo: repo.to_ascii_lowercase(), filter, terms: terms(query).to_lowercase(), page: page.max(1) }
}

fn fetch(k: &Key) -> Result<Page, Error> {
    // For screenshots of every state without asking GitHub: a folder with
    // `list.json` / `search.json` standing in for its answers, and a `busy`,
    // `fail` or `slow` file to act out a refusal, a failure or a wait.
    if let Ok(dir) = std::env::var("HOPPER_TEST_ISSUES") {
        return fixture(std::path::Path::new(&dir), k);
    }
    let repo = &k.repo;
    if !valid_repo(repo) {
        return Err(Error::Failed);
    }
    let (url, query) = if k.terms.is_empty() {
        (format!("https://api.github.com/repos/{repo}/issues"), vec![("state", k.filter.as_str().to_string())])
    } else {
        let mut q = format!("repo:{repo} is:issue");
        if k.filter != Filter::All {
            q.push_str(&format!(" is:{}", k.filter.as_str()));
        }
        q.push(' ');
        q.push_str(&k.terms);
        ("https://api.github.com/search/issues".to_string(), vec![("q", q)])
    };
    let mut req = crate::cf::agent()
        .get(&url)
        .config()
        .http_status_as_error(false)
        .build()
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .query("per_page", PAGE_SIZE.to_string())
        .query("page", k.page.to_string());
    for (n, v) in query {
        req = req.query(n, v);
    }
    if let Some(t) = crate::sources::http::github_token() {
        req = req.header("Authorization", &format!("Bearer {t}"));
    }
    let mut resp = req.call().map_err(|e| {
        log::info!("issues of {repo}: {e}");
        Error::Failed
    })?;
    let status = resp.status().as_u16();
    let remaining = resp.headers().get("x-ratelimit-remaining").and_then(|v| v.to_str().ok()).map(str::to_string);
    let body = resp.body_mut().with_config().limit(8 * 1024 * 1024).read_to_string().unwrap_or_default();
    if rate_limited(status, remaining.as_deref(), &body) {
        log::info!("issues of {repo}: GitHub's limit ({status})");
        return Err(Error::Busy);
    }
    if !(200..300).contains(&status) {
        log::info!("issues of {repo}: GitHub answered {status}");
        return Err(Error::Failed);
    }
    let page = if k.terms.is_empty() { parse_list(&body) } else { parse_search(&body, k.page) };
    page.map_err(|e| {
        log::info!("issues of {repo}: {e:#}");
        Error::Failed
    })
}

fn fixture(dir: &std::path::Path, k: &Key) -> Result<Page, Error> {
    if dir.join("slow").exists() {
        std::thread::sleep(std::time::Duration::from_secs(4));
    }
    if dir.join("busy").exists() {
        return Err(Error::Busy);
    }
    if dir.join("fail").exists() {
        return Err(Error::Failed);
    }
    let search = !k.terms.is_empty();
    let body = std::fs::read_to_string(dir.join(if search { "search.json" } else { "list.json" })).map_err(|_| Error::Failed)?;
    let mut p = if search { parse_search(&body, k.page) } else { parse_list(&body) }.map_err(|_| Error::Failed)?;
    // The same file for every page and filter: filtered here, two pages.
    if k.filter != Filter::All {
        p.issues.retain(|i| i.open == (k.filter == Filter::Open));
        p.total = p.total.map(|_| p.issues.len() as u32);
    }
    p.more = k.page < 2 && !p.issues.is_empty();
    Ok(p)
}

type Cache = Mutex<HashMap<Key, Arc<Page>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Page `page` (from 1) of `repo`'s issues for `filter`, searched for
/// `query` when it has words; kept for the session once it loaded
/// (failures aren't kept, so asking again tries again). Blocks: call it off
/// the UI thread.
pub fn page(repo: &str, filter: Filter, query: &str, page: u32) -> Result<Arc<Page>, Error> {
    let k = key(repo, filter, query, page);
    if let Some(p) = cache().lock().unwrap().get(&k) {
        return Ok(p.clone());
    }
    let p = Arc::new(fetch(&k)?);
    cache().lock().unwrap().insert(k, p.clone());
    Ok(p)
}

/// A page already loaded this session, without asking.
pub fn cached(repo: &str, filter: Filter, query: &str, page: u32) -> Option<Arc<Page>> {
    cache().lock().unwrap().get(&key(repo, filter, query, page)).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = include_str!("ghissues_fixtures/sodium-list.json");
    const SEARCH: &str = include_str!("ghissues_fixtures/sodium-search.json");
    const RATE: &str = include_str!("ghissues_fixtures/rate-limit.json");

    #[test]
    fn list_drops_pull_requests() {
        let p = parse_list(LIST).unwrap();
        assert_eq!(p.issues.len(), 8);
        assert!(!p.issues.iter().any(|i| i.number == 3992), "the pull request is gone");
        // Nine raw items is under a full page: nothing more.
        assert!(!p.more);
        assert_eq!(p.total, None);
    }

    #[test]
    fn list_reads_each_issue() {
        let p = parse_list(LIST).unwrap();
        let i = &p.issues[0];
        assert_eq!(i.number, 3991);
        assert!(i.open);
        assert_eq!(i.url, "https://github.com/CaffeineMC/sodium/issues/3991");
        assert_eq!(i.author, "Hippopotamoss");
        assert_eq!(i.labels, [Label { name: "S-needs-triage".into(), color: "272702".into() }]);
        assert_eq!(i.created, crate::time::parse_iso("2026-11-12T21:37:22Z").unwrap());
        assert_eq!(i.closed, 0);
        let c = p.issues.iter().find(|i| i.number == 3990).unwrap();
        assert!(!c.open);
        assert!(c.closed > 0);
    }

    #[test]
    fn full_page_means_more() {
        let one = r#"{"number":1,"title":"t","html_url":"https://github.com/o/r/issues/1","state":"open"}"#;
        let pr = r#"{"number":2,"title":"p","html_url":"https://github.com/o/r/pull/2","state":"open","pull_request":{}}"#;
        let items: Vec<&str> = (0..20).map(|i| if i % 2 == 0 { one } else { pr }).collect();
        let p = parse_list(&format!("[{}]", items.join(","))).unwrap();
        assert_eq!(p.issues.len(), 10);
        assert!(p.more, "pull requests count towards a full page");
    }

    #[test]
    fn search_reads_total_and_more() {
        let p = parse_search(SEARCH, 1).unwrap();
        assert_eq!(p.total, Some(605));
        assert_eq!(p.issues.len(), 4);
        assert!(p.more);
        assert_eq!(p.issues[0].number, 3940);
        // The thousandth result is as far as GitHub goes.
        assert!(!parse_search(SEARCH, 50).unwrap().more);
        assert!(parse_search(SEARCH, 30).unwrap().more);
        assert!(!parse_search(r#"{"total_count":0,"incomplete_results":false,"items":[]}"#, 1).unwrap().more);
    }

    #[test]
    fn odd_fields_are_tidied() {
        let body = r#"[{"number":5,"title":"  x  ","html_url":"https://evil.example/5","state":"open"},
            {"number":6,"title":"y","html_url":"https://github.com/o/r/issues/6","state":"closed",
             "labels":[{"name":"bug","color":"zz"},{"name":" ","color":"ffffff"}],"user":null}]"#;
        let p = parse_list(body).unwrap();
        // A row only ever opens github.com.
        assert_eq!(p.issues.len(), 1);
        let i = &p.issues[0];
        assert_eq!(i.labels, [Label { name: "bug".into(), color: String::new() }]);
        assert_eq!(i.author, "");
    }

    #[test]
    fn rate_limit_is_busy_not_failed() {
        assert!(parse_list(RATE).is_err());
        assert!(rate_limited(403, Some("0"), RATE));
        assert!(rate_limited(403, None, RATE), "the message alone says so");
        assert!(rate_limited(429, None, ""));
        assert!(!rate_limited(403, Some("12"), r#"{"message":"Resource not accessible"}"#));
        assert!(!rate_limited(404, Some("0"), r#"{"message":"Not Found"}"#));
        assert_eq!(Error::Busy.to_string(), BUSY);
        assert_eq!(Error::Failed.to_string(), LOAD_FAILED);
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(parse_list("<html>").is_err());
        assert!(parse_list("").is_err());
        assert!(parse_search(r#"{"message":"Validation Failed"}"#, 1).is_err());
    }

    #[test]
    fn repo_from_link_variants() {
        let r = |u| repo_of(u);
        assert_eq!(r("https://github.com/CaffeineMC/sodium/issues").as_deref(), Some("CaffeineMC/sodium"));
        assert_eq!(r("https://github.com/CaffeineMC/sodium/issues/12").as_deref(), Some("CaffeineMC/sodium"));
        assert_eq!(r("https://github.com/o/r.git").as_deref(), Some("o/r"));
        assert_eq!(r("https://github.com/o/r/").as_deref(), Some("o/r"));
        assert_eq!(r("http://www.github.com/o/my.mod-1_x?tab=readme#top").as_deref(), Some("o/my.mod-1_x"));
        assert_eq!(r("  https://GitHub.com/o/r  ").as_deref(), Some("o/r"));
        assert_eq!(r("https://gitlab.com/o/r/-/issues"), None);
        assert_eq!(r("https://discord.gg/abc"), None);
        assert_eq!(r("https://github.com/o"), None);
        assert_eq!(r("https://github.com/orgs/o/projects"), None);
        assert_eq!(r("https://github.com.evil.example/o/r"), None);
        assert_eq!(r("https://evil.example/github.com/o/r"), None);
        assert_eq!(r("https://github.com/o/r%2F..%2Fx"), None);
        assert_eq!(r("https://github.com/../r"), None);
        assert_eq!(r("github.com/o/r"), None);
    }

    #[test]
    fn repo_for_prefers_the_tracker() {
        assert_eq!(repo_for(Some("https://github.com/a/b/issues"), Some("https://github.com/c/d")).as_deref(), Some("a/b"));
        assert_eq!(repo_for(Some("https://gitlab.com/a/b/-/issues"), Some("https://github.com/c/d")), None);
        assert_eq!(repo_for(None, Some("https://github.com/c/d")).as_deref(), Some("c/d"));
        assert_eq!(repo_for(Some("  "), Some("https://github.com/c/d")).as_deref(), Some("c/d"));
        assert_eq!(repo_for(None, None), None);
    }

    #[test]
    fn valid_repo_is_strict() {
        assert!(valid_repo("CaffeineMC/sodium"));
        assert!(valid_repo("a-b_c.d/e.f"));
        for bad in ["", "a", "a/", "/b", "a/b/c", "a b/c", "a/b?x", "../b", "a/..", "a/%2e", "ä/b"] {
            assert!(!valid_repo(bad), "{bad}");
        }
    }

    #[test]
    fn search_words_stay_in_the_repo() {
        assert_eq!(terms("  crash   on load "), "crash on load");
        assert_eq!(terms("crash repo:other/x org:y USER:z is:pr"), "crash");
        assert_eq!(key("O/R", Filter::Open, "Crash", 0), key("o/r", Filter::Open, " crash ", 1));
    }

    #[test]
    fn site_url_carries_the_search() {
        assert_eq!(site_url("o/r", Filter::Open, ""), "https://github.com/o/r/issues");
        assert_eq!(site_url("o/r", Filter::All, ""), "https://github.com/o/r/issues?q=is:issue");
        assert_eq!(
            site_url("o/r", Filter::Closed, "crash & burn"),
            "https://github.com/o/r/issues?q=is:issue+is:closed+crash+%26+burn"
        );
    }
}
