//! Command palette state. See docs/architecture.md §4.6. Handlers land in T41.

use crate::model::ids::ChatId;
use crate::state::auth::InputField;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandId {
    ToggleTheme,
    TelemetrySettings,
    SendFile,
    LogOut,
    Quit,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PaletteItem {
    Chat { id: ChatId, score: u32 },
    Command { id: CommandId, score: u32 },
}

#[derive(Debug)]
pub struct PaletteState {
    pub input: InputField,
    /// Ranked by nucleo match score, then chat recency (TDLib order).
    pub results: Vec<PaletteItem>,
    pub selected: usize,
}
