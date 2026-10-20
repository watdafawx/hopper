//! Packs in loose folders (Paxi's resource packs, data packs) looked up on
//! CurseForge and Modrinth, with the version each is and a newer one for
//! the instance, kept for the session. The Resource packs and Data packs
//! tabs share it.

use crate::store::{Msg, Store};
use gpui_kit::prelude::FluentBuilder as _;
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

impl Found {
    /// "3.1.0", or "3.1.0 → 3.2.0" with an update; empty when unknown.
    pub fn label(&self, p: &PackFile) -> (String, Option<String>) {
        let name = |f: &cf::File| instance_ops::pack_version_label(&f.file_name);
        let have = self
            .version
            .installed
            .as_ref()
            .map(name)
            .unwrap_or_else(|| instance_ops::pack_version_label(&p.path.file_name().unwrap_or_default().to_string_lossy()));
        match &self.version.update {
            Some(u) => {
                let (from, to) = hopper::install::version_change(
                    &self.version.installed.as_ref().map_or(String::new(), |f| f.file_name.clone()),
                    &u.file_name,
                );
                let strip = |s: String| s.trim_end_matches(".zip").to_string();
                (strip(from), Some(strip(to)))
            }
            None => (have, None),
        }
    }
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

/// The version a found pack is, with "→ newer" in the update color when
/// there is one, for the line under its name.
pub fn version_tag(f: &Found, p: &PackFile, cx: &App) -> Option<AnyElement> {
    use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex};
    let (have, to) = f.label(p);
    if have.is_empty() && to.is_none() {
        return None;
    }
    let theme = cx.theme();
    Some(
        h_flex()
            .flex_none()
            .gap_1()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(have)
            .when_some(to, |r, to| {
                r.child(Icon::new(gpui_kit::assets::IconName::ArrowRight).xsmall().text_color(theme.warning))
                    .child(div().text_color(theme.warning).font_weight(FontWeight::SEMIBOLD).child(to))
            })
            .into_any_element(),
    )
}

/// An Update button for a found pack with a newer file.
pub fn update_button(id: ElementId, store: &Entity<Store>, root: PathBuf, p: &PackFile, f: &Found) -> Option<AnyElement> {
    use gpui_kit::component::Sizable as _;
    use gpui_kit::component::button::Button;
    let new = f.version.update.as_ref()?;
    let tip = format!("Update to {}", new.display_name);
    let (store, p, f) = (store.clone(), p.clone(), f.clone());
    Some(
        crate::widgets::site_button(Button::new(id), f.project.modrinth.is_some())
            .xsmall()
            .icon(gpui_kit::assets::IconName::Download)
            .label("Update")
            .tooltip(tip)
            .on_click(move |_, _, cx| update(&store, root.clone(), &p, &f, cx))
            .into_any_element(),
    )
}
