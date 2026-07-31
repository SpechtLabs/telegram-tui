//! Chat header strip (spec §6.1 mock: `Alice Müller · online`). No open
//! chat renders the app title, matching the pre-T35 placeholder. Once a chat
//! is open, the left side is the chat's title and the right side is, in
//! priority order: the TDLib connection indicator (spec §14: "connecting…" /
//! "updating…" must be visible rather than manifesting as silence — this
//! predates T35 and keeps first claim on the slot since a reconnect matters
//! more than whether the other person is typing), then the in-chat search
//! hit-count (T47), then a live "typing…" indicator, then the other party's
//! presence.
//!
//! ## Search wins over typing/presence (T47)
//!
//! The search hit-count (`3/7`, `ChatSearchState::current_hit + 1` over
//! `ConversationState::search_hits.len()`) slots in right after the
//! connection indicator and ahead of typing/presence. Rationale: search is
//! `Focus::ChatSearch` — an explicit mode the user is actively in (spec
//! §11's `/`-then-`n`/`N` flow) — the same reason connection status already
//! outranks the other two: a mode the user deliberately entered and is
//! reading feedback from beats an ambient status update. It only shows once
//! there are hits to count (`handle_td_result` has answered with a non-empty
//! list); an empty or not-yet-submitted query falls through to
//! typing/presence exactly as before, so opening search on a chat with no
//! result yet doesn't blank out its presence line.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use tgt_core::app::AppState;
use tgt_core::model::chat::ChatKind;
use tgt_core::model::ids::{ChatId, UserId};
use tgt_core::td::update::{ConnectionPhase, PresenceStatus};
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

const TITLE: &str = "telegram-tui";

/// One line, no box: `view::root` hands over an area that is already padded
/// and already has the header's blank breathing row above it, and puts the
/// single horizontal rule the design language allows underneath
/// (docs/design-language.md §1). The title is the pane's primary text; every
/// status label to its right is tertiary, apart from the two the user is
/// waiting on (a reconnect, someone typing).
pub fn draw(area: Rect, state: &AppState, theme: &Theme, f: &mut Frame) {
    let title = open_chat_title(state).unwrap_or(TITLE);
    let mut spans = vec![Span::styled(
        title.to_string(),
        Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
    )];
    if let Some((label, style)) = right_indicator(state, theme) {
        let used = title.width() + label.width();
        let pad = (area.width as usize).saturating_sub(used);
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(label, style));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
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
/// state (unchanged from pre-T35), then the search hit-count (T47, module
/// docs' "Search wins over typing/presence"), then typing, then presence.
/// Only the open chat contributes search/typing/presence — there is nothing
/// to show them for when the chat list itself has focus.
fn right_indicator(state: &AppState, theme: &Theme) -> Option<(String, Style)> {
    if let Some(label) = connection_label(state.connection) {
        return Some((label.to_string(), Style::new().fg(theme.warning)));
    }

    let chat_id = state.open_chat?;
    if let Some(label) = search_hit_count_label(state, chat_id) {
        return Some((label, Style::new().fg(theme.text_muted)));
    }
    if is_typing_in(state, chat_id) {
        return Some(("typing…".to_string(), Style::new().fg(theme.accent)));
    }

    // Presence is tertiary (design language §2): it is ambient status, and a
    // header that shouts "online" competes with the conversation below it.
    match other_party_presence(state, chat_id) {
        Some(PresenceStatus::Online) => {
            Some(("online".to_string(), Style::new().fg(theme.text_muted)))
        }
        Some(PresenceStatus::Recently) => {
            Some(("recently".to_string(), Style::new().fg(theme.text_muted)))
        }
        Some(PresenceStatus::Offline) | None => None,
    }
}

/// `3/7`-style hit count for in-chat search (T47): `current_hit + 1` over
/// `search_hits.len()`, one-based since "hit 0 of 7" would read as an off-by
/// -one bug rather than the first hit. `None` whenever search isn't active
/// (`chat_search` is `None`) or hasn't found anything yet — a submitted
/// query with zero results, or one not yet answered, falls through to
/// typing/presence (module docs).
fn search_hit_count_label(state: &AppState, chat_id: ChatId) -> Option<String> {
    let search = state.chat_search.as_ref()?;
    let convo = state.conversations.get(&chat_id)?;
    if convo.search_hits.is_empty() {
        return None;
    }
    Some(format!(
        "{}/{}",
        search.current_hit + 1,
        convo.search_hits.len()
    ))
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
    use tgt_core::state::search::ChatSearchState;
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
                pending_view: None,
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
            crash_reports_available: false,
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

    // --- search hit-count indicator (T47) ---------------------------------

    /// `3/7`: `current_hit` is zero-based (2) so the label is one-based
    /// (3), over `search_hits.len()` (7).
    #[test]
    fn search_hit_count_shows_current_over_total_80x3() {
        let mut state = fixture_state();
        state.conversations.get_mut(&CHAT).unwrap().search_hits =
            (1..=7).map(tgt_core::model::ids::MessageId).collect();
        state.chat_search = Some(ChatSearchState {
            input: InputField::default(),
            current_hit: 2,
            in_flight: false,
        });

        let rendered = render_to_string(80, 3, &state);
        assert!(rendered.contains("3/7"), "hit count missing:\n{rendered}");
        insta::assert_snapshot!(rendered);
    }

    /// Search wins the right-hand slot over typing/presence while it has
    /// hits (module docs' "Search wins over typing/presence").
    #[test]
    fn search_hit_count_wins_over_typing_and_presence() {
        let mut state = fixture_state();
        state.presence.users.insert(USER, PresenceStatus::Online);
        state
            .presence
            .typing
            .insert((CHAT, USER), Millis(state.now.0 + 5_000));
        state.conversations.get_mut(&CHAT).unwrap().search_hits = vec![
            tgt_core::model::ids::MessageId(1),
            tgt_core::model::ids::MessageId(2),
        ];
        state.chat_search = Some(ChatSearchState {
            input: InputField::default(),
            current_hit: 0,
            in_flight: false,
        });

        let rendered = render_to_string(80, 3, &state);
        assert!(rendered.contains("1/2"));
        assert!(!rendered.contains("typing…"));
        assert!(!rendered.contains("online"));
    }

    /// The connection indicator still keeps first claim on the slot even
    /// once search has hits (unchanged priority: connection > search).
    #[test]
    fn connection_indicator_still_wins_over_search() {
        let mut state = fixture_state();
        state.connection = ConnectionPhase::Connecting;
        state.conversations.get_mut(&CHAT).unwrap().search_hits =
            vec![tgt_core::model::ids::MessageId(1)];
        state.chat_search = Some(ChatSearchState {
            input: InputField::default(),
            current_hit: 0,
            in_flight: false,
        });

        let rendered = render_to_string(80, 3, &state);
        assert!(rendered.contains("connecting…"));
        assert!(!rendered.contains("1/1"));
    }

    /// An open search overlay with no hits yet (query not submitted, or
    /// submitted and still in flight) falls through to typing/presence —
    /// opening search must not blank out the header's existing indicator.
    #[test]
    fn search_with_no_hits_falls_through_to_presence() {
        let mut state = fixture_state();
        state.presence.users.insert(USER, PresenceStatus::Online);
        state.chat_search = Some(ChatSearchState {
            input: InputField::default(),
            current_hit: 0,
            in_flight: true,
        });

        let rendered = render_to_string(80, 3, &state);
        assert!(rendered.contains("online"));
        assert!(!rendered.contains('/'));
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
