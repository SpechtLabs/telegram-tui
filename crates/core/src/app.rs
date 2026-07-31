//! The Elm-style root: `AppState`, `Boot`, and `App::update`, the single pure
//! transition function. See docs/architecture.md §4.6.

use std::collections::HashMap;

use crate::action::{Action, TdResult};
use crate::effect::{Effect, TelemetryMode};
use crate::model::ids::ChatId;
use crate::model::key::{Key, KeyBindings};
use crate::model::time::Millis;
use crate::state::auth::{self, AuthField, AuthState, InputField, LoginMethod};
use crate::state::chat_list::{self, ChatListState, ChatLoadPhase};
use crate::state::composer::{self, ComposerState};
use crate::state::consent::{ConsentChoice, ConsentState};
use crate::state::conversation::{self, ConversationState};
use crate::state::focus::{Focus, FocusStack};
use crate::state::media::{self, MediaState};
use crate::state::modal::{self, ModalState};
use crate::state::palette::PaletteState;
use crate::state::presence::{self, PresenceState};
use crate::state::search::ChatSearchState;
use crate::state::selection;
use crate::state::toasts::ToastState;
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
    pub consent_needed: bool,
    pub has_credentials: bool,
    pub width: u16,
    pub height: u16,
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
            media: MediaState::default(),
            presence: PresenceState::default(),
            width: boot.width,
            height: boot.height,
            layout_breakpoint_cols: boot.layout_breakpoint_cols,
            theme_name: boot.theme_name,
            theme_generation: 0,
            bindings: boot.bindings,
            telemetry_mode: boot.telemetry_mode,
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
                effects
            }
            Action::Resize { width, height } => {
                self.state.width = width;
                self.state.height = height;
                self.dirty = true;
                Vec::new()
            }
            Action::Key(key) => self.route_key(key),
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
                    outcome: Ok(view), ..
                } = &result
                {
                    effects.extend(conversation::handle_td(
                        &mut self.state,
                        &TdUpdate::NewMessage(view.clone()),
                    ));
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
            // Paste and the remaining completions land with the tasks that
            // own their state. T32 wired the dispatcher end of all of them,
            // so `EditDone`/`DeleteDone`/`ForwardDone`/`ReactionDone` and the
            // `Io` results genuinely arrive here now — they are no-ops
            // because what the user sees of them is the push update that
            // follows (`MessageContentChanged`, `MessagesDeleted`, ...); it
            // is their *failure* that still needs a home, which is T44's
            // toasts. Left total over `Action` either way.
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
    /// reached the bottom unconsumed, which is exactly what the global
    /// bindings (`ctrl+p` palette, T41; `?` help, T44) will hang off.
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
            return self.escape().then(Vec::new);
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

        // 7. Global. `ctrl+p` and `?` are unclaimed until T41/T44 build the
        //    overlays behind them; they fall through here rather than being
        //    swallowed, so adding those layers is additive.
        None
    }

    /// Dispatches to whichever pane is on top of the focus stack, and runs
    /// the focus transitions the handlers deliberately leave to the router
    /// (an `Effect` list cannot express a focus change and shouldn't).
    fn route_pane_key(&mut self, key: Key) -> Option<Vec<Effect>> {
        match self.state.focus.current().clone() {
            Focus::Selection => self.route_selection_key(key),
            Focus::Composer => self.route_composer_key(key),
            Focus::ChatList | Focus::ChatFilter => self.route_chat_list_key(key),
            // Palette, in-chat search and help arrive in M7; `Modal` was
            // handled before any pane ran.
            _ => None,
        }
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
    /// Returns whether it was claimed.
    fn escape(&mut self) -> bool {
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
                    self.state.focus.replace_base(Focus::ChatList);
                } else {
                    return false;
                }
            }
        }
        self.dirty = true;
        true
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
            TdRequest::EditMessageText { chat_id, .. } => (schema::actions::MESSAGE_EDIT, *chat_id),
            TdRequest::DeleteMessages { chat_id, .. } => {
                (schema::actions::MESSAGE_DELETE, *chat_id)
            }
            // The source chat, matching every other message event: what was
            // forwarded is a fact about where it came from.
            TdRequest::ForwardMessages { from_chat_id, .. } => {
                (schema::actions::MESSAGE_FORWARD, *from_chat_id)
            }
            TdRequest::ToggleReaction { chat_id, .. } => (schema::actions::MESSAGE_REACT, *chat_id),
            TdRequest::OpenChat { chat_id } => (schema::actions::CHAT_OPEN, *chat_id),
            _ => return None,
        };
        Some(self.chat_event(action, chat_id))
    }

    /// An allowlisted event about a chat: the hashed id always, the kind when
    /// the sidebar knows it. Both are schema keys (§4.8); neither can carry a
    /// title, a name or a message.
    fn chat_event(&self, action: &'static str, chat_id: ChatId) -> TelemetryEvent {
        let event = TelemetryEvent::ok(action)
            .with_chat_hash(hashing::hash_id(&self.state.telemetry_salt, chat_id.0));
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
            | TdUpdate::ChatNotificationSettings { .. } => {
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
            // Conversation-window only.
            TdUpdate::NewMessage(_)
            | TdUpdate::MessagesDeleted { .. }
            | TdUpdate::MessageContentChanged { .. }
            | TdUpdate::MessageSendSucceeded { .. }
            | TdUpdate::ChatReadOutbox { .. } => {
                self.dirty = true;
                conversation::handle_td(&mut self.state, update)
            }
            // A send that failed asynchronously, after its RPC already
            // returned `Ok`: the window marks the message failed, the
            // composer takes the held text back (spec §14). Both halves are
            // idempotent about it — see `composer::handle_td`'s dedupe note.
            TdUpdate::MessageSendFailed { .. } => {
                self.dirty = true;
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
            telemetry_salt: [0u8; 32],
            consent_needed: false,
            has_credentials: false,
            width: 120,
            height: 40,
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
    use super::tests::{chat, logged_in, message};
    use super::*;
    use crate::model::ids::MessageId;
    use crate::model::message::MessageCaps;
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
        let effects = app.update(Action::Key(Key::Enter));
        assert_eq!(
            describe(&effects),
            ["Td(DeleteMessages)", "Telemetry(message.delete)"]
        );
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

    #[test]
    fn global_palette_key_reaches_through_panes_but_not_modals() {
        // The palette overlay itself lands in T41. What holds today is the
        // routing property it will be built on: no pane consumes `ctrl+p`, so
        // it falls all the way through to the global layer at the bottom of
        // the table — and a modal consumes it before it can get there.
        let mut app = chat_open();
        let palette = app.state().bindings.palette;

        assert!(
            app.dispatch_key(palette).is_none(),
            "the composer must not consume the palette key"
        );
        assert_eq!(app.state().composer.input.text, "");

        app.update(Action::Key(Key::Tab));
        assert!(
            app.dispatch_key(palette).is_none(),
            "the chat list must not consume the palette key"
        );

        let mut app = selection_with_delete_chip();
        assert!(
            app.dispatch_key(palette).is_none(),
            "selection mode must not consume the palette key"
        );

        app.update(Action::Key(Key::Char('x')));
        assert!(
            app.dispatch_key(palette).is_some(),
            "a modal swallows every key shown to it"
        );
        assert!(matches!(app.state().focus.current(), Focus::Modal(_)));
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
}
