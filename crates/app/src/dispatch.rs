//! `Effect` → async execution (docs/architecture.md §2.3, §3). Each effect is
//! spawned; its completion re-enters the loop's action channel as an
//! `Action::TdResult` / `Action::Io`, which is the only way a result ever
//! reaches the pure `update()`.
//!
//! Wired so far: `Quit`, `Telemetry`, `Td`, `SaveConfig`, `CopyToClipboard`,
//! `OpenExternal`, `Alert`.
//!
//! # `Effect::Alert` — the one deliberate write to the terminal
//!
//! Nothing in this process may write to stdout while the TUI holds the
//! alternate screen: a stray byte lands in a cell the renderer believes it
//! owns. The terminal alert (spec §6.4) is the exception, and it is only an
//! exception in the sense that it is *meant* for the terminal emulator
//! rather than for the screen — `OSC 777` (or `BEL`) is a message to the
//! multiplexer, consumed before it can paint anything. [`Inner::alert`] is
//! therefore the single sanctioned escape-sequence write in the crate. It
//! goes through `io::stdout()`, the same global handle
//! `CrosstermBackend` was built on in `main.rs`, so the two writers share
//! one lock and one buffer and cannot interleave mid-sequence; the flush is
//! explicit because the sequence carries no newline to trigger one.
//!
//! What may be written is fixed at compile time in `notify.rs` — `alert`
//! takes no content parameters at all, so no chat title or message text can
//! reach the wire even by accident (spec §6.4's PII rule).
//!
//! # Sending a file — the other impure boundary
//!
//! `state/modal.rs` builds every `SendMessageFile` with
//! `OutgoingFileKind::Document` and whatever path string the user typed or
//! pasted, because `tgt-core` may not touch the filesystem (architecture
//! §9.3): it can neither expand a leading `~`, nor check that the file is
//! there, nor derive a kind from an extension it isn't allowed to resolve.
//! [`resolve_outgoing_file`] does all three here, in the last moment before
//! the request would reach TDLib — see its doc comment.
//!
//! # Opening a file externally
//!
//! `OpenExternal` shells out to whatever the platform uses to hand a file to
//! its default application — see [`DEFAULT_OPENER`] for the three spellings —
//! overridable through `TGT_OPENER`, which is what the integration tests
//! point at a harmless command. The child is spawned and awaited for its
//! exit status only: nothing it writes is read, because a viewer inheriting
//! this process's stdio would paint over the TUI. `Stdio::null()` on all
//! three streams is the enforcement.
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

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, watch};

use tgt_core::action::{Action, IoErrorKind, IoResult, TdResult};
use tgt_core::effect::{ConfigPatch, Effect};
use tgt_core::model::ids::{ChatId, FileId, MessageId};
use tgt_core::model::message::MessageView;
use tgt_core::td::error::TdError;
use tgt_core::td::request::{TdRequest, TdResponse, TdlibParams};
use tgt_core::td::runtime::TdRuntime;

use crate::config::Config;
use crate::media_kind;

/// TDLib parameters that come from neither the config file nor `update()`:
/// the 32-byte database key held in the platform credential store and the
/// database directory, which is mode 0700 where the platform has modes. The
/// api credentials are read from the shared [`Config`] at the
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

// What Telegram shows this session as in the user's active sessions list, so
// it names the machine the client runs on rather than the client. Cosmetic,
// but it is the line a user reads when deciding whether a session is theirs,
// and every platform reporting "Mac" makes that harder rather than easier.
#[cfg(target_os = "macos")]
const DEVICE_MODEL: &str = "Mac";
#[cfg(target_os = "windows")]
const DEVICE_MODEL: &str = "PC";
#[cfg(target_os = "linux")]
const DEVICE_MODEL: &str = "Linux";
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
const DEVICE_MODEL: &str = "Desktop";

/// The platform's "hand this to whatever handles it" command, as a program
/// plus the arguments that have to precede the path. See the module docs.
///
/// macOS has `open` and the freedesktop platforms have `xdg-open`, both of
/// which take the path as their only argument. Windows has no such
/// executable: `start` is a `cmd` builtin, so it has to be invoked through
/// `cmd /c`. The empty `""` is not padding — `start` reads its first quoted
/// argument as the title of the console window to open, so without a title to
/// eat it, a quoted path is consumed as one and nothing is opened.
#[cfg(target_os = "macos")]
const DEFAULT_OPENER: (&str, &[&str]) = ("open", &[]);
#[cfg(target_os = "windows")]
const DEFAULT_OPENER: (&str, &[&str]) = ("cmd", &["/c", "start", ""]);
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const DEFAULT_OPENER: (&str, &[&str]) = ("xdg-open", &[]);

/// Overrides the program above with a bare command taking the path as its
/// only argument — which is what the integration tests point at a harmless
/// command, and the escape hatch for a desktop whose handler is neither of
/// the defaults.
const OPENER_ENV: &str = "TGT_OPENER";

/// Executes `Effect`s produced by `App::update`. Everything an effect needs
/// lives behind one `Arc` so a spawned task can outlive the call that
/// started it.
pub struct Dispatcher {
    inner: Arc<Inner>,
    quit_tx: watch::Sender<bool>,
}

struct Inner {
    action_tx: mpsc::Sender<Action>,
    /// Swappable, because a TDLib client that reaches
    /// `authorizationStateClosed` is dead and only a *new* client can get
    /// back to a usable state — see [`Dispatcher::replace_runtime`].
    /// Cloned out under the lock and never held across an await.
    runtime: Mutex<Arc<dyn TdRuntime>>,
    /// Bumped by every [`Dispatcher::replace_runtime`]. A request spawned
    /// against one client must not deliver its completion into a session
    /// running on the next one: the chat it names may not exist any more.
    generation: AtomicU64,
    config: Arc<Mutex<Config>>,
    td_boot: TdBootParams,
    /// TDLib asked for its parameters before credentials existed. Cleared by
    /// whoever fires the deferred request (see module docs).
    params_pending: AtomicBool,
    /// Carries a fatal config-write failure back to `runtime_loop::run`,
    /// which returns it out through `run_tui`'s `TerminalGuard` so the
    /// message prints to a restored shell rather than into the alternate
    /// screen. Capacity one: the first failure ends the run, and anything
    /// after it is the same failure again.
    fatal_tx: mpsc::Sender<human_errors::Error>,
    /// Probed once, at construction: the heuristic reads `TERM_PROGRAM`,
    /// which cannot change under a running process, and an alert is not the
    /// place to be re-reading the environment.
    supports_osc777: bool,
}

impl Dispatcher {
    /// Builds a dispatcher wired to the loop's action channel and returns
    /// the `watch::Receiver` the loop selects on for `Effect::Quit`.
    pub fn new(
        action_tx: mpsc::Sender<Action>,
        runtime: Arc<dyn TdRuntime>,
        config: Arc<Mutex<Config>>,
        td_boot: TdBootParams,
    ) -> (
        Self,
        watch::Receiver<bool>,
        mpsc::Receiver<human_errors::Error>,
    ) {
        let (quit_tx, quit_rx) = watch::channel(false);
        let (fatal_tx, fatal_rx) = mpsc::channel(1);
        let inner = Arc::new(Inner {
            action_tx,
            runtime: Mutex::new(runtime),
            generation: AtomicU64::new(0),
            config,
            td_boot,
            params_pending: AtomicBool::new(false),
            supports_osc777: crate::notify::supports_osc777(),
            fatal_tx,
        });
        (Dispatcher { inner, quit_tx }, quit_rx, fatal_rx)
    }

    /// The client requests are currently issued against, so the loop can
    /// shut it down before creating its replacement.
    pub fn runtime(&self) -> Arc<dyn TdRuntime> {
        self.inner.runtime()
    }

    /// Swaps in a freshly created TDLib client after the previous one
    /// reached `Closed`, and returns the generation the new one runs under.
    ///
    /// Every request already in flight against the old client is abandoned
    /// here: the generation moves, so their completions are dropped by
    /// [`Inner::deliver`] rather than applied to a session that has moved
    /// on. `params_pending` is cleared because the new client will ask for
    /// its parameters itself, exactly as on a cold boot, and a stale flag
    /// would make the deferred-issue path fire a second time.
    pub fn replace_runtime(&self, runtime: Arc<dyn TdRuntime>) -> u64 {
        *self
            .inner
            .runtime
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = runtime;
        self.inner.params_pending.store(false, Ordering::SeqCst);
        let generation = self.inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
        tracing::debug!(generation, "tdlib client replaced");
        generation
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
                // The trail a crash report arrives with. A no-op unless a
                // Sentry client is bound, and allowlist-shaped either way —
                // it is built from this same event's fields. Before the
                // `emit!` because that macro consumes the event.
                crate::crash::record_action(&event);
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
            Effect::CopyToClipboard { text } => {
                let inner = Arc::clone(&self.inner);
                tokio::spawn(async move { inner.copy_to_clipboard(text).await });
            }
            Effect::OpenExternal { path } => {
                let inner = Arc::clone(&self.inner);
                tokio::spawn(async move { inner.open_external(path).await });
            }
            // Not spawned: this is a handful of bytes into an already-open
            // handle, and a notification the user hears a frame later than
            // the toast it belongs to would be the odder outcome.
            Effect::Alert => self.inner.alert(),
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
    /// Emits the terminal alert — see the module docs for why this write is
    /// allowed to reach stdout while the TUI is up. A terminal that refuses
    /// the bytes costs the user one missed bell, never a failed action, so
    /// the error is logged and dropped.
    fn alert(&self) {
        let mut out = io::stdout().lock();
        let written =
            crate::notify::alert(&mut out, self.supports_osc777).and_then(|()| out.flush());
        if let Err(err) = written {
            tracing::debug!(error = %err, "terminal alert not written");
        }
    }

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
        // Read once, before the await: which client this request belongs to
        // is fixed at the moment it is issued, and that is what its
        // completion is checked against.
        let generation = self.generation.load(Ordering::SeqCst);
        let request = match resolve_outgoing_file(request) {
            Resolved::Send(request) => request,
            Resolved::Failed(failure) => {
                self.deliver(generation, failure).await;
                return;
            }
        };
        let completion = completion_for(&request);
        let kind = request.kind();
        let outcome = self.runtime().request(request).await;

        if let Err(err) = &outcome {
            // The dispatcher never handles errors beyond logging: the state
            // machines decide what an error means (architecture §3).
            tracing::debug!(request = kind, error = %err, "td request failed");
        }

        let Some(result) = map_completion(completion, outcome) else {
            tracing::debug!(request = kind, "td response has no completion action yet");
            return;
        };
        self.deliver(generation, Action::TdResult(result)).await;
    }

    /// The current client, cloned out so the lock is never held across an
    /// await.
    fn runtime(&self) -> Arc<dyn TdRuntime> {
        Arc::clone(
            &self
                .runtime
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
        )
    }

    /// Sends a completion, unless the client it was issued against has since
    /// been replaced.
    ///
    /// Dropped completions are logged rather than discarded quietly. A
    /// silent drop here would be the swallowed-completions bug wearing a
    /// different hat, and the whole reason this generation check exists is
    /// that the alternative — applying a dead client's answer to a live
    /// session — is worse.
    async fn deliver(&self, generation: u64, action: Action) {
        let current = self.generation.load(Ordering::SeqCst);
        if generation != current {
            tracing::debug!(
                issued_under = generation,
                current,
                "dropping a completion from a replaced tdlib client"
            );
            return;
        }
        let _ = self.action_tx.send(action).await;
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

        // A config write that fails ends the run. See `config::unwritable`
        // for why, including the load-bearing note about the auth wizard —
        // read it before changing this to anything softer.
        //
        // The error goes back to `runtime_loop::run` rather than being
        // printed or `panic!`ed here: this task runs while the TUI still
        // owns the alternate screen, so anything written now lands in cells
        // the renderer believes it owns, and a message the user cannot read
        // is the exact failure an actionable message exists to prevent.
        let fatal = match saved {
            Ok(Ok(())) => None,
            Ok(Err(err)) => {
                tracing::error!(error = %err, "could not save the config; aborting");
                Some(err)
            }
            // The blocking task panicked or was cancelled. The config is in
            // an unknown state and the process is already unwell; treat it
            // the same way rather than carrying on as if the write happened.
            Err(err) => {
                tracing::error!(error = %err, "the config save task did not finish; aborting");
                Some(human_errors::system(
                    format!("We couldn't save your configuration: {err}"),
                    &[
                        "This is a bug. Please report it with the log from ~/.local/state/telegram-tui/.",
                    ],
                ))
            }
        };

        if let Some(err) = fatal {
            // `try_send` rather than `send`: the channel holds one error and
            // the loop is on its way out, so a full channel means a failure
            // is already in flight and this one is redundant. Blocking here
            // would keep a doomed task alive for no benefit.
            let _ = self.fatal_tx.try_send(err);
            return;
        }

        let _ = self
            .action_tx
            .send(Action::Io(IoResult::ConfigSaved { outcome: Ok(()) }))
            .await;

        // First run: TDLib asked for its parameters before the wizard had
        // produced any credentials. Now it has (see module docs).
        if matches!(patch, ConfigPatch::Credentials { .. })
            && self.params_pending.swap(false, Ordering::SeqCst)
        {
            self.send_tdlib_parameters().await;
        }
    }

    /// `arboard` talks to the platform clipboard synchronously (a round trip
    /// through NSPasteboard on macOS, and a protocol exchange with the
    /// compositor or X server elsewhere), so it goes on the blocking pool
    /// like every other blocking call in this crate.
    ///
    /// The `Clipboard` handle is built and dropped inside the task rather
    /// than cached on `Inner`: it is neither `Sync` nor cheap to hold across
    /// a suspended TUI, and a copy happens at human speed.
    async fn copy_to_clipboard(&self, text: String) {
        let copied = tokio::task::spawn_blocking(move || {
            arboard::Clipboard::new()
                .and_then(|mut clipboard| clipboard.set_text(text))
                .map_err(|e| e.to_string())
        })
        .await;

        let outcome = match copied {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => {
                tracing::warn!(error = %err, "could not write to the clipboard");
                Err(IoErrorKind::Other)
            }
            Err(err) => {
                tracing::warn!(error = %err, "the clipboard task did not finish");
                Err(IoErrorKind::Other)
            }
        };
        let _ = self
            .action_tx
            .send(Action::Io(IoResult::ClipboardCopied { outcome }))
            .await;
    }

    /// Hands a downloaded file to the platform viewer. See the module docs
    /// for the opener command and why the child's stdio is discarded.
    async fn open_external(&self, path: PathBuf) {
        // An override replaces the whole invocation, leading arguments
        // included: it names a program that takes the path and nothing else.
        let (opener, leading_args) = match std::env::var(OPENER_ENV) {
            Ok(custom) => (custom, &[][..]),
            Err(_) => (DEFAULT_OPENER.0.to_string(), DEFAULT_OPENER.1),
        };
        let status = tokio::process::Command::new(&opener)
            .args(leading_args)
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;

        let outcome = match status {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => {
                tracing::warn!(opener, ?status, "the opener exited non-zero");
                Err(IoErrorKind::Other)
            }
            // A missing opener binary is the one failure with a distinct
            // cause worth telling the domain about: everything else the
            // viewer might do is `Other`.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(opener, "no such opener command");
                Err(IoErrorKind::NotFound)
            }
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                tracing::warn!(opener, "not allowed to run the opener");
                Err(IoErrorKind::Denied)
            }
            Err(err) => {
                tracing::warn!(opener, error = %err, "could not run the opener");
                Err(IoErrorKind::Other)
            }
        };
        let _ = self
            .action_tx
            .send(Action::Io(IoResult::ExternalOpened { path, outcome }))
            .await;
    }
}

/// Finishes a `SendMessageFile` the domain could only build halfway (see the
/// module docs): expands and existence-checks the path, then replaces core's
/// placeholder `Document` kind with the one the extension implies. Every
/// other request passes through untouched.
///
/// [`Resolved::Failed`] carries the completion action to send *instead of*
/// the request: a path that does not resolve never reaches TDLib at all, and
/// the send comes back as a failed `MessageSent`, the same shape a send TDLib
/// rejects produces. That matters more than the error's wording — it is the
/// one completion the composer's spec §14 handling already knows how to
/// unwind, so nothing is left waiting on a request that was never made.
///
/// A path that isn't valid UTF-8 counts as unresolvable: TDLib's JSON
/// interface takes paths as UTF-8 strings, so such a file could not be sent
/// even if it exists.
///
/// One `stat` runs on the caller's task rather than on the blocking pool.
/// It is a single metadata lookup at human speed (one confirmed send), which
/// is well under the cost of the hop it would take to move it.
fn resolve_outgoing_file(request: TdRequest) -> Resolved {
    // A `let ... else` would read better, but its pattern moves `request`
    // and the else branch needs it back to pass through.
    let (chat_id, path, caption) = match request {
        TdRequest::SendMessageFile {
            chat_id,
            path,
            kind: _,
            caption,
        } => (chat_id, path, caption),
        other => return Resolved::Send(other),
    };

    let Some(resolved) = path.to_str().and_then(media_kind::existing_path) else {
        tracing::warn!(path = %path.display(), "cannot send a file that isn't there");
        return Resolved::Failed(Action::TdResult(TdResult::MessageSent {
            chat_id,
            outcome: Err(TdError::Other {
                code: 0,
                message: format!("no such file: {}", path.display()),
            }),
        }));
    };

    Resolved::Send(TdRequest::SendMessageFile {
        chat_id,
        kind: media_kind::kind_for(&resolved),
        path: resolved,
        caption,
    })
}

/// [`resolve_outgoing_file`]'s answer. A plain `Result` would say the same
/// thing, but both sides are large enough that returning one trips
/// `clippy::result_large_err`, and neither `Action` nor `TdRequest` may be
/// boxed — architecture §4.3/§4.7 define both verbatim.
enum Resolved {
    Send(TdRequest),
    Failed(Action),
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
    /// `sendMessage` / `sendMessageFile` returned the optimistic message with
    /// its temporary id. Both send paths share it: a file send answers with
    /// the same optimistic `Message`, and everything downstream of it — the
    /// window append, the composer's spec §14 unwind, the upload the domain
    /// starts tracking under the returned id — is the same work.
    Sent {
        chat_id: ChatId,
    },
    MessageProperties {
        chat_id: ChatId,
        message_id: MessageId,
    },
    Edit {
        chat_id: ChatId,
        message_id: MessageId,
    },
    Delete {
        chat_id: ChatId,
    },
    Forward {
        to_chat_id: ChatId,
    },
    Reaction {
        chat_id: ChatId,
        message_id: MessageId,
    },
    Download {
        file_id: FileId,
    },
    Search {
        chat_id: ChatId,
    },
    LogOut,
    /// Executed for its effect inside TDLib; whatever state it changes comes
    /// back as a push update, so there is no completion action to send.
    FireAndForget,
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
        // `viewMessages` marks messages read; `cancelDownloadFile` stops a
        // transfer. All four are answered with a bare `Ok` and their
        // consequences arrive as updates — a cancelled download's final
        // `updateFile` (no longer downloading, not complete) is what flips
        // the card back to its download affordance, not this response.
        TdRequest::OpenChat { .. }
        | TdRequest::CloseChat { .. }
        | TdRequest::ViewMessages { .. }
        | TdRequest::CancelDownloadFile { .. } => Completion::FireAndForget,
        TdRequest::LogOut => Completion::LogOut,
        TdRequest::SendMessageText { chat_id, .. } | TdRequest::SendMessageFile { chat_id, .. } => {
            Completion::Sent { chat_id: *chat_id }
        }
        TdRequest::GetMessageProperties {
            chat_id,
            message_id,
        } => Completion::MessageProperties {
            chat_id: *chat_id,
            message_id: *message_id,
        },
        TdRequest::EditMessageText {
            chat_id,
            message_id,
            ..
        } => Completion::Edit {
            chat_id: *chat_id,
            message_id: *message_id,
        },
        TdRequest::DeleteMessages { chat_id, .. } => Completion::Delete { chat_id: *chat_id },
        TdRequest::ForwardMessages { to_chat_id, .. } => Completion::Forward {
            to_chat_id: *to_chat_id,
        },
        TdRequest::ToggleReaction {
            chat_id,
            message_id,
            ..
        } => Completion::Reaction {
            chat_id: *chat_id,
            message_id: *message_id,
        },
        TdRequest::DownloadFile { file_id, .. } => Completion::Download { file_id: *file_id },
        TdRequest::SearchChatMessages { chat_id, .. } => Completion::Search { chat_id: *chat_id },
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
        Completion::Sent { chat_id } => Some(TdResult::MessageSent {
            chat_id,
            outcome: outcome.and_then(|response| match response {
                TdResponse::Message(view) => Ok(view),
                other => Err(unexpected_response("sendMessage", "a Message", &other)),
            }),
        }),
        Completion::MessageProperties {
            chat_id,
            message_id,
        } => Some(TdResult::MessagePropertiesLoaded {
            chat_id,
            message_id,
            outcome: outcome.and_then(|response| match response {
                TdResponse::MessageProperties(caps) => Ok(caps),
                other => Err(unexpected_response(
                    "getMessageProperties",
                    "MessageProperties",
                    &other,
                )),
            }),
        }),
        Completion::Edit {
            chat_id,
            message_id,
        } => Some(TdResult::EditDone {
            chat_id,
            message_id,
            outcome: outcome.map(|_| ()),
        }),
        Completion::Delete { chat_id } => Some(TdResult::DeleteDone {
            chat_id,
            outcome: outcome.map(|_| ()),
        }),
        Completion::Forward { to_chat_id } => Some(TdResult::ForwardDone {
            to_chat_id,
            outcome: outcome.map(|_| ()),
        }),
        Completion::Reaction {
            chat_id,
            message_id,
        } => Some(TdResult::ReactionDone {
            chat_id,
            message_id,
            outcome: outcome.map(|_| ()),
        }),
        Completion::Download { file_id } => Some(TdResult::DownloadStarted {
            file_id,
            outcome: outcome.and_then(|response| match response {
                TdResponse::File(file) => Ok(file),
                other => Err(unexpected_response("downloadFile", "a File", &other)),
            }),
        }),
        Completion::Search { chat_id } => Some(TdResult::SearchDone {
            chat_id,
            outcome: outcome.map(found_message_ids),
        }),
        Completion::FireAndForget => None,
    }
}

/// A response of the wrong shape for its request, reported to the domain as
/// an error rather than swallowed.
///
/// The three completions that use this — send, capability lookup, download —
/// each carry a payload the state machine cannot invent, and each has a
/// well-defined error path: a failed send restores the composer text (spec
/// §14), failed caps leave the chip row alone, a failed download leaves the
/// affordance on Download. Dropping the completion instead would strand the
/// state that is waiting for it.
fn unexpected_response(request: &str, expected: &str, got: &TdResponse) -> TdError {
    tracing::warn!(
        request,
        expected,
        response = ?std::mem::discriminant(got),
        "td response has the wrong shape for its request"
    );
    TdError::Other {
        code: 0,
        message: format!("{request} did not answer with {expected}"),
    }
}

/// Search answers with ids or, if the mapping layer produced something else,
/// with none — an empty hit list is a value the search state has a rule for.
fn found_message_ids(response: TdResponse) -> Vec<MessageId> {
    match response {
        TdResponse::FoundMessages { message_ids } => message_ids,
        other => {
            tracing::debug!(
                response = ?std::mem::discriminant(&other),
                "searchChatMessages answered with a non-FoundMessages response; treated as no hits"
            );
            Vec::new()
        }
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

// `effect_kind` lived here to name the effect the catch-all arm dropped.
// With `Alert` wired, `dispatch` is total over `Effect` and nothing is
// dropped any more, so the helper went with the arm — and the match is left
// exhaustive on purpose: a new variant should be a compile error here, not a
// debug line nobody reads.

#[cfg(test)]
mod tests {
    use super::*;
    use tgt_core::model::chat::ChatListId;
    use tgt_core::model::entity::FormattedText;
    use tgt_core::model::ids::{MessageId, UserId};
    use tgt_core::model::message::{MessageCaps, MessageContent, SendState, Sender};
    use tgt_core::td::fake::{FakeTd, RequestMatcher, RespondWith, ScriptStep};
    use tgt_core::td::request::OutgoingFileKind;

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
        let (dispatcher, action_rx, quit_rx, _fatal_rx) = dispatcher_parts(runtime, config);
        (dispatcher, action_rx, quit_rx)
    }

    /// [`dispatcher_with`] plus the fatal-error receiver, for the tests that
    /// are about a config write failing.
    #[allow(clippy::type_complexity)]
    fn dispatcher_parts(
        runtime: Arc<dyn TdRuntime>,
        config: Config,
    ) -> (
        Dispatcher,
        mpsc::Receiver<Action>,
        watch::Receiver<bool>,
        mpsc::Receiver<human_errors::Error>,
    ) {
        let (action_tx, action_rx) = mpsc::channel(8);
        let (dispatcher, quit_rx, fatal_rx) = Dispatcher::new(
            action_tx,
            runtime,
            Arc::new(Mutex::new(config)),
            TdBootParams {
                database_directory: PathBuf::from("/tmp/tgt-test-db"),
                database_encryption_key: vec![0u8; 32],
            },
        );
        (dispatcher, action_rx, quit_rx, fatal_rx)
    }

    fn config_with_credentials() -> Config {
        Config {
            api_id: Some(1234),
            api_hash: Some("hash".to_string()),
            ..Config::default()
        }
    }

    /// A config write that fails must end the run, not be swallowed. Three
    /// things have to hold together, and they are asserted together because
    /// any one of them alone would still leave the user stranded:
    /// the error reaches the loop, no `ConfigSaved` claims success, and the
    /// deferred `SetTdlibParameters` does *not* fire — that last one is the
    /// difference between "aborts with a message" and "sits on a login
    /// screen for ever" (see `config::unwritable`).
    // The env guard is deliberately held across the `save_config` await:
    // the whole point is that `XDG_CONFIG_HOME` still points at the
    // unwritable path while the write runs. It is a `std::sync::Mutex` in a
    // single-threaded test runtime, so there is nothing for an async-aware
    // mutex to buy here.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn a_failed_config_write_aborts_the_run() {
        let _lock = crate::logging::tests::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        // A regular file where the config directory has to go, so the write
        // beneath it cannot succeed.
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, b"not a directory").unwrap();
        // SAFETY: serialized by the shared env lock held above.
        unsafe {
            crate::logging::tests::set_config_dir(&blocker);
        }

        let fake = fake_runtime("");
        let (dispatcher, mut action_rx, _quit_rx, mut fatal_rx) = dispatcher_parts(
            Arc::clone(&fake) as Arc<dyn TdRuntime>,
            config_with_credentials(),
        );

        // TDLib has already asked for its parameters and is waiting on the
        // credentials this write was supposed to persist — the exact
        // situation in which a swallowed failure strands the login screen.
        dispatcher
            .inner
            .params_pending
            .store(true, Ordering::SeqCst);

        dispatcher
            .inner
            .save_config(ConfigPatch::Credentials {
                api_id: 42,
                api_hash: "hash".to_string(),
            })
            .await;

        // SAFETY: serialized by the lock held above.
        unsafe {
            crate::logging::tests::unset_config_dir();
        }

        let err = fatal_rx
            .try_recv()
            .expect("the failure must reach the loop, or nothing ends the run");
        assert!(
            err.message().contains("couldn't save your configuration"),
            "the loop gets the actionable message, not a bare io error: {}",
            err.message()
        );

        assert!(
            action_rx.try_recv().is_err(),
            "no ConfigSaved may claim the write happened"
        );
        assert!(
            fake.received().is_empty(),
            "a failed credentials write must not send TDLib its parameters"
        );
        assert!(
            dispatcher.inner.params_pending.load(Ordering::SeqCst),
            "and the pending flag stays set rather than being consumed by a \
             request that never went out"
        );
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
    async fn send_message_completes_with_the_optimistic_message() {
        let mut optimistic = message(ChatId(9), -1);
        optimistic.send_state = SendState::Sending;
        let script = ScriptStep::Await {
            expect: RequestMatcher::Kind("SendMessageText".to_string()),
            respond: RespondWith::Ok(TdResponse::Message(optimistic.clone())),
        };
        let (dispatcher, mut action_rx, _quit_rx) = dispatcher_with(
            fake_runtime(&serde_json::to_string(&script).unwrap()),
            Config::default(),
        );

        dispatcher.dispatch(Effect::Td(TdRequest::SendMessageText {
            chat_id: ChatId(9),
            reply_to: None,
            text: FormattedText {
                text: "hi".to_string(),
                entities: Vec::new(),
            },
        }));

        let action = action_rx.recv().await.expect("completion action");
        let Action::TdResult(TdResult::MessageSent { chat_id, outcome }) = action else {
            panic!("expected MessageSent, got {action:?}");
        };
        assert_eq!(chat_id, ChatId(9));
        assert_eq!(outcome.unwrap(), optimistic);
    }

    /// The composer is holding the user's text until this completion arrives
    /// (spec §14). A response the mapping cannot read must therefore come
    /// back as an error, not as a dropped completion that strands the text.
    #[tokio::test]
    async fn send_answered_with_the_wrong_shape_is_an_error_not_a_dropped_completion() {
        // `FakeTd` answers an unscripted request with a bare `Ok`.
        let (dispatcher, mut action_rx, _quit_rx) =
            dispatcher_with(fake_runtime(""), Config::default());

        dispatcher.dispatch(Effect::Td(TdRequest::SendMessageText {
            chat_id: ChatId(9),
            reply_to: None,
            text: FormattedText {
                text: "hi".to_string(),
                entities: Vec::new(),
            },
        }));

        let action = action_rx.recv().await.expect("completion action");
        assert!(
            matches!(
                action,
                Action::TdResult(TdResult::MessageSent {
                    outcome: Err(_),
                    ..
                })
            ),
            "got {action:?}"
        );
    }

    #[tokio::test]
    async fn get_message_properties_completes_with_the_fetched_caps() {
        let caps = MessageCaps {
            can_be_edited: true,
            can_be_deleted_for_all_users: true,
            can_be_deleted_only_for_self: true,
            can_be_forwarded: true,
            can_be_saved: true,
        };
        let script = ScriptStep::Await {
            expect: RequestMatcher::Kind("GetMessageProperties".to_string()),
            respond: RespondWith::Ok(TdResponse::MessageProperties(caps)),
        };
        let (dispatcher, mut action_rx, _quit_rx) = dispatcher_with(
            fake_runtime(&serde_json::to_string(&script).unwrap()),
            Config::default(),
        );

        dispatcher.dispatch(Effect::Td(TdRequest::GetMessageProperties {
            chat_id: ChatId(9),
            message_id: MessageId(3),
        }));

        let action = action_rx.recv().await.expect("completion action");
        // Which message the caps belong to is not in the response: like
        // `getChatHistory`, it can only come from the request.
        let Action::TdResult(TdResult::MessagePropertiesLoaded {
            chat_id,
            message_id,
            outcome,
        }) = action
        else {
            panic!("expected MessagePropertiesLoaded, got {action:?}");
        };
        assert_eq!(chat_id, ChatId(9));
        assert_eq!(message_id, MessageId(3));
        assert_eq!(outcome.unwrap(), caps);
    }

    #[tokio::test]
    async fn delete_completes_as_delete_done_for_its_chat() {
        let (dispatcher, mut action_rx, _quit_rx) =
            dispatcher_with(fake_runtime(""), Config::default());

        dispatcher.dispatch(Effect::Td(TdRequest::DeleteMessages {
            chat_id: ChatId(9),
            message_ids: vec![MessageId(3)],
            revoke: true,
        }));

        assert!(matches!(
            action_rx.recv().await.expect("completion action"),
            Action::TdResult(TdResult::DeleteDone {
                chat_id: ChatId(9),
                outcome: Ok(()),
            })
        ));
    }

    /// Whether this machine has a usable clipboard is not what is under test
    /// — that the effect is executed and reports back exactly once is.
    #[tokio::test]
    async fn clipboard_effect_reports_an_io_completion() {
        let (dispatcher, mut action_rx, _quit_rx) =
            dispatcher_with(fake_runtime(""), Config::default());

        dispatcher.dispatch(Effect::CopyToClipboard {
            text: "copied".to_string(),
        });

        assert!(matches!(
            action_rx.recv().await.expect("completion action"),
            Action::Io(IoResult::ClipboardCopied { .. })
        ));
    }

    /// The opener is spawned for real. A path that cannot be opened is the
    /// safe way to prove it: the completion still has to come back, and it
    /// has to carry the path the domain asked about (nothing else in the
    /// response identifies it).
    #[tokio::test]
    async fn open_external_reports_back_with_the_path_it_was_given() {
        let (dispatcher, mut action_rx, _quit_rx) =
            dispatcher_with(fake_runtime(""), Config::default());
        let target = PathBuf::from("/nonexistent/tgt-open-external-test");

        dispatcher.dispatch(Effect::OpenExternal {
            path: target.clone(),
        });

        let action = action_rx.recv().await.expect("completion action");
        let Action::Io(IoResult::ExternalOpened { path, outcome }) = action else {
            panic!("expected ExternalOpened, got {action:?}");
        };
        assert_eq!(path, target);
        assert!(outcome.is_err(), "opening a missing file cannot succeed");
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

    /// Cancelling a download is fire-and-forget: TDLib answers `Ok` and the
    /// card only changes once the final `updateFile` says the transfer
    /// stopped, so there is deliberately no completion action here.
    #[tokio::test]
    async fn cancel_download_reaches_tdlib_without_a_completion_action() {
        let fake = fake_runtime("");
        let (dispatcher, mut action_rx, _quit_rx) =
            dispatcher_with(Arc::clone(&fake) as Arc<dyn TdRuntime>, Config::default());

        dispatcher.dispatch(Effect::Td(TdRequest::CancelDownloadFile {
            file_id: FileId(4),
        }));

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

    /// The kind core could not derive is derived here, and the request that
    /// reaches TDLib carries it.
    #[tokio::test]
    async fn send_file_reaches_tdlib_with_the_kind_its_extension_implies() {
        let dir = tempfile::tempdir().expect("tempdir");
        let clip = dir.path().join("clip.mp4");
        std::fs::write(&clip, b"not really a video").expect("write temp file");

        let fake = fake_runtime("");
        let (dispatcher, _action_rx, _quit_rx) =
            dispatcher_with(Arc::clone(&fake) as Arc<dyn TdRuntime>, Config::default());

        dispatcher.dispatch(Effect::Td(TdRequest::SendMessageFile {
            chat_id: ChatId(9),
            path: clip.clone(),
            kind: OutgoingFileKind::Document,
            caption: None,
        }));

        for _ in 0..50 {
            if !fake.received().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        let received = fake.received();
        assert!(
            matches!(
                &received[0],
                TdRequest::SendMessageFile { path, kind, .. }
                    if path == &clip && *kind == OutgoingFileKind::Video
            ),
            "got {:?}",
            received[0]
        );
    }

    /// A path that isn't there is never sent, and the send still completes —
    /// as a failure, the one shape the composer knows how to unwind (spec
    /// §14). Dropping it instead would leave the send silently unanswered.
    #[tokio::test]
    async fn send_file_with_a_missing_path_fails_without_reaching_tdlib() {
        let fake = fake_runtime("");
        let (dispatcher, mut action_rx, _quit_rx) =
            dispatcher_with(Arc::clone(&fake) as Arc<dyn TdRuntime>, Config::default());

        dispatcher.dispatch(Effect::Td(TdRequest::SendMessageFile {
            chat_id: ChatId(9),
            path: PathBuf::from("/definitely/not/here.jpg"),
            kind: OutgoingFileKind::Document,
            caption: None,
        }));

        let action = action_rx.recv().await.expect("completion action");
        assert!(
            matches!(
                action,
                Action::TdResult(TdResult::MessageSent {
                    chat_id: ChatId(9),
                    outcome: Err(_),
                })
            ),
            "got {action:?}"
        );
        assert!(
            fake.received().is_empty(),
            "a file that isn't there must not reach TDLib"
        );
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
