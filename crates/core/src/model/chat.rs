//! Chat model. See docs/architecture.md §4.2.

use crate::model::ids::ChatId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatKind {
    Private,
    Group,
    Supergroup,
    Channel,
}

impl ChatKind {
    /// Allowlisted telemetry value (`chat.kind`).
    pub fn telemetry_str(self) -> &'static str {
        match self {
            ChatKind::Private => "private",
            ChatKind::Group => "group",
            ChatKind::Supergroup => "supergroup",
            ChatKind::Channel => "channel",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChatListId {
    Main,
    Archive,
    Folder(i32),
}

/// A `ChatFolderInfo`'s title, keyed by the same `i32` a chat's
/// `ChatListId::Folder` names (task #60). TDLib's `updateChatFolders`
/// delivers the *complete* current set of folders on every call — a rename,
/// a deletion, the first sync — so this carries only what a v1 sidebar tab
/// needs; icon, color and sharing flags are TDLib fields this client
/// doesn't render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderInfo {
    pub id: i32,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatPositionEntry {
    pub list: ChatListId,
    /// TDLib's order. 0 means "remove from this list". NEVER computed locally.
    pub order: i64,
    pub is_pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatView {
    pub id: ChatId,
    pub kind: ChatKind,
    pub title: String,
    pub positions: Vec<ChatPositionEntry>,
    pub unread_count: u32,
    pub unread_mention_count: u32,
    pub last_message: Option<MessagePreview>,
    pub is_muted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessagePreview {
    pub sender_name: String,
    pub text: String,
    pub date: i64,
    pub is_outgoing: bool,
}

/// Sort key mirroring TDLib: (order DESC, chat_id DESC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatOrderKey {
    pub order: i64,
    pub chat_id: ChatId,
}

impl Ord for ChatOrderKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .order
            .cmp(&self.order)
            .then(other.chat_id.cmp(&self.chat_id))
    }
}
impl PartialOrd for ChatOrderKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn chat_order_key_sorts_desc_by_order_then_id() {
        let mut set = BTreeSet::new();
        set.insert(ChatOrderKey {
            order: 10,
            chat_id: ChatId(1),
        });
        set.insert(ChatOrderKey {
            order: -5,
            chat_id: ChatId(2),
        });
        set.insert(ChatOrderKey {
            order: 100,
            chat_id: ChatId(3),
        });
        set.insert(ChatOrderKey {
            order: 100,
            chat_id: ChatId(7),
        });
        set.insert(ChatOrderKey {
            order: 0,
            chat_id: ChatId(4),
        });

        let ordered: Vec<ChatOrderKey> = set.into_iter().collect();
        assert_eq!(
            ordered,
            vec![
                ChatOrderKey {
                    order: 100,
                    chat_id: ChatId(7)
                },
                ChatOrderKey {
                    order: 100,
                    chat_id: ChatId(3)
                },
                ChatOrderKey {
                    order: 10,
                    chat_id: ChatId(1)
                },
                ChatOrderKey {
                    order: 0,
                    chat_id: ChatId(4)
                },
                ChatOrderKey {
                    order: -5,
                    chat_id: ChatId(2)
                },
            ]
        );
    }

    fn sample_chat() -> ChatView {
        ChatView {
            id: ChatId(1),
            kind: ChatKind::Supergroup,
            title: "Rust Nerds".to_string(),
            positions: vec![ChatPositionEntry {
                list: ChatListId::Main,
                order: 42,
                is_pinned: true,
            }],
            unread_count: 3,
            unread_mention_count: 1,
            last_message: Some(MessagePreview {
                sender_name: "Ada".to_string(),
                text: "hello".to_string(),
                date: 1_700_000_000,
                is_outgoing: false,
            }),
            is_muted: false,
        }
    }

    #[test]
    fn chat_view_serde_round_trips() {
        let chat = sample_chat();
        let json = serde_json::to_string(&chat).unwrap();
        let back: ChatView = serde_json::from_str(&json).unwrap();
        assert_eq!(chat, back);
    }
}
