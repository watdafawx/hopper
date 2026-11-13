//! The node tunnel (multi-panel approach A): one WebSocket the node opens
//! to its main, carrying many streams with credit-based flow control. See
//! `design/multi-panel.md`, "A0 as built". Phase A0 only: nothing in the
//! panel uses it yet except the hidden `tunnel-spike` subcommand.

// The spike uses only part of the API; later phases use the rest.
#![allow(dead_code)]

pub mod frame;
pub mod mux;
pub mod spike;
pub mod ws;
