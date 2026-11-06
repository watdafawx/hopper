//! A server folder's mods as an instance. Hopper keeps a `hopper.json` in
//! the server folder (as in an instance), so Hopper desktop and Hopper
//! Server both list, switch, remove and update a server's mods with the
//! instance code.

use crate::instances::{self, Instance};
use std::path::Path;

/// Minecraft version for a NeoForge version: 21.1.259 → 1.21.1,
/// 21.0.167 → 1.21, 26.3.0.95 → 26.3.
pub fn mc_for_neoforge(v: &str) -> Option<String> {
    let p: Vec<&str> = v.split('.').collect();
    match p.as_slice() {
        [a, b, _, _] => Some(if *b == "0" { a.to_string() } else { format!("{a}.{b}") }).map(|s| {
            // 26.1.2.x: the third part is Minecraft's patch.
            if p[2] != "0" { format!("{}.{}.{}", p[0], p[1], p[2]) } else { s }
        }),
        [a, b, _] => Some(if *b == "0" { format!("1.{a}") } else { format!("1.{a}.{b}") }),
        _ => None,
    }
}

/// What the server runs, from what Hopper recorded when making it, else
/// its run script, else `console_version` (what its console said last).
pub fn detect(root: &Path, console_version: &str) -> (String, String) {
    if let Some(m) = crate::hosting::HostedMeta::load(root) {
        let l = m.loader.to_lowercase();
        let loader = if let Some(v) = l.strip_prefix("neoforge ") {
            format!("neoforge-{v}")
        } else if l.starts_with("fabric") {
            "fabric".into()
        } else if l.starts_with("quilt") {
            "quilt".into()
        } else {
            String::new()
        };
        if !m.minecraft.is_empty() {
            return (m.minecraft, loader);
        }
    }
    if let Some(v) = crate::server_update::server_neoforge(root) {
        return (mc_for_neoforge(&v).unwrap_or_default(), format!("neoforge-{v}"));
    }
    let loader = if root.join("fabric-server-launch.jar").is_file() {
        "fabric"
    } else if root.join("quilt-server-launch.jar").is_file() {
        "quilt"
    } else {
        ""
    };
    (console_version.to_string(), loader.into())
}

/// Fixes entries older panels saved wrong: a jar that was off saved as
/// "x.jar.disabled" and marked on, and the second entry identifying it again
/// added for the same jar. The jar on disk says whether it's on. True when
/// anything changed.
pub fn heal(i: &mut Instance) -> bool {
    let dir = i.mods_dir();
    let mut changed = false;
    for a in i.addons.iter_mut().filter(|a| a.folder() == "mods") {
        if let Some(base) = a.file_name.strip_suffix(".disabled") {
            a.file_name = base.to_string();
            changed = true;
        }
        let on = dir.join(&a.file_name).is_file();
        let off = dir.join(format!("{}.disabled", a.file_name)).is_file();
        if on != off && a.enabled != on {
            a.enabled = on;
            changed = true;
        }
    }
    let mut seen = std::collections::HashSet::new();
    let before = i.addons.len();
    i.addons.retain(|a| a.folder() != "mods" || seen.insert(a.file_name.to_lowercase()));
    changed || i.addons.len() != before
}

/// The server folder at `root` as an instance, made (and saved) the first
/// time. The second value is true when old entries were fixed (see
/// [`heal`]), so a cached update list for it is stale.
pub fn load(root: &Path, name: &str, console_version: &str) -> anyhow::Result<(Instance, bool)> {
    if root.join(instances::HOPPER_FILE).is_file()
        && let Some(mut i) = instances::load(root)
    {
        let healed = heal(&mut i);
        if healed {
            log::info!("{name}: fixed the mod list's entries for jars that are off");
            i.save()?;
        }
        // Saved before the version was known (a new server that hadn't run):
        // fill it in now if it can be found.
        if i.game_version.is_empty() {
            let (game_version, loader) = detect(root, console_version);
            if !game_version.is_empty() {
                i.game_version = game_version;
                if i.loader.is_empty() {
                    i.loader = loader;
                }
                i.save()?;
            }
        }
        return Ok((i, healed));
    }
    let (game_version, loader) = detect(root, console_version);
    let pack = crate::server_pack_update::recorded(root).map(|m| instances::PackRef {
        project_id: m.project_id,
        file_id: m.pack_file_id,
        name: m.server_pack.clone(),
        server_pack_file_id: m.server_pack_file_id,
        ..Default::default()
    });
    let i = Instance {
        name: name.to_string(),
        root: root.to_path_buf(),
        game_version,
        loader,
        pack,
        format: instances::FORMAT,
        ..Default::default()
    };
    i.save()?;
    Ok((i, false))
}
