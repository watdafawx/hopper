//! Finding the mod that breaks a pack, by halves. Hopper parks (turns off)
//! part of the suspects, the player launches the game and says whether it
//! is still broken, and the suspects narrow each round.
//!
//! Dependencies stay consistent: a mod is parked only together with every
//! mod that needs it, so no mod that is on ever misses a requirement.
//! Libraries (mods many others need) stay on while the other mods are
//! searched, and are searched last, only if every other mod was cleared.
//!
//! The hunt is a journal in the instance folder ([`FILE`]), written before
//! the first rename: it holds every jar's original on/off state, so
//! stopping always puts the folder back exactly, even after a restart.
//! Only `.disabled` renames, never deletes.

use crate::instances::Instance;
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// The journal, beside `hopper.json`.
pub const FILE: &str = "hopper-bisect.json";

/// A mod this many others need (directly or through each other) counts as
/// a library.
pub const LIBRARY_MIN: usize = 3;

/// What goes wrong: only changes the wording, the player judges each round.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Crash,
    Misbehaves,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    /// Still crashes (or misbehaves) with this round's mods.
    Broken,
    Works,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Searching the mods that aren't libraries.
    #[default]
    Mods,
    /// Every other mod was cleared: searching the libraries.
    Libraries,
    /// Done: `culprits` breaks it.
    Found,
    /// Still broken with every suspect off: not one mod's fault.
    NotFound,
}

/// A mod's part in the hunt, for its row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowState {
    /// On this round and still a suspect.
    Testing,
    /// Off this round.
    Parked,
    /// Known not to be the cause.
    Cleared,
    /// Kept on: many mods need it.
    Library,
    /// The player said it's fine.
    Fine,
    Culprit,
}

/// One answered round, enough to undo it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Round {
    pub parked: Vec<String>,
    pub suspects: Vec<String>,
    pub phase: Phase,
    pub verified: bool,
    pub answer: Option<Answer>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hunt {
    pub kind: Kind,
    pub started: String,
    /// Every jar in `mods/` before round 1: file name (without
    /// `.disabled`) → on.
    pub original: BTreeMap<String, bool>,
    /// What each mod needs: file → the files that provide it.
    pub requires: BTreeMap<String, Vec<String>>,
    pub libraries: Vec<String>,
    /// The cause is one of these.
    pub suspects: Vec<String>,
    /// Suspects the player marked known good.
    pub fine: Vec<String>,
    pub phase: Phase,
    /// A round worked with the suspects off, so the cause is among them
    /// (until then it might be something no round turned off).
    pub verified: bool,
    /// Off for the round being played now.
    pub parked: Vec<String>,
    pub rounds: Vec<Round>,
    pub culprits: Vec<String>,
    /// The mod the crash report blames, only shown as a weak hint: reports
    /// often name the last mod in the chain, not the cause.
    pub hint: Option<String>,
}

type Set = BTreeSet<String>;

impl Hunt {
    /// A hunt over the mods on in `original`, with its first round planned.
    pub fn new(kind: Kind, original: BTreeMap<String, bool>, requires: BTreeMap<String, Vec<String>>, hint: Option<String>) -> Self {
        let on: Set = original.iter().filter(|(_, v)| **v).map(|(k, _)| k.clone()).collect();
        // Only edges between mods that are on count.
        let requires: BTreeMap<String, Vec<String>> = requires
            .into_iter()
            .filter(|(k, _)| on.contains(k))
            .map(|(k, v)| {
                let mut v: Vec<String> = v.into_iter().filter(|d| on.contains(d) && *d != k).collect();
                v.sort();
                v.dedup();
                (k, v)
            })
            .filter(|(_, v)| !v.is_empty())
            .collect();
        let mut h = Hunt {
            kind,
            started: crate::time::now_iso(),
            original,
            requires,
            hint,
            ..Default::default()
        };
        h.libraries = on
            .iter()
            .filter(|m| h.closure([m.as_str()]).len() > LIBRARY_MIN)
            .cloned()
            .collect();
        h.suspects = on.iter().filter(|m| !h.libraries.contains(m)).cloned().collect();
        if h.suspects.is_empty() {
            h.phase = Phase::Libraries;
            h.suspects = h.libraries.clone();
        }
        if h.suspects.is_empty() {
            h.phase = Phase::NotFound;
        }
        h.plan();
        h
    }

    fn on(&self) -> impl Iterator<Item = &String> {
        self.original.iter().filter(|(_, v)| **v).map(|(k, _)| k)
    }

    /// Mods that need `file` directly.
    pub fn needed_by(&self, file: &str) -> Vec<String> {
        self.requires
            .iter()
            .filter(|(_, deps)| deps.iter().any(|d| d == file))
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// `set` and every mod that needs one of them, through each other too.
    pub fn closure<'a>(&self, set: impl IntoIterator<Item = &'a str>) -> Set {
        let mut by: HashMap<&str, Vec<&str>> = HashMap::new();
        for (k, deps) in &self.requires {
            for d in deps {
                by.entry(d.as_str()).or_default().push(k.as_str());
            }
        }
        let mut out: Set = Set::new();
        let mut todo: Vec<&str> = set.into_iter().collect();
        while let Some(m) = todo.pop() {
            if out.insert(m.to_string()) {
                todo.extend(by.get(m).into_iter().flatten().copied());
            }
        }
        out
    }

    /// Picks about half of `s` to park: whole related groups when they
    /// fit, else a mod with what needs it. `None` when it can't be split.
    fn split(&self, s: &Set) -> Option<Set> {
        let target = (s.len() / 2).max(1);
        let cl: BTreeMap<&str, Set> = s
            .iter()
            .map(|m| (m.as_str(), self.closure([m.as_str()]).intersection(s).cloned().collect()))
            .collect();
        // Groups: mods tied by needing each other (union-find).
        let names: Vec<&str> = s.iter().map(String::as_str).collect();
        let ix: HashMap<&str, usize> = names.iter().enumerate().map(|(i, n)| (*n, i)).collect();
        let mut parent: Vec<usize> = (0..names.len()).collect();
        fn find(p: &mut [usize], mut i: usize) -> usize {
            while p[i] != i {
                p[i] = p[p[i]];
                i = p[i];
            }
            i
        }
        for (m, c) in &cl {
            for o in c {
                let (a, b) = (find(&mut parent, ix[m]), find(&mut parent, ix[o.as_str()]));
                parent[a] = b;
            }
        }
        let mut groups: BTreeMap<usize, Vec<&str>> = BTreeMap::new();
        for n in &names {
            let r = find(&mut parent, ix[n]);
            groups.entry(r).or_default().push(n);
        }
        let mut groups: Vec<Vec<&str>> = groups.into_values().collect();
        groups.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
        let mut h = Set::new();
        let mut left = Vec::new();
        for g in groups {
            if h.len() + g.len() <= target {
                h.extend(g.iter().map(|x| x.to_string()));
            } else {
                left.push(g);
            }
        }
        // Too big a group: a mod and its dependents at a time, smallest first.
        'outer: for mut g in left {
            if h.len() >= target {
                break;
            }
            g.sort_by_key(|m| (cl[m].len(), *m));
            for m in g {
                let add: Vec<&String> = cl[m].iter().filter(|x| !h.contains(*x)).collect();
                if add.is_empty() {
                    continue;
                }
                if h.is_empty() || h.len() + add.len() <= target {
                    h.extend(add.into_iter().cloned());
                }
                if h.len() >= target {
                    break 'outer;
                }
            }
        }
        (!h.is_empty() && h.len() < s.len()).then_some(h)
    }

    /// Sets `parked` for the next round, or finishes.
    fn plan(&mut self) {
        let s: Set = self.suspects.iter().cloned().collect();
        match self.phase {
            Phase::NotFound => {
                self.parked.clear();
                return;
            }
            Phase::Found => {
                self.parked = self.closure(self.culprits.iter().map(String::as_str)).into_iter().collect();
                return;
            }
            _ => {}
        }
        let half = if s.len() > 1 { self.split(&s) } else { None };
        match half {
            Some(h) => self.parked = self.closure(h.iter().map(String::as_str)).into_iter().collect(),
            // One mod (or one tied group) left.
            None if self.verified => {
                self.phase = Phase::Found;
                self.culprits = s.into_iter().collect();
                self.plan();
            }
            // Never seen it work: confirm with all of them off.
            None => self.parked = self.closure(s.iter().map(String::as_str)).into_iter().collect(),
        }
    }

    pub fn done(&self) -> bool {
        matches!(self.phase, Phase::Found | Phase::NotFound)
    }

    /// Learns what a round showed the jars' metadata missed: `(file,
    /// needs)` pairs of mods that are on, from the loader refusing to
    /// start. Plans the round again, so it isn't answered with a broken
    /// set of mods. False when it knew every pair already.
    pub fn learn(&mut self, edges: &[(String, String)]) -> bool {
        let on: Set = self.on().cloned().collect();
        let mut new = false;
        for (file, needs) in edges {
            if file == needs || !on.contains(file) || !on.contains(needs) {
                continue;
            }
            let v = self.requires.entry(file.clone()).or_default();
            if !v.contains(needs) {
                v.push(needs.clone());
                v.sort();
                new = true;
            }
        }
        if new && !self.done() {
            self.plan();
        }
        new
    }

    /// Takes the player's answer for the round being played.
    pub fn answer(&mut self, a: Answer) {
        if self.done() {
            return;
        }
        self.rounds.push(Round {
            parked: self.parked.clone(),
            suspects: self.suspects.clone(),
            phase: self.phase,
            verified: self.verified,
            answer: Some(a),
        });
        let p: Set = self.parked.iter().cloned().collect();
        match a {
            Answer::Works => {
                self.suspects.retain(|m| p.contains(m));
                self.verified = true;
            }
            Answer::Broken => self.suspects.retain(|m| !p.contains(m)),
        }
        if self.suspects.is_empty() {
            let libs: Vec<String> = self.libraries.iter().filter(|l| !self.fine.contains(l)).cloned().collect();
            if self.phase == Phase::Mods && !libs.is_empty() {
                self.phase = Phase::Libraries;
                self.suspects = libs;
                self.verified = false;
            } else {
                self.phase = Phase::NotFound;
            }
        }
        self.plan();
    }

    /// Takes back the last answer. False when there is none.
    pub fn undo(&mut self) -> bool {
        let Some(r) = self.rounds.pop() else { return false };
        self.parked = r.parked;
        self.suspects = r.suspects;
        self.phase = r.phase;
        self.verified = r.verified;
        self.culprits.clear();
        // Marked fine since: still fine.
        self.suspects.retain(|m| !self.fine.contains(m));
        if self.suspects.is_empty() {
            self.suspects = self.fine.drain(..).collect();
        }
        self.plan();
        true
    }

    /// The player knows `file` is fine: it stops being a suspect.
    pub fn mark_fine(&mut self, file: &str) -> bool {
        if self.done() || self.suspects.len() < 2 || !self.suspects.iter().any(|m| m == file) {
            return false;
        }
        self.suspects.retain(|m| m != file);
        self.fine.push(file.to_string());
        self.plan();
        true
    }

    /// Makes a mod marked fine a suspect again (always safe: more suspects).
    pub fn unmark_fine(&mut self, file: &str) -> bool {
        if self.done() || !self.fine.iter().any(|m| m == file) {
            return false;
        }
        self.fine.retain(|m| m != file);
        self.suspects.push(file.to_string());
        self.suspects.sort();
        self.plan();
        true
    }

    /// The files that should be off now.
    pub fn wanted_off(&self) -> Set {
        self.original
            .iter()
            .filter(|(_, on)| !**on)
            .map(|(k, _)| k.clone())
            .chain(self.parked.iter().cloned())
            .collect()
    }

    /// A mod's part in the hunt; `None` for one it doesn't touch.
    pub fn state(&self, file: &str) -> Option<RowState> {
        if self.original.get(file) != Some(&true) {
            return None;
        }
        let has = |v: &[String]| v.iter().any(|m| m == file);
        Some(if has(&self.culprits) {
            RowState::Culprit
        } else if has(&self.parked) {
            RowState::Parked
        } else if self.phase == Phase::Found {
            return None;
        } else if has(&self.fine) {
            RowState::Fine
        } else if has(&self.suspects) {
            RowState::Testing
        } else if self.phase == Phase::Mods && has(&self.libraries) {
            RowState::Library
        } else {
            RowState::Cleared
        })
    }

    /// This round's number, from 1.
    pub fn round(&self) -> usize {
        self.rounds.len() + 1
    }

    /// About how many rounds the hunt takes in all.
    pub fn rounds_estimate(&self) -> usize {
        let n = self.suspects.len().max(1);
        let halvings = usize::BITS - (n - 1).leading_zeros();
        let confirm = usize::from(!self.verified && n > 1);
        self.rounds.len() + (halvings as usize).max(1) + confirm
    }

    /// How many on mods, parked mods and suspects there are now.
    pub fn counts(&self) -> (usize, usize, usize) {
        (self.on().count(), self.parked.len(), self.suspects.len())
    }
}

pub fn path(root: &Path) -> PathBuf {
    root.join(FILE)
}

/// The instance's hunt, if one is running.
pub fn load(root: &Path) -> Option<Hunt> {
    serde_json::from_str(&std::fs::read_to_string(path(root)).ok()?).ok()
}

/// Writes the journal (a temp file, then a rename).
pub fn save(root: &Path, h: &Hunt) -> anyhow::Result<()> {
    let tmp = root.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(h)?).context("writing the hunt journal")?;
    std::fs::rename(&tmp, path(root)).context("saving the hunt journal")
}

pub fn clear(root: &Path) {
    let _ = std::fs::remove_file(path(root));
}

/// Every jar in `mods/`: file name (without `.disabled`) → on.
pub fn mod_files(mods_dir: &Path) -> BTreeMap<String, bool> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(mods_dir).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        let l = n.to_lowercase();
        if l.ends_with(".jar") {
            out.insert(n, true);
        } else if l.ends_with(".jar.disabled") {
            out.entry(n[..n.len() - ".disabled".len()].to_string()).or_insert(false);
        }
    }
    out
}

/// What each enabled jar needs, from the jars' own metadata (what the
/// loader checks). Removes jars by bits of their index, so every pair is
/// seen apart in a few passes instead of one per jar.
pub fn scan_requires(mods_dir: &Path, on: &[String]) -> BTreeMap<String, Vec<String>> {
    let mut providers: HashMap<String, Vec<&String>> = HashMap::new();
    for f in on {
        for id in crate::deps::provides(&mods_dir.join(f)) {
            providers.entry(id).or_default().push(f);
        }
    }
    let real = |m: &crate::deps::Missing| m.wrong_version.is_none();
    let before: BTreeSet<(String, String)> = crate::deps::missing(mods_dir)
        .into_iter()
        .filter(real)
        .map(|m| (m.file_name, m.needs))
        .collect();
    let bits = (usize::BITS - on.len().saturating_sub(1).leading_zeros()).max(1);
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for b in 0..bits {
        for side in [0, 1] {
            let without: Vec<String> = on
                .iter()
                .enumerate()
                .filter(|(i, _)| (i >> b) & 1 == side)
                .map(|(_, f)| f.clone())
                .collect();
            if without.is_empty() {
                continue;
            }
            for m in crate::deps::missing_without(mods_dir, &without) {
                if !real(&m) || before.contains(&(m.file_name.clone(), m.needs.clone())) {
                    continue;
                }
                // Every removed jar that provides it (two that both do are
                // both kept with it: parking both would break it).
                for p in providers.get(&m.needs).into_iter().flatten() {
                    if without.contains(p) && **p != m.file_name {
                        out.entry(m.file_name.clone()).or_default().insert((*p).clone());
                    }
                }
            }
        }
    }
    out.into_iter().map(|(k, v)| (k, v.into_iter().collect())).collect()
}

/// Turns one jar on or off by renaming it, and its tracked mod's flag.
/// False when it was already so or isn't there.
fn set_file(inst: &mut Instance, file: &str, on: bool) -> anyhow::Result<bool> {
    let dir = inst.mods_dir();
    let (en, dis) = (dir.join(file), dir.join(format!("{file}.disabled")));
    let (from, to) = if on { (&dis, &en) } else { (&en, &dis) };
    let moved = if from.is_file() && !to.exists() {
        std::fs::rename(from, to)
            .map_err(anyhow::Error::from)
            .map_err(crate::install::explain_locked)
            .with_context(|| format!("renaming {file}"))?;
        true
    } else {
        false
    };
    for a in inst.addons.iter_mut().filter(|a| a.folder() == "mods" && a.file_name.eq_ignore_ascii_case(file)) {
        if to.is_file() {
            a.enabled = on;
        }
    }
    Ok(moved)
}

/// Sets every jar the hunt knows to `want(file)`, then saves the instance
/// once. Tries every jar even when one fails. Returns how many changed.
fn set_files(inst: &mut Instance, files: Vec<(String, bool)>) -> anyhow::Result<usize> {
    let mut changed = 0;
    let mut failed = Vec::new();
    for (f, on) in files {
        match set_file(inst, &f, on) {
            Ok(true) => changed += 1,
            Ok(false) => {}
            Err(e) => failed.push(format!("{e:#}")),
        }
    }
    inst.save()?;
    match failed.first() {
        None => Ok(changed),
        Some(first) => anyhow::bail!("{} files didn't change: {first}", failed.len()),
    }
}

/// What the loader's refusal in a round's log ("Mod X requires Y …")
/// shows the metadata missed: `(file, needs)` pairs where X is in a jar
/// that's on and Y only in jars parked this round. What [`Hunt::learn`]
/// takes; empty when the round failed for another reason.
pub fn needs_from_log(mods_dir: &Path, h: &Hunt, lines: &[String]) -> Vec<(String, String)> {
    let mut asked = BTreeSet::new();
    for l in lines {
        let w: Vec<&str> = l.split_whitespace().collect();
        for i in 0..w.len().saturating_sub(3) {
            if w[i] == "Mod" && w[i + 2] == "requires" {
                asked.insert((w[i + 1].to_lowercase(), w[i + 3].to_lowercase()));
            }
        }
    }
    if asked.is_empty() {
        return Vec::new();
    }
    let off = h.wanted_off();
    let path = |f: &str| {
        let p = mods_dir.join(f);
        if p.is_file() { p } else { mods_dir.join(format!("{f}.disabled")) }
    };
    let mut ids: HashMap<String, Vec<&String>> = HashMap::new();
    for f in h.original.keys() {
        for id in crate::deps::provides(&path(f)) {
            ids.entry(id.to_lowercase()).or_default().push(f);
        }
    }
    let mut out = BTreeSet::new();
    for (x, y) in asked {
        let (Some(xf), Some(yf)) = (ids.get(&x), ids.get(&y)) else { continue };
        // Something on provides it: not what was missing.
        if yf.iter().any(|f| !off.contains(*f)) {
            continue;
        }
        for a in xf.iter().filter(|f| !off.contains(**f)) {
            for b in yf {
                out.insert(((*a).clone(), (*b).clone()));
            }
        }
    }
    out.into_iter().collect()
}

/// Puts the folder in this round's state.
pub fn apply(inst: &mut Instance, h: &Hunt) -> anyhow::Result<usize> {
    let off = h.wanted_off();
    let files = h.original.keys().map(|f| (f.clone(), !off.contains(f))).collect();
    set_files(inst, files)
}

/// Starts a hunt: reads the folder, saves the journal, then parks round
/// 1's mods.
pub fn begin(inst: &mut Instance, kind: Kind, hint: Option<String>) -> anyhow::Result<Hunt> {
    anyhow::ensure!(load(&inst.root).is_none(), "a hunt is already running in this instance");
    let dir = inst.mods_dir();
    let original = mod_files(&dir);
    let on: Vec<String> = original.iter().filter(|(_, v)| **v).map(|(k, _)| k.clone()).collect();
    anyhow::ensure!(on.len() >= 2, "fewer than two mods are on");
    let h = Hunt::new(kind, original, scan_requires(&dir, &on), hint);
    save(&inst.root, &h)?;
    apply(inst, &h)?;
    Ok(h)
}

/// Ends the hunt: every jar as it was before round 1, except `keep_off`
/// (the culprits and what needs them) when given. The journal goes only
/// once that worked, so a failed restore can be tried again.
pub fn finish(inst: &mut Instance, keep_off: bool) -> anyhow::Result<usize> {
    let h = load(&inst.root).context("no hunt is running")?;
    let off: Set = if keep_off && h.phase == Phase::Found {
        h.closure(h.culprits.iter().map(String::as_str))
    } else {
        Set::new()
    };
    let files = h.original.iter().map(|(f, on)| (f.clone(), *on && !off.contains(f))).collect();
    let n = set_files(inst, files)?;
    clear(&inst.root);
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn orig(names: &[&str]) -> BTreeMap<String, bool> {
        names.iter().map(|n| (n.to_string(), true)).collect()
    }

    fn reqs(edges: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        edges.iter().map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect())).collect()
    }

    /// No mod that's on needs one that's off.
    fn consistent(h: &Hunt) {
        let off = h.wanted_off();
        for (m, deps) in &h.requires {
            if !off.contains(m) {
                for d in deps {
                    assert!(!off.contains(d), "{m} is on but needs {d}, which is parked");
                }
            }
        }
    }

    /// Plays a hunt where `broken(on mods)` says whether the game breaks.
    fn play(mut h: Hunt, broken: impl Fn(&Set) -> bool) -> Hunt {
        for _ in 0..64 {
            if h.done() {
                return h;
            }
            consistent(&h);
            let off = h.wanted_off();
            let on: Set = h.original.keys().filter(|k| !off.contains(*k)).cloned().collect();
            h.answer(if broken(&on) { Answer::Broken } else { Answer::Works });
        }
        panic!("the hunt didn't end");
    }

    fn pack(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("mod{i:03}.jar")).collect()
    }

    #[test]
    fn finds_any_single_culprit_in_log_rounds() {
        let names = pack(100);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        for c in &names {
            let h = Hunt::new(Kind::Crash, orig(&refs), BTreeMap::new(), None);
            let h = play(h, |on| on.contains(c));
            assert_eq!(h.phase, Phase::Found);
            assert_eq!(h.culprits, vec![c.clone()]);
            assert!(h.rounds.len() <= 9, "{} rounds for {c}", h.rounds.len());
        }
    }

    #[test]
    fn keeps_dependencies_consistent_and_finds_a_dependent() {
        // b and c need a; d needs c; e..j stand alone.
        let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"];
        let r = reqs(&[("b", &["a"]), ("c", &["a"]), ("d", &["c"])]);
        for c in names {
            let h = Hunt::new(Kind::Crash, orig(&names), r.clone(), None);
            // a has 3 mods needing it: a library, kept on.
            assert_eq!(h.libraries, vec!["a".to_string()]);
            let h = play(h, |on| on.contains(c));
            assert_eq!(h.culprits, vec![c.to_string()], "culprit {c}");
            consistent(&h);
        }
    }

    #[test]
    fn a_library_culprit_is_found_after_the_other_mods() {
        let names = ["lib", "a", "b", "c", "x", "y", "z"];
        let r = reqs(&[("a", &["lib"]), ("b", &["lib"]), ("c", &["lib"])]);
        let h = Hunt::new(Kind::Crash, orig(&names), r, None);
        let h = play(h, |on| on.contains("lib"));
        assert_eq!(h.phase, Phase::Found);
        assert_eq!(h.culprits, vec!["lib".to_string()]);
        // Parking it parks what needs it.
        assert!(["a", "b", "c", "lib"].iter().all(|m| h.parked.contains(&m.to_string())));
    }

    #[test]
    fn broken_with_everything_off_finds_nothing() {
        let names = pack(12);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let h = Hunt::new(Kind::Misbehaves, orig(&refs), BTreeMap::new(), None);
        let h = play(h, |_| true);
        assert_eq!(h.phase, Phase::NotFound);
        assert!(h.parked.is_empty());
    }

    #[test]
    fn two_mods_together_names_one_of_them() {
        let names = pack(32);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let h = Hunt::new(Kind::Crash, orig(&refs), BTreeMap::new(), None);
        let (a, b) = (names[3].clone(), names[20].clone());
        let h = play(h, |on| on.contains(&a) && on.contains(&b));
        assert_eq!(h.phase, Phase::Found);
        assert!(h.culprits == vec![a.clone()] || h.culprits == vec![b.clone()]);
    }

    #[test]
    fn mods_off_before_stay_off_and_out_of_the_hunt() {
        let mut o = orig(&["a", "b", "c", "d"]);
        o.insert("old".into(), false);
        let h = Hunt::new(Kind::Crash, o, BTreeMap::new(), None);
        assert!(h.wanted_off().contains("old"));
        assert!(!h.suspects.contains(&"old".to_string()));
        assert_eq!(h.state("old"), None);
    }

    #[test]
    fn undo_and_marking_fine() {
        let names = pack(16);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut h = Hunt::new(Kind::Crash, orig(&refs), BTreeMap::new(), None);
        let first = h.clone();
        h.answer(Answer::Broken);
        assert!(h.undo());
        assert_eq!(h.parked, first.parked);
        assert_eq!(h.suspects, first.suspects);
        assert!(!h.undo());
        let m = h.suspects[0].clone();
        assert!(h.mark_fine(&m));
        assert_eq!(h.state(&m), Some(RowState::Fine));
        assert!(!h.parked.contains(&m));
        assert!(h.unmark_fine(&m));
        assert!(h.suspects.contains(&m));
    }

    #[test]
    fn a_tied_group_is_one_culprit() {
        // a and b need each other: always parked together.
        let names = ["a", "b", "c", "d"];
        let r = reqs(&[("a", &["b"]), ("b", &["a"])]);
        let h = Hunt::new(Kind::Crash, orig(&names), r, None);
        let h = play(h, |on| on.contains("a"));
        assert_eq!(h.culprits, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn reads_requirements_from_the_jars() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("hopper-bisect-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let jar = |file: &str, id: &str, needs: &[&str]| {
            let mut t = format!("modLoader=\"javafml\"\n[[mods]]\nmodId=\"{id}\"\nversion=\"1.0\"\n");
            for n in needs {
                t += &format!("[[dependencies.{id}]]\nmodId=\"{n}\"\ntype=\"required\"\nversionRange=\"[1,)\"\nside=\"BOTH\"\n");
            }
            let mut z = zip::ZipWriter::new(std::fs::File::create(dir.join(file)).unwrap());
            z.start_file::<_, ()>("META-INF/neoforge.mods.toml", Default::default()).unwrap();
            z.write_all(t.as_bytes()).unwrap();
            z.finish().unwrap();
        };
        jar("lib.jar", "lib", &[]);
        jar("a.jar", "a", &["lib"]);
        jar("b.jar", "b", &["lib", "a"]);
        jar("c.jar", "c", &[]);
        jar("d.jar", "d", &["gone"]);
        let on: Vec<String> = mod_files(&dir).into_keys().collect();
        let r = scan_requires(&dir, &on);
        assert_eq!(r.get("a.jar"), Some(&vec!["lib.jar".to_string()]));
        assert_eq!(r.get("b.jar"), Some(&vec!["a.jar".to_string(), "lib.jar".to_string()]));
        // Missing before the hunt: no edge.
        assert_eq!(r.get("d.jar"), None);
        assert_eq!(r.get("c.jar"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn journal_and_renames_restore_exactly() {
        let root = std::env::temp_dir().join(format!("hopper-bisect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mods = root.join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        for n in ["a.jar", "b.jar", "c.jar", "d.jar", "e.jar"] {
            std::fs::write(mods.join(n), "").unwrap();
        }
        std::fs::write(mods.join("off.jar.disabled"), "").unwrap();
        let before = mod_files(&mods);
        let mut inst = Instance { root: root.clone(), name: "t".into(), ..Default::default() };
        let mut h = begin(&mut inst, Kind::Crash, Some("e".into())).unwrap();
        assert_eq!(load(&root).as_ref(), Some(&h));
        assert_eq!(h.original, before);
        // The folder matches the round.
        let now = mod_files(&mods);
        for (f, on) in &now {
            assert_eq!(*on, !h.wanted_off().contains(f), "{f}");
        }
        assert!(now.values().filter(|v| !**v).count() > 1);
        h.answer(Answer::Works);
        save(&root, &h).unwrap();
        apply(&mut inst, &h).unwrap();
        // Stopping puts every jar back.
        finish(&mut inst, false).unwrap();
        assert_eq!(mod_files(&mods), before);
        assert!(load(&root).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn learns_what_the_metadata_missed() {
        let names = ["a", "b", "c", "d", "e", "f"];
        let mut h = Hunt::new(Kind::Crash, orig(&names), BTreeMap::new(), None);
        // Something on this round needs something parked.
        let off = h.wanted_off();
        let on: Vec<&str> = names.iter().copied().filter(|n| !off.contains(*n)).collect();
        let parked = off.iter().next().unwrap().clone();
        let edge = (on[0].to_string(), parked.clone());
        assert!(h.learn(std::slice::from_ref(&edge)));
        consistent(&h);
        assert!(!h.learn(&[edge]), "known already");
        // Only mods that were on count.
        assert!(!h.learn(&[("zz".into(), "a".into())]));
        // Still finds the culprit with the new edge.
        let h = play(h, |on| on.contains("e"));
        assert_eq!(h.phase, Phase::Found);
        assert!(h.culprits.iter().any(|c| c == "e"), "{:?}", h.culprits);
    }
}
