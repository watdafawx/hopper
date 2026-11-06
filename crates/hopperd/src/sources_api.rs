//! Mods from GitHub and other links on a server, as on desktop: look a
//! link up, add the file it gives (or make an installed mod follow it),
//! stop following. The work is hopper-core's `sources`; updates come
//! through the normal mod update check. Off until the owner turns it on.

use crate::api::{ApiResult, bad, require, require_owner};
use crate::mods_api::{entry, forget_updates, instance};
use crate::App;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post};
use axum::{Json, Router};
use hopper_core::install;
use hopper_core::sources::{self, Resolved};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/settings/sources", get(settings).put(set_settings))
        .route("/api/v1/servers/{id}/mods/source/resolve", post(resolve))
        .route("/api/v1/servers/{id}/mods/source/add", post(add))
        .route("/api/v1/servers/{id}/mods/source/unfollow", post(unfollow))
        .route("/api/v1/servers/{id}/mods/source/suggest", get(suggest))
        .route("/api/v1/servers/{id}/mods/source/plan", post(plan))
        .route("/api/v1/servers/{id}/mods/source/adopt", post(adopt))
        .route("/api/v1/servers/{id}/mods/source/unknown", get(unknown))
        .route("/api/v1/servers/{id}/mods/source/adopt-many", post(adopt_many))
        .route("/api/v1/servers/{id}/mods/source/trust", post(trust))
        .route("/api/v1/servers/{id}/mods/sources", axum::routing::put(put_pins))
        .route("/api/v1/servers/{id}/mods/source/page", get(page))
        .route("/api/v1/servers/{id}/packs/source/resolve", post(pack_resolve))
        .route("/api/v1/servers/{id}/packs/source/add", post(pack_add))
}

/// The peek's page for a mod from GitHub or a link: the README (or what's
/// known about a Maven artifact, Jenkins job or link) and the header's
/// facts. Never asks CurseForge or Modrinth.
async fn page(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<FileQuery>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let v = run(move || {
        let inst = instance(&e)?;
        let a = inst
            .addon_by_file(&q.file)
            .filter(|a| a.source.is_some())
            .ok_or_else(|| anyhow::anyhow!("{} doesn't come from GitHub or a link", q.file))?;
        let blocks = hopper_core::source::description(a.project_id)?;
        let o = sources::page::origin(a.project_id).unwrap_or_default();
        Ok(json!({
            "kind": o.kind.label(), "name": o.name, "page": o.page, "open_label": o.open_label(),
            "stars": o.stars, "license": o.license, "released": o.released, "summary": o.summary,
            "description": crate::browse_api::blocks_json(&blocks),
        }))
    })
    .await?;
    Ok(Json(v))
}

fn allowed(app: &App) -> ApiResult<()> {
    if app.cfg.read().unwrap().allow_sources {
        Ok(())
    } else {
        Err(bad("Mods from GitHub and other links are off. Turn them on in Settings › Mod sources first."))
    }
}

/// Whether mods from links are on, and whether a GitHub token is saved
/// (never the token itself).
async fn settings(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let cfg = app.cfg.read().unwrap();
    Ok(Json(json!({ "allowed": cfg.allow_sources, "github_token": !cfg.github_token.trim().is_empty() })))
}

#[derive(Deserialize)]
struct SettingsBody {
    allowed: Option<bool>,
    /// "" removes it.
    github_token: Option<String>,
}

async fn set_settings(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Json(b): Json<SettingsBody>,
) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let mut cfg = app.cfg.write().unwrap();
    if let Some(on) = b.allowed {
        cfg.allow_sources = on;
        sources::set_enabled(on);
    }
    if let Some(t) = &b.github_token {
        cfg.github_token = t.trim().to_string();
        sources::http::set_session_github_token(&cfg.github_token);
    }
    cfg.save(&app.data_dir).map_err(|e| bad(format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ResolveBody {
    link: String,
    #[serde(default)]
    prerelease: bool,
}

fn resolved_json(r: &Resolved) -> Value {
    json!({
        "name": r.name,
        "kind": r.source.kind.label(),
        "publisher": r.source.publisher,
        "page": r.source.page(),
        "trusted": sources::trusted(&r.source),
        // The whole source, for Hopper desktop's dialog (branch folders, ids).
        "source": r.source,
        "pick": r.pick,
        "ambiguous": r.ambiguous,
        "candidates": r.candidates.iter().map(|c| json!({
            "file": c.file_name,
            "version": c.version,
            "title": c.title,
            "size": c.size,
            "date": c.date,
            "author": c.author,
            "prerelease": c.prerelease,
            "fits": c.fit.usable(),
            "tag": c.fit.tag(),
            "good": c.fit == sources::Fit::Good,
            "fit": c.fit,
            "page": c.page,
        })).collect::<Vec<_>>(),
    })
}

async fn resolve(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<ResolveBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    allowed(&app)?;
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let inst = instance(&e)?;
        let r = sources::resolve(&b.link, &inst.game_version, install::loader_of(&inst), b.prerelease)?;
        Ok(resolved_json(&r))
    })
    .await
    .map_err(bad)?
    .map_err(|e| bad(format!("{e:#}")))?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct AddBody {
    link: String,
    #[serde(default)]
    prerelease: bool,
    /// The file picked from the lookup.
    file: String,
    /// An installed mod (its file name) to follow the source instead of
    /// adding a new one.
    #[serde(default)]
    follow: Option<String>,
}

async fn add(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<AddBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    allowed(&app)?;
    forget_updates(&id);
    let title = match &b.follow {
        Some(f) => format!("Following a source for {f} on {}", e.name),
        None => format!("Adding {} to {}", b.file, e.name),
    };
    let job = crate::pack_api::spawn(&app, title, move |job| {
        let mut inst = instance(&e)?;
        // Looked up again: the file list may have moved on since the
        // browser asked.
        let r = sources::resolve(&b.link, &inst.game_version, install::loader_of(&inst), b.prerelease)?;
        let i = r
            .candidates
            .iter()
            .position(|c| c.file_name == b.file)
            .ok_or_else(|| anyhow::anyhow!("{} is no longer offered there; look the link up again", b.file))?;
        sources::trust(&r.source)?;
        // A replaced jar goes to the server's .hopper-backups first.
        let done = match &b.follow {
            Some(f) => sources::follow(&mut inst, f, &r, i, job)?,
            None => sources::install(&mut inst, &r, i, job)?,
        };
        log::info!("{}: {done} from {}", e.name, r.source.publisher);
        Ok(json!({ "added": [done] }))
    });
    Ok(Json(json!({ "job": job })))
}

#[derive(Deserialize)]
struct UnfollowBody {
    file: String,
}

async fn unfollow(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<UnfollowBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    forget_updates(&id);
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut inst = instance(&e)?;
        sources::unfollow(&mut inst, &b.file)
    })
    .await
    .map_err(bad)?
    .map_err(|e| bad(format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true })))
}

async fn run<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(bad)?.map_err(|e| bad(format!("{e:#}")))
}

#[derive(Deserialize)]
struct FileQuery {
    file: String,
}

/// The source a jar's own metadata points to, to pre-fill Follow a source.
async fn suggest(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<FileQuery>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let link = run(move || Ok(sources::suggest(&instance(&e)?, &q.file))).await?;
    Ok(Json(json!({ "link": link })))
}

#[derive(Deserialize)]
struct AdoptBody {
    file: String,
    link: String,
    #[serde(default)]
    prerelease: bool,
}

fn adoption_json(a: &sources::Adoption) -> Value {
    json!({
        "name": a.name,
        "kind": a.source.kind.label(),
        "publisher": a.source.publisher,
        "trusted": sources::trusted(&a.source),
        "matched": a.matched,
        "seen": a.source.seen,
        "newest": a.newest.as_ref().map(|c| json!({ "file": c.file_name, "version": c.version, "title": c.title })),
    })
}

/// How an installed jar would follow a source (nothing changes yet).
async fn plan(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<AdoptBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    allowed(&app)?;
    let v = run(move || Ok(adoption_json(&sources::plan_adopt(&instance(&e)?, &b.file, &b.link, b.prerelease)?))).await?;
    Ok(Json(v))
}

/// Makes the installed jar follow the source, leaving the file as it is.
async fn adopt(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<AdoptBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    allowed(&app)?;
    forget_updates(&id);
    let v = run(move || {
        let mut inst = instance(&e)?;
        let a = sources::plan_adopt(&inst, &b.file, &b.link, b.prerelease)?;
        sources::trust(&a.source)?;
        sources::adopt(&mut inst, &b.file, &a)?;
        Ok(adoption_json(&a))
    })
    .await?;
    Ok(Json(v))
}

/// Unknown mods and the source each jar suggests, for "Find sources".
async fn unknown(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let list = run(move || Ok(sources::unknown_mods(&instance(&e)?))).await?;
    Ok(Json(json!({ "mods": list })))
}

#[derive(Deserialize)]
struct ManyItem {
    file: String,
    link: String,
}

#[derive(Deserialize)]
struct ManyBody {
    items: Vec<ManyItem>,
    #[serde(default)]
    prerelease: bool,
}

/// Follows sources for several unknown mods in one go. Each source is
/// asked once (cached), so a batch stays inside GitHub's limits.
async fn adopt_many(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<ManyBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    allowed(&app)?;
    forget_updates(&id);
    let n = b.items.len();
    let job = crate::pack_api::spawn(&app, format!("Finding sources for {n} mods on {}", e.name), move |job| {
        let mut inst = instance(&e)?;
        let mut done = Vec::new();
        for (i, it) in b.items.iter().enumerate() {
            job.check_cancelled()?;
            job.progress(i, n);
            job.detail(format!("{}: {}", it.file, it.link));
            match sources::plan_adopt(&inst, &it.file, &it.link, b.prerelease) {
                Ok(a) => {
                    sources::trust(&a.source)?;
                    sources::adopt(&mut inst, &it.file, &a)?;
                    done.push(it.file.clone());
                }
                Err(err) => job.note(format!("{}: {err:#}", it.file)),
            }
        }
        Ok(json!({ "added": done }))
    });
    Ok(Json(json!({ "job": job })))
}

/// Confirms the source of a mod that came with a pack or a sync, so its
/// updates are checked.
async fn trust(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<FileQuery>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    allowed(&app)?;
    forget_updates(&id);
    run(move || {
        let inst = instance(&e)?;
        let a = inst
            .addons
            .iter()
            .find(|a| a.file_name.eq_ignore_ascii_case(&b.file))
            .and_then(|a| a.source.clone())
            .ok_or_else(|| anyhow::anyhow!("{} doesn't follow a source", b.file))?;
        sources::trust(&a)
    })
    .await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct PinsBody {
    pins: Vec<sources::carry::Pin>,
}

/// Sources that came along with synced jars (Hopper desktop's sync). Each
/// is attached only to a jar with its pinned sha256, and isn't trusted by
/// this: the owner confirms it before its updates are checked.
async fn put_pins(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<PinsBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    // Mods from links are off on this panel: the jars stay plain jars
    // (nobody here said yes to their sources).
    if !app.cfg.read().unwrap().allow_sources {
        return Ok(Json(json!({ "attached": 0, "off": true })));
    }
    forget_updates(&id);
    let n = run(move || {
        instance(&e)?;
        sources::carry::apply(&e.root, &b.pins)
    })
    .await?;
    Ok(Json(json!({ "attached": n })))
}

// ---- data packs from GitHub or a link ----------------------------------------------------

/// A data pack link looked up for a server: a folder on a GitHub branch,
/// a release's zip or a link to a zip. Also lists where it can go.
async fn pack_resolve(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<ResolveBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    allowed(&app)?;
    let v = run(move || {
        let inst = instance(&e)?;
        let r = sources::packs::resolve(&b.link, &inst.game_version, hopper_core::cf::CLASS_DATAPACKS, b.prerelease)?;
        let mut v = resolved_json(&r);
        v["branch"] = json!(sources::tree::of_url(&r.source.url).map(|t| t.branch));
        v["folders"] = json!(r.candidates.iter().map(|c| sources::tree::parse_link(&c.page).map(|t| t.path)).collect::<Vec<_>>());
        v["dirs"] = json!(crate::browse_api::datapack_dirs(&inst)
            .into_iter()
            .map(|(l, d)| json!({ "label": l, "path": d }))
            .collect::<Vec<_>>());
        Ok(v)
    })
    .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct PackAddBody {
    link: String,
    #[serde(default)]
    prerelease: bool,
    /// The candidate picked: its index in the lookup.
    pick: usize,
    /// One of the lookup's `dirs` (relative to the server).
    dest: String,
}

/// Adds a data pack from a link into one of the server's data pack
/// folders, as a job. A pack of the same name there goes to the server's
/// .hopper-backups first.
async fn pack_add(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<PackAddBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    allowed(&app)?;
    let running = e.server.state().is_alive();
    let job = crate::pack_api::spawn(&app, format!("Adding a data pack to {}", e.name), move |job| {
        let inst = instance(&e)?;
        // Only the server's own data pack folders, never a path from the browser.
        let dirs = crate::browse_api::datapack_dirs(&inst);
        let rel = dirs
            .iter()
            .find(|d| d.1 == b.dest)
            .map(|d| d.1.clone())
            .ok_or_else(|| anyhow::anyhow!("That isn't one of this server's data pack folders."))?;
        let dir = inst.root.join(&rel);
        let r = sources::packs::resolve(&b.link, &inst.game_version, hopper_core::cf::CLASS_DATAPACKS, b.prerelease)?;
        anyhow::ensure!(b.pick < r.candidates.len(), "That file is no longer offered there; look the link up again.");
        sources::trust(&r.source)?;
        let same = dir.join(sources::tree::of_url(&r.source.url).map_or_else(
            || r.candidates[b.pick].file_name.clone(),
            |t| sources::tree::parse_link(&r.candidates[b.pick].page).unwrap_or(t).file_name(),
        ));
        let mut saved = None;
        if same.is_file() {
            saved = hopper_core::sync::backup_into(&inst.root, &same)?;
        }
        let new = sources::packs::install(&inst.root, &dir, &r, b.pick, job)?;
        let file = new.file_name().unwrap_or_default().to_string_lossy().into_owned();
        log::info!("{}: {file} into {rel} from {}", e.name, r.source.publisher);
        Ok(json!({
            "added": [file], "dest": rel, "restart_needed": running,
            "backup": saved.map(|p| p.to_string_lossy().into_owned()),
        }))
    });
    Ok(Json(json!({ "job": job })))
}
