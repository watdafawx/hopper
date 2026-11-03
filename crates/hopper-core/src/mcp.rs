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
    "list_packs",
    "download_status",
    "unknown_mods",
    "look_up_source",
    "crash_analysis",
    "list_servers",
    "server_status",
    "server_sync",
    "server_motd",
    "read_settings",
];
/// Data tools that change an instance (or run a server).
const CHANGE_TOOLS: &[&str] = &[
    "install_project",
    "set_mods_enabled",
    "update_mods",
    "add_from_source",
    "follow_source",
    "unfollow_source",
    "server_action",
];

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
        /// Mouse-wheel notches instead of pixels (smooth scrolling animates
        /// these; pixel scrolls stay direct, like a touchpad).
        notches: bool,
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
            "description": "An instance's mods (or resource packs or shaders) as Hopper tracks them: name, file, version, on or off, side, project id and site (curseforge, modrinth, source or none), author, whether the modpack or the player added it, the update waiting and to which file, and for a mod that follows a source (GitHub, Maven, Jenkins, a link) where from, its pinned release and sha256 and whether that source is trusted on this PC. For data packs and pack folders (Paxi, Global Packs, worlds) use list_packs.",
            "inputSchema": { "type": "object", "required": ["instance"], "properties": {
                "instance": instance_arg(),
                "kind": { "type": "string", "enum": ["mods", "resourcepacks", "shaders"], "description": "Default mods" },
                "filter": { "type": "string", "description": "Only those whose name or file contains this" },
                "only_updates": { "type": "boolean" },
                "only_disabled": { "type": "boolean" },
                "only_added": { "type": "boolean", "description": "Only those the player added, not the modpack (the Mods tab's \"Added by you\")" },
                "only_sources": { "type": "boolean", "description": "Only mods that follow a GitHub, Maven, Jenkins or link source" }
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
        },
        {
            "name": "list_packs",
            "description": "An instance's resource packs, shaders or data packs in every place they load from (this instance, Paxi, Paxi optional, Global Packs, Open Loader, each world's datapacks), each with its file, Enabled (not switched off) and Active (in the game's resource pack list, or the shader Iris/Oculus uses), and the project and update Hopper knows for it.",
            "inputSchema": { "type": "object", "required": ["instance", "kind"], "properties": {
                "instance": instance_arg(),
                "kind": { "type": "string", "enum": ["resourcepacks", "shaders", "datapacks"] }
            } }
        },
        {
            "name": "download_status",
            "description": "Files Hopper gets from the CurseForge website because their authors allow no other download (website-only files in installs and updates): each file's step (waiting, opening, downloading, done, didn't come) and its page to open by hand.",
            "inputSchema": { "type": "object", "properties": { "instance": instance_arg() } }
        },
        {
            "name": "unknown_mods",
            "description": "An instance's jars that CurseForge and Modrinth don't know and that follow no source, each with the GitHub repo its own metadata links to (if any): candidates for follow_source.",
            "inputSchema": { "type": "object", "required": ["instance"], "properties": { "instance": instance_arg() } }
        },
        {
            "name": "look_up_source",
            "description": "Looks a GitHub, Maven, Jenkins or download link up for an instance without changing anything. With file (an installed jar): how that jar matches the source (exact: byte for byte a release's asset; baseline: the release at or before the jar's version; unknown) and the newer release that would show as its update. Without file: the files the newest release offers and which one Hopper would pick. Says whether the source is trusted on this PC.",
            "inputSchema": { "type": "object", "required": ["instance", "link"], "properties": {
                "instance": instance_arg(),
                "link": { "type": "string", "description": "A whole https:// link: a GitHub repo, release or asset, a Maven folder, a Jenkins job, or a .jar download" },
                "file": { "type": "string", "description": "An installed jar's file name (from list_mods or unknown_mods)" },
                "prerelease": { "type": "boolean", "description": "Offer pre-releases too" }
            } }
        },
        {
            "name": "crash_analysis",
            "description": "Hopper's reading of the last crash or join failure, as its crash and join cards show it, not the raw report. For an instance: the crash (headline, kind, suspects with jar, mod id, certainty and why, what they need, the mod that stopped loading, config switches, advice) and the last join problem (server or world, what happened, what to do, the mods named). For a server: its last start's crash. read_log has the raw report.",
            "inputSchema": { "type": "object", "properties": {
                "instance": instance_arg(),
                "server": { "type": "string", "description": "A server's name or part of it (see list_servers), instead of instance" },
                "report": { "type": "string", "description": "Read this crash report (file name from list_crash_reports) with today's rules instead of the saved finding" }
            } }
        },
        {
            "name": "list_servers",
            "description": "The Minecraft servers Hopper knows (rig folders, Hopper Server, hosted on this PC): name, folder, panel, loader, port, Java memory, the instance (pack) it's paired with, and the last ping (online, players, version).",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "server_status",
            "description": "A server's live state from its panel: running, starting, stopping or crashed, players, CPU and memory, TPS, addresses, and a start, stop or restart under way (percent, step, time left) or just finished.",
            "inputSchema": { "type": "object", "required": ["server"], "properties": {
                "server": { "type": "string", "description": "A server's name or part of it" }
            } }
        },
        {
            "name": "server_sync",
            "description": "Compares a server's mods with its paired pack, as the Sync tab does: counts per state, and each mod that differs (not on server, not in pack, old on server, update, downgrade) with both versions and what syncing would do. 'updates' lists the server's mods the pack has newer. Reads only.",
            "inputSchema": { "type": "object", "required": ["server"], "properties": {
                "server": { "type": "string", "description": "A server's name or part of it" },
                "include_same": { "type": "boolean", "description": "Also list mods in sync" }
            } }
        },
        {
            "name": "server_motd",
            "description": "A server's list message (MOTD): the template as the player edits it (with {pack_version}, {players} and the like), the message it fills in to now, what server.properties holds, and every variable's value.",
            "inputSchema": { "type": "object", "required": ["server"], "properties": {
                "server": { "type": "string", "description": "A server's name or part of it" },
                "template": { "type": "string", "description": "Fill in this template instead of the saved one (a preview; nothing is saved)" }
            } }
        },
        {
            "name": "read_settings",
            "description": "Hopper's settings as JSON (certificates hidden; keys and passwords live in Windows' credential store, not here). Pass keys to get only some, e.g. [\"allow_sources\", \"mcp_changes\"].",
            "inputSchema": { "type": "object", "properties": {
                "keys": { "type": "array", "items": { "type": "string" } }
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
        },
        {
            "name": "add_from_source",
            "description": "Adds a mod from a GitHub release, Maven repository, Jenkins job or download link to an instance, as Add from GitHub or a link does; its updates then come from there. Needs Settings > Mod sources on, and the source already trusted on this PC: a new source needs the player to confirm it once in Hopper. Runs as a task.",
            "inputSchema": { "type": "object", "required": ["instance", "link"], "properties": {
                "instance": instance_arg(),
                "link": { "type": "string" },
                "asset": { "type": "string", "description": "The file to take when the release has several (from look_up_source); default Hopper's pick. For a branch folder that holds several packs, the folder's name" },
                "prerelease": { "type": "boolean" },
                "kind": { "type": "string", "enum": ["resourcepacks", "shaders"], "description": "Add a pack instead of a mod: a folder on a GitHub branch (github.com/owner/repo/tree/branch[/folder]), a release's .zip or a link to a .zip" },
                "location": { "type": "string", "description": "Packs: where it goes, a location key or name from list_packs (default this instance)" }
            } }
        },
        {
            "name": "follow_source",
            "description": "Makes an installed jar follow a GitHub, Maven, Jenkins or link source, as Follow a source does: the jar stays as it is (nothing downloads or is replaced); it's matched to a release by its sha256, else by its version, and later releases show as its updates. Needs Settings > Mod sources on and the source already trusted on this PC (else the player confirms it in Hopper).",
            "inputSchema": { "type": "object", "required": ["instance", "file", "link"], "properties": {
                "instance": instance_arg(),
                "file": { "type": "string", "description": "The jar's file name" },
                "link": { "type": "string" },
                "prerelease": { "type": "boolean" }
            } }
        },
        {
            "name": "unfollow_source",
            "description": "Stops a mod following its source: it goes back to the CurseForge or Modrinth project it was, or to a plain jar. The file stays.",
            "inputSchema": { "type": "object", "required": ["instance", "file"], "properties": {
                "instance": instance_arg(),
                "file": { "type": "string" }
            } }
        },
        {
            "name": "server_action",
            "description": "Starts, stops or restarts a server through its panel. Players on it are cut off, so it needs confirm: true, given only after the player said yes to this exact action. Follow it with server_status.",
            "inputSchema": { "type": "object", "required": ["server", "action", "confirm"], "properties": {
                "server": { "type": "string", "description": "A server's name or part of it" },
                "action": { "type": "string", "enum": ["start", "stop", "restart"] },
                "confirm": { "type": "boolean", "description": "Must be true" }
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
            "description": "Scroll at screenshot pixel coordinates. Positive dy scrolls down. With notches true, dy counts mouse-wheel notches instead of pixels.",
            "inputSchema": { "type": "object", "required": ["x", "y", "dy"], "properties": {
                "x": { "type": "number" }, "y": { "type": "number" }, "dy": { "type": "number" },
                "notches": { "type": "boolean" }
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
        if name == "server_action" {
            server_action_of(args)?;
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
            notches: args["notches"].as_bool().unwrap_or(false),
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

/// The panel action a `server_action` call asks for, once it carries
/// `confirm: true`. Kill isn't offered: it loses the world since the last
/// save, so it stays a click in Hopper.
pub fn server_action_of(args: &Value) -> Result<crate::panel::PanelAction, String> {
    use crate::panel::PanelAction;
    let action = match args["action"].as_str().unwrap_or("") {
        "start" => PanelAction::Start,
        "stop" => PanelAction::Stop,
        "restart" => PanelAction::Restart,
        other => return Err(format!("action must be start, stop or restart, not \"{other}\"")),
    };
    if args["confirm"].as_bool() != Some(true) {
        return Err(format!(
            "{} cuts off anyone playing: ask the player first, then call again with confirm: true.",
            action.label()
        ));
    }
    Ok(action)
}

/// The trust rule for sources added over MCP: Mod sources must be on, and
/// an assistant can't trust a new source by itself; the player confirms
/// each one once in Hopper.
pub fn source_gate(allowed: bool, trusted: bool, publisher: &str) -> Result<(), String> {
    if !allowed {
        return Err("Mods from GitHub and other links are off: the player turns them on in Hopper's Settings > Mod sources.".into());
    }
    if !trusted {
        return Err(format!(
            "Needs user confirmation in Hopper: {publisher} isn't a trusted source on this PC yet. Ask the player to add or follow it once in Hopper (Add from GitHub or a link, or Follow a source) and confirm it; after that this works."
        ));
    }
    Ok(())
}

/// Settings as an assistant may read them: certificates (long PEM text)
/// hidden. Keys, tokens and passwords aren't in settings at all.
pub fn redact_settings(mut v: Value) -> Value {
    fn walk(v: &mut Value) {
        match v {
            Value::Object(m) => {
                for (k, x) in m.iter_mut() {
                    let k = k.to_ascii_lowercase();
                    let hide = k == "ca" || k.ends_with("_ca") || k.contains("token") || k.contains("password") || k.contains("secret");
                    if hide && x.as_str().is_some_and(|s| !s.is_empty()) {
                        *x = Value::String("(hidden)".into());
                    } else {
                        walk(x);
                    }
                }
            }
            Value::Array(a) => a.iter_mut().for_each(walk),
            _ => {}
        }
    }
    walk(&mut v);
    v
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
            "instructions": "Hopper is a Minecraft launcher and mod manager. Start with list_instances, then list_mods, list_packs, read_log, crash_analysis or missing_dependencies for an instance; search_projects and project_info look things up on CurseForge and Modrinth; look_up_source and unknown_mods cover mods from GitHub and other links; list_servers, server_status and server_sync cover servers. Tools that change things or drive the window appear only when the user allows them in Hopper's Settings."
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

    #[test]
    fn server_actions_need_confirm() {
        use crate::panel::PanelAction;
        assert!(server_action_of(&json!({ "action": "stop" })).is_err());
        assert!(server_action_of(&json!({ "action": "stop", "confirm": false })).is_err());
        assert!(server_action_of(&json!({ "action": "kill", "confirm": true })).is_err());
        assert_eq!(server_action_of(&json!({ "action": "restart", "confirm": true })), Ok(PanelAction::Restart));
    }

    #[test]
    fn sources_need_the_setting_and_trust() {
        assert!(source_gate(false, true, "a/b").is_err());
        let e = source_gate(true, false, "a/b").unwrap_err();
        assert!(e.contains("Needs user confirmation in Hopper"));
        assert!(source_gate(true, true, "a/b").is_ok());
    }

    #[test]
    fn settings_hide_certificates() {
        let v = redact_settings(json!({
            "panel_ca": "-----BEGIN", "hopper_panels": [{ "ca": "PEM", "name": "rig" }], "zoom": 1.0, "ca": ""
        }));
        assert_eq!(v["panel_ca"], "(hidden)");
        assert_eq!(v["hopper_panels"][0]["ca"], "(hidden)");
        assert_eq!(v["hopper_panels"][0]["name"], "rig");
        assert_eq!(v["ca"], "");
    }
}
