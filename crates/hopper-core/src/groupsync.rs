//! Two-way sync of a group's pack: every member's copy publishes its code
//! into the group, and each PC takes the other members' changes mod by mod.
//!
//! The relay keeps one pack per sender (`<who>/<inst>`), so each member
//! publishes their copy under the pack's own id. A copy here compares each
//! sender's new code with the last one it took from them; only the mods
//! that differ are changes, so a member whose copy is behind (offline, or
//! not yet given someone else's change) never undoes anything by
//! publishing it. The relay numbers every event (`seq`) in one order for
//! everyone, which decides who changed a mod last: a change older than the
//! mod's last known change is dropped, on every PC alike.
//!
//! Echoes end by themselves: applying a change publishes this copy again
//! (its code changed), but the others already have that mod that way, so
//! nothing changes there and nothing is sent on.

use crate::instances::{Addon, Instance};
use crate::jobs::Job;
use crate::modrinth::{is_mr, num_id};
use crate::settings::{FollowedRecipe, Settings};
use crate::sharecode::{Entry, Share, Target};
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// The `key` a member's copy is published with, so a PC that knows two-way
/// groups takes it into its own copy instead of listing it as a pack.
pub const COPY_KEY: &str = "~copy";

/// One mod's place in a code, as two codes of one pack compare it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub enum State {
    /// Not listed: a pack mod as the pack has it, or a mod not there.
    #[default]
    Unlisted,
    /// A pack mod taken out.
    Removed,
    /// A pack mod turned off.
    Off,
    /// A file of the sender's ("cf:<file>", "mr:<version>" or
    /// "src:<file name>"), on or off.
    File { file: String, on: bool },
}

/// What a change did, for "Turned off by Mia".
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub enum Kind {
    #[default]
    Added,
    Removed,
    On,
    Off,
    Updated,
}

impl Kind {
    /// "Turned off by Mia".
    pub fn says(self, who: &str) -> String {
        match self {
            Kind::Added => format!("Added by {who}"),
            Kind::Removed => format!("Removed by {who}"),
            Kind::On => format!("Turned on by {who}"),
            Kind::Off => format!("Turned off by {who}"),
            Kind::Updated => format!("Updated by {who}"),
        }
    }
}

/// A mod's key across codes and PCs: "p:<project>" for CurseForge and
/// Modrinth (Hopper's number), "s:<url>#<pattern>" for a mod from a link,
/// "n:<file name>" for a pack file known only by name.
pub fn addon_key(a: &Addon) -> String {
    match &a.source {
        Some(s) => format!("s:{}#{}", s.url, s.pattern),
        None if a.project_id != 0 => format!("p:{}", a.project_id),
        None => format!("n:{}", a.file_name.trim_end_matches(".disabled").to_lowercase()),
    }
}

/// Whether `a` is the mod a code calls `key`. A code names a pack mod by
/// the pack's own entry ("p:<project>" from a CurseForge pack, "n:<file
/// name>" for a file the pack lists by hash, as every Modrinth pack mod
/// is), which needn't be the key [`addon_key`] gives the same mod here
/// ("p:<number>" for a Modrinth mod, "s:..." for one with a link).
pub fn is_addon(a: &Addon, key: &str) -> bool {
    if addon_key(a) == key {
        return true;
    }
    if let Some(n) = key.strip_prefix("n:") {
        return a.file_name.trim_end_matches(".disabled").eq_ignore_ascii_case(n);
    }
    if let Some(p) = key.strip_prefix("p:") {
        return p.parse::<u64>().is_ok_and(|p| p != 0 && a.project_id == p);
    }
    false
}

/// The mod here a code calls `key`.
pub fn find_addon(inst: &Instance, key: &str) -> Option<usize> {
    inst.addons.iter().position(|a| addon_key(a) == key).or_else(|| inst.addons.iter().position(|a| is_addon(a, key)))
}

fn target_key(t: &Target) -> String {
    match t {
        Target::Curse(p) => format!("p:{p}"),
        Target::Named(n) => format!("n:{}", n.to_lowercase()),
    }
}

/// A code's mods by key. `project` says whose a file is (None: couldn't
/// tell, and then `unsure` is set and the mod is left out).
pub struct Mods {
    pub map: BTreeMap<String, (State, Entry)>,
    pub unsure: bool,
}

pub fn mods(share: &Share, mut project: impl FnMut(&Entry) -> Option<u64>) -> Mods {
    let mut map = BTreeMap::new();
    let mut unsure = false;
    for e in &share.entries {
        let (key, state) = match e {
            Entry::CurseFile { file, enabled } => match project(e) {
                Some(p) => (format!("p:{p}"), State::File { file: format!("cf:{file}"), on: *enabled }),
                None => {
                    unsure = true;
                    continue;
                }
            },
            Entry::ModrinthFile { version, enabled } => match project(e) {
                Some(p) => (format!("p:{p}"), State::File { file: format!("mr:{version}"), on: *enabled }),
                None => {
                    unsure = true;
                    continue;
                }
            },
            Entry::Source { pin, enabled } => (
                format!("s:{}#{}", pin.source.url, pin.source.pattern),
                State::File { file: format!("src:{}", pin.file.to_lowercase()), on: *enabled },
            ),
            Entry::Remove(t) => (target_key(t), State::Removed),
            Entry::Off(t) => (target_key(t), State::Off),
        };
        map.insert(key, (state, e.clone()));
    }
    Mods { map, unsure }
}

/// One mod that differs between two codes.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub key: String,
    pub was: State,
    pub now: State,
    /// The new code's entry for it (None: no longer listed).
    pub entry: Option<Entry>,
}

impl Change {
    pub fn kind(&self) -> Kind {
        match (&self.was, &self.now) {
            (_, State::Off) => Kind::Off,
            (State::Off, State::Unlisted) => Kind::On,
            (_, State::Removed) | (State::File { .. }, State::Unlisted) => Kind::Removed,
            (State::File { file: a, on: was_on }, State::File { file: b, on }) => {
                if a != b {
                    Kind::Updated
                } else if *on && !was_on {
                    Kind::On
                } else {
                    Kind::Off
                }
            }
            (_, State::File { on: false, .. }) => Kind::Off,
            _ => Kind::Added,
        }
    }
}

/// The mods that differ from `was` to `now`. A mod `now` doesn't list
/// while one of its files couldn't be identified isn't called removed: it
/// may only be the one that wasn't recognised.
pub fn changes(was: &Mods, now: &Mods) -> Vec<Change> {
    let mut out = Vec::new();
    for (k, (s, e)) in &now.map {
        let before = was.map.get(k).map(|(s, _)| s.clone()).unwrap_or_default();
        if before != *s {
            out.push(Change { key: k.clone(), was: before, now: s.clone(), entry: Some(e.clone()) });
        }
    }
    if !now.unsure {
        for (k, (s, _)) in &was.map {
            if !now.map.contains_key(k) {
                out.push(Change { key: k.clone(), was: s.clone(), now: State::Unlisted, entry: None });
            }
        }
    }
    out
}

/// Who changed a mod last, as far as this PC knows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Mark {
    pub state: State,
    pub kind: Kind,
    /// Mailbox id, and the name to show.
    pub by: String,
    pub name: String,
    /// The relay's number for the event that carried it.
    pub seq: u64,
    /// When (unix seconds, the relay's).
    pub at: u64,
}

/// What to do with one change from another member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Make it here.
    Apply,
    /// It's already like that here (an echo, or the same change twice).
    Same,
    /// A later change of that mod is known: this one lost.
    Older,
    /// Changed here and not sent yet: that change goes out after this one,
    /// so it wins; this one is looked at again once it is sent.
    Held,
}

/// `here`: the mod in this copy (with the changes already decided);
/// `unsent`: it was changed here since this copy last published.
pub fn verdict(c: &Change, seq: u64, mark: Option<&Mark>, here: &State, unsent: bool) -> Verdict {
    decide(*here == c.now, seq, mark.map(|m| m.seq), unsent)
}

/// The rule behind [`verdict`] for anything kept latest-wins, a mod or one
/// of its settings (`configsync`): `same`, it's already like that here;
/// `seq`, the change's number; `known`, the number of the last change of
/// it this PC knows; `unsent`, changed here and not sent yet.
pub fn decide(same: bool, seq: u64, known: Option<u64>, unsent: bool) -> Verdict {
    if same {
        return Verdict::Same;
    }
    if known.is_some_and(|k| k > seq) {
        return Verdict::Older;
    }
    if unsent {
        return Verdict::Held;
    }
    Verdict::Apply
}

/// This copy's own changes, once a publish of it got `seq`: what differs
/// from the last code it sent, except what other members' changes made
/// (their mark already says so). Nothing the first time (no code sent yet
/// to compare with).
pub fn mark_own(marks: &mut BTreeMap<String, Mark>, prev: Option<&Mods>, now: &Mods, me: &str, seq: u64, at: u64) {
    let Some(prev) = prev else { return };
    for c in changes(prev, now) {
        if marks.get(&c.key).is_some_and(|m| m.state == c.now) {
            continue;
        }
        let kind = c.kind();
        marks.insert(c.key, Mark { state: c.now, kind, by: me.to_string(), name: String::new(), seq, at });
    }
}

/// The marks this copy's own changes get once the code `now` was sent as
/// `seq`: only those that differ from the copy's marks. Runs off the UI
/// thread (it may look files up).
pub fn sent_marks(inst: &Instance, copy: &PackCopy, now: &Share, me: &str, seq: u64, at: u64) -> Vec<(String, Mark)> {
    let Some(prev) = (!copy.sent.is_empty()).then(|| crate::sharecode::decode(&copy.sent).ok()).flatten() else {
        return Vec::new();
    };
    let found = projects_of(inst, &[&prev, now]);
    let mut marks = copy.marks.clone();
    mark_own(&mut marks, Some(&mods(&prev, lookup(&found))), &mods(now, lookup(&found)), me, seq, at);
    marks.into_iter().filter(|(k, m)| copy.marks.get(k) != Some(m)).collect()
}

/// When a mod got the way a code has it: the relay's number for that
/// change and the member who made it. A code carries one for each mod it
/// took from someone (or changed itself and already sent), so a member who
/// takes a change late and publishes it again doesn't date it with their
/// own, newer number: that would beat every change made since and undo it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Stamp {
    pub seq: u64,
    pub by: String,
}

/// The stamps a code going out carries: this copy's marks for the mods the
/// code has the way the mark says. A mod changed here since its mark has
/// none; it's this publish's own change.
pub fn stamps_of(marks: &BTreeMap<String, Mark>, now: &Mods) -> BTreeMap<String, Stamp> {
    let state_of = |k: &str| now.map.get(k).map(|(s, _)| s.clone()).unwrap_or_default();
    marks
        .iter()
        .filter(|(k, m)| m.seq > 0 && state_of(k) == m.state && !(now.unsure && m.state == State::Unlisted))
        .map(|(k, m)| (k.clone(), Stamp { seq: m.seq, by: m.by.clone() }))
        .collect()
}

/// [`stamps_of`] for an instance's code, off the UI thread.
pub fn stamps(inst: &Instance, copy: &PackCopy, now: &Share) -> BTreeMap<String, Stamp> {
    let found = projects_of(inst, &[now]);
    stamps_of(&copy.marks, &mods(now, lookup(&found)))
}

/// A code another member sent, kept until it's taken in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Got {
    pub code: String,
    pub seq: u64,
    pub at: u64,
    pub name: String,
    /// When each mod got the way this code has it (see [`Stamp`]); empty
    /// from an older Hopper or mailbox.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub stamps: BTreeMap<String, Stamp>,
}

/// One copy of a group's pack on this PC.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PackCopy {
    /// The pack's id (the instance id it was first shared from) and the
    /// group it syncs through.
    pub inst: String,
    pub group: String,
    /// The local instance's id.
    pub local: String,
    /// Each member's last code taken in, by mailbox id.
    pub got: BTreeMap<String, Got>,
    /// Each member's newest code not taken in yet.
    pub waiting: BTreeMap<String, Got>,
    /// This copy's last published code and its number.
    pub sent: String,
    pub sent_seq: u64,
    /// Who changed each mod last (by key).
    pub marks: BTreeMap<String, Mark>,
    /// The mods whose settings the group shares, and the settings changed
    /// (`configsync`).
    #[serde(skip_serializing_if = "crate::configsync::Configs::is_empty")]
    pub configs: crate::configsync::Configs,
}

impl PackCopy {
    /// A member's code arrived; an older one than already known is ignored.
    pub fn arrive(&mut self, from: &str, g: Got) {
        let known = self.waiting.get(from).or_else(|| self.got.get(from)).map_or(0, |x| x.seq);
        if g.seq > known || (g.seq == known && self.got.get(from).is_some_and(|x| x.code != g.code)) {
            self.waiting.insert(from.to_string(), g);
        }
    }

    /// The waiting codes, oldest first.
    pub fn queue(&self) -> Vec<(String, Got)> {
        let mut v: Vec<(String, Got)> = self.waiting.iter().map(|(k, g)| (k.clone(), g.clone())).collect();
        v.sort_by_key(|(_, g)| g.seq);
        v
    }

    /// The mailbox lost its data and numbers its events from 1 again. The
    /// numbers this copy kept are from before, so every new code would look
    /// older than what's known and be dropped; they start over too. The
    /// codes stay: they are still what each member last had.
    pub fn relay_reset(&mut self) {
        for g in self.got.values_mut().chain(self.waiting.values_mut()) {
            g.seq = 0;
            g.stamps.clear();
        }
        for m in self.marks.values_mut() {
            m.seq = 0;
        }
        self.configs.relay_reset();
        self.sent_seq = 0;
    }
}

/// What taking in the waiting codes did.
#[derive(Debug, Default)]
pub struct Merged {
    /// Members whose code was taken in fully (by their seq).
    pub taken: Vec<(String, Got)>,
    /// Marks to set.
    pub marks: Vec<(String, Mark)>,
    /// Changes made, for the task's notes: (mod, kind, by name).
    pub made: Vec<(String, Kind, String)>,
    /// Some change waits for this copy's own unsent change.
    pub held: bool,
}

/// Decides each waiting code's changes against this copy (`here`: its
/// mods now, `sent`: as last published), oldest first, and returns the
/// changes to make. `decode` reads a code into mods (None: unreadable,
/// then that code is dropped). `give_up_held`: a change made here has
/// waited too long for its publish; the others' changes to that mod are
/// dropped (the change here goes out later, so it wins anyway) and their
/// codes are done with.
///
/// A change is dated by its stamp when the code has one (the member only
/// passed it on), else by the code's own number (the member made it).
/// A member's first code has nothing to compare with; only its stamped
/// mods are taken from it, since an unstamped difference may just be that
/// member being behind.
pub fn plan(
    copy: &PackCopy,
    here: &Mods,
    sent: Option<&Mods>,
    give_up_held: bool,
    mut decode: impl FnMut(&str) -> Option<Mods>,
) -> (Vec<Change>, Merged) {
    let mut out = Merged::default();
    let mut todo: Vec<Change> = Vec::new();
    let mut marks = copy.marks.clone();
    // What this copy will be once the changes so far are made.
    let mut will: HashMap<String, State> = HashMap::new();
    let state_of = |m: &Mods, k: &str| m.map.get(k).map(|(s, _)| s.clone()).unwrap_or_default();
    // A member's name by id, from what this copy got from them.
    let name_of = |id: &str| -> String {
        copy.got
            .get(id)
            .or_else(|| copy.waiting.get(id))
            .map(|g| g.name.clone())
            .filter(|n| !n.is_empty())
            .or_else(|| copy.marks.values().find(|m| m.by == id && !m.name.is_empty()).map(|m| m.name.clone()))
            .unwrap_or_default()
    };
    for (from, g) in copy.queue() {
        let Some(now) = decode(&g.code) else {
            out.taken.push((from, g));
            continue;
        };
        let found: Vec<Change> = match copy.got.get(&from).and_then(|p| decode(&p.code)) {
            Some(prev) => changes(&prev, &now),
            None => g
                .stamps
                .keys()
                .filter_map(|k| {
                    let was = will.get(k).cloned().unwrap_or_else(|| state_of(here, k));
                    let to = state_of(&now, k);
                    (was != to).then(|| Change { key: k.clone(), was, now: to, entry: now.map.get(k).map(|(_, e)| e.clone()) })
                })
                .collect(),
        };
        let mut held = false;
        for c in found {
            let cur = will.get(&c.key).cloned().unwrap_or_else(|| state_of(here, &c.key));
            let unsent = sent.is_some_and(|s| state_of(s, &c.key) != state_of(here, &c.key));
            let stamp = g.stamps.get(&c.key).filter(|s| s.seq > 0 && s.seq <= g.seq);
            let (seq, by, name) = match stamp {
                Some(s) if s.by == from => (s.seq, from.clone(), g.name.clone()),
                Some(s) => (s.seq, s.by.clone(), name_of(&s.by)),
                None => (g.seq, from.clone(), g.name.clone()),
            };
            let mut v = verdict(&c, seq, marks.get(&c.key), &cur, unsent);
            // Passed on: the change is already known here at that number
            // or later, whatever this copy has made of it since.
            if stamp.is_some() && v != Verdict::Same && marks.get(&c.key).is_some_and(|m| m.seq >= seq) {
                v = Verdict::Older;
            }
            match v {
                Verdict::Apply => {
                    let kind = c.kind();
                    let m = Mark { state: c.now.clone(), kind, by, name: name.clone(), seq, at: g.at };
                    marks.insert(c.key.clone(), m.clone());
                    out.marks.push((c.key.clone(), m));
                    out.made.push((c.key.clone(), kind, name));
                    will.insert(c.key.clone(), c.now.clone());
                    todo.retain(|t| t.key != c.key);
                    todo.push(c);
                }
                Verdict::Held if !give_up_held => held = true,
                Verdict::Held | Verdict::Same | Verdict::Older => {}
            }
        }
        if held {
            out.held = true;
        } else {
            out.taken.push((from, g));
        }
    }
    (todo, out)
}

/// Whose a code's file is: from the instance's own mods when it has that
/// file, else asked from CurseForge or Modrinth (failures give None).
pub fn projects_of(inst: &Instance, shares: &[&Share]) -> HashMap<String, u64> {
    let mut out: HashMap<String, u64> = HashMap::new();
    let mut ask_cf: Vec<u64> = Vec::new();
    for s in shares {
        for e in &s.entries {
            match e {
                Entry::CurseFile { file, .. } => {
                    match inst.addons.iter().find(|a| a.file_id == *file && !is_mr(a.project_id)) {
                        Some(a) => {
                            out.insert(format!("cf:{file}"), a.project_id);
                        }
                        None => ask_cf.push(*file),
                    }
                }
                Entry::ModrinthFile { version, .. } => {
                    let k = format!("mr:{version}");
                    if out.contains_key(&k) {
                        continue;
                    }
                    if let Some(a) = inst.addons.iter().find(|a| a.mr_version == *version || (is_mr(a.project_id) && a.file_id == num_id(version))) {
                        out.insert(k, a.project_id);
                    } else if let Ok(v) = crate::modrinth::version(version) {
                        out.insert(k, num_id(&v.project_id));
                    }
                }
                _ => {}
            }
        }
    }
    ask_cf.sort_unstable();
    ask_cf.dedup();
    for chunk in ask_cf.chunks(500) {
        if let Ok(files) = crate::cf::get_files(chunk) {
            for f in files {
                out.insert(format!("cf:{}", f.id), f.mod_id);
            }
        }
    }
    out
}

/// The lookup [`mods`] wants, from [`projects_of`]'s answers.
pub fn lookup(found: &HashMap<String, u64>) -> impl FnMut(&Entry) -> Option<u64> + '_ {
    move |e| match e {
        Entry::CurseFile { file, .. } => found.get(&format!("cf:{file}")).copied(),
        Entry::ModrinthFile { version, .. } => found.get(&format!("mr:{version}")).copied(),
        _ => None,
    }
}

/// Makes the changes in the instance. A pack mod put back to the pack's
/// own file can't be done from a change alone (the pack's file isn't in
/// the code); it is noted. A change that fails doesn't stop the others,
/// but fails the whole: tried again, the ones made are already there.
pub fn apply(inst: &mut Instance, todo: &[Change], skip_sources: &[crate::sources::carry::Pin], job: &Job) -> anyhow::Result<()> {
    let mut failed: Vec<(String, anyhow::Error)> = Vec::new();
    for (n, c) in todo.iter().enumerate() {
        job.check_cancelled()?;
        job.progress(n, todo.len());
        let at = find_addon(inst, &c.key);
        let name = at.map(|i| inst.addons[i].name.clone()).unwrap_or_else(|| c.key.clone());
        let r: anyhow::Result<()> = match (&c.now, &c.entry) {
            (State::Off, _) => match at {
                Some(i) => switch(inst, i, false, &name, job),
                None => Ok(()),
            },
            (State::Removed, _) => match at {
                Some(i) => crate::install::remove(inst, i),
                None => Ok(()),
            },
            (State::Unlisted, _) => match (&c.was, at) {
                (State::Off, Some(i)) => switch(inst, i, true, &name, job),
                (State::File { .. }, Some(i)) if !inst.addons[i].from_pack => crate::install::remove(inst, i),
                (State::File { .. } | State::Removed, _) => {
                    job.note(format!("{name}: putting back the modpack's own file isn't done by sync"));
                    Ok(())
                }
                _ => Ok(()),
            },
            (State::File { on, .. }, Some(e)) => file_change(inst, at, e, *on, skip_sources, job),
            (State::File { .. }, None) => Ok(()),
        };
        if let Err(e) = r {
            failed.push((name, e));
        }
    }
    inst.save()?;
    match failed.into_iter().next() {
        None => Ok(()),
        Some((name, e)) => Err(e.context(format!("Couldn't change {name}"))),
    }
}

/// Turns a mod on or off. A jar the running game holds open is switched
/// once the game closes; the task says so, rather than reading as done.
fn switch(inst: &mut Instance, i: usize, on: bool, name: &str, job: &Job) -> anyhow::Result<()> {
    crate::install::set_enabled_later(inst, i, on)?;
    if inst.addons.get(i).is_some_and(|a| a.enabled != on) {
        let what = if on { "turns on" } else { "turns off" };
        job.note(format!("{name} {what} when the game closes"));
    }
    Ok(())
}

/// Whether making `c` here would change anything: a switch, removal or
/// put-back of a mod this instance doesn't have does nothing, and isn't
/// told as a change.
pub fn does_something(inst: &Instance, c: &Change) -> bool {
    let here = find_addon(inst, &c.key);
    match (&c.now, &c.entry) {
        (State::Off | State::Removed, _) => here.is_some(),
        (State::Unlisted, _) => here.is_some(),
        (State::File { .. }, Some(_)) => true,
        (State::File { .. }, None) => false,
    }
}

/// Claims each change for History ([`crate::actor`]) as the member's whose
/// change won, with the file it replaces here.
fn claim_changes(inst: &Instance, todo: &[Change], merged: &Merged, group: &str) {
    for c in todo {
        let Some((_, m)) = merged.marks.iter().rev().find(|(k, _)| *k == c.key) else { continue };
        let who = crate::actor::Who::friend(&m.name, &m.by, group, "");
        let file = find_addon(inst, &c.key).map(|i| inst.addons[i].file_name.as_str());
        crate::actor::claim(&c.key, file, &who);
    }
}

/// Takes the waiting codes into the instance at `root`: works out each
/// member's changes and makes the ones that win here. Runs off the UI
/// thread. `allow`: mods from other links may be put in by themselves.
/// `ignore_unsent`: a change here waited for its publish too long; the
/// other members' changes to that mod are dropped and the rest go in.
pub fn merge(root: &Path, copy: &PackCopy, allow: bool, ignore_unsent: bool, job: &Job) -> anyhow::Result<Merged> {
    // Held to the end: an update that finished meanwhile would otherwise be
    // undone by this older copy, with its new jar left behind unlisted.
    let _edit = crate::instances::edit_lock(root);
    let mut inst = crate::instances::load(root).with_context(|| format!("{} isn't an instance any more", root.display()))?;
    if inst.unreadable.is_some() {
        if crate::instances::may_read_later(&inst) {
            anyhow::bail!("Another program has this instance's files open.");
        }
        anyhow::bail!("{}'s settings file is damaged.", inst.name);
    }
    let here = crate::sharecode::make(&inst, job)?.share;
    let read = |c: &str| (!c.is_empty()).then(|| crate::sharecode::decode(c).ok()).flatten();
    let sent = read(&copy.sent);
    let theirs: Vec<Share> = copy.got.values().chain(copy.waiting.values()).filter_map(|g| read(&g.code)).collect();
    let mut all: Vec<&Share> = vec![&here];
    all.extend(sent.iter());
    all.extend(theirs.iter());
    let found = projects_of(&inst, &all);
    let here = mods(&here, lookup(&found));
    let sent = sent.map(|s| mods(&s, lookup(&found)));
    let (todo, mut merged) = plan(copy, &here, sent.as_ref(), ignore_unsent, |c| read(c).map(|s| mods(&s, lookup(&found))));
    // Mods kept only on this PC only learn how the others have them.
    let (mut todo, kept) = crate::onlyhere::split(&mut inst, todo, &mut merged);
    // A change to a mod this copy doesn't have (an echo of a removal, a
    // switch of a mod removed here) changes nothing; the task only tells
    // what really changes. Its mark stays, so who changed it last still
    // counts.
    todo.retain(|c| {
        let keep = does_something(&inst, c);
        if !keep {
            merged.made.retain(|(k, _, _)| *k != c.key);
        }
        keep
    });
    if kept && todo.is_empty() {
        inst.save()?;
    }
    if !todo.is_empty() {
        // Only a merge that changes something shows in Tasks; most codes
        // are echoes of changes already here.
        job.show();
        let who: Vec<&str> = merged.made.iter().map(|(_, _, n)| n.as_str()).filter(|n| !n.is_empty()).collect();
        if let Some(first) = who.first() {
            job.detail(format!("{} change(s) from {first}", todo.len()));
        }
        let pins: Vec<crate::sources::carry::Pin> = todo
            .iter()
            .filter_map(|c| match &c.entry {
                Some(Entry::Source { pin, .. }) => Some(pin.clone()),
                _ => None,
            })
            .collect();
        let skip = crate::sources::carry::auto_skip(&pins, allow);
        // History names the member behind each change; only these mods
        // are theirs, whatever else changes here meanwhile.
        let _scope = crate::actor::begin(root, None);
        claim_changes(&inst, &todo, &merged, &copy.group);
        apply(&mut inst, &todo, &skip, job)?;
    }
    Ok(merged)
}

/// The group packs "Keep everyone in step" follows here as new instances:
/// each pack a member follows in a group this PC joined and keeps in step,
/// with its group's id and name, once its code has come (`recipes`: the
/// packs friends share with this PC). Left out: this PC's own packs, ones
/// it stopped following in that group, ones `busy` says are being followed
/// right now (`"<from>/<inst>"`), and ones something here follows already.
/// A follow counts from the moment its instance is made, before its first
/// version is in, so a pack whose install failed, was cancelled or was cut
/// short by Hopper closing isn't installed again as another copy.
pub fn to_follow(
    st: &Settings,
    groups: &[crate::mailbox::Group],
    recipes: &[crate::mailbox::Event],
    me: &str,
    busy: impl Fn(&str) -> bool,
) -> Vec<(crate::mailbox::Event, String, String)> {
    let mut out: Vec<(crate::mailbox::Event, String, String)> = Vec::new();
    for g in groups.iter().filter(|g| g.me.joined && g.me.in_step) {
        let stopped = st.friend_groups.iter().find(|x| x.id == g.id).map(|x| x.stopped.as_slice()).unwrap_or_default();
        for f in &g.follows {
            let key = format!("{}/{}", f.from, f.inst);
            if f.from == me
                || stopped.contains(&key)
                || busy(&key)
                || out.iter().any(|(e, _, _)| e.from == f.from && e.inst == f.inst)
            {
                continue;
            }
            // Its code hasn't arrived yet; the next look gets it.
            let Some(e) = recipes.iter().find(|e| e.from == f.from && e.inst == f.inst) else { continue };
            // A follow from before ids names the pack by its folder name.
            if !st.followers(&e.from, &e.inst, &e.key).is_empty() {
                continue;
            }
            out.push((e.clone(), g.id.clone(), g.name.clone()));
        }
    }
    out
}

/// The group packs this PC keeps the same as the group: each pack it
/// shares into a group it's in, and each group pack it follows once the
/// follow's first version is in (`groups_of`: the groups a followed pack
/// came through). One copy per pack. Copies already kept stay as they
/// are; a new follow's copy starts from the version it applied last.
/// Returns the copies that stopped (the share turned off, the follow
/// ended or its instance deleted).
pub fn reconcile(copies: &mut Vec<PackCopy>, st: &Settings, groups_of: impl Fn(&FollowedRecipe) -> Vec<String>) -> Vec<PackCopy> {
    let joined: Vec<String> = st.friend_groups.iter().map(|g| g.id.clone()).collect();
    let mut want: Vec<PackCopy> = Vec::new();
    for s in &st.shared_with {
        if s.group.is_empty() || s.inst.is_empty() || !joined.contains(&s.group) || want.iter().any(|c| c.inst == s.inst) {
            continue;
        }
        want.push(PackCopy { inst: s.inst.clone(), group: s.group.clone(), local: s.inst.clone(), ..Default::default() });
    }
    for f in &st.followed_recipes {
        if !f.ended.is_empty()
            || f.local.is_empty()
            || f.inst.is_empty()
            || f.code.is_empty()
            || want.iter().any(|c| c.inst == f.inst || c.local == f.local)
        {
            continue;
        }
        // A copy kept already stays in its group while that group is still
        // joined: on a start the feed hasn't listed the pack again yet, and
        // dropping the copy then loses what it got from each member (their
        // next codes would be taken as starting points, their changes lost).
        let group = Some(f.group.clone())
            .filter(|g| joined.contains(g))
            .or_else(|| groups_of(f).into_iter().find(|g| joined.contains(g)))
            .or_else(|| {
                copies.iter().find(|c| c.inst == f.inst && c.local == f.local).map(|c| c.group.clone()).filter(|g| joined.contains(g))
            });
        let Some(group) = group else { continue };
        let mut c = PackCopy { inst: f.inst.clone(), group, local: f.local.clone(), ..Default::default() };
        c.got.insert(f.peer_id.clone(), Got { code: f.code.clone(), seq: 0, at: f.at, name: f.peer_name.clone(), ..Default::default() });
        want.push(c);
    }
    let old = std::mem::take(copies);
    let mut gone = Vec::new();
    for o in old {
        if want.iter().any(|w| w.inst == o.inst && w.local == o.local) {
            copies.push(o);
        } else {
            gone.push(o);
        }
    }
    for w in want {
        match copies.iter_mut().find(|c| c.inst == w.inst && c.local == w.local) {
            Some(c) => c.group = w.group,
            None => copies.push(w),
        }
    }
    gone
}

fn file_change(
    inst: &mut Instance,
    at: Option<usize>,
    e: &Entry,
    on: bool,
    skip_sources: &[crate::sources::carry::Pin],
    job: &Job,
) -> anyhow::Result<()> {
    let (project, file) = match e {
        Entry::CurseFile { file, .. } => {
            let p = match at {
                Some(i) => inst.addons[i].project_id,
                None => crate::cf::get_files(&[*file])?.first().map(|f| f.mod_id).ok_or_else(|| anyhow::anyhow!("CurseForge has no file {file}"))?,
            };
            (p, *file)
        }
        Entry::ModrinthFile { version, .. } => {
            let v = crate::modrinth::version(version)?;
            (num_id(&v.project_id), num_id(&v.id))
        }
        Entry::Source { pin, enabled } => {
            if skip_sources.iter().any(|s| s.source.url == pin.source.url && s.source.pattern == pin.source.pattern) {
                return Ok(());
            }
            crate::sources::install_pinned(inst, pin, *enabled, job)?;
            return Ok(());
        }
        _ => return Ok(()),
    };
    let same = at.is_some_and(|i| inst.addons[i].file_id == file && inst.addons[i].project_id == project);
    if !same {
        crate::install::install_file(inst, project, file, job)?;
    }
    if let Some(i) = inst.addons.iter().position(|a| a.project_id == project)
        && inst.addons[i].enabled != on
    {
        crate::install::set_enabled_later(inst, i, on)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sharecode::Base;

    fn share(entries: Vec<Entry>) -> Share {
        Share { base: Base::Plain { game_version: "1.21.1".into(), loader: "neoforge".into() }, name: None, loader: None, entries }
    }
    fn cf(file: u64, on: bool) -> Entry {
        Entry::CurseFile { file, enabled: on }
    }
    /// File n belongs to project n / 10 (11 and 12 are two files of mod 1).
    fn m(entries: Vec<Entry>) -> Mods {
        mods(&share(entries), |e| match e {
            Entry::CurseFile { file, .. } => Some(file / 10),
            _ => None,
        })
    }
    fn st(file: u64, on: bool) -> State {
        State::File { file: format!("cf:{file}"), on }
    }
    fn code(entries: Vec<Entry>) -> String {
        crate::sharecode::encode(&share(entries))
    }
    fn dec(c: &str) -> Option<Mods> {
        crate::sharecode::decode(c).ok().map(|s| {
            mods(&s, |e| match e {
                Entry::CurseFile { file, .. } => Some(file / 10),
                _ => None,
            })
        })
    }
    fn got(c: String, seq: u64, name: &str) -> Got {
        Got { code: c, seq, at: seq, name: name.into(), ..Default::default() }
    }

    #[test]
    fn each_change_type_is_one_mod_with_its_kind() {
        let was = m(vec![cf(11, true), cf(21, true), cf(31, true), cf(41, false)]);
        let now = m(vec![cf(12, true), cf(21, false), cf(41, true), cf(51, true)]);
        let mut got: Vec<(String, Kind)> = changes(&was, &now).into_iter().map(|c| (c.key.clone(), c.kind())).collect();
        got.sort();
        assert_eq!(
            got,
            [
                ("p:1".to_string(), Kind::Updated),
                ("p:2".to_string(), Kind::Off),
                ("p:3".to_string(), Kind::Removed),
                ("p:4".to_string(), Kind::On),
                ("p:5".to_string(), Kind::Added),
            ]
        );
        // Pack mods: turned off, back on, taken out.
        let was = m(vec![Entry::Off(Target::Curse(7))]);
        let now = m(vec![Entry::Remove(Target::Curse(8))]);
        let mut got: Vec<(String, Kind)> = changes(&was, &now).into_iter().map(|c| (c.key.clone(), c.kind())).collect();
        got.sort();
        assert_eq!(got, [("p:7".to_string(), Kind::On), ("p:8".to_string(), Kind::Removed)]);
    }

    #[test]
    fn a_file_that_could_not_be_looked_up_is_not_called_removed() {
        let was = m(vec![cf(11, true), cf(21, true)]);
        let now = mods(&share(vec![cf(11, true), cf(21, true)]), |e| match e {
            Entry::CurseFile { file: 21, .. } => None,
            Entry::CurseFile { file, .. } => Some(file / 10),
            _ => None,
        });
        assert!(now.unsure);
        assert!(changes(&was, &now).is_empty());
    }

    #[test]
    fn the_later_change_of_a_mod_wins_and_echoes_change_nothing() {
        let c = Change { key: "p:2".into(), was: st(21, true), now: st(21, false), entry: None };
        let mark = Mark { state: st(21, true), seq: 50, ..Default::default() };
        // Newer than the mod's last change: made here.
        assert_eq!(verdict(&c, 60, Some(&mark), &st(21, true), false), Verdict::Apply);
        // Older: lost to the change already here.
        assert_eq!(verdict(&c, 40, Some(&mark), &st(21, true), false), Verdict::Older);
        // Already off here (an echo of a change made here): nothing.
        assert_eq!(verdict(&c, 60, Some(&mark), &st(21, false), false), Verdict::Same);
        // Turned back on here and not sent yet: the change here goes out
        // later, so it wins; looked at again after it's sent.
        assert_eq!(verdict(&c, 60, None, &st(21, true), true), Verdict::Held);
    }

    #[test]
    fn two_members_changing_different_mods_at_once_both_land() {
        // The pack: mods 1 and 2 on. Mia turns 1 off (seq 10) while Sam
        // turns 2 off (seq 11); each code still has the other's mod on.
        let base = vec![cf(11, true), cf(21, true)];
        let mut copy = PackCopy::default();
        copy.got.insert("mia".into(), got(code(base.clone()), 1, "Mia"));
        copy.got.insert("sam".into(), got(code(base.clone()), 2, "Sam"));
        copy.arrive("sam", got(code(vec![cf(11, true), cf(21, false)]), 11, "Sam"));
        copy.arrive("mia", got(code(vec![cf(11, false), cf(21, true)]), 10, "Mia"));
        let here = m(base.clone());
        let (todo, merged) = plan(&copy, &here, Some(&here), false, dec);
        let mut keys: Vec<(String, State)> = todo.iter().map(|c| (c.key.clone(), c.now.clone())).collect();
        keys.sort();
        assert_eq!(keys, [("p:1".to_string(), st(11, false)), ("p:2".to_string(), st(21, false))]);
        assert_eq!(merged.taken.len(), 2);
        assert!(!merged.held);
        let by: Vec<(String, String)> = merged.marks.iter().map(|(k, mk)| (k.clone(), mk.name.clone())).collect();
        assert!(by.contains(&("p:1".into(), "Mia".into())) && by.contains(&("p:2".into(), "Sam".into())));
    }

    #[test]
    fn two_members_changing_the_same_mod_end_the_same_everywhere() {
        // Mia turns 2 off (seq 10), Sam turns it to a newer file (seq 11).
        let base = vec![cf(21, true)];
        let mia = code(vec![cf(21, false)]);
        let sam = code(vec![cf(22, true)]);
        let start = |copy: &mut PackCopy| {
            copy.got.insert("mia".into(), got(code(base.clone()), 1, "Mia"));
            copy.got.insert("sam".into(), got(code(base.clone()), 2, "Sam"));
        };
        // A third PC gets both: Sam's is later, so Sam's file, on.
        let mut third = PackCopy::default();
        start(&mut third);
        third.arrive("mia", got(mia.clone(), 10, "Mia"));
        third.arrive("sam", got(sam.clone(), 11, "Sam"));
        let (todo, _) = plan(&third, &m(base.clone()), None, false, dec);
        assert_eq!(todo.last().map(|c| c.now.clone()), Some(st(22, true)));
        // Mia's PC (already off, her mark at 10) takes Sam's later change.
        let mut on_mia = PackCopy::default();
        start(&mut on_mia);
        on_mia.got.remove("mia");
        on_mia.marks.insert("p:2".into(), Mark { state: st(21, false), seq: 10, by: "mia".into(), ..Default::default() });
        on_mia.arrive("sam", got(sam.clone(), 11, "Sam"));
        let here = m(vec![cf(21, false)]);
        let (todo, _) = plan(&on_mia, &here, Some(&here), false, dec);
        assert_eq!(todo.iter().map(|c| c.now.clone()).collect::<Vec<_>>(), [st(22, true)]);
        // Sam's PC (his mark at 11) drops Mia's earlier one.
        let mut on_sam = PackCopy::default();
        start(&mut on_sam);
        on_sam.got.remove("sam");
        on_sam.marks.insert("p:2".into(), Mark { state: st(22, true), seq: 11, by: "sam".into(), ..Default::default() });
        on_sam.arrive("mia", got(mia, 10, "Mia"));
        let here = m(vec![cf(22, true)]);
        let (todo, merged) = plan(&on_sam, &here, Some(&here), false, dec);
        assert!(todo.is_empty());
        assert_eq!(merged.taken.len(), 1, "Mia's code is done with, not kept waiting");
    }

    #[test]
    fn a_member_whose_copy_is_behind_undoes_nothing() {
        // Sam turned 1 off (seq 10). Mia hadn't got it yet and added mod 3
        // (seq 12): her code still has 1 on, but she didn't change 1.
        let base = vec![cf(11, true)];
        let mut copy = PackCopy::default();
        copy.got.insert("mia".into(), got(code(base.clone()), 1, "Mia"));
        copy.marks.insert("p:1".into(), Mark { state: st(11, false), seq: 10, by: "sam".into(), ..Default::default() });
        copy.arrive("mia", got(code(vec![cf(11, true), cf(31, true)]), 12, "Mia"));
        let here = m(vec![cf(11, false)]);
        let (todo, _) = plan(&copy, &here, Some(&here), false, dec);
        assert_eq!(todo.iter().map(|c| c.key.clone()).collect::<Vec<_>>(), ["p:3"]);
    }

    #[test]
    fn a_change_made_here_and_not_sent_yet_holds_the_code_back() {
        let base = vec![cf(11, true)];
        let mut copy = PackCopy::default();
        copy.got.insert("mia".into(), got(code(base.clone()), 1, "Mia"));
        copy.arrive("mia", got(code(vec![cf(11, false)]), 10, "Mia"));
        // Turned to the newer file here a moment ago; sent still has 11.
        let here = m(vec![cf(12, true)]);
        let sent = m(base.clone());
        let (todo, merged) = plan(&copy, &here, Some(&sent), false, dec);
        assert!(todo.is_empty());
        assert!(merged.held && merged.taken.is_empty(), "kept waiting for the publish");
        // Once sent (seq 15, marked as ours), Mia's older change loses.
        let mut copy2 = copy.clone();
        let mut marks = copy2.marks.clone();
        mark_own(&mut marks, Some(&sent), &here, "me", 15, 15);
        copy2.marks = marks;
        let (todo, merged) = plan(&copy2, &here, Some(&here), false, dec);
        assert!(todo.is_empty());
        assert_eq!(merged.taken.len(), 1);
    }

    #[test]
    fn own_changes_are_marked_but_echoes_keep_who_made_them() {
        let mut marks = BTreeMap::new();
        marks.insert("p:1".into(), Mark { state: st(11, false), seq: 10, by: "mia".into(), name: "Mia".into(), ..Default::default() });
        let prev = m(vec![cf(11, true), cf(21, true)]);
        let now = m(vec![cf(11, false), cf(21, false)]);
        mark_own(&mut marks, Some(&prev), &now, "me", 20, 20);
        assert_eq!(marks["p:1"].by, "mia", "applied from Mia: still hers");
        assert_eq!((marks["p:2"].by.as_str(), marks["p:2"].seq, marks["p:2"].kind), ("me", 20, Kind::Off));
        // Nothing sent before: no baseline, nothing claimed.
        let mut fresh = BTreeMap::new();
        mark_own(&mut fresh, None, &now, "me", 21, 21);
        assert!(fresh.is_empty());
    }

    #[test]
    fn a_first_code_from_a_member_is_only_the_starting_point() {
        let mut copy = PackCopy::default();
        copy.arrive("mia", got(code(vec![cf(11, false)]), 5, "Mia"));
        let here = m(vec![cf(11, true)]);
        let (todo, merged) = plan(&copy, &here, Some(&here), false, dec);
        assert!(todo.is_empty());
        assert_eq!(merged.taken.len(), 1);
        // An old code replayed after a newer one was taken is ignored.
        let mut copy = PackCopy::default();
        copy.got.insert("mia".into(), got(code(vec![cf(11, false)]), 9, "Mia"));
        copy.arrive("mia", got(code(vec![cf(11, true)]), 5, "Mia"));
        assert!(copy.waiting.is_empty());
    }

    #[test]
    fn the_packs_kept_both_ways_are_group_shares_and_group_follows() {
        use crate::settings::{FriendGroup, SharedPack};
        let mut st = Settings::default();
        st.friend_groups.push(FriendGroup { id: "g".into(), ..Default::default() });
        st.shared_with.push(SharedPack { inst: "mine".into(), group: "g".into(), ..Default::default() });
        st.shared_with.push(SharedPack { inst: "solo".into(), friend: "f".into(), ..Default::default() });
        let follow = |inst: &str, local: &str, group: &str, code: &str| FollowedRecipe {
            peer_id: "o".into(),
            inst: inst.into(),
            local: local.into(),
            group: group.into(),
            code: code.into(),
            ..Default::default()
        };
        // Kept in step; came through the group; from a friend directly;
        // and one whose first version isn't in yet.
        st.followed_recipes.push(follow("p1", "l1", "g", "c1"));
        st.followed_recipes.push(follow("p2", "l2", "", "c2"));
        st.followed_recipes.push(follow("p3", "l3", "", "c3"));
        st.followed_recipes.push(follow("p4", "l4", "g", ""));
        let via = |f: &FollowedRecipe| if f.inst == "p2" { vec!["g".to_string()] } else { Vec::new() };
        let mut copies = Vec::new();
        assert!(reconcile(&mut copies, &st, via).is_empty());
        let mut kept: Vec<(&str, &str)> = copies.iter().map(|c| (c.inst.as_str(), c.local.as_str())).collect();
        kept.sort();
        assert_eq!(kept, [("mine", "mine"), ("p1", "l1"), ("p2", "l2")]);
        assert_eq!(copies.iter().find(|c| c.inst == "p1").unwrap().got["o"].code, "c1");
        // What a copy learned stays; one whose follow ended goes.
        copies.iter_mut().find(|c| c.inst == "p1").unwrap().sent = "sent".into();
        st.followed_recipes[1].ended = "deleted".into();
        let gone = reconcile(&mut copies, &st, via);
        assert_eq!(gone.iter().map(|c| c.inst.as_str()).collect::<Vec<_>>(), ["p2"]);
        assert_eq!(copies.iter().find(|c| c.inst == "p1").unwrap().sent, "sent");
    }

    #[test]
    fn says_who_did_it() {
        assert_eq!(Kind::Off.says("Mia"), "Turned off by Mia");
        assert_eq!(Kind::Updated.says("Sam"), "Updated by Sam");
    }

    // ---- A group in a box --------------------------------------------
    //
    // Three PCs and a relay that keeps one code per sender and pack for
    // each member (a newer one replaces the older), numbering every event
    // in one order. Each PC's mods are project -> (file, on); file n is of
    // project n / 10. The steps are the desktop's: publish (only when the
    // code changed, marking its own changes), read the feed (`arrive`),
    // merge (`plan`, then the changes made), and a start (the copy as saved,
    // set up again by `reconcile` before the feed is read).

    const PACK: &str = "pack";
    const GROUP: &str = "g";

    struct Pc {
        id: String,
        mods: BTreeMap<u64, (u64, bool)>,
        copy: PackCopy,
        /// The follow's applied code (members only).
        follow_code: String,
        cursor: u64,
        /// Merges that changed something, with their changes.
        merges: Vec<Vec<Change>>,
    }

    #[derive(Default)]
    struct Relay {
        seq: u64,
        /// Per member: (seq, from, code).
        queues: BTreeMap<String, Vec<(u64, String, Got)>>,
    }

    impl Pc {
        fn new(id: &str, sharer: bool, start: &[(u64, u64, bool)]) -> Pc {
            let local = if sharer { PACK.to_string() } else { format!("{id}-local") };
            let mods = start.iter().map(|(p, f, on)| (*p, (*f, *on))).collect();
            let copy = PackCopy { inst: PACK.into(), group: GROUP.into(), local, ..Default::default() };
            Pc { id: id.into(), mods, copy, follow_code: String::new(), cursor: 0, merges: Vec::new() }
        }
        fn entries(&self) -> Vec<Entry> {
            self.mods.values().map(|(f, on)| cf(*f, *on)).collect()
        }
        fn code(&self) -> String {
            code(self.entries())
        }
        fn set(&mut self, project: u64, file: u64, on: bool) {
            self.mods.insert(project, (file, on));
        }
        /// Publishes this copy if its code changed since the last time.
        fn publish(&mut self, relay: &mut Relay, all: &[&str], stamped: bool) {
            let now = self.code();
            if now == self.copy.sent {
                return;
            }
            let now_mods = m(self.entries());
            let stamps = if stamped { stamps_of(&self.copy.marks, &now_mods) } else { BTreeMap::new() };
            for to in all.iter().filter(|t| **t != self.id) {
                relay.seq += 1;
                let q = relay.queues.entry(to.to_string()).or_default();
                q.retain(|(_, from, _)| *from != self.id);
                let g = Got { code: now.clone(), seq: relay.seq, at: relay.seq, name: self.id.clone(), stamps: stamps.clone() };
                q.push((relay.seq, self.id.clone(), g));
            }
            let seq = relay.seq;
            // As `sent_marks` and `publish_done` do.
            let prev = dec(&self.copy.sent);
            let mut marks = self.copy.marks.clone();
            mark_own(&mut marks, prev.as_ref(), &now_mods, &self.id, seq, seq);
            for (k, mk) in marks {
                if self.copy.marks.get(&k).is_none_or(|x| x.seq <= mk.seq && x.state != mk.state) {
                    self.copy.marks.insert(k, mk);
                }
            }
            self.copy.sent = now;
            self.copy.sent_seq = seq;
        }
        fn read(&mut self, relay: &Relay) {
            let mut q = relay.queues.get(&self.id).cloned().unwrap_or_default();
            q.sort_by_key(|(s, _, _)| *s);
            let after = self.cursor;
            for (seq, from, g) in q.into_iter().filter(|(s, _, _)| *s > after) {
                self.copy.arrive(&from, g);
                self.cursor = self.cursor.max(seq);
            }
        }
        fn merge(&mut self) {
            if self.copy.waiting.is_empty() {
                return;
            }
            let here = m(self.entries());
            let sent = dec(&self.copy.sent);
            let (todo, merged) = plan(&self.copy, &here, sent.as_ref(), false, dec);
            for c in &todo {
                let p: u64 = c.key.trim_start_matches("p:").parse().unwrap();
                match &c.now {
                    State::File { file, on } => {
                        let f: u64 = file.trim_start_matches("cf:").parse().unwrap();
                        self.mods.insert(p, (f, *on));
                    }
                    _ => {
                        self.mods.remove(&p);
                    }
                }
            }
            // As `on_group_merged` does.
            for (from, g) in merged.taken {
                if self.copy.waiting.get(&from).is_some_and(|w| w.seq == g.seq) {
                    self.copy.waiting.remove(&from);
                }
                if from == "A" && self.copy.local != self.copy.inst {
                    self.follow_code = g.code.clone();
                }
                self.copy.got.insert(from, g);
            }
            for (k, mk) in merged.marks {
                if self.copy.marks.get(&k).is_none_or(|x| x.seq <= mk.seq) {
                    self.copy.marks.insert(k, mk);
                }
            }
            if !todo.is_empty() {
                self.merges.push(todo);
            }
        }
        /// Hopper closed and started again: settings saved and read back,
        /// the copies set up again before the feed lists any pack, and the
        /// whole feed read once more.
        fn restart(&mut self) {
            use crate::settings::{FriendGroup, SharedPack};
            let mut st = Settings::default();
            st.friend_groups.push(FriendGroup { id: GROUP.into(), ..Default::default() });
            if self.copy.local == self.copy.inst {
                st.shared_with.push(SharedPack { inst: PACK.into(), group: GROUP.into(), ..Default::default() });
            } else {
                // Followed by hand: the follow names no group; only the
                // feed says which group the pack came through.
                st.followed_recipes.push(FollowedRecipe {
                    peer_id: "A".into(),
                    inst: PACK.into(),
                    local: self.copy.local.clone(),
                    code: self.follow_code.clone(),
                    ..Default::default()
                });
            }
            st.group_copies = vec![self.copy.clone()];
            let st: Settings = serde_json::from_str(&serde_json::to_string(&st).unwrap()).unwrap();
            let mut copies = st.group_copies.clone();
            let gone = reconcile(&mut copies, &st, |_| Vec::new());
            assert!(gone.is_empty(), "{}'s copy was dropped on start", self.id);
            assert_eq!(copies.len(), 1);
            self.copy = copies.remove(0);
            self.cursor = 0;
        }
    }

    /// The pack: mods 1 to 4, on.
    fn base() -> Vec<(u64, u64, bool)> {
        vec![(1, 11, true), (2, 21, true), (3, 31, true), (4, 41, true)]
    }

    const ALL: [&str; 3] = ["A", "B", "C"];

    /// Everyone publishes and takes in the others' first codes.
    fn group() -> (Relay, Pc, Pc, Pc) {
        let mut relay = Relay::default();
        let mut a = Pc::new("A", true, &base());
        let mut b = Pc::new("B", false, &base());
        let mut c = Pc::new("C", false, &base());
        a.follow_code.clear();
        b.follow_code = a.code();
        c.follow_code = a.code();
        for pc in [&mut a, &mut b, &mut c] {
            pc.publish(&mut relay, &ALL, true);
        }
        for pc in [&mut a, &mut b, &mut c] {
            pc.read(&relay);
            pc.merge();
            assert!(pc.merges.is_empty());
        }
        (relay, a, b, c)
    }

    /// A PC that is on: reads, takes in, publishes what changed.
    fn online(pc: &mut Pc, relay: &mut Relay) {
        pc.read(relay);
        pc.merge();
        pc.publish(relay, &ALL, true);
    }

    fn the_end() -> BTreeMap<u64, (u64, bool)> {
        [(1, (11, false)), (2, (21, false)), (3, (31, false)), (4, (42, true))].into_iter().collect()
    }

    #[test]
    fn a_member_back_after_days_offline_gets_every_change_and_undoes_none() {
        let (mut relay, mut a, mut b, mut c) = group();
        // C closes Hopper. A turns 1 off; B (who has that) turns 2 off and
        // then 3; A, not having read B yet, moves 4 to a newer file and
        // only then takes B's changes in.
        a.set(1, 11, false);
        a.publish(&mut relay, &ALL, true);
        online(&mut b, &mut relay);
        b.set(2, 21, false);
        b.publish(&mut relay, &ALL, true);
        b.set(3, 31, false);
        b.publish(&mut relay, &ALL, true);
        a.set(4, 42, true);
        a.publish(&mut relay, &ALL, true);
        online(&mut a, &mut relay);
        online(&mut b, &mut relay);
        assert_eq!(a.mods, the_end());
        assert_eq!(b.mods, the_end());
        // C starts: one code per member waits for it.
        assert_eq!(relay.queues["C"].len(), 2);
        c.restart();
        c.read(&relay);
        c.merge();
        assert_eq!(c.mods, the_end(), "C has all four changes");
        assert_eq!(c.merges.len(), 1, "taken in as one batch");
        let keys: Vec<&str> = c.merges[0].iter().map(|x| x.key.as_str()).collect();
        let mut unique = keys.clone();
        unique.dedup();
        assert_eq!(keys.len(), unique.len(), "each mod changed once");
        // Who it shows: A turned 1 off, B 2 and 3, A updated 4.
        let by: Vec<(&str, &str)> = ["p:1", "p:2", "p:3", "p:4"].iter().map(|k| (*k, c.copy.marks[*k].by.as_str())).collect();
        assert_eq!(by, [("p:1", "A"), ("p:2", "B"), ("p:3", "B"), ("p:4", "A")]);
        // C's publish undoes nothing anywhere.
        c.publish(&mut relay, &ALL, true);
        let (na, nb) = (a.merges.len(), b.merges.len());
        for pc in [&mut a, &mut b] {
            pc.restart();
            online(pc, &mut relay);
        }
        assert_eq!((a.merges.len(), b.merges.len()), (na, nb));
        assert_eq!(a.mods, the_end());
        assert_eq!(b.mods, the_end());
    }

    #[test]
    fn a_member_back_gets_a_members_changes_the_sharer_never_saw() {
        // The sharer is away too: only B's own code carries 2 and 3.
        let (mut relay, mut a, mut b, mut c) = group();
        b.set(2, 21, false);
        b.publish(&mut relay, &ALL, true);
        b.set(3, 31, false);
        b.publish(&mut relay, &ALL, true);
        c.restart();
        c.read(&relay);
        c.merge();
        assert_eq!((c.mods[&2], c.mods[&3]), ((21, false), (31, false)));
        a.restart();
        online(&mut a, &mut relay);
        assert_eq!((a.mods[&2], a.mods[&3]), ((21, false), (31, false)));
    }

    #[test]
    fn a_member_online_throughout_gets_each_change() {
        let (mut relay, mut a, mut b, mut c) = group();
        a.set(1, 11, false);
        a.publish(&mut relay, &ALL, true);
        online(&mut c, &mut relay);
        assert_eq!(c.mods[&1], (11, false));
        online(&mut b, &mut relay);
        b.set(2, 21, false);
        b.publish(&mut relay, &ALL, true);
        online(&mut c, &mut relay);
        b.set(3, 31, false);
        b.publish(&mut relay, &ALL, true);
        online(&mut c, &mut relay);
        a.set(4, 42, true);
        a.publish(&mut relay, &ALL, true);
        online(&mut c, &mut relay);
        for _ in 0..3 {
            for pc in [&mut a, &mut b, &mut c] {
                online(pc, &mut relay);
            }
        }
        assert_eq!(c.mods, the_end());
        assert_eq!(a.mods, the_end());
        assert_eq!(b.mods, the_end());
        assert_eq!(c.merges.len(), 4, "one batch per change");
    }

    #[test]
    fn the_sharer_turning_a_mod_off_turns_it_off_for_a_member() {
        let (mut relay, mut a, _b, mut c) = group();
        a.set(2, 21, false);
        a.publish(&mut relay, &ALL, true);
        c.read(&relay);
        c.merge();
        assert_eq!(c.mods[&2], (21, false));
        assert_eq!(c.copy.marks["p:2"].kind, Kind::Off);
        // Also after a start between the code arriving and the merge.
        a.set(3, 31, false);
        a.publish(&mut relay, &ALL, true);
        c.read(&relay);
        c.restart();
        c.read(&relay);
        c.merge();
        assert_eq!(c.mods[&3], (31, false));
    }

    /// Kim takes the sharer's update of mod 1 (11 -> 12) late: by the time
    /// her copy publishes, the sharer moved on to 13.
    fn kim_passes_an_update_on_late(stamped: bool) -> (Relay, Pc, Pc, Pc) {
        let mut relay = Relay::default();
        let mut a = Pc::new("A", true, &base());
        let mut k = Pc::new("B", false, &base());
        let mut q = Pc::new("C", false, &base());
        for pc in [&mut a, &mut k, &mut q] {
            pc.publish(&mut relay, &ALL, stamped);
        }
        for pc in [&mut a, &mut k, &mut q] {
            pc.read(&relay);
            pc.merge();
        }
        a.set(1, 12, true);
        a.publish(&mut relay, &ALL, stamped);
        k.read(&relay);
        k.merge();
        online(&mut q, &mut relay);
        a.set(1, 13, true);
        a.publish(&mut relay, &ALL, stamped);
        // Kim's copy goes out only now, still with 12.
        k.publish(&mut relay, &ALL, stamped);
        for pc in [&mut a, &mut q] {
            pc.read(&relay);
            pc.merge();
        }
        (relay, a, k, q)
    }

    #[test]
    fn an_update_passed_on_late_never_brings_the_older_file_back() {
        let (mut relay, mut a, mut k, mut q) = kim_passes_an_update_on_late(true);
        assert_eq!(a.mods[&1], (13, true), "the sharer keeps the newer file");
        assert_eq!(q.mods[&1], (13, true), "and so does everyone else");
        assert!(a.merges.is_empty());
        // Kim catches up, and nothing flips anywhere after.
        for _ in 0..3 {
            for pc in [&mut a, &mut k, &mut q] {
                online(pc, &mut relay);
            }
        }
        assert_eq!((a.mods[&1], k.mods[&1], q.mods[&1]), ((13, true), (13, true), (13, true)));
        assert!(a.merges.is_empty());
        // Without stamps (an older mailbox drops them) the late copy wins,
        // which is how files came back after updates before.
        let (_relay, a, _k, _q) = kim_passes_an_update_on_late(false);
        assert_eq!(a.mods[&1], (12, true));
    }

    #[test]
    fn a_first_code_takes_only_dated_changes() {
        // Mia's first code: 1 off, stamped with Sam's change (seq 30, newer
        // than anything here), and 2 off unstamped (may only be Mia behind).
        let mut copy = PackCopy::default();
        let mut g = got(code(vec![cf(11, false), cf(21, false)]), 40, "Mia");
        g.stamps.insert("p:1".into(), Stamp { seq: 30, by: "sam".into() });
        copy.got.insert("sam".into(), got(code(vec![cf(11, true)]), 5, "Sam"));
        copy.arrive("mia", g);
        let here = m(vec![cf(11, true), cf(21, true)]);
        let (todo, merged) = plan(&copy, &here, Some(&here), false, dec);
        assert_eq!(todo.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(), ["p:1"]);
        assert_eq!((merged.marks[0].1.by.as_str(), merged.marks[0].1.name.as_str(), merged.marks[0].1.seq), ("sam", "Sam", 30));
        assert_eq!(merged.taken.len(), 1);
    }

    #[test]
    fn a_change_held_too_long_is_dropped_not_undone() {
        let base = vec![cf(11, true)];
        let mut copy = PackCopy::default();
        copy.got.insert("mia".into(), got(code(base.clone()), 1, "Mia"));
        copy.arrive("mia", got(code(vec![cf(11, false), cf(21, true)]), 10, "Mia"));
        // Mod 1 moved to file 12 here and not sent yet.
        let here = m(vec![cf(12, true)]);
        let sent = m(base.clone());
        let (todo, merged) = plan(&copy, &here, Some(&sent), true, dec);
        assert_eq!(todo.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(), ["p:2"], "the change here stays");
        assert!(!merged.held && merged.taken.len() == 1);
    }

    #[test]
    fn a_relay_that_starts_over_numbers_this_copy_anew() {
        let mut copy = PackCopy::default();
        copy.got.insert("mia".into(), got(code(vec![cf(11, true)]), 900, "Mia"));
        copy.marks.insert("p:1".into(), Mark { state: st(11, true), seq: 900, ..Default::default() });
        copy.relay_reset();
        copy.arrive("mia", got(code(vec![cf(11, false)]), 3, "Mia"));
        let here = m(vec![cf(11, true)]);
        let (todo, _) = plan(&copy, &here, Some(&here), false, dec);
        assert_eq!(todo.len(), 1);
    }

    #[test]
    fn a_copy_kept_already_survives_a_start_before_the_feed_lists_its_pack() {
        use crate::settings::FriendGroup;
        let mut st = Settings::default();
        st.friend_groups.push(FriendGroup { id: "g".into(), ..Default::default() });
        st.followed_recipes.push(FollowedRecipe { peer_id: "o".into(), inst: "p".into(), local: "l".into(), code: "c".into(), ..Default::default() });
        let mut copies = vec![PackCopy { inst: "p".into(), group: "g".into(), local: "l".into(), ..Default::default() }];
        copies[0].got.insert("sam".into(), got("sam-code".into(), 7, "Sam"));
        assert!(reconcile(&mut copies, &st, |_| Vec::new()).is_empty());
        assert_eq!(copies[0].got["sam"].code, "sam-code");
        // Out of the group: then it goes.
        st.friend_groups.clear();
        assert_eq!(reconcile(&mut copies, &st, |_| Vec::new()).len(), 1);
    }

    #[test]
    fn a_pack_mod_named_by_its_file_is_found() {
        let mr = crate::modrinth::num_id("AANobbMI");
        let a = Addon { project_id: mr, file_name: "sodium-0.6.jar".into(), from_pack: true, ..Default::default() };
        assert!(is_addon(&a, "n:sodium-0.6.jar"));
        assert!(is_addon(&a, &format!("p:{mr}")));
        assert!(!is_addon(&a, "n:other.jar"));
        let off = Change { key: "n:gone.jar".into(), was: State::Unlisted, now: State::Off, entry: None };
        let inst = Instance { addons: vec![a], ..Default::default() };
        assert!(!does_something(&inst, &off), "a mod not here isn't told as changed");
        assert!(does_something(&inst, &Change { key: "n:sodium-0.6.jar".into(), ..off }));
    }

    /// A Modrinth pack's mod, as every PC has it: listed by file name.
    fn on_disk(tag: &str) -> Instance {
        let root = std::env::temp_dir().join(format!("hopper-groupsync-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("mods")).unwrap();
        std::fs::write(root.join("mods").join("sodium-0.6.jar"), b"jar").unwrap();
        let a = Addon {
            project_id: crate::modrinth::num_id("AANobbMI"),
            name: "Sodium".into(),
            file_name: "sodium-0.6.jar".into(),
            enabled: true,
            from_pack: true,
            ..Default::default()
        };
        let inst = Instance { root, name: "T".into(), addons: vec![a], ..Default::default() };
        inst.save().unwrap();
        inst
    }

    fn turn_off() -> Change {
        Change {
            key: "n:sodium-0.6.jar".into(),
            was: State::Unlisted,
            now: State::Off,
            entry: Some(Entry::Off(Target::Named("sodium-0.6.jar".into()))),
        }
    }

    #[test]
    fn a_pack_mod_turned_off_by_the_sharer_turns_off_here() {
        let mut inst = on_disk("off");
        let jobs = crate::jobs::Jobs::default();
        let job = jobs.start("t");
        apply(&mut inst, &[turn_off()], &[], &job).unwrap();
        assert!(!inst.addons[0].enabled);
        assert!(inst.root.join("mods").join("sodium-0.6.jar.disabled").is_file());
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    #[cfg(windows)]
    #[test]
    fn a_pack_mod_turned_off_while_the_game_holds_it_turns_off_when_it_closes() {
        use std::os::windows::fs::OpenOptionsExt as _;
        let mut inst = on_disk("locked");
        let jar = inst.root.join("mods").join("sodium-0.6.jar");
        // Open the way Java's JarFile does: no sharing of delete or rename.
        let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(&jar).unwrap();
        let jobs = crate::jobs::Jobs::default();
        let job = jobs.start("t");
        apply(&mut inst, &[turn_off()], &[], &job).unwrap();
        assert!(inst.addons[0].enabled, "still on while the game has it");
        assert_eq!(crate::locked::switches(&inst.root).get("sodium-0.6.jar"), Some(&false));
        let notes = jobs.snapshot().into_iter().flat_map(|j| j.notes).collect::<Vec<_>>();
        assert!(notes.iter().any(|n| n.contains("turns off when the game closes")), "{notes:?}");
        drop(held);
        assert_eq!(crate::locked::flush(&inst.root), 1);
        assert!(jar.with_extension("jar.disabled").is_file());
        assert!(!crate::instances::load(&inst.root).unwrap().addons[0].enabled);
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    // ---- which group packs keeping in step follows ----

    fn in_step(id: &str, follows: &[(&str, &str)]) -> crate::mailbox::Group {
        crate::mailbox::Group {
            id: id.into(),
            name: format!("{id} group"),
            me: crate::mailbox::GroupMe { joined: true, in_step: true },
            follows: follows.iter().map(|(from, inst)| crate::mailbox::GroupFollow { from: (*from).into(), inst: (*inst).into() }).collect(),
            ..Default::default()
        }
    }
    fn pack(from: &str, inst: &str, key: &str) -> crate::mailbox::Event {
        crate::mailbox::Event { from: from.into(), inst: inst.into(), key: key.into(), code: "code".into(), ..Default::default() }
    }
    fn picked(st: &Settings, groups: &[crate::mailbox::Group], recipes: &[crate::mailbox::Event]) -> Vec<String> {
        to_follow(st, groups, recipes, "me", |_| false).into_iter().map(|(e, g, _)| format!("{}/{}@{g}", e.from, e.inst)).collect()
    }

    #[test]
    fn in_step_follows_what_members_follow() {
        let groups = [in_step("g", &[("ann", "atm"), ("ann", "atm")])];
        // Once, however many members follow it.
        assert_eq!(picked(&Settings::default(), &groups, &[pack("ann", "atm", "ATM10")]), ["ann/atm@g"]);
        // Its code hasn't come yet.
        assert!(picked(&Settings::default(), &groups, &[]).is_empty());
        // A group not kept in step, or not joined.
        let mut off = in_step("g", &[("ann", "atm")]);
        off.me.in_step = false;
        assert!(picked(&Settings::default(), &[off], &[pack("ann", "atm", "")]).is_empty());
        // Being followed right now.
        assert!(to_follow(&Settings::default(), &groups, &[pack("ann", "atm", "")], "me", |k| k == "ann/atm").is_empty());
    }

    #[test]
    fn in_step_skips_own_and_stopped_packs() {
        let groups = [in_step("g", &[("me", "mine"), ("ann", "atm")])];
        let recipes = [pack("me", "mine", ""), pack("ann", "atm", "")];
        let mut st = Settings::default();
        st.friend_groups.push(crate::settings::FriendGroup { id: "g".into(), stopped: vec!["ann/atm".into()], ..Default::default() });
        assert!(picked(&st, &groups, &recipes).is_empty());
        // Stopped in another group only: still followed through this one.
        st.friend_groups[0].id = "other".into();
        assert_eq!(picked(&st, &groups, &recipes), ["ann/atm@g"]);
    }

    #[test]
    fn in_step_skips_packs_followed_already() {
        let groups = [in_step("g", &[("ann", "atm")])];
        let recipes = [pack("ann", "atm", "ATM10")];
        let follow = |inst: &str| FollowedRecipe { peer_id: "ann".into(), inst: inst.into(), root: "/i/ATM10".into(), local: "l1".into(), ..Default::default() };
        let mut st = Settings::default();
        st.followed_recipes.push(follow("atm"));
        assert!(picked(&st, &groups, &recipes).is_empty());
        // Saved by an older build, by the sender's folder name.
        st.followed_recipes[0] = follow("atm10");
        assert!(picked(&st, &groups, &recipes).is_empty());
        // Someone else's pack of the same name is another pack.
        st.followed_recipes[0].peer_id = "bob".into();
        assert_eq!(picked(&st, &groups, &recipes), ["ann/atm@g"]);
    }

    /// The bug this guards: a follow recorded when its instance was made,
    /// whose first version never finished going in (the install failed or
    /// Hopper closed), must still count after a restart, or keeping in
    /// step makes another copy on every start.
    #[test]
    fn unfinished_follow_survives_a_restart() {
        let groups = [in_step("g", &[("ann", "atm")])];
        let recipes = [pack("ann", "atm", "")];
        let mut st = Settings::default();
        st.followed_recipes.push(FollowedRecipe {
            peer_id: "ann".into(),
            inst: "atm".into(),
            root: "/i/ATM10".into(),
            local: "l1".into(),
            code: String::new(),
            seen: "code".into(),
            group: "g".into(),
            ..Default::default()
        });
        st.friend_groups.push(crate::settings::FriendGroup { id: "g".into(), in_step: true, ..Default::default() });
        let back: Settings = serde_json::from_str(&serde_json::to_string(&st).unwrap()).unwrap();
        let f = &back.followed_recipes[0];
        // Still waiting for its first version, so it's applied into the
        // same instance; and no new copy is made.
        assert!(f.waiting() && f.code.is_empty() && f.local == "l1");
        assert!(picked(&back, &groups, &recipes).is_empty());
        // And it isn't kept both ways before that version is in.
        let mut copies = Vec::new();
        reconcile(&mut copies, &back, |_| vec!["g".into()]);
        assert!(copies.is_empty());
    }
}
