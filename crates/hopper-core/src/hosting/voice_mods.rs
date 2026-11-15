//! Proximity voice chat mods on a server behind the VPS relay
//! (design/vps-relay.md, B6): which one the server runs, the UDP port it
//! listens on at home, and the address it tells players to send voice to.
//!
//! Behind the relay, players can't reach home's voice port (home opens no
//! port), so the address the mod advertises is set to the relay's public
//! address and UDP port; the relay passes the packets home over the
//! connection home dialled out. These functions read and change the mod's
//! config text; Hopper Server writes the files (recorded in History) and
//! keeps what they held before, so "Stop using the relay" and "Play at
//! home" put the old values back.
//!
//! - **Simple Voice Chat**: `config/voicechat/voicechat-server.properties`,
//!   `port` (default 24454, `-1` the Minecraft port), `bind_address`
//!   (blank: `server-ip`), `voice_host` (what clients use; may carry a port).
//! - **Plasmo Voice** (2.x): `config/plasmovoice/config.toml`, `[host]` `ip`
//!   and `port` (0: the Minecraft port), `[host.public]` `ip` and `port`
//!   (what clients use; the table is commented out by default).
//!
//! No I/O here.

use std::net::IpAddr;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::properties::Properties;

pub const SVC_CONFIG: &str = "config/voicechat/voicechat-server.properties";
pub const PLASMO_CONFIG: &str = "config/plasmovoice/config.toml";
/// Simple Voice Chat's own default port; the relay listens on the same.
pub const SVC_DEFAULT_PORT: u16 = 24454;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VoiceMod {
    SimpleVoiceChat,
    PlasmoVoice,
}

impl VoiceMod {
    pub fn name(self) -> &'static str {
        match self {
            VoiceMod::SimpleVoiceChat => "Simple Voice Chat",
            VoiceMod::PlasmoVoice => "Plasmo Voice",
        }
    }

    pub fn config_rel(self) -> &'static str {
        match self {
            VoiceMod::SimpleVoiceChat => SVC_CONFIG,
            VoiceMod::PlasmoVoice => PLASMO_CONFIG,
        }
    }

    /// A jar in `mods/` that is this mod (turned-off jars don't count).
    pub fn jar(self, file_name: &str) -> bool {
        let f = file_name.to_ascii_lowercase();
        if !f.ends_with(".jar") {
            return false;
        }
        match self {
            VoiceMod::SimpleVoiceChat => f.starts_with("voicechat-") || f.starts_with("simple-voice-chat") || f.starts_with("simplevoicechat"),
            VoiceMod::PlasmoVoice => f.starts_with("plasmovoice") || f.starts_with("plasmo-voice") || f.starts_with("plasmo_voice"),
        }
    }
}

/// A voice mod found on a server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detected {
    pub kind: VoiceMod,
    /// The UDP port it listens on at home.
    pub port: u16,
    /// Where it listens, when that is one address other than "every
    /// address" or loopback (home sends the players' packets there).
    pub bind: Option<IpAddr>,
}

/// The voice mod in server folder `root`, from its jar in `mods/` (an
/// enabled one) and its config. `server_port` is the Minecraft port, which
/// both mods can share. Simple Voice Chat wins when both are there.
pub fn detect(root: &Path, server_port: u16) -> Option<Detected> {
    let jars: Vec<String> = std::fs::read_dir(root.join("mods")).into_iter().flatten().flatten().filter_map(|d| d.file_name().to_str().map(str::to_string)).collect();
    for kind in [VoiceMod::SimpleVoiceChat, VoiceMod::PlasmoVoice] {
        if !jars.iter().any(|j| kind.jar(j)) {
            continue;
        }
        let text = std::fs::read_to_string(root.join(kind.config_rel())).ok();
        return Some(read_config(kind, text.as_deref(), server_port));
    }
    None
}

/// Port and bind address from a voice mod's config (`None`: not written
/// yet, the mod's defaults).
pub fn read_config(kind: VoiceMod, text: Option<&str>, server_port: u16) -> Detected {
    let specific = |ip: &str| -> Option<IpAddr> {
        let ip: IpAddr = ip.trim().parse().ok()?;
        (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
    };
    match kind {
        VoiceMod::SimpleVoiceChat => {
            let p = Properties::parse(text.unwrap_or(""));
            let port = match p.get("port").and_then(|v| v.trim().parse::<i32>().ok()) {
                Some(-1) => server_port,
                Some(n) if (1..=65535).contains(&n) => n as u16,
                _ => SVC_DEFAULT_PORT,
            };
            // Blank means server.properties' server-ip, which is 127.0.0.1
            // behind the relay; "*" is every address.
            let bind = p.get("bind_address").and_then(specific);
            Detected { kind, port, bind }
        }
        VoiceMod::PlasmoVoice => {
            let t = text.unwrap_or("");
            let port = toml_get(t, "host", "port").and_then(|v| v.parse::<u32>().ok()).filter(|n| (1..=65535).contains(n)).map_or(server_port, |n| n as u16);
            let bind = toml_get(t, "host", "ip").and_then(|v| specific(&unquote(&v)));
            Detected { kind, port, bind }
        }
    }
}

/// What a config held before the relay set the advertised address, so it
/// goes back exactly.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceBefore {
    pub kind: Option<VoiceMod>,
    /// Simple Voice Chat's `voice_host`, or Plasmo's `[host.public]` `ip`,
    /// as written (`None`: the key wasn't there).
    pub host: Option<String>,
    /// Plasmo's `[host.public]` `port`, as written.
    pub port: Option<String>,
    /// Plasmo: the `[host.public]` table wasn't there.
    pub made_table: bool,
    /// The values the relay wrote, so a later hand edit is left alone.
    pub set_host: String,
    pub set_port: String,
}

/// A change for History: key, old value, new value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceChange {
    pub key: String,
    pub was: Option<String>,
    pub now: Option<String>,
}

/// Points the mod's advertised address at `host:port` (the relay). Returns
/// the new config text (`None` when it already says so), what it held
/// before, and the changes.
pub fn apply(kind: VoiceMod, text: Option<&str>, host: &str, port: u16) -> (Option<String>, VoiceBefore, Vec<VoiceChange>) {
    let t = text.unwrap_or("");
    match kind {
        VoiceMod::SimpleVoiceChat => {
            let mut p = Properties::parse(t);
            let want = format!("{host}:{port}");
            let was = p.get("voice_host").map(str::to_string);
            let before = VoiceBefore { kind: Some(kind), host: was.clone(), set_host: want.clone(), ..Default::default() };
            if was.as_deref() == Some(want.as_str()) {
                return (None, before, Vec::new());
            }
            p.set("voice_host", want.clone());
            (Some(p.to_text()), before, vec![VoiceChange { key: "voice_host".into(), was, now: Some(want) }])
        }
        VoiceMod::PlasmoVoice => {
            let ip = format!("\"{}\"", host.replace(['"', '\\'], ""));
            let port_s = port.to_string();
            let had = toml_table_at(t, "host.public").is_some();
            let was_ip = toml_get(t, "host.public", "ip");
            let was_port = toml_get(t, "host.public", "port");
            let before = VoiceBefore { kind: Some(kind), host: was_ip.clone(), port: was_port.clone(), made_table: !had, set_host: ip.clone(), set_port: port_s.clone() };
            if was_ip.as_deref() == Some(ip.as_str()) && was_port.as_deref() == Some(port_s.as_str()) {
                return (None, before, Vec::new());
            }
            let mut out = toml_set(t, "host.public", "ip", &ip);
            out = toml_set(&out, "host.public", "port", &port_s);
            let changes = vec![
                VoiceChange { key: "host.public.ip".into(), was: was_ip.map(|v| unquote(&v)), now: Some(host.to_string()) },
                VoiceChange { key: "host.public.port".into(), was: was_port, now: Some(port_s) },
            ];
            (Some(out), before, changes)
        }
    }
}

/// Puts back what [`apply`] changed. A value someone changed by hand since
/// (no longer the relay's) is left as it is. `None`: nothing to change.
pub fn restore(text: &str, b: &VoiceBefore) -> (Option<String>, Vec<VoiceChange>) {
    let Some(kind) = b.kind else { return (None, Vec::new()) };
    match kind {
        VoiceMod::SimpleVoiceChat => {
            let mut p = Properties::parse(text);
            let now = p.get("voice_host").map(str::to_string);
            if now.as_deref() != Some(b.set_host.as_str()) || now == b.host {
                return (None, Vec::new());
            }
            match &b.host {
                Some(h) => p.set("voice_host", h.clone()),
                None => p.remove("voice_host"),
            }
            (Some(p.to_text()), vec![VoiceChange { key: "voice_host".into(), was: now, now: b.host.clone() }])
        }
        VoiceMod::PlasmoVoice => {
            let ip = toml_get(text, "host.public", "ip");
            let port = toml_get(text, "host.public", "port");
            if ip.as_deref() != Some(b.set_host.as_str()) || port.as_deref() != Some(b.set_port.as_str()) {
                return (None, Vec::new());
            }
            let out = if b.made_table {
                toml_drop_table(text, "host.public")
            } else {
                let mut o = match &b.host {
                    Some(v) => toml_set(text, "host.public", "ip", v),
                    None => toml_remove(text, "host.public", "ip"),
                };
                o = match &b.port {
                    Some(v) => toml_set(&o, "host.public", "port", v),
                    None => toml_remove(&o, "host.public", "port"),
                };
                o
            };
            let changes = vec![
                VoiceChange { key: "host.public.ip".into(), was: ip.map(|v| unquote(&v)), now: b.host.as_ref().map(|v| unquote(v)) },
                VoiceChange { key: "host.public.port".into(), was: port, now: b.port.clone() },
            ];
            (Some(out), changes)
        }
    }
}

// ---- a little TOML: one table's plain `key = value` lines ----------------------------------
//
// Enough for Plasmo's config, keeping every other line (comments included)
// as it was. Values are kept as written (`"text"`, `25565`).

fn eol(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// The table a `[name]` line opens, if the line is one.
fn header(line: &str) -> Option<&str> {
    let t = line.trim();
    if t.starts_with("[[") {
        return None;
    }
    let inner = t.strip_prefix('[')?;
    let end = inner.find(']')?;
    // Anything after the closing bracket must be a comment.
    let rest = inner[end + 1..].trim();
    (rest.is_empty() || rest.starts_with('#')).then(|| inner[..end].trim())
}

/// Lines `[start, end)` of `table`'s body (after its header), and the
/// header's line, if the file has it. The top level is table "".
fn toml_table_at(text: &str, table: &str) -> Option<(usize, usize, Option<usize>)> {
    let lines: Vec<&str> = text.lines().collect();
    let (start, head) = if table.is_empty() {
        (0, None)
    } else {
        let h = lines.iter().position(|l| header(l) == Some(table))?;
        (h + 1, Some(h))
    };
    let end = lines.iter().enumerate().skip(start).find(|(_, l)| l.trim_start().starts_with('[')).map_or(lines.len(), |(i, _)| i);
    Some((start, end, head))
}

fn key_of(line: &str) -> Option<(&str, &str)> {
    let t = line.trim_start();
    if t.starts_with('#') {
        return None;
    }
    let (k, v) = t.split_once('=')?;
    Some((k.trim(), v.trim()))
}

/// Strips a trailing `# comment` from a value outside quotes.
fn value_only(v: &str) -> String {
    let mut quoted = false;
    for (i, c) in v.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '#' if !quoted => return v[..i].trim().to_string(),
            _ => {}
        }
    }
    v.trim().to_string()
}

pub fn toml_get(text: &str, table: &str, key: &str) -> Option<String> {
    let (s, e, _) = toml_table_at(text, table)?;
    text.lines().skip(s).take(e - s).find_map(|l| key_of(l).filter(|(k, _)| *k == key).map(|(_, v)| value_only(v)))
}

fn unquote(v: &str) -> String {
    v.trim().trim_matches('"').to_string()
}

fn join(lines: &[String], nl: &str, trailing: bool) -> String {
    let mut s = lines.join(nl);
    if trailing && !lines.is_empty() {
        s.push_str(nl);
    }
    s
}

/// Sets `key = value` in `table`, adding the line (and the table, at the
/// end of the file) when missing.
fn toml_set(text: &str, table: &str, key: &str, value: &str) -> String {
    let nl = eol(text);
    let trailing = text.is_empty() || text.ends_with('\n');
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    match toml_table_at(text, table) {
        Some((s, e, _)) => {
            if let Some(i) = (s..e).find(|&i| key_of(&lines[i]).is_some_and(|(k, _)| k == key)) {
                let indent: String = lines[i].chars().take_while(|c| c.is_whitespace()).collect();
                lines[i] = format!("{indent}{key} = {value}");
            } else {
                // After the table's last key line, so it isn't under the next
                // table's comments.
                let at = (s..e).rev().find(|&i| key_of(&lines[i]).is_some()).map_or(s, |i| i + 1);
                lines.insert(at, format!("{key} = {value}"));
            }
        }
        None => {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(format!("[{table}]"));
            lines.push(format!("{key} = {value}"));
        }
    }
    join(&lines, nl, trailing)
}

fn toml_remove(text: &str, table: &str, key: &str) -> String {
    let Some((s, e, _)) = toml_table_at(text, table) else { return text.to_string() };
    let trailing = text.ends_with('\n');
    let lines: Vec<String> = text.lines().enumerate().filter(|(i, l)| !(*i >= s && *i < e && key_of(l).is_some_and(|(k, _)| k == key))).map(|(_, l)| l.to_string()).collect();
    join(&lines, eol(text), trailing)
}

/// Takes `table` out: its header and its key lines (comments before the
/// next table stay), and one blank line before it that [`toml_set`] added.
fn toml_drop_table(text: &str, table: &str) -> String {
    let Some((s, e, Some(h))) = toml_table_at(text, table) else { return text.to_string() };
    let trailing = text.ends_with('\n');
    let all: Vec<&str> = text.lines().collect();
    let mut keep = Vec::new();
    for (i, l) in all.iter().enumerate() {
        if i == h || (i >= s && i < e && (key_of(l).is_some() || l.trim().is_empty())) {
            continue;
        }
        keep.push(l.to_string());
    }
    // The blank line toml_set put before a table it added at the end.
    if h > 0 && e == all.len() && all[h - 1].trim().is_empty() && keep.last().is_some_and(|l| l.trim().is_empty()) {
        keep.pop();
    }
    join(&keep, eol(text), trailing)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVC: &str = "# Simple Voice Chat server config v1.21.1-2.5.24\n\n# The port number to use for the voice chat communication.\nport=24454\nbind_address=\nmax_voice_distance=48.0\nvoice_host=\nallow_pings=true\n";

    const PLASMO: &str = "server_id = \"8a1c\"\ndefault_language = \"en_us\"\n\n[host]\n# IP address for the voice server to bind to\nip = \"0.0.0.0\"\n# UDP port for the voice server\n# 0 = same port as Minecraft server\nport = 0\n\n# [host.public]\n# ip = \"127.0.0.1\"\n# port = 0\n\n[voice]\nsample_rate = 48000\n";

    #[test]
    fn svc_port_and_bind() {
        let d = read_config(VoiceMod::SimpleVoiceChat, Some(SVC), 25565);
        assert_eq!((d.port, d.bind), (24454, None));
        let d = read_config(VoiceMod::SimpleVoiceChat, Some("port=-1\nbind_address=*\n"), 25570);
        assert_eq!((d.port, d.bind), (25570, None));
        let d = read_config(VoiceMod::SimpleVoiceChat, Some("port=24500\nbind_address=10.0.0.4\n"), 25565);
        assert_eq!((d.port, d.bind), (24500, Some("10.0.0.4".parse().unwrap())));
        // Not written yet, or nonsense: the mod's own default.
        assert_eq!(read_config(VoiceMod::SimpleVoiceChat, None, 25565).port, 24454);
        assert_eq!(read_config(VoiceMod::SimpleVoiceChat, Some("port=99999\n"), 25565).port, 24454);
    }

    #[test]
    fn plasmo_port_follows_the_server_port() {
        let d = read_config(VoiceMod::PlasmoVoice, Some(PLASMO), 25570);
        assert_eq!((d.port, d.bind), (25570, None));
        let t = PLASMO.replace("port = 0\n\n#", "port = 24460\n\n#");
        assert_eq!(read_config(VoiceMod::PlasmoVoice, Some(&t), 25570).port, 24460);
        // The commented-out [host.public] is not a table.
        assert_eq!(toml_get(PLASMO, "host.public", "port"), None);
        assert_eq!(toml_get(PLASMO, "host", "ip").as_deref(), Some("\"0.0.0.0\""));
    }

    #[test]
    fn jars_are_recognised() {
        assert!(VoiceMod::SimpleVoiceChat.jar("voicechat-fabric-1.20.1-2.5.24.jar"));
        assert!(VoiceMod::SimpleVoiceChat.jar("voicechat-neoforge-1.21.1-2.6.0.jar"));
        assert!(!VoiceMod::SimpleVoiceChat.jar("voicechat-fabric-1.20.1-2.5.24.jar.disabled"));
        assert!(VoiceMod::PlasmoVoice.jar("plasmovoice-fabric-1.20.1-2.1.3.jar"));
        assert!(!VoiceMod::PlasmoVoice.jar("fabric-api-0.92.jar"));
    }

    #[test]
    fn svc_host_goes_on_and_back_exactly() {
        let (new, before, ch) = apply(VoiceMod::SimpleVoiceChat, Some(SVC), "play.example.com", 24454);
        let new = new.unwrap();
        assert!(new.contains("voice_host=play.example.com\\:24454") || new.contains("voice_host=play.example.com:24454"), "{new}");
        assert_eq!(Properties::parse(&new).get("voice_host"), Some("play.example.com:24454"));
        assert_eq!(ch, vec![VoiceChange { key: "voice_host".into(), was: Some(String::new()), now: Some("play.example.com:24454".into()) }]);
        // Twice: nothing more.
        let (again, _, ch2) = apply(VoiceMod::SimpleVoiceChat, Some(&new), "play.example.com", 24454);
        assert!(again.is_none() && ch2.is_empty());
        let (back, ch) = restore(&new, &before);
        assert_eq!(back.as_deref(), Some(SVC));
        assert_eq!(ch.len(), 1);
    }

    #[test]
    fn svc_without_a_config_yet_and_a_hand_edit() {
        let (new, before, _) = apply(VoiceMod::SimpleVoiceChat, None, "203.0.113.5", 24454);
        let new = new.unwrap();
        assert_eq!(Properties::parse(&new).get("voice_host"), Some("203.0.113.5:24454"));
        // The mod filled in its defaults on start; the key comes out again.
        let started = format!("port=24454\n{new}allow_pings=true\n");
        let (back, _) = restore(&started, &before);
        assert_eq!(back.as_deref(), Some("port=24454\nallow_pings=true\n"));
        // Someone set it by hand meanwhile: left alone.
        let hand = new.replace("203.0.113.5", "voice.example.com");
        assert_eq!(restore(&hand, &before).0, None);
    }

    #[test]
    fn plasmo_table_is_added_and_taken_out() {
        let (new, before, ch) = apply(VoiceMod::PlasmoVoice, Some(PLASMO), "play.example.com", 24454);
        let new = new.unwrap();
        assert!(before.made_table);
        assert_eq!(toml_get(&new, "host.public", "ip").as_deref(), Some("\"play.example.com\""));
        assert_eq!(toml_get(&new, "host.public", "port").as_deref(), Some("24454"));
        // [host] untouched.
        assert_eq!(toml_get(&new, "host", "port").as_deref(), Some("0"));
        assert_eq!(ch[0].now.as_deref(), Some("play.example.com"));
        let (back, _) = restore(&new, &before);
        assert_eq!(back.as_deref(), Some(PLASMO));
    }

    #[test]
    fn plasmo_existing_table_keeps_its_values() {
        let t = "[host]\nip = \"0.0.0.0\"\nport = 0\n\n[host.public]\nip = \"1.2.3.4\"\nport = 25570\n\n[voice]\nx = 1\n";
        let (new, before, _) = apply(VoiceMod::PlasmoVoice, Some(t), "relay.example.com", 24454);
        let new = new.unwrap();
        assert!(!before.made_table);
        assert!(new.contains("[host.public]\nip = \"relay.example.com\"\nport = 24454\n\n[voice]"), "{new}");
        assert_eq!(restore(&new, &before).0.as_deref(), Some(t));
        // CRLF stays CRLF.
        let crlf = t.replace('\n', "\r\n");
        let (n2, b2, _) = apply(VoiceMod::PlasmoVoice, Some(&crlf), "relay.example.com", 24454);
        let n2 = n2.unwrap();
        assert!(!n2.replace("\r\n", "").contains('\n'));
        assert_eq!(restore(&n2, &b2).0.as_deref(), Some(crlf.as_str()));
    }

    #[test]
    fn plasmo_quotes_cant_break_out() {
        let (new, _, _) = apply(VoiceMod::PlasmoVoice, Some(""), "a\"b\\c", 1);
        assert_eq!(toml_get(&new.unwrap(), "host.public", "ip").as_deref(), Some("\"abc\""));
    }

    #[test]
    fn detect_reads_the_folder() {
        let d = std::env::temp_dir().join(format!("hopper-voice-detect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("mods")).unwrap();
        assert_eq!(detect(&d, 25565), None);
        std::fs::write(d.join("mods/voicechat-fabric-1.20.1-2.5.24.jar.disabled"), b"").unwrap();
        assert_eq!(detect(&d, 25565), None, "a turned-off jar doesn't count");
        std::fs::write(d.join("mods/voicechat-fabric-1.20.1-2.5.24.jar"), b"").unwrap();
        assert_eq!(detect(&d, 25565).map(|x| (x.kind, x.port)), Some((VoiceMod::SimpleVoiceChat, 24454)));
        std::fs::create_dir_all(d.join("config/voicechat")).unwrap();
        std::fs::write(d.join(SVC_CONFIG), "port=24500\n").unwrap();
        assert_eq!(detect(&d, 25565).map(|x| x.port), Some(24500));
        let _ = std::fs::remove_dir_all(&d);
    }
}
