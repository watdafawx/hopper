//! The MCP server's data tools: what an AI assistant can read from Hopper
//! (instances, mods, logs, crash reports, missing dependencies), look up on
//! CurseForge and Modrinth, and, when the user allows it, change (install,
//! switch on or off, update). See `hopper::mcp` for the tool list.
//!
//! Anything slow (files, the sites) runs in the background; the reply goes
//! back through the request's sender when it's ready.

use crate::app::AppView;
use crate::store::{Msg, Store};
use gpui_kit::*;
use hopper::cf::{self, Loader};
use hopper::instances::Instance;
use hopper::mcp::{Reply, text};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

/// Answers data tool `name` with `args`, now or from the background.
pub fn answer(app: &mut AppView, name: &str, args: Value, reply: Sender<Reply>, cx: &mut Context<AppView>) {
    let store = app.store.clone();
    let r = match name {
        "list_instances" => Some(list_instances(store.read(cx))),
        "list_mods" => Some(list_mods(store.read(cx), &args)),
        "set_mods_enabled" => Some(set_enabled(&store, &args, cx)),
        "update_mods" => Some(update_mods(&store, &args, cx)),
        "install_project" => Some(install(&store, &args, cx)),
        _ => None,
    };
    if let Some(r) = r {
        let _ = reply.send(r);
        return;
    }
    // The rest read files or ask the sites: off the UI thread.
    let inst = args["instance"].as_str().and_then(|q| find(store.read(cx), q).ok()).cloned();
    let want = args["instance"].as_str().map(str::to_string);
    let name = name.to_string();
    cx.background_spawn(async move {
        let needs_instance = matches!(name.as_str(), "read_log" | "list_crash_reports" | "missing_dependencies");
        let r = match (&inst, needs_instance) {
            (None, true) => Err(no_instance(want.as_deref().unwrap_or(""))),
            _ => match name.as_str() {
                "read_log" => read_log(inst.as_ref().expect("checked"), &args),
                "list_crash_reports" => crash_reports(inst.as_ref().expect("checked")),
                "missing_dependencies" => missing(inst.as_ref().expect("checked")),
                "search_projects" => search(inst.as_ref(), &args),
                "project_info" => project_info(inst.as_ref(), &args),
                other => Err(format!("unknown tool {other}")),
            },
        };
        let _ = reply.send(r);
    })
    .detach();
}

/// Data as one JSON text block.
fn json_reply(v: Value) -> Reply {
    Ok(vec![text(serde_json::to_string_pretty(&v).unwrap_or_default())])
}

fn no_instance(q: &str) -> String {
    format!("No instance matches \"{q}\". list_instances shows their names.")
}

/// The instance whose name is `q`, or contains it (one match).
fn find<'a>(store: &'a Store, q: &str) -> Result<&'a Instance, String> {
    let q = q.trim().to_lowercase();
    let all = store.instances();
    if let Some(i) = all.iter().find(|i| i.name.to_lowercase() == q) {
        return Ok(i);
    }
    let hits: Vec<&Instance> = all.iter().filter(|i| i.name.to_lowercase().contains(&q)).collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] => Err(no_instance(&q)),
        many => Err(format!(
            "\"{q}\" matches {}: {}. Use more of the name.",
            many.len(),
            many.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

fn class_of(kind: Option<&str>) -> u32 {
    match kind.unwrap_or("mods") {
        "modpacks" => cf::CLASS_MODPACKS,
        "resourcepacks" => cf::CLASS_RESOURCE_PACKS,
        "shaders" => cf::CLASS_SHADERS,
        "datapacks" => cf::CLASS_DATAPACKS,
        _ => cf::CLASS_MODS,
    }
}

fn site_of(id: u64) -> &'static str {
    if hopper::modrinth::is_mr(id) { "modrinth" } else { "curseforge" }
}

fn updates_of(store: &Store, root: &Path) -> Vec<hopper::install::Update> {
    store.updates(root).and_then(|u| u.ready()).cloned().unwrap_or_default()
}

fn list_instances(store: &Store) -> Reply {
    let v: Vec<Value> = store
        .instances()
        .iter()
        .map(|i| {
            json!({
                "name": i.name,
                "minecraft": i.game_version,
                "loader": i.loader,
                "modpack": i.pack.as_ref().map(|p| json!({ "name": p.name, "version": p.version, "id": p.project_id, "site": site_of(p.project_id) })),
                "folder": i.root.display().to_string(),
                "mods": i.addons.iter().filter(|a| crate::addons::in_class(a, cf::CLASS_MODS)).count(),
                "mods_off": i.addons.iter().filter(|a| crate::addons::in_class(a, cf::CLASS_MODS) && !a.enabled).count(),
                "updates_waiting": updates_of(store, &i.root).len(),
                "running": store.running(&i.root).is_some(),
            })
        })
        .collect();
    json_reply(json!(v))
}

fn list_mods(store: &Store, args: &Value) -> Reply {
    let inst = find(store, args["instance"].as_str().unwrap_or(""))?;
    let class = class_of(args["kind"].as_str());
    let filter = args["filter"].as_str().map(str::to_lowercase);
    let updates = updates_of(store, &inst.root);
    let v: Vec<Value> = inst
        .addons
        .iter()
        .filter(|a| crate::addons::in_class(a, class))
        .filter(|a| {
            filter
                .as_ref()
                .is_none_or(|f| a.name.to_lowercase().contains(f) || a.file_name.to_lowercase().contains(f))
        })
        .filter_map(|a| {
            let upd = updates.iter().find(|u| u.project_id == a.project_id);
            if args["only_updates"].as_bool() == Some(true) && upd.is_none() {
                return None;
            }
            if args["only_disabled"].as_bool() == Some(true) && a.enabled {
                return None;
            }
            Some(json!({
                "name": a.name,
                "file": a.file_name,
                "version": hopper::scan::short_version(&a.file_name),
                "enabled": a.enabled,
                "side": a.env,
                "id": a.project_id,
                "site": if a.project_id == 0 { "none" } else if a.mr_project.is_empty() { "curseforge" } else { "modrinth" },
                "author": a.author,
                "from_modpack": a.from_pack,
                "update_to": upd.map(|u| u.file.file_name.clone()),
            }))
        })
        .collect();
    json_reply(json!({ "instance": inst.name, "count": v.len(), "items": v }))
}

/// The last `lines` lines of `text` (those containing `grep` when given).
fn tail(text: &str, lines: usize, grep: Option<&str>) -> String {
    let grep = grep.map(str::to_lowercase);
    let picked: Vec<&str> = text
        .lines()
        .filter(|l| grep.as_ref().is_none_or(|g| l.to_lowercase().contains(g)))
        .collect();
    picked[picked.len().saturating_sub(lines)..].join("\n")
}

fn crash_dir(inst: &Instance) -> PathBuf {
    inst.root.join("crash-reports")
}

/// Crash reports, newest first.
fn crash_files(inst: &Instance) -> Vec<(PathBuf, std::time::SystemTime)> {
    let mut v: Vec<(PathBuf, std::time::SystemTime)> = std::fs::read_dir(crash_dir(inst))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| Some((e.path(), e.metadata().ok()?.modified().ok()?)))
        .filter(|(p, _)| p.extension().is_some_and(|x| x == "txt"))
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}

fn read_log(inst: &Instance, args: &Value) -> Reply {
    let lines = args["lines"].as_u64().unwrap_or(300).clamp(1, 3000) as usize;
    let which = args["file"].as_str().unwrap_or("latest");
    let path = match which {
        "debug" => inst.root.join("logs/debug.log"),
        "crash" => crash_files(inst).into_iter().next().map(|(p, _)| p).ok_or("This instance has no crash reports.")?,
        "latest" => inst.root.join("logs/latest.log"),
        // A crash report by its file name.
        name => crash_dir(inst).join(Path::new(name).file_name().unwrap_or_default()),
    };
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let body = tail(&String::from_utf8_lossy(&bytes), lines, args["grep"].as_str());
    Ok(vec![text(format!("{} (last {lines} lines)\n\n{body}", path.display()))])
}

fn crash_reports(inst: &Instance) -> Reply {
    let v: Vec<Value> = crash_files(inst)
        .into_iter()
        .take(30)
        .map(|(p, t)| {
            let secs = t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
            json!({ "file": p.file_name().unwrap_or_default().to_string_lossy(), "date": hopper::time::iso_from_secs(secs) })
        })
        .collect();
    json_reply(json!(v))
}

fn missing(inst: &Instance) -> Reply {
    let v: Vec<Value> = hopper::deps::missing(&inst.mods_dir())
        .into_iter()
        .map(|m| {
            json!({
                "mod": m.mod_name,
                "file": m.file_name,
                "needs": m.needs,
                "versions": m.versions,
                "installed_but_wrong_version": m.wrong_version.map(|(jar, v)| json!({ "file": jar, "version": v })),
                "a_disabled_file_has_it": m.disabled_provider,
            })
        })
        .collect();
    json_reply(json!(v))
}

fn loader_arg(args: &Value, inst: Option<&Instance>) -> Loader {
    match args["loader"].as_str() {
        Some(l) => Loader::ALL.iter().copied().find(|x| x.name().eq_ignore_ascii_case(l)).unwrap_or(Loader::Any),
        None => inst.map_or(Loader::Any, hopper::install::loader_of),
    }
}

fn mod_json(m: &cf::Mod) -> Value {
    json!({
        "id": m.id,
        "name": m.name,
        "site": if m.modrinth.is_some() { "modrinth" } else { "curseforge" },
        "summary": m.summary,
        "authors": m.authors.iter().map(|a| a.name.clone()).collect::<Vec<_>>(),
        "downloads": m.download_count as u64,
        "categories": m.categories.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
        "updated": m.date_modified,
        "page": m.page(),
    })
}

fn search(inst: Option<&Instance>, args: &Value) -> Reply {
    let s = cf::Search {
        class_id: class_of(args["kind"].as_str()),
        text: args["query"].as_str().unwrap_or("").to_string(),
        game_version: args["minecraft"]
            .as_str()
            .map(str::to_string)
            .or_else(|| inst.map(|i| i.game_version.clone()))
            .unwrap_or_default(),
        loader: loader_arg(args, inst),
        ..Default::default()
    };
    let r = match args["site"].as_str() {
        Some("modrinth") => hopper::modrinth::search(&s),
        _ => cf::search(&s),
    };
    let (mods, _) = r.map_err(|e| format!("{e:#}"))?;
    json_reply(json!(mods.iter().take(20).map(mod_json).collect::<Vec<_>>()))
}

fn project_info(inst: Option<&Instance>, args: &Value) -> Reply {
    let id = args["id"].as_u64().ok_or("id is needed")?;
    let m = hopper::source::get_mod(id).map_err(|e| format!("{e:#}"))?;
    let gv = inst.map(|i| i.game_version.clone()).unwrap_or_default();
    let files: Vec<Value> = hopper::source::files(id, &gv, loader_arg(args, inst))
        .unwrap_or_default()
        .iter()
        .take(10)
        .map(|f| {
            json!({
                "file": f.file_name,
                "name": f.display_name,
                "type": f.release_name(),
                "date": f.file_date,
                "game_versions": f.game_versions,
            })
        })
        .collect();
    let mut v = mod_json(&m);
    v["files"] = json!(files);
    json_reply(v)
}

fn set_enabled(store: &Entity<Store>, args: &Value, cx: &mut App) -> Reply {
    let root = find(store.read(cx), args["instance"].as_str().unwrap_or(""))?.root.clone();
    let files: Vec<String> = args["files"]
        .as_array()
        .map(|a| a.iter().filter_map(|f| f.as_str().map(|s| s.trim_end_matches(".disabled").to_string())).collect())
        .unwrap_or_default();
    let on = args["enabled"].as_bool().ok_or("enabled is needed")?;
    if let Some(why) = store.read(cx).edit_hold_reason(&root) {
        return Err(format!("Can't change it now: {why}."));
    }
    store.update(cx, |s, cx| {
        let r = s.set_enabled(&root, &files, on);
        cx.notify();
        r
    })?;
    Ok(vec![text(format!("Switched {} {}.", files.len(), if on { "on" } else { "off" }))])
}

fn update_mods(store: &Entity<Store>, args: &Value, cx: &mut App) -> Reply {
    let s = store.read(cx);
    let inst = find(s, args["instance"].as_str().unwrap_or(""))?;
    let root = inst.root.clone();
    let ids: Option<Vec<u64>> = args["ids"].as_array().map(|a| a.iter().filter_map(Value::as_u64).collect());
    let picked: Vec<hopper::install::Update> = updates_of(s, &root)
        .into_iter()
        .filter(|u| ids.as_ref().is_none_or(|ids| ids.contains(&u.project_id)))
        .collect();
    if picked.is_empty() {
        return Err("No updates waiting for those (list_mods with only_updates shows them).".into());
    }
    if !s.queues_updates(&root)
        && let Some(why) = s.edit_hold_reason(&root)
    {
        return Err(format!("Can't update now: {why}."));
    }
    let n = picked.len();
    s.run_updates(&root, picked);
    Ok(vec![text(format!("Updating {n} in Hopper (a task; switched-off mods stay off)."))])
}

fn install(store: &Entity<Store>, args: &Value, cx: &mut App) -> Reply {
    let s = store.read(cx);
    let inst = find(s, args["instance"].as_str().unwrap_or(""))?.clone();
    let id = args["id"].as_u64().ok_or("id is needed")?;
    if let Some(why) = s.edit_hold_reason(&inst.root) {
        return Err(format!("Can't install now: {why}."));
    }
    let root = inst.root.clone();
    let instances_root = PathBuf::from(&s.settings().instances_root);
    s.spawn_instance_job(&root, format!("Installing project {id} into {}", inst.name), move |job| {
        let m = hopper::source::get_mod(id)?;
        let class = m.class_id.unwrap_or(cf::CLASS_MODS);
        let place = hopper::browse::Placement { instances_root: &instances_root, target: Some(inst), dest: None };
        let (changed, line) = hopper::browse::install_project(&m, None, class, place, job)?;
        job.detail(line);
        Ok(Msg::InstanceChanged(changed))
    });
    Ok(vec![text("Installing in Hopper (a task, with required dependencies). list_mods shows it once done.")])
}
