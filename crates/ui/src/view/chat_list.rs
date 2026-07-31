//! Chat list sidebar (spec §6.1 sidebar mock, §7.2 theme tokens, §11 sidebar
//! organization).
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
//!
//! ## Sidebar organization (T43, spec §11)
//!
//! Three header lines above the row list, each drawn only when relevant, in
//! this fixed order:
//!
//! 1. An "esc/a  back" hint, only while `active_list == Archive` (the block
//!    title also becomes `ARCHIVE` in that state).
//! 2. A folder tab strip (`Main · Folder 1 · Folder 2`, active one in
//!    accent), only when `folder_cycle` has more than just `Main` and the
//!    list isn't the archive. There is no folder *name* anywhere in the
//!    model (`ChatListId::Folder` is a bare `i32`), so tabs are labelled by
//!    id; a future task that projects `ChatFolderInfo` titles can replace
//!    `folder_label` without touching layout.
//! 3. The existing `/` filter input line (T15/T22), unchanged.
//!
//! Below those, the row list itself is `visible_rows` (already pinned-first,
//! see `state::chat_list`'s docs) with two additions folded into the same
//! scroll window so selection math stays a single pure function of `(row
//! count, height, selected index)`:
//!
//! - A non-selectable archive pseudo-row at the very top, when
//!   `active_list == Main` and `archive_visible`. It shows the summed
//!   unread count across archived chats and is reachable only by the `a`
//!   key (core's `handle_key_chat_list`), never by `↑`/`↓` — `selected_idx`
//!   is computed by locating the selected `ChatId` in the combined row list,
//!   which never includes `DisplayRow::Archive`, so arrow-key navigation
//!   can't land on it.
//! - A dim horizontal rule between the pinned and unpinned groups, only
//!   when both are non-empty within the current `visible_rows` result.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use tgt_core::app::AppState;
use tgt_core::model::chat::{ChatListId, ChatView};
use tgt_core::model::ids::ChatId;
use tgt_core::state::auth::InputField;
use tgt_core::state::chat_list::{
    ChatListState, archive_unread_total, archive_visible, folder_cycle, visible_rows,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::Theme;

/// One line in the combined, scroll-windowed row list: the real chat rows
/// plus the two synthetic rows this task adds (see the module docs).
enum DisplayRow {
    Archive(u32),
    PinnedSeparator,
    Chat(ChatId),
}

pub fn draw(area: Rect, state: &AppState, theme: &Theme, f: &mut Frame) {
    let list = &state.chat_list;
    let is_archive = list.active_list == ChatListId::Archive;

    let block = Block::bordered()
        .title(if is_archive { "ARCHIVE" } else { "CHATS" })
        .title_style(Style::new().fg(theme.text))
        .border_style(Style::new().fg(theme.text_muted));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let folders = folder_cycle(list);
    let show_folder_tabs = !is_archive && folders.len() > 1;

    let mut constraints = Vec::new();
    if is_archive {
        constraints.push(Constraint::Length(1));
    }
    if show_folder_tabs {
        constraints.push(Constraint::Length(1));
    }
    if list.filter.is_some() {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(0));

    let areas = Layout::vertical(constraints).split(inner);
    let mut next = 0usize;
    if is_archive {
        draw_archive_hint(areas[next], theme, f);
        next += 1;
    }
    if show_folder_tabs {
        draw_folder_tabs(areas[next], &folders, list.active_list, theme, f);
        next += 1;
    }
    if let Some(filter) = &list.filter {
        draw_filter_input(areas[next], filter, theme, f);
        next += 1;
    }
    let rows_area = areas[next];

    let rows = visible_rows(list);
    let show_archive_row = list.active_list == ChatListId::Main && archive_visible(list);
    let display = build_display_rows(&rows, list, show_archive_row);
    if display.is_empty() {
        draw_empty(rows_area, theme, f);
        return;
    }
    draw_display_rows(rows_area, &display, list, theme, f);
}

/// `visible_rows` is already pinned-first (see `state::chat_list`); this
/// only finds the boundary (via the same `is_pinned` predicate `visible_rows`
/// partitions on) to place the separator and, optionally, prepends the
/// archive pseudo-row.
fn build_display_rows(
    rows: &[ChatId],
    list: &ChatListState,
    show_archive_row: bool,
) -> Vec<DisplayRow> {
    let mut display = Vec::with_capacity(rows.len() + 2);
    if show_archive_row {
        display.push(DisplayRow::Archive(archive_unread_total(list)));
    }
    let pinned_count = rows.iter().take_while(|id| is_pinned(list, **id)).count();
    for (i, id) in rows.iter().enumerate() {
        if i == pinned_count && pinned_count > 0 && pinned_count < rows.len() {
            display.push(DisplayRow::PinnedSeparator);
        }
        display.push(DisplayRow::Chat(*id));
    }
    display
}

fn is_pinned(list: &ChatListState, id: ChatId) -> bool {
    list.chats
        .get(&id)
        .and_then(|c| c.positions.iter().find(|p| p.list == list.active_list))
        .map(|p| p.is_pinned)
        .unwrap_or(false)
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

fn draw_display_rows(
    area: Rect,
    display: &[DisplayRow],
    list: &ChatListState,
    theme: &Theme,
    f: &mut Frame,
) {
    let height = area.height as usize;
    if height == 0 {
        return;
    }
    // Only `DisplayRow::Chat` participates in selection, so the archive
    // pseudo-row and the separator are never a scroll anchor — they just
    // ride along in whatever window a real chat's selection produces.
    let selected_idx = list.selected.and_then(|sel| {
        display
            .iter()
            .position(|row| matches!(row, DisplayRow::Chat(id) if *id == sel))
    });
    let offset = scroll_offset(display.len(), height, selected_idx);

    let lines: Vec<Line<'static>> = display
        .iter()
        .skip(offset)
        .take(height)
        .map(|row| match row {
            DisplayRow::Archive(unread) => archive_row_line(*unread, theme),
            DisplayRow::PinnedSeparator => separator_line(area.width, theme),
            DisplayRow::Chat(chat_id) => match list.chats.get(chat_id) {
                Some(chat) => {
                    let selected = list.selected == Some(*chat_id);
                    chat_row_line(chat, selected, area.width, theme)
                }
                // `visible_rows` only yields ids present in `list.chats`, so
                // this is unreachable in practice; an empty line rather than
                // a panic keeps a stale id from crashing the render.
                None => Line::default(),
            },
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

/// "  Archived  N" per the spec §6.1 mock's `Archived  12` row, muted since
/// it's an info row rather than a chat. Omits the count when it's zero
/// (archived chats can all be read).
fn archive_row_line(unread: u32, theme: &Theme) -> Line<'static> {
    let text = if unread > 0 {
        format!("  Archived  {unread}")
    } else {
        "  Archived".to_string()
    };
    Line::from(Span::styled(text, Style::new().fg(theme.text_muted)))
}

/// Dim rule marking the pinned/unpinned boundary (spec §11: "pinned chats
/// above the list"). Deliberately unlabelled — the position alone conveys
/// the split without repeating "pinned" on every render.
fn separator_line(width: u16, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width as usize),
        Style::new().fg(theme.text_muted),
    ))
}

fn draw_archive_hint(area: Rect, theme: &Theme, f: &mut Frame) {
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "esc/a  back",
            Style::new().fg(theme.text_muted),
        ))),
        area,
    );
}

/// `Main · Folder 1 · Folder 2`, active entry in accent+bold. `folders` is
/// `folder_cycle`'s output, already ordered and Archive-free.
fn draw_folder_tabs(
    area: Rect,
    folders: &[ChatListId],
    active: ChatListId,
    theme: &Theme,
    f: &mut Frame,
) {
    let mut spans = Vec::with_capacity(folders.len() * 2);
    for (i, id) in folders.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::new().fg(theme.text_muted)));
        }
        let style = if *id == active {
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.text_muted)
        };
        spans.push(Span::styled(folder_label(*id), style));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// See the module docs: there is no folder title anywhere in the model yet,
/// so folders are labelled by id.
fn folder_label(id: ChatListId) -> String {
    match id {
        ChatListId::Main => "Main".to_string(),
        ChatListId::Folder(n) => format!("Folder {n}"),
        // `folder_cycle` never yields this; kept exhaustive rather than
        // `unreachable!()` so a future change to that invariant degrades
        // instead of panicking mid-render.
        ChatListId::Archive => "Archive".to_string(),
    }
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

    /// A chat with a position in an arbitrary list, pinned or not — the
    /// general form `chat` (Main-only, unpinned) is built on top of.
    fn chat_in(
        id: i64,
        title: &str,
        list: ChatListId,
        order: i64,
        pinned: bool,
        unread: u32,
    ) -> ChatView {
        ChatView {
            id: ChatId(id),
            kind: ChatKind::Private,
            title: title.to_string(),
            positions: vec![ChatPositionEntry {
                list,
                order,
                is_pinned: pinned,
            }],
            unread_count: unread,
            unread_mention_count: 0,
            last_message: None,
            is_muted: false,
        }
    }

    /// Two pinned + two unpinned Main chats (exercising the pinned section
    /// and its separator), one archived chat (the pseudo-row + its unread
    /// total), and two folders (the tab strip) — everything T43 adds, all
    /// visible at once since `active_list == Main`.
    fn sidebar_organization_chat_list() -> ChatListState {
        let seed = [
            chat_in(1, "Alice", ChatListId::Main, 900, true, 2),
            chat_in(2, "Boss", ChatListId::Main, 890, true, 0),
            chat_in(3, "Team Rust", ChatListId::Main, 800, false, 9),
            chat_in(4, "Mom", ChatListId::Main, 790, false, 0),
            chat_in(5, "Old Chat", ChatListId::Archive, 500, false, 12),
            chat_in(6, "Work Chat", ChatListId::Folder(1), 700, false, 0),
            chat_in(7, "News Chat", ChatListId::Folder(2), 600, false, 3),
        ];
        let mut chats = HashMap::new();
        let mut orders: HashMap<ChatListId, BTreeSet<ChatOrderKey>> = HashMap::new();
        for chat in seed {
            let position = chat.positions[0];
            orders
                .entry(position.list)
                .or_default()
                .insert(ChatOrderKey {
                    order: position.order,
                    chat_id: chat.id,
                });
            chats.insert(chat.id, chat);
        }
        ChatListState {
            chats,
            orders,
            active_list: ChatListId::Main,
            selected: Some(ChatId(1)),
            filter: None,
            scroll_offset: 0,
            load: ChatLoadPhase::Complete,
        }
    }

    #[test]
    fn sidebar_pinned_archive_and_folder_tabs_120x40() {
        let state = fixture_state(sidebar_organization_chat_list());
        let rendered = render_to_string(120, 40, &state);
        // Pinned chats, the archive row and its total, and the folder tabs
        // are all present.
        assert!(rendered.contains("Alice"));
        assert!(rendered.contains("Boss"));
        assert!(rendered.contains("Archived  12"));
        assert!(rendered.contains("Folder 1"));
        assert!(rendered.contains("Folder 2"));
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn archive_active_list_shows_archive_title_and_back_hint_120x40() {
        let mut list = sidebar_organization_chat_list();
        list.active_list = ChatListId::Archive;
        list.selected = Some(ChatId(5));
        let state = fixture_state(list);
        let rendered = render_to_string(120, 40, &state);
        assert!(rendered.contains("ARCHIVE"));
        assert!(rendered.contains("esc/a"));
        assert!(rendered.contains("Old Chat"));
        // The folder tab strip and the Main-list archive pseudo-row are both
        // archive-mode-only exclusions.
        assert!(!rendered.contains("Folder 1"));
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
