//! Where a mod runs, for every list that shows it: CurseForge's Client /
//! Server tag on the installed file, else what Modrinth says about the same
//! mod, else what the jar itself declares. Many authors tag the side on
//! Modrinth only (Inventory Profiles Next's CurseForge files carry just a
//! version and a loader), and a mod that lives on GitHub alone has neither,
//! so without the fallbacks those mods show a question mark.
//!
//! The Modrinth answers come from [`crate::modrinth::sides_of`] (the jar's
//! sha1) and [`crate::modrinth::sides_by_slug`] (the same project by slug),
//! the lookups server sync uses, with their disk caches. The jar's answer
//! ([`crate::deps::jar_side`]) needs no network and only fills what both
//! sites leave open. Lists never look up on render: they read what is known
//! here and call [`look_up`] off the UI thread, then redraw when
//! [`generation`] moves.

use crate::instances::{Addon, Instance};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Who said where a mod runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum From {
    CurseForge,
    Modrinth,
    /// The jar's own metadata (mods.toml dependencies, fabric.mod.json).
    Jar,
}

impl From {
    /// "from Modrinth" / "from the jar" for tooltips; `None` for CurseForge,
    /// the default source that needs no note.
    pub fn note(self) -> Option<&'static str> {
        match self {
            From::CurseForge => None,
            From::Modrinth => Some("from Modrinth"),
            From::Jar => Some("from the jar"),
        }
    }
}

/// A mod's side: "client" | "server" | "both", and who said so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Side {
    pub side: String,
    pub from: From,
}

impl Side {
    pub fn is(&self, side: &str) -> bool {
        self.side == side
    }

    /// The side in words, saying when it didn't come from CurseForge
    /// ("Client only (from Modrinth)", "(from the jar)"), for tooltips and
    /// the hover card.
    pub fn words(&self) -> String {
        let base = label(&self.side);
        match self.from.note() {
            None => base.to_string(),
            Some(note) => format!("{base} ({note})"),
        }
    }
}

/// "Client only" / "Server only" / "Client and server".
pub fn label(side: &str) -> &'static str {
    match side {
        "client" => "Client only",
        "server" => "Server only",
        "both" => "Client and server",
        _ => "",
    }
}

fn valid(s: &str) -> bool {
    matches!(s, "client" | "server" | "both")
}

/// CurseForge's tag wins; Modrinth's only fills a gap. `None` when neither
/// says.
pub fn resolve(cf_env: &str, modrinth: Option<&str>) -> Option<Side> {
    resolve_with(cf_env, modrinth, None)
}

/// CurseForge's tag, else Modrinth's, else the jar's own word. Each only
/// fills what the one before leaves open.
pub fn resolve_with(cf_env: &str, modrinth: Option<&str>, jar: Option<&str>) -> Option<Side> {
    if valid(cf_env) {
        return Some(Side { side: cf_env.to_string(), from: From::CurseForge });
    }
    modrinth
        .filter(|s| valid(s))
        .map(|s| Side { side: s.to_string(), from: From::Modrinth })
        .or_else(|| jar.filter(|s| valid(s)).map(|s| Side { side: s.to_string(), from: From::Jar }))
}

/// What Modrinth said, by [`key`]: "" when it was asked and didn't know.
static KNOWN: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
/// What the jars said, by [`jar_key`]: "" when the jar doesn't say. Per file
/// name, so an updated jar is read again.
static JAR_KNOWN: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
/// Each jar read so far, by path, with the size and time it had, so a jar
/// isn't opened again unless it changed (the same jar seen by the lists and
/// by server sync).
static JAR_READ: Mutex<Option<HashMap<PathBuf, (u64, u64, String)>>> = Mutex::new(None);
/// Bumped whenever [`KNOWN`] gains an answer, so lists know to redraw.
static GENERATION: AtomicU64 = AtomicU64::new(0);
static RUNNING: AtomicBool = AtomicBool::new(false);

/// One key per mod: its project when it has one (the side is the
/// project's), else the jar's name.
fn key(a: &Addon) -> String {
    if a.project_id != 0 {
        format!("p{}", a.project_id)
    } else {
        format!("f{}", a.file_name.trim_end_matches(".disabled").to_lowercase())
    }
}

/// The jar's name, with no `.disabled`, for [`JAR_KNOWN`].
fn jar_key(a: &Addon) -> String {
    a.file_name.trim_end_matches(".disabled").to_lowercase()
}

/// Moves each time new answers (Modrinth's or a jar's) land.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// The side the jar at `path` declares ("client" | "server"), "" when it
/// doesn't say. Opens the jar once per size and time, then answers from
/// memory. Blocking disk: not for the UI thread.
pub fn jar_side(path: &Path) -> String {
    jar_side_with(path, &|p| crate::deps::jar_side(p).map(str::to_string))
}

fn jar_side_with(path: &Path, read: &dyn Fn(&Path) -> Option<String>) -> String {
    let Ok(md) = std::fs::metadata(path) else { return String::new() };
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64);
    let stamp = (md.len(), mtime);
    if let Some((len, at, side)) = JAR_READ
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(path))
        && (*len, *at) == stamp
    {
        return side.clone();
    }
    let side = read(path).unwrap_or_default();
    JAR_READ
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(path.to_path_buf(), (stamp.0, stamp.1, side.clone()));
    side
}

/// The side a list shows for this mod, without any network or disk: the
/// CurseForge tag, else Modrinth's answer, else the jar's, if [`look_up`]
/// has one.
pub fn side_of(a: &Addon) -> Option<Side> {
    if valid(&a.env) {
        return resolve(&a.env, None);
    }
    let g = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    let mr = g.as_ref().and_then(|m| m.get(&key(a))).map(String::as_str);
    let j = JAR_KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    let jar = j.as_ref().and_then(|m| m.get(&jar_key(a))).map(String::as_str);
    resolve_with(&a.env, mr, jar)
}

/// Shorthand: the side's value, "" when unknown (for sorting and filters
/// that compare `env` strings).
pub fn env_of(a: &Addon) -> String {
    side_of(a).map(|s| s.side).unwrap_or_default()
}

/// Mods in this instance that need asking: untagged, in mods/, with
/// Modrinth not asked yet this run (`true`), or asked without an answer and
/// the jar not read yet (`false`).
fn wanted<'a>(inst: &'a Instance) -> Vec<(&'a Addon, bool)> {
    let g = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    let j = JAR_KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    inst.addons
        .iter()
        .filter(|a| a.folder() == "mods" && !valid(&a.env))
        .filter_map(|a| {
            let asked = g.as_ref().and_then(|m| m.get(&key(a)));
            let read = j.as_ref().is_some_and(|m| m.contains_key(&jar_key(a)));
            match asked {
                None => Some((a, true)),
                Some(s) if s.is_empty() && !read => Some((a, false)),
                Some(_) => None,
            }
        })
        .collect()
}

/// Whether some of this instance's mods still need asking.
pub fn needs_look_up(inst: &Instance) -> bool {
    !wanted(inst).is_empty()
}

/// Asks Modrinth about this instance's untagged mods, then reads the jars
/// Modrinth has nothing on (blocking: run it off the UI thread). Jars
/// Modrinth has by sha1 first, then the rest by their CurseForge slug. Both
/// lookups cache on disk, misses too, and a jar is opened once per size and
/// time, so only mods never asked before cost anything. `true` when
/// anything new is known.
pub fn look_up(inst: &Instance) -> bool {
    look_up_with(inst, &crate::modrinth::sides_of, &crate::modrinth::sides_by_slug, &|p| {
        Some(jar_side(p)).filter(|s| !s.is_empty())
    })
}

fn look_up_with(
    inst: &Instance,
    sides_of: &dyn Fn(&[PathBuf]) -> HashMap<String, String>,
    sides_by_slug: &dyn Fn(&[(String, String)]) -> HashMap<String, String>,
    jar_side: &dyn Fn(&Path) -> Option<String>,
) -> bool {
    let all: Vec<(Addon, bool)> = wanted(inst).into_iter().map(|(a, ask)| (a.clone(), ask)).collect();
    if all.is_empty() {
        return false;
    }
    let dir = inst.mods_dir();
    let todo: Vec<&Addon> = all.iter().filter(|(_, ask)| *ask).map(|(a, _)| a).collect();
    let paths: Vec<PathBuf> = todo.iter().map(|a| dir.join(&a.file_name)).filter(|p| p.is_file()).collect();
    let by_file = if paths.is_empty() { HashMap::new() } else { sides_of(&paths) };
    let mut found: HashMap<String, String> = HashMap::new();
    let mut by_slug: Vec<(String, String)> = Vec::new();
    for a in &todo {
        if let Some(s) = by_file.get(&a.file_name) {
            found.insert(key(a), s.clone());
        } else if let Some(slug) = crate::serverpack::cf_slug(&a.page) {
            by_slug.push((slug.to_string(), a.name.clone()));
        }
    }
    if !by_slug.is_empty() {
        let sides = sides_by_slug(&by_slug);
        for a in &todo {
            if let Some(s) = crate::serverpack::cf_slug(&a.page).and_then(|slug| sides.get(slug)) {
                found.entry(key(a)).or_insert_with(|| s.clone());
            }
        }
    }
    // The jar: only for mods Modrinth had nothing on, and only a jar that is
    // there. Read before the lock, as it opens a zip.
    let mut from_jar: HashMap<String, String> = HashMap::new();
    for (a, _) in &all {
        let p = dir.join(&a.file_name);
        if found.get(&key(a)).is_none_or(|s| s.is_empty()) && p.is_file() {
            from_jar.insert(jar_key(a), jar_side(&p).filter(|s| valid(s)).unwrap_or_default());
        }
    }
    let mut new = false;
    {
        let mut g = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
        let m = g.get_or_insert_with(HashMap::new);
        for a in &todo {
            let side = found.remove(&key(a)).unwrap_or_default();
            new |= !side.is_empty();
            m.insert(key(a), side);
        }
    }
    {
        let mut g = JAR_KNOWN.lock().unwrap_or_else(|e| e.into_inner());
        let m = g.get_or_insert_with(HashMap::new);
        for (k, side) in from_jar {
            new |= !side.is_empty();
            m.insert(k, side);
        }
    }
    if new {
        GENERATION.fetch_add(1, Ordering::Relaxed);
    }
    new
}

/// [`look_up`] on a background thread, one at a time. `true` while a
/// lookup is running or was just started, so a list can ask again shortly
/// (Hopper Server's web UI polls).
pub fn look_up_in_background(inst: &Instance) -> bool {
    if needs_look_up(inst) && !RUNNING.swap(true, Ordering::SeqCst) {
        let inst = inst.clone();
        std::thread::spawn(move || {
            look_up(&inst);
            RUNNING.store(false, Ordering::SeqCst);
        });
        return true;
    }
    RUNNING.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addon(pid: u64, file: &str, env: &str, page: &str) -> Addon {
        Addon {
            project_id: pid,
            file_name: file.into(),
            env: env.into(),
            page: page.into(),
            name: file.trim_end_matches(".jar").into(),
            class_id: crate::cf::CLASS_MODS,
            ..Default::default()
        }
    }

    #[test]
    fn curseforge_tag_wins_and_modrinth_fills_the_gap() {
        assert_eq!(resolve("client", Some("both")), Some(Side { side: "client".into(), from: From::CurseForge }));
        assert_eq!(resolve("", Some("client")), Some(Side { side: "client".into(), from: From::Modrinth }));
        assert_eq!(resolve("", Some("")), None);
        assert_eq!(resolve("", None), None);
        // Junk from either side counts as unknown.
        assert_eq!(resolve("optional", Some("nope")), None);
        assert_eq!(resolve("weird", Some("server")).map(|s| s.from), Some(From::Modrinth));
    }

    #[test]
    fn jar_fills_only_what_both_sites_leave_open() {
        let from = |cf, mr, jar| resolve_with(cf, mr, jar).map(|s| (s.side, s.from));
        // The jar alone: its word, marked as the jar's.
        assert_eq!(from("", None, Some("client")), Some(("client".into(), From::Jar)));
        assert_eq!(from("", Some(""), Some("server")), Some(("server".into(), From::Jar)));
        // CurseForge's tag wins over the jar, Modrinth's wins over the jar.
        assert_eq!(from("both", Some("client"), Some("client")), Some(("both".into(), From::CurseForge)));
        assert_eq!(from("", Some("both"), Some("client")), Some(("both".into(), From::Modrinth)));
        // Junk from the jar is unknown, and nothing at all stays unknown.
        assert_eq!(from("", None, Some("")), None);
        assert_eq!(from("optional", Some("nope"), Some("sometimes")), None);
        assert_eq!(from("", None, None), None);
    }

    #[test]
    fn words_say_where_it_came_from() {
        assert_eq!(resolve_with("", None, Some("client")).unwrap().words(), "Client only (from the jar)");
        assert_eq!(resolve("", Some("client")).unwrap().words(), "Client only (from Modrinth)");
        assert_eq!(resolve("both", None).unwrap().words(), "Client and server");
        assert_eq!(resolve("server", None).unwrap().words(), "Server only");
    }

    #[test]
    fn look_up_asks_by_jar_then_by_slug_and_remembers_misses() {
        // Ids no other test uses: KNOWN is process-wide.
        let dir = std::env::temp_dir().join(format!("hopper-modsides-{}", std::process::id()));
        let mods = dir.join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        for f in ["ipn.jar", "jei.jar", "nothing.jar", "tagged.jar"] {
            std::fs::write(mods.join(f), f).unwrap();
        }
        let inst = Instance {
            root: dir.clone(),
            addons: vec![
                addon(9_100_001, "ipn.jar", "", "https://www.curseforge.com/minecraft/mc-mods/inventory-profiles-next"),
                addon(9_100_002, "jei.jar", "", "https://www.curseforge.com/minecraft/mc-mods/jei"),
                addon(9_100_003, "nothing.jar", "", "https://www.curseforge.com/minecraft/mc-mods/nothing"),
                addon(9_100_004, "tagged.jar", "server", ""),
            ],
            ..Default::default()
        };
        let asked = std::cell::Cell::new(0);
        let by_jar = |p: &[std::path::PathBuf]| {
            asked.set(asked.get() + 1);
            assert!(p.iter().all(|p| !p.ends_with("tagged.jar")), "a tagged mod isn't asked about");
            HashMap::from([("ipn.jar".to_string(), "client".to_string())])
        };
        let by_slug = |m: &[(String, String)]| {
            assert_eq!(m.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(), ["jei", "nothing"]);
            HashMap::from([("jei".to_string(), "both".to_string())])
        };
        let no_jar = |_: &Path| -> Option<String> { None };
        assert!(needs_look_up(&inst));
        let before = generation();
        assert!(look_up_with(&inst, &by_jar, &by_slug, &no_jar));
        assert!(generation() > before);
        let side = |i: usize| side_of(&inst.addons[i]);
        assert_eq!(side(0), Some(Side { side: "client".into(), from: From::Modrinth }));
        assert_eq!(side(1).map(|s| s.side), Some("both".into()));
        assert_eq!(side(2), None);
        assert_eq!(side(3).map(|s| s.from), Some(From::CurseForge));
        // Asked once: the miss is remembered too.
        assert!(!needs_look_up(&inst));
        assert!(!look_up_with(&inst, &by_jar, &by_slug, &no_jar));
        assert_eq!(asked.get(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn jar_answer_fills_gaps_after_modrinth_and_is_read_once() {
        let dir = std::env::temp_dir().join(format!("hopper-modsides-jar-{}", std::process::id()));
        let mods = dir.join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        for f in ["gh-only.jar", "on-modrinth.jar", "jar-silent.jar", "tagged.jar"] {
            std::fs::write(mods.join(f), f).unwrap();
        }
        let inst = Instance {
            root: dir.clone(),
            addons: vec![
                addon(9_200_001, "gh-only.jar", "", ""),
                addon(9_200_002, "on-modrinth.jar", "", ""),
                addon(9_200_003, "jar-silent.jar", "", ""),
                addon(9_200_004, "tagged.jar", "both", ""),
            ],
            ..Default::default()
        };
        let by_jar = |p: &[PathBuf]| {
            assert!(p.iter().all(|p| !p.ends_with("tagged.jar")));
            HashMap::from([("on-modrinth.jar".to_string(), "both".to_string())])
        };
        let no_slugs = |_: &[(String, String)]| HashMap::new();
        let opened = std::cell::RefCell::new(Vec::<String>::new());
        let read = |p: &Path| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            opened.borrow_mut().push(name.clone());
            // Every jar here claims client-only, to show Modrinth still wins.
            (name != "jar-silent.jar").then(|| "client".to_string())
        };
        let before = generation();
        assert!(look_up_with(&inst, &by_jar, &no_slugs, &read));
        assert!(generation() > before);
        let side = |i: usize| side_of(&inst.addons[i]);
        assert_eq!(side(0), Some(Side { side: "client".into(), from: From::Jar }));
        assert_eq!(side(0).unwrap().words(), "Client only (from the jar)");
        // Modrinth said both: the jar isn't even opened for it.
        assert_eq!(side(1), Some(Side { side: "both".into(), from: From::Modrinth }));
        assert_eq!(side(2), None);
        assert_eq!(side(3).map(|s| s.from), Some(From::CurseForge));
        let mut got = opened.borrow().clone();
        got.sort();
        assert_eq!(got, ["gh-only.jar", "jar-silent.jar"]);
        // Everything is known now, silent jars included: no more asking.
        assert!(!needs_look_up(&inst));
        assert!(!look_up_with(&inst, &by_jar, &no_slugs, &read));
        assert_eq!(opened.borrow().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_jar_is_opened_once_until_it_changes() {
        let dir = std::env::temp_dir().join(format!("hopper-modsides-stamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.jar");
        std::fs::write(&p, "one").unwrap();
        let opens = std::cell::Cell::new(0);
        let read = |_: &Path| {
            opens.set(opens.get() + 1);
            Some("client".to_string())
        };
        assert_eq!(jar_side_with(&p, &read), "client");
        assert_eq!(jar_side_with(&p, &read), "client");
        assert_eq!(opens.get(), 1);
        // A different size is a different jar.
        std::fs::write(&p, "changed").unwrap();
        assert_eq!(jar_side_with(&p, &read), "client");
        assert_eq!(opens.get(), 2);
        // A missing jar answers nothing and isn't opened.
        assert_eq!(jar_side_with(&dir.join("gone.jar"), &read), "");
        assert_eq!(opens.get(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
