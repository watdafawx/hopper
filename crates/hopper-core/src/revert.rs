//! "Revert X": when a mod needs a newer version of another mod (or of the
//! loader) than the instance has, the newest earlier version of it that
//! works with what's installed. The copy Hopper set aside in
//! `.hopper-backups` comes first; else older files from CurseForge or
//! Modrinth are checked one by one.

use crate::crashdetect::Changed;
use crate::instances::Instance;
use crate::jobs::Job;
use std::fs;
use std::path::Path;

/// What the mod asks for that the instance doesn't have: `id` (a mod id,
/// or the loader's) at `have` now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Need {
    pub id: String,
    pub have: String,
}

/// How many older files are downloaded and checked before giving up.
const TRIES: usize = 5;

/// The newest older copy of `file_name` (a jar in `mods/`) in
/// `.hopper-backups` that doesn't ask for more than `need`, ready for
/// [`crate::crashdetect::put_back`]. Copies the backups' clean-up already
/// removed are simply not found. Reads jars: off the UI thread.
pub fn from_backups(root: &Path, file_name: &str, need: &Need) -> Option<Changed> {
    let now = root.join("mods").join(file_name);
    let (ids, name) = crate::deps::own_ids(&now);
    if ids.is_empty() {
        return None;
    }
    let to = crate::deps::own_version(&now);
    for set in crate::backups::list(root) {
        for f in set.files.iter().filter(|f| f.restore_to.starts_with("mods/") && f.name.to_ascii_lowercase().ends_with(".jar")) {
            if !f.path.is_file() || !crate::deps::own_ids(&f.path).0.iter().any(|i| ids.contains(i)) {
                continue;
            }
            let from = crate::deps::own_version(&f.path);
            if (!from.is_empty() && from == to) || crate::deps::needs_newer(&f.path, &need.id, &need.have) {
                continue;
            }
            return Some(Changed {
                file_name: file_name.to_string(),
                name: if name.is_empty() { ids[0].clone() } else { name },
                from,
                to,
                when: set.when,
                backup: f.path.clone(),
            });
        }
    }
    None
}

/// Installs the newest file of `file_name`'s project, older than the one
/// installed, that doesn't ask for more than `need`: each candidate is
/// downloaded and read first. The installed file's name.
pub fn from_source(inst: &mut Instance, file_name: &str, need: &Need, job: &Job) -> anyhow::Result<String> {
    let a = inst
        .addons
        .iter()
        .find(|a| a.file_name.eq_ignore_ascii_case(file_name.trim_end_matches(".disabled")))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("{file_name} isn't one of this instance's mods."))?;
    anyhow::ensure!(a.project_id != 0, "Hopper doesn't know where {} came from, so it can't look for an older version.", a.name);
    let files = crate::source::files(a.project_id, &inst.game_version, crate::install::loader_of(inst))?;
    // Newest first: the ones after the installed file are older.
    let older: Vec<_> = match files.iter().position(|f| f.id == a.file_id) {
        Some(i) => files[i + 1..].to_vec(),
        None => files.into_iter().filter(|f| f.file_date < a.file_date).collect(),
    };
    let dir = std::env::temp_dir().join(format!("hopper-revert-{}-{}", a.project_id, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)?;
    let found = (|| {
        for f in older.iter().take(TRIES) {
            job.check_cancelled()?;
            let Some(url) = f.download_url.as_deref() else { continue };
            job.detail(format!("Checking {}", f.display_name));
            let tmp = dir.join(&f.file_name);
            if crate::cf::download(url, &tmp, |_| {}).is_err() {
                continue;
            }
            let ok = !crate::deps::needs_newer(&tmp, &need.id, &need.have);
            let _ = fs::remove_file(&tmp);
            if ok {
                return Ok(Some(f.id));
            }
        }
        anyhow::Ok(None)
    })();
    let _ = fs::remove_dir_all(&dir);
    match found? {
        Some(id) => crate::install::install_file(inst, a.project_id, id, job),
        None => anyhow::bail!(
            "None of {}'s last {} older versions work with what's installed.",
            a.name,
            TRIES.min(older.len())
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn jar(p: &Path, id: &str, ver: &str, needs: Option<(&str, &str)>) {
        let mut z = zip::ZipWriter::new(fs::File::create(p).unwrap());
        z.start_file::<_, ()>("META-INF/neoforge.mods.toml", Default::default()).unwrap();
        write!(z, "modLoader=\"javafml\"\nloaderVersion=\"[1,)\"\n[[mods]]\nmodId=\"{id}\"\nversion=\"{ver}\"\ndisplayName=\"{id}\"\n").unwrap();
        if let Some((dep, range)) = needs {
            write!(z, "[[dependencies.{id}]]\nmodId=\"{dep}\"\ntype=\"required\"\nversionRange=\"{range}\"\n").unwrap();
        }
        z.finish().unwrap();
    }

    #[test]
    fn backup_copy_that_works_with_what_is_installed() {
        let root = std::env::temp_dir().join(format!("hopper-revert-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mods = root.join("mods");
        let new_set = root.join(".hopper-backups").join("1792845600");
        let old_set = root.join(".hopper-backups").join("1792845500");
        for d in [&mods, &new_set, &old_set] {
            fs::create_dir_all(d).unwrap();
        }
        jar(&mods.join("eaep-1.6.4.jar"), "eaep", "1.6.4", Some(("jei", "[19.70,)")));
        // The newer backup still needs the newer jei; the older one doesn't.
        jar(&new_set.join("eaep-1.6.3.jar"), "eaep", "1.6.3", Some(("jei", "[19.60,)")));
        fs::write(new_set.join(crate::backups::ORIGINS), r#"{"eaep-1.6.3.jar":"mods/eaep-1.6.3.jar"}"#).unwrap();
        jar(&old_set.join("eaep-1.6.2.jar"), "eaep", "1.6.2", Some(("jei", "[19.50,)")));
        fs::write(old_set.join(crate::backups::ORIGINS), r#"{"eaep-1.6.2.jar":"mods/eaep-1.6.2.jar"}"#).unwrap();
        let need = Need { id: "jei".into(), have: "19.57.0.450".into() };
        assert!(crate::deps::needs_newer(&mods.join("eaep-1.6.4.jar"), "jei", "19.57.0.450"));
        let c = from_backups(&root, "eaep-1.6.4.jar", &need).expect("1.6.2 works");
        assert_eq!((c.from.as_str(), c.to.as_str()), ("1.6.2", "1.6.4"));
        // Cleaned out of backups: nothing to put back.
        fs::remove_dir_all(&old_set).unwrap();
        assert!(from_backups(&root, "eaep-1.6.4.jar", &need).is_none());
        fs::remove_dir_all(&root).unwrap();
    }
}
