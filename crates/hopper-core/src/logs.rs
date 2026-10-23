//! Reading game logs and crash reports for display.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Last `max` bytes of a text file.
pub fn tail(path: &Path, max: u64) -> String {
    let Ok(mut f) = std::fs::File::open(path) else {
        return format!("Could not open {}", path.display());
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(max)));
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// The lines of a crash report worth reading first.
pub fn crash_summary(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(l) = lines.next() {
        let t = l.trim();
        if t.starts_with("Description:") {
            out.push(t.to_string());
            // The exception line follows the blank line after Description.
            if let Some(ex) = lines.by_ref().map(str::trim).find(|l| !l.is_empty()) {
                out.push(ex.to_string());
            }
        } else if t.starts_with("Suspected Mod") || t.starts_with("Caused by:") {
            out.push(t.to_string());
            if t.starts_with("Suspected Mod") {
                for next in lines.by_ref().take(4) {
                    if next.trim().is_empty() {
                        break;
                    }
                    out.push(format!("  {}", next.trim()));
                }
            }
        }
        if out.len() >= 10 {
            break;
        }
    }
    out
}

/// How a log line reads at a glance.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Error,
    Warn,
    Plain,
}

pub fn level(line: &str) -> Level {
    if line.contains("ERROR") || line.contains("Exception") {
        Level::Error
    } else if line.contains("WARN") {
        Level::Warn
    } else {
        Level::Plain
    }
}

/// Errors, warnings and stack frames.
pub fn is_problem(line: &str) -> bool {
    level(line) != Level::Plain || line.starts_with("\tat ")
}

/// Crash reports in a game folder, newest first.
pub fn crash_reports(root: &Path) -> Vec<PathBuf> {
    let mut crashes: Vec<PathBuf> = std::fs::read_dir(root.join("crash-reports"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    crashes.sort();
    crashes.reverse();
    crashes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_summary_picks_key_lines() {
        let report = "---- Minecraft Crash Report ----\nTime: x\nDescription: Mod loading error has occurred\n\njava.lang.Exception: Mod Loading has failed\n\tat x\nCaused by: java.lang.NullPointerException\nSuspected Mods:\n\tCreate (create)\n\nother";
        let s = crash_summary(report);
        assert_eq!(s[0], "Description: Mod loading error has occurred");
        assert_eq!(s[1], "java.lang.Exception: Mod Loading has failed");
        assert!(s.iter().any(|l| l.starts_with("Caused by")));
        assert!(s.iter().any(|l| l.contains("Create (create)")));
    }

    #[test]
    fn levels() {
        assert_eq!(level("[12:00] [main/ERROR] boom"), Level::Error);
        assert_eq!(level("[12:00] [main/WARN] hm"), Level::Warn);
        assert!(is_problem("\tat net.minecraft.Foo"));
        assert!(!is_problem("[12:00] [main/INFO] ok"));
    }
}
