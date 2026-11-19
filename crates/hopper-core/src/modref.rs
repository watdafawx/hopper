//! What a mod id from a crash report stands for on one install ("arcane_framework"
//! in "Needs arcane_framework 1.0"): the jar that provides it, or the project a mod
//! site lists under it. Hopper Server's crash card shows the mod's name and icon
//! from this, as Hopper desktop's missing-mods notice does; the lookup of a project
//! is [`crate::install::project_for_mod_id`], the one Install uses.

use crate::cf;
use crate::deps;
use crate::instances::Instance;
use crate::install;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A mod site's project, as the web UI's Browse page opens it.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct ProjectRef {
    /// "modrinth" or "curseforge".
    pub source: &'static str,
    /// Modrinth's project id, or CurseForge's number, as text.
    pub id: String,
    pub name: String,
    pub icon: String,
    pub page: String,
}

/// The project behind a search or slug hit.
pub fn project_ref(m: &cf::Mod) -> ProjectRef {
    let (source, id) = match &m.modrinth {
        Some(mr) => ("modrinth", mr.clone()),
        None => ("curseforge", m.id.to_string()),
    };
    ProjectRef { source, id, name: install::name_head(&m.name).to_string(), icon: m.icon().unwrap_or_default().to_string(), page: m.page() }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum IdRef {
    /// A jar in the mods folder provides it (the file as it is on disk).
    Installed { file: String },
    /// The game, its loader or Java: set in the loader settings.
    Loader,
    /// Not installed; a mod site lists it.
    Project(ProjectRef),
    /// Not installed and no site lists it under that id.
    Unknown,
}

fn loose(s: &str) -> String {
    s.to_lowercase().replace(['_', '-'], "")
}

/// The jars in `mods` (on or off), turned-on ones first.
fn jars(mods: &Path) -> Vec<(String, PathBuf)> {
    let mut v: Vec<(String, PathBuf)> = std::fs::read_dir(mods)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            let l = n.to_lowercase();
            (l.ends_with(".jar") || l.ends_with(".jar.disabled")).then(|| (n, e.path()))
        })
        .collect();
    v.sort_by_key(|(n, _)| (n.to_lowercase().ends_with(".disabled"), n.to_lowercase()));
    v
}

/// The jar that is each of `ids` (a turned-off one counts, a turned-on one
/// wins). A jar named like the id is opened first; only for an id still
/// unfound are all the jars asked for their own ids (kept on disk, so the
/// next ask is quick), then the named-alike ones for the ids they carry
/// inside (a library packed into another mod).
pub fn installed_files(mods: &Path, ids: &[String]) -> BTreeMap<String, String> {
    let all = jars(mods);
    let mut found: BTreeMap<String, String> = BTreeMap::new();
    let alike = |id: &str| {
        let want = loose(id);
        all.iter().filter(move |(n, _)| loose(n).contains(&want)).collect::<Vec<_>>()
    };
    for id in ids {
        if let Some((n, _)) = alike(id).into_iter().find(|(_, p)| deps::own_ids(p).0.iter().any(|i| i == id)) {
            found.insert(id.clone(), n.clone());
        }
    }
    if ids.iter().any(|id| !found.contains_key(id)) {
        for (n, p) in &all {
            let own = deps::own_ids(p).0;
            for id in ids {
                if !found.contains_key(id) && own.iter().any(|i| i == id) {
                    found.insert(id.clone(), n.clone());
                }
            }
        }
    }
    for id in ids {
        if !found.contains_key(id)
            && let Some((n, _)) = alike(id).into_iter().find(|(_, p)| deps::provides(p).iter().any(|i| i == id))
        {
            found.insert(id.clone(), n.clone());
        }
    }
    deps::save_own_ids();
    found
}

/// What each of `ids` is on `inst`: a jar it has, the game or loader, a
/// project to add, or nothing found. `project` asks the mod sites for one
/// that isn't installed ([`project_for_id`] on the panel).
pub fn resolve(
    inst: &Instance,
    ids: &[String],
    project: impl Fn(&Instance, &str) -> Option<ProjectRef>,
) -> BTreeMap<String, IdRef> {
    let mut out: BTreeMap<String, IdRef> = BTreeMap::new();
    let mut want: Vec<String> = Vec::new();
    for id in ids {
        if crate::crashdetect::is_loader_id(id) {
            out.insert(id.clone(), IdRef::Loader);
        } else if !want.contains(id) {
            want.push(id.clone());
        }
    }
    let have = installed_files(&inst.mods_dir(), &want);
    for id in want {
        let r = match have.get(&id) {
            Some(file) => IdRef::Installed { file: file.clone() },
            None => project(inst, &id).map_or(IdRef::Unknown, IdRef::Project),
        };
        out.insert(id, r);
    }
    out
}

/// The project a mod site lists under `id` for `inst`'s game and loader.
pub fn project_for_id(inst: &Instance, id: &str) -> Option<ProjectRef> {
    install::project_for_mod_id(inst, id).ok().flatten().map(|m| project_ref(&m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A jar whose own mod is `id`.
    fn jar(p: &Path, id: &str) {
        use std::io::Write;
        let mut z = zip::ZipWriter::new(fs::File::create(p).unwrap());
        z.start_file::<_, ()>("META-INF/neoforge.mods.toml", Default::default()).unwrap();
        write!(z, "modLoader=\"javafml\"\n[[mods]]\nmodId=\"{id}\"\nversion=\"1.0\"\ndisplayName=\"{id}\"\n").unwrap();
        z.finish().unwrap();
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-modref-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_jar_is_found_by_its_mod_id_whatever_it_is_named() {
        let mods = scratch("files");
        jar(&mods.join("spell_atlas-1.2.jar"), "spell_atlas");
        // Named nothing like its id.
        jar(&mods.join("some-library-3.0.jar"), "arcane_framework");
        // Turned off only.
        jar(&mods.join("old_thing-1.0.jar.disabled"), "old_thing");
        // On and off: the one that is on wins.
        jar(&mods.join("twice-1.0.jar"), "twice");
        jar(&mods.join("twice-0.9.jar.disabled"), "twice");
        let ids: Vec<String> = ["spell_atlas", "arcane_framework", "old_thing", "twice", "nothing_here"].map(String::from).to_vec();
        let got = installed_files(&mods, &ids);
        assert_eq!(got.get("spell_atlas").map(String::as_str), Some("spell_atlas-1.2.jar"));
        assert_eq!(got.get("arcane_framework").map(String::as_str), Some("some-library-3.0.jar"));
        assert_eq!(got.get("old_thing").map(String::as_str), Some("old_thing-1.0.jar.disabled"));
        assert_eq!(got.get("twice").map(String::as_str), Some("twice-1.0.jar"));
        assert!(!got.contains_key("nothing_here"));
        // A jar named like the id that isn't that mod doesn't count.
        jar(&mods.join("nothing_here_addon-1.0.jar"), "addon");
        assert!(!installed_files(&mods, &["nothing_here".to_string()]).contains_key("nothing_here"));
        let _ = fs::remove_dir_all(&mods);
    }

    #[test]
    fn a_project_is_opened_by_the_site_that_lists_it() {
        let mut m = cf::Mod { id: 4242, name: "Arcane Framework".into(), ..Default::default() };
        let p = project_ref(&m);
        assert_eq!((p.source, p.id.as_str(), p.name.as_str()), ("curseforge", "4242", "Arcane Framework"));
        // A site's subtitle is left off, as on desktop.
        m.name = "Arcane Framework - Library for the Almanac Mods".into();
        assert_eq!(project_ref(&m).name, "Arcane Framework");
        m.modrinth = Some("AbCd1234".into());
        let p = project_ref(&m);
        assert_eq!((p.source, p.id.as_str()), ("modrinth", "AbCd1234"));
    }

    #[test]
    fn loader_ids_and_installed_jars_never_ask_a_site() {
        let root = scratch("resolve");
        fs::create_dir_all(root.join("mods")).unwrap();
        jar(&root.join("mods/spell_atlas-1.2.jar"), "spell_atlas");
        let inst = Instance { root: root.clone(), ..Default::default() };
        let asked = std::cell::RefCell::new(Vec::<String>::new());
        let ids: Vec<String> = ["minecraft", "neoforge", "spell_atlas", "arcane_framework", "no_such_mod", "arcane_framework"].map(String::from).to_vec();
        let got = resolve(&inst, &ids, |_, id| {
            asked.borrow_mut().push(id.to_string());
            (id == "arcane_framework").then(|| ProjectRef { source: "modrinth", id: "AbCd1234".into(), name: "Arcane Framework".into(), icon: String::new(), page: String::new() })
        });
        assert_eq!(got["minecraft"], IdRef::Loader);
        assert_eq!(got["neoforge"], IdRef::Loader);
        assert_eq!(got["spell_atlas"], IdRef::Installed { file: "spell_atlas-1.2.jar".into() });
        assert!(matches!(&got["arcane_framework"], IdRef::Project(p) if p.name == "Arcane Framework"));
        assert_eq!(got["no_such_mod"], IdRef::Unknown);
        // Only the two that aren't here were looked up, each once.
        assert_eq!(*asked.borrow(), ["arcane_framework", "no_such_mod"]);
        let j = serde_json::to_value(&got["arcane_framework"]).unwrap();
        assert_eq!(j["kind"], "project");
        assert_eq!(j["source"], "modrinth");
        assert_eq!(serde_json::to_value(&got["spell_atlas"]).unwrap()["file"], "spell_atlas-1.2.jar");
        let _ = fs::remove_dir_all(&root);
    }
}
