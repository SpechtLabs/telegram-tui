//! Monotonic time. See docs/architecture.md §4.1.

use serde::{Deserialize, Serialize};

/// Monotonic milliseconds since process start. Injected via `Action::Tick`;
/// `update()` never reads a clock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Millis(pub u64);

impl Millis {
    pub fn saturating_add(self, ms: u64) -> Millis {
        Millis(self.0.saturating_add(ms))
    }
}
