//! Help overlay: `?` (spec §6.2, global row) opens a full keymap reference —
//! every binding in the interaction model, grouped by the context that owns
//! it, so a user never has to guess which pane a key belongs to.
//!
//! ## Content source
//!
//! The seven groups below mirror spec §6.2's table (Global / Panes / Chat
//! list / Composer / Selection mode / Modal) plus Search, which the spec
//! folds into the chat-list row (`/` filter) but which grew its own context
//! once T42 landed in-chat search (`/` while a message is focused, `n`/`N`
//! stepping). The extra bindings later tasks added on top of §6.2's original
//! table are folded into their owning group rather than listed separately:
//! `a` archive toggle and `[`/`]` folder cycling (T43, chat list),
//! `/send <path>` (T39, composer), and the chip letter shortcuts
//! r/f/e/c/d/x/l/o/s (T26, selection mode — `model::chips::Chip::shortcut`).
//!
//! ## `Focus::Help` gate
//!
//! [`draw`] follows `view::modal`'s convention: it reads `state.focus`
//! itself and no-ops unless `Focus::Help` is on top, rather than trusting the
//! caller to gate the call. That makes this module self-contained and
//! directly testable with a bare `AppState` fixture, which matters here more
//! than usual — as of this writing `crates/core/src/app.rs::route_pane_key`
//! has no arm for `Focus::Help` (its `_ => None` catch-all still carries a
//! stale "Help arrives in T47" comment), and `?` is not bound to anything in
//! `route_key` either. Wiring that routing is out of this module's scope
//! (`app.rs` belongs to a different task); this file renders correctly the
//! moment focus reaches `Help` by whatever means, and its tests drive that
//! state directly rather than through key routing.
//!
//! ## No scroll state, so no scrolling — truncation instead
//!
//! `AppState` has no field for a help-scroll offset (there is nowhere to
//! persist one, and adding a state field is outside this file's ownership).
//! So the overlay never scrolls: it sizes itself to the frame, and if the
//! full keymap does not fit the available height it prints as many rows as
//! fit and replaces the last visible row with a single dim `…` line. At the
//! 120×40 fixture this repo tests against, the full table fits with room to
//! spare; at 80×24 it does not, and the `…` line is exactly what the
//! truncation path is there to exercise.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Paragraph};
use tgt_core::app::AppState;
use tgt_core::state::focus::Focus;

use crate::theme::Theme;

/// One `key → description` row within a [`Group`].
struct Row {
    key: &'static str,
    desc: &'static str,
}

/// A named section of the keymap, rendered as a header line followed by its
/// rows.
struct Group {
    name: &'static str,
    rows: &'static [Row],
}

/// Column the description text starts at, past the longest key label
/// (`"tab / shift+tab"`, `"r f e c d x l o s"`) plus a two-space gutter.
const KEY_COL: usize = 19;

const GROUPS: &[Group] = &[
    Group {
        name: "Global",
        rows: &[
            Row {
                key: "ctrl+p",
                desc: "open command palette",
            },
            Row {
                key: "?",
                desc: "show this help",
            },
            Row {
                key: "ctrl+c",
                desc: "quit",
            },
        ],
    },
    Group {
        name: "Panes",
        rows: &[
            Row {
                key: "← / →",
                desc: "move focus between panes",
            },
            Row {
                key: "tab / shift+tab",
                desc: "cycle panes",
            },
        ],
    },
    Group {
        name: "Chat list",
        rows: &[
            Row {
                key: "↑ / ↓",
                desc: "move selection",
            },
            Row {
                key: "⏎",
                desc: "open chat",
            },
            Row {
                key: "/",
                desc: "filter chats",
            },
            Row {
                key: "a",
                desc: "toggle archive",
            },
            Row {
                key: "[ / ]",
                desc: "cycle folders",
            },
        ],
    },
    Group {
        name: "Composer",
        rows: &[
            Row {
                key: "type",
                desc: "write a message",
            },
            Row {
                key: "⏎",
                desc: "send",
            },
            Row {
                key: "alt+⏎",
                desc: "insert newline",
            },
            Row {
                key: "↑ (empty input)",
                desc: "enter selection mode",
            },
            Row {
                key: "/send <path>",
                desc: "send a file",
            },
        ],
    },
    Group {
        name: "Selection mode",
        rows: &[
            Row {
                key: "↑ / ↓",
                desc: "move highlighted message",
            },
            Row {
                key: "← / →",
                desc: "move focused action chip",
            },
            Row {
                key: "⏎",
                desc: "invoke focused chip",
            },
            Row {
                key: "r f e c d x l o s",
                desc: "chip shortcuts: reply forward react copy edit delete download open resend",
            },
            Row {
                key: "esc",
                desc: "back to composer",
            },
        ],
    },
    Group {
        name: "Modal",
        rows: &[
            Row {
                key: "esc",
                desc: "dismiss",
            },
            Row {
                key: "⏎",
                desc: "confirm",
            },
        ],
    },
    Group {
        name: "Search",
        rows: &[
            Row {
                key: "/",
                desc: "open message search (from selection mode)",
            },
            Row {
                key: "type",
                desc: "search query",
            },
            Row {
                key: "⏎",
                desc: "run search",
            },
            Row {
                key: "n / N",
                desc: "next / previous hit",
            },
            Row {
                key: "esc",
                desc: "close search",
            },
        ],
    },
];

const TITLE: &str = "help";
const FOOTER: &str = "esc close";
const TRUNCATION_MARK: &str = "…";

/// Draws the help overlay when `Focus::Help` is on top of the stack; a no-op
/// otherwise (see module docs for why this file — not the caller — makes
/// that call).
pub fn draw(state: &AppState, theme: &Theme, f: &mut Frame) {
    if !matches!(state.focus.current(), Focus::Help) {
        return;
    }
    let area = f.area();

    // Dim the frame behind the overlay, same convention as `view::modal`.
    f.render_widget(Clear, area);
    f.render_widget(
        Block::new().style(Style::new().bg(theme.surface).fg(theme.text_muted)),
        area,
    );

    let content = build_content(theme);
    let max_content_width = content
        .iter()
        .map(line_width)
        .max()
        .unwrap_or(0)
        .max(TITLE.len())
        .max(FOOTER.len());
    let width = ((max_content_width as u16) + 4).min(area.width);

    // Overhead outside the content rows: 2 border lines, 1 blank separator,
    // 1 footer line.
    const CHROME: u16 = 4;
    let desired_height = content.len() as u16 + CHROME;
    let height = desired_height.min(area.height);
    let outer = centered(area, width, height);

    f.render_widget(Clear, outer);
    let block = Block::bordered()
        .title(Line::from(format!(" {TITLE} ")).centered())
        .style(Style::new().bg(theme.surface))
        .border_style(Style::new().fg(theme.accent));
    let inner = block.inner(outer);
    f.render_widget(block, outer);

    let content_capacity = inner.height.saturating_sub(2) as usize; // minus blank + footer
    let visible = truncate(content, content_capacity, theme);

    let rows = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Min(0),
        ratatui::layout::Constraint::Length(1), // blank separator
        ratatui::layout::Constraint::Length(1), // footer
    ])
    .split(inner);

    f.render_widget(Paragraph::new(visible), rows[0]);
    f.render_widget(
        Paragraph::new(FOOTER).style(Style::new().fg(theme.text_muted)),
        rows[2],
    );
}

/// Centers a fixed-size box inside `area`, clamped so it never overflows
/// (same convention as `view::modal::centered` / `view::palette::centered`).
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

/// Builds every group header + row as one flat list of styled lines, in
/// display order. No blank lines between groups — the header's own accent
/// and bold styling is the separator, which keeps the full table within a
/// 120×40 frame without truncation.
fn build_content(theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for group in GROUPS {
        lines.push(Line::styled(
            group.name,
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));
        for row in group.rows {
            lines.push(format_row(row, theme));
        }
    }
    lines
}

fn format_row(row: &Row, theme: &Theme) -> Line<'static> {
    let pad = KEY_COL.saturating_sub(row.key.chars().count()).max(1);
    let text = format!("  {}{}{}", row.key, " ".repeat(pad), row.desc);
    // A single styled span, not a key/description split at the column
    // boundary: the key column already reads apart from the description by
    // position alone, and this content is static enough that the extra
    // byte-vs-char-boundary bookkeeping a two-span version would need isn't
    // worth paying for.
    Line::styled(text, Style::new().fg(theme.text))
}

fn line_width(line: &Line<'_>) -> usize {
    line.spans.iter().map(|s| s.content.chars().count()).sum()
}

/// Truncates `lines` to `capacity` rows, replacing the last visible row with
/// a dim `…` marker when the full table does not fit. `capacity == 0` (a
/// frame too small even for one row) renders nothing rather than panicking.
fn truncate(lines: Vec<Line<'static>>, capacity: usize, theme: &Theme) -> Vec<Line<'static>> {
    if lines.len() <= capacity {
        return lines;
    }
    if capacity == 0 {
        return Vec::new();
    }
    let mut visible: Vec<Line<'static>> = lines.into_iter().take(capacity - 1).collect();
    visible.push(Line::styled(
        TRUNCATION_MARK,
        Style::new().fg(theme.text_muted),
    ));
    visible
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
    use tgt_core::state::focus::FocusStack;
    use tgt_core::state::media::MediaState;
    use tgt_core::state::presence::PresenceState;
    use tgt_core::state::toasts::ToastState;
    use tgt_core::td::update::{AuthPhase, ConnectionPhase};

    use super::*;

    fn fixture_state(focus: Focus) -> AppState {
        AppState {
            screen: Screen::Main,
            focus: FocusStack::new(focus),
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
            now: Millis(0),
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
    fn nothing_rendered_when_help_is_not_focused() {
        let state = fixture_state(Focus::ChatList);
        let rendered = render_to_string(120, 40, &state);
        assert_eq!(rendered.trim(), "");
    }

    #[test]
    fn every_context_group_and_the_footer_are_present() {
        let state = fixture_state(Focus::Help);
        let rendered = render_to_string(120, 40, &state);
        assert!(rendered.contains("help"));
        for group in GROUPS {
            assert!(
                rendered.contains(group.name),
                "missing group header {:?}",
                group.name
            );
        }
        assert!(rendered.contains("esc close"));
    }

    #[test]
    fn extra_keys_from_later_tasks_are_documented() {
        let state = fixture_state(Focus::Help);
        let rendered = render_to_string(120, 40, &state);
        // T43: archive toggle + folder cycling.
        assert!(rendered.contains("toggle archive"));
        assert!(rendered.contains("cycle folders"));
        // T39: send-file command.
        assert!(rendered.contains("/send <path>"));
        // T42: search hit stepping.
        assert!(rendered.contains("n / N"));
        // T26: chip letter shortcuts.
        assert!(rendered.contains("r f e c d x l o s"));
    }

    #[test]
    fn full_table_fits_120x40_without_truncation() {
        let state = fixture_state(Focus::Help);
        let rendered = render_to_string(120, 40, &state);
        assert!(!rendered.contains(TRUNCATION_MARK));
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn small_frame_truncates_with_an_ellipsis_marker() {
        let state = fixture_state(Focus::Help);
        let rendered = render_to_string(80, 24, &state);
        assert!(rendered.contains(TRUNCATION_MARK));
        insta::assert_snapshot!(rendered);
    }
}
