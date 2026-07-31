//! Message model. See docs/architecture.md §4.2.

use crate::model::entity::FormattedText;
use crate::model::ids::{ChatId, FileId, MessageId, UserId};
use crate::td::error::TdError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageView {
    pub id: MessageId,
    pub chat_id: ChatId,
    pub sender: Sender,
    pub sender_name: String,
    pub is_outgoing: bool,
    /// Unix seconds as delivered by TDLib. Formatting is a ui concern.
    pub date: i64,
    pub content: MessageContent,
    pub reply_to: Option<ReplyPreview>,
    pub send_state: SendState,
    pub reactions: Vec<ReactionView>,
    pub caps: MessageCaps,
    pub is_edited: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Sender {
    User(UserId),
    Chat(ChatId), // channel posts, anonymous admins
}

impl Sender {
    /// Stable value for deterministic per-sender accent color derivation.
    pub fn color_seed(&self) -> i64 {
        match self {
            Sender::User(u) => u.0,
            Sender::Chat(c) => c.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MessageContent {
    Text(FormattedText),
    Photo {
        file_id: FileId,
        width: u32,
        height: u32,
        caption: FormattedText,
    },
    Video {
        file_id: FileId,
        file_name: String,
        size: u64,
        duration_secs: u32,
        caption: FormattedText,
    },
    Audio {
        file_id: FileId,
        file_name: String,
        size: u64,
        duration_secs: u32,
    },
    Document {
        file_id: FileId,
        file_name: String,
        size: u64,
        caption: FormattedText,
    },
    Sticker {
        emoji: String,
    },
    Unsupported {
        description: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SendState {
    /// Optimistic: appended from the sendMessage response (temporary id).
    Sending,
    /// Confirmed by updateMessageSendSucceeded (final id).
    Sent,
    Failed(TdError),
}
// "Read" (✓✓) is not a SendState: it is derived at render time from
// `ConversationState.last_read_outbox >= message.id`.

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionView {
    pub emoji: String,
    pub count: u32,
    pub chosen_by_me: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyPreview {
    pub message_id: MessageId,
    pub sender_name: String,
    /// Single line, pre-truncated by the runtime mapping layer.
    pub excerpt: String,
}

/// Mirrors TDLib's per-message capability flags verbatim. Chips derive from
/// these and are never hardcoded (spec §5.3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageCaps {
    pub can_be_edited: bool,
    pub can_be_deleted_for_all_users: bool,
    pub can_be_deleted_only_for_self: bool,
    pub can_be_forwarded: bool,
    pub can_be_saved: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileSnapshot {
    pub id: FileId,
    pub expected_size: u64,
    pub downloaded_size: u64,
    /// Bytes TDLib has uploaded so far, from `file.remote.uploaded_size`.
    /// Zero for anything that is not being sent.
    ///
    /// Separate from `downloaded_size` because TDLib keeps them in two
    /// different halves of the same `updateFile` — `local` and `remote` —
    /// and a message can be neither, either, or (for a forward) both. An
    /// outgoing message's progress bar has no other source.
    pub uploaded_size: u64,
    pub is_downloading: bool,
    pub is_completed: bool,
    pub local_path: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sender_color_seed_stable() {
        assert_eq!(Sender::User(UserId(42)).color_seed(), 42);
        assert_eq!(Sender::Chat(ChatId(-100)).color_seed(), -100);
    }

    fn sample_message() -> MessageView {
        MessageView {
            id: MessageId(1),
            chat_id: ChatId(2),
            sender: Sender::User(UserId(3)),
            sender_name: "Ada".to_string(),
            is_outgoing: true,
            date: 1_700_000_000,
            content: MessageContent::Text(FormattedText {
                text: "hello".to_string(),
                entities: Vec::new(),
            }),
            reply_to: Some(ReplyPreview {
                message_id: MessageId(0),
                sender_name: "Bob".to_string(),
                excerpt: "hi there".to_string(),
            }),
            send_state: SendState::Failed(TdError::FloodWait { seconds: 5 }),
            reactions: vec![ReactionView {
                emoji: "👍".to_string(),
                count: 1,
                chosen_by_me: true,
            }],
            caps: MessageCaps {
                can_be_edited: true,
                can_be_deleted_for_all_users: true,
                can_be_deleted_only_for_self: false,
                can_be_forwarded: true,
                can_be_saved: true,
            },
            is_edited: false,
        }
    }

    #[test]
    fn message_view_serde_round_trips() {
        let msg = sample_message();
        let json = serde_json::to_string(&msg).unwrap();
        let back: MessageView = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }
}
