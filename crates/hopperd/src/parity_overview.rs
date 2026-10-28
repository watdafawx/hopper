//! Routes added to bring the panel's overview area closer to Hopper desktop.
//!
//! The title bar's Tasks button: `GET /jobs` lists the panel's background
//! jobs (pack updates, comparisons) like desktop's task list, and a finished
//! one can be dismissed. Cancelling uses the existing `/jobs/{job}/cancel`.
//! Dismissing never takes a result from a page that is still following
//! the job: see [`Seen`].
//! Owner only: a job's title names its server, and other users may not see
//! every server.

use crate::App;
use crate::api::{ApiResult, not_found, require_owner};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/jobs", get(list))
        .route("/api/v1/jobs/clear-finished", post(clear_finished))
        .route("/api/v1/jobs/{job}/dismiss", post(dismiss))
}

/// When each finished job was first seen done, and the ones the owner
/// dismissed or cleared. A page following a job (`/jobs/{job}`) takes its
/// result and drops it from the list itself, a second or so after it ends;
/// so a dismissed job only leaves the list for now, and is really removed
/// (with any result nobody fetched) once it has been done for `GRACE`.
struct Seen {
    done_at: HashMap<u64, Instant>,
    hidden: HashSet<u64>,
}

static SEEN: Mutex<Option<Seen>> = Mutex::new(None);
const GRACE: Duration = Duration::from_secs(30);

/// Notes newly finished jobs, removes hidden ones past the grace time, and
/// returns the jobs to show.
fn sweep(app: &App) -> Vec<hopper_core::jobs::JobState> {
    let jobs = app.jobs.snapshot();
    let mut g = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let seen = g.get_or_insert_with(|| Seen { done_at: HashMap::new(), hidden: HashSet::new() });
    let ids: HashSet<u64> = jobs.iter().map(|j| j.id).collect();
    // Jobs a page took (or that went away otherwise) are forgotten here too.
    seen.done_at.retain(|id, _| ids.contains(id));
    seen.hidden.retain(|id| ids.contains(id));
    let now = Instant::now();
    for j in jobs.iter().filter(|j| j.done) {
        seen.done_at.entry(j.id).or_insert(now);
    }
    let gone: Vec<u64> = seen
        .hidden
        .iter()
        .copied()
        .filter(|id| seen.done_at.get(id).is_some_and(|t| now.duration_since(*t) >= GRACE))
        .collect();
    for id in &gone {
        app.jobs.dismiss(*id);
        app.results.lock().unwrap().remove(id);
        seen.hidden.remove(id);
        seen.done_at.remove(id);
    }
    jobs.into_iter().filter(|j| !gone.contains(&j.id) && !seen.hidden.contains(&j.id)).collect()
}

/// Hides finished jobs from the list (see [`Seen`]); running ones are left.
fn hide(app: &App, which: impl Fn(&hopper_core::jobs::JobState) -> bool) -> usize {
    let jobs = sweep(app);
    let mut g = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(seen) = g.as_mut() else { return 0 };
    let mut n = 0;
    for j in jobs.iter().filter(|j| j.done && which(j)) {
        seen.hidden.insert(j.id);
        n += 1;
    }
    drop(g);
    // Ones already past the grace time go now.
    sweep(app);
    n
}

async fn list(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let jobs: Vec<Value> = sweep(&app)
        .into_iter()
        .map(|j| {
            json!({
                "id": j.id, "title": j.title, "detail": j.detail, "progress": j.progress,
                "done": j.done, "error": j.error, "notes": j.notes,
            })
        })
        .collect();
    Ok(Json(json!(jobs)))
}

/// Takes a finished job off the list (a running one is cancelled with
/// `/jobs/{job}/cancel` instead). A page still following it gets its result.
async fn dismiss(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(job): Path<u64>,
) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let st = app.jobs.snapshot().into_iter().find(|j| j.id == job).ok_or_else(|| not_found("job"))?;
    if st.done {
        hide(&app, |j| j.id == job);
    }
    Ok(Json(json!({ "ok": st.done })))
}

/// Takes every finished job off the list, failed ones too. Running jobs
/// stay; a page following a finished one still gets its result.
async fn clear_finished(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let n = hide(&app, |_| true);
    Ok(Json(json!({ "cleared": n })))
}

#[cfg(test)]
mod tests {
    /// The job routes sit beside pack_api's `/jobs/{job}` ones without a clash.
    #[test]
    fn routes_merge_with_pack_jobs() {
        let _ = crate::pack_api::routes().merge(super::routes());
    }
}
