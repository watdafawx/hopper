//! Per-file diff of config-style folders (config/, defaultconfigs/, kubejs/,
//! …) between a pack and its server, so only the files you choose get
//! pushed. Ported from mod-sync's `configdiff.go`, `linediff.js` and
//! `configfields.js`.
//!
//! A big pack's config/ holds thousands of files on a slow share, so the
//! default compare is by size only (an edit nearly always changes it); the
//! content check is opt-in, parallel and hash-cached.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// Folders offered for review, when either side has them.
pub const FOLDERS: &[&str] = &[
    "config",
    "defaultconfigs",
    "kubejs",
    "global_packs",
    "openloader",
    "paxi",
    "scripts",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileStatus {
    OnlyClient,
    OnlyServer,
    Modified,
}

/// Whether a differing file matters to the server, once [`classify`] has
/// looked at it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Settings or a file the server may use.
    #[default]
    Needed,
    /// Client settings or client resources: a server never reads them.
    ClientSide,
    /// Written by a mod for itself (caches, lists, benchmarks).
    Generated,
    /// Both sides hold the same settings; only comments, blank lines or
    /// indentation differ.
    FormatOnly,
}

#[derive(Clone, Debug)]
pub struct FileDiff {
    pub rel: String,
    pub status: FileStatus,
    pub client_size: u64,
    pub server_size: u64,
    pub kind: Kind,
    /// How many settings differ, for a changed file whose settings could be
    /// read (see [`classify`]).
    pub settings: Option<usize>,
    /// Not text: no preview, no settings.
    pub binary: bool,
}

impl FileDiff {
    pub fn new(rel: String, status: FileStatus, client_size: u64, server_size: u64) -> Self {
        Self {
            rel,
            status,
            client_size,
            server_size,
            kind: Kind::Needed,
            settings: None,
            binary: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ConfigDiff {
    pub folder: String,
    pub files: Vec<FileDiff>,
    /// Files on both sides that matched (by size, or by content when `deep`).
    pub same: usize,
    pub deep: bool,
    pub client_files: usize,
    pub server_files: usize,
    /// The files that matched, sorted.
    pub same_files: Vec<String>,
    /// Whether [`classify`] has run.
    pub classified: bool,
}

/// Files that aren't hand-edited config: backups, logs, locks, indexes.
pub fn is_noise(rel: &str) -> bool {
    let l = rel.to_lowercase();
    if l.contains(".bak") || l.contains("/search_index/") || l.contains("/logs/") {
        return true;
    }
    [
        ".old",
        ".tmp",
        ".temp",
        ".log",
        ".log.gz",
        ".lck",
        ".lock",
        ".crc",
        ".ldb",
        ".dat_old",
        ".ds_store",
        "thumbs.db",
        ".hopper-tmp",
    ]
    .iter()
    .any(|s| l.ends_with(s))
}

/// Client-only content inside folders that otherwise go to the server, so
/// server sync never offers it: Paxi's resource packs (Paxi's data packs do
/// belong on the server). `rel` is relative to the instance, with `/`
/// (`config/paxi/resourcepacks/x.zip`, or `paxi/resourcepacks/...`).
pub fn client_only(rel: &str) -> bool {
    let l = rel.to_ascii_lowercase();
    let parts: Vec<&str> = l.split('/').collect();
    // Something inside the folder, not the folder itself.
    let dirs = &parts[..parts.len() - 1];
    dirs.windows(2).any(|w| w == ["paxi", "resourcepacks"])
}

#[derive(Clone, Copy)]
struct Stat {
    size: u64,
    mtime: u64,
}

fn walk(root: &Path) -> HashMap<String, Stat> {
    let mut out = HashMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(p);
                continue;
            }
            let Ok(rel) = p.strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if is_noise(&rel) {
                continue;
            }
            let Ok(md) = e.metadata() else { continue };
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            out.insert(
                rel,
                Stat {
                    size: md.len(),
                    mtime,
                },
            );
        }
    }
    out
}

/// The server disk is slow; re-walking it on every rescan is wasteful.
type Walk = HashMap<String, Stat>;
static WALKS: LazyLock<Mutex<HashMap<PathBuf, (Instant, Walk)>>> = LazyLock::new(Default::default);
const WALK_TTL: Duration = Duration::from_secs(90);

fn walk_cached(root: &Path) -> HashMap<String, Stat> {
    if let Some((at, m)) = WALKS.lock().unwrap().get(root)
        && at.elapsed() < WALK_TTL
    {
        return m.clone();
    }
    // A server on a Hopper Server walks its own disk.
    if let Some(r) = crate::hopperd::locate(root) {
        let m: Walk = r
            .and_then(|(c, id, rel)| c.walk(&id, &rel))
            .inspect_err(|e| log::warn!("config walk: {e:#}"))
            .unwrap_or_default()
            .into_iter()
            .filter(|(rel, _, _)| !is_noise(rel))
            .map(|(rel, size, mtime)| (rel, Stat { size, mtime }))
            .collect();
        WALKS.lock().unwrap().insert(root.to_path_buf(), (Instant::now(), m.clone()));
        return m;
    }
    let m = walk(root);
    WALKS
        .lock()
        .unwrap()
        .insert(root.to_path_buf(), (Instant::now(), m.clone()));
    m
}

/// Every file under `root` as (path inside it, size, modified time): what
/// Hopper Server sends for its side of a compare.
pub fn walk_stats(root: &Path) -> Vec<(String, u64, u64)> {
    walk(root).into_iter().map(|(rel, s)| (rel, s.size, s.mtime)).collect()
}

/// A file's content hash, as the compare uses (cached).
pub fn file_hash(path: &Path) -> Option<String> {
    let md = fs::metadata(path).ok()?;
    let mtime = md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
    let h = hash(path, Stat { size: md.len(), mtime });
    save_hashes();
    h
}

pub fn forget_walk(root: &Path) {
    WALKS.lock().unwrap().remove(root);
}

pub fn forget_all() {
    WALKS.lock().unwrap().clear();
}

// ── content hashes, cached by path + size + mtime ──────────────────

const HASH_CAP: u64 = 32 << 20;

#[derive(Default, Serialize, Deserialize)]
struct HashCache(HashMap<String, String>);

static HASHES: LazyLock<Mutex<HashCache>> = LazyLock::new(|| {
    Mutex::new(
        hash_cache_path()
            .and_then(|p| fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default(),
    )
});

fn hash_cache_path() -> Option<PathBuf> {
    crate::paths::dirs().map(|d| d.cache_dir().join("cfghash.json"))
}

fn save_hashes() {
    let Some(p) = hash_cache_path() else { return };
    let b = serde_json::to_vec(&*HASHES.lock().unwrap()).unwrap_or_default();
    if let Some(d) = p.parent() {
        let _ = fs::create_dir_all(d);
    }
    let _ = fs::write(p, b);
}

fn hash(path: &Path, st: Stat) -> Option<String> {
    let key = format!("{}|{}|{}", path.display(), st.size, st.mtime);
    if let Some(h) = HASHES.lock().unwrap().0.get(&key) {
        return Some(h.clone());
    }
    if st.size > HASH_CAP {
        return None;
    }
    let data = fs::read(path).ok()?;
    let h = sha1_smol::Sha1::from(&data).hexdigest();
    HASHES.lock().unwrap().0.insert(key, h.clone());
    Some(h)
}

fn rank(s: FileStatus) -> u8 {
    match s {
        FileStatus::Modified => 0,
        FileStatus::OnlyClient => 1,
        FileStatus::OnlyServer => 2,
    }
}

/// Compares `folder` under both roots. `progress(done, total)` reports the
/// content check.
pub fn compare(
    client_root: &Path,
    server_root: &Path,
    folder: &str,
    deep: bool,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> ConfigDiff {
    let (c_root, s_root) = (client_root.join(folder), server_root.join(folder));
    let (c, s) = std::thread::scope(|sc| {
        let a = sc.spawn(|| walk_cached(&c_root));
        let b = walk_cached(&s_root);
        (a.join().unwrap_or_default(), b)
    });
    let keep = |rel: &String| !client_only(&format!("{folder}/{rel}"));
    let c: Walk = c.into_iter().filter(|(rel, _)| keep(rel)).collect();
    let s: Walk = s.into_iter().filter(|(rel, _)| keep(rel)).collect();
    let mut files = Vec::new();
    let mut equal = Vec::new();
    for (rel, cs) in &c {
        match s.get(rel) {
            None => files.push(FileDiff::new(rel.clone(), FileStatus::OnlyClient, cs.size, 0)),
            Some(ss) if ss.size != cs.size => {
                files.push(FileDiff::new(rel.clone(), FileStatus::Modified, cs.size, ss.size))
            }
            Some(ss) => equal.push((rel.clone(), *cs, *ss)),
        }
    }
    for (rel, ss) in &s {
        if !c.contains_key(rel) {
            files.push(FileDiff::new(rel.clone(), FileStatus::OnlyServer, 0, ss.size));
        }
    }
    let mut same = equal.len();
    // The server's hashes, from the panel in one request when it's remote.
    let remote_hashes: Option<HashMap<String, String>> = match crate::hopperd::locate(&s_root) {
        Some(r) if deep && !equal.is_empty() => {
            let rels: Vec<String> = equal.iter().map(|(rel, _, _)| format!("{folder}/{rel}")).collect();
            let h = r.and_then(|(c, id, _)| c.hashes(&id, &rels)).unwrap_or_default();
            Some(h.into_iter().map(|(k, v)| (k.strip_prefix(&format!("{folder}/")).unwrap_or(&k).to_string(), v)).collect())
        }
        _ => None,
    };
    if deep && !equal.is_empty() {
        let done = std::sync::atomic::AtomicUsize::new(0);
        let changed = Mutex::new(Vec::new());
        let (chunk, total) = (equal.len().div_ceil(12), equal.len());
        std::thread::scope(|sc| {
            for part in equal.chunks(chunk) {
                let (done, changed, c_root, s_root, rh) = (&done, &changed, &c_root, &s_root, &remote_hashes);
                sc.spawn(move || {
                    for (rel, cs, ss) in part {
                        let a = hash(&c_root.join(rel), *cs);
                        let b = match rh {
                            Some(h) => h.get(rel).cloned(),
                            None => hash(&s_root.join(rel), *ss),
                        };
                        if matches!((&a, &b), (Some(a), Some(b)) if a != b) {
                            changed
                                .lock()
                                .unwrap()
                                .push(FileDiff::new(rel.clone(), FileStatus::Modified, cs.size, ss.size));
                        }
                        let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        if n % 50 == 0 {
                            progress(n, total);
                        }
                    }
                });
            }
        });
        let changed = changed.into_inner().unwrap();
        same -= changed.len();
        files.extend(changed);
        save_hashes();
    }
    let differs: HashSet<&str> = files.iter().map(|f| f.rel.as_str()).collect();
    let mut same_files: Vec<String> = equal
        .iter()
        .map(|(rel, _, _)| rel.clone())
        .filter(|r| !differs.contains(r.as_str()))
        .collect();
    same_files.sort();
    files.sort_by(|a, b| {
        rank(a.status)
            .cmp(&rank(b.status))
            .then_with(|| a.rel.cmp(&b.rel))
    });
    ConfigDiff {
        folder: folder.to_string(),
        files,
        same,
        deep,
        client_files: c.len(),
        server_files: s.len(),
        same_files,
        classified: false,
    }
}

// ── which differences matter to a server ───────────────────────────

/// Config files and folders only a client reads (shaders, minimaps, JEI,
/// HUD and menu mods), matched on the path's first part.
const CLIENT_TOPS: &[&str] = &[
    "jei",
    "emi",
    "iris",
    "oculus",
    "sodium",
    "embeddium",
    "xaerominimap",
    "xaeroworldmap",
    "fancymenu",
    "drippyloadingscreen",
    "loadingbackgrounds",
    "betterf3",
    "entityculling",
    "controlling",
    "mousetweaks",
    "resourcepacks",
    "shaderpacks",
];

/// Client settings or client resources: `rel` is inside the compared
/// folder (`ars_nouveau-client.toml`, `Mekanism/client.toml`). A name that
/// also says server or common isn't, nor is anything in defaultconfigs
/// (that folder seeds per-world server configs).
pub fn client_side(folder: &str, rel: &str) -> bool {
    if folder == "defaultconfigs" {
        return false;
    }
    if client_only(&format!("{folder}/{rel}")) {
        return true;
    }
    let l = rel.to_ascii_lowercase();
    let words: Vec<&str> = l.split(['/', '-', '_', '.', ' ']).filter(|w| !w.is_empty()).collect();
    if words.iter().any(|w| matches!(*w, "server" | "common" | "startup" | "world")) {
        return false;
    }
    if words.iter().any(|w| matches!(*w, "client" | "clientside" | "clientconfig" | "hud" | "keybinds")) {
        return true;
    }
    let top = l.split('/').next().unwrap_or_default();
    let stem = top.split(['.', '-', '_']).next().unwrap_or(top);
    folder == "config" && CLIENT_TOPS.contains(&stem)
}

/// Files a mod writes for itself, not settings anyone picked: caches,
/// benchmarks, error copies, generated mod lists.
pub fn generated(rel: &str) -> bool {
    let l = rel.to_ascii_lowercase();
    drift_noise(rel) || l.ends_with("modlist.json") || l.ends_with(".cache")
}

/// Extensions read as settings files.
fn settings_file(rel: &str) -> bool {
    let l = rel.to_ascii_lowercase();
    [
        ".toml", ".json", ".json5", ".jsonc", ".cfg", ".conf", ".properties", ".ini", ".yml", ".yaml", ".snbt", ".txt",
    ]
    .iter()
    .any(|e| l.ends_with(e))
}

/// The text with full-line comments, blank lines and indentation dropped:
/// two files with the same skeleton hold the same settings.
fn skeleton(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("//") && !l.starts_with(';'))
        .collect()
}

/// What [`read_change`] found in one changed file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    pub format_only: bool,
    pub settings: Option<usize>,
    pub binary: bool,
}

/// Reads both copies of a changed settings file: whether only formatting
/// differs, and how many settings differ.
pub fn read_change(client: &Path, server: &Path) -> Reading {
    let (c, s) = (read_text(client), read_text(server));
    let binary = |r: &Result<String, String>| r.as_ref().is_err_and(|e| e.contains("binary"));
    if binary(&c) || binary(&s) {
        return Reading { format_only: false, settings: None, binary: true };
    }
    let (Ok(c), Ok(s)) = (c, s) else {
        return Reading { format_only: false, settings: None, binary: false };
    };
    let format_only = skeleton(&c) == skeleton(&s);
    let fields = merge_fields(&c, &s);
    let changed = fields.iter().filter(|f| f.changed()).count();
    Reading {
        format_only,
        // Only when the file reads as settings: else the count means little.
        settings: (!fields.is_empty() && !format_only).then_some(changed),
        binary: false,
    }
}

/// At most this many changed files are read per classify: past it, the
/// rest stay Needed (the server share can be slow).
const READ_CAP: usize = 600;

/// A mod's id or name as config files are matched to it (letters and
/// digits, lowercase), for [`classify`]'s client mods.
pub fn mod_key(id_or_name: &str) -> String {
    squash(id_or_name)
}

/// Marks files named after a mod in `client_mods` ([`mod_key`]s of mods the
/// server doesn't run, like client-only ones) as client-side.
pub fn mark_client_mods(diff: &mut ConfigDiff, client_mods: &HashSet<String>) {
    if client_mods.is_empty() || !MOD_CONFIG_FOLDERS.contains(&diff.folder.as_str()) {
        return;
    }
    for f in &mut diff.files {
        if f.kind != Kind::ClientSide && config_owner(&f.rel).is_some_and(|k| client_mods.contains(&k)) {
            f.kind = Kind::ClientSide;
        }
    }
}

/// Sorts each difference into needed, client-side, generated or format
/// only. `client_mods`: see [`mark_client_mods`]. Reads changed settings files on both sides (in parallel), so it
/// runs off the UI thread. `progress(done, total)` reports the reading.
pub fn classify(
    client_root: &Path,
    server_root: &Path,
    diff: &mut ConfigDiff,
    client_mods: &HashSet<String>,
    progress: &(dyn Fn(usize, usize) + Sync),
) {
    let folder = diff.folder.clone();
    for f in &mut diff.files {
        f.kind = if client_side(&folder, &f.rel) {
            Kind::ClientSide
        } else if generated(&f.rel) {
            Kind::Generated
        } else {
            Kind::Needed
        };
    }
    mark_client_mods(diff, client_mods);
    let (c_dir, s_dir) = (client_root.join(&folder), server_root.join(&folder));
    let todo: Vec<usize> = diff
        .files
        .iter()
        .enumerate()
        .filter(|(_, f)| f.status == FileStatus::Modified && f.kind == Kind::Needed && settings_file(&f.rel))
        .map(|(i, _)| i)
        .take(READ_CAP)
        .collect();
    let total = todo.len();
    let done = std::sync::atomic::AtomicUsize::new(0);
    let out: Mutex<Vec<(usize, Reading)>> = Mutex::new(Vec::new());
    if total > 0 {
        progress(0, total);
        let chunk = total.div_ceil(8);
        let files = &diff.files;
        std::thread::scope(|sc| {
            for part in todo.chunks(chunk) {
                let (done, out, c_dir, s_dir) = (&done, &out, &c_dir, &s_dir);
                sc.spawn(move || {
                    for &i in part {
                        let rel = &files[i].rel;
                        let r = read_change(&c_dir.join(rel), &s_dir.join(rel));
                        out.lock().unwrap().push((i, r));
                        let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        if n % 10 == 0 || n == total {
                            progress(n, total);
                        }
                    }
                });
            }
        });
    }
    for (i, r) in out.into_inner().unwrap() {
        let f = &mut diff.files[i];
        f.settings = r.settings;
        f.binary = r.binary;
        if r.format_only {
            f.kind = Kind::FormatOnly;
        }
    }
    diff.classified = true;
}

/// Folders from `FOLDERS` that exist on either side.
pub fn present(client_root: &Path, server_root: &Path) -> Vec<String> {
    if let Some(r) = crate::hopperd::locate(server_root) {
        let theirs = r.and_then(|(c, id, _)| c.dirs(&id, FOLDERS)).unwrap_or_default();
        return FOLDERS
            .iter()
            .filter(|f| client_root.join(f).is_dir() || theirs.iter().any(|t| t == *f))
            .map(|f| f.to_string())
            .collect();
    }
    FOLDERS
        .iter()
        .filter(|f| client_root.join(f).is_dir() || server_root.join(f).is_dir())
        .map(|f| f.to_string())
        .collect()
}

/// Rejects paths that would leave the folder.
pub fn safe_rel(rel: &str) -> bool {
    let r = rel.replace('\\', "/");
    !r.is_empty()
        && !r.starts_with('/')
        && !r.contains(':')
        && !r.split('/').any(|p| p == ".." || p.is_empty())
}

const TEXT_CAP: u64 = 3 << 20;

/// A file's text for the preview, or why it can't be shown.
pub fn read_text(path: &Path) -> Result<String, String> {
    if let Some(r) = crate::hopperd::locate(path) {
        return r
            .and_then(|(c, id, rel)| c.read_text(&id, &rel))
            .map(|t| t.replace("\r\n", "\n"))
            .map_err(|e| format!("{e:#}"));
    }
    let md = fs::metadata(path).map_err(|e| e.to_string())?;
    if md.len() > TEXT_CAP {
        return Err(format!(
            "{} KB is too large to preview; open it in your editor",
            md.len() / 1024
        ));
    }
    let data = fs::read(path).map_err(|e| e.to_string())?;
    if data[..data.len().min(8192)].contains(&0) {
        return Err("Looks like a binary file; open it in your editor".into());
    }
    Ok(String::from_utf8_lossy(&data).replace("\r\n", "\n"))
}

// ── line diff, aligned for side by side ────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Same,
    /// Only on the server (left out of the client).
    Del,
    /// Only on the client.
    Add,
    /// A server line replaced by a client line.
    Change,
}

#[derive(Clone, Debug)]
pub struct Line {
    pub kind: LineKind,
    pub server: Option<String>,
    pub client: Option<String>,
    pub server_no: Option<usize>,
    pub client_no: Option<usize>,
}

/// Server text → client text, with runs of removed lines followed by added
/// lines paired into `Change` rows.
pub fn line_diff(server: &str, client: &str) -> Vec<Line> {
    use similar::{ChangeTag, TextDiff};
    let d = TextDiff::configure()
        .timeout(Duration::from_secs(2))
        .diff_lines(server, client);
    let raw: Vec<Line> = d
        .iter_all_changes()
        .map(|c| {
            let text = c.value().trim_end_matches('\n').to_string();
            match c.tag() {
                ChangeTag::Equal => Line {
                    kind: LineKind::Same,
                    server: Some(text.clone()),
                    client: Some(text),
                    server_no: c.old_index(),
                    client_no: c.new_index(),
                },
                ChangeTag::Delete => Line {
                    kind: LineKind::Del,
                    server: Some(text),
                    client: None,
                    server_no: c.old_index(),
                    client_no: None,
                },
                ChangeTag::Insert => Line {
                    kind: LineKind::Add,
                    server: None,
                    client: Some(text),
                    server_no: None,
                    client_no: c.new_index(),
                },
            }
        })
        .collect();
    let mut rows = Vec::with_capacity(raw.len());
    let mut k = 0;
    while k < raw.len() {
        if raw[k].kind != LineKind::Del {
            rows.push(raw[k].clone());
            k += 1;
            continue;
        }
        let mut d = k;
        while d < raw.len() && raw[d].kind == LineKind::Del {
            d += 1;
        }
        let mut a = d;
        while a < raw.len() && raw[a].kind == LineKind::Add {
            a += 1;
        }
        let (dels, adds) = (&raw[k..d], &raw[d..a]);
        let paired = dels.len().min(adds.len());
        for p in 0..paired {
            rows.push(Line {
                kind: LineKind::Change,
                server: dels[p].server.clone(),
                client: adds[p].client.clone(),
                server_no: dels[p].server_no,
                client_no: adds[p].client_no,
            });
        }
        rows.extend(dels[paired..].iter().cloned());
        rows.extend(adds[paired..].iter().cloned());
        k = a;
    }
    rows
}

/// (+added, −removed, ~changed) line counts.
pub fn stats(rows: &[Line]) -> (usize, usize, usize) {
    rows.iter().fold((0, 0, 0), |(a, d, c), r| match r.kind {
        LineKind::Add => (a + 1, d, c),
        LineKind::Del => (a, d + 1, c),
        LineKind::Change => (a, d, c + 1),
        LineKind::Same => (a, d, c),
    })
}

// ── settings view: key = value pairs, like an in-game config screen ─

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub section: String,
    pub key: String,
    pub value: String,
    pub desc: String,
}

static SECTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[+\s*([^\]]+?)\s*\]+\s*$").unwrap());
static BLOCK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^(?:[A-Za-z]:)?"?([^"{}]+?)"?\s*\{\s*$"#).unwrap());
static LIST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^(?:[A-Za-z]:)?"?([^"=<]+?)"?\s*<\s*$"#).unwrap());
static KV: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^(?:[A-Za-z]:)?(?:"([^"]+)"|([A-Za-z0-9_.\- ]+?))\s*[=:]\s*(.*)$"#).unwrap()
});

fn strip_inline_comment(v: &str) -> String {
    // JSON values carry the separator comma.
    let t = v.trim().trim_end_matches(',').trim_end();
    if t.starts_with(['"', '\'', '[']) {
        return t.to_string();
    }
    match t.find(" #").or_else(|| t.find("\t#")) {
        Some(h) if h > 0 => t[..h].trim().to_string(),
        _ => t.to_string(),
    }
}

/// TOML / .properties / Forge legacy .cfg into fields. A run of comment
/// lines right above a key is its description.
pub fn parse_fields(text: &str) -> Vec<Field> {
    let mut rows = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut comment: Vec<String> = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        i += 1;
        if line.is_empty() {
            comment.clear();
            continue;
        }
        if line.starts_with('#') || line.starts_with('!') || line.starts_with("//") {
            comment.push(
                line.trim_start_matches(['#', '!', '/', ' ', '\t'])
                    .trim()
                    .to_string(),
            );
            continue;
        }
        if line == "}" || line == "}," {
            stack.pop();
            comment.clear();
            continue;
        }
        if let Some(m) = SECTION.captures(line) {
            stack = vec![m[1].to_string()];
            comment.clear();
            continue;
        }
        if let Some(m) = BLOCK.captures(line) {
            stack.push(m[1].trim().to_string());
            comment.clear();
            continue;
        }
        if let Some(m) = LIST.captures(line) {
            let mut vals = Vec::new();
            while i < lines.len() && lines[i].trim() != ">" {
                let v = lines[i].trim();
                if !v.is_empty() {
                    vals.push(v.to_string());
                }
                i += 1;
            }
            i += 1;
            rows.push(Field {
                section: stack.join(" › "),
                key: m[1].trim().to_string(),
                value: format!("[ {} ]", vals.join(", ")),
                desc: comment.join(" "),
            });
            comment.clear();
            continue;
        }
        if let Some(m) = KV.captures(line) {
            let key = m.get(1).or(m.get(2)).map_or("", |k| k.as_str()).trim();
            rows.push(Field {
                section: stack.join(" › "),
                key: key.to_string(),
                value: strip_inline_comment(&m[3]),
                desc: comment.join(" "),
            });
        }
        comment.clear();
    }
    rows
}

#[derive(Clone, Debug, Default)]
pub struct MergedField {
    pub section: String,
    pub key: String,
    pub desc: String,
    pub client: Option<String>,
    pub server: Option<String>,
}

impl MergedField {
    pub fn changed(&self) -> bool {
        self.client != self.server
    }
}

/// Client and server fields side by side, by section + key.
pub fn merge_fields(client: &str, server: &str) -> Vec<MergedField> {
    let mut order: Vec<(String, String)> = Vec::new();
    let mut map: HashMap<(String, String), MergedField> = HashMap::new();
    let mut seen = HashSet::new();
    for (text, is_client) in [(client, true), (server, false)] {
        for f in parse_fields(text) {
            let k = (f.section.clone(), f.key.clone());
            if seen.insert(k.clone()) {
                order.push(k.clone());
            }
            let e = map.entry(k).or_insert_with(|| MergedField {
                section: f.section.clone(),
                key: f.key.clone(),
                ..Default::default()
            });
            if e.desc.is_empty() {
                e.desc = f.desc;
            }
            if is_client {
                e.client = Some(f.value);
            } else {
                e.server = Some(f.value);
            }
        }
    }
    order.into_iter().filter_map(|k| map.remove(&k)).collect()
}

// ── drift: what a pack or server update tends to leave behind ──────

/// Where global datapacks and server scripts live: (folder, prefix inside
/// it, one group per pack under the prefix). Every world on the server
/// loads these, and a server pack often doesn't carry them.
const DATA_ROOTS: &[(&str, &str, bool)] = &[
    ("config", "paxi/datapacks/", true),
    ("config", "openloader/", true),
    ("openloader", "", true),
    ("global_packs", "", true),
    ("kubejs", "data/", false),
    ("kubejs", "server_scripts/", false),
    ("kubejs", "startup_scripts/", false),
    // Quests run on the server; players get the server's copy.
    ("config", "ftbquests/quests/", false),
];

/// Folders whose files belong to mods by name.
const MOD_CONFIG_FOLDERS: &[&str] = &["config", "defaultconfigs"];

/// Files that differ between a pack and its server, in one group.
#[derive(Clone, Debug)]
pub struct DriftGroup {
    /// A datapack's path (`config/paxi/datapacks/tnp_data`) or a mod's name.
    pub label: String,
    /// The Configs view's folder and filter that show these files.
    pub folder: String,
    pub filter: String,
    pub files: Vec<FileDiff>,
}

impl DriftGroup {
    pub fn count(&self, s: FileStatus) -> usize {
        self.files.iter().filter(|f| f.status == s).count()
    }
}

/// Global datapacks, server scripts and mod configs that differ between a
/// pack and its server: what a server or pack update may not have brought
/// along.
#[derive(Clone, Debug, Default)]
pub struct Drift {
    /// Global datapacks and server scripts, any difference.
    pub data: Vec<DriftGroup>,
    /// Config files of mods the server runs, missing on the server or changed.
    pub mods: Vec<DriftGroup>,
}

impl Drift {
    pub fn is_empty(&self) -> bool {
        self.data.is_empty() && self.mods.is_empty()
    }

    /// "2 datapacks and 3 mods' configs", for a notice.
    pub fn summary(&self) -> String {
        let n = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let mut v = Vec::new();
        if !self.data.is_empty() {
            v.push(n(self.data.len(), "datapack or script folder", "datapacks or script folders"));
        }
        if !self.mods.is_empty() {
            v.push(n(self.mods.len(), "mod's configs", "mods' configs"));
        }
        v.join(" and ")
    }

    /// Changes when any listed file or its status does.
    pub fn signature(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for g in self.data.iter().chain(&self.mods) {
            g.label.hash(&mut h);
            for f in &g.files {
                (&f.rel, rank(f.status), f.client_size, f.server_size).hash(&mut h);
            }
        }
        h.finish()
    }
}

/// Letters and digits only, lowercase: `bedrock_ores` and `BedrockOres` match.
fn squash(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// Files a mod writes for itself (caches, benchmarks, error copies), not
/// settings anyone picked.
fn drift_noise(rel: &str) -> bool {
    let l = rel.to_lowercase();
    ["cache", "benchmark", "had-error", "backup"].iter().any(|w| l.contains(w))
        || [".csv", ".dat", ".db", ".bin"].iter().any(|e| l.ends_with(e))
}

/// Whether a changed config matters to the server: its server or common
/// settings. Other changed files are mostly client tweaks (HUD, keys).
fn server_side_config(folder: &str, rel: &str) -> bool {
    let l = rel.to_lowercase();
    folder == "defaultconfigs" || l.contains("server") || l.contains("common")
}

/// The mod a config file is named after, squashed: `bedrockores-common.toml`
/// and `mekanism/general.toml` give `bedrockores` and `mekanism`. Client
/// configs don't matter to a server.
fn config_owner(rel: &str) -> Option<String> {
    let l = rel.to_lowercase();
    if l.contains("client") {
        return None;
    }
    let (first, nested) = match l.split_once('/') {
        Some((d, _)) => (d, true),
        None => (l.as_str(), false),
    };
    let mut stem = if nested { first } else { first.split('.').next().unwrap_or(first) };
    for s in ["-common", "_common", "-server", "_server", "-startup", "_startup", "-config", "_config"] {
        stem = stem.strip_suffix(s).unwrap_or(stem);
    }
    let k = squash(stem);
    (!k.is_empty()).then_some(k)
}

/// What differs in the global datapacks, server scripts and in the configs
/// of `mods` (the server's mods as (mod id, name)) between `client_root`
/// and `server_root`. Compares by size, through the same cache as the
/// Configs view.
pub fn drift(client_root: &Path, server_root: &Path, mods: &[(String, String)]) -> Drift {
    let mut out = Drift::default();
    let mut walked: HashMap<&str, ConfigDiff> = HashMap::new();
    let mut get = |folder: &'static str| -> ConfigDiff {
        walked
            .entry(folder)
            .or_insert_with(|| {
                let remote = crate::hopperd::is_remote_root(server_root);
                if remote || client_root.join(folder).is_dir() || server_root.join(folder).is_dir() {
                    compare(client_root, server_root, folder, false, &|_, _| {})
                } else {
                    ConfigDiff::default()
                }
            })
            .clone()
    };

    let mut data: Vec<DriftGroup> = Vec::new();
    for &(folder, prefix, per_pack) in DATA_ROOTS {
        for f in get(folder).files {
            let Some(rest) = f.rel.strip_prefix(prefix) else { continue };
            if rest.is_empty() || rest.to_lowercase().contains("resources/") || drift_noise(rest) {
                continue;
            }
            let inner = match (per_pack, rest.split_once('/')) {
                (true, Some((pack, _))) => format!("{prefix}{pack}"),
                (true, None) => format!("{prefix}{rest}"),
                (false, _) => prefix.trim_end_matches('/').to_string(),
            };
            let label = format!("{folder}/{inner}");
            match data.iter_mut().find(|g| g.label == label) {
                Some(g) => g.files.push(f),
                None => data.push(DriftGroup {
                    label,
                    folder: folder.to_string(),
                    filter: inner,
                    files: vec![f],
                }),
            }
        }
    }

    let owners: HashMap<String, &str> = mods
        .iter()
        .map(|(id, name)| (squash(id), if name.is_empty() { id.as_str() } else { name.as_str() }))
        .filter(|(k, _)| k.len() >= 3)
        .collect();
    let mut by_mod: Vec<DriftGroup> = Vec::new();
    let mut server_tops: HashMap<(&str, String), bool> = HashMap::new();
    for &folder in MOD_CONFIG_FOLDERS {
        for f in get(folder).files {
            // A server-only file is the server's own business.
            if f.status == FileStatus::OnlyServer
                || drift_noise(&f.rel)
                || (f.status == FileStatus::Modified && !server_side_config(folder, &f.rel))
                || DATA_ROOTS.iter().any(|&(d, p, _)| d == folder && !p.is_empty() && f.rel.starts_with(p))
                || f.rel.starts_with("paxi/")
            {
                continue;
            }
            let Some(name) = config_owner(&f.rel).and_then(|k| owners.get(&k).copied()) else {
                continue;
            };
            // Same settings, other comments: nothing to bring along.
            if f.status == FileStatus::Modified
                && settings_file(&f.rel)
                && read_change(&client_root.join(folder).join(&f.rel), &server_root.join(folder).join(&f.rel))
                    .format_only
            {
                continue;
            }
            // A missing file counts when the server has none of the mod's
            // config: a mod new to the pack. Inside a folder the server
            // already has, missing files are mostly ones the mod generates.
            if f.status == FileStatus::OnlyClient && f.rel.contains('/') {
                let top = f.rel.split('/').next().unwrap_or_default().to_string();
                let has = *server_tops
                    .entry((folder, top.clone()))
                    .or_insert_with(|| server_root.join(folder).join(&top).exists());
                if has {
                    continue;
                }
            }
            match by_mod.iter_mut().find(|g| g.label == name) {
                Some(g) => g.files.push(f),
                None => {
                    let filter = f.rel.split(['/', '-', '.']).next().unwrap_or("").to_string();
                    by_mod.push(DriftGroup {
                        label: name.to_string(),
                        folder: folder.to_string(),
                        filter,
                        files: vec![f],
                    })
                }
            }
        }
    }
    // Missing files first: a mod new to the pack has all its configs missing.
    by_mod.sort_by_key(|g| (std::cmp::Reverse(g.count(FileStatus::OnlyClient)), std::cmp::Reverse(g.files.len())));
    out.data = data;
    out.mods = by_mod;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_client_side_generated_and_format_only() {
        assert!(client_side("config", "ars_nouveau-client.toml"));
        assert!(client_side("config", "Mekanism/client.toml"));
        assert!(client_side("config", "jei/jei-client.ini"));
        assert!(client_side("config", "xaerominimap.txt"));
        assert!(client_side("config", "paxi/resourcepacks/a.zip"));
        assert!(!client_side("config", "ars_nouveau-server.toml"));
        assert!(!client_side("config", "mekanism/general.toml"));
        assert!(!client_side("config", "client-server-sync-common.toml"));
        assert!(!client_side("defaultconfigs", "foo-client.toml"));
        assert!(generated("crash_assistant/modlist.json"));
        assert!(generated("foo/cache/x.json"));
        assert!(!generated("cobblemon/main.json"));

        let root = std::env::temp_dir().join(format!("hopper-cfgdiff-kind-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (c, s) = (root.join("c/config"), root.join("s/config"));
        for d in [&c, &s] {
            fs::create_dir_all(d).unwrap();
        }
        // Only a comment and indentation differ.
        fs::write(c.join("a-common.toml"), "# new comment\n[general]\n  x = 1\n\ny = 2\n").unwrap();
        fs::write(s.join("a-common.toml"), "[general]\nx = 1\ny = 2\n").unwrap();
        // A value differs.
        fs::write(c.join("b-common.toml"), "[general]\nx = 1\ny = 3\n# pad\n").unwrap();
        fs::write(s.join("b-common.toml"), "[general]\nx = 1\ny = 2\n").unwrap();
        fs::write(c.join("c-client.toml"), "z = 1\n").unwrap();
        fs::write(s.join("same.toml"), "q = 1\n").unwrap();
        fs::write(c.join("same.toml"), "q = 1\n").unwrap();
        let mut d = compare(&root.join("c"), &root.join("s"), "config", false, &|_, _| {});
        assert_eq!(d.same_files, ["same.toml"]);
        classify(&root.join("c"), &root.join("s"), &mut d, &HashSet::new(), &|_, _| {});
        let kind = |r: &str| d.files.iter().find(|f| f.rel == r).map(|f| (f.kind, f.settings));
        assert_eq!(kind("a-common.toml"), Some((Kind::FormatOnly, None)));
        assert_eq!(kind("b-common.toml"), Some((Kind::Needed, Some(1))));
        assert_eq!(kind("c-client.toml"), Some((Kind::ClientSide, None)));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn paxi_resource_packs_stay_off_the_server() {
        assert!(client_only("config/paxi/resourcepacks/Fancy_Beds-v4.2.zip"));
        assert!(client_only("config/paxi/resourcepacks/Icons v.1.14.zip.rpo"));
        assert!(client_only("paxi/resourcepacks/x.zip"));
        assert!(!client_only("config/paxi/datapacks/x.zip"));
        assert!(!client_only("config/paxi/datapack_load_order.json"));
        assert!(!client_only("config/paxi/resourcepacks"));

        let root = std::env::temp_dir().join(format!("hopper-cfgdiff-paxi-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (c, s) = (root.join("c"), root.join("s"));
        for d in [&c, &s] {
            fs::create_dir_all(d.join("config/paxi/resourcepacks")).unwrap();
            fs::create_dir_all(d.join("config/paxi/datapacks")).unwrap();
        }
        fs::write(c.join("config/paxi/resourcepacks/rp.zip"), "rp").unwrap();
        fs::write(c.join("config/paxi/resourcepacks/rp.zip.rpo"), "{}").unwrap();
        fs::write(s.join("config/paxi/resourcepacks/old.zip"), "old").unwrap();
        fs::write(c.join("config/paxi/datapacks/dp.zip"), "dp").unwrap();
        let d = compare(&c, &s, "config", true, &|_, _| {});
        let rels: Vec<&str> = d.files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, ["paxi/datapacks/dp.zip"]);
        assert_eq!((d.client_files, d.server_files), (1, 0));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn compares_by_size_then_content() {
        let root = std::env::temp_dir().join(format!("hopper-cfgdiff-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (c, s) = (root.join("c"), root.join("s"));
        for d in [&c, &s] {
            fs::create_dir_all(d.join("config/sub")).unwrap();
        }
        fs::write(c.join("config/a.toml"), "x = 1").unwrap();
        fs::write(s.join("config/a.toml"), "x = 2").unwrap(); // same size, new content
        fs::write(c.join("config/b.toml"), "long = true").unwrap();
        fs::write(s.join("config/b.toml"), "long = false").unwrap();
        fs::write(c.join("config/sub/new.json"), "{}").unwrap();
        fs::write(s.join("config/old.cfg"), "y").unwrap();
        fs::write(c.join("config/same.txt"), "same").unwrap();
        fs::write(s.join("config/same.txt"), "same").unwrap();
        fs::write(c.join("config/x.toml.bak"), "noise").unwrap();

        let quick = compare(&c, &s, "config", false, &|_, _| {});
        let st: Vec<(&str, FileStatus)> = quick
            .files
            .iter()
            .map(|f| (f.rel.as_str(), f.status))
            .collect();
        assert_eq!(
            st,
            [
                ("b.toml", FileStatus::Modified),
                ("sub/new.json", FileStatus::OnlyClient),
                ("old.cfg", FileStatus::OnlyServer),
            ]
        );
        assert_eq!(quick.same, 2);
        forget_walk(&c.join("config"));
        forget_walk(&s.join("config"));
        let deep = compare(&c, &s, "config", true, &|_, _| {});
        assert!(
            deep.files
                .iter()
                .any(|f| f.rel == "a.toml" && f.status == FileStatus::Modified)
        );
        assert_eq!(deep.same, 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn drift_finds_datapacks_and_mod_configs() {
        let root = std::env::temp_dir().join(format!("hopper-drift-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (c, s) = (root.join("c"), root.join("s"));
        let put = |p: PathBuf, t: &str| {
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, t).unwrap();
        };
        put(c.join("config/paxi/datapacks/tnp_data/data/bedrockores/a.json"), "{}");
        put(c.join("config/paxi/datapacks/tnp_data/pack.mcmeta"), "{}");
        put(s.join("config/paxi/datapacks/tnp_data/pack.mcmeta"), "{}");
        put(s.join("config/paxi/datapacks/tnp_data/data/old/x.json"), "{}");
        put(c.join("config/paxi/resourcepacks/r.zip"), "zip");
        put(c.join("config/bedrockores-common.toml"), "a = 1");
        put(s.join("config/bedrockores-common.toml"), "a = 22");
        put(c.join("config/bedrockores-client.toml"), "ui");
        put(c.join("config/mekanism/general.toml"), "x");
        put(c.join("config/jei/jei.ini"), "x");
        put(s.join("config/servermod.toml"), "x");
        put(c.join("kubejs/server_scripts/main.js"), "x");
        put(c.join("kubejs/client_scripts/ui.js"), "x");
        let mods = [
            ("bedrockores".to_string(), "Bedrock Ores".to_string()),
            ("mekanism".to_string(), "Mekanism".to_string()),
            ("servermod".to_string(), "Server Mod".to_string()),
        ];
        let d = drift(&c, &s, &mods);
        let data: Vec<(&str, usize)> = d.data.iter().map(|g| (g.label.as_str(), g.files.len())).collect();
        assert_eq!(data, [("config/paxi/datapacks/tnp_data", 2), ("kubejs/server_scripts", 1)]);
        assert_eq!(d.data[0].filter, "paxi/datapacks/tnp_data");
        let mods: Vec<(&str, usize)> = d.mods.iter().map(|g| (g.label.as_str(), g.files.len())).collect();
        // Mekanism's config is missing (first); jei isn't a server mod,
        // the client config and the server-only file don't count.
        assert_eq!(mods, [("Mekanism", 1), ("Bedrock Ores", 1)]);
        let _ = fs::remove_dir_all(&root);
    }

    /// Prints the drift between TNP-LL8 and its rig server.
    /// `cargo test real_drift -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_drift() {
        let c = Path::new(r"G:\Games\Minecraft\Curse\Instances\TNP Limitless 8");
        let s = Path::new(r"Q:\Minecraft\packs\TNP-LL8");
        let jars = crate::scan::scan_folder(&s.join("mods")).unwrap();
        let mods: Vec<(String, String)> = jars
            .iter()
            .filter(|m| m.side != "client")
            .map(|m| (m.mod_id.clone(), m.mod_name.clone()))
            .collect();
        let t = Instant::now();
        let d = drift(c, s, &mods);
        println!("{:?}", t.elapsed());
        for g in d.data.iter().chain(&d.mods) {
            println!(
                "{} [{} / {}]: {} missing, {} changed, {} server only",
                g.label,
                g.folder,
                g.filter,
                g.count(FileStatus::OnlyClient),
                g.count(FileStatus::Modified),
                g.count(FileStatus::OnlyServer)
            );
            for f in g.files.iter().take(3) {
                println!("    {} {:?}", f.rel, f.status);
            }
        }
    }

    #[test]
    fn pairs_replaced_lines() {
        let rows = line_diff("a\nb\nc\n", "a\nB\nc\nd\n");
        let kinds: Vec<LineKind> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            [
                LineKind::Same,
                LineKind::Change,
                LineKind::Same,
                LineKind::Add
            ]
        );
        assert_eq!(rows[1].server.as_deref(), Some("b"));
        assert_eq!(rows[1].client.as_deref(), Some("B"));
        assert_eq!(stats(&rows), (1, 0, 1));
    }

    #[test]
    fn reads_toml_and_forge_cfg_fields() {
        let toml = "# How far\n[general]\n\t#Range in blocks\n\trange = 16 # inline\n\t\"quoted key\" = \"v\"\n";
        let f = parse_fields(toml);
        assert_eq!(f[0].section, "general");
        assert_eq!((f[0].key.as_str(), f[0].value.as_str()), ("range", "16"));
        assert_eq!(f[0].desc, "Range in blocks");
        assert_eq!(f[1].key, "quoted key");

        let cfg = "general {\n    B:enabled=true\n    S:list <\n        a\n        b\n     >\n}\n";
        let f = parse_fields(cfg);
        assert_eq!(
            (
                f[0].section.as_str(),
                f[0].key.as_str(),
                f[0].value.as_str()
            ),
            ("general", "enabled", "true")
        );
        assert_eq!(f[1].value, "[ a, b ]");

        let m = merge_fields("[g]\na = 1\nb = 2\n", "[g]\na = 1\nb = 3\nc = 4\n");
        let changed: Vec<&str> = m
            .iter()
            .filter(|f| f.changed())
            .map(|f| f.key.as_str())
            .collect();
        assert_eq!(changed, ["b", "c"]);
    }

    #[test]
    fn rejects_escaping_paths() {
        assert!(safe_rel("sub/a.toml"));
        assert!(!safe_rel("../a"));
        assert!(!safe_rel("C:/x"));
        assert!(!safe_rel("/abs"));
    }
}
