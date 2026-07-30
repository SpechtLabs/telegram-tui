//! The history paging state machine — struct/enum/constants only.
//! See docs/architecture.md §4.6. `on_scroll_near_top`, `on_history_loaded`,
//! and `on_history_error` are T17's; do not add them here.

use crate::model::ids::MessageId;
use crate::model::time::Millis;

pub const PAGE_SIZE: u8 = 50;
/// Trigger paging when the scroll anchor is within this many MESSAGES of the
/// oldest loaded one (core counts messages, not rows: rows are a ui concept).
pub const PAGE_TRIGGER_MESSAGES: usize = 20;
/// An empty response is NOT end-of-history (spec §5.2): retry with
/// only_local = false up to this bound before believing TDLib.
pub const MAX_EMPTY_ATTEMPTS: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagingState {
    Idle,
    Loading {
        attempt: u8,
        only_local: bool,
    },
    /// FloodWait or transient error: no requests until `until`.
    Cooldown {
        until: Millis,
    },
    /// Only entered when a non-local request came back empty at max attempts.
    Exhausted,
}

/// What the caller (conversation.rs) must do after feeding an event in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PagingDirective {
    None,
    /// Issue Effect::Td(GetChatHistory { from_message_id, limit: PAGE_SIZE, only_local }).
    Request {
        from_message_id: MessageId,
        only_local: bool,
    },
}
