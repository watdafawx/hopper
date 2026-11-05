//! A small on-disk cache of something worked out from a file, kept by the
//! file's path, size and modified time: a file that changes is worked out
//! again, the rest come from the cache. Used for what each jar's mods are
//! and for each config file's setting counts, so opening the Configs tab
//! after a restart doesn't open every jar and parse every config again.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A file's size and modified time (nanoseconds since 1970, 0 when the
/// system doesn't say).
pub type Stamp = (u64, u128);

pub fn stamp(meta: &std::fs::Metadata) -> Stamp {
    let at = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    (meta.len(), at)
}

#[derive(Serialize, Deserialize)]
#[serde(bound = "V: Serialize + DeserializeOwned")]
pub struct StampCache<V> {
    /// The Hopper version that wrote it: a newer one may read files
    /// differently, so it starts over rather than trust old answers.
    version: String,
    files: HashMap<String, (u64, u128, V)>,
    #[serde(skip)]
    dirty: bool,
    /// Keys used this run, the ones kept when the file grows past `cap`.
    #[serde(skip)]
    seen: HashSet<String>,
    #[serde(skip)]
    cap: usize,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl<V: Clone + Serialize + DeserializeOwned> StampCache<V> {
    /// The cache in `name` under Hopper's cache folder (none when there's
    /// no such folder: it then only lives in memory).
    pub fn named(name: &str, cap: usize) -> StampCache<V> {
        Self::load(crate::paths::dirs().map(|d| d.cache_dir().join(name)), cap)
    }

    pub fn load(path: Option<PathBuf>, cap: usize) -> StampCache<V> {
        let version = env!("CARGO_PKG_VERSION");
        let mut c = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<StampCache<V>>(&b).ok())
            .filter(|c| c.version == version)
            .unwrap_or_else(|| StampCache { version: version.into(), files: HashMap::new(), dirty: false, seen: HashSet::new(), cap, path: None });
        c.cap = cap;
        c.path = path;
        c
    }

    pub fn get(&mut self, key: &str, stamp: Stamp) -> Option<V> {
        let (len, at, v) = self.files.get(key)?;
        if (*len, *at) != stamp {
            return None;
        }
        let v = v.clone();
        if !self.seen.contains(key) {
            self.seen.insert(key.to_string());
        }
        Some(v)
    }

    pub fn put(&mut self, key: String, stamp: Stamp, v: V) {
        self.seen.insert(key.clone());
        self.files.insert(key, (stamp.0, stamp.1, v));
        self.dirty = true;
    }

    /// Writes the file when something new went in: to a temp file first,
    /// so Hopper and Hopper Server sharing one never read half a file.
    pub fn save(&mut self) {
        let Some(p) = self.path.clone() else { return };
        if !self.dirty {
            return;
        }
        if self.files.len() > self.cap {
            let seen = std::mem::take(&mut self.seen);
            self.files.retain(|k, _| seen.contains(k));
            self.seen = seen;
        }
        let Ok(b) = serde_json::to_vec(self) else { return };
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = p.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&tmp, b).is_ok() && std::fs::rename(&tmp, &p).is_ok() {
            self.dirty = false;
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }
}

/// The stamp of each of `paths`, from one listing per folder (`None`: not
/// a file there). Asking about each file on its own takes far longer on
/// Windows (5,000 files: a fifth of a second), and a folder's listing
/// already carries both.
pub fn by_folder(paths: &[PathBuf]) -> Vec<Option<Stamp>> {
    let mut dirs: Vec<&Path> = paths.iter().filter_map(|p| p.parent()).collect();
    dirs.sort_unstable();
    dirs.dedup();
    let listed: HashMap<&Path, HashMap<std::ffi::OsString, Stamp>> = dirs
        .iter()
        .copied()
        .zip(crate::configindex::par_map(&dirs, |d| {
            let mut m = HashMap::new();
            for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
                if let Ok(meta) = e.metadata()
                    && meta.is_file()
                {
                    m.insert(e.file_name(), stamp(&meta));
                }
            }
            m
        }))
        .collect();
    paths.iter().map(|p| listed.get(p.parent()?)?.get(p.file_name()?).copied()).collect()
}

/// `path` as a cache key.
pub fn key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_by_stamp_and_version() {
        let dir = std::env::temp_dir().join(format!("hopper-stampcache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("c.json");
        let mut c: StampCache<String> = StampCache::load(Some(file.clone()), 2);
        c.put("a".into(), (1, 2), "one".into());
        c.put("b".into(), (3, 4), "two".into());
        c.save();
        let mut c: StampCache<String> = StampCache::load(Some(file.clone()), 2);
        assert_eq!(c.get("a", (1, 2)).as_deref(), Some("one"));
        assert_eq!(c.get("a", (1, 3)), None, "a changed file is read again");
        // Past the cap, only what this run used stays.
        c.put("c".into(), (5, 6), "three".into());
        c.save();
        let mut c: StampCache<String> = StampCache::load(Some(file.clone()), 2);
        assert_eq!(c.len(), 2);
        assert_eq!(c.get("b", (3, 4)), None);
        // Another version's file isn't trusted.
        std::fs::write(&file, r#"{"version":"0.0.1","files":{"a":[1,2,"old"]}}"#).unwrap();
        let mut c: StampCache<String> = StampCache::load(Some(file), 2);
        assert_eq!(c.get("a", (1, 2)), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
