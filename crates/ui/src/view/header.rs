//! Chat header strip (spec §6.1 mock: `Alice Müller · online`). No open
//! chat renders the app title, matching the pre-T35 placeholder. Once a chat
//! is open, the left side is the chat's title and the right side is, in
//! priority order: the TDLib connection indicator (spec §14: "connecting…" /
//! "updating…" must be visible rather than manifesting as silence — this
//! predates T35 and keeps first claim on the slot since a reconnect matters
//! more than whether the other person is typing), then a live "typing…"
//! indicator, then the other party's presence.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use tgt_core::app::AppState;
use tgt_core::model::chat::ChatKind;
use tgt_core::model::ids::{ChatId, UserId};
use tgt_core::td::update::{ConnectionPhase, PresenceStatus};
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

const TITLE: &str = "telegram-tui";

pub fn draw(area: Rect, state: &AppState, theme: &Theme, f: &mut Frame) {
    let block = Block::bordered().border_style(Style::new().fg(theme.text_muted));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let title = open_chat_title(state).unwrap_or(TITLE);
    let mut spans = vec![Span::styled(title.to_string(), Style::new().fg(theme.text))];
    if let Some((label, style)) = right_indicator(state, theme) {
        let used = title.width() + label.width();
        let pad = (inner.width as usize).saturating_sub(used);
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(label.to_string(), style));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), inner);
}

fn open_chat_title(state: &AppState) -> Option<&str> {
    let chat_id = state.open_chat?;
    state
        .chat_list
        .chats
        .get(&chat_id)
        .map(|chat| chat.title.as_str())
}

fn connection_label(phase: ConnectionPhase) -> Option<&'static str> {
    match phase {
        ConnectionPhase::WaitingForNetwork => Some("waiting for network…"),
        ConnectionPhase::Connecting => Some("connecting…"),
        ConnectionPhase::ConnectingToProxy => Some("connecting to proxy…"),
        ConnectionPhase::Updating => Some("updating…"),
        ConnectionPhase::Ready => None,
    }
}

/// The right-aligned label plus its style, in priority order: connection
/// state (unchanged from pre-T35), then typing, then presence. Only the
/// open chat contributes typing/presence — there is nothing to show them
/// for when the chat list itself has focus.
fn right_indicator(state: &AppState, theme: &Theme) -> Option<(&'static str, Style)> {
    if let Some(label) = connection_label(state.connection) {
        return Some((label, Style::new().fg(theme.warning)));
    }

    let chat_id = state.open_chat?;
    if is_typing_in(state, chat_id) {
        return Some(("typing…", Style::new().fg(theme.accent)));
    }

    match other_party_presence(state, chat_id) {
        Some(PresenceStatus::Online) => Some(("online", Style::new().fg(theme.accent))),
        Some(PresenceStatus::Recently) => Some(("recently", Style::new().fg(theme.text_muted))),
        Some(PresenceStatus::Offline) | None => None,
    }
}

/// Any typing entry for `chat_id` that has not yet swept past its TTL. Reads
/// the expiry directly against `state.now` rather than trusting that
/// `state::presence::handle_tick` has already run this frame, so the header
/// stays correct even if a tick is late.
fn is_typing_in(state: &AppState, chat_id: ChatId) -> bool {
    state
        .presence
        .typing
        .iter()
        .any(|((c, _user), expiry)| *c == chat_id && *expiry > state.now)
}

/// The other participant's presence for a private chat, `None` for every
/// other chat kind (groups, supergroups, channels don't have a single
/// "online" peer). TDLib gives a private chat's peer user id as the chat id
/// itself — `ChatView` carries no separate user id field, so that identity
/// is how this is derived, not a lookup.
fn other_party_presence(state: &AppState, chat_id: ChatId) -> Option<PresenceStatus> {
    let chat = state.chat_list.chats.get(&chat_id)?;
    if chat.kind != ChatKind::Private {
        return None;
    }
    state.presence.users.get(&UserId(chat_id.0)).copied()
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tgt_core::app::{AppState, Screen};
    use tgt_core::effect::TelemetryMode;
    use tgt_core::model::chat::{ChatOrderKey, ChatView};
    use tgt_core::model::key::KeyBindings;
    use tgt_core::model::time::Millis;
    use tgt_core::state::auth::{AuthField, AuthState, InputField};
    use tgt_core::state::chat_list::ChatListState;
    use tgt_core::state::composer::ComposerState;
    use tgt_core::state::consent::{ConsentChoice, ConsentState};
    use tgt_core::state::conversation::{ConversationState, Scroll};
    use tgt_core::state::focus::{Focus, FocusStack};
    use tgt_core::state::history::PagingState;
    use tgt_core::state::media::MediaState;
    use tgt_core::state::presence::PresenceState;
    use tgt_core::state::toasts::ToastState;
    use tgt_core::td::update::AuthPhase;

    use super::*;

    use crate::theme::Theme;

    const CHAT: ChatId = ChatId(7);
    const USER: UserId = UserId(7); // private chat id == peer user id (see other_party_presence)

    fn private_chat() -> ChatView {
        ChatView {
            id: CHAT,
            kind: ChatKind::Private,
            title: "Alice Müller".to_string(),
            positions: Vec::new(),
            unread_count: 0,
            unread_mention_count: 0,
            last_message: None,
            is_muted: false,
        }
    }

    fn fixture_state() -> AppState {
        let mut chat_list = ChatListState::default();
        chat_list.chats.insert(CHAT, private_chat());
        let mut orders = BTreeSet::new();
        orders.insert(ChatOrderKey {
            order: 1,
            chat_id: CHAT,
        });
        chat_list.orders.insert(chat_list.active_list, orders);

        let mut conversations = HashMap::new();
        conversations.insert(
            CHAT,
            ConversationState {
                chat_id: CHAT,
                messages: Default::default(),
                paging: PagingState::Idle,
                scroll: Scroll::Bottom,
                revealed_spoilers: BTreeSet::new(),
                last_read_inbox: tgt_core::model::ids::MessageId(0),
                last_read_outbox: tgt_core::model::ids::MessageId(0),
                search_hits: Vec::new(),
                selection: None,
            },
        );

        AppState {
            screen: Screen::Main,
            focus: FocusStack::new(Focus::Composer),
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
            chat_list,
            conversations,
            open_chat: Some(CHAT),
            composer: ComposerState::default(),
            modal_ui: None,
            palette: None,
            chat_search: None,
            toasts: ToastState::default(),
            media: MediaState::default(),
            presence: PresenceState::default(),
            width: 80,
            height: 24,
            layout_breakpoint_cols: 100,
            theme_name: "dark".to_string(),
            theme_generation: 0,
            bindings: KeyBindings::default(),
            telemetry_mode: TelemetryMode::Off,
            telemetry_salt: [0u8; 32],
            now: Millis(10_000),
        }
    }

    fn render_to_string(width: u16, height: u16, state: &AppState) -> String {
        let theme = Theme::default_dark();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                draw(area, state, &theme, f);
            })
            .unwrap();
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
    fn no_open_chat_shows_the_app_title() {
        let mut state = fixture_state();
        state.open_chat = None;
        let rendered = render_to_string(40, 3, &state);
        assert!(rendered.contains("telegram-tui"));
        assert!(!rendered.contains("Alice"));
    }

    #[test]
    fn open_chat_shows_its_title_online_80x3() {
        let mut state = fixture_state();
        state.presence.users.insert(USER, PresenceStatus::Online);
        let rendered = render_to_string(80, 3, &state);
        assert!(rendered.contains("Alice Müller"));
        assert!(rendered.contains("online"));
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn recently_online_presence_shows_recently_not_online() {
        let mut state = fixture_state();
        state.presence.users.insert(USER, PresenceStatus::Recently);
        let rendered = render_to_string(80, 3, &state);
        assert!(rendered.contains("recently"));
        assert!(!rendered.contains("online"));
    }

    #[test]
    fn offline_presence_shows_nothing() {
        let mut state = fixture_state();
        state.presence.users.insert(USER, PresenceStatus::Offline);
        let rendered = render_to_string(80, 3, &state);
        assert!(!rendered.contains("online"));
        assert!(!rendered.contains("recently"));
    }

    /// Typing overrides presence even when the peer is also online.
    #[test]
    fn typing_overrides_presence_80x3() {
        let mut state = fixture_state();
        state.presence.users.insert(USER, PresenceStatus::Online);
        state
            .presence
            .typing
            .insert((CHAT, USER), Millis(state.now.0 + 5_000));
        let rendered = render_to_string(80, 3, &state);
        assert!(rendered.contains("typing…"));
        assert!(!rendered.contains("online"));
        insta::assert_snapshot!(rendered);
    }

    /// An expired typing entry (past its TTL relative to `state.now`) must
    /// not still show "typing…" — `is_typing_in` checks the expiry itself
    /// rather than trusting the entry was already swept.
    #[test]
    fn expired_typing_entry_falls_back_to_presence() {
        let mut state = fixture_state();
        state.presence.users.insert(USER, PresenceStatus::Online);
        state
            .presence
            .typing
            .insert((CHAT, USER), Millis(state.now.0.saturating_sub(1)));
        let rendered = render_to_string(80, 3, &state);
        assert!(!rendered.contains("typing…"));
        assert!(rendered.contains("online"));
    }

    /// The connection indicator keeps first claim on the slot even once a
    /// chat is open and its peer is typing (pre-T35 behavior, unchanged).
    #[test]
    fn connection_indicator_still_wins_over_typing_and_presence() {
        let mut state = fixture_state();
        state.connection = ConnectionPhase::Connecting;
        state.presence.users.insert(USER, PresenceStatus::Online);
        state
            .presence
            .typing
            .insert((CHAT, USER), Millis(state.now.0 + 5_000));
        let rendered = render_to_string(80, 3, &state);
        assert!(rendered.contains("connecting…"));
        assert!(!rendered.contains("typing…"));
        assert!(!rendered.contains("online"));
    }

    /// Groups and channels have no single "online" peer.
    #[test]
    fn group_chat_never_shows_presence() {
        let mut state = fixture_state();
        state.chat_list.chats.get_mut(&CHAT).unwrap().kind = ChatKind::Group;
        state.presence.users.insert(USER, PresenceStatus::Online);
        let rendered = render_to_string(80, 3, &state);
        assert!(!rendered.contains("online"));
    }
}
