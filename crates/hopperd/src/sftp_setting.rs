//! The SFTP switch in Settings › Access & devices. SFTP is off on a new
//! install: nothing listens on port 8022 until the owner switches it on
//! here. The switch works live (no restart); switching off also ends the
//! connections that are open.
//!
//! `GET /api/v1/settings/sftp` is the state, `PUT` switches it (owner or a
//! manager, and like the other "who can get in" settings only from the
//! panel's own page unless the owner allowed other pages to manage access:
//! see `web_access::manages_access`).

use crate::App;
use crate::api::{ApiResult, bad, require_owner};
use crate::config::DEFAULT_SFTP_PORT;
use axum::extract::State;
use axum::http::{HeaderMap, Method};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub const PATH: &str = "/api/v1/settings/sftp";

pub fn routes() -> Router<Arc<App>> {
    Router::new().route(PATH, get(get_state).put(put_state))
}

fn view(app: &App) -> Value {
    let port = app.cfg.read().unwrap().sftp_port;
    json!({ "on": port != 0, "port": if port != 0 { port } else { DEFAULT_SFTP_PORT }, "fingerprint": app.sftp_fingerprint })
}

async fn get_state(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    Ok(Json(view(&app)))
}

#[derive(Deserialize)]
struct Body {
    on: bool,
    /// The port to listen on when switching on (default 8022). The web UI
    /// doesn't send it; it is for scripts and for a port that is taken.
    port: Option<u16>,
}

async fn put_state(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<Body>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let (port, bind) = {
        let cfg = app.cfg.read().unwrap();
        (cfg.sftp_port, cfg.bind.clone())
    };
    let want = b.port.unwrap_or(DEFAULT_SFTP_PORT);
    if b.on && want == 0 {
        return Err(bad("Pick a port above 0, or switch SFTP off."));
    }
    if b.on && port == 0 {
        // Bind before saving, so a port that is taken is told to the owner
        // now and the setting doesn't say "on" for nothing.
        let listener = crate::sftp::bind(&bind, want)
            .await
            .map_err(|e| {
                log::warn!("SFTP: can't listen on port {want}: {e}");
                bad(format!("Couldn't switch SFTP on: port {want} is already in use by another program. To use another port, set sftp_port in hopperd.toml."))
            })?;
        {
            let mut cfg = app.cfg.write().unwrap();
            cfg.sftp_port = want;
            cfg.save(&app.data_dir).map_err(|e| bad(format!("{e:#}")))?;
        }
        tokio::spawn(crate::sftp::serve_on(app.clone(), listener, want));
        log::info!("SFTP switched on (port {want})");
    } else if !b.on && port != 0 {
        {
            let mut cfg = app.cfg.write().unwrap();
            cfg.sftp_port = 0;
            cfg.save(&app.data_dir).map_err(|e| bad(format!("{e:#}")))?;
        }
        crate::sftp::stop();
        log::info!("SFTP switched off");
    }
    Ok(Json(view(&app)))
}
