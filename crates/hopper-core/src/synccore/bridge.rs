//! The app's side of `apply`: a [`Fetch`] that knows Hopper's instances,
//! CurseForge and Modrinth (design §7). IO.
//!
//! `apply::run` swaps jars as files. An instance also keeps a record per
//! mod (project, file, name, on or off), so [`InstFetch`] resolves every
//! file it will fetch up front (`prepare`), does the steps that are not
//! plain jars through the existing installers (`apply_other`: a pack
//! version, a loader, a pack mod turned off or put back), and [`finish`]
//! brings the records in line with the files the swap moved. Nothing here
//! makes a sync record.
//!
//! [`finish`]: InstFetch::finish

use super::apply::{ApplyReport, Fetch, FetchError, Outcome};
use super::link::LinkRef;
use super::local::key_of;
use super::model::{Key, PackMode, Value, Why};
use super::plan::{ApplyPlan, Step};
use crate::instances::{self, Addon, Instance};
use crate::jobs::Job;
use crate::{cf, install, modrinth, source};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One file the plan will download, as the sites know it.
struct Resolved {
    project: cf::Mod,
    file: cf::File,
    /// The mod is the pack's own (a pack mod updated by a friend stays the
    /// pack's).
    from_pack: bool,
    /// The file is a link's (GitHub, Maven, a URL): the player or the setting
    /// already allowed it, or the gate would have held the step.
    from_link: bool,
}

pub struct InstFetch<'a> {
    root: PathBuf,
    job: &'a Job,
    /// Settings › Mod sources: files from GitHub or a link may be fetched.
    allow_sources: bool,
    /// The file's name on disk, by the group's file id.
    names: HashMap<String, String>,
    /// The entry of each file the plan sets aside, as the instance had it
    /// before the swap: the jar's History row names the mod from it (the entry
    /// itself is gone from the instance by the time the jar is filed).
    set_aside: HashMap<String, Addon>,
    found: HashMap<String, Result<Resolved, String>>,
    /// The group the steps come from, for History.
    group: String,
    /// Mods the plan puts in as files: their lock is set once the record
    /// exists ([`InstFetch::finish`]).
    adds: std::collections::HashSet<String>,
}

/// A mod key's project, in Hopper's numbers ("cf:3", "shader:cf:3",
/// "mr:AANobbMI").
pub fn project_of(key: &Key) -> Option<u64> {
    let Key::Mod(k) = key else { return None };
    let k = k.strip_prefix("shader:").or_else(|| k.strip_prefix("rp:")).or_else(|| k.strip_prefix("dp:")).unwrap_or(k);
    match k.split_once(':')? {
        ("cf", n) => n.parse().ok(),
        ("mr", id) => Some(modrinth::num_id(id)),
        _ => None,
    }
}

/// The file of a link mod from the address its record carries: the source is
/// told to the lookups (so updates and the mod's page work on this PC too),
/// the file is the one named, its download is the address or, when the record
/// has none, what the source lists under that name. The sha256 the record
/// carries is what the download is checked against.
fn resolve_link(link: &LinkRef, who: &str) -> Result<Resolved, String> {
    let src = link.source();
    let id = crate::sources::src_id(&src.key());
    let stem = link.file_name.trim_end_matches(".jar").to_string();
    crate::sources::remember(id, &src, &stem);
    let project = crate::sources::mod_for(id, &src, &stem);
    let mut file = cf::File {
        id: (crate::sources::src_id(&format!("{}|{}|{}", link.seen, link.file_name, link.sha256)) & !(1 << 62)) | 1,
        mod_id: id,
        display_name: link.seen.clone(),
        file_name: link.file_name.clone(),
        release_type: 1,
        download_url: (!link.download.is_empty()).then(|| link.download.clone()),
        source: Some(Box::new(crate::sources::SrcFile { source: src, digest: link.sha256.clone(), ..Default::default() })),
        ..Default::default()
    };
    if file.download_url.is_none() {
        let listed = source::files(id, "", cf::Loader::Any).map_err(|e| format!("{who}'s link couldn't be read: {e:#}"))?;
        let found = listed
            .into_iter()
            .find(|f| f.file_name == link.file_name && (link.sha256.is_empty() || f.source.as_ref().is_some_and(|s| s.digest == link.sha256)))
            .ok_or_else(|| format!("{} isn't listed at the link any more", link.file_name))?;
        file.download_url = found.download_url;
    }
    Ok(Resolved { project, file, from_pack: false, from_link: true })
}

/// A group file id's number ("cf:11", "mr:<version>").
fn file_number(file: &str) -> Option<u64> {
    match file.split_once(':')? {
        ("cf", n) => n.parse().ok(),
        ("mr", id) => Some(modrinth::num_id(id)),
        _ => None,
    }
}

fn base_name(name: &str) -> String {
    name.trim_end_matches(".disabled").to_string()
}

impl<'a> InstFetch<'a> {
    /// Looks up every file the plan downloads (this reaches the mod sites,
    /// so it runs on the job's thread). A file that can't be looked up
    /// fails its own step later, in [`Fetch::fetch`].
    pub fn prepare(inst: &Instance, plan: &ApplyPlan, allow_sources: bool, group: &str, job: &'a Job) -> InstFetch<'a> {
        let mut me = InstFetch {
            root: inst.root.clone(),
            job,
            allow_sources,
            names: HashMap::new(),
            set_aside: HashMap::new(),
            found: HashMap::new(),
            group: group.to_string(),
            adds: plan.steps.iter().filter(|s| matches!(s.to, Value::File { .. })).filter_map(|s| if let Key::Mod(k) = &s.key { Some(k.clone()) } else { None }).collect(),
        };
        for s in plan.steps.iter().filter(|s| matches!(s.key, Key::Mod(_))) {
            if let Value::File { file, .. } = &s.from
                && let Some(a) = inst.addons.iter().find(|a| key_of(a) == s.key)
            {
                me.names.insert(file.clone(), base_name(&a.file_name));
                me.set_aside.insert(file.clone(), a.clone());
            }
            let Value::File { file, .. } = &s.to else { continue };
            if me.found.contains_key(file) {
                continue;
            }
            let same = matches!(&s.from, Value::File { file: f, .. } if f == file);
            if same {
                continue;
            }
            let r = match LinkRef::parse(file) {
                Some(link) => resolve_link(&link, &s.name),
                None => project_of(&s.key)
                    .zip(file_number(file))
                    .ok_or_else(|| "this mod's file isn't one Hopper can look up".to_string())
                    .and_then(|(p, f)| {
                        let file = source::file(p, f).map_err(|e| format!("{e:#}"))?;
                        let project = source::get_mod(p).map_err(|e| format!("{e:#}"))?;
                        let from_pack = inst.addons.iter().any(|a| key_of(a) == s.key && a.from_pack);
                        Ok(Resolved { project, file, from_pack, from_link: false })
                    }),
            };
            if let Ok(r) = &r {
                me.names.insert(file.clone(), base_name(&r.file.file_name));
            }
            me.found.insert(file.clone(), r);
        }
        me
    }

    /// Who a step's change is in History: the member who made it. Never
    /// "you": a change tagged as the player's own is read back as a record.
    fn who_of(&self, step: &Step) -> crate::actor::Who {
        crate::actor::Who::friend(&step.name, &step.by.author, &self.group, "")
    }

    /// Brings the instance's records in line with the files `apply::run`
    /// swapped: a removed mod leaves the list, a new file takes the old
    /// one's place (keeping its history), a turned-on or off one switches.
    /// Done under each step's author, so History names them.
    pub fn finish(&self, plan: &ApplyPlan, report: &ApplyReport) -> anyhow::Result<()> {
        let Some(mut inst) = instances::load(&self.root) else { return Ok(()) };
        let mut changed = false;
        // One scope for the whole pass, which stays open over the save below:
        // the save reads who each changed mod belongs to. A scope per step
        // ended before the save, so every friend's change was read as this
        // player's own and came back as a record of theirs (it also settled
        // a clash silently, as a follow-up that covered both sides).
        let _scope = crate::actor::begin(&self.root, None);
        let friend = |step: &Step| self.who_of(step);
        let claim = |a: &Addon, who: &crate::actor::Who| crate::actor::claim(&crate::groupkey::addon_key(a), Some(&a.file_name), who);
        for step in &plan.steps {
            let Some(r) = report.results.iter().find(|r| r.key == step.key && r.by == step.by) else { continue };
            if r.outcome != Outcome::Done || !matches!(step.key, Key::Mod(_)) || !is_jar_step(step) {
                continue;
            }
            let who = friend(step);
            let at = inst.addons.iter().position(|a| key_of(a) == step.key);
            match &step.to {
                Value::Gone => {
                    // The record may be gone already (reading the folder drops
                    // a mod whose jar `apply::run` moved): the jar still goes
                    // to History.
                    self.file_in_history(step, &who);
                    if let Some(i) = at {
                        claim(&inst.addons[i], &who);
                        inst.addons.remove(i);
                        changed = true;
                    }
                }
                Value::File { file, on } => {
                    let same = matches!(&step.from, Value::File { file: f, .. } if f == file);
                    if same {
                        if let Some(i) = at {
                            inst.addons[i].enabled = *on;
                            claim(&inst.addons[i], &who);
                            changed = true;
                        }
                    } else if let Some(Ok(r)) = self.found.get(file) {
                        let mut new = install::addon_from(&r.project, &r.file, r.from_pack);
                        self.file_in_history(step, &who);
                        if let Some(i) = at {
                            install::keep_history(&mut new, &inst.addons[i]);
                            claim(&inst.addons[i], &who);
                            inst.addons.remove(i);
                        }
                        new.enabled = *on;
                        claim(&new, &who);
                        inst.addons.push(new);
                        changed = true;
                    }
                }
                _ => {}
            }
        }
        // Locks go on after the records they belong to are there.
        for step in &plan.steps {
            let Key::Lock(k) = &step.key else { continue };
            let done = report.results.iter().any(|r| r.key == step.key && r.by == step.by && r.outcome == Outcome::Done);
            if !done {
                continue;
            }
            let who = friend(step);
            let key = Key::Mod(k.clone());
            if let Some(a) = inst.addons.iter_mut().find(|a| key_of(a) == key)
                && super::local::set_lock(a, &step.to)
            {
                claim(a, &who);
                changed = true;
            }
        }
        if changed {
            inst.save()?;
        }
        Ok(())
    }
}

/// A step `apply::run` swaps as files (a plain jar on both sides).
fn is_jar_step(step: &Step) -> bool {
    let plain = |v: &Value| matches!(v, Value::Gone | Value::File { .. });
    plain(&step.from) && plain(&step.to)
}

impl Fetch for InstFetch<'_> {
    fn fetch(&self, file: &str, into: &Path) -> anyhow::Result<()> {
        let r = self.found.get(file).ok_or_else(|| anyhow::anyhow!("this file wasn't looked up"))?;
        let r = r.as_ref().map_err(|e| anyhow::anyhow!("{e}"))?;
        if r.file.source.is_some() && !self.allow_sources && !r.from_link {
            anyhow::bail!("{} comes from a link you haven't allowed (Settings > Mod sources)", r.project.name);
        }
        install::download_staged(&self.root, &r.project, &r.file, r.from_pack, into, self.job)
    }

    fn file_name(&self, key: &Key, file: &str) -> String {
        let _ = key;
        self.names.get(file).cloned().unwrap_or_else(|| format!("{}.jar", file.replace(':', "_")))
    }

    fn apply_other(&self, step: &Step) -> anyhow::Result<()> {
        let mut inst = instances::load(&self.root).ok_or_else(|| anyhow::anyhow!("the instance couldn't be read"))?;
        let who = self.who_of(step);
        let _scope = crate::actor::begin(&self.root, Some(who));
        // Everything the installers change for this step is the friend's. A
        // scope alone reads any mod it doesn't claim as "yours, made
        // meanwhile", so the installer's save came back as this player's own
        // record (user report 2026-11-26: a friend's apply of five FTB mod
        // updates wrote five records of theirs putting the mods back to the
        // pack's file, which then rolled the update back for everyone; and
        // putting a pack mod back to the pack's file wrote a record of the
        // old file under this player's name). The instance is read afresh
        // above, so the save holds only this step's change.
        let _all = crate::actor::own_all();
        match &step.key {
            Key::Mod(_) => self.pack_mod(&mut inst, step),
            Key::Pack => self.pack(&mut inst, step),
            Key::Loader => {
                let Value::Text(l) = &step.to else { return Ok(()) };
                // Only the version of one kind of loader is followed: another
                // kind brings other mods and settings, so the player decides.
                if !inst.loader.is_empty() && loader_kind(l) != loader_kind(&inst.loader) {
                    anyhow::bail!("{} switched to {}. Hopper doesn't follow a change of loader, so this copy stays on {}.", step.name, crate::groupkey::loader_label(l), crate::groupkey::loader_label(&inst.loader));
                }
                if inst.loader == *l {
                    return Ok(());
                }
                inst.loader = l.clone();
                inst.save()
            }
            // The Minecraft version of a plain instance, changed like in the
            // instance's settings: saved here, the loader for it is installed
            // by the app once this is done (the game isn't running, the plan
            // waits for that). A modpack sets its own version.
            Key::Game => {
                let Value::Text(v) = &step.to else { return Ok(()) };
                if inst.pack.is_some() {
                    anyhow::bail!("{} moved the game to {v}, but this copy follows a modpack, which sets its own Minecraft version.", step.name);
                }
                if inst.game_version == *v {
                    return Ok(());
                }
                inst.game_version = v.clone();
                inst.save()
            }
            // A lock is a mark on the mod's record. A mod that comes with
            // this plan gets its lock in `finish`, once the record is there.
            Key::Lock(k) => {
                let key = Key::Mod(k.clone());
                match inst.addons.iter_mut().find(|a| key_of(a) == key) {
                    Some(a) => {
                        if super::local::set_lock(a, &step.to) {
                            inst.save()?;
                        }
                        Ok(())
                    }
                    None if self.adds.contains(k) || step.to == Value::Gone => Ok(()),
                    None => anyhow::bail!("the mod {} locked isn't in this instance", step.name),
                }
            }
        }
    }
}

impl InstFetch<'_> {
    /// The old jar a swap set aside, filed as a History entry under the
    /// step's author: without it a friend's removal or update left nothing in
    /// History on this PC.
    fn file_in_history(&self, step: &Step, who: &crate::actor::Who) {
        let Value::File { file, on } = &step.from else { return };
        let name = format!("{}{}", self.file_name(&step.key, file), if *on { "" } else { ".disabled" });
        let base = self.root.join(crate::sync::BACKUP_DIR);
        let flat = base.join(super::apply::set_aside_name(&name, &step.by));
        if !flat.is_file() {
            return;
        }
        let dir = base.join(crate::time::now().to_string());
        let dest = dir.join(&name);
        if dest.exists() || std::fs::create_dir_all(&dir).is_err() || std::fs::rename(&flat, &dest).is_err() {
            return;
        }
        crate::actor::claim("", Some(&name), who);
        crate::backups::record(&dir, &dest, &format!("{}/{name}", super::apply::folder_of(&step.key)));
        // Without this the row reads as a bare file ("Bob removed
        // appleskin-fabric-mc26.2-3.0.10.jar, no download source").
        if let Some(a) = self.set_aside.get(file) {
            crate::backups::note_addon(&dest, a);
        }
    }

    /// A mod a modpack ships, turned off, removed, put back; or a jar
    /// replacing a pack mod.
    fn pack_mod(&self, inst: &mut Instance, step: &Step) -> anyhow::Result<()> {
        let at = inst.addons.iter().position(|a| key_of(a) == step.key);
        match (&step.from, &step.to) {
            (_, Value::PackOff) => match at {
                Some(i) => install::set_enabled_later(inst, i, false),
                None => Ok(()),
            },
            (_, Value::Gone | Value::PackRemoved) => match at {
                Some(i) => install::remove(inst, i),
                None => Ok(()),
            },
            (Value::PackOff, Value::PackDefault) => match at {
                Some(i) => install::set_enabled_later(inst, i, true),
                None => Ok(()),
            },
            (_, Value::PackDefault) => {
                let project = project_of(&step.key).ok_or_else(|| anyhow::anyhow!("not a mod Hopper can look up"))?;
                if crate::groupkey::put_back_pack_default(inst, project, self.job)? {
                    Ok(())
                } else {
                    anyhow::bail!("the modpack's own file of {} isn't known here", step.name)
                }
            }
            (_, Value::File { file, on }) => {
                let project = project_of(&step.key).ok_or_else(|| anyhow::anyhow!("not a mod Hopper can look up"))?;
                let n = file_number(file).ok_or_else(|| anyhow::anyhow!("not a file Hopper can look up"))?;
                match install::install_file_outcome(inst, project, n, Some(*on), self.job)? {
                    install::FileInstall::Installed(_) => Ok(()),
                    install::FileInstall::Pending(_) => Err(FetchError::Blocked.into()),
                }
            }
            _ => Ok(()),
        }
    }

    /// The group's pack version, the way the update dialog's "Keep my
    /// changes" does it. A choice the player has to make waits for them.
    fn pack(&self, inst: &mut Instance, step: &Step) -> anyhow::Result<()> {
        let Value::Text(v) = &step.to else { return Ok(()) };
        let (project, file) = parse_pack(v).ok_or_else(|| anyhow::anyhow!("{} moved the pack to a version Hopper can't read", step.name))?;
        let f = source::file(project, file)?;
        // What the record stated, as it applies to a friend's PC: a clean
        // update here never cleans theirs unless it said "for everyone".
        let mode = match &step.why {
            Why::PackUpdate { mode } => *mode,
            _ => PackMode::default(),
        };
        if crate::modpack::update_group_pack(inst, project, &f, &mode, self.job)? {
            Ok(())
        } else {
            anyhow::bail!("Updating the pack needs you: pick what to keep in the update dialog")
        }
    }
}

/// "neoforge" of "neoforge-21.1.256".
fn loader_kind(loader: &str) -> String {
    loader.split_once('-').map_or(loader, |(k, _)| k).to_ascii_lowercase()
}

/// A pack version as the group names it, in Hopper's numbers.
pub fn parse_pack(v: &str) -> Option<(u64, u64)> {
    let (site, rest) = v.split_once(':')?;
    let (p, f) = rest.split_once('/')?;
    match site {
        "cf" => Some((p.parse().ok()?, f.parse().ok()?)),
        "mr" => Some((modrinth::num_id(p), modrinth::num_id(f))),
        _ => None,
    }
}

/// The addon of `key` in `inst`, if any (for callers that list what a
/// step touched).
pub fn addon_of<'a>(inst: &'a Instance, key: &Key) -> Option<&'a Addon> {
    inst.addons.iter().find(|a| key_of(a) == *key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_files_read_back_as_numbers() {
        assert_eq!(project_of(&Key::Mod("cf:3".into())), Some(3));
        assert_eq!(project_of(&Key::Mod("shader:cf:5".into())), Some(5));
        assert_eq!(project_of(&Key::Mod("src:a.jar".into())), None);
        assert_eq!(project_of(&Key::Loader), None);
        assert_eq!(file_number("cf:11"), Some(11));
        assert_eq!(file_number("a.jar"), None);
        assert_eq!(parse_pack("cf:100/8600"), Some((100, 8600)));
        assert_eq!(parse_pack("zip"), None);
    }

    /// A friend's change that `finish` writes into the instance must not
    /// come back as a record of this player's own (live pass #3: every apply
    /// made a "Bob" record, and one of them settled a clash in silence).
    #[test]
    fn a_friends_change_written_by_finish_makes_no_record_of_yours() {
        use super::super::model::Dot;
        crate::actor::record_in_tests();
        let root = std::env::temp_dir().join(format!("hopper-bridge-finish-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("mods")).unwrap();
        std::fs::write(root.join("mods/a.jar"), "a").unwrap();
        std::fs::write(root.join("mods/b.jar"), "b").unwrap();
        let v = serde_json::json!({"name": "T", "format": instances::FORMAT, "addons": [
            {"project_id": 5, "file_name": "a.jar", "name": "A", "enabled": true},
            {"project_id": 6, "file_name": "b.jar", "name": "B", "enabled": true}
        ]});
        std::fs::write(root.join(instances::HOPPER_FILE), serde_json::to_vec(&v).unwrap()).unwrap();
        super::super::local::watch(&root);
        let jobs = crate::jobs::Jobs::default();
        let job = jobs.start("t");
        let fetch = InstFetch { root: root.clone(), job: &job, allow_sources: true, names: HashMap::new(), set_aside: HashMap::new(), found: HashMap::new(), group: "g".into(), adds: Default::default() };
        let by = Dot { author: "mia".into(), n: 1 };
        let step = |key: &str, from: Value, to: Value| Step { key: Key::Mod(key.into()), from, to, by: by.clone(), name: "Mia".into(), why: Why::Player };
        let on = |f: &str| Value::File { file: f.into(), on: true };
        let off = |f: &str| Value::File { file: f.into(), on: false };
        // Mia turned a off and removed b.
        let plan = ApplyPlan { steps: vec![step("cf:5", on("cf:50"), off("cf:50")), step("cf:6", on("cf:60"), Value::Gone)], ..Default::default() };
        let report = ApplyReport { results: plan.steps.iter().map(|s| super::super::apply::StepResult { key: s.key.clone(), by: s.by.clone(), outcome: Outcome::Done }).collect() };
        fetch.finish(&plan, &report).unwrap();
        let inst = instances::load(&root).unwrap();
        assert_eq!(inst.addons.len(), 1);
        assert!(!inst.addons[0].enabled);
        let own = super::super::local::take(&root);
        assert!(own.is_empty(), "a friend's change was read as the player's own: {own:?}");
        super::super::local::unwatch(&root);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The installers' saves in a friend's step are the friend's, never this
    /// player's own (user report 2026-11-26: applying a friend's update of
    /// five pack mods wrote five records of the receiving player's, which
    /// rolled the update back for the whole group). A lock and a pack mod
    /// turned off go through the same scope as a pack mod's new file and need
    /// no download.
    #[test]
    fn a_friends_step_run_by_the_installers_makes_no_record_of_yours() {
        use super::super::model::Dot;
        crate::actor::record_in_tests();
        let root = std::env::temp_dir().join(format!("hopper-bridge-other-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("mods")).unwrap();
        std::fs::write(root.join("mods/a.jar"), "a").unwrap();
        std::fs::write(root.join("mods/b.jar"), "b").unwrap();
        let v = serde_json::json!({"name": "T", "format": instances::FORMAT, "addons": [
            {"project_id": 5, "file_id": 50, "file_name": "a.jar", "name": "A", "enabled": true, "from_pack": true},
            {"project_id": 6, "file_id": 60, "file_name": "b.jar", "name": "B", "enabled": true}
        ]});
        std::fs::write(root.join(instances::HOPPER_FILE), serde_json::to_vec(&v).unwrap()).unwrap();
        super::super::local::watch(&root);
        let _ = super::super::local::take(&root);
        let jobs = crate::jobs::Jobs::default();
        let job = jobs.start("t");
        let fetch = InstFetch { root: root.clone(), job: &job, allow_sources: true, names: HashMap::new(), set_aside: HashMap::new(), found: HashMap::new(), group: "g".into(), adds: Default::default() };
        let by = Dot { author: "mia".into(), n: 1 };
        let step = |key: Key, from: Value, to: Value| Step { key, from, to, by: by.clone(), name: "Mia".into(), why: Why::Player };
        fetch.apply_other(&step(Key::Mod("cf:5".into()), Value::PackDefault, Value::PackOff)).unwrap();
        fetch.apply_other(&step(Key::Lock("cf:6".into()), Value::Gone, Value::Text("all".into()))).unwrap();
        let inst = instances::load(&root).unwrap();
        assert!(!inst.addons.iter().find(|a| a.project_id == 5).unwrap().enabled, "the pack mod was turned off");
        assert!(inst.addons.iter().find(|a| a.project_id == 6).unwrap().locked, "the lock is on");
        let own = super::super::local::take(&root);
        assert!(own.is_empty(), "a friend's step was read as the player's own: {own:?}");
        super::super::local::unwatch(&root);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_friends_removal_shows_in_history_under_their_name() {
        use super::super::model::Dot;
        let root = std::env::temp_dir().join(format!("hopper-bridge-hist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".hopper-backups")).unwrap();
        std::fs::create_dir_all(root.join("mods")).unwrap();
        // Reading the folder already dropped the mod's record (its jar was moved).
        let v = serde_json::json!({"name": "T", "format": instances::FORMAT, "addons": []});
        std::fs::write(root.join(instances::HOPPER_FILE), serde_json::to_vec(&v).unwrap()).unwrap();
        let by = Dot { author: "mia".into(), n: 1 };
        // What `apply::run` did before `finish`: the jar is set aside flat.
        std::fs::write(root.join(".hopper-backups").join(super::super::apply::set_aside_name("b.jar", &by)), "b").unwrap();
        let jobs = crate::jobs::Jobs::default();
        let job = jobs.start("t");
        let mut names = HashMap::new();
        names.insert("cf:60".to_string(), "b.jar".to_string());
        let fetch = InstFetch { root: root.clone(), job: &job, allow_sources: true, names, set_aside: HashMap::new(), found: HashMap::new(), group: "g".into(), adds: Default::default() };
        let step = Step { key: Key::Mod("cf:6".into()), from: Value::File { file: "cf:60".into(), on: true }, to: Value::Gone, by: by.clone(), name: "Mia".into(), why: Why::Player };
        let plan = ApplyPlan { steps: vec![step.clone()], ..Default::default() };
        let report = ApplyReport { results: vec![super::super::apply::StepResult { key: step.key, by, outcome: Outcome::Done }] };
        fetch.finish(&plan, &report).unwrap();
        assert!(!root.join(".hopper-backups").join("b.jar.mia-1").exists(), "the jar stayed loose, outside History");
        let sets = crate::backups::history(&root);
        let file = sets.iter().flat_map(|s| s.files.iter()).find(|f| f.name == "b.jar").expect("the removal is in History");
        assert_eq!(file.who.as_ref().map(|w| w.name.as_str()), Some("Mia"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The row of a friend's removal or roll back names the mod and its site,
    /// like the row of an add does, not only the jar (seen: "Bob removed
    /// appleskin-fabric-mc26.2-3.0.10.jar · No download source").
    #[test]
    fn a_friends_removal_in_history_names_the_mod() {
        use super::super::model::Dot;
        let root = std::env::temp_dir().join(format!("hopper-bridge-hist-name-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".hopper-backups")).unwrap();
        std::fs::create_dir_all(root.join("mods")).unwrap();
        let v = serde_json::json!({"name": "T", "format": instances::FORMAT, "addons": []});
        std::fs::write(root.join(instances::HOPPER_FILE), serde_json::to_vec(&v).unwrap()).unwrap();
        let by = Dot { author: "bob".into(), n: 1 };
        std::fs::write(root.join(".hopper-backups").join(super::super::apply::set_aside_name("appleskin-fabric-mc26.2-3.0.10.jar", &by)), "x").unwrap();
        let jobs = crate::jobs::Jobs::default();
        let job = jobs.start("t");
        let jar = Addon { project_id: 7, file_id: 70, file_name: "appleskin-fabric-mc26.2-3.0.10.jar".into(), name: "AppleSkin".into(), mr_version: "AAAA".into(), enabled: true, ..Default::default() };
        let (mut names, mut set_aside) = (HashMap::new(), HashMap::new());
        names.insert("mr:AAAA".to_string(), jar.file_name.clone());
        set_aside.insert("mr:AAAA".to_string(), jar);
        let fetch = InstFetch { root: root.clone(), job: &job, allow_sources: true, names, set_aside, found: HashMap::new(), group: "g".into(), adds: Default::default() };
        let step = Step { key: Key::Mod("mr:7".into()), from: Value::File { file: "mr:AAAA".into(), on: true }, to: Value::Gone, by: by.clone(), name: "Bob".into(), why: Why::Player };
        let plan = ApplyPlan { steps: vec![step.clone()], ..Default::default() };
        let report = ApplyReport { results: vec![super::super::apply::StepResult { key: step.key, by, outcome: Outcome::Done }] };
        fetch.finish(&plan, &report).unwrap();
        let sets = crate::backups::history(&root);
        let file = sets.iter().flat_map(|s| s.files.iter()).find(|f| f.name == "appleskin-fabric-mc26.2-3.0.10.jar").expect("the removal is in History");
        assert_eq!(file.title(), "AppleSkin");
        assert_eq!(file.site(), "Modrinth");
        assert!(!file.detail().contains("No download source"), "{}", file.detail());
        assert_eq!(file.who.as_ref().map(|w| w.name.as_str()), Some("Bob"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_plain_jars_are_swapped_as_files() {
        let f = |v: Value, w: Value| Step {
            key: Key::Mod("cf:1".into()),
            from: v,
            to: w,
            by: super::super::model::Dot { author: "a".into(), n: 1 },
            name: "Mia".into(),
            why: super::super::model::Why::Player,
        };
        let on = Value::File { file: "cf:2".into(), on: true };
        assert!(is_jar_step(&f(Value::Gone, on.clone())));
        assert!(is_jar_step(&f(on.clone(), Value::Gone)));
        assert!(!is_jar_step(&f(Value::PackDefault, on)));
        assert!(!is_jar_step(&f(Value::Gone, Value::PackOff)));
    }
}
