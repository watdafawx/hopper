//! One door to CurseForge and Modrinth. Project and file ids from
//! `modrinth::num_id` go to Modrinth; everything else to CurseForge.

use crate::cf::{self, Loader};
use crate::modrinth::{self, is_mr, real_id};
use crate::richtext::{self, Block};

pub fn get_mod(id: u64) -> anyhow::Result<cf::Mod> {
    if crate::sources::is_src(id) {
        return crate::sources::get_mod(id);
    }
    if !is_mr(id) {
        return cf::get_mod(id);
    }
    let p = modrinth::project(&real_id(id)?)?;
    let owner = modrinth::team_owners(std::slice::from_ref(&p.team)).unwrap_or_default();
    Ok(modrinth::project_to_mod(
        &p,
        owner.get(&p.team).map_or("", String::as_str),
    ))
}

/// Everything else by the author of `m`, most downloaded first.
pub fn author_projects(m: &cf::Mod) -> anyhow::Result<Vec<cf::Mod>> {
    anyhow::ensure!(!crate::sources::is_src(m.id), "{} isn't on CurseForge or Modrinth", m.name);
    let a = m.authors.first().ok_or_else(|| anyhow::anyhow!("{} lists no author", m.name))?;
    if m.modrinth.is_some() {
        return modrinth::user_projects(&a.name);
    }
    // A project built from an instance's mod list has no author id yet.
    let id = match a.id {
        0 => cf::get_mod(m.id)?.authors.first().map_or(0, |a| a.id),
        id => id,
    };
    anyhow::ensure!(id != 0, "{} lists no author", m.name);
    cf::author_mods(id)
}

/// Files of a project, newest first.
pub fn files(id: u64, game_version: &str, loader: Loader) -> anyhow::Result<Vec<cf::File>> {
    if crate::sources::is_src(id) {
        return crate::sources::files(id, game_version, loader);
    }
    if !is_mr(id) {
        return cf::files(id, game_version, loader);
    }
    Ok(modrinth::versions(&real_id(id)?, game_version, loader)?
        .iter()
        .map(modrinth::version_to_file)
        .collect())
}

pub fn file(project: u64, file: u64) -> anyhow::Result<cf::File> {
    if crate::sources::is_src(project) {
        return crate::sources::file(project, file);
    }
    if !is_mr(project) {
        return cf::file(project, file);
    }
    Ok(modrinth::version_to_file(&modrinth::version(&real_id(
        file,
    )?)?))
}

/// A file's changelog, parsed. Modrinth's comes with the version. One
/// that's only a GitHub link shows what it links to.
pub fn changelog(f: &cf::File) -> anyhow::Result<Vec<Block>> {
    if let Some(s) = &f.source {
        let notes = if s.notes.trim().is_empty() { "No release notes." } else { s.notes.as_str() };
        return Ok(richtext::parse_markdown(notes));
    }
    if crate::sources::is_src(f.mod_id) {
        return Ok(richtext::parse_markdown("No release notes."));
    }
    let blocks = match &f.modrinth {
        Some(m) => richtext::parse_markdown(&m.changelog),
        None => richtext::parse(&cf::changelog(f.mod_id, f.id)?),
    };
    Ok(crate::extlog::expand(blocks, f))
}

/// A project's description, parsed.
pub fn description(id: u64) -> anyhow::Result<Vec<Block>> {
    Ok(page(id)?.0)
}

/// A project's description, and for Modrinth its full gallery: a search
/// hit carries only 350px copies of the images, the project the originals.
pub fn page(id: u64) -> anyhow::Result<(Vec<Block>, Option<Vec<cf::Screenshot>>)> {
    // GitHub, Maven and links: never asked of CurseForge or Modrinth.
    if crate::sources::is_src(id) {
        return Ok((crate::sources::page::description(id)?, None));
    }
    if !is_mr(id) {
        return Ok((richtext::parse(&cf::description(id)?), None));
    }
    let p = modrinth::project(&real_id(id)?)?;
    let shots = modrinth::project_to_mod(&p, "").screenshots;
    Ok((richtext::parse_markdown(&p.body), Some(shots)))
}

/// A project page link on CurseForge or Modrinth, read from its URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectLink {
    /// CurseForge: a class (None when the URL doesn't say) and a slug.
    Cf(Option<u32>, String),
    /// CurseForge, by number.
    CfId(u64),
    /// Modrinth: a slug or id.
    Mr(String),
}

/// Reads a project page link ("curseforge.com/minecraft/mc-mods/jei",
/// "modrinth.com/mod/sodium"); None for any other URL.
pub fn project_link(url: &str) -> Option<ProjectLink> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.split(['?', '#']).next()?;
    let (host, path) = rest.split_once('/')?;
    let host = host.to_ascii_lowercase();
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if host == "curseforge.com" || host.ends_with(".curseforge.com") {
        return match segs.as_slice() {
            ["minecraft", class, slug, ..] => {
                let class = match *class {
                    "mc-mods" => cf::CLASS_MODS,
                    "modpacks" => cf::CLASS_MODPACKS,
                    "texture-packs" => cf::CLASS_RESOURCE_PACKS,
                    "shaders" => cf::CLASS_SHADERS,
                    "data-packs" => cf::CLASS_DATAPACKS,
                    "worlds" => cf::CLASS_WORLDS,
                    _ => return None,
                };
                Some(ProjectLink::Cf(Some(class), slug.to_string()))
            }
            ["projects", p, ..] => Some(match p.parse() {
                Ok(id) => ProjectLink::CfId(id),
                Err(_) => ProjectLink::Cf(None, p.to_string()),
            }),
            _ => None,
        };
    }
    if host == "modrinth.com" || host == "www.modrinth.com" {
        return match segs.as_slice() {
            [
                "mod" | "modpack" | "resourcepack" | "shader" | "datapack" | "plugin" | "project",
                slug,
                ..,
            ] => Some(ProjectLink::Mr(slug.to_string())),
            _ => None,
        };
    }
    None
}

/// The project a page link points at, as an id `get_mod` takes.
pub fn project_from_link(link: &ProjectLink) -> anyhow::Result<u64> {
    match link {
        ProjectLink::CfId(id) => Ok(*id),
        ProjectLink::Cf(class, slug) => cf::by_slug(*class, slug)?
            .map(|m| m.id)
            .ok_or_else(|| anyhow::anyhow!("no CurseForge project \"{slug}\"")),
        ProjectLink::Mr(slug) => Ok(modrinth::num_id(&modrinth::project(slug)?.id)),
    }
}

/// The project a page link points at, ready to show: one lookup by slug
/// rather than the id first and the project after.
pub fn mod_from_link(link: &ProjectLink) -> anyhow::Result<cf::Mod> {
    match link {
        ProjectLink::CfId(id) => cf::get_mod(*id),
        ProjectLink::Cf(class, slug) => {
            cf::by_slug(*class, slug)?.ok_or_else(|| anyhow::anyhow!("no CurseForge project \"{slug}\""))
        }
        ProjectLink::Mr(slug) => {
            let p = modrinth::project(slug)?;
            let owner = modrinth::team_owners(std::slice::from_ref(&p.team)).unwrap_or_default();
            Ok(modrinth::project_to_mod(&p, owner.get(&p.team).map_or("", String::as_str)))
        }
    }
}

pub fn search(s: &cf::Search, modrinth: bool) -> anyhow::Result<(Vec<cf::Mod>, cf::Pagination)> {
    if modrinth {
        modrinth::search(s)
    } else {
        cf::search(s)
    }
}

#[cfg(test)]
mod link_tests {
    use super::*;

    #[test]
    fn reads_project_links() {
        assert_eq!(
            project_link("https://www.curseforge.com/minecraft/mc-mods/emi-quickindex"),
            Some(ProjectLink::Cf(Some(cf::CLASS_MODS), "emi-quickindex".into()))
        );
        assert_eq!(
            project_link("https://www.curseforge.com/minecraft/shaders/complementary/files?x=1"),
            Some(ProjectLink::Cf(Some(cf::CLASS_SHADERS), "complementary".into()))
        );
        assert_eq!(project_link("https://curseforge.com/projects/238222"), Some(ProjectLink::CfId(238222)));
        assert_eq!(
            project_link("https://modrinth.com/mod/sodium#versions"),
            Some(ProjectLink::Mr("sodium".into()))
        );
        assert_eq!(project_link("https://www.curseforge.com/minecraft/search?x"), None);
        assert_eq!(project_link("https://www.curseforge.com/linkout?remoteUrl=x"), None);
        assert_eq!(project_link("https://github.com/x/y"), None);
        assert_eq!(project_link("https://modrinth.com/user/jellysquid"), None);
    }
}
