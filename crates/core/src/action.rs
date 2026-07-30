//! The single input enum fed into `App::update`. See docs/architecture.md §4.3.

use std::path::PathBuf;

use crate::model::ids::{ChatId, FileId, MessageId};
use crate::model::key::Key;
use crate::model::message::{FileSnapshot, MessageView};
use crate::model::time::Millis;
use crate::td::error::TdError;
use crate::td::update::TdUpdate;

#[derive(Debug, Clone)]
pub enum Action {
    /// A key press, unrouted. `update()` routes modal → focused pane → global.
    Key(Key),
    /// Bracketed paste (terminals paste dropped files as plain text paths).
    Paste(String),
    Resize {
        width: u16,
        height: u16,
    },
    /// Periodic housekeeping tick (250 ms). Carries injected time.
    Tick {
        now: Millis,
    },
    /// Pre-digested TDLib push update.
    Td(TdUpdate),
    /// Completion of a dispatched `Effect::Td(_)` request.
    TdResult(TdResult),
    /// Completion of a dispatched non-TDLib effect.
    Io(IoResult),
}

/// Domain-specific completions: the dispatcher maps (request, response) pairs
/// into these mechanically. No correlation tokens; the domain context rides
/// along in the variant. (Judgment call, see architecture §8.)
// Architecture §4.3 defines this enum verbatim (renaming/boxing variants
// requires editing architecture.md first, per the plan's execution rules);
// MessageSent's payload is legitimately the largest variant, so the size
// lint is silenced rather than worked around locally.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum TdResult {
    /// Result of an auth submission (phone / code / password / QR request).
    AuthRequestDone {
        outcome: Result<(), TdError>,
    },
    ChatsLoaded {
        outcome: Result<(), TdError>,
    },
    HistoryLoaded {
        chat_id: ChatId,
        only_local: bool,
        outcome: Result<Vec<MessageView>, TdError>,
    },
    /// sendMessage returned: the optimistic message with its temporary id.
    MessageSent {
        chat_id: ChatId,
        outcome: Result<MessageView, TdError>,
    },
    EditDone {
        chat_id: ChatId,
        message_id: MessageId,
        outcome: Result<(), TdError>,
    },
    DeleteDone {
        chat_id: ChatId,
        outcome: Result<(), TdError>,
    },
    ForwardDone {
        to_chat_id: ChatId,
        outcome: Result<(), TdError>,
    },
    ReactionDone {
        chat_id: ChatId,
        message_id: MessageId,
        outcome: Result<(), TdError>,
    },
    DownloadStarted {
        file_id: FileId,
        outcome: Result<FileSnapshot, TdError>,
    },
    SearchDone {
        chat_id: ChatId,
        outcome: Result<Vec<MessageId>, TdError>,
    },
    LogOutDone {
        outcome: Result<(), TdError>,
    },
}

#[derive(Debug, Clone)]
pub enum IoResult {
    ClipboardCopied {
        outcome: Result<(), IoErrorKind>,
    },
    ExternalOpened {
        path: PathBuf,
        outcome: Result<(), IoErrorKind>,
    },
    ConfigSaved {
        outcome: Result<(), IoErrorKind>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoErrorKind {
    Denied,
    NotFound,
    Other,
}
