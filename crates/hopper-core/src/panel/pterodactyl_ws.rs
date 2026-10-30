//! Pterodactyl's (and Pelican's) console, which only streams over Wings'
//! websocket. One background connection per watched server keeps the latest
//! lines; it closes itself a minute after nobody asked for them.
//!
//! The flow: `GET …/websocket` gives a token and the socket's address; the
//! socket wants `{"event":"auth","args":[token]}`, then `send logs` replays
//! the recent console. Tokens last 10 minutes and Wings says `token expiring`
//! a minute before, so a fresh one is fetched and sent on the same socket.
//! Wings only accepts the panel's own address as the Origin.

use anyhow::{Context as _, bail};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Lines kept per server.
const KEEP: usize = 1000;
/// A connection with no reader for this long closes.
const IDLE: Duration = Duration::from_secs(60);

#[derive(Default)]
struct Feed {
    lines: VecDeque<String>,
    /// Why the last connection failed, while no lines came yet.
    error: Option<String>,
    read_at: Option<Instant>,
    running: bool,
}

type Feeds = Mutex<HashMap<(String, String), Arc<Mutex<Feed>>>>;
static FEEDS: std::sync::LazyLock<Feeds> = std::sync::LazyLock::new(Default::default);

/// What a connection needs to fetch tokens.
#[derive(Clone)]
pub(super) struct Access {
    pub base: String,
    pub key: String,
    pub self_signed: bool,
}

/// The server's latest console lines, oldest first. The first call starts
/// the connection and may return nothing yet.
pub(super) fn lines(access: &Access, id: &str) -> anyhow::Result<Vec<String>> {
    let feed = FEEDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry((access.base.clone(), id.to_string()))
        .or_default()
        .clone();
    let mut f = feed.lock().unwrap_or_else(|e| e.into_inner());
    f.read_at = Some(Instant::now());
    if !f.running {
        f.running = true;
        let (a, id, feed2) = (access.clone(), id.to_string(), feed.clone());
        std::thread::Builder::new()
            .name("ptero-console".into())
            .spawn(move || run(a, id, feed2))
            .context("starting the console reader")?;
    }
    if f.lines.is_empty()
        && let Some(e) = &f.error
    {
        bail!("{e}");
    }
    Ok(f.lines.iter().cloned().collect())
}

fn watched(feed: &Mutex<Feed>) -> bool {
    let mut f = feed.lock().unwrap_or_else(|e| e.into_inner());
    let on = f.read_at.is_some_and(|t| t.elapsed() < IDLE);
    if !on {
        f.running = false;
    }
    on
}

/// Keeps a connection up while someone reads the console.
fn run(a: Access, id: String, feed: Arc<Mutex<Feed>>) {
    let mut wait = Duration::from_secs(2);
    while watched(&feed) {
        match session(&a, &id, &feed) {
            Ok(()) => wait = Duration::from_secs(2),
            Err(e) => {
                log::debug!("pterodactyl console {id}: {e:#}");
                feed.lock().unwrap_or_else(|e| e.into_inner()).error = Some(format!("{e:#}"));
                wait = (wait * 2).min(Duration::from_secs(30));
            }
        }
        let until = Instant::now() + wait;
        while Instant::now() < until && watched(&feed) {
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

/// `{token, socket}` for the server.
fn token(a: &Access, id: &str) -> anyhow::Result<(String, String)> {
    let p = super::Pterodactyl::new(&a.base, &a.key, a.self_signed);
    let v = p.call("GET", &format!("/servers/{id}/websocket"), None)?;
    let d = &v["data"];
    match (d["token"].as_str(), d["socket"].as_str()) {
        (Some(t), Some(s)) => Ok((t.to_string(), s.to_string())),
        _ => bail!("Pterodactyl didn't give a console address."),
    }
}

trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

/// One connection, until it drops or nobody reads any more.
fn session(a: &Access, id: &str, feed: &Mutex<Feed>) -> anyhow::Result<()> {
    let (tok, socket) = token(a, id)?;
    let mut ws = connect(&socket, &origin(&a.base), a.self_signed)?;
    let send = |ws: &mut tungstenite::WebSocket<Box<dyn Stream>>, event: &str, args: Value| {
        ws.send(tungstenite::Message::text(json!({ "event": event, "args": args }).to_string()))
    };
    send(&mut ws, "auth", json!([tok]))?;
    loop {
        if !watched(feed) {
            let _ = ws.close(None);
            return Ok(());
        }
        let msg = match ws.read() {
            Ok(m) => m,
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
            {
                continue;
            }
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let tungstenite::Message::Text(text) = msg else { continue };
        let v: Value = serde_json::from_str(text.as_str()).unwrap_or(Value::Null);
        let arg = v["args"][0].as_str().unwrap_or("");
        match v["event"].as_str().unwrap_or("") {
            "auth success" => {
                // Replays the recent console, so start the list over.
                feed.lock().unwrap_or_else(|e| e.into_inner()).lines.clear();
                send(&mut ws, "send logs", json!([]))?;
            }
            "console output" | "install output" | "daemon message" => {
                let mut f = feed.lock().unwrap_or_else(|e| e.into_inner());
                f.error = None;
                for line in arg.lines() {
                    f.lines.push_back(strip_ansi(line));
                }
                while f.lines.len() > KEEP {
                    f.lines.pop_front();
                }
            }
            "token expiring" => {
                let (t, _) = token(a, id)?;
                send(&mut ws, "auth", json!([t]))?;
            }
            // Start over with a new token.
            "token expired" | "jwt error" => return Ok(()),
            "daemon error" => bail!("Wings: {arg}"),
            _ => {}
        }
    }
}

/// The panel's address as an Origin: scheme, host and port, no path.
pub(super) fn origin(base: &str) -> String {
    let (scheme, rest) = base.split_once("://").unwrap_or(("https", base));
    let host = rest.split('/').next().unwrap_or("");
    format!("{scheme}://{host}")
}

/// Opens `socket` (ws:// or wss://), with a read timeout so the loop can
/// notice nobody reads any more.
fn connect(socket: &str, origin: &str, self_signed: bool) -> anyhow::Result<tungstenite::WebSocket<Box<dyn Stream>>> {
    use tungstenite::client::IntoClientRequest as _;
    let (tls, rest) = match socket.split_once("://") {
        Some(("wss", r)) => (true, r),
        Some(("ws", r)) => (false, r),
        _ => bail!("Unexpected console address {socket}"),
    };
    let hostport = rest.split('/').next().unwrap_or("");
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => (h, p.parse::<u16>()?),
        _ => (hostport, if tls { 443 } else { 80 }),
    };
    let host = host.trim_matches(['[', ']']);
    let addr = (host, port)
        .to_socket_addrs()?
        .next()
        .with_context(|| format!("{host} not found"))?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(10)).with_context(|| format!("console at {hostport}"))?;
    tcp.set_read_timeout(Some(Duration::from_secs(1)))?;
    tcp.set_write_timeout(Some(Duration::from_secs(10)))?;
    let stream: Box<dyn Stream> = if tls {
        let config = crate::tls::client_config(self_signed);
        let name = rustls::pki_types::ServerName::try_from(host.to_string())?;
        let conn = rustls::ClientConnection::new(config, name)?;
        Box::new(rustls::StreamOwned::new(conn, tcp))
    } else {
        Box::new(tcp)
    };
    let mut req = socket.into_client_request()?;
    req.headers_mut().insert("Origin", origin.parse()?);
    // The handshake may hit the read timeout while Wings answers.
    let mut hs = tungstenite::client::client(req, stream);
    let started = Instant::now();
    loop {
        match hs {
            Ok((ws, _)) => return Ok(ws),
            Err(tungstenite::HandshakeError::Interrupted(mid)) if started.elapsed() < Duration::from_secs(15) => {
                hs = mid.handshake();
            }
            Err(tungstenite::HandshakeError::Interrupted(_)) => bail!("The console at {hostport} didn't answer."),
            Err(tungstenite::HandshakeError::Failure(tungstenite::Error::Http(r))) if r.status() == 403 => {
                bail!("Wings refused the console: its allowed origins must include the panel's address ({origin}).")
            }
            Err(tungstenite::HandshakeError::Failure(e)) => return Err(e.into()),
        }
    }
}

/// Removes terminal colour codes (Wings colours its own lines).
pub(super) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' {
            if it.peek() == Some(&'[') {
                it.next();
                // Parameters, then one final letter.
                for d in it.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if c != '\r' {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_is_scheme_and_host() {
        assert_eq!(origin("https://panel.example.com"), "https://panel.example.com");
        assert_eq!(origin("https://panel.example.com:8443/sub"), "https://panel.example.com:8443");
        assert_eq!(origin("http://10.0.0.5"), "http://10.0.0.5");
    }

    #[test]
    fn colour_codes_go() {
        assert_eq!(strip_ansi("\u{1b}[33m\u{1b}[1m[Pterodactyl Daemon]:\u{1b}[39m Checking\r"), "[Pterodactyl Daemon]: Checking");
        assert_eq!(strip_ansi("[12:00:01] [Server thread/INFO]: Done (3.2s)!"), "[12:00:01] [Server thread/INFO]: Done (3.2s)!");
    }

    /// A fake Wings on localhost: checks the Origin and the auth, replays
    /// two lines, then asks for a new token.
    #[test]
    fn reads_the_console_from_a_fake_wings() {
        use std::net::TcpListener;
        let wings = TcpListener::bind("127.0.0.1:0").unwrap();
        let wport = wings.local_addr().unwrap().port();
        let api = TcpListener::bind("127.0.0.1:0").unwrap();
        let aport = api.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{aport}");
        // The panel's API: hands out tokens.
        std::thread::spawn(move || {
            for (n, s) in api.incoming().enumerate() {
                let mut s = s.unwrap();
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let body = json!({ "data": { "token": format!("t{n}"), "socket": format!("ws://127.0.0.1:{wport}/api/servers/u/ws") } }).to_string();
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        let want_origin = base.clone();
        let (seen_tx, seen_rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            let (s, _) = wings.accept().unwrap();
            let check = |req: &tungstenite::handshake::server::Request, resp| {
                let o = req.headers().get("Origin").and_then(|v| v.to_str().ok()).unwrap_or("");
                assert_eq!(o, want_origin);
                Ok(resp)
            };
            let mut ws = tungstenite::accept_hdr(s, check).unwrap();
            loop {
                let Ok(m) = ws.read() else { return };
                let v: Value = serde_json::from_str(m.to_text().unwrap_or("")).unwrap_or(Value::Null);
                let ev = v["event"].as_str().unwrap_or("").to_string();
                seen_tx.send(format!("{ev} {}", v["args"][0].as_str().unwrap_or(""))).ok();
                let out = |ws: &mut tungstenite::WebSocket<TcpStream>, e: &str, a: &str| {
                    ws.send(tungstenite::Message::text(json!({ "event": e, "args": [a] }).to_string())).unwrap();
                };
                match ev.as_str() {
                    "auth" if v["args"][0] == "t0" => out(&mut ws, "auth success", ""),
                    "send logs" => {
                        out(&mut ws, "console output", "\u{1b}[32mline one");
                        out(&mut ws, "console output", "line two");
                        out(&mut ws, "token expiring", "");
                    }
                    _ => {}
                }
            }
        });
        let a = Access { base, key: "ptlc_x".into(), self_signed: false };
        let _ = lines(&a, "abc");
        let mut got = Vec::new();
        for _ in 0..100 {
            got = lines(&a, "abc").unwrap();
            if got.len() == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(got, vec!["line one", "line two"]);
        // auth t0, send logs, then auth with the fresh token.
        let seen: Vec<String> = (0..3).filter_map(|_| seen_rx.recv_timeout(Duration::from_secs(5)).ok()).collect();
        assert_eq!(seen, vec!["auth t0", "send logs ", "auth t1"]);
    }
}
