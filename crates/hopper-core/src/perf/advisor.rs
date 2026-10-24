//! Turns the recorded runs into plain advice: raise or lower memory, try a
//! better preset, notice a slowdown. Pure: no I/O, so the rules are tested
//! with made-up runs. Applying a fix only computes the new settings; the
//! caller saves them the way the Instance settings page does.

use super::{Group, Session, compare};
use crate::jvm_args;
use crate::settings::JavaPreset;

/// Runs shorter than this don't count.
pub const MIN_RUN_SECS: u64 = 60;
/// Runs needed before memory advice (and regressions) is given.
pub const MIN_RUNS: usize = 3;
/// Runs per preset needed before two presets are compared.
pub const MIN_GROUP_RUNS: usize = 2;
/// How many of the newest runs the memory rules look at.
const RECENT: usize = 5;
/// Peak heap at or above this share of the limit counts as full.
const FULL_HEAP: f64 = 0.90;
/// A p99 GC pause longer than this (ms) is long enough to feel (a real
/// 18 GB TNP run starved at 100% heap showed p99 118 ms, worst 2.6 s).
const LONG_PAUSE_MS: f64 = 100.0;
/// One freeze longer than this (ms) counts too.
const FREEZE_MS: f64 = 1000.0;
/// Time spent paused above this share of the run is "frequent".
const PAUSE_SHARE: f64 = 0.03;
/// Memory this far above the peak heap (peak at most this share) is wasted.
const IDLE_HEAP: f64 = 0.55;
/// One raise step.
pub const STEP_MB: u32 = 2048;
/// A preset must beat the current one by this much.
const PRESET_GAIN: f64 = 1.10;
/// ... without being worse than this on the other number.
const PRESET_FLOOR: f64 = 0.95;
/// Load time this many times the earlier median is a regression.
const LOAD_REGRESS: f64 = 1.5;
/// ... and at least this many seconds slower.
const LOAD_MIN_DIFF: f64 = 20.0;
/// 1% lows below this share of the earlier median are a regression.
const LOW_REGRESS: f64 = 0.8;

/// Memory Apply may set at most: half the system RAM, whole 512 MB steps.
/// 0 when the RAM is unknown, so nothing can be applied.
pub fn memory_cap_mb(ram_mb: u64) -> u32 {
    let half = (ram_mb / 2).min(u32::MAX as u64) as u32;
    half / 512 * 512
}

/// The PC's RAM in MB.
pub fn system_ram_mb() -> u64 {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.total_memory() / (1024 * 1024)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    High,
    Warning,
    Tip,
    Info,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Bad,
    Warn,
    Good,
    Plain,
}

/// One number or fact behind a piece of advice.
#[derive(Clone, Debug, PartialEq)]
pub struct Evidence {
    pub text: String,
    pub tone: Tone,
}

/// A change that can be applied in one click.
#[derive(Clone, Debug, PartialEq)]
pub enum Fix {
    SetMemory { to_mb: u32 },
    SwitchPreset { name: String, args: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Advice {
    /// Stable per rule, for keys and tests.
    pub id: &'static str,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    pub evidence: Vec<Evidence>,
    pub fix: Option<Fix>,
    /// Why the fix was held back or capped.
    pub note: Option<String>,
    /// The rule in a sentence, for "Why".
    pub rule: String,
    /// Small chart for "Why", oldest run first, and what it shows.
    pub series: Vec<f32>,
    pub series_label: String,
}

/// What the advisor knows about the instance and the PC.
#[derive(Clone, Debug, Default)]
pub struct Setup {
    /// The memory limit the game runs with now (-Xmx, else the setting).
    pub xmx_mb: u32,
    /// The instance's Java arguments.
    pub args: String,
    /// The PC's RAM, 0 when unknown.
    pub ram_mb: u64,
}

pub struct Report {
    /// Sorted, most urgent first.
    pub advice: Vec<Advice>,
    /// Runs that count (long enough, ended, not crashed).
    pub runs: usize,
    /// Enough runs for the run-based rules.
    pub enough: bool,
}

/// "10 GB", "1.5 GB".
pub fn gb_text(mb: u32) -> String {
    if mb % 1024 == 0 {
        format!("{} GB", mb / 1024)
    } else {
        format!("{:.1} GB", mb as f64 / 1024.0)
    }
}

fn ev(text: impl Into<String>, tone: Tone) -> Evidence {
    Evidence { text: text.into(), tone }
}

/// The settings a fix leads to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub memory_mb: u32,
    pub java_args: String,
}

/// What the Memory setting and Java arguments become with `fix`, the same
/// way the Settings page's slider and preset buttons write them. Memory is
/// never taken above half the PC's RAM (`ram_mb`), whatever the fix says.
pub fn apply(fix: &Fix, memory_mb: u32, args: &str, ram_mb: u64) -> Change {
    let cap = memory_cap_mb(ram_mb);
    let (xms, _) = jvm_args::memory(args);
    match fix {
        Fix::SetMemory { to_mb } => {
            let to = (*to_mb).min(cap).max(1024);
            let xms = xms.filter(|x| *x <= to);
            // Blank arguments stay blank: the Memory setting is enough.
            let java_args = if args.trim().is_empty() && xms.is_none() {
                String::new()
            } else {
                jvm_args::set_memory(args, xms, Some(to))
            };
            Change { memory_mb: to, java_args }
        }
        Fix::SwitchPreset { args: preset, .. } => {
            let (pxms, pxmx) = jvm_args::memory(preset);
            let xmx = pxmx.unwrap_or(memory_mb).min(cap).max(1024);
            let xms = pxms.or(xms).filter(|x| *x <= xmx);
            Change { memory_mb: xmx, java_args: jvm_args::set_memory(preset, xms, Some(xmx)) }
        }
    }
}

fn heap_ratio(s: &Session, xmx_mb: u32) -> Option<f64> {
    let g = s.gc.as_ref()?;
    if g.peak_heap_mb <= 0.0 {
        return None;
    }
    let max = if g.max_heap_mb > 0.0 { g.max_heap_mb } else { xmx_mb as f64 };
    (max > 0.0).then(|| g.peak_heap_mb / max)
}

fn pause_share(s: &Session) -> f64 {
    let secs = s.seconds() as f64;
    match &s.gc {
        Some(g) if secs > 0.0 => g.total_pause_ms / (secs * 1000.0),
        _ => 0.0,
    }
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    super::stats::median(&mut v)
}

/// The advice for an instance. `sessions` in any order.
pub fn advise(sessions: &[Session], presets: &[JavaPreset], setup: &Setup) -> Report {
    let mut runs: Vec<&Session> = sessions
        .iter()
        .filter(|s| s.ended > 0 && s.seconds() >= MIN_RUN_SECS && !s.crashed)
        .collect();
    runs.sort_by_key(|s| s.started);
    let enough = runs.len() >= MIN_RUNS;
    let cap = memory_cap_mb(setup.ram_mb);
    let mut out: Vec<Advice> = Vec::new();

    // Over half the RAM: needs no runs.
    let over_half = setup.ram_mb > 0 && (setup.xmx_mb as u64) * 2 > setup.ram_mb;
    if over_half {
        let fix = (cap >= 2048).then_some(Fix::SetMemory { to_mb: cap });
        out.push(Advice {
            id: "ram-half",
            severity: Severity::Warning,
            title: format!("Memory is more than half of your RAM ({} of {})", gb_text(setup.xmx_mb), gb_text((setup.ram_mb / 1024 * 1024).min(u32::MAX as u64) as u32)),
            detail: "Windows and other apps need room too. When the game takes more than half, Windows can start swapping and the game stutters.".into(),
            evidence: vec![ev(format!("{} set", gb_text(setup.xmx_mb)), Tone::Warn), ev(format!("half your RAM is {}", gb_text(cap)), Tone::Plain)],
            fix,
            note: None,
            rule: "Memory above 50% of the PC's RAM.".into(),
            series: Vec::new(),
            series_label: String::new(),
        });
    }

    // Memory rules look at the newest runs with the memory set now.
    let mut here: Vec<&Session> = runs
        .iter()
        .rev()
        .filter(|s| s.xmx_mb == Some(setup.xmx_mb) && heap_ratio(s, setup.xmx_mb).is_some())
        .take(RECENT)
        .copied()
        .collect();
    here.reverse();
    if here.len() >= MIN_RUNS && setup.xmx_mb > 0 {
        let ratios: Vec<f64> = here.iter().map(|s| heap_ratio(s, setup.xmx_mb).unwrap_or(0.0)).collect();
        let series: Vec<f32> = ratios.iter().map(|r| (*r * 100.0) as f32).collect();
        let hot: Vec<&&Session> = here
            .iter()
            .zip(&ratios)
            .filter(|(s, r)| {
                **r >= FULL_HEAP
                    && s.gc
                        .as_ref()
                        .is_some_and(|g| g.p99_pause_ms > LONG_PAUSE_MS || g.max_pause_ms > FREEZE_MS || pause_share(s) > PAUSE_SHARE)
            })
            .map(|(s, _)| s)
            .collect();
        let peak = ratios.iter().copied().fold(0.0, f64::max);
        let peak_mb = here
            .iter()
            .filter_map(|s| s.gc.as_ref().map(|g| g.peak_heap_mb))
            .fold(0.0, f64::max);
        if hot.len() >= 2 && hot.len() * 2 >= here.len() {
            let worst = hot
                .iter()
                .filter_map(|s| s.gc.as_ref().map(|g| g.p99_pause_ms))
                .fold(0.0, f64::max);
            let want = setup.xmx_mb + STEP_MB;
            let to = want.min(cap);
            let capped = want > cap;
            let fix = (to > setup.xmx_mb).then_some(Fix::SetMemory { to_mb: to });
            let note = if capped && fix.is_some() {
                Some(format!("Capped at {}: half your RAM, so Windows and other apps keep enough.", gb_text(to)))
            } else if capped {
                Some("Memory is already at half your RAM, the most Hopper will suggest, so Windows and other apps keep enough. Fewer mods or another preset can help instead.".into())
            } else {
                None
            };
            out.push(Advice {
                id: "memory-raise",
                severity: Severity::High,
                title: match &fix {
                    Some(Fix::SetMemory { to_mb }) => format!("Raise memory from {} to {}", gb_text(setup.xmx_mb), gb_text(*to_mb)),
                    _ => format!("The game fills its {} of memory", gb_text(setup.xmx_mb)),
                },
                detail: "The game filled almost all of its memory and kept stopping to clean up, which shows as stutter.".into(),
                evidence: vec![
                    ev(format!("Peak heap {} of {} ({:.0}%)", gb_text(peak_mb as u32), gb_text(setup.xmx_mb), peak * 100.0), Tone::Bad),
                    ev(format!("p99 GC pause {worst:.0} ms"), Tone::Bad),
                    ev(format!("{} of {} runs", hot.len(), here.len()), Tone::Plain),
                ],
                fix,
                note,
                rule: format!(
                    "Peak heap over {:.0}% of the limit and a p99 GC pause over {LONG_PAUSE_MS:.0} ms, a freeze over {:.0} s, or over {:.0}% of the time paused, in most of the last runs.",
                    FULL_HEAP * 100.0,
                    FREEZE_MS / 1000.0,
                    PAUSE_SHARE * 100.0
                ),
                series: series.clone(),
                series_label: "Peak heap per run, % of the limit".into(),
            });
        } else if !over_half && setup.xmx_mb >= 4096 && peak <= IDLE_HEAP {
            let need = ((peak_mb * 1.6 / 1024.0).ceil() as u32).max(3) * 1024;
            if need + 1024 <= setup.xmx_mb {
                out.push(Advice {
                    id: "memory-lower",
                    severity: Severity::Tip,
                    title: format!("Lower memory from {} to {}", gb_text(setup.xmx_mb), gb_text(need)),
                    detail: "The game never came close to using what it was given. Lowering it frees RAM for Windows and other apps.".into(),
                    evidence: vec![
                        ev(format!("Peak heap {} of {} ({:.0}%)", gb_text(peak_mb as u32), gb_text(setup.xmx_mb), peak * 100.0), Tone::Good),
                        ev(format!("{} runs", here.len()), Tone::Plain),
                    ],
                    fix: Some(Fix::SetMemory { to_mb: need }),
                    note: None,
                    rule: format!(
                        "Peak heap at most {:.0}% of the limit in the last {} runs; suggests 1.6x the peak, at least 3 GB.",
                        IDLE_HEAP * 100.0,
                        here.len()
                    ),
                    series,
                    series_label: "Peak heap per run, % of the limit".into(),
                });
            }
        }
    }

    // Another preset doing clearly better.
    if enough {
        let groups = compare(sessions, presets, MIN_RUN_SECS);
        let cur_fp = {
            let t = super::tuning_args(&jvm_args::split(&jvm_args::set_memory(&setup.args, None, (setup.xmx_mb > 0).then_some(setup.xmx_mb))));
            super::fingerprint(&t)
        };
        let latest = runs.last().map(|s| s.fingerprint.clone()).unwrap_or_default();
        let cur: Option<&Group> = groups
            .iter()
            .find(|g| g.fingerprint == cur_fp)
            .or_else(|| groups.iter().find(|g| g.fingerprint == latest));
        if let Some(cur) = cur.filter(|g| g.runs >= MIN_GROUP_RUNS)
            && let (Some(cl), Some(ca)) = (cur.low1_fps, cur.avg_fps)
            && cl > 0.0
            && ca > 0.0
        {
            let best = groups
                .iter()
                .filter(|g| g.fingerprint != cur.fingerprint && !g.preset.is_empty() && g.preset != cur.preset && g.runs >= MIN_GROUP_RUNS)
                .filter_map(|g| {
                    let (l, a) = (g.low1_fps?, g.avg_fps?);
                    let better = (l >= cl * PRESET_GAIN && a >= ca * PRESET_FLOOR) || (a >= ca * PRESET_GAIN && l >= cl * PRESET_FLOOR);
                    better.then_some((g, l, a))
                })
                .max_by(|x, y| (x.1 / cl).total_cmp(&(y.1 / cl)));
            if let Some((g, l, a)) = best
                && let Some(p) = presets.iter().find(|p| p.name == g.preset)
            {
                let (_, pxmx) = jvm_args::memory(&p.args);
                let ends = pxmx.unwrap_or(setup.xmx_mb);
                let blocked = setup.ram_mb == 0 || ends > cap;
                let gain = if l >= cl * PRESET_GAIN { (l / cl - 1.0) * 100.0 } else { (a / ca - 1.0) * 100.0 };
                let what = if l >= cl * PRESET_GAIN { "1% lows" } else { "average FPS" };
                let mut evidence = vec![
                    ev(format!("1% low {l:.0} vs {cl:.0} FPS"), Tone::Good),
                    ev(format!("avg {a:.0} vs {ca:.0} FPS"), Tone::Plain),
                    ev(format!("{} runs vs {} runs", g.runs, cur.runs), Tone::Plain),
                ];
                if let (Some(gl), Some(cl2)) = (g.load_secs, cur.load_secs)
                    && gl - cl2 >= 10.0
                {
                    evidence.push(ev(format!("load {:.0} s slower", gl - cl2), Tone::Warn));
                }
                out.push(Advice {
                    id: "preset-switch",
                    severity: Severity::Tip,
                    title: format!("Switch to preset \"{}\": {gain:.0}% better {what}", g.preset),
                    detail: format!("Same pack, same PC: {l:.0} vs {cl:.0} FPS in the slowest 1% of frames."),
                    evidence,
                    fix: (!blocked).then(|| Fix::SwitchPreset { name: p.name.clone(), args: p.args.clone() }),
                    note: blocked.then(|| "That preset sets more than half your RAM, so Hopper won't apply it. Lower its memory first.".to_string()),
                    rule: format!(
                        "Another saved preset has {:.0}%+ better 1% lows or average FPS over at least {MIN_GROUP_RUNS} runs each, without the other number dropping more than {:.0}%.",
                        (PRESET_GAIN - 1.0) * 100.0,
                        (1.0 - PRESET_FLOOR) * 100.0
                    ),
                    series: Vec::new(),
                    series_label: String::new(),
                });
            }
        }
    }

    // Slower than before with the same arguments.
    if let Some(last) = runs.last().filter(|_| enough) {
        let same: Vec<&Session> = runs.iter().filter(|s| s.fingerprint == last.fingerprint).copied().collect();
        if same.len() > MIN_RUNS {
            let earlier = &same[..same.len() - 1];
            let pack_note = match earlier.last() {
                Some(prev) if !last.pack.is_empty() && prev.pack != last.pack && !prev.pack.is_empty() => {
                    format!(" The pack changed from {} to {}.", prev.pack, last.pack)
                }
                _ => String::new(),
            };
            let before_load = median(earlier.iter().filter_map(|s| s.load_secs).collect());
            if let (Some(now), Some(before)) = (last.load_secs, before_load)
                && now >= before * LOAD_REGRESS
                && now - before >= LOAD_MIN_DIFF
            {
                out.push(Advice {
                    id: "load-slower",
                    severity: Severity::Info,
                    title: "Loading takes longer than before".into(),
                    detail: format!("It took {} before and {} now.{pack_note} Nothing to apply; more mods or a bigger pack are the usual cause.", dur(before), dur(now)),
                    evidence: vec![ev(format!("median {before:.0} s to {now:.0} s"), Tone::Warn)],
                    fix: None,
                    note: None,
                    rule: format!("The last run loaded {LOAD_REGRESS}x slower than the median of earlier runs with the same arguments."),
                    series: same.iter().filter_map(|s| s.load_secs.map(|v| v as f32)).collect(),
                    series_label: "Load time per run, seconds".into(),
                });
            }
            let before_low = median(earlier.iter().filter_map(|s| s.frames.as_ref().map(|f| f.low1_fps)).collect());
            if let (Some(now), Some(before)) = (last.frames.as_ref().map(|f| f.low1_fps), before_low)
                && before > 0.0
                && now < before * LOW_REGRESS
            {
                out.push(Advice {
                    id: "low1-worse",
                    severity: Severity::Info,
                    title: "The slowest frames got slower".into(),
                    detail: format!("1% lows were {before:.0} FPS before and {now:.0} now.{pack_note} Nothing to apply; check what changed since the earlier runs."),
                    evidence: vec![ev(format!("1% low {before:.0} to {now:.0} FPS"), Tone::Warn)],
                    fix: None,
                    note: None,
                    rule: format!("The last run's 1% low is under {:.0}% of the median of earlier runs with the same arguments.", LOW_REGRESS * 100.0),
                    series: same.iter().filter_map(|s| s.frames.as_ref().map(|f| f.low1_fps as f32)).collect(),
                    series_label: "1% low per run, FPS".into(),
                });
            }
        }
    }

    out.sort_by_key(|a| a.severity);
    Report { advice: out, runs: runs.len(), enough }
}

fn dur(secs: f64) -> String {
    let s = secs.round() as u64;
    if s >= 60 {
        format!("{} min {} s", s / 60, s % 60)
    } else {
        format!("{s} s")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "reads the real sessions"]
    fn real_sessions() {
        let base = std::path::Path::new(r"G:/Games/Minecraft/Hopper Data/data/sessions");
        let root = std::path::Path::new(r"G:/Games/Minecraft/Curse/Instances/TNP Limitless 8");
        let sessions = crate::perf::load_all(base, root);
        let r = advise(&sessions, &[], &Setup { xmx_mb: 18432, args: String::new(), ram_mb: 61 * 1024 });
        println!("{} sessions", sessions.len());
        for s in &sessions {
            println!("  ended {} secs {} crashed {} xmx {:?}", s.ended > 0, s.seconds(), s.crashed, s.xmx_mb);
        }
        for a in &r.advice {
            println!("{} | {} | {} | note {:?}", a.id, a.title, a.detail, a.note);
        }
    }

    use super::*;
    use crate::perf::gclog::GcStats;
    use crate::perf::stats::FrameStats;

    const GB: u32 = 1024;

    /// A 10-minute run.
    fn run(n: u64, xmx: u32, args: &[&str], low1: f64, avg: f64, peak_frac: f64, p99: f64, load: f64) -> Session {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let fp = super::super::fingerprint(&args);
        Session {
            args,
            fingerprint: fp,
            xmx_mb: Some(xmx),
            started: 1000 * n,
            ended: 1000 * n + 600,
            load_secs: Some(load),
            frames: Some(FrameStats { avg_fps: avg, low1_fps: low1, ..Default::default() }),
            gc: Some(GcStats {
                pauses: 50,
                p99_pause_ms: p99,
                max_pause_ms: p99 * 1.2,
                total_pause_ms: p99 * 5.0,
                peak_heap_mb: xmx as f64 * peak_frac,
                max_heap_mb: xmx as f64,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn setup(xmx: u32, ram_gb: u64) -> Setup {
        Setup { xmx_mb: xmx, args: format!("-Xmx{}G -XX:+UseG1GC", xmx / GB), ram_mb: ram_gb * 1024 }
    }

    fn ids(r: &Report) -> Vec<&'static str> {
        r.advice.iter().map(|a| a.id).collect()
    }

    const G1: &[&str] = &["-XX:+UseG1GC", "-Xmx8G"];

    #[test]
    fn too_few_runs_gives_no_run_advice() {
        let s: Vec<Session> = (1..=2).map(|n| run(n, 8 * GB, G1, 40.0, 90.0, 0.97, 400.0, 100.0)).collect();
        let r = advise(&s, &[], &setup(8 * GB, 48));
        assert!(!r.enough);
        assert!(r.advice.is_empty());
        assert_eq!(r.runs, 2);
    }

    #[test]
    fn short_and_crashed_runs_do_not_count() {
        let mut s: Vec<Session> = (1..=3).map(|n| run(n, 8 * GB, G1, 40.0, 90.0, 0.97, 400.0, 100.0)).collect();
        s[0].ended = s[0].started + 10;
        s[1].crashed = true;
        let r = advise(&s, &[], &setup(8 * GB, 48));
        assert_eq!(r.runs, 1);
        assert!(r.advice.is_empty());
    }

    #[test]
    fn full_heap_with_long_pauses_raises_memory() {
        let s: Vec<Session> = (1..=5).map(|n| run(n, 8 * GB, G1, 44.0, 89.0, 0.97, 410.0, 100.0)).collect();
        let r = advise(&s, &[], &setup(8 * GB, 48));
        let a = &r.advice[0];
        assert_eq!(a.id, "memory-raise");
        assert_eq!(a.severity, Severity::High);
        assert_eq!(a.fix, Some(Fix::SetMemory { to_mb: 10 * GB }));
        assert!(a.note.is_none());
        assert_eq!(a.series.len(), 5);
        // Applying writes Xmx and keeps the rest.
        let c = apply(a.fix.as_ref().unwrap(), 8 * GB, "-Xms4G -Xmx8G -XX:+UseG1GC", 48 * 1024);
        assert_eq!(c, Change { memory_mb: 10 * GB, java_args: "-Xms4G -Xmx10G -XX:+UseG1GC".into() });
    }

    #[test]
    fn full_heap_but_short_pauses_is_fine() {
        let s: Vec<Session> = (1..=5).map(|n| run(n, 8 * GB, G1, 44.0, 89.0, 0.97, 20.0, 100.0)).collect();
        let r = advise(&s, &[], &setup(8 * GB, 48));
        assert!(r.advice.is_empty());
    }

    #[test]
    fn raise_is_capped_at_half_the_ram() {
        // 16 GB PC: cap 8 GB, so 8 -> 10 is refused.
        let s: Vec<Session> = (1..=5).map(|n| run(n, 8 * GB, G1, 44.0, 89.0, 0.97, 410.0, 100.0)).collect();
        let r = advise(&s, &[], &setup(8 * GB, 16));
        let a = &r.advice[0];
        assert_eq!(a.id, "memory-raise");
        assert_eq!(a.fix, None);
        assert!(a.note.as_ref().unwrap().contains("half your RAM"));
        // 20 GB PC: cap 10 GB; 9 -> 11 is capped to 10.
        let s: Vec<Session> = (1..=5).map(|n| run(n, 9 * GB, G1, 44.0, 89.0, 0.97, 410.0, 100.0)).collect();
        let r = advise(&s, &[], &setup(9 * GB, 20));
        let a = &r.advice[0];
        assert_eq!(a.fix, Some(Fix::SetMemory { to_mb: 10 * GB }));
        assert!(a.note.as_ref().unwrap().starts_with("Capped at 10 GB"));
    }

    #[test]
    fn apply_never_goes_over_half_the_ram() {
        let c = apply(&Fix::SetMemory { to_mb: 20 * GB }, 8 * GB, "-Xmx8G", 16 * 1024);
        assert_eq!(c.memory_mb, 8 * GB);
        assert_eq!(c.java_args, "-Xmx8G");
        // Unknown RAM: nothing above the floor.
        let c = apply(&Fix::SetMemory { to_mb: 10 * GB }, 8 * GB, "-Xmx8G", 0);
        assert!(c.memory_mb <= GB);
        // Blank arguments stay blank.
        let c = apply(&Fix::SetMemory { to_mb: 6 * GB }, 4 * GB, "", 32 * 1024);
        assert_eq!(c, Change { memory_mb: 6 * GB, java_args: String::new() });
        // A start size above the new limit is dropped.
        let c = apply(&Fix::SetMemory { to_mb: 4 * GB }, 8 * GB, "-Xms6G -Xmx8G", 32 * 1024);
        assert_eq!(c.java_args, "-Xmx4G");
    }

    #[test]
    fn memory_far_above_the_peak_can_be_lowered() {
        let s: Vec<Session> = (1..=4).map(|n| run(n, 16 * GB, &["-Xmx16G"], 60.0, 100.0, 0.30, 10.0, 100.0)).collect();
        let r = advise(&s, &[], &setup(16 * GB, 64));
        assert_eq!(ids(&r), ["memory-lower"]);
        // Peak 4.8 GB x 1.6 = 7.7 -> 8 GB.
        assert_eq!(r.advice[0].fix, Some(Fix::SetMemory { to_mb: 8 * GB }));
        assert_eq!(r.advice[0].severity, Severity::Tip);
    }

    #[test]
    fn memory_near_the_peak_is_left_alone() {
        let s: Vec<Session> = (1..=4).map(|n| run(n, 8 * GB, G1, 60.0, 100.0, 0.75, 10.0, 100.0)).collect();
        assert!(advise(&s, &[], &setup(8 * GB, 32)).advice.is_empty());
    }

    #[test]
    fn warns_when_memory_is_over_half_the_ram_even_without_runs() {
        let r = advise(&[], &[], &setup(12 * GB, 16));
        assert_eq!(ids(&r), ["ram-half"]);
        let a = &r.advice[0];
        assert_eq!(a.severity, Severity::Warning);
        assert_eq!(a.fix, Some(Fix::SetMemory { to_mb: 8 * GB }));
        assert!(advise(&[], &[], &setup(8 * GB, 16)).advice.is_empty());
    }

    fn presets() -> Vec<JavaPreset> {
        vec![JavaPreset { name: "ZGC".into(), args: "-XX:+UseZGC".into() }]
    }

    #[test]
    fn a_clearly_better_preset_is_suggested() {
        let zgc: &[&str] = &["-XX:+UseZGC", "-Xmx8G"];
        let mut s: Vec<Session> = (1..=5).map(|n| run(n, 8 * GB, G1, 44.0, 89.0, 0.7, 20.0, 100.0)).collect();
        s.extend((6..=8).map(|n| run(n, 8 * GB, zgc, 52.0, 94.0, 0.7, 5.0, 104.0)));
        let r = advise(&s, &presets(), &setup(8 * GB, 48));
        assert_eq!(ids(&r), ["preset-switch"]);
        let a = &r.advice[0];
        assert!(a.title.contains("ZGC"));
        let Some(Fix::SwitchPreset { name, args }) = &a.fix else { panic!("no fix") };
        assert_eq!((name.as_str(), args.as_str()), ("ZGC", "-XX:+UseZGC"));
        // Memory is kept when the preset has none.
        let c = apply(a.fix.as_ref().unwrap(), 8 * GB, "-Xms4G -Xmx8G -XX:+UseG1GC", 48 * 1024);
        assert_eq!(c, Change { memory_mb: 8 * GB, java_args: "-Xms4G -Xmx8G -XX:+UseZGC".into() });
    }

    #[test]
    fn a_slightly_better_or_thin_preset_is_not() {
        let zgc: &[&str] = &["-XX:+UseZGC", "-Xmx8G"];
        // Only 5% better.
        let mut s: Vec<Session> = (1..=4).map(|n| run(n, 8 * GB, G1, 50.0, 90.0, 0.7, 20.0, 100.0)).collect();
        s.extend((5..=7).map(|n| run(n, 8 * GB, zgc, 52.5, 90.0, 0.7, 5.0, 100.0)));
        assert!(advise(&s, &presets(), &setup(8 * GB, 48)).advice.is_empty());
        // Much better, but one run only.
        let mut s: Vec<Session> = (1..=4).map(|n| run(n, 8 * GB, G1, 40.0, 90.0, 0.7, 20.0, 100.0)).collect();
        s.push(run(5, 8 * GB, zgc, 80.0, 120.0, 0.7, 5.0, 100.0));
        assert!(advise(&s, &presets(), &setup(8 * GB, 48)).advice.is_empty());
    }

    #[test]
    fn slower_loading_is_info_without_a_fix() {
        let mut s: Vec<Session> = (1..=4).map(|n| run(n, 8 * GB, G1, 50.0, 90.0, 0.7, 20.0, 130.0)).collect();
        s[3].pack = "Pack 1.77".into();
        s[2].pack = "Pack 1.76".into();
        s.push(run(5, 8 * GB, G1, 50.0, 90.0, 0.7, 20.0, 261.0));
        s[4].pack = "Pack 1.77".into();
        let r = advise(&s, &[], &setup(8 * GB, 48));
        assert_eq!(ids(&r), ["load-slower"]);
        let a = &r.advice[0];
        assert_eq!(a.severity, Severity::Info);
        assert!(a.fix.is_none());
        assert_eq!(a.series.len(), 5);
    }

    #[test]
    fn worse_one_percent_lows_are_info() {
        let mut s: Vec<Session> = (1..=4).map(|n| run(n, 8 * GB, G1, 50.0, 90.0, 0.7, 20.0, 100.0)).collect();
        s.push(run(5, 8 * GB, G1, 30.0, 85.0, 0.7, 20.0, 100.0));
        let r = advise(&s, &[], &setup(8 * GB, 48));
        assert_eq!(ids(&r), ["low1-worse"]);
    }

    #[test]
    fn sorted_most_urgent_first() {
        let mut s: Vec<Session> = (1..=4).map(|n| run(n, 8 * GB, G1, 50.0, 90.0, 0.97, 400.0, 100.0)).collect();
        s.push(run(5, 8 * GB, G1, 20.0, 60.0, 0.97, 400.0, 100.0));
        let r = advise(&s, &[], &setup(8 * GB, 48));
        assert_eq!(ids(&r), ["memory-raise", "low1-worse"]);
    }

    #[test]
    fn cap_rounds_down() {
        assert_eq!(memory_cap_mb(16 * 1024), 8 * GB);
        assert_eq!(memory_cap_mb(0), 0);
        assert_eq!(memory_cap_mb(16 * 1024 - 100), 7680);
        assert_eq!(gb_text(1536), "1.5 GB");
    }
}
