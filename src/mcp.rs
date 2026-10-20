//! A Model Context Protocol server inside Hopper, so an agent can see and
//! drive this window (and only this window) without desktop control.
//!
//! Off unless Hopper starts with `--mcp` (or `HOPPER_MCP=1`). Listens on
//! 127.0.0.1 only. The protocol lives here; each UI answers the commands. Speaks MCP's streamable HTTP transport with plain JSON
//! responses: POST JSON-RPC to `http://127.0.0.1:47801/mcp`.

use serde_json::{Value, json};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

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
pub fn start(
    port: u16,
    wake: impl Fn() + Send + 'static,
) -> anyhow::Result<Receiver<Request>> {
    let server = tiny_http::Server::http(("127.0.0.1", port))
        .map_err(|e| anyhow::anyhow!("MCP port {port}: {e}"))?;
    let (tx, rx) = channel::<Request>();
    std::thread::Builder::new()
        .name("mcp".into())
        .spawn(move || {
            for mut req in server.incoming_requests() {
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
    Ok(rx)
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
            "instructions": "Drives the Hopper window. Take a screenshot first; click coordinates are pixels in the latest screenshot."
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
                    match rrx.recv_timeout(Duration::from_secs(20)) {
                        Ok(Ok(content)) => json!({ "content": content }),
                        Ok(Err(e)) => json!({ "content": [text(e)], "isError": true }),
                        Err(_) => {
                            json!({ "content": [text("Hopper did not answer in 20s")], "isError": true })
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
        assert!(out.contains("screenshot") && out.contains("navigate"));
        let (s, _) = handle(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            &tx,
            &ctx,
        );
        assert_eq!(s, 202);
    }

    #[test]
    fn hover_needs_coordinates() {
        assert!(matches!(
            command_for("hover", &json!({ "x": 1, "y": 2 })),
            Ok(Command::Hover { .. })
        ));
        assert!(command_for("hover", &json!({})).is_err());
    }
}
