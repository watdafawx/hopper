//! What Hopper set aside in an instance's `.hopper-backups`: removed mods,
//! packs and replaced files, by when, with where each came from, to look
//! through, restore or delete for good.
//!
//! A mod jar that CurseForge or Modrinth can hand out again byte for byte
//! isn't copied: a small `<name>.hopper-ref` note of which file it was takes
//! its place, and [`materialize`] downloads it again (hash-checked) when
//! it's restored or undone. Everything else (unknown jars, files their
//! authors keep off other launchers, configs) is kept as a real copy.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// After a jar's name: the note that stands in for it.
pub const REF_EXT: &str = ".hopper-ref";

/// Which CurseForge or Modrinth file a set-aside jar was.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ref {
    /// Modrinth ids are hashed in, as on an addon ([`crate::source`]).
    pub project_id: u64,
    pub file_id: u64,
    pub file_name: String,
    /// Lower-case hex, of the jar as it was.
    pub sha1: String,
    pub size: u64,
}

fn ref_path(jar: &Path) -> PathBuf {
    let mut s = jar.as_os_str().to_owned();
    s.push(REF_EXT);
    PathBuf::from(s)
}

/// The jar, or the note standing in for it, is there.
pub fn exists(jar: &Path) -> bool {
    jar.exists() || ref_path(jar).is_file()
}

/// Only the note is there: restoring `jar` downloads it.
pub fn is_ref(jar: &Path) -> bool {
    !jar.exists() && ref_path(jar).is_file()
}

fn read_ref(jar: &Path) -> Option<Ref> {
    serde_json::from_slice(&fs::read(ref_path(jar)).ok()?).ok()
}

fn sha1_of(p: &Path) -> std::io::Result<String> {
    use std::io::Read as _;
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

/// Whether CurseForge / Modrinth still hands out exactly this file.
type Check = fn(project: u64, file: u64, sha1: &str, size: u64) -> bool;
/// Downloads a note's file to a path, checked against its hash.
type Fetch = fn(r: &Ref, dest: &Path) -> anyhow::Result<()>;

fn check_online(project: u64, file: u64, sha1: &str, size: u64) -> bool {
    let Ok(f) = crate::source::file(project, file) else { return false };
    f.sha1().is_some_and(|h| h == sha1) && f.file_length == size && crate::cf::download_url(&f).is_ok()
}

fn fetch_online(r: &Ref, dest: &Path) -> anyhow::Result<()> {
    let f = crate::source::file(r.project_id, r.file_id)?;
    let url = crate::cf::download_url(&f)?;
    crate::cf::download_checked(&url, dest, &crate::cf::Expect { len: r.size, sha1: r.sha1.clone() }, None, |_| {})
}

fn offline_check(_: u64, _: u64, _: &str, _: u64) -> bool {
    false
}

fn offline_fetch(_: &Ref, _: &Path) -> anyhow::Result<()> {
    anyhow::bail!("offline in tests")
}

thread_local! {
    // Tests swap these per thread, so tests running side by side don't
    // see each other's.
    static HOOKS: std::cell::Cell<Option<(Check, Fetch)>> = const { std::cell::Cell::new(None) };
}

fn hooks() -> (Check, Fetch) {
    let default: (Check, Fetch) = if cfg!(test) { (offline_check, offline_fetch) } else { (check_online, fetch_online) };
    HOOKS.with(|h| h.get()).unwrap_or(default)
}

#[cfg(test)]
pub(crate) fn set_test_hooks(check: Check, fetch: Fetch) {
    HOOKS.with(|h| h.set(Some((check, fetch))));
}

/// Beside a backup's files: the CurseForge / Modrinth file each jar was,
/// to turn the copy into a note later ([`slim`]).
const IDS: &str = ".ids.json";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Cand {
    project_id: u64,
    file_id: u64,
    file_name: String,
    /// Looked up already and kept as a copy (not downloadable as it is).
    #[serde(default)]
    keep: bool,
}

/// Notes which CurseForge / Modrinth file `saved` (a jar just set aside
/// from `path` in `root`) is, when the instance knows. Cheap: no network.
pub(crate) fn note_ids(root: &Path, path: &Path, saved: &Path) {
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else { return };
    let base = name.strip_suffix(".disabled").unwrap_or(&name);
    if !base.to_lowercase().ends_with(".jar") {
        return;
    }
    let Some(inst) = crate::instances::load(root) else { return };
    let Some(a) = inst.addons.iter().find(|a| a.project_id != 0 && a.file_id != 0 && a.file_name.eq_ignore_ascii_case(base)) else {
        return;
    };
    let (Some(dir), Some(saved_name)) = (saved.parent(), saved.file_name()) else { return };
    let p = dir.join(IDS);
    let mut map: std::collections::BTreeMap<String, Cand> = fs::read(&p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    map.insert(
        saved_name.to_string_lossy().into_owned(),
        Cand { project_id: a.project_id, file_id: a.file_id, file_name: a.file_name.clone(), keep: false },
    );
    let _ = fs::write(&p, serde_json::to_vec_pretty(&map).unwrap_or_default());
}

/// Backups younger than this keep their copies, so an Undo right after an
/// action never has to download anything.
const SLIM_AFTER_SECS: i64 = 10 * 60;

/// Turns jar copies in `root`'s backups that CurseForge or Modrinth hands
/// out again byte for byte into notes ([`REF_EXT`]). Asks online, so it
/// runs in the background. Returns the bytes it saved.
pub fn slim(root: &Path) -> u64 {
    slim_older_than(root, SLIM_AFTER_SECS)
}

fn slim_older_than(root: &Path, age: i64) -> u64 {
    let now = crate::time::now();
    let (check, _) = hooks();
    let mut saved = 0;
    for d in fs::read_dir(root.join(crate::sync::BACKUP_DIR)).into_iter().flatten().flatten() {
        let dir = d.path();
        let ids = dir.join(IDS);
        let Ok(b) = fs::read(&ids) else { continue };
        let stamp = d.file_name().to_str().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
        if now - stamp < age {
            continue;
        }
        let mut map: std::collections::BTreeMap<String, Cand> = serde_json::from_slice(&b).unwrap_or_default();
        for (name, c) in map.iter_mut().filter(|(_, c)| !c.keep) {
            let jar = dir.join(name);
            let (Ok(md), Ok(sha1)) = (fs::metadata(&jar), sha1_of(&jar)) else {
                c.keep = true;
                continue;
            };
            let r = Ref { project_id: c.project_id, file_id: c.file_id, file_name: c.file_name.clone(), sha1, size: md.len() };
            if !check(r.project_id, r.file_id, &r.sha1, r.size) {
                c.keep = true;
                continue;
            }
            // The note first, then the copy goes: never neither.
            let note = ref_path(&jar);
            if fs::write(&note, serde_json::to_vec_pretty(&r).unwrap_or_default()).is_ok() {
                if fs::remove_file(&jar).is_ok() {
                    saved += r.size;
                } else {
                    let _ = fs::remove_file(&note);
                    c.keep = true;
                }
            }
        }
        map.retain(|name, c| c.keep && dir.join(name).is_file());
        if map.is_empty() {
            let _ = fs::remove_file(&ids);
        } else {
            let _ = fs::write(&ids, serde_json::to_vec_pretty(&map).unwrap_or_default());
        }
    }
    saved
}

/// Makes sure `jar` is there: when only its note is, downloads the file
/// again from CurseForge or Modrinth (checked against its hash) and drops
/// the note.
pub fn materialize(jar: &Path) -> anyhow::Result<()> {
    if jar.exists() || !ref_path(jar).is_file() {
        return Ok(());
    }
    let r = read_ref(jar).ok_or_else(|| anyhow::anyhow!("{}: its backup note can't be read", jar.display()))?;
    let (_, fetch) = hooks();
    fetch(&r, jar).map_err(|e| anyhow::anyhow!("{}: the backup keeps only where to download it, and downloading it again failed: {e:#}", r.file_name))?;
    let _ = fs::remove_file(ref_path(jar));
    Ok(())
}

/// The newest backup of `original` made at `since` (unix seconds) or later,
/// in a `.hopper-backups/<time>/` folder (repeats are numbered `2-name`):
/// the jar's path there, which may be only its note ([`exists`]).
pub fn find(root: &Path, original: &Path, since: u64) -> Option<PathBuf> {
    let name = original.file_name()?.to_string_lossy().into_owned();
    let mut best: Option<((u64, u32), PathBuf)> = None;
    for d in fs::read_dir(root.join(crate::sync::BACKUP_DIR)).ok()?.flatten() {
        let Some(stamp) = d.file_name().to_str().and_then(|s| s.parse::<u64>().ok()) else { continue };
        if stamp < since {
            continue;
        }
        for e in fs::read_dir(d.path()).into_iter().flatten().flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            let n = n.strip_suffix(REF_EXT).unwrap_or(&n).to_string();
            let rank = if n == name {
                Some(1)
            } else {
                n.strip_suffix(&name).and_then(|p| p.strip_suffix('-')).and_then(|p| p.parse::<u32>().ok())
            };
            if let Some(r) = rank
                && best.as_ref().is_none_or(|(k, _)| *k < (stamp, r))
            {
                best = Some(((stamp, r), d.path().join(&n)));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Beside a backup's files: their name → where they came from (relative to
/// the instance).
pub const ORIGINS: &str = ".origins.json";

/// Notes where a file set aside came from.
pub fn record(backup_dir: &Path, saved: &Path, original_rel: &str) {
    let p = backup_dir.join(ORIGINS);
    let mut map: serde_json::Map<String, serde_json::Value> = fs::read(&p)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let name = saved.file_name().unwrap_or_default().to_string_lossy().into_owned();
    map.insert(name, original_rel.replace('\\', "/").into());
    let _ = fs::write(&p, serde_json::to_vec_pretty(&map).unwrap_or_default());
}

#[derive(Clone, Debug, PartialEq)]
pub struct Saved {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    /// Where it goes back to, relative to the instance ("mods/x.jar").
    pub restore_to: String,
    /// Kept as a note: restoring downloads it again.
    pub download: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Set {
    pub dir: PathBuf,
    /// Seconds since the epoch.
    pub when: i64,
    pub files: Vec<Saved>,
}

impl Set {
    pub fn size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// Where a file with no recorded origin most likely came from.
fn guess(path: &Path, name: &str) -> String {
    let lower = name.to_lowercase();
    // "2-common.toml": the backup's own de-duplicating prefix.
    let clean = match name.split_once('-') {
        Some((n, rest)) if n.parse::<u32>().is_ok() => rest,
        _ => name,
    };
    let folder = if lower.ends_with(".jar") || lower.ends_with(".jar.disabled") {
        "mods"
    } else if lower.ends_with(".zip") || lower.ends_with(".zip.disabled") {
        let shader = fs::File::open(path)
            .ok()
            .and_then(|f| zip::ZipArchive::new(f).ok())
            .is_some_and(|z| z.file_names().any(|n| n.starts_with("shaders/") || n.contains("/shaders/")));
        if shader { "shaderpacks" } else { "resourcepacks" }
    } else if lower.ends_with(".toml") || lower.ends_with(".json") || lower.ends_with(".json5") || lower.ends_with(".cfg") {
        "config"
    } else {
        "mods"
    };
    format!("{folder}/{clean}")
}

/// Everything set aside in an instance, newest first.
pub fn list(root: &Path) -> Vec<Set> {
    let base = root.join(crate::sync::BACKUP_DIR);
    let mut out: Vec<Set> = fs::read_dir(&base)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let when: i64 = e.file_name().to_string_lossy().parse().ok()?;
            let dir = e.path();
            let origins: serde_json::Map<String, serde_json::Value> = fs::read(dir.join(ORIGINS))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            let mut files = Vec::new();
            walk(&dir, &dir, &origins, &mut files);
            files.sort_by_key(|f| f.name.to_lowercase());
            (!files.is_empty()).then_some(Set { dir, when, files })
        })
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.when));
    out
}

fn walk(base: &Path, dir: &Path, origins: &serde_json::Map<String, serde_json::Value>, out: &mut Vec<Saved>) {
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name == ORIGINS || name == IDS {
            continue;
        }
        if p.is_dir() {
            walk(base, &p, origins, out);
            continue;
        }
        // A note stands for its jar: listed under the jar's name and size.
        let (p, name, size, download) = match name.strip_suffix(REF_EXT) {
            Some(jar) => {
                let jp = p.with_file_name(jar);
                let size = read_ref(&jp).map_or(0, |r| r.size);
                let jar = jar.to_string();
                (jp, jar, size, true)
            }
            None => {
                let size = e.metadata().map_or(0, |m| m.len());
                (p, name, size, false)
            }
        };
        let rel = p.strip_prefix(base).unwrap_or(&p).to_string_lossy().replace('\\', "/");
        let restore_to = match origins.get(&name).and_then(|v| v.as_str()) {
            Some(o) => o.to_string(),
            // Kept under its folder ("mods/x.jar") by drag and drop and sync.
            None if rel.contains('/') => rel.clone(),
            None => guess(&p, &name),
        };
        out.push(Saved { size, name, path: p, restore_to, download });
    }
}

/// Puts a file back where it came from. A file already there is kept: the
/// restored one gets " (restored)" in its name instead.
pub fn restore(root: &Path, f: &Saved) -> anyhow::Result<PathBuf> {
    materialize(&f.path)?;
    let mut dest = root.join(&f.restore_to);
    if dest.exists() {
        let stem = dest.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let ext = dest.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        dest = dest.with_file_name(format!("{stem} (restored){ext}"));
    }
    if let Some(p) = dest.parent() {
        fs::create_dir_all(p)?;
    }
    if fs::rename(&f.path, &dest).is_err() {
        fs::copy(&f.path, &dest)?;
        fs::remove_file(&f.path)?;
    }
    // An emptied backup folder goes.
    if let Some(dir) = f.path.parent()
        && fs::read_dir(dir).is_ok_and(|mut d| d.all(|e| e.is_ok_and(|e| e.file_name() == ORIGINS || e.file_name() == IDS)))
    {
        let _ = fs::remove_dir_all(dir);
    }
    Ok(dest)
}

/// Sends a whole backup to the Recycle Bin (or deletes it on a network
/// drive, which has none). `permanent` skips the Recycle Bin.
pub fn delete(set: &Set, permanent: bool) -> anyhow::Result<()> {
    if permanent {
        crate::instance_ops::remove_for_good(&set.dir)?;
    } else if crate::instance_ops::is_remote_drive(&set.dir) {
        fs::remove_dir_all(&set.dir)?;
    } else {
        trash::delete(&set.dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the fake CurseForge hands out for project 5, file 9.
    const REMOTE: &[u8] = b"jar v1";

    fn sha(b: &[u8]) -> String {
        sha1_smol::Sha1::from(b).digest().to_string()
    }

    fn fake_check(project: u64, file: u64, sha1: &str, size: u64) -> bool {
        (project, file) == (5, 9) && sha1 == sha(REMOTE) && size == REMOTE.len() as u64
    }

    fn fake_fetch(r: &Ref, dest: &Path) -> anyhow::Result<()> {
        anyhow::ensure!((r.project_id, r.file_id) == (5, 9), "not found");
        anyhow::ensure!(sha(REMOTE) == r.sha1, "hash differs");
        fs::write(dest, REMOTE)?;
        Ok(())
    }

    fn failing_fetch(_: &Ref, _: &Path) -> anyhow::Result<()> {
        anyhow::bail!("no connection")
    }

    /// An instance knowing `a.jar` as CurseForge project 5 file 9.
    fn instance(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("hopper-backups-ref-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("mods")).unwrap();
        let v = serde_json::json!({"name": "Test", "format": crate::instances::FORMAT, "addons": [
            {"project_id": 5, "file_id": 9, "file_name": "a.jar", "name": "A", "enabled": true}
        ]});
        fs::write(root.join(crate::instances::HOPPER_FILE), serde_json::to_vec(&v).unwrap()).unwrap();
        root
    }

    #[test]
    fn a_downloadable_jar_is_kept_as_a_note_and_downloads_again() {
        set_test_hooks(fake_check, fake_fetch);
        let root = instance("note");
        fs::write(root.join("mods/a.jar"), REMOTE).unwrap();
        fs::write(root.join("mods/b.jar"), "unknown jar").unwrap();
        let a = crate::sync::backup_into(&root, &root.join("mods/a.jar")).unwrap().unwrap();
        let b = crate::sync::backup_into(&root, &root.join("mods/b.jar")).unwrap().unwrap();
        // A fresh backup keeps its copy (an Undo now needs no download)...
        assert!(a.is_file());
        assert_eq!(slim(&root), 0);
        assert!(a.is_file());
        // ...until it's older: then a is only a note; b (unknown) stays a copy.
        assert_eq!(slim_older_than(&root, 0), REMOTE.len() as u64);
        assert!(!root.join("mods/a.jar").exists() && !a.exists() && is_ref(&a) && exists(&a));
        assert!(b.is_file() && !is_ref(&b));
        let sets = list(&root);
        let files: Vec<&Saved> = sets.iter().flat_map(|s| &s.files).collect();
        let fa = files.iter().find(|f| f.name == "a.jar").unwrap();
        assert!(fa.download && fa.size == REMOTE.len() as u64 && fa.restore_to == "mods/a.jar");
        assert!(!files.iter().find(|f| f.name == "b.jar").unwrap().download);
        // The Undo lookup finds it by the jar's name.
        assert_eq!(find(&root, &root.join("mods/a.jar"), 0), Some(a.clone()));
        // Restoring downloads it again and drops the note.
        let back = restore(&root, fa).unwrap();
        assert_eq!(fs::read(&back).unwrap(), REMOTE);
        assert!(!exists(&a));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_jar_that_differs_from_the_download_keeps_a_real_copy() {
        set_test_hooks(fake_check, fake_fetch);
        let root = instance("changed");
        // Same name and ids, but not the bytes CurseForge hands out.
        fs::write(root.join("mods/a.jar"), "patched by hand").unwrap();
        let a = crate::sync::backup_into(&root, &root.join("mods/a.jar")).unwrap().unwrap();
        assert_eq!(slim_older_than(&root, 0), 0);
        assert!(a.is_file() && !is_ref(&a));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_failed_download_says_so_and_keeps_the_note() {
        set_test_hooks(fake_check, failing_fetch);
        let root = instance("offline");
        fs::write(root.join("mods/a.jar"), REMOTE).unwrap();
        let a = crate::sync::backup_into(&root, &root.join("mods/a.jar")).unwrap().unwrap();
        slim_older_than(&root, 0);
        let e = materialize(&a).unwrap_err().to_string();
        assert!(e.contains("a.jar") && e.contains("downloading it again failed") && e.contains("no connection"), "{e}");
        assert!(is_ref(&a));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn offline_by_default_in_tests_keeps_copies() {
        let root = instance("default");
        fs::write(root.join("mods/a.jar"), REMOTE).unwrap();
        let a = crate::sync::backup_into(&root, &root.join("mods/a.jar")).unwrap().unwrap();
        slim_older_than(&root, 0);
        assert!(a.is_file());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn lists_and_restores() {
        let root = std::env::temp_dir().join(format!("hopper-backups-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("resourcepacks")).unwrap();
        fs::create_dir_all(root.join("mods")).unwrap();
        // A pack removed now (recorded), a mod removed earlier (guessed).
        fs::write(root.join("resourcepacks/Bare Bones.zip"), "zip").unwrap();
        let saved = crate::sync::backup_into(&root, &root.join("resourcepacks/Bare Bones.zip")).unwrap().unwrap();
        let old = root.join(crate::sync::BACKUP_DIR).join("1000");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("jei.jar"), "jar").unwrap();

        let sets = list(&root);
        assert_eq!(sets.len(), 2);
        assert_eq!(sets[0].files[0].restore_to, "resourcepacks/Bare Bones.zip");
        assert_eq!(sets[1].files[0].restore_to, "mods/jei.jar");

        let back = restore(&root, &sets[0].files[0]).unwrap();
        assert_eq!(back, root.join("resourcepacks/Bare Bones.zip"));
        assert!(!saved.exists());
        // Its emptied backup folder went with it.
        assert_eq!(list(&root).len(), 1);

        // Something already there stays; the restored copy gets a new name.
        fs::write(root.join("mods/jei.jar"), "newer").unwrap();
        let back = restore(&root, &list(&root)[0].files[0]).unwrap();
        assert_eq!(back, root.join("mods/jei (restored).jar"));
        assert_eq!(fs::read_to_string(root.join("mods/jei.jar")).unwrap(), "newer");
        fs::remove_dir_all(&root).unwrap();
    }
}
