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

/// By (path, size, modified): `None` when neither site has it.
static FOUND: Mutex<Option<HashMap<Key, Option<Found>>>> = Mutex::new(None);

fn key(p: &PackFile) -> Key {
    (p.path.clone(), p.size, p.modified)
}

/// What `p` was found to be: `None` before it's looked up, `Some(None)`
/// when neither site has it.
pub fn found(p: &PackFile) -> Option<Option<Found>> {
    FOUND.lock().ok()?.as_ref()?.get(&key(p)).cloned()
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
/// instance (the old file goes to backups).
pub fn update(store: &Entity<Store>, root: PathBuf, p: &PackFile, f: &Found, cx: &mut App) {
    let Some(file) = f.version.update.clone() else { return };
    let (old, project) = (p.path.clone(), f.project.clone());
    let title = format!("Updating {}", project.name);
    store.read(cx).spawn_instance_job(&root.clone(), title, move |job| {
        let new = instance_ops::update_pack(&root, &old, &project, &file, job)?;
        job.detail(format!("Now {}", new.file_name().unwrap_or_default().to_string_lossy()));
        Ok(Msg::InstanceChanged(root))
    });
}
