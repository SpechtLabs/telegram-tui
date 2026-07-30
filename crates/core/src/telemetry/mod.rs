//! Telemetry: the allowlist schema, the `TelemetryEvent` builder, the
//! `emit!` macro (the only path to the OTLP exporter), and HMAC id hashing.
//! See docs/architecture.md §4.8 and spec §13.2-13.4.

pub mod emit;
pub mod hashing;
pub mod schema;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Error,
    Cancelled,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Error => "error",
            Outcome::Cancelled => "cancelled",
        }
    }
}

/// Every field is either a schema constant (&'static str) or a number/bucket.
/// Free-form strings are structurally impossible except chat_hash, which is
/// produced only by telemetry::hashing::hash_id.
#[derive(Debug, Clone, PartialEq)]
pub struct TelemetryEvent {
    pub action: &'static str, // schema::actions::*
    pub outcome: Outcome,
    pub error_kind: Option<&'static str>, // schema::error_kinds::*
    pub duration_ms: Option<u64>,
    pub chat_kind: Option<&'static str>, // ChatKind::telemetry_str()
    pub chat_hash: Option<String>,       // hashing::hash_id output only
    pub history_page_depth: Option<u32>,
    pub download_size_bucket: Option<&'static str>, // schema::buckets::download_size
}

impl TelemetryEvent {
    pub fn ok(action: &'static str) -> Self {
        Self {
            action,
            outcome: Outcome::Ok,
            error_kind: None,
            duration_ms: None,
            chat_kind: None,
            chat_hash: None,
            history_page_depth: None,
            download_size_bucket: None,
        }
    }

    pub fn error(action: &'static str, kind: &'static str) -> Self {
        Self {
            action,
            outcome: Outcome::Error,
            error_kind: Some(kind),
            duration_ms: None,
            chat_kind: None,
            chat_hash: None,
            history_page_depth: None,
            download_size_bucket: None,
        }
    }

    pub fn cancelled(action: &'static str) -> Self {
        Self {
            action,
            outcome: Outcome::Cancelled,
            error_kind: None,
            duration_ms: None,
            chat_kind: None,
            chat_hash: None,
            history_page_depth: None,
            download_size_bucket: None,
        }
    }

    pub fn with_duration(mut self, ms: u64) -> Self {
        self.duration_ms = Some(ms);
        self
    }

    pub fn with_chat_kind(mut self, kind: &'static str) -> Self {
        self.chat_kind = Some(kind);
        self
    }

    pub fn with_chat_hash(mut self, hash: String) -> Self {
        self.chat_hash = Some(hash);
        self
    }

    pub fn with_page_depth(mut self, depth: u32) -> Self {
        self.history_page_depth = Some(depth);
        self
    }

    pub fn with_download_bucket(mut self, bucket: &'static str) -> Self {
        self.download_size_bucket = Some(bucket);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_macro_compiles_with_event_builder() {
        let event = TelemetryEvent::ok(schema::actions::CHAT_OPEN)
            .with_duration(12)
            .with_chat_kind("private")
            .with_chat_hash(hashing::hash_id(&[7u8; 32], 42))
            .with_page_depth(1)
            .with_download_bucket(schema::buckets::download_size(500));

        crate::emit!(event);
    }
}
