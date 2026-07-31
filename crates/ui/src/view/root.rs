//! Responsive root arrangement (spec §6.1). Two-pane at or above
//! `layout_breakpoint_cols`; a single-pane stack below it. Both arrangements
//! call the same content components — `chat_list::draw`, `conversation::draw`
//! — and differ only in the `Rect` arithmetic that feeds them; the stack is
//! not a second implementation.
//!
//! Which single-pane screen shows is a pure function of state: no open chat,
//! or focus still on the chat list / its filter, shows the list; otherwise
//! (a chat is open and focus has moved off the list, i.e. onto the composer
//! or selection mode) shows the conversation behind a breadcrumb. `Esc`'s
//! "back to the chat list" is core's job (T28's `escape`, which swaps the
//! focus base back to `Focus::ChatList` without touching `open_chat`) — this
//! view only reads the result.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use tgt_core::app::AppState;
use tgt_core::state::focus::Focus;

use crate::render::cache::LayoutCache;
use crate::theme::Theme;
use crate::view::{chat_list, conversation, header, hint_bar};

const SIDEBAR_WIDTH: u16 = 30;

/// `cache` is threaded down to the conversation pane, the only view that lays
/// messages out and therefore the only one that can hit or fill it.
pub fn draw(state: &AppState, theme: &Theme, f: &mut Frame, cache: &mut LayoutCache) {
    let area = f.area();
    f.render_widget(Block::new().style(Style::new().bg(theme.surface)), area);

    let outer = Block::bordered()
        .title(Line::from(" telegram-tui ").left_aligned())
        .border_style(Style::new().fg(theme.text_muted));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    if state.width >= state.layout_breakpoint_cols {
        draw_two_pane(inner, state, theme, f, cache);
    } else {
        draw_single_pane(inner, state, theme, f, cache);
    }
}

/// Two-pane arrangement (spec §6.1 mock): fixed sidebar, main column with the
/// chat header above the conversation, hint bar spanning the bottom.
fn draw_two_pane(
    area: Rect,
    state: &AppState,
    theme: &Theme,
    f: &mut Frame,
    cache: &mut LayoutCache,
) {
    let [content_area, hint_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);

    let [sidebar_area, main_area] =
        Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)])
            .areas(content_area);

    chat_list::draw(sidebar_area, state, theme, f);

    let [header_area, body_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(main_area);
    header::draw(header_area, state, theme, f);
    draw_conversation_and_composer(body_area, state, theme, f, cache);

    // T32 wires selection mode's chip row (view::chips::draw) in place of
    // the hint bar.
    hint_bar::draw(hint_area, theme, f);
}

/// Single-pane stack below the breakpoint (spec §6.1): full-width chat list,
/// or a breadcrumb (`telegram ▸ <chat title>`) over a full-width conversation
/// once a chat is open and focus has left the list.
fn draw_single_pane(
    area: Rect,
    state: &AppState,
    theme: &Theme,
    f: &mut Frame,
    cache: &mut LayoutCache,
) {
    let showing_chat_list = state.open_chat.is_none()
        || matches!(state.focus.current(), Focus::ChatList | Focus::ChatFilter);

    if showing_chat_list {
        let [list_area, hint_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
        chat_list::draw(list_area, state, theme, f);
        hint_bar::draw(hint_area, theme, f);
        return;
    }

    let [breadcrumb_area, body_area, hint_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_breadcrumb(breadcrumb_area, state, theme, f);
    draw_conversation_and_composer(body_area, state, theme, f, cache);
    // T32 wires selection mode's chip row (view::chips::draw) in place of
    // the hint bar.
    hint_bar::draw(hint_area, theme, f);
}

/// `telegram ▸ <chat title>` (spec §6.1): the single-pane stack's back
/// affordance, standing in for the two-pane sidebar. Only reached once
/// `draw_single_pane` has established `open_chat` is `Some`.
fn draw_breadcrumb(area: Rect, state: &AppState, theme: &Theme, f: &mut Frame) {
    let title = state
        .open_chat
        .and_then(|id| state.chat_list.chats.get(&id))
        .map(|chat| chat.title.as_str())
        .unwrap_or_default();

    let line = Line::from(vec![
        Span::styled("telegram ", Style::new().fg(theme.text)),
        Span::styled("▸ ", Style::new().fg(theme.accent)),
        Span::styled(title.to_string(), Style::new().fg(theme.text)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// The conversation viewport and the composer beneath it: the part of the
/// layout both arrangements show identically once a chat is (or can be)
/// open. Factored out so neither arrangement duplicates this rendering — the
/// two-pane and single-pane call sites only differ in what `Rect` they hand
/// in and what sits above it (the chat header vs. the breadcrumb).
fn draw_conversation_and_composer(
    area: Rect,
    state: &AppState,
    theme: &Theme,
    f: &mut Frame,
    cache: &mut LayoutCache,
) {
    let [conversation_area, composer_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(area);

    conversation::draw(conversation_area, state, theme, f, cache);
    draw_composer_placeholder(composer_area, theme, f);
}

/// Composer placeholder: real input handling lands in T30. T32 wires
/// `view::composer::draw` in here once that module has a real implementation
/// (it is still a stub in this worktree).
fn draw_composer_placeholder(area: Rect, theme: &Theme, f: &mut Frame) {
    let composer_block = Block::bordered().border_style(Style::new().fg(theme.text_muted));
    let composer_inner = composer_block.inner(area);
    f.render_widget(composer_block, area);
    f.render_widget(
        Paragraph::new("›  message…").style(Style::new().fg(theme.text_muted)),
        composer_inner,
    );
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tgt_core::app::{AppState, Screen};
    use tgt_core::effect::TelemetryMode;
    use tgt_core::model::chat::{ChatKind, ChatOrderKey, ChatView};
    use tgt_core::model::ids::ChatId;
    use tgt_core::model::key::KeyBindings;
    use tgt_core::model::time::Millis;
    use tgt_core::state::auth::{AuthField, AuthState, InputField};
    use tgt_core::state::chat_list::ChatListState;
    use tgt_core::state::composer::ComposerState;
    use tgt_core::state::consent::{ConsentChoice, ConsentState};
    use tgt_core::state::conversation::{ConversationState, Scroll};
    use tgt_core::state::focus::FocusStack;
    use tgt_core::state::history::PagingState;
    use tgt_core::state::media::MediaState;
    use tgt_core::state::presence::PresenceState;
    use tgt_core::state::toasts::ToastState;
    use tgt_core::td::update::{AuthPhase, ConnectionPhase};

    use super::*;

    const CHAT: ChatId = ChatId(1);

    fn chat_list_with_one_chat() -> ChatListState {
        let mut list = ChatListState::default();
        list.chats.insert(
            CHAT,
            ChatView {
                id: CHAT,
                kind: ChatKind::Private,
                title: "Alice Müller".to_string(),
                positions: Vec::new(),
                unread_count: 2,
                unread_mention_count: 0,
                last_message: None,
                is_muted: false,
            },
        );
        let mut orders = BTreeSet::new();
        orders.insert(ChatOrderKey {
            order: 1,
            chat_id: CHAT,
        });
        list.orders.insert(list.active_list, orders);
        list.selected = Some(CHAT);
        list
    }

    /// `open_chat` and `focus` are the two knobs the tests below flip: which
    /// single-pane screen shows is a pure function of them (module docs).
    fn fixture_state(width: u16, open_chat: Option<ChatId>, focus: FocusStack) -> AppState {
        let mut conversations = HashMap::new();
        if let Some(chat_id) = open_chat {
            conversations.insert(
                chat_id,
                ConversationState {
                    chat_id,
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
        }
        AppState {
            screen: Screen::Main,
            focus,
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
            chat_list: chat_list_with_one_chat(),
            conversations,
            open_chat,
            composer: ComposerState::default(),
            modal_ui: None,
            palette: None,
            chat_search: None,
            toasts: ToastState::default(),
            media: MediaState::default(),
            presence: PresenceState::default(),
            width,
            height: 30,
            layout_breakpoint_cols: 100,
            theme_name: "dark".to_string(),
            theme_generation: 0,
            bindings: KeyBindings::default(),
            telemetry_mode: TelemetryMode::Off,
            telemetry_salt: [0u8; 32],
            now: Millis(0),
        }
    }

    fn render_to_string(width: u16, height: u16, state: &AppState) -> String {
        let theme = Theme::default_dark();
        let mut cache = LayoutCache::new();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| {
                draw(state, &theme, f, &mut cache);
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
    fn single_pane_stack_shows_full_width_chat_list_99x30() {
        let state = fixture_state(99, None, FocusStack::new(Focus::ChatList));
        let rendered = render_to_string(99, 30, &state);
        assert!(rendered.contains("Alice Müller"));
        assert!(rendered.contains(hint_bar::HINT_TEXT));
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn single_pane_stack_shows_breadcrumb_and_conversation_99x30() {
        let state = fixture_state(99, Some(CHAT), FocusStack::new(Focus::Composer));
        let rendered = render_to_string(99, 30, &state);
        assert!(rendered.contains("telegram"));
        assert!(rendered.contains("▸"));
        assert!(rendered.contains("Alice Müller"));
        assert!(rendered.contains("message…"));
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn two_pane_arrangement_at_breakpoint_100x30() {
        let state = fixture_state(100, Some(CHAT), FocusStack::new(Focus::Composer));
        let rendered = render_to_string(100, 30, &state);
        // Both panes visible at once: the sidebar's chat row and the
        // composer placeholder share a frame, which never happens in the
        // single-pane stack.
        assert!(rendered.contains("Alice Müller"));
        assert!(rendered.contains("message…"));
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn single_pane_stack_falls_back_to_chat_list_once_focus_returns_there() {
        // `Esc`'s back button (T28's `escape`): open_chat stays `Some`, only
        // the focus base swaps back to `Focus::ChatList`. The view must
        // follow focus, not `open_chat`, or "back" would render nothing new.
        let state = fixture_state(99, Some(CHAT), FocusStack::new(Focus::ChatList));
        let rendered = render_to_string(99, 30, &state);
        assert!(rendered.contains(hint_bar::HINT_TEXT));
        assert!(!rendered.contains("telegram ▸"));
        assert!(!rendered.contains("message…"));
    }
}
