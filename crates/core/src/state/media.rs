//! Download/upload tracking state. See docs/architecture.md §4.6. Handlers
//! land in T36.

use std::collections::HashMap;

use crate::model::ids::{ChatId, FileId, MessageId};
use crate::model::message::FileSnapshot;

#[derive(Debug, Default)]
pub struct MediaState {
    pub files: HashMap<FileId, FileSnapshot>,
    /// Outgoing uploads keyed by the optimistic message id.
    pub uploads: HashMap<MessageId, UploadProgress>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UploadProgress {
    pub chat_id: ChatId,
    pub uploaded: u64,
    pub total: u64,
}
