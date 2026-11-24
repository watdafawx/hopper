//! What waits for the player before it lands on this PC (design §4). Pure.

use super::model::{Arrival, Change, MemberId};
use super::register::Register;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// When to ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// Relay seconds between a member's last contact and their change
    /// arriving that count as "was away".
    pub away_secs: u64,
    /// More removals or turn-offs than this in one batch ask first.
    pub many_removals: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits { away_secs: 24 * 3600, many_removals: 5 }
    }
}

/// How this PC takes one member's changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Trust {
    #[default]
    Auto,
    AskFirst,
    Ignore,
}

/// Why a record waits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Risk {
    /// A removal or turn-off made at the same time as someone's update.
    Clash,
    /// The author was away this long (relay seconds).
    LongAway { secs: u64 },
    ManyRemovals { count: usize },
    /// The pack moved to an older version.
    PackBack,
    /// The pack moved to another project.
    OtherPack,
    /// A file from GitHub or a link this PC hasn't allowed.
    NewSource,
    /// The member is set to "Ask me first".
    AskFirst,
}

/// What the gate knows besides the record.
pub struct GateCtx<'a> {
    pub limits: Limits,
    pub trust: &'a BTreeMap<MemberId, Trust>,
    pub registers: &'a BTreeMap<super::model::Key, Register>,
    /// The other records of the same batch.
    pub batch: &'a [Change],
}

/// The risks of one incoming record; empty: it lands by itself.
pub fn risks(change: &Change, arrival: &Arrival, ctx: &GateCtx) -> Vec<Risk> {
    let _ = (change, arrival, ctx);
    todo!("P2: gate")
}
