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
