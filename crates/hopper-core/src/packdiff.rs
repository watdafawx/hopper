//! Previewing and merging a modpack update.
//!
//! Mods: which pack mods are added, updated and removed (mods you added
//! stay). Override files (configs and the like) get a three-way merge of the
//! old pack version, the new one and the instance:
//!
//! - the pack didn't change a file: left alone, edited or not;
//! - the pack changed it and you didn't: updated;
//! - both changed it: a conflict, yours is kept unless you take the pack's
//!   (which is backed up first);
//! - the pack dropped a file you never changed: moved to backups.

use crate::cf;
use crate::instances::Instance;
use crate::jobs::Job;
use crate::modpack::{self, Overrides};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Folders that hold the pack's content rather than your preferences
/// (quests, scripts, data): on a conflict the pack's copy is the default.
pub const PACK_CONTENT: [&str; 9] = [
    "config/ftbquests/quests/",
    "config/heracles/",
    "kubejs/",
    "defaultconfigs/",
    "config/paxi/",
    "config/openloader/",
    "global_packs/",
    "scripts/",
    "config/fancymenu/",
];

/// The pack's copy is the default for this conflicting file.
pub fn pack_content(rel: &str) -> bool {
    PACK_CONTENT.iter().any(|p| rel.starts_with(p))
}

/// Your own settings a pack sometimes ships: yours stays unless you pick
/// the pack's, even in a quick update.
pub const KEEP_LOCAL: [&str; 5] = [
    "options.txt",
    "optionsof.txt",
    "optionsshaders.txt",
    "servers.dat",
    "server.properties",
];

/// Yours is the default for this file in a quick update.
pub fn keep_local(rel: &str) -> bool {
    KEEP_LOCAL.iter().any(|k| rel.eq_ignore_ascii_case(k))
}

/// Folders never listed as your files: worlds, logs, backups and the like.
const NOT_LISTED: [&str; 12] = [
    "mods",
    "saves",
    "logs",
    "crash-reports",
    "screenshots",
    "backups",
    "local",
    ".hopper-backups",
    ".hopper",
    "world",
    "journeymap",
    "xaero",
];

/// A mod the pack drops that one of your mods needs.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Needed {
    /// The dropped mod's file (in mods/) and name.
    pub file: String,
    pub name: String,
    pub project: u64,
    pub icon: String,
    /// Your mod that needs it.
    pub by: String,
    pub by_project: u64,
    pub by_icon: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum FileAction {
    /// New in the pack; you don't have it.
    Add,
    /// The pack changed it and you didn't: replaced.
    Update,
    /// You changed it and the pack didn't: kept.
    KeepYours,
    /// Both changed it, in different places: merged line by line.
    Merge,
    /// Both changed the same lines: yours is kept unless you take the
    /// pack's.
    Conflict,
    /// The pack dropped it and you never changed it: moved to backups.
    Remove,
    /// The pack dropped it but you changed it: kept.
    KeepDropped,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FileChange {
    /// Relative to the instance, with `/`.
    pub rel: String,
    pub action: FileAction,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ModAction {
    #[default]
    Add,
    Update,
    Remove,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ModChange {
    pub name: String,
    /// The file you have (empty for an addition).
    pub from: String,
    /// The pack's file (empty for a removal).
    pub to: String,
    pub action: ModAction,
    /// Its project (0 when unknown), for its page.
    pub project: u64,
    /// Its icon's URL, when known.
    pub icon: String,
}

/// A mod you turned off that the update touches: it stays off unless you
/// choose to turn it on.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OffMod {
    pub name: String,
    /// Your (turned off) file.
    pub from: String,
    /// The pack's new file.
    pub to: String,
    /// "p:<project>" for a mod the pack lists, "f:<path>" for a jar the
    /// pack ships in its files.
    pub key: String,
    /// Its project (0 when unknown), for its page.
    pub project: u64,
    /// Its icon's URL, when known.
    pub icon: String,
    /// For a mod you updated: the pack's file is newer than yours.
    pub pack_newer: bool,
}

/// Everything an update would do, before it does it.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct UpdatePlan {
    pub file: cf::File,
    pub mods: Vec<ModChange>,
    pub files: Vec<FileChange>,
    /// Mods you added on top of the pack; they stay.
    pub kept_mods: usize,
    /// Mods you turned off that the update changes.
    pub off: Vec<OffMod>,
    /// Pack mods you removed that the update would put back (keys "r:",
    /// "rp:", "rf:"): they stay out unless you add them back.
    pub removed: Vec<OffMod>,
    /// Pack mods you updated yourself that the pack has another file for
    /// (key "u:<project>"): yours stays unless you take the pack's.
    pub updated: Vec<OffMod>,
    /// For a pack-shipped jar in `off`: your turned-off file it replaces.
    pub(crate) off_old: HashMap<String, String>,
    /// The installed pack version could be read, so your own changes can
    /// be told apart from the pack's. Without it every differing file is a
    /// conflict.
    pub base_known: bool,
    /// Compared with your changes (the installed version was fetched).
    /// Without it (a quick update) every pack file that differs takes the
    /// pack's copy by default, like the CurseForge app.
    #[serde(default)]
    pub compared: bool,
    /// Mods the pack drops that one of your mods needs.
    #[serde(default)]
    pub needed: Vec<Needed>,
    /// Mods you added, which stay (`from` is the file).
    #[serde(default)]
    pub your_mods: Vec<ModChange>,
    /// Files in folders the pack writes to that the pack doesn't ship:
    /// they stay.
    #[serde(default)]
    pub your_files: Vec<String>,
    /// Settings files the pack's mods wrote (in config/ and the like, not
    /// shipped by the pack): they stay too, but aren't listed.
    #[serde(default)]
    pub mod_made: usize,
    pub(crate) new_zip: PathBuf,
    /// Override folders in the zip, later ones on top.
    pub(crate) prefixes: Vec<String>,
    /// Files already identical to the pack's.
    pub(crate) same: Vec<String>,
    /// Merged contents of the `Merge` files.
    pub(crate) merged: HashMap<String, Vec<u8>>,
}

impl UpdatePlan {
    pub fn count(&self, action: FileAction) -> usize {
        self.files.iter().filter(|f| f.action == action).count()
    }

    pub fn mod_count(&self, action: ModAction) -> usize {
        self.mods.iter().filter(|m| m.action == action).count()
    }

    pub fn conflicts(&self) -> impl Iterator<Item = &FileChange> {
        self.files.iter().filter(|f| f.action == FileAction::Conflict)
    }

    /// Conflicts that default to the pack's copy: without a base (a quick
    /// update or a reinstall) all but your own settings ([`keep_local`]),
    /// like the CurseForge app; compared, the pack content folders.
    pub fn default_take(&self) -> HashSet<String> {
        self.conflicts()
            .filter(|f| if self.base_known { pack_content(&f.rel) } else { !keep_local(&f.rel) })
            .map(|f| f.rel.clone())
            .collect()
    }

    /// [`decisions`](Self::decisions), with the turned-off and removed mods
    /// in `turn_on` (their [`OffMod::key`]) turned on or added back; the
    /// rest stay off or out.
    pub fn decisions_with(&self, take_theirs: &HashSet<String>, turn_on: &HashSet<String>) -> Overrides {
        let mut o = self.decisions(take_theirs);
        for m in self.updated.iter().filter(|m| !turn_on.contains(&m.key)) {
            o.keep_mine.insert(m.project);
        }
        for m in self.removed.iter().filter(|m| !turn_on.contains(&m.key)) {
            match m.key.strip_prefix("rf:") {
                Some(rel) => {
                    o.skip.insert(rel.to_string());
                }
                None => {
                    o.leave_out.insert(m.key.clone());
                }
            }
        }
        for m in &self.off {
            let on = turn_on.contains(&m.key);
            if let Some(rel) = m.key.strip_prefix("f:") {
                let old = self.off_old.get(rel).cloned();
                let disabled_name = format!("{rel}.disabled");
                match (on, old) {
                    // On: the new file goes in as usual; your old copy to backups.
                    (true, Some(old)) => o.remove.push(old),
                    (true, None) => {}
                    // Off: the new file arrives as .disabled; a different
                    // old one goes to backups, the same name is backed up
                    // before it's replaced.
                    (false, old) => {
                        o.disable.insert(rel.to_string());
                        match old {
                            Some(old) if old == disabled_name => o.backup.push(old),
                            Some(old) => o.remove.push(old),
                            None => {}
                        }
                    }
                }
            } else if on && let Some(p) = m.key.strip_prefix("p:").and_then(|p| p.parse().ok()) {
                o.enable.insert(p);
            }
        }
        o
    }

    /// The choices made before you pick: turned-off mods stay off, removed
    /// ones out, and of the mods you updated, those where the pack's file
    /// is newer take the pack's.
    pub fn default_choices(&self) -> HashSet<String> {
        self.updated.iter().filter(|m| m.pack_newer).map(|m| m.key.clone()).collect()
    }

    /// What to write, given the conflicts where you take the pack's file.
    pub fn decisions(&self, take_theirs: &HashSet<String>) -> Overrides {
        let mut o = Overrides::default();
        o.skip.extend(self.same.iter().cloned());
        for f in &self.files {
            match f.action {
                FileAction::KeepYours | FileAction::KeepDropped => {
                    o.skip.insert(f.rel.clone());
                }
                FileAction::Conflict if take_theirs.contains(&f.rel) => {
                    o.backup.push(f.rel.clone())
                }
                FileAction::Conflict => {
                    o.skip.insert(f.rel.clone());
                }
                FileAction::Remove => o.remove.push(f.rel.clone()),
                FileAction::Merge => {
                    if let Some(b) = self.merged.get(&f.rel) {
                        o.skip.insert(f.rel.clone());
                        o.backup.push(f.rel.clone());
                        o.write.push((f.rel.clone(), b.clone()));
                    }
                }
                FileAction::Add | FileAction::Update => {}
            }
        }
        o
    }

    /// The pack's copy of an override file.
    pub fn pack_file(&self, rel: &str) -> anyhow::Result<Vec<u8>> {
        let mut z = zip::ZipArchive::new(fs::File::open(&self.new_zip)?)?;
        for p in self.prefixes.iter().rev() {
            let name = entry_name(p, rel);
            if let Ok(mut e) = z.by_name(&name) {
                let mut b = Vec::new();
                e.read_to_end(&mut b)?;
                return Ok(b);
            }
        }
        anyhow::bail!("{rel} is not in the pack")
    }

    /// A unified diff from your file to the pack's (`None` when either is
    /// not text).
    pub fn diff(&self, root: &Path, rel: &str) -> anyhow::Result<Option<String>> {
        let theirs = self.pack_file(rel)?;
        let yours = fs::read(root.join(rel)).unwrap_or_default();
        let (Ok(a), Ok(b)) = (String::from_utf8(yours), String::from_utf8(theirs)) else {
            return Ok(None);
        };
        Ok(Some(
            similar::TextDiff::from_lines(&a, &b)
                .unified_diff()
                .context_radius(3)
                .header("yours", "pack")
                .to_string(),
        ))
    }
}

/// A three-way merge of text by lines: your changes and the pack's, both
/// measured from the installed version, applied together. `None` when they
/// touch the same lines (or not text).
pub fn merge3(base: &str, yours: &str, theirs: &str) -> Option<String> {
    let b: Vec<&str> = base.split_inclusive('\n').collect();
    let y: Vec<&str> = yours.split_inclusive('\n').collect();
    let t: Vec<&str> = theirs.split_inclusive('\n').collect();
    // Each change: the base lines it replaces, and what with.
    let changes = |other: &[&str]| -> Vec<(usize, usize, Vec<String>)> {
        similar::capture_diff_slices(similar::Algorithm::Myers, &b, other)
            .into_iter()
            .filter(|op| op.tag() != similar::DiffTag::Equal)
            .map(|op| {
                let (o, n) = (op.old_range(), op.new_range());
                (o.start, o.end, other[n].iter().map(|s| s.to_string()).collect())
            })
            .collect()
    };
    let mut all: Vec<(usize, usize, Vec<String>)> = changes(&y);
    for c in changes(&t) {
        // Touching or overlapping changes conflict, unless they agree.
        if let Some(same) = all.iter().find(|a| a.0 <= c.1 && c.0 <= a.1) {
            if *same == c {
                continue;
            }
            return None;
        }
        all.push(c);
    }
    all.sort_by_key(|c| (c.0, c.1));
    let mut out = String::with_capacity(theirs.len().max(yours.len()));
    let mut at = 0;
    for (start, end, lines) in all {
        for l in &b[at..start] {
            out.push_str(l);
        }
        for l in &lines {
            out.push_str(l);
        }
        at = end;
    }
    for l in &b[at..] {
        out.push_str(l);
    }
    Some(out)
}

fn read_entry(zip: &Path, prefixes: &[String], rel: &str) -> Option<Vec<u8>> {
    let mut z = zip::ZipArchive::new(fs::File::open(zip).ok()?).ok()?;
    for p in prefixes.iter().rev() {
        let name = entry_name(p, rel);
        if let Ok(mut e) = z.by_name(&name) {
            let mut b = Vec::new();
            e.read_to_end(&mut b).ok()?;
            return Some(b);
        }
    }
    None
}

/// `rel` under override folder `prefix` ("" for the zip's root).
pub(crate) fn entry_name(prefix: &str, rel: &str) -> String {
    match prefix.trim_end_matches('/') {
        "" => rel.to_string(),
        p => format!("{p}/{rel}"),
    }
}

fn sha1_of(bytes: &[u8]) -> String {
    sha1_smol::Sha1::from(bytes).digest().to_string()
}

/// Hashes of the override files in a pack zip, by path relative to the
/// instance. Later prefixes win, like they do when the pack is applied.
fn zip_overrides(zip: &Path, prefixes: &[String]) -> anyhow::Result<HashMap<String, String>> {
    let mut z = zip::ZipArchive::new(fs::File::open(zip)?)?;
    let mut by_prefix: Vec<HashMap<String, String>> = vec![HashMap::new(); prefixes.len()];
    for i in 0..z.len() {
        let mut e = z.by_index(i)?;
        if e.is_dir() {
            continue;
        }
        let Some(name) = e.enclosed_name() else { continue };
        let name = name.to_string_lossy().replace('\\', "/");
        for (pi, p) in prefixes.iter().enumerate() {
            let p = format!("{}/", p.trim_end_matches('/'));
            if let Some(rel) = name.strip_prefix(&p).filter(|r| !r.is_empty()) {
                let mut b = Vec::new();
                e.read_to_end(&mut b)?;
                by_prefix[pi].insert(rel.to_string(), sha1_of(&b));
                break;
            }
        }
    }
    let mut out = HashMap::new();
    for m in by_prefix {
        out.extend(m);
    }
    Ok(out)
}

fn file_hash(path: &Path) -> Option<String> {
    fs::read(path).ok().map(|b| sha1_of(&b))
}

/// The three-way decision for every override file. `old` is the installed
/// pack version's overrides, when known. Returns the changes and the files
/// already identical to the pack's.
pub fn plan_files(
    old: Option<&HashMap<String, String>>,
    new: &HashMap<String, String>,
    root: &Path,
) -> (Vec<FileChange>, Vec<String>) {
    plan_files_by(old, new, root, file_hash)
}

/// [`plan_files`] with hashes of the instance's files from `file_hash`,
/// in the same form as `old` and `new`.
pub fn plan_files_by(
    old: Option<&HashMap<String, String>>,
    new: &HashMap<String, String>,
    root: &Path,
    file_hash: impl Fn(&Path) -> Option<String>,
) -> (Vec<FileChange>, Vec<String>) {
    let mut changes = Vec::new();
    let mut same = Vec::new();
    for (rel, nh) in new {
        let cur = file_hash(&root.join(rel));
        let action = match (&cur, old) {
            (Some(c), _) if c == nh => {
                same.push(rel.clone());
                continue;
            }
            (None, _) => FileAction::Add,
            (Some(c), Some(old)) => match old.get(rel) {
                Some(oh) if c == oh => FileAction::Update,
                Some(oh) if nh == oh => FileAction::KeepYours,
                _ => FileAction::Conflict,
            },
            (Some(_), None) => FileAction::Conflict,
        };
        changes.push(FileChange {
            rel: rel.clone(),
            action,
        });
    }
    if let Some(old) = old {
        for (rel, oh) in old {
            if new.contains_key(rel) {
                continue;
            }
            let action = match file_hash(&root.join(rel)) {
                Some(c) if &c == oh => FileAction::Remove,
                Some(_) => FileAction::KeepDropped,
                None => continue,
            };
            changes.push(FileChange {
                rel: rel.clone(),
                action,
            });
        }
    }
    changes.sort_by(|a, b| a.rel.cmp(&b.rel));
    same.sort();
    (changes, same)
}

/// Whether "updating" to `file` is reinstalling the version the instance has.
pub fn is_reinstall(pack: &crate::instances::PackRef, file: &cf::File) -> bool {
    pack.file_id != 0 && pack.file_id == file.id
}

/// Downloads the new pack version and works out what updating to `file`
/// would do. With `compare` it downloads the installed one too, to tell
/// your edits from the pack's; without (a quick update) the pack's files
/// simply replace yours, like the CurseForge app.
pub fn plan(inst: &Instance, file: &cf::File, compare: bool, job: &Job) -> anyhow::Result<UpdatePlan> {
    let pack = inst
        .pack
        .clone()
        .ok_or_else(|| anyhow::anyhow!("{} isn't from a modpack", inst.name))?;
    let mr = crate::modrinth::is_mr(pack.project_id);
    let fetch = |f: &cf::File| {
        if mr {
            modpack::fetch_mrpack(f, job)
        } else {
            modpack::fetch_pack_zip(f, job)
        }
    };
    job.detail(format!("Downloading {}", file.display_name));
    let new_zip = fetch(file)?;
    // The installed version, to tell your edits from the pack's. Not for a
    // reinstall of that same version: with it as the base every file you
    // changed would count as yours and stay, and a reinstall is for getting
    // the pack's back. Without a base, each one is a conflict to settle.
    let old_zip = if !compare || is_reinstall(&pack, file) {
        None
    } else {
        job.detail("Downloading the installed version to compare");
        crate::source::file(pack.project_id, pack.file_id)
            .and_then(|f| fetch(&f))
            .inspect_err(|e| log::warn!("installed pack version: {e:#}"))
            .ok()
    };
    // A finished download's 100% isn't the comparing's.
    job.progress(0, 0);

    let mut updated = Vec::new();
    let (prefixes, mut mods, mut removed) = if mr {
        let index = modpack::read_mr_index(&new_zip)?;
        let old_index = old_zip.as_ref().and_then(|z| modpack::read_mr_index(z).ok());
        let mods = mr_mod_changes(inst, &index);
        let removed = old_index.map(|o| mr_removed(&index, &o, &mods)).unwrap_or_default();
        (modpack::MR_OVERRIDES.iter().map(|s| s.to_string()).collect(), mods, removed)
    } else {
        let manifest = modpack::read_manifest(&new_zip)?;
        let prefix = if manifest.overrides.is_empty() {
            "overrides".to_string()
        } else {
            manifest.overrides.clone()
        };
        job.detail("Comparing mods");
        let had: Option<HashMap<u64, u64>> = old_zip
            .as_ref()
            .and_then(|z| modpack::read_manifest(z).ok())
            .map(|m| m.files.iter().map(|f| (f.project_id, f.file_id)).collect());
        let (mods, removed, mine) = cf_mod_changes(inst, &manifest, had.as_ref())?;
        updated = mine;
        (vec![prefix], mods, removed)
    };
    // What you removed isn't "added by the pack": it has its own list.
    let removed_names: HashSet<String> = removed.iter().map(|m| m.to.clone()).collect();
    mods.retain(|m| !(m.action == ModAction::Add && removed_names.contains(&m.to)));
    job.detail("Comparing config files");
    let new = zip_overrides(&new_zip, &prefixes)?;
    let old_prefixes: Option<Vec<String>> = old_zip.as_ref().map(|z| {
        if mr {
            prefixes.clone()
        } else {
            modpack::read_manifest(z)
                .ok()
                .map(|m| {
                    vec![if m.overrides.is_empty() {
                        "overrides".to_string()
                    } else {
                        m.overrides
                    }]
                })
                .unwrap_or_else(|| prefixes.clone())
        }
    });
    let old = old_zip.as_ref().and_then(|z| {
        let p = if mr {
            prefixes.clone()
        } else {
            modpack::read_manifest(z)
                .ok()
                .map(|m| {
                    vec![if m.overrides.is_empty() {
                        "overrides".to_string()
                    } else {
                        m.overrides
                    }]
                })
                .unwrap_or_else(|| prefixes.clone())
        };
        zip_overrides(z, &p).ok()
    });
    let (mut files, same) = plan_files(old.as_ref(), &new, &inst.root);
    let mut merged = HashMap::new();
    if let Some(old_zip) = &old_zip {
        job.detail("Merging config files you and the pack both changed");
        let old_prefixes = old_prefixes.clone().unwrap_or_else(|| prefixes.clone());
        for f in files.iter_mut().filter(|f| f.action == FileAction::Conflict) {
            let m = (|| {
                let base = String::from_utf8(read_entry(old_zip, &old_prefixes, &f.rel)?).ok()?;
                let theirs = String::from_utf8(read_entry(&new_zip, &prefixes, &f.rel)?).ok()?;
                let yours = fs::read_to_string(inst.root.join(&f.rel)).ok()?;
                merge3(&base, &yours, &theirs)
            })();
            if let Some(m) = m {
                f.action = FileAction::Merge;
                merged.insert(f.rel.clone(), m.into_bytes());
            }
        }
    }
    let (off, off_old) = off_mods(inst, &mods, &mut files);
    removed.extend(removed_files(inst, &mut files, old.as_ref()));
    removed.sort_by_key(|m| m.name.to_lowercase());
    job.detail("Checking what your mods need");
    let yours: Vec<&crate::instances::Addon> = inst.addons.iter().filter(|a| !a.from_pack).collect();
    let dropped: Vec<(String, String, u64, String)> = mods
        .iter()
        .filter(|m| m.action == ModAction::Remove)
        .map(|m| (m.from.clone(), m.name.clone(), m.project, m.icon.clone()))
        .collect();
    let needed = needed_by_yours(&inst.root.join("mods"), &dropped, |jar| {
        yours
            .iter()
            .find(|a| a.file_name.eq_ignore_ascii_case(jar))
            .map(|a| (a.name.clone(), a.project_id, a.icon.clone()))
            // A jar Hopper has no record of is yours too.
            .or_else(|| (!inst.addons.iter().any(|a| a.file_name.eq_ignore_ascii_case(jar))).then(|| (String::new(), 0, String::new())))
    });
    let your_mods = yours
        .iter()
        .map(|a| ModChange {
            name: if a.name.is_empty() { a.file_name.clone() } else { a.name.clone() },
            from: a.file_name.clone(),
            project: a.project_id,
            icon: a.icon.clone(),
            ..Default::default()
        })
        .collect();
    let mut skip_rels: HashSet<String> = new.keys().cloned().collect();
    if let Some(o) = &old {
        skip_rels.extend(o.keys().cloned());
    }
    skip_rels.extend(inst.addons.iter().map(|a| format!("{}/{}", a.folder(), a.file_name)));
    let your_ids = mod_ids(&inst.root.join("mods"), yours.iter().filter(|a| a.folder() == "mods").map(|a| a.file_name.as_str()));
    let (your_files, mod_made) = split_made(your_files(&inst.root, new.keys().map(String::as_str), &skip_rels), &your_ids);
    Ok(UpdatePlan {
        file: file.clone(),
        mods,
        files,
        off,
        off_old,
        removed,
        updated,
        kept_mods: inst.addons.iter().filter(|a| !a.from_pack).count(),
        base_known: old.is_some(),
        compared: compare,
        needed,
        your_mods,
        your_files,
        mod_made,
        new_zip,
        prefixes,
        same,
        merged,
    })
}

/// Mods in `dropped` (file, name, project, icon) that one of your mods
/// needs: with them gone, a mod `yours` claims (giving its name, project
/// and icon) would miss a required mod it doesn't miss now.
pub fn needed_by_yours(
    mods_dir: &Path,
    dropped: &[(String, String, u64, String)],
    yours: impl Fn(&str) -> Option<(String, u64, String)>,
) -> Vec<Needed> {
    let present: Vec<&(String, String, u64, String)> =
        dropped.iter().filter(|d| mods_dir.join(&d.0).is_file()).collect();
    if present.is_empty() {
        return Vec::new();
    }
    let before: HashSet<(String, String)> = crate::deps::missing(mods_dir)
        .into_iter()
        .map(|m| (m.file_name, m.needs))
        .collect();
    let without: Vec<String> = present.iter().map(|d| d.0.clone()).collect();
    let ids: Vec<Vec<String>> = present
        .iter()
        .map(|d| {
            let p = mods_dir.join(&d.0);
            let mut v = crate::deps::own_ids(&p).0;
            v.extend(crate::deps::provides(&p));
            v
        })
        .collect();
    let mut out: Vec<Needed> = Vec::new();
    for m in crate::deps::missing_without(mods_dir, &without) {
        if before.contains(&(m.file_name.clone(), m.needs.clone())) {
            continue;
        }
        let Some((by, by_project, by_icon)) = yours(&m.file_name) else { continue };
        let Some(i) = ids.iter().position(|v| v.iter().any(|id| *id == m.needs)) else { continue };
        let d = present[i];
        let by = if by.is_empty() { m.mod_name.clone() } else { by };
        if out.iter().any(|n| n.file == d.0 && n.by == by) {
            continue;
        }
        out.push(Needed {
            file: d.0.clone(),
            name: d.1.clone(),
            project: d.2,
            icon: d.3.clone(),
            by,
            by_project,
            by_icon,
        });
    }
    out
}

/// Files under `root` in the top folders the pack writes to (from
/// `pack_rels`) that aren't in `skip`: your own, which an update leaves.
/// Worlds, logs, backups and mods/ aren't listed.
pub fn your_files<'a>(root: &Path, pack_rels: impl Iterator<Item = &'a str>, skip: &HashSet<String>) -> Vec<String> {
    let tops: HashSet<String> = pack_rels
        .filter_map(|r| r.split_once('/').map(|(t, _)| t.to_string()))
        .filter(|t| {
            let l = t.to_ascii_lowercase();
            !NOT_LISTED.contains(&l.as_str()) && !l.starts_with("world")
        })
        .collect();
    let skip_low: HashSet<String> = skip.iter().map(|s| s.to_ascii_lowercase()).collect();
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = tops.iter().map(|t| root.join(t)).collect();
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(p);
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            let Ok(rel) = p.strip_prefix(root) else { continue };
            let rel = rel.to_string_lossy().replace('\\', "/");
            let low = rel.to_ascii_lowercase();
            let base = low.strip_suffix(".disabled").unwrap_or(&low);
            if skip_low.contains(&low) || skip_low.contains(base) {
                continue;
            }
            out.push(rel);
            if out.len() >= 20_000 {
                stack.clear();
                break;
            }
        }
    }
    out.sort();
    out
}

/// Folders where mods write their own settings files.
const SETTINGS_DIRS: [&str; 2] = ["config", "defaultconfigs"];

/// The mod ids of the jars `files` in `mods_dir` (lower case, with their
/// display names), for telling their settings files apart.
pub fn mod_ids<'a>(mods_dir: &Path, files: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut out = Vec::new();
    for f in files {
        let (ids, name) = crate::deps::own_ids(&mods_dir.join(f));
        out.extend(ids);
        if !name.is_empty() {
            out.push(name);
        }
    }
    out.iter()
        .map(|s| norm_id(s))
        .filter(|s| s.len() >= 3)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect()
}

/// Lower case, letters and digits only ("Ars_Conflux" and "ars-conflux"
/// match).
fn norm_id(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

/// Splits [`your_files`] into the ones that are yours (scripts, packs, and
/// settings of your own mods, by `your_ids` from [`mod_ids`]) and a count
/// of settings files the pack's mods wrote.
pub fn split_made(files: Vec<String>, your_ids: &[String]) -> (Vec<String>, usize) {
    let mut made = 0;
    let yours = files
        .into_iter()
        .filter(|rel| {
            let Some((top, rest)) = rel.split_once('/') else { return true };
            if !SETTINGS_DIRS.iter().any(|d| top.eq_ignore_ascii_case(d)) {
                return true;
            }
            // Its first folder, or its file name: "goety/x.toml",
            // "ars_conflux-common.toml".
            let head = norm_id(rest.split('/').next().unwrap_or(rest));
            let mine = your_ids.iter().any(|id| head.starts_with(id.as_str()));
            if !mine {
                made += 1;
            }
            mine
        })
        .collect();
    (yours, made)
}

/// A pack file Hopper treats as a mod or pack you can turn off: directly in
/// mods/ (a jar), resourcepacks/ or shaderpacks/ (a zip).
fn switchable(rel: &str) -> bool {
    let l = rel.to_ascii_lowercase();
    match l.split_once('/') {
        Some(("mods", n)) => !n.contains('/') && n.ends_with(".jar"),
        Some(("resourcepacks" | "shaderpacks", n)) => !n.contains('/') && n.ends_with(".zip"),
        _ => false,
    }
}

/// Your turned-off copy of pack file `rel`: `rel.disabled`, or another
/// version of the same mod (same name without the version) that is off
/// while no copy of it is on.
fn disabled_twin(root: &Path, rel: &str) -> Option<String> {
    if root.join(format!("{rel}.disabled")).is_file() {
        return Some(format!("{rel}.disabled"));
    }
    let (dir, name) = rel.rsplit_once('/')?;
    let stem = crate::serverpack::jar_stem(name);
    if stem.is_empty() {
        return None;
    }
    let mut off = None;
    for e in fs::read_dir(root.join(dir)).ok()?.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        match n.strip_suffix(".disabled") {
            Some(base) if crate::serverpack::jar_stem(base) == stem => off = Some(format!("{dir}/{n}")),
            None if crate::serverpack::jar_stem(&n) == stem => return None,
            _ => {}
        }
    }
    off
}

/// The mods you turned off that this update changes: pack-listed mods it
/// updates, and pack-shipped jars that would otherwise arrive turned on.
/// Those jars leave `files`: the off list handles them.
fn off_mods(
    inst: &Instance,
    mods: &[ModChange],
    files: &mut Vec<FileChange>,
) -> (Vec<OffMod>, HashMap<String, String>) {
    let mut off = Vec::new();
    let mut old = HashMap::new();
    for m in mods.iter().filter(|m| m.action == ModAction::Update) {
        if let Some(a) = inst
            .addons
            .iter()
            .find(|a| a.file_name == m.from && !a.enabled && a.project_id != 0)
        {
            off.push(OffMod {
                name: m.name.clone(),
                from: m.from.clone(),
                to: m.to.clone(),
                key: format!("p:{}", a.project_id),
                project: a.project_id,
                icon: a.icon.clone(),
                ..Default::default()
            });
        }
    }
    files.retain(|f| {
        if f.action != FileAction::Add || !switchable(&f.rel) {
            return true;
        }
        let Some(twin) = disabled_twin(&inst.root, &f.rel) else {
            return true;
        };
        let name = f.rel.rsplit('/').next().unwrap_or(&f.rel).to_string();
        off.push(OffMod {
            name: crate::serverpack::jar_stem(&name),
            from: twin.rsplit('/').next().unwrap_or(&twin).to_string(),
            to: name,
            key: format!("f:{}", f.rel),
            ..Default::default()
        });
        old.insert(f.rel.clone(), twin);
        false
    });
    off.sort_by_key(|m| m.name.to_lowercase());
    (off, old)
}

/// Pack-shipped jars you deleted (the installed version had them, you
/// don't): they'd come back, so they get their own list. Matched by path,
/// or by name without version in the same folder. They leave `files`.
fn removed_files(
    inst: &Instance,
    files: &mut Vec<FileChange>,
    old: Option<&HashMap<String, String>>,
) -> Vec<OffMod> {
    let Some(old) = old else { return Vec::new() };
    let mut out = Vec::new();
    files.retain(|f| {
        if f.action != FileAction::Add || !switchable(&f.rel) {
            return true;
        }
        let (dir, name) = f.rel.rsplit_once('/').unwrap_or(("", &f.rel));
        let stem = crate::serverpack::jar_stem(name);
        let had = old.keys().find(|o| {
            **o == f.rel
                || (switchable(o)
                    && o.rsplit_once('/').is_some_and(|(d, n)| d == dir && !stem.is_empty() && crate::serverpack::jar_stem(n) == stem))
        });
        // Gone from your folder (no copy on, none off).
        let gone = had.is_some_and(|h| {
            !inst.root.join(h).exists() && !inst.root.join(format!("{h}.disabled")).exists()
        });
        if !gone {
            return true;
        }
        out.push(OffMod {
            name: if stem.is_empty() { name.to_string() } else { stem },
            from: had.and_then(|h| h.rsplit('/').next()).unwrap_or(name).to_string(),
            to: name.to_string(),
            key: format!("rf:{}", f.rel),
            ..Default::default()
        });
        false
    });
    out
}

/// Modrinth pack mods you removed: the new version lists them, they aren't
/// in your folder, and the installed version had the same project.
fn mr_removed(new: &modpack::MrIndex, old: &modpack::MrIndex, mods: &[ModChange]) -> Vec<OffMod> {
    let had: HashSet<&str> = old.files.iter().filter_map(|f| f.project()).collect();
    let adds: HashSet<&str> = mods.iter().filter(|m| m.action == ModAction::Add).map(|m| m.to.as_str()).collect();
    new.files
        .iter()
        .filter(|f| adds.contains(f.file_name()) && f.project().is_some_and(|p| had.contains(p)))
        .map(|f| OffMod {
            name: f.file_name().to_string(),
            from: String::new(),
            to: f.file_name().to_string(),
            key: format!("rp:{}", f.path),
            ..Default::default()
        })
        .collect()
}

/// The pack's mod changes, and (with the installed version's files in
/// `had`, project → file) the pack mods you removed that it would add back
/// and those you updated yourself that it would replace.
fn cf_mod_changes(
    inst: &Instance,
    manifest: &crate::instances::PackManifest,
    had: Option<&HashMap<u64, u64>>,
) -> anyhow::Result<(Vec<ModChange>, Vec<OffMod>, Vec<OffMod>)> {
    let wanted: HashMap<u64, u64> = manifest
        .files
        .iter()
        .map(|f| (f.project_id, f.file_id))
        .collect();
    let mut out = Vec::new();
    for a in inst.addons.iter().filter(|a| a.from_pack) {
        if !wanted.contains_key(&a.project_id) {
            out.push(ModChange {
                name: a.name.clone(),
                from: a.file_name.clone(),
                to: String::new(),
                action: ModAction::Remove,
                project: a.project_id,
                icon: a.icon.clone(),
            });
        }
    }
    let todo: Vec<u64> = wanted
        .iter()
        .filter(|(p, f)| {
            !inst
                .addons
                .iter()
                .any(|a| a.project_id == **p && a.file_id == **f)
        })
        .map(|(_, f)| *f)
        .collect();
    if todo.is_empty() {
        return Ok((out, Vec::new(), Vec::new()));
    }
    let files = cf::get_files(&todo)?;
    let (mut removed, mut mine) = (Vec::new(), Vec::new());
    let projects: HashMap<u64, cf::Mod> =
        cf::get_mods(&files.iter().map(|f| f.mod_id).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .map(|m| (m.id, m))
            .collect();
    let names: HashMap<u64, String> = projects.iter().map(|(k, m)| (*k, m.name.clone())).collect();
    let logo = |id: u64| {
        projects
            .get(&id)
            .and_then(|m| m.logo.as_ref())
            .map(|l| l.thumbnail_url.clone())
            .unwrap_or_default()
    };
    for f in files {
        let have = inst.addons.iter().find(|a| a.project_id == f.mod_id);
        if have.is_none() && had.is_some_and(|h| h.contains_key(&f.mod_id)) {
            removed.push(OffMod {
                name: names.get(&f.mod_id).cloned().unwrap_or_else(|| f.display_name.clone()),
                from: String::new(),
                to: f.file_name.clone(),
                key: format!("r:{}", f.mod_id),
                project: f.mod_id,
                icon: logo(f.mod_id),
                ..Default::default()
            });
            continue;
        }
        // You updated it yourself (it isn't the installed version's file),
        // and it's on: keep yours or take the pack's. Turned-off ones are
        // in the turned-off list.
        if let Some(a) = have
            && a.enabled
            && had.is_some_and(|h| h.get(&f.mod_id).is_some_and(|old| *old != a.file_id))
        {
            mine.push(OffMod {
                name: names.get(&f.mod_id).cloned().unwrap_or_else(|| a.name.clone()),
                from: a.file_name.clone(),
                to: f.file_name.clone(),
                key: format!("u:{}", f.mod_id),
                project: f.mod_id,
                icon: if a.icon.is_empty() { logo(f.mod_id) } else { a.icon.clone() },
                // CurseForge file ids grow with time.
                pack_newer: f.id > a.file_id,
            });
            continue;
        }
        out.push(ModChange {
            project: f.mod_id,
            icon: have.map(|a| a.icon.clone()).filter(|i| !i.is_empty()).unwrap_or_else(|| logo(f.mod_id)),
            name: names
                .get(&f.mod_id)
                .cloned()
                .or_else(|| have.map(|a| a.name.clone()))
                .unwrap_or_else(|| f.display_name.clone()),
            from: have.map(|a| a.file_name.clone()).unwrap_or_default(),
            to: f.file_name.clone(),
            action: if have.is_some() {
                ModAction::Update
            } else {
                ModAction::Add
            },
        });
    }
    out.sort_by_key(|m| (m.action as u8, m.name.to_lowercase()));
    mine.sort_by_key(|m| m.name.to_lowercase());
    Ok((out, removed, mine))
}

fn mr_mod_changes(inst: &Instance, index: &modpack::MrIndex) -> Vec<ModChange> {
    let files: Vec<&modpack::MrIndexFile> = index
        .files
        .iter()
        .filter(|f| {
            f.env
                .as_ref()
                .and_then(|e| e.get("client"))
                .is_none_or(|c| c != "unsupported")
        })
        .collect();
    let name_of = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
    let keep: HashSet<String> = files.iter().map(|f| name_of(&f.path).to_lowercase()).collect();
    let have: HashSet<&str> = inst
        .addons
        .iter()
        .map(|a| a.mr_sha1.as_str())
        .filter(|h| !h.is_empty())
        .collect();
    let mut out: Vec<ModChange> = inst
        .addons
        .iter()
        .filter(|a| a.from_pack && !keep.contains(&a.file_name.to_lowercase()))
        .map(|a| ModChange {
            name: a.name.clone(),
            from: a.file_name.clone(),
            to: String::new(),
            action: ModAction::Remove,
            project: a.project_id,
            icon: a.icon.clone(),
        })
        .collect();
    for f in files {
        let new = f.hashes.get("sha1").is_none_or(|h| !have.contains(h.as_str()))
            && !inst.root.join(&f.path).exists();
        if new {
            out.push(ModChange {
                name: name_of(&f.path),
                from: String::new(),
                to: name_of(&f.path),
                action: ModAction::Add,
                ..Default::default()
            });
        }
    }
    out.sort_by_key(|m| (m.action as u8, m.name.to_lowercase()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(s: &str) -> String {
        sha1_of(s.as_bytes())
    }

    #[test]
    fn three_way_merge_of_override_files() {
        let root = std::env::temp_dir().join(format!("hopper-packdiff-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("config")).unwrap();
        let write = |rel: &str, text: &str| fs::write(root.join(rel), text).unwrap();
        write("config/untouched.toml", "a=1");
        write("config/pack-changed.toml", "b=1");
        write("config/you-changed.toml", "c=mine");
        write("config/both.toml", "d=mine");
        write("config/dropped.toml", "e=1");
        write("config/dropped-edited.toml", "f=mine");
        write("config/same.toml", "g=2");

        let old: HashMap<String, String> = [
            ("config/untouched.toml", h("a=1")),
            ("config/pack-changed.toml", h("b=1")),
            ("config/you-changed.toml", h("c=1")),
            ("config/both.toml", h("d=1")),
            ("config/dropped.toml", h("e=1")),
            ("config/dropped-edited.toml", h("f=1")),
            ("config/same.toml", h("g=1")),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let new: HashMap<String, String> = [
            ("config/untouched.toml", h("a=1")),
            ("config/pack-changed.toml", h("b=2")),
            ("config/you-changed.toml", h("c=1")),
            ("config/both.toml", h("d=2")),
            ("config/brand-new.toml", h("n=1")),
            ("config/same.toml", h("g=2")),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();

        let (changes, same) = plan_files(Some(&old), &new, &root);
        let get = |rel: &str| {
            changes
                .iter()
                .find(|c| c.rel == format!("config/{rel}"))
                .map(|c| c.action)
        };
        assert_eq!(get("untouched.toml"), None);
        assert_eq!(get("pack-changed.toml"), Some(FileAction::Update));
        assert_eq!(get("you-changed.toml"), Some(FileAction::KeepYours));
        assert_eq!(get("both.toml"), Some(FileAction::Conflict));
        assert_eq!(get("brand-new.toml"), Some(FileAction::Add));
        assert_eq!(get("dropped.toml"), Some(FileAction::Remove));
        assert_eq!(get("dropped-edited.toml"), Some(FileAction::KeepDropped));
        assert!(same.contains(&"config/same.toml".to_string()));
        assert!(same.contains(&"config/untouched.toml".to_string()));

        // Without the installed version, any difference is a conflict.
        let (blind, _) = plan_files(None, &new, &root);
        assert!(blind
            .iter()
            .filter(|c| c.rel != "config/brand-new.toml")
            .all(|c| c.action == FileAction::Conflict));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn quick_update_takes_the_packs_files_and_lists_yours() {
        let root = std::env::temp_dir().join(format!("hopper-packdiff-quick-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for d in ["config", "kubejs/server_scripts", "saves/w", "mods"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        let write = |rel: &str, text: &str| fs::write(root.join(rel), text).unwrap();
        write("config/a.toml", "a=mine");
        write("config/mine.toml", "x=1");
        write("kubejs/server_scripts/pack.js", "old");
        write("kubejs/server_scripts/my_recipes.js", "mine");
        write("options.txt", "fov:90");
        write("saves/w/level.dat", "w");
        write("mods/mine.jar", "j");
        let new: HashMap<String, String> = [
            ("config/a.toml", h("a=2")),
            ("kubejs/server_scripts/pack.js", h("new")),
            ("options.txt", h("fov:70")),
            ("config/new.toml", h("n")),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let (files, same) = plan_files(None, &new, &root);
        let plan = UpdatePlan {
            file: cf::File::default(),
            mods: Vec::new(),
            files,
            kept_mods: 0,
            off: Vec::new(),
            removed: Vec::new(),
            updated: Vec::new(),
            off_old: HashMap::new(),
            base_known: false,
            compared: false,
            needed: Vec::new(),
            your_mods: Vec::new(),
            your_files: Vec::new(),
            mod_made: 0,
            new_zip: PathBuf::new(),
            prefixes: Vec::new(),
            same,
            merged: HashMap::new(),
        };
        // Like CurseForge: the pack's copy, except your own settings.
        let take = plan.default_take();
        assert!(take.contains("config/a.toml"));
        assert!(take.contains("kubejs/server_scripts/pack.js"));
        assert!(!take.contains("options.txt"));

        let skip: HashSet<String> = new.keys().cloned().collect();
        let yours = your_files(&root, new.keys().map(String::as_str), &skip);
        assert_eq!(yours, vec!["config/mine.toml".to_string(), "kubejs/server_scripts/my_recipes.js".to_string()]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn warns_when_a_dropped_mod_is_needed_by_yours() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("hopper-packdiff-need-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let jar = |name: &str, toml: String| {
            let mut z = zip::ZipWriter::new(fs::File::create(dir.join(name)).unwrap());
            z.start_file::<_, ()>("META-INF/neoforge.mods.toml", Default::default()).unwrap();
            z.write_all(toml.as_bytes()).unwrap();
            z.finish().unwrap();
        };
        let toml = |id: &str, deps: &str| format!("modLoader=\"javafml\"\n[[mods]]\nmodId=\"{id}\"\nversion=\"1.0\"\n{deps}");
        let req = |owner: &str, id: &str| {
            format!("[[dependencies.{owner}]]\nmodId=\"{id}\"\ntype=\"required\"\nversionRange=\"[1,)\"\nside=\"BOTH\"\n")
        };
        jar("core.jar", toml("core", ""));
        jar("other.jar", toml("other", ""));
        jar("mine.jar", toml("mine", &req("mine", "core")));
        jar("packmod.jar", toml("packmod", &req("packmod", "other")));
        let dropped = vec![
            ("core.jar".to_string(), "Core".to_string(), 1, String::new()),
            ("other.jar".to_string(), "Other".to_string(), 2, String::new()),
        ];
        // Only mine.jar is yours: the pack's own mod needing "other" isn't
        // your problem.
        let n = needed_by_yours(&dir, &dropped, |j| (j == "mine.jar").then(|| ("Mine".to_string(), 9, String::new())));
        assert_eq!(n.len(), 1);
        assert_eq!((n[0].file.as_str(), n[0].by.as_str(), n[0].project), ("core.jar", "Mine", 1));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_files_of_your_mods_are_yours() {
        let files = vec![
            "config/ars_conflux-common.toml".to_string(),
            "config/goety/goety.toml".to_string(),
            "config/mekanism/general.toml".to_string(),
            "defaultconfigs/create-server.toml".to_string(),
            "kubejs/server_scripts/mine.js".to_string(),
        ];
        let ids = vec!["arsconflux".to_string(), "goety".to_string()];
        let (yours, made) = split_made(files, &ids);
        assert_eq!(
            yours,
            vec!["config/ars_conflux-common.toml", "config/goety/goety.toml", "kubejs/server_scripts/mine.js"]
        );
        assert_eq!(made, 2);
    }

    #[test]
    fn merges_changes_to_different_lines() {
        let base = "a=1\nb=1\nc=1\nd=1\n";
        let yours = "a=mine\nb=1\nc=1\nd=1\n";
        let theirs = "a=1\nb=1\nc=1\nd=2\ne=new\n";
        assert_eq!(
            merge3(base, yours, theirs).as_deref(),
            Some("a=mine\nb=1\nc=1\nd=2\ne=new\n")
        );
        // Same line changed both ways: no merge.
        assert_eq!(merge3(base, "a=mine\nb=1\nc=1\nd=1\n", "a=2\nb=1\nc=1\nd=1\n"), None);
        // The same change on both sides is fine.
        assert_eq!(
            merge3(base, "a=2\nb=1\nc=1\nd=1\n", "a=2\nb=1\nc=1\nd=1\n").as_deref(),
            Some("a=2\nb=1\nc=1\nd=1\n")
        );
    }

    #[test]
    fn turned_off_mods_stay_off_unless_turned_on() {
        let root = std::env::temp_dir().join(format!("hopper-packdiff-off-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("mods")).unwrap();
        // Pack-shipped jars you turned off: same name, an older version, and
        // one you left on.
        fs::write(root.join("mods/same.jar.disabled"), "old").unwrap();
        fs::write(root.join("mods/foo-1.0.jar.disabled"), "old").unwrap();
        fs::write(root.join("mods/on-1.0.jar"), "old").unwrap();
        let inst = Instance {
            root: root.clone(),
            addons: vec![
                crate::instances::Addon {
                    project_id: 5,
                    file_name: "bar-1.jar".into(),
                    enabled: false,
                    from_pack: true,
                    ..Default::default()
                },
                crate::instances::Addon {
                    project_id: 6,
                    file_name: "baz-1.jar".into(),
                    enabled: true,
                    from_pack: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let change = |from: &str, to: &str| ModChange {
            name: from.into(),
            from: from.into(),
            to: to.into(),
            action: ModAction::Update,
            ..Default::default()
        };
        let mods = vec![change("bar-1.jar", "bar-2.jar"), change("baz-1.jar", "baz-2.jar")];
        let add = |rel: &str| FileChange { rel: rel.into(), action: FileAction::Add };
        let mut files = vec![
            add("mods/same.jar"),
            add("mods/foo-2.0.jar"),
            add("mods/on-2.0.jar"),
            add("config/new.toml"),
        ];
        let (off, off_old) = off_mods(&inst, &mods, &mut files);
        let keys: Vec<&str> = off.iter().map(|m| m.key.as_str()).collect();
        assert_eq!(keys, ["p:5", "f:mods/foo-2.0.jar", "f:mods/same.jar"]);
        let rest: Vec<&str> = files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rest, ["mods/on-2.0.jar", "config/new.toml"], "jars you left on update as usual");

        let plan = UpdatePlan {
            file: cf::File::default(),
            mods,
            files,
            kept_mods: 0,
            off,
            off_old,
            removed: Vec::new(),
            updated: Vec::new(),
            base_known: true,
            compared: true,
            needed: Vec::new(),
            your_mods: Vec::new(),
            your_files: Vec::new(),
            mod_made: 0,
            new_zip: PathBuf::new(),
            prefixes: Vec::new(),
            same: Vec::new(),
            merged: HashMap::new(),
        };
        // Keep all off.
        let o = plan.decisions_with(&HashSet::new(), &HashSet::new());
        assert!(o.disable.contains("mods/foo-2.0.jar") && o.disable.contains("mods/same.jar"));
        assert!(o.remove.contains(&"mods/foo-1.0.jar.disabled".to_string()));
        assert!(o.backup.contains(&"mods/same.jar.disabled".to_string()));
        assert!(o.enable.is_empty());
        // Turn them on.
        let on: HashSet<String> = ["p:5", "f:mods/foo-2.0.jar"].map(String::from).into();
        let o = plan.decisions_with(&HashSet::new(), &on);
        assert!(!o.disable.contains("mods/foo-2.0.jar") && o.disable.contains("mods/same.jar"));
        assert!(o.remove.contains(&"mods/foo-1.0.jar.disabled".to_string()));
        assert_eq!(o.enable, [5u64].into());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn mods_you_removed_stay_out_unless_added_back() {
        let root = std::env::temp_dir().join(format!("hopper-packdiff-rm-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("mods")).unwrap();
        fs::write(root.join("mods/kept-1.jar"), "x").unwrap();
        let inst = Instance { root: root.clone(), ..Default::default() };
        // The installed version shipped gone-1 (you deleted it), kept-1 and
        // a config; the new one ships gone-2, kept-2, brand-new and configs.
        let old: HashMap<String, String> = ["mods/gone-1.jar", "mods/kept-1.jar", "config/a.toml"]
            .into_iter()
            .map(|r| (r.to_string(), h(r)))
            .collect();
        let add = |rel: &str| FileChange { rel: rel.into(), action: FileAction::Add };
        let mut files = vec![add("mods/gone-2.jar"), add("mods/kept-2.jar"), add("mods/new-1.jar"), add("config/b.toml")];
        let removed = removed_files(&inst, &mut files, Some(&old));
        let keys: Vec<&str> = removed.iter().map(|m| m.key.as_str()).collect();
        assert_eq!(keys, ["rf:mods/gone-2.jar"]);
        let rest: Vec<&str> = files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rest, ["mods/kept-2.jar", "mods/new-1.jar", "config/b.toml"]);

        let plan = UpdatePlan {
            file: cf::File::default(),
            mods: Vec::new(),
            files,
            kept_mods: 0,
            off: Vec::new(),
            off_old: HashMap::new(),
            removed: [
                removed.clone(),
                vec![OffMod { name: "x".into(), to: "x.jar".into(), key: "r:9".into(), ..Default::default() }],
            ]
            .concat(),
            updated: Vec::new(),
            base_known: true,
            compared: true,
            needed: Vec::new(),
            your_mods: Vec::new(),
            your_files: Vec::new(),
            mod_made: 0,
            new_zip: PathBuf::new(),
            prefixes: Vec::new(),
            same: Vec::new(),
            merged: HashMap::new(),
        };
        let o = plan.decisions_with(&HashSet::new(), &HashSet::new());
        assert!(o.skip.contains("mods/gone-2.jar"));
        assert!(o.leave_out.contains("r:9"));
        let back: HashSet<String> = ["rf:mods/gone-2.jar", "r:9"].map(String::from).into();
        let o = plan.decisions_with(&HashSet::new(), &back);
        assert!(!o.skip.contains("mods/gone-2.jar") && o.leave_out.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn decisions_follow_the_plan() {
        let plan = UpdatePlan {
            file: cf::File::default(),
            mods: Vec::new(),
            files: vec![
                FileChange { rel: "a".into(), action: FileAction::Update },
                FileChange { rel: "b".into(), action: FileAction::KeepYours },
                FileChange { rel: "c".into(), action: FileAction::Conflict },
                FileChange { rel: "d".into(), action: FileAction::Conflict },
                FileChange { rel: "e".into(), action: FileAction::Remove },
            ],
            kept_mods: 0,
            off: Vec::new(),
            off_old: HashMap::new(),
            removed: Vec::new(),
            updated: Vec::new(),
            base_known: true,
            compared: true,
            needed: Vec::new(),
            your_mods: Vec::new(),
            your_files: Vec::new(),
            mod_made: 0,
            new_zip: PathBuf::new(),
            prefixes: Vec::new(),
            same: vec!["s".into()],
            merged: HashMap::new(),
        };
        let o = plan.decisions(&["d".to_string()].into());
        assert!(o.skip.contains("b") && o.skip.contains("c") && o.skip.contains("s"));
        assert!(!o.skip.contains("a") && !o.skip.contains("d"));
        assert_eq!(o.backup, vec!["d".to_string()]);
        assert_eq!(o.remove, vec!["e".to_string()]);
    }
}

#[cfg(test)]
mod real {
    use super::*;

    /// Plans (never applies) the pending update of a real instance:
    /// `HOPPER_PLAN=<instance folder> cargo test plan_real -- --ignored --nocapture`
    #[test]
    #[ignore = "network and a real instance"]
    fn plan_real() {
        let Ok(dir) = std::env::var("HOPPER_PLAN") else { return };
        crate::env::load(Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env")));
        let inst = crate::instances::load(Path::new(&dir)).expect("instance");
        let p = inst.pack.clone().expect("a pack");
        let ups = crate::instance_ops::pack_updates(&[(
            p.clone(),
            inst.game_version.clone(),
            inst.settings.release_channel,
        )])
        .unwrap();
        let file = ups.first().map(|(_, f)| f.clone()).expect("an update");
        let jobs = crate::jobs::Jobs::new(crate::waker::noop());
        let plan = plan(&inst, &file, true, &jobs.start("plan")).unwrap();
        println!("{} -> {} (base known: {})", p.file_name, file.file_name, plan.base_known);
        for a in [ModAction::Add, ModAction::Update, ModAction::Remove] {
            println!("mods {a:?}: {}", plan.mod_count(a));
        }
        println!("mods you added (kept): {}", plan.kept_mods);
        for a in [
            FileAction::Add,
            FileAction::Update,
            FileAction::KeepYours,
            FileAction::Merge,
            FileAction::Conflict,
            FileAction::Remove,
            FileAction::KeepDropped,
        ] {
            println!("files {a:?}: {}", plan.count(a));
        }
        println!("files identical: {}", plan.same.len());
        for c in plan.conflicts().take(15) {
            println!("  conflict: {}", c.rel);
        }
        if let Some(c) = plan.conflicts().next() {
            let d = plan.diff(&inst.root, &c.rel).unwrap();
            println!("{}", d.unwrap_or("(binary)".into()).lines().take(25).collect::<Vec<_>>().join("\n"));
        }
    }
}
