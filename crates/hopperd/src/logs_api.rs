//! Sharing a server's log or crash report on mclo.gs: the list of logs, a
//! preview of what gets hidden, and the upload. Uploads are public, so the
//! panel asks first and cleans the log (IPs, tokens, paths, user name).

use crate::App;
use crate::api::{ApiResult, bad, not_found, require};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post};
use axum::{Json, Router};
use hopper_core::hosting::logshare;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// Preview text sent to the browser at most.
const PREVIEW: usize = 256 << 10;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/servers/{id}/logs", get(list))
        .route("/api/v1/servers/{id}/logs/preview", get(preview))
        .route("/api/v1/servers/{id}/logs/share", post(share))
}

async fn list(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let l = tokio::task::spawn_blocking(move || logshare::list(&e.root)).await.map_err(bad)?;
    Ok(Json(json!({ "logs": l })))
}

#[derive(Deserialize)]
struct PathQ {
    path: String,
}

async fn preview(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Query(q): Query<PathQ>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let text = logshare::read(&e.root, &q.path)?;
        let (clean, hidden) = logshare::redact(&text, Some(&e.root));
        let cut = clean.len() > PREVIEW;
        let shown = if cut { clean[clean.ceil_char_boundary(clean.len() - PREVIEW)..].to_string() } else { clean };
        Ok(json!({ "hidden": hidden, "size": text.len(), "text": shown, "cut": cut }))
    })
    .await
    .map_err(bad)?
    .map_err(|e| bad(format!("{e:#}")))?;
    Ok(Json(v))
}

async fn share(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(q): Json<PathQ>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let name = e.name.clone();
    let path = q.path.clone();
    let (url, hidden) = tokio::task::spawn_blocking(move || -> anyhow::Result<(String, logshare::Hidden)> {
        let text = logshare::read(&e.root, &q.path)?;
        let (clean, hidden) = logshare::redact(&text, Some(&e.root));
        Ok((hopper_core::mclogs::upload(&clean)?, hidden))
    })
    .await
    .map_err(bad)?
    .map_err(|e| bad(format!("{e:#}")))?;
    log::info!("{name}: shared {path} on mclo.gs ({url}); hid {} IPs, {} tokens", hidden.ips, hidden.tokens);
    Ok(Json(json!({ "url": url, "hidden": hidden })))
}
