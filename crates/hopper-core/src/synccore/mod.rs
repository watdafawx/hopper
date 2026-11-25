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
pub mod bridge;
pub mod cards;
pub mod digest;
pub mod engine;
pub mod gate;
pub mod link;
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

/// The id of one group copy's engine, its saved state and its events: the
/// sharer's instance inside one group. The same instance shared into two
/// groups is two copies, so neither takes in the other's records.
pub fn copy_id(group: &str, inst: &str) -> String {
    format!("{inst}@{group}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_instance_in_two_groups_is_two_copies() {
        assert_ne!(copy_id("g1", "inst"), copy_id("g2", "inst"));
        assert_ne!(copy_id("g1", "a"), copy_id("g1", "b"));
        assert_eq!(copy_id("g1", "inst"), copy_id("g1", "inst"));
        // Two groups' engines of one instance never share a state file.
        let dir = std::path::Path::new("d");
        assert_ne!(store::path(dir, &copy_id("g1", "inst")), store::path(dir, &copy_id("g2", "inst")));
    }
}
