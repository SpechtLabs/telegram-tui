//! In-chat search state. See docs/architecture.md §4.6. Handlers land in T42.

use crate::state::auth::InputField;

#[derive(Debug, Default)]
pub struct ChatSearchState {
    pub input: InputField,
    /// Index into ConversationState.search_hits ('n'/'N' step).
    pub current_hit: usize,
    pub in_flight: bool,
}
