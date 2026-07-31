//! `tgt telemetry show|reset-id` (spec §13.5). Both are plain stdout
//! commands — the TUI never starts on this path, so nothing here is subject
//! to spec §13.3's "nothing but the file logger writes while the TUI is
//! active" rule.
//!
//! For the OTLP egress `show` prints *exactly* what a session would send:
//! only schema constants and, where harmless, the live values a real session
//! would attach. Every line of that section traces back to either
//! `tgt_core::telemetry::schema` or a value a real session actually computes
//! (`otel::load_or_create_identity`, `graphics::probe`, the terminal size).
//!
//! For the crash-reporting egress it cannot make that promise and does not
//! try. A crash report is assembled at the moment something fails, out of
//! the failure's own text and stack, so there is no fixed list to print. The
//! output says what a report is made of and says plainly that its contents
//! are not enumerable in advance — which is a less satisfying answer than
//! the one above, and the true one.

use std::io::Write;

use color_eyre::eyre;
use tgt_core::effect::TelemetryMode;
use tgt_core::telemetry::schema::{actions, buckets, error_kinds, keys};

use crate::config::Config;
use crate::{crash, graphics, otel};

/// Prints what a session would send: which egresses are live, the resource
/// attributes every export attaches once, and the event attribute names with
/// their allowed value sets.
pub fn show(config: &Config) -> eyre::Result<()> {
    match show_to(config, &mut std::io::stdout().lock()) {
        // `tgt telemetry show | head` closes our stdout early; that is the
        // reader's prerogative, not an error worth a nonzero exit.
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe) =>
        {
            Ok(())
        }
        other => other,
    }
}

/// [`show`], writing to an arbitrary sink instead of stdout — what the
/// `show_lists_only_allowlisted_keys` test captures and parses.
pub fn show_to(config: &Config, out: &mut dyn Write) -> eyre::Result<()> {
    writeln!(out, "telemetry: {}", mode_str(config.telemetry_mode))?;
    writeln!(out)?;

    writeln!(out, "crash reports: {}", crash_reports_line(config))?;
    writeln!(
        out,
        "  A report is built when the app crashes or exits with an error. It carries a"
    )?;
    writeln!(
        out,
        "  stack trace, the error or panic message, the app and OS version, and the recent"
    )?;
    writeln!(
        out,
        "  actions listed below as breadcrumbs. The message comes from whatever failed, so"
    )?;
    writeln!(
        out,
        "  unlike the OTLP attributes below it is not drawn from a fixed list and can carry"
    )?;
    writeln!(
        out,
        "  limited content such as a file path. Your IP address, username and hostname are"
    )?;
    writeln!(out, "  not sent.")?;
    writeln!(out)?;

    writeln!(out, "OTLP export: {}", destination_line(config))?;
    writeln!(
        out,
        "  Exactly the keys below and nothing else, enforced by an allowlist and proven by"
    )?;
    writeln!(out, "  a CI test that decodes the wire.")?;
    writeln!(out)?;

    writeln!(out, "resource attributes (sent once per session):")?;
    for (key, value) in resource_attributes()? {
        writeln!(out, "  {key} = {value}")?;
    }
    writeln!(out)?;

    writeln!(out, "event attributes (sent with every action, one of):")?;
    for (key, allowed) in event_attributes() {
        writeln!(out, "  {key} = {allowed}")?;
    }

    Ok(())
}

/// Regenerates the install id and HMAC salt, then prints old → new id (the
/// salt is never printed — spec §13.4 is what makes `chat.hash`
/// irreversible, and that only holds if it never leaves the machine).
pub fn reset_id() -> eyre::Result<()> {
    let (old_id, new_id) = otel::reset_identity()?;
    println!("install id: {old_id} -> {new_id}");
    Ok(())
}

fn mode_str(mode: TelemetryMode) -> &'static str {
    match mode {
        TelemetryMode::On => "on",
        TelemetryMode::Off => "off — nothing is sent by either path below",
    }
}

/// Whether crash reports would actually be sent, reading the same
/// `Config::crash_reports_enabled` and `crash::build_has_dsn` that
/// `crash::init` does, so the two cannot describe different sessions.
fn crash_reports_line(config: &Config) -> String {
    if !config.crash_reports_enabled() {
        return "off — nothing is sent".to_string();
    }
    if !crash::build_has_dsn() {
        return "on, but this build has no Sentry DSN baked in, so nothing is sent".to_string();
    }
    "on — sent to the telegram-tui project's Sentry".to_string()
}

/// Where this session's OTLP export would go, mirroring `otel::init`'s own
/// check: the endpoint the user configured, or nowhere.
fn destination_line(config: &Config) -> String {
    if config.telemetry_mode == TelemetryMode::Off {
        return "off — nothing is sent".to_string();
    }
    match config.custom_destination() {
        Some(dest) => format!(
            "{} ({})",
            dest.endpoint,
            dest.protocol.as_deref().unwrap_or("http/protobuf")
        ),
        None => "nowhere — no collector configured; this path is opt-in".to_string(),
    }
}

/// The session-constant keys every export attaches, with the value this
/// session would actually use. `term.program` and `term.width_bucket` are
/// omitted rather than shown as placeholders when there is nothing real to
/// report, matching what `install_exporter` in `main.rs` would attach.
fn resource_attributes() -> eyre::Result<Vec<(&'static str, String)>> {
    let identity = otel::load_or_create_identity()?;
    let mut attrs = vec![
        (keys::APP_VERSION, env!("CARGO_PKG_VERSION").to_string()),
        (keys::OS_VERSION, std::env::consts::OS.to_string()),
        (keys::INSTALL_ID, identity.install_id),
        (keys::SESSION_ID, otel::new_session_id()),
        (
            keys::TERM_GRAPHICS_PROTOCOL,
            graphics::telemetry_str(graphics::probe()).to_string(),
        ),
    ];

    if let Ok((cols, _)) = crossterm::terminal::size()
        && cols > 0
    {
        attrs.push((keys::TERM_WIDTH_BUCKET, buckets::width(cols).to_string()));
    }

    if let Ok(term) = std::env::var("TERM_PROGRAM")
        && !term.is_empty()
    {
        attrs.push((keys::TERM_PROGRAM, term));
    }

    Ok(attrs)
}

/// The per-event keys, listed by name with their allowed value set rather
/// than a live value — there is no session in progress to draw one from.
fn event_attributes() -> Vec<(&'static str, String)> {
    vec![
        (keys::ACTION, actions_list()),
        (keys::OUTCOME, "ok|error|cancelled".to_string()),
        (keys::ERROR_KIND, error_kinds_list()),
        (keys::DURATION_MS, "<integer milliseconds>".to_string()),
        (
            keys::CHAT_KIND,
            "private|group|supergroup|channel".to_string(),
        ),
        (
            keys::CHAT_HASH,
            "HMAC-SHA256(chat id, per-install salt), 8 bytes hex".to_string(),
        ),
        (keys::HISTORY_PAGE_DEPTH, "<integer>".to_string()),
        (
            keys::DOWNLOAD_SIZE_BUCKET,
            "<1MB|1-10MB|10-100MB|>100MB".to_string(),
        ),
    ]
}

fn actions_list() -> String {
    [
        actions::APP_START,
        actions::APP_QUIT,
        actions::QR_LOGIN,
        actions::PHONE_LOGIN,
        actions::CHAT_OPEN,
        actions::MESSAGE_SEND,
        actions::MESSAGE_REPLY,
        actions::MESSAGE_FORWARD,
        actions::MESSAGE_DELETE,
        actions::MESSAGE_EDIT,
        actions::MESSAGE_REACT,
        actions::HISTORY_PAGE,
        actions::PALETTE_OPEN,
        actions::SEARCH_RUN,
        actions::FILE_DOWNLOAD,
        actions::FILE_UPLOAD,
        actions::THEME_CHANGE,
    ]
    .join("|")
}

fn error_kinds_list() -> String {
    [
        error_kinds::TD_FLOOD_WAIT,
        error_kinds::TD_AUTH,
        error_kinds::TD_RATE_LIMIT,
        error_kinds::TD_OTHER,
        error_kinds::NET_TIMEOUT,
        error_kinds::NET_OFFLINE,
        error_kinds::LAYOUT_PANIC,
        error_kinds::IO_DENIED,
        error_kinds::IO_OTHER,
    ]
    .join("|")
}

#[cfg(test)]
mod tests {
    use tgt_core::telemetry::schema::ALLOWED_KEYS;

    use super::*;
    use crate::logging::tests::{set_config_dir, unset_config_dir};

    /// Every `key = value` line `show_to` writes must name a key from the
    /// allowlist. This is spec §13.2's structural guarantee, checked from
    /// the CLI side: `show` cannot claim a session would send something
    /// that isn't in `ALLOWED_KEYS`.
    ///
    /// Scoped to the OTLP half, which is the half that has an allowlist.
    /// The crash-reporting section writes prose, not `key = value` lines, so
    /// it contributes nothing here — deliberately, since there is no fixed
    /// list of what a crash report contains for this test to check against.
    #[test]
    fn show_lists_only_allowlisted_keys() {
        let _lock = crate::logging::tests::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: serialized by the shared env lock other telemetry tests use.
        unsafe {
            set_config_dir(tmp.path());
        }

        let config = Config::default();
        let mut buf = Vec::new();
        show_to(&config, &mut buf).expect("show_to should succeed against a scratch config dir");

        unsafe {
            unset_config_dir();
        }

        let output = String::from_utf8(buf).expect("show_to writes UTF-8");
        let mut found_any = false;
        for line in output.lines() {
            let Some((key, _value)) = line.split_once(" = ") else {
                continue;
            };
            let key = key.trim();
            found_any = true;
            assert!(
                ALLOWED_KEYS.contains(&key),
                "show printed key {key:?}, which is not in ALLOWED_KEYS: {output}"
            );
        }
        assert!(
            found_any,
            "expected at least one key = value line: {output}"
        );
    }

    /// `show` runs against a scratch config dir and returns the line
    /// starting with `prefix`.
    fn line_for(config: &Config, prefix: &str) -> String {
        let _lock = crate::logging::tests::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: serialized by the shared env lock other telemetry tests use.
        unsafe {
            set_config_dir(tmp.path());
        }

        let mut buf = Vec::new();
        show_to(config, &mut buf).expect("show_to should succeed against a scratch config dir");

        // SAFETY: serialized by the lock above.
        unsafe {
            unset_config_dir();
        }

        let output = String::from_utf8(buf).expect("show_to writes UTF-8");
        output
            .lines()
            .find(|l| l.starts_with(prefix))
            .unwrap_or_else(|| panic!("no {prefix:?} line in:\n{output}"))
            .to_string()
    }

    #[test]
    fn show_reports_the_configured_collector_as_the_otlp_destination() {
        let config = Config {
            telemetry_endpoint: Some("https://collector.example/".to_string()),
            ..Config::default()
        };
        assert!(line_for(&config, "OTLP export:").contains("https://collector.example/"));
    }

    /// OTLP is opt-in now, so the default config — telemetry fully on — must
    /// still report that it exports nowhere. A user reading this output is
    /// entitled to see that the two egresses have different defaults.
    #[test]
    fn show_reports_no_otlp_destination_by_default() {
        let line = line_for(&Config::default(), "OTLP export:");
        assert!(
            line.contains("nowhere"),
            "the default config exports to no collector: {line}"
        );
    }

    /// Every route to "off" has to reach the output, or `show` would be
    /// telling a user who opted out that they are still sending.
    #[test]
    fn show_reports_both_egresses_off_when_the_master_switch_is_off() {
        // What --no-telemetry, TELEGRAM_TUI_TELEMETRY=off and DO_NOT_TRACK
        // all produce.
        let config = Config {
            telemetry_mode: TelemetryMode::Off,
            telemetry_crash_reports: true,
            telemetry_endpoint: Some("https://collector.example/".to_string()),
            ..Config::default()
        };

        assert!(line_for(&config, "telemetry:").contains("off"));
        assert!(line_for(&config, "crash reports:").contains("off"));
        let otlp = line_for(&config, "OTLP export:");
        assert!(otlp.contains("off"), "{otlp}");
        assert!(
            !otlp.contains("collector.example"),
            "a configured endpoint must not be reported as live while telemetry is off: {otlp}"
        );
    }

    #[test]
    fn show_reports_crash_reports_off_when_only_that_switch_is_off() {
        let config = Config {
            telemetry_crash_reports: false,
            ..Config::default()
        };
        assert!(line_for(&config, "crash reports:").contains("off"));
        // The master switch is still on, so the line above must not claim
        // telemetry as a whole is off.
        assert!(line_for(&config, "telemetry:").contains("on"));
    }

    /// A from-source build has no DSN, and saying "on" without saying that
    /// would overstate what it does. CI is such a build, so this is the
    /// branch the test suite actually takes.
    #[test]
    fn show_admits_when_the_build_has_no_dsn() {
        assert!(!crash::build_has_dsn(), "the test build must have no DSN");
        let line = line_for(&Config::default(), "crash reports:");
        assert!(
            line.contains("no Sentry DSN") && line.contains("nothing is sent"),
            "a DSN-less build must say so: {line}"
        );
    }

    #[test]
    fn reset_id_changes_install_id_and_salt() {
        let _lock = crate::logging::tests::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            set_config_dir(tmp.path());
        }

        let before = otel::load_or_create_identity().expect("identity is created on first read");
        let (old_id, new_id) = otel::reset_identity().expect("reset_identity should succeed");
        let after = otel::load_or_create_identity().expect("identity is read back after reset");

        unsafe {
            unset_config_dir();
        }

        assert_eq!(old_id, before.install_id);
        assert_eq!(new_id, after.install_id);
        assert_ne!(before.install_id, after.install_id);
        assert_ne!(before.salt, after.salt);
    }
}
