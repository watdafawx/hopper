//! `/api/v1/fs/{server}/...`: the file manager's routes. The work is in
//! [`crate::files`]; this file is HTTP: auth, streaming, ranges, limits.

use crate::api::{ApiError, ApiResult, bad, not_found, require};
use crate::files::{self, Op};
use crate::servers::Entry;
use crate::App;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use tower_http::compression::CompressionLayer;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{Seek as _, SeekFrom};
use std::sync::Arc;

/// Largest upload chunk accepted. The UI sends 8 MB.
pub const MAX_CHUNK: usize = 32 * 1024 * 1024;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        // JSON replies compress well (a 10k-file folder: 685 KB → ~60 KB);
        // file downloads stay as they are so ranges keep working.
        .route("/api/v1/fs/{id}/list", get(list).layer(CompressionLayer::new()))
        .route("/api/v1/fs/{id}/file", get(download))
        .route("/api/v1/fs/{id}/zip", get(zip))
        .route("/api/v1/fs/{id}/text", get(read_text).put(write_text).layer(CompressionLayer::new()))
        .route("/api/v1/fs/{id}/ops", post(ops))
        .route("/api/v1/fs/{id}/trash", get(trash_list))
        .route("/api/v1/fs/{id}/trash/restore", post(trash_restore))
        .route("/api/v1/fs/{id}/trash/purge", post(trash_purge))
        .route("/api/v1/fs/{id}/uploads", post(upload_start))
        .route(
            "/api/v1/fs/{id}/uploads/{up}",
            get(upload_status).put(upload_chunk).delete(upload_cancel),
        )
        .layer(DefaultBodyLimit::max(MAX_CHUNK))
}

/// Checks auth and finds the server.
fn server(app: &App, headers: &HeaderMap, method: &Method, id: &str) -> ApiResult<Arc<Entry>> {
    require(app, headers, method)?;
    app.servers.get(id).ok_or_else(|| not_found("server"))
}

/// Runs blocking file work off the async threads; errors become 400s.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(bad)?.map_err(|e| bad(format!("{e:#}")))
}

fn path_q(q: &HashMap<String, String>) -> String {
    q.get("path").cloned().unwrap_or_default()
}

async fn list(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let e = server(&app, &headers, &method, &id)?;
    let rel = path_q(&q);
    let entries = blocking(move || files::list(&e.root, &rel)).await?;
    Ok(Json(json!({ "entries": entries })))
}

/// `attachment; filename*=UTF-8''...`
fn disposition(name: &str, inline: bool) -> HeaderValue {
    let enc: String = name
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    let kind = if inline { "inline" } else { "attachment" };
    HeaderValue::from_str(&format!("{kind}; filename*=UTF-8''{enc}")).unwrap_or(HeaderValue::from_static("attachment"))
}

/// `bytes=a-b` / `bytes=a-` / `bytes=-n` within `len`.
fn parse_range(h: &str, len: u64) -> Option<(u64, u64)> {
    let r = h.strip_prefix("bytes=")?;
    if r.contains(',') {
        return None;
    }
    let (a, b) = r.split_once('-')?;
    let (start, end) = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            (len.saturating_sub(n), len.checked_sub(1)?)
        }
        (a, "") => (a.parse().ok()?, len.checked_sub(1)?),
        (a, b) => (a.parse().ok()?, b.parse::<u64>().ok()?.min(len.checked_sub(1)?)),
    };
    (start <= end && end < len).then_some((start, end))
}

/// A file, resumable (Range requests), streamed in 256 KB pieces.
async fn download(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let e = server(&app, &headers, &method, &id)?;
    let rel = path_q(&q);
    let p = files::resolve(&e.root, &rel).map_err(bad)?;
    let meta = tokio::fs::metadata(&p).await.map_err(|_| not_found("file"))?;
    if meta.is_dir() {
        return Err(bad("That's a folder; download it as a zip."));
    }
    let len = meta.len();
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let inline = q.contains_key("inline");
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|h| parse_range(h, len).ok_or(()));
    let (start, end, status) = match range {
        None => (0, len.saturating_sub(1), StatusCode::OK),
        Some(Ok((s, e))) => (s, e, StatusCode::PARTIAL_CONTENT),
        Some(Err(())) => {
            return Ok((StatusCode::RANGE_NOT_SATISFIABLE, [(header::CONTENT_RANGE, format!("bytes */{len}"))]).into_response());
        }
    };
    let count = if len == 0 { 0 } else { end - start + 1 };
    let mut file = std::fs::File::open(&p).map_err(|e| bad(format!("can't open {rel}: {e}")))?;
    file.seek(SeekFrom::Start(start)).map_err(bad)?;
    let body = Body::from_stream(tokio_util::io::ReaderStream::with_capacity(
        tokio::io::AsyncReadExt::take(tokio::fs::File::from_std(file), count),
        256 * 1024,
    ));
    let mut r = Response::builder()
        .status(status)
        .header(header::CONTENT_LENGTH, count)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_DISPOSITION, disposition(&name, inline))
        .header(header::CONTENT_TYPE, if inline { inline_type(&name) } else { "application/octet-stream" })
        // A file from a server folder never runs as a page of the panel.
        .header(header::CONTENT_SECURITY_POLICY, "sandbox; default-src 'none'; img-src 'self'")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CACHE_CONTROL, "no-store");
    if status == StatusCode::PARTIAL_CONTENT {
        r = r.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"));
    }
    r.body(body).map_err(bad)
}

/// What a file may be shown inline as: text or a raster image. Never HTML
/// or SVG, which could run script with the panel's origin.
fn inline_type(name: &str) -> &'static str {
    match crate::ui::mime(name) {
        t @ ("image/png" | "image/jpeg" | "image/gif" | "image/webp") => t,
        _ => "text/plain; charset=utf-8",
    }
}

/// Sends what's written to it down a channel, in 256 KB pieces.
struct ChanWriter {
    tx: tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
    buf: Vec<u8>,
}

impl ChanWriter {
    fn send(&mut self) -> std::io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = Bytes::from(std::mem::replace(&mut self.buf, Vec::with_capacity(256 * 1024)));
        self.tx
            .blocking_send(Ok(chunk))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "download cancelled"))
    }
}

impl std::io::Write for ChanWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(b);
        if self.buf.len() >= 256 * 1024 {
            self.send()?;
        }
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.send()
    }
}

/// Folders and files as one zip, streamed while it's made.
async fn zip(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<Vec<(String, String)>>,
) -> ApiResult<Response> {
    let e = server(&app, &headers, &method, &id)?;
    let paths: Vec<String> = q.iter().filter(|(k, _)| k == "path").map(|(_, v)| v.clone()).collect();
    let mut items = Vec::new();
    for p in &paths {
        items.push(files::resolve(&e.root, p).map_err(bad)?);
    }
    if items.is_empty() {
        items.push(e.root.clone());
    }
    // Entries are named relative to the folder the items are in.
    let base = items[0].parent().map(|p| p.to_path_buf()).unwrap_or_else(|| e.root.clone());
    let base = if items.len() == 1 && items[0] == e.root { e.root.clone() } else { base };
    let name = q
        .iter()
        .find(|(k, _)| k == "name")
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| match (items.len(), items[0].file_name()) {
            (1, Some(n)) if items[0] != e.root => format!("{}.zip", n.to_string_lossy()),
            _ => format!("{}.zip", e.name),
        });
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let root = e.root.clone();
    let at_root = base == root;
    tokio::task::spawn_blocking(move || {
        let w = ChanWriter { tx: tx.clone(), buf: Vec::with_capacity(256 * 1024) };
        let skip = move |n: &str| at_root && files::is_internal(n);
        let res = if items.len() == 1 && items[0] == root {
            let kids: Vec<_> = std::fs::read_dir(&root).map(|rd| rd.flatten().map(|d| d.path()).collect()).unwrap_or_default();
            crate::zipstream::zip_paths(w, &root, &kids, &skip)
        } else {
            crate::zipstream::zip_paths(w, &base, &items, &skip)
        };
        if let Err(err) = res
            && err.kind() != std::io::ErrorKind::BrokenPipe
        {
            log::warn!("zip download: {err}");
            let _ = tx.blocking_send(Err(err));
        }
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "application/zip")
        .header(header::CONTENT_DISPOSITION, disposition(&name, false))
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx)))
        .map_err(bad)
}

async fn read_text(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<files::Text>> {
    let e = server(&app, &headers, &method, &id)?;
    let rel = path_q(&q);
    Ok(Json(blocking(move || files::read_text(&e.root, &rel)).await?))
}

#[derive(Deserialize)]
struct SaveBody {
    path: String,
    content: String,
    /// The modified time the editor loaded; absent for a new file.
    modified: Option<u64>,
    #[serde(default)]
    force: bool,
}

async fn write_text(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<SaveBody>,
) -> ApiResult<Json<files::SaveResult>> {
    let e = server(&app, &headers, &method, &id)?;
    log::info!("{}: saving {}", e.name, b.path);
    Ok(Json(blocking(move || files::write_text(&e.root, &b.path, &b.content, b.modified, b.force)).await?))
}

#[derive(Deserialize)]
struct OpsBody {
    ops: Vec<Op>,
}

/// Runs operations in order, stopping at the first that fails.
async fn ops(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<OpsBody>,
) -> ApiResult<Json<Value>> {
    let e = server(&app, &headers, &method, &id)?;
    let name = e.name.clone();
    let done = blocking(move || {
        let mut done = Vec::new();
        for op in b.ops {
            log::info!("{name}: {op:?}");
            done.push(files::run(&e.root, op)?);
        }
        Ok(done)
    })
    .await?;
    Ok(Json(json!({ "done": done })))
}

async fn trash_list(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let e = server(&app, &headers, &method, &id)?;
    let items = blocking(move || files::list_trash(&e.root)).await?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
struct RestoreBody {
    id: String,
}

async fn trash_restore(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<RestoreBody>,
) -> ApiResult<Json<Value>> {
    let e = server(&app, &headers, &method, &id)?;
    let to = blocking(move || files::restore(&e.root, &b.id)).await?;
    Ok(Json(json!({ "path": to })))
}

#[derive(Deserialize)]
struct PurgeBody {
    #[serde(default)]
    ids: Vec<String>,
}

async fn trash_purge(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<PurgeBody>,
) -> ApiResult<Json<Value>> {
    let e = server(&app, &headers, &method, &id)?;
    log::info!("{}: emptying {} trash item(s)", e.name, if b.ids.is_empty() { "all".into() } else { b.ids.len().to_string() });
    let n = blocking(move || files::purge(&e.root, &b.ids)).await?;
    Ok(Json(json!({ "removed": n })))
}

#[derive(Deserialize)]
struct StartBody {
    path: String,
    size: u64,
    #[serde(default)]
    overwrite: bool,
}

async fn upload_start(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<StartBody>,
) -> ApiResult<Response> {
    let e = server(&app, &headers, &method, &id)?;
    let path = b.path.clone();
    match blocking(move || files::upload_start(&e.root, &b.path, b.size, b.overwrite)).await {
        Ok(up) => Ok(Json(json!({ "id": up.id, "offset": 0, "path": up.path })).into_response()),
        Err(ApiError(_, msg)) if msg.ends_with("already exists.") => {
            Ok((StatusCode::CONFLICT, Json(json!({ "error": msg, "exists": path })))
                .into_response())
        }
        Err(err) => Err(err),
    }
}

async fn upload_status(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path((id, up)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let e = server(&app, &headers, &method, &id)?;
    let (u, have) = blocking(move || files::upload_status(&e.root, &up)).await?;
    Ok(Json(json!({ "id": u.id, "path": u.path, "size": u.size, "offset": have })))
}

async fn upload_chunk(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path((id, up)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<Response> {
    let e = server(&app, &headers, &method, &id)?;
    let offset: u64 = q.get("offset").and_then(|o| o.parse().ok()).ok_or_else(|| bad("offset is required"))?;
    match blocking(move || files::upload_chunk(&e.root, &up, offset, &body)).await? {
        Ok(r) => Ok(Json(r).into_response()),
        // Wrong offset (a retry after a lost reply): say where to go on.
        Err(have) => Ok((StatusCode::CONFLICT, Json(json!({ "error": "offset mismatch", "offset": have }))).into_response()),
    }
}

async fn upload_cancel(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path((id, up)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let e = server(&app, &headers, &method, &id)?;
    blocking(move || files::upload_cancel(&e.root, &up)).await?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::{inline_type, parse_range};

    #[test]
    fn inline_never_html_or_svg() {
        assert_eq!(inline_type("x.html"), "text/plain; charset=utf-8");
        assert_eq!(inline_type("x.svg"), "text/plain; charset=utf-8");
        assert_eq!(inline_type("x.js"), "text/plain; charset=utf-8");
        assert_eq!(inline_type("icon.png"), "image/png");
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=500-", 1000), Some((500, 999)));
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=900-5000", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=1000-", 1000), None);
        assert_eq!(parse_range("bytes=5-1", 1000), None);
        assert_eq!(parse_range("bytes=0-1,5-6", 1000), None);
        assert_eq!(parse_range("items=0-1", 1000), None);
        assert_eq!(parse_range("bytes=0-", 0), None);
    }
}
