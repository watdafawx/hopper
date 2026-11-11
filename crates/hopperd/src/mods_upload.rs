//! Dropping a jar onto a server's Mods tab: `POST /api/v1/servers/{id}/mods/upload?name=x.jar`.
//! The body is the file itself (one request per file, so the browser can show
//! progress). It is written into the server's `mods/` folder, as desktop does
//! for a jar dropped on an instance; the name is cleaned first so nothing can
//! land outside that folder.
//!
//! With `staged=1` the body is empty: the jar was already sent in resumable
//! chunks (the file routes' uploads) to `mods/.{name}.upload`, the place this
//! route writes it anyway, and only the rest happens here. Desktop sends big
//! jars that way so a dropped connection costs one chunk, not the whole jar.

use crate::api::{ApiResult, bad};
use crate::App;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::routing::post;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use hopper_core::install;
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

async fn upload(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    body: Body,
) -> ApiResult<Response> {
    let e = crate::mods_api::entry(&app, &headers, &method, &id)?;
    let name = clean_name(q.get("name").map_or("", |s| s.as_str())).map_err(bad)?;
    // What to do when the jar is a mod the server has already: "ask" (the
    // default) answers 409 with what's there, "replace" or "both" go on.
    let on_conflict = q.get("conflict").map_or("ask", |s| s.as_str()).to_string();
    let mods = e.root.join("mods");
    let dest = dest_in(&mods, &name).map_err(bad)?;
    tokio::fs::create_dir_all(&mods).await.map_err(bad)?;
    // Written beside the target and moved in when whole, so a dropped
    // connection never leaves half a jar the server would try to load.
    let part = mods.join(format!(".{name}.upload"));
    let mut size = 0u64;
    let mut head: Vec<u8> = Vec::with_capacity(4);
    let staged = q.get("staged").is_some_and(|v| v == "1");
    let written: Result<(), String> = if staged {
        // Sent before in chunks: read what arrived instead of the body.
        async {
            use tokio::io::AsyncReadExt as _;
            let mut f = tokio::fs::File::open(&part).await.map_err(|_| "the jar hadn't arrived; send it again".to_string())?;
            size = f.metadata().await.map_err(|x| x.to_string())?.len();
            let mut b = [0u8; 4];
            let n = f.read(&mut b).await.map_err(|x| x.to_string())?;
            head.extend_from_slice(&b[..n]);
            Ok(())
        }
        .await
    } else {
        async {
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
        .await
    };
    if let Err(m) = written {
        let _ = tokio::fs::remove_file(&part).await;
        log::warn!("mod upload {name} to server {id}: {m}");
        return Err(bad(format!("{name}: the upload broke off ({m}).")));
    }
    // A jar is a zip file.
    if size == 0 || head != b"PK\x03\x04" {
        let _ = tokio::fs::remove_file(&part).await;
        log::warn!("mod upload {name} to server {id}: not a jar ({size} bytes{})", if staged { ", sent in chunks" } else { "" });
        return Err(bad(format!("{name} isn't a jar file.")));
    }
    let (e2, dest2, part2, name2) = (e.clone(), dest.clone(), part.clone(), name.clone());
    let done = tokio::task::spawn_blocking(move || -> anyhow::Result<Result<bool, Value>> {
        let mods = dest2.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let new = hopper_core::scan::scan_file(&part2);
        let new_version = if new.version.is_empty() { hopper_core::scan::version_from_file_name(&name2) } else { new.version.clone() };
        // The same mod already on the server: by its id from the jar, not
        // by file name (a new version usually has another name); the same
        // file name is the same mod whatever the jar says.
        let found: Vec<_> = hopper_core::scan::scan_folder(&mods)?
            .into_iter()
            .filter(|m| m.file_name.eq_ignore_ascii_case(&name2) || (!new.mod_id.is_empty() && m.mod_id == new.mod_id))
            .collect();
        let same_name = found.iter().any(|m| m.file_name.eq_ignore_ascii_case(&name2));
        if !found.is_empty() {
            if on_conflict == "ask" {
                let old = &found[0];
                let old_version = if old.version.is_empty() { hopper_core::scan::version_from_file_name(&old.file_name) } else { old.version.clone() };
                let _ = std::fs::remove_file(&part2);
                return Ok(Err(json!({
                    "error": format!("{} is already on the server.", new.mod_name),
                    "conflict": { "mod": new.mod_name, "old_file": old.file_name, "old_version": old_version, "new_version": new_version, "same_name": same_name },
                })));
            }
            if on_conflict == "both" && same_name {
                let _ = std::fs::remove_file(&part2);
                anyhow::bail!("{name2} is already there under that name; both can't stay.");
            }
        }
        let mut replaced = false;
        if on_conflict == "replace" {
            // The old jars go where a removal puts them (Backups in the Mods
            // tab), so they can come back.
            let mut inst = crate::mods_api::instance_of(&e2)?;
            for m in &found {
                let i = crate::mods_api::index_of(&mut inst, &m.file_name)?;
                install::remove(&mut inst, i).map_err(install::explain_locked)?;
                replaced = true;
            }
        }
        std::fs::rename(&part2, &dest2)?;
        Ok(Ok(replaced))
    })
    .await
    .map_err(bad)?;
    let replaced = match done {
        Err(x) => {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(bad(format!("{x:#}")));
        }
        Ok(Err(conflict)) => return Ok((StatusCode::CONFLICT, Json(conflict)).into_response()),
        Ok(Ok(r)) => r,
    };
    // The jar is new: the last update check doesn't know it.
    crate::mods_api::forget_updates(&e.id);
    // TODO(server-live-sync): broadcast the mod-changed event here once
    // claude/distracted-banach-512d85 is on main.
    Ok(Json(json!({ "name": name, "size": size, "replaced": replaced, "running": e.server.state().is_alive() })).into_response())
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
