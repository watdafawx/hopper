//! Modpack updates for a server, run here next to the server's files as
//! jobs: `/servers/{id}/pack/plan`, `/servers/{id}/pack/apply`, then
//! `/jobs/{job}` for progress and the result.
//!
//! Two kinds of callers:
//! - Hopper desktop sends the instance and the pack file itself, and gets
//!   the whole [`ServerPlan`] back to review and send to `apply`.
//! - The web panel asks `/servers/{id}/pack/check` which pack the server
//!   runs and which versions are newer, then plans with just a file id.
//!   The plan stays here; the browser gets a summary, picks the files
//!   both sides changed, and applies it by the plan job's id.
//!
//! Which pack a server runs comes from the server pack record Hopper
//! writes when it builds or updates a server from one, else from the
//! [`LINK`] written when the panel makes a server from a pack without
//! one, else from the server's `hopper.json`.

use crate::api::{ApiError, ApiResult, bad, not_found, require};
use crate::servers::Entry;
use crate::App;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use hopper_core::cf;
use hopper_core::instances::{self, Instance, PackRef};
use hopper_core::server_pack_update::{self as spu, ServerPlan};
use hopper_core::{modrinth, source};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::Path as FsPath;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/servers/{id}/pack/check", get(check))
        .route("/api/v1/servers/{id}/pack/changelog", get(changelog))
        .route("/api/v1/servers/{id}/pack/plan", post(plan))
        .route("/api/v1/servers/{id}/pack/diff", get(diff))
        .route("/api/v1/servers/{id}/pack/apply", post(apply))
        .route("/api/v1/pack-updates", get(all_updates))
        .route("/api/v1/jobs/{job}", get(job))
        .route("/api/v1/jobs/{job}/cancel", post(cancel))
}

/// Uses the desktop's CurseForge key when this panel has none.
fn ensure_cf_key(key: &str) {
    if !key.is_empty() && cf::api_key().is_none() {
        cf::set_session_key(key);
    }
}

/// Runs `work` as a job; its result (as JSON) is kept for `/jobs/{id}`.
pub(crate) fn spawn<T: serde::Serialize>(
    app: &Arc<App>,
    title: String,
    work: impl FnOnce(&hopper_core::jobs::Job) -> anyhow::Result<T> + Send + 'static,
) -> u64 {
    let job = app.jobs.start(title);
    let id = job.id();
    let a = app.clone();
    std::thread::spawn(move || {
        let r = work(&job);
        if let Ok(v) = &r {
            a.results.lock().unwrap().insert(id, serde_json::to_value(v).unwrap_or(Value::Null));
        }
        if let Err(e) = &r {
            log::warn!("job {id}: {e:#}");
        }
        job.finish(&r);
    });
    id
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(bad)?.map_err(|e| bad(format!("{e:#}")))
}

fn entry(app: &App, headers: &HeaderMap, method: &Method, id: &str) -> ApiResult<Arc<Entry>> {
    require(app, headers, method)?;
    app.servers.get(id).ok_or_else(|| not_found("server"))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// ---- which pack a server runs ----------------------------------------------------------------

/// In the server folder: the pack a server was made from, when it has no
/// server pack record (a Modrinth pack, or a pack built from its client
/// files).
pub const LINK: &str = ".hopper-pack.json";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PackLink {
    /// "curseforge" or "modrinth".
    pub source: String,
    /// CurseForge's project id, or Modrinth's (its string id).
    pub project: String,
    /// CurseForge's file id, or Modrinth's version id.
    pub file: String,
}

impl PackLink {
    fn mr(&self) -> bool {
        self.source == "modrinth"
    }

    /// Hopper's numeric ids (Modrinth's mapped, see [`modrinth::num_id`]).
    fn ids(&self) -> anyhow::Result<(u64, u64)> {
        if self.mr() {
            return Ok((modrinth::num_id(&self.project), modrinth::num_id(&self.file)));
        }
        let p = self.project.parse().map_err(|_| anyhow::anyhow!("bad pack id {}", self.project))?;
        let f = self.file.parse().map_err(|_| anyhow::anyhow!("bad pack file id {}", self.file))?;
        Ok((p, f))
    }
}

fn read_link(root: &FsPath) -> Option<PackLink> {
    let l: PackLink = serde_json::from_slice(&std::fs::read(root.join(LINK)).ok()?).ok()?;
    (!l.project.is_empty() && !l.file.is_empty()).then_some(l)
}

/// Records the pack a server the panel just made came from (version
/// `file` of `project`, Hopper's ids), unless it has a server pack record
/// already (that one is kept up to date by pack updates).
pub(crate) fn link_new_server(root: &FsPath, mr: bool, project: u64, file: u64) {
    if spu::recorded(root).is_some_and(|m| m.project_id != 0) {
        return;
    }
    let link = if mr {
        match (modrinth::real_id(project), modrinth::real_id(file)) {
            (Ok(p), Ok(f)) => PackLink { source: "modrinth".into(), project: p, file: f },
            _ => {
                log::warn!("{}: couldn't record its Modrinth pack", root.display());
                return;
            }
        }
    } else {
        PackLink { source: "curseforge".into(), project: project.to_string(), file: file.to_string() }
    };
    write_link(root, &link);
}

fn write_link(root: &FsPath, link: &PackLink) {
    if let Err(e) = serde_json::to_vec_pretty(link).map_err(anyhow::Error::from).and_then(|b| Ok(std::fs::write(root.join(LINK), b)?)) {
        log::warn!("recording {}'s pack: {e:#}", root.display());
    }
}

/// The pack `root` runs, newest record first: the server pack record
/// (updated by every server pack update), the panel's link, then the
/// pack in its hopper.json (made by the Mods tab, or by Hopper desktop).
fn pack_of(root: &FsPath) -> Option<PackLink> {
    if let Some(m) = spu::recorded(root).filter(|m| m.project_id != 0 && m.pack_file_id != 0 && !modrinth::is_mr(m.project_id)) {
        return Some(PackLink { source: "curseforge".into(), project: m.project_id.to_string(), file: m.pack_file_id.to_string() });
    }
    if let Some(l) = read_link(root) {
        return Some(l);
    }
    if !root.join(instances::HOPPER_FILE).is_file() {
        return None;
    }
    let p = instances::load(root)?.pack.filter(|p| p.project_id != 0 && p.file_id != 0)?;
    if modrinth::is_mr(p.project_id) {
        return (!p.mr_project.is_empty() && !p.mr_version.is_empty())
            .then(|| PackLink { source: "modrinth".into(), project: p.mr_project.clone(), file: p.mr_version.clone() });
    }
    Some(PackLink { source: "curseforge".into(), project: p.project_id.to_string(), file: p.file_id.to_string() })
}

// ---- checking for newer versions -------------------------------------------------------------

/// Each server's last check: (when, the pack file it was for, the answer).
static CHECKS: Mutex<Option<HashMap<String, (u64, String, Value)>>> = Mutex::new(None);
const CHECK_FOR: u64 = 6 * 3600;
/// A failed check is tried again sooner.
const FAILED_FOR: u64 = 1800;
/// A check of every server is running in the background.
static CHECKING_ALL: AtomicBool = AtomicBool::new(false);

fn cached(id: &str, link: &PackLink) -> Option<Value> {
    let g = CHECKS.lock().unwrap();
    let (at, file, v) = g.as_ref()?.get(id)?;
    let ttl = if v.get("error").is_some() { FAILED_FOR } else { CHECK_FOR };
    (file == &link.file && now_secs().saturating_sub(*at) < ttl).then(|| v.clone())
}

fn keep(id: &str, link: &PackLink, v: &Value) {
    CHECKS.lock().unwrap().get_or_insert_with(HashMap::new).insert(id.to_string(), (now_secs(), link.file.clone(), v.clone()));
}

fn forget(id: &str) {
    if let Some(m) = CHECKS.lock().unwrap().as_mut() {
        m.remove(id);
    }
}

/// Minecraft from a file's game versions ("1.21.1" among "NeoForge", ...).
fn mc_of(game_versions: &[String]) -> String {
    game_versions
        .iter()
        .find(|g| g.starts_with(|c: char| c.is_ascii_digit()) && g.contains('.'))
        .cloned()
        .unwrap_or_default()
}

fn version_json(f: &cf::File, mr: bool) -> Value {
    json!({
        "id": f.id.to_string(), "name": f.display_name, "file_name": f.file_name, "date": f.file_date,
        "type": f.release_name().to_lowercase(), "mc_version": mc_of(&f.game_versions),
        // Modrinth packs publish no server packs Hopper can update from.
        "server_files": !mr && f.server_pack_file_id.is_some_and(|i| i != 0),
    })
}

/// Looks the pack up: its name, the installed version and the newer ones
/// (newest first).
fn look_up(link: &PackLink) -> anyhow::Result<Value> {
    let mr = link.mr();
    if !mr && cf::api_key().is_none() {
        anyhow::bail!("CurseForge needs an API key to check for pack updates: add one in Access & devices.");
    }
    let (pid, fid) = link.ids()?;
    let m = source::get_mod(pid)?;
    let files: Vec<cf::File> = source::files(pid, "", cf::Loader::Any)?
        .into_iter()
        .filter(|f| f.is_server_pack != Some(true))
        .collect();
    let current = match files.iter().find(|f| f.id == fid) {
        Some(f) => f.clone(),
        None => source::file(pid, fid)?,
    };
    let mut newer: Vec<&cf::File> = files.iter().filter(|f| f.id != fid && f.file_date > current.file_date).collect();
    newer.sort_by(|a, b| b.file_date.cmp(&a.file_date));
    // Every version, for the Versions window (switching to an older one too).
    let mut all: Vec<&cf::File> = files.iter().collect();
    if !all.iter().any(|f| f.id == fid) {
        all.push(&current);
    }
    all.sort_by(|a, b| b.file_date.cmp(&a.file_date));
    let page = if !m.links.website_url.is_empty() {
        m.links.website_url.clone()
    } else if mr {
        format!("https://modrinth.com/modpack/{}", if m.slug.is_empty() { &link.project } else { &m.slug })
    } else {
        String::new()
    };
    Ok(json!({
        "pack": {
            "name": m.name, "icon": m.logo.as_ref().map(|l| l.thumbnail_url.clone()).unwrap_or_default(),
            "source": link.source, "project": link.project, "page": page,
            "current": version_json(&current, mr),
            // A server pack update needs CurseForge's server packs.
            "updatable": !mr,
        },
        "newer": newer.iter().map(|f| version_json(f, mr)).collect::<Vec<_>>(),
        "versions": all.iter().map(|f| version_json(f, mr)).collect::<Vec<_>>(),
        "checked": now_secs(),
    }))
}

/// One server's check, cached for a few hours (`force` looks again).
fn check_server(e: &Entry, force: bool) -> anyhow::Result<Value> {
    let Some(link) = pack_of(&e.root) else {
        return Ok(json!({ "pack": Value::Null, "newer": [] }));
    };
    if !force && let Some(v) = cached(&e.id, &link) {
        return match v.get("error").and_then(Value::as_str) {
            Some(err) => Err(anyhow::anyhow!("{err}")),
            None => Ok(v),
        };
    }
    match look_up(&link) {
        Ok(v) => {
            keep(&e.id, &link, &v);
            Ok(v)
        }
        Err(err) => {
            log::info!("{}: pack update check: {err:#}", e.name);
            keep(&e.id, &link, &json!({ "error": format!("{err:#}") }));
            Err(err)
        }
    }
}

async fn check(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let force = q.get("force").is_some_and(|f| f == "1");
    let mut v = blocking(move || check_server(&e, force)).await?;
    v["running"] = json!(app.servers.get(&id).is_some_and(|e| e.server.state().is_alive()));
    Ok(Json(v))
}

/// Servers with a newer pack version, from the cached checks: `{ id: {
/// from, to, count } }`. Servers whose check is old are checked in the
/// background, for the next call.
async fn all_updates(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    let who = require(&app, &headers, &method)?;
    let mut out = serde_json::Map::new();
    let mut stale = Vec::new();
    // Only the servers this one may see (a scoped token or user).
    for e in app.servers.list().into_iter().filter(|e| who.sees(&e.id)) {
        let Some(link) = pack_of(&e.root) else { continue };
        match cached(&e.id, &link) {
            Some(v) => {
                if let Some(n) = v["newer"].as_array().filter(|n| !n.is_empty()) {
                    out.insert(e.id.clone(), json!({ "from": v["pack"]["current"]["name"], "to": n[0]["name"], "count": n.len() }));
                }
            }
            None => stale.push(e),
        }
    }
    if !stale.is_empty() && !CHECKING_ALL.swap(true, Ordering::SeqCst) {
        std::thread::spawn(move || {
            for e in stale {
                let _ = check_server(&e, false);
            }
            CHECKING_ALL.store(false, Ordering::SeqCst);
        });
    }
    Ok(Json(json!({ "servers": out })))
}

// ---- planning ---------------------------------------------------------------------------------

/// Plans the web panel made, by job id: (server id, plan). One per server.
static PLANS: Mutex<Option<HashMap<u64, (String, ServerPlan)>>> = Mutex::new(None);

fn take_plan(job: u64, server: &str) -> Option<ServerPlan> {
    let mut g = PLANS.lock().unwrap();
    let m = g.as_mut()?;
    (m.get(&job)?.0 == server).then(|| m.remove(&job).map(|(_, p)| p))?
}

fn with_plan<T>(job: u64, server: &str, f: impl FnOnce(&ServerPlan) -> T) -> Option<T> {
    let g = PLANS.lock().unwrap();
    let (s, p) = g.as_ref()?.get(&job)?;
    (s == server).then(|| f(p))
}

#[derive(Deserialize)]
struct PlanBody {
    /// Desktop: the instance the server runs and the pack file.
    #[serde(default)]
    inst: Option<Instance>,
    #[serde(default)]
    pack: Option<cf::File>,
    /// Web: the pack file id to update to (the server's own pack).
    #[serde(default)]
    file: Option<String>,
    /// Compare with the server's changes. Desktop: old desktops don't send
    /// it, and always compared. Web: off unless asked (a quick update, as
    /// desktop does by default).
    #[serde(default)]
    compare: Option<bool>,
    #[serde(default)]
    cf_key: String,
    /// The desktop's "keep downloaded packs" days, for this panel's cache.
    #[serde(default)]
    keep_days: Option<u32>,
}

/// Days a downloaded pack stays here when the desktop doesn't say.
const KEEP_DAYS: u32 = 30;

async fn plan(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<PlanBody>,
) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    ensure_cf_key(&b.cf_key);
    let keep_days = b.keep_days.unwrap_or(KEEP_DAYS);
    let web_compare = b.compare.unwrap_or(false);
    let compare = b.compare.unwrap_or(true);
    if let (Some(inst), Some(pack)) = (b.inst, b.pack) {
        let title = format!("Comparing {} with {}", e.name, pack.display_name);
        let job = spawn(&app, title, move |job| {
            let server = hopper_core::server::load(&e.root);
            let r = spu::plan(&server, &inst, &pack, compare, job);
            hopper_core::modpack::prune_packs_older_than(keep_days);
            r
        });
        return Ok(Json(json!({ "job": job })));
    }
    let file: u64 = b.file.as_deref().map(str::trim).and_then(|f| f.parse().ok()).ok_or_else(|| bad("Which pack version?"))?;
    let link = pack_of(&e.root).ok_or_else(|| bad(format!("{} isn't linked to a modpack.", e.name)))?;
    if link.mr() {
        return Err(bad("Modrinth packs don't publish server packs, so Hopper can't update this server in place."));
    }
    if cf::api_key().is_none() {
        return Err(bad("CurseForge needs an API key: add one in Access & devices."));
    }
    let title = format!("Comparing {} with its pack's update", e.name);
    let job = spawn(&app, title, move |job| {
        let (pid, cur) = link.ids()?;
        job.detail("Looking the pack up");
        let pack = cf::file(pid, file)?;
        anyhow::ensure!(pack.mod_id == pid, "That file isn't from this server's pack.");
        anyhow::ensure!(
            pack.server_pack_file_id.is_some_and(|i| i != 0),
            "The author of {} didn't publish a server pack for it, so Hopper can't update the server from it.",
            pack.display_name
        );
        let mut inst = crate::mods_api::instance_of(&e)?;
        // The pack the server runs, as recorded (hopper.json may be older).
        let marker = spu::recorded(&e.root).filter(|m| m.project_id == pid && m.server_pack_file_id != 0);
        let server_pack = match marker {
            Some(m) => m.server_pack_file_id,
            None => cf::file(pid, cur).ok().and_then(|f| f.server_pack_file_id).unwrap_or(0),
        };
        let old = inst.pack.take().unwrap_or_default();
        inst.pack = Some(PackRef { project_id: pid, file_id: cur, server_pack_file_id: server_pack, ..old });
        let server = hopper_core::server::load(&e.root);
        let p = spu::plan(&server, &inst, &pack, web_compare, job)?;
        hopper_core::modpack::prune_packs_older_than(keep_days);
        let out = summary(job.id(), &p);
        let mut g = PLANS.lock().unwrap();
        let m = g.get_or_insert_with(HashMap::new);
        m.retain(|_, (s, _)| s != &e.id);
        m.insert(job.id(), (e.id.clone(), p));
        Ok(out)
    });
    Ok(Json(json!({ "job": job })))
}

/// What the web panel shows of a plan.
fn summary(plan_id: u64, p: &ServerPlan) -> Value {
    json!({
        "plan": plan_id.to_string(),
        "target": { "id": p.pack.id.to_string(), "name": p.pack.display_name },
        "server_pack": p.plan.file.file_name,
        "base": p.base, "base_assumed": p.base_assumed, "base_known": p.plan.base_known,
        "loader": p.loader.as_ref().map(|(from, to)| json!({ "from": from, "to": to })),
        "mods_note": p.mods_note,
        "kept": p.kept,
        "compared": p.plan.compared,
        "mods": p.plan.mods.iter().map(mod_json).collect::<Vec<_>>(),
        // Mods on the server's own versions: kept unless the update resets them.
        "kept_versions": p.kept_versions.iter().map(mod_json).collect::<Vec<_>>(),
        "your_mods": p.plan.your_mods.iter().map(mod_json).collect::<Vec<_>>(),
        "your_files": p.plan.your_files,
        "mod_made": p.plan.mod_made,
        "needed": p.plan.needed.iter().map(|n| json!({
            "file": n.file, "name": n.name, "project": n.project.to_string(), "icon": n.icon,
            "by": n.by, "by_project": n.by_project.to_string(), "by_icon": n.by_icon,
        })).collect::<Vec<_>>(),
        "files": p.plan.files,
        "take": p.plan.default_take(),
    })
}

fn mod_json(m: &hopper_core::packdiff::ModChange) -> Value {
    json!({
        "name": m.name, "from": m.from, "to": m.to, "action": m.action, "icon": m.icon,
        "project": m.project.to_string(),
    })
}

/// The changelogs of the versions an update to `file` brings (newest
/// first; the installed version's alone when it brings none), and the
/// pack author's advice on updating picked out of them.
async fn changelog(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let file: Option<u64> = q.get("file").and_then(|f| f.trim().parse().ok());
    let v = blocking(move || {
        let link = pack_of(&e.root).ok_or_else(|| anyhow::anyhow!("{} isn't linked to a modpack.", e.name))?;
        let (pid, cur) = link.ids()?;
        let inst = crate::mods_api::instance_of(&e)?;
        let update = match file {
            Some(f) if f != cur => Some(source::file(pid, f)?),
            _ => None,
        };
        let (entries, _) = hopper_core::install::changelogs(&inst, pid, cur, update.as_ref(), true)?;
        let new: Vec<_> = entries.iter().filter(|e| !e.installed).collect();
        let shown: Vec<_> = if new.is_empty() { entries.iter().collect() } else { new };
        let md: Vec<String> = shown.iter().map(|e| hopper_core::richtext::to_markdown(&e.blocks)).collect();
        let advice = hopper_core::richtext::advice(md.iter().map(String::as_str));
        Ok(json!({
            "advice": advice,
            "entries": shown.iter().map(|e| json!({
                "name": e.name, "date": e.date, "release": e.release,
                "blocks": crate::browse_api::blocks_json(&e.blocks),
            })).collect::<Vec<_>>(),
        }))
    })
    .await?;
    Ok(Json(v))
}

/// A file both sides changed: a diff from the server's copy to the pack's.
async fn diff(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let plan: u64 = q.get("plan").and_then(|p| p.parse().ok()).ok_or_else(|| bad("Which plan?"))?;
    let rel = q.get("rel").cloned().ok_or_else(|| bad("Which file?"))?;
    let p = with_plan(plan, &id, |p| p.plan.clone()).ok_or_else(|| bad("That comparison is gone: compare again."))?;
    if !p.files.iter().any(|f| f.rel == rel) {
        return Err(bad("That file isn't in the update."));
    }
    let root = e.root.clone();
    let d = blocking(move || p.diff(&root, &rel)).await?;
    let d = d.map(|mut s| {
        if s.len() > 200_000 {
            let mut cut = 200_000;
            while !s.is_char_boundary(cut) {
                cut -= 1;
            }
            s.truncate(cut);
            s.push_str("\n… (cut short)");
        }
        s
    });
    Ok(Json(json!({ "diff": d })))
}

// ---- applying ---------------------------------------------------------------------------------

#[derive(Deserialize)]
struct ApplyBody {
    /// Desktop: the whole plan.
    #[serde(default)]
    plan: Option<ServerPlan>,
    /// Web: the plan job's id.
    #[serde(default)]
    plan_id: Option<String>,
    /// Conflicting files where the pack's copy wins.
    #[serde(default)]
    take: HashSet<String>,
    /// Web: dropped mods one of the server's own mods needs, kept (their
    /// files).
    #[serde(default)]
    keep: Vec<String>,
    /// Web: mods on the server's own versions go to the pack's too.
    #[serde(default)]
    reset_versions: bool,
    #[serde(default)]
    cf_key: String,
    #[serde(default)]
    keep_days: Option<u32>,
}

async fn apply(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<ApplyBody>,
) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    if e.server.state().is_alive() {
        return Err(ApiError(StatusCode::CONFLICT, format!("{} is running. Stop it first.", e.name)));
    }
    ensure_cf_key(&b.cf_key);
    let web = b.plan.is_none();
    let mut plan = match (b.plan, b.plan_id.as_deref().and_then(|p| p.trim().parse::<u64>().ok())) {
        (Some(p), _) => p,
        (None, Some(job)) => take_plan(job, &id).ok_or_else(|| bad("That comparison is gone: compare again."))?,
        (None, None) => return Err(bad("Which update?")),
    };
    // Desktop resets in the plan it sends; the web panel asks here.
    if b.reset_versions {
        plan.reset_versions();
    }
    if !b.keep.is_empty() {
        let keep: HashSet<String> = b.keep.iter().map(|k| k.to_lowercase()).collect();
        plan.plan
            .mods
            .retain(|m| !(m.action == hopper_core::packdiff::ModAction::Remove && keep.contains(&m.from.to_lowercase())));
    }
    log::info!("{}: updating from {}", e.name, plan.plan.file.display_name);
    let title = format!("{} → {}", e.name, plan.pack.display_name);
    let keep_days = b.keep_days.unwrap_or(KEEP_DAYS);
    let take = b.take;
    let job = spawn(&app, title, move |job| {
        let server = hopper_core::server::load(&e.root);
        // NeoForge's installer runs with the server's own Java.
        let java = server.java.clone().map(std::path::PathBuf::from).unwrap_or_else(|| "java".into());
        let r = spu::apply(&server, &plan, &take, "127.0.0.1", &java, None, job);
        hopper_core::modpack::prune_packs_older_than(keep_days);
        let r = r?;
        after_update(&e, &plan);
        if !web {
            return Ok(serde_json::to_value(&r)?);
        }
        let backups = r.backups.as_ref().map(|b| b.strip_prefix(&e.root).unwrap_or(b).to_string_lossy().replace('\\', "/"));
        Ok(json!({
            "summary": r.summary(), "mods": r.mods, "files": r.files, "removed": r.removed,
            "loader": r.loader.as_ref().map(|(from, to)| json!({ "from": from, "to": to })),
            "backups": backups, "version": plan.pack.display_name,
        }))
    });
    Ok(Json(json!({ "job": job })))
}

/// After an update: the server's hopper.json and pack link name the new
/// version, and its checks start over.
fn after_update(e: &Entry, p: &ServerPlan) {
    if e.root.join(instances::HOPPER_FILE).is_file()
        && let Some(mut inst) = instances::load(&e.root)
    {
        let old = inst.pack.take().unwrap_or_default();
        inst.pack = Some(PackRef {
            project_id: p.pack.mod_id,
            file_id: p.pack.id,
            server_pack_file_id: p.plan.file.id,
            version: p.pack.display_name.clone(),
            file_name: p.pack.file_name.clone(),
            ..old
        });
        if let Err(err) = inst.save() {
            log::warn!("{}: saving hopper.json after the pack update: {err:#}", e.name);
        }
    }
    if let Some(mut l) = read_link(&e.root).filter(|l| !l.mr()) {
        l.file = p.pack.id.to_string();
        write_link(&e.root, &l);
    }
    forget(&e.id);
    crate::mods_api::forget_updates(&e.id);
}

// ---- jobs -------------------------------------------------------------------------------------

async fn job(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(job): Path<u64>,
) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let st = app.jobs.snapshot().into_iter().find(|j| j.id == job).ok_or_else(|| not_found("job"))?;
    let result = if st.done { app.results.lock().unwrap().remove(&job) } else { None };
    if st.done {
        app.jobs.dismiss(job);
    }
    Ok(Json(json!({
        "id": st.id, "title": st.title, "detail": st.detail, "progress": st.progress,
        "done": st.done, "error": st.error, "notes": st.notes, "result": result,
    })))
}

async fn cancel(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(job): Path<u64>,
) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    app.jobs.cancel(job);
    // A cancelled comparison's plan isn't wanted.
    if let Some(m) = PLANS.lock().unwrap().as_mut() {
        m.remove(&job);
    }
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_from_link_and_record() {
        let dir = std::env::temp_dir().join(format!("hopperd-packlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(pack_of(&dir).is_none());
        link_new_server(&dir, false, 925200, 5000001);
        let l = pack_of(&dir).unwrap();
        assert_eq!((l.source.as_str(), l.ids().unwrap()), ("curseforge", (925200, 5000001)));
        // A server pack record wins: it follows updates.
        spu::record(&dir, &spu::Marker { project_id: 925200, pack_file_id: 5000002, server_pack_file_id: 5000003, server_pack: "s.zip".into() }).unwrap();
        assert_eq!(pack_of(&dir).unwrap().ids().unwrap(), (925200, 5000002));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
