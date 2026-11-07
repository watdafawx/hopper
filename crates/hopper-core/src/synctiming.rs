//! Where the time goes when the Sync tab compares a pack with a server.
//!
//! `begin()` starts a run, the steps add their time with [`time`] (plain
//! `Instant` reads, nothing else), and `finish()` turns the run into one line
//! of plain words. Outside a run (`begin` not called) every call does
//! nothing. Only counts and times are kept: no file names, no paths.
//!
//! The desktop app appends the line to `sync-timing.log` in Hopper's data
//! folder; Hopper Server writes it to its own log.

use std::io::Write;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Stage {
    pub name: &'static str,
    /// What `items` counts ("jars", "entries"), for the line.
    pub unit: &'static str,
    pub time: Duration,
    pub calls: u64,
    pub items: u64,
    pub failed: u64,
}

struct Run {
    started: Instant,
    stages: Vec<Stage>,
}

static RUN: Mutex<Option<Run>> = Mutex::new(None);

fn with_run(f: impl FnOnce(&mut Run)) {
    let mut g = RUN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(r) = g.as_mut() {
        f(r);
    }
}

/// Starts a run: what was timed before is dropped.
pub fn begin() {
    let mut g = RUN.lock().unwrap_or_else(|e| e.into_inner());
    *g = Some(Run { started: Instant::now(), stages: Vec::new() });
}

/// Adds `spent` to a stage (made on first use).
pub fn add(name: &'static str, unit: &'static str, spent: Duration, items: u64, failed: bool) {
    with_run(|r| {
        let i = match r.stages.iter().position(|s| s.name == name) {
            Some(i) => i,
            None => {
                r.stages.push(Stage { name, unit, time: Duration::ZERO, calls: 0, items: 0, failed: 0 });
                r.stages.len() - 1
            }
        };
        let s = &mut r.stages[i];
        s.time += spent;
        s.calls += 1;
        s.items += items;
        s.failed += failed as u64;
    });
}

/// Runs `f` and adds its time to the stage; `items` counts what it handled.
pub fn time<T>(name: &'static str, unit: &'static str, items: u64, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let out = f();
    add(name, unit, t.elapsed(), items, false);
    out
}

/// [`time`] for a step that can fail: a failure is counted.
pub fn time_res<T, E>(name: &'static str, unit: &'static str, items: u64, f: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
    let t = Instant::now();
    let out = f();
    add(name, unit, t.elapsed(), items, out.is_err());
    out
}

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

/// "1,158" for 1158.
fn num(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The line for a run: the total, then each stage that happened, in the
/// order it was first timed. A stage that didn't happen (no remote server,
/// no unknown jars) is left out.
pub fn format_line(total: Duration, stages: &[Stage]) -> String {
    let parts: Vec<String> = stages
        .iter()
        .map(|s| {
            let mut p = format!("{} {} ms", s.name, ms(s.time));
            if s.items > 0 {
                p += &format!(" for {} {}", num(s.items), s.unit);
            }
            if s.calls > 1 {
                p += &format!(" in {} calls", s.calls);
            }
            if s.failed > 0 {
                p += &format!(", {} failed", s.failed);
            }
            p
        })
        .collect();
    if parts.is_empty() {
        format!("sync compare: total {} ms", ms(total))
    } else {
        // The pack's and the server's reads run side by side, so the stages
        // can add up to more than the total.
        format!("sync compare: total {} ms ({}; stages overlap)", ms(total), parts.join(", "))
    }
}

/// Ends the run and returns its line; `None` when no run was started.
pub fn finish() -> Option<String> {
    let run = RUN.lock().unwrap_or_else(|e| e.into_inner()).take()?;
    Some(format_line(run.started.elapsed(), &run.stages))
}

/// [`finish`], appended to `sync-timing.log` in Hopper's data folder (with
/// the time as unix seconds) and to the app's log. The file is started over
/// when it gets big.
pub fn finish_to_file() -> Option<String> {
    let line = finish()?;
    log::info!("{line}");
    if let Some(d) = crate::paths::dirs() {
        let path = d.data_dir().join("sync-timing.log");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 200_000) {
            let _ = std::fs::remove_file(&path);
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let _ = std::fs::create_dir_all(d.data_dir());
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "[{now}] {line}");
        }
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(name: &'static str, unit: &'static str, ms: u64, calls: u64, items: u64, failed: u64) -> Stage {
        Stage { name, unit, time: Duration::from_millis(ms), calls, items, failed }
    }

    #[test]
    fn line_lists_stages_in_order() {
        let l = format_line(
            Duration::from_millis(1840),
            &[st("pack scan", "jars", 83, 1, 1158, 0), st("server fetch", "entries", 380, 1, 1158, 0), st("Modrinth lookups", "jars", 1200, 2, 37, 1)],
        );
        assert_eq!(
            l,
            "sync compare: total 1840 ms (pack scan 83 ms for 1,158 jars, server fetch 380 ms for 1,158 entries, Modrinth lookups 1200 ms for 37 jars in 2 calls, 1 failed; stages overlap)"
        );
    }

    #[test]
    fn line_without_stages_does_not_panic() {
        assert_eq!(format_line(Duration::from_millis(5), &[]), "sync compare: total 5 ms");
    }

    #[test]
    fn run_collects_and_outside_a_run_is_silent() {
        // Not started: nothing kept, nothing returned.
        let _ = finish();
        add("diff", "", Duration::from_millis(1), 0, false);
        assert!(finish().is_none());
        begin();
        let v = time("diff", "", 0, || 7);
        let r: Result<(), ()> = time_res("Modrinth lookups", "jars", 3, || Err(()));
        assert_eq!(v, 7);
        assert!(r.is_err());
        let l = finish().unwrap();
        assert!(l.contains("diff ") && l.contains("Modrinth lookups") && l.contains("1 failed"), "{l}");
        assert!(finish().is_none());
    }

    #[test]
    fn thousands_get_commas() {
        assert_eq!(num(0), "0");
        assert_eq!(num(999), "999");
        assert_eq!(num(1000), "1,000");
        assert_eq!(num(1234567), "1,234,567");
    }
}
