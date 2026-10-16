//! Running a hosted server: one java process per server folder, started
//! without a console window, its output kept in a ring buffer, commands
//! sent through stdin.

use super::logparse::{LogEvent, parse_line};
use super::script::{Launch, launch_command};
use anyhow::{Context as _, bail};
use std::collections::{BTreeSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

/// Console lines kept per server.
pub const MAX_LINES: usize = 2000;
/// How long a graceful stop waits for the world to save before killing.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(60);

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
            Some(LogEvent::Stopping) | None => {}
        }
        if self.lines.len() >= MAX_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }
}

#[derive(Default)]
struct Shared {
    tracker: Tracker,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    pid: Option<u32>,
    stop_requested: bool,
    started_at: Option<SystemTime>,
    exit: Option<Option<i32>>,
    /// Bumped per start, so an old run's threads leave a new run alone.
    run: u64,
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

/// A server Hopper runs on this PC, by folder.
pub struct LocalServer {
    root: PathBuf,
    shared: Arc<Mutex<Shared>>,
    sys: Mutex<sysinfo::System>,
}

impl LocalServer {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        LocalServer {
            root: root.into(),
            shared: Default::default(),
            sys: Mutex::new(sysinfo::System::new()),
        }
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
        state_of(g.child.is_some(), g.tracker.done, g.stop_requested, g.exit)
    }

    /// The console so far, oldest first.
    pub fn console(&self) -> Vec<String> {
        self.lock().tracker.lines.iter().cloned().collect()
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
        g.child.as_ref().and(g.pid)
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
            bail!("Port {port} is in use. Stop whatever uses it or change the server's port.");
        }
        let launch = launch_command(&self.root)?;
        self.spawn(&launch)
    }

    /// Starts `launch` in the server folder (no window, piped streams).
    pub fn spawn(&self, launch: &Launch) -> anyhow::Result<()> {
        let mut cmd = Command::new(&launch.java);
        cmd.args(&launch.args)
            .current_dir(&self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("starting {}", launch.java.display()))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let run = {
            let mut g = self.lock();
            g.run += 1;
            g.tracker = Tracker::default();
            g.tracker.push(format!(
                "[Hopper] Starting: {} {}",
                launch.java.display(),
                launch.args.join(" ")
            ));
            g.stdin = child.stdin.take();
            g.pid = Some(child.id());
            g.child = Some(child);
            g.stop_requested = false;
            g.exit = None;
            g.started_at = Some(SystemTime::now());
            g.run
        };
        if let Some(out) = stdout {
            read_lines(out, self.shared.clone(), run, "stdout");
        }
        if let Some(err) = stderr {
            read_lines(err, self.shared.clone(), run, "stderr");
        }
        Ok(())
    }

    /// Sends a console command (leading slash dropped).
    pub fn command(&self, line: &str) -> anyhow::Result<()> {
        let line = line.trim().trim_start_matches('/');
        if line.is_empty() {
            return Ok(());
        }
        let mut g = self.lock();
        reap(&mut g);
        if g.child.is_none() {
            bail!("The server isn't running.");
        }
        let stdin = g.stdin.as_mut().context("the server's console is closed")?;
        stdin.write_all(format!("{line}\n").as_bytes())?;
        stdin.flush()?;
        if line == "stop" {
            g.stop_requested = true;
        }
        g.tracker.push(format!("> {line}"));
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
                        if g.run != run || g.child.is_none() {
                            return;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
                let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
                if g.run == run {
                    log::warn!("server did not stop in {timeout:?}; killing it");
                    g.tracker.push("[Hopper] The server didn't stop in time; ending it.".into());
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
        kill_locked(&mut g);
        Ok(())
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

/// Notes the exit if the process has ended.
fn reap(g: &mut Shared) {
    let Some(child) = g.child.as_mut() else { return };
    match child.try_wait() {
        Ok(Some(status)) => {
            g.exit = Some(status.code());
            g.child = None;
            g.stdin = None;
            let msg = match status.code() {
                Some(c) => format!("[Hopper] The server process ended (exit code {c})."),
                None => "[Hopper] The server process ended.".to_string(),
            };
            g.tracker.push(msg);
        }
        Ok(None) => {}
        Err(e) => log::warn!("checking the server process: {e}"),
    }
}

fn kill_locked(g: &mut Shared) {
    if let Some(child) = g.child.as_mut() {
        g.stop_requested = true;
        let _ = child.kill();
        let _ = child.wait();
    }
    reap(g);
}

/// Feeds a stream into the tracker, line by line (lossy UTF-8, CR trimmed).
fn read_lines(stream: impl Read + Send + 'static, shared: Arc<Mutex<Shared>>, run: u64, name: &str) {
    let spawned = std::thread::Builder::new()
        .name(format!("server-{name}"))
        .spawn(move || {
            let mut r = BufReader::new(stream);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match r.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let line = String::from_utf8_lossy(&buf)
                    .trim_end_matches(['\r', '\n'])
                    .to_string();
                let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
                if g.run != run {
                    break;
                }
                g.tracker.push(line);
            }
        });
    if let Err(e) = spawned {
        log::warn!("no reader for the server's {name}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let _ = std::fs::remove_dir_all(&dir);
    }
}
