//! The Elm-style root: `AppState`, `Boot`, and `App::update`, the single pure
//! transition function. See docs/architecture.md §4.6.

use std::collections::HashMap;

use crate::action::{Action, TdResult};
use crate::effect::{Effect, TelemetryMode};
use crate::model::hit::{ClickButton, HitTarget, ScrollArea};
use crate::model::ids::{ChatId, MessageId};
use crate::model::key::{Key, KeyBindings};
use crate::model::message::{MessageContent, MessageView, SendState};
use crate::model::time::Millis;
use crate::state::auth::{self, AuthField, AuthState, InputField, LoginMethod};
use crate::state::chat_list::{self, ChatListState, ChatLoadPhase};
use crate::state::composer::{self, ComposerState};
use crate::state::consent::{self, ConsentChoice, ConsentState};
use crate::state::conversation::{self, ConversationState, Scroll};
use crate::state::focus::{Focus, FocusStack};
use crate::state::media::{self, MediaState};
use crate::state::modal::{self, ModalState};
use crate::state::palette::{self, PaletteState};
use crate::state::presence::{self, PresenceState};
use crate::state::search::{self, ChatSearchState};
use crate::state::selection;
use crate::state::toasts::{self, ToastState};
use crate::td::request::TdRequest;
use crate::td::update::{AuthPhase, ConnectionPhase, TdUpdate};
use crate::telemetry::{TelemetryEvent, hashing, schema};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Consent,
    Auth,
    Main,
}

#[derive(Debug)]
pub struct AppState {
    pub screen: Screen,
    pub focus: FocusStack,
    pub connection: ConnectionPhase,
    pub consent: ConsentState,
    pub auth: AuthState,
    pub chat_list: ChatListState,
    pub conversations: HashMap<ChatId, ConversationState>,
    pub open_chat: Option<ChatId>,
    pub composer: ComposerState,
    /// Transient UI state of the modal named by `Focus::Modal(_)` — its
    /// cursor, nothing more; the modal's identity and parameters live on the
    /// focus stack (`state/modal.rs` module docs, architecture §4.5).
    /// `Some` exactly while a modal is on top of the stack: `route_key`'s
    /// `sync_modal_storage` creates it on push and drops it on pop, so the
    /// two can never disagree about whether a modal is open.
    pub modal_ui: Option<ModalState>,
    pub palette: Option<PaletteState>,
    pub chat_search: Option<ChatSearchState>,
    pub toasts: ToastState,
    pub media: MediaState,
    pub presence: PresenceState,
    pub width: u16,
    pub height: u16,
    pub layout_breakpoint_cols: u16,
    pub theme_name: String,
    pub theme_generation: u64,
    pub bindings: KeyBindings,
    pub telemetry_mode: TelemetryMode,
    /// Whether this build has a crash-reporting endpoint compiled in. Read
    /// only by the consent screen's copy; see [`Boot::crash_reports_available`].
    pub crash_reports_available: bool,
    /// HMAC salt for hashed-id telemetry attributes. Generated in tgt-app.
    pub telemetry_salt: [u8; 32],
    /// Last observed tick time; the only "clock" update logic may consult.
    pub now: Millis,
}

/// Boot-time data computed impurely in tgt-app and injected as plain values.
#[derive(Debug, Clone)]
pub struct Boot {
    pub theme_name: String,
    pub bindings: KeyBindings,
    pub layout_breakpoint_cols: u16,
    pub telemetry_mode: TelemetryMode,
    pub telemetry_salt: [u8; 32],
    /// Whether this *build* can send crash reports at all — `tgt-app` reads
    /// it from `crash::build_has_dsn()`, which is false in every build made
    /// from source. Consent copy is the only consumer: a screen that offers
    /// to enable something the binary has no endpoint for would be making a
    /// promise it cannot keep, in either direction.
    pub crash_reports_available: bool,
    pub consent_needed: bool,
    pub has_credentials: bool,
    pub width: u16,
    pub height: u16,
    /// `[app] auto_download_photos` (config.rs), default on — see
    /// `state::media`'s "Auto-download" module docs.
    pub auto_download_photos: bool,
}

/// Whether the conversation pane — not the chat list — is what's actually
/// on screen right now (architecture §7.5.1's neighbor, T77 task #70).
///
/// Two-pane draws the sidebar and the conversation together regardless of
/// focus, so a chat being open is enough there. Below the breakpoint,
/// `view::root`'s single-pane stack shows the conversation only once a chat
/// is open *and* focus has left the chat list / its filter — otherwise the
/// list fills the screen and the conversation is not drawn at all. Both
/// `escape()`'s "back to the list" transition and a resize crossing the
/// breakpoint (`Action::Resize`) can flip this without the user ever
/// picking a different chat, and both need to know when they did: TDLib's
/// `openChat` governs a subscription for a chat believed to be actively
/// viewed, and this client can only ever be looking at one place at a time.
///
/// A single implementation rather than two that could drift: `view::root`
/// calls this too, in place of reimplementing the same three-field
/// comparison — `width`, `layout_breakpoint_cols` and `focus` are already
/// `AppState` fields, not a UI-only fact core would otherwise have to
/// reach for.
pub fn conversation_pane_visible(state: &AppState) -> bool {
    if state.screen != Screen::Main || state.open_chat.is_none() {
        return false;
    }
    if state.width >= state.layout_breakpoint_cols {
        return true;
    }
    !matches!(state.focus.current(), Focus::ChatList | Focus::ChatFilter)
}

#[derive(Debug)]
pub struct App {
    state: AppState,
    dirty: bool,
}

impl App {
    pub fn new(boot: Boot) -> Self {
        let screen = if boot.consent_needed {
            Screen::Consent
        } else {
            Screen::Auth
        };
        let state = AppState {
            screen,
            focus: FocusStack::new(Focus::ChatList),
            connection: ConnectionPhase::WaitingForNetwork,
            consent: ConsentState {
                selected: ConsentChoice::Enable,
                acknowledged: false,
            },
            auth: AuthState {
                phase: AuthPhase::WaitTdlibParameters,
                method: None,
                api_id: InputField::default(),
                api_hash: InputField::default(),
                phone: InputField::default(),
                code: InputField::default(),
                password: InputField::default(),
                // The credentials-wizard contract (state/auth.rs module
                // docs): the wizard has no flag of its own, it is entirely
                // driven by `active_field` starting on `ApiId`.
                active_field: if boot.has_credentials {
                    AuthField::Phone
                } else {
                    AuthField::ApiId
                },
                field_error: None,
                flood_wait_until: None,
                in_flight: false,
            },
            chat_list: ChatListState::default(),
            conversations: HashMap::new(),
            open_chat: None,
            composer: ComposerState::default(),
            modal_ui: None,
            palette: None,
            chat_search: None,
            toasts: ToastState::default(),
            media: MediaState::new(boot.auto_download_photos),
            presence: PresenceState::default(),
            width: boot.width,
            height: boot.height,
            layout_breakpoint_cols: boot.layout_breakpoint_cols,
            theme_name: boot.theme_name,
            theme_generation: 0,
            bindings: boot.bindings,
            telemetry_mode: boot.telemetry_mode,
            crash_reports_available: boot.crash_reports_available,
            telemetry_salt: boot.telemetry_salt,
            now: Millis::default(),
        };
        // Set so the first frame draws even before any action arrives.
        App { state, dirty: true }
    }

    /// THE pure transition function. No I/O, no spawning, no clock, no RNG.
    ///
    /// Telemetry is minted here rather than in the handlers (plan rule 7):
    /// one place decides what a user action is called, so two handlers can
    /// never disagree — and a chip or a modal that produces a request gets
    /// the same event as the palette entry that produces it later.
    /// [`Self::telemetry_for`] reads state and returns a value; nothing about
    /// it is impure.
    pub fn update(&mut self, action: Action) -> Vec<Effect> {
        let mut effects = self.dispatch(action);
        let events: Vec<Effect> = effects
            .iter()
            .filter_map(|effect| self.telemetry_for(effect))
            .map(Effect::Telemetry)
            .collect();
        effects.extend(events);
        effects
    }

    fn dispatch(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::Tick { now } => {
                // Caching the clock is not itself render-worthy: only a
                // handler that actually changed something sets dirty.
                self.state.now = now;
                let flood_wait_before = self.state.auth.flood_wait_until;
                let mut effects = auth::handle_tick(&mut self.state, now);
                if self.state.auth.flood_wait_until != flood_wait_before {
                    self.dirty = true;
                }
                let typing_before = self.state.presence.typing.len();
                effects.extend(presence::handle_tick(&mut self.state, now));
                if self.state.presence.typing.len() != typing_before {
                    self.dirty = true;
                }
                // Toast expiry (spec §6.4's 4 s TTL). Same rule as the two
                // sweeps above: a tick that expires nothing is not a frame.
                let toasts_before = self.state.toasts.toasts.len();
                effects.extend(toasts::handle_tick(&mut self.state, now));
                if self.state.toasts.toasts.len() != toasts_before {
                    self.dirty = true;
                }
                // T72: the retry path for a `ViewMessages` TDLib never
                // answered (see `conversation::handle_tick`). Deliberately
                // not a `dirty` trigger — whether the badge clears is
                // TDLib's `updateChatReadInbox` to decide, and that update
                // sets `dirty` on its own.
                effects.extend(conversation::handle_tick(&mut self.state));
                effects
            }
            Action::Resize { width, height } => {
                // Crossing the breakpoint downward with focus still on the
                // chat list can stop rendering the open chat without any
                // key press at all (task #70): a resize is exactly as
                // capable of hiding the conversation as `Esc` is, and
                // `conversation_pane_visible` reads `width` itself, so the
                // snapshot has to come from *before* it changes below.
                let was_visible = conversation_pane_visible(&self.state);
                self.state.width = width;
                self.state.height = height;
                self.dirty = true;
                conversation::close_if_now_hidden(&self.state, was_visible)
            }
            Action::Key(key) => self.route_key(key),
            // Bracketed paste. `handle_paste` decides between inserting the
            // text and holding it as a send-file offer, and enforces its own
            // claim rules (composer focused, a chat open) — a paste arriving
            // anywhere else is a no-op there. Dirty is set either way: a
            // claimed paste always changes the input buffer or the offer,
            // and detecting the unclaimed case just to skip one redundant
            // frame is not worth the bookkeeping.
            Action::Paste(text) => {
                composer::handle_paste(&mut self.state, text);
                self.dirty = true;
                Vec::new()
            }
            Action::Click { target, button } => self.dispatch_click(target, button),
            Action::Scroll { area, up } => self.dispatch_scroll(area, up),
            Action::Td(update) => self.route_td(&update),
            Action::TdResult(TdResult::AuthRequestDone { outcome }) => {
                // Always render-worthy: at minimum the in-flight spinner
                // clears, and usually an inline error or countdown appears.
                self.dirty = true;
                auth::handle_td_result(&mut self.state, &outcome)
            }
            Action::TdResult(TdResult::HistoryLoaded {
                chat_id,
                only_local,
                outcome,
            }) => {
                // The page itself, an emptied retry round and a cooldown all
                // change what the viewport shows or how it behaves.
                self.dirty = true;
                conversation::apply_history_page(&mut self.state, chat_id, only_local, &outcome)
            }
            Action::TdResult(TdResult::ChatsLoaded { outcome }) => {
                // `loadChats` only reports that TDLib accepted the request;
                // the chats themselves arrive as `NewChat`/`ChatPosition`
                // pushes. A failure leaves the phase alone: the list keeps
                // whatever it already holds and the error is the runtime's
                // to log, not this function's to interpret.
                if outcome.is_ok() {
                    self.state.chat_list.load = ChatLoadPhase::Complete;
                    self.dirty = true;
                }
                Vec::new()
            }
            // The send RPC returned. Two halves: the composer's is the held
            // text (dropped on success, restored on failure — spec §14), the
            // window's is architecture §5.2's optimistic append of the
            // message TDLib just minted with a temporary id, so the user sees
            // it before the confirmation push arrives.
            //
            // The append is routed through `conversation::handle_td`'s
            // existing `NewMessage` arm rather than through a second append
            // path: it is the same operation (dedupe by id, insert in id
            // order, evict to the window bound, drop a dangling selection),
            // and TDLib pushes `updateNewMessage` for this very message as
            // well — which that dedupe is exactly what makes harmless.
            Action::TdResult(result @ TdResult::MessageSent { .. }) => {
                self.dirty = true;
                let mut effects = composer::handle_td_result(&mut self.state, &result);
                if let TdResult::MessageSent {
                    chat_id,
                    outcome: Ok(view),
                } = &result
                {
                    effects.extend(conversation::handle_td(
                        &mut self.state,
                        &TdUpdate::NewMessage(view.clone()),
                    ));
                    start_tracking_upload(&mut self.state, *chat_id, view);
                }
                effects
            }
            // Capability flags for the message selection mode landed on
            // (architecture §7). An `Err` deliberately keeps the chips the
            // user is looking at, so it is still worth routing.
            Action::TdResult(TdResult::MessagePropertiesLoaded {
                chat_id,
                message_id,
                outcome,
            }) => {
                self.dirty = true;
                selection::handle_td_result(&mut self.state, chat_id, message_id, &outcome)
            }
            // `DownloadFile`'s answer: on `Ok` the file table gets its first
            // (or updated) snapshot — same shape as the `updateFile` push
            // below, so always render-worthy either way (a failure at least
            // clears the optimistic `is_downloading` bit, per
            // `media::handle_td_result`'s doc comment).
            Action::TdResult(TdResult::DownloadStarted { file_id, outcome }) => {
                self.dirty = true;
                media::handle_td_result(&mut self.state, file_id, &outcome)
            }
            // `searchChatMessages` answered. Render-worthy either way: `Ok`
            // stores the hits and drags the anchor to the first one, `Err`
            // at least clears the query box's in-flight spinner (T42).
            Action::TdResult(TdResult::SearchDone { chat_id, outcome }) => {
                self.dirty = true;
                search::handle_td_result(&mut self.state, chat_id, &outcome)
            }
            // `editMessageText`/`deleteMessages`/`toggleReaction` answer with
            // nothing but success or failure — the visible change, if any,
            // arrives as its own push (`MessageContentChanged`,
            // `MessagesDeleted`, a reaction update). So `Ok` sets no toast
            // and touches no state; it does mint the one telemetry event
            // this action gets, now that `telemetry_for` no longer fires an
            // optimistic `ok` for these three at request time (see that
            // function's doc comment). Reporting outcome only once, from
            // here, keeps one action attempt mapped to one event — the
            // alternative (an `ok` at request time plus an `error`
            // correction here on failure) inflates both counts in any
            // consumer that sums outcomes: 10 attempts with 1 failure would
            // read as 11 events (10 ok, 1 error) instead of 10 (9 ok, 1
            // error), understating the true failure rate and getting worse
            // as it climbs.
            //
            // `Err` is the case that had nowhere to go before this task: a
            // toast (`toasts::on_action_failed`, reusing the queue
            // `on_new_message` already built — all three carry a real
            // `chat_id`, so no shape change) and the `error` half of the
            // outcome pair.
            Action::TdResult(TdResult::EditDone {
                chat_id, outcome, ..
            }) => {
                let action = schema::actions::MESSAGE_EDIT;
                match &outcome {
                    Ok(()) => {
                        vec![Effect::Telemetry(
                            self.chat_event(TelemetryEvent::ok(action), chat_id),
                        )]
                    }
                    Err(err) => {
                        self.dirty = true;
                        let mut effects = vec![Effect::Telemetry(self.chat_event(
                            TelemetryEvent::error(action, err.telemetry_kind()),
                            chat_id,
                        ))];
                        effects.extend(toasts::on_action_failed(
                            &mut self.state,
                            chat_id,
                            "Couldn't save the edit".to_string(),
                        ));
                        effects
                    }
                }
            }
            Action::TdResult(TdResult::DeleteDone { chat_id, outcome }) => {
                let action = schema::actions::MESSAGE_DELETE;
                match &outcome {
                    Ok(()) => {
                        vec![Effect::Telemetry(
                            self.chat_event(TelemetryEvent::ok(action), chat_id),
                        )]
                    }
                    Err(err) => {
                        self.dirty = true;
                        let mut effects = vec![Effect::Telemetry(self.chat_event(
                            TelemetryEvent::error(action, err.telemetry_kind()),
                            chat_id,
                        ))];
                        effects.extend(toasts::on_action_failed(
                            &mut self.state,
                            chat_id,
                            "Couldn't delete the message".to_string(),
                        ));
                        effects
                    }
                }
            }
            Action::TdResult(TdResult::ReactionDone {
                chat_id, outcome, ..
            }) => {
                let action = schema::actions::MESSAGE_REACT;
                match &outcome {
                    Ok(()) => {
                        vec![Effect::Telemetry(
                            self.chat_event(TelemetryEvent::ok(action), chat_id),
                        )]
                    }
                    Err(err) => {
                        self.dirty = true;
                        let mut effects = vec![Effect::Telemetry(self.chat_event(
                            TelemetryEvent::error(action, err.telemetry_kind()),
                            chat_id,
                        ))];
                        effects.extend(toasts::on_action_failed(
                            &mut self.state,
                            chat_id,
                            "Couldn't send the reaction".to_string(),
                        ));
                        effects
                    }
                }
            }
            // Forward could not make the same move as the three above: `Ok`
            // stays a genuine no-op (the optimistic event `telemetry_for`
            // already emitted at request time, tagged with `from_chat_id`,
            // is this action's only `ok` — minting a second one here on
            // success would be exactly the double-count this task is
            // trying to avoid elsewhere). `ForwardDone` carries only
            // `to_chat_id`; replacing the request-time event with one from
            // here would silently swap what a forward event's chat_hash
            // means (source vs. destination), which is a bigger, more
            // product-facing change than this fix and not this task's to
            // make unasked. So `Err` is reported from here, the only place
            // it exists, tagged by the destination chat — the one thing
            // this completion actually carries.
            Action::TdResult(TdResult::ForwardDone {
                to_chat_id,
                outcome,
            }) => match outcome {
                Ok(()) => Vec::new(),
                Err(err) => {
                    self.dirty = true;
                    let action = schema::actions::MESSAGE_FORWARD;
                    let mut effects = vec![Effect::Telemetry(self.chat_event(
                        TelemetryEvent::error(action, err.telemetry_kind()),
                        to_chat_id,
                    ))];
                    effects.extend(toasts::on_action_failed(
                        &mut self.state,
                        to_chat_id,
                        "Couldn't forward the message".to_string(),
                    ));
                    effects
                }
            },
            // `Action::Io(_)` and `TdResult::LogOutDone` stay dropped here,
            // deliberately, past this task's scope: `ConfigSaved` and
            // `LogOutDone` failures want either `Toast.chat_id` widened to
            // `Option<ChatId>` (a shared type — docs/architecture.md first)
            // or a second, chat-less notification path, and logout has no
            // allowlisted telemetry action yet at all, unlike the four
            // above which already had one firing optimistically. Those are
            // product decisions, not wiring.
            _ => Vec::new(),
        }
    }

    /// Spec §6.2's routing table: modal → focused pane → global, first
    /// claimant stops propagation.
    fn route_key(&mut self, key: Key) -> Vec<Effect> {
        let claimed = self.dispatch_key(key);
        self.sync_modal_storage();
        claimed.unwrap_or_default()
    }

    /// One walk down the table. `None` means nothing claimed the key: it
    /// reached the bottom unconsumed. The global layer at that bottom owns
    /// `ctrl+p` (the palette) and the conversation half of `/`; `?` (help)
    /// still falls through until T47 builds the overlay behind it.
    ///
    /// The quit binding is checked ahead of all of it, deliberately. A pane
    /// claims every key it is shown for — the auth wizard is a full-screen
    /// text form, so `state::auth::handle_key` returns `Some` for anything
    /// while `Screen::Auth` is up — and routing `ctrl+c` through it first
    /// would leave a half-finished login unquittable. The interrupt key is
    /// reserved, not routable.
    fn dispatch_key(&mut self, key: Key) -> Option<Vec<Effect>> {
        if key == self.state.bindings.quit {
            return Some(vec![Effect::Quit]);
        }

        // 0. The consent screen is a gate in front of everything else,
        //    `Screen::Auth` included (spec §13.5: shown before login and
        //    before any data is sent). It claims every key while
        //    `Screen::Consent` is up and nothing once it isn't, so no pane,
        //    modal or global binding below is ever reachable from it.
        if let Some(effects) = consent::handle_key(&mut self.state, key) {
            self.dirty = true;
            return Some(effects);
        }

        // 1. Modal. Top of the table because a modal swallows every key it is
        //    shown: nothing below it, pane or global, sees one while it is up.
        if matches!(self.state.focus.current(), Focus::Modal(_)) {
            return Some(self.route_modal_key(key));
        }

        // 2. The auth wizard is a screen, not a pane: it claims everything
        //    while `Screen::Auth` is up (including `Esc`, which is why it
        //    sits above the generic pop) and nothing once it isn't.
        if let Some(effects) = auth::handle_key(&mut self.state, key) {
            self.dirty = true;
            return Some(effects);
        }

        // 3. `Esc` above the panes: every M4 handler deliberately returns
        //    `None` for it (chat_list while filtering, selection mode) so
        //    that the one stack rule lives in one place.
        if key == Key::Esc {
            return self.escape();
        }

        if self.state.screen != Screen::Main {
            return None;
        }

        // 4. The focused pane.
        if let Some(effects) = self.route_pane_key(key) {
            self.dirty = true;
            return Some(effects);
        }

        // 5. The conversation viewport sits *under* the composer and the chip
        //    row rather than beside them, so its scroll keys are reachable
        //    from both without a focus move: `PageUp`/`PageDown` always land
        //    here (neither the composer nor selection mode claims them), and
        //    so does a plain `Down` the composer left alone. `Up` never does
        //    — the composer claims it for the caret or for entering selection
        //    mode (spec §6.2), and selection mode claims it for the cursor.
        //    This needs no change to `conversation::handle_key`'s own claim
        //    rule: "a chat is open and focus isn't the chat list" is implied
        //    by the two focuses routed here.
        if matches!(
            self.state.focus.current(),
            Focus::Composer | Focus::Selection
        ) && let Some(effects) = conversation::handle_key(&mut self.state, key)
        {
            self.dirty = true;
            return Some(effects);
        }

        // 6. Pane movement.
        if self.move_pane_focus(key) {
            self.dirty = true;
            return Some(Vec::new());
        }

        // 7. Global. Reached only by keys no pane above claimed, which is
        //    why the palette opens from any of them — and why a modal, the
        //    one layer that swallows everything, is the single context it
        //    cannot be opened from (spec §6.2).
        if key == self.state.bindings.palette {
            self.dirty = true;
            return Some(self.toggle_palette());
        }

        // `?` help (spec §6.2, global). Reached only when no pane claimed the
        // key, so typing `?` into the composer or a filter still inserts the
        // character; from the chat list or selection mode it opens the
        // overlay. `Esc` closes it through the generic pop in `escape`.
        if key == self.state.bindings.help && !matches!(self.state.focus.current(), Focus::Help) {
            self.state.focus.push(Focus::Help);
            self.dirty = true;
            return Some(Vec::new());
        }

        // `/` is context-dependent (spec §11). Its chat-list half — the
        // filter — was claimed by `chat_list::handle_key` two steps up; this
        // is the other half: in-chat message search, "bound to `/` while the
        // message list is focused". Selection mode IS that focus (there is
        // no separate message-list level, see `route_chat_list_key`), and it
        // leaves `/` unclaimed because no action chip answers to it.
        //
        // Deliberately NOT the composer, though the composer is the
        // conversation side's resting focus: `/` there is a literal
        // character. It opens `/send <path>` (spec §10) from exactly the
        // empty input a search binding would have to claim, and every
        // message containing a slash — a URL, a path, a date — is typed
        // through it. A text field that let `/` escape it would be the worse
        // trade, the same call `move_pane_focus` makes for `←`.
        if key == Key::Char('/')
            && matches!(self.state.focus.current(), Focus::Selection)
            && self.open_chat_search()
        {
            self.dirty = true;
            return Some(Vec::new());
        }

        None
    }

    /// `ctrl+p`. Opening pushes `Focus::Palette` and lets T41 populate the
    /// overlay; pressing it again with the palette up is the way back out,
    /// so the binding that opens the palette is also the one that closes it
    /// rather than a key that stacks a second one on the first.
    ///
    /// The one telemetry event this task mints from a projection rather than
    /// from a request: opening the palette produces no `Effect` to key off
    /// (nothing is asked of TDLib), and `palette.open` is still an
    /// allowlisted user action — the same shape as `route_td`'s login event.
    fn toggle_palette(&mut self) -> Vec<Effect> {
        if matches!(self.state.focus.current(), Focus::Palette) {
            self.state.focus.pop();
            palette::close(&mut self.state);
            return Vec::new();
        }
        self.state.focus.push(Focus::Palette);
        palette::open(&mut self.state);
        vec![Effect::Telemetry(TelemetryEvent::ok(
            schema::actions::PALETTE_OPEN,
        ))]
    }

    /// Enters in-chat search from selection mode. Returns whether it opened —
    /// there is nothing to search with no chat open.
    ///
    /// Selection mode is left behind rather than kept underneath: search
    /// moves the scroll anchor to each hit (T42), and a highlighted message
    /// the user can no longer see would be fighting it for the viewport. So
    /// the selection level is exited (T26's `exit` half, exactly as the
    /// generic `Esc` path and the Reply chip run it) and `Focus::ChatSearch`
    /// takes its place above the composer. `Esc` out of search therefore
    /// lands on the composer, not back on a stale selection.
    fn open_chat_search(&mut self) -> bool {
        if self.state.open_chat.is_none() {
            return false;
        }
        selection::exit(&mut self.state);
        self.state.focus.pop();
        self.state.focus.push(Focus::ChatSearch);
        search::open(&mut self.state);
        true
    }

    /// Dispatches to whichever pane is on top of the focus stack, and runs
    /// the focus transitions the handlers deliberately leave to the router
    /// (an `Effect` list cannot express a focus change and shouldn't).
    fn route_pane_key(&mut self, key: Key) -> Option<Vec<Effect>> {
        match self.state.focus.current().clone() {
            Focus::Selection => self.route_selection_key(key),
            Focus::Composer => self.route_composer_key(key),
            Focus::ChatList | Focus::ChatFilter => self.route_chat_list_key(key),
            Focus::Palette => self.route_palette_key(key),
            // T42 keeps its own focus check inside `handle_key`, so this is
            // the whole wiring: `Esc` is unclaimed there and comes back out
            // of `escape`, which pops this level and calls `search::close`.
            Focus::ChatSearch => search::handle_key(&mut self.state, key),
            // The help overlay swallows everything it is shown (`Esc` never
            // gets here — step 3 pops it via `escape` like any level).
            Focus::Help => Some(Vec::new()),
            // `Modal` was handled before any pane ran.
            _ => None,
        }
    }

    /// The palette. T41's contract splits the close between the two of us:
    /// `handle_key` never touches the focus stack, and `Enter` drops
    /// `app.palette` itself — so `Some` → `None` across the call is the
    /// signal to pop the level `toggle_palette` pushed. `Esc` never gets
    /// here (it is unclaimed there, and intercepted above); it closes the
    /// palette through `escape` instead.
    ///
    /// Invoking a chat entry moves focus the same way `⏎` on a sidebar row
    /// does — the palette is a second door into the same conversation, and
    /// landing on it with the chat list still focused would be a different
    /// outcome for the same intent.
    fn route_palette_key(&mut self, key: Key) -> Option<Vec<Effect>> {
        let open_before = self.state.open_chat;
        let effects = palette::handle_key(&mut self.state, key)?;
        if self.state.palette.is_none() {
            self.state.focus.pop();
            if self.state.open_chat != open_before && self.state.open_chat.is_some() {
                self.state.focus.replace_base(Focus::Composer);
            }
        }
        Some(effects)
    }

    /// Selection mode. Two chips — Reply and Edit — are pure composer-context
    /// moves: T26 arms the composer and documents that the focus move back is
    /// the router's, so a newly set `reply_to`/`editing` is the signal to pop
    /// out of selection mode and let the user type.
    fn route_selection_key(&mut self, key: Key) -> Option<Vec<Effect>> {
        let reply_before = self.state.composer.reply_to;
        let editing_before = self.state.composer.editing;
        let effects = selection::handle_key(&mut self.state, key)?;

        let composer_armed = (self.state.composer.reply_to.is_some()
            && self.state.composer.reply_to != reply_before)
            || (self.state.composer.editing.is_some()
                && self.state.composer.editing != editing_before);
        if composer_armed {
            selection::exit(&mut self.state);
            self.state.focus.pop();
            if *self.state.focus.current() != Focus::Composer {
                // Selection mode is only ever entered from the composer in
                // M4. Should a later task push it from somewhere else, the
                // armed composer still gets the focus it was armed for
                // instead of the chip silently doing nothing.
                self.state.focus.replace_base(Focus::Composer);
            }
        }
        Some(effects)
    }

    /// The composer. `↑` on an empty input pushes `Focus::Selection` (T25);
    /// running T26's entry lifecycle behind that push is the router's job,
    /// and its effects join the composer's in the same batch.
    fn route_composer_key(&mut self, key: Key) -> Option<Vec<Effect>> {
        let mut effects = composer::handle_key(&mut self.state, key)?;
        if *self.state.focus.current() == Focus::Selection {
            effects.extend(selection::enter(&mut self.state));
            if !self.selection_is_active() {
                // Nothing loaded to select. Rather than leave an empty
                // selection mode focused — chips row blank, every key
                // unclaimed — the push is undone and `↑` is a no-op.
                self.state.focus.pop();
            }
        }
        Some(effects)
    }

    /// The chat list (and its `/` filter level, which the same handler owns).
    fn route_chat_list_key(&mut self, key: Key) -> Option<Vec<Effect>> {
        let open_before = self.state.open_chat;
        let effects = chat_list::handle_key(&mut self.state, key)?;

        if self.state.open_chat != open_before && self.state.open_chat.is_some() {
            // `⏎` opened a chat: the conversation side takes focus, whose
            // resting level is the composer (spec §6.2 — typing goes to the
            // composer, `↑` on an empty input enters selection mode; there is
            // no separate "message list" focus).
            //
            // `replace_base`, not `push`: the two panes are siblings, not
            // nested contexts. A pushed level would put the chat list one
            // `Esc` below the composer forever, and pane movement — which
            // only runs at depth 1, see `move_pane_focus` — could never
            // reach the list again. `Esc` still goes back to the list; it is
            // handled as a base swap in `escape`.
            //
            // The window bookkeeping `conversation::open` performs
            // (`conversations` entry + `open_chat`) is already done by
            // `chat_list::open_selected`, which inlines it, so calling it
            // here would be a no-op.
            self.state.focus.replace_base(Focus::Composer);
        }
        Some(effects)
    }

    /// A modal has focus. `⏎` confirms and `esc` dismisses — both close it;
    /// every other key is claimed and swallowed with the modal left standing.
    /// The close decision is keyed off the key rather than off the returned
    /// effects because a confirm can legitimately produce none (T27's
    /// `ConfirmSendFile` until T39 wires the upload).
    ///
    /// `ModalState` lives in `AppState::modal_ui` but `modal::handle_key`
    /// takes it as a second `&mut` (see that module's docs), so it is moved
    /// out for the call and put back after — the cursor has to survive the
    /// arrow keys that move it.
    fn route_modal_key(&mut self, key: Key) -> Vec<Effect> {
        let before = self.state.modal_ui.unwrap_or_default();
        let mut modal_ui = before;
        self.state.modal_ui = None;
        let effects = modal::handle_key(&mut self.state, &mut modal_ui, key).unwrap_or_default();

        let closing = matches!(key, Key::Enter | Key::Esc);
        if closing {
            self.state.focus.pop();
        } else {
            self.state.modal_ui = Some(modal_ui);
        }
        if closing || modal_ui != before {
            self.dirty = true;
        }
        effects
    }

    /// `Esc` pops exactly one level and never the base (architecture §4.5).
    /// Returns `None` if unclaimed, mirroring every `route_*_key` below —
    /// this used to just report whether it was claimed, but the single-pane
    /// "back to the list" transition can now emit `CloseChat` (task #70), so
    /// it needs the same effect-carrying shape as the rest of the table.
    ///
    /// Two things sit above the pop rule, in this order:
    ///
    /// 1. **A toast, if any is showing** (spec §6.4: toasts are "dismissible
    ///    with `esc`"). They are not on the focus stack at all, so the two
    ///    rules had to be ordered by hand: `Esc` peels the newest toast
    ///    first and consumes the key. A toast is the most transient thing on
    ///    screen — it leaves on its own after 4 s — so dismissing one costs
    ///    the user a keypress at worst, while the reverse order would make
    ///    `esc` unable to clear a toast at all whenever any overlay is up.
    ///    A modal is the exception, and it never reaches here: it swallows
    ///    every key it is shown, `Esc` included (`dispatch_key` step 1).
    /// 2. **Backing out of the archive** (T43): with the chat list focused
    ///    and `active_list == Archive`, `chat_list::handle_key` claims `Esc`
    ///    to return to `Main`. That handler sits *below* this one in the
    ///    table, so the archive case is asked here explicitly — otherwise
    ///    the generic rule below would run first and `Esc` would leave the
    ///    conversation instead of the archive.
    fn escape(&mut self) -> Option<Vec<Effect>> {
        if toasts::dismiss_newest(&mut self.state) {
            self.dirty = true;
            return Some(Vec::new());
        }
        if matches!(self.state.focus.current(), Focus::ChatList)
            && chat_list::handle_key(&mut self.state, Key::Esc).is_some()
        {
            self.dirty = true;
            return Some(Vec::new());
        }

        // Snapshot before the match: only the `_` arm's `replace_base` can
        // possibly change it (module docs), but reading it once up here
        // rather than duplicated in that one arm keeps the diff-and-close
        // pattern identical to `Action::Resize`'s.
        let was_visible = conversation_pane_visible(&self.state);
        let mut effects = Vec::new();

        match self.state.focus.current().clone() {
            // Leaving selection mode drops the selection: T26 splits the
            // lifecycle into `enter`/`exit` precisely so the router can run
            // the second half on the generic pop path.
            Focus::Selection => {
                selection::exit(&mut self.state);
                self.state.focus.pop();
            }
            // The `/` filter. T15's convention: `⏎` commits (its handler pops
            // and the filter stays applied), so `Esc` is the one that has to
            // cancel — popping without clearing would leave the list filtered
            // by text nothing is focused on any more.
            Focus::ChatFilter => {
                self.state.chat_list.filter = None;
                self.state.focus.pop();
            }
            // The two M7 overlays. Both leave `Esc` unclaimed on purpose so
            // that the pop and the state teardown stay in one place: T41's
            // `close` drops the query and its results, T42's also clears the
            // open chat's hits, which is what turns the highlight off.
            Focus::Palette => {
                self.state.focus.pop();
                palette::close(&mut self.state);
            }
            Focus::ChatSearch => {
                self.state.focus.pop();
                search::close(&mut self.state);
            }
            _ => {
                if self.state.focus.pop() {
                } else if self.state.screen == Screen::Main
                    && *self.state.focus.current() == Focus::Composer
                {
                    // At the base with the conversation side focused. `Esc`
                    // means "back to the chat list" here — the back button of
                    // the single-pane stack layout (spec §6.1) and the way
                    // out of the conversation in the two-pane one. The base
                    // is swapped, not popped: the stack floor still holds.
                    //
                    // In two-pane this changes nothing `conversation_pane_
                    // visible` cares about (it stays visible regardless of
                    // focus there); in single-pane it is exactly the
                    // transition that stops rendering the conversation, so
                    // the close check below is what actually fires here.
                    self.state.focus.replace_base(Focus::ChatList);
                } else {
                    return None;
                }
            }
        }
        effects.extend(conversation::close_if_now_hidden(&self.state, was_visible));
        self.dirty = true;
        Some(effects)
    }

    /// Pane movement (spec §6.2): `←`/`→` move, `tab`/`shift+tab` cycle.
    /// M4 has two panes — the chat list and the conversation side — so the
    /// cycle is a toggle and `shift+tab` walks the same pair in the opposite
    /// order; the direction only becomes observable when a third pane lands.
    ///
    /// Movement runs at depth 1 only. Anything deeper is an overlay above the
    /// panes (the filter, selection mode, a modal, later the palette), and
    /// swapping the pane underneath one would leave the overlay sitting on a
    /// pane it does not belong to.
    ///
    /// Of §6.2's two movement arrows only `→` survives contact with the
    /// panes it moves between: `←` in the composer is the caret key (T25) and
    /// in selection mode it walks the chip row (T26), and a text field that
    /// let a cursor key escape it would be the worse trade. Leaving the
    /// conversation side is therefore `tab` or `esc`, both of which always
    /// work.
    fn move_pane_focus(&mut self, key: Key) -> bool {
        if self.state.focus.depth() != 1 {
            return false;
        }
        let target = match (self.state.focus.current(), key) {
            (Focus::ChatList, Key::Right | Key::Tab | Key::BackTab) => Focus::Composer,
            (Focus::Composer, Key::Tab | Key::BackTab) => Focus::ChatList,
            _ => return false,
        };
        // Focusing the conversation side with no chat open would strand the
        // cursor on a pane where nothing claims a key.
        if target == Focus::Composer && self.state.open_chat.is_none() {
            return false;
        }
        self.state.focus.replace_base(target);
        true
    }

    /// Keeps `modal_ui` in step with the focus stack: a modal pushed by a
    /// handler (T26's Delete chip today, T39's send-file offer later) gets a
    /// fresh cursor, and a stack with no modal on top carries no modal state.
    /// Deliberately not a reset per keystroke — `route_modal_key` writes the
    /// moved cursor back and this must not clobber it.
    fn sync_modal_storage(&mut self) {
        match self.state.focus.current() {
            Focus::Modal(_) => {
                if self.state.modal_ui.is_none() {
                    self.state.modal_ui = Some(ModalState::default());
                }
            }
            _ => self.state.modal_ui = None,
        }
    }

    /// Mouse routing (architecture §7.5). Hit-testing already happened at
    /// the `tgt-ui` boundary — `Action::Click`/`Action::Scroll` name a
    /// semantic target, never a coordinate — so this is pure `update()`
    /// dispatch like every other action, just keyed off a target instead of
    /// a key.
    ///
    /// Overlays are keyboard-only for now (spec delegates the mouse story to
    /// this task alone): a modal, the palette or help swallow every click
    /// and every scroll exactly as they swallow every key, and nothing off
    /// `Screen::Main` accepts either.
    fn mouse_blocked(&self) -> bool {
        self.state.screen != Screen::Main
            || matches!(
                self.state.focus.current(),
                Focus::Modal(_) | Focus::Palette | Focus::Help
            )
    }

    fn dispatch_click(&mut self, target: HitTarget, button: ClickButton) -> Vec<Effect> {
        if self.mouse_blocked() {
            return Vec::new();
        }
        match (target, button) {
            (HitTarget::ChatRow(chat_id), ClickButton::Left) => self.click_chat_row(chat_id),
            (HitTarget::ArchiveRow, ClickButton::Left) => {
                chat_list::toggle_archive(&mut self.state);
                self.dirty = true;
                Vec::new()
            }
            (HitTarget::FolderTab(list), ClickButton::Left) => {
                self.state.chat_list.active_list = list;
                // Mirrors `chat_list::reset_selection_to_first_visible`
                // (private to that module): selection resets to the new
                // list's first visible row rather than carrying over a
                // selection that belongs to the list just left.
                self.state.chat_list.selected = chat_list::visible_rows(&self.state.chat_list)
                    .first()
                    .copied();
                self.dirty = true;
                Vec::new()
            }
            (HitTarget::Composer, ClickButton::Left) => {
                if self.state.open_chat.is_some() {
                    self.state.focus.replace_base(Focus::Composer);
                    self.dirty = true;
                }
                Vec::new()
            }
            (HitTarget::Message(message_id), ClickButton::Right) => {
                self.click_message_right(message_id)
            }
            // Sub-row targets (architecture §7.5.1, T77). Right-click on
            // either mirrors plain `Message`'s: it enters selection on the
            // message the *row* belongs to, never on `ReplyQuote`'s quoted
            // message, which is why that variant carries both ids.
            (HitTarget::Spoiler(message_id), ClickButton::Left) => {
                let Some(chat_id) = self.state.open_chat else {
                    return Vec::new();
                };
                self.dirty = true;
                conversation::reveal_spoilers(&mut self.state, chat_id, message_id)
            }
            (HitTarget::Spoiler(message_id), ClickButton::Right) => {
                self.click_message_right(message_id)
            }
            (HitTarget::ReplyQuote { quoted, .. }, ClickButton::Left) => {
                let Some(chat_id) = self.state.open_chat else {
                    return Vec::new();
                };
                self.dirty = true;
                conversation::jump_to_message(&mut self.state, chat_id, quoted)
            }
            (HitTarget::ReplyQuote { containing, .. }, ClickButton::Right) => {
                self.click_message_right(containing)
            }
            // Left-click on a message, and right-click on anything else,
            // are no-ops in v1 (architecture §7.5): claimed (nothing falls
            // through to a pane below, there is none left to fall to), but
            // nothing changes.
            _ => Vec::new(),
        }
    }

    /// Left-click on a sidebar row: the same open path `⏎` takes. Driven
    /// through `chat_list::handle_key`'s real `Enter` arm rather than
    /// re-implemented here — a `Focus::ChatList` level is pushed just for
    /// the call (that handler only *reads* the focus to decide it applies;
    /// it never touches the stack itself) and popped straight back off, so
    /// the bracket leaves the stack exactly as it found it. The focus
    /// transition afterward is the one `route_chat_list_key` runs for `⏎`.
    fn click_chat_row(&mut self, chat_id: ChatId) -> Vec<Effect> {
        self.state.chat_list.selected = Some(chat_id);
        self.state.focus.push(Focus::ChatList);
        let effects = chat_list::handle_key(&mut self.state, Key::Enter).unwrap_or_default();
        self.state.focus.pop();
        if self.state.open_chat.is_some() {
            self.state.focus.replace_base(Focus::Composer);
        }
        self.dirty = true;
        effects
    }

    /// Right-click on a message: enters selection mode on exactly that
    /// message, or — already in selection mode — just moves the cursor
    /// there. Mirrors `route_composer_key`'s `↑`-on-empty wiring: the focus
    /// push this function makes speculatively is undone if
    /// `selection::enter_at` came back with nothing selected (message not
    /// loaded, or no chat open at all), and a push is skipped entirely when
    /// selection mode is already up so a stray click can't stack levels.
    fn click_message_right(&mut self, message_id: MessageId) -> Vec<Effect> {
        let already_selecting = matches!(self.state.focus.current(), Focus::Selection);
        if !already_selecting {
            self.state.focus.push(Focus::Selection);
        }
        let effects = selection::enter_at(&mut self.state, message_id);
        if !already_selecting && !self.selection_is_active() {
            self.state.focus.pop();
        }
        self.dirty = true;
        effects
    }

    fn dispatch_scroll(&mut self, area: ScrollArea, up: bool) -> Vec<Effect> {
        if self.mouse_blocked() {
            return Vec::new();
        }
        let effects = match area {
            ScrollArea::ChatList => self.scroll_chat_list(up),
            ScrollArea::Conversation => self.scroll_conversation(up),
        };
        self.dirty = true;
        effects
    }

    /// Wheel over the sidebar: moves the viewport (`chat_list.scroll_offset`)
    /// and leaves `chat_list.selected` alone — the wheel is for looking
    /// around, the selection is where the user's intent is pointed, and
    /// conflating the two was this task's bug (a mouse wheel used to move
    /// the selection like `↑`/`↓`). Claimed independently of focus, same as
    /// every other `ScrollArea` arm, so no `Focus::ChatList` push/pop is
    /// needed here — unlike [`Self::click_chat_row`], this never calls into
    /// key handling.
    fn scroll_chat_list(&mut self, up: bool) -> Vec<Effect> {
        chat_list::scroll_viewport(&mut self.state, up);
        Vec::new()
    }

    /// Wheel over the conversation viewport: extends
    /// [`Self::scroll_conversation_move`]'s anchor step with T66's
    /// auto-download trigger — every anchor move, wheel included, changes
    /// what's near it.
    fn scroll_conversation(&mut self, up: bool) -> Vec<Effect> {
        let chat_id = self.state.open_chat;
        let mut effects = self.scroll_conversation_move(up);
        if let Some(chat_id) = chat_id {
            effects.extend(media::auto_download_photos(&mut self.state, chat_id));
            // T72: same reason `conversation::handle_key` does it — a wheel
            // back down to the newest message is the user looking at it.
            effects.extend(conversation::mark_visible_read(&mut self.state, chat_id));
        }
        effects
    }

    /// The same anchor step and near-top paging trigger as `Up`/`Down` in
    /// `conversation::handle_key`, claimed independently of focus for the
    /// same reason [`Self::scroll_chat_list`] is. `conversation::move_anchor`
    /// itself is private to that module, so this mirrors its stepping logic
    /// using the `pub(crate)` primitives (`index_of`,
    /// `trigger_paging_if_near_top`) already exposed there for exactly this
    /// kind of reuse, plus the `ConversationState` fields it operates on
    /// (all `pub`), rather than widening `conversation.rs`'s public surface
    /// for one more entry point that says the same thing `handle_key`'s
    /// claim rule already does.
    fn scroll_conversation_move(&mut self, up: bool) -> Vec<Effect> {
        let Some(chat_id) = self.state.open_chat else {
            return Vec::new();
        };
        let now = self.state.now;
        let delta: isize = if up { -1 } else { 1 };
        let Some(convo) = self.state.conversations.get_mut(&chat_id) else {
            return Vec::new();
        };
        if convo.messages.is_empty() {
            return Vec::new();
        }
        let last_idx = (convo.messages.len() - 1) as isize;
        let current_idx = match convo.scroll {
            Scroll::Bottom => {
                if delta >= 0 {
                    return Vec::new();
                }
                last_idx + 1
            }
            Scroll::At { message_id, .. } => {
                match conversation::index_of(&convo.messages, message_id) {
                    Some(idx) => idx as isize,
                    // Anchor older than everything loaded: waiting on the page
                    // that contains it, same as `move_anchor`'s own arm for this
                    // case. Stepping it by `delta` is meaningless with nothing
                    // loaded around it to step through.
                    None if convo
                        .messages
                        .front()
                        .is_some_and(|oldest| message_id < oldest.id) =>
                    {
                        return conversation::trigger_paging_if_near_top(convo, chat_id, now);
                    }
                    None => {
                        convo.scroll = Scroll::Bottom;
                        return Vec::new();
                    }
                }
            }
        };
        let target_idx = current_idx + delta;
        if target_idx > last_idx {
            convo.scroll = Scroll::Bottom;
            return Vec::new();
        }
        let clamped_idx = target_idx.clamp(0, last_idx) as usize;
        let new_id = convo.messages[clamped_idx].id;
        convo.scroll = Scroll::At {
            message_id: new_id,
            line_offset: 0,
        };
        conversation::trigger_paging_if_near_top(convo, chat_id, now)
    }

    fn selection_is_active(&self) -> bool {
        self.state
            .open_chat
            .and_then(|chat_id| self.state.conversations.get(&chat_id))
            .is_some_and(|convo| convo.selection.is_some())
    }

    /// The telemetry event an effect implies, if any (see [`Self::update`]).
    /// Keyed off the *request*, not off the handler that produced it: a text
    /// send is `message.send` whether it came from the composer, a Resend
    /// chip or (later) the palette, and it is `message.reply` exactly when
    /// the request itself carries a `reply_to`.
    /// `Edit`/`Delete`/`React` are deliberately absent here: this task moved
    /// them off the optimistic "request fired" event onto their `TdResult`
    /// completion (see the `dispatch` arms for `EditDone`/`DeleteDone`/
    /// `ReactionDone`), which reports the real outcome instead of always
    /// reporting `ok`. `Forward` stays — see the doc comment on the
    /// `ForwardDone` arm for why it could not make the same move.
    fn telemetry_for(&self, effect: &Effect) -> Option<TelemetryEvent> {
        let Effect::Td(request) = effect else {
            return None;
        };
        let (action, chat_id) = match request {
            TdRequest::SendMessageText {
                chat_id, reply_to, ..
            } => {
                let action = if reply_to.is_some() {
                    schema::actions::MESSAGE_REPLY
                } else {
                    schema::actions::MESSAGE_SEND
                };
                (action, *chat_id)
            }
            // The source chat, matching every other message event: what was
            // forwarded is a fact about where it came from. `ForwardDone`
            // only carries the destination, so this is the one place that
            // fact is still available — see the completion arm's docs.
            TdRequest::ForwardMessages { from_chat_id, .. } => {
                (schema::actions::MESSAGE_FORWARD, *from_chat_id)
            }
            TdRequest::OpenChat { chat_id } => (schema::actions::CHAT_OPEN, *chat_id),
            // The query text is never part of the event — only that a search
            // ran, and in what kind of chat (§4.8's allowlist).
            TdRequest::SearchChatMessages { chat_id, .. } => {
                (schema::actions::SEARCH_RUN, *chat_id)
            }
            _ => return None,
        };
        Some(self.chat_event(TelemetryEvent::ok(action), chat_id))
    }

    /// Attaches the two allowlisted per-chat fields (§4.8) to an event
    /// already built by `TelemetryEvent::ok`/`error`/`cancelled`: the hashed
    /// id always, the kind when the sidebar knows it. Neither field, nor
    /// anything else this function touches, can carry a title, a name or a
    /// message.
    fn chat_event(&self, event: TelemetryEvent, chat_id: ChatId) -> TelemetryEvent {
        let event = event.with_chat_hash(hashing::hash_id(&self.state.telemetry_salt, chat_id.0));
        match self.state.chat_list.chats.get(&chat_id) {
            Some(chat) => event.with_chat_kind(chat.kind.telemetry_str()),
            None => event,
        }
    }

    fn route_td(&mut self, update: &TdUpdate) -> Vec<Effect> {
        match update {
            TdUpdate::Auth(phase) => {
                self.dirty = true;
                let was_ready = matches!(self.state.auth.phase, AuthPhase::Ready);
                let mut effects = auth::handle_td(&mut self.state, update);
                // Login completed. Which flow got the user here is the whole
                // point of the two events, and only `auth` knows it — the
                // effects say nothing about it, so this one event is minted
                // from the projection rather than from a request.
                if !was_ready && matches!(phase, AuthPhase::Ready) {
                    let action = match self.state.auth.method {
                        Some(LoginMethod::Qr) => schema::actions::QR_LOGIN,
                        _ => schema::actions::PHONE_LOGIN,
                    };
                    effects.push(Effect::Telemetry(TelemetryEvent::ok(action)));
                }
                effects
            }
            TdUpdate::Connection(phase) => {
                if self.state.connection != *phase {
                    self.state.connection = *phase;
                    self.dirty = true;
                }
                Vec::new()
            }
            // Sidebar-only: chat identity, ordering and badges.
            TdUpdate::NewChat(_)
            | TdUpdate::ChatPosition { .. }
            | TdUpdate::ChatLastMessage { .. }
            | TdUpdate::ChatTitle { .. }
            | TdUpdate::ChatUnreadMentionCount { .. }
            | TdUpdate::ChatNotificationSettings { .. }
            // The folder tab strip's titles (task #60) — sidebar-only in
            // exactly the same sense: nothing outside `chat_list` reads a
            // folder's name.
            | TdUpdate::ChatFolders(_) => {
                self.dirty = true;
                chat_list::handle_td(&mut self.state, update)
            }
            // The one update both panes need: the sidebar's unread badge and
            // the conversation's read marker are the same TDLib fact seen from
            // two places, so it is delivered to both handlers rather than
            // duplicated into either sub-state.
            TdUpdate::ChatReadInbox { .. } => {
                self.dirty = true;
                let mut effects = chat_list::handle_td(&mut self.state, update);
                effects.extend(conversation::handle_td(&mut self.state, update));
                effects
            }
            // The conversation window takes the message; the toast queue
            // then decides whether it is worth telling the user about
            // (spec §6.4). Second, not first: `on_new_message` reads
            // `open_chat` and the sidebar's mute flag, both of which the
            // handlers above own — asking after they have run means the
            // suppression rules see the same state the user does.
            //
            // `Effect::Alert` is what `tgt-app` turns into the terminal
            // escape sequence. It carries no payload; the toast that goes
            // with it holds the title and preview, and never leaves the
            // cell grid.
            TdUpdate::NewMessage(msg) => {
                self.dirty = true;
                let mut effects = conversation::handle_td(&mut self.state, update);
                effects.extend(toasts::on_new_message(&mut self.state, msg));
                effects
            }
            // Conversation-window only.
            TdUpdate::MessagesDeleted { .. }
            | TdUpdate::MessageContentChanged { .. }
            | TdUpdate::ChatReadOutbox { .. } => {
                self.dirty = true;
                conversation::handle_td(&mut self.state, update)
            }
            // The send TDLib accepted has now actually gone out. The window
            // swaps the temporary id for the real one; an upload tracked
            // under that temporary id is over, whatever it was showing
            // (see `start_tracking_upload`).
            TdUpdate::MessageSendSucceeded { old_message_id, .. } => {
                self.dirty = true;
                media::complete_upload(&mut self.state, *old_message_id);
                conversation::handle_td(&mut self.state, update)
            }
            // A send that failed asynchronously, after its RPC already
            // returned `Ok`: the window marks the message failed, the
            // composer takes the held text back (spec §14). Both halves are
            // idempotent about it — see `composer::handle_td`'s dedupe note.
            // A failed upload stops being in flight just as surely as a
            // successful one, so its progress entry goes too.
            TdUpdate::MessageSendFailed { old_message_id, .. } => {
                self.dirty = true;
                media::complete_upload(&mut self.state, *old_message_id);
                let mut effects = conversation::handle_td(&mut self.state, update);
                effects.extend(composer::handle_td(&mut self.state, update));
                effects
            }
            // Reaction updates land on the conversation window like every
            // other per-message mutation above; kept in its own arm because
            // it is M5 territory, not M4's.
            TdUpdate::MessageInteractionInfo { .. } => {
                self.dirty = true;
                conversation::handle_td(&mut self.state, update)
            }
            // Presence: online/offline projection and typing indicators.
            // Dirty is set only when something actually changed — a
            // `UserStatus` repeating the status already on file, or a
            // `ChatAction { is_typing: false }` for a user who wasn't marked
            // typing, must not force an extra render.
            TdUpdate::UserStatus { user_id, status } => {
                let changed = self.state.presence.users.get(user_id) != Some(status);
                let effects = presence::handle_td(&mut self.state, update);
                if changed {
                    self.dirty = true;
                }
                effects
            }
            TdUpdate::ChatAction {
                chat_id, user_id, ..
            } => {
                let was_typing = self
                    .state
                    .presence
                    .typing
                    .contains_key(&(*chat_id, *user_id));
                let effects = presence::handle_td(&mut self.state, update);
                let is_typing_now = self
                    .state
                    .presence
                    .typing
                    .contains_key(&(*chat_id, *user_id));
                if was_typing != is_typing_now {
                    self.dirty = true;
                }
                effects
            }
            // `updateFile`: the download/upload progress table, plus (inside
            // `media::handle_td`) any open selection's Download→Open chip
            // flip. Always render-worthy — a progress bar or a completed
            // download both change what the message row shows, and neither
            // is worth the bookkeeping to detect a no-op push here.
            TdUpdate::File(_) => {
                self.dirty = true;
                media::handle_td(&mut self.state, update)
            }
        }
    }

    /// True once per render-worthy change; cleared on read.
    pub fn take_dirty(&mut self) -> bool {
        let was_dirty = self.dirty;
        self.dirty = false;
        was_dirty
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }
}

/// Starts tracking the upload behind an accepted file send, keyed by the
/// temporary message id `sendMessageFile` just minted (architecture §4.6,
/// `state/media.rs`'s "Upload tracking"). The entry lives until the send
/// succeeds or fails — both arms of `route_td` drop it — and is what the
/// conversation view renders as `↑ name · …` in place of the file card.
///
/// Only an accepted send that is still in flight starts one: a message that
/// came back already `Sent` (TDLib had the file cached) has no upload left
/// to watch.
///
/// KNOWN GAP: the tracked byte count never advances. TDLib reports upload
/// progress as an ordinary `updateFile` keyed by the *file* id it assigned,
/// and `UploadProgress` records no file id to match it against — so nothing
/// can correlate a push back to this message. The bar therefore sits at its
/// initial value for the life of the upload (indeterminate `…` for a photo,
/// `0%` for anything that declares a size). Closing it means adding a
/// `file_id` to `UploadProgress` and matching `media::handle_td`'s
/// `updateFile` arm against it, both in `state/media.rs`.
fn start_tracking_upload(state: &mut AppState, chat_id: ChatId, view: &MessageView) {
    if !matches!(view.send_state, SendState::Sending) {
        return;
    }
    if let Some(total) = upload_total(&view.content) {
        media::start_upload(state, view.id, chat_id, total);
    }
}

/// The declared byte size of a message's file, `None` for content that has
/// no file to upload at all. A `Photo` carries no size in the model, so it
/// tracks with a zero total — which the file card renders as the
/// indeterminate bar rather than as a percentage it would have to invent.
fn upload_total(content: &MessageContent) -> Option<u64> {
    match content {
        MessageContent::Photo { .. } => Some(0),
        MessageContent::Video { size, .. }
        | MessageContent::Audio { size, .. }
        | MessageContent::Document { size, .. } => Some(*size),
        MessageContent::Text(_)
        | MessageContent::Sticker { .. }
        | MessageContent::Unsupported { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::chat::{ChatKind, ChatListId, ChatPositionEntry, ChatView};
    use crate::model::entity::FormattedText;
    use crate::model::ids::{MessageId, UserId};
    use crate::model::message::{MessageCaps, MessageContent, MessageView, SendState, Sender};
    use crate::state::chat_list::visible_rows;
    use crate::td::error::TdError;
    use crate::td::request::TdRequest;

    pub(super) fn boot_fixture() -> Boot {
        Boot {
            theme_name: "dark".to_string(),
            bindings: KeyBindings::default(),
            layout_breakpoint_cols: 100,
            telemetry_mode: TelemetryMode::Off,
            crash_reports_available: false,
            telemetry_salt: [0u8; 32],
            consent_needed: false,
            has_credentials: false,
            width: 120,
            height: 40,
            auto_download_photos: true,
        }
    }

    #[test]
    fn ctrl_c_yields_quit_effect() {
        let mut app = App::new(boot_fixture());
        let effects = app.update(Action::Key(Key::Ctrl('c')));
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::Quit));
    }

    #[test]
    fn tick_updates_now_without_effects() {
        let mut app = App::new(boot_fixture());
        // Drain the initial "first frame" dirty flag before observing tick behavior.
        assert!(app.take_dirty());

        let effects = app.update(Action::Tick { now: Millis(1_234) });
        assert!(effects.is_empty());
        assert_eq!(app.state().now, Millis(1_234));
        assert!(!app.take_dirty());
    }

    #[test]
    fn boot_without_credentials_starts_the_wizard_on_api_id() {
        let app = App::new(boot_fixture());
        assert_eq!(app.state().auth.active_field, AuthField::ApiId);

        let app = App::new(Boot {
            has_credentials: true,
            ..boot_fixture()
        });
        assert_eq!(app.state().auth.active_field, AuthField::Phone);
    }

    #[test]
    fn auth_screen_claims_keys_but_never_the_quit_binding() {
        let mut app = App::new(Boot {
            has_credentials: true,
            ..boot_fixture()
        });
        app.update(Action::Td(TdUpdate::Auth(AuthPhase::WaitCode {
            delivery_hint: "SMS".to_string(),
            length: 5,
        })));

        assert!(app.update(Action::Key(Key::Char('7'))).is_empty());
        assert_eq!(app.state().auth.code.text, "7");

        let effects = app.update(Action::Key(Key::Ctrl('c')));
        assert!(matches!(effects.as_slice(), [Effect::Quit]));
        // The quit key never reached the field.
        assert_eq!(app.state().auth.code.text, "7");
    }

    #[test]
    fn ready_update_switches_to_main_and_loads_chats() {
        let mut app = App::new(Boot {
            has_credentials: true,
            ..boot_fixture()
        });
        let effects = app.update(Action::Td(TdUpdate::Auth(AuthPhase::Ready)));

        assert_eq!(app.state().screen, Screen::Main);
        // Reaching Ready is also the one telemetry event only the auth
        // projection can mint: which login flow finished.
        assert!(matches!(
            effects.as_slice(),
            [
                Effect::Td(TdRequest::LoadChats { .. }),
                Effect::Telemetry(TelemetryEvent {
                    action: schema::actions::PHONE_LOGIN,
                    ..
                })
            ]
        ));
        assert!(app.take_dirty());

        // A second Ready (TDLib repeats the phase on reconnect) is not a
        // second login.
        let effects = app.update(Action::Td(TdUpdate::Auth(AuthPhase::Ready)));
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Telemetry(_))),
            "Ready is only a login event on the transition into it: {effects:?}"
        );
    }

    #[test]
    fn connection_update_is_stored_and_only_dirties_on_change() {
        let mut app = App::new(boot_fixture());
        app.take_dirty();

        app.update(Action::Td(TdUpdate::Connection(ConnectionPhase::Ready)));
        assert_eq!(app.state().connection, ConnectionPhase::Ready);
        assert!(app.take_dirty());

        app.update(Action::Td(TdUpdate::Connection(ConnectionPhase::Ready)));
        assert!(!app.take_dirty());
    }

    #[test]
    fn expiring_flood_wait_on_tick_dirties_once() {
        let mut app = App::new(Boot {
            has_credentials: true,
            ..boot_fixture()
        });
        app.update(Action::TdResult(TdResult::AuthRequestDone {
            outcome: Err(TdError::FloodWait { seconds: 1 }),
        }));
        app.take_dirty();
        assert_eq!(app.state().auth.flood_wait_until, Some(Millis(1_000)));

        app.update(Action::Tick { now: Millis(500) });
        assert!(!app.take_dirty());

        app.update(Action::Tick { now: Millis(1_000) });
        assert_eq!(app.state().auth.flood_wait_until, None);
        assert!(app.take_dirty());
    }

    // -----------------------------------------------------------------
    // M3 routing
    // -----------------------------------------------------------------

    pub(super) fn logged_in() -> App {
        let mut app = App::new(Boot {
            has_credentials: true,
            ..boot_fixture()
        });
        app.update(Action::Td(TdUpdate::Auth(AuthPhase::Ready)));
        app.take_dirty();
        app
    }

    pub(super) fn chat(id: i64, title: &str, order: i64) -> TdUpdate {
        TdUpdate::NewChat(ChatView {
            id: ChatId(id),
            kind: ChatKind::Private,
            title: title.to_string(),
            positions: vec![ChatPositionEntry {
                list: ChatListId::Main,
                order,
                is_pinned: false,
            }],
            unread_count: 3,
            unread_mention_count: 0,
            last_message: None,
            is_muted: false,
        })
    }

    pub(super) fn message(chat_id: i64, id: i64) -> MessageView {
        MessageView {
            id: MessageId(id),
            chat_id: ChatId(chat_id),
            sender: Sender::User(UserId(7)),
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

    #[test]
    fn chat_updates_reach_the_sidebar_in_tdlib_order() {
        let mut app = logged_in();
        app.update(Action::Td(chat(1, "Ada", 10)));
        app.update(Action::Td(chat(2, "Bob", 30)));
        app.update(Action::Td(chat(3, "Cid", 20)));
        assert!(app.take_dirty());

        assert_eq!(
            visible_rows(&app.state().chat_list),
            vec![ChatId(2), ChatId(3), ChatId(1)]
        );

        // Order comes from TDLib alone: a position update reshuffles, and
        // order 0 drops the chat out of the list entirely.
        app.update(Action::Td(TdUpdate::ChatPosition {
            chat_id: ChatId(1),
            position: ChatPositionEntry {
                list: ChatListId::Main,
                order: 99,
                is_pinned: false,
            },
        }));
        app.update(Action::Td(TdUpdate::ChatPosition {
            chat_id: ChatId(3),
            position: ChatPositionEntry {
                list: ChatListId::Main,
                order: 0,
                is_pinned: false,
            },
        }));
        assert_eq!(
            visible_rows(&app.state().chat_list),
            vec![ChatId(1), ChatId(2)]
        );
    }

    #[test]
    fn read_inbox_reaches_both_the_badge_and_the_read_marker() {
        let mut app = logged_in();
        app.update(Action::Td(chat(1, "Ada", 10)));
        app.update(Action::Key(Key::Down));
        app.update(Action::Key(Key::Enter));

        app.update(Action::Td(TdUpdate::ChatReadInbox {
            chat_id: ChatId(1),
            last_read_inbox_message_id: MessageId(42),
            unread_count: 0,
        }));

        let state = app.state();
        assert_eq!(state.chat_list.chats[&ChatId(1)].unread_count, 0);
        assert_eq!(
            state.conversations[&ChatId(1)].last_read_inbox,
            MessageId(42)
        );
    }

    #[test]
    fn reaction_update_replaces_message_reactions() {
        let mut app = logged_in();
        app.update(Action::Td(chat(1, "Ada", 10)));
        app.update(Action::Key(Key::Down));
        app.update(Action::Key(Key::Enter));
        app.update(Action::Td(TdUpdate::NewMessage(message(1, 5))));
        app.take_dirty();

        let effects = app.update(Action::Td(TdUpdate::MessageInteractionInfo {
            chat_id: ChatId(1),
            message_id: MessageId(5),
            reactions: vec![crate::model::message::ReactionView {
                emoji: "👍".to_string(),
                count: 3,
                chosen_by_me: true,
            }],
        }));
        assert!(effects.is_empty());
        assert!(app.take_dirty());

        let convo = &app.state().conversations[&ChatId(1)];
        let msg = convo
            .messages
            .iter()
            .find(|m| m.id == MessageId(5))
            .unwrap();
        assert_eq!(msg.reactions.len(), 1);
        assert_eq!(msg.reactions[0].emoji, "👍");
        assert_eq!(msg.reactions[0].count, 3);
        assert!(msg.reactions[0].chosen_by_me);
    }

    #[test]
    fn read_outbox_advances_marker_only() {
        let mut app = logged_in();
        app.update(Action::Td(chat(1, "Ada", 10)));
        app.update(Action::Key(Key::Down));
        app.update(Action::Key(Key::Enter));
        app.update(Action::Td(TdUpdate::NewMessage(message(1, 5))));
        let before = message(1, 5);
        app.take_dirty();

        app.update(Action::Td(TdUpdate::ChatReadOutbox {
            chat_id: ChatId(1),
            last_read_outbox_message_id: MessageId(5),
        }));
        assert!(app.take_dirty());

        let convo = &app.state().conversations[&ChatId(1)];
        assert_eq!(convo.last_read_outbox, MessageId(5));
        // No per-message mutation: the message itself is untouched.
        let after = convo
            .messages
            .iter()
            .find(|m| m.id == MessageId(5))
            .unwrap();
        assert_eq!(after.content, before.content);
        assert_eq!(after.send_state, before.send_state);
        assert_eq!(after.reactions, before.reactions);
    }

    #[test]
    fn user_status_update_routes_to_presence_and_marks_dirty_only_on_change() {
        let mut app = logged_in();
        app.take_dirty();

        app.update(Action::Td(TdUpdate::UserStatus {
            user_id: UserId(7),
            status: crate::td::update::PresenceStatus::Online,
        }));
        assert!(app.take_dirty());
        assert_eq!(
            app.state().presence.users.get(&UserId(7)),
            Some(&crate::td::update::PresenceStatus::Online)
        );

        // Repeating the same status changes nothing observable: no redraw.
        app.update(Action::Td(TdUpdate::UserStatus {
            user_id: UserId(7),
            status: crate::td::update::PresenceStatus::Online,
        }));
        assert!(!app.take_dirty());
    }

    #[test]
    fn chat_action_typing_routes_to_presence_and_marks_dirty_on_change() {
        let mut app = logged_in();
        app.take_dirty();

        app.update(Action::Td(TdUpdate::ChatAction {
            chat_id: ChatId(1),
            user_id: UserId(7),
            is_typing: true,
        }));
        assert!(app.take_dirty());
        assert!(
            app.state()
                .presence
                .typing
                .contains_key(&(ChatId(1), UserId(7)))
        );

        app.update(Action::Td(TdUpdate::ChatAction {
            chat_id: ChatId(1),
            user_id: UserId(7),
            is_typing: false,
        }));
        assert!(app.take_dirty());
        assert!(
            !app.state()
                .presence
                .typing
                .contains_key(&(ChatId(1), UserId(7)))
        );

        // Redundant "not typing" for a user who wasn't marked typing: no
        // observable change, no redraw.
        app.update(Action::Td(TdUpdate::ChatAction {
            chat_id: ChatId(1),
            user_id: UserId(7),
            is_typing: false,
        }));
        assert!(!app.take_dirty());
    }

    #[test]
    fn tick_sweeps_expired_typing_and_marks_dirty() {
        let mut app = logged_in();
        app.update(Action::Td(TdUpdate::ChatAction {
            chat_id: ChatId(1),
            user_id: UserId(7),
            is_typing: true,
        }));
        app.take_dirty();

        app.update(Action::Tick {
            now: Millis(presence::TYPING_TTL_MS - 1),
        });
        assert!(!app.take_dirty());
        assert!(
            app.state()
                .presence
                .typing
                .contains_key(&(ChatId(1), UserId(7)))
        );

        app.update(Action::Tick {
            now: Millis(presence::TYPING_TTL_MS + 1),
        });
        assert!(app.take_dirty());
        assert!(
            !app.state()
                .presence
                .typing
                .contains_key(&(ChatId(1), UserId(7)))
        );
    }

    #[test]
    fn enter_opens_the_chat_and_the_cursor_follows_until_esc() {
        let mut app = logged_in();
        app.update(Action::Td(chat(1, "Ada", 10)));
        app.update(Action::Key(Key::Down));
        assert_eq!(app.state().chat_list.selected, Some(ChatId(1)));

        let effects = app.update(Action::Key(Key::Enter));
        assert!(matches!(
            effects.as_slice(),
            [
                Effect::Td(TdRequest::OpenChat { chat_id: ChatId(1) }),
                Effect::Td(TdRequest::GetChatHistory {
                    chat_id: ChatId(1),
                    ..
                }),
                Effect::Telemetry(TelemetryEvent {
                    action: schema::actions::CHAT_OPEN,
                    ..
                })
            ]
        ));
        assert_eq!(app.state().open_chat, Some(ChatId(1)));
        // The conversation side is the *base* focus now, not a level pushed
        // on top of the list — pane movement needs both panes at depth 1.
        assert_eq!(*app.state().focus.current(), Focus::Composer);
        assert_eq!(app.state().focus.depth(), 1);

        // Esc still goes back to the list (a base swap, not a pop).
        app.update(Action::Key(Key::Esc));
        assert_eq!(*app.state().focus.current(), Focus::ChatList);
        // The base level is never popped.
        app.update(Action::Key(Key::Esc));
        assert_eq!(app.state().focus.depth(), 1);
        assert_eq!(*app.state().focus.current(), Focus::ChatList);
    }

    #[test]
    fn scrolling_up_pages_history_and_the_result_lands_in_the_window() {
        let mut app = logged_in();
        app.update(Action::Td(chat(1, "Ada", 10)));
        app.update(Action::Key(Key::Down));
        app.update(Action::Key(Key::Enter));

        // The chat's first page, as the dispatcher delivers it.
        app.update(Action::TdResult(TdResult::HistoryLoaded {
            chat_id: ChatId(1),
            only_local: false,
            outcome: Ok(vec![message(1, 10), message(1, 11)]),
        }));
        assert_eq!(app.state().conversations[&ChatId(1)].messages.len(), 2);

        // Scrolling off the bottom lands within PAGE_TRIGGER_MESSAGES of the
        // oldest loaded message, which asks for the page before it. `PageUp`
        // rather than `Up`: with the composer focused, `Up` on an empty input
        // is selection-mode entry (spec §6.2), and only the page keys reach
        // the viewport from there.
        let effects = app.update(Action::Key(Key::PageUp));
        assert!(
            matches!(
                effects.as_slice(),
                [Effect::Td(TdRequest::GetChatHistory {
                    from_message_id: MessageId(10),
                    only_local: false,
                    ..
                })]
            ),
            "expected a page request from the oldest loaded message, got {effects:?}"
        );

        app.update(Action::TdResult(TdResult::HistoryLoaded {
            chat_id: ChatId(1),
            only_local: false,
            outcome: Ok(vec![message(1, 8), message(1, 9)]),
        }));
        let convo = &app.state().conversations[&ChatId(1)];
        assert_eq!(convo.messages.len(), 4);
        assert_eq!(convo.messages.front().unwrap().id, MessageId(8));
    }

    #[test]
    fn chats_loaded_completes_the_load_phase_only_on_success() {
        let mut app = logged_in();
        app.update(Action::TdResult(TdResult::ChatsLoaded {
            outcome: Err(TdError::Other {
                code: 420,
                message: "nope".to_string(),
            }),
        }));
        assert_eq!(app.state().chat_list.load, ChatLoadPhase::Idle);

        app.update(Action::TdResult(TdResult::ChatsLoaded { outcome: Ok(()) }));
        assert_eq!(app.state().chat_list.load, ChatLoadPhase::Complete);
        assert!(app.take_dirty());
    }

    #[test]
    fn update_is_deterministic() {
        let mut a = App::new(boot_fixture());
        let mut b = App::new(boot_fixture());

        let actions = vec![
            Action::Tick { now: Millis(100) },
            Action::Resize {
                width: 90,
                height: 30,
            },
            Action::Key(Key::Char('x')),
            Action::Tick { now: Millis(350) },
        ];

        for action in actions {
            a.update(action.clone());
            b.update(action);
        }

        assert_eq!(format!("{:?}", a.state()), format!("{:?}", b.state()));
    }
}

/// The spec §6.2 routing table itself: modal → focused pane → global, first
/// claimant stops propagation. A sibling of `tests` rather than a submodule
/// of it so the plan's acceptance filter (`cargo test -p tgt-core
/// app::routing`) selects exactly these.
#[cfg(test)]
mod routing {
    use super::tests::{boot_fixture, chat, logged_in, message};
    use super::*;
    use crate::td::error::TdError;
    use crate::telemetry::Outcome;

    #[test]
    fn help_opens_from_chat_list_swallows_keys_and_esc_closes() {
        let mut app = super::tests::logged_in();
        assert!(matches!(app.state().focus.current(), Focus::ChatList));
        app.update(Action::Key(Key::Char('?')));
        assert!(matches!(app.state().focus.current(), Focus::Help));
        // Swallowed: neither reopens help nor reaches the chat list.
        app.update(Action::Key(Key::Char('?')));
        app.update(Action::Key(Key::Down));
        assert!(matches!(app.state().focus.current(), Focus::Help));
        assert_eq!(app.state().focus.depth(), 2);
        app.update(Action::Key(Key::Esc));
        assert!(matches!(app.state().focus.current(), Focus::ChatList));
    }

    #[test]
    fn question_mark_types_into_the_composer_instead_of_opening_help() {
        let mut app = chat_open();
        app.update(Action::Key(Key::Char('?')));
        assert!(!matches!(app.state().focus.current(), Focus::Help));
        assert_eq!(app.state().composer.input.text, "?");
    }
    use crate::model::entity::FormattedText;
    use crate::model::ids::MessageId;
    use crate::model::message::MessageCaps;
    use crate::state::chat_list::visible_rows;
    use crate::state::conversation::Scroll;
    use crate::state::focus::ModalKind;

    const CHAT: ChatId = ChatId(1);
    /// The newest of the two messages `chat_open` loads.
    const NEWEST: MessageId = MessageId(11);

    /// A chat open with one page of history, focus resting on the composer —
    /// the state every §6.2 row below the auth screen is written against.
    fn chat_open() -> App {
        let mut app = logged_in();
        app.update(Action::Td(chat(1, "Ada", 10)));
        app.update(Action::Key(Key::Down));
        app.update(Action::Key(Key::Enter));
        app.update(Action::TdResult(TdResult::HistoryLoaded {
            chat_id: CHAT,
            only_local: false,
            outcome: Ok(vec![message(1, 10), message(1, 11)]),
        }));
        app.take_dirty();
        app
    }

    /// Effects as short ordered labels. `Effect` derives no `PartialEq` (not
    /// this task's to add), and what these tests are about is the exact
    /// sequence, so they compare rendered lines instead of pattern-matching
    /// position by position.
    fn describe(effects: &[Effect]) -> Vec<String> {
        effects
            .iter()
            .map(|effect| match effect {
                Effect::Td(TdRequest::SendMessageText {
                    chat_id,
                    reply_to,
                    text,
                }) => format!(
                    "Td(SendMessageText chat={} reply_to={:?} text={:?})",
                    chat_id.0,
                    reply_to.map(|id| id.0),
                    text.text
                ),
                Effect::Td(request) => format!("Td({})", request.kind()),
                Effect::Telemetry(event) => format!("Telemetry({})", event.action),
                other => format!("{other:?}"),
            })
            .collect()
    }

    fn selected_message(app: &App) -> Option<MessageId> {
        app.state().conversations[&CHAT]
            .selection
            .as_ref()
            .map(|sel| sel.message_id)
    }

    /// Selection mode on the newest message, with the delete capability TDLib
    /// only reports through `getMessageProperties` (architecture §7) folded
    /// in, so the chip row actually offers Delete.
    fn selection_with_delete_chip() -> App {
        let mut app = chat_open();
        app.update(Action::Key(Key::Up));
        app.update(Action::TdResult(TdResult::MessagePropertiesLoaded {
            chat_id: CHAT,
            message_id: NEWEST,
            outcome: Ok(MessageCaps {
                can_be_deleted_for_all_users: true,
                ..MessageCaps::default()
            }),
        }));
        app
    }

    #[test]
    fn modal_swallows_keys_from_panes() {
        let mut app = selection_with_delete_chip();
        assert_eq!(selected_message(&app), Some(NEWEST));

        // The Delete chip confirms before it deletes (spec §6.3).
        let effects = app.update(Action::Key(Key::Char('x')));
        assert!(effects.is_empty(), "delete confirms first: {effects:?}");
        assert!(matches!(
            app.state().focus.current(),
            Focus::Modal(ModalKind::ConfirmDelete { .. })
        ));
        assert_eq!(app.state().modal_ui, Some(ModalState::default()));

        // Everything below the modal is now unreachable: typing does not
        // land in the composer, and the arrows move the modal's own cursor
        // instead of the sidebar row or the message selection.
        let selected_row = app.state().chat_list.selected;
        assert!(app.update(Action::Key(Key::Char('h'))).is_empty());
        assert_eq!(app.state().composer.input.text, "");

        assert!(app.update(Action::Key(Key::Down)).is_empty());
        assert_eq!(app.state().chat_list.selected, selected_row);
        assert_eq!(selected_message(&app), Some(NEWEST));
        assert_eq!(
            app.state().modal_ui.unwrap().cursor,
            1,
            "the arrow moved the modal's cursor, not a pane"
        );

        // Confirm: the modal's request, one level popped, storage dropped.
        // No `Telemetry(message.delete)` here any more — this task moved
        // that event off the optimistic request-time emission and onto
        // `DeleteDone`'s completion, so the outcome it reports is real; see
        // `telemetry_for`'s doc comment and the `DeleteDone` arm in
        // `dispatch`.
        let effects = app.update(Action::Key(Key::Enter));
        assert_eq!(describe(&effects), ["Td(DeleteMessages)"]);
        assert_eq!(*app.state().focus.current(), Focus::Selection);
        assert!(app.state().modal_ui.is_none());
    }

    #[test]
    fn esc_dismisses_a_modal_without_its_effect() {
        let mut app = selection_with_delete_chip();
        app.update(Action::Key(Key::Char('x')));

        let effects = app.update(Action::Key(Key::Esc));
        assert!(effects.is_empty(), "dismissal deletes nothing: {effects:?}");
        assert_eq!(*app.state().focus.current(), Focus::Selection);
        assert!(app.state().modal_ui.is_none());
        // Exactly one level: selection mode survives the dismissal.
        assert_eq!(selected_message(&app), Some(NEWEST));
    }

    /// A successful `DeleteDone` is a genuine no-op except for the one
    /// telemetry event this action gets now that `telemetry_for` no longer
    /// fires an optimistic one at request time: no toast, no alert, no
    /// dirty frame — the `MessagesDeleted` push TDLib sends next is what
    /// actually changes anything on screen.
    #[test]
    fn delete_done_ok_reports_telemetry_only() {
        let mut app = chat_open();
        app.take_dirty();

        let effects = app.update(Action::TdResult(TdResult::DeleteDone {
            chat_id: CHAT,
            outcome: Ok(()),
        }));

        assert_eq!(effects.len(), 1, "expected exactly one effect: {effects:?}");
        let Effect::Telemetry(event) = &effects[0] else {
            panic!("expected a telemetry event: {effects:?}");
        };
        assert_eq!(event.action, schema::actions::MESSAGE_DELETE);
        assert_eq!(event.outcome, Outcome::Ok);
        assert!(event.error_kind.is_none());
        assert!(event.chat_hash.is_some(), "chat_hash should always be set");
        assert!(app.state().toasts.toasts.is_empty());
        assert!(!app.take_dirty(), "a bare Ok has nothing new to render");
    }

    /// This task's actual fix: a failed `DeleteDone` — previously dropped
    /// entirely by the catch-all — now raises a toast, rings the bell, and
    /// reports the real outcome instead of the `ok` `telemetry_for` used to
    /// fire optimistically at request time.
    #[test]
    fn delete_done_err_toasts_and_reports_the_real_outcome() {
        let mut app = chat_open();
        app.take_dirty();

        let effects = app.update(Action::TdResult(TdResult::DeleteDone {
            chat_id: CHAT,
            outcome: Err(TdError::NetTimeout),
        }));

        assert!(
            effects.iter().any(|e| matches!(e, Effect::Alert)),
            "a failure the user caused must ring the bell: {effects:?}"
        );
        let event = effects.iter().find_map(|e| match e {
            Effect::Telemetry(event) => Some(event),
            _ => None,
        });
        let event = event.expect("expected a telemetry event");
        assert_eq!(event.action, schema::actions::MESSAGE_DELETE);
        assert_eq!(event.outcome, Outcome::Error);
        assert_eq!(event.error_kind, Some(TdError::NetTimeout.telemetry_kind()));

        assert_eq!(app.state().toasts.toasts.len(), 1);
        assert_eq!(app.state().toasts.toasts[0].chat_id, CHAT);
        assert!(app.take_dirty(), "the new toast is render-worthy");
    }

    /// `EditDone` and `ReactionDone` mirror `DeleteDone` exactly (same
    /// shape in `App::dispatch`): this is a lighter check that the other
    /// two got the same wiring, not a repeat of the full assertion above.
    #[test]
    fn edit_and_reaction_failures_also_toast_and_report_errors() {
        for (make_result, action) in [
            (
                (|| TdResult::EditDone {
                    chat_id: CHAT,
                    message_id: NEWEST,
                    outcome: Err(TdError::Other {
                        code: 400,
                        message: "not allowed".to_string(),
                    }),
                }) as fn() -> TdResult,
                schema::actions::MESSAGE_EDIT,
            ),
            (
                || TdResult::ReactionDone {
                    chat_id: CHAT,
                    message_id: NEWEST,
                    outcome: Err(TdError::Other {
                        code: 400,
                        message: "not allowed".to_string(),
                    }),
                },
                schema::actions::MESSAGE_REACT,
            ),
        ] {
            let mut app = chat_open();
            let effects = app.update(Action::TdResult(make_result()));

            assert!(effects.iter().any(|e| matches!(e, Effect::Alert)));
            assert!(effects.iter().any(|e| matches!(
                e,
                Effect::Telemetry(event) if event.action == action && event.outcome == Outcome::Error
            )));
            assert_eq!(app.state().toasts.toasts.len(), 1);
        }
    }

    /// Forward could not move onto the same "one event, at completion"
    /// shape as the other three (see the `ForwardDone` arm's doc comment):
    /// `telemetry_for` still fires the optimistic `ok` at request time,
    /// tagged by the source chat, so a successful `ForwardDone` stays a
    /// total no-op here — minting a second `ok` would double-count the one
    /// action. A failure is still reported, from the only place it can be:
    /// tagged by the destination chat, since that is all `ForwardDone`
    /// carries.
    #[test]
    fn forward_done_ok_is_a_no_op_but_failure_still_toasts() {
        let mut app = chat_open();
        const DEST: ChatId = ChatId(9);

        let effects = app.update(Action::TdResult(TdResult::ForwardDone {
            to_chat_id: DEST,
            outcome: Ok(()),
        }));
        assert!(
            effects.is_empty(),
            "the request-time event already reported this: {effects:?}"
        );
        assert!(app.state().toasts.toasts.is_empty());

        let effects = app.update(Action::TdResult(TdResult::ForwardDone {
            to_chat_id: DEST,
            outcome: Err(TdError::Offline),
        }));
        assert!(effects.iter().any(|e| matches!(e, Effect::Alert)));
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::Telemetry(event)
                if event.action == schema::actions::MESSAGE_FORWARD
                    && event.outcome == Outcome::Error
                    && event.error_kind == Some(TdError::Offline.telemetry_kind())
        )));
        assert_eq!(app.state().toasts.toasts.len(), 1);
        assert_eq!(app.state().toasts.toasts[0].chat_id, DEST);
    }

    #[test]
    fn first_claimant_stops_propagation() {
        // `↑` with the composer focused is the composer's (selection entry on
        // an empty input). The viewport underneath never sees it: the scroll
        // anchor is still pinned to the bottom.
        let mut app = chat_open();
        let effects = app.update(Action::Key(Key::Up));
        assert_eq!(describe(&effects), ["Td(GetMessageProperties)"]);
        assert_eq!(*app.state().focus.current(), Focus::Selection);
        assert_eq!(app.state().conversations[&CHAT].scroll, Scroll::Bottom);

        // `PageUp` is claimed by no pane, so it reaches that same viewport
        // through the composer and pages older history in.
        let mut app = chat_open();
        let effects = app.update(Action::Key(Key::PageUp));
        assert_eq!(describe(&effects), ["Td(GetChatHistory)"]);
        assert!(matches!(
            app.state().conversations[&CHAT].scroll,
            Scroll::At {
                message_id: MessageId(10),
                ..
            }
        ));

        // A printable key belongs to whichever pane is focused: the chat
        // list's filter here, never the composer, though a chat is open.
        let mut app = chat_open();
        app.update(Action::Key(Key::Tab));
        app.update(Action::Key(Key::Char('/')));
        app.update(Action::Key(Key::Char('a')));
        assert_eq!(app.state().chat_list.filter.as_ref().unwrap().text, "a");
        assert_eq!(app.state().composer.input.text, "");
    }

    #[test]
    fn tab_cycles_focus_shift_tab_reverses() {
        let mut app = chat_open();
        assert_eq!(*app.state().focus.current(), Focus::Composer);

        // Two panes in M4, so the cycle is a toggle and `shift+tab` walks the
        // same pair the other way round; the direction becomes observable
        // when a third pane lands.
        for expected in [Focus::ChatList, Focus::Composer] {
            app.update(Action::Key(Key::Tab));
            assert_eq!(*app.state().focus.current(), expected);
        }
        for expected in [Focus::ChatList, Focus::Composer] {
            app.update(Action::Key(Key::BackTab));
            assert_eq!(*app.state().focus.current(), expected);
        }

        // `→` moves out of the list. `←` does not come back: it is the
        // composer's caret key, which claims it first.
        app.update(Action::Key(Key::Tab));
        app.update(Action::Key(Key::Right));
        assert_eq!(*app.state().focus.current(), Focus::Composer);
        app.update(Action::Key(Key::Left));
        assert_eq!(*app.state().focus.current(), Focus::Composer);

        // Movement is a depth-1 rule: an overlay is above both panes, so it
        // is popped (or committed) before either can move.
        app.update(Action::Key(Key::Tab));
        app.update(Action::Key(Key::Char('/')));
        assert_eq!(*app.state().focus.current(), Focus::ChatFilter);
        assert!(app.dispatch_key(Key::Tab).is_none());
        assert_eq!(*app.state().focus.current(), Focus::ChatFilter);
    }

    /// `ctrl+p` from every pane, because no pane above the global layer
    /// claims it — and from none of the contexts that sit above that layer:
    /// a modal (which swallows every key) and the auth screen.
    #[test]
    fn ctrl_p_opens_palette_from_any_pane() {
        let palette = boot_fixture().bindings.palette;

        // The composer, with a chat open: the key does not reach the input.
        let mut app = chat_open();
        let effects = app.update(Action::Key(palette));
        assert_eq!(describe(&effects), ["Telemetry(palette.open)"]);
        assert_eq!(*app.state().focus.current(), Focus::Palette);
        assert!(app.state().palette.is_some());
        assert_eq!(app.state().composer.input.text, "");

        // Pressing it again is the way back out, not a second palette.
        assert!(app.update(Action::Key(palette)).is_empty());
        assert_eq!(*app.state().focus.current(), Focus::Composer);
        assert!(app.state().palette.is_none());

        // The chat list.
        app.update(Action::Key(Key::Tab));
        app.update(Action::Key(palette));
        assert_eq!(*app.state().focus.current(), Focus::Palette);
        app.update(Action::Key(Key::Esc));
        assert_eq!(*app.state().focus.current(), Focus::ChatList);

        // Selection mode. The palette stacks on top of it and `Esc` gives it
        // back, selection intact — one level, as always.
        let mut app = selection_with_delete_chip();
        app.update(Action::Key(palette));
        assert_eq!(*app.state().focus.current(), Focus::Palette);
        app.update(Action::Key(Key::Esc));
        assert_eq!(*app.state().focus.current(), Focus::Selection);
        assert_eq!(selected_message(&app), Some(NEWEST));

        // A modal is the one context it cannot be opened from: the key is
        // claimed and swallowed with the modal left standing.
        app.update(Action::Key(Key::Char('x')));
        assert!(app.update(Action::Key(palette)).is_empty());
        assert!(matches!(app.state().focus.current(), Focus::Modal(_)));
        assert!(app.state().palette.is_none());

        // Nor from the auth screen, which claims every key it is shown.
        let mut app = App::new(Boot {
            has_credentials: true,
            ..boot_fixture()
        });
        assert!(app.update(Action::Key(palette)).is_empty());
        assert!(app.state().palette.is_none());
    }

    /// `⏎` on a palette entry: T41 closes `app.palette` itself, and that is
    /// the router's signal to pop the focus level it pushed. Opening a chat
    /// this way lands on the conversation side, exactly like `⏎` on a
    /// sidebar row.
    #[test]
    fn palette_enter_pops_focus() {
        let mut app = logged_in();
        app.update(Action::Td(chat(1, "Ada", 10)));
        app.update(Action::Key(app.state().bindings.palette));
        assert_eq!(app.state().focus.depth(), 2);

        let effects = app.update(Action::Key(Key::Enter));
        assert_eq!(
            describe(&effects),
            ["Td(OpenChat)", "Td(GetChatHistory)", "Telemetry(chat.open)"]
        );
        assert!(app.state().palette.is_none());
        assert_eq!(app.state().open_chat, Some(CHAT));
        assert_eq!(app.state().focus.depth(), 1);
        assert_eq!(*app.state().focus.current(), Focus::Composer);

        // A command entry closes the palette just as surely, and leaves the
        // focus where it was found.
        let mut app = chat_open();
        app.update(Action::Key(app.state().bindings.palette));
        for c in "Quit".chars() {
            app.update(Action::Key(Key::Char(c)));
        }
        let effects = app.update(Action::Key(Key::Enter));
        assert_eq!(describe(&effects), ["Quit"]);
        assert!(app.state().palette.is_none());
        assert_eq!(*app.state().focus.current(), Focus::Composer);
    }

    #[test]
    fn esc_pops_one_level_per_press_and_cancels_the_filter() {
        // Selection mode → composer, dropping the selection with it (T26's
        // exit half of the lifecycle).
        let mut app = chat_open();
        app.update(Action::Key(Key::Up));
        assert_eq!(app.state().focus.depth(), 2);

        app.update(Action::Key(Key::Esc));
        assert_eq!(*app.state().focus.current(), Focus::Composer);
        assert_eq!(app.state().focus.depth(), 1);
        assert!(selected_message(&app).is_none());

        // At the base, Esc leaves the conversation for the list.
        app.update(Action::Key(Key::Esc));
        assert_eq!(*app.state().focus.current(), Focus::ChatList);
        assert_eq!(app.state().focus.depth(), 1);

        // `⏎` commits the filter and keeps it applied; `Esc` cancels it.
        app.update(Action::Key(Key::Char('/')));
        app.update(Action::Key(Key::Char('a')));
        app.update(Action::Key(Key::Enter));
        assert_eq!(*app.state().focus.current(), Focus::ChatList);
        assert!(app.state().chat_list.filter.is_some());

        app.update(Action::Key(Key::Char('/')));
        app.update(Action::Key(Key::Char('z')));
        app.update(Action::Key(Key::Esc));
        assert_eq!(*app.state().focus.current(), Focus::ChatList);
        assert!(app.state().chat_list.filter.is_none());
    }

    #[test]
    fn up_on_an_empty_window_does_not_enter_an_empty_selection() {
        let mut app = logged_in();
        app.update(Action::Td(chat(1, "Ada", 10)));
        app.update(Action::Key(Key::Down));
        app.update(Action::Key(Key::Enter));

        // No history loaded: there is nothing to select, so the push T25
        // makes is undone rather than leaving a blank chip row focused.
        let effects = app.update(Action::Key(Key::Up));
        assert!(effects.is_empty(), "nothing to fetch caps for: {effects:?}");
        assert_eq!(*app.state().focus.current(), Focus::Composer);
        assert_eq!(app.state().focus.depth(), 1);
    }

    #[test]
    fn open_chat_to_reply_sent_produces_the_exact_effect_sequence() {
        let mut app = logged_in();
        app.update(Action::Td(chat(1, "Ada", 10)));
        app.update(Action::Key(Key::Down));

        // `⏎` on the row: open + first page, and the one chat-scoped event.
        assert_eq!(
            describe(&app.update(Action::Key(Key::Enter))),
            ["Td(OpenChat)", "Td(GetChatHistory)", "Telemetry(chat.open)"]
        );
        assert_eq!(*app.state().focus.current(), Focus::Composer);

        app.update(Action::TdResult(TdResult::HistoryLoaded {
            chat_id: CHAT,
            only_local: false,
            outcome: Ok(vec![message(1, 10), message(1, 11)]),
        }));

        // `↑` on the empty composer: selection mode on the newest message,
        // whose capabilities are refreshed on arrival.
        assert_eq!(
            describe(&app.update(Action::Key(Key::Up))),
            ["Td(GetMessageProperties)"]
        );
        assert_eq!(selected_message(&app), Some(NEWEST));

        // The Reply chip arms the composer and produces no effect; the focus
        // move back out of selection mode is the router's.
        assert!(app.update(Action::Key(Key::Char('r'))).is_empty());
        assert_eq!(app.state().composer.reply_to, Some(NEWEST));
        assert_eq!(*app.state().focus.current(), Focus::Composer);
        assert_eq!(app.state().focus.depth(), 1);
        assert!(selected_message(&app).is_none());

        for c in "hi".chars() {
            assert!(app.update(Action::Key(Key::Char(c))).is_empty());
        }

        // `⏎` sends. The request carries the reply target, which is what
        // makes the event `message.reply` rather than `message.send`.
        assert_eq!(
            describe(&app.update(Action::Key(Key::Enter))),
            [
                "Td(SendMessageText chat=1 reply_to=Some(11) text=\"hi\")",
                "Telemetry(message.reply)"
            ]
        );
        assert_eq!(app.state().composer.pending_send.as_deref(), Some("hi"));
        assert!(app.state().composer.reply_to.is_none());
        assert_eq!(app.state().composer.input.text, "");
    }

    /// `Action::Paste` reaches `composer::handle_paste`, which decides
    /// between an ordinary insert and a send-file offer. (Whether the path
    /// exists is the app layer's question, not this one's — see
    /// `composer::looks_like_path`.)
    #[test]
    fn paste_routes_to_the_composer() {
        let mut app = chat_open();

        app.update(Action::Paste("just some words".to_string()));
        assert_eq!(app.state().composer.input.text, "just some words");
        assert!(app.state().composer.pending_path_offer.is_none());

        app.update(Action::Paste("/tmp/dropped.png".to_string()));
        assert_eq!(
            app.state().composer.pending_path_offer,
            Some(std::path::PathBuf::from("/tmp/dropped.png")),
        );
        assert_eq!(
            app.state().composer.input.text,
            "just some words",
            "an offered path is held, not typed into the buffer"
        );
    }

    /// An accepted file send is tracked as an upload under the temporary id
    /// TDLib minted for it, and stops being tracked the moment the send
    /// resolves — either way, since a failed upload is no more in flight
    /// than a finished one.
    #[test]
    fn file_send_tracks_an_upload_until_it_resolves() {
        for resolution in ["succeeded", "failed"] {
            let mut app = chat_open();
            let mut optimistic = message(1, 500);
            optimistic.is_outgoing = true;
            optimistic.send_state = SendState::Sending;
            optimistic.content = MessageContent::Document {
                file_id: crate::model::ids::FileId(3),
                file_name: "report.pdf".to_string(),
                size: 4_000,
                caption: FormattedText {
                    text: String::new(),
                    entities: Vec::new(),
                },
            };

            app.update(Action::TdResult(TdResult::MessageSent {
                chat_id: CHAT,
                outcome: Ok(optimistic.clone()),
            }));

            let tracked = app.state().media.uploads[&MessageId(500)];
            assert_eq!(tracked.chat_id, CHAT);
            assert_eq!(tracked.total, 4_000);
            assert_eq!(tracked.uploaded, 0);

            let resolved = if resolution == "succeeded" {
                let mut sent = optimistic.clone();
                sent.id = MessageId(501);
                sent.send_state = SendState::Sent;
                TdUpdate::MessageSendSucceeded {
                    chat_id: CHAT,
                    old_message_id: MessageId(500),
                    message: sent,
                }
            } else {
                TdUpdate::MessageSendFailed {
                    chat_id: CHAT,
                    old_message_id: MessageId(500),
                    error: crate::td::error::TdError::NetTimeout,
                }
            };
            app.update(Action::Td(resolved));

            assert!(
                app.state().media.uploads.is_empty(),
                "a {resolution} send has nothing left to upload: {:?}",
                app.state().media.uploads
            );
        }
    }

    /// A text send starts no upload: there is no file behind it to watch.
    #[test]
    fn text_send_tracks_no_upload() {
        let mut app = chat_open();
        let mut optimistic = message(1, 500);
        optimistic.is_outgoing = true;
        optimistic.send_state = SendState::Sending;

        app.update(Action::TdResult(TdResult::MessageSent {
            chat_id: CHAT,
            outcome: Ok(optimistic),
        }));

        assert!(app.state().media.uploads.is_empty());
    }

    /// `/` is two different keys depending on where it lands (spec §11):
    /// the sidebar's filter, or in-chat message search from the message
    /// list. In the composer it is neither — it is a slash.
    #[test]
    fn slash_in_message_list_opens_search_but_in_chat_list_opens_filter() {
        // Chat list: the filter level, T15's.
        let mut app = chat_open();
        app.update(Action::Key(Key::Tab));
        app.update(Action::Key(Key::Char('/')));
        assert_eq!(*app.state().focus.current(), Focus::ChatFilter);
        assert!(app.state().chat_search.is_none());

        // Composer: a literal character, because `/send <path>` starts with
        // exactly this key on exactly this empty input.
        let mut app = chat_open();
        app.update(Action::Key(Key::Char('/')));
        assert_eq!(app.state().composer.input.text, "/");
        assert!(app.state().chat_search.is_none());
        assert_eq!(*app.state().focus.current(), Focus::Composer);

        // Message list (selection mode): in-chat search. The selection is
        // left behind rather than kept under the overlay.
        let mut app = chat_open();
        app.update(Action::Key(Key::Up));
        assert_eq!(selected_message(&app), Some(NEWEST));

        app.update(Action::Key(Key::Char('/')));
        assert_eq!(*app.state().focus.current(), Focus::ChatSearch);
        assert!(app.state().chat_search.is_some());
        assert!(selected_message(&app).is_none());

        // The query box takes the typing, and `⏎` fires the search.
        for c in "pr".chars() {
            app.update(Action::Key(Key::Char(c)));
        }
        assert_eq!(app.state().chat_search.as_ref().unwrap().input.text, "pr");
        assert_eq!(
            describe(&app.update(Action::Key(Key::Enter))),
            ["Td(SearchChatMessages)", "Telemetry(search.run)"]
        );

        // `Esc` pops search and takes its hits with it (T42's `close`).
        app.update(Action::TdResult(TdResult::SearchDone {
            chat_id: CHAT,
            outcome: Ok(vec![MessageId(10)]),
        }));
        assert!(!app.state().conversations[&CHAT].search_hits.is_empty());

        app.update(Action::Key(Key::Esc));
        assert!(app.state().chat_search.is_none());
        assert!(app.state().conversations[&CHAT].search_hits.is_empty());
        assert_eq!(*app.state().focus.current(), Focus::Composer);
        assert_eq!(app.state().focus.depth(), 1);
    }

    /// The `searchChatMessages` answer reaches T42's state, which stores the
    /// hits and drags the anchor to the first one.
    #[test]
    fn search_done_routes_to_search_state() {
        let mut app = chat_open();
        app.update(Action::Key(Key::Up));
        app.update(Action::Key(Key::Char('/')));
        app.update(Action::Key(Key::Char('x')));
        app.update(Action::Key(Key::Enter));
        assert!(app.state().chat_search.as_ref().unwrap().in_flight);
        app.take_dirty();

        let effects = app.update(Action::TdResult(TdResult::SearchDone {
            chat_id: CHAT,
            outcome: Ok(vec![MessageId(11), MessageId(10)]),
        }));
        assert!(effects.is_empty());
        assert!(app.take_dirty());

        assert!(!app.state().chat_search.as_ref().unwrap().in_flight);
        assert_eq!(
            app.state().conversations[&CHAT].search_hits,
            vec![MessageId(11), MessageId(10)]
        );
        assert_eq!(
            app.state().conversations[&CHAT].scroll,
            Scroll::At {
                message_id: MessageId(11),
                line_offset: 0,
            }
        );

        // `n` steps to the next hit through the same focus level.
        app.update(Action::Key(Key::Char('n')));
        assert_eq!(
            app.state().conversations[&CHAT].scroll,
            Scroll::At {
                message_id: MessageId(10),
                line_offset: 0,
            }
        );
    }

    /// Spec §6.4: a message arriving somewhere the user is not looking
    /// raises a toast and one terminal alert; the focused chat and a muted
    /// one raise neither.
    #[test]
    fn new_message_in_unfocused_chat_emits_alert_and_toast() {
        let mut app = chat_open();
        app.update(Action::Td(chat(2, "Bob", 20)));
        app.update(Action::Td(chat(3, "Muted Group", 5)));
        app.update(Action::Td(TdUpdate::ChatNotificationSettings {
            chat_id: ChatId(3),
            muted: true,
        }));

        // The open chat: the user is already looking at it — no alert, no
        // toast. The one effect it does produce is T72's read receipt, which
        // is the same fact seen from the other side: this message needs no
        // announcing precisely because it is being read right now.
        let effects = app.update(Action::Td(TdUpdate::NewMessage(message(1, 12))));
        assert_eq!(
            describe(&effects),
            ["Td(ViewMessages)"],
            "focused chat is silent apart from marking the arrival read"
        );
        assert!(app.state().toasts.toasts.is_empty());

        // Another chat: one alert, one toast, titled by the sidebar.
        let effects = app.update(Action::Td(TdUpdate::NewMessage(message(2, 13))));
        assert_eq!(describe(&effects), ["Alert"]);
        assert_eq!(app.state().toasts.toasts.len(), 1);
        assert_eq!(app.state().toasts.toasts[0].title, "Bob");
        assert_eq!(app.state().toasts.toasts[0].body, "message 13");

        // A muted chat: the badge still moves, the toast never appears.
        let effects = app.update(Action::Td(TdUpdate::NewMessage(message(3, 14))));
        assert!(effects.is_empty(), "muted chat is silent: {effects:?}");
        assert_eq!(app.state().toasts.toasts.len(), 1);

        // Suppression is about the alert, not about the message: the silent
        // one still reached the window it belongs to.
        assert!(
            app.state().conversations[&CHAT]
                .messages
                .iter()
                .any(|m| m.id == MessageId(12))
        );

        // And the toast leaves on its own once its TTL is up.
        app.update(Action::Tick {
            now: Millis(crate::state::toasts::TOAST_TTL_MS + 1),
        });
        assert!(app.state().toasts.toasts.is_empty());
        assert!(app.take_dirty());
    }

    /// `Esc` peels the newest toast before it touches the focus stack, and
    /// only the newest: one press, one toast.
    #[test]
    fn esc_dismisses_toast_before_popping_focus() {
        let mut app = chat_open();
        app.update(Action::Td(chat(2, "Bob", 20)));
        app.update(Action::Td(chat(3, "Cid", 15)));
        app.update(Action::Td(TdUpdate::NewMessage(message(2, 13))));
        app.update(Action::Td(TdUpdate::NewMessage(message(3, 14))));
        assert_eq!(app.state().toasts.toasts.len(), 2);

        // Selection mode is up, so there is a level to pop as well — and it
        // survives both dismissals.
        app.update(Action::Key(Key::Up));
        assert_eq!(app.state().focus.depth(), 2);

        app.update(Action::Key(Key::Esc));
        assert_eq!(app.state().toasts.toasts.len(), 1);
        assert_eq!(app.state().toasts.toasts[0].title, "Bob");
        assert_eq!(*app.state().focus.current(), Focus::Selection);

        app.update(Action::Key(Key::Esc));
        assert!(app.state().toasts.toasts.is_empty());
        assert_eq!(*app.state().focus.current(), Focus::Selection);

        // With the stack clear, `Esc` goes back to being the focus key.
        app.update(Action::Key(Key::Esc));
        assert_eq!(*app.state().focus.current(), Focus::Composer);
    }

    /// T43's archive pseudo-mode: `Esc` on the chat list backs out of the
    /// archive into `Main` before the generic pop rule applies.
    #[test]
    fn archive_esc_returns_to_main_list() {
        use crate::model::chat::{ChatListId, ChatPositionEntry};

        let mut app = logged_in();
        app.update(Action::Td(chat(1, "Ada", 10)));
        app.update(Action::Td(chat(2, "Old thread", 20)));
        // Move chat 2 out of Main and into the archive, TDLib's way.
        for position in [
            ChatPositionEntry {
                list: ChatListId::Main,
                order: 0,
                is_pinned: false,
            },
            ChatPositionEntry {
                list: ChatListId::Archive,
                order: 7,
                is_pinned: false,
            },
        ] {
            app.update(Action::Td(TdUpdate::ChatPosition {
                chat_id: ChatId(2),
                position,
            }));
        }

        app.update(Action::Key(Key::Char('a')));
        assert_eq!(app.state().chat_list.active_list, ChatListId::Archive);
        assert_eq!(visible_rows(&app.state().chat_list), vec![ChatId(2)]);

        app.update(Action::Key(Key::Esc));
        assert_eq!(app.state().chat_list.active_list, ChatListId::Main);
        assert_eq!(visible_rows(&app.state().chat_list), vec![ChatId(1)]);
        // One level: the chat list is still the focus, still at the base.
        assert_eq!(*app.state().focus.current(), Focus::ChatList);
        assert_eq!(app.state().focus.depth(), 1);

        // Outside the archive the same key is the ordinary chat-list `Esc`
        // again — nothing to back out of, so it is unclaimed at the base.
        assert!(app.dispatch_key(Key::Esc).is_none());
    }

    #[test]
    fn a_plain_send_is_message_send_not_message_reply() {
        let mut app = chat_open();
        app.update(Action::Key(Key::Char('h')));
        assert_eq!(
            describe(&app.update(Action::Key(Key::Enter))),
            [
                "Td(SendMessageText chat=1 reply_to=None text=\"h\")",
                "Telemetry(message.send)"
            ]
        );
    }

    // -----------------------------------------------------------------
    // Mouse routing (architecture §7.5)
    // -----------------------------------------------------------------

    /// Left-click on a sidebar row produces the exact same effect sequence
    /// `⏎` does, and lands focus on the composer the same way.
    #[test]
    fn click_chat_row_selects_and_opens() {
        let mut app = logged_in();
        app.update(Action::Td(chat(1, "Ada", 10)));
        assert_eq!(app.state().chat_list.selected, None);
        app.take_dirty();

        let effects = app.update(Action::Click {
            target: HitTarget::ChatRow(ChatId(1)),
            button: ClickButton::Left,
        });
        assert_eq!(
            describe(&effects),
            ["Td(OpenChat)", "Td(GetChatHistory)", "Telemetry(chat.open)"]
        );
        assert_eq!(app.state().chat_list.selected, Some(ChatId(1)));
        assert_eq!(app.state().open_chat, Some(ChatId(1)));
        assert_eq!(*app.state().focus.current(), Focus::Composer);
        assert_eq!(app.state().focus.depth(), 1);
        assert!(app.take_dirty());
    }

    /// Right-click on a message names the message under the cursor exactly
    /// (not "the newest loaded" the way `↑`-on-empty does), and a second
    /// right-click while already in selection mode moves the cursor there
    /// instead of stacking a second `Focus::Selection` level.
    #[test]
    fn right_click_message_enters_selection_with_chips() {
        let mut app = chat_open();

        let effects = app.update(Action::Click {
            target: HitTarget::Message(MessageId(10)),
            button: ClickButton::Right,
        });
        // The older of the two loaded messages sits at index 0 of a
        // two-message window — within `PAGE_TRIGGER_MESSAGES` of the oldest
        // loaded message — so anchoring there also fires the same paging
        // request `Up` would (unlike selecting the newest message below,
        // which re-pins to `Scroll::Bottom` and skips the check entirely).
        assert_eq!(
            describe(&effects),
            ["Td(GetMessageProperties)", "Td(GetChatHistory)"]
        );
        assert_eq!(selected_message(&app), Some(MessageId(10)));
        assert_eq!(*app.state().focus.current(), Focus::Selection);
        assert_eq!(app.state().focus.depth(), 2);
        assert!(app.take_dirty());

        let effects = app.update(Action::Click {
            target: HitTarget::Message(NEWEST),
            button: ClickButton::Right,
        });
        assert_eq!(describe(&effects), ["Td(GetMessageProperties)"]);
        assert_eq!(selected_message(&app), Some(NEWEST));
        assert_eq!(app.state().focus.depth(), 2, "no second level stacked");
    }

    /// Left-click on a spoiler reveals exactly that message, without
    /// touching selection or focus (architecture §7.5.1, T77) — unlike
    /// right-click on the same target, which is `Message`'s right-click in
    /// every other respect.
    #[test]
    fn left_click_spoiler_reveals_that_message_only() {
        let mut app = chat_open();

        let effects = app.update(Action::Click {
            target: HitTarget::Spoiler(MessageId(10)),
            button: ClickButton::Left,
        });
        assert!(effects.is_empty());
        assert!(
            app.state().conversations[&CHAT]
                .revealed_spoilers
                .contains(&MessageId(10))
        );
        assert!(
            !app.state().conversations[&CHAT]
                .revealed_spoilers
                .contains(&NEWEST),
            "only the clicked message reveals"
        );
        assert_eq!(
            *app.state().focus.current(),
            Focus::Composer,
            "revealing a spoiler must not enter selection mode"
        );
    }

    #[test]
    fn right_click_spoiler_enters_selection_like_an_ordinary_message() {
        let mut app = chat_open();

        let effects = app.update(Action::Click {
            target: HitTarget::Spoiler(MessageId(10)),
            button: ClickButton::Right,
        });
        assert_eq!(
            describe(&effects),
            ["Td(GetMessageProperties)", "Td(GetChatHistory)"]
        );
        assert_eq!(selected_message(&app), Some(MessageId(10)));
        assert_eq!(*app.state().focus.current(), Focus::Selection);
        assert!(
            app.state().conversations[&CHAT]
                .revealed_spoilers
                .is_empty(),
            "right-click reveals nothing"
        );
    }

    /// Left-click jumps to the *quoted* message; right-click still enters
    /// selection on the *containing* one, which is why the target carries
    /// both ids (architecture §7.5.1, T77).
    #[test]
    fn reply_quote_click_routes_left_to_jump_and_right_to_the_containing_message() {
        let mut app = chat_open();

        let effects = app.update(Action::Click {
            target: HitTarget::ReplyQuote {
                containing: NEWEST,
                quoted: MessageId(10),
            },
            button: ClickButton::Left,
        });
        assert!(effects.is_empty());
        assert_eq!(
            app.state().conversations[&CHAT].scroll,
            Scroll::At {
                message_id: MessageId(10),
                line_offset: 0,
            }
        );
        assert_eq!(
            *app.state().focus.current(),
            Focus::Composer,
            "jumping must not enter selection mode"
        );

        let effects = app.update(Action::Click {
            target: HitTarget::ReplyQuote {
                containing: NEWEST,
                quoted: MessageId(10),
            },
            button: ClickButton::Right,
        });
        assert_eq!(describe(&effects), ["Td(GetMessageProperties)"]);
        assert_eq!(
            selected_message(&app),
            Some(NEWEST),
            "right-click selects the containing message, not the quoted one"
        );
    }

    // --- conversation_pane_visible / task #70 ------------------------------

    #[test]
    fn conversation_pane_visible_true_in_two_pane_regardless_of_focus() {
        let app = chat_open(); // width 120 >= breakpoint 100: two-pane.
        assert_eq!(*app.state().focus.current(), Focus::Composer);
        assert!(conversation_pane_visible(app.state()));
    }

    #[test]
    fn conversation_pane_visible_requires_an_open_chat() {
        let app = logged_in();
        assert!(app.state().open_chat.is_none());
        assert!(!conversation_pane_visible(app.state()));
    }

    /// The exact fact `draw_single_pane` renders on, restated as an
    /// assertion rather than trusted by construction: below the breakpoint,
    /// visibility tracks focus (list vs. everything else), where above it
    /// visibility never does (previous test).
    #[test]
    fn conversation_pane_visible_tracks_focus_only_below_the_breakpoint() {
        let mut app = chat_open();
        app.update(Action::Resize {
            width: 80,
            height: 40,
        });
        assert!(
            conversation_pane_visible(app.state()),
            "focus is still Composer: the conversation should still be up"
        );

        app.update(Action::Key(Key::Esc));
        assert_eq!(*app.state().focus.current(), Focus::ChatList);
        assert!(!conversation_pane_visible(app.state()));
    }

    /// The scenario task #70 exists for: `Esc` back to the chat list in
    /// single-pane stops rendering the conversation, so TDLib has to be
    /// told. Two-pane keeps rendering it regardless of focus, so the same
    /// key must emit nothing.
    #[test]
    fn esc_to_chat_list_closes_the_chat_in_single_pane_but_not_two_pane() {
        let mut single = chat_open();
        single.update(Action::Resize {
            width: 80,
            height: 40,
        });
        single.take_dirty();
        let effects = single.update(Action::Key(Key::Esc));
        assert_eq!(*single.state().focus.current(), Focus::ChatList);
        assert_eq!(describe(&effects), ["Td(CloseChat)"]);

        let mut two = chat_open(); // width 120: never resized, stays two-pane.
        two.take_dirty();
        let effects = two.update(Action::Key(Key::Esc));
        assert_eq!(*two.state().focus.current(), Focus::ChatList);
        assert!(
            effects.is_empty(),
            "the conversation is still on screen in two-pane: nothing to close"
        );
    }

    /// Team lead's follow-up scenario: shrinking the terminal across the
    /// breakpoint can hide the conversation with no key pressed at all, if
    /// focus already happens to be on the chat list from before the resize
    /// (two-pane shows both panes regardless of focus, so that is a normal
    /// thing to have done there).
    #[test]
    fn resize_across_the_breakpoint_closes_a_now_hidden_chat() {
        let mut app = chat_open();
        app.update(Action::Key(Key::Esc)); // focus -> ChatList, still two-pane.
        assert_eq!(*app.state().focus.current(), Focus::ChatList);
        assert!(conversation_pane_visible(app.state()));
        app.take_dirty();

        let effects = app.update(Action::Resize {
            width: 80,
            height: 40,
        });
        assert!(!conversation_pane_visible(app.state()));
        assert_eq!(describe(&effects), ["Td(CloseChat)"]);
    }

    /// The same resize with focus left on the composer never hides the
    /// conversation (single-pane still shows it), so it must emit nothing —
    /// proving the resize handler is not just unconditionally closing on
    /// every resize while a chat happens to be open.
    #[test]
    fn resize_across_the_breakpoint_emits_nothing_when_still_visible() {
        let mut app = chat_open();
        assert_eq!(*app.state().focus.current(), Focus::Composer);

        let effects = app.update(Action::Resize {
            width: 80,
            height: 40,
        });
        assert!(conversation_pane_visible(app.state()));
        assert!(effects.is_empty());
    }

    #[test]
    fn folder_tab_click_switches_list() {
        use crate::model::chat::{ChatKind, ChatListId, ChatPositionEntry, ChatView};

        let mut app = logged_in();
        app.update(Action::Td(chat(1, "Main Chat", 10)));
        app.update(Action::Td(TdUpdate::NewChat(ChatView {
            id: ChatId(2),
            kind: ChatKind::Private,
            title: "Work Chat".to_string(),
            positions: vec![ChatPositionEntry {
                list: ChatListId::Folder(1),
                order: 20,
                is_pinned: false,
            }],
            unread_count: 0,
            unread_mention_count: 0,
            last_message: None,
            is_muted: false,
        })));
        app.take_dirty();

        let effects = app.update(Action::Click {
            target: HitTarget::FolderTab(ChatListId::Folder(1)),
            button: ClickButton::Left,
        });
        assert!(effects.is_empty());
        assert_eq!(app.state().chat_list.active_list, ChatListId::Folder(1));
        // Selection resets to the new list's first visible row, the same
        // rule `cycle_folder` follows for the `[`/`]` keys.
        assert_eq!(app.state().chat_list.selected, Some(ChatId(2)));
        assert!(app.take_dirty());
    }

    #[test]
    fn archive_row_click_toggles() {
        use crate::model::chat::{ChatListId, ChatPositionEntry};

        let mut app = logged_in();
        app.update(Action::Td(chat(1, "Ada", 10)));
        app.update(Action::Td(chat(2, "Old thread", 20)));
        for position in [
            ChatPositionEntry {
                list: ChatListId::Main,
                order: 0,
                is_pinned: false,
            },
            ChatPositionEntry {
                list: ChatListId::Archive,
                order: 7,
                is_pinned: false,
            },
        ] {
            app.update(Action::Td(TdUpdate::ChatPosition {
                chat_id: ChatId(2),
                position,
            }));
        }
        app.take_dirty();

        let effects = app.update(Action::Click {
            target: HitTarget::ArchiveRow,
            button: ClickButton::Left,
        });
        assert!(effects.is_empty());
        assert_eq!(app.state().chat_list.active_list, ChatListId::Archive);
        assert_eq!(visible_rows(&app.state().chat_list), vec![ChatId(2)]);
        assert!(app.take_dirty());

        // Clicking it again toggles back to Main, same as the `a` key.
        app.update(Action::Click {
            target: HitTarget::ArchiveRow,
            button: ClickButton::Left,
        });
        assert_eq!(app.state().chat_list.active_list, ChatListId::Main);
    }

    /// Overlays are keyboard-only for now: a modal claims every click and
    /// every scroll exactly as it claims every key.
    #[test]
    fn clicks_ignored_under_modal() {
        let mut app = selection_with_delete_chip();
        app.update(Action::Key(Key::Char('x')));
        assert!(matches!(app.state().focus.current(), Focus::Modal(_)));
        app.take_dirty();

        let effects = app.update(Action::Click {
            target: HitTarget::ChatRow(CHAT),
            button: ClickButton::Left,
        });
        assert!(effects.is_empty());
        assert!(!app.take_dirty());
        assert!(matches!(app.state().focus.current(), Focus::Modal(_)));

        let effects = app.update(Action::Scroll {
            area: ScrollArea::Conversation,
            up: true,
        });
        assert!(effects.is_empty());
        assert!(!app.take_dirty());
        assert!(matches!(app.state().focus.current(), Focus::Modal(_)));
    }

    #[test]
    fn scroll_conversation_moves_anchor_and_can_trigger_paging() {
        let mut app = logged_in();
        app.update(Action::Td(chat(1, "Ada", 10)));
        app.update(Action::Key(Key::Down));
        app.update(Action::Key(Key::Enter));
        let msgs: Vec<_> = (1..=21).map(|id| message(1, id)).collect();
        app.update(Action::TdResult(TdResult::HistoryLoaded {
            chat_id: CHAT,
            only_local: false,
            outcome: Ok(msgs),
        }));
        app.take_dirty();

        // From `Bottom`, the first wheel-up step anchors on the newest
        // loaded message — same as `Up` from an empty composer's viewport.
        let effects = app.update(Action::Scroll {
            area: ScrollArea::Conversation,
            up: true,
        });
        assert!(effects.is_empty());
        assert_eq!(
            app.state().conversations[&CHAT].scroll,
            Scroll::At {
                message_id: MessageId(21),
                line_offset: 0,
            }
        );
        assert!(app.take_dirty());

        // The second step lands within `PAGE_TRIGGER_MESSAGES` (20) of the
        // oldest loaded message, which asks for the page before it — the
        // wheel keeps the exact paging trigger `Up` has.
        let effects = app.update(Action::Scroll {
            area: ScrollArea::Conversation,
            up: true,
        });
        assert!(
            matches!(
                effects.as_slice(),
                [Effect::Td(TdRequest::GetChatHistory {
                    chat_id: CHAT,
                    from_message_id: MessageId(1),
                    only_local: false,
                    ..
                })]
            ),
            "expected a page request from the oldest loaded message, got {effects:?}"
        );
    }

    /// The chat-list wheel claims the pane on its own — even while the
    /// keyboard focus is elsewhere — but moves only `scroll_offset`, the
    /// viewport. `selected` and the focus stack are untouched: a mouse
    /// wheel looks around, it doesn't move the user's selection (this
    /// task's fix — the wheel used to replay `↑`/`↓` and drag the selection
    /// along with it).
    #[test]
    fn scroll_chat_list_moves_viewport_without_selection_or_focus_change() {
        let mut app = chat_open();
        app.update(Action::Td(chat(2, "Bob", 20)));
        assert_eq!(app.state().chat_list.selected, Some(ChatId(1)));
        app.take_dirty();

        let focus_before = app.state().focus.current().clone();
        let effects = app.update(Action::Scroll {
            area: ScrollArea::ChatList,
            up: false,
        });
        assert!(effects.is_empty());
        assert_eq!(app.state().chat_list.selected, Some(ChatId(1)));
        assert_eq!(app.state().chat_list.scroll_offset, 1);
        assert_eq!(*app.state().focus.current(), focus_before);
        assert_eq!(app.state().focus.depth(), 1);
        assert!(app.take_dirty());
    }

    #[test]
    fn composer_click_focuses_composer() {
        let mut app = chat_open();
        app.update(Action::Key(Key::Tab));
        assert_eq!(*app.state().focus.current(), Focus::ChatList);
        app.take_dirty();

        let effects = app.update(Action::Click {
            target: HitTarget::Composer,
            button: ClickButton::Left,
        });
        assert!(effects.is_empty());
        assert_eq!(*app.state().focus.current(), Focus::Composer);
        assert!(app.take_dirty());

        // With no chat open there is nowhere for the cursor to land: a
        // click on the (empty) composer pane is a no-op.
        let mut app = logged_in();
        let effects = app.update(Action::Click {
            target: HitTarget::Composer,
            button: ClickButton::Left,
        });
        assert!(effects.is_empty());
        assert!(!app.take_dirty());
        assert_eq!(*app.state().focus.current(), Focus::ChatList);
    }
}
