//! Routes added to bring the panel's files area closer to Hopper desktop:
//! a TOML config as settings (switches, pickers and values from the file's
//! own comments, edits that keep the rest of the file), and which mod each
//! entry of `config/` belongs to, so the panel can show the mod's icon and
//! name beside its configs and a picker over a mod's other config files.

use crate::App;
use crate::api::{ApiResult, bad, not_found, require};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post};
use axum::{Json, Router};
use hopper_core::configedit::{self, Section, Value as CValue};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// Config files can be a few MB; the text travels with each edit.
const MAX_BODY: usize = 8 * 1024 * 1024;

/// Endings of config files that are text (desktop's config editor's list).
const TEXT_ENDINGS: [&str; 10] = [".toml", ".json", ".json5", ".cfg", ".properties", ".txt", ".yml", ".yaml", ".snbt", ".ini"];

/// Endings a config's name carries after the mod id (as hopper-core's
/// modconfig reads them).
const SUFFIXES: &[&str] = &[
    "-client", "_client", "-common", "_common", "-server", "_server", "-startup", "_startup", "-config", "_config",
    "-neoforge", "_neoforge", "-forge", "_forge", "-fabric", "_fabric",
];

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
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let version = q.get("version").cloned().unwrap_or_default();
    let text = tokio::task::spawn_blocking(move || crate::files::trash_text(&e.root, &version))
        .await
        .map_err(bad)?
        .map_err(|e| bad(format!("{e:#}")))?;
    Ok(Json(json!({ "content": text })))
}

fn squash(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// The mod id a top-level entry of `config/` is named after, squashed.
fn owner(name: &str, is_dir: bool) -> String {
    let l = name.to_lowercase();
    let mut stem = if is_dir { l.as_str() } else { l.split('.').next().unwrap_or(&l) };
    for s in SUFFIXES {
        stem = stem.strip_suffix(s).unwrap_or(stem);
    }
    squash(stem)
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
        // Squashed mod id -> the jar (name without `.disabled`).
        let mut by_id: HashMap<String, String> = HashMap::new();
        for entry in inside("mods").and_then(|d| std::fs::read_dir(d).ok()).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let lower = name.to_lowercase();
            if !(lower.ends_with(".jar") || lower.ends_with(".jar.disabled")) {
                continue;
            }
            let jar = name.trim_end_matches(".disabled").to_string();
            let (ids, _) = hopper_core::deps::own_ids(&entry.path());
            for i in ids {
                let s = squash(&i);
                if s.len() >= 3 && s != "minecraft" {
                    by_id.entry(s).or_insert_with(|| jar.clone());
                }
            }
        }
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
            let Some(jar) = by_id.get(&owner(&name, is_dir)) else { continue };
            let rel = format!("config/{name}");
            if hopper_core::packdiff::pack_content(&rel) {
                continue;
            }
            owners.insert(name, jar.clone());
            let list = files.entry(jar.clone()).or_default();
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

fn value_json(v: &CValue) -> (&'static str, String) {
    let kind = match v {
        CValue::Bool(_) => "bool",
        CValue::Int(_) => "int",
        CValue::Float(_) => "float",
        CValue::Str(_) => "str",
        CValue::Text(_) => "text",
    };
    (kind, v.text())
}

fn sections_json(sections: &[Section]) -> Value {
    Value::Array(
        sections
            .iter()
            .map(|s| {
                json!({
                    "path": s.path,
                    "comment": s.comment,
                    "entries": s.entries.iter().map(|e| {
                        let (kind, value) = value_json(&e.value);
                        json!({
                            "key": e.key, "kind": kind, "value": value, "comment": e.comment,
                            "range": e.range.map(|(lo, hi)| json!([lo, hi])),
                            "allowed": e.allowed, "default": e.default, "line": e.line,
                        })
                    }).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

#[derive(Deserialize)]
struct ParseBody {
    text: String,
}

/// `{ ok, sections }` for TOML text, or `{ ok: false, error }` when it
/// doesn't read.
async fn parse(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<ParseBody>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let v = tokio::task::spawn_blocking(move || match configedit::parse(&b.text) {
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
        let sections = match configedit::parse(&b.text) {
            Ok(s) => s,
            Err(e) => return fail(e),
        };
        let Some(entry) = sections.iter().flat_map(|s| &s.entries).find(|e| e.path == b.table && e.key == b.key) else {
            return fail(format!("No {} in the file.", b.key));
        };
        let value = match entry.value.parse_like(&b.value) {
            Ok(v) => v,
            Err(e) => return fail(e),
        };
        if let Some(e) = entry.check(&value) {
            return fail(e);
        }
        match configedit::set(&b.text, &b.table, &b.key, &value).and_then(|t| configedit::parse(&t).map(|s| (t, s))) {
            Ok((text, s)) => json!({ "ok": true, "text": text, "sections": sections_json(&s) }),
            Err(e) => fail(format!("{}: {e}", b.key)),
        }
    })
    .await
    .map_err(bad)?;
    Ok(Json(v))
}
