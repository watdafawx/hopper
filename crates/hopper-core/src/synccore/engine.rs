//! The one object the apps use for a group copy (design §11). Pure: every
//! method is a function of the engine's state and its arguments.
//!
//! Holds the group's log of records, folds it with `register`, runs the
//! gate on what is about to land, and keeps the player's answers. Drift
//! and status (P5), plan/apply (P6) and the wire outbox (P4) are other
//! phases; their methods stay `todo!()` here.

use super::apply::ApplyReport;
use super::gate::{self, GateCtx, Limits, Risk, Trust};
use super::local::{self, Drift, Snapshot};
use super::model::{Arrival, Change, Dot, Edit, Key, MemberId, PackMode, Seen, Value, Why};
use super::plan::{ApplyPlan, PackPlan, Step};
use super::register::{fold, merge, Log, Merge, Register};
use super::wire::{Body, MemberStatus, Packet, WIRE_VERSION};
use std::collections::{BTreeMap, BTreeSet};

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
    /// New records of other members, by their shown name (feeds
    /// [`Engine::away_summary`]).
    pub by_author: BTreeMap<String, usize>,
    /// Sentences for the player ("Mia changed this too; ...").
    pub notes: Vec<String>,
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
    /// Held records the player let land ("Take them").
    pub taken: BTreeSet<Dot>,
    /// Link sources ("src:<file>") the player allowed on this PC.
    pub allowed_sources: BTreeSet<String>,
    /// Records of this PC that lost a concurrent update and were already
    /// told about it.
    pub told: BTreeSet<Dot>,
    /// Relay time of this PC's last contact (set by `touch` and every
    /// receive); the "online at" of what it records.
    pub contact: u64,
    /// Members whose packets were refused (another wire version), until a
    /// packet of theirs is taken in again.
    pub outdated: BTreeSet<MemberId>,
}

/// The registers of the log, and which winners wait for the player.
struct Judged {
    regs: BTreeMap<Key, Register>,
    /// Winners the gate holds, with every reason (a clash is not held: the
    /// register keeps the non-destructive side, see `judged`).
    held: BTreeMap<Dot, Vec<Risk>>,
}

impl Engine {
    pub fn new(copy: &str, me: &str, name: &str) -> Engine {
        Engine {
            copy: copy.to_string(),
            me: me.to_string(),
            name: name.to_string(),
            ..Engine::default()
        }
    }

    // ── this PC ──────────────────────────────────────────────────────

    /// The app saw the relay at this time (every successful poll).
    pub fn touch(&mut self, at: u64) {
        self.contact = self.contact.max(at);
    }

    /// Records a change this PC made. `online_at` is the relay time of its
    /// last contact before it (0: the caller doesn't know, this engine's own
    /// last contact stands in; still 0 for a PC that never had contact).
    pub fn record(&mut self, edit: Edit, online_at: u64) -> Dot {
        let n = self.next_n();
        let lamport = self.log.changes.values().map(|(c, _)| c.lamport).max().unwrap_or(0) + 1;
        // What this PC had, not what the group agrees: a held change must
        // not make a later removal look like a turn-off of something new.
        let was = self.applied.get(&edit.key).cloned().unwrap_or_default();
        let dot = Dot { author: self.me.clone(), n };
        let batch = if edit.batch.is_empty() { format!("{}:{n}", self.me) } else { edit.batch };
        let change = Change {
            dot: dot.clone(),
            key: edit.key,
            was,
            now: edit.now,
            name: self.name.clone(),
            lamport,
            seen: self.seen(),
            online_at: if online_at == 0 { self.contact } else { online_at },
            batch,
            why: edit.why,
        };
        self.log.insert(change, None);
        dot
    }

    /// Where the scanned disk differs from what this PC applied. Never sent.
    pub fn drift(&self, disk: &Snapshot) -> Vec<Drift> {
        local::drift(&self.applied, disk)
    }

    /// Keys the group has that this disk lacks (the `missing` of this PC's
    /// status): drift where the file is gone, and what the register says
    /// should be there. An unreadable instance reports none, not everything.
    pub fn missing(&self, disk: &Snapshot) -> Vec<Key> {
        if disk.unreadable {
            return Vec::new();
        }
        let mut keys: BTreeSet<Key> = self
            .drift(disk)
            .into_iter()
            .filter(|d| d.found == Value::Gone && d.expected != Value::Gone)
            .map(|d| d.key)
            .collect();
        for step in self.lands() {
            let there = disk.values.get(&step.key).is_some_and(|v| *v != Value::Gone);
            if matches!(step.key, Key::Mod(_)) && !step.to.is_away() && !there {
                keys.insert(step.key);
            }
        }
        keys.into_iter().collect()
    }

    /// A member's packet was refused (another wire version): the status
    /// says so until one of theirs is taken in.
    pub fn note_out_of_date(&mut self, member: &str) {
        self.outdated.insert(member.to_string());
    }
    pub fn outbox(&self) -> Option<Packet> {
        todo!("P4: outbox")
    }
    pub fn sent(&mut self, packet_hash: &str, arrival: Arrival) {
        let _ = (packet_hash, arrival);
        todo!("P4: sent")
    }

    // ── friends ──────────────────────────────────────────────────────

    /// A packet from a friend. Refuses another wire version.
    pub fn receive(&mut self, packet: Packet, arrival: Arrival) -> Received {
        let mut packet = packet;
        if packet.version != WIRE_VERSION {
            self.outdated.insert(packet.from);
            return Received { refused: true, ..Received::default() };
        }
        let Some((body, status, _hash)) = packet.copies.remove(&self.copy) else {
            return Received::default();
        };
        let changes = match body {
            Body::Tail(c) | Body::Checkpoint(c) => c,
        };
        self.ingest(&packet.from, changes, status, arrival)
    }

    /// Takes in a friend's records and status. The same record twice, in
    /// any order, changes nothing more than the first time.
    pub fn ingest(
        &mut self,
        from: &str,
        changes: Vec<Change>,
        status: MemberStatus,
        arrival: Arrival,
    ) -> Received {
        let before: BTreeSet<u64> = self.reviews().iter().map(|r| r.id).collect();
        let mut got = Received::default();
        for c in changes {
            let mine = c.dot.author == self.me;
            let name = c.name.clone();
            if self.log.insert(c, Some(arrival)) && !mine {
                got.new += 1;
                *got.by_author.entry(name).or_default() += 1;
            }
        }
        self.members.insert(from.to_string(), status);
        self.outdated.remove(from);
        self.touch(arrival.at);
        got.notes = self.tell_losers();
        got.reviews = self
            .reviews()
            .iter()
            .map(|r| r.id)
            .filter(|id| !before.contains(id))
            .collect();
        got
    }

    /// The review cards: clashes waiting for an answer, then held records
    /// grouped by author, batch and reason.
    pub fn reviews(&self) -> Vec<Review> {
        let j = self.judged();
        let mut out = Vec::new();
        for (key, reg) in &j.regs {
            if !reg.is_clash() {
                continue;
            }
            let other = clash_other(reg);
            let mut changes = vec![reg.winner.dot.clone()];
            changes.extend(reg.open.iter().map(|o| o.dot.clone()));
            out.push(Review {
                id: card_id("clash", &format!("{key:?}"), ""),
                risk: Risk::Clash,
                changes,
                keys: vec![key.clone()],
                who: other.name.clone(),
                says: clash_says(reg, other),
            });
        }
        let mut groups: BTreeMap<(String, String, u8), Vec<(&Change, &Risk)>> = BTreeMap::new();
        for (dot, risks) in &j.held {
            let (c, _) = &self.log.changes[dot];
            groups
                .entry((dot.author.clone(), c.batch.clone(), risk_rank(&risks[0])))
                .or_default()
                .push((c, &risks[0]));
        }
        for ((author, batch, rank), items) in groups {
            let risk = items[0].1.clone();
            let who = items[0].0.name.clone();
            let mut keys: Vec<Key> = items.iter().map(|(c, _)| c.key.clone()).collect();
            keys.dedup();
            out.push(Review {
                id: card_id(&rank.to_string(), &author, &batch),
                says: card_says(&risk, &who, items.len()),
                risk,
                changes: items.iter().map(|(c, _)| c.dot.clone()).collect(),
                keys,
                who,
            });
        }
        out
    }

    /// Answers a card (and with `all`, every other card of the same
    /// reason). Take: a clash takes the removal, anything else lets the
    /// held records land; no new record for those. Keep: writes follow-up
    /// records that restore what this PC has, named after the player.
    pub fn answer(&mut self, review: u64, answer: Answer, all: bool) -> Vec<Dot> {
        let cards = self.reviews();
        let Some(card) = cards.iter().find(|c| c.id == review) else {
            return Vec::new();
        };
        let mut picked = vec![card];
        if all {
            picked.extend(
                cards
                    .iter()
                    .filter(|c| c.id != review && risk_rank(&c.risk) == risk_rank(&card.risk)),
            );
        }
        let regs = self.registers();
        let batch = format!("{}-answer-{}", self.me, self.next_n());
        let mut edits = Vec::new();
        for c in picked {
            if c.risk == Risk::Clash {
                let reg = &regs[&c.keys[0]];
                let now = match answer {
                    Answer::Take => clash_other(reg).now.clone(),
                    Answer::Keep => reg.winner.now.clone(),
                };
                edits.push((c.keys[0].clone(), now, c.id));
            } else if answer == Answer::Take {
                self.taken.extend(c.changes.iter().cloned());
            } else {
                for dot in &c.changes {
                    let key = self.log.changes[dot].0.key.clone();
                    let kept = self.applied.get(&key).cloned().unwrap_or_default();
                    edits.push((key, kept, c.id));
                }
            }
        }
        edits
            .into_iter()
            .map(|(key, now, id)| {
                let edit = Edit { key, now, why: Why::Answer { review: id }, batch: batch.clone() };
                self.record(edit, 0)
            })
            .collect()
    }

    pub fn set_trust(&mut self, member: &str, trust: Trust) {
        self.trust.insert(member.to_string(), trust);
    }

    /// The player allowed this link source ("src:<file>") on this PC.
    pub fn allow_source(&mut self, key: &Key) {
        if let Key::Mod(k) = key {
            self.allowed_sources.insert(k.clone());
        }
    }

    // ── disk ─────────────────────────────────────────────────────────

    /// What this PC's mods folder should change to: the register minus what
    /// the gate holds, against what was applied. The apply plan (P6) builds
    /// on it.
    pub fn lands(&self) -> Vec<Step> {
        let j = self.judged();
        let mut steps = Vec::new();
        for (key, reg) in &j.regs {
            let w = &reg.winner;
            if j.held.contains_key(&w.dot) {
                continue;
            }
            let from = self.applied.get(key).cloned().unwrap_or_default();
            if from == w.now || (*key == Key::Loader && !loader_follows(&from, &w.now)) {
                continue;
            }
            steps.push(Step {
                key: key.clone(),
                from,
                to: w.now.clone(),
                by: w.dot.clone(),
                name: w.name.clone(),
            });
        }
        steps
    }

    /// Changes that are the group's but not followed here: a switch to
    /// another kind of loader (Forge to NeoForge) is told, never applied.
    pub fn not_followed(&self) -> Vec<(Dot, String)> {
        let j = self.judged();
        let mut out = Vec::new();
        if let Some(reg) = j.regs.get(&Key::Loader) {
            let w = &reg.winner;
            let from = self.applied.get(&Key::Loader).cloned().unwrap_or_default();
            if !j.held.contains_key(&w.dot) && from != w.now && !loader_follows(&from, &w.now) {
                out.push((
                    w.dot.clone(),
                    format!(
                        "{} switched to {}. Hopper doesn't follow a change of loader, so this copy stays on {}.",
                        w.name,
                        loader_label(&w.now),
                        loader_label(&from)
                    ),
                ));
            }
        }
        out
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

    /// The edits that put back what `member` changed since relay time
    /// `since`, one batch.
    pub fn undo_member(&self, member: &str, since: u64) -> Vec<Edit> {
        self.undo_member_plan(member, since).0
    }

    /// Like [`Engine::undo_member`], and the keys left alone because
    /// someone else changed them after `member`.
    pub fn undo_member_plan(&self, member: &str, since: u64) -> (Vec<Edit>, Vec<Key>) {
        let regs = self.registers();
        let batch = format!("{}-undo-{}", self.me, self.next_n());
        let mut mine: BTreeMap<&Key, &Change> = BTreeMap::new();
        for (c, arrival) in self.log.changes.values() {
            let recent = arrival.is_some_and(|a| a.at >= since);
            if c.dot.author == member && recent && mine.get(&c.key).is_none_or(|o| o.lamport < c.lamport) {
                mine.insert(&c.key, c);
            }
        }
        let (mut edits, mut left) = (Vec::new(), Vec::new());
        for (key, c) in mine {
            match regs.get(key) {
                Some(reg) if reg.winner.dot == c.dot => {
                    if c.was != c.now {
                        edits.push(Edit {
                            key: key.clone(),
                            now: c.was.clone(),
                            why: Why::Undo { of: c.dot.clone() },
                            batch: batch.clone(),
                        });
                    }
                }
                _ => left.push(key.clone()),
            }
        }
        (edits, left)
    }

    // ── reading ──────────────────────────────────────────────────────

    /// What the group agrees for a key (the register), held or not.
    pub fn value(&self, key: &Key) -> Value {
        self.registers()
            .get(key)
            .map(|r| r.winner.now.clone())
            .or_else(|| self.applied.get(key).cloned())
            .unwrap_or_default()
    }

    pub fn last_change(&self, key: &Key) -> Option<&Change> {
        let winner = self.registers().get(key)?.winner.dot.clone();
        self.log.changes.get(&winner).map(|(c, _)| c)
    }

    /// The chip, from the disk and the register, never from bookkeeping of
    /// what was meant: a friend's Hopper that is out of date first, then
    /// what waits for the player, then a mods folder that differs, then the
    /// group's changes not on disk yet, then this PC's own not acked yet.
    pub fn status(&self, disk: &Snapshot) -> Status {
        if let Some(id) = self.outdated.iter().next() {
            let name = self.members.get(id).map_or_else(|| id.clone(), |m| m.name.clone());
            return Status::OutOfDate(name);
        }
        let waiting = self.reviews().len();
        if waiting > 0 {
            return Status::NeedsYou(waiting);
        }
        let differs = self.drift(disk).len();
        if differs > 0 {
            return Status::Differs(differs);
        }
        let behind = self.lands().len();
        if behind > 0 {
            return Status::Behind(behind);
        }
        // A copy's friends are known from their statuses; until one arrives
        // nothing of ours has been acked.
        let sending = self
            .log
            .changes
            .keys()
            .filter(|d| d.author == self.me && (self.members.is_empty() || self.members.values().any(|m| !m.seen.covers(d))))
            .count();
        if sending > 0 {
            return Status::Sending(sending);
        }
        Status::InSync
    }

    pub fn members(&self) -> Vec<MemberStatus> {
        self.members.values().cloned().collect()
    }

    // ── what travels (the wire, P4, builds on these) ─────────────────

    /// The highest dot of each member this PC has taken in.
    pub fn seen(&self) -> Seen {
        let mut s = Seen::default();
        for d in self.log.changes.keys() {
            let n = s.0.entry(d.author.clone()).or_insert(0);
            *n = (*n).max(d.n);
        }
        s
    }

    /// This PC's status for a packet: what it has taken in.
    pub fn my_status(&self) -> MemberStatus {
        MemberStatus {
            member: self.me.clone(),
            name: self.name.clone(),
            seen: self.seen(),
            ..MemberStatus::default()
        }
    }

    /// This PC's own records `member` hasn't acked yet (everything, while
    /// their status is unknown): the cumulative tail of a packet to them.
    pub fn unacked_by(&self, member: &str) -> Vec<Change> {
        let acked = self.members.get(member).map(|s| s.seen.clone()).unwrap_or_default();
        self.log
            .changes
            .values()
            .map(|(c, _)| c)
            .filter(|c| c.dot.author == self.me && !acked.covers(&c.dot))
            .cloned()
            .collect()
    }

    /// The winning record (and open clash records) of every key: what a
    /// new member is sent first.
    pub fn checkpoint(&self) -> Vec<Change> {
        let mut out: BTreeMap<Dot, Change> = BTreeMap::new();
        for reg in self.registers().into_values() {
            for c in std::iter::once(reg.winner).chain(reg.open) {
                out.insert(c.dot.clone(), c);
            }
        }
        out.into_values().collect()
    }

    /// "While you were away: Mia made 5 changes, Noah 2." from the counts
    /// of what arrived (the app decides when a player was away).
    pub fn away_summary(by_author: &BTreeMap<String, usize>) -> Option<String> {
        let mut rows: Vec<(&String, &usize)> = by_author.iter().filter(|(_, n)| **n > 0).collect();
        rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let (first, rest) = rows.split_first()?;
        let mut s = format!("While you were away: {} made {}", first.0, plural(*first.1, "change", "changes"));
        for (name, n) in rest {
            s.push_str(&format!(", {name} {n}"));
        }
        s.push('.');
        Some(s)
    }

    // ── inside ───────────────────────────────────────────────────────

    fn next_n(&self) -> u64 {
        self.log
            .changes
            .keys()
            .filter(|d| d.author == self.me)
            .map(|d| d.n)
            .max()
            .unwrap_or(0)
            + 1
    }

    /// The registers of the records that count here: everything but what a
    /// member set to "Ignore" made (those stay in the log for acks).
    fn registers(&self) -> BTreeMap<Key, Register> {
        if self.trust.values().any(|t| *t == Trust::Ignore) {
            let mut log = Log::default();
            for (c, a) in self.log.changes.values() {
                if self.trust.get(&c.dot.author) != Some(&Trust::Ignore) {
                    log.insert(c.clone(), *a);
                }
            }
            fold(&log)
        } else {
            fold(&self.log)
        }
    }

    /// Registers, and the winners held for the player. A clash is not a hold:
    /// the register already keeps the non-destructive side, and only the
    /// answer closes the card. Records made here are never held.
    fn judged(&self) -> Judged {
        let regs = self.registers();
        let mut items = Vec::new();
        for reg in regs.values() {
            let c = &reg.winner;
            if c.dot.author == self.me || self.taken.contains(&c.dot) {
                continue;
            }
            let Some((_, Some(arrival))) = self.log.changes.get(&c.dot) else { continue };
            let mut seen_by_gate = c.clone();
            seen_by_gate.online_at = self.gate_online_at(c, arrival);
            items.push((seen_by_gate, *arrival));
        }
        let ctx = GateCtx {
            limits: self.limits,
            trust: &self.trust,
            registers: &regs,
            batch: &[],
            allowed_sources: &self.allowed_sources,
        };
        let mut held = BTreeMap::new();
        for h in gate::held(&items, &ctx) {
            let risks: Vec<Risk> = h.risks.into_iter().filter(|r| *r != Risk::Clash).collect();
            if !risks.is_empty() {
                held.insert(h.dot, risks);
            }
        }
        Judged { regs, held }
    }

    /// The "online at" the gate judges a record by, in relay time.
    ///
    /// - 0 means its author had never had contact (a new friend's first
    ///   sync, made before their first packet): not away, it is first contact
    ///   now, so it is judged as arriving the moment they last were in touch.
    /// - The record also counts as made after a long absence when it misses a
    ///   change that reached this PC 24 h (`away_secs`) before it did: the
    ///   gate only reads `online_at`, so the older arrival stands in for it.
    ///   A record that missed nothing, or that never had contact, keeps its
    ///   own value.
    fn gate_online_at(&self, c: &Change, arrival: &Arrival) -> u64 {
        if c.online_at == 0 {
            return arrival.at;
        }
        let mut at = c.online_at;
        for (r, ra) in self.log.changes.values() {
            let Some(ra) = ra else { continue };
            if r.dot.author == c.dot.author || c.seen.covers(&r.dot) || r.seen.covers(&c.dot) {
                continue;
            }
            if ra.at.saturating_add(self.limits.away_secs) < arrival.at {
                at = at.min(ra.at);
            }
        }
        at
    }

    /// Tells this PC's player once about each of their updates that lost to
    /// a concurrent one.
    fn tell_losers(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        for reg in self.registers().values() {
            for o in &reg.open {
                if o.dot.author == self.me
                    && merge(&reg.winner, o) == Merge::Newest
                    && self.told.insert(o.dot.clone())
                {
                    let w = &reg.winner.name;
                    notes.push(format!("{w} changed this too; {w}'s change is kept."));
                }
            }
        }
        notes
    }
}

/// The side of a clash that takes the mod away (or the other pack move).
fn clash_other(reg: &Register) -> &Change {
    reg.open
        .iter()
        .find(|o| merge(&reg.winner, o) == Merge::Clash)
        .unwrap_or(&reg.open[0])
}

fn clash_says(reg: &Register, other: &Change) -> String {
    let w = &reg.winner;
    if w.key == Key::Pack {
        return format!(
            "{} and {} moved the pack at the same time. The newer version stays until someone answers.",
            w.name, other.name
        );
    }
    let verb = if matches!(other.now, Value::File { on: false, .. } | Value::PackOff) {
        "turned off"
    } else {
        "removed"
    };
    format!(
        "{} {verb} a mod that {} changed at the same time. {}'s change stays until someone answers.",
        other.name, w.name, w.name
    )
}

fn card_says(risk: &Risk, who: &str, n: usize) -> String {
    match risk {
        Risk::LongAway { secs } => format!(
            "{who} was away for {}. {who} made {}.",
            span(*secs),
            plural(n, "change", "changes")
        ),
        Risk::ManyRemovals { count } => format!("{who} removed {count} mods at once."),
        Risk::PackBack => format!("{who} moved the pack back to an older version."),
        Risk::OtherPack => format!("{who} switched to another pack."),
        Risk::NewSource => format!("{who} added a mod from a link. Allow it before it installs."),
        Risk::AskFirst => format!(
            "{who} made {}. You asked to be asked first.",
            plural(n, "change", "changes")
        ),
        Risk::Clash => String::new(),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "3 days", or hours under a day.
fn span(secs: u64) -> String {
    let days = secs / 86_400;
    if days >= 1 {
        plural(days as usize, "day", "days")
    } else {
        plural((secs / 3600) as usize, "hour", "hours")
    }
}

/// Orders the reasons, and tells cards of the same reason apart from others.
fn risk_rank(r: &Risk) -> u8 {
    match r {
        Risk::Clash => 0,
        Risk::LongAway { .. } => 1,
        Risk::ManyRemovals { .. } => 2,
        Risk::PackBack => 3,
        Risk::OtherPack => 4,
        Risk::NewSource => 5,
        Risk::AskFirst => 6,
    }
}

/// A card's id: the same for the same card on any call (FNV-1a).
fn card_id(kind: &str, a: &str, b: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for part in [kind, a, b] {
        for byte in part.bytes().chain(std::iter::once(0xff)) {
            h ^= u64::from(byte);
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    h
}

/// The loader's kind: "neoforge" of "neoforge 21.1.77".
fn loader_kind(v: &Value) -> Option<&str> {
    match v {
        Value::Text(t) => t.split_whitespace().next(),
        _ => None,
    }
}

/// A bump inside the same kind is followed; another kind is not.
fn loader_follows(from: &Value, to: &Value) -> bool {
    match (loader_kind(from), loader_kind(to)) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => true,
    }
}

fn loader_label(v: &Value) -> String {
    match loader_kind(v).map(str::to_ascii_lowercase).as_deref() {
        Some("forge") => "Forge".into(),
        Some("neoforge") => "NeoForge".into(),
        Some("fabric") => "Fabric".into(),
        Some("quilt") => "Quilt".into(),
        Some(other) => {
            let mut c = other.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
        }
        None => "its loader".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = 3600;
    const T0: u64 = 1_000_000;

    fn file(n: u64) -> Value {
        Value::File { file: format!("cf:{n}"), on: true }
    }

    fn off(n: u64) -> Value {
        Value::File { file: format!("cf:{n}"), on: false }
    }

    fn mk(n: u64) -> Key {
        Key::Mod(format!("cf:{n}"))
    }

    /// One PC on mods 1 to 3 (files 11, 21, 31).
    fn pc(me: &str) -> Engine {
        let mut e = Engine::new("pack", me, me);
        for n in 1..=3 {
            e.applied.insert(mk(n), file(n * 10 + 1));
        }
        e
    }

    fn edit(key: Key, now: Value) -> Edit {
        Edit { key, now, why: Why::Player, batch: String::new() }
    }

    /// A change the player makes on this PC: recorded, and on its disk.
    fn act(e: &mut Engine, edit: Edit, online_at: u64) -> Dot {
        let (key, now) = (edit.key.clone(), edit.now.clone());
        let dot = e.record(edit, online_at);
        e.applied.insert(key, now);
        dot
    }

    /// `to` takes in `from`'s unacked records, as one event at relay time `at`.
    fn send(from: &Engine, to: &mut Engine, at: u64) -> Received {
        let changes = from.unacked_by(&to.me);
        to.ingest(&from.me, changes, from.my_status(), Arrival { seq: at, at })
    }

    /// Applies what lands, like a PC would.
    fn settle(e: &mut Engine) {
        for s in e.lands() {
            e.applied.insert(s.key, s.to);
        }
    }

    #[test]
    fn record_counts_orders_and_remembers_what_was_seen() {
        let mut a = pc("a");
        let mut b = pc("b");
        let d1 = a.record(edit(mk(1), file(12)), T0);
        let d2 = a.record(edit(mk(2), off(21)), T0);
        assert_eq!((d1.n, d2.n), (1, 2));
        send(&a, &mut b, T0);
        let d = b.record(edit(mk(3), file(32)), T0);
        let c = &b.log.changes[&d].0;
        assert_eq!(c.lamport, 3, "after the 2 records it took in");
        assert_eq!(c.seen.0["a"], 2);
        assert_eq!(c.was, file(31));
        assert_eq!(c.batch, "b:1");
    }

    #[test]
    fn a_plain_change_lands_and_is_named() {
        let (mut a, mut b) = (pc("a"), pc("b"));
        a.record(edit(mk(1), file(12)), T0);
        let got = send(&a, &mut b, T0);
        assert_eq!(got.new, 1);
        assert!(got.reviews.is_empty() && b.reviews().is_empty());
        let steps = b.lands();
        assert_eq!(steps.len(), 1);
        assert_eq!((steps[0].from.clone(), steps[0].to.clone()), (file(11), file(12)));
        settle(&mut b);
        assert!(b.lands().is_empty());
        assert_eq!(b.last_change(&mk(1)).unwrap().name, "a");
        assert_eq!(b.value(&mk(1)), file(12));
    }

    #[test]
    fn the_same_record_twice_adds_nothing() {
        let (mut a, mut b) = (pc("a"), pc("b"));
        a.record(edit(mk(1), file(12)), T0);
        send(&a, &mut b, T0);
        let again = send(&a, &mut b, T0 + 5);
        assert_eq!(again.new, 0);
        assert_eq!(b.log.changes.len(), 1);
        // The first arrival stays.
        assert_eq!(b.log.changes.values().next().unwrap().1.unwrap().at, T0);
    }

    #[test]
    fn update_against_removal_keeps_the_update_and_asks_everywhere() {
        let (mut a, mut b) = (pc("a"), pc("b"));
        act(&mut a, edit(mk(1), file(12)), T0);
        act(&mut b, edit(mk(1), Value::Gone), T0);
        send(&a, &mut b, T0);
        send(&b, &mut a, T0);
        for e in [&a, &b] {
            assert_eq!(e.value(&mk(1)), file(12));
            let r = e.reviews();
            assert_eq!(r.len(), 1);
            assert_eq!(r[0].risk, Risk::Clash);
            assert_eq!(r[0].keys, vec![mk(1)]);
        }
        // The removal never lands on the PC that updated.
        assert!(a.lands().is_empty());
        // The PC that removed gets the update back.
        assert_eq!(b.lands()[0].to, file(12));
    }

    #[test]
    fn the_first_answer_closes_the_clash_for_everyone() {
        let (mut a, mut b, mut c) = (pc("a"), pc("b"), pc("c"));
        a.record(edit(mk(1), file(12)), T0);
        b.record(edit(mk(1), Value::Gone), T0);
        send(&a, &mut c, T0);
        send(&b, &mut c, T0);
        send(&b, &mut a, T0);
        send(&a, &mut b, T0);
        settle(&mut c);
        let id = c.reviews()[0].id;
        let made = c.answer(id, Answer::Take, false);
        assert_eq!(made.len(), 1);
        send(&c, &mut a, T0 + 5);
        send(&c, &mut b, T0 + 5);
        for e in [&a, &b, &c] {
            assert_eq!(e.value(&mk(1)), Value::Gone);
            assert!(e.reviews().is_empty());
            assert_eq!(e.last_change(&mk(1)).unwrap().name, "c");
        }
    }

    #[test]
    fn keep_on_a_clash_restores_the_update() {
        let (mut a, mut c) = (pc("a"), pc("c"));
        let mut b = pc("b");
        a.record(edit(mk(1), file(12)), T0);
        b.record(edit(mk(1), Value::Gone), T0);
        send(&a, &mut c, T0);
        send(&b, &mut c, T0);
        let id = c.reviews()[0].id;
        c.answer(id, Answer::Keep, false);
        assert_eq!(c.value(&mk(1)), file(12));
        assert!(c.reviews().is_empty());
    }

    #[test]
    fn a_long_away_friends_changes_wait_until_taken() {
        let (mut a, mut b) = (pc("a"), pc("b"));
        // b's last contact was 3 days before its changes arrive.
        let one = |key, now| Edit { batch: "go".into(), ..edit(key, now) };
        act(&mut b, one(mk(2), off(21)), T0);
        act(&mut b, one(mk(3), file(32)), T0);
        send(&b, &mut a, T0 + 72 * H);
        let r = a.reviews();
        assert_eq!(r.len(), 1);
        assert!(matches!(r[0].risk, Risk::LongAway { secs } if secs == 72 * H));
        assert_eq!(r[0].says, "b was away for 3 days. b made 2 changes.");
        assert!(a.lands().is_empty());
        assert_eq!(a.value(&mk(2)), off(21), "the group agrees; only the apply waits");
        assert!(a.answer(r[0].id, Answer::Take, false).is_empty());
        assert_eq!(a.lands().len(), 2);
        assert!(a.reviews().is_empty());
    }

    #[test]
    fn keep_writes_follow_ups_that_put_it_back_for_everyone() {
        let (mut a, mut b) = (pc("a"), pc("b"));
        act(&mut b, edit(mk(2), off(21)), T0);
        send(&b, &mut a, T0 + 72 * H);
        let id = a.reviews()[0].id;
        let made = a.answer(id, Answer::Keep, false);
        assert_eq!(made.len(), 1);
        let kept = &a.log.changes[&made[0]].0;
        assert_eq!(kept.now, file(21));
        assert_eq!(kept.why, Why::Answer { review: id });
        assert_eq!(kept.name, "a");
        send(&a, &mut b, T0 + 72 * H);
        assert_eq!(b.lands()[0].to, file(21));
        assert!(a.reviews().is_empty());
    }

    #[test]
    fn a_first_ever_record_is_not_held_as_away() {
        let (mut a, mut d) = (pc("a"), pc("d"));
        // online_at 0: d never had contact.
        d.record(edit(mk(1), file(12)), 0);
        send(&d, &mut a, T0);
        assert!(a.reviews().is_empty());
        assert_eq!(a.lands().len(), 1);
    }

    #[test]
    fn missing_a_change_that_arrived_a_day_before_is_away_too() {
        let (mut a, mut b, mut c) = (pc("a"), pc("b"), pc("c"));
        a.record(edit(mk(1), file(12)), T0);
        send(&a, &mut c, T0);
        // b was in touch the whole time (online_at fresh) yet never took
        // a's change in; its own change reaches c 2 days later.
        b.record(edit(mk(2), file(22)), T0 + 48 * H);
        send(&b, &mut c, T0 + 48 * H);
        let r = c.reviews();
        assert_eq!(r.len(), 1);
        assert!(matches!(r[0].risk, Risk::LongAway { secs } if secs == 48 * H));
        // Had b taken a's change in, nothing would be held.
        let mut b2 = pc("b");
        send(&a, &mut b2, T0);
        b2.record(edit(mk(2), file(22)), T0 + 48 * H);
        let mut c2 = pc("c");
        send(&a, &mut c2, T0);
        send(&b2, &mut c2, T0 + 48 * H);
        assert!(c2.reviews().is_empty());
    }

    #[test]
    fn more_than_five_removals_in_a_batch_ask_once() {
        let (mut a, mut b) = (pc("a"), pc("b"));
        for n in 1..=8 {
            a.applied.insert(mk(n), file(n * 10 + 1));
            b.applied.insert(mk(n), file(n * 10 + 1));
        }
        for n in 1..=6 {
            let e = Edit { batch: "go".into(), ..edit(mk(n), Value::Gone) };
            a.record(e, T0);
        }
        send(&a, &mut b, T0);
        let r = b.reviews();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].risk, Risk::ManyRemovals { count: 6 });
        assert_eq!(r[0].changes.len(), 6);
        assert!(b.lands().is_empty());
        // "Do the same for the other N": a second batch, one tick.
        for n in 1..=6 {
            let e = Edit { batch: "go2".into(), ..edit(mk(n + 10), Value::Gone) };
            a.applied.insert(mk(n + 10), file(1));
            b.applied.insert(mk(n + 10), file(1));
            a.record(e, T0);
        }
        send(&a, &mut b, T0);
        assert_eq!(b.reviews().len(), 2);
        let id = b.reviews()[0].id;
        b.answer(id, Answer::Take, true);
        assert!(b.reviews().is_empty());
        assert_eq!(b.lands().len(), 12);
    }

    #[test]
    fn ask_first_holds_everything_from_that_friend() {
        let (mut a, mut b) = (pc("a"), pc("b"));
        a.set_trust("b", Trust::AskFirst);
        b.record(edit(mk(7), file(71)), T0);
        send(&b, &mut a, T0);
        assert_eq!(a.reviews()[0].risk, Risk::AskFirst);
        assert!(a.lands().is_empty());
        a.set_trust("b", Trust::Auto);
        assert_eq!(a.lands().len(), 1);
    }

    #[test]
    fn ignored_friends_never_land_but_are_acked() {
        let (mut a, mut b) = (pc("a"), pc("b"));
        a.set_trust("b", Trust::Ignore);
        b.record(edit(mk(1), file(12)), T0);
        send(&b, &mut a, T0);
        assert!(a.lands().is_empty());
        assert_eq!(a.value(&mk(1)), file(11));
        assert!(a.seen().covers(&Dot { author: "b".into(), n: 1 }));
    }

    #[test]
    fn a_link_source_waits_until_allowed() {
        let (mut a, mut b) = (pc("a"), pc("b"));
        let key = Key::Mod("src:cool.jar".into());
        a.record(edit(key.clone(), Value::File { file: "src:cool.jar".into(), on: true }), T0);
        send(&a, &mut b, T0);
        assert_eq!(b.reviews()[0].risk, Risk::NewSource);
        assert!(b.lands().is_empty());
        b.allow_source(&key);
        assert!(b.reviews().is_empty());
        assert_eq!(b.lands().len(), 1);
    }

    #[test]
    fn the_loser_of_a_concurrent_update_is_told_once() {
        let (mut a, mut b) = (pc("a"), pc("b"));
        a.record(edit(mk(1), file(12)), T0);
        b.record(edit(mk(1), file(13)), T0);
        let got = send(&b, &mut a, T0);
        // b wins (same lamport, higher author), so a is the loser.
        assert_eq!(got.notes, vec!["b changed this too; b's change is kept.".to_string()]);
        assert!(send(&b, &mut a, T0 + 1).notes.is_empty());
        assert!(send(&a, &mut b, T0).notes.is_empty());
        assert!(a.reviews().is_empty());
    }

    #[test]
    fn undo_member_puts_back_and_skips_what_changed_since() {
        let (mut a, mut c) = (pc("a"), pc("c"));
        c.record(Edit { batch: "x".into(), ..edit(mk(1), file(12)) }, T0);
        c.record(Edit { batch: "x".into(), ..edit(mk(2), off(21)) }, T0);
        send(&c, &mut a, T0);
        let mut b = pc("b");
        send(&c, &mut b, T0);
        b.record(edit(mk(2), file(23)), T0);
        send(&b, &mut a, T0);
        let (edits, left) = a.undo_member_plan("c", T0);
        assert_eq!(left, vec![mk(2)]);
        assert_eq!(edits.len(), 1);
        assert_eq!((edits[0].key.clone(), edits[0].now.clone()), (mk(1), file(11)));
        assert!(matches!(edits[0].why, Why::Undo { .. }));
        assert_eq!(a.undo_member("c", T0 + 10), vec![], "nothing since then");
    }

    #[test]
    fn a_loader_bump_follows_another_kind_is_told() {
        let mut e = Engine::new("pack", "b", "b");
        e.applied.insert(Key::Loader, Value::Text("neoforge 21.1.77".into()));
        let mut a = Engine::new("pack", "a", "a");
        a.applied = e.applied.clone();
        a.record(edit(Key::Loader, Value::Text("neoforge 21.1.80".into())), T0);
        send(&a, &mut e, T0);
        assert_eq!(e.lands().len(), 1);
        settle(&mut e);
        a.record(edit(Key::Loader, Value::Text("forge 52.0.1".into())), T0);
        send(&a, &mut e, T0);
        assert!(e.lands().is_empty());
        let told = e.not_followed();
        assert_eq!(told.len(), 1);
        assert!(told[0].1.contains("Forge") && told[0].1.contains("stays on NeoForge"));
    }

    #[test]
    fn away_summary_names_the_biggest_first() {
        let by: BTreeMap<String, usize> =
            [("C".to_string(), 2), ("A".to_string(), 5), ("D".to_string(), 0)].into();
        assert_eq!(
            Engine::away_summary(&by).as_deref(),
            Some("While you were away: A made 5 changes, C 2.")
        );
        let one: BTreeMap<String, usize> = [("A".to_string(), 1)].into();
        assert_eq!(Engine::away_summary(&one).as_deref(), Some("While you were away: A made 1 change."));
        assert_eq!(Engine::away_summary(&BTreeMap::new()), None);
    }

    #[test]
    fn tail_checkpoint_and_acks() {
        let (mut a, mut b) = (pc("a"), pc("b"));
        a.record(edit(mk(1), file(12)), T0);
        a.record(edit(mk(1), file(13)), T0);
        assert_eq!(a.unacked_by("b").len(), 2);
        send(&a, &mut b, T0);
        // b's status acks them.
        a.ingest("b", vec![], b.my_status(), Arrival { seq: 2, at: T0 });
        assert!(a.unacked_by("b").is_empty());
        // The checkpoint is the winner only.
        assert_eq!(b.checkpoint().len(), 1);
        assert_eq!(b.checkpoint()[0].now, file(13));
    }

    #[test]
    fn a_packet_of_another_version_is_refused() {
        let mut a = pc("a");
        let p = Packet { version: WIRE_VERSION + 1, from: "b".into(), group: "g".into(), copies: BTreeMap::new() };
        assert!(a.receive(p, Arrival::default()).refused);
    }

    fn snap(items: &[(u64, Value)]) -> Snapshot {
        Snapshot { values: items.iter().map(|(n, v)| (mk(*n), v.clone())).collect(), unreadable: false }
    }

    /// The disk of a PC after `settle`: its applied values.
    fn disk_of(e: &Engine) -> Snapshot {
        Snapshot { values: e.applied.clone(), unreadable: false }
    }

    #[test]
    fn drift_is_the_disk_against_what_was_applied() {
        let a = pc("a");
        assert!(a.drift(&disk_of(&a)).is_empty());
        let d = a.drift(&snap(&[(1, file(11)), (3, file(32))]));
        let keys: Vec<Key> = d.iter().map(|d| d.key.clone()).collect();
        assert_eq!(keys, [mk(2), mk(3)]);
    }

    #[test]
    fn status_says_in_sync_only_when_the_disk_agrees() {
        let mut a = pc("a");
        assert_eq!(a.status(&disk_of(&a)), Status::InSync);
        // The disk differs: the chip says so, whatever was meant.
        assert_eq!(a.status(&snap(&[(1, file(11)), (3, file(31))])), Status::Differs(1));
        // An unreadable instance is not "differs" (and not drift).
        let unreadable = Snapshot { values: BTreeMap::new(), unreadable: true };
        assert_eq!(a.status(&unreadable), Status::InSync);
        // Its own change not acked yet.
        act(&mut a, edit(mk(1), file(12)), T0);
        assert_eq!(a.status(&disk_of(&a)), Status::Sending(1));
        // A friend's status acks it.
        let mut b = pc("b");
        send(&a, &mut b, T0);
        a.ingest("b", vec![], b.my_status(), Arrival { seq: 2, at: T0 });
        assert_eq!(a.status(&disk_of(&a)), Status::InSync);
    }

    #[test]
    fn status_puts_what_waits_for_you_first_then_behind() {
        let (mut a, mut b) = (pc("a"), pc("b"));
        b.record(edit(mk(1), file(12)), T0);
        send(&b, &mut a, T0);
        // A plain change from a friend that isn't on disk yet.
        assert_eq!(a.status(&disk_of(&a)), Status::Behind(1));
        settle(&mut a);
        assert_eq!(a.status(&disk_of(&a)), Status::InSync);
        // A removal while away waits for the player.
        b.record(edit(mk(2), Value::Gone), T0);
        send(&b, &mut a, T0 + 72 * H);
        assert_eq!(a.status(&disk_of(&a)), Status::NeedsYou(1));
    }

    #[test]
    fn a_refused_packet_shows_who_is_out_of_date_until_they_send_again() {
        let mut a = pc("a");
        let mut b = pc("b");
        b.record(edit(mk(1), file(12)), T0);
        a.ingest("b", vec![], b.my_status(), Arrival { seq: 1, at: T0 });
        let old = Packet { version: WIRE_VERSION + 1, from: "b".into(), group: "g".into(), copies: BTreeMap::new() };
        assert!(a.receive(old, Arrival::default()).refused);
        assert_eq!(a.status(&disk_of(&a)), Status::OutOfDate("b".into()));
        a.ingest("b", vec![], b.my_status(), Arrival { seq: 2, at: T0 });
        assert_eq!(a.status(&disk_of(&a)), Status::InSync);
    }

    #[test]
    fn missing_lists_what_the_group_has_and_this_disk_lacks() {
        let (mut a, mut b) = (pc("a"), pc("b"));
        // Gone from this folder outside Hopper.
        assert_eq!(a.missing(&snap(&[(1, file(11)), (3, file(31))])), [mk(2)]);
        // A friend's new mod that isn't on this disk yet.
        b.record(edit(mk(7), file(71)), T0);
        send(&b, &mut a, T0);
        let disk = disk_of(&a);
        assert_eq!(a.missing(&disk), [mk(7)]);
        settle(&mut a);
        assert!(a.missing(&disk_of(&a)).is_empty());
        let unreadable = Snapshot { values: BTreeMap::new(), unreadable: true };
        assert!(a.missing(&unreadable).is_empty());
    }

    #[test]
    fn reviews_have_stable_ids() {
        let (mut a, mut b) = (pc("a"), pc("b"));
        b.record(edit(mk(2), off(21)), T0);
        send(&b, &mut a, T0 + 72 * H);
        let first = a.reviews()[0].id;
        a.record(edit(mk(3), file(32)), T0 + 72 * H);
        assert_eq!(a.reviews()[0].id, first);
    }
}
