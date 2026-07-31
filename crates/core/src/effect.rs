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

/// Whether this session may report anything at all.
///
/// A master switch, not a destination. Which egresses a session actually has
/// — the project's Sentry project for crash reports, a user-configured OTLP
/// collector, both, or neither — is decided in `tgt-app` from `[telemetry]`
/// and from what was baked in at build time. `tgt-core` has no business
/// knowing any of that; all it needs is whether minting a `TelemetryEvent`
/// is permitted this run.
///
/// `Off` is what `--no-telemetry`, `TELEGRAM_TUI_TELEMETRY=off`,
/// `DO_NOT_TRACK`, and a Disable at the first-run consent screen all resolve
/// to, and it disables *every* egress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryMode {
    On,
    Off,
}
