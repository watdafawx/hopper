//! Required mods a friends group's pack lacks (`hopper::groupdeps`).
//!
//! Group sync carries the mods members added, so a mod that needs another
//! mod nobody added leaves the pack one short: it can't start, and every PC
//! says "In step". Two things here:
//!
//! - every PC reads its group copies' jars (off the UI thread, when the mods
//!   folder changed) and the sync status shows "Missing a required mod"
//!   instead of "In step" while one is lacking;
//! - after the person adds or updates a mod in a group copy, what that mod
//!   needs and the pack lacks is asked about once (a dialog: add it to the
//!   group too). The ask is only for the jars that add brought in, so a
//!   merge of a friend's change never asks here.

use super::{Msg, Store, StoreEvent};
use gpui_kit::*;
use hopper::groupdeps::{self, Need};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// What the store keeps about it.
#[derive(Default)]
pub struct State {
    /// The last read per group copy (its local instance id).
    list: HashMap<String, Read>,
    /// Group copies being read now; a read asked for meanwhile runs after
    /// (with the jars to ask about, when one asked).
    reading: HashMap<String, Option<HashSet<String>>>,
    /// What was asked about (`groupdeps::key`), per group copy: a "Not now"
    /// isn't asked again.
    told: HashMap<String, HashSet<String>>,
    /// Project name, id and icon of mod ids looked up before.
    names: HashMap<String, (String, u64, String)>,
}

struct Read {
    mtime: Option<SystemTime>,
    needs: Vec<Need>,
}

/// A group copy's jars read: the answer to [`Store::want_group_needs`].
pub struct Scanned {
    local: String,
    root: PathBuf,
    mtime: Option<SystemTime>,
    needs: Vec<Need>,
    /// Names looked up this time: mod id -> (name, project, icon).
    found: Vec<(String, (String, u64, String))>,
    /// The jars to ask about were these (an add asked), else none.
    asked: Option<HashSet<String>>,
}

/// The mods an add or update brought in that need what the pack lacks, and
/// what to say about them.
#[derive(Clone, Debug)]
pub struct Ask {
    pub root: PathBuf,
    pub group: String,
    /// (the need, its project id, its icon url).
    pub needs: Vec<(Need, u64, String)>,
}

impl Store {
    /// Reads the group copy at `root`'s jars for required mods the pack
    /// lacks, unless they were read since the mods folder last changed.
    /// Nothing for an instance that isn't a group copy.
    pub(super) fn want_group_needs(&mut self, root: &Path) {
        self.read_group_needs(root, None);
    }

    /// [`Self::want_group_needs`] after an add or update: `before` is the
    /// jars the mods folder had before it. What the new jars need and the
    /// pack lacks is asked about.
    pub(super) fn ask_group_needs(&mut self, root: &Path, before: HashSet<String>) {
        self.read_group_needs(root, Some(before));
    }

    fn read_group_needs(&mut self, root: &Path, asked: Option<HashSet<String>>) {
        if self.group_preview.is_some() || super::server_mods::remote(root).is_some() {
            return;
        }
        let Some(local) = self.group_copy(root).map(|c| c.local.clone()) else { return };
        let Some(inst) = self.instance(root).cloned() else { return };
        let mods = inst.mods_dir();
        let mtime = std::fs::metadata(&mods).and_then(|m| m.modified()).ok();
        if asked.is_none() && self.group_needs.list.get(&local).is_some_and(|r| r.mtime == mtime) {
            return;
        }
        if let Some(waiting) = self.group_needs.reading.get_mut(&local) {
            // One read at a time; the newer ask wins.
            if asked.is_some() {
                *waiting = asked;
            }
            return;
        }
        self.group_needs.reading.insert(local.clone(), None);
        let names = self.group_needs.names.clone();
        let root = root.to_path_buf();
        self.spawn(move || {
            let needs = groupdeps::needs(&hopper::deps::missing(&mods));
            // Names for the ids, once each: the pack's own page for the id,
            // the way the Mods tab's notice finds it.
            let mut found: Vec<(String, (String, u64, String))> = Vec::new();
            let mut out = needs;
            for n in &mut out {
                if !names.contains_key(&n.id) && !found.iter().any(|(id, _)| *id == n.id) {
                    let p = hopper::install::project_for_mod_id(&inst, &n.id).ok().flatten();
                    let row = p.map(|m| (m.name.clone(), m.id, m.icon().unwrap_or_default().to_string())).unwrap_or_else(|| (n.id.clone(), 0, String::new()));
                    found.push((n.id.clone(), row));
                }
                let known = names.get(&n.id).or_else(|| found.iter().find(|(id, _)| *id == n.id).map(|(_, r)| r));
                if let Some((name, _, _)) = known {
                    n.name = name.clone();
                }
            }
            Msg::GroupNeeds(Box::new(Scanned { local, root, mtime, needs: out, found, asked }))
        });
    }

    pub(super) fn on_group_needs(&mut self, s: Scanned, cx: &mut Context<Self>) {
        let Scanned { local, root, mtime, needs, found, asked } = s;
        for (id, row) in found {
            self.group_needs.names.insert(id, row);
        }
        let again = self.group_needs.reading.remove(&local).flatten();
        let told = self.group_needs.told.entry(local.clone()).or_default();
        let fresh = match &asked {
            Some(before) => groupdeps::fresh(before, &needs, told),
            None => Vec::new(),
        };
        for n in &fresh {
            for (_, file) in &n.by {
                told.insert(groupdeps::key(file, &n.id));
            }
        }
        self.group_needs.list.insert(local.clone(), Read { mtime, needs });
        if !fresh.is_empty() {
            let group = self.group_copy(&root).map(|c| self.group_name(&c.group)).unwrap_or_else(|| "the group".into());
            let needs = fresh
                .into_iter()
                .map(|n| {
                    let (_, project, icon) = self.group_needs.names.get(&n.id).cloned().unwrap_or_default();
                    (n, project, icon)
                })
                .collect();
            cx.emit(StoreEvent::GroupNeeds(Box::new(Ask { root: root.clone(), group, needs })));
        }
        // An add came in while this was being read.
        if again.is_some() {
            self.read_group_needs(&root, again);
        }
        cx.notify();
    }

    /// The required mods the group copy at `root` lacks (empty when it
    /// isn't a group copy or hasn't been read yet).
    pub fn group_needs(&self, root: &Path) -> &[Need] {
        let Some(c) = self.group_copy(root) else { return &[] };
        self.group_needs.list.get(&c.local).map_or(&[], |r| r.needs.as_slice())
    }

    /// The project behind a needed mod id once it was looked up: (project id,
    /// icon url). The id is 0 when no project was found for it.
    pub fn group_need_project(&self, id: &str) -> Option<(u64, String)> {
        self.group_needs.names.get(id).map(|(_, project, icon)| (*project, icon.clone()))
    }

    /// `--dialog group-sync --state needs-mod`: the made-up copy lacks these.
    ///
    /// With the preview's instance at `root`: the need is looked up on
    /// Modrinth for its real name and icon (this blocks, in the harness
    /// only), and the mod that needs it is the instance's first mod from a
    /// site, so its icon and page are real too.
    pub(super) fn preview_group_needs(&mut self, root: &Path, local: &str, mut needs: Vec<Need>) {
        let inst = self.instance(root).cloned();
        for n in &mut needs {
            let Some(inst) = &inst else { break };
            if let Some(a) = inst.addons.iter().find(|a| a.project_id != 0) {
                n.by = vec![(a.name.clone(), a.file_name.clone())];
            }
            if let Ok(Some(m)) = hopper::install::project_for_mod_id(inst, &n.id) {
                let icon = m.icon().unwrap_or_default().to_string();
                n.name = m.name.clone();
                if !icon.is_empty() {
                    self.want_image(&icon);
                }
                self.group_needs.names.insert(n.id.clone(), (m.name, m.id, icon));
            }
        }
        self.group_needs.list.insert(local.to_string(), Read { mtime: None, needs });
    }

    /// Reads every group copy not read yet (on start, and for a copy that
    /// just appeared).
    pub(super) fn seed_group_needs(&mut self) {
        let roots: Vec<PathBuf> = self
            .settings
            .group_copies
            .iter()
            .filter(|c| !self.group_needs.list.contains_key(&c.local) && !self.group_needs.reading.contains_key(&c.local))
            .filter_map(|c| self.instances.iter().find(|i| i.id == c.local).map(|i| i.root.clone()))
            .collect();
        for root in roots {
            self.want_group_needs(&root);
        }
    }

    /// "Add it": downloads the mods the group copy's pack lacks (by their
    /// ids), as a task. The pack's sync then carries them to the group.
    pub fn add_group_needs(&self, root: &Path, ids: Vec<String>) -> Option<u64> {
        self.install_mod_ids(root, ids)
    }

    /// Downloads mods jars ask for by id; one that can't be found is
    /// reported and the rest still go in.
    pub fn install_mod_ids(&self, root: &Path, ids: Vec<String>) -> Option<u64> {
        let mut inst = self.instance(root)?.clone();
        let title = match ids.as_slice() {
            [one] => format!("Installing {one}"),
            many => format!("Installing {} missing mods", many.len()),
        };
        Some(self.spawn_instance_job_id(root, title, move |job| {
            super::fresh(&mut inst);
            let mut got = 0;
            let before = super::Before::at(&inst.root);
            for id in &ids {
                match hopper::install::install_mod_id(&mut inst, id, job) {
                    Ok(added) => {
                        got += 1;
                        job.note(format!("Installed {}", added.join(", ")));
                    }
                    Err(e) => job.note(format!("{id}: {e:#}. Try Browse to find it.")),
                }
            }
            if got == 0 {
                anyhow::bail!("Couldn't find {} on Modrinth or CurseForge. Search for it in Browse.", ids.join(", "));
            }
            Ok(Msg::added(inst.root.clone(), &before, None))
        }))
    }
}
