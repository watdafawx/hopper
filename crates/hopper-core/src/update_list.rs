//! An instance's list of available mod updates, as the apps keep it.
//!
//! A check takes a few seconds. Blanking the list while it runs made every
//! Update button and version chip vanish and come back, which looks like a
//! mod went back a version. So the last finished check stays listed while a
//! new one runs (and after one fails) and is swapped out only when the new
//! answer arrives. Offers whose file changed on disk meanwhile (updated,
//! removed, locked) go at once, since they no longer apply. A mod switched
//! to another file that's still older than the offer (rolled back, say)
//! keeps its offer, now from the new file: the newest file didn't change.
//!
//! The last list is also saved, so a fresh start shows the last known
//! updates instead of none until the first check finishes.

use crate::install::Update;
use crate::instances::Addon;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The content kinds an instance lists in its own tab, in tab order: mods,
/// resource packs, shaders, data packs.
pub const KINDS: [u32; 4] = [
    crate::cf::CLASS_MODS,
    crate::cf::CLASS_RESOURCE_PACKS,
    crate::cf::CLASS_SHADERS,
    crate::cf::CLASS_DATAPACKS,
];

/// Whether an addon is listed in a kind's tab. Mods saved before Hopper
/// noted a class have none, and count as mods.
pub fn in_class(a: &Addon, class: u32) -> bool {
    a.class_id == class || (class == crate::cf::CLASS_MODS && a.class_id == 0)
}

/// The offers for one kind, one per project, and only for projects still
/// installed as that kind. A saved list can name a project that was removed
/// since; counting it would show an update nobody can find.
pub fn pending_in<'a>(addons: &'a [Addon], list: &'a [Update], class: u32) -> impl Iterator<Item = &'a Update> + 'a {
    let mut seen = HashSet::new();
    list.iter().filter(move |u| {
        addons.iter().any(|a| a.project_id == u.project_id && in_class(a, class)) && seen.insert(u.project_id)
    })
}

/// How many offers each kind has, in [`KINDS`] order. The header and the
/// content tabs count from the same filter, so their numbers agree.
pub fn counts(addons: &[Addon], list: &[Update]) -> [usize; 4] {
    KINDS.map(|c| pending_in(addons, list, c).count())
}

#[derive(Clone, Debug, Default)]
pub struct UpdateList {
    /// The last finished check's offers; `None` before any check finished
    /// (and no saved list was read).
    pub list: Option<Vec<Update>>,
    /// A check is running; `list` is the previous answer meanwhile.
    pub checking: bool,
    /// Why the last check failed (cleared when the next one starts).
    pub error: Option<String>,
    /// When the last check in this run finished (not saved), for "checked
    /// 3 min ago".
    pub checked: Option<std::time::Instant>,
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

    /// "checked 3 min ago" for the last finished check of this run.
    pub fn checked_ago(&self) -> Option<String> {
        Some(ago_text(self.checked?.elapsed().as_secs()))
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
                self.checked = Some(std::time::Instant::now());
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Drops offers that no longer apply to the files on disk: the mod is
    /// gone, updated to the offered file or past it, or locked. A mod moved
    /// to an older file keeps its offer, from that file. True when
    /// something changed.
    pub fn keep_current(&mut self, addons: &[Addon]) -> bool {
        let Some(list) = self.list.as_mut() else { return false };
        let mut changed = false;
        list.retain_mut(|u| {
            let before = u.from.clone();
            let keep = still_applies(u, addons);
            changed |= !keep || u.from != before;
            keep
        });
        changed
    }

    /// Takes projects off the list (installed, or locked by the user).
    pub fn drop_projects(&mut self, ids: &[u64]) -> bool {
        let Some(list) = self.list.as_mut() else { return false };
        let before = list.len();
        list.retain(|u| !ids.contains(&u.project_id));
        list.len() != before
    }
}

/// The offer still applies to what's installed now. When the mod went to
/// another file that's older than the offered one (a rollback or a switch
/// to some other old version), the offer moves to that file, so the
/// update stays listed without asking the sites again.
fn still_applies(u: &mut Update, addons: &[Addon]) -> bool {
    let Some(a) = addons.iter().find(|a| a.project_id == u.project_id && !a.locked) else { return false };
    if a.file_name == u.from {
        return true;
    }
    if a.file_name == u.file.file_name || !older_than(a, u.file.id, &u.file.file_date) {
        return false;
    }
    u.from = a.file_name.clone();
    true
}

/// The installed file is older than an offered file (its id and release
/// date). CurseForge's file ids grow over time; elsewhere the release dates
/// tell. Unknown counts as not older, so the offer goes until the next
/// check, as before.
pub fn older_than(a: &Addon, file_id: u64, file_date: &str) -> bool {
    let cf = !crate::modrinth::is_mr(a.project_id) && !crate::sources::is_src(a.project_id);
    if cf && a.file_id != 0 && file_id != 0 {
        return a.file_id < file_id;
    }
    match (crate::time::parse_iso(&a.file_date), crate::time::parse_iso(file_date)) {
        (Some(have), Some(new)) => have < new,
        _ => false,
    }
}

/// A mod went back to an older file that no offer covers (it was up to
/// date): it has an update now, which only a check can name. Mods that
/// kept an offer, or moved forward, need none.
pub fn rolled_back_unlisted(old: &[Addon], new: &[Addon], offers: Option<&[Update]>) -> bool {
    new.iter().filter(|a| a.project_id != 0 && !a.locked).any(|a| {
        let listed = offers.is_some_and(|l| l.iter().any(|u| u.project_id == a.project_id && u.from == a.file_name));
        !listed
            && old
                .iter()
                .any(|o| o.project_id == a.project_id && o.file_name != a.file_name && older_than(a, o.file_id, &o.file_date))
    })
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
    list.retain_mut(|u| still_applies(u, addons));
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

/// A short "how long ago" for a number of seconds.
pub fn ago_text(secs: u64) -> String {
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86399 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86400),
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

    #[test]
    fn ago_text_picks_a_unit() {
        assert_eq!(ago_text(5), "just now");
        assert_eq!(ago_text(125), "2 min ago");
        assert_eq!(ago_text(7300), "2 h ago");
        assert_eq!(ago_text(200000), "2 d ago");
    }

    #[test]
    fn counts_by_kind_skip_removed_and_doubles() {
        let mut rp = addon(2, "geo.zip");
        rp.class_id = crate::cf::CLASS_RESOURCE_PACKS;
        let mut sh = addon(3, "solas.zip");
        sh.class_id = crate::cf::CLASS_SHADERS;
        let mut m = addon(1, "a.jar");
        m.class_id = crate::cf::CLASS_MODS;
        // An old entry with no class is a mod.
        let old = addon(4, "b.jar");
        let addons = vec![m, rp, sh, old];
        let list = vec![
            offer(1, "a.jar", 10),
            offer(1, "a.jar", 11),
            offer(2, "geo.zip", 20),
            offer(3, "solas.zip", 30),
            offer(4, "b.jar", 40),
            // Removed from the instance since the list was saved.
            offer(9, "gone.jar", 90),
        ];
        assert_eq!(counts(&addons, &list), [2, 1, 1, 0]);
        let ids: Vec<u64> = pending_in(&addons, &list, crate::cf::CLASS_MODS).map(|u| u.project_id).collect();
        assert_eq!(ids, vec![1, 4]);
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
    fn rolled_back_mod_keeps_its_update() {
        let mut l = UpdateList::default();
        // Installed 1.1 (file 10); 1.3 (file 30) is offered.
        l.finish(Ok(vec![offer(1, "a-1.1.jar", 30)]));
        let mut old = addon(1, "a-1.0.jar");
        old.file_id = 5;
        assert!(l.keep_current(&[old]));
        let u = &l.ready().unwrap()[0];
        assert_eq!((u.project_id, u.from.as_str(), u.file.id), (1, "a-1.0.jar", 30));
        // Switched up to 1.2: still older than the offer, still listed.
        let mut mid = addon(1, "a-1.2.jar");
        mid.file_id = 20;
        assert!(l.keep_current(&[mid.clone()]));
        assert_eq!(l.ready().unwrap()[0].from, "a-1.2.jar");
        assert!(!l.keep_current(&[mid]));
        // Updated to the offered file, or past it: the offer goes.
        let mut same = addon(1, "new-1.jar");
        same.file_id = 30;
        assert!(l.clone().keep_current(&[same]));
        let mut past = addon(1, "a-1.4.jar");
        past.file_id = 40;
        assert!(l.keep_current(&[past]));
        assert_eq!(ids(&l), Vec::<u64>::new());
    }

    #[test]
    fn rollback_without_ids_goes_by_date() {
        let mr = 1u64 << 63 | 7;
        let mut o = offer(mr, "b-2.jar", 0);
        o.file.file_date = "2026-05-01T10:00:00Z".into();
        let mut l = UpdateList::saved(vec![o]);
        let mut old = addon(mr, "b-1.jar");
        old.file_date = "2026-01-01T10:00:00Z".into();
        assert!(l.keep_current(&[old]));
        assert_eq!(l.ready().unwrap()[0].from, "b-1.jar");
        // No date known: dropped until the next check, as before.
        assert!(l.keep_current(&[addon(mr, "b-0.jar")]));
        assert_eq!(ids(&l), Vec::<u64>::new());
    }

    #[test]
    fn rollback_of_an_up_to_date_mod_asks_for_a_check() {
        let mut now = addon(1, "a-1.3.jar");
        now.file_id = 30;
        let mut back = addon(1, "a-1.1.jar");
        back.file_id = 10;
        assert!(rolled_back_unlisted(&[now.clone()], &[back.clone()], Some(&[])));
        assert!(rolled_back_unlisted(&[now.clone()], &[back.clone()], None));
        // Its offer already followed it: nothing to fetch.
        assert!(!rolled_back_unlisted(&[now.clone()], &[back.clone()], Some(&[offer(1, "a-1.1.jar", 30)])));
        // Moved forward, unchanged, or locked: no check.
        assert!(!rolled_back_unlisted(&[back.clone()], &[now.clone()], Some(&[])));
        assert!(!rolled_back_unlisted(&[now.clone()], &[now.clone()], Some(&[])));
        back.locked = true;
        assert!(!rolled_back_unlisted(&[now], &[back], Some(&[])));
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
