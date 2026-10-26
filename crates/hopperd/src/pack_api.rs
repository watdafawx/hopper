//! Server pack updates run here, next to the server's files, as jobs Hopper
//! desktop follows: `/servers/{id}/pack/plan`, `/servers/{id}/pack/apply`,
//! then `/jobs/{job}` for progress and the result.

use crate::api::{ApiError, ApiResult, not_found, require};
use crate::App;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use hopper_core::cf;
use hopper_core::instances::Instance;
use hopper_core::server_pack_update::{self as spu, ServerPlan};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/servers/{id}/pack/plan", post(plan))
        .route("/api/v1/servers/{id}/pack/apply", post(apply))
        .route("/api/v1/jobs/{job}", get(job))
        .route("/api/v1/jobs/{job}/cancel", post(cancel))
}

/// Uses the desktop's CurseForge key when this panel has none.
fn ensure_cf_key(key: &str) {
    if cf::api_key().is_none() {
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

#[derive(Deserialize)]
struct PlanBody {
    inst: Instance,
    pack: cf::File,
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
    let title = format!("Comparing {} with {}", e.name, b.pack.display_name);
    let job = spawn(&app, title, move |job| {
        let server = hopper_core::server::load(&e.root);
        let r = spu::plan(&server, &b.inst, &b.pack, job);
        hopper_core::modpack::prune_packs_older_than(b.keep_days.unwrap_or(KEEP_DAYS));
        r
    });
    Ok(Json(json!({ "job": job })))
}

#[derive(Deserialize)]
struct ApplyBody {
    plan: ServerPlan,
    #[serde(default)]
    take: HashSet<String>,
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
    log::info!("{}: updating from {}", e.name, b.plan.plan.file.display_name);
    let title = format!("{} → {}", e.name, b.plan.plan.file.display_name);
    let job = spawn(&app, title, move |job| {
        let server = hopper_core::server::load(&e.root);
        // NeoForge's installer runs with the server's own Java.
        let java = server.java.clone().map(std::path::PathBuf::from).unwrap_or_else(|| "java".into());
        let r = spu::apply(&server, &b.plan, &b.take, "127.0.0.1", &java, None, job);
        hopper_core::modpack::prune_packs_older_than(b.keep_days.unwrap_or(KEEP_DAYS));
        r
    });
    Ok(Json(json!({ "job": job })))
}

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
    Ok(Json(json!({ "ok": true })))
}
