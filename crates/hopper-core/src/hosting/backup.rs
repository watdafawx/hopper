//! Backups of a hosted server: zips in `<server>/.hopper-server-backups`,
//! either the whole folder or just its worlds. A running server's world is
//! saved first and saving is paused while the zip is made (`save-off`,
//! `save-all flush`, then `save-on`), so the copy is consistent.
//!
//! Scheduled backups keep the newest `keep`; ones made by hand stay until
//! deleted. Restoring needs the server stopped; worlds it replaces are moved
//! aside first, never deleted.

use super::process::LocalServer;
use crate::jobs::Job;
use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read as _, Write as _};
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
    /// The zip's file name; its id.
    pub name: String,
    /// Unix seconds.
    pub at: u64,
    pub size: u64,
    pub files: usize,
    pub scope: Scope,
    pub kind: Kind,
    pub note: String,
}

const META: &str = "backups.json";

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn dir(root: &Path) -> PathBuf {
    root.join(DIR)
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

/// The server's backups, newest first (only ones whose zip still exists).
pub fn list(root: &Path) -> Vec<Backup> {
    let mut l: Vec<Backup> = load_meta(root).into_iter().filter(|b| dir(root).join(&b.name).is_file()).collect();
    l.sort_by_key(|b| std::cmp::Reverse(b.at));
    l
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

/// Files whose contents are already compressed: stored, not deflated.
fn stored(rel: &str) -> bool {
    let l = rel.to_ascii_lowercase();
    [".jar", ".zip", ".mca", ".png", ".gz", ".jpg", ".ogg", ".mcc"].iter().any(|e| l.ends_with(e))
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
/// is flushed to disk and saving paused while the zip is made.
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

    let stamp = chrono_like(now());
    let name = format!(
        "{stamp}-{}-{}.zip",
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
    fs::create_dir_all(dir(root))?;
    let path = dir(root).join(&name);
    let part = path.with_extension("zip.part");
    let result = (|| -> anyhow::Result<()> {
        let mut z = zip::ZipWriter::new(std::io::BufWriter::with_capacity(1 << 20, fs::File::create(&part)?));
        let deflate = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(true);
        let store = deflate.compression_method(zip::CompressionMethod::Stored);
        let mut buf = vec![0u8; 1 << 20];
        let total = files.len();
        for (i, rel) in files.iter().enumerate() {
            job.check_cancelled()?;
            if i % 50 == 0 {
                job.detail(format!("Backing up ({i}/{total} files)"));
                job.progress(i, total);
            }
            // A file the server holds open is skipped, not fatal.
            let mut f = match fs::File::open(root.join(rel)) {
                Ok(f) => f,
                Err(e) => {
                    log::warn!("backup: skipped {rel}: {e}");
                    continue;
                }
            };
            z.start_file(rel.as_str(), if stored(rel) { store } else { deflate })?;
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                z.write_all(&buf[..n])?;
            }
        }
        z.set_comment(note.to_string());
        z.finish()?.flush()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    fs::rename(&part, &path)?;
    let b = Backup {
        size: fs::metadata(&path)?.len(),
        name,
        at: now(),
        files: files.len(),
        scope,
        kind,
        note: note.to_string(),
    };
    let mut meta = load_meta(root);
    meta.push(b.clone());
    save_meta(root, &meta)?;
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

/// Deletes a backup.
pub fn delete(root: &Path, name: &str) -> anyhow::Result<()> {
    check(name)?;
    let _ = fs::remove_file(dir(root).join(name));
    let mut meta = load_meta(root);
    meta.retain(|b| b.name != name);
    save_meta(root, &meta)
}

/// Keeps the newest `keep` scheduled backups; returns how many went.
pub fn prune(root: &Path, keep: usize) -> anyhow::Result<usize> {
    let scheduled: Vec<Backup> = list(root).into_iter().filter(|b| b.kind == Kind::Scheduled).collect();
    let mut n = 0;
    for b in scheduled.into_iter().skip(keep.max(1)) {
        delete(root, &b.name)?;
        n += 1;
    }
    Ok(n)
}

fn check(name: &str) -> anyhow::Result<()> {
    if name.is_empty() || name.contains(['/', '\\', ':']) || name.contains("..") || !name.ends_with(".zip") {
        bail!("That isn't one of this server's backups.");
    }
    Ok(())
}

/// The paths inside a backup.
pub fn entries(root: &Path, name: &str) -> anyhow::Result<Vec<(String, u64)>> {
    check(name)?;
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

/// Puts a backup back. `only` limits it to these paths (files, or folders
/// ending in `/`); empty restores everything in it. Worlds it contains are
/// replaced whole: the current ones are moved aside first. The server must
/// be stopped.
pub fn restore(root: &Path, name: &str, only: &[String], job: &Job) -> anyhow::Result<usize> {
    check(name)?;
    let mut z = zip::ZipArchive::new(fs::File::open(dir(root).join(name)).context("That backup is gone.")?)?;
    let wanted = |n: &str| only.is_empty() || only.iter().any(|o| n == o || (o.ends_with('/') && n.starts_with(o.as_str())));
    // Whole worlds come back as they were: move the current ones aside.
    if only.is_empty() {
        let names: Vec<String> = (0..z.len()).filter_map(|i| z.by_index_raw(i).ok().map(|e| e.name().to_string())).collect();
        let aside = dir(root).join(REPLACED).join(chrono_like(now()));
        for w in worlds(root) {
            if names.iter().any(|n| n.starts_with(&format!("{w}/"))) {
                fs::create_dir_all(&aside)?;
                fs::rename(root.join(&w), aside.join(&w)).with_context(|| format!("moving the current {w} aside"))?;
            }
        }
    }
    let total = z.len();
    let mut n = 0;
    for i in 0..total {
        job.check_cancelled()?;
        let mut e = z.by_index(i)?;
        let Some(rel) = e.enclosed_name() else { continue };
        let rel_s = rel.to_string_lossy().replace('\\', "/");
        if e.is_dir() || !wanted(&rel_s) || skipped(&rel_s) {
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
        let names: Vec<String> = entries(&root, &full.name).unwrap().into_iter().map(|e| e.0).collect();
        assert!(names.contains(&"config/a.toml".to_string()) && names.contains(&"world/region/r.0.0.mca".to_string()));
        assert!(!names.iter().any(|n| n.starts_with("logs/") || n.starts_with(DIR)), "{names:?}");
        let w = make(&root, Scope::Worlds, Kind::Scheduled, "", None, &job).unwrap();
        assert!(entries(&root, &w.name).unwrap().iter().all(|(n, _)| n.starts_with("world")));
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
        // The world is replaced whole; the old one is kept aside.
        assert!(!root.join("world/new.dat").exists());
        let aside = fs::read_dir(dir(&root).join(REPLACED)).unwrap().next().unwrap().unwrap().path();
        assert!(aside.join("world/new.dat").exists());
        assert!(delete(&root, "../x.zip").is_err());
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
            meta.push(Backup { name, at: 100 + i as u64, size: 1, files: 1, scope: Scope::Worlds, kind, note: String::new() });
        }
        save_meta(&root, &meta).unwrap();
        assert_eq!(prune(&root, 2).unwrap(), 1);
        let left: Vec<String> = list(&root).into_iter().map(|b| b.name).collect();
        assert_eq!(left, ["b3.zip", "b2.zip", "b1.zip"]);
        assert_eq!(last_scheduled(&root), Some(103));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn names() {
        assert_eq!(chrono_like(1_792_764_726), "2026-10-23_14-12-06");
        assert!(check("x.zip").is_ok() && check("../x.zip").is_err() && check("a/b.zip").is_err());
    }
}
