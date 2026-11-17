//! What a Hopper Server says about "Relay through a VPS" for one server
//! (`GET /servers/{id}/vps-relay`, design/vps-relay.md), turned into the
//! words and numbers the desktop's relay card shows. The web card
//! (`RelayCard.svelte`) does the same sums in JavaScript; keep the two
//! in step.

use serde_json::Value;

/// How a health mark looks: the tick, the warning, the cross.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Ok,
    Warn,
    Fail,
}

impl Mark {
    /// From a row's `state` ("ok", "warn" or anything else, which fails).
    pub fn from_state(s: &str) -> Mark {
        match s {
            "ok" => Mark::Ok,
            "warn" => Mark::Warn,
            _ => Mark::Fail,
        }
    }
}

/// One line of the last test ("The relay is connected · 38 ms from here").
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TestRow {
    pub id: String,
    pub state: String,
    pub text: String,
}

/// The last "Test again" the panel kept for the server.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TestResult {
    pub ok: bool,
    /// Unix seconds.
    pub at: u64,
    pub rows: Vec<TestRow>,
}

/// The server's voice chat mod and how its voice travels.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Voice {
    /// "Simple Voice Chat".
    pub name: String,
    /// "on", "tcp", "checking", "old", "other", "down" or "not-set".
    pub state: String,
    pub port: u64,
    pub players: u64,
    pub rtt_ms: Option<u64>,
    /// The server whose voice has the relay's one voice port ("other").
    pub owner: String,
}

/// One server's relay status, as the panel reports it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RelayView {
    /// "up", "connecting", "down" or "off".
    pub state: String,
    pub since: Option<u64>,
    pub error: String,
    pub retry_in: Option<u64>,
    pub rtt_ms: Option<u64>,
    /// What players type to join through the relay.
    pub address: String,
    pub players: u64,
    /// "direct", "relay" or "home" (the server plays at home while the
    /// relay is down).
    pub mode: String,
    /// Voice chat mod and how its voice travels, if any.
    pub voice: Option<Voice>,
    /// Bytes sent, bytes vanilla would have sent, bytes per second now.
    pub sent: u64,
    pub vanilla: u64,
    pub sent_bps: u64,
    pub vanilla_bps: u64,
    /// Bytes the relay's cache answered, and the bytes it would have sent
    /// without it.
    pub raw: u64,
    pub hit_bytes: u64,
    /// "ok", "none" (no address of its own, behind a shared relay) or
    /// "stale" (its address isn't on the relay yet).
    pub addresses_state: String,
    pub addresses_first: String,
    pub test: Option<TestResult>,
}

impl RelayView {
    /// `None` when the server doesn't use the relay (or the panel is too
    /// old to know it).
    pub fn parse(v: &Value) -> Option<RelayView> {
        if v.get("relayed")?.as_bool() != Some(true) {
            return None;
        }
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let n = |o: &Value, k: &str| o.get(k).and_then(Value::as_u64).unwrap_or(0);
        let total = v.get("total").cloned().unwrap_or(Value::Null);
        let now = v.get("now").cloned().unwrap_or(Value::Null);
        let mode = v.pointer("/forward/mode").and_then(Value::as_str).unwrap_or("direct").to_string();
        let voice = v.get("voice").filter(|x| x.is_object()).map(|x| {
            let g = |k: &str| x.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
            Voice {
                name: g("mod"),
                state: g("state"),
                port: n(x, "port"),
                players: n(x, "players"),
                rtt_ms: x.get("rtt_ms").and_then(Value::as_u64),
                owner: g("owner"),
            }
        });
        let test = v.get("test").filter(|x| x.is_object()).map(|t| TestResult {
            ok: t.get("ok").and_then(Value::as_bool).unwrap_or(false),
            at: n(t, "at"),
            rows: t
                .get("rows")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|r| {
                    let g = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                    TestRow { id: g("id"), state: g("state"), text: g("text") }
                })
                .collect(),
        });
        Some(RelayView {
            state: s("state"),
            since: v.get("since").and_then(Value::as_u64),
            error: s("error"),
            retry_in: v.get("retry_in").and_then(Value::as_u64),
            rtt_ms: v.get("rtt_ms").and_then(Value::as_u64),
            address: s("address"),
            players: n(v, "players"),
            mode,
            voice,
            sent: n(&total, "sent"),
            vanilla: n(&total, "vanilla"),
            sent_bps: n(&now, "sent"),
            vanilla_bps: n(&now, "vanilla"),
            raw: n(&total, "raw"),
            hit_bytes: n(&total, "hit_bytes"),
            addresses_state: v.pointer("/addresses/state").and_then(Value::as_str).unwrap_or_default().to_string(),
            addresses_first: v.pointer("/addresses/hosts/0").and_then(Value::as_str).unwrap_or_default().to_string(),
            test,
        })
    }

    pub fn up(&self) -> bool {
        self.state == "up"
    }

    /// The pill: "Connected", "Connecting", "Off" or "Not connected".
    pub fn pill(&self) -> &'static str {
        match self.state.as_str() {
            "up" => "Connected",
            "connecting" => "Connecting",
            "off" => "Off",
            _ => "Not connected",
        }
    }

    /// Share of the upload the relay saved against a plain server, once
    /// anything went through.
    pub fn saved_percent(&self) -> Option<u32> {
        (self.vanilla > 0).then(|| ((1.0 - self.sent as f64 / self.vanilla as f64) * 100.0).round().max(0.0) as u32)
    }

    /// Share of what was sent that the relay's cache answered ("Cache hits").
    pub fn hit_percent(&self) -> Option<u32> {
        (self.raw > 0).then(|| ((self.hit_bytes as f64 / self.raw as f64) * 100.0).round() as u32)
    }

    /// "410 kbit/s", as (number, unit).
    pub fn rate(bytes_per_sec: u64) -> (String, &'static str) {
        let bits = bytes_per_sec as f64 * 8.0;
        if bits >= 1e6 {
            (format!("{:.1}", bits / 1e6), "Mbit/s")
        } else if bits >= 1e3 {
            (format!("{}", (bits / 1e3).round() as u64), "kbit/s")
        } else {
            (format!("{}", bits.round() as u64), "bit/s")
        }
    }

    /// "38 ms" or "under 1 ms" (the tunnel's round trip).
    pub fn ping_text(&self) -> Option<String> {
        if !self.up() {
            return None;
        }
        self.rtt_ms.map(|m| if m < 1 { "under 1 ms".to_string() } else { format!("{m} ms") })
    }

    /// "Up · 3 h", "Connecting" or "Down · 2 min": the card's Relay answer.
    pub fn relay_text(&self, now: u64) -> String {
        let word = match self.state.as_str() {
            "up" => "Up",
            "connecting" => "Connecting",
            _ => "Down",
        };
        match self.since {
            Some(t) if t > 0 && self.state != "connecting" => format!("{word} · {}", span(now.saturating_sub(t))),
            _ => word.to_string(),
        }
    }

    /// The mark in front of the Relay answer.
    pub fn link_mark(&self) -> Mark {
        match self.state.as_str() {
            "up" => Mark::Ok,
            "connecting" => Mark::Warn,
            _ => Mark::Fail,
        }
    }

    /// "Players through it": nothing while the relay is down.
    pub fn players_now(&self) -> u64 {
        if self.up() { self.players } else { 0 }
    }

    /// What the last test found, when it failed: every row (what worked
    /// helps place what didn't), except the relay row while the relay is
    /// down, since a note above says that already.
    pub fn failed_rows(&self) -> Vec<&TestRow> {
        match &self.test {
            Some(t) if !t.ok => t.rows.iter().filter(|r| !(self.state == "down" && r.id == "link")).collect(),
            _ => Vec::new(),
        }
    }

    /// Plain words for what's wrong, when something is.
    pub fn problem(&self) -> Option<String> {
        match self.state.as_str() {
            "off" => Some("The relay is turned off in Hopper Server's settings. Players can't join through it.".into()),
            "down" => {
                let why = self.error.trim();
                let why = if why.is_empty() { "Can't reach the relay".to_string() } else { why.to_string() };
                let why = if why.ends_with(['.', '?', '!']) { why } else { format!("{why}.") };
                Some(format!("{why} The server keeps running; players on the relay see \"Home server is offline\"."))
            }
            _ => None,
        }
    }

    /// "trying again in 4 s" while the relay is down.
    pub fn retry_text(&self) -> Option<String> {
        if self.state != "down" {
            return None;
        }
        self.retry_in.map(|s| if s > 0 { format!("trying again in {s} s") } else { "trying again now".to_string() })
    }

    /// Set when the server plays at home while the relay is down (or the
    /// player chose to), so players join with its own address.
    pub fn at_home(&self) -> bool {
        self.mode == "home"
    }

    /// The note for a server at home: the relay is back, or still down.
    pub fn home_note(&self) -> String {
        format!(
            "{} Players join with this server's own address and the server checks their accounts.{}",
            if self.up() { "The relay is back." } else { "Playing at home while the relay is down." },
            if self.up() { " Go back to the relay so players join through it again." } else { "" }
        )
    }

    /// A relay with several servers where this one has no address of its
    /// own: the relay would refuse its address.
    pub fn no_address(&self) -> bool {
        !self.at_home() && self.addresses_state == "none"
    }

    /// Its address isn't on the relay yet: the setup has to run again.
    pub fn stale_address(&self) -> bool {
        !self.at_home() && self.addresses_state == "stale"
    }

    /// Whether to show the "Players join" address.
    pub fn show_address(&self) -> bool {
        !self.address.is_empty() && !self.at_home() && self.addresses_state != "none"
    }

    /// The voice row's text, same words as the web card.
    pub fn voice_text(&self) -> Option<String> {
        let v = self.voice.as_ref()?;
        let m = &v.name;
        Some(match v.state.as_str() {
            "on" | "tcp" => format!("{m} · {} {}", v.players, if v.players == 1 { "player" } else { "players" }),
            "checking" => format!("{m} · checking UDP {}…", v.port),
            "old" => "The relay needs an update for voice.".to_string(),
            "other" => format!("{m} · goes through the relay for {} only", v.owner),
            _ if self.mode == "relay" => format!("{m} · stop and use the relay again to add it"),
            _ => format!("{m} · not through the relay yet"),
        })
    }

    /// The pill in front of the voice text.
    pub fn voice_pill(&self) -> Option<&'static str> {
        Some(match self.voice.as_ref()?.state.as_str() {
            "on" => "Voice on",
            "tcp" => "Voice slow",
            "checking" => "Voice",
            _ => "Voice off",
        })
    }

    /// The small text at the right of the voice row.
    pub fn voice_right(&self) -> Option<String> {
        let v = self.voice.as_ref()?;
        match v.state.as_str() {
            "on" => Some(format!(
                "UDP {}{}",
                v.port,
                v.rtt_ms.map_or(String::new(), |m| format!(" · {} ms", if m < 1 { "under 1".to_string() } else { m.to_string() }))
            )),
            "tcp" => Some("through the tunnel".to_string()),
            _ => None,
        }
    }

    /// Under the voice row when UDP doesn't get through the VPS's firewall.
    pub fn voice_note(&self) -> Option<String> {
        let v = self.voice.as_ref().filter(|v| v.state == "tcp")?;
        Some(format!(
            "UDP is blocked by the VPS provider's firewall. Open UDP {} on its firewall page for this VPS; until then voice goes through the tunnel, with more delay.",
            v.port
        ))
    }
}

/// "42 s", "5 min", "3 h" or "2 d": the relay card's spans.
pub fn span(secs: u64) -> String {
    if secs < 60 {
        format!("{secs} s")
    } else if secs < 3600 {
        format!("{} min", secs / 60)
    } else if secs < 86_400 {
        format!("{} h", secs / 3600)
    } else {
        format!("{} d", secs / 86_400)
    }
}

/// "just now" or "5 min ago" for a time at unix second `at`.
pub fn ago(now: u64, at: u64) -> String {
    let d = now.saturating_sub(at);
    if d < 30 { "just now".to_string() } else { format!("{} ago", span(d)) }
}

/// Unix seconds now.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// ---- Settings > Relays: `GET /vps-relays` (the web's Relays.svelte) -------------

/// One relay of `GET /vps-relays`, ready to list.
#[derive(Clone, Debug, PartialEq)]
pub struct RelayRow {
    pub address: String,
    /// "up", "connecting", "off" or "down".
    pub state: String,
    /// The pill: "Connected", "Connecting", "Off" or "Not connected".
    pub chip: &'static str,
    /// The one line under the address, and whether it is a problem.
    pub line: String,
    pub bad: bool,
    pub servers: Vec<RelayServerRow>,
    /// The servers use the same panel that listed them, so a take-off needs
    /// the ids that stay.
    pub raw_servers: Vec<String>,
}

/// One server behind a relay.
#[derive(Clone, Debug, PartialEq)]
pub struct RelayServerRow {
    pub id: String,
    pub name: String,
    pub line: String,
    pub bad: bool,
    /// "Accounts checked by the relay", "Playing at home" or "Server checks accounts".
    pub chip: &'static str,
    /// "1 player" or "N players".
    pub players: String,
    /// "Take off" is offered: it doesn't check accounts through the relay.
    pub can_take_off: bool,
}

/// "a minute", "5 min", "3 h" or "2 days": how long a relay has been connected.
fn since(secs: f64) -> String {
    let s = secs.max(0.0);
    if s < 90.0 {
        "a minute".into()
    } else if s < 3600.0 {
        format!("{} min", (s / 60.0).round())
    } else if s < 86_400.0 * 2.0 {
        format!("{} h", (s / 3600.0).round())
    } else {
        format!("{} days", (s / 86_400.0).round())
    }
}

fn text(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// The relays of `GET /vps-relays` with the web page's words.
pub fn relays(v: &Value, now: u64) -> Vec<RelayRow> {
    let Some(list) = v.get("relays").and_then(Value::as_array) else { return Vec::new() };
    list.iter()
        .map(|r| {
            let state = text(r, "state");
            let (chip, line, bad) = match state.as_str() {
                "up" => {
                    let mut parts = vec![format!("Connected for {}", since(now as f64 - r["since"].as_f64().unwrap_or(now as f64)))];
                    if let Some(ms) = r["rtt_ms"].as_f64() {
                        parts.push(format!("ping {} ms", if ms < 1.0 { "under 1".to_string() } else { format!("{}", ms.round()) }));
                    }
                    let ver = text(r, "relay_version");
                    if !ver.is_empty() {
                        parts.push(format!("Hopper Server {ver}"));
                    }
                    ("Connected", parts.join(" · "), false)
                }
                "off" => ("Off", "Turned off in Hopper Server's settings. Players can't join through it.".to_string(), false),
                "connecting" => ("Connecting", "Connecting…".to_string(), false),
                _ => {
                    let e = text(r, "error");
                    ("Not connected", if e.is_empty() { "Can't reach the relay.".to_string() } else { e }, true)
                }
            };
            let voice_owner = text(r, "voice_owner");
            let ids: Vec<Value> = r["servers"].as_array().cloned().unwrap_or_default();
            RelayRow {
                address: text(r, "address"),
                state,
                chip,
                line,
                bad,
                raw_servers: ids.iter().map(|s| text(s, "id")).collect(),
                servers: ids.iter().map(|s| relay_server(s, &voice_owner)).collect(),
            }
        })
        .collect()
}

fn relay_server(s: &Value, voice_owner: &str) -> RelayServerRow {
    let a = &s["addresses"];
    let mode = text(s, "mode");
    let (line, bad) = match a["state"].as_str() {
        Some("none") => ("It has no address of its own, so players can't pick it on this relay. Give it one in its Settings › Network & address.".to_string(), true),
        Some("stale") => (
            format!(
                "Its address {} isn't on the relay yet. Run the relay setup again (Repair on its Overview).",
                a["hosts"].get(0).and_then(Value::as_str).unwrap_or_default()
            ),
            true,
        ),
        _ => {
            let mut parts = vec![if mode == "home" {
                "Players join with its own address for now".to_string()
            } else {
                format!("Players join at {}", text(s, "address"))
            }];
            if s["voice"].as_bool().unwrap_or(false) {
                parts.push("voice chat through the relay".into());
            } else if !voice_owner.is_empty() {
                parts.push(format!("voice chat goes through the relay for {voice_owner} only"));
            }
            (parts.join(" · "), false)
        }
    };
    let players = s["players"].as_u64().unwrap_or(0);
    RelayServerRow {
        id: text(s, "id"),
        name: text(s, "name"),
        line,
        bad,
        chip: match mode.as_str() {
            "relay" => "Accounts checked by the relay",
            "home" => "Playing at home",
            _ => "Server checks accounts",
        },
        players: if players == 1 { "1 player".into() } else { format!("{players} players") },
        // The web offers it for `direct`; its chip is "Server checks accounts".
        can_take_off: mode == "direct",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn relays_list_reads_like_the_web_page() {
        let v = json!({"relays": [{
            "address": "play.example.com", "state": "up", "since": 1000, "rtt_ms": 0.4, "relay_version": "0.78.0",
            "voice_owner": "Cave",
            "servers": [
                {"id": "a", "name": "Cave", "mode": "relay", "address": "cave.example.com", "voice": true, "players": 1, "addresses": {"state": "ok"}},
                {"id": "b", "name": "Sky", "mode": "direct", "address": "sky.example.com", "voice": false, "players": 3, "addresses": {"state": "ok"}},
                {"id": "c", "name": "Home", "mode": "home", "address": "x", "players": 0, "addresses": {"state": "ok"}},
                {"id": "d", "name": "Bare", "mode": "direct", "players": 0, "addresses": {"state": "none"}},
                {"id": "e", "name": "Old", "mode": "direct", "players": 2, "addresses": {"state": "stale", "hosts": ["old.example.com"]}}
            ]}]});
        let r = &relays(&v, 1000 + 3 * 3600)[0];
        assert_eq!((r.chip, r.bad), ("Connected", false));
        assert_eq!(r.line, "Connected for 3 h · ping under 1 ms · Hopper Server 0.78.0");
        assert_eq!(r.raw_servers, ["a", "b", "c", "d", "e"]);
        let s = &r.servers;
        assert_eq!(s[0].line, "Players join at cave.example.com · voice chat through the relay");
        assert_eq!((s[0].chip, s[0].players.as_str(), s[0].can_take_off), ("Accounts checked by the relay", "1 player", false));
        assert_eq!(s[1].line, "Players join at sky.example.com · voice chat goes through the relay for Cave only");
        assert_eq!((s[1].chip, s[1].players.as_str(), s[1].can_take_off), ("Server checks accounts", "3 players", true));
        assert_eq!(
            (s[2].line.as_str(), s[2].chip),
            ("Players join with its own address for now · voice chat goes through the relay for Cave only", "Playing at home")
        );
        assert!(s[3].bad && s[3].line.starts_with("It has no address of its own"));
        assert!(s[4].bad && s[4].line.contains("Its address old.example.com isn't on the relay yet"));
    }

    #[test]
    fn relays_list_down_off_and_connecting() {
        let v = json!({"relays": [
            {"address": "a", "state": "down", "error": "Timed out", "servers": []},
            {"address": "b", "state": "down", "servers": []},
            {"address": "c", "state": "off", "servers": []},
            {"address": "d", "state": "connecting", "servers": []}]});
        let r = relays(&v, 0);
        assert_eq!((r[0].chip, r[0].line.as_str(), r[0].bad), ("Not connected", "Timed out", true));
        assert_eq!(r[1].line, "Can't reach the relay.");
        assert_eq!((r[2].chip, r[2].bad), ("Off", false));
        assert_eq!(r[3].line, "Connecting…");
        assert!(relays(&json!({}), 0).is_empty());
        assert_eq!(since(60.0), "a minute");
        assert_eq!(since(600.0), "10 min");
        assert_eq!(since(86_400.0 * 3.0), "3 days");
    }

    fn up() -> Value {
        json!({"relayed": true, "state": "up", "since": 100, "rtt_ms": 38, "address": "play.example.com:25565",
               "players": 3, "total": {"sent": 400, "vanilla": 1000, "raw": 2000, "hit_bytes": 500},
               "now": {"sent": 51_250, "vanilla": 137_500}, "forward": {"mode": "relay"},
               "voice": {"mod": "Simple Voice Chat", "state": "tcp", "port": 24454, "players": 2}})
    }

    #[test]
    fn not_relayed_means_no_card() {
        assert!(RelayView::parse(&json!({"relayed": false})).is_none());
        assert!(RelayView::parse(&json!({})).is_none());
    }

    #[test]
    fn reads_the_card_numbers() {
        let r = RelayView::parse(&up()).unwrap();
        assert!(r.up());
        assert_eq!(r.pill(), "Connected");
        assert_eq!(r.saved_percent(), Some(60));
        assert_eq!(r.hit_percent(), Some(25));
        assert_eq!(RelayView::rate(r.sent_bps), ("410".into(), "kbit/s"));
        assert_eq!(RelayView::rate(r.vanilla_bps), ("1.1".into(), "Mbit/s"));
        assert_eq!(r.ping_text().as_deref(), Some("38 ms"));
        assert_eq!(r.players, 3);
        assert!(r.problem().is_none());
    }

    #[test]
    fn nothing_sent_yet_has_no_saving() {
        let mut v = up();
        v["total"] = json!({});
        let r = RelayView::parse(&v).unwrap();
        assert_eq!(r.saved_percent(), None);
        assert_eq!(r.hit_percent(), None);
    }

    #[test]
    fn down_says_why_in_one_sentence() {
        let r = RelayView::parse(&json!({"relayed": true, "state": "down", "error": "The relay refused the key", "retry_in": 4})).unwrap();
        assert_eq!(r.pill(), "Not connected");
        assert!(r.ping_text().is_none());
        let p = r.problem().unwrap();
        assert!(p.starts_with("The relay refused the key. The server keeps running"), "{p}");
        assert_eq!(r.retry_text().as_deref(), Some("trying again in 4 s"));
        assert_eq!(r.players_now(), 0);
    }

    #[test]
    fn playing_at_home_is_noticed() {
        let mut v = up();
        v["forward"]["mode"] = json!("home");
        let r = RelayView::parse(&v).unwrap();
        assert!(r.at_home());
        assert!(r.home_note().starts_with("The relay is back. Players join with this server's own address"));
        assert!(!r.show_address());
    }

    #[test]
    fn health_answers_read_like_the_web_card() {
        let r = RelayView::parse(&up()).unwrap();
        // Up for 3 h, a minute and a bit.
        assert_eq!(r.relay_text(100 + 3 * 3600 + 70), "Up · 3 h");
        assert_eq!(r.link_mark(), Mark::Ok);
        let mut v = up();
        v["state"] = json!("connecting");
        let c = RelayView::parse(&v).unwrap();
        assert_eq!(c.relay_text(5000), "Connecting");
        assert_eq!(c.link_mark(), Mark::Warn);
        v["state"] = json!("down");
        assert_eq!(RelayView::parse(&v).unwrap().relay_text(100 + 125), "Down · 2 min");
    }

    #[test]
    fn spans_and_ago() {
        assert_eq!(span(0), "0 s");
        assert_eq!(span(59), "59 s");
        assert_eq!(span(60), "1 min");
        assert_eq!(span(3599), "59 min");
        assert_eq!(span(86_399), "23 h");
        assert_eq!(span(86_400 * 3), "3 d");
        assert_eq!(ago(1000, 990), "just now");
        assert_eq!(ago(1000, 1000 - 300), "5 min ago");
        // A clock a little behind the panel's doesn't underflow.
        assert_eq!(ago(10, 20), "just now");
    }

    #[test]
    fn a_failed_test_lists_its_rows_but_not_the_relay_row_while_down() {
        let mut v = up();
        v["state"] = json!("down");
        v["test"] = json!({"ok": false, "at": 50, "rows": [
            {"id": "link", "state": "fail", "text": "The relay didn't connect"},
            {"id": "players", "state": "fail", "text": "No answer on 25565"}]});
        let r = RelayView::parse(&v).unwrap();
        let rows = r.failed_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "players");
        assert_eq!(Mark::from_state(&rows[0].state), Mark::Fail);
        // Up: every row shows.
        v["state"] = json!("up");
        assert_eq!(RelayView::parse(&v).unwrap().failed_rows().len(), 2);
        // A test that worked lists nothing.
        v["test"]["ok"] = json!(true);
        assert!(RelayView::parse(&v).unwrap().failed_rows().is_empty());
    }

    #[test]
    fn voice_row_words() {
        let mut v = up();
        let r = RelayView::parse(&v).unwrap();
        assert_eq!(r.voice_text().as_deref(), Some("Simple Voice Chat · 2 players"));
        assert_eq!(r.voice_pill(), Some("Voice slow"));
        assert_eq!(r.voice_right().as_deref(), Some("through the tunnel"));
        assert!(r.voice_note().unwrap().starts_with("UDP is blocked by the VPS provider's firewall. Open UDP 24454"));
        v["voice"] = json!({"mod": "Simple Voice Chat", "state": "on", "port": 24454, "players": 1, "rtt_ms": 41});
        let r = RelayView::parse(&v).unwrap();
        assert_eq!(r.voice_text().as_deref(), Some("Simple Voice Chat · 1 player"));
        assert_eq!(r.voice_right().as_deref(), Some("UDP 24454 · 41 ms"));
        assert!(r.voice_note().is_none());
        v["voice"] = json!({"mod": "Simple Voice Chat", "state": "other", "owner": "Alpha"});
        assert_eq!(RelayView::parse(&v).unwrap().voice_text().as_deref(), Some("Simple Voice Chat · goes through the relay for Alpha only"));
        v["voice"] = json!({"mod": "Simple Voice Chat", "state": "not-set"});
        assert_eq!(RelayView::parse(&v).unwrap().voice_text().as_deref(), Some("Simple Voice Chat · stop and use the relay again to add it"));
        v["forward"]["mode"] = json!("direct");
        assert_eq!(RelayView::parse(&v).unwrap().voice_text().as_deref(), Some("Simple Voice Chat · not through the relay yet"));
        v["voice"] = json!({"mod": "Simple Voice Chat", "state": "old"});
        assert_eq!(RelayView::parse(&v).unwrap().voice_text().as_deref(), Some("The relay needs an update for voice."));
    }

    #[test]
    fn address_notes() {
        let mut v = up();
        v["addresses"] = json!({"state": "none"});
        let r = RelayView::parse(&v).unwrap();
        assert!(r.no_address() && !r.show_address());
        v["addresses"] = json!({"state": "stale", "hosts": ["a.example.com"]});
        let r = RelayView::parse(&v).unwrap();
        assert!(r.stale_address() && r.show_address());
        assert_eq!(r.addresses_first, "a.example.com");
    }
}
