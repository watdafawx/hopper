//! The router edge: a small process of its own that holds the Minecraft
//! router port and every player's connection, so players on a running
//! server stay connected while Hopper Server updates, restarts or crashes
//! (design/router-survives-updates.md, phase 2).
//!
//! ```text
//! hopperd edge run --data <dir>    the edge (hopperd starts it; not for people)
//! ```
//!
//! It runs from its own copy of the program (`hopperd-edge.exe` beside
//! hopperd on Windows, so a firewall rule can name one fixed path and
//! hopperd's own exe can be swapped by an update while the edge runs;
//! `<data>/edge/hopperd-edge` on Linux), started outside hopperd's job or
//! session like servers are, and found again from its run file
//! (`<data>/edge/edge.json`: pid, start time, channel, token) when hopperd
//! starts. hopperd keeps all the thinking (`client`, `crate::router`); the
//! edge (`serve`) only pipes, splices and answers while hopperd is away.
//!
//! Seams for later phases: the relay's home end moves in as another source
//! of legs (phase 3); `proto::PROTO` and `Welcome.version` are what edge
//! updates (phase 4) compare.

pub mod client;
pub mod local;
pub mod proto;
pub mod serve;
#[cfg(test)]
mod tests;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const RUN_FILE: &str = "edge.json";
/// Written by `service restart` / `service update` before they stop the
/// service: that stop is a restart, so the edge keeps its players.
pub const KEEP_MARKER: &str = "keep";
#[cfg(windows)]
pub const PROGRAM: &str = "hopperd-edge.exe";
#[cfg(not(windows))]
pub const PROGRAM: &str = "hopperd-edge";

pub fn edge_dir(data: &Path) -> PathBuf {
    data.join("edge")
}

/// What a running edge leaves for the next hopperd.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunFile {
    pub pid: u32,
    /// Its process start time (see [`proc_info`]), so a reused pid isn't taken for it.
    pub created: u64,
    pub version: String,
    pub proto: u16,
    pub channel: String,
    pub token: String,
}

pub fn read_run(data: &Path) -> Option<RunFile> {
    serde_json::from_str(&std::fs::read_to_string(edge_dir(data).join(RUN_FILE)).ok()?).ok()
}

/// Makes the edge folder, readable by this account only on Linux (it holds
/// the channel's socket and the token).
pub fn private_folder(dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        let m = std::fs::symlink_metadata(dir)?;
        if !m.is_dir() || m.uid() != unsafe { libc::geteuid() } {
            anyhow::bail!("{} isn't this account's own folder", dir.display());
        }
    }
    Ok(())
}

/// A process's start time and program file, if it runs.
#[cfg(windows)]
pub fn proc_info(pid: u32) -> Option<(u64, PathBuf)> {
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::System::Threading::{GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut t = [FILETIME::default(); 4];
        let [c, e, k, u] = &mut t;
        let times = GetProcessTimes(h, c, e, k, u);
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        let name = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len);
        // An exited process can still be opened while someone holds it.
        let mut code = 0u32;
        let alive = windows::Win32::System::Threading::GetExitCodeProcess(h, &mut code).is_ok() && code == 259;
        let _ = CloseHandle(h);
        if times.is_err() || name.is_err() || !alive {
            return None;
        }
        let created = (u64::from(t[0].dwHighDateTime) << 32) | u64::from(t[0].dwLowDateTime);
        Some((created, PathBuf::from(String::from_utf16_lossy(&buf[..len as usize]))))
    }
}

#[cfg(unix)]
pub fn proc_info(pid: u32) -> Option<(u64, PathBuf)> {
    let (state, start) = hopper_core::hosting::process::parse_proc_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)?;
    if matches!(state, 'Z' | 'X' | 'x') {
        return None;
    }
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    Some((start, exe))
}

#[cfg(not(any(windows, unix)))]
pub fn proc_info(_: u32) -> Option<(u64, PathBuf)> {
    None
}

/// The run file names a live edge: that pid, started then, running a
/// `hopperd-edge` program. Anything else is some other program now.
pub fn is_ours(r: &RunFile) -> bool {
    proc_info(r.pid).is_some_and(|(created, exe)| r.created != 0 && created == r.created && names_edge(&exe))
}

fn names_edge(exe: &Path) -> bool {
    // Linux adds " (deleted)" to a program file replaced while it runs.
    exe.file_name().map(|n| n.to_string_lossy().to_lowercase()).is_some_and(|n| n == PROGRAM || n.starts_with(&format!("{PROGRAM} ")))
}

/// Ends the edge `r` names, only if it is still that edge.
pub fn kill(r: &RunFile) {
    if !is_ours(r) {
        return;
    }
    log::warn!("router: ending the router process {}", r.pid);
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
        if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, r.pid) {
            let _ = TerminateProcess(h, 1);
            let _ = CloseHandle(h);
        }
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(r.pid as libc::pid_t, libc::SIGKILL);
    }
}

/// Where the edge runs from: one fixed path per install, so a firewall
/// rule for it keeps working and hopperd's own file stays free to update.
pub fn program_paths(data: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    #[cfg(windows)]
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        v.push(dir.join(PROGRAM));
    }
    v.push(edge_dir(data).join(PROGRAM));
    v
}

/// Starts a new edge for the data folder `data`, detached from hopperd.
pub fn spawn(data: &Path) -> anyhow::Result<()> {
    use std::process::{Command, Stdio};
    private_folder(&edge_dir(data))?;
    let me = std::env::current_exe()?;
    // Copied fresh each start (no edge runs then, so the file is free); an
    // edge that still runs from it keeps the copy that's there.
    let mut prog = None;
    for p in program_paths(data) {
        match std::fs::copy(&me, &p) {
            Ok(_) => {
                prog = Some(p);
                break;
            }
            Err(e) if p.exists() => {
                log::warn!("router: couldn't refresh {} ({e}); starting the copy that's there", p.display());
                prog = Some(p);
                break;
            }
            Err(e) => log::debug!("router: can't put the router at {}: {e}", p.display()),
        }
    }
    let prog = prog.context("no place for the router's program")?;
    let mut cmd = Command::new(&prog);
    cmd.arg("edge").arg("run").arg("--data").arg(crate::absolute(data)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let flags = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;
        // Out of hopperd's job (the tray's, a service's) the same way servers
        // are, or the job would end it with hopperd.
        if cmd.creation_flags(flags | CREATE_BREAKAWAY_FROM_JOB).spawn().is_err() {
            cmd.creation_flags(flags).spawn().with_context(|| format!("starting {}", prog.display()))?;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        cmd.spawn().with_context(|| format!("starting {}", prog.display()))?;
    }
    log::info!("router: started the router process ({})", prog.display());
    Ok(())
}

/// The panel is stopping. A real stop (Quit in the tray, the service
/// stopped, Ctrl+C) stops the router too, and its players are disconnected;
/// an update or a restart leaves it running so they stay on.
pub async fn on_panel_stop(app: &crate::App) {
    let keep = edge_dir(&app.boot_dir).join(KEEP_MARKER);
    let marked = std::fs::metadata(&keep).and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|d| d.as_secs() < 600));
    let _ = std::fs::remove_file(&keep);
    let updating = crate::control::exit_code() == crate::control::EXIT_UPDATED;
    if updating || marked {
        log::info!("router: keeps running through the restart; players stay connected");
        return;
    }
    app.edge.stop_for_good().await;
}

/// Asks a restart that stops the panel (`service restart`, `service
/// update`) to leave the router running.
pub fn keep_through_restart(data: &Path) {
    let dir = edge_dir(data);
    if dir.is_dir() {
        let _ = std::fs::write(dir.join(KEEP_MARKER), b"");
    }
}

/// `hopperd edge …`
pub fn command(args: &[String], mut log: env_logger::Builder) -> anyhow::Result<()> {
    let mut data = None;
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        if a == "--data" {
            data = it.next().map(PathBuf::from);
        }
    }
    let data = data.unwrap_or_else(crate::config::data_dir);
    match args.first().map(String::as_str) {
        Some("run") => {
            let dir = edge_dir(&data);
            private_folder(&dir)?;
            let path = dir.join("edge.log");
            if std::fs::metadata(&path).is_ok_and(|m| m.len() > 5 << 20) {
                let _ = std::fs::rename(&path, dir.join("edge.log.old"));
            }
            let file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
            log.target(env_logger::Target::Pipe(Box::new(file))).init();
            run(&data)
        }
        _ => anyhow::bail!("usage: hopperd edge run --data <dir> (Hopper Server starts this itself)"),
    }
}

fn run(data: &Path) -> anyhow::Result<()> {
    if let Some(r) = read_run(data)
        && r.pid != std::process::id()
        && is_ours(&r)
    {
        log::info!("router: another router (process {}) runs for this folder; leaving it", r.pid);
        return Ok(());
    }
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).map_err(|e| anyhow::anyhow!("{e}"))?;
    let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    let channel = local::address(data);
    let created = proc_info(std::process::id()).map_or(0, |(c, _)| c);
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    rt.block_on(async {
        let listener = local::Listener::bind(&channel)?;
        let r = RunFile { pid: std::process::id(), created, version: env!("CARGO_PKG_VERSION").into(), proto: proto::PROTO, channel: channel.clone(), token: token.clone() };
        crate::vps::home::write_private(&edge_dir(data).join(RUN_FILE), serde_json::to_string_pretty(&r)?.as_bytes())?;
        log::info!("router: process {} ready (version {})", r.pid, r.version);
        serve::Edge::new(token).run(listener).await;
        anyhow::Ok(())
    })?;
    if read_run(data).is_some_and(|r| r.pid == std::process::id()) {
        let _ = std::fs::remove_file(edge_dir(data).join(RUN_FILE));
    }
    log::info!("router: process {} ended", std::process::id());
    // Pipes' tasks may still be winding down: they go with the process.
    rt.shutdown_timeout(std::time::Duration::from_millis(200));
    Ok(())
}
