//! A crash log for Hopper's own panics.
//!
//! A panic used to leave only a line on stderr, which a player who started
//! Hopper by double-clicking never sees. This hook appends the panic and a
//! backtrace to `crash.log` in the app's data folder, so a report can be
//! passed on as it is.

use std::io::Write as _;
use std::path::Path;

/// The log's name in the data folder.
pub const FILE: &str = "crash.log";

/// Past this size the oldest entries go first, so a crash loop can't fill
/// the disk. `None` turns that off (the tests).
const MAX_BYTES: u64 = 1 << 20;

/// Writes Hopper's panics to `crash.log` in `dir`, then lets the default
/// hook print them as before. Call once, before the app starts any threads.
pub fn install(app: &str, version: &str, dir: &Path) {
    // The hook lives for the whole process: it can't borrow from the caller.
    let (app, version) = (app.to_string(), version.to_string());
    let log = dir.join(FILE);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let location = info.location().map_or_else(|| "unknown".to_string(), |l| {
            format!("{}:{}:{}", l.file(), l.line(), l.column())
        });
        let message = info.payload_as_str().unwrap_or("panicked without a message");
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        let text = entry(&app, &version, &crate::time::now_iso(), thread.name().unwrap_or("unnamed"), &location, message, &backtrace);
        // A crash log is a nicety: never let a full disk or a locked file
        // stop the panic from reaching stderr and the process from exiting.
        let _ = append(&log, &text, Some(MAX_BYTES));
        previous(info);
    }));
}

/// One panic, in the log's format.
fn entry(app: &str, version: &str, when: &str, thread: &str, location: &str, message: &str, backtrace: &str) -> String {
    format!(
        "=== {when} | {app} {version} | thread {thread} ===\n\
         panicked at {location}:\n\
         {message}\n\
         stack backtrace:\n\
         {}\n\n",
        backtrace.trim_end()
    )
}

/// Appends `text` to `path`, first dropping the oldest whole entries when
/// that keeps the file under `max` bytes. An entry bigger than `max` on its
/// own is still written whole: the newest panic is the one worth keeping.
fn append(path: &Path, text: &str, max: Option<u64>) -> std::io::Result<()> {
    // Two threads can panic at once; without this their writes interleave.
    static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut old = std::fs::read(path).unwrap_or_default();
    if let Some(max) = max {
        let budget = max.saturating_sub(text.len() as u64) as usize;
        if old.len() > budget {
            // Entries end with a blank line: begin again after the one the
            // cut landed in, so the log holds whole entries only.
            let from = old.len() - budget;
            let start = old[from..].windows(2).position(|w| w == b"\n\n").map_or(old.len(), |i| from + i + 2);
            old.drain(..start);
        }
    }
    old.extend_from_slice(text.as_bytes());
    let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(true).open(path)?;
    f.write_all(&old)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_names_the_app_place_and_stack() {
        let text = entry(
            "Hopper",
            "0.60.4",
            "2026-11-10T12:34:56.000Z",
            "Render",
            "src/gpui/main.rs:194:37",
            "open the main window",
            "   0: hopper::main\n   1: std::thread::spawn\n",
        );
        assert!(text.starts_with("=== 2026-11-10T12:34:56.000Z | Hopper 0.60.4 | thread Render ===\n"), "{text}");
        assert!(text.contains("panicked at src/gpui/main.rs:194:37:\nopen the main window\n"), "{text}");
        assert!(text.contains("stack backtrace:\n   0: hopper::main\n   1: std::thread::spawn\n\n"), "{text}");
        assert!(text.ends_with("\n\n"), "{text}");
    }

    #[test]
    fn append_keeps_the_newest_entries_and_whole_ones() {
        let dir = std::env::temp_dir().join(format!("hopper-panic-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(FILE);
        // Room for about two short entries.
        let first = "=== one ===\noldest\n\n";
        let second = "=== two ===\nnewer\n\n";
        let third = "=== three ===\nnewest\n\n";
        append(&path, first, Some(46)).unwrap();
        append(&path, second, Some(46)).unwrap();
        append(&path, third, Some(46)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("one"));
        assert!(text.contains("two"), "{text}");
        assert!(text.ends_with(third), "{text}");
        // An entry over the limit on its own is still written whole.
        append(&path, third, Some(2)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), third);
        // No limit: everything stays.
        append(&path, first, None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("{third}{first}"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
