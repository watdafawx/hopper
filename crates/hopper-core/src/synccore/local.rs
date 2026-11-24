//! This PC's side: records only from what Hopper did, and disk differences
//! shown as drift, never sent (design §5).

use super::model::{Edit, Key, Value};
use std::collections::BTreeMap;
use std::path::Path;

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

/// The disk against what was applied. Pure.
pub fn drift(applied: &BTreeMap<Key, Value>, disk: &Snapshot) -> Vec<Drift> {
    let _ = (applied, disk);
    todo!("P5: drift")
}

/// The hook History's `actor` scope calls when Hopper changes a mod of a
/// group copy: records it as this player's change.
pub fn note(root: &Path, edit: Edit) {
    let _ = (root, edit);
    todo!("P5: actor hook")
}
