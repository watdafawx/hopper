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
use crate::sharecode::{Base, Entry, Share, Target};
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
    /// A mod's lock ([`LOCK_PREFIX`]) was set or changed.
    Locked,
    /// A mod's lock was taken off.
    Unlocked,
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
            Kind::Locked => format!("Locked by {who}"),
            Kind::Unlocked => format!("Unlocked by {who}"),
        }
    }
}

/// What a mod's lock goes by in a group, next to the mods: "~lock:" and the
/// mod's key. The lock is one more thing the group keeps the same, with the
/// same rules as the mods (latest change wins, echoes change nothing, the
/// mark says who). Like [`LOADER_KEY`], not a mod's key in any form.
pub const LOCK_PREFIX: &str = "~lock:";

/// The mod a lock key is about ("p:123" from "~lock:p:123").
pub fn lock_target(key: &str) -> Option<&str> {
    key.strip_prefix(LOCK_PREFIX)
}

/// A lock as a [`State`]: unlisted when the mod isn't locked at all, else
/// "lock:<the version kept>" with `on` for the whole-mod lock.
fn lock_state(version: &str, locked: bool) -> State {
    if !locked && version.is_empty() {
        return State::Unlisted;
    }
    State::File { file: format!("lock:{version}"), on: locked }
}

/// The lock a state means: (locked, the version kept).
pub fn lock_of(state: &State) -> (bool, String) {
    match state {
        State::File { file, on } => (*on, file.strip_prefix("lock:").unwrap_or_default().to_string()),
        _ => (false, String::new()),
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

/// The key the loader goes by next to the mods: it is one more thing the
/// group keeps the same, with the same rules (latest change wins, echoes
/// change nothing). Not a mod's key in any form, so it never meets one.
pub const LOADER_KEY: &str = "~loader";

/// A code's loader as the loader's [`State`]: "loader:neoforge-21.1.256".
/// A pack's own loader isn't listed (like a pack mod as the pack has it).
fn loader_state(loader: Option<&String>) -> State {
    loader.map(|l| State::File { file: format!("loader:{l}"), on: true }).unwrap_or_default()
}

/// The loader a code has set ("neoforge-21.1.256"): the code's own when it
/// differs from the pack's, else the one a plain code names. None: the
/// pack's own (a code of a pack lists no loader when it's the pack's, so a
/// pack's default and the same loader named outright are never two
/// states), or no loader at all.
pub fn loader_of(share: &Share) -> Option<String> {
    if let Some(l) = share.loader.as_ref().filter(|l| !l.is_empty()) {
        return Some(l.clone());
    }
    match &share.base {
        Base::Plain { loader, .. } if !loader.is_empty() => Some(loader.clone()),
        _ => None,
    }
}

/// A code's mods by key, and its loader. `project` says whose a file is
/// (None: couldn't tell, and then `unsure` is set and the mod is left out).
#[derive(Clone)]
pub struct Mods {
    pub map: BTreeMap<String, (State, Entry)>,
    pub unsure: bool,
    /// [`loader_of`] the code.
    pub loader: Option<String>,
    /// The code's pack version ([`crate::grouppack::pack_state`]).
    pub pack: State,
    /// What that pack version comes with, when looked up
    /// ([`fill_packs`]); needed only to compare codes of two versions.
    pub ships: Option<crate::grouppack::Ships>,
    /// The file names of its mods, by key, where known: a jar the new pack
    /// version ships by name is found by it.
    pub names: BTreeMap<String, String>,
}

impl Mods {
    /// What `key` is in this code: a mod's state, or the loader's.
    pub fn state(&self, key: &str) -> State {
        if key == LOADER_KEY {
            return loader_state(self.loader.as_ref());
        }
        if key == PACK_KEY {
            return self.pack.clone();
        }
        self.map.get(key).map(|(s, _)| s.clone()).unwrap_or_default()
    }
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
            Entry::Lock { key, version, locked } => (format!("{LOCK_PREFIX}{key}"), lock_state(version, *locked)),
        };
        map.insert(key, (state, e.clone()));
    }
    Mods { map, unsure, loader: loader_of(share), pack: crate::grouppack::pack_state(&share.base), ships: None, names: BTreeMap::new() }
}

pub use crate::grouppack::PACK_KEY;

/// Fills in what each code's pack version comes with ([`Mods::ships`])
/// and the file names of their mods this instance knows, when the codes
/// are of more than one pack version (only then is it needed: a mod a
/// member had that their new version ships is not one they removed).
pub fn fill_packs(inst: &Instance, all: &mut [&mut Mods], job: &Job) {
    let mut packs: Vec<State> = all.iter().map(|m| m.pack.clone()).collect();
    packs.sort();
    packs.dedup();
    if packs.len() < 2 {
        return;
    }
    let ships: Vec<(State, Option<crate::grouppack::Ships>)> = packs
        .into_iter()
        .map(|p| {
            let s = crate::grouppack::Version::of(&p).and_then(|v| crate::grouppack::ships(&v, job));
            (p, s)
        })
        .collect();
    for m in all.iter_mut() {
        m.ships = ships.iter().find(|(p, _)| *p == m.pack).and_then(|(_, s)| s.clone());
        let keys: Vec<String> = m.map.iter().filter(|(_, (s, _))| matches!(s, State::File { .. })).map(|(k, _)| k.clone()).collect();
        for k in keys {
            if let Some(i) = find_addon(inst, &k) {
                m.names.insert(k, inst.addons[i].file_name.clone());
            }
        }
    }
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
        if self.key == LOADER_KEY || self.key == PACK_KEY {
            return Kind::Updated;
        }
        if lock_target(&self.key).is_some() {
            return if self.now == State::Unlisted { Kind::Unlocked } else { Kind::Locked };
        }
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
/// may only be the one that wasn't recognised. Between codes of two pack
/// versions, what only differs because of the version (see
/// [`crate::grouppack::explained`]) is no change; the version itself is
/// one ([`PACK_KEY`]).
pub fn changes(was: &Mods, now: &Mods) -> Vec<Change> {
    let mut out = mod_changes(was, now);
    out.retain(|c| !crate::grouppack::explained(was, now, c));
    if was.pack != now.pack {
        out.push(Change { key: PACK_KEY.to_string(), was: was.pack.clone(), now: now.pack.clone(), entry: None });
    }
    out
}

fn mod_changes(was: &Mods, now: &Mods) -> Vec<Change> {
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
    let (before, after) = (was.state(LOADER_KEY), now.state(LOADER_KEY));
    if before != after {
        out.push(Change { key: LOADER_KEY.to_string(), was: before, now: after, entry: None });
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
    let (mut prev, mut now) = (mods(&prev, lookup(&found)), mods(now, lookup(&found)));
    // After this copy's own pack update: a mod of the player's the new
    // version ships isn't one they removed.
    if prev.pack != now.pack {
        let jobs = crate::jobs::Jobs::new(crate::waker::noop());
        let j = jobs.start_quiet("Reading the pack's mod list");
        fill_packs(inst, &mut [&mut prev, &mut now], &j);
        j.finish(&Ok::<(), anyhow::Error>(()));
    }
    new_marks(inst, &copy.marks, &prev, &now, &pack_ships(inst), (me, seq, at), remote_file_name)
}

/// [`sent_marks`] once the codes are read: the marks [`mark_own`] gives
/// that differ from `marks`, except a mod that only left the code because
/// its jar is the pack's own now ([`became_packs`]): that's no removal.
pub fn new_marks(
    inst: &Instance,
    marks: &BTreeMap<String, Mark>,
    prev: &Mods,
    now: &Mods,
    shipped: &std::collections::HashSet<String>,
    (me, seq, at): (&str, u64, u64),
    name_of: impl Fn(&Entry) -> Option<String>,
) -> Vec<(String, Mark)> {
    let mut out = marks.clone();
    mark_own(&mut out, Some(prev), now, me, seq, at);
    out.into_iter()
        .filter(|(k, m)| marks.get(k) != Some(m))
        .filter(|(k, m)| m.state != State::Unlisted || !became_packs(inst, shipped, k, prev.map.get(k).map(|(_, e)| e), &name_of))
        .collect()
}

/// The files the modpack itself put in the instance (see
/// [`crate::pack_where::shipped`]); empty when it isn't a pack or nothing
/// says.
pub fn pack_ships(inst: &Instance) -> std::collections::HashSet<String> {
    inst.pack.as_ref().and_then(|_| crate::pack_where::shipped(inst)).unwrap_or_default()
}

/// Whether `a` is a jar the modpack ships in its own overrides.
pub fn ships_jar(shipped: &std::collections::HashSet<String>, a: &Addon) -> bool {
    a.folder() == "mods" && shipped.contains(&format!("mods/{}", a.file_name.trim_end_matches(".disabled").to_lowercase()))
}

/// Whether a mod that left this copy's code (`key`, listed in the last code
/// sent as `was`) only left it because its jar turned out to be the pack's
/// own: an override jar that CurseForge had matched to some other project
/// (a stranger's re-upload) is the pack's since 0.72.1, so the code stops
/// listing it while the jar stays. Telling the group it was removed makes
/// the other members delete a jar their pack needs. `name_of`: a code
/// entry's file name, asked from CurseForge or Modrinth.
pub fn became_packs(
    inst: &Instance,
    shipped: &std::collections::HashSet<String>,
    key: &str,
    was: Option<&Entry>,
    name_of: impl Fn(&Entry) -> Option<String>,
) -> bool {
    if shipped.is_empty() {
        return false;
    }
    if let Some(i) = find_addon(inst, key) {
        return inst.addons[i].from_pack && ships_jar(shipped, &inst.addons[i]);
    }
    // The record no longer has the other project, so only the file's name
    // ties the two together.
    let name = match was {
        Some(Entry::Source { pin, .. }) => Some(pin.file.clone()),
        Some(e @ (Entry::CurseFile { .. } | Entry::ModrinthFile { .. })) => name_of(e),
        _ => None,
    };
    let Some(name) = name else { return false };
    inst.addons.iter().any(|a| a.from_pack && a.file_name.trim_end_matches(".disabled").eq_ignore_ascii_case(&name) && ships_jar(shipped, a))
}

/// A code entry's file name from CurseForge or Modrinth (None: couldn't
/// ask).
fn remote_file_name(e: &Entry) -> Option<String> {
    match e {
        Entry::CurseFile { file, .. } => crate::cf::get_files(&[*file]).ok()?.into_iter().find(|f| f.id == *file).map(|f| f.file_name),
        Entry::ModrinthFile { version, .. } => {
            crate::modrinth::version(version).ok().map(|v| crate::modrinth::version_to_file(&v).file_name)
        }
        _ => None,
    }
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
    let state_of = |k: &str| now.state(k);
    marks
        .iter()
        .filter(|(k, m)| m.seq > 0 && state_of(k) == m.state && !(now.unsure && m.state == State::Unlisted))
        .map(|(k, m)| (k.clone(), Stamp { seq: m.seq, by: m.by.clone() }))
        .collect()
}

/// The stamps a code going out carries ([`stamps_of`]), plus one for each
/// mod this code changes itself (against `sent`, the last code this copy
/// published): `seq` 0 by `me`, "made in this very code". A member only
/// compares a code with the last one it took from this copy, and the relay
/// keeps just the newest code per sender, so a change that puts a mod back
/// the way that member last saw it would otherwise read as no change at
/// all: an Undo right after taking someone's update (before they read the
/// echo) never reached them, and they kept the update. Left out when the
/// pack version moved, where a mod can differ only because of the version.
pub fn stamps_out(marks: &BTreeMap<String, Mark>, sent: Option<&Mods>, now: &Mods, me: &str) -> BTreeMap<String, Stamp> {
    let mut out = stamps_of(marks, now);
    if let Some(sent) = sent.filter(|s| s.pack == now.pack) {
        for c in changes(sent, now) {
            if !out.contains_key(&c.key) && marks.get(&c.key).is_none_or(|m| m.state != c.now) {
                out.insert(c.key, Stamp { seq: 0, by: me.to_string() });
            }
        }
    }
    out
}

/// [`stamps_out`] for an instance's code, off the UI thread.
pub fn stamps(inst: &Instance, copy: &PackCopy, now: &Share, me: &str) -> BTreeMap<String, Stamp> {
    let sent = (!copy.sent.is_empty()).then(|| crate::sharecode::decode(&copy.sent).ok()).flatten();
    let found = match &sent {
        Some(s) => projects_of(inst, &[now, s]),
        None => projects_of(inst, &[now]),
    };
    let sent = sent.as_ref().map(|s| mods(s, lookup(&found)));
    stamps_out(&copy.marks, sent.as_ref(), &mods(now, lookup(&found)), me)
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
    /// Members' changes to mods from GitHub or another link that this PC
    /// left out because it hasn't confirmed their source. Their codes are
    /// taken in all the same, so nothing else would bring them back: they
    /// are tried again once the player confirms the source.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub owed: Vec<Owed>,
}

/// A member's change that waits for the player to confirm a mod's source
/// ([`PackCopy::owed`]). Everything needed to make it later, since the
/// member's code may have moved on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Owed {
    /// The mod's key, and what the member's change did to it.
    pub key: String,
    pub was: State,
    pub now: State,
    pub entry: Option<Entry>,
    /// Who made it, and when (the relay's number and time), as the mark
    /// would say.
    pub by: String,
    pub name: String,
    pub seq: u64,
    pub at: u64,
    /// The player said yes to this mod ("Allow and install" on the group
    /// status): it is made whatever Settings › Mod sources and the trust
    /// list say. Settings saved before this load as not approved.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub approved: bool,
}

impl Owed {
    /// The change this one waits to make.
    fn change(&self) -> Change {
        Change { key: self.key.clone(), was: self.was.clone(), now: self.now.clone(), entry: self.entry.clone() }
    }

    /// The pin it would download, when it still would.
    fn pin(&self) -> Option<&crate::sources::carry::Pin> {
        match &self.entry {
            Some(Entry::Source { pin, .. }) if matches!(self.now, State::File { .. }) => Some(pin),
            _ => None,
        }
    }
}

impl PackCopy {
    /// The mods waiting for the player to confirm their source: (name for
    /// people, source) of each, for "2 mods waiting for you to confirm their
    /// source".
    pub fn owed_sources(&self) -> Vec<(String, crate::sources::ModSource)> {
        self.owed
            .iter()
            .filter_map(Owed::pin)
            .map(|p| (crate::sources::carry::display_name(p), p.source.clone()))
            .collect()
    }

    /// "2 mods waiting for you to confirm their source (Settings › Mod
    /// sources)." None when nothing is owed.
    pub fn owed_says(&self) -> Option<String> {
        let n = self.owed_sources().len();
        let (what, their) = if n == 1 { ("mod", "its") } else { ("mods", "their") };
        (n > 0).then(|| format!("{n} {what} waiting for you to confirm {their} source (Settings › Mod sources)."))
    }

    /// Whether the player confirmed the source of one of the owed mods
    /// since (`allow`: Settings › Mod sources is on): worth a merge now,
    /// which makes it. Never true for a source still waiting, so asking
    /// again and again costs nothing.
    pub fn owed_ready(&self, allow: bool) -> bool {
        self.owed.iter().any(|o| o.approved && o.pin().is_some())
            || (allow && self.owed.iter().filter_map(Owed::pin).any(|p| crate::sources::trusted(&p.source)))
    }

    /// The owed mods that can be said yes to: (name for people, where it's
    /// from "GitHub: owner/repo", its source), for the group status's
    /// "Allow and install".
    pub fn owed_to_allow(&self) -> Vec<(String, String, crate::sources::carry::Pin)> {
        self.owed
            .iter()
            .filter(|o| !o.approved)
            .filter_map(Owed::pin)
            .map(|p| (crate::sources::carry::display_name(p), format!("{}: {}", p.source.kind.label(), p.source.publisher), p.clone()))
            .collect()
    }

    /// The player said yes to the owed mods, except `leave_out` (the ones
    /// they still don't want): they go in on the next merge. Returns
    /// whether any was marked.
    pub fn approve_owed(&mut self, leave_out: &[crate::sources::carry::Pin]) -> bool {
        let mut any = false;
        for o in &mut self.owed {
            if !o.approved && o.pin().is_some_and(|p| !leave_out.contains(p)) {
                o.approved = true;
                any = true;
            }
        }
        any
    }

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
        for o in &mut self.owed {
            o.seq = 0;
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
    /// The loader the instance was moved to ("neoforge-21.1.256"). Saved
    /// already; the new loader still has to be installed (not while the
    /// game runs).
    pub loader: Option<String>,
    /// For each change decided, the pack version of the code it came in
    /// (a member on another version than this copy can't swap or take out
    /// a pack mod here: see [`merge`]).
    pub packs: BTreeMap<String, State>,
    /// The group's pack version went in: what to tell the player.
    pub pack_done: Option<String>,
    /// The group's pack version couldn't go in yet. Nothing else was taken
    /// in either (a member's mod changes are relative to their version, so
    /// they wait for it); the caller shows it and tries again later.
    pub pack_due: Option<crate::grouppack::Due>,
    /// Members' changes left out on purpose, and why (see [`Kept`]). Their
    /// codes are taken in all the same: the change isn't tried again.
    pub kept: Vec<Kept>,
    /// The copy's [`PackCopy::owed`] after this merge (None: unchanged).
    pub owed: Option<Vec<Owed>>,
}

/// A member's change sync didn't make here, for the player to see.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kept {
    /// The mod's key and its name here.
    pub key: String,
    pub name: String,
    /// What the member did, and their name.
    pub kind: Kind,
    pub by: String,
    pub why: KeptWhy,
}

/// Why a member's change was left out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeptWhy {
    /// The player locked the mod's version here: sync may turn it on or
    /// off, but never swaps, removes or puts back its file.
    Locked,
    /// A mod from GitHub or another link, from a source this PC hasn't
    /// confirmed (Settings › Mod sources): nothing is downloaded from a
    /// source nobody was asked about.
    Source,
}

impl Kept {
    /// "Sodium stays as it is: you locked its version. Mia updated it."
    pub fn says(&self) -> String {
        let why = match self.why {
            KeptWhy::Locked => "you locked its version",
            KeptWhy::Source => "you haven't confirmed its source yet (Settings › Mod sources)",
        };
        if self.why == KeptWhy::Source {
            let verb = if self.kind == Kind::Updated { "wasn't updated" } else { "wasn't installed" };
            let what = match self.kind {
                Kind::Updated => "changed its version",
                _ => "added it",
            };
            return if self.by.is_empty() {
                format!("{} {verb}: {why}.", self.name)
            } else {
                format!("{} {verb}: {why}. {} {what}.", self.name, self.by)
            };
        }
        let what = match self.kind {
            Kind::Added => "added it",
            Kind::Removed => "removed it",
            Kind::On => "turned it on",
            Kind::Off => "turned it off",
            Kind::Updated => "changed its version",
            Kind::Locked => "locked it",
            Kind::Unlocked => "unlocked it",
        };
        if self.by.is_empty() {
            format!("{} stays as it is: {why}.", self.name)
        } else {
            format!("{} stays as it is: {why}. {} {what}.", self.name, self.by)
        }
    }
}

/// The file a mod here is, the way a code names it ("cf:<file>",
/// "mr:<version>", "src:<file name>").
fn file_here(a: &Addon) -> Option<String> {
    if a.source.is_some() {
        return Some(format!("src:{}", a.file_name.trim_end_matches(".disabled").to_lowercase()));
    }
    if !a.mr_version.is_empty() {
        return Some(format!("mr:{}", a.mr_version));
    }
    (a.file_id != 0 && !is_mr(a.project_id)).then(|| format!("cf:{}", a.file_id))
}

/// The mod here (`at`) that already is `pin`'s file: the same link and file
/// name, with the pinned bytes (its sha256; a pin without one goes by the
/// release it was installed from). Such a change only needs a switch, so
/// toggling a mod from GitHub never downloads it again.
pub(crate) fn pinned_here(inst: &Instance, at: Option<usize>, pin: &crate::sources::carry::Pin) -> Option<usize> {
    let i = at?;
    let a = inst.addons.get(i)?;
    let s = a.source.as_ref()?;
    let name = |n: &str| n.trim_end_matches(".disabled").to_lowercase();
    if name(&a.file_name) != name(&pin.file) || s.url != pin.source.url || s.pattern != pin.source.pattern {
        return None;
    }
    let sure = if pin.source.sha256.is_empty() {
        !pin.source.seen.is_empty() && s.seen == pin.source.seen
    } else {
        let twin = Addon { enabled: !a.enabled, ..a.clone() };
        [inst.addon_path(a), inst.addon_path(&twin)]
            .iter()
            .find(|p| p.is_file())
            .and_then(|p| crate::sources::sha256_file(p).ok())
            .is_some_and(|h| h.eq_ignore_ascii_case(&pin.source.sha256))
    };
    sure.then_some(i)
}

/// The pin `c` would download here: a mod from a link that isn't here as
/// that file yet.
fn pin_to_get<'a>(inst: &Instance, c: &'a Change) -> Option<&'a crate::sources::carry::Pin> {
    let Some(Entry::Source { pin, .. }) = &c.entry else { return None };
    if !matches!(c.now, State::File { .. }) {
        return None;
    }
    pinned_here(inst, find_addon(inst, &c.key), pin).is_none().then_some(pin)
}

/// Leaves out the changes that would install a mod from a source this PC
/// hasn't confirmed (`skip`: [`crate::sources::carry::auto_skip`]), and
/// lists them in `merged.kept`: nobody can be asked during a sync, and it
/// must not read as done. Their marks go too, as for a locked mod.
fn keep_untrusted(inst: &Instance, mut todo: Vec<Change>, skip: &[crate::sources::carry::Pin], merged: &mut Merged) -> Vec<Change> {
    todo.retain(|c| {
        let Some(pin) = pin_to_get(inst, c).filter(|p| skip.contains(p)) else { return true };
        let by = merged.made.iter().rev().find(|(k, _, _)| *k == c.key).map(|(_, _, n)| n.clone()).unwrap_or_default();
        let name = format!(
            "{} ({}: {})",
            crate::sources::carry::display_name(pin),
            pin.source.kind.label(),
            pin.source.publisher
        );
        // Owed to the player: made once they confirm the source (see
        // `PackCopy::owed`), dated as the member's change was.
        let mark = merged.marks.iter().rev().find(|(k, _)| *k == c.key).map(|(_, m)| m.clone()).unwrap_or_default();
        let owed = Owed { key: c.key.clone(), was: c.was.clone(), now: c.now.clone(), entry: c.entry.clone(), by: mark.by, name: mark.name, seq: mark.seq, at: mark.at, approved: false };
        let list = merged.owed.get_or_insert_with(Vec::new);
        list.retain(|o| o.key != c.key);
        list.push(owed);
        merged.made.retain(|(k, _, _)| *k != c.key);
        merged.marks.retain(|(k, _)| *k != c.key);
        merged.kept.push(Kept { key: c.key.clone(), name, kind: c.kind(), by, why: KeptWhy::Source });
        false
    });
    todo
}

/// Whether `c` would change the file of a mod the player locked here.
/// Turning it on or off keeps the file, so that still goes through.
pub fn locked_out(inst: &Instance, c: &Change) -> bool {
    let Some(a) = find_addon(inst, &c.key).map(|i| &inst.addons[i]) else { return false };
    if !a.locked {
        return false;
    }
    match &c.now {
        State::Off => false,
        // Back on as the pack has it, from turned off: a switch. Anything
        // else back to "as the pack has it" puts the pack's file back.
        State::Unlisted => c.was != State::Off,
        State::Removed => true,
        State::File { file, .. } => file_here(a).is_none_or(|f| f != *file),
    }
}

/// Leaves out the changes that would change a locked mod's file, and
/// lists them in `merged.kept`. Their marks go too: "Updated by Mia" on a
/// mod that wasn't would be wrong.
fn keep_locked(inst: &Instance, mut todo: Vec<Change>, merged: &mut Merged) -> Vec<Change> {
    todo.retain(|c| {
        if !locked_out(inst, c) {
            return true;
        }
        let by = merged.made.iter().rev().find(|(k, _, _)| *k == c.key).map(|(_, _, n)| n.clone()).unwrap_or_default();
        let name = find_addon(inst, &c.key).map(|i| inst.addons[i].name.clone()).unwrap_or_else(|| c.key.clone());
        merged.made.retain(|(k, _, _)| *k != c.key);
        merged.marks.retain(|(k, _)| *k != c.key);
        merged.kept.push(Kept { key: c.key.clone(), name, kind: c.kind(), by, why: KeptWhy::Locked });
        false
    });
    todo
}

/// Makes the group's lock changes here: a mod's lock ([`LOCK_PREFIX`]) is
/// set or taken off the way the member's change says, so nobody offers the
/// update to a mod one of the group keeps on its version. A mod this PC
/// doesn't have, or keeps only here, has nothing to lock (its mark stays,
/// so who changed it last still counts). True when a lock changed (the
/// caller saves).
fn set_locks(inst: &mut Instance, changes: &[Change], merged: &mut Merged) -> bool {
    let mut touched = false;
    for c in changes {
        let at = lock_target(&c.key)
            .and_then(|k| find_addon(inst, k))
            .filter(|&i| !crate::onlyhere::is_kept(inst, &inst.addons[i]));
        let (locked, version) = lock_of(&c.now);
        match at {
            Some(i) if (inst.addons[i].locked, &inst.addons[i].locked_version) != (locked, &version) => {
                inst.addons[i].locked = locked;
                inst.addons[i].locked_version = version;
                touched = true;
            }
            // Already like that, or nothing to lock here: an echo.
            _ => merged.made.retain(|(k, _, _)| *k != c.key),
        }
    }
    touched
}

/// Puts back into `todo` the changes [`PackCopy::owed`] holds, so they are
/// made now if their source is confirmed since (else they are left out and
/// owed again, as the first time). Dropped: one a later change of the mod
/// replaced (a member's, or this PC's own), one this PC changed itself that
/// isn't sent yet (that one is the later), and one already the way it
/// says.
fn revive_owed(copy: &PackCopy, here: &Mods, sent: Option<&Mods>, todo: &mut Vec<Change>, merged: &mut Merged) {
    let mut marks = copy.marks.clone();
    marks.extend(merged.marks.iter().cloned());
    for o in &copy.owed {
        let later = marks.get(&o.key).is_some_and(|m| m.seq > o.seq) || todo.iter().any(|t| t.key == o.key);
        let unsent = sent.is_some_and(|s| s.state(&o.key) != here.state(&o.key));
        if later || unsent || here.state(&o.key) == o.now {
            continue;
        }
        let c = o.change();
        let kind = c.kind();
        let mark = Mark { state: o.now.clone(), kind, by: o.by.clone(), name: o.name.clone(), seq: o.seq, at: o.at };
        merged.marks.push((o.key.clone(), mark));
        merged.made.push((o.key.clone(), kind, o.name.clone()));
        todo.push(c);
    }
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
    let state_of = |m: &Mods, k: &str| m.state(k);
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
        let found: Vec<Change> = match copy.got.get(&from).and_then(|p| decode(&p.code).map(|m| (m, p.seq))) {
            Some((prev, prev_seq)) => {
                let mut found = changes(&prev, &now);
                // The same as in the last code taken from them, but changed
                // since: they changed it and back in codes this PC never
                // got (the relay keeps the newest only), so here it may
                // still be the way in between. Either made in this very
                // code (see `stamps_out`), or dated after that last code: an
                // undo they sent, then another code on top of it while this
                // PC was away. The verdict below still weighs it against
                // what's known here.
                for (k, s) in &g.stamps {
                    let made_now = s.seq == 0 && s.by == from;
                    let since = s.seq > prev_seq && s.seq <= g.seq;
                    if !(made_now || since) || found.iter().any(|c| c.key == *k) {
                        continue;
                    }
                    let was = will.get(k).cloned().unwrap_or_else(|| state_of(here, k));
                    let to = state_of(&now, k);
                    if was != to {
                        found.push(Change { key: k.clone(), was, now: to, entry: now.map.get(k).map(|(_, e)| e.clone()) });
                    }
                }
                found
            }
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
            // Changed here since the last code sent, unless the group's
            // last change of it is what's here: that was taken in from
            // a member, not made here (a publish that didn't get out yet,
            // as `sent_marks` sees it too).
            let unsent = sent.is_some_and(|s| state_of(s, &c.key) != state_of(here, &c.key))
                && copy.marks.get(&c.key).is_none_or(|m| m.state != state_of(here, &c.key));
            // A stamp "made in this code" dates the change by the code.
            let own = Stamp { seq: g.seq, by: from.clone() };
            let stamp = g.stamps.get(&c.key).map(|s| if s.seq == 0 && s.by == from { &own } else { s }).filter(|s| s.seq > 0 && s.seq <= g.seq);
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
                    out.packs.insert(c.key.clone(), now.pack.clone());
                    will.insert(c.key.clone(), c.now.clone());
                    todo.retain(|t| t.key != c.key);
                    todo.push(c);
                }
                // Already the way it says here, but it is later than a change
                // of the mod left out for its source: that change is
                // replaced, so it must not come back when the source is
                // confirmed. Only the mark is kept, nothing is made.
                Verdict::Same if copy.owed.iter().any(|o| o.key == c.key && o.seq < seq) => {
                    let m = Mark { state: c.now.clone(), kind: c.kind(), by, name, seq, at: g.at };
                    marks.insert(c.key.clone(), m.clone());
                    out.marks.push((c.key.clone(), m));
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
    let shipped = if todo.iter().any(|c| matches!(c.now, State::Removed | State::Unlisted)) { pack_ships(inst) } else { Default::default() };
    for (n, c) in todo.iter().enumerate() {
        job.check_cancelled()?;
        job.progress(n, todo.len());
        let at = find_addon(inst, &c.key);
        let name = at.map(|i| inst.addons[i].name.clone()).unwrap_or_else(|| c.key.clone());
        // A jar the pack itself ships stays, whatever the member's code
        // says: their copy may only have stopped listing it because it is
        // the pack's own file there (see `became_packs`), and without it the
        // game may not start. Turning it off still works.
        if let Some(i) = at.filter(|&i| removes(c) && ships_jar(&shipped, &inst.addons[i])) {
            inst.addons[i].from_pack = true;
            job.note(keeps_pack_jar_note(&name));
            continue;
        }
        // `merge` leaves these out already (and says so); any other caller
        // gets the same guard.
        if locked_out(inst, c) {
            job.note(format!("{name}: kept, you locked its version"));
            continue;
        }
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
                // A Modrinth pack names its mods by file, so a pack mod
                // updated here (no longer counted as the pack's) is found
                // by asking the pack first, below.
                (State::File { .. }, Some(i)) if !inst.addons[i].from_pack && !is_mr_pack(inst) => crate::install::remove(inst, i),
                // Back to the pack's own file: how an undone update or
                // removal of a pack mod reads in a member's code.
                (State::File { .. } | State::Removed, _) => match pack_file(inst, &c.key, job) {
                    Some(PackFile::Curse(project, file)) => put_back_pack_file(inst, project, file, job),
                    Some(PackFile::Modrinth(f)) => put_back_mr_file(inst, &f, job),
                    None => match at {
                        Some(i) if matches!(c.was, State::File { .. }) && !inst.addons[i].from_pack => crate::install::remove(inst, i),
                        _ => {
                            job.note(format!("{name}: putting back the modpack's own file isn't done by sync"));
                            Ok(())
                        }
                    },
                },
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

/// A pack version's own file of one mod: what "as the pack has it" means.
enum PackFile {
    /// A CurseForge pack's: (project, file).
    Curse(u64, u64),
    /// A Modrinth pack's index entry (its links and hash).
    Modrinth(crate::modpack::MrIndexFile),
}

fn is_mr_pack(inst: &Instance) -> bool {
    inst.pack.as_ref().is_some_and(|p| p.project_id != 0 && is_mr(p.project_id))
}

/// The file this instance's pack version has for the mod `key`. A
/// CurseForge pack's manifest lists it by project ("p:<project>"). A
/// Modrinth pack's index lists files: a code names its mods by file name
/// ("n:<file name>"), and a mod here updated to another file by its
/// Modrinth project ("p:<number>", read off the pack's download link).
/// None for a mod the pack doesn't have, or a list that can't be read.
fn pack_file(inst: &Instance, key: &str, job: &Job) -> Option<PackFile> {
    let p = inst.pack.as_ref().filter(|p| p.project_id != 0)?;
    if is_mr(p.project_id) {
        let (name, project) = match (key.strip_prefix("n:"), key.strip_prefix("p:")) {
            (Some(n), _) => (Some(n), None),
            (_, Some(p)) => (None, Some(p.parse::<u64>().ok()?)),
            _ => return None,
        };
        let index = mr_pack_index(inst, job)?;
        return crate::modpack::mr_index_file(&index, name, project).cloned().map(PackFile::Modrinth);
    }
    if p.file_id == 0 {
        return None;
    }
    let project: u64 = key.strip_prefix("p:")?.parse().ok()?;
    let m = crate::sharecode::curse_manifest(inst, p.project_id, p.file_id, job).ok()?;
    m.files.iter().find(|f| f.project_id == project).map(|f| PackFile::Curse(project, f.file_id))
}

#[cfg(test)]
thread_local! {
    // Tests hand in a Modrinth pack's index and its files' bytes by link.
    static TEST_MR: std::cell::RefCell<Option<(String, HashMap<String, Vec<u8>>)>> = const { std::cell::RefCell::new(None) };
}

fn mr_pack_index(inst: &Instance, job: &Job) -> Option<crate::modpack::MrIndex> {
    #[cfg(test)]
    if let Some(json) = TEST_MR.with(|t| t.borrow().as_ref().map(|(j, _)| j.clone())) {
        return serde_json::from_str(&json).ok();
    }
    crate::sharecode::mr_pack_index(inst, job).ok()
}

fn put_back_mr_file(inst: &mut Instance, f: &crate::modpack::MrIndexFile, job: &Job) -> anyhow::Result<()> {
    #[cfg(test)]
    if let Some(files) = TEST_MR.with(|t| t.borrow().as_ref().map(|(_, b)| b.clone())) {
        let download = move |url: &str, dest: &Path, e: &crate::cf::Expect, _: &Job| -> anyhow::Result<()> {
            let b = files.get(url).ok_or_else(|| anyhow::anyhow!("no file at {url}"))?;
            anyhow::ensure!(crate::modrinth::sha1(b) == e.sha1, "hash differs");
            std::fs::write(dest, b)?;
            Ok(())
        };
        return crate::modpack::put_back_mr_file(inst, f, job, &download, &|_| Default::default());
    }
    crate::modpack::put_back_mr_file_online(inst, f, job)
}

/// Puts the pack's own `file` of `project` back, on and counted as the
/// pack's again (whatever file of it is here now goes to History).
fn put_back_pack_file(inst: &mut Instance, project: u64, file: u64, job: &Job) -> anyhow::Result<()> {
    let same = inst.addons.iter().any(|a| a.project_id == project && a.file_id == file);
    if !same {
        crate::install::install_file(inst, project, file, job)?;
    }
    if let Some(i) = inst.addons.iter().position(|a| a.project_id == project) {
        inst.addons[i].from_pack = true;
        if !inst.addons[i].enabled {
            crate::install::set_enabled_later(inst, i, true)?;
        }
    }
    Ok(())
}

/// Whether making `c` here would take a mod's file away: a removal, or a
/// file of the member's no longer listed.
fn removes(c: &Change) -> bool {
    matches!((&c.was, &c.now), (_, State::Removed) | (State::File { .. }, State::Unlisted))
}

/// Whether `c` would take away a jar the pack itself ships (`shipped`:
/// [`pack_ships`]). Sync leaves those alone.
pub fn keeps_pack_jar(inst: &Instance, c: &Change, shipped: &std::collections::HashSet<String>) -> bool {
    removes(c) && find_addon(inst, &c.key).is_some_and(|i| ships_jar(shipped, &inst.addons[i]))
}

/// The task's note for a jar sync kept because the pack ships it.
fn keeps_pack_jar_note(name: &str) -> String {
    format!("{name}: kept, the modpack itself comes with this file")
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

/// "NeoForge 21.1.256" from "neoforge-21.1.256"; "Vanilla" for none.
pub fn loader_label(loader: &str) -> String {
    if loader.is_empty() {
        return "Vanilla".into();
    }
    let (kind, ver) = loader.split_once('-').unwrap_or((loader, ""));
    format!("{} {ver}", loader_name(kind)).trim().to_string()
}

/// "NeoForge" from "neoforge" (or "neoforge-21.1.256").
pub fn loader_name(kind: &str) -> String {
    let kind = kind.split_once('-').map_or(kind, |(k, _)| k);
    match kind.to_ascii_lowercase().as_str() {
        "neoforge" => "NeoForge".into(),
        "forge" => "Forge".into(),
        "fabric" => "Fabric".into(),
        "quilt" => "Quilt".into(),
        _ => kind.to_string(),
    }
}

fn loader_kind(loader: &str) -> String {
    loader.split_once('-').map_or(loader, |(k, _)| k).to_ascii_lowercase()
}

/// What a loader change from the group does to this instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoaderStep {
    /// Already the loader here.
    Nothing,
    /// Move to this loader.
    Set(String),
    /// Another kind of loader (NeoForge to Fabric): its mods and settings
    /// don't carry over, so it is left to the player. The new loader's
    /// name, to say so.
    OtherKind(String),
}

/// What to do with a loader change `c` here. `now`: this instance's loader;
/// `pack`: its modpack's own loader (None: not a pack, or not known), to
/// put back when the group went back to it. Only the version changes within
/// one kind of loader.
pub fn loader_step(now: &str, c: &Change, pack: Option<&str>) -> LoaderStep {
    let want = match &c.now {
        State::File { file, .. } => file.strip_prefix("loader:").unwrap_or(file).to_string(),
        _ => match pack.filter(|p| !p.is_empty()) {
            Some(p) => p.to_string(),
            None => return LoaderStep::Nothing,
        },
    };
    if want == now {
        LoaderStep::Nothing
    } else if loader_kind(&want) != loader_kind(now) {
        LoaderStep::OtherKind(loader_name(&want))
    } else {
        LoaderStep::Set(want)
    }
}

/// What a loader change says in the task and for the group's activity:
/// "Sam updated NeoForge to 21.1.256".
pub fn loader_says(who: &str, loader: &str, back_to_pack: bool) -> String {
    let who = if who.is_empty() { "Someone" } else { who };
    let (kind, ver) = loader.split_once('-').unwrap_or((loader, ""));
    let kind = loader_name(kind);
    match (back_to_pack, ver.is_empty()) {
        (true, _) => format!("{who} put the loader back to the pack's own ({})", loader_label(loader)),
        (false, true) => format!("{who} updated the loader to {kind}"),
        (false, false) => format!("{who} updated {kind} to {ver}"),
    }
}

/// The note for a loader the group moved to that this instance won't
/// follow.
pub fn loader_other_kind(who: &str, kind: &str) -> String {
    let who = if who.is_empty() { "Someone" } else { who };
    format!("{who} switched this pack to {kind}. Install it again from their share code to follow.")
}

/// Whether making `c` here would change anything: a switch, removal or
/// put-back of a mod this instance doesn't have does nothing, and isn't
/// told as a change.
pub fn does_something(inst: &Instance, c: &Change) -> bool {
    let here = find_addon(inst, &c.key);
    match (&c.now, &c.entry) {
        (State::Off | State::Removed, _) => here.is_some(),
        // A pack mod taken out here and put back by a member (an undone
        // removal) comes back as the pack has it.
        (State::Unlisted, _) => here.is_some() || c.was == State::Removed,
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

/// Moves the instance to the loader the group moved to (saved by the
/// caller), and says so in the task. True when it was moved.
fn move_loader(inst: &mut Instance, c: &Change, pack: Option<&str>, merged: &mut Merged, job: &Job) -> bool {
    let who = merged.made.iter().find(|(k, _, _)| k == LOADER_KEY).map(|(_, _, n)| n.clone()).unwrap_or_default();
    let step = loader_step(&inst.loader, c, pack);
    if step != LoaderStep::Nothing {
        job.show();
    }
    match step {
        LoaderStep::Set(l) => {
            job.note(loader_says(&who, &l, c.now == State::Unlisted));
            inst.loader = l.clone();
            merged.loader = Some(l);
            true
        }
        LoaderStep::OtherKind(kind) => {
            job.note(loader_other_kind(&who, &kind));
            merged.made.retain(|(k, _, _)| k != LOADER_KEY);
            false
        }
        LoaderStep::Nothing => {
            merged.made.retain(|(k, _, _)| k != LOADER_KEY);
            false
        }
    }
}

/// Takes the waiting codes into the instance at `root`: works out each
/// member's changes and makes the ones that win here. Runs off the UI
/// thread. `allow`: mods from other links may be put in by themselves.
/// `ignore_unsent`: a change here waited for its publish too long; the
/// other members' changes to that mod are dropped and the rest go in.
///
/// A pack version a member moved to (and that wins) goes in first, as a
/// whole pack update that keeps this player's edits; `pack_ok` false (the
/// game is running) leaves it, and everything else, for later
/// ([`Merged::pack_due`]).
pub fn merge(root: &Path, copy: &PackCopy, allow: bool, ignore_unsent: bool, pack_ok: bool, job: &Job) -> anyhow::Result<Merged> {
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
    // The jars the pack ships are the pack's before this copy is compared:
    // an older record (or one from before the pack's list was known) can
    // still have one as a mod of the player's, and a member whose copy
    // already knows it's the pack's no longer lists it.
    let shipped = pack_ships(&inst);
    if !shipped.is_empty() && crate::modpack::claim_override_jars(&mut inst, &shipped, false) {
        inst.save()?;
    }
    let made_here = crate::sharecode::make(&inst, job)?;
    let pack_loader = made_here.pack_loader;
    let here = made_here.share;
    let read = |c: &str| (!c.is_empty()).then(|| crate::sharecode::decode(c).ok()).flatten();
    let sent = read(&copy.sent);
    let theirs: Vec<Share> = copy.got.values().chain(copy.waiting.values()).filter_map(|g| read(&g.code)).collect();
    let mut all: Vec<&Share> = vec![&here];
    all.extend(sent.iter());
    all.extend(theirs.iter());
    let found = projects_of(&inst, &all);
    let read_mods = |s: &Share| mods(s, lookup(&found));
    let mut here = read_mods(&here);
    let mut sent = sent.as_ref().map(read_mods);
    let mut codes: HashMap<String, Mods> = HashMap::new();
    for g in copy.got.values().chain(copy.waiting.values()) {
        if let Some(s) = read(&g.code) {
            codes.insert(g.code.clone(), read_mods(&s));
        }
    }
    {
        let mut every: Vec<&mut Mods> = vec![&mut here];
        every.extend(sent.iter_mut());
        every.extend(codes.values_mut());
        fill_packs(&inst, &mut every, job);
    }
    let (todo, mut merged) = plan(copy, &here, sent.as_ref(), ignore_unsent, |c| codes.get(c).cloned());
    // The pack version goes first: a member's mod changes are relative to
    // their version, and the update may bring or take out mods itself.
    let (pack_change, todo): (Vec<Change>, Vec<Change>) = todo.into_iter().partition(|c| c.key == PACK_KEY);
    let mut here_pack = here.pack.clone();
    let mut pack_loader = pack_loader;
    if let Some(c) = pack_change.first() {
        match pack_step(&mut inst, root, c, &here.pack, pack_ok, &copy.group, &mut merged, job)? {
            PackRun::Nothing => {}
            PackRun::Waits => return Ok(Merged { pack_due: merged.pack_due, ..Default::default() }),
            PackRun::Done => {
                here_pack = c.now.clone();
                // The new version's own loader is the one to go back to.
                pack_loader = Some(inst.loader.clone());
            }
        }
    }
    let shipped = if here_pack != here.pack { pack_ships(&inst) } else { shipped };
    // The loader isn't a mod: it is moved here, not through `apply`.
    let (loader_change, todo): (Vec<Change>, Vec<Change>) = todo.into_iter().partition(|c| c.key == LOADER_KEY);
    let loader_set = loader_change.first().is_some_and(|c| move_loader(&mut inst, c, pack_loader.as_deref(), &mut merged, job));
    // Nor is a lock: it changes no file, so it is set here too.
    let (lock_changes, mut todo): (Vec<Change>, Vec<Change>) = todo.into_iter().partition(|c| lock_target(&c.key).is_some());
    let lock_set = set_locks(&mut inst, &lock_changes, &mut merged);
    // Changes left out before for a source the player hadn't confirmed:
    // made now when they have, left owed otherwise.
    revive_owed(copy, &here, sent.as_ref(), &mut todo, &mut merged);
    let todo = keep_pack_mods(&inst, todo, &mut merged, &here_pack, job);
    // Mods kept only on this PC only learn how the others have them.
    let (todo, kept) = crate::onlyhere::split(&mut inst, todo, &mut merged);
    // A mod the player locked keeps its file, whoever changed it.
    let todo = keep_locked(&inst, todo, &mut merged);
    // A mod from a link this PC hasn't confirmed isn't downloaded unasked;
    // the player is told which one and why (a switch of one already here
    // goes through).
    let wanted: Vec<crate::sources::carry::Pin> = todo.iter().filter_map(|c| pin_to_get(&inst, c).cloned()).collect();
    let mut skip = crate::sources::carry::auto_skip(&wanted, allow);
    // A mod the player said yes to goes in whatever the sources say.
    let approved: Vec<crate::sources::carry::Pin> = copy.owed.iter().filter(|o| o.approved).filter_map(Owed::pin).cloned().collect();
    skip.retain(|p| !approved.contains(p));
    let mut todo = keep_untrusted(&inst, todo, &skip, &mut merged);
    // What is still owed is what was left out just now; what was made, or
    // changed again since, no longer is.
    if !copy.owed.is_empty() || merged.owed.is_some() {
        merged.owed = Some(merged.owed.take().unwrap_or_default());
    }
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
    // A jar the pack ships isn't taken away (see `apply`); the task says
    // so, but it isn't told as the member's change.
    todo.retain(|c| {
        if !keeps_pack_jar(&inst, c, &shipped) {
            return true;
        }
        merged.made.retain(|(k, _, _)| *k != c.key);
        // Nor marked as theirs: the jar is still here, "Removed by" on it
        // would be wrong.
        merged.marks.retain(|(k, _)| *k != c.key);
        if let Some(i) = find_addon(&inst, &c.key) {
            job.show();
            job.note(keeps_pack_jar_note(&inst.addons[i].name));
        }
        false
    });
    if lock_set && let Err(e) = crate::cf_instance::write_locks(&mut inst) {
        // So the CurseForge app keeps the same mods back as well; best
        // effort, as when the player locks one.
        log::warn!("writing CurseForge locks for {}: {e:#}", inst.name);
    }
    if (kept || loader_set || lock_set || merged.pack_done.is_some()) && todo.is_empty() {
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
        // History names the member behind each change; only these mods
        // are theirs, whatever else changes here meanwhile.
        let _scope = crate::actor::begin(root, None);
        claim_changes(&inst, &todo, &merged, &copy.group);
        apply(&mut inst, &todo, &skip, job)?;
    }
    Ok(merged)
}

/// How the pack version step of a [`merge`] went.
enum PackRun {
    /// Nothing to do (already on it, or another pack altogether).
    Nothing,
    /// It can't go in now; [`Merged::pack_due`] says why.
    Waits,
    /// Updated; `inst` is the instance after it.
    Done,
}

/// Moves this instance to the pack version a member moved to (`c`), the
/// way the update dialog's "Keep my changes" does: the new pack zip, its
/// overrides and configs, this player's edits kept, one History entry
/// named after the member. `here`: this copy's pack version.
#[allow(clippy::too_many_arguments)]
fn pack_step(
    inst: &mut Instance,
    root: &Path,
    c: &Change,
    here: &State,
    pack_ok: bool,
    group: &str,
    merged: &mut Merged,
    job: &Job,
) -> anyhow::Result<PackRun> {
    use crate::grouppack::{Due, Step, Version, Wait};
    let mark = merged.marks.iter().rev().find(|(k, _)| k == PACK_KEY).map(|(_, m)| m.clone()).unwrap_or_default();
    let v = match crate::grouppack::step(here, c) {
        Step::Move(v) => v,
        Step::Nothing => {
            merged.made.retain(|(k, _, _)| k != PACK_KEY);
            return Ok(PackRun::Nothing);
        }
        Step::Other => {
            let who = if mark.name.is_empty() { "Someone" } else { mark.name.as_str() };
            job.show();
            job.note(format!("{who} has another pack in the group's copy now. Install it again from their share code to follow."));
            merged.made.retain(|(k, _, _)| k != PACK_KEY);
            return Ok(PackRun::Nothing);
        }
    };
    let (pid, fid) = v.ids();
    let pack_name = inst.pack.as_ref().map(|p| p.name.clone()).unwrap_or_default();
    // CurseForge numbers a pack's files in the order they came out.
    let back = matches!(v, Version::Curse { .. }) && inst.pack.as_ref().is_some_and(|p| fid < p.file_id);
    let file = crate::source::file(pid, fid);
    let version = file
        .as_ref()
        .ok()
        .map(|f| crate::grouppack::version_name(&pack_name, f))
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| if back { "an older version".into() } else { "a new version".into() });
    let found = file.as_ref().ok().cloned();
    let due = |wait| Due { to: v.clone(), file: found.clone(), version: version.clone(), pack: pack_name.clone(), who: mark.name.clone(), back, wait };
    let file = match file {
        Ok(f) => f,
        Err(e) => {
            merged.pack_due = Some(due(Wait::Failed(crate::grouppack::failed_says(&e))));
            return Ok(PackRun::Waits);
        }
    };
    // The store's map of running games stays empty for a few seconds after
    // Hopper starts, while a game may already run: look again here.
    if !pack_ok || crate::game::runs_in(root) {
        merged.pack_due = Some(due(Wait::Game));
        return Ok(PackRun::Waits);
    }
    job.show();
    job.detail(format!("Updating {} to {version}", if pack_name.is_empty() { "the pack" } else { pack_name.as_str() }));
    let updated = {
        // Whatever the new version changes is the member's in History.
        let _scope = crate::actor::begin(root, Some(crate::actor::Who::friend(&mark.name, &mark.by, group, "")));
        let _own = crate::actor::own_all();
        crate::modpack::update_group_pack(inst, pid, &file, job)
    };
    match updated {
        Ok(true) => {}
        Ok(false) => {
            merged.pack_due = Some(due(Wait::Pick));
            return Ok(PackRun::Waits);
        }
        Err(e) => {
            job.check_cancelled()?;
            merged.pack_due = Some(due(Wait::Failed(crate::grouppack::failed_says(&e))));
            return Ok(PackRun::Waits);
        }
    }
    *inst = crate::instances::load(root).with_context(|| format!("{} isn't an instance any more", root.display()))?;
    // The pack's own name for the version it now is, as the instance page
    // shows it.
    let version = inst.pack.as_ref().map(|p| p.version.trim().to_string()).filter(|v| !v.is_empty()).unwrap_or(version);
    let says = crate::grouppack::says_done(&mark.name, &pack_name, &version, back);
    job.note(says.clone());
    merged.pack_done = Some(says);
    Ok(PackRun::Done)
}

/// Leaves out of `todo` what a member on another pack version than this
/// copy (`here_pack`) did to a pack mod here, swapping its file or taking
/// it out: what their version has of it isn't this one's. The task says
/// so; it isn't marked as theirs.
fn keep_pack_mods(inst: &Instance, mut todo: Vec<Change>, merged: &mut Merged, here_pack: &State, job: &Job) -> Vec<Change> {
    todo.retain(|c| {
        if merged.packs.get(&c.key).is_none_or(|p| p == here_pack) || !swaps_pack_mod(inst, c) {
            return true;
        }
        let name = find_addon(inst, &c.key).map(|i| inst.addons[i].name.clone()).unwrap_or_default();
        job.show();
        job.note(format!("{name}: kept as the pack has it. Their copy is on another version of the pack."));
        merged.made.retain(|(k, _, _)| *k != c.key);
        merged.marks.retain(|(k, _)| *k != c.key);
        false
    });
    todo
}

/// Whether making `c` here would swap a pack mod's file for another or
/// take it out (turning it on or off is fine).
fn swaps_pack_mod(inst: &Instance, c: &Change) -> bool {
    let Some(i) = find_addon(inst, &c.key) else { return false };
    let a = &inst.addons[i];
    if !a.from_pack {
        return false;
    }
    match (&c.now, &c.entry) {
        (State::Removed, _) => true,
        (State::Unlisted, _) => matches!(c.was, State::File { .. }),
        (State::File { .. }, Some(Entry::CurseFile { file, .. })) => *file != a.file_id,
        (State::File { .. }, Some(Entry::ModrinthFile { version, .. })) => *version != a.mr_version && num_id(version) != a.file_id,
        (State::File { .. }, Some(Entry::Source { .. })) => true,
        _ => false,
    }
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
            // The pinned jar is here already (a member turned it off or on,
            // or an echo of an update made here): only the switch, with no
            // download. That needs no trust either, nothing is fetched.
            if let Some(i) = pinned_here(inst, at, pin) {
                if inst.addons[i].enabled != *enabled {
                    let name = inst.addons[i].name.clone();
                    switch(inst, i, *enabled, &name, job)?;
                }
                return Ok(());
            }
            if skip_sources.iter().any(|s| s.source.url == pin.source.url && s.source.pattern == pin.source.pattern) {
                job.note(format!("{}: not installed, you haven't confirmed its source on this PC", crate::sources::carry::display_name(pin)));
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
    fn project_of_file(e: &Entry) -> Option<u64> {
        match e {
            Entry::CurseFile { file, .. } => Some(file / 10),
            _ => None,
        }
    }
    fn dec(c: &str) -> Option<Mods> {
        crate::sharecode::decode(c).ok().map(|s| mods(&s, project_of_file))
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

    /// Seen on rig (Lithium): Sam turned 1 off (seq 10) and went away. Mia
    /// took it, turned 1 back on (seq 12), then turned 3 off (seq 14). The
    /// relay keeps her newest code only; in it the undo carries its stamp,
    /// and her last code Sam took (seq 5) had 1 on too.
    #[test]
    fn an_undo_sent_before_another_code_still_reaches_a_member_who_was_away() {
        let mut copy = PackCopy::default();
        copy.got.insert("mia".into(), got(code(vec![cf(11, true), cf(31, true)]), 5, "Mia"));
        copy.marks.insert("p:1".into(), Mark { state: st(11, false), seq: 10, by: "sam".into(), ..Default::default() });
        let mut g = got(code(vec![cf(11, true), cf(31, false)]), 14, "Mia");
        g.stamps.insert("p:1".into(), Stamp { seq: 12, by: "mia".into() });
        g.stamps.insert("p:3".into(), Stamp { seq: 0, by: "mia".into() });
        copy.arrive("mia", g);
        let here = m(vec![cf(11, false), cf(31, true)]);
        let (todo, merged) = plan(&copy, &here, Some(&here), false, dec);
        let mut got: Vec<(String, State)> = todo.iter().map(|c| (c.key.clone(), c.now.clone())).collect();
        got.sort();
        assert_eq!(got, [("p:1".to_string(), st(11, true)), ("p:3".to_string(), st(31, false))]);
        let m1 = merged.marks.iter().find(|(k, _)| k == "p:1").map(|(_, m)| m.clone()).unwrap();
        assert_eq!((m1.seq, m1.by.as_str(), m1.kind), (12, "mia", Kind::On));
    }

    /// The other side of the rule above: a stamp from before the last code
    /// taken, or older than the change known here, sets nothing back.
    #[test]
    fn an_older_stamp_on_an_unchanged_mod_undoes_nothing() {
        for stamp in [4, 8] {
            let mut copy = PackCopy::default();
            copy.got.insert("mia".into(), got(code(vec![cf(11, true), cf(31, true)]), 5, "Mia"));
            copy.marks.insert("p:1".into(), Mark { state: st(11, false), seq: 10, by: "sam".into(), ..Default::default() });
            let mut g = got(code(vec![cf(11, true), cf(31, false)]), 14, "Mia");
            g.stamps.insert("p:1".into(), Stamp { seq: stamp, by: "mia".into() });
            copy.arrive("mia", g);
            let here = m(vec![cf(11, false), cf(31, true)]);
            let (todo, _) = plan(&copy, &here, Some(&here), false, dec);
            assert_eq!(todo.iter().map(|c| c.key.clone()).collect::<Vec<_>>(), ["p:3"], "stamp {stamp}");
        }
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
    /// What every PC of the sim starts with.
    const LOADER: &str = "neoforge-21.1.200";
    const GROUP: &str = "g";

    struct Pc {
        id: String,
        mods: BTreeMap<u64, (u64, bool)>,
        /// The instance's loader.
        loader: String,
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
        /// Each sender's last code, as the group keeps it for whoever
        /// joins later (`group_pack_events` in the relay).
        packs: BTreeMap<String, Got>,
    }

    impl Relay {
        /// `id` says yes to the group: every member's last code reaches
        /// them, numbered with the joining.
        fn join(&mut self, id: &str) {
            self.seq += 1;
            let since = self.seq;
            let q = self.queues.entry(id.to_string()).or_default();
            for (from, g) in self.packs.iter().filter(|(from, _)| *from != id) {
                q.push((since, from.clone(), Got { seq: since, ..g.clone() }));
            }
        }
    }

    impl Pc {
        fn new(id: &str, sharer: bool, start: &[(u64, u64, bool)]) -> Pc {
            let local = if sharer { PACK.to_string() } else { format!("{id}-local") };
            let mods = start.iter().map(|(p, f, on)| (*p, (*f, *on))).collect();
            let copy = PackCopy { inst: PACK.into(), group: GROUP.into(), local, ..Default::default() };
            Pc { id: id.into(), mods, loader: LOADER.into(), copy, follow_code: String::new(), cursor: 0, merges: Vec::new() }
        }
        fn entries(&self) -> Vec<Entry> {
            self.mods.values().map(|(f, on)| cf(*f, *on)).collect()
        }
        fn share(&self) -> Share {
            Share {
                base: Base::Plain { game_version: "1.21.1".into(), loader: self.loader.clone() },
                name: None,
                loader: None,
                entries: self.entries(),
            }
        }
        fn code(&self) -> String {
            crate::sharecode::encode(&self.share())
        }
        fn mods(&self) -> Mods {
            mods(&self.share(), project_of_file)
        }
        fn set(&mut self, project: u64, file: u64, on: bool) {
            self.mods.insert(project, (file, on));
        }
        fn set_loader(&mut self, loader: &str) {
            self.loader = loader.into();
        }
        /// Publishes this copy if its code changed since the last time.
        fn publish(&mut self, relay: &mut Relay, all: &[&str], stamped: bool) {
            let now = self.code();
            if now == self.copy.sent {
                return;
            }
            let now_mods = self.mods();
            let stamps = if stamped { stamps_out(&self.copy.marks, dec(&self.copy.sent).as_ref(), &now_mods, &self.id) } else { BTreeMap::new() };
            for to in all.iter().filter(|t| **t != self.id) {
                relay.seq += 1;
                let q = relay.queues.entry(to.to_string()).or_default();
                q.retain(|(_, from, _)| *from != self.id);
                let g = Got { code: now.clone(), seq: relay.seq, at: relay.seq, name: self.id.clone(), stamps: stamps.clone() };
                q.push((relay.seq, self.id.clone(), g.clone()));
                relay.packs.insert(self.id.clone(), g);
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
            let here = self.mods();
            let sent = dec(&self.copy.sent);
            let (todo, merged) = plan(&self.copy, &here, sent.as_ref(), false, dec);
            for c in &todo {
                if c.key == LOADER_KEY {
                    // As `merge` does.
                    match loader_step(&self.loader, c, None) {
                        LoaderStep::Set(l) => self.loader = l,
                        LoaderStep::Nothing | LoaderStep::OtherKind(_) => {}
                    }
                    continue;
                }
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

    /// A and C aren't friends (both are B's). The relay gives every member
    /// each member's copy (`relay/test_hopper_mailbox.py`,
    /// `test_a_group_reaches_members_who_arent_friends`), so nothing here
    /// passes through B: each change goes straight to everyone, under the
    /// name of whoever made it.
    #[test]
    fn members_who_arent_friends_get_each_others_changes_and_a_late_member_gets_them_all() {
        let (mut relay, mut a, mut b, mut c) = group();
        // B is away the whole time.
        a.set(1, 11, false);
        a.set(4, 42, true);
        a.publish(&mut relay, &ALL, true);
        online(&mut c, &mut relay);
        assert_eq!((c.mods[&1], c.mods[&4]), ((11, false), (42, true)));
        let says = |pc: &Pc, k: &str| pc.copy.marks[k].kind.says(&pc.copy.marks[k].name);
        assert_eq!((says(&c, "p:1"), says(&c, "p:4")), ("Turned off by A".to_string(), "Updated by A".to_string()));
        c.set(2, 21, false);
        c.publish(&mut relay, &ALL, true);
        online(&mut a, &mut relay);
        assert_eq!(a.mods[&2], (21, false));
        assert_eq!(says(&a, "p:2"), "Turned off by C");
        // C turns 3 off while A is closed; then D, a friend of B only,
        // joins and installs the pack as A last shared it.
        c.set(3, 31, false);
        c.publish(&mut relay, &ALL, true);
        let all4 = ["A", "B", "C", "D"];
        let mut d = Pc::new("D", false, &a.mods.iter().map(|(p, (f, on))| (*p, *f, *on)).collect::<Vec<_>>());
        d.follow_code = a.code();
        relay.join("D");
        online_in(&mut d, &mut relay, &all4);
        let want: BTreeMap<u64, (u64, bool)> = [(1, (11, false)), (2, (21, false)), (3, (31, false)), (4, (42, true))].into_iter().collect();
        assert_eq!(d.mods, want, "D has the group's current state, C's late change too");
        assert_eq!(says(&d, "p:3"), "Turned off by C");
        // D undoes A's turning 1 off: everyone gets it, B on coming back.
        d.set(1, 11, true);
        d.publish(&mut relay, &all4, true);
        for _ in 0..3 {
            for pc in [&mut a, &mut b, &mut c, &mut d] {
                online_in(pc, &mut relay, &all4);
            }
        }
        let want: BTreeMap<u64, (u64, bool)> = [(1, (11, true)), (2, (21, false)), (3, (31, false)), (4, (42, true))].into_iter().collect();
        for pc in [&a, &b, &c, &d] {
            assert_eq!(pc.mods, want, "{} ends like the others", pc.id);
            assert_eq!(pc.copy.marks["p:1"].by, "D", "on {}", pc.id);
        }
        // D's own mark has no name (its own changes aren't "by" anyone).
        for pc in [&a, &b, &c] {
            assert_eq!(says(pc, "p:1"), "Turned on by D", "on {}", pc.id);
        }
        assert_eq!(says(&b, "p:4"), "Updated by A");
    }

    fn online_in(pc: &mut Pc, relay: &mut Relay, all: &[&str]) {
        pc.read(relay);
        pc.merge();
        pc.publish(relay, all, true);
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

    fn locked_inst(tag: &str) -> Instance {
        let root = std::env::temp_dir().join(format!("hopper-groupsync-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("mods")).unwrap();
        let mk = |project: u64, file: u64, name: &str, locked: bool| {
            std::fs::write(root.join("mods").join(format!("{name}.jar")), b"jar").unwrap();
            Addon { project_id: project, file_id: file, name: name.into(), file_name: format!("{name}.jar"), enabled: true, locked, ..Default::default() }
        };
        let addons = vec![mk(1, 11, "Sodium", true), mk(2, 21, "Iris", false)];
        let inst = Instance { root: root.clone(), name: "T".into(), addons, ..Default::default() };
        inst.save().unwrap();
        inst
    }

    fn ch(key: &str, was: State, now: State) -> Change {
        let entry = match &now {
            State::File { file, on } => Some(cf(file.trim_start_matches("cf:").parse().unwrap(), *on)),
            _ => None,
        };
        Change { key: key.into(), was, now, entry }
    }

    #[test]
    fn a_locked_mod_keeps_its_file_but_still_turns_on_and_off() {
        let inst = locked_inst("locked-rules");
        let out = |c: Change| locked_out(&inst, &c);
        assert!(out(ch("p:1", st(11, true), st(12, true))), "another version");
        assert!(out(ch("p:1", st(11, true), State::Removed)), "removed");
        assert!(out(ch("p:1", st(11, true), State::Unlisted)), "taken out / back to the pack's file");
        assert!(out(ch("p:1", st(12, false), st(12, true))), "the member's other version, even as a switch there");
        assert!(!out(ch("p:1", st(11, true), st(11, false))), "turned off");
        assert!(!out(ch("p:1", State::Unlisted, State::Off)), "pack mod turned off");
        assert!(!out(ch("p:1", State::Off, State::Unlisted)), "pack mod turned back on");
        assert!(!out(ch("p:2", st(21, true), st(22, true))), "not locked");
        assert!(!out(ch("p:9", State::Unlisted, st(91, true))), "not here");
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    #[test]
    fn a_group_change_to_a_locked_mod_is_left_out_and_said_why() {
        let inst = locked_inst("locked-merge");
        let mia = |seq| Mark { state: st(12, true), kind: Kind::Updated, by: "mia".into(), name: "Mia".into(), seq, at: seq };
        let mut merged = Merged::default();
        for (k, s) in [("p:1", 12), ("p:2", 13)] {
            merged.marks.push((k.into(), mia(s)));
            merged.made.push((k.into(), Kind::Updated, "Mia".into()));
        }
        let todo = vec![ch("p:1", st(11, true), st(12, true)), ch("p:2", st(21, true), st(22, true))];
        let left = keep_locked(&inst, todo, &mut merged);
        assert_eq!(left.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(), ["p:2"]);
        assert_eq!(merged.kept, [Kept { key: "p:1".into(), name: "Sodium".into(), kind: Kind::Updated, by: "Mia".into(), why: KeptWhy::Locked }]);
        assert_eq!(merged.kept[0].says(), "Sodium stays as it is: you locked its version. Mia changed its version.");
        assert!(!merged.marks.iter().any(|(k, _)| k == "p:1"), "no 'Updated by Mia' on it");
        assert!(!merged.made.iter().any(|(k, _, _)| k == "p:1"));
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    #[test]
    fn apply_never_removes_a_locked_mod() {
        let mut inst = locked_inst("locked-apply");
        let notes = apply_notes(&mut inst, &[ch("p:1", st(11, true), State::Removed)]);
        assert!(inst.root.join("mods").join("Sodium.jar").is_file());
        assert!(inst.addon_by_file("Sodium.jar").is_some());
        assert!(notes.iter().any(|n| n == "Sodium: kept, you locked its version"), "{notes:?}");
        // A switch still goes through.
        apply_notes(&mut inst, &[ch("p:1", st(11, true), st(11, false))]);
        assert!(!inst.addons[0].enabled, "turned off");
        let _ = std::fs::remove_dir_all(&inst.root);
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

    /// The sharer removed a mod while a member played. The member took it
    /// in, but the game held the jar, so it waited in the folder. Then a
    /// copy of the member's instance from before the change was saved over
    /// it (the play time written as the game closed; CurseForge writing its
    /// own file back reads the same). With the jar still there, the mod
    /// counted as installed again: the member's next code listed it as
    /// their own, newest change, and the sharer got it back.
    #[cfg(windows)]
    #[test]
    fn a_mod_removed_while_a_member_plays_does_not_come_back_when_they_close_the_game() {
        use std::os::windows::fs::OpenOptionsExt as _;
        let mut inst = locked_inst("removed-while-playing");
        let older = inst.clone();
        let jar = inst.root.join("mods").join("Iris.jar");
        // Open the way Java's JarFile does: no sharing of delete or rename.
        let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(&jar).unwrap();
        let jobs = crate::jobs::Jobs::default();
        let job = jobs.start("t");
        let code_of = |i: &Instance| crate::sharecode::make(i, &job).unwrap().code;
        let root = inst.root.clone();
        let load = || crate::instances::load(&root).unwrap();

        // The sharer (A) took Iris out at 5; the member (B) takes it in.
        let removal = Mark { state: State::Unlisted, kind: Kind::Removed, by: "A".into(), name: String::new(), seq: 5, at: 5 };
        apply(&mut inst, &[ch("p:2", st(21, true), State::Unlisted)], &[], &job).unwrap();
        assert!(find_addon(&inst, "p:2").is_none());
        assert!(jar.is_file(), "the game still has it");
        // The member's code without it, taken in by the sharer at 6.
        let member = PackCopy { marks: [("p:2".to_string(), removal.clone())].into(), ..Default::default() };
        let taken = code_of(&load());
        assert!(dec(&taken).unwrap().map.get("p:2").is_none());

        // The older copy lands on top while the game still has the jar.
        older.save().unwrap();
        let next = code_of(&load());
        let stamps = stamps_out(&member.marks, dec(&taken).as_ref(), &dec(&next).unwrap(), "B");

        // The sharer reads the member's next code (8).
        let here = m(vec![cf(11, true)]);
        let mut sharer = PackCopy { marks: [("p:2".to_string(), removal)].into(), ..Default::default() };
        sharer.got.insert("B".into(), got(taken.clone(), 6, "B"));
        sharer.arrive("B", Got { stamps, ..got(next.clone(), 8, "B") });
        let (todo, _) = plan(&sharer, &here, Some(&here), false, dec);
        assert!(todo.iter().all(|c| c.key != "p:2"), "Iris comes back: {todo:?}");
        assert_eq!(next, taken, "the member's code is unchanged");

        // The game closes: the jar goes, and the mod stays out.
        drop(held);
        assert_eq!(crate::locked::flush(&inst.root), 1);
        assert!(!jar.exists());
        assert!(find_addon(&load(), "p:2").is_none());
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    // ---- the loader goes the way the mods do ----

    /// A plain code of `loader` (no mods).
    fn plain(loader: &str) -> Share {
        Share { base: Base::Plain { game_version: "1.21.1".into(), loader: loader.into() }, name: None, loader: None, entries: Vec::new() }
    }
    /// A code of a CurseForge pack, with a loader only when it isn't the pack's.
    fn of_pack(loader: Option<&str>) -> Share {
        Share { base: Base::Curse { project: 9, file: 90 }, name: None, loader: loader.map(String::from), entries: Vec::new() }
    }
    fn loader_mods(s: &Share) -> Mods {
        mods(s, project_of_file)
    }
    fn loader_got(s: &Share, seq: u64, name: &str) -> Got {
        got(crate::sharecode::encode(s), seq, name)
    }
    fn loader_file(l: &str) -> State {
        State::File { file: format!("loader:{l}"), on: true }
    }
    fn loader_change(was: &str, now: &str) -> Change {
        Change { key: LOADER_KEY.into(), was: loader_file(was), now: loader_file(now), entry: None }
    }

    #[test]
    fn a_loader_version_change_is_one_updated_change() {
        let (was, now) = (loader_mods(&plain("neoforge-21.1.200")), loader_mods(&plain("neoforge-21.1.256")));
        let c = changes(&was, &now);
        assert_eq!(c.len(), 1);
        assert_eq!((c[0].key.as_str(), c[0].kind()), (LOADER_KEY, Kind::Updated));
        assert_eq!(c[0], loader_change("neoforge-21.1.200", "neoforge-21.1.256"));
        assert!(changes(&was, &was).is_empty());
        // A pack's code lists the loader only when it isn't the pack's: the
        // first override is still an update, not an addition.
        let (pack, set) = (loader_mods(&of_pack(None)), loader_mods(&of_pack(Some("neoforge-21.1.256"))));
        let c = changes(&pack, &set);
        assert_eq!((c.len(), c[0].kind()), (1, Kind::Updated));
        assert_eq!(set.state(LOADER_KEY), loader_file("neoforge-21.1.256"));
        // And back to the pack's own.
        let c = changes(&set, &pack);
        assert_eq!((c.len(), c[0].now.clone()), (1, State::Unlisted));
    }

    #[test]
    fn only_the_version_of_the_same_loader_is_followed() {
        let c = loader_change("neoforge-21.1.200", "neoforge-21.1.256");
        assert_eq!(loader_step("neoforge-21.1.200", &c, None), LoaderStep::Set("neoforge-21.1.256".into()));
        // Already there: an echo.
        assert_eq!(loader_step("neoforge-21.1.256", &c, None), LoaderStep::Nothing);
        // NeoForge and Forge are two loaders.
        let c = loader_change("neoforge-21.1.200", "forge-47.2.0");
        assert_eq!(loader_step("neoforge-21.1.200", &c, None), LoaderStep::OtherKind("Forge".into()));
        let c = loader_change("neoforge-21.1.200", "fabric-0.16.9");
        assert_eq!(loader_step("neoforge-21.1.200", &c, None), LoaderStep::OtherKind("Fabric".into()));
        // The group going back to the pack's own loader puts it back; with
        // the pack's loader already here (or unknown) nothing happens.
        let back = Change { key: LOADER_KEY.into(), was: c.now.clone(), now: State::Unlisted, entry: None };
        assert_eq!(loader_step("neoforge-21.1.256", &back, Some("neoforge-21.1.200")), LoaderStep::Set("neoforge-21.1.200".into()));
        assert_eq!(loader_step("neoforge-21.1.200", &back, Some("neoforge-21.1.200")), LoaderStep::Nothing);
        assert_eq!(loader_step("neoforge-21.1.256", &back, None), LoaderStep::Nothing);
    }

    #[test]
    fn the_packs_own_loader_named_outright_is_not_a_change_here() {
        // A code that names the pack's own loader (an older or odd sender)
        // differs from one that leaves it out, but changes nothing here.
        let c = changes(&loader_mods(&of_pack(None)), &loader_mods(&of_pack(Some("neoforge-21.1.200"))));
        assert_eq!(c.len(), 1);
        assert_eq!(loader_step("neoforge-21.1.200", &c[0], Some("neoforge-21.1.200")), LoaderStep::Nothing);
    }

    #[test]
    fn the_loader_says_who_and_what() {
        assert_eq!(loader_says("Sam", "neoforge-21.1.256", false), "Sam updated NeoForge to 21.1.256");
        assert_eq!(loader_says("", "fabric", false), "Someone updated the loader to Fabric");
        assert_eq!(loader_says("Sam", "neoforge-21.1.200", true), "Sam put the loader back to the pack's own (NeoForge 21.1.200)");
        assert_eq!(
            loader_other_kind("Alex", "Fabric"),
            "Alex switched this pack to Fabric. Install it again from their share code to follow."
        );
        assert_eq!(loader_label("neoforge-21.1.256"), "NeoForge 21.1.256");
    }

    #[test]
    fn a_loader_change_is_decided_like_a_mods() {
        let (was, now) = (plain("neoforge-21.1.200"), plain("neoforge-21.1.256"));
        let mut copy = PackCopy::default();
        copy.got.insert("mia".into(), loader_got(&was, 1, "Mia"));
        copy.arrive("mia", loader_got(&now, 10, "Mia"));
        let dec = |c: &str| crate::sharecode::decode(c).ok().map(|s| loader_mods(&s));
        let here = loader_mods(&was);
        let (todo, merged) = plan(&copy, &here, Some(&here), false, dec);
        assert_eq!(todo, [loader_change("neoforge-21.1.200", "neoforge-21.1.256")]);
        let (key, mark) = &merged.marks[0];
        assert_eq!((key.as_str(), mark.by.as_str(), mark.name.as_str(), mark.kind, mark.seq), (LOADER_KEY, "mia", "Mia", Kind::Updated, 10));
        assert_eq!(merged.made, [(LOADER_KEY.to_string(), Kind::Updated, "Mia".to_string())]);
        // Already the new one here (an echo): nothing.
        let here = loader_mods(&now);
        let (todo, merged) = plan(&copy, &here, Some(&here), false, dec);
        assert!(todo.is_empty() && merged.marks.is_empty());
        // A later change of the loader is known: Mia's older one loses.
        let mut copy2 = copy.clone();
        copy2.marks.insert(LOADER_KEY.into(), Mark { state: loader_file("neoforge-21.1.300"), seq: 20, by: "sam".into(), ..Default::default() });
        let here = loader_mods(&plain("neoforge-21.1.300"));
        let (todo, merged) = plan(&copy2, &here, Some(&here), false, dec);
        assert!(todo.is_empty());
        assert_eq!(merged.taken.len(), 1, "Mia's code is done with, not kept waiting");
        // Changed here and not sent yet: waits for the publish, like a mod.
        let (here, sent) = (loader_mods(&plain("neoforge-21.1.300")), loader_mods(&was));
        let (todo, merged) = plan(&copy, &here, Some(&sent), false, dec);
        assert!(todo.is_empty() && merged.held && merged.taken.is_empty());
        let (todo, merged) = plan(&copy, &here, Some(&sent), true, dec);
        assert!(todo.is_empty() && !merged.held && merged.taken.len() == 1, "dropped, not undone");
    }

    #[test]
    fn a_loader_bump_reaches_every_member_and_echoes_change_nothing() {
        let (mut relay, mut a, mut b, mut c) = group();
        a.set_loader("neoforge-21.1.256");
        a.publish(&mut relay, &ALL, true);
        online(&mut b, &mut relay);
        online(&mut c, &mut relay);
        assert_eq!((b.loader.as_str(), c.loader.as_str()), ("neoforge-21.1.256", "neoforge-21.1.256"));
        let mark = &c.copy.marks[LOADER_KEY];
        assert_eq!((mark.by.as_str(), mark.kind), ("A", Kind::Updated));
        // Everyone publishes the new code (their loader changed) and reads
        // the others' again: nothing more happens anywhere.
        for _ in 0..3 {
            for pc in [&mut a, &mut b, &mut c] {
                online(pc, &mut relay);
            }
        }
        assert_eq!((b.merges.len(), c.merges.len(), a.merges.len()), (1, 1, 0));
        assert_eq!((a.loader.as_str(), b.loader.as_str(), c.loader.as_str()), ("neoforge-21.1.256", "neoforge-21.1.256", "neoforge-21.1.256"));
        assert_eq!(a.mods, b.mods);
        // A member who was away gets it after a start, and the mods with it.
        let (mut relay, mut a, _b, mut c) = group();
        a.set_loader("neoforge-21.1.256");
        a.set(1, 11, false);
        a.publish(&mut relay, &ALL, true);
        c.restart();
        c.read(&relay);
        c.merge();
        assert_eq!((c.loader.as_str(), c.mods[&1]), ("neoforge-21.1.256", (11, false)));
    }

    #[test]
    fn two_members_changing_the_loader_end_on_the_later_one_everywhere() {
        for b_first in [true, false] {
            let (mut relay, mut a, mut b, mut c) = group();
            // Neither has read the other's yet.
            let (first, second) = if b_first { ("neoforge-21.1.256", "neoforge-21.1.300") } else { ("neoforge-21.1.300", "neoforge-21.1.256") };
            b.set_loader(first);
            b.publish(&mut relay, &ALL, true);
            c.set_loader(second);
            c.publish(&mut relay, &ALL, true);
            for _ in 0..3 {
                for pc in [&mut a, &mut b, &mut c] {
                    online(pc, &mut relay);
                }
            }
            assert_eq!((a.loader.as_str(), b.loader.as_str(), c.loader.as_str()), (second, second, second), "b first: {b_first}");
        }
    }

    #[test]
    fn a_member_whose_copy_is_behind_never_sets_the_loader_back() {
        let (mut relay, mut a, mut b, mut c) = group();
        a.set_loader("neoforge-21.1.256");
        a.publish(&mut relay, &ALL, true);
        online(&mut b, &mut relay);
        // C hasn't read it, and changes a mod: its code still has the old loader.
        c.set(2, 21, false);
        c.publish(&mut relay, &ALL, true);
        for _ in 0..3 {
            for pc in [&mut a, &mut b, &mut c] {
                online(pc, &mut relay);
            }
        }
        assert_eq!((a.loader.as_str(), b.loader.as_str(), c.loader.as_str()), ("neoforge-21.1.256", "neoforge-21.1.256", "neoforge-21.1.256"));
        assert_eq!((a.mods[&2], b.mods[&2]), ((21, false), (21, false)));
    }

    #[test]
    fn another_loader_is_not_followed_and_sets_nobody_back() {
        let (mut relay, mut a, mut b, mut c) = group();
        a.set_loader("fabric-0.16.9");
        a.publish(&mut relay, &ALL, true);
        online(&mut b, &mut relay);
        online(&mut c, &mut relay);
        assert_eq!((b.loader.as_str(), c.loader.as_str()), (LOADER, LOADER), "left as they are");
        // B changes a mod afterwards: its code still has its own loader,
        // which is no change of the loader, so A stays on Fabric.
        b.set(2, 21, false);
        b.publish(&mut relay, &ALL, true);
        for _ in 0..3 {
            for pc in [&mut a, &mut b, &mut c] {
                online(pc, &mut relay);
            }
        }
        assert_eq!(a.loader, "fabric-0.16.9");
        assert_eq!((b.loader.as_str(), c.loader.as_str()), (LOADER, LOADER));
        assert_eq!(a.mods[&2], (21, false), "the mod change still goes round");
    }

    /// A plain instance on disk with `loader`.
    fn loader_on_disk(tag: &str, loader: &str) -> Instance {
        let root = std::env::temp_dir().join(format!("hopper-groupsync-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("mods")).unwrap();
        let inst = Instance { root, name: "T".into(), game_version: "1.21.1".into(), loader: loader.into(), ..Default::default() };
        inst.save().unwrap();
        inst
    }

    fn merge_loader(tag: &str, here: &str, theirs: &str) -> (Merged, Instance, Vec<String>) {
        let inst = loader_on_disk(tag, here);
        let mut copy = PackCopy::default();
        copy.got.insert("mia".into(), loader_got(&plain(here), 1, "Mia"));
        copy.sent = crate::sharecode::encode(&plain(here));
        copy.arrive("mia", loader_got(&plain(theirs), 10, "Mia"));
        let jobs = crate::jobs::Jobs::default();
        let job = jobs.start("t");
        let merged = merge(&inst.root, &copy, false, false, true, &job).unwrap();
        let notes = jobs.snapshot().into_iter().flat_map(|j| j.notes).collect();
        let back = crate::instances::load(&inst.root).unwrap();
        let _ = std::fs::remove_dir_all(&inst.root);
        (merged, back, notes)
    }

    #[test]
    fn a_merge_moves_the_instance_to_the_groups_loader_and_says_so() {
        let (merged, inst, notes) = merge_loader("loader-up", "neoforge-21.1.200", "neoforge-21.1.256");
        assert_eq!(inst.loader, "neoforge-21.1.256", "saved");
        assert_eq!(merged.loader.as_deref(), Some("neoforge-21.1.256"));
        assert_eq!(merged.taken.len(), 1);
        assert!(notes.iter().any(|n| n == "Mia updated NeoForge to 21.1.256"), "{notes:?}");
    }

    #[test]
    fn a_merge_leaves_another_kind_of_loader_and_says_what_to_do() {
        let (merged, inst, notes) = merge_loader("loader-kind", "neoforge-21.1.200", "fabric-0.16.9");
        assert_eq!(inst.loader, "neoforge-21.1.200");
        assert_eq!(merged.loader, None);
        assert!(merged.made.is_empty());
        assert_eq!(merged.taken.len(), 1, "told once, not on every look");
        assert!(
            notes.iter().any(|n| n == "Mia switched this pack to Fabric. Install it again from their share code to follow."),
            "{notes:?}"
        );
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

    /// ATM10 ships CC: Tweaked 1.120.2 in its overrides; CurseForge matched
    /// that jar to a stranger's re-upload (project 1676502, file 16765020).
    const CC: &str = "cc-tweaked-1.21.1-forge-1.120.2.jar";
    const STRANGER: u64 = 1676502;
    const STRANGER_FILE: u64 = 16765020;

    /// A member's ATM10 on disk with the override jar recorded as the
    /// stranger's project (`from_pack` as an older Hopper left it).
    fn atm10_on_disk(tag: &str, from_pack: bool) -> Instance {
        let root = std::env::temp_dir().join(format!("hopper-groupsync-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("mods")).unwrap();
        std::fs::write(root.join("mods").join(CC), b"jar").unwrap();
        std::fs::write(root.join("minecraftinstance.json"), format!(r#"{{"modpackOverrides":["mods\\{CC}","config\\a.toml"]}}"#)).unwrap();
        let a = Addon {
            project_id: STRANGER,
            file_id: STRANGER_FILE,
            name: "CC Tweaked Compat".into(),
            file_name: CC.into(),
            enabled: true,
            from_pack,
            ..Default::default()
        };
        let pack = crate::instances::PackRef { project_id: 925200, file_id: 9431424, ..Default::default() };
        let inst = Instance { root, name: "ATM10".into(), pack: Some(pack), addons: vec![a], ..Default::default() };
        inst.save().unwrap();
        inst
    }

    fn atm10_mods(entries: Vec<Entry>) -> Mods {
        mods(&share(entries), |e| match e {
            Entry::CurseFile { file, .. } if *file == STRANGER_FILE => Some(STRANGER),
            Entry::CurseFile { file, .. } => Some(file / 10),
            _ => None,
        })
    }

    /// The leader's code before 0.72.1 listed the jar as a mod of theirs;
    /// the next one doesn't (the jar is the pack's there now). What a
    /// follower makes of that, run through the real plan.
    fn leader_stopped_listing_the_jar() -> Vec<Change> {
        let old = crate::sharecode::encode(&share(vec![cf(STRANGER_FILE, true)]));
        let new = crate::sharecode::encode(&share(vec![]));
        let mut copy = PackCopy::default();
        copy.got.insert("lead".into(), Got { code: old.clone(), seq: 5, name: "Lead".into(), ..Default::default() });
        copy.sent = old.clone();
        copy.arrive("lead", Got { code: new, seq: 10, name: "Lead".into(), ..Default::default() });
        let here = atm10_mods(vec![cf(STRANGER_FILE, true)]);
        let (todo, merged) = plan(&copy, &here, Some(&here), false, |c| crate::sharecode::decode(c).ok().map(|s| {
            atm10_mods(s.entries)
        }));
        assert_eq!(merged.made, [(format!("p:{STRANGER}"), Kind::Removed, "Lead".to_string())], "the bug's change");
        todo
    }

    fn apply_notes(inst: &mut Instance, todo: &[Change]) -> Vec<String> {
        let jobs = crate::jobs::Jobs::default();
        let job = jobs.start("t");
        apply(inst, todo, &[], &job).unwrap();
        jobs.snapshot().into_iter().flat_map(|j| j.notes).collect()
    }

    #[test]
    fn sync_keeps_a_jar_the_pack_ships_when_the_leader_stops_listing_it() {
        for from_pack in [false, true] {
            let todo = leader_stopped_listing_the_jar();
            let mut inst = atm10_on_disk(&format!("keep-{from_pack}"), from_pack);
            let notes = apply_notes(&mut inst, &todo);
            assert!(inst.root.join("mods").join(CC).is_file(), "from_pack {from_pack}: the jar stays");
            let a = inst.addon_by_file(CC).expect("still recorded");
            assert!(a.from_pack, "and is the pack's now");
            assert!(notes.iter().any(|n| n == "CC Tweaked Compat: kept, the modpack itself comes with this file"), "{notes:?}");
            assert!(keeps_pack_jar(&inst, &todo[0], &pack_ships(&inst)));
            let _ = std::fs::remove_dir_all(&inst.root);
        }
    }

    #[test]
    fn sync_never_removes_a_jar_the_pack_ships_but_turns_it_off() {
        let key = format!("p:{STRANGER}");
        let mut inst = atm10_on_disk("keep-removed", false);
        let removed = Change { key: key.clone(), was: State::Unlisted, now: State::Removed, entry: None };
        apply_notes(&mut inst, &[removed]);
        assert!(inst.root.join("mods").join(CC).is_file(), "an outright removal keeps it too");
        let off = Change { key, was: State::Unlisted, now: State::Off, entry: None };
        apply_notes(&mut inst, &[off]);
        assert!(inst.root.join("mods").join(format!("{CC}.disabled")).is_file(), "turning it off still works");
        assert!(!inst.addon_by_file(CC).unwrap().enabled);
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    #[test]
    fn a_mod_the_pack_doesnt_ship_is_still_removed() {
        let todo = leader_stopped_listing_the_jar();
        let mut inst = atm10_on_disk("not-shipped", false);
        std::fs::write(inst.root.join("minecraftinstance.json"), r#"{"modpackOverrides":["config\\a.toml"]}"#).unwrap();
        apply_notes(&mut inst, &todo);
        assert!(!inst.root.join("mods").join(CC).exists(), "the leader's own mod, taken out");
        assert!(inst.addon_by_file(CC).is_none());
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    #[test]
    fn the_leader_sends_no_removal_for_a_jar_that_became_the_packs() {
        let inst = atm10_on_disk("sender", true);
        let shipped = pack_ships(&inst);
        // As claim_override_jars leaves it: named after the pack's entry,
        // with no project of its own.
        let mut claimed = inst.clone();
        let a = &mut claimed.addons[0];
        (a.project_id, a.file_id, a.shown_as, a.name) = (0, 0, 282001, "CC: Tweaked".into());
        let prev = atm10_mods(vec![cf(STRANGER_FILE, true), cf(30, true)]);
        let now = atm10_mods(vec![]);
        let name_of = |e: &Entry| matches!(e, Entry::CurseFile { file, .. } if *file == STRANGER_FILE).then(|| CC.to_string());
        let key = format!("p:{STRANGER}");
        for here in [&inst, &claimed] {
            let marks = new_marks(here, &BTreeMap::new(), &prev, &now, &shipped, ("lead", 10, 10), name_of);
            assert!(!marks.iter().any(|(k, _)| *k == key), "no Removed for the pack's jar: {marks:?}");
            // A mod really taken out is still told.
            assert!(marks.iter().any(|(k, m)| k == "p:3" && m.kind == Kind::Removed), "{marks:?}");
        }
        // Not one of the pack's files: a removal like any other.
        let marks = new_marks(&claimed, &BTreeMap::new(), &prev, &now, &Default::default(), ("lead", 10, 10), name_of);
        assert!(marks.iter().any(|(k, m)| *k == key && m.kind == Kind::Removed));
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    // ---- the pack version ------------------------------------------------------------

    /// Pack project 1. Version 85 ships mod 6 (file 61); version 86 ships
    /// mod 6 (file 62) and mod 5 (file 51), which members had added
    /// themselves on 85.
    fn ships_of(file: u64) -> crate::grouppack::Ships {
        match file {
            85 => ["p:6"].iter().map(|s| s.to_string()).collect(),
            86 => ["p:6", "p:5"].iter().map(|s| s.to_string()).collect(),
            _ => Default::default(),
        }
    }
    fn pshare(file: u64, entries: Vec<Entry>) -> Share {
        Share { base: Base::Curse { project: 1, file }, name: None, loader: None, entries }
    }
    fn pcode(file: u64, entries: Vec<Entry>) -> String {
        crate::sharecode::encode(&pshare(file, entries))
    }
    /// A code read the way `merge` reads it: with what its pack version
    /// ships, when that could be looked up.
    fn pmods_of(s: &Share, known: bool) -> Mods {
        let mut m = mods(s, project_of_file);
        if let Base::Curse { file, .. } = s.base
            && known
        {
            m.ships = Some(ships_of(file));
        }
        m
    }
    fn pmods(file: u64, entries: Vec<Entry>) -> Mods {
        pmods_of(&pshare(file, entries), true)
    }
    fn pdec(c: &str) -> Option<Mods> {
        crate::sharecode::decode(c).ok().map(|s| pmods_of(&s, true))
    }
    fn pver(file: u64) -> State {
        crate::grouppack::pack_state(&Base::Curse { project: 1, file })
    }
    fn pack_change(todo: &[Change]) -> Option<&Change> {
        todo.iter().find(|c| c.key == PACK_KEY)
    }

    #[test]
    fn a_pack_version_change_is_one_updated_change() {
        let c = changes(&pmods(85, vec![cf(71, true)]), &pmods(86, vec![cf(71, true)]));
        assert_eq!(c.len(), 1);
        assert_eq!((c[0].key.as_str(), c[0].kind()), (PACK_KEY, Kind::Updated));
        assert_eq!((c[0].was.clone(), c[0].now.clone()), (pver(85), pver(86)));
        // A plain code has no pack version: nothing to compare.
        assert!(changes(&m(vec![cf(11, true)]), &m(vec![cf(11, true)])).is_empty());
    }

    #[test]
    fn a_mod_the_new_pack_version_ships_is_not_removed() {
        // The leader added mod 5 on 85; 86 ships it, so their code stops
        // listing it. That is no removal: followers on 85 keep it.
        let was = pmods(85, vec![cf(51, true), cf(71, true)]);
        let c = changes(&was, &pmods(86, vec![cf(71, true)]));
        assert_eq!(c.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(), [PACK_KEY]);
        // What 86 ships couldn't be looked up: still never a removal.
        let c = changes(&was, &pmods_of(&pshare(86, vec![cf(71, true)]), false));
        assert_eq!(c.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(), [PACK_KEY]);
        // A mod 86 doesn't ship that left the code was removed.
        let c = changes(&was, &pmods(86, vec![cf(51, true)]));
        let gone: Vec<_> = c.iter().filter(|c| c.key != PACK_KEY).map(|c| (c.key.as_str(), c.kind())).collect();
        assert_eq!(gone, [("p:7", Kind::Removed)]);
        // Within one version the old rule holds.
        let c = changes(&pmods(85, vec![cf(51, true)]), &pmods(85, vec![]));
        assert_eq!(c.iter().map(|c| (c.key.as_str(), c.kind())).collect::<Vec<_>>(), [("p:5", Kind::Removed)]);
    }

    #[test]
    fn a_jar_the_new_version_ships_by_name_is_not_removed() {
        let mut was = pmods(85, vec![cf(51, true)]);
        was.names.insert("p:5".into(), "fivemod-1.0.jar".into());
        let mut now = pmods(86, vec![]);
        now.ships = Some(["n:fivemod-1.0.jar".to_string()].into_iter().collect());
        assert_eq!(changes(&was, &now).iter().map(|c| c.key.as_str()).collect::<Vec<_>>(), [PACK_KEY]);
    }

    #[test]
    fn a_pack_mod_taken_out_that_the_new_version_drops_is_no_change() {
        // On 85 they took out pack mod 6; 87 doesn't have it at all.
        let c = changes(&pmods(85, vec![Entry::Remove(Target::Curse(6))]), &pmods(87, vec![]));
        assert_eq!(c.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(), [PACK_KEY]);
        // 86 still has it: putting it back is theirs.
        let c = changes(&pmods(85, vec![Entry::Remove(Target::Curse(6))]), &pmods(86, vec![]));
        assert_eq!(c.iter().filter(|c| c.key != PACK_KEY).count(), 1);
    }

    fn pcopy() -> PackCopy {
        PackCopy { inst: "pack".into(), group: "g".into(), local: "here".into(), ..Default::default() }
    }

    #[test]
    fn a_member_ahead_on_the_pack_is_followed_without_deleting_their_added_mod() {
        // McFawx (A) and this PC both on 85 with mod 5 added; McFawx moves
        // to 86, which ships mod 5.
        let mut copy = pcopy();
        copy.got.insert("A".into(), got(pcode(85, vec![cf(51, true)]), 3, "McFawx"));
        copy.arrive("A", got(pcode(86, vec![]), 9, "McFawx"));
        let here = pmods(85, vec![cf(51, true)]);
        let (todo, merged) = plan(&copy, &here, Some(&here), false, pdec);
        assert_eq!(todo.len(), 1, "{todo:?}");
        let c = pack_change(&todo).expect("the pack version");
        assert_eq!(c.now, pver(86));
        assert_eq!(merged.made, [(PACK_KEY.to_string(), Kind::Updated, "McFawx".to_string())]);
        assert_eq!(crate::grouppack::step(&here.pack, c), crate::grouppack::Step::Move(crate::grouppack::Version::Curse { project: 1, file: 86 }));
        // Once this PC is on 86 too, McFawx's next code is an echo.
        let mut copy = copy.clone();
        copy.got.insert("A".into(), merged.taken[0].1.clone());
        copy.waiting.clear();
        copy.marks.extend(merged.marks.iter().cloned());
        copy.arrive("A", got(pcode(86, vec![cf(81, true)]), 12, "McFawx"));
        let (todo, _) = plan(&copy, &pmods(86, vec![]), None, false, pdec);
        assert_eq!(todo.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(), ["p:8"]);
    }

    #[test]
    fn a_member_behind_on_the_pack_never_moves_it_back() {
        // This PC is on 86 (McFawx moved it at 9); Kim is still on 85 and
        // only adds a mod.
        let mut copy = pcopy();
        copy.marks.insert(PACK_KEY.into(), Mark { state: pver(86), kind: Kind::Updated, by: "A".into(), name: "McFawx".into(), seq: 9, at: 9 });
        copy.got.insert("K".into(), got(pcode(85, vec![cf(51, true)]), 4, "Kim"));
        copy.arrive("K", got(pcode(85, vec![cf(51, true), cf(71, true)]), 11, "Kim"));
        let here = pmods(86, vec![]);
        let (todo, _) = plan(&copy, &here, Some(&here), false, pdec);
        assert!(pack_change(&todo).is_none(), "{todo:?}");
        assert_eq!(todo.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(), ["p:7"]);
    }

    #[test]
    fn moving_the_pack_back_is_a_change_and_the_latest_move_wins() {
        // McFawx moves 85 -> 86 at 5, Mia moves 86 -> 85 at 8: back to 85.
        let mut copy = pcopy();
        copy.got.insert("A".into(), got(pcode(85, vec![]), 1, "McFawx"));
        copy.got.insert("M".into(), got(pcode(86, vec![]), 2, "Mia"));
        copy.arrive("A", got(pcode(86, vec![]), 5, "McFawx"));
        copy.arrive("M", got(pcode(85, vec![]), 8, "Mia"));
        let here = pmods(85, vec![]);
        let (todo, merged) = plan(&copy, &here, Some(&here), false, pdec);
        let c = pack_change(&todo).unwrap();
        assert_eq!(c.now, pver(85));
        assert_eq!(merged.marks.last().map(|(_, m)| m.name.as_str()), Some("Mia"));
        assert_eq!(crate::grouppack::step(&here.pack, c), crate::grouppack::Step::Nothing);
        // The other way round: Mia's move back came first, McFawx's later.
        let mut copy = pcopy();
        copy.got.insert("A".into(), got(pcode(85, vec![]), 1, "McFawx"));
        copy.got.insert("M".into(), got(pcode(86, vec![]), 2, "Mia"));
        copy.arrive("M", got(pcode(85, vec![]), 5, "Mia"));
        copy.arrive("A", got(pcode(86, vec![]), 8, "McFawx"));
        let here = pmods(86, vec![]);
        let (todo, _) = plan(&copy, &here, Some(&here), false, pdec);
        assert_eq!(pack_change(&todo).unwrap().now, pver(86));
        // A later move back on its own reaches a PC on the newer version.
        let mut copy = pcopy();
        copy.got.insert("A".into(), got(pcode(86, vec![]), 5, "McFawx"));
        copy.marks.insert(PACK_KEY.into(), Mark { state: pver(86), kind: Kind::Updated, by: "A".into(), name: "McFawx".into(), seq: 5, at: 5 });
        copy.arrive("A", got(pcode(85, vec![]), 9, "McFawx"));
        let here = pmods(86, vec![]);
        let (todo, _) = plan(&copy, &here, Some(&here), false, pdec);
        let c = pack_change(&todo).unwrap();
        assert_eq!(crate::grouppack::step(&here.pack, c), crate::grouppack::Step::Move(crate::grouppack::Version::Curse { project: 1, file: 85 }));
    }

    #[test]
    fn a_pack_version_changed_here_and_not_sent_holds_the_others_back() {
        let mut copy = pcopy();
        copy.got.insert("A".into(), got(pcode(85, vec![]), 1, "McFawx"));
        copy.arrive("A", got(pcode(86, vec![]), 5, "McFawx"));
        // This PC moved to 87 itself and hasn't published it yet.
        let (todo, merged) = plan(&copy, &pmods(87, vec![]), Some(&pmods(85, vec![])), false, pdec);
        assert!(todo.is_empty());
        assert!(merged.held);
    }

    #[test]
    fn a_pack_version_taken_from_the_group_and_not_sent_yet_holds_nothing_back() {
        // This PC followed McFawx to 86; its own code still says 85 (the
        // mailbox was down, so it never went out). McFawx's next move, to
        // 87, was held as if 86 were this PC's own unsent change, then
        // dropped after the wait: no notice and no update, ever.
        let mut copy = pcopy();
        copy.got.insert("A".into(), got(pcode(86, vec![]), 5, "McFawx"));
        copy.marks.insert(PACK_KEY.into(), Mark { state: pver(86), kind: Kind::Updated, by: "A".into(), name: "McFawx".into(), seq: 5, at: 5 });
        copy.arrive("A", got(pcode(87, vec![]), 9, "McFawx"));
        let (todo, merged) = plan(&copy, &pmods(86, vec![]), Some(&pmods(85, vec![])), false, pdec);
        assert!(!merged.held);
        assert_eq!(pack_change(&todo).map(|c| c.now.clone()), Some(pver(87)));
        // The same for a mod: McFawx's change taken in, then changed again.
        let mut copy = pcopy();
        copy.got.insert("A".into(), got(pcode(86, vec![cf(51, true)]), 5, "McFawx"));
        copy.marks.insert("p:5".into(), Mark { state: State::File { file: "cf:51".into(), on: true }, kind: Kind::Added, by: "A".into(), name: "McFawx".into(), seq: 5, at: 5 });
        copy.arrive("A", got(pcode(86, vec![cf(51, false)]), 9, "McFawx"));
        let (todo, merged) = plan(&copy, &pmods(86, vec![cf(51, true)]), Some(&pmods(86, vec![])), false, pdec);
        assert!(!merged.held);
        assert_eq!(todo.iter().map(|c| (c.key.as_str(), c.kind())).collect::<Vec<_>>(), [("p:5", Kind::Off)]);
        // A change this PC made itself still holds the others back.
        let mut copy = pcopy();
        copy.got.insert("A".into(), got(pcode(85, vec![]), 1, "McFawx"));
        copy.arrive("A", got(pcode(86, vec![]), 5, "McFawx"));
        let (_, merged) = plan(&copy, &pmods(87, vec![]), Some(&pmods(85, vec![])), false, pdec);
        assert!(merged.held);
    }

    #[test]
    fn a_pack_move_goes_out_stamped_and_a_new_member_takes_it() {
        let mut marks = BTreeMap::new();
        marks.insert(PACK_KEY.to_string(), Mark { state: pver(86), kind: Kind::Updated, by: "A".into(), name: String::new(), seq: 9, at: 9 });
        let stamps = stamps_of(&marks, &pmods(86, vec![]));
        assert_eq!(stamps.get(PACK_KEY).map(|s| s.seq), Some(9));
        // A member's first code: only its dated changes are taken, and the
        // pack version is one.
        let mut copy = pcopy();
        let mut g = got(pcode(86, vec![]), 12, "McFawx");
        g.stamps = stamps;
        copy.arrive("A", g);
        let (todo, _) = plan(&copy, &pmods(85, vec![]), None, false, pdec);
        assert_eq!(pack_change(&todo).map(|c| c.now.clone()), Some(pver(86)));
    }

    #[test]
    fn a_member_on_another_version_never_swaps_or_takes_out_a_pack_mod() {
        let jobs = crate::jobs::Jobs::new(crate::waker::noop());
        let job = jobs.start_quiet("test");
        let addon = |p: u64, f: u64, from_pack: bool| Addon { project_id: p, file_id: f, file_name: format!("m{p}.jar"), name: format!("Mod {p}"), from_pack, enabled: true, ..Default::default() };
        let inst = Instance { addons: vec![addon(6, 62, true), addon(7, 71, false)], ..Default::default() };
        // Kim (on 85) moved pack mod 6 to another file, took out mod 7
        // (hers to take out: not the pack's) and added mod 9.
        let todo = vec![
            Change { key: "p:6".into(), was: State::Unlisted, now: st(63, true), entry: Some(cf(63, true)) },
            Change { key: "p:7".into(), was: st(71, true), now: State::Unlisted, entry: None },
            Change { key: "p:9".into(), was: State::Unlisted, now: st(91, true), entry: Some(cf(91, true)) },
        ];
        let mut merged = Merged::default();
        for c in &todo {
            merged.packs.insert(c.key.clone(), pver(85));
            merged.made.push((c.key.clone(), c.kind(), "Kim".into()));
        }
        let kept = keep_pack_mods(&inst, todo.clone(), &mut merged, &pver(86), &job);
        assert_eq!(kept.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(), ["p:7", "p:9"]);
        assert!(merged.made.iter().all(|(k, _, _)| k != "p:6"));
        // On the same version, a swap is theirs to make.
        let mut merged = Merged::default();
        for c in &todo {
            merged.packs.insert(c.key.clone(), pver(86));
        }
        assert_eq!(keep_pack_mods(&inst, todo, &mut merged, &pver(86), &job).len(), 3);
        job.finish(&Ok::<(), anyhow::Error>(()));
    }

    /// Undo from History is a change like any other: B updates mod 2, A
    /// takes it, then B (or A) undoes it. The undo reaches everyone and
    /// the earlier update never comes back, whoever made it and however
    /// quickly the undo follows.
    #[test]
    fn an_undone_update_stays_undone_everywhere() {
        for (undoer, quick) in [("B", false), ("A", false), ("B", true), ("A", true)] {
            let (mut relay, mut a, mut b, mut c) = group();
            b.set(2, 22, true);
            b.publish(&mut relay, &ALL, true);
            online(&mut a, &mut relay);
            if !quick {
                online(&mut c, &mut relay);
                online(&mut b, &mut relay);
            }
            let u = if undoer == "A" { &mut a } else { &mut b };
            u.set(2, 21, true);
            u.publish(&mut relay, &ALL, true);
            for _ in 0..4 {
                for pc in [&mut a, &mut b, &mut c] {
                    online(pc, &mut relay);
                }
            }
            for pc in [&a, &b, &c] {
                assert_eq!(pc.mods[&2], (21, true), "{undoer} undid (quick: {quick}); {} has it back", pc.id);
            }
        }
    }

    /// A member undoing their update (or removal) of a pack mod sends it as
    /// "as the pack has it" (no longer listed). Here that is the file this
    /// instance's CurseForge pack version lists, not "leave it be".
    #[test]
    fn an_undone_pack_mod_change_goes_back_to_the_packs_own_file() {
        let dir = std::env::temp_dir().join(format!("hopper-gs-packfile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("mods")).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            r#"{"name":"P","version":"1.0","projectID":500,"files":[{"projectID":7,"fileID":71,"required":true}],"overrides":"overrides"}"#,
        )
        .unwrap();
        let pack = crate::instances::PackRef { project_id: 500, file_id: 5001, version: "1.0".into(), ..Default::default() };
        let inst = Instance { root: dir.clone(), pack: Some(pack), ..Default::default() };
        let jobs = crate::jobs::Jobs::new(crate::waker::noop());
        let job = jobs.start_quiet("test");
        assert!(matches!(pack_file(&inst, "p:7", &job), Some(PackFile::Curse(7, 71))));
        assert!(pack_file(&inst, "p:8", &job).is_none(), "not one of the pack's");
        assert!(pack_file(&inst, "n:x.jar", &job).is_none());
        // Removed here, put back by the member: it does something.
        let back = Change { key: "p:7".into(), was: State::Removed, now: State::Unlisted, entry: None };
        assert!(does_something(&inst, &back));
        // Already the pack's file but off here: turned on, the pack's again.
        let mut inst = inst;
        std::fs::write(dir.join("mods").join("m.jar.disabled"), b"x").unwrap();
        inst.addons.push(Addon { project_id: 7, file_id: 71, file_name: "m.jar".into(), enabled: false, class_id: crate::cf::CLASS_MODS, ..Default::default() });
        put_back_pack_file(&mut inst, 7, 71, &job).unwrap();
        assert!(inst.addons[0].enabled && inst.addons[0].from_pack);
        job.finish(&Ok::<(), anyhow::Error>(()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same for a Modrinth pack, whose codes name pack mods by file
    /// ("n:<file name>") and whose files come from the pack's own links,
    /// not a CurseForge file id. A member undid their update of a pack mod
    /// (or its removal): here the pack's exact old file comes back from the
    /// pack's link, hash-checked, on and the pack's, in place of the update.
    #[test]
    fn an_undone_modrinth_pack_mod_change_goes_back_to_the_packs_own_file() {
        const OLD: &[u8] = b"sodium 0.5 as the pack has it";
        let url = "https://cdn.modrinth.com/data/AANobbMI/versions/v05/sodium-0.5.jar";
        let index = serde_json::json!({
            "name": "P", "versionId": "1.0",
            "files": [{ "path": "mods/sodium-0.5.jar", "hashes": { "sha1": crate::modrinth::sha1(OLD) },
                        "downloads": [url], "fileSize": OLD.len() }],
            "dependencies": { "minecraft": "1.21.1" }
        });
        let project = num_id("AANobbMI");
        let pack = crate::instances::PackRef { project_id: num_id("packproj"), file_id: num_id("packver"), version: "1.0".into(), ..Default::default() };
        let jobs = crate::jobs::Jobs::new(crate::waker::noop());
        // Updated here (a pack mod still, or a fresh copy taken after the
        // member's removal), then removed here.
        for (tag, updated_from_pack) in [("pack", Some(true)), ("fresh", Some(false)), ("gone", None)] {
            let dir = std::env::temp_dir().join(format!("hopper-gs-mrback-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("mods")).unwrap();
            let mut inst = Instance { root: dir.clone(), pack: Some(pack.clone()), game_version: "1.21.1".into(), ..Default::default() };
            if let Some(from_pack) = updated_from_pack {
                std::fs::write(dir.join("mods/sodium-0.6.jar"), b"sodium 0.6").unwrap();
                inst.addons.push(Addon { project_id: project, file_id: num_id("v06"), file_name: "sodium-0.6.jar".into(), name: "Sodium".into(), enabled: true, from_pack, class_id: crate::cf::CLASS_MODS, mr_version: "v06".into(), ..Default::default() });
            }
            inst.save().unwrap();
            TEST_MR.with(|t| *t.borrow_mut() = Some((index.to_string(), HashMap::from([(url.to_string(), OLD.to_vec())]))));
            let mut todo = vec![Change { key: "n:sodium-0.5.jar".into(), was: State::Removed, now: State::Unlisted, entry: None }];
            if updated_from_pack.is_some() {
                todo.push(Change { key: format!("p:{project}"), was: State::File { file: "mr:v06".into(), on: true }, now: State::Unlisted, entry: None });
            }
            assert!(todo.iter().all(|c| does_something(&inst, c)), "{tag}");
            let job = jobs.start_quiet("test");
            apply(&mut inst, &todo, &[], &job).unwrap();
            job.finish(&Ok::<(), anyhow::Error>(()));
            TEST_MR.with(|t| *t.borrow_mut() = None);
            assert_eq!(std::fs::read(dir.join("mods/sodium-0.5.jar")).ok().as_deref(), Some(OLD), "{tag}: the pack's file isn't back");
            assert!(!dir.join("mods/sodium-0.6.jar").exists(), "{tag}: the update is still in");
            let sodium: Vec<&Addon> = inst.addons.iter().filter(|a| a.file_name.starts_with("sodium")).collect();
            assert_eq!(sodium.len(), 1, "{tag}: {sodium:?}");
            assert!(sodium[0].enabled && sodium[0].from_pack, "{tag}");
            assert_eq!(sodium[0].project_id, project, "{tag}: it's still the same mod");
            assert_eq!(sodium[0].mr_sha1, crate::modrinth::sha1(OLD), "{tag}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// Mods from GitHub (and other links) travelling through a group: a code
/// lists one as `Entry::Source` (the pinned release and sha256), keyed by
/// its link. These run the real install against a fake GitHub on a
/// loopback port, so what a PC downloads is counted.
#[cfg(test)]
mod source_sync_tests {
    use super::*;
    use crate::sources::carry::Pin;
    use crate::sources::{Kind as SrcKind, ModSource};
    use std::io::Write as _;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, MutexGuard};

    /// A source nobody ever confirmed on any PC that runs the tests.
    const REPO: &str = "hopper-test-never-trusted/ezmod";
    const KEY: &str = "s:hopper-test-never-trusted/ezmod#ezmod-#.jar";

    fn jar(release: &str) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file("release.txt", zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(release.as_bytes()).unwrap();
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    fn sha(b: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
    }

    fn pin_of(repo: &str, tag: &str, file: &str, body: &[u8]) -> Pin {
        Pin {
            file: file.into(),
            source: ModSource {
                kind: SrcKind::GitHub,
                url: repo.into(),
                publisher: repo.into(),
                pattern: crate::sources::pattern_of(file),
                seen: tag.into(),
                sha256: sha(body),
                ..Default::default()
            },
        }
    }

    fn pin(tag: &str, file: &str, body: &[u8]) -> Pin {
        pin_of(REPO, tag, file, body)
    }

    fn src(p: &Pin, on: bool) -> Entry {
        Entry::Source { pin: p.clone(), enabled: on }
    }

    fn share_of(entries: Vec<Entry>) -> Share {
        Share { base: Base::Plain { game_version: "1.21.1".into(), loader: "neoforge".into() }, name: None, loader: None, entries }
    }

    fn mods_of(entries: Vec<Entry>) -> Mods {
        mods(&share_of(entries), |_| None)
    }

    fn code(entries: Vec<Entry>) -> String {
        crate::sharecode::encode(&share_of(entries))
    }

    fn dec(c: &str) -> Option<Mods> {
        crate::sharecode::decode(c).ok().map(|s| mods(&s, |_| None))
    }

    fn got(c: String, seq: u64, name: &str) -> Got {
        Got { code: c, seq, at: seq, name: name.into(), ..Default::default() }
    }

    /// The changes from one code to the next.
    fn between(was: Vec<Entry>, now: Vec<Entry>) -> Vec<Change> {
        changes(&mods_of(was), &mods_of(now))
    }

    struct Fake {
        web: Arc<AtomicUsize>,
        api: Arc<AtomicUsize>,
        _g: MutexGuard<'static, ()>,
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            crate::sources::http::set_test_base(None);
            crate::sources::TEST_TRUSTED.lock().unwrap_or_else(|e| e.into_inner()).clear();
        }
    }

    type Answer = (u16, Vec<(&'static str, String)>, Vec<u8>);

    /// A fake GitHub: `answer` gets a website path (`/web/<repo>/...`) or an
    /// API path (`/api/...`) and says (status, headers, body).
    fn fake(answer: impl Fn(&str) -> Answer + Send + 'static) -> Fake {
        let g = crate::sources::http::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::sources::http::reset_for_test();
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let (web, api) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (w2, a2) = (web.clone(), api.clone());
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                let url = req.url().to_string();
                if url.starts_with("/api/") { &a2 } else { &w2 }.fetch_add(1, Ordering::SeqCst);
                let (status, headers, body) = answer(&url);
                let mut resp = tiny_http::Response::from_data(body).with_status_code(status);
                for (k, v) in headers {
                    resp.add_header(tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap());
                }
                let _ = req.respond(resp);
            }
        });
        crate::sources::http::set_test_base(Some(&base));
        Fake { web, api, _g: g }
    }

    /// A fake GitHub holding releases: (tag, file, body).
    fn releases(list: Vec<(&'static str, &'static str, Vec<u8>)>) -> Fake {
        fake(move |url| {
            for (tag, file, body) in &list {
                if url.ends_with(&format!("/releases/download/{tag}/{file}")) {
                    return (200, vec![], body.clone());
                }
            }
            (404, vec![], b"not found".to_vec())
        })
    }

    fn inst_on_disk(tag: &str) -> Instance {
        let root = std::env::temp_dir().join(format!("hopper-gs-src-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("mods")).unwrap();
        let inst = Instance { root, name: "T".into(), game_version: "1.21.1".into(), loader: "neoforge-21.1.200".into(), ..Default::default() };
        inst.save().unwrap();
        inst
    }

    fn run(inst: &mut Instance, todo: &[Change], skip: &[Pin]) -> (anyhow::Result<()>, Vec<String>) {
        let jobs = crate::jobs::Jobs::default();
        let job = jobs.start("t");
        let r = apply(inst, todo, skip, &job);
        (r, jobs.snapshot().into_iter().flat_map(|j| j.notes).collect())
    }

    fn mods_dir_files(inst: &Instance) -> Vec<String> {
        let mut v: Vec<String> =
            std::fs::read_dir(inst.root.join("mods")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    fn file_state(name: &str, on: bool) -> State {
        State::File { file: format!("src:{name}"), on }
    }

    // ---- 1-4: a member's change reaches the others, and ends as theirs ----

    /// A member adds a mod from GitHub, turns it off and on, updates it
    /// and removes it; another PC follows each step. The jar is downloaded
    /// twice only: when it is added and when it is updated.
    #[test]
    fn a_github_mod_added_switched_updated_and_removed_by_a_member_follows_on_another_pc() {
        let (j1, j2) = (jar("1.0"), jar("1.1"));
        let f = releases(vec![("v1", "ezmod-1.0.jar", j1.clone()), ("v2", "ezmod-1.1.jar", j2.clone())]);
        let (p1, p2) = (pin("v1", "ezmod-1.0.jar", &j1), pin("v2", "ezmod-1.1.jar", &j2));
        let mut inst = inst_on_disk("flow");

        // Added: one change, keyed by the link.
        let todo = between(vec![], vec![src(&p1, true)]);
        assert_eq!(todo.len(), 1);
        assert_eq!((todo[0].key.as_str(), todo[0].kind()), (KEY, Kind::Added));
        assert_eq!(todo[0].now, file_state("ezmod-1.0.jar", true));
        let (r, _) = run(&mut inst, &todo, &[]);
        r.unwrap();
        assert_eq!(f.web.load(Ordering::SeqCst), 1);
        assert_eq!(f.api.load(Ordering::SeqCst), 0, "the pin's own link, no API call");
        assert_eq!(mods_dir_files(&inst), ["ezmod-1.0.jar"]);
        let a = &inst.addons[0];
        assert!(a.enabled);
        assert_eq!(addon_key(a), KEY, "the same key the sender's code gave it");
        let s = a.source.as_ref().unwrap();
        assert_eq!((s.seen.as_str(), s.sha256.as_str()), ("v1", sha(&j1).as_str()));

        // Turned off, then on again: a switch, no download.
        let off = between(vec![src(&p1, true)], vec![src(&p1, false)]);
        assert_eq!(off[0].kind(), Kind::Off);
        run(&mut inst, &off, &[]).0.unwrap();
        assert_eq!(mods_dir_files(&inst), ["ezmod-1.0.jar.disabled"]);
        assert!(!inst.addons[0].enabled);
        let on = between(vec![src(&p1, false)], vec![src(&p1, true)]);
        assert_eq!(on[0].kind(), Kind::On);
        run(&mut inst, &on, &[]).0.unwrap();
        assert_eq!(mods_dir_files(&inst), ["ezmod-1.0.jar"]);
        assert!(inst.addons[0].enabled);
        assert_eq!(f.web.load(Ordering::SeqCst), 1, "toggling a GitHub mod never downloads it again");

        // Updated: the new release replaces the old, not beside it.
        let up = between(vec![src(&p1, true)], vec![src(&p2, true)]);
        assert_eq!(up.len(), 1);
        assert_eq!((up[0].key.as_str(), up[0].kind()), (KEY, Kind::Updated));
        run(&mut inst, &up, &[]).0.unwrap();
        assert_eq!(f.web.load(Ordering::SeqCst), 2);
        assert_eq!(mods_dir_files(&inst), ["ezmod-1.1.jar"], "the old jar isn't left in mods/");
        assert_eq!(inst.addons.len(), 1, "one mod, not two");
        let s = inst.addons[0].source.as_ref().unwrap();
        assert_eq!((s.seen.as_str(), s.sha256.as_str()), ("v2", sha(&j2).as_str()));
        assert_eq!(addon_key(&inst.addons[0]), KEY, "still the same mod for the group");

        // The same update again (an echo, or a second member's) changes nothing.
        run(&mut inst, &up, &[]).0.unwrap();
        assert_eq!(f.web.load(Ordering::SeqCst), 2);
        assert_eq!(inst.addons.len(), 1);

        // Removed.
        let gone = between(vec![src(&p2, true)], vec![]);
        assert_eq!((gone[0].key.as_str(), gone[0].kind()), (KEY, Kind::Removed));
        assert!(does_something(&inst, &gone[0]));
        run(&mut inst, &gone, &[]).0.unwrap();
        assert!(inst.addons.is_empty());
        assert!(mods_dir_files(&inst).is_empty());
        assert!(!does_something(&inst, &gone[0]), "a second removal does nothing");
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    #[test]
    fn a_github_mod_added_turned_off_arrives_off_and_a_download_that_isnt_the_pin_is_not_kept() {
        let j1 = jar("1.0");
        let f = releases(vec![("v1", "ezmod-1.0.jar", j1.clone())]);
        let mut inst = inst_on_disk("added-off");
        let p1 = pin("v1", "ezmod-1.0.jar", &j1);
        run(&mut inst, &between(vec![], vec![src(&p1, false)]), &[]).0.unwrap();
        assert_eq!(mods_dir_files(&inst), ["ezmod-1.0.jar.disabled"]);
        assert!(!inst.addons[0].enabled);
        assert_eq!(f.web.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&inst.root);

        // The release's file isn't what the pin says (sha256): nothing kept.
        let mut inst = inst_on_disk("added-wrong");
        let bad = Pin { source: ModSource { sha256: "0".repeat(64), ..p1.source.clone() }, ..p1.clone() };
        let (r, _) = run(&mut inst, &between(vec![], vec![src(&bad, true)]), &[]);
        let e = r.unwrap_err();
        assert!(format!("{e:#}").contains("checksum"), "{e:#}");
        assert!(mods_dir_files(&inst).is_empty(), "{:?}", mods_dir_files(&inst));
        assert!(inst.addons.is_empty());
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    /// A plain follow applies the sender's whole code on every change, with
    /// all its GitHub mods in it: the ones already here as the pinned file
    /// are not downloaded (and their old copy not sent to backups) again.
    #[test]
    fn a_followed_code_applied_again_downloads_no_github_mod_that_is_already_here() {
        let (j1, j2) = (jar("1.0"), jar("1.1"));
        let f = releases(vec![("v1", "ezmod-1.0.jar", j1.clone()), ("v2", "ezmod-1.1.jar", j2.clone())]);
        let (p1, p2) = (pin("v1", "ezmod-1.0.jar", &j1), pin("v2", "ezmod-1.1.jar", &j2));
        let mut inst = inst_on_disk("follow-again");
        let jobs = crate::jobs::Jobs::default();
        let job = jobs.start("t");
        let apply_code = |inst: &mut Instance, entries: Vec<Entry>, prev: Option<Vec<Entry>>| {
            let prev = prev.map(share_of);
            crate::sharecode::apply_existing(inst, &share_of(entries), prev.as_ref(), &job).unwrap();
        };
        apply_code(&mut inst, vec![src(&p1, true)], None);
        assert_eq!((f.web.load(Ordering::SeqCst), mods_dir_files(&inst)), (1, vec!["ezmod-1.0.jar".to_string()]));
        // The same code again, and again with another change in it.
        apply_code(&mut inst, vec![src(&p1, true)], Some(vec![src(&p1, true)]));
        assert_eq!(f.web.load(Ordering::SeqCst), 1, "nothing downloaded for a mod that is here");
        // Turned off by the sender: off here, still no download.
        apply_code(&mut inst, vec![src(&p1, false)], Some(vec![src(&p1, true)]));
        assert_eq!(mods_dir_files(&inst), ["ezmod-1.0.jar.disabled"]);
        assert_eq!(f.web.load(Ordering::SeqCst), 1);
        // Back on.
        apply_code(&mut inst, vec![src(&p1, true)], Some(vec![src(&p1, false)]));
        assert_eq!(mods_dir_files(&inst), ["ezmod-1.0.jar"]);
        assert_eq!(f.web.load(Ordering::SeqCst), 1);
        // A new release is a download, and the only jar after.
        apply_code(&mut inst, vec![src(&p2, true)], Some(vec![src(&p1, true)]));
        assert_eq!((f.web.load(Ordering::SeqCst), mods_dir_files(&inst)), (2, vec!["ezmod-1.1.jar".to_string()]));
        assert_eq!(inst.addons.len(), 1);
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    /// A pack mod swapped for a GitHub one reads as the pack's mod taken
    /// out (by its file name for a Modrinth pack) plus the new source mod.
    #[test]
    fn a_pack_mod_swapped_for_a_github_one_and_a_removal_by_file_name_find_the_right_mod() {
        let j1 = jar("1.0");
        let f = releases(vec![("v1", "ezmod-1.0.jar", j1.clone())]);
        let p1 = pin("v1", "ezmod-1.0.jar", &j1);
        let mut inst = inst_on_disk("swap");
        std::fs::write(inst.root.join("mods/oldmod-1.0.jar"), b"old").unwrap();
        inst.addons.push(Addon {
            project_id: 77,
            name: "Old".into(),
            file_name: "oldmod-1.0.jar".into(),
            enabled: true,
            from_pack: true,
            ..Default::default()
        });
        inst.save().unwrap();
        let todo = between(vec![], vec![Entry::Remove(Target::Named("oldmod-1.0.jar".into())), src(&p1, true)]);
        assert_eq!(
            todo.iter().map(|c| (c.key.as_str(), c.kind())).collect::<Vec<_>>(),
            [("n:oldmod-1.0.jar", Kind::Removed), (KEY, Kind::Added)]
        );
        run(&mut inst, &todo, &[]).0.unwrap();
        assert_eq!(mods_dir_files(&inst), ["ezmod-1.0.jar"]);
        assert_eq!(inst.addons.len(), 1);
        assert_eq!(f.web.load(Ordering::SeqCst), 1);

        // A removal that names the source mod's jar finds it too.
        assert_eq!(find_addon(&inst, "n:ezmod-1.0.jar"), Some(0));
        assert_eq!(find_addon(&inst, KEY), Some(0));
        let by_name = Change {
            key: "n:ezmod-1.0.jar".into(),
            was: State::Unlisted,
            now: State::Removed,
            entry: Some(Entry::Remove(Target::Named("ezmod-1.0.jar".into()))),
        };
        run(&mut inst, &[by_name], &[]).0.unwrap();
        assert!(inst.addons.is_empty());
        assert!(mods_dir_files(&inst).is_empty());
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    // ---- 5: two-way, the latest change wins and says who ----

    #[test]
    fn two_members_changing_the_same_github_mod_end_on_the_later_one_and_say_who() {
        let (j1, j2) = (jar("1.0"), jar("1.1"));
        let (p1, p2) = (pin("v1", "ezmod-1.0.jar", &j1), pin("v2", "ezmod-1.1.jar", &j2));
        let base = vec![src(&p1, true)];
        let mia = code(vec![src(&p1, false)]); // seq 10: turned off
        let sam = code(vec![src(&p2, true)]); // seq 11: updated
        let start = |copy: &mut PackCopy| {
            copy.got.insert("mia".into(), got(code(base.clone()), 1, "Mia"));
            copy.got.insert("sam".into(), got(code(base.clone()), 2, "Sam"));
        };
        // A third PC takes both: Sam's is later.
        let mut third = PackCopy::default();
        start(&mut third);
        third.arrive("mia", got(mia.clone(), 10, "Mia"));
        third.arrive("sam", got(sam.clone(), 11, "Sam"));
        let here = mods_of(base.clone());
        let (todo, merged) = plan(&third, &here, None, false, dec);
        let last = todo.last().unwrap();
        assert_eq!((last.key.as_str(), last.now.clone()), (KEY, file_state("ezmod-1.1.jar", true)));
        let mark = merged.marks.iter().rev().find(|(k, _)| k == KEY).map(|(_, m)| m.clone()).unwrap();
        assert_eq!((mark.kind, mark.name.as_str(), mark.seq), (Kind::Updated, "Sam", 11));
        assert_eq!(mark.kind.says(&mark.name), "Updated by Sam");
        // Mia's PC (off, her mark at 10) takes Sam's later update.
        let mut on_mia = PackCopy::default();
        start(&mut on_mia);
        on_mia.got.remove("mia");
        on_mia.marks.insert(KEY.into(), Mark { state: file_state("ezmod-1.0.jar", false), seq: 10, by: "mia".into(), ..Default::default() });
        on_mia.arrive("sam", got(sam.clone(), 11, "Sam"));
        let here = mods_of(vec![src(&p1, false)]);
        let (todo, _) = plan(&on_mia, &here, Some(&here), false, dec);
        assert_eq!(todo.iter().map(|c| c.now.clone()).collect::<Vec<_>>(), [file_state("ezmod-1.1.jar", true)]);
        // Sam's PC (his mark at 11) drops Mia's earlier switch.
        let mut on_sam = PackCopy::default();
        start(&mut on_sam);
        on_sam.got.remove("sam");
        on_sam.marks.insert(KEY.into(), Mark { state: file_state("ezmod-1.1.jar", true), seq: 11, by: "sam".into(), ..Default::default() });
        on_sam.arrive("mia", got(mia, 10, "Mia"));
        let here = mods_of(vec![src(&p2, true)]);
        let (todo, merged) = plan(&on_sam, &here, Some(&here), false, dec);
        assert!(todo.is_empty());
        assert_eq!(merged.taken.len(), 1);
    }

    // ---- 6: a source this PC hasn't confirmed ----

    fn merge_one(inst: &Instance, was: Vec<Entry>, now: Vec<Entry>, allow: bool) -> (anyhow::Result<Merged>, Vec<String>) {
        let mut copy = PackCopy::default();
        copy.got.insert("mia".into(), got(code(was.clone()), 1, "Mia"));
        copy.sent = code(was);
        copy.arrive("mia", got(code(now), 10, "Mia"));
        let jobs = crate::jobs::Jobs::default();
        let job = jobs.start("t");
        let r = merge(&inst.root, &copy, allow, false, true, &job);
        (r, jobs.snapshot().into_iter().flat_map(|j| j.notes).collect())
    }

    #[test]
    fn a_github_mod_from_an_unconfirmed_source_is_not_downloaded_and_the_player_is_told() {
        let j1 = jar("1.0");
        let f = releases(vec![("v1", "ezmod-1.0.jar", j1.clone())]);
        let p1 = pin("v1", "ezmod-1.0.jar", &j1);
        let inst = inst_on_disk("untrusted");
        // Mod sources off, and on but never confirmed: the same answer.
        for allow in [false, true] {
            let (r, _) = merge_one(&inst, vec![], vec![src(&p1, true)], allow);
            let merged = r.unwrap();
            assert_eq!(f.web.load(Ordering::SeqCst), 0, "nothing downloaded (allow {allow})");
            assert!(mods_dir_files(&inst).is_empty());
            assert_eq!(merged.taken.len(), 1);
            assert!(merged.marks.is_empty() && merged.made.is_empty(), "no 'Added by Mia' on a mod that isn't here: {merged:?}");
            assert_eq!(merged.kept.len(), 1, "{merged:?}");
            let k = &merged.kept[0];
            assert_eq!((k.key.as_str(), k.why, k.kind, k.by.as_str()), (KEY, KeptWhy::Source, Kind::Added, "Mia"));
            assert_eq!(
                k.says(),
                format!("ezmod (GitHub: {REPO}) wasn't installed: you haven't confirmed its source yet (Settings › Mod sources). Mia added it.")
            );
        }
        // An update of one already here is left as it is, said as an update.
        let _ = std::fs::remove_dir_all(&inst.root);
        let mut here = inst_on_disk("untrusted-update");
        run(&mut here, &between(vec![], vec![src(&p1, true)]), &[]).0.unwrap();
        let j2 = jar("1.1");
        let p2 = pin("v2", "ezmod-1.1.jar", &j2);
        let before = f.web.load(Ordering::SeqCst);
        let merged = merge_one(&here, vec![src(&p1, true)], vec![src(&p2, true)], false).0.unwrap();
        assert_eq!(f.web.load(Ordering::SeqCst), before);
        assert_eq!(mods_dir_files(&here), ["ezmod-1.0.jar"]);
        let says = merged.kept[0].says();
        assert!(says.starts_with("ezmod (GitHub: ") && says.contains("wasn't updated") && says.ends_with("Mia changed its version."), "{says}");
        let _ = std::fs::remove_dir_all(&here.root);
    }

    #[test]
    fn switching_a_github_mod_already_here_needs_no_confirmed_source() {
        let j1 = jar("1.0");
        let f = releases(vec![("v1", "ezmod-1.0.jar", j1.clone())]);
        let p1 = pin("v1", "ezmod-1.0.jar", &j1);
        let mut inst = inst_on_disk("untrusted-switch");
        run(&mut inst, &between(vec![], vec![src(&p1, true)]), &[]).0.unwrap();
        let merged = merge_one(&inst, vec![src(&p1, true)], vec![src(&p1, false)], false).0.unwrap();
        assert!(merged.kept.is_empty(), "{merged:?}");
        assert_eq!(merged.made.len(), 1);
        assert_eq!(mods_dir_files(&inst), ["ezmod-1.0.jar.disabled"]);
        assert_eq!(f.web.load(Ordering::SeqCst), 1, "no second download");
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    #[test]
    fn apply_given_a_left_out_source_says_so_and_installs_nothing() {
        let j1 = jar("1.0");
        let f = releases(vec![("v1", "ezmod-1.0.jar", j1.clone())]);
        let p1 = pin("v1", "ezmod-1.0.jar", &j1);
        let mut inst = inst_on_disk("skip-note");
        let (r, notes) = run(&mut inst, &between(vec![], vec![src(&p1, true)]), std::slice::from_ref(&p1));
        r.unwrap();
        assert!(inst.addons.is_empty() && mods_dir_files(&inst).is_empty());
        assert_eq!(f.web.load(Ordering::SeqCst), 0);
        assert_eq!(notes, ["ezmod: not installed, you haven't confirmed its source on this PC"]);
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    /// Several GitHub mods arrive while GitHub is limiting: one error for
    /// the whole sync (so one notice), and GitHub is asked once.
    #[test]
    fn a_github_rate_limit_is_one_error_for_all_the_mods_and_asks_github_once() {
        let reset = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 600;
        let f = fake(move |url| {
            if url.starts_with("/api/") {
                (403, vec![("x-ratelimit-remaining", "0".to_string()), ("x-ratelimit-reset", reset.to_string())], b"{}".to_vec())
            } else {
                (404, vec![], b"not found".to_vec())
            }
        });
        let body = jar("1.0");
        let pins: Vec<Pin> = (0..3).map(|i| pin_of(&format!("hopper-test-never-trusted/m{i}"), "v1", &format!("m{i}-1.0.jar"), &body)).collect();
        let todo: Vec<Change> = pins.iter().flat_map(|p| between(vec![], vec![src(p, true)])).collect();
        assert_eq!(todo.len(), 3);
        let mut inst = inst_on_disk("limit");
        let (r, _) = run(&mut inst, &todo, &[]);
        let e = r.unwrap_err();
        assert!(crate::sources::http::rate_limited(&e).is_some(), "{e:#}");
        assert_eq!(f.api.load(Ordering::SeqCst), 1, "after the first answer the others wait without asking");
        assert!(inst.addons.is_empty());
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    // ---- 7: locked and kept-only-here ----

    #[test]
    fn a_locked_github_mod_still_switches_but_keeps_its_file() {
        let (j1, j2) = (jar("1.0"), jar("1.1"));
        let f = releases(vec![("v1", "ezmod-1.0.jar", j1.clone()), ("v2", "ezmod-1.1.jar", j2.clone())]);
        let (p1, p2) = (pin("v1", "ezmod-1.0.jar", &j1), pin("v2", "ezmod-1.1.jar", &j2));
        let mut inst = inst_on_disk("locked-src");
        run(&mut inst, &between(vec![], vec![src(&p1, true)]), &[]).0.unwrap();
        inst.addons[0].locked = true;
        inst.save().unwrap();
        let off = between(vec![src(&p1, true)], vec![src(&p1, false)]).remove(0);
        let up = between(vec![src(&p1, true)], vec![src(&p2, true)]).remove(0);
        let gone = between(vec![src(&p1, true)], vec![]).remove(0);
        assert!(!locked_out(&inst, &off), "a switch of the same file goes through");
        assert!(locked_out(&inst, &up), "another release is left out");
        assert!(locked_out(&inst, &gone), "so is a removal");
        let (r, notes) = run(&mut inst, &[up, gone], &[]);
        r.unwrap();
        assert_eq!(notes, ["ezmod: kept, you locked its version", "ezmod: kept, you locked its version"]);
        assert_eq!(mods_dir_files(&inst), ["ezmod-1.0.jar"]);
        run(&mut inst, &[off], &[]).0.unwrap();
        assert_eq!(mods_dir_files(&inst), ["ezmod-1.0.jar.disabled"]);
        assert_eq!(f.web.load(Ordering::SeqCst), 1);
        // The merge says why, and leaves no 'Updated by Mia' mark.
        let merged = merge_one(&inst, vec![src(&p1, false)], vec![src(&p2, true)], true).0.unwrap();
        assert_eq!(merged.kept.len(), 1, "{merged:?}");
        assert_eq!(merged.kept[0].why, KeptWhy::Locked);
        assert!(merged.marks.is_empty());
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    #[test]
    fn a_github_mod_kept_only_here_is_left_alone_by_the_group_and_only_learns_how_friends_have_it() {
        let (j1, j2) = (jar("1.0"), jar("1.1"));
        let _f = releases(vec![("v1", "ezmod-1.0.jar", j1.clone()), ("v2", "ezmod-1.1.jar", j2.clone())]);
        let (p1, p2) = (pin("v1", "ezmod-1.0.jar", &j1), pin("v2", "ezmod-1.1.jar", &j2));
        let mut inst = inst_on_disk("only-here");
        run(&mut inst, &between(vec![], vec![src(&p1, true)]), &[]).0.unwrap();
        inst.only_here.insert(KEY.into(), crate::onlyhere::Kept { theirs: Some(src(&p1, true)), name: "ezmod".into() });
        inst.save().unwrap();
        // A member updates it, then removes it.
        let merged = merge_one(&inst, vec![src(&p1, true)], vec![src(&p2, true)], true).0.unwrap();
        assert_eq!(mods_dir_files(&inst), ["ezmod-1.0.jar"], "this PC keeps its own");
        assert!(merged.kept.is_empty() && merged.made.is_empty(), "{merged:?}");
        let back = crate::instances::load(&inst.root).unwrap();
        assert_eq!(back.only_here[KEY].theirs, Some(src(&p2, true)), "friends have 2 now");
        let merged = merge_one(&back, vec![src(&p2, true)], vec![], true).0.unwrap();
        assert_eq!(mods_dir_files(&inst), ["ezmod-1.0.jar"]);
        assert!(merged.made.is_empty());
        assert_eq!(crate::instances::load(&inst.root).unwrap().only_here[KEY].theirs, None);
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    // ---- 8: changes left out for a source stay owed, and come back once it is confirmed ----

    /// A copy that took Mia's code: she added the mod at 10. `merge` runs
    /// against it like the store does.
    fn copy_with_mia(was: Vec<Entry>, now: Vec<Entry>) -> PackCopy {
        let mut copy = PackCopy::default();
        copy.got.insert("mia".into(), got(code(was.clone()), 1, "Mia"));
        copy.sent = code(was);
        copy.arrive("mia", got(code(now), 10, "Mia"));
        copy
    }

    fn merge_copy(inst: &Instance, copy: &PackCopy, allow: bool) -> Merged {
        let jobs = crate::jobs::Jobs::default();
        let job = jobs.start("t");
        merge(&inst.root, copy, allow, false, true, &job).unwrap()
    }

    /// What the store does with a merge's result (`on_group_merged`).
    fn take(copy: &mut PackCopy, m: &Merged) {
        for (from, g) in &m.taken {
            copy.waiting.remove(from);
            copy.got.insert(from.clone(), g.clone());
        }
        for (k, mk) in &m.marks {
            copy.marks.insert(k.clone(), mk.clone());
        }
        if let Some(o) = &m.owed {
            copy.owed = o.clone();
        }
    }

    fn trust_repo() {
        crate::sources::TEST_TRUSTED.lock().unwrap().push(format!("github:{REPO}"));
    }

    #[test]
    fn the_test_trust_key_is_the_sources_own() {
        let p = pin("v1", "ezmod-1.0.jar", &jar("1.0"));
        assert_eq!(p.source.key(), format!("github:{REPO}"), "the key the tests confirm");
    }

    #[test]
    fn a_mod_left_out_for_its_source_is_owed_and_the_list_survives_settings() {
        let j1 = jar("1.0");
        let f = releases(vec![("v1", "ezmod-1.0.jar", j1.clone())]);
        let p1 = pin("v1", "ezmod-1.0.jar", &j1);
        let inst = inst_on_disk("owed-left");
        let mut copy = copy_with_mia(vec![], vec![src(&p1, true)]);
        let merged = merge_copy(&inst, &copy, true);
        assert_eq!(f.web.load(Ordering::SeqCst), 0);
        let owed = merged.owed.clone().expect("what is owed is said");
        assert_eq!(owed.len(), 1);
        let o = &owed[0];
        assert_eq!((o.key.as_str(), o.by.as_str(), o.name.as_str(), o.seq), (KEY, "mia", "Mia", 10));
        assert_eq!(o.now, file_state("ezmod-1.0.jar", true));
        take(&mut copy, &merged);
        assert_eq!(copy.owed_says().as_deref(), Some("1 mod waiting for you to confirm its source (Settings › Mod sources)."));
        assert_eq!(copy.owed_sources().len(), 1);
        assert_eq!(copy.owed_sources()[0].0, "ezmod");
        // Saved with the settings and read back.
        let json = serde_json::to_string(&copy).unwrap();
        let back: PackCopy = serde_json::from_str(&json).unwrap();
        assert_eq!(back.owed, copy.owed);
        // Settings from before it had the list still load, with nothing owed.
        let old: PackCopy = serde_json::from_str(r#"{"inst":"a","group":"g","local":"l","sent":"","sent_seq":3}"#).unwrap();
        assert!(old.owed.is_empty() && old.owed_says().is_none());
        let st: crate::settings::Settings = serde_json::from_str(r#"{"group_copies":[{"inst":"a","group":"g"}]}"#).unwrap();
        assert!(st.group_copies[0].owed.is_empty());
        // Nothing owed writes nothing, so the file doesn't change for others.
        assert!(!serde_json::to_string(&PackCopy::default()).unwrap().contains("owed"));
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    #[test]
    fn confirming_the_source_installs_the_mod_once() {
        let j1 = jar("1.0");
        let f = releases(vec![("v1", "ezmod-1.0.jar", j1.clone())]);
        let p1 = pin("v1", "ezmod-1.0.jar", &j1);
        let inst = inst_on_disk("owed-confirm");
        let mut copy = copy_with_mia(vec![], vec![src(&p1, true)]);
        let first = merge_copy(&inst, &copy, true);
        take(&mut copy, &first);
        assert!(copy.waiting.is_empty() && !copy.owed.is_empty());
        // Not confirmed: nothing to do on a tick, and a merge changes nothing.
        assert!(!copy.owed_ready(true));
        let again = merge_copy(&inst, &copy, true);
        assert_eq!(again.owed.as_ref().map(Vec::len), Some(1), "still owed");
        assert!(mods_dir_files(&inst).is_empty() && f.web.load(Ordering::SeqCst) == 0);
        // The player confirms it.
        trust_repo();
        assert!(copy.owed_ready(true));
        assert!(!copy.owed_ready(false), "Settings › Mod sources off: still waiting");
        let done = merge_copy(&inst, &copy, true);
        assert_eq!(mods_dir_files(&inst), ["ezmod-1.0.jar"]);
        assert_eq!(f.web.load(Ordering::SeqCst), 1);
        assert_eq!(done.owed, Some(vec![]), "nothing is owed any more");
        assert!(done.kept.is_empty(), "{done:?}");
        let mark = done.marks.iter().find(|(k, _)| k == KEY).map(|(_, m)| m.clone()).expect("marked as Mia's change");
        assert_eq!((mark.by.as_str(), mark.name.as_str(), mark.seq, mark.kind), ("mia", "Mia", 10, Kind::Added));
        take(&mut copy, &done);
        assert!(copy.owed.is_empty() && !copy.owed_ready(true));
        // A tick that still had the old list (the store hadn't saved yet)
        // finds the mod there and downloads nothing again.
        let mut stale = copy.clone();
        stale.owed = first.owed.clone().unwrap();
        let third = merge_copy(&inst, &stale, true);
        assert_eq!(f.web.load(Ordering::SeqCst), 1, "installed once");
        assert_eq!(third.owed, Some(vec![]));
        assert_eq!(mods_dir_files(&inst), ["ezmod-1.0.jar"]);
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    #[test]
    fn saying_yes_to_an_owed_mod_installs_it_whatever_the_sources_say() {
        let j1 = jar("1.0");
        let f = releases(vec![("v1", "ezmod-1.0.jar", j1.clone())]);
        let p1 = pin("v1", "ezmod-1.0.jar", &j1);
        let inst = inst_on_disk("owed-approve");
        let mut copy = copy_with_mia(vec![], vec![src(&p1, true)]);
        // Settings › Mod sources is off: the mod is left out and owed.
        let first = merge_copy(&inst, &copy, false);
        take(&mut copy, &first);
        assert_eq!(copy.owed.len(), 1);
        assert!(!copy.owed_ready(false));
        let to_allow = copy.owed_to_allow();
        assert_eq!(to_allow.len(), 1);
        assert_eq!((to_allow[0].0.as_str(), to_allow[0].1.as_str()), ("ezmod", format!("GitHub: {REPO}").as_str()));
        // Still off, nothing changes on another merge.
        let again = merge_copy(&inst, &copy, false);
        assert!(mods_dir_files(&inst).is_empty() && f.web.load(Ordering::SeqCst) == 0, "{again:?}");
        // The player says yes to it: it goes in on the next merge, though
        // the sources are still off, and the list empties.
        assert!(copy.approve_owed(&[]));
        assert!(!copy.approve_owed(&[]), "said once");
        assert!(copy.owed_ready(false));
        assert!(copy.owed_to_allow().is_empty());
        // The yes survives the settings file; settings from before load as no.
        let back: PackCopy = serde_json::from_str(&serde_json::to_string(&copy).unwrap()).unwrap();
        assert!(back.owed[0].approved);
        let old: Owed = serde_json::from_str(r#"{"key":"k"}"#).unwrap();
        assert!(!old.approved);
        let done = merge_copy(&inst, &copy, false);
        assert_eq!(mods_dir_files(&inst), ["ezmod-1.0.jar"]);
        assert_eq!(f.web.load(Ordering::SeqCst), 1);
        assert_eq!(done.owed, Some(vec![]));
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    #[test]
    fn declining_keeps_it_owed_and_downloads_nothing() {
        let j1 = jar("1.0");
        let f = releases(vec![("v1", "ezmod-1.0.jar", j1.clone())]);
        let p1 = pin("v1", "ezmod-1.0.jar", &j1);
        let inst = inst_on_disk("owed-decline");
        let mut copy = copy_with_mia(vec![], vec![src(&p1, true)]);
        for _ in 0..3 {
            let m = merge_copy(&inst, &copy, true);
            take(&mut copy, &m);
            assert_eq!(copy.owed.len(), 1, "owed once, however many merges");
            assert!(!copy.owed_ready(true));
        }
        assert_eq!(f.web.load(Ordering::SeqCst), 0);
        assert!(mods_dir_files(&inst).is_empty());
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    #[test]
    fn the_same_mod_is_owed_once_and_a_newer_change_replaces_the_old_one() {
        let (j1, j2) = (jar("1.0"), jar("1.1"));
        let _f = releases(vec![("v1", "ezmod-1.0.jar", j1.clone()), ("v2", "ezmod-1.1.jar", j2.clone())]);
        let (p1, p2) = (pin("v1", "ezmod-1.0.jar", &j1), pin("v2", "ezmod-1.1.jar", &j2));
        let inst = inst_on_disk("owed-dup");
        let mut copy = copy_with_mia(vec![], vec![src(&p1, true)]);
        let m = merge_copy(&inst, &copy, true);
        take(&mut copy, &m);
        // A list that somehow holds it twice comes out with one.
        copy.owed.push(copy.owed[0].clone());
        // Mia changes it again, to 1.1, while it is still owed.
        copy.arrive("mia", got(code(vec![src(&p2, true)]), 20, "Mia"));
        let m = merge_copy(&inst, &copy, true);
        let owed = m.owed.clone().unwrap();
        assert_eq!(owed.len(), 1, "{owed:?}");
        assert_eq!((owed[0].seq, owed[0].now.clone()), (20, file_state("ezmod-1.1.jar", true)));
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    #[test]
    fn a_later_change_of_the_mod_drops_what_was_owed() {
        let j1 = jar("1.0");
        let _f = releases(vec![("v1", "ezmod-1.0.jar", j1.clone())]);
        let p1 = pin("v1", "ezmod-1.0.jar", &j1);
        let inst = inst_on_disk("owed-later");
        let mut copy = copy_with_mia(vec![], vec![src(&p1, true)]);
        let m = merge_copy(&inst, &copy, true);
        take(&mut copy, &m);
        assert_eq!(copy.owed.len(), 1);
        // Sam removed it afterwards (a later change this PC knows).
        let mut after_sam = copy.clone();
        after_sam.marks.insert(KEY.into(), Mark { state: State::Unlisted, kind: Kind::Removed, by: "sam".into(), name: "Sam".into(), seq: 15, at: 15 });
        trust_repo();
        let m = merge_copy(&inst, &after_sam, true);
        assert!(mods_dir_files(&inst).is_empty(), "Sam's later removal wins over Mia's earlier add");
        assert_eq!(m.owed, Some(vec![]));
        // Or this PC changed the mod itself and hasn't sent it yet.
        let mut own = copy.clone();
        own.sent = code(vec![src(&p1, false)]);
        let m = merge_copy(&inst, &own, true);
        assert!(mods_dir_files(&inst).is_empty());
        assert_eq!(m.owed, Some(vec![]));
        // A waiting code that applies a change of it also replaces it.
        let mut newer = copy.clone();
        newer.arrive("sam", got(code(vec![]), 16, "Sam"));
        newer.got.insert("sam".into(), got(code(vec![src(&p1, true)]), 2, "Sam"));
        let m = merge_copy(&inst, &newer, true);
        assert!(mods_dir_files(&inst).is_empty());
        assert_eq!(m.owed, Some(vec![]));
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    #[test]
    fn a_relay_reset_renumbers_what_is_owed() {
        let j1 = jar("1.0");
        let _f = releases(vec![("v1", "ezmod-1.0.jar", j1.clone())]);
        let p1 = pin("v1", "ezmod-1.0.jar", &j1);
        let inst = inst_on_disk("owed-reset");
        let mut copy = copy_with_mia(vec![], vec![src(&p1, true)]);
        let m = merge_copy(&inst, &copy, true);
        take(&mut copy, &m);
        assert_eq!(copy.owed[0].seq, 10);
        copy.relay_reset();
        assert_eq!(copy.owed[0].seq, 0);
        let _ = std::fs::remove_dir_all(&inst.root);
    }
}

#[cfg(test)]
mod lock_sync_tests {
    use super::*;
    use crate::instances::Instance;

    fn share_of(entries: Vec<Entry>) -> Share {
        Share { base: Base::Plain { game_version: "1.21.1".into(), loader: "neoforge".into() }, name: None, loader: None, entries }
    }

    fn code(entries: Vec<Entry>) -> String {
        crate::sharecode::encode(&share_of(entries))
    }

    /// Mods as the tests have them: CurseForge file `n` is project `n / 10`.
    fn read(c: &str) -> Option<Mods> {
        crate::sharecode::decode(c).ok().map(|s| {
            mods(&s, |e| match e {
                Entry::CurseFile { file, .. } => Some(file / 10),
                _ => None,
            })
        })
    }

    fn file(f: u64) -> Entry {
        Entry::CurseFile { file: f, enabled: true }
    }

    fn lock(key: &str, version: &str, locked: bool) -> Entry {
        Entry::Lock { key: key.into(), version: version.into(), locked }
    }

    fn got(c: String, seq: u64, name: &str) -> Got {
        Got { code: c, seq, at: seq, name: name.into(), ..Default::default() }
    }

    const LK: &str = "~lock:p:5";

    /// This PC's copy: it has Mia's and Sam's codes `before`, and what each
    /// sent now arrives.
    fn plan_for(before: &[(&str, Vec<Entry>)], now: &[(&str, u64, Vec<Entry>)], here: Vec<Entry>) -> (Vec<Change>, Merged) {
        let mut copy = PackCopy::default();
        let name = |who: &str| who[..1].to_uppercase() + &who[1..];
        for (who, entries) in before {
            copy.got.insert((*who).into(), got(code(entries.clone()), 1, &name(who)));
        }
        for (who, seq, entries) in now {
            copy.arrive(who, got(code(entries.clone()), *seq, &name(who)));
        }
        let here = read(&code(here)).unwrap();
        plan(&copy, &here, Some(&here), false, read)
    }

    #[test]
    fn a_lock_reaches_the_other_members_and_says_who() {
        let (todo, merged) = plan_for(&[("mia", vec![file(50)])], &[("mia", 10, vec![file(50), lock("p:5", "", true)])], vec![file(50)]);
        assert_eq!(todo.len(), 1, "{todo:?}");
        let c = &todo[0];
        assert_eq!((c.key.as_str(), c.kind()), (LK, Kind::Locked));
        assert_eq!(lock_of(&c.now), (true, String::new()));
        let (k, mark) = &merged.marks[0];
        assert_eq!((k.as_str(), mark.name.as_str(), mark.by.as_str()), (LK, "Mia", "mia"));
        assert_eq!(mark.kind.says(&mark.name), "Locked by Mia");
        // The mod's own file isn't a change.
        assert!(!todo.iter().any(|c| c.key == "p:5"));
    }

    #[test]
    fn an_unlock_reaches_the_other_members_too() {
        let locked = vec![file(50), lock("p:5", "", true)];
        let (todo, merged) = plan_for(&[("mia", locked.clone())], &[("mia", 10, vec![file(50)])], locked);
        assert_eq!(todo.len(), 1, "{todo:?}");
        assert_eq!((todo[0].key.as_str(), todo[0].kind(), todo[0].now.clone()), (LK, Kind::Unlocked, State::Unlisted));
        assert_eq!(lock_of(&todo[0].now), (false, String::new()));
        assert_eq!(merged.marks[0].1.kind.says("Mia"), "Unlocked by Mia");
    }

    #[test]
    fn the_later_of_two_lock_changes_wins_and_names_who() {
        let both = vec![file(50), lock("p:5", "", true)];
        let none = vec![file(50)];
        // Mia unlocks at 10, Sam locks again with a version at 12.
        let sam = vec![file(50), lock("p:5", "m5-2.jar", true)];
        let (todo, merged) = plan_for(&[("mia", both.clone()), ("sam", none.clone())], &[("mia", 10, none.clone()), ("sam", 12, sam)], both.clone());
        let c: Vec<_> = todo.iter().filter(|c| c.key == LK).collect();
        assert_eq!(c.len(), 1, "one change per mod");
        assert_eq!(lock_of(&c[0].now), (true, "m5-2.jar".into()), "Sam's, the later one");
        let m = merged.marks.iter().rev().find(|(k, _)| k == LK).unwrap().1.clone();
        assert_eq!((m.by.as_str(), m.name.as_str(), m.seq), ("sam", "Sam", 12));
        // The other way round: Sam's is the earlier, and Mia's unlock wins.
        let sam = vec![file(50), lock("p:5", "m5-2.jar", true)];
        let (todo, merged) = plan_for(&[("mia", both.clone()), ("sam", none.clone())], &[("mia", 12, none.clone()), ("sam", 10, sam)], both);
        let c: Vec<_> = todo.iter().filter(|c| c.key == LK).collect();
        assert_eq!((c.len(), c[0].now.clone()), (1, State::Unlisted));
        let m = merged.marks.iter().rev().find(|(k, _)| k == LK).unwrap().1.clone();
        assert_eq!((m.by.as_str(), m.seq), ("mia", 12));
        // A change older than the one this PC knows is dropped.
        let mut copy = PackCopy::default();
        copy.got.insert("mia".into(), got(code(vec![file(50)]), 1, "Mia"));
        copy.marks.insert(LK.into(), Mark { state: State::File { file: "lock:".into(), on: true }, kind: Kind::Locked, by: "sam".into(), name: "Sam".into(), seq: 20, at: 20 });
        copy.arrive("mia", got(code(vec![file(50), lock("p:5", "", false), lock("p:6", "x", false)]), 10, "Mia"));
        let here = read(&code(vec![file(50), lock("p:5", "", true)])).unwrap();
        let (todo, _) = plan(&copy, &here, Some(&here), false, read);
        assert!(!todo.iter().any(|c| c.key == LK), "Sam's lock at 20 is later than Mia's change at 10: {todo:?}");
    }

    #[test]
    fn a_code_without_lock_info_changes_no_lock() {
        // This PC has p:5 locked. Mia's Hopper doesn't know locks: her codes
        // list no lock, before and after her change of another mod.
        let here = vec![file(50), file(60), lock("p:5", "", true)];
        let (todo, _) = plan_for(&[("mia", vec![file(50)])], &[("mia", 10, vec![file(50), file(60)])], here.clone());
        assert!(todo.iter().all(|c| lock_target(&c.key).is_none()), "{todo:?}");
        // The same for the very first code from her: no lock info, no unlock.
        let (todo, _) = plan_for(&[], &[("mia", 10, vec![file(50), file(60)])], here);
        assert!(todo.iter().all(|c| lock_target(&c.key).is_none()), "{todo:?}");
        // And the changes between two such codes list no lock either.
        let (was, now) = (read(&code(vec![file(50)])).unwrap(), read(&code(vec![file(50), file(60)])).unwrap());
        assert!(changes(&was, &now).iter().all(|c| lock_target(&c.key).is_none()));
    }

    #[test]
    fn a_lock_on_a_version_only_is_kept_apart_from_the_whole_mod_lock() {
        // Only one update kept out: not locked, a version noted.
        let m = read(&code(vec![file(50), lock("p:5", "m5-2.jar", false)])).unwrap();
        assert_eq!(m.state(LK), State::File { file: "lock:m5-2.jar".into(), on: false });
        assert_eq!(lock_of(&m.state(LK)), (false, "m5-2.jar".into()));
        let plain = read(&code(vec![file(50), lock("p:5", "", true)])).unwrap();
        assert_ne!(m.state(LK), plain.state(LK));
        assert_eq!(read(&code(vec![file(50)])).unwrap().state(LK), State::Unlisted);
        let c = changes(&plain, &m);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].kind(), Kind::Locked);
    }

    fn inst_with(tag: &str, locks: &[(u64, bool, &str)]) -> Instance {
        let root = std::env::temp_dir().join(format!("hopper-gs-lock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("mods")).unwrap();
        let addons = locks
            .iter()
            .map(|&(p, locked, v)| Addon {
                project_id: p,
                file_id: p * 10,
                file_name: format!("m{p}.jar"),
                name: format!("M{p}"),
                enabled: true,
                locked,
                locked_version: v.into(),
                ..Default::default()
            })
            .collect();
        let inst = Instance { root, name: "T".into(), game_version: "1.21.1".into(), loader: "neoforge-21.1.200".into(), addons, ..Default::default() };
        inst.save().unwrap();
        inst
    }

    fn change(was: State, now: State, key: &str) -> Change {
        Change { key: key.into(), was, now, entry: None }
    }

    #[test]
    fn set_locks_locks_unlocks_and_keeps_the_version_note() {
        let mut inst = inst_with("set", &[(5, false, ""), (6, true, "m6-2.jar"), (7, false, "")]);
        let mut merged = Merged::default();
        let lock5 = change(State::Unlisted, State::File { file: "lock:".into(), on: true }, "~lock:p:5");
        let unlock6 = change(State::File { file: "lock:m6-2.jar".into(), on: true }, State::Unlisted, "~lock:p:6");
        let note7 = change(State::Unlisted, State::File { file: "lock:m7-3.jar".into(), on: false }, "~lock:p:7");
        merged.made = vec![("~lock:p:5".into(), Kind::Locked, "Mia".into())];
        assert!(set_locks(&mut inst, &[lock5, unlock6, note7], &mut merged));
        assert_eq!((inst.addons[0].locked, inst.addons[0].locked_version.as_str()), (true, ""));
        assert_eq!((inst.addons[1].locked, inst.addons[1].locked_version.as_str()), (false, ""));
        assert_eq!((inst.addons[2].locked, inst.addons[2].locked_version.as_str()), (false, "m7-3.jar"));
        assert_eq!(merged.made.len(), 1, "a real change stays in the list");
        // An echo (already like that) and a mod this PC hasn't change nothing.
        let mut merged = Merged { made: vec![("~lock:p:5".into(), Kind::Locked, "Mia".into()), ("~lock:p:99".into(), Kind::Locked, "Mia".into())], ..Default::default() };
        let echo = change(State::Unlisted, State::File { file: "lock:".into(), on: true }, "~lock:p:5");
        let absent = change(State::Unlisted, State::File { file: "lock:".into(), on: true }, "~lock:p:99");
        assert!(!set_locks(&mut inst, &[echo, absent], &mut merged));
        assert!(merged.made.is_empty());
        // A mod kept only on this PC keeps its own lock.
        inst.only_here.insert("p:7".into(), crate::onlyhere::Kept { theirs: None, name: "M7".into() });
        let lock7 = change(State::Unlisted, State::File { file: "lock:".into(), on: true }, "~lock:p:7");
        assert!(!set_locks(&mut inst, &[lock7], &mut Merged::default()));
        assert!(!inst.addons[2].locked);
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    /// The whole merge: Mia locks a mod, it is locked here, it says who and
    /// the file stays; Mia unlocks, and it's unlocked here again.
    #[test]
    fn a_member_locking_and_unlocking_is_done_here_by_the_merge() {
        let inst = inst_with("merge", &[(5, false, ""), (6, false, "")]);
        let run = |copy: &PackCopy| {
            let jobs = crate::jobs::Jobs::default();
            merge(&inst.root, copy, true, false, true, &jobs.start("t")).unwrap()
        };
        let mut copy = PackCopy::default();
        copy.got.insert("mia".into(), got(code(vec![]), 1, "Mia"));
        copy.sent = code(vec![]);
        copy.arrive("mia", got(code(vec![lock("p:5", "", true), lock("p:6", "m6-2.jar", false)]), 10, "Mia"));
        let merged = run(&copy);
        let now = crate::instances::load(&inst.root).unwrap();
        assert_eq!((now.addons[0].locked, now.addons[0].locked_version.as_str()), (true, ""));
        assert_eq!((now.addons[1].locked, now.addons[1].locked_version.as_str()), (false, "m6-2.jar"));
        assert_eq!(merged.taken.len(), 1);
        assert!(!merged.made.is_empty(), "the store refreshes the instance");
        let m = merged.marks.iter().find(|(k, _)| k == "~lock:p:5").unwrap().1.clone();
        assert_eq!(m.kind.says(&m.name), "Locked by Mia");
        // This PC's next code carries the locks, so it doesn't undo them.
        let here = crate::sharecode::make(&now, &crate::jobs::Jobs::default().start("make")).unwrap().share;
        assert!(here.entries.contains(&lock("p:5", "", true)) && here.entries.contains(&lock("p:6", "m6-2.jar", false)));
        // Mia unlocks both.
        let mut copy = PackCopy::default();
        copy.got.insert("mia".into(), got(code(vec![lock("p:5", "", true), lock("p:6", "m6-2.jar", false)]), 10, "Mia"));
        copy.sent = crate::sharecode::encode(&here);
        copy.arrive("mia", got(code(vec![]), 20, "Mia"));
        let merged = run(&copy);
        let now = crate::instances::load(&inst.root).unwrap();
        assert!(now.addons.iter().all(|a| !a.locked && a.locked_version.is_empty()), "{:?}", now.addons);
        let m = merged.marks.iter().find(|(k, _)| k == "~lock:p:5").unwrap().1.clone();
        assert_eq!(m.kind.says(&m.name), "Unlocked by Mia");
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    /// A locked mod's file is kept on every PC: a member's update of a mod
    /// that this PC locks (because Mia just did) is left out as before.
    #[test]
    fn what_a_lock_keeps_is_the_file() {
        let mut inst = inst_with("keeps", &[(5, true, "")]);
        let update = change(State::File { file: "cf:50".into(), on: true }, State::File { file: "cf:51".into(), on: true }, "p:5");
        assert!(locked_out(&inst, &update));
        inst.addons[0].locked = false;
        assert!(!locked_out(&inst, &update));
        let _ = std::fs::remove_dir_all(&inst.root);
    }
}
