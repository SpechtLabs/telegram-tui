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
use std::sync::{Arc, Mutex};

use clap::Parser;
use color_eyre::eyre;
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

use cli::{Cli, Command};
use config::Config;
use dispatch::TdBootParams;
use td_runtime::TdlibRuntime;

fn main() -> eyre::Result<()> {
    let cli = Cli::parse();

    if matches!(cli.command, Some(Command::Telemetry { .. })) {
        // T51 fills this in; for now it must exist and exit cleanly so the
        // subcommand shape is stable for every task that follows. The TUI
        // never starts on this path, so stdout is fair game.
        println!("not implemented until T51");
        return Ok(());
    }

    run_tui(cli)
}

fn run_tui(cli: Cli) -> eyre::Result<()> {
    color_eyre::install()?;

    // Must happen before raw mode: the file logger is the only writer
    // allowed once the TUI takes over the terminal (spec §13.3).
    let _log_guard = logging::init()?;
    tracing::debug!(no_telemetry = cli.no_telemetry, "cli parsed");

    // Everything that can fail loudly — and the Keychain prompt, which may
    // put a dialog on screen — happens before raw mode, while stderr is
    // still a usable place for an error report.
    let config = config::load()?;
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
    let _terminal_guard = TerminalGuard;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let app = App::new(boot_from(&config, &cli, terminal.size()?));
    let theme = Theme::default_dark();
    // Shared with the dispatcher, which applies `Effect::SaveConfig` patches
    // and reads the credentials back when TDLib asks for its parameters.
    let config = Arc::new(Mutex::new(config));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        // Construction is async (it silences TDLib's own logger before
        // anything can write to stderr) and takes no parameters: TDLib is
        // configured in-band, by the `SetTdlibParameters` the dispatcher
        // sends when TDLib reports `WaitTdlibParameters`.
        let runtime: Arc<dyn TdRuntime> = Arc::new(TdlibRuntime::new().await);
        runtime_loop::run(app, &theme, &mut terminal, runtime, config, td_boot).await
    })?;

    Ok(())
}

/// Restores the terminal: leaves the alternate screen, disables raw mode.
/// Errors are swallowed — there is nothing more to do if this itself fails,
/// and it may run from inside the panic hook where propagating isn't an
/// option.
fn restore_terminal() {
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

/// Projects the loaded config (plus the CLI flag and the terminal size) into
/// the plain boot values `tgt-core` is handed. Everything impure about
/// startup is resolved here and nowhere else.
fn boot_from(config: &Config, cli: &Cli, size: ratatui::layout::Size) -> Boot {
    let fields = config.boot_fields();
    Boot {
        theme_name: fields.theme_name,
        bindings: fields.bindings,
        layout_breakpoint_cols: fields.layout_breakpoint_cols,
        telemetry_mode: if cli.no_telemetry {
            TelemetryMode::Off
        } else {
            fields.telemetry_mode
        },
        // TODO(T49/T51): a real per-install HMAC salt is generated and
        // persisted alongside the install id. Nothing exports until T49
        // builds the exporter, so a zero salt hashes nothing meanwhile.
        telemetry_salt: [0u8; 32],
        // TODO(T50): the first-run consent screen decides this from
        // `config.consent_acknowledged`. Until that screen exists there is
        // nothing to consent to (no exporter until T49), so booting straight
        // to auth is honest rather than skipping a gate.
        consent_needed: false,
        has_credentials: fields.has_credentials,
        width: size.width,
        height: size.height,
    }
}
