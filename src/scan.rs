//! Folder scan and jar metadata. Ported from mod-sync's `scanner.go`.

use crate::model::ModInfo;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Metadata files in the order mod-sync trusts them.
const METADATA_PRIORITY: [&str; 4] = [
    "META-INF/neoforge.mods.toml",
    "META-INF/mods.toml",
    "fabric.mod.json",
    "META-INF/MANIFEST.MF",
];

/// Manifest titles that name a build tool or loader, not the mod.
const BAD_MANIFEST_TITLES: [&str; 11] = [
    "jar",
    "neoforge",
    "forge",
    "fabric",
    "minecraft",
    "mod",
    "common",
    "api",
    "core",
    "library",
    "loader",
];

/// Jar metadata keyed by name, size and mtime, kept across runs. Opening every
/// jar over the `Q:` share takes about a minute; a cache hit needs only the
/// directory listing.
static CACHE: Mutex<Option<HashMap<String, ModInfo>>> = Mutex::new(None);

/// Parallel jar reads hide the share's per-file latency on a cold scan.
const SCAN_THREADS: usize = 16;

fn cache_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "Hopper").map(|d| d.cache_dir().join("scan-cache.json"))
}

fn cache_key(m: &ModInfo) -> String {
    format!("{}|{}|{}", m.file_name, m.file_size, m.modified)
}

fn with_cache<T>(f: impl FnOnce(&mut HashMap<String, ModInfo>) -> T) -> T {
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.get_or_insert_with(|| {
        cache_path()
            .and_then(|p| fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    });
    f(cache)
}

fn save_cache() {
    let Some(path) = cache_path() else { return };
    let bytes = with_cache(|c| serde_json::to_vec(c));
    if let Ok(bytes) = bytes {
        let _ = fs::create_dir_all(path.parent().unwrap_or(&path));
        let _ = fs::write(path, bytes);
    }
}

/// Reads every `.jar` directly under `dir`. Jars that fail to open are still
/// listed, named after their file.
pub fn scan_folder(dir: &Path) -> anyhow::Result<Vec<ModInfo>> {
    let mut mods = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let meta = entry.metadata()?;
        if !meta.is_file() || !name.to_ascii_lowercase().ends_with(".jar") {
            continue;
        }
        mods.push(ModInfo {
            file_name: name,
            file_size: meta.len(),
            modified: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64),
            ..Default::default()
        });
    }

    let misses: Vec<usize> = with_cache(|c| {
        (0..mods.len())
            .filter(|&i| match c.get(&cache_key(&mods[i])) {
                Some(hit) => {
                    mods[i] = hit.clone();
                    false
                }
                None => true,
            })
            .collect()
    });

    if !misses.is_empty() {
        let next = AtomicUsize::new(0);
        let read: Vec<(usize, ModInfo)> = std::thread::scope(|s| {
            let workers: Vec<_> = (0..SCAN_THREADS.min(misses.len()))
                .map(|_| {
                    s.spawn(|| {
                        let mut out = Vec::new();
                        while let Some(&i) = misses.get(next.fetch_add(1, Ordering::Relaxed)) {
                            let mut m = mods[i].clone();
                            read_jar_metadata(&dir.join(&m.file_name), &mut m);
                            if m.mod_name.is_empty() {
                                m.mod_name = strip_version_suffix(&m.file_name);
                            }
                            out.push((i, m));
                        }
                        out
                    })
                })
                .collect();
            workers
                .into_iter()
                .flat_map(|w| w.join().unwrap_or_default())
                .collect()
        });
        with_cache(|c| {
            for (_, m) in &read {
                c.insert(cache_key(m), m.clone());
            }
        });
        for (i, m) in read {
            mods[i] = m;
        }
        save_cache();
    }

    mods.sort_by_key(|m| m.mod_name.to_lowercase());
    Ok(mods)
}

fn read_jar_metadata(path: &Path, m: &mut ModInfo) {
    let Ok(file) = fs::File::open(path) else {
        return;
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return;
    };

    for meta in METADATA_PRIORITY {
        let Some(text) = read_entry(&mut zip, meta) else {
            continue;
        };
        match meta {
            "META-INF/neoforge.mods.toml" | "META-INF/mods.toml" => read_mods_toml(&text, m),
            "fabric.mod.json" => read_fabric_json(&text, m),
            _ => read_manifest(&text, m),
        }
        if !m.mod_id.is_empty() {
            break;
        }
    }
    resolve_version(&mut zip, m);
}

fn read_entry(zip: &mut zip::ZipArchive<fs::File>, name: &str) -> Option<String> {
    let mut f = zip.by_name(name).ok()?;
    let mut s = String::new();
    f.by_ref().take(256 * 1024).read_to_string(&mut s).ok()?;
    Some(s)
}

#[derive(Deserialize)]
struct ModsToml {
    #[serde(default)]
    mods: Vec<ModsTomlEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModsTomlEntry {
    #[serde(default)]
    mod_id: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    version: String,
}

fn read_mods_toml(text: &str, m: &mut ModInfo) {
    let Ok(data) = toml::from_str::<ModsToml>(text) else {
        return;
    };
    let Some(first) = data.mods.into_iter().next() else {
        return;
    };
    m.mod_id = first.mod_id;
    m.mod_name = first.display_name;
    m.version = first.version;
}

fn read_fabric_json(text: &str, m: &mut ModInfo) {
    #[derive(Deserialize)]
    struct Fabric {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        version: String,
        #[serde(default)]
        environment: String,
    }
    let Ok(data) = serde_json::from_str::<Fabric>(text) else {
        return;
    };
    m.mod_id = data.id;
    m.mod_name = data.name;
    m.version = data.version;
    m.side = match data.environment.trim().to_ascii_lowercase().as_str() {
        "client" => "client".into(),
        "server" => "server".into(),
        "*" | "both" => "both".into(),
        _ => String::new(),
    };
}

fn read_manifest(text: &str, m: &mut ModInfo) {
    for line in text.lines().map(str::trim) {
        if let Some(t) = line.strip_prefix("Implementation-Title:") {
            let t = t.trim();
            if !BAD_MANIFEST_TITLES.contains(&t.to_ascii_lowercase().as_str()) {
                m.mod_name = t.to_string();
            }
        } else if let Some(v) = line.strip_prefix("Implementation-Version:") {
            m.version = v.trim().to_string();
        }
    }
}

/// mods.toml often says `version="${file.jarVersion}"`; the loader fills that
/// from the manifest at runtime, so we do the same, then fall back to the
/// version in the file name.
fn resolve_version(zip: &mut zip::ZipArchive<fs::File>, m: &mut ModInfo) {
    if !m.version.is_empty() && !m.version.contains("${") {
        return;
    }
    if let Some(text) = read_entry(zip, "META-INF/MANIFEST.MF") {
        let v = text
            .lines()
            .find_map(|l| l.trim().strip_prefix("Implementation-Version:"))
            .map(str::trim)
            .unwrap_or("");
        if !v.is_empty() && !v.contains("${") {
            m.version = v.to_string();
            return;
        }
    }
    let from_name = version_from_file_name(&m.file_name);
    if !from_name.is_empty() || m.version.contains("${") {
        m.version = from_name;
    }
}

pub fn looks_like_version(s: &str) -> bool {
    let Some(c) = s.chars().next() else {
        return false;
    };
    if c.is_ascii_digit() {
        return true;
    }
    let l = s.to_ascii_lowercase();
    // "v1.2", but not a word like "villagers".
    if let Some(rest) = l.strip_prefix('v') {
        return rest.starts_with(|c: char| c.is_ascii_digit());
    }
    ["beta", "alpha", "rc", "pre", "release", "snapshot"]
        .iter()
        .any(|p| l.starts_with(p))
}

fn is_loader_token(s: &str) -> bool {
    matches!(s, "neoforge" | "forge" | "fabric" | "quilt" | "neo")
}

fn is_mc_version_token(s: &str) -> bool {
    s.starts_with("mc1.") || s.starts_with("mc1_")
}

/// A bare MC version like "1.21" / "1.21.1". 1.21 is the last 1.x
/// release, so "1.38.0" is a mod version.
fn looks_like_minecraft_version(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("1.") else {
        return false;
    };
    let minor = rest.split('.').next().unwrap_or("");
    minor.parse::<u32>().is_ok_and(|n| (7..=21).contains(&n))
}

/// "not_enough_glyphs-1.21.1-4.5.1.jar" -> "4.5.1",
/// "ExtendedAE-1.21-2.2.36-neoforge.jar" -> "2.2.36".
pub fn version_from_file_name(file_name: &str) -> String {
    let name = file_name.to_ascii_lowercase();
    let name = name.trim_end_matches(".jar");
    let parts: Vec<&str> = name.split('-').collect();
    let noise =
        |s: &str| is_loader_token(s) || is_mc_version_token(s) || looks_like_minecraft_version(s);
    let Some(start) = (1..parts.len()).find(|&i| looks_like_version(parts[i]) && !noise(parts[i]))
    else {
        return String::new();
    };
    parts[start..]
        .iter()
        .filter(|s| !noise(s))
        .copied()
        .collect::<Vec<_>>()
        .join("-")
}

/// The version to show for a jar: `version_from_file_name` without
/// "+neoforge" / "+mc1.21.1" tags or a " (2)" copy suffix.
/// "minimapshapes_xaero-0.1.0+neoforge-1.21.1.jar" -> "0.1.0".
pub fn short_version(file_name: &str) -> String {
    let v = version_from_file_name(file_name);
    let v = match v.rfind(" (") {
        Some(i) if v.ends_with(')') => &v[..i],
        _ => &v,
    };
    v.split('+')
        .filter(|p| {
            let p = p.strip_prefix("mc").unwrap_or(p);
            !p.is_empty() && !is_loader_token(p) && !looks_like_minecraft_version(p)
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// "journeymap-neoforge-1.21.1-6.0.0-beta.77.jar" -> "journeymap-neoforge".
pub fn strip_version_suffix(file_name: &str) -> String {
    let name = file_name.strip_suffix(".jar").unwrap_or(file_name);
    let parts: Vec<&str> = name.split('-').collect();
    let mut end = parts.len();
    while end > 0 && looks_like_version(parts[end - 1]) {
        end -= 1;
    }
    if end == 0 {
        return parts[0].to_string();
    }
    parts[..end].join("-")
}

/// Numeric-aware version compare. Non-numeric segments compare as text.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let a = a.trim_start_matches('v');
    let b = b.trim_start_matches('v');
    if a == b {
        return Ordering::Equal;
    }
    let ap: Vec<&str> = a.split('.').collect();
    let bp: Vec<&str> = b.split('.').collect();
    for (x, y) in ap.iter().zip(&bp) {
        let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            _ => x.cmp(y),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    ap.len().cmp(&bp.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn version_from_names() {
        assert_eq!(
            version_from_file_name("not_enough_glyphs-1.21.1-4.5.1.jar"),
            "4.5.1"
        );
        assert_eq!(
            version_from_file_name("ExtendedAE-1.21-2.2.36-neoforge.jar"),
            "2.2.36"
        );
        assert_eq!(version_from_file_name("jei.jar"), "");
        assert_eq!(version_from_file_name("easy-villagers-neoforge-1.21.1-1.1.47.jar"), "1.1.47");
        assert_eq!(short_version("minimapshapes_xaero-0.1.0+neoforge-1.21.1.jar"), "0.1.0");
        assert_eq!(short_version("mekanism-enchanting-infuser-compat-1.0.0 (2).jar"), "1.0.0");
        assert_eq!(short_version("netprodis-2.0.0+1.21.jar"), "2.0.0");
        assert_eq!(short_version("mob_grinding_utils-1.1.10+mc1.21.1.jar"), "1.1.10");
        assert_eq!(
            version_from_file_name("integrateddynamics-1.21.1-neoforge-1.38.0-2266.jar"),
            "1.38.0-2266"
        );
    }

    #[test]
    fn strips_trailing_versions() {
        assert_eq!(
            strip_version_suffix("journeymap-neoforge-1.21.1-6.0.0-beta.77.jar"),
            "journeymap-neoforge"
        );
    }

    #[test]
    fn compares_numerically() {
        assert_eq!(compare_versions("1.10.0", "1.9.9"), Ordering::Greater);
        assert_eq!(compare_versions("v2.0", "2.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.2", "1.2.1"), Ordering::Less);
    }
}
