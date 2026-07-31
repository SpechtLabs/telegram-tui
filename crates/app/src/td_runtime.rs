//! `TdlibRuntime`: the real [`TdRuntime`] over tdlib-rs. See
//! docs/architecture.md §4.7 and §7.
//!
//! This is the **only** module in the workspace that imports `tdlib_rs`, and
//! the place where PII-bearing raw TDLib types die: nothing but the reduced
//! `TdUpdate` / `TdResponse` / `TdError` projections crosses into `tgt_core`.
//!
//! Shape of the thing:
//!
//! * `new()` creates the client id, starts a dedicated OS thread running
//!   TDLib's blocking `receive()` C call, and — before anything else — points
//!   TDLib's own logger at a file at low verbosity, because the default log
//!   stream is stderr and the TUI owns the terminal (spec constraint: nothing
//!   reaches stdout/stderr while the TUI is active).
//! * The receive thread pre-digests raw updates into `TdUpdate` and forwards
//!   them down an mpsc channel that `updates()` hands out exactly once.
//! * `request()` maps a `TdRequest` onto the matching `tdlib_rs::functions`
//!   call. Request/response correlation via `@extra` is done **inside**
//!   tdlib-rs (`send_request` subscribes an observer keyed by a counter, and
//!   `receive()` notifies it), so there is no correlation bookkeeping here —
//!   only the requirement that the receive loop is running, which `new()`
//!   guarantees.
//!
//! Construction parameters are deliberately minimal: TDLib is configured
//! later, in-band, by a `TdRequest::SetTdlibParameters` sent once TDLib asks
//! for it via `AuthPhase::WaitTdlibParameters`.
//!
//! Nothing here is reachable from `main` until the run loop is wired to a
//! runtime (T14), so the module allows dead code rather than sprinkling
//! `#[allow]` over every item.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tdlib_rs::{enums as td_enums, functions, types as td_types};
use tokio::sync::mpsc;

use tgt_core::model::chat::{ChatKind, ChatListId, ChatPositionEntry, ChatView, MessagePreview};
use tgt_core::model::entity::{EntityKind, FormattedText, TextEntity};
use tgt_core::model::ids::{ChatId, FileId, MessageId, UserId};
use tgt_core::model::message::{
    FileSnapshot, MessageCaps, MessageContent, MessageView, ReactionView, ReplyPreview, SendState,
    Sender,
};
use tgt_core::td::error::TdError;
use tgt_core::td::request::{OutgoingFileKind, TdRequest, TdResponse, TdlibParams};
use tgt_core::td::runtime::TdRuntime;
use tgt_core::td::update::{AuthPhase, ConnectionPhase, PresenceStatus, TdUpdate};

/// Buffer between the receive thread and the run loop. TDLib bursts hard on
/// cold start (one `updateNewChat` per cached chat); a deep buffer keeps the
/// blocking receive thread from stalling on a busy consumer.
const UPDATE_CHANNEL_CAPACITY: usize = 1024;

/// TDLib's own log verbosity: 1 = errors only. Its log is a debugging aid of
/// last resort, not something the app reads.
const TD_LOG_VERBOSITY: i32 = 1;

/// Rotation bound for TDLib's log file (4 MiB).
const TD_LOG_MAX_BYTES: i64 = 4 * 1024 * 1024;

const APP_DIR: &str = "telegram-tui";
const TD_LOG_FILE: &str = "tdlib.log";

/// Reply excerpts and chat-list previews are one line, hard-capped. The cap
/// counts characters, ellipsis included, so a preview can never blow up a
/// layout regardless of what was pasted into the original message.
const EXCERPT_MAX_CHARS: usize = 80;

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

pub struct TdlibRuntime {
    client_id: i32,
    /// Handed out by `updates()`; `None` after the first call, which is what
    /// makes the second call panic as the trait documents.
    updates_rx: Mutex<Option<mpsc::Receiver<TdUpdate>>>,
    /// A clone of the sender half the receive thread also holds. `execute()`
    /// has no other way to reach the updates channel, and it needs one to
    /// seed `MediaState` from a message it just mapped (history pages, send
    /// results, edits) — see [`Self::seed_file`].
    updates_tx: mpsc::Sender<TdUpdate>,
    names: Arc<NameCache>,
    /// File ids already reported to `MediaState` this session, shared with
    /// the receive thread. See [`SeededFiles`].
    seeded_files: Arc<SeededFiles>,
    receiving: Arc<AtomicBool>,
    /// Taken by [`TdlibRuntime::shutdown`], which is the only caller that
    /// needs to *wait* for the receive thread rather than merely ask it to
    /// stop. `Drop` leaves it in place and unjoined — see both.
    receive_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl TdlibRuntime {
    /// Creates the TDLib client, starts the receive thread, and silences
    /// TDLib's own logging into a file under the XDG state directory.
    ///
    /// Async because silencing TDLib is itself a TDLib request: it has to be
    /// awaited before any other request so that no TDLib log line can reach
    /// stderr while the TUI owns the terminal.
    pub async fn new() -> Self {
        Self::with_log_path(default_td_log_path()).await
    }

    /// Same as [`TdlibRuntime::new`] but with an explicit TDLib log path.
    /// `None` discards TDLib's log entirely (`logStreamEmpty`) — still never
    /// stderr.
    pub async fn with_log_path(log_path: Option<PathBuf>) -> Self {
        let client_id = tdlib_rs::create_client();
        let (updates_tx, updates_rx) = mpsc::channel(UPDATE_CHANNEL_CAPACITY);
        let names = Arc::new(NameCache::default());
        let seeded_files = Arc::new(SeededFiles::default());
        let receiving = Arc::new(AtomicBool::new(true));

        let receive_thread = spawn_receive_thread(
            client_id,
            updates_tx.clone(),
            Arc::clone(&names),
            Arc::clone(&seeded_files),
            Arc::clone(&receiving),
        );

        // First two requests of the process, before anything can log.
        init_tdlib_logging(client_id, log_path).await;

        TdlibRuntime {
            client_id,
            updates_rx: Mutex::new(Some(updates_rx)),
            updates_tx,
            names,
            seeded_files,
            receiving,
            receive_thread: Mutex::new(Some(receive_thread)),
        }
    }

    /// Stops the receive thread **and waits for it to exit**, which is what
    /// makes replacing this client with a fresh one safe.
    ///
    /// # Why a restart cannot just drop and recreate
    ///
    /// `tdlib_rs::receive()` (lib.rs:35) reads the one global `td_receive`
    /// queue shared by every client in the process, and the loop below
    /// discards anything whose `@client_id` is not its own — there is no way
    /// to put an update back. Two receive threads therefore race for one
    /// queue and eat each other's updates. `Drop` only *asks* the thread to
    /// stop and returns immediately, leaving it alive for up to one 2 s
    /// `receive()` timeout, so a naive drop-then-create has a window in
    /// which the dying thread swallows the new client's
    /// `authorizationStateWaitTdlibParameters` — the one update the restart
    /// is waiting for.
    ///
    /// So: join first, create second. The wait is bounded by that same 2 s
    /// timeout and only happens on a deliberate restart.
    ///
    /// Responses are not at risk either way: `@extra` correlation goes
    /// through tdlib-rs's global `OBSERVER`, keyed by a counter rather than
    /// by client, so whichever thread receives a response notifies the right
    /// waiter.
    async fn shutdown_and_join(&self) {
        self.receiving.store(false, Ordering::Release);
        let handle = self
            .receive_thread
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        let Some(handle) = handle else {
            return;
        };
        // On the blocking pool: the thread is inside a 2 s C call and
        // joining it from the async worker would stall the whole runtime.
        if tokio::task::spawn_blocking(move || handle.join())
            .await
            .is_err()
        {
            tracing::warn!("the tdlib receive thread panicked while shutting down");
        }
        tracing::debug!("tdlib receive thread joined");
    }

    /// The TDLib client id this runtime owns. Exposed for diagnostics only —
    /// raw TDLib handles are not part of the `TdRuntime` contract.
    pub fn client_id(&self) -> i32 {
        self.client_id
    }

    async fn execute(&self, req: TdRequest) -> Result<TdResponse, TdError> {
        let client_id = self.client_id;
        tracing::debug!(request = req.kind(), "td request");

        match req {
            TdRequest::SetTdlibParameters(params) => {
                set_tdlib_parameters(client_id, params).await?;
                Ok(TdResponse::Ok)
            }
            TdRequest::SetAuthenticationPhoneNumber { phone } => {
                functions::set_authentication_phone_number(phone, None, client_id)
                    .await
                    .map_err(map_td_error)?;
                Ok(TdResponse::Ok)
            }
            TdRequest::CheckAuthenticationCode { code } => {
                functions::check_authentication_code(code, client_id)
                    .await
                    .map_err(map_td_error)?;
                Ok(TdResponse::Ok)
            }
            TdRequest::CheckAuthenticationPassword { password } => {
                functions::check_authentication_password(password, client_id)
                    .await
                    .map_err(map_td_error)?;
                Ok(TdResponse::Ok)
            }
            TdRequest::RequestQrCodeAuthentication => {
                functions::request_qr_code_authentication(Vec::new(), client_id)
                    .await
                    .map_err(map_td_error)?;
                Ok(TdResponse::Ok)
            }
            TdRequest::LogOut => {
                functions::log_out(client_id).await.map_err(map_td_error)?;
                Ok(TdResponse::Ok)
            }
            TdRequest::LoadChats { list, limit } => {
                functions::load_chats(Some(to_td_chat_list(list)), clamp_i32(limit), client_id)
                    .await
                    .map_err(map_td_error)?;
                Ok(TdResponse::Ok)
            }
            TdRequest::OpenChat { chat_id } => {
                functions::open_chat(chat_id.0, client_id)
                    .await
                    .map_err(map_td_error)?;
                Ok(TdResponse::Ok)
            }
            TdRequest::CloseChat { chat_id } => {
                functions::close_chat(chat_id.0, client_id)
                    .await
                    .map_err(map_td_error)?;
                Ok(TdResponse::Ok)
            }
            TdRequest::GetChatHistory {
                chat_id,
                from_message_id,
                limit,
                only_local,
            } => {
                let td_enums::Messages::Messages(messages) = functions::get_chat_history(
                    chat_id.0,
                    from_message_id.0,
                    0,
                    i32::from(limit),
                    only_local,
                    client_id,
                )
                .await
                .map_err(map_td_error)?;
                Ok(TdResponse::Messages {
                    messages: self.map_messages(messages.messages),
                })
            }
            // The flags `map_caps` cannot read off `message` (architecture
            // §7). Selection mode fires this for every message it lands on,
            // so the chip row is what TDLib will actually accept rather than
            // a guess.
            TdRequest::GetMessageProperties {
                chat_id,
                message_id,
            } => {
                let td_enums::MessageProperties::MessageProperties(props) =
                    functions::get_message_properties(chat_id.0, message_id.0, client_id)
                        .await
                        .map_err(map_td_error)?;
                Ok(TdResponse::MessageProperties(map_message_properties(
                    &props,
                )))
            }
            TdRequest::ViewMessages {
                chat_id,
                message_ids,
            } => {
                functions::view_messages(
                    chat_id.0,
                    message_ids.into_iter().map(|id| id.0).collect(),
                    None,
                    true,
                    client_id,
                )
                .await
                .map_err(map_td_error)?;
                Ok(TdResponse::Ok)
            }
            TdRequest::SendMessageText {
                chat_id,
                reply_to,
                text,
            } => {
                let content =
                    td_enums::InputMessageContent::InputMessageText(td_types::InputMessageText {
                        text: to_td_formatted_text(text),
                        link_preview_options: None,
                        clear_draft: true,
                    });
                let reply_to = reply_to.map(|id| {
                    td_enums::InputMessageReplyTo::Message(td_types::InputMessageReplyToMessage {
                        message_id: id.0,
                        quote: None,
                        checklist_task_id: 0,
                    })
                });
                let td_enums::Message::Message(message) =
                    functions::send_message(chat_id.0, None, reply_to, None, content, client_id)
                        .await
                        .map_err(map_td_error)?;
                Ok(TdResponse::Message(self.map_message(message)))
            }
            TdRequest::SendMessageFile {
                chat_id,
                path,
                kind,
                caption,
            } => {
                let content = input_file_content(path, kind, caption);
                let td_enums::Message::Message(message) =
                    functions::send_message(chat_id.0, None, None, None, content, client_id)
                        .await
                        .map_err(map_td_error)?;
                Ok(TdResponse::Message(self.map_message(message)))
            }
            TdRequest::EditMessageText {
                chat_id,
                message_id,
                text,
            } => {
                let content =
                    td_enums::InputMessageContent::InputMessageText(td_types::InputMessageText {
                        text: to_td_formatted_text(text),
                        link_preview_options: None,
                        clear_draft: false,
                    });
                let td_enums::Message::Message(message) =
                    functions::edit_message_text(chat_id.0, message_id.0, content, client_id)
                        .await
                        .map_err(map_td_error)?;
                Ok(TdResponse::Message(self.map_message(message)))
            }
            TdRequest::DeleteMessages {
                chat_id,
                message_ids,
                revoke,
            } => {
                functions::delete_messages(
                    chat_id.0,
                    message_ids.into_iter().map(|id| id.0).collect(),
                    revoke,
                    client_id,
                )
                .await
                .map_err(map_td_error)?;
                Ok(TdResponse::Ok)
            }
            TdRequest::ForwardMessages {
                to_chat_id,
                from_chat_id,
                message_ids,
            } => {
                let td_enums::Messages::Messages(messages) = functions::forward_messages(
                    to_chat_id.0,
                    None,
                    from_chat_id.0,
                    message_ids.into_iter().map(|id| id.0).collect(),
                    None,
                    false,
                    false,
                    client_id,
                )
                .await
                .map_err(map_td_error)?;
                Ok(TdResponse::Messages {
                    messages: self.map_messages(messages.messages),
                })
            }
            TdRequest::ToggleReaction {
                chat_id,
                message_id,
                emoji,
            } => {
                self.toggle_reaction(chat_id, message_id, emoji).await?;
                Ok(TdResponse::Ok)
            }
            TdRequest::DownloadFile { file_id, priority } => {
                let td_enums::File::File(file) = functions::download_file(
                    file_id.0,
                    i32::from(priority),
                    0,
                    0,
                    false,
                    client_id,
                )
                .await
                .map_err(map_td_error)?;
                // As authoritative as a live `updateFile`: never let a later
                // message payload's copy of this file re-seed over it.
                self.seeded_files.mark(FileId(file.id));
                Ok(TdResponse::File(map_file(file)))
            }
            TdRequest::CancelDownloadFile { file_id } => {
                functions::cancel_download_file(file_id.0, false, client_id)
                    .await
                    .map_err(map_td_error)?;
                Ok(TdResponse::Ok)
            }
            TdRequest::SearchChatMessages {
                chat_id,
                query,
                from_message_id,
                limit,
            } => {
                let td_enums::FoundChatMessages::FoundChatMessages(found) =
                    functions::search_chat_messages(
                        chat_id.0,
                        None,
                        query,
                        None,
                        from_message_id.0,
                        0,
                        i32::from(limit),
                        None,
                        client_id,
                    )
                    .await
                    .map_err(map_td_error)?;
                Ok(TdResponse::FoundMessages {
                    message_ids: found
                        .messages
                        .into_iter()
                        .map(|m| MessageId(m.id))
                        .collect(),
                })
            }
        }
    }

    /// TDLib has no toggle: it has `addMessageReaction` and
    /// `removeMessageReaction`. Read the current state first, then pick.
    async fn toggle_reaction(
        &self,
        chat_id: ChatId,
        message_id: MessageId,
        emoji: String,
    ) -> Result<(), TdError> {
        let td_enums::Message::Message(message) =
            functions::get_message(chat_id.0, message_id.0, self.client_id)
                .await
                .map_err(map_td_error)?;

        let chosen = message
            .interaction_info
            .as_ref()
            .and_then(|info| info.reactions.as_ref())
            .is_some_and(|reactions| {
                reactions.reactions.iter().any(|reaction| {
                    reaction.is_chosen
                        && matches!(
                            &reaction.r#type,
                            td_enums::ReactionType::Emoji(e) if e.emoji == emoji
                        )
                })
            });

        let reaction_type = td_enums::ReactionType::Emoji(td_types::ReactionTypeEmoji { emoji });
        if chosen {
            functions::remove_message_reaction(
                chat_id.0,
                message_id.0,
                reaction_type,
                self.client_id,
            )
            .await
            .map_err(map_td_error)
        } else {
            functions::add_message_reaction(
                chat_id.0,
                message_id.0,
                reaction_type,
                false,
                true,
                self.client_id,
            )
            .await
            .map_err(map_td_error)
        }
    }

    /// Maps one tdlib message and, if it carries a file, seeds `MediaState`
    /// with that file's *current* local state — see module docs at
    /// [`content_file_seed`] for why: `MessageContent` itself keeps only the
    /// file id, so without this a photo downloaded in a previous session
    /// shows "download" again until re-fetched.
    fn map_message(&self, message: td_types::Message) -> MessageView {
        let (view, seed) = map_message_with(&self.names, message);
        if let Some(seed) = seed {
            self.seed_file(seed);
        }
        view
    }

    fn map_messages(&self, messages: Vec<Option<td_types::Message>>) -> Vec<MessageView> {
        messages
            .into_iter()
            .flatten()
            .map(|m| self.map_message(m))
            .collect()
    }

    /// Forwards a synthetic `TdUpdate::File` derived from a message payload,
    /// once per file id per runtime lifetime and without blocking the
    /// caller. See [`seed_and_forward`].
    fn seed_file(&self, seed: FileSnapshot) {
        seed_and_forward(&self.updates_tx, &self.seeded_files, seed);
    }
}

#[async_trait::async_trait]
impl TdRuntime for TdlibRuntime {
    async fn request(&self, req: TdRequest) -> Result<TdResponse, TdError> {
        self.execute(req).await
    }

    async fn shutdown(&self) {
        self.shutdown_and_join().await;
    }

    fn updates(&self) -> mpsc::Receiver<TdUpdate> {
        self.updates_rx
            .lock()
            .expect("updates channel mutex poisoned")
            .take()
            .expect("updates() called twice")
    }
}

impl Drop for TdlibRuntime {
    fn drop(&mut self) {
        // The receive thread notices within one `receive()` timeout (2 s) and
        // exits; it is never joined here, because joining would stall
        // shutdown for that timeout with nothing useful to wait for. The
        // process is going away and the thread goes with it.
        //
        // A *restart* is the case where the wait is worth it, and it calls
        // [`TdlibRuntime::shutdown`] explicitly rather than relying on this.
        self.receiving.store(false, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// Receive loop
// ---------------------------------------------------------------------------

/// TDLib's `receive()` is a blocking C call with an internal 2 s timeout.
/// A dedicated OS thread (rather than a `spawn_blocking` task re-entered every
/// two seconds) keeps it off the tokio blocking pool for the process lifetime.
fn spawn_receive_thread(
    client_id: i32,
    updates_tx: mpsc::Sender<TdUpdate>,
    names: Arc<NameCache>,
    seeded_files: Arc<SeededFiles>,
    receiving: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("tdlib-receive".to_string())
        .spawn(move || {
            while receiving.load(Ordering::Acquire) {
                // `None` means "timed out" or "that was a response, already
                // routed to its `@extra` waiter by tdlib-rs".
                let Some((update, update_client_id)) = tdlib_rs::receive() else {
                    continue;
                };
                if update_client_id != client_id {
                    continue;
                }
                // Peeked before `map_update` can consume `update`: a
                // message-bearing update's seed comes from the exact same
                // payload the mapped `TdUpdate` is built from.
                let pending_seed = update_file_seed(&update);
                let Some(mapped) = map_update(&names, update) else {
                    continue;
                };
                if let TdUpdate::File(seen) = &mapped {
                    // A live `updateFile` is authoritative. Record it so a
                    // message payload carrying a (possibly stale) copy of
                    // the same file — arriving later here, or via the async
                    // `execute()` request/response path — never re-seeds
                    // over it; `MediaState::upsert_file` is a plain last-
                    // write-wins insert, so this ordering guard is the only
                    // thing standing between a fresh download and a stale
                    // one clobbering it.
                    seeded_files.mark(seen.id);
                }
                if let Some(seed) = pending_seed {
                    seed_and_forward(&updates_tx, &seeded_files, seed);
                }
                if updates_tx.blocking_send(mapped).is_err() {
                    // The run loop dropped the receiver: nobody is listening.
                    break;
                }
            }
            tracing::debug!("tdlib receive thread stopped");
        })
        .expect("failed to spawn the tdlib receive thread")
}

async fn init_tdlib_logging(client_id: i32, log_path: Option<PathBuf>) {
    let stream = match log_path {
        Some(path) => {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            td_enums::LogStream::File(td_types::LogStreamFile {
                path: path.to_string_lossy().into_owned(),
                max_file_size: TD_LOG_MAX_BYTES,
                // Never true: redirecting the process's stderr would swallow
                // the panic hook's report, which is the one thing that must
                // still reach the shell.
                redirect_stderr: false,
            })
        }
        None => td_enums::LogStream::Empty,
    };

    // Stream first, then verbosity, so the verbosity change itself cannot be
    // logged to the default (stderr) stream.
    if let Err(err) = functions::set_log_stream(stream, client_id).await {
        tracing::warn!(code = err.code, "could not redirect the TDLib log");
    }
    if let Err(err) = functions::set_log_verbosity_level(TD_LOG_VERBOSITY, client_id).await {
        tracing::warn!(code = err.code, "could not lower TDLib log verbosity");
    }
}

/// `$XDG_STATE_HOME/telegram-tui/tdlib.log`, alongside the app's own log —
/// which means falling back to the cache directory where there is no state
/// directory, exactly as `logging::state_dir` does and for the same reason.
/// The two have to agree: "alongside" is the whole point.
fn default_td_log_path() -> Option<PathBuf> {
    use etcetera::BaseStrategy;
    let strategy = etcetera::choose_base_strategy().ok()?;
    let base = strategy.state_dir().unwrap_or_else(|| strategy.cache_dir());
    Some(base.join(APP_DIR).join(TD_LOG_FILE))
}

// ---------------------------------------------------------------------------
// Name cache
// ---------------------------------------------------------------------------

/// TDLib messages carry a *sender id*, never a sender name; the name lives on
/// the `user`/`chat` objects TDLib guarantees to have sent before anything
/// references them. This cache is that guarantee, materialized — it is fed by
/// the `updateUser` / `updateNewChat` / `updateChatTitle` updates the receive
/// loop sees anyway, and is the reason `MessageView::sender_name` can be
/// filled at all.
///
/// It is the only place raw display names are retained; they leave this module
/// exclusively through `MessageView`/`ChatView`, which is by contract.
#[derive(Default)]
struct NameCache {
    inner: Mutex<NameMaps>,
}

#[derive(Default)]
struct NameMaps {
    users: HashMap<i64, String>,
    chats: HashMap<i64, String>,
}

impl NameCache {
    fn put_user(&self, id: i64, name: String) {
        self.lock().users.insert(id, name);
    }

    fn put_chat(&self, id: i64, title: String) {
        self.lock().chats.insert(id, title);
    }

    fn resolve(&self, sender: Sender) -> String {
        let maps = self.lock();
        match sender {
            Sender::User(id) => maps.users.get(&id.0).cloned(),
            Sender::Chat(id) => maps.chats.get(&id.0).cloned(),
        }
        .unwrap_or_default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, NameMaps> {
        self.inner.lock().unwrap_or_else(|poisoned| {
            // A poisoned name cache costs display names, not correctness.
            poisoned.into_inner()
        })
    }
}

// ---------------------------------------------------------------------------
// File seed tracking
// ---------------------------------------------------------------------------

/// File ids the runtime has already told `MediaState` about this session,
/// via either a live `updateFile` push or a synthetic seed derived from a
/// message payload (see [`content_file_seed`]). Shared between the receive
/// thread and `execute()`'s request/response path so both funnel through the
/// same once-per-id gate.
///
/// `MediaState::upsert_file` (`state/media.rs`) is a plain insert — last
/// write wins, no notion of "more informed" — so this is the only thing
/// standing between a live `updateFile` and a later, possibly-stale, message
/// payload clobbering it. A message's embedded file object can only ever be
/// as fresh as the moment TDLib built that message, never fresher than a
/// push that has already gone out; once a file id is marked, no message
/// payload gets to seed it again.
#[derive(Default)]
struct SeededFiles {
    inner: Mutex<HashSet<FileId>>,
}

impl SeededFiles {
    /// Marks `id` as known. Returns whether this is the first time — i.e.
    /// whether the caller should actually forward the seed it has in hand.
    fn mark(&self, id: FileId) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id)
    }
}

/// Pushes a synthetic `TdUpdate::File`, gated by [`SeededFiles`] so the same
/// id is never forwarded twice (this is also what makes a 50-message page
/// referencing one file push exactly one seed, not fifty).
///
/// Uses `try_send`, never `blocking_send`: a seed is a nice-to-have — the
/// message still renders, just with a "download" affordance for one extra
/// frame — so it must never stall the receive thread or an `execute()`
/// request/response task waiting on a full updates channel. A dropped seed
/// is tolerated on purpose; the next live `updateFile` corrects it.
fn seed_and_forward(tx: &mpsc::Sender<TdUpdate>, seeded: &SeededFiles, seed: FileSnapshot) {
    if !seeded.mark(seed.id) {
        return;
    }
    if tx.try_send(TdUpdate::File(seed)).is_err() {
        tracing::trace!("dropped a file seed: updates channel is full or closed");
    }
}

fn user_display_name(user: &td_types::User) -> String {
    let full = format!("{} {}", user.first_name, user.last_name);
    let full = full.trim();
    if !full.is_empty() {
        return full.to_string();
    }
    user.usernames
        .as_ref()
        .and_then(|u| u.active_usernames.first())
        .map(|u| format!("@{u}"))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Update mapping
// ---------------------------------------------------------------------------

/// The file, if any, that a raw update's message payload would seed into
/// `MediaState` — computed on a reference so the caller can peek it before
/// `map_update` consumes `update` to build the primary `TdUpdate`. Only the
/// three update kinds that carry a full message (as opposed to just an id or
/// a delta) have one; `updateFile` itself is already the authoritative
/// source and does not need seeding from itself.
fn update_file_seed(update: &td_enums::Update) -> Option<FileSnapshot> {
    match update {
        td_enums::Update::NewMessage(u) => content_file_seed(&u.message.content),
        td_enums::Update::MessageSendSucceeded(u) => content_file_seed(&u.message.content),
        td_enums::Update::MessageContent(u) => content_file_seed(&u.new_content),
        _ => None,
    }
}

/// Raw TDLib update → the pre-digested projection core consumes. `None` means
/// "irrelevant to this client": dropped here so that neither the action
/// channel nor `update()` ever sees an update it has no arm for.
fn map_update(names: &NameCache, update: td_enums::Update) -> Option<TdUpdate> {
    match update {
        td_enums::Update::AuthorizationState(u) => {
            Some(TdUpdate::Auth(map_auth_state(u.authorization_state)))
        }
        td_enums::Update::ConnectionState(u) => {
            Some(TdUpdate::Connection(map_connection_state(&u.state)))
        }

        // Cache-only updates: they carry the names every later projection
        // needs, but have no `TdUpdate` of their own.
        td_enums::Update::User(u) => {
            names.put_user(u.user.id, user_display_name(&u.user));
            None
        }

        td_enums::Update::NewChat(u) => {
            names.put_chat(u.chat.id, u.chat.title.clone());
            Some(TdUpdate::NewChat(map_chat(names, u.chat)))
        }
        td_enums::Update::ChatTitle(u) => {
            names.put_chat(u.chat_id, u.title.clone());
            Some(TdUpdate::ChatTitle {
                chat_id: ChatId(u.chat_id),
                title: u.title,
            })
        }
        td_enums::Update::ChatPosition(u) => Some(TdUpdate::ChatPosition {
            chat_id: ChatId(u.chat_id),
            position: map_chat_position(&u.position),
        }),
        td_enums::Update::ChatLastMessage(u) => Some(TdUpdate::ChatLastMessage {
            chat_id: ChatId(u.chat_id),
            preview: u
                .last_message
                .as_ref()
                .map(|m| map_message_preview(names, m)),
            positions: u.positions.iter().map(map_chat_position).collect(),
        }),
        td_enums::Update::ChatReadInbox(u) => Some(TdUpdate::ChatReadInbox {
            chat_id: ChatId(u.chat_id),
            last_read_inbox_message_id: MessageId(u.last_read_inbox_message_id),
            unread_count: non_negative(u.unread_count),
        }),
        td_enums::Update::ChatReadOutbox(u) => Some(TdUpdate::ChatReadOutbox {
            chat_id: ChatId(u.chat_id),
            last_read_outbox_message_id: MessageId(u.last_read_outbox_message_id),
        }),
        td_enums::Update::ChatUnreadMentionCount(u) => Some(TdUpdate::ChatUnreadMentionCount {
            chat_id: ChatId(u.chat_id),
            count: non_negative(u.unread_mention_count),
        }),
        td_enums::Update::ChatNotificationSettings(u) => Some(TdUpdate::ChatNotificationSettings {
            chat_id: ChatId(u.chat_id),
            muted: is_muted(&u.notification_settings),
        }),

        td_enums::Update::NewMessage(u) => {
            Some(TdUpdate::NewMessage(map_message_with(names, u.message).0))
        }
        td_enums::Update::MessageSendSucceeded(u) => Some(TdUpdate::MessageSendSucceeded {
            chat_id: ChatId(u.message.chat_id),
            old_message_id: MessageId(u.old_message_id),
            message: map_message_with(names, u.message).0,
        }),
        td_enums::Update::MessageSendFailed(u) => Some(TdUpdate::MessageSendFailed {
            chat_id: ChatId(u.message.chat_id),
            old_message_id: MessageId(u.old_message_id),
            error: map_error_parts(u.error.code, &u.error.message),
        }),
        td_enums::Update::MessageContent(u) => Some(TdUpdate::MessageContentChanged {
            chat_id: ChatId(u.chat_id),
            message_id: MessageId(u.message_id),
            content: map_content(u.new_content),
        }),
        td_enums::Update::MessageInteractionInfo(u) => Some(TdUpdate::MessageInteractionInfo {
            chat_id: ChatId(u.chat_id),
            message_id: MessageId(u.message_id),
            reactions: u
                .interaction_info
                .as_ref()
                .map(map_reactions)
                .unwrap_or_default(),
        }),
        td_enums::Update::DeleteMessages(u) => {
            // `from_cache` deletions are a storage detail, not a deletion the
            // user performed or should see.
            if u.from_cache {
                return None;
            }
            Some(TdUpdate::MessagesDeleted {
                chat_id: ChatId(u.chat_id),
                message_ids: u.message_ids.into_iter().map(MessageId).collect(),
            })
        }

        td_enums::Update::File(u) => Some(TdUpdate::File(map_file(u.file))),
        td_enums::Update::UserStatus(u) => Some(TdUpdate::UserStatus {
            user_id: UserId(u.user_id),
            status: map_presence(&u.status),
        }),
        td_enums::Update::ChatAction(u) => {
            // The contract carries a `UserId`; chat-authored actions (channel
            // admins posting anonymously) have no user to attribute and are
            // dropped rather than misattributed to a chat id.
            let td_enums::MessageSender::User(sender) = u.sender_id else {
                return None;
            };
            Some(TdUpdate::ChatAction {
                chat_id: ChatId(u.chat_id),
                user_id: UserId(sender.user_id),
                is_typing: !matches!(u.action, td_enums::ChatAction::Cancel),
            })
        }

        other => {
            if tracing::enabled!(tracing::Level::TRACE) {
                tracing::trace!(update = td_type_name(&other).as_deref(), "update dropped");
            }
            None
        }
    }
}

fn map_auth_state(state: td_enums::AuthorizationState) -> AuthPhase {
    match state {
        td_enums::AuthorizationState::WaitTdlibParameters => AuthPhase::WaitTdlibParameters,
        td_enums::AuthorizationState::WaitPhoneNumber => AuthPhase::WaitPhoneNumber,
        td_enums::AuthorizationState::WaitCode(s) => {
            let (delivery_hint, length) = code_delivery(&s.code_info);
            AuthPhase::WaitCode {
                delivery_hint,
                length,
            }
        }
        td_enums::AuthorizationState::WaitOtherDeviceConfirmation(s) => {
            AuthPhase::WaitOtherDeviceConfirmation { link: s.link }
        }
        td_enums::AuthorizationState::WaitPassword(s) => AuthPhase::WaitPassword {
            hint: (!s.password_hint.is_empty()).then_some(s.password_hint),
        },
        td_enums::AuthorizationState::Ready => AuthPhase::Ready,
        td_enums::AuthorizationState::LoggingOut => AuthPhase::LoggingOut,
        td_enums::AuthorizationState::Closing => AuthPhase::Closing,
        td_enums::AuthorizationState::Closed => AuthPhase::Closed,
        // Registration, email login and premium-purchase gating are v1
        // non-goals; surfaced by name rather than swallowed, so the auth
        // screen can dead-end honestly instead of hanging.
        other => AuthPhase::Unsupported {
            name: td_type_name(&other).unwrap_or_else(|| "authorizationState".to_string()),
        },
    }
}

/// A human hint for where the login code went. The phone number TDLib echoes
/// back is masked: it is the user's own number and is shown on screen, but
/// there is no reason to render it in full.
fn code_delivery(info: &td_types::AuthenticationCodeInfo) -> (String, u8) {
    let phone = mask_phone(&info.phone_number);
    match &info.r#type {
        td_enums::AuthenticationCodeType::TelegramMessage(t) => (
            "Telegram message on another device".to_string(),
            clamp_u8(t.length),
        ),
        td_enums::AuthenticationCodeType::Sms(t) => (format!("SMS to {phone}"), clamp_u8(t.length)),
        td_enums::AuthenticationCodeType::SmsWord(_) => (format!("SMS word to {phone}"), 0),
        td_enums::AuthenticationCodeType::SmsPhrase(_) => (format!("SMS phrase to {phone}"), 0),
        td_enums::AuthenticationCodeType::Call(t) => {
            (format!("Phone call to {phone}"), clamp_u8(t.length))
        }
        td_enums::AuthenticationCodeType::FlashCall(_) => (format!("Flash call to {phone}"), 0),
        td_enums::AuthenticationCodeType::MissedCall(t) => {
            (format!("Missed call to {phone}"), clamp_u8(t.length))
        }
        td_enums::AuthenticationCodeType::Fragment(t) => {
            ("Fragment".to_string(), clamp_u8(t.length))
        }
        td_enums::AuthenticationCodeType::FirebaseAndroid(t) => {
            ("Firebase".to_string(), clamp_u8(t.length))
        }
        td_enums::AuthenticationCodeType::FirebaseIos(t) => {
            ("Firebase".to_string(), clamp_u8(t.length))
        }
    }
}

fn map_connection_state(state: &td_enums::ConnectionState) -> ConnectionPhase {
    match state {
        td_enums::ConnectionState::WaitingForNetwork => ConnectionPhase::WaitingForNetwork,
        td_enums::ConnectionState::ConnectingToProxy => ConnectionPhase::ConnectingToProxy,
        td_enums::ConnectionState::Connecting => ConnectionPhase::Connecting,
        td_enums::ConnectionState::Updating => ConnectionPhase::Updating,
        td_enums::ConnectionState::Ready => ConnectionPhase::Ready,
    }
}

fn map_presence(status: &td_enums::UserStatus) -> PresenceStatus {
    match status {
        td_enums::UserStatus::Online(_) => PresenceStatus::Online,
        td_enums::UserStatus::Recently(_) => PresenceStatus::Recently,
        // "last week"/"last month"/"never" are all just "not around".
        _ => PresenceStatus::Offline,
    }
}

// ---------------------------------------------------------------------------
// Chat mapping
// ---------------------------------------------------------------------------

fn map_chat(names: &NameCache, chat: td_types::Chat) -> ChatView {
    ChatView {
        id: ChatId(chat.id),
        kind: map_chat_kind(&chat.r#type),
        title: chat.title,
        positions: chat.positions.iter().map(map_chat_position).collect(),
        unread_count: non_negative(chat.unread_count),
        unread_mention_count: non_negative(chat.unread_mention_count),
        last_message: chat
            .last_message
            .as_ref()
            .map(|m| map_message_preview(names, m)),
        is_muted: is_muted(&chat.notification_settings),
    }
}

fn map_chat_kind(kind: &td_enums::ChatType) -> ChatKind {
    match kind {
        td_enums::ChatType::Private(_) => ChatKind::Private,
        td_enums::ChatType::BasicGroup(_) => ChatKind::Group,
        td_enums::ChatType::Supergroup(t) if t.is_channel => ChatKind::Channel,
        td_enums::ChatType::Supergroup(_) => ChatKind::Supergroup,
        // Secret chats are disabled at `setTdlibParameters`, so this arm is
        // unreachable in practice; a private chat is the honest fallback.
        td_enums::ChatType::Secret(_) => ChatKind::Private,
    }
}

fn map_chat_position(position: &td_types::ChatPosition) -> ChatPositionEntry {
    ChatPositionEntry {
        list: map_chat_list(&position.list),
        order: position.order,
        is_pinned: position.is_pinned,
    }
}

fn map_chat_list(list: &td_enums::ChatList) -> ChatListId {
    match list {
        td_enums::ChatList::Main => ChatListId::Main,
        td_enums::ChatList::Archive => ChatListId::Archive,
        td_enums::ChatList::Folder(f) => ChatListId::Folder(f.chat_folder_id),
    }
}

fn to_td_chat_list(list: ChatListId) -> td_enums::ChatList {
    match list {
        ChatListId::Main => td_enums::ChatList::Main,
        ChatListId::Archive => td_enums::ChatList::Archive,
        ChatListId::Folder(id) => {
            td_enums::ChatList::Folder(td_types::ChatListFolder { chat_folder_id: id })
        }
    }
}

/// `use_default_mute_for` means "ask the scope settings", which would need a
/// second round trip; treated as unmuted until the scope settings are wired.
fn is_muted(settings: &td_types::ChatNotificationSettings) -> bool {
    !settings.use_default_mute_for && settings.mute_for > 0
}

// ---------------------------------------------------------------------------
// Message mapping
// ---------------------------------------------------------------------------

/// Maps a tdlib message, and alongside it the seed snapshot for whatever
/// file its content carries (`None` for fileless content). The seed is
/// derived from `message.content` *before* `map_content` consumes it, since
/// `MessageContent` itself only keeps the file id — see
/// [`content_file_seed`] for why that seed matters.
fn map_message_with(
    names: &NameCache,
    message: td_types::Message,
) -> (MessageView, Option<FileSnapshot>) {
    let sender = map_sender(&message.sender_id);
    // Channel posts and anonymous admins sign with a free-text signature that
    // is more informative than the chat title.
    let sender_name = if message.author_signature.is_empty() {
        names.resolve(sender)
    } else {
        message.author_signature.clone()
    };
    let file_seed = content_file_seed(&message.content);

    let view = MessageView {
        id: MessageId(message.id),
        chat_id: ChatId(message.chat_id),
        sender,
        sender_name,
        is_outgoing: message.is_outgoing,
        date: i64::from(message.date),
        reply_to: message.reply_to.as_ref().and_then(|r| map_reply(names, r)),
        send_state: map_send_state(message.sending_state.as_ref()),
        reactions: message
            .interaction_info
            .as_ref()
            .map(map_reactions)
            .unwrap_or_default(),
        caps: map_caps(&message),
        is_edited: message.edit_date != 0,
        content: map_content(message.content),
    };
    (view, file_seed)
}

fn map_sender(sender: &td_enums::MessageSender) -> Sender {
    match sender {
        td_enums::MessageSender::User(u) => Sender::User(UserId(u.user_id)),
        td_enums::MessageSender::Chat(c) => Sender::Chat(ChatId(c.chat_id)),
    }
}

fn map_send_state(state: Option<&td_enums::MessageSendingState>) -> SendState {
    match state {
        None => SendState::Sent,
        Some(td_enums::MessageSendingState::Pending(_)) => SendState::Sending,
        Some(td_enums::MessageSendingState::Failed(f)) => {
            SendState::Failed(map_error_parts(f.error.code, &f.error.message))
        }
    }
}

/// TDLib 1.8.5x moved the per-message capability flags off `message` and onto
/// `messageProperties`, fetched per message with `getMessageProperties`. Only
/// `can_be_saved` still rides along on the message itself.
///
/// These are therefore the *pessimistic* caps a message carries until
/// selection mode asks for the real ones (`TdRequest::GetMessageProperties`
/// → [`map_message_properties`]). Defaulting to `false` keeps the UI honest —
/// it under-promises rather than offering an action TDLib will refuse.
fn map_caps(message: &td_types::Message) -> MessageCaps {
    MessageCaps {
        can_be_edited: false,
        can_be_deleted_for_all_users: false,
        can_be_deleted_only_for_self: false,
        can_be_forwarded: false,
        can_be_saved: message.can_be_saved,
    }
}

/// The real caps, from `getMessageProperties`. TDLib's `messageProperties`
/// carries three dozen flags; only the five the chip row is derived from
/// (`model/chips.rs`) cross into `tgt_core`.
fn map_message_properties(props: &td_types::MessageProperties) -> MessageCaps {
    MessageCaps {
        can_be_edited: props.can_be_edited,
        can_be_deleted_for_all_users: props.can_be_deleted_for_all_users,
        can_be_deleted_only_for_self: props.can_be_deleted_only_for_self,
        can_be_forwarded: props.can_be_forwarded,
        can_be_saved: props.can_be_saved,
    }
}

fn map_reactions(info: &td_types::MessageInteractionInfo) -> Vec<ReactionView> {
    let Some(reactions) = info.reactions.as_ref() else {
        return Vec::new();
    };
    reactions
        .reactions
        .iter()
        .filter_map(|reaction| match &reaction.r#type {
            td_enums::ReactionType::Emoji(e) => Some(ReactionView {
                emoji: e.emoji.clone(),
                count: non_negative(reaction.total_count),
                chosen_by_me: reaction.is_chosen,
            }),
            // Custom and paid reactions have no emoji to render in a terminal.
            _ => None,
        })
        .collect()
}

/// TDLib's `messageReplyToMessage` carries the replied *id*, plus a quote and
/// origin only when the reply crosses chats. For the common same-chat reply
/// the excerpt and sender have to come from the app's own message store, so
/// they are left empty here rather than costing a round trip per message.
fn map_reply(names: &NameCache, reply: &td_enums::MessageReplyTo) -> Option<ReplyPreview> {
    let td_enums::MessageReplyTo::Message(reply) = reply else {
        // Story replies have no message to jump to.
        return None;
    };

    let excerpt = reply
        .quote
        .as_ref()
        .map(|quote| one_line_excerpt(&quote.text.text))
        .or_else(|| {
            reply
                .content
                .as_ref()
                .map(|content| one_line_excerpt(&content_preview_text(content)))
        })
        .unwrap_or_default();

    let sender_name = match reply.origin.as_ref() {
        Some(td_enums::MessageOrigin::User(o)) => {
            names.resolve(Sender::User(UserId(o.sender_user_id)))
        }
        Some(td_enums::MessageOrigin::HiddenUser(o)) => o.sender_name.clone(),
        Some(td_enums::MessageOrigin::Chat(o)) => {
            if o.author_signature.is_empty() {
                names.resolve(Sender::Chat(ChatId(o.sender_chat_id)))
            } else {
                o.author_signature.clone()
            }
        }
        Some(td_enums::MessageOrigin::Channel(o)) => {
            if o.author_signature.is_empty() {
                names.resolve(Sender::Chat(ChatId(o.chat_id)))
            } else {
                o.author_signature.clone()
            }
        }
        None => String::new(),
    };

    Some(ReplyPreview {
        message_id: MessageId(reply.message_id),
        sender_name,
        excerpt,
    })
}

fn map_message_preview(names: &NameCache, message: &td_types::Message) -> MessagePreview {
    let sender = map_sender(&message.sender_id);
    let sender_name = if message.author_signature.is_empty() {
        names.resolve(sender)
    } else {
        message.author_signature.clone()
    };
    MessagePreview {
        sender_name,
        text: one_line_excerpt(&content_preview_text(&message.content)),
        date: i64::from(message.date),
        is_outgoing: message.is_outgoing,
    }
}

// ---------------------------------------------------------------------------
// Content mapping
// ---------------------------------------------------------------------------

/// TDLib ships every photo size variant; the largest is the one worth
/// downloading for a terminal image protocol, and therefore the one whose
/// file id `map_content` surfaces as `MessageContent::Photo`'s `file_id`.
/// Shared with [`content_file`] so both agree on exactly the same size.
fn largest_photo_size(photo: &td_types::Photo) -> Option<&td_types::PhotoSize> {
    photo
        .sizes
        .iter()
        .max_by_key(|s| i64::from(s.width) * i64::from(s.height))
}

/// The raw tdlib `File` backing a message's content — specifically, the same
/// file whose id ends up in the `MessageContent` `map_content` builds from
/// the same value. `None` for content with no file (text, sticker,
/// unsupported, ...). Kept in lock-step with `map_content`'s arms on
/// purpose: this is what lets a caller learn a file's *current* local state
/// (`local.path`, `is_downloading_completed`) even though `MessageContent`
/// itself keeps only the id.
fn content_file(content: &td_enums::MessageContent) -> Option<td_types::File> {
    match content {
        td_enums::MessageContent::MessagePhoto(c) => {
            largest_photo_size(&c.photo).map(|size| size.photo.clone())
        }
        td_enums::MessageContent::MessageVideo(c) => Some(c.video.video.clone()),
        td_enums::MessageContent::MessageAnimation(c) => Some(c.animation.animation.clone()),
        td_enums::MessageContent::MessageVideoNote(c) => Some(c.video_note.video.clone()),
        td_enums::MessageContent::MessageAudio(c) => Some(c.audio.audio.clone()),
        td_enums::MessageContent::MessageVoiceNote(c) => Some(c.voice_note.voice.clone()),
        td_enums::MessageContent::MessageDocument(c) => Some(c.document.document.clone()),
        _ => None,
    }
}

/// The bug this exists to fix: `use_file_database = true` means a file
/// downloaded in a previous session is still on disk, and TDLib still says
/// so (`local.path`, `local.is_downloading_completed`) on every message
/// payload that references it — but `map_content` only keeps the file id, so
/// without this, `MediaState` starts every session knowing nothing about it
/// and the message shows "download" until re-fetched. Applying [`map_file`]
/// to [`content_file`]'s result turns that payload's file object into the
/// same `FileSnapshot` a live `updateFile` push would carry, so callers can
/// seed `MediaState` with it directly.
fn content_file_seed(content: &td_enums::MessageContent) -> Option<FileSnapshot> {
    content_file(content).map(map_file)
}

fn map_content(content: td_enums::MessageContent) -> MessageContent {
    match content {
        td_enums::MessageContent::MessageText(c) => {
            MessageContent::Text(map_formatted_text(c.text))
        }
        td_enums::MessageContent::MessagePhoto(c) => match largest_photo_size(&c.photo) {
            Some(size) => MessageContent::Photo {
                file_id: FileId(size.photo.id),
                width: non_negative(size.width),
                height: non_negative(size.height),
                caption: map_formatted_text(c.caption),
            },
            None => MessageContent::Unsupported {
                description: "Photo".to_string(),
            },
        },
        td_enums::MessageContent::MessageVideo(c) => MessageContent::Video {
            file_id: FileId(c.video.video.id),
            file_name: c.video.file_name,
            size: file_size(&c.video.video),
            duration_secs: non_negative(c.video.duration),
            caption: map_formatted_text(c.caption),
        },
        // An animation is an autoplaying muted video; the contract has no
        // separate variant and `Video` renders it correctly.
        td_enums::MessageContent::MessageAnimation(c) => MessageContent::Video {
            file_id: FileId(c.animation.animation.id),
            file_name: c.animation.file_name,
            size: file_size(&c.animation.animation),
            duration_secs: non_negative(c.animation.duration),
            caption: map_formatted_text(c.caption),
        },
        td_enums::MessageContent::MessageVideoNote(c) => MessageContent::Video {
            file_id: FileId(c.video_note.video.id),
            file_name: String::new(),
            size: file_size(&c.video_note.video),
            duration_secs: non_negative(c.video_note.duration),
            caption: FormattedText {
                text: String::new(),
                entities: Vec::new(),
            },
        },
        td_enums::MessageContent::MessageAudio(c) => MessageContent::Audio {
            file_id: FileId(c.audio.audio.id),
            file_name: audio_label(&c.audio),
            size: file_size(&c.audio.audio),
            duration_secs: non_negative(c.audio.duration),
        },
        td_enums::MessageContent::MessageVoiceNote(c) => MessageContent::Audio {
            file_id: FileId(c.voice_note.voice.id),
            file_name: String::new(),
            size: file_size(&c.voice_note.voice),
            duration_secs: non_negative(c.voice_note.duration),
        },
        td_enums::MessageContent::MessageDocument(c) => MessageContent::Document {
            file_id: FileId(c.document.document.id),
            file_name: c.document.file_name,
            size: file_size(&c.document.document),
            caption: map_formatted_text(c.caption),
        },
        td_enums::MessageContent::MessageSticker(c) => MessageContent::Sticker {
            emoji: c.sticker.emoji,
        },
        // ~80 further variants (polls, calls, gifts, service messages). Naming
        // them individually would be a maintenance tax for no gain; the
        // serde discriminant is exactly the name TDLib uses, minus the
        // redundant `message` prefix.
        other => MessageContent::Unsupported {
            description: td_type_name(&other)
                .map(|name| name.strip_prefix("message").unwrap_or(&name).to_string())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "Unsupported".to_string()),
        },
    }
}

/// A one-line, plain-text stand-in for a message: chat-list previews and reply
/// excerpts both need "what does this message say" without any markup.
fn content_preview_text(content: &td_enums::MessageContent) -> String {
    match content {
        td_enums::MessageContent::MessageText(c) => c.text.text.clone(),
        td_enums::MessageContent::MessagePhoto(c) => with_caption("Photo", &c.caption.text),
        td_enums::MessageContent::MessageVideo(c) => with_caption("Video", &c.caption.text),
        td_enums::MessageContent::MessageAnimation(c) => with_caption("GIF", &c.caption.text),
        td_enums::MessageContent::MessageDocument(c) => {
            with_caption(&c.document.file_name, &c.caption.text)
        }
        td_enums::MessageContent::MessageAudio(c) => with_caption("Audio", &c.caption.text),
        td_enums::MessageContent::MessageVoiceNote(c) => {
            with_caption("Voice message", &c.caption.text)
        }
        td_enums::MessageContent::MessageVideoNote(_) => "Video message".to_string(),
        td_enums::MessageContent::MessageSticker(c) => {
            format!("{} Sticker", c.sticker.emoji).trim().to_string()
        }
        other => td_type_name(other)
            .map(|name| name.strip_prefix("message").unwrap_or(&name).to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "Message".to_string()),
    }
}

fn with_caption(label: &str, caption: &str) -> String {
    if caption.trim().is_empty() {
        label.to_string()
    } else {
        format!("{label}: {caption}")
    }
}

fn audio_label(audio: &td_types::Audio) -> String {
    if !audio.file_name.is_empty() {
        return audio.file_name.clone();
    }
    let titled = format!("{} {}", audio.performer, audio.title);
    titled.trim().to_string()
}

fn file_size(file: &td_types::File) -> u64 {
    let size = if file.size > 0 {
        file.size
    } else {
        file.expected_size
    };
    size.max(0) as u64
}

fn map_file(file: td_types::File) -> FileSnapshot {
    let expected = if file.expected_size > 0 {
        file.expected_size
    } else {
        file.size
    };
    FileSnapshot {
        id: FileId(file.id),
        expected_size: expected.max(0) as u64,
        downloaded_size: file.local.downloaded_size.max(0) as u64,
        is_downloading: file.local.is_downloading_active,
        is_completed: file.local.is_downloading_completed,
        local_path: (!file.local.path.is_empty()).then(|| PathBuf::from(&file.local.path)),
    }
}

fn input_file_content(
    path: PathBuf,
    kind: OutgoingFileKind,
    caption: Option<FormattedText>,
) -> td_enums::InputMessageContent {
    let file = td_enums::InputFile::Local(td_types::InputFileLocal {
        path: path.to_string_lossy().into_owned(),
    });
    let caption = caption.map(to_td_formatted_text);

    match kind {
        // Dimensions and durations are left at zero: TDLib probes the file
        // during upload and fills them in, and guessing here would only ever
        // be wrong.
        OutgoingFileKind::Photo => {
            td_enums::InputMessageContent::InputMessagePhoto(td_types::InputMessagePhoto {
                photo: file,
                thumbnail: None,
                added_sticker_file_ids: Vec::new(),
                width: 0,
                height: 0,
                caption,
                show_caption_above_media: false,
                self_destruct_type: None,
                has_spoiler: false,
            })
        }
        OutgoingFileKind::Video => {
            td_enums::InputMessageContent::InputMessageVideo(td_types::InputMessageVideo {
                video: file,
                thumbnail: None,
                cover: None,
                start_timestamp: 0,
                added_sticker_file_ids: Vec::new(),
                duration: 0,
                width: 0,
                height: 0,
                supports_streaming: true,
                caption,
                show_caption_above_media: false,
                self_destruct_type: None,
                has_spoiler: false,
            })
        }
        OutgoingFileKind::Audio => {
            td_enums::InputMessageContent::InputMessageAudio(td_types::InputMessageAudio {
                audio: file,
                album_cover_thumbnail: None,
                duration: 0,
                title: String::new(),
                performer: String::new(),
                caption,
            })
        }
        OutgoingFileKind::Document => {
            td_enums::InputMessageContent::InputMessageDocument(td_types::InputMessageDocument {
                document: file,
                thumbnail: None,
                disable_content_type_detection: false,
                caption,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Rich text mapping
// ---------------------------------------------------------------------------

fn map_formatted_text(text: td_types::FormattedText) -> FormattedText {
    FormattedText {
        entities: text.entities.into_iter().filter_map(map_entity).collect(),
        text: text.text,
    }
}

/// Entity types with no counterpart in the core model (cashtags, bot commands,
/// bank card numbers, custom emoji, media timestamps, mention-by-name) are
/// dropped: the text they cover still renders, just without decoration.
fn map_entity(entity: td_types::TextEntity) -> Option<TextEntity> {
    let kind = match entity.r#type {
        td_enums::TextEntityType::Bold => EntityKind::Bold,
        td_enums::TextEntityType::Italic => EntityKind::Italic,
        td_enums::TextEntityType::Underline => EntityKind::Underline,
        td_enums::TextEntityType::Strikethrough => EntityKind::Strikethrough,
        td_enums::TextEntityType::Spoiler => EntityKind::Spoiler,
        td_enums::TextEntityType::Code => EntityKind::Code,
        td_enums::TextEntityType::Pre => EntityKind::Pre { language: None },
        td_enums::TextEntityType::PreCode(p) => EntityKind::Pre {
            language: (!p.language.is_empty()).then_some(p.language),
        },
        td_enums::TextEntityType::BlockQuote | td_enums::TextEntityType::ExpandableBlockQuote => {
            EntityKind::Blockquote
        }
        td_enums::TextEntityType::TextUrl(u) => EntityKind::TextUrl { url: u.url },
        td_enums::TextEntityType::Url => EntityKind::Url,
        td_enums::TextEntityType::Mention => EntityKind::Mention,
        td_enums::TextEntityType::Hashtag => EntityKind::Hashtag,
        _ => return None,
    };
    Some(TextEntity {
        offset_utf16: entity.offset.max(0) as u32,
        length_utf16: entity.length.max(0) as u32,
        kind,
    })
}

fn to_td_formatted_text(text: FormattedText) -> td_types::FormattedText {
    td_types::FormattedText {
        entities: text.entities.into_iter().filter_map(to_td_entity).collect(),
        text: text.text,
    }
}

/// TDLib only accepts *manually specified* entities for the styles a user can
/// actually apply. `Url`/`Mention`/`Hashtag` are detected server-side and are
/// rejected on input, so they are dropped on the way out.
fn to_td_entity(entity: TextEntity) -> Option<td_types::TextEntity> {
    let kind = match entity.kind {
        EntityKind::Bold => td_enums::TextEntityType::Bold,
        EntityKind::Italic => td_enums::TextEntityType::Italic,
        EntityKind::Underline => td_enums::TextEntityType::Underline,
        EntityKind::Strikethrough => td_enums::TextEntityType::Strikethrough,
        EntityKind::Spoiler => td_enums::TextEntityType::Spoiler,
        EntityKind::Code => td_enums::TextEntityType::Code,
        EntityKind::Pre { language: None } => td_enums::TextEntityType::Pre,
        EntityKind::Pre {
            language: Some(language),
        } => td_enums::TextEntityType::PreCode(td_types::TextEntityTypePreCode { language }),
        EntityKind::Blockquote => td_enums::TextEntityType::BlockQuote,
        EntityKind::TextUrl { url } => {
            td_enums::TextEntityType::TextUrl(td_types::TextEntityTypeTextUrl { url })
        }
        EntityKind::Url | EntityKind::Mention | EntityKind::Hashtag => return None,
    };
    Some(td_types::TextEntity {
        offset: clamp_i32(entity.offset_utf16),
        length: clamp_i32(entity.length_utf16),
        r#type: kind,
    })
}

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

async fn set_tdlib_parameters(client_id: i32, params: TdlibParams) -> Result<(), TdError> {
    functions::set_tdlib_parameters(
        false,
        params.database_directory.to_string_lossy().into_owned(),
        // Empty files directory: TDLib then nests files under the database
        // directory, which is the single 0700 tree the app already manages.
        String::new(),
        // TDLib's JSON interface encodes `bytes` as base64.
        base64_encode(&params.database_encryption_key),
        params.use_file_database,
        params.use_chat_info_database,
        params.use_message_database,
        params.use_secret_chats,
        params.api_id,
        params.api_hash,
        params.system_language_code,
        params.device_model,
        // System version: left to TDLib, which probes the OS itself.
        String::new(),
        params.application_version,
        client_id,
    )
    .await
    .map_err(map_td_error)
}

// ---------------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------------

fn map_td_error(error: td_types::Error) -> TdError {
    map_error_parts(error.code, &error.message)
}

/// TDLib reports everything as `(code, message)`. This is the single place
/// those turn into named variants the state machines can match on.
fn map_error_parts(code: i32, message: &str) -> TdError {
    if let Some(seconds) = parse_flood_wait(message) {
        return TdError::FloodWait { seconds };
    }

    let upper = message.to_ascii_uppercase();

    if upper.contains("PHONE_NUMBER_INVALID") {
        return TdError::PhoneNumberInvalid;
    }
    if upper.contains("PHONE_CODE_INVALID") {
        return TdError::CodeInvalid;
    }
    if upper.contains("PASSWORD_HASH_INVALID") {
        return TdError::PasswordInvalid;
    }
    if code == 401
        || upper.contains("UNAUTHORIZED")
        || upper.contains("AUTH_KEY_UNREGISTERED")
        || upper.contains("SESSION_REVOKED")
        || upper.contains("SESSION_EXPIRED")
    {
        return TdError::Unauthorized;
    }
    if code == 408 || upper.contains("TIMEOUT") || upper.contains("TIMED OUT") {
        return TdError::NetTimeout;
    }

    TdError::Other {
        code,
        message: message.to_string(),
    }
}

/// Two spellings in the wild: TDLib's own `"Too Many Requests: retry after N"`
/// and the raw MTProto `"FLOOD_WAIT_N"` it sometimes passes through.
fn parse_flood_wait(message: &str) -> Option<u32> {
    let upper = message.to_ascii_uppercase();

    if let Some(rest) = upper.split("RETRY AFTER").nth(1) {
        return first_number(rest);
    }
    if let Some(rest) = upper.split("FLOOD_WAIT_").nth(1) {
        return first_number(rest);
    }
    None
}

fn first_number(text: &str) -> Option<u32> {
    let digits: String = text
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// First line only, hard-capped at [`EXCERPT_MAX_CHARS`] characters including
/// the ellipsis, so an excerpt can never be taller or wider than its slot.
fn one_line_excerpt(text: &str) -> String {
    let first = text.lines().next().unwrap_or_default().trim();
    let count = first.chars().count();
    if count <= EXCERPT_MAX_CHARS {
        return first.to_string();
    }
    let mut out: String = first.chars().take(EXCERPT_MAX_CHARS - 1).collect();
    out.push('…');
    out
}

/// `+15551234` → `+1***34`: enough to recognize your own number, not enough to
/// be a phone number.
fn mask_phone(phone: &str) -> String {
    let chars: Vec<char> = phone.chars().collect();
    if chars.len() <= 4 {
        return phone.to_string();
    }
    let head: String = chars.iter().take(2).collect();
    let tail: String = chars.iter().skip(chars.len() - 2).collect();
    format!("{head}***{tail}")
}

/// The serde discriminant of any TDLib enum value — the `@type` tag, which is
/// exactly the name in TDLib's own schema. Only the tag is read; the rest of
/// the serialized value (which may carry PII) is dropped on the floor.
fn td_type_name<T: serde::Serialize>(value: &T) -> Option<String> {
    let value = serde_json::to_value(value).ok()?;
    value.get("@type")?.as_str().map(ToString::to_string)
}

fn non_negative(value: i32) -> u32 {
    value.max(0) as u32
}

fn clamp_u8(value: i32) -> u8 {
    value.clamp(0, i32::from(u8::MAX)) as u8
}

fn clamp_i32(value: u32) -> i32 {
    value.min(i32::MAX as u32) as i32
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding. Written out rather than pulled in: the only
/// caller is the 32-byte database encryption key, and the dependency set is
/// frozen by T01.
fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let bits = (b0 << 16) | (b1 << 8) | b2;

        out.push(BASE64_ALPHABET[(bits >> 18 & 0x3f) as usize] as char);
        out.push(BASE64_ALPHABET[(bits >> 12 & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[(bits >> 6 & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[(bits & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Tests — pure mapping only: no client, no network, no TDLib state.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn code_info(r#type: td_enums::AuthenticationCodeType) -> td_types::AuthenticationCodeInfo {
        td_types::AuthenticationCodeInfo {
            phone_number: "+15551234".to_string(),
            r#type,
            next_type: None,
            timeout: 60,
        }
    }

    #[test]
    fn flood_wait_message_parses_seconds() {
        assert_eq!(
            map_error_parts(429, "Too Many Requests: retry after 42"),
            TdError::FloodWait { seconds: 42 }
        );
        assert_eq!(
            map_error_parts(420, "FLOOD_WAIT_17"),
            TdError::FloodWait { seconds: 17 }
        );
        assert_eq!(
            map_error_parts(500, "Something else entirely"),
            TdError::Other {
                code: 500,
                message: "Something else entirely".to_string()
            }
        );
    }

    #[test]
    fn error_codes_map_to_named_variants() {
        let cases: &[(i32, &str, TdError)] = &[
            (400, "PHONE_NUMBER_INVALID", TdError::PhoneNumberInvalid),
            (400, "PHONE_CODE_INVALID", TdError::CodeInvalid),
            (400, "PASSWORD_HASH_INVALID", TdError::PasswordInvalid),
            (401, "Unauthorized", TdError::Unauthorized),
            (401, "AUTH_KEY_UNREGISTERED", TdError::Unauthorized),
            (406, "SESSION_REVOKED", TdError::Unauthorized),
            (500, "Timeout expired", TdError::NetTimeout),
            (408, "Request aborted", TdError::NetTimeout),
        ];

        for (code, message, expected) in cases {
            assert_eq!(
                map_error_parts(*code, message),
                *expected,
                "({code}, {message})"
            );
        }

        // Everything unrecognized keeps both halves, verbatim.
        assert_eq!(
            map_error_parts(400, "CHAT_ADMIN_REQUIRED"),
            TdError::Other {
                code: 400,
                message: "CHAT_ADMIN_REQUIRED".to_string()
            }
        );
    }

    #[test]
    fn flood_wait_wins_over_the_status_code() {
        // 429 is also an "unauthorized-adjacent" code in some transports;
        // the flood parse must run first or the countdown is lost.
        assert_eq!(
            map_error_parts(401, "Too Many Requests: retry after 5"),
            TdError::FloodWait { seconds: 5 }
        );
    }

    #[test]
    fn reply_excerpt_truncated_to_one_line() {
        let excerpt = one_line_excerpt("first line\nsecond line\nthird line");
        assert_eq!(excerpt, "first line");

        let long = "x".repeat(200);
        let excerpt = one_line_excerpt(&format!("{long}\ntail"));
        assert_eq!(excerpt.chars().count(), EXCERPT_MAX_CHARS);
        assert!(excerpt.ends_with('…'));
        assert!(!excerpt.contains('\n'));

        // CRLF counts as a line break too.
        assert_eq!(one_line_excerpt("windows\r\nline"), "windows");
        assert_eq!(one_line_excerpt(""), "");
    }

    #[test]
    fn excerpt_truncation_is_char_safe_not_byte_safe() {
        let text = "é".repeat(200);
        let excerpt = one_line_excerpt(&text);
        assert_eq!(excerpt.chars().count(), EXCERPT_MAX_CHARS);
    }

    #[test]
    fn auth_states_map_to_phases() {
        assert_eq!(
            map_auth_state(td_enums::AuthorizationState::WaitTdlibParameters),
            AuthPhase::WaitTdlibParameters
        );
        assert_eq!(
            map_auth_state(td_enums::AuthorizationState::Ready),
            AuthPhase::Ready
        );
        assert_eq!(
            map_auth_state(td_enums::AuthorizationState::Closed),
            AuthPhase::Closed
        );
    }

    #[test]
    fn wait_code_carries_a_masked_delivery_hint_and_length() {
        let phase = map_auth_state(td_enums::AuthorizationState::WaitCode(
            td_types::AuthorizationStateWaitCode {
                code_info: code_info(td_enums::AuthenticationCodeType::Sms(
                    td_types::AuthenticationCodeTypeSms { length: 5 },
                )),
            },
        ));
        assert_eq!(
            phase,
            AuthPhase::WaitCode {
                delivery_hint: "SMS to +1***34".to_string(),
                length: 5,
            }
        );
    }

    #[test]
    fn wait_password_drops_an_empty_hint() {
        let with_hint = map_auth_state(td_enums::AuthorizationState::WaitPassword(
            td_types::AuthorizationStateWaitPassword {
                password_hint: "the usual".to_string(),
                has_recovery_email_address: false,
                has_passport_data: false,
                recovery_email_address_pattern: String::new(),
            },
        ));
        assert_eq!(
            with_hint,
            AuthPhase::WaitPassword {
                hint: Some("the usual".to_string())
            }
        );

        let without_hint = map_auth_state(td_enums::AuthorizationState::WaitPassword(
            td_types::AuthorizationStateWaitPassword {
                password_hint: String::new(),
                has_recovery_email_address: false,
                has_passport_data: false,
                recovery_email_address_pattern: String::new(),
            },
        ));
        assert_eq!(without_hint, AuthPhase::WaitPassword { hint: None });
    }

    #[test]
    fn unimplemented_auth_state_is_named_not_swallowed() {
        let phase = map_auth_state(td_enums::AuthorizationState::WaitRegistration(
            td_types::AuthorizationStateWaitRegistration {
                terms_of_service: td_types::TermsOfService {
                    text: td_types::FormattedText {
                        text: String::new(),
                        entities: Vec::new(),
                    },
                    min_user_age: 0,
                    show_popup: false,
                },
            },
        ));
        assert_eq!(
            phase,
            AuthPhase::Unsupported {
                name: "authorizationStateWaitRegistration".to_string()
            }
        );
    }

    #[test]
    fn connection_states_map_to_phases() {
        let cases = [
            (
                td_enums::ConnectionState::WaitingForNetwork,
                ConnectionPhase::WaitingForNetwork,
            ),
            (
                td_enums::ConnectionState::Connecting,
                ConnectionPhase::Connecting,
            ),
            (
                td_enums::ConnectionState::ConnectingToProxy,
                ConnectionPhase::ConnectingToProxy,
            ),
            (
                td_enums::ConnectionState::Updating,
                ConnectionPhase::Updating,
            ),
            (td_enums::ConnectionState::Ready, ConnectionPhase::Ready),
        ];
        for (raw, expected) in cases {
            assert_eq!(map_connection_state(&raw), expected);
        }
    }

    #[test]
    fn user_statuses_collapse_to_three_presences() {
        assert_eq!(
            map_presence(&td_enums::UserStatus::Online(td_types::UserStatusOnline {
                expires: 0
            })),
            PresenceStatus::Online
        );
        assert_eq!(
            map_presence(&td_enums::UserStatus::Recently(
                td_types::UserStatusRecently {
                    by_my_privacy_settings: false
                }
            )),
            PresenceStatus::Recently
        );
        assert_eq!(
            map_presence(&td_enums::UserStatus::Empty),
            PresenceStatus::Offline
        );
    }

    #[test]
    fn chat_lists_round_trip() {
        for list in [ChatListId::Main, ChatListId::Archive, ChatListId::Folder(7)] {
            assert_eq!(map_chat_list(&to_td_chat_list(list)), list);
        }
    }

    #[test]
    fn muted_requires_an_explicit_non_default_mute() {
        let mut settings = td_types::ChatNotificationSettings {
            use_default_mute_for: false,
            mute_for: 3600,
            use_default_sound: true,
            sound_id: 0,
            use_default_show_preview: true,
            show_preview: true,
            use_default_mute_stories: true,
            mute_stories: false,
            use_default_story_sound: true,
            story_sound_id: 0,
            use_default_show_story_poster: true,
            show_story_poster: false,
            use_default_disable_pinned_message_notifications: true,
            disable_pinned_message_notifications: false,
            use_default_disable_mention_notifications: true,
            disable_mention_notifications: false,
        };
        assert!(is_muted(&settings));

        settings.mute_for = 0;
        assert!(!is_muted(&settings));

        settings.mute_for = 3600;
        settings.use_default_mute_for = true;
        assert!(!is_muted(&settings));
    }

    #[test]
    fn entities_round_trip_through_tdlib_shapes() {
        let entity = TextEntity {
            offset_utf16: 3,
            length_utf16: 5,
            kind: EntityKind::Pre {
                language: Some("rust".to_string()),
            },
        };
        let round_tripped = map_entity(to_td_entity(entity.clone()).unwrap()).unwrap();
        assert_eq!(round_tripped, entity);

        // Auto-detected entity types are not accepted on input.
        assert!(
            to_td_entity(TextEntity {
                offset_utf16: 0,
                length_utf16: 1,
                kind: EntityKind::Url,
            })
            .is_none()
        );
    }

    #[test]
    fn unrepresentable_entities_are_dropped_not_misrendered() {
        let dropped = map_entity(td_types::TextEntity {
            offset: 0,
            length: 4,
            r#type: td_enums::TextEntityType::BankCardNumber,
        });
        assert!(dropped.is_none());
    }

    #[test]
    fn masked_phone_keeps_only_the_recognizable_ends() {
        assert_eq!(mask_phone("+15551234"), "+1***34");
        // Too short to mask meaningfully: left alone.
        assert_eq!(mask_phone("+123"), "+123");
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        // The shape the encryption key actually takes: 32 bytes → 44 chars.
        assert_eq!(base64_encode(&[0u8; 32]).len(), 44);
    }

    #[test]
    fn td_type_name_reads_the_schema_discriminant() {
        assert_eq!(
            td_type_name(&td_enums::ConnectionState::Ready).as_deref(),
            Some("connectionStateReady")
        );
    }

    #[test]
    fn unsupported_content_is_named_after_its_tdlib_type() {
        let content = map_content(td_enums::MessageContent::MessageContactRegistered);
        assert_eq!(
            content,
            MessageContent::Unsupported {
                description: "ContactRegistered".to_string()
            }
        );
    }

    #[test]
    fn text_content_carries_entities_across() {
        let content = map_content(td_enums::MessageContent::MessageText(
            td_types::MessageText {
                text: td_types::FormattedText {
                    text: "hello".to_string(),
                    entities: vec![td_types::TextEntity {
                        offset: 0,
                        length: 5,
                        r#type: td_enums::TextEntityType::Bold,
                    }],
                },
                link_preview: None,
                link_preview_options: None,
            },
        ));
        assert_eq!(
            content,
            MessageContent::Text(FormattedText {
                text: "hello".to_string(),
                entities: vec![TextEntity {
                    offset_utf16: 0,
                    length_utf16: 5,
                    kind: EntityKind::Bold,
                }],
            })
        );
    }

    #[test]
    fn file_snapshot_prefers_the_known_size_and_local_path() {
        let file = td_types::File {
            id: 9,
            size: 0,
            expected_size: 2048,
            local: td_types::LocalFile {
                path: "/tmp/x.jpg".to_string(),
                can_be_downloaded: true,
                can_be_deleted: true,
                is_downloading_active: true,
                is_downloading_completed: false,
                download_offset: 0,
                downloaded_prefix_size: 0,
                downloaded_size: 512,
            },
            remote: td_types::RemoteFile {
                id: String::new(),
                unique_id: String::new(),
                is_uploading_active: false,
                is_uploading_completed: true,
                uploaded_size: 0,
            },
        };
        let snapshot = map_file(file);
        assert_eq!(
            snapshot,
            FileSnapshot {
                id: FileId(9),
                expected_size: 2048,
                downloaded_size: 512,
                is_downloading: true,
                is_completed: false,
                local_path: Some(PathBuf::from("/tmp/x.jpg")),
            }
        );
    }

    /// A minimal `td_types::File` in a given download state, for the seeding
    /// tests below. `id` and `path` are the only bits those tests care about.
    fn sample_td_file(id: i32, completed: bool, path: &str) -> td_types::File {
        td_types::File {
            id,
            size: 2048,
            expected_size: 2048,
            local: td_types::LocalFile {
                path: if completed {
                    path.to_string()
                } else {
                    String::new()
                },
                can_be_downloaded: true,
                can_be_deleted: completed,
                is_downloading_active: !completed,
                is_downloading_completed: completed,
                download_offset: 0,
                downloaded_prefix_size: 0,
                downloaded_size: if completed { 2048 } else { 0 },
            },
            remote: td_types::RemoteFile {
                id: String::new(),
                unique_id: String::new(),
                is_uploading_active: false,
                is_uploading_completed: true,
                uploaded_size: 0,
            },
        }
    }

    #[test]
    fn a_completed_photo_seeds_its_local_path() {
        let content = td_enums::MessageContent::MessagePhoto(td_types::MessagePhoto {
            photo: td_types::Photo {
                sizes: vec![td_types::PhotoSize {
                    photo: sample_td_file(9, true, "/tmp/x.jpg"),
                    width: 800,
                    height: 600,
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        });

        let seed = content_file_seed(&content).expect("photo carries a file");
        assert_eq!(seed.id, FileId(9));
        assert!(seed.is_completed);
        assert_eq!(seed.local_path, Some(PathBuf::from("/tmp/x.jpg")));
    }

    #[test]
    fn an_undownloaded_photo_seeds_no_path() {
        let content = td_enums::MessageContent::MessagePhoto(td_types::MessagePhoto {
            photo: td_types::Photo {
                sizes: vec![td_types::PhotoSize {
                    photo: sample_td_file(9, false, "/tmp/x.jpg"),
                    width: 800,
                    height: 600,
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        });

        let seed = content_file_seed(&content).expect("photo carries a file");
        assert!(!seed.is_completed);
        assert_eq!(seed.local_path, None);
    }

    #[test]
    fn a_completed_document_also_seeds_a_path() {
        // Not just photos: an already-downloaded PDF should say "open", not
        // "download", after a restart too.
        let content = td_enums::MessageContent::MessageDocument(td_types::MessageDocument {
            document: td_types::Document {
                file_name: "report.pdf".to_string(),
                mime_type: "application/pdf".to_string(),
                minithumbnail: None,
                thumbnail: None,
                document: sample_td_file(42, true, "/tmp/report.pdf"),
            },
            caption: td_types::FormattedText::default(),
        });

        let seed = content_file_seed(&content).expect("document carries a file");
        assert_eq!(seed.id, FileId(42));
        assert!(seed.is_completed);
        assert_eq!(seed.local_path, Some(PathBuf::from("/tmp/report.pdf")));
    }

    #[test]
    fn fileless_content_seeds_nothing() {
        let content = td_enums::MessageContent::MessageText(td_types::MessageText {
            text: td_types::FormattedText::default(),
            link_preview: None,
            link_preview_options: None,
        });
        assert!(content_file_seed(&content).is_none());
    }

    #[test]
    fn seeding_the_same_file_twice_forwards_once() {
        // Exercises the exact path `map_messages` funnels every message in a
        // page through: a 50-message page referencing one file must not push
        // 50 identical `TdUpdate::File`s.
        let (tx, mut rx) = mpsc::channel(8);
        let seeded = SeededFiles::default();
        let snapshot = map_file(sample_td_file(9, true, "/tmp/x.jpg"));

        seed_and_forward(&tx, &seeded, snapshot.clone());
        seed_and_forward(&tx, &seeded, snapshot.clone());
        drop(tx);

        let mut received = Vec::new();
        while let Ok(update) = rx.try_recv() {
            received.push(update);
        }
        assert_eq!(received.len(), 1);
        assert!(matches!(&received[0], TdUpdate::File(f) if f.id == FileId(9)));
    }

    #[test]
    fn a_file_already_marked_live_is_never_reseeded() {
        // Simulates a live `updateFile` having already been forwarded for a
        // file id: a later message payload's (possibly stale) copy of the
        // same file must not clobber it.
        let (tx, mut rx) = mpsc::channel(8);
        let seeded = SeededFiles::default();
        seeded.mark(FileId(9));

        let stale = map_file(sample_td_file(9, false, "/tmp/x.jpg"));
        seed_and_forward(&tx, &seeded, stale);
        drop(tx);

        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn name_cache_resolves_both_sender_shapes() {
        let names = NameCache::default();
        names.put_user(1, "Ada Lovelace".to_string());
        names.put_chat(-100, "Rust Nerds".to_string());

        assert_eq!(names.resolve(Sender::User(UserId(1))), "Ada Lovelace");
        assert_eq!(names.resolve(Sender::Chat(ChatId(-100))), "Rust Nerds");
        // Unknown senders resolve to empty, never to a placeholder that could
        // be mistaken for a real name.
        assert_eq!(names.resolve(Sender::User(UserId(2))), "");
    }

    #[test]
    fn user_display_name_falls_back_to_the_username() {
        let mut user = td_types::User {
            id: 1,
            first_name: String::new(),
            last_name: String::new(),
            usernames: Some(td_types::Usernames {
                active_usernames: vec!["ada".to_string()],
                disabled_usernames: Vec::new(),
                editable_username: "ada".to_string(),
                collectible_usernames: Vec::new(),
            }),
            phone_number: String::new(),
            status: td_enums::UserStatus::Empty,
            profile_photo: None,
            accent_color_id: 0,
            background_custom_emoji_id: 0,
            upgraded_gift_colors: None,
            profile_accent_color_id: -1,
            profile_background_custom_emoji_id: 0,
            emoji_status: None,
            is_contact: false,
            is_mutual_contact: false,
            is_close_friend: false,
            verification_status: None,
            is_premium: false,
            is_support: false,
            restriction_info: None,
            active_story_state: None,
            restricts_new_chats: false,
            paid_message_star_count: 0,
            have_access: true,
            r#type: td_enums::UserType::Regular,
            language_code: String::new(),
            added_to_attachment_menu: false,
        };
        assert_eq!(user_display_name(&user), "@ada");

        user.first_name = "Ada".to_string();
        assert_eq!(user_display_name(&user), "Ada");
    }
}
