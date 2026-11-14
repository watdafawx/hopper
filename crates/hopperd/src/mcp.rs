//! An MCP server (Model Context Protocol, Streamable HTTP) at `/api/v1/mcp`,
//! so AI assistants can look after the servers: status, power, console,
//! files, mods, crashes and backups.
//!
//! It signs in like everything else (an API token or a device token as
//! `Authorization: Bearer …`), and each tool call is checked as the
//! matching API request would be: a read-only token can't start servers,
//! a token or person limited to some servers only sees those.
//!
//! A node's servers (design/multi-panel.md, A4) are listed with the rest
//! and every tool works on them: the call becomes the API request it stands
//! for and goes through `relay::forward`, so the node's ceiling, the
//! allowlist and the node's own checks apply exactly as for the web UI.

use crate::App;
use crate::api::{ApiError, require};
use crate::auth::Who;
use crate::files;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use hopper_core::panel::PanelAction;
use serde_json::{Value, json};
use std::sync::Arc;

const PROTOCOL: &str = "2025-06-18";

pub fn routes() -> Router<Arc<App>> {
    Router::new().route("/api/v1/mcp", post(handle).get(no_stream).delete(no_stream))
}

/// No server-sent stream: every answer comes back on its POST.
async fn no_stream() -> Response {
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

fn tool(name: &str, description: &str, props: Value, required: &[&str], read_only: bool) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": props, "required": required },
        "annotations": { "readOnlyHint": read_only },
    })
}

fn tools() -> Vec<Value> {
    let server = json!({ "type": "string", "description": "Server id, from list_servers" });
    let path = json!({ "type": "string", "description": "Path inside the server folder, like config/create.toml" });
    vec![
        tool("list_servers", "Every server: id, name, state, players, memory, version. A node's servers have ids like `ab12cd34ef56:tnp` and name their node.", json!({}), &[], true),
        tool(
            "console",
            "The newest lines of a server's console.",
            json!({ "server": server, "lines": { "type": "integer", "description": "How many (default 100, at most 1000)" } }),
            &["server"],
            true,
        ),
        tool(
            "power",
            "Start, stop, restart or kill a server.",
            json!({ "server": server, "action": { "type": "string", "enum": ["start", "stop", "restart", "kill"] } }),
            &["server", "action"],
            false,
        ),
        tool(
            "send_command",
            "Run a console command on a running server (without the leading slash), like `say hi` or `list`.",
            json!({ "server": server, "command": { "type": "string" } }),
            &["server", "command"],
            false,
        ),
        tool("list_files", "Files and folders in a folder of the server.", json!({ "server": server, "path": path }), &["server"], true),
        tool("read_file", "A text file's contents (up to a few MB).", json!({ "server": server, "path": path }), &["server", "path"], true),
        tool(
            "write_file",
            "Saves a text file (the old version goes to the panel's trash). Checks TOML, JSON and properties files first.",
            json!({ "server": server, "path": path, "content": { "type": "string" } }),
            &["server", "path", "content"],
            false,
        ),
        tool(
            "search_files",
            "Finds text in files (and file names) under a folder. Plain text, any case; or a regular expression.",
            json!({ "server": server, "query": { "type": "string" }, "path": path, "regex": { "type": "boolean" } }),
            &["server", "query"],
            true,
        ),
        tool("list_mods", "The server's mods: name, version, file, whether it's on.", json!({ "server": server }), &["server"], true),
        tool("last_crash", "The server's last crash: the mods it names (Certain, Likely or Possible, with why), the mod that stopped the server from starting, and a mod updated lately that its error names.", json!({ "server": server }), &["server"], true),
        tool("list_backups", "The server's backups, newest first.", json!({ "server": server }), &["server"], true),
    ]
}

fn rpc_ok(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_err(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn text(t: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": t.into() }] })
}

/// The tools this caller can use: someone who may only look (a read-only
/// token, a viewer) isn't offered the ones that change things.
fn tools_for(who: &Who) -> Vec<Value> {
    let looks_only = match who {
        Who::Token { read_only, .. } => *read_only,
        Who::User { role, .. } => *role == crate::auth::Role::Viewer,
        _ => false,
    };
    tools()
        .into_iter()
        .filter(|t| !looks_only || t["annotations"]["readOnlyHint"].as_bool().unwrap_or(false))
        .collect()
}

fn tool_error(t: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": t.into() }], "isError": true })
}

fn as_text(v: &impl serde::Serialize) -> Value {
    text(serde_json::to_string_pretty(v).unwrap_or_default())
}

async fn handle(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, body: axum::body::Bytes) -> Response {
    let who = match require(&app, &headers, &method) {
        Ok(w) => w,
        Err(ApiError(code, msg)) => return (code, Json(json!({ "error": msg }))).into_response(),
    };
    let Ok(msg) = serde_json::from_slice::<Value>(&body) else {
        return Json(rpc_err(&Value::Null, -32700, "Not JSON.")).into_response();
    };
    // Batches are allowed by older clients.
    if let Value::Array(items) = msg {
        let mut out = Vec::new();
        for m in items {
            if let Some(r) = one(&app, &who, &headers, m).await {
                out.push(r);
            }
        }
        return if out.is_empty() { StatusCode::ACCEPTED.into_response() } else { Json(Value::Array(out)).into_response() };
    }
    match one(&app, &who, &headers, msg).await {
        Some(r) => Json(r).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// One JSON-RPC message; `None` for notifications.
async fn one(app: &Arc<App>, who: &Who, headers: &HeaderMap, msg: Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let id = id?;
    Some(match method {
        "initialize" => {
            let name = app.cfg.read().unwrap().name.clone();
            rpc_ok(
                &id,
                json!({
                    "protocolVersion": msg.pointer("/params/protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL),
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "hopperd", "title": format!("Hopper Server · {name}"), "version": env!("CARGO_PKG_VERSION") },
                    "instructions": "Minecraft servers on this Hopper Server panel. Start with list_servers to get server ids.",
                }),
            )
        }
        "ping" => rpc_ok(&id, json!({})),
        "tools/list" => rpc_ok(&id, json!({ "tools": tools_for(who) })),
        "tools/call" => {
            let name = msg.pointer("/params/name").and_then(Value::as_str).unwrap_or("").to_string();
            let args = msg.pointer("/params/arguments").cloned().unwrap_or(json!({}));
            rpc_ok(&id, call(app, who, headers, &name, &args).await)
        }
        _ => rpc_err(&id, -32601, "Unknown method."),
    })
}

/// A query value, percent-encoded.
fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// One API request relayed to a node's server, as the web UI would send
/// it (same sign-in headers, same checks on both sides): its status and
/// JSON answer.
async fn ask_node(app: &Arc<App>, headers: &HeaderMap, m: Method, path_q: &str, body: Option<Value>) -> (StatusCode, Value) {
    let (path, query) = path_q.split_once('?').map_or((path_q, None), |(p, q)| (p, Some(q)));
    let Some(t) = crate::relay::target(path, query) else { return (StatusCode::NOT_FOUND, json!({ "error": "No server with that id." })) };
    let mut req = axum::http::Request::builder().method(m).uri(path_q);
    for (k, v) in headers {
        if k != axum::http::header::CONTENT_LENGTH && k != axum::http::header::CONTENT_TYPE {
            req = req.header(k, v);
        }
    }
    let body = match body {
        Some(b) => {
            req = req.header(axum::http::header::CONTENT_TYPE, "application/json");
            axum::body::Body::from(serde_json::to_vec(&b).unwrap_or_default())
        }
        None => axum::body::Body::empty(),
    };
    let Ok(req) = req.body(body) else { return (StatusCode::BAD_REQUEST, json!({ "error": "That request can't be made." })) };
    let res = crate::relay::forward(app.clone(), t, req).await;
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 16 * 1024 * 1024).await.unwrap_or_default();
    (status, serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({ "text": String::from_utf8_lossy(&bytes) })))
}

/// A tool call on a node's server: the API request it stands for, sent
/// through the node relay. Changes go in this panel's audit log with the
/// node named, as the web UI's do.
async fn node_call(app: &Arc<App>, who: &Who, headers: &HeaderMap, name: &str, sid: &str, args: &Value) -> Value {
    let s = enc(sid);
    let p = enc(arg(args, "path"));
    let (m, path_q, body) = match name {
        "console" => (Method::GET, format!("/api/v1/servers/{s}/console"), None),
        "power" => {
            let action = arg(args, "action");
            if !matches!(action, "start" | "stop" | "restart" | "kill") {
                return tool_error(format!("Unknown action \"{action}\"."));
            }
            (Method::POST, format!("/api/v1/servers/{s}/power"), Some(json!({ "action": action })))
        }
        "send_command" => (Method::POST, format!("/api/v1/servers/{s}/command"), Some(json!({ "line": arg(args, "command").trim().trim_start_matches('/') }))),
        "list_files" => (Method::GET, format!("/api/v1/fs/{s}/list?path={p}"), None),
        "read_file" => (Method::GET, format!("/api/v1/fs/{s}/text?path={p}"), None),
        "write_file" => (Method::PUT, format!("/api/v1/fs/{s}/text"), Some(json!({ "path": arg(args, "path"), "content": arg(args, "content") }))),
        "search_files" => {
            let q = arg(args, "query");
            if q.trim().len() < 2 {
                return tool_error("Search for at least 2 characters.");
            }
            let regex = args.get("regex").and_then(Value::as_bool).unwrap_or(false);
            (Method::GET, format!("/api/v1/fs/{s}/search?q={}&path={p}&regex={}", enc(q), if regex { "1" } else { "0" }), None)
        }
        "list_mods" => (Method::GET, format!("/api/v1/servers/{s}/mods"), None),
        "last_crash" => (Method::GET, format!("/api/v1/servers/{s}/crash"), None),
        "list_backups" => (Method::GET, format!("/api/v1/servers/{s}/backups"), None),
        other => return tool_error(format!("Unknown tool \"{other}\".")),
    };
    let path = path_q.split('?').next().unwrap_or("").to_string();
    if let Err(e) = allowed(app, who, m.clone(), path.clone()) {
        return tool_error(e);
    }
    // The console's length before a command, to show only what it answered.
    let before = if name == "send_command" {
        ask_node(app, headers, Method::GET, &format!("/api/v1/servers/{s}/console?since={}", u64::MAX), None).await.1["total"].as_u64()
    } else {
        None
    };
    let (status, v) = ask_node(app, headers, m.clone(), &path_q, body).await;
    if m != Method::GET {
        let (node, node_name) = crate::security::node_named(app, &path, None);
        let c = crate::security::caller();
        app.audit.record(&crate::security::Event {
            at: crate::security::now(),
            who: who.name(),
            role: who.role_name().into(),
            ip: c.ip,
            proxy: c.proxy,
            net: c.net,
            method: m.to_string(),
            path: format!("{path} (MCP)"),
            status: status.as_u16(),
            node,
            node_name,
        });
    }
    if !status.is_success() {
        return tool_error(v["error"].as_str().map_or_else(|| format!("The node answered {status}."), str::to_string));
    }
    let lines_of = |v: &Value| -> Vec<String> { v["lines"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default() };
    match name {
        "console" => {
            let n = args.get("lines").and_then(Value::as_u64).unwrap_or(100).clamp(1, 1000) as usize;
            let lines = lines_of(&v);
            let start = lines.len().saturating_sub(n);
            text(if lines.is_empty() { "(nothing in the console; the server may be stopped)".into() } else { lines[start..].join("\n") })
        }
        "power" => text(format!("{} {sid}: done.", arg(args, "action"))),
        "send_command" => {
            // Give it a moment to answer.
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            let lines = match before {
                Some(b) => lines_of(&ask_node(app, headers, Method::GET, &format!("/api/v1/servers/{s}/console?since={b}"), None).await.1),
                None => Vec::new(),
            };
            text(if lines.is_empty() { "Sent. (No console output yet.)".to_string() } else { format!("Sent. Console:\n{}", lines.join("\n")) })
        }
        "read_file" => text(v["content"].as_str().unwrap_or_default()),
        "list_files" => as_text(&v["entries"]),
        _ => as_text(&v),
    }
}

fn arg<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Checks a tool call like the API request it stands for.
fn allowed(app: &App, who: &Who, method: Method, path: String) -> Result<(), String> {
    if let Some(msg) = crate::security::refusal(who, &method, &path) {
        let (node, node_name) = crate::security::node_named(app, &path, None);
        let c = crate::security::caller();
        app.audit.record(&crate::security::Event {
            at: crate::security::now(),
            who: who.name(),
            role: who.role_name().into(),
            ip: c.ip,
            proxy: c.proxy,
            net: c.net,
            method: method.to_string(),
            path: format!("{path} (MCP)"),
            status: 403,
            node,
            node_name,
        });
        return Err(msg.to_string());
    }
    Ok(())
}

async fn call(app: &Arc<App>, who: &Who, headers: &HeaderMap, name: &str, args: &Value) -> Value {
    let sid = arg(args, "server").to_string();
    // A node's server: the same tool, through the node relay.
    if name != "list_servers" && crate::relay::split(&sid).is_some() {
        if !who.sees(&sid) {
            return tool_error(format!("No server \"{sid}\". Use list_servers for the ids."));
        }
        return node_call(app, who, headers, name, &sid, args).await;
    }
    // Every tool but list_servers is about one server.
    let entry = if name == "list_servers" {
        None
    } else {
        match app.servers.get(&sid).filter(|_| who.sees(&sid)) {
            Some(e) => Some(e),
            None => return tool_error(format!("No server \"{sid}\". Use list_servers for the ids.")),
        }
    };
    let (m, route) = match name {
        "power" => (Method::POST, "power"),
        "send_command" => (Method::POST, "command"),
        "write_file" => (Method::PUT, "fs-text"),
        _ => (Method::GET, "read"),
    };
    let path = if route == "fs-text" { format!("/api/v1/fs/{sid}/text") } else { format!("/api/v1/servers/{sid}/{route}") };
    if let Err(e) = allowed(app, who, m, path) {
        return tool_error(e);
    }
    let a = app.clone();
    let who = who.clone();
    let who_name = who.name();
    let name = name.to_string();
    let args = args.clone();
    let r = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let app = a;
        Ok(match name.as_str() {
            "list_servers" => {
                let snap = app.snapshot.borrow().clone();
                let mut mine: Vec<Value> = snap.servers.iter().filter(|s| who.sees(&s.id)).map(|s| json!(s)).collect();
                // A node's servers too, their node named (ids are `<node>:<id>`).
                mine.extend(app.nodes.servers_for(&who));
                as_text(&mine)
            }
            "console" => {
                let e = entry.unwrap();
                let n = args.get("lines").and_then(Value::as_u64).unwrap_or(100).clamp(1, 1000) as usize;
                let (lines, _) = e.server.console_since(0);
                let start = lines.len().saturating_sub(n);
                text(if lines.is_empty() { "(nothing in the console; the server may be stopped)".into() } else { lines[start..].join("\n") })
            }
            "power" => {
                let e = entry.unwrap();
                let action = match arg(&args, "action") {
                    "start" => PanelAction::Start,
                    "stop" => PanelAction::Stop,
                    "restart" => PanelAction::Restart,
                    "kill" => PanelAction::Kill,
                    other => return Ok(tool_error(format!("Unknown action \"{other}\"."))),
                };
                log::info!("{} {} by {who_name} (MCP)", arg(&args, "action"), e.name);
                app.servers.act(&e, action)?;
                app.refresh.notify_one();
                text(format!("{} {}: done.", arg(&args, "action"), e.name))
            }
            "send_command" => {
                let e = entry.unwrap();
                let line = arg(&args, "command").trim().trim_start_matches('/');
                let (_, before) = e.server.console_since(u64::MAX);
                app.servers.command(&e, line)?;
                // Give it a moment to answer.
                std::thread::sleep(std::time::Duration::from_millis(600));
                let (lines, _) = e.server.console_since(before);
                text(if lines.is_empty() { "Sent. (No console output yet.)".to_string() } else { format!("Sent. Console:\n{}", lines.join("\n")) })
            }
            "list_files" => as_text(&files::list(&entry.unwrap().root, arg(&args, "path"))?),
            "read_file" => {
                let (root, rel) = (&entry.unwrap().root, arg(&args, "path"));
                if let Some(msg) = crate::secrets::link_refusal(&who, root, &[rel.to_string()], None, false) {
                    return Ok(tool_error(msg));
                }
                text(files::read_text(root, rel)?.content)
            }
            "write_file" => {
                let r = files::write_text(&entry.unwrap().root, arg(&args, "path"), arg(&args, "content"), None, false)?;
                as_text(&r)
            }
            "search_files" => {
                let q = arg(&args, "query");
                if q.trim().len() < 2 {
                    return Ok(tool_error("Search for at least 2 characters."));
                }
                let regex = args.get("regex").and_then(Value::as_bool).unwrap_or(false);
                let (mut hits, more) = files::search(&entry.unwrap().root, arg(&args, "path"), q, regex, 300)?;
                hits.retain(|h| crate::secrets::shows(&who, &h.path));
                as_text(&json!({ "hits": hits, "more": more }))
            }
            "list_mods" => {
                let dir = entry.unwrap().root.join("mods");
                let mods = if dir.is_dir() { hopper_core::scan::scan_folder(&dir)? } else { Vec::new() };
                let short: Vec<Value> = mods
                    .iter()
                    .map(|m| json!({ "name": m.mod_name, "id": m.mod_id, "version": m.version, "file": m.file_name, "side": m.side, "enabled": !m.file_name.ends_with(".disabled") }))
                    .collect();
                as_text(&short)
            }
            "last_crash" => match hopper_core::crashdetect::server(&entry.unwrap().root) {
                Some(c) => as_text(&c),
                None => text("No crash found."),
            },
            "list_backups" => as_text(&hopper_core::hosting::backup::list(&entry.unwrap().root)),
            other => tool_error(format!("Unknown tool \"{other}\".")),
        })
    })
    .await;
    match r {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => tool_error(format!("{e:#}")),
        Err(e) => tool_error(e.to_string()),
    }
}
