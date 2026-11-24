//! The change record and the values it talks about (design §2).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A member's mailbox id.
pub type MemberId = String;

/// One change's id: its author and the author's own counter (1, 2, 3...).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Dot {
    pub author: MemberId,
    pub n: u64,
}

/// The highest dot of each member a PC had taken in (a version vector).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen(pub BTreeMap<MemberId, u64>);

impl Seen {
    /// Whether the change `d` had been taken in when this was made.
    pub fn covers(&self, d: &Dot) -> bool {
        self.0.get(&d.author).is_some_and(|n| *n >= d.n)
    }
}

/// What a change is about. Mod keys carry their kind and folder
/// ("cf:3", "mr:AANobbMI", "src:<file>", "shader:cf:5", "rp:mr:x").
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Key {
    Mod(String),
    /// A mod's lock, by the mod's key.
    Lock(String),
    Loader,
    /// The modpack version ("cf:<project>/<file>").
    Pack,
    /// The Minecraft version of a plain instance (no modpack).
    Game,
}

/// What a key is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Value {
    /// Not in the instance (removed, or never added).
    #[default]
    Gone,
    /// A file of the author's, on or off.
    File { file: String, on: bool },
    /// A pack mod as the pack ships it.
    PackDefault,
    PackOff,
    PackRemoved,
    /// A loader, game or pack version, or a lock's pinned file.
    Text(String),
}

impl Value {
    /// Whether going from `was` to this takes a mod away or turns it off
    /// (the destructive side of a clash).
    pub fn takes_away(&self, was: &Value) -> bool {
        let _ = was;
        todo!("P1: removal / turn-off test")
    }
}

/// How config files are handled in a pack update.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfigMode {
    /// Changed config files stay as they are.
    #[default]
    Keep,
    /// Config files go back to the pack's.
    Clean,
}

/// A pack update's stated handling, carried in its record so every PC
/// applies the same thing (design §6).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackMode {
    pub here: ConfigMode,
    pub friends: ConfigMode,
    /// Pack mods a member turned off stay off for them.
    pub keep_off: bool,
}

/// Why a record was made.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Why {
    /// The player did it in Hopper.
    #[default]
    Player,
    /// History undo of another record.
    Undo { of: Dot },
    /// An answer to a review card.
    Answer { review: u64 },
    PackUpdate { mode: PackMode },
    /// "Remove for everyone" on a disk difference.
    SharedDrift,
}

/// One change, as it travels (design §2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub dot: Dot,
    pub key: Key,
    pub was: Value,
    pub now: Value,
    /// The author's shown name at the time.
    pub name: String,
    pub lamport: u64,
    pub seen: Seen,
    /// Relay time of the author's last contact before making it.
    pub online_at: u64,
    pub batch: String,
    pub why: Why,
}

/// How the relay numbered an event: its number and its time (seconds).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Arrival {
    pub seq: u64,
    pub at: u64,
}

/// A change this PC makes, before the engine dates it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit {
    pub key: Key,
    pub now: Value,
    pub why: Why,
    pub batch: String,
}
