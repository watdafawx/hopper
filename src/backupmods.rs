//! World backups made by a backup mod (SimpleBackups, FTB Backups, Advanced
//! Backups, Textile Backup…): which one a server or instance has, where it
//! keeps its backups, and restoring a full one.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// A backup mod Hopper knows.
struct Known {
    /// Matched against jar names (lowercase, without `-`, `_` or spaces).
    jar: &'static str,
    name: &'static str,
    /// Its config file (in `config/`) and the key naming the folder.
    config: &'static str,
    key: &'static str,
    /// Where it backs up when the config doesn't say.
    default_dir: &'static str,
    /// The console command that backs up now (without the slash).
    command: Option<&'static str>,
}

/// Most specific first: `ftbbackups2` before `ftbbackups`.
const KNOWN: &[Known] = &[
    Known {
        jar: "simplebackups",
        name: "SimpleBackups",
        config: "simplebackups-common.toml",
        key: "outputPath",
        default_dir: "simplebackups",
        command: Some("simplebackups backup start"),
    },
    Known {
        jar: "ftbbackups3",
        name: "FTB Backups 3",
        config: "ftbbackups3.snbt",
        key: "backup_location",
        default_dir: "backups",
        command: None,
    },
    Known {
        jar: "ftbbackups2",
        name: "FTB Backups 2",
        config: "ftbbackups2.json",
        key: "backup_location",
        default_dir: "backups",
        command: Some("backup start"),
    },
    Known {
        jar: "ftbbackups",
        name: "FTB Backups",
        config: "ftbbackups.cfg",
        key: "folder",
        default_dir: "backups",
        command: None,
    },
    Known {
        jar: "advancedbackups",
        name: "Advanced Backups",
        config: "AdvancedBackups.properties",
        key: "config.advancedbackups.path",
        default_dir: "backups",
        command: Some("backup start"),
    },
    Known {
        jar: "textilebackup",
        name: "Textile Backup",
        config: "textile_backup.json5",
        key: "path",
        default_dir: "backup",
        command: Some("backup start"),
    },
    Known {
        jar: "aromabackup",
        name: "AromaBackup",
        config: "aromabackup.cfg",
        key: "backupFolder",
        default_dir: "backups",
        command: None,
    },
];

/// A backup mod found in a server or instance, and its backups.
#[derive(Clone, Debug, PartialEq)]
pub struct ModBackups {
    /// "SimpleBackups".
    pub mod_name: String,
    /// Where it keeps backups (it may not exist yet).
    pub dir: PathBuf,
    /// Newest first.
    pub files: Vec<BackupFile>,
    /// The console command that backs up now, when Hopper knows it.
    pub command: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BackupFile {
    pub path: PathBuf,
    /// The path under the mod's folder, for showing.
    pub name: String,
    pub size: u64,
    /// Seconds since the epoch.
    pub modified: i64,
}

impl BackupFile {
    /// Only the files changed since an earlier backup: it can't be
    /// restored by itself.
    pub fn partial(&self) -> bool {
        let n = self.name.to_lowercase();
        ["partial", "incremental", "differential"].iter().any(|w| n.contains(w))
    }

    /// Hopper can restore zips; other archives are left to the mod.
    pub fn restorable(&self) -> bool {
        !self.partial() && self.name.to_lowercase().ends_with(".zip")
    }
}

fn squash(s: &str) -> String {
    s.to_lowercase().replace(['-', '_', ' '], "")
}

/// The value of `key` in a TOML, JSON(5), SNBT or properties file.
fn config_value(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let l = line.trim().trim_start_matches('"');
        if l.starts_with('#') || l.starts_with("//") {
            continue;
        }
        let Some(rest) = l.strip_prefix(key) else { continue };
        let rest = rest.trim_start_matches('"').trim_start();
        let Some(rest) = rest.strip_prefix(['=', ':']) else { continue };
        let v = rest.trim().trim_end_matches(',').trim().trim_matches('"').trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    None
}

/// Archives under `dir` (a few folders deep), newest first.
fn archives(dir: &Path) -> Vec<BackupFile> {
    fn walk(base: &Path, dir: &Path, depth: u32, out: &mut Vec<BackupFile>) {
        for e in fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                if depth < 3 {
                    walk(base, &p, depth + 1, out);
                }
                continue;
            }
            let lower = p.to_string_lossy().to_lowercase();
            let archive = [".zip", ".tar.gz", ".tgz", ".tar.zst", ".tar.xz", ".tar", ".7z"]
                .iter()
                .any(|x| lower.ends_with(x));
            if !archive || m.len() == 0 {
                continue;
            }
            out.push(BackupFile {
                name: p
                    .strip_prefix(base)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/"),
                size: m.len(),
                modified: m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs() as i64),
                path: p,
            });
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, 0, &mut out);
    out.sort_by_key(|f| std::cmp::Reverse(f.modified));
    out
}

/// The backup mods in `root` (a server or instance folder), with their
/// backups.
pub fn detect(root: &Path) -> Vec<ModBackups> {
    let jars: Vec<String> = fs::read_dir(root.join("mods"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| squash(&e.file_name().to_string_lossy()))
        .filter(|n| n.ends_with(".jar"))
        .collect();
    let mut out: Vec<ModBackups> = Vec::new();
    for jar in &jars {
        let Some(k) = KNOWN.iter().find(|k| jar.contains(k.jar)) else { continue };
        if out.iter().any(|m| m.mod_name == k.name) {
            continue;
        }
        let configured = fs::read_to_string(root.join("config").join(k.config))
            .ok()
            .and_then(|t| config_value(&t, k.key));
        let dir = match configured {
            Some(c) if Path::new(&c).is_absolute() => PathBuf::from(c),
            Some(c) => root.join(c.trim_start_matches("./")),
            None => root.join(k.default_dir),
        };
        out.push(ModBackups {
            mod_name: k.name.to_string(),
            files: archives(&dir),
            dir,
            command: k.command.map(str::to_string),
        });
    }
    out
}

/// The folder in `zip` holding `level.dat` ("world/", or "" when the
/// world's files are at the top).
fn world_prefix(z: &mut zip::ZipArchive<fs::File>) -> Option<String> {
    (0..z.len())
        .filter_map(|i| z.name_for_index(i).map(str::to_string))
        .filter_map(|n| {
            let n = n.replace('\\', "/");
            let prefix = n.strip_suffix("level.dat")?;
            (prefix.is_empty() || prefix.ends_with('/')).then(|| prefix.to_string())
        })
        .min_by_key(|p| p.len())
}

/// Replaces `world` with the world in a full backup zip. The current world
/// is moved to `<root>/.hopper-backups/before-restore/` first. The server
/// must be stopped.
pub fn restore(root: &Path, world: &Path, backup: &BackupFile) -> anyhow::Result<PathBuf> {
    if backup.partial() {
        anyhow::bail!(
            "{} only has the files changed since an earlier backup. Restore it with the mod.",
            backup.name
        );
    }
    let mut z = zip::ZipArchive::new(fs::File::open(&backup.path)?)?;
    let Some(prefix) = world_prefix(&mut z) else {
        anyhow::bail!("{} has no world in it (no level.dat).", backup.name);
    };
    let name = world
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "world".into());
    // Unpack next to the world first, so a failure leaves it untouched.
    let tmp = root.join(format!(".hopper-restore-{name}"));
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    let r = (|| -> anyhow::Result<()> {
        for i in 0..z.len() {
            let mut f = z.by_index(i)?;
            let Some(rel) = f.enclosed_name() else { continue };
            let rel = rel.to_string_lossy().replace('\\', "/");
            let Some(rel) = rel.strip_prefix(&prefix) else { continue };
            if rel.is_empty() {
                continue;
            }
            let dest = tmp.join(rel);
            if f.is_dir() {
                fs::create_dir_all(&dest)?;
                continue;
            }
            if let Some(p) = dest.parent() {
                fs::create_dir_all(p)?;
            }
            let mut buf = Vec::with_capacity(f.size() as usize);
            f.read_to_end(&mut buf)?;
            fs::File::create(&dest)?.write_all(&buf)?;
        }
        Ok(())
    })();
    if let Err(e) = r {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e);
    }
    let kept = if world.exists() {
        let stamp = crate::time::now_iso().replace([':', '.'], "-");
        let dir = root.join(crate::sync::BACKUP_DIR).join("before-restore");
        fs::create_dir_all(&dir)?;
        let kept = dir.join(format!("{name}-{stamp}"));
        fs::rename(world, &kept)?;
        Some(kept)
    } else {
        None
    };
    if let Err(e) = fs::rename(&tmp, world) {
        // Put the old world back.
        if let Some(k) = &kept {
            let _ = fs::rename(k, world);
        }
        return Err(e.into());
    }
    Ok(kept.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zip_with(path: &Path, files: &[&str]) {
        let mut z = zip::ZipWriter::new(fs::File::create(path).unwrap());
        for f in files {
            z.start_file(*f, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(f.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }

    #[test]
    fn reads_config_values() {
        assert_eq!(config_value("#x\noutputPath = \"backups\"\n", "outputPath").as_deref(), Some("backups"));
        assert_eq!(
            config_value("{\n  \"backup_location\": \"G:/b\",\n}", "backup_location").as_deref(),
            Some("G:/b")
        );
        assert_eq!(
            config_value("config.advancedbackups.path=./backups\n", "config.advancedbackups.path").as_deref(),
            Some("./backups")
        );
        assert_eq!(config_value("# outputPath = \"x\"", "outputPath"), None);
    }

    #[test]
    fn finds_simplebackups_and_restores() {
        let root = std::env::temp_dir().join(format!("hopper-backupmods-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("mods")).unwrap();
        fs::create_dir_all(root.join("config")).unwrap();
        fs::write(root.join("mods/SimpleBackups-1.21-4.0.30.jar"), "").unwrap();
        fs::write(root.join("mods/jei.jar"), "").unwrap();
        fs::write(root.join("config/simplebackups-common.toml"), "outputPath = \"backups\"\n").unwrap();
        fs::create_dir_all(root.join("backups/world")).unwrap();
        let full = root.join("backups/world/world_2026-10-09_16-30-12.zip");
        zip_with(&full, &["world/level.dat", "world/region/r.0.0.mca"]);
        fs::write(root.join("backups/notes.txt"), "x").unwrap();

        let found = detect(&root);
        assert_eq!(found.len(), 1);
        let m = &found[0];
        assert_eq!(m.mod_name, "SimpleBackups");
        assert_eq!(m.dir, root.join("backups"));
        assert_eq!(m.files.len(), 1);
        assert_eq!(m.files[0].name, "world/world_2026-10-09_16-30-12.zip");
        assert!(m.files[0].restorable());

        let world = root.join("world");
        fs::create_dir_all(&world).unwrap();
        fs::write(world.join("level.dat"), "new").unwrap();
        let kept = restore(&root, &world, &m.files[0]).unwrap();
        assert_eq!(fs::read_to_string(world.join("level.dat")).unwrap(), "world/level.dat");
        assert!(world.join("region/r.0.0.mca").exists());
        assert_eq!(fs::read_to_string(kept.join("level.dat")).unwrap(), "new");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn restores_a_world_at_the_top_and_refuses_others() {
        let root = std::env::temp_dir().join(format!("hopper-backupmods2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let top = root.join("2026-01-01.zip");
        zip_with(&top, &["level.dat", "DIM-1/x"]);
        let f = BackupFile { path: top, name: "2026-01-01.zip".into(), size: 1, modified: 0 };
        restore(&root, &root.join("world"), &f).unwrap();
        assert!(root.join("world/DIM-1/x").exists());

        let none = root.join("none.zip");
        zip_with(&none, &["readme.txt"]);
        let f = BackupFile { path: none, name: "none.zip".into(), size: 1, modified: 0 };
        assert!(restore(&root, &root.join("world"), &f).is_err());
        assert!(root.join("world/level.dat").exists());

        let part = BackupFile {
            path: root.join("x.zip"),
            name: "world/incremental/x.zip".into(),
            size: 1,
            modified: 0,
        };
        assert!(!part.restorable());
        fs::remove_dir_all(&root).unwrap();
    }
}
