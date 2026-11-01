//! A start or stop in progress, in the one shape every server view shows
//! (Hopper's server card and header, Hopper Server's list, header and
//! overview): a bar, what it is doing, and the time.
//!
//! Starts come from [`super::boot`]. Stops are read here from the console
//! the same way: "Stopping server", "Saving players", "Saving chunks for
//! level …" per dimension, "All dimensions are saved", then Java ends.
//! When it's over, [`Finished`] says how long it took ("Started in 1m 32s").

use super::boot::{BootStatus, Step, StepState};
use super::logparse::message;
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// No console line for this long while stopping: it may be stuck, so the
/// views offer Kill.
pub const STOP_QUIET_SECS: u32 = 30;

/// The steps of a stop, in order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HaltPhase {
    /// "stop" sent; the server hasn't answered yet.
    #[default]
    Asked,
    Stopping,
    SavingPlayers,
    SavingWorlds,
    /// Every world is saved; Java has yet to end.
    Saved,
}

impl HaltPhase {
    pub fn label(self) -> &'static str {
        match self {
            HaltPhase::Asked => "Stopping…",
            HaltPhase::Stopping => "Stopping the server",
            HaltPhase::SavingPlayers => "Saving players",
            HaltPhase::SavingWorlds => "Saving chunks",
            HaltPhase::Saved => "All dimensions are saved",
        }
    }
}

/// What the console said so far about a stop.
#[derive(Clone, Debug, Default)]
pub struct Halt {
    /// When the stop was asked for, or the server said "Stopping server"
    /// (a stop typed in the game). None: no stop under way.
    pub began: Option<Instant>,
    pub phase: HaltPhase,
    /// Worlds whose chunks were saved or are being saved, in order.
    pub dims: Vec<String>,
}

impl Halt {
    pub fn asked(at: Instant) -> Halt {
        Halt { began: Some(at), ..Default::default() }
    }

    fn reach(&mut self, p: HaltPhase) {
        if p > self.phase {
            self.phase = p;
        }
    }

    pub fn push(&mut self, line: &str, now: Instant) {
        let msg = message(line).trim();
        if msg == "Stopping server" || msg.starts_with("Stopping the server") {
            self.began.get_or_insert(now);
            self.reach(HaltPhase::Stopping);
        }
        // A save-all while running says the same; only a stop counts.
        if self.began.is_none() {
            return;
        }
        if msg == "Saving players" {
            self.reach(HaltPhase::SavingPlayers);
        } else if msg == "Saving worlds" {
            self.reach(HaltPhase::SavingWorlds);
        } else if let Some(rest) = msg.strip_prefix("Saving chunks for level ") {
            self.reach(HaltPhase::SavingWorlds);
            let d = dim_name(rest);
            if !d.is_empty() && !self.dims.contains(&d) {
                self.dims.push(d);
            }
        } else if msg.contains("All dimensions are saved") {
            self.reach(HaltPhase::Saved);
        }
    }

    /// Where the stop is at `now`. `heard`: the console's last line;
    /// `deadline`: when Hopper ends Java if it hasn't stopped.
    pub fn status(&self, now: Instant, heard: Option<Instant>, deadline: Option<Instant>) -> Option<Progress> {
        let began = self.began?;
        let elapsed = now.saturating_duration_since(began).as_secs();
        let quiet = now.saturating_duration_since(heard.map_or(began, |h| h.max(began))).as_secs() as u32;
        // Shares of a typical stop: most of it is saving chunks.
        let worlds = self.dims.len().max(1) as f32;
        let percent: f32 = match self.phase {
            HaltPhase::Asked => 3.0,
            HaltPhase::Stopping => 10.0,
            HaltPhase::SavingPlayers => 20.0,
            // Three worlds in vanilla; packs add more and stretch it.
            HaltPhase::SavingWorlds => 25.0 + 60.0 * (worlds - 0.5).max(0.0) / worlds.max(3.0),
            // Java's own shutdown is quick; creep while it runs.
            HaltPhase::Saved => 90.0 + (quiet as f32 / 2.0).min(9.0),
        };
        let label = match self.phase {
            HaltPhase::SavingWorlds if !self.dims.is_empty() => format!("Saving chunks ({}…)", self.dims.join(", ")),
            p => p.label().to_string(),
        };
        let stuck = quiet >= STOP_QUIET_SECS;
        let ends_in = deadline.map(|d| d.saturating_duration_since(now).as_secs() as u32);
        let detail = if stuck {
            let mut t = format!("Nothing in the console for {}", exact_time(quiet));
            if let Some(e) = ends_in {
                t.push_str(&format!(" · Hopper ends it in {}", exact_time(e)));
            }
            t
        } else if self.phase == HaltPhase::Saved {
            format!("Waiting for Java to end · {}", exact_time(elapsed as u32))
        } else {
            format!("{} so far", exact_time(elapsed as u32))
        };
        let steps = [HaltPhase::Stopping, HaltPhase::SavingPlayers, HaltPhase::SavingWorlds, HaltPhase::Saved]
            .into_iter()
            .map(|p| Step {
                label: match p {
                    HaltPhase::SavingWorlds if !self.dims.is_empty() => format!("Saving chunks ({})", self.dims.join(", ")),
                    _ => p.label().to_string(),
                },
                state: if p == self.phase {
                    StepState::Now
                } else if p < self.phase {
                    StepState::Done
                } else {
                    StepState::Next
                },
            })
            .collect();
        Some(Progress {
            kind: ProgressKind::Stop,
            percent: percent.round().clamp(1.0, 99.0) as u8,
            label,
            short: if stuck { format!("Quiet for {}", exact_time(quiet)) } else { exact_time(elapsed as u32) },
            detail,
            elapsed_secs: elapsed as u32,
            left_secs: None,
            usual_secs: None,
            slow: false,
            stuck,
            ends_in_secs: ends_in,
            restarting: false,
            steps,
        })
    }
}

/// "ServerLevel[world]'/minecraft:the_nether" → "nether";
/// "…/twilightforest:twilight_forest" → "twilight forest".
fn dim_name(rest: &str) -> String {
    let id = rest.trim().trim_matches('\'').rsplit(['/', '\'']).next().unwrap_or("");
    let path = id.rsplit(':').next().unwrap_or(id).trim_matches('\'');
    let path = path.strip_prefix("the_").unwrap_or(path);
    path.replace('_', " ")
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressKind {
    #[default]
    Start,
    Stop,
}

/// A start or stop under way, ready to show.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Progress {
    pub kind: ProgressKind,
    /// 1–99.
    pub percent: u8,
    /// What it is doing: "Loading mods", "Preparing the world 64%",
    /// "Saving chunks (overworld, nether…)".
    pub label: String,
    /// The time, short, for a small card: "~55s left", "7s".
    pub short: String,
    /// The time in a sentence: "32s so far · about 55s left (usually 1m 30s)".
    pub detail: String,
    pub elapsed_secs: u32,
    pub left_secs: Option<u32>,
    pub usual_secs: Option<u32>,
    /// A start taking longer than the last one.
    pub slow: bool,
    /// A stop that has gone quiet: offer Kill.
    pub stuck: bool,
    /// A stop: when Hopper ends Java on its own.
    pub ends_in_secs: Option<u32>,
    /// Part of a restart (the stop half, then the start half).
    pub restarting: bool,
    pub steps: Vec<Step>,
}

impl Progress {
    /// A start, from the boot tracker.
    pub fn start(b: &BootStatus) -> Progress {
        let label = match (b.phase, b.mods, b.spawn) {
            (super::boot::Phase::FindingMods, Some(n), _) => format!("Finding mods ({n})"),
            (super::boot::Phase::LoadingMods, Some(n), _) => format!("Loading {n} mods"),
            (super::boot::Phase::PreparingWorld, _, Some(p)) => format!("Preparing the world {p}%"),
            _ => b.label.to_string(),
        };
        let so_far = format!("{} so far", exact_time(b.elapsed_secs));
        let (short, detail) = match (b.slow, b.left_secs, b.usual_secs) {
            (true, _, Some(u)) => ("Taking longer".to_string(), format!("{so_far} · taking longer than usual ({})", super::boot::short_time(u))),
            (_, Some(l), Some(u)) => (
                format!("~{} left", super::boot::short_time(l)),
                format!("{so_far} · about {} left (usually {})", super::boot::short_time(l), super::boot::short_time(u)),
            ),
            _ => (exact_time(b.elapsed_secs), format!("{so_far} · first start, no estimate yet")),
        };
        Progress {
            kind: ProgressKind::Start,
            percent: b.percent.clamp(1, 99),
            label,
            short,
            detail,
            elapsed_secs: b.elapsed_secs,
            left_secs: b.left_secs,
            usual_secs: b.usual_secs,
            slow: b.slow,
            stuck: false,
            ends_in_secs: None,
            restarting: false,
            steps: b.steps.clone(),
        }
    }

    /// The state pill: "Starting 45%", "Restarting 45%", "Stopping",
    /// "Not stopping".
    pub fn pill(&self) -> String {
        match self.kind {
            ProgressKind::Start if self.restarting => format!("Restarting {}%", self.percent),
            ProgressKind::Start => format!("Starting {}%", self.percent),
            ProgressKind::Stop if self.stuck => "Not stopping".into(),
            ProgressKind::Stop if self.restarting => "Restarting".into(),
            ProgressKind::Stop => "Stopping".into(),
        }
    }

    /// The steps for a tooltip, one a line.
    pub fn steps_text(&self) -> String {
        self.steps
            .iter()
            .map(|s| {
                let mark = match s.state {
                    StepState::Done => "✓",
                    StepState::Now => "▶",
                    StepState::Next => "·",
                };
                format!("{mark} {}", s.label)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A start or stop that just ended, for the "Started in 1m 32s" toast.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finished {
    pub kind: ProgressKind,
    pub secs: u32,
    /// How long ago it ended (filled in when sent).
    #[serde(default)]
    pub ago_secs: u32,
    /// The stop half of a restart (its start says it's done).
    #[serde(default)]
    pub restarting: bool,
}

/// How long a finished start or stop stays news.
pub const FINISHED_FRESH_SECS: u64 = 20;

impl Finished {
    /// "Started in 1m 32s" / "Stopped in 8s".
    pub fn text(&self) -> String {
        match self.kind {
            ProgressKind::Start => format!("Started in {}", exact_time(self.secs)),
            ProgressKind::Stop => format!("Stopped in {}", exact_time(self.secs)),
        }
    }
}

/// "8s", "1m 32s", "12m": to the second (for times that have passed).
pub fn exact_time(secs: u32) -> String {
    match (secs / 60, secs % 60) {
        (0, s) => format!("{s}s"),
        (m, 0) => format!("{m}m"),
        (m, s) => format!("{m}m {s}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const STOP_NEO: &[&str] = &[
        "[00:51:02] [Server thread/INFO] [minecraft/MinecraftServer]: Stopping server",
        "[00:51:02] [Server thread/INFO] [minecraft/MinecraftServer]: Saving players",
        "[00:51:02] [Server thread/INFO] [minecraft/MinecraftServer]: Saving worlds",
        "[00:51:03] [Server thread/INFO] [minecraft/MinecraftServer]: Saving chunks for level 'ServerLevel[world]'/minecraft:overworld",
        "[00:51:05] [Server thread/INFO] [minecraft/MinecraftServer]: Saving chunks for level 'ServerLevel[world]'/minecraft:the_nether",
        "[00:51:06] [Server thread/INFO] [minecraft/MinecraftServer]: Saving chunks for level 'ServerLevel[world]'/minecraft:the_end",
        "[00:51:07] [Server thread/INFO] [minecraft/ChunkMap]: ThreadedAnvilChunkStorage (world): All chunks are saved",
        "[00:51:07] [Server thread/INFO] [minecraft/ChunkMap]: ThreadedAnvilChunkStorage: All dimensions are saved",
    ];

    #[test]
    fn stop_steps_and_worlds() {
        let t0 = Instant::now();
        let mut h = Halt::asked(t0);
        assert_eq!(h.status(t0, None, None).unwrap().label, "Stopping…");
        for l in &STOP_NEO[..2] {
            h.push(l, t0);
        }
        assert_eq!(h.phase, HaltPhase::SavingPlayers);
        for l in &STOP_NEO[2..5] {
            h.push(l, t0);
        }
        let s = h.status(t0 + Duration::from_secs(4), Some(t0 + Duration::from_secs(3)), None).unwrap();
        assert_eq!(s.label, "Saving chunks (overworld, nether…)");
        assert_eq!(s.short, "4s");
        assert!(!s.stuck);
        let p1 = s.percent;
        for l in &STOP_NEO[5..] {
            h.push(l, t0);
        }
        let s = h.status(t0 + Duration::from_secs(5), Some(t0 + Duration::from_secs(5)), None).unwrap();
        assert_eq!(h.dims, ["overworld", "nether", "end"]);
        assert_eq!(s.label, "All dimensions are saved");
        assert!(s.percent > p1 && s.percent >= 90, "{} after {p1}", s.percent);
        assert_eq!(s.steps.iter().filter(|s| s.state == StepState::Done).count(), 3);
    }

    #[test]
    fn save_all_while_running_is_not_a_stop() {
        let mut h = Halt::default();
        h.push("[x] [Server thread/INFO]: Saving chunks for level 'ServerLevel[world]'/minecraft:overworld", Instant::now());
        h.push("[x] [Server thread/INFO]: ThreadedAnvilChunkStorage: All dimensions are saved", Instant::now());
        assert!(h.began.is_none() && h.status(Instant::now(), None, None).is_none());
    }

    #[test]
    fn a_quiet_stop_is_stuck() {
        let t0 = Instant::now();
        let mut h = Halt::asked(t0);
        h.push(STOP_NEO[0], t0);
        let s = h.status(t0 + Duration::from_secs(45), Some(t0), Some(t0 + Duration::from_secs(120))).unwrap();
        assert!(s.stuck);
        assert_eq!(s.pill(), "Not stopping");
        assert_eq!(s.ends_in_secs, Some(75));
        assert_eq!(s.detail, "Nothing in the console for 45s · Hopper ends it in 1m 15s");
    }

    #[test]
    fn dim_names() {
        assert_eq!(dim_name("'ServerLevel[world]'/minecraft:the_nether"), "nether");
        assert_eq!(dim_name("'ServerLevel[world]'/twilightforest:twilight_forest"), "twilight forest");
        assert_eq!(dim_name("'world'/minecraft:overworld"), "overworld");
    }

    #[test]
    fn times() {
        assert_eq!(exact_time(8), "8s");
        assert_eq!(exact_time(92), "1m 32s");
        assert_eq!(exact_time(120), "2m");
        assert_eq!(Finished { kind: ProgressKind::Start, secs: 92, ago_secs: 0, restarting: false }.text(), "Started in 1m 32s");
    }
}
