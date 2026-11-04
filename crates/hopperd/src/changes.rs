//! Change events: every client (web panel tabs, Hopper desktop on any PC)
//! hears when something about a server changed, so its open page updates
//! without a refresh.
//!
//! An event only says *what* changed (`{rev, server, areas, by}`), never the
//! new data: the client fetches what it shows again. That keeps events tiny
//! and permission checks where they already are (the normal GET routes).
//!
//! Where events come from:
//! - every successful change request through the API ([`from_request`],
//!   called by `security::guard`), so new routes are covered without
//!   touching their handlers;
//! - a file watcher on each server's folder ([`watch`]), for changes made
//!   outside the panel: SFTP, a job finishing, the server itself rewriting
//!   `whitelist.json`, someone editing files on the machine.
//!
//! How they reach clients:
//! - the live WebSocket (`/api/v1/ws`) sends `{"t":"changed", …}`;
//! - `GET /api/v1/changes?boot=…&since=…&wait=…` long-polls, for clients
//!   without a socket (Hopper desktop). A client whose `boot` or `since` is
//!   too old gets `reset: true` and reloads what it shows.

use crate::App;
use crate::api::{ApiResult, require};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, Method};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;

/// Events kept for clients catching up. A client further behind reloads.
const KEEP: usize = 512;
/// The longest a long-poll waits for an event.
const MAX_WAIT: u64 = 25;

/// One change.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Change {
    pub rev: u64,
    /// The server it's about; `None` for the panel itself or the server list.
    pub server: Option<String>,
    /// What changed: `list` (servers added, removed, renamed), `server`
    /// (its settings: name, port, memory, MOTD, properties, version),
    /// `mods`, `configs`, `files`, `backups`, `schedule`, `players`,
    /// `worlds`, `panel` (panel-wide settings, users, rules…). `*`: anything.
    pub areas: Vec<String>,
    /// The client that made the change (its `X-Hopperd-Client`), so it can
    /// skip reloading what it already shows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

pub struct Changes {
    ring: Mutex<VecDeque<Change>>,
    rev: watch::Sender<u64>,
}

impl Default for Changes {
    fn default() -> Self {
        Changes { ring: Mutex::new(VecDeque::new()), rev: watch::channel(0).0 }
    }
}

impl Changes {
    /// Records a change and wakes everyone waiting.
    pub fn emit(&self, server: Option<&str>, areas: &[&str], by: Option<String>) {
        if areas.is_empty() {
            return;
        }
        let mut ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        let rev = *self.rev.borrow() + 1;
        ring.push_back(Change {
            rev,
            server: server.map(str::to_string),
            areas: areas.iter().map(|a| a.to_string()).collect(),
            by,
        });
        while ring.len() > KEEP {
            ring.pop_front();
        }
        self.rev.send_replace(rev);
    }

    pub fn rev(&self) -> u64 {
        *self.rev.borrow()
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.rev.subscribe()
    }

    /// Events after `since`, or `None` when some of them are gone (the
    /// client must reload everything).
    pub fn since(&self, since: u64) -> Option<Vec<Change>> {
        let ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        let now = *self.rev.borrow();
        if since > now {
            return None;
        }
        if since == now {
            return Some(Vec::new());
        }
        match ring.front() {
            Some(first) if first.rev <= since + 1 => Some(ring.iter().filter(|c| c.rev > since).cloned().collect()),
            _ => None,
        }
    }
}

/// Records a change on the panel (for code that changes things outside an
/// API request, such as a job finishing).
#[allow(dead_code)] // for work that lands outside a request (jobs)
pub fn emit(app: &App, server: Option<&str>, areas: &[&str]) {
    app.changes.emit(server, areas, None);
}

// ---- events from API requests ---------------------------------------------

/// Last path parts of POST routes that only read (plans, previews, checks).
const READS: &[&str] = &[
    "parse", "preview", "plan", "check", "resolve", "related", "hashes", "share", "search", "diff", "walk", "zip-list",
    "zip-info", "clone-info", "test", "orphans",
];

/// What a successful change request changed: (server, areas). `None` for
/// requests that change nothing clients show (sign-ins, power, console
/// commands: status comes with the live snapshot).
pub fn from_request(method: &Method, path: &str) -> Option<(Option<String>, Vec<&'static str>)> {
    if *method == Method::GET || *method == Method::HEAD || *method == Method::OPTIONS {
        return None;
    }
    let rest = path.strip_prefix("/api/v1/")?;
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    let last = *parts.last()?;
    if READS.contains(&last) {
        return None;
    }
    match parts.as_slice() {
        ["servers", "create" | "create-from-pack" | "from-template" | "import-folder" | "import-zip" | "rescan"] => {
            Some((None, vec!["list"]))
        }
        ["servers", "infos"] | ["servers"] => None,
        ["servers", id] => Some((Some(id.to_string()), vec!["list", "server"])),
        ["servers", id, area, ..] => {
            let areas: Vec<&'static str> = match *area {
                "power" | "command" | "console" | "logs" | "icon" => return None,
                "mods" | "mod-backups" | "packs" | "datapacks" | "crash" | "neoforge" => vec!["mods"],
                "pack" | "version" => vec!["mods", "server"],
                "configs" => vec!["configs"],
                "backups" => vec!["backups"],
                "schedule" => vec!["schedule"],
                "players" => vec!["players"],
                "worlds" => vec!["worlds"],
                "properties" | "jvm" | "address" | "rcon" => vec!["server"],
                "remove" | "clone" => vec!["list"],
                "template" => vec!["panel"],
                _ => vec!["server"],
            };
            Some((Some(id.to_string()), areas))
        }
        ["fs", id, what, ..] => {
            let areas: Vec<&'static str> = match *what {
                "config" => vec!["configs", "files"],
                _ => vec!["files"],
            };
            Some((Some(id.to_string()), areas))
        }
        ["deleted", ..] => Some((None, vec!["list"])),
        ["browse", "install"] => Some((None, vec!["mods"])),
        ["backup-receive", ..] | ["backup-store", ..] | ["backup-targets", ..] => Some((None, vec!["backups", "panel"])),
        [
            "users" | "tokens" | "devices" | "webhooks" | "rules" | "alerts" | "settings" | "templates" | "router" | "panel",
            ..,
        ] => Some((None, vec!["panel"])),
        _ => None,
    }
}

// ---- the long-poll route ----------------------------------------------------

pub fn routes() -> Router<Arc<App>> {
    Router::new().route("/api/v1/changes", get(changes))
}

/// `?boot=&since=&wait=`: events after `since` this panel run, waiting up to
/// `wait` seconds (at most 25) for one. Without `since`: the current place
/// to start from.
async fn changes(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let who = require(&app, &headers, &method)?;
    let boot = crate::ui::boot_id();
    let since: Option<u64> = q.get("since").and_then(|s| s.parse().ok());
    let same_run = q.get("boot").is_none_or(|b| b == boot);
    let (Some(mut since), true) = (since, same_run) else {
        return Ok(Json(json!({ "boot": boot, "rev": app.changes.rev(), "changes": [], "reset": since.is_some() })));
    };
    let wait = Duration::from_secs(q.get("wait").and_then(|w| w.parse().ok()).unwrap_or(0).min(MAX_WAIT));
    let deadline = tokio::time::Instant::now() + wait;
    let mut rx = app.changes.subscribe();
    loop {
        rx.borrow_and_update();
        let Some(list) = app.changes.since(since) else {
            return Ok(Json(json!({ "boot": boot, "rev": app.changes.rev(), "changes": [], "reset": true })));
        };
        let rev = list.last().map_or(since, |c| c.rev);
        let seen: Vec<Change> = list.into_iter().filter(|c| c.server.as_deref().is_none_or(|s| who.sees(s))).collect();
        if !seen.is_empty() {
            return Ok(Json(json!({ "boot": boot, "rev": rev, "changes": seen, "reset": false })));
        }
        since = rev;
        if tokio::time::timeout_at(deadline, rx.changed()).await.is_err() {
            return Ok(Json(json!({ "boot": boot, "rev": since, "changes": [], "reset": false })));
        }
    }
}

// ---- events from the disk ---------------------------------------------------

/// What a changed file inside a server folder means to clients (`rel`:
/// relative path, forward or back slashes). `None` for files nobody shows
/// live (logs, the world, caches: they change all the time while it runs).
pub fn area_of(rel: &Path) -> Option<&'static str> {
    let mut parts = rel.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase());
    let first = parts.next()?;
    let deeper = parts.next().is_some();
    Some(match first.as_str() {
        "mods" => "mods",
        "config" | "defaultconfigs" => "configs",
        ".hopper-backups" => "backups",
        ".hopper-schedule.json" => "schedule",
        // The panel's own record of the mods (names, icons, enabled).
        ".hopper-pack.json" | "hopper.json" => "mods",
        ".hopper-worlds.json" => "worlds",
        "server.properties" | "user_jvm_args.txt" | ".hopper-server.json" | ".hopper-motd.json" | ".hopper-route.json" => "server",
        "whitelist.json" | "ops.json" | "banned-players.json" | "banned-ips.json" => "players",
        // Anything else at the top: the Files tab's first page.
        _ if deeper => return None,
        f if f.starts_with(".hopper-") || f.ends_with(".log") || f.ends_with(".lck") || f == "usercache.json" => return None,
        _ => "files",
    })
}

/// Folders watched inside each server: (name, recursive). The top level
/// is watched without its subfolders, so a running world doesn't flood us.
const WATCHED: &[(&str, bool)] = &[("", false), ("mods", false), ("config", true), ("defaultconfigs", true), (".hopper-backups", false)];

/// Watches every server folder and records what changed, at most once per
/// server and area every third of a second. Follows servers being added and
/// removed.
pub fn watch(app: Arc<App>) {
    use notify::{RecursiveMode, Watcher};
    let (tx, rx) = std::sync::mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = match notify::recommended_watcher(tx) {
        Ok(w) => w,
        Err(e) => {
            log::warn!("change watcher off: {e}");
            return;
        }
    };
    let _ = std::thread::Builder::new().name("change-watch".into()).spawn(move || {
        let mut watched: Vec<PathBuf> = Vec::new();
        let mut roots: Vec<(PathBuf, String)> = Vec::new();
        let mut pending: BTreeSet<(String, &'static str)> = BTreeSet::new();
        let mut last_scan = std::time::Instant::now() - Duration::from_secs(60);
        loop {
            // Servers come and go: re-check the folders every few seconds.
            if last_scan.elapsed() > Duration::from_secs(5) {
                last_scan = std::time::Instant::now();
                let now: Vec<(PathBuf, String)> = app.servers.list().iter().map(|e| (e.root.clone(), e.id.clone())).collect();
                let want: Vec<PathBuf> = now
                    .iter()
                    .flat_map(|(root, _)| WATCHED.iter().map(move |(d, _)| if d.is_empty() { root.clone() } else { root.join(d) }))
                    .filter(|p| p.is_dir())
                    .collect();
                for p in watched.iter().filter(|p| !want.contains(p)) {
                    let _ = watcher.unwatch(p);
                }
                for p in want.iter().filter(|p| !watched.contains(p)) {
                    let recursive = WATCHED.iter().any(|(d, r)| *r && !d.is_empty() && p.ends_with(d));
                    let mode = if recursive { RecursiveMode::Recursive } else { RecursiveMode::NonRecursive };
                    if let Err(e) = watcher.watch(p, mode) {
                        log::debug!("watching {}: {e}", p.display());
                    }
                }
                watched = want;
                roots = now;
            }
            let wait = if pending.is_empty() { Duration::from_secs(5) } else { Duration::from_millis(300) };
            match rx.recv_timeout(wait) {
                Ok(Ok(ev)) => {
                    if matches!(ev.kind, notify::EventKind::Access(_)) {
                        continue;
                    }
                    for p in &ev.paths {
                        let Some((root, id)) = roots.iter().filter(|(r, _)| p.starts_with(r)).max_by_key(|(r, _)| r.as_os_str().len()) else {
                            continue;
                        };
                        if let Some(area) = p.strip_prefix(root).ok().and_then(area_of) {
                            pending.insert((id.clone(), area));
                        }
                    }
                }
                Ok(Err(e)) => log::debug!("change watcher: {e}"),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Quiet for a moment: report what piled up, one
                    // event per server.
                    let mut by: Vec<(String, Vec<&'static str>)> = Vec::new();
                    for (id, area) in std::mem::take(&mut pending) {
                        match by.iter_mut().find(|(i, _)| *i == id) {
                            Some((_, a)) => a.push(area),
                            None => by.push((id, vec![area])),
                        }
                    }
                    for (id, areas) in by {
                        app.changes.emit(Some(&id), &areas, None);
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(m: Method, p: &str) -> Option<(Option<String>, Vec<&'static str>)> {
        from_request(&m, p)
    }

    #[test]
    fn requests_name_what_they_change() {
        let tnp = Some("tnp".to_string());
        assert_eq!(req(Method::POST, "/api/v1/servers/tnp/mods/toggle"), Some((tnp.clone(), vec!["mods"])));
        assert_eq!(req(Method::PUT, "/api/v1/servers/tnp/properties"), Some((tnp.clone(), vec!["server"])));
        assert_eq!(req(Method::PUT, "/api/v1/servers/tnp/schedule"), Some((tnp.clone(), vec!["schedule"])));
        assert_eq!(req(Method::POST, "/api/v1/servers/tnp/players"), Some((tnp.clone(), vec!["players"])));
        assert_eq!(req(Method::DELETE, "/api/v1/servers/tnp/backups/b1"), Some((tnp.clone(), vec!["backups"])));
        assert_eq!(req(Method::POST, "/api/v1/servers/tnp/configs/batch"), Some((tnp.clone(), vec!["configs"])));
        assert_eq!(req(Method::PUT, "/api/v1/fs/tnp/text"), Some((tnp.clone(), vec!["files"])));
        assert_eq!(req(Method::POST, "/api/v1/fs/tnp/config/set"), Some((tnp.clone(), vec!["configs", "files"])));
        assert_eq!(req(Method::PUT, "/api/v1/servers/tnp"), Some((tnp.clone(), vec!["list", "server"])));
        assert_eq!(req(Method::POST, "/api/v1/servers/tnp/remove"), Some((tnp.clone(), vec!["list"])));
        assert_eq!(req(Method::POST, "/api/v1/servers/create"), Some((None, vec!["list"])));
        assert_eq!(req(Method::POST, "/api/v1/servers/import-zip"), Some((None, vec!["list"])));
        assert_eq!(req(Method::PUT, "/api/v1/settings/curseforge"), Some((None, vec!["panel"])));
        assert_eq!(req(Method::POST, "/api/v1/browse/install"), Some((None, vec!["mods"])));
    }

    #[test]
    fn reads_and_status_send_nothing() {
        assert_eq!(req(Method::GET, "/api/v1/servers/tnp/mods"), None);
        assert_eq!(req(Method::POST, "/api/v1/servers/tnp/power"), None);
        assert_eq!(req(Method::POST, "/api/v1/servers/tnp/command"), None);
        assert_eq!(req(Method::POST, "/api/v1/servers/tnp/pack/plan"), None);
        assert_eq!(req(Method::POST, "/api/v1/servers/tnp/jvm/preview"), None);
        assert_eq!(req(Method::POST, "/api/v1/fs/tnp/hashes"), None);
        assert_eq!(req(Method::POST, "/api/v1/servers/tnp/configs/parse"), None);
        assert_eq!(req(Method::POST, "/api/v1/auth/login"), None);
        assert_eq!(req(Method::POST, "/api/v1/mcp"), None);
        assert_eq!(req(Method::POST, "/api/v1/browse/search"), None);
    }

    #[test]
    fn files_map_to_areas() {
        let a = |p: &str| area_of(Path::new(p));
        assert_eq!(a("mods/jei.jar"), Some("mods"));
        assert_eq!(a("config/jei/jei-client.toml"), Some("configs"));
        assert_eq!(a("server.properties"), Some("server"));
        assert_eq!(a("whitelist.json"), Some("players"));
        assert_eq!(a(".hopper-schedule.json"), Some("schedule"));
        assert_eq!(a("hopper.json"), Some("mods"));
        assert_eq!(a(".hopper-backups/2026-11-04.zip"), Some("backups"));
        assert_eq!(a("start.bat"), Some("files"));
        assert_eq!(a("logs/latest.log"), None);
        assert_eq!(a("world/level.dat"), None);
        assert_eq!(a("usercache.json"), None);
        assert_eq!(a(".hopper-trash"), None);
    }

    #[test]
    fn catching_up() {
        let c = Changes::default();
        assert_eq!(c.since(0), Some(vec![]));
        c.emit(Some("a"), &["mods"], None);
        c.emit(None, &["list"], Some("tab1".into()));
        let got = c.since(0).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].by.as_deref(), Some("tab1"));
        assert_eq!(c.since(1).unwrap().len(), 1);
        assert_eq!(c.since(2), Some(vec![]));
        // A client ahead of us (the panel restarted): reload.
        assert_eq!(c.since(9), None);
        // A client too far behind: reload.
        for _ in 0..KEEP + 5 {
            c.emit(Some("a"), &["files"], None);
        }
        assert_eq!(c.since(1), None);
        assert!(c.since(c.rev() - 3).is_some_and(|v| v.len() == 3));
        // Nothing to say: no event.
        let before = c.rev();
        c.emit(Some("a"), &[], None);
        assert_eq!(c.rev(), before);
    }
}
