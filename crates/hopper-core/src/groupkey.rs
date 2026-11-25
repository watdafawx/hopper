//! What a group copy is called and how mods are named across PCs: the keys
//! friends sync uses for a mod, a lock and the loader, the loader's words,
//! putting a pack's own file back, and the record a PC keeps per group copy
//! ([`PackCopy`]: which pack, which group, and its shared mod settings).
//!
//! What changed and who wins is the sync core's (`synccore`); nothing here
//! compares two mod lists.

use crate::instances::{Addon, Instance};
use crate::jobs::Job;
use crate::modrinth::{is_mr, num_id};
use crate::sharecode::Entry;
use crate::settings::{FollowedRecipe, Settings};
use serde::{Deserialize, Serialize};

/// The `key` a member's copy is published with, so a PC that knows two-way
/// groups takes it into its own copy instead of listing it as a pack.
pub const COPY_KEY: &str = "~copy";

/// What a mod's lock goes by in a group, next to the mods: "~lock:" and the
/// mod's key. The lock is one more thing the group keeps the same, with the
/// same rules as the mods (latest change wins, echoes change nothing, the
/// mark says who). Like [`LOADER_KEY`], not a mod's key in any form.
pub const LOCK_PREFIX: &str = "~lock:";

/// The mod a lock key is about ("p:123" from "~lock:p:123").
pub fn lock_target(key: &str) -> Option<&str> {
    key.strip_prefix(LOCK_PREFIX)
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

/// The key the loader goes by next to the mods: it is one more thing the
/// group keeps the same, with the same rules (latest change wins, echoes
/// change nothing). Not a mod's key in any form, so it never meets one.
pub const LOADER_KEY: &str = "~loader";

/// The file a mod here is, the way a code names it ("cf:<file>",
/// "mr:<version>", "src:<file name>").
pub(crate) fn file_here(a: &Addon) -> Option<String> {
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

/// For friends sync (`synccore::bridge`): puts the pack's own file of the
/// CurseForge or Modrinth project `project` back, on and counted as the
/// pack's again. False when the pack's version doesn't have the mod.
pub fn put_back_pack_default(inst: &mut Instance, project: u64, job: &Job) -> anyhow::Result<bool> {
    match pack_file(inst, &format!("p:{project}"), job) {
        Some(PackFile::Curse(p, f)) => put_back_pack_file(inst, p, f, job).map(|pending| pending.is_none()),
        Some(PackFile::Modrinth(f)) => put_back_mr_file(inst, &f, job).map(|()| true),
        None => Ok(false),
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
        let index = crate::sharecode::mr_pack_index(inst, job).ok()?;
        return crate::modpack::mr_index_file(&index, name, project).cloned().map(PackFile::Modrinth);
    }
    if p.file_id == 0 {
        return None;
    }
    let project: u64 = key.strip_prefix("p:")?.parse().ok()?;
    let m = crate::sharecode::curse_manifest(inst, p.project_id, p.file_id, job).ok()?;
    m.files.iter().find(|f| f.project_id == project).map(|f| PackFile::Curse(project, f.file_id))
}

fn put_back_mr_file(inst: &mut Instance, f: &crate::modpack::MrIndexFile, job: &Job) -> anyhow::Result<()> {
    crate::modpack::put_back_mr_file_online(inst, f, job)
}

/// Puts the pack's own `file` of `project` back, on and counted as the
/// pack's again (whatever file of it is here now goes to History).
fn put_back_pack_file(inst: &mut Instance, project: u64, file: u64, job: &Job) -> anyhow::Result<Option<crate::modpack::Pending>> {
    let same = inst.addons.iter().any(|a| a.project_id == project && a.file_id == file);
    if !same {
        if let crate::install::FileInstall::Pending(p) = crate::install::install_file_outcome(inst, project, file, Some(true), job)? {
            return Ok(Some(p));
        }
    }
    if let Some(i) = inst.addons.iter().position(|a| a.project_id == project) {
        inst.addons[i].from_pack = true;
        if !inst.addons[i].enabled {
            crate::install::set_enabled_later(inst, i, true)?;
        }
    }
    Ok(None)
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

/// One copy of a group's pack on this PC: which pack and group it is, and
/// the mod settings the group shares (`configsync`). What friends changed
/// and what is waiting is the sync core's (`data/sync/<copy>.json`).
///
/// Saved in the settings. Older versions kept each member's last code here
/// too (`got`, `waiting`, `sent`, `marks`, `owed`, `held`); those fields are
/// read and dropped, so an old settings file still opens.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PackCopy {
    /// The pack's id (the instance id it was first shared from) and the
    /// group it syncs through.
    pub inst: String,
    pub group: String,
    /// The local instance's id.
    pub local: String,
    /// The mods whose settings the group shares, and the settings changed
    /// (`configsync`).
    #[serde(skip_serializing_if = "crate::configsync::Configs::is_empty")]
    pub configs: crate::configsync::Configs,
}

impl PackCopy {
    /// The mailbox lost its data and numbers its events from 1 again: the
    /// shared settings' numbers start over too.
    pub fn relay_reset(&mut self) {
        self.configs.relay_reset();
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
        let c = PackCopy { inst: f.inst.clone(), group, local: f.local.clone(), ..Default::default() };
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

/// One mod that differs between two codes.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub key: String,
    pub was: State,
    pub now: State,
    /// The new code's entry for it (None: no longer listed).
    pub entry: Option<Entry>,
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

/// Makes the changes in the instance. A pack mod put back to the pack's
/// own file can't be done from a change alone (the pack's file isn't in
/// the code); it is noted. A change that fails doesn't stop the others,
/// but fails the whole: tried again, the ones made are already there.
pub fn apply(inst: &mut Instance, todo: &[Change], skip_sources: &[crate::sources::carry::Pin], job: &Job) -> anyhow::Result<()> {
    apply_with_pending(inst, todo, skip_sources, job).map(|_| ())
}

/// Like [`apply`], returning `(mod key, pending)` for each change whose file
/// couldn't be downloaded automatically and was queued for a browser download.
pub fn apply_with_pending(
    inst: &mut Instance,
    todo: &[Change],
    skip_sources: &[crate::sources::carry::Pin],
    job: &Job,
) -> anyhow::Result<Vec<(String, crate::modpack::Pending)>> {
    let mut failed: Vec<(String, anyhow::Error)> = Vec::new();
    let mut by_hand: Vec<(String, crate::modpack::Pending)> = Vec::new();
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
        let r: anyhow::Result<Option<crate::modpack::Pending>> = match (&c.now, &c.entry) {
            (State::Off, _) => match at {
                Some(i) => switch(inst, i, false, &name, job).map(|()| None),
                None => Ok(None),
            },
            (State::Removed, _) => match at {
                Some(i) => crate::install::remove(inst, i).map(|()| None),
                None => Ok(None),
            },
            (State::Unlisted, _) => match (&c.was, at) {
                (State::Off, Some(i)) => switch(inst, i, true, &name, job).map(|()| None),
                // A Modrinth pack names its mods by file, so a pack mod
                // updated here (no longer counted as the pack's) is found
                // by asking the pack first, below.
                (State::File { .. }, Some(i)) if !inst.addons[i].from_pack && !is_mr_pack(inst) => crate::install::remove(inst, i).map(|()| None),
                // Back to the pack's own file: how an undone update or
                // removal of a pack mod reads in a member's code.
                (State::File { .. } | State::Removed, _) => match pack_file(inst, &c.key, job) {
                    Some(PackFile::Curse(project, file)) => put_back_pack_file(inst, project, file, job),
                    Some(PackFile::Modrinth(f)) => put_back_mr_file(inst, &f, job).map(|()| None),
                    None => match at {
                        Some(i) if matches!(c.was, State::File { .. }) && !inst.addons[i].from_pack => crate::install::remove(inst, i).map(|()| None),
                        _ => {
                            job.note(format!("{name}: putting back the modpack's own file isn't done by sync"));
                            Ok(None)
                        }
                    },
                },
                _ => Ok(None),
            },
            (State::File { on, .. }, Some(e)) => file_change(inst, at, e, *on, skip_sources, job),
            (State::File { .. }, None) => Ok(None),
        };
        match r {
            Ok(Some(p)) => by_hand.push((c.key.clone(), p)),
            Ok(None) => {}
            Err(e) => failed.push((name, e)),
        }
    }
    inst.save()?;
    match failed.into_iter().next() {
        None => Ok(by_hand),
        Some((name, e)) => Err(e.context(format!("Couldn't change {name}"))),
    }
}

/// Whether making `c` here would take a mod's file away: a removal, or a
/// file of the member's no longer listed.
fn removes(c: &Change) -> bool {
    matches!((&c.was, &c.now), (_, State::Removed) | (State::File { .. }, State::Unlisted))
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

fn file_change(
    inst: &mut Instance,
    at: Option<usize>,
    e: &Entry,
    on: bool,
    skip_sources: &[crate::sources::carry::Pin],
    job: &Job,
) -> anyhow::Result<Option<crate::modpack::Pending>> {
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
                return Ok(None);
            }
            if skip_sources.iter().any(|s| s.source.url == pin.source.url && s.source.pattern == pin.source.pattern) {
                job.note(format!("{}: not installed, you haven't confirmed its source on this PC", crate::sources::carry::display_name(pin)));
                return Ok(None);
            }
            crate::sources::install_pinned(inst, pin, *enabled, job)?;
            return Ok(None);
        }
        _ => return Ok(None),
    };
    let same = at.is_some_and(|i| inst.addons[i].file_id == file && inst.addons[i].project_id == project);
    if !same {
        if let crate::install::FileInstall::Pending(p) = crate::install::install_file_outcome(inst, project, file, Some(on), job)? {
            return Ok(Some(p));
        }
    }
    if let Some(i) = inst.addons.iter().position(|a| a.project_id == project)
        && inst.addons[i].enabled != on
    {
        crate::install::set_enabled_later(inst, i, on)?;
    }
    Ok(None)
}

/// Who last changed a mod, and in which of their messages: how a member's
/// message dated its changes on the wire (`Event::marks`). The sync core
/// dates changes its own way; this is only what an older Hopper sends and
/// what is still read from it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Stamp {
    pub seq: u64,
    pub by: String,
}

// ---- pack version wording -----------------------------------------------------------------

/// The pack version a file is, for the update button and notices: "13.6.0"
/// of "Fabulously.Optimized-13.6.0.zip". The file name first: a display
/// name like "13.6.0 for 26.1.2" ends in the Minecraft version, which
/// `short_version` would take.
pub fn version_name(pack: &str, file: &crate::cf::File) -> String {
    let from_file = crate::instance_ops::pack_version_label(&file.file_name);
    if !from_file.is_empty() && from_file.chars().any(|c| c.is_ascii_digit()) {
        return from_file;
    }
    crate::hosting::motd::short_version(pack, &file.display_name)
}

/// A pack version as the instance page's version tag shows it: "8.6" is
/// "v8.6", a name that doesn't start with a digit stays as it is.
pub fn version_tag(version: &str) -> String {
    if version.starts_with(|c: char| c.is_ascii_digit()) { format!("v{version}") } else { version.to_string() }
}

/// The button's version for a pack file ("v8.6"), the same words as the tag
/// under the pack's name. `None` when no version can be read from the file,
/// so the caller shows the file's full name.
pub fn update_label(pack: &str, file: &crate::cf::File) -> Option<String> {
    let v = version_name(pack, file);
    // "8-1.78.1" is the file name's pack number glued to the version: not clean.
    if v.is_empty() || !v.chars().any(|c| c.is_ascii_digit()) || v.contains(|c: char| c.is_whitespace() || c == '-' || c == '_') {
        return None;
    }
    Some(version_tag(&v))
}
