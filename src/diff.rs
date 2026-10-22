//! Client vs server mod diff. Ported from mod-sync's `ComputeDiff`.

use crate::model::{DiffAction, DiffEntry, DiffStatus, ModInfo};
use crate::scan::{compare_versions, strip_version_suffix, version_from_file_name};
use std::collections::{HashMap, HashSet};

/// Mods that only make sense on a client. Used only when nothing else says
/// which side a mod runs on; see `known_client_only` for the matching.
const KNOWN_CLIENT_ONLY: &[&str] = &[
    "sodium",
    "iris",
    "oculus",
    "betterf3",
    "betterthirdperson",
    "entityculling",
    "entity-model-features",
    "entity_texture_features",
    "notenoughanimations",
    "smoothmovement",
    "eatinganimation",
    "cullleaves",
    "visuality",
    "subtle_effects",
    "particle",
    "smart_particles",
    "particular",
    "bettergrass",
    "fancymenu",
    "darkmodeeverywhere",
    "colorfulhearts",
    "statuseffectbars",
    "itemborders",
    "betterblockz",
    "continuity",
    "moreculling",
    "reeses-sodium",
    "sodium-extra",
    "sodiumextra",
    "sodiumoptions",
    "sodiumextrainformation",
    "farsight",
    "better_climbing",
    "boatiview",
    "boatview",
    "dragit",
    "gnetum",
    "flerovium",
    "ibp",
    "light-overlay",
    "modelfix",
    "shut_up_gl_error",
    "gpumemleakfix",
    "derenderpatcher",
    "player-animation",
    "freecam",
    "itemzoom",
    "immersivehotbar",
    "emi",
    "jei",
    "inventoryprofiles",
    "libipn",
    "appleskin",
    "chat_heads",
    "controlling",
    "crafting_on_a_stick",
    "toolbelt",
    "trashslot",
    "distraction_free",
    "betterp2p",
    "advancementplaques",
    "tipthescales",
    "drippyloadingscreen",
    "respackopts",
    "resourcify",
    "screenshot-to-clipboard",
    "modnametooltip",
    "legendarytooltips",
    "durabilitytooltip",
    "shulkerboxtooltip",
    "tagtooltips",
    "missingmodswarning",
    "betteradvancements",
    "searchables",
    "clientcrafting",
    "emojiful",
    "catalogue",
    "equipmentcompare",
    "simplelootviewer",
    "findme",
    "enchdesc",
    "extrasounds",
    "immersiveui",
    "mousetweaks",
    "moremousetweaks",
    "configured",
    "chest_item",
    "toomanyrecipeviewers",
    "configureddefaults",
    "biomemusic",
    "dynamic-fps",
    "extremesoundmuffler",
    "more_sounds",
    "badoptimizations",
    "particle_core",
    "flickerfix",
    "yeetusexperimentus",
    "fast-ip-ping",
    "ixeris",
    "crash_assistant",
    "crashutilities",
    "asynclogger",
];

/// Why a client mod should not go to the server, most trusted signal first.
fn skip_reason(m: &ModInfo) -> Option<String> {
    if let Some(r) = &m.wont_sync {
        return Some(r.clone());
    }
    match m.side.as_str() {
        "client" => Some("Client-only (the mod, CurseForge or Modrinth says so)".into()),
        // The jar or the site says the server needs it: trust that over the list.
        "" => known_client_only(m).then(|| "Known client-only mod".into()),
        _ => None,
    }
}

/// Lowercase words of a name: "Dynamic-FPS_3.9+mc" -> [dynamic, fps, 3, 9, mc].
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// A list entry matches whole words of the file name (so "emi" matches
/// "emi-1.1.jar" but not "alchemistry"), or the mod id exactly.
fn known_client_only(m: &ModInfo) -> bool {
    let file = words(&m.file_name);
    let id: String = words(&m.mod_id).concat();
    KNOWN_CLIENT_ONLY.iter().any(|p| {
        let p = words(p);
        (!id.is_empty() && id == p.concat()) || file.windows(p.len()).any(|w| w == p.as_slice())
    })
}

/// Matches each client jar to a server jar by file name, then by name without
/// the version, then by mod id. The client is the source of truth.
pub fn compute_diff(client: &[ModInfo], server: &[ModInfo]) -> Vec<DiffEntry> {
    let mut by_file = HashMap::new();
    let mut by_base = HashMap::new();
    let mut by_id = HashMap::new();
    for (i, m) in server.iter().enumerate() {
        by_file.insert(m.file_name.to_lowercase(), i);
        by_base.insert(strip_version_suffix(&m.file_name).to_lowercase(), i);
        if !m.mod_id.is_empty() {
            by_id.insert(m.mod_id.to_lowercase(), i);
        }
    }

    let mut seen = HashSet::new();
    let mut client_ids = HashSet::new();
    let mut out = Vec::new();

    for cm in client {
        let matched = by_file
            .get(&cm.file_name.to_lowercase())
            .or_else(|| by_base.get(&strip_version_suffix(&cm.file_name).to_lowercase()))
            .or_else(|| {
                (!cm.mod_id.is_empty())
                    .then(|| by_id.get(&cm.mod_id.to_lowercase()))
                    .flatten()
            })
            .copied();

        let Some(si) = matched else {
            let skip_reason = skip_reason(cm);
            out.push(DiffEntry {
                mod_name: cm.mod_name.clone(),
                client: Some(cm.clone()),
                server: None,
                status: DiffStatus::OnlyClient,
                action: if skip_reason.is_some() {
                    DiffAction::Skip
                } else {
                    DiffAction::CopyToServer
                },
                skip_reason,
            });
            continue;
        };

        let sm = &server[si];
        seen.insert(si);
        if !cm.mod_id.is_empty() {
            client_ids.insert(cm.mod_id.to_lowercase());
        }

        // mods.toml versions often stay "1.36.0" across CI builds while the
        // file name carries the build number, so fall back to that.
        let (mut cv, mut sv) = (cm.version.clone(), sm.version.clone());
        if cv.is_empty() || sv.is_empty() || cv == sv {
            let (fc, fs) = (
                version_from_file_name(&cm.file_name),
                version_from_file_name(&sm.file_name),
            );
            if !fc.is_empty() && !fs.is_empty() {
                (cv, sv) = (fc, fs);
            }
        }
        let by_version = !cv.is_empty() && !sv.is_empty() && cv != sv;
        let by_size = !by_version
            && !cm.file_name.eq_ignore_ascii_case(&sm.file_name)
            && cm.file_size > 0
            && sm.file_size > 0
            && cm.file_size != sm.file_size;

        let (status, action) = if by_version || by_size {
            let status = if by_version && compare_versions(&cv, &sv).is_lt() {
                DiffStatus::ServerNewer
            } else {
                DiffStatus::ClientNewer
            };
            (status, DiffAction::CopyToServer)
        } else {
            (DiffStatus::Same, DiffAction::Skip)
        };
        out.push(DiffEntry {
            mod_name: cm.mod_name.clone(),
            client: Some(cm.clone()),
            server: Some(sm.clone()),
            status,
            action,
            // Already on the server, so any "client-only" claim is moot.
            skip_reason: None,
        });
    }

    for (i, sm) in server.iter().enumerate() {
        if seen.contains(&i) {
            continue;
        }
        // An old file of a mod the client has under a newer name.
        let stale = !sm.mod_id.is_empty() && client_ids.contains(&sm.mod_id.to_lowercase());
        out.push(DiffEntry {
            mod_name: sm.mod_name.clone(),
            client: None,
            server: Some(sm.clone()),
            status: if stale {
                DiffStatus::ServerStale
            } else {
                DiffStatus::OnlyServer
            },
            action: if stale {
                DiffAction::DeleteFromServer
            } else {
                DiffAction::Skip
            },
            skip_reason: None,
        });
    }

    let rank = |s: DiffStatus| match s {
        DiffStatus::ClientNewer | DiffStatus::ServerNewer => 0,
        DiffStatus::ServerStale => 1,
        DiffStatus::OnlyClient => 2,
        DiffStatus::OnlyServer => 3,
        DiffStatus::Same => 4,
    };
    out.sort_by(|a, b| {
        rank(a.status)
            .cmp(&rank(b.status))
            .then_with(|| a.mod_name.to_lowercase().cmp(&b.mod_name.to_lowercase()))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jar(file: &str, id: &str, version: &str) -> ModInfo {
        ModInfo {
            file_name: file.into(),
            mod_id: id.into(),
            mod_name: id.into(),
            version: version.into(),
            file_size: 100,
            ..Default::default()
        }
    }

    #[test]
    fn newer_client_build_is_pushed_and_old_server_file_matched() {
        let client = [jar("create-1.21.1-6.0.5.jar", "create", "6.0.5")];
        let server = [jar("create-1.21.1-6.0.4.jar", "create", "6.0.4")];
        let d = compute_diff(&client, &server);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].status, DiffStatus::ClientNewer);
        assert_eq!(d[0].action, DiffAction::CopyToServer);
    }

    #[test]
    fn client_only_mods_default_to_skip() {
        let d = compute_diff(&[jar("sodium-0.6.jar", "sodium", "0.6")], &[]);
        assert_eq!(d[0].action, DiffAction::Skip);
        assert!(d[0].skip_reason.is_some());
    }

    #[test]
    fn list_matches_whole_words_only() {
        let skip = |f: &str, id: &str| compute_diff(&[jar(f, id, "1")], &[])[0].action;
        assert_eq!(skip("emi-1.1.18+1.21.1+neoforge.jar", "emi"), DiffAction::Skip);
        assert_eq!(skip("dynamic-fps-3.9+minecraft-1.21.0-neoforge.jar", "dynamic_fps"), DiffAction::Skip);
        assert_eq!(skip("Particular-1.21.1-NeoForge-1.2.jar", "particular"), DiffAction::Skip);
        assert_eq!(skip("alchemistry-1.21.1-3.0.jar", "alchemistry"), DiffAction::CopyToServer);
        assert_eq!(skip("premium_wood-1.0.jar", "premium_wood"), DiffAction::CopyToServer);
        assert_eq!(skip("jeiintegration-1.0.jar", "jeiintegration"), DiffAction::CopyToServer);
    }

    #[test]
    fn a_known_side_beats_the_list() {
        let mut a = jar("emi-1.1.jar", "emi", "1.1");
        a.side = "both".into();
        let d = compute_diff(&[a], &[]);
        assert_eq!(d[0].action, DiffAction::CopyToServer);
        assert!(d[0].skip_reason.is_none());
    }

    #[test]
    fn pack_author_exclusion_skips_but_not_when_already_on_server() {
        let mut a = jar("subtle-effects-1.0.jar", "subtle_fx", "1.0");
        a.wont_sync = Some("left out of server pack".into());
        let d = compute_diff(std::slice::from_ref(&a), &[]);
        assert_eq!(d[0].action, DiffAction::Skip);
        let d = compute_diff(&[a.clone()], &[a]);
        assert_eq!(d[0].status, DiffStatus::Same);
        assert!(d[0].skip_reason.is_none());
    }

    #[test]
    fn server_only_mods_are_left_alone() {
        let d = compute_diff(&[], &[jar("spark-1.10.jar", "spark", "1.10")]);
        assert_eq!(d[0].status, DiffStatus::OnlyServer);
        assert_eq!(d[0].action, DiffAction::Skip);
    }
}

/// Runs against the real TNP pack and its server on rig. Read-only.
/// `cargo test real_tnp -- --ignored --nocapture`
#[cfg(test)]
#[test]
#[ignore]
fn real_tnp() {
    use std::path::Path;
    crate::env::load(std::path::Path::new(".env"));
    let inst = crate::instances::load(Path::new(
        r"G:\Games\Minecraft\Curse\Instances\TNP Limitless 8",
    ))
    .unwrap();
    let mut client = crate::scan::scan_folder(&inst.mods_dir()).unwrap();
    let note = crate::serverpack::annotate(&mut client, &inst, &Default::default());
    println!("annotation: {note:?}");
    let server = crate::scan::scan_folder(Path::new(r"Q:\Minecraft\packs\TNP-LL8\mods")).unwrap();
    let d = compute_diff(&client, &server);
    println!("client {} jars, server {} jars", client.len(), server.len());
    for d in d.iter().filter(|d| d.status != DiffStatus::Same) {
        println!(
            "{:?} {:?} {} {:?} -> {:?}",
            d.status,
            d.action,
            d.mod_name,
            d.client.as_ref().map(|m| &m.version),
            d.server.as_ref().map(|m| &m.version)
        );
    }
    let start = std::time::Instant::now();
    let _ = crate::scan::scan_folder(Path::new(r"Q:\Minecraft\packs\TNP-LL8\mods")).unwrap();
    println!("server rescan took {:?}", start.elapsed());
}
