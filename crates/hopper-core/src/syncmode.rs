//! Two choices a player makes on this PC about how friends sync works
//! (`design/group-sync-modes.md`). Neither changes what the relay keeps or
//! sends: both decide only what this PC does with it.
//!
//! - **Take changes from** (per group): everyone, as before, or only the
//!   person who made the group. Then another member's code that arrives is
//!   kept waiting ([`parked`]) instead of being taken in, until the player
//!   presses "Pull from Mia", which takes in that member's code and nothing
//!   else ([`for_merge`]). What the maker took in from others comes with the
//!   maker's code: their copy is the one this PC follows.
//! - **Share my changes** (per PC): right away, as before, or when the
//!   player presses Push. A pack whose changes are held back simply isn't
//!   published; what's held is the difference between the pack as it is and
//!   the last code that went out ([`pending`]), so it lives in the instance
//!   and the settings already, survives a restart and can't get lost.
//!   Receiving goes on as before: a change another member made to a mod this
//!   PC changed and hasn't sent stays held (`groupsync::Verdict::Held`) the
//!   same way it waits for a publish today.

use crate::groupsync::{self, Got, Mark, Mods, PackCopy};
use std::collections::{BTreeMap, HashSet};

/// Whether this PC takes `from`'s changes in by itself. `only`: the id of
/// the group's maker when the group is set to take changes only from them.
pub fn takes(only: Option<&str>, from: &str) -> bool {
    only.is_none_or(|o| o == from)
}

/// The mods a member's waiting code `g` changed since the last code this
/// PC took in from them, as their stamps say (`groupsync::plan` reads them
/// the same way): keys, each once.
pub fn code_changes<'a>(copy: &PackCopy, from: &str, g: &'a Got) -> Vec<&'a str> {
    let had = copy.got.get(from).map_or(0, |p| p.seq);
    g.stamps
        .iter()
        .filter(|(_, s)| (s.seq == 0 && s.by == from) || (s.seq > had && s.seq <= g.seq))
        .map(|(k, _)| k.as_str())
        .collect()
}

/// A member whose changes wait for the player to pull them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parked {
    /// Their mailbox id, and the name to show.
    pub id: String,
    pub name: String,
    /// About how many mods their waiting code changes (0: an older Hopper
    /// that sends no stamps, so it can't tell).
    pub changes: usize,
}

/// The members whose waiting codes this PC doesn't take in by itself,
/// oldest first. Empty when the group takes changes from everyone.
pub fn parked(copy: &PackCopy, only: Option<&str>) -> Vec<Parked> {
    if only.is_none() {
        return Vec::new();
    }
    copy.queue()
        .into_iter()
        .filter(|(from, _)| !takes(only, from))
        .map(|(from, g)| {
            let changes = code_changes(copy, &from, &g).len();
            let name = if g.name.is_empty() { "A friend".to_string() } else { g.name.clone() };
            Parked { id: from, name, changes }
        })
        .collect()
}

/// The copy as a merge should see it: only the waiting codes of the
/// members this PC takes from, plus `pull` (the members the player just
/// pulled from). The others stay waiting in the stored copy, since a merge
/// only clears what it took in.
pub fn for_merge(copy: &PackCopy, only: Option<&str>, pull: &[String]) -> PackCopy {
    let mut c = copy.clone();
    c.waiting.retain(|from, _| takes(only, from) || pull.contains(from));
    c
}

/// The changes made on this PC since `sent` (the last code that went out)
/// to `now`, leaving out what other members' changes made (`marks` names
/// them): what a Push sends that the others don't have.
pub fn own_changes(sent: &Mods, now: &Mods, marks: &BTreeMap<String, Mark>) -> usize {
    let keys: HashSet<String> = groupsync::changes(sent, now)
        .into_iter()
        .filter(|c| marks.get(&c.key).is_none_or(|m| m.state != c.now))
        .map(|c| c.key)
        .collect();
    keys.len()
}

/// What a pack held back from going out has that its last code didn't.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pending {
    /// Changes made here: mods, the loader, the pack version.
    pub changes: usize,
    /// Anything to push at all. True with no count when there is no code
    /// to compare with (sent before this was kept).
    pub any: bool,
}

/// How a pack (`inst`) differs from the last code that went out
/// (`sent`; None when this PC has none), leaving out what other members
/// made (`marks`, for a group copy). Runs off the UI thread: making the
/// code may read the pack's file list.
pub fn pending(inst: &crate::instances::Instance, sent: Option<&str>, marks: &BTreeMap<String, Mark>, job: &crate::jobs::Job) -> anyhow::Result<Pending> {
    let made = crate::sharecode::make(inst, job)?;
    let Some(sent) = sent.filter(|s| !s.is_empty()) else {
        return Ok(Pending { changes: 0, any: true });
    };
    if made.code == sent {
        return Ok(Pending::default());
    }
    let prev = crate::sharecode::decode(sent)?;
    let found = groupsync::projects_of(inst, &[&prev, &made.share]);
    let (mut prev, mut now) = (groupsync::mods(&prev, groupsync::lookup(&found)), groupsync::mods(&made.share, groupsync::lookup(&found)));
    if prev.pack != now.pack {
        groupsync::fill_packs(inst, &mut [&mut prev, &mut now], job);
    }
    let changes = own_changes(&prev, &now, marks);
    Ok(Pending { changes, any: changes > 0 })
}

/// "Push 3 changes", "Push 1 change", or "Push changes" with no count.
pub fn push_label(changes: usize) -> String {
    match changes {
        0 => "Push changes".to_string(),
        1 => "Push 1 change".to_string(),
        n => format!("Push {n} changes"),
    }
}

/// "3 changes", "1 change".
pub fn changes_words(n: usize) -> String {
    if n == 1 { "1 change".to_string() } else { format!("{n} changes") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::groupsync::{Got, Kind, State, Stamp};
    use crate::sharecode::Entry;

    fn got(seq: u64, name: &str, stamps: &[(&str, u64, &str)]) -> Got {
        let stamps = stamps.iter().map(|(k, s, by)| (k.to_string(), Stamp { seq: *s, by: by.to_string() })).collect();
        Got { code: format!("code-{name}-{seq}"), seq, at: 0, name: name.into(), stamps }
    }

    /// A copy of Noah's group pack with codes waiting from Noah (the
    /// maker), Mia and Ava.
    fn copy() -> PackCopy {
        let mut c = PackCopy { inst: "pack".into(), group: "crew".into(), local: "local".into(), ..Default::default() };
        c.got.insert("mia".into(), got(5, "Mia", &[]));
        c.arrive("noah", got(10, "Noah", &[("p:1", 0, "noah")]));
        c.arrive("mia", got(12, "Mia", &[("p:2", 0, "mia"), ("p:3", 11, "mia"), ("p:4", 3, "mia")]));
        c.arrive("ava", got(14, "Ava", &[("p:5", 0, "ava")]));
        c
    }

    #[test]
    fn everyone_takes_every_code_and_parks_none() {
        let c = copy();
        assert!(parked(&c, None).is_empty());
        assert_eq!(for_merge(&c, None, &[]).waiting.len(), 3);
    }

    #[test]
    fn only_the_maker_parks_the_others_with_their_counts() {
        let c = copy();
        let p = parked(&c, Some("noah"));
        // Mia's p:4 is older than the code taken from her before: not new.
        assert_eq!(
            p,
            vec![Parked { id: "mia".into(), name: "Mia".into(), changes: 2 }, Parked { id: "ava".into(), name: "Ava".into(), changes: 1 }]
        );
        let m = for_merge(&c, Some("noah"), &[]);
        assert_eq!(m.waiting.keys().collect::<Vec<_>>(), vec!["noah"]);
    }

    #[test]
    fn pulling_from_one_person_takes_only_theirs_and_the_makers() {
        let c = copy();
        let m = for_merge(&c, Some("noah"), &["mia".into()]);
        let mut keys: Vec<&String> = m.waiting.keys().collect();
        keys.sort();
        assert_eq!(keys, vec!["mia", "noah"]);
        // The stored copy keeps Ava's code waiting.
        assert_eq!(c.waiting.len(), 3);
        assert!(m.waiting.get("ava").is_none());
    }

    #[test]
    fn a_parked_code_stays_parked_when_a_newer_one_comes() {
        let mut c = copy();
        c.arrive("mia", got(20, "Mia", &[("p:2", 0, "mia"), ("p:3", 11, "mia"), ("p:6", 0, "mia")]));
        let p = parked(&c, Some("noah"));
        let mia = p.iter().find(|p| p.id == "mia").unwrap();
        assert_eq!(mia.changes, 3);
        assert_eq!(c.waiting["mia"].seq, 20);
    }

    /// A merge of only what it was given leaves the parked codes waiting:
    /// the store clears just what was taken (by number).
    #[test]
    fn taking_in_the_makers_code_leaves_the_others_waiting() {
        let mut c = copy();
        let m = for_merge(&c, Some("noah"), &[]);
        for (from, g) in m.queue() {
            if c.waiting.get(&from).is_some_and(|w| w.seq == g.seq) {
                c.waiting.remove(&from);
            }
            c.got.insert(from, g);
        }
        assert!(c.waiting.contains_key("mia") && c.waiting.contains_key("ava"));
        assert_eq!(parked(&c, Some("noah")).len(), 2);
    }

    fn mods(entries: &[(&str, &str, bool)]) -> Mods {
        let mut map = BTreeMap::new();
        for (k, f, on) in entries {
            let state = State::File { file: f.to_string(), on: *on };
            map.insert(k.to_string(), (state, Entry::CurseFile { file: 1, enabled: *on }));
        }
        Mods { map, unsure: false, loader: None, pack: State::Unlisted, ships: None, names: BTreeMap::new(), unknown: Vec::new() }
    }

    #[test]
    fn own_changes_leave_out_what_others_made() {
        let sent = mods(&[("p:1", "cf:1", true), ("p:2", "cf:2", true)]);
        // Here: p:1 turned off (mine), p:2 updated by Mia (taken in), p:3
        // added (mine).
        let now = mods(&[("p:1", "cf:1", false), ("p:2", "cf:22", true), ("p:3", "cf:3", true)]);
        let mut marks = BTreeMap::new();
        marks.insert(
            "p:2".to_string(),
            Mark { state: State::File { file: "cf:22".into(), on: true }, kind: Kind::Updated, by: "mia".into(), name: "Mia".into(), seq: 9, at: 0 },
        );
        assert_eq!(own_changes(&sent, &now, &marks), 2);
        assert_eq!(own_changes(&sent, &sent, &marks), 0);
    }

    fn inst_with(tag: &str, mods: &[(u64, bool)]) -> crate::instances::Instance {
        use crate::instances::{Addon, Instance};
        let root = std::env::temp_dir().join(format!("hopper-syncmode-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("mods")).unwrap();
        let addons = mods
            .iter()
            .map(|&(p, enabled)| Addon { project_id: p, file_id: p * 10, file_name: format!("m{p}.jar"), name: format!("M{p}"), enabled, ..Default::default() })
            .collect();
        let inst = Instance { root, name: "T".into(), game_version: "1.21.1".into(), loader: "neoforge-21.1.200".into(), addons, ..Default::default() };
        inst.save().unwrap();
        inst
    }

    /// What a Push would send is the pack on disk against the last code
    /// that went out, both saved: after a restart (the instance read
    /// again, the code from the settings file) it is the same, so nothing
    /// held back is lost.
    #[test]
    fn held_back_changes_are_found_again_after_a_restart() {
        let job = crate::jobs::Jobs::default().start("t");
        let inst = inst_with("restart", &[(5, true), (6, true)]);
        let sent = crate::sharecode::make(&inst, &job).unwrap().code;
        let none = BTreeMap::new();
        assert_eq!(pending(&inst, Some(&sent), &none, &job).unwrap(), Pending::default(), "nothing since it went out");
        // Changed here, not pushed: mod 6 off, mod 7 added.
        let mut now = inst.clone();
        now.addons[1].enabled = false;
        now.addons.push(crate::instances::Addon { project_id: 7, file_id: 70, file_name: "m7.jar".into(), name: "M7".into(), enabled: true, ..Default::default() });
        now.save().unwrap();
        assert_eq!(pending(&now, Some(&sent), &none, &job).unwrap(), Pending { changes: 2, any: true });
        // Restart: the settings file and the instance are read back.
        let mut st = crate::settings::Settings { push_by_hand: true, ..Default::default() };
        st.pushed_codes.insert("pack".into(), sent.clone());
        let st: crate::settings::Settings = serde_json::from_str(&serde_json::to_string(&st).unwrap()).unwrap();
        let back = crate::instances::load(&inst.root).unwrap();
        assert_eq!(pending(&back, st.pushed_codes.get("pack").map(String::as_str), &none, &job).unwrap(), Pending { changes: 2, any: true });
        // Mod 7 came from Mia (taken in): only the change made here counts.
        let mut marks = BTreeMap::new();
        marks.insert("p:7".to_string(), Mark { state: State::File { file: "cf:70".into(), on: true }, kind: Kind::Added, by: "mia".into(), name: "Mia".into(), seq: 4, at: 0 });
        assert_eq!(pending(&back, Some(&sent), &marks, &job).unwrap(), Pending { changes: 1, any: true });
        // Never sent from this PC: something to push, count unknown.
        assert_eq!(pending(&back, None, &none, &job).unwrap(), Pending { changes: 0, any: true });
        let _ = std::fs::remove_dir_all(&inst.root);
    }

    #[test]
    fn push_words_read_plainly() {
        assert_eq!(push_label(0), "Push changes");
        assert_eq!(push_label(1), "Push 1 change");
        assert_eq!(push_label(3), "Push 3 changes");
        assert_eq!(changes_words(1), "1 change");
    }
}
