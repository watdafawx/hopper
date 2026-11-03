//! One list of a kind's packs across every place they can be: the
//! instance's folder, Paxi, Global Packs, OpenLoader and (data packs) each
//! world. A pack in two places is one entry ("2 copies, using Paxi's"),
//! and each entry is in one group: in game (top wins), always on through a
//! loader, in some worlds, or off. Changing where a pack goes is a plan of
//! small file steps (switch a copy on or off, copy it, move it) so nothing
//! is ever deleted: a copy that stops counting is renamed ".disabled",
//! which both Paxi and the game skip, and a move backs the file up first.

use crate::cf::CLASS_RESOURCE_PACKS;
use crate::instances::Instance;
use crate::packs::{self, Location};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// The instance's own folder's location key.
pub const INSTANCE: &str = "instance";
/// Paxi's location key.
pub const PAXI: &str = "paxi";

/// Which group a pack is listed in.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "key", rename_all = "snake_case")]
pub enum Group {
    /// A resource pack the game uses from the instance's folder (options.txt).
    InGame,
    /// A data pack on in one or more worlds.
    InWorlds,
    /// Loaded into every world by a loader (the location key: "paxi" ...).
    Global(String),
    Off,
}

/// Where a pack should go: what the Where pill and dragging set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "key", rename_all = "snake_case")]
pub enum Target {
    Off,
    InGame,
    Global(String),
    /// Data packs: these worlds' keys ("world:<folder>").
    Worlds(Vec<String>),
}

/// One copy of a pack, in one location.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Copy {
    pub key: String,
    pub label: String,
    pub path: PathBuf,
    /// Not ".disabled".
    pub on: bool,
}

/// A pack, however many places hold it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Entry {
    /// File name without ".disabled", as the first copy found spells it.
    pub file: String,
    pub copies: Vec<Copy>,
    pub group: Group,
    /// With two or more copies: the label of the one in effect.
    pub using: Option<String>,
    /// A loader's copy came with the modpack.
    pub from_pack: bool,
    /// Place in its group's order (1 = first): the game's order for In
    /// game (1 wins), Paxi's load order for Paxi.
    pub rank: Option<usize>,
    /// Data packs: the worlds it's on in (keys).
    pub worlds: Vec<String>,
}

impl Entry {
    pub fn copy_in(&self, key: &str) -> Option<&Copy> {
        self.copies.iter().find(|c| c.key == key)
    }

    /// The group as a target (what to go back to, for Undo).
    pub fn target(&self) -> Target {
        match &self.group {
            Group::InGame => Target::InGame,
            Group::InWorlds => Target::Worlds(self.worlds.clone()),
            Group::Global(k) => Target::Global(k.clone()),
            Group::Off => Target::Off,
        }
    }
}

/// A world's location ("world:<folder>").
pub fn is_world(key: &str) -> bool {
    key.starts_with("world:")
}

/// A loader's location (Paxi, Global Packs, OpenLoader, Paxi optional).
fn is_loader(key: &str) -> bool {
    key != INSTANCE && !is_world(key)
}

fn canon(name: &str) -> &str {
    name.strip_suffix(".disabled").unwrap_or(name)
}

/// Where a server's data packs can be: the loaders it has (folder there,
/// or its jar in mods/), then each world (`world:<folder>`), as
/// `packs::locations` says for an instance.
pub fn server_locations(root: &Path) -> Vec<Location> {
    let jars: Vec<String> = fs::read_dir(root.join("mods"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_lowercase())
        .collect();
    let loc = |key: &str, label: &str, dir: PathBuf, every: bool, world: Option<PathBuf>| {
        let count = crate::instance_ops::datapacks(&dir).len();
        Location { key: key.into(), label: label.into(), dir, every_world: every, world, count }
    };
    let mut out = Vec::new();
    for (dir, name, jar) in crate::instance_ops::GLOBAL_DATAPACK_DIRS {
        if root.join(dir).is_dir() || jars.iter().any(|j| j.contains(jar)) {
            out.push(loc(jar, name, root.join(dir), true, None));
        }
    }
    for w in crate::hosting::worlds::list(root) {
        let dir = root.join(&w.name);
        out.push(loc(&format!("world:{}", w.name), &w.name, dir.join("datapacks"), false, Some(dir)));
    }
    out
}

/// `path` relative to `root`, lower case with forward slashes.
fn rel_lower(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/").to_lowercase()
}

/// Paxi's load order file for this kind of pack.
pub fn paxi_order_file(inst: &Instance, class: u32) -> PathBuf {
    let name = if class == CLASS_RESOURCE_PACKS { "resourcepack_load_order.json" } else { "datapack_load_order.json" };
    inst.root.join("config/paxi").join(name)
}

/// Paxi's load order (first loads first, so later ones win), when the
/// instance has the file.
pub fn paxi_order(inst: &Instance, class: u32) -> Option<Vec<String>> {
    let text = fs::read_to_string(paxi_order_file(inst, class)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(v.get("loadOrder")?.as_array()?.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
}

/// Writes Paxi's load order (other keys in the file stay), backing the old
/// file up first.
pub fn set_paxi_order(inst: &Instance, class: u32, names: &[String]) -> anyhow::Result<()> {
    let path = paxi_order_file(inst, class);
    let mut doc: serde_json::Value = fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(|v: &serde_json::Value| v.is_object())
        .unwrap_or_else(|| serde_json::json!({}));
    doc["loadOrder"] = serde_json::json!(names);
    packs::backup_copy(&inst.root, &path)?;
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    fs::write(&path, serde_json::to_string_pretty(&doc)? + "\n")?;
    Ok(())
}

/// The files the modpack itself put in the instance (lower case, forward
/// slashes): CurseForge's `modpackOverrides`, else the cached pack zip's
/// overrides. `None` when neither says.
pub fn shipped(inst: &Instance) -> Option<HashSet<String>> {
    let doc: Option<serde_json::Value> = fs::read_to_string(inst.root.join("minecraftinstance.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let from_doc = doc.as_ref().and_then(|d| d.get("modpackOverrides")?.as_array().cloned()).map(|a| {
        a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect::<Vec<_>>()
    });
    let list = from_doc.or_else(|| crate::modpack::cached_override_files(inst.pack.as_ref()?.file_id))?;
    Some(list.into_iter().map(|p| p.replace('\\', "/").to_lowercase()).collect())
}

/// Whether a copy came with the modpack: its file (or, for a folder pack,
/// anything in it) is one of the pack's.
fn copy_shipped(root: &Path, c: &Copy, set: &HashSet<String>) -> bool {
    let rel = rel_lower(root, &c.path);
    let rel = canon(&rel);
    let dir = format!("{rel}/");
    set.contains(rel) || set.iter().any(|s| s.starts_with(&dir))
}

/// Every pack of `class` in `locs`, one entry per file name, in group
/// order: resource packs In game, then each loader, then Off; data packs
/// each loader, then In worlds, then Off.
pub fn list(inst: &Instance, class: u32, locs: &[Location]) -> Vec<Entry> {
    let rp = class == CLASS_RESOURCE_PACKS;
    let game: Vec<String> =
        if rp { packs::resourcepack_order(inst).iter().map(|s| s.to_lowercase()).collect() } else { Vec::new() };
    let paxi: Vec<String> = paxi_order(inst, class).unwrap_or_default().iter().map(|s| s.to_lowercase()).collect();
    let shipped = if inst.pack.is_some() { shipped(inst) } else { None };
    let mut out: Vec<Entry> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for l in locs {
        for path in crate::instance_ops::datapacks(&l.dir) {
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            if name.to_ascii_lowercase().starts_with(".hopper-") {
                continue;
            }
            let on = !name.ends_with(".disabled");
            let file = canon(&name).to_string();
            let copy = Copy { key: l.key.clone(), label: l.label.clone(), path, on };
            let k = file.to_lowercase();
            match at.get(&k) {
                Some(i) => out[*i].copies.push(copy),
                None => {
                    at.insert(k, out.len());
                    out.push(Entry {
                        file,
                        copies: vec![copy],
                        group: Group::Off,
                        using: None,
                        from_pack: false,
                        rank: None,
                        worlds: Vec::new(),
                    });
                }
            }
        }
    }
    for e in &mut out {
        let k = e.file.to_lowercase();
        let loader_on = e.copies.iter().find(|c| is_loader(&c.key) && c.on);
        e.worlds = e.copies.iter().filter(|c| is_world(&c.key) && c.on).map(|c| c.key.clone()).collect();
        e.group = if let Some(c) = loader_on {
            Group::Global(c.key.clone())
        } else if rp && e.copies.iter().any(|c| c.key == INSTANCE && c.on) && game.contains(&k) {
            Group::InGame
        } else if !e.worlds.is_empty() {
            Group::InWorlds
        } else {
            Group::Off
        };
        if e.copies.len() > 1 {
            e.using = match &e.group {
                Group::Global(key) => e.copy_in(key).map(|c| c.label.clone()),
                Group::InGame => e.copy_in(INSTANCE).map(|c| c.label.clone()),
                _ => None,
            };
        }
        e.from_pack = match &shipped {
            _ if inst.pack.is_none() => false,
            Some(set) => e.copies.iter().any(|c| is_loader(&c.key) && copy_shipped(&inst.root, c, set)),
            // Nothing says which: a loader's copy is taken to be the pack's.
            None => e.copies.iter().any(|c| is_loader(&c.key)),
        };
    }
    // Ranks: the game's order (top first) and Paxi's load order (listed
    // first, in that order; the rest by name, as Paxi loads them after).
    let place = |list: &[String], f: &str| list.iter().position(|x| canon(x) == f);
    let ranked = |group: Group, list: &[String], out: &mut Vec<Entry>| {
        let mut ixs: Vec<usize> = (0..out.len()).filter(|i| out[*i].group == group).collect();
        ixs.sort_by_key(|i| {
            let f = out[*i].file.to_lowercase();
            (place(list, &f).unwrap_or(usize::MAX), f)
        });
        for (n, i) in ixs.into_iter().enumerate() {
            out[i].rank = Some(n + 1);
        }
    };
    ranked(Group::InGame, &game, &mut out);
    ranked(Group::Global(PAXI.into()), &paxi, &mut out);
    let order = |g: &Group| -> usize {
        let loader = |k: &str| locs.iter().position(|l| l.key == k).unwrap_or(0);
        match g {
            Group::InGame => 0,
            Group::Global(k) if rp => 1 + loader(k),
            Group::Global(k) => loader(k),
            Group::InWorlds => 1000,
            Group::Off => 2000,
        }
    };
    out.sort_by(|a, b| {
        order(&a.group)
            .cmp(&order(&b.group))
            .then(a.rank.unwrap_or(usize::MAX).cmp(&b.rank.unwrap_or(usize::MAX)))
            .then_with(|| a.file.to_lowercase().cmp(&b.file.to_lowercase()))
    });
    out
}

/// One small file change of a move.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    /// Rename a copy on or off (".disabled").
    Enable { path: PathBuf, on: bool },
    /// Copy a pack into a folder, switched on.
    CopyInto { src: PathBuf, dir: PathBuf },
    /// Move a pack into a folder, switched on (backed up first).
    MoveInto { src: PathBuf, dir: PathBuf },
    /// Put a resource pack in the game's order (at the top).
    OrderAdd { file: String },
    /// Take a resource pack out of the game's order.
    OrderRemove { file: String },
}

/// The steps that put `e` in `to`. Copies come before switching things
/// off, since a copy may be made from a file that is then switched off.
pub fn plan(e: &Entry, class: u32, to: &Target, locs: &[Location]) -> Vec<Step> {
    let dir_of = |k: &str| locs.iter().find(|l| l.key == k).map(|l| l.dir.clone());
    let mut first: Vec<Step> = Vec::new();
    let mut off: Vec<Step> = Vec::new();
    let enable = |c: &Copy, on: bool, v: &mut Vec<Step>| {
        if c.on != on {
            v.push(Step::Enable { path: c.path.clone(), on });
        }
    };
    // A source to copy from: one in effect, else any.
    let source = e.copies.iter().find(|c| c.on).or(e.copies.first());
    let in_game = e.group == Group::InGame;
    if class == CLASS_RESOURCE_PACKS {
        let own = e.copy_in(INSTANCE);
        let loaders_on: Vec<&Copy> = e.copies.iter().filter(|c| is_loader(&c.key) && c.on).collect();
        match to {
            Target::Off | Target::Worlds(_) => {
                loaders_on.iter().for_each(|c| enable(c, false, &mut off));
                if in_game {
                    off.push(Step::OrderRemove { file: e.file.clone() });
                }
            }
            Target::InGame => {
                match own {
                    Some(c) => {
                        enable(c, true, &mut first);
                        loaders_on.iter().for_each(|c| enable(c, false, &mut off));
                    }
                    None => {
                        let src = loaders_on.first().copied().or(e.copies.first());
                        if let (Some(src), Some(dir)) = (src, dir_of(INSTANCE)) {
                            first.push(Step::MoveInto { src: src.path.clone(), dir });
                            loaders_on.iter().filter(|c| c.path != src.path).for_each(|c| enable(c, false, &mut off));
                        }
                    }
                }
                if !in_game {
                    first.push(Step::OrderAdd { file: e.file.clone() });
                }
            }
            Target::Global(k) => {
                if let Some(c) = e.copy_in(k) {
                    enable(c, true, &mut first);
                } else if let Some(dir) = dir_of(k) {
                    // The instance's own copy stays (CurseForge keeps it
                    // up to date); a loader's copy moves over.
                    match own.or(source) {
                        Some(c) if c.key == INSTANCE => first.push(Step::CopyInto { src: c.path.clone(), dir }),
                        Some(c) => first.push(Step::MoveInto { src: c.path.clone(), dir }),
                        None => {}
                    }
                }
                loaders_on.iter().filter(|c| c.key != *k).for_each(|c| enable(c, false, &mut off));
                if in_game {
                    off.push(Step::OrderRemove { file: e.file.clone() });
                }
            }
        }
    } else {
        let want = |key: &str| match to {
            Target::Global(k) => k == key,
            Target::Worlds(ws) => ws.iter().any(|w| w == key),
            _ => false,
        };
        let wanted: Vec<String> = match to {
            Target::Global(k) => vec![k.clone()],
            Target::Worlds(ws) => ws.clone(),
            _ => Vec::new(),
        };
        for k in &wanted {
            match e.copy_in(k) {
                Some(c) => enable(c, true, &mut first),
                None => {
                    if let (Some(src), Some(dir)) = (source, dir_of(k)) {
                        first.push(Step::CopyInto { src: src.path.clone(), dir });
                    }
                }
            }
        }
        for c in e.copies.iter().filter(|c| c.on && !want(&c.key)) {
            enable(c, false, &mut off);
        }
    }
    first.extend(off);
    first
}

/// The path of `src` switched `on` or off.
fn switched(src: &Path, on: bool) -> PathBuf {
    let name = src.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let base = canon(&name).to_string();
    src.with_file_name(if on { base } else { format!("{base}.disabled") })
}

/// Runs a plan's file steps (not the order ones). `root` is the instance,
/// where moves back their file up.
pub fn run_files(root: &Path, steps: &[Step]) -> anyhow::Result<()> {
    for s in steps {
        match s {
            Step::Enable { path, on } => {
                let to = switched(path, *on);
                if to != *path {
                    if to.exists() {
                        anyhow::bail!("{} is there already", to.display());
                    }
                    fs::rename(path, &to)?;
                }
            }
            Step::CopyInto { src, dir } => {
                let dest = packs::copy_pack(src, dir)?;
                switch_on(&dest)?;
            }
            Step::MoveInto { src, dir } => {
                let dest = packs::move_pack(root, src, dir)?;
                switch_on(&dest)?;
            }
            Step::OrderAdd { .. } | Step::OrderRemove { .. } => {}
        }
    }
    Ok(())
}

/// A copied or moved ".disabled" file, switched on under a free name.
fn switch_on(dest: &Path) -> anyhow::Result<()> {
    let name = dest.file_name().unwrap_or_default().to_string_lossy().into_owned();
    if let Some(base) = name.strip_suffix(".disabled") {
        let dir = dest.parent().unwrap_or(Path::new("."));
        fs::rename(dest, dir.join(packs::free_name(dir, base)))?;
    }
    Ok(())
}

/// The game's order after a plan's order steps (top first); `order` is
/// the order now.
pub fn order_after(order: &[String], steps: &[Step]) -> Vec<String> {
    let mut v = order.to_vec();
    for s in steps {
        match s {
            Step::OrderAdd { file } => {
                if !v.iter().any(|e| e.eq_ignore_ascii_case(file)) {
                    v.insert(0, file.clone());
                }
            }
            Step::OrderRemove { file } => v.retain(|e| !e.eq_ignore_ascii_case(file)),
            _ => {}
        }
    }
    v
}

/// Runs a whole plan: files, then the game's order when it changed.
pub fn apply(inst: &Instance, steps: &[Step]) -> anyhow::Result<()> {
    run_files(&inst.root, steps)?;
    if steps.iter().any(|s| matches!(s, Step::OrderAdd { .. } | Step::OrderRemove { .. })) {
        let now = packs::resourcepack_order(inst);
        packs::set_resourcepack_order(inst, &order_after(&now, steps))?;
    }
    Ok(())
}

/// `names` (file names) with `file` moved to `at` (0-based), for a drag
/// inside a group.
pub fn reorder(names: &[String], file: &str, at: usize) -> Vec<String> {
    let mut v: Vec<String> = names.iter().filter(|n| !n.eq_ignore_ascii_case(file)).cloned().collect();
    v.insert(at.min(v.len()), file.to_string());
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cf::{CLASS_DATAPACKS, CLASS_RESOURCE_PACKS};

    fn tmp(tag: &str) -> Instance {
        let root = std::env::temp_dir().join(format!("hopper-where-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Instance { root, ..Default::default() }
    }

    fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"x").unwrap();
    }

    fn opts(i: &Instance, packs: &[&str]) {
        let list: Vec<String> = packs.iter().rev().map(|p| format!("file/{p}")).collect();
        fs::write(i.root.join("options.txt"), format!("resourcePacks:{}\n", serde_json::to_string(&list).unwrap())).unwrap();
    }

    fn group_of(v: &[Entry], f: &str) -> Group {
        v.iter().find(|e| e.file == f).unwrap().group.clone()
    }

    #[test]
    fn one_entry_per_pack_with_groups_and_ranks() {
        let i = tmp("list");
        touch(&i.root.join("resourcepacks/A.zip"));
        touch(&i.root.join("resourcepacks/B.zip"));
        touch(&i.root.join("resourcepacks/Dup.zip"));
        touch(&i.root.join("resourcepacks/Off.zip.disabled"));
        touch(&i.root.join("resourcepacks/.hopper-trash.zip"));
        touch(&i.root.join("config/paxi/resourcepacks/dup.zip"));
        touch(&i.root.join("config/paxi/resourcepacks/P1.zip"));
        touch(&i.root.join("config/paxi/resourcepacks/P2.zip"));
        touch(&i.root.join("config/paxi/resourcepacks/Gone.zip.disabled"));
        opts(&i, &["B.zip", "A.zip", "Dup.zip"]);
        fs::write(i.root.join("config/paxi/resourcepack_load_order.json"), r#"{"loadOrder":["P2.zip","dup.zip"]}"#).unwrap();
        let locs = packs::locations(&i, CLASS_RESOURCE_PACKS);
        let v = list(&i, CLASS_RESOURCE_PACKS, &locs);
        let names: Vec<&str> = v.iter().map(|e| e.file.as_str()).collect();
        assert_eq!(names, ["B.zip", "A.zip", "P2.zip", "Dup.zip", "P1.zip", "Gone.zip", "Off.zip"]);
        assert_eq!(v[0].rank, Some(1));
        assert_eq!(v[1].rank, Some(2));
        // Paxi's copy wins over the game's.
        let dup = v.iter().find(|e| e.file == "Dup.zip").unwrap();
        assert_eq!(dup.group, Group::Global("paxi".into()));
        assert_eq!(dup.copies.len(), 2);
        assert_eq!(dup.using.as_deref(), Some("Paxi"));
        assert_eq!(dup.rank, Some(2));
        assert_eq!(group_of(&v, "Gone.zip"), Group::Off);
        assert_eq!(group_of(&v, "Off.zip"), Group::Off);
        // No modpack: nothing is the pack's.
        assert!(v.iter().all(|e| !e.from_pack));
        let _ = fs::remove_dir_all(&i.root);
    }

    #[test]
    fn from_pack_follows_the_overrides() {
        let mut i = tmp("ship");
        i.pack = Some(Default::default());
        touch(&i.root.join("config/paxi/resourcepacks/Shipped.zip"));
        touch(&i.root.join("config/paxi/resourcepacks/Mine.zip"));
        touch(&i.root.join("config/paxi/resourcepacks/Folder/pack.mcmeta"));
        fs::write(
            i.root.join("minecraftinstance.json"),
            r#"{"modpackOverrides":["config\\paxi\\resourcepacks\\Shipped.zip","config\\paxi\\resourcepacks\\Folder\\pack.mcmeta"]}"#,
        )
        .unwrap();
        let locs = packs::locations(&i, CLASS_RESOURCE_PACKS);
        let v = list(&i, CLASS_RESOURCE_PACKS, &locs);
        let fp = |f: &str| v.iter().find(|e| e.file == f).unwrap().from_pack;
        assert!(fp("Shipped.zip") && fp("Folder") && !fp("Mine.zip"));
        let _ = fs::remove_dir_all(&i.root);
    }

    #[test]
    fn paxi_order_keeps_other_keys() {
        let i = tmp("porder");
        touch(&i.root.join("config/paxi/datapack_load_order.json"));
        fs::write(paxi_order_file(&i, CLASS_DATAPACKS), r#"{"other":1,"loadOrder":["a"]}"#).unwrap();
        assert_eq!(paxi_order(&i, CLASS_DATAPACKS).unwrap(), ["a"]);
        set_paxi_order(&i, CLASS_DATAPACKS, &["b".into(), "a".into()]).unwrap();
        assert_eq!(paxi_order(&i, CLASS_DATAPACKS).unwrap(), ["b", "a"]);
        let t = fs::read_to_string(paxi_order_file(&i, CLASS_DATAPACKS)).unwrap();
        assert!(t.contains("\"other\": 1"), "{t}");
        assert!(!crate::backups::list(&i.root).is_empty());
        assert_eq!(reorder(&["a".into(), "b".into(), "c".into()], "C", 0), ["C", "a", "b"]);
        assert_eq!(reorder(&["a".into(), "b".into()], "a", 9), ["b", "a"]);
        let _ = fs::remove_dir_all(&i.root);
    }

    #[test]
    fn resource_pack_moves() {
        let i = tmp("rpmove");
        touch(&i.root.join("resourcepacks/Mine.zip"));
        touch(&i.root.join("config/paxi/resourcepacks/Theirs.zip"));
        opts(&i, &["Mine.zip"]);
        let locs = packs::locations(&i, CLASS_RESOURCE_PACKS);
        let entry = |f: &str| list(&i, CLASS_RESOURCE_PACKS, &locs).into_iter().find(|e| e.file == f).unwrap();
        // In game → Paxi: copied there, out of the game's order.
        let steps = plan(&entry("Mine.zip"), CLASS_RESOURCE_PACKS, &Target::Global("paxi".into()), &locs);
        assert!(matches!(steps[0], Step::CopyInto { .. }) && steps.contains(&Step::OrderRemove { file: "Mine.zip".into() }));
        apply(&i, &steps).unwrap();
        let e = entry("Mine.zip");
        assert_eq!((e.group.clone(), e.copies.len()), (Group::Global("paxi".into()), 2));
        assert!(packs::resourcepack_order(&i).is_empty());
        // Back to In game: Paxi's copy is switched off, nothing deleted.
        apply(&i, &plan(&e, CLASS_RESOURCE_PACKS, &Target::InGame, &locs)).unwrap();
        assert_eq!(entry("Mine.zip").group, Group::InGame);
        assert!(i.root.join("config/paxi/resourcepacks/Mine.zip.disabled").exists());
        // Paxi's only copy → In game: moved (backed up), on top.
        apply(&i, &plan(&entry("Theirs.zip"), CLASS_RESOURCE_PACKS, &Target::InGame, &locs)).unwrap();
        assert!(i.root.join("resourcepacks/Theirs.zip").exists());
        assert!(!i.root.join("config/paxi/resourcepacks/Theirs.zip").exists());
        assert_eq!(packs::resourcepack_order(&i), ["Theirs.zip", "Mine.zip"]);
        assert!(crate::backups::list(&i.root).iter().any(|s| s.files.iter().any(|f| f.restore_to.ends_with("Theirs.zip"))));
        // Off: out of the order, the file stays.
        apply(&i, &plan(&entry("Theirs.zip"), CLASS_RESOURCE_PACKS, &Target::Off, &locs)).unwrap();
        assert_eq!(entry("Theirs.zip").group, Group::Off);
        assert!(i.root.join("resourcepacks/Theirs.zip").exists());
        // Off → Paxi from the instance's off copy: Paxi's copy is on.
        let off_mine = entry("Mine.zip");
        apply(&i, &plan(&off_mine, CLASS_RESOURCE_PACKS, &Target::Global("paxi".into()), &locs)).unwrap();
        assert!(i.root.join("config/paxi/resourcepacks/Mine.zip").exists());
        let _ = fs::remove_dir_all(&i.root);
    }

    #[test]
    fn server_has_loaders_and_worlds() {
        let i = tmp("server");
        touch(&i.root.join("world/level.dat"));
        touch(&i.root.join("world/datapacks/A.zip"));
        touch(&i.root.join("mods/paxi-1.21.jar"));
        let locs = server_locations(&i.root);
        assert_eq!(locs.iter().map(|l| l.key.as_str()).collect::<Vec<_>>(), ["paxi", "world:world"]);
        let v = list(&i, CLASS_DATAPACKS, &locs);
        assert_eq!((v[0].file.as_str(), &v[0].group), ("A.zip", &Group::InWorlds));
        apply(&i, &plan(&v[0], CLASS_DATAPACKS, &Target::Global("paxi".into()), &locs)).unwrap();
        assert!(i.root.join("config/paxi/datapacks/A.zip").exists());
        let _ = fs::remove_dir_all(&i.root);
    }

    #[test]
    fn data_pack_moves() {
        let i = tmp("dpmove");
        touch(&i.root.join("saves/W1/level.dat"));
        touch(&i.root.join("saves/W2/level.dat"));
        touch(&i.root.join("saves/W1/datapacks/Loot.zip"));
        touch(&i.root.join("config/paxi/datapacks/Base.zip"));
        let locs = packs::locations(&i, CLASS_DATAPACKS);
        let entry = |f: &str| list(&i, CLASS_DATAPACKS, &locs).into_iter().find(|e| e.file == f).unwrap();
        let v = list(&i, CLASS_DATAPACKS, &locs);
        assert_eq!(v.iter().map(|e| e.file.as_str()).collect::<Vec<_>>(), ["Base.zip", "Loot.zip"]);
        assert_eq!(entry("Loot.zip").group, Group::InWorlds);
        assert_eq!(entry("Loot.zip").worlds, ["world:W1"]);
        // Both worlds.
        let both = Target::Worlds(vec!["world:W1".into(), "world:W2".into()]);
        apply(&i, &plan(&entry("Loot.zip"), CLASS_DATAPACKS, &both, &locs)).unwrap();
        assert_eq!(entry("Loot.zip").worlds.len(), 2);
        // Just W2: W1's copy switches off.
        apply(&i, &plan(&entry("Loot.zip"), CLASS_DATAPACKS, &Target::Worlds(vec!["world:W2".into()]), &locs)).unwrap();
        assert!(i.root.join("saves/W1/datapacks/Loot.zip.disabled").exists());
        // Always on: copied into Paxi, the world's copy off.
        apply(&i, &plan(&entry("Loot.zip"), CLASS_DATAPACKS, &Target::Global("paxi".into()), &locs)).unwrap();
        let e = entry("Loot.zip");
        assert_eq!(e.group, Group::Global("paxi".into()));
        assert!(e.worlds.is_empty() && e.copies.len() == 3);
        // Paxi's pack into one world, then off everywhere.
        apply(&i, &plan(&entry("Base.zip"), CLASS_DATAPACKS, &Target::Worlds(vec!["world:W1".into()]), &locs)).unwrap();
        assert!(i.root.join("saves/W1/datapacks/Base.zip").exists());
        assert!(i.root.join("config/paxi/datapacks/Base.zip.disabled").exists());
        apply(&i, &plan(&entry("Base.zip"), CLASS_DATAPACKS, &Target::Off, &locs)).unwrap();
        assert_eq!(entry("Base.zip").group, Group::Off);
        assert!(entry("Base.zip").copies.iter().all(|c| !c.on));
        let _ = fs::remove_dir_all(&i.root);
    }
}
