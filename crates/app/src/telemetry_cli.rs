//! `tgt telemetry show|reset-id` (spec §13.5). Both are plain stdout
//! commands — the TUI never starts on this path, so nothing here is subject
//! to spec §13.3's "nothing but the file logger writes while the TUI is
//! active" rule.
//!
//! `show` must print *exactly* what a session would send: only schema
//! constants and, where harmless, the live values a real session would
//! attach. `show_to`'s only job beyond formatting is staying honest about
//! that boundary, which is why every line it writes traces back to either
//! `tgt_core::telemetry::schema` or a value a real session actually
//! computes (`otel::load_or_create_identity`, `graphics::probe`, the
//! terminal size).

use std::io::Write;

use color_eyre::eyre;
use tgt_core::effect::TelemetryMode;
use tgt_core::telemetry::schema::{actions, buckets, error_kinds, keys};

use crate::config::Config;
use crate::{graphics, otel};

/// Vendor ingest proxy, baked in at build time (spec §13.6). Read the same
/// way `otel.rs` reads it — `option_env!` resolves per call site from the
/// same environment variable, so this needs no coupling to that module.
const VENDOR_ENDPOINT: Option<&str> = option_env!("TGT_INGEST_ENDPOINT");

/// Prints exactly what a session would send: the resource attributes every
/// session attaches once, the event attribute names with their allowed
/// value sets, and where this session's data would go.
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
    writeln!(out, "telemetry mode: {}", mode_str(config.telemetry_mode))?;
    writeln!(out, "destination: {}", destination_line(config))?;
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
        TelemetryMode::Vendor => "vendor",
        TelemetryMode::Custom => "custom",
        TelemetryMode::Off => "off",
    }
}

/// Where this session's data would go, mirroring `otel::destination`
/// exactly: custom fully replaces vendor, never both (spec §13.5).
fn destination_line(config: &Config) -> String {
    match config.telemetry_mode {
        TelemetryMode::Off => "nowhere — telemetry is disabled".to_string(),
        TelemetryMode::Vendor => match VENDOR_ENDPOINT {
            Some(endpoint) => format!("{endpoint} (vendor)"),
            None => "nowhere — this build has no vendor endpoint baked in; vendor mode is inert"
                .to_string(),
        },
        TelemetryMode::Custom => match config.custom_destination() {
            Some(dest) => format!(
                "{} (custom, {})",
                dest.endpoint,
                dest.protocol.as_deref().unwrap_or("http/protobuf")
            ),
            None => "nowhere — custom mode has no endpoint configured".to_string(),
        },
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

    /// Every `key = value` line `show_to` writes must name a key from the
    /// allowlist. This is spec §13.2's structural guarantee, checked from
    /// the CLI side: `show` cannot claim a session would send something
    /// that isn't in `ALLOWED_KEYS`.
    #[test]
    fn show_lists_only_allowlisted_keys() {
        let _lock = crate::logging::tests::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: serialized by the shared env lock other telemetry tests use.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        }

        let config = Config::default();
        let mut buf = Vec::new();
        show_to(&config, &mut buf).expect("show_to should succeed against a scratch config dir");

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
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

    #[test]
    fn show_reports_custom_mode_destination_and_never_vendor() {
        let _lock = crate::logging::tests::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        }

        let config = Config {
            telemetry_mode: TelemetryMode::Custom,
            telemetry_endpoint: Some("https://collector.example/".to_string()),
            ..Config::default()
        };

        let mut buf = Vec::new();
        show_to(&config, &mut buf).unwrap();

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }

        let output = String::from_utf8(buf).unwrap();
        let destination = output
            .lines()
            .find(|l| l.starts_with("destination:"))
            .expect("a destination line");
        assert!(destination.contains("https://collector.example/"));
        assert!(
            !destination.contains("vendor"),
            "custom mode must never mention the vendor destination: {destination}"
        );
    }

    #[test]
    fn reset_id_changes_install_id_and_salt() {
        let _lock = crate::logging::tests::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        }

        let before = otel::load_or_create_identity().expect("identity is created on first read");
        let (old_id, new_id) = otel::reset_identity().expect("reset_identity should succeed");
        let after = otel::load_or_create_identity().expect("identity is read back after reset");

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }

        assert_eq!(old_id, before.install_id);
        assert_eq!(new_id, after.install_id);
        assert_ne!(before.install_id, after.install_id);
        assert_ne!(before.salt, after.salt);
    }
}
