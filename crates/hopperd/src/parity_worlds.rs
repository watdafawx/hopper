//! Routes added to bring the panel's worlds area closer to Hopper desktop:
//! a world's data packs (listed with the worlds, removed to the server's
//! trash here), as desktop's world cards show them.

use crate::App;
use crate::api::{ApiResult, bad, not_found, require};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method};
use axum::routing::delete;
use axum::{Json, Router};
use hopper_core::hosting::worlds;
use serde_json::{Value, json};
use std::sync::Arc;

pub fn routes() -> Router<Arc<App>> {
    Router::new().route("/api/v1/servers/{id}/worlds/{name}/datapacks/{file}", delete(remove_pack))
}

/// One data pack in a world's `datapacks/`.
#[derive(serde::Serialize)]
pub struct Pack {
    pub name: String,
    pub size: u64,
    pub dir: bool,
}

/// A world's data packs (zips, jars and folders; switched-off ones too),
/// without Hopper's own files, as desktop lists them.
pub fn datapacks(dir: &std::path::Path) -> Vec<Pack> {
    hopper_core::instance_ops::datapacks(dir)
        .into_iter()
        .filter_map(|p| {
            let name = p.file_name()?.to_string_lossy().into_owned();
            if name.to_ascii_lowercase().starts_with(".hopper-") {
                return None;
            }
            let dir = p.is_dir();
            let size = if dir { 0 } else { std::fs::metadata(&p).map_or(0, |m| m.len()) };
            Some(Pack { name, size, dir })
        })
        .collect()
}

/// One file or folder name in `datapacks/`: no separators, no `.` / `..`,
/// no drive or stream (`:`), no control characters, and not Hopper's own
/// `.hopper-*` files (the list hides them, so they can't be meant).
pub fn check_pack_name(file: &str) -> Result<(), &'static str> {
    let bad = file.is_empty()
        || file.len() > 255
        || file == "."
        || file == ".."
        || file.chars().any(|c| matches!(c, '/' | '\\' | ':') || c.is_control())
        || file.to_ascii_lowercase().starts_with(".hopper-");
    if bad { Err("That isn't a data pack's name.") } else { Ok(()) }
}

/// Moves a world's data pack to the server's trash (Files › Trash).
async fn remove_pack(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path((id, name, file)): Path<(String, String, String)>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    worlds::check_name(&name).map_err(bad)?;
    check_pack_name(&file).map_err(bad)?;
    if !e.root.join(&name).is_dir() {
        return Err(not_found("world"));
    }
    let rel = format!("{name}/datapacks/{file}");
    log::info!("{}: data pack {rel} to the trash", e.name);
    let root = e.root.clone();
    tokio::task::spawn_blocking(move || crate::files::trash(&root, &rel))
        .await
        .map_err(bad)?
        .map_err(|err| bad(format!("{err:#}")))?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::check_pack_name;

    #[test]
    fn datapack_names() {
        for ok in ["VanillaTweaks.zip", "my pack", "terralith-2.5.jar", "..hidden.zip", "a.b"] {
            assert!(check_pack_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", ".", "..", "../level.dat", "a/b.zip", r"a\b", "C:x.zip", "p.zip:stream", "x\u{0}y", ".hopper-meta.json", ".HOPPER-x"] {
            assert!(check_pack_name(bad).is_err(), "{bad:?}");
        }
        assert!(check_pack_name(&"a".repeat(256)).is_err());
    }
}
