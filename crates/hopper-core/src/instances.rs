//! Client instances.
//!
//! Hopper keeps its own `hopper.json` in each instance folder. A CurseForge
//! instance (`minecraftinstance.json` + `manifest.json`) is imported into it
//! on first sight and again whenever CurseForge rewrites its files, so both
//! apps can manage the same folder.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

pub const HOPPER_FILE: &str = "hopper.json";
/// Current `Instance::format`. 5: CurseForge's locks (`isLocked`) are read,
/// so instances imported before re-import once to pick them up. 6: a
/// re-import no longer takes a stale pack or loader from the CurseForge app
/// over Hopper's; re-importing once finds packs an older Hopper got wrong.
pub const FORMAT: u32 = 6;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Instance {
    pub name: String,
    #[serde(skip)]
    pub root: PathBuf,
    pub game_version: String,
    /// e.g. "neoforge-21.1.256".
    pub loader: String,
    pub pack: Option<PackRef>,
    pub addons: Vec<Addon>,
    pub settings: LaunchSettings,
    /// mtime of `minecraftinstance.json` when it was last imported.
    pub imported_from_cf: i64,
    /// Bumped when the import gains fields, to re-import CurseForge instances.
    pub format: u32,
    pub icon: Option<String>,
    /// Named sets of disabled mods to switch between.
    pub mod_profiles: Vec<ModProfile>,
    pub last_played: Option<String>,
    /// Total time played through Hopper's watch, in seconds.
    #[serde(default)]
    pub play_seconds: u64,
    /// The Minecraft settings Hopper last wrote into `options.txt`, by key
    /// (see [`crate::mcoptions::apply`]), so a "first start" value is given
    /// only once.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub applied_options: std::collections::BTreeMap<String, String>,
    /// The launcher whose folder this is ("prism"; see
    /// [`crate::launchers::Kind::key`]), or empty for Hopper/CurseForge.
    #[serde(default)]
    pub source: String,
    /// Set when `hopper.json` exists but can't be read or parsed: the
    /// instance is listed under its folder name with this error, and nothing
    /// is saved over the file (see [`set_aside_unreadable`]). Never saved.
    #[serde(default, skip)]
    pub unreadable: Option<String>,
    /// What the CurseForge app wrote over Hopper's pack version, loader or
    /// Minecraft version, while Hopper kept its own (the mod files agree
    /// with Hopper). Shown until the player picks one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cf_change: Option<CfChange>,
}

/// The CurseForge app's values when they disagree with Hopper's record (see
/// [`Instance::cf_change`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CfChange {
    /// The pack file the CurseForge app lists, when it differs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pack: Option<PackRef>,
    /// Its loader ("neoforge-21.1.256"), when it differs.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub loader: String,
    /// Its Minecraft version, when it differs.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub game_version: String,
    /// Hopper's own record held the CurseForge app's file id under the right
    /// version (an older Hopper took it over), so keeping Hopper's looks the
    /// file up again by its version.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub lost_file: bool,
}

/// A saved set of mods to keep disabled.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ModProfile {
    pub name: String,
    /// File names disabled in this profile; everything else is enabled.
    pub disabled: Vec<String>,
}

/// The modpack an instance was installed from.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PackRef {
    pub project_id: u64,
    pub file_id: u64,
    pub name: String,
    pub version: String,
    /// The pack's companion server pack, 0 when the author published none.
    pub server_pack_file_id: u64,
    pub file_name: String,
    /// Modrinth's ids for a Modrinth pack (`project_id` and `file_id` are
    /// hashes of them, which can't be turned back after a restart).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub mr_project: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub mr_version: String,
}

/// One installed project (mod, resource pack or shader pack).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Addon {
    pub project_id: u64,
    pub file_id: u64,
    /// Name on disk without `.disabled`.
    pub file_name: String,
    pub name: String,
    pub icon: String,
    pub page: String,
    pub author: String,
    pub enabled: bool,
    /// Part of the modpack (not added by the user).
    pub from_pack: bool,
    /// "client" | "server" | "both" | "" from CurseForge's file tags.
    pub env: String,
    pub class_id: u32,
    pub file_date: String,
    /// When it went into the instance (ISO 8601).
    pub date_added: String,
    /// Installed file's type: 1 release, 2 beta, 3 alpha (0 unknown).
    pub release_type: u32,
    /// Never offered as an update.
    pub locked: bool,
    /// One update's file locked out (the user keeps this version): still
    /// shown, but only installed on purpose. A different (newer) update
    /// isn't held by it. Empty when none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub locked_version: String,
    /// CurseForge's `isLocked` for this file when Hopper last read or wrote
    /// it. A re-import that finds another value takes CurseForge's change.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub cf_locked: bool,
    /// Projects this file requires.
    pub requires: Vec<u64>,
    /// Modrinth project, when the file came from Modrinth.
    pub mr_project: String,
    pub mr_version: String,
    pub mr_sha1: String,
    /// Where the file comes from when that's not CurseForge or Modrinth
    /// (a GitHub release, a Maven repo, a link): its updates come from there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<crate::sources::ModSource>,
}

impl Addon {
    pub fn folder(&self) -> &'static str {
        folder_for_class(self.class_id)
    }
}

pub fn folder_for_class(class_id: u32) -> &'static str {
    match class_id {
        crate::cf::CLASS_RESOURCE_PACKS => "resourcepacks",
        crate::cf::CLASS_SHADERS => "shaderpacks",
        _ => "mods",
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LaunchSettings {
    /// Max heap in MB. 0 uses the global default.
    pub memory_mb: u32,
    pub java_args: String,
    /// Empty uses the launcher's bundled Java.
    pub java_path: String,
    pub width: u32,
    pub height: u32,
    /// Least stable file type updates may offer: 1 release, 2 beta, 3 alpha.
    pub release_channel: u32,
    /// Apply the player's Minecraft settings (Settings > Minecraft) here.
    pub use_my_options: bool,
}

impl Default for LaunchSettings {
    fn default() -> Self {
        Self {
            memory_mb: 0,
            java_args: String::new(),
            java_path: String::new(),
            width: 1280,
            release_channel: 1,
            height: 720,
            use_my_options: true,
        }
    }
}

impl Instance {
    /// Names of enabled projects that require `project_id`, skipping `except`.
    pub fn required_by(&self, project_id: u64, except: &[String]) -> Vec<String> {
        self.addons
            .iter()
            .filter(|a| {
                a.enabled && a.requires.contains(&project_id) && !except.contains(&a.file_name)
            })
            .map(|a| a.name.clone())
            .collect()
    }

    /// Enabled addons that need any of `files`, directly or through each
    /// other (turning `files` off leaves these without what they need).
    pub fn dependents_of(&self, files: &[String]) -> Vec<&Addon> {
        let mut set: Vec<&Addon> = self.addons.iter().filter(|a| files.contains(&a.file_name)).collect();
        let mut out: Vec<&Addon> = Vec::new();
        loop {
            let ids: Vec<u64> = set.iter().map(|a| a.project_id).filter(|p| *p != 0).collect();
            let more: Vec<&Addon> = self
                .addons
                .iter()
                .filter(|a| a.enabled && !files.contains(&a.file_name))
                .filter(|a| !out.iter().any(|o| o.file_name == a.file_name))
                .filter(|a| a.requires.iter().any(|r| ids.contains(r)))
                .collect();
            if more.is_empty() {
                return out;
            }
            out.extend(more.iter().copied());
            set = more;
        }
    }

    /// Addons that only `files` needed: once those go, nothing left needs
    /// them (through chains too: a library whose only user was another such
    /// library). Never one that something else still needs.
    pub fn orphans_of(&self, files: &[String]) -> Vec<&Addon> {
        let mut gone: Vec<String> = files.to_vec();
        let mut out: Vec<&Addon> = Vec::new();
        loop {
            let needed_by_gone: Vec<u64> = self
                .addons
                .iter()
                .filter(|a| gone.contains(&a.file_name))
                .flat_map(|a| a.requires.iter().copied())
                .collect();
            let more: Vec<&Addon> = self
                .addons
                .iter()
                .filter(|a| a.project_id != 0 && !gone.contains(&a.file_name))
                .filter(|a| needed_by_gone.contains(&a.project_id))
                .filter(|a| {
                    // Nothing that stays needs it.
                    !self
                        .addons
                        .iter()
                        .filter(|o| !gone.contains(&o.file_name) && o.file_name != a.file_name)
                        .any(|o| o.requires.contains(&a.project_id))
                })
                .collect();
            if more.is_empty() {
                return out;
            }
            for a in more {
                gone.push(a.file_name.clone());
                out.push(a);
            }
        }
    }

    /// Disabled addons that `files` need, directly or through each other
    /// (turning `files` on without these stops the game at loading).
    pub fn missing_deps_of(&self, files: &[String]) -> Vec<&Addon> {
        let mut set: Vec<&Addon> = self.addons.iter().filter(|a| files.contains(&a.file_name)).collect();
        let mut out: Vec<&Addon> = Vec::new();
        loop {
            let need: Vec<u64> = set.iter().flat_map(|a| a.requires.iter().copied()).collect();
            let more: Vec<&Addon> = self
                .addons
                .iter()
                .filter(|a| !a.enabled && a.project_id != 0 && need.contains(&a.project_id))
                .filter(|a| !files.contains(&a.file_name))
                .filter(|a| !out.iter().any(|o| o.file_name == a.file_name))
                .collect();
            if more.is_empty() {
                return out;
            }
            out.extend(more.iter().copied());
            set = more;
        }
    }

    pub fn mods_dir(&self) -> PathBuf {
        self.root.join("mods")
    }

    pub fn addon_path(&self, a: &Addon) -> PathBuf {
        let name = if a.enabled {
            a.file_name.clone()
        } else {
            format!("{}.disabled", a.file_name)
        };
        self.root.join(a.folder()).join(name)
    }

    pub fn addon_by_file(&self, file_name: &str) -> Option<&Addon> {
        let f = file_name.trim_end_matches(".disabled");
        self.addons
            .iter()
            .find(|a| a.file_name.eq_ignore_ascii_case(f))
    }

    /// Writes `hopper.json` atomically (a unique temp file, then a rename),
    /// so concurrent saves never mix; the last one wins whole. To change an
    /// instance without overwriting what others saved meanwhile, use
    /// [`Instance::update_on_disk`] or [`Instance::save_merged`].
    ///
    /// Refuses for an [unreadable](Instance::unreadable) instance, which
    /// would otherwise replace the damaged file with an empty one.
    pub fn save(&self) -> anyhow::Result<()> {
        if let Some(e) = &self.unreadable {
            anyhow::bail!(
                "{} is unreadable ({e}); not overwriting it",
                self.root.join(HOPPER_FILE).display()
            );
        }
        write_atomic(
            &self.root.join(HOPPER_FILE),
            &serde_json::to_vec_pretty(self)?,
        )
    }

    /// Loads `root`'s instance fresh from disk, applies `f`, and saves it,
    /// all under a per-folder lock shared with every other `update_on_disk`
    /// and [`save_merged`](Instance::save_merged) in this process. The safe
    /// way for UI actions (toggle, lock, rename, settings) to change one
    /// thing without undoing a job's concurrent save. Returns the saved
    /// instance. Don't call `save`, `update_on_disk` or `save_merged` for the
    /// same folder inside `f` (the lock isn't reentrant).
    pub fn update_on_disk(root: &Path, f: impl FnOnce(&mut Instance)) -> anyhow::Result<Instance> {
        let lock = root_lock(root);
        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut inst =
            load(root).ok_or_else(|| anyhow::anyhow!("{} is not an instance", root.display()))?;
        if let Some(e) = &inst.unreadable {
            anyhow::bail!("{} is unreadable: {e}", root.join(HOPPER_FILE).display());
        }
        f(&mut inst);
        inst.save()?;
        Ok(inst)
    }

    /// Saves this copy (typically a job's, loaded before a long download)
    /// over what is on disk now, keeping what others changed meanwhile:
    ///
    /// - The addon list is this copy's, except that for addons whose file
    ///   name is not in `touched`, `enabled` and `locked` come from disk
    ///   (the user may have toggled or locked them while the job ran).
    /// - Addons on disk that this copy doesn't have and that aren't in
    ///   `touched` were added by someone else; they're kept if their file
    ///   (or its `.disabled` twin) exists.
    /// - Addons in `touched` are exactly as in this copy (removed if absent).
    /// - `play_seconds` and `last_played` are the larger of the two.
    /// - Everything else (name, settings, icon, profiles, pack) is this copy's.
    ///
    /// `touched` holds the file names (without `.disabled`) this writer
    /// added, removed, replaced or toggled. Runs under the same per-folder
    /// lock as [`update_on_disk`](Instance::update_on_disk). Returns what was
    /// saved.
    pub fn save_merged(&self, touched: &[String]) -> anyhow::Result<Instance> {
        let lock = root_lock(&self.root);
        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = self.clone();
        if let Some(disk) = read_own(&self.root).map_err(anyhow::Error::msg)? {
            merge_concurrent(&mut out, disk, touched);
        }
        out.save()?;
        Ok(out)
    }

    /// Folder-safe version of a display name.
    pub fn folder_name(name: &str) -> String {
        name.chars()
            .map(|c| {
                if r#"<>:"/\|?*"#.contains(c) || c.is_control() {
                    '_'
                } else {
                    c
                }
            })
            .collect::<String>()
            .trim()
            .trim_end_matches('.')
            .to_string()
    }
}

/// Every instance folder under `root`.
pub fn discover(root: &Path) -> Vec<Instance> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<Instance> = entries
        .flatten()
        .filter(|e| e.file_name() != crate::instance_ops::HOPPER_TRASH)
        .filter_map(|e| load(&e.path()))
        .collect();
    out.sort_by_key(|i| i.name.to_lowercase());
    out
}

/// `timePlayed` (ms) from a CurseForge minecraftinstance.json, without
/// parsing the whole (often multi-megabyte) file.
fn cf_time_played(path: &Path) -> Option<u64> {
    let text = fs::read_to_string(path).ok()?;
    let i = text.find("\"timePlayed\"")?;
    let rest = text[i + 12..].trim_start_matches([' ', ':', '\t']);
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// A file's modified time in seconds, as `imported_from_cf` records it.
pub fn file_time(p: &Path) -> i64 {
    mtime(p)
}

fn mtime(p: &Path) -> i64 {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

/// Writes `path` through a temp file unique to this process and call, then
/// renames it into place (retrying briefly: antivirus or an indexer may hold
/// the target for a moment).
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
    let tmp = path.with_file_name(format!(
        "{name}.{}-{}.tmp",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&tmp, bytes)?;
    let mut tries = 0;
    loop {
        match fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) if tries < 5 && e.kind() == std::io::ErrorKind::PermissionDenied => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(20 * tries));
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(e.into());
            }
        }
    }
}

/// The in-process lock for one instance folder.
fn root_lock(root: &Path) -> Arc<Mutex<()>> {
    static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> =
        LazyLock::new(Default::default);
    // Windows paths are case-insensitive; one lock per folder however spelled.
    let key = PathBuf::from(
        root.to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .replace('/', "\\")
            .to_lowercase(),
    );
    LOCKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(key)
        .or_default()
        .clone()
}

/// `hopper.json` as saved: `Ok(None)` when there is none, `Err` when it
/// exists but can't be read or parsed.
fn read_own(root: &Path) -> Result<Option<Instance>, String> {
    let p = root.join(HOPPER_FILE);
    match fs::read(&p) {
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(|e| format!("{HOPPER_FILE} is damaged: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{HOPPER_FILE} can't be read: {e}")),
    }
}

/// When each mod went into the instance (seconds since the epoch), by file
/// name without `.disabled`, from `hopper.json` as saved. Only reads.
pub fn added_dates(root: &Path) -> std::collections::HashMap<String, i64> {
    let Ok(Some(inst)) = read_own(root) else { return Default::default() };
    inst.addons
        .iter()
        .filter_map(|a| Some((a.file_name.clone(), crate::time::parse_iso(&a.date_added)?)))
        .collect()
}

/// Renames an unreadable `hopper.json` to `hopper.json.bad-<unix time>`, so
/// the next [`load`] starts over (re-importing from CurseForge when it can).
/// Only for when the user chooses to; Hopper never does it on its own.
/// Returns the new name.
pub fn set_aside_unreadable(root: &Path) -> anyhow::Result<PathBuf> {
    let p = root.join(HOPPER_FILE);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let bad = root.join(format!("{HOPPER_FILE}.bad-{ts}"));
    fs::rename(&p, &bad).map_err(|e| anyhow::anyhow!("renaming {}: {e}", p.display()))?;
    Ok(bad)
}

/// Whether an addon's file (or its `.disabled` twin) is on disk, and which.
fn file_state(root: &Path, a: &Addon) -> Option<bool> {
    let dir = root.join(a.folder());
    if dir.join(&a.file_name).exists() {
        Some(true)
    } else if dir.join(format!("{}.disabled", a.file_name)).exists() {
        Some(false)
    } else {
        None
    }
}

/// Drops the mod entries whose jar is gone, both as `x.jar` and as
/// `x.jar.disabled` (an old jar swapped for a new one outside the entry: a
/// sync, a server pack update, a file deleted by hand). Left in, each shows
/// as a mod that isn't there and as a second copy of the one that is. A jar
/// that comes back is identified again. Returns the dropped file names.
///
/// Does nothing when the mods folder can't be read or none of the entries'
/// jars are in it: a drive that's offline or a moved folder isn't a reason
/// to forget every mod.
pub fn drop_gone_mods(inst: &mut Instance) -> Vec<String> {
    let Ok(rd) = fs::read_dir(inst.mods_dir()) else {
        return Vec::new();
    };
    let on_disk: std::collections::HashSet<String> = rd
        .flatten()
        .map(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            n.strip_suffix(".disabled").map(str::to_string).unwrap_or(n)
        })
        .collect();
    let is_mod = |a: &Addon| a.class_id == crate::cf::CLASS_MODS || a.class_id == 0;
    let there = |a: &Addon| {
        let f = a.file_name.to_lowercase();
        on_disk.contains(f.strip_suffix(".disabled").unwrap_or(&f))
    };
    if !inst.addons.iter().any(|a| is_mod(a) && there(a)) {
        return Vec::new();
    }
    // A jar nothing tracks yet with the same name apart from its version
    // (mod-1.2.jar for a gone mod-1.1.jar) is likely the new copy: its old
    // entry waits for identify, which hands it over (when added, locks,
    // whether it's the pack's) instead of starting fresh.
    let tracked: std::collections::HashSet<String> = inst
        .addons
        .iter()
        .map(|a| a.file_name.to_lowercase().trim_end_matches(".disabled").to_string())
        .collect();
    let new_stems: std::collections::HashSet<String> = on_disk
        .iter()
        .filter(|n| n.ends_with(".jar") && !tracked.contains(*n))
        .map(|n| name_stem(n))
        .collect();
    let mut gone = Vec::new();
    inst.addons.retain(|a| {
        let keep = !is_mod(a) || there(a) || new_stems.contains(&name_stem(&a.file_name));
        if !keep {
            gone.push(a.file_name.clone());
        }
        keep
    });
    gone
}

/// A jar's name without its versions: its letters only, lowercased, with
/// ".jar" and ".disabled" off ("amendments-1.21-2.2.4-neoforge.jar" →
/// "amendmentsneoforge").
fn name_stem(file: &str) -> String {
    let l = file.to_lowercase();
    let l = l.trim_end_matches(".disabled").trim_end_matches(".jar");
    l.chars().filter(|c| c.is_alphabetic()).collect()
}

fn same_file(a: &Addon, b: &Addon) -> bool {
    a.file_name.eq_ignore_ascii_case(&b.file_name)
}

/// A CurseForge re-import replaced `imported`'s addons; bring back what
/// only Hopper knew from its previous `own` copy.
fn merge_reimport(imported: &mut Instance, own: Instance, root: &Path) {
    imported.settings = own.settings;
    imported.mod_profiles = own.mod_profiles;
    // An icon the user picked (anything not a web URL) outlives a re-import.
    if let Some(icon) = &own.icon
        && !icon.is_empty()
        && !icon.starts_with("http://")
        && !icon.starts_with("https://")
    {
        imported.icon = own.icon.clone();
    }
    imported.last_played = imported.last_played.take().max(own.last_played);
    // Hopper's own count, or CurseForge's if that is higher.
    imported.play_seconds = imported.play_seconds.max(own.play_seconds);

    for a in &mut imported.addons {
        if let Some(o) = own.addons.iter().find(|o| same_file(o, a)) {
            a.locked = merged_lock(a, o);
            if a.mr_project.is_empty() {
                a.mr_project = o.mr_project.clone();
                a.mr_version = o.mr_version.clone();
                a.mr_sha1 = o.mr_sha1.clone();
            }
            if !o.date_added.is_empty() {
                a.date_added = o.date_added.clone();
            }
            // CurseForge doesn't know the jar follows a source.
            if o.source.is_some() {
                a.source = o.source.clone();
                a.project_id = o.project_id;
                a.file_id = o.file_id;
                a.page = o.page.clone();
                a.mr_project.clear();
                a.mr_version.clear();
                a.mr_sha1.clear();
            }
        } else if a.project_id != 0
            && let Some(o) = own
                .addons
                .iter()
                .find(|o| o.project_id == a.project_id && o.class_id == a.class_id)
        {
            // Same project, new file (CurseForge updated it): keep the lock.
            a.locked = merged_lock(a, o);
        }
        // The files are the truth about enabled; CurseForge's record may lag.
        if let Some(on) = file_state(root, a) {
            a.enabled = on;
        }
    }
    // Mods Hopper added that CurseForge doesn't know, still on disk.
    for o in own.addons {
        if imported.addons.iter().any(|a| same_file(a, &o)) {
            continue;
        }
        if let Some(on) = file_state(root, &o) {
            imported.addons.push(Addon { enabled: on, ..o });
        }
    }
}

/// After a re-import, decides whose pack version, loader and Minecraft
/// version stand: `imported` holds the CurseForge app's, `own` Hopper's.
///
/// The CurseForge app can write back an old copy of its file (it keeps one
/// while it runs), which would undo a pack update or loader change Hopper
/// made. So Hopper's values stay unless the mods on disk fit the CurseForge
/// app's pack better than the one Hopper installed (`manifest.json`): that
/// is a real update done in the CurseForge app. When Hopper's stay, the
/// CurseForge app's go in [`Instance::cf_change`] for the player to pick.
fn settle_cf_change(
    imported: &mut Instance,
    own: &Instance,
    cf_manifest: Option<&PackManifest>,
    disk_manifest: Option<&PackManifest>,
    root: &Path,
) {
    imported.cf_change = None;
    let cf_version = cf_manifest.map(|m| m.version.as_str()).unwrap_or_default();
    let (cf_pack, own_pack) = match (&imported.pack, &own.pack) {
        (Some(c), Some(o)) if c.project_id == o.project_id => (Some(c), Some(o)),
        _ => (None, None),
    };
    let pack_moved = matches!((cf_pack, own_pack), (Some(c), Some(o)) if c.file_id != o.file_id);
    // An older Hopper took the CurseForge app's file id on a re-import but
    // kept the version from manifest.json: the two no longer belong together.
    let lost_file = matches!((cf_pack, own_pack), (Some(c), Some(o))
        if c.file_id == o.file_id && !cf_version.is_empty() && !o.version.is_empty() && o.version != cf_version);
    let loader_moved = !own.loader.is_empty() && !imported.loader.eq_ignore_ascii_case(&own.loader);
    let mc_moved = !own.game_version.is_empty() && imported.game_version != own.game_version;
    if !pack_moved && !lost_file && !loader_moved && !mc_moved {
        return;
    }
    if pack_moved || lost_file {
        let disk = disk_mods(imported, own, root);
        let fit = |m: &PackManifest| m.files.iter().filter(|f| disk.contains(&(f.project_id, f.file_id))).count();
        let cf_updated = match (cf_manifest, disk_manifest) {
            // The CurseForge app updated the pack and wrote its manifest too.
            (Some(c), Some(d)) if !c.version.is_empty() && c.version == d.version => true,
            (Some(c), Some(d)) => fit(c) > fit(d),
            _ => false,
        };
        if cf_updated {
            return;
        }
    }
    let mut change = CfChange { lost_file, ..Default::default() };
    if pack_moved || lost_file {
        change.pack = imported.pack.clone();
        imported.pack = own.pack.clone();
    }
    if loader_moved {
        change.loader = std::mem::replace(&mut imported.loader, own.loader.clone());
    }
    if mc_moved {
        change.game_version = std::mem::replace(&mut imported.game_version, own.game_version.clone());
    }
    log::warn!(
        "{}: the CurseForge app changed this instance's info (pack {:?}, loader {:?}, Minecraft {:?}); kept Hopper's",
        root.display(),
        change.pack.as_ref().map(|p| &p.version),
        change.loader,
        change.game_version
    );
    imported.cf_change = Some(change);
}

/// `(project, file)` of every mod either app lists whose file is on disk.
fn disk_mods(imported: &Instance, own: &Instance, root: &Path) -> std::collections::HashSet<(u64, u64)> {
    imported
        .addons
        .iter()
        .chain(&own.addons)
        .filter(|a| a.project_id != 0 && file_state(root, a).is_some())
        .map(|a| (a.project_id, a.file_id))
        .collect()
}

/// The lock after a re-import: CurseForge's when it changed since Hopper
/// last saw it, else Hopper's own.
fn merged_lock(imported: &Addon, own: &Addon) -> bool {
    if imported.cf_locked != own.cf_locked {
        imported.cf_locked
    } else {
        own.locked
    }
}

/// [`Instance::save_merged`]'s rule: `out` is the writer's copy, `disk` what
/// is saved now.
fn merge_concurrent(out: &mut Instance, disk: Instance, touched: &[String]) {
    let is_touched = |f: &str| {
        touched
            .iter()
            .any(|t| t.trim_end_matches(".disabled").eq_ignore_ascii_case(f))
    };
    for a in &mut out.addons {
        if is_touched(&a.file_name) {
            continue;
        }
        if let Some(d) = disk.addons.iter().find(|d| same_file(d, a)) {
            a.enabled = d.enabled;
            a.locked = d.locked;
        }
    }
    for d in disk.addons {
        if is_touched(&d.file_name) || out.addons.iter().any(|a| same_file(a, &d)) {
            continue;
        }
        if file_state(&out.root, &d).is_some() {
            out.addons.push(d);
        }
    }
    out.play_seconds = out.play_seconds.max(disk.play_seconds);
    out.last_played = out.last_played.take().max(disk.last_played);
}

/// The instance in `root`, importing (or re-importing) CurseForge's files
/// when they're newer than Hopper's. `None` when the folder isn't an
/// instance. A damaged `hopper.json` gives an instance with
/// [`Instance::unreadable`] set, named after the folder.
pub fn load(root: &Path) -> Option<Instance> {
    let cf_file = root.join("minecraftinstance.json");
    let cf_time = mtime(&cf_file);
    let own = match read_own(root) {
        Ok(own) => own,
        Err(e) => {
            log::warn!("{}: {e}", root.display());
            return Some(Instance {
                name: root.file_name()?.to_string_lossy().into_owned(),
                root: root.to_path_buf(),
                unreadable: Some(e),
                ..Default::default()
            });
        }
    };

    let mut inst = match own {
        Some(mut own)
            if cf_time == 0 || (own.imported_from_cf >= cf_time && own.format >= FORMAT) =>
        {
            // Seed play time once from CurseForge's counter.
            if own.play_seconds == 0
                && cf_time > 0
                && let Some(ms) = cf_time_played(&cf_file)
                && ms >= 60_000
            {
                own.play_seconds = ms / 1000;
                // The path isn't in the file; set it before saving.
                own.root = root.to_path_buf();
                let _ = own.save();
            }
            own
        }
        own => {
            // CurseForge changed the instance (or we never saw it): re-import,
            // keeping Hopper-only settings.
            let Imported { inst: mut imported, cf_manifest, disk_manifest } = import_curseforge(root)?;
            imported.imported_from_cf = cf_time;
            if let Some(own) = own {
                settle_cf_change(&mut imported, &own, cf_manifest.as_ref(), disk_manifest.as_ref(), root);
                merge_reimport(&mut imported, own, root);
            }
            imported.root = root.to_path_buf();
            if let Err(e) = imported.save() {
                log::warn!(
                    "could not write {}: {e:#}",
                    root.join(HOPPER_FILE).display()
                );
            }
            imported
        }
    };
    inst.root = root.to_path_buf();
    // Two entries for one file (the same pack installed from CurseForge and
    // from Modrinth): the newer one stays.
    let mut seen = std::collections::HashSet::new();
    let before = inst.addons.len();
    for i in (0..inst.addons.len()).rev() {
        let key = (inst.addons[i].folder(), inst.addons[i].file_name.to_lowercase());
        if !seen.insert(key) {
            inst.addons.remove(i);
        }
    }
    let gone = drop_gone_mods(&mut inst);
    if !gone.is_empty() {
        log::info!("{}: forgot {} mods whose jars are gone: {}", root.display(), gone.len(), gone.join(", "));
    }
    if inst.addons.len() != before {
        let _ = inst.save();
    }
    // Teach the id registry this instance's Modrinth projects and versions.
    for a in &inst.addons {
        crate::modrinth::num_id(&a.mr_project);
        crate::modrinth::num_id(&a.mr_version);
        if let Some(s) = &a.source {
            crate::sources::remember(a.project_id, s, &a.name);
        }
    }
    // Logos, authors and sides for mods no site describes, from their jars.
    crate::sources::looks::fill(&mut inst);
    if let Some(p) = &inst.pack {
        crate::modrinth::num_id(&p.mr_project);
        crate::modrinth::num_id(&p.mr_version);
    }
    if inst.name.is_empty() {
        inst.name = root.file_name()?.to_string_lossy().into_owned();
    }
    Some(inst)
}

/// An instance another launcher keeps, managed where it is: Hopper's own
/// file goes in its game folder, and the launcher's name, versions and
/// play time are taken again whenever its files change.
pub fn load_foreign(f: &crate::launchers::Foreign) -> Option<Instance> {
    let root = &f.game_dir;
    let meta_time = mtime(&f.meta_file);
    let own = match read_own(root) {
        Ok(own) => own,
        Err(e) => {
            return Some(Instance {
                name: f.name.clone(),
                root: root.clone(),
                unreadable: Some(e),
                ..Default::default()
            });
        }
    };
    let fresh = own.as_ref().is_some_and(|o| o.imported_from_cf >= meta_time && meta_time > 0);
    let mut inst = own.unwrap_or_default();
    if !fresh {
        inst.name = f.name.clone();
        if !f.game_version.is_empty() {
            inst.game_version = f.game_version.clone();
        }
        if !f.loader.is_empty() || !f.game_version.is_empty() {
            inst.loader = f.loader.clone();
        }
        // An icon the user picked in Hopper stays.
        if inst.icon.as_deref().is_none_or(|i| i.is_empty() || i.starts_with("http"))
            && let Some(i) = &f.icon
        {
            inst.icon = Some(i.to_string_lossy().into_owned());
        }
        if let Some(t) = f.last_played {
            inst.last_played = inst.last_played.take().max(Some(crate::time::iso_from_secs(t)));
        }
        inst.play_seconds = inst.play_seconds.max(f.play_seconds);
        if inst.pack.is_none()
            && let Some((project_id, file_id)) = f.cf_pack
        {
            inst.pack = Some(PackRef {
                project_id,
                file_id,
                name: f.name.clone(),
                ..Default::default()
            });
        }
        inst.imported_from_cf = meta_time;
        inst.format = FORMAT;
        inst.source = f.kind.map(|k| k.key().to_string()).unwrap_or_default();
        inst.root = root.clone();
        if let Err(e) = inst.save() {
            log::warn!("could not write {}: {e:#}", root.join(HOPPER_FILE).display());
        }
    }
    inst.root = root.clone();
    if inst.name.is_empty() {
        inst.name = f.name.clone();
    }
    Some(inst)
}

// ── CurseForge import ───────────────────────────────────────────────

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct CfInstance {
    name: String,
    game_version: String,
    base_mod_loader: Option<CfName>,
    installed_modpack: Option<CfAddon>,
    installed_addons: Vec<CfAddon>,
    /// The pack's manifest as the CurseForge app last installed it. Read
    /// loosely: a field it spells oddly mustn't stop the whole import.
    manifest: Option<serde_json::Value>,
    allocated_memory: u32,
    is_memory_override: bool,
    java_args_override: Option<String>,
    last_played: Option<String>,
    /// Milliseconds the CurseForge app has seen it played.
    time_played: u64,
    profile_image_path: Option<String>,
    preference_release_type: Option<u32>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CfName {
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct CfAddon {
    #[serde(rename = "addonID")]
    addon_id: u64,
    name: String,
    file_name_on_disk: String,
    #[serde(rename = "webSiteURL")]
    website_url: String,
    thumbnail_url: String,
    primary_author: Option<String>,
    is_enabled: Option<bool>,
    #[serde(rename = "categoryClassID")]
    category_class_id: u32,
    date_installed: String,
    is_locked: bool,
    installed_file: Option<CfFile>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct CfFile {
    id: u64,
    release_type: u32,
    file_name: String,
    file_date: String,
    server_pack_file_id: u64,
    sortable_game_version: Vec<CfGameVersion>,
    dependencies: Vec<CfDependency>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct CfDependency {
    addon_id: u64,
    /// 3 = required.
    #[serde(rename = "type")]
    kind: u32,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct CfGameVersion {
    game_version_name: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct PackManifest {
    pub name: String,
    pub version: String,
    pub author: String,
    #[serde(rename = "projectID")]
    pub project_id: u64,
    pub minecraft: ManifestMinecraft,
    pub files: Vec<ManifestFile>,
    pub overrides: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ManifestMinecraft {
    pub version: String,
    pub mod_loaders: Vec<ManifestLoader>,
    pub recommended_ram: u32,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct ManifestLoader {
    pub id: String,
    pub primary: bool,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct ManifestFile {
    #[serde(rename = "projectID")]
    pub project_id: u64,
    #[serde(rename = "fileID")]
    pub file_id: u64,
    pub required: bool,
}

impl PackManifest {
    pub fn loader(&self) -> String {
        self.minecraft
            .mod_loaders
            .iter()
            .find(|l| l.primary)
            .or(self.minecraft.mod_loaders.first())
            .map(|l| l.id.clone())
            .unwrap_or_default()
    }
}

/// "Client"/"Server" tags on a CurseForge file.
pub fn env_from_tags<'a>(tags: impl IntoIterator<Item = &'a str>) -> String {
    let (mut c, mut s) = (false, false);
    for t in tags {
        c |= t == "Client";
        s |= t == "Server";
    }
    match (c, s) {
        (true, true) => "both",
        (true, false) => "client",
        (false, true) => "server",
        _ => "",
    }
    .into()
}

/// What [`import_curseforge`] read.
struct Imported {
    inst: Instance,
    /// The manifest inside `minecraftinstance.json`: the pack as the
    /// CurseForge app last installed it.
    cf_manifest: Option<PackManifest>,
    /// `manifest.json`: the pack as last installed into the folder (by
    /// Hopper or the CurseForge app).
    disk_manifest: Option<PackManifest>,
}

fn import_curseforge(root: &Path) -> Option<Imported> {
    let mut raw: CfInstance =
        serde_json::from_slice(&fs::read(root.join("minecraftinstance.json")).ok()?).ok()?;
    let manifest: Option<PackManifest> = fs::read(root.join("manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let cf_manifest = raw
        .manifest
        .take()
        .and_then(|v| serde_json::from_value::<PackManifest>(v).ok())
        .filter(|m| !m.version.is_empty());
    let pack_ids: std::collections::HashSet<u64> = manifest
        .iter()
        .flat_map(|m| m.files.iter().map(|f| f.project_id))
        .collect();

    let pack = raw.installed_modpack.as_ref().map(|p| {
        let f = p.installed_file.as_ref();
        PackRef {
            project_id: manifest
                .as_ref()
                .map_or(0, |m| m.project_id)
                .max(p.addon_id),
            file_id: f.map_or(0, |f| f.id),
            name: p.name.clone(),
            // The version of the file the CurseForge app names, so the two
            // always belong together; manifest.json only when it names none.
            version: cf_manifest
                .as_ref()
                .or(manifest.as_ref())
                .map(|m| m.version.clone())
                .unwrap_or_default(),
            server_pack_file_id: f.map_or(0, |f| f.server_pack_file_id),
            file_name: f
                .map(|f| f.file_name.clone())
                .unwrap_or_else(|| p.file_name_on_disk.clone()),
            ..Default::default()
        }
    });

    let addons = raw
        .installed_addons
        .iter()
        .filter(|a| !a.file_name_on_disk.is_empty())
        .map(|a| {
            let f = a.installed_file.as_ref();
            Addon {
                project_id: a.addon_id,
                file_id: f.map_or(0, |f| f.id),
                file_name: a
                    .file_name_on_disk
                    .trim_end_matches(".disabled")
                    .to_string(),
                name: a.name.clone(),
                icon: a.thumbnail_url.clone(),
                page: a.website_url.clone(),
                author: a.primary_author.clone().unwrap_or_default(),
                enabled: a.is_enabled.unwrap_or(true)
                    && !a.file_name_on_disk.ends_with(".disabled"),
                from_pack: pack_ids.is_empty() || pack_ids.contains(&a.addon_id),
                env: env_from_tags(f.iter().flat_map(|f| {
                    f.sortable_game_version
                        .iter()
                        .map(|v| v.game_version_name.as_str())
                })),
                class_id: a.category_class_id,
                file_date: f.map(|f| f.file_date.clone()).unwrap_or_default(),
                date_added: a.date_installed.clone(),
                release_type: f.map_or(0, |f| f.release_type),
                locked: a.is_locked,
                cf_locked: a.is_locked,
                locked_version: String::new(),
                mr_project: String::new(),
                mr_version: String::new(),
                mr_sha1: String::new(),
                source: None,
                requires: f
                    .map(|f| {
                        f.dependencies
                            .iter()
                            .filter(|d| d.kind == 3)
                            .map(|d| d.addon_id)
                            .collect()
                    })
                    .unwrap_or_default(),
            }
        })
        .collect();

    let loader = raw
        .base_mod_loader
        .map(|l| l.name)
        .filter(|l| !l.is_empty())
        .or_else(|| manifest.as_ref().map(PackManifest::loader))
        .unwrap_or_default();

    let inst = Instance {
        mod_profiles: Vec::new(),
        name: raw.name,
        root: root.to_path_buf(),
        game_version: raw.game_version,
        loader,
        pack,
        addons,
        settings: LaunchSettings {
            memory_mb: if raw.is_memory_override {
                raw.allocated_memory
            } else {
                0
            },
            java_args: raw.java_args_override.unwrap_or_default(),
            release_channel: raw.preference_release_type.unwrap_or(1).clamp(1, 3),
            ..Default::default()
        },
        imported_from_cf: 0,
        source: String::new(),
        format: FORMAT,
        icon: raw
            .installed_modpack
            .as_ref()
            .map(|p| p.thumbnail_url.clone())
            .filter(|u| !u.is_empty())
            .or(raw.profile_image_path),
        last_played: raw.last_played,
        play_seconds: raw.time_played / 1000,
        applied_options: Default::default(),
        unreadable: None,
        cf_change: None,
    };
    Some(Imported { inst, cf_manifest, disk_manifest: manifest })
}

impl Instance {
    /// The header fields only, so drawing the header doesn't clone every addon.
    pub fn clone_header(&self) -> Instance {
        Instance {
            name: self.name.clone(),
            root: self.root.clone(),
            game_version: self.game_version.clone(),
            loader: self.loader.clone(),
            pack: self.pack.clone(),
            icon: self.icon.clone(),
            play_seconds: self.play_seconds,
            unreadable: self.unreadable.clone(),
            cf_change: self.cf_change.clone(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_libraries_only_a_removed_mod_needed() {
        let addon = |file: &str, id: u64, requires: &[u64]| Addon {
            file_name: file.into(),
            name: file.into(),
            project_id: id,
            enabled: true,
            requires: requires.to_vec(),
            ..Default::default()
        };
        let inst = Instance {
            addons: vec![
                addon("big", 1, &[10, 11]),
                addon("other", 2, &[11]),
                addon("lib-a", 10, &[12]),
                addon("lib-shared", 11, &[]),
                addon("lib-deep", 12, &[]),
            ],
            ..Default::default()
        };
        let names = |v: Vec<&Addon>| v.iter().map(|a| a.file_name.clone()).collect::<Vec<_>>();
        // lib-a and (through it) lib-deep go; lib-shared stays for "other".
        assert_eq!(names(inst.orphans_of(&["big".into()])), ["lib-a", "lib-deep"]);
        // Removing both users frees the shared one too.
        let mut all = names(inst.orphans_of(&["big".into(), "other".into()]));
        all.sort();
        assert_eq!(all, ["lib-a", "lib-deep", "lib-shared"]);
        assert!(inst.orphans_of(&["lib-deep".into()]).is_empty());
    }

    #[test]
    fn one_entry_per_file() {
        let root = std::env::temp_dir().join(format!("hopper-dupe-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("resourcepacks")).unwrap();
        fs::write(root.join("resourcepacks/Bare Bones 1.21.11.zip"), "x").unwrap();
        let pack = |id: u64| Addon {
            project_id: id,
            file_name: "Bare Bones 1.21.11.zip".into(),
            name: "Bare Bones".into(),
            enabled: true,
            class_id: crate::cf::CLASS_RESOURCE_PACKS,
            ..Default::default()
        };
        let inst = Instance { name: "t".into(), root: root.clone(), format: FORMAT, addons: vec![pack(1), pack(2)], ..Default::default() };
        inst.save().unwrap();
        let back = load(&root).unwrap();
        assert_eq!(back.addons.len(), 1);
        assert_eq!(back.addons[0].project_id, 2, "the newer entry stays");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn dependency_chains() {
        let addon = |file: &str, id: u64, on: bool, requires: &[u64]| Addon {
            file_name: file.into(),
            name: file.into(),
            project_id: id,
            enabled: on,
            requires: requires.to_vec(),
            ..Default::default()
        };
        let mut inst = Instance {
            addons: vec![
                addon("lib", 1, true, &[]),
                addon("mid", 2, true, &[1]),
                addon("top", 3, true, &[2]),
                addon("other", 4, true, &[]),
                addon("off", 5, false, &[1]),
            ],
            ..Default::default()
        };
        let names = |v: Vec<&Addon>| v.iter().map(|a| a.file_name.clone()).collect::<Vec<_>>();
        // Turning lib off strands mid, and top through mid; "off" is off already.
        assert_eq!(names(inst.dependents_of(&["lib".into()])), ["mid", "top"]);
        assert!(inst.dependents_of(&["other".into()]).is_empty());
        // Turning top on needs mid and lib on.
        for a in &mut inst.addons {
            a.enabled = a.file_name == "other";
        }
        assert_eq!(names(inst.missing_deps_of(&["top".into()])), ["mid", "lib"]);
        assert!(inst.missing_deps_of(&["lib".into()]).is_empty());
    }

    use super::*;

    #[test]
    fn env_tags() {
        assert_eq!(env_from_tags(["1.21.1", "NeoForge", "Client"]), "client");
        assert_eq!(env_from_tags(["Client", "Server"]), "both");
        assert_eq!(env_from_tags(["1.21.1"]), "");
    }

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-inst-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("mods")).unwrap();
        d
    }

    const CF_JSON: &str = r#"{
        "name": "Test Pack",
        "gameVersion": "1.21.1",
        "baseModLoader": { "name": "neoforge-21.1.1" },
        "installedModpack": { "addonID": 900, "name": "Test Pack",
            "thumbnailUrl": "https://media.forgecdn.net/pack.png" },
        "installedAddons": [
            { "addonID": 10, "name": "A", "fileNameOnDisk": "a.jar", "isEnabled": true,
              "categoryClassID": 6, "dateInstalled": "2024-05-05T00:00:00Z",
              "installedFile": { "id": 100, "fileName": "a.jar" } },
            { "addonID": 11, "name": "B", "fileNameOnDisk": "b.jar.disabled", "isEnabled": false,
              "categoryClassID": 6, "installedFile": { "id": 110, "fileName": "b.jar" } },
            { "addonID": 12, "name": "C", "fileNameOnDisk": "c-2.jar", "isEnabled": true,
              "categoryClassID": 6, "installedFile": { "id": 122, "fileName": "c-2.jar" } },
            { "addonID": 13, "name": "D", "fileNameOnDisk": "d.jar", "isEnabled": true,
              "categoryClassID": 6, "installedFile": { "id": 130, "fileName": "d.jar" } }
        ],
        "timePlayed": 0
    }"#;

    fn addon(file: &str, f: impl FnOnce(&mut Addon)) -> Addon {
        let mut a = Addon {
            file_name: file.into(),
            name: file.into(),
            enabled: true,
            class_id: 6,
            ..Default::default()
        };
        f(&mut a);
        a
    }

    #[test]
    fn reimport_keeps_hopper_data() {
        let root = temp("reimport");
        fs::write(root.join("minecraftinstance.json"), CF_JSON).unwrap();
        for f in [
            "a.jar",
            "b.jar.disabled",
            "c-2.jar",
            "d.jar.disabled",
            "hopper.jar.disabled",
        ] {
            fs::write(root.join("mods").join(f), "x").unwrap();
        }
        let own = Instance {
            name: "Test Pack".into(),
            format: FORMAT,
            // Older than minecraftinstance.json: CurseForge rewrote it.
            imported_from_cf: 0,
            icon: Some(r"C:\Icons\mine.png".into()),
            play_seconds: 3600,
            settings: LaunchSettings {
                memory_mb: 6144,
                ..Default::default()
            },
            addons: vec![
                addon("A.jar", |a| {
                    a.project_id = 10;
                    a.locked = true;
                    a.mr_project = "AAAA".into();
                    a.mr_version = "av1".into();
                    a.mr_sha1 = "abc".into();
                    a.date_added = "2020-01-01T00:00:00Z".into();
                }),
                addon("c-1.jar", |a| {
                    a.project_id = 12;
                    a.locked = true;
                }),
                addon("hopper.jar", |a| a.mr_project = "HHHH".into()),
                addon("gone.jar", |a| a.mr_project = "GONE".into()),
            ],
            ..Default::default()
        };
        fs::write(root.join(HOPPER_FILE), serde_json::to_vec(&own).unwrap()).unwrap();

        let inst = load(&root).unwrap();
        assert!(inst.unreadable.is_none());
        assert_eq!(inst.loader, "neoforge-21.1.1");
        let get = |f: &str| {
            inst.addon_by_file(f)
                .unwrap_or_else(|| panic!("{f} missing"))
        };
        let a = get("a.jar");
        assert!(a.locked);
        assert_eq!(
            (
                a.mr_project.as_str(),
                a.mr_version.as_str(),
                a.mr_sha1.as_str()
            ),
            ("AAAA", "av1", "abc")
        );
        assert_eq!(a.date_added, "2020-01-01T00:00:00Z");
        assert_eq!(a.file_id, 100);
        assert!(!get("b.jar").enabled);
        // Updated by CurseForge: the lock follows the project.
        assert!(get("c-2.jar").locked);
        assert!(inst.addon_by_file("c-1.jar").is_none());
        // CurseForge says enabled, the file says disabled: the file wins.
        assert!(!get("d.jar").enabled);
        // Hopper's own mod stays tracked, with its state from disk.
        let h = get("hopper.jar");
        assert_eq!(h.mr_project, "HHHH");
        assert!(!h.enabled);
        // Its file is gone, so it goes.
        assert!(inst.addon_by_file("gone.jar").is_none());
        assert_eq!(inst.icon.as_deref(), Some(r"C:\Icons\mine.png"));
        assert_eq!(inst.settings.memory_mb, 6144);
        assert_eq!(inst.play_seconds, 3600);

        // It was saved: loading again takes the fast path with the same data.
        let again = load(&root).unwrap();
        assert!(again.addon_by_file("hopper.jar").is_some());
        assert!(again.addon_by_file("a.jar").unwrap().locked);
        let _ = fs::remove_dir_all(&root);
    }

    // Trimmed, anonymised copies of a real pack's files after the CurseForge
    // app wrote back an old copy of its file over a pack update Hopper made.
    const FIX_CF_STALE: &str = include_str!("instances_fixtures/cf-app-stale.json");
    const FIX_CF_HOPPER: &str = include_str!("instances_fixtures/cf-hopper-wrote.json");
    const FIX_MANIFEST: &str = include_str!("instances_fixtures/manifest-1.78.1.json");
    const FIX_HOPPER: &str = include_str!("instances_fixtures/hopper.json");

    /// A pack folder from the fixtures: `cf` as CurseForge's file, the mods
    /// Hopper's record lists on disk, then `edit` on Hopper's record.
    fn pack_folder(tag: &str, cf: &str, manifest: &str, edit: impl FnOnce(&mut serde_json::Value)) -> PathBuf {
        let root = temp(tag);
        let mut own: serde_json::Value = serde_json::from_str(FIX_HOPPER).unwrap();
        for a in own["addons"].as_array().unwrap() {
            fs::write(root.join("mods").join(a["file_name"].as_str().unwrap()), "x").unwrap();
        }
        edit(&mut own);
        fs::write(root.join(HOPPER_FILE), own.to_string()).unwrap();
        fs::write(root.join("manifest.json"), manifest).unwrap();
        fs::write(root.join("minecraftinstance.json"), cf).unwrap();
        root
    }

    fn cf_with(base: &str, edit: impl FnOnce(&mut serde_json::Value)) -> String {
        let mut v: serde_json::Value = serde_json::from_str(base).unwrap();
        edit(&mut v);
        v.to_string()
    }

    #[test]
    fn stale_curseforge_copy_keeps_hoppers_pack_and_loader() {
        let root = pack_folder("cf-stale", FIX_CF_STALE, FIX_MANIFEST, |_| {});
        let inst = load(&root).unwrap();
        let pack = inst.pack.as_ref().unwrap();
        assert_eq!((pack.file_id, pack.version.as_str()), (9341561, "1.78.1"));
        assert_eq!(pack.file_name, "Limitless-8-1.78.1.zip");
        assert_eq!(inst.loader, "neoforge-21.1.259");
        let change = inst.cf_change.as_ref().expect("the change is shown");
        let cf_pack = change.pack.as_ref().unwrap();
        assert_eq!((cf_pack.file_id, cf_pack.version.as_str()), (8816233, "1.71.1"));
        assert_eq!(change.loader, "neoforge-21.1.256");
        assert!(!change.lost_file);
        // Saved: the next start still knows both.
        let again = load(&root).unwrap();
        assert_eq!(again.pack.as_ref().unwrap().file_id, 9341561);
        assert_eq!(again.cf_change, inst.cf_change);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn record_an_older_hopper_mixed_up_is_flagged() {
        // What 0.30.2 saved: the CurseForge app's file id under the version
        // from manifest.json, and the loader the player set again.
        let cf = cf_with(FIX_CF_STALE, |v| v["baseModLoader"]["name"] = "neoforge-21.1.265".into());
        let root = pack_folder("cf-lost", &cf, FIX_MANIFEST, |own| {
            own["pack"]["file_id"] = 8816233.into();
            own["pack"]["file_name"] = "Limitless-8-1.71.1.zip".into();
            own["loader"] = "neoforge-21.1.265".into();
            own["format"] = 5.into();
        });
        let inst = load(&root).unwrap();
        assert_eq!(inst.pack.as_ref().unwrap().version, "1.78.1");
        assert_eq!(inst.loader, "neoforge-21.1.265");
        let change = inst.cf_change.as_ref().unwrap();
        assert!(change.lost_file);
        assert_eq!(change.pack.as_ref().unwrap().version, "1.71.1");
        assert!(change.loader.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pack_updated_in_the_curseforge_app_is_taken() {
        // Hopper last saw 1.71.1; the CurseForge app updated to 1.78.1 and
        // the mods on disk are 1.78.1's.
        let old: serde_json::Value = serde_json::from_str(FIX_CF_STALE).unwrap();
        let old_manifest = old["manifest"].to_string();
        let downgrade = |own: &mut serde_json::Value| {
            own["pack"]["file_id"] = 8816233.into();
            own["pack"]["version"] = "1.71.1".into();
            own["loader"] = "neoforge-21.1.248".into();
        };
        for (tag, manifest) in [("cf-upd-wrote", FIX_MANIFEST), ("cf-upd-mods", old_manifest.as_str())] {
            let root = pack_folder(tag, FIX_CF_HOPPER, manifest, downgrade);
            let inst = load(&root).unwrap();
            let pack = inst.pack.as_ref().unwrap();
            assert_eq!((pack.file_id, pack.version.as_str()), (9341561, "1.78.1"), "{tag}");
            assert_eq!(inst.loader, "neoforge-21.1.259", "{tag}");
            assert!(inst.cf_change.is_none(), "{tag}");
            let _ = fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn loader_changed_by_the_curseforge_app_is_never_taken_silently() {
        let cf = cf_with(FIX_CF_HOPPER, |v| v["baseModLoader"]["name"] = "neoforge-21.1.248".into());
        let root = pack_folder("cf-loader", &cf, FIX_MANIFEST, |_| {});
        let inst = load(&root).unwrap();
        assert_eq!(inst.loader, "neoforge-21.1.259");
        assert_eq!(inst.pack.as_ref().unwrap().file_id, 9341561);
        let change = inst.cf_change.as_ref().unwrap();
        assert_eq!(change.loader, "neoforge-21.1.248");
        assert!(change.pack.is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn matching_curseforge_file_shows_no_change() {
        let root = pack_folder("cf-same", FIX_CF_HOPPER, FIX_MANIFEST, |_| {});
        let inst = load(&root).unwrap();
        assert!(inst.cf_change.is_none());
        assert_eq!(inst.pack.as_ref().unwrap().version, "1.78.1");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reimport_takes_curseforge_icon_over_a_web_icon() {
        let root = temp("reimport-icon");
        fs::write(root.join("minecraftinstance.json"), CF_JSON).unwrap();
        let own = Instance {
            icon: Some("https://old.example/icon.png".into()),
            format: FORMAT,
            ..Default::default()
        };
        fs::write(root.join(HOPPER_FILE), serde_json::to_vec(&own).unwrap()).unwrap();
        let inst = load(&root).unwrap();
        assert_eq!(
            inst.icon.as_deref(),
            Some("https://media.forgecdn.net/pack.png")
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn damaged_hopper_json_is_listed_and_never_overwritten() {
        let parent = temp("broken");
        let root = parent.join("Broken Pack");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("minecraftinstance.json"), CF_JSON).unwrap();
        fs::write(root.join(HOPPER_FILE), b"{\"name\": \"Broken").unwrap();
        let inst = load(&root).unwrap();
        assert!(inst.unreadable.as_deref().unwrap().contains("damaged"));
        assert_eq!(inst.name, root.file_name().unwrap().to_string_lossy());
        assert!(inst.save().is_err());
        assert!(Instance::update_on_disk(&root, |i| i.play_seconds = 1).is_err());
        assert!(inst.save_merged(&[]).is_err());
        assert_eq!(
            fs::read(root.join(HOPPER_FILE)).unwrap(),
            b"{\"name\": \"Broken"
        );
        // Shows up in discovery too.
        assert!(
            discover(&parent)
                .iter()
                .any(|i| i.root == root && i.unreadable.is_some())
        );

        // The user chooses to start over: the file is kept aside, CurseForge re-imported.
        let bad = set_aside_unreadable(&root).unwrap();
        assert_eq!(fs::read(&bad).unwrap(), b"{\"name\": \"Broken");
        let inst = load(&root).unwrap();
        assert!(inst.unreadable.is_none());
        assert_eq!(inst.name, "Test Pack");
        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn concurrent_saves_dont_collide_or_lose_updates() {
        let root = temp("concurrent");
        let inst = Instance {
            name: "C".into(),
            root: root.clone(),
            format: FORMAT,
            ..Default::default()
        };
        inst.save().unwrap();
        // Plain saves from many threads: each one lands whole.
        std::thread::scope(|s| {
            for n in 0..8 {
                let mut i = inst.clone();
                s.spawn(move || {
                    for k in 0..10 {
                        i.play_seconds = n * 100 + k;
                        i.save().unwrap();
                    }
                });
            }
        });
        assert!(load(&root).is_some());
        // Read-modify-write through update_on_disk: no increment is lost.
        Instance::update_on_disk(&root, |i| i.play_seconds = 0).unwrap();
        std::thread::scope(|s| {
            for _ in 0..8 {
                let r = root.clone();
                s.spawn(move || {
                    for _ in 0..10 {
                        Instance::update_on_disk(&r, |i| i.play_seconds += 1).unwrap();
                    }
                });
            }
        });
        assert_eq!(load(&root).unwrap().play_seconds, 80);
        // No temp files left behind.
        let leftovers: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn save_merged_keeps_others_changes() {
        let root = temp("merged");
        for f in ["a.jar", "b.jar", "c.jar", "ui-added.jar"] {
            fs::write(root.join("mods").join(f), "x").unwrap();
        }
        let base = Instance {
            name: "M".into(),
            root: root.clone(),
            format: FORMAT,
            play_seconds: 10,
            addons: vec![
                addon("a.jar", |_| {}),
                addon("b.jar", |_| {}),
                addon("old.jar", |_| {}),
            ],
            ..Default::default()
        };
        base.save().unwrap();

        // A job starts from `base`: it replaces old.jar with c.jar.
        let mut job = base.clone();
        job.addons.retain(|a| a.file_name != "old.jar");
        job.addons.push(addon("c.jar", |_| {}));

        // Meanwhile the user disables b, locks a, adds a mod, and plays.
        Instance::update_on_disk(&root, |i| {
            i.addons[1].enabled = false;
            i.addons[0].locked = true;
            i.addons.push(addon("ui-added.jar", |_| {}));
            i.play_seconds = 500;
        })
        .unwrap();
        fs::rename(root.join("mods/b.jar"), root.join("mods/b.jar.disabled")).unwrap();

        let saved = job
            .save_merged(&["old.jar".into(), "c.jar".into()])
            .unwrap();
        let disk = load(&root).unwrap();
        for i in [&saved, &disk] {
            assert!(i.addon_by_file("a.jar").unwrap().locked);
            assert!(!i.addon_by_file("b.jar").unwrap().enabled);
            assert!(i.addon_by_file("c.jar").is_some());
            assert!(i.addon_by_file("old.jar").is_none());
            assert!(i.addon_by_file("ui-added.jar").is_some());
            assert_eq!(i.play_seconds, 500);
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn folder_names_are_safe() {
        assert_eq!(
            Instance::folder_name("All the Mods: 10?"),
            "All the Mods_ 10_"
        );
    }

    #[test]
    fn curseforge_lock_is_read() {
        let root = temp("cflock");
        let json = CF_JSON.replace(r#""fileNameOnDisk": "d.jar","#, r#""fileNameOnDisk": "d.jar", "isLocked": true,"#);
        fs::write(root.join("minecraftinstance.json"), json).unwrap();
        for f in ["a.jar", "b.jar.disabled", "c-2.jar", "d.jar"] {
            fs::write(root.join("mods").join(f), "x").unwrap();
        }
        let inst = load(&root).unwrap();
        let d = inst.addon_by_file("d.jar").unwrap();
        assert!(d.locked && d.cf_locked);
        assert!(!inst.addon_by_file("a.jar").unwrap().locked);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reimport_takes_curseforge_lock_only_when_it_changed() {
        let a = |locked, cf_locked| Addon { locked, cf_locked, ..Default::default() };
        // CurseForge locked it since Hopper last looked.
        assert!(merged_lock(&a(true, true), &a(false, false)));
        // CurseForge unlocked it.
        assert!(!merged_lock(&a(false, false), &a(true, true)));
        // CurseForge didn't change it: Hopper's lock (written while
        // CurseForge was open) stays.
        assert!(merged_lock(&a(false, false), &a(true, false)));
        assert!(!merged_lock(&a(true, true), &a(false, true)));
    }

    #[test]
    fn older_format_reimports_to_read_curseforge_locks() {
        let root = temp("cflock-old");
        let json = CF_JSON.replace(r#""fileNameOnDisk": "d.jar","#, r#""fileNameOnDisk": "d.jar", "isLocked": true,"#);
        fs::write(root.join("minecraftinstance.json"), json).unwrap();
        for f in ["a.jar", "b.jar.disabled", "c-2.jar", "d.jar"] {
            fs::write(root.join("mods").join(f), "x").unwrap();
        }
        // Saved by a Hopper that imported this file but didn't read locks.
        let own = Instance {
            name: "Test Pack".into(),
            format: 4,
            imported_from_cf: file_time(&root.join("minecraftinstance.json")),
            addons: vec![addon("d.jar", |a| a.project_id = 13)],
            ..Default::default()
        };
        fs::write(root.join(HOPPER_FILE), serde_json::to_vec(&own).unwrap()).unwrap();
        let inst = load(&root).unwrap();
        assert!(inst.addon_by_file("d.jar").unwrap().locked);
        assert_eq!(inst.format, FORMAT);
        let _ = fs::remove_dir_all(&root);
    }

    /// A server's mod list after jars were swapped outside its entries
    /// (as on TNP-LL8: a mod sync and identify runs left old entries).
    #[test]
    fn forgets_mods_whose_jars_are_gone() {
        let root = std::env::temp_dir().join(format!("hopper-ghosts-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mods = root.join("mods");
        fs::create_dir_all(&mods).unwrap();
        for f in ["amendments-1.21-2.2.4-neoforge.jar", "jei.jar.disabled", "rftoolsbuilder-1.21-7.0.8.jar"] {
            fs::write(mods.join(f), b"x").unwrap();
        }
        let mk = |file: &str, id: u64, f: fn(&mut Addon)| {
            let mut a = Addon { file_name: file.into(), name: file.into(), project_id: id, enabled: true, class_id: crate::cf::CLASS_MODS, ..Default::default() };
            f(&mut a);
            a
        };
        let mut inst = Instance {
            root: root.clone(),
            addons: vec![
                // Two old copies (from Modrinth, then CurseForge) of the one on disk.
                mk("amendments-1.21-2.2.0-neoforge.jar", 1 << 63, |_| {}),
                mk("amendments-1.21-2.2.2-neoforge.jar", 896746, |_| {}),
                mk("amendments-1.21-2.2.4-neoforge.jar", 1 << 63, |_| {}),
                // Off: its jar is "jei.jar.disabled".
                mk("jei.jar", 3, |a| a.enabled = false),
                // Gone, but an untracked jar of the same name is there: kept for identify.
                mk("rftoolsbuilder-1.21-7.0.7.jar", 4, |a| a.locked = true),
                // Gone for good.
                mk("MultiBuilderTool-1.21.1-1.1.29.jar", 5, |_| {}),
                // Not a mod: left alone.
                mk("pack.zip", 6, |a| a.class_id = crate::cf::CLASS_RESOURCE_PACKS),
            ],
            ..Default::default()
        };
        let gone = super::drop_gone_mods(&mut inst);
        assert_eq!(gone, vec!["amendments-1.21-2.2.0-neoforge.jar", "amendments-1.21-2.2.2-neoforge.jar", "MultiBuilderTool-1.21.1-1.1.29.jar"]);
        let left: Vec<&str> = inst.addons.iter().map(|a| a.file_name.as_str()).collect();
        assert_eq!(left, vec!["amendments-1.21-2.2.4-neoforge.jar", "jei.jar", "rftoolsbuilder-1.21-7.0.7.jar", "pack.zip"]);
        assert!(super::drop_gone_mods(&mut inst).is_empty());

        // Saved and loaded: load forgets them too.
        inst.addons.push(mk("old-1.0.jar", 7, |_| {}));
        inst.save().unwrap();
        let loaded = super::load(&root).unwrap();
        assert!(loaded.addon_by_file("old-1.0.jar").is_none());
        assert!(loaded.addon_by_file("jei.jar").is_some());

        // The mods folder unreadable (a drive offline): nothing is forgotten.
        fs::remove_dir_all(&mods).unwrap();
        let mut offline = loaded.clone();
        assert!(super::drop_gone_mods(&mut offline).is_empty());
        assert_eq!(offline.addons.len(), loaded.addons.len());
        // There but holding none of the jars (a moved folder): the same.
        fs::create_dir_all(&mods).unwrap();
        fs::write(mods.join("other.jar"), b"x").unwrap();
        assert!(super::drop_gone_mods(&mut offline).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn name_stems_ignore_versions() {
        assert_eq!(super::name_stem("amendments-1.21-2.2.4-neoforge.jar"), "amendmentsneoforge");
        assert_eq!(super::name_stem("rftoolsbuilder-1.21-7.0.7.jar.disabled"), super::name_stem("rftoolsbuilder-1.21-7.0.8.jar"));
        assert_ne!(super::name_stem("rftoolsbuilder-1.21-7.0.7.jar"), super::name_stem("rftoolsbase-1.21-7.0.7.jar"));
    }
}

/// Loads a copy of a real server's folder (its hopper.json and mods/, the
/// jars may be empty files) and prints what load forgets. Point
/// HOPPER_GHOST_SAMPLE at the copy; never at a real server.
#[cfg(test)]
#[test]
#[ignore]
fn ghost_sample() {
    let Ok(dir) = std::env::var("HOPPER_GHOST_SAMPLE") else { return };
    let root = Path::new(&dir);
    let before: Instance = serde_json::from_slice(&fs::read(root.join(HOPPER_FILE)).unwrap()).unwrap();
    let inst = load(root).unwrap();
    let mods = |i: &Instance| i.addons.iter().filter(|a| a.folder() == "mods").count();
    println!("mod entries: {} -> {}", mods(&before), mods(&inst));
    println!("jars: {}", crate::install::loose_jars(&inst).len());
}
