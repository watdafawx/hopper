//! Categories of installed projects, for the badges on mod rows. One
//! batched lookup per site fills a disk cache, so a pack costs a request
//! once and opens with its badges after that.

use crate::instances::Addon;
use std::collections::HashMap;
use std::path::PathBuf;

/// The cache key for an addon's project: "cf:123" or "mr:AbCd". `None` for
/// jars Hopper hasn't matched to a project.
pub fn key(a: &Addon) -> Option<String> {
    if !a.mr_project.is_empty() {
        Some(format!("mr:{}", a.mr_project))
    } else if a.project_id != 0 {
        Some(format!("cf:{}", a.project_id))
    } else {
        None
    }
}

fn file() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "Hopper").map(|d| d.cache_dir().join("modcats.json"))
}

/// Everything fetched so far.
pub fn load() -> HashMap<String, Vec<String>> {
    file()
        .and_then(|f| std::fs::read(f).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Adds `found` to the disk cache.
pub fn save(found: &HashMap<String, Vec<String>>) {
    let Some(f) = file() else { return };
    let mut all = load();
    all.extend(found.iter().map(|(k, v)| (k.clone(), v.clone())));
    if let Ok(b) = serde_json::to_vec(&all) {
        let _ = std::fs::create_dir_all(f.parent().unwrap_or(&f));
        let _ = std::fs::write(f, b);
    }
}

/// Looks up the categories of `keys` (from [`key`]). Projects a site
/// doesn't return get an empty list, so they aren't asked for again; a
/// failed request leaves its keys out, to retry later.
pub fn fetch(keys: &[String]) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    let cf_ids: Vec<u64> = keys
        .iter()
        .filter_map(|k| k.strip_prefix("cf:")?.parse().ok())
        .collect();
    let mr_ids: Vec<String> = keys
        .iter()
        .filter_map(|k| k.strip_prefix("mr:").map(str::to_string))
        .collect();
    if !cf_ids.is_empty()
        && let Ok(mods) = crate::cf::get_mods(&cf_ids)
    {
        for id in &cf_ids {
            let cats = mods
                .iter()
                .find(|m| m.id == *id)
                .map(|m| {
                    m.categories
                        .iter()
                        .filter(|c| c.is_class != Some(true) && !c.name.is_empty())
                        .map(|c| c.name.clone())
                        .collect()
                })
                .unwrap_or_default();
            out.insert(format!("cf:{id}"), cats);
        }
    }
    if !mr_ids.is_empty()
        && let Ok(projects) = crate::modrinth::projects(&mr_ids)
    {
        for id in &mr_ids {
            let cats = projects
                .iter()
                .find(|p| p.id == *id)
                .map(|p| p.categories.iter().filter_map(|c| mr_category(c)).collect())
                .unwrap_or_default();
            out.insert(format!("mr:{id}"), cats);
        }
    }
    out
}

/// "game-mechanics" → "Game Mechanics"; loaders aren't categories.
fn mr_category(slug: &str) -> Option<String> {
    if matches!(
        slug,
        "fabric" | "forge" | "neoforge" | "quilt" | "minecraft" | "datapack" | "iris" | "optifine"
            | "vanilla" | "canvas"
    ) {
        return None;
    }
    let words: Vec<String> = slug
        .split('-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or_default()
        })
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modrinth_slugs_read_as_names() {
        assert_eq!(mr_category("game-mechanics").as_deref(), Some("Game Mechanics"));
        assert_eq!(mr_category("utility").as_deref(), Some("Utility"));
        assert_eq!(mr_category("neoforge"), None);
    }

    #[test]
    fn keys_prefer_modrinth() {
        let mut a = Addon { project_id: 7, ..Default::default() };
        assert_eq!(key(&a).as_deref(), Some("cf:7"));
        a.mr_project = "AbC".into();
        assert_eq!(key(&a).as_deref(), Some("mr:AbC"));
        assert_eq!(key(&Addon::default()), None);
    }
}
