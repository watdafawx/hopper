//! Mods from outside CurseForge and Modrinth: GitHub releases, Maven
//! repositories, Jenkins jobs and plain download links.
//!
//! A mod added from one of these remembers where it came from
//! ([`ModSource`], kept on its [`Addon`]), so the normal update check can
//! look there again. These files skip the two sites' review, so the player
//! confirms each source once ([`trusted`]), every file's sha256 is kept,
//! and a file that changes under the same release name is flagged instead
//! of installed. Nothing in a download is ever run: a jar is only opened
//! as a zip to read its mod id, Minecraft and loader.
//!
//! Such a mod gets a project id of its own ([`src_id`], bit 62 set and bit
//! 63 clear, so it never reaches CurseForge or Modrinth), and
//! [`crate::source`] answers for it from what this module knows.

pub mod carry;
pub mod github;
pub mod http;
pub mod jar;
pub mod jenkins;
pub mod looks;
pub mod maven;
pub mod page;
pub mod url;

use crate::cf::{self, Loader};
use crate::install::Update;
use crate::instances::{Addon, Instance};
use crate::jobs::Job;
use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

/// Where a source lives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    GitHub,
    Maven,
    Jenkins,
    Url,
}

impl Kind {
    /// The chip on the mod row.
    pub fn label(self) -> &'static str {
        match self {
            Kind::GitHub => "GitHub",
            Kind::Maven => "Maven",
            Kind::Jenkins => "Jenkins",
            Kind::Url => "Link",
        }
    }
}

/// Where a mod's file comes from, as its [`Addon`] keeps it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModSource {
    pub kind: Kind,
    /// GitHub: "owner/repo". Maven: the artifact's folder URL. Jenkins: the
    /// job's URL. Url: the download link itself.
    pub url: String,
    /// The installed file's name with its numbers folded ([`pattern_of`]):
    /// updates must have the same shape, so a repo that ships a Fabric and
    /// a NeoForge jar keeps giving the same one.
    pub pattern: String,
    /// Offer pre-releases too (off unless the player ticks it).
    pub prerelease: bool,
    /// The release tag, Maven version, Jenkins build or the link's ETag /
    /// Last-Modified the installed file came from.
    pub seen: String,
    /// sha256 of the installed file (lower-case hex).
    pub sha256: String,
    /// Who publishes it, as the player confirmed it ("owner/repo",
    /// a Maven host, a Jenkins host).
    pub publisher: String,
    /// What the mod was before it followed this source (a CurseForge or
    /// Modrinth project), so "Stop following" can put it back.
    #[serde(skip_serializing_if = "is_zero")]
    pub prev_project: u64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub prev_mr_project: String,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl ModSource {
    /// The web page to open for the source.
    pub fn page(&self) -> String {
        match self.kind {
            Kind::GitHub => format!("https://github.com/{}", self.url),
            _ => self.url.clone(),
        }
    }

    /// The key the trust list and the project id go by.
    pub fn key(&self) -> String {
        format!("{}:{}", self.kind.label().to_ascii_lowercase(), self.url.to_ascii_lowercase())
    }
}

/// One file a source offers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Candidate {
    pub file_name: String,
    pub url: String,
    /// Release tag, Maven version, Jenkins build ("#123") or ETag.
    pub version: String,
    /// The release's title, when it has one.
    pub title: String,
    /// ISO 8601, when known.
    pub date: String,
    pub size: u64,
    /// Release notes (Markdown).
    pub notes: String,
    /// Who made the release (GitHub user), when known.
    pub author: String,
    pub prerelease: bool,
    /// sha256 the source states for the file (lower-case hex), when it does.
    pub digest: String,
    /// The release's page.
    pub page: String,
    /// How the file fits the instance, from its name and release.
    pub fit: Fit,
}

/// How a file fits an instance, judged from its name (and release tag)
/// before download. The jar itself is checked after ([`jar::check`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "why", rename_all = "lowercase")]
pub enum Fit {
    /// Names the instance's Minecraft and loader (or no loader).
    Good,
    /// Names nothing that rules it in or out.
    #[default]
    Unknown,
    /// Another loader ("Fabric").
    Loader(String),
    /// Another Minecraft version ("1.20.1").
    Minecraft(String),
    /// Source code, docs or an API jar, not the mod.
    Extra(String),
}

impl Fit {
    pub fn usable(&self) -> bool {
        matches!(self, Fit::Good | Fit::Unknown)
    }

    /// A short tag for the picker ("Fits", "Fabric", "1.20.1", "Source code").
    pub fn tag(&self) -> String {
        match self {
            Fit::Good => "Fits".into(),
            Fit::Unknown => "Not sure".into(),
            Fit::Loader(l) | Fit::Minecraft(l) | Fit::Extra(l) => l.clone(),
        }
    }
}

/// What a pasted link resolves to.
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    /// The source, `seen`/`sha256` empty until a file is installed.
    pub source: ModSource,
    /// A name for the mod (repo or artifact name).
    pub name: String,
    /// Files of the newest release that has any for this instance, best
    /// first; files that don't fit follow, for the picker.
    pub candidates: Vec<Candidate>,
    /// Index of the file Hopper would pick; `None` when nothing fits.
    pub pick: Option<usize>,
    /// Several files fit equally well: the player should choose.
    pub ambiguous: bool,
}

/// Mods from links are allowed on this PC (the app's setting). Off, their
/// update checks don't run; the records stay.
static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Percent-encodes a URL path segment.
pub(crate) fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Bit 62 marks a source's project id; Modrinth's have bit 63.
const SRC_BIT: u64 = 1 << 62;

/// A project id for a source-tracked mod.
pub fn src_id(key: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    (h & !(1 << 63)) | SRC_BIT
}

/// The project id is a source-tracked mod's.
pub fn is_src(id: u64) -> bool {
    id >> 62 == 1
}

/// A stable file id for a candidate (non-zero, fits in 62 bits).
fn file_id(c: &Candidate) -> u64 {
    src_id(&format!("{}|{}|{}", c.version, c.file_name, c.digest)) & !SRC_BIT | 1
}

/// Known sources by project id, for [`crate::source`]'s lookups.
static KNOWN: Mutex<Option<HashMap<u64, (ModSource, String)>>> = Mutex::new(None);
/// The last files listed per project, for [`file`].
static FILES: Mutex<Option<HashMap<u64, Vec<cf::File>>>> = Mutex::new(None);

/// Tells the lookups about a source-tracked mod (done on instance load).
pub fn remember(id: u64, src: &ModSource, name: &str) {
    KNOWN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(id, (src.clone(), name.to_string()));
}

fn known(id: u64) -> Option<(ModSource, String)> {
    KNOWN.lock().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(&id).cloned()
}

/// The source of a source-tracked mod Hopper has seen, with its name.
pub fn known_source(id: u64) -> Option<(ModSource, String)> {
    known(id)
}

/// Icon and authors by project id, as the lists show them ([`looks::fill`]).
static LOOKS: Mutex<Option<HashMap<u64, (String, String)>>> = Mutex::new(None);

/// Tells the peek a source-tracked mod's icon and authors.
pub fn remember_looks(id: u64, icon: &str, authors: &str) {
    LOOKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(id, (icon.to_string(), authors.to_string()));
}

/// The project as the peek and the install code see it.
pub fn get_mod(id: u64) -> anyhow::Result<cf::Mod> {
    let (src, name) = known(id).context("unknown source")?;
    Ok(mod_for(id, &src, &name))
}

fn mod_for(id: u64, src: &ModSource, name: &str) -> cf::Mod {
    let (icon, authors) = LOOKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(&id).cloned())
        .unwrap_or_default();
    // The jar's authors when it names them; the repo shows beside them.
    let author = if authors.is_empty() { src.publisher.clone() } else { authors };
    let icon = if icon.is_empty() && src.kind == Kind::GitHub { looks::avatar(&src.url).unwrap_or_default() } else { icon };
    cf::Mod {
        id,
        name: name.to_string(),
        summary: format!("From {}: {}", src.kind.label(), src.publisher),
        class_id: Some(cf::CLASS_MODS),
        authors: vec![cf::Author { name: author, ..Default::default() }],
        links: cf::Links { website_url: src.page(), ..Default::default() },
        logo: (!icon.is_empty()).then(|| cf::Logo { thumbnail_url: icon.clone(), url: icon }),
        ..Default::default()
    }
}

/// Files the source offers for this Minecraft and loader, newest first
/// (for the Versions list).
pub fn files(id: u64, game_version: &str, loader: Loader) -> anyhow::Result<Vec<cf::File>> {
    let (src, _) = known(id).context("unknown source")?;
    let list: Vec<cf::File> = list_all(&src, game_version, loader)?
        .into_iter()
        .filter(|c| c.fit.usable() && matches_pattern(&src.pattern, &c.file_name))
        .map(|c| to_file(id, &src, &c, game_version, loader, false))
        .collect();
    FILES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(id, list.clone());
    Ok(list)
}

/// One file from the last [`files`] listing.
pub fn file(project: u64, file: u64) -> anyhow::Result<cf::File> {
    FILES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(&project))
        .and_then(|l| l.iter().find(|f| f.id == file))
        .cloned()
        .context("that file is no longer listed; open Versions again")
}

/// What a source file carries through the update and install code.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SrcFile {
    /// The mod's source as it will be once this file is in.
    pub source: ModSource,
    pub notes: String,
    /// sha256 the source states, checked after download.
    pub digest: String,
    /// The release's page.
    pub page: String,
    /// The installed release's file changed (same name, other contents):
    /// shown, but not installed by Update all.
    pub changed: bool,
    /// What the jar must fit, checked after download.
    pub want_mc: String,
    pub want_loader: String,
}

fn to_file(id: u64, src: &ModSource, c: &Candidate, mc: &str, loader: Loader, changed: bool) -> cf::File {
    let mut next = src.clone();
    next.seen = c.version.clone();
    next.sha256 = c.digest.clone();
    cf::File {
        id: file_id(c),
        mod_id: id,
        display_name: if c.title.is_empty() { c.version.clone() } else { c.title.clone() },
        file_name: c.file_name.clone(),
        release_type: if c.prerelease { 2 } else { 1 },
        file_date: c.date.clone(),
        file_length: c.size,
        download_url: Some(c.url.clone()),
        source: Some(Box::new(SrcFile {
            source: next,
            notes: c.notes.clone(),
            digest: c.digest.clone(),
            page: c.page.clone(),
            changed,
            want_mc: mc.to_string(),
            want_loader: loader_key(loader).to_string(),
        })),
        ..Default::default()
    }
}

fn loader_key(l: Loader) -> &'static str {
    crate::modrinth::loader_name(l).unwrap_or("")
}

/// Reads a pasted link. Errors say what to do instead (a Modrinth page
/// goes through Browse).
pub fn parse_link(link: &str) -> anyhow::Result<ModSource> {
    let link = link.trim().trim_end_matches('/');
    if link.is_empty() {
        bail!("Paste a link first");
    }
    let lower = link.to_ascii_lowercase();
    if lower.contains("modrinth.com/") || lower.contains("curseforge.com/") {
        bail!("That's a Modrinth or CurseForge page. Find the mod in Browse instead; Hopper updates those already.");
    }
    if let Some(s) = github::parse_link(link) {
        return Ok(s);
    }
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        bail!("Paste a whole link, starting with https://");
    }
    if lower.contains("/job/") {
        return Ok(jenkins::parse_link(link));
    }
    if let Some(s) = maven::parse_link(link) {
        return Ok(s);
    }
    Ok(url::parse_link(link))
}

/// Every file the source offers, newest release first, each with its fit.
fn list_all(src: &ModSource, mc: &str, loader: Loader) -> anyhow::Result<Vec<Candidate>> {
    let mut all = match src.kind {
        Kind::GitHub => github::candidates(src)?,
        Kind::Maven => maven::candidates(src)?,
        Kind::Jenkins => jenkins::candidates(src)?,
        Kind::Url => url::candidates(src)?,
    };
    all.retain(|c| src.prerelease || !c.prerelease || src.kind == Kind::Url);
    for c in &mut all {
        c.fit = fit(&c.file_name, &c.version, mc, loader);
    }
    Ok(all)
}

/// Most jars to download per lookup just to read what they're for.
const MAX_PROBES: usize = 6;

/// Many repos name neither Minecraft nor the loader in their files
/// (ModMenu's are "modmenu-20.0.3.jar" for every version). Going down the
/// list, files with the source's shape that their names can't place are
/// downloaded and read ([`jar::probe`]) until one fits; the ones that
/// don't are marked. Stops at the first file that fits.
pub fn settle(all: &mut [Candidate], pattern: &str, mc: &str, loader: Loader) {
    let mut probes = 0;
    for c in all.iter_mut() {
        if !c.fit.usable() || !matches_pattern(pattern, &c.file_name) {
            continue;
        }
        if c.fit == Fit::Good {
            return;
        }
        if probes == MAX_PROBES {
            return;
        }
        probes += 1;
        match jar::probe(&c.url, &c.digest, c.size) {
            Ok(info) => match jar::misfit(&info, mc, loader_key(loader)) {
                None => {
                    if !info.loaders.is_empty() || !info.minecraft.is_empty() {
                        c.fit = Fit::Good;
                    }
                    return;
                }
                Some(_) if !info.minecraft.is_empty() && jar::accepts(&info.minecraft, mc) == Some(false) => {
                    c.fit = Fit::Minecraft(jar::range_label(&info.minecraft));
                }
                Some(_) => c.fit = Fit::Loader(jar::pretty(info.loaders.first().copied().unwrap_or("")).to_string()),
            },
            // Can't download it now: leave it to the check after install.
            Err(e) => {
                log::warn!("reading {}: {e:#}", c.file_name);
                return;
            }
        }
    }
}

/// Looks up a pasted link and picks the file for this instance.
pub fn resolve(link: &str, mc: &str, loader: Loader, prerelease: bool) -> anyhow::Result<Resolved> {
    let mut src = parse_link(link)?;
    src.prerelease = prerelease;
    let tag = github::tag_in_link(link);
    let all = list_all(&src, mc, loader)?;
    // A link to one release starts there; otherwise the newest that fits.
    let mut all: Vec<Candidate> = match tag {
        Some(t) if all.iter().any(|c| c.version == t) => {
            all.into_iter().filter(|c| c.version == t).collect()
        }
        // Older than the recent list: ask for that release alone.
        Some(t) if src.kind == Kind::GitHub => {
            let mut one = github::release_by_tag(&src, &t)?;
            for c in &mut one {
                c.fit = fit(&c.file_name, &c.version, mc, loader);
            }
            one
        }
        _ => all,
    };
    let asset = github::asset_in_link(link);
    if asset.is_none() {
        settle(&mut all, "", mc, loader);
    }
    let mut r = pick(src, all, asset.as_deref());
    if r.source.publisher.is_empty() {
        r.source.publisher = publisher_of(&r.source);
    }
    Ok(r)
}

fn publisher_of(src: &ModSource) -> String {
    match src.kind {
        Kind::GitHub => src.url.clone(),
        _ => host_of(&src.url),
    }
}

/// The host part of a URL ("maven.example.com").
pub fn host_of(u: &str) -> String {
    u.split("://").nth(1).unwrap_or(u).split('/').next().unwrap_or("").to_string()
}

/// Picks the best file: the newest release with a usable one, its best
/// fit. `want` is an asset name the link pointed at.
pub fn pick(src: ModSource, all: Vec<Candidate>, want: Option<&str>) -> Resolved {
    let want = want.filter(|w| all.iter().any(|c| c.file_name == *w));
    let name = name_of(&src);
    // The newest release (version) that has a usable file.
    let release = match want {
        Some(w) => all.iter().find(|c| c.file_name == w).map(|c| c.version.clone()),
        None => all.iter().find(|c| c.fit.usable() && c.file_name.ends_with(".jar")).map(|c| c.version.clone()),
    };
    let Some(release) = release else {
        // Nothing fits: list the newest release's files for the picker.
        let first = all.first().map(|c| c.version.clone()).unwrap_or_default();
        let candidates: Vec<Candidate> = all.into_iter().filter(|c| c.version == first).collect();
        return Resolved { source: src, name, candidates, pick: None, ambiguous: false };
    };
    let mut candidates: Vec<Candidate> = all.into_iter().filter(|c| c.version == release).collect();
    candidates.sort_by_key(|c| std::cmp::Reverse(score(c)));
    let pick = match want {
        Some(w) => candidates.iter().position(|c| c.file_name == w),
        None => candidates.first().filter(|c| c.fit.usable()).map(|_| 0),
    };
    let ambiguous = want.is_none()
        && candidates.len() > 1
        && candidates[1].fit.usable()
        && score(&candidates[0]) == score(&candidates[1]);
    let mut r = Resolved { source: src, name, candidates, pick, ambiguous };
    if let Some(i) = r.pick {
        r.source.pattern = pattern_of(&r.candidates[i].file_name);
    }
    r
}

/// A name to show for the mod before its jar says one.
fn name_of(src: &ModSource) -> String {
    let last = src.url.trim_end_matches('/').rsplit('/').next().unwrap_or(&src.url);
    match src.kind {
        Kind::Url => last.trim_end_matches(".jar").to_string(),
        Kind::Jenkins => last.to_string(),
        _ => last.to_string(),
    }
}

fn score(c: &Candidate) -> u32 {
    let base = match c.fit {
        Fit::Good => 30,
        // The right loader by name beats a name that says nothing.
        Fit::Unknown if !loaders_named(&c.file_name.to_ascii_lowercase()).is_empty() => 25,
        Fit::Unknown => 20,
        _ => 0,
    };
    // A plain jar beats a zip; shorter names (no "-all", "-shadow") a bit.
    base + u32::from(c.file_name.ends_with(".jar")) * 5
}

/// Judges a file from its name and release tag.
pub fn fit(file_name: &str, version: &str, mc: &str, loader: Loader) -> Fit {
    let name = file_name.to_ascii_lowercase();
    if !(name.ends_with(".jar")) {
        return Fit::Extra("Not a jar".into());
    }
    let stem = name.trim_end_matches(".jar");
    for (suffix, what) in [
        ("sources", "Source code"),
        ("javadoc", "Docs"),
        ("dev", "Dev build"),
        ("api", "API only"),
        ("slim", "Dev build"),
        ("dev-shadow", "Dev build"),
    ] {
        if stem.ends_with(&format!("-{suffix}")) || stem.ends_with(&format!("_{suffix}")) {
            return Fit::Extra(what.into());
        }
    }
    let text = format!("{} {}", name, version.to_ascii_lowercase());
    // Loader named in the file name only: a tag like "fabric-1.21" names
    // one of several jars.
    let named = match loaders_named(&name) {
        // "neoforge/v19.2.18": a repo that tags each loader's release.
        n if n.is_empty() => loaders_named(&version.to_ascii_lowercase()),
        n => n,
    };
    let want = loader_key(loader);
    let loader_ok = named.is_empty()
        || want.is_empty()
        || named.contains(&want)
        || (want == "quilt" && named.contains(&"fabric"));
    if !loader_ok {
        let l = named[0];
        let mut s = l.to_string();
        s[..1].make_ascii_uppercase();
        if l == "neoforge" {
            s = "NeoForge".into();
        }
        return Fit::Loader(s);
    }
    let mcs = mc_versions(&text);
    if !mcs.is_empty() && !mc.is_empty() {
        let hit = mcs.iter().any(|v| mc_matches(v, mc));
        if !hit {
            return Fit::Minecraft(mcs[0].clone());
        }
        return Fit::Good;
    }
    // Minecraft unsaid: the jar settles it ([`settle`]).
    Fit::Unknown
}

/// Loader names a file name carries ("neoforge", "forge", "fabric", "quilt").
fn loaders_named(name: &str) -> Vec<&'static str> {
    let words: Vec<&str> = name.split(|c: char| !c.is_ascii_alphanumeric()).collect();
    let mut out = Vec::new();
    for w in words {
        let l = match w {
            "neoforge" | "neo" => "neoforge",
            "forge" | "lexforge" => "forge",
            "fabric" => "fabric",
            "quilt" => "quilt",
            _ => continue,
        };
        if !out.contains(&l) {
            out.push(l);
        }
    }
    out
}

/// Minecraft versions in a text: "1.21.1", "mc1.21", "1.20.x". Mod
/// versions look the same ("1.2.0"), so only 1.7 and later count, and
/// "mc" in front makes it certain.
pub fn mc_versions(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let at_word = i == 0 || !b[i - 1].is_ascii_digit() && b[i - 1] != b'.';
        // Since 2026 Minecraft counts by year: "26.1", "26.3.1".
        let year = at_word
            && b.len() > i + 2
            && b[i].is_ascii_digit()
            && b[i + 1].is_ascii_digit()
            && b[i + 2] == b'.'
            && text[i..i + 2].parse::<u32>().is_ok_and(|y| (26..=40).contains(&y));
        if year {
            let rest = &text[i..];
            let end = rest
                .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == 'x'))
                .unwrap_or(rest.len());
            let v = rest[..end].trim_end_matches('.');
            let parts: Vec<&str> = v.split('.').collect();
            let ok = (parts.len() == 2 || parts.len() == 3)
                && parts[1].parse::<u32>().is_ok_and(|n| (1..=12).contains(&n))
                && parts.get(2).is_none_or(|p| *p == "x" || p.parse::<u32>().is_ok_and(|n| n < 20));
            if ok && !out.contains(&v.to_string()) {
                out.push(v.to_string());
            }
            i += end.max(1);
            continue;
        }
        if at_word && text[i..].starts_with("1.") {
            let rest = &text[i..];
            let end = rest
                .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == 'x'))
                .unwrap_or(rest.len());
            let v = rest[..end].trim_end_matches('.');
            let parts: Vec<&str> = v.split('.').collect();
            let minor = parts.get(1).and_then(|p| p.parse::<u32>().ok());
            let ok_len = parts.len() == 2 || parts.len() == 3;
            let ok_patch = parts.get(2).is_none_or(|p| *p == "x" || p.parse::<u32>().is_ok_and(|n| n < 20));
            if ok_len && ok_patch && minor.is_some_and(|m| (7..=40).contains(&m)) {
                let v = v.to_string();
                if !out.contains(&v) {
                    out.push(v);
                }
            }
            i += end.max(1);
            continue;
        }
        i += 1;
    }
    out
}

/// "1.21" or "1.21.x" covers 1.21.1; "1.21.1" only itself.
fn mc_matches(named: &str, mc: &str) -> bool {
    if named == mc {
        return true;
    }
    let base = named.trim_end_matches(".x");
    let parts = base.split('.').count();
    if named.ends_with(".x") || parts == 2 {
        return mc == base || mc.starts_with(&format!("{base}."));
    }
    false
}

/// A file name with every run of digits and dots folded to `#`:
/// "sodium-neoforge-0.6.13+mc1.21.1.jar" gives "sodium-neoforge-#+mc#.jar".
pub fn pattern_of(file_name: &str) -> String {
    let mut out = String::new();
    let mut in_num = false;
    for ch in file_name.to_ascii_lowercase().chars() {
        if ch.is_ascii_digit() || (in_num && ch == '.') {
            if !in_num {
                out.push('#');
                in_num = true;
            }
        } else {
            in_num = false;
            out.push(ch);
        }
    }
    // "1.2.jar": the run swallowed the dot before "jar".
    let mut out = out.replace("#jar", "#.jar");
    // "-alpha.#", "-beta#", "-rc.#": a pre-release of the same file.
    for word in ["alpha", "beta", "rc", "pre", "snapshot"] {
        for form in [format!("-{word}.#"), format!("-{word}#"), format!("-{word}"), format!("+{word}.#"), format!(".{word}.#")] {
            out = out.replace(&form, "");
        }
    }
    out
}

/// A file has the same shape as the installed one. An empty pattern
/// (none recorded) takes any file.
pub fn matches_pattern(pattern: &str, file_name: &str) -> bool {
    pattern.is_empty() || pattern_of(file_name) == pattern
}

/// The file an update check offers for one mod: the newest usable file
/// with the installed file's shape.
fn newest_for(src: &ModSource, all: &[Candidate]) -> Option<Candidate> {
    all.iter()
        .find(|c| c.fit.usable() && matches_pattern(&src.pattern, &c.file_name))
        .cloned()
}

/// New files for the instance's source-tracked mods. Mods sharing a
/// source ask it once; GitHub answers come from the cache when unchanged.
pub fn updates(inst: &Instance) -> anyhow::Result<Vec<Update>> {
    let loader = crate::install::loader_of(inst);
    if !enabled() {
        return Ok(Vec::new());
    }
    // A source that came with a pack or a peer isn't checked until the
    // player confirms it on this PC.
    let mods: Vec<&Addon> = inst
        .addons
        .iter()
        .filter(|a| !a.locked && a.source.as_ref().is_some_and(trusted))
        .collect();
    let mut by_source: HashMap<String, anyhow::Result<Vec<Candidate>>> = HashMap::new();
    let mut out = Vec::new();
    let mut first_err = None;
    for a in mods {
        let Some(src) = a.source.as_ref() else { continue };
        let key = format!("{}|{}", src.key(), src.prerelease);
        let all = by_source.entry(key).or_insert_with(|| list_all(src, &inst.game_version, loader));
        let all = match all {
            Ok(all) => {
                settle(all, &src.pattern, &inst.game_version, loader);
                &*all
            }
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(anyhow::anyhow!("{}: {e:#}", a.name));
                }
                continue;
            }
        };
        if let Some(u) = offer(inst, a, src, all, loader) {
            out.push(u);
        }
    }
    match first_err {
        Some(e) if out.is_empty() => Err(e),
        Some(e) => {
            log::warn!("source updates for {}: {e:#}", inst.name);
            Ok(out)
        }
        None => Ok(out),
    }
}

/// The update for one mod, if any: a newer release, or the same release
/// whose file changed (flagged, not installed by Update all).
fn offer(inst: &Instance, a: &Addon, src: &ModSource, all: &[Candidate], loader: Loader) -> Option<Update> {
    let c = newest_for(src, all)?;
    let same_release = c.version == src.seen;
    // Only strictly newer releases are updates: a jar adopted at 1.4.2
    // isn't offered 1.4.1 because that's what the list has for it.
    if !same_release && src.kind != Kind::Url && !src.seen.is_empty() && !newer(&c.version, &src.seen) {
        return None;
    }
    let changed = same_release
        && c.file_name.eq_ignore_ascii_case(&a.file_name)
        && !c.digest.is_empty()
        && !src.sha256.is_empty()
        && !c.digest.eq_ignore_ascii_case(&src.sha256);
    if same_release && !changed {
        return None;
    }
    // A link's ETag can change for a re-upload of the very same file.
    if src.kind == Kind::Url && c.version.is_empty() {
        return None;
    }
    if a.locked_version == c.file_name && !changed {
        return None;
    }
    Some(Update {
        project_id: a.project_id,
        name: a.name.clone(),
        from: a.file_name.clone(),
        file: to_file(a.project_id, src, &c, &inst.game_version, loader, changed),
    })
}

/// The version a tag or build stands for: "neoforge/v1.4.2" and "v1.4.2"
/// give "1.4.2", "mc1.21.1-0.6.13" gives "0.6.13", "#512" gives "512".
pub fn tag_version(tag: &str) -> String {
    let t = tag.rsplit('/').next().unwrap_or(tag).trim_start_matches('#');
    // "mc1.21.1-0.6.13": the part after the Minecraft version.
    let t = match t.strip_prefix("mc").and_then(|r| r.split_once('-')) {
        Some((_, rest)) => rest,
        None => t,
    };
    let t = t.trim_start_matches(|c: char| c == 'v' || c == 'V');
    // "1.4.2+neoforge-1.21.1": build metadata isn't the version.
    t.split('+').next().unwrap_or(t).to_string()
}

/// `a` is a later release than `b`.
pub fn newer(a: &str, b: &str) -> bool {
    crate::scan::compare_versions(&tag_version(a), &tag_version(b)) == std::cmp::Ordering::Greater
}

/// How an installed jar was matched to a source when adopting it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Matched {
    /// Byte for byte the asset of this release.
    Exact { tag: String },
    /// The jar says it's `version`; the nearest release at or before it is
    /// `tag` (empty when the source has none that old).
    Baseline { version: String, tag: String },
    /// Nothing to go by: the newest release counts as newer.
    Unknown,
}

/// Following a source with the jar already in place: nothing downloads,
/// the file stays as it is, and only later releases are offered.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Adoption {
    /// The source as the mod will keep it (`seen`, `sha256`, `pattern` set).
    pub source: ModSource,
    pub matched: Matched,
    /// The newest release's file that would be offered as an update.
    pub newest: Option<Candidate>,
    /// A name for the mod (the jar's own, else the repo's).
    pub name: String,
}

/// Works out how the installed jar `file_name` relates to the source at
/// `link`: by its sha256 among the release assets, else by the version
/// inside the jar. Reads only the source's listing (cached) and the jar.
pub fn plan_adopt(inst: &Instance, file_name: &str, link: &str, prerelease: bool) -> anyhow::Result<Adoption> {
    let path = jar_path(inst, file_name).context("that jar is no longer in the mods folder")?;
    let sha = sha256_file(&path)?;
    let info = jar::read(&path).unwrap_or_default();
    let mut src = parse_link(link)?;
    src.prerelease = prerelease;
    if src.publisher.is_empty() {
        src.publisher = publisher_of(&src);
    }
    let loader = crate::install::loader_of(inst);
    // Every listed file, pre-releases too: the jar may be one.
    let mut every = src.clone();
    every.prerelease = true;
    let all = list_all(&every, &inst.game_version, loader)?;
    let version = if info.version.is_empty() { crate::scan::version_from_file_name(file_name) } else { info.version.clone() };
    let (mut matched, pattern) = match_jar(&all, &sha, file_name, &version);
    // Older than the recent list: ask GitHub for the release named after
    // the jar's version, and match its file by hash.
    if src.kind == Kind::GitHub
        && let Matched::Baseline { version: v, tag } = &matched
        && tag_version(tag) != *v
    {
        for t in [format!("v{v}"), v.clone()] {
            if let Ok(one) = github::release_by_tag(&src, &t) {
                matched = if one.iter().any(|c| c.digest.eq_ignore_ascii_case(&sha)) {
                    Matched::Exact { tag: t }
                } else {
                    Matched::Baseline { version: v.clone(), tag: t }
                };
                break;
            }
        }
    }
    src.pattern = pattern;
    src.sha256 = sha;
    src.seen = match &matched {
        Matched::Exact { tag } => tag.clone(),
        Matched::Baseline { version, tag } if tag.is_empty() => format!("v{version}"),
        Matched::Baseline { tag, .. } => tag.clone(),
        Matched::Unknown => String::new(),
    };
    let mut usable: Vec<Candidate> = all.into_iter().filter(|c| prerelease || !c.prerelease).collect();
    settle(&mut usable, &src.pattern, &inst.game_version, loader);
    let newest = newest_for(&src, &usable).filter(|c| src.seen.is_empty() || newer(&c.version, &src.seen));
    let name = if info.name.is_empty() { name_of(&src) } else { info.name.clone() };
    Ok(Adoption { source: src, matched, newest, name })
}

/// The release a jar is: the asset with its sha256, else the release with
/// its version, else the nearest earlier one. Returns the match and the
/// file-name pattern updates keep to.
pub fn match_jar(all: &[Candidate], sha: &str, file_name: &str, version: &str) -> (Matched, String) {
    if let Some(c) = all.iter().find(|c| !c.digest.is_empty() && c.digest.eq_ignore_ascii_case(sha)) {
        return (Matched::Exact { tag: c.version.clone() }, pattern_of(&c.file_name));
    }
    // The jar's own name shape when the releases use it, else the shape of
    // the releases' main jar.
    let mine = pattern_of(file_name);
    let pattern = if all.iter().any(|c| pattern_of(&c.file_name) == mine) {
        mine
    } else {
        all.iter()
            .filter(|c| !matches!(c.fit, Fit::Extra(_)))
            .find(|c| c.fit.usable())
            .or_else(|| all.iter().find(|c| !matches!(c.fit, Fit::Extra(_))))
            .map(|c| pattern_of(&c.file_name))
            .unwrap_or(mine)
    };
    if version.is_empty() {
        return (Matched::Unknown, pattern);
    }
    let v = tag_version(version);
    let mut best: Option<&Candidate> = None;
    for c in all {
        let o = crate::scan::compare_versions(&tag_version(&c.version), &v);
        if o == std::cmp::Ordering::Greater {
            continue;
        }
        if best.is_none_or(|b| newer(&c.version, &b.version)) {
            best = Some(c);
        }
    }
    let tag = best.map(|c| c.version.clone()).unwrap_or_default();
    (Matched::Baseline { version: v, tag }, pattern)
}

fn jar_path(inst: &Instance, file_name: &str) -> Option<std::path::PathBuf> {
    let mods = inst.mods_dir();
    [mods.join(file_name), mods.join(format!("{file_name}.disabled"))].into_iter().find(|p| p.is_file())
}

/// Makes the installed jar follow the source as planned, leaving the file
/// alone. The mod keeps its history.
pub fn adopt(inst: &mut Instance, file_name: &str, plan: &Adoption) -> anyhow::Result<()> {
    let i = match inst.addons.iter().position(|a| a.file_name.eq_ignore_ascii_case(file_name)) {
        Some(i) => i,
        None => {
            let path = jar_path(inst, file_name).context("that jar is no longer in the mods folder")?;
            inst.addons.push(Addon {
                file_name: file_name.to_string(),
                enabled: !path.to_string_lossy().ends_with(".disabled"),
                class_id: cf::CLASS_MODS,
                date_added: crate::time::now_iso(),
                ..Default::default()
            });
            inst.addons.len() - 1
        }
    };
    let id = src_id(&format!("{}|{}", plan.source.key(), plan.source.pattern));
    let a = &mut inst.addons[i];
    let mut src = plan.source.clone();
    let (prev, prev_mr) = match &a.source {
        Some(old) => (old.prev_project, old.prev_mr_project.clone()),
        None => (a.project_id, a.mr_project.clone()),
    };
    src.prev_project = prev;
    src.prev_mr_project = prev_mr;
    a.project_id = id;
    a.mr_project.clear();
    a.mr_version.clear();
    a.mr_sha1.clear();
    if a.name.is_empty() {
        a.name = plan.name.clone();
    }
    a.page = src.page();
    if a.author.is_empty() {
        a.author = src.publisher.clone();
    }
    a.source = Some(src.clone());
    remember(id, &src, &a.name.clone());
    inst.save()
}

/// A jar CurseForge and Modrinth don't know, with the GitHub repo its own
/// metadata links to (if any).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Unknown {
    pub file_name: String,
    pub name: String,
    pub link: Option<String>,
}

/// The instance's unknown mods (no project, no source), each with the
/// source its jar suggests. Reads only the jars.
pub fn unknown_mods(inst: &Instance) -> Vec<Unknown> {
    let mut files: Vec<String> = inst
        .addons
        .iter()
        .filter(|a| a.project_id == 0 && a.source.is_none() && a.folder() == "mods")
        .map(|a| a.file_name.clone())
        .collect();
    for f in crate::install::loose_jars(inst) {
        let f = f.trim_end_matches(".disabled").to_string();
        if !files.iter().any(|x| x.eq_ignore_ascii_case(&f)) {
            files.push(f);
        }
    }
    files
        .into_iter()
        .filter_map(|f| {
            let path = jar_path(inst, &f)?;
            let info = jar::read(&path).unwrap_or_default();
            let name = if info.name.is_empty() { crate::scan::strip_version_suffix(&f) } else { info.name.clone() };
            Some(Unknown { link: jar::suggested_link(&info), file_name: f, name })
        })
        .collect()
}

/// The link a jar's own metadata suggests, for "Follow a source…".
pub fn suggest(inst: &Instance, file_name: &str) -> Option<String> {
    let path = jar_path(inst, file_name)?;
    jar::suggested_link(&jar::read(&path).ok()?)
}

/// Checks a downloaded source file: the stated sha256, then the jar's own
/// metadata against the Minecraft and loader it is for. Returns its sha256.
pub fn verify_download(path: &std::path::Path, f: &SrcFile) -> anyhow::Result<String> {
    let sha = sha256_file(path)?;
    if !f.digest.is_empty() && !f.digest.eq_ignore_ascii_case(&sha) {
        bail!("the download doesn't match the checksum the source lists, so Hopper didn't keep it");
    }
    let info = jar::read(path)?;
    if let Some(why) = jar::misfit(&info, &f.want_mc, &f.want_loader) {
        bail!("{why}");
    }
    Ok(sha)
}

pub fn sha256_file(path: &std::path::Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut h = Sha256::new();
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Sources the player said yes to once, by [`ModSource::key`].
fn trust_file() -> Option<std::path::PathBuf> {
    crate::paths::dirs().map(|d| d.data_dir().join("trusted-sources.json"))
}

/// The trust list, read once and kept current by [`trust`] (rows ask
/// for every mod on every frame).
static TRUSTED: Mutex<Option<Vec<String>>> = Mutex::new(None);

fn trust_list() -> Vec<String> {
    TRUSTED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(|| {
            trust_file()
                .and_then(|f| std::fs::read(f).ok())
                .and_then(|b| serde_json::from_slice::<Vec<String>>(&b).ok())
                .unwrap_or_default()
        })
        .clone()
}

/// The player already confirmed this source.
pub fn trusted(src: &ModSource) -> bool {
    trust_list().contains(&src.key())
}

/// Remembers that the player confirmed this source.
pub fn trust(src: &ModSource) -> anyhow::Result<()> {
    let mut l = trust_list();
    if l.contains(&src.key()) {
        return Ok(());
    }
    l.push(src.key());
    if let Some(f) = trust_file() {
        if let Some(p) = f.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(f, serde_json::to_vec_pretty(&l)?)?;
    }
    *TRUSTED.lock().unwrap_or_else(|e| e.into_inner()) = Some(l);
    Ok(())
}

/// The project and file for candidate `idx` of `r`, ready for the install
/// code. `id` keeps an existing mod's id (following a source keeps it as
/// the same entry).
fn project_and_file(inst: &Instance, r: &Resolved, idx: usize, id: u64, name: &str) -> anyhow::Result<(cf::Mod, cf::File)> {
    let c = r.candidates.get(idx).context("no such file")?;
    let mut src = r.source.clone();
    src.pattern = pattern_of(&c.file_name);
    remember(id, &src, name);
    let loader = crate::install::loader_of(inst);
    Ok((mod_for(id, &src, name), to_file(id, &src, c, &inst.game_version, loader, false)))
}

/// Installs file `idx` of a resolved link as a new mod. Returns its file name.
pub fn install(inst: &mut Instance, r: &Resolved, idx: usize, job: &Job) -> anyhow::Result<String> {
    let c = r.candidates.get(idx).context("no such file")?;
    let mut key_src = r.source.clone();
    key_src.pattern = pattern_of(&c.file_name);
    let id = src_id(&format!("{}|{}", key_src.key(), key_src.pattern));
    let (m, f) = project_and_file(inst, r, idx, id, &r.name)?;
    crate::install::put_source_file(inst, &m, &f, job)?;
    inst.save()?;
    Ok(f.file_name)
}

/// Makes an installed mod (`file_name`) follow a source and installs file
/// `idx` of it in place of the jar (nothing is downloaded when the jar is
/// that file already). The mod keeps its history; "Stop following" puts
/// its old project back.
pub fn follow(inst: &mut Instance, file_name: &str, r: &Resolved, idx: usize, job: &Job) -> anyhow::Result<String> {
    let c = r.candidates.get(idx).context("no such file")?.clone();
    let i = match inst.addons.iter().position(|a| a.file_name.eq_ignore_ascii_case(file_name)) {
        Some(i) => i,
        None => {
            // A jar dropped in by hand that Hopper hasn't recorded yet.
            let mods = inst.mods_dir();
            let on = mods.join(file_name).is_file();
            anyhow::ensure!(
                on || mods.join(format!("{file_name}.disabled")).is_file(),
                "that mod is no longer in the instance"
            );
            inst.addons.push(Addon {
                file_name: file_name.to_string(),
                name: r.name.clone(),
                enabled: on,
                class_id: cf::CLASS_MODS,
                date_added: crate::time::now_iso(),
                ..Default::default()
            });
            inst.addons.len() - 1
        }
    };
    let mut key_src = r.source.clone();
    key_src.pattern = pattern_of(&c.file_name);
    let id = src_id(&format!("{}|{}", key_src.key(), key_src.pattern));
    {
        let a = &mut inst.addons[i];
        let mut src = key_src.clone();
        // Following again (another source) keeps the first project to go back to.
        let (prev, prev_mr) = match &a.source {
            Some(old) => (old.prev_project, old.prev_mr_project.clone()),
            None => (a.project_id, a.mr_project.clone()),
        };
        src.prev_project = prev;
        src.prev_mr_project = prev_mr;
        a.project_id = id;
        a.mr_project.clear();
        a.mr_version.clear();
        a.mr_sha1.clear();
        a.source = Some(src);
    }
    let name = inst.addons[i].name.clone();
    let mut r2 = r.clone();
    r2.source = inst.addons[i].source.clone().unwrap_or_default();
    let (m, f) = project_and_file(inst, &r2, idx, id, &name)?;
    if c.file_name.eq_ignore_ascii_case(&inst.addons[i].file_name) {
        // The jar is that file already: just note what it is.
        let path = inst.addon_path(&inst.addons[i]);
        let sha = sha256_file(&path)?;
        if !c.digest.is_empty() && !c.digest.eq_ignore_ascii_case(&sha) {
            bail!("your {} differs from the one in {}; pick another file or update from there", c.file_name, c.version);
        }
        let a = &mut inst.addons[i];
        if let Some(s) = a.source.as_mut() {
            s.seen = c.version.clone();
            s.sha256 = sha;
        }
        a.file_id = f.id;
        a.page = r2.source.page();
        inst.save()?;
        return Ok(c.file_name);
    }
    crate::install::put_source_file(inst, &m, &f, job)?;
    inst.save()?;
    Ok(f.file_name)
}

/// Stops a mod following its source: it goes back to the project it was
/// (CurseForge or Modrinth, matched again by the next check) or to a
/// plain jar.
pub fn unfollow(inst: &mut Instance, file_name: &str) -> anyhow::Result<()> {
    let a = inst
        .addons
        .iter_mut()
        .find(|a| a.file_name.eq_ignore_ascii_case(file_name))
        .context("that mod is no longer in the instance")?;
    let Some(src) = a.source.take() else { return Ok(()) };
    a.project_id = src.prev_project;
    a.mr_project = src.prev_mr_project;
    a.file_id = 0;
    a.page = String::new();
    inst.save()
}

/// The pinned file of a source (its release `seen`, named `file_name`).
pub fn pinned_candidate(src: &ModSource, file_name: &str) -> anyhow::Result<Candidate> {
    let mut list = match src.kind {
        Kind::GitHub => github::release_by_tag(src, &src.seen)?,
        Kind::Maven => vec![Candidate {
            url: format!("{}/{}/{file_name}", src.url, src.seen),
            file_name: file_name.to_string(),
            version: src.seen.clone(),
            ..Default::default()
        }],
        Kind::Jenkins => {
            let n = src.seen.trim_start_matches('#');
            let url = format!("{}/{n}/api/json?tree=number,timestamp,url,artifacts[fileName,relativePath],changeSet[items[msg]]", src.url);
            jenkins::from_build(&http::get_text(&url, false)?)?
        }
        Kind::Url => url::candidates(src)?,
    };
    list.retain(|c| c.file_name.eq_ignore_ascii_case(file_name));
    let mut c = list.pop().with_context(|| format!("{file_name} is no longer at {}", src.page()))?;
    // The pin's hash is what the download must be.
    if !src.sha256.is_empty() {
        c.digest = src.sha256.clone();
    }
    c.fit = Fit::Unknown;
    Ok(c)
}

/// Installs a pinned file (from a share code or pack) as is: the exact
/// release and sha256 it names. The source isn't trusted by this; its
/// updates wait for the player.
pub fn install_pinned(inst: &mut Instance, pin: &carry::Pin, enabled_on: bool, job: &Job) -> anyhow::Result<String> {
    let c = pinned_candidate(&pin.source, &pin.file)?;
    let id = src_id(&format!("{}|{}", pin.source.key(), pin.source.pattern));
    let name = crate::scan::strip_version_suffix(&pin.file);
    remember(id, &pin.source, &name);
    let loader = crate::install::loader_of(inst);
    let f = to_file(id, &pin.source, &c, &inst.game_version, loader, false);
    crate::install::put_source_file(inst, &mod_for(id, &pin.source, &name), &f, job)?;
    if !enabled_on
        && let Some(i) = inst.addons.iter().position(|a| a.project_id == id)
    {
        let _ = crate::install::set_enabled(inst, i, false);
    }
    inst.save()?;
    Ok(f.file_name)
}

/// Settings: the GitHub token, from the system's credential store.
pub const GITHUB_TOKEN: &str = "github_token";

#[cfg(test)]
mod tests;
