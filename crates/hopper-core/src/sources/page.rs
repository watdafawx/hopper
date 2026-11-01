//! What the mod peek shows for a mod from a source: GitHub's README and
//! repo facts (stars, license, last release), or what's known about a
//! Maven artifact, Jenkins job or download link. Never CurseForge or
//! Modrinth. GitHub calls go through [`super::http`], so they share the
//! update checks' cache and rate limit.

use super::{Kind, ModSource};
use crate::richtext::{self, Block};
use anyhow::Context;
use std::collections::HashMap;
use std::sync::Mutex;

/// Facts for the peek's header.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Origin {
    pub kind: Kind,
    /// "owner/repo", the artifact, the job or the file's host.
    pub name: String,
    /// The page "Open on GitHub" / "Open link" goes to.
    pub page: String,
    /// GitHub only, once the repo was read.
    pub stars: Option<u64>,
    /// "MIT", when GitHub says.
    pub license: String,
    /// When the newest release (or version, build) came out, ISO 8601.
    pub released: String,
    /// The repo's one-line description.
    pub summary: String,
}

impl Origin {
    /// The button's label: "Open on GitHub", or "Open link" elsewhere.
    pub fn open_label(&self) -> &'static str {
        if self.kind == Kind::GitHub { "Open on GitHub" } else { "Open link" }
    }
}

static ORIGINS: Mutex<Option<HashMap<u64, Origin>>> = Mutex::new(None);

fn basic(src: &ModSource) -> Origin {
    let name = match src.kind {
        Kind::GitHub => src.url.clone(),
        _ => src
            .url
            .trim_end_matches('/')
            .rsplit('/')
            .find(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| super::host_of(&src.url)),
    };
    Origin { kind: src.kind, name, page: src.page(), ..Default::default() }
}

/// The header facts for a source-tracked mod: the full set once
/// [`description`] ran, else what its source says.
pub fn origin(id: u64) -> Option<Origin> {
    if let Some(o) = ORIGINS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&id)) {
        return Some(o.clone());
    }
    super::known_source(id).map(|(s, _)| basic(&s))
}

fn keep(id: u64, o: Origin) {
    ORIGINS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(id, o);
}

/// The peek's description of a source-tracked mod, parsed. Also reads the
/// header facts ([`origin`]).
pub fn description(id: u64) -> anyhow::Result<Vec<Block>> {
    let (src, name) = super::known_source(id).context("Hopper doesn't know where this mod comes from")?;
    let (o, md) = match src.kind {
        Kind::GitHub => github(&src),
        _ => other(&src, &name),
    };
    keep(id, o);
    Ok(richtext::parse_markdown(&md))
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct Repo {
    description: Option<String>,
    stargazers_count: u64,
    default_branch: String,
    license: Option<License>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct License {
    spdx_id: Option<String>,
    name: Option<String>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct Readme {
    path: String,
    content: String,
    encoding: String,
}

fn github(src: &ModSource) -> (Origin, String) {
    let mut o = basic(src);
    let api = format!("https://api.github.com/repos/{}", src.url);
    let mut branch = "HEAD".to_string();
    match super::http::get_text(&api, true).map(|b| serde_json::from_str::<Repo>(&b)) {
        Ok(Ok(r)) => {
            o.stars = Some(r.stargazers_count);
            o.summary = r.description.unwrap_or_default();
            o.license = r
                .license
                .and_then(|l| l.spdx_id.filter(|s| s != "NOASSERTION").or(l.name))
                .unwrap_or_default();
            if !r.default_branch.is_empty() {
                branch = r.default_branch;
            }
        }
        Ok(Err(e)) => log::warn!("{api}: {e}"),
        Err(e) => log::warn!("{api}: {e:#}"),
    }
    if let Ok(c) = super::github::candidates(src) {
        o.released = c.iter().find(|c| !c.prerelease).or(c.first()).map(|c| c.date.clone()).unwrap_or_default();
    }
    let md = match super::http::get_text(&format!("{api}/readme"), true)
        .and_then(|b| Ok(serde_json::from_str::<Readme>(&b)?))
        .and_then(|r| decode_readme(&r).map(|t| (r.path, t)))
    {
        Ok((path, text)) => resolve_relative(&text, &src.url, &branch, &path),
        Err(e) => {
            log::info!("{}: no README ({e:#})", src.url);
            let mut md = format!("This mod comes from [{0}](https://github.com/{0}) on GitHub.", src.url);
            if !o.summary.is_empty() {
                md = format!("{}\n\n{md}", o.summary);
            }
            md + "\n\nThe repo has no README."
        }
    };
    (o, md)
}

fn decode_readme(r: &Readme) -> anyhow::Result<String> {
    use base64::Engine;
    anyhow::ensure!(r.encoding == "base64", "README in {} encoding", r.encoding);
    let clean: String = r.content.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD.decode(clean)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Links in a README point into the repo: images go to the raw file,
/// other links to the file's page, both relative to the README's folder.
/// Images linked as GitHub file pages (`/blob/`) load from the raw file too.
pub fn resolve_relative(md: &str, repo: &str, branch: &str, readme_path: &str) -> String {
    let dir = readme_path.rsplit_once('/').map(|(d, _)| format!("{d}/")).unwrap_or_default();
    let raw = |p: &str| match p.strip_prefix('/') {
        Some(root) => format!("https://raw.githubusercontent.com/{repo}/{branch}/{root}"),
        None => format!("https://raw.githubusercontent.com/{repo}/{branch}/{dir}{}", p.trim_start_matches("./")),
    };
    let blob = |p: &str| match p.strip_prefix('/') {
        Some(root) => format!("https://github.com/{repo}/blob/{branch}/{root}"),
        None => format!("https://github.com/{repo}/blob/{branch}/{dir}{}", p.trim_start_matches("./")),
    };
    let fix = |target: &str, image: bool| -> String {
        let t = target.trim();
        let absolute = t.starts_with("http://")
            || t.starts_with("https://")
            || t.starts_with("//")
            || t.starts_with('#')
            || t.starts_with("mailto:")
            || t.starts_with("data:");
        if absolute {
            // An image linked as its GitHub page: the page isn't an image.
            if image && let Some(rest) = t.strip_prefix("https://github.com/") {
                let mut parts = rest.splitn(4, '/');
                if let (Some(o), Some(r), Some("blob"), Some(path)) = (parts.next(), parts.next(), parts.next(), parts.next()) {
                    return format!("https://raw.githubusercontent.com/{o}/{r}/{}", path.split('?').next().unwrap_or(path));
                }
            }
            return t.to_string();
        }
        if t.is_empty() {
            return String::new();
        }
        if image { raw(t) } else { blob(t) }
    };

    let mut out = String::with_capacity(md.len() + 256);
    let b = md.as_bytes();
    let mut i = 0;
    while i < md.len() {
        let rest = &md[i..];
        // Markdown: [text](target "title") and ![alt](target).
        if rest.starts_with("](") {
            let image = is_image_link(&md[..i]);
            let inner = &rest[2..];
            let end = inner.find(')').unwrap_or(inner.len());
            let body = &inner[..end];
            let (target, title) = match body.find(char::is_whitespace) {
                Some(s) => body.split_at(s),
                None => (body, ""),
            };
            let target = target.trim_start_matches('<').trim_end_matches('>');
            out.push_str("](");
            out.push_str(&fix(target, image));
            out.push_str(title);
            i += 2 + end;
            continue;
        }
        // HTML: src="…" (images) and href="…".
        let attr = ["src=\"", "src='", "href=\"", "href='"].into_iter().find(|a| {
            rest.len() >= a.len() && rest.as_bytes()[..a.len()].eq_ignore_ascii_case(a.as_bytes()) && (i == 0 || b[i - 1].is_ascii_whitespace())
        });
        if let Some(a) = attr {
            let quote = a.chars().last().unwrap_or('"');
            let inner = &rest[a.len()..];
            let end = inner.find(quote).unwrap_or(inner.len());
            out.push_str(&rest[..a.len()]);
            out.push_str(&fix(&inner[..end], a.starts_with("src")));
            i += a.len() + end;
            continue;
        }
        let c = rest.chars().next().unwrap_or(' ');
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// The text before a "](" closes an image (`![alt`), not a link.
fn is_image_link(before: &str) -> bool {
    let mut depth = 0;
    for (j, c) in before.char_indices().rev() {
        match c {
            ']' => depth += 1,
            '[' if depth == 0 => return before[..j].ends_with('!'),
            '[' => depth -= 1,
            '\n' if before[..j].ends_with('\n') => return false,
            _ => {}
        }
    }
    false
}

/// Maven, Jenkins or a link: what's known, as Markdown.
fn other(src: &ModSource, name: &str) -> (Origin, String) {
    let mut o = basic(src);
    let what = match src.kind {
        Kind::Maven => "a Maven repository",
        Kind::Jenkins => "a Jenkins build server",
        _ => "a download link",
    };
    let mut md = format!("Hopper gets **{name}** from {what}:\n\n[{0}]({0})", src.url);
    match super::list_all(src, "", crate::cf::Loader::Any) {
        Ok(all) => {
            o.released = all.first().map(|c| c.date.clone()).unwrap_or_default();
            let label = match src.kind {
                Kind::Jenkins => "Recent builds",
                Kind::Url => "The file",
                _ => "Recent versions",
            };
            if !all.is_empty() {
                md.push_str(&format!("\n\n### {label}\n"));
                for c in all.iter().take(12) {
                    let date = if c.date.is_empty() { String::new() } else { format!(" · {}", crate::time::relative(&c.date)) };
                    let installed = if !src.seen.is_empty() && c.version == src.seen { " · installed" } else { "" };
                    md.push_str(&format!("\n- {} ({}){date}{installed}", c.version, c.file_name));
                }
            }
        }
        Err(e) => md.push_str(&format!("\n\nCouldn't list its files: {e:#}")),
    }
    (o, md)
}
