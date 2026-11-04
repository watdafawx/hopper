//! A config file on this PC beside the server's copy, setting by setting:
//! which copy of a file the server actually reads, and which settings
//! differ. The Configs tab's compare and Sync's settings rows use it.

use crate::configedit::{self, Entry, Format, Value};
use crate::configindex::Side;

/// One setting both copies have.
#[derive(Clone, Debug)]
pub struct Pair {
    /// The setting as this PC's copy has it (limits and comment from there).
    pub entry: Entry,
    pub client: Value,
    pub server: Value,
}

impl Pair {
    pub fn differs(&self) -> bool {
        !configedit::same(&self.client, &self.server)
    }
}

/// Settings in both texts (format `f`), in this PC's order. Settings only
/// one side has are left out: the mod adds them on its next start.
pub fn pairs(f: Format, client: &str, server: &str) -> Result<Vec<Pair>, String> {
    let c = configedit::parse_as(f, client)?;
    let s = configedit::parse_as(f, server)?;
    let theirs: std::collections::HashMap<(&[String], &str), &Value> =
        s.iter().flat_map(|s| &s.entries).map(|e| ((e.path.as_slice(), e.key.as_str()), &e.value)).collect();
    Ok(c.iter()
        .flat_map(|s| &s.entries)
        .filter_map(|e| theirs.get(&(e.path.as_slice(), e.key.as_str())).map(|v| Pair { entry: e.clone(), client: e.value.clone(), server: (*v).clone() }))
        .collect())
}

/// Whether a file on this PC is worth comparing with the server: a
/// settings file the server reads too (common or server side).
pub fn compared(rel: &str) -> bool {
    Format::of(rel).is_some() && crate::configindex::side_of(rel) != Side::Client && !rel.starts_with("kubejs/")
}

/// Where the server keeps its live copy of this PC's `rel`, from the files
/// it has (`has`). NeoForge 21.1 (FML `ConfigTracker.resolveBasePath`)
/// reads a server config from the world's `serverconfig/` (`level` from
/// server.properties) when that file exists, else from `config/`;
/// `defaultconfigs/` only fills in a missing file and is never read live.
pub fn server_rel(rel: &str, level: &str, has: impl Fn(&str) -> bool) -> Option<String> {
    server_tries(rel, level).into_iter().find(|r| has(r))
}

/// The places [`server_rel`] looks, in order.
pub fn server_tries(rel: &str, level: &str) -> Vec<String> {
    let inner = crate::configowner::inner(rel);
    let mut tries = Vec::new();
    if rel.starts_with("defaultconfigs/") {
        // Seeds, compared with the server's own seeds.
        tries.push(rel.to_string());
    } else {
        if crate::configindex::side_of(rel) == Side::Server {
            tries.push(format!("{level}/serverconfig/{inner}"));
        }
        tries.push(format!("config/{inner}"));
    }
    tries.retain(|r| crate::configdiff::safe_rel(r));
    tries
}

/// Which copy of a server config the game reads, in words for the file's
/// chip. `overridden`: for a `config/` file, a world's `serverconfig/`
/// has the same file (that one wins in that world).
pub fn live_label(rel: &str, overridden: bool) -> Option<&'static str> {
    if rel.starts_with("defaultconfigs/") {
        return Some("Only fills in a missing file");
    }
    if rel.contains("serverconfig/") {
        return Some("In use for this world");
    }
    if crate::configindex::side_of(rel) != Side::Server {
        return None;
    }
    Some(if overridden { "Not used: the world's copy wins" } else { "In use" })
}

/// For a `config/` file: the worlds' `serverconfig/` copies of it among `all`.
pub fn overrides<'a>(rel: &str, all: &'a [String]) -> Vec<&'a String> {
    if !rel.starts_with("config/") {
        return Vec::new();
    }
    let inner = crate::configowner::inner(rel);
    all.iter().filter(|r| r.contains("serverconfig/") && crate::configowner::inner(r) == inner).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_and_places() {
        let c = "#Range: 1 ~ 256\n#Default: 50\nmaxRange = 50\nlog = true\nonlyHere = 1\n";
        let s = "maxRange = 96\nlog = true\n";
        let p = pairs(Format::Toml, c, s).unwrap();
        assert_eq!(p.len(), 2);
        assert!(p[0].differs() && !p[1].differs());
        assert_eq!(p[0].entry.range, Some((Some(1.0), Some(256.0))));
        let has = |r: &str| r == "world/serverconfig/create-server.toml" || r == "config/create-common.toml" || r == "defaultconfigs/other-server.toml";
        // Seeds aren't read live.
        assert_eq!(server_rel("config/other-server.toml", "world", has), None);
        assert_eq!(server_rel("defaultconfigs/other-server.toml", "world", has).as_deref(), Some("defaultconfigs/other-server.toml"));
        let all = vec!["config/create-server.toml".to_string(), "saves/T/serverconfig/create-server.toml".to_string()];
        assert_eq!(overrides("config/create-server.toml", &all).len(), 1);
        assert_eq!(live_label("config/create-server.toml", true), Some("Not used: the world's copy wins"));
        assert_eq!(live_label("config/create-client.toml", false), None);
        assert_eq!(server_rel("config/create-server.toml", "world", has).as_deref(), Some("world/serverconfig/create-server.toml"));
        assert_eq!(server_rel("saves/T/serverconfig/create-server.toml", "world", has).as_deref(), Some("world/serverconfig/create-server.toml"));
        assert_eq!(server_rel("config/create-common.toml", "world", has).as_deref(), Some("config/create-common.toml"));
        assert_eq!(server_rel("config/other-common.toml", "world", has), None);
        assert!(compared("config/create-common.toml") && !compared("config/create-client.toml"));
    }
}
