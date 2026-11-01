//! A mod's source travels with its jar: in exported packs and share codes,
//! between peers, and to servers on sync. What travels is a pin: the
//! source record with the exact release (`seen`) and sha256, so everyone
//! runs the same file. The receiving side attaches it only to a jar with
//! that sha256, and never trusts the source because a pack says so: the
//! mod shows its chip, and updates wait until the player confirms the
//! source on that PC ([`super::trusted`]).

use super::{ModSource, src_id};
use crate::instances::{Addon, Instance};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Seek, Write};
use std::path::Path;

/// The file in an exported pack (beside its manifest) that lists the pins.
/// Other launchers ignore it.
pub const PACK_FILE: &str = "hopper-sources.json";

/// One source-tracked jar: its file name in `mods/` and its source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    pub file: String,
    pub source: ModSource,
}

/// The pins of an instance's (or server's) source-tracked mods. Undo
/// details (what it followed before) stay home.
pub fn pins(inst: &Instance) -> Vec<Pin> {
    inst.addons
        .iter()
        .filter_map(|a| {
            let mut s = a.source.clone()?;
            s.prev_project = 0;
            s.prev_mr_project.clear();
            Some(Pin { file: a.file_name.clone(), source: s })
        })
        .collect()
}

/// The pins in a folder's `hopper.json`, by lower-case file name, without
/// loading (or importing) the whole instance.
pub fn pins_in(root: &Path) -> HashMap<String, ModSource> {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Lite {
        addons: Vec<Addon>,
    }
    let Ok(b) = std::fs::read(root.join(crate::instances::HOPPER_FILE)) else { return HashMap::new() };
    let lite: Lite = serde_json::from_slice(&b).unwrap_or_default();
    lite.addons
        .into_iter()
        .filter_map(|a| {
            let mut s = a.source?;
            s.prev_project = 0;
            s.prev_mr_project.clear();
            Some((a.file_name.to_lowercase(), s))
        })
        .collect()
}

/// Writes the pins (if any) into a pack being exported.
pub fn write_pack<W: Write + Seek>(z: &mut zip::ZipWriter<W>, inst: &Instance) -> anyhow::Result<()> {
    let p = pins(inst);
    if p.is_empty() {
        return Ok(());
    }
    z.start_file(PACK_FILE, zip::write::SimpleFileOptions::default())?;
    z.write_all(&serde_json::to_vec_pretty(&p)?)?;
    Ok(())
}

/// The pins an exported pack carries (none for packs from elsewhere).
pub fn read_pack<R: Read + Seek>(r: R) -> Vec<Pin> {
    let Ok(mut zip) = zip::ZipArchive::new(r) else { return Vec::new() };
    let Ok(mut f) = zip.by_name(PACK_FILE) else { return Vec::new() };
    let mut s = String::new();
    if f.by_ref().take(4 << 20).read_to_string(&mut s).is_err() {
        return Vec::new();
    }
    serde_json::from_str(&s).unwrap_or_default()
}

/// Attaches pins to the jars of `root`'s mods folder that have the pinned
/// sha256 (a jar with other contents isn't the pinned file, so it stays
/// as it was). Folders without a `hopper.json` are left alone. Returns how
/// many mods now follow a source.
pub fn apply(root: &Path, pins: &[Pin]) -> anyhow::Result<usize> {
    if pins.is_empty() || !root.join(crate::instances::HOPPER_FILE).is_file() {
        return Ok(0);
    }
    let mut n = 0;
    Instance::update_on_disk(root, |inst| n = attach(inst, pins))?;
    Ok(n)
}

/// [`apply`] on an instance in memory (the caller saves it).
pub fn attach(inst: &mut Instance, pins: &[Pin]) -> usize {
    let mods = inst.mods_dir();
    let mut n = 0;
    for p in pins {
        let file = p.file.rsplit(['/', '\\']).next().unwrap_or(&p.file).trim_end_matches(".disabled").to_string();
        let (path, on) = match (mods.join(&file), mods.join(format!("{file}.disabled"))) {
            (a, _) if a.is_file() => (a, true),
            (_, b) if b.is_file() => (b, false),
            _ => continue,
        };
        if !p.source.sha256.is_empty() {
            match super::sha256_file(&path) {
                Ok(h) if h.eq_ignore_ascii_case(&p.source.sha256) => {}
                _ => continue,
            }
        }
        let id = src_id(&format!("{}|{}", p.source.key(), p.source.pattern));
        let i = match inst.addons.iter().position(|a| a.file_name.eq_ignore_ascii_case(&file)) {
            Some(i) => i,
            None => {
                inst.addons.push(Addon {
                    file_name: file.clone(),
                    enabled: on,
                    class_id: crate::cf::CLASS_MODS,
                    date_added: crate::time::now_iso(),
                    ..Default::default()
                });
                inst.addons.len() - 1
            }
        };
        let a = &mut inst.addons[i];
        let mut s = p.source.clone();
        // What it was here before stays known, for Stop following.
        if a.source.is_none() {
            s.prev_project = a.project_id;
            s.prev_mr_project = a.mr_project.clone();
        } else if let Some(old) = &a.source {
            s.prev_project = old.prev_project;
            s.prev_mr_project = old.prev_mr_project.clone();
        }
        a.project_id = id;
        a.mr_project.clear();
        a.mr_version.clear();
        a.mr_sha1.clear();
        if a.name.is_empty() {
            a.name = crate::scan::strip_version_suffix(&file);
        }
        a.page = s.page();
        if a.author.is_empty() {
            a.author = s.publisher.clone();
        }
        super::remember(id, &s, &a.name.clone());
        a.source = Some(s);
        n += 1;
    }
    n
}

/// The pins for `files` (mods jars) of the folder at `root`.
pub fn pins_for(root: &Path, files: &[String]) -> Vec<Pin> {
    let all = pins_in(root);
    files
        .iter()
        .filter_map(|f| {
            let name = f.rsplit(['/', '\\']).next().unwrap_or(f).trim_end_matches(".disabled");
            all.get(&name.to_lowercase()).map(|s| Pin { file: name.to_string(), source: s.clone() })
        })
        .collect()
}

/// After a sync copied jars into `to` (an instance or a server): their
/// pins from `from` go along. A Hopper Server gets them over its API.
pub fn carry(from: &Path, to: &Path, files: &[String]) {
    let pins = pins_for(from, files);
    if pins.is_empty() {
        return;
    }
    let r = match crate::hopperd::locate(to) {
        Some(c) => c.and_then(|(client, id, _)| {
            client.put(&format!("/api/v1/servers/{}/mods/sources", super::enc(&id)), serde_json::json!({ "pins": pins }))?;
            Ok(())
        }),
        None => apply(to, &pins).map(|_| ()),
    };
    if let Err(e) = r {
        log::warn!("sources didn't go along to {}: {e:#}", to.display());
    }
}

/// The mods jars among sync results that landed in `root` (file names).
pub fn copied_jars(results: &[crate::model::SyncResult], root: &Path) -> Vec<String> {
    results
        .iter()
        .filter(|r| r.ok)
        .filter_map(|r| match &r.op {
            crate::model::SyncOp::Copy { dst, .. } if dst.starts_with(root) => {
                let parent = dst.parent()?.file_name()?.to_string_lossy().to_lowercase();
                let name = dst.file_name()?.to_string_lossy().into_owned();
                (parent == "mods" && name.to_lowercase().trim_end_matches(".disabled").ends_with(".jar")).then_some(name)
            }
            _ => None,
        })
        .collect()
}
