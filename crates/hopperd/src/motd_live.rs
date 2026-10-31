//! The server list message filled in fresh for the router's answers: a
//! running server's own answer has the message from its start, so `{day}`,
//! `{players}` and `{world_size}` would stand still. Filled in at most once
//! a minute per server, off the ping's path (a world's size and an RCON
//! question take a while); a ping uses the last one meanwhile.

use crate::servers::Entry;
use hopper_core::hosting::motd::{self, Live};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const FRESH: Duration = Duration::from_secs(60);

/// Per server: when it was filled in, the message (None: no template), and
/// whether a refill is under way.
struct Slot {
    at: Instant,
    text: Option<String>,
    busy: bool,
}

fn cache() -> &'static Mutex<HashMap<String, Slot>> {
    static C: OnceLock<Mutex<HashMap<String, Slot>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Drops a server's message (after its template changed).
pub fn forget(id: &str) {
    cache().lock().unwrap().remove(id);
}

/// What a running server knows: players from its console, the day over
/// RCON when it's on (else level.dat, saved every few minutes).
fn live(e: &Entry, ask_rcon: bool) -> Live {
    let alive = e.server.state().is_alive();
    let t = e.server.tracker();
    let day = if alive && ask_rcon {
        crate::alerts::rcon_once(e, "time query day").and_then(|r| r.split_whitespace().last().and_then(|n| n.trim_end_matches('.').parse().ok()))
    } else {
        None
    };
    Live { players: alive.then_some(t.players.len() as u32), max_players: t.max_from_list, day }
}

/// The values for the panel's preview (the same the router fills in).
pub fn vars(e: &Entry) -> std::collections::BTreeMap<String, String> {
    motd::vars(&e.root, &live(e, false))
}

fn fill(e: &Entry, ask_rcon: bool) -> Option<String> {
    motd::current(&e.root, &live(e, ask_rcon))
}

/// The server's message filled in, or None when it has no template (the
/// server's own message stands then).
pub async fn message(e: &Arc<Entry>) -> Option<String> {
    let (have, stale) = {
        let mut c = cache().lock().unwrap();
        match c.get_mut(&e.id) {
            Some(s) if s.at.elapsed() < FRESH || s.busy => (true, s.text.clone()),
            Some(s) => {
                s.busy = true;
                (false, s.text.clone())
            }
            None => (false, None),
        }
    };
    if have {
        return stale;
    }
    let first = !cache().lock().unwrap().contains_key(&e.id);
    if first {
        // Nothing to show yet: fill in now, without RCON (which can take
        // seconds on a busy server), and the full one in the background.
        let e2 = e.clone();
        let text = tokio::task::spawn_blocking(move || fill(&e2, false)).await.ok().flatten();
        cache().lock().unwrap().insert(e.id.clone(), Slot { at: Instant::now().checked_sub(FRESH).unwrap_or_else(Instant::now), text: text.clone(), busy: true });
        refill(e.clone());
        return text;
    }
    refill(e.clone());
    stale
}

fn refill(e: Arc<Entry>) {
    tokio::task::spawn_blocking(move || {
        let text = fill(&e, true);
        cache().lock().unwrap().insert(e.id.clone(), Slot { at: Instant::now(), text, busy: false });
    });
}
