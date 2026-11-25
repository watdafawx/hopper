//! Scenarios for the rewritten sync core (design/sync-rewrite.md), against
//! `hopper_core::synccore::sim`: an in-memory group with a relay that keeps
//! only the newest event per sender and key, an offline switch per person,
//! fake mods folders, restarts and failing downloads. No Python, no ports,
//! no sleeps; relay time moves only with `advance`.
//!
//! Every test is `#[ignore = "sync-rewrite"]` until the phase that makes it
//! pass un-ignores it (the phase table in the design). Each one names the
//! audit items it covers: W1..W37 (workflow catalogue, in the audit's
//! order), R1..R5 (top risks), R6 (Minecraft version on plain instances),
//! R7 (150-pack queue drop), C1..C7 (fix-history causes, in the audit's
//! order), and D (a decision from the design's §14).
//!
//! Run them while building a phase:
//!
//! ```text
//! cargo test -p hopper-core --test sync_scenarios rewrite:: -- --ignored
//! ```
//!
//! Mods: mod n is CurseForge project n; its files are n*10+1, n*10+2...
//! The group starts with mods 1 to 6 on their first file, all on.

#![allow(dead_code)]

mod apply;
mod away;
mod clashes;
mod disk;
mod diverge;
mod everyday;
mod membership;
mod packs;
mod undo;
mod wire;

use hopper_core::synccore::sim::Group;
use hopper_core::synccore::{Key, Value};

/// Mod n's key.
pub fn m(n: u64) -> Key {
    Key::Mod(format!("cf:{n}"))
}

/// A file, on.
pub fn f(file: u64) -> Value {
    Value::File { file: format!("cf:{file}"), on: true }
}

/// A file, off.
pub fn off(file: u64) -> Value {
    Value::File { file: format!("cf:{file}"), on: false }
}

/// Mods 1 to 6 on their first file.
pub fn base() -> Vec<(Key, Value)> {
    (1..=6).map(|n| (m(n), f(n * 10 + 1))).collect()
}

/// A, B and C in one group, everyone online and in step.
pub fn three() -> Group {
    Group::with(&["A", "B", "C"], &base())
}

pub const DAY: u64 = 24 * 3600;
