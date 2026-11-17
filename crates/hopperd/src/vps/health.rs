//! The relay's health check: the relay card's "Test again" and the setup's
//! last step ("Test"). It answers the questions an owner asks when players
//! can't join: is the relay connected, and can a player reach this server
//! through the VPS? Each answer is one row in plain words, with what to do
//! when it fails.
//!
//! It never knocks on the relay's tunnel port by itself: the relay counts a
//! connection that doesn't finish the key check as a failed try (10 in 10
//! minutes and home is locked out), so "is it reachable" is answered by the
//! supervisor's own next try, which "Test again" starts at once.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, Method};
use axum::Json;
use hopper_core::hosting::router::{self as core, Read};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

use crate::App;
use crate::api::{ApiResult, bad, not_found, require_owner};
use crate::tunnel::mux::MuxError;

/// How long the test waits for the supervisor's try to connect.
const LINK_WAIT: Duration = Duration::from_secs(12);
/// How long a server list ping through the VPS may take.
const PING_WAIT: Duration = Duration::from_secs(8);

fn row(id: &str, state: &str, text: impl Into<String>) -> Value {
    json!({ "id": id, "state": state, "text": text.into() })
}

/// Why a connection to the relay ended, in words an owner can act on.
pub fn mux_words(e: &MuxError) -> String {
    match e {
        MuxError::Timeout => "The relay stopped answering.".into(),
        MuxError::Closed | MuxError::Io(_) => "The connection to the relay dropped.".into(),
        MuxError::GoAway(_, r) if r.contains("Another home connection") => "Another panel connected to this relay with the same key and took it over.".into(),
        MuxError::GoAway(_, _) => "The relay closed the connection (it restarted or was stopped).".into(),
        MuxError::Version { peer, .. } => format!("The relay runs an older Hopper Server ({peer}). Open the relay setup and press Run again to update it."),
        MuxError::Protocol(_) => "The relay sent something this panel doesn't understand. Open the relay setup and press Run again to update it.".into(),
    }
}

/// A failed TCP connect to the VPS, as the end of "Can't reach the VPS at
/// host:port: …".
pub fn connect_words(e: &std::io::Error, port: u16) -> String {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::ConnectionRefused => "the VPS is up, but the relay isn't running there. Open the relay setup and press Run again to repair it.".into(),
        K::TimedOut => no_answer(port),
        K::ConnectionReset | K::ConnectionAborted => "the VPS closed the connection.".into(),
        K::NotFound => "that name isn't known. Check the address.".into(),
        K::HostUnreachable | K::NetworkUnreachable | K::AddrNotAvailable => "this PC has no way to it. Is the internet connection here working?".into(),
        // Windows: "No such host is known" / a name lookup that failed for now.
        _ if matches!(e.raw_os_error(), Some(11001 | 11004)) => "that name isn't known. Check the address.".into(),
        _ if e.raw_os_error() == Some(11002) => "the name lookup failed. Is the internet connection here working?".into(),
        _ => "the connection failed.".into(),
    }
}

pub fn no_answer(port: u16) -> String {
    format!("no answer. The VPS may be switched off, or its firewall blocks TCP {port}. Check it on the provider's page.")
}

/// Splits "host[:port]" with a default port (IPv6 in brackets).
fn host_port(a: &str, default: u16) -> Option<(String, u16)> {
    let a = a.trim();
    if a.is_empty() {
        return None;
    }
    if let Some(rest) = a.strip_prefix('[') {
        let (h, tail) = rest.split_once(']')?;
        let port = match tail.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None if tail.is_empty() => default,
            None => return None,
        };
        return Some((h.to_string(), port));
    }
    match a.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => Some((h.to_string(), p.parse().ok()?)),
        Some(_) => Some((a.to_string(), default)),
        None => Some((a.to_string(), default)),
    }
}

/// What a server list ping on the players' port got back.
#[derive(Debug, PartialEq)]
pub enum Answer {
    /// Home's router (or Velocity in front of it) answered: the whole path works.
    Home,
    /// The relay answered by itself: its port is open, home isn't connected.
    RelayAlone,
    /// No answer: (the reason, for the row).
    None(String),
}

/// Reads the status JSON's answer: the relay's own one names itself.
pub fn who_answered(status: &Value) -> Answer {
    // The description is a text component or a plain string.
    let d = &status["description"];
    let offline = d.as_str().or_else(|| d["text"].as_str()).is_some_and(|t| t.contains("Home server is offline"));
    if status["version"]["name"] == "Hopper relay" || offline { Answer::RelayAlone } else { Answer::Home }
}

/// A server list ping (status only) on the players' address.
async fn list_ping(host: &str, port: u16) -> Answer {
    let go = async {
        let mut s = match TcpStream::connect((host, port)).await {
            Ok(s) => s,
            Err(e) => return Answer::None(connect_words(&e, port)),
        };
        let mut out = core::handshake_packet(767, host, port, 1);
        out.extend(core::packet(0, &[]));
        if s.write_all(&out).await.is_err() {
            return Answer::None("the VPS closed the connection.".into());
        }
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match core::read_packet(&buf, 1 << 20) {
                Ok(Read::Done((0, body), _)) => {
                    return match core::read_string(body, 1 << 20).ok().and_then(|(t, _)| serde_json::from_str::<Value>(&t).ok()) {
                        Some(v) => who_answered(&v),
                        None => Answer::None("something answered that isn't a Minecraft server.".into()),
                    };
                }
                Ok(Read::Done(_, _)) | Err(_) => return Answer::None("something answered that isn't a Minecraft server.".into()),
                Ok(_) => {}
            }
            match s.read(&mut chunk).await {
                Ok(0) | Err(_) => return Answer::None("the VPS closed the connection without an answer.".into()),
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
    };
    tokio::time::timeout(PING_WAIT, go).await.unwrap_or_else(|_| Answer::None(no_answer(port)))
}

/// Runs the test for `id` (a server set to use the relay) and keeps the
/// result for the card.
pub async fn test(app: &Arc<App>, id: &str) -> Value {
    let cfg = app.vps.config();
    let mut rows = Vec::new();
    // 1. The relay connection: a new try now unless it's up.
    if app.vps.status().state != "up" {
        app.vps.wake();
        let until = Instant::now() + LINK_WAIT;
        // The supervisor marks "connecting" first; wait for the try's end.
        tokio::time::sleep(Duration::from_millis(300)).await;
        while Instant::now() < until && !matches!(app.vps.status().state, "up" | "down" | "off") {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    let st = app.vps.status();
    let up = st.state == "up";
    rows.push(match st.state {
        "up" => {
            let rtt = app.vps.server_status(id)["rtt_ms"].as_u64();
            row("link", "ok", match rtt {
                Some(ms) => format!("The relay is connected · {} ms from here", if ms < 1 { "under 1".to_string() } else { ms.to_string() }),
                None => "The relay is connected".to_string(),
            })
        }
        "off" => row("link", "fail", "The relay is turned off in this panel's settings."),
        "connecting" => row("link", "fail", "The relay didn't connect in time. Test again in a minute."),
        _ => row("link", "fail", if st.error.is_empty() { "Can't reach the relay.".to_string() } else { st.error.clone() }),
    });
    // 2. A player's way in: the players' address, from this PC.
    // With several servers on the relay, the server's own address (B7).
    let address = super::home::server_players_address(app, id);
    let own = address != app.vps.players_address();
    let game_port = if st.game_port != 0 { st.game_port } else { 25565 };
    match host_port(&address, if own || cfg.public_address.trim().is_empty() { game_port } else { 25565 }) {

        None => rows.push(row("players", "fail", "There is no players' address yet. Open the relay setup to finish it.")),
        Some((host, port)) => rows.push(match list_ping(&host, port).await {
            Answer::Home if up => row("players", "ok", format!("Players reach this server at {address}")),
            // Connected a moment ago, then dropped, or the other way round.
            Answer::Home => row("players", "ok", format!("Players reach a server at {address}")),
            Answer::RelayAlone => row("players", if up { "warn" } else { "fail" }, format!("Players reach the VPS at {address}, but it says \"Home server is offline\" until the relay connects.")),
            Answer::None(why) => row("players", "fail", format!("Players can't reach {address}: {why}")),
        }),
    }
    // 3. Voice chat, for a server with a voice mod pointed at the relay.
    if let Some(e) = app.servers.get(id) {
        let (a, root, set) = (app.clone(), e.root.clone(), super::home::has_voice(app, id));
        let v = tokio::task::spawn_blocking(move || a.vps.voice_json(&root, set)).await.unwrap_or_default();
        let port = v["port"].as_u64().unwrap_or(24454);
        match v["state"].as_str() {
            Some("on") => rows.push(row("voice", "ok", format!("Voice chat goes through UDP {port}"))),
            Some("tcp") => rows.push(row("voice", "warn", format!("Voice chat goes through the tunnel: UDP {port} is blocked. Open UDP {port} on the provider's firewall page for less delay."))),
            Some("checking") => rows.push(row("voice", "warn", format!("Voice chat: still checking UDP {port}. Test again in a few seconds."))),
            Some("old") => rows.push(row("voice", "warn", "Voice chat needs a newer relay. Open the relay setup and press Run again.")),
            _ => {}
        }
    }
    let ok = rows.iter().all(|r| r["state"] != "fail");
    let res = json!({ "ok": ok, "at": super::home::unix(), "rows": rows });
    app.vps.set_last_test(id, res.clone());
    res
}

/// `POST /api/v1/servers/{id}/vps-relay/test`. Owner only, like "Try now":
/// it starts a connection try and probes the VPS from this panel.
pub async fn test_route(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, UrlPath(id): UrlPath<String>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    if !app.vps.config().servers.iter().any(|s| *s == id) {
        return Err(bad("This server doesn't use the relay."));
    }
    Ok(Json(test(&app, &id).await))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_split_with_a_default_port() {
        assert_eq!(host_port("play.example.com", 25565), Some(("play.example.com".into(), 25565)));
        assert_eq!(host_port("203.0.113.5:4000", 25565), Some(("203.0.113.5".into(), 4000)));
        assert_eq!(host_port("[2001:db8::1]", 25565), Some(("2001:db8::1".into(), 25565)));
        assert_eq!(host_port("[2001:db8::1]:7", 25565), Some(("2001:db8::1".into(), 7)));
        assert_eq!(host_port("2001:db8::1", 30000), Some(("2001:db8::1".into(), 30000)));
        assert_eq!(host_port("  ", 25565), None);
        assert_eq!(host_port("host:notaport", 25565), None);
    }

    #[test]
    fn the_relays_own_answer_is_told_apart() {
        assert_eq!(who_answered(&json!({ "version": { "name": "Hopper relay" }, "description": { "text": "Home server is offline" } })), Answer::RelayAlone);
        assert_eq!(who_answered(&json!({ "version": { "name": "1.20.1" }, "description": { "text": "A Minecraft Server" } })), Answer::Home);
        // Something in front of the relay keeping its words.
        assert_eq!(who_answered(&json!({ "version": { "name": "Velocity 3.4.0" }, "description": "Home server is offline" })), Answer::RelayAlone);
    }

    /// Every reason says what happened without system error text.
    #[test]
    fn reasons_are_plain() {
        use std::io::{Error, ErrorKind};
        for e in [Error::from(ErrorKind::ConnectionRefused), Error::from(ErrorKind::TimedOut), Error::from(ErrorKind::ConnectionReset), Error::from_raw_os_error(11001), Error::other("x")] {
            let w = connect_words(&e, 25566);
            assert!(!w.contains("os error") && w.ends_with('.'), "{w}");
        }
        assert!(connect_words(&Error::from(ErrorKind::TimedOut), 25566).contains("TCP 25566"));
        for e in [MuxError::Timeout, MuxError::Closed, MuxError::GoAway(1, "Another home connection took over this relay.".into()), MuxError::Protocol("bad frame".into()), MuxError::Version { ours: 3, theirs: 1, peer: "0.60.0".into() }] {
            let w = mux_words(&e);
            assert!(!w.contains("protocol error") && !w.contains("bad frame") && w.ends_with('.'), "{w}");
        }
    }

    /// Against a listener that answers like the relay does while home is
    /// away, and one that answers like a server.
    #[tokio::test]
    async fn list_ping_tells_who_answered() {
        async fn answer_with(desc: Value) -> u16 {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = l.local_addr().unwrap().port();
            tokio::spawn(async move {
                let (mut s, _) = l.accept().await.unwrap();
                let mut b = [0u8; 512];
                let _ = s.read(&mut b).await.unwrap();
                let text = desc.to_string();
                let mut body = Vec::new();
                core::write_varint(&mut body, text.len() as i32);
                body.extend_from_slice(text.as_bytes());
                s.write_all(&core::packet(0, &body)).await.unwrap();
            });
            port
        }
        let p = answer_with(json!({ "version": { "name": "Hopper relay", "protocol": 767 }, "description": { "text": "Home server is offline" } })).await;
        assert_eq!(list_ping("127.0.0.1", p).await, Answer::RelayAlone);
        let p = answer_with(json!({ "version": { "name": "1.21.1", "protocol": 767 }, "description": { "text": "hi" } })).await;
        assert_eq!(list_ping("127.0.0.1", p).await, Answer::Home);
        // A port that hangs up without a word (kept bound, so nothing else
        // can take it during the test).
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mute = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((s, _)) = l.accept().await {
                drop(s);
            }
        });
        assert!(matches!(list_ping("127.0.0.1", mute).await, Answer::None(_)));
    }
}
