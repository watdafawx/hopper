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

/// How a carry went: what was sent, what the other side attached, and why
/// the rest stayed plain. One value for every sync (instance, local server,
/// Hopper Server), so the words are the same everywhere.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CarryReport {
    /// Mods whose sources were sent along.
    pub sent: usize,
    /// Mods that now follow their source on the other side.
    pub attached: usize,
    /// The Hopper Server has "mods from links" turned off and took none.
    pub off: bool,
    /// The sources couldn't be sent at all (panel unreachable, refused).
    pub failed: Option<String>,
}

impl CarryReport {
    /// What a panel's answer to the pins route says. An answer without
    /// `attached` (an old panel) counts as all attached: nothing to say.
    pub fn from_answer(sent: usize, answer: &serde_json::Value) -> CarryReport {
        let off = answer["off"].as_bool().unwrap_or(false);
        let attached = if off { 0 } else { answer["attached"].as_u64().map_or(sent, |n| (n as usize).min(sent)) };
        CarryReport { sent, attached, off, failed: None }
    }

    /// Adds another carry's numbers (a sync can go both ways).
    pub fn merge(&mut self, o: CarryReport) {
        self.sent += o.sent;
        self.attached += o.attached;
        self.off |= o.off;
        if self.failed.is_none() {
            self.failed = o.failed;
        }
    }

    /// Mods whose source didn't go along.
    pub fn missed(&self) -> usize {
        self.sent.saturating_sub(self.attached)
    }

    /// True when the panel's own setting is the reason, so the fix is to
    /// turn "mods from links" on there.
    pub fn needs_panel_setting(&self) -> bool {
        self.off && self.missed() > 0
    }

    /// The one plain line for the sync's result, or `None` when everything
    /// went along (or nothing had a source).
    pub fn note(&self) -> Option<String> {
        let n = self.missed();
        if n == 0 && self.failed.is_none() {
            return None;
        }
        let n = if n == 0 { self.sent } else { n };
        let mods = if n == 1 { "1 mod's source".to_string() } else { format!("{n} mods' sources") };
        let why = if self.off {
            "this panel has mods from links turned off".to_string()
        } else if let Some(e) = &self.failed {
            format!("they couldn't be sent ({})", e.lines().next().unwrap_or(e).trim())
        } else {
            "those jars aren't the file the source was pinned to".to_string()
        };
        Some(format!("{mods} didn't go along: {why}."))
    }
}

/// After a sync copied jars into `to` (an instance or a server): their
/// pins from `from` go along. A Hopper Server gets them over its API.
pub fn carry(from: &Path, to: &Path, files: &[String]) -> CarryReport {
    carry_ids(from, to, files);
    let pins = pins_for(from, files);
    let sent = pins.len();
    if sent == 0 {
        return CarryReport::default();
    }
    let r = match crate::hopperd::locate(to) {
        Some(c) => c.and_then(|(client, id, _)| {
            let a = client.put(&format!("/servers/{}/mods/sources", super::enc(&id)), serde_json::json!({ "pins": pins }))?;
            Ok(CarryReport::from_answer(sent, &a))
        }),
        // A folder Hopper doesn't track (no hopper.json) has nowhere to keep
        // them: nothing to report, as before.
        None if !to.join(crate::instances::HOPPER_FILE).is_file() => Ok(CarryReport::default()),
        None => apply(to, &pins).map(|n| CarryReport { sent, attached: n, ..Default::default() }),
    };
    r.unwrap_or_else(|e| {
        log::warn!("sources didn't go along to {}: {e:#}", to.display());
        CarryReport { sent, failed: Some(format!("{e:#}")), ..Default::default() }
    })
}

/// A CurseForge or Modrinth mod's record going along with its jar on a
/// sync, so the other side lists it as that mod (icon, page, updates)
/// instead of a bare jar. Attached only to a jar with this sha256.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IdPin {
    pub file: String,
    pub sha256: String,
    pub addon: Addon,
}

/// The records for `files` (mods jars, on or off) of the folder at `root`
/// that CurseForge or Modrinth names. Mods from links travel as [`Pin`]s
/// instead (they wait for the other side to trust their source).
pub fn id_pins_for(root: &Path, files: &[String]) -> Vec<IdPin> {
    if files.is_empty() || crate::hopperd::remote_of(root).is_some() {
        return Vec::new();
    }
    let Some(inst) = crate::instances::load(root) else { return Vec::new() };
    let mods = inst.mods_dir();
    files
        .iter()
        .filter_map(|f| {
            let name = f.rsplit(['/', '\\']).next().unwrap_or(f).trim_end_matches(".disabled");
            let a = inst.addons.iter().find(|a| a.folder() == "mods" && a.file_name.eq_ignore_ascii_case(name))?;
            if a.source.is_some() || (a.project_id == 0 && a.mr_project.is_empty()) {
                return None;
            }
            let path = [mods.join(name), mods.join(format!("{name}.disabled"))].into_iter().find(|p| p.is_file())?;
            let sha256 = super::sha256_file(&path).ok()?;
            let mut addon = a.clone();
            // What the player said about it here stays here; the other side
            // keeps its own (see `attach_ids`).
            addon.file_name = name.to_string();
            addon.from_pack = false;
            addon.locked = false;
            addon.locked_version.clear();
            addon.cf_locked = false;
            addon.date_added.clear();
            // A picture in this PC's cache means nothing over there.
            if !addon.icon.starts_with("http") {
                addon.icon.clear();
            }
            Some(IdPin { file: name.to_string(), sha256, addon })
        })
        .collect()
}

/// Attaches records to the jars of the instance (or server) that have the
/// pinned sha256 and no record naming them yet: a jar Hopper already knows
/// keeps its record. A bare entry (see [`crate::install::is_bare`]) gives
/// way, keeping what it held. Returns how many were attached.
pub fn attach_ids(inst: &mut Instance, pins: &[IdPin]) -> usize {
    let mods = inst.mods_dir();
    let mut n = 0;
    for p in pins {
        let file = p.file.rsplit(['/', '\\']).next().unwrap_or(&p.file).trim_end_matches(".disabled").to_string();
        if file.is_empty() || file.starts_with('.') || !file.to_ascii_lowercase().ends_with(".jar") {
            continue;
        }
        let (path, on) = match (mods.join(&file), mods.join(format!("{file}.disabled"))) {
            (a, _) if a.is_file() => (a, true),
            (_, b) if b.is_file() => (b, false),
            _ => continue,
        };
        match super::sha256_file(&path) {
            Ok(h) if h.eq_ignore_ascii_case(&p.sha256) => {}
            _ => continue,
        }
        let known = inst.addons.iter().any(|a| {
            a.folder() == "mods" && !crate::install::is_bare(a) && a.file_name.trim_end_matches(".disabled").eq_ignore_ascii_case(&file)
        });
        if known {
            continue;
        }
        let mut a = p.addon.clone();
        a.file_name = file;
        a.enabled = on;
        a.source = None;
        a.class_id = crate::cf::CLASS_MODS;
        if a.date_added.is_empty() {
            a.date_added = crate::time::now_iso();
        }
        crate::install::put_identified(inst, a);
        n += 1;
    }
    n
}

/// [`attach_ids`] on the folder at `root` (saved). Folders without a
/// `hopper.json` are left alone.
pub fn apply_ids(root: &Path, pins: &[IdPin]) -> anyhow::Result<usize> {
    if pins.is_empty() || !root.join(crate::instances::HOPPER_FILE).is_file() {
        return Ok(0);
    }
    let mut n = 0;
    Instance::update_on_disk(root, |inst| n = attach_ids(inst, pins))?;
    Ok(n)
}

/// After a sync copied jars into `to`: their CurseForge / Modrinth records
/// from `from` go along. A Hopper Server gets them over its API (an older
/// one without the route keeps plain jars until Identify). Returns how many
/// were attached; failures are only logged, the sync itself went fine.
pub fn carry_ids(from: &Path, to: &Path, files: &[String]) -> usize {
    let pins = id_pins_for(from, files);
    if pins.is_empty() {
        return 0;
    }
    let r = match crate::hopperd::locate(to) {
        Some(c) => c.and_then(|(client, id, _)| {
            let a = client.put(&format!("/servers/{}/mods/records", super::enc(&id)), serde_json::json!({ "records": pins }))?;
            Ok(a["attached"].as_u64().unwrap_or(0) as usize)
        }),
        None => apply_ids(to, &pins),
    };
    r.unwrap_or_else(|e| {
        log::warn!("mod records didn't go along to {}: {e:#}", to.display());
        0
    })
}

/// The mods jars among sync results that landed in `root`, or were turned
/// on or off there (file names): their records go along.
pub fn copied_jars(results: &[crate::model::SyncResult], root: &Path) -> Vec<String> {
    use crate::model::SyncOp;
    results
        .iter()
        .filter(|r| r.ok)
        .filter_map(|r| match &r.op {
            SyncOp::Copy { dst, .. } | SyncOp::Enable { path: dst } | SyncOp::Disable { path: dst } if dst.starts_with(root) => {
                let parent = dst.parent()?.file_name()?.to_string_lossy().to_lowercase();
                let name = dst.file_name()?.to_string_lossy().into_owned();
                (parent == "mods" && name.to_lowercase().trim_end_matches(".disabled").ends_with(".jar")).then_some(name)
            }
            _ => None,
        })
        .collect()
}

/// The sources among `pins` this PC hasn't trusted yet, each once, for
/// the confirm before an import installs their mods.
pub fn new_sources(pins: &[Pin]) -> Vec<ModSource> {
    let mut out: Vec<ModSource> = Vec::new();
    for p in pins {
        if !super::trusted(&p.source) && !out.iter().any(|s| s.key() == p.source.key()) {
            out.push(p.source.clone());
        }
    }
    out
}

/// The pins to leave out when nobody can be asked (a followed pack that
/// updates by itself, "Keep everyone in step"): all of them while Settings
/// › Mod sources is off, else those from sources the player hasn't
/// confirmed yet. The same answer the ask would need, never a guess.
pub fn auto_skip(pins: &[Pin], allow_sources: bool) -> Vec<Pin> {
    pins.iter()
        .filter(|p| !allow_sources || !super::trusted(&p.source))
        .cloned()
        .collect()
}

/// A pinned mod's name for people: the repo's name for GitHub ("EZActions"),
/// else the jar's name without its version and loader ("modmenu").
pub fn display_name(p: &Pin) -> String {
    if p.source.kind == super::Kind::GitHub
        && let Some(repo) = p.source.publisher.rsplit('/').next().filter(|r| !r.is_empty() && p.source.publisher.contains('/'))
    {
        return repo.to_string();
    }
    let mut n = crate::scan::strip_version_suffix(&p.file);
    for l in ["-neoforge", "-forge", "-fabric", "-quilt"] {
        if n.to_lowercase().ends_with(l) && n.len() > l.len() {
            n.truncate(n.len() - l.len());
            break;
        }
    }
    n
}

/// "These 3 mods won't be installed: A (GitHub: owner/repo), …", for
/// the import's result when the player skipped mods from other sources.
pub fn skipped_note(pins: &[Pin]) -> Option<String> {
    if pins.is_empty() {
        return None;
    }
    let names = pins
        .iter()
        .map(|p| format!("{} ({}: {})", display_name(p), p.source.kind.label(), p.source.publisher))
        .collect::<Vec<_>>()
        .join(", ");
    Some(if pins.len() == 1 {
        format!("This mod from other sources won't be installed: {names}")
    } else {
        format!("These {} mods from other sources won't be installed: {names}", pins.len())
    })
}

/// Leaves the pinned jars out of an imported instance (the player skipped
/// mods from other sources): deletes each jar that is the pinned file
/// (same sha256) and its row. Returns the file names taken out.
pub fn skip(inst: &mut Instance, pins: &[Pin]) -> Vec<String> {
    let mods = inst.mods_dir();
    let mut out = Vec::new();
    for p in pins {
        let file = p.file.rsplit(['/', '\\']).next().unwrap_or(&p.file).trim_end_matches(".disabled").to_string();
        let Some(path) = [mods.join(&file), mods.join(format!("{file}.disabled"))].into_iter().find(|f| f.is_file()) else { continue };
        if !p.source.sha256.is_empty() {
            match super::sha256_file(&path) {
                Ok(h) if h.eq_ignore_ascii_case(&p.source.sha256) => {}
                _ => continue,
            }
        }
        if std::fs::remove_file(&path).is_ok() {
            inst.addons.retain(|a| !a.file_name.eq_ignore_ascii_case(&file));
            out.push(file);
        }
    }
    out
}

#[cfg(test)]
mod auto_skip_tests {
    use super::*;

    fn pin(url: &str) -> Pin {
        Pin {
            file: format!("{url}.jar"),
            source: ModSource {
                url: format!("hopper-test-never-trusted/{url}"),
                publisher: format!("hopper-test-never-trusted/{url}"),
                ..Default::default()
            },
        }
    }

    #[test]
    fn nothing_from_other_sources_installs_by_itself_unless_confirmed() {
        let pins = vec![pin("a"), pin("b")];
        // Mod sources off: every pinned mod waits for the player.
        assert_eq!(auto_skip(&pins, false), pins);
        // On, but these sources were never confirmed: still left out.
        assert_eq!(auto_skip(&pins, true), pins);
        assert!(auto_skip(&[], false).is_empty());
    }
}
