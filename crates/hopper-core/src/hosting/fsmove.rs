//! Moving and deleting a server's folder without leaving it half done.
//!
//! On Windows a folder can't be renamed while anything has a file in it, or
//! the folder itself, open ("Access is denied", os error 5; or a sharing
//! violation, 32). The holder is often brief (a file watcher letting go, a
//! virus scanner), so the move is retried a few times; when it still fails
//! the Restart Manager says which program has the files, and the error names
//! it. Nothing is ever deleted in place as a fallback to a failed move.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Waits between tries of a move that's refused: about 3 seconds in all.
pub const RETRY_DELAYS: [Duration; 6] = [
    Duration::from_millis(100),
    Duration::from_millis(250),
    Duration::from_millis(450),
    Duration::from_millis(700),
    Duration::from_millis(900),
    Duration::from_millis(700),
];

/// Prefix of a folder that is being deleted for good. It is renamed to this
/// first, so a half-deleted server can't pass for a server, and anything
/// left over is swept at the next start.
pub const DELETING_PREFIX: &str = ".hopper-deleting-";

/// A program that has files of a folder open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    pub name: String,
    pub pid: u32,
}

/// Windows refuses with 5 (access denied) or 32 (sharing violation) while
/// something holds the folder.
pub fn is_locked(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(5) | Some(32))
}

/// Renames `src` to `dest`. Before each try `release` lets go of whatever
/// the panel itself holds there; a refusal because of a lock is retried
/// after each of `delays`. Other errors come back at once.
pub fn rename_retry(
    src: &Path,
    dest: &Path,
    release: &dyn Fn(),
    rename: &dyn Fn(&Path, &Path) -> io::Result<()>,
    delays: &[Duration],
) -> io::Result<()> {
    let mut i = 0;
    loop {
        release();
        match rename(src, dest) {
            Ok(()) => return Ok(()),
            Err(e) if is_locked(&e) && i < delays.len() => {
                std::thread::sleep(delays[i]);
                i += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// The words for a move that was refused. `holders` is who has files open
/// (empty when nobody was found).
pub fn locked_message(what: &str, holders: &[Holder], err: &io::Error) -> String {
    if !is_locked(err) {
        return format!("Couldn't move {what}: {err}. Nothing was changed.");
    }
    if holders.is_empty() {
        return format!(
            "Couldn't move {what}: something has the folder open (a folder is held, so no program can be named). Close Explorer windows and anything watching it, then try again. Nothing was changed."
        );
    }
    let list = holders.iter().map(|h| format!("{} (pid {})", h.name, h.pid)).collect::<Vec<_>>().join(", ");
    let (verb, them) = if holders.len() == 1 { ("is", "it") } else { ("are", "them") };
    format!("Couldn't move {what}: {list} {verb} using files in it. Close {them} and try again. Nothing was changed.")
}

/// Moves `src` to `dest` (same drive), all or nothing. On a refusal the
/// error names who holds the folder, when Windows can tell.
pub fn move_folder(what: &str, src: &Path, dest: &Path, release: &dyn Fn()) -> anyhow::Result<()> {
    move_folder_with(what, src, dest, release, &|a, b| std::fs::rename(a, b), &RETRY_DELAYS, &holders)
}

pub fn move_folder_with(
    what: &str,
    src: &Path,
    dest: &Path,
    release: &dyn Fn(),
    rename: &dyn Fn(&Path, &Path) -> io::Result<()>,
    delays: &[Duration],
    find_holders: &dyn Fn(&Path) -> Vec<Holder>,
) -> anyhow::Result<()> {
    match rename_retry(src, dest, release, rename, delays) {
        Ok(()) => Ok(()),
        Err(e) => {
            let who = if is_locked(&e) { find_holders(src) } else { Vec::new() };
            log::warn!("moving {} to {}: {e}", src.display(), dest.display());
            Err(anyhow::anyhow!(locked_message(what, &who, &e)))
        }
    }
}

// ---- who has it open (Windows Restart Manager) ------------------------------------------------

/// The programs that have a file under `dir` open, by the Windows Restart
/// Manager. Empty when none is found or off Windows. A folder handle (an
/// Explorer window showing it) isn't a file, so it never shows here.
pub fn holders(dir: &Path) -> Vec<Holder> {
    #[cfg(windows)]
    {
        win::holders(dir)
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        Vec::new()
    }
}

#[cfg(windows)]
mod win {
    use super::Holder;
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use windows::Win32::Foundation::ERROR_MORE_DATA;
    use windows::Win32::System::RestartManager::{
        CCH_RM_SESSION_KEY, RM_PROCESS_INFO, RmEndSession, RmGetList, RmRegisterResources, RmStartSession,
    };
    use windows::core::{PCWSTR, PWSTR};

    /// Files given to one Restart Manager session.
    const CHUNK: usize = 400;
    /// Stop looking after this many files (a huge folder).
    const MAX_FILES: usize = 200_000;

    fn files_under(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut todo = vec![dir.to_path_buf()];
        while let Some(d) = todo.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let Ok(t) = e.file_type() else { continue };
                if t.is_dir() {
                    todo.push(e.path());
                } else if t.is_file() {
                    out.push(e.path());
                    if out.len() >= MAX_FILES {
                        return out;
                    }
                }
            }
        }
        out
    }

    pub fn holders(dir: &Path) -> Vec<Holder> {
        let files = files_under(dir);
        let mut found: Vec<Holder> = Vec::new();
        for chunk in files.chunks(CHUNK) {
            // SAFETY: the buffers outlive the calls; the session is ended below.
            unsafe { chunk_holders(chunk, &mut found) };
        }
        found
    }

    unsafe fn chunk_holders(chunk: &[PathBuf], found: &mut Vec<Holder>) {
        let mut session = 0u32;
        let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
        if unsafe { RmStartSession(&mut session, None, PWSTR(key.as_mut_ptr())) }.0 != 0 {
            return;
        }
        let wide: Vec<Vec<u16>> = chunk.iter().map(|p| p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()).collect();
        let ptrs: Vec<PCWSTR> = wide.iter().map(|w| PCWSTR(w.as_ptr())).collect();
        if unsafe { RmRegisterResources(session, Some(&ptrs), None, None) }.0 == 0 {
            let (mut needed, mut have, mut reasons) = (0u32, 0u32, 0u32);
            let first = unsafe { RmGetList(session, &mut needed, &mut have, None, &mut reasons) };
            if first.0 == 0 || first == ERROR_MORE_DATA {
                let mut infos = vec![RM_PROCESS_INFO::default(); needed.max(1) as usize + 4];
                have = infos.len() as u32;
                if unsafe { RmGetList(session, &mut needed, &mut have, Some(infos.as_mut_ptr()), &mut reasons) }.0 == 0 {
                    for i in infos.iter().take(have as usize) {
                        let end = i.strAppName.iter().position(|c| *c == 0).unwrap_or(i.strAppName.len());
                        let name = String::from_utf16_lossy(&i.strAppName[..end]);
                        let pid = i.Process.dwProcessId;
                        if !found.iter().any(|h| h.pid == pid) {
                            found.push(Holder { name, pid });
                        }
                    }
                }
            }
        }
        let _ = unsafe { RmEndSession(session) };
    }
}

// ---- deleting for good -----------------------------------------------------------------------

/// Checks that `target` may be deleted for good: a real folder (not a link)
/// that's a direct child of the servers folder `root`, and not a folder the
/// panel keeps for itself. Returns its canonical path.
pub fn permanent_target(root: &Path, target: &Path) -> Result<PathBuf, String> {
    let root_c = root.canonicalize().map_err(|e| format!("The servers folder isn't there: {e}"))?;
    if root_c.parent().is_none() {
        return Err("The servers folder is a whole drive: Hopper won't delete anything in it for good.".into());
    }
    let meta = std::fs::symlink_metadata(target).map_err(|e| format!("The server's folder isn't there: {e}"))?;
    if meta.file_type().is_symlink() || is_reparse(&meta) {
        return Err("The server's folder is a link to somewhere else: Hopper won't delete it for good.".into());
    }
    if !meta.is_dir() {
        return Err("The server's folder isn't a folder.".into());
    }
    let t_c = target.canonicalize().map_err(|e| format!("The server's folder isn't there: {e}"))?;
    if t_c == root_c || t_c.parent() != Some(root_c.as_path()) {
        return Err("That folder isn't directly inside the servers folder: Hopper won't delete it for good.".into());
    }
    let name = t_c.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if name.is_empty() || name.starts_with(".hopper-") {
        return Err("That folder is one the panel keeps for itself.".into());
    }
    Ok(t_c)
}

#[cfg(windows)]
fn is_reparse(m: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    m.file_attributes() & 0x400 != 0
}
#[cfg(not(windows))]
fn is_reparse(_: &std::fs::Metadata) -> bool {
    false
}

fn clear_readonly(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let Ok(t) = e.file_type() else { continue };
        if t.is_symlink() {
            continue;
        }
        if let Ok(m) = e.metadata() {
            let mut p = m.permissions();
            if p.readonly() {
                #[allow(clippy::permissions_set_readonly_false)]
                p.set_readonly(false);
                let _ = std::fs::set_permissions(e.path(), p);
            }
        }
        if t.is_dir() {
            clear_readonly(&e.path());
        }
    }
}

/// How many entries are left under `dir`.
pub fn count_left(dir: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.flatten().map(|e| 1 + if e.file_type().is_ok_and(|t| t.is_dir()) { count_left(&e.path()) } else { 0 }).sum()
}

/// Removes a folder tree, clearing read-only flags and retrying a few times
/// (a scanner may hold a file for a moment). On failure says what is left.
pub fn remove_tree(dir: &Path) -> Result<(), String> {
    let mut last = None;
    for i in 0..4 {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => return Ok(()),
            Err(e) if !dir.exists() && e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                last = Some(e);
                clear_readonly(dir);
                std::thread::sleep(Duration::from_millis(250 * (i + 1)));
            }
        }
    }
    let left = count_left(dir);
    Err(format!("{} files and folders are left in {} ({}).", left, dir.display(), last.map(|e| e.to_string()).unwrap_or_default()))
}

/// Removes folders a permanent delete left behind. Returns how many.
pub fn sweep_deleting(servers_root: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(servers_root) else { return 0 };
    let mut n = 0;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with(DELETING_PREFIX) || !e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        match remove_tree(&e.path()) {
            Ok(()) => {
                log::info!("removed {name}, left over from a delete");
                n += 1;
            }
            Err(msg) => log::warn!("cleaning up {name}: {msg}"),
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn denied() -> io::Error {
        io::Error::from_raw_os_error(5)
    }
    const FAST: [Duration; 3] = [Duration::ZERO; 3];

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopperd-fsmove-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn retries_then_succeeds_and_releases_each_time() {
        let (tries, released) = (Cell::new(0), Cell::new(0));
        let r = rename_retry(
            Path::new("a"),
            Path::new("b"),
            &|| released.set(released.get() + 1),
            &|_, _| {
                tries.set(tries.get() + 1);
                if tries.get() <= 2 { Err(denied()) } else { Ok(()) }
            },
            &FAST,
        );
        assert!(r.is_ok());
        assert_eq!((tries.get(), released.get()), (3, 3));
    }

    #[test]
    fn gives_up_after_the_last_delay_and_names_the_holder() {
        let tries = Cell::new(0);
        let who = |_: &Path| vec![Holder { name: "Notepad".into(), pid: 42 }];
        let err = move_folder_with("all the mons", Path::new("a"), Path::new("b"), &|| {}, &|_, _| {
            tries.set(tries.get() + 1);
            Err(denied())
        }, &FAST, &who)
        .unwrap_err()
        .to_string();
        assert_eq!(tries.get(), 4);
        assert_eq!(err, "Couldn't move all the mons: Notepad (pid 42) is using files in it. Close it and try again. Nothing was changed.");
    }

    #[test]
    fn nobody_found_says_a_folder_is_held() {
        let err = move_folder_with("x", Path::new("a"), Path::new("b"), &|| {}, &|_, _| Err(io::Error::from_raw_os_error(32)), &FAST, &|_| Vec::new())
            .unwrap_err()
            .to_string();
        assert!(err.contains("close Explorer windows") || err.contains("Close Explorer windows"), "{err}");
        assert!(err.ends_with("Nothing was changed."));
    }

    #[test]
    fn other_errors_are_not_retried() {
        let tries = Cell::new(0);
        let r = rename_retry(Path::new("a"), Path::new("b"), &|| {}, &|_, _| {
            tries.set(tries.get() + 1);
            Err(io::Error::from(io::ErrorKind::NotFound))
        }, &FAST);
        assert!(r.is_err());
        assert_eq!(tries.get(), 1);
    }

    #[test]
    fn permanent_guards() {
        let root = scratch("guard");
        let srv = root.join("srv");
        std::fs::create_dir_all(srv.join("world")).unwrap();
        assert!(permanent_target(&root, &srv).is_ok());
        // The root itself, and a drive root.
        assert!(permanent_target(&root, &root).is_err());
        let drive = root.ancestors().last().unwrap().to_path_buf();
        assert!(permanent_target(&drive, &root).is_err() || root.parent() == Some(drive.as_path()));
        assert!(permanent_target(&drive, &drive).is_err());
        // Outside the root; and deeper than a direct child.
        let other = scratch("guard-other");
        assert!(permanent_target(&root, &other).is_err());
        assert!(permanent_target(&root, &srv.join("world")).is_err());
        // A sibling that merely shares the root's name as a prefix.
        let sib = PathBuf::from(format!("{}-sibling", root.display()));
        std::fs::create_dir_all(&sib).unwrap();
        assert!(permanent_target(&root, &sib).is_err());
        // The panel's own folders.
        std::fs::create_dir_all(root.join(".hopper-deleted")).unwrap();
        assert!(permanent_target(&root, &root.join(".hopper-deleted")).is_err());
        // Not there at all.
        assert!(permanent_target(&root, &root.join("nope")).is_err());
        for d in [&root, &other, &sib] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[cfg(windows)]
    #[test]
    fn link_is_refused() {
        let root = scratch("link");
        let real = scratch("link-real");
        let link = root.join("junction");
        // A junction needs no admin rights.
        let ok = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&link).arg(&real).output().map(|o| o.status.success()).unwrap_or(false);
        if ok {
            assert!(permanent_target(&root, &link).is_err());
            let _ = std::fs::remove_dir(&link);
        }
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&real);
    }

    #[test]
    fn sweep_removes_only_deleting_folders_even_read_only() {
        let root = scratch("sweep");
        let left = root.join(format!("{DELETING_PREFIX}1"));
        std::fs::create_dir_all(left.join("world")).unwrap();
        let f = left.join("world").join("level.dat");
        std::fs::write(&f, b"x").unwrap();
        let mut p = std::fs::metadata(&f).unwrap().permissions();
        p.set_readonly(true);
        std::fs::set_permissions(&f, p).unwrap();
        let keep = root.join("real-server");
        std::fs::create_dir_all(&keep).unwrap();
        assert_eq!(sweep_deleting(&root), 1);
        assert!(!left.exists());
        assert!(keep.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn restart_manager_names_this_process() {
        let dir = scratch("rm");
        let f = dir.join("held.txt");
        std::fs::write(&f, b"x").unwrap();
        let _open = std::fs::File::open(&f).unwrap();
        let found = holders(&dir);
        assert!(found.iter().any(|h| h.pid == std::process::id()), "{found:?}");
        drop(_open);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
