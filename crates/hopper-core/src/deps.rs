//! Required mods that aren't installed, read from each jar's own metadata
//! (what the loader checks at startup), not CurseForge's project
//! relations, which list other loaders' dependencies and stale ones.

use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

/// Ids every loader provides itself.
const BUILTIN: &[&str] = &[
    "minecraft",
    "neoforge",
    "forge",
    "fml",
    "javafml",
    "java",
    "fabricloader",
    "fabric",
    "quilt_loader",
    "lowcodefml",
    "mixinextras",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Missing {
    /// The mod that needs it.
    pub mod_name: String,
    pub file_name: String,
    /// The missing mod id, and the versions it accepts.
    pub needs: String,
    pub versions: String,
    /// A disabled jar that would provide it.
    pub disabled_provider: Option<String>,
    /// Installed, but a version outside `versions`: (jar, its version).
    pub wrong_version: Option<(String, String)>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Toml {
    mods: Vec<TomlMod>,
    dependencies: HashMap<String, Vec<TomlDep>>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct TomlMod {
    mod_id: String,
    display_name: String,
    version: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct TomlDep {
    mod_id: String,
    /// NeoForge: "required" | "optional" | "incompatible" | "discouraged".
    #[serde(rename = "type")]
    kind: String,
    /// Forge: true when required.
    mandatory: Option<bool>,
    version_range: String,
    side: String,
    /// Why, on an incompatible one.
    reason: String,
}

#[derive(Default)]
struct JarInfo {
    name: String,
    provides: Vec<String>,
    /// The mod ids this jar itself is (not nested).
    own: Vec<String>,
    /// (mod id, its version) for the mods this jar itself is (not nested).
    versions: Vec<(String, String)>,
    /// (mod id, version range)
    requires: Vec<(String, String)>,
    /// Optional mods: not needed, but when installed the loader still
    /// holds them to the range. (mod id, version range)
    optional: Vec<(String, String)>,
    /// Mods it refuses to load beside: (mod id, version range, reason).
    incompatible: Vec<(String, String, String)>,
    /// What the loader reads to decide the jar is a mod.
    meta: Meta,
    /// A jar with both files: what its mods.toml declares (the fields
    /// above are its neoforge.mods.toml's).
    forge: Option<Box<TomlDeps>>,
}

/// (required, optional, incompatible), as in [`JarInfo`].
type TomlDeps = (Vec<(String, String)>, Vec<(String, String)>, Vec<(String, String, String)>);

fn toml_deps(t: &Toml) -> TomlDeps {
    let mut out = TomlDeps::default();
    for deps in t.dependencies.values() {
        for d in deps {
            if d.side.eq_ignore_ascii_case("SERVER") {
                continue;
            }
            let id = d.mod_id.to_lowercase();
            let required = d.kind.eq_ignore_ascii_case("required") || d.mandatory == Some(true);
            if required {
                out.0.push((id, d.version_range.clone()));
            } else if d.kind.eq_ignore_ascii_case("incompatible") {
                out.2.push((id, d.version_range.clone(), d.reason.clone()));
            } else if d.kind.eq_ignore_ascii_case("optional") || d.mandatory == Some(false) {
                out.1.push((id, d.version_range.clone()));
            }
        }
    }
    out
}

/// Which loader metadata a jar carries.
#[derive(Default, Clone, Copy)]
struct Meta {
    /// `META-INF/neoforge.mods.toml`: `Some(true)` when it reads as TOML
    /// and names its `modLoader`.
    neoforge: Option<bool>,
    fabric: bool,
    /// The manifest marks it a library or language provider (`FMLModType`).
    fml_mod_type: bool,
}

fn read(zip: &mut zip::ZipArchive<impl Read + std::io::Seek>, name: &str) -> Option<String> {
    let mut e = zip.by_name(name).ok()?;
    let mut s = String::new();
    e.by_ref().take(512 * 1024).read_to_string(&mut s).ok()?;
    Some(s)
}

fn inspect(zip: &mut zip::ZipArchive<impl Read + std::io::Seek>, depth: u8) -> JarInfo {
    let mut info = JarInfo::default();
    if let Some(text) = read(zip, "META-INF/neoforge.mods.toml") {
        // Read loosely: a file this can't parse isn't judged.
        let ok = match toml::from_str::<toml::Value>(&text) {
            Ok(v) => v.get("modLoader").and_then(|l| l.as_str()).is_some_and(|l| !l.trim().is_empty()),
            Err(_) => true,
        };
        info.meta.neoforge = Some(ok);
    }
    info.meta.fabric = zip.by_name("fabric.mod.json").is_ok();
    info.meta.fml_mod_type = ["META-INF/MANIFEST.MF", "META-INF/MANIFEST.mf"]
        .iter()
        .filter_map(|m| read(zip, m))
        .any(|t| t.lines().any(|l| l.trim_start().starts_with("FMLModType:")));
    for meta in ["META-INF/neoforge.mods.toml", "META-INF/mods.toml"] {
        let Some(text) = read(zip, meta) else {
            continue;
        };
        let Ok(t) = toml::from_str::<Toml>(&text) else {
            continue;
        };
        // ${file.jarVersion} comes from the manifest, as the loader does.
        let manifest = read(zip, "META-INF/MANIFEST.MF")
            .and_then(|t| {
                t.lines()
                    .find_map(|l| l.trim().strip_prefix("Implementation-Version:").map(|v| v.trim().to_string()))
            })
            .filter(|v| !v.is_empty() && !v.contains("${"));
        for m in &t.mods {
            info.provides.push(m.mod_id.to_lowercase());
            info.own.push(m.mod_id.to_lowercase());
            let v = if m.version.contains("${") { manifest.clone().unwrap_or_default() } else { m.version.clone() };
            if !v.is_empty() {
                info.versions.push((m.mod_id.to_lowercase(), v));
            }
            if info.name.is_empty() {
                info.name = m.display_name.clone();
            }
        }
        let (r, o, i) = toml_deps(&t);
        (info.requires, info.optional, info.incompatible) = (r, o, i);
        // Forge, and NeoForge before 1.20.5, read mods.toml instead.
        if meta.contains("neoforge")
            && let Some(t) = read(zip, "META-INF/mods.toml").and_then(|x| toml::from_str::<Toml>(&x).ok())
        {
            info.forge = Some(Box::new(toml_deps(&t)));
        }
        break;
    }
    // Multi-loader jars carry Fabric metadata too; on Forge and NeoForge
    // only mods.toml counts.
    let forge_jar = !info.provides.is_empty();
    if !forge_jar
        && let Some(text) = read(zip, "fabric.mod.json")
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
    {
        if let Some(id) = v["id"].as_str() {
            info.provides.push(id.to_lowercase());
            info.own.push(id.to_lowercase());
            if let Some(ver) = v["version"].as_str().filter(|x| !x.contains("${")) {
                info.versions.push((id.to_lowercase(), ver.to_string()));
            }
        }
        for p in v["provides"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p.as_str())
        {
            info.provides.push(p.to_lowercase());
        }
        if info.name.is_empty() {
            info.name = v["name"].as_str().unwrap_or_default().to_string();
        }
        for (id, range) in v["depends"].as_object().into_iter().flatten() {
            info.requires.push((
                id.to_lowercase(),
                range.to_string().trim_matches('"').to_string(),
            ));
        }
        // "breaks" stops the game; "conflicts" only warns.
        for (id, range) in v["breaks"].as_object().into_iter().flatten() {
            info.incompatible.push((
                id.to_lowercase(),
                range.to_string().trim_matches('"').to_string(),
                String::new(),
            ));
        }
    }
    // Jar-in-jar: NeoForge's jarjar and Fabric's bundled jars provide ids too.
    if depth < 2 {
        let nested: Vec<String> = zip
            .file_names()
            .filter(|n| {
                (n.starts_with("META-INF/jarjar/") || n.starts_with("META-INF/jars/"))
                    && n.ends_with(".jar")
            })
            .map(str::to_string)
            .collect();
        for n in nested {
            let mut buf = Vec::new();
            if zip
                .by_name(&n)
                .and_then(|mut e| Ok(e.read_to_end(&mut buf)?))
                .is_err()
            {
                continue;
            }
            if let Ok(mut z) = zip::ZipArchive::new(std::io::Cursor::new(buf)) {
                let inner = inspect(&mut z, depth + 1);
                info.provides.extend(inner.provides);
            }
        }
    }
    info
}

/// Each jar read so far, by path, with the size and modified time it had:
/// the Mods tab's scan fills it, so a later check (what removing a mod
/// breaks) reads only jars that changed instead of the whole pack again.
type JarCache = HashMap<PathBuf, (u64, Option<std::time::SystemTime>, Arc<JarInfo>)>;
static JARS: LazyLock<Mutex<JarCache>> = LazyLock::new(Default::default);

fn jar(path: &Path) -> Option<Arc<JarInfo>> {
    let meta = std::fs::metadata(path).ok()?;
    let stamp = (meta.len(), meta.modified().ok());
    let cache = || JARS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((len, at, info)) = cache().get(path)
        && (*len, *at) == stamp
    {
        return Some(info.clone());
    }
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    let info = Arc::new(inspect(&mut z, 0));
    cache().insert(path.to_path_buf(), (stamp.0, stamp.1, info.clone()));
    Some(info)
}

/// The mod ids a jar provides (nested jars included).
pub fn provides(path: &Path) -> Vec<String> {
    jar(path).map(|j| j.provides.clone()).unwrap_or_default()
}

/// The mod ids a jar itself is, without the ones nested inside it, and
/// its display name (empty when it has none).
pub fn own_ids(path: &Path) -> (Vec<String>, String) {
    jar(path).map(|j| (j.own.clone(), j.name.clone())).unwrap_or_default()
}

/// Reads jars on a few threads.
fn read_all(list: &[&PathBuf]) -> Vec<(PathBuf, Arc<JarInfo>)> {
    let chunk = list.len().div_ceil(8).max(1);
    std::thread::scope(|s| {
        let parts: Vec<_> = list
            .chunks(chunk)
            .map(|part| s.spawn(move || part.iter().filter_map(|p| jar(p).map(|j| ((*p).clone(), j))).collect::<Vec<_>>()))
            .collect();
        parts.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    })
}

/// The enabled jars of a mods folder.
fn enabled_jars(mods_dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(mods_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().to_lowercase().ends_with(".jar") && p.is_file())
        .collect()
}

/// Required mod ids no enabled jar in `mods_dir` provides.
pub fn missing(mods_dir: &Path) -> Vec<Missing> {
    missing_without(mods_dir, &[])
}

/// [`missing`] as it would be with the files named in `without` gone.
pub fn missing_without(mods_dir: &Path, without: &[String]) -> Vec<Missing> {
    let entries: Vec<PathBuf> = std::fs::read_dir(mods_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().unwrap_or_default().to_string_lossy();
            !without.iter().any(|w| *w == n)
        })
        .collect();
    let is = |p: &Path, ext: &str| p.to_string_lossy().to_lowercase().ends_with(ext);
    let enabled: Vec<&PathBuf> = entries.iter().filter(|p| is(p, ".jar")).collect();
    let disabled: Vec<&PathBuf> = entries.iter().filter(|p| is(p, ".jar.disabled")).collect();
    let on = read_all(&enabled);
    let off = read_all(&disabled);
    let provided: HashSet<&str> = on
        .iter()
        .flat_map(|(_, j)| j.provides.iter().map(String::as_str))
        .chain(BUILTIN.iter().copied())
        .collect();
    // Which jar provides each id, and at what version.
    let versions: HashMap<&str, (&PathBuf, &str)> = on
        .iter()
        .flat_map(|(p, j)| j.versions.iter().map(move |(id, v)| (id.as_str(), (p, v.as_str()))))
        .collect();
    let name_of = |p: &Path| p.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let mut out = Vec::new();
    for (path, j) in &on {
        let mut seen = HashSet::new();
        for (id, range) in &j.requires {
            if !seen.insert(id.clone()) {
                continue;
            }
            if provided.contains(id.as_str()) {
                // Installed: is it a version this mod accepts?
                if let Some((by, have)) = versions.get(id.as_str())
                    && *by != path
                    && accepts(range, have) == Some(false)
                {
                    out.push(Missing {
                        mod_name: if j.name.is_empty() { id.clone() } else { j.name.clone() },
                        file_name: name_of(path),
                        needs: id.clone(),
                        versions: range.clone(),
                        disabled_provider: None,
                        wrong_version: Some((name_of(by), have.to_string())),
                    });
                }
                continue;
            }
            let disabled_provider = off
                .iter()
                .find(|(_, d)| d.provides.iter().any(|p| p == id))
                .map(|(p, _)| {
                    p.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                });
            out.push(Missing {
                mod_name: if j.name.is_empty() {
                    id.clone()
                } else {
                    j.name.clone()
                },
                file_name: path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                needs: id.clone(),
                versions: range.clone(),
                disabled_provider,
                wrong_version: None,
            });
        }
    }
    out.sort_by_key(|a| a.mod_name.to_lowercase());
    out
}

/// Something in a mods folder the loader refuses at startup, besides a
/// missing mod ([`missing`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blocker {
    /// The jar at fault, and its mod's name.
    pub file_name: String,
    pub mod_name: String,
    pub kind: BlockerKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockerKind {
    /// The loader or Minecraft isn't a version the mod accepts.
    Loader { id: String, versions: String, have: String },
    /// A mod it needs or works with is installed at a version it rejects.
    WrongVersion { needs: String, versions: String, jar: String, have: String },
    /// It refuses to load beside this installed mod.
    Incompatible { with: String, jar: String, reason: String },
    /// The loader can't read the jar as a mod: why.
    Invalid(String),
}

impl Blocker {
    /// One line for the user, after the mod's name.
    pub fn text(&self) -> String {
        match &self.kind {
            BlockerKind::Loader { id, versions, have } => {
                format!("needs {} {}, this instance has {have}", loader_name(id), range_words(versions))
            }
            BlockerKind::WrongVersion { needs, versions, jar, have } => {
                format!("only works with {needs} {}, {jar} is {have}", range_words(versions))
            }
            BlockerKind::Incompatible { with, jar, reason } => {
                let why = if reason.is_empty() { String::new() } else { format!(": {reason}") };
                format!("won't load beside {with} ({jar}){why}")
            }
            BlockerKind::Invalid(why) => why.clone(),
        }
    }
}

fn loader_name(id: &str) -> &str {
    match id {
        "neoforge" => "NeoForge",
        "forge" => "Forge",
        "fabricloader" => "Fabric Loader",
        "quilt_loader" => "Quilt Loader",
        "minecraft" => "Minecraft",
        _ => id,
    }
}

/// "[21.1.262,)" → "21.1.262 or newer"; other ranges as written.
fn range_words(r: &str) -> String {
    let t = r.trim();
    if let Some(v) = t.strip_prefix('[').and_then(|x| x.strip_suffix(",)")) {
        return format!("{} or newer", v.trim());
    }
    if let Some(v) = t.strip_prefix(">=") {
        return format!("{} or newer", v.trim());
    }
    t.to_string()
}

/// The loader's kind and version out of an instance's loader id:
/// "neoforge-21.1.259" → ("neoforge", "21.1.259"); "fabric-loader-0.16"
/// and "fabric-0.16.14-1.21.1" → ("fabric", "0.16…").
fn loader_version<'a>(loader: &'a str, game: &str) -> Option<(&'a str, &'a str)> {
    let (kind, rest) = loader.split_once('-')?;
    let rest = rest.strip_prefix("loader-").unwrap_or(rest);
    let v = rest.split('-').find(|p| *p != game && !p.is_empty())?;
    Some((kind, v))
}

/// Whether Minecraft `v` is at least 1.`min.0`.`min.1` (26.1 and on count
/// as newer than any 1.x).
fn mc_at_least(v: &str, min: (u64, u64)) -> bool {
    let mut p = v.split('.').map(|x| x.parse::<u64>().unwrap_or(0));
    match (p.next(), p.next(), p.next()) {
        (Some(1), Some(a), b) => (a, b.unwrap_or(0)) >= min,
        (Some(y), _, _) => y > 1,
        _ => false,
    }
}

/// Whether `have` is below `range`'s lowest version. Only that stops the
/// game: NeoForge loads mods past a range's upper end (Cucumber's
/// "[1.21,1.21.1)" on 1.21.1 runs).
fn too_old(range: &str, have: &str) -> bool {
    let r = range.trim().trim_matches('"');
    if r.starts_with('[') && r.contains('"') {
        let Ok(alts) = serde_json::from_str::<Vec<String>>(r) else { return false };
        return !alts.is_empty() && alts.iter().all(|a| too_old(a, have));
    }
    if r.starts_with('[') || r.starts_with('(') {
        let inner = r[1..].trim_end_matches([']', ')']);
        let lo = inner.split(',').next().unwrap_or_default().trim();
        return !lo.is_empty() && accepts(&format!("{}{lo},)", &r[..1]), have) == Some(false);
    }
    r.split_whitespace().any(|part| {
        if part.starts_with('<') {
            return false;
        }
        let v = part.trim_start_matches(['>', '=', '~', '^']).trim_end_matches(".x").trim_end_matches(".*");
        !v.is_empty() && accepts(&format!(">={v}"), have) == Some(false)
    })
}

/// What the loader will refuse in `mods_dir` for an instance of `loader`
/// ("neoforge-21.1.259") and Minecraft `game`: mods needing another loader
/// or Minecraft version, wrong versions of needed or optional mods, mods
/// that refuse each other, and jars the loader can't read. Missing mods
/// are [`missing`]'s.
pub fn blockers(mods_dir: &Path, loader: &str, game: &str) -> Vec<Blocker> {
    let paths = enabled_jars(mods_dir);
    let refs: Vec<&PathBuf> = paths.iter().collect();
    let on = read_all(&refs);
    let name_of = |p: &Path| p.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let mod_name = |p: &Path, j: &JarInfo| if j.name.is_empty() { name_of(p) } else { j.name.clone() };
    let mut out = Vec::new();

    // The loader's own ids, at the instance's versions.
    let lv = loader_version(loader, game);
    let neoforge = lv.is_some_and(|(k, _)| k.eq_ignore_ascii_case("neoforge"));
    let mut builtin: HashMap<&str, &str> = HashMap::new();
    if !game.is_empty() {
        builtin.insert("minecraft", game);
    }
    if let Some((kind, v)) = lv {
        match kind.to_ascii_lowercase().as_str() {
            "neoforge" => {
                builtin.insert("neoforge", v);
                // 1.20.1's NeoForge still answered to "forge".
                if !mc_at_least(game, (20, 2)) {
                    builtin.insert("forge", v);
                }
            }
            "forge" => {
                builtin.insert("forge", v);
            }
            "fabric" => {
                builtin.insert("fabricloader", v);
            }
            "quilt" => {
                builtin.insert("quilt_loader", v);
            }
            _ => {}
        }
    }

    // Jars that aren't mods to this loader. NeoForge from 1.20.5 reports
    // them; older loaders pass over them.
    if neoforge && mc_at_least(game, (20, 5)) {
        let connector = on.iter().any(|(_, j)| j.provides.iter().any(|p| p == "connector"));
        for p in &paths {
            let Some((_, j)) = on.iter().find(|(q, _)| q == p) else {
                out.push(Blocker {
                    file_name: name_of(p),
                    mod_name: name_of(p),
                    kind: BlockerKind::Invalid("can't be opened as a jar (a broken download?)".into()),
                });
                continue;
            };
            let why = match j.meta.neoforge {
                // FML 4 (to 1.21.1) needs modLoader in the file.
                Some(false) if !mc_at_least(game, (21, 2)) => {
                    Some("isn't a valid NeoForge mod file here (made for a newer NeoForge: no modLoader)")
                }
                Some(_) => None,
                None if j.meta.fml_mod_type => None,
                None if j.meta.fabric && !connector => Some("is a Fabric mod, not a NeoForge one"),
                None => None,
            };
            if let Some(why) = why {
                out.push(Blocker { file_name: name_of(p), mod_name: mod_name(p, j), kind: BlockerKind::Invalid(why.into()) });
            }
        }
    }

    let versions: HashMap<&str, (&PathBuf, &str)> = on
        .iter()
        .flat_map(|(p, j)| j.versions.iter().map(move |(id, v)| (id.as_str(), (p, v.as_str()))))
        .collect();
    // Which jar provides each id (nested ones too), for incompatibilities.
    let mut provider: HashMap<&str, &PathBuf> = HashMap::new();
    for (p, j) in &on {
        for id in &j.provides {
            provider.entry(id.as_str()).or_insert(p);
        }
    }

    // Forge, and NeoForge for 1.20.1, read mods.toml over neoforge.mods.toml.
    let reads_mods_toml = !neoforge || !mc_at_least(game, (20, 5));
    for (path, j) in &on {
        let (requires, optional, incompatible) = match &j.forge {
            Some(f) if reads_mods_toml => (&f.0, &f.1, &f.2),
            _ => (&j.requires, &j.optional, &j.incompatible),
        };
        let mut seen = HashSet::new();
        for (id, range) in requires.iter().chain(optional) {
            if !seen.insert(id.clone()) || j.provides.contains(id) {
                continue;
            }
            if let Some(have) = builtin.get(id.as_str()) {
                if too_old(range, have) {
                    out.push(Blocker {
                        file_name: name_of(path),
                        mod_name: mod_name(path, j),
                        kind: BlockerKind::Loader { id: id.clone(), versions: range.clone(), have: have.to_string() },
                    });
                }
                continue;
            }
            if let Some((by, have)) = versions.get(id.as_str())
                && *by != path
                && too_old(range, have)
            {
                out.push(Blocker {
                    file_name: name_of(path),
                    mod_name: mod_name(path, j),
                    kind: BlockerKind::WrongVersion {
                        needs: id.clone(),
                        versions: range.clone(),
                        jar: name_of(by),
                        have: have.to_string(),
                    },
                });
            }
        }
        for (id, range, reason) in incompatible {
            // Embeddium gets Iris past its own "incompatible" (ATM10 runs both).
            if id == "embeddium" {
                continue;
            }
            let Some(by) = provider.get(id.as_str()).filter(|by| **by != path) else { continue };
            // Its version, when known, must be in the range.
            let hit = match versions.get(id.as_str()) {
                Some((_, have)) => accepts(range, have) != Some(false),
                None => true,
            };
            if hit {
                out.push(Blocker {
                    file_name: name_of(path),
                    mod_name: mod_name(path, j),
                    kind: BlockerKind::Incompatible { with: id.clone(), jar: name_of(by), reason: reason.clone() },
                });
            }
        }
    }
    out.sort_by_key(|b| b.mod_name.to_lowercase());
    out
}

/// A version as numbers plus whether it's a pre-release: the leading
/// dotted numbers ("19.2.17" of "19.2.17+1.21.1", "1.11.8" of
/// "1.11.8-1.21.1"), and a following -alpha/-beta/-rc/-pre/-snapshot, or a
/// bare trailing "-" (Maven's "lowest pre-release of"). `None` when it
/// doesn't start with a number.
fn key(v: &str) -> Option<(Vec<u64>, bool)> {
    let v = v.trim();
    let end = v.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(v.len());
    let nums: Vec<u64> = v[..end].trim_end_matches('.').split('.').filter_map(|p| p.parse().ok()).collect();
    if nums.is_empty() {
        return None;
    }
    let rest = v[end..].to_lowercase();
    let pre = rest == "-"
        || ["-alpha", "-beta", "-rc", "-pre", "-snapshot", ".alpha", ".beta"]
            .iter()
            .any(|t| rest.starts_with(t));
    Some((nums, pre))
}

/// "1.21.1-4.4.9" → "4.4.9": a Minecraft prefix before the mod's own
/// version.
fn without_mc(v: &str) -> &str {
    match v.trim().split_once('-') {
        // A real Minecraft version (1.7 to 1.22), not "1.0.203".
        Some((mc, rest)) if mc.matches('.').count() <= 2
            && mc.strip_prefix("1.").and_then(|m| m.split('.').next()?.parse::<u32>().ok()).is_some_and(|m| (7..=22).contains(&m))
            && rest.chars().next().is_some_and(|c| c.is_ascii_digit()) => rest,
        _ => v,
    }
}

/// `have` against a range bound. A Minecraft prefix on `have` is dropped
/// unless the bound is written that way too (Botania's "1.20.1-450").
fn cmp(have: &str, bound: &str) -> Option<std::cmp::Ordering> {
    let b = without_mc(bound);
    let a = if b != bound.trim() {
        // "1.21.1-2.4.21": both sides lose the prefix.
        without_mc(have)
    } else if key(bound).is_some_and(|(n, _)| n.first() == Some(&1)) {
        // "1.20.1" style bound: compare as written.
        have
    } else {
        without_mc(have)
    };
    let ((mut x, xp), (mut y, yp)) = (key(a)?, key(b)?);
    // A version cut short ("10.7.19" for 10.7.19.85) can't be judged.
    if x.len() < y.len() && y.starts_with(&x) {
        return None;
    }
    let n = x.len().max(y.len());
    x.resize(n, 0);
    y.resize(n, 0);
    // Same numbers: a pre-release comes before the release.
    Some(x.cmp(&y).then(yp.cmp(&xp)))
}

/// Whether version `have` fits `range`: a Maven range ("[4.4,)",
/// "[1.0,2.0)", "4.4" meaning at least) or a Fabric one (">=0.116",
/// "<2", "~1.2", "^1.2", "1.2.x", "*", several separated by spaces).
/// `None` when the range isn't understood (no warning then).
pub fn accepts(range: &str, have: &str) -> Option<bool> {
    use std::cmp::Ordering::*;
    let r = range.trim().trim_matches('"');
    if r.is_empty() || r == "*" || have.trim().is_empty() {
        return Some(true);
    }
    // "NONE", "any", "null", a file id: not a range.
    if !r.contains(|c: char| c.is_ascii_digit()) || (!r.contains('.') && !r.starts_with(['[', '('])) {
        return None;
    }
    // Fabric lists alternatives as a JSON array.
    if r.starts_with('[') && r.contains('"') {
        let alts: Vec<String> = serde_json::from_str(r).ok()?;
        let res: Vec<Option<bool>> = alts.iter().map(|a| accepts(a, have)).collect();
        if res.iter().any(|x| *x == Some(true)) {
            return Some(true);
        }
        return if res.iter().all(|x| x.is_some()) { Some(false) } else { None };
    }
    // Maven: [a,b) (a,b] [a] ; several joined by "," between brackets.
    if r.starts_with('[') || r.starts_with('(') {
        let inner = &r[1..r.len().checked_sub(1)?];
        let (lo_inc, hi_inc) = (r.starts_with('['), r.ends_with(']'));
        let (lo, hi) = match inner.split_once(',') {
            Some((l, h)) => (l.trim(), h.trim()),
            None => (inner.trim(), inner.trim()),
        };
        if !lo.is_empty() {
            match cmp(have, lo)? {
                Less => return Some(false),
                Equal if !lo_inc => return Some(false),
                _ => {}
            }
        }
        if !hi.is_empty() {
            match cmp(have, hi)? {
                Greater => return Some(false),
                Equal if !hi_inc => return Some(false),
                _ => {}
            }
        }
        return Some(true);
    }
    // Fabric / semver comparators, all must hold.
    let mut ok = true;
    for part in r.split_whitespace() {
        let (op, v) = ["<=", ">=", "<", ">", "=", "~", "^"]
            .iter()
            .find_map(|op| part.strip_prefix(op).map(|v| (*op, v)))
            .unwrap_or(("", part));
        let v = v.trim();
        if v.is_empty() {
            return None;
        }
        let o = cmp(have, v.trim_end_matches(".x").trim_end_matches(".*"))?;
        let holds = match op {
            ">=" => o != Less,
            ">" => o == Greater,
            "<=" => o != Greater,
            "<" => o == Less,
            "=" => o == Equal,
            // ~1.2 and ^1.2 and "1.2.x": at least it, same leading part.
            "~" | "^" | "" => {
                let keep = if op == "^" { 1 } else { v.split('.').count().saturating_sub(1).max(1) };
                let want: Vec<&str> = v.split('.').filter(|p| *p != "x" && *p != "*").collect();
                let got: Vec<&str> = have.trim().split(['.', '-', '+']).collect();
                let prefix_ok = want.iter().take(keep).zip(got.iter()).all(|(a, b)| a == b);
                if op.is_empty() && !v.contains('x') && !v.contains('*') {
                    // A bare version: that version (Fabric) or at least it (Forge soft).
                    o != Less
                } else {
                    prefix_ok && o != Less
                }
            }
            _ => return None,
        };
        ok &= holds;
    }
    Some(ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn jar(path: &Path, files: &[(&str, &str)]) {
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (n, b) in files {
            z.start_file::<_, ()>(*n, Default::default()).unwrap();
            z.write_all(b.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }

    #[test]
    fn version_ranges() {
        assert_eq!(accepts("[4.4,)", "4.4.9"), Some(true));
        assert_eq!(accepts("[4.4,)", "4.2.1"), Some(false));
        assert_eq!(accepts("[1.0,2.0)", "2.0"), Some(false));
        assert_eq!(accepts("[1.0,2.0)", "1.9.9"), Some(true));
        assert_eq!(accepts("[21.1.0,)", "21.1.256"), Some(true));
        assert_eq!(accepts(">=0.116.5+1.21.1", "0.115.0+1.21.1"), Some(false));
        assert_eq!(accepts(">=0.116.5+1.21.1", "0.116.7+1.21.1"), Some(true));
        assert_eq!(accepts(">=1.2 <2", "2.1"), Some(false));
        assert_eq!(accepts("~1.2", "1.2.5"), Some(true));
        assert_eq!(accepts("1.21.x", "1.21.1"), Some(true));
        assert_eq!(accepts("*", "anything"), Some(true));
        assert_eq!(accepts("[4.4,)", "1.21.1-4.4.9"), Some(true));
        assert_eq!(accepts("[\">=1.0\", \"<0.5\"]", "1.2"), Some(true));
        // Found on real packs.
        assert_eq!(accepts("[19.2.2-beta,)", "19.2.17"), Some(true));
        assert_eq!(accepts("[1.11.7-, 1.12-)", "1.11.8-1.21.1"), Some(true));
        assert_eq!(accepts("[2.5.10-, 2.6-)", "2.5.10"), Some(true));
        assert_eq!(accepts("[1.0.203-1.21.1-snapshot, )", "1.0.228"), Some(true));
        assert_eq!(accepts("[1.20.1, 1.21)", "1.20.1-450-FORGE"), Some(true));
        assert_eq!(accepts("[1.21.1,)", "1.21.1-1.2.31"), Some(true));
        assert_eq!(accepts("NONE", "2.6.3"), None);
        assert_eq!(accepts("8321864", "6.5.1"), None);
        assert_eq!(accepts("[1.6.0,1.7.0)", "1.7.3+1.21.1"), Some(false));
        assert_eq!(accepts("[1.21.1-2.4.21,1.21.1-2.5.0)", "1.21.1-2.4.21"), Some(true));
        assert_eq!(accepts("[1.21.1-2,1.21.1-3)", "1.21.1-2.2.0"), Some(true));
        assert_eq!(accepts("[1.20.2-1.5,)", "1.20.1-2.7"), Some(true));
        assert_eq!(accepts("1.21.1-1.2.10", "1.3.4"), Some(true));
        assert_eq!(accepts("10.7.19.85", "10.7.19"), None);
    }

    #[test]
    fn sees_what_removing_a_jar_breaks() {
        let t = std::env::temp_dir().join(format!("hopper-depgone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        jar(
            &t.join("needs.jar"),
            &[("META-INF/neoforge.mods.toml", "[[mods]]
modId=\"needs\"
[[dependencies.needs]]
modId=\"geckolib\"
type=\"required\"
versionRange=\"[4,)\"
side=\"BOTH\"
")],
        );
        jar(&t.join("geckolib.jar"), &[("META-INF/neoforge.mods.toml", "[[mods]]
modId=\"geckolib\"
version=\"4.5\"
")]);
        assert!(missing(&t).is_empty());
        let m = missing_without(&t, &["geckolib.jar".into()]);
        assert_eq!(m.len(), 1);
        assert_eq!((m[0].file_name.as_str(), m[0].needs.as_str()), ("needs.jar", "geckolib"));
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn warns_about_a_too_old_dependency() {
        let t = std::env::temp_dir().join(format!("hopper-depver-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        jar(
            &t.join("needs.jar"),
            &[("META-INF/neoforge.mods.toml", "[[mods]]\nmodId=\"needs\"\nversion=\"1.0\"\n[[dependencies.needs]]\nmodId=\"geckolib\"\ntype=\"required\"\nversionRange=\"[4.4,)\"\nside=\"BOTH\"\n")],
        );
        jar(
            &t.join("geckolib.jar"),
            &[("META-INF/neoforge.mods.toml", "[[mods]]\nmodId=\"geckolib\"\nversion=\"4.2.1\"\n")],
        );
        let m = missing(&t);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].needs, "geckolib");
        assert_eq!(m[0].wrong_version, Some(("geckolib.jar".to_string(), "4.2.1".to_string())));
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn finds_missing_required_mods() {
        let dir = std::env::temp_dir().join(format!("hopper-deps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let toml = |id: &str, deps: &str| {
            format!("[[mods]]\nmodId=\"{id}\"\ndisplayName=\"{id} mod\"\n{deps}")
        };
        let req = |owner: &str, id: &str| {
            format!(
                "[[dependencies.{owner}]]\nmodId=\"{id}\"\ntype=\"required\"\nversionRange=\"[1,)\"\nside=\"BOTH\"\n"
            )
        };
        jar(
            &dir.join("addon.jar"),
            &[(
                "META-INF/neoforge.mods.toml",
                &(toml(
                    "addon",
                    &(req("addon", "base")
                        + &req("addon", "lib")
                        + &req("addon", "neoforge")
                        + &req("addon", "gone")),
                )),
            )],
        );
        jar(
            &dir.join("base.jar"),
            &[("META-INF/neoforge.mods.toml", &toml("base", ""))],
        );
        // "lib" only comes bundled inside another jar.
        let nested = dir.join("lib-inner.jar");
        jar(
            &nested,
            &[("META-INF/neoforge.mods.toml", &toml("lib", ""))],
        );
        let inner = std::fs::read(&nested).unwrap();
        std::fs::remove_file(&nested).unwrap();
        {
            let mut z = zip::ZipWriter::new(std::fs::File::create(dir.join("bundle.jar")).unwrap());
            z.start_file::<_, ()>("META-INF/neoforge.mods.toml", Default::default())
                .unwrap();
            z.write_all(toml("bundle", "").as_bytes()).unwrap();
            z.start_file::<_, ()>("META-INF/jarjar/lib-inner.jar", Default::default())
                .unwrap();
            z.write_all(&inner).unwrap();
            z.finish().unwrap();
        }
        // "gone" exists, but disabled.
        jar(
            &dir.join("gone.jar.disabled"),
            &[("META-INF/neoforge.mods.toml", &toml("gone", ""))],
        );

        let m = missing(&dir);
        assert_eq!(m.len(), 1, "{m:?}");
        assert_eq!(m[0].needs, "gone");
        assert_eq!(m[0].mod_name, "addon mod");
        assert_eq!(m[0].disabled_provider.as_deref(), Some("gone.jar.disabled"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_what_the_loader_refuses() {
        let t = std::env::temp_dir().join(format!("hopper-blockers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        let neo = "META-INF/neoforge.mods.toml";
        let head = "modLoader=\"javafml\"\nloaderVersion=\"[1,)\"\n";
        let dep = |owner: &str, id: &str, kind: &str, range: &str| {
            format!("[[dependencies.{owner}]]\nmodId=\"{id}\"\ntype=\"{kind}\"\nversionRange=\"{range}\"\nside=\"BOTH\"\n")
        };
        let m = |id: &str, v: &str| format!("[[mods]]\nmodId=\"{id}\"\nversion=\"{v}\"\n");
        // Made for a newer NeoForge: no modLoader.
        jar(&t.join("towers.jar"), &[(neo, &m("structory_towers", "1.0.17"))]);
        jar(&t.join("almanac.jar"), &[(neo, &(head.to_string() + &m("almanac", "1") + &dep("almanac", "neoforge", "required", "[21.1.262,)")))]);
        jar(&t.join("act.jar"), &[(neo, &(head.to_string() + &m("ae2craftingtime", "1") + &dep("ae2craftingtime", "data_energistics", "optional", "[3.3.5,)")))]);
        jar(&t.join("de.jar"), &[(neo, &(head.to_string() + &m("data_energistics", "1.0.0")))]);
        jar(&t.join("meu.jar"), &[(neo, &(head.to_string() + &m("mekanism_empowered_unleashed", "1") + &dep("mekanism_empowered_unleashed", "mekanism_unleashed", "INCOMPATIBLE", "[0.0.0,)")))]);
        jar(&t.join("mu.jar"), &[(neo, &(head.to_string() + &m("mekanism_unleashed", "1.21.1-0.3.4")))]);
        // Past a range's top end loads fine.
        jar(&t.join("cucumber.jar"), &[(neo, &(head.to_string() + &m("cucumber", "8") + &dep("cucumber", "minecraft", "required", "[1.21,1.21.1)")))]);

        let b = blockers(&t, "neoforge-21.1.259", "1.21.1");
        let got: Vec<(&str, &str)> = b.iter().map(|b| (b.file_name.as_str(), match b.kind {
            BlockerKind::Loader { .. } => "loader",
            BlockerKind::WrongVersion { .. } => "version",
            BlockerKind::Incompatible { .. } => "incompatible",
            BlockerKind::Invalid(_) => "invalid",
        })).collect();
        assert_eq!(got.len(), 4, "{b:#?}");
        for want in [("towers.jar", "invalid"), ("almanac.jar", "loader"), ("act.jar", "version"), ("meu.jar", "incompatible")] {
            assert!(got.contains(&want), "{want:?} not in {got:?}");
        }
        assert!(blockers(&t, "neoforge-21.1.264", "1.21.1").iter().all(|b| b.file_name != "almanac.jar"));
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    #[ignore = "reads the real instances"]
    fn real_blockers() {
        for e in std::fs::read_dir(r"G:\Games\Minecraft\Curse\Instances").unwrap().flatten() {
            let Ok(text) = std::fs::read_to_string(e.path().join("minecraftinstance.json")) else { continue };
            let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            let game = v["gameVersion"].as_str().unwrap_or_default().to_string();
            let loader = v["baseModLoader"]["name"].as_str().unwrap_or_default().to_string();
            let b = blockers(&e.path().join("mods"), &loader, &game);
            println!("{} ({loader}, {game}): {}", e.file_name().to_string_lossy(), b.len());
            for x in &b {
                println!("  {} [{}] {}", x.mod_name, x.file_name, x.text());
            }
        }
    }

    #[test]
    #[ignore = "reads the real instances"]
    fn real_missing() {
        for e in std::fs::read_dir(r"G:\Games\Minecraft\Curse\Instances")
            .unwrap()
            .flatten()
        {
            let t = std::time::Instant::now();
            let m = missing(&e.path().join("mods"));
            println!(
                "{}: {} in {:?}",
                e.file_name().to_string_lossy(),
                m.len(),
                t.elapsed()
            );
            for x in m.iter().take(40) {
                println!(
                    "  {} ({}) needs {} {} {:?} have {:?}",
                    x.mod_name, x.file_name, x.needs, x.versions, x.disabled_provider, x.wrong_version
                );
            }
        }
    }
}
