//! Dropping a jar onto a server's Mods tab: `POST /api/v1/servers/{id}/mods/upload?name=x.jar`.
//! The body is the file itself (one request per file, so the browser can show
//! progress). It is written into the server's `mods/` folder, as desktop does
//! for a jar dropped on an instance; the name is cleaned first so nothing can
//! land outside that folder.

use crate::api::{ApiError, ApiResult, bad};
use crate::App;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path as FsPath;
use std::sync::Arc;
use tokio::io::AsyncWriteExt as _;
use tokio_stream::StreamExt as _;

/// Largest jar accepted. Big mods are well under this.
pub const MAX_JAR: usize = 512 * 1024 * 1024;

pub fn routes() -> Router<Arc<App>> {
    Router::new().route("/api/v1/servers/{id}/mods/upload", post(upload).layer(DefaultBodyLimit::max(MAX_JAR)))
}

/// A file name that is safe to create in `mods/`: just a name (any folder
/// part the browser sent is dropped), ending in ".jar", with nothing
/// Windows refuses.
pub fn clean_name(raw: &str) -> Result<String, String> {
    let name = raw.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if name.is_empty() || name == "." || name == ".." {
        return Err("The file has no name.".into());
    }
    if name.chars().any(|c| c.is_control() || "<>:\"|?*".contains(c)) {
        return Err(format!("{name}: the name has characters a file can't have."));
    }
    if name.ends_with('.') || name.len() > 200 {
        return Err(format!("{name}: the name is not usable."));
    }
    if !name.to_ascii_lowercase().ends_with(".jar") {
        return Err(format!("{name}: only .jar files go into mods."));
    }
    // Names Windows keeps for devices ("con.jar" opens the console).
    let stem = name.split('.').next().unwrap_or("").to_ascii_lowercase();
    let reserved = ["con", "prn", "aux", "nul"].contains(&stem.as_str())
        || ["com", "lpt"].iter().any(|p| stem.strip_prefix(p).is_some_and(|n| n.len() == 1 && n != "0" && n.chars().all(|c| c.is_ascii_digit())));
    if reserved {
        return Err(format!("{name}: Windows keeps that name."));
    }
    Ok(name.to_string())
}

/// Where `name` goes inside `mods_dir`, checked to stay in it.
pub fn dest_in(mods_dir: &FsPath, name: &str) -> Result<std::path::PathBuf, String> {
    let name = clean_name(name)?;
    let dest = mods_dir.join(&name);
    if dest.parent() != Some(mods_dir) {
        return Err("That name points outside the mods folder.".into());
    }
    Ok(dest)
}

fn err(code: StatusCode, msg: impl Into<String>) -> ApiError {
    ApiError(code, msg.into())
}

async fn upload(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    body: Body,
) -> ApiResult<Json<Value>> {
    let e = crate::mods_api::entry(&app, &headers, &method, &id)?;
    let name = clean_name(q.get("name").map_or("", |s| s.as_str())).map_err(bad)?;
    let replace = q.get("replace").is_some_and(|v| v == "1");
    let mods = e.root.join("mods");
    let dest = dest_in(&mods, &name).map_err(bad)?;
    if dest.exists() && !replace {
        return Err(err(StatusCode::CONFLICT, format!("{name} is already in mods.")));
    }
    tokio::fs::create_dir_all(&mods).await.map_err(bad)?;
    // Written beside the target and moved in when whole, so a dropped
    // connection never leaves half a jar the server would try to load.
    let part = mods.join(format!(".{name}.upload"));
    let mut size = 0u64;
    let mut head: Vec<u8> = Vec::with_capacity(4);
    let written: Result<(), String> = async {
        let mut f = tokio::fs::File::create(&part).await.map_err(|x| x.to_string())?;
        let mut s = body.into_data_stream();
        while let Some(chunk) = s.next().await {
            let chunk = chunk.map_err(|x| x.to_string())?;
            if head.len() < 4 {
                head.extend(chunk.iter().take(4 - head.len()));
            }
            size += chunk.len() as u64;
            f.write_all(&chunk).await.map_err(|x| x.to_string())?;
        }
        f.flush().await.map_err(|x| x.to_string())?;
        Ok(())
    }
    .await;
    if let Err(m) = written {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(bad(format!("{name}: the upload broke off ({m}).")));
    }
    // A jar is a zip file.
    if size == 0 || head != b"PK\x03\x04" {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(bad(format!("{name} isn't a jar file.")));
    }
    let (root, dest2, part2) = (e.root.clone(), dest.clone(), part.clone());
    let replaced = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
        let mut replaced = false;
        // The old file with that name is kept aside, as desktop does.
        if dest2.exists() {
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let keep = root.join(".hopper-backups").join(stamp.to_string()).join("mods");
            std::fs::create_dir_all(&keep)?;
            std::fs::rename(&dest2, keep.join(dest2.file_name().unwrap_or_default()))?;
            replaced = true;
        }
        std::fs::rename(&part2, &dest2)?;
        Ok(replaced)
    })
    .await
    .map_err(bad)?
    .map_err(|x| bad(format!("{x:#}")))?;
    // The jar is new: the last update check doesn't know it.
    crate::mods_api::forget_updates(&e.id);
    // TODO(server-live-sync): broadcast the mod-changed event here once
    // claude/distracted-banach-512d85 is on main.
    Ok(Json(json!({ "name": name, "size": size, "replaced": replaced, "running": e.server.state().is_alive() })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_jar_names_pass() {
        assert_eq!(clean_name("jei-1.21.1-19.jar").unwrap(), "jei-1.21.1-19.jar");
        assert_eq!(clean_name("Some Mod [fabric].JAR").unwrap(), "Some Mod [fabric].JAR");
    }

    #[test]
    fn folders_are_dropped_from_names() {
        assert_eq!(clean_name("../../evil.jar").unwrap(), "evil.jar");
        assert_eq!(clean_name("..\\..\\evil.jar").unwrap(), "evil.jar");
        assert_eq!(clean_name("C:\\Users\\me\\a.jar").unwrap(), "a.jar");
        assert_eq!(clean_name("/etc/a.jar").unwrap(), "a.jar");
    }

    #[test]
    fn bad_names_are_refused() {
        for n in ["", ".", "..", "a/..", "x.zip", "x.jar.exe", "a:b.jar", "a\0.jar", "con.jar", "NUL.jar", "com1.jar", "lpt9.x.jar", "x.jar."] {
            assert!(clean_name(n).is_err(), "{n:?} should be refused");
        }
        assert!(clean_name(&format!("{}.jar", "a".repeat(250))).is_err());
        // Not reserved: a name that only starts like one.
        assert!(clean_name("console.jar").is_ok());
        assert!(clean_name("com0.jar").is_ok());
    }

    #[test]
    fn destination_stays_in_mods() {
        let mods = std::path::Path::new("srv/mods");
        for n in ["a.jar", "../a.jar", "..\\..\\a.jar", "sub/a.jar"] {
            let d = dest_in(mods, n).unwrap();
            assert_eq!(d.parent(), Some(mods));
        }
        assert!(dest_in(mods, "../../x.txt").is_err());
    }
}
