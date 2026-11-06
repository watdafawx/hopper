//! Which pack a server was made from, kept in the server folder when it has
//! no server pack record (a Modrinth pack, or one built from its client
//! files or from an instance). Hopper Server reads it for the server's pack
//! version, updates and changelogs; Hopper desktop writes the same file for
//! servers it hosts, so a folder means the same thing to both.

use crate::instances::PackRef;
use crate::modrinth;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The file in the server folder.
pub const LINK: &str = ".hopper-pack.json";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PackLink {
    /// "curseforge" or "modrinth".
    pub source: String,
    /// CurseForge's project id, or Modrinth's (its string id).
    pub project: String,
    /// CurseForge's file id, or Modrinth's version id.
    pub file: String,
}

impl PackLink {
    pub fn mr(&self) -> bool {
        self.source == "modrinth"
    }

    /// Hopper's numeric ids (Modrinth's mapped, see [`modrinth::num_id`]).
    pub fn ids(&self) -> anyhow::Result<(u64, u64)> {
        if self.mr() {
            return Ok((modrinth::num_id(&self.project), modrinth::num_id(&self.file)));
        }
        let p = self.project.parse().map_err(|_| anyhow::anyhow!("bad pack id {}", self.project))?;
        let f = self.file.parse().map_err(|_| anyhow::anyhow!("bad pack file id {}", self.file))?;
        Ok((p, f))
    }

    /// The link for an instance's pack, when it has one Hopper can name
    /// (a Modrinth pack needs its string ids recorded).
    pub fn of_pack(p: &PackRef) -> Option<PackLink> {
        if p.project_id == 0 || p.file_id == 0 {
            return None;
        }
        if modrinth::is_mr(p.project_id) || !p.mr_project.is_empty() {
            return (!p.mr_project.is_empty() && !p.mr_version.is_empty()).then(|| PackLink {
                source: "modrinth".into(),
                project: p.mr_project.clone(),
                file: p.mr_version.clone(),
            });
        }
        Some(PackLink { source: "curseforge".into(), project: p.project_id.to_string(), file: p.file_id.to_string() })
    }
}

/// The link in `root`, when there is a usable one.
pub fn read(root: &Path) -> Option<PackLink> {
    let l: PackLink = serde_json::from_slice(&std::fs::read(root.join(LINK)).ok()?).ok()?;
    (!l.project.is_empty() && !l.file.is_empty()).then_some(l)
}

/// Writes `link` into `root`; a failure is only logged (the server works
/// without it).
pub fn write(root: &Path, link: &PackLink) {
    let r = serde_json::to_vec_pretty(link)
        .map_err(anyhow::Error::from)
        .and_then(|b| Ok(std::fs::write(root.join(LINK), b)?));
    if let Err(e) = r {
        log::warn!("recording {}'s pack: {e:#}", root.display());
    }
}

/// Whether `root` already has a server pack record, which pack updates
/// keep current and which wins over a link.
fn has_record(root: &Path) -> bool {
    crate::server_pack_update::recorded(root).is_some_and(|m| m.project_id != 0)
}

/// Records the pack a new server came from (version `file` of `project`,
/// Hopper's ids), unless it has a server pack record already.
pub fn link_new_server(root: &Path, mr: bool, project: u64, file: u64) {
    if has_record(root) {
        return;
    }
    let link = if mr {
        match (modrinth::real_id(project), modrinth::real_id(file)) {
            (Ok(p), Ok(f)) => PackLink { source: "modrinth".into(), project: p, file: f },
            _ => {
                log::warn!("{}: couldn't record its Modrinth pack", root.display());
                return;
            }
        }
    } else {
        PackLink { source: "curseforge".into(), project: project.to_string(), file: file.to_string() }
    };
    write(root, &link);
}

/// Records the pack of the instance a new server was made from, unless it
/// has a server pack record already.
pub fn link_from_instance(root: &Path, pack: &PackRef) {
    if has_record(root) {
        return;
    }
    if let Some(l) = PackLink::of_pack(pack) {
        write(root, &l);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn of_pack_names_curseforge_and_modrinth_packs() {
        let cf = PackRef { project_id: 925200, file_id: 7, ..Default::default() };
        assert_eq!(
            PackLink::of_pack(&cf),
            Some(PackLink { source: "curseforge".into(), project: "925200".into(), file: "7".into() })
        );
        let mut mr = PackRef { project_id: 1 << 63 | 5, file_id: 1 << 63 | 6, ..Default::default() };
        // Without its string ids a Modrinth pack can't be named.
        assert_eq!(PackLink::of_pack(&mr), None);
        mr.mr_project = "AbCd".into();
        mr.mr_version = "XyZ".into();
        let l = PackLink::of_pack(&mr).unwrap();
        assert!(l.mr());
        assert_eq!((l.project.as_str(), l.file.as_str()), ("AbCd", "XyZ"));
        assert_eq!(PackLink::of_pack(&PackRef::default()), None);
    }

    #[test]
    fn link_from_instance_writes_and_reads_back() {
        let root = std::env::temp_dir().join(format!("hopper-pack-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        link_from_instance(&root, &PackRef { project_id: 42, file_id: 9, ..Default::default() });
        let l = read(&root).unwrap();
        assert_eq!(l.ids().unwrap(), (42, 9));
        let _ = std::fs::remove_dir_all(&root);
    }
}
