//! How background work asks the UI to redraw, whichever UI it is.

use std::sync::Arc;

pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// A waker that does nothing (tests, headless use).
pub fn noop() -> Waker {
    Arc::new(|| {})
}
