//! The one object the apps use for a group copy (design §11). Pure: every
//! method is a function of the engine's state and its arguments.

use super::apply::ApplyReport;
use super::gate::{Limits, Risk, Trust};
use super::local::{Drift, Snapshot};
use super::model::{Arrival, Change, Dot, Edit, Key, MemberId, PackMode, Value};
use super::plan::{ApplyPlan, PackPlan};
use super::register::Log;
use super::wire::{MemberStatus, Packet};
use std::collections::BTreeMap;

/// A review card: records that wait for the player.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Review {
    pub id: u64,
    pub risk: Risk,
    pub changes: Vec<Dot>,
    pub keys: Vec<Key>,
    /// Who made them.
    pub who: String,
    /// The card's sentence ("Mia removed Sodium while she was away.").
    pub says: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Let the friend's change land.
    Take,
    /// Keep what is here (writes a follow-up record).
    Keep,
}

/// The status chip, from the disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    InSync,
    /// This PC's changes not acked by everyone yet.
    Sending(usize),
    NeedsYou(usize),
    /// The mods folder differs from what was applied ("This PC differs").
    Differs(usize),
    /// Friends' changes not applied yet (downloads, game running).
    Behind(usize),
    /// A member's Hopper sends another format.
    OutOfDate(String),
}

/// What a received packet did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Received {
    pub new: usize,
    pub reviews: Vec<u64>,
    /// The packet was refused (another wire version).
    pub refused: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Engine {
    pub copy: String,
    pub me: MemberId,
    pub name: String,
    pub log: Log,
    pub applied: BTreeMap<Key, Value>,
    pub trust: BTreeMap<MemberId, Trust>,
    pub limits: Limits,
    pub members: BTreeMap<MemberId, MemberStatus>,
}

impl Engine {
    pub fn new(copy: &str, me: &str, name: &str) -> Engine {
        let _ = (copy, me, name);
        todo!("P3: new")
    }
    pub fn record(&mut self, edit: Edit, online_at: u64) -> Dot {
        let _ = (edit, online_at);
        todo!("P3: record")
    }
    pub fn drift(&self, disk: &Snapshot) -> Vec<Drift> {
        let _ = disk;
        todo!("P5: drift")
    }
    pub fn outbox(&self) -> Option<Packet> {
        todo!("P4: outbox")
    }
    pub fn sent(&mut self, packet_hash: &str, arrival: Arrival) {
        let _ = (packet_hash, arrival);
        todo!("P4: sent")
    }
    pub fn receive(&mut self, packet: Packet, arrival: Arrival) -> Received {
        let _ = (packet, arrival);
        todo!("P3: receive")
    }
    pub fn reviews(&self) -> Vec<Review> {
        todo!("P3: reviews")
    }
    pub fn answer(&mut self, review: u64, answer: Answer, all: bool) -> Vec<Dot> {
        let _ = (review, answer, all);
        todo!("P3: answer")
    }
    pub fn set_trust(&mut self, member: &str, trust: Trust) {
        let _ = (member, trust);
        todo!("P3: trust")
    }
    pub fn plan(&self) -> ApplyPlan {
        todo!("P6: plan")
    }
    pub fn applied(&mut self, report: &ApplyReport) {
        let _ = report;
        todo!("P6: applied")
    }
    pub fn pack_plan(&self, version: &str, mode: PackMode) -> PackPlan {
        let _ = (version, mode);
        todo!("P6: pack plan")
    }
    pub fn undo_member(&self, member: &str, since: u64) -> Vec<Edit> {
        let _ = (member, since);
        todo!("P3: undo a member")
    }
    pub fn value(&self, key: &Key) -> Value {
        let _ = key;
        todo!("P3: value")
    }
    pub fn last_change(&self, key: &Key) -> Option<&Change> {
        let _ = key;
        todo!("P3: last change")
    }
    pub fn status(&self, disk: &Snapshot) -> Status {
        let _ = disk;
        todo!("P5: status")
    }
    pub fn members(&self) -> Vec<MemberStatus> {
        todo!("P3: members")
    }
}
