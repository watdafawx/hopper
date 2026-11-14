//! Every settings file of an instance or server, for the Configs tab: where
//! they are, which mod each belongs to, which side reads it, and their
//! settings, read lazily and cached by size and time so a search across
//! all of them (tens of thousands of settings) stays quick.

use crate::configedit::{self, Format, Section};
use crate::configowner::Owners;
use crate::stampcache::{self, StampCache};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

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
        } else if ft.is_file() && Format::of(&name).is_some() && (r.starts_with("defaultconfigs/") || !crate::packdiff::pack_content(&r)) {
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

/// Read files: path -> (size and time, text and settings).
static CACHE: LazyLock<Mutex<HashMap<PathBuf, (Option<stampcache::Stamp>, Parsed)>>> = LazyLock::new(Default::default);

/// The text and settings of `root`/`rel`, cached until the file changes.
pub fn read(root: &Path, rel: &str) -> Parsed {
    let stamp = std::fs::metadata(root.join(rel)).ok().map(|m| stampcache::stamp(&m));
    read_at(root, rel, stamp)
}

/// Each of `rels`' size and time, one folder listing at a time
/// ([`stampcache::by_folder`]): this runs on every search.
pub fn stamps(root: &Path, rels: &[String]) -> Vec<Option<stampcache::Stamp>> {
    let paths: Vec<PathBuf> = rels.iter().map(|r| root.join(r)).collect();
    stampcache::by_folder(&paths)
}

/// [`read`] with the file's size and time already known (`None`: it's gone).
fn read_at(root: &Path, rel: &str, stamp: Option<stampcache::Stamp>) -> Parsed {
    let path = root.join(rel);
    let cache = || CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, p)) = cache().get(&path)
        && *at == stamp
    {
        return p.clone();
    }
    let parsed: Parsed = Arc::new(match (stamp, Format::of(rel)) {
        (None, _) => Err("The file is gone.".into()),
        (_, None) => Err("Hopper can't read this kind of file as settings.".into()),
        (Some((len, _)), _) if len > MAX_BYTES => Err("This file is too big to edit here.".into()),
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
    c.insert(path, (stamp, parsed.clone()));
    parsed
}

/// Runs `f` on every item on all cores, the results in the items' order.
/// Items are handed out in small runs, so a few big files don't keep one
/// thread busy while the rest sit idle. An item whose `f` panics is left
/// out rather than taking the rest down.
pub fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    // More threads than this only wait on the disk (timed on 1,100 jars
    // and 5,000 configs).
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8).min(items.len());
    let one = |it: &T| std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(it))).ok();
    if threads <= 1 {
        return items.iter().filter_map(one).collect();
    }
    let next = AtomicUsize::new(0);
    // Neighbours together (files of one folder read back to back are
    // quicker), in small enough runs that the threads finish together.
    let batch = (items.len() / (threads * 8)).max(1);
    let mut got: Vec<(usize, R)> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut got = Vec::new();
                    loop {
                        let start = next.fetch_add(batch, Ordering::Relaxed);
                        if start >= items.len() {
                            break;
                        }
                        for (i, it) in items.iter().enumerate().skip(start).take(batch) {
                            if let Some(r) = one(it) {
                                got.push((i, r));
                            }
                        }
                    }
                    got
                })
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    got.sort_unstable_by_key(|(i, _)| *i);
    got.into_iter().map(|(_, r)| r).collect()
}

/// Reads many files on all cores (the search across every mod).
pub fn read_all(root: &Path, rels: &[String]) -> Vec<(String, Parsed)> {
    let st = stamps(root, rels);
    let items: Vec<(&String, Option<stampcache::Stamp>)> = rels.iter().zip(st).collect();
    par_map(&items, |(r, st)| ((*r).clone(), read_at(root, r, *st)))
}

/// How a file reads as settings, without its settings: what the mod list
/// and its counts need.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Summary {
    /// Settings in it (0 when it doesn't read as settings).
    pub settings: usize,
    /// Settings not at the default the file states.
    pub changed: usize,
    /// Why it doesn't read as settings.
    pub error: Option<String>,
}

impl Summary {
    pub fn of(p: &Result<(String, Vec<Section>), String>) -> Summary {
        match p {
            Ok((_, s)) => Summary { settings: s.iter().map(|x| x.entries.len()).sum(), changed: changed_count(s), error: None },
            Err(e) => Summary { error: Some(e.clone()), ..Summary::default() },
        }
    }
}

/// Every file's counts, kept on disk by path, size and time: the second
/// open (even after a restart) parses only the files that changed.
static SUMMARIES: LazyLock<Mutex<StampCache<Summary>>> = LazyLock::new(|| Mutex::new(StampCache::named("config-counts.json", SUMMARY_FORMAT, 60_000)));

/// The format of what `config-counts.json` holds. Bump it when the parsing
/// of config files or the way [`Summary`] counts changes (so old counts are
/// worked out again); not for a new Hopper version, or every update would
/// parse every config again.
const SUMMARY_FORMAT: u32 = 1;

/// The [`Summary`] of each of `rels`, in order: from the cache when the
/// file hasn't changed, else read (and kept for [`read`] and [`search`]).
pub fn summaries(root: &Path, rels: &[String]) -> Vec<(String, Summary)> {
    summaries_in(&SUMMARIES, root, rels)
}

fn summaries_in(cache: &Mutex<StampCache<Summary>>, root: &Path, rels: &[String]) -> Vec<(String, Summary)> {
    let lock = || cache.lock().unwrap_or_else(|e| e.into_inner());
    let stamps = stamps(root, rels);
    let known: Vec<Option<Summary>> = {
        let mut c = lock();
        rels.iter().zip(&stamps).map(|(r, st)| c.get(&stampcache::key(&root.join(r)), (*st)?)).collect()
    };
    let missing: Vec<usize> = (0..rels.len()).filter(|&i| known[i].is_none()).collect();
    let fresh: Vec<(usize, Summary)> = par_map(&missing, |&i| (i, Summary::of(&read_at(root, &rels[i], stamps[i]))));
    let mut out: Vec<Option<Summary>> = known;
    {
        let mut c = lock();
        for (i, s) in fresh {
            // A file that's gone isn't kept: it has no stamp to check.
            if let Some(st) = stamps[i] {
                c.put(stampcache::key(&root.join(&rels[i])), st, s.clone());
            }
            out[i] = Some(s);
        }
        c.save();
    }
    rels.iter().cloned().zip(out.into_iter().map(Option::unwrap_or_default)).collect()
}

/// Reads and indexes every file for [`search`] ahead of the first query
/// (off the UI thread, after the list is up).
pub fn warm(root: &Path, rels: &[String]) {
    let st = stamps(root, rels);
    let items: Vec<(&String, Option<stampcache::Stamp>)> = rels.iter().zip(st).collect();
    par_map(&items, |(r, st)| {
        hay(root, r, *st);
    });
}

/// A setting found by [`search`].
#[derive(Clone, Debug)]
pub struct Hit {
    pub rel: String,
    pub entry: configedit::Entry,
}

/// Each setting of a file, lowercased for search once instead of on every
/// query: its friendly name, key and table, and its description.
struct Hay {
    name: Vec<String>,
    desc: Vec<String>,
}

/// The search text of read files, kept while [`read`] hands back the same parse.
static HAYS: LazyLock<Mutex<HashMap<PathBuf, (Parsed, Arc<Hay>)>>> = LazyLock::new(Default::default);

fn hay(root: &Path, rel: &str, stamp: Option<stampcache::Stamp>) -> (Parsed, Option<Arc<Hay>>) {
    let p = read_at(root, rel, stamp);
    let Ok((_, sections)) = &*p else { return (p, None) };
    let path = root.join(rel);
    let lock = || HAYS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((q, h)) = lock().get(&path)
        && Arc::ptr_eq(q, &p)
    {
        return (p.clone(), Some(h.clone()));
    }
    let entries = sections.iter().flat_map(|s| &s.entries);
    let h = Arc::new(Hay {
        name: entries.clone().map(|e| format!("{} {} {}", e.label(), e.key, e.path.join(" ")).to_lowercase()).collect(),
        desc: entries.map(|e| e.comment.to_lowercase()).collect(),
    });
    let mut c = lock();
    if c.len() > 8000 {
        c.clear();
    }
    c.insert(path, (p.clone(), h.clone()));
    (p, Some(h))
}

/// Settings whose friendly name or key (and, with `desc`, description)
/// holds every word of `query`, file by file in the order of `rels`.
pub fn search(root: &Path, rels: &[String], query: &str, desc: bool, limit: usize) -> Vec<Hit> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Vec::new();
    }
    // A word never holds a space, so being in the name or in the
    // description is the same as being in both joined by one.
    let st = stamps(root, rels);
    let items: Vec<(&String, Option<stampcache::Stamp>)> = rels.iter().zip(st).collect();
    let per_file = par_map(&items, |&(rel, st)| {
        let (p, Some(h)) = hay(root, rel, st) else { return Vec::new() };
        let Ok((_, sections)) = &*p else { return Vec::new() };
        let mut out = Vec::new();
        for (i, e) in sections.iter().flat_map(|s| &s.entries).enumerate() {
            if words.iter().all(|w| h.name[i].contains(w.as_str()) || (desc && h.desc[i].contains(w.as_str()))) {
                out.push(Hit { rel: rel.clone(), entry: e.clone() });
                if out.len() >= limit {
                    break;
                }
            }
        }
        out
    });
    per_file.into_iter().flatten().take(limit).collect()
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

    #[test]
    fn a_server_folder_lists_all_three_places() {
        // A dedicated server: config/, defaultconfigs/ and the world named
        // in server.properties, all owned by the same mod.
        let root = std::env::temp_dir().join(format!("hopper-cfgindex-srv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let w = |rel: &str, t: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        };
        w("server.properties", "level-name=survival
");
        w("config/create-server.toml", "a = 1
");
        w("defaultconfigs/create-server.toml", "a = 1
");
        w("survival/serverconfig/create-server.toml", "a = 2
");
        w("world/serverconfig/create-server.toml", "a = 3
");
        let rels: Vec<String> = list(&root).into_iter().map(|(r, _)| r).collect();
        assert_eq!(rels, ["config/create-server.toml", "defaultconfigs/create-server.toml", "survival/serverconfig/create-server.toml"]);
        let owners = crate::configowner::Owners::new(vec![crate::configowner::ModRef { key: "create.jar".into(), ids: vec!["create".into()], name: "Create".into() }]);
        let files = index(&root, &owners);
        assert!(files.iter().all(|f| f.owner.as_deref() == Some("create.jar")), "{files:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The search before the per-file search text was cached: one
    /// lowercased string per setting, built on every query.
    fn search_before(root: &Path, rels: &[String], query: &str, desc: bool, limit: usize) -> Vec<(String, usize)> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if words.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for r in rels {
            let p = read(root, r);
            let Ok((_, sections)) = &*p else { continue };
            for e in sections.iter().flat_map(|s| &s.entries) {
                let hay = format!("{} {} {}{}", e.label(), e.key, e.path.join(" "), if desc { format!(" {}", e.comment) } else { String::new() }).to_lowercase();
                if words.iter().all(|w| hay.contains(w.as_str())) {
                    out.push((r.clone(), e.line));
                    if out.len() >= limit {
                        return out;
                    }
                }
            }
        }
        out
    }

    /// The counts before they were cached: every file read and parsed.
    fn counts_before(root: &Path, rels: &[String]) -> Vec<(String, Summary)> {
        rels.iter().map(|r| (r.clone(), Summary::of(&read(root, r)))).collect()
    }

    /// The cached counts and the faster search give what reading every
    /// file did, in every format, also after files change or go.
    #[test]
    fn cached_counts_and_search_match_reading_everything() {
        let root = std::env::temp_dir().join(format!("hopper-cfgsame-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let w = |rel: &str, t: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        };
        w("config/create-client.toml", "[client]\n#Higher density means more particles.\n#Range: 0.0 ~ 1.0\n#Default: 0.5\nfanParticleDensity = 0.2\n#Default: true\nenableSpawnRate = true\n");
        w("config/mekanism/general.toml", "[general]\n\t#Max energy per tick.\n\t#Default: 100\n\tmaxEnergy = 250\n");
        w("config/oldmod.cfg", "general {\n    # Spawn rate of ores [default: 4]\n    I:spawnRate=8\n    B:enabled=true\n}\n");
        w("config/ftbquests/client.snbt", "{\n\t# Spawn particles\n\tparticles: true\n\tmax_rate: 3\n}\n");
        w("config/jei/jei-client.json5", "{\n  // Max items per page\n  \"maxItems\": 50,\n  \"spawn\": { \"rate\": 2 }\n}\n");
        w("config/simple.properties", "# Spawn rate\nspawn-rate=4\nmax-players=10\n");
        w("config/flat.yml", "spawnRate: 3\nenabled: false\n");
        w("config/bad.json", "{ not json");
        w("config/binary.toml", "\u{0}\u{1}");
        w("defaultconfigs/create-server.toml", "#Default: 20\nmaxBeltLength = 20\n");
        w("saves/T/serverconfig/create-server.toml", "#Default: 20\nmaxBeltLength = 30\n");
        std::fs::write(root.join("config/binary.toml"), [0xff, 0xfe, 0x00]).unwrap();
        let rels: Vec<String> = list(&root).into_iter().map(|(r, _)| r).collect();
        assert!(rels.len() >= 10, "{rels:?}");

        let cache = Mutex::new(StampCache::load(Some(root.join("cache/counts.json")), SUMMARY_FORMAT, 100));
        let check = |rels: &[String], why: &str| {
            assert_eq!(summaries_in(&cache, &root, rels), counts_before(&root, rels), "counts, {why}");
            for q in ["spawn rate", "max", "SPAWN", "particles", "rate 2", "a", "nothing-here", "  "] {
                for desc in [false, true] {
                    for limit in [1, 3, 1000] {
                        let now: Vec<(String, usize)> = search(&root, rels, q, desc, limit).into_iter().map(|h| (h.rel, h.entry.line)).collect();
                        assert_eq!(now, search_before(&root, rels, q, desc, limit), "search {q:?} desc={desc} limit={limit}, {why}");
                    }
                }
            }
        };
        check(&rels, "first open");
        assert!(root.join("cache/counts.json").is_file(), "the counts are kept on disk");
        // A fresh run reads the counts from the file.
        let cache2 = Mutex::new(StampCache::<Summary>::load(Some(root.join("cache/counts.json")), SUMMARY_FORMAT, 100));
        assert_eq!(cache2.lock().unwrap().len(), rels.iter().filter(|r| root.join(r).is_file()).count());
        check(&rels, "second open");
        // A changed file (other size) and a removed one are read again.
        w("config/create-client.toml", "[client]\n#Default: 0.5\nfanParticleDensity = 0.5\n#Default: true\nenableSpawnRate = false\nextraSpawnRate = 1\n");
        std::fs::remove_file(root.join("config/simple.properties")).unwrap();
        check(&rels, "after changes");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn par_map_keeps_order() {
        let items: Vec<usize> = (0..10_000).collect();
        assert_eq!(par_map(&items, |i| i * 2), items.iter().map(|i| i * 2).collect::<Vec<_>>());
        assert!(par_map(&Vec::<usize>::new(), |i| *i).is_empty());
        // One that panics is left out, the rest come back.
        let got = par_map(&items[..100], |&i| if i == 7 { panic!("boom") } else { i });
        assert_eq!(got.len(), 99);
        assert!(got.windows(2).all(|w| w[0] < w[1]));
    }
}
