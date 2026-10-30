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

/// The command that reports TPS, by loader ("neoforge 21.1.259", "paper",
/// …) and Minecraft version; `None` when there's no cheap one.
///
/// spark comes last: it answers on its own thread after the command
/// returns, so over RCON its reply is empty or only its first line.
pub fn tps_command(loader: Option<&str>, mc_version: &str, has_spark: bool) -> Option<&'static str> {
    // Loaders come with their version ("forge 1.20.1-47.4.10",
    // "neoforge-21.1.259"): the name is the leading letters.
    let family = loader.map(|l| l.trim().chars().take_while(|c| c.is_ascii_alphabetic()).collect::<String>().to_ascii_lowercase());
    match family.as_deref() {
        Some("neoforge") => return Some("neoforge tps"),
        Some("forge") => return Some("forge tps"),
        Some("paper" | "purpur" | "spigot" | "folia" | "bukkit") => return Some("tps"),
        _ => {}
    }
    // Vanilla 1.20.3 added /tick query.
    let mut parts = mc_version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let (major, minor, patch) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    if (major, minor, patch) >= (1, 20, 3) {
        return Some("tick query");
    }
    has_spark.then_some("spark tps")
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
    // Paper: "TPS from last 1m, 5m, 15m: 20.0, 19.9, 19.8"; spark puts the
    // numbers on the next line. Only that line counts: a spark reply cut
    // short over RCON can run into another command's answer ("There are 0
    // of a max of 16 players"), which isn't a TPS of 0.
    if let Some(i) = clean.find("TPS from last") {
        let after = &clean[i..];
        let colon = after.find(':')?;
        let line = after[colon + 1..].lines().find(|l| !l.trim().is_empty())?;
        let n = nums(line);
        if n.len() < 3 || !line.contains(',') {
            return None;
        }
        return Some((n[0].min(20.0), None));
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

/// Where RCON ports are picked from: Minecraft's default and up.
const RCON_PORTS: std::ops::Range<u16> = 25575..26575;

/// What [`enable`] did to a server's RCON settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setup {
    /// It was on with a password already; nothing changed.
    Already { port: u16 },
    /// The owner's password was kept; RCON was switched on (and given a free
    /// port if its own clashed with another server).
    Adopted { port: u16 },
    /// Turned on with a new port and password.
    Enabled { port: u16 },
}

impl Setup {
    pub fn port(self) -> u16 {
        match self {
            Setup::Already { port } | Setup::Adopted { port } | Setup::Enabled { port } => port,
        }
    }

    /// Whether server.properties changed (and needs saving).
    pub fn changed(self) -> bool {
        !matches!(self, Setup::Already { .. })
    }
}

/// Turns RCON on in `p` so the panel can read TPS. An RCON setup the owner
/// made is kept: a password stays, and so does its port unless another
/// server uses it. `taken` holds the ports other servers use (game and
/// RCON); `free` says whether a port can be bound on this PC right now;
/// `password` makes a new one when there is none.
pub fn enable(p: &mut super::Properties, taken: &[u16], free: impl Fn(u16) -> bool, password: impl FnOnce() -> String) -> anyhow::Result<Setup> {
    let pass = p.get("rcon.password").map(str::trim).unwrap_or("").to_string();
    let own = p.get("rcon.port").and_then(|v| v.trim().parse::<u16>().ok());
    if p.get("enable-rcon") == Some("true") && !pass.is_empty() {
        return Ok(Setup::Already { port: own.unwrap_or(RCON_PORTS.start) });
    }
    let game = p.port();
    let clash = |n: u16| n == game || taken.contains(&n);
    let port = match own {
        Some(n) if !pass.is_empty() && !clash(n) => n,
        _ => RCON_PORTS.clone().find(|n| !clash(*n) && free(*n)).ok_or_else(|| anyhow::anyhow!("No free port for RCON."))?,
    };
    let kept = !pass.is_empty();
    p.set("enable-rcon", "true");
    p.set("rcon.port", port.to_string());
    if !kept {
        p.set("rcon.password", password());
    }
    // Commands the panel runs shouldn't show up in ops' chat.
    p.set_default("broadcast-rcon-to-ops", "false");
    Ok(if kept { Setup::Adopted { port } } else { Setup::Enabled { port } })
}

/// The ports a set of servers use, for the game and for RCON (on or off:
/// an owner may switch theirs back on).
pub fn ports_in_use(roots: &[std::path::PathBuf]) -> Vec<u16> {
    let mut out = Vec::new();
    for r in roots {
        if let Ok(p) = super::Properties::load(&r.join("server.properties")) {
            out.push(p.port());
            if let Some(n) = p.get("rcon.port").and_then(|v| v.trim().parse().ok()) {
                out.push(n);
            }
        }
    }
    out
}

/// [`enable`] on the server in `root`, saving its server.properties when it
/// changed. `others` are the other servers' folders.
pub fn enable_in(root: &Path, others: &[std::path::PathBuf], password: impl FnOnce() -> String) -> anyhow::Result<Setup> {
    let path = root.join("server.properties");
    let mut p = super::Properties::load(&path)?;
    let taken = ports_in_use(others);
    let s = enable(&mut p, &taken, |n| std::net::TcpListener::bind(("127.0.0.1", n)).is_ok(), password)?;
    if s.changed() {
        p.save(&path)?;
    }
    Ok(s)
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

    /// Real replies from TNP-LL8 (NeoForge 1.21.1, spark 1.10.124) over RCON.
    #[test]
    fn parses_real_replies() {
        // `neoforge tps`, trimmed to a few of its 52 dimensions.
        let neo = "Overworld: 20.000 TPS (0.884 ms/tick)
Meteorite Cluster: 20.000 TPS (0.293 ms/tick)
sgjourney:cavum_tenebrae: 20.000 TPS (0.223 ms/tick)
The Nether: 20.000 TPS (0.186 ms/tick)
AE2 Spatial Storage: 20.000 TPS (0.181 ms/tick)
Overall: 20.000 TPS (10.574 ms/tick)
";
        assert_eq!(parse_tps(neo), Some((20.0, Some(10.574))));
        // `spark tps`: empty, or only its first line, never the numbers.
        assert_eq!(parse_tps(""), None);
        assert_eq!(parse_tps("[⚡] TPS from last 5s, 10s, 1m, 5m, 15m:
"), None);
        // The rest of a spark reply showing up in the next command's answer.
        assert_eq!(parse_tps("[⚡] TPS from last 5s, 10s, 1m, 5m, 15m:
There are 0 of a max of 16 players online: 
"), None);
        // A whole spark reply (made up, in spark's layout), should one come through.
        let spark = "[⚡] TPS from last 5s, 10s, 1m, 5m, 15m:
[⚡]  8.1, 9.4, 12.0, 15.2, 18.9
[⚡]
[⚡] Tick durations (min/med/95%ile/max ms) from last 10s, 1m:
[⚡]  12.1/80.3/250.0/900.4;  10.0/70.2/240.1/27000.0
";
        assert_eq!(parse_tps(spark), Some((8.1, None)));
    }

    #[test]
    fn picks_a_command() {
        assert_eq!(tps_command(Some("NeoForge"), "1.21.1", false), Some("neoforge tps"));
        assert_eq!(tps_command(Some("paper"), "1.21.1", false), Some("tps"));
        assert_eq!(tps_command(None, "1.21.1", false), Some("tick query"));
        assert_eq!(tps_command(Some("fabric"), "1.20.1", false), None);
        assert_eq!(tps_command(Some("fabric"), "1.20.1", true), Some("spark tps"));
        // As Hopper reads them from a server's files.
        assert_eq!(tps_command(Some("neoforge 21.1.259"), "1.21.1", true), Some("neoforge tps"));
        assert_eq!(tps_command(Some("neoforge-21.1.259"), "1.21.1", true), Some("neoforge tps"));
        assert_eq!(tps_command(Some("forge 1.20.1-47.4.10"), "1.20.1", true), Some("forge tps"));
        assert_eq!(tps_command(Some("fabric 0.18.1"), "1.21.1", true), Some("tick query"), "vanilla's over spark");
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

    use super::super::Properties;

    const PW: fn() -> String = || "newpass".to_string();

    #[test]
    fn enable_picks_a_port_clear_of_other_servers() {
        let mut p = Properties::parse("server-port=25575\n");
        // Its own game port, another server's RCON and a busy port are skipped.
        let s = enable(&mut p, &[25576], |n| n != 25577, PW).unwrap();
        assert_eq!(s, Setup::Enabled { port: 25578 });
        assert_eq!(p.get("enable-rcon"), Some("true"));
        assert_eq!(p.get("rcon.port"), Some("25578"));
        assert_eq!(p.get("rcon.password"), Some("newpass"));
        assert_eq!(p.get("broadcast-rcon-to-ops"), Some("false"));
    }

    #[test]
    fn enable_keeps_comments_and_order() {
        let text = "#Minecraft server properties\n#Thu Oct 30\nmotd=Hi \\u00A7aall\nenable-rcon=false\n\nrcon.port=25575\nserver-port=25565\nbroadcast-rcon-to-ops=true\n";
        let mut p = Properties::parse(text);
        enable(&mut p, &[], |_| true, PW).unwrap();
        let out = p.to_text();
        let lines: Vec<&str> = out.lines().collect();
        // Untouched lines come back exactly as written, escapes and all.
        assert_eq!(&lines[..4], &["#Minecraft server properties", "#Thu Oct 30", "motd=Hi \\u00A7aall", "enable-rcon=true"]);
        assert_eq!(lines[4], "");
        assert_eq!(lines[5], "rcon.port=25575");
        assert_eq!(lines[6], "server-port=25565");
        // The owner's choice stays.
        assert_eq!(lines[7], "broadcast-rcon-to-ops=true");
        assert_eq!(lines[8], "rcon.password=newpass");
        assert_eq!(lines.len(), 9);
    }

    #[test]
    fn enable_adopts_an_existing_setup() {
        // On with a password: left exactly as it is, even on a shared port.
        let text = "enable-rcon=true\nrcon.port=25600\nrcon.password=mine\n";
        let mut p = Properties::parse(text);
        assert_eq!(enable(&mut p, &[25600], |_| true, PW).unwrap(), Setup::Already { port: 25600 });
        assert_eq!(p.to_text(), Properties::parse(text).to_text());
        // Off but set up: switched on with the same port and password.
        let mut p = Properties::parse("enable-rcon=false\nrcon.port=25600\nrcon.password=mine\n");
        assert_eq!(enable(&mut p, &[], |_| true, PW).unwrap(), Setup::Adopted { port: 25600 });
        assert_eq!(p.get("rcon.password"), Some("mine"));
        // Its port clashes with another server: the password stays, the port moves.
        let mut p = Properties::parse("enable-rcon=false\nrcon.port=25600\nrcon.password=mine\n");
        assert_eq!(enable(&mut p, &[25575, 25600], |_| true, PW).unwrap(), Setup::Adopted { port: 25576 });
        // On but no password: Minecraft won't start RCON, so it gets one.
        let mut p = Properties::parse("enable-rcon=true\nrcon.port=25590\nrcon.password=\n");
        assert_eq!(enable(&mut p, &[], |_| true, PW).unwrap(), Setup::Enabled { port: 25575 });
        assert_eq!(p.get("rcon.password"), Some("newpass"));
    }

    #[test]
    fn enable_in_saves_and_reads_other_servers() {
        let d = std::env::temp_dir().join(format!("hopper-rcon-enable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (a, b) = (d.join("a"), d.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("server.properties"), "server-port=25566\nenable-rcon=true\nrcon.port=25575\nrcon.password=x\n").unwrap();
        std::fs::write(b.join("server.properties"), "# b\nserver-port=25567\n").unwrap();
        let s = enable_in(&b, &[a.clone()], PW).unwrap();
        assert_ne!(s.port(), 25575);
        let text = std::fs::read_to_string(b.join("server.properties")).unwrap();
        assert!(text.starts_with("# b\nserver-port=25567\nenable-rcon=true\n"), "{text}");
        assert_eq!(settings(&b), Some((s.port(), "newpass".to_string())));
        // A second run changes nothing.
        assert_eq!(enable_in(&b, &[a], PW).unwrap(), Setup::Already { port: s.port() });
        let _ = std::fs::remove_dir_all(&d);
    }
}
