//! `tgt --demo`: runs the real TUI (the actual `runtime_loop::Core`, the real
//! dispatcher, the real `App::update`) against a scripted, offline chat
//! history instead of a Telegram account. Built so the README and docs site
//! can show `tgt`'s features working — a chat list with folders and unread
//! badges, opening a conversation, a reply and a reaction, a spoiler being
//! revealed, an inline photo — without recording (or risking) anyone's real
//! conversations, and as one committed, regenerable `asciinema` recording
//! rather than a hand-performed one nobody can reproduce after the UI moves.
//!
//! # It is structurally unable to reach a real account
//!
//! This module never imports `crate::td_runtime` — the only module in this
//! crate that imports `tdlib_rs` (the same rule architecture.md's crate
//! boundaries enforce for `tgt-core`/`tgt-ui`, just self-applied here rather
//! than checked by `scripts/check-crate-boundaries.sh`). [`run`] builds a
//! `tgt_core::td::fake::FakeTd` (via [`script::build`]) and hands it to
//! `runtime_loop::run` in exactly the slot `main.rs::run_tui` hands a
//! `TdlibRuntime` — there is no code path from here to the real client, not
//! a flag that happens to be off.
//!
//! Three more things follow from never touching the real profile:
//!
//! - **No config file.** `run` never calls `config::load()`; it builds a
//!   `Config` literal in memory. Nothing here reads
//!   `~/.config/telegram-tui/`.
//! - **No Keychain entry.** `keychain::db_key()` and
//!   `keychain::td_database_dir()` are never called. `TdBootParams` is filled
//!   with a disposable path under this run's scratch directory (below), and
//!   it is never actually read: the fixture emits `AuthPhase::Ready`
//!   immediately, so `Dispatcher::request_tdlib_parameters` — the only
//!   reader of `TdBootParams`'s two fields — never fires.
//! - **No network.** `otel::init` and `crash::init` are never called, so
//!   there is no OTLP exporter and no Sentry client for anything to go
//!   through even if telemetry were left on (it isn't — `Boot.telemetry_mode`
//!   is forced `Off` below). `FakeTd` is a plain in-memory struct; it never
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
//! # Runtime design: a scripted fixture, not a lenient backend
//!
//! An earlier version of this module built a lenient, freely-drivable
//! in-memory `TdRuntime` (answering *any* request against live mutable
//! state), reasoning that a playable demo needed to tolerate an
//! unpredictable interaction. The brief has since narrowed to producing one
//! committed, scripted `asciinema` recording — and re-recording a script is
//! just re-running it, so a strict, single-cursor `FakeTd` fixture
//! (`crate::td::fake`, already exercised by seven integration test binaries)
//! gets the same regenerability for far less code, *provided* the fixture
//! can predict the exact `TdRequest` sequence the driver script produces.
//!
//! That was the real risk worth checking before committing to the simpler
//! design, and it is a genuine one here, not a hypothetical: opening a chat
//! for the first time triggers `state::conversation::apply_history_page`'s
//! "cold open" case, which can fan out into *multiple* `TdRequest`s from a
//! single `update()` call — T59's remote reconcile, T67's viewport-fill, and
//! (separately) an auto-download `DownloadFile` for any photo now in view —
//! each dispatched via its own `tokio::spawn`, with no ordering guarantee
//! across them (see `runtime_loop::Core::step_until`'s `effects.drain()`
//! loop, and `apply_history_page`'s own doc comment: "a cold open does put
//! both in flight at once"). A strict linear script cannot predict which of
//! those arrives first, so [`content`] structurally avoids the fan-out
//! rather than betting on an order: Nova's opening page is padded to a full
//! `PAGE_SIZE` (50) messages, which is what starves `fill_viewport` of a
//! reason to fire at all (see `content.rs`'s module docs — this mirrors
//! `read_only.rs`'s own `read_only_script`), and the photo's `FileSnapshot`
//! is pushed as already-downloaded before the chat ever opens, which is what
//! keeps `DownloadFile` from being issued in the first place. What's left —
//! `LoadChats` and exactly two `GetChatHistory` requests — is precisely
//! predictable, so [`script::build`] scripts those and lets `FakeTd`'s
//! default `Ok` fallback answer everything else (`OpenChat`, `ViewMessages`,
//! `CloseChat`), the same way `read_only.rs`'s tests do.
//!
//! # Content
//!
//! `content.rs` seeds five fictional chats: Nova (the one the recording
//! opens) carries a reply, a reaction, an edited message, a spoiler and the
//! demo's one photo, as the newest of a full 50-message history; the other
//! four exist only to populate the sidebar — an unread badge, a group, a
//! channel and a supergroup, two of the latter in folders — and are never
//! opened. `photo.rs` supplies the photo itself: a drawn placeholder by
//! default, or a real file via `TGT_DEMO_PHOTO` (see that module's docs for
//! how to point it at an actual photo without rebuilding). Everything is
//! invented: no real names, no plausible phone numbers, nothing that could
//! be mistaken for a real person's messages.

mod content;
mod photo;
mod script;

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
/// `runtime_loop::run` against the scripted fixture [`script::build`]
/// produces, then teardown. Mirrors `main.rs::run_tui`'s shape closely
/// enough to reuse its small pure helpers (`enable_modes_into`,
/// `graphics_capability`, `resolve_theme`, `TerminalGuard`, `MOUSE_CAPTURE`
/// — all crate-root items, visible here as this module's ancestor, per
/// Rust's ordinary privacy rules) but skips every step that would touch a
/// real account. See the module docs for exactly what that means and what
/// backs the guarantee.
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
        // for recording. The fixture carries the session the rest of the
        // way to `Screen::Main` by emitting `AuthPhase::Ready` immediately.
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
        let runtime: Arc<dyn TdRuntime> = Arc::new(script::build(photo_path));
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
            // No restart factory: the fixture never reports
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
