//! `curseforge://install?addonId=…&fileId=…` links, the ones the "Install"
//! button on curseforge.com opens. When Hopper is set as their handler, a
//! click starts Hopper (or hands the link to the one already running over
//! a localhost socket) and opens that project ready to install.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

const PORT: u16 = 47802;
const SCHEME: &str = "curseforge";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub project_id: u64,
    pub file_id: Option<u64>,
}

pub fn is_link(s: &str) -> bool {
    s.to_ascii_lowercase().starts_with("curseforge://")
}

pub fn parse(url: &str) -> Option<Link> {
    if !is_link(url) {
        return None;
    }
    let query = url.split_once('?')?.1;
    let mut project = None;
    let mut file = None;
    for kv in query.split('&') {
        let (k, v) = kv.split_once('=')?;
        match k.to_ascii_lowercase().as_str() {
            "addonid" | "projectid" => project = v.trim_end_matches('/').parse().ok(),
            "fileid" => file = v.trim_end_matches('/').parse().ok(),
            _ => {}
        }
    }
    Some(Link {
        project_id: project?,
        file_id: file,
    })
}

/// Hands `url` to a running Hopper. False when none is listening.
pub fn forward(url: &str) -> bool {
    let Ok(mut s) =
        TcpStream::connect_timeout(&([127, 0, 0, 1], PORT).into(), Duration::from_millis(400))
    else {
        return false;
    };
    s.write_all(format!("{url}\n").as_bytes()).is_ok()
}

/// Accepts links from later launches, on localhost only.
pub fn listen(on_link: impl Fn(String) + Send + 'static) {
    let Ok(l) = TcpListener::bind(("127.0.0.1", PORT)) else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("links".into())
        .spawn(move || {
            for s in l.incoming().flatten() {
                let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
                let mut line = String::new();
                if BufReader::new(s).read_line(&mut line).is_ok() && is_link(line.trim()) {
                    on_link(line.trim().to_string());
                }
            }
        });
}

// ── the per-user URL handler ────────────────────────────────────────

const KEY: &str = r"HKCU\Software\Classes\curseforge";

fn reg(args: &[&str]) -> Option<String> {
    let mut cmd = std::process::Command::new("reg");
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The command that opens `curseforge://` links now, if any.
pub fn current_handler() -> Option<String> {
    let out = reg(&["query", &format!(r"{KEY}\shell\open\command"), "/ve"])?;
    out.lines()
        .find_map(|l| l.split_once("REG_SZ").map(|(_, v)| v.trim().to_string()))
}

pub fn ours() -> bool {
    let exe = std::env::current_exe().unwrap_or_default();
    current_handler().is_some_and(|h| {
        h.to_lowercase()
            .contains(&exe.to_string_lossy().to_lowercase())
    })
}

/// Makes Hopper open `curseforge://` links. Returns the previous handler
/// so it can be put back.
pub fn take_over() -> anyhow::Result<Option<String>> {
    let prev = current_handler().filter(|_| !ours());
    let exe = std::env::current_exe()?;
    let cmd = format!("\"{}\" \"%1\"", exe.display());
    for (sub, name, val) in [
        ("", "/ve", "URL:CurseForge"),
        ("", "/v", ""),
        (r"\shell\open\command", "/ve", cmd.as_str()),
    ] {
        let key = format!("{KEY}{sub}");
        let ok = if name == "/v" {
            reg(&["add", &key, "/v", "URL Protocol", "/d", val, "/f"])
        } else {
            reg(&["add", &key, "/ve", "/d", val, "/f"])
        };
        anyhow::ensure!(ok.is_some(), "couldn't write {key}");
    }
    let _ = SCHEME;
    Ok(prev)
}

/// Gives `curseforge://` links back to `prev` (the CurseForge app).
pub fn give_back(prev: Option<&str>) -> anyhow::Result<()> {
    match prev {
        Some(p) => {
            anyhow::ensure!(
                reg(&[
                    "add",
                    &format!(r"{KEY}\shell\open\command"),
                    "/ve",
                    "/d",
                    p,
                    "/f"
                ])
                .is_some(),
                "couldn't restore the previous handler"
            );
        }
        None => {
            reg(&["delete", KEY, "/f"]);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_install_links() {
        assert_eq!(
            parse("curseforge://install?addonId=238222&fileId=5101366"),
            Some(Link {
                project_id: 238222,
                file_id: Some(5101366)
            })
        );
        assert_eq!(
            parse("curseforge://install?addonId=925200&fileId=7000000&gameId=432/"),
            Some(Link {
                project_id: 925200,
                file_id: Some(7000000)
            })
        );
        assert_eq!(
            parse("curseforge://install?addonId=1").unwrap().file_id,
            None
        );
        assert_eq!(parse("https://example.com"), None);
    }
}
