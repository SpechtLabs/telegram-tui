//! `tgt --demo`: runs the real TUI (the actual `runtime_loop::Core`, the real
//! dispatcher, the real `App::update`) against an in-memory, scripted chat
//! history instead of a Telegram account. Built for recordings: the README
//! and docs site want to show `tgt`'s features working without recording (or
//! risking) anyone's real conversations, and without asking a viewer to
//! bring their own Telegram account and API credentials just to reproduce a
//! screenshot.
//!
//! # It is structurally unable to reach a real account
//!
//! This module never imports `crate::td_runtime` — the only module in this
//! crate that imports `tdlib_rs` (the same rule architecture.md's crate
//! boundaries enforce for `tgt-core`/`tgt-ui`, just self-applied here rather
//! than checked by `scripts/check-crate-boundaries.sh`). [`run`] builds a
//! [`runtime::DemoTd`] and hands it to `runtime_loop::run` in exactly the
//! slot `main.rs::run_tui` hands a `TdlibRuntime` — there is no code path
//! from here to the real client, not a flag that happens to be off.
//!
//! Three more things follow from never touching the real profile:
//!
//! - **No config file.** `run` never calls `config::load()`; it builds a
//!   `Config` literal in memory. Nothing here reads
//!   `~/.config/telegram-tui/`.
//! - **No Keychain entry.** `keychain::db_key()` and
//!   `keychain::td_database_dir()` are never called. `TdBootParams` is filled
//!   with a disposable path under this run's scratch directory (below), and
//!   it is never actually read: `DemoTd` emits `AuthPhase::Ready` at
//!   construction, so `Dispatcher::request_tdlib_parameters` — the only
//!   reader of `TdBootParams`'s two fields — never fires.
//! - **No network.** `otel::init` and `crash::init` are never called, so
//!   there is no OTLP exporter and no Sentry client for anything to go
//!   through even if telemetry were left on (it isn't — `Boot.telemetry_mode`
//!   is forced `Off` below). `DemoTd` is a plain in-memory struct; it never
//!   opens a socket.
//!
//! As defense in depth — in case a future change ever adds a code path that
//! *would* touch the real profile — [`run`] also redirects `XDG_CONFIG_HOME`/
//! `XDG_DATA_HOME`/`XDG_STATE_HOME` (and their Windows equivalents,
//! `APPDATA`/`LOCALAPPDATA`) to a fresh temporary directory before anything
//! else runs, for the life of the process. A stray `Effect::SaveConfig` (a
//! theme toggle from the palette, say) still resolves to a real
//! `config.save()` call, but with these set it writes into the scratch
//! directory instead of the user's real config. This does **not** cover the
//! Keychain: `keyring::Entry::new` talks to the platform credential store
//! directly, not through these directories — which is exactly why the
//! stronger guarantee above is "never called" rather than "redirected".
//!
//! # Runtime design: a lenient in-memory backend, not a scripted fixture
//!
//! `runtime::DemoTd`'s module docs cover this in depth; the short version is
//! that `crate::td::fake::FakeTd`-style scripted fixtures expect requests in
//! a fixed order, which is right for an assertion-driven test and wrong for
//! something a person (or a recording script) drives live — scroll further
//! than planned, open chats out of order, type something unscripted, and a
//! strict script stalls. `DemoTd` instead holds the mock chats as live,
//! mutable state and answers any request that names something the data has,
//! so the same demo is re-recordable and interactively drivable rather than
//! a single fragile take.
//!
//! # Content
//!
//! `content.rs` seeds five fictional chats: one built to show off a reply, a
//! reaction, an edited message, a spoiler and an inline photo in a single
//! open-chat pass; another with an unread badge; a group, a channel and a
//! supergroup, two of the latter in folders. `photo.rs` supplies the one
//! photo — a drawn placeholder by default, or a real file via
//! `TGT_DEMO_PHOTO` (see that module's docs for how to point it at an actual
//! photo without rebuilding). Everything is invented: no real names, no
//! plausible phone numbers, nothing that could be mistaken for a real
//! person's messages.

mod content;
mod photo;
mod runtime;

use std::io;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use color_eyre::eyre;
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use tgt_core::app::{App, Boot};
use tgt_core::effect::TelemetryMode;
use tgt_core::model::key::KeyBindings;
use tgt_core::td::runtime::TdRuntime;

use crate::config::Config;
use crate::dispatch::TdBootParams;
use crate::runtime_loop;
use crate::{MOUSE_CAPTURE, TerminalGuard, enable_modes_into, graphics_capability, resolve_theme};

/// Runs the demo session to completion: terminal setup, the real
/// `runtime_loop::run` against a [`runtime::DemoTd`], then teardown. Mirrors
/// `main.rs::run_tui`'s shape closely enough to reuse its small pure
/// helpers (`enable_modes_into`, `graphics_capability`, `resolve_theme`,
/// `TerminalGuard`, `MOUSE_CAPTURE` — all crate-root items, visible here as
/// this module's ancestor, per Rust's ordinary privacy rules) but skips
/// every step that would touch a real account. See the module docs for
/// exactly what that means and what backs the guarantee.
pub fn run() -> eyre::Result<()> {
    // Every directory a stray write could land in, redirected before
    // anything else runs. See the module docs' "structurally unable to
    // reach a real account" section for what this covers and what it does
    // not.
    let scratch = tempfile::tempdir()?;
    redirect_state_dirs(scratch.path());

    color_eyre::install()?;
    let (_log_guard, _export_handle) = crate::logging::init()?;
    tracing::info!(
        scratch = %scratch.path().display(),
        "tgt --demo starting; offline, in-memory, cannot reach a real account"
    );

    let (photo_path, _width, _height) = photo::resolve(scratch.path())?;

    let graphics = graphics_capability(crate::graphics::probe(), true);

    let config = Config {
        // Forced off, not merely defaulted off: a demo session is not the
        // place to ask whether the project may collect anonymous usage data
        // from something that isn't a real usage session (module docs).
        telemetry_mode: TelemetryMode::Off,
        consent_acknowledged: true,
        ..Config::default()
    };

    let td_boot = TdBootParams {
        // Never read (module docs) — pointed at the scratch directory
        // anyway, as defense in depth.
        database_directory: scratch.path().join("td"),
        database_encryption_key: vec![0u8; 32],
    };

    crate::panic::install(crate::restore_terminal);

    enable_raw_mode()?;
    // From here on, every exit path restores the terminal via this guard's
    // `Drop`, exactly like `run_tui`'s.
    let terminal_guard = TerminalGuard;
    execute!(io::stdout(), EnterAlternateScreen)?;
    if config.mouse {
        MOUSE_CAPTURE.store(true, Ordering::SeqCst);
    }
    enable_modes_into(&mut io::stdout(), config.mouse)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let size = terminal.size()?;

    let boot = Boot {
        theme_name: config.theme.clone(),
        bindings: KeyBindings::default(),
        layout_breakpoint_cols: config.layout_breakpoint_cols,
        telemetry_mode: TelemetryMode::Off,
        telemetry_salt: [0u8; 32],
        crash_reports_available: false,
        // Skips both the first-run consent screen and the my.telegram.org
        // credentials wizard: a demo that asked for either would be useless
        // for recording. `DemoTd` carries the session the rest of the way
        // to `Screen::Main` by emitting `AuthPhase::Ready` immediately.
        consent_needed: false,
        has_credentials: true,
        width: size.width,
        height: size.height,
        auto_download_photos: config.auto_download_photos,
    };
    let app = App::new(boot);
    let theme = resolve_theme(&config.theme);
    let config = Arc::new(Mutex::new(config));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let outcome = rt.block_on(async move {
        let runtime: Arc<dyn TdRuntime> = Arc::new(runtime::DemoTd::new(photo_path));
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
                measure_cell: crate::graphics::cell_size,
            },
            // No restart factory: `DemoTd` never reports
            // `AuthPhase::Closed`, so there is nothing to restart from.
            None,
        )
        .await
    });

    // Restore the terminal before returning, matching `run_tui`'s ordering
    // (the guard would do this anyway on drop, but explicit here since
    // there is no telemetry flush after it to sequence against).
    drop(terminal_guard);
    outcome
}

/// Points every directory a config write, a log file or (if it were ever
/// created) a TDLib database could land in at `dir` instead of the user's
/// real profile. See the module docs for what this does and does not cover.
///
/// Unlike `logging::tests`' equivalent test-only helpers, this runs in a
/// real binary invocation, not a test — but the same platform split applies:
/// `etcetera`'s native strategy is XDG on unix (so `XDG_CONFIG_HOME` and
/// friends are read) and reads `APPDATA`/`LOCALAPPDATA` directly on Windows,
/// never XDG names.
fn redirect_state_dirs(dir: &std::path::Path) {
    // SAFETY: called once, at the very start of `run`, before the tokio
    // runtime, the terminal event reader thread, or anything else that
    // could plausibly be reading these variables concurrently exists.
    unsafe {
        #[cfg(unix)]
        {
            std::env::set_var("XDG_CONFIG_HOME", dir);
            std::env::set_var("XDG_DATA_HOME", dir);
            std::env::set_var("XDG_STATE_HOME", dir);
        }
        #[cfg(windows)]
        {
            std::env::set_var("APPDATA", dir);
            std::env::set_var("LOCALAPPDATA", dir);
        }
    }
}
