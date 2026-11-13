//! Running a hosted server: one java process per server folder, started
//! without a console window and not tied to Hopper. Its output goes to a
//! file Hopper tails into a ring buffer; commands go through stdin. Hopper
//! can restart (a self-update) and pick the server up again.

use super::boot::{BOOT_FILE, Boot, BootHistory, BootRecord, BootStatus};
use super::logparse::{LogEvent, parse_line};
use super::progress::{FINISHED_FRESH_SECS, Finished, Halt, Progress, ProgressKind};
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
/// The longest a graceful stop waits, however busy the save. Big packs can
/// take minutes to save their worlds, and killing Java mid-save can corrupt
/// them, so this is generous.
pub const STOP_HARD_CAP: Duration = Duration::from_secs(10 * 60);
/// How long a stopped server may say nothing at all, before it starts
/// saving, until it counts as hung.
pub const STOP_SILENT: Duration = Duration::from_secs(120);
/// Once it is saving, how long it may go without a new console line.
pub const SAVE_QUIET: Duration = Duration::from_secs(5 * 60);
/// What a stopping server says while it saves its worlds.
const SAVING: &[&str] = &[
    "Saving chunks for level",
    "Saving worlds",
    "Saving players",
    "ThreadedAnvilChunkStorage",
    "All chunks are saved",
    "All dimensions are saved",
];
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
    /// When each online player joined, for the ones seen joining this run.
    pub joined: std::collections::HashMap<String, SystemTime>,
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
    /// When the stopping server last said it was saving.
    pub saving_at: Option<Instant>,
    /// When the console last said anything.
    pub heard_at: Option<Instant>,
    /// How far the start has got.
    pub boot: Boot,
    /// How far a stop has got.
    pub halt: Halt,
    /// TPS (and ms per tick) spark printed in the console, and when.
    pub console_tps: Option<super::rcon::ConsoleTps>,
    spark: super::rcon::SparkBlock,
}

impl Tracker {
    pub fn push(&mut self, line: String) {
        let now = Instant::now();
        self.heard_at = Some(now);
        self.spark.push(&line, &mut self.console_tps);
        self.boot.push(&line, now);
        self.halt.push(&line, now);
        match parse_line(&line) {
            Some(LogEvent::Done) => self.done = true,
            Some(LogEvent::Joined(n)) => {
                self.joined.entry(n.clone()).or_insert_with(SystemTime::now);
                self.players.insert(n);
            }
            Some(LogEvent::Left(n)) => {
                self.joined.remove(&n);
                self.players.remove(&n);
            }
            Some(LogEvent::List(_, max, names)) => {
                self.max_from_list = Some(max);
                // Old servers print names on the next line; keep ours then.
                if line.contains(" of a max of ") {
                    self.players = names.into_iter().collect();
                    let on = &self.players;
                    self.joined.retain(|n, _| on.contains(n));
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
            self.saved_at = Some(now);
        }
        if self.stopping && SAVING.iter().any(|m| line.contains(m)) {
            self.saving_at = Some(now);
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

/// How long a graceful stop waits before ending Java. It waits as long as
/// the server is visibly getting there: every console line pushes the
/// deadline back, by a lot once it is saving its worlds, up to a hard cap.
/// Once the worlds are saved, Java gets [`LINGER_AFTER_STOP`] to exit (the
/// console reader ends it then; this is only the backstop).
#[derive(Clone, Copy, Debug)]
pub struct StopWait {
    pub hard: Duration,
    pub silent: Duration,
    pub saving_quiet: Duration,
    pub linger: Duration,
}

impl Default for StopWait {
    fn default() -> Self {
        StopWait { hard: STOP_HARD_CAP, silent: STOP_SILENT, saving_quiet: SAVE_QUIET, linger: LINGER_AFTER_STOP }
    }
}

impl StopWait {
    /// When a stop asked for at `asked` gives up, given what the console
    /// said so far.
    pub fn deadline(&self, asked: Instant, t: &Tracker) -> Instant {
        if let Some(saved) = t.saved_at {
            return saved + self.linger + Duration::from_secs(5);
        }
        let heard = t.heard_at.map_or(asked, |h| h.max(asked));
        let quiet = if t.saving_at.is_some() { self.saving_quiet } else { self.silent };
        (heard + quiet).min(asked + self.hard)
    }

    /// The longest any stop can take, for callers that wait for one.
    pub fn longest(&self) -> Duration {
        self.hard + self.linger + Duration::from_secs(10)
    }
}

/// "45 seconds", "3 minutes", "10 minutes 20 seconds".
fn wait_text(d: Duration) -> String {
    let s = d.as_secs();
    let (m, s) = (s / 60, s % 60);
    let unit = |n: u64, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    match (m, s) {
        (0, s) => unit(s, "second"),
        (m, 0) => unit(m, "minute"),
        (m, s) => format!("{} {}", unit(m, "minute"), unit(s, "second")),
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
    /// When the process was created, so a reused pid isn't taken for the
    /// server: a Windows FILETIME, or on Linux the start time in
    /// `/proc/<pid>/stat` (clock ticks since boot).
    created: u64,
    /// Unix seconds.
    started: u64,
    /// Windows: inside the server's process, a handle to the write end of
    /// its stdin. The server holds it, so its stdin never closes when Hopper
    /// exits, and a new Hopper copies it back out to send commands.
    stdin: u64,
    /// Unix: its stdin is the server's FIFO ([`stdin_fifo`]), which a new
    /// Hopper opens again to send commands. False for a plain pipe: a
    /// restarted Hopper can't reach that one's console.
    #[serde(default)]
    fifo: bool,
    stop_requested: bool,
}

#[cfg(windows)]
type Proc = win::Process;
#[cfg(unix)]
type Proc = ux::Process;

/// Where the servers' stdin FIFOs live on Unix, set by the app to a folder
/// of its own. Not in the server folder: the file browser, SFTP and
/// backups would open it there, hang on it and eat the server's commands.
static STDIN_DIR: std::sync::RwLock<Option<PathBuf>> = std::sync::RwLock::new(None);

/// Sets the folder for the servers' stdin FIFOs (Unix; Windows doesn't use
/// one). Call before the first [`LocalServer::new`], with the same folder
/// every run, so a restarted app finds the FIFOs again. Unset, it's a
/// folder of this user's in the temp folder.
pub fn set_stdin_dir(dir: impl Into<PathBuf>) {
    *STDIN_DIR.write().unwrap_or_else(|e| e.into_inner()) = Some(dir.into());
}

/// The stdin FIFO for the server in `root`: one per folder, with the same
/// name after a restart.
#[cfg_attr(not(unix), allow(dead_code))]
fn stdin_fifo(root: &Path) -> PathBuf {
    let dir = STDIN_DIR.read().unwrap_or_else(|e| e.into_inner()).clone();
    #[cfg(unix)]
    let dir = dir.unwrap_or_else(|| std::env::temp_dir().join(format!("hopper-stdin-{}", unsafe { libc::geteuid() })));
    #[cfg(not(unix))]
    let dir = dir.unwrap_or_else(std::env::temp_dir);
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    dir.join(fifo_name(&root))
}

/// FNV-1a of the folder's path: unlike std's hasher it stays the same from
/// one build to the next, so an updated Hopper finds the FIFO too.
#[cfg_attr(not(unix), allow(dead_code))]
fn fifo_name(root: &Path) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in root.to_string_lossy().bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}.stdin")
}

/// From a `/proc/<pid>/stat` line: the state letter (field 3) and the
/// start time (field 22). The name in brackets may hold spaces and
/// brackets itself, so the fields count from the last ')'.
#[cfg_attr(not(unix), allow(dead_code))]
fn parse_proc_stat(line: &str) -> Option<(char, u64)> {
    let rest = &line[line.rfind(')')? + 1..];
    let mut f = rest.split_ascii_whitespace();
    let state = f.next()?.chars().next()?;
    let start = f.nth(18)?.parse().ok()?;
    Some((state, start))
}

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
    /// Its mods when it started ([`crate::disk_watch::content_stamp`]),
    /// to tell whether they changed since.
    started_with: Option<std::collections::BTreeSet<(String, u64)>>,
    exit: Option<Option<i32>>,
    info: Option<RunInfo>,
    root: PathBuf,
    /// Bumped per start, so an old run's threads leave a new run alone.
    run: u64,
    /// [`LINGER_AFTER_STOP`], shorter in tests.
    linger: Option<Duration>,
    /// [`StopWait::default`], shorter in tests.
    stop_wait: Option<StopWait>,
    /// The last start, to time this one by.
    boot_last: Option<BootRecord>,
    /// This start's times are in `.hopper-boot.json`.
    boot_saved: bool,
    /// The last start or stop that ended, and when (for "Started in 1m 32s").
    finished: Option<(Finished, Instant)>,
    /// A restart is under way: the stop, then the start.
    restarting: bool,
    /// The start looks stuck (see [`crate::stuckstart`]).
    stuck: Option<crate::stuckstart::StuckReport>,
    /// The player chose to wait this many more seconds before being asked again.
    stuck_snooze: Option<i64>,
    /// The console file, read by the console thread and by [`reap`].
    feed: Option<Feed>,
}

/// The console file as read so far. Shared, not the console thread's own:
/// the check that sees the process end reads the file first, so the lines
/// a server writes just before it exits ("All dimensions are saved") are in
/// the console before anything sees it stopped.
struct Feed {
    file: File,
    /// The start of a line not finished yet.
    pending: Vec<u8>,
}

impl Feed {
    fn open(path: &Path) -> Option<Feed> {
        match File::open(path) {
            Ok(file) => Some(Feed { file, pending: Vec::new() }),
            Err(e) => {
                log::warn!("reading {}: {e}", path.display());
                None
            }
        }
    }
}

/// The most one look at the console file reads. A process can't keep it
/// reading under the lock by writing without a pause; the rest waits for
/// the next look.
const FEED_CAP: usize = 8 * 1024 * 1024;

impl Shared {
    /// Reads what's new in the console file into the tracker. Returns the
    /// whole lines it got, and whether it read anything at all.
    fn read_console(&mut self) -> (Vec<String>, bool) {
        let Some(feed) = self.feed.as_mut() else { return (Vec::new(), false) };
        let mut chunk = vec![0u8; 64 * 1024];
        let mut read = 0;
        while read < FEED_CAP {
            match feed.file.read(&mut chunk) {
                Ok(n @ 1..) => {
                    feed.pending.extend_from_slice(&chunk[..n]);
                    read += n;
                }
                _ => break,
            }
        }
        let mut lines = Vec::new();
        let mut start = 0;
        while let Some(i) = feed.pending[start..].iter().position(|&b| b == b'\n') {
            lines.push(String::from_utf8_lossy(&feed.pending[start..start + i]).trim_end_matches('\r').to_string());
            start += i + 1;
        }
        feed.pending.drain(..start);
        for l in &lines {
            self.tracker.push(l.clone());
        }
        (lines, read > 0)
    }

    /// A line of Hopper's in the console, in order with the server's.
    fn note(&mut self, line: &str) {
        match self.log.as_mut() {
            Some(f) => {
                let _ = writeln!(f, "{line}");
            }
            None => self.tracker.push(line.to_string()),
        }
    }

    /// Notes how long the start took, once it is done, for the next one.
    fn save_boot(&mut self) {
        if self.boot_saved {
            return;
        }
        let Some(r) = self.tracker.boot.record() else {
            return;
        };
        self.boot_saved = true;
        self.finished = Some((Finished { kind: ProgressKind::Start, secs: r.secs.round() as u32, ago_secs: 0, restarting: false }, Instant::now()));
        self.restarting = false;
        let mut h = BootHistory::load(&self.root);
        h.add(r);
        if let Err(e) = h.save(&self.root) {
            log::warn!("saving {BOOT_FILE}: {e}");
        }
    }

    fn save_run(&self) {
        if let Some(info) = &self.info
            && let Ok(json) = serde_json::to_string(info)
            && let Err(e) = write_private(&self.root.join(RUN_FILE), json.as_bytes())
        {
            log::warn!("saving {RUN_FILE}: {e}");
        }
    }
}

/// Writes `path` readable by this user only (Unix), never through a link.
#[cfg(unix)]
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    // One made by an older Hopper may be readable by others.
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(data)
}

#[cfg(not(unix))]
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, data)
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
type StartHook = Arc<dyn Fn(&Path) + Send + Sync>;

static BEFORE_START: std::sync::RwLock<Option<StartHook>> = std::sync::RwLock::new(None);

/// Runs `f` with a server's folder just before every start, whatever asked
/// for it (a button, a restart, the schedule, autostart, a player joining a
/// sleeping server). The panel uses it to turn RCON on in time. One hook;
/// setting it again replaces it.
pub fn on_before_start(f: impl Fn(&Path) + Send + Sync + 'static) {
    *BEFORE_START.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(f));
}

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
        s.reattach();
        s
    }

    /// Running, and a restarted Hopper picks it up again with its console
    /// and commands: always on Windows; on Unix when its stdin is the FIFO
    /// (not a server started by an older Hopper, or when the FIFO failed).
    pub fn reattachable(&self) -> bool {
        let mut g = self.lock();
        reap(&mut g);
        g.proc.is_some() && (cfg!(windows) || g.info.as_ref().is_some_and(|i| i.fifo && i.created != 0))
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

    /// Whether Hopper may use RCON on this server now (see
    /// [`super::rcon::allowed`]): up, and nothing has started stopping it,
    /// whether Hopper was asked to or the console said "Stopping server".
    pub fn rcon_allowed(&self) -> bool {
        let mut g = self.lock();
        reap(&mut g);
        let state = state_of(g.proc.is_some(), g.tracker.done, g.stop_requested, g.exit);
        super::rcon::allowed(&state, g.tracker.stopping)
    }

    /// Adds a line of Hopper's to the console ("[Hopper] …"), in order with
    /// the server's own.
    pub fn note(&self, line: &str) {
        self.lock().note(line);
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

    /// The last TPS spark printed in the console this run, if any.
    pub fn console_tps(&self) -> Option<super::rcon::ConsoleTps> {
        self.lock().tracker.console_tps
    }

    /// When each online player joined (only the ones seen joining).
    pub fn joined_at(&self) -> std::collections::HashMap<String, SystemTime> {
        self.lock().tracker.joined.clone()
    }

    /// How far the start has got, while the server is starting.
    pub fn boot_status(&self) -> Option<BootStatus> {
        let mut g = self.lock();
        reap(&mut g);
        let starting = state_of(g.proc.is_some(), g.tracker.done, g.stop_requested, g.exit) == ServerState::Starting;
        starting.then(|| g.tracker.boot.status(Instant::now(), g.boot_last.as_ref()))
    }

    /// A start or stop under way, ready to show; None when neither.
    pub fn progress(&self) -> Option<Progress> {
        let mut g = self.lock();
        reap(&mut g);
        let now = Instant::now();
        let mut p = match state_of(g.proc.is_some(), g.tracker.done, g.stop_requested, g.exit) {
            ServerState::Starting => Some(Progress::start(&g.tracker.boot.status(now, g.boot_last.as_ref()))),
            ServerState::Stopping => {
                let mut wait = g.stop_wait.unwrap_or_default();
                wait.linger = g.linger.unwrap_or(wait.linger);
                // A kill, or a stop from before this Hopper: no "stop" seen.
                let halt = if g.tracker.halt.began.is_some() { g.tracker.halt.clone() } else { Halt::asked(now) };
                let deadline = halt.began.map(|a| wait.deadline(a, &g.tracker));
                halt.status(now, g.tracker.heard_at, deadline)
            }
            _ => None,
        };
        if let Some(p) = p.as_mut() {
            p.restarting = g.restarting;
        }
        p
    }

    /// The start or stop that just ended, while it is news.
    pub fn finished(&self) -> Option<Finished> {
        let g = self.lock();
        let (f, at) = g.finished.as_ref()?;
        let ago = at.elapsed().as_secs();
        (ago < FINISHED_FRESH_SECS).then(|| Finished { ago_secs: ago as u32, ..f.clone() })
    }

    /// A restart is under way.
    pub fn restarting(&self) -> bool {
        self.lock().restarting
    }

    /// Marks a restart that a caller drives (the schedule's daily restart):
    /// its stop and start show as one restart.
    pub fn set_restarting(&self, on: bool) {
        self.lock().restarting = on;
    }

    /// Stops the server, waits for it to end (as long as a stop may take),
    /// then starts it again. Returns at once; a stopped server just starts.
    pub fn restart(self: &Arc<Self>) -> anyhow::Result<()> {
        if !self.state().is_alive() {
            return self.start();
        }
        self.set_restarting(true);
        if let Err(e) = self.stop() {
            self.set_restarting(false);
            return Err(e);
        }
        let s = self.clone();
        std::thread::Builder::new()
            .name("server-restart".into())
            .spawn(move || {
                // As long as the stop may take: a big world can save for minutes.
                let end = Instant::now() + s.stop_longest();
                while s.state().is_alive() && Instant::now() < end {
                    std::thread::sleep(Duration::from_millis(250));
                }
                if let Err(e) = s.start() {
                    s.set_restarting(false);
                    log::warn!("restarting {}: {e:#}", s.root().display());
                }
            })?;
        Ok(())
    }

    /// How long the last start took, in seconds.
    pub fn usual_start_secs(&self) -> Option<u32> {
        BootHistory::load(&self.root).last().map(|r| r.secs.round() as u32)
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

    /// Shorter stop waits, for tests.
    #[doc(hidden)]
    pub fn set_stop_wait(&self, w: StopWait) {
        self.lock().stop_wait = Some(w);
    }

    /// The start looks stuck: what the thread dump showed. Gone when it
    /// gets going again, stops, or the player chose to wait.
    pub fn stuck(&self) -> Option<crate::stuckstart::StuckReport> {
        let g = self.lock();
        if g.tracker.done || g.stop_requested || g.proc.is_none() {
            return None;
        }
        g.stuck.clone()
    }

    /// Hides the stuck notice and asks again after `secs` more quiet seconds.
    pub fn snooze_stuck(&self, secs: i64) {
        let mut g = self.lock();
        g.stuck = None;
        g.stuck_snooze = Some(secs);
    }

    pub fn started_at(&self) -> Option<SystemTime> {
        self.lock().started_at
    }

    /// Mods were added, updated, turned off or removed since it started:
    /// they load on its next start. False while it's stopped.
    pub fn changed_since_start(&self) -> bool {
        if !self.state().is_alive() {
            return false;
        }
        let Some(start) = self.lock().started_with.clone() else { return false };
        crate::disk_watch::content_stamp(&self.root) != start
    }

    /// Starts the server as its run.bat would. Fails when it is already
    /// running, the EULA isn't accepted or the port is taken.
    pub fn start(&self) -> anyhow::Result<()> {
        if self.state().is_alive() {
            bail!("{} is already running", self.name());
        }
        // Jars the last run held open while mods were removed, updated or
        // switched: stopped now, so they change before Java loads them.
        crate::locked::flush(&self.root);
        // Before anything reads server.properties, so what the hook writes
        // (RCON, say) is in this run.
        let hook = BEFORE_START.read().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(h) = hook {
            h(&self.root);
        }
        if !super::eula_accepted(&self.root) {
            bail!("Accept the Minecraft EULA for this server first (eula.txt).");
        }
        // The server list message's {pack_version}, {day}…, filled in for this run.
        if let Err(e) = super::motd::apply(&self.root) {
            log::warn!("{}: couldn't fill in the server list message: {e:#}", self.name());
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
        #[cfg(windows)]
        let (stdin_r, stdin_w) = stdin_pipe()?;
        // A FIFO the server holds open itself, read and write, so it never
        // sees its input end when Hopper exits, and a restarted Hopper opens
        // it again. Without one, a pipe: commands end with this Hopper.
        #[cfg(unix)]
        let (stdin_r, stdin_w, fifo) = match ux::fifo(&stdin_fifo(&self.root)) {
            Ok((r, w)) => (Stdio::from(r), w, true),
            Err(e) => {
                log::warn!("{}: no console FIFO ({e}); using a pipe", self.root.display());
                let _ = writeln!(log, "[Hopper] If Hopper restarts while this server runs, it can't reach its console until the server is restarted.");
                let (r, w) = stdin_pipe()?;
                (r, w, false)
            }
        };
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
        #[cfg(unix)]
        let child = {
            use std::os::unix::process::CommandExt;
            // Its own session: Ctrl+C in Hopper's terminal, the terminal
            // closing, or a signal to Hopper's process group (a service
            // manager stopping Hopper) doesn't reach the server.
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            cmd.spawn().with_context(|| format!("starting {}", launch.java.display()))?
        };
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
                fifo: false,
                stop_requested: false,
            };
            (proc, Some(info))
        };
        #[cfg(unix)]
        let (proc, info) = {
            // Watched by pid from here (waitpid while it's Hopper's child,
            // /proc once it isn't), the same as one picked up again.
            drop(child);
            let proc = ux::Process::spawned(pid);
            let info = RunInfo {
                pid,
                created: proc.created(),
                started: started.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
                stdin: 0,
                fifo,
                stop_requested: false,
            };
            (proc, Some(info))
        };
        let stamp = crate::disk_watch::content_stamp(&self.root);
        let run = {
            let mut g = self.lock();
            g.run += 1;
            g.tracker = Tracker { boot: Boot::starting(Instant::now()), ..Default::default() };
            g.boot_last = BootHistory::load(&self.root).last().cloned();
            g.boot_saved = false;
            g.stdin = Some(stdin_w);
            g.log = Some(log);
            g.pid = Some(pid);
            g.proc = Some(proc);
            g.stop_requested = false;
            g.exit = None;
            g.started_at = Some(started);
            g.started_with = Some(stamp);
            g.root = self.root.clone();
            g.info = info;
            g.feed = Feed::open(&path);
            g.save_run();
            g.run
        };
        tail(path, self.shared.clone(), run);
        Ok(())
    }

    /// The process `info` names, if it is still this server's, and a way to
    /// send it commands if there is one.
    #[cfg(windows)]
    fn adopt(&self, info: &mut RunInfo) -> Option<(Proc, Option<File>)> {
        let proc = win::Process::open(info.pid)
            .filter(|p| info.created != 0 && p.created() == info.created)
            .and_then(|mut p| matches!(p.try_wait(), Ok(None)).then_some(p))?;
        let stdin = (info.stdin != 0)
            .then(|| proc.fetch(info.stdin))
            .flatten()
            .map(File::from);
        Some((proc, stdin))
    }

    /// The same on Unix. The run file is in the server folder, where people
    /// can edit files, so its pid is only taken when that process started
    /// when the file says, runs in this server's folder, and (for commands)
    /// reads this server's FIFO; anything else could be some other program.
    #[cfg(unix)]
    fn adopt(&self, info: &mut RunInfo) -> Option<(Proc, Option<File>)> {
        if info.created == 0 {
            return None;
        }
        let proc = ux::Process::open(info.pid, info.created)?;
        if !ux::runs_in(info.pid, &self.root) {
            return None;
        }
        let fifo = stdin_fifo(&self.root);
        let stdin = if info.fifo && ux::stdin_is(info.pid, &fifo) {
            ux::writer(&fifo).map_err(|e| log::warn!("opening {}: {e}", fifo.display())).ok()
        } else {
            None
        };
        // What the next Hopper can count on.
        info.fifo = stdin.is_some();
        Some((proc, stdin))
    }

    /// Picks up the server an earlier Hopper started, if it still runs.
    fn reattach(&self) {
        let run_file = self.root.join(RUN_FILE);
        let Some(mut info) = std::fs::read_to_string(&run_file)
            .ok()
            .and_then(|s| serde_json::from_str::<RunInfo>(&s).ok())
        else {
            return;
        };
        let Some((proc, stdin)) = self.adopt(&mut info) else {
            let _ = std::fs::remove_file(&run_file);
            #[cfg(unix)]
            ux::remove_fifo(&stdin_fifo(&self.root));
            return;
        };
        let path = self.root.join(CONSOLE_FILE);
        if stdin.is_none() {
            log::warn!("{}: no way to send commands to the running server", self.root.display());
        }
        let log = OpenOptions::new().append(true).open(&path).ok();
        let run = {
            let mut g = self.lock();
            g.run += 1;
            // The console is read again from the top, all at once: the start
            // began when the process did, but its steps' times are lost.
            let ago = SystemTime::now().duration_since(UNIX_EPOCH + Duration::from_secs(info.started)).unwrap_or_default();
            let began = Instant::now().checked_sub(ago).unwrap_or_else(Instant::now);
            g.tracker = Tracker { boot: Boot::read_again(began), ..Default::default() };
            g.boot_last = BootHistory::load(&self.root).last().cloned();
            g.boot_saved = false;
            g.stdin = stdin;
            g.log = log;
            g.pid = Some(info.pid);
            g.proc = Some(proc);
            g.stop_requested = info.stop_requested;
            g.exit = None;
            g.started_at = Some(UNIX_EPOCH + Duration::from_secs(info.started));
            // Found running: what it loaded is unknown, so from now.
            g.started_with = Some(crate::disk_watch::content_stamp(&self.root));
            g.root = self.root.clone();
            g.info = Some(info);
            g.feed = Feed::open(&path);
            g.save_run();
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
            g.tracker.halt.began.get_or_insert(Instant::now());
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

    /// Sends "stop", then waits for Java to exit while the server is still
    /// getting there (see [`StopWait`]), and ends it only past that.
    /// Returns at once.
    pub fn stop(&self) -> anyhow::Result<()> {
        if !self.state().is_alive() {
            self.dismiss_crash();
            return Ok(());
        }
        let run = self.lock().run;
        if self.command("stop").is_err() && !self.terminate() {
            return self.kill();
        }
        let shared = self.shared.clone();
        let asked = Instant::now();
        std::thread::Builder::new()
            .name("server-stop".into())
            .spawn(move || {
                let mut told_saving = false;
                loop {
                    {
                        let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
                        reap(&mut g);
                        if g.run != run || g.proc.is_none() {
                            return;
                        }
                        let mut wait = g.stop_wait.unwrap_or_default();
                        wait.linger = g.linger.unwrap_or(wait.linger);
                        if !told_saving && g.tracker.saving_at.is_some() && g.tracker.saved_at.is_none() {
                            told_saving = true;
                            g.note(SAVING_NOTE);
                        }
                        if Instant::now() >= wait.deadline(asked, &g.tracker) {
                            let took = wait_text(asked.elapsed());
                            if g.tracker.saved_at.is_some() {
                                log::warn!("{}: Java kept running after the server stopped; killing it", g.root.display());
                                g.note(LINGER_NOTE);
                            } else if g.tracker.saving_at.is_some() {
                                log::warn!("{}: still saving {took} after the stop; killing it", g.root.display());
                                g.note(&format!(
                                    "[Hopper] The server was still saving its worlds {took} after the stop; ended it. The last changes may not be saved."
                                ));
                            } else {
                                log::warn!("{}: didn't stop in {took}; killing it", g.root.display());
                                g.note(&format!("[Hopper] The server didn't stop in {took} and wasn't saving; ended it."));
                            }
                            kill_locked(&mut g);
                            return;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
            })?;
        Ok(())
    }

    /// No console to send "stop" to (a server picked up without one): asks
    /// Java to shut down instead (SIGTERM), which runs Minecraft's own stop
    /// and saves the worlds. The stop's waits and kill follow as usual.
    #[cfg(unix)]
    fn terminate(&self) -> bool {
        let mut g = self.lock();
        reap(&mut g);
        if !g.proc.as_ref().is_some_and(|p| p.terminate()) {
            return false;
        }
        g.note("[Hopper] No console to send \"stop\" to: asked the server to shut down.");
        g.stop_requested = true;
        g.tracker.halt.began.get_or_insert(Instant::now());
        if let Some(i) = g.info.as_mut() {
            i.stop_requested = true;
        }
        g.save_run();
        true
    }

    #[cfg(not(unix))]
    fn terminate(&self) -> bool {
        false
    }

    /// The longest [`Self::stop`] can take, for callers that wait for it.
    pub fn stop_longest(&self) -> Duration {
        let g = self.lock();
        let mut wait = g.stop_wait.unwrap_or_default();
        wait.linger = g.linger.unwrap_or(wait.linger);
        wait.longest()
    }

    /// Stops and waits (then kills, like [`Self::stop`]). For app shutdown.
    pub fn stop_blocking(&self) {
        if self.stop().is_err() {
            let _ = self.kill();
        }
        let end = Instant::now() + self.stop_longest();
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

#[cfg(unix)]
fn stdin_pipe() -> anyhow::Result<(Stdio, File)> {
    use std::os::fd::OwnedFd;
    let (r, w) = std::io::pipe()?;
    Ok((Stdio::from(r), File::from(OwnedFd::from(w))))
}

fn try_wait(p: &mut Proc) -> std::io::Result<Option<Option<i32>>> {
    p.try_wait()
}

/// Notes the exit if the process has ended.
fn reap(g: &mut Shared) {
    let Some(p) = g.proc.as_mut() else { return };
    match try_wait(p) {
        Ok(Some(code)) => {
            // Its last lines first: written before it exited, so they're in
            // the file now, and the stop and its result can depend on them.
            g.read_console();
            if let Some(began) = g.tracker.halt.began.filter(|_| g.stop_requested) {
                g.finished = Some((Finished { kind: ProgressKind::Stop, secs: began.elapsed().as_secs() as u32, ago_secs: 0, restarting: g.restarting }, Instant::now()));
            } else {
                // Ended on its own: a restart under way won't get to its start.
                g.restarting = false;
            }
            g.exit = Some(code);
            g.proc = None;
            g.stdin = None;
            g.log = None;
            g.info = None;
            if !g.root.as_os_str().is_empty() {
                let _ = std::fs::remove_file(g.root.join(RUN_FILE));
                #[cfg(unix)]
                ux::remove_fifo(&stdin_fifo(&g.root));
                // Stopped when asked, cleanly: the last start that worked,
                // for crash help's "changed since" (see crashengine).
                if g.stop_requested && code == Some(0) {
                    crate::crashdetect::mark_good_run(&g.root);
                }
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

/// What the console says while a stopping server saves its worlds.
const SAVING_NOTE: &str = "[Hopper] Saving worlds… Hopper waits until they're saved.";

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
        p.kill();
    }
    reap(g);
}

/// Feeds the console file into the tracker line by line (lossy UTF-8, CR
/// trimmed) until the process ends, then says how it ended.
fn tail(path: PathBuf, shared: Arc<Mutex<Shared>>, run: u64) {
    let spawned = std::thread::Builder::new()
        .name("server-console".into())
        .spawn(move || {
            // Stuck-start watch: fed the same lines, looked at every 5 s.
            let mut watch = crate::stuckstart::Watch::new(&path, true, crate::time::now(), true);
            let mut looked = Instant::now();
            // When this thread first saw the process gone. Something it left
            // running can keep writing to the file; read on only a little.
            let mut gone: Option<Instant> = None;
            {
                let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
                if g.run == run && g.feed.is_none() {
                    return;
                }
                g.stuck = None;
            }
            loop {
                let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
                if g.run != run {
                    return;
                }
                let (lines, got) = g.read_console();
                for l in &lines {
                    watch.feed_line(l, crate::time::now());
                }
                g.save_boot();
                reap(&mut g);
                let linger = g.linger.unwrap_or(LINGER_AFTER_STOP);
                if g.proc.is_some() && g.tracker.saved_at.is_some_and(|t| t.elapsed() >= linger) {
                    end_lingering(&mut g);
                    // Once more, to read the note it left in the console file.
                    continue;
                }
                // Ended, and its last lines are in: whatever saw the exit
                // ([`reap`]) read the file after it.
                if g.proc.is_none() && (!got || gone.get_or_insert_with(Instant::now).elapsed() >= Duration::from_secs(2)) {
                    if let Some(f) = g.feed.take()
                        && !f.pending.is_empty()
                    {
                        g.tracker.push(String::from_utf8_lossy(&f.pending).trim_end_matches('\r').to_string());
                    }
                    let msg = match g.exit {
                        Some(Some(c)) => format!("[Hopper] The server process ended (exit code {c})."),
                        _ => "[Hopper] The server process ended.".to_string(),
                    };
                    g.tracker.push(msg);
                    return;
                }
                if let Some(secs) = g.stuck_snooze.take() {
                    watch.snooze(crate::time::now(), secs);
                    g.stuck = None;
                }
                let starting = g.proc.is_some() && !g.stop_requested && !g.tracker.done && g.stuck.is_none();
                let pid = g.pid;
                let root = g.root.clone();
                drop(g);
                if starting
                    && looked.elapsed() >= Duration::from_secs(5)
                    && let Some(pid) = pid
                {
                    looked = Instant::now();
                    let now = crate::time::now();
                    if let Some(stall) = watch.check(now, crate::stuckstart::cpu_time_ms(pid), crate::stuckstart::settings()) {
                        look_stuck(root, pid, stall, shared.clone(), run);
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });
    if let Err(e) = spawned {
        log::warn!("no reader for the server's console: {e}");
    }
}

/// Takes and reads a thread dump of a stuck start, off the console thread,
/// and keeps what it found for the panel.
fn look_stuck(root: PathBuf, pid: u32, stall: crate::stuckstart::Stall, shared: Arc<Mutex<Shared>>, run: u64) {
    let _ = std::thread::Builder::new().name("server-stuck".into()).spawn(move || {
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let report = crate::stuckstart::investigate(&root, &name, pid, &stall, crate::time::now());
        let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
        if g.run != run || g.tracker.done || g.stop_requested {
            return;
        }
        let saved = report
            .dump
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| format!(" Thread dump saved: crash-reports/{} (Logs & crashes).", n.to_string_lossy()))
            .unwrap_or_default();
        g.note(&format!("[Hopper] {}.{saved}", report.headline));
        g.stuck = Some(report);
    });
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

/// The server's process by pid on Unix, so a restarted Hopper can watch,
/// stop and talk to a server it didn't start (or did, before an update
/// started the new program in its place).
#[cfg(unix)]
mod ux {
    use std::fs::{File, Metadata, OpenOptions};
    use std::io;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::Path;
    use std::time::{Duration, Instant};

    fn me() -> u32 {
        unsafe { libc::geteuid() }
    }

    /// The process's state letter and start time, from /proc.
    fn stat(pid: u32) -> Option<(char, u64)> {
        super::parse_proc_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
    }

    pub struct Process {
        pid: u32,
        /// Its start time ([`stat`]), 0 when /proc couldn't say.
        start: u64,
        ended: Option<Option<i32>>,
    }

    impl Process {
        /// Process `pid`, if it is running and started at `start` (a reused
        /// pid belongs to some other program).
        pub fn open(pid: u32, start: u64) -> Option<Process> {
            let p = Process { pid, start, ended: None };
            p.same().then_some(p)
        }

        /// A child just started.
        pub fn spawned(pid: u32) -> Process {
            Process { pid, start: stat(pid).map_or(0, |(_, s)| s), ended: None }
        }

        pub fn created(&self) -> u64 {
            self.start
        }

        /// Still the process it was: there, not ended, the same start time.
        fn same(&self) -> bool {
            self.start != 0 && matches!(stat(self.pid), Some((st, s)) if s == self.start && !matches!(st, 'Z' | 'X' | 'x'))
        }

        /// `Some(exit code)` once it has ended. A process Hopper didn't start
        /// (in this program) has no exit code to read: just "ended".
        pub fn try_wait(&mut self) -> io::Result<Option<Option<i32>>> {
            if let Some(e) = self.ended {
                return Ok(Some(e));
            }
            let mut status = 0;
            let r = unsafe { libc::waitpid(self.pid as libc::pid_t, &mut status, libc::WNOHANG) };
            let ended = if r == self.pid as libc::pid_t {
                Some(libc::WIFEXITED(status).then(|| libc::WEXITSTATUS(status)))
            } else if r == 0 {
                None
            } else {
                // Not Hopper's child: /proc says whether it still runs.
                (!self.same()).then_some(None)
            };
            self.ended = ended;
            Ok(ended)
        }

        fn signal(&self, sig: libc::c_int) -> bool {
            self.ended.is_none() && self.same() && unsafe { libc::kill(self.pid as libc::pid_t, sig) } == 0
        }

        /// Asks it to shut down (SIGTERM).
        pub fn terminate(&self) -> bool {
            self.signal(libc::SIGTERM)
        }

        /// Ends it (SIGKILL) and waits up to 5 s for it to go.
        pub fn kill(&mut self) {
            self.signal(libc::SIGKILL);
            let end = Instant::now() + Duration::from_secs(5);
            while matches!(self.try_wait(), Ok(None)) && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    /// The process runs in the server folder `root`.
    pub fn runs_in(pid: u32, root: &Path) -> bool {
        match (std::fs::read_link(format!("/proc/{pid}/cwd")), std::fs::canonicalize(root)) {
            (Ok(cwd), Ok(root)) => cwd == root,
            _ => false,
        }
    }

    fn same_file(a: &Metadata, b: &Metadata) -> bool {
        a.dev() == b.dev() && a.ino() == b.ino()
    }

    /// The process's stdin is the FIFO at `fifo`.
    pub fn stdin_is(pid: u32, fifo: &Path) -> bool {
        match (std::fs::metadata(format!("/proc/{pid}/fd/0")), std::fs::symlink_metadata(fifo)) {
            (Ok(a), Ok(b)) => b.file_type().is_fifo() && same_file(&a, &b),
            _ => false,
        }
    }

    /// Makes `dir` (0700) if it's missing, and refuses it unless it is this
    /// user's own folder, not a link, that nobody else can get into.
    fn private_dir(dir: &Path) -> io::Result<()> {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        let m = std::fs::symlink_metadata(dir)?;
        if !m.is_dir() || m.uid() != me() || m.mode() & 0o077 != 0 {
            return Err(io::Error::other(format!("{} isn't a folder only this user can open", dir.display())));
        }
        Ok(())
    }

    /// Opens the FIFO at `path` without following a link, checks it's this
    /// user's FIFO, makes it owner-only and blocking again (the open itself
    /// is non-blocking so it can't hang).
    fn open_fifo(path: &Path, read: bool) -> io::Result<File> {
        let f = OpenOptions::new()
            .read(read)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let m = f.metadata()?;
        if !m.file_type().is_fifo() || m.uid() != me() {
            return Err(io::Error::other(format!("{} isn't Hopper's console FIFO", path.display())));
        }
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let fd = f.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(f)
    }

    /// The FIFO at `path`, made or reused (an old one holds nothing once
    /// nobody has it open): the server's end, opened read-write so its
    /// input never ends while it runs, and Hopper's write end. Something
    /// else at `path` (a file, a link) is left alone and refused.
    pub fn fifo(path: &Path) -> io::Result<(File, File)> {
        private_dir(path.parent().ok_or_else(|| io::Error::other("no folder for the FIFO"))?)?;
        match std::fs::symlink_metadata(path) {
            Ok(m) if m.file_type().is_fifo() && m.uid() == me() => {}
            Ok(_) => return Err(io::Error::other(format!("{} is in the way", path.display()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                use std::os::unix::ffi::OsStrExt;
                let c = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
                if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Err(e) => return Err(e),
        }
        let server = open_fifo(path, true)?;
        let hopper = writer(path)?;
        Ok((server, hopper))
    }

    /// Hopper's write end of a running server's FIFO. Fails (rather than
    /// waits) when nothing reads it any more.
    pub fn writer(path: &Path) -> io::Result<File> {
        open_fifo(path, false)
    }

    /// Removes the FIFO at `path`, and nothing else that might be there.
    pub fn remove_fifo(path: &Path) {
        if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_fifo()) {
            let _ = std::fs::remove_file(path);
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
        // Join times follow joins and leaves; a name only seen in `list` has none.
        assert_eq!(t.joined.keys().collect::<Vec<_>>(), ["Steve"]);
        t.push("[10:03:00] [Server thread/INFO]: There are 2 of a max of 10 players online: Bob, Steve".into());
        assert_eq!(t.players.len(), 2);
        assert!(t.joined.contains_key("Steve") && !t.joined.contains_key("Bob"));
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

    /// Whatever sees the process end reads its last lines into the console
    /// first, with no console thread to catch them later: a server's
    /// "All dimensions are saved", written just before it exits, is never
    /// missing once it counts as stopped.
    #[test]
    fn the_exit_check_reads_the_last_lines_first() {
        let dir = std::env::temp_dir().join(format!("hopper-host-drain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(CONSOLE_FILE);
        // What the process wrote before it exited.
        std::fs::write(&path, format!("{DONE}\n{STOPPING}\n{SAVED}\n")).unwrap();
        #[cfg(windows)]
        let child = Command::new("cmd").args(["/c", "exit 0"]).spawn().unwrap();
        #[cfg(unix)]
        let child = Command::new("true").spawn().unwrap();
        #[cfg(windows)]
        let proc = win::Process::open(child.id()).unwrap();
        #[cfg(unix)]
        let proc = ux::Process::spawned(child.id());
        let mut g = Shared { proc: Some(proc), feed: Feed::open(&path), ..Default::default() };
        let end = Instant::now() + Duration::from_secs(20);
        while g.exit.is_none() && Instant::now() < end {
            reap(&mut g);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(g.exit, Some(Some(0)));
        assert!(g.tracker.lines.iter().any(|l| l == SAVED), "{:?}", g.tracker.lines);
        assert!(g.tracker.saved_at.is_some());
        drop(child);
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

    /// RCON only between "Done" and the stop request (Stop, Restart and
    /// schedules all send "stop"; Kill ends the process).
    #[cfg(windows)]
    #[test]
    fn rcon_only_between_done_and_the_stop() {
        let (s, dir) = hanging("rcon-starting", &[]);
        assert_eq!(s.state(), ServerState::Starting);
        assert!(!s.rcon_allowed(), "before Done");
        s.kill().unwrap();
        assert!(!s.rcon_allowed(), "killed");
        let _ = std::fs::remove_dir_all(&dir);

        let (s, dir) = hanging("rcon-stop", &[DONE]);
        assert!(wait_for(&s, DONE), "{:?}", s.console());
        assert!(s.rcon_allowed());
        s.stop().unwrap();
        assert!(!s.rcon_allowed(), "right after the stop request");
        s.kill().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `/stop` Hopper didn't send (in game, another tool): the console's
    /// "Stopping server" cuts RCON though the state still says running.
    #[cfg(windows)]
    #[test]
    fn rcon_ends_when_the_console_says_stopping() {
        let (s, dir) = hanging("rcon-console", &[DONE, STOPPING]);
        assert!(wait_for(&s, STOPPING), "{:?}", s.console());
        assert_eq!(s.state(), ServerState::Running);
        assert!(!s.rcon_allowed());
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

    const SAVING_LINE: &str = "[10:01:00] [Server thread/INFO]: Saving chunks for level 'ServerLevel[world]'/minecraft:overworld";

    /// A tracker fed `lines` as if each came `at` seconds after the stop.
    fn heard(asked: Instant, lines: &[(u64, &str)]) -> Tracker {
        let mut t = Tracker::default();
        for &(at, l) in lines {
            let (saving, saved) = (t.saving_at.take(), t.saved_at.take());
            t.push(l.into());
            // The times push just set (it uses the clock) become `at`.
            let when = Some(asked + Duration::from_secs(at));
            t.heard_at = when;
            t.saving_at = if t.saving_at.is_some() { when } else { saving };
            t.saved_at = if t.saved_at.is_some() { when } else { saved };
        }
        t
    }

    #[test]
    fn a_silent_server_gets_the_short_wait() {
        let w = StopWait::default();
        let asked = Instant::now();
        let t = heard(asked, &[(0, DONE)]);
        assert_eq!(w.deadline(asked, &t), asked + STOP_SILENT);
        // Any line before the save starts still counts as progress.
        let t = heard(asked, &[(0, DONE), (0, STOPPING), (50, "[x] [Server thread/INFO]: Unloading mods")]);
        assert_eq!(w.deadline(asked, &t), asked + Duration::from_secs(50) + STOP_SILENT);
    }

    #[test]
    fn saving_lines_push_the_deadline_back() {
        let w = StopWait::default();
        let asked = Instant::now();
        let t = heard(asked, &[(0, DONE), (0, STOPPING), (5, SAVING_LINE)]);
        assert!(t.saving_at.is_some() && t.saved_at.is_none());
        assert_eq!(w.deadline(asked, &t), asked + Duration::from_secs(5) + SAVE_QUIET);
        // Past the silent cap, still saving: not given up on.
        let t = heard(asked, &[(0, DONE), (0, STOPPING), (5, SAVING_LINE), (200, SAVING_LINE)]);
        assert_eq!(w.deadline(asked, &t), asked + Duration::from_secs(200) + SAVE_QUIET);
        // But never past the hard cap.
        let t = heard(asked, &[(0, DONE), (0, STOPPING), (5, SAVING_LINE), (500, SAVING_LINE)]);
        assert_eq!(w.deadline(asked, &t), asked + STOP_HARD_CAP);
    }

    #[test]
    fn a_save_before_the_stop_is_not_saving() {
        let mut t = Tracker::default();
        t.push(DONE.into());
        t.push("[x] [Server thread/INFO]: Saving the game (this may take a moment!)".into());
        t.push(SAVING_LINE.into());
        assert!(t.saving_at.is_none());
    }

    #[test]
    fn once_saved_the_linger_takes_over() {
        let w = StopWait::default();
        let asked = Instant::now();
        let t = heard(asked, &[(0, DONE), (0, STOPPING), (5, SAVING_LINE), (400, SAVED)]);
        assert!(t.saved_at.is_some());
        // Past the hard cap is fine here: the worlds are saved.
        assert_eq!(w.deadline(asked, &t), asked + Duration::from_secs(400) + LINGER_AFTER_STOP + Duration::from_secs(5));
    }

    #[test]
    fn wait_texts() {
        assert_eq!(wait_text(Duration::from_secs(1)), "1 second");
        assert_eq!(wait_text(Duration::from_secs(45)), "45 seconds");
        assert_eq!(wait_text(Duration::from_secs(600)), "10 minutes");
        assert_eq!(wait_text(Duration::from_secs(61)), "1 minute 1 second");
    }

    /// A fake server: says it's ready, waits for "stop", then runs `after`
    /// (PowerShell) and exits. Short stop waits so tests are quick.
    #[cfg(windows)]
    fn stoppable(tag: &str, after: &str) -> (LocalServer, PathBuf) {
        let dir = std::env::temp_dir().join(format!("hopper-host-stopwait-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let s = LocalServer::new(&dir);
        s.set_linger(Duration::from_secs(2));
        s.set_stop_wait(StopWait {
            hard: Duration::from_secs(30),
            silent: Duration::from_secs(2),
            saving_quiet: Duration::from_secs(3),
            linger: Duration::from_secs(2),
        });
        let say = |l: &str| format!("[Console]::Out.WriteLine('{}'); [Console]::Out.Flush(); ", l.replace('\'', "''"));
        let after = after.replace("SAY ", "").replace("STOPPING", &say(STOPPING)).replace("SAVING", &say(SAVING_LINE)).replace("SAVED", &say(SAVED));
        let script = format!(
            "{}while (($l = [Console]::In.ReadLine()) -ne $null) {{ if ($l -eq 'stop') {{ break }} }}; {after}",
            say(DONE)
        );
        s.spawn(&Launch { note: None, java: "powershell".into(), args: vec!["-NoProfile".into(), "-Command".into(), script] })
            .unwrap();
        assert!(wait_for(&s, DONE), "{:?}", s.console());
        (s, dir)
    }

    /// Saving slowly, quiet between save lines for longer than the silent
    /// cap: the stop waits for it and the server exits on its own.
    #[cfg(windows)]
    #[test]
    fn a_slow_save_is_waited_for() {
        let (s, dir) = stoppable("slow", "STOPPING SAVING Start-Sleep -Seconds 6; SAVING Start-Sleep -Seconds 6; SAVED exit 0");
        // Its own waits: 6 s between save lines is past the silent cap, so
        // only the saving allowance keeps it alive. That allowance is far
        // above the gap: a busy CI box can stall the stand-in for seconds,
        // and the 3 s one ended it mid-save there.
        s.set_stop_wait(StopWait {
            hard: Duration::from_secs(90),
            silent: Duration::from_secs(4),
            saving_quiet: Duration::from_secs(30),
            linger: Duration::from_secs(2),
        });
        s.stop().unwrap();
        assert!(wait_dead(&s, Duration::from_secs(60)), "{:?}", s.console());
        assert!(s.console().iter().any(|l| l == SAVED), "{:?}", s.console());
        std::thread::sleep(Duration::from_millis(400));
        let console = s.console();
        assert_eq!(s.state(), ServerState::Stopped);
        assert!(console.iter().any(|l| l == SAVING_NOTE), "{console:?}");
        assert!(!console.iter().any(|l| l.contains("ended it")), "{console:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Says nothing after the stop: ended after the short cap.
    #[cfg(windows)]
    #[test]
    fn a_silent_stop_is_ended_after_the_short_wait() {
        let (s, dir) = stoppable("silent", "Start-Sleep -Seconds 60");
        let t = Instant::now();
        s.stop().unwrap();
        assert!(wait_dead(&s, Duration::from_secs(15)), "{:?}", s.console());
        assert!(t.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(400));
        let console = s.console();
        assert!(console.iter().any(|l| l.contains("didn't stop in") && l.contains("wasn't saving")), "{console:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Starts saving, then hangs mid-save: ended, and the console says it
    /// was saving.
    #[cfg(windows)]
    #[test]
    fn a_hung_save_is_ended_and_says_so() {
        let (s, dir) = stoppable("hung", "STOPPING SAVING Start-Sleep -Seconds 60");
        s.stop().unwrap();
        assert!(wait_dead(&s, Duration::from_secs(15)), "{:?}", s.console());
        std::thread::sleep(Duration::from_millis(400));
        let console = s.console();
        assert!(console.iter().any(|l| l.contains("still saving its worlds") && l.contains("seconds after the stop")), "{console:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Generous: CI starts several PowerShell stand-ins at once, and a cold
    // PowerShell there can take well over 20 s to print its first line.
    fn wait_for(s: &LocalServer, line: &str) -> bool {
        let end = Instant::now() + Duration::from_secs(60);
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

    #[test]
    fn proc_stat_gives_the_state_and_start_time() {
        // Field 22 is 987654; the name has spaces and brackets of its own.
        let line = "1234 (java (x) y) S 1 1234 1234 0 -1 4194560 100 0 0 0 10 5 0 0 20 0 30 0 987654 123456 789 18446744073709551615";
        assert_eq!(parse_proc_stat(line), Some(('S', 987654)));
        assert_eq!(parse_proc_stat("1 (init) Z 0 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 42\n"), Some(('Z', 42)));
        assert_eq!(parse_proc_stat("1 (short) S 0 1"), None);
        assert_eq!(parse_proc_stat("no brackets"), None);
    }

    #[test]
    fn fifo_names_are_stable_and_per_folder() {
        // FNV-1a: fixed values, so a new build finds an old run's FIFO.
        assert_eq!(fifo_name(Path::new("")), "cbf29ce484222325.stdin");
        assert_eq!(fifo_name(Path::new("a")), "af63dc4c8601ec8c.stdin");
        assert_ne!(fifo_name(Path::new("/srv/one")), fifo_name(Path::new("/srv/two")));
    }

    /// A scratch folder for the FIFOs, set once for every test (the setting
    /// is global; each server's FIFO is named by its own folder).
    #[cfg(unix)]
    fn test_stdin_dir() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| set_stdin_dir(std::env::temp_dir().join(format!("hopper-test-stdin-{}", std::process::id()))));
    }

    #[cfg(unix)]
    #[test]
    fn fifos_are_made_reused_and_never_taken_over() {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};
        let dir = std::env::temp_dir().join(format!("hopper-fifo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fifo = dir.join("x.stdin");
        let (server, hopper) = ux::fifo(&fifo).unwrap();
        let m = std::fs::symlink_metadata(&fifo).unwrap();
        assert!(m.file_type().is_fifo());
        assert_eq!(m.permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        drop((server, hopper));
        // Left over from an earlier run: used again.
        let (mut server, mut hopper) = ux::fifo(&fifo).unwrap();
        hopper.write_all(b"list\n").unwrap();
        let mut buf = [0u8; 5];
        server.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"list\n");
        // Hopper's end closing doesn't end the server's input: a new one writes on.
        drop(hopper);
        ux::writer(&fifo).unwrap().write_all(b"stop\n").unwrap();
        server.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"stop\n");
        drop(server);
        // Nothing reads it any more: no writer (rather than a hang).
        assert!(ux::writer(&fifo).is_err());
        ux::remove_fifo(&fifo);
        assert!(!fifo.exists());
        // A file or a link in its place is refused and left alone.
        std::fs::write(&fifo, "keep").unwrap();
        assert!(ux::fifo(&fifo).is_err());
        assert!(ux::writer(&fifo).is_err());
        ux::remove_fifo(&fifo);
        assert_eq!(std::fs::read_to_string(&fifo).unwrap(), "keep");
        std::fs::remove_file(&fifo).unwrap();
        let target = dir.join("target");
        std::fs::write(&target, "keep").unwrap();
        std::os::unix::fs::symlink(&target, &fifo).unwrap();
        assert!(ux::fifo(&fifo).is_err());
        assert!(ux::writer(&fifo).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
        // A folder others can open is refused.
        let open = dir.join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(ux::fifo(&open.join("y.stdin")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A run file pointing at some other process (a reused pid, a hand
    /// edit) is never taken for the server.
    #[cfg(unix)]
    #[test]
    fn a_run_file_for_another_process_is_ignored() {
        test_stdin_dir();
        let dir = std::env::temp_dir().join(format!("hopper-host-notours-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // This test program: running, but not in the server folder, and
        // with the wrong start time.
        let info = RunInfo { pid: std::process::id(), created: 1, started: 0, stdin: 0, fifo: true, stop_requested: false };
        std::fs::write(dir.join(RUN_FILE), serde_json::to_string(&info).unwrap()).unwrap();
        let s = LocalServer::new(&dir);
        assert!(!s.state().is_alive());
        assert!(!dir.join(RUN_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Unix: a new LocalServer (a restarted Hopper) picks up a process the
    /// old one started: console from the start, commands through the FIFO,
    /// kill, and the FIFO removed once it has ended.
    #[cfg(unix)]
    #[test]
    fn reattaches_after_hopper_restarts_unix() {
        use std::os::unix::fs::PermissionsExt;
        test_stdin_dir();
        let dir = std::env::temp_dir().join(format!("hopper-host-reattach-unix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let first = LocalServer::new(&dir);
        // Echoes every stdin line, like a console answering.
        let echo = "while IFS= read -r l; do echo \"got $l\"; done";
        first.spawn(&Launch { note: None, java: "sh".into(), args: vec!["-c".into(), echo.into()] }).unwrap();
        assert!(first.reattachable());
        first.command("hello").unwrap();
        assert!(wait_for(&first, "got hello"), "{:?}", first.console());
        let pid = first.pid().unwrap();
        drop(first);
        assert!(dir.join(RUN_FILE).exists());
        assert_eq!(std::fs::metadata(dir.join(RUN_FILE)).unwrap().permissions().mode() & 0o777, 0o600);
        // Its own session, away from Hopper's process group.
        assert_eq!(unsafe { libc::getsid(pid as libc::pid_t) }, pid as libc::pid_t);

        let second = LocalServer::new(&dir);
        assert!(second.state().is_alive());
        assert_eq!(second.pid(), Some(pid));
        assert!(second.reattachable());
        assert!(wait_for(&second, "got hello"), "{:?}", second.console());
        second.command("world").unwrap();
        assert!(wait_for(&second, "got world"), "{:?}", second.console());
        assert!(second.console().iter().any(|l| l == "> world"));
        second.kill().unwrap();
        assert_eq!(second.state(), ServerState::Stopped);
        assert!(!dir.join(RUN_FILE).exists());
        assert!(!stdin_fifo(&dir).exists(), "the FIFO goes with the server");
        assert!(!LocalServer::new(&dir).state().is_alive());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Picked up without a console (its FIFO gone): Stop asks the process
    /// to shut down (SIGTERM) instead of killing it at once.
    #[cfg(unix)]
    #[test]
    fn a_server_without_a_console_is_stopped_by_signal() {
        test_stdin_dir();
        let dir = std::env::temp_dir().join(format!("hopper-host-sigterm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let first = LocalServer::new(&dir);
        let script = "trap 'echo bye; exit 0' TERM; echo up; while :; do sleep 0.1; done";
        first.spawn(&Launch { note: None, java: "sh".into(), args: vec!["-c".into(), script.into()] }).unwrap();
        assert!(wait_for(&first, "up"), "{:?}", first.console());
        drop(first);
        ux::remove_fifo(&stdin_fifo(&dir));

        let second = LocalServer::new(&dir);
        assert!(second.state().is_alive());
        assert!(!second.reattachable(), "no console: an update would have to wait for it");
        assert!(second.command("list").is_err());
        second.stop().unwrap();
        assert!(wait_for(&second, "bye"), "{:?}", second.console());
        let end = Instant::now() + Duration::from_secs(10);
        while second.state().is_alive() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(second.state(), ServerState::Stopped);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
