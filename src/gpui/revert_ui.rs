//! "Revert X to 1.6.3": the fix offered beside Turn off and Update when a
//! mod needs a newer mod or loader than the folder has. Shared by the Mods
//! tab's problem notice, the Logs crash card and a server's crash card;
//! the offers are worked out once per mods folder in the store.

use crate::store::Store;
use gpui_kit::*;
use hopper::deps::{Blocker, BlockerKind};
use hopper::instances::Addon;
use std::collections::HashMap;
use std::path::Path;

/// One mod an older version of it would fix.
#[derive(Clone, Debug)]
pub struct Offer {
    /// Its jar in the mods folder now, and its name.
    pub file_name: String,
    pub name: String,
    /// Its project (0 when Hopper can't place it): the "needs" mark goes on it.
    pub project_id: u64,
    pub need: hopper::revert::Need,
    /// What newer versions need, for the mark: "TMRV Updated 19.70.0.460+".
    pub needs: String,
    /// The other mod (or loader), for sentences.
    pub other: String,
    /// The needed mod's jar now, or the loader: the mark ends when it changes.
    pub have: String,
    /// The older copy in `.hopper-backups` that works, when there is one.
    pub backup: Option<hopper::crashdetect::Changed>,
}

impl Offer {
    /// The button's label and tooltip.
    pub fn label(&self) -> (String, String) {
        let (name, other) = (&self.name, &self.other);
        match &self.backup {
            Some(ch) if !ch.from.is_empty() => (
                format!("Revert {name} to {}", ch.from),
                format!("Puts back {}, the copy Hopper kept, which works with your {other}. The newer one goes to Backups, so you can undo this there.", ch.from),
            ),
            Some(_) => (
                format!("Revert {name}"),
                format!("Puts back the copy Hopper kept, which works with your {other}. The newer one goes to Backups, so you can undo this there."),
            ),
            None => (format!("Revert {name}"), format!("Installs the newest older version of {name} that works with your {other}.")),
        }
    }
}

/// What a blocked mod asks for that an older version of it may not: a newer
/// mod or loader.
fn need_of(b: &Blocker) -> Option<hopper::revert::Need> {
    match &b.kind {
        BlockerKind::WrongVersion { needs, have, .. } | BlockerKind::Loader { id: needs, have, .. } => {
            Some(hopper::revert::Need { id: needs.clone(), have: have.clone() })
        }
        _ => None,
    }
}

/// The lowest version a range accepts ("[19.70.0.460,)" → "19.70.0.460").
fn range_min(range: &str) -> Option<String> {
    let r = range.trim();
    let r = r
        .strip_prefix(['[', '('])
        .map(|r| r.split(',').next().unwrap_or(""))
        .unwrap_or(r.trim_start_matches(['>', '=', '~', '^']));
    let m = r.trim();
    (!m.is_empty()).then(|| m.to_string())
}

/// Revert offers for the mods folder under `root` (an instance's or a
/// server's), by jar, checked against `loader` and Minecraft `game`.
/// `addons` names and places the mods; `online` also offers mods with no
/// older copy kept but a known project (installed from CurseForge or
/// Modrinth on Revert). Reads every jar: off the UI thread.
pub fn offers(root: &Path, loader: &str, game: &str, addons: &[Addon], online: bool) -> HashMap<String, Offer> {
    let mods = root.join("mods");
    let by_file = |f: &str| addons.iter().find(|a| a.file_name.eq_ignore_ascii_case(f.trim_end_matches(".disabled")));
    hopper::deps::blockers(&mods, loader, game)
        .into_iter()
        .filter_map(|b| {
            let need = need_of(&b)?;
            let (range, have, other) = match &b.kind {
                BlockerKind::WrongVersion { needs, versions, jar, .. } => {
                    (versions.clone(), jar.clone(), by_file(jar).map_or_else(|| needs.clone(), |a| a.name.clone()))
                }
                BlockerKind::Loader { id, versions, .. } => {
                    let kind = loader.split('-').next().unwrap_or("");
                    let name = if id == "minecraft" {
                        "Minecraft".to_string()
                    } else {
                        crate::store::loader_label(&format!("{kind}-")).trim_end_matches(['-', ' ']).to_string()
                    };
                    (versions.clone(), loader.to_string(), name)
                }
                _ => return None,
            };
            // A record that names another jar still has the mod's name.
            let project_id = by_file(&b.file_name)
                .map(|a| a.project_id)
                .filter(|p| *p != 0)
                .or_else(|| addons.iter().find(|a| a.project_id != 0 && a.name.eq_ignore_ascii_case(&b.mod_name)).map(|a| a.project_id))
                .unwrap_or(0);
            let backup = hopper::revert::from_backups(root, &b.file_name, &need);
            if backup.is_none() && !(online && project_id != 0) {
                return None;
            }
            let needs = match range_min(&range) {
                Some(m) => format!("{other} {m}+"),
                None => format!("a newer {other}"),
            };
            Some((
                b.file_name.clone(),
                Offer { file_name: b.file_name, name: b.mod_name, project_id, need, needs, other, have, backup },
            ))
        })
        .collect()
}

/// Reverts a mod in an instance: puts the kept copy back, or installs the
/// newest older file that works (a task). Its updates are then marked with
/// what they need. True when the kept copy is back.
pub fn revert_instance(store: &Entity<Store>, root: &Path, offer: &Offer, window: &mut Window, cx: &mut App) -> bool {
    let (pid, needs, have) = (offer.project_id, offer.needs.clone(), offer.have.clone());
    let r = root.to_path_buf();
    let hold = move |store: &Entity<Store>, cx: &mut App| store.update(cx, |s, cx| s.hold_update(&r, pid, needs, have, cx));
    match &offer.backup {
        Some(ch) => {
            let back = crate::crash_help::put_back_changed(store, root, ch, window, cx);
            if back {
                hold(store, cx);
            }
            back
        }
        None => {
            let Some(mut inst) = store.read(cx).instance(root).cloned() else { return false };
            hold(store, cx);
            let (file, need, name) = (offer.file_name.clone(), offer.need.clone(), offer.name.clone());
            store.read(cx).spawn_instance_job(root, format!("Reverting {name}"), move |job| {
                let got = hopper::revert::from_source(&mut inst, &file, &need, job)?;
                job.note(format!("Reverted {name} to {got}"));
                Ok(crate::store::Msg::InstanceChanged(inst.root.clone()))
            });
            window.refresh();
            false
        }
    }
}

/// The store's cache entry: the mods folder's modified time it was read at.
pub type Cached = (Option<std::time::SystemTime>, std::sync::Arc<HashMap<String, Offer>>);

/// When the mods folder under `root` was last changed.
pub fn mods_mtime(root: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(root.join("mods")).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn range_min_reads_the_lower_bound() {
        assert_eq!(super::range_min("[19.70.0.460,)").as_deref(), Some("19.70.0.460"));
        assert_eq!(super::range_min("(1.2,2.0]").as_deref(), Some("1.2"));
        assert_eq!(super::range_min(">=0.5.3").as_deref(), Some("0.5.3"));
        assert_eq!(super::range_min("[,3)"), None);
    }
}
