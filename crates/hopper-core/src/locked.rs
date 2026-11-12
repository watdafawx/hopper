//! Files a running game holds open, waiting to be moved out of an
//! instance or renamed. Windows refuses to move, delete or rename a jar Java
//! opened with `java.util.jar.JarFile` (no share-delete), so an update,
//! removal or switch while playing can't happen yet. It goes on this list
//! instead of failing halfway, and happens once the game lets go (when it
//! ends, or before the next launch).

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// The list, in the instance folder.
pub const FILE: &str = ".hopper-locked.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Queued {
    /// Relative to the instance folder (`mods/x.jar`).
    pub path: String,
    /// To the Recycle Bin instead of Hopper's backups.
    #[serde(default)]
    pub delete: bool,
    /// Renamed to this (relative too) instead of moved out: a mod turned
    /// off or on (`x.jar` <-> `x.jar.disabled`) while the game has it open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

/// Windows refused because another process has the file open
/// (ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION).
pub fn is_locked(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>()
            .is_some_and(io_locked)
    })
}

pub fn io_locked(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(32 | 33))
}

pub fn list(root: &Path) -> Vec<Queued> {
    fs::read(root.join(FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write(root: &Path, list: &[Queued]) -> anyhow::Result<()> {
    let p = root.join(FILE);
    if list.is_empty() {
        match fs::remove_file(&p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => return Ok(()),
        }
    }
    fs::write(p, serde_json::to_vec_pretty(list)?)?;
    Ok(())
}

/// File names (lowercase, no `.disabled`) queued in `mods/`, to leave out
/// of the loose-jar and duplicate checks: they are already on their way out.
/// One only waiting to be renamed stays, so it isn't in here.
pub fn queued_mods(root: &Path) -> std::collections::HashSet<String> {
    list(root)
        .into_iter()
        .filter(|q| q.to.is_none())
        .filter_map(|q| {
            let rel = q.path.replace('\\', "/");
            let name = rel.strip_prefix("mods/")?;
            Some(name.to_lowercase().trim_end_matches(".disabled").to_string())
        })
        .collect()
}

/// Switches waiting for the game: file name (lowercase, no `.disabled`) to
/// whether it ends up on. The Mods tab shows that state on the switch, so
/// flipping it again takes the switch back.
pub fn switches(root: &Path) -> std::collections::HashMap<String, bool> {
    list(root)
        .into_iter()
        .filter_map(|q| {
            let to = q.to?;
            let name = to.rsplit('/').next()?.to_lowercase();
            let on = !name.ends_with(".disabled");
            Some((name.trim_end_matches(".disabled").to_string(), on))
        })
        .collect()
}

/// Puts `path` on the list (once).
pub fn queue(root: &Path, path: &Path, delete: bool) -> anyhow::Result<()> {
    let rel = rel(root, path);
    let mut l = list(root);
    l.retain(|q| !q.path.eq_ignore_ascii_case(&rel));
    l.push(Queued { path: rel, delete, to: None });
    log::info!("{} is in use; it leaves when the game closes", path.display());
    write(root, &l)
}

/// Puts renaming `path` to `to` on the list, in place of anything queued
/// for `path` (a newer switch wins over an older one).
pub fn queue_rename(root: &Path, path: &Path, to: &Path) -> anyhow::Result<()> {
    let (from, to) = (rel(root, path), rel(root, to));
    let mut l = list(root);
    l.retain(|q| !q.path.eq_ignore_ascii_case(&from));
    l.push(Queued { path: from, delete: false, to: Some(to) });
    log::info!("{} is in use; it is renamed when the game closes", path.display());
    write(root, &l)
}

/// Forgets a rename queued for `path` (the mod was switched back, or the
/// switch went through). A queued move out stays.
pub fn unqueue_rename(root: &Path, path: &Path) {
    let rel = rel(root, path);
    let mut l = list(root);
    let n = l.len();
    l.retain(|q| q.to.is_none() || !q.path.eq_ignore_ascii_case(&rel));
    if l.len() != n {
        let _ = write(root, &l);
    }
}

/// Forgets `path` (it came back, e.g. reinstalled under the same name).
pub fn unqueue(root: &Path, path: &Path) {
    let rel = rel(root, path);
    let mut l = list(root);
    let n = l.len();
    l.retain(|q| !q.path.eq_ignore_ascii_case(&rel));
    if l.len() != n {
        let _ = write(root, &l);
    }
}

/// Moves out or renames whatever the game has let go of; the rest stays
/// queued. Returns how many went.
pub fn flush(root: &Path) -> usize {
    let l = list(root);
    if l.is_empty() {
        return 0;
    }
    let mut keep = Vec::new();
    let mut done = 0;
    for q in l {
        let p: PathBuf = root.join(&q.path);
        let r = if let Some(to) = &q.to {
            rename(root, &p, to)
        } else if q.delete {
            crate::install::delete_path(&p)
        } else {
            crate::sync::backup_into(root, &p).map(|_| ())
        };
        match r {
            Ok(()) => done += 1,
            Err(e) if is_locked(&e) => keep.push(q),
            Err(e) => log::warn!("removing {}: {e:#}", p.display()),
        }
    }
    if let Err(e) = write(root, &keep) {
        log::warn!("saving {}: {e:#}", root.join(FILE).display());
    }
    done
}

/// A queued switch: renames the jar, and sets the mod's switch in the
/// instance to match (it was left as the file was while the game held it).
/// Gone meanwhile, or something already at `to`: nothing to do.
fn rename(root: &Path, from: &Path, to: &str) -> anyhow::Result<()> {
    let to_path = root.join(to);
    if !from.exists() || to_path.exists() {
        return Ok(());
    }
    fs::rename(from, &to_path)?;
    let base = root.join(to.strip_suffix(".disabled").unwrap_or(to));
    let on = !to.ends_with(".disabled");
    if root.join(crate::instances::HOPPER_FILE).is_file() {
        crate::instances::Instance::update_on_disk(root, |i| {
            let root = i.root.clone();
            for a in i.addons.iter_mut().filter(|a| root.join(a.folder()).join(&a.file_name) == base) {
                a.enabled = on;
            }
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_flush_and_names() {
        let dir = std::env::temp_dir().join(format!("hopper-locked-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("mods")).unwrap();
        let jar = dir.join("mods").join("Old-1.0.jar");
        fs::write(&jar, b"x").unwrap();
        queue(&dir, &jar, false).unwrap();
        queue(&dir, &jar, false).unwrap();
        assert_eq!(list(&dir).len(), 1);
        assert!(queued_mods(&dir).contains("old-1.0.jar"));
        // Nothing holds it here, so it moves to backups.
        assert_eq!(flush(&dir), 1);
        assert!(!jar.exists());
        assert!(list(&dir).is_empty());
        assert!(!dir.join(FILE).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Held the way `java.util.jar.JarFile` holds a jar: Windows refuses to
    /// move it, so removal queues it, and it leaves once let go.
    #[cfg(windows)]
    #[test]
    fn locked_jar_waits_for_the_game() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = std::env::temp_dir().join(format!("hopper-locked-held-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("mods")).unwrap();
        let jar = dir.join("mods").join("held.jar");
        fs::write(&jar, b"x").unwrap();
        // FILE_SHARE_READ | FILE_SHARE_WRITE, no FILE_SHARE_DELETE.
        let held = fs::OpenOptions::new().read(true).share_mode(1 | 2).open(&jar).unwrap();
        crate::install::move_out(&dir, &jar, false).unwrap();
        assert!(jar.exists());
        assert_eq!(list(&dir).len(), 1);
        // Still held: stays queued, and no stray copy lands in backups.
        assert_eq!(flush(&dir), 0);
        assert_eq!(list(&dir).len(), 1);
        let backups = dir.join(crate::sync::BACKUP_DIR);
        let copies = walk(&backups);
        assert_eq!(copies, 0, "a locked file must not be copied to backups");
        drop(held);
        assert_eq!(flush(&dir), 1);
        assert!(!jar.exists());
        assert!(list(&dir).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A followed pack turns off a mod the game holds open: the switch
    /// waits, the mod stays a mod (not on its way out), and once let go the
    /// jar is renamed and the instance says it's off.
    #[cfg(windows)]
    #[test]
    fn locked_switch_waits_for_the_game() {
        use crate::instances::{Addon, Instance};
        use std::os::windows::fs::OpenOptionsExt;
        let dir = std::env::temp_dir().join(format!("hopper-locked-switch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("mods")).unwrap();
        let jar = dir.join("mods").join("held.jar");
        fs::write(&jar, b"x").unwrap();
        let mut inst = Instance {
            name: "t".into(),
            root: dir.clone(),
            game_version: "1.21.1".into(),
            addons: vec![Addon { project_id: 7, file_id: 1, file_name: "held.jar".into(), name: "Held".into(), enabled: true, class_id: crate::cf::CLASS_MODS, ..Default::default() }],
            ..Default::default()
        };
        inst.save().unwrap();
        let held = fs::OpenOptions::new().read(true).share_mode(1 | 2).open(&jar).unwrap();
        crate::install::set_enabled_later(&mut inst, 0, false).unwrap();
        // Nothing changed yet: the game has it loaded.
        assert!(jar.exists() && inst.addons[0].enabled);
        assert_eq!(list(&dir)[0].to.as_deref(), Some("mods/held.jar.disabled"));
        assert_eq!(switches(&dir).get("held.jar"), Some(&false));
        assert!(queued_mods(&dir).is_empty());
        assert_eq!(flush(&dir), 0);
        assert_eq!(list(&dir).len(), 1);
        // Turned back on before the game let go: nothing left to do.
        crate::install::set_enabled_later(&mut inst, 0, true).unwrap();
        assert!(list(&dir).is_empty());
        crate::install::set_enabled_later(&mut inst, 0, false).unwrap();
        drop(held);
        assert_eq!(flush(&dir), 1);
        assert!(!jar.exists() && dir.join("mods").join("held.jar.disabled").exists());
        assert!(list(&dir).is_empty());
        let after = crate::instances::load(&dir).unwrap();
        assert!(!after.addon_by_file("held.jar").unwrap().enabled);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Removed after a switch was queued: the move out wins.
    #[test]
    fn a_move_out_replaces_a_queued_switch() {
        let dir = std::env::temp_dir().join(format!("hopper-locked-replace-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("mods")).unwrap();
        let jar = dir.join("mods").join("a.jar");
        queue_rename(&dir, &jar, &dir.join("mods").join("a.jar.disabled")).unwrap();
        queue(&dir, &jar, true).unwrap();
        assert_eq!(list(&dir), vec![Queued { path: "mods/a.jar".into(), delete: true, to: None }]);
        // A queued move isn't dropped by forgetting a switch.
        unqueue_rename(&dir, &jar);
        assert_eq!(list(&dir).len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    fn walk(p: &Path) -> usize {
        fs::read_dir(p)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| if e.path().is_dir() { walk(&e.path()) } else { 1 })
            .sum()
    }
}
