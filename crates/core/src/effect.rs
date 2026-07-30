//! Everything `App::update` may ask the outside world to do.
//! See docs/architecture.md §4.4.

use std::path::PathBuf;

use crate::td::request::TdRequest;
use crate::telemetry::TelemetryEvent;

#[derive(Debug, Clone)]
pub enum Effect {
    /// Execute a TDLib request. Completion re-enters as `Action::TdResult`.
    Td(TdRequest),
    /// Emit an allowlisted telemetry event (dispatcher calls `emit!`).
    Telemetry(TelemetryEvent),
    /// Ring the terminal: OSC 777 with a GENERIC body, or BEL fallback.
    /// Deliberately carries no payload — PII cannot ride on it structurally.
    Alert,
    CopyToClipboard {
        text: String,
    },
    OpenExternal {
        path: PathBuf,
    },
    SaveConfig(ConfigPatch),
    Quit,
}

/// The only config mutations `update()` may request.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigPatch {
    Theme(String),
    TelemetryMode(TelemetryMode),
    Credentials { api_id: i32, api_hash: String },
    ConsentAcknowledged { enabled: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryMode {
    Vendor,
    Custom,
    Off,
}
