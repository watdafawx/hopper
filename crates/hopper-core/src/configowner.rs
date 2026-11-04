//! Which mod a config file belongs to, one rule for every screen (the
//! Configs tab, each mod's config button, Sync, the server panel): the
//! file or folder name, minus endings like `-client`, against the mod ids
//! in each jar; then the jar's display name; then the longest mod id the
//! name starts with (`createaddition-common.toml` is Create Crafts &
//! Additions, not Create).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Endings a config's name carries after the mod id (`jei-client.toml`,
/// `create-server.toml`).
pub const SUFFIXES: &[&str] = &[
    "-client", "_client", "-common", "_common", "-server", "_server", "-startup", "_startup", "-config", "_config",
    "-neoforge", "_neoforge", "-forge", "_forge", "-fabric", "_fabric", "-general", "_general",
];

/// Letters and digits, lowercase.
pub fn squash(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// Where config files sit, in front of the part named after the mod.
const BASES: &[&str] = &["config/", "defaultconfigs/", "kubejs/config/"];

/// `rel` without its config folder (`config/`, `defaultconfigs/`,
/// `kubejs/config/`, a world's `serverconfig/`).
pub fn inner(rel: &str) -> &str {
    for b in BASES {
        if let Some(r) = rel.strip_prefix(b) {
            return r;
        }
    }
    if let Some(i) = rel.find("serverconfig/") {
        return &rel[i + "serverconfig/".len()..];
    }
    rel
}

/// The name a config file is named after, squashed: `config/jei-client.toml`
/// and `config/mekanism/general.toml` give `jei` and `mekanism`.
pub fn stem(rel: &str) -> String {
    let l = inner(rel).to_lowercase();
    let (first, nested) = match l.split_once('/') {
        Some((d, _)) => (d, true),
        None => (l.as_str(), false),
    };
    let mut stem = if nested { first } else { first.split('.').next().unwrap_or(first) };
    // `create-client.toml`, but also `ftbquests-client-config`.
    for _ in 0..2 {
        for s in SUFFIXES {
            stem = stem.strip_suffix(s).unwrap_or(stem);
        }
    }
    squash(stem)
}

/// A mod as configs are matched to it.
#[derive(Clone, Debug, Default)]
pub struct ModRef {
    /// How the caller knows it (the jar's file name).
    pub key: String,
    /// Its own mod ids (not the ones nested inside), as written.
    pub ids: Vec<String>,
    /// Its display name.
    pub name: String,
}

impl ModRef {
    /// A jar's own ids and name (cached by `deps`).
    pub fn of_jar(key: String, path: &Path, name: &str) -> ModRef {
        let (ids, jar_name) = crate::deps::own_ids(path);
        ModRef { key, ids, name: if name.is_empty() { jar_name } else { name.to_string() } }
    }
}

/// Reads many jars on a few threads (first time; `deps` caches them).
pub fn of_jars(jars: Vec<(String, PathBuf, String)>) -> Vec<ModRef> {
    let chunk = jars.len().div_ceil(8).max(1);
    std::thread::scope(|s| {
        let parts: Vec<_> = jars
            .chunks(chunk)
            .map(|c| s.spawn(move || c.iter().map(|(k, p, n)| ModRef::of_jar(k.clone(), p, n)).collect::<Vec<_>>()))
            .collect();
        parts.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    })
}

/// The matcher, built once per mod list.
pub struct Owners {
    mods: Vec<ModRef>,
    by_id: HashMap<String, usize>,
    by_name: HashMap<String, usize>,
    /// Ids long enough to match as a prefix, longest first.
    prefixes: Vec<(String, usize)>,
}

impl Owners {
    pub fn new(mods: Vec<ModRef>) -> Owners {
        let (mut by_id, mut by_name, mut prefixes) = (HashMap::new(), HashMap::new(), Vec::new());
        for (i, m) in mods.iter().enumerate() {
            for id in &m.ids {
                let s = squash(id);
                if s.len() < 3 || s == "minecraft" || s == "neoforge" || s == "forge" {
                    continue;
                }
                by_id.entry(s.clone()).or_insert(i);
                if s.len() >= 4 {
                    prefixes.push((s, i));
                }
            }
            let n = squash(&m.name);
            if n.len() >= 4 {
                by_name.entry(n).or_insert(i);
            }
        }
        prefixes.sort_by_key(|(s, _)| std::cmp::Reverse(s.len()));
        Owners { mods, by_id, by_name, prefixes }
    }

    pub fn mods(&self) -> &[ModRef] {
        &self.mods
    }

    /// The mod `rel` belongs to, if one matches.
    pub fn owner(&self, rel: &str) -> Option<&ModRef> {
        let s = stem(rel);
        if s.len() < 3 {
            return None;
        }
        let i = self
            .by_id
            .get(&s)
            .or_else(|| self.by_name.get(&s))
            .copied()
            .or_else(|| self.prefixes.iter().find(|(p, _)| s.starts_with(p.as_str())).map(|(_, i)| *i))?;
        self.mods.get(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(key: &str, ids: &[&str], name: &str) -> ModRef {
        ModRef { key: key.into(), ids: ids.iter().map(|s| s.to_string()).collect(), name: name.into() }
    }

    #[test]
    fn matches() {
        let o = Owners::new(vec![
            m("create.jar", &["create"], "Create"),
            m("cca.jar", &["createaddition"], "Create Crafts & Additions"),
            m("mek.jar", &["mekanism"], "Mekanism"),
            m("ae2.jar", &["ae2"], "Applied Energistics 2"),
            m("ftbq.jar", &["ftbquests"], "FTB Quests"),
            m("bh.jar", &["bhmenu"], "BH Menu"),
        ]);
        let k = |r: &str| o.owner(r).map(|m| m.key.as_str());
        assert_eq!(k("config/create-client.toml"), Some("create.jar"));
        assert_eq!(k("config/createaddition-common.toml"), Some("cca.jar"));
        assert_eq!(k("config/mekanism/general.toml"), Some("mek.jar"));
        assert_eq!(k("config/ae2/client.json"), Some("ae2.jar"));
        assert_eq!(k("saves/T/serverconfig/create-server.toml"), Some("create.jar"));
        assert_eq!(k("defaultconfigs/ftbquests/quests.snbt"), Some("ftbq.jar"));
        assert_eq!(k("config/ftbquests-client-config.snbt"), Some("ftbq.jar"));
        // By display name, and by prefix.
        assert_eq!(k("config/applied-energistics-2.toml"), Some("ae2.jar"));
        assert_eq!(k("config/mekanismtools.toml"), Some("mek.jar"));
        assert_eq!(k("config/async.toml"), None);
        assert_eq!(stem("config/bhmenu-client.toml"), "bhmenu");
    }
}
