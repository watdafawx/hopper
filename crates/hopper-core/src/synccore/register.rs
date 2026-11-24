//! The merge rule (design §3): every record of a copy, folded per key into
//! what the group agrees. Pure; order-free and idempotent.

use super::model::{Arrival, Change, Dot, Key};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Every record of one copy this PC has seen, with how the relay numbered
/// the event that brought it (None: made here, not sent yet).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Log {
    pub changes: BTreeMap<Dot, (Change, Option<Arrival>)>,
}

/// One key's state in the group: the winning record, and records made at
/// the same time that aren't settled yet (a clash waiting for an answer).
#[derive(Clone, Debug, PartialEq)]
pub struct Register {
    pub winner: Change,
    pub open: Vec<Change>,
}

/// How an incoming record relates to a key's current record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Merge {
    /// It had seen the current one: it replaces it.
    Follows,
    /// The current one had seen it: History only.
    Stale,
    /// Both say the same.
    Same,
    /// Made at the same time, both keep the mod: the higher
    /// (lamport, author) wins, the other author is told.
    Newest,
    /// Made at the same time, one takes the mod away: keep it and ask.
    Clash,
}

pub fn merge(current: &Change, incoming: &Change) -> Merge {
    let _ = (current, incoming);
    todo!("P1: merge rule")
}

/// The register of every key, from the whole log.
pub fn fold(log: &Log) -> BTreeMap<Key, Register> {
    let _ = log;
    todo!("P1: fold")
}

/// A hash of the winning dot per key, to tell two PCs agree (design §10).
pub fn hash(registers: &BTreeMap<Key, Register>) -> String {
    let _ = registers;
    todo!("P1: register hash")
}
