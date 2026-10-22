//! Installing and updating CurseForge modpacks, and exporting an instance as one.

use crate::cf;
use crate::install::addon_from;
use crate::instances::{Instance, LaunchSettings, PackManifest, PackRef, folder_for_class};
use crate::jobs::Job;
use anyhow::Context;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const PARALLEL_DOWNLOADS: usize = 8;

/// A pack install or update downloads here first; files move into the
/// instance only once every download worked.
const STAGING_DIR: &str = ".hopper-staging";

/// Files the last pack install or update could not download, as JSON
/// (see [`pending_downloads`]).
pub const PENDING_FILE: &str = ".hopper-pending.json";

fn cache_dir() -> PathBuf {
    crate::paths::dirs()
        .map(|d| d.cache_dir().join("packs"))
        .unwrap_or_else(|| std::env::temp_dir().join("hopper-packs"))
}

fn downloads_dir() -> Option<PathBuf> {
    directories::UserDirs::new().and_then(|u| u.download_dir().map(Path::to_path_buf))
}

/// Picks a free folder name under `root`.
fn unique_dir(root: &Path, name: &str) -> PathBuf {
    let base = Instance::folder_name(name);
    let mut dir = root.join(&base);
    let mut n = 2;
    while dir.exists() {
        dir = root.join(format!("{base} ({n})"));
        n += 1;
    }
    dir
}

/// Creates `dir` (which must not exist yet) and fills it with `f`. When
/// `f` fails the folder is removed again, so a failed install leaves no
/// half-made instance behind. A folder this call did not create is never
/// touched.
fn in_new_folder<T>(dir: &Path, f: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
    if let Some(p) = dir.parent() {
        fs::create_dir_all(p)?;
    }
    fs::create_dir(dir).with_context(|| format!("creating {}", dir.display()))?;
    let r = f();
    if r.is_err()
        && let Err(e) = fs::remove_dir_all(dir)
    {
        log::warn!("could not remove {}: {e}", dir.display());
    }
    r
}

/// What a pack update does with its override files (paths relative to the
/// instance, with `/`). `None` in the functions below writes them all.
#[derive(Clone, Debug, Default)]
pub struct Overrides {
    /// Files left as they are (you changed them, the pack didn't, or you
    /// chose to keep yours).
    pub skip: HashSet<String>,
    /// Files the pack no longer ships and you never changed.
    pub remove: Vec<String>,
    /// Files replaced although you changed them; backed up first.
    pub backup: Vec<String>,
    /// Contents to write instead of the pack's (merged files).
    pub write: Vec<(String, Vec<u8>)>,
}

// ── files a pack wants that could not be downloaded ─────────────────

/// A pack file Hopper could not download, usually because the author only
/// allows downloads from the CurseForge website. Kept in the instance's
/// [`PENDING_FILE`] until the next pack install or update of it.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Pending {
    /// The project's name, or the file name when the project is unknown.
    pub name: String,
    pub file_name: String,
    /// Folder under the instance it belongs in, e.g. "mods".
    pub folder: String,
    /// 0 when unknown (Modrinth pack files).
    pub project_id: u64,
    pub file_id: u64,
    /// Where to download it by hand.
    pub url: String,
    /// Why Hopper could not.
    pub reason: String,
    /// Size and SHA-1 (lower-case hex) when known, to recognise the file in
    /// Downloads even when the browser renamed it.
    pub size: u64,
    pub sha1: String,
    /// The instance's entry for it once it's in place.
    pub addon: Option<crate::instances::Addon>,
}

/// Files the last pack install or update of `inst` could not download.
/// Empty when everything arrived.
pub fn pending_downloads(inst: &Instance) -> Vec<Pending> {
    fs::read(inst.root.join(PENDING_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Forgets the pending downloads (the user dealt with them).
pub fn clear_pending(inst: &Instance) -> anyhow::Result<()> {
    clear_pending_at(&inst.root)
}

fn clear_pending_at(root: &Path) -> anyhow::Result<()> {
    match fs::remove_file(root.join(PENDING_FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Adds files to download by hand to an instance's list (a file already
/// listed is replaced).
pub fn add_pending(root: &Path, more: &[Pending]) -> anyhow::Result<()> {
    let mut list: Vec<Pending> = fs::read(root.join(PENDING_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    list.retain(|p| !more.iter().any(|m| m.file_name == p.file_name));
    list.extend(more.iter().cloned());
    write_pending(root, &list)
}

/// The copy of `p` in `dir` (the user's Downloads): the same name, or the
/// browser's renamed copy ("name (1).jar"), and when the size and SHA-1 are
/// known, only a file that matches them. Half-written downloads
/// (.crdownload, .part, .tmp) never match.
pub fn find_download(p: &Pending, dir: &Path) -> Option<PathBuf> {
    let (stem, ext) = match p.file_name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (p.file_name.clone(), String::new()),
    };
    let renamed = |n: &str| {
        // "name (1).jar", "name(2).jar", "name - Copy.jar"
        n.strip_prefix(&stem)
            .and_then(|r| r.strip_suffix(&ext))
            .is_some_and(|mid| {
                let m = mid.trim();
                m == "- Copy"
                    || (m.starts_with('(') && m.ends_with(')') && m[1..m.len() - 1].chars().all(|c| c.is_ascii_digit()))
            })
    };
    let mut candidates: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            (n == p.file_name || renamed(&n)).then(|| e.path())
        })
        .collect();
    // The exact name first.
    candidates.sort_by_key(|c| c.file_name().is_none_or(|n| n.to_string_lossy() != p.file_name));
    candidates.into_iter().find(|c| {
        let Ok(m) = fs::metadata(c) else { return false };
        if p.size > 0 && m.len() != p.size {
            return false;
        }
        if p.sha1.is_empty() {
            return p.size == 0 || m.len() == p.size;
        }
        fs::read(c).is_ok_and(|b| crate::modrinth::sha1(&b).eq_ignore_ascii_case(&p.sha1))
    })
}

/// Pending files of `inst` that are now in `dir` (Downloads).
pub fn ready_downloads(inst_root: &Path, dir: &Path) -> Vec<(Pending, PathBuf)> {
    let list: Vec<Pending> = fs::read(inst_root.join(PENDING_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    list.into_iter()
        .filter_map(|p| {
            let at = find_download(&p, dir)?;
            Some((p, at))
        })
        .collect()
}

/// Moves pending files the user downloaded into the instance: each goes in
/// its folder (replacing that project's old file), gets its entry, leaves
/// the pending list and is deleted from Downloads. Returns the names placed.
pub fn place_downloads(inst: &mut Instance, dir: &Path) -> anyhow::Result<Vec<String>> {
    let ready = ready_downloads(&inst.root, dir);
    let mut placed = Vec::new();
    for (p, at) in &ready {
        let folder = if p.folder.is_empty() { "mods" } else { p.folder.as_str() };
        let dest = inst.root.join(folder).join(&p.file_name);
        fs::create_dir_all(dest.parent().unwrap_or(&inst.root))?;
        // Copy first: Downloads is often another drive.
        let tmp = dest.with_extension("hopper-tmp");
        fs::copy(at, &tmp).with_context(|| format!("copying {}", at.display()))?;
        match &p.addon {
            Some(a) => {
                let old = inst
                    .addons
                    .iter()
                    .position(|o| o.project_id == a.project_id && a.project_id != 0)
                    .map(|i| inst.addons.remove(i));
                let mut new = a.clone();
                new.file_name = p.file_name.clone();
                crate::install::replace_addon(inst, old, new, &tmp)?;
            }
            None => {
                fs::rename(&tmp, &dest)?;
            }
        }
        let _ = fs::remove_file(&tmp);
        // Ours now: the copy in Downloads goes.
        let _ = fs::remove_file(at);
        placed.push(p.name.clone());
    }
    if !ready.is_empty() {
        let left: Vec<Pending> = pending_downloads(inst)
            .into_iter()
            .filter(|p| !ready.iter().any(|(r, _)| r.file_name == p.file_name))
            .collect();
        write_pending(&inst.root, &left)?;
        inst.save()?;
    }
    Ok(placed)
}

/// The user's Downloads folder.
pub fn user_downloads() -> Option<PathBuf> {
    downloads_dir()
}

/// Where Hopper's own download window saves website-only files, for the
/// download watcher to put in place like files from Downloads.
pub fn fetch_dir() -> Option<PathBuf> {
    crate::paths::dirs().map(|d| d.data_dir().join("fetched"))
}

/// The folders pending files can land in: Hopper's download window's, then
/// the user's Downloads.
pub fn landing_dirs() -> Vec<PathBuf> {
    fetch_dir().into_iter().chain(downloads_dir()).collect()
}

fn write_pending(root: &Path, pending: &[Pending]) -> anyhow::Result<()> {
    if pending.is_empty() {
        return clear_pending_at(root);
    }
    let tmp = root.join(format!("{PENDING_FILE}.tmp"));
    fs::write(&tmp, serde_json::to_vec_pretty(pending)?)?;
    fs::rename(&tmp, root.join(PENDING_FILE))?;
    Ok(())
}

fn note_pending(job: &Job, pending: &[Pending], from_downloads: bool) {
    for p in pending {
        job.note(format!(
            "{} — download by hand: {}",
            p.file_name,
            if p.url.is_empty() { &p.reason } else { &p.url }
        ));
    }
    if from_downloads && !pending.is_empty() {
        job.note(format!(
            "Put the files above in {} and run Update pack again; Hopper picks them up from there.",
            downloads_dir().map_or("your Downloads folder".into(), |d| d.display().to_string())
        ));
    }
}

// ── the pack download cache ──────────────────────────────────────────

/// `true` when `path` is in the cache; marks it used now, for pruning.
fn cache_hit(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    if let Ok(f) = fs::File::options().write(true).open(path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
    true
}

/// Frees the pack download cache: files unused for longer than `max_age`
/// go, then the least recently used until the rest fits in `keep_bytes`.
/// Files in use are skipped. Returns the bytes freed.
pub fn prune_pack_cache(keep_bytes: u64, max_age: Option<Duration>) -> anyhow::Result<u64> {
    prune_dir(&cache_dir(), keep_bytes, max_age, std::time::SystemTime::now())
}

/// Bytes the pack download cache takes.
pub fn pack_cache_size() -> u64 {
    cache_files(&cache_dir()).iter().map(|f| f.1).sum()
}

/// (path, bytes, last used) of the cached packs, oldest first.
fn cache_files(dir: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut files: Vec<_> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().ends_with(".hopper-part"))
        .filter_map(|e| {
            let md = e.metadata().ok().filter(|m| m.is_file())?;
            Some((e.path(), md.len(), md.modified().ok()?))
        })
        .collect();
    files.sort_by_key(|f| f.2);
    files
}

fn prune_dir(
    dir: &Path,
    keep_bytes: u64,
    max_age: Option<Duration>,
    now: std::time::SystemTime,
) -> anyhow::Result<u64> {
    let files = cache_files(dir);
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    let mut freed = 0;
    for (path, len, used) in files {
        let stale = max_age.is_some_and(|a| now.duration_since(used).unwrap_or_default() > a);
        if !stale && total <= keep_bytes {
            continue;
        }
        if fs::remove_file(&path).is_ok() {
            total -= len;
            freed += len;
        }
    }
    Ok(freed)
}

pub(crate) fn fetch_pack_zip(pack_file: &cf::File, job: &Job) -> anyhow::Result<PathBuf> {
    // CurseForge file ids are unique, so they make the cache key.
    let zip_path = cache_dir().join(format!("{}.zip", pack_file.id));
    if !cache_hit(&zip_path) {
        let url = cf::download_url(pack_file)?;
        job.detail(format!("Downloading {}", pack_file.file_name));
        let total = pack_file.file_length as usize;
        // Size and speed in the detail, refreshed about once a second.
        let mut last = (std::time::Instant::now(), 0u64);
        let mut speed = 0.0f64;
        cf::download_checked(&url, &zip_path, &pack_file.expect(), Some(job), |n| {
            job.progress(n as usize, total);
            let dt = last.0.elapsed().as_secs_f64();
            if dt >= 1.0 || n < last.1 {
                let now = n.saturating_sub(last.1) as f64 / dt.max(0.001);
                speed = if speed == 0.0 { now } else { speed * 0.5 + now * 0.5 };
                last = (std::time::Instant::now(), n);
                job.detail(download_detail(&pack_file.file_name, n, total as u64, speed));
            }
        })?;
    }
    Ok(zip_path)
}

/// "Downloading x.zip: 512 MB of 1.4 GB · 24 MB/s · about 40 s left".
fn download_detail(name: &str, done: u64, total: u64, speed: f64) -> String {
    use crate::instance_ops::human_bytes;
    let mut s = format!("Downloading {name}: {}", human_bytes(done));
    if total > 0 {
        s.push_str(&format!(" of {}", human_bytes(total)));
    }
    if speed >= 1.0 {
        s.push_str(&format!(" · {}/s", human_bytes(speed as u64)));
        if total > done {
            let left = ((total - done) as f64 / speed).round() as u64;
            s.push_str(&match left {
                0..60 => format!(" · about {left} s left"),
                _ => format!(" · about {} min left", left.div_ceil(60)),
            });
        }
    }
    s
}

pub(crate) fn read_manifest(zip_path: &Path) -> anyhow::Result<PackManifest> {
    let mut z = zip::ZipArchive::new(fs::File::open(zip_path)?)?;
    let mut s = String::new();
    z.by_name("manifest.json")
        .context("pack has no manifest.json")?
        .read_to_string(&mut s)?;
    Ok(serde_json::from_str(&s)?)
}

/// The files under a downloaded pack's overrides folder, in zip order with
/// backslashes, as CurseForge lists them in `modpackOverrides`. `None` when
/// the pack file isn't in Hopper's cache.
pub fn cached_override_files(file_id: u64) -> Option<Vec<String>> {
    let zip_path = cache_dir().join(format!("{file_id}.zip"));
    let manifest = read_manifest(&zip_path).ok()?;
    let mut z = zip::ZipArchive::new(fs::File::open(&zip_path).ok()?).ok()?;
    let prefix = match manifest.overrides.trim_end_matches('/') {
        "" => "overrides/".to_string(),
        p => format!("{p}/"),
    };
    let mut out = Vec::new();
    for i in 0..z.len() {
        let e = z.by_index_raw(i).ok()?;
        let name = e.name().replace('\\', "/");
        if let Some(rel) = name.strip_prefix(&prefix)
            && !e.is_dir()
            && !rel.is_empty()
            && safe_pack_path(rel)
        {
            out.push(rel.replace('/', "\\"));
        }
    }
    Some(out)
}

/// Copies `overrides/` from the pack into the instance, except what
/// `overrides` says to leave. Returns files written.
fn extract_overrides(
    zip_path: &Path,
    prefix: &str,
    dest: &Path,
    overrides: Option<&Overrides>,
) -> anyhow::Result<usize> {
    let mut z = zip::ZipArchive::new(fs::File::open(zip_path)?)?;
    let prefix = format!("{}/", prefix.trim_end_matches('/'));
    let mut n = 0;
    for i in 0..z.len() {
        let mut e = z.by_index(i)?;
        let Some(rel) = e.enclosed_name() else {
            continue;
        };
        let Ok(rel) = rel.strip_prefix(&prefix) else {
            continue;
        };
        if e.is_dir() || rel.as_os_str().is_empty() {
            continue;
        }
        let key = rel.to_string_lossy().replace('\\', "/");
        if overrides.is_some_and(|o| o.skip.contains(&key)) {
            continue;
        }
        let out = dest.join(rel);
        fs::create_dir_all(out.parent().unwrap_or(dest))?;
        let mut f = fs::File::create(&out)?;
        std::io::copy(&mut e, &mut f)?;
        n += 1;
    }
    Ok(n)
}

fn pack_manifest_bytes(zip_path: &Path) -> anyhow::Result<Vec<u8>> {
    let mut z = zip::ZipArchive::new(fs::File::open(zip_path)?)?;
    let mut m = Vec::new();
    z.by_name("manifest.json")?.read_to_end(&mut m)?;
    Ok(m)
}

/// Installs a pack as a new instance under `instances_root`.
pub fn install(
    instances_root: &Path,
    pack: &cf::Mod,
    pack_file: &cf::File,
    job: &Job,
) -> anyhow::Result<Instance> {
    let zip_path = fetch_pack_zip(pack_file, job)?;
    let manifest = read_manifest(&zip_path)?;
    let root = unique_dir(instances_root, &pack.name);
    in_new_folder(&root, || {
        let mut inst = Instance {
            name: pack.name.clone(),
            root: root.clone(),
            game_version: manifest.minecraft.version.clone(),
            loader: manifest.loader(),
            pack: None,
            icon: pack.icon().map(str::to_string),
            settings: LaunchSettings {
                memory_mb: manifest.minecraft.recommended_ram,
                ..Default::default()
            },
            format: crate::instances::FORMAT,
            ..Default::default()
        };
        apply_pack(
            &mut inst,
            Some((pack, pack_file)),
            &zip_path,
            &manifest,
            None,
            job,
        )?;
        // Keep the pack's own manifest beside the instance, as CurseForge does.
        fs::write(root.join("manifest.json"), pack_manifest_bytes(&zip_path)?)?;
        inst.save()?;
        Ok(inst)
    })
}

/// Installs a CurseForge pack zip from disk as a new instance.
pub fn import_zip(instances_root: &Path, zip_path: &Path, job: &Job) -> anyhow::Result<Instance> {
    let manifest = read_manifest(zip_path)?;
    let name = if manifest.name.is_empty() {
        zip_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or("Imported pack".into())
    } else {
        manifest.name.clone()
    };
    let root = unique_dir(instances_root, &name);
    in_new_folder(&root, || {
        let mut inst = Instance {
            name,
            root: root.clone(),
            game_version: manifest.minecraft.version.clone(),
            loader: manifest.loader(),
            settings: LaunchSettings {
                memory_mb: manifest.minecraft.recommended_ram,
                ..Default::default()
            },
            format: crate::instances::FORMAT,
            ..Default::default()
        };
        apply_pack(&mut inst, None, zip_path, &manifest, None, job)?;
        fs::write(root.join("manifest.json"), pack_manifest_bytes(zip_path)?)?;
        inst.save()?;
        Ok(inst)
    })
}

/// Moves an existing pack instance to another pack file (update or downgrade).
/// Nothing in the instance changes until every file is downloaded; a
/// failed or cancelled download leaves it as it was.
pub fn update(
    inst: &mut Instance,
    pack: &cf::Mod,
    pack_file: &cf::File,
    overrides: Option<&Overrides>,
    job: &Job,
) -> anyhow::Result<()> {
    let zip_path = fetch_pack_zip(pack_file, job)?;
    let manifest = read_manifest(&zip_path)?;
    apply_pack(
        inst,
        Some((pack, pack_file)),
        &zip_path,
        &manifest,
        overrides,
        job,
    )?;
    fs::write(inst.root.join("manifest.json"), pack_manifest_bytes(&zip_path)?)?;
    inst.save()
}

// ── downloading a pack's files, then putting them in place ──────────

/// Downloads for one pack install or update, under the instance's
/// [`STAGING_DIR`]; the folder goes when this is dropped.
struct Staging {
    dir: PathBuf,
}

impl Staging {
    fn new(root: &Path) -> anyhow::Result<Self> {
        let dir = root.join(STAGING_DIR);
        // Left over from a crash: only ever Hopper's own temp files.
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// Where download `i` waits (one folder each, so names never clash).
    fn path(&self, i: usize, file_name: &str) -> PathBuf {
        self.dir.join(i.to_string()).join(file_name)
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// A downloaded file waiting to move into the instance.
struct Staged {
    /// The entry it becomes; `None` for a file outside mods/resource
    /// packs/shader packs, which goes to `rel` untracked.
    addon: Option<crate::instances::Addon>,
    /// Path under the instance, with `/`.
    rel: String,
    tmp: PathBuf,
}

/// How one file of a pack went.
enum Fetched {
    Done(Staged),
    /// Could not be downloaded here; the user can do it by hand.
    Pending(Pending),
    /// A real failure (network, wrong checksum): the update stops.
    Failed(String),
    Skipped,
}

/// Sorts what [`parallel`] fetched; fails, leaving the instance alone,
/// when the job was cancelled or any file failed.
fn sort_fetched(got: Vec<Fetched>, job: &Job) -> anyhow::Result<(Vec<Staged>, Vec<Pending>)> {
    let (mut staged, mut pending, mut failed) = (Vec::new(), Vec::new(), Vec::new());
    for g in got {
        match g {
            Fetched::Done(s) => staged.push(s),
            Fetched::Pending(p) => pending.push(p),
            Fetched::Failed(e) => failed.push(e),
            Fetched::Skipped => {}
        }
    }
    job.check_cancelled()?;
    if !failed.is_empty() {
        anyhow::bail!(
            "{} of the pack's files could not be downloaded, so nothing was changed:\n{}",
            failed.len(),
            failed.join("\n")
        );
    }
    Ok((staged, pending))
}

/// A 4xx answer: the file is gone or refused, retrying won't help. Rate
/// limits and timeouts count as failures instead.
fn refused(e: &anyhow::Error) -> bool {
    cf::http_status(e).is_some_and(|s| (400..500).contains(&s) && s != 408 && s != 429)
}

/// Runs `f` over `items` on a few threads; results keep the items' order.
/// Counts progress on `job`.
pub(crate) fn parallel<T: Sync, R: Send>(items: &[T], job: &Job, f: impl Fn(usize, &T) -> R + Sync) -> Vec<R> {
    let done = AtomicUsize::new(0);
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<Option<R>>> = Mutex::new(items.iter().map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..PARALLEL_DOWNLOADS.min(items.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    let r = f(i, item);
                    out.lock().unwrap_or_else(|e| e.into_inner())[i] = Some(r);
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    job.progress(n, items.len());
                    job.detail(format!("Downloaded {n} of {}", items.len()));
                }
            });
        }
    });
    out.into_inner()
        .unwrap_or_else(|e| e.into_inner())
        .into_iter()
        .flatten()
        .collect()
}

/// Moves the staged files into the instance. Each replaces the entry for
/// the same project (or file name), keeping its history and disabled
/// state; then pack entries `drop` picks go to backups.
fn commit(
    inst: &mut Instance,
    staged: Vec<Staged>,
    drop: impl Fn(&crate::instances::Addon) -> bool,
) -> anyhow::Result<()> {
    for s in staged {
        match s.addon {
            Some(new) => {
                let stem = crate::serverpack::jar_stem(&new.file_name);
                let old = inst
                    .addons
                    .iter()
                    .position(|o| {
                        (new.project_id != 0 && o.project_id == new.project_id)
                            || (o.folder() == new.folder()
                                && o.file_name.eq_ignore_ascii_case(&new.file_name))
                    })
                    // A file with no known project: the leaving pack file
                    // with the same name minus version ("a-1.jar" for
                    // "a-2.jar") is its older self.
                    .or_else(|| {
                        inst.addons.iter().position(|o| {
                            (o.project_id == 0 || new.project_id == 0)
                                && o.folder() == new.folder()
                                && drop(o)
                                && !stem.is_empty()
                                && crate::serverpack::jar_stem(&o.file_name) == stem
                        })
                    })
                    .map(|i| inst.addons.remove(i));
                crate::install::replace_addon(inst, old, new, &s.tmp)?;
            }
            None => {
                let to = inst.root.join(&s.rel);
                fs::create_dir_all(to.parent().unwrap_or(&inst.root))?;
                fs::rename(&s.tmp, &to)
                    .or_else(|_| fs::copy(&s.tmp, &to).map(|_| ()))
                    .with_context(|| format!("moving {} into place", s.rel))?;
            }
        }
    }
    let mut i = 0;
    while i < inst.addons.len() {
        if drop(&inst.addons[i]) {
            crate::install::move_out(&inst.root, &inst.addon_path(&inst.addons[i]), false)?;
            inst.addons.remove(i);
        } else {
            i += 1;
        }
    }
    Ok(())
}

/// A file name that stays inside its folder.
fn plain_file_name(name: &str) -> bool {
    crate::configdiff::safe_rel(name) && !name.contains(['/', '\\'])
}

/// `pack` is `None` for a zip from disk.
fn apply_pack(
    inst: &mut Instance,
    pack: Option<(&cf::Mod, &cf::File)>,
    zip_path: &Path,
    manifest: &PackManifest,
    overrides: Option<&Overrides>,
    job: &Job,
) -> anyhow::Result<()> {
    let wanted: HashMap<u64, u64> = manifest
        .files
        .iter()
        .map(|f| (f.project_id, f.file_id))
        .collect();

    // Only fetch files that changed. Nothing in the instance changes until
    // they are all here.
    let todo: Vec<u64> = wanted
        .iter()
        .filter(|(p, f)| {
            !inst
                .addons
                .iter()
                .any(|a| a.project_id == **p && a.file_id == **f)
        })
        .map(|(_, f)| *f)
        .collect();
    job.detail(format!("Looking up {} files", todo.len()));
    let files = cf::get_files(&todo)?;
    let mods: HashMap<u64, cf::Mod> =
        cf::get_mods(&files.iter().map(|f| f.mod_id).collect::<Vec<_>>())?
            .into_iter()
            .map(|m| (m.id, m))
            .collect();
    job.check_cancelled()?;

    let staging = Staging::new(&inst.root)?;
    let downloads = downloads_dir();
    let got = parallel(&files, job, |i, f| {
        if job.is_cancelled() {
            return Fetched::Failed("Cancelled".into());
        }
        let Some(m) = mods.get(&f.mod_id) else {
            return Fetched::Skipped;
        };
        if !plain_file_name(&f.file_name) {
            return Fetched::Failed(format!("{}: not a plain file name", f.file_name));
        }
        let folder = folder_for_class(m.class_id.unwrap_or(cf::CLASS_MODS));
        let tmp = staging.path(i, &f.file_name);
        let pending = |reason: String| {
            Fetched::Pending(Pending {
                name: m.name.clone(),
                file_name: f.file_name.clone(),
                folder: folder.to_string(),
                project_id: m.id,
                file_id: f.id,
                url: f.manual_url(m),
                reason,
                size: f.expect().len,
                sha1: f.expect().sha1,
                addon: Some(crate::install::addon_from(m, f, true)),
            })
        };
        let r = match cf::download_url(f) {
            Ok(url) => cf::download_checked(&url, &tmp, &f.expect(), Some(job), |_| {}),
            // Blocked by the author: use a copy the user already downloaded.
            Err(_) => match downloads
                .as_ref()
                .map(|d| d.join(&f.file_name))
                .filter(|p| p.is_file())
            {
                Some(p) => match fs::File::open(&p).map_err(anyhow::Error::from).and_then(|src| {
                    cf::write_checked(src, &tmp, &f.expect(), &|| false, &mut |_| {})
                }) {
                    Ok(()) => Ok(()),
                    Err(e) => return pending(format!("the copy in {}: {e:#}", p.display())),
                },
                None => {
                    return pending(
                        "The author only allows downloads from the CurseForge website".into(),
                    );
                }
            },
        };
        match r {
            Ok(()) => Fetched::Done(Staged {
                addon: Some(addon_from(m, f, true)),
                rel: format!("{folder}/{}", f.file_name),
                tmp,
            }),
            Err(_) if job.is_cancelled() => Fetched::Failed("Cancelled".into()),
            Err(e) if refused(&e) => pending(format!("{e:#}")),
            Err(e) => Fetched::Failed(format!("{}: {e:#}", f.file_name)),
        }
    });
    let (staged, pending) = sort_fetched(got, job)?;

    // Everything is here: now change the instance. Pack mods dropped by
    // the new version go to backups; mods you added stay.
    inst.game_version = manifest.minecraft.version.clone();
    inst.loader = manifest.loader();
    commit(inst, staged, |a| {
        a.from_pack && !wanted.contains_key(&a.project_id)
    })?;
    drop(staging);
    note_pending(job, &pending, true);
    write_pending(&inst.root, &pending)?;
    // The files moved: record them before anything else can fail.
    inst.save()?;

    job.detail("Copying overrides");
    prepare_overrides(&inst.root, overrides)?;
    let n = extract_overrides(
        zip_path,
        if manifest.overrides.is_empty() {
            "overrides"
        } else {
            &manifest.overrides
        },
        &inst.root,
        overrides,
    )?;
    log::info!("{} overrides written", n);
    write_merged(&inst.root, overrides)?;

    inst.pack = match pack {
        Some((pack, pack_file)) => Some(PackRef {
            project_id: pack.id,
            file_id: pack_file.id,
            name: pack.name.clone(),
            version: manifest.version.clone(),
            server_pack_file_id: pack_file.server_pack_file_id.unwrap_or(0),
            file_name: pack_file.file_name.clone(),
            ..Default::default()
        }),
        // A local zip still names its CurseForge project, when it has one.
        None => (manifest.project_id != 0).then(|| PackRef {
            project_id: manifest.project_id,
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            ..Default::default()
        }),
    };
    Ok(())
}

/// Backs up the files an update replaces although you changed them, and
/// moves away the ones the pack dropped. Backups keep their folders, under
/// `.hopper-backups/<time>/pack-update/`.
fn prepare_overrides(root: &Path, overrides: Option<&Overrides>) -> anyhow::Result<()> {
    let Some(o) = overrides else { return Ok(()) };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let dir = root
        .join(crate::sync::BACKUP_DIR)
        .join(stamp.to_string())
        .join("pack-update");
    for (rel, remove) in o
        .backup
        .iter()
        .map(|r| (r, false))
        .chain(o.remove.iter().map(|r| (r, true)))
    {
        let src = root.join(rel);
        if !src.is_file() {
            continue;
        }
        let dest = dir.join(rel);
        fs::create_dir_all(dest.parent().unwrap_or(&dir))?;
        if remove {
            fs::rename(&src, &dest).or_else(|_| fs::copy(&src, &dest).and_then(|_| fs::remove_file(&src)))?;
        } else {
            fs::copy(&src, &dest)?;
        }
    }
    Ok(())
}

/// Writes merged override files.
fn write_merged(root: &Path, overrides: Option<&Overrides>) -> anyhow::Result<()> {
    for (rel, bytes) in overrides.map(|o| o.write.as_slice()).unwrap_or_default() {
        let out = root.join(rel);
        fs::create_dir_all(out.parent().unwrap_or(root))?;
        fs::write(out, bytes)?;
    }
    Ok(())
}

/// Folders a pack export includes, when present.
pub const EXPORT_FOLDERS: [&str; 8] = [
    "config",
    "defaultconfigs",
    "kubejs",
    "scripts",
    "resourcepacks",
    "shaderpacks",
    "datapacks",
    "options.txt",
];

/// Writes a CurseForge-format pack zip: tracked projects go in `manifest.json`,
/// loose files and `folders` go under `overrides/`.
pub fn export(
    inst: &Instance,
    version: &str,
    author: &str,
    folders: &[&str],
    out: &Path,
) -> anyhow::Result<()> {
    // Only CurseForge projects can go in a CurseForge manifest; Modrinth
    // files ride along as overrides.
    let curse =
        |a: &&crate::instances::Addon| a.project_id != 0 && !crate::modrinth::is_mr(a.project_id);
    let tracked: HashSet<String> = inst
        .addons
        .iter()
        .filter(curse)
        .map(|a| a.file_name.to_lowercase())
        .collect();
    let files: Vec<serde_json::Value> = inst
        .addons
        .iter()
        .filter(curse)
        .filter(|a| a.enabled)
        .map(|a| serde_json::json!({ "projectID": a.project_id, "fileID": a.file_id, "required": true }))
        .collect();
    let manifest = serde_json::json!({
        "minecraft": {
            "version": inst.game_version,
            "modLoaders": [{ "id": inst.loader, "primary": true }],
            "recommendedRam": inst.settings.memory_mb,
        },
        "manifestType": "minecraftModpack",
        "manifestVersion": 1,
        "name": inst.name,
        "version": version,
        "author": author,
        "files": files,
        "overrides": "overrides",
    });

    let mut z = zip::ZipWriter::new(fs::File::create(out)?);
    let opts = zip::write::SimpleFileOptions::default();
    z.start_file("manifest.json", opts)?;
    z.write_all(serde_json::to_string_pretty(&manifest)?.as_bytes())?;

    let add = |path: &Path, rel: &str, z: &mut zip::ZipWriter<fs::File>| -> anyhow::Result<()> {
        z.start_file(format!("overrides/{rel}"), opts)?;
        std::io::copy(&mut fs::File::open(path)?, z)?;
        Ok(())
    };
    // Untracked jars ride along as overrides.
    if let Ok(rd) = fs::read_dir(inst.mods_dir()) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.to_lowercase().ends_with(".jar") && !tracked.contains(&n.to_lowercase()) {
                add(&e.path(), &format!("mods/{n}"), &mut z)?;
            }
        }
    }
    for folder in folders {
        let base = inst.root.join(folder);
        if base.is_file() {
            add(&base, folder, &mut z)?;
            continue;
        }
        let mut stack = vec![base.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(rel) = p.strip_prefix(&inst.root) {
                    let rel = rel.to_string_lossy().replace('\\', "/");
                    // Skip projects already in the manifest.
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    if !tracked.contains(&name) {
                        add(&p, &rel, &mut z)?;
                    }
                }
            }
        }
    }
    z.finish()?;
    Ok(())
}

/// A new empty instance with no pack.
pub fn create(
    instances_root: &Path,
    name: &str,
    game_version: &str,
    loader: &str,
) -> anyhow::Result<Instance> {
    let root = unique_dir(instances_root, name);
    fs::create_dir_all(root.join("mods"))?;
    let inst = Instance {
        name: name.to_string(),
        root,
        game_version: game_version.to_string(),
        loader: loader.to_string(),
        format: crate::instances::FORMAT,
        ..Default::default()
    };
    inst.save()?;
    Ok(inst)
}

#[cfg(test)]
mod tests {
    #[test]
    fn picks_up_browser_downloads() {
        let t = std::env::temp_dir().join(format!("hopper-dl-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        let (dl, root) = (t.join("Downloads"), t.join("inst"));
        fs::create_dir_all(&dl).unwrap();
        fs::create_dir_all(&root).unwrap();
        let body = b"jar bytes".to_vec();
        let sha1 = crate::modrinth::sha1(&body);
        let p = Pending {
            name: "Blocked Mod".into(),
            file_name: "blocked-1.0.jar".into(),
            folder: "mods".into(),
            project_id: 42,
            file_id: 7,
            size: body.len() as u64,
            sha1: sha1.clone(),
            addon: Some(crate::instances::Addon {
                project_id: 42,
                file_id: 7,
                name: "Blocked Mod".into(),
                file_name: "blocked-1.0.jar".into(),
                enabled: true,
                class_id: cf::CLASS_MODS,
                ..Default::default()
            }),
            ..Default::default()
        };
        add_pending(&root, &[p.clone()]).unwrap();
        // Half-written and wrong-content files don't count.
        fs::write(dl.join("blocked-1.0.jar.crdownload"), &body).unwrap();
        fs::write(dl.join("blocked-1.0.jar"), b"other bytes").unwrap();
        assert!(find_download(&p, &dl).is_none());
        // The browser's renamed copy with the right bytes does.
        fs::write(dl.join("blocked-1.0 (1).jar"), &body).unwrap();
        assert_eq!(find_download(&p, &dl), Some(dl.join("blocked-1.0 (1).jar")));

        let mut inst = Instance { root: root.clone(), name: "t".into(), ..Default::default() };
        let placed = place_downloads(&mut inst, &dl).unwrap();
        assert_eq!(placed, ["Blocked Mod"]);
        assert_eq!(fs::read(root.join("mods/blocked-1.0.jar")).unwrap(), body);
        assert!(!dl.join("blocked-1.0 (1).jar").exists());
        assert!(dl.join("blocked-1.0.jar").exists(), "unrelated files stay");
        assert!(pending_downloads(&inst).is_empty());
        assert_eq!(inst.addons.len(), 1);
        assert_eq!(inst.addons[0].project_id, 42);
        fs::remove_dir_all(&t).unwrap();
    }

    use super::*;

    #[test]
    fn unique_dirs_do_not_collide() {
        let root = std::env::temp_dir().join(format!("hopper-uniq-{}", std::process::id()));
        fs::create_dir_all(root.join("Pack")).unwrap();
        assert_eq!(unique_dir(&root, "Pack"), root.join("Pack (2)"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn export_then_read_manifest() {
        let root = std::env::temp_dir().join(format!("hopper-exp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut inst = create(&root, "Test", "1.21.1", "neoforge-21.1.256").unwrap();
        fs::create_dir_all(inst.root.join("config")).unwrap();
        fs::write(inst.root.join("config/a.toml"), "x=1").unwrap();
        fs::write(inst.root.join("mods/loose.jar"), "jar").unwrap();
        inst.addons.push(crate::instances::Addon {
            project_id: 1,
            file_id: 2,
            file_name: "t.jar".into(),
            enabled: true,
            ..Default::default()
        });
        let out = root.join("out.zip");
        export(&inst, "1.0", "me", &["config"], &out).unwrap();
        let m = read_manifest(&out).unwrap();
        assert_eq!(m.files.len(), 1);
        assert_eq!(m.loader(), "neoforge-21.1.256");
        let z = zip::ZipArchive::new(fs::File::open(&out).unwrap()).unwrap();
        let names: Vec<_> = z.file_names().collect();
        assert!(names.contains(&"overrides/config/a.toml"));
        assert!(names.contains(&"overrides/mods/loose.jar"));
        let _ = fs::remove_dir_all(&root);
    }
}

/// End to end against real CurseForge, in a temp folder:
/// `cargo test real_end_to_end -- --ignored --nocapture`
#[cfg(test)]
#[test]
#[ignore]
fn real_end_to_end() {
    crate::env::load(Path::new(".env"));
    let root = std::env::temp_dir().join("hopper-e2e");
    let _ = fs::remove_dir_all(&root);
    let jobs = crate::jobs::Jobs::default();

    // A fresh instance, then JEI (238222) with dependencies.
    let mut inst = create(&root, "E2E", "1.21.1", "neoforge-21.1.256").unwrap();
    let jei = cf::get_mod(238222).unwrap();
    let f = crate::install::best_file(jei.id, jei.class_id, &inst)
        .unwrap()
        .unwrap();
    let job = jobs.start("jei");
    let added = crate::install::install(&mut inst, &jei, &f, &job).unwrap();
    println!("installed {added:?} -> {}", f.file_name);
    assert!(inst.mods_dir().join(&f.file_name).exists());

    // Pretend we have an older JEI and check the update path.
    // JEI's 1.21.1 builds are betas: on the Beta channel an older beta
    // must offer the newest one.
    let older = cf::files(jei.id, "1.21.1", cf::Loader::NeoForge)
        .unwrap()
        .into_iter()
        .nth(3)
        .unwrap();
    inst.settings.release_channel = 2;
    inst.addons[0].file_id = older.id;
    inst.addons[0].release_type = older.release_type;
    let ups = crate::install::check_updates(&inst).unwrap();
    println!(
        "updates: {:?}",
        ups.iter().map(|u| &u.file.file_name).collect::<Vec<_>>()
    );
    assert_eq!(ups.len(), 1);
    let (logs, all) =
        crate::install::changelogs(&inst, jei.id, older.id, Some(&ups[0].file), false).unwrap();
    assert!(all.len() > logs.len());
    println!("changelog entries: {}", logs.len());
    assert!(logs.len() >= 3 && logs.last().unwrap().installed);

    // Disable, enable, remove.
    crate::install::set_enabled(&mut inst, 0, false).unwrap();
    assert!(
        inst.mods_dir()
            .join(format!("{}.disabled", f.file_name))
            .exists()
    );
    crate::install::set_enabled(&mut inst, 0, true).unwrap();
    crate::install::remove(&mut inst, 0).unwrap();
    assert!(!inst.mods_dir().join(&f.file_name).exists());

    // A small real modpack.
    let pack = cf::get_mod(1459394).unwrap();
    let pf = cf::file(pack.id, pack.main_file_id).unwrap();
    let job = jobs.start("pack");
    let p = install(&root, &pack, &pf, &job).unwrap();
    let state = jobs.snapshot();
    println!(
        "pack {} -> {} addons, loader {}, notes {:?}",
        p.name,
        p.addons.len(),
        p.loader,
        state.last().map(|j| &j.notes)
    );
    assert!(!p.addons.is_empty());
    let reloaded = crate::instances::load(&p.root).unwrap();
    assert_eq!(reloaded.addons.len(), p.addons.len());

    // Export it and read it back.
    let out = root.join("export.zip");
    export(&p, "1.0", "me", &["config"], &out).unwrap();
    assert!(read_manifest(&out).unwrap().files.len() >= p.addons.len() - 1);

    // Import the export back from disk.
    let job = jobs.start("import");
    let imported = import_zip(&root, &out, &job).unwrap();
    println!(
        "imported {} -> {} addons",
        imported.name,
        imported.addons.len()
    );
    assert!(imported.addons.len() + 3 >= p.addons.len());
    assert!(imported.root != p.root);
    let _ = fs::remove_dir_all(&root);
}

// ── Modrinth packs (.mrpack) ─────────────────────────────────────────

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct MrIndex {
    name: String,
    version_id: String,
    pub(crate) files: Vec<MrIndexFile>,
    dependencies: HashMap<String, String>,
}

#[derive(serde::Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct MrIndexFile {
    pub(crate) path: String,
    pub(crate) hashes: HashMap<String, String>,
    downloads: Vec<String>,
    pub(crate) env: Option<HashMap<String, String>>,
    file_size: u64,
}

impl MrIndex {
    /// The Minecraft version it's for.
    pub(crate) fn minecraft(&self) -> String {
        self.dependencies.get("minecraft").cloned().unwrap_or_default()
    }

    /// Hopper's loader id, e.g. "neoforge-21.1.256".
    pub(crate) fn loader(&self) -> String {
        for (key, kind) in [
            ("neoforge", "neoforge"),
            ("forge", "forge"),
            ("fabric-loader", "fabric"),
            ("quilt-loader", "quilt"),
        ] {
            if let Some(v) = self.dependencies.get(key) {
                return format!("{kind}-{v}");
            }
        }
        String::new()
    }
}

/// Installs a Modrinth modpack version as a new instance.
pub fn install_mrpack(
    instances_root: &Path,
    pack: &cf::Mod,
    file: &cf::File,
    job: &Job,
) -> anyhow::Result<Instance> {
    let zip_path = fetch_mrpack(file, job)?;
    let index = read_mr_index(&zip_path)?;

    let root = unique_dir(instances_root, &pack.name);
    in_new_folder(&root, || {
        let mut inst = Instance {
            name: pack.name.clone(),
            root: root.clone(),
            game_version: index
                .dependencies
                .get("minecraft")
                .cloned()
                .unwrap_or_default(),
            loader: index.loader(),
            icon: pack.icon().map(str::to_string),
            format: crate::instances::FORMAT,
            ..Default::default()
        };
        apply_mrpack(&mut inst, pack, file, &zip_path, &index, None, job)?;
        Ok(inst)
    })
}

/// Imports a `.mrpack` from disk. When the file is a published Modrinth
/// pack version (found by its sha1) the instance can update like one
/// installed from Browse; otherwise it's a plain instance.
pub fn import_mrpack(instances_root: &Path, path: &Path, job: &Job) -> anyhow::Result<Instance> {
    let index = read_mr_index(path)?;
    job.detail("Looking the pack up on Modrinth");
    let bytes = fs::read(path)?;
    let sha1 = crate::modrinth::sha1(&bytes);
    let published = crate::modrinth::versions_by_sha1(std::slice::from_ref(&sha1))
        .ok()
        .and_then(|m| m.get(&sha1).cloned())
        .and_then(|v| {
            let p = crate::modrinth::project(&v.project_id).ok()?;
            Some((
                crate::modrinth::project_to_mod(&p, ""),
                crate::modrinth::version_to_file(&v),
            ))
        });
    let (pack, file) = published.clone().unwrap_or_else(|| {
        let name = if index.name.is_empty() {
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or("Imported pack".into())
        } else {
            index.name.clone()
        };
        (
            cf::Mod {
                name,
                ..Default::default()
            },
            cf::File {
                file_name: path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                ..Default::default()
            },
        )
    });
    let root = unique_dir(instances_root, &pack.name);
    in_new_folder(&root, || {
        let mut inst = Instance {
            name: pack.name.clone(),
            root: root.clone(),
            game_version: index
                .dependencies
                .get("minecraft")
                .cloned()
                .unwrap_or_default(),
            loader: index.loader(),
            icon: pack.icon().map(str::to_string),
            format: crate::instances::FORMAT,
            ..Default::default()
        };
        apply_mrpack(&mut inst, &pack, &file, path, &index, None, job)?;
        if published.is_none() {
            // Not on Modrinth: nothing to update from.
            inst.pack = None;
            for a in &mut inst.addons {
                a.from_pack = true;
            }
            inst.save()?;
        }
        Ok(inst)
    })
}

/// Override folders of a Modrinth pack, later ones on top.
pub(crate) const MR_OVERRIDES: [&str; 2] = ["overrides", "client-overrides"];

/// The cache file for a Modrinth pack version: by version id (or the
/// file's sha1), so two packs with the same file name never collide.
fn mrpack_cache_name(file: &cf::File) -> String {
    let mr = file.modrinth.as_ref();
    let key = mr
        .map(|m| m.version_id.as_str())
        .filter(|v| !v.is_empty())
        .map(|v| format!("mr-{v}"))
        .or_else(|| file.sha1().map(|h| format!("sha1-{h}")))
        .or_else(|| (file.id != 0).then(|| format!("id-{}", file.id)))
        .unwrap_or_else(|| {
            let url = file.download_url.as_deref().unwrap_or_default();
            format!(
                "url-{}",
                sha1_smol::Sha1::from(format!("{url}\n{}", file.file_name)).digest()
            )
        });
    format!("{}.mrpack", Instance::folder_name(&key))
}

pub(crate) fn fetch_mrpack(file: &cf::File, job: &Job) -> anyhow::Result<PathBuf> {
    let url = file.download_url.clone().context("pack has no download")?;
    let zip_path = cache_dir().join(mrpack_cache_name(file));
    if !cache_hit(&zip_path) {
        job.detail(format!("Downloading {}", file.file_name));
        let total = file.file_length as usize;
        cf::download_checked(&url, &zip_path, &file.expect(), Some(job), |n| {
            job.progress(n as usize, total)
        })?;
    }
    Ok(zip_path)
}

pub(crate) fn read_mr_index(zip_path: &Path) -> anyhow::Result<MrIndex> {
    let mut z = zip::ZipArchive::new(fs::File::open(zip_path)?)?;
    let mut text = String::new();
    z.by_name("modrinth.index.json")
        .context("not a Modrinth pack")?
        .read_to_string(&mut text)?;
    Ok(serde_json::from_str(&text)?)
}

/// Moves a Modrinth pack instance to another pack version. Nothing in the
/// instance changes until every file is downloaded and checked.
pub fn update_mrpack(
    inst: &mut Instance,
    pack: &cf::Mod,
    file: &cf::File,
    overrides: Option<&Overrides>,
    job: &Job,
) -> anyhow::Result<()> {
    let zip_path = fetch_mrpack(file, job)?;
    let index = read_mr_index(&zip_path)?;
    apply_mrpack(inst, pack, file, &zip_path, &index, overrides, job)
}

/// Updates or switches whichever site the pack came from. Override files
/// follow `overrides`; `None` merges them the safe way (see `packdiff`):
/// files only you changed are kept, changes to different lines are merged,
/// and where both changed the same lines the pack's copy wins only in pack
/// content folders (quests, scripts…), with yours backed up.
pub fn update_any(
    inst: &mut Instance,
    pack_id: u64,
    file: &cf::File,
    overrides: Option<&Overrides>,
    job: &Job,
) -> anyhow::Result<()> {
    let merged;
    let overrides = match overrides {
        Some(o) => o,
        None => {
            let plan = crate::packdiff::plan(inst, file, job)?;
            merged = plan.decisions(&plan.default_take());
            &merged
        }
    };
    let m = crate::source::get_mod(pack_id)?;
    if crate::modrinth::is_mr(pack_id) {
        update_mrpack(inst, &m, file, Some(overrides), job)
    } else {
        update(inst, &m, file, Some(overrides), job)
    }
}

/// A pack path that stays inside the instance: relative, no `..`, no
/// drive or UNC prefix.
fn safe_pack_path(path: &str) -> bool {
    crate::configdiff::safe_rel(path)
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Folder class and file name of a pack file Hopper lists as a project:
/// one directly in mods/, resourcepacks/ or shaderpacks/.
fn listed_folder(path: &str) -> Option<(u32, &str)> {
    let (dir, name) = path.split_once('/')?;
    if name.is_empty() || name.contains('/') {
        return None;
    }
    let class = match dir {
        "mods" => cf::CLASS_MODS,
        "resourcepacks" => cf::CLASS_RESOURCE_PACKS,
        "shaderpacks" => cf::CLASS_SHADERS,
        _ => return None,
    };
    Some((class, name))
}

impl MrIndexFile {
    fn sha1(&self) -> String {
        self.hashes
            .get("sha1")
            .map(|h| h.to_ascii_lowercase())
            .unwrap_or_default()
    }

    pub(crate) fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// The entry for a pack file nobody identified: no project, just its name
/// and hash, so later updates can replace or remove it.
fn plain_pack_addon(path: &str, sha1: &str) -> Option<crate::instances::Addon> {
    let (class_id, name) = listed_folder(path)?;
    let stem = name
        .strip_suffix(".jar")
        .or_else(|| name.strip_suffix(".zip"))
        .unwrap_or(name);
    Some(crate::instances::Addon {
        file_name: name.to_string(),
        name: stem.to_string(),
        enabled: true,
        from_pack: true,
        class_id,
        date_added: crate::time::now_iso(),
        mr_sha1: sha1.to_string(),
        ..Default::default()
    })
}

/// Modrinth versions (by sha1) and their projects.
type MrLookup = (
    HashMap<String, crate::modrinth::Version>,
    Vec<crate::modrinth::Project>,
);

/// Downloads `url` to `dest`, checked against the expectation.
type MrDownload<'a> = &'a (dyn Fn(&str, &Path, &cf::Expect, &Job) -> anyhow::Result<()> + Sync);

fn lookup_on_modrinth(hashes: &[String]) -> MrLookup {
    let versions = crate::modrinth::versions_by_sha1(hashes).unwrap_or_default();
    let projects = crate::modrinth::projects(
        &versions
            .values()
            .map(|v| v.project_id.clone())
            .collect::<Vec<_>>(),
    )
    .unwrap_or_default();
    (versions, projects)
}

fn apply_mrpack(
    inst: &mut Instance,
    pack: &cf::Mod,
    file: &cf::File,
    zip_path: &Path,
    index: &MrIndex,
    overrides: Option<&Overrides>,
    job: &Job,
) -> anyhow::Result<()> {
    apply_mrpack_with(
        inst,
        pack,
        file,
        zip_path,
        index,
        overrides,
        job,
        &|url, dest, expect, job| cf::download_checked(url, dest, expect, Some(job), |_| {}),
        &lookup_on_modrinth,
    )
}

#[allow(clippy::too_many_arguments)]
fn apply_mrpack_with(
    inst: &mut Instance,
    pack: &cf::Mod,
    file: &cf::File,
    zip_path: &Path,
    index: &MrIndex,
    overrides: Option<&Overrides>,
    job: &Job,
    download: MrDownload,
    lookup: &dyn Fn(&[String]) -> MrLookup,
) -> anyhow::Result<()> {
    let root = inst.root.clone();
    // Client files only; servers get their own.
    let mut files: Vec<MrIndexFile> = Vec::new();
    for f in &index.files {
        let client = f.env.as_ref().and_then(|e| e.get("client"));
        if client.is_some_and(|c| c == "unsupported") {
            continue;
        }
        if !safe_pack_path(&f.path) {
            job.note(format!(
                "Skipped {}: the path leaves the instance folder",
                f.path
            ));
            continue;
        }
        files.push(f.clone());
    }
    let keep: HashSet<String> = files.iter().map(|f| f.file_name().to_lowercase()).collect();

    // Files already in place (same hash) are not fetched again.
    let present: HashSet<String> = inst
        .addons
        .iter()
        .filter(|a| !a.mr_sha1.is_empty() && inst.addon_path(a).exists())
        .map(|a| a.mr_sha1.to_ascii_lowercase())
        .collect();
    let todo: Vec<MrIndexFile> = files
        .into_iter()
        .filter(|f| {
            let sha1 = f.sha1();
            if sha1.is_empty() {
                return true;
            }
            if listed_folder(&f.path).is_some() {
                !present.contains(&sha1)
            } else {
                // Untracked files: compare what is on disk.
                fs::read(root.join(&f.path))
                    .map_or(true, |b| crate::modrinth::sha1(&b) != sha1)
            }
        })
        .collect();

    let staging = Staging::new(&root)?;
    let got = parallel(&todo, job, |i, f| {
        if job.is_cancelled() {
            return Fetched::Failed("Cancelled".into());
        }
        let tmp = staging.path(i, f.file_name());
        let expect = cf::Expect {
            len: f.file_size,
            sha1: f.sha1(),
        };
        let pending = |url: &str, reason: String| {
            Fetched::Pending(Pending {
                name: f.file_name().to_string(),
                file_name: f.file_name().to_string(),
                folder: f
                    .path
                    .rsplit_once('/')
                    .map(|(d, _)| d.to_string())
                    .unwrap_or_default(),
                url: url.to_string(),
                reason,
                size: f.file_size,
                sha1: f.sha1(),
                addon: plain_pack_addon(&f.path, &f.sha1()),
                ..Default::default()
            })
        };
        // Mirrors in order.
        let mut last = None;
        for url in &f.downloads {
            match download(url, &tmp, &expect, job) {
                Ok(()) => {
                    return Fetched::Done(Staged {
                        addon: plain_pack_addon(&f.path, &f.sha1()),
                        rel: f.path.clone(),
                        tmp,
                    });
                }
                Err(e) => last = Some(e),
            }
        }
        match last {
            None => pending("", "the pack gives no download link".into()),
            Some(_) if job.is_cancelled() => Fetched::Failed("Cancelled".into()),
            Some(e) if refused(&e) => pending(&f.downloads[0], format!("{e:#}")),
            Some(e) => Fetched::Failed(format!("{}: {e:#}", f.path)),
        }
    });
    let (mut staged, pending) = sort_fetched(got, job)?;

    // Name the pack's files through Modrinth so updates and icons work.
    // Files it doesn't know stay listed under their file names.
    job.detail("Looking up pack files on Modrinth");
    let hashes: Vec<String> = staged
        .iter()
        .filter_map(|s| s.addon.as_ref())
        .map(|a| a.mr_sha1.clone())
        .filter(|h| !h.is_empty())
        .collect();
    let (versions, projects) = lookup(&hashes);
    for s in &mut staged {
        let Some(plain) = s.addon.as_mut() else {
            continue;
        };
        let Some(v) = versions.get(&plain.mr_sha1) else {
            continue;
        };
        let Some(p) = projects.iter().find(|p| p.id == v.project_id) else {
            continue;
        };
        let m = crate::modrinth::project_to_mod(p, "");
        let mut mr_file = crate::modrinth::version_to_file(v);
        if let Some(mr) = mr_file.modrinth.as_mut() {
            mr.sha1 = plain.mr_sha1.clone();
        }
        let mut a = addon_from(&m, &mr_file, true);
        a.file_name = plain.file_name.clone();
        a.class_id = plain.class_id;
        a.env = crate::modrinth::env(&p.client_side, &p.server_side).to_string();
        *plain = a;
    }

    // Everything is here: now change the instance. Pack files the new
    // version dropped go to backups; files you added stay.
    if let Some(mc) = index.dependencies.get("minecraft") {
        inst.game_version = mc.clone();
    }
    inst.loader = index.loader();
    commit(inst, staged, |a| {
        a.from_pack && !keep.contains(&a.file_name.to_lowercase())
    })?;
    drop(staging);
    note_pending(job, &pending, false);
    write_pending(&root, &pending)?;
    inst.save()?;

    // Overrides, then client overrides on top.
    prepare_overrides(&root, overrides)?;
    for prefix in MR_OVERRIDES {
        extract_overrides(zip_path, prefix, &root, overrides)?;
    }
    write_merged(&root, overrides)?;

    inst.pack = Some(PackRef {
        project_id: pack.id,
        file_id: file.id,
        name: pack.name.clone(),
        version: index.version_id.clone(),
        server_pack_file_id: 0,
        file_name: file.file_name.clone(),
        mr_project: pack.modrinth.clone().unwrap_or_default(),
        mr_version: file.modrinth.as_ref().map(|m| m.version_id.clone()).unwrap_or_default(),
    });
    if inst.name.is_empty() {
        inst.name = index.name.clone();
    }
    inst.save()
}

#[cfg(test)]
mod mrpack_import_tests {
    use super::*;

    #[test]
    #[ignore = "network: downloads Fabulously Optimized"]
    fn imports_a_local_mrpack() {
        let root = std::env::temp_dir().join(format!("hopper-mrimport-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let jobs = crate::jobs::Jobs::new(crate::waker::noop());
        let v =
            crate::modrinth::versions("fabulously-optimized", "1.21.1", cf::Loader::Any).unwrap();
        let f = crate::modrinth::version_to_file(&v[0]);
        let path = root.join(&f.file_name);
        cf::download(f.download_url.as_ref().unwrap(), &path, |_| {}).unwrap();

        let a = import_mrpack(&root.join("instances"), &path, &jobs.start("a")).unwrap();
        println!(
            "published: {} {:?} addons {}",
            a.name,
            a.pack.as_ref().map(|p| &p.version),
            a.addons.len()
        );
        assert!(a.pack.is_some());
        assert!(!a.addons.is_empty());

        // Re-zipped: same content, different bytes, so not a published file.
        let copy = root.join("custom.mrpack");
        {
            let mut src = zip::ZipArchive::new(fs::File::open(&path).unwrap()).unwrap();
            let mut out = zip::ZipWriter::new(fs::File::create(&copy).unwrap());
            for i in 0..src.len() {
                let e = src.by_index(i).unwrap();
                out.raw_copy_file(e).unwrap();
            }
            out.set_comment("hopper test");
            out.finish().unwrap();
        }
        let b = import_mrpack(&root.join("instances"), &copy, &jobs.start("b")).unwrap();
        println!(
            "custom: {} pack {:?} addons {}",
            b.name,
            b.pack.is_some(),
            b.addons.len()
        );
        assert!(b.pack.is_none());
        assert!(b.root != a.root);
        let _ = fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod override_tests {
    use super::*;

    #[test]
    fn overrides_follow_the_decisions() {
        let dir = std::env::temp_dir().join(format!("hopper-overrides-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let root = dir.join("inst");
        fs::create_dir_all(root.join("config")).unwrap();
        for (f, t) in [
            ("config/keep.toml", "mine"),
            ("config/take.toml", "mine"),
            ("config/gone.toml", "old"),
            ("config/merge.toml", "mine"),
        ] {
            fs::write(root.join(f), t).unwrap();
        }
        let zip_path = dir.join("pack.zip");
        {
            let mut z = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
            let opts = zip::write::SimpleFileOptions::default();
            for (f, t) in [
                ("overrides/config/keep.toml", "pack"),
                ("overrides/config/take.toml", "pack"),
                ("overrides/config/new.toml", "pack"),
                ("overrides/config/merge.toml", "pack"),
            ] {
                z.start_file(f, opts).unwrap();
                z.write_all(t.as_bytes()).unwrap();
            }
            z.finish().unwrap();
        }
        let o = Overrides {
            skip: ["config/keep.toml".to_string(), "config/merge.toml".to_string()].into(),
            remove: vec!["config/gone.toml".into()],
            backup: vec!["config/take.toml".into(), "config/merge.toml".into()],
            write: vec![("config/merge.toml".into(), b"merged".to_vec())],
        };
        prepare_overrides(&root, Some(&o)).unwrap();
        extract_overrides(&zip_path, "overrides", &root, Some(&o)).unwrap();
        write_merged(&root, Some(&o)).unwrap();
        let read = |f: &str| fs::read_to_string(root.join(f)).ok();
        assert_eq!(read("config/keep.toml").as_deref(), Some("mine"));
        assert_eq!(read("config/take.toml").as_deref(), Some("pack"));
        assert_eq!(read("config/new.toml").as_deref(), Some("pack"));
        assert_eq!(read("config/merge.toml").as_deref(), Some("merged"));
        assert_eq!(read("config/gone.toml"), None);
        // Backups keep their folders.
        let backups: Vec<PathBuf> = walk(&root.join(crate::sync::BACKUP_DIR));
        let names: Vec<String> = backups
            .iter()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .collect();
        assert!(names.iter().any(|n| n.ends_with("pack-update/config/take.toml")));
        assert!(names.iter().any(|n| n.ends_with("pack-update/config/gone.toml")));
        let _ = fs::remove_dir_all(&dir);
    }

    fn walk(d: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for e in fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
        out
    }
}

#[cfg(test)]
mod pack_fix_tests {
    use super::*;
    use crate::instances::Addon;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-packfix-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn sha(b: &[u8]) -> String {
        crate::modrinth::sha1(b)
    }

    fn zip_with(path: &Path, entries: &[(&str, &[u8])]) {
        let mut z = zip::ZipWriter::new(fs::File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        for (n, b) in entries {
            z.start_file(*n, opts).unwrap();
            z.write_all(b).unwrap();
        }
        z.finish().unwrap();
    }

    /// An index listing `files` as (path, url); the fake server below
    /// serves `url` as its own bytes, so sha1 is of the url text unless
    /// `bad_hash` names the path.
    fn index(files: &[(&str, &str)], bad_hash: &str) -> MrIndex {
        let files: Vec<serde_json::Value> = files
            .iter()
            .map(|(p, u)| {
                let h = if *p == bad_hash {
                    "0".repeat(40)
                } else {
                    sha(u.as_bytes())
                };
                serde_json::json!({
                    "path": p,
                    "hashes": { "sha1": h, "sha512": "ignored" },
                    "downloads": [u],
                    "fileSize": u.len(),
                })
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "name": "Pack",
            "versionId": "1",
            "files": files,
            "dependencies": { "minecraft": "1.21.1", "neoforge": "21.1.1" },
        }))
        .unwrap()
    }

    fn fake_download(url: &str, dest: &Path, expect: &cf::Expect, _: &Job) -> anyhow::Result<()> {
        if url.starts_with("404") {
            return Err(anyhow::Error::new(ureq::Error::StatusCode(404)));
        }
        cf::write_checked(
            std::io::Cursor::new(url.as_bytes().to_vec()),
            dest,
            expect,
            &|| false,
            &mut |_| {},
        )
    }

    fn nobody_knows(_: &[String]) -> MrLookup {
        Default::default()
    }

    fn apply(inst: &mut Instance, idx: &MrIndex, zip: &Path, job: &Job) -> anyhow::Result<()> {
        apply_mrpack_with(
            inst,
            &cf::Mod::default(),
            &cf::File::default(),
            zip,
            idx,
            None,
            job,
            &fake_download,
            &nobody_knows,
        )
    }

    fn mods(inst: &Instance) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(inst.mods_dir())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn unsafe_pack_paths_are_rejected() {
        for bad in [
            "../evil.jar",
            "mods/../../evil.jar",
            "/abs.jar",
            "C:/evil.jar",
            "C:evil.jar",
            "\\\\server\\share\\evil.jar",
            "//server/share/evil.jar",
            "mods\\..\\..\\evil.jar",
            "mods//x.jar",
            "",
        ] {
            assert!(!safe_pack_path(bad), "{bad}");
        }
        for good in ["mods/a.jar", "config/sub/x.toml", "resourcepacks/r.zip"] {
            assert!(safe_pack_path(good), "{good}");
        }
    }

    #[test]
    fn mrpack_files_are_tracked_checked_and_keep_disabled() {
        let dir = temp("mr");
        let zip = dir.join("pack.mrpack");
        zip_with(
            &zip,
            &[
                ("modrinth.index.json", b"{}"),
                ("overrides/config/c.toml", b"x=1"),
            ],
        );
        let jobs = crate::jobs::Jobs::default();
        let job = jobs.start("t");
        let mut inst = Instance {
            name: "T".into(),
            root: dir.join("inst"),
            ..Default::default()
        };
        fs::create_dir_all(&inst.root).unwrap();

        // v1: two mods nobody identifies, plus paths that try to escape.
        let v1 = index(
            &[
                ("mods/a-1.jar", "a1"),
                ("mods/b.jar", "b"),
                ("../evil.jar", "evil"),
                ("C:/evil2.jar", "evil"),
                ("\\\\server\\share\\evil3.jar", "evil"),
            ],
            "",
        );
        apply(&mut inst, &v1, &zip, &job).unwrap();
        assert_eq!(mods(&inst), ["a-1.jar", "b.jar"]);
        assert!(!dir.join("evil.jar").exists());
        assert!(!inst.root.join(STAGING_DIR).exists());
        assert_eq!(inst.game_version, "1.21.1");
        // Unidentified files are pack entries now (finding 4).
        assert_eq!(inst.addons.len(), 2);
        assert!(inst.addons.iter().all(|a| a.from_pack && a.project_id == 0));
        assert_eq!(
            fs::read_to_string(inst.root.join("config/c.toml")).unwrap(),
            "x=1"
        );

        // Disable both, then update: a-1 becomes a-2 and b gets new bytes
        // under the same name. Both stay disabled (finding 3), the old
        // a-1 goes to backups and no b.jar appears beside b.jar.disabled.
        for i in 0..2 {
            crate::install::set_enabled(&mut inst, i, false).unwrap();
        }
        let v2 = index(&[("mods/a-2.jar", "a2"), ("mods/b.jar", "b2")], "");
        apply(&mut inst, &v2, &zip, &job).unwrap();
        assert_eq!(mods(&inst), ["a-2.jar.disabled", "b.jar.disabled"]);
        assert_eq!(fs::read(inst.mods_dir().join("b.jar.disabled")).unwrap(), b"b2");
        assert!(inst.addons.iter().all(|a| !a.enabled));
        assert_eq!(inst.addons.len(), 2);
        let backups = walk(&inst.root.join(crate::sync::BACKUP_DIR));
        assert!(backups.iter().any(|p| p.ends_with("a-1.jar.disabled")));

        // v3 drops b (an unidentified file): it is removed (finding 4).
        let v3 = index(&[("mods/a-2.jar", "a2")], "");
        apply(&mut inst, &v3, &zip, &job).unwrap();
        assert_eq!(mods(&inst), ["a-2.jar.disabled"]);
        assert_eq!(inst.addons.len(), 1);

        // v4 has a file whose bytes don't match its sha1: the update
        // fails and the instance is as it was (findings 1 and 6).
        let before = inst.addons.len();
        let v4 = index(&[("mods/a-3.jar", "a3"), ("mods/c.jar", "c")], "mods/c.jar");
        let err = apply(&mut inst, &v4, &zip, &job).unwrap_err();
        assert!(format!("{err:#}").contains("sha1"), "{err:#}");
        assert_eq!(mods(&inst), ["a-2.jar.disabled"]);
        assert_eq!(inst.addons.len(), before);
        assert!(!inst.root.join(STAGING_DIR).exists());

        // A file the server refuses is recorded as pending (finding 5).
        let v5 = index(&[("mods/a-2.jar", "a2"), ("mods/gone.jar", "404")], "");
        apply(&mut inst, &v5, &zip, &job).unwrap();
        let p = pending_downloads(&inst);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].file_name, "gone.jar");
        assert_eq!(p[0].folder, "mods");
        clear_pending(&inst).unwrap();
        assert!(pending_downloads(&inst).is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancelled_update_changes_nothing() {
        let dir = temp("cancel");
        let zip = dir.join("pack.mrpack");
        zip_with(&zip, &[("modrinth.index.json", b"{}")]);
        let jobs = crate::jobs::Jobs::default();
        let job = jobs.start("t");
        let mut inst = Instance {
            root: dir.join("inst"),
            ..Default::default()
        };
        fs::create_dir_all(&inst.root).unwrap();
        job.cancel();
        let err = apply(&mut inst, &index(&[("mods/a.jar", "a")], ""), &zip, &job).unwrap_err();
        assert_eq!(err.to_string(), "Cancelled");
        assert!(inst.addons.is_empty());
        assert!(!inst.mods_dir().exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_keeps_disabled_and_backs_up_the_disabled_file() {
        // The CurseForge path: same project, new file.
        let dir = temp("commit");
        let mut inst = Instance {
            root: dir.join("inst"),
            ..Default::default()
        };
        fs::create_dir_all(inst.mods_dir()).unwrap();
        let old = |id, name: &str| Addon {
            project_id: id,
            file_id: 1,
            file_name: name.into(),
            enabled: false,
            from_pack: true,
            locked: true,
            date_added: "2020".into(),
            ..Default::default()
        };
        inst.addons = vec![old(1, "x-1.jar"), old(2, "y.jar"), old(3, "gone.jar")];
        for f in ["x-1.jar.disabled", "y.jar.disabled", "gone.jar.disabled"] {
            fs::write(inst.mods_dir().join(f), "old").unwrap();
        }
        let staging = Staging::new(&inst.root).unwrap();
        let stage = |i, id, name: &str| {
            let tmp = staging.path(i, name);
            fs::create_dir_all(tmp.parent().unwrap()).unwrap();
            fs::write(&tmp, "new").unwrap();
            Staged {
                addon: Some(Addon {
                    project_id: id,
                    file_id: 2,
                    file_name: name.into(),
                    enabled: true,
                    from_pack: true,
                    ..Default::default()
                }),
                rel: format!("mods/{name}"),
                tmp,
            }
        };
        let staged = vec![stage(0, 1, "x-2.jar"), stage(1, 2, "y.jar")];
        commit(&mut inst, staged, |a| a.project_id == 3).unwrap();
        drop(staging);
        assert_eq!(mods(&inst), ["x-2.jar.disabled", "y.jar.disabled"]);
        assert_eq!(fs::read_to_string(inst.mods_dir().join("y.jar.disabled")).unwrap(), "new");
        assert_eq!(inst.addons.len(), 2);
        for a in &inst.addons {
            assert!(!a.enabled && a.locked && a.date_added == "2020", "{a:?}");
        }
        let backups = walk(&inst.root.join(crate::sync::BACKUP_DIR));
        assert!(backups.iter().any(|p| p.ends_with("x-1.jar.disabled")));
        assert!(backups.iter().any(|p| p.ends_with("gone.jar.disabled")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_import_leaves_no_folder() {
        let dir = temp("orphan");
        let instances = dir.join("instances");
        // A valid manifest with no files (no network), but overrides that
        // can't be written: "a" is a file and a folder at once.
        let zip = dir.join("broken.zip");
        zip_with(
            &zip,
            &[
                (
                    "manifest.json",
                    br#"{"minecraft":{"version":"1.21.1","modLoaders":[]},"name":"Broken","files":[],"overrides":"overrides"}"#,
                ),
                ("overrides/a", b"file"),
                ("overrides/a/b", b"under a file"),
            ],
        );
        let jobs = crate::jobs::Jobs::default();
        assert!(import_zip(&instances, &zip, &jobs.start("t")).is_err());
        assert_eq!(fs::read_dir(&instances).unwrap().count(), 0);

        // A good one stays.
        let ok = dir.join("ok.zip");
        zip_with(
            &ok,
            &[(
                "manifest.json",
                br#"{"minecraft":{"version":"1.21.1","modLoaders":[]},"name":"Fine","files":[]}"#,
            )],
        );
        let inst = import_zip(&instances, &ok, &jobs.start("t")).unwrap();
        assert!(inst.root.join("manifest.json").exists());

        // An existing folder is never claimed or removed.
        let taken = instances.join("Taken");
        fs::create_dir_all(&taken).unwrap();
        assert!(in_new_folder(&taken, || Ok(())).is_err());
        assert!(in_new_folder(&instances.join("New"), || -> anyhow::Result<()> {
            anyhow::bail!("boom")
        })
        .is_err());
        assert!(taken.exists() && !instances.join("New").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mrpack_cache_names_do_not_collide() {
        let file = |version: &str, name: &str| cf::File {
            file_name: name.into(),
            modrinth: Some(crate::modrinth::MrFile {
                version_id: version.into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let a = mrpack_cache_name(&file("AAA", "pack-1.0.mrpack"));
        let b = mrpack_cache_name(&file("BBB", "pack-1.0.mrpack"));
        assert_ne!(a, b);
        assert_eq!(a, mrpack_cache_name(&file("AAA", "other.mrpack")));
        // No Modrinth version: the sha1, then the url.
        let mut f = cf::File {
            file_name: "p.mrpack".into(),
            download_url: Some("https://x/1".into()),
            ..Default::default()
        };
        let by_url = mrpack_cache_name(&f);
        f.download_url = Some("https://x/2".into());
        assert_ne!(by_url, mrpack_cache_name(&f));
        f.hashes = vec![cf::FileHash {
            value: "abc".into(),
            algo: 1,
        }];
        assert_eq!(mrpack_cache_name(&f), "sha1-abc.mrpack");
    }

    #[test]
    fn prune_drops_old_then_least_recently_used() {
        use std::time::SystemTime;
        let dir = temp("prune");
        let now = SystemTime::now();
        let day = Duration::from_secs(86_400);
        for (name, age_days, len) in [("old.zip", 40u64, 10usize), ("mid.zip", 5, 10), ("new.zip", 1, 10)] {
            let p = dir.join(name);
            fs::write(&p, vec![0u8; len]).unwrap();
            fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(now - day * age_days as u32)
                .unwrap();
        }
        fs::write(dir.join("x.hopper-part"), "in progress").unwrap();
        // Age only.
        assert_eq!(prune_dir(&dir, u64::MAX, Some(day * 30), now).unwrap(), 10);
        assert!(!dir.join("old.zip").exists());
        // Size only: the least recently used goes first.
        assert_eq!(prune_dir(&dir, 10, None, now).unwrap(), 10);
        assert!(!dir.join("mid.zip").exists() && dir.join("new.zip").exists());
        assert!(dir.join("x.hopper-part").exists());
        assert_eq!(prune_dir(&dir.join("missing"), 0, None, now).unwrap(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    fn walk(d: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for e in fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
        out
    }
}
