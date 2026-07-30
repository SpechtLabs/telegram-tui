//! Per-chat message window. See docs/architecture.md §4.6. Handlers land in
//! T16; the `selection` field is added in T26.

use std::collections::{BTreeSet, VecDeque};

use crate::model::ids::{ChatId, MessageId};
use crate::model::message::MessageView;
use crate::state::history::PagingState;

/// Bounded loaded window: memory stays flat in long-lived sessions.
pub const WINDOW_MAX_MESSAGES: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    /// Pinned to newest; new messages keep the view at the bottom.
    Bottom,
    /// Anchored at a message (stable across prepends), offset in laid-out lines.
    At {
        message_id: MessageId,
        line_offset: u16,
    },
}

#[derive(Debug)]
pub struct ConversationState {
    pub chat_id: ChatId,
    /// Ascending by message id; prepend on page, append on new message.
    pub messages: VecDeque<MessageView>,
    pub paging: PagingState,
    pub scroll: Scroll,
    pub revealed_spoilers: BTreeSet<MessageId>,
    pub last_read_inbox: MessageId,
    pub last_read_outbox: MessageId,
    /// In-chat search hits (populated by state/search.rs).
    pub search_hits: Vec<MessageId>,
}
