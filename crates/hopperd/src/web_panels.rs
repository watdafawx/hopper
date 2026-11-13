//! The other panels a person added to this panel's web UI (multi-panel B2):
//! each one's address and name, per account, so a second browser shows the
//! same groups. Never a token or a password: those stay in the browser that
//! signed in (design/multi-panel.md, "Adding a panel").

use crate::App;
use crate::api::{ApiError, ApiResult, bad, require};
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// More than anyone runs; it keeps the file small.
const MAX_PANELS: usize = 32;
const MAX_NAME: usize = 64;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Panel {
    /// The panel's origin (`http://rig:8790`).
    pub url: String,
    #[serde(default)]
    pub name: String,
}

static LOCK: Mutex<()> = Mutex::new(());

fn file(app: &App) -> std::path::PathBuf {
    app.data_dir.join("web-panels.json")
}

fn load(path: &std::path::Path) -> HashMap<String, Vec<Panel>> {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save(path: &std::path::Path, all: &HashMap<String, Vec<Panel>>) -> anyhow::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(all)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Checks and tidies a list: every address an origin, no repeats, names
/// short and plain, not too many.
pub fn tidy(list: &[Panel]) -> Result<Vec<Panel>, String> {
    let mut out: Vec<Panel> = Vec::new();
    for p in list {
        let url = crate::web_access::normalize(&p.url)?;
        if out.iter().any(|q| q.url == url) {
            continue;
        }
        let name: String = p.name.chars().filter(|c| !c.is_control()).take(MAX_NAME).collect::<String>().trim().to_string();
        out.push(Panel { url, name });
    }
    if out.len() > MAX_PANELS {
        return Err(format!("At most {MAX_PANELS} panels."));
    }
    Ok(out)
}

/// The key a person's list is kept under: the owner, or a user's id.
/// Devices and API tokens have no list.
fn key(app: &App, headers: &HeaderMap, method: &Method) -> ApiResult<String> {
    let who = require(app, headers, method)?;
    match who.account() {
        Some(None) => Ok("owner".into()),
        Some(Some(id)) => Ok(format!("user:{id}")),
        None => Err(ApiError(StatusCode::FORBIDDEN, "Only people signed in with a password have a panel list.".into())),
    }
}

pub fn routes() -> Router<Arc<App>> {
    Router::new().route("/api/v1/auth/me/panels", get(get_panels).put(put_panels))
}

async fn get_panels(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    let k = key(&app, &headers, &method)?;
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    Ok(Json(json!(load(&file(&app)).remove(&k).unwrap_or_default())))
}

#[derive(Deserialize)]
struct Body {
    panels: Vec<Panel>,
}

async fn put_panels(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<Body>) -> ApiResult<Json<Value>> {
    let k = key(&app, &headers, &method)?;
    let list = tidy(&b.panels).map_err(bad)?;
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = file(&app);
    let mut all = load(&path);
    if list.is_empty() {
        all.remove(&k);
    } else {
        all.insert(k, list.clone());
    }
    save(&path, &all).map_err(|e| bad(format!("{e:#}")))?;
    Ok(Json(json!(list)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(url: &str, name: &str) -> Panel {
        Panel { url: url.into(), name: name.into() }
    }

    #[test]
    fn tidy_normalizes_and_dedups() {
        let out = tidy(&[p("HTTP://Rig:8790/", " rig "), p("http://rig:8790", "again"), p("https://vm:443", "vm")]).unwrap();
        assert_eq!(out, vec![p("http://rig:8790", "rig"), p("https://vm", "vm")]);
    }

    #[test]
    fn tidy_refuses_paths_and_junk() {
        assert!(tidy(&[p("http://rig:8790/api", "")]).is_err());
        assert!(tidy(&[p("rig:8790", "")]).is_err());
        assert!(tidy(&[p("javascript:alert(1)", "")]).is_err());
        assert!(tidy(&[p("http://user:pw@rig", "")]).is_err());
    }

    #[test]
    fn tidy_caps_names_and_count() {
        let long = "x".repeat(200);
        assert_eq!(tidy(&[p("http://rig", &format!("a\u{7}b{long}"))]).unwrap()[0].name.chars().count(), MAX_NAME);
        let many: Vec<Panel> = (1..=MAX_PANELS + 1).map(|i| p(&format!("http://rig:{i}"), "")).collect();
        assert!(tidy(&many).is_err());
        assert!(tidy(&many[..MAX_PANELS]).is_ok());
    }
}
