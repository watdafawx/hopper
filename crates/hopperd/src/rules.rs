//! Automations: "when server X does Y, do Z".
//!
//! A rule has a trigger (one of the panel's events, low TPS or a player
//! count that lasts, or a time), optional conditions ("only if"), steps
//! (console command, backup, start / stop / restart, call a URL, wait), a
//! cooldown and its last runs. Rules live in `rules.json` in the data
//! folder; only the owner sees or changes them, since their steps can stop
//! servers and type commands.
//!
//! Events reach the rules from [`crate::webhooks::emit`]; everything else is
//! checked every [`TICK`] by [`start`]'s thread.
//!
//! Left out on purpose: running programs on the PC. The panel is reachable
//! from phones and other PCs, so a rule that starts any program would make
//! one stolen owner sign-in into full control of the PC. "Call a URL"
//! covers it: point it at a local service that runs the script.
//! Crash restarts, the daily restart with warnings, scheduled backups and
//! auto-RCON stay where they are (each server's Schedule, Alerts).

use crate::App;
use crate::api::{ApiError, ApiResult, bad, not_found, require_owner};
use crate::webhooks::Event;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use hopper_core::hosting::schedule::{LocalTime, parse_hhmm};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const FILE: &str = "rules.json";
/// Runs kept per rule (they also drive the cooldown).
const HISTORY: usize = 20;
const TICK: Duration = Duration::from_secs(15);
/// How late a daily rule may still run (the panel was busy or asleep).
const DAILY_GRACE_MIN: u32 = 5;
/// Rules that start, stop, restart or back up wait at least this long
/// between runs, so a rule can't set itself off in a loop (restart → start → restart).
const MIN_POWER_COOLDOWN: u32 = 5;
const MAX_STEPS: usize = 10;
const MAX_WAIT: u32 = 3600;

/// Panel events a rule can start from.
pub const EVENTS: &[(&str, &str)] = &[
    ("start", "The server started"),
    ("stop", "The server stopped"),
    ("crash", "The server crashed"),
    ("join", "A player joined"),
    ("leave", "A player left"),
    ("backup", "A backup was made"),
    ("wake", "It was woken up by a player joining"),
    ("sleep", "It was put to sleep (nobody on)"),
    ("problem", "Something the panel tried failed"),
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Trigger {
    /// One of [`EVENTS`].
    Event { event: String },
    /// TPS below `below` for `minutes`.
    Tps { below: f32, minutes: u32 },
    /// Players online at least (`at_most` false) or at most `count`, for `minutes`.
    Players {
        #[serde(default)]
        at_most: bool,
        count: u32,
        minutes: u32,
    },
    /// Every `minutes`.
    Every { minutes: u32 },
    /// Every day at "HH:MM" local time (which days: [`Only::days`]).
    Daily { at: String },
}

/// Conditions that must all hold for a rule to run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Only {
    /// The server is up (running, past "Done").
    pub running: bool,
    pub players_min: Option<u32>,
    pub players_max: Option<u32>,
    /// A time window, "HH:MM" to "HH:MM" local; it may cross midnight.
    pub from: Option<String>,
    pub to: Option<String>,
    /// 0 Monday … 6 Sunday; empty is every day.
    pub days: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Step {
    /// A console command; `{server}` and `{player}` are filled in.
    Command { line: String },
    Backup {
        #[serde(default)]
        worlds_only: bool,
    },
    /// "start", "stop" or "restart".
    Power { action: String },
    /// POSTs JSON about what happened.
    Url { url: String },
    Wait { seconds: u32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub at: u64,
    pub server_id: String,
    pub server: String,
    pub ok: bool,
    pub text: String,
    /// Started with "Run now" (doesn't count for the cooldown).
    #[serde(default)]
    pub manual: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Server ids; empty means all.
    #[serde(default)]
    pub servers: Vec<String>,
    pub trigger: Trigger,
    #[serde(default)]
    pub only: Only,
    pub steps: Vec<Step>,
    /// Minutes between runs on the same server. 0: none.
    #[serde(default)]
    pub cooldown_minutes: u32,
    /// Newest first.
    #[serde(default)]
    pub history: Vec<Run>,
}

fn yes() -> bool {
    true
}

// ---- matching (no clock, no files: tested below) ------------------------------------

/// What a rule looks at on one server.
#[derive(Clone, Copy, Debug)]
pub struct Ctx {
    pub running: bool,
    pub players: u32,
    pub tps: Option<f32>,
    pub local: LocalTime,
}

impl Rule {
    fn covers(&self, server_id: &str) -> bool {
        self.enabled && (self.servers.is_empty() || self.servers.iter().any(|s| s == server_id))
    }

    fn wants_event(&self, e: &Event) -> bool {
        matches!(&self.trigger, Trigger::Event { event } if event == e.event) && self.covers(&e.server_id)
    }

    /// No automatic run on this server within the cooldown.
    fn cooled(&self, server_id: &str, now: u64) -> bool {
        let gap = u64::from(self.cooldown_minutes) * 60;
        gap == 0 || !self.history.iter().any(|r| !r.manual && r.server_id == server_id && now.saturating_sub(r.at) < gap)
    }
}

/// Monday 0 … Sunday 6. (1970-01-01 was a Thursday.)
fn weekday(t: LocalTime) -> u8 {
    (t.day() + 3).rem_euclid(7) as u8
}

/// `minute` (after midnight) is within from..to; a window may cross midnight.
fn in_window(from: u32, to: u32, minute: u32) -> bool {
    if from <= to { (from..to).contains(&minute) } else { minute >= from || minute < to }
}

impl Only {
    pub fn holds(&self, c: &Ctx) -> bool {
        if self.running && !c.running {
            return false;
        }
        if self.players_min.is_some_and(|m| c.players < m) || self.players_max.is_some_and(|m| c.players > m) {
            return false;
        }
        if !self.days.is_empty() && !self.days.contains(&weekday(c.local)) {
            return false;
        }
        match (self.from.as_deref().and_then(parse_hhmm), self.to.as_deref().and_then(parse_hhmm)) {
            (Some(f), Some(t)) => in_window(f, t, c.local.minute_of_day()),
            _ => true,
        }
    }
}

/// What the timed and lasting triggers remember between ticks, per
/// (rule, server).
#[derive(Default)]
struct Clock {
    /// When a lasting condition (TPS, players) started holding.
    since: HashMap<(String, String), u64>,
    /// Fired for the current stretch; again only once it clears.
    fired: HashSet<(String, String)>,
    /// "Every N minutes": the last slot (fired or skipped).
    last_every: HashMap<(String, String), u64>,
    /// "Daily": the last local day it ran.
    last_day: HashMap<(String, String), i64>,
}

impl Clock {
    /// True once, when `cond` has held for `minutes`; again only after it
    /// stops holding.
    fn lasted(&mut self, key: &(String, String), cond: bool, minutes: u32, now: u64) -> bool {
        if !cond {
            self.since.remove(key);
            self.fired.remove(key);
            return false;
        }
        let since = *self.since.entry(key.clone()).or_insert(now);
        if self.fired.contains(key) || now.saturating_sub(since) < u64::from(minutes) * 60 {
            return false;
        }
        self.fired.insert(key.clone());
        true
    }

    /// Whether a non-event trigger is due now. Conditions are checked
    /// after (a skipped slot isn't made up later).
    fn due(&mut self, rule: &Rule, server_id: &str, c: &Ctx, now: u64) -> bool {
        let key = (rule.id.clone(), server_id.to_string());
        match &rule.trigger {
            Trigger::Event { .. } => false,
            Trigger::Tps { below, minutes } => {
                let low = c.running && c.tps.is_some_and(|t| t < *below);
                self.lasted(&key, low, *minutes, now)
            }
            Trigger::Players { at_most, count, minutes } => {
                let hit = c.running && if *at_most { c.players <= *count } else { c.players >= *count };
                self.lasted(&key, hit, *minutes, now)
            }
            Trigger::Every { minutes } => {
                // The first look starts the count (the panel just started).
                let last = *self.last_every.entry(key.clone()).or_insert(now);
                if now.saturating_sub(last) < u64::from((*minutes).max(1)) * 60 {
                    return false;
                }
                self.last_every.insert(key, now);
                true
            }
            Trigger::Daily { at } => {
                let Some(m) = parse_hhmm(at) else { return false };
                let today = c.local.day();
                let mod_ = c.local.minute_of_day();
                if mod_ < m || mod_ >= m + DAILY_GRACE_MIN || self.last_day.get(&key) == Some(&today) {
                    return false;
                }
                self.last_day.insert(key, today);
                true
            }
        }
    }

    /// Forgets rules and servers that are gone.
    fn keep(&mut self, live: &HashSet<(String, String)>) {
        self.since.retain(|k, _| live.contains(k));
        self.fired.retain(|k| live.contains(k));
        self.last_every.retain(|k, _| live.contains(k));
        self.last_day.retain(|k, _| live.contains(k));
    }
}

/// The player in a join or leave event ("Steve joined **ATM10**").
fn player_of(e: &Event) -> Option<String> {
    let sep = match e.event {
        "join" => " joined ",
        "leave" => " left ",
        _ => return None,
    };
    e.text.split_once(sep).map(|(p, _)| p.to_string())
}

fn fill(line: &str, server: &str, player: Option<&str>) -> String {
    line.replace("{server}", server).replace("{player}", player.unwrap_or(""))
}

/// Why a rule can't be saved, in words for the form.
fn check(r: &Rule) -> Result<(), String> {
    if r.name.trim().is_empty() {
        return Err("Give the automation a name.".into());
    }
    match &r.trigger {
        Trigger::Event { event } if !EVENTS.iter().any(|(k, _)| k == event) => return Err(format!("Unknown event {event}.")),
        Trigger::Tps { below, minutes } if !(1.0..=20.0).contains(below) || !(1..=120).contains(minutes) => {
            return Err("TPS goes from 1 to 20, for 1 to 120 minutes.".into());
        }
        Trigger::Players { count, minutes, .. } if *count > 1000 || !(1..=1440).contains(minutes) => {
            return Err("Pick a player count and 1 to 1440 minutes.".into());
        }
        Trigger::Every { minutes } if !(1..=10080).contains(minutes) => return Err("Every 1 minute to 7 days.".into()),
        Trigger::Daily { at } if parse_hhmm(at).is_none() => return Err("Type the time as HH:MM, like 04:00.".into()),
        _ => {}
    }
    let o = &r.only;
    if o.from.is_some() != o.to.is_some() || [&o.from, &o.to].iter().any(|t| t.as_deref().is_some_and(|t| parse_hhmm(t).is_none())) {
        return Err("A time window needs both times as HH:MM.".into());
    }
    if o.days.iter().any(|d| *d > 6) {
        return Err("Unknown day.".into());
    }
    if let (Some(a), Some(b)) = (o.players_min, o.players_max) {
        if a > b {
            return Err("The fewest players is more than the most.".into());
        }
    }
    if r.steps.is_empty() {
        return Err("Add at least one step.".into());
    }
    if r.steps.len() > MAX_STEPS {
        return Err(format!("At most {MAX_STEPS} steps."));
    }
    for s in &r.steps {
        match s {
            Step::Command { line } if line.trim().trim_start_matches('/').is_empty() => return Err("A command step is empty.".into()),
            Step::Power { action } if !matches!(action.as_str(), "start" | "stop" | "restart") => {
                return Err("A server step starts, stops or restarts it.".into());
            }
            Step::Url { url } if !(url.starts_with("https://") || url.starts_with("http://")) => {
                return Err("The URL has to start with https:// (or http://).".into());
            }
            Step::Wait { seconds } if !(1..=MAX_WAIT).contains(seconds) => return Err("Wait 1 second to 1 hour.".into()),
            _ => {}
        }
    }
    let heavy = r.steps.iter().any(|s| matches!(s, Step::Power { .. } | Step::Backup { .. }));
    if heavy && r.cooldown_minutes < MIN_POWER_COOLDOWN {
        return Err(format!(
            "Automations that start, stop, restart or back up wait at least {MIN_POWER_COOLDOWN} minutes between runs."
        ));
    }
    Ok(())
}

fn summarize(n: usize) -> String {
    if n == 1 { "1 step done".into() } else { format!("{n} steps done") }
}

// ---- storage ----------------------------------------------------------------------------

fn lock() -> &'static Mutex<Option<Vec<Rule>>> {
    static R: OnceLock<Mutex<Option<Vec<Rule>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

fn with<T>(app: &App, f: impl FnOnce(&mut Vec<Rule>) -> T) -> (T, bool) {
    let mut g = lock().lock().unwrap_or_else(|e| e.into_inner());
    let rules = g.get_or_insert_with(|| {
        std::fs::read(app.data_dir.join(FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    });
    let before = rules.clone();
    let out = f(rules);
    let changed = *rules != before;
    if changed {
        let path = app.data_dir.join(FILE);
        let tmp = path.with_extension("json.tmp");
        let r = serde_json::to_vec_pretty(&*rules).map_err(anyhow::Error::from).and_then(|b| {
            std::fs::write(&tmp, b)?;
            std::fs::rename(&tmp, &path)?;
            Ok(())
        });
        if let Err(e) = r {
            log::warn!("couldn't save {FILE}: {e:#}");
        }
    }
    (out, changed)
}

fn load(app: &App) -> Vec<Rule> {
    with(app, |r| r.clone()).0
}

fn record(app: &App, id: &str, run: Run) {
    with(app, |rules| {
        if let Some(r) = rules.iter_mut().find(|r| r.id == id) {
            r.history.insert(0, run);
            r.history.truncate(HISTORY);
        }
    });
}

// ---- running ----------------------------------------------------------------------------

/// (rule, server) pairs running now: a rule waiting out a step doesn't start again.
fn busy() -> &'static Mutex<HashSet<(String, String)>> {
    static B: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();
    B.get_or_init(Default::default)
}

fn clock() -> &'static Mutex<Clock> {
    static C: OnceLock<Mutex<Clock>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn ctx_of(s: &crate::servers::Summary, local: LocalTime) -> Ctx {
    Ctx { running: s.state == "running", players: s.players.len() as u32, tps: crate::alerts::tps_now(&s.id), local }
}

/// Runs the rules that start from these events.
pub fn on_events(app: &Arc<App>, events: &[Event]) {
    let rules = load(app);
    if !rules.iter().any(|r| r.enabled && matches!(r.trigger, Trigger::Event { .. })) {
        return;
    }
    let snap = app.snapshot.borrow().clone();
    let local = LocalTime::now();
    let now = crate::webhooks::now();
    for e in events {
        let Some(s) = snap.servers.iter().find(|s| s.id == e.server_id) else { continue };
        let c = ctx_of(s, local);
        for r in rules.iter().filter(|r| r.wants_event(e) && r.only.holds(&c) && r.cooled(&e.server_id, now)) {
            launch(app, r.clone(), e.clone(), false);
        }
    }
}

/// Looks at the lasting and timed triggers.
fn tick(app: &Arc<App>) {
    let rules = load(app);
    let snap = app.snapshot.borrow().clone();
    let local = LocalTime::now();
    let now = crate::webhooks::now();
    let mut live = HashSet::new();
    let mut due = Vec::new();
    {
        let mut ck = clock().lock().unwrap_or_else(|e| e.into_inner());
        for r in rules.iter().filter(|r| !matches!(r.trigger, Trigger::Event { .. })) {
            for s in snap.servers.iter().filter(|s| r.covers(&s.id)) {
                live.insert((r.id.clone(), s.id.clone()));
                let c = ctx_of(s, local);
                if ck.due(r, &s.id, &c, now) && r.only.holds(&c) && r.cooled(&s.id, now) {
                    due.push((r.clone(), s.id.clone(), s.name.clone(), trigger_text(r, &s.name)));
                }
            }
        }
        ck.keep(&live);
    }
    for (r, id, name, text) in due {
        let e = Event { event: "rule", server_id: id, server: name, text, at: now };
        launch(app, r, e, false);
    }
}

fn trigger_text(r: &Rule, server: &str) -> String {
    match &r.trigger {
        Trigger::Event { event } => format!("{server}: {event}"),
        Trigger::Tps { below, minutes } => format!("{server} ran below {below} TPS for {minutes} min"),
        Trigger::Players { at_most, count, minutes } => {
            format!("{server} had {} {count} players for {minutes} min", if *at_most { "at most" } else { "at least" })
        }
        Trigger::Every { minutes } => format!("{server}: every {minutes} min"),
        Trigger::Daily { at } => format!("{server}: daily at {at}"),
    }
}

/// Runs a rule's steps for one server on its own thread.
fn launch(app: &Arc<App>, rule: Rule, e: Event, manual: bool) {
    let key = (rule.id.clone(), e.server_id.clone());
    if !busy().lock().unwrap().insert(key.clone()) {
        return;
    }
    let app = app.clone();
    std::thread::Builder::new()
        .name("automation".into())
        .spawn(move || {
            let run = run_steps(&app, &rule, &e, manual);
            log::info!("automation {} on {}: {}", rule.name, e.server, run.text);
            record(&app, &rule.id, run);
            busy().lock().unwrap().remove(&key);
        })
        .map_err(|err| log::warn!("automation thread: {err}"))
        .ok();
}

fn run_steps(app: &Arc<App>, rule: &Rule, e: &Event, manual: bool) -> Run {
    let started = crate::webhooks::now();
    let player = player_of(e);
    let mut run = Run { at: started, server_id: e.server_id.clone(), server: e.server.clone(), ok: true, text: String::new(), manual };
    for (i, s) in rule.steps.iter().enumerate() {
        if let Err(err) = step(app, rule, s, e, player.as_deref()) {
            run.ok = false;
            run.text = format!("Step {} ({}): {err:#}", i + 1, step_name(s));
            return run;
        }
    }
    run.text = summarize(rule.steps.len());
    run
}

fn step_name(s: &Step) -> &'static str {
    match s {
        Step::Command { .. } => "command",
        Step::Backup { .. } => "backup",
        Step::Power { action } if action == "start" => "start",
        Step::Power { action } if action == "stop" => "stop",
        Step::Power { .. } => "restart",
        Step::Url { .. } => "call a URL",
        Step::Wait { .. } => "wait",
    }
}

fn step(app: &Arc<App>, rule: &Rule, s: &Step, e: &Event, player: Option<&str>) -> anyhow::Result<()> {
    use hopper_core::hosting::backup;
    use hopper_core::panel::PanelAction;
    let entry = app.servers.get(&e.server_id).ok_or_else(|| anyhow::anyhow!("the server is gone"))?;
    match s {
        Step::Command { line } => app.servers.command(&entry, &fill(line, &entry.name, player)),
        Step::Backup { worlds_only } => {
            if crate::backups_api::is_restoring(&entry.id) {
                anyhow::bail!("it's being restored from a backup");
            }
            let scope = if *worlds_only { backup::Scope::Worlds } else { backup::Scope::Full };
            let job = app.jobs.start(format!("Backing up {}", entry.name));
            let r = backup::make(&entry.root, scope, backup::Kind::Scheduled, &format!("Automation: {}", rule.name), Some(&entry.server), &job);
            job.finish(&r);
            let b = r?;
            // Automation backups count as scheduled ones: the Schedule's "keep" applies.
            let _ = backup::prune(&entry.root, hopper_core::hosting::Schedule::load(&entry.root).backup_keep as usize);
            let text = format!("{} backed up ({})", entry.name, b.name);
            crate::webhooks::emit(app, vec![Event { event: "backup", server_id: entry.id.clone(), server: entry.name.clone(), text, at: crate::webhooks::now() }]);
            crate::offsite::kick();
            Ok(())
        }
        Step::Power { action } => {
            let a = match action.as_str() {
                "start" => PanelAction::Start,
                "stop" => PanelAction::Stop,
                _ => PanelAction::Restart,
            };
            if !matches!(a, PanelAction::Stop) && crate::backups_api::is_restoring(&entry.id) {
                anyhow::bail!("it's being restored from a backup");
            }
            log::info!("{action} {} by automation {}", entry.name, rule.name);
            app.servers.act(&entry, a)?;
            app.refresh.notify_one();
            Ok(())
        }
        Step::Url { url } => {
            let panel = format!("Hopper Server · {}", app.cfg.read().unwrap().name);
            let body = json!({ "automation": rule.name, "event": e.event, "server_id": e.server_id, "server": e.server,
                               "player": player, "text": e.text.replace("**", ""), "at": e.at, "panel": panel });
            let r = crate::webhooks::agent().post(url).send_json(body).map_err(|err| anyhow::anyhow!("{err}"))?;
            let status = r.status().as_u16();
            if !(200..300).contains(&status) {
                anyhow::bail!("the URL answered HTTP {status}");
            }
            Ok(())
        }
        Step::Wait { seconds } => {
            std::thread::sleep(Duration::from_secs(u64::from(*seconds).min(u64::from(MAX_WAIT))));
            // Turned off or removed meanwhile: stop here.
            if !load(app).iter().any(|r| r.id == rule.id && r.enabled) {
                anyhow::bail!("stopped: the automation was turned off");
            }
            Ok(())
        }
    }
}

/// Checks the timed and lasting triggers for as long as the panel runs.
pub fn start(app: Arc<App>) {
    std::thread::Builder::new()
        .name("automations".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(20));
            loop {
                tick(&app);
                std::thread::sleep(TICK);
            }
        })
        .map_err(|e| log::warn!("no automations: {e}"))
        .ok();
}

// ---- API ----------------------------------------------------------------------------------

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/rules", get(list).post(create))
        .route("/api/v1/rules/{id}", put(update).delete(remove))
        .route("/api/v1/rules/{id}/run", post(run_now))
}

#[derive(Deserialize)]
struct RuleBody {
    name: String,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    servers: Vec<String>,
    trigger: Trigger,
    #[serde(default)]
    only: Only,
    steps: Vec<Step>,
    #[serde(default)]
    cooldown_minutes: u32,
}

impl RuleBody {
    fn into_rule(self, id: String, history: Vec<Run>) -> Rule {
        let steps = self
            .steps
            .into_iter()
            .map(|s| match s {
                Step::Command { line } => Step::Command { line: line.trim().to_string() },
                Step::Url { url } => Step::Url { url: url.trim().to_string() },
                s => s,
            })
            .collect();
        let blank = |t: Option<String>| t.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        let only = Only { from: blank(self.only.from), to: blank(self.only.to), ..self.only };
        Rule { id, name: self.name.trim().to_string(), enabled: self.enabled, servers: self.servers, trigger: self.trigger, only, steps, cooldown_minutes: self.cooldown_minutes, history }
    }
}

async fn list(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let events: Vec<Value> = EVENTS.iter().map(|(k, d)| json!({ "id": k, "label": d })).collect();
    Ok(Json(json!({ "rules": load(&app), "events": events, "min_power_cooldown": MIN_POWER_COOLDOWN })))
}

async fn create(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<RuleBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let r = b.into_rule(crate::auth::random_hex(6), Vec::new());
    check(&r).map_err(bad)?;
    let id = r.id.clone();
    log::info!("automation {} added", r.name);
    with(&app, |all| all.push(r));
    Ok(Json(json!({ "id": id })))
}

async fn update(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<RuleBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let history = load(&app).into_iter().find(|r| r.id == id).ok_or_else(|| not_found("automation"))?.history;
    let r = b.into_rule(id.clone(), history);
    check(&r).map_err(bad)?;
    with(&app, |all| {
        if let Some(x) = all.iter_mut().find(|x| x.id == id) {
            *x = r;
        }
    });
    Ok(Json(json!({ "ok": true })))
}

async fn remove(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let (found, _) = with(&app, |all| {
        let n = all.len();
        all.retain(|r| r.id != id);
        all.len() != n
    });
    if !found {
        return Err(not_found("automation"));
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct RunBody {
    server: String,
}

/// Runs a rule's steps on one server now, whatever its trigger and
/// conditions say. Without waits it answers with the result.
async fn run_now(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<RunBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let rule = load(&app).into_iter().find(|r| r.id == id).ok_or_else(|| not_found("automation"))?;
    let entry = app.servers.get(&b.server).ok_or_else(|| not_found("server"))?;
    let e = Event { event: "rule", server_id: entry.id.clone(), server: entry.name.clone(), text: format!("Run by hand on {}", entry.name), at: crate::webhooks::now() };
    if rule.steps.iter().any(|s| matches!(s, Step::Wait { .. })) {
        launch(&app, rule, e, true);
        return Ok(Json(json!({ "started": true })));
    }
    let key = (rule.id.clone(), entry.id.clone());
    if !busy().lock().unwrap().insert(key.clone()) {
        return Err(ApiError(StatusCode::CONFLICT, "It's running on that server already.".into()));
    }
    let a = app.clone();
    let run = tokio::task::spawn_blocking(move || {
        let run = run_steps(&a, &rule, &e, true);
        log::info!("automation {} on {} (by hand): {}", rule.name, e.server, run.text);
        record(&a, &rule.id, run.clone());
        busy().lock().unwrap().remove(&key);
        run
    })
    .await
    .map_err(bad)?;
    Ok(Json(json!({ "run": run })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(trigger: Trigger) -> Rule {
        Rule {
            id: "r".into(),
            name: "R".into(),
            enabled: true,
            servers: Vec::new(),
            trigger,
            only: Only::default(),
            steps: vec![Step::Command { line: "say hi".into() }],
            cooldown_minutes: 0,
            history: Vec::new(),
        }
    }

    fn ev(event: &'static str, server: &str, text: &str) -> Event {
        Event { event, server_id: server.into(), server: server.to_uppercase(), text: text.into(), at: 0 }
    }

    /// 2026-10-12 is a Monday.
    fn at(s: &str) -> LocalTime {
        LocalTime::parse(s).unwrap()
    }

    fn ctx(running: bool, players: u32, tps: Option<f32>, local: &str) -> Ctx {
        Ctx { running, players, tps, local: at(local) }
    }

    #[test]
    fn matches_events_and_servers() {
        let mut r = rule(Trigger::Event { event: "crash".into() });
        assert!(r.wants_event(&ev("crash", "a", "")));
        assert!(!r.wants_event(&ev("stop", "a", "")));
        r.servers = vec!["b".into()];
        assert!(!r.wants_event(&ev("crash", "a", "")));
        assert!(r.wants_event(&ev("crash", "b", "")));
        r.enabled = false;
        assert!(!r.wants_event(&ev("crash", "b", "")));
        // Timed rules never match events.
        assert!(!rule(Trigger::Every { minutes: 5 }).wants_event(&ev("crash", "a", "")));
    }

    #[test]
    fn cooldown_is_per_server_and_skips_manual_runs() {
        let mut r = rule(Trigger::Event { event: "join".into() });
        r.cooldown_minutes = 10;
        let run = |server: &str, at: u64, manual| Run { at, server_id: server.into(), server: String::new(), ok: true, text: String::new(), manual };
        r.history = vec![run("a", 1000, false), run("b", 1500, true)];
        assert!(!r.cooled("a", 1000 + 599));
        assert!(r.cooled("a", 1000 + 600));
        assert!(r.cooled("b", 1600), "a run by hand doesn't count");
        assert!(r.cooled("c", 1001));
        r.cooldown_minutes = 0;
        assert!(r.cooled("a", 1000));
    }

    #[test]
    fn conditions() {
        let mut o = Only { running: true, players_min: Some(1), players_max: Some(3), ..Default::default() };
        assert!(o.holds(&ctx(true, 2, None, "2026-10-12T12:00")));
        assert!(!o.holds(&ctx(false, 2, None, "2026-10-12T12:00")));
        assert!(!o.holds(&ctx(true, 0, None, "2026-10-12T12:00")));
        assert!(!o.holds(&ctx(true, 4, None, "2026-10-12T12:00")));
        // Weekdays only: Monday yes, Sunday no.
        o.days = vec![0, 1, 2, 3, 4];
        assert!(o.holds(&ctx(true, 1, None, "2026-10-12T12:00")));
        assert!(!o.holds(&ctx(true, 1, None, "2026-10-18T12:00")));
    }

    #[test]
    fn time_windows_cross_midnight() {
        let o = Only { from: Some("22:00".into()), to: Some("06:00".into()), ..Default::default() };
        assert!(o.holds(&ctx(false, 0, None, "2026-10-12T23:30")));
        assert!(o.holds(&ctx(false, 0, None, "2026-10-12T05:59")));
        assert!(!o.holds(&ctx(false, 0, None, "2026-10-12T06:00")));
        assert!(!o.holds(&ctx(false, 0, None, "2026-10-12T12:00")));
        let day = Only { from: Some("09:00".into()), to: Some("17:00".into()), ..Default::default() };
        assert!(day.holds(&ctx(false, 0, None, "2026-10-12T09:00")));
        assert!(!day.holds(&ctx(false, 0, None, "2026-10-12T17:00")));
    }

    #[test]
    fn low_tps_must_last_and_fires_once_per_stretch() {
        let r = rule(Trigger::Tps { below: 15.0, minutes: 2 });
        let mut ck = Clock::default();
        let t = "2026-10-12T12:00";
        assert!(!ck.due(&r, "a", &ctx(true, 0, Some(10.0), t), 1000));
        assert!(!ck.due(&r, "a", &ctx(true, 0, Some(10.0), t), 1119));
        assert!(ck.due(&r, "a", &ctx(true, 0, Some(10.0), t), 1120));
        assert!(!ck.due(&r, "a", &ctx(true, 0, Some(9.0), t), 1500), "once per stretch");
        // Recovers, then drops again: a new stretch.
        assert!(!ck.due(&r, "a", &ctx(true, 0, Some(19.0), t), 1600));
        assert!(!ck.due(&r, "a", &ctx(true, 0, Some(10.0), t), 1700));
        assert!(ck.due(&r, "a", &ctx(true, 0, Some(10.0), t), 1820));
        // No reading or not running isn't low TPS.
        let mut ck = Clock::default();
        assert!(!ck.due(&r, "b", &ctx(true, 0, None, t), 0));
        assert!(!ck.due(&r, "b", &ctx(false, 0, Some(5.0), t), 1000));
    }

    #[test]
    fn player_counts_last() {
        let r = rule(Trigger::Players { at_most: true, count: 0, minutes: 30 });
        let mut ck = Clock::default();
        let t = "2026-10-12T12:00";
        assert!(!ck.due(&r, "a", &ctx(true, 0, None, t), 0));
        assert!(!ck.due(&r, "a", &ctx(true, 1, None, t), 1000), "someone joined: starts over");
        assert!(!ck.due(&r, "a", &ctx(true, 0, None, t), 1100));
        assert!(ck.due(&r, "a", &ctx(true, 0, None, t), 1100 + 1800));
        let mut busy = rule(Trigger::Players { at_most: false, count: 5, minutes: 1 });
        busy.id = "busy".into();
        assert!(!ck.due(&busy, "a", &ctx(true, 5, None, t), 0));
        assert!(ck.due(&busy, "a", &ctx(true, 6, None, t), 60));
    }

    #[test]
    fn every_counts_from_the_first_look() {
        let r = rule(Trigger::Every { minutes: 10 });
        let mut ck = Clock::default();
        let t = "2026-10-12T12:00";
        assert!(!ck.due(&r, "a", &ctx(true, 0, None, t), 1000), "not right as the panel starts");
        assert!(!ck.due(&r, "a", &ctx(true, 0, None, t), 1599));
        assert!(ck.due(&r, "a", &ctx(true, 0, None, t), 1600));
        assert!(!ck.due(&r, "a", &ctx(true, 0, None, t), 1615));
        assert!(ck.due(&r, "a", &ctx(true, 0, None, t), 2200));
        // Each server keeps its own count.
        assert!(!ck.due(&r, "b", &ctx(true, 0, None, t), 2200));
    }

    #[test]
    fn daily_runs_once_within_its_grace() {
        let r = rule(Trigger::Daily { at: "04:00".into() });
        let mut ck = Clock::default();
        assert!(!ck.due(&r, "a", &ctx(true, 0, None, "2026-10-12T03:59"), 0));
        assert!(ck.due(&r, "a", &ctx(true, 0, None, "2026-10-12T04:02"), 0));
        assert!(!ck.due(&r, "a", &ctx(true, 0, None, "2026-10-12T04:03"), 0), "once a day");
        assert!(!ck.due(&r, "a", &ctx(true, 0, None, "2026-10-13T04:05"), 0), "too late the next day");
        assert!(ck.due(&r, "a", &ctx(true, 0, None, "2026-10-14T04:00"), 0));
    }

    #[test]
    fn forgets_gone_rules() {
        let r = rule(Trigger::Every { minutes: 1 });
        let mut ck = Clock::default();
        ck.due(&r, "a", &ctx(true, 0, None, "2026-10-12T12:00"), 0);
        ck.keep(&HashSet::new());
        assert!(ck.last_every.is_empty());
    }

    #[test]
    fn fills_player_and_server() {
        let e = ev("join", "a", "Steve joined **A**");
        assert_eq!(player_of(&e).as_deref(), Some("Steve"));
        assert_eq!(player_of(&ev("leave", "a", "Alex left **A**")).as_deref(), Some("Alex"));
        assert_eq!(player_of(&ev("crash", "a", "**A** crashed")), None);
        assert_eq!(fill("tellraw {player} \"Welcome to {server}\"", "ATM10", Some("Steve")), "tellraw Steve \"Welcome to ATM10\"");
    }

    #[test]
    fn checks_rules() {
        let mut r = rule(Trigger::Event { event: "start".into() });
        assert!(check(&r).is_ok());
        r.trigger = Trigger::Event { event: "tps".into() };
        assert!(check(&r).is_err(), "low TPS is its own trigger");
        r.trigger = Trigger::Daily { at: "4am".into() };
        assert!(check(&r).is_err());
        r.trigger = Trigger::Daily { at: "4:00".into() };
        assert!(check(&r).is_ok());
        // A restart on start with no cooldown could loop.
        r.trigger = Trigger::Event { event: "start".into() };
        r.steps = vec![Step::Power { action: "restart".into() }];
        assert!(check(&r).unwrap_err().contains("at least 5 minutes"));
        r.cooldown_minutes = 5;
        assert!(check(&r).is_ok());
        r.steps = vec![Step::Power { action: "kill".into() }];
        assert!(check(&r).is_err());
        r.steps = vec![Step::Url { url: "ftp://x".into() }];
        assert!(check(&r).is_err());
        r.steps = vec![Step::Wait { seconds: 0 }];
        assert!(check(&r).is_err());
        r.steps = Vec::new();
        assert!(check(&r).is_err());
        r.steps = vec![Step::Command { line: "/".into() }];
        assert!(check(&r).is_err());
        r.steps = vec![Step::Command { line: "list".into() }];
        r.only.from = Some("22:00".into());
        assert!(check(&r).is_err(), "a window needs both ends");
        r.only.to = Some("06:00".into());
        assert!(check(&r).is_ok());
    }

    #[test]
    fn reads_the_saved_shape() {
        let j = r#"{"id":"x","name":"Lag","trigger":{"kind":"tps","below":12,"minutes":3},
            "only":{"players_max":3},"steps":[{"kind":"command","line":"say hi"},{"kind":"wait","seconds":60},{"kind":"power","action":"restart"}],
            "cooldown_minutes":30}"#;
        let r: Rule = serde_json::from_str(j).unwrap();
        assert!(r.enabled && r.servers.is_empty() && r.history.is_empty());
        assert_eq!(r.trigger, Trigger::Tps { below: 12.0, minutes: 3 });
        assert_eq!(r.steps.len(), 3);
        assert!(check(&r).is_ok());
    }
}
