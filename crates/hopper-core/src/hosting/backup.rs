//! Backups of a hosted server in `<server>/.hopper-server-backups`, either
//! the whole folder or just its worlds. A running server's world is saved
//! first and saving is paused while the backup is made (`save-off`,
//! `save-all flush`, then `save-on`), so the copy is consistent.
//!
//! New backups are snapshots in a deduplicated chunk store
//! ([`super::chunkstore`]): each one only adds what changed since the last.
//! Older panels made zips; those stay listed and restore the same way.
//!
//! Scheduled backups are thinned by [`Keep`] (the newest N, one a day, one a
//! week); ones made by hand or before a risky action stay until deleted.
//! Restoring needs the server stopped; worlds it replaces are moved aside
//! first, never deleted.

use super::chunkstore::{self, FileRef, Manifest, Store};
use super::process::LocalServer;
use crate::jobs::Job;
use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DIR: &str = ".hopper-server-backups";
/// Worlds a restore replaced, kept beside the backups.
const REPLACED: &str = "replaced";

/// What a backup holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Everything but logs, crash reports and Hopper's own folders.
    Full,
    /// The worlds (level-name and its _nether / _the_end).
    Worlds,
}

/// Why it was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Manual,
    Scheduled,
    /// Made before something risky (a restore, a pack update).
    Safety,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Backup {
    /// Its id: the zip's file name, or a snapshot's name (ending `.snap`).
    pub name: String,
    /// Unix seconds.
    pub at: u64,
    /// Bytes: of the zip, or of all the files a snapshot holds.
    pub size: u64,
    pub files: usize,
    pub scope: Scope,
    pub kind: Kind,
    pub note: String,
    /// Bytes this backup added to the store (snapshots; 0 for zips, whose
    /// `size` is what they take).
    #[serde(default)]
    pub added: u64,
}

impl Backup {
    pub fn is_zip(&self) -> bool {
        self.name.ends_with(".zip")
    }
}

const META: &str = "backups.json";

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn dir(root: &Path) -> PathBuf {
    root.join(DIR)
}

fn meta_lock() -> &'static std::sync::Mutex<()> {
    static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
    &L
}

fn load_meta(root: &Path) -> Vec<Backup> {
    fs::read(dir(root).join(META)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_meta(root: &Path, list: &[Backup]) -> anyhow::Result<()> {
    fs::create_dir_all(dir(root))?;
    let tmp = dir(root).join(format!("{META}.tmp"));
    fs::write(&tmp, serde_json::to_vec_pretty(list)?)?;
    fs::rename(tmp, dir(root).join(META))?;
    Ok(())
}

/// Changes the list of backups under a lock, so two jobs finishing at once
/// don't drop each other's entry.
fn edit_meta(root: &Path, f: impl FnOnce(&mut Vec<Backup>)) -> anyhow::Result<()> {
    let _g = meta_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut meta = load_meta(root);
    f(&mut meta);
    save_meta(root, &meta)
}

/// The server's chunk store.
pub fn store(root: &Path) -> Store {
    Store::new(&dir(root))
}

fn exists(root: &Path, name: &str) -> bool {
    if name.ends_with(chunkstore::EXT) { store(root).has_manifest(name) } else { dir(root).join(name).is_file() }
}

/// The server's backups, newest first (only ones that still exist).
pub fn list(root: &Path) -> Vec<Backup> {
    let mut l: Vec<Backup> = load_meta(root).into_iter().filter(|b| exists(root, &b.name)).collect();
    l.sort_by_key(|b| std::cmp::Reverse(b.at));
    l
}

/// Bytes the backups take on disk: the store plus the zips.
pub fn usage(root: &Path) -> u64 {
    let zips: u64 = list(root).iter().filter(|b| b.is_zip()).map(|b| b.size).sum();
    zips + store(root).usage()
}

/// A snapshot whose chunks were put in the store by hand (brought back
/// from an off-site copy) joins the list.
pub fn adopt(root: &Path, m: &Manifest) -> anyhow::Result<Backup> {
    check(&m.name)?;
    if !m.name.ends_with(chunkstore::EXT) {
        bail!("not a snapshot");
    }
    store(root).save_manifest(m)?;
    let b = Backup {
        name: m.name.clone(),
        at: m.at,
        size: m.size,
        files: m.files.len(),
        scope: m.scope,
        kind: m.kind,
        note: m.note.clone(),
        added: m.added,
    };
    edit_meta(root, |meta| {
        meta.retain(|x| x.name != b.name);
        meta.push(b.clone());
    })?;
    Ok(b)
}

/// The world folders: `level-name` and its dimension folders (Bukkit-style
/// servers keep them apart).
pub fn worlds(root: &Path) -> Vec<String> {
    let level = super::properties::Properties::load(&root.join("server.properties"))
        .ok()
        .and_then(|p| p.get("level-name").map(str::to_string))
        .unwrap_or_else(|| "world".into());
    [level.clone(), format!("{level}_nether"), format!("{level}_the_end")]
        .into_iter()
        .filter(|w| root.join(w).is_dir())
        .collect()
}

/// Left out of every backup: other backups, Hopper's folders, logs.
fn skipped(rel: &str) -> bool {
    let first = rel.split('/').next().unwrap_or("");
    matches!(first, DIR | ".hopper-trash" | ".hopper-uploads" | ".hopper-backups" | "logs" | "crash-reports" | "debug")
        || rel.ends_with("session.lock")
        || rel == super::process::CONSOLE_FILE
}

/// Files whose contents are already compressed: stored as they are.
fn stored(rel: &str) -> bool {
    let l = rel.to_ascii_lowercase();
    [".jar", ".zip", ".mca", ".png", ".gz", ".jpg", ".ogg", ".mcc", ".linear"].iter().any(|e| l.ends_with(e))
}

fn walk(root: &Path, from: &Path, out: &mut Vec<String>) {
    let Ok(rd) = fs::read_dir(from) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(rel) = p.strip_prefix(root) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if skipped(&rel) {
            continue;
        }
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(root, &p, out),
            Ok(t) if t.is_file() => out.push(rel),
            _ => {}
        }
    }
}

/// Waits until the console shows `needle` in a line after `seen`.
fn wait_for(server: &LocalServer, seen: u64, needle: &str, timeout: Duration) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        let (lines, _) = server.console_since(seen);
        if lines.iter().any(|l| l.contains(needle)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

/// Pauses saving while alive; turns it back on when dropped.
struct SavePause<'a>(Option<&'a LocalServer>);

impl Drop for SavePause<'_> {
    fn drop(&mut self) {
        if let Some(s) = self.0 {
            let _ = s.command("save-on");
        }
    }
}

/// Makes a backup. `server` is the running server, when it runs: its world
/// is flushed to disk and saving paused while the backup is made.
pub fn make(root: &Path, scope: Scope, kind: Kind, note: &str, server: Option<&LocalServer>, job: &Job) -> anyhow::Result<Backup> {
    let running = server.filter(|s| s.state().is_alive());
    let _pause = match running {
        Some(s) => {
            job.detail("Saving the world");
            let seen = s.console_since(0).1;
            s.command("save-off")?;
            let pause = SavePause(Some(s));
            s.command("save-all flush")?;
            if !wait_for(s, seen, "Saved the game", Duration::from_secs(120)) {
                log::warn!("{}: no \"Saved the game\" within 2 minutes; backing up anyway", root.display());
            }
            pause
        }
        None => SavePause(None),
    };

    job.detail("Listing files");
    let mut files = Vec::new();
    match scope {
        Scope::Full => walk(root, root, &mut files),
        Scope::Worlds => {
            let ws = worlds(root);
            if ws.is_empty() {
                bail!("This server has no world yet.");
            }
            for w in ws {
                walk(root, &root.join(w), &mut files);
            }
        }
    }
    files.sort();

    // Named in the PC's local time, as people read it (the listing's dates
    // come from the files themselves).
    let stamp = chrono_like(super::schedule::LocalTime::now().0.max(0) as u64);
    let base = format!(
        "{stamp}-{}-{}",
        match kind {
            Kind::Manual => "manual",
            Kind::Scheduled => "scheduled",
            Kind::Safety => "safety",
        },
        match scope {
            Scope::Full => "full",
            Scope::Worlds => "worlds",
        }
    );
    let st = store(root);
    let lock = st.lock();
    let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut name = format!("{base}{}", chunkstore::EXT);
    let mut n = 2;
    while st.has_manifest(&name) || dir(root).join(&name).exists() {
        name = format!("{base}-{n}{}", chunkstore::EXT);
        n += 1;
    }

    // The newest snapshots: files unchanged since are reused without
    // reading them.
    let mut prev: HashMap<String, FileRef> = HashMap::new();
    for b in list(root).iter().filter(|b| !b.is_zip()).take(4) {
        if let Ok(m) = st.manifest(&b.name) {
            for f in m.files {
                prev.entry(f.path.clone()).or_insert(f);
            }
        }
    }

    let mut out = Vec::with_capacity(files.len());
    let (mut size, mut added) = (0u64, 0u64);
    let total = files.len();
    for (i, rel) in files.iter().enumerate() {
        job.check_cancelled()?;
        if i % 50 == 0 {
            job.detail(format!("Backing up ({i}/{total} files)"));
            job.progress(i, total);
        }
        // A file the server holds open is skipped, not fatal.
        match st.put_file(rel, &root.join(rel), prev.get(rel), !stored(rel)) {
            Ok(p) => {
                size += p.file.size;
                added += p.added;
                out.push(p.file);
            }
            Err(e) => log::warn!("backup: skipped {rel}: {e:#}"),
        }
    }
    let m = Manifest { v: 1, name: name.clone(), at: now(), scope, kind, note: note.to_string(), size, added, files: out };
    st.save_manifest(&m)?;
    let b = Backup { name, at: m.at, size, files: m.files.len(), scope, kind, note: note.to_string(), added };
    edit_meta(root, |meta| meta.push(b.clone()))?;
    Ok(b)
}

/// "2026-10-23_14-05-09" (UTC), for file names.
fn chrono_like(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}_{:02}-{:02}-{:02}", rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// Deletes a backup (and the chunks no other backup uses).
pub fn delete(root: &Path, name: &str) -> anyhow::Result<()> {
    remove(root, name)?;
    if name.ends_with(chunkstore::EXT) {
        store(root).gc()?;
    }
    Ok(())
}

/// Deletes a backup's zip or manifest and its entry; chunks stay until the
/// next clean-up.
fn remove(root: &Path, name: &str) -> anyhow::Result<()> {
    check(name)?;
    if name.ends_with(chunkstore::EXT) {
        store(root).remove_manifest(name);
    } else {
        let _ = fs::remove_file(dir(root).join(name));
    }
    edit_meta(root, |meta| meta.retain(|b| b.name != name))
}

/// Which scheduled backups stay: the newest `last`, plus the newest one of
/// each of the last `daily_days` days and of the last `weekly_weeks` weeks.
/// Manual and before-a-risky-action backups are never thinned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keep {
    pub last: u32,
    pub daily_days: u32,
    pub weekly_weeks: u32,
}

impl Default for Keep {
    fn default() -> Self {
        Keep { last: 12, daily_days: 0, weekly_weeks: 0 }
    }
}

const KEEP: &str = "keep.json";

/// The server's keep rules. `last` is the schedule's "backups kept", so
/// Hopper desktop's schedule form and the panel agree; the daily and weekly
/// counts live beside the backups.
pub fn keep_rules(root: &Path) -> Keep {
    let mut k: Keep = fs::read(dir(root).join(KEEP)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    k.last = super::schedule::Schedule::load(root).backup_keep.max(1);
    k
}

/// Saves the keep rules (and the schedule's "backups kept").
pub fn set_keep(root: &Path, k: Keep) -> anyhow::Result<Keep> {
    let k = Keep { last: k.last.clamp(1, 500), daily_days: k.daily_days.min(366), weekly_weeks: k.weekly_weeks.min(520) };
    let mut s = super::schedule::Schedule::load(root);
    if s.backup_keep != k.last {
        s.backup_keep = k.last;
        s.save(root)?;
    }
    fs::create_dir_all(dir(root))?;
    fs::write(dir(root).join(KEEP), serde_json::to_vec_pretty(&k)?)?;
    Ok(k)
}

/// The scheduled backups (newest first, as `(name, at)`) that `keep` lets
/// go. `offset` is the PC's UTC offset in seconds, so days are local days.
pub fn thin(scheduled: &[(String, u64)], keep: Keep, now: u64, offset: i64) -> Vec<String> {
    let day = |t: u64| (t as i64 + offset).div_euclid(86_400);
    // Weeks start on Monday (1970-01-01 was a Thursday).
    let week = |t: u64| (day(t) + 3).div_euclid(7);
    let mut kept = vec![false; scheduled.len()];
    for k in kept.iter_mut().take(keep.last.max(1) as usize) {
        *k = true;
    }
    let mut days = std::collections::HashSet::new();
    let mut weeks = std::collections::HashSet::new();
    for (i, (_, at)) in scheduled.iter().enumerate() {
        let d = day(*at);
        if day(now) - d < keep.daily_days as i64 && days.insert(d) {
            kept[i] = true;
        }
        let w = week(*at);
        if week(now) - w < keep.weekly_weeks as i64 && weeks.insert(w) {
            kept[i] = true;
        }
    }
    scheduled.iter().zip(kept).filter(|(_, k)| !k).map(|((n, _), _)| n.clone()).collect()
}

/// Thins the scheduled backups by the server's keep rules, with `keep` as
/// the newest-N count; returns how many went.
pub fn prune(root: &Path, keep: usize) -> anyhow::Result<usize> {
    let mut rules = keep_rules(root);
    rules.last = keep.max(1) as u32;
    let scheduled: Vec<(String, u64)> = list(root).into_iter().filter(|b| b.kind == Kind::Scheduled).map(|b| (b.name, b.at)).collect();
    let offset = super::schedule::LocalTime::now().0 - now() as i64;
    let gone = thin(&scheduled, rules, now(), offset);
    let mut snaps = false;
    for n in &gone {
        snaps |= n.ends_with(chunkstore::EXT);
        remove(root, n)?;
    }
    if snaps {
        store(root).gc()?;
    }
    Ok(gone.len())
}

pub fn check(name: &str) -> anyhow::Result<()> {
    if name.is_empty()
        || name.contains(['/', '\\', ':'])
        || name.contains("..")
        || !(name.ends_with(".zip") || name.ends_with(chunkstore::EXT))
    {
        bail!("That isn't one of this server's backups.");
    }
    Ok(())
}

/// The paths inside a backup.
pub fn entries(root: &Path, name: &str) -> anyhow::Result<Vec<(String, u64)>> {
    check(name)?;
    if name.ends_with(chunkstore::EXT) {
        return Ok(store(root).manifest(name)?.files.into_iter().map(|f| (f.path, f.size)).collect());
    }
    let mut z = zip::ZipArchive::new(fs::File::open(dir(root).join(name)).context("That backup is gone.")?)?;
    let mut out = Vec::with_capacity(z.len());
    for i in 0..z.len() {
        let e = z.by_index_raw(i)?;
        if !e.is_dir() {
            out.push((e.name().to_string(), e.size()));
        }
    }
    Ok(out)
}

fn wanted(only: &[String], n: &str) -> bool {
    only.is_empty() || only.iter().any(|o| n == o || (o.ends_with('/') && n.starts_with(o.as_str())))
}

/// Moves the current worlds that `names` (a backup's paths) hold aside.
fn move_worlds_aside(root: &Path, names: &[String]) -> anyhow::Result<()> {
    let stamp = chrono_like(now());
    let mut aside = dir(root).join(REPLACED).join(&stamp);
    let mut n = 2;
    while aside.exists() {
        aside = dir(root).join(REPLACED).join(format!("{stamp}-{n}"));
        n += 1;
    }
    for w in worlds(root) {
        if names.iter().any(|n| n.starts_with(&format!("{w}/"))) {
            fs::create_dir_all(&aside)?;
            fs::rename(root.join(&w), aside.join(&w)).with_context(|| format!("moving the current {w} aside"))?;
        }
    }
    Ok(())
}

/// Puts a backup back. `only` limits it to these paths (files, or folders
/// ending in `/`); empty restores everything in it. Worlds it contains are
/// replaced whole: the current ones are moved aside first. The server must
/// be stopped.
pub fn restore(root: &Path, name: &str, only: &[String], job: &Job) -> anyhow::Result<usize> {
    check(name)?;
    if name.ends_with(chunkstore::EXT) {
        return restore_snapshot(root, name, only, job);
    }
    let mut z = zip::ZipArchive::new(fs::File::open(dir(root).join(name)).context("That backup is gone.")?)?;
    // Whole worlds come back as they were: move the current ones aside.
    if only.is_empty() {
        let names: Vec<String> = (0..z.len()).filter_map(|i| z.by_index_raw(i).ok().map(|e| e.name().to_string())).collect();
        move_worlds_aside(root, &names)?;
    }
    let total = z.len();
    let mut n = 0;
    for i in 0..total {
        job.check_cancelled()?;
        let mut e = z.by_index(i)?;
        let Some(rel) = e.enclosed_name() else { continue };
        let rel_s = rel.to_string_lossy().replace('\\', "/");
        if e.is_dir() || !wanted(only, &rel_s) || skipped(&rel_s) {
            continue;
        }
        if i % 50 == 0 {
            job.detail(format!("Restoring ({i}/{total} files)"));
            job.progress(i, total);
        }
        let dest = root.join(&rel);
        if let Some(p) = dest.parent() {
            fs::create_dir_all(p)?;
        }
        let mut out = fs::File::create(&dest).with_context(|| format!("writing {rel_s}"))?;
        std::io::copy(&mut e, &mut out)?;
        n += 1;
    }
    if n == 0 {
        bail!("Nothing in that backup matched.");
    }
    Ok(n)
}

fn restore_snapshot(root: &Path, name: &str, only: &[String], job: &Job) -> anyhow::Result<usize> {
    let st = store(root);
    let m = st.manifest(name)?;
    let safe = |p: &str| {
        let rel = Path::new(p);
        !rel.is_absolute() && rel.components().all(|c| matches!(c, std::path::Component::Normal(_)))
    };
    let files: Vec<&FileRef> = m.files.iter().filter(|f| wanted(only, &f.path) && !skipped(&f.path) && safe(&f.path)).collect();
    if files.is_empty() {
        bail!("Nothing in that backup matched.");
    }
    // Every chunk must be there before anything on the server changes.
    job.detail("Checking the backup");
    for f in &files {
        if let Some(h) = f.chunks.iter().find(|h| !st.has_chunk(h)) {
            bail!("This backup is damaged: part of {} ({h}) is missing.", f.path);
        }
    }
    if only.is_empty() {
        let names: Vec<String> = m.files.iter().map(|f| f.path.clone()).collect();
        move_worlds_aside(root, &names)?;
    }
    let total = files.len();
    for (i, f) in files.iter().enumerate() {
        job.check_cancelled()?;
        if i % 50 == 0 {
            job.detail(format!("Restoring ({i}/{total} files)"));
            job.progress(i, total);
        }
        st.write_file(f, &root.join(&f.path))?;
    }
    Ok(total)
}

/// Turns an old zip backup into a snapshot, which shares its data with the
/// other snapshots (and can be copied off-site). The zip goes once the
/// snapshot is saved.
pub fn convert(root: &Path, name: &str, job: &Job) -> anyhow::Result<Backup> {
    check(name)?;
    if !name.ends_with(".zip") {
        bail!("That backup is already in the new format.");
    }
    let old = list(root).into_iter().find(|b| b.name == name).context("That backup is gone.")?;
    let tmp = dir(root).join(format!("converting-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let result = (|| -> anyhow::Result<Backup> {
        job.detail("Unpacking the zip");
        let mut z = zip::ZipArchive::new(fs::File::open(dir(root).join(name)).context("That backup is gone.")?)?;
        for i in 0..z.len() {
            job.check_cancelled()?;
            let mut e = z.by_index(i)?;
            let Some(rel) = e.enclosed_name() else { continue };
            if e.is_dir() {
                continue;
            }
            let dest = tmp.join(rel);
            fs::create_dir_all(dest.parent().unwrap())?;
            std::io::copy(&mut e, &mut fs::File::create(&dest)?)?;
        }
        let mut files = Vec::new();
        walk(&tmp, &tmp, &mut files);
        files.sort();
        let st = store(root);
        let lock = st.lock();
        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
        let new_name = format!("{}{}", name.trim_end_matches(".zip"), chunkstore::EXT);
        let (mut size, mut added, mut out) = (0, 0, Vec::with_capacity(files.len()));
        let total = files.len();
        for (i, rel) in files.iter().enumerate() {
            job.check_cancelled()?;
            if i % 50 == 0 {
                job.detail(format!("Converting ({i}/{total} files)"));
                job.progress(i, total);
            }
            let p = st.put_file(rel, &tmp.join(rel), None, !stored(rel))?;
            size += p.file.size;
            added += p.added;
            // The extracted copy's time means nothing; never match on it.
            out.push(FileRef { mtime: 0, ..p.file });
        }
        let m = Manifest { v: 1, name: new_name.clone(), at: old.at, scope: old.scope, kind: old.kind, note: old.note.clone(), size, added, files: out };
        st.save_manifest(&m)?;
        let b = Backup { name: new_name, at: old.at, size, files: m.files.len(), scope: old.scope, kind: old.kind, note: old.note.clone(), added };
        edit_meta(root, |meta| {
            meta.retain(|x| x.name != name);
            meta.push(b.clone());
        })?;
        let _ = fs::remove_file(dir(root).join(name));
        Ok(b)
    })();
    let _ = fs::remove_dir_all(&tmp);
    result
}

/// When the newest scheduled backup was made.
pub fn last_scheduled(root: &Path) -> Option<u64> {
    list(root).into_iter().find(|b| b.kind == Kind::Scheduled).map(|b| b.at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-backup-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn server(root: &Path) {
        fs::write(root.join("server.properties"), "level-name=world\n").unwrap();
        fs::create_dir_all(root.join("world/region")).unwrap();
        fs::create_dir_all(root.join("world_nether")).unwrap();
        fs::create_dir_all(root.join("config")).unwrap();
        fs::create_dir_all(root.join("logs")).unwrap();
        fs::write(root.join("world/level.dat"), "level").unwrap();
        fs::write(root.join("world/region/r.0.0.mca"), vec![7u8; 10_000]).unwrap();
        fs::write(root.join("world_nether/x.dat"), "nether").unwrap();
        fs::write(root.join("config/a.toml"), "a = 1").unwrap();
        fs::write(root.join("logs/latest.log"), "log").unwrap();
    }

    #[test]
    fn full_and_world_backups_restore() {
        let root = temp("full");
        server(&root);
        let job = crate::jobs::Jobs::default().start("t");
        let full = make(&root, Scope::Full, Kind::Manual, "first", None, &job).unwrap();
        assert!(full.name.ends_with(chunkstore::EXT));
        let names: Vec<String> = entries(&root, &full.name).unwrap().into_iter().map(|e| e.0).collect();
        assert!(names.contains(&"config/a.toml".to_string()) && names.contains(&"world/region/r.0.0.mca".to_string()));
        assert!(!names.iter().any(|n| n.starts_with("logs/") || n.starts_with(DIR)), "{names:?}");
        let w = make(&root, Scope::Worlds, Kind::Scheduled, "", None, &job).unwrap();
        assert!(entries(&root, &w.name).unwrap().iter().all(|(n, _)| n.starts_with("world")));
        // Nothing changed: the second backup adds nothing.
        assert_eq!(w.added, 0);
        assert_eq!(list(&root).len(), 2);

        // Break things, then restore one file, then everything.
        fs::write(root.join("config/a.toml"), "broken").unwrap();
        fs::write(root.join("world/level.dat"), "broken").unwrap();
        fs::write(root.join("world/new.dat"), "made after").unwrap();
        restore(&root, &full.name, &["config/a.toml".into()], &job).unwrap();
        assert_eq!(fs::read_to_string(root.join("config/a.toml")).unwrap(), "a = 1");
        assert_eq!(fs::read_to_string(root.join("world/level.dat")).unwrap(), "broken");
        restore(&root, &full.name, &[], &job).unwrap();
        assert_eq!(fs::read_to_string(root.join("world/level.dat")).unwrap(), "level");
        assert_eq!(fs::read(root.join("world/region/r.0.0.mca")).unwrap(), vec![7u8; 10_000]);
        // The world is replaced whole; the old one is kept aside.
        assert!(!root.join("world/new.dat").exists());
        let aside = fs::read_dir(dir(&root).join(REPLACED)).unwrap().next().unwrap().unwrap().path();
        assert!(aside.join("world/new.dat").exists());
        assert!(delete(&root, "../x.zip").is_err());

        // Deleting one keeps the other whole; deleting both empties the store.
        delete(&root, &full.name).unwrap();
        let j2 = crate::jobs::Jobs::default().start("t");
        restore(&root, &w.name, &[], &j2).unwrap();
        assert_eq!(fs::read_to_string(root.join("world/level.dat")).unwrap(), "level");
        delete(&root, &w.name).unwrap();
        assert!(list(&root).is_empty());
        let left = fs::read_dir(store(&root).dir().join("chunks")).map(|d| d.flatten().flat_map(|x| fs::read_dir(x.path()).unwrap().flatten()).count()).unwrap_or(0);
        assert_eq!(left, 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn old_zips_still_list_and_restore() {
        let root = temp("zip");
        server(&root);
        fs::create_dir_all(dir(&root)).unwrap();
        let name = "2026-10-20_01-11-43-manual-full.zip";
        {
            use std::io::Write as _;
            let mut z = zip::ZipWriter::new(fs::File::create(dir(&root).join(name)).unwrap());
            z.start_file("config/a.toml", zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(b"from zip").unwrap();
            z.finish().unwrap();
        }
        save_meta(&root, &[Backup { name: name.into(), at: 1, size: 100, files: 1, scope: Scope::Full, kind: Kind::Manual, note: String::new(), added: 0 }]).unwrap();
        assert_eq!(list(&root).len(), 1);
        assert_eq!(entries(&root, name).unwrap()[0].0, "config/a.toml");
        restore(&root, name, &[], &crate::jobs::Jobs::default().start("t")).unwrap();
        assert_eq!(fs::read_to_string(root.join("config/a.toml")).unwrap(), "from zip");
        assert_eq!(usage(&root), 100);
        // Converted to a snapshot: same date and contents, the zip gone.
        let job = crate::jobs::Jobs::default().start("t");
        let c = convert(&root, name, &job).unwrap();
        assert!(c.name.ends_with(chunkstore::EXT) && c.at == 1);
        assert!(!dir(&root).join(name).exists());
        assert_eq!(list(&root).len(), 1);
        fs::write(root.join("config/a.toml"), "changed").unwrap();
        restore(&root, &c.name, &[], &job).unwrap();
        assert_eq!(fs::read_to_string(root.join("config/a.toml")).unwrap(), "from zip");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_keeps_newest_scheduled_and_all_manual() {
        let root = temp("prune");
        server(&root);
        let mut meta = Vec::new();
        for (i, kind) in [Kind::Scheduled, Kind::Manual, Kind::Scheduled, Kind::Scheduled].into_iter().enumerate() {
            let name = format!("b{i}.zip");
            fs::create_dir_all(dir(&root)).unwrap();
            fs::write(dir(&root).join(&name), "z").unwrap();
            meta.push(Backup { name, at: 100 + i as u64, size: 1, files: 1, scope: Scope::Worlds, kind, note: String::new(), added: 0 });
        }
        save_meta(&root, &meta).unwrap();
        assert_eq!(prune(&root, 2).unwrap(), 1);
        let left: Vec<String> = list(&root).into_iter().map(|b| b.name).collect();
        assert_eq!(left, ["b3.zip", "b2.zip", "b1.zip"]);
        assert_eq!(last_scheduled(&root), Some(103));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn thinning_keeps_one_a_day_and_one_a_week() {
        const D: u64 = 86_400;
        // A Monday noon UTC; one backup every 6 hours for 30 days.
        let now = 1_792_454_400 + D / 2;
        let all: Vec<(String, u64)> = (0..120).map(|i| (format!("b{i}"), now - i * 6 * 3600)).collect();
        let keep = Keep { last: 3, daily_days: 7, weekly_weeks: 4 };
        let gone = thin(&all, keep, now, 0);
        let kept: Vec<&(String, u64)> = all.iter().filter(|(n, _)| !gone.contains(n)).collect();
        // Newest 3, plus one for each of 7 days (today's newest is already
        // in), plus one for each older week.
        assert!(kept.iter().take(3).map(|(n, _)| n.as_str()).eq(["b0", "b1", "b2"]));
        let days: std::collections::HashSet<i64> = kept.iter().map(|(_, t)| (*t / D) as i64).collect();
        assert!(days.len() >= 7 && days.len() <= 11, "{days:?}");
        assert!(kept.len() < 15, "{}", kept.len());
        // Nothing older than 4 weeks stays.
        assert!(kept.iter().all(|(_, t)| now - t < 28 * D + D));
        // Only `last`: plain newest-N.
        assert_eq!(thin(&all, Keep { last: 5, daily_days: 0, weekly_weeks: 0 }, now, 0).len(), 115);
    }

    #[test]
    fn names() {
        assert_eq!(chrono_like(1_792_764_726), "2026-10-23_14-12-06");
        assert!(check("x.zip").is_ok() && check("x.snap").is_ok() && check("../x.zip").is_err() && check("a/b.zip").is_err());
    }
}
