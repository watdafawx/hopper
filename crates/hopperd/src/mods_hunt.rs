//! Mod profiles (named sets of mods that are off, to switch between) and
//! finding the mod that breaks a server, as in Hopper desktop's Mods tab.
//! The rounds are hopper-core's `bisect`; a server can also run them by
//! itself: start it with half the suspects off, see whether it gets to
//! "Done", answer, repeat.
//!
//! Booting a world with mods missing drops their blocks when it saves, so
//! a hunt plays on a throwaway world by default (`level-name` is switched
//! and put back at the end).

use crate::App;
use crate::api::{ApiError, ApiResult, bad, not_found, require};
use crate::servers::Entry;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use hopper_core::bisect::{self, Answer, Hunt, Kind, Phase, RowState};
use hopper_core::hosting::{Properties, ServerState, process::STOP_TIMEOUT};
use hopper_core::instances::Instance;
use hopper_core::install;
use hopper_core::panel::PanelAction;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/servers/{id}/mods/profiles", get(profiles))
        .route("/api/v1/servers/{id}/mods/profiles/save", post(save_profile))
        .route("/api/v1/servers/{id}/mods/profiles/use", post(use_profile))
        .route("/api/v1/servers/{id}/mods/profiles/delete", post(delete_profile))
        .route("/api/v1/servers/{id}/mods/hunt", get(status))
        .route("/api/v1/servers/{id}/mods/hunt/start", post(start))
        .route("/api/v1/servers/{id}/mods/hunt/answer", post(answer))
        .route("/api/v1/servers/{id}/mods/hunt/undo", post(undo))
        .route("/api/v1/servers/{id}/mods/hunt/fine", post(fine))
        .route("/api/v1/servers/{id}/mods/hunt/finish", post(finish))
        .route("/api/v1/servers/{id}/mods/hunt/auto", post(auto))
}

fn entry(app: &App, headers: &HeaderMap, method: &Method, id: &str) -> ApiResult<Arc<Entry>> {
    require(app, headers, method)?;
    app.servers.get(id).ok_or_else(|| not_found("server"))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(bad)?.map_err(|e| bad(format!("{e:#}")))
}

fn conflict(msg: &str) -> ApiError {
    ApiError(StatusCode::CONFLICT, msg.to_string())
}

/// A hunt owns which mods are on until it ends.
pub(crate) fn hunting(root: &std::path::Path) -> bool {
    bisect::path(root).is_file()
}

pub(crate) fn refuse_while_hunting(e: &Entry) -> ApiResult<()> {
    if hunting(&e.root) {
        return Err(conflict("Finding the mod that breaks it is running: it decides which mods are on until you stop it."));
    }
    Ok(())
}

// ---- profiles ---------------------------------------------------------------

fn names(list: &[&hopper_core::instances::Addon], cap: usize) -> Vec<String> {
    list.iter()
        .take(cap)
        .map(|a| if a.name.is_empty() { a.file_name.trim_end_matches(".jar").to_string() } else { a.name.clone() })
        .collect()
}

/// Each profile with what switching to it would turn on and off now.
async fn profiles(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let v = blocking(move || {
        let inst = crate::mods_api::instance_of(&e)?;
        let list: Vec<Value> = inst
            .mod_profiles
            .iter()
            .map(|p| {
                let (on, off) = install::profile_changes(&inst, p);
                json!({
                    "name": p.name, "disabled": p.disabled.len(),
                    "turns_on": on.len(), "turns_off": off.len(),
                    "on_names": names(&on, 4), "off_names": names(&off, 4),
                })
            })
            .collect();
        Ok(json!({ "profiles": list }))
    })
    .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct NameBody {
    name: String,
}

async fn save_profile(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<NameBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let name = b.name.trim().to_string();
    if name.is_empty() {
        return Err(bad("Give the profile a name."));
    }
    blocking(move || {
        let mut inst = crate::mods_api::instance_of(&e)?;
        install::save_profile(&mut inst, &name)
    })
    .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn use_profile(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<NameBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    refuse_while_hunting(&e)?;
    let running = e.server.state().is_alive();
    log::info!("{}: switching to mod profile {}", e.name, b.name);
    let n = blocking(move || {
        let mut inst = crate::mods_api::instance_of(&e)?;
        install::apply_profile(&mut inst, &b.name).map_err(install::explain_locked)
    })
    .await?;
    Ok(Json(json!({ "changed": n, "restart_needed": running && n > 0 })))
}

async fn delete_profile(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<NameBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let gone = blocking(move || {
        let mut inst = crate::mods_api::instance_of(&e)?;
        install::delete_profile(&mut inst, &b.name)
    })
    .await?;
    Ok(Json(json!({ "deleted": gone })))
}

// ---- the hunt's server side ---------------------------------------------------

/// Beside the journal: what only a server hunt needs.
const SIDE: &str = "hopper-bisect-server.json";
/// The throwaway world rounds play on.
const SCRATCH: &str = "hopper-hunt-world";
/// A round that hasn't reached "Done" by then counts as still broken.
const ROUND_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Side {
    /// Hopper runs the rounds.
    auto: bool,
    /// `level-name` before the hunt, when rounds play on [`SCRATCH`].
    world: Option<String>,
    /// What the rounds did, newest last.
    log: Vec<String>,
    /// Why the rounds stopped by themselves.
    paused: Option<String>,
    /// When the round being played was started (Unix seconds).
    round_started: Option<u64>,
    last_round_secs: Option<u64>,
}

fn load_side(root: &std::path::Path) -> Side {
    std::fs::read_to_string(root.join(SIDE)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save_side(root: &std::path::Path, s: &Side) {
    if let Ok(b) = serde_json::to_vec_pretty(s)
        && let Err(e) = std::fs::write(root.join(SIDE), b)
    {
        log::warn!("writing {SIDE}: {e}");
    }
}

fn edit_side(root: &std::path::Path, f: impl FnOnce(&mut Side)) {
    let mut s = load_side(root);
    f(&mut s);
    save_side(root, &s);
}

/// "2:31" (minutes:seconds).
fn took(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn note(root: &std::path::Path, line: String) {
    let t = hopper_core::hosting::schedule::LocalTime::now().hhmm();
    edit_side(root, |s| {
        s.log.push(format!("[{t}] {line}"));
        let n = s.log.len();
        if n > 40 {
            s.log.drain(..n - 40);
        }
    });
}

/// Servers whose rounds run now, and ones asked to pause.
static RUNNING: Mutex<Option<HashSet<String>>> = Mutex::new(None);
static PAUSE: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn set_has(m: &Mutex<Option<HashSet<String>>>, id: &str) -> bool {
    m.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|s| s.contains(id))
}

fn set_put(m: &Mutex<Option<HashSet<String>>>, id: &str, on: bool) -> bool {
    let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
    let s = g.get_or_insert_with(HashSet::new);
    if on { s.insert(id.to_string()) } else { s.remove(id) }
}

fn active(id: &str) -> bool {
    set_has(&RUNNING, id)
}

fn refuse_while_auto(e: &Entry) -> ApiResult<()> {
    if active(&e.id) {
        return Err(conflict("Hopper is running the rounds. Pause it first to answer yourself."));
    }
    Ok(())
}

fn stop_server(e: &Entry) {
    if e.server.state().is_alive() {
        e.server.stop_blocking(STOP_TIMEOUT);
    }
    // A crash stays shown until dismissed.
    let _ = e.server.stop(STOP_TIMEOUT);
}

/// The mod's name: Hopper's, else the jar's own, else its file name.
fn mod_name(inst: &Instance, file: &str) -> String {
    if let Some(a) = inst.addon_by_file(file).filter(|a| !a.name.is_empty()) {
        return a.name.clone();
    }
    let dir = inst.mods_dir();
    let p = if dir.join(file).is_file() { dir.join(file) } else { dir.join(format!("{file}.disabled")) };
    let (_, name) = hopper_core::deps::own_ids(&p);
    if name.is_empty() { file.trim_end_matches(".jar").to_string() } else { name }
}

fn mod_names(inst: &Instance, files: &[String]) -> Vec<String> {
    files.iter().map(|f| mod_name(inst, f)).collect()
}

/// The hunt for the panel: the round, counts, and each mod's chip.
fn hunt_json(h: &Hunt, inst: &Instance) -> Value {
    let (_, parked, suspects) = h.counts();
    let out: HashSet<&String> = h
        .suspects
        .iter()
        .chain(&h.parked)
        .chain(&h.fine)
        .chain(h.libraries.iter().filter(|_| h.phase == Phase::Mods))
        .collect();
    let cleared = h.original.iter().filter(|(f, on)| **on && !out.contains(f)).count();
    // Off with the culprit because they need it (only once it's found).
    let along: Vec<String> = h.parked.iter().filter(|p| h.phase == Phase::Found && !h.culprits.contains(p)).cloned().collect();
    let mut rows = serde_json::Map::new();
    for f in h.original.keys() {
        let Some(state) = h.state(f) else { continue };
        let suspect = h.suspects.iter().any(|s| s == f);
        let needs: Vec<String> = h.requires.get(f).cloned().unwrap_or_default();
        let (label, mut tip) = match state {
            RowState::Testing => ("testing", "On this round, still a suspect. Click if you know it's fine.".to_string()),
            RowState::Parked if suspect => ("parked", "Off this round, still a suspect. Click if you know it's fine.".to_string()),
            RowState::Parked if h.culprits.is_empty() => {
                let why: Vec<String> = needs.iter().filter(|n| h.parked.contains(n)).cloned().collect();
                ("parked", format!("Not a suspect: off this round because it needs {}.", mod_names(inst, &why).join(", ")))
            }
            RowState::Parked => ("parked", "Off with the culprit, because it needs it.".to_string()),
            RowState::Cleared => ("cleared", "Not the cause: ruled out by an earlier round.".to_string()),
            RowState::Library => ("library", "Many mods need it, so it stays on. Tested last, only if no other mod is the cause.".to_string()),
            RowState::Fine => ("fine", "You marked it fine. Click to make it a suspect again.".to_string()),
            RowState::Culprit => ("culprit", "Breaks the server when on.".to_string()),
        };
        let by = h.needed_by(f);
        if !by.is_empty() {
            let mut n = mod_names(inst, &by[..by.len().min(5)]);
            if by.len() > 5 {
                n.push(format!("{} more", by.len() - 5));
            }
            tip += &format!("\nNeeded by {}", n.join(", "));
        }
        let click = match state {
            RowState::Fine => Some("suspect"),
            RowState::Testing | RowState::Parked if suspect && h.suspects.len() > 1 => Some("fine"),
            _ => None,
        };
        rows.insert(f.to_lowercase(), json!({ "state": label, "tip": tip, "click": click }));
    }
    json!({
        "kind": h.kind, "phase": h.phase, "round": h.round(), "rounds": h.rounds.len(),
        "estimate": h.rounds_estimate().max(h.round()),
        "parked": parked, "suspects": suspects, "cleared": cleared, "verified": h.verified,
        "all_suspects_off": h.suspects.iter().all(|s| h.parked.contains(s)),
        "culprits": mod_names(inst, &h.culprits), "along": mod_names(inst, &along),
        "rows": rows,
    })
}

async fn status(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let v = blocking(move || {
        let inst = crate::mods_api::instance_of(&e)?;
        let h = bisect::load(&e.root);
        let side = h.as_ref().map(|_| load_side(&e.root)).unwrap_or_default();
        // Every jar on, known to Hopper or not (what a hunt would cover).
        let on = bisect::mod_files(&inst.mods_dir()).values().filter(|on| **on).count();
        Ok(json!({
            "hunt": h.as_ref().map(|h| hunt_json(h, &inst)),
            "auto": side.auto, "active": active(&e.id), "paused": side.paused,
            "log": side.log, "round_started": side.round_started, "last_round_secs": side.last_round_secs,
            "scratch_world": side.world.is_some(),
            "players": e.server.players().len(), "on": on,
            "timeout_minutes": ROUND_TIMEOUT.as_secs() / 60,
        }))
    })
    .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct StartBody {
    #[serde(default)]
    kind: Kind,
    #[serde(default)]
    auto: bool,
    #[serde(default = "yes")]
    scratch_world: bool,
}

fn yes() -> bool {
    true
}

fn props_path(e: &Entry) -> std::path::PathBuf {
    e.root.join("server.properties")
}

/// Points the server at the throwaway world; returns the old level-name.
fn use_scratch(e: &Entry) -> anyhow::Result<String> {
    let mut p = Properties::load(&props_path(e))?;
    let old = p.get("level-name").filter(|w| !w.trim().is_empty()).unwrap_or("world").to_string();
    p.set("level-name", SCRATCH);
    p.save(&props_path(e))?;
    Ok(old)
}

/// Puts the real world back and drops the throwaway one.
fn end_scratch(e: &Entry, old: &str) -> anyhow::Result<()> {
    let mut p = Properties::load(&props_path(e))?;
    p.set("level-name", old);
    p.save(&props_path(e))?;
    let dir = e.root.join(SCRATCH);
    if dir.is_dir()
        && let Err(err) = std::fs::remove_dir_all(&dir)
    {
        log::warn!("{}: removing {SCRATCH}: {err}", e.name);
    }
    Ok(())
}

async fn start(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<StartBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    refuse_while_hunting(&e)?;
    log::info!("{}: finding the mod that breaks it (auto: {})", e.name, b.auto);
    let a = app.clone();
    let job = crate::pack_api::spawn(&app, format!("Finding the mod that breaks {}", e.name), move |job| {
        job.detail("Stopping the server");
        stop_server(&e);
        let world = if b.scratch_world { Some(use_scratch(&e)?) } else { None };
        job.detail("Reading what each mod needs");
        let mut inst = crate::mods_api::instance_of(&e)?;
        let h = match bisect::begin(&mut inst, b.kind, None) {
            Ok(h) => h,
            Err(err) => {
                if let Some(w) = &world {
                    let _ = end_scratch(&e, w);
                }
                return Err(err);
            }
        };
        let _ = std::fs::remove_file(e.root.join(SIDE));
        save_side(&e.root, &Side { auto: b.auto, world, ..Default::default() });
        note(&e.root, format!("Round 1: {} of {} mods off", h.parked.len(), h.counts().0));
        if b.auto {
            launch(a, e.clone());
        }
        Ok(json!({ "parked": h.parked.len() }))
    });
    Ok(Json(json!({ "job": job })))
}

/// Changes the hunt, saves it, then renames the jars to match (the server
/// stopped first: a running one holds its jars).
fn change(
    app: &Arc<App>,
    e: Arc<Entry>,
    title: &str,
    edit: impl FnOnce(&mut Hunt, &Entry) -> anyhow::Result<Option<String>> + Send + 'static,
) -> u64 {
    crate::pack_api::spawn(app, title.to_string(), move |job| {
        let mut h = bisect::load(&e.root).ok_or_else(|| anyhow::anyhow!("No hunt is running."))?;
        let message = edit(&mut h, &e)?;
        job.detail("Stopping the server");
        stop_server(&e);
        bisect::save(&e.root, &h)?;
        let mut inst = crate::mods_api::instance_of(&e)?;
        let n = bisect::apply(&mut inst, &h)?;
        Ok(json!({ "changed": n, "message": message }))
    })
}

/// The loader refused to start because a mod needs one parked this round
/// (its jar didn't say): Hopper learns it and plans the round again
/// instead of taking the answer. The note, when it did.
fn learn_from_log(e: &Entry, h: &mut Hunt, lines: &[String]) -> Option<String> {
    let edges = bisect::needs_from_log(&e.root.join("mods"), h, lines);
    if edges.is_empty() || !h.learn(&edges) {
        return None;
    }
    let inst = crate::mods_api::instance_of(e).ok()?;
    let mut said: Vec<String> = edges
        .iter()
        .take(3)
        .map(|(a, b)| format!("{} needs {}", mod_name(&inst, a), mod_name(&inst, b)))
        .collect();
    if edges.len() > 3 {
        said.push(format!("{} more", edges.len() - 3));
    }
    Some(format!(
        "Round {}: the server refused to start because {} (the jars don't say so). Hopper keeps them together now and plays the round again.",
        h.round(),
        said.join(", ")
    ))
}

#[derive(Deserialize)]
struct AnswerBody {
    answer: Answer,
}

async fn answer(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<AnswerBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    refuse_while_auto(&e)?;
    let root = e.root.clone();
    let job = change(&app, e, "Next round of the hunt", move |h, e| {
        anyhow::ensure!(!h.done(), "The hunt is over.");
        if b.answer == Answer::Broken
            && let Some(msg) = learn_from_log(e, h, &e.server.console())
        {
            note(&root, msg.clone());
            return Ok(Some(msg));
        }
        let r = h.round();
        h.answer(b.answer);
        note(&root, format!("Round {r}: you said {}", if b.answer == Answer::Works { "it works" } else { "still broken" }));
        Ok(None)
    });
    Ok(Json(json!({ "job": job })))
}

async fn undo(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    refuse_while_auto(&e)?;
    let job = change(&app, e, "Taking back the last answer", |h, _| {
        anyhow::ensure!(h.undo(), "No answer to take back.");
        Ok(None)
    });
    Ok(Json(json!({ "job": job })))
}

#[derive(Deserialize)]
struct FineBody {
    file: String,
    fine: bool,
}

async fn fine(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<FineBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    refuse_while_auto(&e)?;
    let title = if b.fine { "Marking a mod fine" } else { "Making a mod a suspect again" };
    let job = change(&app, e, title, move |h, _| {
        let f = h.original.keys().find(|k| k.eq_ignore_ascii_case(b.file.trim_end_matches(".disabled"))).cloned().unwrap_or(b.file);
        let ok = if b.fine { h.mark_fine(&f) } else { h.unmark_fine(&f) };
        anyhow::ensure!(ok, "That mod can't change now.");
        Ok(None)
    });
    Ok(Json(json!({ "job": job })))
}

#[derive(Deserialize)]
struct FinishBody {
    #[serde(default)]
    keep_off: bool,
}

/// Ends the hunt: rounds stop, the server stops, every mod goes back (but
/// the culprit with `keep_off`) and the real world comes back.
async fn finish(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<FinishBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    if !hunting(&e.root) {
        return Err(conflict("No hunt is running."));
    }
    let title = if b.keep_off { "Restoring every mod but the culprit" } else { "Restoring every mod" };
    let job = crate::pack_api::spawn(&app, title.to_string(), move |job| {
        if active(&e.id) {
            job.detail("Stopping the rounds");
            set_put(&PAUSE, &e.id, true);
            let end = Instant::now() + STOP_TIMEOUT + Duration::from_secs(10);
            while active(&e.id) && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        job.detail("Stopping the server");
        stop_server(&e);
        let side = load_side(&e.root);
        let mut inst = crate::mods_api::instance_of(&e)?;
        let n = bisect::finish(&mut inst, b.keep_off)?;
        if let Some(w) = &side.world {
            end_scratch(&e, w)?;
        }
        let _ = std::fs::remove_file(e.root.join(SIDE));
        log::info!("{}: hunt ended, {n} mods changed", e.name);
        Ok(json!({ "changed": n }))
    });
    Ok(Json(json!({ "job": job })))
}

#[derive(Deserialize)]
struct AutoBody {
    run: bool,
}

/// Pauses Hopper's rounds, or runs them (again).
async fn auto(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<AutoBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let h = bisect::load(&e.root).ok_or_else(|| conflict("No hunt is running."))?;
    if b.run {
        if h.kind != Kind::Crash {
            return Err(bad("Hopper can only run the rounds when the server won't start or crashes."));
        }
        edit_side(&e.root, |s| {
            s.auto = true;
            s.paused = None;
        });
        launch(app.clone(), e);
    } else {
        set_put(&PAUSE, &e.id, true);
        edit_side(&e.root, |s| s.auto = false);
    }
    app.refresh.notify_one();
    Ok(Json(json!({ "ok": true })))
}

/// Runs the rounds in the background until the hunt ends or is paused.
fn launch(app: Arc<App>, e: Arc<Entry>) {
    if !set_put(&RUNNING, &e.id, true) {
        return;
    }
    set_put(&PAUSE, &e.id, false);
    std::thread::Builder::new()
        .name(format!("hunt-{}", e.id))
        .spawn(move || {
            if let Err(err) = rounds(&app, &e) {
                let msg = format!("{err:#}");
                log::warn!("{}: hunt rounds stopped: {msg}", e.name);
                note(&e.root, format!("Paused: {msg}"));
                edit_side(&e.root, |s| {
                    s.auto = false;
                    s.paused = Some(msg);
                });
            }
            edit_side(&e.root, |s| s.round_started = None);
            set_put(&RUNNING, &e.id, false);
            set_put(&PAUSE, &e.id, false);
            app.refresh.notify_one();
        })
        .ok();
}

fn rounds(app: &Arc<App>, e: &Arc<Entry>) -> anyhow::Result<()> {
    let paused = || set_has(&PAUSE, &e.id) || !hunting(&e.root);
    // Rounds played again after learning what a mod needs.
    let mut relearned = 0;
    loop {
        if paused() {
            return Ok(());
        }
        let Some(mut h) = bisect::load(&e.root) else { return Ok(()) };
        if h.done() {
            return Ok(());
        }
        stop_server(e);
        let mut inst = crate::mods_api::instance_of(e)?;
        bisect::apply(&mut inst, &h)?;
        let r = h.round();
        note(&e.root, format!("Round {r}: {} mods off, starting", h.parked.len()));
        edit_side(&e.root, |s| s.round_started = Some(now_secs()));
        app.servers.act(e, PanelAction::Start).map_err(|err| anyhow::anyhow!("the server didn't start: {err:#}"))?;
        app.refresh.notify_one();
        let t0 = Instant::now();
        let a = loop {
            std::thread::sleep(Duration::from_secs(2));
            if paused() {
                note(&e.root, format!("Round {r}: paused, the server keeps running"));
                return Ok(());
            }
            let took = took(t0.elapsed().as_secs());
            match e.server.state() {
                ServerState::Running => {
                    note(&e.root, format!("Round {r}: started fine in {took}"));
                    break Answer::Works;
                }
                ServerState::Crashed(code) => {
                    let code = code.map_or(String::new(), |c| format!(" (exit code {c})"));
                    note(&e.root, format!("Round {r}: crashed after {took}{code}"));
                    break Answer::Broken;
                }
                ServerState::Stopped => {
                    note(&e.root, format!("Round {r}: stopped by itself after {took}"));
                    break Answer::Broken;
                }
                _ if t0.elapsed() > ROUND_TIMEOUT => {
                    note(&e.root, format!("Round {r}: not started after {took}, counting it as broken"));
                    break Answer::Broken;
                }
                _ => {}
            }
        };
        let secs = t0.elapsed().as_secs();
        let console = e.server.console();
        stop_server(e);
        // Read again: nothing else may change it while rounds run, but a
        // stop may have ended the hunt meanwhile.
        if paused() {
            return Ok(());
        }
        h = bisect::load(&e.root).ok_or_else(|| anyhow::anyhow!("the hunt's journal is gone"))?;
        if a == Answer::Broken
            && relearned < 40
            && let Some(msg) = learn_from_log(e, &mut h, &console)
        {
            relearned += 1;
            bisect::save(&e.root, &h)?;
            note(&e.root, msg);
            continue;
        }
        h.answer(a);
        bisect::save(&e.root, &h)?;
        edit_side(&e.root, |s| {
            s.last_round_secs = Some(secs);
            s.round_started = None;
        });
        match h.phase {
            Phase::Found => {
                let mut inst = crate::mods_api::instance_of(e)?;
                bisect::apply(&mut inst, &h)?;
                note(&e.root, format!("Found it after {} rounds: {}", h.rounds.len(), mod_names(&inst, &h.culprits).join(", ")));
                return Ok(());
            }
            Phase::NotFound => {
                note(&e.root, "Still broken with every suspect off: no single mod breaks it".into());
                return Ok(());
            }
            _ => {}
        }
        app.refresh.notify_one();
    }
}

/// On start: rounds that were running when hopperd stopped show as paused.
pub fn resume_note(app: &App) {
    for e in app.servers.list() {
        if hunting(&e.root) {
            let s = load_side(&e.root);
            if s.auto {
                edit_side(&e.root, |s| {
                    s.auto = false;
                    s.paused = Some("Hopper Server restarted".into());
                    s.round_started = None;
                });
            }
        }
    }
}
