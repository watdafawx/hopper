//! Hopper Server updating itself from the GitHub releases tagged
//! `hopperd-v<version>` (Windows; Docker and Linux update the image or the
//! binary instead).
//!
//! A while after start and then every few hours it looks for a newer
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

use crate::App;
use crate::api::{ApiResult, bad, require, require_owner};
use anyhow::{Context as _, bail};
use axum::extract::State;
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
/// How often to look for a new version.
const EVERY: Duration = Duration::from_secs(6 * 3600);
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
}

static STATUS: Mutex<Option<Status>> = Mutex::new(None);

fn status() -> Status {
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
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

struct Release {
    version: String,
    url: String,
    size: u64,
    notes: String,
    page: String,
}

/// The newest Hopper Server release newer than this build, with a Windows zip.
fn newest() -> anyhow::Result<Option<Release>> {
    let url = format!("https://api.github.com/repos/{REPO}/releases?per_page=30");
    let all: Vec<GhRelease> = hopper_core::cf::agent()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .context("reading Hopper Server's releases")?
        .body_mut()
        .read_json()
        .context("reading the releases")?;
    let mut found: Vec<Release> = all
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter_map(|r| {
            let version = r.tag_name.strip_prefix(TAG)?.to_string();
            if !hopper_core::selfupdate::is_newer(&version, &current()) {
                return None;
            }
            let a = r.assets.iter().find(|a| a.name.to_lowercase().ends_with("-windows-x64.zip"))?;
            Some(Release { version, url: a.browser_download_url.clone(), size: a.size, notes: r.body.unwrap_or_default(), page: r.html_url })
        })
        .collect();
    found.sort_by(|a, b| {
        if hopper_core::selfupdate::is_newer(&a.version, &b.version) {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        }
    });
    Ok(found.into_iter().next())
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

/// Downloads `r` and puts it in place of the running exe.
fn install(r: &Release) -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let tmp = exe.with_extension("download");
    hopper_core::cf::download_checked(&r.url, &tmp, &hopper_core::cf::Expect { len: r.size, ..Default::default() }, None, |_| {})
        .context("downloading the update")?;
    let bytes = exe_from_zip(&tmp);
    let _ = std::fs::remove_file(&tmp);
    let bytes = bytes?;
    if bytes.get(..2) != Some(b"MZ") {
        bail!("The download isn't a Windows program.");
    }
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
/// (auto-update on, or "Update now").
fn run_once(app: &Arc<App>, install_it: bool) {
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
    log::info!("updating Hopper Server {} → {}", current(), r.version);
    set(|s| s.state = "downloading".into());
    if let Err(e) = install(&r) {
        log::warn!("update to {}: {e:#}", r.version);
        set(|s| {
            s.state = "error".into();
            s.error = Some(format!("{e:#}"));
        });
        return;
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
            run_once(&app, auto);
            std::thread::sleep(EVERY);
        }
    });
}

// ---- API ----------------------------------------------------------------------------------

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/panel/update", get(get_status).put(put_auto))
        .route("/api/v1/panel/update/now", post(now))
}

async fn get_status(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let s = status();
    Ok(Json(json!({
        "current": s.current, "latest": s.latest, "notes": s.notes, "page": s.page, "checked": s.checked,
        "state": s.state, "error": s.error,
        "auto": app.cfg.read().unwrap().auto_update,
        "why_not": why_not(),
    })))
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
    std::thread::spawn(move || run_once(&a, true));
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
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
