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
        .route("/api/v1/servers/{id}/logs/text", get(text))
        .route("/api/v1/servers/{id}/crashes", get(crashes))
        .route("/api/v1/servers/{id}/crashes/read", get(crash_read))
        .route("/api/v1/servers/{id}/logs/delete", post(delete))
        .route("/api/v1/servers/{id}/logs/clear", post(clear))
}

/// A log's text: its last [`TAIL`] bytes (whole lines), or what was
/// written since `from` (a byte offset an earlier read returned as `to`),
/// for following it. A log that shrank (a new start) reads from the end.
#[derive(Deserialize)]
struct TextQ {
    path: String,
    #[serde(default)]
    from: u64,
}

const TAIL: u64 = 400 << 10;
const APPEND_MAX: u64 = 4 << 20;

async fn text(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Query(q): Query<TextQ>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        use std::io::{Read, Seek, SeekFrom};
        let rel = q.path.replace('\\', "/");
        if !logshare::list(&e.root).iter().any(|f| f.path == rel) {
            anyhow::bail!("That isn't one of this server's logs.");
        }
        if rel.ends_with(".gz") {
            let t = logshare::read(&e.root, &rel)?;
            let cut = t.len().saturating_sub(TAIL as usize);
            let shown = t[t.ceil_char_boundary(cut)..].to_string();
            return Ok(json!({ "text": shown, "to": 0, "size": t.len(), "reset": true }));
        }
        let mut f = std::fs::File::open(e.root.join(&rel))?;
        let size = f.metadata()?.len();
        let (start, reset) = if q.from == 0 || q.from > size || size - q.from > APPEND_MAX { (size.saturating_sub(TAIL), true) } else { (q.from, false) };
        f.seek(SeekFrom::Start(start))?;
        let mut buf = Vec::new();
        f.take(size - start).read_to_end(&mut buf)?;
        // Whole lines only: the rest comes with the next read.
        match buf.iter().rposition(|b| *b == b'\n') {
            Some(i) => buf.truncate(i + 1),
            None => buf.clear(),
        }
        let mut text = String::from_utf8_lossy(&buf).into_owned();
        // A tail starts mid-line: drop that part line.
        if reset && start > 0 && let Some(i) = text.find('\n') {
            text.drain(..=i);
        }
        Ok(json!({ "text": text, "to": start + buf.len() as u64, "size": size, "reset": reset }))
    })
    .await
    .map_err(bad)?
    .map_err(|e| bad(format!("{e:#}")))?;
    Ok(Json(v))
}

/// Verdicts already worked out, by report and its size and time: a crash
/// report doesn't change once written.
type Verdicts = std::collections::HashMap<(std::path::PathBuf, u64, u64), String>;
static VERDICTS: std::sync::LazyLock<std::sync::Mutex<Verdicts>> = std::sync::LazyLock::new(Default::default);

/// How many of the newest reports get a verdict in the list.
const WITH_VERDICT: usize = 25;
/// New verdicts worked out per request: the list comes back at once and
/// says `pending` until the rest are done (the browser asks again).
const VERDICTS_PER_ASK: usize = 3;
/// Servers listed once already: their first list has no verdicts (the
/// first one reads every jar), so it shows at once.
static LISTED: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<std::path::PathBuf>>> = std::sync::LazyLock::new(Default::default);

/// The server's crash reports, newest first, each with Hopper's one-line
/// verdict (the newest few; the rest by name and time).
async fn crashes(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let list = tokio::task::spawn_blocking(move || {
        let mut files: Vec<logshare::LogFile> = logshare::list(&e.root).into_iter().filter(|f| f.crash).collect();
        files.sort_by(|a, b| b.modified.cmp(&a.modified).then(b.path.cmp(&a.path)));
        let mods = e.root.join("mods");
        let seen = !LISTED.lock().unwrap_or_else(|x| x.into_inner()).insert(e.root.clone());
        let mut budget = if seen { VERDICTS_PER_ASK } else { 0 };
        let mut pending = false;
        let list = files
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let p = e.root.join(&f.path);
                let key = (p.clone(), f.size, f.modified);
                let cached = VERDICTS.lock().unwrap_or_else(|x| x.into_inner()).get(&key).cloned();
                let verdict = match cached {
                    Some(v) => Some(v),
                    None if i < WITH_VERDICT && budget == 0 => {
                        pending = true;
                        None
                    }
                    None if i < WITH_VERDICT => {
                        budget -= 1;
                        let v = hopper_core::crashdetect::server_report(&e.root, &p)
                            .map(|c| hopper_core::crashdetect::verdict(&c, &|s| hopper_core::crashdetect::jar_name(&mods, &s.file_name)))
                            .unwrap_or_default();
                        VERDICTS.lock().unwrap_or_else(|x| x.into_inner()).insert(key, v.clone());
                        Some(v)
                    }
                    None => None,
                };
                // Read from the report's own text (the verdict can't tell).
                let follow = i < WITH_VERDICT && f.path.ends_with(".txt") && hopper_core::crashgroup::is_follow_up_text(&hopper_core::logs::tail(&p, 2 << 20));
                json!({ "path": f.path, "size": f.size, "modified": f.modified, "verdict": verdict, "follow": follow })
            })
            .collect::<Vec<_>>();
        // Follow-up reports (their error only came while the loader undid
        // the registries after the start's real one) point at the report
        // that explains them, so the list can fold them under it.
        let mut list = list;
        let extra: Vec<(usize, Value)> = {
            use hopper_core::crashgroup::{Item, Role, follow_up_title, group};
            let items: Vec<Item> = list
                .iter()
                .map(|v| Item { modified: v["modified"].as_i64().unwrap_or(0), follow_up: v["follow"].as_bool().unwrap_or(false), verdict: v["verdict"].as_str() })
                .collect();
            group(&items)
                .into_iter()
                .enumerate()
                .filter_map(|(i, r)| match r {
                    Role::Lead(n) => Some((i, json!({ "follow_ups": n.len() }))),
                    Role::FollowUp(lead) => Some((
                        i,
                        json!({
                            "follow_up": true,
                            "lead": lead.map(|j| list[j]["path"].clone()),
                            "title": follow_up_title(lead.and_then(|j| list[j]["verdict"].as_str())),
                        }),
                    )),
                    Role::Plain => None,
                })
                .collect()
        };
        for (i, e) in extra {
            if let (Some(o), Some(add)) = (list[i].as_object_mut(), e.as_object()) {
                o.extend(add.clone());
            }
        }
        (list, pending)
    })
    .await
    .map_err(bad)?;
    Ok(Json(json!({ "crashes": list.0, "pending": list.1 })))
}

/// One crash report, read like the crash card reads the last one: the
/// mods with how sure, why, and what to do. `names` gives each named jar's
/// name for people.
async fn crash_read(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Query(q): Query<PathQ>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let rel = q.path.replace('\\', "/");
        if !logshare::list(&e.root).iter().any(|f| f.crash && f.path == rel) {
            anyhow::bail!("That isn't one of this server's crash reports.");
        }
        let c = hopper_core::crashdetect::server_report(&e.root, &e.root.join(&rel));
        let mods = e.root.join("mods");
        let mut names = serde_json::Map::new();
        if let Some(c) = &c {
            let files = c.suspects.iter().flat_map(|s| std::iter::once(s.file_name.clone()).chain(s.partner.iter().map(|p| p.file_name.clone())));
            for f in files.chain(c.stopped.iter().flat_map(|st| st.skipped.clone())) {
                names.insert(f.clone(), json!(hopper_core::crashdetect::jar_name(&mods, &f)));
            }
        }
        let lingered = c.as_ref().and_then(|c| c.lingered_note());
        Ok(json!({ "crash": c, "report": rel, "names": names, "lingered": lingered }))
    })
    .await
    .map_err(bad)?
    .map_err(|e| bad(format!("{e:#}")))?;
    Ok(Json(v))
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

#[derive(Deserialize)]
struct DeleteBody {
    paths: Vec<String>,
}

/// Deletes crash reports or logs (to the server's trash): only plain files
/// directly in `crash-reports/` and `logs/`, and never the log a running
/// server is writing.
async fn delete(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<DeleteBody>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let running = e.server.state().is_alive();
    let n = tokio::task::spawn_blocking(move || hopper_core::crashfiles::delete(&e.root, &b.paths, running))
        .await
        .map_err(bad)?
        .map_err(|e| bad(format!("{e:#}")))?;
    Ok(Json(json!({ "deleted": n })))
}

#[derive(Deserialize)]
struct ClearBody {
    /// "crashes" or "logs".
    what: String,
}

/// "Clear all": every crash report, or every log but the current
/// latest.log and debug.log.
async fn clear(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<ClearBody>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let kind = hopper_core::crashfiles::Kind::parse(&b.what).ok_or_else(|| bad("Say what to clear: crashes or logs."))?;
    let n = tokio::task::spawn_blocking(move || hopper_core::crashfiles::clear(&e.root, kind))
        .await
        .map_err(bad)?
        .map_err(|e| bad(format!("{e:#}")))?;
    Ok(Json(json!({ "deleted": n })))
}
