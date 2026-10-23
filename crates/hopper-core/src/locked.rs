//! Files a running game holds open, waiting to be moved out of an
//! instance. Windows refuses to move or delete a jar Java opened with
//! `java.util.jar.JarFile` (no share-delete), so an update or removal while
//! playing leaves the old file behind. It goes on this list instead of
//! failing halfway, and leaves once the game lets go (when it ends, or
//! before the next launch).

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
pub fn queued_mods(root: &Path) -> std::collections::HashSet<String> {
    list(root)
        .into_iter()
        .filter_map(|q| {
            let rel = q.path.replace('\\', "/");
            let name = rel.strip_prefix("mods/")?;
            Some(name.to_lowercase().trim_end_matches(".disabled").to_string())
        })
        .collect()
}

/// Puts `path` on the list (once).
pub fn queue(root: &Path, path: &Path, delete: bool) -> anyhow::Result<()> {
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let mut l = list(root);
    l.retain(|q| !q.path.eq_ignore_ascii_case(&rel));
    l.push(Queued { path: rel, delete });
    log::info!("{} is in use; it leaves when the game closes", path.display());
    write(root, &l)
}

/// Forgets `path` (it came back, e.g. reinstalled under the same name).
pub fn unqueue(root: &Path, path: &Path) {
    let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/");
    let mut l = list(root);
    let n = l.len();
    l.retain(|q| !q.path.eq_ignore_ascii_case(&rel));
    if l.len() != n {
        let _ = write(root, &l);
    }
}

/// Moves out whatever the game has let go of; the rest stays queued.
/// Returns how many left.
pub fn flush(root: &Path) -> usize {
    let l = list(root);
    if l.is_empty() {
        return 0;
    }
    let mut keep = Vec::new();
    let mut done = 0;
    for q in l {
        let p: PathBuf = root.join(&q.path);
        let r = if q.delete {
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
