//! Webhooks: Discord or any URL hears when servers start, stop, crash,
//! players join or leave, backups finish and schedules act.
//!
//! Events come from comparing each live snapshot with the one before, and
//! from the scheduler. Hooks live in `webhooks.json` in the data folder;
//! only the owner can see or change them (their URLs are secrets).

use crate::App;
use crate::api::{ApiResult, bad, not_found, require_owner};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use hopper_core::hosting::ScheduleEvent;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const FILE: &str = "webhooks.json";

/// Every event a hook can ask for.
pub const EVENTS: &[(&str, &str)] = &[
    ("start", "Server started"),
    ("stop", "Server stopped"),
    ("crash", "Server crashed"),
    ("join", "Player joined"),
    ("leave", "Player left"),
    ("backup", "Backup made"),
    ("wake", "Server woken up by a player joining"),
    ("sleep", "Server put to sleep (nobody on)"),
    ("schedule", "Scheduled restarts and warnings"),
    ("problem", "Something the panel tried failed"),
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hook {
    pub id: String,
    pub name: String,
    pub url: String,
    /// "discord" or "json".
    pub kind: String,
    pub events: Vec<String>,
    /// Server ids; empty means all.
    #[serde(default)]
    pub servers: Vec<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// The last delivery's error, if it failed.
    #[serde(default)]
    pub last_error: Option<String>,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub event: &'static str,
    pub server_id: String,
    pub server: String,
    pub text: String,
    pub at: u64,
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn hooks_lock() -> &'static Mutex<Option<Vec<Hook>>> {
    static H: OnceLock<Mutex<Option<Vec<Hook>>>> = OnceLock::new();
    H.get_or_init(Default::default)
}

fn load(app: &App) -> Vec<Hook> {
    let mut g = hooks_lock().lock().unwrap();
    g.get_or_insert_with(|| {
        std::fs::read(app.data_dir.join(FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    })
    .clone()
}

fn save(app: &App, hooks: Vec<Hook>) -> anyhow::Result<()> {
    let path = app.data_dir.join(FILE);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&hooks)?)?;
    std::fs::rename(&tmp, &path)?;
    *hooks_lock().lock().unwrap() = Some(hooks);
    Ok(())
}

fn agent() -> &'static ureq::Agent {
    static A: OnceLock<ureq::Agent> = OnceLock::new();
    A.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .user_agent(concat!("hopperd/", env!("CARGO_PKG_VERSION")))
            .http_status_as_error(false)
            .build()
            .into()
    })
}

fn color(event: &str) -> u32 {
    match event {
        "start" | "join" => 0x22c55e,
        "stop" | "leave" => 0x71717a,
        "crash" | "problem" => 0xef4444,
        "backup" => 0x60a5fa,
        _ => 0xf97316,
    }
}

fn body(hook: &Hook, e: &Event, panel: &str) -> Value {
    if hook.kind == "discord" {
        json!({
            "username": panel,
            "embeds": [{ "description": e.text, "color": color(e.event), "footer": { "text": e.server }, "timestamp": rfc3339(e.at) }],
            "allowed_mentions": { "parse": [] },
        })
    } else {
        json!({ "event": e.event, "server_id": e.server_id, "server": e.server, "text": e.text.replace("**", ""), "at": e.at, "panel": panel })
    }
}

fn rfc3339(t: u64) -> String {
    time::OffsetDateTime::from_unix_timestamp(t as i64)
        .ok()
        .and_then(|d| d.format(&time::format_description::well_known::Rfc3339).ok())
        .unwrap_or_default()
}

/// Posts one event to one hook. Err is the reason, for the UI.
fn deliver(hook: &Hook, e: &Event, panel: &str) -> Result<(), String> {
    let r = agent().post(&hook.url).send_json(body(hook, e, panel)).map_err(|err| err.to_string())?;
    let status = r.status().as_u16();
    if (200..300).contains(&status) { Ok(()) } else { Err(format!("{} answered HTTP {status}", host_of(&hook.url))) }
}

fn host_of(url: &str) -> String {
    url.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or(url).to_string()
}

/// Sends `events` to every hook that wants them, in the background.
pub fn emit(app: &Arc<App>, events: Vec<Event>) {
    if events.is_empty() {
        return;
    }
    for e in &events {
        log::info!("event {}: {}", e.event, e.text);
    }
    let hooks: Vec<Hook> = load(app).into_iter().filter(|h| h.enabled).collect();
    if hooks.is_empty() {
        return;
    }
    let app = app.clone();
    let panel = format!("Hopper Server · {}", app.cfg.read().unwrap().name);
    std::thread::spawn(move || {
        let mut errors: HashMap<String, Option<String>> = HashMap::new();
        for h in &hooks {
            for e in events.iter().filter(|e| h.events.iter().any(|x| x == e.event) && (h.servers.is_empty() || h.servers.contains(&e.server_id))) {
                let r = deliver(h, e, &panel);
                if let Err(err) = &r {
                    log::warn!("webhook {}: {err}", h.name);
                }
                errors.insert(h.id.clone(), r.err());
            }
        }
        // Remember the outcome for the UI.
        if errors.iter().any(|(id, e)| hooks.iter().any(|h| &h.id == id && &h.last_error != e)) {
            let mut all = load(&app);
            for h in &mut all {
                if let Some(e) = errors.get(&h.id) {
                    h.last_error = e.clone();
                }
            }
            let _ = save(&app, all);
        }
    });
}

#[derive(Clone, PartialEq)]
struct Seen {
    state: &'static str,
    players: Vec<String>,
    /// Already up when the panel first saw it (the panel restarted and
    /// picked the server up again). Its "running" isn't a start.
    picked_up: bool,
}

fn alive(state: &str) -> bool {
    matches!(state, "starting" | "running" | "stopping")
}

/// What changed between two looks at the servers.
fn changes(before: &HashMap<String, Seen>, s: &crate::servers::Summary) -> Vec<Event> {
    let mut out = Vec::new();
    let Some(b) = before.get(&s.id) else { return out };
    let ev = |event, text: String| Event { event, server_id: s.id.clone(), server: s.name.clone(), text, at: now() };
    if b.state != s.state {
        match (b.state, s.state) {
            (_, "running") if !b.picked_up => out.push(ev("start", format!("**{}** is up", s.name))),
            (_, "crashed") => out.push(ev(
                "crash",
                format!("**{}** crashed{}", s.name, s.exit_code.map(|c| format!(" (exit code {c})")).unwrap_or_default()),
            )),
            ("running" | "stopping", "stopped") => out.push(ev("stop", format!("**{}** stopped", s.name))),
            _ => {}
        }
    }
    // Players only count while the server stays up (a stop isn't everyone leaving).
    if b.state == "running" && s.state == "running" {
        for p in s.players.iter().filter(|p| !b.players.contains(p)) {
            out.push(ev("join", format!("{p} joined **{}**", s.name)));
        }
        for p in b.players.iter().filter(|p| !s.players.contains(p)) {
            out.push(ev("leave", format!("{p} left **{}**", s.name)));
        }
    }
    out
}

fn from_schedule(app: &App, e: &ScheduleEvent) -> Option<Event> {
    let entry = app.servers.list().into_iter().find(|x| x.root == e.root())?;
    let kind = match e {
        ScheduleEvent::BackedUp { .. } => "backup",
        ScheduleEvent::Failed { .. } | ScheduleEvent::GaveUp { .. } => "problem",
        _ => "schedule",
    };
    Some(Event { event: kind, server_id: entry.id.clone(), server: entry.name.clone(), text: e.message(), at: now() })
}

/// Watches the live snapshot and the scheduler for as long as the panel runs.
pub fn watch(app: Arc<App>) {
    tokio::spawn(async move {
        let mut rx = app.snapshot.subscribe();
        let mut before: HashMap<String, Seen> = HashMap::new();
        let mut first = true;
        loop {
            if rx.changed().await.is_err() {
                return;
            }
            let snap = rx.borrow_and_update().clone();
            let mut events = Vec::new();
            for s in &snap.servers {
                if !first {
                    events.extend(changes(&before, s));
                }
                let picked_up = match before.get(&s.id) {
                    Some(b) => b.picked_up && alive(s.state),
                    None => alive(s.state),
                };
                before.insert(s.id.clone(), Seen { state: s.state, players: s.players.clone(), picked_up });
            }
            // Servers that just appeared count from the next look.
            first = false;
            let sched = app.scheduler.lock().unwrap().as_ref().map(|s| s.events()).unwrap_or_default();
            events.extend(sched.iter().filter_map(|e| from_schedule(&app, e)));
            emit(&app, events);
        }
    });
}

// ---- API -------------------------------------------------------------------

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/webhooks", get(list).post(create))
        .route("/api/v1/webhooks/{id}", put(update).delete(remove))
        .route("/api/v1/webhooks/{id}/test", post(test))
}

#[derive(Deserialize)]
struct HookBody {
    name: String,
    url: String,
    #[serde(default)]
    kind: String,
    events: Vec<String>,
    #[serde(default)]
    servers: Vec<String>,
    #[serde(default = "yes")]
    enabled: bool,
}

fn check(b: &HookBody) -> ApiResult<String> {
    let url = b.url.trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(bad("The URL has to start with https:// (or http://)."));
    }
    if let Some(e) = b.events.iter().find(|e| !EVENTS.iter().any(|(k, _)| k == e)) {
        return Err(bad(format!("Unknown event {e}.")));
    }
    let kind = if b.kind.is_empty() {
        if url.contains("discord.com/api/webhooks") || url.contains("discordapp.com/api/webhooks") { "discord" } else { "json" }
    } else {
        b.kind.as_str()
    };
    if kind != "discord" && kind != "json" {
        return Err(bad("The kind is discord or json."));
    }
    Ok(kind.to_string())
}

/// Hooks without the secret part of their URLs.
fn shown(hooks: &[Hook]) -> Vec<Value> {
    hooks
        .iter()
        .map(|h| {
            let mut v = serde_json::to_value(h).unwrap_or_default();
            v["url"] = json!(mask(&h.url));
            v
        })
        .collect()
}

/// "https://discord.com/api/webhooks/123/abcdef…" → "https://discord.com/api/webhooks/123/••••".
fn mask(url: &str) -> String {
    match url.rfind('/') {
        Some(i) if i + 1 < url.len() && url[..i].matches('/').count() >= 3 => format!("{}/••••", &url[..i]),
        _ => url.to_string(),
    }
}

async fn list(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let events: Vec<Value> = EVENTS.iter().map(|(k, d)| json!({ "id": k, "label": d })).collect();
    Ok(Json(json!({ "hooks": shown(&load(&app)), "events": events })))
}

async fn create(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<HookBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let kind = check(&b)?;
    let mut all = load(&app);
    let h = Hook {
        id: crate::auth::random_hex(6),
        name: if b.name.trim().is_empty() { host_of(b.url.trim()) } else { b.name.trim().to_string() },
        url: b.url.trim().to_string(),
        kind,
        events: b.events,
        servers: b.servers,
        enabled: b.enabled,
        last_error: None,
    };
    all.push(h.clone());
    save(&app, all).map_err(bad)?;
    Ok(Json(json!({ "id": h.id })))
}

async fn update(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(mut b): Json<HookBody>,
) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let mut all = load(&app);
    let h = all.iter_mut().find(|h| h.id == id).ok_or_else(|| not_found("webhook"))?;
    // The UI only has the masked URL; keeping it means "unchanged".
    if b.url.trim() == mask(&h.url) || b.url.trim().is_empty() {
        b.url = h.url.clone();
    }
    let kind = check(&b)?;
    h.name = b.name.trim().to_string();
    h.url = b.url.trim().to_string();
    h.kind = kind;
    h.events = b.events;
    h.servers = b.servers;
    h.enabled = b.enabled;
    h.last_error = None;
    save(&app, all).map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

async fn remove(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let mut all = load(&app);
    let n = all.len();
    all.retain(|h| h.id != id);
    if all.len() == n {
        return Err(not_found("webhook"));
    }
    save(&app, all).map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

/// Sends a sample event right away and says how it went.
async fn test(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let h = load(&app).into_iter().find(|h| h.id == id).ok_or_else(|| not_found("webhook"))?;
    let name = app.cfg.read().unwrap().name.clone();
    let e = Event { event: "start", server_id: String::new(), server: name.clone(), text: "Test from Hopper Server: webhooks work.".into(), at: now() };
    let panel = format!("Hopper Server · {name}");
    let r = tokio::task::spawn_blocking(move || deliver(&h, &e, &panel)).await.map_err(bad)?;
    match r {
        Ok(()) => Ok(Json(json!({ "ok": true }))),
        Err(e) => Err(bad(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(state: &'static str, players: &[&str]) -> crate::servers::Summary {
        crate::servers::Summary {
            id: "s".into(),
            name: "S".into(),
            path: String::new(),
            port: 0,
            state,
            exit_code: Some(1),
            managed_by: None,
            loader: None,
            version: String::new(),
            max_memory: None,
            cpu: 0.0,
            mem_bytes: 0,
            players: players.iter().map(|p| p.to_string()).collect(),
            max_players: 0,
            uptime_secs: None,
            addresses: Vec::new(),
            sleep_after_minutes: 0,
        }
    }

    fn seen(state: &'static str, players: &[&str]) -> HashMap<String, Seen> {
        HashMap::from([("s".to_string(), Seen { state, players: players.iter().map(|p| p.to_string()).collect(), picked_up: false })])
    }

    #[test]
    fn transitions() {
        let kinds = |b, s| changes(&b, &s).iter().map(|e| e.event).collect::<Vec<_>>();
        assert_eq!(kinds(seen("starting", &[]), summary("running", &[])), ["start"]);
        assert_eq!(kinds(seen("running", &["a"]), summary("crashed", &[])), ["crash"]);
        assert_eq!(kinds(seen("stopping", &[]), summary("stopped", &[])), ["stop"]);
        assert_eq!(kinds(seen("running", &["a", "b"]), summary("running", &["b", "c"])), ["join", "leave"]);
        assert_eq!(kinds(seen("running", &["a"]), summary("stopped", &[])), ["stop"], "stopping isn't everyone leaving");
        assert!(kinds(seen("stopped", &[]), summary("starting", &[])).is_empty());
        assert!(changes(&HashMap::new(), &summary("running", &[])).is_empty());
        // Picked up again after the panel restarted: reaching "running" isn't a start.
        let mut b = seen("starting", &[]);
        b.get_mut("s").unwrap().picked_up = true;
        assert!(kinds(b, summary("running", &[])).is_empty());
    }

    #[test]
    fn masks_secrets() {
        assert_eq!(mask("https://discord.com/api/webhooks/123/abc"), "https://discord.com/api/webhooks/123/••••");
        assert_eq!(mask("https://example.com"), "https://example.com");
    }
}
