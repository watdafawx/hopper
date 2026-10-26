//! What's left of the mod hunt ("Find the mod that breaks it"), removed in
//! 0.13.5: on a big pack it turned mods off in the player's real worlds,
//! where their blocks and items are lost, and KubeJS scripts, data packs and
//! configs that use those mods failed in ways that read as the wrong answer.
//!
//! A hunt left running by an older Hopper has its journal ([`FILE`]) in the
//! instance folder with every jar's on/off state from before it started;
//! [`restore_leftover`] puts the folder back that way and removes it.

use crate::instances::Instance;
use anyhow::Context as _;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The journal an older Hopper's hunt kept, beside `hopper.json`.
pub const FILE: &str = "hopper-bisect.json";

/// A mod this many others need counts as a library (see
/// [`crate::crashdetect`]).
pub const LIBRARY_MIN: usize = 3;

pub fn path(root: &Path) -> PathBuf {
    root.join(FILE)
}

/// The part of the old journal a restore needs.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Journal {
    /// Every jar in `mods/` before the hunt: file name (without
    /// `.disabled`) → on.
    original: BTreeMap<String, bool>,
}

/// A hunt an older Hopper left running in `inst`: every jar back on or off
/// as before it started, then the journal goes (only once that worked, so
/// a failed restore is tried again next time). `None` when there was none.
pub fn restore_leftover(inst: &mut Instance) -> anyhow::Result<Option<usize>> {
    let p = path(&inst.root);
    let Ok(text) = std::fs::read_to_string(&p) else { return Ok(None) };
    let j: Journal = serde_json::from_str(&text).context("reading the old mod hunt's journal")?;
    let mut changed = 0;
    let mut failed = Vec::new();
    for (file, on) in &j.original {
        match set_file(inst, file, *on) {
            Ok(true) => changed += 1,
            Ok(false) => {}
            Err(e) => failed.push(format!("{e:#}")),
        }
    }
    inst.save()?;
    if let Some(first) = failed.first() {
        anyhow::bail!("{} files didn't change back: {first}", failed.len());
    }
    let _ = std::fs::remove_file(&p);
    Ok(Some(changed))
}

/// Turns one jar on or off by its `.disabled` name; true when it moved.
fn set_file(inst: &mut Instance, file: &str, on: bool) -> anyhow::Result<bool> {
    let dir = inst.mods_dir();
    let (en, dis) = (dir.join(file), dir.join(format!("{file}.disabled")));
    let (from, to) = if on { (&dis, &en) } else { (&en, &dis) };
    let moved = if from.is_file() && !to.exists() {
        std::fs::rename(from, to)
            .map_err(anyhow::Error::from)
            .map_err(crate::install::explain_locked)
            .with_context(|| format!("renaming {file}"))?;
        true
    } else {
        false
    };
    for a in inst.addons.iter_mut().filter(|a| a.folder() == "mods" && a.file_name.eq_ignore_ascii_case(file)) {
        if to.is_file() {
            a.enabled = on;
        }
    }
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leftover_hunt_is_put_back() {
        let root = std::env::temp_dir().join(format!("hopper-bisect-left-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mods = root.join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        // Before the hunt: a on, b off. The hunt left a off and b on.
        std::fs::write(mods.join("a.jar.disabled"), "a").unwrap();
        std::fs::write(mods.join("b.jar"), "b").unwrap();
        std::fs::write(path(&root), r#"{"original":{"a.jar":true,"b.jar":false},"phase":"mods"}"#).unwrap();
        let mut inst = crate::modpack::create(&root.join(".."), "x", "1.21.1", "").unwrap();
        inst.root = root.clone();
        assert_eq!(restore_leftover(&mut inst).unwrap(), Some(2));
        assert!(mods.join("a.jar").is_file() && mods.join("b.jar.disabled").is_file());
        assert!(!path(&root).exists());
        assert_eq!(restore_leftover(&mut inst).unwrap(), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
