//! What the VPS setup reads and runs, without any I/O, so it can be tested:
//! the fields a player types (checked before they go anywhere near a
//! command), what the check commands print, the firewall rules to add or
//! take out, and the systemd unit.
//!
//! Every command the setup runs is built here from fixed text, numbers it
//! checked and hex it made itself. The host and user a player types are
//! only ever used to open the ssh connection, never inside a command.

use serde::{Deserialize, Serialize};

use crate::vps::home::Backend;

/// The relay's system user and group.
pub const USER: &str = "hopper-relay";
/// The program and the record of what the setup changed (root's).
pub const OPT: &str = "/opt/hopper-relay";
pub const BIN: &str = "/opt/hopper-relay/hopperd";
pub const INSTALLED: &str = "/opt/hopper-relay/installed.json";
/// The relay's own data (its key hash, its certificate): only its user.
pub const DATA: &str = "/var/lib/hopper-relay";
pub const UNIT: &str = "/etc/systemd/system/hopper-relay.service";
pub const SERVICE: &str = "hopper-relay";
/// Marks the firewall rules the setup added, so removing takes out only those.
pub const COMMENT: &str = "hopper-relay";

// ---- Velocity (B4) ---------------------------------------------------------------------------
//
// The chain on the VPS: player → the relay's game port → Velocity on
// 127.0.0.1:VELOCITY_PORT (it checks the account and ends the player's
// encryption) → the relay's backend port 127.0.0.1:BACKEND_PORT → one
// stream to home → the router → the server. Velocity runs as its own user:
// it parses what players send, so it can't read the relay's key or CA.

pub const VELOCITY_USER: &str = "hopper-velocity";
pub const JAVA_DIR: &str = "/opt/hopper-relay/java";
pub const JAVA: &str = "/opt/hopper-relay/java/bin/java";
pub const VELOCITY_DIR: &str = "/opt/hopper-relay/velocity";
pub const VELOCITY_JAR: &str = "/opt/hopper-relay/velocity/velocity.jar";
/// Velocity's settings and the forwarding secret (root's; Velocity's group may read).
pub const ETC: &str = "/etc/hopper-relay";
pub const SECRET: &str = "/etc/hopper-relay/forwarding.secret";
pub const VELOCITY_TOML: &str = "/etc/hopper-relay/velocity.toml";
pub const VELOCITY_DATA: &str = "/var/lib/hopper-velocity";
pub const VELOCITY_UNIT: &str = "/etc/systemd/system/hopper-velocity.service";
pub const VELOCITY_SERVICE: &str = "hopper-velocity";
pub const VELOCITY_PORT: u16 = 25577;
/// The relay's voice port (B6): Simple Voice Chat's usual one, so a player
/// who types the address by hand finds it where they'd look first.
pub const VOICE_PORT: u16 = hopper_core::hosting::voice_mods::SVC_DEFAULT_PORT;
pub const BACKEND_PORT: u16 = 25580;

/// A download pinned in this release: where, its SHA-256 and size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pinned {
    pub name: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

/// Velocity 4.3.1 (build 45, channel STABLE), from PaperMC's downloads.
/// Needs Java 25. 3.5.1, the first pin, lost support on 2026-09-30; a
/// relay set up with it is moved to this one by "Run again" (the jar's
/// SHA-256 and the Java archive's differ, so both steps replace them).
pub const VELOCITY: Pinned = Pinned {
    name: "Velocity 4.3.1",
    url: "https://fill-data.papermc.io/v1/objects/0844bcabdcdb4f5b875906fd4c3365bc1bb76918e42bc595ed88ad195258dd75/velocity-4.3.1-45.jar",
    sha256: "0844bcabdcdb4f5b875906fd4c3365bc1bb76918e42bc595ed88ad195258dd75",
    size: 42_342_611,
};

/// The Java the pinned Velocity needs, for the steps' names.
pub const JAVA_MAJOR: u32 = 25;

/// Eclipse Temurin 25.0.5+8 LTS (the JRE), for the VPS's CPU.
pub fn java(arch: &str) -> Option<Pinned> {
    match arch {
        "x64" => Some(Pinned {
            name: "Java 25 (Temurin 25.0.5)",
            url: "https://github.com/adoptium/temurin25-binaries/releases/download/jdk-25.0.5%2B8/OpenJDK25U-jre_x64_linux_hotspot_25.0.5_8.tar.gz",
            sha256: "6fac98f0c8202efc3135a9e9e42aa61963ec42d1c5cbb40caf231e7b14a24ef6",
            size: 61_736_206,
        }),
        "arm64" => Some(Pinned {
            name: "Java 25 (Temurin 25.0.5)",
            url: "https://github.com/adoptium/temurin25-binaries/releases/download/jdk-25.0.5%2B8/OpenJDK25U-jre_aarch64_linux_hotspot_25.0.5_8.tar.gz",
            sha256: "963931723891302ebb2687178532c3d793d71e2577cd058061c7efb396140a86",
            size: 60_517_419,
        }),
        _ => None,
    }
}

/// Each relayed server's place in the relay's Velocity (B7): `wanted` is
/// (server id, its name, its exact addresses) in the relay's order. A
/// server already there keeps its name and loopback port, so adding or
/// taking off one server leaves the others' lines as they were; a new one
/// gets the lowest free port from [`BACKEND_PORT`] (the first server keeps
/// the port a relay from before B7 used). An address two servers claim
/// stays with the first. Past [`crate::vps::MAX_BACKENDS`] servers the rest
/// are left out.
pub fn assign_backends(old: &[Backend], wanted: &[(String, String, Vec<String>)]) -> Vec<Backend> {
    let max = crate::vps::MAX_BACKENDS;
    let mut out: Vec<Backend> = Vec::new();
    let kept: Vec<&Backend> = old.iter().filter(|b| wanted.iter().any(|w| w.0 == b.server) && b.port >= BACKEND_PORT && b.port < BACKEND_PORT + max).collect();
    let mut used_ports: Vec<u16> = kept.iter().map(|b| b.port).collect();
    let mut used_names: Vec<String> = kept.iter().map(|b| b.name.clone()).collect();
    let mut claimed: Vec<String> = Vec::new();
    for (id, name, hosts) in wanted {
        let hosts: Vec<String> = hosts.iter().filter(|h| !claimed.contains(h)).cloned().collect();
        claimed.extend(hosts.iter().cloned());
        if let Some(b) = kept.iter().find(|b| b.server == *id) {
            out.push(Backend { hosts, ..(*b).clone() });
            continue;
        }
        let Some(port) = (BACKEND_PORT..BACKEND_PORT + max).find(|p| !used_ports.contains(p)) else { continue };
        used_ports.push(port);
        let base = server_key(name);
        let name = (1..).map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") }).find(|n| !used_names.contains(n)).expect("a free name");
        used_names.push(name.clone());
        out.push(Backend { server: id.clone(), name, port, hosts });
    }
    out
}

/// A server's name as a key in `velocity.toml`: lower case letters, digits,
/// `-` and `_`, never Velocity's own `try`.
fn server_key(name: &str) -> String {
    let mut k: String = name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' }).collect();
    k = k.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    k.truncate(32);
    let k = k.trim_matches('-').to_string();
    if k.is_empty() || k == "try" { format!("server-{k}").trim_end_matches('-').to_string() } else { k }
}

/// Velocity's `velocity.toml`. `online` is false only on a test box
/// (`HOPPERD_VPS_TEST_OFFLINE`), where test logins have no Mojang account.
/// `backends`: one line per relayed server, and its addresses as forced
/// hosts (B7); none: the one server from before B7, as `home`.
pub fn velocity_toml(online: bool, backends: &[Backend]) -> String {
    let (servers, forced) = if backends.is_empty() {
        (format!("home = \"127.0.0.1:{BACKEND_PORT}\"\ntry = [\"home\"]\n"), String::new())
    } else {
        let mut s = String::new();
        let mut f = String::new();
        for b in backends {
            s.push_str(&format!("{} = \"127.0.0.1:{}\"\n", b.name, b.port));
            for h in &b.hosts {
                f.push_str(&format!("\"{h}\" = [\"{}\"]\n", b.name));
            }
        }
        let names: Vec<String> = backends.iter().map(|b| format!("\"{}\"", b.name)).collect();
        s.push_str(&format!("try = [{}]\n", names.join(", ")));
        (s, f)
    };
    format!(
        "# Made by Hopper Server's relay setup. Velocity starts from this copy each time.
config-version = \"2.7\"
bind = \"127.0.0.1:{VELOCITY_PORT}\"
motd = \"Hopper relay\"
show-max-players = 500
online-mode = {online}
force-key-authentication = {online}
prevent-client-proxy-connections = false
player-info-forwarding-mode = \"modern\"
forwarding-secret-file = \"{SECRET}\"
announce-forge = false
kick-existing-players = false
ping-passthrough = \"all\"
enable-player-address-logging = true

[servers]
{servers}
[forced-hosts]
{forced}
[advanced]

compression-threshold = 256
compression-level = -1
login-ratelimit = 3000
connection-timeout = 5000
read-timeout = 30000
haproxy-protocol = true
tcp-fast-open = false
bungee-plugin-message-channel = true
show-ping-requests = false
failover-on-unexpected-server-disconnect = true
announce-proxy-commands = true
log-command-executions = false
log-player-connections = true
accepts-transfers = false

[query]
enabled = false
port = {VELOCITY_PORT}
map = \"Velocity\"
show-plugins = false
"
    )
}

/// Velocity's systemd unit: its own user, the payload limits big modpacks
/// need, and nothing writable but its data folder.
pub fn velocity_unit() -> String {
    format!(
        "# Made by Hopper Server's relay setup. Remove the relay from the panel to take it out.
[Unit]
Description=Velocity for the Hopper relay (Relay through a VPS)
After=network-online.target
Wants=network-online.target

[Service]
User={VELOCITY_USER}
Group={VELOCITY_USER}
WorkingDirectory={VELOCITY_DATA}
ExecStartPre=/bin/cp -f {VELOCITY_TOML} {VELOCITY_DATA}/velocity.toml
ExecStart={JAVA} -Xms128M -Xmx512M -XX:+UseG1GC -Dvelocity.max-plugin-message-payload-size.clientbound=16777216 -Dvelocity.max-plugin-message-payload-size.serverbound=16777216 -Dvelocity.max-known-packs=1024 -jar {VELOCITY_JAR}
Restart=always
RestartSec=5
SuccessExitStatus=143
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
PrivateDevices=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictSUIDSGID=true
LockPersonality=true
CapabilityBoundingSet=
AmbientCapabilities=
ReadWritePaths={VELOCITY_DATA}
UMask=0077
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
"
    )
}

/// `stat -c '%n %U %G %a'` of Velocity's folders and files, as the setup leaves them.
pub fn velocity_files_right(stat: &str) -> bool {
    let want = [
        format!("{ETC} root {VELOCITY_USER} 750"),
        format!("{SECRET} root {VELOCITY_USER} 640"),
        format!("{VELOCITY_TOML} root {VELOCITY_USER} 640"),
        format!("{VELOCITY_DATA} {VELOCITY_USER} {VELOCITY_USER} 700"),
        format!("{VELOCITY_JAR} root root 644"),
    ];
    let got: Vec<&str> = stat.lines().map(str::trim).collect();
    want.iter().all(|w| got.contains(&w.as_str()))
}

/// Least memory and free disk the relay is set up on.
pub const MIN_MEMORY: u64 = 1800 << 20; // a "2 GB" VPS reports a little under 2 GiB
pub const MIN_DISK: u64 = 2 << 30;

// ---- fields a player types ---------------------------------------------------------------

/// A host name or IP address, as typed. Only letters, digits, dots, dashes
/// and an IPv6 address's colons (in brackets or not): nothing a shell or
/// the ssh library could read as anything else.
pub fn valid_host(h: &str) -> Result<String, String> {
    let h = h.trim();
    let bare = h.strip_prefix('[').and_then(|x| x.strip_suffix(']')).unwrap_or(h);
    if bare.is_empty() {
        return Err("Type the VPS's address (its IP address or name).".into());
    }
    if bare.parse::<std::net::IpAddr>().is_ok() {
        return Ok(bare.to_string());
    }
    let ok = bare.len() <= 253
        && !bare.starts_with(['-', '.'])
        && bare.split('.').all(|l| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    if ok { Ok(bare.to_ascii_lowercase()) } else { Err("That isn't an address. Type an IP address like 203.0.113.10 or a name like vps.example.com.".into()) }
}

/// A Linux user name: what `useradd` accepts by default.
pub fn valid_user(u: &str) -> Result<String, String> {
    let u = u.trim();
    let ok = !u.is_empty()
        && u.len() <= 32
        && u.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && u.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.'));
    if ok { Ok(u.to_string()) } else { Err("The ssh user is one word of small letters and digits, like ubuntu, debian or root.".into()) }
}

/// A port for players or for the tunnel: above 1023, so the relay needs no
/// privileges to listen on it.
pub fn valid_port(p: u16) -> Result<u16, String> {
    if p >= 1024 { Ok(p) } else { Err(format!("Port {p} needs root to listen on. Use a port from 1024 up.")) }
}

/// `s` in single quotes for `sh`.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Runs `cmd` with root rights: as it is for root, through `sudo -n`
/// otherwise (never asks for a password: setup needs passwordless sudo).
pub fn as_root(root: bool, cmd: &str) -> String {
    let q = sh_quote(cmd);
    if root { format!("sh -c {q}") } else { format!("sudo -n sh -c {q}") }
}

/// Runs `cmd` as the relay's user, from a root shell.
pub fn as_relay(cmd: &str) -> String {
    format!("runuser -u {USER} -- {cmd}")
}

// ---- what the check commands print ----------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Os {
    pub id: String,
    pub version_id: String,
    pub pretty: String,
}

pub fn parse_os_release(text: &str) -> Os {
    let mut o = Os::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
        match k.trim() {
            "ID" => o.id = v.to_ascii_lowercase(),
            "VERSION_ID" => o.version_id = v,
            "PRETTY_NAME" => o.pretty = v,
            _ => {}
        }
    }
    if o.pretty.is_empty() {
        o.pretty = format!("{} {}", o.id, o.version_id).trim().to_string();
    }
    o
}

/// Ubuntu 22.04 / 24.04 and Debian 12 / 13 (design/vps-relay.md, Decisions).
pub fn os_supported(o: &Os) -> Result<String, String> {
    let short = match (o.id.as_str(), o.version_id.as_str()) {
        ("ubuntu", v @ ("22.04" | "24.04")) => format!("Ubuntu {v}"),
        ("debian", v @ ("12" | "13")) => format!("Debian {v}"),
        _ => {
            let name = if o.pretty.is_empty() { "This system".to_string() } else { o.pretty.clone() };
            return Err(format!("{name} isn't supported yet. The relay runs on Ubuntu 22.04 or 24.04 and Debian 12 or 13."));
        }
    };
    Ok(short)
}

/// `uname -m` → the release's name for it.
pub fn parse_arch(uname: &str) -> Option<&'static str> {
    match uname.trim() {
        "x86_64" | "amd64" => Some("x64"),
        "aarch64" | "arm64" => Some("arm64"),
        _ => None,
    }
}

/// `MemTotal` from `/proc/meminfo`, in bytes.
pub fn parse_meminfo(text: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

/// The least free space of the folders `df -Pk` was asked about, in bytes.
pub fn parse_df_avail(text: &str) -> Option<u64> {
    text.lines().skip(1).filter_map(|l| l.split_whitespace().nth(3)?.parse::<u64>().ok()).min().map(|kb| kb * 1024)
}

/// Ports something listens on, from `ss -Hltn` (TCP).
pub fn parse_listening(ss: &str) -> Vec<u16> {
    let mut v: Vec<u16> = ss.lines().filter_map(|l| l.split_whitespace().nth(3)?.rsplit_once(':')?.1.parse().ok()).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// What `relay status` prints.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RelayStatus {
    pub version: String,
    pub fingerprint: String,
    pub key_sha256: String,
    pub bind: String,
    pub game_port: u16,
    pub tunnel_port: u16,
    pub velocity_port: u16,
    pub backend_port: u16,
    /// 0 on a relay from before B6.
    pub voice_port: u16,
}

/// The last line of `out` that reads as JSON (the program's log goes to
/// stderr, but be lenient).
pub fn last_json<T: for<'a> Deserialize<'a>>(out: &str) -> Option<T> {
    out.lines().rev().find_map(|l| serde_json::from_str(l.trim()).ok())
}

/// The CPU a Linux program is built for, from its ELF header.
pub fn elf_arch(head: &[u8]) -> Option<&'static str> {
    if head.len() < 20 || &head[..4] != b"\x7fELF" {
        return None;
    }
    match u16::from_le_bytes([head[18], head[19]]) {
        0x3e => Some("x64"),
        0xb7 => Some("arm64"),
        _ => None,
    }
}

// ---- the firewall on the VPS ---------------------------------------------------------------

/// One rule the setup added (kept in `installed.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum FwRule {
    /// `ufw allow <port>/tcp` (`/udp` for voice, B6).
    Ufw {
        port: u16,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        udp: bool,
    },
    /// An iptables ACCEPT for the port in INPUT, marked with [`COMMENT`].
    Iptables {
        port: u16,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        udp: bool,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FwPlan {
    /// Commands, in order (all run as root).
    pub add: Vec<String>,
    /// The rules those commands add.
    pub rules: Vec<FwRule>,
    /// Ports already let in (or nothing filters them).
    pub open: Vec<u16>,
    /// Can't be done safely here: what the player has to do by hand.
    pub manual: Option<String>,
    /// `netfilter-persistent save` is part of `add`.
    pub saved: bool,
}

/// `ufw status` says it is on.
pub fn ufw_active(status: &str) -> bool {
    status.lines().any(|l| l.trim().eq_ignore_ascii_case("status: active"))
}

/// A port `ufw status` already lets in (TCP, or UDP, from anywhere; a rule
/// with no protocol lets in both).
fn ufw_allows(status: &str, port: u16, udp: bool) -> bool {
    status.lines().any(|l| {
        let mut w = l.split_whitespace();
        let to = w.next().unwrap_or("");
        (to == port.to_string() || to == format!("{port}/{}", proto(udp))) && l.contains("ALLOW") && !l.contains("ALLOW OUT")
    })
}

/// Where the first rule that would turn the ports away sits in INPUT
/// (1-based, as `iptables -I INPUT <n>` counts), from `iptables -S INPUT`.
/// `None`: nothing turns them away (the chain accepts by default and has no
/// REJECT or DROP for them).
pub fn iptables_block_at(rules: &str, ports: &[u16]) -> Option<usize> {
    let mut n = 0;
    let mut policy_drop = false;
    for l in rules.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        if w.first() == Some(&"-P") {
            policy_drop = w.get(2).is_some_and(|p| *p == "DROP" || *p == "REJECT");
            continue;
        }
        if w.first() != Some(&"-A") {
            continue;
        }
        n += 1;
        let target = w.iter().position(|x| *x == "-j").and_then(|i| w.get(i + 1)).copied().unwrap_or("");
        if !matches!(target, "REJECT" | "DROP") {
            continue;
        }
        // A rule for other ports, or only for some interfaces or senders,
        // doesn't turn ours away; a catch-all or one naming our port does.
        let narrowed = w.iter().any(|x| matches!(*x, "-i" | "-s" | "--state" | "--ctstate"));
        let dports = rule_ports(&w);
        let hits = match &dports {
            None => !narrowed,
            Some(d) => ports.iter().any(|p| d.contains(p)),
        };
        if hits {
            return Some(n);
        }
    }
    policy_drop.then_some(n + 1)
}

/// The ports a rule names (`--dport 25565`, `--dports 25565,25566`), if any.
fn rule_ports(w: &[&str]) -> Option<Vec<u16>> {
    let i = w.iter().position(|x| *x == "--dport" || *x == "--dports")?;
    let v = w.get(i + 1)?;
    Some(
        v.split(',')
            .flat_map(|p| match p.split_once(':') {
                Some((a, b)) => {
                    let (a, b) = (a.parse().unwrap_or(0u16), b.parse().unwrap_or(0u16));
                    (a..=b).collect::<Vec<_>>()
                }
                None => p.parse().ok().into_iter().collect(),
            })
            .collect(),
    )
}

/// An ACCEPT for `port` (TCP, or UDP) before position `before` (anywhere
/// when `None`).
fn iptables_accepts(rules: &str, port: u16, udp: bool, before: Option<usize>) -> bool {
    let mut n = 0;
    for l in rules.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        if w.first() != Some(&"-A") {
            continue;
        }
        n += 1;
        if before.is_some_and(|b| n >= b) {
            return false;
        }
        let accept = w.windows(2).any(|p| p == ["-j", "ACCEPT"]);
        let narrowed = w.iter().any(|x| matches!(*x, "-i" | "-s"));
        if accept && !narrowed && rule_ports(&w).is_some_and(|d| d.contains(&port)) && !w.windows(2).any(|p| p == ["-p", proto(!udp)]) {
            return true;
        }
    }
    false
}

pub fn iptables_rule(port: u16, udp: bool) -> String {
    format!("-p {} --dport {port} -m comment --comment {COMMENT} -j ACCEPT", proto(udp))
}

fn proto(udp: bool) -> &'static str {
    if udp { "udp" } else { "tcp" }
}

/// Native nftables (not iptables' own tables) with an input chain that
/// drops by default: changing it safely isn't something to do over ssh.
pub fn nft_blocks_input(ruleset: &str) -> bool {
    let mut table = String::new();
    for l in ruleset.lines() {
        let t = l.trim();
        if let Some(rest) = t.strip_prefix("table ") {
            table = rest.trim_end_matches('{').trim().to_string();
        }
        // iptables-nft keeps its rules in "ip filter" / "ip6 filter"; those
        // are read through iptables instead.
        if t.contains("hook input") && t.contains("policy drop") && table != "ip filter" && table != "ip6 filter" {
            return true;
        }
    }
    false
}

/// What to do so the `tcp` and `udp` ports get in. Only ever *adds* ACCEPT
/// rules for those ports: it never turns a firewall on, changes a default
/// policy, or touches any other rule, so the ssh connection it runs over
/// can't be cut.
///
/// - ufw on: `ufw allow <port>/tcp` (or `/udp`) for each port it doesn't
///   allow yet.
/// - otherwise iptables: an ACCEPT inserted just before the first rule that
///   would reject the ports (cloud images often end INPUT with a REJECT),
///   then `netfilter-persistent save` when that is installed.
/// - native nftables that drops input: told to the player, not changed.
/// - nothing filtering: nothing to do.
pub fn plan_firewall(ufw: Option<&str>, iptables: Option<&str>, nft: Option<&str>, persist: bool, tcp: &[u16], udp: &[u16]) -> FwPlan {
    let mut p = FwPlan::default();
    let all: Vec<(u16, bool)> = tcp.iter().map(|&t| (t, false)).chain(udp.iter().map(|&u| (u, true))).collect();
    let ports: Vec<u16> = all.iter().map(|a| a.0).collect();
    if let Some(s) = ufw.filter(|s| ufw_active(s)) {
        for &(port, udp) in &all {
            if ufw_allows(s, port, udp) {
                p.open.push(port);
            } else {
                p.add.push(format!("ufw allow {port}/{} comment {COMMENT}", proto(udp)));
                p.rules.push(FwRule::Ufw { port, udp });
            }
        }
        return p;
    }
    if let Some(rules) = iptables {
        match iptables_block_at(rules, &ports) {
            None => p.open.extend_from_slice(&ports),
            Some(at) => {
                let mut pos = at;
                for &(port, udp) in &all {
                    if iptables_accepts(rules, port, udp, Some(at)) {
                        p.open.push(port);
                        continue;
                    }
                    p.add.push(format!("iptables -I INPUT {pos} {}", iptables_rule(port, udp)));
                    p.rules.push(FwRule::Iptables { port, udp });
                    pos += 1;
                }
                if !p.add.is_empty() && persist {
                    p.add.push("netfilter-persistent save".into());
                    p.saved = true;
                }
            }
        }
    }
    // A packet has to pass every table that hooks input, so iptables'
    // ACCEPT doesn't get it through a native nftables chain that drops.
    if nft.is_some_and(nft_blocks_input) {
        let list: Vec<String> = all.iter().map(|(p, u)| format!("{p}/{}", proto(*u))).collect();
        p.manual = Some(format!("The VPS's nftables rules drop what isn't allowed. Allow {} there (in /etc/nftables.conf), then run the setup again.", list.join(" and ")));
        p.open.clear();
    }
    p
}

/// Takes out exactly the rules the setup added.
pub fn remove_firewall(rules: &[FwRule], persist: bool) -> Vec<String> {
    let mut v = Vec::new();
    let mut ipt = false;
    for r in rules {
        match r {
            FwRule::Ufw { port, udp } => v.push(format!("ufw delete allow {port}/{}", proto(*udp))),
            FwRule::Iptables { port, udp } => {
                // Every copy of it, in case Repair ran while one was there.
                let rule = iptables_rule(*port, *udp);
                v.push(format!("while iptables -C INPUT {rule} 2>/dev/null; do iptables -D INPUT {rule}; done"));
                ipt = true;
            }
        }
    }
    if ipt && persist {
        v.push("netfilter-persistent save".into());
    }
    v
}

// ---- the service ------------------------------------------------------------------------

/// The systemd unit. The relay runs as its own user with no login and no
/// capabilities (its ports are above 1023); it can write only its data
/// folder.
pub fn unit() -> String {
    format!(
        "# Made by Hopper Server's relay setup. Remove the relay from the panel to take it out.
[Unit]
Description=Hopper relay (Relay through a VPS)
After=network-online.target
Wants=network-online.target

[Service]
User={USER}
Group={USER}
ExecStart={BIN} relay run --data {DATA}
Restart=always
RestartSec=5
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
PrivateDevices=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictSUIDSGID=true
LockPersonality=true
CapabilityBoundingSet=
AmbientCapabilities=
ReadWritePaths={DATA}
UMask=0077
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
"
    )
}

/// What `installed.json` keeps: everything the setup changed, so Remove
/// takes out that and nothing else.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Installed {
    /// The relay user didn't exist before.
    pub made_user: bool,
    pub firewall: Vec<FwRule>,
    /// The program in place, and the version it said it was.
    pub binary_sha256: String,
    pub binary_version: String,
    /// Velocity's user didn't exist before.
    pub made_velocity_user: bool,
    /// The Java archive the setup unpacked (its pinned SHA-256).
    pub java_sha256: String,
}

/// One step of a run, as the dialog shows it.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Step {
    pub id: &'static str,
    pub title: String,
    /// "wait", "run", "ok" (already right: nothing done), "done" (changed),
    /// "warn" (done, with something to know), "fail", "skip".
    pub state: &'static str,
    pub detail: String,
}

impl Step {
    pub fn new(id: &'static str, title: impl Into<String>) -> Step {
        Step { id, title: title.into(), state: "wait", detail: String::new() }
    }
}

/// The steps of an install (repair is the same list: each checks first).
pub fn install_steps(version: &str, ports: (u16, u16)) -> Vec<Step> {
    vec![
        Step::new("user", "Relay user and folders"),
        Step::new("binary", format!("Hopper relay {version}")),
        Step::new("java", format!("Java {JAVA_MAJOR} for Velocity")),
        Step::new("velocity", format!("{} and the forwarding secret", VELOCITY.name)),
        Step::new("keys", "Relay key and certificate"),
        Step::new("service", "Relay service"),
        Step::new("velocity-service", "Velocity service"),
        Step::new("firewall", format!("Firewall on the VPS: {} and {}, voice {VOICE_PORT}", ports.0, ports.1)),
        Step::new("reach", "Ports open from the internet"),
        Step::new("pair", "Connect this panel to the relay"),
    ]
}

pub fn remove_steps() -> Vec<Step> {
    vec![
        Step::new("service", "Stop and remove the relay and Velocity services"),
        Step::new("firewall", "Take out the firewall rules it added"),
        Step::new("files", "Delete the relay's program and data"),
        Step::new("user", "Delete the relay and Velocity users"),
        Step::new("pair", "Disconnect this panel"),
        Step::new("login", "Forget the ssh login"),
    ]
}

/// Bytes as the dialog shows them: "12 GB", "1.9 GB", "512 MB".
pub fn size(b: u64) -> String {
    let gb = b as f64 / (1u64 << 30) as f64;
    if gb >= 10.0 {
        format!("{gb:.0} GB")
    } else if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{} MB", b >> 20)
    }
}

/// The INPUT rules marked as the relay's, from `iptables -S INPUT`.
pub fn iptables_marked(rules: &str) -> Vec<FwRule> {
    rules
        .lines()
        .filter(|l| l.split_whitespace().any(|w| w == COMMENT || w == format!("\"{COMMENT}\"")))
        .filter_map(|l| {
            let w: Vec<&str> = l.split_whitespace().collect();
            let udp = w.windows(2).any(|p| p == ["-p", "udp"]);
            w.iter().position(|x| *x == "--dport").and_then(|i| w.get(i + 1)?.parse().ok()).map(|port| FwRule::Iptables { port, udp })
        })
        .collect()
}

// ---- is it already right? (each install step checks before it acts) ------------------------

/// `stat -c '%n %U %G %a' OPT DATA`, when the user exists.
pub fn folders_right(user_exists: bool, stat: &str) -> bool {
    user_exists && stat.trim() == format!("{OPT} root root 755\n{DATA} {USER} {USER} 700")
}

/// `stat -c '%U %a' DATA DATA/*`: the folder 0700 and every file 0600,
/// all the relay user's.
pub fn data_private(stat: &str) -> bool {
    let mut lines = stat.lines();
    lines.next() == Some(&format!("{USER} 700")) && lines.all(|l| l == format!("{USER} 600"))
}

/// The program in place is the one wanted: the test file by checksum, a
/// release by the checksum recorded when it was put there and the version
/// it says it is.
pub fn binary_right(current_sha: &str, recorded_sha: &str, says: &str, version: &str, file_sha: Option<&str>) -> bool {
    match file_sha {
        Some(f) => current_sha == f,
        None => !current_sha.is_empty() && current_sha == recorded_sha && says.trim() == format!("hopperd {version}"),
    }
}

/// What the relay says about its key and settings against what home has:
/// (the key matches, nothing needs doing).
pub fn keys_right(st: Option<&RelayStatus>, home_key: &str, home_fp: &str, home_addr: &str, addr: &str, bind: &str, ports: (u16, u16), private: bool) -> (bool, bool) {
    let Some(s) = st else { return (false, false) };
    let key = !home_key.is_empty() && s.key_sha256 == super::super::link::hash_hex(&super::super::link::key_hash(home_key)) && home_fp == s.fingerprint;
    let settings = s.bind == bind && s.game_port == ports.0 && s.tunnel_port == ports.1 && s.velocity_port == VELOCITY_PORT && s.backend_port == BACKEND_PORT && s.voice_port == VOICE_PORT;
    (key, key && settings && private && home_addr == addr)
}

/// What the service step does, in order.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum ServiceAct {
    WriteUnit,
    Enable,
    Restart,
}

pub fn service_acts(unit_now: &str, enabled: &str, active: &str, restart: bool) -> Vec<ServiceAct> {
    service_acts_for(&unit(), unit_now, enabled, active, restart)
}

/// [`service_acts`] for any unit (`want` is what it should say).
pub fn service_acts_for(want: &str, unit_now: &str, enabled: &str, active: &str, restart: bool) -> Vec<ServiceAct> {
    let mut v = Vec::new();
    let write = unit_now != want;
    if write {
        v.push(ServiceAct::WriteUnit);
    }
    if enabled.trim() != "enabled" {
        v.push(ServiceAct::Enable);
    }
    if write || restart || active.trim() != "active" {
        v.push(ServiceAct::Restart);
    }
    v
}
