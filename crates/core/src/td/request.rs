//! TDLib request/response types. See docs/architecture.md §4.7.

use crate::model::chat::ChatListId;
use crate::model::entity::FormattedText;
use crate::model::ids::{ChatId, FileId, MessageId};
use crate::model::message::{FileSnapshot, MessageView};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TdlibParams {
    pub api_id: i32,
    pub api_hash: String,
    pub database_directory: PathBuf, // ~/.local/share/telegram-tui/td/, mode 0700
    pub database_encryption_key: Vec<u8>, // 32 bytes from macOS Keychain
    pub use_message_database: bool,  // true
    pub use_chat_info_database: bool, // true
    pub use_file_database: bool,     // true
    pub use_secret_chats: bool,      // false — spec non-goal
    pub system_language_code: String,
    pub device_model: String,
    pub application_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OutgoingFileKind {
    Photo,
    Video,
    Audio,
    Document,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TdRequest {
    SetTdlibParameters(TdlibParams),
    SetAuthenticationPhoneNumber {
        phone: String,
    },
    CheckAuthenticationCode {
        code: String,
    },
    CheckAuthenticationPassword {
        password: String,
    },
    RequestQrCodeAuthentication,
    LogOut,
    LoadChats {
        list: ChatListId,
        limit: u32,
    },
    OpenChat {
        chat_id: ChatId,
    },
    CloseChat {
        chat_id: ChatId,
    },
    GetChatHistory {
        chat_id: ChatId,
        from_message_id: MessageId,
        limit: u8,
        only_local: bool,
    },
    ViewMessages {
        chat_id: ChatId,
        message_ids: Vec<MessageId>,
    },
    SendMessageText {
        chat_id: ChatId,
        reply_to: Option<MessageId>,
        text: FormattedText,
    },
    SendMessageFile {
        chat_id: ChatId,
        path: PathBuf,
        kind: OutgoingFileKind,
        caption: Option<FormattedText>,
    },
    EditMessageText {
        chat_id: ChatId,
        message_id: MessageId,
        text: FormattedText,
    },
    DeleteMessages {
        chat_id: ChatId,
        message_ids: Vec<MessageId>,
        revoke: bool,
    },
    ForwardMessages {
        to_chat_id: ChatId,
        from_chat_id: ChatId,
        message_ids: Vec<MessageId>,
    },
    ToggleReaction {
        chat_id: ChatId,
        message_id: MessageId,
        emoji: String,
    },
    DownloadFile {
        file_id: FileId,
        priority: i8,
    },
    CancelDownloadFile {
        file_id: FileId,
    },
    SearchChatMessages {
        chat_id: ChatId,
        query: String,
        from_message_id: MessageId,
        limit: u8,
    },
}

impl TdRequest {
    /// Discriminant name for RequestMatcher::Kind and local logging.
    pub fn kind(&self) -> &'static str {
        match self {
            TdRequest::SetTdlibParameters(_) => "SetTdlibParameters",
            TdRequest::SetAuthenticationPhoneNumber { .. } => "SetAuthenticationPhoneNumber",
            TdRequest::CheckAuthenticationCode { .. } => "CheckAuthenticationCode",
            TdRequest::CheckAuthenticationPassword { .. } => "CheckAuthenticationPassword",
            TdRequest::RequestQrCodeAuthentication => "RequestQrCodeAuthentication",
            TdRequest::LogOut => "LogOut",
            TdRequest::LoadChats { .. } => "LoadChats",
            TdRequest::OpenChat { .. } => "OpenChat",
            TdRequest::CloseChat { .. } => "CloseChat",
            TdRequest::GetChatHistory { .. } => "GetChatHistory",
            TdRequest::ViewMessages { .. } => "ViewMessages",
            TdRequest::SendMessageText { .. } => "SendMessageText",
            TdRequest::SendMessageFile { .. } => "SendMessageFile",
            TdRequest::EditMessageText { .. } => "EditMessageText",
            TdRequest::DeleteMessages { .. } => "DeleteMessages",
            TdRequest::ForwardMessages { .. } => "ForwardMessages",
            TdRequest::ToggleReaction { .. } => "ToggleReaction",
            TdRequest::DownloadFile { .. } => "DownloadFile",
            TdRequest::CancelDownloadFile { .. } => "CancelDownloadFile",
            TdRequest::SearchChatMessages { .. } => "SearchChatMessages",
        }
    }
}

// Boxing `Message(MessageView)` would deviate from the verbatim contract in
// docs/architecture.md §4.7; the size skew is accepted instead.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TdResponse {
    Ok,
    Chats { chat_ids: Vec<ChatId> },
    Messages { messages: Vec<MessageView> },
    Message(MessageView),
    FoundMessages { message_ids: Vec<MessageId> },
    File(FileSnapshot),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::entity::FormattedText;
    use std::collections::HashSet;

    fn all_variants() -> Vec<TdRequest> {
        vec![
            TdRequest::SetTdlibParameters(TdlibParams {
                api_id: 1,
                api_hash: "hash".to_string(),
                database_directory: PathBuf::from("/tmp/td"),
                database_encryption_key: vec![0u8; 32],
                use_message_database: true,
                use_chat_info_database: true,
                use_file_database: true,
                use_secret_chats: false,
                system_language_code: "en".to_string(),
                device_model: "mac".to_string(),
                application_version: "0.1.0".to_string(),
            }),
            TdRequest::SetAuthenticationPhoneNumber {
                phone: "+15551234".to_string(),
            },
            TdRequest::CheckAuthenticationCode {
                code: "12345".to_string(),
            },
            TdRequest::CheckAuthenticationPassword {
                password: "hunter2".to_string(),
            },
            TdRequest::RequestQrCodeAuthentication,
            TdRequest::LogOut,
            TdRequest::LoadChats {
                list: ChatListId::Main,
                limit: 50,
            },
            TdRequest::OpenChat { chat_id: ChatId(1) },
            TdRequest::CloseChat { chat_id: ChatId(1) },
            TdRequest::GetChatHistory {
                chat_id: ChatId(1),
                from_message_id: MessageId(0),
                limit: 50,
                only_local: false,
            },
            TdRequest::ViewMessages {
                chat_id: ChatId(1),
                message_ids: vec![MessageId(1)],
            },
            TdRequest::SendMessageText {
                chat_id: ChatId(1),
                reply_to: None,
                text: FormattedText {
                    text: "hi".to_string(),
                    entities: Vec::new(),
                },
            },
            TdRequest::SendMessageFile {
                chat_id: ChatId(1),
                path: PathBuf::from("/tmp/file.jpg"),
                kind: OutgoingFileKind::Photo,
                caption: None,
            },
            TdRequest::EditMessageText {
                chat_id: ChatId(1),
                message_id: MessageId(1),
                text: FormattedText {
                    text: "edited".to_string(),
                    entities: Vec::new(),
                },
            },
            TdRequest::DeleteMessages {
                chat_id: ChatId(1),
                message_ids: vec![MessageId(1)],
                revoke: true,
            },
            TdRequest::ForwardMessages {
                to_chat_id: ChatId(2),
                from_chat_id: ChatId(1),
                message_ids: vec![MessageId(1)],
            },
            TdRequest::ToggleReaction {
                chat_id: ChatId(1),
                message_id: MessageId(1),
                emoji: "👍".to_string(),
            },
            TdRequest::DownloadFile {
                file_id: FileId(1),
                priority: 32,
            },
            TdRequest::CancelDownloadFile { file_id: FileId(1) },
            TdRequest::SearchChatMessages {
                chat_id: ChatId(1),
                query: "hello".to_string(),
                from_message_id: MessageId(0),
                limit: 50,
            },
        ]
    }

    #[test]
    fn request_kind_names_are_unique() {
        let variants = all_variants();
        let kinds: HashSet<&'static str> = variants.iter().map(TdRequest::kind).collect();
        assert_eq!(kinds.len(), variants.len());
    }

    #[test]
    fn request_kind_matches_variant_identifier() {
        assert_eq!(
            TdRequest::GetChatHistory {
                chat_id: ChatId(1),
                from_message_id: MessageId(0),
                limit: 50,
                only_local: false,
            }
            .kind(),
            "GetChatHistory"
        );
        assert_eq!(
            TdRequest::SendMessageText {
                chat_id: ChatId(1),
                reply_to: None,
                text: FormattedText {
                    text: "hi".to_string(),
                    entities: Vec::new(),
                },
            }
            .kind(),
            "SendMessageText"
        );
    }

    #[test]
    fn get_chat_history_serde_round_trips() {
        let req = TdRequest::GetChatHistory {
            chat_id: ChatId(42),
            from_message_id: MessageId(100),
            limit: 50,
            only_local: false,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: TdRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn send_message_text_serde_round_trips() {
        let req = TdRequest::SendMessageText {
            chat_id: ChatId(1),
            reply_to: Some(MessageId(7)),
            text: FormattedText {
                text: "hello world".to_string(),
                entities: Vec::new(),
            },
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: TdRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn td_response_messages_serde_round_trips() {
        let resp = TdResponse::Chats {
            chat_ids: vec![ChatId(1), ChatId(2)],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: TdResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, back);
    }
}
