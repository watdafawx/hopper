//! Adding, updating, toggling and removing projects in an instance.
//! Nothing is deleted outright: replaced and removed files go to the
//! instance's `.hopper-backups` folder.

use crate::cf::{self, Loader};
use crate::instances::{Addon, Instance, env_from_tags, folder_for_class};
use crate::jobs::Job;
use crate::sync::backup_into;
use anyhow::Context;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;

pub fn loader_of(inst: &Instance) -> Loader {
    Loader::from_loader_id(&inst.loader)
}

pub fn addon_from(m: &cf::Mod, f: &cf::File, from_pack: bool) -> Addon {
    Addon {
        project_id: m.id,
        file_id: f.id,
        file_name: f.file_name.clone(),
        name: m.name.clone(),
        icon: m.icon().unwrap_or_default().to_string(),
        page: m.page(),
        author: m.author().to_string(),
        enabled: true,
        from_pack,
        env: env_from_tags(f.game_versions.iter().map(String::as_str)),
        class_id: m.class_id.unwrap_or(cf::CLASS_MODS),
        file_date: f.file_date.clone(),
        date_added: crate::time::now_iso(),
        release_type: f.release_type,
        locked: false,
        cf_locked: false,
        requires: match &f.modrinth {
            Some(mr) => mr
                .requires
                .iter()
                .map(|p| crate::modrinth::num_id(p))
                .collect(),
            None => f
                .dependencies
                .iter()
                .filter(|d| d.relation_type == cf::DEP_REQUIRED)
                .map(|d| d.mod_id)
                .collect(),
        },
        mr_project: f
            .modrinth
            .as_ref()
            .map(|m| m.project_id.clone())
            .unwrap_or_default(),
        mr_version: f
            .modrinth
            .as_ref()
            .map(|m| m.version_id.clone())
            .unwrap_or_default(),
        mr_sha1: f
            .modrinth
            .as_ref()
            .map(|m| m.sha1.clone())
            .unwrap_or_default(),
    }
}

/// A new file of a mod keeps when it was first added (for "NEW" and the
/// Date added sort), whether its updates are ignored and whether it is
/// disabled. The caller puts the file at `addon_path` of the result.
pub fn keep_history(new: &mut Addon, old: &Addon) {
    if !old.date_added.is_empty() {
        new.date_added = old.date_added.clone();
    }
    new.locked = old.locked;
    new.cf_locked = old.cf_locked;
    new.enabled = old.enabled;
}

/// Same file, ignoring case (Windows paths).
pub(crate) fn same_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    a.to_string_lossy()
        .replace('\\', "/")
        .eq_ignore_ascii_case(&b.to_string_lossy().replace('\\', "/"))
}

/// Puts the new file of a project in place of the old one: `new` takes the
/// old entry's history (including disabled), the old file goes to backups
/// unless the new one lands on the same path, and `downloaded` moves to
/// where `new` says (`x.jar.disabled` for a disabled mod).
pub(crate) fn replace_addon(
    inst: &mut Instance,
    old: Option<Addon>,
    mut new: Addon,
    downloaded: &std::path::Path,
) -> anyhow::Result<()> {
    if let Some(old) = old {
        keep_history(&mut new, &old);
        let old_path = inst.addon_path(&old);
        if !same_path(&old_path, &inst.addon_path(&new)) && !same_path(&old_path, downloaded) {
            // A running game holding the old jar doesn't stop the update:
            // the old one leaves when the game lets go of it.
            if let Err(e) = backup_into(&inst.root, &old_path) {
                if !crate::locked::is_locked(&e) {
                    return Err(e);
                }
                crate::locked::queue(&inst.root, &old_path, false)?;
            }
        }
    }
    let to = inst.addon_path(&new);
    crate::locked::unqueue(&inst.root, &to);
    if to != downloaded {
        if let Some(p) = to.parent() {
            fs::create_dir_all(p)?;
        }
        if fs::rename(downloaded, &to).is_err() {
            fs::copy(downloaded, &to)
                .with_context(|| format!("moving {} into place", to.display()))?;
            let _ = fs::remove_file(downloaded);
        }
    }
    // Another entry for the same file (the same pack from CurseForge and
    // Modrinth) is replaced, not doubled.
    let path = inst.addon_path(&new);
    inst.addons.retain(|a| !same_path(&inst_path(&inst.root, a), &path));
    inst.addons.push(new);
    Ok(())
}

fn inst_path(root: &std::path::Path, a: &Addon) -> std::path::PathBuf {
    let mut name = a.file_name.clone();
    if !a.enabled {
        name.push_str(".disabled");
    }
    root.join(a.folder()).join(name)
}

/// Best file of a project for this instance: newest release, else newest of any kind.
/// Resource and shader packs ignore the loader filter.
pub fn best_file(
    project: u64,
    class_id: Option<u32>,
    inst: &Instance,
) -> anyhow::Result<Option<cf::File>> {
    let is_mod = class_id.unwrap_or(cf::CLASS_MODS) == cf::CLASS_MODS;
    let loader = if is_mod { loader_of(inst) } else { Loader::Any };
    let files = crate::source::files(project, &inst.game_version, loader)?;
    let pick = files
        .iter()
        .find(|f| f.release_type == 1)
        .or(files.first())
        .cloned();
    Ok(pick)
}

/// Downloads one file into the instance, replacing the project's old file.
fn put_file(
    inst: &mut Instance,
    m: &cf::Mod,
    f: &cf::File,
    from_pack: bool,
    job: &Job,
) -> anyhow::Result<()> {
    match fetch_file(&inst.root, m, f, from_pack, job, true, &|_| {})? {
        Fetched::File(dest) => place_file(inst, m, f, from_pack, &dest),
        Fetched::Pending(p) => add_pending_note(&inst.root, p, job),
    }
}

/// What [`fetch_file`] got.
enum Fetched {
    /// Downloaded to this path in the instance.
    File(std::path::PathBuf),
    /// Website-only: the user downloads it by hand.
    Pending(crate::modpack::Pending),
}

/// Downloads `f` into its folder without touching the instance's records,
/// so several can run at once. `bytes`: report byte progress on `job`.
fn fetch_file(
    root: &std::path::Path,
    m: &cf::Mod,
    f: &cf::File,
    from_pack: bool,
    job: &Job,
    bytes: bool,
    tick: &(dyn Fn(u64) + Sync),
) -> anyhow::Result<Fetched> {
    let folder = folder_for_class(m.class_id.unwrap_or(cf::CLASS_MODS));
    let Ok(url) = cf::download_url(f) else {
        // The author allows website downloads only: the user downloads it in
        // the browser and Hopper picks it up from Downloads.
        let e = f.expect();
        return Ok(Fetched::Pending(crate::modpack::Pending {
            name: m.name.clone(),
            file_name: f.file_name.clone(),
            folder: folder.to_string(),
            project_id: m.id,
            file_id: f.id,
            url: f.manual_url(m),
            reason: "The author only allows downloads from the CurseForge website".into(),
            size: e.len,
            sha1: e.sha1,
            addon: Some(addon_from(m, f, from_pack)),
        }));
    };
    let dest = root.join(folder).join(&f.file_name);
    let total = f.file_length;
    if bytes {
        job.detail(format!("Downloading {}", f.file_name));
    }
    cf::download_checked(&url, &dest, &f.expect(), Some(job), |n| {
        if bytes && total > 0 {
            job.progress(n as usize, total as usize);
        }
        tick(n);
    })?;
    Ok(Fetched::File(dest))
}

/// Records a downloaded file in the instance in place of the project's old one.
fn place_file(
    inst: &mut Instance,
    m: &cf::Mod,
    f: &cf::File,
    from_pack: bool,
    dest: &std::path::Path,
) -> anyhow::Result<()> {
    let new = addon_from(m, f, from_pack);
    let old = inst
        .addons
        .iter()
        .position(|a| a.project_id == m.id)
        .map(|i| inst.addons.remove(i));
    replace_addon(inst, old, new, dest)
}

fn add_pending_note(root: &std::path::Path, p: crate::modpack::Pending, job: &Job) -> anyhow::Result<()> {
    let name = p.name.clone();
    crate::modpack::add_pending(root, &[p])?;
    job.note(format!(
        "{name}: the author only allows downloads from the CurseForge website. Hopper's download window gets it from there (or save it to Downloads yourself) and adds it when it arrives."
    ));
    Ok(())
}

/// What an install of a project would download: the project and every
/// required dependency the instance lacks, in install order (the project
/// first). Resolving only reads the APIs; nothing is downloaded.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub steps: Vec<(cf::Mod, cf::File)>,
    /// Mods a resource or shader pack asks for, only suggested.
    pub suggest: Vec<String>,
    /// Dependencies with no file for this version, and incompatibilities.
    pub notes: Vec<String>,
    /// Which project asks for each dependency in `steps`, by id.
    pub by: HashMap<u64, String>,
    /// Required mods the instance already has: name, and whether it is on
    /// (an off one is turned back on).
    pub kept: Vec<(String, bool)>,
    /// Installed entries that stand in for a dependency from the other
    /// site (CurseForge's Pipez for Modrinth's), by project id.
    pub same: Vec<u64>,
}

impl Plan {
    /// The dependencies it adds, without the project itself.
    pub fn extra(&self) -> &[(cf::Mod, cf::File)] {
        self.steps.get(1..).unwrap_or_default()
    }
}

/// Resolves what [`install`] would add for `file` of `m`.
pub fn plan(inst: &Instance, m: &cf::Mod, file: &cf::File, job: Option<&Job>) -> anyhow::Result<Plan> {
    let have: HashSet<u64> = inst.addons.iter().map(|a| a.project_id).collect();
    let mut out = Plan::default();

    // Breadth-first over required dependencies.
    let mut queue: VecDeque<(cf::Mod, cf::File)> = VecDeque::from([(m.clone(), file.clone())]);
    let mut seen: HashSet<u64> = HashSet::from([m.id]);
    let is_mod = |m: &cf::Mod| m.class_id.unwrap_or(cf::CLASS_MODS) == cf::CLASS_MODS;
    while let Some((pm, pf)) = queue.pop_front() {
        let required: Vec<u64> = match &pf.modrinth {
            Some(mr) => mr
                .requires
                .iter()
                .map(|p| crate::modrinth::num_id(p))
                .collect(),
            None => pf
                .dependencies
                .iter()
                .filter(|d| d.relation_type == cf::DEP_REQUIRED)
                .map(|d| d.mod_id)
                .collect(),
        };
        for dep in required {
            if !seen.insert(dep) {
                continue;
            }
            if have.contains(&dep) {
                if is_mod(&pm)
                    && let Some(a) = inst.addons.iter().find(|a| a.project_id == dep)
                {
                    out.kept.push((a.name.clone(), a.enabled));
                }
                continue;
            }
            if let Some(job) = job {
                job.detail(format!("Resolving dependency {dep}"));
            }
            let dm = crate::source::get_mod(dep)?;
            // Already here from the other site: installing it again would
            // swap the entry (and its history) for a new one.
            if let Some(a) = crate::browse::installed_addon(inst, &dm) {
                if is_mod(&pm) {
                    out.kept.push((a.name.clone(), a.enabled));
                    out.same.push(a.project_id);
                }
                continue;
            }
            if !is_mod(&pm) && is_mod(&dm) {
                out.suggest.push(format!("{} suggests the mod {}", pm.name, dm.name));
                continue;
            }
            match best_file(dep, dm.class_id, inst)? {
                Some(df) => {
                    out.by.insert(dep, pm.name.clone());
                    queue.push_back((dm, df))
                }
                None => out.notes.push(format!(
                    "{} needs {}, which has no file for this version",
                    pm.name, dm.name
                )),
            }
        }
        for d in pf
            .dependencies
            .iter()
            .filter(|d| d.relation_type == cf::DEP_INCOMPATIBLE)
        {
            if have.contains(&d.mod_id) {
                out.notes.push(format!(
                    "{} is marked incompatible with an installed mod ({})",
                    pm.name, d.mod_id
                ));
            }
        }
        out.steps.push((pm, pf));
    }
    Ok(out)
}

/// Installs `file` of project `m` and every required dependency the instance
/// lacks. Returns the names of what was added. Mods a resource or shader
/// pack asks for are only suggested (a note), never downloaded.
pub fn install(
    inst: &mut Instance,
    m: &cf::Mod,
    file: &cf::File,
    job: &Job,
) -> anyhow::Result<Vec<String>> {
    let mut added = Vec::new();
    let is_mod = |m: &cf::Mod| m.class_id.unwrap_or(cf::CLASS_MODS) == cf::CLASS_MODS;
    let Plan { steps: plan, suggest, notes, same, .. } = self::plan(inst, m, file, Some(job))?;
    for n in notes {
        job.note(n);
    }

    for (i, (pm, pf)) in plan.iter().enumerate() {
        job.progress(i, plan.len());
        put_file(inst, pm, pf, false, job)?;
        added.push(pm.name.clone());
    }
    for n in suggest {
        job.note(n);
    }
    // Dependencies that are installed but turned off go back on (mods
    // only ask for this; packs just suggest).
    let need: HashSet<u64> = plan
        .iter()
        .filter(|(pm, _)| is_mod(pm))
        .flat_map(|(_, pf)| match &pf.modrinth {
            Some(mr) => mr.requires.iter().map(|p| crate::modrinth::num_id(p)).collect::<Vec<_>>(),
            None => pf
                .dependencies
                .iter()
                .filter(|d| d.relation_type == cf::DEP_REQUIRED)
                .map(|d| d.mod_id)
                .collect(),
        })
        .chain(same)
        .collect();
    let off: Vec<usize> = (0..inst.addons.len())
        .filter(|&i| !inst.addons[i].enabled && need.contains(&inst.addons[i].project_id))
        .collect();
    for i in off {
        set_enabled(inst, i, true)?;
        job.note(format!("Turned {} back on (needed)", inst.addons[i].name));
    }
    inst.save()?;
    Ok(added)
}

/// The project for a mod id a jar asks for ("geckolib"): Modrinth's slug,
/// else a Modrinth or CurseForge search hit whose slug or name is that id.
/// Only exact matches count, so a lookalike is never installed.
fn squash_id(s: &str) -> String {
    s.to_lowercase().replace(['-', '_', ' ', '.'], "")
}

/// Word endings library ids glue on ("cyclopscore", "codechickenlib")
/// that their pages split off ("cyclops-core", "codechicken-lib").
const ID_SUFFIXES: &[&str] = &["library", "core", "lib", "api", "config", "utils", "util"];

/// `mod_id` with a glued-on ending split off: ("cyclops", "core").
fn split_id_suffix(mod_id: &str) -> Option<(&str, &str)> {
    ID_SUFFIXES.iter().find_map(|suf| {
        let head = mod_id.strip_suffix(suf)?;
        let head = head.trim_end_matches(['_', '-']);
        (head.len() >= 3).then_some((head, *suf))
    })
}

/// Modrinth slugs to try for a mod id, most likely first.
fn slug_tries(mod_id: &str) -> Vec<String> {
    let dashed = mod_id.replace('_', "-");
    let mut tries = vec![mod_id.to_string(), dashed.clone()];
    if let Some((head, suf)) = split_id_suffix(mod_id) {
        tries.push(format!("{}-{suf}", head.replace('_', "-")));
    }
    tries.push(format!("{dashed}-api"));
    tries.push(format!("{dashed}-lib"));
    let mut seen = std::collections::HashSet::new();
    tries.retain(|t| seen.insert(t.clone()));
    tries
}

/// Whether a search hit's slug or name is the mod id. A trailing version
/// tail on the slug is allowed (CurseForge's "codechicken-lib-1-8").
fn hit_is_mod_id(want: &str, slug: &str, name: &str) -> bool {
    let slug_sq = squash_id(slug);
    let slug_trimmed = slug.trim_end_matches(|c: char| c.is_ascii_digit() || c == '-');
    slug_sq == want || squash_id(name) == want || (slug_trimmed.len() < slug.len() && squash_id(slug_trimmed) == want)
}

pub fn project_for_mod_id(inst: &Instance, mod_id: &str) -> anyhow::Result<Option<cf::Mod>> {
    let squash = squash_id;
    let want = squash(mod_id);
    // Library ids whose pages are named differently.
    const ALIASES: &[(&str, &str)] = &[
        ("kotlinforforge", "kotlin-for-forge"),
        ("forgeconfigapiport", "forge-config-api-port"),
        ("fabric", "fabric-api"),
        ("cloth_config", "cloth-config"),
        ("mixinextras", "mixinextras"),
        ("terrablender", "terrablender"),
        ("resourcefullib", "resourceful-lib"),
        ("resourcefulconfig", "resourceful-config"),
        ("puzzleslib", "puzzles-lib"),
        ("searchables", "searchables"),
        ("ftblibrary", "ftb-library-forge"),
    ];
    let alias = ALIASES.iter().find(|(id, _)| *id == mod_id).map(|(_, s)| s.to_string());
    let tries = alias.into_iter().chain(slug_tries(mod_id));
    for slug in tries {
        if let Ok(p) = crate::modrinth::project(&slug)
            && p.project_type == "mod"
            && (squash(&p.slug).starts_with(&want) || squash(&p.title).starts_with(&want) || slug != mod_id)
        {
            return Ok(Some(crate::modrinth::project_to_mod(&p, "")));
        }
    }
    // Search the id as typed, then with a glued-on ending split off
    // ("cyclops core"), as search engines match whole words.
    let mut texts = vec![mod_id.replace(['_', '-'], " ")];
    if let Some((head, suf)) = split_id_suffix(mod_id) {
        texts.push(format!("{} {suf}", head.replace(['_', '-'], " ")));
    }
    for text in texts {
        let search = cf::Search {
            class_id: cf::CLASS_MODS,
            text,
            game_version: inst.game_version.clone(),
            loader: loader_of(inst),
            ..Default::default()
        };
        for modrinth in [true, false] {
            let Ok((hits, _)) = crate::source::search(&search, modrinth) else { continue };
            if let Some(m) = hits.into_iter().find(|m| hit_is_mod_id(&want, &m.slug, &m.name)) {
                return Ok(Some(m));
            }
        }
    }
    Ok(None)
}

/// Installs the mod a jar asks for by its id, with its own dependencies.
pub fn install_mod_id(inst: &mut Instance, mod_id: &str, job: &Job) -> anyhow::Result<Vec<String>> {
    job.detail(format!("Looking up {mod_id}"));
    let m = project_for_mod_id(inst, mod_id)?
        .with_context(|| format!("No mod called {mod_id} on Modrinth or CurseForge"))?;
    let f = best_file(m.id, m.class_id, inst)?
        .with_context(|| format!("{} has no file for Minecraft {} with this loader", m.name, inst.game_version))?;
    install(inst, &m, &f, job)
}

pub fn set_enabled(inst: &mut Instance, idx: usize, enabled: bool) -> anyhow::Result<()> {
    let a = inst.addons.get(idx).context("no such mod")?.clone();
    if a.enabled == enabled {
        return Ok(());
    }
    let from = inst.addon_path(&a);
    inst.addons[idx].enabled = enabled;
    let to = inst.addon_path(&inst.addons[idx]);
    if let Err(e) = fs::rename(&from, &to) {
        inst.addons[idx].enabled = a.enabled;
        return Err(e).with_context(|| format!("renaming {}", from.display()));
    }
    inst.save()
}

pub fn remove(inst: &mut Instance, idx: usize) -> anyhow::Result<()> {
    let a = inst.addons.get(idx).context("no such mod")?.clone();
    move_out(&inst.root, &inst.addon_path(&a), false)?;
    inst.addons.remove(idx);
    inst.save()
}

/// `e`, explained when Windows refused because a running game has the file
/// open. NeoForge (and Forge 1.17+) open mod jars in a way that lets them
/// be renamed and deleted while playing; Fabric and older Forge don't.
pub fn explain_locked(e: anyhow::Error) -> anyhow::Error {
    if crate::locked::is_locked(&e) {
        e.context("the game has this file open, so Windows won't let it change. Close the game and try again")
    } else {
        e
    }
}

/// Deletes a file for good: to the Recycle Bin, or straight away on a
/// network drive (which has none).
pub(crate) fn delete_path(path: &std::path::Path) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if crate::instance_ops::is_remote_drive(path) {
        if path.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
        return Ok(());
    }
    trash::delete(path).with_context(|| format!("deleting {}", path.display()))
}

/// Moves a file to backups (or the Recycle Bin with `delete`). One a
/// running game holds open is queued to leave when the game lets go
/// ([`crate::locked`]) instead of failing.
pub(crate) fn move_out(root: &std::path::Path, path: &std::path::Path, delete: bool) -> anyhow::Result<()> {
    let r = if delete {
        delete_path(path)
    } else {
        backup_into(root, path).map(|_| ())
    };
    match r {
        Err(e) if crate::locked::is_locked(&e) => crate::locked::queue(root, path, delete),
        r => r,
    }
}

/// Deletes an installed file (to the Recycle Bin) instead of keeping a
/// backup.
pub fn delete(inst: &mut Instance, idx: usize) -> anyhow::Result<()> {
    let a = inst.addons.get(idx).context("no such mod")?.clone();
    move_out(&inst.root, &inst.addon_path(&a), true)?;
    inst.addons.remove(idx);
    inst.save()
}

/// Deletes an installed file at once, skipping the Recycle Bin (Shift with
/// Delete). One the game holds open leaves for the Recycle Bin once it
/// lets go, as a locked file can't be removed now.
pub fn purge(inst: &mut Instance, idx: usize) -> anyhow::Result<()> {
    let a = inst.addons.get(idx).context("no such mod")?.clone();
    purge_path(&inst.root, &inst.addon_path(&a))?;
    inst.addons.remove(idx);
    inst.save()
}

/// [`purge`] for a jar Hopper does not track.
pub fn purge_loose_file(inst: &Instance, file_name: &str) -> anyhow::Result<()> {
    purge_path(&inst.root, &inst.mods_dir().join(file_name))
}

fn purge_path(root: &std::path::Path, path: &std::path::Path) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let r = if path.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }
        .with_context(|| format!("deleting {}", path.display()));
    match r {
        Err(e) if crate::locked::is_locked(&e) => crate::locked::queue(root, path, true),
        r => r,
    }
}

/// [`delete`] for a jar Hopper does not track.
pub fn delete_loose_file(inst: &Instance, file_name: &str) -> anyhow::Result<()> {
    move_out(&inst.root, &inst.mods_dir().join(file_name), true)
}

/// Removes a jar Hopper does not track (no CurseForge project).
pub fn remove_loose_file(inst: &Instance, file_name: &str) -> anyhow::Result<()> {
    move_out(&inst.root, &inst.mods_dir().join(file_name), false)
}

#[derive(Clone, Debug)]
pub struct Update {
    pub project_id: u64,
    pub name: String,
    pub from: String,
    pub file: cf::File,
}

impl Update {
    /// The installed and offered versions, read from the two file names.
    pub fn versions(&self) -> (String, String) {
        let (old, new) = version_change(&self.from, &self.file.file_name);
        // File names that don't say a version ("Pack.zip" both times):
        // the new file's date tells them apart.
        if old == new {
            let when = crate::time::date_of(&self.file.file_date);
            if !when.is_empty() {
                return ("yours".into(), when);
            }
        }
        (old, new)
    }
}

/// The part of two file names that differs, widened to whole
/// version-looking pieces: "create-1.21.1-6.0.4.jar" and
/// "create-1.21.1-6.0.6.jar" give ("6.0.4", "6.0.6"). Names with nothing
/// in common come back whole, minus the extension.
pub fn version_change(from: &str, to: &str) -> (String, String) {
    fn stem(s: &str) -> &str {
        s.strip_suffix(".jar")
            .or_else(|| s.strip_suffix(".zip"))
            .unwrap_or(s)
    }
    let (a, b) = (stem(from), stem(to));
    let sep = |c: char| matches!(c, '-' | '_' | '+' | ' ');
    // Common prefix, backed up to just after a separator.
    let mut pre = a
        .char_indices()
        .zip(b.chars())
        .take_while(|((_, x), y)| x == y)
        .map(|((i, x), _)| i + x.len_utf8())
        .last()
        .unwrap_or(0);
    while pre > 0 && !a[..pre].ends_with(sep) {
        pre -= a[..pre].chars().next_back().map_or(1, char::len_utf8);
    }
    // Common suffix (after the prefix), moved to just before a separator.
    let (ra, rb) = (&a[pre..], &b[pre.min(b.len())..]);
    let mut suf = ra
        .chars()
        .rev()
        .zip(rb.chars().rev())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum::<usize>();
    while suf > 0 && !ra[ra.len() - suf..].starts_with(sep) {
        suf -= ra[ra.len() - suf..].chars().next().map_or(1, char::len_utf8);
    }
    let (x, y) = (&ra[..ra.len() - suf], &rb[..rb.len() - suf]);
    if x.is_empty() || y.is_empty() {
        return (a.to_string(), b.to_string());
    }
    (x.to_string(), y.to_string())
}

/// Newer files for installed projects that fit the instance.
pub fn check_updates(inst: &Instance) -> anyhow::Result<Vec<Update>> {
    let ids: Vec<u64> = inst
        .addons
        .iter()
        .filter(|a| a.project_id != 0 && !crate::modrinth::is_mr(a.project_id))
        .map(|a| a.project_id)
        .collect();
    let mods = cf::get_mods(&ids)?;
    let loader = loader_of(inst).api_id();
    let mut newest: HashMap<u64, u64> = HashMap::new();
    for m in &mods {
        let Some(a) = inst
            .addons
            .iter()
            .find(|a| a.project_id == m.id && !a.locked)
        else {
            continue;
        };
        let best = newer_file_id(inst, a, m, loader);
        if let Some(b) = best
            && b > a.file_id
        {
            newest.insert(m.id, b);
        }
    }
    let files = cf::get_files(&newest.values().copied().collect::<Vec<_>>())?;
    let mut out: Vec<Update> = files
        .into_iter()
        .filter_map(|f| {
            let a = inst.addons.iter().find(|a| a.project_id == f.mod_id)?;
            Some(Update {
                project_id: f.mod_id,
                name: a.name.clone(),
                from: a.file_name.clone(),
                file: f,
            })
        })
        .collect();
    // Modrinth failing (a timeout, say) leaves CurseForge's updates up
    // rather than none; with none to show, its error shows instead.
    match modrinth_updates(inst) {
        Ok(mr) => out.extend(mr),
        Err(e) if !out.is_empty() => log::warn!("Modrinth updates for {}: {e:#}", inst.name),
        Err(e) => return Err(e),
    }
    Ok(out)
}

/// The newest CurseForge file of `m` for this instance, from its file
/// index: right Minecraft version and loader, as stable as the channel
/// allows (or as the file already installed). Bigger than `a.file_id`
/// means an update.
pub fn newer_file_id(inst: &Instance, a: &Addon, m: &cf::Mod, loader: Option<u32>) -> Option<u64> {
    let mod_class = m.class_id.unwrap_or(cf::CLASS_MODS) == cf::CLASS_MODS;
    m.latest_files_indexes
        .iter()
        .filter(|ix| ix.game_version == inst.game_version)
        .filter(|ix| {
            !mod_class || loader.is_none() || ix.mod_loader.is_none() || ix.mod_loader == loader
        })
        .filter(|ix| ix.release_type <= inst.settings.release_channel.max(a.release_type).max(1))
        .map(|ix| ix.file_id)
        .max()
}

/// Newer Modrinth versions for Modrinth-sourced files, by file hash.
fn modrinth_updates(inst: &Instance) -> anyhow::Result<Vec<Update>> {
    let mr: Vec<&crate::instances::Addon> = inst
        .addons
        .iter()
        .filter(|a| !a.mr_sha1.is_empty() && !a.locked)
        .collect();
    if mr.is_empty() {
        return Ok(Vec::new());
    }
    let hashes: Vec<String> = mr.iter().map(|a| a.mr_sha1.clone()).collect();
    // One retry: a connection that times out once usually goes through.
    let latest = crate::modrinth::latest_by_sha1(&hashes, &inst.game_version, loader_of(inst))
        .or_else(|_| crate::modrinth::latest_by_sha1(&hashes, &inst.game_version, loader_of(inst)))?;
    let channel = inst.settings.release_channel;
    Ok(mr
        .into_iter()
        .filter_map(|a| {
            let v = latest.get(&a.mr_sha1)?;
            let f = crate::modrinth::version_to_file(v);
            (v.id != a.mr_version && f.release_type <= channel.max(a.release_type).max(1)).then(
                || Update {
                    project_id: a.project_id,
                    name: a.name.clone(),
                    from: a.file_name.clone(),
                    file: f,
                },
            )
        })
        .collect())
}

/// Returns the download pages of files whose authors allow website
/// downloads only, for the user to open.
pub fn apply_updates(inst: &mut Instance, updates: &[Update], job: &Job) -> anyhow::Result<Vec<String>> {
    apply_updates_with(inst, updates, job, &|_, _| {})
}

/// [`apply_updates`], telling `on` how each project goes:
/// `(project, Some(0..=1))` as its file downloads, `(project, None)` once
/// it's in place.
pub fn apply_updates_with(
    inst: &mut Instance,
    updates: &[Update],
    job: &Job,
    on: &(dyn Fn(u64, Option<f32>) + Sync),
) -> anyhow::Result<Vec<String>> {
    let todo: Vec<(cf::Mod, &Update, bool)> = updates
        .iter()
        .filter_map(|u| {
            let a = inst.addons.iter().find(|a| a.project_id == u.project_id)?;
            Some((addon_as_mod(a), u, a.from_pack))
        })
        .collect();
    // Downloads run side by side; the instance's records change one at a
    // time after. One file shows its bytes, several count files.
    let root = inst.root.clone();
    let one = todo.len() == 1;
    let got = crate::modpack::parallel(&todo, job, |_, (m, u, from_pack)| {
        let total = u.file.file_length.max(1) as f32;
        on(u.project_id, Some(0.0));
        fetch_file(&root, m, &u.file, *from_pack, job, one, &|n| {
            on(u.project_id, Some((n as f32 / total).min(1.0)))
        })
    });
    let mut pages = Vec::new();
    for ((m, u, from_pack), r) in todo.iter().zip(got) {
        // A disabled mod stays disabled (see `keep_history`).
        let r = r.and_then(|f| match f {
            Fetched::File(dest) => place_file(inst, m, &u.file, *from_pack, &dest).map(|_| true),
            // Hopper's download window gets it; no note to act on.
            Fetched::Pending(p) => {
                pages.push(p.url.clone());
                crate::modpack::add_pending(&inst.root, &[p]).map(|_| false)
            }
        });
        match r {
            Ok(true) => on(u.project_id, None),
            Ok(false) => {}
            Err(e) => {
                if !job.is_cancelled() {
                    job.note(format!("{}: {e:#}", u.name));
                }
            }
        }
    }
    // Keep what was done before a cancel.
    inst.save()?;
    job.check_cancelled()?;
    Ok(pages)
}

/// Installs one specific file of a project (CurseForge or Modrinth),
/// keeping its pack and enabled state. Returns the file name.
pub fn install_file(
    inst: &mut Instance,
    project_id: u64,
    file_id: u64,
    job: &Job,
) -> anyhow::Result<String> {
    let f = crate::source::file(project_id, file_id)?;
    let known = inst.addons.iter().find(|a| a.project_id == project_id);
    let (m, from_pack) = match known {
        Some(a) => (addon_as_mod(a), a.from_pack),
        None => (crate::source::get_mod(project_id)?, false),
    };
    // Keeps a disabled mod disabled (see `keep_history`).
    put_file(inst, &m, &f, from_pack, job)?;
    inst.save()?;
    Ok(f.file_name)
}

/// The project as `put_file` needs it, from what the instance already knows.
fn addon_as_mod(a: &crate::instances::Addon) -> cf::Mod {
    cf::Mod {
        id: a.project_id,
        name: a.name.clone(),
        class_id: Some(if a.class_id == 0 {
            cf::CLASS_MODS
        } else {
            a.class_id
        }),
        logo: Some(cf::Logo {
            thumbnail_url: a.icon.clone(),
            url: a.icon.clone(),
        }),
        authors: vec![cf::Author {
            name: a.author.clone(),
            ..Default::default()
        }],
        links: cf::Links {
            website_url: a.page.clone(),
            ..Default::default()
        },
        modrinth: (!a.mr_project.is_empty()).then(|| a.mr_project.clone()),
        ..Default::default()
    }
}

/// Two or more enabled jars in `mods/` that are one mod, whatever their
/// versions. The game refuses to start with both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Duplicate {
    pub name: String,
    /// Newest file first (the one to keep).
    pub files: Vec<String>,
}

/// Enabled jars that are the same CurseForge/Modrinth project or declare
/// the same mod id. Files already queued to leave ([`crate::locked`]) don't
/// count. Reads every jar: run it off the UI thread.
pub fn duplicates(inst: &Instance) -> Vec<Duplicate> {
    let skip = crate::locked::queued_mods(&inst.root);
    let dir = inst.mods_dir();
    let Ok(rd) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut jars: Vec<(String, std::time::SystemTime)> = rd
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            let l = n.to_lowercase();
            (l.ends_with(".jar") && !skip.contains(&l))
                .then(|| (n, e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH)))
        })
        .collect();
    jars.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    // Key ("p:<project>" or "m:<mod id>") -> jars, in newest-first order.
    let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
    let mut names: Vec<String> = Vec::with_capacity(jars.len());
    for (i, (file, _)) in jars.iter().enumerate() {
        let tracked = inst.addon_by_file(file);
        if let Some(a) = tracked.filter(|a| a.project_id != 0) {
            by_key.entry(format!("p:{}", a.project_id)).or_default().push(i);
        }
        let (mut ids, shown) = crate::deps::own_ids(&dir.join(file));
        names.push(match tracked {
            Some(a) => a.name.clone(),
            None if !shown.is_empty() => shown,
            None => file.clone(),
        });
        ids.sort();
        ids.dedup();
        for id in ids {
            by_key.entry(format!("m:{id}")).or_default().push(i);
        }
    }
    // Merge keys that share a jar, so one mod is one warning.
    let mut group: Vec<usize> = (0..jars.len()).collect();
    fn top(g: &mut [usize], mut i: usize) -> usize {
        while g[i] != i {
            g[i] = g[g[i]];
            i = g[i];
        }
        i
    }
    for list in by_key.values().filter(|l| l.len() > 1) {
        for &j in &list[1..] {
            let (a, b) = (top(&mut group, list[0]), top(&mut group, j));
            if a != b {
                group[a.max(b)] = a.min(b);
            }
        }
    }
    let mut out: Vec<Duplicate> = Vec::new();
    let mut at: HashMap<usize, usize> = HashMap::new();
    for i in 0..jars.len() {
        let g = top(&mut group, i);
        match at.get(&g) {
            Some(&k) => out[k].files.push(jars[i].0.clone()),
            None => {
                at.insert(g, out.len());
                out.push(Duplicate {
                    name: names[i].clone(),
                    files: vec![jars[i].0.clone()],
                });
            }
        }
    }
    out.retain(|d| d.files.len() > 1);
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// Jars in `mods/` that Hopper does not track yet.
pub fn loose_jars(inst: &Instance) -> Vec<String> {
    let Ok(rd) = fs::read_dir(inst.mods_dir()) else {
        return Vec::new();
    };
    let tracked: HashSet<String> = inst
        .addons
        .iter()
        .map(|a| a.file_name.to_lowercase())
        .chain(crate::locked::queued_mods(&inst.root))
        .collect();
    rd.flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            let l = n.to_lowercase();
            (l.ends_with(".jar") || l.ends_with(".jar.disabled"))
                && !tracked.contains(l.trim_end_matches(".disabled"))
        })
        .collect()
}

/// Identifies untracked jars by fingerprint (what CurseForge calls a scan)
/// and adds the matches. Returns how many were identified.
pub fn identify_loose(inst: &mut Instance, job: &Job) -> anyhow::Result<usize> {
    let loose = loose_jars(inst);
    let mut by_fp = HashMap::new();
    for (i, name) in loose.iter().enumerate() {
        job.progress(i, loose.len());
        job.detail(format!("Fingerprinting {name}"));
        let bytes = fs::read(inst.mods_dir().join(name))?;
        by_fp.insert(cf::fingerprint(&bytes), name.clone());
    }
    let matches = cf::match_fingerprints(&by_fp.keys().copied().collect::<Vec<_>>())?;
    let mods = cf::get_mods(&matches.iter().map(|m| m.id).collect::<Vec<_>>())?;
    let mut n = 0;
    for mt in matches {
        let Some(name) = by_fp.get(&(mt.file.file_fingerprint as u32)) else {
            continue;
        };
        let Some(m) = mods.iter().find(|m| m.id == mt.id) else {
            continue;
        };
        let mut a = addon_from(m, &mt.file, false);
        a.enabled = !name.ends_with(".disabled");
        a.file_name = name.trim_end_matches(".disabled").to_string();
        inst.addons.push(a);
        n += 1;
    }
    n += identify_on_modrinth(inst, &loose, job).unwrap_or_else(|e| {
        job.note(format!("Modrinth lookup failed: {e:#}"));
        0
    });
    let missing: Vec<&str> = loose
        .iter()
        .filter(|l| {
            !inst.addons.iter().any(|a| {
                a.file_name
                    .eq_ignore_ascii_case(l.trim_end_matches(".disabled"))
            })
        })
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        job.note(format!(
            "Not on CurseForge or Modrinth: {}",
            missing.join(", ")
        ));
    }
    inst.save()?;
    Ok(n)
}

/// One version's changelog.
#[derive(Clone, Debug)]
pub struct ChangelogEntry {
    pub name: String,
    pub date: String,
    pub release: &'static str,
    pub blocks: Vec<crate::richtext::Block>,
    /// The version the instance has now (always last).
    pub installed: bool,
}

/// Most versions to fetch for a compound changelog.
const MAX_CHANGELOGS: usize = 25;

/// Changelogs for every file after the installed one, up to `update`,
/// newest first, then the installed file's own changelog last. Also returns
/// every file that fits the instance, for picking another version.
pub fn changelogs(
    inst: &Instance,
    project_id: u64,
    installed_file: u64,
    update: Option<&cf::File>,
    is_pack: bool,
) -> anyhow::Result<(Vec<ChangelogEntry>, Vec<cf::File>)> {
    let class = inst
        .addons
        .iter()
        .find(|a| a.project_id == project_id)
        .map(|a| a.class_id);
    let loader = if !is_pack && class.unwrap_or(cf::CLASS_MODS) == cf::CLASS_MODS {
        loader_of(inst)
    } else {
        Loader::Any
    };
    let listed = crate::source::files(project_id, &inst.game_version, loader)?;
    let mut between: Vec<cf::File> = match update {
        Some(u) => listed
            .iter()
            .filter(|f| f.id > installed_file && f.id <= u.id)
            .cloned()
            .collect(),
        None => Vec::new(),
    };
    if let Some(u) = update
        && !between.iter().any(|f| f.id == u.id)
    {
        between.push(u.clone());
    }
    between.sort_by(|a, b| b.file_date.cmp(&a.file_date));
    between.truncate(MAX_CHANGELOGS);
    let current = match listed.iter().find(|f| f.id == installed_file) {
        Some(f) => Some(f.clone()),
        None => crate::source::file(project_id, installed_file).ok(),
    };
    let n_new = between.len();
    between.extend(current);

    let fetched: Vec<anyhow::Result<Vec<crate::richtext::Block>>> = std::thread::scope(|s| {
        let handles: Vec<_> = between
            .iter()
            .map(|f| s.spawn(move || crate::source::changelog(f)))
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("changelog thread panicked")))
            })
            .collect()
    });
    let entries = between
        .iter()
        .zip(fetched)
        .enumerate()
        .map(|(i, (f, blocks))| {
            let mut blocks = blocks.unwrap_or_default();
            if blocks.is_empty() {
                blocks = crate::richtext::parse("<p><i>No changelog for this version.</i></p>");
            }
            ChangelogEntry {
                name: f.display_name.clone(),
                date: f.file_date.clone(),
                release: f.release_name(),
                blocks,
                installed: i >= n_new,
            }
        })
        .collect();
    Ok((entries, listed))
}

/// Identifies still-untracked jars on Modrinth by sha1.
fn identify_on_modrinth(inst: &mut Instance, names: &[String], job: &Job) -> anyhow::Result<usize> {
    use crate::modrinth;
    let left: Vec<&String> = names
        .iter()
        .filter(|n| {
            !inst.addons.iter().any(|a| {
                a.file_name
                    .eq_ignore_ascii_case(n.trim_end_matches(".disabled"))
            })
        })
        .collect();
    if left.is_empty() {
        return Ok(0);
    }
    job.detail(format!("Looking up {} jars on Modrinth", left.len()));
    let mut by_hash = HashMap::new();
    for name in &left {
        by_hash.insert(
            modrinth::sha1(&fs::read(inst.mods_dir().join(name))?),
            (*name).clone(),
        );
    }
    let hashes: Vec<String> = by_hash.keys().cloned().collect();
    let versions = modrinth::versions_by_sha1(&hashes)?;
    let project_ids: Vec<String> = versions.values().map(|v| v.project_id.clone()).collect();
    let projects = modrinth::projects(&project_ids)?;
    let teams: Vec<String> = projects.iter().map(|p| p.team.clone()).collect();
    let owners = modrinth::team_owners(&teams).unwrap_or_default();
    let mut n = 0;
    for (hash, v) in &versions {
        let (Some(name), Some(p)) = (
            by_hash.get(hash),
            projects.iter().find(|p| p.id == v.project_id),
        ) else {
            continue;
        };
        let m = modrinth::project_to_mod(p, owners.get(&p.team).map_or("", String::as_str));
        let mut f = modrinth::version_to_file(v);
        // The jar on disk is the one that matched, whatever its name.
        if let Some(mr) = f.modrinth.as_mut() {
            mr.sha1 = hash.clone();
        }
        let mut a = addon_from(&m, &f, false);
        a.env = modrinth::env(&p.client_side, &p.server_side).to_string();
        a.enabled = !name.ends_with(".disabled");
        a.file_name = name.trim_end_matches(".disabled").to_string();
        inst.addons.push(a);
        n += 1;
    }
    Ok(n)
}

/// Replaces an installed project with the same mod from the other site
/// (CurseForge ↔ Modrinth), found by slug or name. Returns the new name.
pub fn switch_source(inst: &mut Instance, file_name: &str, job: &Job) -> anyhow::Result<String> {
    let old = inst
        .addon_by_file(file_name)
        .context("no such mod")?
        .clone();
    let to_modrinth = old.mr_project.is_empty();
    let site = if to_modrinth {
        "Modrinth"
    } else {
        "CurseForge"
    };
    let norm = |s: &str| {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase()
    };
    let old_slug = old
        .page
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_lowercase();

    job.detail(format!("Looking for {} on {site}", old.name));
    let class = if old.class_id == 0 {
        cf::CLASS_MODS
    } else {
        old.class_id
    };
    let search = cf::Search {
        class_id: class,
        text: old.name.clone(),
        game_version: inst.game_version.clone(),
        loader: if class == cf::CLASS_MODS {
            loader_of(inst)
        } else {
            Loader::Any
        },
        ..Default::default()
    };
    let (hits, _) = crate::source::search(&search, to_modrinth)?;
    let m = hits
        .iter()
        .find(|m| !old_slug.is_empty() && m.slug.eq_ignore_ascii_case(&old_slug))
        .or_else(|| hits.iter().find(|m| norm(&m.name) == norm(&old.name)))
        .cloned()
        .with_context(|| {
            format!(
                "{} is not on {site} for {} {}",
                old.name, inst.game_version, inst.loader
            )
        })?;
    let f = best_file(m.id, m.class_id, inst)?.with_context(|| {
        format!(
            "{} on {site} has no file for {} {}",
            m.name, inst.game_version, inst.loader
        )
    })?;

    install(inst, &m, &f, job)?;
    // The old entry goes; its file too, unless the new one has the same name.
    if let Some(i) = inst
        .addons
        .iter()
        .position(|a| a.project_id == old.project_id)
    {
        let gone = inst.addons.remove(i);
        if !gone.file_name.eq_ignore_ascii_case(&f.file_name) {
            move_out(&inst.root, &inst.addon_path(&gone), false)?;
        }
    }
    if let Some(new) = inst.addons.iter_mut().find(|a| a.project_id == m.id) {
        new.from_pack = old.from_pack;
        new.locked = old.locked;
        new.cf_locked = old.cf_locked;
    }
    inst.save()?;
    Ok(format!(
        "{} now comes from {site} ({})",
        m.name, f.file_name
    ))
}

/// Saves which mods are disabled now as profile `name` (replacing one of
/// the same name).
pub fn save_profile(inst: &mut Instance, name: &str) -> anyhow::Result<()> {
    let disabled: Vec<String> = inst
        .addons
        .iter()
        .filter(|a| !a.enabled)
        .map(|a| a.file_name.clone())
        .collect();
    inst.mod_profiles.retain(|p| p.name != name);
    inst.mod_profiles.push(crate::instances::ModProfile {
        name: name.to_string(),
        disabled,
    });
    inst.save()
}

/// Enables and disables mods to match a profile. Returns how many changed.
pub fn apply_profile(inst: &mut Instance, name: &str) -> anyhow::Result<usize> {
    let p = inst
        .mod_profiles
        .iter()
        .find(|p| p.name == name)
        .context("no such profile")?
        .clone();
    let off: HashSet<String> = p.disabled.iter().map(|f| f.to_lowercase()).collect();
    let mut changed = 0;
    for i in 0..inst.addons.len() {
        let want = !off.contains(&inst.addons[i].file_name.to_lowercase());
        if inst.addons[i].enabled != want {
            set_enabled(inst, i, want)?;
            changed += 1;
        }
    }
    inst.save()?;
    Ok(changed)
}

#[cfg(test)]
mod version_change_tests {
    use super::version_change;

    fn vc(a: &str, b: &str) -> (String, String) {
        version_change(a, b)
    }

    #[test]
    fn picks_out_the_versions() {
        let s = |a: &str, b: &str| (a.to_string(), b.to_string());
        assert_eq!(vc("create-1.21.1-6.0.4.jar", "create-1.21.1-6.0.6.jar"), s("6.0.4", "6.0.6"));
        assert_eq!(
            vc("jei-1.21.1-neoforge-19.21.0.247.jar", "jei-1.21.1-neoforge-19.21.2.250.jar"),
            s("19.21.0.247", "19.21.2.250")
        );
        assert_eq!(
            vc("sodium-fabric-0.5.8+mc1.20.1.jar", "sodium-fabric-0.5.11+mc1.20.1.jar"),
            s("0.5.8", "0.5.11")
        );
        assert_eq!(vc("a.jar", "b.jar"), s("a", "b"));
        assert_eq!(vc("same.jar", "same.jar"), s("same", "same"));
    }
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn profiles_round_trip() {
        let root = std::env::temp_dir().join(format!("hopper-modprof-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut inst = crate::modpack::create(&root, "P", "1.21.1", "").unwrap();
        for n in ["a.jar", "b.jar"] {
            fs::write(inst.mods_dir().join(n), n).unwrap();
            inst.addons.push(crate::instances::Addon {
                project_id: 1,
                file_name: n.into(),
                enabled: true,
                ..Default::default()
            });
        }
        save_profile(&mut inst, "all on").unwrap();
        set_enabled(&mut inst, 1, false).unwrap();
        save_profile(&mut inst, "no b").unwrap();
        assert_eq!(apply_profile(&mut inst, "all on").unwrap(), 1);
        assert!(inst.mods_dir().join("b.jar").exists());
        assert_eq!(apply_profile(&mut inst, "no b").unwrap(), 1);
        assert!(inst.mods_dir().join("b.jar.disabled").exists());
        let _ = fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod mod_id_lookup {
    /// `cargo test --lib real_mod_id_lookup -- --ignored --nocapture` (network)
    #[test]
    #[ignore]
    fn real_mod_id_lookup() {
        let inst = crate::instances::Instance {
            game_version: "1.21.1".into(),
            loader: "neoforge-21.1.256".into(),
            ..Default::default()
        };
        for id in ["geckolib", "architectury", "kotlinforforge", "cloth_config", "curios", "notarealmod_xyz"] {
            let m = super::project_for_mod_id(&inst, id).unwrap();
            println!("{id} -> {:?}", m.map(|m| (m.name, m.slug, m.id)));
        }
    }

    /// Library ids that glue "core"/"lib" onto the name, which the slug
    /// splits off. `cargo test --lib glued_suffix_ids_resolve -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn glued_suffix_ids_resolve() {
        let inst = crate::instances::Instance {
            game_version: "1.21.1".into(),
            loader: "neoforge-21.1.256".into(),
            ..Default::default()
        };
        for (id, name) in [("cyclopscore", "cyclops core"), ("codechickenlib", "codechicken")] {
            let m = super::project_for_mod_id(&inst, id).unwrap().unwrap_or_else(|| panic!("{id} not found"));
            println!("{id} -> {} ({}, {})", m.name, m.slug, m.id);
            assert!(m.name.to_lowercase().contains(name), "{id} matched {}", m.name);
            let f = super::best_file(m.id, m.class_id, &inst).unwrap();
            let f = f.unwrap_or_else(|| panic!("{id}: no NeoForge 1.21.1 file"));
            println!("  file {}", f.file_name);
        }
    }

    #[test]
    fn splits_glued_suffixes() {
        use super::split_id_suffix as s;
        assert_eq!(s("cyclopscore"), Some(("cyclops", "core")));
        assert_eq!(s("codechickenlib"), Some(("codechicken", "lib")));
        assert_eq!(s("puzzleslib"), Some(("puzzles", "lib")));
        assert_eq!(s("ftblibrary"), Some(("ftb", "library")));
        assert_eq!(s("cloth_config"), Some(("cloth", "config")));
        // Too short to be a name on its own, or no known ending.
        assert_eq!(s("core"), None);
        assert_eq!(s("xlib"), None);
        assert_eq!(s("geckolib"), Some(("gecko", "lib")));
        assert_eq!(s("balm"), None);
    }

    #[test]
    fn slug_tries_include_split_form() {
        let t = super::slug_tries("cyclopscore");
        assert_eq!(t[0], "cyclopscore");
        assert!(t.contains(&"cyclops-core".to_string()));
        let t = super::slug_tries("codechickenlib");
        assert!(t.contains(&"codechicken-lib".to_string()));
        // The id itself stays first, and nothing repeats.
        let t = super::slug_tries("balm");
        assert_eq!(t, ["balm", "balm-api", "balm-lib"]);
        let t = super::slug_tries("cloth_config");
        assert_eq!(t[..2], ["cloth_config", "cloth-config"]);
        assert_eq!(t.iter().filter(|s| *s == "cloth-config").count(), 1);
    }

    #[test]
    fn hits_match_by_slug_name_or_versioned_slug() {
        use super::hit_is_mod_id as hit;
        assert!(hit("cyclopscore", "cyclops-core", "Cyclops Core"));
        assert!(hit("codechickenlib", "codechicken-lib-1-8", "CodeChicken Lib"));
        assert!(hit("codechickenlib", "codechicken-lib-1-8", "Something Else"));
        assert!(hit("balm", "balm", "Balm"));
        // Lookalikes don't count.
        assert!(!hit("codechickenlib", "codechickenlib-unofficial", "CodeChickenLib Unofficial 1.7.10"));
        assert!(!hit("cyclopscore", "cyclops-core-addon", "Cyclops Core Addon"));
        assert!(!hit("balm", "balmy", "Balmy"));
    }
}

#[cfg(test)]
mod locked_tests {
    #[test]
    fn explains_sharing_violations_only() {
        let locked = anyhow::Error::from(std::io::Error::from_raw_os_error(32)).context("renaming x.jar");
        assert!(format!("{:#}", super::explain_locked(locked)).starts_with("the game has this file open"));
        let other = anyhow::Error::from(std::io::Error::from_raw_os_error(5));
        assert!(!format!("{:#}", super::explain_locked(other)).contains("the game"));
    }
}

#[cfg(test)]
mod duplicate_tests {
    use std::io::Write;

    fn jar(path: &std::path::Path, mod_id: &str) {
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        z.start_file::<_, ()>("META-INF/neoforge.mods.toml", Default::default()).unwrap();
        write!(z, "[[mods]]\nmodId=\"{mod_id}\"\nversion=\"1\"\n").unwrap();
        z.finish().unwrap();
    }

    #[test]
    fn same_mod_id_or_project_is_a_duplicate() {
        let root = std::env::temp_dir().join(format!("hopper-dups-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mods = root.join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        jar(&mods.join("evil-1.2.98.jar"), "evilcraft");
        std::thread::sleep(std::time::Duration::from_millis(30));
        jar(&mods.join("evil-1.2.99.jar"), "evilcraft");
        jar(&mods.join("other.jar"), "other");
        // Different ids, same project.
        jar(&mods.join("a-old.jar"), "a_old_id");
        jar(&mods.join("a-new.jar"), "a_new_id");
        // Disabled copies don't load, so they don't count.
        jar(&mods.join("other-2.jar.disabled"), "other");
        let addon = |file: &str, pid| crate::instances::Addon {
            file_name: file.into(),
            name: "A".into(),
            project_id: pid,
            enabled: true,
            ..Default::default()
        };
        let mut inst = crate::instances::Instance {
            root: root.clone(),
            addons: vec![addon("a-old.jar", 7), addon("a-new.jar", 7)],
            ..Default::default()
        };
        let d = super::duplicates(&inst);
        assert_eq!(d.len(), 2, "{d:?}");
        let evil = d.iter().find(|x| x.files.iter().any(|f| f.starts_with("evil"))).unwrap();
        assert_eq!(evil.files, ["evil-1.2.99.jar", "evil-1.2.98.jar"]);
        // A file queued to leave is not counted.
        crate::locked::queue(&root, &mods.join("evil-1.2.98.jar"), false).unwrap();
        inst.addons.clear();
        assert!(super::duplicates(&inst).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod versions_tests {
    #[test]
    fn same_name_falls_back_to_the_date() {
        let u = super::Update {
            from: "Bare Bones x Mods.zip".into(),
            file: crate::cf::File {
                file_name: "Bare Bones x Mods.zip".into(),
                file_date: "2026-10-13T10:00:00Z".into(),
                ..Default::default()
            },
            project_id: 0,
            name: String::new(),
        };
        assert_eq!(u.versions(), ("yours".to_string(), "13/10/2026".to_string()));
    }
}
