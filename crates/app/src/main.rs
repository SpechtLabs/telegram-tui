//! Entry point: CLI parsing, logging/panic-hook setup, terminal
//! setup/teardown, and handing off to the run loop.
//! See docs/architecture.md §2.3, §3, §9.3; spec §14.

mod cli;
mod config;
mod crash;
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
mod update;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use clap::Parser;
use color_eyre::eyre;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
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
    match dispatch_cli(Cli::parse()) {
        Ok(()) => Ok(()),
        Err(report) => Err(report_to_user(report)),
    }
}

fn dispatch_cli(cli: Cli) -> eyre::Result<()> {
    // Like `telemetry show`, this never starts the TUI, so stdout is free.
    if let Some(Command::Update {
        require_signature,
        force,
    }) = &cli.command
    {
        return update::run(*require_signature, *force);
    }

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

/// Prints a `human_errors::Error` as itself and exits, rather than letting
/// `color_eyre` render it.
///
/// Everything that reaches here has already restored the terminal and
/// flushed the log — `run_tui`'s guards drop as it returns — so stderr is
/// usable, which is the whole reason the fatal config write travels back up
/// the loop instead of printing where it happens.
///
/// The bypass exists because `color_eyre` would append its source chain and
/// a `Location: crates/app/src/config.rs:479` to a message that already
/// carries its cause and its remedy. That location is useful for a panic and
/// noise for a user who needs to know their config directory isn't writable.
/// Every other error keeps the full `color_eyre` report, backtrace and all.
fn report_to_user(report: eyre::Report) -> eyre::Report {
    let Some(err) = report.downcast_ref::<human_errors::Error>() else {
        return report;
    };
    eprintln!("{}", err.message());
    // 1 rather than a `?` return: returning would print the report a second
    // time, under `Error:`, which is exactly what this function exists to
    // avoid.
    std::process::exit(1);
}

/// The effective telemetry master switch for this run, applying the one
/// precedence step left after `config::load()` (spec §13.5): `DO_NOT_TRACK`
/// and `TELEGRAM_TUI_TELEMETRY` are already folded into `config_mode` by that
/// call's env overrides, so the only thing left to apply here is
/// `--no-telemetry`, which forces the session to `Off` without rewriting
/// the config file.
///
/// `Off` here disables **both** egresses: it gates `crash::init` through
/// `Config::crash_reports_enabled` and `otel::init` through
/// `Config::custom_destination`, and neither has any other way in.
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
    // later can change its answer. Its two consumers are the draw path (via
    // `graphics_capability` below) and the telemetry session further down.
    let graphics_protocol = graphics::probe();
    tracing::info!(
        protocol = graphics::telemetry_str(graphics_protocol),
        inline_images = config.inline_images,
        "terminal graphics protocol probed"
    );
    let graphics = graphics_capability(graphics_protocol, config.inline_images);

    // The install id is exported; the salt never is — it is what makes
    // `chat.hash` irreversible (spec §13.4). Both are read (or generated)
    // whether or not telemetry is on, so that turning it on later does not
    // change the hashes of chats already seen.
    let identity = otel::load_or_create_identity()?;
    let telemetry_mode = effective_telemetry_mode(&cli, config.telemetry_mode);
    // `config` still holds the file's own master switch; this run's may be
    // stricter because of `--no-telemetry`. Both egresses read their gate
    // off this copy so the flag reaches them without a second code path.
    let session_config = Config {
        telemetry_mode,
        ..config.clone()
    };

    // Read from disk before the consent screen (below, via `App::new`) can
    // possibly run this session — an unacknowledged first run therefore
    // never constructs an exporter or a crash reporter, whatever the user is
    // about to choose. The screen's own acknowledgement only takes effect on
    // the *next* run, once its `ConfigPatch::ConsentAcknowledged` has
    // round-tripped to disk.
    let acknowledged = config.consent_acknowledged;

    // Between `color_eyre::install` above and `panic::install` below, so the
    // panic hook chain ends up restore-terminal → Sentry capture → color-eyre
    // print. See `crash::init`.
    let crash_reporter = acknowledged
        .then(|| crash::init(session_config.crash_reports_enabled()))
        .flatten();

    let otel_guard = if acknowledged {
        install_exporter(&export_handle, &session_config, &identity)
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
    // Flagged before the escape sequence goes out, not after: if the
    // `execute!` below fails halfway or something panics inside it, the
    // restore path still knows to send the disable sequence. Sending it when
    // capture was never on is harmless; leaving it unsent when it was is not.
    if config.mouse {
        MOUSE_CAPTURE.store(true, Ordering::SeqCst);
    }
    enable_modes_into(&mut io::stdout(), config.mouse)?;
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
        // Abandoning a QR login is only possible via `logOut`, which ends in
        // `authorizationStateClosed` — terminal for that client instance, so
        // getting back to a usable login needs a fresh one. See
        // `runtime_loop::Core::restart_client` for why it only fires
        // pre-authorization.
        let restart: runtime_loop::RuntimeFactory = Arc::new(|| {
            Box::pin(async {
                let runtime: Arc<dyn TdRuntime> = Arc::new(TdlibRuntime::new().await);
                runtime
            })
        });
        runtime_loop::run(
            app,
            &mut terminal,
            runtime,
            config,
            td_boot,
            runtime_loop::Presentation {
                theme,
                resolve_theme,
                graphics,
                measure_cell: graphics::cell_size,
            },
            Some(restart),
        )
        .await
    });

    // Restore the terminal before flushing telemetry: the flush is allowed
    // up to two seconds (spec §13.7), and those seconds should be spent in
    // front of a usable shell rather than a frozen alternate screen. The
    // guard's `Drop` still covers every path that leaves before this point.
    drop(terminal_guard);

    // The run's own failure is the most useful thing a crash reporter can
    // carry, and it has to be captured before the client is closed — hence
    // reporting here rather than letting the `?` below carry it out of
    // `main` unseen. A panic needs none of this: the panic integration's
    // hook has already captured it by the time control reaches here, if it
    // reaches here at all.
    // `runtime_loop::run` already returns an `eyre::Result`, so the run's
    // failure — including a fatal config write — arrives as a `Report`.
    if let (Err(report), Some(reporter)) = (&outcome, &crash_reporter) {
        reporter.record_fatal_error(report);
    }
    if let Some(reporter) = crash_reporter {
        reporter.shutdown();
    }
    if let Some(guard) = otel_guard {
        guard.shutdown();
    }

    outcome?;
    Ok(())
}

/// Builds the OTLP exporter and swaps it into the live subscriber.
/// Telemetry is never a reason to fail startup (spec §13.7): no configured
/// endpoint, an unreachable collector, or a malformed protocol all resolve
/// to "no exporter" plus a local debug line.
fn install_exporter(
    export_handle: &logging::ExportHandle,
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

    // `None` unless the user configured a collector *and* the master switch
    // is on — `custom_destination` is the one place that decides both, so
    // there is nothing left for this call site to check.
    let destination = config
        .custom_destination()
        .map(|dest| otel::CustomEndpoint {
            endpoint: dest.endpoint,
            protocol: dest.protocol,
            headers: dest.headers,
        });

    match otel::init(&session, destination) {
        Ok(Some(exporter)) => match logging::install_export_layer(export_handle, exporter.layer) {
            Ok(()) => Some(exporter.guard),
            Err(err) => {
                tracing::debug!(%err, "telemetry export layer could not be installed");
                None
            }
        },
        Ok(None) => {
            tracing::debug!("no OTLP collector configured; not exporting");
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

/// Restores the terminal: releases mouse capture (if it was taken), turns
/// bracketed paste back off, leaves the alternate screen, disables raw mode.
/// Errors are swallowed — there is nothing more to do if this itself fails,
/// and it may run from inside the panic hook where propagating isn't an
/// option.
///
/// `swap` rather than `load`: a panic runs this from the hook and then again
/// from `TerminalGuard::drop` as the stack unwinds, and the second pass has
/// no reason to re-send a sequence the first one already sent.
fn restore_terminal() {
    let _ = restore_modes_into(
        &mut io::stdout(),
        MOUSE_CAPTURE.swap(false, Ordering::SeqCst),
    );
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
}

/// The escape sequences that put the terminal into the modes the TUI needs,
/// written to an arbitrary sink so a test can read them back.
///
/// Bracketed paste is unconditional, unlike mouse capture. Without it
/// crossterm never emits `Event::Paste` at all and a paste arrives as
/// individual keystrokes — so an embedded newline acts as Enter and the
/// composer sends half of what was pasted, which is what anyone pasting a
/// link or a multi-line message hits. Everything downstream of the event
/// already exists (`tgt_ui::input::map_event`, `composer::handle_paste`, the
/// `~/` expansion in `runtime_loop`); this call is the only thing that
/// produces the event in the first place. There is no configuration that
/// turns it off, so nothing flags it: the teardown always sends the disable,
/// and a terminal that never had it on ignores that.
///
/// # A terminal that cannot bracket pastes is not a startup failure
///
/// `Unsupported` is swallowed, and that is load-bearing rather than
/// defensive. On Windows without VT support, crossterm routes commands to
/// the console API instead of writing sequences (`command.rs:123-130`
/// checks `is_ansi_code_supported`, which asks the *process's* console, not
/// this writer), and `EnableBracketedPaste::execute_winapi` returns
/// `ErrorKind::Unsupported` with "Bracketed paste not implemented in the
/// legacy Windows API". Propagating that would mean `tgt` refusing to start
/// on a legacy Windows console — trading a working client for a paste
/// nicety. The degraded behaviour is the pre-existing one: a multi-line
/// paste arrives as keystrokes.
///
/// Only `Unsupported` is swallowed. A real write failure still propagates,
/// because that means the terminal handle itself is broken.
fn enable_modes_into(out: &mut impl io::Write, mouse: bool) -> io::Result<()> {
    match execute!(out, EnableBracketedPaste) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::Unsupported => {
            tracing::warn!(
                %err,
                "this terminal cannot bracket pastes; a pasted newline will \
                 be delivered as Enter"
            );
        }
        Err(err) => return Err(err),
    }
    if mouse {
        execute!(out, EnableMouseCapture)?;
    }
    Ok(())
}

/// The escape sequences [`restore_terminal`] sends, written to an arbitrary
/// sink so a test can read them back.
///
/// Split out for exactly that reason: leaving a terminal in bracketed-paste
/// mode after the process is gone is invisible from inside the process, and
/// the panic path — where it matters most — is the one nobody exercises by
/// hand. `disable_raw_mode` and `LeaveAlternateScreen` stay in the caller:
/// they are terminal syscalls against the real handle with nothing to
/// capture.
fn restore_modes_into(out: &mut impl io::Write, mouse_captured: bool) -> io::Result<()> {
    if mouse_captured {
        execute!(out, DisableMouseCapture)?;
    }
    // Unconditional, matching the enable. A terminal that never had it on
    // ignores the sequence; one that did would otherwise keep wrapping every
    // paste in `\e[200~`/`\e[201~` in the user's shell, long after `tgt`
    // has exited.
    execute!(out, DisableBracketedPaste)
}

/// Restores the terminal when dropped, so every way `run_tui` can exit after
/// raw mode is entered leaves a usable shell, not just the success path.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Maps the startup probe into the value `tgt-ui` draws from
/// (architecture §4.9.1). Two distinct "no" answers collapse into the same
/// `None`, which is the whole point of the boundary: the ui crate never
/// learns *why* it has no protocol, only that it has none and must draw the
/// design-language §4 line instead.
///
/// - `enabled` is `[app].inline_images`. Turning it off is how a user
///   overrules a probe that guessed right about the protocol and wrong about
///   the result (`config.rs` has the cases).
/// - `GraphicsProtocol::None` is the probe finding nothing to speak — which
///   includes running under tmux without `TGT_FORCE_GRAPHICS=1` (see
///   `graphics::probe_from`).
fn graphics_capability(
    protocol: graphics::GraphicsProtocol,
    enabled: bool,
) -> Option<tgt_ui::render::image::Capability> {
    use graphics::GraphicsProtocol;
    use tgt_ui::render::image::Capability;

    if !enabled {
        return None;
    }
    match protocol {
        GraphicsProtocol::Kitty => Some(Capability::Kitty),
        GraphicsProtocol::Iterm2 => Some(Capability::Iterm2),
        GraphicsProtocol::Sixel => Some(Capability::Sixel),
        GraphicsProtocol::None => None,
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
/// The one resolution path, used twice: once here at startup (`[app].theme`
/// from the config file), and again mid-session whenever
/// `AppState::theme_generation` changes (T60's live theme switching —
/// `state::palette::CommandId::ToggleTheme`). The second use isn't a direct
/// call: this function is passed *by value* to `runtime_loop::run` (as its
/// `ThemeResolver` parameter, a plain `fn(&str) -> Theme`), which stores it
/// and calls it from `draw_if_due` on a generation change. Threading it
/// through as a value rather than having `runtime_loop.rs` reach for
/// `crate::resolve_theme` keeps that module compiling standalone under the
/// `crates/app/tests/*.rs` integration binaries that `#[path]`-include it
/// without `main.rs` (see `runtime_loop::ThemeResolver`'s doc comment).
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
        // False in every build made from source, which is most of them, and
        // the consent screen says so rather than offering to enable an
        // endpoint that is not there (spec §13.5, §13.6).
        crash_reports_available: crash::build_has_dsn(),
        // The first-run screen (spec §13.5): shown whenever the config does
        // not yet record an acknowledged choice. It is the only writer of
        // `consent_acknowledged` (via `ConfigPatch::ConsentAcknowledged`),
        // so an unacknowledged install always boots here before auth and
        // before `install_exporter` above ever runs.
        consent_needed: !config.consent_acknowledged,
        has_credentials: fields.has_credentials,
        width: size.width,
        height: size.height,
        auto_download_photos: fields.auto_download_photos,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_telemetry_flag_beats_config() {
        let cli = Cli::try_parse_from(["tgt", "--no-telemetry"]).unwrap();
        assert_eq!(
            effective_telemetry_mode(&cli, TelemetryMode::On),
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
            effective_telemetry_mode(&cli, TelemetryMode::On),
            TelemetryMode::On
        );
    }

    /// A terminal left in bracketed-paste mode outlives the process: the
    /// user's shell keeps wrapping every paste in `\e[200~`/`\e[201~` until
    /// they reset it by hand. It is invisible from inside the process, and
    /// the path where it matters most — the panic hook — is the one nobody
    /// exercises by hand, so it is asserted on the bytes.
    ///
    /// Both callers of the teardown go through `restore_modes_into`:
    /// `TerminalGuard::drop` on every ordinary exit, and `panic::install`'s
    /// hook on an unwinding one.
    /// A sink that fails every write with a chosen kind, so the
    /// `Unsupported` carve-out can be exercised on any platform rather than
    /// only on the one that produces it naturally.
    struct FailingSink(io::ErrorKind);

    impl io::Write for FailingSink {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(self.0))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The carve-out that keeps `tgt` starting on a terminal that cannot
    /// bracket pastes, and the limit on it.
    ///
    /// This is the regression the Windows CI job caught: `run_tui` calls
    /// `enable_modes_into` with `?`, and on a legacy Windows console
    /// `EnableBracketedPaste` answers `Unsupported`, so before this the
    /// client refused to start there. Losing paste bracketing is a degraded
    /// terminal; refusing to run is a broken client.
    ///
    /// The second half matters as much: swallowing *every* error would hide
    /// a genuinely broken terminal handle behind a silent success.
    #[test]
    fn setup_tolerates_an_unsupported_mode_but_not_a_broken_terminal() {
        enable_modes_into(&mut FailingSink(io::ErrorKind::Unsupported), false)
            .expect("a terminal without bracketed paste must still start");

        let broken = enable_modes_into(&mut FailingSink(io::ErrorKind::BrokenPipe), false);
        assert_eq!(
            broken
                .expect_err("a broken handle must not be swallowed")
                .kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    /// Byte-level assertions only hold where crossterm writes sequences.
    ///
    /// On Windows it may not: `queue` consults `is_ansi_code_supported`,
    /// which asks the *process's* console rather than the writer it was
    /// handed, so a test writing into a `Vec` still takes the console-API
    /// branch when that console has no VT support (`command.rs:123-130`).
    /// Nothing then reaches the buffer, and for `EnableBracketedPaste` the
    /// call fails outright. The Windows half of this pair asserts the
    /// property that actually matters there instead.
    #[cfg(not(windows))]
    #[test]
    fn setup_always_turns_bracketed_paste_on() {
        // 2004 is the DEC private mode for bracketed paste; `h` enables it.
        const ENABLE_PASTE: &[u8] = b"\x1b[?2004h";

        let mut without_mouse = Vec::new();
        enable_modes_into(&mut without_mouse, false).expect("the sequences are supported here");
        assert!(
            contains(&without_mouse, ENABLE_PASTE),
            "bracketed paste must be enabled even with mouse reporting off, or a \
             pasted newline is delivered as Enter and sends half the text: {:?}",
            String::from_utf8_lossy(&without_mouse)
        );

        let mut with_mouse = Vec::new();
        enable_modes_into(&mut with_mouse, true).expect("the sequences are supported here");
        assert!(
            contains(&with_mouse, ENABLE_PASTE),
            "and with it on: {:?}",
            String::from_utf8_lossy(&with_mouse)
        );
        assert!(
            contains(&with_mouse, b"\x1b[?1006h"),
            "mouse reporting must still be enabled when configured: {:?}",
            String::from_utf8_lossy(&with_mouse)
        );
    }

    /// The Windows half, and it is not a skip — it pins the thing that
    /// broke.
    ///
    /// `EnableBracketedPaste::execute_winapi` returns
    /// `ErrorKind::Unsupported` ("Bracketed paste not implemented in the
    /// legacy Windows API"), and `run_tui` calls this with `?`. Propagating
    /// it would make `tgt` refuse to start on a legacy Windows console for
    /// the sake of a paste nicety. So the assertion here is that setup
    /// *succeeds* whether or not the terminal can bracket pastes.
    ///
    /// It passes on a console that does support VT as well, where the
    /// sequences are written normally — either way, no error.
    #[cfg(windows)]
    #[test]
    fn setup_survives_a_terminal_that_cannot_bracket_pastes() {
        let mut buf = Vec::new();
        enable_modes_into(&mut buf, false)
            .expect("an unsupported bracketed paste must not fail startup");
        enable_modes_into(&mut buf, true).expect("nor with mouse reporting on");
    }

    #[cfg(not(windows))]
    #[test]
    fn the_teardown_always_turns_bracketed_paste_back_off() {
        // 2004 is the DEC private mode for bracketed paste; `l` disables it.
        const DISABLE_PASTE: &[u8] = b"\x1b[?2004l";
        // 1000/1002/1003/1015/1006 are the mouse-reporting modes crossterm
        // switches off together; checking one is enough to tell the two
        // sequences apart.
        const DISABLE_MOUSE_TAIL: &[u8] = b"\x1b[?1006l";

        let mut without_mouse = Vec::new();
        restore_modes_into(&mut without_mouse, false).expect("writing to a Vec cannot fail");
        assert!(
            contains(&without_mouse, DISABLE_PASTE),
            "bracketed paste must be disabled even when mouse capture never was: {:?}",
            String::from_utf8_lossy(&without_mouse)
        );
        assert!(
            !contains(&without_mouse, DISABLE_MOUSE_TAIL),
            "mouse capture that was never taken must not be released: {:?}",
            String::from_utf8_lossy(&without_mouse)
        );

        let mut with_mouse = Vec::new();
        restore_modes_into(&mut with_mouse, true).expect("writing to a Vec cannot fail");
        assert!(
            contains(&with_mouse, DISABLE_PASTE),
            "bracketed paste must be disabled alongside mouse capture: {:?}",
            String::from_utf8_lossy(&with_mouse)
        );
        assert!(
            contains(&with_mouse, DISABLE_MOUSE_TAIL),
            "mouse capture that was taken must be released: {:?}",
            String::from_utf8_lossy(&with_mouse)
        );
    }

    /// The teardown's Windows counterpart. `DisableBracketedPaste`'s
    /// `execute_winapi` returns `Ok(())` rather than `Unsupported`, so this
    /// is the weaker claim that it stays infallible — `restore_terminal`
    /// swallows errors anyway, but a teardown that started failing would be
    /// worth knowing about.
    #[cfg(windows)]
    #[test]
    fn the_teardown_never_fails_on_windows() {
        let mut buf = Vec::new();
        restore_modes_into(&mut buf, false).expect("teardown must not fail");
        restore_modes_into(&mut buf, true).expect("nor with mouse capture released");
    }

    #[cfg(not(windows))]
    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    /// Declining has to stop the screen coming back. `consent_needed` is
    /// computed here, from `consent_acknowledged` alone, so this is the
    /// place the re-prompt property actually lives — `config`'s own test
    /// proves the flag survives a reload, and this proves the flag is what
    /// the next boot reads.
    ///
    /// The bug this pins: `ConsentAcknowledged { enabled }` used to be
    /// applied as `consent_acknowledged = enabled`, which recorded a Disable
    /// as "never answered". The screen then reappeared on every launch, for
    /// ever, and the user's decline was never written down.
    #[test]
    fn declining_consent_is_remembered_and_does_not_re_prompt() {
        let cli = Cli::try_parse_from(["tgt"]).unwrap();
        let size = ratatui::layout::Size {
            width: 120,
            height: 40,
        };

        let mut config = Config::default();
        assert!(
            boot_from(&config, &cli, size, [0u8; 32]).consent_needed,
            "a fresh install must show the screen"
        );

        config.apply_patch(&tgt_core::effect::ConfigPatch::ConsentAcknowledged { enabled: false });

        let boot = boot_from(&config, &cli, size, [0u8; 32]);
        assert!(
            !boot.consent_needed,
            "declining must be recorded as an answer, not as silence"
        );
        assert_eq!(
            boot.telemetry_mode,
            TelemetryMode::Off,
            "and the answer itself has to be the one that was given"
        );
        assert!(!config.crash_reports_enabled());
        assert!(config.custom_destination().is_none());

        // Accepting is the same shape, with the other answer.
        let mut accepted = Config::default();
        accepted.apply_patch(&tgt_core::effect::ConfigPatch::ConsentAcknowledged { enabled: true });
        let boot = boot_from(&accepted, &cli, size, [0u8; 32]);
        assert!(!boot.consent_needed);
        assert_eq!(boot.telemetry_mode, TelemetryMode::On);
    }

    /// `--no-telemetry` has to reach *both* egresses, and it reaches them
    /// through the `session_config` `run_tui` builds — the flag is applied
    /// to `telemetry_mode`, and both `crash_reports_enabled` and
    /// `custom_destination` gate on that. This reproduces that assembly, so
    /// a future change that gave one egress its own path off `config`
    /// instead would fail here.
    #[test]
    fn no_telemetry_flag_silences_crash_reports_and_otlp_alike() {
        let cli = Cli::try_parse_from(["tgt", "--no-telemetry"]).unwrap();

        // A config that asks for everything: both egresses on, a collector
        // named, consent long since given.
        let config = Config {
            telemetry_mode: TelemetryMode::On,
            telemetry_crash_reports: true,
            telemetry_endpoint: Some("https://collector.example/".to_string()),
            consent_acknowledged: true,
            ..Config::default()
        };
        assert!(config.crash_reports_enabled());
        assert!(config.custom_destination().is_some());

        let session_config = Config {
            telemetry_mode: effective_telemetry_mode(&cli, config.telemetry_mode),
            ..config
        };
        assert!(!session_config.crash_reports_enabled());
        assert!(session_config.custom_destination().is_none());
        assert!(crash::init(session_config.crash_reports_enabled()).is_none());
    }

    /// Every protocol the probe can report has to reach `tgt-ui` as the
    /// capability that speaks it — a mapping that is easy to get silently
    /// wrong (the two enums are deliberately separate types, and three of
    /// the four variants have near-identical names).
    #[test]
    fn every_probed_protocol_maps_to_its_ui_capability() {
        use graphics::GraphicsProtocol;
        use tgt_ui::render::image::Capability;

        for (protocol, expected) in [
            (GraphicsProtocol::Kitty, Some(Capability::Kitty)),
            (GraphicsProtocol::Iterm2, Some(Capability::Iterm2)),
            (GraphicsProtocol::Sixel, Some(Capability::Sixel)),
            (GraphicsProtocol::None, None),
        ] {
            assert_eq!(graphics_capability(protocol, true), expected);
            assert_eq!(
                graphics_capability(protocol, false),
                None,
                "[app].inline_images = false must beat any probed protocol"
            );
        }
    }

    /// `tgt-core`'s `state::palette::BUILTIN_THEME_NAMES` is a copy of
    /// `tgt_ui::theme::loader::builtin_names()` — `tgt-core` can't depend on
    /// `tgt-ui` to read the real catalogue directly (crate-boundary rule,
    /// architecture.md §2), so `ToggleTheme` cycles its own list instead.
    /// `tgt-app` is the one crate that depends on both and can catch the two
    /// drifting apart, which would otherwise show up only as a silent gap
    /// in the palette's theme cycle (a catalogue entry `ToggleTheme` can
    /// never land on) or a name it offers that `resolve_theme` can't
    /// actually resolve to that built-in.
    #[test]
    fn palette_builtin_theme_names_matches_the_real_catalogue() {
        assert_eq!(
            tgt_core::state::palette::BUILTIN_THEME_NAMES.as_slice(),
            tgt_ui::theme::loader::builtin_names(),
        );
    }

    /// Every name in the shared catalogue must actually resolve through
    /// this file's `resolve_theme` — the same guarantee
    /// `theme::loader::builtin_catalogue_resolves_every_name_and_defines_every_token`
    /// gives the loader in isolation, checked again here at the real call
    /// site the palette's cycle and startup both go through.
    ///
    /// `resolve_theme` also applies `COLORTERM`-driven truecolor
    /// degradation (module docs on `resolve_theme`), so `expected` runs the
    /// exact same degrade-or-not decision rather than comparing against the
    /// raw catalogue `Theme` — this test is about catalogue coverage, not
    /// re-testing `for_terminal`.
    #[test]
    fn resolve_theme_resolves_every_builtin_catalogue_name() {
        let truecolor = std::env::var("COLORTERM")
            .map(|value| value.contains("truecolor") || value.contains("24bit"))
            .unwrap_or(false);

        for &name in tgt_ui::theme::loader::builtin_names() {
            let resolved = resolve_theme(name);
            let expected = tgt_ui::theme::loader::for_terminal(
                tgt_ui::theme::loader::builtin(name).unwrap(),
                truecolor,
            );
            assert_eq!(
                resolved, expected,
                "resolve_theme({name:?}) did not resolve to the builtin catalogue entry"
            );
        }
    }
}
