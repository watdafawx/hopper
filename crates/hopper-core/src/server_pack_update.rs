//! Updating a server from its pack's server pack, previewed like a modpack
//! update ([`crate::packdiff`]): which mods the new server pack adds,
//! updates and drops, and a three-way merge of its other files (config,
//! scripts) against the server pack the server came from.
//!
//! The server pack the server came from is recorded in [`MARKER`] when
//! Hopper builds or updates a server from one. Without that record the
//! instance's installed pack version is assumed. That old server pack is
//! never downloaded whole: its file list (with CRCs) comes from the zip's
//! central directory, and only files both sides changed are read from it.
//!
//! World folders and server state (ops, whitelist, bans) are never touched.

use crate::cf;
use crate::hosting::server_pack::{self as sp, Action, ModSource};
use crate::instances::Instance;
use crate::jobs::Job;
use crate::modpack;
use crate::packdiff::{self, FileAction, ModAction, ModChange, UpdatePlan};
use crate::serverpack::{self, ZipEntry};
use crate::server::{self, ServerInfo, Status};
use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// In the server folder: which server pack it was built or last updated
/// from.
pub const MARKER: &str = ".hopper-server-pack.json";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Marker {
    pub project_id: u64,
    /// The (client) pack file this server pack belongs to.
    pub pack_file_id: u64,
    pub server_pack_file_id: u64,
    /// The server pack's file name.
    pub server_pack: String,
}

/// What `root` records about its server pack.
pub fn recorded(root: &Path) -> Option<Marker> {
    serde_json::from_slice(&fs::read(root.join(MARKER)).ok()?).ok()
}

pub fn record(root: &Path, m: &Marker) -> anyhow::Result<()> {
    fs::write(root.join(MARKER), serde_json::to_vec_pretty(m)?)?;
    Ok(())
}

/// Server state a server pack must never overwrite.
const STATE_FILES: [&str; 5] = [
    "ops.json",
    "whitelist.json",
    "banned-players.json",
    "banned-ips.json",
    "usercache.json",
];

/// Whether a server pack file at `rel` is the pack's to update: not the
/// world (`level` is the server's level-name) and not server state.
fn updatable(rel: &str, level: &str) -> bool {
    let low = rel.to_ascii_lowercase();
    let top = low.split('/').next().unwrap_or("");
    if low.contains('/') && (top == level.to_ascii_lowercase() || top.starts_with("world")) {
        return false;
    }
    !STATE_FILES.contains(&low.as_str()) && !low.starts_with(".hopper")
}

/// A jar directly under mods/: its file name.
fn mod_jar(rel: &str) -> Option<&str> {
    let name = rel.strip_prefix("mods/").or_else(|| rel.strip_prefix("MODS/"))?;
    (!name.contains('/') && name.to_ascii_lowercase().ends_with(".jar")).then_some(name)
}

fn key(crc: u32, size: u64) -> String {
    format!("{crc:08x}-{size}")
}

/// [`key`] of a file on disk.
fn file_key(path: &Path) -> Option<String> {
    let b = fs::read(path).ok()?;
    let mut c = flate2::Crc::new();
    c.update(&b);
    Some(key(c.sum(), b.len() as u64))
}

/// One side of the comparison: a server pack's files and jars.
#[derive(Default)]
struct Side {
    /// Non-jar files by path in the server folder → [`key`].
    files: HashMap<String, String>,
    /// Jars under mods/ by file name → size.
    jars: HashMap<String, u64>,
    /// Remote entries by path, for the old server pack.
    remote: HashMap<String, ZipEntry>,
    mods_included: bool,
}

/// Splits a server pack's entries into [`Side`]. `entry(i)` gives the
/// (crc, size) of entry `i`.
fn side(
    names: &[String],
    level: &str,
    mut entry: impl FnMut(usize) -> anyhow::Result<(u32, u64)>,
) -> anyhow::Result<(Side, String)> {
    let plan = sp::plan(names);
    let prefix = sp::wrapper_prefix(names);
    let mut s = Side {
        mods_included: matches!(plan.mods, ModSource::Included(_)),
        ..Default::default()
    };
    for (i, a) in plan.actions.iter().enumerate() {
        let Action::Copy(rel) = a else { continue };
        if !updatable(rel, level) {
            continue;
        }
        let (crc, size) = entry(i)?;
        match mod_jar(rel) {
            Some(j) => {
                s.jars.insert(j.to_string(), size);
            }
            None => {
                s.files.insert(rel.clone(), key(crc, size));
            }
        }
    }
    Ok((s, prefix))
}

/// Everything a server pack update would do, before it does it.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ServerPlan {
    /// The changes, as for a modpack update: `plan.file` is the new server
    /// pack, `plan.kept_mods` the server's mods that aren't the pack's.
    pub plan: UpdatePlan,
    /// The pack version the server pack belongs to.
    pub pack: cf::File,
    /// (on the server, the pack's) NeoForge, when they differ.
    pub loader: Option<(String, String)>,
    /// The server pack compared against, by file name.
    pub base: Option<String>,
    /// `base` is a guess: the server has no record of its server pack.
    pub base_assumed: bool,
    /// Why the server's mods aren't compared, when they aren't.
    pub mods_note: Option<String>,
    /// Server mods that stay as they are.
    pub kept: KeptMods,
}

/// The pack versions a server could be updated to: the newer one first
/// (when `latest` is set), then the instance's. Only versions with a
/// server pack.
pub fn targets(inst: &Instance, latest: Option<cf::File>) -> anyhow::Result<Vec<cf::File>> {
    let Some(p) = inst.pack.as_ref().filter(|p| p.project_id != 0) else {
        bail!("{} wasn't installed from a modpack.", inst.name);
    };
    if crate::modrinth::is_mr(p.project_id) {
        bail!("Modrinth packs don't publish server packs.");
    }
    let installed = cf::file(p.project_id, p.file_id).context("looking up the pack on CurseForge")?;
    let installed_id = installed.id;
    let mut out: Vec<cf::File> = latest
        .into_iter()
        .filter(|f| f.id != installed_id)
        .chain([installed])
        .filter(|f| f.server_pack_file_id.is_some_and(|id| id != 0))
        .collect();
    out.dedup_by_key(|f| f.id);
    if out.is_empty() {
        let name = if p.name.is_empty() { &inst.name } else { &p.name };
        bail!("The author of {name} didn't publish a server pack for this version.");
    }
    Ok(out)
}

/// Downloads `pack`'s server pack and works out what updating `server` to
/// it would do. `inst` is the instance the server runs.
pub fn plan(
    server: &ServerInfo,
    inst: &Instance,
    pack: &cf::File,
    job: &Job,
) -> anyhow::Result<ServerPlan> {
    // A server on a Hopper Server: the panel plans it next to the files.
    if let Some(r) = &server.remote {
        return crate::hopperd::Client::for_remote(r)?.pack_plan(&r.id, inst, pack, job);
    }
    let pid = inst.pack.as_ref().map_or(0, |p| p.project_id);
    let sp_id = pack.server_pack_file_id.filter(|id| *id != 0).ok_or_else(|| {
        anyhow::anyhow!("{} has no server pack.", pack.display_name)
    })?;
    let level = server
        .properties
        .get("level-name")
        .cloned()
        .unwrap_or_else(|| "world".into());

    job.detail("Reading the pack's loader");
    let manifest = modpack::read_manifest(&modpack::fetch_pack_zip(pack, job)?)?;
    let loader = match (
        manifest.loader().strip_prefix("neoforge-").map(str::to_string),
        crate::server_update::server_neoforge(&server.root),
    ) {
        (Some(want), Some(have)) if want != have => Some((have, want)),
        _ => None,
    };

    let file = cf::file(pid, sp_id).context("looking up the server pack on CurseForge")?;
    let new_zip = modpack::fetch_pack_zip(&file, job)?;
    job.check_cancelled()?;
    job.detail("Reading the server pack");
    let names = sp::zip_names(&new_zip)?;
    let mut z = zip::ZipArchive::new(fs::File::open(&new_zip)?)?;
    let (new, prefix) = side(&names, &level, |i| {
        let e = z.by_index_raw(i)?;
        Ok((e.crc32(), e.size()))
    })?;

    // The server pack the server came from.
    let marker = recorded(&server.root).filter(|m| m.project_id == pid && m.server_pack_file_id != 0);
    let base_assumed = marker.is_none();
    let base_id = match &marker {
        Some(m) => m.server_pack_file_id,
        None => inst.pack.as_ref().map_or(0, |p| p.server_pack_file_id),
    };
    let mut base_name = None;
    let mut base_url = None;
    let mut base_prefix = prefix.clone();
    let base: Option<Side> = if base_id == 0 {
        None
    } else if base_id == file.id {
        base_name = Some(file.file_name.clone());
        let mut z = zip::ZipArchive::new(fs::File::open(&new_zip)?)?;
        side(&names, &level, |i| {
            let e = z.by_index_raw(i)?;
            Ok((e.crc32(), e.size()))
        })
        .ok()
        .map(|(s, _)| s)
    } else {
        job.detail("Reading the old server pack's file list");
        (|| -> anyhow::Result<Side> {
            let f = cf::file(pid, base_id)?;
            let url = cf::download_url(&f)?;
            let entries = serverpack::remote_zip_entries(&url, f.file_length)?;
            let names: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();
            let (mut s, p) = side(&names, &level, |i| Ok((entries[i].crc32, entries[i].size)))?;
            let plan = sp::plan(&names);
            for (i, a) in plan.actions.iter().enumerate() {
                if let Action::Copy(rel) = a
                    && s.files.contains_key(rel)
                {
                    s.remote.insert(rel.clone(), entries[i].clone());
                }
            }
            base_name = Some(f.file_name.clone());
            base_url = Some(url);
            base_prefix = p;
            Ok(s)
        })()
        .inspect_err(|e| log::warn!("old server pack: {e:#}"))
        .ok()
    };

    // Config and other files.
    job.detail("Comparing files");
    let (mut files, same) =
        packdiff::plan_files_by(base.as_ref().map(|b| &b.files), &new.files, &server.root, file_key);
    let mut merged = HashMap::new();
    if let Some(b) = &base {
        let conflicts: Vec<_> = files
            .iter_mut()
            .filter(|f| f.action == FileAction::Conflict)
            .collect();
        if !conflicts.is_empty() {
            job.detail("Merging files you and the pack both changed");
        }
        let mut z = zip::ZipArchive::new(fs::File::open(&new_zip)?)?;
        for f in conflicts {
            job.check_cancelled()?;
            let m = (|| {
                let base_bytes = match (&base_url, b.remote.get(&f.rel)) {
                    (Some(url), Some(e)) => serverpack::remote_zip_read(url, e).ok()?,
                    (None, _) => read(&mut z, &packdiff::entry_name(&base_prefix, &f.rel))?,
                    _ => return None,
                };
                let theirs = read(&mut z, &packdiff::entry_name(&prefix, &f.rel))?;
                let yours = fs::read_to_string(server.root.join(&f.rel)).ok()?;
                packdiff::merge3(
                    &String::from_utf8(base_bytes).ok()?,
                    &yours,
                    &String::from_utf8(theirs).ok()?,
                )
            })();
            if let Some(m) = m {
                f.action = FileAction::Merge;
                merged.insert(f.rel.clone(), m.into_bytes());
            }
        }
    }

    // Mods.
    let (mods, kept, mods_note) = if new.mods_included {
        let (mut m, k) = mod_changes(&server.mods_dir(), &new.jars, base.as_ref().map(|b| &b.jars));
        // The instance knows these mods: their projects and icons.
        for c in &mut m {
            let jar = if c.to.is_empty() { &c.from } else { &c.to };
            let stem = serverpack::jar_stem(jar);
            if let Some(a) = inst
                .addons
                .iter()
                .find(|a| a.file_name.eq_ignore_ascii_case(jar) || (!stem.is_empty() && serverpack::jar_stem(&a.file_name) == stem))
            {
                c.project = a.project_id;
                c.icon = a.icon.clone();
                if !a.name.is_empty() {
                    c.name = a.name.clone();
                }
            }
        }
        (m, k, None)
    } else {
        (
            Vec::new(),
            KeptMods::default(),
            Some(format!(
                "{} doesn't include its mods (it downloads them at first start), so only its other files are compared. Server sync brings the mods over from the instance.",
                file.file_name
            )),
        )
    };

    Ok(ServerPlan {
        plan: UpdatePlan {
            file,
            mods,
            files,
            kept_mods: kept.added,
            off: Vec::new(),
            off_old: HashMap::new(),
            removed: Vec::new(),
            updated: Vec::new(),
            base_known: base.is_some(),
            new_zip,
            prefixes: vec![prefix],
            same,
            merged,
        },
        pack: pack.clone(),
        loader,
        base: base_name,
        base_assumed,
        mods_note,
        kept,
    })
}

fn read(z: &mut zip::ZipArchive<fs::File>, name: &str) -> Option<Vec<u8>> {
    let mut e = z.by_name(name).ok()?;
    if e.size() > 16 << 20 {
        return None;
    }
    let mut b = Vec::new();
    e.read_to_end(&mut b).ok()?;
    Some(b)
}

/// Jars in `dir`, by file name → size.
fn jars_in(dir: &Path) -> HashMap<String, u64> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let md = e.metadata().ok()?;
            (md.is_file() && name.to_ascii_lowercase().ends_with(".jar")).then(|| (name, md.len()))
        })
        .collect()
}

/// Server mods that stay as they are, by why.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeptMods {
    /// Not the pack's: you added them.
    pub added: usize,
    /// The pack's mod in another version than the pack ships, where the
    /// pack didn't change it: your version stays.
    pub yours: usize,
    /// The pack's mods the server doesn't have (you left them off), where
    /// the pack didn't add them in this version: not added.
    pub left_off: usize,
}

/// The server's mods against the new server pack's jars, three-way with
/// the old server pack (`base`) like a modpack update: the pack's changes
/// are applied, yours stay. Same name and size is the same file; a jar
/// with the same name stem is another version of the mod.
///
/// - a jar new in the pack is added; one the old pack had too but the
///   server lacks was left off and stays off;
/// - a jar the pack changed replaces the server's version; where the pack
///   didn't change it, the server's other version stays;
/// - a jar the pack dropped is removed when the server has the pack's
///   exact file, kept otherwise.
///
/// Without `base` every difference is taken from the pack, nothing is
/// removed.
fn mod_changes(
    dir: &Path,
    new: &HashMap<String, u64>,
    base: Option<&HashMap<String, u64>>,
) -> (Vec<ModChange>, KeptMods) {
    let have = jars_in(dir);
    let low = |s: &str| s.to_ascii_lowercase();
    let have_low: HashMap<String, &String> = have.keys().map(|k| (low(k), k)).collect();
    let new_low: HashSet<String> = new.keys().map(|k| low(k)).collect();
    let base_low: Option<HashMap<String, u64>> =
        base.map(|b| b.iter().map(|(k, v)| (low(k), *v)).collect());
    let base_stems: HashSet<String> = base
        .map(|b| b.keys().map(|k| serverpack::jar_stem(k)).collect())
        .unwrap_or_default();
    // The pack shipped this exact jar before too: it didn't change it.
    let unchanged = |name: &str, size: u64| {
        base_low.as_ref().is_some_and(|b| b.get(&low(name)) == Some(&size))
    };
    let mut by_stem: HashMap<String, Vec<&String>> = HashMap::new();
    let mut have_sorted: Vec<&String> = have.keys().collect();
    have_sorted.sort();
    for h in have_sorted {
        by_stem.entry(serverpack::jar_stem(h)).or_default().push(h);
    }
    let mut used: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    let mut kept = KeptMods::default();
    let mut new_sorted: Vec<(&String, &u64)> = new.iter().collect();
    new_sorted.sort();
    for (name, &size) in new_sorted {
        let stem = serverpack::jar_stem(name);
        let update = |from: &String| ModChange {
            name: stem.clone(),
            from: from.clone(),
            to: name.clone(),
            action: ModAction::Update,
            ..Default::default()
        };
        if let Some(h) = have_low.get(&low(name)) {
            used.insert(low(h));
            if have[*h] != size {
                if unchanged(name, size) {
                    kept.yours += 1;
                } else {
                    out.push(update(h));
                }
            }
            continue;
        }
        let other = by_stem.get(&stem).and_then(|v| {
            v.iter()
                .find(|h| !used.contains(&low(h)) && !new_low.contains(&low(h)))
                .copied()
        });
        match other {
            Some(h) => {
                used.insert(low(h));
                if unchanged(name, size) {
                    kept.yours += 1;
                } else {
                    out.push(update(h));
                }
            }
            None if base.is_some() && base_stems.contains(&stem) => kept.left_off += 1,
            None => out.push(ModChange {
                name: stem,
                from: String::new(),
                to: name.clone(),
                action: ModAction::Add,
                ..Default::default()
            }),
        }
    }
    let mut gone: Vec<&String> = have
        .keys()
        .filter(|h| !used.contains(&low(h)) && !new_low.contains(&low(h)))
        .collect();
    gone.sort();
    for h in gone {
        if base_low.as_ref().is_some_and(|b| b.get(&low(h)) == Some(&have[h])) {
            out.push(ModChange {
                name: serverpack::jar_stem(h),
                from: h.clone(),
                to: String::new(),
                action: ModAction::Remove,
                ..Default::default()
            });
        } else {
            kept.added += 1;
        }
    }
    (out, kept)
}

/// What [`apply`] did.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Report {
    pub loader: Option<(String, String)>,
    pub mods: [usize; 3],
    pub files: usize,
    pub removed: usize,
    pub panel: Option<String>,
    pub backups: Option<PathBuf>,
}

impl Report {
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some((from, to)) = &self.loader {
            parts.push(format!("NeoForge {from} » {to}"));
        }
        let [a, u, r] = self.mods;
        parts.push(format!("mods: {a} added, {u} updated, {r} removed"));
        parts.push(format!("{} files written, {} removed", self.files, self.removed));
        if let Some(p) = &self.panel {
            parts.push(p.clone());
        }
        parts.join(" · ")
    }
}

/// Updates `server` as `p` says. `take` holds the conflicting files where
/// the pack's copy wins. Refuses while the server answers a ping. Replaced
/// and removed files go to `.hopper-backups/<time>/server-pack-update/`.
pub fn apply(
    server: &ServerInfo,
    p: &ServerPlan,
    take: &HashSet<String>,
    host: &str,
    java: &Path,
    panel: Option<&dyn crate::panel::ServerPanel>,
    job: &Job,
) -> anyhow::Result<Report> {
    // A server on a Hopper Server: the panel updates its own files.
    if let Some(r) = &server.remote {
        return crate::hopperd::Client::for_remote(r)?.pack_apply(&r.id, p, take, job);
    }
    if matches!(server::ping(host, server.port()), Status::Online { .. }) {
        bail!("{} is running. Stop it first.", server.name);
    }
    let root = &server.root;
    let mut report = Report::default();

    if let Some((have, want)) = &p.loader {
        crate::server_update::install_neoforge_server(root, want, java, job)?;
        crate::server_update::rewrite_run_scripts(root, have, want)?;
        report.loader = p.loader.clone();
    }
    if let (Some(want), Some(panel)) = (
        p.loader
            .as_ref()
            .map(|(_, w)| w.clone())
            .or_else(|| crate::server_update::server_neoforge(root)),
        panel,
    ) {
        report.panel = crate::server_update::point_panel(server, &want, panel, job);
    }

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let backups = root
        .join(crate::sync::BACKUP_DIR)
        .join(stamp.to_string())
        .join("server-pack-update");
    let stash = |rel: &str, keep: bool| -> anyhow::Result<()> {
        let src = root.join(rel);
        if !src.is_file() {
            return Ok(());
        }
        let dest = backups.join(rel);
        fs::create_dir_all(dest.parent().unwrap_or(&backups))?;
        if keep {
            fs::copy(&src, &dest)?;
        } else {
            fs::rename(&src, &dest)
                .or_else(|_| fs::copy(&src, &dest).and_then(|_| fs::remove_file(&src)))?;
        }
        Ok(())
    };

    let plan = &p.plan;
    let prefix = plan.prefixes.first().cloned().unwrap_or_default();
    let mut z = zip::ZipArchive::new(fs::File::open(&plan.new_zip)?)?;
    let mut extract = |rel: &str| -> anyhow::Result<()> {
        let mut e = z
            .by_name(&packdiff::entry_name(&prefix, rel))
            .with_context(|| format!("{rel} isn't in the server pack"))?;
        let dest = root.join(rel);
        fs::create_dir_all(dest.parent().unwrap_or(root))?;
        let mut f = fs::File::create(&dest).with_context(|| format!("writing {}", dest.display()))?;
        std::io::copy(&mut e, &mut f).with_context(|| format!("extracting {rel}"))?;
        Ok(())
    };

    // Mods.
    let total = plan.mods.len();
    for (i, m) in plan.mods.iter().enumerate() {
        job.check_cancelled()?;
        if i % 20 == 0 {
            job.detail(format!("Updating mods ({i}/{total})"));
            job.progress(i, total);
        }
        if !m.from.is_empty() && m.action != ModAction::Add {
            stash(&format!("mods/{}", m.from), false)?;
        }
        if !m.to.is_empty() {
            extract(&format!("mods/{}", m.to))?;
        }
        report.mods[match m.action {
            ModAction::Add => 0,
            ModAction::Update => 1,
            ModAction::Remove => 2,
        }] += 1;
    }

    // Files.
    job.detail("Updating files");
    let o = plan.decisions(take);
    for rel in &o.backup {
        stash(rel, true)?;
    }
    for rel in &o.remove {
        stash(rel, false)?;
        report.removed += 1;
    }
    for f in &plan.files {
        job.check_cancelled()?;
        let write = match f.action {
            FileAction::Add | FileAction::Update => true,
            FileAction::Conflict => take.contains(&f.rel),
            _ => false,
        };
        if write {
            extract(&f.rel)?;
            report.files += 1;
        }
    }
    for (rel, bytes) in &o.write {
        let out = root.join(rel);
        fs::create_dir_all(out.parent().unwrap_or(root))?;
        fs::write(out, bytes)?;
        report.files += 1;
    }
    job.progress(total, total);

    record(
        root,
        &Marker {
            project_id: p.pack.mod_id,
            pack_file_id: p.pack.id,
            server_pack_file_id: plan.file.id,
            server_pack: plan.file.file_name.clone(),
        },
    )?;
    if backups.is_dir() {
        report.backups = Some(backups);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worlds_and_state_stay() {
        assert!(!updatable("world/level.dat", "world"));
        assert!(!updatable("MyWorld/region/r.0.0.mca", "myworld"));
        assert!(!updatable("ops.json", "world"));
        assert!(updatable("config/foo.toml", "world"));
        assert!(updatable("kubejs/server_scripts/a.js", "world"));
        assert_eq!(mod_jar("mods/a-1.0.jar"), Some("a-1.0.jar"));
        assert_eq!(mod_jar("mods/sub/a.jar"), None);
        assert_eq!(mod_jar("config/a.jar"), None);
    }

    fn jars(list: &[(&str, u64)]) -> HashMap<String, u64> {
        list.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    fn server_dir(tag: &str, list: &[(&str, u64)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hopper-spu-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (n, size) in list {
            fs::write(dir.join(n), vec![0u8; *size as usize]).unwrap();
        }
        dir
    }

    fn by_action(ch: &[ModChange], a: ModAction) -> Vec<(String, String)> {
        let mut v: Vec<_> = ch
            .iter()
            .filter(|c| c.action == a)
            .map(|c| (c.from.clone(), c.to.clone()))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn three_way_mods() {
        let dir = server_dir(
            "3w",
            &[
                ("same-1.0.jar", 1),
                ("jei-neoforge-1.21.1-19.0.jar", 1), // pack updates it
                ("mine-2.0.jar", 1),                 // you updated it, pack didn't
                ("dropped-1.0.jar", 1),              // pack drops it
                ("tweaked-1.0.jar", 2),              // pack drops it, you changed it
                ("extra-1.0.jar", 1),                // yours alone
            ],
        );
        let base = jars(&[
            ("same-1.0.jar", 1),
            ("jei-neoforge-1.21.1-19.0.jar", 1),
            ("mine-1.0.jar", 1),
            ("dropped-1.0.jar", 1),
            ("tweaked-1.0.jar", 1),
            ("offmod-1.0.jar", 1), // you left it off
        ]);
        let new = jars(&[
            ("same-1.0.jar", 1),
            ("jei-neoforge-1.21.1-19.5.jar", 1),
            ("mine-1.0.jar", 1),
            ("offmod-1.0.jar", 1),
            ("added-2.0.jar", 1),
        ]);
        let (ch, kept) = mod_changes(&dir, &new, Some(&base));
        assert_eq!(by_action(&ch, ModAction::Add), vec![(String::new(), "added-2.0.jar".into())]);
        assert_eq!(
            by_action(&ch, ModAction::Update),
            vec![("jei-neoforge-1.21.1-19.0.jar".into(), "jei-neoforge-1.21.1-19.5.jar".into())]
        );
        assert_eq!(by_action(&ch, ModAction::Remove), vec![("dropped-1.0.jar".into(), String::new())]);
        assert_eq!(kept, KeptMods { added: 2, yours: 1, left_off: 1 });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_a_base_the_pack_wins() {
        let dir = server_dir("nb", &[("a-1.0.jar", 1), ("b-1.0.jar", 1), ("mine.jar", 1)]);
        let new = jars(&[("a-1.0.jar", 3), ("b-2.0.jar", 1), ("c-1.0.jar", 1)]);
        let (ch, kept) = mod_changes(&dir, &new, None);
        assert_eq!(by_action(&ch, ModAction::Add), vec![(String::new(), "c-1.0.jar".into())]);
        assert_eq!(
            by_action(&ch, ModAction::Update),
            vec![("a-1.0.jar".into(), "a-1.0.jar".into()), ("b-1.0.jar".into(), "b-2.0.jar".into())]
        );
        assert!(by_action(&ch, ModAction::Remove).is_empty());
        assert_eq!(kept.added, 1);
        let _ = fs::remove_dir_all(&dir);
    }
}

/// Plans and applies TNP-LL8's server pack to a scratch server that
/// records an older server pack, so the old side comes from CurseForge's
/// file list by range. Nothing of the user's is touched.
/// `cargo test real_server_pack_update -- --ignored --nocapture`
#[cfg(test)]
#[test]
#[ignore]
fn real_server_pack_update() {
    let inst = crate::instances::load(Path::new(r"G:\Games\Minecraft\Curse\Instances\TNP Limitless 8")).unwrap();
    let pack = inst.pack.clone().unwrap();
    let installed = cf::file(pack.project_id, pack.file_id).unwrap();
    let older = cf::files(pack.project_id, "", cf::Loader::Any)
        .unwrap()
        .into_iter()
        .filter(|f| f.is_server_pack != Some(true) && f.server_pack_file_id.is_some_and(|i| i != 0))
        .find(|f| f.id != installed.id && f.file_date < installed.file_date)
        .unwrap();
    println!("old: {}  new: {}", older.display_name, installed.display_name);

    let root = std::env::temp_dir().join("hopper-spu-real");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("mods")).unwrap();
    fs::create_dir_all(root.join("world")).unwrap();
    fs::write(root.join("world/level.dat"), "keep").unwrap();
    fs::write(root.join("ops.json"), "[]").unwrap();
    fs::write(root.join("server.properties"), "server-port=1\nlevel-name=world\n").unwrap();
    let nf = inst.loader.strip_prefix("neoforge-").unwrap();
    fs::write(
        root.join("run.bat"),
        format!("java @user_jvm_args.txt @libraries/net/neoforged/neoforge/{nf}/win_args.txt nogui %*\r\n"),
    )
    .unwrap();
    fs::write(root.join("mods/my-own-mod-1.0.jar"), "mine").unwrap();
    record(
        &root,
        &Marker {
            project_id: pack.project_id,
            pack_file_id: older.id,
            server_pack_file_id: older.server_pack_file_id.unwrap(),
            server_pack: older.display_name.clone(),
        },
    )
    .unwrap();
    let server = crate::server::discover(&std::env::temp_dir())
        .into_iter()
        .find(|s| s.root == root)
        .unwrap();

    // Seed the server as the old server pack left it: its jars (by size),
    // and three config files the pack changed: one as shipped, one with
    // your line on top, one rewritten.
    let old_sp = cf::file(pack.project_id, older.server_pack_file_id.unwrap()).unwrap();
    let url = cf::download_url(&old_sp).unwrap();
    let entries = serverpack::remote_zip_entries(&url, old_sp.file_length).unwrap();
    let names: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();
    let old_plan = sp::plan(&names);
    let mut old_files = HashMap::new();
    for (i, a) in old_plan.actions.iter().enumerate() {
        let Action::Copy(rel) = a else { continue };
        match mod_jar(rel) {
            Some(j) => fs::File::create(root.join("mods").join(j))
                .unwrap()
                .set_len(entries[i].size)
                .unwrap(),
            None => {
                old_files.insert(rel.clone(), entries[i].clone());
            }
        }
    }
    let new_sp = cf::file(pack.project_id, installed.server_pack_file_id.unwrap()).unwrap();
    let new_zip = modpack::fetch_pack_zip(&new_sp, &crate::jobs::Jobs::default().start("dl")).unwrap();
    let new_names = sp::zip_names(&new_zip).unwrap();
    let new_prefix = sp::wrapper_prefix(&new_names);
    let mut z = zip::ZipArchive::new(fs::File::open(&new_zip).unwrap()).unwrap();
    let mut changed: Vec<String> = old_files
        .iter()
        .filter(|(rel, e)| {
            rel.ends_with(".toml")
                && z.by_name(&packdiff::entry_name(&new_prefix, rel))
                    .is_ok_and(|n| n.crc32() != e.crc32 && n.size() < 200_000)
        })
        .map(|(rel, _)| rel.clone())
        .collect();
    changed.sort();
    println!("{} .toml files the pack changed", changed.len());
    assert!(changed.len() >= 3, "need 3 changed config files");
    for (k, rel) in changed.iter().take(3).enumerate() {
        let base = serverpack::remote_zip_read(&url, &old_files[rel]).unwrap();
        let dest = root.join(rel);
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        let body = match k {
            0 => base,
            1 => [b"# mine\n".as_slice(), &base].concat(),
            _ => b"totally = \"different\"\n".to_vec(),
        };
        fs::write(dest, body).unwrap();
    }

    let jobs = crate::jobs::Jobs::default();
    let job = jobs.start("test");
    let p = plan(&server, &inst, &installed, &job).unwrap();
    let action = |rel: &str| p.plan.files.iter().find(|f| f.rel == rel).map(|f| f.action);
    println!("seeded: {:?}", changed.iter().take(3).map(|r| (r, action(r))).collect::<Vec<_>>());
    assert_eq!(action(&changed[0]), Some(FileAction::Update));
    assert!(matches!(action(&changed[1]), Some(FileAction::Merge | FileAction::Conflict)));
    assert_eq!(action(&changed[2]), Some(FileAction::Conflict));
    println!(
        "base {:?} (assumed {}), loader {:?}",
        p.base, p.base_assumed, p.loader
    );
    println!(
        "mods +{} ~{} -{}  kept {:?}",
        p.plan.mod_count(ModAction::Add),
        p.plan.mod_count(ModAction::Update),
        p.plan.mod_count(ModAction::Remove),
        p.kept
    );
    for a in [FileAction::Add, FileAction::Update, FileAction::Conflict, FileAction::Merge, FileAction::Remove] {
        println!("files {a:?}: {}", p.plan.count(a));
    }
    assert!(p.plan.base_known && !p.base_assumed);
    assert!(p.loader.is_none());
    assert_eq!(p.kept.added, 1, "my-own-mod stays");

    let r = apply(&server, &p, &HashSet::new(), "127.0.0.1", Path::new("java"), None, &job).unwrap();
    println!("{}", r.summary());
    assert_eq!(fs::read_to_string(root.join("world/level.dat")).unwrap(), "keep");
    assert_eq!(fs::read_to_string(root.join("ops.json")).unwrap(), "[]");
    assert!(root.join("mods/my-own-mod-1.0.jar").is_file());
    for m in &p.plan.mods {
        if !m.to.is_empty() {
            assert!(root.join("mods").join(&m.to).is_file(), "{} installed", m.to);
        }
        if m.action == ModAction::Remove || (m.action == ModAction::Update && m.from != m.to) {
            assert!(!root.join("mods").join(&m.from).exists(), "{} gone", m.from);
        }
    }
    assert!(r.backups.as_ref().is_some_and(|b| b.join("mods").is_dir()));
    if let Some(m) = p.plan.merged.get(&changed[1]) {
        assert_eq!(&fs::read(root.join(&changed[1])).unwrap(), m);
        assert!(m.starts_with(b"# mine"));
    }
    assert_eq!(fs::read(root.join(&changed[2])).unwrap(), b"totally = \"different\"\n", "conflict keeps yours");
    let written = p
        .plan
        .files
        .iter()
        .find(|f| f.action == FileAction::Add)
        .map(|f| root.join(&f.rel))
        .unwrap();
    assert!(written.is_file(), "{} written", written.display());
    assert_eq!(recorded(&root).unwrap().server_pack_file_id, p.plan.file.id);
    let _ = fs::remove_dir_all(&root);
}
