//! What a Hopper Server says about "Relay through a VPS" for one server
//! (`GET /servers/{id}/vps-relay`, design/vps-relay.md), turned into the
//! words and numbers the desktop's read-only relay card shows. The web
//! card (`RelayCard.svelte`) does the same sums in JavaScript; keep the two
//! in step.

use serde_json::Value;

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
    /// Voice chat mod and how its voice travels ("on", "tcp", ...), if any.
    pub voice: Option<(String, String)>,
    /// Bytes sent, bytes vanilla would have sent, bytes per second now.
    pub sent: u64,
    pub vanilla: u64,
    pub sent_bps: u64,
    pub vanilla_bps: u64,
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
            (g("mod"), g("state"))
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

    pub fn ping_text(&self) -> Option<String> {
        if !self.up() {
            return None;
        }
        self.rtt_ms.map(|m| if m < 1 { "under 1 ms".to_string() } else { format!("{m} ms") })
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

    /// Set when the server plays at home while the relay is down (or the
    /// player chose to), so players join with its own address.
    pub fn at_home(&self) -> bool {
        self.mode == "home"
    }

    pub fn voice_text(&self) -> Option<String> {
        let (m, st) = self.voice.as_ref()?;
        Some(match st.as_str() {
            "on" => format!("{m}: voice goes straight through"),
            "tcp" => format!("{m}: voice goes through the tunnel, with more delay"),
            "checking" => format!("{m}: checking voice"),
            "old" => "The relay needs an update for voice.".to_string(),
            _ => format!("{m}: voice isn't through the relay yet"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn up() -> Value {
        json!({"relayed": true, "state": "up", "since": 100, "rtt_ms": 38, "address": "play.example.com:25565",
               "players": 3, "total": {"sent": 400, "vanilla": 1000, "raw": 2000, "hit_bytes": 0},
               "now": {"sent": 51_250, "vanilla": 137_500}, "forward": {"mode": "relay"},
               "voice": {"mod": "Simple Voice Chat", "state": "tcp"}})
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
        assert_eq!(RelayView::rate(r.sent_bps), ("410".into(), "kbit/s"));
        assert_eq!(RelayView::rate(r.vanilla_bps), ("1.1".into(), "Mbit/s"));
        assert_eq!(r.ping_text().as_deref(), Some("38 ms"));
        assert_eq!(r.players, 3);
        assert!(r.voice_text().unwrap().contains("more delay"));
        assert!(r.problem().is_none());
    }

    #[test]
    fn nothing_sent_yet_has_no_saving() {
        let mut v = up();
        v["total"] = json!({});
        assert_eq!(RelayView::parse(&v).unwrap().saved_percent(), None);
    }

    #[test]
    fn down_says_why_in_one_sentence() {
        let r = RelayView::parse(&json!({"relayed": true, "state": "down", "error": "The relay refused the key", "retry_in": 4})).unwrap();
        assert_eq!(r.pill(), "Not connected");
        assert!(r.ping_text().is_none());
        let p = r.problem().unwrap();
        assert!(p.starts_with("The relay refused the key. The server keeps running"), "{p}");
    }

    #[test]
    fn playing_at_home_is_noticed() {
        let mut v = up();
        v["forward"]["mode"] = json!("home");
        assert!(RelayView::parse(&v).unwrap().at_home());
    }
}
