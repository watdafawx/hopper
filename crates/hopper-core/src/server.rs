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
    /// e.g. "neoforge 21.1.256", "fabric 0.18.1": what the start script
    /// runs ([`crate::hosting::script::start_info`]).
    pub loader: Option<String>,
    pub java: Option<String>,
    /// The -Xmx the server starts with (last on its command line, so
    /// user_jvm_args.txt's or the start script's). None: the JVM default.
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

    /// Where the server is, for messages: its folder, or "TNP-LL8 on rig"
    /// for a Hopper Server (its root is a stand-in path, not a folder).
    pub fn place(&self, panels: &[crate::settings::HopperPanel]) -> String {
        let Some(r) = &self.remote else {
            return self.root.display().to_string();
        };
        let panel = panels
            .iter()
            .find(|p| p.id == r.panel)
            .map(|p| p.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| crate::hopperd::host_name(&r.base));
        format!("{} on {panel}", self.name)
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
    let start = crate::hosting::script::start_info(root);
    ServerInfo {
        name: root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        root: root.to_path_buf(),
        manager,
        properties: read_properties(&root.join("server.properties")),
        loader: start.loader,
        java: start.java,
        max_memory: start.max_memory,
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
    fn place_names_remote_servers() {
        let mut s = ServerInfo {
            name: "TNP-LL8".into(),
            root: crate::hopperd::panel_root("p1", "TNP-LL8"),
            manager: Manager::Unknown,
            properties: BTreeMap::new(),
            loader: None,
            java: None,
            max_memory: None,
            remote: Some(crate::hopperd::Remote {
                base: "https://192.168.50.3:8790".into(),
                id: "x".into(),
                ca: String::new(),
                panel: "p1".into(),
            }),
        };
        let panels = vec![crate::settings::HopperPanel {
            id: "p1".into(),
            name: "rig".into(),
            url: String::new(),
            ca: String::new(),
        }];
        assert_eq!(s.place(&panels), "TNP-LL8 on rig");
        assert_eq!(s.place(&[]), "TNP-LL8 on 192.168.50.3");
        s.remote = None;
        s.root = PathBuf::from(r"G:\Minecraft\packs\TNP-LL8");
        assert_eq!(s.place(&panels), r"G:\Minecraft\packs\TNP-LL8");
    }

    #[test]
    fn varint_round_trip() {
        for n in [0, 1, 127, 128, 25565, -1] {
            let mut buf = Vec::new();
            write_varint(&mut buf, n);
            assert_eq!(read_varint(&mut buf.as_slice()).unwrap(), n);
        }
    }

    /// A temp server folder with `files` (path, text); "" makes an empty file.
    fn folder(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hopper-srv-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for (p, text) in files {
            let p = dir.join(p);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            if p.extension().is_some() || !text.is_empty() {
                fs::write(&p, text).unwrap();
            } else {
                fs::create_dir_all(&p).unwrap();
            }
        }
        dir
    }

    const FORGE_HEADER: &str = "@echo off\r\nREM Forge requires a configured set of both JVM and program arguments.\r\nREM Add custom JVM arguments to the user_jvm_args.txt\r\n";

    #[test]
    fn reads_neoforge_run_bat() {
        let dir = folder("neo", &[
            ("server.properties", ""),
            ("run.bat", "@echo off\nREM java stuff\n\"G:\\jdk\\bin\\java.exe\" @user_jvm_args.txt @libraries/net/neoforged/neoforge/21.1.256/win_args.txt nogui%*\n"),
            ("user_jvm_args.txt", "-Xms12G\n-Xmx16G\n-XX:+UseG1GC\n"),
        ]);
        let i = load(&dir);
        assert_eq!(i.loader.as_deref(), Some("neoforge 21.1.256"));
        assert_eq!(i.java.as_deref(), Some("G:\\jdk\\bin\\java.exe"));
        assert_eq!(i.max_memory.as_deref(), Some("16G"));
        assert_eq!(crate::hosting::script::start_info(&dir).minecraft.as_deref(), Some("1.21.1"));
        let _ = fs::remove_dir_all(&dir);
    }

    /// BCG: Fabric started by start.bat with -Xmx in it, no user_jvm_args.txt.
    #[test]
    fn fabric_start_bat_server() {
        let dir = folder("fabric", &[
            ("server.properties", ""),
            ("start.bat", "java -Dfabric.log.level=DEBUG -Xmx8G -jar fabric-server-launch.jar nogui\r\npause"),
            ("fabric-server-launch.jar", ""),
            (".fabric/server/1.20.1-server.jar", ""),
            (".fabric/server/fabric-loader-server-0.18.1-minecraft-1.20.1.jar", ""),
            ("versions/1.20.1/server-1.20.1.jar", ""),
            ("libraries/net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar", ""),
            ("libraries/net/fabricmc/fabric-loader/0.18.1/fabric-loader-0.18.1.jar", ""),
        ]);
        let i = load(&dir);
        assert_eq!(i.loader.as_deref(), Some("fabric 0.18.1"));
        assert_eq!(i.max_memory.as_deref(), Some("8G"));
        assert_eq!(i.java.as_deref(), Some("java"));
        let s = crate::hosting::script::start_info(&dir);
        assert_eq!((s.minecraft.as_deref(), s.max_memory_mb), (Some("1.20.1"), Some(8192)));
        // Without Fabric's cache: the newest loader in libraries, the
        // version from versions/.
        fs::remove_dir_all(dir.join(".fabric")).unwrap();
        let s = crate::hosting::script::start_info(&dir);
        assert_eq!((s.loader.as_deref(), s.minecraft.as_deref()), (Some("fabric 0.18.1"), Some("1.20.1")));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Linggango: two Forge versions in libraries; the one run.bat names
    /// wins. A second java line (a leftover) is ignored, as cmd never gets
    /// past the first while the server runs.
    #[test]
    fn forge_version_is_the_one_the_script_names() {
        for (named, other) in [("47.4.15", "47.4.10"), ("47.4.10", "47.4.15")] {
            let bat = format!("{FORGE_HEADER}java @user_jvm_args.txt @libraries/net/minecraftforge/forge/1.20.1-{named}/win_args.txt %*\r\njava -Xmx10024M -Xms10024M -jar server.jar nogui\r\npause\r\n");
            let lib = |v: &str| format!("libraries/net/minecraftforge/forge/1.20.1-{v}/win_args.txt");
            let (a, b) = (lib(named), lib(other));
            let dir = folder(&format!("forge-{named}"), &[
                ("server.properties", ""),
                ("run.bat", &bat),
                ("user_jvm_args.txt", "# A good default for linggango server is 10GB.\n# Uncomment the next line to set it.\n-Xmx10G"),
                (&a, "x"),
                (&b, "x"),
            ]);
            let i = load(&dir);
            assert_eq!(i.loader, Some(format!("forge 1.20.1-{named}")));
            assert_eq!(i.max_memory.as_deref(), Some("10G"));
            assert_eq!(crate::hosting::script::start_info(&dir).minecraft.as_deref(), Some("1.20.1"));
            let _ = fs::remove_dir_all(&dir);
        }
    }

    /// Val: several -Xmx lines in user_jvm_args.txt (Java uses the last);
    /// only commented ones: no -Xmx at all, the JVM's default.
    #[test]
    fn memory_is_the_last_xmx() {
        let bat = format!("{FORGE_HEADER}java @user_jvm_args.txt @libraries/net/minecraftforge/forge/1.20.1-47.4.10/win_args.txt %*\r\npause\r\n");
        let dir = folder("xmx", &[
            ("server.properties", ""),
            ("run.bat", &bat),
            ("user_jvm_args.txt", "# For example, to set the maximum to 3GB: -Xmx3G\n-Xmx3G\n-Xmx4G\n"),
        ]);
        let i = load(&dir);
        assert_eq!((i.loader.as_deref(), i.max_memory.as_deref()), (Some("forge 1.20.1-47.4.10"), Some("4G")));
        fs::write(dir.join("user_jvm_args.txt"), "# Uncomment the next line to set it.\n# -Xmx4G").unwrap();
        assert_eq!(load(&dir).max_memory, None);
        // The script's own -Xmx after @user_jvm_args.txt beats the file's.
        fs::write(dir.join("user_jvm_args.txt"), "-Xmx4G\n").unwrap();
        fs::write(dir.join("run.bat"), "java @user_jvm_args.txt -Xmx6G -jar server.jar nogui\r\n").unwrap();
        assert_eq!(load(&dir).max_memory.as_deref(), Some("6G"));
        // No start script: user_jvm_args.txt alone.
        fs::remove_file(dir.join("run.bat")).unwrap();
        assert_eq!(load(&dir).max_memory.as_deref(), Some("4G"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_forge_and_quilt() {
        let dir = folder("old", &[
            ("server.properties", ""),
            ("start.bat", "\"C:\\jre8\\bin\\java.exe\" -Xmx6G -jar forge-1.12.2-14.23.5.2860.jar nogui\r\n"),
        ]);
        let s = crate::hosting::script::start_info(&dir);
        assert_eq!(s.loader.as_deref(), Some("forge 1.12.2-14.23.5.2860"));
        assert_eq!(s.minecraft.as_deref(), Some("1.12.2"));
        let _ = fs::remove_dir_all(&dir);
        let dir = folder("quilt", &[
            ("server.properties", ""),
            ("quilt-server-launch.jar", ""),
            ("libraries/org/quiltmc/quilt-loader/0.26.4/quilt-loader-0.26.4.jar", ""),
        ]);
        assert_eq!(load(&dir).loader.as_deref(), Some("quilt 0.26.4"));
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

/// What every server under G:\Minecraft\packs on rig starts as (read only).
/// `cargo test -p hopper-core real_start_info -- --ignored --nocapture`
#[cfg(test)]
#[test]
#[ignore]
fn real_start_info() {
    for s in discover(Path::new(r"G:\Minecraft\packs")) {
        println!("{}: {:?}", s.name, crate::hosting::script::start_info(&s.root));
    }
}
