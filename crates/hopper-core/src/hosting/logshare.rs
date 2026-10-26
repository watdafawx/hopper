//! Getting a server log ready to share publicly (on mclo.gs): the logs and
//! crash reports a server has, read (gzipped ones too), and cleaned of what
//! shouldn't be public: IP addresses, tokens and keys, the server's folder
//! and the Windows user name. Player names stay; they help read a log.

use regex::Regex;
use serde::Serialize;
use std::fs;
use std::io::Read as _;
use std::path::Path;
use std::sync::LazyLock;

/// What was hidden, by kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Hidden {
    pub ips: usize,
    pub tokens: usize,
    pub paths: usize,
    pub users: usize,
}

/// A log or crash report on the server.
#[derive(Clone, Debug, Serialize)]
pub struct LogFile {
    /// Relative to the server folder, with `/`.
    pub path: String,
    pub size: u64,
    /// Unix seconds.
    pub modified: u64,
    pub crash: bool,
}

/// The server's logs (`logs/`) and crash reports, newest first.
pub fn list(root: &Path) -> Vec<LogFile> {
    let mut out = Vec::new();
    for (dir, crash) in [("logs", false), ("crash-reports", true)] {
        let Ok(rd) = fs::read_dir(root.join(dir)) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let l = name.to_ascii_lowercase();
            if !(l.ends_with(".log") || l.ends_with(".log.gz") || l.ends_with(".txt")) {
                continue;
            }
            let Ok(m) = e.metadata() else { continue };
            if !m.is_file() {
                continue;
            }
            let modified = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
            out.push(LogFile { path: format!("{dir}/{name}"), size: m.len(), modified, crash });
        }
    }
    // latest.log first, then newest.
    out.sort_by_key(|f| (!f.path.ends_with("/latest.log"), std::cmp::Reverse(f.modified)));
    out
}

/// Checks `rel` is one of [`list`]'s files and reads it (unzipping .gz).
pub fn read(root: &Path, rel: &str) -> anyhow::Result<String> {
    let rel = rel.replace('\\', "/");
    if !list(root).iter().any(|f| f.path == rel) {
        anyhow::bail!("That isn't one of this server's logs.");
    }
    let bytes = fs::read(root.join(&rel))?;
    let bytes = if rel.ends_with(".gz") {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(&bytes[..]).read_to_end(&mut out)?;
        out
    } else {
        bytes
    };
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

static IPV4: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(25[0-5]|2[0-4]\d|1?\d?\d)\.(25[0-5]|2[0-4]\d|1?\d?\d)\.(25[0-5]|2[0-4]\d|1?\d?\d)\.(25[0-5]|2[0-4]\d|1?\d?\d)\b").unwrap()
});
static IPV6: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(?:[0-9a-f]{1,4}:)+:(?:[0-9a-f]{1,4}(?::[0-9a-f]{1,4})*)?|\b(?:[0-9a-f]{1,4}:){2,7}[0-9a-f]{1,4}\b").unwrap());
static SECRETS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // Discord webhooks carry their secret in the URL.
        r"https?://(?:[\w-]+\.)?discord(?:app)?\.com/api/webhooks/\S+",
        r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{8,}",
        r"(?i)--(?:accessToken|session|clientId|xuid)\s+\S+",
        r#"(?i)\b(?:token|secret|password|passwd|pass|api[_-]?key|apikey|access[_-]?key|rcon\.password|private[_-]?key)\b\s*[=:]\s*["']?[^\s"',;]{3,}"#,
        // JSON web tokens and long hex / base64 secrets after "key".
        r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{5,}",
    ]
    .iter()
    .map(|r| Regex::new(r).unwrap())
    .collect()
});
static WIN_USER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)([A-Z]:[\\/]+Users[\\/]+)[^\\/\r\n"' ]+"#).unwrap());

/// Addresses that say nothing about anyone.
fn harmless_ip(ip: &str) -> bool {
    ip.starts_with("127.") || ip == "0.0.0.0" || ip.starts_with("255.")
}

/// `text` cleaned for sharing, and what was hidden. `root` is the server's
/// folder, written as `<server>` instead.
pub fn redact(text: &str, root: Option<&Path>) -> (String, Hidden) {
    let mut h = Hidden::default();
    let mut s = text.to_string();
    for re in SECRETS.iter() {
        let n = re.find_iter(&s).count();
        if n > 0 {
            h.tokens += n;
            s = re.replace_all(&s, "<hidden>").into_owned();
        }
    }
    if let Some(root) = root {
        let full = root.to_string_lossy().into_owned();
        for form in [full.clone(), full.replace('\\', "/")] {
            // Windows paths ignore case: "E:\Dev" and "e:\dev" are one folder.
            if form.len() > 3
                && let Ok(re) = Regex::new(&format!("(?i){}", regex::escape(&form)))
            {
                let n = re.find_iter(&s).count();
                if n > 0 {
                    h.paths += n;
                    s = re.replace_all(&s, "<server>").into_owned();
                }
            }
        }
    }
    let n = WIN_USER.find_iter(&s).count();
    if n > 0 {
        h.users += n;
        s = WIN_USER.replace_all(&s, "${1}<user>").into_owned();
    }
    s = IPV4
        .replace_all(&s, |c: &regex::Captures| {
            let ip = &c[0];
            if harmless_ip(ip) {
                ip.to_string()
            } else {
                h.ips += 1;
                "<ip>".to_string()
            }
        })
        .into_owned();
    s = IPV6
        .replace_all(&s, |c: &regex::Captures| {
            let ip = &c[0];
            // "12:04:55" is a time, not an address: an address has a letter,
            // a "::" or more than three groups.
            let groups = ip.split(':').filter(|g| !g.is_empty()).count();
            let looks = ip.contains("::") && groups >= 1 || ip.chars().any(|c| c.is_ascii_alphabetic()) || groups > 3;
            if !looks || ip == "::1" || ip == "::" {
                ip.to_string()
            } else {
                h.ips += 1;
                "<ip>".to_string()
            }
        })
        .into_owned();
    (s, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_what_should_not_be_public() {
        let root = Path::new(r"E:\dev\servers\survival");
        let log = concat!(
            "[01:54:24] [Server thread/INFO]: Steve[/192.168.50.20:51234] logged in\n",
            "[01:54:25] [Server thread/INFO]: Starting minecraft server on 0.0.0.0:25565\n",
            "[01:54:25] [main/INFO]: Loading from E:\\Dev\\Servers\\survival\\mods and C:\\Users\\MIcha\\AppData\n",
            "[01:54:26] webhook https://discord.com/api/webhooks/123/abcDEF failed\n",
            "rcon.password=hunter22 token: abcdef123 Authorization: Bearer abc.def-ghi_jkl\n",
            "Joined from 2001:db8:85a3::8a2e:370:7334 at 12:04:55, mod version 1.21.1, 4.1.97.Final\n",
        );
        let (s, h) = redact(log, Some(root));
        assert!(!s.contains("192.168.50.20") && s.contains("0.0.0.0:25565"), "{s}");
        assert!(!s.contains("MIcha") && s.contains(r"C:\Users\<user>"), "{s}");
        assert!(s.contains(r"<server>\mods"), "{s}");
        assert!(!s.contains("abcDEF") && !s.contains("hunter22") && !s.contains("abcdef123") && !s.contains("abc.def"), "{s}");
        assert!(!s.contains("2001:db8"), "{s}");
        assert!(s.contains("12:04:55") && s.contains("[01:54:24]") && s.contains("1.21.1") && s.contains("4.1.97.Final"), "{s}");
        assert!(s.contains("Steve"));
        assert_eq!(h.ips, 2);
        assert_eq!(h.users, 1);
        assert_eq!(h.paths, 1);
        assert!(h.tokens >= 4, "{h:?}");
    }

    #[test]
    fn lists_and_reads_logs() {
        let d = std::env::temp_dir().join(format!("hopper-logshare-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("logs")).unwrap();
        fs::create_dir_all(d.join("crash-reports")).unwrap();
        fs::write(d.join("logs/latest.log"), "hello").unwrap();
        {
            use std::io::Write as _;
            let mut z = flate2::write::GzEncoder::new(fs::File::create(d.join("logs/2026-10-01-1.log.gz")).unwrap(), flate2::Compression::fast());
            z.write_all(b"old log").unwrap();
            z.finish().unwrap();
        }
        fs::write(d.join("crash-reports/crash-x-server.txt"), "boom").unwrap();
        fs::write(d.join("logs/notes.bin"), "x").unwrap();
        let l = list(&d);
        assert_eq!(l.len(), 3);
        assert_eq!(l[0].path, "logs/latest.log");
        assert_eq!(read(&d, "logs/2026-10-01-1.log.gz").unwrap(), "old log");
        assert!(read(&d, "server.properties").is_err());
        assert!(read(&d, "logs/../server.properties").is_err());
        let _ = fs::remove_dir_all(&d);
    }
}
