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
    if *here == c.now {
        return Verdict::Same;
    }
    if mark.is_some_and(|m| m.seq > seq) {
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

/// A code another member sent, kept until it's taken in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Got {
    pub code: String,
    pub seq: u64,
    pub at: u64,
    pub name: String,
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
/// then that code is dropped).
pub fn plan(
    copy: &PackCopy,
    here: &Mods,
    sent: Option<&Mods>,
    mut decode: impl FnMut(&str) -> Option<Mods>,
) -> (Vec<Change>, Merged) {
    let mut out = Merged::default();
    let mut todo: Vec<Change> = Vec::new();
    let mut marks = copy.marks.clone();
    // What this copy will be once the changes so far are made.
    let mut will: HashMap<String, State> = HashMap::new();
    let state_of = |m: &Mods, k: &str| m.map.get(k).map(|(s, _)| s.clone()).unwrap_or_default();
    for (from, g) in copy.queue() {
        let Some(now) = decode(&g.code) else {
            out.taken.push((from, g));
            continue;
        };
        let Some(prev) = copy.got.get(&from).and_then(|p| decode(&p.code)) else {
            // Their first code: the starting point, nothing to take from it.
            out.taken.push((from, g));
            continue;
        };
        let mut held = false;
        for c in changes(&prev, &now) {
            let cur = will.get(&c.key).cloned().unwrap_or_else(|| state_of(here, &c.key));
            let unsent = sent.is_some_and(|s| state_of(s, &c.key) != state_of(here, &c.key));
            match verdict(&c, g.seq, marks.get(&c.key), &cur, unsent) {
                Verdict::Apply => {
                    let kind = c.kind();
                    let m = Mark { state: c.now.clone(), kind, by: from.clone(), name: g.name.clone(), seq: g.seq, at: g.at };
                    marks.insert(c.key.clone(), m.clone());
                    out.marks.push((c.key.clone(), m));
                    out.made.push((c.key.clone(), kind, g.name.clone()));
                    will.insert(c.key.clone(), c.now.clone());
                    todo.retain(|t| t.key != c.key);
                    todo.push(c);
                }
                Verdict::Held => held = true,
                Verdict::Same | Verdict::Older => {}
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
    let find = |inst: &Instance, key: &str| inst.addons.iter().position(|a| addon_key(a) == key);
    let mut failed: Vec<(String, anyhow::Error)> = Vec::new();
    for (n, c) in todo.iter().enumerate() {
        job.check_cancelled()?;
        job.progress(n, todo.len());
        let at = find(inst, &c.key);
        let name = at.map(|i| inst.addons[i].name.clone()).unwrap_or_else(|| c.key.clone());
        let r: anyhow::Result<()> = match (&c.now, &c.entry) {
            (State::Off, _) => match at {
                Some(i) => crate::install::set_enabled(inst, i, false),
                None => Ok(()),
            },
            (State::Removed, _) => match at {
                Some(i) => crate::install::remove(inst, i),
                None => Ok(()),
            },
            (State::Unlisted, _) => match (&c.was, at) {
                (State::Off, Some(i)) => crate::install::set_enabled(inst, i, true),
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

/// Takes the waiting codes into the instance at `root`: works out each
/// member's changes and makes the ones that win here. Runs off the UI
/// thread. `allow`: mods from other links may be put in by themselves.
/// `ignore_unsent`: a change here waited for its publish too long; the
/// other members' changes go in anyway.
pub fn merge(root: &Path, copy: &PackCopy, allow: bool, ignore_unsent: bool, job: &Job) -> anyhow::Result<Merged> {
    let mut inst = crate::instances::load(root).with_context(|| format!("{} isn't an instance any more", root.display()))?;
    if inst.unreadable.is_some() {
        if crate::instances::may_read_later(&inst) {
            anyhow::bail!("Another program has this instance's files open.");
        }
        anyhow::bail!("{}'s settings file is damaged.", inst.name);
    }
    let here = crate::sharecode::make(&inst, job)?.share;
    let read = |c: &str| (!c.is_empty()).then(|| crate::sharecode::decode(c).ok()).flatten();
    let sent = if ignore_unsent { None } else { read(&copy.sent) };
    let theirs: Vec<Share> = copy.got.values().chain(copy.waiting.values()).filter_map(|g| read(&g.code)).collect();
    let mut all: Vec<&Share> = vec![&here];
    all.extend(sent.iter());
    all.extend(theirs.iter());
    let found = projects_of(&inst, &all);
    let here = mods(&here, lookup(&found));
    let sent = sent.map(|s| mods(&s, lookup(&found)));
    let (todo, mut merged) = plan(copy, &here, sent.as_ref(), |c| read(c).map(|s| mods(&s, lookup(&found))));
    // Mods kept only on this PC only learn how the others have them.
    let (todo, kept) = crate::onlyhere::split(&mut inst, todo, &mut merged);
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
        apply(&mut inst, &todo, &skip, job)?;
    }
    Ok(merged)
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
        let group = Some(f.group.clone())
            .filter(|g| joined.contains(g))
            .or_else(|| groups_of(f).into_iter().find(|g| joined.contains(g)));
        let Some(group) = group else { continue };
        let mut c = PackCopy { inst: f.inst.clone(), group, local: f.local.clone(), ..Default::default() };
        c.got.insert(f.peer_id.clone(), Got { code: f.code.clone(), seq: 0, at: f.at, name: f.peer_name.clone() });
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
        crate::install::set_enabled(inst, i, on)?;
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
        Got { code: c, seq, at: seq, name: name.into() }
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
        let (todo, merged) = plan(&copy, &here, Some(&here), dec);
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
        let (todo, _) = plan(&third, &m(base.clone()), None, dec);
        assert_eq!(todo.last().map(|c| c.now.clone()), Some(st(22, true)));
        // Mia's PC (already off, her mark at 10) takes Sam's later change.
        let mut on_mia = PackCopy::default();
        start(&mut on_mia);
        on_mia.got.remove("mia");
        on_mia.marks.insert("p:2".into(), Mark { state: st(21, false), seq: 10, by: "mia".into(), ..Default::default() });
        on_mia.arrive("sam", got(sam.clone(), 11, "Sam"));
        let here = m(vec![cf(21, false)]);
        let (todo, _) = plan(&on_mia, &here, Some(&here), dec);
        assert_eq!(todo.iter().map(|c| c.now.clone()).collect::<Vec<_>>(), [st(22, true)]);
        // Sam's PC (his mark at 11) drops Mia's earlier one.
        let mut on_sam = PackCopy::default();
        start(&mut on_sam);
        on_sam.got.remove("sam");
        on_sam.marks.insert("p:2".into(), Mark { state: st(22, true), seq: 11, by: "sam".into(), ..Default::default() });
        on_sam.arrive("mia", got(mia, 10, "Mia"));
        let here = m(vec![cf(22, true)]);
        let (todo, merged) = plan(&on_sam, &here, Some(&here), dec);
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
        let (todo, _) = plan(&copy, &here, Some(&here), dec);
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
        let (todo, merged) = plan(&copy, &here, Some(&sent), dec);
        assert!(todo.is_empty());
        assert!(merged.held && merged.taken.is_empty(), "kept waiting for the publish");
        // Once sent (seq 15, marked as ours), Mia's older change loses.
        let mut copy2 = copy.clone();
        let mut marks = copy2.marks.clone();
        mark_own(&mut marks, Some(&sent), &here, "me", 15, 15);
        copy2.marks = marks;
        let (todo, merged) = plan(&copy2, &here, Some(&here), dec);
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
        let (todo, merged) = plan(&copy, &here, Some(&here), dec);
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
}
