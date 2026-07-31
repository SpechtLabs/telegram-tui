//! `Effect` → async execution (docs/architecture.md §2.3, §3). Each effect is
//! spawned; its completion re-enters the loop's action channel as an
//! `Action::TdResult` / `Action::Io`, which is the only way a result ever
//! reaches the pure `update()`.
//!
//! Wired so far: `Quit`, `Telemetry`, `Td`, `SaveConfig`. Clipboard,
//! `OpenExternal` (T32) and `Alert` (T44) are still logged and dropped.
//!
//! # `SetTdlibParameters` — the impure boundary
//!
//! `TdlibParams` carries the api credentials, the Keychain database key and
//! the database directory: boot facts `tgt-core` deliberately does not hold,
//! so `state::auth::handle_td` emits no effect for `AuthPhase::
//! WaitTdlibParameters` and this dispatcher issues the request instead (see
//! [`Dispatcher::request_tdlib_parameters`], called from `runtime_loop`).
//!
//! On a first run the credentials do not exist yet when TDLib asks: the auth
//! screen is showing the my.telegram.org wizard, and the request has to wait
//! for it. That case is not an error, it is the normal first-run ordering —
//! the request is marked pending and fired by the `SaveConfig` handler once
//! `ConfigPatch::Credentials` has been persisted.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, watch};

use tgt_core::action::{Action, IoErrorKind, IoResult, TdResult};
use tgt_core::effect::{ConfigPatch, Effect};
use tgt_core::model::ids::ChatId;
use tgt_core::model::message::MessageView;
use tgt_core::td::error::TdError;
use tgt_core::td::request::{TdRequest, TdResponse, TdlibParams};
use tgt_core::td::runtime::TdRuntime;

use crate::config::Config;

/// TDLib parameters that come from neither the config file nor `update()`:
/// the 32-byte database key held in the macOS Keychain and the 0700 database
/// directory. The api credentials are read from the shared [`Config`] at the
/// moment the request is built, so a wizard that has just written them is
/// picked up without rebuilding anything.
#[derive(Debug, Clone)]
pub struct TdBootParams {
    pub database_directory: PathBuf,
    pub database_encryption_key: Vec<u8>,
}

/// Secret chats are a spec non-goal; the three databases are on because the
/// client is expected to survive restarts without re-downloading the world.
/// `system_version` is left to TDLib, which probes the OS itself.
const SYSTEM_LANGUAGE_CODE: &str = "en";
const DEVICE_MODEL: &str = "Mac";

/// Executes `Effect`s produced by `App::update`. Everything an effect needs
/// lives behind one `Arc` so a spawned task can outlive the call that
/// started it.
pub struct Dispatcher {
    inner: Arc<Inner>,
    quit_tx: watch::Sender<bool>,
}

struct Inner {
    action_tx: mpsc::Sender<Action>,
    runtime: Arc<dyn TdRuntime>,
    config: Arc<Mutex<Config>>,
    td_boot: TdBootParams,
    /// TDLib asked for its parameters before credentials existed. Cleared by
    /// whoever fires the deferred request (see module docs).
    params_pending: AtomicBool,
}

impl Dispatcher {
    /// Builds a dispatcher wired to the loop's action channel and returns
    /// the `watch::Receiver` the loop selects on for `Effect::Quit`.
    pub fn new(
        action_tx: mpsc::Sender<Action>,
        runtime: Arc<dyn TdRuntime>,
        config: Arc<Mutex<Config>>,
        td_boot: TdBootParams,
    ) -> (Self, watch::Receiver<bool>) {
        let (quit_tx, quit_rx) = watch::channel(false);
        let inner = Arc::new(Inner {
            action_tx,
            runtime,
            config,
            td_boot,
            params_pending: AtomicBool::new(false),
        });
        (Dispatcher { inner, quit_tx }, quit_rx)
    }

    /// Executes one effect. Never blocks: everything with latency is spawned.
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
            Effect::Td(request) => {
                let inner = Arc::clone(&self.inner);
                tokio::spawn(async move { inner.execute_td(request).await });
            }
            Effect::SaveConfig(patch) => {
                let inner = Arc::clone(&self.inner);
                tokio::spawn(async move { inner.save_config(patch).await });
            }
            other => {
                tracing::debug!(
                    effect = effect_kind(&other),
                    "effect not yet wired; dropped"
                );
            }
        }
    }

    /// Issues `SetTdlibParameters`, or defers it until credentials exist.
    /// Called by `runtime_loop` when TDLib reports
    /// `AuthPhase::WaitTdlibParameters` — see the module docs for why this
    /// does not come out of `update()` as an effect.
    pub fn request_tdlib_parameters(&self) {
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move { inner.send_tdlib_parameters().await });
    }
}

impl Inner {
    async fn send_tdlib_parameters(&self) {
        let Some(params) = self.build_params() else {
            self.params_pending.store(true, Ordering::SeqCst);
            tracing::info!(
                "TDLib asked for parameters before credentials exist; \
                 waiting for the credentials wizard"
            );
            return;
        };
        self.params_pending.store(false, Ordering::SeqCst);
        self.execute_td(TdRequest::SetTdlibParameters(params)).await;
    }

    /// `None` while the my.telegram.org credentials are still missing.
    fn build_params(&self) -> Option<TdlibParams> {
        let config = self.config.lock().unwrap_or_else(|p| p.into_inner());
        let api_id = config.api_id?;
        let api_hash = config.api_hash.clone()?;
        Some(TdlibParams {
            api_id,
            api_hash,
            database_directory: self.td_boot.database_directory.clone(),
            database_encryption_key: self.td_boot.database_encryption_key.clone(),
            use_message_database: true,
            use_chat_info_database: true,
            use_file_database: true,
            use_secret_chats: false,
            system_language_code: SYSTEM_LANGUAGE_CODE.to_string(),
            device_model: DEVICE_MODEL.to_string(),
            application_version: env!("CARGO_PKG_VERSION").to_string(),
        })
    }

    async fn execute_td(&self, request: TdRequest) {
        let completion = completion_for(&request);
        let kind = request.kind();
        let outcome = self.runtime.request(request).await;

        if let Err(err) = &outcome {
            // The dispatcher never handles errors beyond logging: the state
            // machines decide what an error means (architecture §3).
            tracing::debug!(request = kind, error = %err, "td request failed");
        }

        let Some(result) = map_completion(completion, outcome) else {
            tracing::debug!(request = kind, "td response has no completion action yet");
            return;
        };
        let _ = self.action_tx.send(Action::TdResult(result)).await;
    }

    async fn save_config(&self, patch: ConfigPatch) {
        let config = Arc::clone(&self.config);
        let to_apply = patch.clone();
        // Config writes are blocking file I/O (write + rename): off the
        // async worker, like every other blocking call in this crate.
        let saved = tokio::task::spawn_blocking(move || {
            let mut config = config.lock().unwrap_or_else(|p| p.into_inner());
            config.apply_patch(&to_apply);
            config.save()
        })
        .await;

        let outcome = match saved {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => {
                tracing::warn!(error = %err, "could not save the config");
                Err(IoErrorKind::Other)
            }
            Err(err) => {
                tracing::warn!(error = %err, "the config save task did not finish");
                Err(IoErrorKind::Other)
            }
        };
        let saved_ok = outcome.is_ok();
        let _ = self
            .action_tx
            .send(Action::Io(IoResult::ConfigSaved { outcome }))
            .await;

        // First run: TDLib asked for its parameters before the wizard had
        // produced any credentials. Now it has (see module docs).
        if saved_ok
            && matches!(patch, ConfigPatch::Credentials { .. })
            && self.params_pending.swap(false, Ordering::SeqCst)
        {
            self.send_tdlib_parameters().await;
        }
    }
}

/// Which `TdResult` a request's response becomes. The mapping is by request
/// kind alone — the domain context rides in the variant, so there are no
/// correlation tokens to keep (architecture §8). What the domain cannot
/// recover from the response alone travels in the variant instead:
/// `getChatHistory` answers with a bare message list, so the chat it belongs
/// to and the `only_local` flag it was asked with are copied out of the
/// request here. The paging machine needs both (spec §5.2: an empty *local*
/// response means something different from an empty remote one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Completion {
    Auth,
    Chats,
    History {
        chat_id: ChatId,
        only_local: bool,
    },
    LogOut,
    /// Executed for its effect inside TDLib; whatever state it changes comes
    /// back as a push update, so there is no completion action to send.
    FireAndForget,
    /// Owned by a milestone that has not landed yet.
    Unwired,
}

fn completion_for(request: &TdRequest) -> Completion {
    match request {
        TdRequest::SetTdlibParameters(_)
        | TdRequest::SetAuthenticationPhoneNumber { .. }
        | TdRequest::CheckAuthenticationCode { .. }
        | TdRequest::CheckAuthenticationPassword { .. }
        | TdRequest::RequestQrCodeAuthentication => Completion::Auth,
        TdRequest::LoadChats { .. } => Completion::Chats,
        TdRequest::GetChatHistory {
            chat_id,
            only_local,
            ..
        } => Completion::History {
            chat_id: *chat_id,
            only_local: *only_local,
        },
        // `openChat`/`closeChat` move TDLib's per-chat update subscription;
        // `viewMessages` marks messages read. All three are answered with a
        // bare `Ok` and their consequences arrive as updates.
        TdRequest::OpenChat { .. }
        | TdRequest::CloseChat { .. }
        | TdRequest::ViewMessages { .. } => Completion::FireAndForget,
        TdRequest::LogOut => Completion::LogOut,
        // Send, edit, delete, forward, reactions, downloads and search
        // complete into their own variants from M4 on.
        _ => Completion::Unwired,
    }
}

fn map_completion(
    completion: Completion,
    outcome: Result<TdResponse, TdError>,
) -> Option<TdResult> {
    match completion {
        Completion::Auth => Some(TdResult::AuthRequestDone {
            outcome: outcome.map(|_| ()),
        }),
        Completion::Chats => Some(TdResult::ChatsLoaded {
            outcome: outcome.map(|_| ()),
        }),
        Completion::LogOut => Some(TdResult::LogOutDone {
            outcome: outcome.map(|_| ()),
        }),
        Completion::History {
            chat_id,
            only_local,
        } => Some(TdResult::HistoryLoaded {
            chat_id,
            only_local,
            outcome: outcome.map(messages_of),
        }),
        Completion::FireAndForget | Completion::Unwired => None,
    }
}

/// A `getChatHistory` that answers with anything other than a message list is
/// reported as an empty page rather than dropped: empty is a value the paging
/// machine has a rule for (retry — it is never proof of end-of-history), and
/// swallowing the completion instead would strand `PagingState::Loading` with
/// nothing to move it along.
fn messages_of(response: TdResponse) -> Vec<MessageView> {
    match response {
        TdResponse::Messages { messages } => messages,
        other => {
            tracing::debug!(
                response = ?std::mem::discriminant(&other),
                "getChatHistory answered with a non-Messages response; treated as an empty page"
            );
            Vec::new()
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
    use tgt_core::model::entity::FormattedText;
    use tgt_core::model::ids::{MessageId, UserId};
    use tgt_core::model::message::{MessageCaps, MessageContent, SendState, Sender};
    use tgt_core::td::fake::{FakeTd, RequestMatcher, RespondWith, ScriptStep};

    fn message(chat_id: ChatId, id: i64) -> MessageView {
        MessageView {
            id: MessageId(id),
            chat_id,
            sender: Sender::User(UserId(1)),
            sender_name: "Ada".to_string(),
            is_outgoing: false,
            date: 1_700_000_000 + id,
            content: MessageContent::Text(FormattedText {
                text: format!("message {id}"),
                entities: Vec::new(),
            }),
            reply_to: None,
            send_state: SendState::Sent,
            reactions: Vec::new(),
            caps: MessageCaps::default(),
            is_edited: false,
        }
    }

    fn fake_runtime(fixture: &str) -> Arc<FakeTd> {
        Arc::new(FakeTd::from_jsonl(fixture).expect("fixture parses"))
    }

    fn dispatcher_with(
        runtime: Arc<dyn TdRuntime>,
        config: Config,
    ) -> (Dispatcher, mpsc::Receiver<Action>, watch::Receiver<bool>) {
        let (action_tx, action_rx) = mpsc::channel(8);
        let (dispatcher, quit_rx) = Dispatcher::new(
            action_tx,
            runtime,
            Arc::new(Mutex::new(config)),
            TdBootParams {
                database_directory: PathBuf::from("/tmp/tgt-test-db"),
                database_encryption_key: vec![0u8; 32],
            },
        );
        (dispatcher, action_rx, quit_rx)
    }

    fn config_with_credentials() -> Config {
        Config {
            api_id: Some(1234),
            api_hash: Some("hash".to_string()),
            ..Config::default()
        }
    }

    #[tokio::test]
    async fn quit_effect_flips_the_quit_signal() {
        let (dispatcher, _action_rx, mut quit_rx) =
            dispatcher_with(fake_runtime(""), Config::default());

        assert!(!*quit_rx.borrow());
        dispatcher.dispatch(Effect::Quit);
        quit_rx.changed().await.unwrap();
        assert!(*quit_rx.borrow());
    }

    #[tokio::test]
    async fn auth_request_completes_as_auth_request_done() {
        let fake = fake_runtime("");
        let (dispatcher, mut action_rx, _quit_rx) =
            dispatcher_with(Arc::clone(&fake) as Arc<dyn TdRuntime>, Config::default());

        dispatcher.dispatch(Effect::Td(TdRequest::CheckAuthenticationCode {
            code: "12345".to_string(),
        }));

        let action = action_rx.recv().await.expect("completion action");
        assert!(matches!(
            action,
            Action::TdResult(TdResult::AuthRequestDone { outcome: Ok(()) })
        ));
        assert_eq!(fake.received().len(), 1);
    }

    #[tokio::test]
    async fn load_chats_completes_as_chats_loaded() {
        let (dispatcher, mut action_rx, _quit_rx) =
            dispatcher_with(fake_runtime(""), Config::default());

        dispatcher.dispatch(Effect::Td(TdRequest::LoadChats {
            list: ChatListId::Main,
            limit: 200,
        }));

        assert!(matches!(
            action_rx.recv().await.expect("completion action"),
            Action::TdResult(TdResult::ChatsLoaded { outcome: Ok(()) })
        ));
    }

    #[tokio::test]
    async fn get_chat_history_completes_with_the_request_context_attached() {
        let messages = vec![message(ChatId(9), 100), message(ChatId(9), 101)];
        let script = ScriptStep::Await {
            expect: RequestMatcher::Kind("GetChatHistory".to_string()),
            respond: RespondWith::Ok(TdResponse::Messages {
                messages: messages.clone(),
            }),
        };
        let fixture = serde_json::to_string(&script).unwrap();
        let (dispatcher, mut action_rx, _quit_rx) =
            dispatcher_with(fake_runtime(&fixture), Config::default());

        dispatcher.dispatch(Effect::Td(TdRequest::GetChatHistory {
            chat_id: ChatId(9),
            from_message_id: tgt_core::model::ids::MessageId(0),
            limit: 50,
            only_local: false,
        }));

        let action = action_rx.recv().await.expect("completion action");
        // The response is a bare message list: which chat it belongs to and
        // which flag it was asked with can only come from the request.
        let Action::TdResult(TdResult::HistoryLoaded {
            chat_id,
            only_local,
            outcome,
        }) = action
        else {
            panic!("expected HistoryLoaded, got {action:?}");
        };
        assert_eq!(chat_id, ChatId(9));
        assert!(!only_local);
        assert_eq!(outcome.unwrap(), messages);
    }

    #[tokio::test]
    async fn history_response_without_messages_is_an_empty_page_not_a_dropped_completion() {
        // `FakeTd` answers an unscripted request with a bare `Ok`. The paging
        // machine must still hear about it (spec §5.2), as an empty page.
        let (dispatcher, mut action_rx, _quit_rx) =
            dispatcher_with(fake_runtime(""), Config::default());

        dispatcher.dispatch(Effect::Td(TdRequest::GetChatHistory {
            chat_id: ChatId(9),
            from_message_id: tgt_core::model::ids::MessageId(0),
            limit: 50,
            only_local: true,
        }));

        let action = action_rx.recv().await.expect("completion action");
        assert!(matches!(
            action,
            Action::TdResult(TdResult::HistoryLoaded {
                chat_id: ChatId(9),
                only_local: true,
                outcome: Ok(ref msgs),
            }) if msgs.is_empty()
        ));
    }

    #[tokio::test]
    async fn view_messages_reaches_tdlib_without_a_completion_action() {
        let fake = fake_runtime("");
        let (dispatcher, mut action_rx, _quit_rx) =
            dispatcher_with(Arc::clone(&fake) as Arc<dyn TdRuntime>, Config::default());

        dispatcher.dispatch(Effect::Td(TdRequest::ViewMessages {
            chat_id: ChatId(9),
            message_ids: vec![tgt_core::model::ids::MessageId(1)],
        }));

        for _ in 0..50 {
            if !fake.received().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        assert_eq!(fake.received().len(), 1);
        assert!(action_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn unwired_request_reports_nothing_but_still_reaches_the_runtime() {
        let fake = fake_runtime("");
        let (dispatcher, mut action_rx, _quit_rx) =
            dispatcher_with(Arc::clone(&fake) as Arc<dyn TdRuntime>, Config::default());

        dispatcher.dispatch(Effect::Td(TdRequest::OpenChat {
            chat_id: tgt_core::model::ids::ChatId(7),
        }));

        // The request is executed; there is simply no completion action for
        // it until the milestone that consumes one lands.
        tokio::task::yield_now().await;
        for _ in 0..50 {
            if !fake.received().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        assert_eq!(fake.received().len(), 1);
        assert!(action_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn tdlib_parameters_are_built_from_config_and_boot_facts() {
        let fake = fake_runtime("");
        let (dispatcher, mut action_rx, _quit_rx) = dispatcher_with(
            Arc::clone(&fake) as Arc<dyn TdRuntime>,
            config_with_credentials(),
        );

        dispatcher.request_tdlib_parameters();

        assert!(matches!(
            action_rx.recv().await.expect("completion action"),
            Action::TdResult(TdResult::AuthRequestDone { outcome: Ok(()) })
        ));
        let received = fake.received();
        let TdRequest::SetTdlibParameters(params) = &received[0] else {
            panic!("expected SetTdlibParameters, got {:?}", received[0]);
        };
        assert_eq!(params.api_id, 1234);
        assert_eq!(params.api_hash, "hash");
        assert_eq!(params.database_encryption_key.len(), 32);
        assert!(!params.use_secret_chats);
        assert!(params.use_message_database);
        assert_eq!(params.application_version, env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn tdlib_parameters_wait_for_credentials_instead_of_failing() {
        let fake = fake_runtime("");
        let (dispatcher, _action_rx, _quit_rx) =
            dispatcher_with(Arc::clone(&fake) as Arc<dyn TdRuntime>, Config::default());

        dispatcher.request_tdlib_parameters();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        assert!(
            fake.received().is_empty(),
            "no credentials yet: nothing may be sent to TDLib"
        );
        assert!(dispatcher.inner.params_pending.load(Ordering::SeqCst));
    }
}
