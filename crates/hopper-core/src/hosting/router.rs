//! Server addresses: one Minecraft port for many servers. A client's first
//! packet (the handshake) carries the address the player typed; the panel
//! reads it, picks the server with that address and passes the connection
//! on to the server's own port. A server that isn't up gets an answer from
//! the panel instead: "Sleeping · join to wake it" in the server list, and
//! joining starts it.
//!
//! This module is the protocol and the matching, without sockets; the
//! panel does the networking. Written from the Minecraft protocol
//! (wiki.vg), not from other proxies.

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use super::boot::{BootStatus, StepState, short_time};
use serde_json::{Value, json};
use std::path::Path;

/// The port players use; servers behind the router get ports of their own.
pub const DEFAULT_PORT: u16 = 25565;

/// Longest handshake accepted (a real one is under 300 bytes).
pub const MAX_HANDSHAKE: usize = 1024;

// ---- per-server settings -----------------------------------------------------

pub const FILE: &str = ".hopper-route.json";

/// A server's addresses and sleep setting, kept in its folder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RouteSettings {
    /// Full addresses ("survival.mc.example.com", "*.mc.example.com").
    pub addresses: Vec<String>,
    /// Stop the server after this many minutes with nobody on; 0: never.
    pub sleep_after_minutes: u32,
    /// How long the last start took, for "about 2 min" when waking it.
    pub last_start_secs: Option<u32>,
    /// No address at all, not even the default `name.<domain>`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub off: bool,
    /// A player joining a sleeping server starts it (on unless turned off).
    pub wake_on_join: bool,
    /// Who may wake it by joining.
    pub wake_who: WakeWho,
    /// The server list's line while it sleeps; empty: [`SLEEP_MESSAGE`].
    #[serde(skip_serializing_if = "String::is_empty")]
    pub sleep_message: String,
    /// After a wake that didn't come up, joining starts it again only after
    /// this many minutes; 0: only once someone starts it in the panel.
    pub retry_after_minutes: u32,
    /// The last wake that didn't come up, until a start that does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wake_failed: Option<WakeFailed>,
    /// Who last woke it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_wake: Option<LastWake>,
    /// What a player who joins while it starts gets.
    pub while_starting: WhileStarting,
    /// What a Forge / NeoForge player joining a modded server that's starting
    /// gets (the lobby island can't take them).
    pub modded_players: ModdedPlayers,
}

/// What a modded (Forge / NeoForge) player joining a modded server that's
/// starting gets. Vanilla players are not affected (see [`WhileStarting`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModdedPlayers {
    /// "Join again in about 2 min."
    #[default]
    Kick,
    /// Held on the connecting screen until the server is up (experimental;
    /// see hopperd's `hold`).
    Wait,
}

/// What a player joining a server that's starting gets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WhileStarting {
    /// The lobby island with the start's progress, then a move to the
    /// server (Minecraft 1.21 / 1.21.1; see `limbo`). Others get the kick.
    #[default]
    Lobby,
    /// "Join again in about 2 min."
    Kick,
}

impl Default for RouteSettings {
    fn default() -> Self {
        RouteSettings {
            addresses: Vec::new(),
            sleep_after_minutes: 0,
            last_start_secs: None,
            off: false,
            wake_on_join: true,
            wake_who: WakeWho::Anyone,
            sleep_message: String::new(),
            retry_after_minutes: 0,
            wake_failed: None,
            last_wake: None,
            while_starting: WhileStarting::Lobby,
            modded_players: ModdedPlayers::Kick,
        }
    }
}

/// The server list's line for a sleeping server, unless one is set.
pub const SLEEP_MESSAGE: &str = "💤 Sleeping · join to wake it";

/// Who may wake a sleeping server by joining. The name in a login isn't
/// checked until the server itself is up (online mode), so this keeps
/// strangers from waking it by accident, not on purpose.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WakeWho {
    #[default]
    Anyone,
    /// The whitelist and the ops.
    Whitelist,
    Ops,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WakeFailed {
    /// Unix seconds.
    pub at: i64,
    /// Who joined to wake it.
    pub by: String,
    /// "crashed while starting", "stopped before it was up", or the start's error.
    pub why: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LastWake {
    pub by: String,
    /// Unix seconds.
    pub at: i64,
}

impl RouteSettings {
    /// Whether joining may start it now: sleep and wake on, and no failed
    /// wake still holding it (see [`RouteSettings::retry_after_minutes`]).
    pub fn wakes(&self, now: i64) -> bool {
        self.sleep_after_minutes > 0 && self.wake_on_join && self.failed_hold(now).is_none()
    }

    /// The failed wake that keeps joins from starting it now, if any.
    pub fn failed_hold(&self, now: i64) -> Option<&WakeFailed> {
        let f = self.wake_failed.as_ref()?;
        let retry = self.retry_after_minutes as i64 * 60;
        (retry == 0 || now < f.at + retry).then_some(f)
    }

    /// Minutes until a failed wake lets joins try again; None: never (it
    /// waits for the panel), or nothing failed.
    pub fn retry_in_minutes(&self, now: i64) -> Option<u32> {
        let f = self.failed_hold(now)?;
        (self.retry_after_minutes > 0).then(|| ((f.at + self.retry_after_minutes as i64 * 60 - now + 59) / 60).max(1) as u32)
    }

    pub fn message(&self) -> &str {
        if self.sleep_message.trim().is_empty() { SLEEP_MESSAGE } else { self.sleep_message.trim() }
    }

    pub fn load(root: &Path) -> Self {
        std::fs::read_to_string(root.join(FILE))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        let path = root.join(FILE);
        std::fs::write(&path, serde_json::to_string_pretty(self)?).with_context(|| format!("writing {}", path.display()))
    }
}

/// The names in a server's `whitelist.json` or `ops.json`, lower case.
fn names_in(root: &Path, file: &str) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(root.join(file)) else { return Vec::new() };
    let list: Vec<Value> = serde_json::from_str(&text).unwrap_or_default();
    list.iter().filter_map(|e| e["name"].as_str()).map(|n| n.to_ascii_lowercase()).collect()
}

/// Whether `player` may wake the server in `root` under `who`.
pub fn may_wake(root: &Path, who: WakeWho, player: &str) -> bool {
    let p = player.trim().to_ascii_lowercase();
    match who {
        WakeWho::Anyone => true,
        _ if p.is_empty() => false,
        WakeWho::Ops => names_in(root, "ops.json").contains(&p),
        WakeWho::Whitelist => names_in(root, "whitelist.json").contains(&p) || names_in(root, "ops.json").contains(&p),
    }
}

// ---- the last real server list answer ----------------------------------------

pub const PING_FILE: &str = ".hopper-ping.json";

/// The parts of a server list answer that tell a client whether it can
/// join (version, mod lists, chat settings); not the message or players.
const PING_KEYS: &[&str] = &["version", "isModded", "forgeData", "neoForgeData", "modinfo", "modpackData", "enforcesSecureChat", "preventsChatReports"];

/// Whether a saved ping (see [`ping_part`]) is from a server with mods that
/// clients must have: Forge, NeoForge (which reports `neoForgeData`) or old
/// FML. A modded client can't wait on the lobby island for such a server.
pub fn ping_is_modded(ping: &Value) -> bool {
    ping["isModded"].as_bool() == Some(true) || ["forgeData", "neoForgeData", "modinfo"].iter().any(|k| ping.get(*k).is_some())
}

/// What of a running server's own answer to keep for its stand-ins, or
/// None when it isn't a real one.
pub fn ping_part(status: &Value) -> Option<Value> {
    let protocol = status["version"]["protocol"].as_i64()?;
    if protocol < 0 || status["version"]["name"].as_str().is_none() {
        return None;
    }
    let mut out = serde_json::Map::new();
    for k in PING_KEYS {
        if let Some(v) = status.get(*k) {
            out.insert(k.to_string(), v.clone());
        }
    }
    Some(Value::Object(out))
}

/// Keeps a running server's answer (see [`ping_part`]) for when it sleeps;
/// writes only when it changed.
pub fn remember_ping(root: &Path, status: &Value) {
    let Some(part) = ping_part(status) else { return };
    if last_ping(root).as_ref() == Some(&part) {
        return;
    }
    if let Ok(t) = serde_json::to_string(&part) {
        let _ = std::fs::write(root.join(PING_FILE), t);
    }
}

pub fn last_ping(root: &Path) -> Option<Value> {
    std::fs::read_to_string(root.join(PING_FILE)).ok().and_then(|t| serde_json::from_str(&t).ok()).filter(|v: &Value| v.is_object())
}

/// The name part of a server's default address: "All the Mons" →
/// "all-the-mons". Spaces and underscores become hyphens, anything else
/// that can't be in an address is dropped. None when nothing is left.
pub fn address_slug(name: &str) -> Option<String> {
    let mut out = String::new();
    for c in name.trim().chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if matches!(c, ' ' | '_' | '-') && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(63);
    let out = out.trim_matches('-');
    (!out.is_empty()).then(|| out.to_string())
}

/// A server for [`effective_addresses`]: its id, name and saved settings.
pub struct Named<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub settings: &'a RouteSettings,
}

/// The addresses players can use for one server.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Effective {
    pub addresses: Vec<String>,
    /// They're the default `name.<domain>`, not saved ones.
    pub default: bool,
}

/// Every server's addresses: its saved ones, else (with a domain, and not
/// turned off) `slug.<domain>` from its name. A default another server
/// already has gets `-2`, `-3`…; servers claim defaults in id order so the
/// same one keeps the plain name across restarts. `domain` is empty when
/// defaults don't apply (no domain, or routing off).
pub fn effective_addresses(servers: &[Named], domain: &str) -> Vec<Effective> {
    let mut taken: Vec<String> = servers.iter().flat_map(|s| s.settings.addresses.iter().cloned()).collect();
    let mut out: Vec<Effective> = servers
        .iter()
        .map(|s| Effective { addresses: s.settings.addresses.clone(), default: false })
        .collect();
    if domain.is_empty() {
        return out;
    }
    let mut order: Vec<usize> = (0..servers.len()).collect();
    order.sort_by(|a, b| servers[*a].id.cmp(servers[*b].id));
    for i in order {
        let s = &servers[i];
        if s.settings.off || !s.settings.addresses.is_empty() {
            continue;
        }
        let Some(base) = address_slug(s.name) else { continue };
        let mut n = 1;
        let address = loop {
            let label = if n == 1 {
                base.clone()
            } else {
                let tail = format!("-{n}");
                format!("{}{tail}", base[..base.len().min(63 - tail.len())].trim_end_matches('-'))
            };
            let a = format!("{label}.{domain}");
            if !taken.contains(&a) {
                break a;
            }
            n += 1;
        };
        taken.push(address.clone());
        out[i] = Effective { addresses: vec![address], default: true };
    }
    out
}

/// `raw` as an address to store: lower case, no trailing dot; letters,
/// digits and hyphens per label; `*.` only at the start.
pub fn clean_address(raw: &str) -> anyhow::Result<String> {
    let a = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if a.is_empty() {
        bail!("Type an address.");
    }
    if a.len() > 253 {
        bail!("That address is too long.");
    }
    let (wild, rest) = match a.strip_prefix("*.") {
        Some(r) => (true, r),
        None => (false, a.as_str()),
    };
    let labels: Vec<&str> = rest.split('.').collect();
    if wild && labels.len() < 2 {
        bail!("A wildcard needs a domain after it, like *.mc.example.com.");
    }
    for l in &labels {
        let ok = !l.is_empty()
            && l.len() <= 63
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !l.starts_with('-')
            && !l.ends_with('-');
        if !ok {
            bail!("\"{a}\" isn't an address: use letters, digits, hyphens and dots (like survival.example.com).");
        }
    }
    Ok(a)
}

/// The address the client typed: what's before Forge's marker
/// ("host\0FML3\0"), without a trailing dot, lower case.
pub fn normalize_host(raw: &str) -> String {
    raw.split('\0').next().unwrap_or("").trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Which route serves `host`: an exact address first, else the longest
/// matching wildcard (`*.mc.example.com` matches `a.mc.example.com`, not
/// `mc.example.com`).
pub fn pick<'a, T>(host: &str, routes: &'a [(T, Vec<String>)]) -> Option<&'a T> {
    if host.is_empty() {
        return None;
    }
    if let Some((t, _)) = routes.iter().find(|(_, a)| a.iter().any(|x| x == host)) {
        return Some(t);
    }
    routes
        .iter()
        .flat_map(|(t, addrs)| addrs.iter().filter_map(move |a| a.strip_prefix('*').map(|suffix| (t, suffix))))
        .filter(|(_, suffix)| host.len() > suffix.len() && host.ends_with(suffix))
        .max_by_key(|(_, suffix)| suffix.len())
        .map(|(t, _)| t)
}

// ---- protocol ---------------------------------------------------------------------

/// What reading a packet from the start of a buffer gave.
#[derive(Debug, PartialEq)]
pub enum Read<T> {
    /// The packet and how many bytes it used.
    Done(T, usize),
    /// More bytes are needed.
    More,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Handshake {
    pub protocol: i32,
    /// As sent (Forge marker and all).
    pub raw_host: String,
    /// See [`normalize_host`].
    pub host: String,
    pub port: u16,
    /// 1: status (server list), 2: login, 3: transfer (a login too).
    pub next: i32,
}

impl Handshake {
    pub fn is_login(&self) -> bool {
        matches!(self.next, 2 | 3)
    }

    /// Whether the client says it is Forge / NeoForge: they append a marker
    /// after the address ("host\0FML3\0", "\0FORGE\0" ...). Fabric clients
    /// send nothing, so this is false for them.
    pub fn is_forge_client(&self) -> bool {
        self.raw_host.split('\0').skip(1).any(|p| p.starts_with("FML") || p.eq_ignore_ascii_case("forge"))
    }
}

/// A VarInt from `b`: (value, bytes used), `None` if `b` ends first.
pub fn read_varint(b: &[u8]) -> anyhow::Result<Option<(i32, usize)>> {
    let mut v: u32 = 0;
    for (i, byte) in b.iter().enumerate() {
        if i == 5 {
            bail!("VarInt too long");
        }
        v |= ((byte & 0x7f) as u32) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok(Some((v as i32, i + 1)));
        }
    }
    if b.len() >= 5 {
        bail!("VarInt too long");
    }
    Ok(None)
}

pub fn write_varint(out: &mut Vec<u8>, v: i32) {
    let mut v = v as u32;
    loop {
        if v & !0x7f == 0 {
            out.push(v as u8);
            return;
        }
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
}

pub fn write_string(out: &mut Vec<u8>, s: &str) {
    write_varint(out, s.len() as i32);
    out.extend_from_slice(s.as_bytes());
}

/// A string inside a packet body: (value, bytes used).
pub fn read_string(b: &[u8], max: usize) -> anyhow::Result<(String, usize)> {
    let (len, n) = read_varint(b)?.context("string cut short")?;
    let len = usize::try_from(len).context("negative string length")?;
    if len > max {
        bail!("string too long");
    }
    let s = b.get(n..n + len).context("string cut short")?;
    Ok((String::from_utf8_lossy(s).into_owned(), n + len))
}

/// A whole packet from the start of `b`: (id, body, bytes used).
pub fn read_packet(b: &[u8], max: usize) -> anyhow::Result<Read<(i32, &[u8])>> {
    let Some((len, n)) = read_varint(b)? else { return Ok(Read::More) };
    let len = usize::try_from(len).context("negative packet length")?;
    if len == 0 || len > max {
        bail!("bad packet length {len}");
    }
    let Some(packet) = b.get(n..n + len) else { return Ok(Read::More) };
    let (id, m) = read_varint(packet)?.context("packet without an id")?;
    Ok(Read::Done((id, &packet[m..]), n + len))
}

/// Whether `b` starts like the pre-1.7 server list ping (0xFE), which this
/// doesn't answer.
pub fn is_legacy_ping(b: &[u8]) -> bool {
    b.first() == Some(&0xfe)
}

/// The handshake at the start of `b`.
pub fn read_handshake(b: &[u8]) -> anyhow::Result<Read<Handshake>> {
    let (id, body, used) = match read_packet(b, MAX_HANDSHAKE)? {
        Read::More => return Ok(Read::More),
        Read::Done((id, body), used) => (id, body, used),
    };
    if id != 0 {
        bail!("not a handshake (packet {id:#x})");
    }
    let (protocol, n) = read_varint(body)?.context("handshake cut short")?;
    let (raw_host, m) = read_string(&body[n..], 255 + 64)?;
    let p = n + m;
    let port = body.get(p..p + 2).map(|x| u16::from_be_bytes([x[0], x[1]])).context("handshake cut short")?;
    let (next, _) = read_varint(&body[p + 2..])?.context("handshake cut short")?;
    Ok(Read::Done(Handshake { host: normalize_host(&raw_host), raw_host, protocol, port, next }, used))
}

/// The player name from a Login Start packet at the start of `b`.
pub fn read_login_name(b: &[u8]) -> anyhow::Result<Read<String>> {
    Ok(match read_login_start(b)? {
        Read::Done((name, _), used) => Read::Done(name, used),
        Read::More => Read::More,
    })
}

/// A Login Start packet: the name and, from 1.20.2 on, the player's id.
pub fn read_login_start(b: &[u8]) -> anyhow::Result<Read<(String, Option<[u8; 16]>)>> {
    match read_packet(b, 4096)? {
        Read::More => Ok(Read::More),
        Read::Done((0, body), used) => {
            let (name, n) = read_string(body, 64)?;
            let uuid = body.get(n..n + 16).map(|u| u.try_into().unwrap_or([0; 16]));
            Ok(Read::Done((name, uuid), used))
        }
        Read::Done((id, _), _) => bail!("not a login start (packet {id:#x})"),
    }
}

pub fn packet(id: i32, body: &[u8]) -> Vec<u8> {
    let mut inner = Vec::with_capacity(body.len() + 2);
    write_varint(&mut inner, id);
    inner.extend_from_slice(body);
    let mut out = Vec::with_capacity(inner.len() + 3);
    write_varint(&mut out, inner.len() as i32);
    out.extend_from_slice(&inner);
    out
}

/// A handshake packet (for tests and for checking a server answers).
pub fn handshake_packet(protocol: i32, host: &str, port: u16, next: i32) -> Vec<u8> {
    let mut body = Vec::new();
    write_varint(&mut body, protocol);
    write_string(&mut body, host);
    body.extend_from_slice(&port.to_be_bytes());
    write_varint(&mut body, next);
    packet(0, &body)
}

/// The status response (server list entry) packet.
pub fn status_packet(status: &Value) -> Vec<u8> {
    let mut body = Vec::new();
    write_string(&mut body, &status.to_string());
    packet(0, &body)
}

/// The answer to a status ping (same payload back).
pub fn pong_packet(payload: &[u8]) -> Vec<u8> {
    packet(1, payload)
}

/// Disconnects a player during login with `text` (a chat component).
pub fn login_disconnect_packet(text: &Value) -> Vec<u8> {
    let mut body = Vec::new();
    write_string(&mut body, &text.to_string());
    packet(0, &body)
}

// ---- what players see ----------------------------------------------------------

/// Why the panel answers for a server itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Standin {
    /// Stopped, and joining starts it.
    Sleeping,
    /// Stopped, and it doesn't wake on join (sleep is off).
    Stopped,
    /// Stopped, and the panel can't start it (another tool runs it).
    Offline,
    Starting,
    Stopping,
    Crashed,
    /// A join woke it, but it didn't come up: joining doesn't start it
    /// again until someone starts it in the panel (or the retry time).
    WakeFailed,
    /// No server has this address.
    Unknown,
}

impl Standin {
    /// Its name in the answer's `hopper.state`, which tells Hopper's own
    /// pings a stand-in from the server.
    pub fn key(self) -> &'static str {
        match self {
            Standin::Sleeping => "sleeping",
            Standin::Stopped => "stopped",
            Standin::Offline => "offline",
            Standin::Starting => "starting",
            Standin::Stopping => "stopping",
            Standin::Crashed => "crashed",
            Standin::WakeFailed => "wake_failed",
            Standin::Unknown => "unknown",
        }
    }
}

/// "about 2 min" from the last start's length.
pub fn wake_estimate(last_start_secs: Option<u32>) -> String {
    match last_start_secs {
        None => "a minute or two".into(),
        Some(s) if s < 50 => "under a minute".into(),
        Some(s) => format!("about {} min", (s + 30) / 60),
    }
}

/// Bars in the server list's progress bar. "|" is 2 px wide in Minecraft's
/// font and crisp; block characters fall back to the blurry unicode font.
const BAR: usize = 40;
const NO_ID: &str = "00000000-0000-0000-0000-000000000000";

/// A start's line in the server list: bar, percent, what it is doing.
fn boot_line(b: &BootStatus) -> Vec<Value> {
    let color = if b.slow { "gold" } else { "aqua" };
    let filled = (b.percent as usize * BAR + 50) / 100;
    vec![
        json!({ "text": "|".repeat(filled), "color": color }),
        json!({ "text": "|".repeat(BAR - filled), "color": "dark_gray" }),
        json!({ "text": format!(" {}%", b.percent), "color": color }),
        json!({ "text": format!(" {}", b.label), "color": "gray" }),
    ]
}

/// The right-hand label of a starting server (the client shows it in red
/// unless it carries its own colour code).
fn boot_label(b: &BootStatus) -> String {
    match b.left_secs {
        _ if b.slow => "§6Taking longer".into(),
        Some(l) => format!("§b~{} left", short_time(l)),
        None => format!("§bStarting {}%", b.percent),
    }
}

/// The lines shown on hovering the label: the steps and the times.
fn boot_hover(b: &BootStatus) -> Vec<String> {
    let mut out: Vec<String> = b
        .steps
        .iter()
        .map(|s| match s.state {
            StepState::Done => format!("§a✔ {}", s.label),
            StepState::Now => format!("{}▶ {}", if b.slow { "§6" } else { "§b" }, s.label),
            StepState::Next => format!("§8· {}", s.label),
        })
        .collect();
    let so_far = format!("{} so far", short_time(b.elapsed_secs));
    out.push(match (b.slow, b.left_secs, b.usual_secs) {
        (true, _, Some(u)) => format!("§6{so_far} · taking longer than usual ({})", short_time(u)),
        (_, Some(l), Some(u)) => format!("§7{so_far} · about {} left (usually {})", short_time(l), short_time(u)),
        _ => format!("§7{so_far} · first start: no estimate yet"),
    });
    out
}

/// The server list entry for a server that isn't up. Protocol -1 makes the
/// client show `version` (in red) where the ping bars go. A starting server
/// shows how far it has got (`boot`); players see it again on Refresh.
/// A sleeping one shows `message` and, with `real` (its own last answer,
/// see [`last_ping`]), its real version and mods, so the client lists it
/// as joinable.
#[allow(clippy::too_many_arguments)]
pub fn standin_status(
    kind: Standin,
    motd: &str,
    last_start_secs: Option<u32>,
    max_players: u32,
    favicon: Option<&str>,
    boot: Option<&BootStatus>,
    message: &str,
    real: Option<&Value>,
) -> Value {
    let (label, color, rest) = match kind {
        Standin::Sleeping => ("Sleeping", "yellow", format!("join to wake it ({})", wake_estimate(last_start_secs))),
        Standin::Stopped => ("Offline", "gray", "the server is stopped".into()),
        Standin::Offline => ("Offline", "gray", "started from another panel".into()),
        Standin::Starting => ("Starting…", "aqua", "try again in a moment".into()),
        Standin::Stopping => ("Stopping", "gold", "back soon".into()),
        Standin::Crashed => ("Crashed", "red", "the owner has been told".into()),
        Standin::WakeFailed => ("Failed to start", "red", "ask an admin".into()),
        Standin::Unknown => ("No server at this address", "red", "check the address with whoever runs the server".into()),
    };
    let boot = boot.filter(|_| kind == Standin::Starting);
    let mut extra = match boot {
        Some(b) => boot_line(b),
        None if kind == Standin::Sleeping => {
            let m = if message.trim().is_empty() { SLEEP_MESSAGE } else { message.trim() };
            vec![json!({ "text": m, "color": color }), json!({ "text": format!(" ({})", wake_estimate(last_start_secs)), "color": "gray" })]
        }
        None => vec![json!({ "text": format!("● {label}"), "color": color }), json!({ "text": format!(" · {rest}"), "color": "gray" })],
    };
    if !motd.trim().is_empty() && kind != Standin::Unknown {
        extra.insert(0, json!({ "text": format!("{}\n", first_line(motd)), "color": "white" }));
    }
    let version = match boot {
        Some(b) => boot_label(b),
        None if kind == Standin::Starting => "Starting".into(),
        None if kind == Standin::WakeFailed => "Couldn't start".into(),
        None => label.to_string(),
    };
    let mut hover = boot.map(boot_hover).unwrap_or_default();
    if kind == Standin::Sleeping {
        hover = vec!["§eSleeping".into(), format!("§7Join to wake it: {}", wake_estimate(last_start_secs))];
    }
    let sample: Vec<Value> = hover.into_iter().map(|name| json!({ "name": name, "id": NO_ID })).collect();
    let mut v = json!({
        "version": { "name": version, "protocol": -1 },
        "players": { "max": max_players, "online": 0, "sample": sample },
        "description": { "text": "", "extra": extra },
        "hopper": { "state": kind.key() },
    });
    // Asleep, it answers as the server would: same version and mod list.
    if kind == Standin::Sleeping
        && let Some(Value::Object(real)) = real
        && real.get("version").is_some_and(|x| x["protocol"].as_i64().is_some_and(|p| p >= 0))
    {
        for (k, x) in real {
            v[k] = x.clone();
        }
    }
    if let Some(f) = favicon {
        v["favicon"] = json!(f);
    }
    v
}

/// Whether a server list answer is a stand-in from Hopper Server's router
/// rather than the server itself.
pub fn is_standin(v: &Value) -> bool {
    v["version"]["protocol"].as_i64() == Some(-1) || v["hopper"]["state"].is_string()
}

/// What a player who may not wake it is told.
pub fn refused_kick(name: &str, who: WakeWho) -> Value {
    let list = if who == WakeWho::Ops { "ops" } else { "whitelisted players" };
    json!({ "text": "", "extra": [
        { "text": format!("{name} is asleep."), "color": "yellow" },
        { "text": format!("\n\nOnly {list} can wake it. Ask an admin."), "color": "gray" },
    ] })
}

/// What a player joining after a failed wake is told.
pub fn wake_failed_kick(name: &str, retry_in_minutes: Option<u32>) -> Value {
    let rest = match retry_in_minutes {
        Some(m) => format!("Ask an admin, or join again in {m} min to try once more."),
        None => "Ask an admin to start it from the panel.".into(),
    };
    json!({ "text": "", "extra": [
        { "text": format!("{name} couldn't start."), "color": "red" },
        { "text": format!("\n\n{rest}"), "color": "gray" },
    ] })
}

/// What a player joining a server that isn't up is told.
pub fn standin_kick(kind: Standin, name: &str, last_start_secs: Option<u32>, boot: Option<&BootStatus>) -> Value {
    let (head, color, rest) = match kind {
        Standin::Starting if let Some(b) = boot => (
            format!("{name} is starting: {}%, {}.", b.percent, b.label.to_lowercase()),
            if b.slow { "gold" } else { "aqua" },
            match b.left_secs {
                _ if b.slow => "It's taking longer than usual. Join again in a moment.".into(),
                Some(l) => format!("About {} left. Join again then.", short_time(l)),
                None => "Join again in a moment.".into(),
            },
        ),
        Standin::Sleeping => (format!("Starting {name}…"), "yellow", format!("Join again in {}.", wake_estimate(last_start_secs))),
        Standin::WakeFailed => return wake_failed_kick(name, None),
        Standin::Starting => (format!("{name} is starting."), "aqua", "Join again in a moment.".into()),
        Standin::Stopping => (format!("{name} is stopping."), "gold", "Try again in a minute.".into()),
        Standin::Crashed => (format!("{name} crashed."), "red", "The owner can see what happened in the panel.".into()),
        Standin::Stopped => (format!("{name} is offline."), "gray", "Ask the owner to start it.".into()),
        Standin::Offline => (format!("{name} is offline."), "gray", "It's started from another panel.".into()),
        Standin::Unknown => ("No server at this address.".into(), "red", "Check the address with whoever runs the server.".into()),
    };
    json!({ "text": "", "extra": [ { "text": head, "color": color }, { "text": format!("\n\n{rest}"), "color": "gray" } ] })
}

/// The first line of a server.properties MOTD, without colour codes.
fn first_line(motd: &str) -> String {
    let line = motd.replace("\\n", "\n");
    let line = line.lines().next().unwrap_or("");
    let mut out = String::new();
    let mut skip = false;
    for c in line.chars() {
        if skip {
            skip = false;
        } else if c == '§' {
            skip = true;
        } else {
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// `server-icon.png` as the data URL the server list shows.
pub fn favicon(root: &Path) -> Option<String> {
    use base64::Engine as _;
    let png = std::fs::read(root.join("server-icon.png")).ok()?;
    (png.len() < 64 * 1024).then(|| format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_round_trip() {
        for v in [0, 1, 127, 128, 255, 25565, 2_097_151, i32::MAX, -1] {
            let mut b = Vec::new();
            write_varint(&mut b, v);
            assert_eq!(read_varint(&b).unwrap(), Some((v, b.len())), "{v}");
        }
        assert_eq!(read_varint(&[0x80]).unwrap(), None);
        assert!(read_varint(&[0xff; 6]).is_err());
    }

    #[test]
    fn reads_handshakes_whole_or_in_pieces() {
        let mut p = handshake_packet(767, "Survival.MC.example.com.\0FML3\0", 25565, 2);
        p.extend_from_slice(&[9, 9]); // the next packet's start
        for cut in 0..p.len() - 2 {
            assert_eq!(read_handshake(&p[..cut]).unwrap(), Read::More, "cut at {cut}");
        }
        let Read::Done(h, used) = read_handshake(&p).unwrap() else { panic!() };
        assert_eq!(used, p.len() - 2);
        assert_eq!((h.protocol, h.port, h.next), (767, 25565, 2));
        assert_eq!(h.host, "survival.mc.example.com");
        assert!(h.raw_host.ends_with("\0FML3\0") && h.is_login() && h.is_forge_client());
        let plain = Handshake { protocol: 767, raw_host: "mc.example.com".into(), host: "mc.example.com".into(), port: 25565, next: 2 };
        assert!(!plain.is_forge_client());
        assert!(Handshake { raw_host: "a.b\0FORGE\0".into(), ..plain.clone() }.is_forge_client());
        assert!(read_handshake(&[3, 5, 0, 0]).is_err(), "not packet 0");
        assert!(read_handshake(&[0]).is_err(), "empty packet");
        assert!(is_legacy_ping(&[0xfe, 0x01]));
    }

    #[test]
    fn reads_login_name() {
        let mut body = Vec::new();
        write_string(&mut body, "Steve");
        body.extend_from_slice(&[0u8; 16]); // uuid (1.20.2+)
        let p = packet(0, &body);
        assert_eq!(read_login_name(&p).unwrap(), Read::Done("Steve".to_string(), p.len()));
        assert_eq!(read_login_name(&p[..3]).unwrap(), Read::More);
        assert_eq!(read_login_start(&p).unwrap(), Read::Done(("Steve".to_string(), Some([0; 16])), p.len()));
    }

    #[test]
    fn picks_exact_then_longest_wildcard() {
        let routes = vec![
            ("all", vec!["*.example.com".to_string()]),
            ("mc", vec!["*.mc.example.com".to_string()]),
            ("survival", vec!["survival.mc.example.com".to_string(), "smp.example.com".to_string()]),
        ];
        assert_eq!(pick("survival.mc.example.com", &routes), Some(&"survival"));
        assert_eq!(pick("smp.example.com", &routes), Some(&"survival"));
        assert_eq!(pick("creative.mc.example.com", &routes), Some(&"mc"));
        assert_eq!(pick("x.example.com", &routes), Some(&"all"));
        assert_eq!(pick("mc.example.com", &routes), Some(&"all"), "*.mc… doesn't match mc… itself");
        assert_eq!(pick("example.com", &routes), None);
        assert_eq!(pick("192.168.50.3", &routes), None);
        assert_eq!(pick("", &routes), None);
    }

    #[test]
    fn cleans_addresses() {
        assert_eq!(clean_address(" Survival.Example.COM. ").unwrap(), "survival.example.com");
        assert_eq!(clean_address("*.mc.example.com").unwrap(), "*.mc.example.com");
        for bad in ["", "a..b", "-a.com", "a b.com", "*.com", "x.*.com", "a_b.com"] {
            assert!(clean_address(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn standins_say_what_is_going_on() {
        let s = standin_status(Standin::Sleeping, "§aMy §lSMP\\nline two", Some(130), 20, None, None, "", None);
        assert_eq!(s["version"]["protocol"], -1);
        let text = s["description"]["extra"].as_array().unwrap().iter().map(|e| e["text"].as_str().unwrap()).collect::<String>();
        assert_eq!(text, "My SMP\n💤 Sleeping · join to wake it (about 2 min)");
        let k = standin_kick(Standin::Sleeping, "ATM10", None, None).to_string();
        assert!(k.contains("Starting ATM10") && k.contains("Join again in a minute or two."), "{k}");
        assert_eq!(wake_estimate(Some(20)), "under a minute");
        let u = standin_status(Standin::Unknown, "ignored", None, 0, None, None, "", None).to_string();
        assert!(u.contains("No server at this address") && !u.contains("ignored"));
    }

    #[test]
    fn starting_servers_show_their_progress() {
        use crate::hosting::boot::{Boot, BootRecord};
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let mut b = Boot::starting(t0);
        b.push("[00:00:01] [main/INFO] [Launcher/MODLAUNCHER]: ModLauncher running: args []", t0 + Duration::from_secs(1));
        b.push("[00:00:20] [main/INFO] [x/]: Launching target 'forgeserver' with arguments [nogui]", t0 + Duration::from_secs(20));
        let last = BootRecord { secs: 100.0, marks: vec![Some(0.0), Some(1.0), Some(5.0), Some(20.0), Some(60.0), Some(90.0), Some(95.0)], mods: None };
        let st = b.status(t0 + Duration::from_secs(40), Some(&last));
        assert_eq!((st.percent, st.left_secs), (40, Some(60)));
        let s = standin_status(Standin::Starting, "My Pack", None, 20, None, Some(&st), "", None);
        let text = s["description"]["extra"].as_array().unwrap().iter().map(|e| e["text"].as_str().unwrap()).collect::<String>();
        assert_eq!(text, format!("My Pack\n{}{} 40% Loading mods", "|".repeat(16), "|".repeat(24)));
        assert_eq!(s["version"]["name"], "§b~1m left");
        let hover: Vec<_> = s["players"]["sample"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(hover.first().map(String::as_str), Some("§a✔ Starting Java"));
        assert!(hover.contains(&"§b▶ Loading mods".to_string()), "{hover:?}");
        assert!(hover.contains(&"§8· Loading data".to_string()), "{hover:?}");
        assert_eq!(hover.last().unwrap(), "§740s so far · about 1m left (usually 1m 40s)");
        let k = standin_kick(Standin::Starting, "ATM10", None, Some(&st)).to_string();
        assert!(k.contains("ATM10 is starting: 40%, loading mods.") && k.contains("About 1m left"), "{k}");
        // No progress (up, but not answering yet): as before.
        let s = standin_status(Standin::Starting, "", None, 20, None, None, "", None);
        assert_eq!(s["version"]["name"], "Starting");
        // Past the last start's time.
        let slow = b.status(t0 + Duration::from_secs(400), Some(&last));
        let s = standin_status(Standin::Starting, "", None, 20, None, Some(&slow), "", None);
        assert_eq!(s["version"]["name"], "§6Taking longer");
        // Other kinds ignore a stray progress.
        let s = standin_status(Standin::Sleeping, "", None, 20, None, Some(&st), "", None);
        assert_eq!(s["version"]["name"], "Sleeping");
    }

    #[test]
    fn settings_round_trip() {
        let dir = std::env::temp_dir().join(format!("hopper-route-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(RouteSettings::load(&dir), RouteSettings::default());
        let s = RouteSettings { addresses: vec!["a.example.com".into()], sleep_after_minutes: 15, last_start_secs: Some(40), wake_who: WakeWho::Ops, ..Default::default() };
        s.save(&dir).unwrap();
        assert_eq!(RouteSettings::load(&dir), s);
        // Older files have no `off`; a turned-off server keeps it.
        let off = RouteSettings { off: true, ..Default::default() };
        off.save(&dir).unwrap();
        assert_eq!(RouteSettings::load(&dir), off);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wake_settings_default_on_and_old_files_keep_working() {
        // A file from before wake settings: waking stays on.
        let s: RouteSettings = serde_json::from_str(r#"{"addresses":[],"sleep_after_minutes":15}"#).unwrap();
        assert!(s.wake_on_join && s.wake_who == WakeWho::Anyone && s.wakes(0));
        assert_eq!(s.message(), SLEEP_MESSAGE);
        let off = RouteSettings { sleep_after_minutes: 0, ..s.clone() };
        assert!(!off.wakes(0), "no sleep, no wake");
        assert_eq!(s.while_starting, WhileStarting::Lobby, "the lobby by default");
        assert_eq!(s.modded_players, ModdedPlayers::Kick, "modded players get the message by default");
        let w: RouteSettings = serde_json::from_str(r#"{"modded_players":"wait"}"#).unwrap();
        assert_eq!(w.modded_players, ModdedPlayers::Wait);
        let t: RouteSettings = serde_json::from_str(r#"{"sleep_after_minutes":5,"wake_who":"whitelist","wake_on_join":false,"while_starting":"kick"}"#).unwrap();
        assert_eq!(t.while_starting, WhileStarting::Kick);
        assert_eq!((t.wake_who, t.wakes(0)), (WakeWho::Whitelist, false));
    }

    #[test]
    fn failed_wakes_hold_until_the_panel_or_the_retry_time() {
        let failed = Some(WakeFailed { at: 1000, by: "Alex".into(), why: "crashed while starting".into() });
        let s = RouteSettings { sleep_after_minutes: 15, wake_failed: failed.clone(), ..Default::default() };
        assert!(!s.wakes(1000) && !s.wakes(1_000_000), "0: waits for the panel");
        assert_eq!(s.retry_in_minutes(1000), None);
        let r = RouteSettings { retry_after_minutes: 10, ..s };
        assert!(!r.wakes(1000 + 9 * 60));
        assert_eq!(r.retry_in_minutes(1000 + 9 * 60 + 30), Some(1));
        assert_eq!(r.retry_in_minutes(1000), Some(10));
        assert!(r.wakes(1000 + 10 * 60));
        let k = wake_failed_kick("ATM10", Some(3)).to_string();
        assert!(k.contains("ATM10 couldn't start.") && k.contains("join again in 3 min"), "{k}");
        assert!(wake_failed_kick("ATM10", None).to_string().contains("start it from the panel"));
    }

    #[test]
    fn who_may_wake() {
        let dir = std::env::temp_dir().join(format!("hopper-wake-who-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("whitelist.json"), r#"[{"uuid":"1","name":"Alex"}]"#).unwrap();
        std::fs::write(dir.join("ops.json"), r#"[{"uuid":"2","name":"OpSteve","level":4}]"#).unwrap();
        assert!(may_wake(&dir, WakeWho::Anyone, ""));
        assert!(may_wake(&dir, WakeWho::Whitelist, "alex") && may_wake(&dir, WakeWho::Whitelist, "OPSTEVE"));
        assert!(!may_wake(&dir, WakeWho::Whitelist, "Stranger") && !may_wake(&dir, WakeWho::Whitelist, ""));
        assert!(may_wake(&dir, WakeWho::Ops, "OpSteve") && !may_wake(&dir, WakeWho::Ops, "Alex"));
        let _ = std::fs::remove_file(dir.join("ops.json"));
        assert!(!may_wake(&dir, WakeWho::Ops, "OpSteve"), "no ops file: nobody");
        assert!(refused_kick("ATM10", WakeWho::Ops).to_string().contains("Only ops can wake it"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn neoforge_pings_count_as_modded() {
        let neo = json!({ "version": { "name": "1.21.1", "protocol": 767 }, "neoForgeData": { "channels": [], "mods": [] } });
        assert!(ping_part(&neo).unwrap().get("neoForgeData").is_some(), "kept for later");
        assert!(ping_is_modded(&ping_part(&neo).unwrap()));
        assert!(ping_is_modded(&json!({ "forgeData": {} })));
        assert!(ping_is_modded(&json!({ "modinfo": {} })));
        assert!(ping_is_modded(&json!({ "isModded": true })));
        assert!(!ping_is_modded(&json!({ "version": { "name": "1.21.1", "protocol": 767 } })), "vanilla");
    }

    #[test]
    fn sleeping_servers_answer_with_their_real_version() {
        let real = json!({
            "version": { "name": "NeoForge 1.21.1", "protocol": 767 },
            "players": { "max": 12, "online": 3 },
            "description": { "text": "Live MOTD" },
            "isModded": true,
            "forgeData": { "d": "abc" },
        });
        let part = ping_part(&real).unwrap();
        assert!(part.get("players").is_none() && part.get("description").is_none());
        assert_eq!(part["forgeData"]["d"], "abc");
        assert!(ping_part(&json!({ "version": { "name": "Sleeping", "protocol": -1 } })).is_none(), "a stand-in isn't kept");
        let dir = std::env::temp_dir().join(format!("hopper-ping-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(last_ping(&dir), None);
        remember_ping(&dir, &real);
        assert_eq!(last_ping(&dir), Some(part.clone()));
        let _ = std::fs::remove_dir_all(&dir);

        let s = standin_status(Standin::Sleeping, "My SMP", Some(130), 12, None, None, "Zzz, join me", Some(&part));
        assert_eq!(s["version"], json!({ "name": "NeoForge 1.21.1", "protocol": 767 }));
        assert_eq!((s["isModded"].as_bool(), s["players"]["online"].as_i64()), (Some(true), Some(0)));
        let text = s["description"]["extra"].as_array().unwrap().iter().map(|e| e["text"].as_str().unwrap()).collect::<String>();
        assert_eq!(text, "My SMP\nZzz, join me (about 2 min)");
        assert!(is_standin(&s), "Hopper still sees it's asleep");
        assert!(!is_standin(&real));
        // Failed: keeps the red label so nobody waits on it.
        let f = standin_status(Standin::WakeFailed, "My SMP", None, 12, None, None, "", Some(&part));
        assert_eq!((f["version"]["protocol"].as_i64(), f["version"]["name"].as_str()), (Some(-1), Some("Couldn't start")));
        assert_eq!(f["hopper"]["state"], "wake_failed");
        let text = f["description"]["extra"].as_array().unwrap().iter().map(|e| e["text"].as_str().unwrap()).collect::<String>();
        assert_eq!(text, "My SMP\n● Failed to start · ask an admin");
    }

    #[test]
    fn encodes_answers_as_packets() {
        let s = status_packet(&json!({ "a": 1 }));
        let Read::Done((id, body), used) = read_packet(&s, 1 << 16).unwrap() else { panic!() };
        assert_eq!((id, used), (0, s.len()));
        assert_eq!(read_string(body, 1 << 16).unwrap().0, r#"{"a":1}"#);
        let p = pong_packet(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(p, [9, 1, 1, 2, 3, 4, 5, 6, 7, 8]);
        let k = login_disconnect_packet(&json!({ "text": "hi" }));
        let Read::Done((0, body), _) = read_packet(&k, 1 << 16).unwrap() else { panic!() };
        assert_eq!(read_string(body, 99).unwrap().0, r#"{"text":"hi"}"#);
    }

    #[test]
    fn address_slugs() {
        assert_eq!(address_slug("all the mons").as_deref(), Some("all-the-mons"));
        assert_eq!(address_slug("TNP-LL8").as_deref(), Some("tnp-ll8"));
        assert_eq!(address_slug("  Val's_Pack!! ").as_deref(), Some("vals-pack"));
        assert_eq!(address_slug("-- a  -  b --").as_deref(), Some("a-b"));
        assert_eq!(address_slug("Ünïcode"), Some("ncode".into()));
        assert_eq!(address_slug("!!!"), None);
        assert_eq!(address_slug(&"x".repeat(80)).unwrap().len(), 63);
        // Cut at 63 never leaves a trailing hyphen.
        let s = address_slug(&format!("{} b", "a".repeat(62))).unwrap();
        assert_eq!(s, "a".repeat(62));
    }

    fn eff(list: &[(&str, &str, RouteSettings)], domain: &str) -> Vec<Effective> {
        let named: Vec<Named> = list.iter().map(|(id, name, s)| Named { id, name, settings: s }).collect();
        effective_addresses(&named, domain)
    }

    #[test]
    fn default_addresses() {
        let none = RouteSettings::default();
        let saved = RouteSettings { addresses: vec!["tnp.kyuubi.dev".into()], ..Default::default() };
        let off = RouteSettings { off: true, ..Default::default() };
        let list = [("bcg", "BCG", none.clone()), ("tnp-ll8", "TNP-LL8", saved), ("val", "Val", off)];
        let e = eff(&list, "kyuubi.dev");
        assert_eq!(e[0], Effective { addresses: vec!["bcg.kyuubi.dev".into()], default: true });
        // A saved address wins over the default.
        assert_eq!(e[1], Effective { addresses: vec!["tnp.kyuubi.dev".into()], default: false });
        // Turned off: nothing.
        assert_eq!(e[2], Effective::default());
        // No domain (or routing off): only saved addresses.
        let e = eff(&list, "");
        assert!(e[0].addresses.is_empty() && e[1].addresses.len() == 1);
    }

    #[test]
    fn default_address_clashes() {
        let none = RouteSettings::default();
        // Listed in any order, the lower id keeps the plain name.
        let list = [("all-the-mons-2", "All The Mons", none.clone()), ("all-the-mons", "all the mons", none.clone())];
        let e = eff(&list, "mc.test");
        assert_eq!(e[1].addresses, ["all-the-mons.mc.test"]);
        assert_eq!(e[0].addresses, ["all-the-mons-2.mc.test"]);
        // A saved address elsewhere pushes the default along.
        let mine = RouteSettings { addresses: vec!["bcg.mc.test".into()], ..Default::default() };
        let list = [("a", "Other", mine), ("bcg", "BCG", none.clone())];
        assert_eq!(eff(&list, "mc.test")[1].addresses, ["bcg-2.mc.test"]);
        // A long name still fits in one label with its number.
        let long = "x".repeat(70);
        let list = [("a", long.as_str(), none.clone()), ("b", long.as_str(), none)];
        let e = eff(&list, "mc.test");
        let label = e[1].addresses[0].split('.').next().unwrap().to_string();
        assert!(label.len() <= 63 && label.ends_with("-2"));
        assert!(clean_address(&e[1].addresses[0]).is_ok());
    }
}
