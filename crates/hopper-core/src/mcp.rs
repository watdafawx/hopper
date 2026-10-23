//! A Model Context Protocol server inside Hopper, for AI assistants: read
//! instances, mods, logs and crash reports, search CurseForge and
//! Modrinth, and (when allowed) install, switch and update mods or see and
//! drive the window.
//!
//! Off unless switched on in Settings (or Hopper starts with `--mcp`).
//! Listens on 127.0.0.1 only, and turns away requests from web pages (an
//! Origin header that isn't localhost), so a site can't reach it through
//! the browser. The protocol lives here; the UI answers the commands.
//! Speaks MCP's streamable HTTP transport with plain JSON responses: POST
//! JSON-RPC to `http://127.0.0.1:47801/mcp`.

use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

/// Which groups of tools are offered, set from Settings while running.
static CHANGES: AtomicBool = AtomicBool::new(false);
static WINDOW: AtomicBool = AtomicBool::new(false);

/// Allows (or not) the tools that change instances and the ones that see
/// and drive the window.
pub fn set_access(changes: bool, window: bool) {
    CHANGES.store(changes, Ordering::Relaxed);
    WINDOW.store(window, Ordering::Relaxed);
}

/// Read-only data tools, answered from Hopper's library and the sites.
const DATA_TOOLS: &[&str] = &[
    "list_instances",
    "list_mods",
    "read_log",
    "list_crash_reports",
    "missing_dependencies",
    "search_projects",
    "project_info",
];
/// Data tools that change an instance.
const CHANGE_TOOLS: &[&str] = &["install_project", "set_mods_enabled", "update_mods"];

pub const DEFAULT_PORT: u16 = 47801;
const PROTOCOL: &str = "2025-06-18";

/// A tool call the UI thread has to answer.
pub enum Command {
    Screenshot,
    State,
    Navigate(Value),
    /// Screenshot-pixel coordinates.
    Click {
        x: f32,
        y: f32,
        double: bool,
        /// "left" (the default), "right", "middle", "back" or "forward".
        button: String,
    },
    /// Move the pointer there (hover cards, tooltips).
    Hover {
        x: f32,
        y: f32,
    },
    Drag {
        from: (f32, f32),
        to: (f32, f32),
    },
    Scroll {
        x: f32,
        y: f32,
        dy: f32,
    },
    Type(String),
    Key(String),
    /// Holds (or lets go of) modifier keys, as for a Shift+click.
    Modifiers { shift: bool, ctrl: bool, alt: bool },
    /// A data tool, by name, with its arguments.
    Data(String, Value),
}

pub type Reply = Result<Vec<Value>, String>;
pub type Request = (Command, Sender<Reply>);

pub fn text(s: impl Into<String>) -> Value {
    json!({ "type": "text", "text": s.into() })
}

pub fn image_png(base64: String) -> Value {
    json!({ "type": "image", "data": base64, "mimeType": "image/png" })
}

fn tools() -> Value {
    let mut out: Vec<Value> = data_tools().as_array().cloned().unwrap_or_default();
    if CHANGES.load(Ordering::Relaxed) {
        out.extend(change_tools().as_array().cloned().unwrap_or_default());
    }
    if WINDOW.load(Ordering::Relaxed) {
        out.extend(window_tools().as_array().cloned().unwrap_or_default());
    }
    Value::Array(out)
}

fn instance_arg() -> Value {
    json!({ "type": "string", "description": "The instance's name, or part of it (see list_instances)" })
}

fn data_tools() -> Value {
    json!([
        {
            "name": "list_instances",
            "description": "Every Minecraft instance in Hopper: name, Minecraft version, loader, modpack and version, folder, mod count, updates waiting.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "list_mods",
            "description": "An instance's mods (or resource packs or shaders): name, file, version, on or off, side, project id and site, author, whether an update waits and to which file.",
            "inputSchema": { "type": "object", "required": ["instance"], "properties": {
                "instance": instance_arg(),
                "kind": { "type": "string", "enum": ["mods", "resourcepacks", "shaders"], "description": "Default mods" },
                "filter": { "type": "string", "description": "Only those whose name or file contains this" },
                "only_updates": { "type": "boolean" },
                "only_disabled": { "type": "boolean" }
            } }
        },
        {
            "name": "read_log",
            "description": "The end of an instance's game log: latest.log (default), debug.log, or the newest crash report.",
            "inputSchema": { "type": "object", "required": ["instance"], "properties": {
                "instance": instance_arg(),
                "file": { "type": "string", "enum": ["latest", "debug", "crash"] },
                "lines": { "type": "integer", "description": "How many lines from the end (default 300, at most 3000)" },
                "grep": { "type": "string", "description": "Only lines containing this (case-insensitive)" }
            } }
        },
        {
            "name": "list_crash_reports",
            "description": "An instance's crash reports, newest first, with their dates; read one with read_log file=crash or by name with read_log.",
            "inputSchema": { "type": "object", "required": ["instance"], "properties": { "instance": instance_arg() } }
        },
        {
            "name": "missing_dependencies",
            "description": "Required mods an instance lacks or has in a version its mods don't accept, read from each jar's metadata (what the loader checks at startup).",
            "inputSchema": { "type": "object", "required": ["instance"], "properties": { "instance": instance_arg() } }
        },
        {
            "name": "search_projects",
            "description": "Search CurseForge or Modrinth for mods, modpacks, resource packs, shaders or data packs, for a Minecraft version and loader.",
            "inputSchema": { "type": "object", "required": ["query"], "properties": {
                "query": { "type": "string" },
                "site": { "type": "string", "enum": ["curseforge", "modrinth"], "description": "Default curseforge" },
                "kind": { "type": "string", "enum": ["mods", "modpacks", "resourcepacks", "shaders", "datapacks"] },
                "minecraft": { "type": "string", "description": "e.g. 1.21.1; defaults to the instance's when instance is given" },
                "loader": { "type": "string", "enum": ["neoforge", "forge", "fabric", "quilt"] },
                "instance": instance_arg()
            } }
        },
        {
            "name": "project_info",
            "description": "A project's details by id (from search_projects or list_mods): summary, authors, downloads, categories, links, and its files for a Minecraft version and loader.",
            "inputSchema": { "type": "object", "required": ["id"], "properties": {
                "id": { "type": "integer" },
                "instance": instance_arg()
            } }
        }
    ])
}

fn change_tools() -> Value {
    json!([
        {
            "name": "install_project",
            "description": "Install a project (by id from search_projects) into an instance, with its required dependencies, as Browse's Install does. Runs as a task in Hopper.",
            "inputSchema": { "type": "object", "required": ["instance", "id"], "properties": {
                "instance": instance_arg(),
                "id": { "type": "integer" }
            } }
        },
        {
            "name": "set_mods_enabled",
            "description": "Switch mods on or off by file name (from list_mods).",
            "inputSchema": { "type": "object", "required": ["instance", "files", "enabled"], "properties": {
                "instance": instance_arg(),
                "files": { "type": "array", "items": { "type": "string" } },
                "enabled": { "type": "boolean" }
            } }
        },
        {
            "name": "update_mods",
            "description": "Update an instance's mods that have updates waiting: the given project ids, or all of them. Runs as a task in Hopper.",
            "inputSchema": { "type": "object", "required": ["instance"], "properties": {
                "instance": instance_arg(),
                "ids": { "type": "array", "items": { "type": "integer" }, "description": "Leave out for every update" }
            } }
        }
    ])
}

fn window_tools() -> Value {
    json!([
        {
            "name": "screenshot",
            "description": "PNG of the Hopper window. Click coordinates are pixels in this image. Popups are separate OS windows unless Hopper runs with --embed-windows, which draws them inside this one.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "state",
            "description": "Hopper's state as JSON: tab, instances, selected instance, servers and status, sync diff summary, browse results, running tasks.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "navigate",
            "description": "Jump to a view. All fields optional.",
            "inputSchema": { "type": "object", "properties": {
                "tab": { "type": "string", "enum": ["library", "browse", "servers", "sync"] },
                "instance": { "type": "string", "description": "Part of an instance name" },
                "server": { "type": "string", "description": "Part of a server folder name; on the sync tab this starts the comparison" },
                "view": { "type": "string", "enum": ["mods", "resourcepacks", "shaders", "datapacks", "worlds", "screenshots", "settings", "logs"] },
                "search": { "type": "string", "description": "Browse search text" },
                "class": { "type": "string", "enum": ["mods", "modpacks", "resourcepacks", "shaders"] }
            } }
        },
        {
            "name": "click",
            "description": "Click at screenshot pixel coordinates; button is left (default), right, middle, back or forward.",
            "inputSchema": { "type": "object", "required": ["x", "y"], "properties": {
                "x": { "type": "number" }, "y": { "type": "number" }, "double": { "type": "boolean" },
                "button": { "type": "string" }
            } }
        },
        {
            "name": "drag",
            "description": "Press the left button at (x0, y0), move to (x1, y1) in steps and let go, in screenshot pixel coordinates. For resize handles and drag and drop.",
            "inputSchema": { "type": "object", "required": ["x0", "y0", "x1", "y1"], "properties": {
                "x0": { "type": "number" }, "y0": { "type": "number" },
                "x1": { "type": "number" }, "y1": { "type": "number" }
            } }
        },
        {
            "name": "hover",
            "description": "Move the pointer to screenshot pixel coordinates, for hover cards and tooltips. Wait a moment, then take a screenshot.",
            "inputSchema": { "type": "object", "required": ["x", "y"], "properties": {
                "x": { "type": "number" }, "y": { "type": "number" }
            } }
        },
        {
            "name": "scroll",
            "description": "Scroll at screenshot pixel coordinates. Positive dy scrolls down.",
            "inputSchema": { "type": "object", "required": ["x", "y", "dy"], "properties": {
                "x": { "type": "number" }, "y": { "type": "number" }, "dy": { "type": "number" }
            } }
        },
        {
            "name": "type",
            "description": "Type text into the focused field.",
            "inputSchema": { "type": "object", "required": ["text"], "properties": { "text": { "type": "string" } } }
        },
        {
            "name": "key",
            "description": "Press a key: Enter, Escape, Tab, Backspace, ArrowUp, ArrowDown, ArrowLeft, ArrowRight, PageUp, PageDown, Home, End, Delete.",
            "inputSchema": { "type": "object", "required": ["name"], "properties": { "name": { "type": "string" } } }
        },
        {
            "name": "modifiers",
            "description": "Hold modifier keys until the next call (all false lets go): Shift, Ctrl, Alt.",
            "inputSchema": { "type": "object", "properties": { "shift": { "type": "boolean" }, "ctrl": { "type": "boolean" }, "alt": { "type": "boolean" } } }
        }
    ])
}

fn command_for(name: &str, args: &Value) -> Result<Command, String> {
    let num = |k: &str| {
        args[k]
            .as_f64()
            .map(|v| v as f32)
            .ok_or(format!("missing number '{k}'"))
    };
    if DATA_TOOLS.contains(&name) {
        return Ok(Command::Data(name.to_string(), args.clone()));
    }
    if CHANGE_TOOLS.contains(&name) {
        if !CHANGES.load(Ordering::Relaxed) {
            return Err("Changing instances is off: switch on \"Let assistants change instances\" in Hopper's Settings > Advanced.".into());
        }
        return Ok(Command::Data(name.to_string(), args.clone()));
    }
    if !WINDOW.load(Ordering::Relaxed) {
        return Err(format!(
            "{name} needs window control: switch on \"Let assistants see and use the window\" in Hopper's Settings > Advanced."
        ));
    }
    Ok(match name {
        "screenshot" => Command::Screenshot,
        "state" => Command::State,
        "navigate" => Command::Navigate(args.clone()),
        "click" => Command::Click {
            x: num("x")?,
            y: num("y")?,
            double: args["double"].as_bool().unwrap_or(false),
            button: args["button"].as_str().unwrap_or("left").to_string(),
        },
        "drag" => Command::Drag {
            from: (num("x0")?, num("y0")?),
            to: (num("x1")?, num("y1")?),
        },
        "hover" => Command::Hover {
            x: num("x")?,
            y: num("y")?,
        },
        "scroll" => Command::Scroll {
            x: num("x")?,
            y: num("y")?,
            dy: num("dy")?,
        },
        "type" => Command::Type(args["text"].as_str().ok_or("missing 'text'")?.to_string()),
        "key" => Command::Key(args["name"].as_str().ok_or("missing 'name'")?.to_string()),
        "modifiers" => Command::Modifiers {
            shift: args["shift"].as_bool().unwrap_or(false),
            ctrl: args["ctrl"].as_bool().unwrap_or(false),
            alt: args["alt"].as_bool().unwrap_or(false),
        },
        other => return Err(format!("unknown tool {other}")),
    })
}

/// Starts the server thread. Requests come out of the returned receiver; the
/// UI answers each through its reply sender.
/// `wake` asks the UI to look at the receiver soon.
/// A running server; dropping it doesn't stop it, [`Server::stop`] does.
pub struct Server(std::sync::Arc<tiny_http::Server>);

impl Server {
    pub fn stop(&self) {
        self.0.unblock();
    }
}

/// Whether a request comes from a web page rather than an assistant: a
/// browser sends Origin, and only a page on this PC may talk to Hopper.
fn from_web_page(req: &tiny_http::Request) -> bool {
    req.headers().iter().any(|h| {
        h.field.equiv("Origin") && {
            let o = h.value.as_str().to_ascii_lowercase();
            !(o.starts_with("http://localhost") || o.starts_with("http://127.0.0.1") || o == "null")
        }
    })
}

pub fn start(
    port: u16,
    wake: impl Fn() + Send + 'static,
) -> anyhow::Result<(Receiver<Request>, Server)> {
    let server = std::sync::Arc::new(
        tiny_http::Server::http(("127.0.0.1", port)).map_err(|e| anyhow::anyhow!("port {port}: {e}"))?,
    );
    let (tx, rx) = channel::<Request>();
    let s2 = server.clone();
    std::thread::Builder::new()
        .name("mcp".into())
        .spawn(move || {
            for mut req in s2.incoming_requests() {
                if from_web_page(&req) {
                    let _ = req.respond(tiny_http::Response::from_string("").with_status_code(403));
                    continue;
                }
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                let (status, out) = handle(&body, &tx, &wake);
                let mut resp = tiny_http::Response::from_string(out).with_status_code(status);
                resp.add_header(
                    tiny_http::Header::from_bytes("Content-Type", "application/json")
                        .expect("static header"),
                );
                let _ = req.respond(resp);
            }
        })?;
    log::info!("MCP server on http://127.0.0.1:{port}/mcp");
    Ok((rx, Server(server)))
}

fn handle(body: &str, tx: &Sender<Request>, wake: &dyn Fn()) -> (u16, String) {
    let Ok(msg) = serde_json::from_str::<Value>(body) else {
        return (400, json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "parse error" } }).to_string());
    };
    let id = msg.get("id").cloned();
    let method = msg["method"].as_str().unwrap_or("");
    // Notifications get no body.
    let Some(id) = id else {
        return (202, String::new());
    };
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "hopper", "version": env!("CARGO_PKG_VERSION") },
            "instructions": "Hopper is a Minecraft launcher and mod manager. Start with list_instances, then list_mods, read_log or missing_dependencies for an instance; search_projects and project_info look things up on CurseForge and Modrinth. Tools that change things or drive the window appear only when the user allows them in Hopper's Settings."
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            let args = msg["params"]["arguments"].clone();
            Ok(match command_for(name, &args) {
                Err(e) => json!({ "content": [text(e)], "isError": true }),
                Ok(cmd) => {
                    let (rtx, rrx) = channel();
                    let _ = tx.send((cmd, rtx));
                    wake();
                    match rrx.recv_timeout(Duration::from_secs(60)) {
                        Ok(Ok(content)) => json!({ "content": content }),
                        Ok(Err(e)) => json!({ "content": [text(e)], "isError": true }),
                        Err(_) => {
                            json!({ "content": [text("Hopper did not answer in 60s")], "isError": true })
                        }
                    }
                }
            })
        }
        _ => Err(json!({ "code": -32601, "message": format!("method not found: {method}") })),
    };
    let out = match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
    };
    (200, out.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_and_list_without_ui() {
        let (tx, _rx) = channel();
        let ctx = || {};
        let (s, out) = handle(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
            &tx,
            &ctx,
        );
        assert_eq!(s, 200);
        assert!(out.contains("\"name\":\"hopper\""));
        let (_, out) = handle(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            &tx,
            &ctx,
        );
        assert!(out.contains("list_instances") && out.contains("read_log"));
        assert!(!out.contains("install_project") && !out.contains("screenshot"));
        set_access(true, true);
        let (_, out) = handle(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#, &tx, &ctx);
        assert!(out.contains("install_project") && out.contains("screenshot"));
        // Window tools need coordinates (in this test: the switches are
        // shared, so tests that flip them can't run side by side).
        assert!(matches!(
            command_for("hover", &json!({ "x": 1, "y": 2 })),
            Ok(Command::Hover { .. })
        ));
        assert!(command_for("hover", &json!({})).is_err());
        set_access(false, false);
        assert!(command_for("hover", &json!({ "x": 1, "y": 2 })).is_err());
        assert!(command_for("install_project", &json!({})).is_err());
        let (s, _) = handle(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            &tx,
            &ctx,
        );
        assert_eq!(s, 202);
    }

}
