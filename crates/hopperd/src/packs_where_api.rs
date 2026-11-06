//! A server's data packs as one list, as desktop's Data packs tab shows an
//! instance's: every place a pack can be (Paxi, Global Packs, OpenLoader,
//! each world) folded into one entry per pack, in a group (Always on, In
//! worlds, Off), with Where to change it and Paxi's load order. The work is
//! `hopper_core::pack_where`'s: nothing is deleted, a copy that stops
//! counting is renamed ".disabled".

use crate::App;
use crate::api::{ApiResult, bad, not_found, require};
use crate::servers::Entry;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post};
use axum::{Json, Router};
use hopper_core::cf::CLASS_DATAPACKS;
use hopper_core::instances::Instance;
use hopper_core::pack_where::{self as pw, Target};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/servers/{id}/datapacks", get(list))
        .route("/api/v1/servers/{id}/datapacks/where", post(set_where))
        .route("/api/v1/servers/{id}/datapacks/order", post(set_order))
}

fn entry(app: &App, headers: &HeaderMap, method: &Method, id: &str) -> ApiResult<Arc<Entry>> {
    require(app, headers, method)?;
    app.servers.get(id).ok_or_else(|| not_found("server"))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(bad)?.map_err(|e| bad(format!("{e:#}")))
}

/// The server folder as the shared code's "instance".
fn inst(root: &std::path::Path) -> Instance {
    Instance { root: root.to_path_buf(), ..Default::default() }
}

/// The list without paths on disk: keys, labels, groups (the shape is
/// `pack_where::to_panel`'s, which desktop reads back with `from_panel`).
fn listing(root: &std::path::Path) -> Value {
    let locs = pw::server_locations(root);
    pw::to_panel(root, &pw::list(&inst(root), CLASS_DATAPACKS, &locs), &locs)
}

async fn list(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let running = e.server.state().is_alive();
    let root = e.root.clone();
    let mut v = blocking(move || Ok(listing(&root))).await?;
    v["running"] = json!(running);
    Ok(Json(v))
}

#[derive(Deserialize)]
struct WhereBody {
    file: String,
    to: Target,
}

async fn set_where(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<WhereBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    crate::parity_worlds::check_pack_name(&b.file).map_err(bad)?;
    let running = e.server.state().is_alive();
    let (root, name) = (e.root.clone(), e.name.clone());
    blocking(move || {
        let locs = pw::server_locations(&root);
        let i = inst(&root);
        let Some(entry) = pw::list(&i, CLASS_DATAPACKS, &locs).into_iter().find(|x| x.file.eq_ignore_ascii_case(&b.file)) else {
            anyhow::bail!("{} isn't one of this server's data packs.", b.file);
        };
        let steps = pw::plan(&entry, CLASS_DATAPACKS, &b.to, &locs);
        // Every path comes from the server's own folders; check anyway.
        let inside = |p: &std::path::Path| p.starts_with(&root);
        for s in &steps {
            let ok = match s {
                pw::Step::Enable { path, .. } => inside(path),
                pw::Step::CopyInto { src, dir } | pw::Step::MoveInto { src, dir } => inside(src) && inside(dir),
                _ => true,
            };
            if !ok {
                anyhow::bail!("That isn't inside the server's folder.");
            }
        }
        log::info!("{name}: data pack {} -> {:?}", entry.file, b.to);
        pw::apply(&i, &steps)?;
        Ok(())
    })
    .await?;
    Ok(Json(json!({ "ok": true, "reload": running })))
}

#[derive(Deserialize)]
struct OrderBody {
    names: Vec<String>,
}

async fn set_order(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<OrderBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    for n in &b.names {
        crate::parity_worlds::check_pack_name(n).map_err(bad)?;
    }
    let running = e.server.state().is_alive();
    let root = e.root.clone();
    blocking(move || pw::set_paxi_order(&inst(&root), CLASS_DATAPACKS, &b.names)).await?;
    Ok(Json(json!({ "ok": true, "reload": running })))
}
