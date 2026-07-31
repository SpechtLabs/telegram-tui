//! Rolling file log under the XDG state directory (spec §13.3): the only
//! writer while the TUI is active, since nothing may reach stdout/stderr
//! once raw mode and the alternate screen are engaged. See
//! docs/architecture.md §9.3.
//!
//! This module owns the process's single subscriber. The OTLP exporter
//! (`otel`) is a second layer in that same registry rather than a second
//! subscriber, because only one subscriber can be global.
//!
//! # Why the export layer arrives late
//!
//! Whether to export at all depends on the config file (consent, mode,
//! endpoint), and reading that config already wants to warn about unknown
//! keys — which needs a subscriber. Rather than choosing which of the two
//! loses, `init` installs a [`tracing_subscriber::reload`] placeholder for
//! the exporter and hands back a handle; `main` fills it in once the config
//! has been read and consent checked. Nothing is exported before that call,
//! and the log file is live from the first line of `run_tui`.

use std::path::PathBuf;

use color_eyre::eyre;
use etcetera::BaseStrategy;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, Registry, reload};

const APP_DIR: &str = "telegram-tui";
const LOG_FILE_PREFIX: &str = "tgt.log";

/// The exporter layer, boxed so `logging` need not name the OTLP types.
/// Layered directly onto the `Registry`, which is what lets it be boxed
/// against a single, stable subscriber type.
pub type ExportLayer = Box<dyn tracing_subscriber::Layer<Registry> + Send + Sync + 'static>;

/// Swaps the exporter into the live subscriber. See [`install_export_layer`].
pub type ExportHandle = reload::Handle<Option<ExportLayer>, Registry>;

/// Installs a daily-rolling, non-blocking file logger as the global tracing
/// subscriber, with an empty slot reserved for the OTLP exporter.
///
/// The returned `WorkerGuard` must be held for the process lifetime:
/// dropping it flushes and stops the writer thread.
pub fn init() -> eyre::Result<(WorkerGuard, ExportHandle)> {
    let dir = state_dir()?;
    std::fs::create_dir_all(&dir)?;

    let file_appender = tracing_appender::rolling::daily(&dir, LOG_FILE_PREFIX);
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    // `RUST_LOG` is a knob for the local file, so it is a per-layer filter
    // rather than a global one. Turning the file log down to `warn` while
    // debugging something noisy must not also silence telemetry, whose
    // events are emitted at `info`.
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_filter(env_filter);

    let (export_slot, export_handle) = reload::Layer::new(None::<ExportLayer>);

    tracing_subscriber::registry()
        .with(export_slot)
        .with(file_layer)
        .try_init()
        .map_err(|err| eyre::eyre!("failed to install tracing subscriber: {err}"))?;

    Ok((guard, export_handle))
}

/// Puts the OTLP exporter into the slot `init` reserved. Every event emitted
/// after this returns is offered to it; everything before is file-only.
pub fn install_export_layer(handle: &ExportHandle, layer: ExportLayer) -> eyre::Result<()> {
    handle
        .reload(Some(layer))
        .map_err(|err| eyre::eyre!("failed to install the telemetry export layer: {err}"))
}

/// `$XDG_STATE_HOME/telegram-tui/`, defaulting to `~/.local/state/telegram-tui/`,
/// and `%LOCALAPPDATA%\telegram-tui\` on Windows.
///
/// A state directory is an XDG idea, so `etcetera`'s Windows strategy answers
/// `None` for it. Its cache directory is `%LOCALAPPDATA%`, which is where
/// Windows expects exactly this kind of file: regenerable, machine-local, and
/// specifically not something to sync onto every other machine the user signs
/// in to — which `%APPDATA%`, the roaming half and the one holding the config
/// and the database, would do. On unix `state_dir` always answers, so the
/// fallback is unreachable there and `~/.cache` is never used.
fn state_dir() -> eyre::Result<PathBuf> {
    let strategy = etcetera::choose_base_strategy()
        .map_err(|err| eyre::eyre!("could not determine the state directory: {err}"))?;
    let base = strategy.state_dir().unwrap_or_else(|| strategy.cache_dir());
    Ok(base.join(APP_DIR))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::{Mutex, MutexGuard};

    use super::*;

    // The environment is process-wide; serialize any test that mutates it
    // so parallel `cargo test` runs don't race each other. Shared with
    // `otel`'s tests, which set `XDG_CONFIG_HOME`.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Held for as long as a test needs the process environment to itself.
    /// Poisoning is ignored: a panicking test leaves the environment dirty,
    /// not the lock's data, and every holder sets what it needs anyway.
    pub(crate) fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner())
    }

    /// Generates a `set_<x>_dir`/`unset_<x>_dir` pair that redirects one of
    /// `etcetera`'s directories into a tempdir for the life of an
    /// `env_lock()` guard.
    ///
    /// The variable name is platform-dependent because
    /// `etcetera::choose_base_strategy()` picks the OS's *native* strategy,
    /// not always XDG. On unix that native strategy *is* XDG, so
    /// `XDG_*_HOME` works. On Windows it reads `APPDATA`/`LOCALAPPDATA`
    /// straight from the environment before ever falling back to the
    /// Roaming/Local known folders (see
    /// `etcetera-0.11.0/src/base_strategy/windows.rs`) and never looks at
    /// `XDG_*_HOME` at all. `state_dir` in particular has no Windows XDG
    /// equivalent at all: `etcetera` answers `None` for it there and
    /// `logging::state_dir` falls back to `cache_dir`, which is
    /// `%LOCALAPPDATA%` — the same variable `data_dir` uses. Setting only
    /// the XDG name here would pass silently on unix while silently
    /// reading and writing the *real* user profile on Windows (the bug this
    /// module exists to close, T71). Do not "simplify" this back to a single
    /// XDG_* variable.
    ///
    /// Prefer these `set_*`/`unset_*` pairs to a bare `remove_var` sweep
    /// where the two differ: unsetting `APPDATA`/`LOCALAPPDATA` outright
    /// makes `etcetera` fall back to the *real* known folder rather than
    /// erroring, so a "clear ambient state" step that removes it and then
    /// forgets to set a tempdir back would point a Windows test at the
    /// user's actual profile instead of failing loudly.
    macro_rules! env_dir_override {
        ($set:ident, $unset:ident, unix: $unix_var:literal, windows: $win_var:literal) => {
            // Not every platform uses every directory: the data-dir pair is
            // only reached from `keychain`'s unix-only mode test, so on
            // Windows it is dead and `-D warnings` would reject it. Which
            // helpers are live is a property of each platform's test set, not
            // a mistake worth failing the build over.
            #[allow(dead_code)]
            /// # Safety
            /// Caller must be holding [`env_lock`].
            #[cfg(unix)]
            pub(crate) unsafe fn $set(path: &std::path::Path) {
                // SAFETY: forwarded to the caller of this unsafe fn.
                unsafe { std::env::set_var($unix_var, path) };
            }
            #[allow(dead_code)]
            #[cfg(windows)]
            pub(crate) unsafe fn $set(path: &std::path::Path) {
                // SAFETY: forwarded to the caller of this unsafe fn.
                unsafe { std::env::set_var($win_var, path) };
            }

            #[allow(dead_code)]
            /// # Safety
            /// Caller must be holding [`env_lock`].
            #[cfg(unix)]
            pub(crate) unsafe fn $unset() {
                // SAFETY: forwarded to the caller of this unsafe fn.
                unsafe { std::env::remove_var($unix_var) };
            }
            #[allow(dead_code)]
            #[cfg(windows)]
            pub(crate) unsafe fn $unset() {
                // SAFETY: forwarded to the caller of this unsafe fn.
                unsafe { std::env::remove_var($win_var) };
            }
        };
    }

    env_dir_override!(set_config_dir, unset_config_dir, unix: "XDG_CONFIG_HOME", windows: "APPDATA");
    env_dir_override!(set_data_dir, unset_data_dir, unix: "XDG_DATA_HOME", windows: "LOCALAPPDATA");
    env_dir_override!(set_state_dir, unset_state_dir, unix: "XDG_STATE_HOME", windows: "LOCALAPPDATA");

    #[test]
    fn logging_writes_under_state_dir() {
        let _lock = env_lock();

        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: serialized by ENV_LOCK above; no other thread reads or
        // writes the state-dir override while this guard is held.
        unsafe {
            set_state_dir(tmp.path());
        }

        let (guard, _export_handle) =
            init().expect("logging::init should succeed against a tempdir");
        tracing::info!("logging smoke test event");
        drop(guard);

        // SAFETY: serialized by ENV_LOCK above.
        unsafe {
            unset_state_dir();
        }

        let app_dir = tmp.path().join(APP_DIR);
        let entries: Vec<_> = std::fs::read_dir(&app_dir)
            .unwrap_or_else(|err| panic!("expected {app_dir:?} to exist: {err}"))
            .filter_map(Result::ok)
            .collect();
        assert!(!entries.is_empty(), "no log file created under {app_dir:?}");

        let has_content = entries.iter().any(|entry| {
            std::fs::metadata(entry.path())
                .map(|meta| meta.len() > 0)
                .unwrap_or(false)
        });
        assert!(has_content, "log file(s) under {app_dir:?} were empty");
    }
}
