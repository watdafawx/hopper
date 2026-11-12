//! "Only on this PC": mods a player keeps out of friends sync (a minimap,
//! shaders, a performance mod).
//!
//! A kept mod is never published the way it is here. The code carries it
//! the way friends last had it ([`Kept::theirs`]), so marking a mod doesn't
//! read as "removed" on their side, and changing it here sends nothing.
//! Changes from friends (a followed pack, a group copy, the pack update a
//! follow brings) leave it alone; they only update `theirs`. That way a new
//! follower still gets the group's version, and this copy's next publish
//! matches what the others already have, so nothing echoes back.
//! "Sync again" puts the mod back the way friends have it; it never sends
//! this PC's version over theirs.
//!
//! The marks live in the instance's `hopper.json` and never go in a code.

use crate::groupsync::{self, Change, Merged, Mods, State};
use crate::instances::{Addon, Instance};
use crate::jobs::Job;
use crate::sharecode::{Entry, Share};
use serde::{Deserialize, Serialize};

/// One kept mod.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Kept {
    /// The mod as friends last had it: its entry in their code, or `None`
    /// when the code doesn't list it (a pack mod as the pack has it, or a
    /// mod they don't have).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theirs: Option<Entry>,
    /// The mod's name when it was marked, to list it after it's removed
    /// here.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
}

/// Whether this mod is kept only on this PC.
pub fn is_kept(inst: &Instance, a: &Addon) -> bool {
    inst.only_here.contains_key(&groupsync::addon_key(a))
}

/// The entries a code carries for the kept mods, in place of how they are
/// here.
pub fn theirs(inst: &Instance) -> impl Iterator<Item = Entry> + '_ {
    inst.only_here.values().filter_map(|k| k.theirs.clone())
}

/// Marks `keys` ([`groupsync::addon_key`]) as kept only on this PC, each
/// remembered the way friends last had it. `sent`: the code this PC last
/// published, when known; otherwise the instance as it is now, which is
/// what was published while it's in step. Runs off the UI thread (reading
/// the instance as a code may look its pack up).
pub fn mark(inst: &mut Instance, keys: &[String], sent: Option<&Share>, job: &Job) -> anyhow::Result<()> {
    if keys.iter().all(|k| inst.only_here.contains_key(k)) {
        return Ok(());
    }
    let now;
    let share = match sent {
        Some(s) => s,
        None => {
            now = crate::sharecode::make(inst, job)?.share;
            &now
        }
    };
    let found = groupsync::projects_of(inst, &[share]);
    let mods = groupsync::mods(share, groupsync::lookup(&found));
    mark_from(inst, keys, &mods);
    inst.save()
}

/// [`mark`], given the published code's mods.
pub fn mark_from(inst: &mut Instance, keys: &[String], published: &Mods) {
    for k in keys {
        if inst.only_here.contains_key(k) {
            continue;
        }
        let theirs = published.map.get(k).map(|(_, e)| e.clone());
        let name = inst.addons.iter().find(|a| groupsync::addon_key(a) == *k).map(|a| a.name.clone()).unwrap_or_default();
        inst.only_here.insert(k.clone(), Kept { theirs, name });
    }
}

/// [`mark`] without reading the pack: each mod is remembered as it is here
/// now, before the change the player is about to make. That is what this
/// PC published while it's in step with friends, and it needs no network,
/// so the mark and the change land together on the UI thread. A pack mod
/// is taken to be the pack's file. Not saved.
pub fn mark_here(inst: &mut Instance, keys: &[String]) {
    let in_pack = inst.pack.as_ref().is_some_and(|p| p.project_id != 0);
    for k in keys {
        if inst.only_here.contains_key(k) {
            continue;
        }
        let Some(a) = inst.addons.iter().find(|a| groupsync::addon_key(a) == *k) else { continue };
        let theirs = if in_pack && a.from_pack {
            let t = if a.project_id != 0 && !crate::modrinth::is_mr(a.project_id) && !crate::sources::is_src(a.project_id) {
                crate::sharecode::Target::Curse(a.project_id)
            } else {
                crate::sharecode::Target::Named(a.file_name.clone())
            };
            (!a.enabled).then_some(Entry::Off(t))
        } else if let Some(s) = &a.source {
            let mut source = s.clone();
            source.prev_project = 0;
            source.prev_mr_project.clear();
            Some(Entry::Source { pin: crate::sources::carry::Pin { file: a.file_name.clone(), source }, enabled: a.enabled })
        } else {
            crate::sharecode::file_entry(a)
        };
        let name = a.name.clone();
        inst.only_here.insert(k.clone(), Kept { theirs, name });
    }
}

/// What "Sync again" does to a kept mod here, for the confirm ("turns
/// on"). `None`: nothing changes.
pub fn sync_again_does(a: Option<&Addon>, theirs: Option<&Entry>) -> Option<&'static str> {
    match (a, theirs) {
        (None, None) | (None, Some(Entry::Off(_) | Entry::Remove(_))) => None,
        (None, Some(_)) => Some("is added"),
        (Some(a), None) if !a.from_pack => Some("is removed"),
        (Some(a), None) => (!a.enabled).then_some("turns on"),
        (Some(_), Some(Entry::Remove(_))) => Some("is removed"),
        (Some(a), Some(Entry::Off(_))) => a.enabled.then_some("turns off"),
        (Some(a), Some(e)) => {
            let (same, on) = match e {
                Entry::CurseFile { file, enabled } => (a.file_id == *file && !crate::modrinth::is_mr(a.project_id), *enabled),
                Entry::ModrinthFile { version, enabled } => (a.mr_version == *version, *enabled),
                Entry::Source { pin, enabled } => (a.file_name.eq_ignore_ascii_case(&pin.file), *enabled),
                Entry::Off(_) | Entry::Remove(_) => unreachable!(),
            };
            if !same {
                Some("changes to their version")
            } else if on != a.enabled {
                Some(if on { "turns on" } else { "turns off" })
            } else {
                None
            }
        }
    }
}

/// "Sync again": takes the marks off `keys` and puts each mod back the way
/// friends have it. The marks come off even when a change fails (it's
/// noted and the error returned); the mod is then an ordinary synced one.
pub fn sync_again(inst: &mut Instance, keys: &[String], job: &Job) -> anyhow::Result<()> {
    // The copy handed in can be older than the folder (an update or merge may
    // have finished since), so start from what's on disk.
    let _edit = crate::instances::edit_lock(&inst.root);
    crate::instances::reload(inst);
    let mut todo = Vec::new();
    for k in keys {
        let Some(kept) = inst.only_here.remove(k) else { continue };
        let a = inst.addons.iter().find(|a| groupsync::addon_key(a) == *k);
        if sync_again_does(a, kept.theirs.as_ref()).is_none() {
            continue;
        }
        todo.push(Change { key: k.clone(), was: state_here(a), now: state_of(kept.theirs.as_ref()), entry: kept.theirs });
    }
    inst.save()?;
    if todo.is_empty() {
        return Ok(());
    }
    groupsync::apply(inst, &todo, &[], job)
}

/// A mod here as [`groupsync::apply`] reads the state it changes from.
fn state_here(a: Option<&Addon>) -> State {
    match a {
        None => State::Unlisted,
        Some(a) if !a.from_pack => State::File { file: String::new(), on: a.enabled },
        Some(a) if !a.enabled => State::Off,
        Some(_) => State::Unlisted,
    }
}

fn state_of(e: Option<&Entry>) -> State {
    match e {
        None => State::Unlisted,
        Some(Entry::Off(_)) => State::Off,
        Some(Entry::Remove(_)) => State::Removed,
        Some(Entry::CurseFile { enabled, .. } | Entry::ModrinthFile { enabled, .. } | Entry::Source { enabled, .. }) => {
            State::File { file: String::new(), on: *enabled }
        }
    }
}

/// Splits the changes a group merge decided on: those to kept mods only
/// update what friends have ([`Kept::theirs`]); the rest are returned to
/// be made. Their marks stay (who changed a mod last still decides later
/// changes, and this PC's next publish then matches them), but they stay
/// out of the task's notes. Returns the changes to make and whether a
/// kept mod was updated (the instance then needs saving).
pub fn split(inst: &mut Instance, todo: Vec<Change>, merged: &mut Merged) -> (Vec<Change>, bool) {
    let mut rest = Vec::new();
    let mut touched = false;
    for c in todo {
        match inst.only_here.get_mut(&c.key) {
            Some(kept) => {
                kept.theirs = c.entry;
                touched = true;
                merged.made.retain(|(k, _, _)| *k != c.key);
            }
            None => rest.push(c),
        }
    }
    (rest, touched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::groupsync::{Got, Kind, Mark, PackCopy};
    use crate::sharecode::{Base, Target};

    fn addon(p: u64, file: u64, on: bool, from_pack: bool) -> Addon {
        Addon { project_id: p, file_id: file, file_name: format!("m{p}-{file}.jar"), name: format!("M{p}"), enabled: on, from_pack, ..Default::default() }
    }

    fn plain(entries: Vec<Entry>) -> Share {
        Share { base: Base::Plain { game_version: "1.21.1".into(), loader: "neoforge".into() }, name: None, loader: None, entries }
    }

    fn inst_with(addons: Vec<Addon>) -> Instance {
        Instance { name: "T".into(), game_version: "1.21.1".into(), loader: "neoforge".into(), addons, ..Default::default() }
    }

    /// The mods of a code whose CurseForge files are `file = project * 10`.
    fn mods_of(s: &Share) -> Mods {
        groupsync::mods(s, |e| match e {
            Entry::CurseFile { file, .. } => Some(file / 10),
            _ => None,
        })
    }

    fn job() -> (crate::jobs::Jobs, Job) {
        let jobs = crate::jobs::Jobs::default();
        let j = jobs.start("t");
        (jobs, j)
    }

    #[test]
    fn a_kept_mod_is_published_as_friends_last_had_it() {
        // 1 is kept: on here before marking, then turned off here. 2 is
        // ordinary. 3 was added and kept before it was ever published.
        let mut inst = inst_with(vec![addon(1, 10, true, false), addon(2, 20, true, false)]);
        let published = mods_of(&plain(vec![
            Entry::CurseFile { file: 10, enabled: true },
            Entry::CurseFile { file: 20, enabled: true },
        ]));
        mark_from(&mut inst, &["p:1".into()], &published);
        inst.addons[0].enabled = false;
        inst.addons.push(addon(3, 30, true, false));
        mark_from(&mut inst, &["p:3".into()], &published);
        let (_j, job) = job();
        let s = crate::sharecode::make(&inst, &job).unwrap().share;
        let mut e = s.entries.clone();
        e.sort_by_key(|e| format!("{e:?}"));
        assert_eq!(e, vec![Entry::CurseFile { file: 10, enabled: true }, Entry::CurseFile { file: 20, enabled: true }]);
        // So the others see no change from marking it.
        assert!(groupsync::changes(&published, &mods_of(&s)).is_empty());
    }

    #[test]
    fn marking_twice_keeps_the_first_copy() {
        let mut inst = inst_with(vec![addon(1, 10, true, false)]);
        mark_from(&mut inst, &["p:1".into()], &mods_of(&plain(vec![Entry::CurseFile { file: 10, enabled: true }])));
        mark_from(&mut inst, &["p:1".into()], &mods_of(&plain(vec![])));
        assert_eq!(inst.only_here["p:1"].theirs, Some(Entry::CurseFile { file: 10, enabled: true }));
    }

    #[test]
    fn the_marks_are_saved_but_never_in_a_code() {
        let mut inst = inst_with(vec![addon(1, 10, true, false)]);
        inst.only_here.insert("p:1".into(), Kept { theirs: Some(Entry::Off(Target::Curse(1))), ..Default::default() });
        let json = serde_json::to_string(&inst).unwrap();
        let back: Instance = serde_json::from_str(&json).unwrap();
        assert_eq!(back.only_here, inst.only_here);
        // An instance with none writes nothing.
        assert!(!serde_json::to_string(&inst_with(vec![])).unwrap().contains("only_here"));
    }

    #[test]
    fn sync_again_says_what_changes_here() {
        let on = addon(1, 10, true, true);
        let off = addon(1, 10, false, true);
        let mine = addon(1, 10, true, false);
        let cf = |file, enabled| Entry::CurseFile { file, enabled };
        assert_eq!(sync_again_does(Some(&off), None), Some("turns on"));
        assert_eq!(sync_again_does(Some(&on), None), None);
        assert_eq!(sync_again_does(Some(&mine), None), Some("is removed"));
        assert_eq!(sync_again_does(Some(&on), Some(&Entry::Off(Target::Curse(1)))), Some("turns off"));
        assert_eq!(sync_again_does(Some(&on), Some(&Entry::Remove(Target::Curse(1)))), Some("is removed"));
        assert_eq!(sync_again_does(Some(&mine), Some(&cf(10, true))), None);
        assert_eq!(sync_again_does(Some(&mine), Some(&cf(11, true))), Some("changes to their version"));
        assert_eq!(sync_again_does(Some(&mine), Some(&cf(10, false))), Some("turns off"));
        assert_eq!(sync_again_does(None, Some(&cf(10, true))), Some("is added"));
        assert_eq!(sync_again_does(None, None), None);
    }

    #[test]
    fn sync_again_turns_a_kept_pack_mod_back_on() {
        let dir = std::env::temp_dir().join(format!("hopper-onlyhere-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut inst = inst_with(vec![addon(1, 10, true, true)]);
        inst.root = dir.clone();
        std::fs::create_dir_all(inst.mods_dir()).unwrap();
        std::fs::write(inst.mods_dir().join("m1-10.jar"), b"x").unwrap();
        inst.only_here.insert("p:1".into(), Kept::default());
        let (_j, job) = job();
        crate::install::set_enabled(&mut inst, 0, false).unwrap();
        sync_again(&mut inst, &["p:1".into()], &job).unwrap();
        assert!(inst.only_here.is_empty());
        assert!(inst.addons[0].enabled);
        assert!(inst.mods_dir().join("m1-10.jar").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_follow_leaves_kept_mods_alone() {
        let dir = std::env::temp_dir().join(format!("hopper-onlyhere-follow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pack = |f: &str| Addon { file_name: f.into(), name: f.into(), enabled: true, from_pack: true, ..Default::default() };
        let src = crate::sources::ModSource { url: "owner/d".into(), pattern: "d-#.jar".into(), ..Default::default() };
        let d = Addon { file_name: "d-1.jar".into(), name: "D".into(), enabled: true, source: Some(src.clone()), ..Default::default() };
        // a, c and d are kept here; b isn't.
        let mut inst = inst_with(vec![pack("a.jar"), pack("b.jar"), pack("c.jar"), d]);
        inst.root = dir.clone();
        std::fs::create_dir_all(inst.mods_dir()).unwrap();
        for a in &inst.addons {
            std::fs::write(inst.mods_dir().join(&a.file_name), b"x").unwrap();
        }
        for k in ["n:a.jar", "n:c.jar", "s:owner/d#d-#.jar"] {
            inst.only_here.insert(k.into(), Kept::default());
        }
        // The owner had d last time and turned a and b off and took c out.
        let prev = plain(vec![Entry::Source {
            pin: crate::sources::carry::Pin { file: "d-1.jar".into(), source: src },
            enabled: true,
        }]);
        let now = plain(vec![
            Entry::Off(Target::Named("a.jar".into())),
            Entry::Off(Target::Named("b.jar".into())),
            Entry::Remove(Target::Named("c.jar".into())),
        ]);
        let (_j, job) = job();
        crate::sharecode::apply_existing(&mut inst, &now, Some(&prev), &job).unwrap();
        let on = |f: &str| inst.addons.iter().find(|a| a.file_name == f).map(|a| a.enabled);
        assert_eq!(on("a.jar"), Some(true));
        assert_eq!(on("b.jar"), Some(false));
        assert_eq!(on("c.jar"), Some(true));
        assert_eq!(on("d-1.jar"), Some(true));
        // What the owner has now is remembered for the next publish.
        assert_eq!(inst.only_here["n:a.jar"].theirs, Some(Entry::Off(Target::Named("a.jar".into()))));
        assert_eq!(inst.only_here["n:c.jar"].theirs, Some(Entry::Remove(Target::Named("c.jar".into()))));
        assert_eq!(inst.only_here["s:owner/d#d-#.jar"].theirs, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn marking_here_keeps_what_was_published() {
        // A pack mod (on), a pack mod (off) and one added by hand.
        let mut inst = inst_with(vec![addon(1, 10, true, true), addon(2, 20, false, true), addon(3, 30, true, false)]);
        inst.pack = Some(crate::instances::PackRef { project_id: 99, file_id: 990, ..Default::default() });
        let keys: Vec<String> = ["p:1", "p:2", "p:3"].iter().map(|s| s.to_string()).collect();
        mark_here(&mut inst, &keys);
        // As a code lists them: the pack's file needs no entry.
        assert_eq!(inst.only_here["p:1"].theirs, None);
        assert_eq!(inst.only_here["p:2"].theirs, Some(Entry::Off(Target::Curse(2))));
        assert_eq!(inst.only_here["p:3"].theirs, Some(Entry::CurseFile { file: 30, enabled: true }));
        // A plain instance's mods are all its own.
        let mut plain = inst_with(vec![addon(4, 40, false, false)]);
        mark_here(&mut plain, &["p:4".to_string()]);
        assert_eq!(plain.only_here["p:4"].theirs, Some(Entry::CurseFile { file: 40, enabled: false }));
    }

    fn got(code: &Share, seq: u64, name: &str) -> Got {
        Got { code: crate::sharecode::encode(code), seq, at: seq, name: name.into() }
    }

    #[test]
    fn a_group_change_to_a_kept_mod_only_updates_theirs() {
        // Mia turns off 1 (kept here) and 2 (not kept).
        let before = plain(vec![Entry::CurseFile { file: 10, enabled: true }, Entry::CurseFile { file: 20, enabled: true }]);
        let after = plain(vec![Entry::CurseFile { file: 10, enabled: false }, Entry::CurseFile { file: 20, enabled: false }]);
        let mut copy = PackCopy { inst: "i".into(), group: "g".into(), local: "l".into(), ..Default::default() };
        copy.got.insert("mia".into(), got(&before, 1, "Mia"));
        copy.arrive("mia", got(&after, 2, "Mia"));
        copy.sent = crate::sharecode::encode(&before);

        let mut inst = inst_with(vec![addon(1, 10, true, false), addon(2, 20, true, false)]);
        mark_from(&mut inst, &["p:1".into()], &mods_of(&before));
        // Turned off here too, just for me: published as friends had it.
        inst.addons[0].enabled = false;
        let (_j, job) = job();
        let here = crate::sharecode::make(&inst, &job).unwrap().share;
        let read = |c: &str| crate::sharecode::decode(c).ok().map(|s| mods_of(&s));
        let (todo, mut merged) = groupsync::plan(&copy, &mods_of(&here), Some(&mods_of(&before)), read);
        assert_eq!(todo.len(), 2);
        let (todo, touched) = split(&mut inst, todo, &mut merged);
        assert!(touched);
        assert_eq!(todo.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(), ["p:2"]);
        assert_eq!(inst.only_here["p:1"].theirs, Some(Entry::CurseFile { file: 10, enabled: false }));
        // Who changed it stays known; only the notes leave it out.
        assert!(merged.marks.iter().any(|(k, m)| k == "p:1" && m.by == "mia"));
        assert!(merged.made.iter().all(|(k, _, _)| k != "p:1"));

        // This PC makes the other change, then publishes: 1 goes out as Mia
        // has it, so the others see nothing new from it, and no mark of
        // this PC's own replaces Mia's.
        inst.addons[1].enabled = false;
        let republished = crate::sharecode::make(&inst, &job).unwrap().share;
        let mut marks = copy.marks.clone();
        marks.extend(merged.marks.iter().cloned());
        let theirs_now = mods_of(&after);
        assert!(groupsync::changes(&theirs_now, &mods_of(&republished)).is_empty());
        let had = marks.clone();
        groupsync::mark_own(&mut marks, Some(&mods_of(&before)), &mods_of(&republished), "me", 3, 3);
        assert_eq!(marks, had);
        assert_eq!(marks["p:1"], Mark { state: State::File { file: "cf:10".into(), on: false }, kind: Kind::Off, by: "mia".into(), name: "Mia".into(), seq: 2, at: 2 });
    }

    #[test]
    fn a_group_copy_without_the_mark_still_takes_the_change() {
        // Marks are per PC: Mia's copy has nothing kept, so this PC's
        // change to 1 (made before it was kept) reaches her as usual.
        let before = plain(vec![Entry::CurseFile { file: 10, enabled: true }]);
        let after = plain(vec![Entry::CurseFile { file: 10, enabled: false }]);
        let mut copy = PackCopy::default();
        copy.got.insert("me".into(), got(&before, 1, "Me"));
        copy.arrive("me", got(&after, 2, "Me"));
        let mut inst = inst_with(vec![addon(1, 10, true, false)]);
        let read = |c: &str| crate::sharecode::decode(c).ok().map(|s| mods_of(&s));
        let (todo, mut merged) = groupsync::plan(&copy, &mods_of(&before), None, read);
        let (todo, touched) = split(&mut inst, todo, &mut merged);
        assert!(!touched);
        assert_eq!(todo.len(), 1);
    }
}
