//! What Hopper set aside in an instance's `.hopper-backups`: removed mods,
//! packs and replaced files, by when, with where each came from, to look
//! through, restore or delete for good.

use std::fs;
use std::path::{Path, PathBuf};

/// Beside a backup's files: their name → where they came from (relative to
/// the instance).
pub const ORIGINS: &str = ".origins.json";

/// Notes where a file set aside came from.
pub fn record(backup_dir: &Path, saved: &Path, original_rel: &str) {
    let p = backup_dir.join(ORIGINS);
    let mut map: serde_json::Map<String, serde_json::Value> = fs::read(&p)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let name = saved.file_name().unwrap_or_default().to_string_lossy().into_owned();
    map.insert(name, original_rel.replace('\\', "/").into());
    let _ = fs::write(&p, serde_json::to_vec_pretty(&map).unwrap_or_default());
}

#[derive(Clone, Debug, PartialEq)]
pub struct Saved {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    /// Where it goes back to, relative to the instance ("mods/x.jar").
    pub restore_to: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Set {
    pub dir: PathBuf,
    /// Seconds since the epoch.
    pub when: i64,
    pub files: Vec<Saved>,
}

impl Set {
    pub fn size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// Where a file with no recorded origin most likely came from.
fn guess(path: &Path, name: &str) -> String {
    let lower = name.to_lowercase();
    // "2-common.toml": the backup's own de-duplicating prefix.
    let clean = match name.split_once('-') {
        Some((n, rest)) if n.parse::<u32>().is_ok() => rest,
        _ => name,
    };
    let folder = if lower.ends_with(".jar") || lower.ends_with(".jar.disabled") {
        "mods"
    } else if lower.ends_with(".zip") || lower.ends_with(".zip.disabled") {
        let shader = fs::File::open(path)
            .ok()
            .and_then(|f| zip::ZipArchive::new(f).ok())
            .is_some_and(|z| z.file_names().any(|n| n.starts_with("shaders/") || n.contains("/shaders/")));
        if shader { "shaderpacks" } else { "resourcepacks" }
    } else if lower.ends_with(".toml") || lower.ends_with(".json") || lower.ends_with(".json5") || lower.ends_with(".cfg") {
        "config"
    } else {
        "mods"
    };
    format!("{folder}/{clean}")
}

/// Everything set aside in an instance, newest first.
pub fn list(root: &Path) -> Vec<Set> {
    let base = root.join(crate::sync::BACKUP_DIR);
    let mut out: Vec<Set> = fs::read_dir(&base)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let when: i64 = e.file_name().to_string_lossy().parse().ok()?;
            let dir = e.path();
            let origins: serde_json::Map<String, serde_json::Value> = fs::read(dir.join(ORIGINS))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            let mut files = Vec::new();
            walk(&dir, &dir, &origins, &mut files);
            files.sort_by_key(|f| f.name.to_lowercase());
            (!files.is_empty()).then_some(Set { dir, when, files })
        })
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.when));
    out
}

fn walk(base: &Path, dir: &Path, origins: &serde_json::Map<String, serde_json::Value>, out: &mut Vec<Saved>) {
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name == ORIGINS {
            continue;
        }
        if p.is_dir() {
            walk(base, &p, origins, out);
            continue;
        }
        let rel = p.strip_prefix(base).unwrap_or(&p).to_string_lossy().replace('\\', "/");
        let restore_to = match origins.get(&name).and_then(|v| v.as_str()) {
            Some(o) => o.to_string(),
            // Kept under its folder ("mods/x.jar") by drag and drop and sync.
            None if rel.contains('/') => rel.clone(),
            None => guess(&p, &name),
        };
        out.push(Saved {
            size: e.metadata().map_or(0, |m| m.len()),
            name,
            path: p,
            restore_to,
        });
    }
}

/// Puts a file back where it came from. A file already there is kept: the
/// restored one gets " (restored)" in its name instead.
pub fn restore(root: &Path, f: &Saved) -> anyhow::Result<PathBuf> {
    let mut dest = root.join(&f.restore_to);
    if dest.exists() {
        let stem = dest.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let ext = dest.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        dest = dest.with_file_name(format!("{stem} (restored){ext}"));
    }
    if let Some(p) = dest.parent() {
        fs::create_dir_all(p)?;
    }
    if fs::rename(&f.path, &dest).is_err() {
        fs::copy(&f.path, &dest)?;
        fs::remove_file(&f.path)?;
    }
    // An emptied backup folder goes.
    if let Some(dir) = f.path.parent()
        && fs::read_dir(dir).is_ok_and(|mut d| d.all(|e| e.is_ok_and(|e| e.file_name() == ORIGINS)))
    {
        let _ = fs::remove_dir_all(dir);
    }
    Ok(dest)
}

/// Sends a whole backup to the Recycle Bin (or deletes it on a network
/// drive, which has none). `permanent` skips the Recycle Bin.
pub fn delete(set: &Set, permanent: bool) -> anyhow::Result<()> {
    if permanent {
        crate::instance_ops::remove_for_good(&set.dir)?;
    } else if crate::instance_ops::is_remote_drive(&set.dir) {
        fs::remove_dir_all(&set.dir)?;
    } else {
        trash::delete(&set.dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_and_restores() {
        let root = std::env::temp_dir().join(format!("hopper-backups-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("resourcepacks")).unwrap();
        fs::create_dir_all(root.join("mods")).unwrap();
        // A pack removed now (recorded), a mod removed earlier (guessed).
        fs::write(root.join("resourcepacks/Bare Bones.zip"), "zip").unwrap();
        let saved = crate::sync::backup_into(&root, &root.join("resourcepacks/Bare Bones.zip")).unwrap().unwrap();
        let old = root.join(crate::sync::BACKUP_DIR).join("1000");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("jei.jar"), "jar").unwrap();

        let sets = list(&root);
        assert_eq!(sets.len(), 2);
        assert_eq!(sets[0].files[0].restore_to, "resourcepacks/Bare Bones.zip");
        assert_eq!(sets[1].files[0].restore_to, "mods/jei.jar");

        let back = restore(&root, &sets[0].files[0]).unwrap();
        assert_eq!(back, root.join("resourcepacks/Bare Bones.zip"));
        assert!(!saved.exists());
        // Its emptied backup folder went with it.
        assert_eq!(list(&root).len(), 1);

        // Something already there stays; the restored copy gets a new name.
        fs::write(root.join("mods/jei.jar"), "newer").unwrap();
        let back = restore(&root, &list(&root)[0].files[0]).unwrap();
        assert_eq!(back, root.join("mods/jei (restored).jar"));
        assert_eq!(fs::read_to_string(root.join("mods/jei.jar")).unwrap(), "newer");
        fs::remove_dir_all(&root).unwrap();
    }
}
