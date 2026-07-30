//! Toast queue state. See docs/architecture.md §4.6. Handlers land in T44.

use std::collections::VecDeque;

use crate::model::ids::ChatId;
use crate::model::time::Millis;

pub const TOAST_MAX: usize = 3;
pub const TOAST_TTL_MS: u64 = 4_000;

/// In-app only: title/body may contain chat titles and message text because
/// they never leave the terminal cell grid. Effect::Alert (the escape-sequence
/// path) carries no payload at all.
#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub chat_id: ChatId,
    pub title: String,
    pub body: String,
    pub expires_at: Millis,
}

#[derive(Debug, Default)]
pub struct ToastState {
    pub toasts: VecDeque<Toast>,
}
