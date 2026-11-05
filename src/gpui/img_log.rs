//! A timing log for remote images, for measuring how fast the peek and the
//! image viewer fill in. Off unless `HOPPER_IMG_LOG=<file>` is set: then
//! each event appends `<ms since the first event> <event> <key> [<ms>]` to
//! that file, the last number being how long since the key was wanted.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

struct Log {
    file: std::fs::File,
    boot: Instant,
    wanted: HashMap<String, Instant>,
    once: HashSet<(String, String)>,
}

fn log() -> Option<&'static Mutex<Log>> {
    static L: OnceLock<Option<Mutex<Log>>> = OnceLock::new();
    L.get_or_init(|| {
        let path = std::env::var_os("HOPPER_IMG_LOG")?;
        let file = std::fs::OpenOptions::new().create(true).append(true).open(path).ok()?;
        Some(Mutex::new(Log { file, boot: Instant::now(), wanted: HashMap::new(), once: HashSet::new() }))
    })
    .as_ref()
}

fn write(l: &mut Log, event: &str, key: &str, since: Option<Instant>) {
    let t = l.boot.elapsed().as_secs_f64() * 1000.;
    let dt = since.map(|s| format!(" {:.1}", s.elapsed().as_secs_f64() * 1000.)).unwrap_or_default();
    let _ = writeln!(l.file, "{t:.1} {event} {key}{dt}");
}

/// Whether the log is on (so callers can skip building keys).
pub fn on() -> bool {
    log().is_some()
}

/// `key` was asked for: later events on it say how long it took.
pub fn want(event: &str, key: &str) {
    let Some(l) = log() else { return };
    let mut l = l.lock().unwrap_or_else(|e| e.into_inner());
    l.wanted.insert(key.to_string(), Instant::now());
    write(&mut l, event, key, None);
}

/// Something happened to `key`, timed from when it was wanted.
pub fn mark(event: &str, key: &str) {
    let Some(l) = log() else { return };
    let mut l = l.lock().unwrap_or_else(|e| e.into_inner());
    let since = l.wanted.get(key).copied();
    write(&mut l, event, key, since);
}

/// [`mark`], only the first time for this event and key.
pub fn once(event: &str, key: &str) {
    let Some(l) = log() else { return };
    let mut l = l.lock().unwrap_or_else(|e| e.into_inner());
    if !l.once.insert((event.to_string(), key.to_string())) {
        return;
    }
    let since = l.wanted.get(key).copied();
    write(&mut l, event, key, since);
}
