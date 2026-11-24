//! An in-memory friends group for the scenario tests
//! (`tests/sync_scenarios/`): one [`Engine`] per person, a fake mods folder
//! each, and a relay that keeps only the newest event per sender and key,
//! with its per-sender queue limit and body cap. Relay time moves only with
//! [`Group::advance`]; nothing sleeps.
//!
//! People are named by a short string ("A", "B"), which is also their
//! member id and shown name. Built in P3 (engine) and P4 (relay limits).

use super::apply::Outcome;
use super::engine::{Answer, Review, Status};
use super::gate::Trust;
use super::local::Drift;
use super::model::{Key, PackMode, Value};
use super::plan::PackPlan;

/// One History row as a person's History would show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryRow {
    pub key: Key,
    /// "Removed by A".
    pub says: String,
    /// Who made it.
    pub by: String,
}

/// A group of people, their PCs and the relay between them.
pub struct Group {
    _todo: (),
}

impl Group {
    // ── setup ────────────────────────────────────────────────────────

    /// `names` in one group, all online, every PC on `mods` and in step.
    pub fn with(names: &[&str], mods: &[(Key, Value)]) -> Group {
        let _ = (names, mods);
        todo!("P3: sim")
    }
    /// Relay time now (seconds).
    pub fn now(&self) -> u64 {
        todo!("P3: sim")
    }
    pub fn advance(&mut self, secs: u64) {
        let _ = secs;
        todo!("P3: sim")
    }
    /// Delivers and applies until nothing moves.
    pub fn settle(&mut self) {
        todo!("P3: sim")
    }

    // ── what a player does in Hopper ─────────────────────────────────

    pub fn edit(&mut self, who: &str, key: Key, now: Value) {
        let _ = (who, key, now);
        todo!("P3: sim")
    }
    /// Several changes as one action (one batch).
    pub fn edits(&mut self, who: &str, edits: &[(Key, Value)]) {
        let _ = (who, edits);
        todo!("P3: sim")
    }
    /// History undo of the last row for `key`.
    pub fn undo_last(&mut self, who: &str, key: &Key) {
        let _ = (who, key);
        todo!("P3: sim")
    }
    /// "Put back `member`'s changes" since relay time `since`.
    pub fn undo_member(&mut self, who: &str, member: &str, since: u64) {
        let _ = (who, member, since);
        todo!("P3: sim")
    }
    pub fn answer(&mut self, who: &str, review: u64, answer: Answer, all: bool) {
        let _ = (who, review, answer, all);
        todo!("P3: sim")
    }
    pub fn set_trust(&mut self, who: &str, member: &str, trust: Trust) {
        let _ = (who, member, trust);
        todo!("P3: sim")
    }
    pub fn push_by_hand(&mut self, who: &str, on: bool) {
        let _ = (who, on);
        todo!("P3: sim")
    }
    pub fn push(&mut self, who: &str) {
        let _ = who;
        todo!("P3: sim")
    }
    pub fn only_here(&mut self, who: &str, key: &Key) {
        let _ = (who, key);
        todo!("P3: sim")
    }
    pub fn allow_source(&mut self, who: &str, key: &Key) {
        let _ = (who, key);
        todo!("P3: sim")
    }
    pub fn put_back(&mut self, who: &str) {
        let _ = who;
        todo!("P5: sim")
    }
    pub fn share_drift(&mut self, who: &str) {
        let _ = who;
        todo!("P5: sim")
    }
    pub fn pack_plan(&self, who: &str, version: &str, mode: PackMode) -> PackPlan {
        let _ = (who, version, mode);
        todo!("P6: sim")
    }
    pub fn update_pack(&mut self, who: &str, version: &str, mode: PackMode) {
        let _ = (who, version, mode);
        todo!("P6: sim")
    }
    /// What a pack version ships, for every PC's lookups.
    pub fn pack_ships(&mut self, version: &str, keys: &[Key]) {
        let _ = (version, keys);
        todo!("P6: sim")
    }
    /// A config file changed on this PC (not shared).
    pub fn edit_config(&mut self, who: &str, file: &str) {
        let _ = (who, file);
        todo!("P6: sim")
    }
    /// Shares a mod's settings with the group (configsync).
    pub fn share_settings(&mut self, who: &str, mod_id: &str) {
        let _ = (who, mod_id);
        todo!("P3: sim")
    }
    pub fn set_setting(&mut self, who: &str, mod_id: &str, setting: &str, value: &str) {
        let _ = (who, mod_id, setting, value);
        todo!("P3: sim")
    }
    pub fn make_world(&mut self, who: &str, name: &str) {
        let _ = (who, name);
        todo!("P3: sim")
    }
    pub fn rename(&mut self, who: &str, name: &str) {
        let _ = (who, name);
        todo!("P3: sim")
    }
    /// Shares one more pack copy in the group.
    pub fn share_copy(&mut self, who: &str, copy: &str) {
        let _ = (who, copy);
        todo!("P4: sim")
    }

    // ── the PC ───────────────────────────────────────────────────────

    /// A change on disk Hopper didn't make (Explorer, another launcher).
    pub fn outside(&mut self, who: &str, key: Key, now: Value) {
        let _ = (who, key, now);
        todo!("P5: sim")
    }
    pub fn unreadable(&mut self, who: &str, on: bool) {
        let _ = (who, on);
        todo!("P5: sim")
    }
    pub fn game(&mut self, who: &str, running: bool) {
        let _ = (who, running);
        todo!("P6: sim")
    }
    /// The next downloads of `key`'s file on this PC end with `how`.
    pub fn fail_fetch(&mut self, who: &str, key: &Key, how: Outcome) {
        let _ = (who, key, how);
        todo!("P6: sim")
    }
    /// The PC dies after `after_steps` file swaps of its next apply.
    pub fn crash_mid_apply(&mut self, who: &str, after_steps: usize) {
        let _ = (who, after_steps);
        todo!("P6: sim")
    }
    /// Hopper restarts: state saved and loaded again, journal recovered.
    pub fn restart(&mut self, who: &str) {
        let _ = who;
        todo!("P5: sim")
    }
    pub fn clock_skew(&mut self, who: &str, secs: i64) {
        let _ = (who, secs);
        todo!("P3: sim")
    }
    /// Damages saved state: drops `author`'s record `n`.
    pub fn forget_record(&mut self, who: &str, author: &str, n: u64) {
        let _ = (who, author, n);
        todo!("P4: sim")
    }
    /// This PC sends packets of another wire version from now on.
    pub fn old_hopper(&mut self, who: &str) {
        let _ = who;
        todo!("P4: sim")
    }

    // ── the network and the group ────────────────────────────────────

    pub fn offline(&mut self, who: &str) {
        let _ = who;
        todo!("P3: sim")
    }
    pub fn online(&mut self, who: &str) {
        let _ = who;
        todo!("P3: sim")
    }
    /// The order senders' waiting events reach `who` next time.
    pub fn deliver_order(&mut self, who: &str, senders: &[&str]) {
        let _ = (who, senders);
        todo!("P3: sim")
    }
    pub fn relay_down(&mut self) {
        todo!("P4: sim")
    }
    pub fn relay_up(&mut self) {
        todo!("P4: sim")
    }
    pub fn relay_body_cap(&mut self, bytes: usize) {
        let _ = bytes;
        todo!("P4: sim")
    }
    pub fn join(&mut self, name: &str) {
        let _ = name;
        todo!("P3: sim")
    }
    pub fn remove(&mut self, by: &str, member: &str) {
        let _ = (by, member);
        todo!("P3: sim")
    }
    pub fn delete_group(&mut self, by: &str) {
        let _ = by;
        todo!("P3: sim")
    }

    // ── reading ──────────────────────────────────────────────────────

    /// What this PC's disk has.
    pub fn value(&self, who: &str, key: &Key) -> Value {
        let _ = (who, key);
        todo!("P3: sim")
    }
    /// What this PC's engine says the group agrees.
    pub fn group_value(&self, who: &str, key: &Key) -> Value {
        let _ = (who, key);
        todo!("P3: sim")
    }
    pub fn group_hash(&self, who: &str) -> String {
        let _ = who;
        todo!("P4: sim")
    }
    /// The shown name of who changed `key` last, as this PC knows it.
    pub fn last_by(&self, who: &str, key: &Key) -> Option<String> {
        let _ = (who, key);
        todo!("P3: sim")
    }
    pub fn history(&self, who: &str) -> Vec<HistoryRow> {
        let _ = who;
        todo!("P3: sim")
    }
    pub fn reviews(&self, who: &str) -> Vec<Review> {
        let _ = who;
        todo!("P3: sim")
    }
    /// Sentences this PC was told (notes, summaries).
    pub fn notes(&self, who: &str) -> Vec<String> {
        let _ = who;
        todo!("P3: sim")
    }
    pub fn status(&self, who: &str) -> Status {
        let _ = who;
        todo!("P5: sim")
    }
    pub fn drift(&self, who: &str) -> Vec<Drift> {
        let _ = who;
        todo!("P5: sim")
    }
    /// The keys `member` reported missing on their disk, as `who` sees it.
    pub fn member_missing(&self, who: &str, member: &str) -> Vec<Key> {
        let _ = (who, member);
        todo!("P5: sim")
    }
    pub fn by_hand(&self, who: &str) -> Vec<Key> {
        let _ = who;
        todo!("P6: sim")
    }
    pub fn fetch_tries(&self, who: &str, key: &Key) -> usize {
        let _ = (who, key);
        todo!("P6: sim")
    }
    /// How many files of `key` are on this PC's disk.
    pub fn files_of(&self, who: &str, key: &Key) -> usize {
        let _ = (who, key);
        todo!("P6: sim")
    }
    /// The folder `key`'s file sits in ("mods", "shaderpacks").
    pub fn folder(&self, who: &str, key: &Key) -> Option<String> {
        let _ = (who, key);
        todo!("P3: sim")
    }
    pub fn setting(&self, who: &str, mod_id: &str, setting: &str) -> Option<String> {
        let _ = (who, mod_id, setting);
        todo!("P3: sim")
    }
    pub fn edited_config(&self, who: &str, file: &str) -> bool {
        let _ = (who, file);
        todo!("P6: sim")
    }
    pub fn has_world(&self, who: &str, name: &str) -> bool {
        let _ = (who, name);
        todo!("P3: sim")
    }
    pub fn following(&self, who: &str) -> bool {
        let _ = who;
        todo!("P3: sim")
    }
    /// Copies this PC follows.
    pub fn copies(&self, who: &str) -> usize {
        let _ = who;
        todo!("P4: sim")
    }
    /// Events waiting for `who` on the relay.
    pub fn relay_events(&self, who: &str) -> usize {
        let _ = who;
        todo!("P4: sim")
    }
    /// Size of the packet this PC would send now.
    pub fn outbox_bytes(&self, who: &str) -> usize {
        let _ = who;
        todo!("P4: sim")
    }
    /// Records made by `author` that `who` holds.
    pub fn records_by(&self, who: &str, author: &str) -> usize {
        let _ = (who, author);
        todo!("P3: sim")
    }
}
