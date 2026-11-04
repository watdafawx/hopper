//! Every settings file of an instance or server, for the Configs tab: where
//! they are, which mod each belongs to, which side reads it, and their
//! settings, read lazily and cached by size and time so a search across
//! all of them (tens of thousands of settings) stays quick.

use crate::configedit::{self, Format, Section};
use crate::configowner::Owners;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::SystemTime;

/// Files past this size aren't settings (or are too big to edit here).
pub const MAX_BYTES: u64 = 1 << 20;

/// Who reads a config file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Side {
    /// Only this PC (`-client`).
    Client,
    /// Both the game and the server, each its own copy (`-common`).
    Common,
    /// The server decides and sends it to players (`-server`, `serverconfig/`).
    Server,
    /// Read once at start (`-startup`).
    Startup,
    /// The name doesn't say.
    Unknown,
}

impl Side {
    /// The chip the editor shows.
    pub fn label(self) -> &'static str {
        match self {
            Side::Client => "This PC",
            Side::Common => "Both",
            Side::Server => "Server decides",
            Side::Startup => "At start",
            Side::Unknown => "",
        }
    }
}

/// Which side reads `rel`, from its name and folder.
pub fn side_of(rel: &str) -> Side {
    let l = rel.to_lowercase();
    if l.contains("serverconfig/") {
        return Side::Server;
    }
    let name = l.rsplit('/').next().unwrap_or(&l);
    let has = |w: &str| name.contains(&format!("-{w}")) || name.contains(&format!("_{w}")) || name.starts_with(&format!("{w}.")) || l.contains(&format!("/{w}/"));
    if has("client") {
        Side::Client
    } else if has("server") {
        Side::Server
    } else if has("common") {
        Side::Common
    } else if has("startup") {
        Side::Startup
    } else {
        Side::Unknown
    }
}

/// One settings file.
#[derive(Clone, Debug)]
pub struct CfgFile {
    /// Relative to the instance, with `/`.
    pub rel: String,
    pub format: Option<Format>,
    pub side: Side,
    pub size: u64,
    /// The owning mod's key (its jar's file name), when one matched.
    pub owner: Option<String>,
}

/// Names that are never settings: backups mods leave, and Hopper's own temp files.
fn junk(name: &str) -> bool {
    let l = name.to_lowercase();
    [".bak", ".old", ".migrated", ".backup", ".hopper-tmp", ".tmp", ".lock"].iter().any(|e| l.ends_with(e)) || l.contains(".bak.") || l.starts_with('.')
}

/// Folders inside `config/` that hold data or caches, not settings.
const SKIP_DIRS: &[&str] = &["spark", "paxi", "super_resolution", "backups", "cache", "logs", "openloader", "kubejs", "resourcepacks", "datapacks", "fancymenu/assets"];

fn walk(root: &Path, dir: &Path, rel: &str, depth: usize, out: &mut Vec<(String, u64)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let r = format!("{rel}{name}");
        let Ok(ft) = e.file_type() else { continue };
        // Links aren't followed: one could lead outside, or round in a loop.
        if ft.is_symlink() || junk(&name) {
            continue;
        }
        if ft.is_dir() {
            let low = r.to_lowercase();
            if depth < 6 && !SKIP_DIRS.iter().any(|s| low.ends_with(&format!("/{s}")) || low == format!("config/{s}")) {
                walk(root, &e.path(), &format!("{r}/"), depth + 1, out);
            }
        } else if ft.is_file() && Format::of(&name).is_some() && !crate::packdiff::pack_content(&r) {
            let size = e.metadata().map_or(0, |m| m.len());
            if size <= MAX_BYTES {
                out.push((r, size));
            }
        }
    }
}

/// Every settings file under `root`: `config/`, `defaultconfigs/`,
/// `kubejs/config/` and each world's `serverconfig/` (a server's `world/`
/// too), sorted.
pub fn list(root: &Path) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    for base in ["config/", "defaultconfigs/", "kubejs/config/"] {
        walk(root, &root.join(base), base, 0, &mut out);
    }
    let mut worlds: Vec<String> = Vec::new();
    for e in std::fs::read_dir(root.join("saves")).into_iter().flatten().flatten() {
        if e.path().join("serverconfig").is_dir() {
            worlds.push(format!("saves/{}/serverconfig/", e.file_name().to_string_lossy()));
        }
    }
    // A server's world, named in server.properties (`world` by default).
    let level = std::fs::read_to_string(root.join("server.properties"))
        .ok()
        .and_then(|t| t.lines().find_map(|l| l.strip_prefix("level-name=").map(|s| s.trim().to_string())))
        .unwrap_or_else(|| "world".into());
    if crate::configdiff::safe_rel(&level) && root.join(&level).join("serverconfig").is_dir() {
        worlds.push(format!("{level}/serverconfig/"));
    }
    for w in worlds {
        walk(root, &root.join(&w), &w, 0, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

/// The files with their owners and sides.
pub fn index(root: &Path, owners: &Owners) -> Vec<CfgFile> {
    list(root)
        .into_iter()
        .map(|(rel, size)| CfgFile {
            format: Format::of(&rel),
            side: side_of(&rel),
            owner: owners.owner(&rel).map(|m| m.key.clone()),
            rel,
            size,
        })
        .collect()
}

type Parsed = Arc<Result<(String, Vec<Section>), String>>;

/// Read files: path -> (size, time, text and settings).
static CACHE: LazyLock<Mutex<HashMap<PathBuf, (u64, Option<SystemTime>, Parsed)>>> = LazyLock::new(Default::default);

/// The text and settings of `root`/`rel`, cached until the file changes.
pub fn read(root: &Path, rel: &str) -> Parsed {
    let path = root.join(rel);
    let meta = std::fs::metadata(&path).ok();
    let stamp = (meta.as_ref().map_or(0, |m| m.len()), meta.as_ref().and_then(|m| m.modified().ok()));
    let cache = || CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((len, at, p)) = cache().get(&path)
        && (*len, *at) == stamp
    {
        return p.clone();
    }
    let parsed: Parsed = Arc::new(match (meta, Format::of(rel)) {
        (None, _) => Err("The file is gone.".into()),
        (_, None) => Err("Hopper can't read this kind of file as settings.".into()),
        (Some(m), _) if m.len() > MAX_BYTES => Err("This file is too big to edit here.".into()),
        (Some(_), Some(f)) => match std::fs::read(&path) {
            Err(e) => Err(format!("Couldn't read it: {e}")),
            Ok(b) => match String::from_utf8(b) {
                Err(_) => Err("This file isn't plain text.".into()),
                Ok(text) => configedit::parse_as(f, &text).map(|s| (text, s)),
            },
        },
    });
    let mut c = cache();
    // Keep it bounded: a few instances' worth.
    if c.len() > 8000 {
        c.clear();
    }
    c.insert(path, (stamp.0, stamp.1, parsed.clone()));
    parsed
}

/// Reads many files on a few threads (the search across every mod).
pub fn read_all(root: &Path, rels: &[String]) -> Vec<(String, Parsed)> {
    let chunk = rels.len().div_ceil(8).max(1);
    std::thread::scope(|s| {
        let parts: Vec<_> = rels.chunks(chunk).map(|c| s.spawn(move || c.iter().map(|r| (r.clone(), read(root, r))).collect::<Vec<_>>())).collect();
        parts.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    })
}

/// A setting found by [`search`].
#[derive(Clone, Debug)]
pub struct Hit {
    pub rel: String,
    pub entry: configedit::Entry,
}

/// Settings whose friendly name or key (and, with `desc`, description)
/// holds every word of `query`.
pub fn search(root: &Path, rels: &[String], query: &str, desc: bool, limit: usize) -> Vec<Hit> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (rel, p) in read_all(root, rels) {
        let Ok((_, sections)) = &*p else { continue };
        for e in sections.iter().flat_map(|s| &s.entries) {
            let hay = format!("{} {} {}{}", e.label(), e.key, e.path.join(" "), if desc { format!(" {}", e.comment) } else { String::new() }).to_lowercase();
            if words.iter().all(|w| hay.contains(w.as_str())) {
                out.push(Hit { rel: rel.clone(), entry: e.clone() });
                if out.len() >= limit {
                    return out;
                }
            }
        }
    }
    out
}

/// How many settings in a file aren't at their default (the file says one).
pub fn changed_count(sections: &[Section]) -> usize {
    sections.iter().flat_map(|s| &s.entries).filter(|e| e.is_default() == Some(false)).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sides() {
        assert_eq!(side_of("config/create-client.toml"), Side::Client);
        assert_eq!(side_of("config/create-common.toml"), Side::Common);
        assert_eq!(side_of("config/create-server.toml"), Side::Server);
        assert_eq!(side_of("saves/T/serverconfig/ae2-server.toml"), Side::Server);
        assert_eq!(side_of("config/mekanism/client.toml"), Side::Client);
        assert_eq!(side_of("config/jei/jei-client.ini"), Side::Client);
        assert_eq!(side_of("config/async.toml"), Side::Unknown);
    }

    #[test]
    fn lists_and_searches() {
        let root = std::env::temp_dir().join(format!("hopper-cfgindex-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let w = |rel: &str, t: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        };
        w("config/create-client.toml", "[client]\n#Higher density means more particles.\n#Default: 0.5\nfanParticleDensity = 0.2\n");
        w("config/create-client.toml.bak", "x");
        w("config/spark/data.json", "{}");
        w("config/readme.txt", "x");
        w("saves/T/serverconfig/create-server.toml", "maxBeltLength = 20\n");
        let rels: Vec<String> = list(&root).into_iter().map(|(r, _)| r).collect();
        assert_eq!(rels, ["config/create-client.toml", "saves/T/serverconfig/create-server.toml"]);
        let hits = search(&root, &rels, "fan density", false, 10);
        assert_eq!(hits.len(), 1);
        assert!(search(&root, &rels, "particles", false, 10).is_empty());
        assert_eq!(search(&root, &rels, "particles", true, 10).len(), 1);
        let p = read(&root, "config/create-client.toml");
        assert_eq!(changed_count(&p.as_ref().as_ref().unwrap().1), 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
