//! What a followed pack says is missing here, and why.
//!
//! Two things end up in that list and they need different words. A jar the
//! sharer added by hand is on no mod site: only the sharer can send it, and
//! their code names it ([`FollowedRecipe::left_out`]). A file that failed to
//! download is on a mod site and Hopper fetches it again by itself
//! ([`FollowedRecipe::failed`]). Told as one ("isn't on any mod site, ask
//! them to send it"), the player asks a friend for a file Hopper is about to
//! get.
//!
//! The list is also kept true: the sharer's next code replaces only what the
//! sharer said, a failed file stays listed until it is here or the pack no
//! longer has it, and one mod is one row.

use std::collections::HashMap;

use crate::instances::Instance;
use crate::settings::FollowedRecipe;
use crate::sharecode::{self, Entry};

/// A file of a followed pack that failed to come in.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Failed {
    /// The file, as the code lists it.
    pub entry: Entry,
    /// The mod's name once it is known ([`lookup`]); empty until then, and
    /// the row is worded by what it is ([`sharecode::entry_says`]).
    #[serde(default)]
    pub name: String,
}

impl Failed {
    /// The row for people: the mod's name, else what the file is. Never
    /// empty for a file.
    pub fn says(&self) -> String {
        if self.name.trim().is_empty() { sharecode::entry_says(&self.entry) } else { self.name.trim().to_string() }
    }
}

/// What tells two rows apart: the file on its site, or the link a mod comes
/// from (so a newer release of the same mod takes the older one's row).
/// None for an entry that has no file.
pub fn key(e: &Entry) -> Option<String> {
    match e {
        Entry::CurseFile { file, .. } => Some(format!("cf:{file}")),
        Entry::ModrinthFile { version, .. } => Some(format!("mr:{version}")),
        Entry::Source { pin, .. } => Some(format!("s:{}#{}", pin.source.url, pin.source.pattern)),
        Entry::Remove(_) | Entry::Off(_) | Entry::Lock { .. } => None,
    }
}

/// The file a row from before [`FollowedRecipe::failed`] names. Hopper
/// 0.89.1 put failed downloads into `left_out` as [`sharecode::entry_says`]
/// text; read back, they get the right words and can be looked up.
fn legacy(row: &str) -> Option<Entry> {
    let inner = |start: &str| row.strip_prefix(start)?.strip_suffix(')').map(str::trim).filter(|s| !s.is_empty());
    if let Some(file) = inner("a CurseForge mod (file ").and_then(|n| n.parse().ok()) {
        return Some(Entry::CurseFile { file, enabled: true });
    }
    inner("a Modrinth mod (version ").map(|v| Entry::ModrinthFile { version: v.to_string(), enabled: true })
}

/// Puts `e` in the failed rows: one row per file, the newest entry, and a
/// name already known is kept.
fn put(failed: &mut Vec<Failed>, e: &Entry) {
    let Some(k) = key(e) else { return };
    let name = match e {
        Entry::Source { pin, .. } => crate::sources::carry::display_name(pin),
        _ => String::new(),
    };
    match failed.iter_mut().find(|r| key(&r.entry).as_deref() == Some(k.as_str())) {
        Some(r) => {
            r.entry = e.clone();
            if !name.is_empty() {
                r.name = name;
            }
        }
        None => failed.push(Failed { entry: e.clone(), name }),
    }
}

/// Moves failed downloads an older Hopper kept in `left_out` to `failed`,
/// and takes out names said twice.
fn tidy(f: &mut FollowedRecipe) {
    let mut names: Vec<String> = Vec::new();
    for row in std::mem::take(&mut f.left_out) {
        match legacy(&row) {
            Some(e) => put(&mut f.failed, &e),
            None if row.trim().is_empty() || names.iter().any(|n| n.eq_ignore_ascii_case(&row)) => {}
            None => names.push(row),
        }
    }
    f.left_out = names;
}

/// Whether `code` still lists the file `e`. True when the code can't be
/// read: a row is never dropped on a guess.
fn listed(code: &str, e: &Entry) -> bool {
    let Ok(share) = sharecode::decode(code) else { return true };
    let k = key(e);
    share.entries.iter().any(|x| key(x) == k)
}

/// `code` was applied here and `missing` of it didn't come in (from
/// [`sharecode::landed`]): those are listed as failed downloads, and a file
/// of the code listed before that did come in this time is taken off.
pub fn applied(f: &mut FollowedRecipe, code: &str, missing: &[Entry]) {
    tidy(f);
    // Only a code that reads can say a file of it came in.
    if let Ok(share) = sharecode::decode(code) {
        let among = |list: &[Entry], e: &Entry| list.iter().any(|x| key(x) == key(e));
        f.failed.retain(|r| !among(&share.entries, &r.entry) || among(missing, &r.entry));
    }
    for e in missing {
        put(&mut f.failed, e);
    }
}

/// The sharer's newer `code` arrived, naming the jars they added by hand
/// (`hand_added`). Their word replaces only what they said before. A failed
/// download stays listed while the pack still has that file: the code knows
/// nothing about what came in here, and wiping the row took the reason off
/// the screen before the player read it.
pub fn sharer_said(f: &mut FollowedRecipe, code: &str, hand_added: &[String]) {
    tidy(f);
    f.left_out = hand_added.to_vec();
    tidy(f);
    f.failed.retain(|r| listed(code, &r.entry));
}

/// Whether the jar the sharer named is in `inst`: by the mod's name or its
/// file's, as the sharer's code names it (`sharecode::make`).
fn has_jar(inst: &Instance, name: &str) -> bool {
    inst.addons.iter().any(|a| a.name.eq_ignore_ascii_case(name) || a.file_name.trim_end_matches(".disabled").eq_ignore_ascii_case(name))
}

/// Takes off every row whose file is in `inst` now (fetched by a later
/// read, or put in by hand). True when a row went.
pub fn drop_here(f: &mut FollowedRecipe, inst: &Instance) -> bool {
    let before = (f.left_out.clone(), f.failed.clone());
    tidy(f);
    f.failed.retain(|r| !sharecode::on_disk(inst, &r.entry));
    f.left_out.retain(|n| !has_jar(inst, n));
    before != (f.left_out.clone(), f.failed.clone())
}

/// The jars the sharer added by hand, as their code named them: what a
/// member's copy passes on to the others. Never a failed download of this
/// PC, which is nobody else's news.
pub fn hand_added(f: &FollowedRecipe) -> Vec<String> {
    let mut f = f.clone();
    tidy(&mut f);
    f.left_out
}

/// The failed rows that have no name yet, to look up.
pub fn unnamed(f: &FollowedRecipe) -> Vec<Entry> {
    let mut f = f.clone();
    tidy(&mut f);
    f.failed.into_iter().filter(|r| r.name.trim().is_empty()).map(|r| r.entry).collect()
}

/// Writes looked-up names (by [`key`]) into the failed rows. A file the
/// lookup didn't name keeps its plain wording. True when a row changed.
pub fn set_names(f: &mut FollowedRecipe, names: &HashMap<String, String>) -> bool {
    tidy(f);
    let mut changed = false;
    for r in &mut f.failed {
        let Some(n) = key(&r.entry).and_then(|k| names.get(&k)).map(|n| n.trim()).filter(|n| !n.is_empty()) else { continue };
        if r.name != n {
            r.name = n.to_string();
            changed = true;
        }
    }
    changed
}

/// "A", "A and B", "A, B, C and 2 more".
fn listing(names: &[String]) -> String {
    const SHOWN: usize = 3;
    let mut list: Vec<String> = names.iter().take(SHOWN).cloned().collect();
    if names.len() > SHOWN {
        list.push(format!("{} more", names.len() - SHOWN));
    }
    match list.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The line for jars a friend added by hand (on no mod site), by name, and
/// who to ask for them. Long lists name the first few.
pub fn no_source_line(names: &[String], who: &str) -> String {
    let listed = listing(names);
    if names.len() == 1 {
        format!("{listed} isn't on any mod site, so Hopper can't download it. Ask {who} to send it.")
    } else {
        format!("{listed} aren't on any mod site, so Hopper can't download them. Ask {who} to send them.")
    }
}

/// The line for files that failed to download: nothing for the player to
/// do, Hopper fetches them the next time it reads the pack's changes.
pub fn failed_line(names: &[String]) -> String {
    format!("Hopper couldn't download {} just now. It tries again by itself.", listing(names))
}

/// What the follow's panel says is missing: a line for the downloads that
/// failed, a line for the jars only `who` can send. Empty when nothing is.
pub fn lines(f: &FollowedRecipe, who: &str) -> Vec<String> {
    let mut f = f.clone();
    tidy(&mut f);
    let failed: Vec<String> = f.failed.iter().map(Failed::says).filter(|n| !n.is_empty()).collect();
    let mut out = Vec::new();
    if !failed.is_empty() {
        out.push(failed_line(&failed));
    }
    if !f.left_out.is_empty() {
        out.push(no_source_line(&f.left_out, who));
    }
    out
}

/// [`lines`] for the follow as `inst` is now: a file that is here is not
/// said to be missing, whatever the saved list still holds.
pub fn lines_for(f: &FollowedRecipe, inst: Option<&Instance>, who: &str) -> Vec<String> {
    let mut f = f.clone();
    if let Some(inst) = inst {
        drop_here(&mut f, inst);
    }
    lines(&f, who)
}

/// The names of the mods `entries` are files of, by [`key`]: one question
/// to CurseForge and one to Modrinth for all of them. A site that doesn't
/// answer leaves its files out (they keep their plain wording, and are
/// asked about again another time).
pub fn lookup(entries: &[Entry]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let cf: Vec<u64> = entries.iter().filter_map(|e| if let Entry::CurseFile { file, .. } = e { Some(*file) } else { None }).collect();
    if !cf.is_empty() {
        let named = crate::cf::get_files(&cf).and_then(|files| {
            let mut ids: Vec<u64> = files.iter().map(|f| f.mod_id).collect();
            ids.sort_unstable();
            ids.dedup();
            Ok((files, crate::cf::get_mods(&ids)?))
        });
        match named {
            Ok((files, mods)) => {
                for f in &files {
                    if let Some(m) = mods.iter().find(|m| m.id == f.mod_id) {
                        out.insert(format!("cf:{}", f.id), m.name.clone());
                    }
                }
            }
            Err(e) => log::warn!("followed pack: naming {} CurseForge file(s) that didn't come in: {e:#}", cf.len()),
        }
    }
    let mr: Vec<String> = entries.iter().filter_map(|e| if let Entry::ModrinthFile { version, .. } = e { Some(version.clone()) } else { None }).collect();
    if !mr.is_empty() {
        let named = crate::modrinth::versions_by_id(&mr).and_then(|versions| {
            let mut ids: Vec<String> = versions.iter().map(|v| v.project_id.clone()).collect();
            ids.sort();
            ids.dedup();
            Ok((versions, crate::modrinth::projects(&ids)?))
        });
        match named {
            Ok((versions, projects)) => {
                for v in &versions {
                    if let Some(p) = projects.iter().find(|p| p.id == v.project_id) {
                        out.insert(format!("mr:{}", v.id), p.title.clone());
                    }
                }
            }
            Err(e) => log::warn!("followed pack: naming {} Modrinth file(s) that didn't come in: {e:#}", mr.len()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instances::Addon;
    use crate::sharecode::{Base, Share};

    fn cf(file: u64) -> Entry {
        Entry::CurseFile { file, enabled: true }
    }

    fn mr(v: &str) -> Entry {
        Entry::ModrinthFile { version: v.into(), enabled: true }
    }

    fn code(entries: Vec<Entry>) -> String {
        sharecode::encode(&Share { base: Base::Plain { game_version: "1.21.1".into(), loader: "neoforge".into() }, name: None, loader: None, entries })
    }

    fn addon(project: u64, file: u64, name: &str) -> Addon {
        Addon { project_id: project, file_id: file, name: name.into(), file_name: format!("{name}.jar"), enabled: true, ..Default::default() }
    }

    fn named(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn a_failed_download_and_a_jar_on_no_site_get_their_own_words() {
        let mut f = FollowedRecipe { left_out: vec!["my-own-mod.jar".into()], ..Default::default() };
        applied(&mut f, &code(vec![cf(51)]), &[cf(51)]);
        set_names(&mut f, &named(&[("cf:51", "Sodium")]));
        assert_eq!(
            lines(&f, "Mia"),
            [
                "Hopper couldn't download Sodium just now. It tries again by itself.",
                "my-own-mod.jar isn't on any mod site, so Hopper can't download it. Ask Mia to send it.",
            ]
        );
        // The failed download is never said to be on no site, nor asked for.
        let failed = &lines(&f, "Mia")[0];
        assert!(!failed.contains("mod site") && !failed.contains("Ask"), "{failed}");
    }

    #[test]
    fn several_of_each_are_listed_and_long_lists_are_cut() {
        let mut f = FollowedRecipe { left_out: vec!["a.jar".into(), "b.jar".into()], ..Default::default() };
        let missing = [cf(1), cf(2), cf(3), cf(4), cf(5)];
        applied(&mut f, &code(missing.to_vec()), &missing);
        set_names(&mut f, &named(&[("cf:1", "A"), ("cf:2", "B"), ("cf:3", "C")]));
        assert_eq!(
            lines(&f, "Mia"),
            [
                "Hopper couldn't download A, B, C and 2 more just now. It tries again by itself.",
                "a.jar and b.jar aren't on any mod site, so Hopper can't download them. Ask Mia to send them.",
            ]
        );
    }

    #[test]
    fn a_lookup_that_fails_leaves_the_plain_wording_not_empty_text() {
        let mut f = FollowedRecipe::default();
        applied(&mut f, &code(vec![cf(51), mr("AbCd1234")]), &[cf(51), mr("AbCd1234")]);
        assert_eq!(unnamed(&f), [cf(51), mr("AbCd1234")]);
        // The sites didn't answer, or answered with a blank name.
        assert!(!set_names(&mut f, &HashMap::new()));
        assert!(!set_names(&mut f, &named(&[("cf:51", "  ")])));
        assert_eq!(
            lines(&f, "Mia"),
            ["Hopper couldn't download a CurseForge mod (file 51) and a Modrinth mod (version AbCd1234) just now. It tries again by itself."]
        );
        // One site answered: that row is named, the other keeps its wording.
        assert!(set_names(&mut f, &named(&[("mr:AbCd1234", "Lithium")])));
        assert_eq!(lines(&f, "Mia"), ["Hopper couldn't download a CurseForge mod (file 51) and Lithium just now. It tries again by itself."]);
        assert_eq!(unnamed(&f), [cf(51)], "only the unnamed row is asked about again");
    }

    #[test]
    fn the_sharers_next_code_keeps_a_failed_row_and_its_name() {
        let mut f = FollowedRecipe { left_out: vec!["old-hand.jar".into()], ..Default::default() };
        applied(&mut f, &code(vec![cf(11), cf(51)]), &[cf(51)]);
        set_names(&mut f, &named(&[("cf:51", "Sodium")]));
        // A newer code that still has file 51, and names another hand-added jar.
        sharer_said(&mut f, &code(vec![cf(11), cf(51), cf(61)]), &["new-hand.jar".into()]);
        assert_eq!(f.failed, [Failed { entry: cf(51), name: "Sodium".into() }], "the reason is still there");
        assert_eq!(f.left_out, ["new-hand.jar"], "what the sharer said is theirs to replace");
        // The pack dropped that file (a newer one of the mod, or the mod went).
        sharer_said(&mut f, &code(vec![cf(11), cf(52)]), &[]);
        assert!(f.failed.is_empty() && f.left_out.is_empty(), "{f:?}");
    }

    #[test]
    fn a_code_that_does_not_read_drops_nothing() {
        let mut f = FollowedRecipe::default();
        applied(&mut f, &code(vec![cf(51)]), &[cf(51)]);
        sharer_said(&mut f, "not a code", &[]);
        assert_eq!(f.failed.len(), 1);
    }

    #[test]
    fn a_row_goes_once_its_file_is_here_and_a_missing_one_stays() {
        let mut f = FollowedRecipe { left_out: vec!["Hand Mod".into(), "other.jar".into()], ..Default::default() };
        applied(&mut f, &code(vec![cf(51), cf(61)]), &[cf(51), cf(61)]);
        // A later read fetched file 51, and the player put "Hand Mod" in.
        let inst = Instance { addons: vec![addon(5, 51, "Sodium"), addon(0, 0, "hand mod")], ..Default::default() };
        assert_eq!(lines_for(&f, Some(&inst), "Mia").len(), 2);
        assert!(lines_for(&f, Some(&inst), "Mia")[0].contains("file 61") && !lines_for(&f, Some(&inst), "Mia")[0].contains("file 51"));
        assert!(drop_here(&mut f, &inst));
        assert_eq!(f.failed, [Failed { entry: cf(61), name: String::new() }]);
        assert_eq!(f.left_out, ["other.jar"]);
        assert!(!drop_here(&mut f, &inst), "nothing more to take off");
        // The same arrives through the sharer's next code plus the folder.
        sharer_said(&mut f, &code(vec![cf(51), cf(61)]), &["other.jar".into()]);
        drop_here(&mut f, &Instance { addons: vec![addon(6, 61, "Lithium"), addon(0, 0, "other")], ..Default::default() });
        assert!(lines(&f, "Mia").is_empty(), "{f:?}");
    }

    #[test]
    fn the_same_reason_again_makes_no_second_row() {
        let mut f = FollowedRecipe::default();
        let c = code(vec![cf(51), mr("v1")]);
        applied(&mut f, &c, &[cf(51), mr("v1")]);
        set_names(&mut f, &named(&[("cf:51", "Sodium")]));
        // It failed again, switched off this time: one row, the name kept.
        applied(&mut f, &c, &[Entry::CurseFile { file: 51, enabled: false }, mr("v1")]);
        sharer_said(&mut f, &c, &["a.jar".into(), "A.JAR".into(), "a.jar".into()]);
        sharer_said(&mut f, &c, &["a.jar".into()]);
        assert_eq!(f.failed.len(), 2);
        assert_eq!(f.failed[0], Failed { entry: Entry::CurseFile { file: 51, enabled: false }, name: "Sodium".into() });
        assert_eq!(f.left_out, ["a.jar"]);
    }

    #[test]
    fn an_apply_where_the_file_came_in_takes_its_row_off() {
        let mut f = FollowedRecipe::default();
        let c = code(vec![cf(51), cf(61)]);
        applied(&mut f, &c, &[cf(51), cf(61)]);
        applied(&mut f, &c, &[cf(61)]);
        assert_eq!(f.failed, [Failed { entry: cf(61), name: String::new() }]);
    }

    #[test]
    fn rows_from_before_are_read_as_failed_downloads() {
        // 0.89.1 kept a failed download in `left_out`, in these words.
        let mut f = FollowedRecipe {
            left_out: vec!["a CurseForge mod (file 51)".into(), "a Modrinth mod (version AbCd1234)".into(), "my-own-mod.jar".into()],
            ..Default::default()
        };
        assert_eq!(hand_added(&f), ["my-own-mod.jar"], "a failed download here isn't passed on as a hand-added jar");
        assert_eq!(unnamed(&f), [cf(51), mr("AbCd1234")]);
        assert_eq!(lines(&f, "Mia").len(), 2);
        assert!(lines(&f, "Mia")[0].starts_with("Hopper couldn't download a CurseForge mod (file 51) and a Modrinth mod"));
        assert!(set_names(&mut f, &named(&[("cf:51", "Sodium")])));
        assert_eq!(f.left_out, ["my-own-mod.jar"]);
        assert_eq!(f.failed[0].says(), "Sodium");
    }

    #[test]
    fn a_mod_from_a_link_is_named_without_a_lookup() {
        let pin = crate::sources::carry::Pin {
            source: crate::sources::ModSource { kind: crate::sources::Kind::GitHub, publisher: "owner/EZActions".into(), url: "https://github.com/owner/EZActions".into(), ..Default::default() },
            file: "ezactions-1.2.jar".into(),
        };
        let e = Entry::Source { pin, enabled: true };
        let mut f = FollowedRecipe::default();
        applied(&mut f, &code(vec![e.clone()]), &[e]);
        assert!(unnamed(&f).is_empty());
        assert_eq!(lines(&f, "Mia"), ["Hopper couldn't download EZActions just now. It tries again by itself."]);
    }

    #[test]
    fn the_follow_record_reads_without_the_new_list_and_writes_it_back() {
        let old: FollowedRecipe = serde_json::from_str(r#"{"peer_id":"aa","left_out":["x.jar"]}"#).unwrap();
        assert!(old.failed.is_empty());
        assert!(!serde_json::to_string(&old).unwrap().contains("failed"), "nothing new in a file that has none");
        let mut f = old;
        applied(&mut f, &code(vec![cf(51)]), &[cf(51)]);
        let back: FollowedRecipe = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(back, f);
    }
}
