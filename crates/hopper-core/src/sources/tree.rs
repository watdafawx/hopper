//! Packs kept as files on a GitHub branch, with no release to download
//! (Faithful's mod packs, for one): `github.com/<owner>/<repo>/tree/<branch>[/<folder>]`.
//!
//! The source's url is "owner/repo/tree/branch[/folder]", so its page and
//! trust key say exactly which folder it is. Its "version" is the newest
//! commit that touched the folder; one commits call per check (cached by
//! ETag, so an unchanged branch costs nothing) says whether there's a newer
//! one. Installing downloads the branch at that commit as a zip (GitHub's
//! codeload archive) and repacks the folder as a pack zip with its
//! `pack.mcmeta` at the top. Entries are written in name order with a fixed
//! date, so the same commit gives the same bytes. A shared pack pins the
//! commit itself.

use super::{Candidate, http};
use anyhow::{Context, bail};
use serde::Deserialize;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// A folder on a branch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tree {
    pub owner: String,
    pub repo: String,
    pub branch: String,
    /// Folder inside the repo, "" for its root; no leading or trailing "/".
    pub path: String,
}

impl Tree {
    /// The source url: "owner/repo/tree/branch[/folder]".
    pub fn url(&self) -> String {
        let mut u = format!("{}/{}/tree/{}", self.owner, self.repo, self.branch);
        if !self.path.is_empty() {
            u.push('/');
            u.push_str(&self.path);
        }
        u
    }

    pub fn repo_key(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }

    /// The folder's page on GitHub.
    pub fn page(&self) -> String {
        format!("https://github.com/{}", self.url())
    }

    /// The same branch, one folder down.
    pub fn child(&self, name: &str) -> Tree {
        let path = if self.path.is_empty() { name.to_string() } else { format!("{}/{name}", self.path) };
        Tree { path, ..self.clone() }
    }

    /// The pack's file name. It stays the same from one commit to the next:
    /// the game and Paxi remember packs by file name, so an update keeps
    /// its place in the pack order.
    pub fn file_name(&self) -> String {
        let mut stem = self.repo.clone();
        if let Some(last) = self.path.rsplit('/').next().filter(|l| !l.is_empty()) {
            stem.push('-');
            stem.push_str(last);
        }
        stem.push('-');
        stem.push_str(&self.branch);
        let clean: String = stem
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+') { c } else { '-' })
            .collect();
        format!("{clean}.zip")
    }
}

/// The folder a source url ("owner/repo/tree/branch[/folder]") names.
/// A branch with "/" in its name can't be told from a folder; the first
/// part is taken as the branch.
pub fn of_url(url: &str) -> Option<Tree> {
    let mut it = url.split('/').filter(|p| !p.is_empty());
    let owner = it.next()?.to_string();
    let repo = it.next()?.trim_end_matches(".git").to_string();
    if it.next()? != "tree" {
        return None;
    }
    let branch = super::github::percent_decode(it.next()?);
    let path: Vec<String> = it.map(super::github::percent_decode).collect();
    Some(Tree { owner, repo, branch, path: path.join("/") })
}

/// The folder a pasted link names: "https://github.com/o/r/tree/branch/folder".
pub fn parse_link(link: &str) -> Option<Tree> {
    let rest = link
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.");
    let rest = rest.strip_prefix("github.com/")?;
    let rest = rest.split(['?', '#']).next()?.trim_end_matches('/');
    of_url(rest)
}

/// One commit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    /// ISO 8601.
    pub date: String,
    pub message: String,
    pub author: String,
}

impl Commit {
    /// "989f3bf · 2026-08-01".
    pub fn label(&self) -> String {
        label(&self.sha, &self.date)
    }
}

/// "989f3bf · 2026-08-01" for a commit and its date.
pub fn label(sha: &str, date: &str) -> String {
    let short = &sha[..sha.len().min(7)];
    match date.get(..10) {
        Some(d) => format!("{short} · {d}"),
        None => short.to_string(),
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CommitJson {
    sha: String,
    commit: CommitInner,
    author: Option<super::github::User>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CommitInner {
    message: String,
    committer: Option<Who>,
    author: Option<Who>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Who {
    name: String,
    date: String,
}

fn to_commit(c: CommitJson) -> Commit {
    let date = c.commit.committer.as_ref().or(c.commit.author.as_ref()).map(|w| w.date.clone()).unwrap_or_default();
    let author = c
        .author
        .map(|u| u.login)
        .filter(|l| !l.is_empty())
        .or_else(|| c.commit.author.map(|w| w.name))
        .unwrap_or_default();
    Commit { sha: c.sha, date, message: c.commit.message, author }
}

/// Commits as the commits API lists them, newest first.
pub fn parse_commits(json: &str) -> anyhow::Result<Vec<Commit>> {
    let list: Vec<CommitJson> = serde_json::from_str(json)?;
    Ok(list.into_iter().map(to_commit).collect())
}

/// The commits a compare lists (oldest first, as GitHub gives them).
pub fn parse_compare(json: &str) -> anyhow::Result<Vec<Commit>> {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Cmp {
        commits: Vec<CommitJson>,
    }
    let c: Cmp = serde_json::from_str(json)?;
    Ok(c.commits.into_iter().map(to_commit).collect())
}

fn api(t: &Tree, rest: &str) -> String {
    format!("https://api.github.com/repos/{}/{}{rest}", t.owner, t.repo)
}

/// The newest commit that touched the folder: one call, free when the
/// answer hasn't changed (ETag).
pub fn head(t: &Tree) -> anyhow::Result<Commit> {
    let mut q = format!("/commits?sha={}&per_page=1", super::enc(&t.branch));
    if !t.path.is_empty() {
        q.push_str(&format!("&path={}", t.path.split('/').map(super::enc).collect::<Vec<_>>().join("/")));
    }
    let body = http::get_text(&api(t, &q), true)?;
    parse_commits(&body)?
        .into_iter()
        .next()
        .with_context(|| format!("{} has no commits on {}", if t.path.is_empty() { &t.repo } else { &t.path }, t.branch))
}

/// The commits after `from` up to `to`, newest first, for "What changed".
pub fn changes(t: &Tree, from: &str, to: &str) -> anyhow::Result<Vec<Commit>> {
    let body = http::get_text(&api(t, &format!("/compare/{from}...{to}")), true)?;
    let mut list = parse_compare(&body)?;
    list.reverse();
    Ok(list)
}

/// Commit messages as release notes: one line each, newest first.
pub fn notes(commits: &[Commit]) -> String {
    let mut out = String::new();
    for c in commits.iter().take(30) {
        let first = c.message.lines().next().unwrap_or("").trim();
        out.push_str(&format!("- {first} ({})\n", &c.sha[..c.sha.len().min(7)]));
    }
    if commits.len() > 30 {
        out.push_str(&format!("- and {} more\n", commits.len() - 30));
    }
    out
}

/// An entry of a folder listing.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
}

pub fn parse_listing(json: &str) -> anyhow::Result<Vec<Entry>> {
    Ok(serde_json::from_str(json)?)
}

/// The folder's files and folders (one call).
pub fn listing(t: &Tree) -> anyhow::Result<Vec<Entry>> {
    let path: String = t.path.split('/').filter(|p| !p.is_empty()).map(super::enc).collect::<Vec<_>>().join("/");
    let body = http::get_text(&api(t, &format!("/contents/{path}?ref={}", super::enc(&t.branch))), true)?;
    match parse_listing(&body) {
        Ok(l) => Ok(l),
        Err(_) => bail!("{} is a file, not a folder", t.page()),
    }
}

/// What marks a pack's top folder: `pack.mcmeta` for resource and data
/// packs, a `shaders` folder for shader packs.
pub fn is_pack_root(entries: &[Entry], class: u32) -> bool {
    if class == crate::cf::CLASS_SHADERS {
        entries.iter().any(|e| e.kind == "dir" && e.name.eq_ignore_ascii_case("shaders"))
    } else {
        entries.iter().any(|e| e.kind == "file" && e.name == "pack.mcmeta")
    }
}

/// How big the repo is (GitHub's figure for every branch together, so the
/// download is usually smaller). Bytes; 0 when unknown.
pub fn repo_size(t: &Tree) -> u64 {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Repo {
        size: u64,
    }
    http::get_text(&api(t, ""), true)
        .ok()
        .and_then(|b| serde_json::from_str::<Repo>(&b).ok())
        .map_or(0, |r| r.size * 1024)
}

/// The archive of the whole repo at one commit.
pub fn archive_url(t: &Tree, sha: &str) -> String {
    format!("https://codeload.github.com/{}/{}/zip/{sha}", t.owner, t.repo)
}

/// The file a folder at a commit installs as.
pub fn candidate(t: &Tree, c: &Commit, size: u64) -> Candidate {
    Candidate {
        file_name: t.file_name(),
        url: archive_url(t, &c.sha),
        version: c.sha.clone(),
        title: c.label(),
        date: c.date.clone(),
        size,
        notes: c.message.clone(),
        author: c.author.clone(),
        page: t.page(),
        ..Default::default()
    }
}

/// What a pasted folder link offers: the folder itself when it's a pack,
/// else each folder one level down (the player picks). Three calls: the
/// listing, the newest commit, the repo's size.
pub fn resolve(t: &Tree, class: u32) -> anyhow::Result<Vec<Candidate>> {
    let entries = listing(t)?;
    let size = repo_size(t);
    if is_pack_root(&entries, class) {
        let c = head(t)?;
        return Ok(vec![candidate(t, &c, size)]);
    }
    let subs: Vec<&Entry> = entries.iter().filter(|e| e.kind == "dir" && !e.name.starts_with('.')).collect();
    if subs.is_empty() {
        bail!("{}", not_a_pack(class));
    }
    // The folders' own commits are looked up once one is picked; until
    // then they share the branch's.
    let c = head(t)?;
    Ok(subs
        .into_iter()
        .map(|e| {
            let child = t.child(&e.name);
            let mut cand = candidate(&child, &c, size);
            cand.title = e.name.clone();
            cand
        })
        .collect())
}

fn not_a_pack(class: u32) -> &'static str {
    if class == crate::cf::CLASS_SHADERS {
        "This folder isn't a shader pack (no shaders folder), and neither are the folders in it."
    } else {
        "This folder isn't a pack (no pack.mcmeta), and neither are the folders in it."
    }
}

/// Downloads the repo at `sha` and writes the folder as a pack zip at
/// `dest`. `tick` gets the bytes downloaded so far.
pub fn build(t: &Tree, sha: &str, class: u32, dest: &Path, tick: impl FnMut(u64)) -> anyhow::Result<()> {
    let dir = crate::paths::temp_dir().join("hopper-downloads");
    std::fs::create_dir_all(&dir)?;
    let archive = dir.join(format!("{}-{}-{}.archive.zip", t.owner, t.repo, &sha[..sha.len().min(12)]));
    crate::cf::download(&archive_url(t, sha), &archive, tick).with_context(|| format!("downloading {}", t.repo))?;
    let r = repack(&archive, &t.path, class, dest);
    let _ = std::fs::remove_file(&archive);
    r
}

/// Writes the folder `path` of a repo archive (its entries all under one
/// top folder, "repo-sha/") as a pack zip at `dest`.
pub fn repack(archive: &Path, path: &str, class: u32, dest: &Path) -> anyhow::Result<()> {
    let mut zin = zip::ZipArchive::new(std::fs::File::open(archive)?).context("the download isn't a zip")?;
    let top = {
        let first = zin.by_index(0).context("the download is empty")?;
        first.name().split('/').next().unwrap_or("").to_string()
    };
    let prefix = if path.is_empty() { format!("{top}/") } else { format!("{top}/{}/", path.trim_matches('/')) };
    let mut names: Vec<(String, usize)> = (0..zin.len())
        .filter_map(|i| {
            let f = zin.by_index(i).ok()?;
            let rel = f.name().strip_prefix(&prefix)?.to_string();
            (!rel.is_empty() && !f.is_dir() && safe(&rel)).then_some((rel, i))
        })
        .collect();
    names.sort();
    let marker = if class == crate::cf::CLASS_SHADERS {
        names.iter().any(|(n, _)| n.starts_with("shaders/"))
    } else {
        names.iter().any(|(n, _)| n == "pack.mcmeta")
    };
    if !marker {
        if class == crate::cf::CLASS_SHADERS {
            bail!("That folder isn't a shader pack: it has no shaders folder.");
        }
        bail!("That folder isn't a pack: it has no pack.mcmeta.");
    }
    let tmp = dest.with_extension("zip.part");
    {
        let mut zout = zip::ZipWriter::new(std::fs::File::create(&tmp)?);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .last_modified_time(zip::DateTime::default());
        let mut buf = Vec::new();
        for (name, i) in &names {
            let mut f = zin.by_index(*i)?;
            buf.clear();
            f.read_to_end(&mut buf)?;
            // Repo housekeeping isn't part of the pack.
            if name.starts_with(".github/") || name == ".gitignore" || name == ".gitattributes" {
                continue;
            }
            zout.start_file(name.as_str(), opts)?;
            zout.write_all(&buf)?;
        }
        zout.finish()?;
    }
    std::fs::rename(&tmp, dest).or_else(|_| {
        std::fs::copy(&tmp, dest)?;
        std::fs::remove_file(&tmp)
    })?;
    Ok(())
}

/// A path inside a zip that stays inside the pack.
fn safe(rel: &str) -> bool {
    !rel.starts_with('/') && !rel.contains('\\') && rel.split('/').all(|p| p != ".." && !p.contains(':'))
}

/// Where a built pack goes before it's put in place.
pub fn holding_path(file_name: &str) -> PathBuf {
    crate::paths::temp_dir().join("hopper-downloads").join(file_name)
}
