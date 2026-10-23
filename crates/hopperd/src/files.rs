//! A server's files: listing, reading and saving text, moving things
//! around, the trash and resumable uploads. Every path the API takes is
//! relative to the server's folder and checked to stay inside it.
//!
//! Nothing is deleted outright: trashing moves into `.hopper-trash` in the
//! server folder (same drive, so instant), and saving over a file keeps the
//! old copy there too. Uploads land in `.hopper-uploads` and are renamed
//! into place when complete.

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const TRASH_DIR: &str = ".hopper-trash";
pub const UPLOAD_DIR: &str = ".hopper-uploads";
/// Trash older than this is removed when the trash is next listed.
const TRASH_KEEP: Duration = Duration::from_secs(30 * 24 * 3600);
/// Saved-over copies kept per file.
const VERSIONS_KEPT: usize = 20;
/// Largest file the editor opens.
pub const MAX_TEXT: u64 = 4 * 1024 * 1024;
/// Unfinished uploads older than this are dropped.
const UPLOAD_KEEP: Duration = Duration::from_secs(24 * 3600);

/// Names hopperd keeps for itself; not listed, not touched by file ops.
pub fn is_internal(name: &str) -> bool {
    name == TRASH_DIR || name == UPLOAD_DIR || name == hopper_core::hosting::backup::DIR
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn now() -> u64 {
    secs(SystemTime::now())
}

/// `rel` ("config/create.toml", backslashes allowed) inside `root`. Refuses
/// `..`, absolute paths, drive letters and anything a symlink would lead
/// outside the folder.
pub fn resolve(root: &Path, rel: &str) -> anyhow::Result<PathBuf> {
    let mut out = root.to_path_buf();
    for part in rel.replace('\\', "/").split('/') {
        match part {
            "" | "." => {}
            ".." => bail!("Paths can't go above the server folder."),
            p if p.contains(':') => bail!("That isn't a path in the server folder."),
            p => out.push(p),
        }
    }
    // Follow symlinks on whatever part exists and make sure it stays in.
    let canon_root = fs::canonicalize(root).with_context(|| format!("server folder {}", root.display()))?;
    let mut probe = out.as_path();
    loop {
        if let Ok(c) = fs::canonicalize(probe) {
            if !c.starts_with(&canon_root) {
                bail!("That path leads outside the server folder.");
            }
            break;
        }
        match probe.parent() {
            Some(p) => probe = p,
            None => break,
        }
    }
    Ok(out)
}

/// `p` relative to `root`, with forward slashes.
pub fn rel_of(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn check_name(name: &str) -> anyhow::Result<&str> {
    let n = name.trim();
    if n.is_empty() || n == "." || n == ".." || n.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']) {
        bail!("\"{name}\" can't be used as a name.");
    }
    if is_internal(n) {
        bail!("{n} is reserved for hopperd.");
    }
    Ok(n)
}

// ---- listing -------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    /// Unix seconds.
    pub modified: u64,
}

pub fn list(root: &Path, rel: &str) -> anyhow::Result<Vec<Entry>> {
    let dir = resolve(root, rel)?;
    let rd = fs::read_dir(&dir).with_context(|| format!("can't open {}", if rel.is_empty() { "the server folder" } else { rel }))?;
    let at_root = dir == root;
    let mut out: Vec<Entry> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if at_root && is_internal(&name) {
                return None;
            }
            // The directory read already has type, size and time (free on
            // Windows); only links need a second look to follow them.
            let mut meta = e.metadata().ok()?;
            if meta.file_type().is_symlink() {
                meta = fs::metadata(e.path()).unwrap_or(meta);
            }
            Some(Entry {
                dir: meta.is_dir(),
                size: if meta.is_dir() { 0 } else { meta.len() },
                modified: meta.modified().map(secs).unwrap_or(0),
                name,
            })
        })
        .collect();
    out.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(out)
}

// ---- text ------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct Text {
    pub content: String,
    pub modified: u64,
    pub size: u64,
}

pub fn read_text(root: &Path, rel: &str) -> anyhow::Result<Text> {
    let p = resolve(root, rel)?;
    let meta = fs::metadata(&p).with_context(|| format!("{rel} doesn't exist"))?;
    if meta.is_dir() {
        bail!("{rel} is a folder.");
    }
    if meta.len() > MAX_TEXT {
        bail!("{rel} is too big to edit here ({} MB). Download it instead.", meta.len() / (1024 * 1024));
    }
    let bytes = fs::read(&p)?;
    if bytes.iter().take(8192).any(|b| *b == 0) {
        bail!("{rel} isn't a text file.");
    }
    let content = String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("{rel} isn't UTF-8 text."))?;
    Ok(Text { content, modified: meta.modified().map(secs).unwrap_or(0), size: meta.len() })
}

/// A syntax problem in a config, so it isn't saved broken.
pub fn validate(name: &str, content: &str) -> Result<(), String> {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "json" | "mcmeta" => serde_json::from_str::<serde_json::Value>(content)
            .map(|_| ())
            .map_err(|e| format!("JSON error on line {}, column {}: {e}", e.line(), e.column())),
        "toml" => toml::from_str::<toml::Value>(content).map(|_| ()).map_err(|e| {
            let line = e.span().map(|s| content[..s.start.min(content.len())].lines().count().max(1));
            match line {
                Some(l) => format!("TOML error on line {l}: {}", e.message()),
                None => format!("TOML error: {}", e.message()),
            }
        }),
        _ => Ok(()),
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum SaveResult {
    Saved { modified: u64, size: u64 },
    /// The file changed on disk after it was opened.
    Conflict { modified: u64 },
    Invalid { error: String },
}

/// Saves `content`; the old copy goes to the trash as a version. `expected`
/// is the modified time the editor loaded (None for a new file).
pub fn write_text(root: &Path, rel: &str, content: &str, expected: Option<u64>, force: bool) -> anyhow::Result<SaveResult> {
    let p = resolve(root, rel)?;
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    check_name(&name)?;
    if !force && let Err(error) = validate(&name, content) {
        return Ok(SaveResult::Invalid { error });
    }
    if let Ok(meta) = fs::metadata(&p) {
        if meta.is_dir() {
            bail!("{rel} is a folder.");
        }
        let on_disk = meta.modified().map(secs).unwrap_or(0);
        if !force && expected.is_some_and(|e| e != on_disk) {
            return Ok(SaveResult::Conflict { modified: on_disk });
        }
        keep_version(root, rel, &p)?;
    }
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = p.with_file_name(format!(".{name}.hopperd-tmp"));
    fs::write(&tmp, content.as_bytes()).with_context(|| format!("writing {rel}"))?;
    fs::rename(&tmp, &p).with_context(|| format!("saving {rel}"))?;
    let meta = fs::metadata(&p)?;
    Ok(SaveResult::Saved { modified: meta.modified().map(secs).unwrap_or(0), size: meta.len() })
}

// ---- trash -----------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrashItem {
    pub id: String,
    /// Where it was, relative to the server folder.
    pub path: String,
    /// "deleted", or "saved-over" for a file's previous version.
    pub kind: String,
    pub at: u64,
    pub size: u64,
    pub dir: bool,
}

const META: &str = "item.json";
const ITEM: &str = "item";

fn trash_root(root: &Path) -> PathBuf {
    root.join(TRASH_DIR)
}

fn size_of(p: &Path) -> u64 {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => fs::read_dir(p).map(|rd| rd.flatten().map(|e| size_of(&e.path())).sum()).unwrap_or(0),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

fn new_trash_slot(root: &Path) -> anyhow::Result<(String, PathBuf)> {
    let id = format!("{}-{}", now(), crate::auth::random_hex(4));
    let slot = trash_root(root).join(&id);
    fs::create_dir_all(&slot)?;
    Ok((id, slot))
}

fn write_meta(slot: &Path, item: &TrashItem) -> anyhow::Result<()> {
    fs::write(slot.join(META), serde_json::to_string_pretty(item)?)?;
    Ok(())
}

/// Moves `rel` into the trash.
pub fn trash(root: &Path, rel: &str) -> anyhow::Result<TrashItem> {
    let p = resolve(root, rel)?;
    if p == root {
        bail!("The server folder itself can't be trashed.");
    }
    let meta = fs::symlink_metadata(&p).with_context(|| format!("{rel} doesn't exist"))?;
    let (id, slot) = new_trash_slot(root)?;
    let item = TrashItem { id, path: rel_of(root, &p), kind: "deleted".into(), at: now(), size: size_of(&p), dir: meta.is_dir() };
    write_meta(&slot, &item)?;
    if let Err(e) = fs::rename(&p, slot.join(ITEM)) {
        let _ = fs::remove_dir_all(&slot);
        return Err(e).with_context(|| format!("moving {rel} to the trash (is the server using it?)"));
    }
    Ok(item)
}

/// Keeps a copy of `p` as its previous version, and only the newest few.
pub fn keep_version(root: &Path, rel: &str, p: &Path) -> anyhow::Result<()> {
    let (id, slot) = new_trash_slot(root)?;
    fs::copy(p, slot.join(ITEM)).with_context(|| format!("keeping the old {rel}"))?;
    let path = rel_of(root, p);
    write_meta(&slot, &TrashItem { id, path: path.clone(), kind: "saved-over".into(), at: now(), size: size_of(p), dir: false })?;
    let mut versions: Vec<TrashItem> =
        list_trash(root)?.into_iter().filter(|t| t.kind == "saved-over" && t.path == path).collect();
    versions.sort_by_key(|t| std::cmp::Reverse(t.at));
    for old in versions.into_iter().skip(VERSIONS_KEPT) {
        let _ = fs::remove_dir_all(trash_root(root).join(&old.id));
    }
    Ok(())
}

/// What's in the trash, newest first. Drops items past their time.
pub fn list_trash(root: &Path) -> anyhow::Result<Vec<TrashItem>> {
    let Ok(rd) = fs::read_dir(trash_root(root)) else { return Ok(Vec::new()) };
    let cutoff = now().saturating_sub(TRASH_KEEP.as_secs());
    let mut out = Vec::new();
    for e in rd.flatten() {
        let item: Option<TrashItem> = fs::read_to_string(e.path().join(META)).ok().and_then(|t| serde_json::from_str(&t).ok());
        match item {
            Some(i) if i.at >= cutoff && e.path().join(ITEM).exists() => out.push(i),
            Some(_) => {
                let _ = fs::remove_dir_all(e.path());
            }
            None => {}
        }
    }
    out.sort_by_key(|t| std::cmp::Reverse(t.at));
    Ok(out)
}

/// `name` in `dir`, or "name (2).ext" etc. if taken.
fn free_name(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (2..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).unwrap()
}

/// Puts a trashed item back where it was (under a new name if that spot is
/// taken). Returns where it went.
pub fn restore(root: &Path, id: &str) -> anyhow::Result<String> {
    check_name(id)?;
    let slot = trash_root(root).join(id);
    let item: TrashItem = serde_json::from_str(&fs::read_to_string(slot.join(META)).context("That item isn't in the trash.")?)?;
    let target = resolve(root, &item.path)?;
    let parent = target.parent().context("bad path")?;
    fs::create_dir_all(parent)?;
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let dest = if item.kind == "saved-over" && target.exists() {
        // Restoring a version replaces the current file, which is kept.
        keep_version(root, &item.path, &target)?;
        fs::remove_file(&target)?;
        target
    } else {
        free_name(parent, &name)
    };
    fs::rename(slot.join(ITEM), &dest).context("restoring")?;
    let _ = fs::remove_dir_all(&slot);
    Ok(rel_of(root, &dest))
}

/// Deletes trashed items for good (all of them when `ids` is empty).
pub fn purge(root: &Path, ids: &[String]) -> anyhow::Result<usize> {
    let items = list_trash(root)?;
    let mut n = 0;
    for i in items.iter().filter(|i| ids.is_empty() || ids.contains(&i.id)) {
        fs::remove_dir_all(trash_root(root).join(&i.id))?;
        n += 1;
    }
    Ok(n)
}

/// A trashed file's text (a saved-over version, for diffs).
pub fn trash_text(root: &Path, id: &str) -> anyhow::Result<String> {
    check_name(id)?;
    let p = trash_root(root).join(id).join(ITEM);
    let meta = fs::metadata(&p).context("That version is gone.")?;
    if meta.is_dir() || meta.len() > MAX_TEXT {
        bail!("That version can't be shown as text.");
    }
    let bytes = fs::read(&p)?;
    if bytes.iter().take(8192).any(|b| *b == 0) {
        bail!("That version isn't text.");
    }
    Ok(String::from_utf8_lossy(&bytes).replace("\r\n", "\n"))
}

// ---- search ----------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct Hit {
    pub path: String,
    /// 1-based; 0 when the name matched.
    pub line: usize,
    pub text: String,
}

/// Files bigger than this aren't searched inside.
const SEARCH_MAX_FILE: u64 = 2 * 1024 * 1024;

/// Looks for `query` in file names and text files under `rel`: plain text
/// (any case) or a regular expression. At most `limit` hits.
pub fn search(root: &Path, rel: &str, query: &str, regex: bool, limit: usize) -> anyhow::Result<(Vec<Hit>, bool)> {
    let start = resolve(root, rel)?;
    let re = if regex {
        regex::RegexBuilder::new(query).case_insensitive(true).size_limit(1 << 20).build().map_err(|e| anyhow::anyhow!("{e}"))?
    } else {
        regex::RegexBuilder::new(&regex::escape(query)).case_insensitive(true).build()?
    };
    let mut hits = Vec::new();
    let mut stack = vec![start];
    let skip_dirs = ["libraries", "versions", ".fabric", "cache", "logs", "crash-reports"];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                let top = p.parent() == Some(root);
                if !(is_internal(&name) || (top && skip_dirs.contains(&name.as_str())) || name.ends_with("region")) {
                    stack.push(p);
                }
                continue;
            }
            let path = rel_of(root, &p);
            if re.is_match(&name) {
                hits.push(Hit { path: path.clone(), line: 0, text: name.clone() });
            }
            if hits.len() >= limit {
                return Ok((hits, true));
            }
            let lower = name.to_lowercase();
            if [".jar", ".zip", ".mca", ".png", ".dat", ".nbt", ".ogg", ".gz", ".class", ".exe", ".dll"].iter().any(|x| lower.ends_with(x)) {
                continue;
            }
            if e.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > SEARCH_MAX_FILE {
                continue;
            }
            let Ok(bytes) = fs::read(&p) else { continue };
            if bytes.iter().take(4096).any(|b| *b == 0) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            for (i, l) in text.lines().enumerate() {
                if re.is_match(l) {
                    let t: String = l.trim().chars().take(240).collect();
                    hits.push(Hit { path: path.clone(), line: i + 1, text: t });
                    if hits.len() >= limit {
                        return Ok((hits, true));
                    }
                }
            }
        }
    }
    Ok((hits, false))
}

// ---- operations --------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Mkdir { path: String },
    Rename { path: String, name: String },
    Move { paths: Vec<String>, to: String },
    Copy { paths: Vec<String>, to: String },
    Trash { paths: Vec<String> },
}

fn copy_rec(from: &Path, to: &Path) -> anyhow::Result<()> {
    let meta = fs::metadata(from)?;
    if meta.is_dir() {
        fs::create_dir_all(to)?;
        for e in fs::read_dir(from)?.flatten() {
            copy_rec(&e.path(), &to.join(e.file_name()))?;
        }
    } else {
        fs::copy(from, to).with_context(|| format!("copying {}", from.display()))?;
    }
    Ok(())
}

/// Runs one operation; returns a short summary.
pub fn run(root: &Path, op: Op) -> anyhow::Result<String> {
    match op {
        Op::Mkdir { path } => {
            let p = resolve(root, &path)?;
            check_name(&p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())?;
            if p.exists() {
                bail!("{path} already exists.");
            }
            fs::create_dir_all(&p)?;
            Ok(format!("Made {path}"))
        }
        Op::Rename { path, name } => {
            let p = resolve(root, &path)?;
            if p == root {
                bail!("Rename the server folder from its machine.");
            }
            let name = check_name(&name)?;
            let to = p.with_file_name(name);
            if to.exists() && !to.as_os_str().eq_ignore_ascii_case(p.as_os_str()) {
                bail!("{name} already exists here.");
            }
            fs::rename(&p, &to).with_context(|| format!("renaming {path} (is the server using it?)"))?;
            Ok(format!("Renamed to {name}"))
        }
        Op::Move { paths, to } => {
            let dest = resolve(root, &to)?;
            if !dest.is_dir() {
                bail!("{to} isn't a folder.");
            }
            let mut n = 0;
            for path in &paths {
                let p = resolve(root, path)?;
                if p == root || dest.starts_with(&p) {
                    bail!("Can't move {path} into itself.");
                }
                let name = p.file_name().context("bad path")?;
                let target = dest.join(name);
                if target == p {
                    continue;
                }
                if target.exists() {
                    bail!("{} already exists in {to}.", name.to_string_lossy());
                }
                fs::rename(&p, &target).with_context(|| format!("moving {path}"))?;
                n += 1;
            }
            Ok(format!("Moved {n}"))
        }
        Op::Copy { paths, to } => {
            let dest = resolve(root, &to)?;
            if !dest.is_dir() {
                bail!("{to} isn't a folder.");
            }
            for path in &paths {
                let p = resolve(root, path)?;
                if dest.starts_with(&p) {
                    bail!("Can't copy {path} into itself.");
                }
                let name = p.file_name().context("bad path")?.to_string_lossy().into_owned();
                copy_rec(&p, &free_name(&dest, &name))?;
            }
            Ok(format!("Copied {}", paths.len()))
        }
        Op::Trash { paths } => {
            for path in &paths {
                trash(root, path)?;
            }
            Ok(format!("Moved {} to the trash", paths.len()))
        }
    }
}

// ---- uploads -----------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Upload {
    pub id: String,
    /// Where it goes, relative to the server folder.
    pub path: String,
    pub size: u64,
    pub created: u64,
}

fn upload_dir(root: &Path) -> PathBuf {
    root.join(UPLOAD_DIR)
}

/// Starts an upload of `size` bytes to `rel`. Refuses to replace a file
/// unless `overwrite` (the replaced file goes to the trash on completion).
pub fn upload_start(root: &Path, rel: &str, size: u64, overwrite: bool) -> anyhow::Result<Upload> {
    let target = resolve(root, rel)?;
    for c in rel_of(root, &target).split('/') {
        check_name(c)?;
    }
    if target.is_dir() {
        bail!("{rel} is a folder.");
    }
    if target.exists() && !overwrite {
        bail!("{rel} already exists.");
    }
    let dir = upload_dir(root);
    fs::create_dir_all(&dir)?;
    // Old leftovers.
    if let Ok(rd) = fs::read_dir(&dir) {
        for e in rd.flatten() {
            let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > UPLOAD_KEEP);
            if old {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    let up = Upload { id: crate::auth::random_hex(12), path: rel_of(root, &target), size, created: now() };
    fs::write(dir.join(format!("{}.json", up.id)), serde_json::to_string(&up)?)?;
    fs::File::create(dir.join(format!("{}.part", up.id)))?;
    Ok(up)
}

fn upload_paths(root: &Path, id: &str) -> anyhow::Result<(PathBuf, PathBuf)> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("Unknown upload.");
    }
    let dir = upload_dir(root);
    Ok((dir.join(format!("{id}.json")), dir.join(format!("{id}.part"))))
}

/// The upload and how many bytes have arrived.
pub fn upload_status(root: &Path, id: &str) -> anyhow::Result<(Upload, u64)> {
    let (meta, part) = upload_paths(root, id)?;
    let up: Upload = serde_json::from_str(&fs::read_to_string(meta).context("That upload has expired. Start it again.")?)?;
    let have = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    Ok((up, have))
}

#[derive(Debug, Serialize)]
pub struct ChunkResult {
    pub offset: u64,
    pub done: bool,
}

/// Writes `data` at `offset`, which must be where the upload stands (so a
/// retried chunk can't leave a gap). Moves the file into place when full.
pub fn upload_chunk(root: &Path, id: &str, offset: u64, data: &[u8]) -> anyhow::Result<Result<ChunkResult, u64>> {
    let (up, have) = upload_status(root, id)?;
    if offset != have {
        return Ok(Err(have));
    }
    if offset + data.len() as u64 > up.size {
        bail!("More data than the upload's size.");
    }
    let (meta, part) = upload_paths(root, id)?;
    let mut f = fs::OpenOptions::new().write(true).open(&part)?;
    f.seek(SeekFrom::Start(offset))?;
    f.write_all(data)?;
    f.flush()?;
    drop(f);
    let offset = offset + data.len() as u64;
    if offset < up.size {
        return Ok(Ok(ChunkResult { offset, done: false }));
    }
    let target = resolve(root, &up.path)?;
    if target.exists() {
        trash(root, &up.path).with_context(|| format!("replacing {}", up.path))?;
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(&part, &target).with_context(|| format!("saving {}", up.path))?;
    let _ = fs::remove_file(meta);
    Ok(Ok(ChunkResult { offset, done: true }))
}

pub fn upload_cancel(root: &Path, id: &str) -> anyhow::Result<()> {
    let (meta, part) = upload_paths(root, id)?;
    let _ = fs::remove_file(part);
    let _ = fs::remove_file(meta);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopperd-files-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn paths_stay_inside() {
        let root = temp("paths");
        fs::create_dir_all(root.join("config")).unwrap();
        assert_eq!(resolve(&root, "config/a.toml").unwrap(), root.join("config").join("a.toml"));
        assert_eq!(resolve(&root, "/config\\a.toml").unwrap(), root.join("config").join("a.toml"));
        assert_eq!(resolve(&root, "").unwrap(), root);
        for bad in ["../x", "config/../../x", "C:/Windows", "c:\\x", "..", "a/../.."] {
            assert!(resolve(&root, bad).is_err(), "{bad}");
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn junctions_out_are_refused() {
        let root = temp("junction");
        let outside = temp("outside");
        let st = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(root.join("link"))
            .arg(&outside)
            .output()
            .unwrap();
        assert!(st.status.success());
        assert!(resolve(&root, "link/secret.txt").is_err());
        assert!(list(&root, "link").is_err());
        let _ = fs::remove_dir(root.join("link"));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn list_hides_internal_and_sorts_folders_first() {
        let root = temp("list");
        fs::create_dir_all(root.join("mods")).unwrap();
        fs::create_dir_all(root.join(TRASH_DIR)).unwrap();
        fs::write(root.join("b.txt"), "b").unwrap();
        fs::write(root.join("A.txt"), "aa").unwrap();
        let names: Vec<String> = list(&root, "").unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["mods", "A.txt", "b.txt"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn save_validates_detects_conflicts_and_keeps_versions() {
        let root = temp("text");
        fs::create_dir_all(root.join("config")).unwrap();
        fs::write(root.join("config/c.toml"), "a = 1\n").unwrap();
        let t = read_text(&root, "config/c.toml").unwrap();
        let bad = write_text(&root, "config/c.toml", "a = \n", Some(t.modified), false).unwrap();
        assert!(matches!(bad, SaveResult::Invalid { ref error } if error.contains("line 1")), "{bad:?}");
        let ok = write_text(&root, "config/c.toml", "a = 2\n", Some(t.modified), false).unwrap();
        assert!(matches!(ok, SaveResult::Saved { .. }));
        // Stale editor: conflict, unless forced.
        let stale = write_text(&root, "config/c.toml", "a = 3\n", Some(1), false).unwrap();
        assert!(matches!(stale, SaveResult::Conflict { .. }));
        assert_eq!(fs::read_to_string(root.join("config/c.toml")).unwrap(), "a = 2\n");
        let versions = list_trash(&root).unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!((versions[0].kind.as_str(), versions[0].path.as_str()), ("saved-over", "config/c.toml"));
        // Restoring the version puts "a = 1" back and keeps "a = 2".
        restore(&root, &versions[0].id).unwrap();
        assert_eq!(fs::read_to_string(root.join("config/c.toml")).unwrap(), "a = 1\n");
        assert_eq!(list_trash(&root).unwrap().len(), 1);
        assert!(validate("x.json", "{\"a\": }").unwrap_err().contains("line 1"));
        fs::write(root.join("bin.dat"), [0u8, 1, 2]).unwrap();
        assert!(read_text(&root, "bin.dat").is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn search_names_and_text() {
        let root = temp("search");
        fs::create_dir_all(root.join("config/sub")).unwrap();
        fs::create_dir_all(root.join("mods")).unwrap();
        fs::write(root.join("config/create.toml"), "a = 1\nmaxBeltLength = 20\n").unwrap();
        fs::write(root.join("config/sub/other.json"), "{\"maxbeltlength\": 5}").unwrap();
        fs::write(root.join("mods/belt.jar"), [0u8, 1, 2]).unwrap();
        let (h, more) = search(&root, "", "maxBeltLength", false, 100).unwrap();
        assert!(!more);
        let got: Vec<(String, usize)> = h.iter().map(|x| (x.path.clone(), x.line)).collect();
        assert!(got.contains(&("config/create.toml".into(), 2)) && got.contains(&("config/sub/other.json".into(), 1)), "{got:?}");
        let (h, _) = search(&root, "", "belt", false, 100).unwrap();
        assert!(h.iter().any(|x| x.path == "mods/belt.jar" && x.line == 0), "names match; jars aren't read");
        let (h, _) = search(&root, "config", r"max\w+ = \d+", true, 100).unwrap();
        assert_eq!(h.len(), 1);
        assert!(search(&root, "", "(", true, 10).is_err());
        let (h, more) = search(&root, "", "a", false, 2).unwrap();
        assert!(more && h.len() == 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn ops_and_trash_roundtrip() {
        let root = temp("ops");
        run(&root, Op::Mkdir { path: "mods/disabled".into() }).unwrap();
        fs::write(root.join("mods/a.jar"), "jar").unwrap();
        run(&root, Op::Copy { paths: vec!["mods/a.jar".into()], to: "mods".into() }).unwrap();
        assert!(root.join("mods/a (2).jar").exists());
        run(&root, Op::Move { paths: vec!["mods/a (2).jar".into()], to: "mods/disabled".into() }).unwrap();
        assert!(root.join("mods/disabled/a (2).jar").exists());
        run(&root, Op::Rename { path: "mods/disabled/a (2).jar".into(), name: "b.jar".into() }).unwrap();
        assert!(run(&root, Op::Rename { path: "mods/a.jar".into(), name: "../x".into() }).is_err());
        assert!(run(&root, Op::Move { paths: vec!["mods".into()], to: "mods/disabled".into() }).is_err());
        run(&root, Op::Trash { paths: vec!["mods/disabled".into()] }).unwrap();
        assert!(!root.join("mods/disabled").exists());
        let t = list_trash(&root).unwrap();
        assert_eq!((t.len(), t[0].dir, t[0].size), (1, true, 3));
        assert_eq!(restore(&root, &t[0].id).unwrap(), "mods/disabled");
        assert!(root.join("mods/disabled/b.jar").exists());
        run(&root, Op::Trash { paths: vec!["mods/a.jar".into()] }).unwrap();
        assert_eq!(purge(&root, &[]).unwrap(), 1);
        assert!(list_trash(&root).unwrap().is_empty());
        assert!(trash(&root, "").is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn uploads_resume_and_replace_safely() {
        let root = temp("upload");
        fs::create_dir_all(root.join("mods")).unwrap();
        fs::write(root.join("mods/x.jar"), "old").unwrap();
        assert!(upload_start(&root, "mods/x.jar", 6, false).is_err());
        let up = upload_start(&root, "mods/x.jar", 6, true).unwrap();
        assert!(matches!(upload_chunk(&root, &up.id, 0, b"abc").unwrap(), Ok(ChunkResult { offset: 3, done: false })));
        // A retried chunk at the wrong offset is told where to continue.
        assert_eq!(upload_chunk(&root, &up.id, 0, b"abc").unwrap().unwrap_err(), 3);
        assert_eq!(upload_status(&root, &up.id).unwrap().1, 3);
        assert!(upload_chunk(&root, &up.id, 3, b"defg").is_err(), "too much data");
        assert!(matches!(upload_chunk(&root, &up.id, 3, b"def").unwrap(), Ok(ChunkResult { done: true, .. })));
        assert_eq!(fs::read_to_string(root.join("mods/x.jar")).unwrap(), "abcdef");
        // The replaced jar is in the trash, the upload's files are gone.
        assert_eq!(list_trash(&root).unwrap()[0].path, "mods/x.jar");
        assert!(upload_status(&root, &up.id).is_err());
        // Folder uploads make their folders.
        let up = upload_start(&root, "shaderpacks/pack/a.txt", 1, false).unwrap();
        upload_chunk(&root, &up.id, 0, b"z").unwrap().unwrap();
        assert!(root.join("shaderpacks/pack/a.txt").exists());
        assert!(upload_start(&root, "../evil", 1, false).is_err());
        assert!(upload_status(&root, "../../x").is_err());
        let _ = fs::remove_dir_all(&root);
    }
}
