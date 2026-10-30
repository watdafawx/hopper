//! How far a starting server has got, from its console, and how long it has
//! left, from how long its last start took.
//!
//! A server's log has no "mod 412 of 1132" counter (that is only on the
//! client's loading screen), but every loader goes through the same steps
//! and says when it reaches each: finding mods, setting up mixins,
//! constructing mods, loading data, starting the server, preparing the
//! world. The time each step was reached on the last start gives the
//! percentage and the time left; a first start falls back to typical
//! shares of a modded start.

use super::logparse::{LogEvent, message, parse_line};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Instant;

/// The steps of a start, in order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Launching,
    FindingMods,
    PreparingMods,
    LoadingMods,
    LoadingData,
    StartingServer,
    PreparingWorld,
    Done,
}

pub const PHASES: [Phase; 8] = [
    Phase::Launching,
    Phase::FindingMods,
    Phase::PreparingMods,
    Phase::LoadingMods,
    Phase::LoadingData,
    Phase::StartingServer,
    Phase::PreparingWorld,
    Phase::Done,
];

/// Where each step starts in a typical modded start, as a share of the
/// whole: used when there is no earlier start to go by.
const TYPICAL: [f32; 8] = [0.0, 0.03, 0.10, 0.30, 0.60, 0.85, 0.88, 1.0];

/// Where in its step a start is assumed to stop creeping forward, so the
/// bar never sits at the next step's mark before the log gets there.
const STEP_CAP: f32 = 0.95;

impl Phase {
    pub fn index(self) -> usize {
        self as usize
    }

    /// What the server is doing, for players.
    pub fn label(self) -> &'static str {
        match self {
            Phase::Launching => "Starting Java",
            Phase::FindingMods => "Finding mods",
            Phase::PreparingMods => "Preparing mods",
            Phase::LoadingMods => "Loading mods",
            Phase::LoadingData => "Loading data",
            Phase::StartingServer => "Starting the server",
            Phase::PreparingWorld => "Preparing the world",
            Phase::Done => "Ready",
        }
    }

    /// Steps only a modded server has.
    fn modded(self) -> bool {
        matches!(self, Phase::FindingMods | Phase::PreparingMods | Phase::LoadingMods)
    }
}

/// What the console said so far about the start.
#[derive(Clone, Debug, Default)]
pub struct Boot {
    /// When the process started.
    pub began: Option<Instant>,
    pub phase: Phase,
    /// When each step was reached.
    pub reached: [Option<Instant>; 8],
    /// "Loading 432 mods:" (Fabric, and Forge packs with Puzzles Lib).
    announced: Option<u32>,
    /// NeoForge's "Found mod file" lines, counted as they come.
    mod_files: u32,
    /// "Preparing spawn area: 51%".
    pub spawn: Option<u8>,
    /// The lines were read back after a restart of Hopper, all at once, so
    /// the times steps were reached aren't real.
    pub replayed: bool,
}

impl Boot {
    pub fn starting(at: Instant) -> Boot {
        let mut b = Boot { began: Some(at), ..Default::default() };
        b.reached[0] = Some(at);
        b
    }

    /// A start whose console is read again from the top after Hopper
    /// restarted: it began at `began`, but the steps' times aren't real.
    pub fn read_again(began: Instant) -> Boot {
        Boot { replayed: true, ..Boot::starting(began) }
    }

    /// Mods found so far (or announced).
    pub fn mods(&self) -> Option<u32> {
        self.announced.or((self.mod_files > 0).then_some(self.mod_files))
    }

    fn reach(&mut self, p: Phase, now: Instant) {
        if p > self.phase {
            self.phase = p;
            self.reached[p.index()] = Some(now);
        }
    }

    pub fn push(&mut self, line: &str, now: Instant) {
        if self.phase == Phase::Done {
            return;
        }
        if self.began.is_none() {
            *self = Boot { replayed: self.replayed, ..Boot::starting(now) };
        }
        if parse_line(line) == Some(LogEvent::Done) {
            self.reach(Phase::Done, now);
            return;
        }
        let msg = message(line).trim();
        let thread = thread(line);
        if let Some(pct) = msg.strip_prefix("Preparing spawn area: ").and_then(|r| r.trim_end_matches('%').trim().parse::<u8>().ok()) {
            self.reach(Phase::PreparingWorld, now);
            if self.phase == Phase::PreparingWorld {
                self.spawn = Some(pct.min(100).max(self.spawn.unwrap_or(0)));
            }
        } else if msg.starts_with("Preparing level") || msg.starts_with("Preparing start region") {
            self.reach(Phase::PreparingWorld, now);
        } else if msg.starts_with("Starting minecraft server version") {
            self.reach(Phase::StartingServer, now);
        } else if msg.starts_with("Reloading ResourceManager")
            || (msg.starts_with("Loaded ") && (msg.ends_with(" recipes") || msg.ends_with(" advancements")))
            // Data packs load on the Worker-Main threads; mods use them
            // too while being set up, so only after mixins are ready.
            || (thread.starts_with("Worker-Main") && self.phase >= Phase::PreparingMods)
        {
            self.reach(Phase::LoadingData, now);
        } else if thread.starts_with("modloading-worker") || msg.starts_with("Launching target '") {
            self.reach(Phase::LoadingMods, now);
        } else if msg.starts_with("Loading coremod") || msg.starts_with("Compatibility level set to") || msg.starts_with("SpongePowered MIXIN Subsystem") {
            self.reach(Phase::PreparingMods, now);
        } else if msg.starts_with("ModLauncher running")
            || (msg.starts_with("ModLauncher ") && msg.contains(" starting"))
            || (msg.starts_with("Loading Minecraft ") && (msg.contains("Fabric Loader") || msg.contains("Quilt Loader")))
        {
            self.reach(Phase::FindingMods, now);
        }
        if self.announced.is_none()
            && let Some(n) = msg.strip_prefix("Loading ").and_then(|r| r.strip_suffix(" mods:")).and_then(|n| n.trim().parse().ok())
        {
            self.announced = Some(n);
            self.reach(Phase::FindingMods, now);
        }
        if msg.starts_with("Found mod file ") {
            self.mod_files += 1;
            self.reach(Phase::FindingMods, now);
        }
    }

    fn secs(&self, at: Option<Instant>) -> Option<f32> {
        Some(at?.saturating_duration_since(self.began?).as_secs_f32())
    }

    /// How this start went, once it is done, to time the next one by.
    pub fn record(&self) -> Option<BootRecord> {
        if self.phase != Phase::Done || self.replayed {
            return None;
        }
        let secs = self.secs(self.reached[Phase::Done.index()])?;
        let marks = self.reached.iter().map(|r| self.secs(*r).map(round1)).collect();
        Some(BootRecord { secs: round1(secs), marks, mods: self.mods() })
    }

    /// Where the start is at `now`, timed by `last` (the last start) when
    /// there was one.
    pub fn status(&self, now: Instant, last: Option<&BootRecord>) -> BootStatus {
        let k = self.phase.index();
        let elapsed = self.began.map_or(0.0, |b| now.saturating_duration_since(b).as_secs_f32());
        let at_k = self.secs(self.reached[k]).unwrap_or(0.0);
        let spawn = (self.phase == Phase::PreparingWorld).then_some(self.spawn).flatten().map(|p| p as f32 / 100.0);
        let (share, left, slow) = match last.filter(|r| r.secs > 0.0) {
            Some(r) if self.phase != Phase::Done => {
                let total = r.secs;
                let marks = r.timeline();
                let (pk, next) = (marks[k], marks[k + 1]);
                // Faster or slower than last time so far: the rest likely is
                // too. A replayed console has no real times to compare.
                let pace = if self.replayed || pk < 3.0 || at_k < 1.0 { 1.0 } else { (at_k / pk).clamp(0.5, 2.0) };
                let seg = (next - pk).max(0.0);
                let within = match spawn {
                    Some(f) => seg * f,
                    None => (elapsed - at_k).max(0.0) / pace,
                };
                let capped = within >= seg * STEP_CAP;
                let pos = pk + within.min(seg * STEP_CAP);
                let slow = capped && elapsed > total * pace + 5.0;
                let left = (!slow).then(|| ((total - pos) * pace).max(1.0));
                (pos / total, left, slow)
            }
            _ => {
                let share = TYPICAL[k.min(7)] + spawn.unwrap_or(0.0) * (TYPICAL[(k + 1).min(7)] - TYPICAL[k.min(7)]);
                (share, None, false)
            }
        };
        let percent = if self.phase == Phase::Done { 100 } else { ((share * 100.0).round() as i32).clamp(1, 99) as u8 };
        BootStatus {
            phase: self.phase,
            label: self.phase.label(),
            percent,
            elapsed_secs: elapsed.round() as u32,
            left_secs: left.map(|l| l.round() as u32),
            usual_secs: last.map(|r| r.secs.round() as u32),
            slow,
            mods: self.mods(),
            spawn: self.spawn.filter(|_| self.phase == Phase::PreparingWorld),
            steps: self.steps(),
        }
    }

    /// The steps to list: those reached, the current one, and the ones
    /// still to come (the mod steps only on a modded server).
    fn steps(&self) -> Vec<Step> {
        let modded = PHASES.iter().any(|p| p.modded() && self.reached[p.index()].is_some()) || self.mods().is_some();
        PHASES[..7]
            .iter()
            .filter_map(|&p| {
                let state = if p == self.phase {
                    StepState::Now
                } else if self.reached[p.index()].is_some() {
                    StepState::Done
                } else if p > self.phase && (modded || !p.modded()) {
                    StepState::Next
                } else {
                    return None;
                };
                let label = match (p, self.mods()) {
                    (Phase::FindingMods, Some(n)) if state == StepState::Done => format!("Found {n} mods"),
                    (Phase::FindingMods, Some(n)) => format!("Finding mods ({n})"),
                    (Phase::PreparingWorld, _) if state == StepState::Now && self.spawn.is_some() => format!("Preparing the world {}%", self.spawn.unwrap_or(0)),
                    _ => p.label().to_string(),
                };
                Some(Step { label, state })
            })
            .collect()
    }
}

fn round1(x: f32) -> f32 {
    (x * 10.0).round() / 10.0
}

/// The thread in "[12:00:00] [Server thread/INFO] …": "Server thread".
fn thread(line: &str) -> &str {
    let Some(rest) = line.strip_prefix('[').and_then(|l| l.split_once("] [")).map(|(_, r)| r) else {
        return "";
    };
    let end = rest.find(['/', ']']).unwrap_or(rest.len());
    &rest[..end]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Done,
    Now,
    Next,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Step {
    pub label: String,
    pub state: StepState,
}

/// A start in progress, for the panel and the server list.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BootStatus {
    pub phase: Phase,
    /// What it is doing now ("Loading mods").
    pub label: &'static str,
    /// 1–99 while starting.
    pub percent: u8,
    pub elapsed_secs: u32,
    /// Time left, when there is an earlier start to go by and this one
    /// isn't past it.
    pub left_secs: Option<u32>,
    /// How long the last start took.
    pub usual_secs: Option<u32>,
    /// Taking longer than the last start did.
    pub slow: bool,
    pub mods: Option<u32>,
    pub spawn: Option<u8>,
    pub steps: Vec<Step>,
}

/// One finished start: seconds to "Done", and when each step was reached.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BootRecord {
    pub secs: f32,
    pub marks: Vec<Option<f32>>,
    #[serde(default)]
    pub mods: Option<u32>,
}

impl BootRecord {
    /// When each step started, every step filled in and kept in order.
    fn timeline(&self) -> [f32; 8] {
        let mark = |i: usize| self.marks.get(i).copied().flatten();
        let mut out = [0.0; 8];
        out[7] = self.secs;
        if (0..7).all(|i| mark(i).is_none()) {
            // Only the total is known: typical shares of it.
            for i in 0..7 {
                out[i] = TYPICAL[i] * self.secs;
            }
        } else {
            // A skipped step (Fabric never says it constructs mods, vanilla
            // has no mod steps) takes no time: it starts where the next
            // reached one does.
            for i in (0..7).rev() {
                out[i] = mark(i).unwrap_or(out[i + 1]);
            }
        }
        for i in 1..8 {
            out[i] = out[i].max(out[i - 1]).min(self.secs);
        }
        out
    }
}

/// The last few starts of a server, in `.hopper-boot.json` in its folder.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BootHistory {
    pub starts: Vec<BootRecord>,
}

pub const BOOT_FILE: &str = ".hopper-boot.json";
const KEEP: usize = 5;

impl BootHistory {
    pub fn load(root: &Path) -> BootHistory {
        std::fs::read_to_string(root.join(BOOT_FILE)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn last(&self) -> Option<&BootRecord> {
        self.starts.last()
    }

    pub fn add(&mut self, r: BootRecord) {
        self.starts.push(r);
        let extra = self.starts.len().saturating_sub(KEEP);
        self.starts.drain(..extra);
    }

    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        std::fs::write(root.join(BOOT_FILE), serde_json::to_string(self).unwrap_or_default())
    }
}

/// "45s", "1m 20s", "3m": a time rounded to what's worth saying (the
/// nearest 5 s under a minute, 10 s under 10 minutes, minute after; never
/// "0s").
pub fn short_time(secs: u32) -> String {
    let step = if secs < 60 {
        5
    } else if secs < 600 {
        10
    } else {
        60
    };
    let s = ((secs + step / 2) / step * step).max(5);
    match (s / 60, s % 60) {
        (0, s) => format!("{s}s"),
        (m, 0) => format!("{m}m"),
        (m, s) => format!("{m}m {s:02}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Feeds `lines` as (seconds after the start, line).
    fn run(lines: &[(f32, &str)]) -> (Boot, Instant) {
        let t0 = Instant::now();
        let mut b = Boot::starting(t0);
        for (s, l) in lines {
            b.push(l, t0 + Duration::from_secs_f32(*s));
        }
        (b, t0)
    }

    fn at(t0: Instant, s: f32) -> Instant {
        t0 + Duration::from_secs_f32(s)
    }

    const NEOFORGE: &[(f32, &str)] = &[
        (0.5, "[Hopper] Starting: java.exe -Xmx16G @libraries/net/neoforged/neoforge/21.1.259/win_args.txt nogui"),
        (1.0, "[23:31:48.181] [main/INFO] [Launcher/MODLAUNCHER]: ModLauncher running: args [--launchTarget, forgeserver, --fml.neoForgeVersion, 21.1.259]"),
        (36.0, "[23:32:24.026] [main/INFO] [loading.moddiscovery.ModDiscoverer/SCAN]: Found mod file \"asynclogger-2.2.3+1.21.1-neoforge-mod.jar\" [parent: , locator: jar]"),
        (36.1, "[23:32:24.027] [main/INFO] [loading.moddiscovery.ModDiscoverer/SCAN]: Found library file \"MixinSquared-0.3.7.jar\" [parent: mixinsquared-forge-0.3.7.jar]"),
        (36.2, "[23:32:24.028] [main/INFO] [loading.moddiscovery.ModDiscoverer/SCAN]: Found mod file \"create-1.21.1-6.0.6.jar\" [parent: , locator: jar]"),
        (42.0, "[23:32:30.264] [main/INFO] [loading.FMLServiceProvider/CORE]: Loading coremod script-engine for [apothic_enchanting, xaerominimap]"),
        (272.0, "[23:36:20.032] [main/INFO] [mixin/]: Compatibility level set to JAVA_21"),
        (274.0, "[23:36:22.492] [main/INFO] [LaunchServiceHandler/MODLAUNCHER]: Launching target 'forgeserver' with arguments [nogui]"),
        (275.0, "[23:36:22.690] [main/INFO] [C2ME Modules Manager/]: Initializing com.ishland.c2me.base.mixin"),
        (305.0, "[23:36:53] [modloading-worker-0/INFO] [uk.co.ex.es.EslMod/]: [ESL] Lobby manager initialized"),
        (380.0, "[00:43:24] [Worker-Main-5/ERROR] [minecraft/LootDataType]: Couldn't parse element ResourceKey[minecraft:root / minecraft:loot_table]"),
        (470.0, "[00:45:03] [main/INFO] [minecraft/RecipeManager]: Loaded 57019 recipes"),
        (485.0, "[00:45:18] [Server thread/INFO] [minecraft/DedicatedServer]: Starting minecraft server version 1.21.1"),
        (488.0, "[00:45:21] [Server thread/INFO] [minecraft/DedicatedServer]: Preparing level \"world\""),
        (490.0, "[00:45:23] [Worker-Main-20/INFO] [minecraft/LoggerChunkProgressListener]: Preparing spawn area: 0%"),
        (491.0, "[00:45:24] [Worker-Main-15/INFO] [minecraft/LoggerChunkProgressListener]: Preparing spawn area: 51%"),
        (492.0, "[00:45:24] [Server thread/INFO] [minecraft/DedicatedServer]: Done (5.558s)! For help, type \"help\""),
    ];

    #[test]
    fn neoforge_steps() {
        let (b, t0) = run(&NEOFORGE[..2]);
        assert_eq!(b.phase, Phase::FindingMods);
        let (b, _) = run(&NEOFORGE[..5]);
        assert_eq!(b.mods(), Some(2), "mod files counted, libraries not");
        let (b, _) = run(&NEOFORGE[..7]);
        assert_eq!(b.phase, Phase::PreparingMods);
        let (b, _) = run(&NEOFORGE[..9]);
        assert_eq!(b.phase, Phase::LoadingMods);
        let (b, _) = run(&NEOFORGE[..11]);
        assert_eq!(b.phase, Phase::LoadingData);
        let (b, _) = run(&NEOFORGE[..13]);
        assert_eq!(b.phase, Phase::StartingServer);
        let (b, _) = run(&NEOFORGE[..16]);
        assert_eq!((b.phase, b.spawn), (Phase::PreparingWorld, Some(51)));
        let (b, _) = run(NEOFORGE);
        assert_eq!(b.phase, Phase::Done);
        let r = b.record().unwrap();
        assert_eq!(r.secs, 492.0);
        assert_eq!(r.marks[Phase::LoadingData.index()], Some(380.0));
        assert_eq!(b.status(at(t0, 500.0), None).percent, 100);
    }

    #[test]
    fn forge_1_20_steps() {
        let (b, _) = run(&[
            (1.0, "[00:38:55] [main/INFO] [cp.mo.mo.Launcher/MODLAUNCHER]: ModLauncher 10.0.9+10.0.9+main.dcd20f30 starting: java version 17.0.20 by Eclipse Adoptium"),
            (30.0, "[00:39:27] [main/INFO] [mixin/]: Compatibility level set to JAVA_17"),
            (32.0, "[00:39:29] [main/INFO] [cp.mo.mo.LaunchServiceHandler/MODLAUNCHER]: Launching target 'forgeserver' with arguments [nogui]"),
            (34.0, "[00:39:31] [main/INFO] [Puzzles Lib/]: Loading 512 mods:"),
        ]);
        assert_eq!((b.phase, b.mods()), (Phase::LoadingMods, Some(512)), "a mod count late on doesn't go back a step");
        assert!(b.reached[Phase::FindingMods.index()].is_some());
    }

    #[test]
    fn fabric_steps() {
        let (b, _) = run(&[
            (1.0, "[00:36:53] [main/INFO]: Loading Minecraft 1.20.1 with Fabric Loader 0.18.1"),
            (2.0, "[00:36:54] [main/INFO]: Loading 432 mods:"),
            (2.1, "\t- ysns 2.0.2"),
            (12.0, "[00:37:05] [main/INFO]: SpongePowered MIXIN Subsystem Version=0.8.7 Source=file:/G:/x"),
            (59.0, "[00:37:52] [Worker-Main-12/INFO]: [Load My F***ing Tags]: Loaded Config File!"),
        ]);
        assert_eq!((b.phase, b.mods()), (Phase::LoadingData, Some(432)));
        let s = b.status(Instant::now(), None);
        // Fabric never says it constructs mods: the step is left out.
        let labels: Vec<_> = s.steps.iter().map(|s| (s.label.as_str(), s.state)).collect();
        assert_eq!(
            labels,
            [
                ("Starting Java", StepState::Done),
                ("Found 432 mods", StepState::Done),
                ("Preparing mods", StepState::Done),
                ("Loading data", StepState::Now),
                ("Starting the server", StepState::Next),
                ("Preparing the world", StepState::Next),
            ]
        );
    }

    #[test]
    fn paper_and_vanilla_have_no_mod_steps() {
        let (b, t0) = run(&[
            (1.0, "[10:00:01 INFO]: Loaded 1290 recipes"),
            (2.0, "[10:00:02 INFO]: Starting minecraft server version 1.21.1"),
            (4.0, "[10:00:04 INFO]: Preparing level \"world\""),
            (5.0, "[10:00:05 INFO]: Preparing spawn area: 40%"),
        ]);
        assert_eq!(b.phase, Phase::PreparingWorld);
        let s = b.status(at(t0, 5.0), None);
        assert!(s.steps.iter().all(|s| !s.label.contains("mods")), "{:?}", s.steps);
        assert_eq!(s.steps.last().unwrap().label, "Preparing the world 40%");
        // 88% + 40% of the last 12%.
        assert_eq!(s.percent, 93);
        assert_eq!(s.left_secs, None, "no earlier start: no time left");
    }

    #[test]
    fn spawn_percent_never_goes_back() {
        let (b, _) = run(&[(1.0, "[x] [Worker-Main-1/INFO]: Preparing spawn area: 62%"), (1.1, "[x] [Worker-Main-2/INFO]: Preparing spawn area: 22%")]);
        assert_eq!(b.spawn, Some(62));
    }

    fn last() -> BootRecord {
        let (b, _) = run(NEOFORGE);
        b.record().unwrap()
    }

    #[test]
    fn time_left_from_the_last_start() {
        let r = last();
        // Same pace as last time: 100 s into loading data (reached at 380 s,
        // like last time) → 480 of 492 s.
        let (b, t0) = run(&NEOFORGE[..11]);
        let s = b.status(at(t0, 400.0), Some(&r));
        assert_eq!(s.percent, 81);
        assert_eq!(s.left_secs, Some(92));
        assert_eq!(s.usual_secs, Some(492));
        assert!(!s.slow);
        // The long silent step (230 s with no output) still moves.
        let (b, t0) = run(&NEOFORGE[..6]);
        let early = b.status(at(t0, 60.0), Some(&r)).percent;
        let later = b.status(at(t0, 200.0), Some(&r)).percent;
        assert!(early < later, "{early} < {later}");
        assert_eq!(b.status(at(t0, 200.0), Some(&r)).left_secs, Some(292));
    }

    #[test]
    fn a_slower_start_scales_the_time_left() {
        let r = last();
        // Reached loading data at 760 s instead of 380 s: twice as slow.
        let t0 = Instant::now();
        let mut b = Boot::starting(t0);
        for (s, l) in &NEOFORGE[..11] {
            b.push(l, at(t0, s * 2.0));
        }
        let s = b.status(at(t0, 760.0), Some(&r));
        assert_eq!(s.percent, 77);
        assert_eq!(s.left_secs, Some(224), "(492 - 380) × 2");
    }

    #[test]
    fn past_the_usual_time_says_slow_and_stops_guessing() {
        let r = last();
        let (b, t0) = run(&NEOFORGE[..11]);
        let s = b.status(at(t0, 2000.0), Some(&r));
        assert!(s.slow);
        assert_eq!(s.left_secs, None);
        assert!(s.percent < 99 && s.percent >= 90, "{}", s.percent);
    }

    #[test]
    fn percent_only_goes_up_as_steps_come() {
        let r = last();
        let t0 = Instant::now();
        let mut b = Boot::starting(t0);
        let mut prev = 0;
        for (i, (s, l)) in NEOFORGE.iter().enumerate() {
            // A start 30% faster than last time.
            let now = at(t0, s * 0.7);
            b.push(l, now);
            // Look at a few moments before the next line comes.
            let next = NEOFORGE.get(i + 1).map_or(s + 10.0, |n| n.0) * 0.7;
            for f in [0.0, 0.3, 0.9] {
                let p = b.status(at(t0, s * 0.7 + (next - s * 0.7) * f), Some(&r)).percent;
                assert!(p >= prev, "{l}: {p} < {prev}");
                prev = p;
            }
        }
    }

    #[test]
    fn skipped_steps_fill_in_from_the_next() {
        // Fabric: no mod-construction step; the gap is data loading's.
        let r = BootRecord { secs: 100.0, marks: vec![Some(0.0), Some(1.0), Some(10.0), None, Some(50.0), Some(90.0), Some(95.0), Some(100.0)], mods: None };
        assert_eq!(r.timeline(), [0.0, 1.0, 10.0, 50.0, 50.0, 90.0, 95.0, 100.0]);
        // Older records with nothing but the total use typical shares.
        let r = BootRecord { secs: 100.0, marks: vec![], mods: None };
        assert!((r.timeline()[4] - 60.0).abs() < 0.01);
    }

    #[test]
    fn replayed_consoles_are_not_recorded() {
        let mut b = Boot { replayed: true, ..Boot::starting(Instant::now()) };
        b.push("[x] [Server thread/INFO]: Done (1.0s)! For help, type \"help\"", Instant::now());
        assert_eq!(b.phase, Phase::Done);
        assert!(b.record().is_none());
    }

    #[test]
    fn history_keeps_the_last_five() {
        let dir = std::env::temp_dir().join(format!("hopper-boot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut h = BootHistory::load(&dir);
        assert_eq!(h.last(), None);
        for i in 1..=7 {
            h.add(BootRecord { secs: i as f32, marks: vec![], mods: Some(3) });
        }
        h.save(&dir).unwrap();
        let back = BootHistory::load(&dir);
        assert_eq!(back.starts.len(), 5);
        assert_eq!(back.last().unwrap().secs, 7.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn short_times() {
        assert_eq!(short_time(1), "5s");
        assert_eq!(short_time(42), "40s");
        assert_eq!(short_time(61), "1m");
        assert_eq!(short_time(76), "1m 20s");
        assert_eq!(short_time(605), "10m");
    }

    #[test]
    fn threads() {
        assert_eq!(thread("[00:45:23] [Worker-Main-20/INFO] [minecraft/X]: m"), "Worker-Main-20");
        assert_eq!(thread("[10:00:00 INFO]: m"), "");
        assert_eq!(thread("plain"), "");
    }
}
