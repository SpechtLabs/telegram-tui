//! The ONLY path to the OTLP exporter. The subscriber layer in tgt-app
//! exports only events carrying `telemetry.public` AND target
//! `"tgt_telemetry"`; everything else stays in the local rolling file.
//! `#[macro_export]` places this at the crate root, so callers invoke it as
//! `tgt_core::emit!(...)`.

#[macro_export]
macro_rules! emit {
    ($event:expr) => {{
        let __ev: $crate::telemetry::TelemetryEvent = $event;
        ::tracing::info!(
            target: "tgt_telemetry",
            action = __ev.action,
            telemetry.public = true,
            outcome = __ev.outcome.as_str(),
            error.kind = __ev.error_kind,
            duration_ms = __ev.duration_ms,
            chat.kind = __ev.chat_kind,
            chat.hash = __ev.chat_hash.as_deref(),
            history.page_depth = __ev.history_page_depth,
            download.size_bucket = __ev.download_size_bucket,
        );
    }};
}
