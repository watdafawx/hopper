//! CurseForge and Modrinth project links a player meets outside Hopper
//! (shared on Discord, clicked in a browser), read from their URL. Reading
//! one never touches the network; [`resolve`] looks the project up.
//!
//! Every form the two sites have used is read: `www.` or not, the old
//! `minecraft.curseforge.com/projects/...` and `mods.curse.com` pages,
//! project ids instead of slugs, and the `/files/<id>` and
//! `/version/<id>` pages of one file. Anything else is not a mod link, so
//! the link opener hands it to the browser.

use crate::cf;
use crate::source::ProjectLink;

/// One file of the project, when the link is to a file's page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileRef {
    /// A CurseForge file id.
    Cf(u64),
    /// A Modrinth version id or version number.
    Mr(String),
}

/// A project link, read from its URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModLink {
    pub project: ProjectLink,
    pub file: Option<FileRef>,
}

impl ModLink {
    /// The site's name, for the player.
    pub fn site(&self) -> &'static str {
        match self.project {
            ProjectLink::Mr(_) => "Modrinth",
            _ => "CurseForge",
        }
    }
}

/// Reads a project link ("https://www.curseforge.com/minecraft/mc-mods/jei",
/// "modrinth.com/mod/sodium/version/0.6.0"); None for any other URL.
pub fn parse(url: &str) -> Option<ModLink> {
    let url = url.trim();
    let rest = match url.split_once("://") {
        Some((scheme, r)) if scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http") => r,
        Some(_) => return None,
        None => url,
    };
    let rest = rest.split(['?', '#']).next()?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    // A port or a login in the host is never one of the sites' own links.
    if host.contains([':', '@']) {
        return None;
    }
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    match host {
        "curseforge.com" | "beta.curseforge.com" | "legacy.curseforge.com" => curseforge(&segs),
        // The old site, before the game folders.
        "minecraft.curseforge.com" => match segs.as_slice() {
            ["projects", p, more @ ..] => Some(ModLink { project: cf_project(p), file: cf_file(more) }),
            _ => None,
        },
        // Curse's even older pages: ".../mc-mods/minecraft/238222-jei".
        "mods.curse.com" => match segs.as_slice() {
            [_, "minecraft", p, ..] => {
                let id = p.split('-').next()?.parse().ok()?;
                Some(ModLink { project: ProjectLink::CfId(id), file: None })
            }
            _ => None,
        },
        "modrinth.com" => modrinth(&segs),
        _ => None,
    }
}

fn curseforge(segs: &[&str]) -> Option<ModLink> {
    match segs {
        ["minecraft", class, slug, more @ ..] => {
            let class = match *class {
                "mc-mods" => Some(cf::CLASS_MODS),
                "modpacks" => Some(cf::CLASS_MODPACKS),
                "texture-packs" => Some(cf::CLASS_RESOURCE_PACKS),
                "shaders" => Some(cf::CLASS_SHADERS),
                "data-packs" => Some(cf::CLASS_DATAPACKS),
                "worlds" => Some(cf::CLASS_WORLDS),
                "customization" => Some(cf::CLASS_CUSTOMIZATION),
                // Plugins have no class of their own in Hopper; the slug
                // alone finds them.
                "bukkit-plugins" => None,
                _ => return None,
            };
            // "/minecraft/mc-mods/search" and the like are lists, not projects.
            if matches!(*slug, "search" | "categories" | "category") {
                return None;
            }
            Some(ModLink { project: ProjectLink::Cf(class, slug.to_string()), file: cf_file(more) })
        }
        ["projects", p, more @ ..] => Some(ModLink { project: cf_project(p), file: cf_file(more) }),
        _ => None,
    }
}

fn cf_project(p: &str) -> ProjectLink {
    match p.parse() {
        Ok(id) => ProjectLink::CfId(id),
        Err(_) => ProjectLink::Cf(None, p.to_string()),
    }
}

/// "files/5101366" or "download/5101366" after the project.
fn cf_file(more: &[&str]) -> Option<FileRef> {
    match more {
        ["files" | "download", id, ..] => id.parse().ok().map(FileRef::Cf),
        _ => None,
    }
}

fn modrinth(segs: &[&str]) -> Option<ModLink> {
    match segs {
        ["mod" | "modpack" | "resourcepack" | "shader" | "datapack" | "plugin" | "project", slug, more @ ..] => {
            let file = match more {
                ["version", v, ..] => Some(FileRef::Mr(v.to_string())),
                _ => None,
            };
            Some(ModLink { project: ProjectLink::Mr(slug.to_string()), file })
        }
        _ => None,
    }
}

/// A mod link that is the whole of `text` (what was copied), allowing the
/// angle brackets Discord puts round a link it shouldn't preview. Text with
/// anything else in it isn't offered: the player copied a message, not a
/// link.
pub fn from_text(text: &str) -> Option<(String, ModLink)> {
    let t = text.trim();
    if t.len() > 2048 || t.chars().any(char::is_whitespace) {
        return None;
    }
    let t = t.strip_prefix('<').and_then(|t| t.strip_suffix('>')).unwrap_or(t);
    let url = if t.contains("://") { t.to_string() } else { format!("https://{t}") };
    // Without a scheme, only the sites' own host names count.
    if !t.contains("://") {
        let host = t.split('/').next()?.to_ascii_lowercase();
        if !host.contains("curseforge.com") && !host.contains("modrinth.com") && !host.contains("curse.com") {
            return None;
        }
    }
    parse(&url).map(|l| (url, l))
}

/// The project a link points at, and its file when the link names one.
/// A file that can't be found still shows the project.
pub fn resolve(link: &ModLink) -> anyhow::Result<(cf::Mod, Option<cf::File>)> {
    let m = crate::source::mod_from_link(&link.project)?;
    let file = match &link.file {
        Some(FileRef::Cf(id)) if m.modrinth.is_none() => cf::file(m.id, *id).ok(),
        Some(FileRef::Mr(v)) if m.modrinth.is_some() => match &link.project {
            ProjectLink::Mr(p) => crate::modrinth::project_version(p, v).ok().map(|v| crate::modrinth::version_to_file(&v)),
            _ => None,
        },
        _ => None,
    };
    Ok((m, file))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cf(class: Option<u32>, slug: &str) -> ProjectLink {
        ProjectLink::Cf(class, slug.into())
    }

    fn proj(url: &str) -> Option<ProjectLink> {
        parse(url).map(|l| l.project)
    }

    #[test]
    fn curseforge_pages() {
        let jei = Some(cf(Some(cf::CLASS_MODS), "jei"));
        for u in [
            "https://www.curseforge.com/minecraft/mc-mods/jei",
            "https://curseforge.com/minecraft/mc-mods/jei/",
            "http://www.curseforge.com/minecraft/mc-mods/jei?utm_source=discord",
            "https://WWW.CurseForge.com/minecraft/mc-mods/jei#comments",
            "https://beta.curseforge.com/minecraft/mc-mods/jei",
            "https://legacy.curseforge.com/minecraft/mc-mods/jei/relations/dependencies",
            "https://www.curseforge.com/minecraft/mc-mods/jei/gallery",
            "www.curseforge.com/minecraft/mc-mods/jei",
        ] {
            assert_eq!(proj(u), jei, "{u}");
        }
        assert_eq!(proj("https://www.curseforge.com/minecraft/modpacks/all-the-mods-10"), Some(cf(Some(cf::CLASS_MODPACKS), "all-the-mods-10")));
        assert_eq!(proj("https://www.curseforge.com/minecraft/texture-packs/faithful-32x"), Some(cf(Some(cf::CLASS_RESOURCE_PACKS), "faithful-32x")));
        assert_eq!(proj("https://www.curseforge.com/minecraft/shaders/complementary-reimagined"), Some(cf(Some(cf::CLASS_SHADERS), "complementary-reimagined")));
        assert_eq!(proj("https://www.curseforge.com/minecraft/data-packs/terralith"), Some(cf(Some(cf::CLASS_DATAPACKS), "terralith")));
        assert_eq!(proj("https://www.curseforge.com/minecraft/worlds/skyblock"), Some(cf(Some(cf::CLASS_WORLDS), "skyblock")));
        assert_eq!(proj("https://www.curseforge.com/minecraft/customization/x"), Some(cf(Some(cf::CLASS_CUSTOMIZATION), "x")));
        assert_eq!(proj("https://www.curseforge.com/minecraft/bukkit-plugins/worldedit"), Some(cf(None, "worldedit")));
    }

    #[test]
    fn curseforge_files_and_ids() {
        assert_eq!(
            parse("https://www.curseforge.com/minecraft/mc-mods/jei/files/5101366"),
            Some(ModLink { project: cf(Some(cf::CLASS_MODS), "jei"), file: Some(FileRef::Cf(5101366)) })
        );
        assert_eq!(parse("https://www.curseforge.com/minecraft/mc-mods/jei/download/5101366").unwrap().file, Some(FileRef::Cf(5101366)));
        // The files list isn't one file.
        assert_eq!(parse("https://www.curseforge.com/minecraft/mc-mods/jei/files/all?page=1").unwrap().file, None);
        assert_eq!(parse("https://www.curseforge.com/minecraft/mc-mods/jei/files").unwrap().file, None);
        assert_eq!(proj("https://www.curseforge.com/projects/238222"), Some(ProjectLink::CfId(238222)));
        assert_eq!(proj("https://curseforge.com/projects/jei"), Some(cf(None, "jei")));
        // The old site.
        assert_eq!(proj("https://minecraft.curseforge.com/projects/jei"), Some(cf(None, "jei")));
        assert_eq!(proj("https://minecraft.curseforge.com/projects/238222"), Some(ProjectLink::CfId(238222)));
        assert_eq!(
            parse("https://minecraft.curseforge.com/projects/jei/files/2803400").unwrap().file,
            Some(FileRef::Cf(2803400))
        );
        assert_eq!(proj("https://mods.curse.com/mc-mods/minecraft/238222-just-enough-items-jei"), Some(ProjectLink::CfId(238222)));
    }

    #[test]
    fn modrinth_pages() {
        let sodium = Some(ProjectLink::Mr("sodium".into()));
        for u in [
            "https://modrinth.com/mod/sodium",
            "https://www.modrinth.com/mod/sodium/",
            "https://modrinth.com/mod/sodium#versions",
            "https://modrinth.com/mod/sodium/versions?l=fabric",
            "https://modrinth.com/mod/sodium/gallery",
            "https://modrinth.com/mod/sodium/changelog",
            "modrinth.com/mod/sodium",
        ] {
            assert_eq!(proj(u), sodium, "{u}");
        }
        for kind in ["modpack", "resourcepack", "shader", "datapack", "plugin", "project"] {
            assert_eq!(proj(&format!("https://modrinth.com/{kind}/x")), Some(ProjectLink::Mr("x".into())), "{kind}");
        }
        // A project id works as well as a slug.
        assert_eq!(proj("https://modrinth.com/project/AANobbMI"), Some(ProjectLink::Mr("AANobbMI".into())));
        assert_eq!(
            parse("https://modrinth.com/mod/sodium/version/mc1.21.1-0.6.0-fabric"),
            Some(ModLink {
                project: ProjectLink::Mr("sodium".into()),
                file: Some(FileRef::Mr("mc1.21.1-0.6.0-fabric".into()))
            })
        );
        assert_eq!(parse("https://modrinth.com/mod/sodium/version/Yp8wLY1P").unwrap().file, Some(FileRef::Mr("Yp8wLY1P".into())));
    }

    #[test]
    fn other_links_are_not_mod_links() {
        for u in [
            "https://www.curseforge.com/minecraft/search?search=jei",
            "https://www.curseforge.com/minecraft/mc-mods",
            "https://www.curseforge.com/minecraft/mc-mods/search?x",
            "https://www.curseforge.com/linkout?remoteUrl=x",
            "https://www.curseforge.com/wow/addons/details",
            "https://www.curseforge.com/minecraft/mc-addons/x",
            "https://www.curseforge.com/",
            "https://curseforge.com",
            "https://modrinth.com/user/jellysquid",
            "https://modrinth.com/mods?q=sodium",
            "https://modrinth.com/collection/abc",
            "https://modrinth.com",
            "https://docs.modrinth.com/mod/x",
            "https://notmodrinth.com/mod/sodium",
            "https://modrinth.com.evil.example/mod/sodium",
            "https://modrinth.com:8080/mod/sodium",
            "https://user@modrinth.com/mod/sodium",
            "ftp://modrinth.com/mod/sodium",
            "curseforge://install?addonId=1",
            "https://github.com/CaffeineMC/sodium",
            "https://www.youtube.com/watch?v=x",
            "",
        ] {
            assert_eq!(parse(u), None, "{u}");
        }
    }

    #[test]
    fn copied_text() {
        let (url, l) = from_text("  https://modrinth.com/mod/sodium \n").unwrap();
        assert_eq!(url, "https://modrinth.com/mod/sodium");
        assert_eq!(l.site(), "Modrinth");
        // Discord's no-preview brackets.
        assert_eq!(from_text("<https://www.curseforge.com/minecraft/mc-mods/jei>").unwrap().1.site(), "CurseForge");
        // No scheme: only the sites' own hosts.
        assert_eq!(from_text("www.curseforge.com/minecraft/mc-mods/jei").unwrap().0, "https://www.curseforge.com/minecraft/mc-mods/jei");
        assert!(from_text("modrinth.com/mod/sodium").is_some());
        // A message with a link in it, or anything that isn't one.
        assert!(from_text("try https://modrinth.com/mod/sodium").is_none());
        assert!(from_text("https://modrinth.com/mod/sodium https://modrinth.com/mod/iris").is_none());
        assert!(from_text("sodium").is_none());
        assert!(from_text("mod/sodium").is_none());
        assert!(from_text("https://github.com/x/y").is_none());
        assert!(from_text(&format!("https://modrinth.com/mod/{}", "a".repeat(3000))).is_none());
    }
}
