//! Applying a diff to a server folder. Ported from mod-sync's `sync.go` and
//! `backup.go`: nothing is overwritten or deleted without a backup first.

use crate::model::{DiffAction, DiffEntry, SyncOp, SyncResult};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Backups live inside the server folder, so moving a file there is a rename
/// on the same volume (fast over the `Q:` share) instead of a copy.
pub const BACKUP_DIR: &str = ".hopper-backups";

pub fn plan(diff: &[DiffEntry], client_mods: &Path, server_mods: &Path) -> Vec<SyncOp> {
    let mut ops = Vec::new();
    for d in diff {
        match d.action {
            DiffAction::Skip => {}
            DiffAction::CopyToServer => {
                let Some(c) = &d.client else { continue };
                // A version swap removes the old server file as well.
                if let Some(s) = &d.server
                    && !s.file_name.eq_ignore_ascii_case(&c.file_name)
                {
                    ops.push(SyncOp::Delete {
                        path: server_mods.join(&s.file_name),
                    });
                }
                ops.push(SyncOp::Copy {
                    src: client_mods.join(&c.file_name),
                    dst: server_mods.join(&c.file_name),
                });
            }
            DiffAction::DeleteFromServer => {
                if let Some(s) = &d.server {
                    ops.push(SyncOp::Delete {
                        path: server_mods.join(&s.file_name),
                    });
                }
            }
        }
    }
    ops
}

/// `server_root` holds the backup folder; one timestamped subfolder per run.
pub fn execute(ops: Vec<SyncOp>, server_root: &Path) -> Vec<SyncResult> {
    // A server on a Hopper Server: its side goes over the panel's API.
    let remote = ops.iter().find_map(|op| match op {
        SyncOp::Copy { src, dst } => crate::hopperd::remote_of(dst).or_else(|| crate::hopperd::remote_of(src)),
        SyncOp::Delete { path } => crate::hopperd::remote_of(path),
    });
    if let Some((root, r)) = remote {
        // Local backups (a pull replacing a client file) stay local.
        let local = if crate::hopperd::is_remote_root(server_root) {
            std::env::temp_dir()
        } else {
            server_root.to_path_buf()
        };
        return crate::hopperd::execute(ops, &r, &root, &local);
    }
    execute_local(ops, server_root)
}

/// [`execute`] for operations on this PC's disks only.
pub fn execute_local(ops: Vec<SyncOp>, server_root: &Path) -> Vec<SyncResult> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let backup_dir = server_root.join(BACKUP_DIR).join(stamp.to_string());
    crate::retention::schedule(server_root);
    ops.into_iter()
        .map(|op| {
            let r = match &op {
                SyncOp::Copy { src, dst } => copy(src, dst, &backup_dir),
                SyncOp::Delete { path } => backup(path, &backup_dir),
            };
            match r {
                Ok(backup) => SyncResult {
                    op,
                    ok: true,
                    error: None,
                    backup,
                },
                Err(e) => SyncResult {
                    op,
                    ok: false,
                    error: Some(format!("{e:#}")),
                    backup: None,
                },
            }
        })
        .collect()
}

/// Moves `path` into a timestamped folder under `<root>/.hopper-backups`.
pub fn backup_into(root: &Path, path: &Path) -> anyhow::Result<Option<PathBuf>> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let dir = root.join(BACKUP_DIR).join(stamp.to_string());
    let saved = backup(path, &dir)?;
    if saved.is_some() {
        crate::retention::schedule(root);
    }
    // Where it came from, so it can go back there.
    if let Some(s) = &saved
        && let Ok(rel) = path.strip_prefix(root)
    {
        crate::backups::record(&dir, s, &rel.to_string_lossy());
    }
    Ok(saved)
}

/// Moves `path` into `backup_dir`. `None` when there was nothing to move.
fn backup(path: &Path, backup_dir: &Path) -> anyhow::Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    fs::create_dir_all(backup_dir)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    // Config folders repeat names (many `common.toml`s) within one run.
    let mut dest = backup_dir.join(&*name);
    let mut n = 1;
    while crate::backups::exists(&dest) {
        n += 1;
        dest = backup_dir.join(format!("{n}-{name}"));
    }
    // Which CurseForge / Modrinth file a jar is, read before it leaves,
    // so the copy can become a note of it later (backups::slim).
    let ids = backup_dir.parent().and_then(Path::parent).and_then(|root| crate::backups::ids_for(root, path));
    if let Err(e) = fs::rename(path, &dest) {
        // In use (a running game): copying would only leave a second copy.
        if crate::locked::io_locked(&e) {
            return Err(e.into());
        }
        // Different volume: copy, then remove. Fail closed if the copy fails.
        fs::copy(path, &dest)?;
        if let Err(e) = fs::remove_file(path) {
            let _ = fs::remove_file(&dest);
            return Err(e.into());
        }
    }
    if let Some(c) = ids {
        crate::backups::note_ids(&dest, c);
    }
    Ok(Some(dest))
}

/// Copies through a temp file and a rename, so a crash never leaves a
/// half-written jar in `mods`.
fn copy(src: &Path, dst: &Path, backup_dir: &Path) -> anyhow::Result<Option<PathBuf>> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut tmp = dst.as_os_str().to_owned();
    tmp.push(".hopper-tmp");
    let tmp = PathBuf::from(tmp);
    fs::copy(src, &tmp)?;
    let saved = match backup(dst, backup_dir) {
        Ok(saved) => saved,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    };
    if let Err(e) = fs::rename(&tmp, dst) {
        let _ = fs::remove_file(&tmp);
        if let Some(saved) = &saved {
            let _ = fs::rename(saved, dst);
        }
        return Err(e.into());
    }
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_backs_up_the_file_it_replaces() {
        let root = std::env::temp_dir().join(format!("hopper-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (client, server) = (root.join("client"), root.join("server/mods"));
        fs::create_dir_all(&client).unwrap();
        fs::create_dir_all(&server).unwrap();
        fs::write(client.join("a.jar"), b"new").unwrap();
        fs::write(server.join("a.jar"), b"old").unwrap();

        let r = execute(
            vec![SyncOp::Copy {
                src: client.join("a.jar"),
                dst: server.join("a.jar"),
            }],
            &root.join("server"),
        );
        assert!(r[0].ok, "{:?}", r[0].error);
        assert_eq!(fs::read(server.join("a.jar")).unwrap(), b"new");
        assert_eq!(fs::read(r[0].backup.as_ref().unwrap()).unwrap(), b"old");
        let _ = fs::remove_dir_all(&root);
    }
}
