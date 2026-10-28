//! Scheduled tasks for hosted servers: a daily restart with chat warnings,
//! restarting after a crash (up to a limit per hour) and starting servers
//! with Hopper.
//!
//! Each server keeps its [`Schedule`] in `.hopper-schedule.json`. A
//! [`Scheduler`] runs one background thread over a [`LocalPanel`]; the app
//! drains what it did with [`Scheduler::events`] to show notifications.
//!
//! The timing lives in [`Core::tick`], which takes the time as a parameter
//! and the servers through the small [`ScheduledServer`] trait, so the tests
//! step through a day with fake servers instead of sleeping.

use super::local_panel::LocalPanel;
use super::process::{LocalServer, ServerState};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

const SCHEDULE_FILE: &str = ".hopper-schedule.json";
/// How often the scheduler looks at the servers.
pub const TICK: Duration = Duration::from_secs(20);
/// How often while a restart waits for its server to stop.
const FAST_TICK: Duration = Duration::from_secs(1);
/// How late a daily restart may still happen (Hopper busy, PC asleep).
/// Later than this it is skipped for the day rather than surprising players.
const RESTART_GRACE_SECS: i64 = 5 * 60;
const DAY: i64 = 86_400;
const HOUR: i64 = 3_600;

/// A server's scheduled tasks, from `<server>/.hopper-schedule.json`.
/// Every field has a default, so an empty or partial file works.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Schedule {
    /// Restart every day at "HH:MM", local time. `None`: no daily restart.
    pub daily_restart: Option<String>,
    /// Minutes before the daily restart to warn players in chat.
    pub warn_minutes: Vec<u32>,
    /// Start the server again when it crashes.
    pub auto_restart_on_crash: bool,
    /// Crash restarts allowed in any hour before giving up. 0: none.
    pub max_crash_restarts: u32,
    /// Start the server when Hopper starts.
    pub start_with_hopper: bool,
    /// Back up every this many hours while the server runs. 0: never.
    pub backup_every_hours: u32,
    /// Scheduled backups kept; older ones are deleted.
    pub backup_keep: u32,
    /// What scheduled backups hold.
    pub backup_scope: super::backup::Scope,
}

impl Default for Schedule {
    fn default() -> Self {
        Schedule {
            daily_restart: None,
            warn_minutes: vec![10, 5, 1],
            auto_restart_on_crash: true,
            max_crash_restarts: 3,
            start_with_hopper: false,
            backup_every_hours: 0,
            backup_keep: 12,
            backup_scope: super::backup::Scope::Worlds,
        }
    }
}

impl Schedule {
    /// The server's schedule; the defaults when it has no file (or a
    /// broken one, which is logged), so the settings form and the scheduler
    /// always agree on what applies.
    pub fn load(root: &Path) -> Self {
        let path = root.join(SCHEDULE_FILE);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_else(|e| {
            log::warn!("{}: {e}; using the default schedule", path.display());
            Self::default()
        })
    }

    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        std::fs::write(root.join(SCHEDULE_FILE), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// The daily restart as minutes after midnight, if set and valid.
    pub fn restart_minute(&self) -> Option<u32> {
        self.daily_restart.as_deref().and_then(parse_hhmm)
    }

    /// What's wrong with the schedule, for the settings form.
    pub fn validate(&self) -> Result<(), String> {
        match self.daily_restart.as_deref() {
            Some(t) if !t.trim().is_empty() && parse_hhmm(t).is_none() => {
                Err(format!("\"{t}\" isn't a time. Use HH:MM, e.g. 04:00."))
            }
            _ => Ok(()),
        }
    }
}

/// "04:00" or "4:00" to minutes after midnight.
pub fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    if !(1..=2).contains(&h.len()) || m.len() != 2 {
        return None;
    }
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

fn hhmm(minute: u32) -> String {
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

/// Local wall-clock time as seconds since 1970-01-01 00:00 local (so
/// `secs / 86400` is the local day). The clock the scheduler runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LocalTime(pub i64);

impl LocalTime {
    /// Now, in this PC's time zone (UTC where Hopper can't tell).
    pub fn now() -> Self {
        let utc = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        LocalTime(utc + utc_offset_secs())
    }

    /// From "2026-10-12T04:00" (local), for tests and the UI.
    pub fn parse(s: &str) -> Option<Self> {
        crate::time::parse_iso(s).map(LocalTime)
    }

    pub fn day(self) -> i64 {
        self.0.div_euclid(DAY)
    }

    /// Minutes after midnight.
    pub fn minute_of_day(self) -> u32 {
        (self.0.rem_euclid(DAY) / 60) as u32
    }

    /// "04:00".
    pub fn hhmm(self) -> String {
        hhmm(self.minute_of_day())
    }
}

/// Local time minus UTC, from what Windows says both are now.
#[cfg(windows)]
fn utc_offset_secs() -> i64 {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTimeRaw {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        millis: u16,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLocalTime(st: *mut SystemTimeRaw);
        fn GetSystemTime(st: *mut SystemTimeRaw);
    }
    let (mut local, mut utc) = (SystemTimeRaw::default(), SystemTimeRaw::default());
    // SAFETY: both fill the SYSTEMTIME they are given, which is a valid,
    // writable struct of the right layout.
    unsafe {
        GetLocalTime(&mut local);
        GetSystemTime(&mut utc);
    }
    let secs = |t: &SystemTimeRaw| {
        crate::time::parse_iso(&format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}",
            t.year, t.month, t.day, t.hour, t.minute
        ))
        .map(|s| s + t.second as i64)
    };
    match (secs(&local), secs(&utc)) {
        // Offsets are whole minutes; rounding absorbs a second ticking
        // between the two calls.
        (Some(l), Some(u)) => ((l - u) as f64 / 60.0).round() as i64 * 60,
        _ => 0,
    }
}

#[cfg(not(windows))]
fn utc_offset_secs() -> i64 {
    0
}

/// The next daily restart after `now`, if the schedule has one.
pub fn next_restart(schedule: &Schedule, now: LocalTime) -> Option<LocalTime> {
    let minute = schedule.restart_minute()? as i64;
    let today = LocalTime(now.day() * DAY + minute * 60);
    Some(if today > now { today } else { LocalTime(today.0 + DAY) })
}

/// "04:00 today" or "04:00 tomorrow", for "Next restart: …".
pub fn describe_next_restart(schedule: &Schedule, now: LocalTime) -> Option<String> {
    let next = next_restart(schedule, now)?;
    let when = if next.day() == now.day() { "today" } else { "tomorrow" };
    Some(format!("{} {when}", next.hhmm()))
}

/// What the scheduler did, for the app to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScheduleEvent {
    /// Players were told the server restarts in `minutes`.
    Warned { root: PathBuf, minutes: u32 },
    /// A scheduled restart finished (the server is starting again).
    Restarted { root: PathBuf, why: String },
    /// The server crashed and was started again; `attempt` within the hour.
    CrashRestarted { root: PathBuf, attempt: u32 },
    /// The server crashed `crashes` times in an hour; it is left stopped.
    GaveUp { root: PathBuf, crashes: u32 },
    /// Started with Hopper.
    AutoStarted { root: PathBuf },
    /// A scheduled backup was made (its file name).
    BackedUp { root: PathBuf, name: String },
    /// Something the scheduler tried failed.
    Failed { root: PathBuf, error: String },
}

impl ScheduleEvent {
    pub fn root(&self) -> &Path {
        match self {
            ScheduleEvent::Warned { root, .. }
            | ScheduleEvent::Restarted { root, .. }
            | ScheduleEvent::CrashRestarted { root, .. }
            | ScheduleEvent::GaveUp { root, .. }
            | ScheduleEvent::AutoStarted { root }
            | ScheduleEvent::BackedUp { root, .. }
            | ScheduleEvent::Failed { root, .. } => root,
        }
    }

    /// One line for a notification, e.g. "My Pack restarted (daily restart at 04:00)".
    pub fn message(&self) -> String {
        let name = self
            .root()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root().display().to_string());
        match self {
            ScheduleEvent::Warned { minutes, .. } => {
                format!("{name}: warned players, restart in {}", minutes_text(*minutes))
            }
            ScheduleEvent::Restarted { why, .. } => format!("{name} restarted ({why})"),
            ScheduleEvent::CrashRestarted { attempt, .. } => {
                format!("{name} crashed and was restarted (attempt {attempt} this hour)")
            }
            ScheduleEvent::GaveUp { crashes, .. } => format!(
                "{name} crashed {crashes} times within an hour; Hopper stopped restarting it"
            ),
            ScheduleEvent::AutoStarted { .. } => format!("{name} started with Hopper"),
            ScheduleEvent::BackedUp { name: file, .. } => format!("{name} backed up ({file})"),
            ScheduleEvent::Failed { error, .. } => format!("{name}: {error}"),
        }
    }
}

fn minutes_text(m: u32) -> String {
    if m == 1 { "1 minute".into() } else { format!("{m} minutes") }
}

/// The parts of a hosted server the scheduler uses; [`LocalServer`] in the
/// app, a fake in tests.
pub trait ScheduledServer {
    fn root(&self) -> &Path;
    fn state(&self) -> ServerState;
    fn start(&self) -> anyhow::Result<()>;
    /// Asks the server to stop (saving the world); returns at once.
    fn stop(&self) -> anyhow::Result<()>;
    fn command(&self, line: &str) -> anyhow::Result<()>;
    /// Makes a scheduled backup and drops old ones; returns its file name.
    fn backup(&self, _scope: super::backup::Scope, _keep: u32) -> anyhow::Result<String> {
        anyhow::bail!("backups aren't supported here")
    }
    /// When the last scheduled backup was made (Unix seconds).
    fn last_backup(&self) -> Option<i64> {
        None
    }
}

impl ScheduledServer for LocalServer {
    fn root(&self) -> &Path {
        LocalServer::root(self)
    }
    fn state(&self) -> ServerState {
        LocalServer::state(self)
    }
    fn start(&self) -> anyhow::Result<()> {
        LocalServer::start(self)
    }
    fn stop(&self) -> anyhow::Result<()> {
        LocalServer::stop(self)
    }
    fn command(&self, line: &str) -> anyhow::Result<()> {
        LocalServer::command(self, line)
    }
    fn backup(&self, scope: super::backup::Scope, keep: u32) -> anyhow::Result<String> {
        let root = LocalServer::root(self);
        let job = crate::jobs::Jobs::default().start("scheduled backup");
        let b = super::backup::make(root, scope, super::backup::Kind::Scheduled, "", Some(self), &job)?;
        super::backup::prune(root, keep as usize)?;
        Ok(b.name)
    }
    fn last_backup(&self) -> Option<i64> {
        super::backup::last_scheduled(LocalServer::root(self)).map(|t| t as i64)
    }
}

/// What the scheduler remembers about one server.
#[derive(Default)]
struct Track {
    /// (day of the restart, warning minutes) already sent.
    warned: BTreeSet<(i64, u32)>,
    /// Day of the last daily restart, so a day never gets two.
    last_restart_day: Option<i64>,
    /// A restart stopped the server; start it once it is down. The reason.
    pending_start: Option<String>,
    /// When crash restarts happened, within the last hour.
    crash_restarts: VecDeque<i64>,
    /// Why Hopper stopped restarting the server after crashes. Cleared
    /// once the server is no longer crashed (someone started it).
    gave_up: Option<String>,
    /// Last scheduled backup, in Unix seconds (read from the server's
    /// backups the first time).
    last_backup: Option<i64>,
}

/// The scheduler's logic without threads or a clock.
#[derive(Default)]
struct Core {
    tracks: BTreeMap<PathBuf, Track>,
}

impl Core {
    fn pending(&self) -> bool {
        self.tracks.values().any(|t| t.pending_start.is_some())
    }

    /// Looks at each server at `now` and does what is due.
    fn tick(&mut self, now: LocalTime, servers: &[(&dyn ScheduledServer, Schedule)]) -> Vec<ScheduleEvent> {
        let mut events = Vec::new();
        for (server, schedule) in servers {
            let root = server.root().to_path_buf();
            let track = self.tracks.entry(root.clone()).or_default();
            tick_server(track, now, *server, schedule, &mut events);
        }
        events
    }
}

fn tick_server(
    t: &mut Track,
    now: LocalTime,
    s: &dyn ScheduledServer,
    schedule: &Schedule,
    events: &mut Vec<ScheduleEvent>,
) {
    let root = s.root().to_path_buf();
    // Finding the mod that breaks it starts and stops the server itself.
    if crate::bisect::path(&root).is_file() {
        return;
    }
    let state = s.state();

    // A scheduled restart in progress: start once the server is down.
    if let Some(why) = &t.pending_start {
        if !state.is_alive() {
            let why = why.clone();
            t.pending_start = None;
            events.push(match s.start() {
                Ok(()) => ScheduleEvent::Restarted { root, why },
                Err(e) => ScheduleEvent::Failed {
                    root,
                    error: format!("couldn't start after the {why}: {e:#}"),
                },
            });
        }
        return;
    }

    // Crashes.
    if let ServerState::Crashed(code) = state {
        let limit = schedule.max_crash_restarts;
        if t.gave_up.is_none() && schedule.auto_restart_on_crash && limit > 0 {
            while t.crash_restarts.front().is_some_and(|&at| at <= now.0 - HOUR) {
                t.crash_restarts.pop_front();
            }
            let crashes = t.crash_restarts.len() as u32;
            if crashes >= limit {
                let code = code.map_or("no exit code".into(), |c| format!("exit code {c}"));
                t.gave_up = Some(format!(
                    "Crashed {} times within an hour (last: {code}); not restarting it again \
                     until it is started by hand.",
                    crashes + 1
                ));
                events.push(ScheduleEvent::GaveUp { root, crashes: crashes + 1 });
            } else {
                t.crash_restarts.push_back(now.0);
                events.push(match s.start() {
                    Ok(()) => ScheduleEvent::CrashRestarted { root, attempt: crashes + 1 },
                    Err(e) => ScheduleEvent::Failed {
                        root,
                        error: format!("couldn't restart after a crash: {e:#}"),
                    },
                });
            }
        }
        return;
    }
    t.gave_up = None;

    // Backups, every few hours while it runs.
    if schedule.backup_every_hours > 0 && state == ServerState::Running {
        let unix = now.0 - local_offset();
        let last = *t.last_backup.get_or_insert_with(|| s.last_backup().unwrap_or(0));
        if unix - last >= schedule.backup_every_hours as i64 * HOUR {
            t.last_backup = Some(unix);
            events.push(match s.backup(schedule.backup_scope, schedule.backup_keep) {
                Ok(name) => ScheduleEvent::BackedUp { root: root.clone(), name },
                Err(e) => ScheduleEvent::Failed { root: root.clone(), error: format!("scheduled backup failed: {e:#}") },
            });
        }
    }

    // The daily restart: look at yesterday's, today's and tomorrow's, so
    // warnings before a 00:05 restart and a 23:58 restart's grace both
    // work across midnight.
    let Some(minute) = schedule.restart_minute() else { return };
    let running = state == ServerState::Running;
    let max_warn = schedule.warn_minutes.iter().copied().max().unwrap_or(0) as i64;
    t.warned.retain(|&(day, _)| day >= now.day() - 1);
    for day in now.day() - 1..=now.day() + 1 {
        let at = day * DAY + minute as i64 * 60;
        if running && now.0 < at && now.0 >= at - max_warn * 60 {
            // Every warning that is due and unsent; send one, saying the
            // real time left (late ticks or a late start say "3 minutes",
            // not "10").
            let due: Vec<u32> = schedule
                .warn_minutes
                .iter()
                .copied()
                .filter(|&m| now.0 >= at - m as i64 * 60 && !t.warned.contains(&(day, m)))
                .collect();
            if !due.is_empty() {
                t.warned.extend(due.iter().map(|&m| (day, m)));
                let left = ((at - now.0 + 59) / 60) as u32;
                let text = format!("say Server restarts in {}", minutes_text(left));
                if s.command(&text).is_ok() {
                    events.push(ScheduleEvent::Warned { root: root.clone(), minutes: left });
                }
            }
        }
        if now.0 >= at && now.0 < at + RESTART_GRACE_SECS && t.last_restart_day != Some(day) && running {
            t.last_restart_day = Some(day);
            let _ = s.command("say Server is restarting now.");
            match s.stop() {
                Ok(()) => t.pending_start = Some(format!("daily restart at {}", hhmm(minute))),
                Err(e) => events.push(ScheduleEvent::Failed {
                    root: root.clone(),
                    error: format!("couldn't stop for the daily restart: {e:#}"),
                }),
            }
            return;
        }
    }
}

/// Local time minus Unix time, in seconds.
fn local_offset() -> i64 {
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    LocalTime::now().0 - unix
}

/// Starts the servers marked `start_with_hopper` that aren't running and
/// whose EULA is accepted.
fn autostart(servers: &[(&dyn ScheduledServer, Schedule)]) -> Vec<ScheduleEvent> {
    let mut events = Vec::new();
    for (s, schedule) in servers {
        if !schedule.start_with_hopper || s.state().is_alive() || crate::bisect::path(s.root()).is_file() {
            continue;
        }
        let root = s.root().to_path_buf();
        if !super::eula_accepted(&root) {
            events.push(ScheduleEvent::Failed {
                root,
                error: "not started with Hopper: accept the Minecraft EULA first".into(),
            });
            continue;
        }
        events.push(match s.start() {
            Ok(()) => ScheduleEvent::AutoStarted { root },
            Err(e) => ScheduleEvent::Failed { root, error: format!("couldn't start with Hopper: {e:#}") },
        });
    }
    events
}

struct Inner {
    core: Mutex<Core>,
    events: Mutex<Vec<ScheduleEvent>>,
    stop: Mutex<bool>,
    wake: Condvar,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Runs the schedules of every server in a [`LocalPanel`] on one
/// background thread. The thread ends on [`Scheduler::stop`] or drop.
pub struct Scheduler {
    inner: Arc<Inner>,
    thread: Option<JoinHandle<()>>,
}

impl Scheduler {
    /// Starts the thread; it looks at the servers now and every [`TICK`].
    pub fn start(panel: Arc<LocalPanel>) -> Self {
        let inner = Arc::new(Inner {
            core: Mutex::new(Core::default()),
            events: Mutex::new(Vec::new()),
            stop: Mutex::new(false),
            wake: Condvar::new(),
        });
        let worker = inner.clone();
        let thread = std::thread::Builder::new()
            .name("server-scheduler".into())
            .spawn(move || run(&worker, &panel))
            .map_err(|e| log::warn!("no server scheduler: {e}"))
            .ok();
        Scheduler { inner, thread }
    }

    /// What happened since the last call, oldest first.
    pub fn events(&self) -> Vec<ScheduleEvent> {
        std::mem::take(&mut *lock(&self.inner.events))
    }

    /// Why Hopper stopped restarting the server in `root` after crashes,
    /// while it is still down.
    pub fn gave_up(&self, root: &Path) -> Option<String> {
        lock(&self.inner.core).tracks.get(root).and_then(|t| t.gave_up.clone())
    }

    /// Starts the servers marked to start with Hopper (EULA accepted, not
    /// already running). What happened goes to [`Self::events`].
    pub fn start_autostart(&self, panel: &LocalPanel) {
        let servers = panel.all();
        let list: Vec<(&dyn ScheduledServer, Schedule)> = servers
            .iter()
            .map(|s| (&**s as &dyn ScheduledServer, Schedule::load(s.root())))
            .collect();
        let events = autostart(&list);
        lock(&self.inner.events).extend(events);
    }

    /// Ends the thread (it finishes the tick it is in).
    pub fn stop(&mut self) {
        *lock(&self.inner.stop) = true;
        self.inner.wake.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(inner: &Inner, panel: &LocalPanel) {
    loop {
        let servers = panel.all();
        let list: Vec<(&dyn ScheduledServer, Schedule)> = servers
            .iter()
            .map(|s| (&**s as &dyn ScheduledServer, Schedule::load(s.root())))
            .collect();
        let (events, pending) = {
            let mut core = lock(&inner.core);
            (core.tick(LocalTime::now(), &list), core.pending())
        };
        if !events.is_empty() {
            for e in &events {
                log::info!("scheduler: {}", e.message());
            }
            lock(&inner.events).extend(events);
        }
        drop(list);
        drop(servers);
        let wait = if pending { FAST_TICK } else { TICK };
        let stop = lock(&inner.stop);
        let (stop, _) = inner
            .wake
            .wait_timeout_while(stop, wait, |s| !*s)
            .unwrap_or_else(|e| e.into_inner());
        if *stop {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        root: PathBuf,
        state: Mutex<ServerState>,
        log: Mutex<Vec<String>>,
        start_fails: bool,
    }

    impl Fake {
        fn new(name: &str, state: ServerState) -> Self {
            Fake {
                root: PathBuf::from(name),
                state: Mutex::new(state),
                log: Mutex::new(Vec::new()),
                start_fails: false,
            }
        }
        fn set(&self, s: ServerState) {
            *lock(&self.state) = s;
        }
        fn log(&self) -> Vec<String> {
            std::mem::take(&mut *lock(&self.log))
        }
    }

    impl ScheduledServer for Fake {
        fn root(&self) -> &Path {
            &self.root
        }
        fn state(&self) -> ServerState {
            lock(&self.state).clone()
        }
        fn start(&self) -> anyhow::Result<()> {
            lock(&self.log).push("start".into());
            if self.start_fails {
                anyhow::bail!("port in use");
            }
            self.set(ServerState::Running);
            Ok(())
        }
        fn stop(&self) -> anyhow::Result<()> {
            lock(&self.log).push("stop".into());
            self.set(ServerState::Stopping);
            Ok(())
        }
        fn command(&self, line: &str) -> anyhow::Result<()> {
            lock(&self.log).push(line.into());
            Ok(())
        }
        fn backup(&self, _scope: crate::hosting::backup::Scope, keep: u32) -> anyhow::Result<String> {
            lock(&self.log).push(format!("backup keep {keep}"));
            Ok("b.zip".into())
        }
    }

    #[test]
    fn backs_up_every_few_hours_while_running() {
        let s = Fake::new("/srv/a", ServerState::Running);
        s.set(ServerState::Running);
        let sched = Schedule { backup_every_hours: 2, backup_keep: 5, ..Schedule::default() };
        let mut core = Core::default();
        let tick = |core: &mut Core, t: &str| core.tick(at(t), &[(&s as &dyn ScheduledServer, sched.clone())]);
        let e = tick(&mut core, "2026-10-23 10:00");
        assert!(matches!(&e[..], [ScheduleEvent::BackedUp { name, .. }] if name == "b.zip"), "{e:?}");
        assert!(tick(&mut core, "2026-10-23 11:00").is_empty());
        assert!(tick(&mut core, "2026-10-23 11:59").is_empty());
        assert_eq!(tick(&mut core, "2026-10-23 12:00").len(), 1);
        // Stopped: no backups (nothing changes).
        s.set(ServerState::Stopped);
        assert!(tick(&mut core, "2026-10-23 15:00").is_empty());
        assert_eq!(lock(&s.log).iter().filter(|l| l.starts_with("backup keep 5")).count(), 2);
    }

    fn at(s: &str) -> LocalTime {
        LocalTime::parse(s).unwrap()
    }

    fn restart_at(t: &str) -> Schedule {
        Schedule { daily_restart: Some(t.into()), ..Schedule::default() }
    }

    #[test]
    fn warnings_and_restart_across_a_day() {
        let s = Fake::new("pack", ServerState::Running);
        let sched = restart_at("04:00");
        let mut core = Core::default();
        let mut all = Vec::new();
        let mut log = Vec::new();
        // Every 20 s from 03:00 to 05:00; the server takes 40 s to stop.
        let mut now = at("2026-10-12T03:00");
        let mut stopping_since = None;
        while now < at("2026-10-12T05:00") {
            if s.state() == ServerState::Stopping {
                let since = *stopping_since.get_or_insert(now.0);
                if now.0 - since >= 40 {
                    s.set(ServerState::Stopped);
                }
            }
            all.extend(core.tick(now, &[(&s as &dyn ScheduledServer, sched.clone())]));
            log.extend(s.log());
            now = LocalTime(now.0 + 20);
        }
        let warned: Vec<u32> = all
            .iter()
            .filter_map(|e| match e {
                ScheduleEvent::Warned { minutes, .. } => Some(*minutes),
                _ => None,
            })
            .collect();
        assert_eq!(warned, [10, 5, 1]);
        assert_eq!(
            log,
            [
                "say Server restarts in 10 minutes",
                "say Server restarts in 5 minutes",
                "say Server restarts in 1 minute",
                "say Server is restarting now.",
                "stop",
                "start",
            ]
        );
        assert_eq!(
            all.last(),
            Some(&ScheduleEvent::Restarted { root: "pack".into(), why: "daily restart at 04:00".into() })
        );
        assert_eq!(s.state(), ServerState::Running);
    }

    #[test]
    fn late_start_warns_with_the_real_time_left() {
        let s = Fake::new("pack", ServerState::Running);
        let mut core = Core::default();
        let ev = core.tick(at("2026-10-12T03:57"), &[(&s as &dyn ScheduledServer, restart_at("04:00"))]);
        assert_eq!(ev, [ScheduleEvent::Warned { root: "pack".into(), minutes: 3 }]);
        // The 10- and 5-minute warnings count as sent; the 1-minute one comes.
        let ev = core.tick(at("2026-10-12T03:58"), &[(&s as &dyn ScheduledServer, restart_at("04:00"))]);
        assert!(ev.is_empty());
        let ev = core.tick(at("2026-10-12T03:59"), &[(&s as &dyn ScheduledServer, restart_at("04:00"))]);
        assert_eq!(ev, [ScheduleEvent::Warned { root: "pack".into(), minutes: 1 }]);
    }

    #[test]
    fn never_restarts_twice_a_day_and_only_when_running() {
        let s = Fake::new("pack", ServerState::Running);
        let sched = Schedule { warn_minutes: vec![], ..restart_at("04:00") };
        let mut core = Core::default();
        let mut tick = |t: &str| core.tick(at(t), &[(&s as &dyn ScheduledServer, sched.clone())]);
        tick("2026-10-12T04:00");
        assert_eq!(s.log(), ["say Server is restarting now.", "stop"]);
        s.set(ServerState::Stopped);
        tick("2026-10-12T04:01");
        assert_eq!(s.log(), ["start"]);
        // Still inside the grace window: no second restart.
        tick("2026-10-12T04:02");
        tick("2026-10-12T04:04");
        assert!(s.log().is_empty());
        // After the window: nothing either, even much later in the day.
        tick("2026-10-12T12:00");
        assert!(s.log().is_empty());
        // Stopped at restart time: skipped, and not done later when started.
        s.set(ServerState::Stopped);
        tick("2026-10-13T04:00");
        assert!(s.log().is_empty());
        s.set(ServerState::Running);
        tick("2026-10-13T04:10");
        assert!(s.log().is_empty());
        // The next day restarts again.
        tick("2026-10-14T04:00");
        assert_eq!(s.log(), ["say Server is restarting now.", "stop"]);
    }

    #[test]
    fn warnings_before_a_restart_just_after_midnight() {
        let s = Fake::new("pack", ServerState::Running);
        let sched = restart_at("00:05");
        let mut core = Core::default();
        let ev = core.tick(at("2026-10-12T23:55"), &[(&s as &dyn ScheduledServer, sched.clone())]);
        assert_eq!(ev, [ScheduleEvent::Warned { root: "pack".into(), minutes: 10 }]);
        core.tick(at("2026-10-13T00:05"), &[(&s as &dyn ScheduledServer, sched.clone())]);
        assert!(s.log().contains(&"stop".to_string()));
        // A 23:58 restart still happens at 00:01 (in its grace window).
        let s2 = Fake::new("late", ServerState::Running);
        let sched = Schedule { warn_minutes: vec![], ..restart_at("23:58") };
        core.tick(at("2026-10-13T00:01"), &[(&s2 as &dyn ScheduledServer, sched)]);
        assert!(s2.log().contains(&"stop".to_string()));
    }

    #[test]
    fn crash_restarts_up_to_the_hourly_limit() {
        let s = Fake::new("pack", ServerState::Crashed(Some(1)));
        let sched = Schedule::default(); // 3 per hour
        let mut core = Core::default();
        let mut tick = |t: &str| core.tick(at(t), &[(&s as &dyn ScheduledServer, sched.clone())]);
        for (i, t) in ["10:00", "10:10", "10:20"].iter().enumerate() {
            s.set(ServerState::Crashed(Some(1)));
            let ev = tick(&format!("2026-10-12T{t}"));
            assert_eq!(ev, [ScheduleEvent::CrashRestarted { root: "pack".into(), attempt: i as u32 + 1 }]);
        }
        s.set(ServerState::Crashed(Some(1)));
        let ev = tick("2026-10-12T10:30");
        assert_eq!(ev, [ScheduleEvent::GaveUp { root: "pack".into(), crashes: 4 }]);
        // Given up: no more tries or events while it stays crashed, even
        // after the hour has passed.
        assert!(tick("2026-10-12T10:31").is_empty());
        assert!(tick("2026-10-12T12:00").is_empty());
        assert_eq!(s.log().iter().filter(|l| *l == "start").count(), 3);
        // Started by hand: it is watched again, and the hour has reset.
        s.set(ServerState::Running);
        assert!(tick("2026-10-12T12:01").is_empty());
        s.set(ServerState::Crashed(None));
        let ev = tick("2026-10-12T12:02");
        assert_eq!(ev, [ScheduleEvent::CrashRestarted { root: "pack".into(), attempt: 1 }]);
    }

    #[test]
    fn crash_window_slides_by_the_hour() {
        let s = Fake::new("pack", ServerState::Running);
        let sched = Schedule { max_crash_restarts: 2, ..Schedule::default() };
        let mut core = Core::default();
        let mut crash = |t: &str| {
            s.set(ServerState::Crashed(Some(1)));
            core.tick(at(t), &[(&s as &dyn ScheduledServer, sched.clone())])
        };
        assert!(matches!(crash("2026-10-12T10:00")[0], ScheduleEvent::CrashRestarted { attempt: 1, .. }));
        assert!(matches!(crash("2026-10-12T10:40")[0], ScheduleEvent::CrashRestarted { attempt: 2, .. }));
        // 10:00 has dropped out of the hour by 11:01.
        assert!(matches!(crash("2026-10-12T11:01")[0], ScheduleEvent::CrashRestarted { attempt: 2, .. }));
        assert!(matches!(crash("2026-10-12T11:05")[0], ScheduleEvent::GaveUp { crashes: 3, .. }));
    }

    #[test]
    fn crash_restart_off_and_failing_starts() {
        let s = Fake::new("pack", ServerState::Crashed(Some(1)));
        let off = Schedule { auto_restart_on_crash: false, ..Schedule::default() };
        let mut core = Core::default();
        assert!(core.tick(at("2026-10-12T10:00"), &[(&s as &dyn ScheduledServer, off)]).is_empty());
        let mut bad = Fake::new("bad", ServerState::Crashed(Some(1)));
        bad.start_fails = true;
        let ev = core.tick(at("2026-10-12T10:00"), &[(&bad as &dyn ScheduledServer, Schedule::default())]);
        assert!(matches!(&ev[0], ScheduleEvent::Failed { error, .. } if error.contains("port in use")));
    }

    #[test]
    fn next_restart_around_midnight() {
        let s = restart_at("04:00");
        assert_eq!(next_restart(&s, at("2026-10-12T03:59")), Some(at("2026-10-12T04:00")));
        assert_eq!(next_restart(&s, at("2026-10-12T04:00")), Some(at("2026-10-13T04:00")));
        assert_eq!(describe_next_restart(&s, at("2026-10-12T23:59")).unwrap(), "04:00 tomorrow");
        assert_eq!(describe_next_restart(&s, at("2026-10-13T00:00")).unwrap(), "04:00 today");
        let m = restart_at("0:00");
        assert_eq!(describe_next_restart(&m, at("2026-10-12T23:59")).unwrap(), "00:00 tomorrow");
        assert_eq!(next_restart(&m, at("2026-12-31T23:30")), Some(at("2027-01-01T00:00")));
        assert_eq!(next_restart(&Schedule::default(), at("2026-10-12T00:00")), None);
    }

    #[test]
    fn parses_times() {
        assert_eq!(parse_hhmm("04:00"), Some(240));
        assert_eq!(parse_hhmm(" 4:05 "), Some(245));
        assert_eq!(parse_hhmm("23:59"), Some(1439));
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("12:60"), None);
        assert_eq!(parse_hhmm("noon"), None);
        assert!(restart_at("25:00").validate().is_err());
        assert!(restart_at("04:00").validate().is_ok());
        assert!(Schedule::default().validate().is_ok());
        assert_eq!(at("2026-10-12T04:07").hhmm(), "04:07");
        // Sanity: the PC's offset is a whole number of minutes within a day.
        let off = LocalTime::now().0 - at(&crate::time::now_iso()).0;
        assert!(off.abs() < DAY);
    }

    #[test]
    fn schedule_file_round_trip_and_defaults() {
        let dir = std::env::temp_dir().join(format!("hopper-schedule-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(dir.join(SCHEDULE_FILE));
        // No file: the defaults.
        let d = Schedule::load(&dir);
        assert_eq!(d, Schedule::default());
        assert_eq!(d.warn_minutes, [10, 5, 1]);
        assert!(d.auto_restart_on_crash && !d.start_with_hopper);
        assert_eq!(d.max_crash_restarts, 3);
        assert_eq!(d.daily_restart, None);
        // A partial file fills in the rest.
        std::fs::write(dir.join(SCHEDULE_FILE), r#"{"daily_restart":"05:30"}"#).unwrap();
        let p = Schedule::load(&dir);
        assert_eq!(p.restart_minute(), Some(330));
        assert_eq!(p.warn_minutes, [10, 5, 1]);
        // Round trip.
        let s = Schedule {
            daily_restart: Some("03:15".into()),
            warn_minutes: vec![15, 2],
            auto_restart_on_crash: false,
            max_crash_restarts: 5,
            start_with_hopper: true,
            backup_every_hours: 6,
            ..Schedule::default()
        };
        s.save(&dir).unwrap();
        assert_eq!(Schedule::load(&dir), s);
        // Broken: defaults rather than an error.
        std::fs::write(dir.join(SCHEDULE_FILE), "{ nope").unwrap();
        assert_eq!(Schedule::load(&dir), Schedule::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn autostart_needs_the_flag_and_the_eula() {
        let dir = std::env::temp_dir().join(format!("hopper-autostart-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = Fake::new("x", ServerState::Stopped);
        s.root = dir.clone();
        let on = Schedule { start_with_hopper: true, ..Schedule::default() };
        assert!(autostart(&[(&s as &dyn ScheduledServer, Schedule::default())]).is_empty());
        let ev = autostart(&[(&s as &dyn ScheduledServer, on.clone())]);
        assert!(matches!(&ev[0], ScheduleEvent::Failed { error, .. } if error.contains("EULA")));
        super::super::write_eula(&dir, true).unwrap();
        let ev = autostart(&[(&s as &dyn ScheduledServer, on.clone())]);
        assert_eq!(ev, [ScheduleEvent::AutoStarted { root: dir.clone() }]);
        // Already running: left alone.
        assert!(autostart(&[(&s as &dyn ScheduledServer, on)]).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scheduler_thread_stops_on_drop() {
        let panel = Arc::new(LocalPanel::new());
        let sched = Scheduler::start(panel.clone());
        assert!(sched.events().is_empty());
        let started = std::time::Instant::now();
        drop(sched);
        assert!(started.elapsed() < Duration::from_secs(5));
        // The thread let go of the panel.
        assert_eq!(Arc::strong_count(&panel), 1);
    }
}
