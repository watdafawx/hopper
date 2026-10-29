//! How long `.hopper-backups` keeps what Hopper set aside: the per-action
//! folders (`<time>/`, `restore-<time>/`, `undo-<time>/`) that hold the old
//! files behind an Undo, and automatic snapshots. Shared by Hopper and
//! Hopper Server, which keeps the same folder in each server.
//!
//! Kept: the last [`Retention::keep_actions`] actions, anything newer than
//! [`Retention::keep_days`], and always the newest action. Past that, and
//! whenever the folder is over [`Retention::max_mb`], the oldest go first.
//! Snapshots the user took or named, world zips and worlds moved aside
//! before a restore are never touched.

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
        Retention { keep_actions: 20, keep_days: 14, max_mb: 1024 }
    }
}

/// Undo toasts and running jobs still point at fresh backups: nothing this
/// young is pruned, whatever the size.
const GRACE_SECS: i64 = 10 * 60;

static POLICY: RwLock<Option<Retention>> = RwLock::new(None);

/// Sets the rules every later prune uses (each app at start and when its
/// settings change).
pub fn set_policy(r: Retention) {
    *POLICY.write().unwrap_or_else(|e| e.into_inner()) = Some(r);
}

pub fn policy() -> Retention {
    POLICY.read().unwrap_or_else(|e| e.into_inner()).unwrap_or_default()
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

/// What a prune did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pruned {
    pub folders: usize,
    pub snapshots: usize,
    pub freed: u64,
}

/// Where a backups folder's space goes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    /// Per-action folders and automatic snapshots: what pruning manages.
    pub managed: u64,
    /// Snapshots the user took or named, world zips and anything else.
    pub kept: u64,
    /// Number of per-action folders and automatic snapshots.
    pub actions: usize,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.managed + self.kept
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
    Usage { managed: folders + all - pinned, kept: other + pinned, actions: entries.len() }
}

/// Which entries go under `r` at `now`, oldest first. Pure, for the tests.
fn choose(entries: &[Entry], store: &Store, r: Retention, now: i64) -> Vec<usize> {
    let mut gone = vec![false; entries.len()];
    let day_cut = now - i64::from(r.keep_days) * 86_400;
    // entries are newest first; index 0 (the newest) always stays.
    for (i, e) in entries.iter().enumerate().skip(1) {
        let young = e.when > now - GRACE_SECS;
        if !young && i >= r.keep_actions as usize && e.when < day_cut {
            gone[i] = true;
        }
    }
    if r.max_mb > 0 {
        let cap = r.max_mb.saturating_mul(1024 * 1024);
        let total = |gone: &[bool]| -> u64 {
            let live: HashSet<String> = entries
                .iter()
                .zip(gone)
                .filter_map(|(e, g)| match &e.item {
                    Item::Snapshot { id } if !g => Some(id.clone()),
                    _ => None,
                })
                .collect();
            let folders: u64 = entries
                .iter()
                .zip(gone)
                .map(|(e, g)| match e.item {
                    Item::Folder { bytes, .. } if !g => bytes,
                    _ => 0,
                })
                .sum();
            folders + store.bytes(&live)
        };
        for i in (1..entries.len()).rev() {
            if total(&gone) <= cap {
                break;
            }
            if !gone[i] && entries[i].when <= now - GRACE_SECS {
                gone[i] = true;
            }
        }
    }
    let mut out: Vec<usize> = (0..entries.len()).filter(|&i| gone[i]).collect();
    out.reverse();
    out
}

/// Prunes `root`'s backups under `r` now.
pub fn prune_with(root: &Path, r: Retention) -> Pruned {
    run(root, r, false)
}

/// What [`prune_with`] would remove, touching nothing.
pub fn preview(root: &Path, r: Retention) -> Pruned {
    run(root, r, true)
}

fn run(root: &Path, r: Retention, dry: bool) -> Pruned {
    let _g = lock_for(root);
    let (entries, store, _) = scan(root);
    let now = crate::time::now();
    let pick = choose(&entries, &store, r, now);
    let live_before: HashSet<String> = store.uses.keys().cloned().collect();
    let mut out = Pruned::default();
    let mut snaps = Vec::new();
    for i in pick {
        match &entries[i].item {
            Item::Folder { dir, bytes } => {
                // A file still open (a running game) keeps its folder for next time.
                if dry || fs::remove_dir_all(dir).is_ok() {
                    out.folders += 1;
                    out.freed += bytes;
                }
            }
            Item::Snapshot { id } => snaps.push(id.clone()),
        }
    }
    if !snaps.is_empty() {
        let live_after: HashSet<String> = live_before.iter().filter(|id| !snaps.contains(id)).cloned().collect();
        out.freed += store.bytes(&live_before) - store.bytes(&live_after);
        out.snapshots = snaps.len();
        if !dry {
            crate::snapshots::delete_many(root, &snaps);
        }
    }
    out
}

/// Prunes `root`'s backups under the current policy.
pub fn prune(root: &Path) -> Pruned {
    prune_with(root, policy())
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

/// Prunes `root` a few seconds from now, in the background. Called after
/// every new backup; a run of them (one per file during a sync) coalesces
/// into one prune.
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
        prune(&root);
    });
}

/// Prunes every folder in `roots`, one after another, in the background
/// (each app at start).
pub fn prune_all_later(roots: Vec<PathBuf>) {
    let _ = std::thread::Builder::new().name("hopper-prune-all".into()).spawn(move || {
        for r in roots {
            if r.join(crate::sync::BACKUP_DIR).is_dir() {
                prune(&r);
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

    /// An action folder `days` old holding one file of `bytes`.
    fn action(root: &Path, name: &str, bytes: usize) -> PathBuf {
        let d = root.join(crate::sync::BACKUP_DIR).join(name);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("x.jar"), vec![0u8; bytes]).unwrap();
        d
    }

    fn names(root: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(root.join(crate::sync::BACKUP_DIR))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    /// What the default rules would remove from `HOPPER_RETENTION_ROOT`,
    /// touching nothing.
    #[test]
    #[ignore]
    fn preview_real_folder() {
        let Ok(root) = std::env::var("HOPPER_RETENTION_ROOT") else { return };
        let root = PathBuf::from(root);
        let u = usage(&root);
        let p = preview(&root, Retention::default());
        println!("{u:?}
would remove {p:?}");
    }

    #[test]
    fn preview_touches_nothing() {
        let root = tmp("preview");
        action(&root, "1000", 10);
        action(&root, "2000", 10);
        let p = preview(&root, Retention { keep_actions: 0, keep_days: 0, max_mb: 0 });
        assert_eq!((p.folders, p.freed), (1, 10));
        assert_eq!(names(&root), ["1000", "2000"]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn reads_folder_stamps() {
        assert_eq!(stamp("1793138546"), Some(1793138546));
        assert_eq!(stamp("restore-1793138546510"), Some(1793138546));
        assert_eq!(stamp("undo-1793138546"), Some(1793138546));
        assert_eq!(stamp("worlds"), None);
        assert_eq!(stamp("before-restore"), None);
        assert_eq!(stamp("run.bat.21.1.1"), None);
    }

    #[test]
    fn keeps_the_last_n_and_the_recent_and_drops_the_oldest_first() {
        let root = tmp("order");
        let now = crate::time::now();
        // 30 actions a day apart, the newest 23 hours old.
        let at = |i: i64| (now - i * DAY + 3600).to_string();
        for i in 1..=30 {
            action(&root, &at(i), 10);
        }
        let r = Retention { keep_actions: 5, keep_days: 10, max_mb: 0 };
        let p = prune_with(&root, r);
        // Kept: the 10 younger than 10 days.
        assert_eq!(p.folders, 20);
        let left = names(&root);
        assert_eq!(left.len(), 10);
        assert!(left.contains(&at(1)) && left.contains(&at(10)));
        assert!(!left.contains(&at(11)));

        // keep_actions wins when it's the larger: nothing more goes.
        let p = prune_with(&root, Retention { keep_actions: 20, keep_days: 0, max_mb: 0 });
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
        let p = prune_with(&root, Retention { keep_actions: 0, keep_days: 0, max_mb: 0 });
        assert_eq!(p.folders, 1);
        assert_eq!(names(&root), ["2000", "before-restore", "run.bat.21.1.1", "worlds"]);
        let u = usage(&root);
        assert_eq!(u.actions, 1);
        assert_eq!(u.managed, 10);
        assert_eq!(u.kept, 4096 * 2 + 1);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn never_deletes_the_newest_even_over_the_cap() {
        let root = tmp("newest");
        action(&root, "1000", 3 * 1024 * 1024);
        let p = prune_with(&root, Retention { keep_actions: 0, keep_days: 0, max_mb: 1 });
        assert_eq!(p, Pruned::default());
        action(&root, "2000", 3 * 1024 * 1024);
        let p = prune_with(&root, Retention { keep_actions: 0, keep_days: 0, max_mb: 1 });
        assert_eq!(p.folders, 1);
        assert_eq!(names(&root), ["2000"]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_size_cap_drops_oldest_until_under() {
        let root = tmp("cap");
        let now = crate::time::now();
        // Five 1 MB actions, all recent enough to keep by days and count.
        for i in 1..=5 {
            action(&root, &(now - i * 3600).to_string(), 1024 * 1024);
        }
        let p = prune_with(&root, Retention { keep_actions: 20, keep_days: 14, max_mb: 3 });
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
        let p = prune_with(&root, Retention { keep_actions: 0, keep_days: 0, max_mb: 1 });
        assert_eq!(p.folders, 1);
        assert_eq!(names(&root).len(), 2);
        fs::remove_dir_all(&root).unwrap();
    }

    /// A snapshot manifest written straight, `when` seconds, using `files`
    /// (hash, size) blobs.
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
    fn prunes_automatic_snapshots_and_their_unused_blobs_only() {
        let root = tmp("snaps");
        // Old automatic ones sharing "aa11", a manual one and a named one.
        snapshot(&root, 1_000_000, "bulk_update", "", &[("aa11", 100), ("bb22", 200)]);
        snapshot(&root, 2_000_000, "bulk_update", "", &[("aa11", 100), ("cc33", 300)]);
        snapshot(&root, 3_000_000, "manual", "", &[("dd44", 400)]);
        snapshot(&root, 4_000_000, "bulk_update", "Before the big one", &[("ee55", 500)]);
        // The newest action, a folder.
        action(&root, "5000", 10);
        assert_eq!(usage(&root).managed, 100 + 200 + 300 + 10);
        let p = prune_with(&root, Retention { keep_actions: 0, keep_days: 0, max_mb: 0 });
        assert_eq!(p.snapshots, 2);
        assert_eq!(p.freed, 600);
        let left: Vec<String> = crate::snapshots::list(&root).into_iter().map(|s| s.id).collect();
        assert_eq!(left, ["4000000", "3000000"]);
        assert_eq!(blob_count(&root), 2);
        assert_eq!(names(&root), ["5000", "snapshots"]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_shared_blob_stays_while_a_kept_snapshot_uses_it() {
        let root = tmp("shared");
        snapshot(&root, 1_000_000, "bulk_update", "", &[("aa11", 100), ("bb22", 200)]);
        snapshot(&root, 2_000_000, "bulk_update", "", &[("aa11", 100)]);
        let p = prune_with(&root, Retention { keep_actions: 1, keep_days: 0, max_mb: 0 });
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
        let p = prune_with(&root, Retention { keep_actions: 2, keep_days: 0, max_mb: 0 });
        assert_eq!((p.folders, p.snapshots), (1, 1));
        assert_eq!(names(&root), ["3000", "snapshots"]);
        assert_eq!(crate::snapshots::list(&root)[0].id, "4000000");
        fs::remove_dir_all(&root).unwrap();
    }
}
