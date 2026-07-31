//! Selection-mode action chip row (spec §6.3 mock:
//! `‹ [R Reply] [F Forward] [E React] [C Copy] [D Delete] ›`).
//!
//! Each chip renders as `[<SHORTCUT> <Label>]`. The row has two states and
//! only two: the focused chip (`SelectionState::chip_cursor`) sits on
//! `surface_raised` with its shortcut letter in `accent` bold and its label
//! in `text`; every other chip is uniformly `text_muted` with its letter in
//! `accent_dim`, so the row reads as one dim strip with a single lit cell
//! rather than five competing buttons (docs/design-language.md §5 — focus is
//! a background and an accent, never inverse video).
//!
//! `core` (`state/selection.rs`) tracks `chip_scroll` as the first visible
//! chip index but has no way to measure the terminal, so it assumes a fixed
//! window (`CHIP_VISIBLE_MAX`) purely for its own cursor-follow bookkeeping.
//! This view is the thing that actually knows the available width: it starts
//! at `chip_scroll` (core's call on *where* the window begins) and then
//! greedily fits as many chips as the real `area.width` allows, showing `‹`
//! when chips are scrolled off the left and `›` when chips remain off the
//! right — exactly spec §6.3's "chips exceeding the available width scroll
//! horizontally with `‹` `›` affordances".

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use tgt_core::app::AppState;
use tgt_core::model::chips::Chip;

use crate::theme::Theme;

/// Columns a single scroll arrow ("‹ " or " ›") costs.
const ARROW_WIDTH: usize = 2;

pub fn draw(area: Rect, state: &AppState, theme: &Theme, f: &mut Frame) {
    let Some(chat_id) = state.open_chat else {
        return;
    };
    let Some(sel) = state
        .conversations
        .get(&chat_id)
        .and_then(|convo| convo.selection.as_ref())
    else {
        return;
    };
    if sel.chips.is_empty() {
        return;
    }

    let start = sel.chip_scroll.min(sel.chips.len() - 1);
    let remaining = &sel.chips[start..];
    let left_arrow = start > 0;
    let width = area.width as usize;

    let (visible_count, right_arrow) = fit_window(remaining, left_arrow, width);

    let mut spans: Vec<Span<'static>> = Vec::new();
    if left_arrow {
        spans.push(Span::styled("‹ ", Style::new().fg(theme.text_muted)));
    }
    for (i, chip) in remaining.iter().take(visible_count).enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        let idx = start + i;
        spans.extend(chip_spans(*chip, idx == sel.chip_cursor, theme));
    }
    if right_arrow {
        spans.push(Span::styled(" ›", Style::new().fg(theme.text_muted)));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// How many chips (from the front of `remaining`) fit in `width` columns,
/// and whether a right-scroll affordance is needed for the rest. Tries the
/// whole remaining row first (reserving space for the left arrow only, since
/// that one is already decided); falls back to reserving both arrows and
/// greedily fitting chips into what is left, showing at least one chip even
/// if it does not quite fit rather than rendering an empty row.
fn fit_window(remaining: &[Chip], left_arrow: bool, width: usize) -> (usize, bool) {
    let left_cost = if left_arrow { ARROW_WIDTH } else { 0 };
    if row_width(remaining) + left_cost <= width {
        return (remaining.len(), false);
    }

    let budget = width.saturating_sub(left_cost + ARROW_WIDTH);
    let mut used = 0usize;
    let mut count = 0usize;
    for (i, chip) in remaining.iter().enumerate() {
        let w = chip_width(*chip) + if i > 0 { 1 } else { 0 };
        if count > 0 && used + w > budget {
            break;
        }
        used += w;
        count += 1;
    }
    (count.max(1), true)
}

fn row_width(chips: &[Chip]) -> usize {
    if chips.is_empty() {
        return 0;
    }
    chips.iter().map(|c| chip_width(*c)).sum::<usize>() + (chips.len() - 1)
}

/// `[` + shortcut + ` ` + label + `]`.
fn chip_width(chip: Chip) -> usize {
    chip.label().len() + 4
}

fn chip_spans(chip: Chip, focused: bool, theme: &Theme) -> Vec<Span<'static>> {
    let bg = focused.then_some(theme.surface_raised);
    let plain = with_bg(
        Style::new().fg(if focused {
            theme.text
        } else {
            theme.text_muted
        }),
        bg,
    );
    let letter = with_bg(
        Style::new()
            .fg(if focused {
                theme.accent
            } else {
                theme.accent_dim
            })
            .add_modifier(Modifier::BOLD),
        bg,
    );
    // The brackets belong to the chrome of the chip, not to its label, so
    // they stay dim even on the focused one.
    let bracket = with_bg(Style::new().fg(theme.text_muted), bg);
    vec![
        Span::styled("[", bracket),
        Span::styled(chip.shortcut().to_ascii_uppercase().to_string(), letter),
        Span::styled(format!(" {}", chip.label()), plain),
        Span::styled("]", bracket),
    ]
}

fn with_bg(style: Style, bg: Option<Color>) -> Style {
    match bg {
        Some(c) => style.bg(c),
        None => style,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tgt_core::app::{AppState, Screen};
    use tgt_core::effect::TelemetryMode;
    use tgt_core::model::ids::ChatId;
    use tgt_core::model::key::KeyBindings;
    use tgt_core::model::time::Millis;
    use tgt_core::state::auth::{AuthField, AuthState, InputField};
    use tgt_core::state::chat_list::ChatListState;
    use tgt_core::state::composer::ComposerState;
    use tgt_core::state::consent::{ConsentChoice, ConsentState};
    use tgt_core::state::conversation::{self, ConversationState};
    use tgt_core::state::focus::{Focus, FocusStack};
    use tgt_core::state::media::MediaState;
    use tgt_core::state::presence::PresenceState;
    use tgt_core::state::selection::SelectionState;
    use tgt_core::state::toasts::ToastState;
    use tgt_core::td::update::{AuthPhase, ConnectionPhase};

    use super::*;

    const CHAT: ChatId = ChatId(1);

    fn fixture_state(chips: Vec<Chip>, chip_cursor: usize, chip_scroll: usize) -> AppState {
        let mut app = AppState {
            screen: Screen::Main,
            focus: FocusStack::new(Focus::Selection),
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
            crash_reports_available: false,
            telemetry_salt: [0u8; 32],
            now: Millis(0),
        };
        conversation::open(&mut app, CHAT);
        let convo: &mut ConversationState = app.conversations.get_mut(&CHAT).unwrap();
        convo.selection = Some(SelectionState {
            message_id: tgt_core::model::ids::MessageId(1),
            chips,
            chip_cursor,
            chip_scroll,
        });
        app
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

    fn spec_chips() -> Vec<Chip> {
        vec![
            Chip::Reply,
            Chip::Forward,
            Chip::React,
            Chip::Copy,
            Chip::Delete,
        ]
    }

    #[test]
    fn nothing_rendered_without_an_open_selection() {
        let state = fixture_state(Vec::new(), 0, 0);
        let rendered = render_to_string(120, 1, &state);
        assert_eq!(rendered.trim(), "");
    }

    #[test]
    fn fitting_row_shows_all_chips_without_arrows_120_wide() {
        let state = fixture_state(spec_chips(), 2, 0);
        let rendered = render_to_string(120, 1, &state);
        assert!(rendered.contains("[R Reply]"));
        assert!(rendered.contains("[F Forward]"));
        assert!(rendered.contains("[E React]"));
        assert!(rendered.contains("[C Copy]"));
        assert!(rendered.contains("[X Delete]"));
        assert!(!rendered.contains('‹'));
        assert!(!rendered.contains('›'));
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn overflowing_row_shows_scroll_affordances_40_wide() {
        let chips = vec![
            Chip::Reply,
            Chip::Forward,
            Chip::React,
            Chip::Copy,
            Chip::Edit,
            Chip::Download,
            Chip::Delete,
        ];
        // Scrolled: chip_scroll > 0 means content sits off the left edge,
        // and the row is far too dense to fit the tail in 40 columns, so a
        // right affordance is also expected.
        let state = fixture_state(chips, 3, 2);
        let rendered = render_to_string(40, 1, &state);
        assert!(
            rendered.starts_with('‹'),
            "expected a left arrow: {rendered:?}"
        );
        assert!(
            rendered.contains('›'),
            "expected a right arrow: {rendered:?}"
        );
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn focused_chip_is_the_one_at_chip_cursor_not_just_the_window_start() {
        // Regression guard for an off-by-one between the visible slice and
        // the absolute chip_cursor index: cursor sits on the 3rd chip
        // (index 2, "React") while chip_scroll is 0, so the window start and
        // the focused index differ.
        let state = fixture_state(spec_chips(), 2, 0);
        let rendered = render_to_string(120, 1, &state);
        assert!(rendered.contains("[E React]"));
    }
}
