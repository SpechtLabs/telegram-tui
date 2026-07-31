//! Semantic mouse targets. Plain data, no ratatui types (architecture §7.5):
//! resolving screen coordinates to a target is a `tgt-ui` concern (the
//! `HitMap` built fresh on every frame, T58), so `update()` never sees a
//! `Rect` or a cell position — only what was hit.

use serde::{Deserialize, Serialize};

use crate::model::chat::ChatListId;
use crate::model::ids::{ChatId, MessageId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HitTarget {
    ChatRow(ChatId),
    ArchiveRow,
    FolderTab(ChatListId),
    Message(MessageId),
    Composer,
    /// A masked spoiler run's cells (architecture §7.5.1): sub-row, and
    /// only pushed for the columns the block glyphs actually occupy.
    Spoiler(MessageId),
    /// A reply-quote line's cells (architecture §7.5.1). `quoted` is the
    /// jump target; `containing` is the message the line's block belongs
    /// to, so right-click still enters selection on the right message even
    /// though the quoted message may not be loaded at all.
    ReplyQuote {
        containing: MessageId,
        quoted: MessageId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClickButton {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScrollArea {
    ChatList,
    Conversation,
}
