//! Projects saved from Browse for later, kept in `bookmarks.json` in
//! Hopper's data folder. Each keeps enough to list it (and install it)
//! without asking CurseForge or Modrinth first.

use crate::cf;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bookmark {
    /// CurseForge project id; 0 for a Modrinth project.
    pub id: u64,
    /// Modrinth project id; empty for a CurseForge project.
    pub modrinth: String,
    pub slug: String,
    /// CurseForge class (`cf::CLASS_*`); Modrinth projects map to the same.
    pub class: u32,
    pub name: String,
    pub icon: String,
    pub author: String,
    pub summary: String,
    /// The project's web page.
    pub page: String,
    pub downloads: f64,
    /// Unix seconds when it was saved.
    pub added: u64,
}

impl Bookmark {
    pub fn of(m: &cf::Mod, browsing: u32) -> Self {
        Bookmark {
            id: if m.modrinth.is_some() { 0 } else { m.id },
            modrinth: m.modrinth.clone().unwrap_or_default(),
            slug: m.slug.clone(),
            class: m.class_id.unwrap_or(browsing),
            name: m.name.clone(),
            icon: m.icon().unwrap_or_default().to_string(),
            author: m.author().to_string(),
            summary: m.summary.clone(),
            page: m.links.website_url.clone(),
            downloads: m.download_count,
            added: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        }
    }

    /// The id Browse uses for the project (Modrinth's through `num_id`).
    pub fn project_id(&self) -> u64 {
        if self.modrinth.is_empty() {
            self.id
        } else {
            crate::modrinth::num_id(&self.modrinth)
        }
    }

    /// The project as a search hit, enough to show and install it.
    pub fn to_mod(&self) -> cf::Mod {
        let mr = !self.modrinth.is_empty();
        cf::Mod {
            id: self.project_id(),
            name: self.name.clone(),
            slug: self.slug.clone(),
            summary: self.summary.clone(),
            download_count: self.downloads,
            class_id: Some(self.class),
            logo: (!self.icon.is_empty()).then(|| cf::Logo {
                thumbnail_url: self.icon.clone(),
                url: self.icon.clone(),
            }),
            authors: vec![cf::Author {
                name: self.author.clone(),
                url: if mr {
                    format!("https://modrinth.com/user/{}", self.author)
                } else {
                    String::new()
                },
                ..Default::default()
            }],
            links: cf::Links {
                website_url: self.page.clone(),
                ..Default::default()
            },
            allow_mod_distribution: mr.then_some(true),
            modrinth: mr.then(|| self.modrinth.clone()),
            ..Default::default()
        }
    }
}

/// Whether `b` is project `m`.
fn same(b: &Bookmark, m: &cf::Mod) -> bool {
    match &m.modrinth {
        Some(id) => b.modrinth == *id,
        None => b.modrinth.is_empty() && b.id == m.id,
    }
}

/// The bookmarks in one file.
pub struct Bookmarks {
    path: PathBuf,
    list: Vec<Bookmark>,
}

impl Bookmarks {
    pub fn at(path: PathBuf) -> Self {
        let list = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Bookmarks { path, list }
    }

    /// Newest first.
    pub fn list(&self) -> &[Bookmark] {
        &self.list
    }

    pub fn has(&self, m: &cf::Mod) -> bool {
        self.list.iter().any(|b| same(b, m))
    }

    /// Saves `m`, or removes it when it is saved. Returns whether it is
    /// saved now.
    pub fn toggle(&mut self, m: &cf::Mod, browsing: u32) -> anyhow::Result<bool> {
        let had = self.list.len();
        self.list.retain(|b| !same(b, m));
        let saved = self.list.len() == had;
        if saved {
            self.list.insert(0, Bookmark::of(m, browsing));
        }
        self.save()?;
        Ok(saved)
    }

    fn save(&self) -> anyhow::Result<()> {
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = self.path.with_extension("json.part");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&self.list)?)?;
        std::fs::rename(tmp, &self.path)?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

static SHARED: Mutex<Option<Bookmarks>> = Mutex::new(None);

/// Runs `f` on Hopper's own bookmarks (loaded on first use).
pub fn with<T>(f: impl FnOnce(&mut Bookmarks) -> T) -> T {
    let mut g = SHARED.lock().unwrap_or_else(|e| e.into_inner());
    let b = g.get_or_insert_with(|| {
        let path = crate::paths::dirs()
            .map(|d| d.data_dir().join("bookmarks.json"))
            .unwrap_or_else(|| PathBuf::from("bookmarks.json"));
        Bookmarks::at(path)
    });
    f(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-bookmarks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("bookmarks.json")
    }

    #[test]
    fn toggles_and_survives_a_reload() {
        let p = temp();
        let cf_mod = cf::Mod {
            id: 42,
            name: "Pack".into(),
            class_id: Some(cf::CLASS_MODPACKS),
            logo: Some(cf::Logo {
                thumbnail_url: "https://x/icon.png".into(),
                url: String::new(),
            }),
            ..Default::default()
        };
        let mr_mod = cf::Mod {
            id: crate::modrinth::num_id("AANobbMI"),
            name: "Sodium".into(),
            modrinth: Some("AANobbMI".into()),
            ..Default::default()
        };
        let mut b = Bookmarks::at(p.clone());
        assert!(b.toggle(&cf_mod, cf::CLASS_MODS).unwrap());
        assert!(b.toggle(&mr_mod, cf::CLASS_MODS).unwrap());
        assert!(b.has(&cf_mod) && b.has(&mr_mod));

        let mut again = Bookmarks::at(p.clone());
        assert_eq!(again.list().len(), 2);
        // Newest first; a Modrinth bookmark comes back as a Modrinth project.
        let m = again.list()[0].to_mod();
        assert_eq!(m.modrinth.as_deref(), Some("AANobbMI"));
        assert_eq!(m.id, mr_mod.id);
        assert_eq!(m.class_id, Some(cf::CLASS_MODS));
        let c = again.list()[1].to_mod();
        assert_eq!((c.id, c.class_id), (42, Some(cf::CLASS_MODPACKS)));
        assert_eq!(c.icon(), Some("https://x/icon.png"));

        assert!(!again.toggle(&cf_mod, cf::CLASS_MODS).unwrap());
        assert!(!Bookmarks::at(p.clone()).has(&cf_mod));
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }
}
