//! Backups a backup mod made inside a server's own folder (SimpleBackups,
//! FTB Backups, Advanced Backups, Textile Backup, or a plain `backups` folder
//! left behind): list, restore, remove and download them. The same ones
//! Hopper desktop's server Backups tab shows, for the panel.

use crate::App;
use crate::api::{ApiError, ApiResult, bad, not_found, require, require_owner};
use crate::servers::Entry;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use hopper_core::backupmods;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/servers/{id}/backupmods", get(list))
        .route("/api/v1/servers/{id}/backupmods/restore", post(restore))
        .route("/api/v1/servers/{id}/backupmods/remove", post(remove))
        .route("/api/v1/servers/{id}/backupmods/download", get(download))
}

fn entry(app: &App, headers: &HeaderMap, method: &Method, id: &str) -> ApiResult<Arc<Entry>> {
    require(app, headers, method)?;
    app.servers.get(id).ok_or_else(|| not_found("server"))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(bad)?.map_err(|e| bad(format!("{e:#}")))
}

fn conflict(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::CONFLICT, msg.into())
}

/// What the panel calls a backup: the mod's name and the file's path under
/// its folder. Never a path Hopper then trusts: both are looked up again.
#[derive(Deserialize)]
struct Pick {
    #[serde(rename = "mod")]
    module: String,
    name: String,
}

impl Pick {
    fn lookup(&self, root: std::path::PathBuf) -> impl FnOnce() -> anyhow::Result<backupmods::BackupFile> + Send + 'static {
        let (module, name) = (self.module.clone(), self.name.clone());
        move || {
            backupmods::find(&root, &module, &name)
                .ok_or_else(|| anyhow::anyhow!("That backup is gone. Refresh the list."))
        }
    }
}

async fn list(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let owner = require_owner(&app, &headers, &method).is_ok();
    let root = e.root.clone();
    let v = blocking(move || {
        let mods: Vec<Value> = backupmods::detect(&root)
            .into_iter()
            .map(|m| {
                // Inside the server: the panel shows a short, readable path.
                let dir = m
                    .dir
                    .strip_prefix(&root)
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|_| m.dir.to_string_lossy().into_owned());
                let files: Vec<Value> = m
                    .files
                    .iter()
                    .map(|f| {
                        json!({
                            "name": f.name,
                            "world": backupmods::world_of(f),
                            "size": f.size,
                            "modified": f.modified,
                            "restorable": f.restorable(),
                            "partial": f.partial(),
                        })
                    })
                    .collect();
                json!({ "mod": m.mod_name, "dir": dir, "command": m.command, "files": files })
            })
            .collect();
        Ok(json!({ "mods": mods, "can_remove": owner }))
    })
    .await?;
    Ok(Json(v))
}

async fn restore(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(p): Json<Pick>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    if e.server.state().is_alive() {
        return Err(conflict(format!("Stop {} before restoring a backup.", e.name)));
    }
    let file = blocking(p.lookup(e.root.clone())).await?;
    let Some(world) = backupmods::world_of(&file).map(str::to_string) else {
        return Err(conflict(
            "That backup isn't in a world folder, so Hopper can't tell which world it restores. Restore it with the mod.",
        ));
    };
    let guard = crate::backups_api::Restoring::start(&e.id)
        .ok_or_else(|| conflict(format!("{} is already being restored.", e.name)))?;
    log::info!("{}: restoring {} over {world}", e.name, file.name);
    let root = e.root.clone();
    let world_dir = root.join(&world);
    let shown = file.name.clone();
    let who = crate::mods_api::changer(&app, &headers, &method);
    let job = crate::pack_api::spawn(&app, format!("Restoring {}", e.name), move |_job| {
        let _guard = guard;
        let kept = backupmods::restore_by(&root, &world_dir, &file, &who).inspect_err(|err| {
            log::warn!("{}", format!("restore of {shown} failed: {err:#}"));
        })?;
        Ok(json!({ "ok": true, "world": world, "kept": kept.to_string_lossy() }))
    });
    Ok(Json(json!({ "job": job })))
}

async fn remove(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(p): Json<Pick>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    require_owner(&app, &headers, &method)?;
    let root = e.root.clone();
    let name = p.name.clone();
    let who = crate::mods_api::changer(&app, &headers, &method);
    let gone = blocking(move || {
        let f = p.lookup(root.clone())()?;
        backupmods::remove_by(&root, &f, &who)
    })
    .await?;
    log::info!("{}: removed backup {name}, kept at {}", e.name, gone.display());
    Ok(Json(json!({ "ok": true, "kept": gone.to_string_lossy() })))
}

#[derive(Deserialize)]
struct PickQuery {
    #[serde(rename = "mod")]
    module: String,
    name: String,
}

async fn download(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Query(q): Query<PickQuery>) -> ApiResult<Response> {
    let e = entry(&app, &headers, &method, &id)?;
    let pick = Pick { module: q.module, name: q.name };
    let file = blocking(pick.lookup(e.root.clone())).await?;
    let fname = file
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "backup.zip".into());
    let path = file.path.clone();
    log::info!("{}: downloading {}", e.name, file.name);
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    tokio::task::spawn_blocking(move || {
        let mut w = crate::fs_api::ChanWriter { tx: tx.clone(), buf: Vec::with_capacity(256 * 1024) };
        let res = (|| -> std::io::Result<()> {
            let mut f = std::fs::File::open(&path)?;
            std::io::copy(&mut f, &mut w)?;
            std::io::Write::flush(&mut w)
        })();
        if let Err(err) = res
            && err.kind() != std::io::ErrorKind::BrokenPipe
        {
            log::warn!("backup download: {err}");
            let _ = tx.blocking_send(Err(err));
        }
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "application/zip")
        .header(header::CONTENT_DISPOSITION, crate::fs_api::disposition(&fname, false))
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx)))
        .map_err(bad)
}
