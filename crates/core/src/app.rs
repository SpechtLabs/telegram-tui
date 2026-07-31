//! The Elm-style root: `AppState`, `Boot`, and `App::update`, the single pure
//! transition function. See docs/architecture.md §4.6.

use std::collections::HashMap;

use crate::action::{Action, TdResult};
use crate::effect::{Effect, TelemetryMode};
use crate::model::ids::ChatId;
use crate::model::key::{Key, KeyBindings};
use crate::model::time::Millis;
use crate::state::auth::{self, AuthField, AuthState, InputField};
use crate::state::chat_list::{self, ChatListState, ChatLoadPhase};
use crate::state::composer::ComposerState;
use crate::state::consent::{ConsentChoice, ConsentState};
use crate::state::conversation::{self, ConversationState};
use crate::state::focus::{Focus, FocusStack};
use crate::state::media::MediaState;
use crate::state::palette::PaletteState;
use crate::state::presence::PresenceState;
use crate::state::search::ChatSearchState;
use crate::state::toasts::ToastState;
use crate::td::update::{AuthPhase, ConnectionPhase, TdUpdate};

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
    pub fn update(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::Tick { now } => {
                // Caching the clock is not itself render-worthy: only a
                // handler that actually changed something sets dirty.
                self.state.now = now;
                let flood_wait_before = self.state.auth.flood_wait_until;
                let effects = auth::handle_tick(&mut self.state, now);
                if self.state.auth.flood_wait_until != flood_wait_before {
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
            // Paste, the remaining `TdResult` completions and `Io` land with
            // the tasks that own their state (T25-T27, T36, ...). Left as a
            // deliberate no-op so `update` stays total over `Action`.
            _ => Vec::new(),
        }
    }

    /// Spec §6.2's routing order: modal → focused pane → global, first
    /// claimant wins.
    ///
    /// The quit binding is checked ahead of all of it, deliberately. A pane
    /// claims every key it is shown for — the auth wizard is a full-screen
    /// text form, so `state::auth::handle_key` returns `Some` for anything
    /// while `Screen::Auth` is up — and routing `ctrl+c` through it first
    /// would leave a half-finished login unquittable. The interrupt key is
    /// reserved, not routable.
    fn route_key(&mut self, key: Key) -> Vec<Effect> {
        if key == self.state.bindings.quit {
            return vec![Effect::Quit];
        }

        // `auth::handle_key` claims everything while `Screen::Auth` is up and
        // nothing once it isn't, so it stays first and needs no screen check
        // of its own. Modals (T27) and the selection pane (T26) slot in above
        // and below the M3 panes as their tasks land; T28 replaces this
        // sequence with the full spec §6.2 table.
        if let Some(effects) = auth::handle_key(&mut self.state, key) {
            self.dirty = true;
            return effects;
        }

        if self.state.screen == Screen::Main {
            let open_before = self.state.open_chat;
            if let Some(effects) = chat_list::handle_key(&mut self.state, key) {
                self.dirty = true;
                self.follow_focus_into_opened_chat(open_before);
                return effects;
            }
            if let Some(effects) = conversation::handle_key(&mut self.state, key) {
                self.dirty = true;
                return effects;
            }
        }

        // Global. `Esc` pops exactly one focus level and never the base
        // (architecture §4.5): the panes deliberately leave the pop to the
        // router — `chat_list::handle_key` returns `None` for `Esc` while
        // filtering for precisely this reason.
        if key == Key::Esc && self.state.focus.pop() {
            self.dirty = true;
        }
        Vec::new()
    }

    /// Enter on a chat row opens it; the cursor follows into the conversation
    /// so `↑`/`↓` scroll history instead of continuing to move the sidebar
    /// selection, and `Esc` pops straight back to the list.
    ///
    /// `Focus::Composer` is the conversation side's resting focus per spec
    /// §6.2 (typing goes to the composer, `↑` on an empty input enters
    /// selection mode) — there is no separate "message list" focus. The
    /// composer itself is inert this milestone (T25/T30), but
    /// `conversation::handle_key` claims scroll keys for any focus other than
    /// `ChatList`, which is what makes paging reachable from the keyboard at
    /// the M3 gate. T28 owns the real pane-movement table.
    fn follow_focus_into_opened_chat(&mut self, open_before: Option<ChatId>) {
        if self.state.open_chat != open_before
            && self.state.open_chat.is_some()
            && *self.state.focus.current() == Focus::ChatList
        {
            self.state.focus.push(Focus::Composer);
        }
    }

    fn route_td(&mut self, update: &TdUpdate) -> Vec<Effect> {
        match update {
            TdUpdate::Auth(_) => {
                self.dirty = true;
                auth::handle_td(&mut self.state, update)
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
            | TdUpdate::MessageSendFailed { .. }
            | TdUpdate::ChatReadOutbox { .. } => {
                self.dirty = true;
                conversation::handle_td(&mut self.state, update)
            }
            // File, presence and reaction updates arrive with M5/M6.
            _ => Vec::new(),
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

    fn boot_fixture() -> Boot {
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
        assert!(matches!(
            effects.as_slice(),
            [Effect::Td(TdRequest::LoadChats { .. })]
        ));
        assert!(app.take_dirty());
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

    fn logged_in() -> App {
        let mut app = App::new(Boot {
            has_credentials: true,
            ..boot_fixture()
        });
        app.update(Action::Td(TdUpdate::Auth(AuthPhase::Ready)));
        app.take_dirty();
        app
    }

    fn chat(id: i64, title: &str, order: i64) -> TdUpdate {
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

    fn message(chat_id: i64, id: i64) -> MessageView {
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
                })
            ]
        ));
        assert_eq!(app.state().open_chat, Some(ChatId(1)));
        assert_eq!(*app.state().focus.current(), Focus::Composer);

        // With the cursor inside the conversation, arrows scroll history
        // rather than the sidebar; Esc hands it back to the list.
        app.update(Action::Key(Key::Esc));
        assert_eq!(*app.state().focus.current(), Focus::ChatList);
        // The base level is never popped.
        app.update(Action::Key(Key::Esc));
        assert_eq!(app.state().focus.depth(), 1);
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
        // oldest loaded message, which asks for the page before it.
        let effects = app.update(Action::Key(Key::Up));
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
