//! Keeps CurseForge's `minecraftinstance.json` in step with Hopper's loader,
//! so an instance changed in Hopper starts the same loader from CurseForge.
//!
//! The loader (`baseModLoader`), Minecraft version (`gameVersion`) and,
//! after a modpack update, the pack's file go back. CurseForge finds added,
//! removed and disabled mods itself by scanning the mods folder. The `baseModLoader` record is CurseForge's own,
//! read from its API, since it embeds the loader's version and install
//! profiles that CurseForge launches with.

use crate::instances::Instance;
use anyhow::Context as _;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

pub const CF_FILE: &str = "minecraftinstance.json";

/// What [`write_loader`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Synced {
    /// Not a CurseForge instance, or it already matches.
    Nothing,
    /// CurseForge's file now names the instance's loader.
    Written,
    /// CurseForge is open and would write its own copy back on exit.
    CurseForgeOpen,
}

/// The loader and Minecraft version CurseForge's file names, if it has one.
pub fn cf_loader(root: &Path) -> Option<(String, String)> {
    let v: Value = serde_json::from_slice(&fs::read(root.join(CF_FILE)).ok()?).ok()?;
    let name = v["baseModLoader"]["name"].as_str().unwrap_or_default().to_string();
    let mc = v["gameVersion"].as_str().unwrap_or_default().to_string();
    Some((name, mc))
}

/// Whether CurseForge's file disagrees with Hopper about the loader.
pub fn differs(inst: &Instance) -> Option<(String, String)> {
    let (name, mc) = cf_loader(&inst.root)?;
    let same = same_loader(&name, &inst.loader, &inst.game_version) && (mc.is_empty() || mc == inst.game_version);
    (!same).then_some((name, mc))
}

/// CurseForge spells Fabric and Quilt with the Minecraft version on the end
/// ("fabric-0.16.14-1.21.1"); Hopper's picker doesn't.
fn same_loader(cf: &str, hopper: &str, mc: &str) -> bool {
    cf.eq_ignore_ascii_case(hopper) || cf.eq_ignore_ascii_case(&format!("{hopper}-{mc}"))
}

/// Whether the CurseForge app is running.
pub fn curseforge_running() -> bool {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    sys.processes()
        .values()
        .any(|p| p.name().to_string_lossy().eq_ignore_ascii_case("CurseForge.exe"))
}

/// Writes the instance's loader and Minecraft version into CurseForge's
/// file, keeping a copy of the old one under `.hopper-backups`. Records the
/// new file time in `inst` (the caller saves it), so Hopper doesn't read its
/// own write back as a CurseForge change.
pub fn write_loader(inst: &mut Instance) -> anyhow::Result<Synced> {
    let path = inst.root.join(CF_FILE);
    let Ok(bytes) = fs::read(&path) else {
        return Ok(Synced::Nothing);
    };
    if differs(inst).is_none() || inst.loader.is_empty() {
        return Ok(Synced::Nothing);
    }
    if curseforge_running() {
        return Ok(Synced::CurseForgeOpen);
    }
    let mut doc: Value = serde_json::from_slice(&bytes).context("reading minecraftinstance.json")?;
    let record = modloader(&inst.loader, &inst.game_version)?;
    let base = &mut doc["baseModLoader"];
    if !base.is_object() {
        *base = Value::Object(Default::default());
    }
    merge_record(base, &record);
    doc["gameVersion"] = Value::String(inst.game_version.clone());

    replace(inst, &bytes, &doc)?;
    Ok(Synced::Written)
}

/// Mods whose lock CurseForge's file (`isLocked`) and Hopper disagree on.
pub fn locks_differ(inst: &Instance) -> usize {
    let Some(doc) = fs::read(inst.root.join(CF_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(strip_bom(&b)).ok())
    else {
        return 0;
    };
    lock_changes(&doc, inst).len()
}

/// `(index in installedAddons, Hopper's lock)` for each entry whose
/// `isLocked` differs from Hopper's addon for the same file.
fn lock_changes(doc: &Value, inst: &Instance) -> Vec<(usize, bool)> {
    let Some(list) = doc["installedAddons"].as_array() else {
        return Vec::new();
    };
    list.iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let file = e["fileNameOnDisk"].as_str()?;
            let a = inst.addon_by_file(file)?;
            (e["isLocked"].as_bool().unwrap_or(false) != a.locked).then_some((i, a.locked))
        })
        .collect()
}

/// Writes Hopper's locks into CurseForge's file (`isLocked`), so the
/// CurseForge app doesn't update a mod Hopper keeps on its version either.
/// Records what it wrote as each addon's `cf_locked` and the new file time
/// in `inst` (the caller saves it).
pub fn write_locks(inst: &mut Instance) -> anyhow::Result<Synced> {
    let path = inst.root.join(CF_FILE);
    let Ok(bytes) = fs::read(&path) else {
        return Ok(Synced::Nothing);
    };
    let mut doc: Value = serde_json::from_slice(strip_bom(&bytes)).context("reading minecraftinstance.json")?;
    let changes = lock_changes(&doc, inst);
    if changes.is_empty() {
        return Ok(Synced::Nothing);
    }
    if curseforge_running() {
        return Ok(Synced::CurseForgeOpen);
    }
    for (i, locked) in changes {
        doc["installedAddons"][i]["isLocked"] = Value::Bool(locked);
    }
    // Only flags change, so no backup: one per click would pile up copies
    // of a file that runs to megabytes.
    swap_in(inst, &doc)?;
    for a in &mut inst.addons {
        a.cf_locked = a.locked;
    }
    Ok(Synced::Written)
}

/// The pack file CurseForge's file records, when it lists the same pack
/// as `inst` at another file.
pub fn pack_differs(inst: &Instance) -> Option<String> {
    let pack = inst.pack.as_ref()?;
    let v: Value = serde_json::from_slice(strip_bom(&fs::read(inst.root.join(CF_FILE)).ok()?)).ok()?;
    let m = &v["installedModpack"];
    if m["addonID"].as_u64() != Some(pack.project_id) {
        return None;
    }
    let same = m["installedFile"]["id"].as_u64() == Some(pack.file_id) && v["fileID"].as_u64() == Some(pack.file_id);
    (!same).then(|| {
        m["installedFile"]["fileName"]
            .as_str()
            .or(m["fileNameOnDisk"].as_str())
            .unwrap_or_default()
            .to_string()
    })
}

/// Writes the instance's modpack file into CurseForge's file, so
/// CurseForge sees the pack Hopper moved it to and doesn't offer (or
/// install) the old one. Keeps a backup and records the file time, as
/// [`write_loader`] does.
pub fn write_pack(inst: &mut Instance) -> anyhow::Result<Synced> {
    let path = inst.root.join(CF_FILE);
    let Ok(bytes) = fs::read(&path) else {
        return Ok(Synced::Nothing);
    };
    let Some(pack) = inst.pack.clone() else {
        return Ok(Synced::Nothing);
    };
    if pack_differs(inst).is_none() {
        return Ok(Synced::Nothing);
    }
    if curseforge_running() {
        return Ok(Synced::CurseForgeOpen);
    }
    let mut doc: Value = serde_json::from_slice(strip_bom(&bytes)).context("reading minecraftinstance.json")?;
    let file = crate::cf::file_value(pack.project_id, pack.file_id)?;
    let manifest = fs::read(inst.root.join("manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(strip_bom(&b)).ok());
    let overrides = crate::modpack::cached_override_files(pack.file_id);
    merge_pack(&mut doc, &file, manifest, overrides, &crate::time::now_iso());
    replace(inst, &bytes, &doc)?;
    Ok(Synced::Written)
}

fn strip_bom(b: &[u8]) -> &[u8] {
    b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(b)
}

/// Puts a new pack file (`file`, CurseForge's API record) into CurseForge's
/// document. `manifest` and `overrides` are the new pack's; `None` keeps
/// what the document has.
fn merge_pack(doc: &mut Value, file: &Value, manifest: Option<Value>, overrides: Option<Vec<String>>, now: &str) {
    doc["fileID"] = file["id"].clone();
    doc["fileDate"] = file["fileDate"].clone();
    if let Some(m) = manifest {
        doc["manifest"] = m;
    }
    if let Some(o) = overrides {
        doc["modpackOverrides"] = Value::from(o);
    }
    let m = &mut doc["installedModpack"];
    if let Some(name) = file["fileName"].as_str() {
        m["fileNameOnDisk"] = Value::from(name);
    }
    m["dateUpdated"] = Value::from(now);
    let installed = &mut m["installedFile"];
    if !installed.is_object() {
        *installed = Value::Object(Default::default());
    }
    merge_file(installed, file);
}

/// Copies an API file record into CurseForge's own spelling of it
/// (`installedFile`), keeping fields the API doesn't send.
fn merge_file(dst: &mut Value, api: &Value) {
    for k in [
        "id",
        "fileName",
        "fileDate",
        "fileLength",
        "releaseType",
        "fileStatus",
        "downloadUrl",
        "alternateFileId",
        "serverPackFileId",
        "isAvailable",
    ] {
        if let Some(v) = api.get(k) {
            dst[k] = v.clone();
        }
    }
    let list = |k: &str| api[k].as_array().cloned().unwrap_or_default();
    dst["isAlternate"] = Value::from(api["isServerPack"].as_bool().unwrap_or(false));
    dst["projectId"] = api["modId"].clone();
    dst["fileNameOnDisk"] = api["fileName"].clone();
    dst["packageFingerprint"] = api["fileFingerprint"].clone();
    dst["gameVersion"] = Value::from(list("gameVersions"));
    dst["sortableGameVersion"] = list("sortableGameVersions")
        .iter()
        .map(|g| {
            serde_json::json!({
                "gameVersion": g["gameVersion"],
                "gameVersionName": g["gameVersionName"],
                "gameVersionTypeId": g["gameVersionTypeId"],
            })
        })
        .collect();
    dst["dependencies"] = list("dependencies")
        .iter()
        .map(|d| serde_json::json!({"addonId": d["modId"], "type": d["relationType"]}))
        .collect();
    dst["modules"] = list("modules")
        .iter()
        .map(|m| serde_json::json!({"foldername": m["name"], "fingerprint": m["fingerprint"], "invalidFingerprint": false}))
        .collect();
    dst["hashes"] = list("hashes")
        .iter()
        .map(|h| serde_json::json!({"type": h["algo"], "value": h["value"]}))
        .collect();
}

/// Backs up `old`, swaps `doc` in and records the new file time.
fn replace(inst: &mut Instance, old: &[u8], doc: &Value) -> anyhow::Result<()> {
    backup(&inst.root, old)?;
    swap_in(inst, doc)
}

/// Writes `doc` over CurseForge's file (atomically) and records the new
/// file time.
fn swap_in(inst: &mut Instance, doc: &Value) -> anyhow::Result<()> {
    let path = inst.root.join(CF_FILE);
    let text = serde_json::to_vec(doc)?;
    let tmp = path.with_extension("json.hopper-tmp");
    fs::write(&tmp, &text)?;
    fs::rename(&tmp, &path).context("replacing minecraftinstance.json")?;
    inst.imported_from_cf = crate::instances::file_time(&path);
    Ok(())
}

/// Copies the fields CurseForge keeps from its API record. A file that has
/// none yet gets CurseForge's usual set.
fn merge_record(base: &mut Value, record: &Value) {
    const USUAL: [&str; 12] = [
        "forgeVersion",
        "name",
        "type",
        "downloadUrl",
        "filename",
        "installMethod",
        "latest",
        "recommended",
        "versionJson",
        "librariesInstallLocation",
        "minecraftVersion",
        "installProfileJson",
    ];
    let keys: Vec<String> = match base.as_object() {
        Some(m) if !m.is_empty() => m.keys().cloned().collect(),
        _ => USUAL.iter().map(|k| k.to_string()).collect(),
    };
    for k in keys {
        if let Some(v) = record.get(&k) {
            base[&k] = v.clone();
        }
    }
}

/// CurseForge's record for a loader, trying its spelling with the
/// Minecraft version on the end (Fabric, Quilt) when the bare one is
/// unknown.
fn modloader(loader: &str, mc: &str) -> anyhow::Result<Value> {
    let mut last = None;
    for name in [loader.to_string(), format!("{loader}-{mc}")] {
        match crate::cf::modloader(&name) {
            Ok(v) => return Ok(v),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no loader name")))
        .with_context(|| format!("CurseForge doesn't know {loader}"))
}

fn backup(root: &Path, bytes: &[u8]) -> anyhow::Result<PathBuf> {
    let dir = root
        .join(".hopper-backups")
        .join(crate::time::now().to_string());
    fs::create_dir_all(&dir)?;
    let p = dir.join(CF_FILE);
    fs::write(&p, bytes)?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fabric_names_match_with_or_without_mc() {
        assert!(same_loader("fabric-0.16.14-1.21.1", "fabric-0.16.14", "1.21.1"));
        assert!(same_loader("neoforge-21.1.256", "neoforge-21.1.256", "1.21.1"));
        assert!(!same_loader("neoforge-21.1.256", "neoforge-21.1.262", "1.21.1"));
    }

    #[test]
    fn merge_keeps_only_fields_curseforge_has() {
        let mut base = json!({"name": "neoforge-21.1.256", "forgeVersion": "21.1.256", "versionJson": "old"});
        let record = json!({"name": "neoforge-21.1.262", "forgeVersion": "21.1.262", "versionJson": "new", "id": 7, "approved": true});
        merge_record(&mut base, &record);
        assert_eq!(base, json!({"name": "neoforge-21.1.262", "forgeVersion": "21.1.262", "versionJson": "new"}));
    }

    #[test]
    fn merge_fills_an_empty_block() {
        let mut base = json!({});
        merge_record(&mut base, &json!({"name": "n", "filename": "f.jar", "id": 1}));
        assert_eq!(base, json!({"name": "n", "filename": "f.jar"}));
    }

    #[test]
    fn pack_update_moves_curseforge_to_the_new_file() {
        let mut doc = json!({
            "baseModLoader": {"name": "neoforge-21.1.248"},
            "manifest": {"version": "1.71.0"},
            "fileDate": "old",
            "installedModpack": {
                "addonID": 1027782,
                "fileNameOnDisk": "Limitless-8-1.71.0.zip",
                "dateUpdated": "old",
                "installedFile": {
                    "id": 1, "fileName": "Limitless-8-1.71.0.zip", "isCompatibleWithClient": false,
                    "projectStatus": 4, "hashes": [{"type": 1, "value": "aa"}]
                },
                "latestFile": {"id": 2}
            },
            "projectID": 1027782,
            "fileID": 1,
            "modpackOverrides": ["config\\old.toml"],
            "installedAddons": [{"addonID": 5}],
            "gameVersion": "1.21.1"
        });
        let file = json!({
            "id": 2, "gameId": 432, "modId": 1027782, "isAvailable": true, "displayName": "Limitless 8 - v1.71.1",
            "fileName": "Limitless-8-1.71.1.zip", "releaseType": 1, "fileStatus": 4,
            "hashes": [{"value": "bb", "algo": 1}], "fileDate": "2026-09-05T17:53:56.86Z", "fileLength": 10,
            "downloadCount": 907, "downloadUrl": "https://x/Limitless-8-1.71.1.zip",
            "gameVersions": ["1.21.1", "NeoForge"],
            "sortableGameVersions": [{"gameVersionName": "1.21.1", "gameVersionPadded": "p", "gameVersion": "1.21.1", "gameVersionTypeId": 77784}],
            "dependencies": [{"modId": 9, "relationType": 3}], "alternateFileId": 3, "isServerPack": false,
            "serverPackFileId": 4, "fileFingerprint": 77,
            "modules": [{"name": "overrides", "fingerprint": 5}]
        });
        merge_pack(&mut doc, &file, Some(json!({"version": "1.71.1"})), Some(vec!["config\\new.toml".into()]), "now");
        assert_eq!(doc["fileID"], 2);
        assert_eq!(doc["fileDate"], "2026-09-05T17:53:56.86Z");
        assert_eq!(doc["manifest"], json!({"version": "1.71.1"}));
        assert_eq!(doc["modpackOverrides"], json!(["config\\new.toml"]));
        assert_eq!(doc["projectID"], 1027782);
        assert_eq!(doc["installedAddons"], json!([{"addonID": 5}]));
        assert_eq!(doc["baseModLoader"], json!({"name": "neoforge-21.1.248"}));
        let m = &doc["installedModpack"];
        assert_eq!(m["fileNameOnDisk"], "Limitless-8-1.71.1.zip");
        assert_eq!(m["dateUpdated"], "now");
        assert_eq!(m["latestFile"], json!({"id": 2}));
        assert_eq!(
            m["installedFile"],
            json!({
                "id": 2, "fileName": "Limitless-8-1.71.1.zip", "isCompatibleWithClient": false, "projectStatus": 4,
                "hashes": [{"type": 1, "value": "bb"}], "fileDate": "2026-09-05T17:53:56.86Z", "fileLength": 10,
                "releaseType": 1, "fileStatus": 4, "downloadUrl": "https://x/Limitless-8-1.71.1.zip",
                "alternateFileId": 3, "serverPackFileId": 4, "isAvailable": true, "isAlternate": false,
                "projectId": 1027782, "fileNameOnDisk": "Limitless-8-1.71.1.zip", "packageFingerprint": 77,
                "gameVersion": ["1.21.1", "NeoForge"],
                "sortableGameVersion": [{"gameVersion": "1.21.1", "gameVersionName": "1.21.1", "gameVersionTypeId": 77784}],
                "dependencies": [{"addonId": 9, "type": 3}],
                "modules": [{"foldername": "overrides", "fingerprint": 5, "invalidFingerprint": false}]
            })
        );
    }

    #[test]
    fn pack_update_keeps_manifest_and_overrides_it_cant_read() {
        let mut doc = json!({"manifest": {"v": 1}, "modpackOverrides": ["a"], "installedModpack": {}});
        merge_pack(&mut doc, &json!({"id": 2, "fileName": "p.zip"}), None, None, "now");
        assert_eq!(doc["manifest"], json!({"v": 1}));
        assert_eq!(doc["modpackOverrides"], json!(["a"]));
        assert_eq!(doc["installedModpack"]["installedFile"]["id"], 2);
    }

    #[test]
    fn lock_changes_list_disagreements() {
        use crate::instances::Addon;
        let doc = json!({"installedAddons": [
            {"fileNameOnDisk": "a.jar", "isLocked": false},
            {"fileNameOnDisk": "b.jar.disabled", "isLocked": true},
            {"fileNameOnDisk": "c.jar", "isLocked": true},
            {"fileNameOnDisk": "gone.jar", "isLocked": true},
        ]});
        let add = |f: &str, locked| Addon { file_name: f.into(), locked, ..Default::default() };
        let inst = Instance {
            addons: vec![add("a.jar", true), add("b.jar", false), add("c.jar", true)],
            ..Default::default()
        };
        assert_eq!(lock_changes(&doc, &inst), vec![(0, true), (1, false)]);
    }
}
