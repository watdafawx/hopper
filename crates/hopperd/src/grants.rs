//! One-use grants for pages on other origins (design/multi-panel.md, B1).
//!
//! A page on another origin signs in with a bearer token, which browsers
//! can't attach to a WebSocket, a download link or an `<img>`. For those it
//! first asks for a grant with its token, then uses the grant once:
//! - a **live ticket** (30 s) opens the live WebSocket, carried in
//!   `Sec-WebSocket-Protocol` so it never sits in a URL or a log;
//! - a **file link** (60 s) downloads or previews named files of one server.
//!
//! Every grant is bound to the session that asked (by its hash), the page
//! that asked (its `Origin`), and what it is for. Using it removes it, even
//! when the use is then refused, so it can't be tried twice. Only the
//! SHA-256 of the secret is kept, like sessions.

use crate::auth::{hash_token, random_hex};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const TICKET_TTL: Duration = Duration::from_secs(30);
pub const LINK_TTL: Duration = Duration::from_secs(60);
/// Grants waiting at once, all sessions together; a page asking in a loop
/// fills its own share first ([`PER_SESSION`]).
const MAX_WAITING: usize = 2048;
const PER_SESSION: usize = 64;
/// The WebSocket subprotocol the panel answers with, and the prefix of the
/// one that carries the ticket: `new WebSocket(url, ["hopperd", "hopperd-ticket.<t>"])`.
pub const WS_PROTOCOL: &str = "hopperd";
pub const WS_TICKET_PREFIX: &str = "hopperd-ticket.";

#[derive(Clone, Debug, PartialEq)]
pub enum Purpose {
    /// The live WebSocket.
    Live,
    /// Files of one server: one file, or a zip of several paths.
    File { server: String, paths: Vec<String>, zip: bool, inline: bool, name: Option<String> },
}

#[derive(Clone, Debug)]
pub struct Grant {
    pub purpose: Purpose,
    /// SHA-256 of the session or token that asked; checked again on use.
    pub cred: String,
    /// The `Origin` of the request that asked (`None`: no Origin).
    pub origin: Option<String>,
    until: Instant,
}

#[derive(Default)]
pub struct Grants {
    waiting: Mutex<HashMap<String, Grant>>,
}

impl Grants {
    /// Makes a grant; returns its secret (shown once).
    pub fn issue(&self, purpose: Purpose, cred: &str, origin: Option<&str>) -> Result<String, &'static str> {
        let ttl = match purpose {
            Purpose::Live => TICKET_TTL,
            Purpose::File { .. } => LINK_TTL,
        };
        let now = Instant::now();
        let mut m = self.waiting.lock().unwrap();
        m.retain(|_, g| g.until > now);
        if m.len() >= MAX_WAITING || m.values().filter(|g| g.cred == cred).count() >= PER_SESSION {
            return Err("Too many links waiting. Try again in a minute.");
        }
        let secret = random_hex(32);
        m.insert(hash_token(&secret), Grant { purpose, cred: cred.to_string(), origin: origin.map(str::to_string), until: now + ttl });
        Ok(secret)
    }

    /// Uses up the grant with `secret`: it's gone after this call whatever
    /// the caller then decides. `None` when unknown, used or expired.
    pub fn take(&self, secret: &str) -> Option<Grant> {
        if secret.is_empty() || secret.len() > 128 {
            return None;
        }
        let g = self.waiting.lock().unwrap().remove(&hash_token(secret))?;
        (g.until > Instant::now()).then_some(g)
    }

    #[cfg(test)]
    pub(crate) fn age(&self, secret: &str, by: Duration) {
        if let Some(g) = self.waiting.lock().unwrap().get_mut(&hash_token(secret)) {
            g.until -= by;
        }
    }
}

/// The ticket in a WebSocket request's `Sec-WebSocket-Protocol`, if any.
pub fn ticket_in(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get_all(axum::http::header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .find_map(|p| p.trim().strip_prefix(WS_TICKET_PREFIX).map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_use_and_expiring() {
        let g = Grants::default();
        let t = g.issue(Purpose::Live, "h1", Some("http://fox:8790")).unwrap();
        assert_eq!(t.len(), 64);
        let got = g.take(&t).unwrap();
        assert_eq!(got.purpose, Purpose::Live);
        assert_eq!(got.cred, "h1");
        assert_eq!(got.origin.as_deref(), Some("http://fox:8790"));
        assert!(g.take(&t).is_none(), "single use");
        // Expired: refused, and gone.
        let t = g.issue(Purpose::Live, "h1", None).unwrap();
        g.age(&t, TICKET_TTL);
        assert!(g.take(&t).is_none());
        // A file link lives longer than a ticket, but not forever.
        let f = Purpose::File { server: "s".into(), paths: vec!["a".into()], zip: false, inline: false, name: None };
        let l = g.issue(f.clone(), "h1", None).unwrap();
        g.age(&l, TICKET_TTL);
        assert_eq!(g.take(&l).unwrap().purpose, f);
        let l = g.issue(f, "h1", None).unwrap();
        g.age(&l, LINK_TTL);
        assert!(g.take(&l).is_none());
        // Unknown or silly secrets.
        assert!(g.take("").is_none());
        assert!(g.take("nope").is_none());
        assert!(g.take(&"a".repeat(1000)).is_none());
    }

    #[test]
    fn secrets_are_stored_hashed() {
        let g = Grants::default();
        let t = g.issue(Purpose::Live, "h1", None).unwrap();
        assert!(!g.waiting.lock().unwrap().contains_key(&t));
    }

    #[test]
    fn a_looping_page_fills_only_its_share() {
        let g = Grants::default();
        for _ in 0..PER_SESSION {
            g.issue(Purpose::Live, "busy", None).unwrap();
        }
        assert!(g.issue(Purpose::Live, "busy", None).is_err());
        assert!(g.issue(Purpose::Live, "someone-else", None).is_ok());
    }

    #[test]
    fn ticket_from_subprotocol_header() {
        let mut h = axum::http::HeaderMap::new();
        h.insert("sec-websocket-protocol", "hopperd, hopperd-ticket.abc123".parse().unwrap());
        assert_eq!(ticket_in(&h).as_deref(), Some("abc123"));
        h.insert("sec-websocket-protocol", "hopperd".parse().unwrap());
        assert_eq!(ticket_in(&h), None);
    }
}
