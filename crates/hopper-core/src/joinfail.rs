//! Failures to join a server or open a world: the game doesn't crash, it
//! shows a disconnect screen (or the world list again), so the crash scan
//! never sees them. They're read from `logs/latest.log` after the game ends,
//! and from a server's own log for the players it turned away.
//!
//! What counts: the mod loader refusing the connection (mod channels or
//! registries that don't match, a different NeoForge, mods the client
//! lacks), vanilla refusing it (another Minecraft version, a bad session,
//! the server unreachable), and a singleplayer world that won't load (its
//! data packs, or saved by a newer game). A normal quit says none of these.
//!
//! Mods the message names are listed as involved, never blamed: like a
//! crash's blamed mod, a channel list names every mod that differs, and
//! libraries (see [`crate::crashtrace::is_pass_through`]) go last.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

/// Kept beside the instance until a game ends without a join problem.
pub const FILE: &str = ".hopper-join.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinKind {
    /// Mods (or their versions, network channels) differ from the server's.
    Mods,
    /// Content (blocks, items) the server has and this game lacks.
    Registry,
    /// Another NeoForge / Forge build, or a vanilla game on a modded server.
    Loader,
    /// Another Minecraft version.
    GameVersion,
    /// Signed out, or Mojang's sign-in servers didn't answer.
    Session,
    /// The server couldn't be reached.
    Network,
    /// A singleplayer world's data packs failed.
    DataPacks,
    /// A singleplayer world saved by a newer game.
    NewerWorld,
    /// Disconnected for another reason (the server's own words).
    Other,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JoinProblem {
    /// When the game ended (seconds).
    pub when: i64,
    pub kind: JoinKind,
    /// The server's address (`host:port`), or the world's name; may be empty.
    pub target: String,
    /// A singleplayer world, not a server.
    pub world: bool,
    /// What happened, in plain words.
    pub why: String,
    /// What to do about it.
    pub fix: String,
    /// Mod ids the message names, mods before libraries.
    pub mods: Vec<String>,
    /// (this game's, the server's) version, when the message says.
    pub versions: Option<(String, String)>,
    /// The log line it was read from, trimmed.
    pub line: String,
    /// The jar in the mods folder for each of [`Self::mods`] ("" when this
    /// game doesn't have it).
    #[serde(default)]
    pub jars: Vec<String>,
}

impl JoinProblem {
    /// The card's title.
    pub fn title(&self) -> String {
        match (self.world, self.target.is_empty()) {
            (true, true) => "The world didn't open".into(),
            (true, false) => format!("{} didn't open", self.target),
            (false, true) => "Couldn't join the server".into(),
            (false, false) => format!("Couldn't join {}", self.target),
        }
    }
}

fn re(p: &str) -> Regex {
    Regex::new(p).expect("static regex")
}

// ---- what a reason says ----------------------------------------------------

/// One refusal, classified: the same rules for the client's disconnect
/// screen and the server's "lost connection" line.
#[derive(Clone, Debug, PartialEq)]
pub struct Reason {
    pub kind: JoinKind,
    pub why: String,
    pub fix: String,
    pub mods: Vec<String>,
    /// (client's, server's), either may be empty.
    pub versions: Option<(String, String)>,
}

static CHANNEL_MISMATCH: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)mismatched mod channel list|channels? .{0,300}?\brejected their|mismatching network versions|incompatible network (?:versions|channels)|failed to negotiate network|channel negotiation failed|missing mods?:|mod mismatch")
});
static REGISTRY: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)registry remapping failed|missing registr(?:y entries|ies)|unknown registry entries|registry sync packet with unknown entries|registries? (?:don't|do not) match")
});
static LOADER: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)incompatible (?:fml|neoforge|forge)(?: modded server| client)?(?: version)?|this server has mods that require|requires? (?:neoforge|forge) to be installed|server is running (?:neoforge|forge)|vanilla (?:client|server)[^.]*not (?:supported|allowed)|you are trying to connect to a server that is running")
});
static OUTDATED: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)outdated (client|server)!?(?:\s*(?:please use|i'?m still on)\s*([\w.+-]+))?|incompatible client! please use (?:neoforge |forge )?([\w.+-]+)"));
static SESSION: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)failed to (?:log ?in|verify username)[^:]*: invalid session|invalid session \(try restarting|authentication servers are down|multiplayer is disabled|not authenticated with minecraft\.net")
});
static NETWORK: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)connection refused|connection timed out|timed out: no further information|AnnotatedConnectException|io\.netty[\w.$]*ConnectException|UnknownHostException|unknown host|no route to host")
});
static DATAPACKS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)failed to load datapacks|errors in currently selected data ?packs|datapack load failure|failed to load data packs")
});
static NEWER_WORLD: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)(?:world|level) was saved (?:in|with) a newer version|saved with a newer version|newer version of minecraft"));
/// A channel `mod:path` in a list (`[create:main, jei:handshake]`).
static CHANNEL: LazyLock<Regex> = LazyLock::new(|| re(r"\b([a-z0-9_.-]{2,64}):[a-z0-9_/.-]+"));
/// NeoForge's per-mod line: `create (server: 6, client: 5)` or
/// `create: server version 6, client version 5`.
static MOD_VERSIONS: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\b([a-z0-9_.-]{2,64})\b:?\s*\(?\s*server(?: version)?:? ?([\w.+-]+),\s*client(?: version)?:? ?([\w.+-]+)"));
/// "NeoForge 21.1.77", "neoforge-21.1.77", "FML 4.0".
static LOADER_VERSION: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\b(?:neoforge|forge)[ -]?v?(\d+\.\d+(?:\.\d+)?(?:-[\w.]+)?)"));
/// "Missing mods: [a, b]" / "missing on the client: a, b".
static MOD_LIST: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)(?:missing mods?|mods? missing|missing on the (?:client|server)|mods that require[^:]*)\s*:?\s*\[?([a-z0-9_., -]+)\]?"));

/// Namespaces that are the game or a loader, not a mod.
fn not_a_mod(ns: &str) -> bool {
    matches!(ns, "minecraft" | "neoforge" | "forge" | "fml" | "c" | "common" | "fabric" | "brand" | "register" | "unregister" | "http" | "https")
}

/// Mod ids a message names, mods first and libraries last, each once.
fn named_mods(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |id: &str| {
        let id = id.trim().trim_matches(|c: char| c == ',' || c == '.').to_ascii_lowercase();
        if id.len() >= 2 && !not_a_mod(&id) && !id.chars().all(|c| c.is_ascii_digit() || c == '.') && !out.contains(&id) {
            out.push(id);
        }
    };
    for c in MOD_VERSIONS.captures_iter(text) {
        push(&c[1]);
    }
    for c in CHANNEL.captures_iter(text) {
        push(&c[1]);
    }
    for c in MOD_LIST.captures_iter(text) {
        for id in c[1].split([',', ' ']) {
            if !id.is_empty() && !matches!(id, "and" | "the" | "be" | "to" | "installed" | "on" | "client") {
                push(id);
            }
        }
    }
    out.retain(|id| !matches!(id.as_str(), "server" | "client" | "version" | "mods" | "mod"));
    // A library that every other mod's channel goes through isn't the news.
    out.sort_by_key(|id| crate::crashtrace::is_pass_through(id));
    out
}

const LOADER_WHY: &str = "The server runs a different NeoForge (or Forge) than this game, or needs one this game doesn't have.";
const LOADER_FIX: &str = "Use the same NeoForge build as the server: change the pack's, or update the server.";

/// What one disconnect or refusal message means. `None` when it doesn't
/// say a join failed (a normal quit, "Server closed", a kick).
pub fn classify(msg: &str) -> Option<Reason> {
    let r = |kind, why: &str, fix: &str| Reason { kind, why: why.into(), fix: fix.into(), mods: Vec::new(), versions: None };
    if REGISTRY.is_match(msg) {
        let mut x = r(
            JoinKind::Registry,
            "The server has blocks or items this game doesn't know. Its mods aren't the same as yours.",
            "Use the same mods, at the same versions, as the server.",
        );
        x.mods = named_mods(msg);
        return Some(x);
    }
    if LOADER.is_match(msg) {
        let v = LOADER_VERSION.captures(msg).map(|c| c[1].to_string()).unwrap_or_default();
        let mut x = r(JoinKind::Loader, LOADER_WHY, LOADER_FIX);
        x.versions = (!v.is_empty()).then(|| (String::new(), v));
        x.mods = named_mods(msg);
        return Some(x);
    }
    if CHANNEL_MISMATCH.is_match(msg) || MOD_VERSIONS.is_match(msg) {
        let mut x = r(
            JoinKind::Mods,
            "Some mods don't match the server's: missing on one side, or at another version.",
            "Install the mods the server has, at the same versions.",
        );
        x.mods = named_mods(msg);
        if x.mods.len() == 1
            && let Some(c) = MOD_VERSIONS.captures(msg)
        {
            x.versions = Some((c[3].to_string(), c[2].to_string()));
        }
        return Some(x);
    }
    if let Some(c) = OUTDATED.captures(msg) {
        let (side, v) = match (c.get(1), c.get(2).or(c.get(3))) {
            (Some(s), v) => (s.as_str().to_ascii_lowercase(), v.map_or("", |m| m.as_str()).to_string()),
            (None, v) => ("client".to_string(), v.map_or("", |m| m.as_str()).to_string()),
        };
        // An "Incompatible client! Please use <NeoForge build>" is the loader's.
        if c.get(3).is_some() && LOADER_VERSION.is_match(msg) {
            let mut x = r(JoinKind::Loader, LOADER_WHY, LOADER_FIX);
            x.versions = Some((String::new(), v));
            return Some(x);
        }
        let mut x = if side == "client" {
            r(JoinKind::GameVersion, "The server runs a newer Minecraft than this game.", "Play the server's Minecraft version, or ask its owner which pack to use.")
        } else {
            r(JoinKind::GameVersion, "The server runs an older Minecraft than this game.", "Play the server's Minecraft version.")
        };
        if !v.is_empty() {
            x.versions = Some((String::new(), v));
        }
        return Some(x);
    }
    if SESSION.is_match(msg) {
        let down = msg.to_ascii_lowercase().contains("servers are down");
        return Some(if down {
            r(JoinKind::Session, "Mojang's sign-in servers didn't answer, so the server couldn't check who you are.", "Wait a few minutes and try again.")
        } else {
            r(JoinKind::Session, "The server couldn't check your Minecraft account: your sign-in had run out.", "Sign in again in Accounts, then restart the game.")
        });
    }
    if DATAPACKS.is_match(msg) {
        return Some(r(
            JoinKind::DataPacks,
            "The world's data packs failed to load, so the world didn't open.",
            "Open it in Safe Mode from the game's world list, or take out the data pack that changed. Data packs has the world's list.",
        ));
    }
    if NEWER_WORLD.is_match(msg) {
        return Some(r(
            JoinKind::NewerWorld,
            "The world was saved by a newer Minecraft than this game.",
            "Open it with the Minecraft version it was last played on. Opening it in an older one can break it.",
        ));
    }
    if NETWORK.is_match(msg) {
        return Some(r(
            JoinKind::Network,
            "The server didn't answer: it's off, still starting, or the address or port is wrong.",
            "Check that the server is running and the address is right, then try again.",
        ));
    }
    None
}

// ---- the game's log --------------------------------------------------------

/// Lines where the game says why it left a server.
static DISCONNECT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)(?:client disconnected with reason|disconnected from server(?: with reason)?|connection lost|lost connection|failed to connect to the server|couldn't connect to server|disconnect reason)\s*[:\-]?\s*(.*)$")
});
/// The server the game last tried: `Connecting to host, 25565`.
static CONNECTING: LazyLock<Regex> = LazyLock::new(|| re(r"Connecting to ([^\s,]+),? ?(\d+)?"));
/// Reasons that are a normal end of play.
fn normal(reason: &str) -> bool {
    let r = reason.trim().trim_matches(|c: char| c == '"' || c == '\'' || c == '.').to_ascii_lowercase();
    r.is_empty()
        || matches!(r.as_str(), "disconnected" | "quitting" | "server closed" | "disconnect.quitting" | "multiplayer.disconnect.server_shutdown" | "kicked by an operator")
        || r.contains("translation{key='disconnect.quitting'")
        || r.contains("server_shutdown")
        || r.contains("server closed")
}

/// A disconnect text component as words: `literal{Outdated client}`,
/// `translation{key='multiplayer.disconnect.outdated_client', args=[1.21.1]}`.
fn plain(reason: &str) -> String {
    reason
        .replace("literal{", "")
        .replace("translation{key='", "")
        .replace("', args=[", " ")
        .replace(['}', ']', '['], " ")
        .replace("multiplayer.disconnect.outdated_client", "Outdated client! Please use")
        .replace("multiplayer.disconnect.outdated_server", "Outdated server! I'm still on")
        .replace("multiplayer.disconnect.unverified_username", "Failed to verify username: invalid session")
        .replace("disconnect.loginFailedInfo.invalidSession", "Failed to login: Invalid session")
        .replace("disconnect.loginFailedInfo.serversUnavailable", "Authentication servers are down")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The game's last join problem in a log, if its last attempt failed.
/// Lines are classified newest first; a later successful join (or world
/// load) after a failure means it worked in the end.
pub fn in_log(log: &str) -> Option<JoinProblem> {
    let lines: Vec<&str> = log.lines().collect();
    let target = |upto: usize| -> String {
        lines[..upto].iter().rev().find_map(|l| CONNECTING.captures(l)).map_or(String::new(), |c| match c.get(2) {
            Some(p) if p.as_str() != "25565" => format!("{}:{}", &c[1], p.as_str()),
            _ => c[1].to_string(),
        })
    };
    for (i, line) in lines.iter().enumerate().rev() {
        // Joined after this point: whatever failed before it doesn't matter.
        if joined(line) {
            return None;
        }
        let msg = plain(line);
        let reason = if let Some(c) = DISCONNECT.captures(&msg) {
            let why = c[1].to_string();
            // The reason may sit on the lines after (NeoForge lists the
            // mods there; vanilla puts the connect error there).
            let more: String = lines[i + 1..lines.len().min(i + 12)]
                .iter()
                .take_while(|l| !l.starts_with('['))
                .map(|l| format!(" {}", plain(l.trim())))
                .collect();
            let text = format!("{msg}{more}");
            if normal(&why) && classify(&text).is_none() {
                continue;
            }
            classify(&text).or_else(|| {
                Some(Reason {
                    kind: JoinKind::Other,
                    why: format!("The server ended the connection: \"{}\"", cut(why.trim(), 200)),
                    fix: "If it says why, do that; else check that the pack and the server have the same mods.".into(),
                    mods: Vec::new(),
                    versions: None,
                })
            })
        } else if is_log_line(line) {
            // Away from a disconnect, only the loader's own words count:
            // mods' update checks log connect errors and timeouts too.
            match classify(&msg) {
                Some(r) if !matches!(r.kind, JoinKind::Network | JoinKind::Other | JoinKind::Session) => Some(r),
                Some(r) if r.kind == JoinKind::Session && line.contains("Render thread") => Some(r),
                _ => continue,
            }
        } else {
            continue;
        };
        let Some(mut r) = reason else { continue };
        let world = matches!(r.kind, JoinKind::DataPacks | JoinKind::NewerWorld);
        // The loader often names the mods a few lines before the
        // disconnect itself, in the same attempt: gather them, and the
        // mods inside any trace under those lines (past the libraries).
        let start = lines[..i].iter().rposition(|l| CONNECTING.is_match(l)).unwrap_or(i.saturating_sub(200));
        for j in start..=i {
            let here = plain(lines[j]);
            let about = classify(&here).is_some_and(|x| matches!(x.kind, JoinKind::Mods | JoinKind::Registry | JoinKind::Loader));
            if j != i && !about {
                continue;
            }
            for id in named_mods(&here) {
                if !r.mods.contains(&id) {
                    r.mods.push(id);
                }
            }
            for l in lines[j + 1..lines.len().min(j + 60)].iter().take_while(|l| {
                let t = l.trim_start();
                t.starts_with("at ") || t.starts_with("Caused by") || t.is_empty()
            }) {
                if let Some((id, _)) = crate::crashtrace::frame_mod(l)
                    && !id.is_empty()
                    && !r.mods.contains(&id)
                    && !crate::crashtrace::is_pass_through(&id)
                {
                    r.mods.push(id);
                }
            }
        }
        r.mods.sort_by_key(|id| crate::crashtrace::is_pass_through(id));
        return Some(JoinProblem {
            when: 0,
            kind: r.kind,
            target: if world { String::new() } else { target(i) },
            world,
            why: r.why,
            fix: r.fix,
            mods: r.mods,
            versions: r.versions,
            line: cut(line.trim(), 300),
            jars: Vec::new(),
        });
    }
    None
}

/// A log line from the game or a mod (`[time] [thread/LEVEL] …`), not a
/// trace line or a mod's banner.
fn is_log_line(l: &str) -> bool {
    l.starts_with('[') && (l.contains("/WARN") || l.contains("/ERROR") || l.contains("/FATAL") || l.contains("/INFO"))
}

/// The game got in: it joined a server or opened a world.
fn joined(l: &str) -> bool {
    l.contains("logged in with entity id")
        || l.contains("Loaded ") && l.contains(" advancements") && l.contains("Render thread")
        || l.contains("[CHAT] ")
        || l.contains("Started serving on")
        || l.contains("joined the game")
}

fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
}

fn modified_secs(p: &Path) -> Option<i64> {
    let t = fs::metadata(p).ok()?.modified().ok()?;
    Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64)
}

/// The join problem of the game in `root` that started at `started`, read
/// from its `logs/latest.log` once it ended.
pub fn detect(root: &Path, started: i64) -> Option<JoinProblem> {
    let log = root.join("logs").join("latest.log");
    if modified_secs(&log).is_some_and(|t| t < started) {
        return None;
    }
    let text = crate::logs::tail(&log, 4 << 20);
    let mut p = in_log(&text)?;
    p.when = crate::time::now();
    Some(p)
}

impl JoinProblem {
    /// Matches [`Self::mods`] to jars: `files` are the mods folder's file
    /// names and `ids` gives each jar's own mod ids.
    pub fn match_jars(&mut self, files: &[String], ids: impl Fn(&str) -> Vec<String>) {
        let owned: Vec<(String, Vec<String>)> = files.iter().filter(|f| f.ends_with(".jar")).map(|f| (f.clone(), ids(f))).collect();
        self.jars = self
            .mods
            .iter()
            .map(|m| owned.iter().find(|(_, own)| own.iter().any(|i| i.eq_ignore_ascii_case(m))).map_or(String::new(), |(f, _)| f.clone()))
            .collect();
    }
}

pub fn load(root: &Path) -> Option<JoinProblem> {
    serde_json::from_slice(&fs::read(root.join(FILE)).ok()?).ok()
}

pub fn save(root: &Path, p: &JoinProblem) {
    if let Ok(b) = serde_json::to_vec_pretty(p) {
        let _ = fs::write(root.join(FILE), b);
    }
}

pub fn clear(root: &Path) {
    let _ = fs::remove_file(root.join(FILE));
}

// ---- the server's log ------------------------------------------------------

/// A player the server turned away, and why.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Refusal {
    pub player: String,
    pub kind: JoinKind,
    pub why: String,
    pub fix: String,
    pub mods: Vec<String>,
    /// The server's own words.
    pub reason: String,
}

/// `Steve lost connection: …`, `Steve (uuid) lost connection: …`,
/// `Disconnecting Steve (/1.2.3.4:5): …`, and the old
/// `Disconnecting com.mojang.authlib.GameProfile@…[id=…,name=Steve,…] (…): …`.
static SERVER_LOST: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?:\]: |^)(?:Disconnecting )?(?:com\.mojang\.authlib\.GameProfile@\w+\[[^\]]*name=([\w]{1,16})[^\]]*\]|([\w]{1,16})(?:\[[^\]]*\])?)(?: \([^)]*\))?(?: lost connection|):? (.+)$")
});

/// Players a server's log shows it turned away for a mismatch (mods,
/// registries, versions), oldest first. Normal quits and plain timeouts
/// aren't refusals.
pub fn server_refusals(log: &str) -> Vec<Refusal> {
    let mut out = Vec::new();
    let lines: Vec<&str> = log.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if !(line.contains("lost connection") || line.contains("Disconnecting ")) {
            continue;
        }
        let Some(c) = SERVER_LOST.captures(line) else { continue };
        let player = c.get(1).or(c.get(2)).map_or("", |m| m.as_str()).to_string();
        let reason = plain(&c[3]);
        let more: String = lines[i + 1..lines.len().min(i + 12)]
            .iter()
            .take_while(|l| !l.starts_with('['))
            .map(|l| format!(" {}", l.trim()))
            .collect();
        let Some(r) = classify(&format!("{reason}{more}")) else { continue };
        // A player whose connection dropped isn't a mismatch.
        if r.kind == JoinKind::Network || r.kind == JoinKind::Session && !reason.to_ascii_lowercase().contains("session") {
            continue;
        }
        // Said to the server's owner, not the player.
        let fix = match r.kind {
            JoinKind::Mods | JoinKind::Registry => "The player's pack needs the server's mods, at the same versions.".to_string(),
            JoinKind::Loader => "The player needs the server's NeoForge build, or the server theirs.".to_string(),
            JoinKind::GameVersion => "The player needs the server's Minecraft version.".to_string(),
            _ => r.fix,
        };
        out.push(Refusal { player, kind: r.kind, why: r.why, fix, mods: r.mods, reason: cut(reason.trim(), 300) });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/joinfail_fixtures").join(name);
        fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    }

    fn client(name: &str) -> JoinProblem {
        in_log(&fixture(name)).unwrap_or_else(|| panic!("{name}: no join problem found"))
    }

    #[test]
    fn neoforge_channel_mismatch_names_the_mods() {
        let p = client("neo-channels.log");
        assert_eq!(p.kind, JoinKind::Mods);
        assert_eq!(p.target, "play.example.net");
        assert!(p.mods.contains(&"create".to_string()), "{:?}", p.mods);
        assert!(p.mods.contains(&"sophisticatedbackpacks".to_string()), "{:?}", p.mods);
        assert!(!p.mods.contains(&"minecraft".to_string()));
        // The library goes last.
        assert_eq!(p.mods.last().map(String::as_str), Some("balm"), "{:?}", p.mods);
    }

    #[test]
    fn forge_mismatched_channel_list() {
        let p = client("forge-mismatched-channels.log");
        assert_eq!(p.kind, JoinKind::Mods);
        assert!(p.mods.contains(&"waystones".to_string()), "{:?}", p.mods);
    }

    #[test]
    fn neoforge_version_differs() {
        let p = client("neo-loader-version.log");
        assert_eq!(p.kind, JoinKind::Loader);
        assert_eq!(p.versions.as_ref().map(|v| v.1.as_str()), Some("21.1.77"));
        assert_eq!(p.target, "192.168.50.3:25570");
    }

    #[test]
    fn server_mods_the_client_lacks() {
        let p = client("neo-requires-mods.log");
        assert_eq!(p.kind, JoinKind::Loader);
    }

    #[test]
    fn registry_mismatch_looks_past_libraries() {
        let p = client("neo-registry.log");
        assert_eq!(p.kind, JoinKind::Registry);
        assert!(p.mods.contains(&"mysticalagriculture".to_string()), "{:?}", p.mods);
        assert!(!p.mods.contains(&"architectury".to_string()), "{:?}", p.mods);
    }

    #[test]
    fn missing_registries() {
        let p = client("neo-missing-registries.log");
        assert_eq!(p.kind, JoinKind::Registry);
        assert!(p.mods.contains(&"ars_nouveau".to_string()), "{:?}", p.mods);
    }

    #[test]
    fn vanilla_outdated_client_and_server() {
        let p = client("vanilla-outdated-client.log");
        assert_eq!(p.kind, JoinKind::GameVersion);
        assert_eq!(p.versions.as_ref().map(|v| v.1.as_str()), Some("1.21.4"));
        assert!(p.why.contains("newer"));
        let p = client("vanilla-outdated-server.log");
        assert_eq!(p.kind, JoinKind::GameVersion);
        assert!(p.why.contains("older"));
    }

    #[test]
    fn invalid_session_and_auth_down() {
        let p = client("vanilla-invalid-session.log");
        assert_eq!(p.kind, JoinKind::Session);
        assert!(p.fix.contains("Sign in"));
        let p = client("vanilla-auth-down.log");
        assert_eq!(p.kind, JoinKind::Session);
        assert!(p.fix.contains("Wait"));
    }

    #[test]
    fn unreachable_server() {
        let p = client("vanilla-connection-refused.log");
        assert_eq!(p.kind, JoinKind::Network);
        assert_eq!(p.target, "mc.example.org");
        let p = client("vanilla-timed-out.log");
        assert_eq!(p.kind, JoinKind::Network);
    }

    #[test]
    fn world_data_packs_and_newer_save() {
        let p = client("world-datapacks.log");
        assert_eq!(p.kind, JoinKind::DataPacks);
        assert!(p.world);
        assert!(p.target.is_empty());
        let p = client("world-newer.log");
        assert_eq!(p.kind, JoinKind::NewerWorld);
        assert!(p.world);
    }

    #[test]
    fn other_reason_is_quoted() {
        let p = client("other-whitelist.log");
        assert_eq!(p.kind, JoinKind::Other);
        assert!(p.why.contains("You are not white-listed on this server!"), "{}", p.why);
    }

    #[test]
    fn a_normal_quit_is_not_a_problem() {
        assert_eq!(in_log(&fixture("normal-quit.log")), None);
    }

    #[test]
    fn a_failure_then_a_join_is_not_a_problem() {
        assert_eq!(in_log(&fixture("failed-then-joined.log")), None);
    }

    #[test]
    fn server_log_refusals() {
        let r = server_refusals(&fixture("server-refusals.log"));
        let who: Vec<(&str, JoinKind)> = r.iter().map(|r| (r.player.as_str(), r.kind)).collect();
        assert_eq!(
            who,
            vec![("Alex", JoinKind::Mods), ("Steve", JoinKind::GameVersion), ("Notch", JoinKind::Registry), ("Herobrine", JoinKind::Mods)]
        );
        assert!(r[0].mods.contains(&"create".to_string()), "{:?}", r[0].mods);
    }

    #[test]
    fn plain_reads_text_components() {
        let t = plain("translation{key='multiplayer.disconnect.outdated_client', args=[1.21.4]}");
        assert_eq!(classify(&t).map(|r| r.kind), Some(JoinKind::GameVersion));
    }
}
