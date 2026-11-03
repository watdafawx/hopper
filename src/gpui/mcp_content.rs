//! The MCP server's tools for content beyond the mod list: packs in every
//! place they load from, website-only downloads, mods from GitHub and
//! other links (look up, add, follow, stop following), Hopper's reading of
//! crashes and join failures, and settings. See `hopper::mcp` for the
//! tool list and `mcp_data` for the rest.
//!
//! Sources keep the app's trust rule: an assistant can add or follow only
//! a source the player already trusted on this PC; a new one needs the
//! player's yes in Hopper ([`hopper::mcp::source_gate`]).

use crate::mcp_data::{find, json_reply, no_instance, updates_of};
use crate::store::{Msg, Store};
use gpui_kit::*;
use hopper::cf;
use hopper::instances::Instance;
use hopper::mcp::{Reply, source_gate, text};
use hopper::sources::{self, Matched, ModSource};
use hopper::webfetch::Step;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::mpsc::Sender;

pub fn answer(store: &Entity<Store>, name: &str, args: Value, reply: Sender<Reply>, cx: &mut App) {
    let r = match name {
        "download_status" => Some(download_status(store.read(cx), &args)),
        "read_settings" => Some(read_settings(store.read(cx), &args)),
        "unfollow_source" => Some(unfollow(store, &args, cx)),
        _ => None,
    };
    let s = store.read(cx);
    if let Some(r) = r {
        let _ = reply.send(r);
        return;
    }
    let inst = match args["instance"].as_str() {
        Some(q) => match find(s, q) {
            Ok(i) => Some(i.clone()),
            Err(e) => {
                let _ = reply.send(Err(e));
                return;
            }
        },
        None => None,
    };
    let name = name.to_string();
    match name.as_str() {
        "follow_source" | "add_from_source" => {
            let Some(inst) = inst else {
                let _ = reply.send(Err(no_instance("")));
                return;
            };
            change_source(store, &name, inst, args, reply, cx);
        }
        "crash_analysis" => {
            let server = match args["server"].as_str() {
                Some(q) => match crate::mcp_servers::find_server(s, q) {
                    Ok(sv) => Some(sv.root.clone()),
                    Err(e) => {
                        let _ = reply.send(Err(e));
                        return;
                    }
                },
                None => None,
            };
            cx.background_spawn(async move {
                let _ = reply.send(crash_analysis(inst.as_ref(), server, &args));
            })
            .detach();
        }
        _ => {
            let Some(inst) = inst else {
                let _ = reply.send(Err("instance is needed (see list_instances).".into()));
                return;
            };
            let updates = updates_of(s, &inst.root);
            cx.background_spawn(async move {
                let r = match name.as_str() {
                    "list_packs" => list_packs(&inst, &updates, &args),
                    "unknown_mods" => unknown_mods(&inst),
                    "look_up_source" => look_up(&inst, &args),
                    other => Err(format!("unknown tool {other}")),
                };
                let _ = reply.send(r);
            })
            .detach();
        }
    }
}

/// A source as list_mods and the source tools show it.
pub fn source_json(s: &ModSource) -> Value {
    json!({
        "kind": s.kind.label(),
        "where": s.url,
        "page": s.page(),
        "publisher": s.publisher,
        "release": s.seen,
        "sha256": s.sha256,
        "file_pattern": s.pattern,
        "prerelease": s.prerelease,
        "trusted": sources::trusted(s),
    })
}

/// A website-only download's step, in words.
pub fn fetch_step(step: Step, size: u64) -> String {
    match step {
        Step::Queued => "waiting its turn".into(),
        Step::Opening => "opening CurseForge".into(),
        Step::Check | Step::Countdown => "waiting for CurseForge".into(),
        Step::Downloading(_) => match step.progress(size) {
            Some(p) => format!("downloading {}%", (p * 100.0).floor() as u32),
            None => "downloading".into(),
        },
        Step::Done => "done".into(),
        Step::Failed => "didn't come: the page gave no file in time".into(),
        Step::Closed => "didn't come: the download window was closed".into(),
        Step::Unusable => "came, but isn't the file CurseForge lists, so Hopper didn't use it".into(),
    }
}

fn download_status(s: &Store, args: &Value) -> Reply {
    let roots: Vec<(String, PathBuf)> = match args["instance"].as_str() {
        Some(q) => {
            let i = find(s, q)?;
            vec![(i.name.clone(), i.root.clone())]
        }
        None => s.instances().iter().map(|i| (i.name.clone(), i.root.clone())).collect(),
    };
    let mut out = Vec::new();
    for (name, root) in roots {
        let (view, files) = s.fetch_files(&root);
        if files.is_empty() {
            continue;
        }
        out.push(json!({
            "instance": name,
            "status": view.as_ref().map(|v| v.button_label()),
            "files": files.iter().map(|f| json!({
                "name": f.name,
                "project": f.project_id,
                "file_id": f.file_id,
                "page": f.url,
                "step": fetch_step(f.shown_step(), f.size),
                "for_project": f.by,
            })).collect::<Vec<_>>(),
        }));
    }
    if out.is_empty() {
        return Ok(vec![text("No website-only downloads waiting or under way.")]);
    }
    json_reply(json!(out))
}

fn read_settings(s: &Store, args: &Value) -> Reply {
    let all = hopper::mcp::redact_settings(serde_json::to_value(s.settings()).map_err(|e| e.to_string())?);
    let v = match args["keys"].as_array() {
        Some(keys) => {
            let mut m = serde_json::Map::new();
            for k in keys.iter().filter_map(Value::as_str) {
                m.insert(k.to_string(), all.get(k).cloned().unwrap_or(Value::Null));
            }
            Value::Object(m)
        }
        None => all,
    };
    json_reply(v)
}

/// The class and its file names as the game lists them.
fn class_of(kind: &str) -> Result<u32, String> {
    match kind {
        "resourcepacks" => Ok(cf::CLASS_RESOURCE_PACKS),
        "shaders" => Ok(cf::CLASS_SHADERS),
        "datapacks" => Ok(cf::CLASS_DATAPACKS),
        other => Err(format!("kind must be resourcepacks, shaders or datapacks, not \"{other}\"")),
    }
}

fn list_packs(inst: &Instance, updates: &[hopper::install::Update], args: &Value) -> Reply {
    let class = class_of(args["kind"].as_str().unwrap_or(""))?;
    // The game's resource pack list (top wins first) and the shader in use.
    let order = hopper::packs::resourcepack_order(inst);
    let shader = hopper::instance_ops::active_shader(inst);
    let locations: Vec<Value> = hopper::packs::locations(inst, class)
        .into_iter()
        .map(|loc| {
            let packs: Vec<Value> = hopper::instance_ops::datapacks(&loc.dir)
                .into_iter()
                .map(|p| {
                    let on_disk = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
                    let enabled = !on_disk.to_ascii_lowercase().ends_with(".disabled");
                    let file = on_disk.strip_suffix(".disabled").unwrap_or(&on_disk).to_string();
                    let active = match class {
                        cf::CLASS_RESOURCE_PACKS if loc.key == "instance" => order.iter().position(|o| *o == file).map(|i| json!(i + 1)),
                        cf::CLASS_SHADERS => Some(json!(shader.as_deref() == Some(file.as_str()))),
                        // Global folders and data packs load whatever is on.
                        _ => Some(json!(enabled)),
                    };
                    let addon = inst.addons.iter().find(|a| a.class_id == class && a.file_name.eq_ignore_ascii_case(&file));
                    let upd = addon.and_then(|a| updates.iter().find(|u| u.project_id == a.project_id && a.project_id != 0));
                    // A pack added from GitHub or a link: its source and newer version.
                    if let Some((r, installed, newer)) = sources::packs::update_of(&p) {
                        return json!({
                            "file": file,
                            "folder": false,
                            "enabled": enabled,
                            "active": active.unwrap_or(json!(false)),
                            "name": r.name,
                            "id": installed.mod_id,
                            "source": source_json(&r.source),
                            "version": installed.display_name,
                            "update_to": newer.as_ref().map(|f| f.display_name.clone()),
                            "what_changed": newer.as_ref().and_then(|f| f.source.as_ref()).map(|s| s.notes.clone()),
                        });
                    }
                    json!({
                        "file": file,
                        "folder": p.is_dir(),
                        "enabled": enabled,
                        // Resource packs: place in the game's list (1 = top), absent when not picked.
                        "active": active.unwrap_or(json!(false)),
                        "name": addon.map(|a| a.name.clone()),
                        "id": addon.map(|a| a.project_id),
                        "added_by_you": addon.map(|a| inst.pack.is_none() || !a.from_pack),
                        "update_to": upd.map(|u| u.file.file_name.clone()),
                    })
                })
                .collect();
            json!({
                "location": loc.label,
                "key": loc.key,
                "folder": loc.dir.display().to_string(),
                "every_world": loc.every_world,
                "packs": packs,
            })
        })
        .collect();
    json_reply(json!({
        "instance": inst.name,
        "kind": args["kind"],
        "active_shader": if class == cf::CLASS_SHADERS { json!(shader) } else { Value::Null },
        "locations": locations,
    }))
}

fn unknown_mods(inst: &Instance) -> Reply {
    let v: Vec<Value> = sources::unknown_mods(inst)
        .into_iter()
        .map(|u| json!({ "file": u.file_name, "name": u.name, "suggested_link": u.link }))
        .collect();
    json_reply(json!({
        "instance": inst.name,
        "mod_sources_on": sources::enabled(),
        "count": v.len(),
        "items": v,
    }))
}

fn matched_json(m: &Matched) -> Value {
    match m {
        Matched::Exact { tag } => json!({ "by": "hash", "release": tag, "about": "byte for byte this release's file" }),
        Matched::Baseline { version, tag } if tag.is_empty() => {
            json!({ "by": "version", "version": version, "release": null, "about": "the source has no release this old; newer ones count as updates" })
        }
        Matched::Baseline { version, tag } => {
            json!({ "by": "version", "version": version, "release": tag, "about": "the jar's version; the nearest release at or before it" })
        }
        Matched::Unknown => json!({ "by": "nothing", "about": "the jar names no version; the newest release counts as newer" }),
    }
}

fn candidate_json(c: &sources::Candidate) -> Value {
    json!({
        "file": c.file_name,
        "release": c.version,
        "title": c.title,
        "date": c.date,
        "size": c.size,
        "prerelease": c.prerelease,
        "fits": c.fit.tag(),
        "sha256": c.digest,
        "page": c.page,
    })
}

fn look_up(inst: &Instance, args: &Value) -> Reply {
    let link = args["link"].as_str().ok_or("link is needed")?;
    let pre = args["prerelease"].as_bool().unwrap_or(false);
    if let Some(file) = args["file"].as_str() {
        let a = sources::plan_adopt(inst, file, link, pre).map_err(|e| format!("{e:#}"))?;
        return json_reply(json!({
            "file": file,
            "name": a.name,
            "source": source_json(&a.source),
            "matched": matched_json(&a.matched),
            "update": a.newest.as_ref().map(candidate_json),
            "follow_would": "keep the jar as it is; nothing downloads",
        }));
    }
    let loader = hopper::install::loader_of(inst);
    let r = sources::resolve(link, &inst.game_version, loader, pre).map_err(|e| format!("{e:#}"))?;
    json_reply(json!({
        "name": r.name,
        "source": source_json(&r.source),
        "files": r.candidates.iter().map(candidate_json).collect::<Vec<_>>(),
        "pick": r.pick.and_then(|i| r.candidates.get(i)).map(|c| c.file_name.clone()),
        "ambiguous": r.ambiguous,
    }))
}

/// follow_source and add_from_source: looked up in the background, then
/// (when the source passes the trust rule) run as a task like the dialog.
fn change_source(store: &Entity<Store>, name: &str, inst: Instance, args: Value, reply: Sender<Reply>, cx: &mut App) {
    let allowed = store.read(cx).settings().allow_sources;
    if let Err(e) = source_gate(allowed, true, "") {
        let _ = reply.send(Err(e));
        return;
    }
    if let Some(why) = store.read(cx).edit_hold_reason(&inst.root) {
        let _ = reply.send(Err(format!("Can't change it now: {why}.")));
        return;
    }
    let Some(link) = args["link"].as_str().map(str::to_string) else {
        let _ = reply.send(Err("link is needed".into()));
        return;
    };
    let pre = args["prerelease"].as_bool().unwrap_or(false);
    let store = store.clone();
    if name == "follow_source" {
        let Some(file) = args["file"].as_str().map(str::to_string) else {
            let _ = reply.send(Err("file is needed".into()));
            return;
        };
        let (i2, f2, l2) = (inst.clone(), file.clone(), link.clone());
        let plan = cx.background_spawn(async move { sources::plan_adopt(&i2, &f2, &l2, pre).map_err(|e| format!("{e:#}")) });
        cx.spawn(async move |cx| {
            let r = match plan.await {
                Err(e) => Err(e),
                Ok(a) => cx
                    .update(|cx| {
                        source_gate(true, sources::trusted(&a.source), &a.source.publisher)?;
                        let root = inst.root.clone();
                        let label = a.name.clone();
                        let out = json!({
                            "file": file,
                            "source": source_json(&a.source),
                            "matched": matched_json(&a.matched),
                            "update": a.newest.as_ref().map(candidate_json),
                            "done": "Following in Hopper (a task). The jar stays as it is.",
                        });
                        store.update(cx, |s, _| {
                            s.spawn_instance_job(&root.clone(), format!("{label} follows {}", a.source.publisher), move |job| {
                                let mut inst = hopper::instances::load(&root).ok_or_else(|| anyhow::anyhow!("the instance is gone"))?;
                                sources::adopt(&mut inst, &file, &a)?;
                                job.detail(match &a.newest {
                                    Some(c) => format!("{label} follows {}. {} is out: it's in Updates.", a.source.publisher, c.version),
                                    None => format!("{label} follows {}. It's up to date.", a.source.publisher),
                                });
                                Ok(Msg::InstanceChanged(root))
                            });
                        });
                        json_reply(out)
                    }),
            };
            let _ = reply.send(r);
        })
        .detach();
        return;
    }
    if let Some(kind) = args["kind"].as_str() {
        add_pack(store, inst, kind, &link, pre, &args, reply, cx);
        return;
    }
    // add_from_source
    let asset = args["asset"].as_str().map(str::to_string);
    let (mc, loader) = (inst.game_version.clone(), hopper::install::loader_of(&inst));
    let l2 = link.clone();
    let found = cx.background_spawn(async move { sources::resolve(&l2, &mc, loader, pre).map_err(|e| format!("{e:#}")) });
    cx.spawn(async move |cx| {
        let r = match found.await {
            Err(e) => Err(e),
            Ok(r) => cx
                .update(|cx| {
                    source_gate(true, sources::trusted(&r.source), &r.source.publisher)?;
                    let idx = match &asset {
                        Some(a) => r.candidates.iter().position(|c| c.file_name.eq_ignore_ascii_case(a)).ok_or(format!(
                            "{a} isn't among the release's files: {}",
                            r.candidates.iter().map(|c| c.file_name.as_str()).collect::<Vec<_>>().join(", ")
                        ))?,
                        None if r.ambiguous => {
                            return Err(format!(
                                "Several files fit; pick one with asset: {}",
                                r.candidates.iter().filter(|c| c.fit.usable()).map(|c| c.file_name.as_str()).collect::<Vec<_>>().join(", ")
                            ));
                        }
                        None => r.pick.ok_or("No file of the newest release fits this instance (look_up_source lists them).")?,
                    };
                    let file = r.candidates[idx].file_name.clone();
                    let root = inst.root.clone();
                    let publisher = r.source.publisher.clone();
                    store.update(cx, |s, _| {
                        s.spawn_instance_job(&root, format!("Adding {file}"), move |job| {
                            let mut inst = inst;
                            let done = sources::install(&mut inst, &r, idx, job)?;
                            job.detail(format!("{done} is in. Updates come from {publisher}."));
                            Ok(Msg::InstanceChanged(inst.root.clone()))
                        });
                    });
                    Ok(vec![text(format!("Adding {file} in Hopper (a task). list_mods shows it once done."))])
                }),
        };
        let _ = reply.send(r);
    })
    .detach();
}

/// add_from_source with `kind`: a pack into one of the instance's pack
/// locations, as the Packs tab's From GitHub or a link does.
#[allow(clippy::too_many_arguments)]
fn add_pack(store: Entity<Store>, inst: Instance, kind: &str, link: &str, pre: bool, args: &Value, reply: Sender<Reply>, cx: &mut App) {
    let class = match class_of(kind) {
        Ok(c) if c != cf::CLASS_DATAPACKS => c,
        Ok(_) => {
            let _ = reply.send(Err("Data packs from links are added per world in Hopper Server; here use resourcepacks or shaders.".into()));
            return;
        }
        Err(e) => {
            let _ = reply.send(Err(e));
            return;
        }
    };
    let want = args["location"].as_str().unwrap_or("instance").to_ascii_lowercase();
    let Some(loc) = hopper::packs::locations(&inst, class)
        .into_iter()
        .find(|l| l.world.is_none() && (l.key.eq_ignore_ascii_case(&want) || l.label.to_ascii_lowercase() == want))
    else {
        let _ = reply.send(Err(format!("No pack location \"{want}\" here (list_packs lists them).")));
        return;
    };
    let asset = args["asset"].as_str().map(str::to_string);
    let (mc, l2) = (inst.game_version.clone(), link.to_string());
    let found = cx.background_spawn(async move { sources::packs::resolve(&l2, &mc, class, pre).map_err(|e| format!("{e:#}")) });
    cx.spawn(async move |cx| {
        let r = match found.await {
            Err(e) => Err(e),
            Ok(r) => cx.update(|cx| {
                source_gate(true, sources::trusted(&r.source), &r.source.publisher)?;
                let names: Vec<String> = r.candidates.iter().map(|c| if c.title.is_empty() { c.file_name.clone() } else { c.title.clone() }).collect();
                let idx = match &asset {
                    Some(a) => r
                        .candidates
                        .iter()
                        .position(|c| c.file_name.eq_ignore_ascii_case(a) || c.title.eq_ignore_ascii_case(a))
                        .ok_or(format!("{a} isn't offered there: {}", names.join(", ")))?,
                    None if r.ambiguous || r.pick.is_none() => return Err(format!("Pick one with asset: {}", names.join(", "))),
                    None => r.pick.unwrap_or(0),
                };
                let (root, dir, place) = (inst.root.clone(), loc.dir.clone(), loc.label.clone());
                let file = r.candidates[idx].file_name.clone();
                let publisher = r.source.publisher.clone();
                store.update(cx, |s, _| {
                    s.spawn_instance_job(&root.clone(), format!("Adding {file}"), move |job| {
                        let new = sources::packs::install(&root, &dir, &r, idx, job)?;
                        let f = new.file_name().unwrap_or_default().to_string_lossy().into_owned();
                        job.detail(format!("{f} is in {place}. Updates come from {publisher}."));
                        Ok(Msg::InstanceChanged(root))
                    });
                });
                Ok(vec![text(format!("Adding {file} to {} in Hopper (a task). list_packs shows it once done.", loc.label))])
            }),
        };
        let _ = reply.send(r);
    })
    .detach();
}

fn unfollow(store: &Entity<Store>, args: &Value, cx: &mut App) -> Reply {
    let s = store.read(cx);
    let inst = find(s, args["instance"].as_str().unwrap_or(""))?;
    let file = args["file"].as_str().ok_or("file is needed")?.to_string();
    let a = inst
        .addons
        .iter()
        .find(|a| a.file_name.eq_ignore_ascii_case(&file))
        .ok_or(format!("No mod {file} in {} (list_mods shows the files).", inst.name))?;
    if a.source.is_none() {
        return Err(format!("{file} follows no source."));
    }
    if let Some(why) = s.edit_hold_reason(&inst.root) {
        return Err(format!("Can't change it now: {why}."));
    }
    crate::source_dialog::unfollow(store.clone(), inst.root.clone(), file.clone(), cx);
    Ok(vec![text(format!("{file} stops following its source (a task). The file stays."))])
}

/// Mod names for a crash's jars, from the instance's list.
fn jar_name(inst: Option<&Instance>, file: &str) -> Option<String> {
    let f = file.trim_end_matches(".disabled");
    inst?.addons.iter().find(|a| a.file_name.eq_ignore_ascii_case(f)).map(|a| a.name.clone()).filter(|n| !n.is_empty())
}

fn crash_json(c: &hopper::crashdetect::Crash, inst: Option<&Instance>) -> Value {
    let mut v = serde_json::to_value(c).unwrap_or_default();
    v["when"] = json!(hopper::time::iso_from_secs(c.when));
    v["certainty_words"] = json!("Certain: this mod caused it. Likely: it very probably did. Possible: it's involved, maybe as a bystander.");
    if let Some(list) = v["suspects"].as_array_mut() {
        for (s, j) in c.suspects.iter().zip(list.iter_mut()) {
            j["name"] = json!(jar_name(inst, &s.file_name));
        }
    }
    if let Some(n) = c.lingered_note() {
        v["lingered_note"] = json!(n);
    }
    v
}

fn crash_analysis(inst: Option<&Instance>, server: Option<PathBuf>, args: &Value) -> Reply {
    if let Some(root) = server {
        let c = hopper::crashscan::last_crash(&root);
        return json_reply(json!({
            "server": root.display().to_string(),
            "crash": c.as_ref().map(|c| crash_json(c, None)),
        }));
    }
    let inst = inst.ok_or("instance or server is needed")?;
    let mods_dir = inst.mods_dir();
    let jars = || -> Vec<String> {
        std::fs::read_dir(&mods_dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".jar") || n.ends_with(".jar.disabled"))
            .collect()
    };
    let ids = |f: &str| hopper::deps::own_ids(&mods_dir.join(f)).0;
    // A named report, else the saved finding read again with today's rules
    // (or the newest report when it's newer than that), as the card does.
    let saved = hopper::crashdetect::load(&inst.root);
    let latest = hopper::logs::crash_reports(&inst.root).into_iter().next();
    let crash = match args["report"].as_str() {
        Some(r) => {
            let p = inst.root.join("crash-reports").join(std::path::Path::new(r).file_name().unwrap_or_default());
            if !p.is_file() {
                return Err(format!("{} isn't there (list_crash_reports shows them).", p.display()));
            }
            hopper::crashdetect::from_report(&inst.root, &p, &jars(), ids)
        }
        None => match (&saved, &latest) {
            (Some(c), Some(l)) if hopper::crashdetect::is_newer(l, c) => hopper::crashdetect::from_report(&inst.root, l, &jars(), ids),
            (Some(c), _) => hopper::crashdetect::reread(&inst.root, c, &jars(), ids).or(saved.clone()),
            (None, _) => None,
        },
    };
    let join = hopper::joinfail::load(&inst.root);
    json_reply(json!({
        "instance": inst.name,
        "crash": crash.as_ref().map(|c| crash_json(c, Some(inst))),
        "join_problem": join.as_ref().map(|p| {
            let mut v = serde_json::to_value(p).unwrap_or_default();
            v["title"] = json!(p.title());
            v["when"] = json!(hopper::time::iso_from_secs(p.when));
            v
        }),
        "note": if crash.is_none() && join.is_none() {
            "No crash or join problem since the last game that ended normally. read_log file=crash has older raw reports."
        } else { "" },
    }))
}
