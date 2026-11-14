//! Hopper's own log, kept in a file so a player can send it with a bug
//! report. The release exe has no console, so stderr alone is lost.
//!
//! The file is `logs/hopper.log` in the data folder. Each start rotates the
//! older ones (`hopper.1.log` ... `hopper.4.log`, [`KEEP`] runs in all), and
//! one run stops writing at [`MAX_BYTES`] so a log loop can't fill the disk.
//! Every line goes through [`redact`] first: the log is meant to be passed
//! on, so keys, tokens, share codes and the Windows user name never reach it.

use regex::{Regex, RegexSet};
use std::borrow::Cow;
use std::fs::{self, File};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// The current run's log.
pub const FILE: &str = "hopper.log";

/// Runs kept: this one and the four before it.
pub const KEEP: usize = 5;

/// One run's file stops growing here.
pub const MAX_BYTES: u64 = 10 << 20;

/// "Copy log" copies the whole file up to this size, else the last
/// [`COPY_LINES`] lines.
pub const COPY_BYTES: u64 = 512 << 10;
pub const COPY_LINES: usize = 2000;

/// The `logs` folder in the data folder.
pub fn dir() -> Option<PathBuf> {
    crate::paths::dirs().map(|d| d.data_dir().join("logs"))
}

/// The current run's log file.
pub fn path() -> Option<PathBuf> {
    dir().map(|d| d.join(FILE))
}

/// `hopper.log` for run 0, `hopper.2.log` for the run before the last.
fn name_of(n: usize) -> String {
    if n == 0 { FILE.to_string() } else { format!("hopper.{n}.log") }
}

/// Moves the older logs up one place, dropping the oldest, so
/// `hopper.log` is free for this run. `keep` counts this run too.
pub fn rotate(dir: &Path, keep: usize) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let keep = keep.max(1);
    // Anything past the last kept slot goes (a lower `keep` than before).
    for n in keep..keep + 10 {
        let _ = fs::remove_file(dir.join(name_of(n)));
    }
    for n in (0..keep - 1).rev() {
        let from = dir.join(name_of(n));
        if from.exists() {
            let to = dir.join(name_of(n + 1));
            let _ = fs::remove_file(&to);
            fs::rename(&from, &to)?;
        }
    }
    Ok(())
}

/// The file the log writes to: stops after `max` bytes with one last line
/// saying so.
pub struct Sink {
    file: File,
    written: u64,
    max: u64,
    capped: bool,
}

impl Sink {
    /// Rotates `dir`'s logs, then starts this run's file.
    pub fn start(dir: &Path, keep: usize, max: u64) -> io::Result<Self> {
        rotate(dir, keep)?;
        let file = fs::OpenOptions::new().create(true).write(true).truncate(true).open(dir.join(FILE))?;
        Ok(Self { file, written: 0, max, capped: false })
    }
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.capped {
            return Ok(buf.len());
        }
        if self.written + buf.len() as u64 > self.max {
            self.capped = true;
            let note = format!("[log full at {} MB: the rest of this run isn't saved here]\n", self.max >> 20);
            let _ = self.file.write_all(note.as_bytes());
            return Ok(buf.len());
        }
        // A full disk must not turn into a failing logger.
        if self.file.write_all(buf).is_ok() {
            self.written += buf.len() as u64;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Where the logger's lines go before and after the file is chosen.
enum Stage {
    /// Not yet: lines wait here (a helper window or a forwarded link never
    /// gets a file, so it can't rotate away the running Hopper's log).
    Waiting(Vec<u8>),
    File(Sink),
    /// The file couldn't be made; stderr only.
    Off,
}

static STAGE: Mutex<Stage> = Mutex::new(Stage::Waiting(Vec::new()));

/// Lines held while waiting for the file; more than this are dropped.
const WAIT_BYTES: usize = 64 << 10;

/// The logger's output: stderr always (a dev run's console; the release
/// exe has none, which is fine), and the log file once [`attach`] ran.
pub struct Pipe;

impl Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = io::stderr().write_all(buf);
        match &mut *STAGE.lock().unwrap_or_else(|e| e.into_inner()) {
            Stage::Waiting(held) if held.len() + buf.len() <= WAIT_BYTES => held.extend_from_slice(buf),
            Stage::File(sink) => {
                let _ = sink.write(buf);
            }
            _ => {}
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Starts this run's log file (older runs move up one place) and writes the
/// opening line, then what was logged while waiting. Call once, from the
/// real app start; not from the helper windows.
pub fn attach(version: &str) {
    let Some(dir) = dir() else { return };
    let mut stage = STAGE.lock().unwrap_or_else(|e| e.into_inner());
    let Stage::Waiting(held) = &mut *stage else { return };
    let held = std::mem::take(held);
    *stage = match Sink::start(&dir, KEEP, MAX_BYTES) {
        Ok(mut sink) => {
            let data = crate::paths::dirs().map(|d| d.data_dir().to_path_buf()).unwrap_or_default();
            let first = line(&crate::time::log_stamp(), log::Level::Info, "hopper", &start_line(version, &data));
            let _ = sink.write(first.as_bytes());
            let _ = sink.write(&held);
            Stage::File(sink)
        }
        Err(e) => {
            eprintln!("Hopper's log file: {e}");
            Stage::Off
        }
    };
}

/// What never belongs in a log that gets sent around. Each pattern keeps
/// the name and hides the value.
static SECRETS: LazyLock<(RegexSet, Vec<(Regex, &'static str)>)> = LazyLock::new(|| {
    let rules: &[(&str, &str)] = &[
        // Discord webhooks carry their secret in the URL.
        (r"https?://(?:[\w-]+\.)?discord(?:app)?\.com/api/webhooks/\S+", "<hidden>"),
        (r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{8,}", "Bearer <hidden>"),
        // A header written out (`x-api-key: ...`).
        (r#"(?i)\b(x-api-key|authorization|cookie|set-cookie)\b(\s*[=:]\s*)["']?[^\s"',;]+(?:\s+[A-Za-z0-9._~+/=-]{8,})?"#, "${1}${2}<hidden>"),
        // `token: ...`, `mailbox_token=...`: any name that says what it holds.
        (r#"(?i)([\w.-]*(?:token|secret|password|passwd|api[_-]?key|apikey|access[_-]?key|private[_-]?key)[\w.-]*)(\s*[=:]\s*)["']?[^\s"',;&]{3,}"#, "${1}${2}<hidden>"),
        // `?key=...&code=...` in a URL (a bare word, so only with `=`).
        (r#"(?i)\b(key|code|pass|sig|signature)(=)["']?[^\s"',;&]{3,}"#, "${1}${2}<hidden>"),
        // "invite code: ...", "setup code ...".
        (r#"(?i)\b((?:invite|share|setup|join|pairing)[ _-]?(?:code|key|link))(\s*[=:]\s*)["']?[^\s"',;&]{3,}"#, "${1}${2}<hidden>"),
        // JSON web tokens.
        (r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{5,}", "<hidden>"),
        // CurseForge's key is a bcrypt-style string; GitHub's start with gh*_.
        (r"\$2[aby]\$\d\d\$[./A-Za-z0-9]{20,}", "<hidden>"),
        (r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})", "<hidden>"),
        // A share code (`HOP-` + base64).
        (r"\bHOP-[A-Za-z0-9_-]{12,}", "HOP-<hidden>"),
        // The Windows user name in a folder path.
        (r#"(?i)([A-Z]:[\\/]+Users[\\/]+)[^\\/\r\n"':]+"#, "${1}<user>"),
    ];
    let set = RegexSet::new(rules.iter().map(|r| r.0)).expect("log redaction patterns");
    let each = rules.iter().map(|(p, to)| (Regex::new(p).expect("log redaction pattern"), *to)).collect();
    (set, each)
});

/// `text` with secrets and the Windows user name hidden. Cheap for the
/// usual line: one pass of the set says nothing matched.
pub fn redact(text: &str) -> Cow<'_, str> {
    let (set, each) = &*SECRETS;
    let hits = set.matches(text);
    if !hits.matched_any() {
        return Cow::Borrowed(text);
    }
    let mut s = text.to_string();
    for i in hits.iter() {
        let (re, to) = &each[i];
        s = re.replace_all(&s, *to).into_owned();
    }
    Cow::Owned(s)
}

/// One log line: local time, level, where it came from, the message.
/// Multi-line messages keep every line, indented under the first.
pub fn line(stamp: &str, level: log::Level, target: &str, message: &str) -> String {
    let message = redact(message);
    // Some libraries log with no target at all.
    let mut out = if target.is_empty() { format!("{stamp} {:<5} ", level.as_str()) } else { format!("{stamp} {:<5} {target}: ", level.as_str()) };
    for (i, l) in message.trim_end().lines().enumerate() {
        if i > 0 {
            out.push_str("\n    ");
        }
        out.push_str(l);
    }
    out.push('\n');
    out
}

/// The line that opens a run: nothing private, just what a bug report needs.
pub fn start_line(version: &str, data_dir: &Path) -> String {
    format!(
        "Hopper {version} started on {} ({}), data folder {}",
        windows_version(),
        std::env::consts::ARCH,
        data_dir.display()
    )
}

/// "Windows 10.0.26300 (25H2)", from the registry; the OS name elsewhere.
pub fn windows_version() -> String {
    #[cfg(windows)]
    {
        use windows_registry::LOCAL_MACHINE;
        if let Ok(k) = LOCAL_MACHINE.open(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion") {
            let get = |name: &str| k.get_string(name).unwrap_or_default();
            let (major, minor, build) = (k.get_u32("CurrentMajorVersionNumber").unwrap_or(10), k.get_u32("CurrentMinorVersionNumber").unwrap_or(0), get("CurrentBuildNumber"));
            let edition = get("DisplayVersion");
            let mut s = format!("Windows {major}.{minor}.{build}");
            if !edition.is_empty() {
                s.push_str(&format!(" ({edition})"));
            }
            return s;
        }
    }
    std::env::consts::OS.to_string()
}

/// The log's text for "Copy log": the whole file when it's small, else the
/// last `lines` lines (starting on a whole line).
pub fn tail_text(path: &Path, max_bytes: u64, lines: usize) -> io::Result<String> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    if len <= max_bytes {
        let mut b = Vec::new();
        f.read_to_end(&mut b)?;
        return Ok(String::from_utf8_lossy(&b).into_owned());
    }
    // Read from the end, enough bytes to hold `lines` lines (a line is
    // rarely over a few hundred bytes), and cut to the last `lines`.
    let take = (lines as u64 * 400).min(len);
    f.seek(SeekFrom::Start(len - take))?;
    let mut b = Vec::new();
    f.read_to_end(&mut b)?;
    let text = String::from_utf8_lossy(&b);
    let all: Vec<&str> = text.lines().collect();
    // The first line may be a cut one: drop it unless the read began at the top.
    let skip_cut = usize::from(take < len);
    let body = &all[skip_cut.min(all.len())..];
    let from = body.len().saturating_sub(lines);
    Ok(format!("[last {} lines of the log]\n{}\n", body.len() - from, body[from..].join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-applog-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn read(d: &Path, n: usize) -> Option<String> {
        fs::read_to_string(d.join(name_of(n))).ok()
    }

    #[test]
    fn keeps_five_runs_and_drops_the_oldest() {
        let d = scratch("rotate");
        for run in 1..=7 {
            let mut s = Sink::start(&d, KEEP, MAX_BYTES).unwrap();
            writeln!(s, "run {run}").unwrap();
        }
        // Newest first: run 7 is hopper.log, run 3 is hopper.4.log.
        for (n, run) in (0..5).zip((3..=7).rev()) {
            assert_eq!(read(&d, n).as_deref(), Some(format!("run {run}\n").as_str()), "slot {n}");
        }
        assert!(read(&d, 5).is_none(), "a sixth file is kept");
        assert_eq!(fs::read_dir(&d).unwrap().count(), 5);
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn a_smaller_keep_trims_old_files() {
        let d = scratch("trim");
        for run in 1..=5 {
            let mut s = Sink::start(&d, 5, MAX_BYTES).unwrap();
            writeln!(s, "run {run}").unwrap();
        }
        Sink::start(&d, 2, MAX_BYTES).unwrap();
        assert_eq!(fs::read_dir(&d).unwrap().count(), 2);
        assert_eq!(read(&d, 1).as_deref(), Some("run 5\n"));
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn stops_at_the_size_cap() {
        let d = scratch("cap");
        let mut s = Sink::start(&d, KEEP, 100).unwrap();
        for _ in 0..50 {
            s.write_all(b"0123456789\n").unwrap();
        }
        let size = fs::metadata(d.join(FILE)).unwrap().len();
        let text = read(&d, 0).unwrap();
        assert!(text.contains("log full"), "{text}");
        assert!(size < 100 + 120, "{size} bytes after 550 written");
        // Later writes still succeed and add nothing.
        s.write_all(b"more\n").unwrap();
        assert_eq!(fs::metadata(d.join(FILE)).unwrap().len(), size);
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn hides_keys_tokens_codes_and_the_user_name() {
        let cases = [
            ("GET https://api.example.com/v1?key=abc123def456&page=2", "abc123def456"),
            ("sent x-api-key: $2a$10$abcdefghijklmnopqrstuuABCDEFGHIJKLMNOPQRSTUVWXYZ012", "abcdefghijklmnopqrstu"),
            ("header Authorization: Bearer abcdef.ghijkl-mnop", "abcdef.ghijkl"),
            ("token=ghp_abcdefghijklmnopqrstuvwxyz0123456789", "ghp_abcdefgh"),
            ("using ghp_abcdefghijklmnopqrstuvwxyz0123456789 now", "ghp_abcdefgh"),
            ("mailbox_token: s3cr3tvalue", "s3cr3tvalue"),
            ("password = \"hunter22\"", "hunter22"),
            ("share HOP-AbCdEf0123456789xyz_-", "AbCdEf0123456789"),
            ("invite code: ABCD-EFGH-1234", "ABCD-EFGH"),
            ("setup code=ZXCV9876", "ZXCV9876"),
            ("jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcDEF123", "eyJhbGci"),
            ("https://discord.com/api/webhooks/123/secretpart", "secretpart"),
            (r"reading C:\Users\Alice Smith\AppData\x.json", "Alice"),
            ("reading c:/users/Bob/x", "Bob"),
        ];
        for (input, secret) in cases {
            let out = redact(input);
            assert!(!out.contains(secret), "{input:?} -> {out:?}");
            assert!(out.contains("<hidden>") || out.contains("<user>"), "{input:?} -> {out:?}");
        }
        // The name stays, so the line still reads.
        assert!(redact("key=abc123def456").starts_with("key="));
    }

    #[test]
    fn leaves_plain_lines_alone() {
        for l in [
            "installed 12 mods in 3.4 s",
            "download window for https://example.com/pack.zip: Some(0)",
            "keycode handled, passed 3 checks",
            r"data folder X:\Dev\Hopper Data",
        ] {
            assert!(matches!(redact(l), Cow::Borrowed(_)), "{l}");
        }
    }

    #[test]
    fn a_line_has_time_level_and_target_and_is_redacted() {
        let l = line("2026-11-14 09:00:00.123", log::Level::Warn, "hopper_core::cf", "retry ?token=zzzzzz\nsecond");
        assert_eq!(l, "2026-11-14 09:00:00.123 WARN  hopper_core::cf: retry ?token=<hidden>\n    second\n");
    }

    #[test]
    fn a_line_without_a_target_has_no_colon_gap() {
        assert_eq!(line("T", log::Level::Error, "", "boom"), "T ERROR boom\n");
    }

    #[test]
    fn the_start_line_names_version_and_folder() {
        let s = start_line("0.65.0", Path::new("X:/Hopper Data"));
        assert!(s.starts_with("Hopper 0.65.0 started on "), "{s}");
        assert!(s.contains("Hopper Data"), "{s}");
    }

    #[test]
    fn copy_takes_the_whole_small_file_and_the_end_of_a_big_one() {
        let d = scratch("tail");
        let p = d.join(FILE);
        fs::write(&p, "a\nb\nc\n").unwrap();
        assert_eq!(tail_text(&p, 1000, 2).unwrap(), "a\nb\nc\n");
        let big: String = (0..5000).map(|i| format!("line {i}\n")).collect();
        fs::write(&p, &big).unwrap();
        let t = tail_text(&p, 1000, 2000).unwrap();
        assert!(t.starts_with("[last 2000 lines of the log]\nline 3000\n"), "{}", &t[..60]);
        assert!(t.ends_with("line 4999\n"));
        let _ = fs::remove_dir_all(d);
    }
}
