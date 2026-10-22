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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

#[derive(Clone, Debug)]
pub struct FileChange {
    /// Relative to the instance, with `/`.
    pub rel: String,
    pub action: FileAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModAction {
    Add,
    Update,
    Remove,
}

#[derive(Clone, Debug)]
pub struct ModChange {
    pub name: String,
    /// The file you have (empty for an addition).
    pub from: String,
    /// The pack's file (empty for a removal).
    pub to: String,
    pub action: ModAction,
}

/// Everything an update would do, before it does it.
#[derive(Clone, Debug)]
pub struct UpdatePlan {
    pub file: cf::File,
    pub mods: Vec<ModChange>,
    pub files: Vec<FileChange>,
    /// Mods you added on top of the pack; they stay.
    pub kept_mods: usize,
    /// The installed pack version could be read, so your own changes can
    /// be told apart from the pack's. Without it every differing file is a
    /// conflict.
    pub base_known: bool,
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

    /// Conflicts that default to the pack's copy (pack content folders).
    pub fn default_take(&self) -> HashSet<String> {
        self.conflicts()
            .filter(|f| pack_content(&f.rel))
            .map(|f| f.rel.clone())
            .collect()
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

/// Downloads the new pack version (and the installed one, to compare
/// against) and works out what updating to `file` would do.
pub fn plan(inst: &Instance, file: &cf::File, job: &Job) -> anyhow::Result<UpdatePlan> {
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
    // The installed version, to tell your edits from the pack's.
    job.detail("Downloading the installed version to compare");
    let old_zip = crate::source::file(pack.project_id, pack.file_id)
        .and_then(|f| fetch(&f))
        .inspect_err(|e| log::warn!("installed pack version: {e:#}"))
        .ok();

    let (prefixes, mods) = if mr {
        let index = modpack::read_mr_index(&new_zip)?;
        (
            modpack::MR_OVERRIDES.iter().map(|s| s.to_string()).collect(),
            mr_mod_changes(inst, &index),
        )
    } else {
        let manifest = modpack::read_manifest(&new_zip)?;
        let prefix = if manifest.overrides.is_empty() {
            "overrides".to_string()
        } else {
            manifest.overrides.clone()
        };
        job.detail("Comparing mods");
        (vec![prefix], cf_mod_changes(inst, &manifest)?)
    };
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
    Ok(UpdatePlan {
        file: file.clone(),
        mods,
        files,
        kept_mods: inst.addons.iter().filter(|a| !a.from_pack).count(),
        base_known: old.is_some(),
        new_zip,
        prefixes,
        same,
        merged,
    })
}

fn cf_mod_changes(
    inst: &Instance,
    manifest: &crate::instances::PackManifest,
) -> anyhow::Result<Vec<ModChange>> {
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
        return Ok(out);
    }
    let files = cf::get_files(&todo)?;
    let names: HashMap<u64, String> =
        cf::get_mods(&files.iter().map(|f| f.mod_id).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .map(|m| (m.id, m.name))
            .collect();
    for f in files {
        let have = inst.addons.iter().find(|a| a.project_id == f.mod_id);
        out.push(ModChange {
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
    Ok(out)
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
            base_known: true,
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
        crate::env::load(Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.env")));
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
        let plan = plan(&inst, &file, &jobs.start("plan")).unwrap();
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
