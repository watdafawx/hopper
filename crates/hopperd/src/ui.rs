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

/// Which web UI this exe carries: a hash of index.html, which names every
/// built file by its content hash. The panel reloads when it changes.
pub fn build_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        UI.get_file("index.html").map(|f| f.contents()).unwrap_or_default().hash(&mut h);
        format!("{:016x}", h.finish())
    })
}

/// Different every time the panel starts, so a page can tell it restarted.
pub fn boot_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        format!("{}-{}", t.as_millis(), std::process::id())
    })
}

/// The Content-Security-Policy on the web UI (design/multi-panel.md, B0).
/// Scripts only from this panel's own files: nothing inline, no eval, no
/// plugins, no `<base>`, never inside another site's frame. That is what
/// stops text from a mod page or a log from ever running as script.
/// - `style-src 'unsafe-inline'`: Svelte writes `style="…"` on elements
///   (sizes, colours, progress) and the file editor (CodeMirror) puts its
///   theme in a `<style>` element it makes at run time. CSS can't run
///   script; text from mods and logs is set as text, never as markup.
/// - `img-src` any http(s), data: and blob: URLs: mod and pack icons and
///   pictures come from CurseForge, Modrinth, GitHub and the authors' sites.
/// - `connect-src` any http(s)/ws(s): a page may later call other panels;
///   what limits it is `script-src`, not where it may call.
pub const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' https: http: data: blob:; font-src 'self' data:; connect-src 'self' https: http: ws: wss:; media-src 'self' blob: data:; worker-src 'self' blob:; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'; manifest-src 'self'";

pub async fn serve(uri: Uri, headers: axum::http::HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") {
        return (StatusCode::NOT_FOUND, axum::Json(serde_json::json!({ "error": "No such API route." }))).into_response();
    }
    let (file, path) = match UI.get_file(path) {
        Some(f) if !path.is_empty() => (f, path),
        // A page from before an update asks for its old hashed files: a 404
        // (not index.html as JavaScript) lets it notice and reload.
        _ if path.starts_with("assets/") => return (StatusCode::NOT_FOUND, "Not found").into_response(),
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
            (header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP)),
            (header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")),
        ],
        gz.map_or(file.contents(), |g| g.contents()),
    )
        .into_response();
    if gz.is_some() {
        r.headers_mut().insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pages_carry_the_csp() {
        for path in ["/", "/index.html", "/some/route", "/logo.svg"] {
            let r = serve(path.parse().unwrap(), Default::default()).await;
            let csp = r.headers().get(header::CONTENT_SECURITY_POLICY).and_then(|v| v.to_str().ok()).unwrap_or_default();
            assert_eq!(csp, CSP, "{path}");
            assert_eq!(r.headers()[header::X_FRAME_OPTIONS], "DENY");
        }
        let d: Vec<&str> = CSP.split(';').map(str::trim).collect();
        for want in ["script-src 'self'", "object-src 'none'", "base-uri 'none'", "frame-ancestors 'none'", "style-src 'self' 'unsafe-inline'"] {
            assert!(d.contains(&want), "{want}");
        }
        assert!(!CSP.contains("unsafe-eval"));
        let script = d.iter().find(|x| x.starts_with("script-src")).unwrap();
        assert!(!script.contains("unsafe") && !script.contains("http") && !script.contains('*'));
        assert!(d.iter().any(|x| x.starts_with("connect-src") && x.contains("https:") && x.contains("wss:")));
    }

    /// The built page itself has nothing the CSP would block.
    #[test]
    fn built_page_has_no_inline_script_or_style() {
        let html = std::str::from_utf8(UI.get_file("index.html").expect("UI built").contents()).unwrap();
        for tag in html.split("<script").skip(1) {
            let open = tag.split('>').next().unwrap_or("");
            assert!(open.contains("src="), "inline <script> in index.html: {open}");
        }
        assert!(!html.contains("<style"), "inline <style> in index.html");
        assert!(!html.contains(" style="), "style attribute in index.html");
        for h in [" onload=", " onerror=", " onclick="] {
            assert!(!html.contains(h), "inline handler in index.html: {h}");
        }
    }
}
