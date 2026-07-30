//! TDLib error type. See docs/architecture.md §4.7.
//!
//! Ownership note: transferred from T05 to T02 by the orchestrator, because
//! `model/message.rs` (`SendState::Failed`) embeds `TdError` and T05 depends
//! on T02 — keeping it on T05 would create a cycle. `telemetry_kind` returns
//! string literals matching `schema::error_kinds`; it deliberately does not
//! import the telemetry module (T03 builds it in parallel).

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
pub enum TdError {
    #[error("flood wait: retry in {seconds}s")]
    FloodWait { seconds: u32 },
    #[error("phone number invalid")]
    PhoneNumberInvalid,
    #[error("code invalid")]
    CodeInvalid,
    #[error("password invalid")]
    PasswordInvalid,
    #[error("unauthorized")]
    Unauthorized,
    #[error("network timeout")]
    NetTimeout,
    #[error("offline")]
    Offline,
    #[error("td error {code}: {message}")]
    Other { code: i32, message: String },
}

impl TdError {
    /// Allowlisted telemetry value (`error.kind`), from schema::error_kinds.
    pub fn telemetry_kind(&self) -> &'static str {
        match self {
            TdError::FloodWait { .. } => "td.flood_wait",
            TdError::PhoneNumberInvalid
            | TdError::CodeInvalid
            | TdError::PasswordInvalid
            | TdError::Unauthorized => "td.auth",
            TdError::NetTimeout => "net.timeout",
            TdError::Offline => "net.offline",
            TdError::Other { .. } => "td.other",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flood_wait_maps_to_td_flood_wait_kind() {
        assert_eq!(
            TdError::FloodWait { seconds: 42 }.telemetry_kind(),
            "td.flood_wait"
        );
    }

    #[test]
    fn auth_variants_map_to_td_auth_kind() {
        assert_eq!(TdError::PhoneNumberInvalid.telemetry_kind(), "td.auth");
        assert_eq!(TdError::CodeInvalid.telemetry_kind(), "td.auth");
        assert_eq!(TdError::PasswordInvalid.telemetry_kind(), "td.auth");
        assert_eq!(TdError::Unauthorized.telemetry_kind(), "td.auth");
    }

    #[test]
    fn net_timeout_maps_to_net_timeout_kind() {
        assert_eq!(TdError::NetTimeout.telemetry_kind(), "net.timeout");
    }

    #[test]
    fn offline_maps_to_net_offline_kind() {
        assert_eq!(TdError::Offline.telemetry_kind(), "net.offline");
    }

    #[test]
    fn other_maps_to_td_other_kind() {
        assert_eq!(
            TdError::Other {
                code: 500,
                message: "boom".into()
            }
            .telemetry_kind(),
            "td.other"
        );
    }
}
