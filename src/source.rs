//! One door to CurseForge and Modrinth. Project and file ids from
//! `modrinth::num_id` go to Modrinth; everything else to CurseForge.

use crate::cf::{self, Loader};
use crate::modrinth::{self, is_mr, real_id};
use crate::richtext::{self, Block};

pub fn get_mod(id: u64) -> anyhow::Result<cf::Mod> {
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
    if !is_mr(id) {
        return cf::files(id, game_version, loader);
    }
    Ok(modrinth::versions(&real_id(id)?, game_version, loader)?
        .iter()
        .map(modrinth::version_to_file)
        .collect())
}

pub fn file(project: u64, file: u64) -> anyhow::Result<cf::File> {
    if !is_mr(project) {
        return cf::file(project, file);
    }
    Ok(modrinth::version_to_file(&modrinth::version(&real_id(
        file,
    )?)?))
}

/// A file's changelog, parsed. Modrinth's comes with the version.
pub fn changelog(f: &cf::File) -> anyhow::Result<Vec<Block>> {
    match &f.modrinth {
        Some(m) => Ok(richtext::parse_markdown(&m.changelog)),
        None => Ok(richtext::parse(&cf::changelog(f.mod_id, f.id)?)),
    }
}

/// A project's description, parsed.
pub fn description(id: u64) -> anyhow::Result<Vec<Block>> {
    if !is_mr(id) {
        return Ok(richtext::parse(&cf::description(id)?));
    }
    Ok(richtext::parse_markdown(
        &modrinth::project(&real_id(id)?)?.body,
    ))
}

pub fn search(s: &cf::Search, modrinth: bool) -> anyhow::Result<(Vec<cf::Mod>, cf::Pagination)> {
    if modrinth {
        modrinth::search(s)
    } else {
        cf::search(s)
    }
}
