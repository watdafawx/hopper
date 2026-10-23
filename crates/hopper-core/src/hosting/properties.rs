//! `server.properties`: read, edit and write it back with its comments,
//! blank lines and key order intact, plus the common keys for a form.
//!
//! [`crate::server`] has a quick read-only reader (`key=value` split, no
//! escapes); this one understands Java's escapes and writes the file.

use std::fs;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
enum Line {
    /// A comment, a blank line or anything that isn't `key=value`, verbatim.
    Other(String),
    /// `raw` is the line as read; `None` once the value has been changed.
    Entry {
        key: String,
        value: String,
        raw: Option<String>,
    },
}

/// A `server.properties` file, editable without losing its layout.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Properties {
    lines: Vec<Line>,
}

impl Properties {
    pub fn parse(text: &str) -> Self {
        let lines = text
            .lines()
            .map(|l| {
                let t = l.trim_start();
                if t.is_empty() || t.starts_with('#') || t.starts_with('!') {
                    return Line::Other(l.to_string());
                }
                match split_entry(t) {
                    Some((k, v)) => Line::Entry {
                        key: unescape(&k),
                        value: unescape(&v),
                        raw: Some(l.to_string()),
                    },
                    None => Line::Other(l.to_string()),
                }
            })
            .collect();
        Properties { lines }
    }

    /// Reads `path`; a missing file is an empty set.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        match fs::read(path) {
            Ok(bytes) => Ok(Self::parse(&String::from_utf8_lossy(&bytes))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(anyhow::Error::new(e).context(format!("reading {}", path.display()))),
        }
    }

    /// Writes through a temp file.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let tmp = path.with_extension("properties.hopper-tmp");
        fs::write(&tmp, self.to_text())?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for l in &self.lines {
            match l {
                Line::Other(s) => out.push_str(s),
                Line::Entry { raw: Some(r), .. } => out.push_str(r),
                Line::Entry { key, value, raw: None } => {
                    out.push_str(&escape(key, true));
                    out.push('=');
                    out.push_str(&escape(value, false));
                }
            }
            out.push('\n');
        }
        out
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.lines.iter().rev().find_map(|l| match l {
            Line::Entry { key: k, value, .. } if k == key => Some(value.as_str()),
            _ => None,
        })
    }

    /// Changes `key` in place, or adds it at the end.
    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        let value = value.into();
        for l in self.lines.iter_mut().rev() {
            if let Line::Entry { key: k, value: v, raw } = l
                && k == key
            {
                if *v != value {
                    *v = value;
                    *raw = None;
                }
                return;
            }
        }
        self.lines.push(Line::Entry {
            key: key.to_string(),
            value,
            raw: None,
        });
    }

    /// Sets `key` only when the file doesn't have it yet.
    pub fn set_default(&mut self, key: &str, value: impl Into<String>) {
        if self.get(key).is_none() {
            self.set(key, value);
        }
    }

    pub fn remove(&mut self, key: &str) {
        self.lines
            .retain(|l| !matches!(l, Line::Entry { key: k, .. } if k == key));
    }

    /// (key, value) in file order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.lines.iter().filter_map(|l| match l {
            Line::Entry { key, value, .. } => Some((key.as_str(), value.as_str())),
            Line::Other(_) => None,
        })
    }

    pub fn port(&self) -> u16 {
        self.get("server-port")
            .and_then(|p| p.trim().parse().ok())
            .unwrap_or(25565)
    }
}

/// Splits at the first unescaped `=`, `:` or whitespace, as Java does.
fn split_entry(line: &str) -> Option<(String, String)> {
    let mut key = String::new();
    let mut chars = line.char_indices().peekable();
    let mut rest_at = line.len();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                key.push(c);
                if let Some((_, n)) = chars.next() {
                    key.push(n);
                }
            }
            '=' | ':' => {
                rest_at = i + 1;
                break;
            }
            c if c.is_whitespace() => {
                // "key  =  value" or "key value".
                let after = line[i..].trim_start();
                rest_at = line.len() - after.len();
                if let Some(s) = after.strip_prefix(['=', ':']) {
                    rest_at = line.len() - s.len();
                }
                break;
            }
            _ => key.push(c),
        }
    }
    if key.is_empty() {
        return None;
    }
    Some((key, line[rest_at..].trim_start().to_string()))
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('f') => out.push('\x0c'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(ch) => out.push(ch),
                    None => {
                        out.push_str("\\u");
                        out.push_str(&hex);
                    }
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Java's `Properties.store` escaping (non-ASCII as `\uXXXX`, which every
/// Minecraft version reads).
fn escape(s: &str, is_key: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.chars().enumerate() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '=' | ':' | '#' | '!' => {
                out.push('\\');
                out.push(c);
            }
            ' ' if is_key || i == 0 => out.push_str("\\ "),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04X}", u));
                }
            }
            c => out.push(c),
        }
    }
    out
}

// ---- the form ----------------------------------------------------------

/// What kind of value a key takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropKind {
    Bool,
    Int { min: i64, max: i64 },
    /// One of these values.
    Enum(&'static [&'static str]),
    String,
}

/// A key the server form shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PropSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: PropKind,
    pub description: &'static str,
    /// Minecraft's own default.
    pub default: &'static str,
}

impl PropSpec {
    /// `Err` with a message when `value` doesn't fit the key.
    pub fn validate(&self, value: &str) -> Result<(), String> {
        let v = value.trim();
        match self.kind {
            PropKind::Bool if v == "true" || v == "false" => Ok(()),
            PropKind::Bool => Err(format!("{} must be true or false", self.label)),
            PropKind::Int { min, max } => match v.parse::<i64>() {
                Ok(n) if (min..=max).contains(&n) => Ok(()),
                _ => Err(format!("{} must be a number from {min} to {max}", self.label)),
            },
            PropKind::Enum(options) if options.contains(&v) => Ok(()),
            PropKind::Enum(options) => {
                Err(format!("{} must be one of: {}", self.label, options.join(", ")))
            }
            PropKind::String => Ok(()),
        }
    }
}

pub const DIFFICULTIES: &[&str] = &["peaceful", "easy", "normal", "hard"];
pub const GAMEMODES: &[&str] = &["survival", "creative", "adventure", "spectator"];

/// The keys most people change, in form order.
pub const COMMON: &[PropSpec] = &[
    PropSpec {
        key: "motd",
        label: "Message of the day",
        kind: PropKind::String,
        description: "Shown under the server's name in the multiplayer list.",
        default: "A Minecraft Server",
    },
    PropSpec {
        key: "max-players",
        label: "Max players",
        kind: PropKind::Int { min: 1, max: 1000 },
        description: "How many players can be online at once.",
        default: "20",
    },
    PropSpec {
        key: "difficulty",
        label: "Difficulty",
        kind: PropKind::Enum(DIFFICULTIES),
        description: "How hard the world is.",
        default: "easy",
    },
    PropSpec {
        key: "gamemode",
        label: "Game mode",
        kind: PropKind::Enum(GAMEMODES),
        description: "The game mode new players get.",
        default: "survival",
    },
    PropSpec {
        key: "hardcore",
        label: "Hardcore",
        kind: PropKind::Bool,
        description: "Players are banned when they die (difficulty is locked to hard).",
        default: "false",
    },
    PropSpec {
        key: "pvp",
        label: "PvP",
        kind: PropKind::Bool,
        description: "Players can hurt each other.",
        default: "true",
    },
    PropSpec {
        key: "online-mode",
        label: "Online mode",
        kind: PropKind::Bool,
        description: "Check players against Microsoft accounts. Turn off only for LAN testing; anyone could join as anyone.",
        default: "true",
    },
    PropSpec {
        key: "white-list",
        label: "Whitelist",
        kind: PropKind::Bool,
        description: "Only players on the whitelist can join.",
        default: "false",
    },
    PropSpec {
        key: "view-distance",
        label: "View distance",
        kind: PropKind::Int { min: 3, max: 32 },
        description: "How many chunks around each player the server sends.",
        default: "10",
    },
    PropSpec {
        key: "simulation-distance",
        label: "Simulation distance",
        kind: PropKind::Int { min: 3, max: 32 },
        description: "How many chunks around each player keep ticking (mobs, crops, machines).",
        default: "10",
    },
    PropSpec {
        key: "server-port",
        label: "Port",
        kind: PropKind::Int { min: 1, max: 65535 },
        description: "The TCP port players connect to.",
        default: "25565",
    },
    PropSpec {
        key: "level-name",
        label: "World folder",
        kind: PropKind::String,
        description: "The world's folder inside the server folder.",
        default: "world",
    },
    PropSpec {
        key: "level-seed",
        label: "Seed",
        kind: PropKind::String,
        description: "Seed for a new world. Empty picks a random one.",
        default: "",
    },
    PropSpec {
        key: "spawn-protection",
        label: "Spawn protection",
        kind: PropKind::Int { min: 0, max: 1000 },
        description: "Radius around spawn only operators can build in. 0 turns it off.",
        default: "16",
    },
    PropSpec {
        key: "allow-flight",
        label: "Allow flight",
        kind: PropKind::Bool,
        description: "Don't kick players for flying. Most modpacks need this on (jetpacks, rings).",
        default: "false",
    },
    PropSpec {
        key: "enable-command-block",
        label: "Command blocks",
        kind: PropKind::Bool,
        description: "Command blocks run their commands.",
        default: "false",
    },
];

/// The form's spec for `key`, if it is one of the common keys.
pub fn spec(key: &str) -> Option<&'static PropSpec> {
    COMMON.iter().find(|s| s.key == key)
}

/// What a new server starts with (keys Minecraft fills in itself are left out).
pub fn defaults(port: u16, motd: &str, modded: bool) -> Properties {
    let mut p = Properties::parse("#Minecraft server properties\n#Written by Hopper\n");
    for s in COMMON {
        let v = match s.key {
            "server-port" => port.to_string(),
            "motd" if !motd.is_empty() => motd.to_string(),
            // Modpacks fly (jetpacks, angel rings) and kick otherwise.
            "allow-flight" if modded => "true".into(),
            _ => s.default.to_string(),
        };
        p.set(s.key, v);
    }
    p.set("query.port", port.to_string());
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "#Minecraft server properties\n#Mon Oct 12 10:00:00 CEST 2026\nallow-flight=false\n\n# my note\nmotd=A \\u00E9 server\\: hi\nlevel-seed=\nserver-port = 25565\nweird line without equals\n";

    #[test]
    fn round_trip_keeps_everything() {
        let p = Properties::parse(FILE);
        assert_eq!(p.to_text(), FILE);
        assert_eq!(p.get("motd"), Some("A é server: hi"));
        assert_eq!(p.get("level-seed"), Some(""));
        assert_eq!(p.port(), 25565);
        assert_eq!(p.get("weird"), Some("line without equals"));
    }

    #[test]
    fn edits_in_place_and_appends() {
        let mut p = Properties::parse(FILE);
        p.set("allow-flight", "true");
        p.set("motd", "Héllo: world");
        p.set("white-list", "true");
        p.set("server-port", "25565"); // unchanged: keeps its spacing
        let text = p.to_text();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "#Minecraft server properties");
        assert_eq!(lines[2], "allow-flight=true");
        assert_eq!(lines[4], "# my note");
        assert_eq!(lines[5], "motd=H\\u00E9llo\\: world");
        assert_eq!(lines[7], "server-port = 25565");
        assert_eq!(lines.last(), Some(&"white-list=true"));
        let again = Properties::parse(&text);
        assert_eq!(again.get("motd"), Some("Héllo: world"));
        let keys: Vec<&str> = again.entries().map(|(k, _)| k).collect();
        assert_eq!(
            keys,
            ["allow-flight", "motd", "level-seed", "server-port", "weird", "white-list"]
        );
    }

    #[test]
    fn specs_cover_the_common_keys_and_validate() {
        for k in [
            "motd", "max-players", "difficulty", "gamemode", "pvp", "online-mode",
            "white-list", "view-distance", "simulation-distance", "server-port",
            "level-name", "level-seed", "spawn-protection", "allow-flight",
            "enable-command-block", "hardcore",
        ] {
            let s = spec(k).unwrap_or_else(|| panic!("{k} missing"));
            assert!(s.validate(s.default).is_ok(), "{k} default invalid");
        }
        assert!(spec("difficulty").unwrap().validate("hard").is_ok());
        assert!(spec("difficulty").unwrap().validate("nightmare").is_err());
        assert!(spec("server-port").unwrap().validate("70000").is_err());
        assert!(spec("pvp").unwrap().validate("yes").is_err());
    }

    #[test]
    fn defaults_for_a_modded_server() {
        let p = defaults(25570, "Hi", true);
        assert_eq!(p.port(), 25570);
        assert_eq!(p.get("allow-flight"), Some("true"));
        assert_eq!(p.get("motd"), Some("Hi"));
        assert!(p.to_text().starts_with("#Minecraft server properties\n"));
    }
}
