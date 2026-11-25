//! A compact digest of what a PC has applied, mod by mod, so friends can tell
//! when their copies differ in a way no record explains.
//!
//! The cure for a silent divergence: a PC that starts from its own disk with
//! an empty log (the first start after an upgrade) records and posts nothing,
//! so two PCs whose mods already differ stay different and both read "In
//! sync". Every packet carries a hash of this digest; the whole list goes
//! only to a friend who asked for it (the hash-mismatch pattern of the
//! checkpoint, design §10). Pure.

use super::local;
use super::model::{Key, Value};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A mod's state on one PC: its id, its file ("" when it has none, or when
/// the mod is a pack's own), and the code below. A mod that is gone is not
/// listed at all. Serialises as a three item array, so a 1,172-mod pack is
/// about 14 KB once deflated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateEntry(pub String, pub String, pub u8);

/// A file, turned on.
pub const ON: u8 = 1;
/// A file, turned off.
pub const OFF: u8 = 2;
/// A pack's own mod, as the pack ships it.
pub const PACK_ON: u8 = 3;
/// A pack's own mod, turned off.
pub const PACK_OFF: u8 = 4;
/// A mod this PC keeps only here ("Only on this PC"): never compared.
pub const KEPT: u8 = 5;

/// The mods of `applied`, in key order. Loader, pack, game and locks are
/// left out: those have machinery of their own (a pack that differs is the
/// pack plan's business, a loader of another kind is never followed).
pub fn entries(applied: &BTreeMap<Key, Value>, kept: &BTreeSet<Key>) -> Vec<StateEntry> {
    let mut out = Vec::new();
    let mut seen_kept = BTreeSet::new();
    for (key, value) in applied {
        let Key::Mod(id) = key else { continue };
        // A jar from a link ("src:...") has no project to download by id, so a
        // friend could not adopt it: it is never compared.
        if id.starts_with("src:") {
            continue;
        }
        if kept.contains(key) {
            seen_kept.insert(key);
            out.push(StateEntry(id.clone(), String::new(), KEPT));
            continue;
        }
        let entry = match value {
            Value::File { file, on: true } => StateEntry(id.clone(), file.clone(), ON),
            Value::File { file, on: false } => StateEntry(id.clone(), file.clone(), OFF),
            Value::PackDefault => StateEntry(id.clone(), String::new(), PACK_ON),
            Value::PackOff => StateEntry(id.clone(), String::new(), PACK_OFF),
            Value::Gone | Value::PackRemoved | Value::Text(_) => continue,
        };
        out.push(entry);
    }
    // A kept mod this PC has no applied value for is still not compared.
    for key in kept {
        if let Key::Mod(id) = key
            && !id.starts_with("src:")
            && !seen_kept.contains(key)
        {
            out.push(StateEntry(id.clone(), String::new(), KEPT));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// FNV-1a over a fixed byte form, the same on every PC and build. Never
/// empty: an empty hash means "a Hopper that doesn't send a digest".
pub fn hash(entries: &[StateEntry]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut eat = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x100000001b3);
        }
    };
    for e in entries {
        eat(&(e.0.len() as u64).to_le_bytes());
        eat(e.0.as_bytes());
        eat(&(e.1.len() as u64).to_le_bytes());
        eat(e.1.as_bytes());
        eat(&[e.2]);
    }
    format!("{h:016x}")
}

/// The value an entry stands for. `None` for a kept mod.
pub fn value(e: &StateEntry) -> Option<Value> {
    Some(match e.2 {
        ON => Value::File { file: e.1.clone(), on: true },
        OFF => Value::File { file: e.1.clone(), on: false },
        PACK_ON => Value::PackDefault,
        PACK_OFF => Value::PackOff,
        _ => return None,
    })
}

/// One mod two PCs have differently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub key: Key,
    /// What this PC has applied.
    pub mine: Value,
    /// What the friend has applied.
    pub theirs: Value,
}

/// The mods this PC and a friend have differently, in key order, skipping
/// what a record explains (`explained`: every key the log has a record of,
/// since a settled PC has applied the group's value for it) and what either
/// side keeps only on its own PC. A pack's own mod and the file it is
/// count as the same ([`local::same`]).
pub fn rows(mine: &[StateEntry], theirs: &[StateEntry], explained: &BTreeSet<Key>) -> Vec<Row> {
    let index = |list: &[StateEntry]| -> BTreeMap<String, Option<Value>> { list.iter().map(|e| (e.0.clone(), value(e))).collect() };
    let (a, b) = (index(mine), index(theirs));
    let ids: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    let mut out = Vec::new();
    for id in ids {
        let key = Key::Mod(id.clone());
        if explained.contains(&key) {
            continue;
        }
        // `Some(None)` is a kept mod: not compared.
        let (m, t) = match (a.get(id), b.get(id)) {
            (Some(None), _) | (_, Some(None)) => continue,
            (m, t) => (m.cloned().flatten().unwrap_or_default(), t.cloned().flatten().unwrap_or_default()),
        };
        if !local::same(&m, &t) {
            out.push(Row { key, mine: m, theirs: t });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(n: u64) -> Key {
        Key::Mod(format!("cf:{n}"))
    }

    fn file(n: u64, on: bool) -> Value {
        Value::File { file: format!("cf:{n}"), on }
    }

    fn applied(items: &[(u64, Value)]) -> BTreeMap<Key, Value> {
        items.iter().map(|(n, v)| (k(*n), v.clone())).collect()
    }

    #[test]
    fn gone_mods_and_other_keys_are_not_listed() {
        let mut a = applied(&[(1, file(11, true)), (2, Value::Gone), (3, Value::PackRemoved)]);
        a.insert(Key::Pack, Value::Text("cf:9/1".into()));
        a.insert(Key::Loader, Value::Text("neoforge 21".into()));
        let e = entries(&a, &BTreeSet::new());
        assert_eq!(e, vec![StateEntry("cf:1".into(), "cf:11".into(), ON)]);
    }

    #[test]
    fn a_jar_from_a_link_is_never_listed() {
        let mut a = applied(&[(1, file(11, true))]);
        a.insert(Key::Mod("src:cool.jar".into()), Value::File { file: "cool.jar".into(), on: true });
        let e = entries(&a, &BTreeSet::from([Key::Mod("src:cool.jar".into())]));
        assert_eq!(e.len(), 1);
    }

    #[test]
    fn the_hash_tells_a_turned_off_mod_from_a_turned_on_one() {
        let on = entries(&applied(&[(1, file(11, true))]), &BTreeSet::new());
        let off = entries(&applied(&[(1, file(11, false))]), &BTreeSet::new());
        assert_ne!(hash(&on), hash(&off));
        assert_eq!(hash(&on), hash(&on.clone()));
        assert_eq!(hash(&[]).len(), 16);
    }

    #[test]
    fn rows_list_what_differs_and_nothing_else() {
        let mine = entries(&applied(&[(1, file(11, true)), (2, file(21, true)), (3, file(31, true))]), &BTreeSet::new());
        let theirs = entries(&applied(&[(1, file(11, false)), (2, file(21, true)), (4, file(41, true))]), &BTreeSet::new());
        let r = rows(&mine, &theirs, &BTreeSet::new());
        assert_eq!(r.len(), 3);
        assert_eq!((r[0].key.clone(), r[0].mine.clone(), r[0].theirs.clone()), (k(1), file(11, true), file(11, false)));
        // They have mod 4 and not 3; this PC has 3 and not 4.
        assert_eq!((r[1].mine.clone(), r[1].theirs.clone()), (file(31, true), Value::Gone));
        assert_eq!((r[2].mine.clone(), r[2].theirs.clone()), (Value::Gone, file(41, true)));
    }

    #[test]
    fn another_file_of_the_same_mod_differs() {
        let mine = entries(&applied(&[(1, file(11, true))]), &BTreeSet::new());
        let theirs = entries(&applied(&[(1, file(12, true))]), &BTreeSet::new());
        assert_eq!(rows(&mine, &theirs, &BTreeSet::new()).len(), 1);
    }

    #[test]
    fn a_pack_mod_and_the_file_it_is_are_the_same() {
        let mine = entries(&applied(&[(1, Value::PackDefault), (2, Value::PackOff)]), &BTreeSet::new());
        let theirs = entries(&applied(&[(1, file(11, true)), (2, file(21, false))]), &BTreeSet::new());
        assert!(rows(&mine, &theirs, &BTreeSet::new()).is_empty());
    }

    #[test]
    fn a_mod_kept_only_here_and_a_mod_a_record_explains_are_skipped() {
        let kept = BTreeSet::from([k(1)]);
        let mine = entries(&applied(&[(1, file(11, true)), (2, file(21, true))]), &kept);
        let theirs = entries(&applied(&[(1, Value::Gone), (2, file(21, false))]), &BTreeSet::new());
        let explained = BTreeSet::from([k(2)]);
        assert!(rows(&mine, &theirs, &explained).is_empty());
        // Without the record, mod 2 differs; mod 1 stays skipped.
        let r = rows(&mine, &theirs, &BTreeSet::new());
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].key, k(2));
        // Kept on the other side too.
        let theirs_kept = entries(&applied(&[(1, file(12, true))]), &BTreeSet::from([k(1)]));
        assert!(rows(&entries(&applied(&[(1, Value::Gone)]), &BTreeSet::new()), &theirs_kept, &BTreeSet::new()).is_empty());
    }
}
