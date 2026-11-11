//! The MCP server's server tools: the servers Hopper knows, their live
//! state and start/stop progress from their panel, the Sync tab's
//! comparison with the paired pack, the list message (MOTD), and (with
//! changes allowed and `confirm: true`) start, stop and restart.

use crate::mcp_data::json_reply;
use crate::store::Store;
use gpui_kit::*;
use hopper::mcp::{Reply, text};
use hopper::panel::{PanelAction, PanelStats, ServerPanel};
use hopper::server::ServerInfo;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::mpsc::Sender;

pub fn answer(store: &Entity<Store>, name: &str, args: Value, reply: Sender<Reply>, cx: &mut App) {
    let s = store.read(cx);
    if name == "list_servers" {
        let _ = reply.send(list_servers(s));
        return;
    }
    let server = match find_server(s, args["server"].as_str().unwrap_or("")) {
        Ok(sv) => sv.clone(),
        Err(e) => {
            let _ = reply.send(Err(e));
            return;
        }
    };
    match name {
        "server_sync" => {
            let Some(inst) = s.paired(&server).cloned() else {
                let _ = reply.send(Err(format!("{} isn't paired with an instance (pair it on its Sync tab).", server.name)));
                return;
            };
            let anyway = s.settings().sync_anyway.clone();
            cx.background_spawn(async move { let _ = reply.send(sync(&server, &inst, &anyway, &args)); }).detach();
        }
        "server_motd" => {
            let live = s.panel_stats(&server.root).map(|p| hopper::hosting::motd::Live {
                players: Some(p.online),
                max_players: Some(p.max).filter(|m| *m > 0),
                day: None,
            });
            cx.background_spawn(async move { let _ = reply.send(motd(&server, live.unwrap_or_default(), &args)); }).detach();
        }
        "server_status" | "server_action" => {
            let action = if name == "server_action" {
                match hopper::mcp::server_action_of(&args) {
                    Ok(a) => Some(a),
                    Err(e) => {
                        let _ = reply.send(Err(e));
                        return;
                    }
                }
            } else {
                None
            };
            let Some(panel) = panel_for(s, &server) else {
                let _ = reply.send(Err(format!(
                    "{} has no panel Hopper can reach (set one up in Settings > Servers).",
                    server.name
                )));
                return;
            };
            cx.background_spawn(async move {
                let r = match action {
                    Some(a) => act(&*panel, &server, a),
                    None => status(&*panel, &server),
                };
                let _ = reply.send(r);
            })
            .detach();
        }
        other => {
            let _ = reply.send(Err(format!("unknown tool {other}")));
        }
    }
}

/// The server whose name (or folder name) is `q`, or contains it (one match).
pub fn find_server<'a>(s: &'a Store, q: &str) -> Result<&'a ServerInfo, String> {
    let q = q.trim().to_lowercase();
    if q.is_empty() {
        return Err("server is needed (see list_servers).".into());
    }
    let all = s.servers();
    let folder = |sv: &ServerInfo| sv.root.file_name().map(|f| f.to_string_lossy().to_lowercase()).unwrap_or_default();
    if let Some(sv) = all.iter().find(|sv| sv.name.to_lowercase() == q || folder(sv) == q) {
        return Ok(sv);
    }
    let hits: Vec<&ServerInfo> =
        all.iter().filter(|sv| sv.name.to_lowercase().contains(&q) || folder(sv).contains(&q)).collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] => Err(format!("No server matches \"{q}\". list_servers shows their names.")),
        many => Err(format!(
            "\"{q}\" matches {}: {}. Use more of the name.",
            many.len(),
            many.iter().map(|sv| sv.name.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// The panel that runs `server`, as the server page's controls make it.
pub(crate) fn panel_for(s: &Store, server: &ServerInfo) -> Option<Arc<dyn ServerPanel>> {
    if s.is_hosted(&server.root) {
        return Some(s.local_panel());
    }
    if let Some(r) = &server.remote {
        hopper::panel::hopperd_token_for(&r.panel)?;
        let c = hopper::hopperd::Client::for_remote(r).ok()?;
        return Some(Arc::new(hopper::panel::HopperServer::from_client(c)));
    }
    let p = hopper::panel::known_panel(s.settings(), &server.root)?;
    hopper::panel::token_for(&p.id)?;
    hopper::panel::open(&p).map(Into::into)
}

/// The server's id in its panel.
pub(crate) fn panel_id(panel: &dyn ServerPanel, server: &ServerInfo) -> Result<String, String> {
    if let Some(r) = &server.remote {
        return Ok(r.id.clone());
    }
    let list = panel.servers().map_err(|e| format!("{e:#}"))?;
    hopper::panel::match_server(&list, server)
        .map(|p| p.id.clone())
        .ok_or(format!("{} doesn't list {}.", panel.name(), server.name))
}

fn status_word(p: &PanelStats) -> &'static str {
    if p.crashed {
        "crashed"
    } else if p.stopping {
        "stopping"
    } else if p.starting {
        "starting"
    } else if p.running {
        "running"
    } else {
        "stopped"
    }
}

fn stats_json(p: &PanelStats) -> Value {
    json!({
        "state": status_word(p),
        "players": { "online": p.online, "max": p.max, "names": p.players },
        "cpu_percent": p.cpu,
        "memory": p.mem,
        "memory_percent": p.mem_percent,
        "tps": p.tps.as_ref().map(|t| json!({ "tps": t.tps, "mspt": t.mspt })),
        "no_tps_because": p.no_tps,
        "version": p.version,
        "world_size": p.world_size,
        "started": p.started,
        "addresses": p.addresses,
        "sleeps_after_minutes": p.sleep_minutes,
        "reached_via": p.via,
        "progress": p.progress,
        "just_finished": p.finished,
    })
}

fn list_servers(s: &Store) -> Reply {
    let v: Vec<Value> = s
        .servers()
        .iter()
        .map(|sv| {
            let ping = s.status(&sv.root).map(|st| match st {
                hopper::server::Status::Online { players, max, version, ms } => {
                    json!({ "online": true, "players": players, "max": max, "version": version, "ms": ms })
                }
                hopper::server::Status::Offline => json!({ "online": false }),
            });
            json!({
                "name": sv.name,
                "folder": sv.root.display().to_string(),
                "panel": if s.is_hosted(&sv.root) { "Hopper (this PC)".to_string() }
                    else if let Some(r) = &sv.remote { format!("Hopper Server {}", r.base) }
                    else { format!("{:?}", sv.manager) },
                "loader": sv.loader,
                "java": sv.java,
                "max_memory": sv.max_memory,
                "port": sv.port(),
                "paired_instance": s.paired(sv).map(|i| i.name.clone()),
                "ping": ping,
                "panel_state": s.panel_stats(&sv.root).map(|p| status_word(p)),
            })
        })
        .collect();
    json_reply(json!(v))
}

fn status(panel: &dyn ServerPanel, server: &ServerInfo) -> Reply {
    let id = panel_id(panel, server)?;
    let p = panel.stats(&id).map_err(|e| format!("{e:#}"))?;
    let mut v = stats_json(&p);
    v["server"] = json!(server.name);
    v["panel"] = json!(panel.name());
    json_reply(v)
}

fn act(panel: &dyn ServerPanel, server: &ServerInfo, action: PanelAction) -> Reply {
    let id = panel_id(panel, server)?;
    panel.act(&id, action).map_err(|e| format!("{e:#}"))?;
    let doing = match action {
        PanelAction::Start => "Starting",
        PanelAction::Stop => "Stopping",
        _ => "Restarting",
    };
    Ok(vec![text(format!("{doing} {} through {}. server_status shows its progress.", server.name, panel.name()))])
}

fn sync(server: &ServerInfo, inst: &hopper::instances::Instance, anyway: &std::collections::HashSet<u64>, args: &Value) -> Reply {
    use hopper::model::DiffStatus;
    let (rows, note) = hopper::rigsync::compare(inst, server, anyway)?;
    let mut counts = serde_json::Map::new();
    for r in &rows {
        let k = hopper::rigsync::row_label(r).to_string();
        let n = counts.get(&k).and_then(Value::as_u64).unwrap_or(0);
        counts.insert(k, json!(n + 1));
    }
    let side = |m: &Option<hopper::model::ModInfo>| m.as_ref().map(|m| json!({ "file": m.file_name, "version": m.version, "mod_id": m.mod_id, "off": m.off }));
    let row = |r: &hopper::model::DiffEntry| {
        json!({
            "mod": r.mod_name,
            "state": hopper::rigsync::row_label(r),
            "change": hopper::rigsync::row_change(r),
            "pack": side(&r.client),
            "server": side(&r.server),
            "stays_off_server": hopper::rigsync::is_wont(r),
            "why_skipped": r.skip_reason,
            "about": hopper::rigsync::explain(r),
        })
    };
    let include_same = args["include_same"].as_bool() == Some(true);
    json_reply(json!({
        "server": server.name,
        "pack": inst.name,
        "server_neoforge": hopper::server_update::server_neoforge(&server.root),
        "pack_loader": inst.loader,
        "counts": counts,
        "updates": rows.iter().filter(|r| r.status == DiffStatus::ClientNewer).map(row).collect::<Vec<_>>(),
        "mods": rows.iter().filter(|r| include_same || r.status != DiffStatus::Same).map(row).collect::<Vec<_>>(),
        "flagged_wont_sync": note.flagged,
        "note": note.note,
    }))
}

/// A message without its § colour and style codes.
fn plain(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '§' {
            it.next();
        } else {
            out.push(c);
        }
    }
    out
}

fn motd(server: &ServerInfo, live: hopper::hosting::motd::Live, args: &Value) -> Reply {
    use hopper::hosting::motd;
    let root = &server.root;
    let saved = motd::template(root);
    let t = args["template"].as_str().map(motd::normalize).unwrap_or_else(|| saved.clone());
    let vars = motd::vars(root, &live);
    let filled = if motd::has_vars(&t) { motd::expand(&t, &vars) } else { t.clone() };
    let props = hopper::hosting::properties::Properties::load(&root.join("server.properties"))
        .ok()
        .and_then(|p| p.get("motd").map(str::to_string));
    json_reply(json!({
        "server": server.name,
        "template": t,
        "saved_template": saved,
        "preview": filled,
        "preview_plain": plain(&filled),
        "in_server_properties": props,
        "fills_in_while_running": motd::is_dynamic(&t),
        "variables": vars,
        "note": if server.remote.is_some() { "On a Hopper Server: read from Hopper's copy of its files." } else { "" },
    }))
}
