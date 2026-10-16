//! Browse helpers shared by the UIs: installing a project wherever it
//! belongs, a project's dependencies, and whether an instance already has it.

use crate::cf::{self, Loader};
use crate::instances::Instance;
use crate::jobs::Job;
use crate::{install, instance_ops, modpack};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Where an install from Browse goes.
pub struct Placement<'a> {
    /// New modpack instances go here.
    pub instances_root: &'a Path,
    /// The instance mods, packs, shaders, worlds and data packs go into.
    pub target: Option<Instance>,
    /// The folder data packs (or global resource packs) go into.
    pub dest: Option<PathBuf>,
}

/// `true` for a modpack, which installs as a new instance.
pub fn is_modpack(m: &cf::Mod, browsing: u32) -> bool {
    m.class_id == Some(cf::CLASS_MODPACKS)
        || (browsing == cf::CLASS_MODPACKS && m.class_id.is_none())
}

/// Installs `m` (the given `file`, or the best fit): a modpack becomes a new
/// instance, anything else goes into the target. Returns the instance that
/// changed or appeared, and a line describing what happened.
pub fn install_project(
    m: &cf::Mod,
    file: Option<cf::File>,
    browsing: u32,
    place: Placement,
    job: &Job,
) -> anyhow::Result<(PathBuf, String)> {
    if is_modpack(m, browsing) {
        let inst = if m.modrinth.is_some() {
            let f = match file {
                Some(f) => f,
                None => crate::source::files(m.id, "", Loader::Any)?
                    .into_iter()
                    .find(|f| f.release_type == 1)
                    .ok_or_else(|| anyhow::anyhow!("{} has no release", m.name))?,
            };
            modpack::install_mrpack(place.instances_root, m, &f, job)?
        } else {
            let f = match file {
                Some(f) => f,
                None => cf::file(m.id, m.main_file_id)?,
            };
            modpack::install(place.instances_root, m, &f, job)?
        };
        let line = format!("Installed into {}", inst.root.display());
        return Ok((inst.root, line));
    }
    let mut inst = place
        .target
        .ok_or_else(|| anyhow::anyhow!("pick an instance to install into"))?;
    let class = m.class_id.unwrap_or(browsing);
    // Resource packs headed for a loader folder (Paxi…) skip tracking.
    let global_rp = class == cf::CLASS_RESOURCE_PACKS
        && place
            .dest
            .as_ref()
            .is_some_and(|d| *d != inst.root.join("resourcepacks"));
    let pick = |inst: &Instance| -> anyhow::Result<cf::File> {
        match file.clone() {
            Some(f) => Ok(f),
            None => install::best_file(m.id, Some(class), inst)?.ok_or_else(|| {
                anyhow::anyhow!("{} has no file for {} {}", m.name, inst.game_version, inst.loader)
            }),
        }
    };
    let rel = |dir: &Path| {
        dir.strip_prefix(&inst.root)
            .unwrap_or(dir)
            .display()
            .to_string()
    };
    let line = if class == cf::CLASS_WORLDS {
        let f = pick(&inst)?;
        let dir = instance_ops::install_world(&inst, m, &f, job)?;
        format!(
            "Added world {}",
            dir.file_name().unwrap_or_default().to_string_lossy()
        )
    } else if class == cf::CLASS_DATAPACKS || global_rp {
        let dir = place.dest.ok_or_else(|| {
            anyhow::anyhow!(
                "pick where data packs go: {} has no world or global data pack folder yet",
                inst.name
            )
        })?;
        let f = pick(&inst)?;
        instance_ops::install_datapack(&dir, m, &f, job)?;
        format!("Added to {}", rel(&dir))
    } else {
        let f = match file {
            Some(f) => f,
            None => install::best_file(m.id, m.class_id, &inst)?.ok_or_else(|| {
                anyhow::anyhow!("{} has no file for {} {}", m.name, inst.game_version, inst.loader)
            })?,
        };
        let added = install::install(&mut inst, m, &f, job)?;
        format!("Added {}", added.join(", "))
    };
    Ok((inst.root.clone(), line))
}

/// A file's required, optional and incompatible projects, required first.
pub fn dependencies(f: &cf::File) -> Vec<(cf::Mod, u32)> {
    let wanted: Vec<(u64, u32)> = match &f.modrinth {
        Some(mr) => mr
            .requires
            .iter()
            .map(|p| (crate::modrinth::num_id(p), cf::DEP_REQUIRED))
            .collect(),
        None => f
            .dependencies
            .iter()
            .filter(|d| matches!(d.relation_type, 2 | cf::DEP_REQUIRED | cf::DEP_INCOMPATIBLE))
            .map(|d| (d.mod_id, d.relation_type))
            .collect(),
    };
    let curse: Vec<u64> = wanted
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| !crate::modrinth::is_mr(*id))
        .collect();
    let mut found: HashMap<u64, cf::Mod> = cf::get_mods(&curse)
        .unwrap_or_default()
        .into_iter()
        .map(|m| (m.id, m))
        .collect();
    for (id, _) in &wanted {
        if crate::modrinth::is_mr(*id)
            && let Ok(m) = crate::source::get_mod(*id)
        {
            found.insert(*id, m);
        }
    }
    let mut out: Vec<(cf::Mod, u32)> = wanted
        .into_iter()
        .filter_map(|(id, rel)| found.get(&id).cloned().map(|m| (m, rel)))
        .collect();
    out.sort_by_key(|(m, rel)| (*rel != cf::DEP_REQUIRED, m.name.to_lowercase()));
    out
}

/// How `m` is installed in `inst`: the same project, or the same mod from the
/// other site (matched by slug or name). Returns the source it came from.
pub fn installed_as(inst: &Instance, m: &cf::Mod) -> Option<&'static str> {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase()
    };
    let slug = |page: &str| {
        page.trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_lowercase()
    };
    let (name, mslug) = (norm(&m.name), m.slug.to_lowercase());
    inst.addons
        .iter()
        .find(|a| {
            a.project_id == m.id
                || (!mslug.is_empty() && slug(&a.page) == mslug)
                || norm(&a.name) == name
        })
        .map(|a| {
            if a.mr_project.is_empty() {
                "CurseForge"
            } else {
                "Modrinth"
            }
        })
}

/// A modpack's official server files: what a dedicated server runs.
#[derive(Clone, Debug)]
pub struct ServerFiles {
    pub project: u64,
    /// The pack file (CurseForge) or version (Modrinth) they go with.
    pub pack_file: u64,
    /// CurseForge's server pack file id; 0 on Modrinth.
    pub server_file: u64,
    /// The zip itself when already known (Modrinth's); CurseForge's is
    /// looked up by [`server_zip`].
    pub file: Option<cf::File>,
}

/// The server pack of a CurseForge pack's main file, else of its newest
/// file that has one, from the files a search hit (or project) carries.
pub fn curse_server_files(m: &cf::Mod) -> Option<ServerFiles> {
    if m.modrinth.is_some() {
        return None;
    }
    curse_server_files_in(m.id, m.main_file_id, &m.latest_files)
}

/// [`curse_server_files`] from a list of the pack's files.
pub fn curse_server_files_in(project: u64, main: u64, files: &[cf::File]) -> Option<ServerFiles> {
    let has = |f: &&cf::File| {
        f.is_server_pack != Some(true) && f.server_pack_file_id.is_some_and(|id| id != 0)
    };
    let f = files
        .iter()
        .filter(has)
        .find(|f| f.id == main)
        .or_else(|| files.iter().filter(has).max_by(|a, b| a.file_date.cmp(&b.file_date)))?;
    Some(ServerFiles {
        project,
        pack_file: f.id,
        server_file: f.server_pack_file_id.unwrap_or(0),
        file: None,
    })
}

/// The server zip a Modrinth pack version carries, if it has one.
pub fn modrinth_server_files(v: &crate::modrinth::Version) -> Option<ServerFiles> {
    let file = crate::modrinth::server_zip(v)?;
    Some(ServerFiles {
        project: crate::modrinth::num_id(&v.project_id),
        pack_file: crate::modrinth::num_id(&v.id),
        server_file: 0,
        file: Some(file),
    })
}

/// Server files for each of these Modrinth packs (by Browse id), from the
/// versions their search hits list as newest, in one request. Packs
/// without them map to `None`.
pub fn modrinth_server_files_for(versions: &[u64]) -> anyhow::Result<HashMap<u64, Option<ServerFiles>>> {
    let ids: Vec<String> = versions
        .iter()
        .filter_map(|v| crate::modrinth::real_id(*v).ok())
        .collect();
    let mut out = HashMap::new();
    for v in crate::modrinth::versions_by_id(&ids)? {
        out.insert(crate::modrinth::num_id(&v.project_id), modrinth_server_files(&v));
    }
    Ok(out)
}

/// The newest server files a Modrinth pack has, looking through its versions.
pub fn modrinth_latest_server_files(project: u64) -> anyhow::Result<Option<ServerFiles>> {
    let id = crate::modrinth::real_id(project)?;
    Ok(crate::modrinth::versions(&id, "", Loader::Any)?
        .iter()
        .find_map(modrinth_server_files))
}

/// The server zip as a file to download.
pub fn server_zip(s: &ServerFiles) -> anyhow::Result<cf::File> {
    match &s.file {
        Some(f) => Ok(f.clone()),
        None => cf::file(s.project, s.server_file),
    }
}

/// The pack version server files go with, from the site (no download):
/// its name and the Minecraft and loader tags in `game_versions`.
pub fn server_files_version(s: &ServerFiles) -> anyhow::Result<cf::File> {
    if s.file.is_some() {
        let id = crate::modrinth::real_id(s.pack_file)?;
        Ok(crate::modrinth::version_to_file(&crate::modrinth::version(&id)?))
    } else {
        cf::file(s.project, s.pack_file)
    }
}

/// Saves the server zip to `dest`.
pub fn download_server_files(s: &ServerFiles, dest: &Path, job: &Job) -> anyhow::Result<()> {
    let f = server_zip(s)?;
    let url = cf::download_url(&f)?;
    let total = f.file_length;
    job.detail(format!("Downloading {}", f.file_name));
    cf::download(&url, dest, |n| {
        if total > 0 {
            job.progress(n as usize, total as usize);
        }
    })?;
    Ok(())
}

/// "1.2M" style counts.
pub fn human_count(n: f64) -> String {
    match n {
        n if n >= 1e9 => format!("{:.1}B", n / 1e9),
        n if n >= 1e6 => format!("{:.1}M", n / 1e6),
        n if n >= 1e3 => format!("{:.1}K", n / 1e3),
        n => format!("{n:.0}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instances::Addon;

    #[test]
    fn installed_as_matches_across_sites() {
        let inst = Instance {
            addons: vec![Addon {
                project_id: 1,
                name: "Just Enough Items (JEI)".into(),
                page: "https://www.curseforge.com/minecraft/mc-mods/jei".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let same = cf::Mod {
            id: 1,
            ..Default::default()
        };
        assert_eq!(installed_as(&inst, &same), Some("CurseForge"));
        let by_slug = cf::Mod {
            id: 99,
            slug: "JEI".into(),
            ..Default::default()
        };
        assert_eq!(installed_as(&inst, &by_slug), Some("CurseForge"));
        let other = cf::Mod {
            id: 5,
            name: "Sodium".into(),
            ..Default::default()
        };
        assert_eq!(installed_as(&inst, &other), None);
    }

    #[test]
    fn finds_a_packs_server_files() {
        let file = |id: u64, sp: Option<u64>, date: &str| cf::File {
            id,
            server_pack_file_id: sp,
            file_date: date.into(),
            ..Default::default()
        };
        let mut m = cf::Mod {
            id: 7,
            main_file_id: 2,
            latest_files: vec![file(1, Some(11), "2026-01"), file(2, Some(22), "2026-02")],
            ..Default::default()
        };
        let s = curse_server_files(&m).unwrap();
        assert_eq!((s.project, s.pack_file, s.server_file), (7, 2, 22));
        // The main file has none: the newest that does.
        m.latest_files[1].server_pack_file_id = None;
        m.latest_files.push(file(3, Some(33), "2025-12"));
        assert_eq!(curse_server_files(&m).unwrap().server_file, 11);
        m.latest_files.iter_mut().for_each(|f| f.server_pack_file_id = Some(0));
        assert!(curse_server_files(&m).is_none());

        use crate::modrinth::{Version, VersionFile};
        let vf = |name: &str, primary: bool| VersionFile {
            url: format!("https://cdn/{name}"),
            filename: name.into(),
            primary,
            size: 5,
            hashes: [("sha1".to_string(), "ab".to_string())].into(),
        };
        let mut v = Version {
            id: "ver".into(),
            project_id: "proj".into(),
            files: vec![vf("Pack 1.0.mrpack", true)],
            ..Default::default()
        };
        assert!(modrinth_server_files(&v).is_none());
        v.files.push(vf("Pack-1.0-Server.zip", false));
        let s = modrinth_server_files(&v).unwrap();
        let f = s.file.unwrap();
        assert_eq!(f.file_name, "Pack-1.0-Server.zip");
        assert_eq!(f.download_url.as_deref(), Some("https://cdn/Pack-1.0-Server.zip"));
        assert_eq!(f.sha1().as_deref(), Some("ab"));
        assert_eq!(s.project, crate::modrinth::num_id("proj"));
    }

    /// Network: a Modrinth pack's server zip and a CurseForge server pack
    /// both resolve to files that download.
    #[test]
    #[ignore]
    fn real_server_files_resolve() {
        let mr = crate::modrinth::num_id("VUmd23oG"); // Create Live 5
        let s = modrinth_latest_server_files(mr).unwrap().expect("Create Live 5 has a server zip");
        let f = server_zip(&s).unwrap();
        assert!(f.file_name.to_lowercase().contains("server"), "{}", f.file_name);
        assert!(f.file_length > 0 && f.sha1().is_some());
        let m = cf::get_mod(925200).unwrap(); // ATM10
        let files = cf::files(m.id, "", Loader::Any).unwrap();
        let s = curse_server_files_in(m.id, m.main_file_id, &files).expect("ATM10 has server packs");
        let f = server_zip(&s).unwrap();
        assert!(f.is_server_pack == Some(true) || f.file_name.to_lowercase().contains("server"));
        assert!(cf::download_url(&f).is_ok());
        let dir = std::env::temp_dir().join(format!("hopper-sf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = modrinth_latest_server_files(mr).unwrap().unwrap();
        let dest = dir.join("server.zip");
        download_server_files(&s, &dest, &crate::jobs::Jobs::default().start("test")).unwrap();
        assert_eq!(std::fs::metadata(&dest).unwrap().len(), s.file.unwrap().file_length);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn counts_read_short() {
        assert_eq!(human_count(950.0), "950");
        assert_eq!(human_count(12_300.0), "12.3K");
        assert_eq!(human_count(4_500_000.0), "4.5M");
    }
}
