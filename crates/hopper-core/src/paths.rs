//! Where Hopper keeps its own files: settings, accounts, caches, downloads.
//!
//! Everything lives in one folder next to hopper.exe, [`FOLDER`], so a user
//! can see (and pick the drive of) all of it. Hopper used to spread it over
//! `%APPDATA%\Hopper` (data) and `%LOCALAPPDATA%\Hopper` (cache): a copy
//! that still has that old layout keeps using it until the user moves it
//! ([`move_to`]); a new copy starts out next to its exe, and the user can
//! move it anywhere later.
//!
//! Resolved once per run:
//! 1. `HOPPER_DATA_DIR`, when set;
//! 2. the folder a [`LINK`] file next to the exe names (one the user
//!    picked);
//! 3. [`FOLDER`] next to the exe, when it exists;
//! 4. a dev build (an exe under a cargo `target` folder): where the user's
//!    Hopper moved its data ([`POINTER`]), else the old layout;
//! 5. the old layout, when it has settings;
//! 6. [`FOLDER`] next to the exe, created now (the old layout when the
//!    exe's folder can't be written).

use anyhow::Context as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The folder next to hopper.exe.
pub const FOLDER: &str = "Hopper Data";

/// Left in the old data folder after a move: the path the data went to,
/// so dev builds (and a curious user) can find it.
pub const POINTER: &str = "moved-to.txt";

/// Next to hopper.exe when Hopper's files are in a folder the user picked
/// elsewhere: that folder's path.
pub const LINK: &str = "hopper-data.txt";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dirs {
    data: PathBuf,
    cache: PathBuf,
    /// `Some(root)` when everything sits under one root folder.
    root: Option<PathBuf>,
}

impl Dirs {
    /// Under one root: `root/data` and `root/cache`.
    pub fn portable(root: PathBuf) -> Self {
        Self {
            data: root.join("data"),
            cache: root.join("cache"),
            root: Some(root),
        }
    }

    /// Settings, accounts, downloaded Java and the like.
    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    /// Things Hopper can download again: pack zips, images, lookups.
    pub fn cache_dir(&self) -> &Path {
        &self.cache
    }

    /// The one folder holding everything, when Hopper uses one.
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// Temporary downloads (installers, mod files before they're placed).
    pub fn temp_dir(&self) -> PathBuf {
        self.cache.join("tmp")
    }
}

/// The old layout: `%APPDATA%\Hopper\data` and `%LOCALAPPDATA%\Hopper\cache`.
/// `HOPPER_LEGACY_ROOT` stands in for the user folder (`<it>\Roaming`,
/// `<it>\Local`), to try a move without touching real files.
pub fn old_layout() -> Option<Dirs> {
    if let Some(r) = std::env::var_os("HOPPER_LEGACY_ROOT").filter(|r| !r.is_empty()) {
        let r = PathBuf::from(r);
        return Some(Dirs {
            data: r.join("Roaming").join("Hopper").join("data"),
            cache: r.join("Local").join("Hopper").join("cache"),
            root: None,
        });
    }
    directories::ProjectDirs::from("", "", "Hopper").map(|d| Dirs {
        data: d.data_dir().to_path_buf(),
        cache: d.cache_dir().to_path_buf(),
        root: None,
    })
}

/// Hopper's folders for this run.
pub fn dirs() -> Option<&'static Dirs> {
    static D: OnceLock<Option<Dirs>> = OnceLock::new();
    D.get_or_init(resolve).as_ref()
}

/// Temporary downloads: under Hopper's folder, else the system's temp.
pub fn temp_dir() -> PathBuf {
    let d = dirs().map_or_else(std::env::temp_dir, Dirs::temp_dir);
    let _ = fs::create_dir_all(&d);
    d
}

/// The exe's folder.
pub fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
}

/// A build run from a cargo `target` folder.
pub fn dev_build() -> bool {
    std::env::current_exe().is_ok_and(|e| is_dev_exe(&e))
}

fn is_dev_exe(exe: &Path) -> bool {
    exe.components().any(|c| c.as_os_str().eq_ignore_ascii_case("target"))
}

/// The folder `HOPPER_DATA_DIR` names, when set: everything stays in it.
pub fn forced_root() -> Option<PathBuf> {
    std::env::var_os("HOPPER_DATA_DIR").filter(|p| !p.is_empty()).map(PathBuf::from)
}

fn resolve() -> Option<Dirs> {
    if let Some(p) = forced_root() {
        return Some(Dirs::portable(p));
    }
    let exe = std::env::current_exe().ok();
    choose(
        exe.as_deref().and_then(Path::parent),
        exe.as_deref().is_some_and(is_dev_exe),
        old_layout(),
    )
}

/// The folder a [`LINK`] next to the exe names, if any.
fn linked(exe_dir: &Path) -> Option<PathBuf> {
    let s = fs::read_to_string(exe_dir.join(LINK)).ok()?;
    let p = PathBuf::from(s.trim());
    (p.is_absolute()).then_some(p)
}

/// [`resolve`] without the environment: `exe_dir` is the exe's folder.
fn choose(exe_dir: Option<&Path>, dev: bool, old: Option<Dirs>) -> Option<Dirs> {
    if let Some(p) = exe_dir.and_then(linked) {
        return Some(Dirs::portable(p));
    }
    if let Some(here) = exe_dir.map(|d| d.join(FOLDER)).filter(|h| h.is_dir()) {
        return Some(Dirs::portable(here));
    }
    let moved = old
        .as_ref()
        .and_then(|o| fs::read_to_string(o.data.join(POINTER)).ok())
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| p.is_dir());
    if dev {
        return moved.map(Dirs::portable).or(old);
    }
    if old.as_ref().is_some_and(has_settings) {
        return old;
    }
    if let Some(dir) = exe_dir {
        let here = dir.join(FOLDER);
        if fs::create_dir_all(&here).is_ok() {
            return Some(Dirs::portable(here));
        }
    }
    old
}

fn has_settings(d: &Dirs) -> bool {
    d.data.join("settings.json").is_file()
}

/// Where "next to hopper.exe" is.
pub fn beside_exe() -> Option<PathBuf> {
    Some(exe_dir()?.join(FOLDER))
}

/// When Hopper still uses the old layout and could move it next to the
/// exe: (the old folders' size in bytes, where they'd go).
pub fn move_offer() -> Option<(u64, PathBuf)> {
    let d = dirs()?;
    if d.root().is_some() || dev_build() {
        return None;
    }
    let dest = beside_exe()?;
    check_dest(&dest).ok()?;
    Some((used(), dest))
}

/// Bytes in Hopper's folders now.
pub fn used() -> u64 {
    dirs().map_or(0, |d| dir_size(&d.data) + dir_size(&d.cache))
}

/// Whether Hopper's files could move to `dest`: a folder that is empty or
/// new, can be written, and isn't inside the current one (or around it).
pub fn check_dest(dest: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(dest.is_absolute(), "Pick a full folder path.");
    if let Some(d) = dirs() {
        let now = d.root().unwrap_or(&d.data);
        anyhow::ensure!(dest != now, "Hopper's files are already there.");
        anyhow::ensure!(
            !dest.starts_with(now) && !now.starts_with(dest) && !d.data.starts_with(dest),
            "Pick a folder outside Hopper's current one."
        );
    }
    if dest.exists() {
        anyhow::ensure!(dest.is_dir(), "{} is a file.", dest.display());
        let empty = fs::read_dir(dest).map(|mut r| r.next().is_none()).unwrap_or(false);
        anyhow::ensure!(empty, "{} isn't empty. Pick an empty or new folder.", dest.display());
    }
    // Can it be written? (Program Files can't.)
    let existed = dest.exists();
    fs::create_dir_all(dest).with_context(|| format!("Hopper can't make {}", dest.display()))?;
    let probe = dest.join(".hopper-write-test");
    let w = fs::write(&probe, b"");
    let _ = fs::remove_file(&probe);
    if !existed {
        let _ = fs::remove_dir(dest);
    }
    w.with_context(|| format!("Hopper can't write in {}", dest.display()))?;
    Ok(())
}

pub fn dir_size(dir: &Path) -> u64 {
    walk(dir).iter().map(|(_, len)| len).sum()
}

/// Files under `dir` (relative path, size), skipping symlinks.
fn walk(dir: &Path) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let Ok(rd) = fs::read_dir(dir.join(&rel)) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let r = rel.join(e.file_name());
            if ft.is_dir() {
                stack.push(r);
            } else if ft.is_file() {
                out.push((r, e.metadata().map_or(0, |m| m.len())));
            }
        }
    }
    out
}

/// Copies Hopper's folders into `dest` (`dest/data`, `dest/cache`),
/// checking every file's size, then points the exe at `dest`: a [`LINK`]
/// next to it, unless `dest` is [`FOLDER`] right there. Leaves [`POINTER`]
/// in the old layout's data folder too, for dev builds. The old folders
/// stay until [`remove_old`] runs after a restart. `progress(done, total)`
/// counts bytes. Leaves nothing half-made behind on failure.
pub fn move_to(dest: &Path, progress: impl FnMut(u64, u64)) -> anyhow::Result<()> {
    check_dest(dest)?;
    let from = dirs().context("Hopper has no folder")?.clone();
    copy_layout(&from, dest, progress)?;
    if let Some(dir) = exe_dir() {
        point(&dir, dest)?;
    }
    // An installed copy elsewhere follows too.
    if let Some(installed) = crate::setup::installed_exe().and_then(|e| e.parent().map(Path::to_path_buf))
        && Some(&installed) != exe_dir().as_ref()
    {
        let _ = point(&installed, dest);
    }
    if let Some(old) = old_layout() {
        let _ = fs::create_dir_all(&old.data);
        let _ = fs::write(old.data.join(POINTER), dest.to_string_lossy().as_bytes());
    }
    Ok(())
}

/// Makes the exe in `exe_dir` use `dest`: a [`LINK`] file, or none when
/// `dest` is the [`FOLDER`] beside it.
pub fn point(exe_dir: &Path, dest: &Path) -> anyhow::Result<()> {
    let link = exe_dir.join(LINK);
    if dest == exe_dir.join(FOLDER) {
        if link.exists() {
            fs::remove_file(&link)?;
        }
        return Ok(());
    }
    fs::write(&link, dest.to_string_lossy().as_bytes())
        .with_context(|| format!("writing {}", link.display()))
}

fn copy_layout(old: &Dirs, dest: &Path, mut progress: impl FnMut(u64, u64)) -> anyhow::Result<()> {
    let new = Dirs::portable(dest.to_path_buf());
    let existed = dest.exists();
    let r = (|| -> anyhow::Result<()> {
        let pairs = [(&old.data, &new.data), (&old.cache, &new.cache)];
        let files: Vec<(usize, PathBuf, u64)> = pairs
            .iter()
            .enumerate()
            .flat_map(|(i, (from, _))| walk(from).into_iter().map(move |(r, l)| (i, r, l)))
            .collect();
        let total: u64 = files.iter().map(|f| f.2).sum();
        let mut done = 0;
        for (i, rel, len) in &files {
            let (from, to) = pairs[*i];
            if rel.as_os_str() == POINTER {
                continue;
            }
            let (src, dst) = (from.join(rel), to.join(rel));
            fs::create_dir_all(dst.parent().unwrap_or(to))?;
            let n = fs::copy(&src, &dst).with_context(|| format!("copying {}", src.display()))?;
            anyhow::ensure!(n == *len, "{} copied short", src.display());
            done += len;
            progress(done, total);
        }
        fs::create_dir_all(&new.data)?;
        fs::create_dir_all(&new.cache)?;
        Ok(())
    })();
    if r.is_err() && !existed {
        let _ = fs::remove_dir_all(dest);
    }
    r
}

/// After a move and restart: deletes what Hopper moved away from. `old` is
/// the folder it used before (`None`: the old AppData layout, whose data
/// folder keeps only [`POINTER`]). Nothing happens unless this run really
/// uses another place. Retries for a while, since the Hopper that moved
/// the files may still be closing.
pub fn remove_old(old: Option<&Path>) {
    let Some(d) = dirs() else { return };
    let Some(now) = d.root() else { return };
    for _ in 0..20 {
        let done = match old {
            Some(o) => {
                if o == now || now.starts_with(o) || o.starts_with(now) || !o.exists() {
                    return;
                }
                fs::remove_dir_all(o).is_ok()
            }
            None => {
                let Some(l) = old_layout() else { return };
                if d.data_dir() == l.data {
                    return;
                }
                let a = clear_except(&l.data, POINTER);
                let b = fs::remove_dir_all(&l.cache).or_else(|e| {
                    if l.cache.exists() { Err(e) } else { Ok(()) }
                });
                if a.is_ok() && b.is_ok() {
                    // Cache sat in %LOCALAPPDATA%\Hopper: drop that now-empty folder.
                    if let Some(p) = l.cache.parent() {
                        let _ = fs::remove_dir(p);
                    }
                    true
                } else {
                    false
                }
            }
        };
        if done {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    log::warn!("couldn't remove all of Hopper's old folder");
}

fn clear_except(dir: &Path, keep: &str) -> std::io::Result<()> {
    let Ok(rd) = fs::read_dir(dir) else { return Ok(()) };
    let mut res = Ok(());
    for e in rd.flatten() {
        if e.file_name() == keep {
            continue;
        }
        let p = e.path();
        let r = if p.is_dir() { fs::remove_dir_all(&p) } else { fs::remove_file(&p) };
        if r.is_err() {
            res = r;
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-paths-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn old_in(base: &Path) -> Dirs {
        Dirs {
            data: base.join("roaming"),
            cache: base.join("local"),
            root: None,
        }
    }

    #[test]
    fn picks_the_right_place() {
        let base = tmp("choose");
        let exe = base.join("app");
        fs::create_dir_all(&exe).unwrap();
        let old = old_in(&base);

        // Fresh: a folder next to the exe.
        let d = choose(Some(&exe), false, Some(old.clone())).unwrap();
        assert_eq!(d.root(), Some(exe.join(FOLDER).as_path()));
        assert!(exe.join(FOLDER).is_dir());
        fs::remove_dir_all(exe.join(FOLDER)).unwrap();

        // Old layout with settings: keep using it (the move is offered).
        fs::create_dir_all(&old.data).unwrap();
        fs::write(old.data.join("settings.json"), "{}").unwrap();
        assert_eq!(choose(Some(&exe), false, Some(old.clone())), Some(old.clone()));

        // Moved: the folder next to the exe wins.
        fs::create_dir_all(exe.join(FOLDER)).unwrap();
        assert_eq!(
            choose(Some(&exe), false, Some(old.clone())).unwrap().root(),
            Some(exe.join(FOLDER).as_path())
        );

        // A dev build elsewhere follows the pointer.
        let dev = base.join("target");
        fs::create_dir_all(&dev).unwrap();
        fs::write(old.data.join(POINTER), exe.join(FOLDER).to_string_lossy().as_bytes()).unwrap();
        assert_eq!(
            choose(Some(&dev), true, Some(old.clone())).unwrap().root(),
            Some(exe.join(FOLDER).as_path())
        );
        assert!(!dev.join(FOLDER).exists(), "dev builds never make their own");

        // A picked folder, named by the link file, beats the one beside it.
        let picked = base.join("picked");
        point(&exe, &picked).unwrap();
        assert_eq!(choose(Some(&exe), false, Some(old.clone())).unwrap().root(), Some(picked.as_path()));
        point(&exe, &exe.join(FOLDER)).unwrap();
        assert!(!exe.join(LINK).exists(), "back beside the exe: no link");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn copies_everything_and_leaves_a_pointer() {
        let base = tmp("copy");
        let old = old_in(&base);
        fs::create_dir_all(old.data.join("java/jdk")).unwrap();
        fs::create_dir_all(old.cache.join("packs")).unwrap();
        fs::write(old.data.join("settings.json"), "{}").unwrap();
        fs::write(old.data.join("java/jdk/release"), "21").unwrap();
        fs::write(old.cache.join("packs/1.zip"), vec![7u8; 5000]).unwrap();
        let dest = base.join("app").join(FOLDER);
        let mut last = (0, 0);
        copy_layout(&old, &dest, |d, t| last = (d, t)).unwrap();
        assert_eq!(last, (5004, 5004));
        assert_eq!(fs::read_to_string(dest.join("data/settings.json")).unwrap(), "{}");
        assert_eq!(fs::read_to_string(dest.join("data/java/jdk/release")).unwrap(), "21");
        assert_eq!(fs::read(dest.join("cache/packs/1.zip")).unwrap().len(), 5000);
        // A pointer in the old folder isn't copied.
        fs::write(old.data.join(POINTER), "x").unwrap();
        fs::remove_dir_all(&dest).unwrap();
        copy_layout(&old, &dest, |_, _| {}).unwrap();
        assert!(!dest.join("data").join(POINTER).exists());
        clear_except(&old.data, POINTER).unwrap();
        assert_eq!(fs::read_dir(&old.data).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    #[cfg_attr(not(windows), ignore = "uses Windows paths and tools")]
    fn dev_exe() {
        assert!(is_dev_exe(Path::new(r"X:\Dev\hopper\target\perf\fast\hopper.exe")));
        assert!(!is_dev_exe(Path::new(r"D:\Games\Hopper\hopper.exe")));
    }
}
