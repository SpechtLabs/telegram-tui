//! First-run telemetry consent screen (spec §13.5, architecture §4.6). The
//! whole frame belongs to it, the same way the auth wizard owns its screen
//! (`view/auth.rs`'s module docs) — there is nothing behind it yet, since it
//! is shown before login and before any data is sent.
//!
//! The copy here is the plain-language disclosure spec §13.5 asks for. It is
//! kept in lockstep with `tgt_core::telemetry::schema::ALLOWED_KEYS` by hand
//! rather than generated from it: the schema lists wire field names
//! (`term.width_bucket`, `chat.hash`, …), and a screen full of those would
//! fail the "plain language" bar on its own. Anyone adding a key to the
//! allowlist should re-read this text.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use tgt_core::app::AppState;
use tgt_core::state::consent::ConsentChoice;

use crate::theme::Theme;

const PANEL_WIDTH: u16 = 72;
const PANEL_HEIGHT: u16 = 24;

pub fn draw(state: &AppState, theme: &Theme, f: &mut Frame) {
    let area = f.area();
    f.render_widget(Block::new().style(Style::new().bg(theme.surface)), area);

    let outer = centered(area, PANEL_WIDTH, PANEL_HEIGHT);
    let block = Block::bordered()
        .title(Line::from(" Before we start ").centered())
        .border_style(Style::new().fg(theme.accent));
    let inner = block.inner(outer);
    f.render_widget(block, outer);

    let [
        intro_area,
        _gap1,
        collected_area,
        _gap2,
        not_collected_area,
        _gap3,
        destination_area,
        _gap4,
        controls_area,
        _gap5,
        options_area,
        _gap6,
        hint_area,
    ] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(inner);

    f.render_widget(
        Paragraph::new("telegram-tui can send anonymous usage telemetry to help find bugs and see which features get used.")
            .wrap(Wrap { trim: true })
            .style(Style::new().fg(theme.text)),
        intro_area,
    );

    f.render_widget(
        labeled_paragraph(
            "Collected: ",
            "app/OS version, terminal type, action names (e.g. \"sent a message\"), \
             outcomes, error kinds, durations, and a random per-install id.",
            theme.success,
            theme.text_muted,
        ),
        collected_area,
    );

    f.render_widget(
        labeled_paragraph(
            "Never collected: ",
            "message text, contact names, phone numbers, chat titles, or file names.",
            theme.danger,
            theme.text_muted,
        ),
        not_collected_area,
    );

    f.render_widget(
        Paragraph::new("Sent to: the telegram-tui project's telemetry endpoint.")
            .wrap(Wrap { trim: true })
            .style(Style::new().fg(theme.text_muted)),
        destination_area,
    );

    f.render_widget(
        Paragraph::new(
            "Change your mind later: `--no-telemetry`, `TELEGRAM_TUI_TELEMETRY=off`, \
             `DO_NOT_TRACK=1`, or edit config.toml.",
        )
        .wrap(Wrap { trim: true })
        .style(Style::new().fg(theme.text_muted)),
        controls_area,
    );

    draw_options(options_area, state.consent.selected, theme, f);

    f.render_widget(
        Paragraph::new("↑↓←→/tab choose · ⏎ confirm").style(Style::new().fg(theme.text_muted)),
        hint_area,
    );
}

fn labeled_paragraph<'a>(
    label: &'a str,
    rest: &'a str,
    label_color: ratatui::style::Color,
    rest_color: ratatui::style::Color,
) -> Paragraph<'a> {
    Paragraph::new(Line::from(vec![
        Span::styled(
            label,
            Style::new().fg(label_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(rest, Style::new().fg(rest_color)),
    ]))
    .wrap(Wrap { trim: true })
}

fn draw_options(area: Rect, selected: ConsentChoice, theme: &Theme, f: &mut Frame) {
    let [enable_area, disable_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area);

    f.render_widget(
        option_paragraph("Enable", selected == ConsentChoice::Enable, theme),
        enable_area,
    );
    f.render_widget(
        option_paragraph("Disable", selected == ConsentChoice::Disable, theme),
        disable_area,
    );
}

fn option_paragraph(label: &str, active: bool, theme: &Theme) -> Paragraph<'static> {
    let marker = if active { "▶ " } else { "  " };
    let style = if active {
        Style::new()
            .fg(theme.surface)
            .bg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(theme.text)
    };
    Paragraph::new(format!("{marker}{label}"))
        .style(style)
        .alignment(Alignment::Center)
}

/// Centers a fixed-size box inside `area`, clamped so it never overflows.
/// Mirrors `view/auth.rs`'s helper of the same name (private to that module,
/// so duplicated rather than shared — see that file's module docs on the
/// whole-frame screen convention).
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
    use tgt_core::state::consent::ConsentState;
    use tgt_core::state::focus::{Focus, FocusStack};
    use tgt_core::state::media::MediaState;
    use tgt_core::state::presence::PresenceState;
    use tgt_core::state::toasts::ToastState;
    use tgt_core::td::update::{AuthPhase, ConnectionPhase};

    use super::*;

    fn fixture_state(selected: ConsentChoice) -> AppState {
        AppState {
            screen: Screen::Consent,
            focus: FocusStack::new(Focus::ChatList),
            connection: ConnectionPhase::WaitingForNetwork,
            consent: ConsentState {
                selected,
                acknowledged: false,
            },
            auth: AuthState {
                phase: AuthPhase::WaitTdlibParameters,
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
    fn consent_screen_100x30() {
        let state = fixture_state(ConsentChoice::Enable);
        insta::assert_snapshot!(render_to_string(100, 30, &state));
    }

    #[test]
    fn discloses_what_is_and_is_not_collected() {
        let state = fixture_state(ConsentChoice::Enable);
        let rendered = render_to_string(120, 34, &state);
        assert!(rendered.contains("Collected"), "buffer:\n{rendered}");
        assert!(rendered.contains("Never collected"), "buffer:\n{rendered}");
        assert!(rendered.contains("Enable"), "buffer:\n{rendered}");
        assert!(rendered.contains("Disable"), "buffer:\n{rendered}");
        // Every message-shaped thing the spec forbids must never appear as
        // if it were collected — spot-check the copy names them as excluded.
        assert!(rendered.contains("message text"), "buffer:\n{rendered}");
        assert!(rendered.contains("phone numbers"), "buffer:\n{rendered}");
    }

    #[test]
    fn disable_selected_highlights_disable() {
        let state = fixture_state(ConsentChoice::Disable);
        let rendered = render_to_string(120, 34, &state);
        assert!(rendered.contains("▶ Disable"), "buffer:\n{rendered}");
    }
}
