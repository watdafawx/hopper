//! Whole-instance actions: duplicate, delete, repair, worlds, screenshots,
//! and checking a pack for a newer version.

use crate::cf;
use crate::instances::{Instance, PackRef};
use crate::jobs::Job;
use anyhow::Context;
use std::fs;
use std::path::{Path, PathBuf};

/// Copies an instance folder to a new one named "<name> (copy)".
pub fn duplicate(inst: &Instance, job: &Job) -> anyhow::Result<PathBuf> {
    let parent = inst
        .root
        .parent()
        .context("instance has no parent folder")?;
    copy_to(inst, parent, &format!("{} (copy)", inst.name), job)
}

/// Copies another launcher's instance into Hopper's instances folder, as an
/// instance of Hopper's own.
pub fn copy_into(inst: &Instance, instances_root: &Path, job: &Job) -> anyhow::Result<PathBuf> {
    copy_to(inst, instances_root, &inst.name, job)
}

fn copy_to(inst: &Instance, parent: &Path, name: &str, job: &Job) -> anyhow::Result<PathBuf> {
    let name = name.to_string();
    let mut dest = parent.join(Instance::folder_name(&name));
    let mut n = 2;
    while dest.exists() {
        dest = parent.join(Instance::folder_name(&format!("{name} ({n})")));
        n += 1;
    }
    let files = walk(&inst.root);
    for (i, f) in files.iter().enumerate() {
        let rel = f.strip_prefix(&inst.root)?;
        // Backups and caches are per instance; don't carry them over.
        if rel.starts_with(crate::sync::BACKUP_DIR)
            || rel.starts_with("cache")
            || rel.starts_with("logs")
        {
            continue;
        }
        let out = dest.join(rel);
        fs::create_dir_all(out.parent().unwrap_or(&dest))?;
        fs::copy(f, &out).with_context(|| format!("copying {}", f.display()))?;
        if i % 50 == 0 {
            job.progress(i, files.len());
            job.detail(format!("Copied {i} of {} files", files.len()));
        }
    }
    // A copy of another launcher's instance has no Hopper file yet when it
    // was never saved; write the original's.
    if !dest.join(crate::instances::HOPPER_FILE).exists() {
        let mut own = inst.clone();
        own.root = dest.clone();
        own.save()?;
    }
    let mut copy = crate::instances::load(&dest).context("the copy is not a readable instance")?;
    copy.name = name;
    copy.source.clear();
    copy.save()?;
    Ok(dest)
}

/// Where a deleted instance went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeleteOutcome {
    /// In the Windows Recycle Bin.
    RecycleBin,
    /// Gone for good (the "skip the Recycle Bin" setting).
    Deleted,
    /// On a network drive, which has no Recycle Bin (Windows would delete
    /// it for good): moved aside to this folder instead, under
    /// `<parent>\.hopper-trash`.
    MovedAside(PathBuf),
}

/// Folder beside instances that deleted instances on network drives go to.
pub const HOPPER_TRASH: &str = ".hopper-trash";

/// Sends the instance folder to the Recycle Bin, or, on a network drive,
/// moves it to `<parent>\.hopper-trash\<name>-<timestamp>`.
pub fn delete(inst: &Instance) -> anyhow::Result<()> {
    delete_instance(inst, false).map(|_| ())
}

/// [`delete`], saying where the folder went so the UI can word it.
/// `permanent` skips the Recycle Bin (and `.hopper-trash`).
pub fn delete_instance(inst: &Instance, permanent: bool) -> anyhow::Result<DeleteOutcome> {
    if permanent {
        remove_for_good(&inst.root)?;
        return Ok(DeleteOutcome::Deleted);
    }
    if is_remote_drive(&inst.root) {
        return move_to_hopper_trash(&inst.root).map(DeleteOutcome::MovedAside);
    }
    trash::delete(&inst.root)
        .with_context(|| format!("moving {} to the Recycle Bin", inst.root.display()))?;
    Ok(DeleteOutcome::RecycleBin)
}

/// Deletes a file or folder outright, without the Recycle Bin. Read-only
/// files (git packs, some mods) are made writable first, or Windows refuses.
pub fn remove_for_good(path: &Path) -> anyhow::Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let r = if meta.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) };
    if r.is_err() {
        clear_readonly(path);
        if meta.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }
            .with_context(|| format!("deleting {}", path.display()))?;
    }
    Ok(())
}

#[allow(clippy::permissions_set_readonly_false)]
fn clear_readonly(path: &Path) {
    let Ok(m) = fs::symlink_metadata(path) else {
        return;
    };
    let mut perm = m.permissions();
    if perm.readonly() {
        perm.set_readonly(false);
        let _ = fs::set_permissions(path, perm);
    }
    if m.is_dir() {
        for e in fs::read_dir(path).into_iter().flatten().flatten() {
            clear_readonly(&e.path());
        }
    }
}

/// Moves `dir` to `<parent>\.hopper-trash\<name>-<timestamp>` (same volume,
/// so it is a rename). Returns the new path.
pub fn move_to_hopper_trash(dir: &Path) -> anyhow::Result<PathBuf> {
    let parent = dir.parent().context("the folder has no parent")?;
    let name = dir
        .file_name()
        .context("the folder has no name")?
        .to_string_lossy()
        .into_owned();
    let trash = parent.join(HOPPER_TRASH);
    fs::create_dir_all(&trash).with_context(|| format!("creating {}", trash.display()))?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut dest = trash.join(format!("{name}-{ts}"));
    let mut n = 2;
    while dest.exists() {
        dest = trash.join(format!("{name}-{ts}-{n}"));
        n += 1;
    }
    fs::rename(dir, &dest)
        .with_context(|| format!("moving {} to {}", dir.display(), dest.display()))?;
    Ok(dest)
}

/// Whether `path` is on a network drive (a mapped drive or a UNC path).
/// The Recycle Bin doesn't cover those.
#[cfg(windows)]
pub fn is_remote_drive(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Component, Prefix};
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let root = match abs.components().next() {
        Some(Component::Prefix(p)) => match p.kind() {
            Prefix::UNC(..) | Prefix::VerbatimUNC(..) => return true,
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) => format!("{}:\\", d as char),
            _ => return false,
        },
        _ => return false,
    };
    let wide: Vec<u16> = std::ffi::OsStr::new(&root)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call.
    let kind = unsafe {
        windows::Win32::Storage::FileSystem::GetDriveTypeW(windows::core::PCWSTR(wide.as_ptr()))
    };
    // DRIVE_REMOTE (in WindowsProgramming, not worth another feature).
    kind == 4
}

#[cfg(not(windows))]
pub fn is_remote_drive(_path: &Path) -> bool {
    false
}

/// Re-downloads tracked files that are missing from disk.
pub fn repair(inst: &Instance, job: &Job) -> anyhow::Result<usize> {
    let missing: Vec<&crate::instances::Addon> = inst
        .addons
        .iter()
        .filter(|a| a.project_id != 0 && !inst.addon_path(a).exists())
        .collect();
    let mut fixed = 0;
    // Modrinth files come straight from their version.
    for a in missing.iter().filter(|a| !a.mr_version.is_empty()) {
        job.detail(format!("Downloading {}", a.file_name));
        let v = crate::modrinth::version(&a.mr_version)?;
        if let Some(url) = crate::modrinth::version_to_file(&v).download_url {
            cf::download(&url, &inst.addon_path(a), |_| {})?;
            fixed += 1;
        }
    }
    let missing: Vec<&crate::instances::Addon> = missing
        .into_iter()
        .filter(|a| a.mr_version.is_empty())
        .collect();
    if missing.is_empty() {
        return Ok(fixed);
    }
    let files = cf::get_files(&missing.iter().map(|a| a.file_id).collect::<Vec<_>>())?;
    for (i, a) in missing.iter().enumerate() {
        job.progress(i, missing.len());
        let Some(f) = files.iter().find(|f| f.id == a.file_id) else {
            continue;
        };
        job.detail(format!("Downloading {}", f.file_name));
        match cf::download_url(f) {
            Ok(url) => {
                cf::download(&url, &inst.addon_path(a), |_| {})?;
                fixed += 1;
            }
            Err(_) => job.note(format!(
                "{} — download by hand: {}/download/{}",
                a.name,
                a.page.trim_end_matches('/'),
                a.file_id
            )),
        }
    }
    Ok(fixed)
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

#[derive(Clone, Debug)]
pub struct World {
    pub name: String,
    pub dir: PathBuf,
    /// `icon.png`, when the world has one.
    pub icon: Option<PathBuf>,
    /// Seconds since the epoch, from `level.dat`'s mtime.
    pub last_played: i64,
}

/// Worlds in `saves/`, most recently played first.
pub fn worlds(inst: &Instance) -> Vec<World> {
    let mut out: Vec<World> = fs::read_dir(inst.root.join("saves"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("level.dat").exists())
        .map(|dir| World {
            name: dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            icon: Some(dir.join("icon.png")).filter(|p| p.exists()),
            last_played: fs::metadata(dir.join("level.dat"))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64),
            dir,
        })
        .collect();
    out.sort_by_key(|w| std::cmp::Reverse(w.last_played));
    out
}

/// Saves a modpack's server pack (the zip a dedicated server runs) to `dest`.
pub fn download_server_pack(
    project: u64,
    server_file: u64,
    dest: &Path,
    job: &Job,
) -> anyhow::Result<()> {
    let f = cf::file(project, server_file)?;
    let url = cf::download_url(&f)?;
    let total = f.file_length;
    job.detail(format!("Downloading {}", f.file_name));
    cf::download(&url, dest, |n| {
        if total > 0 {
            job.progress(n as usize, total as usize);
        }
    })?;
    Ok(())
}

/// Downloads a file to a temp path, or explains how to get it by hand.
fn fetch(m: &cf::Mod, f: &cf::File, job: &Job) -> anyhow::Result<PathBuf> {
    let url = cf::download_url(f).map_err(|_| {
        anyhow::anyhow!(
            "The author blocks downloads from other apps; get it from {}",
            f.manual_url(m)
        )
    })?;
    let dir = crate::paths::temp_dir().join("hopper-downloads");
    fs::create_dir_all(&dir)?;
    let dest = dir.join(&f.file_name);
    let total = f.file_length;
    job.detail(format!("Downloading {}", f.file_name));
    cf::download(&url, &dest, |n| {
        if total > 0 {
            job.progress(n as usize, total as usize);
        }
    })?;
    Ok(dest)
}

/// A folder name in `parent` based on `name` that isn't taken yet.
fn free_name(parent: &Path, name: &str) -> PathBuf {
    let base = Instance::folder_name(name);
    let mut p = parent.join(&base);
    let mut n = 2;
    while p.exists() {
        p = parent.join(format!("{base} ({n})"));
        n += 1;
    }
    p
}

/// Unpacks a world zip into a new folder in `saves/`. The world is the
/// folder in the zip that holds `level.dat` (often the zip's only folder).
pub fn install_world(
    inst: &Instance,
    m: &cf::Mod,
    f: &cf::File,
    job: &Job,
) -> anyhow::Result<PathBuf> {
    let zip_path = fetch(m, f, job)?;
    let r = extract_world(&zip_path, &inst.root.join("saves"), &m.name);
    let _ = fs::remove_file(&zip_path);
    r
}

/// A world zip from disk (dropped on Hopper) into a new folder in `saves/`.
pub fn import_world_zip(inst: &Instance, zip_path: &Path) -> anyhow::Result<PathBuf> {
    let name = zip_path
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "World".into());
    extract_world(zip_path, &inst.root.join("saves"), &name)
}

fn extract_world(zip_path: &Path, saves: &Path, name: &str) -> anyhow::Result<PathBuf> {
    let mut z = zip::ZipArchive::new(fs::File::open(zip_path)?)?;
    let level = (0..z.len())
        .filter_map(|i| z.by_index(i).ok().map(|e| e.name().to_string()))
        .filter(|n| n == "level.dat" || n.ends_with("/level.dat"))
        .min_by_key(|n| n.matches('/').count())
        .ok_or_else(|| anyhow::anyhow!("that download has no level.dat; it isn't a world"))?;
    let prefix = level.trim_end_matches("level.dat").to_string();
    let world_name = prefix
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(name)
        .to_string();
    fs::create_dir_all(saves)?;
    let dest = free_name(saves, &world_name);
    for i in 0..z.len() {
        let mut e = z.by_index(i)?;
        let Some(rel) = e.name().strip_prefix(&prefix).map(str::to_string) else {
            continue;
        };
        if rel.is_empty() || !crate::configdiff::safe_rel(rel.trim_end_matches('/')) {
            continue;
        }
        let out = dest.join(&rel);
        if e.is_dir() {
            fs::create_dir_all(&out)?;
        } else {
            if let Some(p) = out.parent() {
                fs::create_dir_all(p)?;
            }
            std::io::copy(&mut e, &mut fs::File::create(&out)?)?;
        }
    }
    Ok(dest)
}

/// Folders that load data packs into every world, by the mod that reads
/// them. Shown when the folder exists or the mod is installed.
pub const GLOBAL_DATAPACK_DIRS: &[(&str, &str, &str)] = &[
    ("config/paxi/datapacks", "Paxi", "paxi"),
    ("config/openloader/data", "OpenLoader", "openloader"),
    ("global_packs/required_data", "Global Packs", "globalpacks"),
];

/// Where a data pack can go: global loader folders first, then each
/// world's `datapacks/`. (label, folder relative to the instance)
pub fn datapack_targets(inst: &Instance) -> Vec<(String, String)> {
    let jars: Vec<String> = fs::read_dir(inst.mods_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_lowercase())
        .collect();
    let mut out: Vec<(String, String)> = GLOBAL_DATAPACK_DIRS
        .iter()
        .filter(|(dir, _, jar)| {
            inst.root.join(dir).is_dir() || jars.iter().any(|j| j.contains(jar))
        })
        .map(|(dir, name, _)| (format!("{name}: every world ({dir})"), dir.to_string()))
        .collect();
    for w in worlds(inst) {
        out.push((
            format!("World: {}", w.name),
            format!("saves/{}/datapacks", w.name),
        ));
    }
    out
}

/// Folders whose resource packs load for every world (and are forced on),
/// by the mod that reads them.
pub const GLOBAL_RESOURCEPACK_DIRS: &[(&str, &str, &str)] = &[
    ("config/paxi/resourcepacks", "Paxi", "paxi"),
    ("config/openloader/resources", "OpenLoader", "openloader"),
    (
        "global_packs/required_resources",
        "Global Packs",
        "globalpacks",
    ),
];

/// Global resource pack folders present in the instance (or whose mod is).
pub fn global_resourcepack_dirs(inst: &Instance) -> Vec<(&'static str, &'static str)> {
    let jars: Vec<String> = fs::read_dir(inst.mods_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_lowercase())
        .collect();
    GLOBAL_RESOURCEPACK_DIRS
        .iter()
        .filter(|(dir, _, jar)| {
            inst.root.join(dir).is_dir() || jars.iter().any(|j| j.contains(jar))
        })
        .map(|(dir, name, _)| (*dir, *name))
        .collect()
}

/// Where a resource pack can go: the instance's own folder (tracked, gets
/// updates), then global loader folders.
pub fn resourcepack_targets(inst: &Instance) -> Vec<(String, String)> {
    let mut out = vec![(
        "This instance (resourcepacks, gets updates)".to_string(),
        "resourcepacks".to_string(),
    )];
    for (dir, name) in global_resourcepack_dirs(inst) {
        out.push((format!("{name}: always on ({dir})"), dir.to_string()));
    }
    out
}

/// Puts a data pack into `dir` (a world's `datapacks/` or a global folder).
pub fn install_datapack(
    dir: &Path,
    m: &cf::Mod,
    f: &cf::File,
    job: &Job,
) -> anyhow::Result<PathBuf> {
    let tmp = fetch(m, f, job)?;
    fs::create_dir_all(dir)?;
    let dest = dir.join(&f.file_name);
    if fs::rename(&tmp, &dest).is_err() {
        fs::copy(&tmp, &dest)?;
        let _ = fs::remove_file(&tmp);
    }
    Ok(dest)
}

/// Data packs in a folder (zips, jars and folders).
pub fn datapacks(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            // Switched off (Paxi skips "x.zip.disabled") still counts.
            let n = p.file_name().unwrap_or_default().to_string_lossy().to_ascii_lowercase();
            let n = n.trim_end_matches(".disabled");
            p.is_dir() || n.ends_with(".zip") || n.ends_with(".jar")
        })
        .collect();
    out.sort();
    out
}

/// Resource pack files the game has switched on (options.txt's
/// `file/<name>` entries), lowercased.
pub fn active_resourcepacks(inst: &Instance) -> std::collections::HashSet<String> {
    let text = fs::read_to_string(inst.root.join("options.txt")).unwrap_or_default();
    let Some(list) = text.lines().find_map(|l| l.strip_prefix("resourcePacks:")) else {
        return Default::default();
    };
    serde_json::from_str::<Vec<String>>(list)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| e.strip_prefix("file/").map(str::to_lowercase))
        .collect()
}

/// The shader pack Iris or Oculus has selected, when shaders are on.
pub fn active_shader(inst: &Instance) -> Option<String> {
    ["iris.properties", "oculus.properties"]
        .iter()
        .find_map(|f| {
            let text = fs::read_to_string(inst.root.join("config").join(f)).ok()?;
            let get = |k: &str| {
                text.lines()
                    .find_map(|l| l.strip_prefix(&format!("{k}=")))
                    .map(|v| v.trim().replace("\\:", ":"))
            };
            if get("enableShaders").as_deref() == Some("false") {
                return None;
            }
            get("shaderPack").filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("(internal)"))
        })
}

/// Whether a shader loader (Iris, Oculus) is installed and enabled.
pub fn has_shader_loader(inst: &Instance) -> bool {
    fs::read_dir(inst.mods_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_lowercase())
        .any(|n| n.ends_with(".jar") && (n.starts_with("iris") || n.starts_with("oculus")))
}

/// Projects (CurseForge ids and Modrinth `num_id`s) whose files sit in the
/// global pack folders (Paxi, OpenLoader, Global Packs), mapped to the
/// loader's name. Found by fingerprint and sha1, like CurseForge's scan.
pub fn global_pack_projects(inst: &Instance) -> std::collections::HashMap<u64, &'static str> {
    use std::collections::HashMap;
    let mut by_fp: HashMap<u32, &'static str> = HashMap::new();
    let mut by_sha: HashMap<String, &'static str> = HashMap::new();
    for (dir, name, _) in GLOBAL_RESOURCEPACK_DIRS.iter().chain(GLOBAL_DATAPACK_DIRS) {
        for p in datapacks(&inst.root.join(dir)) {
            if p.is_dir() {
                continue;
            }
            let Ok(bytes) = fs::read(&p) else { continue };
            by_fp.insert(cf::fingerprint(&bytes), name);
            by_sha.insert(crate::modrinth::sha1(&bytes), name);
        }
    }
    let mut out = HashMap::new();
    if by_fp.is_empty() {
        return out;
    }
    if let Ok(matches) = cf::match_fingerprints(&by_fp.keys().copied().collect::<Vec<_>>()) {
        for m in matches {
            if let Some(n) = by_fp.get(&(m.file.file_fingerprint as u32)) {
                out.insert(m.id, *n);
            }
        }
    }
    if let Ok(versions) =
        crate::modrinth::versions_by_sha1(&by_sha.keys().cloned().collect::<Vec<_>>())
    {
        for (sha, v) in versions {
            if let Some(n) = by_sha.get(&sha) {
                out.insert(crate::modrinth::num_id(&v.project_id), *n);
            }
        }
    }
    out
}

/// A resource or data pack on disk, with what its pack.mcmeta says.
#[derive(Clone, Debug, Default)]
pub struct PackFile {
    pub path: PathBuf,
    /// File name with Minecraft's § formatting codes removed.
    pub name: String,
    pub description: String,
    /// pack.png, copied out of the zip into the cache.
    pub icon: Option<PathBuf>,
    pub size: u64,
    pub modified: i64,
}

/// Removes Minecraft's `§x` formatting codes.
pub fn strip_formatting(s: &str) -> String {
    let mut out = String::new();
    let mut skip = false;
    for c in s.chars() {
        if skip {
            skip = false;
        } else if c == '§' {
            skip = true;
        } else {
            out.push(c);
        }
    }
    out
}

fn mcmeta_description(v: &serde_json::Value) -> String {
    fn text(v: &serde_json::Value) -> String {
        match v {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(a) => a.iter().map(text).collect(),
            serde_json::Value::Object(o) => {
                o.get("text").map(text).unwrap_or_default()
                    + &o.get("extra").map(text).unwrap_or_default()
            }
            _ => String::new(),
        }
    }
    strip_formatting(&text(&v["pack"]["description"]))
}

/// `path` as it is when switched on ("x.zip" for "x.zip.disabled"): what
/// a pack is known by, so turning it off or on keeps what it was found to be.
pub fn enabled_path(path: &Path) -> PathBuf {
    match path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".disabled")) {
        Some(on) => path.with_file_name(on),
        None => path.to_path_buf(),
    }
}

/// Packs in a folder with their names, descriptions and icons.
pub fn pack_files(dir: &Path) -> Vec<PackFile> {
    use std::io::Read;
    let cache = crate::paths::temp_dir().join("hopper-packicons");
    let _ = fs::create_dir_all(&cache);
    let mut out: Vec<PackFile> = datapacks(dir)
        .into_iter()
        .map(|path| {
            let md = fs::metadata(&path).ok();
            let modified = md
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64);
            let size = md.as_ref().map_or(0, |m| m.len());
            let (mut description, mut icon) = (String::new(), None);
            if path.is_dir() {
                if let Ok(t) = fs::read_to_string(path.join("pack.mcmeta")) {
                    description = mcmeta_description(&serde_json::from_str(&t).unwrap_or_default());
                }
                icon = Some(path.join("pack.png")).filter(|p| p.exists());
            } else if let Ok(mut z) = fs::File::open(&path)
                .map_err(anyhow::Error::from)
                .and_then(|f| Ok(zip::ZipArchive::new(f)?))
            {
                if let Ok(mut e) = z.by_name("pack.mcmeta") {
                    let mut t = String::new();
                    let _ = e.read_to_string(&mut t);
                    description = mcmeta_description(&serde_json::from_str(&t).unwrap_or_default());
                }
                let key = sha1_smol::Sha1::from(format!("{}|{size}|{modified}", enabled_path(&path).display()))
                    .hexdigest();
                let png = cache.join(format!("{key}.png"));
                if png.exists() {
                    icon = Some(png);
                } else if let Ok(mut e) = z.by_name("pack.png") {
                    let mut b = Vec::new();
                    if e.read_to_end(&mut b).is_ok() && fs::write(&png, b).is_ok() {
                        icon = Some(png);
                    }
                }
            }
            let file = path.file_name().unwrap_or_default().to_string_lossy();
            let name = strip_formatting(file.trim_end_matches(".disabled").trim_end_matches(".zip"));
            PackFile {
                path,
                name,
                description,
                icon,
                size,
                modified,
            }
        })
        .collect();
    out.sort_by_key(|p| p.name.to_lowercase());
    out
}

/// Zips a world into `<instance>/.hopper-backups/worlds/`.
pub fn backup_world(inst: &Instance, world: &World, job: &Job) -> anyhow::Result<PathBuf> {
    use std::io::Write;
    let dir = inst.root.join(crate::sync::BACKUP_DIR).join("worlds");
    fs::create_dir_all(&dir)?;
    let stamp = crate::time::now_iso().replace([':', '.'], "-");
    let out = dir.join(format!(
        "{}-{stamp}.zip",
        Instance::folder_name(&world.name)
    ));
    let files = walk(&world.dir);
    let mut z = zip::ZipWriter::new(fs::File::create(&out)?);
    let opts = zip::write::SimpleFileOptions::default();
    for (i, f) in files.iter().enumerate() {
        // The lock file is held open while the world is loaded.
        if f.file_name().is_some_and(|n| n == "session.lock") {
            continue;
        }
        let rel = f
            .strip_prefix(&world.dir)?
            .to_string_lossy()
            .replace('\\', "/");
        z.start_file(format!("{}/{rel}", world.name), opts)?;
        z.write_all(&fs::read(f)?)?;
        job.progress(i, files.len());
    }
    z.finish()?;
    Ok(out)
}

/// `screenshots/*.png`, newest first.
pub fn screenshots(inst: &Instance) -> Vec<PathBuf> {
    let mut out: Vec<(std::time::SystemTime, PathBuf)> =
        fs::read_dir(inst.root.join("screenshots"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("png") || x.eq_ignore_ascii_case("jpg"))
            })
            .map(|p| {
                (
                    fs::metadata(&p)
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::UNIX_EPOCH),
                    p,
                )
            })
            .collect();
    out.sort_by_key(|a| std::cmp::Reverse(a.0));
    out.into_iter().map(|(_, p)| p).collect()
}

/// A newer file of each pack, keyed by pack project id.
pub fn pack_updates(packs: &[(PackRef, String, u32)]) -> anyhow::Result<Vec<(u64, cf::File)>> {
    let ids: Vec<u64> = packs
        .iter()
        .map(|(p, _, _)| p.project_id)
        // Modrinth packs are not on CurseForge.
        .filter(|&id| id != 0 && !crate::modrinth::is_mr(id))
        .collect();
    let mods = cf::get_mods(&ids)?;
    let mut newest = Vec::new();
    for (p, game_version, channel) in packs {
        let Some(m) = mods.iter().find(|m| m.id == p.project_id) else {
            continue;
        };
        let best = m
            .latest_files_indexes
            .iter()
            .filter(|ix| game_version.is_empty() || &ix.game_version == game_version)
            .filter(|ix| ix.release_type <= (*channel).max(1))
            .map(|ix| ix.file_id)
            .max();
        if let Some(b) = best
            && b > p.file_id
            && p.file_id != 0
        {
            newest.push((p.project_id, b));
        }
    }
    let files = cf::get_files(&newest.iter().map(|(_, f)| *f).collect::<Vec<_>>())?;
    let mut out: Vec<(u64, cf::File)> = files
        .into_iter()
        .filter(|f| f.is_server_pack != Some(true))
        .map(|f| (f.mod_id, f))
        .collect();
    // Modrinth packs: a version listed before the installed one is newer.
    for (p, game_version, channel) in packs
        .iter()
        .filter(|(p, _, _)| crate::modrinth::is_mr(p.project_id))
    {
        let Ok(list) = crate::source::files(p.project_id, game_version, cf::Loader::Any) else {
            continue;
        };
        let Some(installed_at) = list.iter().position(|f| f.id == p.file_id) else {
            continue;
        };
        if let Some(newer) = list[..installed_at]
            .iter()
            .find(|f| f.release_type <= (*channel).max(1))
        {
            out.push((p.project_id, newer.clone()));
        }
    }
    Ok(out)
}

/// Bytes taken by everything under `dir` (links are not followed).
pub fn folder_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                stack.push(e.path());
            } else if t.is_file() {
                total += e.metadata().map_or(0, |m| m.len());
            }
        }
    }
    total
}

/// "18.4 GB", "512 MB", "12 KB".
pub fn human_bytes(b: u64) -> String {
    const K: f64 = 1024.0;
    let b = b as f64;
    if b >= K * K * K {
        format!("{:.1} GB", b / (K * K * K))
    } else if b >= K * K {
        format!("{:.0} MB", b / (K * K))
    } else {
        format!("{:.0} KB", b / K)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_add_up_and_read_well() {
        let root = std::env::temp_dir().join(format!("hopper-size-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("x"), [0u8; 100]).unwrap();
        fs::write(root.join("a/b/y"), [0u8; 50]).unwrap();
        assert_eq!(folder_size(&root), 150);
        fs::remove_dir_all(&root).ok();
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
        assert_eq!(human_bytes(512 * 1024 * 1024), "512 MB");
        assert_eq!(human_bytes(2048), "2 KB");
    }

    #[test]
    fn duplicate_skips_backups_and_renames() {
        let root = std::env::temp_dir().join(format!("hopper-dup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let inst = crate::modpack::create(&root, "Orig", "1.21.1", "").unwrap();
        fs::write(inst.root.join("mods/a.jar"), "a").unwrap();
        fs::create_dir_all(inst.root.join(".hopper-backups")).unwrap();
        fs::write(inst.root.join(".hopper-backups/old.jar"), "x").unwrap();
        let jobs = crate::jobs::Jobs::default();
        let dest = duplicate(&inst, &jobs.start("dup")).unwrap();
        assert!(dest.join("mods/a.jar").exists());
        assert!(!dest.join(".hopper-backups").exists());
        assert_eq!(crate::instances::load(&dest).unwrap().name, "Orig (copy)");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn remote_drives_and_moving_aside() {
        let root = std::env::temp_dir().join(format!("hopper-trash-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("Pack");
        fs::create_dir_all(dir.join("mods")).unwrap();
        fs::write(dir.join("mods/a.jar"), "a").unwrap();
        // The temp folder is local.
        assert!(!is_remote_drive(&dir));
        #[cfg(windows)]
        assert!(is_remote_drive(Path::new(r"\\server\share\Instances\Pack")));

        let moved = move_to_hopper_trash(&dir).unwrap();
        assert!(!dir.exists());
        assert_eq!(moved.parent().unwrap(), root.join(HOPPER_TRASH));
        assert!(
            moved
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("Pack-")
        );
        assert_eq!(fs::read_to_string(moved.join("mods/a.jar")).unwrap(), "a");
        // A second delete of the same name in the same second doesn't collide.
        fs::create_dir_all(&dir).unwrap();
        let again = move_to_hopper_trash(&dir).unwrap();
        assert_ne!(again, moved);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn worlds_and_backup() {
        let root = std::env::temp_dir().join(format!("hopper-world-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let inst = crate::modpack::create(&root, "W", "1.21.1", "").unwrap();
        let w = inst.root.join("saves/My World");
        fs::create_dir_all(w.join("region")).unwrap();
        fs::write(w.join("level.dat"), "lvl").unwrap();
        fs::write(w.join("region/r.0.0.mca"), "r").unwrap();
        fs::write(w.join("session.lock"), "").unwrap();
        let list = worlds(&inst);
        assert_eq!(list.len(), 1);
        let jobs = crate::jobs::Jobs::default();
        let zip = backup_world(&inst, &list[0], &jobs.start("b")).unwrap();
        let z = zip::ZipArchive::new(fs::File::open(zip).unwrap()).unwrap();
        let names: Vec<_> = z.file_names().collect();
        assert!(names.contains(&"My World/region/r.0.0.mca"));
        assert!(!names.iter().any(|n| n.ends_with("session.lock")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn extracts_a_world_from_its_folder_in_the_zip() {
        use std::io::Write;
        let root = std::env::temp_dir().join(format!("hopper-worldzip-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let zp = root.join("w.zip");
        let mut z = zip::ZipWriter::new(fs::File::create(&zp).unwrap());
        for (n, b) in [
            ("readme.txt", "hi"),
            ("Skyblock/level.dat", "x"),
            ("Skyblock/region/r.0.0.mca", "r"),
        ] {
            z.start_file::<_, ()>(n, Default::default()).unwrap();
            z.write_all(b.as_bytes()).unwrap();
        }
        z.finish().unwrap();
        let saves = root.join("saves");
        let a = extract_world(&zp, &saves, "Sky").unwrap();
        assert_eq!(a, saves.join("Skyblock"));
        assert!(a.join("region/r.0.0.mca").exists());
        assert!(!a.join("readme.txt").exists());
        // A second copy gets its own folder.
        assert_eq!(
            extract_world(&zp, &saves, "Sky").unwrap(),
            saves.join("Skyblock (2)")
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore = "network: downloads a world and a data pack from CurseForge"]
    fn real_world_and_datapack() {
        crate::env::load(std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../.env"
        )));
        let root = std::env::temp_dir().join(format!("hopper-realworld-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let inst = Instance {
            root: root.clone(),
            game_version: "1.21.1".into(),
            ..Default::default()
        };
        let jobs = crate::jobs::Jobs::new(crate::waker::noop());
        let pick = |class: u32, text: &str| {
            let (hits, _) = crate::source::search(
                &cf::Search {
                    class_id: class,
                    text: text.into(),
                    game_version: "1.21.1".into(),
                    loader: cf::Loader::Any,
                    category: None,
                    sort: cf::SortField::Popularity,
                    index: 0,
                },
                false,
            )
            .unwrap();
            hits.into_iter()
                .find_map(|m| {
                    let f = crate::install::best_file(m.id, Some(class), &inst).ok()??;
                    (f.file_length < 30 << 20 && cf::download_url(&f).is_ok()).then_some((m, f))
                })
                .expect("a small downloadable project")
        };
        let (m, f) = pick(cf::CLASS_WORLDS, "parkour");
        let w = install_world(&inst, &m, &f, &jobs.start("w")).unwrap();
        println!("world {} -> {}", m.name, w.display());
        assert!(w.join("level.dat").exists());
        let (dm, df) = pick(cf::CLASS_DATAPACKS, "");
        let d = install_datapack(&w.join("datapacks"), &dm, &df, &jobs.start("d")).unwrap();
        println!("datapack {} -> {}", dm.name, d.display());
        assert_eq!(datapacks(&w.join("datapacks")), [d]);
        let _ = fs::remove_dir_all(&root);
    }
}

/// What to search for a pack by: its file name without versions
/// ("ExtraDataModels-1.21-4.3" → "ExtraDataModels").
pub fn pack_search_name(file: &str) -> String {
    // A pack switched off is the same pack.
    let file = file.strip_suffix(".disabled").unwrap_or(file);
    let stem = file
        .strip_suffix(".zip")
        .or_else(|| file.strip_suffix(".jar"))
        .unwrap_or(file);
    let stem = strip_formatting(stem);
    stem.split(['-', '_', ' ', '+', '(', ')', '[', ']'])
        .filter(|w| !w.is_empty())
        .take_while(|w| {
            let w = w.trim_start_matches(['v', 'V']);
            !w.starts_with(|c: char| c.is_ascii_digit())
                && !w.eq_ignore_ascii_case("mc")
        })
        .map(split_camel)
        .collect::<Vec<_>>()
        .join(" ")
}

/// "ExtraDataModels" → "Extra Data Models": sites search by words.
fn split_camel(w: &str) -> String {
    let mut out = String::new();
    let mut prev: Option<char> = None;
    for c in w.chars() {
        if c.is_uppercase() && prev.is_some_and(|p| p.is_lowercase()) {
            out.push(' ');
        }
        out.push(c);
        prev = Some(c);
    }
    out
}

/// Lowercase letters and digits only, for comparing names.
fn squash(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// A pack file found on CurseForge or Modrinth.
#[derive(Clone, Debug)]
pub struct PackMatch {
    pub project: cf::Mod,
    /// The file it is: known when found by its hash; else looked for by
    /// name among the project's files (see [`loose_pack_versions`]).
    pub file: Option<cf::File>,
}

/// Packs found on CurseForge or Modrinth: zips by file hash (CurseForge's
/// fingerprint, Modrinth's sha1), then the rest (folders, repacked zips)
/// by a search for their name that must match a project's name or slug,
/// among projects of `class` (or mods: some data packs are published as
/// mods, for Modrinth's datapack loader). The flag is false when a site
/// didn't answer, so packs not found may be there after all.
pub fn identify_packs(paths: &[PathBuf], class: u32) -> (std::collections::HashMap<PathBuf, PackMatch>, bool) {
    let mut answered = true;
    use std::collections::HashMap;
    let mut by_fp: HashMap<u32, Vec<PathBuf>> = HashMap::new();
    let mut by_sha: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for p in paths.iter().filter(|p| p.is_file()) {
        let Ok(bytes) = fs::read(p) else { continue };
        by_fp.entry(cf::fingerprint(&bytes)).or_default().push(p.clone());
        by_sha.entry(crate::modrinth::sha1(&bytes)).or_default().push(p.clone());
    }
    // Path → (CurseForge id, file) or (Modrinth id, file).
    let mut cf_ids: HashMap<PathBuf, (u64, cf::File)> = HashMap::new();
    let mut mr_ids: HashMap<PathBuf, (String, cf::File)> = HashMap::new();
    if !by_fp.is_empty() {
        match cf::match_fingerprints(&by_fp.keys().copied().collect::<Vec<_>>()) {
            Ok(matches) => {
                for m in matches {
                    for p in by_fp.get(&(m.file.file_fingerprint as u32)).into_iter().flatten() {
                        cf_ids.insert(p.clone(), (m.id, m.file.clone()));
                    }
                }
            }
            Err(_) => answered = false,
        }
    }
    let by_sha_result = (!by_sha.is_empty())
        .then(|| crate::modrinth::versions_by_sha1(&by_sha.keys().cloned().collect::<Vec<_>>()));
    if matches!(by_sha_result, Some(Err(_))) {
        answered = false;
    }
    if let Some(Ok(versions)) = by_sha_result {
        for (sha, v) in versions {
            for p in by_sha.get(&sha).into_iter().flatten() {
                if !cf_ids.contains_key(p) {
                    mr_ids.insert(p.clone(), (v.project_id.clone(), crate::modrinth::version_to_file(&v)));
                }
            }
        }
    }
    let mut out: HashMap<PathBuf, PackMatch> = HashMap::new();
    let mut ids: Vec<u64> = cf_ids.values().map(|(id, _)| *id).collect();
    ids.sort_unstable();
    ids.dedup();
    if !ids.is_empty()
        && let Ok(mods) = cf::get_mods(&ids)
    {
        for (p, (id, f)) in &cf_ids {
            if let Some(m) = mods.iter().find(|m| m.id == *id) {
                out.insert(p.clone(), PackMatch { project: m.clone(), file: Some(f.clone()) });
            }
        }
    }
    let mut ids: Vec<String> = mr_ids.values().map(|(id, _)| id.clone()).collect();
    ids.sort_unstable();
    ids.dedup();
    if !ids.is_empty()
        && let Ok(projects) = crate::modrinth::projects(&ids)
    {
        for (p, (id, f)) in &mr_ids {
            if let Some(pr) = projects.iter().find(|pr| pr.id == *id) {
                let project = crate::modrinth::project_to_mod(pr, "");
                out.insert(p.clone(), PackMatch { project, file: Some(f.clone()) });
            }
        }
    }
    let rest: Vec<&PathBuf> = paths.iter().filter(|p| !out.contains_key(*p)).collect();
    for p in rest {
        let file = p.file_name().unwrap_or_default().to_string_lossy();
        let name = pack_search_name(&file);
        let want = squash(&name);
        if want.len() < 4 {
            continue;
        }
        let same = |m: &cf::Mod| squash(&m.name) == want || squash(&m.slug) == want;
        let hit = [class, cf::CLASS_MODS].into_iter().find_map(|class_id| {
            let s = cf::Search {
                class_id,
                text: name.clone(),
                ..Default::default()
            };
            cf::search(&s)
                .ok()
                .and_then(|(v, _)| v.into_iter().find(same))
                .or_else(|| {
                    crate::modrinth::search(&s)
                        .ok()
                        .and_then(|(v, _)| v.into_iter().find(same))
                })
        });
        if let Some(m) = hit {
            out.insert(p.clone(), PackMatch { project: m, file: None });
        }
    }
    (out, answered)
}

/// A found pack's installed file and the newer one for the instance's
/// Minecraft version, when there is one. A pack found by name is matched
/// to a file of its project by file name; one that matches none has no
/// known version, so nothing is offered for it.
#[derive(Clone, Debug, Default)]
pub struct PackVersion {
    pub installed: Option<cf::File>,
    pub update: Option<cf::File>,
}

/// [`PackVersion`]s of found packs (path → match), read eight at a time.
pub fn loose_pack_versions(
    inst: &Instance,
    found: &[(PathBuf, PackMatch)],
) -> std::collections::HashMap<PathBuf, PackVersion> {
    let chunk = found.len().div_ceil(8).max(1);
    std::thread::scope(|s| {
        let parts: Vec<_> = found
            .chunks(chunk)
            .map(|part| {
                s.spawn(move || {
                    part.iter()
                        .map(|(path, m)| (path.clone(), pack_version(inst, path, m)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        parts.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    })
}

fn pack_version(inst: &Instance, path: &Path, m: &PackMatch) -> PackVersion {
    let files = crate::source::files(m.project.id, &inst.game_version, crate::cf::Loader::Any).unwrap_or_default();
    let path = enabled_path(path);
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let installed = m.file.clone().or_else(|| files.iter().find(|f| f.file_name == name).cloned());
    let best = files.iter().find(|f| f.release_type == 1).or(files.first());
    let update = match (&installed, best) {
        (Some(i), Some(b)) if b.id != i.id && b.file_date > i.file_date => Some(b.clone()),
        _ => None,
    };
    PackVersion { installed, update }
}

/// Replaces a pack with `file` of its project in the same folder; the old
/// one goes to the instance's backups.
pub fn update_pack(root: &Path, old: &Path, m: &cf::Mod, file: &cf::File, job: &Job) -> anyhow::Result<PathBuf> {
    let dir = old.parent().context("the pack has no folder")?;
    let new = install_datapack(dir, m, file, job)?;
    if new != old {
        crate::sync::backup_into(root, old)?;
    }
    Ok(new)
}

/// The version to show for a pack file: "3.1.0" of "Abibliophobia-3.1.0.zip".
pub fn pack_version_label(file_name: &str) -> String {
    let stem = file_name
        .strip_suffix(".zip")
        .or_else(|| file_name.strip_suffix(".jar"))
        .unwrap_or(file_name);
    crate::scan::short_version(&format!("{stem}.jar"))
}

#[cfg(test)]
mod pack_name_tests {
    use super::pack_search_name;

    #[test]
    fn drops_versions() {
        assert_eq!(pack_search_name("ExtraDataModels-1.21-4.3"), "Extra Data Models");
        assert_eq!(pack_search_name("AntiBlindnessDarkness-1.21_v2.zip"), "Anti Blindness Darkness");
        assert_eq!(pack_search_name("BotanyMinecolonies.zip"), "Botany Minecolonies");
        assert_eq!(pack_search_name("tnp_data"), "tnp data");
        assert_eq!(pack_search_name("Better Trades [1.21.1-1.3.0].zip"), "Better Trades");
    }

    /// A pack switched off (renamed to .disabled) is looked up and keyed
    /// as the same pack, so it keeps its icon, author and link.
    #[test]
    fn disabled_is_the_same_pack() {
        use super::enabled_path;
        use std::path::Path;
        assert_eq!(pack_search_name("Pretty X Smart Pipez.zip.disabled"), "Pretty X Smart Pipez");
        assert_eq!(pack_search_name("Pretty X Smart Pipez.zip.disabled"), pack_search_name("Pretty X Smart Pipez.zip"));
        let dir = Path::new("config/paxi/resourcepacks");
        assert_eq!(enabled_path(&dir.join("a b.zip.disabled")), dir.join("a b.zip"));
        assert_eq!(enabled_path(&dir.join("a b.zip")), dir.join("a b.zip"));
        assert_eq!(enabled_path(&dir.join("tnp_resources")), dir.join("tnp_resources"));
    }
}


