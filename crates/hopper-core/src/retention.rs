//! How long `.hopper-backups` keeps what Hopper set aside: the per-action
//! folders (`<time>/`, `restore-<time>/`, `undo-<time>/`) that hold the old
//! files behind an Undo, and automatic snapshots. Shared by Hopper and
//! Hopper Server, which keeps the same folder in each server.
//!
//! Kept: the last [`Retention::keep_actions`] actions and everything newer
//! than [`Retention::keep_days`] (either is enough), always the newest
//! action and anything from the last few minutes. With a size limit set,
//! the oldest of the rest go until the folder fits. Snapshots the user took
//! or named, world zips and worlds moved aside before a restore are never
//! touched, and nothing is deleted outright: what goes, goes to the Recycle
//! Bin (Hopper Server: the server's trash).
//!
//! Two ways it runs:
//! - Clean up (the user's button): everything past the rules goes.
//! - Right after a new backup: what that backup (or time) pushed past the
//!   rules. What was already past them when this version first pruned the
//!   folder is a backlog left for Clean up, so it is never cleared without
//!   the user asking.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Retention {
    /// The last this many actions stay, however old.
    pub keep_actions: u32,
    /// Actions newer than this stay, however many.
    pub keep_days: u32,
    /// Past this (MB) the oldest go even if the rules above would keep
    /// them; 0 is no limit.
    pub max_mb: u64,
}

impl Default for Retention {
    fn default() -> Self {
        Retention { keep_actions: 30, keep_days: 30, max_mb: 0 }
    }
}

/// Undo toasts and running jobs still point at fresh backups: nothing this
/// young goes, whatever the rules.
const GRACE_SECS: i64 = 10 * 60;

/// In `.hopper-backups`: the backlog automatic pruning leaves alone (see
/// the module docs).
const MARKER: &str = ".retention.json";

static POLICY: RwLock<Option<Retention>> = RwLock::new(None);

/// Sets the rules every later prune uses (each app at start and when its
/// settings change).
pub fn set_policy(r: Retention) {
    *POLICY.write().unwrap_or_else(|e| e.into_inner()) = Some(r);
}

pub fn policy() -> Retention {
    POLICY.read().unwrap_or_else(|e| e.into_inner()).unwrap_or_default()
}

/// Where pruned backups go: given the folder being pruned and the paths
/// leaving it. Never deletes for good.
pub type Dispose = fn(root: &Path, paths: &[PathBuf]) -> anyhow::Result<()>;

static DISPOSE: RwLock<Option<Dispose>> = RwLock::new(None);

/// Replaces the Recycle Bin (Hopper Server uses each server's trash).
pub fn set_disposer(f: Dispose) {
    *DISPOSE.write().unwrap_or_else(|e| e.into_inner()) = Some(f);
}

fn disposer() -> Dispose {
    DISPOSE.read().unwrap_or_else(|e| e.into_inner()).unwrap_or(recycle)
}

/// The Recycle Bin; a network drive has none, so there they move to
/// `.hopper-trash` beside them, as deleted instances do.
fn recycle(_root: &Path, paths: &[PathBuf]) -> anyhow::Result<()> {
    let mut local = Vec::new();
    for p in paths {
        if crate::instance_ops::is_remote_drive(p) {
            crate::instance_ops::move_to_hopper_trash(p)?;
        } else {
            local.push(p);
        }
    }
    if !local.is_empty() {
        trash::delete_all(local)?;
    }
    Ok(())
}

/// One thing that can go.
#[derive(Clone, Debug, PartialEq)]
enum Item {
    Folder { dir: PathBuf, bytes: u64 },
    Snapshot { id: String },
}

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    /// Seconds since the epoch.
    when: i64,
    item: Item,
}

/// What a prune did (or, for a preview, would do).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pruned {
    pub folders: usize,
    pub snapshots: usize,
    pub freed: u64,
}

impl Pruned {
    pub fn count(&self) -> usize {
        self.folders + self.snapshots
    }
}

/// Where a backups folder's space goes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    /// Per-action folders and automatic snapshots: what pruning removes.
    pub managed: u64,
    /// Snapshots the user took or named: never removed, but they count
    /// toward the size limit.
    pub pinned: u64,
    /// World zips, worlds moved aside and anything else: left alone and
    /// not counted toward the limit.
    pub other: u64,
    /// Number of per-action folders and automatic snapshots.
    pub actions: usize,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.managed + self.pinned + self.other
    }

    /// What the size limit is measured against.
    pub fn limited(&self) -> u64 {
        self.managed + self.pinned
    }
}

/// The action's time from a folder name: `1793138546`, `restore-1793138546510`
/// or `undo-1793138546`. Millisecond stamps are turned into seconds.
fn stamp(name: &str) -> Option<i64> {
    let n = name.strip_prefix("restore-").or_else(|| name.strip_prefix("undo-")).unwrap_or(name);
    let v: i64 = n.parse().ok()?;
    Some(if v > 100_000_000_000 { v / 1000 } else { v })
}

fn dir_bytes(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_bytes(&e.path()),
            _ => e.metadata().map_or(0, |m| m.len()),
        })
        .sum()
}

fn same_file(p: &Path, size: u64, mtime_ms: i64) -> bool {
    let Ok(md) = fs::metadata(p) else { return false };
    let ms = md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64);
    md.len() == size && ms == mtime_ms
}

/// Blob sizes by hash, and which blobs each prunable snapshot uses.
struct Store {
    size: HashMap<String, u64>,
    uses: HashMap<String, Vec<String>>,
    /// Blobs a kept (manual or named) snapshot holds: never freed.
    pinned: HashSet<String>,
}

impl Store {
    fn bytes(&self, live: &HashSet<String>) -> u64 {
        let mut seen: HashSet<&String> = self.pinned.iter().collect();
        for id in live {
            seen.extend(self.uses.get(id).into_iter().flatten());
        }
        seen.iter().map(|h| self.size.get(*h).copied().unwrap_or(0)).sum()
    }
}

fn scan(root: &Path) -> (Vec<Entry>, Store, u64) {
    let base = root.join(crate::sync::BACKUP_DIR);
    let mut out = Vec::new();
    let mut other = 0;
    for e in fs::read_dir(&base).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        if name == crate::snapshots::DIR {
            continue;
        }
        match stamp(&name) {
            Some(when) if path.is_dir() => out.push(Entry { when, item: Item::Folder { bytes: dir_bytes(&path), dir: path } }),
            _ if path.is_dir() => other += dir_bytes(&path),
            _ => other += e.metadata().map_or(0, |m| m.len()),
        }
    }
    let mut store = Store { size: HashMap::new(), uses: HashMap::new(), pinned: HashSet::new() };
    for s in crate::snapshots::list(root) {
        for f in &s.files {
            // A jar still in the instance is the same file as its blob (a hard
            // link): removing the snapshot frees nothing for it.
            let size = if f.rel.ends_with(".jar") && same_file(&root.join(&f.rel), f.size, f.mtime) { 0 } else { f.size };
            store.size.insert(f.hash.clone(), size);
        }
        let hashes = s.files.into_iter().map(|f| f.hash);
        if s.reason.is_auto() && s.name.is_empty() {
            store.uses.insert(s.id.clone(), hashes.collect());
            out.push(Entry { when: s.when, item: Item::Snapshot { id: s.id } });
        } else {
            store.pinned.extend(hashes);
        }
    }
    out.sort_by_key(|e| std::cmp::Reverse(e.when));
    (out, store, other)
}

/// How much the backups folder holds, and how much of it pruning manages.
pub fn usage(root: &Path) -> Usage {
    let (entries, store, other) = scan(root);
    let live: HashSet<String> = store.uses.keys().cloned().collect();
    let folders: u64 = entries.iter().map(|e| if let Item::Folder { bytes, .. } = e.item { bytes } else { 0 }).sum();
    let all = store.bytes(&live);
    let pinned = store.bytes(&HashSet::new());
    Usage { managed: folders + all - pinned, pinned, other, actions: entries.len() }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    /// The user's Clean up: everything past the rules.
    Full,
    /// Right after a new backup: past the rules, but not the backlog.
    Auto,
}

struct Choice {
    /// Indices to remove, oldest first.
    gone: Vec<usize>,
    /// Bytes the limited part holds after.
    left: u64,
    /// Past the rules but left for Clean up (automatic runs only).
    backlog: usize,
}

/// Which entries go under `r` at `now`; entries from `backlog_to` or
/// earlier are the backlog and stay. Pure, for the tests.
fn choose(entries: &[Entry], store: &Store, r: Retention, now: i64, backlog_to: Option<i64>) -> Choice {
    let mut gone = vec![false; entries.len()];
    let mut backlog = vec![false; entries.len()];
    let day_cut = now - i64::from(r.keep_days) * 86_400;
    let young = |e: &Entry| e.when > now - GRACE_SECS;
    // entries are newest first; index 0 (the newest) always stays.
    for (i, e) in entries.iter().enumerate().skip(1) {
        if young(e) || i < r.keep_actions as usize || e.when >= day_cut {
            continue;
        }
        match backlog_to {
            Some(t) if e.when <= t => backlog[i] = true,
            _ => gone[i] = true,
        }
    }
    // The backlog is the user's to clean up: it doesn't count toward the
    // limit either, or newer backups would go to make room for it.
    let total = |gone: &[bool]| -> u64 {
        let keep = |i: usize| !gone[i] && !backlog[i];
        let live: HashSet<String> = entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| match &e.item {
                Item::Snapshot { id } if keep(i) => Some(id.clone()),
                _ => None,
            })
            .collect();
        let folders: u64 = entries
            .iter()
            .enumerate()
            .map(|(i, e)| match e.item {
                Item::Folder { bytes, .. } if keep(i) => bytes,
                _ => 0,
            })
            .sum();
        folders + store.bytes(&live)
    };
    if r.max_mb > 0 {
        let cap = r.max_mb.saturating_mul(1024 * 1024);
        for i in (1..entries.len()).rev() {
            if total(&gone) <= cap {
                break;
            }
            if !gone[i] && !backlog[i] && !young(&entries[i]) {
                gone[i] = true;
            }
        }
    }
    let mut out: Vec<usize> = (0..entries.len()).filter(|&i| gone[i]).collect();
    out.reverse();
    Choice { left: total(&gone), gone: out, backlog: backlog.iter().filter(|b| **b).count() }
}

fn marker(root: &Path) -> PathBuf {
    root.join(crate::sync::BACKUP_DIR).join(MARKER)
}

/// The backlog's newest time: `None` with no marker yet, `Some(None)` with
/// no backlog.
fn read_backlog(root: &Path) -> Option<Option<i64>> {
    let v: serde_json::Value = serde_json::from_slice(&fs::read(marker(root)).ok()?).ok()?;
    Some(v["backlog_to"].as_i64())
}

fn write_backlog(root: &Path, t: Option<i64>) {
    let _ = fs::write(marker(root), serde_json::json!({ "backlog_to": t }).to_string());
}

/// Folders with backups Clean up would remove, or over their size limit.
static NEEDS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn set_needs(root: &Path, needs: bool) {
    let mut n = NEEDS.lock().unwrap_or_else(|e| e.into_inner());
    n.retain(|p| p != root);
    if needs {
        n.push(root.to_path_buf());
    }
}

/// `root` has old backups only Clean up removes, or is over its size
/// limit (as of its last check or prune).
pub fn needs_cleanup(root: &Path) -> bool {
    NEEDS.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|p| p == root)
}

fn run(root: &Path, r: Retention, mode: Mode, dry: bool, dispose: Dispose) -> Pruned {
    let _g = lock_for(root);
    let (entries, store, _) = scan(root);
    let now = crate::time::now();
    let backlog_to = match mode {
        Mode::Full => None,
        Mode::Auto => read_backlog(root).unwrap_or_else(|| {
            // First automatic run in this folder: what was already past the
            // rules before the backup just made is the backlog.
            let before = entries.get(1..).unwrap_or_default();
            let c = choose(before, &store, r, now, None);
            let t = c.gone.iter().map(|&i| before[i].when).max();
            if !dry {
                write_backlog(root, t);
            }
            t
        }),
    };
    let c = choose(&entries, &store, r, now, backlog_to);
    let live_before: HashSet<String> = store.uses.keys().cloned().collect();
    let mut out = Pruned::default();
    let mut dirs = Vec::new();
    let mut snaps = Vec::new();
    for &i in &c.gone {
        match &entries[i].item {
            Item::Folder { dir, bytes } => {
                dirs.push(dir.clone());
                out.folders += 1;
                out.freed += bytes;
            }
            Item::Snapshot { id } => snaps.push(id.clone()),
        }
    }
    if !snaps.is_empty() {
        let live_after: HashSet<String> = live_before.iter().filter(|id| !snaps.contains(id)).cloned().collect();
        out.freed += store.bytes(&live_before) - store.bytes(&live_after);
        out.snapshots = snaps.len();
    }
    let over = r.max_mb > 0 && c.left > r.max_mb.saturating_mul(1024 * 1024);
    if dry {
        return out;
    }
    if !dirs.is_empty()
        && let Err(e) = dispose(root, &dirs)
    {
        // A file still open (a running game) keeps them for next time.
        log::warn!("cleaning up backups in {}: {e:#}", root.display());
        out.folders = 0;
        out.freed = 0;
    }
    if !snaps.is_empty()
        && let Err(e) = crate::snapshots::take_out(root, &snaps).and_then(|p| p.map_or(Ok(()), |p| dispose(root, &[p])))
    {
        log::warn!("cleaning up snapshots in {}: {e:#}", root.display());
        out.snapshots = 0;
    }
    if mode == Mode::Full {
        write_backlog(root, None);
    }
    set_needs(root, c.backlog > 0 || over);
    out
}

/// Clean up: moves every backup in `root` past the current rules to the
/// Recycle Bin (Hopper Server: the server's trash).
pub fn clean_up(root: &Path) -> Pruned {
    run(root, policy(), Mode::Full, false, disposer())
}

/// What [`clean_up`] would remove now, touching nothing.
pub fn preview(root: &Path) -> Pruned {
    run(root, policy(), Mode::Full, true, disposer())
}

/// Notes whether `root` has old backups for Clean up, touching nothing.
pub fn check(root: &Path) {
    let r = policy();
    let p = run(root, r, Mode::Full, true, disposer());
    let over = r.max_mb > 0 && usage(root).limited() > r.max_mb.saturating_mul(1024 * 1024);
    set_needs(root, p.count() > 0 || over);
}

/// One prune per folder at a time.
fn lock_for(root: &Path) -> std::sync::MutexGuard<'static, ()> {
    static LOCKS: Mutex<Vec<(PathBuf, &'static Mutex<()>)>> = Mutex::new(Vec::new());
    let m = {
        let mut l = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        match l.iter().find(|(p, _)| p == root) {
            Some((_, m)) => *m,
            None => {
                let m: &'static Mutex<()> = Box::leak(Box::new(Mutex::new(())));
                l.push((root.to_path_buf(), m));
                m
            }
        }
    };
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Folders waiting for a prune.
static PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Prunes what just crossed the rules in `root`, a few seconds from now in
/// the background. Called after every new backup; a run of them (one per
/// file during a sync) coalesces into one prune.
pub fn schedule(root: &Path) {
    if cfg!(test) {
        return;
    }
    {
        let mut p = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        if p.iter().any(|x| x == root) {
            return;
        }
        p.push(root.to_path_buf());
    }
    let root = root.to_path_buf();
    let _ = std::thread::Builder::new().name("hopper-prune".into()).spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(5));
        PENDING.lock().unwrap_or_else(|e| e.into_inner()).retain(|x| x != &root);
        run(&root, policy(), Mode::Auto, false, disposer());
    });
}

/// [`check`]s every folder in `roots` in the background, removing nothing
/// (each app at start).
pub fn check_all_later(roots: Vec<PathBuf>) {
    let _ = std::thread::Builder::new().name("hopper-backups-check".into()).spawn(move || {
        for r in roots {
            if r.join(crate::sync::BACKUP_DIR).is_dir() {
                check(&r);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    fn tmp(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("hopper-retention-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(crate::sync::BACKUP_DIR)).unwrap();
        root
    }

    /// Tests' Recycle Bin: `<root>/disposed/`.
    fn to_bin(root: &Path, paths: &[PathBuf]) -> anyhow::Result<()> {
        let bin = root.join("disposed");
        fs::create_dir_all(&bin)?;
        for p in paths {
            fs::rename(p, bin.join(p.file_name().unwrap()))?;
        }
        Ok(())
    }

    fn full(root: &Path, r: Retention) -> Pruned {
        run(root, r, Mode::Full, false, to_bin)
    }

    fn auto(root: &Path, r: Retention) -> Pruned {
        run(root, r, Mode::Auto, false, to_bin)
    }

    fn binned(root: &Path) -> Vec<String> {
        let mut v: Vec<String> =
            fs::read_dir(root.join("disposed")).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    /// An action folder holding one file of `bytes`.
    fn action(root: &Path, name: &str, bytes: usize) -> PathBuf {
        let d = root.join(crate::sync::BACKUP_DIR).join(name);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("x.jar"), vec![0u8; bytes]).unwrap();
        d
    }

    /// What's in `.hopper-backups`, but not the marker.
    fn names(root: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(root.join(crate::sync::BACKUP_DIR))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != MARKER)
            .collect();
        v.sort();
        v
    }

    const NO_RULES: Retention = Retention { keep_actions: 0, keep_days: 0, max_mb: 0 };

    #[test]
    fn defaults_keep_30_actions_and_30_days_with_no_size_limit() {
        assert_eq!(Retention::default(), Retention { keep_actions: 30, keep_days: 30, max_mb: 0 });
    }

    #[test]
    fn reads_folder_stamps() {
        assert_eq!(stamp("1793138546"), Some(1793138546));
        assert_eq!(stamp("restore-1793138546510"), Some(1793138546));
        assert_eq!(stamp("undo-1793138546"), Some(1793138546));
        assert_eq!(stamp("worlds"), None);
        assert_eq!(stamp("before-restore"), None);
        assert_eq!(stamp(".pruned-1793138546510"), None);
        assert_eq!(stamp("run.bat.21.1.1"), None);
    }

    #[test]
    fn clean_up_keeps_the_last_n_and_the_recent_and_bins_the_rest() {
        let root = tmp("order");
        let now = crate::time::now();
        // 30 actions a day apart, the newest 23 hours old.
        let at = |i: i64| (now - i * DAY + 3600).to_string();
        for i in 1..=30 {
            action(&root, &at(i), 10);
        }
        let p = full(&root, Retention { keep_actions: 5, keep_days: 10, max_mb: 0 });
        // Kept: the 10 younger than 10 days (more than the last 5).
        assert_eq!(p.folders, 20);
        let left = names(&root);
        assert_eq!(left.len(), 10);
        assert!(left.contains(&at(1)) && left.contains(&at(10)));
        assert!(!left.contains(&at(11)));
        // Nothing deleted: all 20 are in the bin.
        assert_eq!(binned(&root).len(), 20);
        assert!(binned(&root).contains(&at(30)));

        // keep_actions wins when it's the larger.
        let p = full(&root, Retention { keep_actions: 20, keep_days: 0, max_mb: 0 });
        assert_eq!(p.folders, 0);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn leaves_worlds_and_other_folders_alone() {
        let root = tmp("others");
        let b = root.join(crate::sync::BACKUP_DIR);
        for d in ["worlds", "before-restore"] {
            fs::create_dir_all(b.join(d)).unwrap();
            fs::write(b.join(d).join("w.zip"), vec![0u8; 4096]).unwrap();
        }
        fs::write(b.join("run.bat.21.1.1"), "x").unwrap();
        action(&root, "1000", 10);
        action(&root, "2000", 10);
        let p = full(&root, NO_RULES);
        assert_eq!(p.folders, 1);
        assert_eq!(names(&root), ["2000", "before-restore", "run.bat.21.1.1", "worlds"]);
        let u = usage(&root);
        assert_eq!(u.actions, 1);
        assert_eq!(u.managed, 10);
        let marker_len = fs::metadata(marker(&root)).unwrap().len();
        assert_eq!((u.pinned, u.other), (0, 4096 * 2 + 1 + marker_len));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn never_removes_the_newest_even_over_the_cap() {
        let root = tmp("newest");
        let cap = Retention { keep_actions: 0, keep_days: 0, max_mb: 1 };
        action(&root, "1000", 3 * 1024 * 1024);
        assert_eq!(full(&root, cap), Pruned::default());
        assert!(needs_cleanup(&root));
        action(&root, "2000", 3 * 1024 * 1024);
        assert_eq!(full(&root, cap).folders, 1);
        assert_eq!(names(&root), ["2000"]);
        assert!(needs_cleanup(&root));
        full(&root, Retention { max_mb: 4, ..cap });
        assert!(!needs_cleanup(&root));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_size_cap_bins_oldest_until_under() {
        let root = tmp("cap");
        let now = crate::time::now();
        // Five 1 MB actions, all recent enough to keep by days and count.
        for i in 1..=5 {
            action(&root, &(now - i * 3600).to_string(), 1024 * 1024);
        }
        let p = full(&root, Retention { keep_actions: 20, keep_days: 14, max_mb: 3 });
        assert_eq!(p.folders, 2);
        assert_eq!(p.freed, 2 * 1024 * 1024);
        let left = names(&root);
        assert!(left.contains(&(now - 3600).to_string()) && left.contains(&(now - 3 * 3600).to_string()));
        assert!(!left.contains(&(now - 5 * 3600).to_string()));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn spares_the_last_ten_minutes_over_the_cap() {
        let root = tmp("grace");
        let now = crate::time::now();
        action(&root, &now.to_string(), 2 * 1024 * 1024);
        action(&root, &(now - 60).to_string(), 2 * 1024 * 1024);
        action(&root, &(now - 3600).to_string(), 2 * 1024 * 1024);
        let p = full(&root, Retention { keep_actions: 0, keep_days: 0, max_mb: 1 });
        assert_eq!(p.folders, 1);
        assert_eq!(names(&root).len(), 2);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn preview_touches_nothing() {
        let root = tmp("preview");
        action(&root, "1000", 10);
        action(&root, "2000", 10);
        let p = run(&root, NO_RULES, Mode::Full, true, to_bin);
        assert_eq!((p.folders, p.freed), (1, 10));
        assert_eq!(names(&root), ["1000", "2000"]);
        assert!(!marker(&root).exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn after_a_backup_only_what_it_pushed_out_goes_and_the_backlog_waits() {
        let root = tmp("auto");
        let now = crate::time::now();
        let r = Retention { keep_actions: 3, keep_days: 7, max_mb: 0 };
        // A backlog from before: 10 actions, 31 to 40 days old.
        let old = |i: i64| (now - (30 + i) * DAY).to_string();
        for i in 1..=10 {
            action(&root, &old(i), 10);
        }
        // First automatic run, right after a new backup: what crossed the
        // rules before stays.
        action(&root, &(now - 20).to_string(), 10);
        let p = auto(&root, r);
        // Only the one this backup pushed past "the last 3" goes.
        assert_eq!(p.folders, 1);
        assert_eq!(binned(&root), [old(3)]);
        assert!(needs_cleanup(&root));
        // The next backup pushes out one more.
        action(&root, &(now - 10).to_string(), 10);
        assert_eq!(auto(&root, r).folders, 1);
        assert_eq!(binned(&root), [old(3), old(2)]);
        // Clean up takes the rest of the backlog.
        let p = full(&root, r);
        assert_eq!(p.folders, 7);
        assert!(!needs_cleanup(&root));
        assert_eq!(binned(&root).len(), 9);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn after_a_clean_up_actions_aging_out_go_with_the_next_backup() {
        let root = tmp("aging");
        let now = crate::time::now();
        let r = Retention { keep_actions: 1, keep_days: 7, max_mb: 0 };
        // Cleaned up 5 days ago; this action turned 7 days old since.
        action(&root, &(now - 8 * DAY).to_string(), 10);
        write_backlog(&root, None);
        action(&root, &(now - 20).to_string(), 10);
        assert_eq!(auto(&root, r).folders, 1);
        assert_eq!(binned(&root), [(now - 8 * DAY).to_string()]);
        fs::remove_dir_all(&root).unwrap();
    }

    /// A snapshot manifest written straight, using `files` (hash, size)
    /// blobs.
    fn snapshot(root: &Path, id: i64, reason: &str, name: &str, files: &[(&str, usize)]) {
        let base = root.join(crate::sync::BACKUP_DIR).join(crate::snapshots::DIR);
        let entries: Vec<_> = files
            .iter()
            .map(|(h, n)| {
                let d = base.join("blobs").join(&h[..2]);
                fs::create_dir_all(&d).unwrap();
                fs::write(d.join(h), vec![0u8; *n]).unwrap();
                serde_json::json!({"rel": format!("mods/{h}.jar"), "size": n, "hash": h})
            })
            .collect();
        let v = serde_json::json!({"when": id / 1000, "reason": {"kind": reason, "count": 1, "from": "1", "to": "2"}, "name": name, "files": entries});
        fs::write(base.join(format!("{id}.json")), serde_json::to_vec(&v).unwrap()).unwrap();
    }

    fn blob_count(root: &Path) -> usize {
        let b = root.join(crate::sync::BACKUP_DIR).join(crate::snapshots::DIR).join("blobs");
        fs::read_dir(b).into_iter().flatten().flatten().map(|d| fs::read_dir(d.path()).unwrap().count()).sum()
    }

    #[test]
    fn bins_automatic_snapshots_and_their_unused_blobs_only() {
        let root = tmp("snaps");
        // Old automatic ones sharing "aa11", a manual one and a named one.
        snapshot(&root, 1_000_000, "bulk_update", "", &[("aa11", 100), ("bb22", 200)]);
        snapshot(&root, 2_000_000, "bulk_update", "", &[("aa11", 100), ("cc33", 300)]);
        snapshot(&root, 3_000_000, "manual", "", &[("dd44", 400)]);
        snapshot(&root, 4_000_000, "bulk_update", "Before the big one", &[("ee55", 500)]);
        // The newest action, a folder.
        action(&root, "5000", 10);
        assert_eq!(usage(&root).managed, 100 + 200 + 300 + 10);
        let p = full(&root, NO_RULES);
        assert_eq!(p.snapshots, 2);
        assert_eq!(p.freed, 600);
        let left: Vec<String> = crate::snapshots::list(&root).into_iter().map(|s| s.id).collect();
        assert_eq!(left, ["4000000", "3000000"]);
        assert_eq!(blob_count(&root), 2);
        // Both manifests and their three blobs went to the bin, in one folder.
        let bin = binned(&root);
        assert_eq!(bin.len(), 1);
        let pruned = root.join("disposed").join(&bin[0]);
        assert!(pruned.join("1000000.json").is_file() && pruned.join("2000000.json").is_file());
        for h in ["aa11", "bb22", "cc33"] {
            assert!(pruned.join("blobs").join(&h[..2]).join(h).is_file(), "{h}");
        }
        assert_eq!(names(&root), ["5000", "snapshots"]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_shared_blob_stays_while_a_kept_snapshot_uses_it() {
        let root = tmp("shared");
        snapshot(&root, 1_000_000, "bulk_update", "", &[("aa11", 100), ("bb22", 200)]);
        snapshot(&root, 2_000_000, "bulk_update", "", &[("aa11", 100)]);
        let p = full(&root, Retention { keep_actions: 1, keep_days: 0, max_mb: 0 });
        assert_eq!((p.snapshots, p.freed), (1, 200));
        assert_eq!(blob_count(&root), 1);
        assert_eq!(crate::snapshots::list(&root)[0].id, "2000000");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_jar_still_in_the_instance_costs_nothing() {
        let root = tmp("linked");
        snapshot(&root, 1_000_000, "bulk_update", "", &[("aa11", 100), ("bb22", 200)]);
        // The instance still has aa11's jar, unchanged since the snapshot.
        fs::create_dir_all(root.join("mods")).unwrap();
        let jar = root.join("mods/aa11.jar");
        fs::write(&jar, vec![0u8; 100]).unwrap();
        let ms = fs::metadata(&jar).unwrap().modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
        let m = root.join(crate::sync::BACKUP_DIR).join(crate::snapshots::DIR).join("1000000.json");
        let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&m).unwrap()).unwrap();
        v["files"][0]["mtime"] = ms.into();
        fs::write(&m, serde_json::to_vec(&v).unwrap()).unwrap();
        assert_eq!(usage(&root).managed, 200);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn mixes_folders_and_snapshots_by_age() {
        let root = tmp("mixed");
        action(&root, "1000", 10);
        snapshot(&root, 2_000_000, "bulk_update", "", &[("aa11", 100)]);
        action(&root, "3000", 10);
        snapshot(&root, 4_000_000, "pack_update", "", &[("bb22", 100)]);
        let p = full(&root, Retention { keep_actions: 2, keep_days: 0, max_mb: 0 });
        assert_eq!((p.folders, p.snapshots), (1, 1));
        assert_eq!(names(&root), ["3000", "snapshots"]);
        assert_eq!(crate::snapshots::list(&root)[0].id, "4000000");
        fs::remove_dir_all(&root).unwrap();
    }
}
