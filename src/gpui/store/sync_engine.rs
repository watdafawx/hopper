//! Friends group sync on this PC, over the new core (`hopper::synccore`,
//! design/sync-rewrite.md). One engine per group copy: it records what the
//! player does (the `actor` hook queues the edits, this drains them), takes
//! in what friends send, posts what they haven't got, and applies what
//! landed with `synccore::apply`. Nothing here guesses a change from two mod
//! lists; the screens only read [`Store::sync_status`] and the reviews.

use super::{Msg, Store, StoreEvent};
use gpui_kit::*;
use hopper::groupstatus::Status as Chip;
use hopper::mailbox::{Event, Kind};
use hopper::synccore::{
    self, Answer, ApplyPlan, Arrival, Dot, Edit, Engine, Key, PackMode, Review, Snapshot, Status, Step, Trust, Value, Why,
    apply::{self, ApplyReport, Outcome, StepResult},
    bridge, local, wire,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The instance id of a sync event: the copy's id after this, then (when it
/// went in parts) `#part-<i>`.
pub const PREFIX: &str = "~sync:";
/// The relay keeps an event's code in a field of 20 000 bytes; "HOP-" and
/// the part's header are inside this.
const PART_BYTES: usize = 19_000;
/// A post that failed waits this long.
const POST_RETRY: Duration = Duration::from_secs(10);
/// Events that came before their copy was set up, kept this many.
const EARLY_MAX: usize = 64;

/// What `Msg::SyncApplied` carries: an apply job's end.
pub struct SyncApplied {
    pub local: String,
    pub report: ApplyReport,
    /// Put back from drift: the steps aren't the register's.
    pub put_back: bool,
}

struct Slot {
    engine: Engine,
    root: PathBuf,
    group: String,
    dirty: bool,
    posting: bool,
    applying: bool,
    /// The disk as the last tick read it.
    snap: Snapshot,
    /// Members seen in the group so far (a new one gets a checkpoint).
    known: HashSet<String>,
    /// Failures said once.
    said: HashSet<String>,
    /// Mods History's Undo just changed: their difference on disk is the
    /// player's Undo, not drift.
    undone: Vec<(Key, Instant)>,
}

#[derive(Default)]
pub struct SyncCore {
    /// By the local instance's id.
    slots: HashMap<String, Slot>,
    parts: HashMap<(String, String), wire::Reassembly>,
    early: Vec<Event>,
    post_failed_at: Option<Instant>,
    ticks: u64,
}

impl SyncCore {
    fn by_copy(&mut self, copy: &str) -> Option<&mut Slot> {
        self.slots.values_mut().find(|s| s.engine.copy == copy)
    }
}

/// The copy id and the part of an event's `inst`, when it is a sync event.
fn parse_inst(inst: &str) -> Option<&str> {
    let rest = inst.strip_prefix(PREFIX)?;
    Some(rest.split("#part-").next().unwrap_or(rest))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn only_here_keys(inst: &hopper::instances::Instance) -> BTreeSet<Key> {
    inst.addons.iter().filter(|a| hopper::onlyhere::is_kept(inst, a)).map(local::key_of).collect()
}

impl Store {
    // ---- Setting up ---------------------------------------------------------

    /// Whether the instance at `root` is a group copy run by the sync core.
    pub fn is_sync_copy(&self, root: &Path) -> bool {
        self.sync.slots.values().any(|s| s.root == root)
    }

    fn sync_dir() -> Option<PathBuf> {
        hopper::paths::dirs().map(|d| d.data_dir().to_path_buf())
    }

    /// The engine of a copy: saved state when it reads, else a new one that
    /// starts from the disk (so nothing already there is fetched again).
    fn sync_open(&self, copy: &str, me: &str, name: &str, snap: &Snapshot) -> Engine {
        let loaded = Self::sync_dir().and_then(|dir| match synccore::store::load(&dir, copy) {
            Ok(e) => e,
            Err(e) => {
                log::warn!("sync state of {copy} couldn't be read, starting clean: {e:#}");
                None
            }
        });
        match loaded {
            Some(mut e) if e.me == me => {
                e.name = name.to_string();
                e
            }
            _ => {
                let mut e = Engine::new(copy, me, name);
                e.applied = snap.values.clone();
                e
            }
        }
    }

    fn sync_save(slot: &mut Slot) {
        if !slot.dirty {
            return;
        }
        slot.dirty = false;
        if let Some(dir) = Self::sync_dir()
            && let Err(e) = synccore::store::save(&dir, &slot.engine)
        {
            log::warn!("saving the sync state of {}: {e:#}", slot.engine.copy);
        }
    }

    // ---- The tick -----------------------------------------------------------

    /// Twice a second: sets up engines for the group copies, takes in what the
    /// player did, posts, and applies. Cheap when nothing changed.
    pub(super) fn sync_tick(&mut self, cx: &mut Context<Self>) {
        self.sync.ticks += 1;
        let Some(mb) = self.mailbox.clone() else { return };
        let (me, my_name) = (mb.account().id.clone(), mb.account().name.clone());
        if me.is_empty() {
            return;
        }
        // The copies this PC keeps both ways, with their instances.
        let wanted: Vec<(String, String, String)> = self
            .settings
            .group_copies
            .iter()
            .filter(|c| self.instances.iter().any(|i| i.id == c.local))
            .map(|c| (c.local.clone(), c.inst.clone(), c.group.clone()))
            .collect();
        let gone: Vec<String> = self.sync.slots.keys().filter(|l| !wanted.iter().any(|(w, ..)| w == *l)).cloned().collect();
        for l in gone {
            if let Some(s) = self.sync.slots.remove(&l) {
                local::unwatch(&s.root);
            }
        }
        let healthy = self.mailbox_error.is_none();
        let now = unix_now();
        for (local_id, copy, group) in wanted {
            let Some(inst) = self.instances.iter().find(|i| i.id == local_id) else { continue };
            let snap = local::snapshot(inst);
            let root = inst.root.clone();
            let kept = only_here_keys(inst);
            let playing = self.running(&root).is_some();
            if !self.sync.slots.contains_key(&local_id) {
                let engine = self.sync_open(&copy, &me, &my_name, &snap);
                local::watch(&root);
                let mut slot = Slot { engine, root: root.clone(), group: group.clone(), dirty: true, posting: false, applying: false, snap: snap.clone(), known: HashSet::new(), said: HashSet::new(), undone: Vec::new() };
                // A swap the last run stopped in the middle is finished now.
                match apply::recover(&root) {
                    Ok(Some(report)) => slot.engine.applied(&report),
                    Ok(None) => {}
                    Err(e) => log::warn!("finishing an interrupted sync apply at {}: {e:#}", root.display()),
                }
                self.sync.slots.insert(local_id.clone(), slot);
                // Events that came before this copy existed.
                let early: Vec<Event> = std::mem::take(&mut self.sync.early);
                for e in early {
                    self.sync_receive(e, cx);
                }
            }
            let members: Vec<(String, String)> = self
                .mailbox_groups()
                .iter()
                .find(|g| g.id == group)
                .map(|g| g.members.iter().filter(|m| m.joined && m.id != me).map(|m| (m.id.clone(), m.name.clone())).collect())
                .unwrap_or_default();
            let only = self.settings.friend_groups.iter().find(|g| g.id == group).and_then(|g| g.only_from(&me).map(str::to_string));
            let allow_sources = self.settings.allow_sources;
            let push_wait = self.settings.push_by_hand && !self.push_now.contains(&local_id);
            let busy = self.follow_busy(&root);
            let Some(slot) = self.sync.slots.get_mut(&local_id) else { continue };
            slot.group = group.clone();
            slot.snap = snap.clone();
            if healthy {
                slot.engine.touch(now);
            }
            slot.engine.game_running = playing;
            // What the player did here since the last tick.
            for edit in local::take(&root) {
                if kept.contains(&edit.key) {
                    slot.engine.applied.insert(edit.key.clone(), edit.now.clone());
                    continue;
                }
                slot.engine.record(edit.clone(), 0);
                slot.engine.applied.insert(edit.key, edit.now);
                slot.dirty = true;
            }
            // The pack's version, the loader and the Minecraft version have no
            // hook of their own: a change of them here, not made by an apply,
            // is the player's.
            if !slot.applying {
                for key in [Key::Pack, Key::Loader, Key::Game] {
                    let Some(now_v) = snap.values.get(&key).cloned() else { continue };
                    match slot.engine.applied.get(&key) {
                        None => {
                            slot.engine.applied.insert(key, now_v);
                            slot.dirty = true;
                        }
                        Some(v) if *v != now_v => {
                            let why = if key == Key::Pack { Why::PackUpdate { mode: PackMode::default() } } else { Why::Player };
                            slot.engine.record(Edit { key: key.clone(), now: now_v.clone(), why, batch: String::new() }, 0);
                            slot.engine.applied.insert(key, now_v);
                            slot.dirty = true;
                        }
                        Some(_) => {}
                    }
                }
            }
            // History's Undo: the files came back, so the change is the
            // player's own, made as an Undo of the one it undoes.
            if !slot.undone.is_empty() {
                slot.undone.retain(|(_, t)| t.elapsed() < Duration::from_secs(30));
                for d in slot.engine.drift(&snap) {
                    let Some(i) = slot.undone.iter().position(|(k, _)| *k == d.key) else { continue };
                    slot.undone.remove(i);
                    if kept.contains(&d.key) {
                        continue;
                    }
                    let why = slot.engine.last_change(&d.key).map_or(Why::Player, |c| Why::Undo { of: c.dot.clone() });
                    slot.engine.record(Edit { key: d.key.clone(), now: d.found.clone(), why, batch: String::new() }, 0);
                    slot.engine.applied.insert(d.key, d.found);
                    slot.dirty = true;
                }
            }
            // "Take changes from": the others wait for the player's yes.
            for (id, _) in &members {
                let trust = match &only {
                    Some(maker) if maker != id => Trust::AskFirst,
                    _ => Trust::Auto,
                };
                if slot.engine.trust.get(id).copied().unwrap_or_default() != trust {
                    slot.engine.set_trust(id, trust);
                    slot.dirty = true;
                }
                if slot.known.insert(id.clone()) && !slot.engine.members.contains_key(id) {
                    slot.engine.new_member(id);
                }
            }
            // Mods from a link: the setting that allows them all answers the
            // card by itself.
            if allow_sources {
                for r in slot.engine.reviews().into_iter().filter(|r| r.risk == synccore::Risk::NewSource) {
                    for k in &r.keys {
                        slot.engine.allow_source(k);
                    }
                    slot.engine.answer(r.id, Answer::Take, false);
                    slot.dirty = true;
                }
            }
            // Post what the group hasn't got.
            let to: Vec<String> = members.iter().map(|(id, _)| id.clone()).collect();
            let post = healthy
                && !push_wait
                && !slot.posting
                && !to.is_empty()
                && self.sync.post_failed_at.is_none_or(|t| t.elapsed() >= POST_RETRY);
            let outs = if post { slot.engine.outbox(&to, &snap, 0) } else { Vec::new() };
            let apply_now = !slot.applying && !busy && root.is_dir() && (self.sync.ticks % 4 == 0 || slot.dirty);
            let plan = if apply_now {
                let mut p = slot.engine.plan();
                p.steps.retain(|s| !kept.contains(&s.key));
                p
            } else {
                ApplyPlan::default()
            };
            Self::sync_save(slot);
            if !outs.is_empty() {
                self.sync_post(&local_id, outs, cx);
            }
            if !plan.steps.is_empty() {
                self.sync_apply(&local_id, plan, false, cx);
            }
        }
    }

    // ---- Network ------------------------------------------------------------

    /// Posts one event (in parts when big) to the whole group: everyone gets
    /// the same records, each takes in only what is new to them.
    fn sync_post(&mut self, local_id: &str, outs: Vec<synccore::engine::Outgoing>, cx: &mut Context<Self>) {
        let Some(mb) = self.mailbox.clone() else { return };
        let Some(slot) = self.sync.slots.get_mut(local_id) else { return };
        let mut changes: Vec<synccore::Change> = Vec::new();
        let mut full = false;
        let mut status = outs[0].status.clone();
        for o in &outs {
            let list = match &o.body {
                wire::Body::Tail(c) => c,
                wire::Body::Checkpoint(c) => {
                    full = true;
                    c
                }
            };
            for c in list {
                if !changes.iter().any(|x| x.dot == c.dot) {
                    changes.push(c.clone());
                }
            }
            status.needs_full |= o.status.needs_full;
        }
        let body = if full { wire::Body::Checkpoint(changes) } else { wire::Body::Tail(changes) };
        let mut copies = BTreeMap::new();
        copies.insert(slot.engine.copy.clone(), (body, status, outs[0].hash.clone()));
        let packet = wire::Packet { version: wire::WIRE_VERSION, from: slot.engine.me.clone(), group: slot.group.clone(), copies };
        let parts = wire::encode_with(&packet, PART_BYTES);
        let limits = wire::RelayLimits::default();
        if parts.iter().any(|p| p.len() + 4 > limits.max_body) {
            log::warn!("a sync packet of {} is too big for the relay", slot.engine.copy);
            return;
        }
        slot.posting = true;
        let (gid, copy, name) = (slot.group.clone(), slot.engine.copy.clone(), slot.engine.name.clone());
        let local_id = local_id.to_string();
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    let n = parts.len();
                    for (i, p) in parts.iter().enumerate() {
                        let inst = wire::part_key(&format!("{PREFIX}{copy}"), i, n);
                        mb.publish_with(&[], std::slice::from_ref(&gid), &inst, "~sync", &name, &format!("HOP-{p}"), &[], &BTreeMap::new(), "")?;
                    }
                    Ok::<(), anyhow::Error>(())
                })
                .await;
            this.update(cx, |s, cx| s.sync_posted(&local_id, outs, r, cx)).ok();
        })
        .detach();
    }

    fn sync_posted(&mut self, local_id: &str, outs: Vec<synccore::engine::Outgoing>, r: anyhow::Result<()>, cx: &mut Context<Self>) {
        let Some(slot) = self.sync.slots.get_mut(local_id) else { return };
        slot.posting = false;
        match r {
            Ok(()) => {
                for o in &outs {
                    slot.engine.sent(o);
                }
                slot.dirty = true;
                Self::sync_save(slot);
                // A Push by hand went out.
                self.push_now.remove(local_id);
                self.to_push.remove(local_id);
                self.publish_said = None;
            }
            Err(e) => {
                log::warn!("sending changes to the group: {e:#}");
                self.sync.post_failed_at = Some(Instant::now());
                let text = hopper::mailbox::says(&e);
                if self.publish_said.as_deref() != Some(text.as_str()) {
                    cx.emit(StoreEvent::Notice(text.clone(), true));
                    self.publish_said = Some(text);
                }
            }
        }
        cx.notify();
    }

    /// An event of the feed: a part of a friend's packet for one of the
    /// copies here. Taken in at once; what it changes lands on the next tick.
    pub(super) fn sync_receive(&mut self, e: Event, cx: &mut Context<Self>) {
        let Some(copy) = parse_inst(&e.inst).map(str::to_string) else { return };
        if e.from == self.mailbox_id() {
            return;
        }
        if !self.sync.slots.values().any(|s| s.engine.copy == copy) {
            self.sync.early.retain(|x| !(x.from == e.from && x.inst == e.inst));
            if self.sync.early.len() < EARLY_MAX {
                self.sync.early.push(e);
            }
            return;
        }
        let part = e.code.strip_prefix("HOP-").unwrap_or(&e.code);
        let asm = self.sync.parts.entry((e.from.clone(), copy.clone())).or_default();
        let packet = match asm.push(part) {
            Ok(Some(p)) => p,
            Ok(None) => return,
            Err(wire::WireError::OutOfDate { .. }) => {
                if let Some(slot) = self.sync.by_copy(&copy) {
                    slot.engine.note_out_of_date(&e.from);
                    slot.dirty = true;
                    Self::sync_save(slot);
                }
                cx.notify();
                return;
            }
            Err(err) => {
                log::warn!("a damaged sync packet from {}: {err}", e.from);
                return;
            }
        };
        let mb_id = self.mailbox_id();
        let Some(slot) = self.sync.by_copy(&copy) else { return };
        if packet.from == mb_id {
            return;
        }
        let got = slot.engine.receive(packet, Arrival { seq: e.seq, at: e.at });
        slot.dirty = true;
        Self::sync_save(slot);
        let notes = got.notes;
        for n in notes {
            cx.emit(StoreEvent::Notice(n, false));
        }
        cx.notify();
    }

    /// Pull now: reads everyone's latest events from the relay and takes
    /// them in. Says what it found when done.
    pub(super) fn sync_pull(&mut self, root: &Path, cx: &mut Context<Self>) {
        let Some(mb) = self.mailbox_or_say(cx) else { return };
        let Some(local_id) = self.sync.slots.iter().find(|(_, s)| s.root == root).map(|(l, _)| l.clone()) else { return };
        self.group_pulling.insert(local_id.clone(), true);
        self.group_pulled.remove(&local_id);
        cx.notify();
        let root = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            let r = cx.background_spawn(async move { mb.feed(0, 0, "") }).await;
            this.update(cx, |s, cx| {
                match r {
                    Ok(feed) => {
                        s.mailbox_checked = Some(unix_now());
                        for e in feed.events.into_iter().filter(|e| e.kind() == Kind::Recipe && e.inst.starts_with(PREFIX)) {
                            s.sync_receive(e, cx);
                        }
                    }
                    Err(e) => {
                        log::warn!("Pull now couldn't reach the group: {e:#}");
                        s.group_pulling.insert(local_id.clone(), false);
                    }
                }
                s.sync_tick(cx);
                s.sync_pulled(&local_id, &root, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Pull now's end: the copy's chip says what happened.
    fn sync_pulled(&mut self, local_id: &str, root: &Path, cx: &mut Context<Self>) {
        let Some(reached) = self.group_pulling.remove(local_id) else { return };
        let Some(after) = self.group_status(root) else { return };
        let (text, bad) = hopper::groupstatus::pull_says(reached, 0, &[], &after, unix_now());
        self.group_pulled.insert(local_id.to_string(), (text.clone(), bad, Instant::now()));
        cx.emit(StoreEvent::Notice(text, bad));
        cx.notify();
    }

    // ---- Applying -----------------------------------------------------------

    /// Runs `plan` on the instance, off the UI thread. A put back is the same
    /// swap with the steps made from the drift.
    fn sync_apply(&mut self, local_id: &str, plan: ApplyPlan, put_back: bool, cx: &mut Context<Self>) {
        let _ = cx;
        let Some(gid) = self.sync.slots.get(local_id).map(|s| s.group.clone()) else { return };
        let group = self.mailbox_groups().iter().find(|g| g.id == gid).map(|g| g.name.clone()).unwrap_or_default();
        let Some(slot) = self.sync.slots.get_mut(local_id) else { return };
        if slot.applying {
            return;
        }
        slot.applying = true;
        let (root, copy) = (slot.root.clone(), slot.engine.copy.clone());
        let allow = self.settings.allow_sources;
        let name = self.instance(&root).map(|i| i.name.clone()).unwrap_or_default();
        let who: Vec<String> = {
            let mut w: Vec<String> = plan.steps.iter().map(|s| s.name.clone()).filter(|n| !n.is_empty()).collect();
            w.dedup();
            w
        };
        let title = match who.as_slice() {
            [one] if !put_back => format!("Updating {name} with {one}'s changes"),
            _ if put_back => format!("Putting {name} back as the group has it"),
            _ => format!("Updating {name} with the group's changes"),
        };
        let local_id = local_id.to_string();
        let _ = copy;
        self.spawn_instance_job_quiet(&root.clone(), title, move |job| {
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let Some(inst) = hopper::instances::load(&root) else {
                    return failed(&plan, "the instance couldn't be read");
                };
                let fetch = bridge::InstFetch::prepare(&inst, &plan, allow, &group, job);
                let report = apply::run(&root, &plan, &fetch, job);
                if let Err(e) = fetch.finish(&plan, &report) {
                    log::warn!("updating the instance's records after a sync apply: {e:#}");
                }
                report
            }));
            let report = run.unwrap_or_else(|_| failed(&plan, "Hopper hit a bug and stopped this task."));
            Ok(Msg::SyncApplied(Box::new(SyncApplied { local: local_id, report, put_back })))
        });
    }

    pub(super) fn on_sync_applied(&mut self, a: SyncApplied, cx: &mut Context<Self>) {
        let Some(slot) = self.sync.slots.get_mut(&a.local) else { return };
        slot.applying = false;
        if !a.put_back {
            slot.engine.applied(&a.report);
        }
        slot.dirty = true;
        let root = slot.root.clone();
        let mut done = 0;
        let mut loader = false;
        let mut said: Vec<String> = Vec::new();
        for r in &a.report.results {
            match &r.outcome {
                Outcome::Done => {
                    done += 1;
                    loader |= r.key == Key::Loader;
                }
                Outcome::Failed(why) => {
                    log::warn!("a synced change didn't go in ({:?}): {why}", r.key);
                    let line = format!("Couldn't take in {}'s change. {why} Hopper tries again.", r.by.author);
                    if slot.said.insert(format!("{:?}|{}", r.key, r.by.n)) {
                        said.push(line);
                    }
                }
                Outcome::Blocked | Outcome::Retry { .. } => {}
            }
        }
        Self::sync_save(slot);
        // The loader the group moved to is installed like a change in Settings.
        if loader && self.running(&root).is_none()
            && let Some(inst) = hopper::instances::load(&root)
        {
            self.install_loader(inst);
        }
        if done > 0 || a.report.results.iter().any(|r| r.outcome == Outcome::Blocked) {
            self.want_group_needs(&root);
            self.handle(Msg::InstanceChanged(root.clone()), cx);
        }
        for line in said.into_iter().take(1) {
            cx.emit(StoreEvent::Notice(line, true));
        }
        if let Some(id) = self.group_copy(&root).map(|c| c.local.clone()) {
            self.group_pulled.remove(&id);
        }
        cx.notify();
    }

    // ---- The chip and the cards ---------------------------------------------

    /// Whether the instance with this id is a group copy run by the sync core.
    pub(super) fn sync_has_copy(&self, id: &str) -> bool {
        self.sync.slots.contains_key(id)
    }

    /// The ids of the copies that hold changes back for Push.
    pub(super) fn sync_copy_ids(&self) -> impl Iterator<Item = &String> {
        self.sync.slots.keys().filter(|id| self.sync_held(id).is_some())
    }

    /// How many of its own changes the copy holds back for Push.
    pub(super) fn sync_held(&self, id: &str) -> Option<usize> {
        let s = self.sync.slots.get(id)?;
        match s.engine.status(&s.snap) {
            Status::Sending(n) if self.settings.push_by_hand && !self.push_now.contains(id) => Some(n),
            _ => None,
        }
    }

    /// The chip's state for a group copy, from the engine's status and the
    /// words the screens already have.
    pub(super) fn sync_chip(&self, root: &Path, connected: bool) -> Option<Chip> {
        let (local_id, slot) = self.sync.slots.iter().find(|(_, s)| s.root == root)?;
        let status = slot.engine.status(&slot.snap);
        let by_hand: Vec<String> = self.group_pending(root).into_iter().map(|p| p.name).collect();
        let playing = self.running(root).is_some();
        let checked = self.mailbox_checked;
        if let Status::OutOfDate(who) = &status {
            let who = if who.is_empty() { "A friend".to_string() } else { who.clone() };
            return Some(Chip::Cant { why: format!("{who}'s Hopper is older and sends changes this one can't read. They need to update Hopper."), again: false, mods: Vec::new() });
        }
        if !connected {
            return Some(Chip::Offline { checked });
        }
        if !by_hand.is_empty() {
            return Some(Chip::ByHand { mods: by_hand });
        }
        let waiting = slot.engine.plan();
        let retry = waiting.waits.iter().any(|(_, w)| matches!(w, synccore::Wait::Retry { .. }));
        Some(match status {
            Status::NeedsYou(n) => {
                let reviews = slot.engine.reviews();
                let only_asks = !reviews.is_empty() && reviews.iter().all(|r| r.risk == synccore::Risk::AskFirst);
                if only_asks {
                    let mut from: Vec<hopper::syncmode::Parked> = Vec::new();
                    for r in &reviews {
                        let id = r.changes.first().map(|d| d.author.clone()).unwrap_or_default();
                        match from.iter_mut().find(|p| p.id == id) {
                            Some(p) => p.changes += r.changes.len(),
                            None => from.push(hopper::syncmode::Parked { id, name: r.who.clone(), changes: r.changes.len() }),
                        }
                    }
                    let only = self.only_from_group(&slot.group).map(|(_, n)| n).unwrap_or_default();
                    Chip::Parked { from, only }
                } else {
                    let title = if n == 1 { "A change waits for your answer".to_string() } else { format!("{n} changes wait for your answer") };
                    Chip::WaitsForYou { title }
                }
            }
            Status::Differs(n) => Chip::Behind { mods: n, missing: slot.engine.missing(&slot.snap).len() == n },
            Status::Behind(n) if retry => Chip::Cant { why: format!("{n} of the group's changes didn't go in."), again: true, mods: Vec::new() },
            Status::Behind(n) if playing && slot.engine.game_running => Chip::WaitsForGame { what: vec![if n == 1 { "1 change".to_string() } else { format!("{n} changes") }] },
            Status::Behind(n) => {
                let mut from: Vec<String> = waiting.steps.iter().map(|s| s.name.clone()).filter(|n| !n.is_empty()).collect();
                from.sort();
                from.dedup();
                Chip::Updating { changes: n, from }
            }
            Status::Sending(n) if self.settings.push_by_hand && !self.push_now.contains(local_id) => Chip::ToPush { changes: n, after: None },
            Status::Sending(_) => Chip::Sending,
            Status::InSync => {
                let needs = self.group_needs(root);
                if needs.is_empty() { Chip::InStep { checked } } else { Chip::NeedsMod { needs: needs.to_vec() } }
            }
            Status::OutOfDate(_) => unreachable!("handled above"),
        })
    }

    /// The reviews of the copy at `root`: changes that wait for the player.
    pub fn sync_reviews(&self, root: &Path) -> Vec<Review> {
        self.sync.slots.values().find(|s| s.root == root).map(|s| s.engine.reviews()).unwrap_or_default()
    }

    /// The player's answer to a card: let the change land, or keep what is here.
    pub fn answer_sync_review(&mut self, root: &Path, review: u64, answer: Answer, cx: &mut Context<Self>) {
        let Some(slot) = self.sync.slots.values_mut().find(|s| s.root == root) else { return };
        if answer == Answer::Take {
            for r in slot.engine.reviews().into_iter().filter(|r| r.id == review && r.risk == synccore::Risk::NewSource) {
                for k in &r.keys {
                    slot.engine.allow_source(k);
                }
            }
        }
        slot.engine.answer(review, answer, false);
        slot.dirty = true;
        Self::sync_save(slot);
        self.sync_tick(cx);
        cx.notify();
    }

    /// "Pull from Mia": answers every card of hers with yes.
    pub(super) fn sync_take_from(&mut self, root: &Path, who: &str, cx: &mut Context<Self>) {
        let Some(slot) = self.sync.slots.values_mut().find(|s| s.root == root) else { return };
        let ids: Vec<u64> = slot
            .engine
            .reviews()
            .into_iter()
            .filter(|r| r.risk == synccore::Risk::AskFirst && r.changes.iter().any(|d| d.author == who))
            .map(|r| r.id)
            .collect();
        for id in ids {
            slot.engine.answer(id, Answer::Take, false);
        }
        slot.dirty = true;
        Self::sync_save(slot);
        self.sync_tick(cx);
    }

    /// Who changed the mod last, when it wasn't this PC: "Turned off by Mia".
    pub fn sync_mark(&self, root: &Path, a: &hopper::instances::Addon) -> Option<String> {
        let slot = self.sync.slots.values().find(|s| s.root == root)?;
        let key = local::key_of(a);
        let c = slot.engine.last_change(&key)?;
        if c.dot.author == slot.engine.me || c.name.is_empty() {
            return None;
        }
        // Changed here since: the mark is out of date.
        if slot.engine.applied.get(&key) != Some(&c.now) && !matches!(c.now, Value::PackDefault) {
            return None;
        }
        let who = &c.name;
        Some(match (&c.was, &c.now) {
            (_, Value::Gone | Value::PackRemoved) => format!("Removed by {who}"),
            (_, Value::PackOff | Value::File { on: false, .. }) => format!("Turned off by {who}"),
            (Value::PackOff | Value::File { on: false, .. }, Value::PackDefault | Value::File { on: true, .. }) => format!("Turned on by {who}"),
            (Value::Gone, Value::File { .. }) => format!("Added by {who}"),
            (_, Value::File { .. } | Value::PackDefault) => format!("Updated by {who}"),
            _ => return None,
        })
    }

    /// Who locked the mod last (a lock record's author), when it wasn't this PC.
    pub fn sync_lock_mark(&self, root: &Path, a: &hopper::instances::Addon) -> Option<String> {
        let slot = self.sync.slots.values().find(|s| s.root == root)?;
        let Key::Mod(k) = local::key_of(a) else { return None };
        let c = slot.engine.last_change(&Key::Lock(k))?;
        if c.dot.author == slot.engine.me || c.name.is_empty() || matches!(c.now, Value::Gone) {
            return None;
        }
        (a.locked || !a.locked_version.is_empty()).then(|| format!("Locked by {}", c.name))
    }

    // ---- Differences on this PC ---------------------------------------------

    /// "Make my copy match": the mods folder goes back to what was applied
    /// (`keys`: only these). Nothing is sent.
    pub(super) fn sync_put_back(&mut self, root: &Path, keys: Option<Vec<String>>, cx: &mut Context<Self>) {
        let Some((local_id, slot)) = self.sync.slots.iter().find(|(_, s)| s.root == root) else { return };
        let local_id = local_id.clone();
        let drifts = slot.engine.drift(&slot.snap);
        let kept = self.instance(root).map(only_here_keys).unwrap_or_default();
        let mut steps = Vec::new();
        for d in drifts {
            if kept.contains(&d.key) {
                continue;
            }
            if let (Some(keys), Key::Mod(k)) = (&keys, &d.key)
                && !keys.iter().any(|x| x == k)
            {
                continue;
            }
            let (from, to) = (d.found, d.expected);
            // A pack mod is put back as the pack ships it: a state, not a file.
            steps.push(Step { key: d.key, from, to, by: Dot { author: slot.engine.me.clone(), n: 0 }, name: slot.engine.name.clone(), why: Why::Player });
        }
        if steps.is_empty() {
            return;
        }
        self.sync_apply(&local_id, ApplyPlan { steps, waits: Vec::new() }, true, cx);
    }

    /// "Remove for everyone" (and the like): the differences on this PC become
    /// this player's changes, once, so the group gets them. The screen for it
    /// is the next phase's.
    #[allow(dead_code)]
    pub fn sync_share_drift(&mut self, root: &Path, cx: &mut Context<Self>) {
        let Some(slot) = self.sync.slots.values_mut().find(|s| s.root == root) else { return };
        let drifts = slot.engine.drift(&slot.snap);
        let batch = format!("{}:drift:{}", slot.engine.me, unix_now());
        for e in local::share_edits(&drifts, &batch) {
            let (key, now) = (e.key.clone(), e.now.clone());
            slot.engine.record(e, 0);
            slot.engine.applied.insert(key, now);
        }
        slot.dirty = true;
        Self::sync_save(slot);
        self.sync_tick(cx);
        cx.notify();
    }

    /// History's Undo put these files (names in the instance's folders) back
    /// or took them away: the next tick records what that changed as Undo.
    pub fn sync_note_undo(&mut self, root: &Path, files: &[String]) {
        let Some(inst) = self.instances.iter().find(|i| i.root == root) else { return };
        let keys: Vec<Key> = inst
            .addons
            .iter()
            .filter(|a| files.iter().any(|f| f.trim_end_matches(".disabled").eq_ignore_ascii_case(a.file_name.trim_end_matches(".disabled"))))
            .map(local::key_of)
            .collect();
        if let Some(slot) = self.sync.slots.values_mut().find(|s| s.root == root) {
            slot.undone.extend(keys.into_iter().map(|k| (k, Instant::now())));
        }
    }

    /// The members whose changes wait for the player to take: (id, name).
    pub(super) fn only_from_group(&self, group: &str) -> Option<(String, String)> {
        let me = self.mailbox_id();
        let g = self.settings.friend_groups.iter().find(|g| g.id == group)?;
        let id = g.only_from(&me)?.to_string();
        let name = self.mailbox_friends().iter().find(|f| f.id == id).map(|f| f.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| "the group's maker".to_string());
        Some((id, name))
    }
}

fn failed(plan: &ApplyPlan, why: &str) -> ApplyReport {
    ApplyReport { results: plan.steps.iter().map(|s| StepResult { key: s.key.clone(), by: s.by.clone(), outcome: Outcome::Failed(why.to_string()) }).collect() }
}
