//! Composer state. See docs/architecture.md §4.6. Handlers land in T25.

use std::path::PathBuf;

use crate::model::ids::MessageId;
use crate::state::auth::InputField;

#[derive(Debug, Default)]
pub struct ComposerState {
    /// Multi-line buffer; `alt+enter` inserts '\n'.
    pub input: InputField,
    pub reply_to: Option<MessageId>,
    /// When set, Enter submits an edit instead of a send.
    pub editing: Option<MessageId>,
    /// Text held while a send is in flight. Restored to `input` on failure
    /// (spec §14: send failures never discard typed text).
    pub pending_send: Option<String>,
    /// A pasted bare path that exists on disk: offer to send as file.
    pub pending_path_offer: Option<PathBuf>,
}
