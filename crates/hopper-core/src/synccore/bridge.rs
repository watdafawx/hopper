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
use super::local::key_of;
use super::model::{Key, Value};
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
}

pub struct InstFetch<'a> {
    root: PathBuf,
    job: &'a Job,
    /// Settings › Mod sources: files from GitHub or a link may be fetched.
    allow_sources: bool,
    /// The file's name on disk, by the group's file id.
    names: HashMap<String, String>,
    found: HashMap<String, Result<Resolved, String>>,
    /// The group the steps come from, for History.
    group: String,
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
            found: HashMap::new(),
            group: group.to_string(),
        };
        for s in plan.steps.iter().filter(|s| matches!(s.key, Key::Mod(_))) {
            if let Value::File { file, .. } = &s.from
                && let Some(a) = inst.addons.iter().find(|a| key_of(a) == s.key)
            {
                me.names.insert(file.clone(), base_name(&a.file_name));
            }
            let Value::File { file, .. } = &s.to else { continue };
            if me.found.contains_key(file) {
                continue;
            }
            let same = matches!(&s.from, Value::File { file: f, .. } if f == file);
            if same {
                continue;
            }
            let r = project_of(&s.key)
                .zip(file_number(file))
                .ok_or_else(|| "this mod's file isn't one Hopper can look up".to_string())
                .and_then(|(p, f)| {
                    let file = source::file(p, f).map_err(|e| format!("{e:#}"))?;
                    let project = source::get_mod(p).map_err(|e| format!("{e:#}"))?;
                    let from_pack = inst.addons.iter().any(|a| key_of(a) == s.key && a.from_pack);
                    Ok(Resolved { project, file, from_pack })
                });
            if let Ok(r) = &r {
                me.names.insert(file.clone(), base_name(&r.file.file_name));
            }
            me.found.insert(file.clone(), r);
        }
        me
    }

    /// Brings the instance's records in line with the files `apply::run`
    /// swapped: a removed mod leaves the list, a new file takes the old
    /// one's place (keeping its history), a turned-on or off one switches.
    /// Done under each step's author, so History names them.
    pub fn finish(&self, plan: &ApplyPlan, report: &ApplyReport) -> anyhow::Result<()> {
        let Some(mut inst) = instances::load(&self.root) else { return Ok(()) };
        let mut changed = false;
        for (step, r) in plan.steps.iter().zip(&report.results) {
            if r.outcome != Outcome::Done || !matches!(step.key, Key::Mod(_)) || !is_jar_step(step) {
                continue;
            }
            let who = crate::actor::Who::friend(&step.name, &step.by.author, &self.group, "");
            let _scope = crate::actor::begin(&self.root, Some(who));
            let at = inst.addons.iter().position(|a| key_of(a) == step.key);
            match &step.to {
                Value::Gone => {
                    if let Some(i) = at {
                        inst.addons.remove(i);
                        changed = true;
                    }
                }
                Value::File { file, on } => {
                    let same = matches!(&step.from, Value::File { file: f, .. } if f == file);
                    if same {
                        if let Some(i) = at {
                            inst.addons[i].enabled = *on;
                            crate::actor::claim_addon(&inst.addons[i]);
                            changed = true;
                        }
                    } else if let Some(Ok(r)) = self.found.get(file) {
                        let mut new = install::addon_from(&r.project, &r.file, r.from_pack);
                        if let Some(i) = at {
                            install::keep_history(&mut new, &inst.addons[i]);
                            inst.addons.remove(i);
                        }
                        new.enabled = *on;
                        crate::actor::claim_addon(&new);
                        inst.addons.push(new);
                        changed = true;
                    }
                }
                _ => {}
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
        if r.file.source.is_some() && !self.allow_sources {
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
        let who = crate::actor::Who::friend(&step.name, &step.by.author, &self.group, "");
        let _scope = crate::actor::begin(&self.root, Some(who));
        match &step.key {
            Key::Mod(_) => self.pack_mod(&mut inst, step),
            Key::Pack => self.pack(&mut inst, step),
            Key::Loader => {
                let Value::Text(l) = &step.to else { return Ok(()) };
                inst.loader = l.clone();
                inst.save()
            }
            Key::Game => {
                let Value::Text(v) = &step.to else { return Ok(()) };
                anyhow::bail!("{} moved the game to {v}. Change the Minecraft version in Versions to follow.", step.name)
            }
            // A lock has no file of its own yet: nothing to do here.
            Key::Lock(_) => Ok(()),
        }
    }
}

impl InstFetch<'_> {
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
                if crate::groupsync::put_back_pack_default(inst, project, self.job)? {
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
        if crate::modpack::update_group_pack(inst, project, &f, self.job)? {
            Ok(())
        } else {
            anyhow::bail!("Updating the pack needs you: pick what to keep in the update dialog")
        }
    }
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
