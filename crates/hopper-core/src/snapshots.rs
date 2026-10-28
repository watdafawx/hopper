//! Snapshots: an instance's mods, configs and Hopper's metadata at one
//! moment, to compare with now and put back. Taken before bulk updates, pack
//! updates and config resets, or by hand. Worlds are never part of them.
//!
//! Stored in `.hopper-backups/snapshots/`: one `<id>.json` manifest per
//! snapshot, and each file's content once, by its SHA-1, in `blobs/`. Jars
//! are hard-linked there (updates replace them, never edit them in place);
//! configs and metadata are copied, since they're edited in place.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Under the instance's backups folder.
pub const DIR: &str = "snapshots";
/// Automatic snapshots kept per instance; manual ones stay until deleted.
pub const KEEP_AUTO: usize = 10;
/// Folders a snapshot holds, whole.
const FOLDERS: &[&str] = &["mods", "config"];
/// Metadata files at the instance root. Restore never deletes these.
const META: &[&str] = &[crate::instances::HOPPER_FILE, "minecraftinstance.json", "manifest.json"];

/// One snapshot job at a time: they share blobs.
static LOCK: Mutex<()> = Mutex::new(());

/// Why a snapshot was taken.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reason {
    Manual,
    /// Update all, or the update review sheet.
    BulkUpdate { count: usize },
    PackUpdate { from: String, to: String },
    /// A mod's config reset; `name` is the mod.
    ConfigReset { name: String },
    /// The safety snapshot a restore takes first; `of` is what was restored.
    BeforeRestore { of: String },
}

impl Reason {
    pub fn label(&self) -> String {
        match self {
            Reason::Manual => "Snapshot".into(),
            Reason::BulkUpdate { count: 1 } => "Before updating 1 mod".into(),
            Reason::BulkUpdate { count } => format!("Before updating {count} mods"),
            Reason::PackUpdate { from, to } if from.is_empty() => format!("Before pack update to {to}"),
            Reason::PackUpdate { from, to } => format!("Before pack update {from} → {to}"),
            Reason::ConfigReset { name } => format!("Before config reset: {name}"),
            Reason::BeforeRestore { of } => format!("Before restoring \"{of}\""),
        }
    }

    /// Taken by Hopper, so pruned after [`KEEP_AUTO`].
    pub fn is_auto(&self) -> bool {
        !matches!(self, Reason::Manual)
    }
}

/// One file in a snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Relative to the instance, with `/` ("mods/jei.jar").
    pub rel: String,
    pub size: u64,
    /// Modified time in ms, to skip hashing unchanged files next time.
    #[serde(default)]
    pub mtime: i64,
    pub hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    /// The manifest's file stem (ms since the epoch).
    #[serde(skip)]
    pub id: String,
    /// Seconds since the epoch.
    pub when: i64,
    pub reason: Reason,
    /// A name the user gave it; empty uses the reason.
    #[serde(default)]
    pub name: String,
    /// Bytes this snapshot added to the store (files no other one had).
    #[serde(default)]
    pub new_bytes: u64,
    pub files: Vec<Entry>,
}

impl Snapshot {
    pub fn label(&self) -> String {
        if self.name.is_empty() { self.reason.label() } else { self.name.clone() }
    }

    /// Total size of its files.
    pub fn size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// Jars in `mods/`.
    pub fn mods(&self) -> usize {
        self.files.iter().filter(|f| is_mod(&f.rel)).count()
    }

    pub fn configs(&self) -> usize {
        self.files.iter().filter(|f| f.rel.starts_with("config/")).count()
    }
}

/// How a file or mod differs now from the snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// There now, not in the snapshot: a restore removes it.
    Added,
    /// In the snapshot, gone now: a restore puts it back.
    Removed,
    /// Another version or content now: a restore takes it back.
    Changed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModChange {
    pub name: String,
    /// Its CurseForge project (0 when unknown) and icon URL, for the list's
    /// icon and its mod page.
    pub project: u64,
    pub icon: String,
    /// File name in the snapshot.
    pub then: Option<String>,
    /// File name now.
    pub now: Option<String>,
}

impl ModChange {
    pub fn kind(&self) -> Kind {
        match (&self.then, &self.now) {
            (None, _) => Kind::Added,
            (_, None) => Kind::Removed,
            _ => Kind::Changed,
        }
    }

    /// Short "then" and "now" for a changed mod: versions when the file
    /// names have them, else the names. "(off)" marks a disabled file.
    pub fn versions(&self) -> (String, String) {
        let then = self.then.as_deref().unwrap_or_default();
        let now = self.now.as_deref().unwrap_or_default();
        let (vt, vn) = (version(then), version(now));
        if !vt.is_empty() && !vn.is_empty() && (vt != vn || then != now) {
            (vt, vn)
        } else {
            (then.to_string(), now.to_string())
        }
    }
}

fn version(file: &str) -> String {
    let (base, off) = match file.strip_suffix(".disabled") {
        Some(b) => (b, " (off)"),
        None => (file, ""),
    };
    let v = crate::scan::version_from_file_name(base);
    if v.is_empty() { v } else { format!("{v}{off}") }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileChange {
    pub rel: String,
    pub kind: Kind,
}

/// A snapshot against the instance now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes {
    pub mods: Vec<ModChange>,
    /// Configs and other files in the snapshot's folders.
    pub files: Vec<FileChange>,
    /// hopper.json or the CurseForge files differ.
    pub meta: bool,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.mods.is_empty() && self.files.is_empty() && !self.meta
    }

    pub fn mods_of(&self, k: Kind) -> usize {
        self.mods.iter().filter(|m| m.kind() == k).count()
    }

    /// Files a restore touches (mods and others, not metadata).
    pub fn total(&self) -> usize {
        self.mods.len() + self.files.len()
    }
}

fn base(root: &Path) -> PathBuf {
    root.join(crate::sync::BACKUP_DIR).join(DIR)
}

fn blob(base: &Path, hash: &str) -> PathBuf {
    base.join("blobs").join(&hash[..2.min(hash.len())]).join(hash)
}

fn is_mod(rel: &str) -> bool {
    rel.strip_prefix("mods/").is_some_and(|n| {
        let n = n.to_ascii_lowercase();
        !n.contains('/') && (n.ends_with(".jar") || n.ends_with(".jar.disabled"))
    })
}

fn is_meta(rel: &str) -> bool {
    META.contains(&rel)
}

fn mtime_ms(m: &fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn hash_file(p: &Path) -> std::io::Result<String> {
    let mut f = fs::File::open(p)?;
    let mut h = sha1_smol::Sha1::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.digest().to_string())
}

/// Copies `from` to `to`, hashing what was copied. Returns (hash, size).
fn copy_hashing(from: &Path, to: &Path) -> std::io::Result<(String, u64)> {
    let mut f = fs::File::open(from)?;
    let mut out = fs::File::create(to)?;
    let mut h = sha1_smol::Sha1::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut size = 0;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        out.write_all(&buf[..n])?;
        size += n as u64;
    }
    out.flush()?;
    Ok((h.digest().to_string(), size))
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf, fs::Metadata)>) {
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let Ok(m) = e.metadata() else { continue };
        if m.is_dir() {
            walk(root, &p, out);
        } else if m.is_file() {
            let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
            out.push((rel, p, m));
        }
    }
}

/// Files in the snapshot folders that aren't settings, left out of
/// snapshots (so a restore leaves them alone too): the spark profiler's
/// output in `config/spark` (thousands of files), all but its config.
fn skipped(rel: &str) -> bool {
    rel.strip_prefix("config/spark/").is_some_and(|r| r != "config.json")
}

/// The instance's snapshotted files as they are now: (rel, path, metadata).
fn current(root: &Path) -> Vec<(String, PathBuf, fs::Metadata)> {
    let mut out = Vec::new();
    for f in FOLDERS {
        walk(root, &root.join(f), &mut out);
    }
    out.retain(|(rel, _, _)| !skipped(rel));
    for m in META {
        let p = root.join(m);
        if let Ok(md) = fs::metadata(&p)
            && md.is_file()
        {
            out.push((m.to_string(), p, md));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

type Cache = HashMap<(String, u64, i64), String>;

/// Hashes already known, from every manifest, by (rel, size, mtime).
fn cache(snaps: &[Snapshot]) -> Cache {
    let mut c = Cache::new();
    for s in snaps {
        for f in &s.files {
            if f.mtime != 0 {
                c.entry((f.rel.clone(), f.size, f.mtime)).or_insert_with(|| f.hash.clone());
            }
        }
    }
    c
}

/// Every snapshot of an instance, newest first.
pub fn list(root: &Path) -> Vec<Snapshot> {
    let mut out: Vec<Snapshot> = fs::read_dir(base(root))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(".json")?;
            id.parse::<i64>().ok()?;
            let mut s: Snapshot = serde_json::from_slice(&fs::read(e.path()).ok()?).ok()?;
            s.id = id.to_string();
            Some(s)
        })
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.id.parse::<i64>().unwrap_or(0)));
    out
}

fn write_manifest(base: &Path, s: &Snapshot) -> anyhow::Result<()> {
    let p = base.join(format!("{}.json", s.id));
    let tmp = base.join(format!("{}.json.tmp", s.id));
    fs::write(&tmp, serde_json::to_vec_pretty(s)?)?;
    fs::rename(&tmp, &p)?;
    Ok(())
}

/// Snapshots the instance now. Safe to call from a background job; heavy
/// the first time (hashes and links every jar), quick after.
pub fn take(root: &Path, reason: Reason) -> anyhow::Result<Snapshot> {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let s = take_locked(root, reason, &|_, _| {})?;
    prune_locked(root);
    Ok(s)
}

/// Snapshots taken inside a job: (job id, instance root, snapshot id), the
/// newest last, so the task can open it.
static BY_JOB: Mutex<Vec<(u64, PathBuf, String)>> = Mutex::new(Vec::new());

/// [`take`] inside a running job, showing its progress there. The task can
/// then open it (see [`of_job`]).
pub fn take_in(root: &Path, reason: Reason, job: &crate::jobs::Job) -> anyhow::Result<Snapshot> {
    job.detail("Saving a snapshot to undo this");
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let s = take_locked(root, reason, &|i, n| {
        job.progress(i, n);
        job.detail(format!("Saving a snapshot to undo this: {i} of {n} files"));
    })?;
    prune_locked(root);
    let mut by = BY_JOB.lock().unwrap_or_else(|e| e.into_inner());
    by.push((job.id(), root.to_path_buf(), s.id.clone()));
    let extra = by.len().saturating_sub(64);
    by.drain(..extra);
    Ok(s)
}

/// The snapshot job `id` took: its instance root and snapshot id.
pub fn of_job(id: u64) -> Option<(PathBuf, String)> {
    let by = BY_JOB.lock().unwrap_or_else(|e| e.into_inner());
    by.iter().rev().find(|(j, ..)| *j == id).map(|(_, r, s)| (r.clone(), s.clone()))
}

/// How many snapshots an instance has, without reading them.
pub fn count(root: &Path) -> usize {
    fs::read_dir(base(root))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().strip_suffix(".json").is_some_and(|id| id.parse::<i64>().is_ok()))
        .count()
}

/// Hashes one file (or takes its known hash) and stores it in the blobs
/// when no snapshot has it yet. `tmp` is this thread's own copy target.
#[allow(clippy::too_many_arguments)]
fn store_one(
    base: &Path,
    known: &Cache,
    tmp: &Path,
    rel: &str,
    path: &Path,
    md: &fs::Metadata,
    fresh_after: i64,
    new_bytes: &std::sync::atomic::AtomicU64,
) -> anyhow::Result<Entry> {
    use anyhow::Context as _;
    use std::sync::atomic::Ordering;
    let key = (rel.to_string(), md.len(), mtime_ms(md));
    let hash = match known.get(&key).filter(|_| key.2 < fresh_after) {
        Some(h) => h.clone(),
        None => hash_file(path).with_context(|| format!("reading {rel}"))?,
    };
    let mut entry = Entry { rel: rel.to_string(), size: md.len(), mtime: key.2, hash };
    let dest = blob(base, &entry.hash);
    if dest.exists() {
        return Ok(entry);
    }
    fs::create_dir_all(dest.parent().unwrap_or(base))?;
    // Jars: a hard link, if it still matches what was hashed.
    if is_mod(rel) && fs::hard_link(path, &dest).is_ok() {
        if fs::metadata(&dest).is_ok_and(|m| m.len() == entry.size && mtime_ms(&m) == entry.mtime) {
            new_bytes.fetch_add(entry.size, Ordering::Relaxed);
            return Ok(entry);
        }
        // Changed since it was hashed: this link isn't that content.
        let _ = fs::remove_file(&dest);
    }
    let (h, size) = copy_hashing(path, tmp).with_context(|| format!("copying {rel}"))?;
    // Changed while being read: keep what was copied.
    entry.hash = h;
    entry.size = size;
    let dest = blob(base, &entry.hash);
    fs::create_dir_all(dest.parent().unwrap_or(base))?;
    if dest.exists() {
        let _ = fs::remove_file(tmp);
    } else {
        fs::rename(tmp, &dest)?;
        new_bytes.fetch_add(size, Ordering::Relaxed);
    }
    Ok(entry)
}

fn take_locked(root: &Path, reason: Reason, progress: &dyn Fn(usize, usize)) -> anyhow::Result<Snapshot> {
    let base = base(root);
    fs::create_dir_all(base.join("blobs"))?;
    let known = cache(&list(root));
    let all = current(root);
    let total = all.len();
    // A file changed within the last moments can change again without its
    // size or time moving (same millisecond), so it's always read.
    let fresh_after = now_ms().saturating_sub(2000);
    // Files are read and stored by several threads: a first snapshot reads
    // thousands of files. Progress is shown from this thread.
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let new_bytes = AtomicU64::new(0);
    let failed = AtomicBool::new(false);
    let slots: Vec<Mutex<Option<anyhow::Result<Entry>>>> = (0..total).map(|_| Mutex::new(None)).collect();
    let workers = std::env::var("HOPPER_SNAPSHOT_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()))
        .clamp(1, 8);
    std::thread::scope(|sc| {
        for w in 0..workers {
            let (all, known, base, slots) = (&all, &known, &base, &slots);
            let (next, done, new_bytes, failed) = (&next, &done, &new_bytes, &failed);
            sc.spawn(move || {
                let tmp = base.join("blobs").join(format!("incoming-{}-{w}", std::process::id()));
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= all.len() || failed.load(Ordering::Relaxed) {
                        break;
                    }
                    let (rel, path, md) = &all[i];
                    let r = store_one(base, known, &tmp, rel, path, md, fresh_after, new_bytes);
                    if r.is_err() {
                        failed.store(true, Ordering::Relaxed);
                    }
                    *slots[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                    done.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
        loop {
            let d = done.load(Ordering::Relaxed);
            progress(d, total);
            if d >= total || failed.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    });
    let mut files = Vec::with_capacity(total);
    for slot in slots {
        match slot.into_inner().unwrap_or_else(|e| e.into_inner()) {
            Some(Ok(e)) => files.push(e),
            Some(Err(e)) => return Err(e),
            None => anyhow::bail!("The snapshot stopped partway"),
        }
    }
    let new_bytes = new_bytes.into_inner();
    let mut ms = now_ms();
    while base.join(format!("{ms}.json")).exists() {
        ms += 1;
    }
    let s = Snapshot {
        id: ms.to_string(),
        when: ms / 1000,
        reason,
        name: String::new(),
        new_bytes,
        files,
    };
    write_manifest(&base, &s)?;
    Ok(s)
}

/// Drops automatic snapshots past [`KEEP_AUTO`], then unused blobs.
fn prune_locked(root: &Path) {
    let base = base(root);
    for s in list(root).iter().filter(|s| s.reason.is_auto()).skip(KEEP_AUTO) {
        let _ = fs::remove_file(base.join(format!("{}.json", s.id)));
    }
    gc_locked(root);
}

/// Removes blobs no manifest uses, and leftovers of a crashed copy.
fn gc_locked(root: &Path) {
    let used: HashSet<String> = list(root).into_iter().flat_map(|s| s.files.into_iter().map(|f| f.hash)).collect();
    let blobs = base(root).join("blobs");
    for d in fs::read_dir(&blobs).into_iter().flatten().flatten() {
        let p = d.path();
        if !p.is_dir() {
            let _ = fs::remove_file(&p);
            continue;
        }
        for f in fs::read_dir(&p).into_iter().flatten().flatten() {
            if !used.contains(&*f.file_name().to_string_lossy()) {
                let _ = fs::remove_file(f.path());
            }
        }
        let _ = fs::remove_dir(&p);
    }
}

/// Deletes a snapshot and the files only it used.
pub fn delete(root: &Path, id: &str) -> anyhow::Result<()> {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    fs::remove_file(base(root).join(format!("{id}.json")))?;
    gc_locked(root);
    Ok(())
}

/// Names a snapshot; an empty name goes back to its reason. A named
/// automatic snapshot stays automatic.
pub fn rename(root: &Path, id: &str, name: &str) -> anyhow::Result<()> {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = list(root).into_iter().find(|s| s.id == id).ok_or_else(|| anyhow::anyhow!("That snapshot is gone"))?;
    s.name = name.trim().to_string();
    write_manifest(&base(root), &s)
}

/// Disk the snapshot store takes (jars hard-linked to the instance's own
/// count too).
pub fn used_bytes(root: &Path) -> u64 {
    let mut out = Vec::new();
    let b = base(root).join("blobs");
    walk(&b, &b, &mut out);
    out.iter().map(|(_, _, m)| m.len()).sum()
}

/// Hashes of the instance's files now, by rel.
fn now_hashes(root: &Path, known: &Cache) -> anyhow::Result<HashMap<String, String>> {
    use anyhow::Context as _;
    let mut out = HashMap::new();
    for (rel, path, md) in current(root) {
        let h = match known.get(&(rel.clone(), md.len(), mtime_ms(&md))) {
            Some(h) => h.clone(),
            None => hash_file(&path).with_context(|| format!("reading {rel}"))?,
        };
        out.insert(rel, h);
    }
    Ok(out)
}

/// File name (lowercase, no `.disabled`) → (identity, display name), from
/// a hopper.json's addons.
/// Every CurseForge project the instance has had, by file name (lowercase,
/// no `.disabled`) → (project, icon URL): its hopper.json now and the one in
/// each snapshot, newest first. For backed-up files, which keep no project.
pub fn projects_seen(root: &Path) -> HashMap<String, (u64, String)> {
    let base = base(root);
    let mut jsons: Vec<Vec<u8>> = fs::read(root.join(crate::instances::HOPPER_FILE)).into_iter().collect();
    let mut seen = HashSet::new();
    for snap in list(root) {
        if let Some(f) = snap.files.iter().find(|f| f.rel == crate::instances::HOPPER_FILE)
            && seen.insert(f.hash.clone())
            && let Ok(b) = fs::read(blob(&base, &f.hash))
        {
            jsons.push(b);
        }
    }
    let mut out = HashMap::new();
    for b in jsons {
        let Ok(inst) = serde_json::from_slice::<crate::instances::Instance>(&b) else { continue };
        for a in inst.addons.into_iter().filter(|a| a.project_id != 0) {
            let key = a.file_name.to_lowercase().trim_end_matches(".disabled").to_string();
            out.entry(key).or_insert((a.project_id, a.icon));
        }
    }
    out
}

/// Identity → (project, icon URL), from a hopper.json's addons.
fn addon_projects(json: Option<&[u8]>) -> HashMap<String, (u64, String)> {
    let Some(inst) = json.and_then(|b| serde_json::from_slice::<crate::instances::Instance>(b).ok()) else {
        return HashMap::new();
    };
    inst.addons
        .into_iter()
        .filter(|a| a.project_id != 0)
        .map(|a| (format!("p:{}", a.project_id), (a.project_id, a.icon)))
        .collect()
}

fn addon_names(json: Option<Vec<u8>>) -> HashMap<String, (String, String)> {
    let Some(inst) = json.and_then(|b| serde_json::from_slice::<crate::instances::Instance>(&b).ok()) else {
        return HashMap::new();
    };
    inst.addons
        .into_iter()
        .map(|a| {
            let key = if a.project_id != 0 {
                format!("p:{}", a.project_id)
            } else if !a.mr_project.is_empty() {
                format!("m:{}", a.mr_project)
            } else {
                format!("f:{}", crate::scan::strip_version_suffix(&a.file_name.to_lowercase()))
            };
            (a.file_name.to_lowercase(), (key, a.name))
        })
        .collect()
}

fn mod_key(file: &str, names: &HashMap<String, (String, String)>) -> (String, String) {
    let lower = file.to_lowercase();
    let bare = lower.strip_suffix(".disabled").unwrap_or(&lower);
    if let Some((k, n)) = names.get(bare) {
        return (k.clone(), n.clone());
    }
    let stem = crate::scan::strip_version_suffix(bare);
    let shown = file.strip_suffix(".disabled").unwrap_or(file);
    let shown = crate::scan::strip_version_suffix(shown);
    (format!("f:{stem}"), shown)
}

/// What differs between `snap` and the instance now.
pub fn compare(root: &Path, snap: &Snapshot) -> anyhow::Result<Changes> {
    let known = cache(std::slice::from_ref(snap)).into_iter().chain(cache(&list(root))).collect();
    let now = now_hashes(root, &known)?;
    // Older snapshots still hold what's left out now: not a change.
    let then: HashMap<&str, &str> =
        snap.files.iter().filter(|f| !skipped(&f.rel)).map(|f| (f.rel.as_str(), f.hash.as_str())).collect();
    let base = base(root);
    let hopper = crate::instances::HOPPER_FILE;
    let json_then = then.get(hopper).and_then(|h| fs::read(blob(&base, h)).ok());
    let json_now = fs::read(root.join(hopper)).ok();
    let mut projects = addon_projects(json_then.as_deref());
    projects.extend(addon_projects(json_now.as_deref()));
    let names_then = addon_names(json_then);
    let names_now = addon_names(json_now);

    let mut out = Changes::default();
    // Mods, matched by project so a new version reads as a change.
    let mut by_key: HashMap<String, (String, Vec<String>, Vec<String>)> = HashMap::new();
    for (rel, h) in &then {
        if is_mod(rel) && now.get(*rel).map(String::as_str) != Some(h) {
            let file = rel["mods/".len()..].to_string();
            let (k, n) = mod_key(&file, &names_then);
            by_key.entry(k).or_insert_with(|| (n, vec![], vec![])).1.push(file);
        }
    }
    for (rel, h) in &now {
        if is_mod(rel) && then.get(rel.as_str()) != Some(&h.as_str()) {
            let file = rel["mods/".len()..].to_string();
            let (k, n) = mod_key(&file, &names_now);
            let e = by_key.entry(k).or_insert_with(|| (n.clone(), vec![], vec![]));
            if e.0.is_empty() {
                e.0 = n;
            }
            e.2.push(file);
        }
    }
    for (key, (name, mut t, mut n)) in by_key {
        let (project, icon) = projects.get(&key).cloned().unwrap_or_default();
        t.sort();
        n.sort();
        let len = t.len().max(n.len());
        for i in 0..len {
            out.mods.push(ModChange {
                name: name.clone(),
                project,
                icon: icon.clone(),
                then: t.get(i).cloned(),
                now: n.get(i).cloned(),
            });
        }
    }
    out.mods.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.then.cmp(&b.then)));

    for (rel, h) in &then {
        if is_mod(rel) {
            continue;
        }
        let kind = match now.get(*rel) {
            None => Kind::Removed,
            Some(n) if n != h => Kind::Changed,
            Some(_) => continue,
        };
        if is_meta(rel) {
            out.meta = true;
        } else {
            out.files.push(FileChange { rel: rel.to_string(), kind });
        }
    }
    for rel in now.keys() {
        if !is_mod(rel) && !then.contains_key(rel.as_str()) && !is_meta(rel) {
            out.files.push(FileChange { rel: rel.clone(), kind: Kind::Added });
        }
    }
    out.files.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

/// A restore's result.
#[derive(Clone, Debug)]
pub struct Restored {
    /// Taken first, to undo the restore, when the person asked for it.
    pub safety: Option<Snapshot>,
    /// Without that snapshot: where the files the restore took away went
    /// (in `.hopper-backups`), if any did.
    pub aside: Option<PathBuf>,
    /// Files removed or put back.
    pub changed: usize,
}

/// Puts the instance back as it was in snapshot `id`: mods, configs and
/// metadata (play time is kept). With `snapshot_first` (the person ticked
/// it) a "Before restoring" snapshot comes first, so this can be undone;
/// without it, the files it takes away or replaces are moved to
/// `.hopper-backups/restore-<time>` instead of deleted, as a mod update
/// does. Close the game first.
pub fn restore(root: &Path, id: &str, snapshot_first: bool) -> anyhow::Result<Restored> {
    use anyhow::Context as _;
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let base = base(root);
    let snap = list(root).into_iter().find(|s| s.id == id).ok_or_else(|| anyhow::anyhow!("That snapshot is gone"))?;
    // Every file must be there before anything is touched.
    for f in &snap.files {
        if !crate::configdiff::safe_rel(&f.rel) {
            anyhow::bail!("The snapshot names an odd path: {}", f.rel);
        }
        let ok = fs::metadata(blob(&base, &f.hash)).is_ok_and(|m| m.len() == f.size);
        if !ok {
            anyhow::bail!("The snapshot's copy of {} is missing or damaged, so nothing was changed", f.rel);
        }
    }
    let safety = if snapshot_first { Some(take_locked(root, Reason::BeforeRestore { of: snap.label() }, &|_, _| {})?) } else { None };
    let aside_dir = root.join(crate::sync::BACKUP_DIR).join(format!("restore-{}", now_ms()));
    let after = |e: anyhow::Error| match &safety {
        Some(s) => e.context(format!("Restore stopped partway. \"{}\" in Backups has the instance as it was", s.reason.label())),
        None => e.context(format!("Restore stopped partway. Files it took away are in {}", aside_dir.display())),
    };
    // What's there now, by hash: from the snapshot just taken, else read
    // (hashes known from earlier snapshots are reused).
    let now_owned: Vec<(String, String)> = match &safety {
        Some(s) => s.files.iter().map(|f| (f.rel.clone(), f.hash.clone())).collect(),
        None => {
            let known = cache(&list(root));
            let fresh_after = now_ms().saturating_sub(2000);
            let mut out = Vec::new();
            for (rel, path, md) in current(root) {
                let key = (rel.clone(), md.len(), mtime_ms(&md));
                let hash = match known.get(&key).filter(|_| key.2 < fresh_after) {
                    Some(h) => h.clone(),
                    None => hash_file(&path).with_context(|| format!("reading {rel}"))?,
                };
                out.push((rel, hash));
            }
            out
        }
    };
    let now: HashMap<&str, &str> = now_owned.iter().map(|(r, h)| (r.as_str(), h.as_str())).collect();
    let mut aside = None;
    let want: HashMap<&str, &str> =
        snap.files.iter().filter(|f| !skipped(&f.rel)).map(|f| (f.rel.as_str(), f.hash.as_str())).collect();
    let mut changed = 0;

    // Off: files not in the snapshot or different (metadata is replaced, not removed).
    let mut emptied = HashSet::new();
    for (rel, h) in &now {
        if is_meta(rel) || want.get(rel) == Some(h) {
            continue;
        }
        let p = root.join(rel);
        if safety.is_some() {
            fs::remove_file(&p).with_context(|| format!("removing {rel}")).map_err(after)?;
        } else {
            let to = aside_dir.join(rel);
            fs::create_dir_all(to.parent().unwrap_or(&aside_dir)).map_err(|e| after(e.into()))?;
            fs::rename(&p, &to).with_context(|| format!("moving {rel} aside")).map_err(after)?;
            aside = Some(aside_dir.clone());
        }
        if !want.contains_key(rel) {
            changed += 1;
        }
        if let Some(d) = p.parent() {
            emptied.insert(d.to_path_buf());
        }
    }
    // Folders that emptied go, up to the snapshot's folders.
    let tops: Vec<PathBuf> = FOLDERS.iter().map(|f| root.join(f)).collect();
    for d in emptied {
        let mut d = d;
        while !tops.contains(&d) && tops.iter().any(|t| d.starts_with(t)) && fs::remove_dir(&d).is_ok() {
            match d.parent() {
                Some(p) => d = p.to_path_buf(),
                None => break,
            }
        }
    }

    // Back: the snapshot's files that differ now.
    let hopper = crate::instances::HOPPER_FILE;
    let old_json: Option<serde_json::Value> =
        fs::read(root.join(hopper)).ok().and_then(|b| serde_json::from_slice(&b).ok());
    for f in snap.files.iter().filter(|f| !skipped(&f.rel)) {
        if now.get(f.rel.as_str()) == Some(&f.hash.as_str()) {
            continue;
        }
        changed += 1;
        let dest = root.join(&f.rel);
        if let Some(p) = dest.parent() {
            fs::create_dir_all(p).map_err(|e| after(e.into()))?;
        }
        let src = blob(&base, &f.hash);
        let linked = is_mod(&f.rel) && fs::hard_link(&src, &dest).is_ok();
        if !linked {
            let tmp = dest.with_file_name(format!(".{}.hopper-restore", dest.file_name().unwrap_or_default().to_string_lossy()));
            fs::copy(&src, &tmp).with_context(|| format!("putting back {}", f.rel)).map_err(after)?;
            fs::rename(&tmp, &dest).with_context(|| format!("putting back {}", f.rel)).map_err(after)?;
        }
    }
    if want.contains_key(hopper) {
        keep_stats(root, old_json);
    }
    Ok(Restored { safety, aside, changed })
}

/// After hopper.json came back: keeps play time from before, and marks
/// CurseForge's file as already read, so it isn't imported over the
/// restored one.
fn keep_stats(root: &Path, old: Option<serde_json::Value>) {
    let p = root.join(crate::instances::HOPPER_FILE);
    let Some(mut v) = fs::read(&p).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()) else {
        return;
    };
    let Some(obj) = v.as_object_mut() else { return };
    if let Some(old) = old.as_ref().and_then(|o| o.as_object()) {
        for k in ["play_seconds", "last_played"] {
            if let Some(x) = old.get(k) {
                obj.insert(k.into(), x.clone());
            }
        }
    }
    let cf = fs::metadata(root.join("minecraftinstance.json"))
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    if let Some(t) = cf {
        let had = obj.get("imported_from_cf").and_then(|x| x.as_i64()).unwrap_or(0);
        obj.insert("imported_from_cf".into(), had.max(t).into());
    }
    let tmp = p.with_extension("json.hopper-restore");
    if fs::write(&tmp, serde_json::to_vec_pretty(&v).unwrap_or_default()).is_ok() {
        let _ = fs::rename(&tmp, &p);
    }
}

/// "Today 14:02", "Yesterday 21:40", older ones as "3 days ago".
pub fn when_label(secs: i64) -> String {
    let rel = crate::time::relative_secs(secs);
    if rel == "Today" || rel == "Yesterday" {
        let t = (secs + local_offset()).rem_euclid(86_400);
        format!("{rel} {:02}:{:02}", t / 3600, t % 3600 / 60)
    } else {
        rel
    }
}

/// The local time zone's offset from UTC now, in seconds.
#[cfg(windows)]
fn local_offset() -> i64 {
    use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    let mut tz = TIME_ZONE_INFORMATION::default();
    // 2: daylight saving time is in effect.
    let id = unsafe { GetTimeZoneInformation(&mut tz) };
    let bias = tz.Bias + if id == 2 { tz.DaylightBias } else { tz.StandardBias };
    -(bias as i64) * 60
}

#[cfg(not(windows))]
fn local_offset() -> i64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("hopper-snap-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("mods")).unwrap();
        fs::create_dir_all(root.join("config/sub")).unwrap();
        root
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    /// Replaces a file the way updates do: a new file renamed over it.
    fn replace(root: &Path, rel: &str, body: &str) {
        let tmp = root.join("new.tmp");
        fs::write(&tmp, body).unwrap();
        fs::rename(&tmp, root.join(rel)).unwrap();
    }

    fn state(root: &Path) -> Vec<(String, String)> {
        current(root)
            .into_iter()
            .filter(|(r, _, _)| r != crate::instances::HOPPER_FILE)
            .map(|(r, p, _)| (r, fs::read_to_string(p).unwrap()))
            .collect()
    }

    fn hopper_json(root: &Path, addons: &[(u64, &str, &str)], play: u64) {
        let addons: Vec<_> = addons
            .iter()
            .map(|(id, file, name)| serde_json::json!({"project_id": id, "file_name": file, "name": name, "enabled": true}))
            .collect();
        let v = serde_json::json!({"name": "Test", "addons": addons, "play_seconds": play, "format": crate::instances::FORMAT});
        fs::write(root.join("hopper.json"), serde_json::to_vec(&v).unwrap()).unwrap();
    }

    fn blobs(root: &Path) -> usize {
        let mut out = Vec::new();
        let b = base(root).join("blobs");
        walk(&b, &b, &mut out);
        out.len()
    }

    /// Times a first and a second snapshot of the folder in
    /// `HOPPER_SNAPSHOT_ROOT` (a throwaway copy), then removes them.
    #[test]
    #[ignore]
    fn time_real_copy() {
        let Ok(root) = std::env::var("HOPPER_SNAPSHOT_ROOT") else { return };
        let root = PathBuf::from(root);
        let _ = fs::remove_dir_all(base(&root));
        let t = std::time::Instant::now();
        let s = take(&root, Reason::Manual).unwrap();
        println!("first: {} files in {:.1}s", s.files.len(), t.elapsed().as_secs_f64());
        let t = std::time::Instant::now();
        take(&root, Reason::Manual).unwrap();
        println!("second: {:.1}s", t.elapsed().as_secs_f64());
        let _ = fs::remove_dir_all(base(&root));
    }

    /// Times what opening Backups does on the instance in
    /// `HOPPER_SNAPSHOT_OPEN` (read only).
    #[test]
    #[ignore]
    fn time_open() {
        let Ok(root) = std::env::var("HOPPER_SNAPSHOT_OPEN") else { return };
        let root = PathBuf::from(root);
        let t = std::time::Instant::now();
        let l = list(&root);
        println!("list: {} in {:.0} ms", l.len(), t.elapsed().as_secs_f64() * 1000.);
        let t = std::time::Instant::now();
        let u = used_bytes(&root);
        println!("used_bytes: {u} in {:.0} ms", t.elapsed().as_secs_f64() * 1000.);
        let t = std::time::Instant::now();
        let c = compare(&root, &l[0]).unwrap();
        println!("compare: {} mods {} files in {:.0} ms", c.mods.len(), c.files.len(), t.elapsed().as_secs_f64() * 1000.);
        let t = std::time::Instant::now();
        let _ = current(&root);
        println!("walk: {:.0} ms", t.elapsed().as_secs_f64() * 1000.);
        let t = std::time::Instant::now();
        let b = crate::backups::list(&root);
        println!("backups::list: {} sets in {:.0} ms", b.len(), t.elapsed().as_secs_f64() * 1000.);
    }

    #[test]
    fn older_snapshots_with_spark_output_compare_clean() {
        let root = temp("spark-old");
        write(&root, "config/a.toml", "a=1");
        write(&root, "config/spark/mat/a.bin", "dump");
        let mut s = take(&root, Reason::Manual).unwrap();
        // As a snapshot from before spark was left out would have it.
        s.files.push(Entry { rel: "config/spark/mat/old.bin".into(), size: 3, mtime: 1, hash: "x".into() });
        let c = compare(&root, &s).unwrap();
        assert!(c.files.is_empty(), "{:?}", c.files.iter().map(|f| &f.rel).collect::<Vec<_>>());
    }

    #[test]
    fn leaves_out_spark_output() {
        let root = temp("spark");
        write(&root, "config/spark/config.json", "{}");
        write(&root, "config/spark/mat/a.bin", "dump");
        write(&root, "config/spark/p.sparkprofile", "prof");
        let s = take(&root, Reason::Manual).unwrap();
        let rels: Vec<&str> = s.files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, ["config/spark/config.json"]);
        // A restore doesn't touch what the snapshot left out.
        restore(&root, &s.id, false).unwrap();
        assert!(root.join("config/spark/mat/a.bin").is_file());
        assert!(root.join("config/spark/p.sparkprofile").is_file());
    }

    #[test]
    fn stores_each_file_once() {
        let root = temp("once");
        write(&root, "mods/jei-1.0.jar", "jei1");
        write(&root, "config/a.toml", "a=1");
        let s1 = take(&root, Reason::Manual).unwrap();
        assert_eq!(s1.files.len(), 2);
        assert_eq!(s1.new_bytes, 7);
        let s2 = take(&root, Reason::BulkUpdate { count: 3 }).unwrap();
        assert_eq!(s2.new_bytes, 0);
        assert_eq!(blobs(&root), 2);
        let l = list(&root);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].id, s2.id);
        assert_eq!(l[0].label(), "Before updating 3 mods");
        assert_eq!(l[0].mods(), 1);
        assert_eq!(l[0].configs(), 1);
        // Jars are linked, configs copied: editing a config in place
        // leaves the snapshot's copy alone.
        write(&root, "config/a.toml", "a=2");
        let h = &l[0].files.iter().find(|f| f.rel == "config/a.toml").unwrap().hash;
        assert_eq!(fs::read_to_string(blob(&base(&root), h)).unwrap(), "a=1");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn restores_exactly_and_can_be_undone() {
        let root = temp("restore");
        write(&root, "mods/jei-1.0.jar", "jei1");
        write(&root, "mods/gone-1.0.jar", "gone");
        write(&root, "config/a.toml", "a=1");
        write(&root, "config/sub/b.toml", "b=1");
        hopper_json(&root, &[(1, "jei-1.0.jar", "JEI")], 100);
        let snap = take(&root, Reason::PackUpdate { from: "1".into(), to: "2".into() }).unwrap();
        let before = state(&root);

        // The update: a new jar version, one removed, one added, configs edited.
        fs::remove_file(root.join("mods/jei-1.0.jar")).unwrap();
        write(&root, "mods/jei-2.0.jar", "jei2");
        fs::remove_file(root.join("mods/gone-1.0.jar")).unwrap();
        write(&root, "mods/new-1.0.jar", "new");
        replace(&root, "config/a.toml", "a=2");
        write(&root, "config/fresh/c.toml", "c=1");
        fs::remove_file(root.join("config/sub/b.toml")).unwrap();
        hopper_json(&root, &[(1, "jei-2.0.jar", "JEI"), (2, "new-1.0.jar", "New")], 500);
        let after = state(&root);

        let c = compare(&root, &snap).unwrap();
        assert_eq!(c.mods_of(Kind::Changed), 1);
        assert_eq!(c.mods_of(Kind::Added), 1);
        assert_eq!(c.mods_of(Kind::Removed), 1);
        let jei = c.mods.iter().find(|m| m.name == "JEI").unwrap();
        assert_eq!(jei.versions(), ("1.0".to_string(), "2.0".to_string()));
        assert_eq!(c.files.len(), 3);
        assert!(c.meta);

        let r = restore(&root, &snap.id, true).unwrap();
        assert_eq!(state(&root), before);
        assert!(!root.join("config/fresh").exists());
        let safety = r.safety.unwrap();
        assert!(matches!(safety.reason, Reason::BeforeRestore { .. }));
        assert!(r.aside.is_none());
        // Play time stays; the mod list is the snapshot's.
        let j: serde_json::Value = serde_json::from_slice(&fs::read(root.join("hopper.json")).unwrap()).unwrap();
        assert_eq!(j["play_seconds"], 500);
        assert_eq!(j["addons"].as_array().unwrap().len(), 1);
        assert!(compare(&root, &snap).unwrap().mods.is_empty());

        // And undone.
        restore(&root, &safety.id, true).unwrap();
        assert_eq!(state(&root), after);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn restore_without_a_snapshot_moves_what_it_takes_away_aside() {
        let root = temp("restore-aside");
        write(&root, "mods/jei-1.0.jar", "jei1");
        write(&root, "config/a.toml", "a=1");
        let snap = take(&root, Reason::Manual).unwrap();
        let before = state(&root);
        fs::remove_file(root.join("mods/jei-1.0.jar")).unwrap();
        write(&root, "mods/jei-2.0.jar", "jei2");
        replace(&root, "config/a.toml", "a=2");

        let r = restore(&root, &snap.id, false).unwrap();
        assert_eq!(state(&root), before);
        assert!(r.safety.is_none());
        // No new snapshot; what it took away is in .hopper-backups.
        assert_eq!(list(&root).len(), 1);
        let aside = r.aside.unwrap();
        assert!(aside.starts_with(root.join(crate::sync::BACKUP_DIR)));
        assert_eq!(fs::read_to_string(aside.join("mods/jei-2.0.jar")).unwrap(), "jei2");
        assert_eq!(fs::read_to_string(aside.join("config/a.toml")).unwrap(), "a=2");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn keeps_the_last_ten_automatic() {
        let root = temp("keep");
        write(&root, "mods/a.jar", "a");
        let manual = take(&root, Reason::Manual).unwrap();
        for i in 0..12 {
            write(&root, "config/n.toml", &format!("n={i}"));
            take(&root, Reason::ConfigReset { name: "X".into() }).unwrap();
        }
        let l = list(&root);
        assert_eq!(l.iter().filter(|s| s.reason.is_auto()).count(), KEEP_AUTO);
        assert!(l.iter().any(|s| s.id == manual.id));
        // The jar plus the ten configs still used.
        assert_eq!(blobs(&root), 1 + KEEP_AUTO);

        rename(&root, &manual.id, "Stable").unwrap();
        assert_eq!(list(&root).iter().find(|s| s.id == manual.id).unwrap().label(), "Stable");
        delete(&root, &manual.id).unwrap();
        assert_eq!(list(&root).len(), KEEP_AUTO);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_damaged_snapshot_changes_nothing() {
        let root = temp("damaged");
        write(&root, "mods/a.jar", "a");
        let s = take(&root, Reason::Manual).unwrap();
        // Replace the jar the way an update does: the linked copy is safe.
        replace(&root, "mods/a.jar", "b");
        assert_eq!(fs::read_to_string(blob(&base(&root), &s.files[0].hash)).unwrap(), "a");
        fs::remove_file(blob(&base(&root), &s.files[0].hash)).unwrap();
        assert!(restore(&root, &s.id, false).is_err());
        assert_eq!(fs::read_to_string(root.join("mods/a.jar")).unwrap(), "b");
        assert_eq!(list(&root).len(), 1);
        fs::remove_dir_all(&root).unwrap();
    }
}
