//! "Changes in the files", the part around [`crate::jar_changes`]: getting the
//! two jars (local files first, one download at most per missing side), the
//! result cache, and the temp folder. See design/changelog-from-files.md.
//!
//! What it keeps: a small JSON result per (project, old file, new file) in
//! Hopper's cache folder, for [`EXPIRE_DAYS`] days. Never a jar. A jar
//! downloaded to compare lives in a temp folder (never a mods folder) and is
//! deleted when the scan ends; the temp folder is also cleared at start and
//! exit. Nothing here touches an instance's files or History.

use crate::cf;
use crate::jar_changes::{self, JarChanges};
use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// A cached result is dropped after this many days.
pub const EXPIRE_DAYS: u64 = 90;
/// Largest jar compared (bytes). Bigger files are skipped, not downloaded.
const MAX_JAR: u64 = 200 * 1024 * 1024;
/// Most versions one view scans, so opening a long list can't start dozens of downloads.
pub const MAX_PER_VIEW: usize = 5;

/// The notice (setting off) and the labels (setting on), the same words in
/// both apps.
pub const NOTICE: &str = "The author didn't write a changelog. Hopper can compare the files to show what changed.";
pub const RESULT_LABEL: &str = "No changelog from the author. What changed in the files:";
pub const RESULT_FOOTER: &str = "Scanned from the jar files by Hopper, not by the author.";
pub const SETTING_HELP: &str =
    "Show what changed in the files when a mod has no changelog. Hopper compares the old and new file; it may download one of them once.";

/// The key a "Not now" is stored under in the dismissed notices.
pub fn dismiss_key(project_id: u64) -> String {
    format!("files-changes:{project_id}")
}

/// The version released just before `f` among `all` (any order): the one a
/// scan compares it with. `None` for the oldest.
pub fn previous_of<'a>(all: &'a [cf::File], f: &cf::File) -> Option<&'a cf::File> {
    all.iter()
        .filter(|o| o.id != f.id && o.is_server_pack != Some(true) && o.file_date < f.file_date)
        .max_by(|a, b| a.file_date.cmp(&b.file_date).then(a.id.cmp(&b.id)))
}

// ------------------------------------------------------------------- cache

#[derive(Serialize, Deserialize)]
struct Cached {
    /// Unix seconds when scanned.
    saved: u64,
    changes: JarChanges,
}

/// A side of the key: the file's sha1, else its id.
fn side(f: &cf::File) -> String {
    f.sha1().unwrap_or_else(|| format!("id{}", f.id))
}

/// The cache entry's file name for a project and two files.
pub fn cache_key(project_id: u64, old: &cf::File, new: &cf::File) -> String {
    let raw = format!("{project_id}|{}|{}", side(old), side(new));
    format!("{}.json", sha1_smol::Sha1::from(raw).digest())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn cache_dir() -> Option<PathBuf> {
    crate::paths::dirs().map(|d| d.cache_dir().join("changelog-files"))
}

fn cache_get(dir: &Path, key: &str, now: u64) -> Option<JarChanges> {
    let p = dir.join(key);
    let c: Cached = serde_json::from_slice(&fs::read(&p).ok()?).ok()?;
    if now.saturating_sub(c.saved) > EXPIRE_DAYS * 86_400 {
        let _ = fs::remove_file(&p);
        return None;
    }
    Some(c.changes)
}

fn cache_put(dir: &Path, key: &str, changes: &JarChanges, now: u64) {
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    if let Ok(b) = serde_json::to_vec(&Cached { saved: now, changes: changes.clone() }) {
        // Whole or not at all: a half-written file would read as a miss, but tidy anyway.
        let tmp = dir.join(format!("{key}.tmp"));
        if fs::write(&tmp, b).is_ok() && fs::rename(&tmp, dir.join(key)).is_err() {
            let _ = fs::remove_file(&tmp);
        }
    }
}

/// Removes results older than [`EXPIRE_DAYS`] (or unreadable) from `dir`.
pub fn purge_expired_in(dir: &Path, now: u64) -> usize {
    let mut n = 0;
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let ok = fs::read(&p)
            .ok()
            .and_then(|b| serde_json::from_slice::<Cached>(&b).ok())
            .is_some_and(|c| now.saturating_sub(c.saved) <= EXPIRE_DAYS * 86_400);
        if !ok && fs::remove_file(&p).is_ok() {
            n += 1;
        }
    }
    n
}

/// Run once at start: drops old results and clears the temp folder.
pub fn tidy() {
    if let Some(d) = cache_dir() {
        purge_expired_in(&d, now_secs());
    }
    clear_temp();
}

// -------------------------------------------------------------------- temp

fn temp_root() -> PathBuf {
    crate::paths::temp_dir().join("changelog-scan")
}

/// Deletes every jar left in the scan temp folder (start and exit).
pub fn clear_temp() {
    let _ = fs::remove_dir_all(temp_root());
}

/// One scan's folder; deleted with the guard, however the scan ends.
struct TempScan(PathBuf);

impl TempScan {
    fn new(root: &Path) -> anyhow::Result<Self> {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = root.join(format!("{}-{n}", std::process::id()));
        fs::create_dir_all(&d)?;
        Ok(Self(d))
    }
}

impl Drop for TempScan {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// ------------------------------------------------------------------- files

fn sha1_of(path: &Path) -> Option<String> {
    use std::io::Read as _;
    let mut f = fs::File::open(path).ok()?;
    let mut h = sha1_smol::Sha1::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Some(h.digest().to_string())
}

/// Is `path` this very file? The checksum when the site lists one, else the
/// size (with no size either, no).
pub fn is_file(path: &Path, f: &cf::File) -> bool {
    let Ok(m) = fs::metadata(path) else { return false };
    if !m.is_file() {
        return false;
    }
    match f.sha1() {
        Some(want) => sha1_of(path).is_some_and(|h| h == want),
        None => f.file_length > 0 && m.len() == f.file_length,
    }
}

/// One jar to compare: a local file that is it, or a download into `tmp`.
fn jar_for(f: &cf::File, local: &[PathBuf], tmp: &TempScan, tag: &str) -> anyhow::Result<PathBuf> {
    if let Some(p) = local.iter().find(|p| is_file(p, f)) {
        return Ok(p.clone());
    }
    // A file from GitHub or a link: only compared when both are on this PC.
    if f.source.is_some() {
        bail!("Hopper can only compare this version when both files are on this PC.");
    }
    if f.file_length > MAX_JAR {
        bail!("This file is too big to compare.");
    }
    let url = cf::download_url(f)?;
    let dest = tmp.0.join(format!("{tag}.jar"));
    cf::download_checked(&url, &dest, &f.expect(), None, |_| {}).with_context(|| format!("Couldn't download {}", f.file_name))?;
    Ok(dest)
}

/// Where to look first for a version's file: the installed jar, then the
/// files History keeps with the same name.
pub fn local_candidates(root: &Path, installed: Option<&Path>, f: &cf::File) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = installed.map(Path::to_path_buf).into_iter().collect();
    for set in crate::backups::list(root) {
        for s in set.files {
            if !s.download && s.name.eq_ignore_ascii_case(&f.file_name) {
                out.push(s.path);
            }
        }
    }
    out
}

/// What a scan needs: the project, the two versions, and where their files
/// may already be.
pub struct Request<'a> {
    pub project_id: u64,
    pub old: &'a cf::File,
    pub new: &'a cf::File,
    pub old_local: Vec<PathBuf>,
    pub new_local: Vec<PathBuf>,
}

/// The cached result for a request, without any work.
pub fn cached(r: &Request) -> Option<JarChanges> {
    cache_get(&cache_dir()?, &cache_key(r.project_id, r.old, r.new), now_secs())
}

/// Compares the two versions: the cache, else the local files, else a
/// download of what's missing (deleted straight after).
pub fn scan(r: &Request) -> anyhow::Result<JarChanges> {
    scan_in(r, cache_dir().as_deref(), &temp_root(), now_secs())
}

fn scan_in(r: &Request, cache: Option<&Path>, temp: &Path, now: u64) -> anyhow::Result<JarChanges> {
    let key = cache_key(r.project_id, r.old, r.new);
    if let Some(c) = cache.and_then(|d| cache_get(d, &key, now)) {
        return Ok(c);
    }
    let tmp = TempScan::new(temp)?;
    let old = jar_for(r.old, &r.old_local, &tmp, "old")?;
    let new = jar_for(r.new, &r.new_local, &tmp, "new")?;
    let changes = jar_changes::compare_files(&old, &new)?;
    // The jars go now; only the small result stays.
    drop(tmp);
    if let Some(d) = cache {
        cache_put(d, &key, &changes, now);
    }
    Ok(changes)
}

// -------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn file(id: u64, sha: &str, date: &str) -> cf::File {
        cf::File {
            id,
            file_name: format!("m-{id}.jar"),
            file_date: date.into(),
            hashes: if sha.is_empty() { vec![] } else { vec![cf::FileHash { value: sha.into(), algo: 1 }] },
            ..Default::default()
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-clf-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn jar(path: &Path, lang: &str) {
        let mut z = zip::ZipWriter::new(fs::File::create(path).unwrap());
        z.start_file("assets/m/lang/en_us.json", zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(lang.as_bytes()).unwrap();
        z.finish().unwrap();
    }

    #[test]
    fn key_is_per_project_and_files() {
        let (a, b, c) = (file(1, "aa", "2026-01-01"), file(2, "bb", "2026-02-01"), file(3, "cc", "2026-03-01"));
        let k = cache_key(7, &a, &b);
        assert_eq!(k, cache_key(7, &a, &b));
        assert_ne!(k, cache_key(8, &a, &b));
        assert_ne!(k, cache_key(7, &a, &c));
        assert_ne!(k, cache_key(7, &b, &a));
        // No checksum: the id stands in.
        assert_ne!(cache_key(7, &file(1, "", ""), &b), cache_key(7, &file(2, "", ""), &b));
        assert!(k.ends_with(".json") && k.len() == 45);
    }

    #[test]
    fn results_expire_after_90_days_and_purge_drops_them() {
        let d = scratch("expire");
        let ch = JarChanges { languages_added: vec!["German".into()], ..Default::default() };
        cache_put(&d, "fresh.json", &ch, 100_000_000);
        cache_put(&d, "old.json", &ch, 100_000_000 - 91 * 86_400);
        fs::write(d.join("junk.json"), b"not json").unwrap();
        let now = 100_000_000;
        assert_eq!(cache_get(&d, "fresh.json", now), Some(ch.clone()));
        assert!(cache_get(&d, "fresh.json", now + 89 * 86_400).is_some());
        assert!(cache_get(&d, "fresh.json", now + 91 * 86_400).is_none(), "past 90 days is a miss");
        assert!(!d.join("fresh.json").exists(), "an expired entry is deleted when read");
        cache_put(&d, "fresh.json", &ch, now);
        assert_eq!(purge_expired_in(&d, now), 2, "old + junk");
        assert!(d.join("fresh.json").exists() && !d.join("old.json").exists() && !d.join("junk.json").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn scan_uses_local_files_caches_the_result_and_leaves_no_jar() {
        let d = scratch("scan");
        let (old_p, new_p) = (d.join("old.jar"), d.join("new.jar"));
        jar(&old_p, r#"{"block.m.a":"A"}"#);
        jar(&new_p, r#"{"block.m.a":"A","block.m.b":"B"}"#);
        let (mut old, mut new) = (file(1, "", "2026-01-01"), file(2, "", "2026-02-01"));
        old.file_length = fs::metadata(&old_p).unwrap().len();
        new.file_length = fs::metadata(&new_p).unwrap().len();
        let (cache, temp) = (d.join("cache"), d.join("tmp"));
        let req = Request { project_id: 5, old: &old, new: &new, old_local: vec![old_p.clone()], new_local: vec![new_p.clone()] };
        let r = scan_in(&req, Some(&cache), &temp, 10).unwrap();
        assert_eq!(r.texts, [1, 0, 0]);
        assert!(temp.read_dir().is_ok_and(|mut e| e.next().is_none()), "the scan folder is gone");
        assert_eq!(fs::read_dir(&cache).unwrap().count(), 1, "one small result file");
        // The local files are not needed again: the cache answers.
        let gone = Request { project_id: 5, old: &old, new: &new, old_local: vec![], new_local: vec![] };
        assert_eq!(scan_in(&gone, Some(&cache), &temp, 20).unwrap(), r);
        // Without the cache and without the files, a file from a link can't be fetched.
        let mut linked = new.clone();
        linked.source = Some(Default::default());
        let miss = Request { project_id: 5, old: &old, new: &linked, old_local: vec![old_p.clone()], new_local: vec![] };
        let e = scan_in(&miss, None, &temp, 20).unwrap_err();
        assert!(format!("{e:#}").contains("both files are on this PC"), "{e:#}");
        assert!(temp.read_dir().is_ok_and(|mut e| e.next().is_none()), "a failed scan cleans up too");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_local_file_must_be_the_file() {
        let d = scratch("is");
        let p = d.join("a.jar");
        jar(&p, "{}");
        let mut f = file(1, "", "");
        f.file_length = fs::metadata(&p).unwrap().len();
        assert!(is_file(&p, &f));
        f.file_length += 1;
        assert!(!is_file(&p, &f), "wrong size");
        f.hashes = vec![cf::FileHash { value: "0000".into(), algo: 1 }];
        assert!(!is_file(&p, &f), "wrong checksum");
        f.hashes = vec![cf::FileHash { value: sha1_of(&p).unwrap(), algo: 1 }];
        assert!(is_file(&p, &f));
        assert!(!is_file(&d.join("missing.jar"), &f));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn temp_folder_is_cleared() {
        let d = scratch("temp");
        let root = d.join("changelog-scan");
        {
            let t = TempScan::new(&root).unwrap();
            fs::write(t.0.join("old.jar"), b"x").unwrap();
            assert!(t.0.join("old.jar").exists());
        }
        assert!(root.read_dir().unwrap().next().is_none(), "the guard removed its folder");
        fs::create_dir_all(root.join("left-over")).unwrap();
        fs::write(root.join("left-over").join("a.jar"), b"x").unwrap();
        let _ = fs::remove_dir_all(&root);
        assert!(!root.exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn previous_is_the_one_just_before() {
        let all = vec![file(3, "c", "2026-03-01"), file(1, "a", "2026-01-01"), file(2, "b", "2026-02-01")];
        assert_eq!(previous_of(&all, &all[0]).map(|f| f.id), Some(2));
        assert_eq!(previous_of(&all, &all[2]).map(|f| f.id), Some(1));
        assert!(previous_of(&all, &all[1]).is_none());
    }
}
