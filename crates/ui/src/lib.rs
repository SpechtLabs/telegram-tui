//! Module tree; no logic beyond the `view` root below.

pub mod input;
pub mod render;
pub mod theme;
pub mod view;

use ratatui::Frame;
use tgt_core::app::{AppState, Screen};

use crate::render::cache::LayoutCache;
use crate::theme::Theme;

pub fn view(state: &AppState, theme: &Theme, f: &mut Frame, cache: &mut LayoutCache) {
    match state.screen {
        // The auth wizard owns the whole frame; it is a screen, not a pane.
        Screen::Auth => view::auth::draw(state, theme, f),
        // TODO(T50): `Screen::Consent` gets `view::consent::draw`. Until the
        // consent screen exists nothing ever sets that screen (main.rs boots
        // with `consent_needed: false`), so falling through to the shell is
        // unreachable rather than wrong.
        Screen::Consent | Screen::Main => view::root::draw(state, theme, f, cache),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tgt_core::app::{AppState, Screen};
    use tgt_core::effect::TelemetryMode;
    use tgt_core::model::key::KeyBindings;
    use tgt_core::model::time::Millis;
    use tgt_core::state::auth::{AuthField, AuthState, InputField};
    use tgt_core::state::chat_list::ChatListState;
    use tgt_core::state::composer::ComposerState;
    use tgt_core::state::consent::{ConsentChoice, ConsentState};
    use tgt_core::state::focus::{Focus, FocusStack};
    use tgt_core::state::media::MediaState;
    use tgt_core::state::presence::PresenceState;
    use tgt_core::state::toasts::ToastState;
    use tgt_core::td::update::{AuthPhase, ConnectionPhase};

    use super::*;
    use crate::view::hint_bar;

    fn fixture_state() -> AppState {
        AppState {
            screen: Screen::Main,
            focus: FocusStack::new(Focus::ChatList),
            connection: ConnectionPhase::Ready,
            consent: ConsentState {
                selected: ConsentChoice::Enable,
                acknowledged: true,
            },
            auth: AuthState {
                phase: AuthPhase::Ready,
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
            modal_ui: None,
            palette: None,
            chat_search: None,
            toasts: ToastState::default(),
            media: MediaState::default(),
            presence: PresenceState::default(),
            width: 120,
            height: 40,
            layout_breakpoint_cols: 100,
            theme_name: "dark".to_string(),
            theme_generation: 0,
            bindings: KeyBindings::default(),
            telemetry_mode: TelemetryMode::Off,
            telemetry_salt: [0u8; 32],
            now: Millis::default(),
        }
    }

    /// Flattens a TestBackend buffer into one string per row, newline
    /// separated, so a plain `contains` check can look for rendered text.
    fn render_to_string(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let mut out = String::with_capacity(buffer.content.len() + buffer.area.height as usize);
        for row in buffer.content.chunks(buffer.area.width as usize) {
            for cell in row {
                out.push_str(cell.symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn auth_screen_replaces_the_shell_entirely() {
        let mut state = fixture_state();
        state.screen = Screen::Auth;
        state.auth.phase = AuthPhase::WaitTdlibParameters;
        let theme = Theme::default_dark();
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        let mut cache = LayoutCache::new();

        terminal
            .draw(|f| view(&state, &theme, f, &mut cache))
            .unwrap();

        let rendered = render_to_string(&terminal);
        assert!(
            rendered.contains("Starting Telegram client"),
            "expected the auth wizard, got:\n{rendered}"
        );
        assert!(
            !rendered.contains("CHATS"),
            "the two-pane shell must not render behind the wizard:\n{rendered}"
        );
    }

    #[test]
    fn view_renders_chats_sidebar_and_hint_bar() {
        let state = fixture_state();
        let theme = Theme::default_dark();
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        let mut cache = LayoutCache::new();

        terminal
            .draw(|f| view(&state, &theme, f, &mut cache))
            .unwrap();

        let rendered = render_to_string(&terminal);
        assert!(
            rendered.contains("CHATS"),
            "buffer did not contain CHATS:\n{rendered}"
        );
        assert!(
            rendered.contains(hint_bar::HINT_TEXT),
            "buffer did not contain the hint bar text:\n{rendered}"
        );
    }
}
