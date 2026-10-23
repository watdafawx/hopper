//! The web UI, built into the binary (crates/hopperd/ui → dist, copied in by
//! build.rs). Any path that isn't a file gets index.html, so the UI's own
//! routes work on reload.

use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use include_dir::{Dir, include_dir};

static UI: Dir<'static> = include_dir!("$OUT_DIR/ui");

pub fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "webmanifest" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "txt" | "log" | "properties" | "toml" | "cfg" | "yml" | "yaml" => "text/plain; charset=utf-8",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}

pub async fn serve(uri: Uri, headers: axum::http::HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") {
        return (StatusCode::NOT_FOUND, axum::Json(serde_json::json!({ "error": "No such API route." }))).into_response();
    }
    let (file, path) = match UI.get_file(path) {
        Some(f) if !path.is_empty() => (f, path),
        _ => match UI.get_file("index.html") {
            Some(f) => (f, "index.html"),
            None => return (StatusCode::NOT_FOUND, "UI not built").into_response(),
        },
    };
    let gzip_ok = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|e| e.trim().starts_with("gzip")));
    let gz = if gzip_ok { UI.get_file(format!("{path}.gz")) } else { None };
    // Vite names built assets by content hash, so they never change.
    let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    let mut r = (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(mime(path))),
            (header::CACHE_CONTROL, HeaderValue::from_static(cache)),
            (header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
            (header::REFERRER_POLICY, HeaderValue::from_static("same-origin")),
            (header::VARY, HeaderValue::from_static("accept-encoding")),
        ],
        gz.map_or(file.contents(), |g| g.contents()),
    )
        .into_response();
    if gz.is_some() {
        r.headers_mut().insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    }
    r
}
