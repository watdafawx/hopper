//! What a downloaded jar says about itself: its mod id, loader and the
//! Minecraft versions it accepts. Only read as a zip; nothing in it runs.

use std::io::{Read, Seek};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JarInfo {
    pub mod_id: String,
    pub name: String,
    /// Loaders the metadata files are for: "fabric", "neoforge", "forge".
    pub loaders: Vec<&'static str>,
    /// The Minecraft range it declares ("[1.21.1,1.22)", ">=1.21 <1.21.2"),
    /// empty when it doesn't.
    pub minecraft: String,
    /// The mod's own version ("1.4.2"), empty when the file only has a
    /// build placeholder ("${file.jarVersion}").
    pub version: String,
    /// Web links the mod lists (home page, sources, issue tracker).
    pub links: Vec<String>,
}

pub fn read(path: &std::path::Path) -> anyhow::Result<JarInfo> {
    let f = std::fs::File::open(path)?;
    read_from(f).map_err(|e| anyhow::anyhow!("{} isn't a mod jar Hopper can read ({e})", path.file_name().unwrap_or_default().to_string_lossy()))
}

pub fn read_from<R: Read + Seek>(r: R) -> anyhow::Result<JarInfo> {
    let mut zip = zip::ZipArchive::new(r)?;
    let mut info = JarInfo::default();
    let mut text = |name: &str| -> Option<String> {
        let mut f = zip.by_name(name).ok()?;
        let mut s = String::new();
        f.by_ref().take(512 * 1024).read_to_string(&mut s).ok()?;
        Some(s)
    };
    if let Some(t) = text("META-INF/neoforge.mods.toml") {
        info.loaders.push("neoforge");
        read_toml(&t, &mut info);
    }
    if let Some(t) = text("META-INF/mods.toml") {
        // NeoForge before 20.5 used Forge's file name.
        info.loaders.push("forge");
        if info.mod_id.is_empty() {
            read_toml(&t, &mut info);
        }
    }
    for fabric in ["fabric.mod.json", "quilt.mod.json"] {
        if let Some(t) = text(fabric) {
            info.loaders.push("fabric");
            if info.mod_id.is_empty() {
                read_fabric(&t, &mut info);
            }
        }
    }
    Ok(info)
}

fn real(v: Option<&str>) -> String {
    v.map(str::trim).filter(|s| !s.is_empty() && !s.contains("${")).unwrap_or("").to_string()
}

fn read_toml(t: &str, info: &mut JarInfo) {
    let Ok(v) = t.parse::<toml::Table>() else { return };
    for key in ["issueTrackerURL", "displayURL"] {
        let l = real(v.get(key).and_then(|x| x.as_str()));
        if !l.is_empty() {
            info.links.push(l);
        }
    }
    if let Some(m) = v.get("mods").and_then(|m| m.as_array()).and_then(|a| a.first()) {
        info.mod_id = m.get("modId").and_then(|x| x.as_str()).unwrap_or("").to_string();
        info.name = m.get("displayName").and_then(|x| x.as_str()).unwrap_or("").to_string();
        info.version = real(m.get("version").and_then(|x| x.as_str()));
        for key in ["displayURL", "issueTrackerURL", "updateJSONURL"] {
            let l = real(m.get(key).and_then(|x| x.as_str()));
            if !l.is_empty() {
                info.links.push(l);
            }
        }
    }
    // [[dependencies.<modid>]] modId = "minecraft", versionRange = "[1.21.1,)"
    if let Some(deps) = v.get("dependencies").and_then(|d| d.as_table()) {
        for list in deps.values().filter_map(|l| l.as_array()) {
            for d in list {
                if d.get("modId").and_then(|x| x.as_str()) == Some("minecraft")
                    && let Some(r) = d.get("versionRange").and_then(|x| x.as_str())
                {
                    info.minecraft = r.to_string();
                    return;
                }
            }
        }
    }
}

fn read_fabric(t: &str, info: &mut JarInfo) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(t) else { return };
    // quilt.mod.json nests it under "quilt_loader".
    let v = v.get("quilt_loader").cloned().unwrap_or(v);
    info.mod_id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
    info.name = v.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
    info.version = real(v.get("version").and_then(|x| x.as_str()));
    // "contact": { "sources", "homepage", "issues" } (Quilt: "metadata.contact").
    let contact = v.get("contact").or_else(|| v.get("metadata").and_then(|m| m.get("contact")));
    if let Some(c) = contact {
        for key in ["sources", "homepage", "issues"] {
            let l = real(c.get(key).and_then(|x| x.as_str()));
            if !l.is_empty() {
                info.links.push(l);
            }
        }
    }
    info.minecraft = match v.get("depends").and_then(|d| d.get("minecraft")) {
        Some(serde_json::Value::String(s)) => s.clone(),
        // Any of several ranges.
        Some(serde_json::Value::Array(a)) => a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" || "),
        _ => String::new(),
    };
}

/// The GitHub repo ("owner/repo") a link points into: the repo, its
/// issues, a release, raw files ("raw.githubusercontent.com/owner/repo")
/// or a GitHub Pages site doesn't count (no repo in it).
pub fn github_repo(link: &str) -> Option<String> {
    let l = link.trim().trim_start_matches("https://").trim_start_matches("http://").trim_start_matches("www.");
    let rest = l.strip_prefix("github.com/").or_else(|| l.strip_prefix("raw.githubusercontent.com/"))?;
    let mut parts = rest.split(['/', '?', '#']).filter(|p| !p.is_empty());
    let (owner, repo) = (parts.next()?, parts.next()?.trim_end_matches(".git"));
    // Not a repo: github.com/orgs/x, /sponsors/x, /settings.
    if ["orgs", "sponsors", "settings", "marketplace", "topics"].contains(&owner) {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

/// The GitHub repo the jar's own links point to, as a link to paste.
pub fn suggested_link(info: &JarInfo) -> Option<String> {
    info.links.iter().find_map(|l| github_repo(l)).map(|r| format!("https://github.com/{r}"))
}

/// Why the jar doesn't fit (`None` when it does or can't tell).
pub fn misfit(info: &JarInfo, mc: &str, loader: &str) -> Option<String> {
    if !loader.is_empty() && !info.loaders.is_empty() {
        let ok = info.loaders.iter().any(|l| {
            *l == loader
                || (loader == "quilt" && *l == "fabric")
                // Old NeoForge jars carry mods.toml only.
                || (loader == "neoforge" && *l == "forge" && mc_before(mc, "1.20.5"))
        });
        if !ok {
            return Some(format!("This file is a {} mod; the instance uses {}", pretty(info.loaders[0]), pretty(loader)));
        }
    }
    if !mc.is_empty() && !info.minecraft.is_empty() && accepts(&info.minecraft, mc) == Some(false) {
        return Some(format!("This file is for Minecraft {}, not {mc}", info.minecraft));
    }
    None
}

pub fn pretty(l: &str) -> &str {
    match l {
        "neoforge" => "NeoForge",
        "forge" => "Forge",
        "fabric" => "Fabric",
        "quilt" => "Quilt",
        o => o,
    }
}

/// A short label for a Minecraft range ("[1.21.9,)" → "1.21.9+").
pub fn range_label(r: &str) -> String {
    let r = r.trim();
    let first = r
        .trim_start_matches(['[', '(', '>', '=', '~', '^'])
        .split([',', ' ', ')', ']'])
        .next()
        .unwrap_or(r);
    if first.is_empty() { r.to_string() } else { format!("MC {first}") }
}

/// What a jar at `url` is for, downloading it once to a temp file. The
/// answer is kept on disk by URL (and checksum), so a release is read
/// once whatever checks follow.
pub fn probe(url: &str, digest: &str, size: u64) -> anyhow::Result<JarInfo> {
    use std::collections::HashMap;
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Seen {
        loaders: Vec<String>,
        minecraft: String,
        mod_id: String,
    }
    let key = format!("{url}|{digest}");
    let file = crate::paths::dirs().map(|d| d.cache_dir().join("source-jars.json"));
    let mut cache: HashMap<String, Seen> = file
        .as_ref()
        .and_then(|f| std::fs::read(f).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    if let Some(s) = cache.get(&key) {
        return Ok(JarInfo {
            mod_id: s.mod_id.clone(),
            loaders: s.loaders.iter().filter_map(|l| loader_static(l)).collect(),
            minecraft: s.minecraft.clone(),
            ..Default::default()
        });
    }
    let dir = std::env::temp_dir().join("hopper-probe");
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!("{:x}.jar", super::src_id(&key)));
    let expect = crate::cf::Expect { len: size, sha1: String::new() };
    let r = crate::cf::download_checked(url, &tmp, &expect, None, |_| {}).and_then(|_| read(&tmp));
    let _ = std::fs::remove_file(&tmp);
    let info = r?;
    cache.insert(
        key,
        Seen {
            loaders: info.loaders.iter().map(|l| l.to_string()).collect(),
            minecraft: info.minecraft.clone(),
            mod_id: info.mod_id.clone(),
        },
    );
    if let Some(f) = file
        && let Ok(b) = serde_json::to_vec(&cache)
    {
        let _ = std::fs::create_dir_all(f.parent().unwrap_or(&f));
        let _ = std::fs::write(f, b);
    }
    Ok(info)
}

fn loader_static(l: &str) -> Option<&'static str> {
    match l {
        "fabric" => Some("fabric"),
        "neoforge" => Some("neoforge"),
        "forge" => Some("forge"),
        _ => None,
    }
}

fn mc_before(mc: &str, v: &str) -> bool {
    crate::scan::compare_versions(mc, v) == std::cmp::Ordering::Less
}

/// Whether `range` accepts `mc`. Reads Maven ranges ("[1.21,1.22)") and
/// Fabric's ("~1.21.1", ">=1.21 <1.22", "1.21.x", "*"); `None` when it
/// can't tell.
pub fn accepts(range: &str, mc: &str) -> Option<bool> {
    let range = range.trim();
    if range.is_empty() || range == "*" {
        return Some(true);
    }
    if range.contains("||") {
        let mut any_known = false;
        for part in range.split("||") {
            match accepts(part, mc) {
                Some(true) => return Some(true),
                Some(false) => any_known = true,
                None => {}
            }
        }
        return any_known.then_some(false);
    }
    if range.starts_with('[') || range.starts_with('(') {
        return maven_range(range, mc);
    }
    let mut known = false;
    for p in range.split_whitespace() {
        match predicate(p, mc) {
            Some(false) => return Some(false),
            Some(true) => known = true,
            None => {}
        }
    }
    known.then_some(true)
}

fn cmp(a: &str, b: &str) -> std::cmp::Ordering {
    crate::scan::compare_versions(a, b)
}

fn maven_range(r: &str, mc: &str) -> Option<bool> {
    use std::cmp::Ordering::*;
    // Several ranges: "[1.20,1.20.1],[1.21,)".
    let mut out = None;
    let mut rest = r;
    while !rest.is_empty() {
        let end = rest.find([']', ')'])?;
        let one = &rest[..=end];
        rest = rest[end + 1..].trim_start_matches(',').trim();
        let open = one.as_bytes()[0];
        let close = one.as_bytes()[one.len() - 1];
        let inner = &one[1..one.len() - 1];
        let ok = match inner.split_once(',') {
            None => cmp(mc, inner.trim()) == Equal,
            Some((lo, hi)) => {
                let (lo, hi) = (lo.trim(), hi.trim());
                let lo_ok = lo.is_empty() || matches!((cmp(mc, lo), open), (Greater, _) | (Equal, b'['));
                let hi_ok = hi.is_empty() || matches!((cmp(mc, hi), close), (Less, _) | (Equal, b']'));
                lo_ok && hi_ok
            }
        };
        if ok {
            return Some(true);
        }
        out = Some(false);
    }
    out
}

fn predicate(p: &str, mc: &str) -> Option<bool> {
    use std::cmp::Ordering::*;
    let (op, v) = [">=", "<=", ">", "<", "=", "~", "^"]
        .iter()
        .find_map(|op| p.strip_prefix(op).map(|v| (*op, v)))
        .unwrap_or(("", p));
    let v = v.trim();
    if v.is_empty() || !v.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    // Snapshots and pre-releases in a range ("1.21-alpha.24.10.a"): judge
    // by the release they lead to.
    let v = v.split(['-', '+']).next().unwrap_or(v);
    if v.ends_with(".x") || v.ends_with(".*") {
        let base = &v[..v.len() - 2];
        return Some(mc == base || mc.starts_with(&format!("{base}.")));
    }
    let o = cmp(mc, v);
    Some(match op {
        ">=" => o != Less,
        "<=" => o != Greater,
        ">" => o == Greater,
        "<" => o == Less,
        "=" | "" => o == Equal,
        // Same minor line, at least v.
        "~" => {
            let line: Vec<&str> = v.split('.').take(2).collect();
            o != Less && mc.split('.').take(2).eq(line.iter().copied())
        }
        // Same major, at least v.
        "^" => o != Less && mc.split('.').next() == v.split('.').next(),
        _ => return None,
    })
}
