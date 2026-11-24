//! An engine's saved state, `data/sync/<copy>.json` (atomic write). IO.

use super::engine::Engine;
use std::path::Path;

pub fn load(data: &Path, copy: &str) -> anyhow::Result<Option<Engine>> {
    let _ = (data, copy);
    todo!("P5: load")
}

pub fn save(data: &Path, engine: &Engine) -> anyhow::Result<()> {
    let _ = (data, engine);
    todo!("P5: save")
}
