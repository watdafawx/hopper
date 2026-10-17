//! Skins to browse: the current skins of well-known players, looked up
//! through Mojang's public API (names to ids in batches of ten, then each
//! profile's textures) and kept in Hopper's cache for a week, so browsing
//! again costs no requests and stays far under Mojang's rate limits.
//! Names that no longer exist are remembered as missing and skipped.

use crate::mcprofile::Variant;
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// Players whose skins are worth a look: Mojang staff, Hermits, SMP and
/// YouTube regulars.
pub const NAMES: &[&str] = &[
    "Notch", "jeb_", "Dinnerbone", "Grumm", "Technoblade", "Dream", "GeorgeNotFound", "Sapnap",
    "TommyInnit", "Tubbo", "Ph1LzA", "WilburSoot", "Ranboo", "Quackity", "KarlJacobs",
    "BadBoyHalo", "Skeppy", "CaptainSparklez", "Grian", "MumboJumbo", "xisumavoid", "iskall85",
    "GoodTimeWithScar", "Docm77", "BdoubleO100", "ZombieCleo", "Keralis", "Stressmonster101",
    "ImpulseSV", "TangoTek", "Rendog", "Joehills", "cubfan135", "Hypnotizd", "PearlescentMoon",
    "falsesymmetry", "GeminiTay", "Smallishbeans", "Smajor1995", "LDShadowLady", "DanTDM",
    "PopularMMOs", "SSundee", "AntVenom", "Vikkstar123", "Aphmau", "Fruitberries", "Purpled",
    "Illumina1337", "Feinberg", "Punz", "Fundy", "Nihachu", "HBomb94", "Eret", "Callahan",
    "Awesamdude", "Antfrost", "Jschlatt", "Seapeekay", "Hannahxxrose", "Foolish_Gamers",
    "Slimecicle", "CaptainPuffy", "Michaelmcchill", "VelvetIsCake", "Shubble", "Krinios",
    "Solidarity", "PeteZahHutt", "Jacksepticeye", "Pewdiepie", "Ethoslab", "Welsknight",
    "VintageBeef", "Xbcrafted", "Mefs", "Thinknoodles", "Wemmbu", "Parrot", "Hbomb", "Fitmc",
    "Popbob", "Hausemaster", "Tapl", "Couriway", "Feinberg", "Doni", "Ludwig", "Skips",
];

/// A player's skin, found.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Found {
    pub name: String,
    pub url: String,
    pub variant: Variant,
}

#[derive(Default, Serialize, Deserialize)]
struct Cache {
    /// Lowercase name → the skin, or None for no such player.
    #[serde(default)]
    players: HashMap<String, Option<Found>>,
    /// When the entries were looked up (unix seconds), per name.
    #[serde(default)]
    checked: HashMap<String, i64>,
}

const KEEP_SECS: i64 = 7 * 86_400;

fn cache_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "Hopper").map(|d| d.cache_dir().join("random-skins.json"))
}

fn load() -> Cache {
    cache_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save(c: &Cache) {
    let Some(p) = cache_path() else { return };
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(b) = serde_json::to_vec(c) {
        let tmp = p.with_extension("json.part");
        if std::fs::write(&tmp, b).is_ok() {
            let _ = std::fs::rename(tmp, p);
        }
    }
}

/// The names in a new order for `seed`, each once.
pub fn shuffled(seed: u64) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = NAMES.to_vec();
    names.sort_unstable_by_key(|n| n.to_lowercase());
    names.dedup_by_key(|n| n.to_lowercase());
    // Fisher–Yates with a small xorshift.
    let mut x = seed | 1;
    for i in (1..names.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        names.swap(i, (x % (i as u64 + 1)) as usize);
    }
    names
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .user_agent(concat!("Hopper/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// The skins of `names` that exist, in order: from the cache, else from
/// Mojang (blocking). Each found skin's PNG is downloaded too.
pub fn resolve(names: &[&str]) -> anyhow::Result<Vec<(Found, PathBuf)>> {
    let mut cache = load();
    let now = crate::time::now();
    let fresh = |c: &Cache, n: &str| c.checked.get(n).is_some_and(|t| now - t < KEEP_SECS);
    let wanted: Vec<String> = names
        .iter()
        .map(|n| n.to_lowercase())
        .filter(|n| !fresh(&cache, n))
        .collect();
    let agent = agent();
    // Names to ids, ten per request.
    let mut ids: HashMap<String, (String, String)> = HashMap::new();
    for chunk in wanted.chunks(10) {
        let mut r = agent
            .post("https://api.mojang.com/profiles/minecraft")
            .send_json(json!(chunk))
            .context("Couldn't reach Mojang")?;
        if !r.status().is_success() {
            anyhow::bail!("Mojang answered {} looking up players.", r.status().as_u16());
        }
        let v: Value = r.body_mut().read_json()?;
        for p in v.as_array().into_iter().flatten() {
            if let (Some(id), Some(name)) = (p["id"].as_str(), p["name"].as_str()) {
                ids.insert(name.to_lowercase(), (id.to_string(), name.to_string()));
            }
        }
        for n in chunk {
            if !ids.contains_key(n) {
                cache.players.insert(n.clone(), None);
                cache.checked.insert(n.clone(), now);
            }
        }
    }
    // Each id's textures, gently.
    for n in &wanted {
        let Some((id, name)) = ids.get(n) else { continue };
        std::thread::sleep(Duration::from_millis(120));
        let mut r = agent
            .get(format!("https://sessionserver.mojang.com/session/minecraft/profile/{id}"))
            .call()
            .context("Couldn't reach Mojang")?;
        if r.status().as_u16() == 429 {
            // Rate limited: keep what we have and try the rest later.
            break;
        }
        let found = if r.status().is_success() {
            let v: Value = r.body_mut().read_json()?;
            crate::mcprofile::skin_from_session_profile(&v).ok().map(|(url, variant)| Found {
                name: name.clone(),
                url,
                variant: variant.unwrap_or_default(),
            })
        } else {
            None
        };
        cache.players.insert(n.clone(), found);
        cache.checked.insert(n.clone(), now);
    }
    save(&cache);
    let mut out = Vec::new();
    for n in names {
        if let Some(Some(f)) = cache.players.get(&n.to_lowercase())
            && let Ok(path) = crate::imgcache::fetch(&f.url)
        {
            out.push((f.clone(), path));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shuffles_every_name_once() {
        let a = shuffled(1);
        let b = shuffled(2);
        assert_ne!(a, b);
        let mut sa = a.clone();
        sa.sort_unstable();
        let mut sb = b.clone();
        sb.sort_unstable();
        assert_eq!(sa, sb);
        let lower: std::collections::HashSet<String> = a.iter().map(|n| n.to_lowercase()).collect();
        assert_eq!(lower.len(), a.len(), "no duplicates");
        assert!(a.iter().all(|n| crate::mcprofile::validate_name(n).is_ok()), "valid names");
    }
}
