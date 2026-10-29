//! Resource packs, shader packs and data packs: where they live, the game's
//! pack order, the active shader, and copying/moving packs between places.

use crate::instance_ops as ops;
use crate::instances::Instance;
use std::fs;
use std::path::{Path, PathBuf};

/// Where packs of a kind can live for an instance.
#[derive(Clone, Debug, PartialEq)]
pub struct Location {
    /// Stable key: "instance", "paxi", "globalpacks", "openloader", "paxi-optional", "world:<folder name>" ...
    pub key: String,
    /// "This instance", "Paxi", "Global Packs", "Paxi optional", or the world's name.
    pub label: String,
    pub dir: PathBuf,
    /// Loaded into every world (global loader folders), vs one world.
    pub every_world: bool,
    /// A world's folder ("world:" keys only), for sorting by last played.
    pub world: Option<PathBuf>,
    /// Packs present (files/folders that are packs; ".disabled" ones count).
    pub count: usize,
}

// Paxi's optional data packs; shown only when the folder exists.
const PAXI_OPTIONAL: &str = "config/paxi/optional_data";

fn loc(key: &str, label: &str, dir: PathBuf, every_world: bool, world: Option<PathBuf>) -> Location {
    let count = ops::datapacks(&dir).len();
    Location { key: key.into(), label: label.into(), dir, every_world, world, count }
}

/// Key for a global folder: the loader name its mod jar is matched by.
fn global_key(dir: &str, table: &[(&str, &str, &str)]) -> String {
    table.iter().find(|(d, _, _)| *d == dir).map_or(String::new(), |t| t.2.into())
}

/// Locations for `class` (CLASS_RESOURCE_PACKS, CLASS_SHADERS, CLASS_DATAPACKS).
pub fn locations(inst: &Instance, class: u32) -> Vec<Location> {
    use crate::cf::{CLASS_DATAPACKS, CLASS_RESOURCE_PACKS, CLASS_SHADERS};
    let mut out = Vec::new();
    if class == CLASS_RESOURCE_PACKS {
        out.push(loc("instance", "This instance", inst.root.join("resourcepacks"), false, None));
        for (dir, name) in ops::global_resourcepack_dirs(inst) {
            let key = global_key(dir, ops::GLOBAL_RESOURCEPACK_DIRS);
            out.push(loc(&key, name, inst.root.join(dir), true, None));
        }
    } else if class == CLASS_SHADERS {
        out.push(loc("instance", "This instance", inst.root.join("shaderpacks"), false, None));
    } else if class == CLASS_DATAPACKS {
        let jars: Vec<String> = fs::read_dir(inst.mods_dir())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_lowercase())
            .collect();
        for (dir, name, jar) in ops::GLOBAL_DATAPACK_DIRS {
            if inst.root.join(dir).is_dir() || jars.iter().any(|j| j.contains(jar)) {
                out.push(loc(jar, name, inst.root.join(dir), true, None));
            }
        }
        if inst.root.join(PAXI_OPTIONAL).is_dir() {
            out.push(loc("paxi-optional", "Paxi optional", inst.root.join(PAXI_OPTIONAL), true, None));
        }
        for w in ops::worlds(inst) {
            let folder = w.dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
            out.push(loc(
                &format!("world:{folder}"),
                &w.name,
                w.dir.join("datapacks"),
                false,
                Some(w.dir.clone()),
            ));
        }
    }
    out
}

/// The JSON array on an options.txt line `key:[...]`.
fn options_list(text: &str, key: &str) -> Option<Vec<String>> {
    let prefix = format!("{key}:");
    let v = text.lines().find_map(|l| l.strip_prefix(&prefix))?;
    serde_json::from_str(v.trim()).ok()
}

/// Resource pack file names in the game's order, top (wins) first.
pub fn resourcepack_order(inst: &Instance) -> Vec<String> {
    let text = fs::read_to_string(inst.root.join("options.txt")).unwrap_or_default();
    let mut v: Vec<String> = options_list(&text, "resourcePacks")
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| e.strip_prefix("file/").map(str::to_string))
        .collect();
    v.reverse();
    v
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A free name in `dir` for `name` ("x (2).zip", "x (3).zip", ...).
fn free_name(dir: &Path, name: &str) -> String {
    if !dir.join(name).exists() {
        return name.into();
    }
    let (base, disabled) = match name.strip_suffix(".disabled") {
        Some(b) => (b, ".disabled"),
        None => (name, ""),
    };
    let (stem, ext) = match base.rfind('.') {
        Some(i) if i > 0 => (&base[..i], &base[i..]),
        _ => (base, ""),
    };
    (2..)
        .map(|n| format!("{stem} ({n}){ext}{disabled}"))
        .find(|c| !dir.join(c).exists())
        .unwrap_or_else(|| name.into())
}

fn copy_all(src: &Path, dst: &Path) -> anyhow::Result<()> {
    if src.is_dir() {
        fs::create_dir_all(dst)?;
        for e in fs::read_dir(src)? {
            let e = e?;
            copy_all(&e.path(), &dst.join(e.file_name()))?;
        }
    } else {
        if let Some(p) = dst.parent() {
            fs::create_dir_all(p)?;
        }
        fs::copy(src, dst)?;
    }
    Ok(())
}

/// Copies `path` into `<root>/.hopper-backups/<secs>/`, noting its origin.
/// Does nothing when it doesn't exist.
fn backup_copy(root: &Path, path: &Path) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let dir = root.join(crate::sync::BACKUP_DIR).join(unix_secs().to_string());
    fs::create_dir_all(&dir)?;
    crate::retention::schedule(root);
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let saved = dir.join(free_name(&dir, &name));
    copy_all(path, &saved)?;
    let rel = path
        .strip_prefix(root)
        .map_or_else(|_| name.clone(), |r| r.to_string_lossy().into_owned());
    crate::backups::record(&dir, &saved, &rel);
    Ok(())
}

fn line_end(l: &str) -> &'static str {
    if l.ends_with("\r\n") {
        "\r\n"
    } else if l.ends_with('\n') {
        "\n"
    } else {
        ""
    }
}

/// Replaces the first line for which `is_key` holds (keeping its ending),
/// or appends `new` when none.
fn set_line(text: &str, is_key: impl Fn(&str) -> bool, new: &str) -> String {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut found = false;
    let mut out = String::new();
    for l in text.split_inclusive('\n') {
        if !found && is_key(l) {
            found = true;
            out.push_str(new);
            out.push_str(line_end(l));
        } else {
            out.push_str(l);
        }
    }
    if !found {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(nl);
        }
        out.push_str(new);
        out.push_str(nl);
    }
    out
}

/// Rewrites options.txt `resourcePacks` so `top_first` are on, in that order.
pub fn set_resourcepack_order(inst: &Instance, top_first: &[String]) -> anyhow::Result<()> {
    let path = inst.root.join("options.txt");
    let text = fs::read_to_string(&path).unwrap_or_default();
    backup_copy(&inst.root, &path)?;
    let existing = options_list(&text, "resourcePacks").unwrap_or_default();
    let others: Vec<String> = existing.iter().filter(|e| !e.starts_with("file/")).cloned().collect();
    // Where the file entries sat among the others (vanilla stays beneath).
    let at = existing
        .iter()
        .position(|e| e.starts_with("file/"))
        .map_or(others.len(), |p| existing[..p].iter().filter(|e| !e.starts_with("file/")).count());
    let mut list: Vec<String> = others[..at].to_vec();
    list.extend(top_first.iter().rev().map(|n| format!("file/{n}")));
    list.extend(others[at..].iter().cloned());
    let line = format!("resourcePacks:{}", serde_json::to_string(&list)?);
    let mut out = set_line(&text, |l| l.starts_with("resourcePacks:"), &line);
    if let Some(inc) = options_list(&out, "incompatibleResourcePacks") {
        let keep: Vec<String> = inc
            .into_iter()
            .filter(|e| !e.starts_with("file/") || list.contains(e))
            .collect();
        let line = format!("incompatibleResourcePacks:{}", serde_json::to_string(&keep)?);
        out = set_line(&out, |l| l.starts_with("incompatibleResourcePacks:"), &line);
    }
    fs::write(&path, out)?;
    Ok(())
}

/// Sets `key=value` in .properties text, keeping other lines and order.
fn set_property(text: &str, key: &str, value: &str) -> String {
    let is_key = |l: &str| {
        let t = l.trim_start();
        !t.starts_with('#')
            && !t.starts_with('!')
            && t.split(['=', ':']).next().is_some_and(|k| k.trim() == key)
    };
    set_line(text, is_key, &format!("{key}={value}"))
}

/// Picks the active shader for Iris/Oculus (None switches shaders off).
pub fn set_active_shader(inst: &Instance, pack: Option<&str>) -> anyhow::Result<()> {
    let jars: Vec<String> = fs::read_dir(inst.mods_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_lowercase())
        .filter(|n| n.ends_with(".jar"))
        .collect();
    let cfg = inst.root.join("config");
    let mut files: Vec<&str> = Vec::new();
    for (file, jar) in [("iris.properties", "iris"), ("oculus.properties", "oculus")] {
        if jars.iter().any(|n| n.starts_with(jar)) || cfg.join(file).exists() {
            files.push(file);
        }
    }
    if files.is_empty() {
        files.push("iris.properties");
    }
    fs::create_dir_all(&cfg)?;
    for f in files {
        let path = cfg.join(f);
        let mut text = fs::read_to_string(&path).unwrap_or_default();
        backup_copy(&inst.root, &path)?;
        if let Some(name) = pack {
            text = set_property(&text, "shaderPack", &name.replace(':', "\\:"));
            text = set_property(&text, "enableShaders", "true");
        } else {
            text = set_property(&text, "enableShaders", "false");
        }
        fs::write(&path, text)?;
    }
    Ok(())
}

/// Copies a pack (file or folder) into `dest_dir`; a clash gets " (2)" etc.
pub fn copy_pack(src: &Path, dest_dir: &Path) -> anyhow::Result<PathBuf> {
    fs::create_dir_all(dest_dir)?;
    let name = src
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("no file name"))?
        .to_string_lossy()
        .into_owned();
    let dest = dest_dir.join(free_name(dest_dir, &name));
    copy_all(src, &dest)?;
    Ok(dest)
}

/// Moves a pack into `dest_dir`, after backing the source up under `root`.
pub fn move_pack(root: &Path, src: &Path, dest_dir: &Path) -> anyhow::Result<PathBuf> {
    backup_copy(root, src)?;
    fs::create_dir_all(dest_dir)?;
    let name = src
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("no file name"))?
        .to_string_lossy()
        .into_owned();
    let dest = dest_dir.join(free_name(dest_dir, &name));
    if fs::rename(src, &dest).is_err() {
        copy_all(src, &dest)?;
        if src.is_dir() {
            fs::remove_dir_all(src)?;
        } else {
            fs::remove_file(src)?;
        }
    }
    Ok(dest)
}

/// Labels of the other locations holding a pack with the same file name.
pub fn also_in(locations: &[Location], current_key: &str, file_name: &str) -> Vec<String> {
    let want = file_name.to_lowercase();
    locations
        .iter()
        .filter(|l| l.key != current_key)
        .filter(|l| {
            fs::read_dir(&l.dir)
                .into_iter()
                .flatten()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().to_lowercase() == want)
        })
        .map(|l| l.label.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cf::{CLASS_DATAPACKS, CLASS_RESOURCE_PACKS, CLASS_SHADERS};

    fn tmp(tag: &str) -> Instance {
        let root = std::env::temp_dir().join(format!("hopper-packs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Instance { root, ..Default::default() }
    }

    fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"x").unwrap();
    }

    #[test]
    fn locations_by_class() {
        let i = tmp("loc");
        touch(&i.root.join("resourcepacks/a.zip"));
        touch(&i.root.join("config/paxi/resourcepacks/b.zip.disabled"));
        touch(&i.root.join("config/paxi/datapacks/d.zip"));
        touch(&i.root.join("saves/W1/level.dat"));
        touch(&i.root.join("saves/W1/datapacks/e.zip"));
        let r = locations(&i, CLASS_RESOURCE_PACKS);
        assert_eq!(r.iter().map(|l| l.key.as_str()).collect::<Vec<_>>(), ["instance", "paxi"]);
        assert_eq!((r[0].count, r[1].count, r[1].every_world), (1, 1, true));
        assert_eq!(locations(&i, CLASS_SHADERS).len(), 1);
        let d = locations(&i, CLASS_DATAPACKS);
        assert_eq!(d.iter().map(|l| l.key.as_str()).collect::<Vec<_>>(), ["paxi", "world:W1"]);
        assert!(d[1].world.is_some() && !d[1].every_world && d[1].count == 1);
        let _ = fs::remove_dir_all(&i.root);
    }

    #[test]
    fn order_roundtrip() {
        let i = tmp("order");
        fs::write(
            i.root.join("options.txt"),
            "a:1\r\nresourcePacks:[\"vanilla\",\"file/A.zip\",\"mod_resources\",\"file/B.zip\"]\r\nincompatibleResourcePacks:[\"file/B.zip\",\"file/C.zip\"]\r\nz:2\r\n",
        )
        .unwrap();
        assert_eq!(resourcepack_order(&i), ["B.zip", "A.zip"]);
        set_resourcepack_order(&i, &["C.zip".into(), "A.zip".into()]).unwrap();
        let t = fs::read_to_string(i.root.join("options.txt")).unwrap();
        assert!(t.contains("resourcePacks:[\"vanilla\",\"file/A.zip\",\"file/C.zip\",\"mod_resources\"]\r\n"), "{t}");
        assert!(t.contains("incompatibleResourcePacks:[\"file/C.zip\"]\r\n"), "{t}");
        assert!(t.starts_with("a:1\r\n") && t.ends_with("z:2\r\n"));
        assert_eq!(resourcepack_order(&i), ["C.zip", "A.zip"]);
        // A backup with an origin was made.
        assert!(!crate::backups::list(&i.root).is_empty());
        let _ = fs::remove_dir_all(&i.root);
    }

    #[test]
    fn order_creates_line() {
        let i = tmp("orderNew");
        set_resourcepack_order(&i, &["A.zip".into()]).unwrap();
        let t = fs::read_to_string(i.root.join("options.txt")).unwrap();
        assert_eq!(t, "resourcePacks:[\"file/A.zip\"]\n");
        let _ = fs::remove_dir_all(&i.root);
    }

    #[test]
    fn shader_set_and_off() {
        let i = tmp("shader");
        touch(&i.root.join("mods/iris-1.0.jar"));
        fs::create_dir_all(i.root.join("config")).unwrap();
        fs::write(i.root.join("config/iris.properties"), "#c\nenableShaders=false\nother=1\n").unwrap();
        set_active_shader(&i, Some("My:Pack.zip")).unwrap();
        let t = fs::read_to_string(i.root.join("config/iris.properties")).unwrap();
        assert_eq!(t, "#c\nenableShaders=true\nother=1\nshaderPack=My\\:Pack.zip\n");
        assert_eq!(ops::active_shader(&i).as_deref(), Some("My:Pack.zip"));
        set_active_shader(&i, None).unwrap();
        assert_eq!(ops::active_shader(&i), None);
        assert!(!i.root.join("config/oculus.properties").exists());
        let _ = fs::remove_dir_all(&i.root);
    }

    #[test]
    fn shader_creates_file() {
        let i = tmp("shaderNew");
        touch(&i.root.join("mods/oculus-1.jar"));
        set_active_shader(&i, Some("S.zip")).unwrap();
        let t = fs::read_to_string(i.root.join("config/oculus.properties")).unwrap();
        assert_eq!(t, "shaderPack=S.zip\nenableShaders=true\n");
        let _ = fs::remove_dir_all(&i.root);
    }

    #[test]
    fn copy_and_move() {
        let i = tmp("cm");
        touch(&i.root.join("a/p.zip"));
        touch(&i.root.join("a/dir/pack.mcmeta"));
        let d = i.root.join("b");
        let c1 = copy_pack(&i.root.join("a/p.zip"), &d).unwrap();
        let c2 = copy_pack(&i.root.join("a/p.zip"), &d).unwrap();
        assert!(c1.ends_with("p.zip") && c2.ends_with("p (2).zip") && c2.exists());
        let cd = copy_pack(&i.root.join("a/dir"), &d).unwrap();
        assert!(cd.join("pack.mcmeta").exists());
        let m = move_pack(&i.root, &i.root.join("a/p.zip"), &d).unwrap();
        assert!(m.ends_with("p (3).zip") && m.exists() && !i.root.join("a/p.zip").exists());
        let sets = crate::backups::list(&i.root);
        assert_eq!(sets[0].files[0].restore_to, "a/p.zip");
        let _ = fs::remove_dir_all(&i.root);
    }

    #[test]
    fn also_in_matches() {
        let i = tmp("also");
        touch(&i.root.join("resourcepacks/Pack.ZIP"));
        touch(&i.root.join("config/paxi/resourcepacks/pack.zip"));
        let l = locations(&i, CLASS_RESOURCE_PACKS);
        assert_eq!(also_in(&l, "instance", "pack.zip"), ["Paxi"]);
        assert_eq!(also_in(&l, "paxi", "PACK.zip"), ["This instance"]);
        assert!(also_in(&l, "paxi", "nope.zip").is_empty());
        let _ = fs::remove_dir_all(&i.root);
    }
}
