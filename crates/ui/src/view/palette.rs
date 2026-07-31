//! Command palette overlay: centered, fuzzy-matched over a unified result set
//! of chats and commands (spec §11; `state::palette` module docs for the
//! ranking rule this view only ever displays, never recomputes).
//!
//! ## Match highlighting
//!
//! `PaletteItem` (`state::palette`) carries the final nucleo `score` but not
//! the matched character positions — storing per-item indices there would
//! mean paying for `Pattern::indices` (strictly more work than the `score`
//! path core already runs) on every keystroke, for every item, including the
//! ones scrolled off-screen. That cost is never worth it: a redraw only ever
//! paints the rows inside the scroll window, so this view reruns
//! `Pattern::indices` itself, once per painted row, right before turning that
//! row into spans. Bounded by viewport height rather than result-set size,
//! this stays cheap regardless of how many chats exist, and the selected row
//! — the one thing the user's eye is on — is always among the rows redone
//! this way, since the scroll window is computed to keep it visible.
//!
//! `Pattern::indices` documents its output as unsorted and possibly
//! duplicated (multi-atom queries append per-atom); [`matched_positions`]
//! sorts and dedups before use, matching the dedup recipe in nucleo's own
//! doc comment.
//!
//! ## Command labels
//!
//! `state::palette::COMMANDS` (the label text nucleo matches commands
//! against) is private to that module, so [`command_label`] holds its own
//! copy of the same five strings for display. Keep the two in sync by hand;
//! a mismatch would only affect which characters get highlighted within a
//! command row, not ranking or invocation (both keyed on `CommandId`, not
//! text).

use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32Str};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};
use tgt_core::app::AppState;
use tgt_core::state::palette::{CommandId, PaletteItem, PaletteState};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::Theme;

/// Draws the palette overlay when `state.palette.is_some()`; a no-op
/// otherwise. Wiring this into `root.rs` behind `Focus::Palette` is T48's
/// job — this module is exercised directly by its own tests until then.
pub fn draw(state: &AppState, theme: &Theme, f: &mut Frame) {
    let Some(palette) = state.palette.as_ref() else {
        return;
    };
    let area = f.area();

    let width = 60.min(area.width);
    let raw_height = (area.height as u32 * 60) / 100;
    let height = raw_height.max(5).min(area.height as u32) as u16;
    let outer = centered(area, width, height);

    // Rounded, one-line border in `border` on `surface_raised`, two columns
    // of internal padding (docs/design-language.md §1): the same panel
    // treatment `view::modal` and `view::help` use, so the three overlays
    // read as one family.
    f.render_widget(Clear, outer);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Line::from(" palette ").centered())
        .style(Style::new().bg(theme.surface_raised))
        .border_style(Style::new().fg(theme.border))
        .padding(Padding::horizontal(2));
    let inner = block.inner(outer);
    f.render_widget(block, outer);

    let rows = Layout::vertical([
        Constraint::Length(1), // query input, cursor at top
        Constraint::Length(1), // dim rule separating query from results
        Constraint::Min(0),    // results
    ])
    .split(inner);

    draw_query(rows[0], &palette.input.text, palette.input.cursor, theme, f);
    draw_separator(rows[1], theme, f);
    draw_results(rows[2], state, palette, theme, f);
}

/// Centers a fixed-size box inside `area`, clamped so it never overflows
/// (same convention as `view::modal::centered`).
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

/// `› ` prompt plus the query text with a reverse-video cursor cell
/// (`view::chat_list::draw_filter_input`'s cursor treatment, applied here to
/// the whole-buffer query rather than a chat-list filter).
fn draw_query(area: Rect, text: &str, cursor: usize, theme: &Theme, f: &mut Frame) {
    let chars: Vec<char> = text.chars().collect();
    let cursor_chars = text[..cursor.min(text.len())]
        .chars()
        .count()
        .min(chars.len());
    let base = Style::new().fg(theme.text);
    let cursor_style = Style::new().fg(theme.surface).bg(theme.accent);

    let mut spans = vec![Span::styled(
        "\u{203a} ",
        Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
    )];
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

fn draw_separator(area: Rect, theme: &Theme, f: &mut Frame) {
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "\u{2500}".repeat(area.width as usize),
            Style::new().fg(theme.border),
        ))),
        area,
    );
}

fn draw_results(
    area: Rect,
    state: &AppState,
    palette: &PaletteState,
    theme: &Theme,
    f: &mut Frame,
) {
    let height = area.height as usize;
    if height == 0 {
        return;
    }
    if palette.results.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "no matches",
                Style::new().fg(theme.text_muted),
            )))
            .alignment(Alignment::Center),
            area,
        );
        return;
    }

    let offset = scroll_offset(palette.results.len(), height, palette.selected);
    let query = palette.input.text.as_str();

    let lines: Vec<Line<'static>> = palette
        .results
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(idx, item)| {
            let selected = idx == palette.selected;
            result_row_line(item, state, selected, query, area.width, theme)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

/// Pure clamp of the viewport to the selection — same shape as
/// `view::chat_list::scroll_offset`, specialized to a `usize` selection since
/// the palette (unlike the chat list) always has a selected index once any
/// result exists.
fn scroll_offset(total: usize, height: usize, selected: usize) -> usize {
    if height == 0 || total <= height {
        return 0;
    }
    let max_offset = total - height;
    selected.saturating_sub(height - 1).min(max_offset)
}

fn result_row_line(
    item: &PaletteItem,
    state: &AppState,
    selected: bool,
    query: &str,
    width: u16,
    theme: &Theme,
) -> Line<'static> {
    let width = width as usize;
    // The same selection idiom as the chat list: an accent bar at the left
    // edge plus a wash across the row. `selection` rather than
    // `surface_raised` because the panel itself is already raised, and a row
    // painted its own background colour would be invisible on it.
    let marker: &'static str = if selected { "\u{258f} " } else { "  " };
    let row_bg = selected.then_some(theme.selection);
    let with_row_bg = |mut style: Style| {
        if let Some(bg) = row_bg {
            style = style.bg(bg);
        }
        style
    };

    let (label, tag) = match item {
        PaletteItem::Chat { id, .. } => {
            let title = state
                .chat_list
                .chats
                .get(id)
                .map(|chat| chat.title.clone())
                .unwrap_or_else(|| "(unknown chat)".to_string());
            (title, None)
        }
        PaletteItem::Command { id, .. } => (command_label(*id).to_string(), Some("cmd")),
    };

    let positions = matched_positions(&label, query);
    let tag_width = tag.map(|t| t.width() + 1).unwrap_or(0);
    let label_budget = width.saturating_sub(2 + tag_width);

    let base_style = with_row_bg(Style::new().fg(theme.text));
    let match_style = with_row_bg(Style::new().fg(theme.accent).add_modifier(Modifier::BOLD));
    let marker_style = with_row_bg(Style::new().fg(theme.accent));

    let (label_spans, label_width) =
        label_spans(&label, &positions, label_budget, base_style, match_style);

    let mut spans = vec![Span::styled(marker, marker_style)];
    spans.extend(label_spans);

    let mid_pad = width.saturating_sub(2 + label_width + tag_width);
    if mid_pad > 0 {
        spans.push(Span::styled(" ".repeat(mid_pad), with_row_bg(Style::new())));
    }
    if let Some(tag) = tag {
        spans.push(Span::styled(" ", with_row_bg(Style::new())));
        spans.push(Span::styled(
            tag,
            with_row_bg(Style::new().fg(theme.text_muted)),
        ));
    }
    Line::from(spans)
}

/// The five command labels `state::palette::COMMANDS` matches against — see
/// the module docs' "Command labels" section for why this can't just import
/// that table.
fn command_label(id: CommandId) -> &'static str {
    match id {
        CommandId::ToggleTheme => "Toggle theme",
        CommandId::TelemetrySettings => "Telemetry settings",
        CommandId::SendFile => "Send file",
        CommandId::LogOut => "Log out",
        CommandId::Quit => "Quit",
    }
}

/// Sorted, deduplicated char indices into `label` that `query` matched
/// (empty when `query` is empty — an empty pattern has no atoms, so
/// `Pattern::indices` never records a position, which is exactly "no
/// highlighting" for the empty-query recency list).
fn matched_positions(label: &str, query: &str) -> Vec<usize> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf: Vec<char> = Vec::new();
    let haystack = Utf32Str::new(label, &mut buf);
    let mut indices: Vec<u32> = Vec::new();
    if pattern
        .indices(haystack, &mut matcher, &mut indices)
        .is_none()
    {
        return Vec::new();
    }
    indices.sort_unstable();
    indices.dedup();
    indices.into_iter().map(|i| i as usize).collect()
}

/// Renders `text` as spans split at `positions` (matched chars in `matched`
/// style, everything else in `base`), truncated to `budget` display columns
/// with a trailing `…` when it doesn't fit — the same width-aware truncation
/// as `view::chat_list::truncate_to_width`, extended to preserve highlight
/// runs. Returns the spans plus their total display width.
fn label_spans(
    text: &str,
    positions: &[usize],
    budget: usize,
    base: Style,
    matched: Style,
) -> (Vec<Span<'static>>, usize) {
    if budget == 0 {
        return (Vec::new(), 0);
    }
    if text.width() <= budget {
        return highlighted_spans(text.chars(), positions, base, matched);
    }
    if budget == 1 {
        return (vec![Span::styled("\u{2026}", base)], 1);
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_matched = false;
    let mut width = 0usize;
    for (i, ch) in text.chars().enumerate() {
        let cw = ch.width().unwrap_or(0);
        if width + cw > budget - 1 {
            break;
        }
        push_char(
            &mut spans,
            &mut run,
            &mut run_matched,
            ch,
            i,
            positions,
            base,
            matched,
        );
        width += cw;
    }
    flush_run(&mut spans, &mut run, run_matched, base, matched);
    spans.push(Span::styled("\u{2026}", base));
    (spans, width + 1)
}

fn highlighted_spans(
    chars: std::str::Chars<'_>,
    positions: &[usize],
    base: Style,
    matched: Style,
) -> (Vec<Span<'static>>, usize) {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_matched = false;
    let mut width = 0usize;
    for (i, ch) in chars.enumerate() {
        push_char(
            &mut spans,
            &mut run,
            &mut run_matched,
            ch,
            i,
            positions,
            base,
            matched,
        );
        width += ch.width().unwrap_or(0);
    }
    flush_run(&mut spans, &mut run, run_matched, base, matched);
    (spans, width)
}

#[allow(clippy::too_many_arguments)]
fn push_char(
    spans: &mut Vec<Span<'static>>,
    run: &mut String,
    run_matched: &mut bool,
    ch: char,
    i: usize,
    positions: &[usize],
    base: Style,
    matched: Style,
) {
    let is_match = positions.binary_search(&i).is_ok();
    if !run.is_empty() && is_match != *run_matched {
        spans.push(Span::styled(
            std::mem::take(run),
            if *run_matched { matched } else { base },
        ));
    }
    *run_matched = is_match;
    run.push(ch);
}

fn flush_run(
    spans: &mut Vec<Span<'static>>,
    run: &mut String,
    run_matched: bool,
    base: Style,
    matched: Style,
) {
    if !run.is_empty() {
        spans.push(Span::styled(
            std::mem::take(run),
            if run_matched { matched } else { base },
        ));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tgt_core::app::{AppState, Screen};
    use tgt_core::effect::TelemetryMode;
    use tgt_core::model::chat::{ChatKind, ChatListId, ChatOrderKey, ChatPositionEntry, ChatView};
    use tgt_core::model::ids::ChatId;
    use tgt_core::model::key::{Key, KeyBindings};
    use tgt_core::model::time::Millis;
    use tgt_core::state::auth::{AuthField, AuthState, InputField};
    use tgt_core::state::chat_list::ChatListState;
    use tgt_core::state::composer::ComposerState;
    use tgt_core::state::consent::{ConsentChoice, ConsentState};
    use tgt_core::state::focus::{Focus, FocusStack};
    use tgt_core::state::media::MediaState;
    use tgt_core::state::palette;
    use tgt_core::state::presence::PresenceState;
    use tgt_core::state::toasts::ToastState;
    use tgt_core::td::update::{AuthPhase, ConnectionPhase};

    use super::*;

    /// Mirrors `App::new`'s construction (same pattern as
    /// `state::palette`'s and `view::modal`'s tests).
    fn fixture_state() -> AppState {
        AppState {
            screen: Screen::Main,
            focus: FocusStack::new(Focus::Palette),
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
            width: 100,
            height: 30,
            layout_breakpoint_cols: 100,
            theme_name: "dark".to_string(),
            theme_generation: 0,
            bindings: KeyBindings::default(),
            telemetry_mode: TelemetryMode::Off,
            crash_reports_available: false,
            telemetry_salt: [0u8; 32],
            now: Millis(0),
        }
    }

    fn insert_chat(app: &mut AppState, id: i64, title: &str, order: i64) {
        let chat = ChatView {
            id: ChatId(id),
            kind: ChatKind::Private,
            title: title.to_string(),
            positions: vec![ChatPositionEntry {
                list: ChatListId::Main,
                order,
                is_pinned: false,
            }],
            unread_count: 0,
            unread_mention_count: 0,
            last_message: None,
            is_muted: false,
        };
        app.chat_list.chats.insert(ChatId(id), chat);
        app.chat_list
            .orders
            .entry(ChatListId::Main)
            .or_default()
            .insert(ChatOrderKey {
                order,
                chat_id: ChatId(id),
            });
    }

    fn type_str(app: &mut AppState, s: &str) {
        for c in s.chars() {
            palette::handle_key(app, Key::Char(c));
        }
    }

    fn render_to_string(width: u16, height: u16, state: &AppState) -> String {
        let theme = Theme::default_dark();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(state, &theme, f)).unwrap();
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
    fn nothing_rendered_when_palette_closed() {
        let state = fixture_state();
        let rendered = render_to_string(100, 30, &state);
        assert_eq!(rendered.trim(), "");
    }

    #[test]
    fn results_list_with_highlighted_match_spans_100x30() {
        let mut app = fixture_state();
        insert_chat(&mut app, 1, "Alice Smith", 30);
        insert_chat(&mut app, 2, "Alicia Keys", 20);
        insert_chat(&mut app, 3, "Bob Jones", 10);

        palette::open(&mut app);
        type_str(&mut app, "ali");

        let rendered = render_to_string(100, 30, &app);
        assert!(rendered.contains("palette"));
        assert!(rendered.contains("Alice Smith"));
        assert!(rendered.contains("Alicia Keys"));
        assert!(!rendered.contains("Bob Jones"));
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn empty_query_lists_recency_then_commands_100x30() {
        let mut app = fixture_state();
        insert_chat(&mut app, 1, "Alice", 10);
        insert_chat(&mut app, 2, "Bob", 20);

        palette::open(&mut app);

        let rendered = render_to_string(100, 30, &app);
        assert!(rendered.contains("Bob"));
        assert!(rendered.contains("Alice"));
        assert!(rendered.contains("Quit"));
        assert!(rendered.contains("cmd"));
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn no_results_shows_dim_message_100x30() {
        let mut app = fixture_state();
        insert_chat(&mut app, 1, "Alice", 10);

        palette::open(&mut app);
        type_str(&mut app, "zzzqqqxx");

        let rendered = render_to_string(100, 30, &app);
        assert!(rendered.contains("no matches"));
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn scroll_offset_keeps_selection_visible_without_over_scrolling() {
        assert_eq!(scroll_offset(20, 5, 2), 0);
        assert_eq!(scroll_offset(20, 5, 10), 6);
        assert_eq!(scroll_offset(20, 5, 19), 15);
        assert_eq!(scroll_offset(3, 5, 2), 0);
    }

    #[test]
    fn matched_positions_empty_query_yields_no_highlights() {
        assert_eq!(matched_positions("Alice", ""), Vec::<usize>::new());
    }

    #[test]
    fn matched_positions_sorted_and_deduped() {
        let positions = matched_positions("Alice Smith", "ali");
        let mut sorted = positions.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(positions, sorted);
        assert!(!positions.is_empty());
    }
}
