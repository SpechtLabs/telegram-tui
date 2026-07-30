//! `Effect` → execution (docs/architecture.md §2.3, §3). For this task's
//! scope, only `Quit` and `Telemetry` do real work; every other effect is
//! logged and dropped until the task that owns it lands (T09 `Td`, T13
//! `SaveConfig`, T32 clipboard/`OpenExternal`, T44 `Alert`).

use tokio::sync::{mpsc, watch};

use tgt_core::action::Action;
use tgt_core::effect::Effect;

/// Executes `Effect`s produced by `App::update`. Holds a clone-able sender
/// into the loop's action channel — for later tasks whose effects complete
/// asynchronously and need to report back as `Action::TdResult`/`Action::Io`
/// — and a `watch` channel the loop selects on to notice `Effect::Quit`.
#[derive(Debug)]
pub struct Dispatcher {
    // Unused outside tests until a task lands an effect that completes
    // asynchronously (T09's `Effect::Td`, T13's `SaveConfig`, ...): those
    // handlers will report back through `action_sender()` below.
    #[allow(dead_code)]
    action_tx: mpsc::Sender<Action>,
    quit_tx: watch::Sender<bool>,
}

impl Dispatcher {
    /// Builds a dispatcher wired to the loop's action channel and returns
    /// the `watch::Receiver` the loop should select on for `Effect::Quit`.
    pub fn new(action_tx: mpsc::Sender<Action>) -> (Self, watch::Receiver<bool>) {
        let (quit_tx, quit_rx) = watch::channel(false);
        (Dispatcher { action_tx, quit_tx }, quit_rx)
    }

    /// A clone of the action-channel sender, for effect handlers landing in
    /// later tasks that need to report a completion back into the loop.
    #[allow(dead_code)]
    pub fn action_sender(&self) -> mpsc::Sender<Action> {
        self.action_tx.clone()
    }

    /// Executes one effect.
    pub fn dispatch(&self, effect: Effect) {
        match effect {
            Effect::Quit => {
                // A send error here only means the loop already exited,
                // which is the outcome we wanted anyway.
                let _ = self.quit_tx.send(true);
            }
            Effect::Telemetry(event) => {
                // The sole `emit!` call site, per architecture.md §4.8.
                tgt_core::emit!(event);
            }
            other => {
                tracing::debug!(
                    effect = effect_kind(&other),
                    "effect not yet wired; dropped"
                );
            }
        }
    }
}

fn effect_kind(effect: &Effect) -> &'static str {
    match effect {
        Effect::Td(req) => req.kind(),
        Effect::Telemetry(_) => "Telemetry",
        Effect::Alert => "Alert",
        Effect::CopyToClipboard { .. } => "CopyToClipboard",
        Effect::OpenExternal { .. } => "OpenExternal",
        Effect::SaveConfig(_) => "SaveConfig",
        Effect::Quit => "Quit",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tgt_core::model::chat::ChatListId;
    use tgt_core::td::request::TdRequest;

    #[tokio::test]
    async fn quit_effect_flips_the_quit_signal() {
        let (action_tx, _action_rx) = mpsc::channel(8);
        let (dispatcher, mut quit_rx) = Dispatcher::new(action_tx);

        assert!(!*quit_rx.borrow());
        dispatcher.dispatch(Effect::Quit);
        quit_rx.changed().await.unwrap();
        assert!(*quit_rx.borrow());
    }

    #[test]
    fn unwired_effect_does_not_panic() {
        let (action_tx, _action_rx) = mpsc::channel(8);
        let (dispatcher, _quit_rx) = Dispatcher::new(action_tx);

        dispatcher.dispatch(Effect::Td(TdRequest::LoadChats {
            list: ChatListId::Main,
            limit: 200,
        }));
    }

    #[test]
    fn action_sender_reaches_the_loop_channel() {
        let (action_tx, mut action_rx) = mpsc::channel(8);
        let (dispatcher, _quit_rx) = Dispatcher::new(action_tx);

        let sender = dispatcher.action_sender();
        sender
            .try_send(Action::Resize {
                width: 1,
                height: 1,
            })
            .unwrap();
        assert!(action_rx.try_recv().is_ok());
    }
}
