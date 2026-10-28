//! Hopper Server updating itself from the GitHub releases tagged
//! `hopperd-v<version>` (Windows; Docker and Linux update the image or the
//! binary instead).
//!
//! A while after start and then every half hour it looks for a newer
//! release. With auto-update on (the default) it downloads the Windows zip,
//! puts the new hopperd.exe in place of the running one (Windows lets a
//! running exe be renamed, not overwritten) and restarts once no job is
//! running. Minecraft servers keep running through it; the panel finds
//! them again.
//! - As the Windows service: it exits with 75 and the service manager
//!   starts it again (the service restarts after a crash), on the new exe.
//! - Under the tray (portable / Start with Windows): it exits with
//!   [`crate::control::EXIT_UPDATED`]; the tray starts the new exe's tray
//!   and closes, so the icon comes back on the new version.
//! - From a terminal: the new exe is in place for the next start.
//!
//! `GET /api/v1/panel/update` is the state, `PUT` sets auto-update (owner),
//! `POST /api/v1/panel/update/now` checks and installs now (owner).
//! `GET /api/v1/panel/releases` lists the releases with their notes (the
//! version window by the logo), and `POST /api/v1/panel/update/install`
//! installs one of them, older ones too (owner). Rolling back makes
//! auto-update skip the version it came from and older ones, so it doesn't
//! put that version straight back; a newer release still installs.

use crate::App;
use crate::api::{ApiResult, bad, require, require_owner};
use anyhow::{Context as _, bail};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Where the releases are.
pub const REPO: &str = "watdafawx/hopper";
const TAG: &str = "hopperd-v";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// How often to look for a new version (as often as Hopper desktop).
const EVERY: Duration = Duration::from_secs(30 * 60);
/// Written before restarting into an update, so the service's tray doesn't
/// alert "stopped" while the service manager starts it again.
pub const MARKER: &str = "updating.json";

/// Set by `main` when running as the Windows service.
pub static AS_SERVICE: AtomicBool = AtomicBool::new(false);
static BUSY: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub current: &'static str,
    /// A newer version, once seen.
    pub latest: Option<String>,
    pub notes: String,
    pub page: String,
    /// Unix seconds of the last check.
    pub checked: u64,
    /// "", "checking", "downloading", "waiting" (for jobs), "restarting",
    /// "ready" (installed, used from the next start), "error".
    pub state: String,
    pub error: Option<String>,
    /// The version being installed (an update, or an older one).
    pub target: String,
}

static STATUS: Mutex<Option<Status>> = Mutex::new(None);

pub fn status() -> Status {
    STATUS.lock().unwrap().clone().unwrap_or(Status { current: CURRENT, ..Default::default() })
}

fn set(f: impl FnOnce(&mut Status)) {
    let mut g = STATUS.lock().unwrap();
    let s = g.get_or_insert_with(|| Status { current: CURRENT, ..Default::default() });
    f(s);
}

/// Why this copy can't update itself, if it can't.
pub fn why_not() -> Option<&'static str> {
    if !cfg!(windows) {
        return Some("Hopper Server updates itself on Windows. Here, pull the new Docker image or replace the hopperd binary.");
    }
    let exe = std::env::current_exe().ok()?;
    let p = exe.to_string_lossy().to_lowercase().replace('/', "\\");
    if p.contains("\\target\\") || p.contains("\\scratchpad\\") {
        return Some("A development build: it doesn't update itself.");
    }
    None
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

#[derive(Clone)]
struct Release {
    version: String,
    url: String,
    size: u64,
    notes: String,
    page: String,
    /// 2026-10-18T09:08:51Z.
    published: String,
}

/// Hopper Server's releases with a Windows zip, newest first, from GitHub's
/// answer.
fn parse_releases(all: Vec<GhRelease>) -> Vec<Release> {
    let mut found: Vec<Release> = all
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter_map(|r| {
            let version = r.tag_name.strip_prefix(TAG)?.to_string();
            let a = r.assets.iter().find(|a| a.name.to_lowercase().ends_with("-windows-x64.zip"))?;
            Some(Release {
                version,
                url: a.browser_download_url.clone(),
                size: a.size,
                notes: r.body.unwrap_or_default(),
                page: r.html_url,
                published: r.published_at.unwrap_or_default(),
            })
        })
        .collect();
    found.sort_by(|a, b| {
        if hopper_core::selfupdate::is_newer(&a.version, &b.version) {
            std::cmp::Ordering::Less
        } else if a.version == b.version {
            std::cmp::Ordering::Equal
        } else {
            std::cmp::Ordering::Greater
        }
    });
    found
}

/// The last list read, so opening the version window again doesn't ask
/// GitHub each time (it allows 60 calls an hour without a token).
static LISTED: Mutex<Option<(std::time::Instant, Vec<Release>)>> = Mutex::new(None);
const LIST_FOR: Duration = Duration::from_secs(5 * 60);

/// Hopper Server's releases, newest first. `fresh` always asks GitHub (the
/// update check); otherwise a list from the last few minutes is reused.
fn releases(fresh: bool) -> anyhow::Result<Vec<Release>> {
    if !fresh
        && let Some((at, list)) = LISTED.lock().unwrap().as_ref()
        && at.elapsed() < LIST_FOR
    {
        return Ok(list.clone());
    }
    let url = format!("https://api.github.com/repos/{REPO}/releases?per_page=50");
    let all: Vec<GhRelease> = hopper_core::cf::agent()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .context("reading Hopper Server's releases")?
        .body_mut()
        .read_json()
        .context("reading the releases")?;
    let list = parse_releases(all);
    *LISTED.lock().unwrap() = Some((std::time::Instant::now(), list.clone()));
    Ok(list)
}

/// The newest Hopper Server release newer than this build, with a Windows zip.
fn newest() -> anyhow::Result<Option<Release>> {
    let cur = current();
    Ok(releases(true)?.into_iter().find(|r| hopper_core::selfupdate::is_newer(&r.version, &cur)))
}

/// Whether auto-update leaves `version` alone: it's at or below the version
/// a rollback came from.
fn skipped(skip: Option<&str>, version: &str) -> bool {
    skip.is_some_and(|s| !hopper_core::selfupdate::is_newer(version, s))
}

/// The skip mark after installing `to` over `from` by hand. Going back
/// skips everything out so far (`newest`, `from`, or what a rollback
/// skipped already), so auto-update doesn't undo it; the next release
/// still installs. Going forward to or past the skipped version clears it.
fn skip_after(skip: Option<&str>, from: &str, to: &str, newest: Option<&str>) -> Option<String> {
    use hopper_core::selfupdate::is_newer;
    if is_newer(from, to) {
        let top = [skip, newest].into_iter().flatten().fold(from, |a, b| if is_newer(b, a) { b } else { a });
        return Some(top.to_string());
    }
    skip.filter(|s| is_newer(s, to)).map(str::to_string)
}

/// This build's version; `HOPPERD_TEST_VERSION` pretends to be another
/// (to try an update against the real releases).
fn current() -> String {
    std::env::var("HOPPERD_TEST_VERSION").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| CURRENT.to_string())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Renames, retrying while antivirus holds a fresh exe open.
fn rename_retrying(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut last = None;
    for i in 0..40 {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if matches!(e.raw_os_error(), Some(32) | Some(5)) => {
                last = Some(e);
                std::thread::sleep(Duration::from_millis(100 + 25 * i));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("rename kept failing")))
}

/// hopperd.exe out of the release zip.
fn exe_from_zip(zip: &Path) -> anyhow::Result<Vec<u8>> {
    let f = std::fs::File::open(zip)?;
    let mut z = zip::ZipArchive::new(f).context("opening the download")?;
    for i in 0..z.len() {
        let mut e = z.by_index(i)?;
        if e.name().rsplit('/').next().is_some_and(|n| n.eq_ignore_ascii_case("hopperd.exe")) {
            let mut out = Vec::new();
            std::io::copy(&mut e, &mut out)?;
            return Ok(out);
        }
    }
    bail!("The download has no hopperd.exe.")
}

/// Downloads `r` (to `tmp`, removed after) and returns its hopperd.exe.
fn fetch(r: &Release, tmp: &Path) -> anyhow::Result<Vec<u8>> {
    hopper_core::cf::download_checked(&r.url, tmp, &hopper_core::cf::Expect { len: r.size, ..Default::default() }, None, |_| {})
        .context("downloading the update")?;
    let bytes = exe_from_zip(tmp);
    let _ = std::fs::remove_file(tmp);
    let bytes = bytes?;
    if bytes.get(..2) != Some(b"MZ") {
        bail!("The download isn't a Windows program.");
    }
    Ok(bytes)
}

/// Downloads `r` and puts it in place of the running exe.
fn install(r: &Release) -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let bytes = fetch(r, &exe.with_extension("download"))?;
    let staged = exe.with_extension("new.exe");
    let old = exe.with_extension("old.exe");
    std::fs::write(&staged, &bytes).context("saving the update")?;
    let _ = std::fs::remove_file(&old);
    rename_retrying(&exe, &old).context("moving the running copy aside")?;
    if let Err(e) = rename_retrying(&staged, &exe) {
        let _ = rename_retrying(&old, &exe);
        return Err(e).context("putting the update in place");
    }
    Ok(exe)
}

#[cfg_attr(not(windows), allow(dead_code))]
/// The newest version newer than this build, if there is one (for the
/// service's tray, which checks on its own).
pub fn latest() -> anyhow::Result<Option<String>> {
    Ok(newest()?.map(|r| r.version))
}

#[cfg_attr(not(windows), allow(dead_code))]
/// Downloads the newest release's hopperd.exe into `dir`, for the
/// service's tray to install into the service (`<exe> service update`).
pub fn download_newest(dir: &Path) -> anyhow::Result<(String, PathBuf)> {
    let Some(r) = newest()? else { bail!("Hopper Server is up to date.") };
    std::fs::create_dir_all(dir)?;
    let bytes = fetch(&r, &dir.join("hopperd.download"))?;
    let exe = dir.join("hopperd.exe");
    std::fs::write(&exe, bytes).context("saving the update")?;
    Ok((r.version, exe))
}

/// Checks now on this thread (the tray's "Check for updates"); installs too
/// when `install_it` (someone asked, so a rollback's skip doesn't apply).
/// Returns once the check is done; with one running already, returns at
/// once (the status says "checking").
pub fn check(app: &Arc<App>, install_it: bool) {
    run_once(app, install_it, true);
}

/// What an earlier update left beside the exe; at start.
pub fn clean_up(boot: &Path) {
    if let Ok(exe) = std::env::current_exe() {
        for ext in ["old.exe", "new.exe", "download"] {
            let _ = std::fs::remove_file(exe.with_extension(ext));
        }
    }
    let _ = std::fs::remove_file(boot.join(MARKER));
}

/// Looks for a newer version; installs and restarts into it when `install`
/// (auto-update on, or "Update now"). `asked`: someone asked for it, so
/// the version a rollback came from installs too.
fn run_once(app: &Arc<App>, install_it: bool, asked: bool) {
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    let _guard = scopeguard(|| BUSY.store(false, Ordering::SeqCst));
    set(|s| {
        s.state = "checking".into();
        s.error = None;
    });
    let r = match newest() {
        Ok(r) => r,
        Err(e) => {
            log::info!("update check: {e:#}");
            set(|s| {
                s.state = "error".into();
                s.error = Some(format!("{e:#}"));
                s.checked = now_secs();
            });
            return;
        }
    };
    set(|s| {
        s.checked = now_secs();
        s.state = String::new();
        s.latest = r.as_ref().map(|r| r.version.clone());
        s.notes = r.as_ref().map(|r| r.notes.clone()).unwrap_or_default();
        s.page = r.as_ref().map(|r| r.page.clone()).unwrap_or_default();
    });
    let Some(r) = r else { return };
    if !install_it || why_not().is_some() {
        return;
    }
    if !asked && skipped(app.cfg.read().unwrap().skip_auto_up_to.as_deref(), &r.version) {
        log::info!("auto-update leaves {} alone: the panel was rolled back from it", r.version);
        return;
    }
    install_and_restart(app, &r);
}

/// Installs one release, older ones too ("Roll back to X"); on its own
/// thread. Fails at once when an update is already running.
fn install_version(app: &Arc<App>, version: &str) -> anyhow::Result<()> {
    if BUSY.swap(true, Ordering::SeqCst) {
        bail!("An update is already running.");
    }
    let r = releases(false).and_then(|list| {
        list.into_iter()
            .find(|r| r.version == version)
            .with_context(|| format!("Hopper Server {version} has no Windows download."))
    });
    let r = match r {
        Ok(r) if r.version == current() => Err(anyhow::anyhow!("The panel already runs {version}.")),
        r => r,
    };
    let r = match r {
        Ok(r) => r,
        Err(e) => {
            BUSY.store(false, Ordering::SeqCst);
            return Err(e);
        }
    };
    let a = app.clone();
    std::thread::spawn(move || {
        let _guard = scopeguard(|| BUSY.store(false, Ordering::SeqCst));
        set(|s| s.error = None);
        install_and_restart(&a, &r);
    });
    Ok(())
}

/// Downloads `r`, puts it in place, and restarts into it once no job runs.
fn install_and_restart(app: &Arc<App>, r: &Release) {
    let from = current();
    log::info!("updating Hopper Server {from} → {}", r.version);
    set(|s| {
        s.state = "downloading".into();
        s.target = r.version.clone();
    });
    if let Err(e) = install(r) {
        log::warn!("update to {}: {e:#}", r.version);
        set(|s| {
            s.state = "error".into();
            s.error = Some(format!("{e:#}"));
        });
        return;
    }
    // Going back: auto-update mustn't put the newer one straight back.
    {
        let mut cfg = app.cfg.write().unwrap();
        let newest = LISTED.lock().unwrap().as_ref().and_then(|(_, l)| l.first().map(|r| r.version.clone()));
        let skip = skip_after(cfg.skip_auto_up_to.as_deref(), &from, &r.version, newest.as_deref());
        if skip != cfg.skip_auto_up_to {
            cfg.skip_auto_up_to = skip;
            if let Err(e) = cfg.save(&app.data_dir) {
                log::warn!("saving the skipped version: {e:#}");
            }
        }
    }
    // Long work (a pack update, a backup) finishes first.
    set(|s| s.state = "waiting".into());
    while app.jobs.snapshot().iter().any(|j| !j.done) {
        std::thread::sleep(Duration::from_secs(20));
    }
    restart_into(app, &r.version);
}

fn restart_into(app: &Arc<App>, version: &str) {
    let tray = std::env::var_os("HOPPERD_TRAY_PID").is_some() && crate::control::under_tray();
    if !AS_SERVICE.load(Ordering::SeqCst) && !tray {
        log::info!("Hopper Server {version} is in place: it runs from the next start");
        set(|s| s.state = "ready".into());
        return;
    }
    let _ = std::fs::write(app.boot_dir.join(MARKER), json!({ "from": CURRENT, "to": version, "at": now_secs() }).to_string());
    set(|s| s.state = "restarting".into());
    log::warn!("restarting into Hopper Server {version}");
    if tray {
        crate::control::exit_for_update();
    } else {
        std::thread::sleep(Duration::from_millis(600));
        std::process::exit(75);
    }
}

/// Runs `f` when dropped.
fn scopeguard(f: impl FnOnce()) -> impl Drop {
    struct G<F: FnOnce()>(Option<F>);
    impl<F: FnOnce()> Drop for G<F> {
        fn drop(&mut self) {
            if let Some(f) = self.0.take() {
                f();
            }
        }
    }
    G(Some(f))
}

/// The checker, for the panel's lifetime.
pub fn start(app: Arc<App>) {
    clean_up(&app.boot_dir);
    let _ = std::thread::Builder::new().name("auto-update".into()).spawn(move || {
        let first = if std::env::var_os("HOPPERD_TEST_VERSION").is_some() { 15 } else { 120 };
        std::thread::sleep(Duration::from_secs(first));
        loop {
            let auto = app.cfg.read().unwrap().auto_update;
            run_once(&app, auto, false);
            std::thread::sleep(EVERY);
        }
    });
}

// ---- API ----------------------------------------------------------------------------------

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/panel/update", get(get_status).put(put_auto))
        .route("/api/v1/panel/update/now", post(now))
        .route("/api/v1/panel/update/install", post(install_one))
        .route("/api/v1/panel/releases", get(list_releases))
}

async fn get_status(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let s = status();
    Ok(Json(json!({
        "current": current(), "latest": s.latest, "notes": s.notes, "page": s.page, "checked": s.checked,
        "state": s.state, "error": s.error, "target": s.target,
        "auto": app.cfg.read().unwrap().auto_update,
        "skip": app.cfg.read().unwrap().skip_auto_up_to,
        "why_not": why_not(),
    })))
}

#[derive(Deserialize)]
struct ListQuery {
    /// 1: ask GitHub now ("Check for updates").
    fresh: Option<u8>,
}

/// The releases for the version window, newest first, with their notes.
async fn list_releases(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Query(q): Query<ListQuery>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let fresh = q.fresh.is_some_and(|f| f != 0);
    let list = tokio::task::spawn_blocking(move || releases(fresh)).await.map_err(bad)?.map_err(|e| bad(format!("{e:#}")))?;
    // The list doubles as an update check (as in Hopper desktop): the
    // version by the logo shows what it found.
    let cur = current();
    let up = list.iter().find(|r| hopper_core::selfupdate::is_newer(&r.version, &cur));
    set(|s| {
        if s.state.is_empty() || s.state == "error" {
            s.state = String::new();
            s.error = None;
        }
        s.checked = now_secs();
        s.latest = up.map(|r| r.version.clone());
        s.notes = up.map(|r| r.notes.clone()).unwrap_or_default();
        s.page = up.map(|r| r.page.clone()).unwrap_or_default();
    });
    let list: Vec<Value> = list
        .iter()
        .map(|r| {
            json!({
                "version": r.version, "published": r.published, "page": r.page,
                "notes": hopper_core::selfupdate::clean_notes(&r.notes),
            })
        })
        .collect();
    Ok(Json(json!({ "current": current(), "releases": list })))
}

#[derive(Deserialize)]
struct InstallBody {
    version: String,
}

/// "Update to X" / "Roll back to X" from the version window.
async fn install_one(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<InstallBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    if let Some(w) = why_not() {
        return Err(bad(w));
    }
    let a = app.clone();
    let v = b.version.trim().to_string();
    tokio::task::spawn_blocking(move || install_version(&a, &v)).await.map_err(bad)?.map_err(|e| bad(format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct AutoBody {
    auto: bool,
}

async fn put_auto(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<AutoBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    {
        let mut cfg = app.cfg.write().unwrap();
        cfg.auto_update = b.auto;
        cfg.save(&app.data_dir).map_err(|e| bad(format!("{e:#}")))?;
    }
    log::info!("auto-update {}", if b.auto { "on" } else { "off" });
    Ok(Json(json!({ "ok": true })))
}

async fn now(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    if let Some(w) = why_not() {
        return Err(bad(w));
    }
    let a = app.clone();
    std::thread::spawn(move || run_once(&a, true, true));
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::{skip_after, skipped};

    #[test]
    fn rollback_skips_the_version_it_left() {
        // Back from 0.13.7 to 0.13.4: 0.13.5–0.13.7 stay out, 0.13.8 comes in.
        let s = skip_after(None, "0.13.7", "0.13.4", Some("0.13.7"));
        assert_eq!(s.as_deref(), Some("0.13.7"));
        assert!(skipped(s.as_deref(), "0.13.7"));
        assert!(skipped(s.as_deref(), "0.13.5"));
        assert!(!skipped(s.as_deref(), "0.13.8"));
        assert!(!skipped(None, "0.13.8"));
        // Back again from 0.13.4 to 0.13.2: still skips up to 0.13.7.
        assert_eq!(skip_after(s.as_deref(), "0.13.4", "0.13.2", Some("0.13.7")).as_deref(), Some("0.13.7"));
        // Back from an older one while 0.13.9 is out: up to 0.13.9 stays out.
        assert_eq!(skip_after(None, "0.13.4", "0.13.2", Some("0.13.9")).as_deref(), Some("0.13.9"));
        // No list read: the version it came from.
        assert_eq!(skip_after(None, "0.13.4", "0.13.2", None).as_deref(), Some("0.13.4"));
        // Forward by hand to 0.13.5: 0.13.6 and 0.13.7 stay skipped.
        assert_eq!(skip_after(s.as_deref(), "0.13.4", "0.13.5", Some("0.13.7")).as_deref(), Some("0.13.7"));
        // Forward to the skipped one or past it: cleared.
        assert_eq!(skip_after(s.as_deref(), "0.13.4", "0.13.7", Some("0.13.7")), None);
        assert_eq!(skip_after(s.as_deref(), "0.13.4", "0.14.0", Some("0.14.0")), None);
        assert_eq!(skip_after(None, "0.13.4", "0.13.5", Some("0.13.7")), None);
    }

    #[test]
    fn releases_need_a_windows_zip_and_sort_newest_first() {
        let gh: Vec<super::GhRelease> = serde_json::from_value(serde_json::json!([
            { "tag_name": "hopperd-v0.13.5", "html_url": "p5", "published_at": "2026-10-28T01:00:00Z",
              "assets": [{ "name": "hopperd-0.13.5-windows-x64.zip", "browser_download_url": "u5", "size": 5 }] },
            { "tag_name": "hopperd-v0.13.10", "body": "notes",
              "assets": [{ "name": "hopperd-0.13.10-windows-x64.zip", "browser_download_url": "u10" }] },
            { "tag_name": "hopperd-v0.13.6", "assets": [{ "name": "hopperd-0.13.6-linux-x64.tar.gz", "browser_download_url": "x" }] },
            { "tag_name": "v0.15.10", "assets": [{ "name": "hopper-windows-x64.zip", "browser_download_url": "d" }] },
            { "tag_name": "hopperd-v0.14.0", "prerelease": true,
              "assets": [{ "name": "hopperd-0.14.0-windows-x64.zip", "browser_download_url": "u14" }] }
        ]))
        .unwrap();
        let list = super::parse_releases(gh);
        let v: Vec<&str> = list.iter().map(|r| r.version.as_str()).collect();
        assert_eq!(v, ["0.13.10", "0.13.5"]);
        assert_eq!(list[1].published, "2026-10-28T01:00:00Z");
        assert_eq!(list[0].notes, "notes");
    }

    #[test]
    fn exe_from_zip_finds_it() {
        let d = std::env::temp_dir().join(format!("hopperd-upd-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let zip = d.join("x.zip");
        {
            let mut w = zip::ZipWriter::new(std::fs::File::create(&zip).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            w.start_file("README.md", o).unwrap();
            std::io::Write::write_all(&mut w, b"hi").unwrap();
            w.start_file("hopperd.exe", o).unwrap();
            std::io::Write::write_all(&mut w, b"MZ new").unwrap();
            w.finish().unwrap();
        }
        assert_eq!(super::exe_from_zip(&zip).unwrap(), b"MZ new");
        let _ = std::fs::remove_dir_all(&d);
    }
}
