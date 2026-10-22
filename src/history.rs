//! The change log: every file a sync copied or removed, with undo and redo.
//! Ported from mod-sync's change log. Undo restores from the backup taken
//! when the change ran; nothing is undone without one.

use crate::model::{SyncOp, SyncResult};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    /// Written to `to` from `from`; `backup` holds what `to` had before.
    Copied,
    /// `from` was removed; `backup` holds it.
    Removed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Change {
    pub id: u64,
    /// Changes made by one sync share a batch.
    pub batch: u64,
    pub time: String,
    pub action: Action,
    /// "server" or "client": the side that was written.
    pub side: String,
    /// The path shown, relative to the side's root.
    pub label: String,
    pub from: Option<PathBuf>,
    pub to: Option<PathBuf>,
    pub backup: Option<PathBuf>,
    /// Where undo and redo keep the files they replace.
    pub backup_root: PathBuf,
    /// The server the sync was for, to show its history.
    pub server: PathBuf,
    pub undone: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct Log {
    next: u64,
    changes: Vec<Change>,
}

const KEEP: usize = 1000;

static LOG: Mutex<Option<Log>> = Mutex::new(None);

fn path() -> Option<PathBuf> {
    crate::paths::dirs().map(|d| d.data_dir().join("history.json"))
}

fn read<R>(f: impl FnOnce(&Log) -> R) -> R {
    let mut g = LOG.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(load))
}

fn load() -> Log {
    path()
        .and_then(|p| fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Runs `f` on the log and saves it.
fn with<R>(f: impl FnOnce(&mut Log) -> R) -> R {
    let mut g = LOG.lock().unwrap_or_else(|e| e.into_inner());
    let log = g.get_or_insert_with(load);
    let r = f(log);
    if let Some(p) = path() {
        if let Some(dir) = p.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(b) = serde_json::to_vec(&*log) {
            let tmp = p.with_extension("json.tmp");
            if fs::write(&tmp, b).is_ok() {
                let _ = fs::rename(tmp, &p);
            }
        }
    }
    r
}

fn label(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Records the successful results of one sync run. `root` is the side that
/// was written (it also holds the backups).
pub fn record(server: &Path, side: &str, root: &Path, results: &[SyncResult]) {
    let done: Vec<&SyncResult> = results.iter().filter(|r| r.ok).collect();
    if done.is_empty() {
        return;
    }
    with(|log| {
        log.next += 1;
        let batch = log.next;
        let time = crate::time::now_iso();
        for r in done {
            log.next += 1;
            let (action, from, to, shown) = match &r.op {
                SyncOp::Copy { src, dst } => {
                    (Action::Copied, Some(src.clone()), Some(dst.clone()), dst)
                }
                SyncOp::Delete { path } => (Action::Removed, Some(path.clone()), None, path),
            };
            log.changes.insert(
                0,
                Change {
                    id: log.next,
                    batch,
                    time: time.clone(),
                    action,
                    side: side.to_string(),
                    label: label(root, shown),
                    from,
                    to,
                    backup: r.backup.clone(),
                    backup_root: root.to_path_buf(),
                    server: server.to_path_buf(),
                    undone: false,
                },
            );
        }
        log.changes.truncate(KEEP);
    });
}

/// Changes for one server, newest first.
pub fn for_server(server: &Path) -> Vec<Change> {
    read(|log| {
        log.changes
            .iter()
            .filter(|c| c.server == server)
            .cloned()
            .collect()
    })
}

pub fn clear(server: &Path) {
    with(|log| log.changes.retain(|c| c.server != server));
}

fn copy_over(src: &Path, dst: &Path) -> anyhow::Result<()> {
    if let Some(p) = dst.parent() {
        fs::create_dir_all(p)?;
    }
    fs::copy(src, dst)?;
    Ok(())
}

fn undo_one(c: &mut Change) -> anyhow::Result<()> {
    match c.action {
        Action::Copied => {
            let to =
                c.to.as_ref()
                    .ok_or_else(|| anyhow::anyhow!("no destination recorded"))?;
            // Keep the synced copy so redo works even if the source changed.
            let kept = crate::sync::backup_into(&c.backup_root, to)?;
            if let Some(b) = &c.backup {
                copy_over(b, to)?;
            }
            // Redo restores the synced copy, not whatever the source is now.
            if let Some(k) = kept {
                c.from = Some(k);
            }
        }
        Action::Removed => {
            let b = c
                .backup
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("no backup was kept for {}", c.label))?;
            let from = c
                .from
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("original location unknown"))?;
            copy_over(b, from)?;
        }
    }
    c.undone = true;
    Ok(())
}

fn redo_one(c: &mut Change) -> anyhow::Result<()> {
    match c.action {
        Action::Copied => {
            let (from, to) = c
                .from
                .clone()
                .zip(c.to.clone())
                .ok_or_else(|| anyhow::anyhow!("missing source or destination"))?;
            if !from.exists() {
                anyhow::bail!("{} is gone", from.display());
            }
            let kept = crate::sync::backup_into(&c.backup_root, &to)?;
            copy_over(&from, &to)?;
            if kept.is_some() {
                c.backup = kept;
            }
        }
        Action::Removed => {
            let from = c
                .from
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("original location unknown"))?;
            c.backup = crate::sync::backup_into(&c.backup_root, from)?.or(c.backup.take());
        }
    }
    c.undone = false;
    Ok(())
}

/// Undoes (or redoes) the given changes, newest first for undo and oldest
/// first for redo so a version swap unwinds in order. Returns the errors.
pub fn apply(ids: &[u64], undo: bool) -> Vec<String> {
    with(|log| {
        let mut idx: Vec<usize> = log
            .changes
            .iter()
            .enumerate()
            .filter(|(_, c)| ids.contains(&c.id) && c.undone != undo)
            .map(|(i, _)| i)
            .collect();
        if !undo {
            idx.reverse();
        }
        let mut errors = Vec::new();
        for i in idx {
            let c = &mut log.changes[i];
            let r = if undo { undo_one(c) } else { redo_one(c) };
            if let Err(e) = r {
                errors.push(format!("{}: {e:#}", c.label));
            }
        }
        errors
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-hist-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn undo_and_redo_a_copy_over_an_existing_file() {
        let root = tmp("copy");
        let (src, dst) = (root.join("client.toml"), root.join("server/config/a.toml"));
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::write(&src, "new").unwrap();
        fs::write(&dst, "old").unwrap();
        let backup = crate::sync::backup_into(&root.join("server"), &dst).unwrap();
        fs::copy(&src, &dst).unwrap();
        let mut c = Change {
            id: 1,
            batch: 1,
            time: String::new(),
            action: Action::Copied,
            side: "server".into(),
            label: "config/a.toml".into(),
            from: Some(src.clone()),
            to: Some(dst.clone()),
            backup,
            backup_root: root.join("server"),
            server: root.join("server"),
            undone: false,
        };
        undo_one(&mut c).unwrap();
        assert_eq!(fs::read_to_string(&dst).unwrap(), "old");
        // The source changes after the undo; redo still restores the synced copy.
        fs::write(&src, "newer").unwrap();
        redo_one(&mut c).unwrap();
        assert_eq!(fs::read_to_string(&dst).unwrap(), "new");
        undo_one(&mut c).unwrap();
        assert_eq!(fs::read_to_string(&dst).unwrap(), "old");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn undo_a_copy_of_a_new_file_removes_it() {
        let root = tmp("new");
        let (src, dst) = (root.join("a.jar"), root.join("server/mods/a.jar"));
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::write(&src, "x").unwrap();
        fs::copy(&src, &dst).unwrap();
        let mut c = Change {
            id: 1,
            batch: 1,
            time: String::new(),
            action: Action::Copied,
            side: "server".into(),
            label: "mods/a.jar".into(),
            from: Some(src),
            to: Some(dst.clone()),
            backup: None,
            backup_root: root.join("server"),
            server: root.join("server"),
            undone: false,
        };
        undo_one(&mut c).unwrap();
        assert!(!dst.exists());
        redo_one(&mut c).unwrap();
        assert_eq!(fs::read_to_string(&dst).unwrap(), "x");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn undo_a_removal_restores_it() {
        let root = tmp("rm");
        let f = root.join("server/mods/old.jar");
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(&f, "old").unwrap();
        let backup = crate::sync::backup_into(&root.join("server"), &f).unwrap();
        assert!(!f.exists());
        let mut c = Change {
            id: 1,
            batch: 1,
            time: String::new(),
            action: Action::Removed,
            side: "server".into(),
            label: "mods/old.jar".into(),
            from: Some(f.clone()),
            to: None,
            backup,
            backup_root: root.join("server"),
            server: root.join("server"),
            undone: false,
        };
        undo_one(&mut c).unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "old");
        redo_one(&mut c).unwrap();
        assert!(!f.exists());
        let _ = fs::remove_dir_all(&root);
    }
}
