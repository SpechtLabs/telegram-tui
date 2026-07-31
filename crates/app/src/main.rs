//! Entry point: CLI parsing, logging/panic-hook setup, terminal
//! setup/teardown, and handing off to the run loop.
//! See docs/architecture.md §2.3, §3, §9.3; spec §14.

mod cli;
mod config;
mod dispatch;
mod graphics;
mod keychain;
mod logging;
mod media_kind;
mod notify;
mod otel;
mod panic;
mod runtime_loop;
mod td_runtime;
mod telemetry_cli;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use clap::Parser;
use color_eyre::eyre;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tgt_core::app::{App, Boot};
use tgt_core::effect::TelemetryMode;
use tgt_core::td::runtime::TdRuntime;
use tgt_ui::theme::Theme;
use tgt_ui::theme::loader;

use cli::{Cli, Command};
use config::Config;
use dispatch::TdBootParams;
use td_runtime::TdlibRuntime;

fn main() -> eyre::Result<()> {
    let cli = Cli::parse();

    if let Some(Command::Telemetry { action }) = &cli.command {
        // Neither subcommand starts the TUI, so stdout is fair game (spec
        // §13.3's "nothing but the file logger while the TUI is active"
        // does not apply here).
        let mut config = config::load()?;
        config.telemetry_mode = effective_telemetry_mode(&cli, config.telemetry_mode);
        return match action {
            cli::TelemetryAction::Show => telemetry_cli::show(&config),
            cli::TelemetryAction::ResetId => telemetry_cli::reset_id(),
        };
    }

    run_tui(cli)
}

/// The effective telemetry mode for this run, applying the one precedence
/// step left after `config::load()` (spec §13.5): `DO_NOT_TRACK` and
/// `TELEGRAM_TUI_TELEMETRY` are already folded into `config_mode` by that
/// call's env overrides, so the only thing left to apply here is
/// `--no-telemetry`, which forces the session to `Off` without rewriting
/// the config file.
fn effective_telemetry_mode(cli: &Cli, config_mode: TelemetryMode) -> TelemetryMode {
    if cli.no_telemetry {
        TelemetryMode::Off
    } else {
        config_mode
    }
}

fn run_tui(cli: Cli) -> eyre::Result<()> {
    color_eyre::install()?;

    // Must happen before raw mode: the file logger is the only writer
    // allowed once the TUI takes over the terminal (spec §13.3). The
    // exporter goes into the slot this reserves, once the config below says
    // whether there is anything to export.
    let (_log_guard, export_handle) = logging::init()?;
    tracing::debug!(no_telemetry = cli.no_telemetry, "cli parsed");

    // Everything that can fail loudly — and the Keychain prompt, which may
    // put a dialog on screen — happens before raw mode, while stderr is
    // still a usable place for an error report.
    let config = config::load()?;

    // Once per session, before raw mode: the probe only reads environment
    // variables the terminal set when it started this process, so nothing
    // later can change its answer. The draw path still renders every photo
    // as a placeholder card (see `graphics`'s module docs); the probe's
    // other consumer is the telemetry session below.
    let graphics_protocol = graphics::probe();
    tracing::info!(
        protocol = graphics::telemetry_str(graphics_protocol),
        "terminal graphics protocol probed"
    );

    // The install id is exported; the salt never is — it is what makes
    // `chat.hash` irreversible (spec §13.4). Both are read (or generated)
    // whether or not telemetry is on, so that turning it on later does not
    // change the hashes of chats already seen.
    let identity = otel::load_or_create_identity()?;
    let telemetry_mode = effective_telemetry_mode(&cli, config.telemetry_mode);
    // Read from disk before the consent screen (below, via `App::new`) can
    // possibly run this session — an unacknowledged first run therefore
    // never constructs an exporter, whatever the user is about to choose.
    // The screen's own acknowledgement only takes effect on the *next* run,
    // once its `ConfigPatch::ConsentAcknowledged` has round-tripped to disk.
    let otel_guard = if config.consent_acknowledged && telemetry_mode != TelemetryMode::Off {
        install_exporter(&export_handle, telemetry_mode, &config, &identity)
    } else {
        None
    };

    let td_boot = TdBootParams {
        database_directory: keychain::td_database_dir()?,
        database_encryption_key: keychain::db_key()?.to_vec(),
    };

    // Wraps whatever hook color-eyre just installed: on panic, the terminal
    // is restored first, then color-eyre's report prints to a usable shell.
    panic::install(restore_terminal);

    enable_raw_mode()?;
    // From here on, every exit path — normal return, an early `?`, or an
    // unwinding panic — restores the terminal via this guard's `Drop`.
    let terminal_guard = TerminalGuard;
    execute!(io::stdout(), EnterAlternateScreen)?;
    if config.mouse {
        // Flagged before the escape sequence goes out, not after: if this
        // `execute!` fails halfway or something panics inside it, the
        // restore path still knows to send the disable sequence. Sending it
        // when capture was never on is harmless; leaving it unsent when it
        // was is not.
        MOUSE_CAPTURE.store(true, Ordering::SeqCst);
        execute!(io::stdout(), EnableMouseCapture)?;
    }
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let app = App::new(boot_from(&config, &cli, terminal.size()?, identity.salt));
    let theme = resolve_theme(&config.theme);
    // Shared with the dispatcher, which applies `Effect::SaveConfig` patches
    // and reads the credentials back when TDLib asks for its parameters.
    let config = Arc::new(Mutex::new(config));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let outcome = rt.block_on(async move {
        // Construction is async (it silences TDLib's own logger before
        // anything can write to stderr) and takes no parameters: TDLib is
        // configured in-band, by the `SetTdlibParameters` the dispatcher
        // sends when TDLib reports `WaitTdlibParameters`.
        let runtime: Arc<dyn TdRuntime> = Arc::new(TdlibRuntime::new().await);
        runtime_loop::run(app, &theme, &mut terminal, runtime, config, td_boot).await
    });

    // Restore the terminal before flushing telemetry: the flush is allowed
    // up to two seconds (spec §13.7), and those seconds should be spent in
    // front of a usable shell rather than a frozen alternate screen. The
    // guard's `Drop` still covers every path that leaves before this point.
    drop(terminal_guard);
    if let Some(guard) = otel_guard {
        guard.shutdown();
    }

    outcome?;
    Ok(())
}

/// Builds the exporter and swaps it into the live subscriber. Telemetry is
/// never a reason to fail startup (spec §13.7): a build with no vendor
/// endpoint, an unreachable collector, or a malformed custom protocol all
/// resolve to "no exporter" plus a local debug line.
fn install_exporter(
    export_handle: &logging::ExportHandle,
    mode: TelemetryMode,
    config: &Config,
    identity: &otel::Identity,
) -> Option<otel::OtelGuard> {
    let (cols, _) = crossterm::terminal::size().unwrap_or((0, 0));
    let session = otel::SessionContext {
        install_id: identity.install_id.clone(),
        session_id: otel::new_session_id(),
        term_program: std::env::var("TERM_PROGRAM").ok().filter(|s| !s.is_empty()),
        graphics_protocol: graphics::telemetry_str(graphics::probe()),
        width_bucket: tgt_core::telemetry::schema::buckets::width(cols),
    };

    // Under `mode = "custom"` this replaces the vendor destination outright;
    // `otel::init` never combines the two (spec §13.5). `custom_destination`
    // already returns `None` unless `config.telemetry_mode` is `Custom`, so
    // passing it here regardless of `mode` (which may differ from
    // `config.telemetry_mode` under `--no-telemetry`) is safe either way.
    let custom = config
        .custom_destination()
        .map(|dest| otel::CustomEndpoint {
            endpoint: dest.endpoint,
            protocol: dest.protocol,
            headers: dest.headers,
        });

    match otel::init(mode, &session, custom) {
        Ok(Some(exporter)) => match logging::install_export_layer(export_handle, exporter.layer) {
            Ok(()) => Some(exporter.guard),
            Err(err) => {
                tracing::debug!(%err, "telemetry export layer could not be installed");
                None
            }
        },
        Ok(None) => {
            tracing::debug!(
                "telemetry is configured but this build has no destination for it; not exporting"
            );
            None
        }
        Err(err) => {
            tracing::debug!(%err, "telemetry exporter unavailable; continuing without it");
            None
        }
    }
}

/// Whether mouse capture was turned on this session, so [`restore_terminal`]
/// knows whether it has to be turned back off.
///
/// A process-global flag rather than a field on `TerminalGuard` or a value
/// captured by the closure: the panic hook's restore has to run from
/// whichever thread panicked and therefore must be `Fn() + Send + Sync +
/// 'static`, and it is installed *before* the config-driven decision is even
/// made. An `AtomicBool` both threads can read is the smallest thing that
/// lets one function serve the normal teardown and the panic path
/// identically — which is the point, since those are exactly the two ways a
/// terminal gets left in reporting mode, spraying escape codes at the shell.
static MOUSE_CAPTURE: AtomicBool = AtomicBool::new(false);

/// Restores the terminal: releases mouse capture (if it was taken), leaves
/// the alternate screen, disables raw mode. Errors are swallowed — there is
/// nothing more to do if this itself fails, and it may run from inside the
/// panic hook where propagating isn't an option.
///
/// `swap` rather than `load`: a panic runs this from the hook and then again
/// from `TerminalGuard::drop` as the stack unwinds, and the second pass has
/// no reason to re-send a sequence the first one already sent.
fn restore_terminal() {
    if MOUSE_CAPTURE.swap(false, Ordering::SeqCst) {
        let _ = execute!(io::stdout(), DisableMouseCapture);
    }
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
}

/// Restores the terminal when dropped, so every way `run_tui` can exit after
/// raw mode is entered leaves a usable shell, not just the success path.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Resolves `[app].theme` (spec §7.2 / architecture §4.9) to a `Theme`:
/// `tgt_ui::theme::loader::builtin` first, then a user file at
/// `<config_dir>/themes/<name>.toml`, then `Theme::default_dark` — every
/// failure in the file path (missing file, malformed TOML, a bad color)
/// falls back to `default_dark` with a local warning rather than failing
/// startup, matching `config.rs`'s "never brick the app over a config
/// mistake" stance. Truecolor vs. 256-color degradation is decided here too
/// (`COLORTERM` containing `"truecolor"` or `"24bit"`), since detecting the
/// terminal's capability is this call site's job, not the loader's (module
/// docs on `tgt_ui::theme::loader`).
///
/// Live theme *toggling* (re-resolving mid-session, e.g. from the command
/// palette) is deferred: the palette's `ToggleTheme` stays a no-op for now.
fn resolve_theme(theme_name: &str) -> Theme {
    let theme = loader::builtin(theme_name).unwrap_or_else(|| match theme_file_path(theme_name) {
        Ok(path) => match loader::load_theme(&path) {
            Ok(theme) => theme,
            Err(err) => {
                tracing::warn!(
                    theme = %theme_name,
                    path = %path.display(),
                    error = %err,
                    "could not load theme file; using default_dark"
                );
                Theme::default_dark()
            }
        },
        Err(err) => {
            tracing::warn!(
                theme = %theme_name,
                error = %err,
                "could not determine the theme file path; using default_dark"
            );
            Theme::default_dark()
        }
    });

    let truecolor = std::env::var("COLORTERM")
        .map(|value| value.contains("truecolor") || value.contains("24bit"))
        .unwrap_or(false);
    loader::for_terminal(theme, truecolor)
}

/// `<config_dir>/telegram-tui/themes/<name>.toml` — same base directory as
/// `config.rs`'s `config_path` (kept separate rather than exposed from
/// `config.rs`, since only this call site needs it).
fn theme_file_path(name: &str) -> eyre::Result<std::path::PathBuf> {
    use etcetera::BaseStrategy;

    let strategy = etcetera::choose_base_strategy()
        .map_err(|err| eyre::eyre!("could not determine the config directory: {err}"))?;
    Ok(strategy
        .config_dir()
        .join("telegram-tui")
        .join("themes")
        .join(format!("{name}.toml")))
}

/// Projects the loaded config (plus the CLI flag and the terminal size) into
/// the plain boot values `tgt-core` is handed. Everything impure about
/// startup is resolved here and nowhere else.
fn boot_from(
    config: &Config,
    cli: &Cli,
    size: ratatui::layout::Size,
    telemetry_salt: [u8; 32],
) -> Boot {
    let fields = config.boot_fields();
    Boot {
        theme_name: fields.theme_name,
        bindings: fields.bindings,
        layout_breakpoint_cols: fields.layout_breakpoint_cols,
        telemetry_mode: effective_telemetry_mode(cli, fields.telemetry_mode),
        // Generated once per install and persisted `0600` next to the
        // install id; never transmitted (spec §13.4).
        telemetry_salt,
        // The first-run screen (spec §13.5): shown whenever the config does
        // not yet record an acknowledged choice. It is the only writer of
        // `consent_acknowledged` (via `ConfigPatch::ConsentAcknowledged`),
        // so an unacknowledged install always boots here before auth and
        // before `install_exporter` above ever runs.
        consent_needed: !config.consent_acknowledged,
        has_credentials: fields.has_credentials,
        width: size.width,
        height: size.height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_telemetry_flag_beats_config() {
        let cli = Cli::try_parse_from(["tgt", "--no-telemetry"]).unwrap();
        assert_eq!(
            effective_telemetry_mode(&cli, TelemetryMode::Vendor),
            TelemetryMode::Off
        );
        assert_eq!(
            effective_telemetry_mode(&cli, TelemetryMode::Custom),
            TelemetryMode::Off
        );
        // Off stays Off either way, but this confirms the flag doesn't need
        // config to already say Off to take effect.
        assert_eq!(
            effective_telemetry_mode(&cli, TelemetryMode::Off),
            TelemetryMode::Off
        );

        let cli = Cli::try_parse_from(["tgt"]).unwrap();
        assert_eq!(
            effective_telemetry_mode(&cli, TelemetryMode::Vendor),
            TelemetryMode::Vendor
        );
        assert_eq!(
            effective_telemetry_mode(&cli, TelemetryMode::Custom),
            TelemetryMode::Custom
        );
    }
}
