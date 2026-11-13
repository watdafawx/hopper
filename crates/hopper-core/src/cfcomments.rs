//! A CurseForge project's comments, as its website's Comments tab shows
//! them.
//!
//! CurseForge's API has no comments, so these come from the website's own
//! endpoint (`www.curseforge.com/api/v1/mods/{id}/comments`), which needs
//! no key. It isn't documented and may change or start refusing us, so
//! every failure is one plain error the panel shows next to a link to the
//! page; nothing else depends on it. The API key is never sent there.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Context;
use serde::Deserialize;

/// Comments asked for at a time. The site counts replies in a page too.
pub const PAGE_SIZE: u32 = 20;

/// Shown when the comments can't be had, whatever the reason.
pub const LOAD_FAILED: &str = "Comments couldn't be loaded.";

#[derive(Debug, Clone, PartialEq)]
pub struct Comment {
    pub id: u64,
    /// The name CurseForge shows.
    pub author: String,
    pub avatar: Option<String>,
    /// The poster's CurseForge id and login name, for [`Comment::is_by`].
    pub author_id: u64,
    pub login: String,
    /// When it was posted, seconds since the epoch.
    pub posted: i64,
    /// Plain text: the site's own text form of the comment, never its HTML.
    pub text: String,
    pub pinned: bool,
    /// Taken down (by its author or a moderator), kept for its replies.
    pub removed: bool,
    /// Every reply in the thread, one level deep: replies to replies are
    /// listed in order after the one they answer (each starts with the
    /// @name it answers), so a narrow panel never indents past one step.
    pub replies: Vec<Comment>,
}

impl Comment {
    /// Whether one of the project's authors wrote it: by CurseForge id when
    /// both sides have one, else by name (either the shown or the login
    /// name, ignoring case). A removed comment names no one.
    pub fn is_by(&self, authors: &[crate::cf::Author]) -> bool {
        if self.removed {
            return false;
        }
        authors.iter().any(|a| {
            if a.id != 0 && a.id == self.author_id {
                return true;
            }
            let name = a.name.trim();
            !name.is_empty() && (name.eq_ignore_ascii_case(&self.author) || name.eq_ignore_ascii_case(&self.login))
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Page {
    /// Pinned ones first, then newest first as the site gives them.
    pub comments: Vec<Comment>,
    /// The page these are (from 0).
    pub index: u32,
    /// How many there are in all, replies included.
    pub total: u32,
}

impl Page {
    /// Whether a later page has more.
    pub fn more(&self) -> bool {
        !self.comments.is_empty() && (self.index as u64 + 1) * (PAGE_SIZE as u64) < (self.total as u64)
    }
}

/// The page's address on the website, for "Open comments on CurseForge":
/// the project's own page plus `/comments`.
pub fn site_url(project_page: &str) -> String {
    format!("{}/comments", project_page.trim_end_matches('/'))
}

#[derive(Deserialize)]
struct Raw {
    data: Vec<RawComment>,
    #[serde(default)]
    pagination: Option<RawPagination>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPagination {
    #[serde(default)]
    index: u32,
    #[serde(default)]
    total_count: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawComment {
    id: u64,
    #[serde(default)]
    text: String,
    #[serde(default)]
    author: Option<RawAuthor>,
    #[serde(default)]
    date_posted: i64,
    #[serde(default)]
    status: u32,
    #[serde(default)]
    is_pinned: bool,
    #[serde(default)]
    replies: Vec<RawComment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAuthor {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    username: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    twitch_avatar_url: Option<String>,
}

/// Status 1 is a live comment; 2 is one taken down (its text is empty).
const STATUS_LIVE: u32 = 1;

fn comment(r: RawComment) -> Comment {
    let (author, avatar, author_id, login) = match r.author {
        Some(a) => {
            let login = a.username.trim().to_string();
            let name = if a.display_name.trim().is_empty() { a.username } else { a.display_name };
            (name.trim().to_string(), a.twitch_avatar_url.filter(|u| u.starts_with("https://")), a.id, login)
        }
        None => (String::new(), None, 0, String::new()),
    };
    let text = tidy(&r.text);
    let removed = r.status != STATUS_LIVE || text.is_empty();
    // A removed comment shows no one's name or picture (the site's neither).
    let (author, avatar, author_id, login) =
        if removed { (String::new(), None, 0, String::new()) } else { (author, avatar, author_id, login) };
    Comment {
        id: r.id,
        author,
        avatar,
        author_id,
        login,
        posted: r.date_posted / 1000,
        removed,
        text,
        pinned: r.is_pinned,
        replies: {
            let mut all = Vec::new();
            flatten(r.replies, &mut all);
            all
        },
    }
}

/// Puts a reply tree in one list, each reply followed by its own replies.
fn flatten(raw: Vec<RawComment>, out: &mut Vec<Comment>) {
    for r in raw {
        let mut c = comment(r);
        let deeper = std::mem::take(&mut c.replies);
        if !c.removed {
            out.push(c);
        }
        out.extend(deeper);
    }
}

/// The site's text has Windows line ends and no-break spaces for blank
/// lines: plain lines, at most one blank line in a row.
fn tidy(s: &str) -> String {
    let mut out = String::new();
    let mut blank = 0;
    for line in s.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
        let line = line.replace('\u{a0}', " ");
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank += 1;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        out.push_str(line);
    }
    out
}

/// Reads one page of the site's answer. Anything that isn't its JSON (a
/// Cloudflare check page, an error) is an error.
pub fn parse(body: &str) -> anyhow::Result<Page> {
    let raw: Raw = serde_json::from_str(body).context("not the comments list")?;
    let (index, total) = raw.pagination.map_or((0, 0), |p| (p.index, p.total_count));
    let mut comments: Vec<Comment> = raw
        .data
        .into_iter()
        .map(comment)
        // A taken-down comment stays only for the replies under it.
        .filter(|c| !c.removed || !c.replies.is_empty())
        .collect();
    // Stable: the rest keep the site's order (newest first).
    comments.sort_by_key(|c| !c.pinned);
    Ok(Page { comments, index, total })
}

fn fetch(project: u64, page: u32) -> anyhow::Result<Page> {
    // For screenshots of states the live site rarely shows (a pinned
    // comment, a refused request): a file standing in for its answer.
    if let Ok(path) = std::env::var("HOPPER_TEST_COMMENTS") {
        return parse(&std::fs::read_to_string(path)?);
    }
    let url = format!("https://www.curseforge.com/api/v1/mods/{project}/comments");
    // The API agent (Hopper's user agent, time limits); the API key is
    // only ever added per request in `cf`, so none goes here.
    let mut resp = crate::cf::agent()
        .get(&url)
        .query("page", page.to_string())
        .query("size", PAGE_SIZE.to_string())
        .header("Accept", "application/json")
        .call()
        .with_context(|| format!("GET comments of {project}"))?;
    let body = resp.body_mut().with_config().limit(8 * 1024 * 1024).read_to_string()?;
    parse(&body)
}

type Cache = Mutex<HashMap<(u64, u32), Arc<Page>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// One page of a CurseForge project's comments, kept for the session once
/// it loaded (failures aren't kept, so opening it again tries again).
/// Blocks: call it off the UI thread. The error reads [`LOAD_FAILED`].
pub fn page(project: u64, page: u32) -> anyhow::Result<Arc<Page>> {
    if let Some(p) = cache().lock().unwrap().get(&(project, page)) {
        return Ok(p.clone());
    }
    match fetch(project, page) {
        Ok(p) => {
            let p = Arc::new(p);
            cache().lock().unwrap().insert((project, page), p.clone());
            Ok(p)
        }
        Err(e) => {
            log::info!("comments of {project} (page {page}): {e:#}");
            Err(anyhow::anyhow!(LOAD_FAILED))
        }
    }
}

/// Who made a project, to mark their comments, for a caller that doesn't
/// have the project in hand (the panel's route). Kept for the session once
/// asked; empty when the API won't say (no key, no answer), so nothing is
/// marked then. Blocks: call it off the UI thread.
pub fn authors(project: u64) -> Vec<crate::cf::Author> {
    // For screenshots: `id:name` standing in for the project's author.
    if let Ok(v) = std::env::var("HOPPER_TEST_COMMENT_AUTHOR") {
        let (id, name) = v.split_once(':').unwrap_or(("0", &v));
        return vec![crate::cf::Author { id: id.parse().unwrap_or(0), name: name.to_string(), url: String::new() }];
    }
    static KEPT: OnceLock<Mutex<HashMap<u64, Vec<crate::cf::Author>>>> = OnceLock::new();
    let kept = KEPT.get_or_init(Default::default);
    if let Some(a) = kept.lock().unwrap().get(&project) {
        return a.clone();
    }
    match crate::cf::get_mod(project) {
        Ok(m) => {
            kept.lock().unwrap().insert(project, m.authors.clone());
            m.authors
        }
        Err(e) => {
            log::info!("authors of {project}: {e:#}");
            Vec::new()
        }
    }
}

/// A page already loaded this session, without asking.
pub fn cached(project: u64, page: u32) -> Option<Arc<Page>> {
    cache().lock().unwrap().get(&(project, page)).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("cfcomments_fixtures/curseforge-223852.json");

    #[test]
    fn pinned_comes_first_then_site_order() {
        let p = parse(FIXTURE).unwrap();
        assert_eq!(p.total, 1357);
        assert_eq!(p.index, 0);
        assert!(p.more());
        let names: Vec<&str> = p.comments.iter().map(|c| c.author.as_str()).collect();
        assert_eq!(
            names,
            ["dabby712", "nicaine64209", "user_6suobi304g6cu60l", "", "ShinigamiWolfie", "BonsUnleashed"]
        );
        assert!(p.comments[0].pinned);
        assert!(p.comments[1..].iter().all(|c| !c.pinned));
    }

    #[test]
    fn reads_author_date_avatar_and_replies() {
        let p = parse(FIXTURE).unwrap();
        let c = p.comments.iter().find(|c| c.author == "ShinigamiWolfie").unwrap();
        assert_eq!(c.posted, 1_792_378_115);
        assert!(c.avatar.as_deref().is_some_and(|a| a.starts_with("https://")));
        // Two replies, the second with one of its own: all three, one level.
        assert_eq!(c.replies.len(), 3);
        assert!(c.replies[0].text.starts_with("@ShinigamiWolfie"));
        assert!(!c.text.contains('<'));
    }

    #[test]
    fn removed_comment_kept_only_for_its_replies() {
        let p = parse(FIXTURE).unwrap();
        let c = p.comments.iter().find(|c| c.removed).unwrap();
        assert!(c.removed);
        assert!(c.text.is_empty());
        assert!(c.author.is_empty() && c.avatar.is_none());
        assert_eq!(c.replies.len(), 1);
        let gone = r#"{"data":[{"id":1,"text":"","status":2,"isPinned":false,"datePosted":0}],"pagination":{"index":0,"totalCount":1,"pageSize":20}}"#;
        assert!(parse(gone).unwrap().comments.is_empty());
    }

    #[test]
    fn replies_to_replies_come_after_the_one_they_answer() {
        let body = r#"{"data":[{"id":1,"text":"top","status":1,"datePosted":0,"replies":[
            {"id":2,"text":"a","status":1,"datePosted":0,"replies":[{"id":3,"text":"a1","status":1,"datePosted":0}]},
            {"id":4,"text":"","status":2,"datePosted":0,"replies":[{"id":5,"text":"b1","status":1,"datePosted":0}]},
            {"id":6,"text":"c","status":1,"datePosted":0}]}],"pagination":{"index":0,"totalCount":6}}"#;
        let p = parse(body).unwrap();
        let ids: Vec<u64> = p.comments[0].replies.iter().map(|c| c.id).collect();
        assert_eq!(ids, [2, 3, 5, 6]);
        assert!(p.comments[0].replies.iter().all(|c| c.replies.is_empty()));
    }

    #[test]
    fn empty_list() {
        let p = parse(r#"{"data":[],"pagination":{"index":0,"totalCount":0,"pageSize":20}}"#).unwrap();
        assert!(p.comments.is_empty());
        assert!(!p.more());
    }

    #[test]
    fn html_or_garbage_is_an_error() {
        assert!(parse("<!DOCTYPE html><html><title>Just a moment...</title></html>").is_err());
        assert!(parse("").is_err());
        assert!(parse(r#"{"statusCode":404,"message":"Mod with ID 9 not found","error":"Not Found"}"#).is_err());
        assert!(parse(r#"{"data":"nope"}"#).is_err());
    }

    #[test]
    fn tidy_keeps_lines_and_one_blank() {
        assert_eq!(tidy("Hi,\r\n\u{a0}\r\nThanks.\r\n\r\n\r\nBye  "), "Hi,\n\nThanks.\n\nBye");
        assert_eq!(tidy("\u{a0}\r\none\r\ntwo"), "one\ntwo");
    }

    #[test]
    fn more_stops_at_the_total() {
        let one = Comment { id: 1, author: String::new(), avatar: None, author_id: 0, login: String::new(), posted: 0, text: "x".into(), pinned: false, removed: false, replies: vec![] };
        let p = |index, total| Page { comments: vec![one.clone()], index, total };
        assert!(p(0, 21).more());
        assert!(!p(0, 20).more());
        assert!(!p(67, 1357).more());
        assert!(p(66, 1357).more());
    }

    #[test]
    fn project_author_matched_by_id_then_by_name() {
        let by = |id, author: &str, login: &str| Comment {
            id: 1,
            author: author.into(),
            avatar: None,
            author_id: id,
            login: login.into(),
            posted: 0,
            text: "x".into(),
            pinned: false,
            removed: false,
            replies: vec![],
        };
        let who = |id, name: &str| crate::cf::Author { id, name: name.into(), url: String::new() };
        // Same id, whatever the names say.
        assert!(by(114142538, "Other", "other").is_by(&[who(114142538, "Maker")]));
        // No usable id on the project side: the shown or the login name, any case.
        assert!(by(5, "Maker", "x").is_by(&[who(0, "maker")]));
        assert!(by(5, "Shown", "MAKER").is_by(&[who(0, "maker")]));
        // Different id and name; an empty author name matches nothing.
        assert!(!by(5, "Other", "other").is_by(&[who(6, "Maker")]));
        assert!(!by(0, "", "").is_by(&[who(0, "")]));
        // A removed comment names no one.
        let mut gone = by(6, "Maker", "maker");
        gone.removed = true;
        assert!(!gone.is_by(&[who(6, "Maker")]));
    }

    #[test]
    fn reads_the_posters_id_and_login() {
        let p = parse(FIXTURE).unwrap();
        let c = p.comments.iter().find(|c| c.author == "user_6suobi304g6cu60l").unwrap();
        assert_eq!(c.author_id, 114142538);
        assert_eq!(c.login, "user_6suobi304g6cu60l");
    }

    #[test]
    fn site_url_adds_comments() {
        assert_eq!(
            site_url("https://www.curseforge.com/minecraft/mc-mods/webdisplays-remaster/"),
            "https://www.curseforge.com/minecraft/mc-mods/webdisplays-remaster/comments"
        );
    }
}
