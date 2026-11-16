//! The Mods tab's details and backups, like Hopper desktop's mod peek and
//! Backups view: a mod's versions and changelogs, changing or rolling back
//! its version, locking it, resetting its config, and the jars Hopper set
//! aside in `.hopper-backups` (by Hopper desktop or this panel).

use crate::App;
use crate::api::{ApiResult, bad, not_found, require, require_owner};
use crate::servers::Entry;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post};
use axum::{Json, Router};
use hopper_core::instances::Instance;
use hopper_core::{backups, install, modconfig, richtext, source};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/servers/{id}/mods/versions", get(versions))
        .route("/api/v1/servers/{id}/mods/changelog", get(changelog))
        .route("/api/v1/servers/{id}/mods/version", post(set_version))
        .route("/api/v1/servers/{id}/mods/lock", post(lock))
        .route("/api/v1/servers/{id}/mods/config", get(config_files))
        .route("/api/v1/servers/{id}/mods/config-reset", post(config_reset))
        .route("/api/v1/servers/{id}/mod-backups", get(backup_list))
        .route("/api/v1/servers/{id}/mod-backups/count", get(backup_count))
        .route("/api/v1/servers/{id}/mod-backups/restore", post(backup_restore))
        .route("/api/v1/servers/{id}/mod-backups/delete", post(backup_delete))
        .route("/api/v1/servers/{id}/mod-backups/clean", post(backup_clean))
        .route("/api/v1/servers/{id}/mod-backups/undo", post(history_undo))
        .route("/api/v1/servers/{id}/mod-backups/put-back", post(history_put_back))
        .route("/api/v1/servers/{id}/mod-backups/free", post(history_free))
        .route("/api/v1/settings/backup-retention", get(get_retention).put(put_retention))
}

fn entry(app: &App, headers: &HeaderMap, method: &Method, id: &str) -> ApiResult<Arc<Entry>> {
    require(app, headers, method)?;
    app.servers.get(id).ok_or_else(|| not_found("server"))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(bad)?.map_err(|e| bad(format!("{e:#}")))
}

/// Mod ids go to the browser as strings (Modrinth's are 64-bit).
fn id_str(id: u64) -> String {
    id.to_string()
}

fn addon_index(inst: &Instance, file: &str) -> anyhow::Result<usize> {
    let f = file.trim_end_matches(".disabled");
    inst.addons
        .iter()
        .position(|a| a.file_name.trim_end_matches(".disabled").eq_ignore_ascii_case(f))
        .ok_or_else(|| anyhow::anyhow!("Hopper doesn't know where {file} is from yet. Identify it first."))
}

// ---- versions and changelogs --------------------------------------------------

/// A mod's files for this server's Minecraft version and loader, newest
/// first, marking the installed one.
async fn versions(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let file = q.get("file").cloned().unwrap_or_default();
    let v = blocking(move || {
        let inst = crate::mods_api::instance_of(&e)?;
        let a = inst.addons[addon_index(&inst, &file)?].clone();
        // The player opened Versions: a GitHub mod's list is asked for again.
        let files = source::files_checked(a.project_id, &inst.game_version, install::loader_of(&inst), hopper_core::sources::Check::Manual)?;
        let rows: Vec<Value> = files
            .iter()
            .take(40)
            .map(|f| {
                json!({
                    "id": id_str(f.id), "name": f.display_name, "file_name": f.file_name, "date": f.file_date,
                    // Just the version ("1.1.0-beta.54"), as Hopper desktop shows it.
                    "version": hopper_core::scan::short_version(&f.file_name),
                    "type": match f.release_type { 2 => "beta", 3 => "alpha", _ => "release" },
                    "installed": f.id == a.file_id,
                })
            })
            .collect();
        Ok(json!({
            "project_id": id_str(a.project_id), "file_id": id_str(a.file_id), "locked": a.locked,
            "page": a.page, "name": a.name, "versions": rows,
        }))
    })
    .await?;
    Ok(Json(v))
}

/// One file's changelog, as Markdown.
async fn changelog(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    entry(&app, &headers, &method, &id)?;
    let project: u64 = q.get("project").and_then(|p| p.parse().ok()).ok_or_else(|| bad("Which mod?"))?;
    let file: u64 = q.get("file").and_then(|p| p.parse().ok()).ok_or_else(|| bad("Which file?"))?;
    let full = q.get("full").is_some_and(|v| v == "1");
    let t = blocking(move || {
        let f = source::file(project, file)?;
        let md = richtext::to_markdown(&source::changelog(&f)?);
        // The update review wants the whole text.
        Ok(if full { hopper_core::changelog_trim::Trimmed { newer: md, ..Default::default() } } else { hopper_core::changelog_trim::trim_file(&md, &f) })
    })
    .await?;
    // Blank markup counts as no notes, so the card says so.
    let mut t = t;
    if !hopper_core::changelog_trim::has_visible_text(&t.newer) {
        t.newer.clear();
    }
    // Only its own version's notes; the older releases a cumulative text repeats are `earlier`.
    Ok(Json(json!({ "markdown": t.newer, "earlier": t.earlier, "earlier_count": t.earlier_count })))
}

#[derive(Deserialize)]
struct VersionBody {
    file: String,
    file_id: String,
}

/// Installs another version of a mod (an update or a roll back); the jar it
/// replaces goes to `.hopper-backups`.
async fn set_version(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Json(b): Json<VersionBody>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let file_id: u64 = b.file_id.trim().parse().map_err(|_| bad("Which version?"))?;
    let job = crate::pack_api::spawn(&app, format!("Changing a mod on {}", e.name), move |job| {
        let mut inst = crate::mods_api::instance_of(&e)?;
        let before = inst.addons.clone();
        let a = inst.addons[addon_index(&inst, &b.file)?].clone();
        let name = install::install_file(&mut inst, a.project_id, file_id, job)?;
        crate::mods_api::follow_switch(&e.id, &a.file_name, &before, &inst);
        log::info!("{}: {} is now {name}", e.name, a.name);
        Ok(json!({ "file": name }))
    });
    Ok(Json(json!({ "job": job })))
}

#[derive(Deserialize)]
struct LockBody {
    file: String,
    locked: bool,
}

/// Keeps a mod on its version: update checks skip it.
async fn lock(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<LockBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    blocking(move || {
        let inst = crate::mods_api::instance_of(&e)?;
        let i = addon_index(&inst, &b.file)?;
        Instance::update_on_disk(&e.root, |inst| {
            if let Some(a) = inst.addons.get_mut(i) {
                a.locked = b.locked;
            }
        })?;
        Ok(())
    })
    .await?;
    Ok(Json(json!({ "ok": true })))
}

// ---- config reset --------------------------------------------------------------

async fn config_files(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let file = q.get("file").cloned().unwrap_or_default();
    let files = blocking(move || {
        let inst = crate::mods_api::instance_of(&e)?;
        let a = inst.addons[addon_index(&inst, &file)?].clone();
        Ok(modconfig::files(&inst, &a))
    })
    .await?;
    Ok(Json(json!({ "files": files })))
}

#[derive(Deserialize)]
struct FileBody {
    file: String,
}

/// Moves a mod's config files to `.hopper-backups`; the mod writes fresh
/// ones on the next start.
async fn config_reset(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<FileBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let running = e.server.state().is_alive();
    let n = blocking(move || {
        let inst = crate::mods_api::instance_of(&e)?;
        let a = inst.addons[addon_index(&inst, &b.file)?].clone();
        let files = modconfig::files(&inst, &a);
        modconfig::reset(&e.root, &files)
    })
    .await?;
    Ok(Json(json!({ "reset": n, "restart_needed": running })))
}

// ---- .hopper-backups ----------------------------------------------------------------

async fn backup_list(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let named = crate::mods_api::names_people(&app);
    let (mut sets, usage, copies) = blocking(move || {
        // The entries are read every time. The sizes next to them (what the
        // folder holds, what Clean up would free, the full jar copies) take
        // as long again to work out and only change when something is added
        // or removed, so they're kept for a while ([`Sizes`]); and the three
        // listings of the same folders run side by side. Together they took
        // 2.4 s on 380 entries, one after another.
        let r = hopper_core::retention::policy();
        let root = &e.root;
        let sig = sizes_sig(root);
        let kept = sig.and_then(|sig| Sizes::get(root, sig, r));
        Ok(std::thread::scope(|s| {
            let sizes = kept.is_none().then(|| {
                let usage = s.spawn(|| hopper_core::retention::usage_and_preview(root));
                let copies = s.spawn(|| backups::copies_size(root));
                (usage, copies)
            });
            let sets = backups::history(root);
            let gone = "a History listing panicked";
            let (u, would, copies) = match (kept, sizes) {
                (Some(k), _) => (k.usage, k.would, k.copies),
                (None, Some((usage, copies))) => {
                    let (u, would) = usage.join().expect(gone);
                    let copies = copies.join().expect(gone);
                    if let Some(sig) = sig {
                        Sizes::put(root, Sizes { sig, at: Instant::now(), policy: r, usage: u.clone(), would: would.clone(), copies });
                    }
                    (u, would, copies)
                }
                (None, None) => unreachable!("sizes are worked out when none were kept"),
            };
            (sets, (u, would, r), copies)
        }))
    })
    .await?;
    let (u, would, r) = usage;
    // With several people on the panel, "You" would read as whoever is
    // looking. A change nobody was noted on (made before people were added,
    // or by a part of the panel that doesn't note it yet) says the panel.
    if named {
        for w in sets.iter_mut().flat_map(|s| s.files.iter_mut()).filter_map(|f| f.who.as_mut()).filter(|w| w.is_you()) {
            *w = hopper_core::actor::Who::panel("Hopper Server");
        }
    }
    let total: u64 = sets.iter().map(|s| s.size()).sum();
    // History: what happened, the mod, its versions (shared with Hopper desktop).
    let rows: Vec<Value> = sets.iter().map(backups::Set::to_json).collect();
    Ok(Json(json!({
        "sets": rows, "total_size": total, "copies": copies,
        // The whole folder (snapshots and world zips too) and the clean up rules.
        "usage": { "total": u.total(), "limited": u.limited(), "frees": would.freed, "removes": would.count(),
                   "keep_actions": r.keep_actions, "keep_days": r.keep_days, "max_mb": r.max_mb },
    })))
}

/// The sizes the History list shows, kept per server folder: what the
/// backups folder holds, what Clean up would free and the bytes of full jar
/// copies. Kept while the folder still lists the same entries (see
/// [`sizes_sig`]), the clean up rules are the same, and for no more than
/// [`SIZES_FOR`] (entries age past the rules with time). Anything here that
/// changes History drops them ([`Sizes::forget`]).
#[derive(Clone)]
struct Sizes {
    sig: (u128, usize),
    at: Instant,
    policy: hopper_core::retention::Retention,
    usage: hopper_core::retention::Usage,
    would: hopper_core::retention::Pruned,
    copies: u64,
}

const SIZES_FOR: Duration = Duration::from_secs(5 * 60);

fn all_sizes() -> std::sync::MutexGuard<'static, HashMap<PathBuf, Sizes>> {
    static MAP: std::sync::OnceLock<Mutex<HashMap<PathBuf, Sizes>>> = std::sync::OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap_or_else(|e| e.into_inner())
}

impl Sizes {
    fn get(root: &FsPath, sig: (u128, usize), policy: hopper_core::retention::Retention) -> Option<Sizes> {
        let all = all_sizes();
        all.get(root).filter(|k| k.sig == sig && k.policy == policy && k.at.elapsed() < SIZES_FOR).cloned()
    }

    fn put(root: &FsPath, k: Sizes) {
        all_sizes().insert(root.to_path_buf(), k);
    }

    /// After a change made here that can alter them.
    fn forget(root: &FsPath) {
        all_sizes().remove(root);
    }
}

/// What a server's `.hopper-backups` lists at the top: how many things and
/// when the folder last gained or lost one. A new action folder changes it;
/// `None` when the folder can't be looked at (then nothing is kept).
fn sizes_sig(root: &FsPath) -> Option<(u128, usize)> {
    let base = root.join(hopper_core::sync::BACKUP_DIR);
    let mtime = std::fs::metadata(&base).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    Some((mtime, std::fs::read_dir(&base).ok()?.count()))
}

/// How many History entries are newer than `since` (seconds), for the
/// header button's badge. A folder listing only, so it can be asked often.
async fn backup_count(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let since: i64 = q.get("since").and_then(|s| s.parse().ok()).unwrap_or(0);
    let (n, newest) = blocking(move || Ok(backups::count_since(&e.root, since))).await?;
    Ok(Json(json!({ "new": n, "newest": newest })))
}

/// Where old backups go when they're cleaned up: the server's trash (Files
/// › Trash), which empties itself after its usual time.
pub fn to_server_trash(root: &std::path::Path, paths: &[std::path::PathBuf]) -> anyhow::Result<()> {
    for p in paths {
        let rel = p.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
        crate::files::trash(root, &rel)?;
    }
    Ok(())
}

/// How long every server's `.hopper-backups` keeps old files.
async fn get_retention(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    Ok(Json(json!(app.cfg.read().unwrap().backup_retention)))
}

async fn put_retention(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(r): Json<hopper_core::retention::Retention>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    {
        let mut cfg = app.cfg.write().unwrap();
        cfg.backup_retention = r;
        cfg.save(&app.data_dir).map_err(|e| bad(format!("{e:#}")))?;
    }
    // Applied from the next backup on; older ones wait for Clean up.
    hopper_core::retention::set_policy(r);
    Ok(Json(json!(r)))
}

/// Removes old backups now, by the panel's clean up rules.
async fn backup_clean(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let root = e.root.clone();
    let p = blocking(move || Ok(hopper_core::retention::clean_up(&e.root))).await?;
    Sizes::forget(&root);
    Ok(Json(json!({ "removed": p.folders + p.snapshots, "freed": p.freed })))
}

#[derive(Deserialize)]
struct RestoreBody {
    when: i64,
    /// One file of the set; none: all of them.
    #[serde(default)]
    name: Option<String>,
}

/// Puts files back from a backup. A jar coming back to `mods/` first moves
/// the jar of the same mod that's there now into a new backup, so the
/// server never loads a mod twice and the restore can be undone the same
/// way.
async fn backup_restore(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<RestoreBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let running = e.server.state().is_alive();
    let root = e.root.clone();
    let done = blocking(move || {
        let set = backups::list(&e.root).into_iter().find(|s| s.when == b.when).ok_or_else(|| anyhow::anyhow!("That backup is gone."))?;
        let files: Vec<_> = set.files.iter().filter(|f| b.name.as_ref().is_none_or(|n| *n == f.name)).cloned().collect();
        anyhow::ensure!(!files.is_empty(), "Nothing to restore.");
        let mut inst = crate::mods_api::instance_of(&e)?;
        let mut out = Vec::new();
        for f in &files {
            // Kept as a note of where to get it: download it again first
            // (its mod ids are read from the jar).
            backups::materialize(&f.path)?;
            if f.restore_to.starts_with("mods/") && f.name.to_lowercase().contains(".jar") {
                let (ids, _) = hopper_core::deps::own_ids(&f.path);
                if !ids.is_empty() {
                    set_aside_same_mod(&mut inst, &ids)?;
                }
            }
            let to = backups::restore(&e.root, f)?;
            out.push(to.file_name().unwrap_or_default().to_string_lossy().into_owned());
        }
        Ok(out)
    })
    .await?;
    crate::mods_api::forget_updates(&id);
    Sizes::forget(&root);
    Ok(Json(json!({ "restored": done, "restart_needed": running })))
}

#[derive(Deserialize)]
struct UndoBody {
    when: i64,
    /// One file of the entry; none: all of them.
    #[serde(default)]
    name: Option<String>,
    /// Several files of the entry (a pack update's configs).
    #[serde(default)]
    names: Vec<String>,
}

/// Undo from History, as desktop's: a mod downloads again from its site
/// with its old entry; a config gets its old text back (what's there now
/// goes to History first). A mod whose version its site no longer has
/// comes back as `gone`, with the versions it does have, for the picker.
async fn history_undo(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<UndoBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let running = e.server.state().is_alive();
    let root = e.root.clone();
    let (done, gone, errors, pack) = blocking(move || {
        let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let sets: Vec<_> = backups::history(&e.root).into_iter().filter(|s| s.when == b.when).collect();
        anyhow::ensure!(!sets.is_empty(), "That History entry is gone.");
        // The whole entry of a pack update (no names): its pack record goes
        // back with it, once all of it came back.
        let whole_pack = if b.name.is_none() && b.names.is_empty() { sets[0].pack.clone() } else { None };
        // A name picks that entry out of whichever set has it; no name is
        // the whole (first) set.
        let files: Vec<_> = match &b.name {
            Some(n) => sets.iter().flat_map(|s| &s.files).filter(|f| f.name == *n).take(1).cloned().collect(),
            None if !b.names.is_empty() => sets[0].files.iter().filter(|f| b.names.contains(&f.name)).cloned().collect(),
            None => sets[0].files.clone(),
        };
        anyhow::ensure!(!files.is_empty(), "Nothing to undo.");
        let mut inst = crate::mods_api::instance_of(&e)?;
        let (mut done, mut gone, mut errors) = (Vec::new(), None, Vec::new());
        for f in &files {
            match hopper_core::undo::undo(&mut inst, f) {
                Ok(p) => done.push(p.file_name().unwrap_or_default().to_string_lossy().into_owned()),
                Err(err) => match err.downcast::<hopper_core::undo::Gone>() {
                    Ok(g) if gone.is_none() => {
                        gone = Some(json!({
                            "name": f.name, "title": g.name, "message": g.to_string(), "closest": g.closest, "version": g.version, "site": g.site,
                            "choices": g.choices.iter().map(|c| json!({ "id": c.id, "file_name": c.file_name, "version": hopper_core::undo::version_of(&c.file_name), "date": c.file_date })).collect::<Vec<_>>(),
                        }))
                    }
                    Ok(g) => errors.push(format!("{}: {g}", f.name)),
                    // Only the CurseForge website has the file. A panel has no
                    // download window: it says where to get it.
                    Err(err) => match err.downcast::<hopper_core::cf::WebsiteOnly>() {
                        Ok(w) => errors.push(hopper_core::undo::website_message(&w, false)),
                        Err(err) => errors.push(format!("{}: {err:#}", f.name)),
                    },
                },
            }
        }
        let mut pack = None;
        if let Some(m) = whole_pack
            && gone.is_none()
            && errors.is_empty()
        {
            match hopper_core::undo::restore_pack(&mut inst, &m, since) {
                Ok(r) => pack = Some(r.to_json()),
                Err(err) => errors.push(format!("{}'s pack version wasn't put back: {err:#}", m.name)),
            }
        }
        Ok((done, gone, errors, pack))
    })
    .await?;
    crate::mods_api::forget_updates(&id);
    crate::pack_api::forget(&id);
    Sizes::forget(&root);
    Ok(Json(json!({ "restored": done, "gone": gone, "errors": errors, "restart_needed": running, "pack": pack })))
}

#[derive(Deserialize)]
struct PutBackBody {
    when: i64,
    name: String,
    file_id: u64,
}

/// Puts another version back for a History entry whose own version is gone
/// from its site (the picker's choice).
async fn history_put_back(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<PutBackBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let running = e.server.state().is_alive();
    let root = e.root.clone();
    let put = blocking(move || {
        // As Undo finds it: a pack update's files sit in several folders.
        let sets: Vec<_> = backups::history(&e.root).into_iter().filter(|s| s.when == b.when).collect();
        let f = sets.iter().flat_map(|s| &s.files).find(|f| f.name == b.name).ok_or_else(|| anyhow::anyhow!("That History entry is gone."))?;
        let r = f.src.as_ref().ok_or_else(|| anyhow::anyhow!("{}: History doesn't know where it came from", f.name))?;
        let mut inst = crate::mods_api::instance_of(&e)?;
        let file = hopper_core::undo::choices(&inst, r)?.into_iter().find(|c| c.id == b.file_id).ok_or_else(|| anyhow::anyhow!("That version isn't offered any more."))?;
        hopper_core::undo::put_back_as(&mut inst, f, &file, None)?;
        Ok(file.file_name)
    })
    .await?;
    crate::mods_api::forget_updates(&id);
    Sizes::forget(&root);
    Ok(Json(json!({ "restored": [put], "restart_needed": running })))
}

/// The one-time clean up the owner starts: old jar copies the sites still
/// hand out become notes of where to download them.
async fn history_free(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let root = e.root.clone();
    let (freed, kept) = blocking(move || Ok(backups::free_space(&e.root))).await?;
    Sizes::forget(&root);
    Ok(Json(json!({ "freed": freed, "kept": kept })))
}

/// Moves the jars in `mods/` that are the mod `ids` (by their mod ids) to
/// `.hopper-backups`.
fn set_aside_same_mod(inst: &mut Instance, ids: &[String]) -> anyhow::Result<()> {
    let dir = inst.mods_dir();
    let mut same = Vec::new();
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let lower = name.to_lowercase();
        if !(lower.ends_with(".jar") || lower.ends_with(".jar.disabled")) {
            continue;
        }
        let (own, _) = hopper_core::deps::own_ids(&e.path());
        if own.iter().any(|o| ids.contains(o)) {
            same.push(name);
        }
    }
    for name in same {
        match addon_index(inst, &name) {
            Ok(i) => install::remove(inst, i).map_err(install::explain_locked)?,
            Err(_) => install::remove_loose_file(inst, &name).map_err(install::explain_locked)?,
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct DeleteBody {
    when: i64,
}

/// Deletes a backup to the server's trash (a rename inside its folder, so
/// it can come back from Files › Trash), like old backups past their time.
/// Never the Recycle Bin: nobody empties it on a server and the drive fills.
async fn backup_delete(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<DeleteBody>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let root = e.root.clone();
    blocking(move || {
        let set = backups::list(&e.root).into_iter().find(|s| s.when == b.when).ok_or_else(|| anyhow::anyhow!("That backup is gone."))?;
        to_server_trash(&e.root, &[set.dir])
    })
    .await?;
    Sizes::forget(&root);
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("hopperd-sizes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(hopper_core::sync::BACKUP_DIR)).unwrap();
        root
    }

    fn sized(sig: (u128, usize), policy: hopper_core::retention::Retention) -> Sizes {
        Sizes { sig, at: Instant::now(), policy, usage: Default::default(), would: Default::default(), copies: 7 }
    }

    #[test]
    fn kept_sizes_are_used_only_while_the_folder_and_the_rules_are_the_same() {
        let root = folder("kept");
        let rules = hopper_core::retention::Retention::default();
        let sig = sizes_sig(&root).unwrap();
        assert!(Sizes::get(&root, sig, rules).is_none(), "nothing kept yet");
        Sizes::put(&root, sized(sig, rules));
        assert_eq!(Sizes::get(&root, sig, rules).map(|k| k.copies), Some(7));
        // Other clean up rules: worked out again.
        let other = hopper_core::retention::Retention { keep_actions: rules.keep_actions + 1, ..rules };
        assert!(Sizes::get(&root, sig, other).is_none());
        // A new History entry changes what the folder lists.
        std::fs::create_dir_all(root.join(hopper_core::sync::BACKUP_DIR).join("1700000000")).unwrap();
        let after = sizes_sig(&root).unwrap();
        assert_ne!(after, sig);
        assert!(Sizes::get(&root, after, rules).is_none());
        // Anything that changes History drops them.
        Sizes::put(&root, sized(after, rules));
        Sizes::forget(&root);
        assert!(Sizes::get(&root, after, rules).is_none());
        // Old ones aren't trusted: entries age past the rules by themselves.
        let mut old = sized(after, rules);
        old.at = Instant::now().checked_sub(SIZES_FOR + Duration::from_secs(1)).unwrap();
        Sizes::put(&root, old);
        assert!(Sizes::get(&root, after, rules).is_none());
        // A server with no backups folder keeps nothing.
        assert!(sizes_sig(&root.join("nope")).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
