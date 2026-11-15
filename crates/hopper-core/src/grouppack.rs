//! The pack version in a group pack kept both ways.
//!
//! Every member's code names the pack and its exact version, then lists
//! mods relative to that version. Two-way sync takes changes mod by mod
//! (`groupsync`), so without this a member who moved the pack to a new
//! version left everyone else on the old one, and worse: a mod they had
//! added themselves that the new version ships drops out of their code
//! (it is the pack's now), which read as "removed" and deleted it on every
//! PC still on the old version.
//!
//! The pack version is one more thing the group keeps the same, under
//! [`PACK_KEY`], with the same rules as a mod or the loader: latest change
//! wins (moving back counts too), echoes change nothing, and an unsent
//! change here holds the others back. Applying it runs the whole pack
//! update ("Keep my changes"), before any of that member's mod changes.

use crate::groupsync::{Change, Mods, State};
use crate::sharecode::Base;
use std::collections::HashSet;
use std::path::PathBuf;

/// The key the pack version goes by next to the mods. Not a mod's key in
/// any form, so it never meets one.
pub const PACK_KEY: &str = "~pack";

/// A code's pack version as [`State`]: "pack:cf:<project>:<file>" or
/// "pack:mr:<project>:<version>". A plain code has none.
pub fn pack_state(base: &Base) -> State {
    match base {
        Base::Plain { .. } => State::Unlisted,
        Base::Curse { project, file } => State::File { file: format!("pack:cf:{project}:{file}"), on: true },
        Base::Modrinth { project, version } => State::File { file: format!("pack:mr:{project}:{version}"), on: true },
    }
}

/// A pack version, read back from its [`pack_state`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Version {
    Curse { project: u64, file: u64 },
    Modrinth { project: String, version: String },
}

impl Version {
    pub fn of(s: &State) -> Option<Version> {
        let State::File { file, .. } = s else { return None };
        let rest = file.strip_prefix("pack:")?;
        let (site, rest) = rest.split_once(':')?;
        let (p, v) = rest.split_once(':')?;
        match site {
            "cf" => Some(Version::Curse { project: p.parse().ok()?, file: v.parse().ok()? }),
            "mr" if !p.is_empty() && !v.is_empty() => Some(Version::Modrinth { project: p.into(), version: v.into() }),
            _ => None,
        }
    }

    /// Hopper's numbers for it (Modrinth ids hashed, as instances keep
    /// them), for `source::file` and the update.
    pub fn ids(&self) -> (u64, u64) {
        match self {
            Version::Curse { project, file } => (*project, *file),
            Version::Modrinth { project, version } => (crate::modrinth::num_id(project), crate::modrinth::num_id(version)),
        }
    }

    fn project_key(&self) -> String {
        match self {
            Version::Curse { project, .. } => format!("cf:{project}"),
            Version::Modrinth { project, .. } => format!("mr:{project}"),
        }
    }

    /// Whether `other` is a version of the same pack.
    pub fn same_pack(&self, other: &Version) -> bool {
        self.project_key() == other.project_key()
    }

    fn cache_name(&self) -> String {
        match self {
            Version::Curse { project, file } => format!("cf-{project}-{file}.json"),
            Version::Modrinth { project, version } => {
                let safe = |s: &str| s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>();
                format!("mr-{}-{}.json", safe(project), safe(version))
            }
        }
    }
}

/// What a pack version change from the group does to this instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Already on that version (or the group has no pack version).
    Nothing,
    /// Update (or move back) to this version.
    Move(Version),
    /// Another pack altogether: not followed (the group's pack was
    /// installed again from someone's share code).
    Other,
}

/// What to do here with the pack change `c` (`here`: this instance's pack
/// version as [`pack_state`] gives it).
pub fn step(here: &State, c: &Change) -> Step {
    if c.now == *here {
        return Step::Nothing;
    }
    let Some(want) = Version::of(&c.now) else { return Step::Nothing };
    match Version::of(here) {
        Some(h) if h.same_pack(&want) => Step::Move(want),
        _ => Step::Other,
    }
}

/// The mods a pack version comes with: "p:<project>" for each mod its
/// manifest lists (CurseForge, or a Modrinth file's project), "n:<file
/// name>" for each jar it ships by name (a Modrinth pack's files, a
/// CurseForge pack's override jars).
pub type Ships = HashSet<String>;

/// Whether a pack version that comes with `ships` has the mod a code calls
/// `key` (`name`: that mod's file name when known, for a jar the pack
/// ships under its own name).
pub fn has(ships: &Ships, key: &str, name: Option<&str>) -> bool {
    ships.contains(key) || name.is_some_and(|n| ships.contains(&format!("n:{}", n.trim_end_matches(".disabled").to_lowercase())))
}

/// Whether a mod that differs between two codes of one member only differs
/// because their pack version changed, and so is no change of theirs:
///
/// - a mod of theirs the new version comes with left the code (it is the
///   pack's own now; calling it removed deletes it from everyone still on
///   the old version);
/// - a pack mod they had taken out or turned off that the new version no
///   longer has left the code too (the new version takes it out itself).
///
/// Unknown contents (the version couldn't be looked up) count as
/// explained: a mod leaving a code is never taken as removed then.
pub fn explained(was: &Mods, now: &Mods, c: &Change) -> bool {
    if was.pack == now.pack || c.now != State::Unlisted || c.key == PACK_KEY || c.key == crate::groupsync::LOADER_KEY {
        return false;
    }
    let Some(ships) = now.ships.as_ref() else { return true };
    let name = was.names.get(&c.key).or_else(|| now.names.get(&c.key)).map(String::as_str);
    match c.was {
        State::File { .. } => has(ships, &c.key, name),
        State::Removed | State::Off => !has(ships, &c.key, name),
        State::Unlisted => false,
    }
}

// ---- what a version ships, looked up once ---------------------------------------------

fn cache_dir() -> PathBuf {
    crate::paths::dirs()
        .map(|d| d.cache_dir().join("pack-ships"))
        .unwrap_or_else(|| std::env::temp_dir().join("hopper-pack-ships"))
}

static SEEN: std::sync::Mutex<Option<std::collections::HashMap<String, Ships>>> = std::sync::Mutex::new(None);

/// What pack version `v` comes with: from memory, the disk cache, or the
/// pack's zip (cached by the pack update too). None: it couldn't be read.
pub fn ships(v: &Version, job: &crate::jobs::Job) -> Option<Ships> {
    let name = v.cache_name();
    if let Some(s) = SEEN.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(&name).cloned())) {
        return Some(s);
    }
    let path = cache_dir().join(&name);
    let found = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Ships>(&b).ok())
        .or_else(|| match read_ships(v, job) {
            Ok(s) => {
                let _ = std::fs::create_dir_all(cache_dir());
                if let Ok(b) = serde_json::to_vec(&s) {
                    let _ = std::fs::write(&path, b);
                }
                Some(s)
            }
            Err(e) => {
                log::warn!("reading what pack version {name} comes with: {e:#}");
                None
            }
        })?;
    if let Ok(mut g) = SEEN.lock() {
        g.get_or_insert_with(Default::default).insert(name, found.clone());
    }
    Some(found)
}

fn read_ships(v: &Version, job: &crate::jobs::Job) -> anyhow::Result<Ships> {
    let (p, f) = v.ids();
    let file = crate::source::file(p, f)?;
    match v {
        Version::Curse { .. } => crate::modpack::curse_ships(&file, job),
        Version::Modrinth { .. } => crate::modpack::modrinth_ships(&file, job),
    }
}

// ---- wording ------------------------------------------------------------------------------

/// "8.6" from "All the Mods 10-8.6".
pub fn short_version(pack: &str, display: &str) -> String {
    crate::hosting::motd::short_version(pack, display)
}

/// The pack version a file is, for the notice and the toast: "13.6.0" of
/// "Fabulously.Optimized-13.6.0.zip". The file name first: a display name
/// like "13.6.0 for 26.1.2" ends in the Minecraft version, which
/// [`short_version`] would take.
pub fn version_name(pack: &str, file: &crate::cf::File) -> String {
    let from_file = crate::instance_ops::pack_version_label(&file.file_name);
    if !from_file.is_empty() && from_file.chars().any(|c| c.is_ascii_digit()) {
        return from_file;
    }
    short_version(pack, &file.display_name)
}

/// The toast and the task's note once the group's pack version is in:
/// "McFawx updated All the Mods 10 to 8.6. Your edits were kept."
pub fn says_done(who: &str, pack: &str, version: &str, back: bool) -> String {
    let who = if who.is_empty() { "Someone in your group" } else { who };
    let verb = if back { "moved" } else { "updated" };
    let pack = if pack.is_empty() { "the pack" } else { pack };
    format!("{who} {verb} {pack} {}{version}. Your edits were kept.", if back { "back to " } else { "to " })
}

/// Why the group's pack version couldn't go in, in plain words: the
/// notice shows it to the player, not the web request that failed.
pub fn failed_says(e: &anyhow::Error) -> String {
    if format!("{e:#}").contains("status: 404") {
        return "the pack's site doesn't have that version.".into();
    }
    crate::recipe::says(e)
}

/// Why the group's pack version isn't in yet, for the instance page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wait {
    /// The game is running: it goes in once the game closes.
    Game,
    /// The update needs the player to pick (settings both changed, or a
    /// mod of theirs needs one the pack drops).
    Pick,
    /// It failed; the text says why.
    Failed(String),
}

/// The group moved the pack and this PC hasn't followed yet.
#[derive(Clone, Debug)]
pub struct Due {
    pub to: Version,
    /// The version's file, for the update dialog (None: it couldn't be
    /// looked up).
    pub file: Option<crate::cf::File>,
    /// The new version's name ("8.6") and the pack's.
    pub version: String,
    pub pack: String,
    /// Who moved it.
    pub who: String,
    /// It is older than the version here.
    pub back: bool,
    pub wait: Wait,
}

impl Due {
    /// "McFawx updated All the Mods 10 to 8.6".
    pub fn title(&self) -> String {
        let who = if self.who.is_empty() { "Someone in your group" } else { &self.who };
        let verb = if self.back { "moved" } else { "updated" };
        let to = if self.back { "back to" } else { "to" };
        let pack = if self.pack.is_empty() { "the pack" } else { &self.pack };
        format!("{who} {verb} {pack} {to} {}", self.version)
    }

    /// The line under it.
    pub fn detail(&self) -> String {
        match &self.wait {
            Wait::Game => "Your copy updates when you close the game. Your edits are kept.".into(),
            Wait::Pick => "Some of your settings changed in this update too. Pick what to keep, then the group's other changes follow.".into(),
            Wait::Failed(why) => format!("Couldn't update: {why} Hopper tries again in 10 minutes."),
        }
    }

    /// The button: "Update pack to 8.6".
    pub fn action(&self) -> String {
        if self.back { format!("Move pack back to {}", self.version) } else { format!("Update pack to {}", self.version) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pack_version_reads_back() {
        let s = pack_state(&Base::Curse { project: 925200, file: 7001 });
        assert_eq!(Version::of(&s), Some(Version::Curse { project: 925200, file: 7001 }));
        let m = pack_state(&Base::Modrinth { project: "AbC".into(), version: "xY1".into() });
        assert_eq!(Version::of(&m), Some(Version::Modrinth { project: "AbC".into(), version: "xY1".into() }));
        assert_eq!(pack_state(&Base::Plain { game_version: "1.21.1".into(), loader: "neoforge".into() }), State::Unlisted);
        assert_eq!(Version::of(&State::File { file: "cf:5".into(), on: true }), None);
    }

    fn change(was: &State, now: &State) -> Change {
        Change { key: PACK_KEY.into(), was: was.clone(), now: now.clone(), entry: None }
    }

    #[test]
    fn the_same_pack_moves_and_another_pack_is_left() {
        let v85 = pack_state(&Base::Curse { project: 1, file: 85 });
        let v86 = pack_state(&Base::Curse { project: 1, file: 86 });
        let other = pack_state(&Base::Curse { project: 2, file: 1 });
        assert_eq!(step(&v85, &change(&v85, &v86)), Step::Move(Version::Curse { project: 1, file: 86 }));
        // Moving back is a move like any other.
        assert_eq!(step(&v86, &change(&v86, &v85)), Step::Move(Version::Curse { project: 1, file: 85 }));
        assert_eq!(step(&v86, &change(&v85, &v86)), Step::Nothing);
        assert_eq!(step(&v85, &change(&v85, &other)), Step::Other);
    }

    #[test]
    fn says_who_and_what() {
        assert_eq!(says_done("McFawx", "All the Mods 10", "8.6", false), "McFawx updated All the Mods 10 to 8.6. Your edits were kept.");
        assert_eq!(says_done("Mia", "ATM10", "8.5", true), "Mia moved ATM10 back to 8.5. Your edits were kept.");
        let d = Due {
            to: Version::Curse { project: 1, file: 86 },
            file: None,
            version: "8.6".into(),
            pack: "All the Mods 10".into(),
            who: "McFawx".into(),
            back: false,
            wait: Wait::Game,
        };
        assert_eq!(d.title(), "McFawx updated All the Mods 10 to 8.6");
        assert_eq!(d.action(), "Update pack to 8.6");
        assert_eq!(short_version("All the Mods 10", "All the Mods 10-8.6"), "8.6");
    }

    #[test]
    fn a_version_is_named_by_the_pack_not_by_minecraft() {
        let file = |name: &str, display: &str| crate::cf::File { file_name: name.into(), display_name: display.into(), ..Default::default() };
        // The display name ends in the Minecraft version: "to 26.1.2" was
        // shown for Fabulously Optimized 13.6.0.
        assert_eq!(version_name("Fabulously Optimized", &file("Fabulously.Optimized-13.6.0.zip", "13.6.0 for 26.1.2")), "13.6.0");
        assert_eq!(version_name("All the Mods 10", &file("All the Mods 10-8.6.zip", "All the Mods 10-8.6")), "8.6");
        // A file name without a version: the display name's.
        assert_eq!(version_name("All the Mods 10", &file("pack.zip", "All the Mods 10-8.6")), "8.6");
    }
}
