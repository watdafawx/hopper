//! A hosted server's worlds: the folders with a `level.dat` (or a world
//! made but not played yet), which one `level-name` makes active, new
//! worlds, resets with a kept or new seed, and pre-generation with Chunky
//! (installing it, reading its progress from the console).

use super::backup::{self, Backup, Kind, Scope};
use super::process::LocalServer;
use super::properties::Properties;
use crate::jobs::Job;
use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Worlds made in the panel but not played yet, with the seed each gets.
const META: &str = ".hopper-worlds.json";
/// Where a reset parks the Nether and End while the world folder goes.
const KEEP: &str = ".hopper-worlds-keep";

#[derive(Clone, Debug, Serialize)]
pub struct World {
    /// The folder name, as `level-name` holds it.
    pub name: String,
    /// Its folders: the world, plus Bukkit-style `_nether` / `_the_end`.
    pub folders: Vec<String>,
    /// Bytes, all folders together.
    pub size: u64,
    /// Unix seconds; 0 when never played.
    pub last_played: u64,
    /// From `level.dat`, or the seed it will get (made, not played).
    pub seed: Option<String>,
    pub active: bool,
    /// Has a `level.dat` (false: made here, created on the next start).
    pub created: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Meta {
    /// World name → seed ("" = random) for worlds not created yet.
    pending: BTreeMap<String, String>,
    /// Last pre-generation started here: world, radius, unix seconds.
    pregen: Option<PregenStart>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PregenStart {
    pub world: String,
    pub radius: u32,
    pub at: u64,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn load_meta(root: &Path) -> Meta {
    fs::read(root.join(META)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_meta(root: &Path, m: &Meta) -> anyhow::Result<()> {
    fs::write(root.join(META), serde_json::to_vec_pretty(m)?)?;
    Ok(())
}

fn props(root: &Path) -> anyhow::Result<Properties> {
    Properties::load(&root.join("server.properties"))
}

/// The active world's folder name (`level-name`, "world" when unset).
pub fn level_name(root: &Path) -> String {
    props(root)
        .ok()
        .and_then(|p| p.get("level-name").map(str::to_string))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "world".into())
}

/// A world name that is one plain folder in the server folder.
pub fn check_name(name: &str) -> anyhow::Result<()> {
    let bad = name.is_empty()
        || name.len() > 64
        || name.starts_with('.')
        || name.ends_with('.')
        || name.ends_with(' ')
        || name.chars().any(|c| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control());
    if bad {
        bail!("A world name is one folder name: letters, numbers, - and _ (no slashes, no leading dot).");
    }
    let reserved = ["mods", "plugins", "config", "logs", "libraries", "crash-reports", "versions", "defaultconfigs", "kubejs"];
    if reserved.contains(&name.to_ascii_lowercase().as_str()) {
        bail!("\"{name}\" is one of the server's own folders.");
    }
    Ok(())
}

fn mtime(p: &Path) -> u64 {
    fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())
}

/// Bytes in a world folder. Each file's own metadata, not the directory
/// listing's: Windows only updates a listed size when the file is closed,
/// so a running server's region files would read as 0.
fn world_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                stack.push(e.path());
            } else if t.is_file() {
                total += fs::metadata(e.path()).map_or(0, |m| m.len());
            }
        }
    }
    total
}

/// The world's folders that exist: itself and Bukkit-style dimension folders.
pub fn folders(root: &Path, name: &str) -> Vec<String> {
    [name.to_string(), format!("{name}_nether"), format!("{name}_the_end")]
        .into_iter()
        .filter(|f| root.join(f).is_dir())
        .collect()
}

/// The server's worlds, active first, then by last played.
pub fn list(root: &Path) -> Vec<World> {
    let active = level_name(root);
    let meta = load_meta(root);
    let mut names: Vec<String> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.') && root.join(n).join("level.dat").is_file())
        .collect();
    // Bukkit keeps "world_nether" beside "world", each with a level.dat.
    let all = names.clone();
    names.retain(|n| {
        let base = n.strip_suffix("_nether").or_else(|| n.strip_suffix("_the_end"));
        !base.is_some_and(|b| all.iter().any(|x| x == b))
    });
    for n in meta.pending.keys().chain(std::iter::once(&active)) {
        if !names.contains(n) {
            names.push(n.clone());
        }
    }
    let mut out: Vec<World> = names
        .into_iter()
        .map(|name| {
            let dir = root.join(&name);
            let created = dir.join("level.dat").is_file();
            let fs_ = folders(root, &name);
            let size = fs_.iter().map(|f| world_size(&root.join(f))).sum();
            let info = if created { read_level(&dir.join("level.dat")) } else { LevelInfo::default() };
            let seed = if created {
                info.seed.map(|s| s.to_string())
            } else {
                meta.pending.get(&name).cloned().or_else(|| {
                    // The active one not made yet takes level-seed.
                    (name == active).then(|| props(root).ok().and_then(|p| p.get("level-seed").map(str::to_string))).flatten()
                })
            }
            .filter(|s| !s.is_empty());
            let last_played = if created { info.last_played.map_or_else(|| mtime(&dir.join("level.dat")), |ms| (ms / 1000).max(0) as u64) } else { 0 };
            World { active: name == active, name, folders: fs_, size, last_played, seed, created }
        })
        .collect();
    out.sort_by_key(|w| (!w.active, std::cmp::Reverse(w.last_played)));
    out
}

/// Makes `name` the world the server loads next start. A world made here
/// and not played yet brings its seed along. Needs the server stopped.
pub fn set_active(root: &Path, name: &str) -> anyhow::Result<()> {
    check_name(name)?;
    let mut meta = load_meta(root);
    let created = root.join(name).join("level.dat").is_file();
    if !created && !meta.pending.contains_key(name) {
        bail!("There's no world \"{name}\".");
    }
    let path = root.join("server.properties");
    let mut p = Properties::load(&path)?;
    p.set("level-name", name);
    if let Some(seed) = meta.pending.get(name) {
        p.set("level-seed", seed.clone());
    }
    p.save(&path)?;
    if created {
        meta.pending.remove(name);
        save_meta(root, &meta)?;
    }
    Ok(())
}

/// Adds a world made on the next start of the server with it active.
/// `seed` empty = random.
pub fn create(root: &Path, name: &str, seed: &str, activate: bool) -> anyhow::Result<()> {
    check_name(name)?;
    if root.join(name).exists() {
        bail!("There's already a folder called \"{name}\".");
    }
    let seed = seed.trim();
    if seed.len() > 64 {
        bail!("That seed is too long.");
    }
    let mut meta = load_meta(root);
    meta.pending.insert(name.to_string(), seed.to_string());
    save_meta(root, &meta)?;
    if activate {
        set_active(root, name)?;
    }
    Ok(())
}

/// Backs up one world (all its folders) before something risky. The
/// active one goes through the normal worlds backup (saving paused while
/// `server` runs); another is backed up by pointing `level-name` at it for
/// the moment it takes to list its files.
pub fn backup_world(root: &Path, name: &str, kind: Kind, note: &str, server: Option<&LocalServer>, job: &Job) -> anyhow::Result<Backup> {
    let active = level_name(root);
    if name == active {
        return backup::make(root, Scope::Worlds, kind, note, server, job);
    }
    if !root.join(name).join("level.dat").is_file() {
        bail!("\"{name}\" hasn't been played yet; there's nothing to back up.");
    }
    // Put server.properties back exactly as it was, whatever happens.
    struct Restore(PathBuf, Vec<u8>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = fs::write(&self.0, &self.1);
        }
    }
    let path = root.join("server.properties");
    let _back = Restore(path.clone(), fs::read(&path).unwrap_or_default());
    let mut p = Properties::load(&path)?;
    p.set("level-name", name);
    p.save(&path)?;
    backup::make(root, Scope::Worlds, kind, note, None, job)
}

/// What a reset does with the seed.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "mode", content = "seed", rename_all = "snake_case")]
pub enum SeedChoice {
    /// The world's own seed: the same terrain again.
    Same,
    /// A new random one.
    New,
    /// This one (a number or any text, as Minecraft takes it).
    Custom(String),
}

fn random_seed() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    (h.finish() as i64).to_string()
}

/// Resets a world: its folders go through `trash` (rel paths, so they can
/// come back from the panel's trash), and the next start makes it again
/// with the chosen seed. `keep_dims` keeps the Nether and End. Stop the
/// server first (a backup, [`backup_world`], only when the person asks for
/// one); returns the seed.
pub fn reset(root: &Path, name: &str, seed: &SeedChoice, keep_dims: bool, trash: &dyn Fn(&str) -> anyhow::Result<()>) -> anyhow::Result<String> {
    check_name(name)?;
    let dir = root.join(name);
    let seed = match seed {
        SeedChoice::Same => read_level(&dir.join("level.dat"))
            .seed
            .map(|s| s.to_string())
            .or_else(|| props(root).ok().and_then(|p| p.get("level-seed").map(str::to_string)))
            .unwrap_or_default(),
        SeedChoice::New => random_seed(),
        SeedChoice::Custom(s) => s.trim().to_string(),
    };
    // Vanilla/modded keep the Nether and End inside the world folder.
    let keep_dir = root.join(KEEP).join(name);
    let mut kept: Vec<String> = Vec::new();
    if keep_dims && dir.is_dir() {
        fs::create_dir_all(&keep_dir)?;
        for d in ["DIM-1", "DIM1"] {
            if dir.join(d).is_dir() {
                fs::rename(dir.join(d), keep_dir.join(d)).with_context(|| format!("keeping {d}"))?;
                kept.push(d.to_string());
            }
        }
    }
    let mut parts: Vec<String> = Vec::new();
    for f in folders(root, name) {
        let dim = f != name;
        if !(dim && keep_dims) {
            parts.push(f);
        }
    }
    let r = (|| {
        for f in &parts {
            trash(f)?;
        }
        anyhow::Ok(())
    })();
    // Put the kept dimensions back, whatever happened.
    if !kept.is_empty() {
        fs::create_dir_all(&dir)?;
        for d in &kept {
            fs::rename(keep_dir.join(d), dir.join(d)).with_context(|| format!("putting {d} back"))?;
        }
        let _ = fs::remove_dir(&keep_dir);
        let _ = fs::remove_dir(root.join(KEEP));
    }
    r?;
    let mut meta = load_meta(root);
    meta.pending.insert(name.to_string(), seed.clone());
    if meta.pregen.as_ref().is_some_and(|p| p.world == name) {
        meta.pregen = None;
    }
    save_meta(root, &meta)?;
    if level_name(root) == name {
        let path = root.join("server.properties");
        let mut p = Properties::load(&path)?;
        p.set("level-seed", seed.clone());
        p.save(&path)?;
    }
    Ok(seed)
}

/// Deletes a world that isn't active (its folders go through `trash`).
pub fn delete(root: &Path, name: &str, trash: &dyn Fn(&str) -> anyhow::Result<()>) -> anyhow::Result<()> {
    check_name(name)?;
    if level_name(root) == name {
        bail!("\"{name}\" is the active world. Make another one active first.");
    }
    for f in folders(root, name) {
        trash(&f)?;
    }
    let mut meta = load_meta(root);
    if meta.pending.remove(name).is_some() || meta.pregen.as_ref().is_some_and(|p| p.world == name) {
        if meta.pregen.as_ref().is_some_and(|p| p.world == name) {
            meta.pregen = None;
        }
        save_meta(root, &meta)?;
    }
    Ok(())
}

/// Unpacks an uploaded world zip (`zip_rel`, in the server folder) into a
/// new world `name`: the folder holding `level.dat` inside the zip becomes
/// the world folder. The zip is removed afterwards.
pub fn import_zip(root: &Path, zip_rel: &str, name: &str, job: &Job) -> anyhow::Result<()> {
    check_name(name)?;
    let dest = root.join(name);
    if dest.exists() {
        bail!("There's already a folder called \"{name}\".");
    }
    let zip_path = root.join(zip_rel);
    if zip_rel.contains("..") || !zip_path.is_file() {
        bail!("The upload isn't there.");
    }
    let mut z = zip::ZipArchive::new(fs::File::open(&zip_path)?).context("That isn't a zip file.")?;
    // The shortest path ending in level.dat marks the world's root.
    let prefix = (0..z.len())
        .filter_map(|i| z.by_index(i).ok().map(|f| f.name().replace('\\', "/")))
        .filter(|n| n == "level.dat" || n.ends_with("/level.dat"))
        .min_by_key(|n| n.len())
        .map(|n| n.trim_end_matches("level.dat").to_string())
        .context("No level.dat in that zip: it isn't a Minecraft world.")?;
    let tmp = root.join(format!(".{name}.importing"));
    let _ = fs::remove_dir_all(&tmp);
    let total = z.len();
    for i in 0..total {
        let mut f = z.by_index(i)?;
        let n = f.name().replace('\\', "/");
        let Some(rel) = n.strip_prefix(&prefix) else { continue };
        if rel.is_empty() || rel.split('/').any(|p| p == ".." || p.contains(':')) {
            continue;
        }
        let out = tmp.join(rel);
        if f.is_dir() {
            fs::create_dir_all(&out)?;
        } else {
            if let Some(p) = out.parent() {
                fs::create_dir_all(p)?;
            }
            std::io::copy(&mut f, &mut fs::File::create(&out)?)?;
        }
        if i % 200 == 0 {
            job.detail(format!("Unpacking {} of {total}", i + 1));
        }
    }
    fs::rename(&tmp, &dest)?;
    let _ = fs::remove_file(&zip_path);
    Ok(())
}

// ---- level.dat --------------------------------------------------------------------

#[derive(Default, Debug)]
pub struct LevelInfo {
    pub seed: Option<i64>,
    /// Milliseconds.
    pub last_played: Option<i64>,
}

/// The seed and last played time from a (gzipped NBT) `level.dat`.
pub fn read_level(path: &Path) -> LevelInfo {
    let Ok(f) = fs::File::open(path) else { return LevelInfo::default() };
    let mut buf = Vec::new();
    if flate2::read::GzDecoder::new(f).take(64 << 20).read_to_end(&mut buf).is_err() {
        return LevelInfo::default();
    }
    let mut info = LevelInfo::default();
    let mut r = Nbt { b: &buf, at: 0, path: Vec::new(), info: &mut info };
    let _ = r.root();
    info
}

/// Just enough NBT to find `Data.LastPlayed`, `Data.WorldGenSettings.seed`
/// (1.16+) and `Data.RandomSeed` (older).
struct Nbt<'a> {
    b: &'a [u8],
    at: usize,
    path: Vec<String>,
    info: &'a mut LevelInfo,
}

impl Nbt<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let s = self.b.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }
    fn i16(&mut self) -> Option<i16> {
        self.take(2).map(|s| i16::from_be_bytes([s[0], s[1]]))
    }
    fn i32(&mut self) -> Option<i32> {
        self.take(4).map(|s| i32::from_be_bytes(s.try_into().unwrap()))
    }
    fn i64(&mut self) -> Option<i64> {
        self.take(8).map(|s| i64::from_be_bytes(s.try_into().unwrap()))
    }
    fn string(&mut self) -> Option<String> {
        let n = self.i16()? as u16 as usize;
        self.take(n).map(|s| String::from_utf8_lossy(s).into_owned())
    }
    fn root(&mut self) -> Option<()> {
        let t = self.u8()?;
        let _ = self.string()?;
        self.payload(t, 0)
    }
    fn payload(&mut self, t: u8, depth: usize) -> Option<()> {
        if depth > 64 {
            return None;
        }
        match t {
            1 => drop(self.take(1)?),
            2 => drop(self.take(2)?),
            3 | 5 => drop(self.take(4)?),
            4 => {
                let v = self.i64()?;
                let p: Vec<&str> = self.path.iter().map(String::as_str).collect();
                match p.as_slice() {
                    ["Data", "LastPlayed"] => self.info.last_played = Some(v),
                    ["Data", "WorldGenSettings", "seed"] => self.info.seed = Some(v),
                    ["Data", "RandomSeed"] if self.info.seed.is_none() => self.info.seed = Some(v),
                    _ => {}
                }
            }
            6 => drop(self.take(8)?),
            7 => {
                let n = self.i32()?.max(0) as usize;
                self.take(n)?;
            }
            8 => drop(self.string()?),
            9 => {
                let et = self.u8()?;
                let n = self.i32()?.max(0) as usize;
                for _ in 0..n {
                    self.path.push("[]".into());
                    let r = self.payload(et, depth + 1);
                    self.path.pop();
                    r?;
                }
            }
            10 => loop {
                let ct = self.u8()?;
                if ct == 0 {
                    break;
                }
                let name = self.string()?;
                self.path.push(name);
                let r = self.payload(ct, depth + 1);
                self.path.pop();
                r?;
            },
            11 => {
                let n = self.i32()?.max(0) as usize;
                self.take(n.checked_mul(4)?)?;
            }
            12 => {
                let n = self.i32()?.max(0) as usize;
                self.take(n.checked_mul(8)?)?;
            }
            _ => return None,
        }
        Some(())
    }
}

// ---- Chunky -----------------------------------------------------------------------

/// How Chunky goes on this server: a mod for a loader, or a plugin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "loader", rename_all = "snake_case")]
pub enum ChunkyKind {
    /// Modrinth loader name: fabric, quilt, neoforge, forge.
    Mod(String),
    Plugin,
}

impl ChunkyKind {
    /// From the server's loader text ("fabric 0.16.9", "NeoForge 21.1.259",
    /// "paper 1.21.1-133"); None for vanilla.
    pub fn of(loader: &str, root: &Path) -> Option<Self> {
        let l = loader.to_ascii_lowercase();
        for m in ["neoforge", "forge", "fabric", "quilt"] {
            if l.starts_with(m) {
                return Some(ChunkyKind::Mod(m.into()));
            }
        }
        if ["paper", "purpur", "spigot", "bukkit", "folia", "pufferfish"].iter().any(|p| l.starts_with(p)) {
            return Some(ChunkyKind::Plugin);
        }
        if l.is_empty() && root.join("plugins").is_dir() {
            return Some(ChunkyKind::Plugin);
        }
        None
    }

    fn folder(&self) -> &'static str {
        match self {
            ChunkyKind::Mod(_) => "mods",
            ChunkyKind::Plugin => "plugins",
        }
    }

    fn accepts(&self, loaders: &[String]) -> bool {
        let has = |x: &str| loaders.iter().any(|l| l == x);
        match self {
            ChunkyKind::Mod(l) if l == "quilt" => has("quilt") || has("fabric"),
            ChunkyKind::Mod(l) => has(l),
            ChunkyKind::Plugin => ["paper", "bukkit", "spigot", "purpur", "folia"].iter().any(|x| has(x)),
        }
    }
}

/// The Chunky jar in `mods` or `plugins`, when there is one (on).
pub fn chunky_jar(root: &Path) -> Option<String> {
    for d in ["mods", "plugins"] {
        for e in fs::read_dir(root.join(d)).into_iter().flatten().flatten() {
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            if n.starts_with("chunky") && !n.starts_with("chunkyborder") && n.ends_with(".jar") {
                return Some(format!("{d}/{}", e.file_name().to_string_lossy()));
            }
        }
    }
    None
}

/// Downloads Chunky from Modrinth for `mc` into mods/ or plugins/.
/// Returns the file's path relative to the server folder.
pub fn install_chunky(root: &Path, mc: &str, kind: &ChunkyKind, job: &Job) -> anyhow::Result<String> {
    if mc.is_empty() {
        bail!("Hopper doesn't know this server's Minecraft version yet. Start it once first.");
    }
    job.detail(format!("Finding Chunky for Minecraft {mc}"));
    let versions = crate::modrinth::versions("chunky", mc, crate::cf::Loader::Any)?;
    let v = versions
        .iter()
        .filter(|v| kind.accepts(&v.loaders))
        .find(|v| v.version_type == "release")
        .or_else(|| versions.iter().find(|v| kind.accepts(&v.loaders)))
        .with_context(|| format!("Chunky has no build for Minecraft {mc} on this server type."))?;
    let file = v.files.iter().find(|f| f.primary).or(v.files.first()).context("Chunky's version has no file")?;
    let dir = root.join(kind.folder());
    fs::create_dir_all(&dir)?;
    let dest: PathBuf = dir.join(&file.filename);
    job.detail(format!("Downloading {}", file.filename));
    crate::cf::download(&file.url, &dest, |_| {})?;
    Ok(format!("{}/{}", kind.folder(), file.filename))
}

/// Notes that a pre-generation started (so its radius shows later).
pub fn note_pregen(root: &Path, world: &str, radius: u32) -> anyhow::Result<()> {
    let mut meta = load_meta(root);
    meta.pregen = Some(PregenStart { world: world.into(), radius, at: now() });
    save_meta(root, &meta)
}

pub fn last_pregen(root: &Path) -> Option<PregenStart> {
    load_meta(root).pregen
}

/// Chunky's progress, from its console lines.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Pregen {
    /// running, paused, done, cancelled.
    pub state: String,
    /// "minecraft:overworld" (mods) or "world" (plugins).
    pub dimension: String,
    pub percent: f32,
    pub chunks: u64,
    /// "0:04:10" as Chunky prints it.
    pub eta: String,
    /// Chunks per second.
    pub rate: f32,
}

fn after<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    s.find(key).map(|i| &s[i + key.len()..])
}

fn num_until(s: &str, stop: char) -> &str {
    s.split(stop).next().unwrap_or("").trim()
}

fn running_numbers(rest: &str, p: &mut Pregen) {
    if let Some(x) = after(rest, "Processed: ") {
        p.chunks = num_until(x, ' ').replace(',', "").parse().unwrap_or(0);
    }
    if let Some(x) = after(rest, "(") {
        p.percent = num_until(x, '%').parse().unwrap_or(0.0);
    }
    if let Some(x) = after(rest, "ETA: ") {
        p.eta = num_until(x, ',').to_string();
    }
    if let Some(x) = after(rest, "Rate: ") {
        p.rate = num_until(x, ' ').parse().unwrap_or(0.0);
    }
}

/// Chunky's state from its console lines: the last task line decides,
/// a stop after "Cancelling all tasks" is a cancel, and a pause keeps the
/// numbers of the last progress line before it.
pub fn chunky_progress(lines: &[String]) -> Option<Pregen> {
    let mut found: Option<Pregen> = None;
    for l in lines.iter().rev() {
        let Some(t) = after(l, "[Chunky] ") else { continue };
        if t.starts_with("Cancelling all tasks") {
            // The latest event (or just before the stop it caused).
            let dimension = found.map(|p| p.dimension).unwrap_or_default();
            return Some(Pregen { state: "cancelled".into(), dimension, ..Default::default() });
        }
        let Some(t) = t.strip_prefix("Task ") else { continue };
        let (word, rest) = match t.split_once(' ') {
            Some(x) => x,
            None => continue,
        };
        let rest = rest.trim_start_matches("for ").trim_start_matches("in ");
        let dimension = rest.split(['.', ' ']).next().unwrap_or("").to_string();
        match (word, found.as_mut()) {
            ("running", Some(p)) => {
                // Numbers for a pause found further down.
                let state = std::mem::take(&mut p.state);
                running_numbers(rest, p);
                p.state = state;
                p.eta.clear();
                p.rate = 0.0;
                return found;
            }
            ("running", None) => {
                let mut p = Pregen { state: "running".into(), dimension, ..Default::default() };
                running_numbers(rest, &mut p);
                return Some(p);
            }
            ("finished", _) => {
                let mut p = Pregen { state: "done".into(), dimension, percent: 100.0, ..Default::default() };
                if let Some(x) = after(rest, "Processed: ") {
                    p.chunks = num_until(x, ' ').replace(',', "").parse().unwrap_or(0);
                }
                return Some(p);
            }
            ("started" | "continuing", None) => return Some(Pregen { state: "running".into(), dimension, ..Default::default() }),
            ("started" | "continuing", Some(_)) => return found,
            ("paused" | "stopped", None) => found = Some(Pregen { state: "paused".into(), dimension, ..Default::default() }),
            ("paused" | "stopped", Some(_)) => {}
            ("cancelled", _) => return Some(Pregen { state: "cancelled".into(), dimension, ..Default::default() }),
            _ => {}
        }
    }
    found
}

/// The commands that start a pre-generation of `radius` blocks around
/// spawn, in the overworld.
pub fn chunky_commands(radius: u32, plugin: bool, level: &str, center: Option<(i64, i64)>) -> Vec<String> {
    let world = if plugin { level.to_string() } else { "minecraft:overworld".into() };
    let mut v = vec![format!("chunky world {world}")];
    match center {
        Some((x, z)) => v.push(format!("chunky center {x} {z}")),
        None => v.push("chunky spawn".into()),
    }
    v.push("chunky shape square".into());
    v.push(format!("chunky radius {radius}"));
    v.push("chunky start".into());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lines() {
        let l = |s: &str| s.to_string();
        let lines = vec![
            l("[12:00:00] [Server thread/INFO]: [Chunky] Task started for minecraft:overworld at 0, 0 with radius 100."),
            l("[12:00:01] [Chunky-minecraft:overworld Thread/INFO]: [Chunky] Task running for minecraft:overworld. Processed: 1,234 chunks (12.34%), ETA: 0:04:10, Rate: 85.2 cps, Current: 10, -5"),
        ];
        let p = chunky_progress(&lines).unwrap();
        assert_eq!((p.state.as_str(), p.dimension.as_str(), p.chunks, p.eta.as_str()), ("running", "minecraft:overworld", 1234, "0:04:10"));
        assert!((p.percent - 12.34).abs() < 0.01 && (p.rate - 85.2).abs() < 0.01);
        let mut more = lines.clone();
        more.push(l("[INFO]: [Chunky] Task finished for minecraft:overworld. Processed: 196 chunks (100.00%), Total time: 0:00:03"));
        let p = chunky_progress(&more).unwrap();
        assert_eq!((p.state.as_str(), p.chunks), ("done", 196));
        more.push(l("[INFO]: [Chunky] Task cancelled for world."));
        assert_eq!(chunky_progress(&more).unwrap().state, "cancelled");
        // As Chunky 1.4.23 on Fabric prints them.
        let mut real = vec![
            l("[04:52:38] [Server thread/INFO]: [Chunky] Task started in minecraft:overworld for the square region centered at 0, 0 with radius 3000."),
        ];
        assert_eq!(chunky_progress(&real).unwrap().state, "running");
        real.push(l("[04:53:24] [Chunky-minecraft:overworld Thread/INFO]: [Chunky] Task running for minecraft:overworld. Processed: 2622 chunks (1.84%), ETA: 0:33:33, Rate: 69.3 cps, Current: 3, 7"));
        real.push(l("[04:53:25] [Server thread/INFO]: [Chunky] Task paused for minecraft:overworld."));
        real.push(l("[04:53:25] [Chunky-minecraft:overworld Thread/INFO]: [Chunky] Task stopped for minecraft:overworld."));
        let p = chunky_progress(&real).unwrap();
        assert_eq!((p.state.as_str(), p.chunks), ("paused", 2622));
        assert!((p.percent - 1.84).abs() < 0.01);
        real.push(l("[04:53:31] [Server thread/INFO]: [Chunky] Task continuing for minecraft:overworld."));
        assert_eq!(chunky_progress(&real).unwrap().state, "running");
        real.push(l("[04:53:40] [Server thread/INFO]: [Chunky] Cancelled tasks cannot be continued. If you are sure you want to cancel, type '/chunky confirm'."));
        real.push(l("[04:53:40] [Server thread/INFO]: [Chunky] Cancelling all tasks."));
        real.push(l("[04:53:40] [Chunky-minecraft:overworld Thread/INFO]: [Chunky] Task stopped for minecraft:overworld."));
        assert_eq!(chunky_progress(&real).unwrap().state, "cancelled");
        // Cancelled while paused: no stop line follows.
        real.truncate(real.len() - 4);
        assert_eq!(chunky_progress(&real).unwrap().state, "paused");
        real.push(l("[05:01:07] [Server thread/INFO]: [Chunky] Cancelling all tasks."));
        assert_eq!(chunky_progress(&real).unwrap().state, "cancelled");
        assert_eq!(chunky_progress(&[l("nothing")]), None);
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-worlds-{tag}-{}", now()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// level.dat with Data { LastPlayed: 1700000000000L, WorldGenSettings { seed: -42L } }.
    fn level_dat(path: &Path) {
        let mut b = vec![10u8, 0, 0];
        let tag = |b: &mut Vec<u8>, t: u8, n: &str| {
            b.push(t);
            b.extend((n.len() as u16).to_be_bytes());
            b.extend(n.as_bytes());
        };
        tag(&mut b, 10, "Data");
        tag(&mut b, 8, "LevelName");
        b.extend(3u16.to_be_bytes());
        b.extend(b"abc");
        tag(&mut b, 4, "LastPlayed");
        b.extend(1_700_000_000_000i64.to_be_bytes());
        tag(&mut b, 9, "List");
        b.push(3);
        b.extend(2i32.to_be_bytes());
        b.extend([0, 0, 0, 1, 0, 0, 0, 2]);
        tag(&mut b, 10, "WorldGenSettings");
        tag(&mut b, 4, "seed");
        b.extend((-42i64).to_be_bytes());
        b.push(0);
        b.push(0);
        b.push(0);
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut e, &b).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, e.finish().unwrap()).unwrap();
    }

    #[test]
    fn reads_level_dat() {
        let d = tmp("nbt");
        level_dat(&d.join("world/level.dat"));
        let i = read_level(&d.join("world/level.dat"));
        assert_eq!((i.seed, i.last_played), (Some(-42), Some(1_700_000_000_000)));
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn create_switch_reset_delete() {
        let d = tmp("flow");
        fs::write(d.join("server.properties"), "level-name=world\nlevel-seed=\n").unwrap();
        level_dat(&d.join("world/level.dat"));
        fs::create_dir_all(d.join("world/DIM-1")).unwrap();
        fs::write(d.join("world/DIM-1/x"), "nether").unwrap();
        // Bukkit style sibling is grouped, not its own world.
        level_dat(&d.join("world_nether/level.dat"));
        let ws = list(&d);
        assert_eq!(ws.len(), 1);
        assert_eq!((ws[0].active, ws[0].seed.as_deref(), ws[0].folders.len()), (true, Some("-42"), 2));

        create(&d, "event", "123", false).unwrap();
        assert!(create(&d, "world", "", false).is_err());
        assert!(create(&d, "../x", "", false).is_err());
        let ws = list(&d);
        assert_eq!(ws.len(), 2);
        let ev = ws.iter().find(|w| w.name == "event").unwrap();
        assert_eq!((ev.created, ev.seed.as_deref(), ev.active), (false, Some("123"), false));

        set_active(&d, "event").unwrap();
        let p = props(&d).unwrap();
        assert_eq!((p.get("level-name"), p.get("level-seed")), (Some("event"), Some("123")));
        assert!(delete(&d, "event", &|_| Ok(())).is_err());

        set_active(&d, "world").unwrap();
        let trashed = std::cell::RefCell::new(Vec::<String>::new());
        let trash = |rel: &str| {
            trashed.borrow_mut().push(rel.to_string());
            fs::rename(d.join(rel), d.join(format!("{rel}.gone")))?;
            Ok(())
        };
        let seed = reset(&d, "world", &SeedChoice::Same, true, &trash).unwrap();
        assert_eq!(seed, "-42");
        assert_eq!(*trashed.borrow(), vec!["world".to_string()]);
        assert_eq!(fs::read_to_string(d.join("world/DIM-1/x")).unwrap(), "nether");
        assert!(!d.join("world/level.dat").exists() && d.join("world_nether").is_dir());
        assert_eq!(props(&d).unwrap().get("level-seed"), Some("-42"));

        delete(&d, "event", &trash).unwrap();
        assert!(list(&d).iter().all(|w| w.name != "event"));
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn chunky_kind() {
        let d = std::env::temp_dir();
        assert_eq!(ChunkyKind::of("fabric 0.16.9", &d), Some(ChunkyKind::Mod("fabric".into())));
        assert_eq!(ChunkyKind::of("NeoForge 21.1.259", &d), Some(ChunkyKind::Mod("neoforge".into())));
        assert_eq!(ChunkyKind::of("paper 1.21.1-133", &d), Some(ChunkyKind::Plugin));
        assert_eq!(ChunkyKind::of("Vanilla", &d), None);
        assert!(ChunkyKind::Mod("quilt".into()).accepts(&["fabric".into()]));
        assert_eq!(chunky_commands(100, false, "world", None).last().unwrap(), "chunky start");
    }
}
