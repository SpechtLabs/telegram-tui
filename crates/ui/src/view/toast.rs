//! Toast stack: transient lower-right overlays for messages arriving in an
//! unfocused, unmuted chat (`tgt_core::state::toasts`, spec §6.4).
//!
//! Renders over whatever the rest of the frame already painted — callers
//! must draw this last. Root wiring (calling `draw` from `view/root.rs`) is
//! T45's job; this module only owns the drawing itself.
//!
//! Newest toast renders at the bottom of the stack, closest to the corner
//! the eye lands on first; older toasts stack upward above it. `esc`
//! dismisses the newest one (`state/toasts.rs::dismiss_newest`), so bottom
//! and "next to go" are always the same toast.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use tgt_core::app::AppState;
use tgt_core::state::toasts::Toast;

use crate::theme::Theme;

const TOAST_WIDTH: u16 = 32;
/// Top border + title line + body line + bottom border.
const TOAST_HEIGHT: u16 = 4;
const MARGIN: u16 = 1;

pub fn draw(state: &AppState, theme: &Theme, f: &mut Frame) {
    let area = f.area();
    let width = TOAST_WIDTH.min(area.width.saturating_sub(MARGIN * 2));
    if width == 0 {
        return;
    }

    // `.rev()`: the queue is oldest-to-newest (push_back), so the last
    // element is the newest — draw it first, at the bottom-most slot.
    for (i, toast) in state.toasts.toasts.iter().rev().enumerate() {
        let i = i as u16;
        let bottom_offset = MARGIN + (i + 1) * TOAST_HEIGHT;
        if bottom_offset > area.height {
            // Out of vertical room; older toasts simply don't fit on screen.
            break;
        }
        let rect = Rect {
            x: area.x + area.width.saturating_sub(width + MARGIN),
            y: area.y + area.height - bottom_offset,
            width,
            height: TOAST_HEIGHT,
        };
        draw_one(f, theme, rect, toast);
    }
}

fn draw_one(f: &mut Frame, theme: &Theme, rect: Rect, toast: &Toast) {
    f.render_widget(Clear, rect);
    let block = Block::new()
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.surface_raised).fg(theme.text_muted));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let lines = vec![
        Line::styled(
            truncate(&toast.title, inner.width as usize),
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            truncate(&toast.body, inner.width as usize),
            Style::new().fg(theme.text),
        ),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

/// Truncates to `max` display columns, appending an ellipsis when cut.
/// Toast text is a single line by contract, so char count stands in for
/// column width here (no wide-CJK handling, matching other short-label
/// truncation in this crate, e.g. `view/chat_list.rs`).
fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};

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
    use tgt_core::state::focus::{Focus, FocusStack};
    use tgt_core::state::media::MediaState;
    use tgt_core::state::presence::PresenceState;
    use tgt_core::state::toasts::ToastState;
    use tgt_core::td::update::{AuthPhase, ConnectionPhase};

    use super::*;

    fn fixture_state(toasts: Vec<Toast>) -> AppState {
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
            toasts: ToastState {
                toasts: VecDeque::from(toasts),
            },
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

    fn toast(chat_id: i64, title: &str, body: &str) -> Toast {
        Toast {
            chat_id: ChatId(chat_id),
            title: title.to_string(),
            body: body.to_string(),
            expires_at: Millis(5_000),
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
    fn nothing_rendered_when_no_toasts() {
        let state = fixture_state(Vec::new());
        let rendered = render_to_string(120, 40, &state);
        assert_eq!(rendered.trim(), "");
    }

    #[test]
    fn draw_two_toasts_120x40() {
        let state = fixture_state(vec![
            toast(1, "Alice", "hey, are you around?"),
            toast(2, "Team Chat", "deploy is green"),
        ]);
        let rendered = render_to_string(120, 40, &state);
        assert!(rendered.contains("Alice"));
        assert!(rendered.contains("hey, are you around?"));
        assert!(rendered.contains("Team Chat"));
        assert!(rendered.contains("deploy is green"));
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn long_title_and_body_are_truncated_to_fit() {
        let state = fixture_state(vec![toast(
            1,
            "A very long chat title that will not fit in the box",
            "an equally long message body that also needs truncation applied",
        )]);
        let rendered = render_to_string(120, 40, &state);
        assert!(rendered.contains('…'));
    }
}
