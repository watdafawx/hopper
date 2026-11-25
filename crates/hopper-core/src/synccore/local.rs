//! This PC's side: records only from what Hopper did, and disk differences
//! shown as drift, never sent (design §5).
//!
//! Two halves, kept apart:
//!
//! - Records: [`note_save`] is the one hook `actor` calls when an instance is
//!   saved. It turns what the player did into [`Edit`]s and queues them for
//!   the instance's root when that root is a watched group copy; the app
//!   drains them with [`take`] into `Engine::record`. A change made by a
//!   friend's sync (another `Who`) or by Undo never becomes an edit here.
//! - Drift: [`drift`] compares a scan with what was applied. The disk is
//!   never diffed to make records.

use super::model::{Edit, Key, Value, Why};
use crate::instances::Addon;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

/// A scan of an instance's mods folder (and the other addon folders), by key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub values: BTreeMap<Key, Value>,
    /// The instance couldn't be read: nothing is drift, nothing is sent.
    pub unreadable: bool,
}

/// One key where the disk differs from what this PC last applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drift {
    pub key: Key,
    pub expected: Value,
    pub found: Value,
}

/// Whether a scanned value is the one that was applied. A pack mod is
/// scanned as the file it is, so the pack's own states match their files.
pub(crate) fn same(expected: &Value, found: &Value) -> bool {
    match (expected, found) {
        (a, b) if a == b => true,
        (Value::PackDefault, Value::File { on: true, .. }) => true,
        (Value::PackOff, Value::File { on: false, .. }) => true,
        (Value::PackRemoved, Value::Gone) => true,
        (Value::File { on: true, .. }, Value::PackDefault) => true,
        (Value::File { on: false, .. }, Value::PackOff) => true,
        (Value::Gone, Value::PackRemoved) => true,
        _ => false,
    }
}

/// The disk against what was applied. Pure.
///
/// A mod missing from the scan is gone; a mod in the scan that was never
/// applied is a jar nobody here asked for. The other keys (loader, game,
/// pack, locks) are only compared when the scan reads them. An unreadable
/// instance has no drift: it tells nothing about the folder.
pub fn drift(applied: &BTreeMap<Key, Value>, disk: &Snapshot) -> Vec<Drift> {
    if disk.unreadable {
        return Vec::new();
    }
    let mut keys: Vec<&Key> = applied.keys().chain(disk.values.keys()).collect();
    keys.sort();
    keys.dedup();
    let mut out = Vec::new();
    for key in keys {
        let found = match disk.values.get(key) {
            Some(v) => v.clone(),
            None if matches!(key, Key::Mod(_)) => Value::Gone,
            None => continue,
        };
        let expected = applied.get(key).cloned().unwrap_or_default();
        if !same(&expected, &found) {
            out.push(Drift { key: key.clone(), expected, found });
        }
    }
    out
}

/// "Remove for everyone" and "Add for everyone": the records that share what
/// differs on this PC, one batch. A mod missing from the folder is removed
/// for everyone; a mod only this PC has (added outside Hopper) is added for
/// everyone, as the file it is here. A file that was swapped or turned off
/// outside Hopper has no such button (it can only be put back).
pub fn share_edits(drifts: &[Drift], batch: &str) -> Vec<Edit> {
    drifts
        .iter()
        .filter_map(|d| {
            let now = if d.found == Value::Gone && d.expected != Value::Gone {
                if matches!(d.expected, Value::PackDefault | Value::PackOff) { Value::PackRemoved } else { Value::Gone }
            } else if matches!(d.expected, Value::Gone | Value::PackRemoved) && d.found != Value::Gone {
                d.found.clone()
            } else {
                return None;
            };
            Some(Edit { key: d.key.clone(), now, why: Why::SharedDrift, batch: batch.to_string() })
        })
        .collect()
}

/// "Put them back": what each drifting key should be again, from what was
/// applied (the apply plan restores it; nothing is sent).
pub fn put_back(drifts: &[Drift]) -> Vec<(Key, Value)> {
    drifts.iter().map(|d| (d.key.clone(), d.expected.clone())).collect()
}

// ── records: the actor hook ──────────────────────────────────────────

/// Edits waiting per watched instance root.
static WATCHED: Mutex<BTreeMap<String, Vec<Edit>>> = Mutex::new(BTreeMap::new());

fn norm(root: &Path) -> String {
    root.to_string_lossy().replace('\\', "/").trim_end_matches('/').to_lowercase()
}

fn with_watched<R>(f: impl FnOnce(&mut BTreeMap<String, Vec<Edit>>) -> R) -> R {
    let mut g = WATCHED.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}

/// The app calls this for an instance that is a group copy: from now on
/// what the player does there queues edits.
pub fn watch(root: &Path) {
    with_watched(|w| {
        w.entry(norm(root)).or_default();
    });
}

pub fn unwatch(root: &Path) {
    with_watched(|w| {
        w.remove(&norm(root));
    });
}

/// Queues one change as this player's, for a watched root. Anything else
/// (an instance that isn't a group copy) is ignored.
pub fn note(root: &Path, edit: Edit) {
    with_watched(|w| {
        if let Some(q) = w.get_mut(&norm(root)) {
            q.push(edit);
        }
    });
}

/// Takes the queued edits for the app to hand to `Engine::record`.
pub fn take(root: &Path) -> Vec<Edit> {
    with_watched(|w| w.get_mut(&norm(root)).map(std::mem::take).unwrap_or_default())
}

/// The hook `actor::note_save` calls when an instance at `root` is saved:
/// what the player changed between `old` and `new` becomes edits (a mod
/// added, turned on or off, another file, removed). `owner` says whose an
/// addon's change is: `Some(batch)` for the player's own, `None` for a
/// friend's sync or Undo, which never make records here.
pub fn note_save(root: &Path, old: &[Addon], new: &[Addon], owner: impl Fn(&Addon) -> Option<String>) {
    if !with_watched(|w| w.contains_key(&norm(root))) {
        return;
    }
    for edit in save_edits(old, new, owner) {
        note(root, edit);
    }
}

/// [`note_save`]'s pure part.
pub fn save_edits(old: &[Addon], new: &[Addon], owner: impl Fn(&Addon) -> Option<String>) -> Vec<Edit> {
    let mut before: BTreeMap<Key, &Addon> = BTreeMap::new();
    for a in old {
        before.entry(key_of(a)).or_insert(a);
    }
    let mut after: BTreeMap<Key, &Addon> = BTreeMap::new();
    for a in new {
        after.entry(key_of(a)).or_insert(a);
    }
    let mut out = Vec::new();
    for (key, a) in &after {
        let was = before.get(key);
        // A lock put on or taken off is its own record.
        if let Key::Mod(k) = key {
            let (lock, had) = (lock_of(a), was.map_or(Value::Gone, |o| lock_of(o)));
            if lock != had
                && let Some(batch) = owner(a)
            {
                out.push(Edit { key: Key::Lock(k.clone()), now: lock, why: Why::Player, batch });
            }
        }
        let changed = match was {
            None => true,
            Some(o) => file_of(o) != file_of(a) || o.enabled != a.enabled,
        };
        if !changed {
            continue;
        }
        if let Some(batch) = owner(a) {
            let now = value_of(a);
            out.push(Edit { key: key.clone(), now, why: Why::Player, batch });
        }
    }
    for (key, o) in &before {
        if after.contains_key(key) {
            continue;
        }
        if let Some(batch) = owner(o) {
            let now = if o.from_pack { Value::PackRemoved } else { Value::Gone };
            out.push(Edit { key: key.clone(), now, why: Why::Player, batch });
        }
    }
    out
}

/// A mod's key: the project it came from, with the kind of its folder.
pub fn key_of(a: &Addon) -> Key {
    let id = if let Some(src) = link_source(a) {
        super::link::key_text(src)
    } else if a.project_id != 0 {
        format!("cf:{}", a.project_id)
    } else if !a.mr_project.is_empty() {
        format!("mr:{}", a.mr_project)
    } else {
        format!("src:{}", a.file_name.trim_end_matches(".disabled"))
    };
    Key::Mod(match a.folder() {
        "shaderpacks" => format!("shader:{id}"),
        "resourcepacks" => format!("rp:{id}"),
        _ => id,
    })
}

/// The source of a mod that comes from GitHub or a link (its project id is
/// the source's own, or it has none), so it is named by where it lives.
fn link_source(a: &Addon) -> Option<&crate::sources::ModSource> {
    a.source.as_ref().filter(|_| a.project_id == 0 || crate::sources::is_src(a.project_id))
}

/// The file an addon is on, as the group names files.
pub fn file_of(a: &Addon) -> String {
    if let Some(src) = link_source(a) {
        super::link::LinkRef::of(src, a.file_name.trim_end_matches(".disabled")).id()
    } else if a.project_id != 0 && a.file_id != 0 {
        format!("cf:{}", a.file_id)
    } else if !a.mr_project.is_empty() && !a.mr_version.is_empty() {
        format!("mr:{}", a.mr_version)
    } else {
        a.file_name.trim_end_matches(".disabled").to_string()
    }
}

/// The scan of an instance as the app has it loaded: every addon by key,
/// the loader, the Minecraft version of a plain instance and the pack
/// version. An instance that couldn't be read is `unreadable`. Pure: the
/// app reads the instance, this only turns it into values.
pub fn snapshot(inst: &crate::instances::Instance) -> Snapshot {
    if inst.unreadable.is_some() {
        return Snapshot { values: BTreeMap::new(), unreadable: true };
    }
    let mut values = BTreeMap::new();
    for a in &inst.addons {
        let key = key_of(a);
        if let (Key::Mod(k), l) = (&key, lock_of(a))
            && l != Value::Gone
        {
            values.entry(Key::Lock(k.clone())).or_insert(l);
        }
        values.entry(key).or_insert_with(|| value_of(a));
    }
    if !inst.loader.is_empty() {
        values.insert(Key::Loader, Value::Text(inst.loader.clone()));
    }
    match &inst.pack {
        Some(p) => {
            if let Some(v) = pack_text(p) {
                values.insert(Key::Pack, Value::Text(v));
            }
        }
        None if !inst.game_version.is_empty() => {
            values.insert(Key::Game, Value::Text(inst.game_version.clone()));
        }
        None => {}
    }
    Snapshot { values, unreadable: false }
}

/// A pack version as the group names it: "cf:<project>/<file>" or
/// "mr:<project>/<version>". A pack Hopper holds no ids for has none.
pub fn pack_text(p: &crate::instances::PackRef) -> Option<String> {
    if !p.mr_project.is_empty() && !p.mr_version.is_empty() {
        Some(format!("mr:{}/{}", p.mr_project, p.mr_version))
    } else if p.project_id != 0 && p.file_id != 0 {
        Some(format!("cf:{}/{}", p.project_id, p.file_id))
    } else {
        None
    }
}

/// A mod's lock as the group has it: not locked is `Gone`, never offered
/// as an update "all", one update's file locked out "v:<that file>".
pub fn lock_of(a: &Addon) -> Value {
    if a.locked {
        Value::Text("all".into())
    } else if !a.locked_version.is_empty() {
        Value::Text(format!("v:{}", a.locked_version))
    } else {
        Value::Gone
    }
}

/// Puts a lock on or takes it off (the other half of [`lock_of`]). False
/// when the value isn't a lock.
pub fn set_lock(a: &mut Addon, v: &Value) -> bool {
    match v {
        Value::Gone => {
            a.locked = false;
            a.locked_version.clear();
        }
        Value::Text(t) if t == "all" => {
            a.locked = true;
            a.locked_version.clear();
        }
        Value::Text(t) if t.starts_with("v:") => {
            a.locked = false;
            a.locked_version = t[2..].to_string();
        }
        _ => return false,
    }
    true
}

fn value_of(a: &Addon) -> Value {
    if a.from_pack {
        return if a.enabled { Value::PackDefault } else { Value::PackOff };
    }
    Value::File { file: file_of(a), on: a.enabled }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(n: u64) -> Key {
        Key::Mod(format!("cf:{n}"))
    }

    fn on(n: u64) -> Value {
        Value::File { file: format!("cf:{n}"), on: true }
    }

    fn off(n: u64) -> Value {
        Value::File { file: format!("cf:{n}"), on: false }
    }

    fn applied(items: &[(u64, Value)]) -> BTreeMap<Key, Value> {
        items.iter().map(|(n, v)| (k(*n), v.clone())).collect()
    }

    fn snap(items: &[(u64, Value)]) -> Snapshot {
        Snapshot { values: applied(items), unreadable: false }
    }

    fn addon(project: u64, file: u64, enabled: bool) -> Addon {
        Addon {
            project_id: project,
            file_id: file,
            file_name: format!("mod{project}-{file}.jar"),
            enabled,
            ..Addon::default()
        }
    }

    fn mine(_: &Addon) -> Option<String> {
        Some("b1".into())
    }

    #[test]
    fn a_disk_that_matches_has_no_drift() {
        let a = applied(&[(1, on(11)), (2, off(21))]);
        assert!(drift(&a, &snap(&[(1, on(11)), (2, off(21))])).is_empty());
    }

    #[test]
    fn a_missing_swapped_or_extra_jar_is_drift() {
        let a = applied(&[(1, on(11)), (2, on(21)), (3, on(31))]);
        let d = drift(&a, &snap(&[(1, on(11)), (3, on(32)), (4, on(41))]));
        let keys: Vec<Key> = d.iter().map(|d| d.key.clone()).collect();
        assert_eq!(keys, [k(2), k(3), k(4)]);
        assert_eq!(d[0].found, Value::Gone);
        assert_eq!(d[1].expected, on(31));
        assert_eq!(d[1].found, on(32));
        assert_eq!(d[2].expected, Value::Gone);
    }

    #[test]
    fn a_turned_off_jar_is_drift_too() {
        let a = applied(&[(1, on(11))]);
        assert_eq!(drift(&a, &snap(&[(1, off(11))])).len(), 1);
    }

    #[test]
    fn pack_states_match_their_files() {
        let a = applied(&[(1, Value::PackDefault), (2, Value::PackOff), (3, Value::PackRemoved)]);
        assert!(drift(&a, &snap(&[(1, on(11)), (2, off(21))])).is_empty());
        let d = drift(&a, &snap(&[(2, on(21))]));
        assert_eq!(d.iter().map(|d| d.key.clone()).collect::<Vec<_>>(), [k(1), k(2)]);
    }

    #[test]
    fn an_unreadable_instance_has_no_drift() {
        let a = applied(&[(1, on(11))]);
        let disk = Snapshot { values: BTreeMap::new(), unreadable: true };
        assert!(drift(&a, &disk).is_empty());
    }

    #[test]
    fn other_keys_are_only_compared_when_scanned() {
        let mut a = applied(&[(1, on(11))]);
        a.insert(Key::Loader, Value::Text("neoforge-21.1".into()));
        let mut disk = snap(&[(1, on(11))]);
        assert!(drift(&a, &disk).is_empty());
        disk.values.insert(Key::Loader, Value::Text("neoforge-21.4".into()));
        assert_eq!(drift(&a, &disk).len(), 1);
    }

    #[test]
    fn sharing_drift_removes_only_what_is_missing() {
        let a = applied(&[(1, on(11)), (2, on(21)), (3, Value::PackDefault), (4, on(41))]);
        let d = drift(&a, &snap(&[(1, on(12)), (4, off(41))]));
        let edits = share_edits(&d, "x");
        let got: Vec<(Key, Value)> = edits.iter().map(|e| (e.key.clone(), e.now.clone())).collect();
        assert_eq!(got, [(k(2), Value::Gone), (k(3), Value::PackRemoved)]);
        assert!(edits.iter().all(|e| e.why == Why::SharedDrift && e.batch == "x"));
    }

    #[test]
    fn sharing_drift_adds_a_jar_only_this_pc_has() {
        // Key 5 was never applied (a jar added outside Hopper); key 1 is the
        // same file, so nothing differs there.
        let a = applied(&[(1, on(11)), (5, Value::Gone)]);
        let d = drift(&a, &snap(&[(1, on(11)), (5, on(51))]));
        let edits = share_edits(&d, "x");
        let got: Vec<(Key, Value)> = edits.iter().map(|e| (e.key.clone(), e.now.clone())).collect();
        assert_eq!(got, [(k(5), on(51))]);
        assert!(edits.iter().all(|e| e.why == Why::SharedDrift));
    }

    #[test]
    fn put_back_names_what_was_applied() {
        let a = applied(&[(1, on(11))]);
        let d = drift(&a, &snap(&[]));
        assert_eq!(put_back(&d), [(k(1), on(11))]);
    }

    #[test]
    fn a_save_makes_edits_for_adds_updates_switches_and_removals() {
        let old = [addon(1, 11, true), addon(2, 21, true), addon(3, 31, true), addon(4, 41, true)];
        let new = [addon(1, 12, true), addon(2, 21, false), addon(3, 31, true), addon(5, 51, true)];
        let got: Vec<(Key, Value)> = save_edits(&old, &new, mine).into_iter().map(|e| (e.key, e.now)).collect();
        assert_eq!(
            got,
            [(k(1), on(12)), (k(2), off(21)), (k(5), on(51)), (k(4), Value::Gone)]
        );
    }

    #[test]
    fn a_friends_change_or_undo_makes_no_edit() {
        let old = [addon(1, 11, true)];
        let new = [addon(1, 12, true)];
        assert!(save_edits(&old, &new, |_| None).is_empty());
    }

    #[test]
    fn an_unchanged_save_makes_no_edit() {
        let a = [addon(1, 11, true)];
        assert!(save_edits(&a, &a, mine).is_empty());
    }

    #[test]
    fn pack_mods_are_pack_states() {
        let mut shipped = addon(1, 11, true);
        shipped.from_pack = true;
        let mut off_it = shipped.clone();
        off_it.enabled = false;
        let got = save_edits(&[shipped.clone()], &[off_it.clone()], mine);
        assert_eq!(got[0].now, Value::PackOff);
        let got = save_edits(&[off_it], &[shipped.clone()], mine);
        assert_eq!(got[0].now, Value::PackDefault);
        let got = save_edits(&[shipped], &[], mine);
        assert_eq!(got[0].now, Value::PackRemoved);
    }

    #[test]
    fn addons_outside_mods_carry_their_folder() {
        let mut shader = addon(5, 51, true);
        shader.class_id = crate::cf::CLASS_SHADERS;
        assert_eq!(key_of(&shader), Key::Mod("shader:cf:5".into()));
        let mut link = addon(0, 0, true);
        link.file_name = "thing.jar.disabled".into();
        assert_eq!(key_of(&link), Key::Mod("src:thing.jar".into()));
    }

    fn github_mod(tag: &str) -> Addon {
        let mut a = addon(crate::sources::src_id("github:o/r"), 5, true);
        a.file_name = format!("tweaks-{tag}.jar");
        a.source = Some(crate::sources::ModSource {
            kind: crate::sources::Kind::GitHub,
            url: "o/r".into(),
            seen: tag.into(),
            sha256: "cd".repeat(32),
            ..Default::default()
        });
        a
    }

    #[test]
    fn a_link_mod_is_named_by_its_source_and_its_file_carries_the_address() {
        let (v1, v2) = (github_mod("v1"), github_mod("v2"));
        // The key survives an update; the file id changes with the release.
        assert_eq!(key_of(&v1), Key::Mod("src:github:o/r".into()));
        assert_eq!(key_of(&v1), key_of(&v2));
        let Value::File { file, on } = value_of(&v1) else { panic!("a file") };
        assert!(on);
        let link = crate::synccore::link::LinkRef::parse(&file).expect("a link id");
        assert_eq!(link.download, "https://github.com/o/r/releases/download/v1/tweaks-v1.jar");
        assert_eq!(link.sha256, "cd".repeat(32));
        assert_ne!(file_of(&v1), file_of(&v2));
        // Turned off (".disabled") is the same file.
        let mut off = v1.clone();
        off.file_name.push_str(".disabled");
        assert_eq!(file_of(&off), file_of(&v1));
    }

    #[test]
    fn a_lock_put_on_or_off_is_its_own_record_and_the_scan_sees_it() {
        let (mut locked, plain) = (addon(1, 11, true), addon(1, 11, true));
        locked.locked = true;
        let got = save_edits(&[plain.clone()], &[locked.clone()], mine);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].key, Key::Lock("cf:1".into()));
        assert_eq!(got[0].now, Value::Text("all".into()));
        // The lock coming off is a record too ("Gone" is no lock).
        let off = save_edits(&[locked.clone()], &[plain.clone()], mine);
        assert_eq!((off[0].key.clone(), off[0].now.clone()), (Key::Lock("cf:1".into()), Value::Gone));
        // One update's file kept out is another kind of lock.
        let mut pinned = plain.clone();
        pinned.locked_version = "cf:12".into();
        assert_eq!(lock_of(&pinned), Value::Text("v:cf:12".into()));
        // Applying puts it back the way it was made.
        let mut a = plain;
        assert!(set_lock(&mut a, &lock_of(&pinned)));
        assert_eq!(a.locked_version, "cf:12");
        assert!(!a.locked);
        assert!(set_lock(&mut a, &Value::Text("all".into())) && a.locked && a.locked_version.is_empty());
        assert!(set_lock(&mut a, &Value::Gone) && !a.locked);
        assert!(!set_lock(&mut a, &Value::PackOff));
        // A mod added already locked comes with its lock.
        let added = save_edits(&[], &[locked], mine);
        assert!(added.iter().any(|e| e.key == Key::Lock("cf:1".into())));
    }

    #[test]
    fn only_watched_roots_queue_edits() {
        let root = std::env::temp_dir().join("hopper-local-watch-test-root");
        let other = std::env::temp_dir().join("hopper-local-watch-test-other");
        watch(&root);
        let edit = Edit { key: k(1), now: on(12), why: Why::Player, batch: String::new() };
        note(&root, edit.clone());
        note(&other, edit.clone());
        note_save(&root, &[addon(1, 11, true)], &[addon(1, 13, true)], mine);
        note_save(&other, &[addon(1, 11, true)], &[addon(1, 13, true)], mine);
        let got = take(&root);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], edit);
        assert_eq!(got[1].now, on(13));
        assert!(take(&root).is_empty());
        assert!(take(&other).is_empty());
        unwatch(&root);
        note(&root, edit);
        assert!(take(&root).is_empty());
    }
}
