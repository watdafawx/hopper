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
                let src = client_mods.join(&c.file_name);
                let src = if c.off { disabled_path(&src) } else { src };
                // The server's copy is turned off: the new file goes on or
                // off as it is in the pack, and the old one goes (History
                // keeps it), so no older copy stays beside the new one.
                if let Some(s) = d.server.as_ref().filter(|s| s.off) {
                    let dst = server_mods.join(&c.file_name);
                    let same_name = s.file_name.eq_ignore_ascii_case(&c.file_name);
                    ops.push(SyncOp::Copy { src, dst: dst.clone() });
                    if c.off {
                        ops.push(SyncOp::Disable { path: dst });
                    }
                    // Turned off, the same name: the turn-off already set the old file aside.
                    if !(c.off && same_name) {
                        ops.push(SyncOp::Delete { path: disabled_path(&server_mods.join(&s.file_name)) });
                    }
                    continue;
                }
                // A version swap removes the old server file as well.
                if let Some(s) = &d.server
                    && !s.file_name.eq_ignore_ascii_case(&c.file_name)
                {
                    ops.push(SyncOp::Delete {
                        path: server_mods.join(&s.file_name),
                    });
                }
                ops.push(SyncOp::Copy {
                    src,
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
            DiffAction::DisableOnServer => {
                if let Some(s) = &d.server {
                    ops.push(SyncOp::Disable {
                        path: server_mods.join(&s.file_name),
                    });
                }
            }
            DiffAction::EnableOnServer => {
                if let Some(s) = &d.server {
                    ops.push(SyncOp::Enable {
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
    execute_with(ops, server_root, &|_, _| {})
}

/// How much of a sync's progress `op` is: a file copied counts its bytes,
/// anything else one, so a big upload moves the bar while it's sent
/// instead of jumping at the end.
pub fn op_weight(op: &SyncOp) -> u64 {
    match op {
        SyncOp::Copy { src, .. } => fs::metadata(src).map_or(1, |m| m.len().max(1)),
        _ => 1,
    }
}

/// [`execute`], telling `progress` (done, total in [`op_weight`]s) as it goes.
pub fn execute_with(ops: Vec<SyncOp>, server_root: &Path, progress: &dyn Fn(u64, u64)) -> Vec<SyncResult> {
    // A server on a Hopper Server: its side goes over the panel's API.
    let remote = ops.iter().find_map(|op| match op {
        SyncOp::Copy { src, dst } => crate::hopperd::remote_of(dst).or_else(|| crate::hopperd::remote_of(src)),
        SyncOp::Delete { path } | SyncOp::Disable { path } | SyncOp::Enable { path } => crate::hopperd::remote_of(path),
    });
    if let Some((root, r)) = remote {
        // Local backups (a pull replacing a client file) stay local.
        let local = if crate::hopperd::is_remote_root(server_root) {
            std::env::temp_dir()
        } else {
            server_root.to_path_buf()
        };
        return crate::hopperd::execute(ops, &r, &root, &local, progress);
    }
    let total = ops.iter().map(op_weight).sum();
    // A server reached over SFTP / FTP: change Hopper's copy, then the server.
    let results = if let Some(m) = crate::remotefs::mirror_of(server_root) {
        let mut results = execute_local(ops, server_root);
        crate::remotefs::push(&m, &mut results);
        results
    } else {
        execute_local(ops, server_root)
    };
    progress(total, total);
    results
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
                SyncOp::Disable { path } => disable(path, &backup_dir),
                SyncOp::Enable { path } => enable(path, &backup_dir),
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
    // A jar CurseForge or Modrinth knows isn't kept: it's deleted and a
    // note of which file it was stands in, so Undo downloads it again.
    // Unknown jars and text files are kept as copies (nothing else has them).
    let ids = backup_dir.parent().and_then(Path::parent).and_then(|root| crate::backups::ids_for(root, path));
    if let Some(c) = &ids {
        crate::backups::note_instead(path, &dest, c)?;
        return Ok(Some(dest));
    }
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
    Ok(Some(dest))
}

/// Copies through a temp file and a rename, so a crash never leaves a
/// half-written jar in `mods`.
/// `<jar>.disabled` path for a jar.
pub fn disabled_path(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(".disabled");
    PathBuf::from(p)
}

/// Renames a jar to `<jar>.disabled`; an older `.disabled` copy in the way
/// goes to the backups. Nothing to back up for the jar itself: it stays.
fn disable(path: &Path, backup_dir: &Path) -> anyhow::Result<Option<PathBuf>> {
    let to = disabled_path(path);
    let saved = backup(&to, backup_dir)?;
    fs::rename(path, &to)?;
    Ok(saved)
}

/// Renames `<jar>.disabled` back to the jar. A copy that's on already
/// wins: the off one is then a leftover and goes to the backups.
fn enable(path: &Path, backup_dir: &Path) -> anyhow::Result<Option<PathBuf>> {
    let off = disabled_path(path);
    if path.exists() {
        return backup(&off, backup_dir);
    }
    fs::rename(&off, path)?;
    Ok(None)
}

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

    #[test]
    fn progress_counts_bytes_copied_and_ends_full() {
        let root = std::env::temp_dir().join(format!("hopper-test-progress-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (client, server) = (root.join("client"), root.join("server/mods"));
        fs::create_dir_all(&client).unwrap();
        fs::create_dir_all(&server).unwrap();
        fs::write(client.join("big.jar"), vec![0u8; 5000]).unwrap();
        fs::write(server.join("old.jar"), b"x").unwrap();
        let ops = vec![
            SyncOp::Copy { src: client.join("big.jar"), dst: server.join("big.jar") },
            SyncOp::Delete { path: server.join("old.jar") },
        ];
        // A big file weighs its bytes, anything else one.
        assert_eq!(ops.iter().map(op_weight).collect::<Vec<_>>(), vec![5000, 1]);
        let seen = std::cell::RefCell::new(Vec::new());
        let r = execute_with(ops, &root.join("server"), &|d, t| seen.borrow_mut().push((d, t)));
        assert!(r.iter().all(|r| r.ok));
        assert_eq!(seen.into_inner().last(), Some(&(5001, 5001)));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn disable_renames_the_jar_and_keeps_an_old_off_copy() {
        let root = std::env::temp_dir().join(format!("hopper-test-off-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mods = root.join("mods");
        fs::create_dir_all(&mods).unwrap();
        fs::write(mods.join("a.jar"), b"now").unwrap();
        fs::write(mods.join("a.jar.disabled"), b"older").unwrap();
        let r = execute(vec![SyncOp::Disable { path: mods.join("a.jar") }], &root);
        assert!(r[0].ok, "{:?}", r[0].error);
        assert!(!mods.join("a.jar").exists());
        assert_eq!(fs::read(mods.join("a.jar.disabled")).unwrap(), b"now");
        assert_eq!(fs::read(r[0].backup.as_ref().unwrap()).unwrap(), b"older");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn enable_renames_the_off_jar_back() {
        let root = std::env::temp_dir().join(format!("hopper-test-on-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mods = root.join("mods");
        fs::create_dir_all(&mods).unwrap();
        fs::write(mods.join("a.jar.disabled"), b"off").unwrap();
        let r = execute(vec![SyncOp::Enable { path: mods.join("a.jar") }], &root);
        assert!(r[0].ok, "{:?}", r[0].error);
        assert!(!mods.join("a.jar.disabled").exists());
        assert_eq!(fs::read(mods.join("a.jar")).unwrap(), b"off");
        assert!(r[0].backup.is_none());
        // Already on beside a leftover off copy: the jar that's on stays, the
        // off copy goes to the backups.
        fs::write(mods.join("b.jar"), b"on").unwrap();
        fs::write(mods.join("b.jar.disabled"), b"old").unwrap();
        let r = execute(vec![SyncOp::Enable { path: mods.join("b.jar") }], &root);
        assert!(r[0].ok, "{:?}", r[0].error);
        assert_eq!(fs::read(mods.join("b.jar")).unwrap(), b"on");
        assert!(!mods.join("b.jar.disabled").exists());
        assert_eq!(fs::read(r[0].backup.as_ref().unwrap()).unwrap(), b"old");
        let _ = fs::remove_dir_all(&root);
    }
}
