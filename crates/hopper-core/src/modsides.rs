//! Where a mod runs, for every list that shows it: CurseForge's Client /
//! Server tag on the installed file, else what Modrinth says about the same
//! mod. Many authors tag the side on Modrinth only (Inventory Profiles Next's
//! CurseForge files carry just a version and a loader), so without the
//! fallback those mods show a question mark.
//!
//! The Modrinth answers come from [`crate::modrinth::sides_of`] (the jar's
//! sha1) and [`crate::modrinth::sides_by_slug`] (the same project by slug),
//! the lookups server sync uses, with their disk caches. Lists never look up
//! on render: they read what is known here and call [`look_up`] off the UI
//! thread, then redraw when [`generation`] moves.

use crate::instances::{Addon, Instance};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Who said where a mod runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum From {
    CurseForge,
    Modrinth,
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

    /// The side in words, saying when it came from Modrinth ("Client only
    /// (from Modrinth)"), for tooltips and the hover card.
    pub fn words(&self) -> String {
        let base = label(&self.side);
        match self.from {
            From::CurseForge => base.to_string(),
            From::Modrinth => format!("{base} (from Modrinth)"),
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
    if valid(cf_env) {
        return Some(Side { side: cf_env.to_string(), from: From::CurseForge });
    }
    modrinth
        .filter(|s| valid(s))
        .map(|s| Side { side: s.to_string(), from: From::Modrinth })
}

/// What Modrinth said, by [`key`]: "" when it was asked and didn't know.
static KNOWN: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
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

/// Moves each time new Modrinth answers land.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// The side a list shows for this mod, without any network or disk: the
/// CurseForge tag, else Modrinth's answer if [`look_up`] has one.
pub fn side_of(a: &Addon) -> Option<Side> {
    if valid(&a.env) {
        return resolve(&a.env, None);
    }
    let g = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    let mr = g.as_ref().and_then(|m| m.get(&key(a))).map(String::as_str);
    resolve(&a.env, mr)
}

/// Shorthand: the side's value, "" when unknown (for sorting and filters
/// that compare `env` strings).
pub fn env_of(a: &Addon) -> String {
    side_of(a).map(|s| s.side).unwrap_or_default()
}

/// Mods in this instance that need asking: untagged, in mods/, not asked
/// yet this run.
fn wanted<'a>(inst: &'a Instance) -> Vec<&'a Addon> {
    let g = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    inst.addons
        .iter()
        .filter(|a| a.folder() == "mods" && !valid(&a.env))
        .filter(|a| g.as_ref().is_none_or(|m| !m.contains_key(&key(a))))
        .collect()
}

/// Whether some of this instance's mods still need asking.
pub fn needs_look_up(inst: &Instance) -> bool {
    !wanted(inst).is_empty()
}

/// Asks Modrinth about this instance's untagged mods (blocking: run it off
/// the UI thread). Jars Modrinth has by sha1 first, then the rest by their
/// CurseForge slug. Both lookups cache on disk, misses too, so only mods
/// never asked before cost a request. `true` when anything new is known.
pub fn look_up(inst: &Instance) -> bool {
    look_up_with(inst, &crate::modrinth::sides_of, &crate::modrinth::sides_by_slug)
}

fn look_up_with(
    inst: &Instance,
    sides_of: &dyn Fn(&[std::path::PathBuf]) -> HashMap<String, String>,
    sides_by_slug: &dyn Fn(&[(String, String)]) -> HashMap<String, String>,
) -> bool {
    let todo: Vec<Addon> = wanted(inst).into_iter().cloned().collect();
    if todo.is_empty() {
        return false;
    }
    let dir = inst.mods_dir();
    let paths: Vec<std::path::PathBuf> = todo
        .iter()
        .map(|a| dir.join(&a.file_name))
        .filter(|p| p.is_file())
        .collect();
    let by_file = sides_of(&paths);
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
    let mut g = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    let mut new = false;
    for a in &todo {
        let side = found.remove(&key(a)).unwrap_or_default();
        new |= !side.is_empty();
        m.insert(key(a), side);
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
    fn words_say_where_it_came_from() {
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
        assert!(needs_look_up(&inst));
        let before = generation();
        assert!(look_up_with(&inst, &by_jar, &by_slug));
        assert!(generation() > before);
        let side = |i: usize| side_of(&inst.addons[i]);
        assert_eq!(side(0), Some(Side { side: "client".into(), from: From::Modrinth }));
        assert_eq!(side(1).map(|s| s.side), Some("both".into()));
        assert_eq!(side(2), None);
        assert_eq!(side(3).map(|s| s.from), Some(From::CurseForge));
        // Asked once: the miss is remembered too.
        assert!(!needs_look_up(&inst));
        assert!(!look_up_with(&inst, &by_jar, &by_slug));
        assert_eq!(asked.get(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
