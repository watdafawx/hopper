//! Recently deleted: what a player deletes (an instance, a world, a backup,
//! a server) is moved aside instead of being destroyed, so it can be put
//! back for a while, without the Recycle Bin (which lives on C: and which
//! players forget to empty).
//!
//! A delete is a rename into a `.hopper-deleted` folder on the same drive as
//! the thing deleted, normally beside the instance or server it belongs to,
//! so it is instant and never copies anything. Each entry is a folder named
//! `<folder>-<seconds>` holding the moved folder's own files plus
//! [`META`], which says what it was, where it came from and when it goes.
//! A sweep removes entries past that time for good.
//!
//! Hopper Server keeps deleted servers the same way (30 days, in its
//! servers folder); Hopper keeps them [`KEEP_DAYS`]. Entries written before
//! the time was stored in them count as Hopper Server's.

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// The folder deleted things wait in.
pub const DIR: &str = ".hopper-deleted";
/// Inside each entry: what it was and where it goes back to.
pub const META: &str = ".hopper-deleted.json";
/// How long Hopper keeps what it deleted.
pub const KEEP_DAYS: u64 = 7;
/// Hopper Server's time, and the time of an entry that doesn't say.
pub const SERVER_KEEP_DAYS: u64 = 30;
const DAY: u64 = 86_400;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Instance,
    World,
    /// A snapshot (an instance's Backups): its manifest and the files only
    /// it used. Restoring puts them back into the instance's store.
    Backup,
    /// Hopper Server's entries say nothing: they are servers.
    #[default]
    Server,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Instance => "Instance",
            Kind::World => "World",
            Kind::Backup => "Backup",
            Kind::Server => "Server",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Meta {
    /// What the player called it.
    pub name: String,
    /// Where it goes back to.
    pub from: PathBuf,
    /// When it was deleted (seconds since the epoch).
    pub at: u64,
    pub kind: Kind,
    /// When it goes for good; none: [`SERVER_KEEP_DAYS`] after `at`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<u64>,
    /// Bytes, counted when it was deleted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// The instance or server a world or backup was in.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub owner: String,
}

impl Meta {
    pub fn new(kind: Kind, name: impl Into<String>, from: &Path) -> Self {
        let at = now();
        Meta { name: name.into(), from: from.to_path_buf(), at, kind, expires: Some(at + KEEP_DAYS * DAY), ..Default::default() }
    }

    pub fn owner(mut self, owner: impl Into<String>) -> Self {
        self.owner = owner.into();
        self
    }

    /// Kept `days` instead of [`KEEP_DAYS`].
    pub fn keep_days(mut self, days: u64) -> Self {
        self.expires = Some(self.at + days * DAY);
        self
    }
}

/// One deleted thing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The entry's folder inside a [`DIR`].
    pub dir: PathBuf,
    pub meta: Meta,
}

impl Entry {
    /// The entry folder's name: how the UI and the API refer to it.
    pub fn id(&self) -> String {
        self.dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    pub fn expires(&self) -> u64 {
        self.meta.expires.unwrap_or(self.meta.at + SERVER_KEEP_DAYS * DAY)
    }

    /// Bytes: as counted at delete time, else counted now.
    pub fn size(&self) -> u64 {
        self.meta.size.unwrap_or_else(|| crate::instance_ops::folder_size(&self.dir))
    }
}

/// "Goes today", "Goes tomorrow", "Goes in 5 days".
pub fn goes_in(expires: u64, now: u64) -> String {
    match expires.saturating_sub(now).div_ceil(DAY) {
        0 => "Goes today".into(),
        1 => "Goes tomorrow".into(),
        n => format!("Goes in {n} days"),
    }
}

/// An entry's line under its name: "World from ATMons · deleted
/// yesterday · goes in 6 days".
pub fn caption(e: &Entry, now: u64) -> String {
    let what = if e.meta.owner.is_empty() { e.meta.kind.label().to_string() } else { format!("{} from {}", e.meta.kind.label(), e.meta.owner) };
    let when = crate::time::relative_secs(e.meta.at as i64).to_lowercase();
    format!("{what} · deleted {when} · {}", goes_in(e.expires(), now).to_lowercase())
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Where things deleted from `owner` (an instance or server folder) go:
/// beside it first (one place for a whole instances folder), else inside
/// it. Both are on its drive, short of a junction.
pub fn places_for(owner: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = owner.parent().filter(|p| !p.as_os_str().is_empty()) {
        v.push(p.join(DIR));
    }
    v.push(owner.join(DIR));
    v
}

/// The drive a path is on: `C:`, `\\server\share`, or (elsewhere) the
/// device of its nearest existing folder.
fn volume(p: &Path) -> Option<String> {
    let abs = std::path::absolute(p).ok()?;
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        match abs.components().next()? {
            Component::Prefix(px) => Some(match px.kind() {
                Prefix::Disk(d) | Prefix::VerbatimDisk(d) => format!("{}:", (d as char).to_ascii_uppercase()),
                Prefix::UNC(s, sh) | Prefix::VerbatimUNC(s, sh) => {
                    format!(r"\\{}\{}", s.to_string_lossy(), sh.to_string_lossy()).to_lowercase()
                }
                _ => return None,
            }),
            _ => None,
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let mut cur = abs.as_path();
        loop {
            if let Ok(m) = fs::metadata(cur) {
                return Some(m.dev().to_string());
            }
            cur = cur.parent()?;
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = abs;
        None
    }
}

/// Whether `a` and `b` are on the same drive, so moving one next to the
/// other is a rename.
pub fn same_volume(a: &Path, b: &Path) -> bool {
    matches!((volume(a), volume(b)), (Some(x), Some(y)) if x == y)
}

/// Windows' "not the same device" (17) or EXDEV (18): a rename that
/// would have to copy.
fn other_device(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(17) | Some(18))
}

/// A name for `name` in `dir` that's free: `<name>-<at>`, then `-2`, `-3`...
fn free_entry(dir: &Path, name: &str, at: u64) -> PathBuf {
    let mut dest = dir.join(format!("{name}-{at}"));
    let mut n = 2;
    while dest.exists() {
        dest = dir.join(format!("{name}-{at}-{n}"));
        n += 1;
    }
    dest
}

/// Moves `item` (a folder) into the first of `places` on its drive.
/// Only ever a rename: a place on another drive is skipped, and when none
/// fits nothing is moved. A folder something has open is refused with the
/// program's name.
pub fn move_in(item: &Path, places: &[PathBuf], mut meta: Meta) -> anyhow::Result<Entry> {
    let what = if meta.name.is_empty() { item.display().to_string() } else { meta.name.clone() };
    let folder = item.file_name().context("the folder has no name")?.to_string_lossy().into_owned();
    if !item.is_dir() {
        anyhow::bail!("{} isn't a folder", item.display());
    }
    for place in places {
        // Not into itself, and never onto another drive (that would copy).
        if place.starts_with(item) || !same_volume(item, place) {
            continue;
        }
        fs::create_dir_all(place).with_context(|| format!("creating {}", place.display()))?;
        let dest = free_entry(place, &folder, meta.at);
        match crate::hosting::fsmove::rename_retry(item, &dest, &|| {}, &|a, b| fs::rename(a, b), &crate::hosting::fsmove::RETRY_DELAYS) {
            Ok(()) => {}
            Err(e) if other_device(&e) => {
                let _ = fs::remove_dir(place);
                continue;
            }
            Err(e) => {
                let who = if crate::hosting::fsmove::is_locked(&e) { crate::hosting::fsmove::holders(item) } else { Vec::new() };
                anyhow::bail!(crate::hosting::fsmove::locked_message(&what, &who, &e));
            }
        }
        meta.size = Some(crate::instance_ops::folder_size(&dest));
        if let Err(e) = write_meta(&dest, &meta) {
            // Without its note it can't be put back: undo the move.
            return Err(match fs::rename(&dest, item) {
                Ok(()) => e.context(format!("Couldn't move {what} to Recently deleted. Nothing was deleted")),
                Err(e2) => e.context(format!("Couldn't finish deleting {what}, and couldn't put it back ({e2}). It is at {}", dest.display())),
            });
        }
        remember(place);
        return Ok(Entry { dir: dest, meta });
    }
    anyhow::bail!(
        "Couldn't move {what} to Recently deleted: there's no room for it on its own drive. Tick \"Delete for good now\" to delete it instead."
    )
}

/// Writes an entry's note (Hopper Server writes its own entries with it).
pub fn write_meta(entry_dir: &Path, meta: &Meta) -> anyhow::Result<()> {
    fs::write(entry_dir.join(META), serde_json::to_vec_pretty(meta)?).with_context(|| format!("writing {}", entry_dir.join(META).display()))
}

fn read_meta(entry_dir: &Path) -> Meta {
    fs::read(entry_dir.join(META)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// What `place` (a [`DIR`] folder) holds, newest first.
pub fn list(place: &Path) -> Vec<Entry> {
    let mut v: Vec<Entry> = fs::read_dir(place)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| !e.file_name().to_string_lossy().starts_with(crate::hosting::fsmove::DELETING_PREFIX))
        .map(|e| {
            let dir = e.path();
            Entry { meta: read_meta(&dir), dir }
        })
        .collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.meta.at));
    v
}

/// [`list`] over several places, newest first.
pub fn list_all(places: &[PathBuf]) -> Vec<Entry> {
    let mut v: Vec<Entry> = places.iter().flat_map(|p| list(p)).collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.meta.at));
    v
}

/// The entry `id` in `place`; `id` is a bare folder name.
pub fn find(place: &Path, id: &str) -> Option<Entry> {
    if id.is_empty() || id.contains(['/', '\\']) || id.contains("..") || id.starts_with('.') {
        return None;
    }
    let dir = place.join(id);
    dir.is_dir().then(|| Entry { meta: read_meta(&dir), dir })
}

/// Where a restore of something from `from` lands: `from` itself when
/// it's free, else "<name> (restored)", "<name> (restored 2)"...
pub fn restore_target(from: &Path) -> PathBuf {
    if !from.exists() {
        return from.to_path_buf();
    }
    let name = from.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let parent = from.parent().unwrap_or(Path::new(""));
    let mut n = 1;
    loop {
        let p = parent.join(if n == 1 { format!("{name} (restored)") } else { format!("{name} (restored {n})") });
        if !p.exists() {
            return p;
        }
        n += 1;
    }
}

/// Puts an entry back where it came from, and returns where it landed
/// (another name when that one is taken again). A backup's files go back
/// into its instance's store.
pub fn restore(entry: &Entry) -> anyhow::Result<PathBuf> {
    let from = &entry.meta.from;
    let name = if entry.meta.name.is_empty() { entry.id() } else { entry.meta.name.clone() };
    if from.as_os_str().is_empty() {
        anyhow::bail!("Hopper doesn't know where {name} came from.");
    }
    let parent = from.parent().context("it has no folder to go back to")?;
    if entry.meta.kind == Kind::Backup {
        // The store's folder may be gone with its last snapshot; the
        // instance must still be there.
        let inst = parent.parent().unwrap_or(parent);
        if !inst.is_dir() {
            anyhow::bail!("{name} was in {}, which is gone. Restore that first.", gone_name(entry, inst));
        }
        fs::create_dir_all(from)?;
        merge_into(&entry.dir, from, true)?;
        crate::hosting::fsmove::remove_tree(&entry.dir).map_err(anyhow::Error::msg)?;
        return Ok(from.clone());
    }
    if !parent.is_dir() {
        anyhow::bail!("{name} was in {}, which is gone. Restore that first.", gone_name(entry, parent));
    }
    let to = restore_target(from);
    fs::rename(&entry.dir, &to).with_context(|| format!("moving {name} back to {}", to.display()))?;
    let _ = fs::remove_file(to.join(META));
    Ok(to)
}

fn gone_name(entry: &Entry, dir: &Path) -> String {
    if entry.meta.owner.is_empty() { dir.display().to_string() } else { entry.meta.owner.clone() }
}

/// Moves every file under `src` to the same place under `dest`, keeping
/// what `dest` already has (a snapshot store's blobs are named by hash).
fn merge_into(src: &Path, dest: &Path, top: bool) -> anyhow::Result<()> {
    for e in fs::read_dir(src)?.flatten() {
        let name = e.file_name();
        if top && name == META {
            continue;
        }
        let (from, to) = (e.path(), dest.join(&name));
        if e.file_type()?.is_dir() {
            fs::create_dir_all(&to)?;
            merge_into(&from, &to, false)?;
        } else if !to.exists() {
            fs::rename(&from, &to).with_context(|| format!("moving {} back", to.display()))?;
        }
    }
    Ok(())
}

/// Deletes an entry for good; returns the bytes freed. Renamed first, so a
/// half-removed entry never shows as something to restore.
pub fn purge(entry: &Entry) -> anyhow::Result<u64> {
    let bytes = entry.size();
    let place = entry.dir.parent().context("no folder")?;
    let tmp = place.join(format!("{}{}", crate::hosting::fsmove::DELETING_PREFIX, entry.id()));
    fs::rename(&entry.dir, &tmp).with_context(|| format!("deleting {}", entry.meta.name))?;
    crate::hosting::fsmove::remove_tree(&tmp).map_err(anyhow::Error::msg)?;
    Ok(bytes)
}

/// What a sweep or Empty now removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Swept {
    pub count: usize,
    pub bytes: u64,
}

/// Removes for good the entries in `places` whose time is up by `now`
/// (all of them with `everything`), and what an earlier purge left half
/// done. An emptied place folder goes too.
pub fn sweep(places: &[PathBuf], now: u64, everything: bool) -> Swept {
    let mut out = Swept::default();
    for place in places {
        crate::hosting::fsmove::sweep_deleting(place);
        for e in list(place) {
            if !everything && e.expires() > now {
                continue;
            }
            match purge(&e) {
                Ok(b) => {
                    log::info!("removed {} for good (deleted {})", e.dir.display(), e.meta.at);
                    out.count += 1;
                    out.bytes += b;
                }
                Err(err) => log::warn!("removing {}: {err:#}", e.dir.display()),
            }
        }
        let _ = fs::remove_dir(place);
    }
    out
}

// ---- where the places are ----------------------------------------------------

/// Hopper's list of the places it ever used, so a sweep finds them all
/// (an instance in another launcher's folder, a server elsewhere).
fn registry() -> Option<PathBuf> {
    crate::paths::dirs().map(|d| d.data_dir().join("recently-deleted.json"))
}

fn remember(place: &Path) {
    if let Some(file) = registry() {
        remember_in(&file, place);
    }
}

fn remember_in(file: &Path, place: &Path) {
    let mut v = places_in(file);
    if v.iter().any(|p| p == place) {
        return;
    }
    v.push(place.to_path_buf());
    if let Some(d) = file.parent() {
        let _ = fs::create_dir_all(d);
    }
    let _ = fs::write(file, serde_json::to_vec_pretty(&v).unwrap_or_default());
}

fn places_in(file: &Path) -> Vec<PathBuf> {
    fs::read(file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Every place to look in: the remembered ones and `beside` (the folders
/// that hold the instances and servers Hopper knows now), each once.
pub fn known_places(beside: &[PathBuf]) -> Vec<PathBuf> {
    let mut v = registry().map(|f| places_in(&f)).unwrap_or_default();
    for b in beside {
        let p = b.join(DIR);
        if !v.contains(&p) {
            v.push(p);
        }
    }
    v.retain(|p| p.is_dir());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("hopper-rd-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn folder(dir: &Path, file: &str) {
        fs::create_dir_all(dir.join("mods")).unwrap();
        fs::write(dir.join("mods").join(file), file).unwrap();
    }

    /// A server Hopper Server deleted before this module (its note has only
    /// name, from and at) lists as a server kept 30 days, and comes back
    /// beside a new server of the same folder name instead of failing.
    #[test]
    fn old_hopper_server_entry() {
        let root = scratch("oldsrv");
        let place = root.join(DIR);
        let src = root.join("smp");
        let dir = place.join("smp-100");
        folder(&dir, "a.jar");
        fs::write(dir.join(META), format!(r#"{{"name":"SMP","from":{},"at":100}}"#, serde_json::to_string(&src).unwrap())).unwrap();
        folder(&src, "new.jar");
        let e = &list(&place)[0];
        assert_eq!((e.meta.kind, e.meta.name.as_str(), e.expires()), (Kind::Server, "SMP", 100 + SERVER_KEEP_DAYS * DAY));
        let back = restore(e).unwrap();
        assert_eq!(back, root.join("smp (restored)"));
        assert!(back.join("mods/a.jar").is_file() && src.join("mods/new.jar").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn move_then_restore() {
        let root = scratch("move");
        let inst = root.join("Pack");
        folder(&inst, "a.jar");
        let e = move_in(&inst, &places_for(&inst), Meta::new(Kind::Instance, "Pack", &inst)).unwrap();
        assert!(!inst.exists());
        // Beside it, in the instances folder: one place for all of them.
        assert_eq!(e.dir.parent().unwrap(), root.join(DIR));
        assert!(e.id().starts_with("Pack-"));
        assert_eq!(e.meta.size, Some(5));
        let listed = list(&root.join(DIR));
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].meta.kind, Kind::Instance);
        assert_eq!(listed[0].meta.name, "Pack");
        assert_eq!(listed[0].expires(), listed[0].meta.at + KEEP_DAYS * DAY);
        assert_eq!(find(&root.join(DIR), &e.id()).unwrap().dir, e.dir);
        assert!(find(&root.join(DIR), "..").is_none());

        let back = restore(&listed[0]).unwrap();
        assert_eq!(back, inst);
        assert_eq!(fs::read_to_string(inst.join("mods/a.jar")).unwrap(), "a.jar");
        assert!(!inst.join(META).exists());
        assert!(list(&root.join(DIR)).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_never_overwrites() {
        let root = scratch("clash");
        let inst = root.join("Pack");
        folder(&inst, "old.jar");
        let e1 = move_in(&inst, &places_for(&inst), Meta::new(Kind::Instance, "Pack", &inst)).unwrap();
        // A new instance took the name, then that was deleted too.
        folder(&inst, "new.jar");
        let e2 = move_in(&inst, &places_for(&inst), Meta::new(Kind::Instance, "Pack", &inst)).unwrap();
        assert_ne!(e1.dir, e2.dir);
        folder(&inst, "newest.jar");
        let a = restore(&e1).unwrap();
        assert_eq!(a, root.join("Pack (restored)"));
        assert!(a.join("mods/old.jar").exists());
        let b = restore(&e2).unwrap();
        assert_eq!(b, root.join("Pack (restored 2)"));
        assert!(b.join("mods/new.jar").exists());
        assert!(inst.join("mods/newest.jar").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn world_needs_its_instance() {
        let root = scratch("world");
        let inst = root.join("Pack");
        let world = inst.join("saves").join("My World");
        fs::create_dir_all(&world).unwrap();
        fs::write(world.join("level.dat"), "x").unwrap();
        let e = move_in(&world, &places_for(&inst), Meta::new(Kind::World, "My World", &world).owner("Pack")).unwrap();
        assert_eq!(e.dir.parent().unwrap(), root.join(DIR));
        // The instance goes for good: its world can't come back alone.
        fs::remove_dir_all(&inst).unwrap();
        let err = restore(&e).unwrap_err().to_string();
        assert!(err.contains("Pack, which is gone"), "{err}");
        assert!(e.dir.exists());
        fs::create_dir_all(inst.join("saves")).unwrap();
        assert_eq!(restore(&e).unwrap(), world);
        assert!(world.join("level.dat").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn backup_merges_into_the_store() {
        let root = scratch("backup");
        let inst = root.join("Pack");
        let store = inst.join(".hopper-backups").join("snapshots");
        fs::create_dir_all(store.join("blobs/ab")).unwrap();
        fs::write(store.join("blobs/ab/abkept"), "kept").unwrap();
        let out = inst.join(".hopper-backups").join(".pruned-1");
        fs::create_dir_all(out.join("blobs/cd")).unwrap();
        fs::write(out.join("1.json"), "{}").unwrap();
        fs::write(out.join("blobs/cd/cdgone"), "gone").unwrap();
        let e = move_in(&out, &places_for(&inst), Meta::new(Kind::Backup, "Snapshot", &store).owner("Pack")).unwrap();
        assert!(!out.exists());
        assert_eq!(restore(&e).unwrap(), store);
        assert!(store.join("1.json").exists());
        assert_eq!(fs::read_to_string(store.join("blobs/cd/cdgone")).unwrap(), "gone");
        assert_eq!(fs::read_to_string(store.join("blobs/ab/abkept")).unwrap(), "kept");
        assert!(!store.join(META).exists());
        assert!(!e.dir.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sweep_after_seven_days() {
        let root = scratch("sweep");
        let place = root.join(DIR);
        let (a, b) = (root.join("Old"), root.join("New"));
        folder(&a, "a.jar");
        folder(&b, "b.jar");
        let mut old = Meta::new(Kind::Instance, "Old", &a);
        old.at -= 8 * DAY;
        old = old.keep_days(KEEP_DAYS);
        move_in(&a, &places_for(&a), old).unwrap();
        move_in(&b, &places_for(&b), Meta::new(Kind::Instance, "New", &b)).unwrap();
        // Hopper Server's own entry: no time stored, kept 30 days.
        let srv = place.join("smp-1");
        fs::create_dir_all(&srv).unwrap();
        fs::write(srv.join(META), format!(r#"{{"name":"smp","from":"x","at":{}}}"#, now() - 10 * DAY)).unwrap();
        let s = sweep(&[place.clone()], now(), false);
        assert_eq!(s.count, 1);
        assert_eq!(s.bytes, 5);
        let left: Vec<String> = list(&place).iter().map(|e| e.meta.name.clone()).collect();
        assert_eq!(left, ["New", "smp"]);
        assert_eq!(list(&place).iter().find(|e| e.meta.name == "smp").unwrap().meta.kind, Kind::Server);
        // A week on, the new one goes; the server's waits for its 30 days.
        assert_eq!(sweep(&[place.clone()], now() + 7 * DAY + 1, false).count, 1);
        assert_eq!(list(&place).len(), 1);
        // Empty now: everything, and the emptied folder.
        assert_eq!(sweep(&[place.clone()], now(), true).count, 1);
        assert!(!place.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn same_drive_only() {
        let root = scratch("drive");
        assert!(same_volume(&root, &root.join("x/y")));
        #[cfg(windows)]
        {
            assert!(same_volume(Path::new(r"c:\a"), Path::new(r"C:\b")));
            assert!(!same_volume(Path::new(r"C:\a"), Path::new(r"D:\a")));
            assert!(same_volume(Path::new(r"\\Srv\Share\a"), Path::new(r"\\srv\share\b")));
            assert!(!same_volume(Path::new(r"\\srv\one\a"), Path::new(r"\\srv\two\a")));
        }
        // A place on another drive is skipped; with none left, nothing moves.
        let item = root.join("Pack");
        folder(&item, "a.jar");
        let other = if cfg!(windows) {
            let here = volume(&root).unwrap();
            // Only compared, never created or touched.
            PathBuf::from(if here == "B:" { r"A:\nowhere" } else { r"B:\nowhere" }).join(DIR)
        } else {
            PathBuf::from("/proc/hopper-nowhere").join(DIR)
        };
        let err = move_in(&item, &[other.clone()], Meta::new(Kind::Instance, "Pack", &item)).unwrap_err().to_string();
        assert!(err.contains("Delete for good now"), "{err}");
        assert!(item.join("mods/a.jar").exists());
        assert!(!other.exists());
        // The next place on its own drive is used instead.
        let e = move_in(&item, &[other, root.join(DIR)], Meta::new(Kind::Instance, "Pack", &item)).unwrap();
        assert_eq!(e.dir.parent().unwrap(), root.join(DIR));
        // Never into itself.
        let inner = root.join("Inner");
        folder(&inner, "b.jar");
        let e = move_in(&inner, &[inner.join(DIR), root.join(DIR)], Meta::new(Kind::Instance, "Inner", &inner)).unwrap();
        assert_eq!(e.dir.parent().unwrap(), root.join(DIR));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn words_for_the_list() {
        let t = 1_000 * DAY;
        assert_eq!(goes_in(t, t), "Goes today");
        assert_eq!(goes_in(t + 60, t), "Goes tomorrow");
        assert_eq!(goes_in(t + DAY, t), "Goes tomorrow");
        assert_eq!(goes_in(t + 6 * DAY + 1, t), "Goes in 7 days");
        let mut e = Entry { dir: PathBuf::from("x"), meta: Meta::new(Kind::World, "New World", Path::new("w")).owner("ATMons") };
        let c = caption(&e, e.meta.at);
        assert_eq!(c, "World from ATMons · deleted today · goes in 7 days");
        e.meta.owner.clear();
        e.meta.kind = Kind::Instance;
        assert!(caption(&e, e.meta.at).starts_with("Instance · deleted today"));
    }

    #[test]
    fn registry_remembers_each_place_once() {
        let root = scratch("reg");
        let file = root.join("data/recently-deleted.json");
        remember_in(&file, &root.join("a").join(DIR));
        remember_in(&file, &root.join("a").join(DIR));
        remember_in(&file, &root.join("b").join(DIR));
        assert_eq!(places_in(&file), [root.join("a").join(DIR), root.join("b").join(DIR)]);
        let _ = fs::remove_dir_all(&root);
    }
}
