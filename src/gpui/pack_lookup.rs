//! Packs in loose folders (Paxi's resource packs, data packs) looked up on
//! CurseForge and Modrinth, with the version each is and a newer one for
//! the instance, kept for the session. The Resource packs and Data packs
//! tabs share it.

use crate::store::{Msg, Store};
use gpui_kit::*;
use hopper::cf;
use hopper::instance_ops::{self, PackFile, PackMatch, PackVersion};
use hopper::instances::Instance;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// What a pack file was found to be.
#[derive(Clone)]
pub struct Found {
    pub project: cf::Mod,
    pub version: PackVersion,
}


type Key = (PathBuf, u64, i64);

/// By (path as switched on, size, modified): `None` when neither site has
/// it. Turning a pack off renames it (size and date stay), so it keeps
/// what it was found to be.
static FOUND: Mutex<Option<HashMap<Key, Option<Found>>>> = Mutex::new(None);

fn key(p: &PackFile) -> Key {
    (instance_ops::enabled_path(&p.path), p.size, p.modified)
}

/// What `p` was found to be: `None` before it's looked up, `Some(None)`
/// when neither site has it.
pub fn found(p: &PackFile) -> Option<Option<Found>> {
    FOUND.lock().ok()?.as_ref()?.get(&key(p)).cloned()
}

/// The project the pack file at `path` (switched on or off) was found to
/// be, when it's project `id`.
pub fn found_project(path: &std::path::Path, id: u64) -> Option<cf::Mod> {
    let want = instance_ops::enabled_path(path);
    let g = FOUND.lock().ok()?;
    g.as_ref()?
        .iter()
        .find_map(|((p, _, _), f)| f.as_ref().filter(|f| *p == want && f.project.id == id).map(|f| f.project.clone()))
}

/// Packs looked up whose versions aren't checked yet, by key.
static UNCHECKED: Mutex<Option<HashMap<Key, PackMatch>>> = Mutex::new(None);

/// Looks up the packs not looked up yet. Blocking: run it in the
/// background, then [`check_versions`]. Returns the found projects'
/// icons, to fetch.
pub fn look_up(packs: &[PackFile], class: u32) -> Vec<String> {
    let want: Vec<&PackFile> = packs.iter().filter(|p| found(p).is_none()).collect();
    if want.is_empty() {
        return Vec::new();
    }
    let paths: Vec<PathBuf> = want.iter().map(|p| p.path.clone()).collect();
    let (hits, answered) = instance_ops::identify_packs(&paths, class);
    let mut icons = Vec::new();
    if let (Ok(mut g), Ok(mut u)) = (FOUND.lock(), UNCHECKED.lock()) {
        let (map, unchecked) = (g.get_or_insert_with(HashMap::new), u.get_or_insert_with(HashMap::new));
        for p in want {
            let hit = hits.get(&p.path).map(|m| {
                icons.extend(m.project.icon().map(str::to_string));
                unchecked.insert(key(p), m.clone());
                Found { project: m.project.clone(), version: PackVersion::default() }
            });
            // A miss is remembered only when both sites answered; else the
            // pack is looked up again next time it's shown.
            if hit.is_some() || answered {
                map.insert(key(p), hit);
            }
        }
    }
    icons
}

/// Checks the found packs among `packs` for their version and a newer
/// file. Blocking. Returns whether any was checked (to redraw).
pub fn check_versions(packs: &[PackFile], inst: &Instance) -> bool {
    let list: Vec<(PathBuf, PackMatch)> = {
        let Ok(mut u) = UNCHECKED.lock() else { return false };
        let Some(u) = u.as_mut() else { return false };
        packs.iter().filter_map(|p| u.remove(&key(p)).map(|m| (p.path.clone(), m))).collect()
    };
    if list.is_empty() {
        return false;
    }
    let versions = instance_ops::loose_pack_versions(inst, &list);
    if let Ok(mut g) = FOUND.lock()
        && let Some(map) = g.as_mut()
    {
        for p in packs {
            if let (Some(Some(f)), Some(v)) = (map.get_mut(&key(p)), versions.get(&p.path)) {
                f.version = v.clone();
            }
        }
    }
    true
}

/// Replaces `p` with its newer file in the same folder, as a job on the
/// instance (the old file goes to backups). A website-only file comes
/// through the download window and lands in that same folder.
pub fn update(store: &Entity<Store>, root: PathBuf, p: &PackFile, f: &Found, cx: &mut App) {
    let Some(file) = f.version.update.clone() else { return };
    let (old, project) = (p.path.clone(), f.project.clone());
    let title = format!("Updating {}", project.name);
    store.read(cx).spawn_instance_job(&root.clone(), title, move |job| {
        match instance_ops::update_pack(&root, &old, &project, &file, job)? {
            instance_ops::Placed::File(new) => {
                job.detail(format!("Now {}", new.file_name().unwrap_or_default().to_string_lossy()));
                Ok(Msg::InstanceChanged(root))
            }
            instance_ops::Placed::Pending(url) => {
                job.detail(format!("{}: getting it from the CurseForge website", project.name));
                Ok(Msg::Updated(root, Vec::new(), vec![url], None))
            }
        }
    });
}
