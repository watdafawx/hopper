//! Crashes whose report can't name a mod because the game looped until it
//! ran out of stack. Java keeps only the last 1,024 frames of a
//! `StackOverflowError`, so the report shows one library frame over and
//! over (`DataResult$Error.lambda$ap$2` a thousand times) and nothing of
//! the mod that started the loop. Used by [`crate::crashdetect`].
//!
//! The game log usually still knows: in the seconds before the crash, on the
//! same thread, the same error came again and again, and each copy has its
//! full trace. The first mod in those traces that isn't a library (EMI,
//! DataFixerUpper and Mixin only pass the work on) started the loop. When
//! the last errors are all about one mod's items, that mod's items set the
//! loop off: it is the trigger, not the cause.

use crate::crashdetect::{Certainty, Raw};
use regex::Regex;
use std::sync::LazyLock;

fn re(p: &str) -> Regex {
    Regex::new(p).expect("static regex")
}

/// Java's default cap on the frames it keeps of one trace.
const FRAME_CAP: usize = 1024;
/// How far before the crash report's time the log still counts. The report
/// is written a few seconds after the last error: walking 1,024 frames
/// takes time.
const WINDOW_SECS: i64 = 20;

/// Code that runs other mods' work or the game's: never the one that started
/// a loop when a mod's frame sits under it. The game's own libraries, recipe
/// viewers (their API calls into every mod's items) and
/// [`crate::crashtrace::is_pass_through`].
const LIBRARIES: [&str; 12] = [
    "datafixerupper", "brigadier", "authlib", "minecraft", "neoforge", "forge", "fml", "fabricloader", "emi", "jei",
    "roughlyenoughitems", "rei",
];

fn is_library(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    LIBRARIES.contains(&id.as_str()) || crate::crashtrace::is_pass_through(&id)
}

/// A crash that is the end of a loop no report frame explains.
#[derive(Clone, Debug, PartialEq)]
pub struct Loop {
    /// The library it looped in, for people ("DataFixerUpper, the game's
    /// data library").
    pub library: String,
    /// The thread that crashed ("Render thread"), empty when unknown.
    pub thread: String,
    /// The report's time of day, in seconds (the log has no date on Fabric).
    pub time: Option<i64>,
}

impl Loop {
    /// What happened, in one sentence.
    pub fn sentence(&self) -> String {
        format!(
            "The game got stuck in a loop inside {} and ran out of stack, so the crash report can't show which mod started it.",
            self.library
        )
    }
}

/// `Time: 2026-11-03 06:15:31`
static REPORT_TIME: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^Time: \d{4}-\d{2}-\d{2} (\d{1,2}):(\d{2}):(\d{2})"));

/// The frame without its jar and transformer notes: two copies of one call
/// site read the same.
fn site(frame: &str) -> &str {
    let t = frame.trim();
    match t.find(')') {
        Some(i) => &t[..=i],
        None => t,
    }
}

/// The library a frame is in, for people.
fn library_name(frame: &str) -> String {
    static MODULE: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*at\s+(?:[\w-]+/)?([\w.-]+)@[^/\s]+/"));
    static CLASS: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*at\s+(?:[\w.@+-]+/)*([a-z][\w]*(?:\.[a-z][\w]*)*)\.[A-Z]"));
    let module = MODULE.captures(frame).map(|m| m[1].to_ascii_lowercase()).unwrap_or_default();
    match module.as_str() {
        "datafixerupper" => return "DataFixerUpper (the game's data library)".into(),
        "brigadier" => return "Brigadier (the game's command library)".into(),
        "minecraft" => return "the game's own code".into(),
        m if m.starts_with("java.") || m.starts_with("jdk.") => return "Java's own code".into(),
        m if !m.is_empty() && !m.contains('.') => return m.to_string(),
        _ => {}
    }
    match CLASS.captures(frame).map(|m| m[1].to_string()) {
        Some(p) if p.starts_with("com.mojang.serialization") || p.starts_with("com.mojang.datafixers") => {
            "DataFixerUpper (the game's data library)".into()
        }
        Some(p) if p.starts_with("java.") || p.starts_with("jdk.") => "Java's own code".into(),
        Some(p) if p.starts_with("net.minecraft.") => "the game's own code".into(),
        Some(p) => p,
        None => "one library".into(),
    }
}

/// The mod a frame belongs to, if it is a mod's: its module id, or the mod
/// whose mixin the frame is (`handler$…$modid$…`). `Some("")` for a mod jar
/// with no id.
fn frame_owner(frame: &str) -> Option<String> {
    if let Some(m) = crate::crashdetect::MIXIN_METHOD.captures(frame) {
        return Some(m[1].to_string());
    }
    crate::crashtrace::frame_mod(frame).map(|(id, jar)| if id.is_empty() { jar } else { id })
}

/// The crash report's trace is one library frame repeating, cut off by
/// Java, with no mod in it: the report can't say who started it.
pub fn truncated(text: &str) -> Option<Loop> {
    let error = crate::crashtrace::report_error(text)?;
    let frames = crate::crashtrace::frames_after(text, &error);
    if frames.is_empty() || !(error.contains("StackOverflowError") || frames.len() >= FRAME_CAP) {
        return None;
    }
    // The site most of the frames are: the loop.
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for f in &frames {
        *counts.entry(site(f)).or_default() += 1;
    }
    let (top, n) = counts.iter().max_by_key(|(_, n)| **n).map(|(s, n)| (*s, *n))?;
    if n < 8 || n * 2 < frames.len() {
        return None;
    }
    // A mod's own frame in the loop names it: the usual reading does that.
    if frames.iter().any(|f| frame_owner(f).is_some_and(|id| is_mod(&id))) {
        return None;
    }
    let frame = frames.iter().find(|f| site(f) == top)?;
    let thread = text
        .lines()
        .skip_while(|l| l.trim() != "-- Head --")
        .take(4)
        .find_map(|l| l.trim().strip_prefix("Thread: "))
        .unwrap_or_default()
        .to_string();
    let time = REPORT_TIME.captures(text).map(|m| secs_of_day(&m[1], &m[2], &m[3]));
    Some(Loop { library: library_name(frame), thread, time })
}

/// A jar that is the game's or a library's (`datafixerupper-8.0.16.jar`).
fn is_library_jar(jar: &str) -> bool {
    let j = jar.to_ascii_lowercase();
    LIBRARIES.iter().any(|l| j.starts_with(&format!("{l}-")))
        || ["client-", "server-", "neoforge-", "forge-", "bus-", "eventbus", "modlauncher", "bootstraplauncher", "securejarhandler", "mixin", "guava", "gson", "fastutil", "netty", "lwjgl", "log4j", "slf4j", "jopt"]
            .iter()
            .any(|p| j.starts_with(p))
}

/// A mod's id (or jar) that isn't a library's or the game's.
fn is_mod(id: &str) -> bool {
    if id.ends_with(".jar") { !is_library_jar(id) } else { !id.is_empty() && !is_library(id) }
}

fn secs_of_day(h: &str, m: &str, s: &str) -> i64 {
    h.parse::<i64>().unwrap_or(0) * 3600 + m.parse::<i64>().unwrap_or(0) * 60 + s.parse::<i64>().unwrap_or(0)
}

/// A log line that starts an entry: `[03Nov2026 06:15:26.863] [Render
/// thread/ERROR] [EMI/]: msg` (NeoForge, Forge), `[06:15:26] [Render
/// thread/ERROR]: msg` (vanilla, Fabric).
static ENTRY: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^\[(?:\d{1,2}\w{3}\d{4} )?(\d{2}):(\d{2}):(\d{2})(?:\.\d+)?\] \[([^\]]+)/([A-Z]+)\](?: \[[^\]]*\])?:?\s?(.*)$")
});

/// One ERROR or WARN in the log, with its trace.
struct Entry<'a> {
    secs: i64,
    thread: &'a str,
    message: &'a str,
    /// The exception line (`java.lang.IllegalStateException: …`).
    error: &'a str,
    frames: Vec<&'a str>,
}

fn entries(log: &str) -> Vec<Entry<'_>> {
    let mut out: Vec<Entry> = Vec::new();
    let mut open = false;
    for l in log.lines() {
        if let Some(m) = ENTRY.captures(l) {
            open = matches!(&m[5], "ERROR" | "WARN" | "FATAL");
            if open {
                out.push(Entry {
                    secs: secs_of_day(&m[1], &m[2], &m[3]),
                    thread: m.get(4).map_or("", |g| g.as_str()),
                    message: m.get(6).map_or("", |g| g.as_str()),
                    error: "",
                    frames: Vec::new(),
                });
            }
            continue;
        }
        let Some(e) = out.last_mut().filter(|_| open) else { continue };
        let t = l.trim_start();
        if t.starts_with("at ") {
            e.frames.push(l);
        } else if e.error.is_empty() && e.frames.is_empty() && !t.is_empty() {
            e.error = t;
        }
    }
    out
}

/// The mods in a trace, top first, libraries left out.
fn mods_in(frames: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in frames {
        let Some(id) = frame_owner(f) else { continue };
        if !is_mod(&id) {
            continue;
        }
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&id)) {
            out.push(id);
        }
    }
    out
}

/// What the log says about the loop.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    /// The mod whose code the repeated errors came from.
    pub initiator: String,
    /// Its version, as the frames print it ("2.0.0-beta6").
    pub version: String,
    /// How many errors came from it in the window.
    pub count: usize,
    /// The error, short ("IllegalStateException: Can't access registry …").
    pub error: String,
    /// Mods under it in the trace: they called it.
    pub callers: Vec<String>,
    /// The mod whose items the last errors were about: (namespace, of how
    /// many of the last errors, out of how many).
    pub trigger: Option<(String, usize, usize)>,
}

/// `silentgear:sword` in a message: the item an error was about.
static ITEM_ID: LazyLock<Regex> = LazyLock::new(|| re(r"\b([a-z][a-z0-9_.-]*):[a-z0-9_/.-]+"));

/// The mod that kept failing on the crashed thread in the seconds before the
/// crash, from the game log (`latest.log` or `debug.log`).
pub fn initiator(l: &Loop, log: &str) -> Option<Found> {
    let all = entries(log);
    let anchor = l.time.or_else(|| all.last().map(|e| e.secs))?;
    let window: Vec<&Entry> = all
        .iter()
        .filter(|e| l.thread.is_empty() || e.thread == l.thread)
        .filter(|e| {
            // Seconds before the report's time, across midnight too; the log
            // may run a moment past it.
            let d = (anchor - e.secs).rem_euclid(86_400);
            d <= WINDOW_SECS || d >= 86_400 - 5
        })
        .filter(|e| !e.frames.is_empty())
        .collect();
    // Each error's first mod: the one that asked the libraries for the work.
    let firsts: Vec<(usize, Vec<String>)> =
        window.iter().enumerate().filter_map(|(i, e)| Some((i, mods_in(&e.frames))).filter(|(_, m)| !m.is_empty())).collect();
    let mut counts: Vec<(String, usize, usize)> = Vec::new(); // (mod, count, last index)
    for (i, mods) in &firsts {
        match counts.iter_mut().find(|c| c.0.eq_ignore_ascii_case(&mods[0])) {
            Some(c) => {
                c.1 += 1;
                c.2 = *i;
            }
            None => counts.push((mods[0].clone(), 1, *i)),
        }
    }
    let (initiator, count, last) = counts.into_iter().max_by_key(|c| (c.1, c.2))?;
    let entry = window[last];
    let version = entry
        .frames
        .iter()
        .find(|f| frame_owner(f).is_some_and(|id| id.eq_ignore_ascii_case(&initiator)))
        .and_then(|f| crate::crashtrace::frame_version(f))
        .unwrap_or_default();
    // The mods under it with code of their own in the trace. A mixin that
    // only wraps a step of the game's (every tick runs through it) isn't one.
    let own_frame = |id: &str| entry.frames.iter().any(|f| crate::crashtrace::frame_mod(f).is_some_and(|(m, _)| m.eq_ignore_ascii_case(id)));
    let callers = firsts
        .iter()
        .find(|(i, _)| *i == last)
        .map(|(_, m)| m[1..].iter().filter(|c| own_frame(c)).cloned().collect())
        .unwrap_or_default();
    let error = short_error(entry.error);
    // The last errors before the crash: what they were about.
    let recent: Vec<&Entry> = window.iter().rev().take(5).copied().collect();
    let mut about: Vec<(String, usize)> = Vec::new();
    for e in &recent {
        let Some(ns) = ITEM_ID
            .captures_iter(e.message)
            .map(|m| m[1].to_string())
            .find(|ns| !matches!(ns.as_str(), "minecraft" | "http" | "https") && !is_library(ns))
        else {
            continue;
        };
        match about.iter_mut().find(|a| a.0 == ns) {
            Some(a) => a.1 += 1,
            None => about.push((ns, 1)),
        }
    }
    let trigger = about
        .into_iter()
        .max_by_key(|a| a.1)
        .filter(|(ns, k)| *k >= 3 && *k * 2 > recent.len() && !ns.eq_ignore_ascii_case(&initiator))
        .map(|(ns, k)| (ns, k, recent.len()));
    Some(Found { initiator, version, count, error, callers, trigger })
}

/// `java.lang.IllegalStateException: Can't access registry …; Can't access
/// registry …` → `IllegalStateException: Can't access registry …` (the
/// same message, chained again for every failure, said once).
fn short_error(e: &str) -> String {
    let e = e.strip_prefix("java.lang.").unwrap_or(e);
    let e = e.split("; ").next().unwrap_or(e).trim();
    if e.chars().count() > 160 { format!("{}…", e.chars().take(160).collect::<String>()) } else { e.to_string() }
}

/// A version the author marks as not finished.
pub fn is_prerelease(version: &str) -> bool {
    let v = version.to_ascii_lowercase();
    ["alpha", "beta", "-pre", "-rc", "snapshot"].iter().any(|p| v.contains(p))
}

/// The findings: the mod that started the loop (likely, possible when it
/// failed only once or twice), the mods that called it, and the mod whose
/// items set it off (possible).
pub fn raws(l: &Loop, f: &Found) -> Vec<Raw> {
    let raw = |id: &str, certainty, why: String| Raw {
        mod_id: id.to_string(),
        jar: String::new(),
        certainty,
        why,
        class: String::new(),
        needs: Vec::new(),
    };
    let on = if l.thread.is_empty() { String::new() } else { format!(" on the {}", l.thread) };
    let times = if f.count == 1 { "once".to_string() } else { format!("{} times", f.count) };
    let error = if f.error.is_empty() { String::new() } else { format!(": {}", f.error) };
    let what_to_do = if is_prerelease(&f.version) {
        format!(" It's a test version ({}): update it, or turn it off and play again.", f.version)
    } else {
        " Update it, or turn it off and play again.".into()
    };
    let certainty = if f.count >= 3 { Certainty::Likely } else { Certainty::Possible };
    let mut out = vec![raw(
        &f.initiator,
        certainty,
        format!("{} Just before it, this mod's code failed {times}{on}{error}.{what_to_do}", l.sentence()),
    )];
    for c in &f.callers {
        out.push(raw(
            c,
            Certainty::Possible,
            format!("Its code called {}, where the errors happened, so it was on the way. Involved, but unlikely the cause.", f.initiator),
        ));
    }
    if let Some((ns, k, of)) = &f.trigger {
        out.push(raw(
            ns,
            Certainty::Possible,
            format!(
                "The last errors before the crash were about its items ({k} of the last {of}): they set the loop off, but the errors came from {}. It's the trigger, not the cause.",
                f.initiator
            ),
        ));
    }
    out
}

/// "You added it today." for a mod added in the last two weeks.
pub fn added_note(added: i64, now: i64) -> Option<String> {
    let days = now.div_euclid(86_400) - added.div_euclid(86_400);
    match days {
        i64::MIN..=-1 => None,
        0 => Some("You added it today.".into()),
        1 => Some("You added it yesterday.".into()),
        2..=13 => Some(format!("You added it {days} days ago.")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = include_str!("crashdetect_fixtures/emi-loop-crash.txt");
    const LOG: &str = include_str!("crashdetect_fixtures/emi-loop-latest.log");

    #[test]
    fn a_stack_overflow_in_a_library_is_a_loop() {
        let l = truncated(REPORT).expect("one library frame, over and over");
        assert_eq!(l.library, "DataFixerUpper (the game's data library)");
        assert_eq!(l.thread, "Render thread");
        assert_eq!(l.time, Some(6 * 3600 + 15 * 60 + 31));
        assert!(l.sentence().contains("can't show which mod started it"));
    }

    #[test]
    fn a_loop_in_a_mods_own_code_is_not_this_case() {
        let mine = REPORT.replace(
            "MC-BOOTSTRAP/datafixerupper@8.0.16/com.mojang.serialization.DataResult$Error.lambda$ap$2(DataResult.java:354) ~[datafixerupper-8.0.16.jar%23150!/:?]",
            "TRANSFORMER/loopy@1.0/com.example.loopy.Walker.walk(Walker.java:12) ~[loopy-1.0.jar%23150!/:?]",
        );
        assert!(truncated(&mine).is_none(), "the usual reading names loopy");
        // Some other error with a short trace isn't a loop.
        let other = REPORT.replace("java.lang.StackOverflowError: Unexpected error", "java.lang.NullPointerException: Unexpected error");
        assert!(truncated(&other).is_none());
    }

    #[test]
    fn a_full_trace_of_one_frame_counts_without_the_stack_overflow_name() {
        let frame = "\tat MC-BOOTSTRAP/datafixerupper@8.0.16/com.mojang.serialization.DataResult$Error.lambda$ap$2(DataResult.java:354) ~[datafixerupper-8.0.16.jar%23150!/:?] {re:mixin}\n";
        let text = format!("Time: 2026-11-03 06:15:31\nDescription: Unexpected error\n\njava.lang.IllegalStateException: deep\n{}\n", frame.repeat(FRAME_CAP));
        assert!(truncated(&text).is_some());
    }

    #[test]
    fn the_log_names_the_mod_that_kept_failing_and_the_trigger() {
        let l = truncated(REPORT).unwrap();
        let f = initiator(&l, LOG).expect("the log has the repeated errors");
        assert_eq!(f.initiator, "collapsible_groups");
        assert_eq!(f.version, "2.0.0-beta6");
        assert_eq!(f.count, 10, "every EMI error on the Render thread, not the other thread's or the old one");
        assert!(f.error.starts_with("IllegalStateException: Can't access registry ResourceKey[minecraft:root / minecraft:block]"), "{}", f.error);
        assert!(!f.error.contains(';'), "said once: {}", f.error);
        assert_eq!(f.callers, ["mysticalagricultureplus"]);
        assert_eq!(f.trigger, Some(("silentgear".into(), 4, 5)));

        let r = raws(&l, &f);
        let got: Vec<(&str, Certainty)> = r.iter().map(|r| (r.mod_id.as_str(), r.certainty)).collect();
        assert_eq!(
            got,
            [("collapsible_groups", Certainty::Likely), ("mysticalagricultureplus", Certainty::Possible), ("silentgear", Certainty::Possible)]
        );
        assert!(r[0].why.contains("loop inside DataFixerUpper"), "{}", r[0].why);
        assert!(r[0].why.contains("failed 10 times on the Render thread"), "{}", r[0].why);
        assert!(r[0].why.contains("test version (2.0.0-beta6)"), "{}", r[0].why);
        assert!(r[2].why.contains("trigger, not the cause"), "{}", r[2].why);
    }

    #[test]
    fn errors_long_before_the_crash_or_on_another_thread_do_not_count() {
        let l = Loop { library: "x".into(), thread: "Render thread".into(), time: Some(6 * 3600 + 20 * 60) };
        assert!(initiator(&l, LOG).is_none(), "five minutes later: nothing in the window");
        let l = Loop { thread: "Worker-Main-4".into(), time: Some(6 * 3600 + 14 * 60 + 45), ..l };
        assert_eq!(initiator(&l, LOG).unwrap().initiator, "oldmod");
        // No report time: the log's last entry is the anchor.
        let l = Loop { library: "x".into(), thread: String::new(), time: None };
        assert_eq!(initiator(&l, LOG).unwrap().initiator, "collapsible_groups");
    }

    #[test]
    fn added_notes() {
        let day = 86_400;
        let now = 20_000 * day + 3600;
        assert_eq!(added_note(now - 60, now).as_deref(), Some("You added it today."));
        assert_eq!(added_note(now - day, now).as_deref(), Some("You added it yesterday."));
        assert_eq!(added_note(now - 2 * day, now).as_deref(), Some("You added it 2 days ago."));
        assert_eq!(added_note(now - 30 * day, now), None);
        assert!(is_prerelease("2.0.0-beta6") && is_prerelease("1.0-ALPHA") && !is_prerelease("1.21.1-2.0.2"));
    }
}
