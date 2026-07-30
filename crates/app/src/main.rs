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
use tgt_core::model::key::KeyBindings;
use tgt_ui::theme::Theme;

use cli::{Cli, Command};

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

    // Wraps whatever hook color-eyre just installed: on panic, the terminal
    // is restored first, then color-eyre's report prints to a usable shell.
    panic::install(restore_terminal);

    enable_raw_mode()?;
    // From here on, every exit path — normal return, an early `?`, or an
    // unwinding panic — restores the terminal via this guard's `Drop`.
    let _terminal_guard = TerminalGuard;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let boot = default_boot(terminal.size()?);
    let mut app = App::new(boot);
    let theme = Theme::default_dark();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(runtime_loop::run(&mut app, &theme, &mut terminal))?;

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

fn default_boot(size: ratatui::layout::Size) -> Boot {
    Boot {
        theme_name: "default".to_string(),
        bindings: KeyBindings::default(),
        layout_breakpoint_cols: 100,
        telemetry_mode: TelemetryMode::Off,
        telemetry_salt: [0u8; 32],
        consent_needed: false,
        has_credentials: false,
        width: size.width,
        height: size.height,
    }
}
