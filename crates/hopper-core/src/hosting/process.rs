//! Running a hosted server: one java process per server folder, started
//! without a console window and not tied to Hopper. Its output goes to a
//! file Hopper tails into a ring buffer; commands go through stdin. Hopper
//! can restart (a self-update) and pick the server up again.

use super::logparse::{LogEvent, parse_line};
use super::script::{Launch, launch_command};
use anyhow::{Context as _, bail};
use std::collections::{BTreeSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Console lines kept per server.
pub const MAX_LINES: usize = 2000;
/// How long a graceful stop waits for the world to save before killing.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(60);
/// How long Java may keep running once the server has stopped and saved its
/// worlds. Mods' threads that never end (Distant Horizons' timers, say) can
/// keep it alive for good; past this it is ended.
pub const LINGER_AFTER_STOP: Duration = Duration::from_secs(60);

/// Where a server is in its life.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerState {
    Stopped,
    /// The process runs; the "Done (" line hasn't come yet.
    Starting,
    Running,
    /// "stop" was sent; waiting for the process to end.
    Stopping,
    /// Ended with a non-zero exit code (or none) without being asked to.
    Crashed(Option<i32>),
}

impl ServerState {
    pub fn is_alive(&self) -> bool {
        matches!(self, ServerState::Starting | ServerState::Running | ServerState::Stopping)
    }
}

/// What the console said so far, updated line by line.
#[derive(Clone, Debug, Default)]
pub struct Tracker {
    pub lines: VecDeque<String>,
    pub players: BTreeSet<String>,
    pub done: bool,
    pub version: String,
    pub port_in_use: bool,
    pub eula_missing: bool,
    pub max_from_list: Option<u32>,
    /// Lines pushed since this tracker was made, including ones dropped
    /// from `lines`, so a reader can ask for only the lines after its last.
    pub total: u64,
    /// The server said "Stopping server" (asked to, or after a crash).
    pub stopping: bool,
    /// The server wrote a crash report this run.
    pub crashed: bool,
    /// When the stopping server said its worlds were saved: all that's left
    /// is for Java to exit.
    pub saved_at: Option<Instant>,
}

impl Tracker {
    pub fn push(&mut self, line: String) {
        match parse_line(&line) {
            Some(LogEvent::Done) => self.done = true,
            Some(LogEvent::Joined(n)) => {
                self.players.insert(n);
            }
            Some(LogEvent::Left(n)) => {
                self.players.remove(&n);
            }
            Some(LogEvent::List(_, max, names)) => {
                self.max_from_list = Some(max);
                // Old servers print names on the next line; keep ours then.
                if line.contains(" of a max of ") {
                    self.players = names.into_iter().collect();
                }
            }
            Some(LogEvent::Version(v)) => self.version = v,
            Some(LogEvent::PortInUse) => self.port_in_use = true,
            Some(LogEvent::EulaNotAccepted) => self.eula_missing = true,
            Some(LogEvent::Stopping) => self.stopping = true,
            None => {}
        }
        if crate::crashdetect::CRASH_SAVED.iter().any(|m| line.contains(m)) {
            self.crashed = true;
        }
        // 1.17+ says "All dimensions are saved" once; older versions "All
        // chunks are saved" per world, the last one at the end. A `save-all
        // flush` says it too, so only after "Stopping server".
        if self.stopping && (line.contains("All dimensions are saved") || line.contains("All chunks are saved")) {
            self.saved_at = Some(Instant::now());
        }
        if self.lines.len() >= MAX_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
        self.total += 1;
    }

    /// The lines after the first `seen` (as counted by `total`), oldest
    /// first, and the new total. Lines already dropped are skipped. When
    /// `seen` is past `total` the server started again with a fresh console,
    /// so every line comes back.
    pub fn since(&self, seen: u64) -> (Vec<String>, u64) {
        let seen = if seen > self.total { 0 } else { seen };
        let new = (self.total - seen).min(self.lines.len() as u64) as usize;
        let skip = self.lines.len() - new;
        (self.lines.iter().skip(skip).cloned().collect(), self.total)
    }
}

/// Java's output, in the server folder. Hopper tails it, so a restarted
/// Hopper can read the console from the start.
pub const CONSOLE_FILE: &str = ".hopper-console.log";
/// The running server's process, so a restarted Hopper finds it again.
pub const RUN_FILE: &str = ".hopper-run.json";

/// What `.hopper-run.json` holds while the server runs.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct RunInfo {
    pid: u32,
    /// When the process was created (a Windows FILETIME), so a reused pid
    /// isn't taken for the server.
    created: u64,
    /// Unix seconds.
    started: u64,
    /// Inside the server's process: a handle to the write end of its stdin.
    /// The server holds it, so its stdin never closes when Hopper exits,
    /// and a new Hopper copies it back out to send commands.
    stdin: u64,
    stop_requested: bool,
}

#[cfg(windows)]
type Proc = win::Process;
#[cfg(not(windows))]
type Proc = std::process::Child;

#[derive(Default)]
struct Shared {
    tracker: Tracker,
    proc: Option<Proc>,
    stdin: Option<File>,
    /// Appends Hopper's own lines (commands sent, notes) to the console file.
    log: Option<File>,
    pid: Option<u32>,
    stop_requested: bool,
    started_at: Option<SystemTime>,
    exit: Option<Option<i32>>,
    info: Option<RunInfo>,
    root: PathBuf,
    /// Bumped per start, so an old run's threads leave a new run alone.
    run: u64,
    /// [`LINGER_AFTER_STOP`], shorter in tests.
    linger: Option<Duration>,
}

impl Shared {
    /// A line of Hopper's in the console, in order with the server's.
    fn note(&mut self, line: &str) {
        match self.log.as_mut() {
            Some(f) => {
                let _ = writeln!(f, "{line}");
            }
            None => self.tracker.push(line.to_string()),
        }
    }

    fn save_run(&self) {
        if let Some(info) = &self.info
            && let Ok(json) = serde_json::to_string(info)
            && let Err(e) = std::fs::write(self.root.join(RUN_FILE), json)
        {
            log::warn!("saving {RUN_FILE}: {e}");
        }
    }
}

/// The state a process left behind.
pub fn state_of(alive: bool, done: bool, stop_requested: bool, exit: Option<Option<i32>>) -> ServerState {
    if alive {
        return match (stop_requested, done) {
            (true, _) => ServerState::Stopping,
            (false, true) => ServerState::Running,
            (false, false) => ServerState::Starting,
        };
    }
    match exit {
        Some(Some(0)) | None => ServerState::Stopped,
        Some(_) if stop_requested => ServerState::Stopped,
        Some(code) => ServerState::Crashed(code),
    }
}

/// A server Hopper runs on this PC, by folder. The java process runs on
/// its own: it outlives Hopper, and a restarted Hopper picks it up again
/// (console, players, commands) from `.hopper-run.json`.
pub struct LocalServer {
    root: PathBuf,
    shared: Arc<Mutex<Shared>>,
    sys: Mutex<sysinfo::System>,
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        // Ends the console reader; the server itself keeps running.
        self.lock().run += 1;
    }
}

impl LocalServer {
    /// Manages the server in `root`, reconnecting to it if it is still
    /// running from an earlier Hopper.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let s = LocalServer {
            root: root.into(),
            shared: Default::default(),
            sys: Mutex::new(sysinfo::System::new()),
        };
        #[cfg(windows)]
        s.reattach();
        s
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Checks the process, then reports where the server is.
    pub fn state(&self) -> ServerState {
        let mut g = self.lock();
        reap(&mut g);
        state_of(g.proc.is_some(), g.tracker.done, g.stop_requested, g.exit)
    }

    /// The console so far, oldest first.
    pub fn console(&self) -> Vec<String> {
        self.lock().tracker.lines.iter().cloned().collect()
    }

    /// Console lines after the first `seen`, and the count to pass next
    /// time (see [`Tracker::since`]).
    pub fn console_since(&self, seen: u64) -> (Vec<String>, u64) {
        self.lock().tracker.since(seen)
    }

    /// Players online, from joins and leaves (and `list` replies).
    pub fn players(&self) -> Vec<String> {
        self.lock().tracker.players.iter().cloned().collect()
    }

    pub fn tracker(&self) -> Tracker {
        self.lock().tracker.clone()
    }

    pub fn pid(&self) -> Option<u32> {
        let mut g = self.lock();
        reap(&mut g);
        g.proc.as_ref().and(g.pid)
    }

    /// How long Java may run on after the server stopped (tests).
    pub fn set_linger(&self, d: Duration) {
        self.lock().linger = Some(d);
    }

    pub fn started_at(&self) -> Option<SystemTime> {
        self.lock().started_at
    }

    /// Starts the server as its run.bat would. Fails when it is already
    /// running, the EULA isn't accepted or the port is taken.
    pub fn start(&self) -> anyhow::Result<()> {
        if self.state().is_alive() {
            bail!("{} is already running", self.name());
        }
        if !super::eula_accepted(&self.root) {
            bail!("Accept the Minecraft EULA for this server first (eula.txt).");
        }
        let props = super::properties::Properties::load(&self.root.join("server.properties"))?;
        let port = props.port();
        if !super::port_free(port) {
            bail!("Port {port} is in use: the server may already be running in another panel (Crafty, MCSS), or something else has the port.");
        }
        // Minecraft locks its world's session.lock while it runs.
        let world = props.get("level-name").filter(|w| !w.trim().is_empty()).unwrap_or("world");
        if let Ok(f) = OpenOptions::new().read(true).write(true).open(self.root.join(world).join("session.lock")) {
            match f.try_lock() {
                Ok(()) => {
                    let _ = f.unlock();
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    bail!("Its world ({world}) is open in another program: the server may already be running in another panel (Crafty, MCSS).");
                }
                Err(_) => {}
            }
        }
        let launch = launch_command(&self.root)?;
        self.spawn(&launch)
    }

    /// Starts `launch` in the server folder: no window, not tied to
    /// Hopper, output to the console file, commands through a stdin pipe.
    pub fn spawn(&self, launch: &Launch) -> anyhow::Result<()> {
        let path = self.root.join(CONSOLE_FILE);
        File::create(&path).with_context(|| format!("creating {}", path.display()))?;
        // Append-only handles: Java's output and Hopper's notes interleave
        // without overwriting each other.
        let append = || OpenOptions::new().append(true).open(&path);
        let mut log = append()?;
        if let Some(n) = &launch.note {
            let _ = writeln!(log, "[Hopper] {n}");
        }
        let _ = writeln!(log, "[Hopper] Starting: {} {}", launch.java.display(), launch.args.join(" "));
        let (stdin_r, stdin_w) = stdin_pipe()?;
        let mut cmd = Command::new(&launch.java);
        cmd.args(&launch.args)
            .current_dir(&self.root)
            .stdin(stdin_r)
            .stdout(append()?)
            .stderr(append()?);
        #[cfg(windows)]
        let child = {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            let flags = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;
            // Out of Hopper's job, if it is in one that ends with it. Jobs
            // that don't allow that refuse the flag: start inside it then.
            match cmd.creation_flags(flags | CREATE_BREAKAWAY_FROM_JOB).spawn() {
                Ok(c) => c,
                Err(_) => cmd.creation_flags(flags).spawn().with_context(|| format!("starting {}", launch.java.display()))?,
            }
        };
        #[cfg(not(windows))]
        let child = cmd.spawn().with_context(|| format!("starting {}", launch.java.display()))?;
        drop(cmd);
        let pid = child.id();
        let started = SystemTime::now();
        #[cfg(windows)]
        let (proc, info) = {
            use std::os::windows::io::AsRawHandle;
            let proc = win::Process::open(pid).context("watching the server process")?;
            drop(child);
            let stdin = proc.plant(stdin_w.as_raw_handle()).unwrap_or_else(|| {
                log::warn!("couldn't hand the server its own stdin; commands end when Hopper does");
                0
            });
            let info = RunInfo {
                pid,
                created: proc.created(),
                started: started.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
                stdin,
                stop_requested: false,
            };
            (proc, Some(info))
        };
        #[cfg(not(windows))]
        let (proc, info) = (child, None);
        let run = {
            let mut g = self.lock();
            g.run += 1;
            g.tracker = Tracker::default();
            g.stdin = Some(stdin_w);
            g.log = Some(log);
            g.pid = Some(pid);
            g.proc = Some(proc);
            g.stop_requested = false;
            g.exit = None;
            g.started_at = Some(started);
            g.root = self.root.clone();
            g.info = info;
            g.save_run();
            g.run
        };
        tail(path, self.shared.clone(), run);
        Ok(())
    }

    /// Picks up the server an earlier Hopper started, if it still runs.
    #[cfg(windows)]
    fn reattach(&self) {
        let run_file = self.root.join(RUN_FILE);
        let Some(info) = std::fs::read_to_string(&run_file)
            .ok()
            .and_then(|s| serde_json::from_str::<RunInfo>(&s).ok())
        else {
            return;
        };
        let proc = win::Process::open(info.pid)
            .filter(|p| info.created != 0 && p.created() == info.created)
            .and_then(|mut p| matches!(p.try_wait(), Ok(None)).then_some(p));
        let Some(proc) = proc else {
            let _ = std::fs::remove_file(&run_file);
            return;
        };
        let path = self.root.join(CONSOLE_FILE);
        let stdin = (info.stdin != 0)
            .then(|| proc.fetch(info.stdin))
            .flatten()
            .map(File::from);
        if stdin.is_none() {
            log::warn!("{}: no way to send commands to the running server", self.root.display());
        }
        let log = OpenOptions::new().append(true).open(&path).ok();
        let run = {
            let mut g = self.lock();
            g.run += 1;
            g.tracker = Tracker::default();
            g.stdin = stdin;
            g.log = log;
            g.pid = Some(info.pid);
            g.proc = Some(proc);
            g.stop_requested = info.stop_requested;
            g.exit = None;
            g.started_at = Some(UNIX_EPOCH + Duration::from_secs(info.started));
            g.root = self.root.clone();
            g.info = Some(info);
            g.note("[Hopper] Reconnected to the running server.");
            g.run
        };
        log::info!("reconnected to the server in {}", self.root.display());
        tail(path, self.shared.clone(), run);
    }

    /// Sends a console command (leading slash dropped).
    pub fn command(&self, line: &str) -> anyhow::Result<()> {
        let line = line.trim().trim_start_matches('/');
        if line.is_empty() {
            return Ok(());
        }
        let mut g = self.lock();
        reap(&mut g);
        if g.proc.is_none() {
            bail!("The server isn't running.");
        }
        if g.stdin.is_none() {
            bail!("the server's console is closed");
        }
        g.note(&format!("> {line}"));
        let stdin = g.stdin.as_mut().context("the server's console is closed")?;
        stdin.write_all(format!("{line}\n").as_bytes())?;
        stdin.flush()?;
        if line == "stop" {
            g.stop_requested = true;
            if let Some(i) = g.info.as_mut() {
                i.stop_requested = true;
            }
            g.save_run();
        }
        Ok(())
    }

    /// Asks for `list`; the reply updates [`Self::players`].
    pub fn refresh_players(&self) -> anyhow::Result<()> {
        self.command("list")
    }

    /// Sends "stop", then kills the process if it is still there after
    /// `timeout`. Returns at once.
    pub fn stop(&self, timeout: Duration) -> anyhow::Result<()> {
        if !self.state().is_alive() {
            self.dismiss_crash();
            return Ok(());
        }
        let run = self.lock().run;
        if self.command("stop").is_err() {
            return self.kill();
        }
        let shared = self.shared.clone();
        std::thread::Builder::new()
            .name("server-stop".into())
            .spawn(move || {
                let end = Instant::now() + timeout;
                while Instant::now() < end {
                    {
                        let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
                        reap(&mut g);
                        if g.run != run || g.proc.is_none() {
                            return;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
                let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
                if g.run == run {
                    if g.tracker.saved_at.is_some() {
                        log::warn!("{}: Java kept running after the server stopped; killing it", g.root.display());
                        g.note(LINGER_NOTE);
                    } else {
                        log::warn!("server did not stop in {timeout:?}; killing it");
                        g.note("[Hopper] The server didn't stop in time; ending it.");
                    }
                    kill_locked(&mut g);
                }
            })?;
        Ok(())
    }

    /// Stops and waits (up to `timeout`, then kills). For app shutdown.
    pub fn stop_blocking(&self, timeout: Duration) {
        if self.stop(timeout).is_err() {
            let _ = self.kill();
        }
        let end = Instant::now() + timeout + Duration::from_secs(2);
        while self.state().is_alive() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(200));
        }
        if self.state().is_alive() {
            let _ = self.kill();
        }
    }

    /// Ends the process at once (the world isn't saved).
    pub fn kill(&self) -> anyhow::Result<()> {
        let mut g = self.lock();
        reap(&mut g);
        if g.proc.is_none() {
            drop(g);
            self.dismiss_crash();
            return Ok(());
        }
        kill_locked(&mut g);
        Ok(())
    }

    /// Stop or kill on a server that already crashed: it counts as stopped
    /// by hand, so a crash restart that hasn't happened yet won't happen.
    fn dismiss_crash(&self) {
        let mut g = self.lock();
        if matches!(state_of(g.proc.is_some(), g.tracker.done, g.stop_requested, g.exit), ServerState::Crashed(_)) {
            g.stop_requested = true;
        }
    }

    /// The process's CPU (percent of the whole machine) and memory (bytes).
    pub fn usage(&self) -> Option<(f32, u64)> {
        let pid = sysinfo::Pid::from_u32(self.pid()?);
        let mut sys = self.sys.lock().unwrap_or_else(|e| e.into_inner());
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[pid]),
            true,
            sysinfo::ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        let p = sys.process(pid)?;
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f32;
        Some((p.cpu_usage() / cores, p.memory()))
    }

    fn name(&self) -> String {
        self.root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// A pipe for the server's stdin: (its end, Hopper's end).
#[cfg(windows)]
fn stdin_pipe() -> anyhow::Result<(Stdio, File)> {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows::Win32::Foundation::HANDLE;
    let (mut r, mut w) = (HANDLE::default(), HANDLE::default());
    unsafe { windows::Win32::System::Pipes::CreatePipe(&mut r, &mut w, None, 0) }
        .context("making the server's console pipe")?;
    let (r, w) = unsafe { (OwnedHandle::from_raw_handle(r.0), OwnedHandle::from_raw_handle(w.0)) };
    Ok((Stdio::from(r), File::from(w)))
}

#[cfg(not(windows))]
fn stdin_pipe() -> anyhow::Result<(Stdio, File)> {
    use std::os::fd::OwnedFd;
    let (r, w) = std::io::pipe()?;
    Ok((Stdio::from(r), File::from(OwnedFd::from(w))))
}

#[cfg(windows)]
fn try_wait(p: &mut Proc) -> std::io::Result<Option<Option<i32>>> {
    p.try_wait()
}

#[cfg(not(windows))]
fn try_wait(p: &mut Proc) -> std::io::Result<Option<Option<i32>>> {
    Ok(p.try_wait()?.map(|s| s.code()))
}

/// Notes the exit if the process has ended.
fn reap(g: &mut Shared) {
    let Some(p) = g.proc.as_mut() else { return };
    match try_wait(p) {
        Ok(Some(code)) => {
            g.exit = Some(code);
            g.proc = None;
            g.stdin = None;
            g.log = None;
            g.info = None;
            if !g.root.as_os_str().is_empty() {
                let _ = std::fs::remove_file(g.root.join(RUN_FILE));
            }
        }
        Ok(None) => {}
        Err(e) => log::warn!("checking the server process: {e}"),
    }
}

fn kill_locked(g: &mut Shared) {
    if g.proc.is_some() {
        g.stop_requested = true;
    }
    end_process(g);
}

/// What the console says when Java is ended for running on after the stop.
const LINGER_NOTE: &str = "[Hopper] Java kept running after the server stopped; ended it.";

/// Ends Java when it runs on after the server stopped and saved its worlds.
/// A stop that followed a crash stays a crash (the crash card, a crash
/// restart); any other stop counts as asked for, even one typed in the game.
fn end_lingering(g: &mut Shared) {
    log::warn!("{}: Java kept running after the server stopped; ending it", g.root.display());
    g.note(LINGER_NOTE);
    if !g.tracker.crashed {
        g.stop_requested = true;
    }
    end_process(g);
}

fn end_process(g: &mut Shared) {
    if let Some(p) = g.proc.as_mut() {
        #[cfg(windows)]
        p.kill();
        #[cfg(not(windows))]
        {
            let _ = p.kill();
            let _ = p.wait();
        }
    }
    reap(g);
}

/// Feeds the console file into the tracker line by line (lossy UTF-8, CR
/// trimmed) until the process ends, then says how it ended.
fn tail(path: PathBuf, shared: Arc<Mutex<Shared>>, run: u64) {
    let spawned = std::thread::Builder::new()
        .name("server-console".into())
        .spawn(move || {
            let mut f = match File::open(&path) {
                Ok(f) => f,
                Err(e) => {
                    log::warn!("reading {}: {e}", path.display());
                    return;
                }
            };
            let mut pending = Vec::new();
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                let mut got = false;
                while let Ok(n @ 1..) = f.read(&mut chunk) {
                    pending.extend_from_slice(&chunk[..n]);
                    got = true;
                }
                let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
                if g.run != run {
                    return;
                }
                let mut start = 0;
                while let Some(i) = pending[start..].iter().position(|&b| b == b'\n') {
                    let line = String::from_utf8_lossy(&pending[start..start + i]);
                    g.tracker.push(line.trim_end_matches('\r').to_string());
                    start += i + 1;
                }
                pending.drain(..start);
                reap(&mut g);
                let linger = g.linger.unwrap_or(LINGER_AFTER_STOP);
                if g.proc.is_some() && g.tracker.saved_at.is_some_and(|t| t.elapsed() >= linger) {
                    end_lingering(&mut g);
                    // Once more, to read the note it left in the console file.
                    continue;
                }
                if g.proc.is_none() && !got {
                    if !pending.is_empty() {
                        g.tracker.push(String::from_utf8_lossy(&pending).trim_end_matches('\r').to_string());
                    }
                    let msg = match g.exit {
                        Some(Some(c)) => format!("[Hopper] The server process ended (exit code {c})."),
                        _ => "[Hopper] The server process ended.".to_string(),
                    };
                    g.tracker.push(msg);
                    return;
                }
                drop(g);
                std::thread::sleep(Duration::from_millis(200));
            }
        });
    if let Err(e) = spawned {
        log::warn!("no reader for the server's console: {e}");
    }
}

/// The server's process by handle, so a restarted Hopper can watch, stop
/// and talk to a server it didn't start.
#[cfg(windows)]
mod win {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use windows::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, FILETIME, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_DUP_HANDLE,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess,
        WaitForSingleObject,
    };

    pub struct Process(OwnedHandle);

    impl Process {
        pub fn open(pid: u32) -> Option<Process> {
            let access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE | PROCESS_DUP_HANDLE;
            let h = unsafe { OpenProcess(access, false, pid) }.ok()?;
            Some(Process(unsafe { OwnedHandle::from_raw_handle(h.0) }))
        }

        fn h(&self) -> HANDLE {
            HANDLE(self.0.as_raw_handle())
        }

        /// When the process was created (FILETIME), 0 if unknown.
        pub fn created(&self) -> u64 {
            let mut t = [FILETIME::default(); 4];
            let [c, e, k, u] = &mut t;
            if unsafe { GetProcessTimes(self.h(), c, e, k, u) }.is_err() {
                return 0;
            }
            (u64::from(t[0].dwHighDateTime) << 32) | u64::from(t[0].dwLowDateTime)
        }

        /// `Some(exit code)` once it has ended.
        pub fn try_wait(&mut self) -> std::io::Result<Option<Option<i32>>> {
            if unsafe { WaitForSingleObject(self.h(), 0) } != WAIT_OBJECT_0 {
                return Ok(None);
            }
            let mut code = 0u32;
            Ok(Some(unsafe { GetExitCodeProcess(self.h(), &mut code) }.ok().map(|_| code as i32)))
        }

        pub fn kill(&mut self) {
            unsafe {
                let _ = TerminateProcess(self.h(), 1);
                WaitForSingleObject(self.h(), 5000);
            }
        }

        /// Copies Hopper's `handle` into the process; its value there.
        pub fn plant(&self, handle: RawHandle) -> Option<u64> {
            let mut out = HANDLE::default();
            unsafe {
                DuplicateHandle(GetCurrentProcess(), HANDLE(handle), self.h(), &mut out, 0, false, DUPLICATE_SAME_ACCESS)
            }
            .ok()?;
            Some(out.0 as usize as u64)
        }

        /// Copies the process's handle `value` into Hopper.
        pub fn fetch(&self, value: u64) -> Option<OwnedHandle> {
            let mut out = HANDLE::default();
            unsafe {
                DuplicateHandle(
                    self.h(),
                    HANDLE(value as usize as RawHandle),
                    GetCurrentProcess(),
                    &mut out,
                    0,
                    false,
                    DUPLICATE_SAME_ACCESS,
                )
            }
            .ok()?;
            Some(unsafe { OwnedHandle::from_raw_handle(out.0) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_returns_only_new_lines_and_survives_restarts() {
        let mut t = Tracker::default();
        for i in 0..3 {
            t.push(format!("line {i}"));
        }
        let (all, seen) = t.since(0);
        assert_eq!((all.len(), seen), (3, 3));
        t.push("line 3".into());
        assert_eq!(t.since(seen), (vec!["line 3".to_string()], 4));
        assert!(t.since(4).0.is_empty());
        // More lines than the ring keeps: only what is still there.
        for i in 0..MAX_LINES + 10 {
            t.push(format!("x {i}"));
        }
        assert_eq!(t.since(4).0.len(), MAX_LINES);
        // A new run starts a new tracker: a reader past it gets everything.
        let mut fresh = Tracker::default();
        fresh.push("again".into());
        assert_eq!(fresh.since(9999), (vec!["again".to_string()], 1));
    }

    #[test]
    fn states() {
        assert_eq!(state_of(true, false, false, None), ServerState::Starting);
        assert_eq!(state_of(true, true, false, None), ServerState::Running);
        assert_eq!(state_of(true, true, true, None), ServerState::Stopping);
        assert_eq!(state_of(false, false, false, None), ServerState::Stopped);
        assert_eq!(state_of(false, true, false, Some(Some(0))), ServerState::Stopped);
        assert_eq!(state_of(false, true, true, Some(Some(1))), ServerState::Stopped);
        assert_eq!(state_of(false, true, false, Some(Some(1))), ServerState::Crashed(Some(1)));
        assert_eq!(state_of(false, false, false, Some(None)), ServerState::Crashed(None));
    }

    #[test]
    fn tracker_follows_the_log_and_caps_lines() {
        let mut t = Tracker::default();
        for l in [
            "[10:00:01] [Server thread/INFO]: Starting minecraft server version 1.21.1",
            "[10:00:30] [Server thread/INFO]: Done (28.5s)! For help, type \"help\"",
            "[10:01:00] [Server thread/INFO]: Alex joined the game",
            "[10:01:01] [Server thread/INFO]: Steve joined the game",
            "[10:02:00] [Server thread/INFO]: Alex left the game",
        ] {
            t.push(l.into());
        }
        assert!(t.done);
        assert_eq!(t.version, "1.21.1");
        assert_eq!(t.players.iter().collect::<Vec<_>>(), ["Steve"]);
        t.push("[10:03:00] [Server thread/INFO]: There are 2 of a max of 10 players online: Bob, Steve".into());
        assert_eq!(t.players.len(), 2);
        assert_eq!(t.max_from_list, Some(10));
        for i in 0..MAX_LINES + 5 {
            t.push(format!("line {i}"));
        }
        assert_eq!(t.lines.len(), MAX_LINES);
        assert_eq!(t.lines.back().map(String::as_str), Some(&*format!("line {}", MAX_LINES + 4)));
    }

    /// A stand-in process (cmd echoing lines) instead of a real server.
    #[cfg(windows)]
    #[test]
    fn captures_output_and_exit() {
        let dir = std::env::temp_dir().join(format!("hopper-host-proc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = LocalServer::new(&dir);
        s.spawn(&Launch {
            note: None,
            java: "cmd".into(),
            args: vec![
                "/c".into(),
                "echo [1] [Server thread/INFO]: Done (1.0s)! For help, type \"help\"& echo [2] [x/INFO]: Alex joined the game& exit 3".into(),
            ],
        })
        .unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        while s.state().is_alive() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(s.state(), ServerState::Crashed(Some(3)));
        assert_eq!(s.players(), ["Alex"]);
        assert!(s.console().iter().any(|l| l.contains("Done (")));
        assert!(!dir.join(RUN_FILE).exists());
        // Kill on the crashed server: stopped by hand, so no crash restart.
        s.kill().unwrap();
        assert_eq!(s.state(), ServerState::Stopped);
        assert!(!dir.join(RUN_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A stand-in server that prints `lines` and then never exits, like Java
    /// held open by a mod's threads after the server stopped.
    #[cfg(windows)]
    fn hanging(tag: &str, lines: &[&str]) -> (LocalServer, PathBuf) {
        let dir = std::env::temp_dir().join(format!("hopper-host-linger-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = LocalServer::new(&dir);
        s.set_linger(Duration::from_secs(2));
        let print: String = lines.iter().map(|l| format!("[Console]::Out.WriteLine('{l}'); ")).collect();
        let script = format!("{print}[Console]::Out.Flush(); Start-Sleep -Seconds 120");
        s.spawn(&Launch { note: None, java: "powershell".into(), args: vec!["-NoProfile".into(), "-Command".into(), script] })
            .unwrap();
        (s, dir)
    }

    #[cfg(windows)]
    fn wait_dead(s: &LocalServer, limit: Duration) -> bool {
        let end = Instant::now() + limit;
        while s.state().is_alive() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(100));
        }
        !s.state().is_alive()
    }

    const DONE: &str = "[10:00:00] [Server thread/INFO]: Done (1.0s)! For help, type \"help\"";
    const STOPPING: &str = "[10:01:00] [Server thread/INFO]: Stopping server";
    const SAVED: &str = "[10:01:01] [Server thread/INFO]: ThreadedAnvilChunkStorage: All dimensions are saved";

    /// Stopped and saved, but Java runs on: ended after the wait, as a stop.
    #[cfg(windows)]
    #[test]
    fn java_left_running_after_the_stop_is_ended() {
        let (s, dir) = hanging("stop", &[DONE, STOPPING, SAVED]);
        assert!(wait_dead(&s, Duration::from_secs(20)), "{:?}", s.console());
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(s.state(), ServerState::Stopped);
        assert!(s.console().iter().any(|l| l == LINGER_NOTE), "{:?}", s.console());
        assert!(!dir.join(RUN_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// After a crash the same, but it stays a crash (card, crash restart).
    #[cfg(windows)]
    #[test]
    fn java_left_running_after_a_crash_is_ended_as_a_crash() {
        let crash = "[10:00:59] [Server thread/ERROR]: This crash report has been saved to: crash-reports\\crash-x-server.txt";
        let (s, dir) = hanging("crash", &[DONE, crash, STOPPING, SAVED]);
        assert!(wait_dead(&s, Duration::from_secs(20)), "{:?}", s.console());
        assert_eq!(s.state(), ServerState::Crashed(Some(1)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `save-all flush` says the worlds are saved too: a running server.
    #[cfg(windows)]
    #[test]
    fn a_save_while_running_ends_nothing() {
        let (s, dir) = hanging("save", &[DONE, SAVED]);
        assert!(wait_for(&s, SAVED), "{:?}", s.console());
        assert!(!wait_dead(&s, Duration::from_secs(4)));
        assert_eq!(s.state(), ServerState::Running);
        s.kill().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_tracker_sees_the_stop_save_and_crash() {
        let mut t = Tracker::default();
        t.push(DONE.into());
        t.push(SAVED.into());
        assert!(t.saved_at.is_none(), "a save-all while running");
        t.push(STOPPING.into());
        assert!(t.stopping && !t.crashed);
        t.push("[x] [Server thread/INFO]: ThreadedAnvilChunkStorage (world): All chunks are saved".into());
        assert!(t.saved_at.is_some(), "older versions' line");
        t.push("[x] [Server thread/FATAL]: Preparing crash report with UUID 1234".into());
        assert!(t.crashed);
    }

    fn wait_for(s: &LocalServer, line: &str) -> bool {
        let end = Instant::now() + Duration::from_secs(20);
        while Instant::now() < end {
            if s.console().iter().any(|l| l == line) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// A new LocalServer (a restarted Hopper) picks up a process the old
    /// one started: console from the start, commands, kill.
    #[cfg(windows)]
    #[test]
    fn reattaches_after_hopper_restarts() {
        let dir = std::env::temp_dir().join(format!("hopper-host-reattach-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = LocalServer::new(&dir);
        // Echoes every stdin line, like a console answering.
        let echo = "while (($l = [Console]::In.ReadLine()) -ne $null) { [Console]::Out.WriteLine('got ' + $l); [Console]::Out.Flush() }";
        first
            .spawn(&Launch {
                note: None,
                java: "powershell".into(),
                args: vec!["-NoProfile".into(), "-Command".into(), echo.into()],
            })
            .unwrap();
        first.command("hello").unwrap();
        assert!(wait_for(&first, "got hello"), "{:?}", first.console());
        let pid = first.pid().unwrap();
        drop(first);
        assert!(dir.join(RUN_FILE).exists());

        let second = LocalServer::new(&dir);
        assert!(second.state().is_alive());
        assert_eq!(second.pid(), Some(pid));
        assert!(wait_for(&second, "got hello"), "{:?}", second.console());
        second.command("world").unwrap();
        assert!(wait_for(&second, "got world"), "{:?}", second.console());
        assert!(second.console().iter().any(|l| l == "> world"));
        second.kill().unwrap();
        assert_eq!(second.state(), ServerState::Stopped);
        assert!(!dir.join(RUN_FILE).exists());
        // Nothing left to pick up.
        assert!(!LocalServer::new(&dir).state().is_alive());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
