//! Counts of the timer wake-ups that can happen while Hopper sits idle, for
//! the memory log (`HOPPER_MEMLOG=1`, see `memlog.rs`). Each counter is one
//! atomic add, so leaving them in costs nothing; the log line shows them.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

pub static MCP: AtomicU64 = AtomicU64::new(0);
pub static DOWNLOADS: AtomicU64 = AtomicU64::new(0);
pub static PANEL: AtomicU64 = AtomicU64::new(0);
pub static HISTORY: AtomicU64 = AtomicU64::new(0);
/// Browse's Minecraft version list loads.
pub static VERSIONS: AtomicU64 = AtomicU64::new(0);
/// The store's own poll loop, woken by a message, a dirty flag or a job tick
/// (see `store::poll`); near zero while Hopper sits idle.
pub static STORE_POLL: AtomicU64 = AtomicU64::new(0);

pub fn hit(counter: &AtomicU64) {
    counter.fetch_add(1, Relaxed);
}

/// `name=count` for each counter, since start.
pub fn report() -> String {
    let all = [
        ("mcp", &MCP),
        ("downloads", &DOWNLOADS),
        ("panel", &PANEL),
        ("history", &HISTORY),
        ("versions", &VERSIONS),
        ("store_poll", &STORE_POLL),
    ];
    all.iter().map(|(n, c)| format!("{n}={}", c.load(Relaxed))).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_lists_every_counter_and_hit_counts() {
        let before = VERSIONS.load(Relaxed);
        hit(&VERSIONS);
        let line = report();
        for name in ["mcp=", "downloads=", "panel=", "history=", "versions=", "store_poll="] {
            assert!(line.contains(name), "{name} missing in {line}");
        }
        assert!(VERSIONS.load(Relaxed) > before);
    }
}
