//! An in-memory friends group for the scenario tests
//! (`tests/sync_scenarios/`): one [`Engine`] per person, a fake mods folder
//! each, and a relay that keeps only the newest event per sender and key,
//! with its per-sender queue limit and body cap. Relay time moves only with
//! [`Group::advance`]; nothing sleeps.
//!
//! People are named by a short string ("A", "B"), which is also their
//! member id and shown name. Built in P3 (engine) and P4 (relay limits).
//!
//! Events carry records in memory (`Event`), not wire packets: the wire is
//! P4's. The relay is [`SimRelay`]: one event per (recipient, sender), the
//! newest replacing the older (every event is cumulative, so nothing is
//! lost). P4 adds its limits (queue size, body cap, down) to that struct.

use super::apply::Outcome;
use super::engine::{Answer, Engine, Review, Status};
use super::gate::{Limits, Trust};
use super::local::Drift;
use super::model::{Arrival, Change, Dot, Edit, Key, PackMode, Seen, Value, Why};
use super::plan::PackPlan;
use super::wire::MemberStatus;
use std::collections::{BTreeMap, BTreeSet};

/// Relay time when a group starts (so "never online" is 0, never "now").
const START: u64 = 1_000_000;

/// One History row as a person's History would show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryRow {
    pub key: Key,
    /// "Removed by A".
    pub says: String,
    /// Who made it.
    pub by: String,
}

/// What one sender has for one recipient on the relay: every record the
/// recipient hasn't acked (or a checkpoint), and the sender's status.
#[derive(Clone, Debug)]
pub struct Event {
    pub seq: u64,
    /// Relay time it was posted.
    pub at: u64,
    pub from: String,
    pub changes: Vec<Change>,
    pub status: MemberStatus,
}

/// Something the relay tells a PC besides records.
#[derive(Clone, Debug)]
enum Notice {
    Removed { by: String },
    Deleted { by: String },
    Told(String),
}

/// The relay between the PCs.
#[derive(Default)]
pub struct SimRelay {
    pub seq: u64,
    /// The newest event per (recipient, sender).
    pub events: BTreeMap<(String, String), Event>,
    notices: BTreeMap<String, Vec<Notice>>,
}

impl SimRelay {
    /// Stores an event, replacing the sender's older one for this recipient.
    pub fn post(&mut self, to: &str, mut event: Event) {
        self.seq += 1;
        event.seq = self.seq;
        self.events.insert((to.to_string(), event.from.clone()), event);
    }

    /// Takes everything waiting for `to`, in `order` (senders named first
    /// come first), else by sender name.
    pub fn take(&mut self, to: &str, order: Option<&[String]>) -> Vec<Event> {
        let mut froms: Vec<String> =
            self.events.keys().filter(|(t, _)| t == to).map(|(_, f)| f.clone()).collect();
        if let Some(order) = order {
            froms.sort_by_key(|f| order.iter().position(|o| o == f).unwrap_or(usize::MAX));
        }
        froms.into_iter().filter_map(|f| self.events.remove(&(to.to_string(), f))).collect()
    }
}

/// One person's PC.
struct Pc {
    engine: Engine,
    disk: BTreeMap<Key, Value>,
    online: bool,
    /// Relay time it went offline.
    offline_since: u64,
    /// It has talked to the relay at least once (a joiner hasn't yet).
    ever_contacted: bool,
    following: bool,
    /// Only stored: nothing may read a PC clock (that is the point of the test).
    #[allow(dead_code)]
    skew: i64,
    push_by_hand: bool,
    push_pending: bool,
    only_here: BTreeSet<Key>,
    notes: Vec<String>,
    told_loader: BTreeSet<Dot>,
    /// Came back from a long absence: counts what arrives for a summary.
    returning: bool,
    away: BTreeMap<String, usize>,
    order: Option<Vec<String>>,
    /// What was last posted to each member: its records and this PC's seen.
    last_posted: BTreeMap<String, (Vec<Dot>, Seen)>,
    /// Members that still need a checkpoint from this PC.
    checkpoint_for: BTreeSet<String>,
    settings: BTreeMap<(String, String), String>,
    settings_read: usize,
    worlds: BTreeSet<String>,
    /// The instance's name; renaming it changes nothing in sync.
    #[allow(dead_code)]
    instance: String,
}

impl Pc {
    fn disk_value(&self, key: &Key) -> Value {
        self.disk.get(key).cloned().unwrap_or_default()
    }

    /// Puts a value on disk and records that it was applied.
    fn land(&mut self, key: &Key, value: Value) {
        self.disk.insert(key.clone(), value.clone());
        self.engine.applied.insert(key.clone(), value);
    }

    /// A change the player makes here: a record, unless the mod stays on
    /// this PC.
    fn make(&mut self, key: &Key, now: &Value, why: Why, batch: &str, at: u64) {
        if !self.only_here.contains(key) {
            let edit = Edit { key: key.clone(), now: now.clone(), why, batch: batch.into() };
            self.engine.record(edit, at);
        }
        self.land(key, now.clone());
    }
}

/// A group of people, their PCs and the relay between them.
pub struct Group {
    now: u64,
    relay: SimRelay,
    pcs: BTreeMap<String, Pc>,
    members: BTreeSet<String>,
    /// What a new PC starts on.
    base: Vec<(Key, Value)>,
    batches: u64,
    limits: Limits,
    shared: BTreeSet<String>,
    /// Shared settings changes: (who, mod, setting, value).
    setting_log: Vec<(String, String, String, String)>,
}

impl Group {
    // ── setup ────────────────────────────────────────────────────────

    /// `names` in one group, all online, every PC on `mods` and in step.
    pub fn with(names: &[&str], mods: &[(Key, Value)]) -> Group {
        let mut g = Group {
            now: START,
            relay: SimRelay::default(),
            pcs: BTreeMap::new(),
            members: BTreeSet::new(),
            base: mods.to_vec(),
            batches: 0,
            limits: Limits::default(),
            shared: BTreeSet::new(),
            setting_log: Vec::new(),
        };
        for n in names {
            g.add_pc(n, true);
            g.members.insert(n.to_string());
        }
        g
    }
    /// Relay time now (seconds).
    pub fn now(&self) -> u64 {
        self.now
    }
    pub fn advance(&mut self, secs: u64) {
        self.now += secs;
    }
    /// Delivers and applies until nothing moves.
    pub fn settle(&mut self) {
        let now = self.now;
        for pc in self.pcs.values_mut() {
            if pc.online {
                pc.ever_contacted = true;
                pc.engine.touch(now);
            }
        }
        for _ in 0..200 {
            let mut moved = self.post_all();
            moved |= self.deliver_all();
            moved |= self.deliver_notices();
            moved |= self.apply_all();
            moved |= self.settings_all();
            if !moved {
                self.summarise();
                return;
            }
        }
        panic!("the group did not settle in 200 rounds");
    }

    // ── what a player does in Hopper ─────────────────────────────────

    pub fn edit(&mut self, who: &str, key: Key, now: Value) {
        self.edits(who, &[(key, now)]);
    }
    /// Several changes as one action (one batch).
    pub fn edits(&mut self, who: &str, edits: &[(Key, Value)]) {
        let at = self.contact(who);
        let batch = self.new_batch(who);
        let pc = self.pc_mut(who);
        for (key, now) in edits {
            pc.make(key, now, Why::Player, &batch, at);
        }
    }
    /// History undo of the last row for `key`.
    pub fn undo_last(&mut self, who: &str, key: &Key) {
        let at = self.contact(who);
        let batch = self.new_batch(who);
        let pc = self.pc_mut(who);
        let last = pc
            .engine
            .log
            .changes
            .values()
            .map(|(c, _)| c)
            .filter(|c| &c.key == key)
            .max_by_key(|c| (c.lamport, c.dot.clone()))
            .cloned();
        if let Some(last) = last {
            pc.make(key, &last.was, Why::Undo { of: last.dot }, &batch, at);
        }
    }
    /// "Put back `member`'s changes" since relay time `since`.
    pub fn undo_member(&mut self, who: &str, member: &str, since: u64) {
        let at = self.contact(who);
        let pc = self.pc_mut(who);
        let (edits, left) = pc.engine.undo_member_plan(member, since);
        for e in edits {
            pc.make(&e.key, &e.now, e.why, &e.batch, at);
        }
        if !left.is_empty() {
            pc.notes.push(format!("Left {} as is: changed since {member}'s change.", left.len()));
        }
    }
    pub fn answer(&mut self, who: &str, review: u64, answer: Answer, all: bool) {
        let at = self.contact(who);
        let pc = self.pc_mut(who);
        pc.engine.touch(at);
        pc.engine.answer(review, answer, all);
    }
    pub fn set_trust(&mut self, who: &str, member: &str, trust: Trust) {
        self.pc_mut(who).engine.set_trust(member, trust);
    }
    pub fn push_by_hand(&mut self, who: &str, on: bool) {
        self.pc_mut(who).push_by_hand = on;
    }
    pub fn push(&mut self, who: &str) {
        self.pc_mut(who).push_pending = true;
    }
    pub fn only_here(&mut self, who: &str, key: &Key) {
        self.pc_mut(who).only_here.insert(key.clone());
    }
    pub fn allow_source(&mut self, who: &str, key: &Key) {
        self.pc_mut(who).engine.allow_source(key);
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
    /// Shares a mod's settings with the group (configsync, which rides
    /// along unchanged: settings travel beside the records).
    pub fn share_settings(&mut self, who: &str, mod_id: &str) {
        let _ = who;
        self.shared.insert(mod_id.to_string());
    }
    pub fn set_setting(&mut self, who: &str, mod_id: &str, setting: &str, value: &str) {
        self.pc_mut(who).settings.insert((mod_id.into(), setting.into()), value.into());
        if self.shared.contains(mod_id) {
            self.setting_log.push((who.into(), mod_id.into(), setting.into(), value.into()));
        }
    }
    pub fn make_world(&mut self, who: &str, name: &str) {
        self.pc_mut(who).worlds.insert(name.to_string());
    }
    pub fn rename(&mut self, who: &str, name: &str) {
        self.pc_mut(who).instance = name.to_string();
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
    /// The PC's own clock is off by `secs`. Nothing reads it: only relay
    /// time orders or gates anything.
    pub fn clock_skew(&mut self, who: &str, secs: i64) {
        self.pc_mut(who).skew = secs;
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
        let now = self.now;
        let pc = self.pc_mut(who);
        if pc.online {
            pc.online = false;
            pc.offline_since = now;
        }
    }
    pub fn online(&mut self, who: &str) {
        let (now, away) = (self.now, self.limits.away_secs);
        let pc = self.pc_mut(who);
        if !pc.online {
            if pc.ever_contacted && now.saturating_sub(pc.offline_since) > away {
                pc.returning = true;
                pc.away.clear();
            }
            pc.online = true;
        }
    }
    /// The order senders' waiting events reach `who` next time.
    pub fn deliver_order(&mut self, who: &str, senders: &[&str]) {
        self.pc_mut(who).order = Some(senders.iter().map(|s| s.to_string()).collect());
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
    /// A new person: starts on the group's first state, as a share code
    /// gives it, and is sent a checkpoint by everyone.
    pub fn join(&mut self, name: &str) {
        self.add_pc(name, false);
        for m in self.members.clone() {
            self.pc_mut(&m).checkpoint_for.insert(name.to_string());
        }
        self.members.insert(name.to_string());
    }
    pub fn remove(&mut self, by: &str, member: &str) {
        self.members.remove(member);
        self.relay.events.retain(|(to, _), _| to != member);
        self.relay.notices.entry(member.to_string()).or_default().push(Notice::Removed { by: by.into() });
        for m in self.members.clone() {
            if m != by {
                let told = Notice::Told(format!("{by} removed {member} from the group."));
                self.relay.notices.entry(m).or_default().push(told);
            }
        }
        self.pc_mut(by).notes.push(format!("{member} was removed from the group."));
    }
    pub fn delete_group(&mut self, by: &str) {
        for m in std::mem::take(&mut self.members) {
            if m != by {
                self.relay.notices.entry(m).or_default().push(Notice::Deleted { by: by.into() });
            }
        }
        self.relay.events.clear();
        let pc = self.pc_mut(by);
        pc.following = false;
        pc.notes.push("The group was deleted.".into());
    }

    // ── reading ──────────────────────────────────────────────────────

    /// What this PC's disk has.
    pub fn value(&self, who: &str, key: &Key) -> Value {
        self.pc(who).disk_value(key)
    }
    /// What this PC's engine says the group agrees.
    pub fn group_value(&self, who: &str, key: &Key) -> Value {
        self.pc(who).engine.value(key)
    }
    pub fn group_hash(&self, who: &str) -> String {
        let _ = who;
        todo!("P4: sim")
    }
    /// The shown name of who changed `key` last, as this PC knows it.
    pub fn last_by(&self, who: &str, key: &Key) -> Option<String> {
        self.pc(who).engine.last_change(key).map(|c| c.name.clone())
    }
    pub fn history(&self, who: &str) -> Vec<HistoryRow> {
        let mut all: Vec<&Change> = self.pc(who).engine.log.changes.values().map(|(c, _)| c).collect();
        all.sort_by_key(|c| (c.lamport, c.dot.clone()));
        all.into_iter()
            .map(|c| HistoryRow { key: c.key.clone(), says: says(c), by: c.name.clone() })
            .collect()
    }
    pub fn reviews(&self, who: &str) -> Vec<Review> {
        self.pc(who).engine.reviews()
    }
    /// Sentences this PC was told (notes, summaries).
    pub fn notes(&self, who: &str) -> Vec<String> {
        self.pc(who).notes.clone()
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
        let Key::Mod(k) = key else { return None };
        if self.pc(who).disk_value(key) == Value::Gone {
            return None;
        }
        let folder = match k.split(':').next() {
            Some("shader") => "shaderpacks",
            Some("rp") => "resourcepacks",
            Some("dp") => "datapacks",
            _ => "mods",
        };
        Some(folder.to_string())
    }
    pub fn setting(&self, who: &str, mod_id: &str, setting: &str) -> Option<String> {
        self.pc(who).settings.get(&(mod_id.to_string(), setting.to_string())).cloned()
    }
    pub fn edited_config(&self, who: &str, file: &str) -> bool {
        let _ = (who, file);
        todo!("P6: sim")
    }
    pub fn has_world(&self, who: &str, name: &str) -> bool {
        self.pc(who).worlds.contains(name)
    }
    pub fn following(&self, who: &str) -> bool {
        self.pc(who).following
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
        self.pc(who).engine.log.changes.keys().filter(|d| d.author == author).count()
    }

    // ── inside ───────────────────────────────────────────────────────

    fn pc(&self, who: &str) -> &Pc {
        self.pcs.get(who).unwrap_or_else(|| panic!("no PC named {who}"))
    }

    fn pc_mut(&mut self, who: &str) -> &mut Pc {
        self.pcs.get_mut(who).unwrap_or_else(|| panic!("no PC named {who}"))
    }

    fn add_pc(&mut self, name: &str, ever_contacted: bool) {
        let mut engine = Engine::new("pack", name, name);
        engine.limits = self.limits;
        let mut disk = BTreeMap::new();
        for (k, v) in &self.base {
            disk.insert(k.clone(), v.clone());
            engine.applied.insert(k.clone(), v.clone());
        }
        let settings_read = self.setting_log.len();
        self.pcs.insert(
            name.to_string(),
            Pc {
                engine,
                disk,
                online: true,
                offline_since: 0,
                ever_contacted,
                following: true,
                skew: 0,
                push_by_hand: false,
                push_pending: false,
                only_here: BTreeSet::new(),
                notes: Vec::new(),
                told_loader: BTreeSet::new(),
                returning: false,
                away: BTreeMap::new(),
                order: None,
                last_posted: BTreeMap::new(),
                checkpoint_for: BTreeSet::new(),
                settings: BTreeMap::new(),
                settings_read,
                worlds: BTreeSet::new(),
                instance: "Our pack".into(),
            },
        );
    }

    fn new_batch(&mut self, who: &str) -> String {
        self.batches += 1;
        format!("{who}#{}", self.batches)
    }

    /// The relay time of this PC's last contact (0: never had any).
    fn contact(&self, who: &str) -> u64 {
        let pc = self.pc(who);
        if !pc.ever_contacted {
            0
        } else if pc.online {
            self.now
        } else {
            pc.offline_since
        }
    }

    /// Every online PC posts what each member hasn't acked, or a
    /// checkpoint to a new one, when that differs from what it last posted.
    fn post_all(&mut self) -> bool {
        let members: Vec<String> = self.members.iter().cloned().collect();
        let mut moved = false;
        for s in &members {
            let at = self.now;
            let pc = self.pc_mut(s);
            if !pc.online || !pc.following || (pc.push_by_hand && !pc.push_pending) {
                continue;
            }
            let mut events = Vec::new();
            for r in members.iter().filter(|r| *r != s) {
                if let Some(acked) = pc.engine.members.get(r).map(|m| m.seen.clone()) {
                    if pc.checkpoint_for.contains(r)
                        && pc.engine.checkpoint().iter().all(|c| acked.covers(&c.dot))
                    {
                        pc.checkpoint_for.remove(r);
                    }
                }
                let mut changes = pc.engine.unacked_by(r);
                if pc.checkpoint_for.contains(r) {
                    for c in pc.engine.checkpoint() {
                        if !changes.iter().any(|x| x.dot == c.dot) {
                            changes.push(c);
                        }
                    }
                }
                let dots: Vec<Dot> = changes.iter().map(|c| c.dot.clone()).collect();
                let seen = pc.engine.seen();
                let last = pc.last_posted.get(r).cloned().unwrap_or_default();
                let new_records = !dots.is_empty() && dots != last.0;
                if !new_records && seen == last.1 {
                    continue;
                }
                pc.last_posted.insert(r.clone(), (dots, seen));
                let event = Event { seq: 0, at, from: s.clone(), changes, status: pc.engine.my_status() };
                events.push((r.clone(), event));
            }
            pc.push_pending = false;
            for (r, event) in events {
                self.relay.post(&r, event);
                moved = true;
            }
        }
        moved
    }

    /// Every online PC takes in what waits for it.
    fn deliver_all(&mut self) -> bool {
        let names: Vec<String> = self.pcs.keys().cloned().collect();
        let mut moved = false;
        for p in &names {
            let pc = self.pc(p);
            if !pc.online || !pc.following || !self.relay.events.keys().any(|(to, _)| to == p) {
                continue;
            }
            let order = self.pc_mut(p).order.take();
            let events = self.relay.take(p, order.as_deref());
            let pc = self.pc_mut(p);
            for e in events {
                let at = Arrival { seq: e.seq, at: e.at };
                let got = pc.engine.ingest(&e.from, e.changes, e.status, at);
                pc.notes.extend(got.notes);
                if pc.returning {
                    for (name, n) in got.by_author {
                        *pc.away.entry(name).or_default() += n;
                    }
                }
                moved = true;
            }
        }
        moved
    }

    fn deliver_notices(&mut self) -> bool {
        let names: Vec<String> = self.pcs.keys().cloned().collect();
        let mut moved = false;
        for p in &names {
            if !self.pc(p).online {
                continue;
            }
            let Some(list) = self.relay.notices.remove(p) else { continue };
            let pc = self.pc_mut(p);
            for n in list {
                match n {
                    Notice::Removed { by } => {
                        pc.following = false;
                        pc.notes.push(format!("{by} removed you from the group. Your copy stays as it is."));
                    }
                    Notice::Deleted { by } => {
                        pc.following = false;
                        pc.notes.push(format!("{by} deleted the group. Your copy stays as it is."));
                    }
                    Notice::Told(s) => pc.notes.push(s),
                }
                moved = true;
            }
        }
        moved
    }

    /// Every PC puts what the engine lets land on its disk.
    fn apply_all(&mut self) -> bool {
        let mut moved = false;
        for pc in self.pcs.values_mut() {
            for step in pc.engine.lands() {
                if pc.only_here.contains(&step.key) {
                    continue;
                }
                pc.land(&step.key, step.to);
                moved = true;
            }
            for (dot, note) in pc.engine.not_followed() {
                if pc.told_loader.insert(dot) {
                    pc.notes.push(note);
                    moved = true;
                }
            }
        }
        moved
    }

    /// Shared settings reach everyone else, online ones first come first.
    fn settings_all(&mut self) -> bool {
        let mut moved = false;
        for (name, pc) in self.pcs.iter_mut() {
            if !pc.online || !pc.following {
                continue;
            }
            for (from, m, s, v) in &self.setting_log[pc.settings_read..] {
                if from != name {
                    pc.settings.insert((m.clone(), s.clone()), v.clone());
                    moved = true;
                }
            }
            pc.settings_read = self.setting_log.len();
        }
        moved
    }

    /// A PC back from a long absence is told what happened, once.
    fn summarise(&mut self) {
        for pc in self.pcs.values_mut() {
            if pc.returning {
                if let Some(s) = Engine::away_summary(&pc.away) {
                    pc.notes.push(s);
                    pc.returning = false;
                    pc.away.clear();
                }
            }
        }
    }
}

/// A History row's sentence.
fn says(c: &Change) -> String {
    let by = &c.name;
    match &c.why {
        Why::Undo { .. } => return format!("Put back by {by}"),
        Why::Answer { .. } if c.was == c.now => return format!("Kept by {by}"),
        _ => {}
    }
    let verb = match (&c.was, &c.now) {
        (_, Value::Gone | Value::PackRemoved) => "Removed",
        (Value::File { file: a, on: false }, Value::File { file: b, on: true }) if a == b => "Turned on",
        (_, Value::File { on: false, .. } | Value::PackOff) => "Turned off",
        (Value::Gone, Value::File { .. }) => "Added",
        (Value::File { .. }, Value::File { .. }) => "Updated",
        _ => "Changed",
    };
    format!("{verb} by {by}")
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

    fn group() -> Group {
        Group::with(&["A", "B"], &[(k(1), on(11))])
    }

    #[test]
    fn the_relay_keeps_only_the_newest_event_per_sender() {
        let mut r = SimRelay::default();
        let ev = |at| Event { seq: 0, at, from: "A".into(), changes: vec![], status: MemberStatus::default() };
        r.post("B", ev(1));
        r.post("B", ev(2));
        r.post("C", ev(3));
        let got = r.take("B", None);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].at, 2);
        assert!(r.take("B", None).is_empty());
        assert_eq!(r.take("C", None).len(), 1);
    }

    #[test]
    fn take_follows_the_order_asked_for() {
        let mut r = SimRelay::default();
        for from in ["A", "B", "C"] {
            let e = Event { seq: 0, at: 1, from: from.into(), changes: vec![], status: MemberStatus::default() };
            r.post("D", e);
        }
        let order = vec!["C".to_string(), "A".to_string()];
        let froms: Vec<String> = r.take("D", Some(&order)).into_iter().map(|e| e.from).collect();
        assert_eq!(froms, ["C", "A", "B"]);
    }

    #[test]
    fn a_change_reaches_the_friend_and_settling_goes_quiet() {
        let mut g = group();
        g.edit("A", k(1), on(12));
        assert_eq!(g.value("B", &k(1)), on(11), "nothing travels before a settle");
        g.settle();
        assert_eq!(g.value("B", &k(1)), on(12));
        // A second settle with nothing to do changes nothing and doesn't hang.
        g.settle();
        assert_eq!(g.relay.events.len(), 0);
    }

    #[test]
    fn an_offline_pc_gets_the_newest_event_with_everything_in_it() {
        let mut g = group();
        g.offline("B");
        g.edit("A", k(1), Value::File { file: "cf:11".into(), on: false });
        g.settle();
        g.edit("A", k(1), on(11));
        g.settle();
        assert_eq!(g.relay.events.len(), 1, "one event for B, the newest");
        g.online("B");
        g.settle();
        assert_eq!(g.records_by("B", "A"), 2);
    }

    #[test]
    fn relay_time_only_moves_with_advance() {
        let mut g = group();
        let t = g.now();
        g.settle();
        assert_eq!(g.now(), t);
        g.advance(90);
        assert_eq!(g.now(), t + 90);
    }

    #[test]
    fn history_sentences() {
        let mut g = group();
        g.edit("A", k(1), Value::File { file: "cf:11".into(), on: false });
        g.edit("A", k(1), on(11));
        g.edit("A", k(1), on(12));
        g.edit("A", k(1), Value::Gone);
        g.edit("A", k(1), on(13));
        let says: Vec<String> = g.history("A").into_iter().map(|h| h.says).collect();
        assert_eq!(
            says,
            ["Turned off by A", "Turned on by A", "Updated by A", "Removed by A", "Added by A"]
        );
    }
}
