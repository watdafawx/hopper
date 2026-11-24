//! Making a plan on disk (design §7). IO: staged fetches, a journal, then
//! the swap; one outcome per step so a failure never loses whose a change is.

use super::model::{Dot, Key};
use super::plan::ApplyPlan;
use crate::jobs::Job;
use std::path::Path;

/// Gets one file of a step into the staging folder.
pub trait Fetch {
    fn fetch(&self, file: &str, into: &Path) -> anyhow::Result<()>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    Failed(String),
    /// The author blocked downloads (CurseForge 403): by hand.
    Blocked,
    Retry { after_secs: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepResult {
    pub key: Key,
    pub by: Dot,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApplyReport {
    pub results: Vec<StepResult>,
}

pub fn run(root: &Path, plan: &ApplyPlan, fetch: &dyn Fetch, job: &Job) -> ApplyReport {
    let _ = (root, plan, fetch, job);
    todo!("P6: staged, journaled apply")
}

/// Finishes or rolls back a swap a crash cut short.
pub fn recover(root: &Path) -> anyhow::Result<Option<ApplyReport>> {
    let _ = root;
    todo!("P6: journal recovery")
}
