//! Hopper Server updating itself from the GitHub releases tagged
//! `hopperd-v<version>` (Windows and Linux; Docker updates the image
//! instead).
//!
//! A while after start and then every half hour it looks for a newer
//! release. With auto-update on (the default) it downloads the release for
//! this computer (the Windows zip, or the Linux x64 / arm64 tar.gz), puts
//! the new program in place of the running one (Windows lets a running exe
//! be renamed, not overwritten; Linux renames the new file over it, see
//! `updswap`) and restarts once no job is running. Minecraft servers keep
//! running through it.
//! The download comes from GitHub's HTTPS release; its length and the
//! SHA-256 GitHub lists for the asset (`digest`; releases without one are
//! checked by length only, and that is logged) must match, and it must be a
//! program for this OS and CPU (Windows and Linux alike).
//! - On Linux: the panel starts the new program in its own place (same
//!   process id, so systemd keeps watching it). If that fails under systemd
//!   it exits with 75 and the unit restarts it (`Restart=on-failure`).
//!   Docker, a folder the service can't write to and development builds
//!   refuse (see [`why_not`]).
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
    /// While "waiting": what for, e.g. "2 running servers to stop".
    pub waiting_for: String,
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

/// The end of the release download's name for this OS and CPU; `None` where
/// there is no download (macOS, other CPUs).
fn asset_suffix(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("windows", _) => Some("-windows-x64.zip"),
        ("linux", "x86_64") => Some("-linux-x64.tar.gz"),
        ("linux", "aarch64") => Some("-linux-arm64.tar.gz"),
        _ => None,
    }
}

/// The running program's path. On Linux, after an update replaced the file
/// the kernel calls the old one "<path> (deleted)": the path is still the
/// place to start the new one from.
fn exe_path() -> std::io::Result<PathBuf> {
    Ok(without_deleted(std::env::current_exe()?, cfg!(windows)))
}

fn without_deleted(exe: PathBuf, windows: bool) -> PathBuf {
    match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(p) if !windows => PathBuf::from(p),
        _ => exe,
    }
}

/// What decides whether this copy can update itself.
struct Host<'a> {
    os: &'a str,
    arch: &'a str,
    exe: &'a Path,
    /// In a Docker / Podman container.
    container: bool,
    /// The exe's folder takes new files (only asked off Windows).
    writable: bool,
}

/// Docker's and Podman's marks: their marker files, or a container runtime
/// in the first process's cgroup.
fn container_hints(marker_file: bool, env_flag: bool, cgroup: &str) -> bool {
    marker_file || env_flag || ["docker", "kubepods", "libpod", "containerd"].iter().any(|m| cgroup.contains(m))
}

fn in_container() -> bool {
    let marker = Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists();
    let cgroup = std::fs::read_to_string("/proc/1/cgroup").unwrap_or_default();
    container_hints(marker, std::env::var_os("HOPPERD_DOCKER").is_some(), &cgroup)
}

fn why_not_for(h: &Host) -> Option<String> {
    if asset_suffix(h.os, h.arch).is_none() {
        return Some("Hopper Server has no download for this kind of computer, so it can't update itself. Replace the hopperd file yourself.".into());
    }
    if h.container {
        return Some("Hopper Server runs in Docker here. To update it, pull the new image and start the container again.".into());
    }
    let p = h.exe.to_string_lossy().to_lowercase().replace('\\', "/");
    if p.contains("/target/") || p.contains("/scratchpad/") {
        return Some("A development build: it doesn't update itself.".into());
    }
    if !h.writable {
        let dir = h.exe.parent().unwrap_or(h.exe).display();
        return Some(format!(
            "Hopper Server can't replace itself: the account that runs it can't write to {dir}. Move hopperd to a folder that account owns, run `sudo hopperd service install` again, or update it by hand."
        ));
    }
    None
}

/// Why this copy can't update itself, if it can't.
pub fn why_not() -> Option<String> {
    let exe = exe_path().ok()?;
    let windows = cfg!(windows);
    let writable = windows || exe.parent().is_some_and(crate::updswap::dir_writable);
    why_not_for(&Host { os: std::env::consts::OS, arch: std::env::consts::ARCH, exe: &exe, container: !windows && in_container(), writable })
}

/// The release download for the running OS and CPU. Where there is none,
/// the Windows one stands in so the version window still lists releases
/// (installing is refused by [`why_not`]).
fn suffix() -> &'static str {
    asset_suffix(std::env::consts::OS, std::env::consts::ARCH).unwrap_or("-windows-x64.zip")
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
    /// "sha256:<hex>" (GitHub lists it for newer assets).
    #[serde(default)]
    digest: Option<String>,
}

#[derive(Clone)]
struct Release {
    version: String,
    url: String,
    size: u64,
    /// The download's SHA-256 as GitHub lists it; `None` for releases
    /// without one (then only the length is checked).
    sha256: Option<String>,
    notes: String,
    page: String,
    /// 2026-10-18T09:08:51Z.
    published: String,
}

/// Hopper Server's releases with a download ending in `suffix`, newest
/// first, from GitHub's answer.
fn parse_releases(all: Vec<GhRelease>, suffix: &str) -> Vec<Release> {
    let mut found: Vec<Release> = all
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter_map(|r| {
            let version = r.tag_name.strip_prefix(TAG)?.to_string();
            let a = r.assets.iter().find(|a| a.name.to_lowercase().ends_with(suffix))?;
            Some(Release {
                version,
                url: a.browser_download_url.clone(),
                size: a.size,
                sha256: crate::updswap::expected_sha256(a.digest.as_deref()),
                notes: r.body.unwrap_or_default(),
                page: r.html_url,
                published: r.published_at.unwrap_or_default(),
            })
        })
        .collect();
    found.sort_by(|a, b| hopper_core::selfupdate::compare(&b.version, &a.version));
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
    let list = parse_releases(all, suffix());
    *LISTED.lock().unwrap() = Some((std::time::Instant::now(), list.clone()));
    Ok(list)
}

/// The newest Hopper Server release newer than this build, with a download
/// for this computer.
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
#[cfg_attr(not(windows), allow(dead_code))]
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

/// Downloads `r` (to `tmp`, removed after) and returns the program in it
/// (hopperd.exe from the zip, hopperd from the tar.gz). Only GitHub's
/// release over HTTPS; its length and, when GitHub lists one, its SHA-256
/// must match; and the file must be a program for this OS and CPU.
fn fetch(r: &Release, tmp: &Path) -> anyhow::Result<Vec<u8>> {
    hopper_core::cf::download_checked(&r.url, tmp, &hopper_core::cf::Expect { len: r.size, ..Default::default() }, None, |_| {})
        .context("downloading the update")?;
    match crate::updswap::verify_sha256(tmp, r.sha256.as_deref()) {
        Ok(crate::updswap::Checked::Matched) => {}
        Ok(crate::updswap::Checked::NotListed) => log::info!("Hopper Server {}: GitHub lists no checksum for this download; only its size was checked", r.version),
        Err(e) => {
            let _ = std::fs::remove_file(tmp);
            return Err(e);
        }
    }
    let bytes = if cfg!(windows) { exe_from_zip(tmp) } else { crate::updswap::file_from_targz(tmp, "hopperd") };
    let _ = std::fs::remove_file(tmp);
    let bytes = bytes?;
    crate::updswap::check_program(&bytes, std::env::consts::OS, std::env::consts::ARCH).map_err(|m| anyhow::anyhow!(m))?;
    Ok(bytes)
}

/// Downloads `r` and puts it in place of the running exe.
fn install(r: &Release) -> anyhow::Result<PathBuf> {
    let exe = exe_path()?;
    if !cfg!(windows) {
        // The new file replaces the old in one rename; the running copy
        // keeps going until the restart.
        let bytes = fetch(r, &crate::updswap::beside(&exe, ".download"))?;
        crate::updswap::swap_in(&exe, &bytes)?;
        return Ok(exe);
    }
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
    if let Ok(exe) = exe_path() {
        if cfg!(windows) {
            for ext in ["old.exe", "new.exe", "download"] {
                let _ = std::fs::remove_file(exe.with_extension(ext));
            }
        } else {
            // `hopperd.old` stays: it is the version before the update, to
            // move back by hand if the new one won't start.
            for suffix in [".new", ".download"] {
                let _ = std::fs::remove_file(crate::updswap::beside(&exe, suffix));
            }
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
    let _guard = scopeguard(idle);
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
            .with_context(|| format!("Hopper Server {version} has no download for this computer."))
    });
    let r = match r {
        Ok(r) if r.version == current() => Err(anyhow::anyhow!("The panel already runs {version}.")),
        r => r,
    };
    let r = match r {
        Ok(r) => r,
        Err(e) => {
            idle();
            return Err(e);
        }
    };
    let a = app.clone();
    std::thread::spawn(move || {
        let _guard = scopeguard(idle);
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
    let exe = match install(r) {
        Ok(exe) => exe,
        Err(e) => {
            log::warn!("update to {}: {e:#}", r.version);
            set(|s| {
                s.state = "error".into();
                s.error = Some(format!("{e:#}"));
            });
            return;
        }
    };
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
    // Long work (a pack update, a backup) finishes first. On Linux so do the
    // Minecraft servers the restarted panel couldn't take over (started by
    // an older panel, without the console FIFO); the rest keep running.
    set(|s| s.state = "waiting".into());
    let mut told = false;
    loop {
        let jobs = app.jobs.snapshot().iter().filter(|j| !j.done).count();
        let running = running_servers(app);
        let g = gate(cfg!(windows), STOP_SERVERS.load(Ordering::SeqCst), &running);
        if let Gate::StopThem(n) = g
            && !told
        {
            told = true;
            log::warn!("stopping {n} running server(s) for the update");
            stop_servers(app);
        }
        if jobs == 0 && g == Gate::Go {
            break;
        }
        let what = waiting_text(jobs, g);
        set(|s| s.waiting_for = what);
        std::thread::sleep(Duration::from_secs(3));
    }
    set(|s| s.waiting_for = String::new());
    restart_into(app, &r.version, &exe);
}

/// Whether running Minecraft servers hold the restart back.
#[derive(Debug, PartialEq, Clone, Copy)]
enum Gate {
    /// None to wait for.
    Go,
    /// This many are running: wait for them to be stopped.
    Wait(usize),
    /// This many are running and someone said to stop them: do that, then go.
    StopThem(usize),
}

/// Windows reattaches to servers it finds running, so they never hold a
/// restart back there. Elsewhere any running server does: nothing stops one
/// unless `consent` (someone picked "Stop servers and update").
fn gate(windows: bool, consent: bool, running: &[String]) -> Gate {
    match (windows, running.len(), consent) {
        (true, _, _) | (_, 0, _) => Gate::Go,
        (_, n, false) => Gate::Wait(n),
        (_, n, true) => Gate::StopThem(n),
    }
}

/// What the update is waiting for, for the panel to say.
fn waiting_text(jobs: usize, g: Gate) -> String {
    let servers = match g {
        Gate::Wait(n) | Gate::StopThem(n) => Some(format!("{n} running server{} to stop", if n == 1 { "" } else { "s" })),
        Gate::Go => None,
    };
    let jobs = (jobs > 0).then(|| format!("{jobs} running job{} to finish", if jobs == 1 { "" } else { "s" }));
    [servers, jobs].into_iter().flatten().collect::<Vec<_>>().join(" and ")
}

/// Set by "Stop servers and update"; read by the wait. Cleared when the
/// update run ends, so it never carries over to a later one.
static STOP_SERVERS: AtomicBool = AtomicBool::new(false);

/// The names of this panel's servers that are starting, running or stopping
/// and that a restarted panel couldn't pick up again (empty on Windows,
/// where it always can).
pub fn running_servers(app: &App) -> Vec<String> {
    if cfg!(windows) {
        return Vec::new();
    }
    app.servers
        .list()
        .iter()
        .filter(|e| holds_back(e.server.state().is_alive(), e.server.reattachable()))
        .map(|e| e.name.clone())
        .collect()
}

/// A running server holds a Linux restart back only when the restarted
/// panel couldn't take it over: one started by an older panel, or whose
/// console FIFO couldn't be made. The rest keep running through it.
fn holds_back(alive: bool, reattachable: bool) -> bool {
    alive && !reattachable
}

/// The normal stop for each server still running (saves its worlds first).
fn stop_servers(app: &App) {
    for e in app.servers.list().iter() {
        if matches!(e.server.state(), hopper_core::hosting::ServerState::Starting | hopper_core::hosting::ServerState::Running)
            && let Err(err) = e.server.stop()
        {
            log::warn!("stopping {}: {err:#}", e.name);
        }
    }
}

/// The run is over: nothing is busy, and a "stop the servers" answer is spent.
fn idle() {
    STOP_SERVERS.store(false, Ordering::SeqCst);
    BUSY.store(false, Ordering::SeqCst);
}

/// How the panel moves onto the new program.
#[derive(Debug, PartialEq)]
enum Restart {
    /// Nothing restarts it: the new one runs from the next start.
    Ready,
    /// Windows service: exit, the service manager starts it again.
    ExitForService,
    /// Windows tray: exit for the tray to start the new one.
    ExitForTray,
    /// Linux: start the new program in this process's place.
    Reexec,
}

fn restart_plan(windows: bool, service: bool, tray: bool) -> Restart {
    if !windows {
        Restart::Reexec
    } else if tray {
        Restart::ExitForTray
    } else if service {
        Restart::ExitForService
    } else {
        Restart::Ready
    }
}

/// Starts `exe` with this process's arguments in this process's place (same
/// process id, so systemd keeps watching it; open files and sockets close).
/// Only returns on failure.
#[cfg(unix)]
fn reexec(exe: &Path) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    std::process::Command::new(exe).args(std::env::args_os().skip(1)).exec()
}

#[cfg(not(unix))]
fn reexec(_exe: &Path) -> std::io::Error {
    std::io::Error::other("not supported here")
}

fn restart_into(app: &Arc<App>, version: &str, exe: &Path) {
    let tray = std::env::var_os("HOPPERD_TRAY_PID").is_some() && crate::control::under_tray();
    let plan = restart_plan(cfg!(windows), AS_SERVICE.load(Ordering::SeqCst), tray);
    if plan == Restart::Ready {
        log::info!("Hopper Server {version} is in place: it runs from the next start");
        set(|s| s.state = "ready".into());
        return;
    }
    if plan != Restart::Reexec {
        let _ = std::fs::write(app.boot_dir.join(MARKER), json!({ "from": CURRENT, "to": version, "at": now_secs() }).to_string());
    }
    set(|s| s.state = "restarting".into());
    log::warn!("restarting into Hopper Server {version}");
    match plan {
        Restart::ExitForTray => crate::control::exit_for_update(),
        Restart::ExitForService => {
            std::thread::sleep(Duration::from_millis(600));
            std::process::exit(75);
        }
        _ => {
            std::thread::sleep(Duration::from_millis(600));
            let e = reexec(exe);
            log::warn!("starting the new version failed: {e}");
            // Under systemd, exiting with an error makes the unit start the
            // new program (Restart=on-failure in the unit `service install`
            // writes). From a terminal there's nothing to start it.
            if std::env::var_os("INVOCATION_ID").is_some() {
                std::process::exit(75);
            }
            set(|s| s.state = "ready".into());
        }
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
        "current": current(), "latest": s.latest, "notes": hopper_core::selfupdate::clean_notes(&s.notes), "page": s.page, "checked": s.checked,
        "state": s.state, "error": s.error, "target": s.target, "waiting_for": s.waiting_for,
        "running_servers": running_servers(&app),
        "auto": app.cfg.read().unwrap().auto_update,
        "skip": app.cfg.read().unwrap().skip_auto_up_to,
        "why_not": why_not(), "os": std::env::consts::OS,
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
    /// "Stop servers and update" (Linux; see [`gate`]).
    #[serde(default)]
    stop_servers: bool,
}

/// "Update to X" / "Roll back to X" from the version window.
async fn install_one(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<InstallBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    if let Some(w) = why_not() {
        return Err(bad(w));
    }
    if b.stop_servers {
        STOP_SERVERS.store(true, Ordering::SeqCst);
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

#[derive(Deserialize, Default)]
struct NowBody {
    #[serde(default)]
    stop_servers: bool,
}

async fn now(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, body: Option<Json<NowBody>>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    if let Some(w) = why_not() {
        return Err(bad(w));
    }
    // Without this the update waits for running servers to stop on their own.
    if body.is_some_and(|b| b.stop_servers) {
        STOP_SERVERS.store(true, Ordering::SeqCst);
    }
    let a = app.clone();
    std::thread::spawn(move || run_once(&a, true, true));
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::{Host, skip_after, skipped, why_not_for};
    use std::path::Path;

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
        let list = super::parse_releases(gh, "-windows-x64.zip");
        let v: Vec<&str> = list.iter().map(|r| r.version.as_str()).collect();
        assert_eq!(v, ["0.13.10", "0.13.5"]);
        assert_eq!(list[1].published, "2026-10-28T01:00:00Z");
        assert_eq!(list[0].notes, "notes");
    }

    #[test]
    fn the_download_is_chosen_by_os_and_cpu() {
        use super::asset_suffix;
        assert_eq!(asset_suffix("windows", "x86_64"), Some("-windows-x64.zip"));
        assert_eq!(asset_suffix("linux", "x86_64"), Some("-linux-x64.tar.gz"));
        assert_eq!(asset_suffix("linux", "aarch64"), Some("-linux-arm64.tar.gz"));
        assert_eq!(asset_suffix("linux", "riscv64"), None);
        assert_eq!(asset_suffix("linux", "arm"), None);
        assert_eq!(asset_suffix("macos", "aarch64"), None);
        let gh: Vec<super::GhRelease> = serde_json::from_value(serde_json::json!([
            { "tag_name": "hopperd-v0.54.0", "assets": [
                { "name": "hopperd-0.54.0-windows-x64.zip", "browser_download_url": "w" },
                { "name": "hopperd-0.54.0-linux-x64.tar.gz", "browser_download_url": "lx" },
                { "name": "hopperd-0.54.0-linux-arm64.tar.gz", "browser_download_url": "la" }] },
            { "tag_name": "hopperd-v0.53.0", "assets": [
                { "name": "hopperd-0.53.0-windows-x64.zip", "browser_download_url": "w2" }] }
        ]))
        .unwrap();
        let arm = super::parse_releases(gh, asset_suffix("linux", "aarch64").unwrap());
        assert_eq!(arm.len(), 1);
        assert_eq!(arm[0].url, "la");
    }

    fn host<'a>(os: &'a str, arch: &'a str, exe: &'a Path) -> Host<'a> {
        Host { os, arch, exe, container: false, writable: true }
    }

    #[test]
    fn refusal_reasons() {
        let exe = Path::new("/home/mc/hopperd/hopperd");
        assert_eq!(why_not_for(&host("linux", "aarch64", exe)), None);
        assert_eq!(why_not_for(&host("linux", "x86_64", exe)), None);
        // Unknown CPU or OS: plain message.
        assert!(why_not_for(&host("linux", "riscv64", exe)).unwrap().contains("no download for this kind of computer"));
        assert!(why_not_for(&host("macos", "aarch64", exe)).is_some());
        // Docker.
        let mut h = host("linux", "x86_64", exe);
        h.container = true;
        assert!(why_not_for(&h).unwrap().contains("pull the new image"));
        // Development builds, either kind of path.
        for p in ["/home/me/hopper/target/release/hopperd", r"X:\Dev\hopper\target\perf\release\hopperd.exe", "/tmp/claude/scratchpad/hopperd"] {
            assert!(why_not_for(&host("linux", "x86_64", Path::new(p))).unwrap().contains("development build"), "{p}");
        }
        // A folder the service can't write to: says which.
        let mut h = host("linux", "x86_64", Path::new("/usr/local/bin/hopperd"));
        h.writable = false;
        let m = why_not_for(&h).unwrap();
        assert!(m.contains("/usr/local/bin") && m.contains("service install"), "{m}");
        // The order: no download first, then Docker, then the build, then the folder.
        h.container = true;
        assert!(why_not_for(&h).unwrap().contains("Docker"));
        assert_eq!(why_not_for(&host("windows", "x86_64", Path::new(r"C:\Program Files\Hopper Server\hopperd.exe"))), None);
    }

    #[test]
    fn running_servers_hold_a_linux_restart_back() {
        use super::{Gate, gate, waiting_text};
        let none: Vec<String> = vec![];
        let two = vec!["Survival".to_string(), "Creative".to_string()];
        // Nothing running: go.
        assert_eq!(gate(false, false, &none), Gate::Go);
        assert_eq!(gate(false, true, &none), Gate::Go);
        // Running, nobody said to stop them (automatic, or "wait"): wait.
        assert_eq!(gate(false, false, &two), Gate::Wait(2));
        // "Stop servers and update": stop them, then go.
        assert_eq!(gate(false, true, &two), Gate::StopThem(2));
        // Windows reattaches to running servers, so they never hold it back.
        assert_eq!(gate(true, false, &two), Gate::Go);
        assert_eq!(gate(true, true, &two), Gate::Go);
        // What the panel says.
        assert_eq!(waiting_text(0, Gate::Wait(2)), "2 running servers to stop");
        assert_eq!(waiting_text(0, Gate::StopThem(1)), "1 running server to stop");
        assert_eq!(waiting_text(1, Gate::Go), "1 running job to finish");
        assert_eq!(waiting_text(3, Gate::Wait(1)), "1 running server to stop and 3 running jobs to finish");
        assert_eq!(waiting_text(0, Gate::Go), "");
    }

    #[test]
    fn only_servers_the_new_panel_cant_take_over_hold_it_back() {
        use super::{Gate, gate, holds_back};
        // Started with the console FIFO: picked up again, no wait.
        assert!(!holds_back(true, true));
        // Started by an older panel, or without the FIFO: waited for.
        assert!(holds_back(true, false));
        // Stopped ones never count.
        assert!(!holds_back(false, false));
        assert!(!holds_back(false, true));
        // Two running, one of them from an older panel: only that one is asked about.
        let servers = [("Survival", true, true), ("Creative", true, false), ("Old", false, false)];
        let held: Vec<String> = servers.iter().filter(|s| holds_back(s.1, s.2)).map(|s| s.0.to_string()).collect();
        assert_eq!(held, ["Creative"]);
        assert_eq!(gate(false, false, &held), Gate::Wait(1));
        assert_eq!(gate(false, true, &held), Gate::StopThem(1));
        // All of them can be taken over: the update goes ahead.
        let none: Vec<String> = servers.iter().filter(|s| holds_back(s.1, s.1)).map(|s| s.0.to_string()).collect();
        assert_eq!(gate(false, false, &none), Gate::Go);
    }

    #[test]
    fn the_checksum_github_lists_rides_along_with_the_release() {
        let gh: Vec<super::GhRelease> = serde_json::from_value(serde_json::json!([
            { "tag_name": "hopperd-v0.55.0", "assets": [
                { "name": "hopperd-0.55.0-linux-x64.tar.gz", "browser_download_url": "u", "size": 3,
                  "digest": "sha256:BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD" }] },
            { "tag_name": "hopperd-v0.54.0", "assets": [
                { "name": "hopperd-0.54.0-linux-x64.tar.gz", "browser_download_url": "u", "digest": null }] },
            { "tag_name": "hopperd-v0.53.0", "assets": [
                { "name": "hopperd-0.53.0-linux-x64.tar.gz", "browser_download_url": "u" }] }
        ]))
        .unwrap();
        let list = super::parse_releases(gh, "-linux-x64.tar.gz");
        assert_eq!(list[0].sha256.as_deref(), Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
        // Older releases list none: only the length is checked.
        assert_eq!(list[1].sha256, None);
        assert_eq!(list[2].sha256, None);
    }

    #[test]
    fn container_hints_read_the_markers_and_the_cgroup() {
        use super::container_hints;
        assert!(container_hints(true, false, ""));
        assert!(container_hints(false, true, ""));
        assert!(container_hints(false, false, "12:cpu:/docker/3f2a9c\n"));
        assert!(container_hints(false, false, "0::/kubepods.slice/kubepods-pod1.slice\n"));
        // A normal systemd host, cgroup v1 and v2.
        assert!(!container_hints(false, false, "12:cpu:/system.slice/hopperd.service\n"));
        assert!(!container_hints(false, false, "0::/init.scope\n"));
        assert!(!container_hints(false, false, ""));
    }

    #[test]
    fn restart_rules() {
        use super::{Restart, restart_plan};
        // Windows as before: tray first, then the service, else next start.
        assert_eq!(restart_plan(true, false, false), Restart::Ready);
        assert_eq!(restart_plan(true, true, false), Restart::ExitForService);
        assert_eq!(restart_plan(true, false, true), Restart::ExitForTray);
        assert_eq!(restart_plan(true, true, true), Restart::ExitForTray);
        // Linux restarts into the new program however it was started.
        assert_eq!(restart_plan(false, false, false), Restart::Reexec);
        assert_eq!(restart_plan(false, true, false), Restart::Reexec);
    }

    #[test]
    fn the_deleted_mark_of_a_replaced_exe_is_dropped() {
        use super::without_deleted;
        let p = std::path::PathBuf::from("/home/mc/hopperd/hopperd (deleted)");
        assert_eq!(without_deleted(p.clone(), false), Path::new("/home/mc/hopperd/hopperd"));
        assert_eq!(without_deleted(p.clone(), true), p);
        assert_eq!(without_deleted("/home/mc/hopperd".into(), false), Path::new("/home/mc/hopperd"));
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
