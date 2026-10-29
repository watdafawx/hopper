//! A small RCON client, for reading a server's TPS without typing into its
//! console (where the command and its answer would show up for everyone).
//!
//! The protocol: little-endian packets of `length, id, type, body\0\0`.
//! Type 3 logs in, type 2 runs a command, type 0 is the answer.

use anyhow::{Context as _, bail};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::process::ServerState;

const LOGIN: i32 = 3;
const COMMAND: i32 = 2;

pub struct Rcon {
    s: TcpStream,
    id: i32,
    timeout: Duration,
    closed: Arc<AtomicBool>,
}

/// How often a waiting read checks whether it was cut (see [`Closer`]):
/// Windows doesn't wake a blocked read when the socket is shut down.
const SLICE: Duration = Duration::from_millis(100);

/// Where a server's RCON answers and its password, from server.properties;
/// `None` when RCON is off or has no password.
pub fn settings(root: &Path) -> Option<(u16, String)> {
    let p = super::Properties::load(&root.join("server.properties")).ok()?;
    if p.get("enable-rcon") != Some("true") {
        return None;
    }
    let pass = p.get("rcon.password").unwrap_or("").to_string();
    if pass.is_empty() {
        return None;
    }
    let port = p.get("rcon.port").and_then(|v| v.trim().parse().ok()).unwrap_or(25575);
    Some((port, pass))
}

/// Whether Hopper may talk to a server over RCON: only once it's up
/// ("Done") and before anything starts stopping it. A stopping server shuts
/// its command handlers (spark's among them) down while it saves, and an
/// open RCON connection can keep it from closing at all.
pub fn allowed(state: &ServerState, stopping_seen: bool) -> bool {
    *state == ServerState::Running && !stopping_seen
}

/// Closes an [`Rcon`] connection from another thread, even while a command
/// waits for its answer (which then fails at once).
pub struct Closer(TcpStream, Arc<AtomicBool>);

impl Closer {
    pub fn close(&self) {
        self.1.store(true, Ordering::SeqCst);
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

impl Rcon {
    pub fn closer(&self) -> std::io::Result<Closer> {
        Ok(Closer(self.s.try_clone()?, self.closed.clone()))
    }

    pub fn connect(port: u16, password: &str, timeout: Duration) -> anyhow::Result<Self> {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let s = TcpStream::connect_timeout(&addr, timeout).with_context(|| format!("RCON on port {port}"))?;
        s.set_read_timeout(Some(SLICE.min(timeout)))?;
        s.set_write_timeout(Some(timeout))?;
        let mut r = Rcon { s, id: 0, timeout, closed: Default::default() };
        let id = r.send(LOGIN, password)?;
        // Some servers send an empty answer before the login result.
        loop {
            let (rid, kind, _) = r.recv()?;
            if rid == -1 {
                bail!("RCON refused the password");
            }
            if rid == id && kind == COMMAND {
                return Ok(r);
            }
        }
    }

    fn send(&mut self, kind: i32, body: &str) -> anyhow::Result<i32> {
        if self.closed.load(Ordering::SeqCst) {
            bail!("RCON connection closed");
        }
        self.id = self.id.wrapping_add(1).max(1);
        let mut p = Vec::with_capacity(14 + body.len());
        p.extend_from_slice(&(10 + body.len() as i32).to_le_bytes());
        p.extend_from_slice(&self.id.to_le_bytes());
        p.extend_from_slice(&kind.to_le_bytes());
        p.extend_from_slice(body.as_bytes());
        p.extend_from_slice(&[0, 0]);
        self.s.write_all(&p)?;
        Ok(self.id)
    }

    /// Fills `buf`, in short slices so a [`Closer`] can end the wait.
    fn read_full(&mut self, buf: &mut [u8], deadline: Instant) -> anyhow::Result<()> {
        let mut got = 0;
        while got < buf.len() {
            if self.closed.load(Ordering::SeqCst) {
                bail!("RCON connection closed");
            }
            match self.s.read(&mut buf[got..]) {
                Ok(0) => bail!("RCON connection closed by the server"),
                Ok(n) => got += n,
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => {
                    if Instant::now() >= deadline {
                        bail!("RCON didn't answer in {} s", self.timeout.as_secs());
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    fn recv(&mut self) -> anyhow::Result<(i32, i32, String)> {
        let deadline = Instant::now() + self.timeout;
        let mut len = [0u8; 4];
        self.read_full(&mut len, deadline)?;
        let len = i32::from_le_bytes(len);
        if !(10..=1 << 20).contains(&len) {
            bail!("bad RCON packet");
        }
        let mut buf = vec![0u8; len as usize];
        self.read_full(&mut buf, deadline)?;
        let id = i32::from_le_bytes(buf[0..4].try_into().unwrap());
        let kind = i32::from_le_bytes(buf[4..8].try_into().unwrap());
        let body = String::from_utf8_lossy(&buf[8..buf.len() - 2]).into_owned();
        Ok((id, kind, body))
    }

    /// Runs a command and returns its answer.
    pub fn command(&mut self, cmd: &str) -> anyhow::Result<String> {
        let id = self.send(COMMAND, cmd)?;
        loop {
            let (rid, _, body) = self.recv()?;
            if rid == id {
                return Ok(body);
            }
        }
    }
}

/// The command that reports TPS, by loader ("neoforge", "paper", …) and
/// Minecraft version; `None` when there's no cheap one.
pub fn tps_command(loader: Option<&str>, mc_version: &str, has_spark: bool) -> Option<&'static str> {
    match loader.map(str::to_ascii_lowercase).as_deref() {
        Some("neoforge") => return Some("neoforge tps"),
        Some("forge") => return Some("forge tps"),
        Some("paper" | "purpur" | "spigot" | "folia" | "bukkit") => return Some("tps"),
        _ => {}
    }
    if has_spark {
        return Some("spark tps");
    }
    // Vanilla 1.20.3 added /tick query.
    let mut parts = mc_version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let (major, minor, patch) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    ((major, minor, patch) >= (1, 20, 3)).then_some("tick query")
}

/// Ticks per second and milliseconds per tick, read from a TPS command's
/// answer (colour codes and all).
pub fn parse_tps(text: &str) -> Option<(f32, Option<f32>)> {
    let clean = strip_colors(text);
    let nums = |s: &str| -> Vec<f32> {
        s.split(|c: char| !(c.is_ascii_digit() || c == '.'))
            .filter(|t| !t.is_empty() && t.chars().any(|c| c.is_ascii_digit()))
            .filter_map(|t| t.trim_matches('.').parse().ok())
            .collect()
    };
    // Paper / spark: "TPS from last 1m, 5m, 15m: 20.0, 19.9, 19.8".
    if let Some(i) = clean.find("TPS from last") {
        let after = &clean[i..];
        let colon = after.find(':')?;
        let tps = *nums(&after[colon + 1..]).first()?;
        return Some((tps.min(20.0), None));
    }
    // Vanilla /tick query: "Average time per tick: 1.2ms (Target: 50.0ms)".
    if let Some(i) = clean.find("Average time per tick") {
        let mspt = *nums(&clean[i..]).first()?;
        let target = clean.find("Target tick rate").and_then(|j| nums(&clean[j..]).first().copied()).unwrap_or(20.0);
        let tps = if mspt > 0.0 { (1000.0 / mspt).min(target) } else { target };
        return Some((tps, Some(mspt)));
    }
    // Forge / NeoForge: per-dimension lines and an "Overall" line with
    // "Mean tick time: 0.8 ms. Mean TPS: 20.000" (or "20.000 TPS (0.8 ms/tick)").
    let read = |line: &str| -> Option<(f32, Option<f32>)> {
        let l = line.to_ascii_lowercase();
        let ms = l.find("ms").and_then(|i| nums(&l[..i]).last().copied());
        let tps = if let Some(i) = l.find("mean tps") {
            nums(&l[i..]).first().copied()
        } else {
            l.find("tps").and_then(|i| nums(&l[..i]).last().copied())
        }?;
        Some((tps, ms))
    };
    if let Some(r) = clean.lines().find(|l| l.to_ascii_lowercase().contains("overall")).and_then(read) {
        return Some(r);
    }
    clean.lines().filter_map(read).min_by(|a, b| a.0.total_cmp(&b.0))
}

fn strip_colors(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut skip = false;
    for c in s.chars() {
        if skip {
            skip = false;
        } else if c == '§' {
            skip = true;
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tps_answers() {
        assert_eq!(parse_tps("§6TPS from last 1m, 5m, 15m: §a19.5, §a20.0, §a20.0"), Some((19.5, None)));
        assert_eq!(parse_tps("TPS from last 5s, 10s, 1m, 5m, 15m:\n *20.0, 20.0, 20.0, 20.0, 20.0\n"), Some((20.0, None)));
        let forge = "minecraft:overworld: Mean tick time: 0.612 ms. Mean TPS: 20.000\nminecraft:the_nether: Mean tick time: 0.020 ms. Mean TPS: 20.000\nOverall: Mean tick time: 0.837 ms. Mean TPS: 20.000";
        assert_eq!(parse_tps(forge), Some((20.0, Some(0.837))));
        let neo = "Dim minecraft:overworld: 12.500 TPS (80.000 ms/tick)\nOverall: 12.500 TPS (80.100 ms/tick)";
        assert_eq!(parse_tps(neo), Some((12.5, Some(80.1))));
        let vanilla = "The game is running normally\nTarget tick rate: 20.0 per second.\nAverage time per tick: 100.0ms (Target: 50.0ms)";
        assert_eq!(parse_tps(vanilla), Some((10.0, Some(100.0))));
        assert_eq!(parse_tps("Unknown command"), None);
    }

    #[test]
    fn picks_a_command() {
        assert_eq!(tps_command(Some("NeoForge"), "1.21.1", false), Some("neoforge tps"));
        assert_eq!(tps_command(Some("paper"), "1.21.1", false), Some("tps"));
        assert_eq!(tps_command(None, "1.21.1", false), Some("tick query"));
        assert_eq!(tps_command(Some("fabric"), "1.20.1", false), None);
        assert_eq!(tps_command(Some("fabric"), "1.20.1", true), Some("spark tps"));
    }

    #[test]
    fn rcon_only_while_running() {
        assert!(allowed(&ServerState::Running, false));
        assert!(!allowed(&ServerState::Running, true), "said Stopping server");
        assert!(!allowed(&ServerState::Starting, false), "before Done");
        assert!(!allowed(&ServerState::Stopping, false));
        assert!(!allowed(&ServerState::Stopped, false));
        assert!(!allowed(&ServerState::Crashed(Some(1)), false));
    }

    #[test]
    fn closer_ends_a_waiting_command() {
        // Logs in, then never answers the command.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut b = [0u8; 64];
            let _ = s.read(&mut b).unwrap();
            let id = i32::from_le_bytes(b[4..8].try_into().unwrap());
            let mut p = 10i32.to_le_bytes().to_vec();
            p.extend_from_slice(&id.to_le_bytes());
            p.extend_from_slice(&2i32.to_le_bytes());
            p.extend_from_slice(&[0, 0]);
            s.write_all(&p).unwrap();
            std::thread::sleep(Duration::from_secs(10));
        });
        let mut r = Rcon::connect(port, "pw", Duration::from_secs(8)).unwrap();
        let c = r.closer().unwrap();
        let t = std::time::Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            c.close();
        });
        assert!(r.command("spark tps").is_err());
        assert!(t.elapsed() < Duration::from_secs(3), "the command waited for its timeout");
    }

    #[test]
    fn talks_rcon() {
        // A tiny fake server: accepts the login, answers one command.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let read = |s: &mut TcpStream| {
                let mut len = [0u8; 4];
                s.read_exact(&mut len).unwrap();
                let mut b = vec![0u8; i32::from_le_bytes(len) as usize];
                s.read_exact(&mut b).unwrap();
                (i32::from_le_bytes(b[0..4].try_into().unwrap()), String::from_utf8_lossy(&b[8..b.len() - 2]).into_owned())
            };
            let reply = |s: &mut TcpStream, id: i32, kind: i32, body: &str| {
                let mut p = (10 + body.len() as i32).to_le_bytes().to_vec();
                p.extend_from_slice(&id.to_le_bytes());
                p.extend_from_slice(&kind.to_le_bytes());
                p.extend_from_slice(body.as_bytes());
                p.extend_from_slice(&[0, 0]);
                s.write_all(&p).unwrap();
            };
            let (id, pass) = read(&mut s);
            reply(&mut s, if pass == "pw" { id } else { -1 }, 2, "");
            let (id, cmd) = read(&mut s);
            reply(&mut s, id, 0, &format!("ran {cmd}"));
        });
        let mut r = Rcon::connect(port, "pw", Duration::from_secs(2)).unwrap();
        assert_eq!(r.command("tps").unwrap(), "ran tps");
    }
}
