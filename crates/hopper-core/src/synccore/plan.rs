//! What to do on disk, and what a pack update does for each friend, before
//! anything runs (design §6, §7). Pure.

use super::model::{ConfigMode, Dot, Key, MemberId, Value};

/// One file change to make, and whose change it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub key: Key,
    pub from: Value,
    pub to: Value,
    pub by: Dot,
    pub name: String,
}

/// Why a key isn't changed yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wait {
    GameRunning,
    Review(u64),
    Source,
    /// The author blocked downloads: by hand.
    ByHand,
    /// Tried and failed; again after this many seconds.
    Retry { after_secs: u64 },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApplyPlan {
    pub steps: Vec<Step>,
    pub waits: Vec<(Key, Wait)>,
}

/// What a pack update does on one friend's PC, from their last status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FriendPlan {
    pub member: MemberId,
    pub name: String,
    pub configs: ConfigMode,
    /// Their changed config files that stay theirs.
    pub kept_files: usize,
    /// Pack mods they turned off that stay off.
    pub stays_off: Vec<Key>,
    pub waits_for_game: bool,
    /// The line the dialog shows ("Mia: keeps her 3 changed settings files.").
    pub says: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackPlan {
    /// The line for this PC.
    pub here: String,
    pub friends: Vec<FriendPlan>,
}
