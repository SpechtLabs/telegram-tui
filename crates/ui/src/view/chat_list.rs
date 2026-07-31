//! Chat list sidebar (spec §6.1 sidebar mock, §7.2 theme tokens, §11 sidebar
//! organization; docs/design-language.md §1/§5 for its chrome).
//!
//! `draw` fills the area `view::root` hands it — already padded, never
//! bordered — with a dim `CHATS` (or `ARCHIVE`) section label over rows from
//! `tgt_core::state::chat_list::visible_rows`. The sidebar is a region, not
//! a widget: it draws no box, and the only line anywhere near it is the
//! vertical rule `root` puts between it and the conversation.
//!
//! ## Selection and badges (design language §5)
//!
//! The selected row is a `▏` bar in `accent` at the left edge plus a
//! `surface_raised` background across the row — never inverse video, which
//! reads as a solid block of noise and drowns the row's own hierarchy.
//! Unselected rows reserve the same two columns so titles stay aligned.
//! Unread counts are `accent` bold, mentions `warning`, right-aligned and
//! unbracketed; muted chats drop to `text_muted` entirely.
//!
//! ## Scroll window (T76)
//!
//! `ChatListState.scroll_offset` is core-owned state this view reads but
//! never writes — views only read `AppState`, so a wheel step never lands
//! here directly; core's `state::chat_list::scroll_viewport` owns that
//! mutation, and `draw` just interprets the result each frame. Rendering the
//! window is a two-step split that mirrors the split in the field's own doc
//! comment:
//!
//! - `display_offset_for_scroll` translates `scroll_offset` — an index into
//!   `visible_rows`, i.e. real chat rows only, since core has no notion of
//!   this view's synthetic `DisplayRow::Archive`/`PinnedSeparator` entries —
//!   into a start index over `display`. `scroll_offset == 0` always maps to
//!   display index `0` rather than to wherever `visible_rows()[0]` lands, so
//!   the archive pseudo-row (when present) still shows at the very top of an
//!   unscrolled list instead of always being one wheel-tick out of reach.
//! - `resolve_offset` then applies the one adjustment core *can't* make
//!   without knowing the pane's rendered height: if the selection sits below
//!   what the wheel-derived window currently shows, the window is pulled
//!   down (never up — core already guarantees `scroll_offset` never sits
//!   past `selected`'s row, so the window is never above it to begin with)
//!   just far enough to put the selection back on screen. This is what makes
//!   `↑`/`↓` recover from a wheel scroll that carried the viewport away from
//!   the selection, without a wheel scroll fighting `↑`/`↓` for control the
//!   rest of the time.
//!
//! Both functions are pure — `resolve_offset` over `(row count, viewport
//! height, wheel-derived offset, selected index)` — so nothing here needs
//! per-frame mutable state beyond what `AppState` already carries.
//!
//! ## Sidebar organization (T43, spec §11)
//!
//! Three header lines above the row list, each drawn only when relevant, in
//! this fixed order:
//!
//! 1. An "esc/a  back" hint, only while `active_list == Archive` (the section
//!    label also becomes `ARCHIVE` in that state).
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
//! `display` list the scroll window above operates over:
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
//!
//! ### Vertical rhythm (T75)
//!
//! With an archive hint, tabs, or a filter line on screen, the last header
//! line used to sit directly above row 0 of the list. Nothing told the eye
//! where the chrome ended and the rows began, so a folder tab strip read as
//! just another row and a pinned chat under it looked glued to the tabs.
//! `draw` now inserts one blank row after that header block — but only when
//! the block has grown past the bare `label, blank` pair, so the common case
//! (no tabs, not filtering, not in Archive) still costs the same two rows it
//! always did. That single blank is the whole fix: it is spent once, between
//! chrome and the list, not per optional line and not again in front of the
//! archive pseudo-row.
//!
//! The archive pseudo-row does *not* get its own separating blank before the
//! chat rows below it. It is deliberately laid out like a row (`archive_row_line`
//! shares `chat_row_line`'s two-column left inset) because it *is* one — the
//! top entry of the same list, just non-selectable — and splitting it off
//! with another blank would both cost a row we can't spare on a short
//! terminal and misrepresent it as chrome, which it isn't.
//!
//! The dim pinned/unpinned rule stays. It answers a different question than
//! the new blank row does — not "where does the list start" but "where do
//! pinned chats end" — and a blank line can't carry the same meaning inside
//! a list of otherwise-identical rows the way it can between chrome and
//! content. It's the one place in this view a rule still earns its keep.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use tgt_core::app::AppState;
use tgt_core::model::chat::{ChatListId, ChatView};
use tgt_core::model::hit::HitTarget;
use tgt_core::model::ids::ChatId;
use tgt_core::state::auth::InputField;
use tgt_core::state::chat_list::{
    ChatListState, archive_unread_total, archive_visible, folder_cycle, visible_rows,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::render::hit::HitMap;
use crate::theme::Theme;

/// One line in the combined, scroll-windowed row list: the real chat rows
/// plus the two synthetic rows this task adds (see the module docs).
enum DisplayRow {
    Archive(u32),
    PinnedSeparator,
    Chat(ChatId),
}

/// Columns the selection bar and its trailing space occupy. Reserved on
/// every row, selected or not, so titles never shift sideways as the
/// selection moves.
const MARKER_WIDTH: usize = 2;

pub fn draw(area: Rect, state: &AppState, theme: &Theme, f: &mut Frame, hits: &mut HitMap) {
    let list = &state.chat_list;
    let is_archive = list.active_list == ChatListId::Archive;

    let folders = folder_cycle(list);
    let show_folder_tabs = !is_archive && folders.len() > 1;

    // Section label, then a blank row, then whichever of the three header
    // lines apply, then — only when at least one of those optional lines is
    // present — one more blank row before the rows themselves (see the
    // module docs' "Vertical rhythm" section for why that row is
    // conditional).
    let has_extra_header_lines = is_archive || show_folder_tabs || list.filter.is_some();
    let mut constraints = vec![Constraint::Length(1), Constraint::Length(1)];
    if is_archive {
        constraints.push(Constraint::Length(1));
    }
    if show_folder_tabs {
        constraints.push(Constraint::Length(1));
    }
    if list.filter.is_some() {
        constraints.push(Constraint::Length(1));
    }
    if has_extra_header_lines {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(0));

    let areas = Layout::vertical(constraints).split(area);
    draw_section_label(areas[0], is_archive, theme, f);
    let mut next = 2usize;
    if is_archive {
        draw_archive_hint(areas[next], theme, f);
        next += 1;
    }
    if show_folder_tabs {
        draw_folder_tabs(areas[next], &folders, list.active_list, theme, f, hits);
        next += 1;
    }
    if let Some(filter) = &list.filter {
        draw_filter_input(areas[next], filter, theme, f);
        next += 1;
    }
    if has_extra_header_lines {
        next += 1; // the blank row separating chrome from the rows below
    }
    let rows_area = areas[next];

    let rows = visible_rows(list);
    let show_archive_row = list.active_list == ChatListId::Main && archive_visible(list);
    let display = build_display_rows(&rows, list, show_archive_row);
    if display.is_empty() {
        draw_empty(rows_area, theme, f);
        return;
    }
    draw_display_rows(rows_area, &display, list, theme, f, hits);
}

/// `CHATS` / `ARCHIVE` as a dim uppercase section label. It replaces the
/// bordered block's title: a label carries the same "this is the chat list"
/// meaning as a box did, at the cost of one row instead of four columns and
/// two rows of chrome (design language §1).
fn draw_section_label(area: Rect, is_archive: bool, theme: &Theme, f: &mut Frame) {
    let label = if is_archive { "ARCHIVE" } else { "CHATS" };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            label,
            Style::new()
                .fg(theme.text_muted)
                .add_modifier(Modifier::BOLD),
        ))),
        area,
    );
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
    hits: &mut HitMap,
) {
    let height = area.height as usize;
    if height == 0 {
        return;
    }
    // Only `DisplayRow::Chat` participates in selection, so the archive
    // pseudo-row and the separator are never a scroll anchor — they just
    // ride along in whatever window the wheel or the selection produces.
    let selected_idx = list.selected.and_then(|sel| {
        display
            .iter()
            .position(|row| matches!(row, DisplayRow::Chat(id) if *id == sel))
    });
    let core_offset = display_offset_for_scroll(display, list.scroll_offset);
    let offset = resolve_offset(display.len(), height, core_offset, selected_idx);

    // The hit region for a row is recorded here, off the same
    // `(offset, index)` pair that decides where the row is painted, so the
    // two can't drift apart the way a second pass over `display` could. The
    // pinned separator is deliberately unclickable: it is a rule, not a row.
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(height.min(display.len()));
    for (i, row) in display.iter().skip(offset).take(height).enumerate() {
        let row_rect = Rect {
            x: area.x,
            y: area.y + i as u16,
            width: area.width,
            height: 1,
        };
        let line = match row {
            DisplayRow::Archive(unread) => {
                hits.push(row_rect, HitTarget::ArchiveRow);
                archive_row_line(*unread, theme)
            }
            DisplayRow::PinnedSeparator => separator_line(area.width, theme),
            DisplayRow::Chat(chat_id) => match list.chats.get(chat_id) {
                Some(chat) => {
                    hits.push(row_rect, HitTarget::ChatRow(*chat_id));
                    let selected = list.selected == Some(*chat_id);
                    chat_row_line(chat, selected, area.width, theme)
                }
                // `visible_rows` only yields ids present in `list.chats`, so
                // this is unreachable in practice; an empty line rather than
                // a panic keeps a stale id from crashing the render.
                None => Line::default(),
            },
        };
        lines.push(line);
    }
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
/// the split without repeating "pinned" on every render — and drawn in
/// `border`, the token reserved for chrome.
fn separator_line(width: u16, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width as usize),
        Style::new().fg(theme.border),
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
///
/// Each label's own columns become its hit region; the ` · ` separators
/// between them are not part of any tab, so a click that lands on one does
/// nothing rather than picking a neighbour arbitrarily.
fn draw_folder_tabs(
    area: Rect,
    folders: &[ChatListId],
    active: ChatListId,
    theme: &Theme,
    f: &mut Frame,
    hits: &mut HitMap,
) {
    const SEPARATOR: &str = " · ";

    let mut spans = Vec::with_capacity(folders.len() * 2);
    let mut col = 0u16;
    for (i, id) in folders.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(SEPARATOR, Style::new().fg(theme.border)));
            col += SEPARATOR.width() as u16;
        }
        let style = if *id == active {
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.text_muted)
        };
        let label = folder_label(*id);
        let label_width = label.width() as u16;
        // A strip wider than the pane is clipped by the `Paragraph`, so the
        // regions are clipped the same way: no tab is clickable past the
        // right edge, and one straddling it is only clickable where it shows.
        let visible_width = label_width.min(area.width.saturating_sub(col));
        hits.push(
            Rect {
                x: area.x + col,
                y: area.y,
                width: visible_width,
                height: 1,
            },
            HitTarget::FolderTab(*id),
        );
        col += label_width;
        spans.push(Span::styled(label, style));
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

/// Translates `scroll_offset` — core's index into `visible_rows`, i.e. real
/// chat rows only — into a start index over `display`, which additionally
/// carries the archive pseudo-row and the pinned/unpinned separator this
/// view inserts. See the module docs' "Scroll window" section.
///
/// `scroll_offset == 0` is special-cased to display index `0` rather than
/// computed like every other offset, so the archive row (when present)
/// still shows at the very top of an unscrolled list: core's index space
/// has no notion of that synthetic row, so without this case `scroll_offset
/// == 0` would map to wherever `visible_rows()[0]` lands in `display`,
/// permanently scrolling the archive row one tick above reach.
fn display_offset_for_scroll(display: &[DisplayRow], scroll_offset: usize) -> usize {
    if scroll_offset == 0 {
        return 0;
    }
    let mut seen = 0usize;
    for (i, row) in display.iter().enumerate() {
        if matches!(row, DisplayRow::Chat(_)) {
            if seen == scroll_offset {
                return i;
            }
            seen += 1;
        }
    }
    // `scroll_offset` pointed past the last chat row — stale relative to a
    // `display` that just shrank (e.g. a filter narrowing the results in
    // the same frame core computed it against a wider list). Falling back
    // to the last row keeps this in bounds rather than reading past the end
    // of `display`; core's own clamp corrects the field by the next frame.
    display.len().saturating_sub(1)
}

/// See the module docs' "Scroll window" section for the invariant this
/// implements: `core_offset` (the wheel's, via
/// `display_offset_for_scroll`) is the starting point, and the only
/// adjustment made here is pulling the window down to keep `selected_idx`
/// on screen from below — core already guarantees `core_offset` never sits
/// past the selection, so this never needs to pull the window back up.
fn resolve_offset(
    total: usize,
    height: usize,
    core_offset: usize,
    selected_idx: Option<usize>,
) -> usize {
    if height == 0 || total <= height {
        return 0;
    }
    let max_offset = total - height;
    let offset = core_offset.min(max_offset);
    match selected_idx {
        Some(idx) if idx >= offset + height => idx.saturating_sub(height - 1).min(max_offset),
        _ => offset,
    }
}

/// One sidebar row: the `▏` selection bar, the title (truncated to fit), and
/// a right-aligned unread badge (`@` prefix marks unread mentions, per spec
/// §6.1's sidebar mock). A selected row paints `surface_raised` across its
/// full width, not just the text, so the padding spans carry the background
/// too — the bar and that wash are the entire selection treatment
/// (design language §5).
fn chat_row_line(chat: &ChatView, selected: bool, width: u16, theme: &Theme) -> Line<'static> {
    let width = width as usize;

    let badge = badge_text(chat);
    let badge_width = badge.as_deref().map(UnicodeWidthStr::width).unwrap_or(0);
    let badge_col = if badge_width > 0 { badge_width + 1 } else { 0 };

    let title_budget = width.saturating_sub(MARKER_WIDTH + badge_col);
    let title = truncate_to_width(&chat.title, title_budget);
    let used = MARKER_WIDTH + title.width() + badge_col;
    let mid_pad = width.saturating_sub(used);

    let row_bg = selected.then_some(theme.surface_raised);
    let with_row_bg = |mut style: Style| {
        if let Some(bg) = row_bg {
            style = style.bg(bg);
        }
        style
    };

    let text_style = with_row_bg(if chat.is_muted {
        Style::new().fg(theme.text_muted)
    } else {
        Style::new().fg(theme.text)
    });

    let mut spans = vec![
        Span::styled(
            if selected { "▏" } else { " " },
            with_row_bg(Style::new().fg(theme.accent)),
        ),
        Span::styled(" ", with_row_bg(Style::new())),
        Span::styled(title, text_style),
    ];
    if mid_pad > 0 {
        spans.push(Span::styled(" ".repeat(mid_pad), with_row_bg(Style::new())));
    }
    if let Some(badge) = badge {
        spans.push(Span::styled(" ", with_row_bg(Style::new())));
        let mentioned = chat.unread_mention_count > 0;
        let badge_style = with_row_bg(
            Style::new()
                .fg(if mentioned {
                    theme.warning
                } else {
                    theme.accent
                })
                .add_modifier(Modifier::BOLD),
        );
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
                draw(area, state, &theme, f, &mut HitMap::new());
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

    /// Same drive as [`render_to_string`], but keeps the [`HitMap`] `draw`
    /// filled in alongside the flattened buffer, so a test can look up
    /// where a row painted and then probe that exact cell (T76: proving a
    /// click still resolves the right chat after a wheel scroll).
    fn render_with_hits(width: u16, height: u16, state: &AppState) -> (String, HitMap) {
        let theme = Theme::default_dark();
        let mut hits = HitMap::new();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                draw(area, state, &theme, f, &mut hits);
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
        (out, hits)
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

    /// A cramped sidebar (T75): with tabs, the archive row and two pinned
    /// chats all wanting space, an 8-row pane only has room for a handful of
    /// rows below the new rhythm blank. This must not overflow the area or
    /// panic — `Layout::vertical`'s `Min(0)` for the row list is what keeps
    /// it from doing so — and the selected row must still land inside the
    /// pane.
    #[test]
    fn cramped_height_with_tabs_does_not_overflow_120x8() {
        let state = fixture_state(sidebar_organization_chat_list());
        let rendered = render_to_string(120, 8, &state);
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines.len(), 8);
        assert!(rendered.contains("Main · Folder 1 · Folder 2"));
        // Header (label, blank, tabs, blank) plus at least the archive row
        // and the selected pinned chat must fit.
        assert!(rendered.contains("Alice"));
        insta::assert_snapshot!(rendered);
    }

    /// A pane shorter than the header block itself (label + blank + tabs +
    /// blank is 4 rows) must still render without panicking; `ratatui`
    /// shrinks the `Length` constraints rather than overflowing.
    #[test]
    fn height_shorter_than_header_does_not_panic_120x2() {
        let state = fixture_state(sidebar_organization_chat_list());
        let rendered = render_to_string(120, 2, &state);
        assert_eq!(rendered.lines().count(), 2);
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
    fn resolve_offset_follows_selection_down_but_never_up() {
        // No wheel scroll (core_offset 0), selection inside the first
        // window already: no adjustment needed.
        assert_eq!(resolve_offset(20, 5, 0, Some(2)), 0);
        // Selection past the window: pulled down just enough to reveal it.
        assert_eq!(resolve_offset(20, 5, 0, Some(10)), 6);
        // Never scrolls past the point where the last row is at the bottom.
        assert_eq!(resolve_offset(20, 5, 0, Some(19)), 15);
        // Short lists never scroll, whatever core_offset claims.
        assert_eq!(resolve_offset(3, 5, 2, Some(2)), 0);
        // No selection: honour core_offset as-is (clamped to the end).
        assert_eq!(resolve_offset(20, 5, 0, None), 0);
        assert_eq!(resolve_offset(20, 5, 4, None), 4);
        assert_eq!(resolve_offset(20, 5, 999, None), 15);
        // A wheel scroll that already shows the selection is left alone —
        // it is not pulled back up toward the top of the window.
        assert_eq!(resolve_offset(20, 5, 8, Some(9)), 8);
        // A wheel scroll that left the selection below the window still
        // gets pulled down to reveal it, same as the no-scroll case.
        assert_eq!(resolve_offset(20, 5, 0, Some(12)), 8);
    }

    #[test]
    fn display_offset_for_scroll_maps_visible_rows_index_into_display() {
        let display = vec![
            DisplayRow::Archive(3),
            DisplayRow::Chat(ChatId(1)),
            DisplayRow::Chat(ChatId(2)),
            DisplayRow::PinnedSeparator,
            DisplayRow::Chat(ChatId(3)),
            DisplayRow::Chat(ChatId(4)),
        ];
        // scroll_offset 0 always lands on display index 0, so the archive
        // row shows at the top of an unscrolled list even though it has no
        // index of its own in core's `visible_rows`-space.
        assert_eq!(display_offset_for_scroll(&display, 0), 0);
        // scroll_offset 1 is the second real chat row (ChatId(2)) — the
        // archive row is skipped, not counted.
        assert_eq!(display_offset_for_scroll(&display, 1), 2);
        // scroll_offset 2 is the third chat row (ChatId(3)); the separator
        // sitting right before it in `display` is skipped too.
        assert_eq!(display_offset_for_scroll(&display, 2), 4);
        // Past the last chat row: falls back to the end of `display` rather
        // than panicking or reading out of bounds.
        assert_eq!(display_offset_for_scroll(&display, 99), display.len() - 1);
    }

    /// The bug this task fixes: a wheel scroll used to move the selection,
    /// because the render window was derived purely from `selected`. Now
    /// `list.scroll_offset` alone drives the window, and it can leave the
    /// selection scrolled off-screen — exactly what a real wheel scroll
    /// does before the user next presses `↑`/`↓`.
    #[test]
    fn scroll_offset_moves_the_window_independent_of_selection() {
        let mut list = seeded_chat_list(Some(1)); // selected: Alice, row 0
        list.scroll_offset = 3; // wheel-scrolled to "#rust-de", row 3
        let state = fixture_state(list);
        // Header is 2 rows (no tabs/filter/archive here); 3 more give a
        // rows_area exactly 3 rows tall — less than the 8 total rows, so
        // the window actually has something to hide.
        let rendered = render_to_string(120, 5, &state);
        assert!(rendered.contains("#rust-de"));
        assert!(rendered.contains("Bob"));
        // Alice is selected but scrolled off above the window — the wheel
        // moved the viewport, not the selection, and this state must not
        // be "corrected" back to showing the selection.
        assert!(!rendered.contains("Alice"));
    }

    /// The wheel-driven window still honours a keyboard-moved selection
    /// once it falls off the *bottom* — the one adjustment `resolve_offset`
    /// is allowed to make, since core can't do it without the pane height.
    #[test]
    fn keyboard_selection_below_the_window_pulls_it_back_into_view() {
        let mut list = seeded_chat_list(Some(8)); // selected: Carol, row 7 (last)
        list.scroll_offset = 0; // no wheel scroll: fresh, top of the list
        let state = fixture_state(list);
        let rendered = render_to_string(120, 5, &state);
        assert!(rendered.contains("Carol"));
    }

    /// Scrolling away from the top (T76) hides the archive pseudo-row along
    /// with the pinned chats above the scroll target — `scroll_offset == 0`
    /// is the only value that shows it, per `display_offset_for_scroll`'s
    /// contract.
    #[test]
    fn scrolling_past_the_top_hides_the_archive_row() {
        let mut list = sidebar_organization_chat_list();
        list.scroll_offset = 1; // wheel-scrolled past Alice, to Boss
        let state = fixture_state(list);
        // Header with tabs costs 4 rows; 3 more give a rows_area smaller
        // than the list's 6 display rows.
        let rendered = render_to_string(120, 7, &state);
        assert!(rendered.contains("Boss"));
        assert!(rendered.contains("Team Rust"));
        assert!(!rendered.contains("Archived  12"));
        assert!(!rendered.contains("Alice"));
    }

    /// The part most likely to break silently per the task brief: after a
    /// wheel scroll, a click must resolve the chat actually painted at that
    /// row, not whatever chat used to be there before the window moved.
    #[test]
    fn click_after_scroll_resolves_the_row_actually_painted() {
        let mut list = seeded_chat_list(Some(1)); // selected: Alice, row 0
        list.scroll_offset = 3; // wheel-scrolled to "#rust-de", row 3
        let state = fixture_state(list);
        let (rendered, hits) = render_with_hits(120, 5, &state);
        let lines: Vec<&str> = rendered.lines().collect();
        // rows_area starts at y = 2 (label + blank), one row per visible
        // chat: #rust-de (id 4), Bob (id 5), Archived (id 6).
        assert!(lines[2].contains("#rust-de"));
        assert_eq!(hits.target_at(5, 2), Some(HitTarget::ChatRow(ChatId(4))));
        assert!(lines[3].contains("Bob"));
        assert_eq!(hits.target_at(5, 3), Some(HitTarget::ChatRow(ChatId(5))));
        assert!(lines[4].contains("Archived"));
        assert_eq!(hits.target_at(5, 4), Some(HitTarget::ChatRow(ChatId(6))));
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
