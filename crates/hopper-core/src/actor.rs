//! Who made each change History shows: you on this PC, a friend whose
//! pack you follow, a member of a group you keep in step with, or a
//! Hopper Server panel user.
//!
//! A sync marks the work it does on its own thread ([`begin`]) and names
//! the mods it changes ([`claim`]); anything else, even at the same moment
//! on another thread, stays yours. Every History entry made while a mark is
//! up shares its batch, so one sync (or one bulk action) is one row.
//!
//! History also notes what sets nothing aside: a mod added, turned on or
//! turned off. Those are records only ([`Record`]); no copy of the file is
//! kept. Undo removes an added mod and switches the others back.

use crate::instances::{Addon, Instance};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

/// Who made a change. An empty `name` is you, on this PC.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct Who {
    /// The name shown ("Mia"); empty for you.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// A friend's mailbox id (to find their face), or "panel:<user>" for a
    /// Hopper Server user.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// The friend group a two-way change came through (its id).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub group: String,
    /// The followed pack's name, for "Updated from Robin's All the Mods 10".
    #[serde(skip_serializing_if = "String::is_empty")]
    pub pack: String,
    /// Their Minecraft profile id when they were signed in, so History
    /// shows their face even after they stop being a friend.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub mc: String,
}

impl Who {
    pub fn you() -> Who {
        Who::default()
    }

    /// A friend: their pack you follow (`pack`), or a group's copy (`group`).
    pub fn friend(name: &str, id: &str, group: &str, pack: &str) -> Who {
        Who { name: name.to_string(), id: id.to_string(), group: group.to_string(), pack: pack.to_string(), mc: String::new() }
    }

    /// The same person with their Minecraft profile id, for their face.
    pub fn with_face(mut self, mc: &str) -> Who {
        self.mc = mc.to_string();
        self
    }

    /// A Hopper Server panel user.
    pub fn panel(user: &str) -> Who {
        Who { name: user.to_string(), id: format!("panel:{user}"), ..Default::default() }
    }

    pub fn is_you(&self) -> bool {
        self.name.is_empty()
    }

    /// "You" or their name.
    pub fn label(&self) -> String {
        if self.is_you() { "You".to_string() } else { self.name.clone() }
    }
}

/// What a record-only History entry was.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Did {
    Added,
    On,
    Off,
}

/// A change that set nothing aside, kept in a History folder's
/// `.records.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub did: Did,
    /// The mod's key ([`crate::groupsync::addon_key`]), to find it again.
    pub key: String,
    /// Its entry as it was after the change.
    pub addon: Addon,
    #[serde(default)]
    pub who: Who,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub batch: String,
}

/// Beside a History folder's files: who set each aside, and its batch.
pub const WHO_FILE: &str = ".who.json";
/// Beside a History folder's files (or alone in it): record-only entries.
pub const RECORDS_FILE: &str = ".records.json";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tag {
    pub who: Who,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub batch: String,
}

struct Scope {
    root: String,
    batch: String,
    /// Who did whatever isn't claimed below; None: you.
    default: Option<Who>,
    /// Mod keys this scope's work changes, and by whom.
    keys: HashMap<String, Who>,
    /// File names (lower case, no ".disabled") of the mods it changes.
    files: HashMap<String, Who>,
    /// Every change is this scope's (a bulk action of yours, or a panel
    /// user's request), not only claimed ones.
    all: bool,
    /// Undo: nothing it does becomes a record.
    quiet: bool,
}

thread_local! {
    static SCOPES: RefCell<Vec<Scope>> = const { RefCell::new(Vec::new()) };
}

fn norm(root: &Path) -> String {
    root.to_string_lossy().replace('\\', "/").trim_end_matches('/').to_lowercase()
}

fn base(file: &str) -> String {
    file.trim_end_matches(".disabled").to_lowercase()
}

fn new_batch() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    format!("{ms}-{}", N.fetch_add(1, Ordering::Relaxed))
}

/// Ends its scope when dropped.
#[must_use = "the scope ends when this is dropped"]
pub struct Guard(());

impl Drop for Guard {
    fn drop(&mut self) {
        SCOPES.with(|s| {
            s.borrow_mut().pop();
        });
    }
}

fn push(s: Scope) -> Guard {
    SCOPES.with(|v| v.borrow_mut().push(s));
    Guard(())
}

/// A sync's work on `root`, on this thread, until the guard drops. Only
/// what it [`claim`]s is `who`'s in the add / on / off records; files it
/// sets aside are `who`'s when claimed, else `who` (when given) as the
/// sync's own.
pub fn begin(root: &Path, who: Option<Who>) -> Guard {
    push(Scope { root: norm(root), batch: new_batch(), default: who, keys: HashMap::new(), files: HashMap::new(), all: false, quiet: false })
}

/// One action of `who` (you, a panel user) that changes many mods: one
/// History row for all of it.
pub fn batch(root: &Path, who: Who) -> Guard {
    push(Scope { root: norm(root), batch: new_batch(), default: Some(who), keys: HashMap::new(), files: HashMap::new(), all: true, quiet: false })
}

/// Undo's own changes on `root`: not recorded again.
pub fn quiet(root: &Path) -> Guard {
    push(Scope { root: norm(root), batch: String::new(), default: None, keys: HashMap::new(), files: HashMap::new(), all: false, quiet: true })
}

/// Marks a mod (by key, and its file when known) as changed by `who` in
/// the innermost scope on this thread. Nothing without one.
pub fn claim(key: &str, file: Option<&str>, who: &Who) {
    SCOPES.with(|v| {
        if let Some(s) = v.borrow_mut().last_mut() {
            if !key.is_empty() {
                s.keys.insert(key.to_string(), who.clone());
            }
            if let Some(f) = file.filter(|f| !f.is_empty()) {
                s.files.insert(base(f), who.clone());
            }
        }
    });
}

/// Until dropped, everything this thread changes in the innermost scope
/// is its own, claimed or not: a pack update a follow runs, which changes
/// whatever the pack's new version changes.
pub fn own_all() -> OwnAll {
    SCOPES.with(|v| {
        if let Some(s) = v.borrow_mut().last_mut() {
            s.all = true;
        }
    });
    OwnAll(())
}

pub struct OwnAll(());

impl Drop for OwnAll {
    fn drop(&mut self) {
        SCOPES.with(|v| {
            if let Some(s) = v.borrow_mut().last_mut() {
                s.all = false;
            }
        });
    }
}

/// [`claim`] for an entry, as the scope's own.
pub fn claim_addon(a: &Addon) {
    let who = SCOPES.with(|v| v.borrow().last().and_then(|s| s.default.clone())).unwrap_or_default();
    claim(&crate::groupsync::addon_key(a), Some(&a.file_name), &who);
}

/// [`claim`] by key alone, as the scope's own.
pub fn claim_key(key: &str) {
    let who = SCOPES.with(|v| v.borrow().last().and_then(|s| s.default.clone())).unwrap_or_default();
    claim(key, None, &who);
}

/// Who set `file` aside in `root` just now, and its batch.
pub(crate) fn tag_for_file(root: &Path, file: &str) -> Tag {
    let r = norm(root);
    let f = base(file);
    SCOPES.with(|v| {
        let v = v.borrow();
        let Some(s) = v.iter().rev().find(|s| s.root == r && !s.quiet) else { return Tag::default() };
        let mine = |who: Who| Tag { who, batch: s.batch.clone() };
        if let Some(w) = s.files.get(&f) {
            return mine(w.clone());
        }
        match &s.default {
            Some(w) => mine(w.clone()),
            None => Tag::default(),
        }
    })
}

/// Who changed the mod `a` in `root` just now; None when it isn't
/// recorded (Undo).
fn tag_for_addon(root: &Path, a: &Addon) -> Option<Tag> {
    let r = norm(root);
    SCOPES.with(|v| {
        let v = v.borrow();
        let Some(s) = v.iter().rev().find(|s| s.root == r) else { return Some(Tag::default()) };
        if s.quiet {
            return None;
        }
        let mine = |who: Who| Some(Tag { who, batch: s.batch.clone() });
        if let Some(w) = s.keys.get(&crate::groupsync::addon_key(a)).or_else(|| s.files.get(&base(&a.file_name))) {
            return mine(w.clone());
        }
        if s.all {
            return mine(s.default.clone().unwrap_or_default());
        }
        // Not this sync's change: yours, made meanwhile.
        Some(Tag::default())
    })
}

#[cfg(test)]
thread_local! {
    static RECORD_IN_TESTS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Tests that check records turn them on; other tests' saves don't write
/// History folders they would count.
#[cfg(test)]
pub(crate) fn record_in_tests() {
    RECORD_IN_TESTS.with(|c| c.set(true));
}

pub(crate) fn recording() -> bool {
    #[cfg(test)]
    {
        RECORD_IN_TESTS.with(|c| c.get())
    }
    #[cfg(not(test))]
    {
        true
    }
}

/// The record-only changes from `old` to `new`: mods added, turned on or
/// turned off. A mod is the same one by key or by file name, so an entry
/// that only got identified (bare to CurseForge) isn't "added".
pub fn diff(old: &[Addon], new: &[Addon]) -> Vec<(Did, Addon)> {
    let by_key: HashMap<String, &Addon> = old.iter().map(|a| (crate::groupsync::addon_key(a), a)).collect();
    let by_file: HashMap<String, &Addon> = old.iter().map(|a| (base(&a.file_name), a)).collect();
    let mut out = Vec::new();
    for n in new {
        let o = by_key.get(&crate::groupsync::addon_key(n)).or_else(|| by_file.get(&base(&n.file_name)));
        match o {
            None => out.push((Did::Added, n.clone())),
            Some(o) if o.enabled != n.enabled => out.push((if n.enabled { Did::On } else { Did::Off }, n.clone())),
            Some(_) => {}
        }
    }
    out
}

/// Notes in History what saving `new` over `old` (as on disk) adds or
/// switches. Called by [`Instance::save`]. A brand-new instance (`old`
/// None) notes nothing: its first mods aren't changes.
pub(crate) fn note_save(old: Option<&Instance>, new: &Instance) {
    if !recording() {
        return;
    }
    let Some(old) = old else { return };
    let changes = diff(&old.addons, &new.addons);
    if changes.is_empty() {
        return;
    }
    let recs: Vec<Record> = changes
        .into_iter()
        .filter_map(|(did, a)| {
            let t = tag_for_addon(&new.root, &a)?;
            Some(Record { did, key: crate::groupsync::addon_key(&a), addon: a, who: t.who, batch: t.batch })
        })
        .collect();
    if recs.is_empty() {
        return;
    }
    let dir = new.root.join(crate::sync::BACKUP_DIR).join(crate::time::now().to_string());
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let p = dir.join(RECORDS_FILE);
    let mut all = read_records(&dir);
    all.extend(recs);
    let _ = fs::write(&p, serde_json::to_vec_pretty(&all).unwrap_or_default());
    crate::retention::schedule(&new.root);
}

/// A History folder's record-only entries.
pub fn read_records(dir: &Path) -> Vec<Record> {
    fs::read(dir.join(RECORDS_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Drops one record (undone): `did` of the mod `key`. The folder goes
/// when nothing else is in it.
pub fn forget_record(dir: &Path, key: &str, did: Did) {
    let mut all = read_records(dir);
    let Some(i) = all.iter().position(|r| r.key == key && r.did == did) else { return };
    all.remove(i);
    if all.is_empty() {
        let _ = fs::remove_file(dir.join(RECORDS_FILE));
        crate::backups::drop_if_empty(dir);
    } else {
        let _ = fs::write(dir.join(RECORDS_FILE), serde_json::to_vec_pretty(&all).unwrap_or_default());
    }
}

/// Notes who set `saved` aside (a file in the History folder `dir` of the
/// instance at `root`). With no scope up it notes "You": folders from
/// before this have no note at all and read as before, with no name.
pub(crate) fn note_set_aside(root: &Path, dir: &Path, saved: &Path, original: &str) {
    let name = original.rsplit(['/', '\\']).next().unwrap_or(original);
    let tag = tag_for_file(root, name);
    let p = dir.join(WHO_FILE);
    let mut map: BTreeMap<String, Tag> = fs::read(&p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let key = saved.file_name().unwrap_or_default().to_string_lossy().into_owned();
    map.insert(key, tag);
    let _ = fs::write(&p, serde_json::to_vec_pretty(&map).unwrap_or_default());
}

/// A History folder's who-notes, by saved file name.
pub fn read_tags(dir: &Path) -> BTreeMap<String, Tag> {
    fs::read(dir.join(WHO_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// The root of the instance a History folder (`<root>/.hopper-backups/<time>`) belongs to.
pub(crate) fn root_of(dir: &Path) -> Option<PathBuf> {
    let mut p = dir;
    while let Some(parent) = p.parent() {
        if p.file_name().is_some_and(|n| n == crate::sync::BACKUP_DIR) {
            return Some(parent.to_path_buf());
        }
        p = parent;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addon(file: &str, project: u64, on: bool) -> Addon {
        Addon { file_name: file.into(), project_id: project, enabled: on, name: file.into(), ..Default::default() }
    }

    #[test]
    fn diff_finds_adds_and_switches_only() {
        let old = vec![addon("a.jar", 1, true), addon("b.jar", 2, true), addon("c.jar", 0, true), addon("gone.jar", 4, true)];
        let new = vec![
            addon("a-2.jar", 1, true),  // updated: set aside, not a record
            addon("b.jar", 2, false),   // turned off
            addon("c.jar", 33, true),   // identified since: the same mod
            addon("d.jar", 5, true),    // added
        ];
        let got: Vec<(Did, String)> = diff(&old, &new).into_iter().map(|(d, a)| (d, a.file_name)).collect();
        assert_eq!(got, vec![(Did::Off, "b.jar".into()), (Did::Added, "d.jar".into())]);
    }

    #[test]
    fn a_sync_owns_only_what_it_claims() {
        let root = Path::new("C:/x/Pack");
        let mia = Who::friend("Mia", "mb1", "g1", "");
        let _g = begin(root, None);
        claim("p:1", Some("sodium.jar"), &mia);
        let t = tag_for_addon(root, &addon("sodium.jar", 1, false)).unwrap();
        assert_eq!(t.who, mia);
        assert!(!t.batch.is_empty());
        // Changed in the same moment but not by the sync: yours.
        let t = tag_for_addon(root, &addon("jei.jar", 2, false)).unwrap();
        assert!(t.who.is_you() && t.batch.is_empty());
        // Another instance: not this scope's.
        assert_eq!(tag_for_addon(Path::new("C:/x/Other"), &addon("sodium.jar", 1, false)).unwrap(), Tag::default());
        assert_eq!(tag_for_file(root, "Sodium.jar.disabled").who, mia);
    }

    #[test]
    fn a_bulk_action_is_one_batch_and_undo_is_quiet() {
        let root = Path::new("C:/x/Pack");
        let a = {
            let _g = batch(root, Who::you());
            let a = tag_for_addon(root, &addon("a.jar", 1, true)).unwrap();
            let b = tag_for_addon(root, &addon("b.jar", 2, true)).unwrap();
            assert_eq!(a.batch, b.batch);
            a
        };
        assert!(!a.batch.is_empty());
        // Scope over: the next change is a row of its own.
        assert_eq!(tag_for_addon(root, &addon("a.jar", 1, true)).unwrap().batch, "");
        let _q = quiet(root);
        assert_eq!(tag_for_addon(root, &addon("a.jar", 1, true)), None);
    }

    #[test]
    fn old_tags_and_records_without_who_still_read() {
        let r: Record = serde_json::from_str(r#"{"did":"Added","key":"p:1","addon":{"file_name":"a.jar"}}"#).unwrap();
        assert!(r.who.is_you() && r.batch.is_empty());
        let t: Tag = serde_json::from_str("{}").unwrap();
        assert_eq!(t, Tag::default());
    }

    /// An instance with a.jar (project 5) and b.jar (project 6), both on.
    fn instance(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("hopper-actor-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("mods")).unwrap();
        fs::write(root.join("mods/a.jar"), "a").unwrap();
        fs::write(root.join("mods/b.jar"), "b").unwrap();
        let v = serde_json::json!({"name": "Test", "format": crate::instances::FORMAT, "addons": [
            {"project_id": 5, "file_name": "a.jar", "name": "A", "enabled": true},
            {"project_id": 6, "file_name": "b.jar", "name": "B", "enabled": true}
        ]});
        fs::write(root.join(crate::instances::HOPPER_FILE), serde_json::to_vec(&v).unwrap()).unwrap();
        root
    }

    fn entries(root: &Path) -> Vec<crate::backups::Saved> {
        crate::backups::history(root).into_iter().flat_map(|s| s.files).collect()
    }

    #[test]
    fn your_add_and_switch_show_and_undo_reverts_them() {
        record_in_tests();
        let root = instance("you");
        let mut inst = crate::instances::load(&root).unwrap();
        fs::write(root.join("mods/c.jar"), "c").unwrap();
        inst.addons.push(Addon { project_id: 7, file_name: "c.jar".into(), name: "C".into(), enabled: true, ..Default::default() });
        inst.save().unwrap();
        crate::install::set_enabled(&mut inst, 0, false).unwrap();
        let got = entries(&root);
        let added = got.iter().find(|f| f.change == crate::backups::Change::Added).expect("an add entry");
        assert_eq!((added.title().as_str(), added.who.as_ref().map(Who::label)), ("C", Some("You".to_string())));
        let off = got.iter().find(|f| f.change == crate::backups::Change::TurnedOff).expect("an off entry");
        assert_eq!(off.title(), "A");
        // No copy of anything was kept for these.
        assert!(got.iter().all(|f| f.size == 0));
        // Undo turns it back on, drops the entry and notes nothing new.
        crate::undo::undo(&mut inst, off).unwrap();
        assert!(crate::instances::load(&root).unwrap().addons[0].enabled);
        let got = entries(&root);
        assert!(!got.iter().any(|f| f.change == crate::backups::Change::TurnedOff || f.change == crate::backups::Change::TurnedOn));
        // Undo of an add removes the mod (to History, as any removal).
        let added = got.iter().find(|f| f.change == crate::backups::Change::Added).unwrap().clone();
        crate::undo::undo(&mut inst, &added).unwrap();
        let inst = crate::instances::load(&root).unwrap();
        assert!(!inst.addons.iter().any(|a| a.file_name == "c.jar") && !root.join("mods/c.jar").exists());
        let got = entries(&root);
        assert!(!got.iter().any(|f| f.change == crate::backups::Change::Added));
        let gone = got.iter().find(|f| f.name == "c.jar").expect("the removal");
        assert_eq!(gone.change, crate::backups::Change::Removed);
        assert!(gone.who.as_ref().is_some_and(Who::is_you));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_sync_names_only_the_mods_it_changed() {
        record_in_tests();
        let root = instance("sync");
        let mia = Who::friend("Mia", "mb1", "g1", "");
        let mut inst = crate::instances::load(&root).unwrap();
        {
            let _s = begin(&root, None);
            claim("p:5", Some("a.jar"), &mia);
            // The sync turns a off; b is turned off in the same moment by you
            // (as a save on its thread would carry).
            inst.addons[0].enabled = false;
            inst.addons[1].enabled = false;
            inst.save().unwrap();
            // And sets a aside, which shows in place of its switch.
            crate::sync::backup_into(&root, &root.join("mods/a.jar")).unwrap();
        }
        let sets = crate::backups::history(&root);
        let a = sets.iter().flat_map(|s| &s.files).find(|f| f.name == "a.jar").unwrap();
        assert!(a.record.is_none() && !a.batch.is_empty());
        assert_eq!(a.who.as_ref(), Some(&mia));
        let b_off = sets.iter().flat_map(|s| &s.files).find(|f| f.name == "b.jar").unwrap();
        assert_eq!(b_off.change, crate::backups::Change::TurnedOff);
        assert!(b_off.who.as_ref().is_some_and(Who::is_you) && b_off.batch.is_empty());
        // Mia's changes are one set, yours another.
        let mias = sets.iter().find(|s| s.files.iter().any(|f| f.who.as_ref() == Some(&mia))).unwrap();
        assert!(mias.files.iter().all(|f| f.who.as_ref() == Some(&mia)));
        assert_eq!(mias.who(), Some(&mia));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn one_batch_over_several_seconds_is_one_set() {
        let root = instance("batch");
        let robin = Who::friend("Robin", "mb2", "", "All the Mods 10");
        let rec = |file: &str, did: Did| Record {
            did,
            key: format!("n:{file}"),
            addon: Addon { file_name: file.into(), name: file.into(), project_id: 1, ..Default::default() },
            who: robin.clone(),
            batch: "b1".into(),
        };
        for (when, r) in [(1000, rec("x.jar", Did::Added)), (1004, rec("y.jar", Did::Off)), (1009, rec("z.jar", Did::Off))] {
            let d = root.join(crate::sync::BACKUP_DIR).join(when.to_string());
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join(RECORDS_FILE), serde_json::to_vec(&vec![r]).unwrap()).unwrap();
        }
        let sets = crate::backups::history(&root);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].when, 1009);
        assert_eq!(sets[0].summary(), "1 mod added, 2 turned off");
        assert_eq!(sets[0].headline().as_deref(), Some("Updated from Robin's All the Mods 10"));
        // The badge counts them as entries.
        assert_eq!(crate::backups::count_since(&root, 0).0, 3);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn entries_from_before_who_was_noted_still_load() {
        let root = instance("old");
        let d = root.join(crate::sync::BACKUP_DIR).join("1000");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("common.toml"), "x").unwrap();
        fs::write(d.join(crate::backups::ORIGINS), r#"{"common.toml":"config/common.toml"}"#).unwrap();
        let sets = crate::backups::history(&root);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].files[0].who, None);
        assert_eq!(sets[0].headline(), None);
        // One set aside now, with no sync running, is yours.
        crate::sync::backup_into(&root, &root.join("mods/b.jar")).unwrap();
        let b = entries(&root).into_iter().find(|f| f.name == "b.jar").unwrap();
        assert!(b.who.is_some_and(|w| w.is_you()));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn root_of_a_history_folder() {
        let d = Path::new("C:/i/Pack").join(crate::sync::BACKUP_DIR).join("123").join("pack-update").join("config");
        assert_eq!(root_of(&d), Some(Path::new("C:/i/Pack").to_path_buf()));
    }
}
