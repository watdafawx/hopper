//! How each folder besides `mods` syncs with a server, ported from
//! mod-sync's sync rules: review file by file (the default), mirror
//! everything except some patterns, mirror only matching files, or a live
//! link (an NTFS junction, local disks only). Mirrors go through
//! `sync::execute`, so every change is backed up and can be undone from
//! History, unlike mod-sync's robocopy.

use crate::model::SyncOp;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    /// Per-file diff in the Configs view; nothing syncs by itself.
    #[default]
    Review,
    /// Mirror everything except files matching the patterns (left alone).
    MirrorExcept,
    /// Mirror only files matching the patterns.
    MirrorOnly,
    /// The destination folder becomes a junction to the source.
    Link,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Review => "Review changes",
            Mode::MirrorExcept => "Mirror all except…",
            Mode::MirrorOnly => "Mirror only…",
            Mode::Link => "Live link",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rule {
    pub folder: String,
    pub mode: Mode,
    /// Comma-separated globs (`*` and `?`) or plain substrings.
    pub patterns: String,
    /// Server → client (worlds, backups) instead of client → server.
    pub pull: bool,
    /// The folder a live link moved aside, put back on unlink.
    pub displaced: Option<PathBuf>,
}

impl Rule {
    pub fn new(folder: &str) -> Self {
        Self {
            folder: folder.into(),
            ..Default::default()
        }
    }

    pub fn patterns(&self) -> Vec<String> {
        self.patterns
            .split(',')
            .map(|p| p.trim().to_lowercase())
            .filter(|p| !p.is_empty())
            .collect()
    }

    /// (from, to) folders for this rule's direction.
    pub fn ends(&self, client: &Path, server: &Path) -> (PathBuf, PathBuf) {
        let (c, s) = (client.join(&self.folder), server.join(&self.folder));
        if self.pull { (s, c) } else { (c, s) }
    }
}

/// Rules per server root.
pub type Rules = HashMap<String, Vec<Rule>>;

fn glob(p: &str, s: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some(b'*'), _) => go(&p[1..], s) || (!s.is_empty() && go(p, &s[1..])),
            (Some(b'?'), Some(_)) => go(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &s[1..]),
            _ => false,
        }
    }
    go(p.as_bytes(), s.as_bytes())
}

/// A pattern matches the file name, or any path segment, by glob; a
/// pattern without wildcards matches as a substring.
pub fn matches(patterns: &[String], rel: &str) -> bool {
    let rel = rel.to_lowercase();
    patterns.iter().any(|p| {
        if p.contains(['*', '?']) {
            rel.split('/').any(|seg| glob(p, seg)) || glob(p, &rel)
        } else {
            rel.contains(p.as_str())
        }
    })
}

fn walk(root: &Path) -> HashMap<String, fs::Metadata> {
    let mut out = HashMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                stack.push(p);
            } else if let Ok(rel) = p.strip_prefix(root) {
                let rel = rel.to_string_lossy().replace('\\', "/");
                // Backups, logs and locks aren't worth mirroring.
                if !crate::configdiff::is_noise(&rel) {
                    out.insert(rel, md);
                }
            }
        }
    }
    out
}

#[derive(Clone, Debug, Default)]
pub struct MirrorPlan {
    pub copy: Vec<String>,
    pub delete: Vec<String>,
    pub ops: Vec<SyncOp>,
}

/// What mirroring `rule` would change at the destination.
pub fn plan_mirror(rule: &Rule, client: &Path, server: &Path) -> MirrorPlan {
    let (from, to) = rule.ends(client, server);
    let pats = rule.patterns();
    let only = rule.mode == Mode::MirrorOnly;
    let wanted = |rel: &str| {
        if only {
            matches(&pats, rel)
        } else {
            !matches(&pats, rel)
        }
    };
    let src = walk(&from);
    let dst = walk(&to);
    let mut plan = MirrorPlan::default();
    let mut names: Vec<&String> = src.keys().filter(|r| wanted(r)).collect();
    names.sort();
    // Same size and a newer source: read both to be sure. The server disk
    // is slow, so that runs on several threads.
    let check: Vec<&String> = names
        .iter()
        .copied()
        .filter(|rel| {
            dst.get(*rel).is_some_and(|d| {
                d.len() == src[*rel].len() && src[*rel].modified().ok() > d.modified().ok()
            })
        })
        .collect();
    let changed: HashSet<&String> = if check.is_empty() {
        HashSet::new()
    } else {
        let chunk = check.len().div_ceil(12);
        std::thread::scope(|sc| {
            let parts: Vec<_> = check
                .chunks(chunk)
                .map(|part| {
                    let (from, to) = (&from, &to);
                    sc.spawn(move || {
                        part.iter()
                            .copied()
                            .filter(|rel| {
                                fs::read(from.join(rel)).ok() != fs::read(to.join(rel)).ok()
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            parts
                .into_iter()
                .flat_map(|h| h.join().unwrap_or_default())
                .collect()
        })
    };
    for rel in names {
        let differs = match dst.get(rel) {
            None => true,
            Some(d) => d.len() != src[rel].len() || changed.contains(rel),
        };
        if differs {
            plan.copy.push(rel.clone());
            plan.ops.push(SyncOp::Copy {
                src: from.join(rel),
                dst: to.join(rel),
            });
        }
    }
    let keep: HashSet<&String> = src.keys().filter(|r| wanted(r)).collect();
    let mut extra: Vec<&String> = dst
        .keys()
        // "All except" leaves excluded files at the destination alone.
        .filter(|r| !keep.contains(r) && (only || !matches(&pats, r)))
        .collect();
    extra.sort();
    for rel in extra {
        plan.delete.push(rel.clone());
        plan.ops.push(SyncOp::Delete { path: to.join(rel) });
    }
    plan
}

// ── live links (junctions) ──────────────────────────────────────────

/// True when `p` is on a network drive or share, where junctions can't
/// point (and the server's machine couldn't follow one anyway).
pub fn on_network(p: &Path) -> bool {
    let s = p.to_string_lossy();
    if s.starts_with(r"\\") {
        return true;
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetDriveTypeW(root: *const u16) -> u32;
        }
        const DRIVE_REMOTE: u32 = 4;
        if let Some(letter) = s.get(..2).filter(|d| d.ends_with(':')) {
            let root: Vec<u16> = format!("{letter}\\").encode_utf16().chain([0]).collect();
            // SAFETY: a valid, NUL-terminated wide string.
            return unsafe { GetDriveTypeW(root.as_ptr()) } == DRIVE_REMOTE;
        }
    }
    false
}

pub fn is_link(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) || fs::read_link(p).is_ok()
}

/// Makes `to` a junction to `from`, moving an existing `to` into backups.
/// Returns where the displaced folder went.
pub fn link(from: &Path, to: &Path, backup_root: &Path) -> anyhow::Result<Option<PathBuf>> {
    anyhow::ensure!(
        !on_network(from) && !on_network(to),
        "Live links only work when both folders are on this PC's own disks"
    );
    anyhow::ensure!(from.is_dir(), "{} doesn't exist", from.display());
    if is_link(to) {
        return Ok(None);
    }
    let displaced = crate::sync::backup_into(backup_root, to)?;
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    let out = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        // cmd reads a forward slash as a switch.
        .arg(to.to_string_lossy().replace('/', "\\"))
        .arg(from.to_string_lossy().replace('/', "\\"))
        .output()?;
    if !out.status.success() {
        if let Some(d) = &displaced {
            let _ = fs::rename(d, to);
        }
        anyhow::bail!(
            "mklink failed: {}",
            String::from_utf8_lossy(&out.stdout).trim().to_string()
                + String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(displaced)
}

/// Removes the junction at `to` (never what it points to) and puts back
/// the folder it displaced.
pub fn unlink(to: &Path, displaced: Option<&Path>) -> anyhow::Result<()> {
    anyhow::ensure!(is_link(to), "{} is not a live link", to.display());
    fs::remove_dir(to)?;
    if let Some(d) = displaced.filter(|d| d.exists()) {
        fs::rename(d, to)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_match_like_robocopy_filters() {
        let p = vec!["*.disabled".to_string(), "secret".to_string()];
        assert!(matches(&p, "a/b.jar.disabled"));
        assert!(matches(&p, "my-secret.txt"));
        assert!(!matches(&p, "a/b.jar"));
        assert!(matches(
            &["journeymap".to_string()],
            "journeymap/data/x.json"
        ));
        assert!(matches(&["*.zip".to_string()], "backups/2026.zip"));
    }

    #[test]
    fn mirror_plans_copies_and_deletions() {
        let root = std::env::temp_dir().join(format!("hopper-rules-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (c, s) = (root.join("c/scripts"), root.join("s/scripts"));
        fs::create_dir_all(c.join("sub")).unwrap();
        fs::create_dir_all(&s).unwrap();
        fs::write(c.join("a.js"), "new").unwrap();
        fs::write(c.join("sub/b.js"), "b").unwrap();
        fs::write(c.join("skip.old2"), "x").unwrap();
        fs::write(s.join("old.js"), "old").unwrap();
        fs::write(s.join("keep.old2"), "x").unwrap();

        let mut r = Rule::new("scripts");
        r.mode = Mode::MirrorExcept;
        r.patterns = "*.old2".into();
        let p = plan_mirror(&r, &root.join("c"), &root.join("s"));
        assert_eq!(p.copy, ["a.js", "sub/b.js"]);
        assert_eq!(p.delete, ["old.js"]); // keep.old2 is excluded, left alone

        r.mode = Mode::MirrorOnly;
        r.patterns = "*.js".into();
        let p = plan_mirror(&r, &root.join("c"), &root.join("s"));
        assert_eq!(p.copy, ["a.js", "sub/b.js"]);
        assert_eq!(p.delete, ["keep.old2", "old.js"]);

        // Pull reverses it.
        r.pull = true;
        let p = plan_mirror(&r, &root.join("c"), &root.join("s"));
        assert_eq!(p.copy, ["old.js"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg_attr(not(windows), ignore = "uses Windows paths and tools")]
    fn links_and_unlinks_a_local_folder() {
        let root = std::env::temp_dir().join(format!("hopper-link-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (from, to) = (root.join("client/cfg"), root.join("server/cfg"));
        fs::create_dir_all(&from).unwrap();
        fs::create_dir_all(&to).unwrap();
        fs::write(from.join("a.txt"), "client").unwrap();
        fs::write(to.join("a.txt"), "server").unwrap();
        let displaced = link(&from, &to, &root.join("server")).unwrap();
        assert!(is_link(&to));
        assert_eq!(fs::read_to_string(to.join("a.txt")).unwrap(), "client");
        unlink(&to, displaced.as_deref()).unwrap();
        assert!(!is_link(&to));
        assert_eq!(fs::read_to_string(to.join("a.txt")).unwrap(), "server");
        assert_eq!(fs::read_to_string(from.join("a.txt")).unwrap(), "client");
        assert!(!on_network(&root));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore = "reads the real pack and server"]
    fn real_mirror_plan() {
        let mut r = Rule::new("config");
        r.mode = Mode::MirrorExcept;
        let t = std::time::Instant::now();
        let p = plan_mirror(
            &r,
            Path::new(r"G:\Games\Minecraft\Curse\Instances\TNP Limitless 8"),
            Path::new(r"Q:\Minecraft\packs\TNP-LL8"),
        );
        println!(
            "{} copy, {} delete in {:?}",
            p.copy.len(),
            p.delete.len(),
            t.elapsed()
        );
        assert!(on_network(Path::new(r"Q:\Minecraft")));
    }
}
