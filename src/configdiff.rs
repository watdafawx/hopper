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

#[derive(Clone, Debug)]
pub struct FileDiff {
    pub rel: String,
    pub status: FileStatus,
    pub client_size: u64,
    pub server_size: u64,
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
    let m = walk(root);
    WALKS
        .lock()
        .unwrap()
        .insert(root.to_path_buf(), (Instant::now(), m.clone()));
    m
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
    directories::ProjectDirs::from("", "", "Hopper").map(|d| d.cache_dir().join("cfghash.json"))
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
    let mut files = Vec::new();
    let mut equal = Vec::new();
    for (rel, cs) in &c {
        match s.get(rel) {
            None => files.push(FileDiff {
                rel: rel.clone(),
                status: FileStatus::OnlyClient,
                client_size: cs.size,
                server_size: 0,
            }),
            Some(ss) if ss.size != cs.size => files.push(FileDiff {
                rel: rel.clone(),
                status: FileStatus::Modified,
                client_size: cs.size,
                server_size: ss.size,
            }),
            Some(ss) => equal.push((rel.clone(), *cs, *ss)),
        }
    }
    for (rel, ss) in &s {
        if !c.contains_key(rel) {
            files.push(FileDiff {
                rel: rel.clone(),
                status: FileStatus::OnlyServer,
                client_size: 0,
                server_size: ss.size,
            });
        }
    }
    let mut same = equal.len();
    if deep && !equal.is_empty() {
        let done = std::sync::atomic::AtomicUsize::new(0);
        let changed = Mutex::new(Vec::new());
        let (chunk, total) = (equal.len().div_ceil(12), equal.len());
        std::thread::scope(|sc| {
            for part in equal.chunks(chunk) {
                let (done, changed, c_root, s_root) = (&done, &changed, &c_root, &s_root);
                sc.spawn(move || {
                    for (rel, cs, ss) in part {
                        let a = hash(&c_root.join(rel), *cs);
                        let b = hash(&s_root.join(rel), *ss);
                        if matches!((&a, &b), (Some(a), Some(b)) if a != b) {
                            changed.lock().unwrap().push(FileDiff {
                                rel: rel.clone(),
                                status: FileStatus::Modified,
                                client_size: cs.size,
                                server_size: ss.size,
                            });
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
    }
}

/// Folders from `FOLDERS` that exist on either side.
pub fn present(client_root: &Path, server_root: &Path) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
