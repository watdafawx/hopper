//! A mod's own config files, found by the mod ids in its jar, and resetting
//! them: they go to backups and the mod writes fresh defaults next launch.

use crate::instances::{Addon, Instance};
use std::path::Path;

/// Endings a config's name carries after the mod id (`jei-client.toml`,
/// `create-server.toml`).
const SUFFIXES: &[&str] = &[
    "-client", "_client", "-common", "_common", "-server", "_server", "-startup", "_startup", "-config", "_config",
    "-neoforge", "_neoforge", "-forge", "_forge", "-fabric", "_fabric",
];

fn squash(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// The mod id a top-level entry of `config/` is named after, squashed: the
/// file `jei-client.toml` and the folder `mekanism` give `jei`, `mekanism`.
fn owner(name: &str, is_dir: bool) -> String {
    let l = name.to_lowercase();
    let mut stem = if is_dir { l.as_str() } else { l.split('.').next().unwrap_or(&l) };
    for s in SUFFIXES {
        stem = stem.strip_suffix(s).unwrap_or(stem);
    }
    squash(stem)
}

/// The config files of `addon`, relative to the instance with `/`, file by
/// file: entries of `config/` named after one of the jar's own mod ids.
/// Pack content kept under `config/` (quests, Paxi datapacks, menus) isn't
/// a mod's settings and is left out.
pub fn files(inst: &Instance, addon: &Addon) -> Vec<String> {
    let (ids, _) = crate::deps::own_ids(&inst.addon_path(addon));
    files_for(&inst.root, &ids)
}

fn files_for(root: &Path, ids: &[String]) -> Vec<String> {
    let ids: Vec<String> = ids.iter().map(|i| squash(i)).filter(|i| i.len() >= 3 && i != "minecraft").collect();
    if ids.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir(root.join("config")) else { return out };
    for e in dir.flatten() {
        let path = e.path();
        let is_dir = path.is_dir();
        let name = e.file_name().to_string_lossy().to_string();
        if !ids.contains(&owner(&name, is_dir)) {
            continue;
        }
        let rel = format!("config/{name}");
        if is_dir {
            walk(&path, &rel, &mut out);
        } else {
            out.push(rel);
        }
    }
    out.retain(|r| !crate::packdiff::pack_content(r));
    out.sort();
    out
}

fn walk(dir: &Path, rel: &str, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let r = format!("{rel}/{}", e.file_name().to_string_lossy());
        if e.path().is_dir() {
            walk(&e.path(), &r, out);
        } else {
            out.push(r);
        }
    }
}

/// Moves `files` (from [`files`]) into the instance's backups, where they
/// can be put back. Returns how many moved; a file in use stops it.
pub fn reset(root: &Path, files: &[String]) -> anyhow::Result<usize> {
    let mut n = 0;
    for rel in files {
        if !crate::configdiff::safe_rel(rel) || !rel.starts_with("config/") {
            continue;
        }
        if crate::sync::backup_into(root, &root.join(rel))?.is_some() {
            n += 1;
        }
    }
    // Folders the move left empty go too, so the mod starts clean.
    let mut dirs: Vec<&String> = files.iter().collect();
    dirs.sort_by_key(|r| std::cmp::Reverse(r.matches('/').count()));
    for rel in dirs {
        let mut p = root.join(rel);
        while let Some(parent) = p.parent().map(Path::to_path_buf) {
            if parent == root.join("config") || !parent.starts_with(root.join("config")) {
                break;
            }
            if std::fs::remove_dir(&parent).is_err() {
                break;
            }
            p = parent;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_and_resets() {
        let root = std::env::temp_dir().join(format!("hopper-modconfig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let w = |rel: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "x").unwrap();
        };
        w("config/jei-client.toml");
        w("config/jei/blacklist.json");
        w("config/jeresources-common.toml");
        w("config/create-server.toml");
        w("config/paxi/datapacks/a.zip");
        let ids = ["jei".to_string()];
        assert_eq!(files_for(&root, &ids), ["config/jei-client.toml", "config/jei/blacklist.json"]);
        // Pack content under config/ never counts as a mod's settings.
        assert!(files_for(&root, &["paxi".to_string()]).is_empty());
        let got = files_for(&root, &ids);
        assert_eq!(reset(&root, &got).unwrap(), 2);
        assert!(!root.join("config/jei").exists(), "the emptied folder goes");
        assert!(root.join("config/jeresources-common.toml").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
