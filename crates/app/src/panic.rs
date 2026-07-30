//! Panic hook that restores the terminal before the default hook prints
//! (docs/architecture.md §9.3, spec §14): a TUI that panics with raw mode
//! still enabled leaves the user with an unusable shell.

use std::panic::{self, PanicHookInfo};

/// Installs a panic hook that runs `restore` first, then chains into
/// whatever hook was previously installed (color-eyre's colorized printer,
/// or the Rust default). Call this *after* anything else that installs its
/// own panic hook, so `restore` runs before that hook's output.
pub fn install(restore: impl Fn() + Send + Sync + 'static) {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info: &PanicHookInfo<'_>| {
        restore();
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    // `panic::set_hook` is process-global; serialize tests that touch it so
    // parallel `cargo test` runs don't clobber each other's hook.
    static PANIC_HOOK_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn panic_hook_runs_restore_before_default() {
        let _lock = PANIC_HOOK_LOCK.lock().unwrap();

        let restore_ran = Arc::new(AtomicBool::new(false));
        let restore_had_run_when_chained_hook_fired = Arc::new(AtomicBool::new(false));

        let saved_hook = panic::take_hook();

        // Stands in for "whatever hook was previously installed" (e.g.
        // color-eyre's, or the Rust default): records whether `restore` had
        // already run by the time it's invoked.
        {
            let restore_ran = Arc::clone(&restore_ran);
            let restore_had_run_when_chained_hook_fired =
                Arc::clone(&restore_had_run_when_chained_hook_fired);
            panic::set_hook(Box::new(move |_info| {
                restore_had_run_when_chained_hook_fired
                    .store(restore_ran.load(Ordering::SeqCst), Ordering::SeqCst);
            }));
        }

        install({
            let restore_ran = Arc::clone(&restore_ran);
            move || restore_ran.store(true, Ordering::SeqCst)
        });

        let result = panic::catch_unwind(|| panic!("panic_hook_runs_restore_before_default probe"));
        assert!(result.is_err());

        // Restore whatever hook was active before this test ran, so later
        // tests (and any panics they trigger) aren't affected.
        panic::set_hook(saved_hook);

        assert!(
            restore_ran.load(Ordering::SeqCst),
            "restore closure never ran"
        );
        assert!(
            restore_had_run_when_chained_hook_fired.load(Ordering::SeqCst),
            "restore ran after the chained hook instead of before it"
        );
    }
}
