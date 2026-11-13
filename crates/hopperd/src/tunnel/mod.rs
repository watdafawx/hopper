//! The node tunnel (multi-panel approach A): one WebSocket the node opens
//! to its main, carrying many streams with credit-based flow control. See
//! `design/multi-panel.md`, "A0 as built" and "A1 as built". A1 connects
//! and keeps the connection; relaying requests over it is A2.

// A1 uses only part of the stream API; A2 uses the rest.
#![allow(dead_code)]

pub mod dial;
pub mod frame;
pub mod mux;
pub mod node;
pub mod ws;
