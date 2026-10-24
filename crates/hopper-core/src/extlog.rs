//! Changelogs that are only a link to GitHub, fetched and shown in place:
//! the matching section of a CHANGELOG file, a commit's message, a
//! compare's commits, a release's notes or a pull request. The link stays
//! underneath. Anything else, or anything that can't be fetched, stays the
//! link it was.

use crate::cf;
use crate::richtext::{self, Block, Kind, Span};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

#[derive(Clone, Debug, PartialEq)]
enum Target {
    /// A file in a repo; `anchor` is the section's heading slug.
    File {
        repo: String,
        git_ref: String,
        path: String,
        anchor: Option<String>,
    },
    Commit { repo: String, sha: String },
    Compare { repo: String, range: String },
    Release { repo: String, tag: String },
    Pull { repo: String, number: u64 },
    /// A wiki page of a repo ("Home" when the link names none).
    Wiki { repo: String, page: String },
}

impl Target {
    fn repo(&self) -> &str {
        match self {
            Target::File { repo, .. }
            | Target::Commit { repo, .. }
            | Target::Compare { repo, .. }
            | Target::Release { repo, .. }
            | Target::Pull { repo, .. }
            | Target::Wiki { repo, .. } => repo,
        }
    }

    /// What the link under the fetched text says.
    fn label(&self) -> String {
        match self {
            Target::File { repo, path, .. } => format!("{repo} · {}", path.rsplit('/').next().unwrap_or(path)),
            Target::Commit { repo, sha } => format!("{repo} · commit {}", &sha[..sha.len().min(7)]),
            Target::Compare { repo, range } => format!("{repo} · {range}"),
            Target::Release { repo, tag } => format!("{repo} · release {tag}"),
            Target::Pull { repo, number } => format!("{repo} · pull request #{number}"),
            Target::Wiki { repo, page } => format!("{repo} · wiki {page}"),
        }
    }
}

/// A GitHub link Hopper can fetch the text behind.
fn parse_target(url: &str) -> Option<Target> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let (rest, anchor) = match rest.split_once('#') {
        Some((r, a)) => (r, Some(a.to_string()).filter(|a| !a.is_empty())),
        None => (rest, None),
    };
    let rest = rest.split('?').next()?;
    let mut segs = rest.split('/').filter(|s| !s.is_empty());
    let host = segs.next()?.to_ascii_lowercase();
    let segs: Vec<&str> = segs.collect();
    if host == "raw.githubusercontent.com" {
        let [owner, name, tail @ ..] = segs.as_slice() else { return None };
        let (git_ref, path) = match tail {
            ["refs", "heads" | "tags", r, path @ ..] => (*r, path),
            [r, path @ ..] => (*r, path),
            [] => return None,
        };
        return (!path.is_empty()).then(|| Target::File {
            repo: format!("{owner}/{name}"),
            git_ref: git_ref.to_string(),
            path: path.join("/"),
            anchor,
        });
    }
    if host != "github.com" && host != "www.github.com" {
        return None;
    }
    let [owner, name, tail @ ..] = segs.as_slice() else { return None };
    let repo = format!("{owner}/{name}");
    if let ["wiki", page @ ..] = tail {
        let page = page.first().copied().unwrap_or("Home");
        return (!page.starts_with('_')).then(|| Target::Wiki { repo, page: page.to_string() });
    }
    let [kind, tail @ ..] = tail else { return None };
    match (*kind, tail) {
        ("blob", [r, path @ ..]) if !path.is_empty() => Some(Target::File {
            repo,
            git_ref: r.to_string(),
            path: path.join("/"),
            anchor,
        }),
        ("commit", [sha, ..]) => Some(Target::Commit { repo, sha: sha.to_string() }),
        ("pull", [_, "commits", sha, ..]) => Some(Target::Commit { repo, sha: sha.to_string() }),
        ("pull", [n, ..]) => Some(Target::Pull { repo, number: n.parse().ok()? }),
        ("compare", [range, ..]) if range.contains("..") => Some(Target::Compare { repo, range: range.to_string() }),
        ("releases", ["tag", tag @ ..]) if !tag.is_empty() => Some(Target::Release { repo, tag: tag.join("/") }),
        _ => None,
    }
}

/// The one URL a changelog consists of, when it has nothing much besides
/// ("Changelog: <link>", or the bare link).
fn link_only(blocks: &[Block]) -> Option<String> {
    let mut urls: Vec<String> = Vec::new();
    let mut words = 0usize;
    for b in blocks {
        if matches!(b.kind, Kind::Row(_) | Kind::Code) {
            return None;
        }
        for s in &b.spans {
            if s.image.is_some() {
                return None;
            }
            if let Some(l) = &s.link {
                urls.push(l.clone());
                continue;
            }
            for (part, is_url) in richtext::bare_urls(&s.text) {
                if is_url {
                    urls.push(part.to_string());
                } else {
                    words += part.split_whitespace().count();
                }
            }
        }
    }
    urls.dedup_by(|a, b| a.trim_end_matches('/') == b.trim_end_matches('/'));
    match urls.as_slice() {
        [u] if words <= 8 => Some(u.clone()),
        _ => None,
    }
}

/// `blocks` with a changelog that's only a GitHub link replaced by what it
/// links to, `f` telling which version's section to show.
pub fn expand(blocks: Vec<Block>, f: &cf::File) -> Vec<Block> {
    let Some(url) = link_only(&blocks) else { return blocks };
    let Some(target) = parse_target(&url) else { return blocks };
    match resolve(&target, f) {
        Ok(Some(md)) => {
            let mut out = richtext::parse_markdown(&md);
            if out.is_empty() {
                return blocks;
            }
            out.push(Block {
                kind: Kind::Para,
                spans: vec![
                    Span {
                        text: "From ".into(),
                        italic: true,
                        ..Default::default()
                    },
                    Span {
                        text: target.label(),
                        italic: true,
                        link: Some(url),
                        ..Default::default()
                    },
                ],
            });
            out
        }
        Ok(None) => blocks,
        Err(e) => {
            log::warn!("changelog {url}: {e:#}");
            blocks
        }
    }
}

/// The Markdown behind `t`, or `None` when there's nothing to show for `f`.
fn resolve(t: &Target, f: &cf::File) -> anyhow::Result<Option<String>> {
    let repo = t.repo();
    let md = match t {
        Target::File { git_ref, path, anchor, .. } => {
            let ext = path.rsplit('/').next().unwrap_or("").rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
            if !matches!(ext.as_deref(), None | Some("md" | "markdown" | "txt")) {
                return Ok(None);
            }
            let text = fetch(&format!("https://raw.githubusercontent.com/{repo}/{git_ref}/{path}"), FILE_TTL)?;
            let Some(s) = section(&text, anchor.as_deref(), &version_hints(f)) else {
                return Ok(None);
            };
            let dir = path.rsplit_once('/').map(|(d, _)| format!("{d}/")).unwrap_or_default();
            rebase_links(
                &s,
                &format!("https://github.com/{repo}/blob/{git_ref}/{path}"),
                &format!("https://github.com/{repo}/blob/{git_ref}/{dir}"),
                &format!("https://raw.githubusercontent.com/{repo}/{git_ref}/{dir}"),
            )
        }
        Target::Wiki { page, .. } => {
            let text = fetch(&format!("https://raw.githubusercontent.com/wiki/{repo}/{page}.md"), FILE_TTL)?;
            // Wikis often list only x.y.0; a patch falls back to the nearest
            // heading below it in the same x.y.
            let mut hints = Vec::new();
            for v in version_hints(f) {
                hints.extend(patch_fallbacks(&v));
            }
            let Some(s) = section(&text, None, &hints) else {
                return Ok(None);
            };
            s
        }
        Target::Commit { sha, .. } => {
            let v = api(&format!("repos/{repo}/commits/{sha}"))?;
            message(v["commit"]["message"].as_str().unwrap_or(""))
        }
        Target::Compare { range, .. } => {
            let v = api(&format!("repos/{repo}/compare/{range}"))?;
            let commits = v["commits"].as_array().cloned().unwrap_or_default();
            let total = v["total_commits"].as_u64().unwrap_or(commits.len() as u64) as usize;
            let mut lines: Vec<String> = commits
                .iter()
                .rev()
                .take(MAX_COMMITS)
                .filter_map(|c| {
                    let title = c["commit"]["message"].as_str()?.lines().next()?.trim();
                    let sha = c["sha"].as_str().unwrap_or("");
                    Some(format!("- {title} (`{}`)", &sha[..sha.len().min(7)]))
                })
                .collect();
            if total > lines.len() && !lines.is_empty() {
                lines.push(format!("- *…and {} more*", total - lines.len()));
            }
            lines.join("\n")
        }
        Target::Release { tag, .. } => {
            let v = api(&format!("repos/{repo}/releases/tags/{tag}"))?;
            let body = v["body"].as_str().unwrap_or("").trim();
            if !body.is_empty() {
                body.to_string()
            } else {
                // GitHub's page shows an annotated tag's message when the
                // release itself has no notes.
                match tag_message(repo, tag) {
                    Ok(Some(m)) => m,
                    other => {
                        if let Err(e) = other {
                            log::warn!("tag {repo} {tag}: {e:#}");
                        }
                        v["name"].as_str().unwrap_or("").to_string()
                    }
                }
            }
        }
        Target::Pull { number, .. } => {
            let v = api(&format!("repos/{repo}/pulls/{number}"))?;
            let title = v["title"].as_str().unwrap_or("").trim();
            let body = v["body"].as_str().unwrap_or("").trim();
            format!("**{title}**\n\n{body}")
        }
    };
    let md = issue_links(md.trim(), repo);
    Ok(Some(md).filter(|m| !m.trim().is_empty()))
}

/// Most commits listed for a compare link.
const MAX_COMMITS: usize = 50;
/// A CHANGELOG file changes as versions come out; a commit or release
/// doesn't.
const FILE_TTL: Duration = Duration::from_secs(6 * 3600);
const API_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

/// A commit message as Markdown: its title in bold, the rest as written.
fn message(m: &str) -> String {
    let m = m.trim();
    match m.split_once('\n') {
        Some((title, body)) => format!("**{}**\n\n{}", title.trim(), body.trim()),
        None => format!("**{m}**"),
    }
}

/// An annotated tag's message as Markdown, `None` for a lightweight tag or
/// an empty message.
fn tag_message(repo: &str, tag: &str) -> anyhow::Result<Option<String>> {
    let r = api(&format!("repos/{repo}/git/ref/tags/{tag}"))?;
    if r["object"]["type"].as_str() != Some("tag") {
        return Ok(None);
    }
    let Some(sha) = r["object"]["sha"].as_str() else { return Ok(None) };
    let t = api(&format!("repos/{repo}/git/tags/{sha}"))?;
    Ok(tag_text(t["message"].as_str().unwrap_or("")))
}

/// A tag message without its signature, and without a first line that only
/// says "Changelog" (GitHub puts that beside the tag name as the title).
fn tag_text(m: &str) -> Option<String> {
    let m = m.split("-----BEGIN PGP SIGNATURE-----").next().unwrap_or(m);
    let m = m.split("-----BEGIN SSH SIGNATURE-----").next().unwrap_or(m).trim();
    let (first, rest) = m.split_once('\n').unwrap_or((m, ""));
    let plain = first.trim().trim_end_matches(':').trim_start_matches('#').trim();
    let m = if !rest.trim().is_empty() && plain.eq_ignore_ascii_case("changelog") {
        rest.trim()
    } else {
        m
    };
    (!m.is_empty()).then(|| m.to_string())
}

/// The version numbers in a file's name ("1.11.0" in "Excessive Utilities
/// 1.21.1 1.11.0-build.69"), the Minecraft versions left out, longest first.
fn version_hints(f: &cf::File) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in [&f.display_name, &f.file_name] {
        let mut cur = String::new();
        for c in name.chars().chain([' ']) {
            if c.is_ascii_digit() || (c == '.' && !cur.is_empty() && !cur.ends_with('.')) {
                cur.push(c);
                continue;
            }
            let v = cur.trim_end_matches('.').to_string();
            cur.clear();
            if v.contains('.') && !f.game_versions.contains(&v) && !out.contains(&v) {
                out.push(v);
            }
        }
    }
    out.sort_by_key(|v| std::cmp::Reverse(v.len()));
    out
}

/// `v`, then the same x.y with each lower patch down to 0 ("5.27.2" gives
/// 5.27.2, 5.27.1, 5.27.0). Anything but a plain x.y.z stays as it is.
fn patch_fallbacks(v: &str) -> Vec<String> {
    let parts: Vec<&str> = v.split('.').collect();
    let patch = match parts.as_slice() {
        [_, _, p] => p.parse::<u32>().ok(),
        _ => None,
    };
    let Some(p) = patch else { return vec![v.to_string()] };
    (0..=p.min(500)).rev().map(|n| format!("{}.{}.{n}", parts[0], parts[1])).collect()
}

/// GitHub's anchor for a heading: lower case, punctuation dropped, spaces
/// to hyphens.
fn slug(heading: &str) -> String {
    heading
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

/// A heading's text as shown: `[1.11.0](url)` reads "1.11.0".
fn heading_text(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw.trim().trim_end_matches('#').trim();
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find("](").and_then(|c| after[c..].find(')').map(|e| (c, c + e))) {
            Some((close, end)) => {
                out.push_str(&after[..close]);
                rest = &after[end + 1..];
            }
            None => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.replace(['*', '`'], "")
}

/// Whether `text` has `v` as a whole version, not part of a longer one.
fn has_version(text: &str, v: &str) -> bool {
    text.match_indices(v).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let mut after = text[i + v.len()..].chars();
        let (a1, a2) = (after.next(), after.next());
        let digit = |c: Option<char>| c.is_some_and(|c| c.is_ascii_digit());
        !digit(before) && before != Some('.') && !digit(a1) && !(a1 == Some('.') && digit(a2))
    })
}

/// The part of a Markdown file under the heading `anchor` points at, or
/// failing that the first heading naming one of `versions`; without the
/// heading itself. `None` when no heading matches or it's empty.
fn section(md: &str, anchor: Option<&str>, versions: &[String]) -> Option<String> {
    let lines: Vec<&str> = md.lines().collect();
    let mut heads: Vec<(usize, usize, String)> = Vec::new();
    let mut fence = false;
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
            continue;
        }
        if fence || l.len() - t.len() > 3 {
            continue;
        }
        let level = t.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&level) && t[level..].starts_with([' ', '\t']) {
            heads.push((i, level, heading_text(&t[level..])));
        }
    }
    let mut seen: HashMap<String, usize> = HashMap::new();
    let slugs: Vec<String> = heads
        .iter()
        .map(|(_, _, text)| {
            let s = slug(text);
            let n = seen.entry(s.clone()).or_default();
            let out = if *n == 0 { s } else { format!("{s}-{n}") };
            *n += 1;
            out
        })
        .collect();
    let anchor = anchor.map(|a| a.to_lowercase());
    let pick = anchor
        .as_ref()
        .and_then(|a| slugs.iter().position(|s| s == a))
        .or_else(|| versions.iter().find_map(|v| heads.iter().position(|(_, _, t)| has_version(t, v))))?;
    let (start, level, _) = heads[pick];
    let end = heads[pick + 1..]
        .iter()
        .find(|(_, l, _)| *l <= level)
        .map_or(lines.len(), |h| h.0);
    let body = lines[start + 1..end].join("\n");
    let body = body.trim();
    (!body.is_empty()).then(|| body.chars().take(30_000).collect())
}

/// Relative links and images in a repo's Markdown made absolute: anchors
/// against `file` (its page), links against `page` (its folder on
/// github.com), images against `raw`.
fn rebase_links(md: &str, file: &str, page: &str, raw: &str) -> String {
    let fix = |u: &str, img: bool| -> Option<String> {
        let u = u.trim();
        if u.is_empty() || u.contains("://") || u.starts_with("mailto:") || u.starts_with("//") {
            return None;
        }
        if u.starts_with('#') {
            return Some(format!("{file}{u}"));
        }
        Some(join(if img { raw } else { page }, u))
    };
    let mut out = String::with_capacity(md.len());
    let mut opens: Vec<usize> = Vec::new();
    let mut rest = md;
    while let Some(i) = rest.find(['[', ']', '"']) {
        let (head, tail) = rest.split_at(i);
        out.push_str(head);
        let c = tail.as_bytes()[0];
        if c == b'[' {
            opens.push(out.len());
            out.push('[');
            rest = &tail[1..];
            continue;
        }
        if c == b']' && tail[1..].starts_with('(') {
            let img = opens.pop().is_some_and(|o| out[..o].ends_with('!'));
            if let Some(end) = tail.find(')') {
                let inner = &tail[2..end];
                let (u, title) = match inner.split_once(' ') {
                    Some((u, t)) => (u, format!(" {t}")),
                    None => (inner, String::new()),
                };
                let bare = u.trim_matches(['<', '>']);
                if let Some(fixed) = fix(bare, img) {
                    out.push_str(&format!("](<{fixed}>{title})"));
                    rest = &tail[end + 1..];
                    continue;
                }
            }
            out.push(']');
            rest = &tail[1..];
            continue;
        }
        if c == b'"' {
            // `src="…"` / `href="…"` in raw HTML.
            let attr = ["src=", "href="].into_iter().find(|a| out.ends_with(a));
            if let Some(a) = attr
                && let Some(end) = tail[1..].find('"')
            {
                let v = &tail[1..1 + end];
                out.push('"');
                out.push_str(&fix(v, a == "src=").unwrap_or_else(|| v.to_string()));
                out.push('"');
                rest = &tail[end + 2..];
                continue;
            }
        }
        out.push(c as char);
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

/// `rel` resolved against the folder URL `base` (ending in `/`), with `./`
/// and `../` followed.
fn join(base: &str, rel: &str) -> String {
    let (scheme, path) = base.split_once("://").unwrap_or(("https", base));
    let mut parts: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    // host/owner/repo/blob/ref stay put; `/x` starts from the ref.
    let floor = 5.min(parts.len());
    let rel = match rel.strip_prefix('/') {
        Some(r) => {
            parts.truncate(floor);
            r
        }
        None => rel,
    };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.len() > floor {
                    parts.pop();
                }
            }
            s => parts.push(s),
        }
    }
    format!("{scheme}://{}", parts.join("/"))
}

/// `#123` issue and pull request numbers as links to them, outside code.
fn issue_links(md: &str, repo: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let mut code = false;
    let mut fence = false;
    for line in md.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            fence = !fence;
        }
        if fence || line.trim_start().starts_with("```") {
            out.push_str(line);
            continue;
        }
        let mut prev: Option<char> = None;
        let mut chars = line.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if c == '`' {
                code = !code;
            }
            let starts = !code
                && c == '#'
                && matches!(prev, None | Some(' ' | '(' | ',' | '\t'))
                && line[i + 1..].starts_with(|d: char| d.is_ascii_digit());
            if starts {
                let digits: String = line[i + 1..].chars().take_while(char::is_ascii_digit).collect();
                let next = line[i + 1 + digits.len()..].chars().next();
                if !next.is_some_and(|n| n.is_alphanumeric() || n == '_' || n == ']') {
                    out.push_str(&format!("[#{digits}](https://github.com/{repo}/issues/{digits})"));
                    for _ in 0..digits.len() {
                        chars.next();
                    }
                    prev = digits.chars().last();
                    continue;
                }
            }
            out.push(c);
            prev = Some(c);
        }
        code = false;
    }
    out
}

fn cache_dir() -> Option<PathBuf> {
    crate::paths::dirs().map(|d| d.cache_dir().join("changelog-links"))
}

/// GitHub's REST API, as JSON.
fn api(path: &str) -> anyhow::Result<serde_json::Value> {
    Ok(serde_json::from_str(&fetch(&format!("https://api.github.com/{path}"), API_TTL)?)?)
}

/// `url`'s text, from memory, from disk when younger than `ttl`, or
/// fetched. Versions linking the same file wait on one fetch. A stale copy
/// stands in when GitHub can't be reached or rate-limits.
fn fetch(url: &str, ttl: Duration) -> anyhow::Result<String> {
    type Slot = Arc<OnceLock<Result<String, String>>>;
    static MEM: OnceLock<Mutex<HashMap<String, Slot>>> = OnceLock::new();
    let slot = MEM
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(url.to_string())
        .or_default()
        .clone();
    let r = slot.get_or_init(|| fetch_uncached(url, ttl).map_err(|e| format!("{e:#}")));
    match r {
        Ok(t) => Ok(t.clone()),
        Err(e) => {
            // Let a later look try again.
            if let Some(m) = MEM.get() {
                m.lock().unwrap_or_else(|e| e.into_inner()).remove(url);
            }
            anyhow::bail!("{e}")
        }
    }
}

fn fetch_uncached(url: &str, ttl: Duration) -> anyhow::Result<String> {
    let path = cache_dir().map(|d| d.join(sha1_smol::Sha1::from(url).digest().to_string()));
    let cached = path.as_ref().and_then(|p| {
        let age = std::fs::metadata(p).ok()?.modified().ok()?.elapsed().unwrap_or_default();
        Some((std::fs::read_to_string(p).ok()?, age))
    });
    if let Some((t, age)) = &cached
        && *age < ttl
    {
        return Ok(t.clone());
    }
    let got = (|| -> anyhow::Result<String> {
        let mut req = cf::agent().get(url).header("User-Agent", "Hopper");
        if url.starts_with("https://api.github.com/") {
            req = req.header("Accept", "application/vnd.github+json");
        }
        Ok(req.call()?.body_mut().with_config().limit(4 << 20).read_to_string()?)
    })();
    match got {
        Ok(t) => {
            if let Some(p) = &path {
                let _ = std::fs::create_dir_all(p.parent().unwrap_or(p));
                let _ = std::fs::write(p, &t);
            }
            Ok(t)
        }
        Err(e) => match cached {
            Some((t, _)) => {
                if let Some(p) = &path {
                    // Not again until the TTL is up.
                    let _ = std::fs::File::options().append(true).open(p).and_then(|f| f.set_modified(SystemTime::now()));
                }
                Ok(t)
            }
            None => Err(e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(display: &str) -> cf::File {
        cf::File {
            display_name: display.into(),
            game_versions: vec!["1.21.1".into(), "NeoForge".into()],
            ..Default::default()
        }
    }

    #[test]
    fn targets() {
        assert_eq!(
            parse_target("https://github.com/Berry-Club/Excessive-Utilities/blob/master/CHANGELOG.md#1110"),
            Some(Target::File {
                repo: "Berry-Club/Excessive-Utilities".into(),
                git_ref: "master".into(),
                path: "CHANGELOG.md".into(),
                anchor: Some("1110".into()),
            })
        );
        assert_eq!(
            parse_target("https://raw.githubusercontent.com/a/b/refs/heads/main/docs/CHANGES.md"),
            Some(Target::File {
                repo: "a/b".into(),
                git_ref: "main".into(),
                path: "docs/CHANGES.md".into(),
                anchor: None,
            })
        );
        assert_eq!(
            parse_target("https://github.com/a/b/commit/abc123?diff=split"),
            Some(Target::Commit { repo: "a/b".into(), sha: "abc123".into() })
        );
        assert_eq!(
            parse_target("https://github.com/a/b/compare/v1.0...v1.1"),
            Some(Target::Compare { repo: "a/b".into(), range: "v1.0...v1.1".into() })
        );
        assert_eq!(
            parse_target("https://github.com/a/b/releases/tag/1.2.0"),
            Some(Target::Release { repo: "a/b".into(), tag: "1.2.0".into() })
        );
        assert_eq!(parse_target("https://github.com/a/b/pull/12/files"), Some(Target::Pull { repo: "a/b".into(), number: 12 }));
        assert_eq!(parse_target("https://github.com/a/b/commits/main"), None);
        assert_eq!(parse_target("https://gitlab.com/a/b/-/blob/main/CHANGELOG.md"), None);
    }

    #[test]
    fn only_a_link() {
        let b = richtext::parse("<p>https://github.com/a/b/commit/abc</p>");
        assert_eq!(link_only(&b).as_deref(), Some("https://github.com/a/b/commit/abc"));
        let b = richtext::parse(r#"<p>Changelog: <a href="https://github.com/a/b/commit/abc">here</a></p>"#);
        assert_eq!(link_only(&b).as_deref(), Some("https://github.com/a/b/commit/abc"));
        let b = richtext::parse("<ul><li>Fixed a crash when opening the menu with a full inventory</li><li>See https://github.com/a/b/issues/1</li></ul>");
        assert_eq!(link_only(&b), None);
    }

    const WIKI: &str = "# 5.27.0

Big release.

## Performance

- Faster surface

# 5.26.0

* Drops Fabric

# 5.24.1

Patch notes

# 5.24.0

Old
";

    fn wiki_pick(name: &str) -> Option<String> {
        let mut hints = Vec::new();
        for v in version_hints(&file(name)) {
            hints.extend(patch_fallbacks(&v));
        }
        section(WIKI, None, &hints)
    }

    #[test]
    fn wiki_links() {
        let w = |repo: &str, page: &str| Some(Target::Wiki { repo: repo.into(), page: page.into() });
        assert_eq!(parse_target("https://github.com/embeddedt/ModernFix/wiki/Changelog"), w("embeddedt/ModernFix", "Changelog"));
        assert_eq!(parse_target("https://github.com/embeddedt/ModernFix/wiki"), w("embeddedt/ModernFix", "Home"));
        assert_eq!(parse_target("https://github.com/a/b/wiki/"), w("a/b", "Home"));
        assert_eq!(parse_target("https://github.com/a/b/wiki/Changelog#x"), w("a/b", "Changelog"));
        assert_eq!(parse_target("https://github.com/a/b/wiki/_history"), None);
        // A link with generic text still counts.
        let b = richtext::parse(r#"<p>Please check the <a href="https://github.com/a/b/wiki/Changelog">GitHub wiki</a> for major changes.</p>"#);
        assert_eq!(link_only(&b).as_deref(), Some("https://github.com/a/b/wiki/Changelog"));
    }

    #[test]
    fn wiki_sections() {
        // Exact heading.
        assert_eq!(wiki_pick("modernfix-neoforge-5.26.0+mc1.21.1.jar").as_deref(), Some("* Drops Fabric"));
        // A patch falls back to its x.y.0.
        let s = wiki_pick("modernfix-neoforge-5.27.27+mc1.21.1.jar").unwrap();
        assert!(s.starts_with("Big release.") && s.contains("Faster surface") && !s.contains("Drops"));
        // The highest heading at or below the patch wins.
        assert_eq!(wiki_pick("modernfix-neoforge-5.24.3+mc1.21.1.jar").as_deref(), Some("Patch notes"));
        assert_eq!(wiki_pick("modernfix-neoforge-5.24.0+mc1.21.1.jar").as_deref(), Some("Old"));
        // No heading for that minor: nothing, not the page.
        assert_eq!(wiki_pick("modernfix-neoforge-5.25.2+mc1.21.1.jar"), None);
        assert_eq!(wiki_pick("modernfix-neoforge-6.0.1+mc1.21.1.jar"), None);
    }

    const LOG: &str = "# 1.11.0\n\n### Changed\n\n- New tag (#87)\n\n# 1.10.0\n\n### Fixed\n\n- Old fix\n\n```\n# 1.9.0 not a heading\n```\n";

    #[test]
    fn sections() {
        let s = section(LOG, Some("1110"), &[]).unwrap();
        assert_eq!(s, "### Changed\n\n- New tag (#87)");
        // A stale or odd anchor falls back to the version in the file name.
        let hints = version_hints(&file("Excessive Utilities 1.21.1 1.10.0-build.64"));
        assert_eq!(hints, ["1.10.0"]);
        assert!(section(LOG, Some("L12"), &hints).unwrap().contains("Old fix"));
        assert_eq!(section(LOG, None, &["1.9.0".into()]), None);
        assert!(has_version("[1.11.0] - 2026-10-18", "1.11.0"));
        assert!(!has_version("1.11.0.1", "1.11.0"));
        assert!(!has_version("11.11.0", "1.11.0"));
        assert_eq!(slug(&heading_text("[1.2.0](https://x) - 2024-01-02")), "120---2024-01-02");
    }

    #[test]
    fn tag_messages() {
        assert_eq!(tag_text("Changelog\n- Fixed a crash\n- Added x").as_deref(), Some("- Fixed a crash\n- Added x"));
        assert_eq!(tag_text("v1.2\n\n- Fix").as_deref(), Some("v1.2\n\n- Fix"));
        assert_eq!(
            tag_text("- Fix\n-----BEGIN PGP SIGNATURE-----\nabc\n-----END PGP SIGNATURE-----\n").as_deref(),
            Some("- Fix")
        );
        assert_eq!(tag_text("  \n"), None);
    }

    #[test]
    fn links() {
        assert_eq!(
            issue_links("- Fix (#87), see #9 and `#12`, not #abc or a#1", "a/b"),
            "- Fix ([#87](https://github.com/a/b/issues/87)), see [#9](https://github.com/a/b/issues/9) and `#12`, not #abc or a#1"
        );
        let f = "https://github.com/a/b/blob/main/docs/CHANGES.md";
        let page = "https://github.com/a/b/blob/main/docs/";
        let raw = "https://raw.githubusercontent.com/a/b/main/docs/";
        assert_eq!(
            rebase_links("[x](../README.md) ![i](img/a.png) [y](https://z.io) [t](#top)", f, page, raw),
            "[x](<https://github.com/a/b/blob/main/README.md>) ![i](<https://raw.githubusercontent.com/a/b/main/docs/img/a.png>) [y](https://z.io) [t](<https://github.com/a/b/blob/main/docs/CHANGES.md#top>)"
        );
        assert_eq!(
            rebase_links(r#"<img src="shot.png">"#, f, page, raw),
            r#"<img src="https://raw.githubusercontent.com/a/b/main/docs/shot.png">"#
        );
    }

    #[test]
    #[ignore = "network"]
    fn live() {
        for (url, name) in [
            ("https://github.com/Berry-Club/Excessive-Utilities/blob/master/CHANGELOG.md#1110", "Excessive Utilities 1.21.1 1.11.0-build.69"),
            ("https://github.com/Berry-Club/Excessive-Utilities/blob/master/CHANGELOG.md", "Excessive Utilities 1.21.1 1.10.0-build.64"),
            ("https://github.com/FabricMC/fabric-loader/releases/tag/0.16.10", "x"),
            ("https://github.com/VazkiiMods/Morph-o-Tool/releases/tag/release-1.21.1-1.8-40", "x"),
            ("https://github.com/FabricMC/fabric-loader/compare/0.16.9...0.16.10", "x"),
            ("https://github.com/FabricMC/fabric-loader/commit/a1f9c1c", "x"),
        ] {
            let b = richtext::parse(&format!("<p>{url}</p>"));
            let out = expand(b, &file(name));
            println!("==== {url}
{}
", richtext::to_markdown(&out));
            assert!(out.len() > 1, "{url}");
        }
    }
}
