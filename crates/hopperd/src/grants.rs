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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub const TICKET_TTL: Duration = Duration::from_secs(30);
pub const LINK_TTL: Duration = Duration::from_secs(60);
/// Grants waiting at once, all accounts together.
const MAX_WAITING: usize = 2048;
/// One session's share: a page asking in a loop is refused after this many.
const PER_SESSION: usize = 64;
/// One account's share, over all its sessions (a person can have dozens of
/// sessions, so the per-session share alone doesn't bound them). An account
/// at its share loses its own oldest waiting grant instead of being
/// refused: it can never lock itself out of its console, and it can't take
/// the pool from anyone else.
const PER_ACCOUNT: usize = 64;
/// Bytes of paths all waiting links may hold together, so links naming
/// many long paths can't add up to a lot of memory.
const MAX_BYTES: usize = 16 * 1024 * 1024;
/// Bytes of paths one account's waiting links may hold.
const MAX_BYTES_PER_ACCOUNT: usize = 2 * 1024 * 1024;
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

impl Purpose {
    /// Roughly what it keeps in memory.
    fn size(&self) -> usize {
        match self {
            Purpose::Live => 0,
            Purpose::File { server, paths, name, .. } => server.len() + paths.iter().map(|p| p.len() + 24).sum::<usize>() + name.as_ref().map_or(0, String::len),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Grant {
    pub purpose: Purpose,
    /// SHA-256 of the session or token that asked; checked again on use.
    pub cred: String,
    /// Who it counts against (`Auth::account_key`); see [`PER_ACCOUNT`].
    acct: String,
    /// Order made in, to drop the oldest first.
    seq: u64,
    /// The `Origin` of the request that asked (`None`: no Origin).
    pub origin: Option<String>,
    until: Instant,
}

#[derive(Default)]
pub struct Grants {
    waiting: Mutex<HashMap<String, Grant>>,
    seq: AtomicU64,
}

/// Drops the oldest waiting grant that `of` accepts; whether one went.
fn drop_oldest(m: &mut HashMap<String, Grant>, of: impl Fn(&Grant) -> bool) -> bool {
    let Some(key) = m.iter().filter(|(_, g)| of(g)).min_by_key(|(_, g)| g.seq).map(|(k, _)| k.clone()) else { return false };
    m.remove(&key);
    true
}

/// The account holding the most grants (`by_bytes`: the most bytes).
fn heaviest(m: &HashMap<String, Grant>, by_bytes: bool) -> Option<String> {
    let mut load: HashMap<&str, usize> = HashMap::new();
    for g in m.values() {
        *load.entry(&g.acct).or_default() += if by_bytes { g.purpose.size() } else { 1 };
    }
    load.into_iter().max_by_key(|(_, n)| *n).map(|(a, _)| a.to_string())
}

impl Grants {
    /// Makes a grant; returns its secret (shown once). `acct` is whose it
    /// counts against: past its share the account's own oldest waiting grants
    /// go to make room, and when the whole pool is full the biggest holder
    /// gives one up, so no account can use up the pool for the others.
    pub fn issue(&self, purpose: Purpose, cred: &str, acct: &str, origin: Option<&str>) -> Result<String, &'static str> {
        let ttl = match purpose {
            Purpose::Live => TICKET_TTL,
            Purpose::File { .. } => LINK_TTL,
        };
        let now = Instant::now();
        let mut m = self.waiting.lock().unwrap();
        m.retain(|_, g| g.until > now);
        let size = purpose.size();
        let busy = || Err("Too many links waiting. Try again in a minute.");
        if size > MAX_BYTES_PER_ACCOUNT || m.values().filter(|g| g.cred == cred).count() >= PER_SESSION {
            return busy();
        }
        let mine = |g: &Grant| g.acct == acct;
        while m.values().filter(|g| mine(g)).count() >= PER_ACCOUNT || m.values().filter(|g| mine(g)).map(|g| g.purpose.size()).sum::<usize>() + size > MAX_BYTES_PER_ACCOUNT {
            if !drop_oldest(&mut m, mine) {
                return busy();
            }
        }
        while m.len() >= MAX_WAITING {
            let Some(a) = heaviest(&m, false) else { return busy() };
            drop_oldest(&mut m, |g| g.acct == a);
        }
        while m.values().map(|g| g.purpose.size()).sum::<usize>() + size > MAX_BYTES {
            let Some(a) = heaviest(&m, true) else { return busy() };
            drop_oldest(&mut m, |g| g.acct == a);
        }
        let secret = random_hex(32);
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        m.insert(hash_token(&secret), Grant { purpose, cred: cred.to_string(), acct: acct.to_string(), seq, origin: origin.map(str::to_string), until: now + ttl });
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
        let t = g.issue(Purpose::Live, "h1", "acct", Some("http://fox:8790")).unwrap();
        assert_eq!(t.len(), 64);
        let got = g.take(&t).unwrap();
        assert_eq!(got.purpose, Purpose::Live);
        assert_eq!(got.cred, "h1");
        assert_eq!(got.origin.as_deref(), Some("http://fox:8790"));
        assert!(g.take(&t).is_none(), "single use");
        // Expired: refused, and gone.
        let t = g.issue(Purpose::Live, "h1", "acct", None).unwrap();
        g.age(&t, TICKET_TTL);
        assert!(g.take(&t).is_none());
        // A file link lives longer than a ticket, but not forever.
        let f = Purpose::File { server: "s".into(), paths: vec!["a".into()], zip: false, inline: false, name: None };
        let l = g.issue(f.clone(), "h1", "acct", None).unwrap();
        g.age(&l, TICKET_TTL);
        assert_eq!(g.take(&l).unwrap().purpose, f);
        let l = g.issue(f, "h1", "acct", None).unwrap();
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
        let t = g.issue(Purpose::Live, "h1", "acct", None).unwrap();
        assert!(!g.waiting.lock().unwrap().contains_key(&t));
    }

    #[test]
    fn a_looping_page_fills_only_its_share() {
        let g = Grants::default();
        for _ in 0..PER_SESSION {
            g.issue(Purpose::Live, "busy", "acct", None).unwrap();
        }
        assert!(g.issue(Purpose::Live, "busy", "acct", None).is_err());
        assert!(g.issue(Purpose::Live, "someone-else", "acct", None).is_ok());
    }

    #[test]
    fn waiting_links_hold_little_memory() {
        let g = Grants::default();
        let big = Purpose::File { server: "s".into(), paths: vec!["a".repeat(1024 * 1024)], zip: true, inline: false, name: None };
        for i in 0..64 {
            // Each from its own account, so only the pool-wide limit is at work.
            g.issue(big.clone(), &format!("session-{i}"), &format!("acct-{i}"), None).unwrap();
        }
        let held: usize = g.waiting.lock().unwrap().values().map(|x| x.purpose.size()).sum();
        assert!(held <= MAX_BYTES, "{held} bytes of one-megabyte links waiting");
        // Tickets hold nothing and still go through.
        assert!(g.issue(Purpose::Live, "someone", "someone", None).is_ok());
    }

    #[test]
    fn one_account_cannot_fill_the_pool() {
        let g = Grants::default();
        // Many sessions of one account: only the newest 64 grants wait.
        let first = g.issue(Purpose::Live, "s0", "owner", None).unwrap();
        let mut last = String::new();
        for i in 1..(MAX_WAITING + 100) {
            last = g.issue(Purpose::Live, &format!("s{}", i % 40), "owner", None).unwrap();
        }
        assert!(g.take(&first).is_none(), "its oldest went to make room");
        assert!(g.take(&last).is_some(), "the newest request was not refused");
        assert!(g.waiting.lock().unwrap().values().filter(|x| x.acct == "owner").count() <= PER_ACCOUNT);
        // Everyone else still gets through.
        assert!(g.issue(Purpose::Live, "v1", "vic", None).is_ok());
        let f = Purpose::File { server: "s".into(), paths: vec!["a".into()], zip: false, inline: false, name: None };
        assert!(g.issue(f, "v1", "vic", None).is_ok());
    }

    #[test]
    fn one_account_cannot_hold_much_memory() {
        let g = Grants::default();
        let mb = Purpose::File { server: "s".into(), paths: vec!["a".repeat(1024 * 1024)], zip: true, inline: false, name: None };
        let a = g.issue(mb.clone(), "s1", "owner", None).unwrap();
        g.issue(mb.clone(), "s2", "owner", None).unwrap();
        let c = g.issue(mb, "s3", "owner", None).unwrap();
        assert!(g.take(&a).is_none(), "the oldest went to stay under 2 MB");
        assert!(g.take(&c).is_some());
        let held: usize = g.waiting.lock().unwrap().values().map(|x| x.purpose.size()).sum();
        assert!(held <= MAX_BYTES_PER_ACCOUNT, "{held} bytes");
        // Ten accounts at their share: the pool isn't exhausted for an eleventh.
        let mb = Purpose::File { server: "s".into(), paths: vec!["a".repeat(1024 * 1024)], zip: true, inline: false, name: None };
        for i in 0..10 {
            g.issue(mb.clone(), &format!("x{i}"), &format!("user{i}"), None).unwrap();
        }
        assert!(g.issue(mb, "late", "late-user", None).is_ok());
        assert!(g.issue(Purpose::Live, "late", "late-user", None).is_ok());
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
