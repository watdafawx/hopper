//! What travels through the relay (design §10). Pure.
//!
//! One event per sender and group, cumulative (every record some member
//! hasn't acked), so the relay keeping only the newest event loses nothing.

use super::model::{Change, Key, MemberId, Seen};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Packets of any other version are refused ("Mia's Hopper is out of date").
pub const WIRE_VERSION: u8 = 1;

/// A packet over this many bytes (after deflate + base64url) goes in parts.
pub const MAX_PART: usize = 60 * 1024;

/// A member's state, sent with every packet: acks, and what the pack plan
/// and the status need to know about their PC.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberStatus {
    pub member: MemberId,
    pub name: String,
    /// What they have taken in (acks).
    pub seen: Seen,
    /// What they have applied on disk.
    pub applied: Seen,
    pub pack: String,
    pub edited_configs: Vec<String>,
    pub off_pack_mods: Vec<Key>,
    /// Keys the group has that their disk lacks.
    pub missing: Vec<Key>,
    pub game_running: bool,
    /// Their register hash differed: send them a checkpoint.
    pub needs_full: bool,
}

/// One copy's part of a packet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Body {
    /// The records some member hasn't acked yet.
    Tail(Vec<Change>),
    /// The winning record (and open clash records) of every key.
    Checkpoint(Vec<Change>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Packet {
    pub version: u8,
    pub from: MemberId,
    pub group: String,
    /// By copy id.
    pub copies: BTreeMap<String, (Body, MemberStatus, String)>,
}

/// The relay bodies for a packet: one, or parts under [`MAX_PART`].
pub fn encode(p: &Packet) -> Vec<String> {
    let _ = p;
    todo!("P4: encode")
}

/// A packet from its parts; an error for another version or a part missing.
pub fn decode(parts: &[String]) -> anyhow::Result<Packet> {
    let _ = parts;
    todo!("P4: decode")
}
