//! An instance's History (kept in its `.hopper-backups` folder): what was
//! removed, updated, switched or edited, by when, with where each file came
//! from, to undo.
//!
//! A mod jar CurseForge or Modrinth knows isn't copied: it's deleted, and a
//! small `<name>.hopper-ref` note of which file it was (project, file, hash,
//! name, its entry) takes its place. Undo downloads it again (hash-checked,
//! [`crate::undo`]). Only what can't be downloaded again is kept as a copy:
//! jars with no known source, and the small text files Hopper changes
//! (configs, options.txt).

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
    /// The Modrinth version id itself: the hashed `file_id` only maps back
    /// while this run has seen it.
    #[serde(default)]
    pub mr_version: String,
    /// The Modrinth project (slug or id), to list its other versions when
    /// this one is gone.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mr_project: String,
    /// The mod's name, for History.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Its entry in the instance as it was (on or off, from the pack, when
    /// added), put back by Undo. Absent in notes made before 0.40.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addon: Option<crate::instances::Addon>,
}

pub(crate) fn ref_path(jar: &Path) -> PathBuf {
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

pub(crate) fn read_ref(jar: &Path) -> Option<Ref> {
    serde_json::from_slice(&fs::read(ref_path(jar)).ok()?).ok()
}

pub(crate) fn sha1_of(p: &Path) -> std::io::Result<String> {
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
type Check = fn(r: &Ref) -> bool;
/// Downloads a note's file to a path, checked against its hash.
type Fetch = fn(r: &Ref, dest: &Path) -> anyhow::Result<()>;

/// The note's file as CurseForge or Modrinth lists it now.
pub(crate) fn remote_file(r: &Ref) -> anyhow::Result<crate::cf::File> {
    if r.mr_version.is_empty() {
        crate::source::file(r.project_id, r.file_id)
    } else {
        Ok(crate::modrinth::version_to_file(&crate::modrinth::version(&r.mr_version)?))
    }
}

fn check_online(r: &Ref) -> bool {
    let Ok(f) = remote_file(r) else { return false };
    f.sha1().is_some_and(|h| h == r.sha1) && f.file_length == r.size && crate::cf::download_url(&f).is_ok()
}

fn fetch_online(r: &Ref, dest: &Path) -> anyhow::Result<()> {
    let f = remote_file(r)?;
    let url = crate::cf::download_url(&f)?;
    crate::cf::download_checked(&url, dest, &crate::cf::Expect { len: r.size, sha1: r.sha1.clone() }, None, |_| {})
}

fn offline_check(_: &Ref) -> bool {
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

pub(crate) fn hooks() -> (Check, Fetch) {
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
/// Beside a History folder's files: what set them aside, when not the user.
const REASON: &str = ".reason";

/// Marks every History folder made since `since` (unix seconds) as set
/// aside by `reason` ("Clean update").
pub fn mark_reason(root: &Path, since: u64, reason: &str) {
    for d in fs::read_dir(root.join(crate::sync::BACKUP_DIR)).into_iter().flatten().flatten() {
        let Some(stamp) = d.file_name().to_str().and_then(|s| s.parse::<u64>().ok()) else { continue };
        if stamp >= since {
            let _ = fs::write(d.path().join(REASON), reason);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Cand {
    project_id: u64,
    file_id: u64,
    file_name: String,
    #[serde(default)]
    mr_version: String,
    /// Looked up already and kept as a copy (not downloadable as it is).
    #[serde(default)]
    keep: bool,
    #[serde(default)]
    mr_project: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    addon: Option<crate::instances::Addon>,
}

impl Cand {
    fn to_ref(&self, sha1: String, size: u64) -> Ref {
        Ref {
            project_id: self.project_id,
            file_id: self.file_id,
            file_name: self.file_name.clone(),
            sha1,
            size,
            mr_version: self.mr_version.clone(),
            mr_project: self.mr_project.clone(),
            name: self.name.clone(),
            addon: self.addon.clone(),
        }
    }
}

/// Which CurseForge / Modrinth file `path` (a jar in `root` about to be
/// set aside) is, when the instance knows. Read before it moves. Cheap: no
/// network.
pub(crate) fn ids_for(root: &Path, path: &Path) -> Option<Cand> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    let base = name.strip_suffix(".disabled").unwrap_or(&name);
    if !base.to_lowercase().ends_with(".jar") {
        return None;
    }
    let inst = crate::instances::load(root)?;
    let a = inst.addons.iter().find(|a| a.project_id != 0 && a.file_id != 0 && a.file_name.eq_ignore_ascii_case(base))?;
    Some(Cand {
        project_id: a.project_id,
        file_id: a.file_id,
        file_name: a.file_name.clone(),
        mr_version: a.mr_version.clone(),
        keep: false,
        mr_project: a.mr_project.clone(),
        name: a.name.clone(),
        addon: Some(a.clone()),
    })
}

/// Deletes `path` (a jar CurseForge or Modrinth knows, per `c`) and leaves
/// only a note of which file it was at `dest`, its place in a History
/// folder: Undo downloads it again. The note is written first, so there's
/// never neither.
pub(crate) fn note_instead(path: &Path, dest: &Path, c: &Cand) -> anyhow::Result<()> {
    let size = fs::metadata(path)?.len();
    let r = c.to_ref(sha1_of(path)?, size);
    let note = ref_path(dest);
    fs::write(&note, serde_json::to_vec_pretty(&r)?)?;
    if let Err(e) = fs::remove_file(path) {
        let _ = fs::remove_file(&note);
        return Err(e.into());
    }
    Ok(())
}

/// Notes in History that the mod at `path` (one CurseForge or Modrinth
/// knows, turned off) is about to be turned on, without moving it: Undo
/// puts its entry back as it was, off. Nothing for a jar with no known
/// source.
pub fn note_before_switch(root: &Path, path: &Path) -> anyhow::Result<()> {
    let Some(c) = ids_for(root, path) else { return Ok(()) };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let dir = root.join(crate::sync::BACKUP_DIR).join(stamp.to_string());
    fs::create_dir_all(&dir)?;
    let dest = dir.join(&c.file_name);
    let r = c.to_ref(sha1_of(path)?, fs::metadata(path)?.len());
    fs::write(ref_path(&dest), serde_json::to_vec_pretty(&r)?)?;
    // Undo reads "off" from the place it goes back to.
    if let Some(a) = &c.addon {
        let off = if a.enabled { "" } else { ".disabled" };
        record(&dir, &dest, &format!("{}/{}{off}", a.folder(), a.file_name));
    }
    crate::retention::schedule(root);
    Ok(())
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
            let r = c.to_ref(sha1, md.len());
            if !check(&r) {
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
    fetch(&r, jar).map_err(|e| anyhow::anyhow!("{}: History keeps only where to download it, and downloading it again failed: {e:#}", r.file_name))?;
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
    if let Some(root) = crate::actor::root_of(backup_dir) {
        crate::actor::note_set_aside(&root, backup_dir, saved, original_rel);
    }
}

/// A History folder's own notes, not entries.
fn is_bookkeeping(name: &str) -> bool {
    matches!(name, ORIGINS | IDS | CHECKED | REASON | crate::actor::WHO_FILE)
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
    /// Which CurseForge / Modrinth file it was, when known (`sha1` is empty
    /// for a kept copy).
    pub src: Option<Ref>,
    /// What happened to it, judged against the instance now.
    pub change: Change,
    /// The file of the same project in the instance now (the version it was
    /// updated or rolled back to).
    pub now: Option<String>,
    /// "CurseForge" or "Modrinth" for the file in `now`.
    pub now_site: &'static str,
    /// What set it aside, when not the user ("Clean update").
    pub reason: String,
    /// Who made the change; None for entries from before Hopper noted it.
    pub who: Option<crate::actor::Who>,
    /// The sync or bulk action it was part of (one row in History).
    pub batch: String,
    /// A record-only entry (added, turned on or off): what it was and the
    /// mod's key. Nothing was set aside; Undo switches it back.
    pub record: Option<(crate::actor::Did, String)>,
}

impl Saved {
    /// The mod's name when History knows it, else the file's.
    pub fn title(&self) -> String {
        match &self.src {
            Some(r) if !r.name.is_empty() => r.name.clone(),
            _ => self.name.trim_end_matches(".disabled").to_string(),
        }
    }

    /// "CurseForge" or "Modrinth", where Undo downloads it from; empty for
    /// a kept copy.
    pub fn site(&self) -> &'static str {
        match &self.src {
            Some(r) if !r.mr_version.is_empty() => "Modrinth",
            Some(_) => "CurseForge",
            None => "",
        }
    }

    /// The line under an entry in History: "1.6.3 → 1.6.4 · CurseForge",
    /// "0.116.15 · Modrinth", "No download source · copy kept (2 MB)" or
    /// where a config goes back to.
    pub fn detail(&self) -> String {
        let base = self.name.trim_end_matches(".disabled");
        let line = match (self.change, &self.now, &self.src) {
            // A record of a mod from a link: no site to name, no copy kept.
            (_, _, None) if self.record.is_some() => crate::undo::version_of(base),
            // Only turned on or off: the same file, so no "X → X".
            (Change::TurnedOn | Change::TurnedOff, _, Some(_)) => format!("{} · {}", crate::undo::version_of(base), self.site()),
            (Change::Updated | Change::RolledBack | Change::Switched, Some(now), _) => {
                let (old, new) = crate::install::version_change(base, now);
                let moved = !self.now_site.is_empty() && self.site() != self.now_site;
                if old != new {
                    format!("{old} → {new} · {}", self.site())
                } else if base != now.as_str() {
                    // The same version text: say what differs instead.
                    let site = if moved { format!("{} → {}", self.site(), self.now_site) } else { self.site().to_string() };
                    format!("{base} → {now} · {site}")
                } else if moved {
                    format!("{} · {} → {}", crate::undo::version_of(base), self.site(), self.now_site)
                } else {
                    format!("{} · {}", crate::undo::version_of(base), self.site())
                }
            }
            (_, _, Some(_)) => format!("{} · {}", crate::undo::version_of(base), self.site()),
            (_, _, None) if is_jar(&self.name) => {
                format!("No download source · copy kept ({})", crate::instance_ops::human_bytes(self.size))
            }
            (_, _, None) => self.restore_to.clone(),
        };
        if self.reason.is_empty() { line } else { format!("{line} · by {}", self.reason) }
    }
}

/// What a History entry was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Gone from the instance.
    Removed,
    /// A newer file of the same mod is there now.
    Updated,
    /// An older file of the same mod is there now.
    RolledBack,
    /// Another file of the same mod (or the same file again) is there now.
    Switched,
    /// The same file, now on (it was off when set aside).
    TurnedOn,
    /// The same file, now off.
    TurnedOff,
    /// A text file (a config, options.txt) whose earlier text this is.
    Edited,
    /// Something else of the same name is there now.
    Replaced,
    /// Put in (a record: nothing set aside).
    Added,
}

impl Change {
    pub fn label(self) -> &'static str {
        match self {
            Change::Removed => "Removed",
            Change::Updated => "Updated",
            Change::RolledBack => "Rolled back",
            Change::Switched => "Switched",
            Change::TurnedOn => "Turned on",
            Change::TurnedOff => "Turned off",
            Change::Edited => "Changed",
            Change::Replaced => "Replaced",
            Change::Added => "Added",
        }
    }

    /// The label as a verb after a name: "Mia turned off Sodium".
    pub fn verb(self) -> &'static str {
        match self {
            Change::Removed => "removed",
            Change::Updated => "updated",
            Change::RolledBack => "rolled back",
            Change::Switched => "switched",
            Change::TurnedOn => "turned on",
            Change::TurnedOff => "turned off",
            Change::Edited => "changed",
            Change::Replaced => "replaced",
            Change::Added => "added",
        }
    }

    /// [`Change::label`] read back; anything else is a removal.
    pub fn from_label(s: &str) -> Change {
        match s {
            "Updated" => Change::Updated,
            "Rolled back" => Change::RolledBack,
            "Switched" => Change::Switched,
            "Turned on" => Change::TurnedOn,
            "Turned off" => Change::TurnedOff,
            "Changed" => Change::Edited,
            "Replaced" => Change::Replaced,
            "Added" => Change::Added,
            _ => Change::Removed,
        }
    }
}

/// Small text files Hopper edits: their earlier text is the History entry.
pub fn is_text(name: &str) -> bool {
    let l = name.to_lowercase();
    [".toml", ".json", ".json5", ".cfg", ".txt", ".properties", ".yml", ".yaml", ".ini", ".snbt", ".conf"]
        .iter()
        .any(|e| l.ends_with(e))
}

/// Judges each file against the instance now.
fn judge(root: &Path, inst: Option<&crate::instances::Instance>, f: &mut Saved) {
    let here = root.join(&f.restore_to).exists();
    let same = f.src.as_ref().and_then(|r| {
        let addons = &inst?.addons;
        addons.iter().find(|a| a.project_id == r.project_id).or_else(|| addons.iter().find(|a| a.file_name.eq_ignore_ascii_case(&r.file_name)))
    });
    f.now = same.map(|a| a.file_name.clone());
    f.now_site = same.map_or("", |a| if a.mr_version.is_empty() { "CurseForge" } else { "Modrinth" });
    f.change = match (f.src.as_ref(), same) {
        (Some(r), Some(a)) => {
            let old_date = r.addon.as_ref().map(|o| o.file_date.as_str()).unwrap_or("");
            let newer = if !old_date.is_empty() && !a.file_date.is_empty() && r.mr_version.is_empty() == a.mr_version.is_empty() {
                a.file_date.as_str().cmp(old_date)
            } else if r.mr_version.is_empty() && a.mr_version.is_empty() && a.source.is_none() && !crate::sources::is_src(a.project_id) {
                // Only CurseForge file ids grow with time.
                a.file_id.cmp(&r.file_id)
            } else {
                std::cmp::Ordering::Equal
            };
            match newer {
                std::cmp::Ordering::Greater => Change::Updated,
                std::cmp::Ordering::Less => Change::RolledBack,
                std::cmp::Ordering::Equal => {
                    // The very same file, only on or off now.
                    let was_on = r.addon.as_ref().map_or(!f.name.ends_with(".disabled"), |o| o.enabled);
                    let same_file = r.file_name == a.file_name && r.file_id == a.file_id && r.mr_version == a.mr_version;
                    match (same_file, was_on, a.enabled) {
                        (true, false, true) => Change::TurnedOn,
                        (true, true, false) => Change::TurnedOff,
                        _ => Change::Switched,
                    }
                }
            }
        }
        (Some(_), None) => Change::Removed,
        (None, _) if here && is_text(&f.name) => Change::Edited,
        (None, _) if here => Change::Replaced,
        (None, _) => Change::Removed,
    };
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

    /// The one person behind every entry, when there is one.
    pub fn who(&self) -> Option<&crate::actor::Who> {
        let first = self.files.first()?.who.as_ref()?;
        self.files.iter().all(|f| f.who.as_ref().is_some_and(|w| w.name == first.name && w.id == first.id)).then_some(first)
    }

    /// The people behind its entries, in order, each once.
    pub fn people(&self) -> Vec<crate::actor::Who> {
        let mut out: Vec<crate::actor::Who> = Vec::new();
        for w in self.files.iter().filter_map(|f| f.who.as_ref()) {
            if !out.iter().any(|o| o.name == w.name && o.id == w.id) {
                out.push(w.clone());
            }
        }
        out
    }

    /// What its entries did, counted: "3 mods updated, 1 added".
    pub fn summary(&self) -> String {
        const ORDER: [Change; 9] = [
            Change::Updated,
            Change::Added,
            Change::Removed,
            Change::TurnedOff,
            Change::TurnedOn,
            Change::RolledBack,
            Change::Switched,
            Change::Edited,
            Change::Replaced,
        ];
        let mods = self.files.iter().all(|f| f.src.is_some() || is_jar(&f.name));
        let mut parts = Vec::new();
        for c in ORDER {
            let n = self.files.iter().filter(|f| f.change == c).count();
            if n == 0 {
                continue;
            }
            if parts.is_empty() {
                let what = match (mods, n) {
                    (true, 1) => "mod",
                    (true, _) => "mods",
                    (false, 1) => "file",
                    (false, _) => "files",
                };
                parts.push(format!("{n} {what} {}", c.verb()));
            } else {
                parts.push(format!("{n} {}", c.verb()));
            }
        }
        parts.join(", ")
    }

    /// The line a set of several entries shows: "Updated from Robin's All
    /// the Mods 10", "Mia turned off 3 mods", "You changed 20 mods". None
    /// when nobody is known (entries from before Hopper noted it).
    pub fn headline(&self) -> Option<String> {
        let n = self.files.len();
        let mods = self.files.iter().all(|f| f.src.is_some() || is_jar(&f.name));
        let what = match (mods, n) {
            (true, 1) => "1 mod".to_string(),
            (true, _) => format!("{n} mods"),
            (false, 1) => "1 file".to_string(),
            (false, _) => format!("{n} files"),
        };
        let same = self.files.iter().all(|f| f.change == self.files[0].change);
        let verb = if same { self.files[0].change.verb() } else { "changed" };
        if let Some(w) = self.who() {
            if !w.pack.is_empty() && !w.is_you() {
                return Some(format!("Updated from {}'s {}", w.name, w.pack));
            }
            return Some(format!("{} {verb} {what}", w.label()));
        }
        let people = self.people();
        match people.as_slice() {
            [] => None,
            [a, b] => Some(format!("{} and {} {verb} {what}", a.label(), b.label())),
            [a, ..] => Some(format!("{} and others {verb} {what}", a.label())),
        }
    }

    /// The set as Hopper Server sends it (`GET /servers/{id}/mod-backups`),
    /// so Hopper desktop shows a panel server's History with the same rows
    /// as an instance's.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "when": self.when, "size": self.size(),
            "files": self.files.iter().map(|f| serde_json::json!({
                "name": f.name, "size": f.size, "restore_to": f.restore_to, "download": f.download,
                // Shown as is by the web UI.
                "change": f.change.label(), "title": f.title(), "detail": f.detail(),
                "project_id": f.src.as_ref().map_or(0, |r| r.project_id),
                "icon": f.src.as_ref().and_then(|r| r.addon.as_ref()).map(|a| a.icon.clone()).unwrap_or_default(),
                // The rest of the entry, for Hopper desktop.
                "src": f.src, "now": f.now, "now_site": f.now_site, "reason": f.reason,
                // Who did it ("" name: you), and for a record what Undo switches back.
                "who": f.who, "verb": f.change.verb(), "batch": f.batch,
                "record": f.record.as_ref().map(|(d, k)| serde_json::json!({ "did": d, "key": k })),
            })).collect::<Vec<_>>(),
            "head": self.headline(), "summary": self.summary(),
        })
    }

    /// Reads [`Set::to_json`] back. A panel from before the `src` field still
    /// gives rows that read right, only without versions under them.
    pub fn from_json(v: &serde_json::Value) -> Option<Set> {
        let when = v["when"].as_i64()?;
        let s = |v: &serde_json::Value, k: &str| v[k].as_str().unwrap_or_default().to_string();
        let files = v["files"]
            .as_array()?
            .iter()
            .map(|f| Saved {
                path: PathBuf::from(s(f, "name")),
                name: s(f, "name"),
                size: f["size"].as_u64().unwrap_or(0),
                restore_to: s(f, "restore_to"),
                download: f["download"].as_bool().unwrap_or(false),
                src: serde_json::from_value(f["src"].clone()).ok(),
                change: Change::from_label(&s(f, "change")),
                now: f["now"].as_str().map(str::to_string),
                now_site: match f["now_site"].as_str() {
                    Some("CurseForge") => "CurseForge",
                    Some("Modrinth") => "Modrinth",
                    _ => "",
                },
                reason: s(f, "reason"),
                who: serde_json::from_value(f["who"].clone()).ok(),
                batch: s(f, "batch"),
                record: serde_json::from_value(f["record"]["did"].clone()).ok().map(|d| (d, s(&f["record"], "key"))),
            })
            .collect();
        // Sets are told apart by their time; there is no folder here.
        Some(Set { dir: PathBuf::from(when.to_string()), when, files })
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

/// How many entries History holds from after `since` (a set's time, in
/// seconds), and the newest time. A directory listing only, so it's cheap
/// enough to poll for the History button's badge.
pub fn count_since(root: &Path, since: i64) -> (usize, i64) {
    let base = root.join(crate::sync::BACKUP_DIR);
    let (mut n, mut newest) = (0, 0);
    for e in fs::read_dir(base).into_iter().flatten().flatten() {
        let Ok(when) = e.file_name().to_string_lossy().parse::<i64>() else { continue };
        // A folder holding only its bookkeeping is not an entry.
        let has_file = fs::read_dir(e.path())
            .into_iter()
            .flatten()
            .flatten()
            .any(|f| !is_bookkeeping(&f.file_name().to_string_lossy()));
        if !has_file {
            continue;
        }
        newest = newest.max(when);
        if when > since {
            n += 1;
        }
    }
    (n, newest)
}

/// Everything set aside in an instance, newest first.
pub fn list(root: &Path) -> Vec<Set> {
    let base = root.join(crate::sync::BACKUP_DIR);
    let inst = crate::instances::load(root);
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
            let ids: std::collections::BTreeMap<String, Cand> =
                fs::read(dir.join(IDS)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
            let mut files = Vec::new();
            walk(&dir, &dir, &origins, &ids, &mut files);
            let reason = fs::read_to_string(dir.join(REASON)).unwrap_or_default();
            let tags = crate::actor::read_tags(&dir);
            for f in &mut files {
                f.reason = reason.clone();
                if let Some(t) = tags.get(&f.name) {
                    f.who = Some(t.who.clone());
                    f.batch = t.batch.clone();
                }
                judge(root, inst.as_ref(), f);
            }
            files.sort_by_key(|f| f.name.to_lowercase());
            (!files.is_empty()).then_some(Set { dir, when, files })
        })
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.when));
    out
}

/// Everything History shows for an instance, newest first: what [`list`]
/// has, plus mods added, turned on or off (records, nothing set aside),
/// with one set per sync or bulk action however many seconds it took.
pub fn history(root: &Path) -> Vec<Set> {
    let mut sets = list(root);
    let base = root.join(crate::sync::BACKUP_DIR);
    for e in fs::read_dir(&base).into_iter().flatten().flatten() {
        let Ok(when) = e.file_name().to_string_lossy().parse::<i64>() else { continue };
        let dir = e.path();
        let recs = crate::actor::read_records(&dir);
        if recs.is_empty() {
            continue;
        }
        let files: Vec<Saved> = recs.iter().map(|r| record_entry(&dir, r)).collect();
        match sets.iter_mut().find(|s| s.dir == dir) {
            Some(s) => {
                // A modpack update notes "turned on" by setting a note aside
                // too: that entry stays, the record goes.
                let shown: Vec<String> = s.files.iter().map(|f| f.name.trim_end_matches(".disabled").to_lowercase()).collect();
                s.files.extend(files.into_iter().filter(|f| !shown.contains(&f.name.to_lowercase())));
            }
            None => sets.push(Set { dir, when, files }),
        }
    }
    group_batches(sets)
}

/// A record as a History entry.
fn record_entry(dir: &Path, r: &crate::actor::Record) -> Saved {
    let a = &r.addon;
    let file = a.file_name.trim_end_matches(".disabled").to_string();
    let src = (a.project_id != 0).then(|| Ref {
        project_id: a.project_id,
        file_id: a.file_id,
        file_name: file.clone(),
        sha1: String::new(),
        size: 0,
        mr_version: a.mr_version.clone(),
        mr_project: a.mr_project.clone(),
        name: a.name.clone(),
        addon: Some(a.clone()),
    });
    let change = match r.did {
        crate::actor::Did::Added => Change::Added,
        crate::actor::Did::On => Change::TurnedOn,
        crate::actor::Did::Off => Change::TurnedOff,
    };
    Saved {
        path: dir.join(crate::actor::RECORDS_FILE),
        name: file.clone(),
        size: 0,
        restore_to: format!("{}/{file}", a.folder()),
        download: false,
        src,
        change,
        now: None,
        now_site: "",
        reason: String::new(),
        who: Some(r.who.clone()),
        batch: r.batch.clone(),
        record: Some((r.did, r.key.clone())),
    }
}

/// Splits and joins sets so each holds one batch: a sync's or a bulk
/// action's entries over several seconds become one set, and a change of
/// yours in the same second as a sync's stays out of it. Newest first.
fn group_batches(sets: Vec<Set>) -> Vec<Set> {
    let mut out: Vec<Set> = Vec::new();
    let mut at: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for s in sets {
        let mut plain = Vec::new();
        for f in s.files {
            if f.batch.is_empty() {
                plain.push(f);
                continue;
            }
            match at.get(&f.batch) {
                Some(&i) => {
                    out[i].when = out[i].when.max(s.when);
                    out[i].files.push(f);
                }
                None => {
                    at.insert(f.batch.clone(), out.len());
                    // Its own key: the folder it started in, and the batch.
                    let dir = PathBuf::from(format!("{}#{}", s.dir.display(), f.batch));
                    out.push(Set { dir, when: s.when, files: vec![f] });
                }
            }
        }
        if !plain.is_empty() {
            out.push(Set { dir: s.dir, when: s.when, files: plain });
        }
    }
    for s in &mut out {
        s.files.sort_by_key(|f| f.name.to_lowercase());
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.when));
    out
}

fn walk(
    base: &Path,
    dir: &Path,
    origins: &serde_json::Map<String, serde_json::Value>,
    ids: &std::collections::BTreeMap<String, Cand>,
    out: &mut Vec<Saved>,
) {
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if is_bookkeeping(&name) || name == crate::actor::RECORDS_FILE {
            continue;
        }
        if p.is_dir() {
            walk(base, &p, origins, ids, out);
            continue;
        }
        // A note stands for its jar: listed under the jar's name and size.
        let (p, name, size, download, src) = match name.strip_suffix(REF_EXT) {
            Some(jar) => {
                let jp = p.with_file_name(jar);
                let r = read_ref(&jp);
                let size = r.as_ref().map_or(0, |r| r.size);
                let jar = jar.to_string();
                (jp, jar, size, true, r)
            }
            None => {
                let size = e.metadata().map_or(0, |m| m.len());
                let src = ids.get(&name).map(|c| c.to_ref(String::new(), size));
                (p, name, size, false, src)
            }
        };
        let rel = p.strip_prefix(base).unwrap_or(&p).to_string_lossy().replace('\\', "/");
        let restore_to = match origins.get(&name).and_then(|v| v.as_str()) {
            Some(o) => o.to_string(),
            // A pack update keeps its copies under "pack-update/", with the
            // instance's folders below that.
            None if rel.starts_with("pack-update/") => rel["pack-update/".len()..].to_string(),
            // Kept under its folder ("mods/x.jar") by drag and drop and sync.
            None if rel.contains('/') => rel.clone(),
            None => guess(&p, &name),
        };
        out.push(Saved {
            size,
            name,
            path: p,
            restore_to,
            download,
            src,
            change: Change::Removed,
            now: None,
            now_site: "",
            reason: String::new(),
            who: None,
            batch: String::new(),
            record: None,
        });
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
    if let Some(dir) = f.path.parent() {
        drop_if_empty(dir);
    }
    Ok(dest)
}

/// Removes a History folder that holds nothing but its own notes.
pub(crate) fn drop_if_empty(dir: &Path) {
    if fs::read_dir(dir).is_ok_and(|mut d| d.all(|e| e.is_ok_and(|e| is_bookkeeping(&e.file_name().to_string_lossy())))) {
        let _ = fs::remove_dir_all(dir);
    }
}

fn is_jar(name: &str) -> bool {
    let l = name.to_lowercase();
    l.ends_with(".jar") || l.ends_with(".jar.disabled")
}

/// Beside a History folder's files: jar copies [`free_space`] looked up and
/// found no download for, so the offer doesn't come back for them.
const CHECKED: &str = ".checked.json";

fn checked(dir: &Path) -> std::collections::BTreeSet<String> {
    fs::read(dir.join(CHECKED)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Every jar copy History holds (not notes) that hasn't been looked up
/// yet, with its size.
fn jar_copies(root: &Path) -> Vec<(PathBuf, u64)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, u64)>) {
        let done = checked(dir);
        for e in fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if p.is_dir() {
                walk(&p, out);
            } else if is_jar(&name) && !done.contains(&name) {
                out.push((p, e.metadata().map_or(0, |m| m.len())));
            }
        }
    }
    let mut out = Vec::new();
    for d in fs::read_dir(root.join(crate::sync::BACKUP_DIR)).into_iter().flatten().flatten() {
        // Only action folders (named by the second): not snapshots or worlds.
        if d.file_name().to_str().is_some_and(|n| n.parse::<u64>().is_ok()) {
            walk(&d.path(), &mut out);
        }
    }
    out
}

/// Bytes of mod jars History keeps as full copies (from before 0.40, or
/// with no known source). [`free_space`] turns the ones CurseForge or
/// Modrinth still hands out into notes.
pub fn copies_size(root: &Path) -> u64 {
    jar_copies(root).iter().map(|(_, n)| n).sum()
}

/// Identified online by hash: which CurseForge / Modrinth file a copy is.
type Identify = fn(&[(PathBuf, String, u32)]) -> std::collections::HashMap<PathBuf, Ref>;

fn identify_online(jars: &[(PathBuf, String, u32)]) -> std::collections::HashMap<PathBuf, Ref> {
    let mut out = std::collections::HashMap::new();
    // CurseForge first, by fingerprint: most instances track their mods
    // there, so Undo puts the same entry back.
    let fps: Vec<u32> = jars.iter().map(|(_, _, fp)| *fp).collect();
    if let Ok(matches) = crate::cf::match_fingerprints(&fps) {
        for (p, sha1, fp) in jars {
            let Some(m) = matches.iter().find(|m| m.file.file_fingerprint == *fp as u64) else { continue };
            // Only files CurseForge lets launchers download, byte for byte.
            if m.file.sha1().is_none_or(|h| h != *sha1) || crate::cf::download_url(&m.file).is_err() {
                continue;
            }
            out.insert(p.clone(), Ref {
                project_id: m.id,
                file_id: m.file.id,
                file_name: m.file.file_name.clone(),
                sha1: sha1.clone(),
                size: m.file.file_length,
                mr_version: String::new(),
                mr_project: String::new(),
                name: String::new(),
                addon: None,
            });
        }
    }
    // Modrinth by sha1, for the rest.
    let rest: Vec<&(PathBuf, String, u32)> = jars.iter().filter(|(p, _, _)| !out.contains_key(p)).collect();
    let hashes: Vec<String> = rest.iter().map(|(_, h, _)| h.clone()).collect();
    for chunk in hashes.chunks(500) {
        let Ok(found) = crate::modrinth::versions_by_sha1(chunk) else { continue };
        for (p, sha1, _) in &rest {
            let Some(v) = found.get(sha1) else { continue };
            let Some(vf) = v.files.iter().find(|f| f.hashes.get("sha1") == Some(sha1)) else { continue };
            let f = crate::modrinth::version_to_file(v);
            out.insert(p.clone(), Ref {
                project_id: crate::modrinth::num_id(&v.project_id),
                file_id: f.id,
                file_name: vf.filename.clone(),
                sha1: sha1.clone(),
                size: vf.size,
                mr_version: v.id.clone(),
                mr_project: v.project_id.clone(),
                name: String::new(),
                addon: None,
            });
        }
    }
    out
}

fn identify_offline(_: &[(PathBuf, String, u32)]) -> std::collections::HashMap<PathBuf, Ref> {
    Default::default()
}

thread_local! {
    static IDENTIFY: std::cell::Cell<Option<Identify>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_test_identify(f: Identify) {
    IDENTIFY.with(|h| h.set(Some(f)));
}

/// The one-time clean up the user starts from History: turns every jar
/// copy CurseForge or Modrinth still hands out byte for byte into a note,
/// so Undo downloads it instead. Copies nothing knows stay. Returns (bytes
/// freed, bytes still kept as copies). Asks online: run it in the
/// background.
pub fn free_space(root: &Path) -> (u64, u64) {
    // Copies whose ids History noted when they were set aside.
    let mut freed = slim_older_than(root, 0);
    let identify = IDENTIFY.with(|h| h.get()).unwrap_or(if cfg!(test) { identify_offline } else { identify_online });
    let jars: Vec<(PathBuf, String, u32)> = jar_copies(root)
        .into_iter()
        .filter_map(|(p, _)| {
            let bytes = fs::read(&p).ok()?;
            let sha1 = sha1_smol::Sha1::from(&bytes).digest().to_string();
            Some((p, sha1, crate::cf::fingerprint(&bytes)))
        })
        .collect();
    let found = identify(&jars);
    let mut kept = 0;
    for (p, _, _) in &jars {
        let Some(r) = found.get(p) else {
            // Nothing hands it out: it stays, and isn't offered again.
            if let (Some(dir), Some(name)) = (p.parent(), p.file_name()) {
                let mut done = checked(dir);
                done.insert(name.to_string_lossy().into_owned());
                let _ = fs::write(dir.join(CHECKED), serde_json::to_vec(&done).unwrap_or_default());
            }
            kept += fs::metadata(p).map_or(0, |m| m.len());
            continue;
        };
        let note = ref_path(p);
        if fs::write(&note, serde_json::to_vec_pretty(r).unwrap_or_default()).is_ok() {
            if fs::remove_file(p).is_ok() {
                freed += r.size;
            } else {
                let _ = fs::remove_file(&note);
            }
        }
    }
    (freed, kept)
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

    fn fake_check(r: &Ref) -> bool {
        (r.project_id, r.file_id) == (5, 9) && r.sha1 == sha(REMOTE) && r.size == REMOTE.len() as u64
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
    fn a_known_jar_leaves_only_a_note_and_undo_downloads_it() {
        set_test_hooks(fake_check, fake_fetch);
        let root = instance("note");
        fs::write(root.join("mods/a.jar"), REMOTE).unwrap();
        fs::write(root.join("mods/b.jar"), "unknown jar").unwrap();
        let mut inst = crate::instances::load(&root).unwrap();
        crate::install::remove(&mut inst, 0).unwrap();
        let a = find(&root, &root.join("mods/a.jar"), 0).unwrap();
        let b = crate::sync::backup_into(&root, &root.join("mods/b.jar")).unwrap().unwrap();
        // a is deleted at once, a note in its place; b (unknown) is a copy.
        assert!(!root.join("mods/a.jar").exists() && !a.exists() && is_ref(&a));
        assert!(b.is_file() && !is_ref(&b));
        let r = read_ref(&a).unwrap();
        assert_eq!((r.project_id, r.file_id, r.sha1.as_str(), r.name.as_str()), (5, 9, sha(REMOTE).as_str(), "A"));
        assert!(r.addon.is_some_and(|x| x.enabled));
        let sets = list(&root);
        let files: Vec<&Saved> = sets.iter().flat_map(|s| &s.files).collect();
        let fa = files.iter().find(|f| f.name == "a.jar").unwrap();
        assert!(fa.download && fa.size == REMOTE.len() as u64 && fa.restore_to == "mods/a.jar");
        assert_eq!(fa.change, Change::Removed);
        let fb = files.iter().find(|f| f.name == "b.jar").unwrap();
        assert!(!fb.download && fb.src.is_none());
        // The remove's Undo toast finds it by the jar's name.
        assert_eq!(find(&root, &root.join("mods/a.jar"), 0), Some(a.clone()));
        // Undo from History downloads it again and puts its entry back.
        let back = crate::undo::undo(&mut inst, fa).unwrap();
        assert_eq!(fs::read(&back).unwrap(), REMOTE);
        assert!(!exists(&a));
        let inst = crate::instances::load(&root).unwrap();
        assert!(inst.addons.iter().any(|x| x.project_id == 5 && x.file_name == "a.jar" && x.enabled));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn undo_puts_a_disabled_mod_back_disabled() {
        set_test_hooks(fake_check, fake_fetch);
        let root = instance("off");
        let mut inst = crate::instances::load(&root).unwrap();
        inst.addons[0].enabled = false;
        inst.save().unwrap();
        fs::write(root.join("mods/a.jar.disabled"), REMOTE).unwrap();
        crate::install::remove(&mut inst, 0).unwrap();
        assert!(!root.join("mods/a.jar.disabled").exists() && inst.addons.is_empty());
        let f = list(&root).remove(0).files.remove(0);
        assert_eq!(f.restore_to, "mods/a.jar.disabled");
        crate::undo::undo(&mut inst, &f).unwrap();
        assert_eq!(fs::read(root.join("mods/a.jar.disabled")).unwrap(), REMOTE);
        assert!(inst.addons.iter().any(|x| x.file_name == "a.jar" && !x.enabled));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn undoing_an_update_rolls_back() {
        set_test_hooks(fake_check, fake_fetch);
        let root = instance("update");
        fs::write(root.join("mods/a.jar"), REMOTE).unwrap();
        // The update: a.jar (file 9) set aside, a2.jar (file 10) in.
        crate::sync::backup_into(&root, &root.join("mods/a.jar")).unwrap();
        let mut inst = crate::instances::load(&root).unwrap();
        inst.addons[0].file_id = 10;
        inst.addons[0].file_name = "a2.jar".into();
        inst.save().unwrap();
        fs::write(root.join("mods/a2.jar"), "v2").unwrap();
        let f = list(&root).remove(0).files.remove(0);
        assert_eq!((f.change, f.now.as_deref()), (Change::Updated, Some("a2.jar")));
        // Folders are named by the second: the roll back gets its own.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        crate::undo::undo(&mut inst, &f).unwrap();
        assert_eq!(fs::read(root.join("mods/a.jar")).unwrap(), REMOTE);
        assert!(!root.join("mods/a2.jar").exists());
        assert_eq!(inst.addons.len(), 1);
        assert_eq!(inst.addons[0].file_id, 9);
        // The newer file is History now, as a roll back to undo in turn.
        let f = list(&root).remove(0).files.remove(0);
        assert_eq!((f.name.as_str(), f.change), ("a2.jar", Change::RolledBack));
        fs::remove_dir_all(&root).unwrap();
    }

    fn not_found(_: &Ref, _: &Path) -> anyhow::Result<()> {
        anyhow::bail!("404")
    }

    fn two_others(_: &Ref) -> anyhow::Result<Vec<crate::cf::File>> {
        Ok(vec![
            crate::cf::File { id: 11, file_name: "a-1.1.jar".into(), ..Default::default() },
            crate::cf::File { id: 8, file_name: "a-0.9.jar".into(), ..Default::default() },
        ])
    }

    #[test]
    fn a_version_gone_upstream_offers_the_others() {
        set_test_hooks(fake_check, not_found);
        crate::undo::set_test_choices(two_others);
        let root = instance("gone");
        fs::write(root.join("mods/a.jar"), REMOTE).unwrap();
        let a = crate::sync::backup_into(&root, &root.join("mods/a.jar")).unwrap().unwrap();
        let f = list(&root).remove(0).files.remove(0);
        let mut inst = crate::instances::load(&root).unwrap();
        let e = crate::undo::undo(&mut inst, &f).unwrap_err();
        let gone = e.downcast_ref::<crate::undo::Gone>().unwrap();
        assert_eq!(gone.to_string(), "Version a is no longer on CurseForge");
        assert_eq!(gone.choices.len(), 2);
        // The newest not newer than file 9.
        assert_eq!(gone.choices[gone.closest].id, 8);
        // Nothing changed: the note stays, nothing half-downloaded.
        assert!(is_ref(&a) && !root.join("mods/a.jar.hopper-tmp").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_failed_download_says_so_and_keeps_the_note() {
        set_test_hooks(fake_check, failing_fetch);
        let root = instance("offline");
        fs::write(root.join("mods/a.jar"), REMOTE).unwrap();
        let a = crate::sync::backup_into(&root, &root.join("mods/a.jar")).unwrap().unwrap();
        let e = materialize(&a).unwrap_err().to_string();
        assert!(e.contains("a.jar") && e.contains("downloading it again failed") && e.contains("no connection"), "{e}");
        assert!(is_ref(&a));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn undoing_a_config_edit_puts_the_old_text_back() {
        let root = instance("config");
        fs::create_dir_all(root.join("config")).unwrap();
        fs::write(root.join("config/a.toml"), "old = 1").unwrap();
        crate::sync::backup_into(&root, &root.join("config/a.toml")).unwrap();
        fs::write(root.join("config/a.toml"), "new = 2").unwrap();
        let f = list(&root).remove(0).files.remove(0);
        assert_eq!(f.change, Change::Edited);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let mut inst = crate::instances::load(&root).unwrap();
        crate::undo::undo(&mut inst, &f).unwrap();
        assert_eq!(fs::read_to_string(root.join("config/a.toml")).unwrap(), "old = 1");
        // The text it had is History now, so the undo can be undone.
        let f = list(&root).remove(0).files.remove(0);
        assert_eq!(fs::read_to_string(&f.path).unwrap(), "new = 2");
        fs::remove_dir_all(&root).unwrap();
    }

    fn knows_c(jars: &[(PathBuf, String, u32)]) -> std::collections::HashMap<PathBuf, Ref> {
        jars.iter()
            .filter(|(p, _, _)| p.ends_with("c.jar"))
            .map(|(p, sha1, _)| {
                (p.clone(), Ref { project_id: 7, file_id: 3, file_name: "c.jar".into(), sha1: sha1.clone(), size: 6, mr_version: String::new(), mr_project: String::new(), name: String::new(), addon: None })
            })
            .collect()
    }

    #[test]
    fn freeing_space_turns_old_copies_into_notes() {
        set_test_identify(knows_c);
        let root = instance("free");
        // Copies from before notes: one the sites know, one they don't.
        let old = root.join(crate::sync::BACKUP_DIR).join("1000");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("c.jar"), "c jar!").unwrap();
        fs::write(old.join("mine.jar"), "hand made").unwrap();
        assert_eq!(copies_size(&root), 15);
        let (freed, kept) = free_space(&root);
        assert_eq!((freed, kept), (6, 9));
        assert!(is_ref(&old.join("c.jar")) && old.join("mine.jar").is_file());
        // Looked up once: the copy left isn't offered again.
        assert_eq!(copies_size(&root), 0);
        assert_eq!(list(&root)[0].files.len(), 2);
        fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    #[ignore]
    fn real_sites_round_trip() {
        let Ok(base) = std::env::var("HOPPER_REF_TEST_ROOT") else { return };
        set_test_hooks(check_online, fetch_online);
        let root = PathBuf::from(base).join("Ref Round Trip");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("mods")).unwrap();
        // Jade on CurseForge (324717) and AppleSkin on Modrinth, NeoForge 1.21.1.
        let cf = crate::cf::files(324717, "1.21.1", crate::cf::Loader::NeoForge).unwrap().into_iter().next().unwrap();
        let mr = crate::modrinth::versions("appleskin", "1.21.1", crate::cf::Loader::NeoForge).unwrap().into_iter().next().unwrap();
        let mrf = crate::modrinth::version_to_file(&mr);
        let mut addons = Vec::new();
        for (f, project, mr_version) in [(&cf, 324717u64, String::new()), (&mrf, crate::modrinth::num_id("appleskin"), mr.id.clone())] {
            let name = format!("{}{}", if mr_version.is_empty() { "cf-" } else { "mr-" }, f.file_name);
            let url = crate::cf::download_url(f).unwrap();
            crate::cf::download_checked(&url, &root.join("mods").join(&name), &f.expect(), None, |_| {}).unwrap();
            addons.push(serde_json::json!({"project_id": project, "file_id": f.id, "file_name": name, "name": name,
                "enabled": true, "mr_project": if mr_version.is_empty() { "" } else { "appleskin" }, "mr_version": mr_version}));
        }
        let v = serde_json::json!({"name": "Ref Round Trip", "format": crate::instances::FORMAT, "addons": addons});
        fs::write(root.join(crate::instances::HOPPER_FILE), serde_json::to_vec(&v).unwrap()).unwrap();
        let mut set_aside = Vec::new();
        for a in &addons {
            let jar = root.join("mods").join(a["file_name"].as_str().unwrap());
            let before = sha1_of(&jar).unwrap();
            let saved = crate::sync::backup_into(&root, &jar).unwrap().unwrap();
            set_aside.push((jar, saved, before));
        }
        let slimmed = slim_older_than(&root, 0);
        println!("slimmed {slimmed} bytes");
        for (jar, saved, before) in &set_aside {
            assert!(is_ref(saved), "{} wasn't turned into a note", saved.display());
            // Undo: where the remove put it, downloaded again.
            let found = find(&root, jar, 0).unwrap();
            materialize(&found).unwrap();
            let after = sha1_of(&found).unwrap();
            println!("{}: {before} -> {after}", found.file_name().unwrap().to_string_lossy());
            assert_eq!(&after, before);
        }
        fs::remove_dir_all(&root).unwrap();
    }

    fn saved_of(name: &str, now: &str, change: Change) -> Saved {
        let src = Ref {
            project_id: 1,
            file_id: 2,
            file_name: name.into(),
            sha1: String::new(),
            size: 1,
            mr_version: String::new(),
            mr_project: String::new(),
            name: "Brewin".into(),
            addon: None,
        };
        Saved {
            path: PathBuf::new(),
            name: name.into(),
            size: 1,
            restore_to: format!("mods/{name}"),
            download: true,
            src: Some(src),
            change,
            now: Some(now.into()),
            now_site: "CurseForge",
            reason: String::new(),
            who: None,
            batch: String::new(),
            record: None,
        }
    }

    #[test]
    fn detail_never_shows_the_same_version_twice() {
        // Only on or off: one version, no arrow.
        let mut f = saved_of("brewin-1.21.1.jar.disabled", "brewin-1.21.1.jar", Change::TurnedOn);
        f.reason = "Clean update".into();
        assert_eq!(f.detail(), "1.21.1 · CurseForge · by Clean update");
        // A real swap shows what differs.
        let f = saved_of("brewin-1.21.1.jar", "brewin-1.21.1-fixed.jar", Change::Switched);
        assert_eq!(f.detail(), "1.21.1 → 1.21.1-fixed · CurseForge");
        // Another site says so.
        let mut f = saved_of("brewin-1.21.1.jar", "brewin-1.21.1.jar", Change::Switched);
        f.now_site = "Modrinth";
        assert_eq!(f.detail(), "1.21.1 · CurseForge → Modrinth");
        assert_eq!(Change::TurnedOn.label(), "Turned on");
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

    #[test]
    fn pack_update_copies_go_back_to_their_folder() {
        let root = std::env::temp_dir().join(format!("hopper-backups-pu-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join(crate::sync::BACKUP_DIR).join("1000").join("pack-update").join("config");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.json"), "mine").unwrap();
        let sets = list(&root);
        assert_eq!(sets[0].files[0].restore_to, "config/a.json");
        let back = restore(&root, &sets[0].files[0]).unwrap();
        assert_eq!(back, root.join("config/a.json"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn set_json_round_trip() {
        let src: Ref = serde_json::from_value(serde_json::json!({
            "project_id": 7, "file_id": 9, "file_name": "jei-1.2.jar", "sha1": "", "size": 5, "name": "JEI",
        }))
        .unwrap();
        let set = Set {
            dir: PathBuf::from("x"),
            when: 1_700_000_000,
            files: vec![Saved {
                path: PathBuf::from("jei-1.2.jar"),
                name: "jei-1.2.jar".into(),
                size: 5,
                restore_to: "mods/jei-1.2.jar".into(),
                download: true,
                src: Some(src),
                change: Change::Updated,
                now: Some("jei-1.3.jar".into()),
                now_site: "CurseForge",
                reason: "Clean update".into(),
                who: Some(crate::actor::Who::friend("Mia", "mb1", "", "")),
                batch: "b1".into(),
                record: None,
            }],
        };
        let back = Set::from_json(&set.to_json()).unwrap();
        assert_eq!(back.when, set.when);
        assert_eq!(back.files, set.files.iter().map(|f| Saved { path: PathBuf::from(&f.name), ..f.clone() }).collect::<Vec<_>>());
        assert_eq!(back.files[0].detail(), set.files[0].detail());
        // Every label reads back as itself.
        for c in [Change::Removed, Change::Updated, Change::RolledBack, Change::Switched, Change::TurnedOn, Change::TurnedOff, Change::Edited, Change::Replaced] {
            assert_eq!(Change::from_label(c.label()), c);
        }
    }
}
