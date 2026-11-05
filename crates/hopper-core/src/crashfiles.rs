//! Deleting crash reports and old logs from a server's or game's folder.
//! Only plain files directly inside `crash-reports/` and `logs/` can go,
//! whatever path a caller (a browser, another PC) sends, and they go to the
//! trash (the Recycle Bin, or Hopper Server's trash), never for good.

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// The two folders whose files can be deleted here.
const DIRS: [&str; 2] = ["crash-reports", "logs"];

/// The logs a running game or server is writing.
const CURRENT: [&str; 2] = ["latest.log", "debug.log"];

/// What "Clear all" clears.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Crashes,
    Logs,
}

impl Kind {
    fn dir(self) -> &'static str {
        match self {
            Kind::Crashes => "crash-reports",
            Kind::Logs => "logs",
        }
    }

    /// From the word the API sends.
    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "crashes" => Some(Kind::Crashes),
            "logs" => Some(Kind::Logs),
            _ => None,
        }
    }
}

/// `rel` as "crash-reports/name" or "logs/name": one level under one of the
/// two folders, no `..`, no drive letters, no folders of its own.
pub fn clean_rel(rel: &str) -> Result<String> {
    let rel = rel.replace('\\', "/");
    let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    let [dir, name] = parts.as_slice() else { bail!("That isn't a log or crash report.") };
    if !DIRS.contains(dir) || *name == ".." || name.contains(':') || name.starts_with('.') {
        bail!("That isn't a log or crash report.");
    }
    Ok(format!("{dir}/{name}"))
}

/// Whether the file is one a running game or server is writing.
pub fn in_use(rel: &str, running: bool) -> bool {
    running && rel.strip_prefix("logs/").is_some_and(|n| CURRENT.contains(&n))
}

/// The real path of `rel` in `root`, when it's a plain file there (a
/// symlink or junction that leads out of the folder is refused).
fn locate(root: &Path, rel: &str) -> Result<PathBuf> {
    let rel = clean_rel(rel)?;
    let p = root.join(&rel);
    let meta = std::fs::symlink_metadata(&p).map_err(|_| anyhow::anyhow!("{rel} isn't there any more."))?;
    if !meta.is_file() {
        bail!("{rel} isn't a file.");
    }
    let (dir, canon) = (std::fs::canonicalize(p.parent().unwrap_or(root))?, std::fs::canonicalize(root)?);
    if !dir.starts_with(&canon) {
        bail!("That isn't a log or crash report.");
    }
    Ok(p)
}

/// Deletes the named files (to the trash). Nothing is deleted when one is
/// refused: a file a running game or server is using, or not a log or report.
pub fn delete(root: &Path, rels: &[String], running: bool) -> Result<usize> {
    let mut files = Vec::new();
    for r in rels {
        let rel = clean_rel(r)?;
        if in_use(&rel, running) {
            bail!("{} is in use while it runs. Stop it first.", rel.trim_start_matches("logs/"));
        }
        files.push(locate(root, &rel)?);
    }
    if files.is_empty() {
        return Ok(0);
    }
    crate::retention::dispose(root, &files)?;
    Ok(files.len())
}

/// The files "Clear all" would delete: every crash report, or every log but
/// the current latest.log / debug.log.
pub fn clearable(root: &Path, kind: Kind) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(root.join(kind.dir())) else { return Vec::new() };
    let mut out: Vec<String> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| format!("{}/{}", kind.dir(), e.file_name().to_string_lossy()))
        .filter(|rel| clean_rel(rel).is_ok())
        .filter(|rel| !(kind == Kind::Logs && CURRENT.contains(&rel.trim_start_matches("logs/"))))
        .collect();
    out.sort();
    out
}

/// "Clear all": deletes [`clearable`]; returns how many went.
pub fn clear(root: &Path, kind: Kind) -> Result<usize> {
    delete(root, &clearable(root, kind), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_files_one_level_under_the_two_folders_pass() {
        assert_eq!(clean_rel("crash-reports/crash-1.txt").unwrap(), "crash-reports/crash-1.txt");
        assert_eq!(clean_rel("logs\\2026-11-04-1.log.gz").unwrap(), "logs/2026-11-04-1.log.gz");
        assert_eq!(clean_rel("./logs//latest.log").unwrap(), "logs/latest.log");
        for bad in [
            "",
            "logs",
            "logs/",
            "../logs/latest.log",
            "logs/../server.properties",
            "logs/../../x",
            "crash-reports/sub/a.txt",
            "mods/a.jar",
            "world/level.dat",
            "/etc/passwd",
            "C:/Windows/x.log",
            "logs/C:x",
            "logs/.hidden",
            "server.properties",
            "logsx/a.log",
        ] {
            assert!(clean_rel(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn a_running_servers_current_logs_stay() {
        assert!(in_use("logs/latest.log", true));
        assert!(in_use("logs/debug.log", true));
        assert!(!in_use("logs/latest.log", false));
        assert!(!in_use("logs/2026-11-04-1.log.gz", true));
        assert!(!in_use("crash-reports/latest.log", true));
    }

    fn scratch(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("hopper-crashfiles-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(p.join("crash-reports")).unwrap();
        std::fs::create_dir_all(p.join("logs")).unwrap();
        std::fs::write(p.join("server.properties"), "a=b").unwrap();
        for f in ["crash-reports/a.txt", "crash-reports/b.txt", "logs/latest.log", "logs/debug.log", "logs/old.log.gz"] {
            std::fs::write(p.join(f), "x").unwrap();
        }
        p
    }

    #[test]
    fn a_refused_path_deletes_nothing() {
        let root = scratch("refuse");
        let r = delete(&root, &["crash-reports/a.txt".into(), "../server.properties".into()], false);
        assert!(r.is_err());
        assert!(root.join("crash-reports/a.txt").is_file(), "the good path must not go when another is refused");
        assert!(root.join("server.properties").is_file());
        assert!(delete(&root, &["crash-reports/nope.txt".into()], false).is_err());
        assert!(delete(&root, &["logs/latest.log".into()], true).is_err());
        assert!(root.join("logs/latest.log").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn clear_all_lists_reports_and_old_logs_only() {
        let root = scratch("clear");
        assert_eq!(clearable(&root, Kind::Crashes), ["crash-reports/a.txt", "crash-reports/b.txt"]);
        assert_eq!(clearable(&root, Kind::Logs), ["logs/old.log.gz"]);
        assert_eq!(Kind::parse("crashes"), Some(Kind::Crashes));
        assert_eq!(Kind::parse("mods"), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_folder_is_refused() {
        let root = scratch("link");
        let outside = std::env::temp_dir().join(format!("hopper-crashfiles-out-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("a.log"), "x").unwrap();
        std::fs::remove_dir_all(root.join("logs")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("logs")).unwrap();
        assert!(delete(&root, &["logs/a.log".into()], false).is_err());
        assert!(outside.join("a.log").is_file());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
