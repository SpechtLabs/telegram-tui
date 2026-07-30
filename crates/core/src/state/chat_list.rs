//! Chat list state: mirrors TDLib ordering verbatim. See docs/architecture.md
//! §4.6. Handlers land in T15.

use std::collections::{BTreeSet, HashMap};

use crate::model::chat::{ChatListId, ChatOrderKey, ChatView};
use crate::model::ids::ChatId;
use crate::state::auth::InputField;

// `ChatListState` below derives `Default`, which requires every field to be
// `Default`. Architecture §4.6's listing gives `ChatLoadPhase` no `#[default]`
// variant; adding one here (Idle, the natural starting phase) is the minimal
// deviation that makes the derive on ChatListState compile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ChatLoadPhase {
    #[default]
    Idle,
    Loading,
    Complete,
}

#[derive(Debug, Default)]
pub struct ChatListState {
    pub chats: HashMap<ChatId, ChatView>,
    /// One TDLib-mirrored order set per chat list. Never computed locally.
    pub orders: HashMap<ChatListId, BTreeSet<ChatOrderKey>>,
    pub active_list: ChatListId,
    pub selected: Option<ChatId>,
    pub filter: Option<InputField>,
    pub scroll_offset: usize,
    pub load: ChatLoadPhase,
}

// ChatListId is a foreign type (owned by T02's model/chat.rs, not touched
// here); its derive list can't gain `Default`/`#[default]`, so this stays a
// manual impl per architecture §4.6.
#[allow(clippy::derivable_impls)]
impl Default for ChatListId {
    fn default() -> Self {
        ChatListId::Main
    }
}
