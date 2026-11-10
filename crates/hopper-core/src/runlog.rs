//! The log of the start a crash report came from. The game and the server
//! write each start to logs/latest.log and pack the last one away as
//! `logs/2026-11-05-7.log.gz` when the next start begins, so an older
//! report's start is in one of those. A report written by an error that
//! only followed from an earlier one (a "follow-up") says nothing about the
//! cause; that start's log does.
//!
//! The packed file's time is when the next start began, so the start of a
//! report written at T is the log with the earliest time at or after T
//! (latest.log when no start came since). Its first line says when that
//! start began, which must be before T.

use std::io::Read;
use std::path::Path;

/// Bytes read from a log at most (a long start can write a lot).
const MAX_BYTES: u64 = 64 << 20;

/// A start's log.
pub struct RunLog {
    /// "logs/2026-11-05-7.log.gz", or "logs/latest.log".
    pub rel: String,
    /// Its whole text (up to [`MAX_BYTES`]).
    pub text: String,
}

impl RunLog {
    /// Whether it is the current latest.log (not a packed older one).
    pub fn is_latest(&self) -> bool {
        self.rel == "logs/latest.log"
    }
}

fn modified_secs(p: &Path) -> Option<i64> {
    let t = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64)
}

/// `2026-11-05-7.log.gz` or `2026-11-05-7.log`: an older game log (not
/// debug.log's `debug-1.log.gz`).
fn is_old_log(name: &str) -> bool {
    let stem = name.strip_suffix(".log.gz").or_else(|| name.strip_suffix(".log")).unwrap_or("");
    let b = stem.as_bytes();
    b.len() >= 12
        && b[..10].iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() })
        && b[10] == b'-'
        && b[11..].iter().all(u8::is_ascii_digit)
}

/// A log's text, unpacked when it is a `.gz`.
pub fn read_text(p: &Path) -> Option<String> {
    let f = std::fs::File::open(p).ok()?;
    let mut buf = Vec::new();
    if p.extension().is_some_and(|e| e == "gz") {
        flate2::read::GzDecoder::new(f).take(MAX_BYTES).read_to_end(&mut buf).ok()?;
    } else {
        f.take(MAX_BYTES).read_to_end(&mut buf).ok()?;
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// When a log's first line says it was written, for Forge's and NeoForge's
/// `[05Nov2026 07:52:26.119]`. `None` for a line without a date (Fabric
/// and vanilla write the time only).
pub fn first_time(text: &str) -> Option<i64> {
    line_time(text.lines().find(|l| l.starts_with('['))?)
}

/// When one log line says it was written (`[05Nov2026 07:52:26.119] …`).
pub fn line_time(l: &str) -> Option<i64> {
    let s = l.get(1..19)?;
    let day: i64 = s.get(0..2)?.parse().ok()?;
    let mon = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"].iter().position(|m| s.get(2..5) == Some(*m))? as i64 + 1;
    let year: i64 = s.get(5..9)?.parse().ok()?;
    let (h, mi, sec): (i64, i64, i64) = (s.get(10..12)?.parse().ok()?, s.get(13..15)?.parse().ok()?, s.get(16..18)?.parse().ok()?);
    (s.as_bytes().get(9) == Some(&b' ')).then(|| crate::time::local_secs(year, mon, day, h, mi, sec))
}

/// The log of the start that wrote a crash report at `written` (seconds
/// since the epoch), in `root/logs`. `None` when it is gone (deleted, or
/// only the newer starts' logs are kept).
pub fn for_report(root: &Path, written: i64) -> Option<RunLog> {
    let dir = root.join("logs");
    let mut logs: Vec<(i64, String)> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (name == "latest.log" || is_old_log(&name)).then(|| Some((modified_secs(&e.path())?, name)))?
        })
        // The report is written a moment before the log's last lines.
        .filter(|(t, _)| *t + 5 >= written)
        .collect();
    // The earliest one at or after the report; latest.log wins a tie (a
    // start packed away in the same second as its own end can't be).
    logs.sort_by_key(|(t, n)| (*t, n != "latest.log"));
    let (t, name) = logs.into_iter().next()?;
    let text = read_text(&dir.join(&name))?;
    match first_time(&text) {
        // A start that began after the report isn't the report's.
        Some(start) if start > written + 120 => return None,
        Some(_) => {}
        // No date to check: only a log that ended near the report.
        None if t - written > 6 * 3600 => return None,
        None => {}
    }
    Some(RunLog { rel: format!("logs/{name}"), text })
}

/// The log of the start before the one in `rel` ("logs/latest.log" or a
/// packed one): what the game had on last time.
pub fn previous(root: &Path, rel: &str) -> Option<RunLog> {
    let dir = root.join("logs");
    let this = modified_secs(&root.join(rel))?;
    let name = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            is_old_log(&name).then(|| Some((modified_secs(&e.path())?, name)))?
        })
        .filter(|(t, n)| *t < this && format!("logs/{n}") != rel)
        .max_by_key(|(t, _)| *t)?
        .1;
    let text = read_text(&dir.join(&name))?;
    Some(RunLog { rel: format!("logs/{name}"), text })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn set_time(p: &Path, secs: i64) {
        let f = std::fs::File::options().write(true).open(p).unwrap();
        f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64)).unwrap();
    }

    fn line(secs: i64, rest: &str) -> String {
        // The first line in the log's own format, on this PC's clock.
        let local = secs + crate::hosting::schedule::utc_offset_secs();
        let iso = crate::time::iso_from_secs(local);
        let mon = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][iso[5..7].parse::<usize>().unwrap() - 1];
        format!("[{}{mon}{} {}.000] [main/INFO] {rest}\n", &iso[8..10], &iso[0..4], &iso[11..19])
    }

    #[test]
    fn names() {
        assert!(is_old_log("2026-11-05-7.log.gz"));
        assert!(is_old_log("2026-11-05-12.log"));
        assert!(!is_old_log("debug-1.log.gz"));
        assert!(!is_old_log("latest.log"));
        assert!(!is_old_log("2026-11-05.log.gz"));
    }

    #[test]
    fn reads_the_date_of_the_first_line() {
        let t = 1_793_000_000;
        assert_eq!(first_time(&line(t, "start")), Some(t));
        assert_eq!(first_time("[07:52:26] [main/INFO]: start"), None);
    }

    #[test]
    fn a_report_gets_the_log_of_its_own_start() {
        let root = std::env::temp_dir().join(format!("hopper-runlog-{}", std::process::id()));
        let dir = root.join("logs");
        std::fs::create_dir_all(&dir).unwrap();
        // Three starts: 1000 (crashed 1200), 2000 (crashed 2200), 3000 (now).
        let base = 1_793_000_000;
        let write_gz = |name: &str, start: i64, packed: i64, body: &str| {
            let p = dir.join(name);
            let mut z = flate2::write::GzEncoder::new(std::fs::File::create(&p).unwrap(), flate2::Compression::fast());
            z.write_all(format!("{}{body}", line(base + start, "start")).as_bytes()).unwrap();
            z.finish().unwrap();
            set_time(&p, base + packed);
        };
        write_gz("2026-11-05-1.log.gz", 1000, 2000, "first start");
        write_gz("2026-11-05-2.log.gz", 2000, 3000, "second start");
        let latest = dir.join("latest.log");
        std::fs::write(&latest, format!("{}third start", line(base + 3000, "start"))).unwrap();
        set_time(&latest, base + 3100);
        std::fs::write(dir.join("debug-1.log.gz"), b"not this").unwrap();
        set_time(&dir.join("debug-1.log.gz"), base + 1300);

        let l = for_report(&root, base + 1200).unwrap();
        assert_eq!(l.rel, "logs/2026-11-05-1.log.gz");
        assert!(l.text.contains("first start") && !l.is_latest());
        assert_eq!(for_report(&root, base + 2200).unwrap().rel, "logs/2026-11-05-2.log.gz");
        assert!(for_report(&root, base + 3050).unwrap().is_latest());
        // The first start's log is gone: the second start's isn't it.
        std::fs::remove_file(dir.join("2026-11-05-1.log.gz")).unwrap();
        assert!(for_report(&root, base + 1200).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
