//! An instance's list of available mod updates, as the apps keep it.
//!
//! A check takes a few seconds. Blanking the list while it runs made every
//! Update button and version chip vanish and come back, which looks like a
//! mod went back a version. So the last finished check stays listed while a
//! new one runs (and after one fails) and is swapped out only when the new
//! answer arrives. Offers whose file changed on disk meanwhile (updated,
//! removed, put back, locked) go at once, since they no longer apply.
//!
//! The last list is also saved, so a fresh start shows the last known
//! updates instead of none until the first check finishes.

use crate::install::Update;
use crate::instances::Addon;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct UpdateList {
    /// The last finished check's offers; `None` before any check finished
    /// (and no saved list was read).
    pub list: Option<Vec<Update>>,
    /// A check is running; `list` is the previous answer meanwhile.
    pub checking: bool,
    /// Why the last check failed (cleared when the next one starts).
    pub error: Option<String>,
}

impl UpdateList {
    /// A list read back from the last run, to show until a check finishes.
    pub fn saved(list: Vec<Update>) -> Self {
        Self { list: Some(list), ..Self::default() }
    }

    /// The offers to show: the last known ones, also while checking.
    pub fn ready(&self) -> Option<&Vec<Update>> {
        self.list.as_ref()
    }

    /// A check starts: everything listed stays listed.
    pub fn begin(&mut self) {
        self.checking = true;
        self.error = None;
    }

    /// A check finished: its answer replaces the list in one go. A failed
    /// check keeps the old list and notes why.
    pub fn finish(&mut self, result: Result<Vec<Update>, String>) {
        self.checking = false;
        match result {
            Ok(u) => {
                self.list = Some(u);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Drops offers that no longer apply to the files on disk: the mod is
    /// gone, its file changed (updated or put back), or it's locked. True
    /// when something went.
    pub fn keep_current(&mut self, addons: &[Addon]) -> bool {
        let Some(list) = self.list.as_mut() else { return false };
        let before = list.len();
        list.retain(|u| still_applies(u, addons));
        list.len() != before
    }

    /// Takes projects off the list (installed, or locked by the user).
    pub fn drop_projects(&mut self, ids: &[u64]) -> bool {
        let Some(list) = self.list.as_mut() else { return false };
        let before = list.len();
        list.retain(|u| !ids.contains(&u.project_id));
        list.len() != before
    }
}

/// The offer is still for the file that's installed now.
fn still_applies(u: &Update, addons: &[Addon]) -> bool {
    addons
        .iter()
        .any(|a| a.project_id == u.project_id && a.file_name == u.from && !a.locked)
}

// ---- saved lists -----------------------------------------------------------

fn saved_path() -> Option<PathBuf> {
    crate::paths::dirs().map(|d| d.cache_dir().join("mod-updates.json"))
}

fn read_all(path: &Path) -> HashMap<String, Vec<Update>> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The list saved for an instance by an earlier run, minus offers whose
/// file changed since.
pub fn load(root: &Path, addons: &[Addon]) -> Option<Vec<Update>> {
    let mut all = read_all(&saved_path()?);
    let mut list = all.remove(&root.to_string_lossy().to_string())?;
    list.retain(|u| still_applies(u, addons));
    Some(list)
}

/// Saves an instance's list for the next run (`None` forgets it).
pub fn save(root: &Path, list: Option<&[Update]>) {
    let Some(path) = saved_path() else { return };
    let mut all = read_all(&path);
    let key = root.to_string_lossy().to_string();
    match list {
        Some(l) => {
            if all.get(&key).is_some_and(|old| same(old, l)) {
                return;
            }
            all.insert(key, l.to_vec());
        }
        None => {
            if all.remove(&key).is_none() {
                return;
            }
        }
    }
    let Ok(bytes) = serde_json::to_vec(&all) else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = crate::instances::write_atomic(&path, &bytes) {
        log::warn!("saving the mod update lists: {e:#}");
    }
}

fn same(a: &[Update], b: &[Update]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| (x.project_id, x.file.id, &x.from) == (y.project_id, y.file.id, &y.from))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cf::File;

    fn addon(pid: u64, file: &str) -> Addon {
        Addon { project_id: pid, file_name: file.into(), ..Addon::default() }
    }

    fn offer(pid: u64, from: &str, to_id: u64) -> Update {
        Update {
            project_id: pid,
            name: format!("mod {pid}"),
            from: from.into(),
            file: File { id: to_id, file_name: format!("new-{pid}.jar"), ..File::default() },
        }
    }

    fn ids(l: &UpdateList) -> Vec<u64> {
        l.ready().map(|v| v.iter().map(|u| u.project_id).collect()).unwrap_or_default()
    }

    #[test]
    fn old_offers_stay_while_checking() {
        let mut l = UpdateList::default();
        l.finish(Ok(vec![offer(1, "a.jar", 10), offer(2, "b.jar", 20)]));
        l.begin();
        assert!(l.checking);
        assert_eq!(ids(&l), vec![1, 2]);
    }

    #[test]
    fn finished_check_replaces_the_list() {
        let mut l = UpdateList::saved(vec![offer(1, "a.jar", 10), offer(2, "b.jar", 20)]);
        l.begin();
        l.finish(Ok(vec![offer(3, "c.jar", 30)]));
        assert!(!l.checking);
        assert_eq!(ids(&l), vec![3]);
        // An empty answer means up to date, not "unknown".
        l.begin();
        l.finish(Ok(vec![]));
        assert_eq!(l.ready().map(Vec::len), Some(0));
    }

    #[test]
    fn failed_check_keeps_the_old_list() {
        let mut l = UpdateList::default();
        l.finish(Ok(vec![offer(1, "a.jar", 10)]));
        l.begin();
        l.finish(Err("CurseForge is down".into()));
        assert!(!l.checking);
        assert_eq!(l.error.as_deref(), Some("CurseForge is down"));
        assert_eq!(ids(&l), vec![1]);
        // The next check clears the error but still keeps the list.
        l.begin();
        assert_eq!(l.error, None);
        assert_eq!(ids(&l), vec![1]);
    }

    #[test]
    fn offers_for_changed_files_go_at_once() {
        let mut l = UpdateList::default();
        l.finish(Ok(vec![offer(1, "a.jar", 10), offer(2, "b.jar", 20), offer(3, "c.jar", 30), offer(4, "d.jar", 40)]));
        l.begin();
        let mut locked = addon(4, "d.jar");
        locked.locked = true;
        // 1 unchanged, 2 updated to another file, 3 removed, 4 locked.
        let disk = vec![addon(1, "a.jar"), addon(2, "b-1.1.jar"), locked];
        assert!(l.keep_current(&disk));
        assert_eq!(ids(&l), vec![1]);
        assert!(l.checking);
        assert!(!l.keep_current(&disk));
    }

    #[test]
    fn dropping_projects_keeps_the_rest() {
        let mut l = UpdateList::saved(vec![offer(1, "a.jar", 10), offer(2, "b.jar", 20)]);
        assert!(l.drop_projects(&[2]));
        assert_eq!(ids(&l), vec![1]);
        assert!(!UpdateList::default().drop_projects(&[1]));
    }

    #[test]
    fn updates_round_trip_through_json() {
        let v = vec![offer(1, "a.jar", 10)];
        let back: Vec<Update> = serde_json::from_slice(&serde_json::to_vec(&v).unwrap()).unwrap();
        assert!(same(&v, &back));
        assert_eq!(back[0].file.file_name, "new-1.jar");
    }
}
