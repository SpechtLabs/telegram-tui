//! Rolling file log under the XDG state directory (spec §13.3): the only
//! writer while the TUI is active, since nothing may reach stdout/stderr
//! once raw mode and the alternate screen are engaged. See
//! docs/architecture.md §9.3.

use std::path::PathBuf;

use color_eyre::eyre;
use etcetera::BaseStrategy;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

const APP_DIR: &str = "telegram-tui";
const LOG_FILE_PREFIX: &str = "tgt.log";

/// Installs a daily-rolling, non-blocking file logger as the global tracing
/// subscriber and returns its `WorkerGuard`. The guard must be held for the
/// process lifetime: dropping it flushes and stops the writer thread.
pub fn init() -> eyre::Result<WorkerGuard> {
    let dir = state_dir()?;
    std::fs::create_dir_all(&dir)?;

    let file_appender = tracing_appender::rolling::daily(&dir, LOG_FILE_PREFIX);
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(file_layer)
        .try_init()
        .map_err(|err| eyre::eyre!("failed to install tracing subscriber: {err}"))?;

    Ok(guard)
}

/// `$XDG_STATE_HOME/telegram-tui/`, defaulting to `~/.local/state/telegram-tui/`.
fn state_dir() -> eyre::Result<PathBuf> {
    let strategy = etcetera::choose_base_strategy()
        .map_err(|err| eyre::eyre!("could not determine the state directory: {err}"))?;
    let base = strategy
        .state_dir()
        .ok_or_else(|| eyre::eyre!("this platform strategy has no state directory"))?;
    Ok(base.join(APP_DIR))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    // `XDG_STATE_HOME` is process-wide; serialize any test that mutates it
    // so parallel `cargo test` runs don't race each other.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn logging_writes_under_state_dir() {
        let _lock = ENV_LOCK.lock().unwrap();

        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: serialized by ENV_LOCK above; no other thread reads or
        // writes XDG_STATE_HOME while this guard is held.
        unsafe {
            std::env::set_var("XDG_STATE_HOME", tmp.path());
        }

        let guard = init().expect("logging::init should succeed against a tempdir");
        tracing::info!("logging smoke test event");
        drop(guard);

        unsafe {
            std::env::remove_var("XDG_STATE_HOME");
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
