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
/// The store's own 50 ms message poll (not changed here).
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
