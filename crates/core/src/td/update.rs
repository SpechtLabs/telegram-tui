//! TDLib update projections. See docs/architecture.md §4.7.

use crate::model::chat::{ChatPositionEntry, ChatView, MessagePreview};
use crate::model::ids::{ChatId, MessageId, UserId};
use crate::model::message::{FileSnapshot, MessageContent, MessageView, ReactionView};
use crate::td::error::TdError;
use serde::{Deserialize, Serialize};

/// Defined here (not in state/presence.rs) because TdUpdate carries it and the
/// td types are implemented before the state handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PresenceStatus {
    Online,
    Recently,
    Offline,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AuthPhase {
    WaitTdlibParameters,
    WaitPhoneNumber,
    WaitCode {
        delivery_hint: String,
        length: u8,
    },
    WaitPassword {
        hint: Option<String>,
    },
    WaitOtherDeviceConfirmation {
        link: String,
    },
    Ready,
    LoggingOut,
    Closing,
    Closed,
    /// States v1 does not implement (e.g. registration): rendered as a
    /// dead-end screen with the state name, never silently swallowed.
    Unsupported {
        name: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionPhase {
    WaitingForNetwork,
    Connecting,
    ConnectingToProxy,
    Updating,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TdUpdate {
    Auth(AuthPhase),
    Connection(ConnectionPhase),
    NewChat(ChatView),
    ChatPosition {
        chat_id: ChatId,
        position: ChatPositionEntry,
    },
    ChatLastMessage {
        chat_id: ChatId,
        preview: Option<MessagePreview>,
        positions: Vec<ChatPositionEntry>,
    },
    ChatReadInbox {
        chat_id: ChatId,
        last_read_inbox_message_id: MessageId,
        unread_count: u32,
    },
    ChatReadOutbox {
        chat_id: ChatId,
        last_read_outbox_message_id: MessageId,
    },
    ChatTitle {
        chat_id: ChatId,
        title: String,
    },
    ChatUnreadMentionCount {
        chat_id: ChatId,
        count: u32,
    },
    ChatNotificationSettings {
        chat_id: ChatId,
        muted: bool,
    },
    NewMessage(MessageView),
    MessageSendSucceeded {
        chat_id: ChatId,
        old_message_id: MessageId,
        message: MessageView,
    },
    MessageSendFailed {
        chat_id: ChatId,
        old_message_id: MessageId,
        error: TdError,
    },
    MessageContentChanged {
        chat_id: ChatId,
        message_id: MessageId,
        content: MessageContent,
    },
    MessageInteractionInfo {
        chat_id: ChatId,
        message_id: MessageId,
        reactions: Vec<ReactionView>,
    },
    MessagesDeleted {
        chat_id: ChatId,
        message_ids: Vec<MessageId>,
    },
    File(FileSnapshot),
    UserStatus {
        user_id: UserId,
        status: PresenceStatus,
    },
    ChatAction {
        chat_id: ChatId,
        user_id: UserId,
        is_typing: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::chat::{ChatKind, ChatListId};

    #[test]
    fn auth_wait_code_serde_round_trips() {
        let update = TdUpdate::Auth(AuthPhase::WaitCode {
            delivery_hint: "SMS to +1***34".to_string(),
            length: 5,
        });
        let json = serde_json::to_string(&update).unwrap();
        let back: TdUpdate = serde_json::from_str(&json).unwrap();
        assert_eq!(update, back);
    }

    #[test]
    fn auth_unsupported_serde_round_trips() {
        let update = TdUpdate::Auth(AuthPhase::Unsupported {
            name: "authorizationStateWaitRegistration".to_string(),
        });
        let json = serde_json::to_string(&update).unwrap();
        let back: TdUpdate = serde_json::from_str(&json).unwrap();
        assert_eq!(update, back);
    }

    #[test]
    fn chat_last_message_with_positions_serde_round_trips() {
        let update = TdUpdate::ChatLastMessage {
            chat_id: ChatId(1),
            preview: Some(MessagePreview {
                sender_name: "Ada".to_string(),
                text: "hello".to_string(),
                date: 1_700_000_000,
                is_outgoing: false,
            }),
            positions: vec![ChatPositionEntry {
                list: ChatListId::Main,
                order: 42,
                is_pinned: false,
            }],
        };
        let json = serde_json::to_string(&update).unwrap();
        let back: TdUpdate = serde_json::from_str(&json).unwrap();
        assert_eq!(update, back);
    }

    #[test]
    fn new_chat_serde_round_trips() {
        let update = TdUpdate::NewChat(ChatView {
            id: ChatId(1),
            kind: ChatKind::Private,
            title: "Ada".to_string(),
            positions: Vec::new(),
            unread_count: 0,
            unread_mention_count: 0,
            last_message: None,
            is_muted: false,
        });
        let json = serde_json::to_string(&update).unwrap();
        let back: TdUpdate = serde_json::from_str(&json).unwrap();
        assert_eq!(update, back);
    }

    #[test]
    fn chat_action_serde_round_trips() {
        let update = TdUpdate::ChatAction {
            chat_id: ChatId(1),
            user_id: UserId(2),
            is_typing: true,
        };
        let json = serde_json::to_string(&update).unwrap();
        let back: TdUpdate = serde_json::from_str(&json).unwrap();
        assert_eq!(update, back);
    }

    #[test]
    fn message_send_failed_serde_round_trips() {
        let update = TdUpdate::MessageSendFailed {
            chat_id: ChatId(1),
            old_message_id: MessageId(-1),
            error: TdError::FloodWait { seconds: 30 },
        };
        let json = serde_json::to_string(&update).unwrap();
        let back: TdUpdate = serde_json::from_str(&json).unwrap();
        assert_eq!(update, back);
    }
}
