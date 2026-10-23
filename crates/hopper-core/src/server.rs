//! Servers on rig: discovery, config, loader version and a status ping.
//!
//! Hopper only reads here. Crafty or MCSS own start/stop, so Hopper never
//! touches the process.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Manager {
    Crafty,
    Mcss,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct ServerInfo {
    pub name: String,
    pub root: PathBuf,
    pub manager: Manager,
    pub properties: BTreeMap<String, String>,
    /// e.g. "neoforge 21.1.256", from the run script.
    pub loader: Option<String>,
    pub java: Option<String>,
    /// -Xmx from user_jvm_args.txt.
    pub max_memory: Option<String>,
    /// On a Hopper Server: reached through its API, not the file system.
    pub remote: Option<crate::hopperd::Remote>,
}

impl ServerInfo {
    pub fn port(&self) -> u16 {
        self.properties
            .get("server-port")
            .and_then(|p| p.parse().ok())
            .unwrap_or(25565)
    }

    pub fn mods_dir(&self) -> PathBuf {
        self.root.join("mods")
    }
}

/// Every folder under `root` with a `server.properties` is a server.
pub fn discover(root: &Path) -> Vec<ServerInfo> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<ServerInfo> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("server.properties").is_file())
        .map(|p| load(&p))
        .collect();
    out.sort_by_key(|s| s.name.to_lowercase());
    out
}

pub fn load(root: &Path) -> ServerInfo {
    let manager = if root.join("crafty_managed.txt").exists() {
        Manager::Crafty
    } else if root.join("mcss_server_config.json").exists() {
        Manager::Mcss
    } else {
        Manager::Unknown
    };
    let (loader, java) = read_run_script(&root.join("run.bat"));
    ServerInfo {
        name: root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        root: root.to_path_buf(),
        manager,
        properties: read_properties(&root.join("server.properties")),
        loader,
        java,
        max_memory: fs::read_to_string(root.join("user_jvm_args.txt"))
            .ok()
            .and_then(|t| {
                t.lines()
                    .find_map(|l| l.trim().strip_prefix("-Xmx").map(str::to_string))
            }),
        remote: None,
    }
}

fn read_properties(path: &Path) -> BTreeMap<String, String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

/// Finds `@libraries/net/neoforged/neoforge/<ver>/win_args.txt` (or the Forge
/// equivalent) and the java executable on the launch line.
fn read_run_script(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(text) = fs::read_to_string(path) else {
        return (None, None);
    };
    for line in text.lines().map(str::trim) {
        let low = line.to_ascii_lowercase();
        if low.starts_with("rem") || low.starts_with("::") || !low.contains("java") {
            continue;
        }
        let java = if let Some(rest) = line.strip_prefix('"') {
            rest.split('"').next().map(str::to_string)
        } else {
            line.split_whitespace().next().map(str::to_string)
        };
        for (needle, name) in [
            ("net/neoforged/neoforge/", "neoforge"),
            ("net/minecraftforge/forge/", "forge"),
        ] {
            if let Some(i) = line.find(needle) {
                let ver = line[i + needle.len()..].split('/').next().unwrap_or("");
                return (Some(format!("{name} {ver}")), java);
            }
        }
        return (None, java);
    }
    (None, None)
}

#[derive(Clone, Debug)]
pub enum Status {
    Online {
        players: u32,
        max: u32,
        version: String,
    },
    Offline,
}

/// Server List Ping (the multiplayer screen's request).
pub fn ping(host: &str, port: u16) -> Status {
    ping_inner(host, port).unwrap_or(Status::Offline)
}

fn ping_inner(host: &str, port: u16) -> anyhow::Result<Status> {
    // "rig" resolves to an IPv6 link-local address first, which needs a scope
    // id to connect. Try IPv4 first, then everything else.
    let mut addrs: Vec<_> = (host, port).to_socket_addrs()?.collect();
    addrs.sort_by_key(|a| !a.is_ipv4());
    let mut s = addrs
        .iter()
        .find_map(|a| TcpStream::connect_timeout(a, Duration::from_millis(1500)).ok())
        .ok_or_else(|| anyhow::anyhow!("no address answered"))?;
    s.set_read_timeout(Some(Duration::from_secs(3)))?;

    // Handshake: id 0, protocol -1, host, port, next state 1 (status).
    let mut body = vec![0x00];
    write_varint(&mut body, -1);
    write_varint(&mut body, host.len() as i32);
    body.extend_from_slice(host.as_bytes());
    body.extend_from_slice(&port.to_be_bytes());
    write_varint(&mut body, 1);
    let mut packet = Vec::new();
    write_varint(&mut packet, body.len() as i32);
    packet.extend(body);
    // Status request: length 1, id 0.
    packet.extend([0x01, 0x00]);
    s.write_all(&packet)?;

    let _len = read_varint(&mut s)?;
    let _id = read_varint(&mut s)?;
    let json_len = read_varint(&mut s)? as usize;
    let mut json = vec![0; json_len];
    s.read_exact(&mut json)?;
    let v: serde_json::Value = serde_json::from_slice(&json)?;
    Ok(Status::Online {
        players: v["players"]["online"].as_u64().unwrap_or(0) as u32,
        max: v["players"]["max"].as_u64().unwrap_or(0) as u32,
        version: v["version"]["name"].as_str().unwrap_or("").to_string(),
    })
}

fn write_varint(buf: &mut Vec<u8>, value: i32) {
    let mut v = value as u32;
    loop {
        if v & !0x7F == 0 {
            buf.push(v as u8);
            return;
        }
        buf.push((v & 0x7F | 0x80) as u8);
        v >>= 7;
    }
}

fn read_varint(r: &mut impl Read) -> anyhow::Result<i32> {
    let mut out = 0i32;
    for i in 0..5 {
        let mut b = [0u8];
        r.read_exact(&mut b)?;
        out |= ((b[0] & 0x7F) as i32) << (7 * i);
        if b[0] & 0x80 == 0 {
            return Ok(out);
        }
    }
    anyhow::bail!("varint too long")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trip() {
        for n in [0, 1, 127, 128, 25565, -1] {
            let mut buf = Vec::new();
            write_varint(&mut buf, n);
            assert_eq!(read_varint(&mut buf.as_slice()).unwrap(), n);
        }
    }

    #[test]
    fn reads_neoforge_run_bat() {
        let dir = std::env::temp_dir().join(format!("hopper-run-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let bat = dir.join("run.bat");
        fs::write(
            &bat,
            "@echo off\nREM java stuff\n\"G:\\jdk\\bin\\java.exe\" @user_jvm_args.txt @libraries/net/neoforged/neoforge/21.1.256/win_args.txt nogui%*\n",
        )
        .unwrap();
        let (loader, java) = read_run_script(&bat);
        assert_eq!(loader.as_deref(), Some("neoforge 21.1.256"));
        assert_eq!(java.as_deref(), Some("G:\\jdk\\bin\\java.exe"));
        let _ = fs::remove_dir_all(&dir);
    }
}

/// Pings the real servers on rig. `cargo test real_ping -- --ignored --nocapture`
#[cfg(test)]
#[test]
#[ignore]
fn real_ping() {
    for port in [25565, 25567] {
        println!(
            "rig:{port} -> {:?}",
            ping_inner("rig", port).map_err(|e| format!("{e:#}"))
        );
    }
}
