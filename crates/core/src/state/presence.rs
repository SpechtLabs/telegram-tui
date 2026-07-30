//! Presence/typing state. See docs/architecture.md §4.6. Handlers land in T34.

use std::collections::HashMap;

use crate::model::ids::{ChatId, UserId};
use crate::model::time::Millis;
use crate::td::update::PresenceStatus;

pub const TYPING_TTL_MS: u64 = 6_000;

#[derive(Debug, Default)]
pub struct PresenceState {
    pub users: HashMap<UserId, PresenceStatus>,
    /// (chat, user) → expiry; swept on Tick.
    pub typing: HashMap<(ChatId, UserId), Millis>,
}
