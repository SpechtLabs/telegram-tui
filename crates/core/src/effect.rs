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
///
/// There is deliberately no `TelemetryMode` variant. The consent screen is
/// the only thing that changes the telemetry switch, and it does so through
/// `ConsentAcknowledged`, which carries the choice *and* the acknowledgement
/// in one patch — two patches would leave a window in which the config on
/// disk said "answered" while still holding the previous switch. A variant
/// nothing constructs also reads, to anyone auditing this file, like the
/// mechanism by which the opt-out persists; it isn't, and one already
/// concluded from its absence in production that declining was broken.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigPatch {
    Theme(String),
    Credentials {
        api_id: i32,
        api_hash: String,
    },
    /// The user answered the first-run screen. `enabled` is the answer;
    /// acknowledgement is unconditional, so a Disable persists instead of
    /// re-prompting forever.
    ConsentAcknowledged {
        enabled: bool,
    },
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
