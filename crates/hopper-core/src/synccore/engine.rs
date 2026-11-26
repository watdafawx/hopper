//! The one object the apps use for a group copy (design §11). Pure: every
//! method is a function of the engine's state and its arguments.
//!
//! Holds the group's log of records, folds it with `register`, runs the
//! gate on what is about to land, and keeps the player's answers. Drift
//! and status (P5), plan/apply (P6) and the wire outbox (P4) are other
//! phases; their methods stay `todo!()` here.

use super::apply::{ApplyReport, Outcome};
use super::digest::{self, StateEntry};
use super::gate::{self, GateCtx, Limits, Risk, Trust};
use super::local::{self, Drift, Snapshot};
use super::model::{Arrival, Change, Dot, Edit, Key, MemberId, PackMode, Seen, Value, Why};
use super::plan::{self, ApplyPlan, PackPlan, Step, Wait};
use super::register::{self, fold, merge, Log, Merge, Register};
use super::wire::{self, Body, MemberStatus, Packet, WIRE_VERSION};
use serde::{Deserialize, Serialize};
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
    /// Set for [`Risk::Differs`]: the mods this PC and a friend have
    /// differently. No record is held for such a card (`changes` is empty).
    pub differs: Option<Differs>,
}

/// What a "Differs" card is about: one friend and the mods that differ.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Differs {
    pub member: MemberId,
    pub rows: Vec<digest::Row>,
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
    /// This PC's game is running (jar changes wait; the app sets it).
    pub game_running: bool,
    /// Config files changed on this PC, for the pack plan (never shared).
    pub edited_configs: Vec<String>,
    /// Keys whose last apply failed or was rate limited: not tried again
    /// until their time (relay seconds). Per key, so one never holds others.
    pub retry: BTreeMap<Key, Retry>,
    /// Keys the author blocked downloads of, with the record that was
    /// blocked: by hand, never retried until a newer record.
    pub blocked: BTreeMap<Key, Dot>,
    /// What was posted to whom (the wire outbox, `outbox` / `sent`).
    pub posting: Posting,
    /// Mods this PC keeps only here ("Only on this PC"): never compared
    /// with a friend's. Set by the app on each tick, so not saved.
    pub kept_here: BTreeSet<Key>,
    /// Each friend's list of mods, with the state hash it was sent for. Kept
    /// apart from `members` so a status without the list doesn't drop it.
    pub states: BTreeMap<MemberId, (String, Vec<StateEntry>)>,
}

/// What an event to one member said, to post again only on a difference.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Posted {
    pub dots: Vec<Dot>,
    pub seen: Seen,
    pub missing: Vec<Key>,
    pub needs_full: bool,
    pub needs_state: bool,
    /// The state hash of the list sent (empty: none went).
    pub listed: String,
    /// Copies besides the engine's own.
    pub extra_copies: usize,
}

/// The outbox's memory, per member.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Posting {
    /// What was last posted to each member.
    last: BTreeMap<MemberId, Posted>,
    /// The register hash each sender's last packet carried.
    their_hash: BTreeMap<MemberId, String>,
    /// Members already sent the full checkpoint they asked for.
    checkpointed: BTreeSet<MemberId>,
    /// Members that still need a checkpoint from this PC.
    new_members: BTreeSet<MemberId>,
    /// Events built (every 50th carries a checkpoint).
    pub publishes: u64,
    /// The status last posted, so a status-only change is posted too.
    status: Option<MemberStatus>,
}

/// One event to post: the body and status for `to`, to be wrapped in a
/// packet (with one empty entry per more copy) and handed to the relay.
#[derive(Clone, Debug)]
pub struct Outgoing {
    pub to: MemberId,
    pub body: Body,
    pub status: MemberStatus,
    /// The register hash the packet carries.
    pub hash: String,
    posted: Posted,
    ask_now: bool,
}

/// When a failed step is tried again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Retry {
    /// The record that failed (a newer one starts fresh).
    pub by: Dot,
    pub until: u64,
    pub tries: u32,
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
        // An undo from History names the record it undoes: the key's last.
        let why = match edit.why {
            w if w == Why::undo_of_last() => self.last_change(&edit.key).map_or(Why::Player, |c| Why::Undo { of: c.dot.clone() }),
            w => w,
        };
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
            why,
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
    /// The register hash of everything this PC holds.
    pub fn hash(&self) -> String {
        register::hash(&fold(&self.log))
    }

    /// Whether this PC, level with `from` by `seen`, holds a different
    /// register than that sender's last packet carried (damaged state).
    fn needs_full_from(&self, from: &str) -> bool {
        let (Some(theirs), Some(m)) = (self.posting.their_hash.get(from), self.members.get(from)) else {
            return false;
        };
        m.seen == self.seen() && self.reviews().is_empty() && wire::hash_mismatch(&self.hash(), theirs)
    }

    /// This PC's status for a packet to `to`: what it has taken in, what its
    /// disk lacks and whether it needs `to` to send everything.
    pub fn status_for(&self, to: &str, disk: &Snapshot) -> MemberStatus {
        let mut status = self.my_status();
        status.missing = self.missing(disk);
        status.needs_full = self.needs_full_from(to);
        status.needs_state = self.asks_state(to);
        // The list of mods goes only to a friend who asked for it.
        if self.members.get(to).is_some_and(|m| m.needs_state) {
            status.state = digest::entries(&self.applied, &self.kept_here);
        }
        status
    }

    /// Nothing waits to land on this PC's disk: the register's values are
    /// applied (a step in a download, a back-off, a running game or a held
    /// record all count as waiting). Mods kept only here don't count.
    pub fn settled(&self) -> bool {
        let j = self.judged();
        let holding = j.regs.iter().any(|(key, reg)| {
            j.held.contains_key(&reg.winner.dot)
                && !self.kept_here.contains(key)
                && self.applied.get(key).cloned().unwrap_or_default() != reg.winner.now
        });
        !holding && self.lands().iter().all(|s| self.kept_here.contains(&s.key))
    }

    /// The pack version this PC has applied.
    fn pack_text(&self) -> String {
        match self.applied.get(&Key::Pack) {
            Some(Value::Text(t)) => t.clone(),
            _ => String::new(),
        }
    }

    /// Whether `m`'s mods may be compared with this PC's: the same records
    /// (equal register hashes, so nothing of anyone's is in flight), both
    /// landed everything (so no download, back-off, running game or held
    /// record explains it), the same pack version, a Hopper that sends the
    /// digest, a friend whose changes count, and a different state hash.
    fn comparable(&self, m: &MemberStatus, mine: &str, hash: &str, settled: bool) -> bool {
        m.member != self.me
            && !m.state_hash.is_empty()
            && !m.reg_hash.is_empty()
            && !self.outdated.contains(&m.member)
            && self.trust.get(&m.member) != Some(&Trust::Ignore)
            && m.settled
            && settled
            && m.reg_hash == hash
            && m.pack == self.pack_text()
            && m.state_hash != mine
    }

    /// Whether this PC needs `member`'s list of mods: they are comparable
    /// and this PC doesn't hold the list for their current hash yet.
    fn asks_state(&self, member: &str) -> bool {
        let Some(m) = self.members.get(member) else { return false };
        let mine = digest::hash(&digest::entries(&self.applied, &self.kept_here));
        self.comparable(m, &mine, &self.hash(), self.settled())
            && self.states.get(member).is_none_or(|(h, _)| *h != m.state_hash)
    }

    /// The "Differs" cards: one per friend whose mods differ from this
    /// PC's with nothing in flight and nothing unlanded on either side. A
    /// mod any record is about is never in them: its value is the group's.
    fn differ_reviews(&self) -> Vec<Review> {
        if self.members.is_empty() {
            return Vec::new();
        }
        let mine = digest::entries(&self.applied, &self.kept_here);
        let (mine_hash, hash, settled) = (digest::hash(&mine), self.hash(), self.settled());
        let explained: BTreeSet<Key> = self.log.changes.values().map(|(c, _)| c.key.clone()).collect();
        let mut out = Vec::new();
        for (id, m) in &self.members {
            if !self.comparable(m, &mine_hash, &hash, settled) {
                continue;
            }
            let Some((h, list)) = self.states.get(id) else { continue };
            if *h != m.state_hash {
                continue;
            }
            let rows = digest::rows(&mine, list, &explained);
            if rows.is_empty() {
                continue;
            }
            let who = if m.name.trim().is_empty() { "A friend".to_string() } else { m.name.clone() };
            out.push(Review {
                id: card_id("differs", id, ""),
                risk: Risk::Differs,
                changes: Vec::new(),
                keys: rows.iter().map(|r| r.key.clone()).collect(),
                says: format!("{who}'s mods and this PC's differ on {}.", plural(rows.len(), "mod", "mods")),
                who,
                differs: Some(Differs { member: id.clone(), rows }),
            });
        }
        out
    }

    /// A person joined the group: every packet to them carries a checkpoint
    /// until they have taken it in.
    pub fn new_member(&mut self, member: &str) {
        self.posting.new_members.insert(member.to_string());
    }

    /// What to post to each of `to` now (the real app and the sim both
    /// build their packets from this): what they haven't acked, a
    /// checkpoint to a new member, to one that asked, or every 50th time,
    /// and a repost when only the status changed. One event per member,
    /// however many copies of the group this PC holds (`extra_copies`).
    /// Nothing is due when it all matches what was last posted.
    pub fn outbox(&mut self, to: &[String], disk: &Snapshot, extra_copies: usize) -> Vec<Outgoing> {
        let own_status = self.my_status();
        let status_changed = self.posting.status.as_ref() != Some(&own_status);
        let hash = self.hash();
        let mut out = Vec::new();
        for r in to.iter().filter(|r| **r != self.me) {
            if let Some(acked) = self.members.get(r).map(|m| m.seen.clone()) {
                if self.posting.new_members.contains(r) && self.checkpoint().iter().all(|c| acked.covers(&c.dot)) {
                    self.posting.new_members.remove(r);
                }
            }
            // A member whose hash differed asked for everything, once.
            let asked = self.members.get(r).is_some_and(|m| m.needs_full);
            if !asked {
                self.posting.checkpointed.remove(r);
            }
            let ask_now = asked && !self.posting.checkpointed.contains(r);
            let new_member = self.posting.new_members.contains(r);
            let mut changes = self.unacked_by(r);
            if new_member || ask_now {
                for c in self.checkpoint() {
                    if !changes.iter().any(|x| x.dot == c.dot) {
                        changes.push(c);
                    }
                }
            }
            let status = self.status_for(r, disk);
            let posted = Posted {
                dots: changes.iter().map(|c| c.dot.clone()).collect(),
                seen: status.seen.clone(),
                missing: status.missing.clone(),
                needs_full: status.needs_full,
                needs_state: status.needs_state,
                listed: if status.state.is_empty() { String::new() } else { status.state_hash.clone() },
                extra_copies,
            };
            let last = self.posting.last.get(r).cloned().unwrap_or_default();
            let new_records = !posted.dots.is_empty() && posted.dots != last.dots;
            let same_news = posted.seen == last.seen
                && posted.missing == last.missing
                && posted.needs_full == last.needs_full
                && posted.needs_state == last.needs_state
                && posted.listed == last.listed
                && posted.extra_copies == last.extra_copies;
            if !new_records && !ask_now && same_news && !status_changed {
                continue;
            }
            self.posting.publishes += 1;
            let periodic = wire::send_checkpoint(false, false, self.posting.publishes);
            if periodic {
                for c in self.checkpoint() {
                    if !changes.iter().any(|x| x.dot == c.dot) {
                        changes.push(c);
                    }
                }
            }
            let full = new_member || ask_now || periodic;
            let body = if full { Body::Checkpoint(changes) } else { Body::Tail(changes) };
            out.push(Outgoing { to: r.clone(), body, status, hash: hash.clone(), posted, ask_now });
        }
        self.posting.status = Some(own_status);
        out
    }

    /// The relay took `event`: it is what the member was last sent, so the
    /// same thing is not posted again.
    pub fn sent(&mut self, event: &Outgoing) {
        self.posting.last.insert(event.to.clone(), event.posted.clone());
        if event.ask_now {
            self.posting.checkpointed.insert(event.to.clone());
        }
    }

    // ── friends ──────────────────────────────────────────────────────

    /// A packet from a friend. Refuses another wire version.
    pub fn receive(&mut self, packet: Packet, arrival: Arrival) -> Received {
        let mut packet = packet;
        if packet.version != WIRE_VERSION {
            self.outdated.insert(packet.from);
            return Received { refused: true, ..Received::default() };
        }
        let Some((body, status, hash)) = packet.copies.remove(&self.copy) else {
            return Received::default();
        };
        self.posting.their_hash.insert(packet.from.clone(), hash);
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
        let mut status = status;
        // Their list of mods is kept for the hash it came with; the status
        // that is saved and compared stays small.
        let list = std::mem::take(&mut status.state);
        if !list.is_empty() && !status.state_hash.is_empty() {
            self.states.insert(from.to_string(), (status.state_hash.clone(), list));
        }
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
                differs: None,
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
                differs: None,
            });
        }
        out.extend(self.differ_reviews());
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
            if let Some(d) = &c.differs {
                // Each differing mod becomes this player's record: the
                // friend's value (applied here by the normal plan) or this
                // PC's own (everyone else follows it).
                for row in &d.rows {
                    let now = match answer {
                        Answer::Take => row.theirs.clone(),
                        Answer::Keep => row.mine.clone(),
                    };
                    edits.push((row.key.clone(), now, c.id));
                }
            } else if c.risk == Risk::Clash {
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
                why: w.why.clone(),
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

    /// What to do on disk now: `lands()` minus what waits (the game, a
    /// review, a source, a back-off, a download only the player can make).
    pub fn plan(&self) -> ApplyPlan {
        let reviews = self.reviews();
        let j = self.judged();
        let mut waits: Vec<(Key, Wait)> = Vec::new();
        // Held winners that would change this PC: each says what it waits for.
        for (key, reg) in &j.regs {
            let w = &reg.winner;
            let Some(risks) = j.held.get(&w.dot) else { continue };
            if self.applied.get(key).cloned().unwrap_or_default() == w.now {
                continue;
            }
            let wait = if risks.contains(&Risk::NewSource) {
                Wait::Source
            } else {
                let id = reviews.iter().find(|r| r.changes.contains(&w.dot)).map_or(0, |r| r.id);
                Wait::Review(id)
            };
            waits.push((key.clone(), wait));
        }
        let mut steps = Vec::new();
        for step in self.lands() {
            if self.blocked.get(&step.key) == Some(&step.by) {
                waits.push((step.key, Wait::ByHand));
            } else if let Some(r) = self.retry.get(&step.key).filter(|r| r.by == step.by && self.contact < r.until) {
                waits.push((step.key, Wait::Retry { after_secs: r.until - self.contact }));
            } else if self.game_running && !matches!(step.key, Key::Lock(_)) {
                waits.push((step.key, Wait::GameRunning));
            } else {
                steps.push(step);
            }
        }
        waits.sort_by(|a, b| a.0.cmp(&b.0));
        ApplyPlan { steps, waits }
    }

    /// Takes in what `apply::run` did: `applied` moves only for steps that
    /// are done, to the value of the record they were for. Nothing here
    /// makes a record, so a halfway failure never re-publishes what landed
    /// under this player's name.
    pub fn applied(&mut self, report: &ApplyReport) {
        for r in &report.results {
            match &r.outcome {
                Outcome::Done => {
                    if let Some((c, _)) = self.log.changes.get(&r.by) {
                        self.applied.insert(r.key.clone(), c.now.clone());
                    }
                    self.retry.remove(&r.key);
                    self.blocked.remove(&r.key);
                }
                Outcome::Blocked => {
                    self.retry.remove(&r.key);
                    self.blocked.insert(r.key.clone(), r.by.clone());
                }
                Outcome::Retry { after_secs } => self.back_off(r, *after_secs),
                Outcome::Failed(_) => {
                    let tries = self.retry.get(&r.key).filter(|x| x.by == r.by).map_or(0, |x| x.tries);
                    // 1 min, 2, 4 ... never more than an hour.
                    let after = (60u64 << tries.min(6)).min(3600);
                    self.back_off(r, after)
                }
            }
        }
    }

    fn back_off(&mut self, r: &super::apply::StepResult, after_secs: u64) {
        let tries = self.retry.get(&r.key).filter(|x| x.by == r.by).map_or(0, |x| x.tries) + 1;
        let retry = Retry { by: r.by.clone(), until: self.contact + after_secs, tries };
        self.retry.insert(r.key.clone(), retry);
    }

    /// What a pack update to `version` in `mode` does for this PC and each
    /// friend, from their last status, before the player presses Update.
    pub fn pack_plan(&self, version: &str, mode: PackMode) -> PackPlan {
        let from = match self.applied.get(&Key::Pack) {
            Some(Value::Text(t)) => t.clone(),
            _ => String::new(),
        };
        let members: Vec<MemberStatus> =
            self.members.values().filter(|m| m.member != self.me).cloned().collect();
        plan::pack_plan(&from, version, &mode, &members)
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
        self.status_with(disk, self.reviews().len())
    }

    /// [`Engine::status`] with `waiting` cards counted: the screens pass how
    /// many the player hasn't put aside ("Not now"), so a card put aside no
    /// longer holds the chip at "Needs you".
    pub fn status_with(&self, disk: &Snapshot, waiting: usize) -> Status {
        self.status_counting(disk, waiting, |_| true)
    }

    /// [`Engine::status_with`] where only the members `counts` says yes to
    /// can make the status "out of date": a friend whose older Hopper sends
    /// another format is that friend's to fix, so this PC's chip skips them.
    pub fn status_counting(&self, disk: &Snapshot, waiting: usize, counts: impl Fn(&str) -> bool) -> Status {
        if let Some(id) = self.outdated.iter().find(|m| counts(m)) {
            let name = self.members.get(id).map_or_else(|| id.clone(), |m| m.name.clone());
            return Status::OutOfDate(name);
        }
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

    /// Whether `member` has taken in every change this PC made, as far as
    /// their last status says. A member never heard from, or whose Hopper is
    /// older (its packets are refused), has not.
    pub fn acked_by(&self, member: &str) -> bool {
        !self.outdated.contains(member)
            && self.members.get(member).is_some_and(|m| self.log.changes.keys().filter(|d| d.author == self.me).all(|d| m.seen.covers(d)))
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
            pack: match self.applied.get(&Key::Pack) {
                Some(Value::Text(t)) => t.clone(),
                _ => String::new(),
            },
            edited_configs: self.edited_configs.clone(),
            off_pack_mods: self
                .applied
                .iter()
                .filter(|(_, v)| **v == Value::PackOff)
                .map(|(k, _)| k.clone())
                .collect(),
            game_running: self.game_running,
            state_hash: digest::hash(&digest::entries(&self.applied, &self.kept_here)),
            reg_hash: self.hash(),
            settled: self.settled(),
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
        // A link mod this PC already has a file of comes from a source that is
        // on this PC already (the key names the source): a friend's update or
        // undo of it is not a new source to allow.
        let mut allowed = self.allowed_sources.clone();
        for (k, v) in &self.applied {
            if let (Key::Mod(m), Value::File { .. }) = (k, v)
                && gate::is_link_source(m)
            {
                allowed.insert(m.clone());
            }
        }
        let ctx = GateCtx {
            limits: self.limits,
            trust: &self.trust,
            registers: &regs,
            batch: &[],
            allowed_sources: &allowed,
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
        Risk::Clash | Risk::Differs => String::new(),
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
        Risk::Differs => 7,
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
    use super::super::model::ConfigMode;
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
    fn a_history_undo_names_the_record_it_undoes() {
        let (mut a, mut b) = (pc("a"), pc("b"));
        let up = act(&mut a, edit(mk(1), file(12)), T0);
        send(&a, &mut b, T0);
        settle(&mut b);
        // B undoes A's update from History: the save only knows "undo".
        b.record(Edit { why: Why::undo_of_last(), ..edit(mk(1), file(11)) }, T0);
        let last = b.last_change(&mk(1)).unwrap();
        assert_eq!(last.why, Why::Undo { of: up });
        assert_eq!((last.was.clone(), last.now.clone()), (file(12), file(11)));
        // A takes it like any change: no card, it lands.
        send(&b, &mut a, T0 + 1);
        assert!(a.reviews().is_empty());
        assert_eq!(a.lands().len(), 1);
        assert_eq!(a.value(&mk(1)), file(11));
        // An undo of something no record names is the player's own change.
        b.record(Edit { why: Why::undo_of_last(), ..edit(mk(5), file(52)) }, T0);
        assert_eq!(b.last_change(&mk(5)).unwrap().why, Why::Player);
    }

    #[test]
    fn a_link_mod_this_pc_has_needs_no_allowing_for_its_next_file() {
        let (mut a, mut b) = (pc("a"), pc("b"));
        let key = Key::Mod("src:github:fox/tweaks".into());
        let v = |f: &str| Value::File { file: f.into(), on: true };
        // A added the mod; B allowed it and took it.
        act(&mut a, edit(key.clone(), v("link:143")), T0);
        send(&a, &mut b, T0);
        b.allow_source(&key);
        settle(&mut b);
        // B goes back to the older file; A takes it without a card.
        b.record(Edit { why: Why::undo_of_last(), ..edit(key.clone(), v("link:142")) }, T0);
        send(&b, &mut a, T0 + 1);
        assert!(a.reviews().is_empty(), "{:?}", a.reviews());
        assert_eq!(a.lands().len(), 1);
        // A third PC that never allowed the link is still asked.
        let mut c = pc("c");
        send(&b, &mut c, T0 + 1);
        assert!(c.reviews().iter().any(|r| r.risk == Risk::NewSource));
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
        Snapshot { values: items.iter().map(|(n, v)| (mk(*n), v.clone())).collect(), unreadable: false, ..Default::default() }
    }

    /// The disk of a PC after `settle`: its applied values.
    fn disk_of(e: &Engine) -> Snapshot {
        Snapshot { values: e.applied.clone(), unreadable: false, ..Default::default() }
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
        let unreadable = Snapshot { values: BTreeMap::new(), unreadable: true, ..Default::default() };
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
    fn acked_by_means_a_current_friend_took_every_change_in() {
        let mut a = pc("a");
        act(&mut a, edit(mk(1), file(12)), T0);
        // Never heard from: not acked. A friend whose Hopper is older: never.
        assert!(!a.acked_by("b"));
        a.note_out_of_date("c");
        assert!(!a.acked_by("c"));
        let mut b = pc("b");
        send(&a, &mut b, T0);
        a.ingest("b", vec![], b.my_status(), Arrival { seq: 2, at: T0 });
        assert!(a.acked_by("b"));
        // A newer change of this PC's isn't taken in yet.
        act(&mut a, edit(mk(1), file(13)), T0);
        assert!(!a.acked_by("b"));
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
        let unreadable = Snapshot { values: BTreeMap::new(), unreadable: true, ..Default::default() };
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

    /// A and B in step; A updates mod 1; B has it as a plan step.
    fn pair() -> (Engine, Engine, Dot) {
        let (mut a, mut b) = (pc("A"), pc("B"));
        let d = act(&mut a, edit(mk(1), file(12)), T0);
        send(&a, &mut b, T0);
        (a, b, d)
    }

    fn done(key: Key, by: &Dot) -> super::super::apply::StepResult {
        super::super::apply::StepResult { key, by: by.clone(), outcome: Outcome::Done }
    }

    #[test]
    fn a_plan_is_the_register_minus_what_waits() {
        let (_a, mut b, d) = pair();
        let p = b.plan();
        assert_eq!(p.steps.len(), 1);
        assert_eq!((p.steps[0].key.clone(), p.steps[0].by.clone()), (mk(1), d));
        assert!(p.waits.is_empty());
        b.game_running = true;
        let p = b.plan();
        assert!(p.steps.is_empty());
        assert_eq!(p.waits, vec![(mk(1), Wait::GameRunning)]);
    }

    #[test]
    fn applied_moves_only_for_done_steps_and_makes_no_record() {
        let (_a, mut b, d) = pair();
        let before = b.log.changes.len();
        let other = Dot { author: "A".into(), n: 99 };
        let report = ApplyReport {
            results: vec![
                done(mk(1), &d),
                super::super::apply::StepResult { key: mk(2), by: other, outcome: Outcome::Failed("x".into()) },
            ],
        };
        b.applied(&report);
        assert_eq!(b.applied[&mk(1)], file(12));
        assert_eq!(b.applied[&mk(2)], file(21));
        assert_eq!(b.log.changes.len(), before, "nothing re-published as this PC's");
        assert!(b.plan().steps.is_empty());
    }

    #[test]
    fn a_failed_step_backs_off_per_key_and_comes_back() {
        let (_a, mut b, d) = pair();
        b.touch(T0);
        let fail = |o| ApplyReport {
            results: vec![super::super::apply::StepResult { key: mk(1), by: d.clone(), outcome: o }],
        };
        b.applied(&fail(Outcome::Failed("reset".into())));
        let p = b.plan();
        assert!(p.steps.is_empty());
        assert_eq!(p.waits, vec![(mk(1), Wait::Retry { after_secs: 60 })]);
        b.touch(T0 + 60);
        assert_eq!(b.plan().steps.len(), 1, "tried again after its time");
        b.applied(&fail(Outcome::Failed("reset".into())));
        assert_eq!(b.plan().waits, vec![(mk(1), Wait::Retry { after_secs: 120 })], "longer each time");
        b.applied(&fail(Outcome::Retry { after_secs: 300 }));
        assert_eq!(b.plan().waits[0].1, Wait::Retry { after_secs: 300 });
    }

    #[test]
    fn a_blocked_download_waits_by_hand_until_a_newer_record() {
        let (mut a, mut b, d) = pair();
        b.applied(&ApplyReport {
            results: vec![super::super::apply::StepResult { key: mk(1), by: d, outcome: Outcome::Blocked }],
        });
        assert_eq!(b.plan().waits, vec![(mk(1), Wait::ByHand)]);
        b.touch(T0 + 9 * H);
        assert_eq!(b.plan().waits, vec![(mk(1), Wait::ByHand)], "never retried by a timer");
        act(&mut a, edit(mk(1), file(13)), T0 + 10);
        send(&a, &mut b, T0 + 20);
        let p = b.plan();
        assert_eq!(p.steps.len(), 1, "a new record starts fresh");
        assert!(p.waits.is_empty());
    }

    #[test]
    fn a_held_record_is_a_wait_with_its_card() {
        let (mut a, mut b) = (pc("A"), pc("B"));
        b.set_trust("A", Trust::AskFirst);
        act(&mut a, edit(mk(1), file(12)), T0);
        send(&a, &mut b, T0);
        let p = b.plan();
        assert!(p.steps.is_empty());
        let card = b.reviews()[0].id;
        assert_eq!(p.waits, vec![(mk(1), Wait::Review(card))]);
    }

    #[test]
    fn the_pack_plan_reads_the_members_last_status() {
        let (a, mut b, _d) = pair();
        let mut status = a.my_status();
        status.edited_configs = vec!["a.toml".into(), "b.toml".into()];
        status.game_running = true;
        b.members.insert("A".into(), status);
        let mode = PackMode { here: ConfigMode::Keep, friends: ConfigMode::Keep, keep_off: true };
        let p = b.pack_plan("cf:100/8700", mode);
        assert_eq!(p.friends.len(), 1);
        assert_eq!(p.friends[0].kept_files, 2);
        assert!(p.friends[0].waits_for_game);
    }

    #[test]
    fn my_status_carries_what_the_plan_shows() {
        let mut e = pc("A");
        e.applied.insert(Key::Pack, Value::Text("cf:100/8600".into()));
        e.applied.insert(mk(4), Value::PackOff);
        e.edited_configs = vec!["c.toml".into()];
        e.game_running = true;
        let s = e.my_status();
        assert_eq!(s.pack, "cf:100/8600");
        assert_eq!(s.off_pack_mods, vec![mk(4)]);
        assert_eq!(s.edited_configs, vec!["c.toml".to_string()]);
        assert!(s.game_running);
    }

    fn disk(e: &Engine) -> Snapshot {
        Snapshot { values: e.applied.clone(), unreadable: false, ..Default::default() }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// Builds the outbox and marks every event as taken by the relay.
    fn post(e: &mut Engine, to: &[&str]) -> Vec<Outgoing> {
        let snap = disk(e);
        let out = e.outbox(&names(to), &snap, 0);
        for o in &out {
            e.sent(o);
        }
        out
    }

    #[test]
    fn the_outbox_has_one_event_per_recipient_and_not_for_me() {
        let mut a = pc("A");
        act(&mut a, edit(mk(1), file(12)), T0);
        let out = post(&mut a, &["A", "B", "C"]);
        assert_eq!(out.iter().map(|o| o.to.as_str()).collect::<Vec<_>>(), vec!["B", "C"]);
        for o in &out {
            assert!(matches!(&o.body, Body::Tail(c) if c.len() == 1));
            assert_eq!(o.hash, a.hash());
        }
    }

    #[test]
    fn nothing_is_posted_again_until_something_changes() {
        let mut a = pc("A");
        act(&mut a, edit(mk(1), file(12)), T0);
        assert_eq!(post(&mut a, &["B"]).len(), 1);
        assert!(post(&mut a, &["B"]).is_empty());
        act(&mut a, edit(mk(2), file(22)), T0);
        let out = post(&mut a, &["B"]);
        assert!(matches!(&out[0].body, Body::Tail(c) if c.len() == 2));
    }

    #[test]
    fn an_event_the_relay_refused_is_built_again() {
        let mut a = pc("A");
        act(&mut a, edit(mk(1), file(12)), T0);
        let snap = disk(&a);
        assert_eq!(a.outbox(&names(&["B"]), &snap, 0).len(), 1);
        // Not marked sent: the next round builds it again.
        assert_eq!(a.outbox(&names(&["B"]), &snap, 0).len(), 1);
    }

    #[test]
    fn a_new_member_gets_a_checkpoint_until_they_have_it() {
        let mut a = pc("A");
        act(&mut a, edit(mk(1), file(12)), T0);
        a.new_member("B");
        let out = post(&mut a, &["B"]);
        assert!(matches!(&out[0].body, Body::Checkpoint(c) if !c.is_empty()));
        // B acks everything: the next event is an ordinary tail.
        let mut acked = a.my_status();
        acked.member = "B".into();
        a.members.insert("B".into(), acked);
        a.game_running = true;
        let out = post(&mut a, &["B"]);
        assert!(matches!(&out[0].body, Body::Tail(_)));
    }

    #[test]
    fn a_status_only_change_is_posted_again() {
        let mut a = pc("A");
        post(&mut a, &["B"]);
        assert!(post(&mut a, &["B"]).is_empty());
        a.game_running = true;
        let out = post(&mut a, &["B"]);
        assert_eq!(out.len(), 1);
        assert!(out[0].status.game_running);
        assert!(matches!(&out[0].body, Body::Tail(c) if c.is_empty()));
    }

    #[test]
    fn every_fiftieth_event_carries_a_checkpoint() {
        let mut a = pc("A");
        let mut full = 0;
        for i in 0..100 {
            a.game_running = i % 2 == 0;
            for o in post(&mut a, &["B"]) {
                if matches!(o.body, Body::Checkpoint(_)) {
                    full += 1;
                }
            }
        }
        assert_eq!(a.posting.publishes, 100);
        assert_eq!(full, 2);
    }
}
