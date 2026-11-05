//! A stuck start's saved thread dump (`crash-reports/stuck-<time>.txt`,
//! written by the stall watch) read back for Logs & crashes. Its header
//! says in words what Hopper found while the game or server hung; it is
//! read into a [`Crash`] marked `stuck`, so the list and the crash card
//! show it with the same rows (mod, how sure, why) as a crash.

use crate::crashdetect::{Certainty, Crash, Suspect};
use std::path::{Path, PathBuf};

/// The file name a saved dump starts with.
pub const PREFIX: &str = "stuck-";
const HEAD: &str = "---- Hopper: stuck start ----";

/// Whether a file in `crash-reports` is a stuck start's dump, not a crash.
pub fn is_stuck(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(PREFIX))
}

/// The stuck starts' dumps of `root`.
pub fn dumps(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root.join("crash-reports")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| is_stuck(p)).collect()
}

/// A dump's header read as a crash: the headline, the mods with how sure
/// and why, and the rest (what waits on what, the last log line) as advice.
pub fn read(path: &Path, server: bool) -> Option<Crash> {
    use std::io::Read;
    // The header is at the top; the dump under it can be long.
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.take(64 << 10).read_to_end(&mut buf).ok()?;
    parse(&String::from_utf8_lossy(&buf), path, server)
}

fn parse(text: &str, path: &Path, server: bool) -> Option<Crash> {
    let mut lines = text.lines().skip_while(|l| l.trim() != HEAD).skip(1).take_while(|l| !l.trim().is_empty());
    let headline = lines.next()?.trim().to_string();
    let mut suspects = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    for l in lines {
        let (tag, rest) = l.split_once(": ").unwrap_or(("", l));
        let found = match tag {
            "Certain" => Some((Certainty::Certain, false)),
            "Likely" => Some((Certainty::Likely, false)),
            "Possible" => Some((Certainty::Possible, false)),
            "waiting (not the cause)" => Some((Certainty::Possible, true)),
            _ => None,
        };
        // "Likely: some-mod-1.0.jar (why)"
        match found.and_then(|f| Some((f, rest.split_once(" (")?))) {
            Some(((certainty, knock_on), (file, why))) => suspects.push(Suspect {
                file_name: file.trim().to_string(),
                certainty,
                knock_on,
                why: why.strip_suffix(')').unwrap_or(why).to_string(),
                ..Default::default()
            }),
            None => notes.push(l.trim().to_string()),
        }
    }
    let when = std::fs::metadata(path).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    Some(Crash { when, source: path.to_path_buf(), headline, suspects, server, advice: notes.join(" "), stuck: true, ..Default::default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_header_as_a_crash() {
        let text = "---- Hopper: stuck start ----\nTNP-LL8 looks stuck while loading mods (no new log line for 4 minutes: 1.00 of a core).\nThe main thread is busy in one mod.\nLikely: create-1.21.1-6.0.jar (its code is running on the main thread)\nwaiting (not the cause): jei-19.jar (waits for the main thread)\nLast log line: [main/INFO] Loading\n\n2026-11-05 17:33:21\nFull thread dump";
        let c = parse(text, Path::new("."), true).unwrap();
        assert!(c.stuck && c.server);
        assert!(c.headline.starts_with("TNP-LL8 looks stuck"));
        assert_eq!(c.suspects.len(), 2);
        assert_eq!((c.suspects[0].file_name.as_str(), c.suspects[0].certainty), ("create-1.21.1-6.0.jar", Certainty::Likely));
        assert_eq!(c.suspects[0].why, "its code is running on the main thread");
        assert!(c.suspects[1].knock_on);
        assert!(c.advice.contains("busy in one mod") && c.advice.contains("Last log line"));
        assert!(parse("Full thread dump", Path::new("."), true).is_none());
        assert!(is_stuck(Path::new("crash-reports/stuck-2026-11-05_17.33.21.txt")));
    }
}
