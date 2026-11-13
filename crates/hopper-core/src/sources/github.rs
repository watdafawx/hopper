//! GitHub releases: the jars attached to a repo's releases.

use super::{Candidate, Kind, ModSource};
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Release {
    pub tag_name: String,
    pub name: Option<String>,
    pub body: Option<String>,
    pub draft: bool,
    pub prerelease: bool,
    pub published_at: Option<String>,
    pub html_url: String,
    pub author: Option<User>,
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct User {
    pub login: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Asset {
    pub name: String,
    pub size: u64,
    pub browser_download_url: String,
    pub updated_at: Option<String>,
    /// "sha256:…", which GitHub lists for assets uploaded since mid-2025.
    pub digest: Option<String>,
}

/// "owner/repo" from a GitHub link: the repo, its releases page, one
/// release, or one asset's download link.
pub fn parse_link(link: &str) -> Option<ModSource> {
    let rest = link
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.");
    let rest = rest.strip_prefix("github.com/")?;
    let mut parts = rest.split(['/', '?', '#']).filter(|p| !p.is_empty());
    let owner = parts.next()?;
    let repo = parts.next()?.trim_end_matches(".git");
    Some(ModSource {
        kind: Kind::GitHub,
        url: format!("{owner}/{repo}"),
        publisher: format!("{owner}/{repo}"),
        ..Default::default()
    })
}

/// The tag a release link names (".../releases/tag/v1.2" or an asset's
/// ".../releases/download/v1.2/x.jar").
pub fn tag_in_link(link: &str) -> Option<String> {
    let i = link.find("/releases/tag/").map(|i| i + 14).or_else(|| link.find("/releases/download/").map(|i| i + 19))?;
    let tag = link[i..].split(['/', '?', '#']).next()?;
    (!tag.is_empty()).then(|| percent_decode(tag))
}

/// The asset a download link names.
pub fn asset_in_link(link: &str) -> Option<String> {
    let i = link.find("/releases/download/")? + 19;
    let mut it = link[i..].split('/');
    it.next()?;
    let name = it.next()?.split(['?', '#']).next()?;
    (!name.is_empty()).then(|| percent_decode(name))
}

pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(Ok(v)) = s.get(i + 1..i + 3).map(|h| u8::from_str_radix(h, 16))
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The releases list, newest first.
fn releases_url(repo: &str) -> String {
    format!("https://api.github.com/repos/{repo}/releases?per_page=30")
}

/// Every jar of the repo's recent releases, newest release first.
pub fn candidates(src: &ModSource) -> anyhow::Result<Vec<Candidate>> {
    let body = super::http::get_text_fresh(&releases_url(&src.url), true, super::http::LIST_FRESH)?;
    let releases = parse_releases(&body)?;
    Ok(from_releases_for(&releases, src.class))
}

/// One release by its tag, for a link to a release older than the recent
/// list.
pub fn release_by_tag(src: &ModSource, tag: &str) -> anyhow::Result<Vec<Candidate>> {
    let tag_path: String = tag.split('/').map(|p| p.to_string()).collect::<Vec<_>>().join("%2F");
    let url = format!("https://api.github.com/repos/{}/releases/tags/{tag_path}", src.url);
    let r: Release = serde_json::from_str(&super::http::get_text_fresh(&url, true, super::http::TAG_FRESH)?)?;
    Ok(from_releases_for(std::slice::from_ref(&r), src.class))
}

pub fn parse_releases(json: &str) -> anyhow::Result<Vec<Release>> {
    Ok(serde_json::from_str(json)?)
}

/// The candidates in a releases list: drafts skipped, jars only.
pub fn from_releases(releases: &[Release]) -> Vec<Candidate> {
    from_releases_for(releases, 0)
}

/// [`from_releases`] for a source of `class`: zips for packs, jars for
/// mods (0).
pub fn from_releases_for(releases: &[Release], class: u32) -> Vec<Candidate> {
    let ext = if class == 0 { ".jar" } else { ".zip" };
    let mut out = Vec::new();
    for r in releases.iter().filter(|r| !r.draft) {
        for a in r.assets.iter().filter(|a| a.name.to_ascii_lowercase().ends_with(ext)) {
            out.push(Candidate {
                file_name: a.name.clone(),
                url: a.browser_download_url.clone(),
                version: r.tag_name.clone(),
                title: r.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| r.tag_name.clone()),
                date: a.updated_at.clone().or_else(|| r.published_at.clone()).unwrap_or_default(),
                size: a.size,
                notes: r.body.clone().unwrap_or_default(),
                author: r.author.as_ref().map(|u| u.login.clone()).unwrap_or_default(),
                prerelease: r.prerelease,
                digest: a
                    .digest
                    .as_deref()
                    .and_then(|d| d.strip_prefix("sha256:"))
                    .unwrap_or("")
                    .to_ascii_lowercase(),
                page: r.html_url.clone(),
                fit: Default::default(),
            });
        }
    }
    out
}
