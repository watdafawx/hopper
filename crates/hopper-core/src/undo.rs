//! Undo from an instance's History ([`crate::backups`]): puts a removed,
//! updated or edited file back as it was.
//!
//! A mod jar CurseForge or Modrinth knows comes back by downloading that
//! exact file again (hash-checked) with its old entry (name, on or off).
//! When the site no longer has that file, [`Gone`] says so and lists the
//! versions it does have, to put one of those back instead
//! ([`put_back_as`]). Anything else (unknown jars, configs) comes back from
//! the copy History kept; what's there now goes to History first, so the
//! undo can itself be undone.

use crate::backups::{Ref, Saved};
use crate::cf;
use crate::instances::{Addon, Instance};
use crate::jobs::Job;
use std::fs;
use std::path::{Path, PathBuf};

/// The file to put back is no longer on its site.
#[derive(Debug, Clone)]
pub struct Gone {
    pub name: String,
    pub version: String,
    /// "CurseForge", "Modrinth", "GitHub" or another link's kind.
    pub site: &'static str,
    /// What the site has for this instance's Minecraft and loader, newest
    /// first.
    pub choices: Vec<cf::File>,
    /// The choice closest to the one removed.
    pub closest: usize,
}

impl std::fmt::Display for Gone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Version {} is no longer on {}", self.version, self.site)
    }
}

impl std::error::Error for Gone {}

/// What Undo says when the file only the CurseForge website hands out
/// ([`cf::WebsiteOnly`]) has to be downloaded by hand. `picks_up`: Hopper
/// opens the download window and puts it back itself (the desktop app);
/// else the player adds it (a server panel).
pub fn website_message(w: &cf::WebsiteOnly, picks_up: bool) -> String {
    let what = format!("{} {}", w.name, version_of(&w.file_name));
    if picks_up {
        format!("{what} can only be downloaded from the CurseForge website. Download it there and Hopper puts it back.")
    } else if w.url.is_empty() {
        format!("{what} can only be downloaded from the CurseForge website. Download {} there, then add it to the server's mods.", w.file_name)
    } else {
        format!("{what} can only be downloaded from the CurseForge website. Download {} from {}, then add it to the server's mods.", w.file_name, w.url)
    }
}

/// The project and file a website-only note stands for, to put through the
/// same install as the mod page's Files tab: it opens the download window and
/// adds the file when it arrives.
pub fn website_file(r: &Ref) -> anyhow::Result<(cf::Mod, cf::File)> {
    let m = match &r.addon {
        Some(a) => crate::install::addon_as_mod(a),
        None => crate::source::get_mod(r.project_id)?,
    };
    Ok((m, crate::backups::remote_file(r)?))
}

fn site(r: &Ref) -> &'static str {
    crate::backups::site_of_ref(r)
}

/// The update that brought `file_name` (a jar in `mods/`, on or off): the
/// copy History kept of the file it replaced, and when, newest first. The
/// Mods tab's load-problem notice offers "Roll back" with it, which is
/// History's own Undo of that entry. Reads `.hopper-backups`: off the UI
/// thread.
pub fn update_that_brought(root: &Path, file_name: &str) -> Option<(i64, Saved)> {
    brought_by(&crate::backups::list(root), file_name)
}

/// [`update_that_brought`] over History's sets (newest first).
pub fn brought_by(sets: &[crate::backups::Set], file_name: &str) -> Option<(i64, Saved)> {
    let bare = |s: &str| s.trim_end_matches(".disabled").to_ascii_lowercase();
    let want = bare(file_name);
    sets.iter().find_map(|set| {
        set.files
            .iter()
            .find(|f| {
                f.change == crate::backups::Change::Updated
                    && f.record.is_none()
                    && f.restore_to.starts_with("mods/")
                    && f.now.as_deref().is_some_and(|n| bare(n) == want)
            })
            .map(|f| (set.when, f.clone()))
    })
}

/// The version a file name says, without the Minecraft version and loader
/// most names carry too ("jei-1.21.1-neoforge-19.21.0.247.jar" → "19.21.0.247").
pub fn version_of(file_name: &str) -> String {
    let base = file_name.strip_suffix(".disabled").unwrap_or(file_name);
    let base = base.strip_suffix(".jar").or_else(|| base.strip_suffix(".zip")).unwrap_or(base);
    let parts: Vec<&str> = base.split(['-', '_', '+', ' ']).filter(|p| !p.is_empty()).collect();
    let numeric = |p: &str| {
        let p = p.trim_start_matches(['v', 'V']);
        p.starts_with(|c: char| c.is_ascii_digit())
    };
    let loader = |p: &str| matches!(p.to_lowercase().as_str(), "neoforge" | "forge" | "fabric" | "quilt" | "mc");
    // "1.21.1", "mc1.21": the game's version, not the mod's.
    let game = |p: &str| {
        let p = p.to_lowercase();
        let p = p.strip_prefix("mc").unwrap_or(&p);
        let mut it = p.split('.');
        it.next() == Some("1") && it.next().is_some_and(|m| m.parse::<u32>().is_ok_and(|m| (7..=30).contains(&m))) && it.all(|x| x.parse::<u32>().is_ok())
    };
    let start = parts.iter().position(|p| numeric(p) || p.to_lowercase().starts_with("mc1"));
    let Some(start) = start else { return base.to_string() };
    let tail: Vec<&str> = parts[start..].iter().copied().filter(|p| !loader(p)).collect();
    let own: Vec<&str> = tail.iter().copied().filter(|p| !game(p)).collect();
    match (own.is_empty(), tail.is_empty()) {
        (false, _) => own.join("-"),
        (true, false) => tail.join("-"),
        _ => base.to_string(),
    }
}

type Choices = fn(&Ref) -> anyhow::Result<Vec<cf::File>>;

thread_local! {
    // Tests list a project's files without going online.
    static CHOICES: std::cell::Cell<Option<Choices>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_test_choices(f: Choices) {
    CHOICES.with(|h| h.set(Some(f)));
}

/// The files the site has for `r`'s project for this instance, newest first.
pub fn choices(inst: &Instance, r: &Ref) -> anyhow::Result<Vec<cf::File>> {
    if let Some(f) = CHOICES.with(|h| h.get()) {
        return f(r);
    }
    anyhow::ensure!(!cfg!(test), "offline in tests");
    let loader = crate::install::loader_of(inst);
    let mut files = if !r.mr_version.is_empty() && !r.mr_project.is_empty() {
        crate::modrinth::versions(&r.mr_project, &inst.game_version, loader)?
            .iter()
            .map(crate::modrinth::version_to_file)
            .collect()
    } else {
        crate::source::files(r.project_id, &inst.game_version, loader)?
    };
    files.sort_by(|a, b| b.file_date.cmp(&a.file_date));
    Ok(files)
}

/// Which of `files` (newest first) is closest to the one `r` was: the
/// newest not newer than it, else the oldest.
fn closest(r: &Ref, files: &[cf::File]) -> usize {
    let date = r.addon.as_ref().map(|a| a.file_date.as_str()).unwrap_or("");
    if !date.is_empty() {
        if let Some(i) = files.iter().position(|f| f.file_date.as_str() <= date) {
            return i;
        }
        return files.len().saturating_sub(1);
    }
    if r.mr_version.is_empty() && !crate::sources::is_src(r.project_id) {
        // CurseForge file ids grow over time; a GitHub / link file's are hashes.
        if let Some(i) = files.iter().position(|f| f.id <= r.file_id) {
            return i;
        }
    }
    0
}

/// Gets `r`'s exact file to `dest`: from History's copy when it kept one,
/// else downloaded (hash-checked). When the site no longer has it, the
/// error is a [`Gone`]; when only the CurseForge website hands it out, a
/// [`cf::WebsiteOnly`].
fn fetch(inst: &Instance, f: &Saved, r: &Ref, dest: &Path) -> anyhow::Result<()> {
    if f.path.is_file() {
        if fs::rename(&f.path, dest).is_err() {
            fs::copy(&f.path, dest)?;
        }
        return Ok(());
    }
    let (_, fetch) = crate::backups::hooks();
    let Err(e) = fetch(r, dest) else { return Ok(()) };
    let _ = fs::remove_file(dest);
    // Only the CurseForge website has this file (the author turned off
    // downloads elsewhere): not gone, and not a failure to retry. The caller
    // gets the typed error and opens the download window for it.
    if e.downcast_ref::<cf::WebsiteOnly>().is_some() {
        return Err(e);
    }
    // Gone, or just offline? Gone only when the site lists the project's
    // files and this one isn't among them.
    match choices(inst, r) {
        Ok(files) if !files.iter().any(|x| x.id == r.file_id) => {
            let closest = closest(r, &files);
            Err(Gone {
                name: if r.name.is_empty() { r.file_name.clone() } else { r.name.clone() },
                version: version_of(&r.file_name),
                site: site(r),
                choices: files,
                closest,
            }
            .into())
        }
        _ => Err(e.context(format!("downloading {} again from {}", r.file_name, site(r)))),
    }
}

/// The entry `r` had, else one made from what History knows.
fn entry_of(r: &Ref) -> Addon {
    let mut a = r.addon.clone().unwrap_or_else(|| Addon {
        project_id: r.project_id,
        file_id: r.file_id,
        name: if r.name.is_empty() { r.file_name.clone() } else { r.name.clone() },
        enabled: true,
        mr_project: r.mr_project.clone(),
        mr_version: r.mr_version.clone(),
        date_added: crate::time::now_iso(),
        ..Default::default()
    });
    a.file_name = r.file_name.clone();
    a.file_id = r.file_id;
    a
}

/// Undoes one History entry in `inst`; returns where the file went back.
/// The caller saves nothing: the instance is saved here.
pub fn undo(inst: &mut Instance, f: &Saved) -> anyhow::Result<PathBuf> {
    if let Some((did, key)) = &f.record {
        return undo_record(inst, f, *did, key);
    }
    // Putting it back isn't a new change for History to note.
    let _quiet = crate::actor::quiet(&inst.root);
    let dest = inst.root.join(&f.restore_to);
    let jar = f.restore_to.to_lowercase().trim_end_matches(".disabled").ends_with(".jar");
    match (&f.src, jar) {
        (Some(r), true) => {
            let on = !f.restore_to.ends_with(".disabled");
            let tmp = dest.with_file_name(format!("{}.hopper-tmp", r.file_name));
            if let Some(p) = tmp.parent() {
                fs::create_dir_all(p)?;
            }
            fetch(inst, f, r, &tmp)?;
            put_entry(inst, entry_of(r), &tmp, on)?;
            forget(f);
            Ok(dest)
        }
        _ => {
            crate::backups::materialize(&f.path)?;
            // What's there now goes to History, so this undo can be undone.
            if dest.exists() {
                crate::sync::backup_into(&inst.root, &dest)?;
            }
            if let Some(p) = dest.parent() {
                fs::create_dir_all(p)?;
            }
            if fs::rename(&f.path, &dest).is_err() {
                fs::copy(&f.path, &dest)?;
            }
            forget(f);
            Ok(dest)
        }
    }
}

/// Undoes a record-only entry: removes a mod that was added (it goes to
/// History like any removal), or switches it back. A mod no longer there
/// only drops the entry.
fn undo_record(inst: &mut Instance, f: &Saved, did: crate::actor::Did, key: &str) -> anyhow::Result<PathBuf> {
    use crate::actor::Did;
    if matches!(did, Did::Moved | Did::Copied) {
        anyhow::bail!("A move or copy of the whole server can't be undone from History.");
    }
    if matches!(did, Did::Restored | Did::BackupRemoved) {
        anyhow::bail!("Backup restores and removals can't be undone from History; the copy is kept under .hopper-backups.");
    }
    let _quiet = crate::actor::quiet(&inst.root);
    let at = inst
        .addons
        .iter()
        .position(|a| crate::groupsync::addon_key(a) == key)
        .or_else(|| inst.addons.iter().position(|a| a.file_name.trim_end_matches(".disabled").eq_ignore_ascii_case(&f.name)));
    match (did, at) {
        (Did::Added, Some(i)) => crate::install::remove(inst, i)?,
        (Did::On, Some(i)) => crate::install::set_enabled(inst, i, false)?,
        (Did::Off, Some(i)) => crate::install::set_enabled(inst, i, true)?,
        (Did::Moved | Did::Copied | Did::Restored | Did::BackupRemoved, _) | (_, None) => {}
    }
    if let Some(dir) = f.path.parent() {
        crate::actor::forget_record(dir, key, did);
    }
    Ok(inst.root.join(&f.restore_to))
}

/// Puts `new` (its file downloaded to `tmp`) in place of whatever file of
/// the same project the instance has now, on or off as `on` says.
fn put_entry(inst: &mut Instance, new: Addon, tmp: &Path, on: bool) -> anyhow::Result<()> {
    // An old copy that was off, put back while the mod's copy that's on is
    // there: it was removed on its own (the "Old copy" chip), not replaced
    // by an update, so the copy that's on stays and this one goes beside it.
    let beside = !on && !inst.addons.iter().any(|a| a.file_name.eq_ignore_ascii_case(&new.file_name))
        && inst.addons.iter().any(|a| a.project_id == new.project_id && a.enabled);
    // The same project, or the same file tracked from the other site (a
    // note made from a copy can name either): replaced, never doubled.
    let old = if beside {
        None
    } else {
        inst.addons
            .iter()
            .position(|a| a.project_id == new.project_id)
            .or_else(|| inst.addons.iter().position(|a| a.file_name.eq_ignore_ascii_case(&new.file_name)))
            .map(|i| inst.addons.remove(i))
    };
    let mut new = new;
    if old.is_none() {
        new.enabled = on;
    }
    let (pid, file) = (new.project_id, new.file_name.clone());
    if let Err(e) = crate::install::replace_addon(inst, old, new, tmp) {
        let _ = fs::remove_file(tmp);
        return Err(e);
    }
    if let Some(i) = inst.addons.iter().position(|a| a.project_id == pid && (!beside || a.file_name.eq_ignore_ascii_case(&file)))
        && inst.addons[i].enabled != on
    {
        crate::install::set_enabled(inst, i, on)?;
    }
    inst.save()
}

/// Puts `file` (one of [`Gone::choices`]) back for entry `f`, whose own
/// version is gone.
pub fn put_back_as(inst: &mut Instance, f: &Saved, file: &cf::File, job: Option<&Job>) -> anyhow::Result<PathBuf> {
    let r = f.src.as_ref().ok_or_else(|| anyhow::anyhow!("{}: History doesn't know where it came from", f.name))?;
    let on = !f.restore_to.ends_with(".disabled");
    let mut new = entry_of(r);
    let m = match &r.addon {
        Some(a) => crate::install::addon_as_mod(a),
        None => crate::source::get_mod(r.project_id)?,
    };
    let dest = inst.root.join(&f.restore_to).with_file_name(format!("{}.hopper-tmp", file.file_name));
    let url = cf::download_url(file)?;
    cf::download_checked(&url, &dest, &file.expect(), job, |_| {})?;
    // The picked file, with the old entry's history.
    let fresh = crate::install::addon_from(&m, file, new.from_pack);
    new = Addon { file_id: fresh.file_id, file_name: fresh.file_name, file_date: fresh.file_date, release_type: fresh.release_type, mr_version: fresh.mr_version, mr_sha1: fresh.mr_sha1, requires: fresh.requires, ..new };
    put_entry(inst, new, &dest, on)?;
    forget(f);
    Ok(inst.root.join(&f.restore_to).with_file_name(&file.file_name))
}

/// Drops an entry's file (note or copy) from History once it's undone; an
/// emptied folder goes too.
fn forget(f: &Saved) {
    let _ = fs::remove_file(&f.path);
    let _ = fs::remove_file(crate::backups::ref_path(&f.path));
    if let Some(dir) = f.path.parent() {
        crate::backups::drop_if_empty(dir);
    }
}

/// What [`restore_pack`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackRestore {
    /// The record is what it was before the update. `version`: the pack
    /// version it names again (empty when History doesn't know it).
    Restored { version: String },
    /// Another update came after this one: the record stays. `version`: what
    /// it names now (empty when unknown).
    Moved { version: String },
    /// The entry was made before Hopper kept the old record: nothing to put
    /// back.
    Unknown,
}

impl PackRestore {
    /// How Hopper Server tells it (`POST /servers/{id}/mod-backups/undo`).
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            PackRestore::Restored { version } => serde_json::json!({ "state": "restored", "version": version }),
            PackRestore::Moved { version } => serde_json::json!({ "state": "moved", "version": version }),
            PackRestore::Unknown => serde_json::json!({ "state": "unknown" }),
        }
    }

    /// Reads [`PackRestore::to_json`] back; none for a panel that doesn't
    /// say (an older one, or the entry wasn't undone whole).
    pub fn from_json(v: &serde_json::Value) -> Option<PackRestore> {
        let version = v["version"].as_str().unwrap_or_default().to_string();
        match v["state"].as_str()? {
            "restored" => Some(PackRestore::Restored { version }),
            "moved" => Some(PackRestore::Moved { version }),
            _ => None,
        }
    }
}

/// The pack version a record shows to the player.
fn shown_version(p: &crate::instances::PackRef) -> String {
    if !p.version.is_empty() {
        p.version.clone()
    } else {
        version_of(&p.file_name)
    }
}

/// After Undo all put back everything a pack update changed (mods, configs):
/// also puts back the pack record the update replaced (pack version and
/// file, loader and its version, Minecraft version; a server's pack record
/// and link), so the header names the old version and the update is offered
/// again. Only for the whole entry: one mod put back is no reason to move
/// the pack. When the pack was updated again since (the record no longer
/// names the file `m` went to), nothing is changed. What the undo set aside
/// from `since` (unix seconds) is marked as the update put back, which can
/// itself be undone.
pub fn restore_pack(inst: &mut Instance, m: &crate::backups::PackMark, since: u64) -> anyhow::Result<PackRestore> {
    use crate::backups::{PackBefore, PackMark};
    use crate::server_pack_update as spu;
    let Some(b) = &m.before else { return Ok(PackRestore::Unknown) };
    let root = inst.root.clone();
    let server = b.server.is_some();
    let now = if server { spu::recorded(&root).map_or(0, |r| r.pack_file_id) } else { inst.pack.as_ref().map_or(0, |p| p.file_id) };
    if m.file_id != 0 && now != m.file_id {
        return Ok(PackRestore::Moved { version: inst.pack.as_ref().map(shown_version).unwrap_or_default() });
    }
    // What this undo replaces, to put back if the undo is undone.
    let was = if server { PackBefore::of_server(&root) } else { PackBefore::of_instance(inst) };
    if let Some(p) = &b.pack {
        inst.pack = Some(p.clone());
    }
    if !b.loader.is_empty() {
        inst.loader = b.loader.clone();
    }
    if !b.game_version.is_empty() {
        inst.game_version = b.game_version.clone();
    }
    // A server without a hopper.json has none made for it here.
    if !server || root.join(crate::instances::HOPPER_FILE).is_file() {
        inst.save()?;
    }
    if let Some(mk) = &b.server {
        if *mk == spu::Marker::default() {
            let _ = fs::remove_file(root.join(spu::MARKER));
        } else {
            spu::record(&root, mk)?;
        }
    }
    if let Some(l) = &b.link {
        crate::hosting::pack_link::write(&root, l);
    }
    if !server {
        // CurseForge's own file names the new version too: told, unless
        // CurseForge is open (Settings has "Update CurseForge" for that).
        if let Err(e) = crate::cf_instance::write_pack(inst) {
            log::warn!("{}: CurseForge's pack version after Undo: {e:#}", inst.name);
        }
        if let Err(e) = crate::cf_instance::write_loader(inst) {
            log::warn!("{}: CurseForge's loader after Undo: {e:#}", inst.name);
        }
        let _ = inst.save();
    }
    let version = b.pack.as_ref().map(shown_version).unwrap_or_default();
    let file_id = match &b.server {
        Some(mk) => mk.pack_file_id,
        None => b.pack.as_ref().map_or(0, |p| p.file_id),
    };
    crate::backups::mark_pack(
        &root,
        since,
        &PackMark { name: m.name.clone(), version: version.clone(), before: Some(was), file_id, undone: true },
    );
    Ok(PackRestore::Restored { version })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(name: &str, change: crate::backups::Change, now: Option<&str>) -> Saved {
        Saved {
            path: PathBuf::from(format!("C:/b/{name}")),
            name: name.into(),
            size: 1,
            restore_to: format!("mods/{name}"),
            download: false,
            src: None,
            change,
            now: now.map(str::to_string),
            now_site: "",
            reason: String::new(),
            who: None,
            batch: String::new(),
            put_back: false,
            record: None,
        }
    }

    fn set(when: i64, files: Vec<Saved>) -> crate::backups::Set {
        crate::backups::Set { dir: PathBuf::from(format!("C:/b/{when}")), when, files, pack: None }
    }

    /// "Roll back" on the load-problem notice: the newest update that put the
    /// current file in, whether the mod is on or off now.
    #[test]
    fn the_update_that_brought_a_file() {
        use crate::backups::Change;
        let sets = vec![
            set(300, vec![saved("other-1.0.jar", Change::Updated, Some("other-2.0.jar"))]),
            set(200, vec![saved("towers-1.2.3.jar", Change::Updated, Some("towers-1.3.0.jar"))]),
            set(100, vec![saved("towers-1.1.0.jar", Change::Updated, Some("towers-1.3.0.jar"))]),
        ];
        let (when, f) = brought_by(&sets, "towers-1.3.0.jar").unwrap();
        assert_eq!((when, f.name.as_str()), (200, "towers-1.2.3.jar"));
        assert_eq!(brought_by(&sets, "Towers-1.3.0.jar.disabled").unwrap().0, 200);
        // No update brought it: no roll back.
        assert!(brought_by(&sets, "lonely-1.0.jar").is_none());
        // A removal or a switch isn't the update that brought it.
        let sets = vec![
            set(200, vec![saved("towers-1.2.3.jar", Change::Removed, None)]),
            set(100, vec![saved("towers-1.2.jar", Change::Switched, Some("towers-1.3.0.jar"))]),
        ];
        assert!(brought_by(&sets, "towers-1.3.0.jar").is_none());
    }

    #[test]
    fn versions_from_file_names() {
        assert_eq!(version_of("jei-1.21.1-neoforge-19.21.0.247.jar"), "19.21.0.247");
        assert_eq!(version_of("appleskin-neoforge-mc1.21-3.0.9.jar"), "3.0.9");
        assert_eq!(version_of("JadeAddons-1.21.1-NeoForge-6.1.2.jar"), "6.1.2");
        assert_eq!(version_of("effortlessbuilding-4.2.37+neoforge-1.21.1.jar"), "4.2.37");
        assert_eq!(version_of("forgified-fabric-api-0.116.15+2.3.6+1.21.1.jar"), "0.116.15-2.3.6");
        assert_eq!(version_of("create-1.21.1.jar"), "1.21.1");
        assert_eq!(version_of("ExtendedAE-Plus-1.6.3.jar.disabled"), "1.6.3");
        assert_eq!(version_of("Jade_15.10.4.jar"), "15.10.4");
        assert_eq!(version_of("nothing.jar"), "nothing");
    }

    /// Undoing the removal of an old copy left off (TNP-LL8's doubles, 2026-11)
    /// puts it back beside the copy that's on; undoing an update of a mod
    /// that's off still swaps the files.
    #[test]
    fn an_old_copy_goes_back_beside_the_one_on() {
        let dir = std::env::temp_dir().join(format!("hopper-undo-beside-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("mods")).unwrap();
        let addon = |f: &str, on: bool| Addon { file_name: f.into(), project_id: 7, file_id: 1, enabled: on, class_id: cf::CLASS_MODS, ..Default::default() };
        let mut inst = Instance { root: dir.clone(), ..Default::default() };
        fs::write(dir.join("mods/x-2.jar"), b"new").unwrap();
        inst.addons = vec![addon("x-2.jar", true)];
        let tmp = dir.join("mods/x-1.jar.hopper-tmp");
        fs::write(&tmp, b"old").unwrap();
        put_entry(&mut inst, addon("x-1.jar", false), &tmp, false).unwrap();
        assert!(dir.join("mods/x-2.jar").is_file(), "the copy that's on stays");
        assert!(dir.join("mods/x-1.jar.disabled").is_file(), "the old copy is back, off");
        assert_eq!(inst.addons.len(), 2);

        // Both off: an update of a turned-off mod being undone.
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("mods")).unwrap();
        fs::write(dir.join("mods/x-2.jar.disabled"), b"new").unwrap();
        inst.addons = vec![addon("x-2.jar", false)];
        fs::write(&tmp, b"old").unwrap();
        put_entry(&mut inst, addon("x-1.jar", false), &tmp, false).unwrap();
        assert!(!dir.join("mods/x-2.jar.disabled").exists());
        assert!(dir.join("mods/x-1.jar.disabled").is_file());
        assert_eq!(inst.addons.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    fn file(id: u64, date: &str) -> cf::File {
        cf::File { id, file_date: date.into(), ..Default::default() }
    }

    fn pack_ref(file_id: u64, version: &str) -> crate::instances::PackRef {
        crate::instances::PackRef { project_id: 77, file_id, name: "TNP Limitless 8".into(), version: version.into(), ..Default::default() }
    }

    /// An instance on pack version 1.81.0 (file 200) that was on 1.80.0
    /// (file 100) before the update, with the mark the update left.
    fn updated_instance(tag: &str) -> (Instance, crate::backups::PackMark, PathBuf) {
        use crate::backups::{PackBefore, PackMark};
        let dir = std::env::temp_dir().join(format!("hopper-undo-pack-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        // One set-aside config, so there is a History folder to mark.
        let cfg = dir.join(crate::sync::BACKUP_DIR).join("5000").join("config");
        fs::create_dir_all(&cfg).unwrap();
        fs::write(cfg.join("a.toml"), "x").unwrap();
        let mut inst = Instance {
            name: "TNP".into(),
            root: dir.clone(),
            game_version: "1.21.1".into(),
            loader: "neoforge-21.1.256".into(),
            pack: Some(pack_ref(100, "1.80.0")),
            ..Default::default()
        };
        let before = PackBefore::of_instance(&inst);
        inst.pack = Some(pack_ref(200, "1.81.0"));
        inst.loader = "neoforge-21.1.300".into();
        inst.game_version = "1.21.4".into();
        inst.save().unwrap();
        let mark = PackMark { before: Some(before), file_id: 200, ..PackMark::new("TNP Limitless 8".into(), "1.81.0".into()) };
        (inst, mark, dir)
    }

    #[test]
    fn undo_all_puts_the_pack_record_back() {
        let (mut inst, mark, dir) = updated_instance("back");
        let r = restore_pack(&mut inst, &mark, 5000).unwrap();
        assert_eq!(r, PackRestore::Restored { version: "1.80.0".into() });
        // On disk, not only in memory: the header and the update offer read it.
        let now = crate::instances::load(&dir).unwrap();
        let p = now.pack.unwrap();
        assert_eq!((p.file_id, p.version.as_str(), p.project_id), (100, "1.80.0", 77));
        assert_eq!(now.loader, "neoforge-21.1.256");
        assert_eq!(now.game_version, "1.21.1");
        // What the undo set aside reads as the update put back, and undoing
        // that moves the pack on again.
        let sets = crate::backups::list(&dir);
        let put = sets[0].pack.clone().unwrap();
        assert!(put.undone);
        assert_eq!((put.version.as_str(), put.file_id), ("1.80.0", 100));
        let mut inst = crate::instances::load(&dir).unwrap();
        assert_eq!(restore_pack(&mut inst, &put, 5000).unwrap(), PackRestore::Restored { version: "1.81.0".into() });
        let now = crate::instances::load(&dir).unwrap();
        assert_eq!((now.pack.unwrap().file_id, now.loader.as_str(), now.game_version.as_str()), (200, "neoforge-21.1.300", "1.21.4"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pack_updated_again_is_left_alone() {
        let (mut inst, mark, dir) = updated_instance("moved");
        inst.pack = Some(pack_ref(300, "1.82.0"));
        inst.save().unwrap();
        let r = restore_pack(&mut inst, &mark, 5000).unwrap();
        assert_eq!(r, PackRestore::Moved { version: "1.82.0".into() });
        let now = crate::instances::load(&dir).unwrap();
        assert_eq!(now.pack.unwrap().file_id, 300);
        assert_eq!(now.loader, "neoforge-21.1.300");
        // Nothing was marked as put back.
        assert!(crate::backups::list(&dir)[0].pack.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_mark_from_before_the_record_was_kept_changes_nothing() {
        let (mut inst, mut mark, dir) = updated_instance("old");
        mark.before = None;
        assert_eq!(restore_pack(&mut inst, &mark, 5000).unwrap(), PackRestore::Unknown);
        assert_eq!(crate::instances::load(&dir).unwrap().pack.unwrap().file_id, 200);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_server_gets_its_pack_record_and_link_back() {
        use crate::backups::{PackBefore, PackMark};
        use crate::hosting::pack_link::{self, PackLink};
        use crate::server_pack_update as spu;
        let dir = std::env::temp_dir().join(format!("hopper-undo-pack-server-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let cfg = dir.join(crate::sync::BACKUP_DIR).join("5000").join("config");
        fs::create_dir_all(&cfg).unwrap();
        fs::write(cfg.join("a.toml"), "x").unwrap();
        let link = |f: &str| PackLink { source: "curseforge".into(), project: "77".into(), file: f.into() };
        let marker = |pack: u64| spu::Marker { project_id: 77, pack_file_id: pack, server_pack_file_id: pack + 1, server_pack: "server.zip".into() };
        spu::record(&dir, &marker(100)).unwrap();
        pack_link::write(&dir, &link("100"));
        let before = PackBefore::of_server(&dir);
        spu::record(&dir, &marker(200)).unwrap();
        pack_link::write(&dir, &link("200"));
        let mark = PackMark { before: Some(before), file_id: 200, ..PackMark::new("TNP".into(), "1.81.0".into()) };
        let mut inst = Instance { name: "TNP".into(), root: dir.clone(), ..Default::default() };
        restore_pack(&mut inst, &mark, 5000).unwrap();
        assert_eq!(spu::recorded(&dir), Some(marker(100)));
        assert_eq!(pack_link::read(&dir), Some(link("100")));
        // The server had no hopper.json and none is made for it.
        assert!(!dir.join(crate::instances::HOPPER_FILE).exists());

        // A server that had no pack record: it is gone again.
        let blank = PackBefore { server: Some(spu::Marker::default()), ..Default::default() };
        spu::record(&dir, &marker(200)).unwrap();
        let mark = PackMark { before: Some(blank), file_id: 200, ..PackMark::new("TNP".into(), "1.81.0".into()) };
        restore_pack(&mut inst, &mark, 5000).unwrap();
        assert!(spu::recorded(&dir).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn closest_is_the_newest_not_newer() {
        let files = [file(30, "2026-03"), file(20, "2026-02"), file(10, "2026-01")];
        let mut r = Ref { project_id: 1, file_id: 25, file_name: "a.jar".into(), sha1: String::new(), size: 0, mr_version: String::new(), mr_project: String::new(), name: String::new(), addon: None };
        // No date: by file id.
        assert_eq!(closest(&r, &files), 1);
        r.addon = Some(Addon { file_date: "2026-02-15".into(), ..Default::default() });
        assert_eq!(closest(&r, &files), 1);
        r.addon = Some(Addon { file_date: "2025-12".into(), ..Default::default() });
        assert_eq!(closest(&r, &files), 2);
    }
}
