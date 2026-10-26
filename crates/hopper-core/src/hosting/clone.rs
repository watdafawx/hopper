//! Copies of a hosted server: a clone (the whole folder, with or without
//! its world, on its own port) and templates (mods, configs and start
//! files without a world or players, kept apart for New server to start
//! from).
//!
//! Never copied: backups, logs, crash reports, Hopper's working folders,
//! the address and the schedule (both belong to the original), and the
//! files of a running server (`session.lock`, the console).

use super::backup;
use super::properties::Properties;
use crate::jobs::Job;
use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Left out of every copy (first path part, or a file in the root).
pub const NOT_COPIED: &[&str] = &[
    backup::DIR,
    ".hopper-trash",
    ".hopper-uploads",
    ".hopper-backups",
    ".hopper-staging",
    ".hopper-tmp",
    "logs",
    "crash-reports",
    "debug",
    super::router::FILE,
    ".hopper-schedule.json",
    super::process::CONSOLE_FILE,
    super::process::RUN_FILE,
    ".hopper-crash.json",
    super::version_change::RECORD,
];

/// Also left out of templates: who plays there, and the EULA (the person
/// making a server from the template agrees to it again).
const PLAYER_FILES: &[&str] = &[
    "ops.json",
    "whitelist.json",
    "banned-players.json",
    "banned-ips.json",
    "usercache.json",
    super::EULA_FILE,
];

/// A template's own description, in its folder.
pub const TEMPLATE_META: &str = "hopper-template.json";

/// Sizes a copy would have, in bytes.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Sizes {
    pub total: u64,
    pub world: u64,
    pub files: usize,
}

fn rel_of(root: &Path, p: &Path) -> Option<String> {
    Some(p.strip_prefix(root).ok()?.to_string_lossy().replace('\\', "/"))
}

/// What a copy of `root` takes. `skip_world` / `skip_players`: as in
/// [`copy_server`].
fn wanted(rel: &str, worlds: &[String], skip_world: bool, skip_players: bool) -> bool {
    let first = rel.split('/').next().unwrap_or("");
    if NOT_COPIED.contains(&first) || rel.ends_with("session.lock") {
        return false;
    }
    if skip_world && worlds.iter().any(|w| w == first) {
        return false;
    }
    if skip_players && !rel.contains('/') && PLAYER_FILES.contains(&rel) {
        return false;
    }
    !(rel == TEMPLATE_META)
}

/// Every file a copy would take, with sizes.
fn files(root: &Path, skip_world: bool, skip_players: bool) -> Vec<(String, u64)> {
    let worlds = backup::worlds(root);
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Some(rel) = rel_of(root, &p) else { continue };
            if !wanted(&rel, &worlds, skip_world, skip_players) {
                continue;
            }
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(p),
                Ok(t) if t.is_file() => out.push((rel, e.metadata().map_or(0, |m| m.len()))),
                _ => {}
            }
        }
    }
    out.sort();
    out
}

/// How big a clone of `root` is, and how much of it is the world.
pub fn sizes(root: &Path) -> Sizes {
    let all = files(root, false, false);
    let worlds = backup::worlds(root);
    let world = all
        .iter()
        .filter(|(r, _)| worlds.iter().any(|w| r.split('/').next() == Some(w.as_str())))
        .map(|(_, n)| n)
        .sum();
    Sizes { total: all.iter().map(|(_, n)| n).sum(), world, files: all.len() }
}

/// Copies `src` to the new folder `dst` (which must not exist, or be
/// empty), leaving out [`NOT_COPIED`], the world when `skip_world`, and
/// player lists and the EULA when `skip_players`. Removes what it made if
/// a step fails. Returns the number of files.
fn copy_server(src: &Path, dst: &Path, skip_world: bool, skip_players: bool, job: &Job) -> anyhow::Result<usize> {
    if dst.exists() && fs::read_dir(dst).map(|mut d| d.next().is_some()).unwrap_or(true) {
        bail!("{} already exists.", dst.display());
    }
    let list = files(src, skip_world, skip_players);
    let total: u64 = list.iter().map(|(_, n)| n).sum::<u64>().max(1);
    let existed = dst.exists();
    let r = (|| -> anyhow::Result<usize> {
        fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
        let mut done = 0u64;
        for (i, (rel, n)) in list.iter().enumerate() {
            job.check_cancelled()?;
            if i % 50 == 0 {
                job.detail(format!("Copying ({} of {} MB)", done >> 20, total >> 20));
                job.progress((done >> 10) as usize, (total >> 10) as usize);
            }
            let to = dst.join(rel);
            if let Some(p) = to.parent() {
                fs::create_dir_all(p)?;
            }
            // A file another program holds open is skipped, not fatal.
            if let Err(e) = fs::copy(src.join(rel), &to) {
                log::warn!("copy: skipped {rel}: {e}");
                continue;
            }
            done += n;
        }
        Ok(list.len())
    })();
    if r.is_err() && !existed {
        let _ = fs::remove_dir_all(dst);
    }
    r
}

/// Points server.properties at `port` (and query.port, when it was the
/// same as the game port).
pub fn set_port(root: &Path, port: u16) -> anyhow::Result<()> {
    let path = root.join("server.properties");
    let mut p = Properties::load(&path).unwrap_or_default();
    let old = p.get("server-port").map(str::to_string);
    if old.is_some() && p.get("query.port").map(str::to_string) == old {
        p.set("query.port", &port.to_string());
    }
    p.set("server-port", &port.to_string());
    p.save(&path)
}

/// Clones the stopped server in `src` into `dst` on `port`. `with_world`
/// false leaves the world out, so the clone makes a new one on its first
/// start. Players lists are copied.
pub fn clone_server(src: &Path, dst: &Path, with_world: bool, port: u16, job: &Job) -> anyhow::Result<usize> {
    let n = copy_server(src, dst, !with_world, false, job)?;
    if let Err(e) = set_port(dst, port) {
        let _ = fs::remove_dir_all(dst);
        return Err(e);
    }
    Ok(n)
}

/// A saved template.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Template {
    /// Its folder's name; its id.
    pub id: String,
    pub name: String,
    /// The server it was saved from.
    pub from: String,
    pub minecraft: String,
    /// "NeoForge 21.1.265", "Vanilla", "".
    pub loader: String,
    pub mods: usize,
    pub size: u64,
    /// The heap its start files give java (MB); 0 when they don't say.
    pub memory_mb: u32,
    /// Unix seconds.
    pub at: u64,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn jar_count(dir: &Path) -> usize {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().to_ascii_lowercase().ends_with(".jar"))
        .count()
}

/// Saves `src` (named `from`) as template `name` under `dir`: everything
/// but the world, players and the EULA.
pub fn save_template(src: &Path, from: &str, dir: &Path, name: &str, job: &Job) -> anyhow::Result<Template> {
    let name = name.trim();
    anyhow::ensure!(!name.is_empty(), "Give the template a name.");
    let id = crate::instances::Instance::folder_name(name);
    anyhow::ensure!(!id.is_empty(), "Give the template a name with letters or digits.");
    let dst = dir.join(&id);
    if dst.exists() {
        bail!("There is a template called {name} already.");
    }
    copy_server(src, &dst, true, true, job)?;
    let have = super::version_change::installed(src);
    let t = Template {
        id,
        name: name.to_string(),
        from: from.to_string(),
        minecraft: have.minecraft.clone(),
        loader: have.label(),
        mods: jar_count(&dst.join("mods")) + jar_count(&dst.join("plugins")),
        size: files(&dst, false, false).iter().map(|(_, n)| n).sum(),
        memory_mb: super::jvm::JvmArgs::load(&dst).ok().and_then(|a| a.xmx_mb).unwrap_or(0),
        at: now(),
    };
    fs::write(dst.join(TEMPLATE_META), serde_json::to_vec_pretty(&t)?)?;
    Ok(t)
}

/// The templates under `dir`, newest first.
pub fn templates(dir: &Path) -> Vec<Template> {
    let mut out: Vec<Template> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let mut t: Template = serde_json::from_slice(&fs::read(e.path().join(TEMPLATE_META)).ok()?).ok()?;
            t.id = e.file_name().to_string_lossy().into_owned();
            Some(t)
        })
        .collect();
    out.sort_by_key(|t| std::cmp::Reverse(t.at));
    out
}

/// A template's folder, if `id` names one under `dir`.
pub fn template_dir(dir: &Path, id: &str) -> anyhow::Result<PathBuf> {
    if id.is_empty() || id.contains(['/', '\\', ':']) || id.contains("..") {
        bail!("That isn't a template.");
    }
    let p = dir.join(id);
    anyhow::ensure!(p.join(TEMPLATE_META).is_file(), "There's no template {id}.");
    Ok(p)
}

/// Deletes a template.
pub fn delete_template(dir: &Path, id: &str) -> anyhow::Result<()> {
    let p = template_dir(dir, id)?;
    fs::remove_dir_all(&p).with_context(|| format!("deleting {}", p.display()))
}

/// A new server in `dst` from template `id`, on `port`. `accept_eula`
/// only when the person agreed to the Minecraft EULA.
pub fn from_template(dir: &Path, id: &str, dst: &Path, port: u16, motd: &str, accept_eula: bool, job: &Job) -> anyhow::Result<usize> {
    let src = template_dir(dir, id)?;
    let n = copy_server(&src, dst, false, false, job)?;
    let r = (|| -> anyhow::Result<()> {
        set_port(dst, port)?;
        if !motd.trim().is_empty() {
            let path = dst.join("server.properties");
            let mut p = Properties::load(&path).unwrap_or_default();
            p.set("motd", motd.trim());
            p.save(&path)?;
        }
        super::write_eula(dst, accept_eula)
    })();
    if let Err(e) = r {
        let _ = fs::remove_dir_all(dst);
        return Err(e);
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-clone-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn server(root: &Path) {
        fs::write(root.join("server.properties"), "level-name=world\nserver-port=25630\nquery.port=25630\nmotd=Old\n").unwrap();
        for d in ["world/region", "world_nether", "mods", "config", "logs", backup::DIR, ".hopper-backups"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        fs::write(root.join("world/level.dat"), "level").unwrap();
        fs::write(root.join("world/session.lock"), "lock").unwrap();
        fs::write(root.join("world/region/r.0.0.mca"), vec![1u8; 4096]).unwrap();
        fs::write(root.join("world_nether/x.dat"), "n").unwrap();
        fs::write(root.join("mods/a.jar"), "jar").unwrap();
        fs::write(root.join("config/a.toml"), "a = 1").unwrap();
        fs::write(root.join("logs/latest.log"), "log").unwrap();
        fs::write(root.join(backup::DIR).join("b.zip"), "zip").unwrap();
        fs::write(root.join(super::super::router::FILE), "{}").unwrap();
        fs::write(root.join(".hopper-schedule.json"), "{}").unwrap();
        fs::write(root.join("ops.json"), "[]").unwrap();
        fs::write(root.join("eula.txt"), "eula=true").unwrap();
        fs::write(root.join("run.bat"), "java @user_jvm_args.txt -jar server.jar nogui").unwrap();
    }

    #[test]
    fn clone_with_and_without_world() {
        let base = temp("c");
        let src = base.join("Survival");
        fs::create_dir_all(&src).unwrap();
        server(&src);
        let job = crate::jobs::Jobs::default().start("t");
        let s = sizes(&src);
        assert_eq!(s.world, 5 + 4096 + 1);
        assert!(s.total > s.world);

        let a = base.join("A");
        clone_server(&src, &a, true, 25631, &job).unwrap();
        assert!(a.join("world/level.dat").is_file() && a.join("world_nether/x.dat").is_file());
        assert!(!a.join("world/session.lock").exists());
        assert!(a.join("mods/a.jar").is_file() && a.join("ops.json").is_file() && a.join("eula.txt").is_file());
        for gone in ["logs", backup::DIR, ".hopper-backups", super::super::router::FILE, ".hopper-schedule.json"] {
            assert!(!a.join(gone).exists(), "{gone} was copied");
        }
        let p = Properties::load(&a.join("server.properties")).unwrap();
        assert_eq!((p.get("server-port"), p.get("query.port"), p.get("motd")), (Some("25631"), Some("25631"), Some("Old")));
        // The original is untouched.
        assert_eq!(Properties::load(&src.join("server.properties")).unwrap().get("server-port"), Some("25630"));

        let b = base.join("B");
        clone_server(&src, &b, false, 25632, &job).unwrap();
        assert!(!b.join("world").exists() && !b.join("world_nether").exists());
        assert!(b.join("config/a.toml").is_file());
        // Not over an existing folder.
        assert!(clone_server(&src, &b, false, 25633, &job).is_err());
        assert!(b.join("config/a.toml").is_file());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn templates_round_trip() {
        let base = temp("t");
        let src = base.join("Survival");
        fs::create_dir_all(&src).unwrap();
        server(&src);
        let dir = base.join("templates");
        let job = crate::jobs::Jobs::default().start("t");
        let t = save_template(&src, "Survival", &dir, "Base pack", &job).unwrap();
        assert_eq!((t.id.as_str(), t.mods), ("Base pack", 1));
        let td = dir.join(&t.id);
        assert!(!td.join("world").exists() && !td.join("ops.json").exists() && !td.join("eula.txt").exists());
        assert!(td.join("mods/a.jar").is_file() && td.join(TEMPLATE_META).is_file());
        assert!(save_template(&src, "Survival", &dir, "Base pack", &job).is_err());
        assert_eq!(templates(&dir), vec![t.clone()]);

        let new = base.join("servers/New");
        from_template(&dir, &t.id, &new, 25640, "Hello", true, &job).unwrap();
        assert!(super::super::eula_accepted(&new));
        assert!(!new.join(TEMPLATE_META).exists() && new.join("mods/a.jar").is_file());
        let p = Properties::load(&new.join("server.properties")).unwrap();
        assert_eq!((p.get("server-port"), p.get("motd")), (Some("25640"), Some("Hello")));

        assert!(template_dir(&dir, "../Survival").is_err());
        delete_template(&dir, &t.id).unwrap();
        assert!(templates(&dir).is_empty());
        let _ = fs::remove_dir_all(&base);
    }
}
