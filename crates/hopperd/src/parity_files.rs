//! Routes added to bring the panel's files area closer to Hopper desktop:
//! a config file as settings (switches, pickers and values from the file's
//! own comments, edits that keep the rest of the file), and which mod each
//! entry of `config/` belongs to, so the panel can show the mod's icon and
//! name beside its configs and a picker over a mod's other config files.

use crate::App;
use crate::api::{ApiResult, bad, not_found, require};
use crate::configs::sections_json;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post};
use axum::{Json, Router};
use hopper_core::configedit;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// Config files can be a few MB; the text travels with each edit.
const MAX_BODY: usize = 8 * 1024 * 1024;

/// Endings of config files that are text (desktop's config editor's list).
const TEXT_ENDINGS: [&str; 10] = [".toml", ".json", ".json5", ".cfg", ".properties", ".txt", ".yml", ".yaml", ".snbt", ".ini"];

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/fs/{id}/config-owners", get(owners))
        .route("/api/v1/fs/{id}/config/parse", post(parse).layer(DefaultBodyLimit::max(MAX_BODY)))
        .route("/api/v1/fs/{id}/config/set", post(set).layer(DefaultBodyLimit::max(MAX_BODY)))
        .route("/api/v1/fs/{id}/version-text", get(version_text))
}

/// A saved-over version's whole text (History's "Use this version"), so it
/// comes back exactly, blank lines at the end included.
async fn version_text(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let who = require(&app, &headers, &method)?;
    let e = app.servers.get(&id).filter(|_| who.sees(&id)).ok_or_else(|| not_found("server"))?;
    let version = q.get("version").cloned().unwrap_or_default();
    let text = tokio::task::spawn_blocking(move || crate::files::trash_text(&e.root, &version))
        .await
        .map_err(bad)?
        .map_err(|e| bad(format!("{e:#}")))?;
    Ok(Json(json!({ "content": text })))
}

fn is_text(rel: &str) -> bool {
    let l = rel.to_ascii_lowercase();
    TEXT_ENDINGS.iter().any(|e| l.ends_with(e))
}

fn walk(dir: &std::path::Path, rel: &str, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let r = format!("{rel}/{}", e.file_name().to_string_lossy());
        // Links aren't followed: one could lead outside the server folder,
        // or round in a loop.
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            walk(&e.path(), &r, out);
        } else if ft.is_file() {
            out.push(r);
        }
    }
}

/// `{ owners: { "<entry of config/>": "<jar>" }, files: { "<jar>": ["config/…"] } }`:
/// the mod each config file or folder is named after (by the mod ids in its
/// jar), and each mod's text config files.
async fn owners(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let v = tokio::task::spawn_blocking(move || {
        let root = e.root.clone();
        // Inside the server folder only (a linked mods/ or config/ that
        // leads elsewhere reads as empty).
        let inside = |name: &str| crate::files::resolve(&root, name).ok();
        // The same rule as the Configs page and Hopper desktop.
        let owners_of = hopper_core::configowner::Owners::new(crate::configs::mod_refs(&root).0);
        let mut owners: BTreeMap<String, String> = BTreeMap::new();
        let mut files: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for entry in inside("config").and_then(|d| std::fs::read_dir(d).ok()).into_iter().flatten().flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let is_dir = ft.is_dir();
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = format!("config/{name}");
            let probe = if is_dir { format!("{rel}/") } else { rel.clone() };
            let Some(jar) = owners_of.owner(&probe).map(|m| m.key.clone()) else { continue };
            if hopper_core::packdiff::pack_content(&rel) {
                continue;
            }
            owners.insert(name, jar.clone());
            let list = files.entry(jar).or_default();
            if is_dir {
                walk(&path, &rel, list);
            } else {
                list.push(rel);
            }
        }
        for list in files.values_mut() {
            list.retain(|r| is_text(r) && !hopper_core::packdiff::pack_content(r));
            list.sort();
        }
        json!({ "owners": owners, "files": files })
    })
    .await
    .map_err(bad)?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct ParseBody {
    text: String,
    /// The file's path: read as its own format. Without it, TOML.
    #[serde(default)]
    rel: String,
}

fn format_of(rel: &str) -> Result<configedit::Format, String> {
    if rel.is_empty() {
        return Ok(configedit::Format::Toml);
    }
    configedit::Format::of(rel).ok_or_else(|| "Hopper can't read this kind of file as settings.".to_string())
}

/// `{ ok, sections }` for a config's text, or `{ ok: false, error }` when it
/// doesn't read.
async fn parse(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<ParseBody>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let v = tokio::task::spawn_blocking(move || match format_of(&b.rel).and_then(|f| configedit::parse_as(f, &b.text)) {
        Ok(s) => json!({ "ok": true, "sections": sections_json(&s) }),
        Err(error) => json!({ "ok": false, "error": error }),
    })
    .await
    .map_err(bad)?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct SetBody {
    text: String,
    /// The file's path: read as its own format. Without it, TOML.
    #[serde(default)]
    rel: String,
    /// The table the setting is in.
    #[serde(default)]
    table: Vec<String>,
    key: String,
    /// The value as typed (`true`, `8`, `SAFE`, `["a", "b"]`).
    value: String,
}

/// Sets one setting in `text`, the way desktop's config editor does: the
/// typed value read as the setting's kind and checked against its range and
/// allowed values; only the value's own characters change. `{ ok, text,
/// sections }`, or `{ ok: false, error }` (the field's message).
async fn set(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<SetBody>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let v = tokio::task::spawn_blocking(move || {
        let fail = |error: String| json!({ "ok": false, "error": error });
        let f = match format_of(&b.rel) {
            Ok(f) => f,
            Err(e) => return fail(e),
        };
        let sections = match configedit::parse_as(f, &b.text) {
            Ok(s) => s,
            Err(e) => return fail(e),
        };
        let Some(entry) = sections.iter().flat_map(|s| &s.entries).find(|e| e.path == b.table && e.key == b.key) else {
            return fail(format!("No {} in the file.", b.key));
        };
        // The masked secret sent back: nothing changes.
        if b.value == crate::configs::MASK && crate::configs::secret(&entry.key) {
            return json!({ "ok": true, "text": b.text, "sections": sections_json(&sections) });
        }
        let value = match entry.value.parse_like(&b.value) {
            Ok(v) => v,
            Err(e) => return fail(e),
        };
        let edit = configedit::Edit { path: b.table.clone(), key: b.key.clone(), value };
        match configedit::apply(f, &b.text, &[edit]).and_then(|t| configedit::parse_as(f, &t).map(|s| (t, s)).map_err(|e| vec![configedit::Refused { path: Vec::new(), key: String::new(), error: e }])) {
            Ok((text, s)) => json!({ "ok": true, "text": text, "sections": sections_json(&s) }),
            Err(bad) => fail(bad.into_iter().next().map(|r| r.error).unwrap_or_default()),
        }
    })
    .await
    .map_err(bad)?;
    Ok(Json(v))
}
