//! Routes added to bring the panel's console area closer to Hopper desktop.
//!
//! The console's "Whole log file" view (desktop's Live / Whole log file
//! switch): this run's `logs/latest.log`, read from the start and then
//! followed by byte offset. Who may read it: the guard middleware checks
//! the signed-in account sees this server (`/api/v1/servers/{id}/…`).

use crate::App;
use crate::api::{ApiResult, bad, not_found, require};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

/// The most of the log sent at once (its end, when it's longer).
const MAX_READ: u64 = 8 << 20;

pub fn routes() -> Router<Arc<App>> {
    Router::new().route("/api/v1/servers/{id}/console/log", get(log_file))
}

#[derive(Deserialize)]
struct From {
    /// Where the last read ended (0: from the start).
    #[serde(default)]
    from: u64,
}

/// `{ text, end, reset, cut }`: the lines after `from` (whole lines only),
/// where they end, whether the file started over (a new run) so the view
/// should drop what it has, and whether the start was left out.
async fn log_file(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Query(q): Query<From>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let root = e.root.clone();
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        // Inside the server folder, also when logs is a link (files::resolve
        // follows links and refuses one that leads out).
        let path = crate::files::resolve(&root, "logs/latest.log")?;
        let Ok(mut f) = std::fs::File::open(&path) else {
            return Ok(json!({ "text": "", "end": 0, "reset": q.from > 0, "cut": false, "missing": true }));
        };
        let len = f.metadata()?.len();
        // A new run truncates the file: read it again from the start. A run
        // that already wrote past `from` is caught too: every read ends on
        // a whole line, so the byte before `from` must be a newline.
        let mut reset = q.from == 0 || q.from > len;
        if !reset {
            let mut b = [0u8; 1];
            f.seek(SeekFrom::Start(q.from - 1))?;
            reset = f.read_exact(&mut b).is_err() || b[0] != b'\n';
        }
        let mut start = if reset { 0 } else { q.from };
        let cut = len - start > MAX_READ;
        if cut {
            start = len - MAX_READ;
        }
        f.seek(SeekFrom::Start(start))?;
        let mut buf = Vec::with_capacity((len - start) as usize);
        f.take(len - start).read_to_end(&mut buf)?;
        // Only whole lines: the last one may still be being written.
        let keep = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        buf.truncate(keep);
        let mut text = String::from_utf8_lossy(&buf).into_owned();
        if cut {
            // Starts mid-line: drop the partial first line.
            text = text.split_once('\n').map(|(_, r)| r.to_string()).unwrap_or_default();
        }
        Ok(json!({ "text": text, "end": start + keep as u64, "reset": reset || cut, "cut": cut, "missing": false }))
    })
    .await
    .map_err(bad)?
    .map_err(|e| bad(format!("{e:#}")))?;
    Ok(Json(v))
}
