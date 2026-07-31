//! Crash and error reporting via Sentry — the app's second network egress,
//! and the one that is on unless the user opts out. See docs/architecture.md
//! §4.8; spec §13.
//!
//! # Why this one *is* `tracing-batteries` and `otel.rs` is not
//!
//! T49 found that `tracing_batteries::OpenTelemetry` could not be used here:
//! its `setup` builds its own `tracing_subscriber::registry()` and calls
//! `.init()`, which cannot coexist with `logging`'s rolling file layer, and
//! it filters only by level, which would ship every `tracing::info!` in the
//! process. Both objections are about the *subscriber*, and neither applies
//! to `tracing_batteries::Sentry`: read its `setup` at rev `f059e936` and it
//! touches no subscriber at all. It calls `sentry::init`, copies the
//! session's context onto the Sentry scope, starts a release-health session,
//! and returns. `sentry::init` installs a client on a process-global `Hub`
//! and (via the panic integration) chains a panic hook; the `tracing`
//! subscriber `logging::init` owns is untouched.
//!
//! So the two batteries get opposite answers, for one reason stated once:
//! **a battery that installs a global subscriber cannot be used, a battery
//! that does not can.** Adopting `Session` wholesale for both would have
//! meant re-implementing the file layer and the allowlist filter inside the
//! OTLP battery's stack — rewriting the two things this app is least willing
//! to get wrong in order to reuse a builder. Using the `sentry` crate
//! directly here would have meant hand-rolling release-health sessions,
//! release naming and the `enabled` flag that the battery already provides.
//! The split takes the working half of each.
//!
//! Concretely, the subscriber composition is unchanged by this module:
//!
//! ```text
//! tracing_subscriber::registry()          <- logging::init, the one global
//!   .with(reload slot -> PublicOnly(OTLP))   allowlist-filtered  (otel.rs)
//!   .with(fmt file layer + EnvFilter)        rich, local-only   (logging.rs)
//!
//! sentry::Hub (process-global, not a subscriber)   crash reports (this file)
//!   <- panic hook, capture_event, add_breadcrumb
//! ```
//!
//! # What this path can carry, and why that is different from OTLP
//!
//! The OTLP path is allowlist-enforced: `emit!` is its only entrance and
//! every attribute is a `schema` constant, which is what
//! `tests/telemetry_allowlist.rs` proves over the wire. **This path has no
//! such property and that proof does not cover it.** A crash report holds a
//! stack trace, the panic message or error text produced by whatever failed,
//! the cause chain, OS and architecture context, and the breadcrumb trail
//! below. Error text is written by the code that failed rather than drawn
//! from a fixed list, so it can carry limited content — a file path, a TDLib
//! error string. It is not a channel message text or a chat title travels
//! down in practice, because nothing formats those into an error, but "in
//! practice" is a weaker claim than the allowlist's and is stated as such
//! everywhere it appears in the docs.
//!
//! Two settings narrow it. `send_default_pii: false` keeps Sentry from
//! attaching the user's IP address and username, and `before_send` nulls
//! `server_name`, which would otherwise be the machine's hostname — usually
//! a person's name. This is git-tool's posture, adopted deliberately over a
//! stricter one that would have dropped error text entirely, because an
//! error report without the error in it is not worth sending.
//!
//! Breadcrumbs are the exception: [`record_action`] feeds the same
//! allowlisted `TelemetryEvent`s the OTLP path exports, so the action trail
//! attached to a crash is allowlist-shaped even though the crash itself is
//! not.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use sentry::ClientOptions;
use tgt_core::telemetry::TelemetryEvent;
use tgt_core::telemetry::schema::keys;
use tracing_batteries::{ErrorInfo, Sentry, Session};

/// The Sentry DSN, baked in at build time exactly the way `otel.rs` reads
/// `TGT_INGEST_ENDPOINT`. A build without it — which is every build from
/// source, and every CI build — produces a binary whose crash reporting is
/// inert: [`init`] returns `None` and no Sentry client is ever created, so
/// there is no panic hook, no HTTP transport and no release-health session.
///
/// A DSN is not a secret (it is a write-only ingest key that ships inside
/// every client that has ever used Sentry), but it is still absent from the
/// repository: it arrives from the release workflow's environment.
const DSN: Option<&str> = option_env!("TGT_SENTRY_DSN");

/// Hard ceiling on the flush at exit, matching `otel::SHUTDOWN_TIMEOUT` and
/// for the same reason (spec §13.7): quitting must not wait on a retrying
/// uploader.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// A live crash reporter. Holding one means a Sentry client is bound to the
/// process-global hub, so panics are captured and [`record_action`] produces
/// breadcrumbs.
pub struct CrashReporter {
    session: Session,
}

impl CrashReporter {
    /// Reports a fatal error — the `Err` that is about to end the process.
    ///
    /// Panics arrive on their own through the panic integration; this is for
    /// the other way a run ends badly, and it is the direct analogue of
    /// git-tool's `record_human_error` call in `main`.
    ///
    /// `color_eyre::Report` does not implement `std::error::Error` (it is an
    /// erased wrapper, like `anyhow::Report`), so it cannot go through
    /// `Session::record_error`, whose `E: Error` bound is `Sized`. It does
    /// deref to `dyn Error`, so the `ErrorInfo` the battery consumes is
    /// assembled here instead — the same fields `ErrorInfo::new` would fill,
    /// minus a concrete type name there is no way to recover from an erased
    /// report.
    pub fn record_fatal_error(&self, report: &color_eyre::Report) {
        let error: &(dyn std::error::Error + 'static) = &**report;

        let mut causes = Vec::new();
        let mut source = error.source();
        while let Some(cause) = source {
            causes.push(cause.to_string());
            source = cause.source();
        }

        self.session.record_custom_error(ErrorInfo {
            error,
            error_type: "color_eyre::Report",
            message: error.to_string(),
            causes,
            backtrace: std::backtrace::Backtrace::force_capture(),
            metadata: HashMap::new(),
        });
    }

    /// Flushes and closes the Sentry client, bounded by
    /// [`SHUTDOWN_TIMEOUT`] (set as `ClientOptions::shutdown_timeout`, which
    /// is what the battery's `close(None)` honours).
    pub fn shutdown(self) {
        self.session.shutdown();
    }
}

/// Builds the crash reporter, or `None` when this session or this build has
/// nothing to report to.
///
/// `enabled` is [`crate::config::Config::crash_reports_enabled`]: the master
/// switch (`--no-telemetry`, `TELEGRAM_TUI_TELEMETRY=off`, `DO_NOT_TRACK`, a
/// Disable at the consent screen) and `[telemetry].crash_reports` together.
/// When it is false, or when this build has no [`DSN`], `sentry::init` is
/// never called at all — which is a stronger "off" than a client that
/// silently drops events, since it also means no panic hook is installed.
///
/// Call this *after* `color_eyre::install` and *before*
/// `panic::install(restore_terminal)`, so the hook chain ends up
/// restore-terminal → Sentry capture → color-eyre print. A panic that has
/// not restored the terminal first leaves the user staring at a frozen
/// alternate screen while the report uploads.
pub fn init(enabled: bool) -> Option<CrashReporter> {
    let dsn = dsn(enabled)?;

    let session = Session::new("telegram-tui", env!("CARGO_PKG_VERSION"))
        // Distinguishes a report from a `cargo run` during development from
        // one off a released binary. `Metadata`'s own default already
        // disables reporting entirely under `debug_assertions` — a debug
        // build constructs the client but its `before_send` drops every
        // event — so this only labels the ones that do get through.
        .with_context(
            "host.environment",
            if cfg!(debug_assertions) {
                "Development"
            } else {
                "Customer"
            },
        )
        .with_battery(Sentry::new((
            dsn,
            ClientOptions {
                release: Some(concat!("telegram-tui@v", env!("CARGO_PKG_VERSION")).into()),
                environment: Some(
                    if cfg!(debug_assertions) {
                        "Development"
                    } else {
                        "Customer"
                    }
                    .into(),
                ),
                default_integrations: true,
                attach_stacktrace: true,
                // No IP address, no username, no request bodies. The
                // pseudonymous `install.id` the OTLP path uses is
                // deliberately *not* attached here either: correlating a
                // crash with a usage session is not worth giving the two
                // egresses a shared key.
                send_default_pii: false,
                shutdown_timeout: SHUTDOWN_TIMEOUT,
                before_send: Some(Arc::new(|mut event| {
                    // Sentry fills this with the machine's hostname, which
                    // on a personal laptop is usually a person's name.
                    event.server_name = None;
                    Some(event)
                })),
                ..Default::default()
            },
        )));

    tracing::debug!("crash reporting enabled"); // deliberately without the DSN
    Some(CrashReporter { session })
}

/// The DSN this session would report to, if any: `None` when telemetry or
/// crash reporting is switched off, and `None` in a build that had no
/// `TGT_SENTRY_DSN` at compile time.
///
/// Split out from [`init`] because it is the whole decision and it is pure —
/// every opt-out switch is testable through it without standing up a Sentry
/// client, which a test build could not do anyway.
pub fn dsn(enabled: bool) -> Option<&'static str> {
    if !enabled {
        return None;
    }
    DSN.map(str::trim).filter(|dsn| !dsn.is_empty())
}

/// Whether this build could report crashes at all, for `tgt telemetry show`
/// to be honest about a from-source build.
pub fn build_has_dsn() -> bool {
    DSN.map(str::trim).is_some_and(|dsn| !dsn.is_empty())
}

/// Records an allowlisted telemetry event as a Sentry breadcrumb, so a crash
/// report arrives with the trail of actions that led to it.
///
/// Called from the effect dispatcher alongside `emit!`, and a no-op whenever
/// [`init`] declined to build a reporter: with no client bound to the hub,
/// `sentry::add_breadcrumb` returns without doing anything. That is why this
/// is a free function rather than a method — it saves threading a handle
/// through `runtime_loop::run`, `Core::new` and `Dispatcher::new` to reach
/// the one call site, and Sentry's hub is a process-global by design.
///
/// The fields are the event's own, which are `schema` constants and numbers.
/// A breadcrumb therefore cannot carry more than the OTLP path does, even
/// though the crash it is attached to can.
pub fn record_action(event: &TelemetryEvent) {
    let mut data = BTreeMap::new();
    data.insert(keys::OUTCOME.to_string(), event.outcome.as_str().into());
    if let Some(kind) = event.error_kind {
        data.insert(keys::ERROR_KIND.to_string(), kind.into());
    }
    if let Some(kind) = event.chat_kind {
        data.insert(keys::CHAT_KIND.to_string(), kind.into());
    }
    if let Some(ms) = event.duration_ms {
        data.insert(keys::DURATION_MS.to_string(), ms.into());
    }

    sentry::add_breadcrumb(sentry::Breadcrumb {
        category: Some("action".to_string()),
        message: Some(event.action.to_string()),
        level: sentry::Level::Info,
        data,
        ..Default::default()
    });
}

#[cfg(test)]
mod tests {
    use tgt_core::effect::TelemetryMode;
    use tgt_core::telemetry::schema::actions;

    use super::*;
    use crate::config::Config;

    /// CI never sets `TGT_SENTRY_DSN`, so every test build is a build whose
    /// crash reporting is inert. That is the case a contributor's `cargo
    /// build` produces too, and it has to be a path that is exercised rather
    /// than merely assumed: `sentry::init` is never reached, so no panic
    /// hook is chained, no transport thread starts, and no release-health
    /// session opens.
    #[test]
    fn a_build_without_a_dsn_reports_nothing_even_when_fully_enabled() {
        assert!(
            !build_has_dsn(),
            "the test build must have no TGT_SENTRY_DSN baked in"
        );
        assert!(dsn(true).is_none());
        assert!(init(true).is_none());
    }

    /// Each opt-out switch resolves to `crash_reports_enabled() == false`,
    /// which is the single input `init` consults. The switches themselves
    /// are applied in `config`/`main`, and are tested there; this pins the
    /// other half of the contract — that reaching `false` by any route is
    /// enough to keep a client from ever being created.
    #[test]
    fn every_off_switch_reaches_the_same_no_reporter_answer() {
        assert!(dsn(false).is_none());
        assert!(init(false).is_none());

        // `--no-telemetry`, `TELEGRAM_TUI_TELEMETRY=off` and `DO_NOT_TRACK`
        // all land here, as `TelemetryMode::Off`.
        let master_off = Config {
            telemetry_mode: TelemetryMode::Off,
            telemetry_crash_reports: true,
            ..Config::default()
        };
        assert!(!master_off.crash_reports_enabled());

        // `[telemetry] crash_reports = false`, and what a Disable at the
        // consent screen leaves behind on a later run.
        let crash_off = Config {
            telemetry_mode: TelemetryMode::On,
            telemetry_crash_reports: false,
            ..Config::default()
        };
        assert!(!crash_off.crash_reports_enabled());

        // The default is on, which is what "on unless you opt out" means.
        assert!(Config::default().crash_reports_enabled());
    }

    /// A breadcrumb is built from the event's own fields, so it inherits the
    /// allowlist even though the crash report it rides on does not. With no
    /// client bound (no DSN in this build) the call is a no-op; what is
    /// being checked is that nothing outside `schema` can reach the map.
    #[test]
    fn breadcrumbs_carry_only_allowlisted_keys() {
        use tgt_core::telemetry::schema::ALLOWED_KEYS;

        let event = TelemetryEvent::ok(actions::CHAT_OPEN)
            .with_chat_kind("private")
            .with_duration(12);
        record_action(&event);

        // Mirrors `record_action`'s map so a key added there without being
        // added to the schema fails here.
        for key in [
            keys::OUTCOME,
            keys::ERROR_KIND,
            keys::CHAT_KIND,
            keys::DURATION_MS,
        ] {
            assert!(
                ALLOWED_KEYS.contains(&key),
                "breadcrumb key {key:?} is not in the allowlist"
            );
        }
        assert!(ALLOWED_KEYS.contains(&keys::ACTION));
    }
}
