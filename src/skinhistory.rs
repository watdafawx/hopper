//! Skins the player has worn, kept as PNG files in Hopper's data folder so
//! one can be put back on later (Mojang keeps only the current one).

use crate::mcprofile::Variant;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// How many skins the history keeps.
pub const KEEP: usize = 24;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// File name in the history folder (the PNG's SHA-1).
    pub file: String,
    #[serde(default)]
    pub variant: Variant,
    /// ISO time it was last worn or added.
    #[serde(default)]
    pub added: String,
}

/// The history in one folder.
pub struct History {
    dir: PathBuf,
}

impl History {
    /// Hopper's own history folder.
    pub fn open() -> Option<Self> {
        crate::paths::dirs()
            .map(|d| Self::at(d.data_dir().join("skins")))
    }

    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn index(&self) -> PathBuf {
        self.dir.join("history.json")
    }

    /// Newest first.
    pub fn list(&self) -> Vec<Entry> {
        std::fs::read(self.index())
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<Entry>>(&b).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|e| self.path(e).is_file())
            .collect()
    }

    pub fn path(&self, e: &Entry) -> PathBuf {
        self.dir.join(&e.file)
    }

    fn save(&self, list: &[Entry]) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.index().with_extension("json.part");
        std::fs::write(&tmp, serde_json::to_vec_pretty(list)?)?;
        std::fs::rename(tmp, self.index())?;
        Ok(())
    }

    /// Keeps a skin PNG (moving it to the front when already kept) and
    /// returns its entry. The oldest beyond [`KEEP`] are dropped.
    pub fn remember(&self, png: &[u8], variant: Variant) -> anyhow::Result<Entry> {
        crate::mcprofile::check_skin_png(png)?;
        let file = format!("{}.png", sha1_smol::Sha1::from(png).digest());
        std::fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(&file);
        if !path.is_file() {
            std::fs::write(&path, png)?;
        }
        let entry = Entry {
            file: file.clone(),
            variant,
            added: crate::time::now_iso(),
        };
        let mut list = self.list();
        list.retain(|e| e.file != file);
        list.insert(0, entry.clone());
        for old in list.split_off(list.len().min(KEEP)) {
            let _ = std::fs::remove_file(self.path(&old));
        }
        self.save(&list)?;
        Ok(entry)
    }

    /// Takes a skin out of the history and deletes its file.
    pub fn forget(&self, file: &str) -> anyhow::Result<()> {
        let mut list = self.list();
        list.retain(|e| e.file != file);
        self.save(&list)?;
        if !file.contains(['/', '\\']) {
            let _ = std::fs::remove_file(self.dir.join(file));
        }
        Ok(())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skin(seed: u8) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(64, 64, image::Rgba([seed, 0, 0, 255]));
        crate::skinrender::png_bytes(&img)
    }

    #[test]
    fn keeps_newest_first_without_duplicates() {
        let dir = std::env::temp_dir().join(format!("hopper-skins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let h = History::at(dir.clone());
        let a = h.remember(&skin(1), Variant::Classic).unwrap();
        let b = h.remember(&skin(2), Variant::Slim).unwrap();
        assert_eq!(h.list().iter().map(|e| &e.file).collect::<Vec<_>>(), [&b.file, &a.file]);
        h.remember(&skin(1), Variant::Slim).unwrap();
        let l = h.list();
        assert_eq!(l.len(), 2);
        assert_eq!((l[0].file.as_str(), l[0].variant), (a.file.as_str(), Variant::Slim));
        assert!(h.path(&l[0]).is_file());
        h.forget(&a.file).unwrap();
        assert_eq!(h.list().len(), 1);
        assert!(!dir.join(&a.file).exists());
        assert!(h.remember(b"not a png", Variant::Classic).is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn drops_the_oldest_past_the_limit() {
        let dir = std::env::temp_dir().join(format!("hopper-skins-max-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let h = History::at(dir.clone());
        let first = h.remember(&skin(0), Variant::Classic).unwrap();
        for i in 1..=KEEP as u8 {
            h.remember(&skin(i), Variant::Classic).unwrap();
        }
        let l = h.list();
        assert_eq!(l.len(), KEEP);
        assert!(l.iter().all(|e| e.file != first.file));
        assert!(!dir.join(&first.file).exists());
        std::fs::remove_dir_all(dir).ok();
    }
}
