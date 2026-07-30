//! The Elm-style root: `AppState`, `Boot`, and `App::update`, the single pure
//! transition function. See docs/architecture.md §4.6.

use std::collections::HashMap;

use crate::action::Action;
use crate::effect::{Effect, TelemetryMode};
use crate::model::ids::ChatId;
use crate::model::key::KeyBindings;
use crate::model::time::Millis;
use crate::state::auth::{AuthField, AuthState, InputField};
use crate::state::chat_list::ChatListState;
use crate::state::composer::ComposerState;
use crate::state::consent::{ConsentChoice, ConsentState};
use crate::state::conversation::ConversationState;
use crate::state::focus::{Focus, FocusStack};
use crate::state::media::MediaState;
use crate::state::palette::PaletteState;
use crate::state::presence::PresenceState;
use crate::state::search::ChatSearchState;
use crate::state::toasts::ToastState;
use crate::td::update::{AuthPhase, ConnectionPhase};

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
                active_field: AuthField::Phone,
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
                // Caching the clock is not itself render-worthy: dirty stays
                // whatever later handlers (e.g. TTL sweeps) leave it as.
                self.state.now = now;
                Vec::new()
            }
            Action::Resize { width, height } => {
                self.state.width = width;
                self.state.height = height;
                self.dirty = true;
                Vec::new()
            }
            Action::Key(k) if k == self.state.bindings.quit => vec![Effect::Quit],
            // Key routing (modal → focused pane → global), Paste, Td,
            // TdResult, and Io dispatch are wired in by later tasks
            // (T11, T15, T25-T27, T28, ...). Left as a deliberate no-op so
            // `update` stays total over `Action` from day one.
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
    use crate::model::key::Key;

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
