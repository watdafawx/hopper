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
    /// "CurseForge" or "Modrinth".
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

fn site(r: &Ref) -> &'static str {
    if r.mr_version.is_empty() { "CurseForge" } else { "Modrinth" }
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
    if r.mr_version.is_empty() {
        // CurseForge file ids grow over time.
        if let Some(i) = files.iter().position(|f| f.id <= r.file_id) {
            return i;
        }
    }
    0
}

/// Gets `r`'s exact file to `dest`: from History's copy when it kept one,
/// else downloaded (hash-checked). When the site no longer has it, the
/// error is a [`Gone`].
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

/// Puts `new` (its file downloaded to `tmp`) in place of whatever file of
/// the same project the instance has now, on or off as `on` says.
fn put_entry(inst: &mut Instance, new: Addon, tmp: &Path, on: bool) -> anyhow::Result<()> {
    // The same project, or the same file tracked from the other site (a
    // note made from a copy can name either): replaced, never doubled.
    let old = inst
        .addons
        .iter()
        .position(|a| a.project_id == new.project_id)
        .or_else(|| inst.addons.iter().position(|a| a.file_name.eq_ignore_ascii_case(&new.file_name)))
        .map(|i| inst.addons.remove(i));
    let mut new = new;
    if old.is_none() {
        new.enabled = on;
    }
    let pid = new.project_id;
    if let Err(e) = crate::install::replace_addon(inst, old, new, tmp) {
        let _ = fs::remove_file(tmp);
        return Err(e);
    }
    if let Some(i) = inst.addons.iter().position(|a| a.project_id == pid)
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

#[cfg(test)]
mod tests {
    use super::*;

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

    fn file(id: u64, date: &str) -> cf::File {
        cf::File { id, file_date: date.into(), ..Default::default() }
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
