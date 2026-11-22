//! "Does my copy really have what the group has?": a check against what is
//! on disk, and the repair of what it finds (`design/group-sync-status.md`).
//!
//! Group sync works on the difference between a member's new code and the
//! last one taken from them ([`PackCopy::got`]), never on the mods folder. So
//! a copy that silently lacks a mod (a lookup failed once, a first install
//! left it out, a try was cut short) says "same as the group" for good, and
//! nothing could put the mod back. This module compares the two directly:
//!
//! - what the group says each mod is: the group's last change of it
//!   ([`PackCopy::marks`], which decide a mod the way [`crate::groupsync`]'s
//!   `verdict` does: the latest change wins), else what the members' last
//!   codes taken in list;
//! - what the instance has: its mods, off or on, which file, locked, kept
//!   only on this PC.
//!
//! Each difference is one [`Row`] with its reason ([`Why`]). Only a mod that
//! is missing, another file, switched differently, or left out for its source
//! can be repaired ([`Why::repairable`]); a mod kept only on this PC and one
//! the player locked are listed and never touched. A deliberate removal is no
//! difference: the group's mark says "removed", and nothing is expected.
//!
//! Nothing here looks at the disk beyond the instance's own record (no file
//! is read), and the one network question is the project of a code's file this
//! instance doesn't have ([`crate::groupsync::projects_of`], batched), so a
//! check is cheap even for a pack of 300 mods. It runs when asked and after a
//! merge, never on the publish tick.

use crate::groupsync::{self, Change, Got, Kind, Mark, Merged, Mods, Owed, PackCopy, State, file_here, find_addon};
use crate::instances::{Addon, Instance};
use crate::jobs::Job;
use crate::modrinth::is_mr;
use crate::sharecode::{Entry, Share};
use crate::sources::carry::Pin;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

/// Why a mod of the group differs here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Why {
    /// The group has it; this copy doesn't.
    Missing,
    /// This copy has the mod, but another file (version) than the group's.
    Different,
    /// Off here, on in the group.
    OffHere,
    /// On here, off in the group.
    OnHere,
    /// A mod from GitHub or another link whose source this PC hasn't
    /// confirmed: it was left out on purpose until the player says yes.
    LeftOut,
    /// The player keeps this mod only on this PC, so the group's version of
    /// it doesn't count. Listed, never changed.
    OnlyHere,
    /// The player locked this mod's version. Listed, never changed.
    Locked,
}

impl Why {
    /// Whether "Make my copy match" changes it.
    pub fn repairable(self) -> bool {
        !matches!(self, Why::OnlyHere | Why::Locked)
    }

    /// The button on the row.
    pub fn action(self) -> &'static str {
        match self {
            Why::Missing => "Get it",
            Why::Different => "Update",
            Why::OffHere => "Turn it on",
            Why::OnHere => "Turn it off",
            Why::LeftOut => "Allow and get it",
            Why::OnlyHere | Why::Locked => "",
        }
    }
}

/// One mod that differs.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// The mod's key ([`groupsync::addon_key`]).
    pub key: String,
    /// Its name for people ("" when only its id is known: the app looks it
    /// up for the few rows it shows).
    pub name: String,
    /// Its project (for its icon and page), 0 when unknown.
    pub project: u64,
    pub why: Why,
    /// This copy's file is older than the group's (both are CurseForge
    /// files, which are numbered in the order they came out).
    pub older: bool,
    /// What makes it as the group has it; none when nothing can do that (a
    /// kept or locked mod, or a code that doesn't say which file).
    pub change: Option<Change>,
    /// The mod from a link, for [`Why::LeftOut`]: the source to say yes to.
    pub pin: Option<Pin>,
    /// Who made the group's last change of it, when known.
    pub by: String,
    pub by_name: String,
    /// The mark that change would carry when this copy has none for it.
    pub mark: Option<Mark>,
}

impl Row {
    /// Whether pressing its button can do something.
    pub fn fixable(&self) -> bool {
        self.why.repairable() && self.change.is_some()
    }
}

/// What a check found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Verified {
    pub rows: Vec<Row>,
    /// Files of the group's codes whose mod couldn't be looked up because
    /// CurseForge or Modrinth didn't answer: the check can't say about them.
    pub unchecked: Vec<Entry>,
    /// When (unix seconds).
    pub at: u64,
}

impl Verified {
    /// The rows "Make my copy match" would change.
    pub fn to_fix(&self) -> Vec<&Row> {
        self.rows.iter().filter(|r| r.fixable()).collect()
    }

    /// The rows that are the player's own choice (kept only here, locked).
    pub fn kept(&self) -> Vec<&Row> {
        self.rows.iter().filter(|r| !r.why.repairable()).collect()
    }

    /// Whether the copy has what the group has (what is kept or locked
    /// doesn't count, and a check that couldn't finish isn't a yes).
    pub fn matches(&self) -> bool {
        self.rows.iter().all(|r| !r.why.repairable()) && self.unchecked.is_empty()
    }

    /// The headline for the strip: "Your copy is missing 2 mods the group
    /// has." Only about what differs.
    pub fn headline(&self) -> String {
        let n = self.rows.iter().filter(|r| r.why.repairable()).count();
        if n == 0 {
            return "Your copy matches the group.".to_string();
        }
        let missing = self.rows.iter().filter(|r| r.why.repairable()).all(|r| matches!(r.why, Why::Missing | Why::LeftOut));
        crate::groupstatus::behind_says(n, missing)
    }

    /// What the status takes from this check.
    pub fn check(&self) -> crate::groupstatus::Check {
        use crate::groupstatus::Check;
        let rows: Vec<&Row> = self.rows.iter().filter(|r| r.why.repairable()).collect();
        if !rows.is_empty() {
            return Check::Behind { mods: rows.len(), missing: rows.iter().all(|r| matches!(r.why, Why::Missing | Why::LeftOut)) };
        }
        if self.unchecked.is_empty() { Check::Matches } else { Check::Couldnt }
    }
}

/// Everything [`verify`] reads.
pub struct Facts<'a> {
    pub inst: &'a Instance,
    pub copy: &'a PackCopy,
    /// Each member's last code taken in ([`PackCopy::got`]), read into mods
    /// by member id.
    pub codes: &'a BTreeMap<String, Mods>,
    /// The last code this copy published, read.
    pub sent: Option<&'a Mods>,
    /// This copy's pack version as codes name it ([`crate::grouppack::pack_state`]);
    /// None when it can't be told without asking a site.
    pub pack: Option<&'a State>,
    /// The mods from links this PC wouldn't download unasked
    /// ([`crate::sources::carry::auto_skip`] of the codes' pins).
    pub skip: &'a [Pin],
    /// Files already waiting for a browser download (`.hopper-pending.json`):
    /// they aren't listed as missing/repairable, because "Make my copy match"
    /// can't fetch them automatically.
    pub pending: &'a [crate::modpack::Pending],
}

/// What the group says one mod is.
struct Want {
    state: State,
    entry: Option<Entry>,
    by: String,
    by_name: String,
    /// The mark this copy would get for it (None: it has one).
    mark: Option<Mark>,
    /// The mod is owed to this PC (a change left out for its source).
    owed: Option<Owed>,
    /// A code of a member on another pack version says so (see
    /// `groupsync::keep_pack_mods`).
    other_pack: bool,
}

fn special(key: &str) -> bool {
    key.starts_with('~')
}

/// The group's state of one mod: its mark, else the newest of the members'
/// codes that list it. A code that doesn't list it says nothing: a mod a
/// member never had isn't one they removed (a removal is a mark).
fn want_of(f: &Facts, key: &str) -> Option<Want> {
    let mark = f.copy.marks.get(key);
    // The members' codes that list it: newest first by the change's number
    // (its stamp, "made in this code" being the code's own), else the code's.
    let mut cands: Vec<(u64, &str, &Got, &State, &Entry, &Mods)> = Vec::new();
    for (from, g) in &f.copy.got {
        let Some(m) = f.codes.get(from) else { continue };
        let Some((s, e)) = m.map.get(key) else { continue };
        let dated = match g.stamps.get(key) {
            Some(st) if st.seq > 0 => st.seq,
            _ => g.seq,
        };
        cands.push((dated, from.as_str(), g, s, e, m));
    }
    cands.sort_by(|a, b| b.0.cmp(&a.0).then(b.2.seq.cmp(&a.2.seq)).then(a.1.cmp(b.1)));
    let owed = f.copy.owed.iter().find(|o| o.key == key && mark.is_none_or(|m| o.seq >= m.seq)).cloned();
    if let Some(o) = &owed {
        return Some(Want {
            state: o.now.clone(),
            entry: o.entry.clone(),
            by: o.by.clone(),
            by_name: o.name.clone(),
            mark: None,
            owed,
            other_pack: false,
        });
    }
    let (state, entry, by, by_name, new_mark) = match (mark, cands.first()) {
        (Some(m), _) => {
            let entry = cands.iter().find(|c| *c.3 == m.state).map(|c| c.4.clone());
            (m.state.clone(), entry, m.by.clone(), m.name.clone(), None)
        }
        (None, Some((dated, from, g, s, e, _))) => {
            let mark = Mark { state: (*s).clone(), kind: Kind::Added, by: (*from).to_string(), name: g.name.clone(), seq: *dated, at: g.at };
            ((*s).clone(), Some((*e).clone()), (*from).to_string(), g.name.clone(), Some(mark))
        }
        (None, None) => return None,
    };
    let other_pack = match f.pack {
        Some(here) => cands.iter().filter(|c| *c.3 == state).any(|c| c.5.pack != *here),
        None => true,
    };
    Some(Want { state, entry, by, by_name, mark: new_mark, owed: None, other_pack })
}

/// A mod here as the codes name its state: a mod of the pack is unlisted
/// when on and "off" when off; any other is its file.
fn state_here(inst: &Instance, a: Option<&Addon>, sent: Option<&State>) -> State {
    match a {
        None if matches!(sent, Some(State::Removed)) => State::Removed,
        None => State::Unlisted,
        Some(a) if a.from_pack && inst.pack.as_ref().is_some_and(|p| p.project_id != 0) => {
            if a.enabled { State::Unlisted } else { State::Off }
        }
        Some(a) => State::File { file: file_here(a).unwrap_or_default(), on: a.enabled },
    }
}

/// "cf:123" -> 123.
fn cf_number(file: &str) -> Option<u64> {
    file.strip_prefix("cf:")?.parse().ok()
}

/// Whether `pending` already has a browser download waiting for the exact
/// CurseForge file `want` asks for.
fn pending_for(pending: &[crate::modpack::Pending], want: &State) -> bool {
    let State::File { file, .. } = want else { return false };
    cf_number(file).is_some_and(|fid| pending.iter().any(|p| p.file_id == fid))
}

/// The project a key names ("p:123"), 0 when it names none.
fn key_project(key: &str) -> u64 {
    key.strip_prefix("p:").and_then(|p| p.parse().ok()).unwrap_or(0)
}

/// The pin of a mod's entry, if it comes from a link.
fn pin_of(e: Option<&Entry>) -> Option<&Pin> {
    match e {
        Some(Entry::Source { pin, .. }) => Some(pin),
        _ => None,
    }
}

/// The group's state of every mod it has, compared with this copy.
pub fn verify(f: &Facts, now: u64) -> Verified {
    let mut keys: BTreeSet<&str> = BTreeSet::new();
    for m in f.codes.values() {
        keys.extend(m.map.keys().map(String::as_str));
    }
    keys.extend(f.copy.marks.keys().map(String::as_str));
    keys.extend(f.copy.owed.iter().map(|o| o.key.as_str()));
    let mut rows: Vec<Row> = Vec::new();
    // One row per mod here, whichever key the codes name it by.
    let mut seen_at: BTreeSet<usize> = BTreeSet::new();
    let mut seen_key: BTreeSet<String> = BTreeSet::new();
    for key in keys.into_iter().filter(|k| !special(k)) {
        let Some(w) = want_of(f, key) else { continue };
        let at = find_addon(f.inst, key);
        let a = at.map(|i| &f.inst.addons[i]);
        // A change made here that isn't sent yet is this PC's own, going out:
        // the group takes it, it isn't one the copy lacks (`groupsync::plan`
        // holds the others' change to it the same way).
        let here = state_here(f.inst, a, f.sent.map(|s| s.state(key)).as_ref());
        let unsent = f.sent.is_some_and(|s| s.state(key) != here) && f.copy.marks.get(key).is_none_or(|m| m.state != here);
        if unsent {
            continue;
        }
        // A member's change of it is kept aside until this PC's own is sent
        // or taken back ([`PackCopy::held`]): the merge makes it then, and
        // the status says so. Not a mod the copy lacks.
        if f.copy.held.iter().any(|h| h.key == key) {
            continue;
        }
        let Some(row) = row_of(f, key, &w, a, &here) else { continue };
        let first = match at {
            Some(i) => seen_at.insert(i),
            None => seen_key.insert(key.to_string()),
        };
        if first {
            rows.push(row);
        }
    }
    // The ones to fix first, then what the player keeps; alphabetical.
    rows.sort_by(|a, b| (!a.why.repairable(), a.name.to_lowercase(), &a.key).cmp(&(!b.why.repairable(), b.name.to_lowercase(), &b.key)));
    Verified { rows, unchecked: Vec::new(), at: now }
}

/// The row for one mod, or none when this copy has it as the group says.
fn row_of(f: &Facts, key: &str, w: &Want, a: Option<&Addon>, here: &State) -> Option<Row> {
    let inst = f.inst;
    let kept = inst.only_here.contains_key(key) || a.is_some_and(|a| crate::onlyhere::is_kept(inst, a));
    let name = a.map(|a| a.name.clone()).filter(|n| !n.is_empty()).or_else(|| pin_of(w.entry.as_ref()).map(crate::sources::carry::display_name)).unwrap_or_default();
    let project = a.map(Addon::shown_project).filter(|p| *p != 0).unwrap_or_else(|| key_project(key));
    let (why, older) = match (&w.state, a) {
        // A removal, or "as the pack has it": nothing is expected here. A
        // pack mod put back on is a switch.
        (State::Removed, _) => return None,
        (State::Unlisted, Some(a)) if a.from_pack && !a.enabled && f.copy.marks.get(key).is_some_and(|m| m.kind == Kind::On) => (Why::OffHere, false),
        (State::Unlisted, _) => return None,
        (State::Off, Some(a)) if a.enabled => (Why::OnHere, false),
        (State::Off, _) => return None,
        (State::File { .. }, None) => (Why::Missing, false),
        (State::File { file, on }, Some(a)) => {
            let hf = file_here(a);
            if hf.as_deref() != Some(file.as_str()) {
                // A member on another pack version: what their version has
                // of a pack mod isn't this one's.
                if a.from_pack && w.other_pack {
                    return None;
                }
                let older = hf.as_deref().and_then(cf_number).zip(cf_number(file)).is_some_and(|(h, g)| h < g);
                (Why::Different, older)
            } else if a.enabled != *on {
                (if *on { Why::OffHere } else { Why::OnHere }, false)
            } else {
                return None;
            }
        }
    };
    let mark = w.mark.clone().map(|m| Mark { kind: Change { key: key.to_string(), was: here.clone(), now: w.state.clone(), entry: None }.kind(), ..m });
    let pin = pin_of(w.entry.as_ref()).cloned();
    // Already waiting in the browser-download list for this exact file: not a
    // mod "Make my copy match" can fetch automatically (`groupstatus::Status::ByHand`).
    if !kept && matches!(why, Why::Missing | Why::Different) && pending_for(f.pending, &w.state) {
        return None;
    }
    let (why, change) = if kept {
        (Why::OnlyHere, None)
    } else if a.is_some_and(|a| a.locked) && why == Why::Different {
        (Why::Locked, None)
    } else {
        // The mod's source isn't allowed on this PC (or the member's change
        // is still owed): it's left out until the player says yes.
        let gets = matches!(why, Why::Missing | Why::Different);
        let left_out = gets && pin.as_ref().is_some_and(|p| w.owed.as_ref().is_some_and(|o| !o.approved) || f.skip.contains(p));
        let why = if left_out { Why::LeftOut } else { why };
        // A switch needs no file from the codes: the one here, on or off.
        let entry = w.entry.clone().or_else(|| matches!(why, Why::OffHere | Why::OnHere).then(|| toggled_entry(a, &w.state)).flatten());
        let can = entry.is_some() || matches!(w.state, State::Unlisted | State::Off);
        let change = can.then(|| Change { key: key.to_string(), was: here.clone(), now: w.state.clone(), entry });
        (why, change)
    };
    // A mod to get that no code says the file of: nothing to offer (its
    // member has moved on; their newer code brings the change).
    if why.repairable() && change.is_none() {
        return None;
    }
    Some(Row { key: key.to_string(), name, project, why, older, change, pin, by: w.by.clone(), by_name: w.by_name.clone(), mark })
}

/// The entry for switching a mod that's here on or off, when the codes don't
/// carry one for the state wanted: the file here, as the group has it on or
/// off.
fn toggled_entry(a: Option<&Addon>, want: &State) -> Option<Entry> {
    let State::File { on, .. } = want else { return None };
    let a = a?;
    match crate::sharecode::file_entry(a)? {
        Entry::CurseFile { file, .. } => Some(Entry::CurseFile { file, enabled: *on }),
        Entry::ModrinthFile { version, .. } => Some(Entry::ModrinthFile { version, enabled: *on }),
        other => Some(other),
    }
}

/// The pack version this copy is on as a code names it, when that can be
/// told without asking a site.
pub fn pack_of(inst: &Instance) -> Option<State> {
    let Some(p) = inst.pack.as_ref().filter(|p| p.project_id != 0) else { return Some(State::Unlisted) };
    if is_mr(p.project_id) {
        return (!p.mr_project.is_empty() && !p.mr_version.is_empty())
            .then(|| State::File { file: format!("pack:mr:{}:{}", p.mr_project, p.mr_version), on: true });
    }
    (p.file_id != 0).then(|| State::File { file: format!("pack:cf:{}:{}", p.project_id, p.file_id), on: true })
}

/// [`verify`] for an instance and its copy, as the app asks it: reads the
/// codes (looking up the projects of files this instance doesn't have, in
/// batches) and compares. Off the UI thread.
///
/// `allow`: Settings › Mod sources.
pub fn check(inst: &Instance, copy: &PackCopy, allow: bool, now: u64) -> Verified {
    let decode = |c: &str| (!c.is_empty()).then(|| crate::sharecode::decode(c).ok()).flatten();
    let theirs = taken_shares(copy);
    let sent = decode(&copy.sent);
    let mut all: Vec<&Share> = theirs.iter().map(|(_, s)| s).collect();
    all.extend(sent.iter());
    let (found, lookup_failed) = groupsync::projects_of_noted(inst, &all);
    let read = |s: &Share| groupsync::mods(s, groupsync::lookup(&found));
    let codes: BTreeMap<String, Mods> = theirs.iter().map(|(k, s)| ((*k).clone(), read(s))).collect();
    let sent = sent.as_ref().map(read);
    let pack = pack_of(inst);
    let pins: Vec<Pin> = codes
        .values()
        .flat_map(|m| m.map.values())
        .filter_map(|(_, e)| pin_of(Some(e)).cloned())
        .chain(copy.owed.iter().filter_map(|o| pin_of(o.entry.as_ref()).cloned()))
        .collect();
    let mut skip = crate::sources::carry::auto_skip(&pins, allow);
    // A mod the player said yes to goes in whatever the sources say.
    let approved: Vec<Pin> = copy.owed.iter().filter(|o| o.approved).filter_map(|o| pin_of(o.entry.as_ref()).cloned()).collect();
    skip.retain(|p| !approved.contains(p));
    let pending = crate::modpack::pending_downloads(inst);
    let mut v = verify(&Facts { inst, copy, codes: &codes, sent: sent.as_ref(), pack: pack.as_ref(), skip: &skip, pending: &pending }, now);
    // A file whose project can't be told is a mod the check can't speak for.
    // Only when a site failed to answer: a file the site no longer has is not
    // a reason to say "try again" for ever.
    v.unchecked = unchecked_of(&codes, lookup_failed);
    v
}

/// The codes this copy took in (`got`), read. Codes that wait (`waiting`) are
/// the group's changes this PC hasn't taken in: they are Updating, or Parked
/// for a sync mode that waits for the player, and never "missing".
fn taken_shares(copy: &PackCopy) -> Vec<(&String, Share)> {
    copy.got.iter().filter(|(_, g)| !g.code.is_empty()).filter_map(|(k, g)| crate::sharecode::decode(&g.code).ok().map(|s| (k, s))).collect()
}

/// The files the check can't speak for: those whose project couldn't be
/// looked up because a site didn't answer (`lookup_failed`). Empty when a
/// site answered, even if it no longer has the file: that isn't a reason to
/// say "try again" for ever.
fn unchecked_of(codes: &BTreeMap<String, Mods>, lookup_failed: bool) -> Vec<Entry> {
    let mut un: Vec<Entry> = Vec::new();
    if lookup_failed {
        for e in codes.values().flat_map(|m| m.unknown.iter()) {
            if !un.contains(e) {
                un.push(e.clone());
            }
        }
    }
    un
}

/// What making the copy match did.
#[derive(Debug, Default)]
pub struct Repaired {
    /// The rows that were made (their mods are as the group has them now,
    /// unless the apply says otherwise).
    pub done: Vec<Row>,
    /// The rows left as they were: the player didn't allow their source, or
    /// they aren't the player's to repair.
    pub left: Vec<Row>,
    /// Marks to save, and the owed list after (see [`Merged`]).
    pub merged: Merged,
    /// CurseForge files that couldn't be downloaded automatically and were
    /// queued in `.hopper-pending.json` for a browser download.
    pub by_hand: Vec<crate::modpack::Pending>,
    /// The check again, once it was made.
    pub after: Option<Verified>,
}

/// The rows to make and the rows to leave, of what [`check`] found. A row is
/// made when it can be repaired, the player asked for it (`keys`: the rows
/// they saw; None: all), and, for a mod from a link nobody has confirmed, they
/// said yes to its source (`approved`, or an owed change they approved).
pub fn select(rows: Vec<Row>, copy: &PackCopy, keys: Option<&[String]>, approved: &[Pin]) -> (Vec<Row>, Vec<Row>) {
    let (mut doing, mut left) = (Vec::new(), Vec::new());
    for r in rows {
        let wanted = keys.is_none_or(|k| k.contains(&r.key));
        let said_yes = r.pin.as_ref().is_some_and(|p| approved.contains(p)) || copy.owed.iter().any(|o| o.key == r.key && o.approved);
        if wanted && r.fixable() && (r.why != Why::LeftOut || said_yes) {
            doing.push(r);
        } else {
            left.push(r);
        }
    }
    (doing, left)
}

/// Makes the instance at `root` as the group has it, for the rows [`check`]
/// finds. `keys`: only these mods (the rows the player saw); None: every one
/// that can be repaired. `approved`: the mods from links the player said yes
/// to; a [`Why::LeftOut`] row not in it is left out. The changes go through
/// [`groupsync::apply`], the same as sync's own: nothing here installs a
/// file by another path. Runs off the UI thread.
pub fn repair(root: &Path, copy: &PackCopy, keys: Option<&[String]>, approved: &[Pin], allow: bool, job: &Job) -> anyhow::Result<Repaired> {
    // Held to the end, as a merge holds it: an update that finished
    // meanwhile mustn't be undone by an older copy of the record.
    let _edit = crate::instances::edit_lock(root);
    let mut inst = crate::instances::load(root).ok_or_else(|| anyhow::anyhow!("{} isn't an instance any more", root.display()))?;
    if inst.unreadable.is_some() {
        anyhow::bail!("{}'s settings file is damaged.", inst.name);
    }
    let found = check(&inst, copy, allow, 0);
    let (unchecked, at) = (found.unchecked.clone(), found.at);
    let (doing, left) = select(found.rows, copy, keys, approved);
    let mut out = Repaired { left, ..Default::default() };
    let todo: Vec<Change> = doing.iter().filter_map(|r| r.change.clone()).collect();
    if todo.is_empty() {
        out.after = Some(Verified { rows: out.left.clone(), unchecked, at });
        return Ok(out);
    }
    job.show();
    // Mods from links aren't downloaded unasked: the ones the player said
    // yes to are the only ones past the source check.
    let wanted: Vec<Pin> = doing.iter().filter_map(|r| r.pin.clone()).collect();
    let mut skip = crate::sources::carry::auto_skip(&wanted, allow);
    skip.retain(|p| !approved.contains(p) && !copy.owed.iter().filter(|o| o.approved).filter_map(|o| pin_of(o.entry.as_ref())).any(|q| q == p));
    let by_hand = groupsync::apply_with_pending(&mut inst, &todo, &skip, job)?;
    let by_hand_keys: BTreeSet<String> = by_hand.iter().map(|(k, _)| k.clone()).collect();
    let mut landed: Vec<Row> = Vec::new();
    for r in doing {
        if let Some(m) = &r.mark {
            out.merged.marks.push((r.key.clone(), m.clone()));
        }
        if !by_hand_keys.contains(&r.key) {
            out.merged.made.push((r.key.clone(), r.change.as_ref().map(Change::kind).unwrap_or_default(), r.by_name.clone()));
            landed.push(r);
        }
    }
    out.by_hand = by_hand.into_iter().map(|(_, p)| p).collect();
    // What was owed and is made now isn't owed any more.
    if copy.owed.iter().any(|o| landed.iter().any(|r| r.key == o.key)) {
        out.merged.owed = Some(copy.owed.iter().filter(|o| !landed.iter().any(|r| r.key == o.key)).cloned().collect());
    }
    out.done = landed;
    // What the folder says now, so the strip shows the result without
    // another round trip.
    out.after = Some(check(&inst, copy, allow, 0));
    Ok(out)
}

/// What the rows of `v` say in a line for the task and the notice: "Got
/// Sodium and JEI."
pub fn done_says(done: &[Row]) -> String {
    let names: Vec<String> = done.iter().map(|r| if r.name.is_empty() { "a mod".to_string() } else { r.name.clone() }).collect();
    let list = match names.as_slice() {
        [] => return String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [a, b, rest @ ..] => format!("{a}, {b} and {} more", rest.len()),
    };
    format!("Made your copy match the group for {list}.")
}

/// Names of rows by key, for a row that came without one.
pub fn name_rows(rows: &mut [Row], names: &HashMap<String, String>) {
    for r in rows.iter_mut().filter(|r| r.name.is_empty()) {
        if let Some(n) = names.get(&r.key) {
            r.name = n.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instances::PackRef;
    use crate::sharecode::{Base, Target};
    use crate::sources::ModSource;

    fn share(entries: Vec<Entry>) -> Share {
        Share { base: Base::Plain { game_version: "1.21.1".into(), loader: "neoforge".into() }, name: None, loader: None, entries }
    }
    fn cf(file: u64, on: bool) -> Entry {
        Entry::CurseFile { file, enabled: on }
    }
    /// File n belongs to project n / 10 (11 and 12 are two files of mod 1).
    fn read(entries: Vec<Entry>) -> Mods {
        groupsync::mods(&share(entries), |e| match e {
            Entry::CurseFile { file, .. } => Some(file / 10),
            _ => None,
        })
    }
    fn addon(project: u64, file: u64, on: bool) -> Addon {
        Addon { project_id: project, file_id: file, enabled: on, name: format!("Mod {project}"), file_name: format!("mod{project}.jar"), ..Default::default() }
    }
    fn inst(addons: Vec<Addon>) -> Instance {
        Instance { addons, ..Default::default() }
    }
    fn mark(state: State, kind: Kind, seq: u64) -> Mark {
        Mark { state, kind, by: "mia".into(), name: "Mia".into(), seq, at: seq }
    }
    fn cf_state(file: u64, on: bool) -> State {
        State::File { file: format!("cf:{file}"), on }
    }
    /// A copy that took in these members' codes: (member, seq, entries).
    fn group(members: Vec<(&str, u64, Vec<Entry>)>) -> (PackCopy, BTreeMap<String, Mods>) {
        let mut copy = PackCopy::default();
        let mut codes = BTreeMap::new();
        for (id, seq, entries) in members {
            copy.got.insert(id.into(), Got { seq, at: seq, name: id.to_uppercase(), ..Default::default() });
            codes.insert(id.to_string(), read(entries));
        }
        (copy, codes)
    }
    fn run(inst: &Instance, copy: &PackCopy, codes: &BTreeMap<String, Mods>, sent: Option<&Mods>, skip: &[Pin]) -> Verified {
        verify(&Facts { inst, copy, codes, sent, pack: Some(&State::Unlisted), skip, pending: &[] }, 100)
    }
    fn pin(file: &str) -> Pin {
        let source = ModSource { url: "o/r".into(), pattern: "cool-#.jar".into(), publisher: "o/r".into(), ..Default::default() };
        Pin { file: file.into(), source }
    }
    fn only(v: &Verified) -> &Row {
        assert_eq!(v.rows.len(), 1, "{:?}", v.rows);
        &v.rows[0]
    }

    #[test]
    fn a_site_that_did_not_answer_leaves_files_unchecked_and_one_that_did_does_not() {
        let mut codes = BTreeMap::new();
        let mut m = read(vec![]);
        m.unknown = vec![cf(31, true)];
        codes.insert("a".to_string(), m);
        assert_eq!(unchecked_of(&codes, true), vec![cf(31, true)]);
        // A file the site no longer has: no "try again" for ever.
        assert!(unchecked_of(&codes, false).is_empty());
        // And a check that has such files never says it matches.
        let v = Verified { unchecked: unchecked_of(&codes, true), ..Default::default() };
        assert!(!v.matches());
        assert_eq!(v.check(), crate::groupstatus::Check::Couldnt);
    }

    #[test]
    fn only_the_codes_taken_in_are_read_never_the_ones_that_wait() {
        // Parked (Only maker) and push-by-hand changes wait: the copy isn't
        // "missing" them, the status says it waits.
        let code = |file| crate::sharecode::encode(&share(vec![cf(file, true)]));
        let mut copy = PackCopy::default();
        copy.got.insert("a".into(), Got { code: code(11), seq: 1, ..Default::default() });
        copy.waiting.insert("b".into(), Got { code: code(21), seq: 2, ..Default::default() });
        let read: Vec<&String> = taken_shares(&copy).into_iter().map(|(k, _)| k).collect();
        assert_eq!(read, vec!["a"]);
    }

    #[test]
    fn a_mod_the_group_has_and_this_copy_lacks_is_missing() {
        // The bug: the copy lacks mod 1 and nothing else says so.
        let (copy, codes) = group(vec![("a", 5, vec![cf(11, true), cf(21, true)])]);
        let v = run(&inst(vec![addon(2, 21, true)]), &copy, &codes, None, &[]);
        let r = only(&v);
        assert_eq!((r.key.as_str(), r.why), ("p:1", Why::Missing));
        assert_eq!(r.change.as_ref().map(|c| (c.now.clone(), c.entry.clone())), Some((cf_state(11, true), Some(cf(11, true)))));
        assert_eq!(r.by_name, "A");
        assert!(!v.matches());
        assert_eq!(v.headline(), "Your copy is missing 1 mod the group has.");
        assert_eq!(v.check(), crate::groupstatus::Check::Behind { mods: 1, missing: true });
    }

    #[test]
    fn a_mod_already_at_the_right_version_is_not_listed() {
        let (copy, codes) = group(vec![("a", 5, vec![cf(11, true), cf(21, false)])]);
        let v = run(&inst(vec![addon(1, 11, true), addon(2, 21, false)]), &copy, &codes, None, &[]);
        assert!(v.rows.is_empty(), "{:?}", v.rows);
        assert!(v.matches());
        assert_eq!(v.headline(), "Your copy matches the group.");
    }

    #[test]
    fn another_file_is_listed_and_says_if_this_copy_is_older() {
        let (copy, codes) = group(vec![("a", 5, vec![cf(12, true)])]);
        let v = run(&inst(vec![addon(1, 11, true)]), &copy, &codes, None, &[]);
        let r = only(&v);
        assert_eq!((r.why, r.older), (Why::Different, true));
        assert_eq!(r.change.as_ref().map(|c| c.now.clone()), Some(cf_state(12, true)));
        // The other way round (newer here): still different, not older.
        let (copy, codes) = group(vec![("a", 5, vec![cf(11, true)])]);
        let v = run(&inst(vec![addon(1, 12, true)]), &copy, &codes, None, &[]);
        assert_eq!((only(&v).why, only(&v).older), (Why::Different, false));
    }

    #[test]
    fn a_switch_differs_in_both_directions() {
        let (copy, codes) = group(vec![("a", 5, vec![cf(11, true), cf(21, false)])]);
        let v = run(&inst(vec![addon(1, 11, false), addon(2, 21, true)]), &copy, &codes, None, &[]);
        let whys: Vec<(&str, Why)> = v.rows.iter().map(|r| (r.key.as_str(), r.why)).collect();
        assert_eq!(whys, [("p:1", Why::OffHere), ("p:2", Why::OnHere)]);
        assert!(v.rows.iter().all(Row::fixable));
    }

    #[test]
    fn a_pack_mod_turned_off_in_the_group_is_on_here() {
        let mut a = addon(5, 51, true);
        a.from_pack = true;
        let mut i = inst(vec![a]);
        i.pack = Some(PackRef { project_id: 100, file_id: 1, ..Default::default() });
        let (copy, codes) = group(vec![("a", 5, vec![Entry::Off(Target::Curse(5))])]);
        let v = run(&i, &copy, &codes, None, &[]);
        let r = only(&v);
        assert_eq!((r.why, r.change.as_ref().map(|c| c.now.clone())), (Why::OnHere, Some(State::Off)));
    }

    #[test]
    fn a_deliberate_removal_is_not_called_missing() {
        // The 79361df5 case: Mia's code listed mod 1 before she took it out;
        // her removal is the later change, so the copy must not get it back.
        let (mut copy, codes) = group(vec![("mia", 5, vec![cf(11, true)])]);
        copy.marks.insert("p:1".into(), mark(State::Unlisted, Kind::Removed, 9));
        let v = run(&inst(vec![]), &copy, &codes, None, &[]);
        assert!(v.rows.is_empty(), "{:?}", v.rows);
        assert!(v.matches());
        // Without the removal's mark the same code makes it missing.
        copy.marks.clear();
        assert_eq!(only(&run(&inst(vec![]), &copy, &codes, None, &[])).why, Why::Missing);
    }

    #[test]
    fn a_removal_not_sent_yet_is_this_pcs_own_and_not_repaired() {
        // Taken out here; the last code sent still had it.
        let (copy, codes) = group(vec![("a", 5, vec![cf(11, true)])]);
        let sent = read(vec![cf(11, true)]);
        let v = run(&inst(vec![]), &copy, &codes, Some(&sent), &[]);
        assert!(v.rows.is_empty(), "{:?}", v.rows);
        // Sent without it (a plain miss): still missing.
        let sent = read(vec![]);
        assert_eq!(only(&run(&inst(vec![]), &copy, &codes, Some(&sent), &[])).why, Why::Missing);
    }

    #[test]
    fn a_change_kept_aside_for_this_pcs_own_is_not_called_different() {
        // Mia moved mod 1 to file 13 while this PC had it changed and
        // unsent; the wait ran out and her code was taken in. This PC took
        // its change back since (file 11 again, as last sent). Her update is
        // kept aside and the next merge makes it: not a mod to repair.
        let (mut copy, codes) = group(vec![("mia", 5, vec![cf(13, true)])]);
        let sent = read(vec![cf(11, true)]);
        let i = inst(vec![addon(1, 11, true)]);
        copy.held.push(groupsync::HeldChange { key: "p:1".into(), now: cf_state(13, true), entry: Some(cf(13, true)), by: "mia".into(), name: "Mia".into(), seq: 5, ..Default::default() });
        let v = run(&i, &copy, &codes, Some(&sent), &[]);
        assert!(v.rows.is_empty(), "{:?}", v.rows);
        // Without it (as before the fix, when the change was dropped) the
        // same copy reads as behind.
        copy.held.clear();
        assert_eq!(only(&run(&i, &copy, &codes, Some(&sent), &[])).why, Why::Different);
    }

    #[test]
    fn the_newest_code_of_a_mod_counts() {
        let (copy, codes) = group(vec![("a", 3, vec![cf(11, true)]), ("b", 7, vec![cf(12, true)])]);
        let v = run(&inst(vec![addon(1, 11, true)]), &copy, &codes, None, &[]);
        assert_eq!(only(&v).change.as_ref().map(|c| c.now.clone()), Some(cf_state(12, true)));
        // A mark that is later than every code (a change passed on) decides.
        let (mut copy, codes) = group(vec![("a", 3, vec![cf(11, true)]), ("b", 7, vec![cf(12, true)])]);
        copy.marks.insert("p:1".into(), mark(cf_state(11, true), Kind::Updated, 9));
        let v = run(&inst(vec![addon(1, 11, true)]), &copy, &codes, None, &[]);
        assert!(v.rows.is_empty(), "{:?}", v.rows);
    }

    #[test]
    fn codes_not_taken_in_say_nothing() {
        // Waiting codes belong to Pull, and a member this copy never took a
        // code from isn't the group's yet.
        let (mut copy, mut codes) = group(vec![("a", 5, vec![cf(11, true)])]);
        copy.waiting.insert("b".into(), Got { seq: 8, ..Default::default() });
        codes.insert("b".into(), read(vec![cf(21, true)]));
        codes.insert("c".into(), read(vec![cf(31, true)]));
        let v = run(&inst(vec![addon(1, 11, true)]), &copy, &codes, None, &[]);
        assert!(v.rows.is_empty(), "{:?}", v.rows);
    }

    #[test]
    fn a_mod_kept_only_here_is_listed_and_never_repaired() {
        let mut i = inst(vec![addon(1, 10, true)]);
        i.only_here.insert("p:1".into(), crate::onlyhere::Kept::default());
        let (copy, codes) = group(vec![("a", 5, vec![cf(12, true)])]);
        let v = run(&i, &copy, &codes, None, &[]);
        let r = only(&v);
        assert_eq!((r.why, r.change.is_none(), r.fixable()), (Why::OnlyHere, true, false));
        assert!(v.to_fix().is_empty());
        assert_eq!(v.kept().len(), 1);
        // What the player keeps doesn't make the copy differ from the group.
        assert!(v.matches());
        assert_eq!(v.headline(), "Your copy matches the group.");
    }

    #[test]
    fn a_mod_locked_here_is_listed_and_never_repaired() {
        let mut a = addon(1, 11, true);
        a.locked = true;
        let (copy, codes) = group(vec![("a", 5, vec![cf(12, true)])]);
        let v = run(&inst(vec![a]), &copy, &codes, None, &[]);
        let r = only(&v);
        assert_eq!((r.why, r.fixable()), (Why::Locked, false));
        assert!(v.to_fix().is_empty());
        // Turning a locked mod on or off keeps its file, so that is a switch.
        let (copy, codes) = group(vec![("a", 5, vec![cf(12, true)])]);
        let mut a = addon(1, 12, false);
        a.locked = true;
        assert_eq!(only(&run(&inst(vec![a]), &copy, &codes, None, &[])).why, Why::OffHere);
    }

    #[test]
    fn a_mod_from_a_link_nobody_confirmed_is_left_out_until_allowed() {
        let p = pin("cool-1.jar");
        let e = Entry::Source { pin: p.clone(), enabled: true };
        let (copy, codes) = group(vec![("a", 5, vec![e])]);
        // Trusted source: a plain missing mod.
        assert_eq!(only(&run(&inst(vec![]), &copy, &codes, None, &[])).why, Why::Missing);
        // Not confirmed on this PC: left out, with the source to say yes to.
        let v = run(&inst(vec![]), &copy, &codes, None, std::slice::from_ref(&p));
        let r = only(&v);
        assert_eq!((r.why, r.pin.as_ref()), (Why::LeftOut, Some(&p)));
        // Nothing of it is made until the player says yes to that source.
        let (doing, left) = select(v.rows.clone(), &copy, None, &[]);
        assert_eq!((doing.len(), left.len()), (0, 1));
        let (doing, left) = select(v.rows, &copy, None, std::slice::from_ref(&p));
        assert_eq!((doing.len(), left.len()), (1, 0));
    }

    #[test]
    fn a_change_owed_for_its_source_is_listed_as_left_out() {
        // The member's code moved on, but the change is still owed.
        let p = pin("cool-1.jar");
        let e = Entry::Source { pin: p.clone(), enabled: true };
        let key = "s:o/r#cool-#.jar".to_string();
        let (mut copy, codes) = group(vec![("a", 5, vec![])]);
        let now = State::File { file: "src:cool-1.jar".into(), on: true };
        copy.owed.push(groupsync::Owed { key: key.clone(), was: State::Unlisted, now, entry: Some(e), by: "a".into(), name: "A".into(), seq: 4, at: 4, approved: false });
        let v = run(&inst(vec![]), &copy, &codes, None, &[]);
        assert_eq!((only(&v).key.clone(), only(&v).why), (key.clone(), Why::LeftOut));
        // Said yes: it is the one thing to make.
        copy.owed[0].approved = true;
        let v = run(&inst(vec![]), &copy, &codes, None, &[]);
        assert_eq!(only(&v).why, Why::Missing);
        assert_eq!(select(v.rows, &copy, None, &[]).0.len(), 1);
    }

    #[test]
    fn a_pack_mod_of_another_pack_version_is_not_another_file() {
        // Mia is on the pack's next version, whose file of mod 5 differs. That
        // is the pack's update, not this copy lacking anything.
        let mut a = addon(5, 51, true);
        a.from_pack = true;
        let (copy, mut codes) = group(vec![("a", 5, vec![cf(52, true)])]);
        codes.get_mut("a").unwrap().pack = State::File { file: "pack:cf:100:2".into(), on: true };
        let here = State::File { file: "pack:cf:100:1".into(), on: true };
        let i = inst(vec![a]);
        let v = verify(&Facts { inst: &i, copy: &copy, codes: &codes, sent: None, pack: Some(&here), skip: &[], pending: &[] }, 0);
        assert!(v.rows.is_empty(), "{:?}", v.rows);
        // The same file change on the same pack version is a real difference.
        codes.get_mut("a").unwrap().pack = here.clone();
        let v = verify(&Facts { inst: &i, copy: &copy, codes: &codes, sent: None, pack: Some(&here), skip: &[], pending: &[] }, 0);
        assert_eq!(only(&v).why, Why::Different);
    }

    #[test]
    fn only_what_the_player_saw_is_repaired() {
        let (copy, codes) = group(vec![("a", 5, vec![cf(11, true), cf(21, true), cf(31, true)])]);
        let v = run(&inst(vec![]), &copy, &codes, None, &[]);
        assert_eq!(v.to_fix().len(), 3);
        let keys = vec!["p:2".to_string()];
        let (doing, left) = select(v.rows, &copy, Some(&keys), &[]);
        assert_eq!(doing.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), ["p:2"]);
        assert_eq!(left.len(), 2);
    }

    #[test]
    fn the_rows_the_player_keeps_are_never_selected() {
        let mut i = inst(vec![addon(1, 10, true)]);
        i.only_here.insert("p:1".into(), crate::onlyhere::Kept::default());
        let (copy, codes) = group(vec![("a", 5, vec![cf(12, true)])]);
        let (doing, left) = select(run(&i, &copy, &codes, None, &[]).rows, &copy, None, &[]);
        assert_eq!((doing.len(), left.len()), (0, 1));
    }

    #[test]
    fn a_check_that_could_not_finish_does_not_say_it_matches() {
        let v = Verified { unchecked: vec![cf(11, true)], ..Default::default() };
        assert!(!v.matches());
        assert_eq!(v.check(), crate::groupstatus::Check::Couldnt);
        assert_eq!(Verified::default().check(), crate::groupstatus::Check::Matches);
        assert!(Verified::default().matches());
    }

    #[test]
    fn rows_come_fixable_first_then_by_name() {
        let mut i = inst(vec![addon(9, 90, true)]);
        i.only_here.insert("p:9".into(), crate::onlyhere::Kept::default());
        let (copy, codes) = group(vec![("a", 5, vec![cf(92, true), cf(41, true), cf(11, true)])]);
        let v = run(&i, &copy, &codes, None, &[]);
        let keys: Vec<&str> = v.rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["p:1", "p:4", "p:9"]);
    }

    #[test]
    fn what_was_done_reads_as_a_sentence() {
        let row = |n: &str| Row { key: n.into(), name: n.into(), project: 0, why: Why::Missing, older: false, change: None, pin: None, by: String::new(), by_name: String::new(), mark: None };
        assert_eq!(done_says(&[]), "");
        assert_eq!(done_says(&[row("Sodium")]), "Made your copy match the group for Sodium.");
        assert_eq!(done_says(&[row("Sodium"), row("JEI")]), "Made your copy match the group for Sodium and JEI.");
        assert_eq!(done_says(&[row("A"), row("B"), row("C"), row("D")]), "Made your copy match the group for A, B and 2 more.");
        assert_eq!(Why::Missing.action(), "Get it");
        assert_eq!(Why::LeftOut.action(), "Allow and get it");
    }

    #[test]
    fn a_mod_already_waiting_for_browser_download_is_not_called_missing() {
        let (copy, codes) = group(vec![("a", 5, vec![cf(11, true)])]);
        let p = crate::modpack::Pending {
            name: "Entity Culling".into(),
            file_name: "entityculling.jar".into(),
            project_id: 1,
            file_id: 11,
            url: "https://www.curseforge.com/minecraft/mc-mods/entityculling/download/11".into(),
            ..Default::default()
        };
        let i = inst(vec![]);
        // Exact wanted file is in pending: not listed as missing for repair.
        let v = verify(&Facts { inst: &i, copy: &copy, codes: &codes, sent: None, pack: Some(&State::Unlisted), skip: &[], pending: std::slice::from_ref(&p) }, 0);
        assert!(v.rows.is_empty(), "{:?}", v.rows);
        assert!(v.matches());
        // Group moved to a newer file (12) than what's pending (11): still flagged.
        let (copy2, codes2) = group(vec![("a", 6, vec![cf(12, true)])]);
        let v2 = verify(&Facts { inst: &i, copy: &copy2, codes: &codes2, sent: None, pack: Some(&State::Unlisted), skip: &[], pending: std::slice::from_ref(&p) }, 0);
        assert_eq!(only(&v2).why, Why::Missing);
        // Without pending: listed as missing as usual.
        assert_eq!(only(&run(&i, &copy, &codes, None, &[])).why, Why::Missing);
    }
}
