//! Looking inside zips and jars from the Files tab, like folders: one
//! level of entries at a time, and downloading a single entry. Nothing is
//! extracted to disk.

use crate::App;
use crate::api::{ApiResult, bad, not_found, require};
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::io::Read as _;
use std::sync::Arc;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/fs/{id}/zip-list", get(zip_list))
        .route("/api/v1/fs/{id}/zip-entry", get(zip_entry))
}

fn open(app: &App, headers: &HeaderMap, method: &Method, id: &str, rel: &str) -> ApiResult<zip::ZipArchive<std::fs::File>> {
    require(app, headers, method)?;
    let e = app.servers.get(id).ok_or_else(|| not_found("server"))?;
    let p = crate::files::resolve(&e.root, rel).map_err(bad)?;
    let f = std::fs::File::open(&p).map_err(|_| not_found("file"))?;
    zip::ZipArchive::new(f).map_err(|_| bad("That isn't a zip file Hopper can read."))
}

/// The entries directly in `inner` ("" for the top) of the zip at `path`:
/// folders first, with sizes; plus the mod's name for a jar.
async fn zip_list(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let rel = q.get("path").cloned().unwrap_or_default();
    let inner = q.get("inner").cloned().unwrap_or_default().trim_matches('/').to_string();
    let mut z = open(&app, &headers, &method, &id, &rel)?;
    let v = tokio::task::spawn_blocking(move || {
        let prefix = if inner.is_empty() { String::new() } else { format!("{inner}/") };
        // name -> (dir, size, compressed)
        let mut here: BTreeMap<String, (bool, u64, u64)> = BTreeMap::new();
        let total = z.len();
        for i in 0..total {
            let Ok(f) = z.by_index_raw(i) else { continue };
            let name = f.name().replace('\\', "/");
            let Some(rest) = name.strip_prefix(&prefix) else { continue };
            if rest.is_empty() {
                continue;
            }
            match rest.split_once('/') {
                Some((dir, _)) => {
                    here.entry(dir.to_string()).or_insert((true, 0, 0));
                }
                None => {
                    here.insert(rest.to_string(), (false, f.size(), f.compressed_size()));
                }
            }
        }
        let mut entries: Vec<Value> = here
            .into_iter()
            .map(|(name, (dir, size, packed))| json!({ "name": name, "dir": dir, "size": size, "packed": packed }))
            .collect();
        entries.sort_by_key(|e| (!e["dir"].as_bool().unwrap_or(false), e["name"].as_str().unwrap_or("").to_lowercase()));
        json!({ "entries": entries, "total": total })
    })
    .await
    .map_err(bad)?;
    let mut v = v;
    if rel.to_lowercase().ends_with(".jar") {
        let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
        if let Ok(p) = crate::files::resolve(&e.root, &rel) {
            let (ids, name) = tokio::task::spawn_blocking(move || hopper_core::deps::own_ids(&p)).await.map_err(bad)?;
            v["mod"] = json!({ "name": name, "ids": ids });
        }
    }
    Ok(Json(v))
}

/// One entry of a zip, as a download (or inline for images and text).
async fn zip_entry(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let rel = q.get("path").cloned().unwrap_or_default();
    let entry = q.get("entry").cloned().unwrap_or_default();
    let inline = q.contains_key("inline");
    let mut z = open(&app, &headers, &method, &id, &rel)?;
    let (name, bytes) = tokio::task::spawn_blocking(move || -> anyhow::Result<(String, Vec<u8>)> {
        let mut f = z.by_name(&entry)?;
        anyhow::ensure!(f.is_file(), "That's a folder in the zip.");
        anyhow::ensure!(f.size() <= 512 * 1024 * 1024, "That entry is too big to download from inside the zip.");
        let mut out = Vec::with_capacity(f.size() as usize);
        f.read_to_end(&mut out)?;
        let name = entry.rsplit('/').next().unwrap_or(&entry).to_string();
        Ok((name, out))
    })
    .await
    .map_err(bad)?
    .map_err(|e| bad(format!("{e:#}")))?;
    let lower = name.to_lowercase();
    let kind = if !inline {
        "application/octet-stream"
    } else if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else {
        "text/plain; charset=utf-8"
    };
    let safe: String = name.chars().filter(|c| !c.is_control() && *c != '"').collect();
    let disposition = if inline { format!("inline; filename=\"{safe}\"") } else { format!("attachment; filename=\"{safe}\"") };
    Ok((
        [
            (header::CONTENT_TYPE, kind.to_string()),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CONTENT_SECURITY_POLICY, "sandbox; default-src 'none'; img-src 'self'".to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        Body::from(bytes),
    )
        .into_response())
}
