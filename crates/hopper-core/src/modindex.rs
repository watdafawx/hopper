//! What each jar in a mods folder holds, for crash help: its class
//! packages, the namespaces of its assets and data (`assets/<ns>/`,
//! `data/<ns>/`) and its mixin configs. A crash names these (a frame's
//! class, `mythsandlegends:graveyard`, `securitycraft.mixins.json`), and
//! this says which jar they come from.
//!
//! Only file names are read (the zip's directory, nothing is unpacked).
//! Built when a crash is read, never at launch, and kept beside the
//! instance in [`FILE`], keyed by each jar's size and modified time: a jar
//! added, updated or removed is read again on the next crash, the rest
//! come from the file.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

/// Kept beside the instance (or server).
pub const FILE: &str = ".hopper-modindex.json";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Jar {
    file: String,
    size: u64,
    modified: i64,
    packages: Vec<String>,
    namespaces: Vec<String>,
    mixins: Vec<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    jars: Vec<Jar>,
}

/// One owner per name, or `None` when more than one jar has it (a shaded
/// library, an addon that adds to its parent's namespace).
type Owners = HashMap<String, Option<String>>;

#[derive(Default)]
pub struct ModIndex {
    /// Jars that are on.
    packages: Owners,
    namespaces: Owners,
    mixins: Owners,
    /// The same for jars turned off: a mod turned off after the crash is
    /// still the one it names. Asked only when no jar that's on has it.
    off_packages: Owners,
    off_namespaces: Owners,
}

fn own(map: &mut Owners, key: &str, jar: &str) {
    map.entry(key.to_string())
        .and_modify(|o| {
            if o.as_deref() != Some(jar) {
                *o = None;
            }
        })
        .or_insert_with(|| Some(jar.to_string()));
}

fn stamp(p: &Path) -> Option<(u64, i64)> {
    let m = fs::metadata(p).ok()?;
    let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    Some((m.len(), t))
}

/// The file names in one jar, sorted into what [`ModIndex`] keeps.
fn read_jar(dir: &Path, file: &str) -> Option<Jar> {
    let p = dir.join(file);
    let (size, modified) = stamp(&p)?;
    let z = zip::ZipArchive::new(fs::File::open(&p).ok()?).ok()?;
    let (mut packages, mut namespaces, mut mixins) = (HashSet::new(), HashSet::new(), HashSet::new());
    for n in z.file_names() {
        if n.ends_with(".class") {
            if !n.starts_with("META-INF/")
                && let Some((pkg, _)) = n.rsplit_once('/')
            {
                packages.insert(pkg.replace('/', "."));
            }
            continue;
        }
        let mut parts = n.split('/');
        match (parts.next(), parts.next(), parts.next()) {
            (Some("assets" | "data"), Some(ns), Some(_)) if !ns.is_empty() => {
                namespaces.insert(ns.to_string());
            }
            (Some(top), None, None) if top.ends_with(".json") && top.to_ascii_lowercase().contains("mixin") => {
                mixins.insert(top.to_string());
            }
            _ => {}
        }
    }
    let sorted = |s: HashSet<String>| {
        let mut v: Vec<String> = s.into_iter().collect();
        v.sort();
        v
    };
    Some(Jar { file: file.to_string(), size, modified, packages: sorted(packages), namespaces: sorted(namespaces), mixins: sorted(mixins) })
}

impl ModIndex {
    /// The index of the jars in `root`'s mods folder named in `mods`,
    /// reading only jars that changed since [`FILE`] was written.
    pub fn load(root: &Path, mods: &[String]) -> Self {
        Self::load_dir(root, &root.join("mods"), mods)
    }

    /// The same for a mods folder `dir`, with the cache in `root`.
    pub fn load_dir(root: &Path, dir: &Path, mods: &[String]) -> Self {
        let cache = root.join(FILE);
        let saved: Saved = fs::read(&cache).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let by_file: HashMap<&str, &Jar> = saved.jars.iter().map(|j| (j.file.as_str(), j)).collect();
        let wanted: Vec<&String> = mods.iter().filter(|m| m.ends_with(".jar") || m.ends_with(".jar.disabled")).collect();
        let mut jars: Vec<Jar> = Vec::with_capacity(wanted.len());
        let mut todo: Vec<&String> = Vec::new();
        for m in &wanted {
            match (by_file.get(m.as_str()), stamp(&dir.join(m))) {
                (Some(j), Some((size, modified))) if j.size == size && j.modified == modified => jars.push((*j).clone()),
                _ => todo.push(m),
            }
        }
        let changed = !todo.is_empty() || jars.len() != saved.jars.len();
        // Big packs have a thousand jars: a few threads read them.
        if !todo.is_empty() {
            let chunk = todo.len().div_ceil(8).max(1);
            let read: Vec<Jar> = std::thread::scope(|s| {
                let parts: Vec<_> =
                    todo.chunks(chunk).map(|c| s.spawn(move || c.iter().filter_map(|m| read_jar(dir, m)).collect::<Vec<_>>())).collect();
                parts.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
            });
            jars.extend(read);
        }
        if changed {
            jars.sort_by(|a, b| a.file.cmp(&b.file));
            if let Ok(b) = serde_json::to_vec(&Saved { jars: jars.clone() }) {
                let _ = fs::write(&cache, b);
            }
        }
        let mut idx = Self::default();
        for j in &jars {
            let on = j.file.ends_with(".jar");
            for p in &j.packages {
                own(if on { &mut idx.packages } else { &mut idx.off_packages }, p, &j.file);
            }
            for n in &j.namespaces {
                own(if on { &mut idx.namespaces } else { &mut idx.off_namespaces }, n, &j.file);
            }
            if on {
                for m in &j.mixins {
                    own(&mut idx.mixins, m, &j.file);
                }
            }
        }
        idx
    }

    /// The jar a class (`com.x.Y$Z`) is in.
    pub fn jar_of_class(&self, class: &str) -> Option<&str> {
        let pkg = class.rsplit_once('.')?.0;
        match self.packages.get(pkg) {
            Some(o) => o.as_deref(),
            None => self.off_packages.get(pkg)?.as_deref(),
        }
    }

    /// The one jar with assets or data in namespace `ns`.
    pub fn jar_of_namespace(&self, ns: &str) -> Option<&str> {
        match self.namespaces.get(ns) {
            Some(o) => o.as_deref(),
            None => self.off_namespaces.get(ns)?.as_deref(),
        }
    }

    /// The jar with this mixin config (`x.mixins.json`).
    pub fn jar_of_mixin_config(&self, name: &str) -> Option<&str> {
        self.mixins.get(name)?.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn jar(p: &Path, names: &[&str]) {
        let mut z = zip::ZipWriter::new(fs::File::create(p).unwrap());
        for n in names {
            z.start_file::<_, ()>(*n, Default::default()).unwrap();
            z.write_all(b"x").unwrap();
        }
        z.finish().unwrap();
    }

    #[test]
    fn names_map_to_one_jar_and_the_cache_follows_changes() {
        let root = std::env::temp_dir().join(format!("hopper-modindex-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("mods");
        fs::create_dir_all(&dir).unwrap();
        jar(&dir.join("a.jar"), &["com/a/A.class", "assets/alpha/x.json", "data/shared/y.json", "alpha.mixins.json"]);
        jar(&dir.join("b.jar"), &["com/b/B.class", "assets/shared/x.json", "com/lib/L.class"]);
        jar(&dir.join("c.jar.disabled"), &["com/lib/L.class", "assets/gamma/x.json"]);
        let mods = vec!["a.jar".to_string(), "b.jar".to_string(), "c.jar.disabled".to_string()];
        let idx = ModIndex::load(&root, &mods);
        assert_eq!(idx.jar_of_class("com.a.A$Inner"), Some("a.jar"));
        // On beats off; two jars that are on share none.
        assert_eq!(idx.jar_of_class("com.lib.L"), Some("b.jar"));
        assert_eq!(idx.jar_of_namespace("alpha"), Some("a.jar"));
        assert_eq!(idx.jar_of_namespace("shared"), None);
        assert_eq!(idx.jar_of_namespace("gamma"), Some("c.jar.disabled"));
        assert_eq!(idx.jar_of_mixin_config("alpha.mixins.json"), Some("a.jar"));
        assert!(root.join(FILE).is_file());

        // A jar replaced is read again; the cache isn't stale.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        jar(&dir.join("a.jar"), &["com/a2/A.class", "assets/alpha/x.json"]);
        let idx = ModIndex::load(&root, &mods);
        assert_eq!(idx.jar_of_class("com.a.A"), None);
        assert_eq!(idx.jar_of_class("com.a2.A"), Some("a.jar"));
        assert_eq!(idx.jar_of_mixin_config("alpha.mixins.json"), None);
        let _ = fs::remove_dir_all(&root);
    }
}
