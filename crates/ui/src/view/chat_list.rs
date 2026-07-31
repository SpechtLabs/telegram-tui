//! Chat list sidebar (spec §6.1 sidebar mock, §7.2 theme tokens).
//!
//! `draw` fills the `CHATS` block's interior with rows from
//! `tgt_core::state::chat_list::visible_rows`. `crates/ui/src/view/root.rs`
//! currently draws a placeholder (empty) `CHATS` block itself and is owned by
//! another task; wiring `root::draw_sidebar` to call `chat_list::draw` is
//! left for the task that owns `root.rs` (T24 per docs/plan.md). This module
//! is exercised directly by its own tests in the meantime.
//!
//! ## Scroll window
//!
//! `ChatListState.scroll_offset` is core-owned state this view never writes
//! to — views only read `AppState`. Instead `scroll_offset` (the private fn
//! below, not the state field) recomputes a window offset from scratch every
//! frame: if the selected row's index already fits in the first `height`
//! rows the offset is 0, otherwise the offset is the smallest value that
//! puts the selection on the last visible row (`idx - height + 1`, clamped
//! so the window never runs past the end of the list). This is a pure
//! function of `(row count, viewport height, selected index)`, so it needs
//! no persisted scroll state and always agrees with the current selection.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use tgt_core::app::AppState;
use tgt_core::model::chat::ChatView;
use tgt_core::model::ids::ChatId;
use tgt_core::state::auth::InputField;
use tgt_core::state::chat_list::{ChatListState, visible_rows};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::Theme;

pub fn draw(area: Rect, state: &AppState, theme: &Theme, f: &mut Frame) {
    let block = Block::bordered()
        .title("CHATS")
        .title_style(Style::new().fg(theme.text))
        .border_style(Style::new().fg(theme.text_muted));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let list = &state.chat_list;
    let rows_area = match &list.filter {
        Some(filter) => {
            let [filter_area, rows_area] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
            draw_filter_input(filter_area, filter, theme, f);
            rows_area
        }
        None => inner,
    };

    let rows = visible_rows(list);
    if rows.is_empty() {
        draw_empty(rows_area, theme, f);
        return;
    }
    draw_rows(rows_area, &rows, list, theme, f);
}

/// Renders the `/`-prefixed filter line with a reverse-video cursor cell,
/// mirroring `view::auth::field_line`'s cursor treatment.
fn draw_filter_input(area: Rect, filter: &InputField, theme: &Theme, f: &mut Frame) {
    let chars: Vec<char> = filter.text.chars().collect();
    let cursor_chars = filter.text[..filter.cursor]
        .chars()
        .count()
        .min(chars.len());
    let base = Style::new().fg(theme.text);
    let cursor_style = Style::new().fg(theme.surface).bg(theme.accent);

    let mut spans = vec![Span::styled("/", Style::new().fg(theme.text_muted))];
    let before: String = chars[..cursor_chars].iter().collect();
    if !before.is_empty() {
        spans.push(Span::styled(before, base));
    }
    if cursor_chars < chars.len() {
        spans.push(Span::styled(chars[cursor_chars].to_string(), cursor_style));
        let after: String = chars[cursor_chars + 1..].iter().collect();
        if !after.is_empty() {
            spans.push(Span::styled(after, base));
        }
    } else {
        spans.push(Span::styled(" ", cursor_style));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_empty(area: Rect, theme: &Theme, f: &mut Frame) {
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "no chats",
            Style::new().fg(theme.text_muted),
        ))),
        area,
    );
}

fn draw_rows(area: Rect, rows: &[ChatId], list: &ChatListState, theme: &Theme, f: &mut Frame) {
    let height = area.height as usize;
    if height == 0 {
        return;
    }
    let selected_idx = list
        .selected
        .and_then(|sel| rows.iter().position(|&r| r == sel));
    let offset = scroll_offset(rows.len(), height, selected_idx);

    let lines: Vec<Line<'static>> = rows
        .iter()
        .skip(offset)
        .take(height)
        .filter_map(|chat_id| {
            let chat = list.chats.get(chat_id)?;
            let selected = list.selected == Some(*chat_id);
            Some(chat_row_line(chat, selected, area.width, theme))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

/// See the module docs' "Scroll window" section for the invariant this
/// implements: a pure, stateless clamp of the viewport to the selection.
fn scroll_offset(total: usize, height: usize, selected_idx: Option<usize>) -> usize {
    if height == 0 || total <= height {
        return 0;
    }
    let max_offset = total - height;
    match selected_idx {
        None => 0,
        Some(idx) => idx.saturating_sub(height - 1).min(max_offset),
    }
}

/// One sidebar row: `▸ ` selection marker, title (truncated to fit), and a
/// right-aligned unread badge (`@` prefix marks unread mentions, per spec
/// §6.1's sidebar mock). Selected rows paint `theme.selection` across the
/// full row width, not just the text, so padding spans carry the background
/// too.
fn chat_row_line(chat: &ChatView, selected: bool, width: u16, theme: &Theme) -> Line<'static> {
    let width = width as usize;
    let marker: &'static str = if selected { "▸ " } else { "  " };

    let badge = badge_text(chat);
    let badge_width = badge.as_deref().map(UnicodeWidthStr::width).unwrap_or(0);
    let badge_col = if badge_width > 0 { badge_width + 1 } else { 0 };

    let title_budget = width.saturating_sub(2 + badge_col);
    let title = truncate_to_width(&chat.title, title_budget);
    let used = 2 + title.width() + badge_col;
    let mid_pad = width.saturating_sub(used);

    let row_bg = selected.then_some(theme.selection);
    let with_row_bg = |mut style: Style| {
        if let Some(bg) = row_bg {
            style = style.bg(bg);
        }
        style
    };

    let marker_style = with_row_bg(if selected {
        Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(theme.text_muted)
    });
    let text_style = with_row_bg(if chat.is_muted {
        Style::new().fg(theme.text_muted)
    } else {
        Style::new().fg(theme.text)
    });

    let mut spans = vec![
        Span::styled(marker, marker_style),
        Span::styled(title, text_style),
    ];
    if mid_pad > 0 {
        spans.push(Span::styled(" ".repeat(mid_pad), with_row_bg(Style::new())));
    }
    if let Some(badge) = badge {
        spans.push(Span::styled(" ", with_row_bg(Style::new())));
        let mentioned = chat.unread_mention_count > 0;
        let badge_style = with_row_bg(if mentioned {
            Style::new().fg(theme.warning).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.accent)
        });
        spans.push(Span::styled(badge, badge_style));
    }
    Line::from(spans)
}

/// `None` when the row has nothing to show; otherwise the count, `@`-prefixed
/// when `unread_mention_count > 0` (spec §6.1: mentions get a distinct
/// marker rather than just a bigger number).
fn badge_text(chat: &ChatView) -> Option<String> {
    if chat.unread_count == 0 && chat.unread_mention_count == 0 {
        return None;
    }
    let mut s = String::new();
    if chat.unread_mention_count > 0 {
        s.push('@');
    }
    if chat.unread_count > 0 {
        s.push_str(&chat.unread_count.to_string());
    }
    Some(s)
}

/// Truncates to `budget` display columns, appending `…` when the text
/// doesn't fit. Column-aware (via `unicode-width`) so wide glyphs don't
/// overrun the sidebar.
fn truncate_to_width(text: &str, budget: usize) -> String {
    if budget == 0 {
        return String::new();
    }
    if text.width() <= budget {
        return text.to_string();
    }
    if budget == 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    let mut w = 0usize;
    for ch in text.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw > budget - 1 {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tgt_core::app::{AppState, Screen};
    use tgt_core::effect::TelemetryMode;
    use tgt_core::model::chat::{ChatKind, ChatListId, ChatOrderKey, ChatPositionEntry};
    use tgt_core::model::key::KeyBindings;
    use tgt_core::model::time::Millis;
    use tgt_core::state::auth::{AuthField, AuthState, LoginMethod};
    use tgt_core::state::chat_list::ChatLoadPhase;
    use tgt_core::state::composer::ComposerState;
    use tgt_core::state::consent::{ConsentChoice, ConsentState};
    use tgt_core::state::focus::{Focus, FocusStack};
    use tgt_core::state::media::MediaState;
    use tgt_core::state::presence::PresenceState;
    use tgt_core::state::toasts::ToastState;
    use tgt_core::td::update::{AuthPhase, ConnectionPhase};

    use super::*;

    fn fixture_state(chat_list: ChatListState) -> AppState {
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
                method: Some(LoginMethod::Phone),
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
            now: Millis(0),
        }
    }

    fn chat(id: i64, title: &str, unread: u32, mention: u32, muted: bool) -> ChatView {
        ChatView {
            id: ChatId(id),
            kind: ChatKind::Private,
            title: title.to_string(),
            positions: vec![ChatPositionEntry {
                list: ChatListId::Main,
                order: 900 - id * 10,
                is_pinned: false,
            }],
            unread_count: unread,
            unread_mention_count: mention,
            last_message: None,
            is_muted: muted,
        }
    }

    /// Eight chats spanning every badge/mute combination this view renders,
    /// in the order the spec §6.1 mock shows (Alice first, Archived near the
    /// bottom).
    fn seeded_chats() -> Vec<ChatView> {
        vec![
            chat(1, "Alice", 2, 0, false),
            chat(2, "Team Rust", 9, 0, false),
            chat(3, "Mom", 0, 0, true),
            chat(4, "#rust-de", 1, 1, false),
            chat(5, "Bob", 0, 0, false),
            chat(6, "Archived", 12, 0, false),
            chat(7, "Design Team", 0, 0, true),
            chat(8, "Carol", 5, 2, false),
        ]
    }

    fn seeded_chat_list(selected: Option<i64>) -> ChatListState {
        let mut chats = HashMap::new();
        let mut orders = BTreeSet::new();
        for chat in seeded_chats() {
            orders.insert(ChatOrderKey {
                order: chat.positions[0].order,
                chat_id: chat.id,
            });
            chats.insert(chat.id, chat);
        }
        ChatListState {
            chats,
            orders: HashMap::from([(ChatListId::Main, orders)]),
            active_list: ChatListId::Main,
            selected: selected.map(ChatId),
            filter: None,
            scroll_offset: 0,
            load: ChatLoadPhase::Complete,
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
    fn populated_list_with_badges_and_selection_120x40() {
        let state = fixture_state(seeded_chat_list(Some(1)));
        insta::assert_snapshot!(render_to_string(120, 40, &state));
    }

    #[test]
    fn filtered_list_120x40() {
        let mut list = seeded_chat_list(Some(2));
        list.filter = Some(InputField {
            text: "team".to_string(),
            cursor: 4,
        });
        let state = fixture_state(list);
        let rendered = render_to_string(120, 40, &state);
        assert!(rendered.contains("Team Rust"));
        assert!(!rendered.contains("Alice"));
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn empty_state_120x40() {
        let state = fixture_state(ChatListState::default());
        let rendered = render_to_string(120, 40, &state);
        assert!(rendered.contains("no chats"));
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn scroll_offset_keeps_selection_visible_without_over_scrolling() {
        // Selection inside the first window: no scroll needed.
        assert_eq!(scroll_offset(20, 5, Some(2)), 0);
        // Selection past the window: scroll the minimum to reveal it.
        assert_eq!(scroll_offset(20, 5, Some(10)), 6);
        // Never scrolls past the point where the last row is at the bottom.
        assert_eq!(scroll_offset(20, 5, Some(19)), 15);
        // Short lists never scroll.
        assert_eq!(scroll_offset(3, 5, Some(2)), 0);
        // No selection: show the top of the list.
        assert_eq!(scroll_offset(20, 5, None), 0);
    }

    #[test]
    fn badge_text_prefixes_mentions_and_hides_when_zero() {
        assert_eq!(badge_text(&chat(1, "x", 0, 0, false)), None);
        assert_eq!(
            badge_text(&chat(1, "x", 3, 0, false)),
            Some("3".to_string())
        );
        assert_eq!(
            badge_text(&chat(1, "x", 3, 1, false)),
            Some("@3".to_string())
        );
    }

    #[test]
    fn truncate_to_width_appends_ellipsis_when_over_budget() {
        assert_eq!(truncate_to_width("hello", 10), "hello");
        assert_eq!(truncate_to_width("hello world", 6), "hello…");
        assert_eq!(truncate_to_width("hello", 0), "");
        assert_eq!(truncate_to_width("hello", 1), "…");
    }
}
