//! A Hopper Server that answers 429 ("Too many requests") is asked to
//! slow down, not down: its servers stay listed as they were, the list
//! says it's waiting, and it is asked again by itself once the wait the
//! panel sent (Retry-After) is over. Before this, one busy minute (a big
//! pack's sync) replaced the whole list with a red error and Try again.

use super::{Msg, Store};
use hopper::hopperd::{Client, Refused};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Panels that asked to slow down: panel id -> when asking is fine again.
static SLOWED: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(Default::default);

fn slowed_until(panel: &str) -> Option<Instant> {
    SLOWED.lock().unwrap_or_else(|e| e.into_inner()).get(panel).copied().filter(|t| *t > Instant::now())
}

/// Asks panel `id` for its servers (off the UI thread). Told to slow down,
/// it waits as long as the panel said and asks again, so the message only
/// comes back with a real answer; meanwhile the old list stays. A second
/// ask while one is waiting adds nothing (that one answers for both).
pub(super) fn list_servers(id: String, client: &Client) -> Msg {
    let lock = || SLOWED.lock().unwrap_or_else(|e| e.into_inner());
    // Another ask is waiting out the panel's pause: it answers for this one.
    if lock().contains_key(&id) {
        return Msg::Nothing;
    }
    loop {
        match client.server_infos() {
            Err(e) if let Some(secs) = Refused::slow_down(&e) => {
                log::info!("Hopper Server {id:?} asked to slow down; asking again in {secs} s");
                lock().insert(id.clone(), Instant::now() + Duration::from_secs(secs));
                std::thread::sleep(Duration::from_secs(secs));
            }
            r => {
                lock().remove(&id);
                return Msg::PanelServers(id, r.map_err(|e| format!("{e:#}")));
            }
        }
    }
}

impl Store {
    /// Seconds until panel `panel` is asked again, while it asked Hopper
    /// to slow down (its list shows the old servers and says so).
    pub fn panel_slowed(&self, panel: &str) -> Option<u64> {
        slowed_until(panel).map(|t| t.saturating_duration_since(Instant::now()).as_secs().max(1))
    }
}
