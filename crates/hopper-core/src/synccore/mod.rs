//! The friends group sync core, rewritten (design/sync-rewrite.md).
//!
//! A change is a record made when Hopper makes it: what it was, what it is
//! now, who made it, and what they had already seen. Nothing here guesses
//! a change from two mod lists. The pure parts (`model`, `register`,
//! `gate`, `local`, `plan`, `wire`, `engine`) never touch the disk, the
//! network or the clock; `apply` and `store` do the IO.
//!
//! Skeleton only: the bodies are `todo!()` until the phases in the design
//! fill them in. The scenarios in `tests/sync_scenarios/` are written
//! against this API and un-ignored phase by phase.

pub mod apply;
pub mod engine;
pub mod gate;
pub mod local;
pub mod model;
pub mod plan;
pub mod register;
#[doc(hidden)]
pub mod sim;
pub mod store;
pub mod wire;

pub use engine::{Answer, Engine, Received, Review, Status};
pub use gate::{Limits, Risk, Trust};
pub use local::{Drift, Snapshot};
pub use model::{Arrival, Change, ConfigMode, Dot, Edit, Key, PackMode, Seen, Value, Why};
pub use plan::{ApplyPlan, FriendPlan, PackPlan, Step, Wait};
