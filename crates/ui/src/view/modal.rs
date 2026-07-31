//! Modal overlay: a centered box over a dimmed frame (spec §6.3's
//! destructive-confirmation UX; architecture §4.5's `ModalKind`).
//!
//! Only two kinds exist today (`state/focus.rs::ModalKind`):
//! `ConfirmDelete` ("Delete for me" / "Delete for everyone", the second
//! option only when `can_be_deleted_for_all_users`) and `ConfirmSendFile`
//! (T39 wires the actual send). Both read their cursor from
//! `AppState::modal_ui` (`state/modal.rs::ModalState`) — never derive it
//! locally, so the highlighted row always matches what `Enter` would
//! actually confirm.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};
use tgt_core::app::AppState;
use tgt_core::state::focus::{Focus, ModalKind};
use tgt_core::state::modal::ModalState;

use crate::theme::Theme;

pub fn draw(state: &AppState, theme: &Theme, f: &mut Frame) {
    let Focus::Modal(kind) = state.focus.current().clone() else {
        return;
    };
    let area = f.area();

    // Dim the pane content behind the modal: a flat, muted canvas rather
    // than whatever the chat list / conversation last painted there.
    f.render_widget(Clear, area);
    f.render_widget(
        Block::new().style(Style::new().bg(theme.surface).fg(theme.text_muted)),
        area,
    );

    match kind {
        ModalKind::ConfirmDelete { can_revoke, .. } => {
            let modal_ui = state.modal_ui.unwrap_or_default();
            draw_confirm_delete(area, theme, f, can_revoke, modal_ui);
        }
        ModalKind::ConfirmSendFile { path } => {
            draw_confirm_send_file(area, theme, f, &path);
        }
    }
}

/// Centers a fixed-size box inside `area`, clamped so it never overflows.
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

/// An overlay panel: rounded, single-line border in `theme.border` on
/// `surface_raised`, two columns of internal padding, centered
/// (docs/design-language.md §1). The panel is told apart from the frame
/// behind it by its raised surface, so its border does not also have to
/// shout in `accent`.
fn panel(area: Rect, theme: &Theme, f: &mut Frame, width: u16, height: u16, title: &str) -> Rect {
    let outer = centered(area, width, height);
    f.render_widget(Clear, outer);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Line::from(format!(" {title} ")).centered())
        .style(Style::new().bg(theme.surface_raised))
        .border_style(Style::new().fg(theme.border))
        .padding(Padding::horizontal(PANEL_PADDING));
    let inner = block.inner(outer);
    f.render_widget(block, outer);
    inner
}

/// Columns of internal padding on each side of an overlay panel, and the
/// per-side allowance every width calculation below has to add on top of
/// the border column.
const PANEL_PADDING: u16 = 2;
/// Border + padding, both sides: what a panel costs around its content.
const PANEL_CHROME: u16 = 2 * (1 + PANEL_PADDING);

const DELETE_FOR_ME: &str = "Delete for me";
const DELETE_FOR_EVERYONE: &str = "Delete for everyone";
const DELETE_HINT: &str = "⏎ confirm · esc cancel";

fn draw_confirm_delete(
    area: Rect,
    theme: &Theme,
    f: &mut Frame,
    can_revoke: bool,
    modal_ui: ModalState,
) {
    let options: &[&str] = if can_revoke {
        &[DELETE_FOR_ME, DELETE_FOR_EVERYONE]
    } else {
        &[DELETE_FOR_ME]
    };
    let cursor = modal_ui.cursor.min(options.len() - 1);

    let width = ["Delete message?", DELETE_FOR_EVERYONE, DELETE_HINT]
        .iter()
        .map(|s| s.chars().count() as u16)
        .max()
        .unwrap_or(0)
        + PANEL_CHROME;
    // title line already drawn by panel(); body = blank + options + blank + hint.
    let height = 1 + options.len() as u16 + 2;
    let inner = panel(area, theme, f, width, height + 2, "Delete message?");

    let mut lines: Vec<Line<'static>> = vec![Line::from("")];
    for (i, opt) in options.iter().enumerate() {
        let mut style = Style::new().fg(theme.text);
        if i == cursor {
            style = style.bg(theme.selection).add_modifier(Modifier::BOLD);
        }
        lines.push(Line::styled((*opt).to_string(), style));
    }
    lines.push(Line::from(""));
    lines.push(Line::styled(DELETE_HINT, Style::new().fg(theme.text_muted)));

    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
}

fn draw_confirm_send_file(area: Rect, theme: &Theme, f: &mut Frame, path: &std::path::Path) {
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    const HINT: &str = "⏎ send · esc cancel";

    let width = ["Send file?", filename.as_str(), HINT]
        .iter()
        .map(|s| s.chars().count() as u16)
        .max()
        .unwrap_or(0)
        + PANEL_CHROME;
    let inner = panel(area, theme, f, width, 6, "Send file?");

    let lines = vec![
        Line::from(""),
        Line::styled(filename, Style::new().fg(theme.text)),
        Line::from(""),
        Line::styled(HINT, Style::new().fg(theme.text_muted)),
    ];
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tgt_core::app::{AppState, Screen};
    use tgt_core::effect::TelemetryMode;
    use tgt_core::model::ids::{ChatId, MessageId};
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

    const CHAT: ChatId = ChatId(1);
    const MSG: MessageId = MessageId(42);

    fn fixture_state(focus: Focus, modal_ui: Option<ModalState>) -> AppState {
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
            open_chat: Some(CHAT),
            composer: ComposerState::default(),
            modal_ui,
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

    fn confirm_delete(can_revoke: bool) -> Focus {
        Focus::Modal(ModalKind::ConfirmDelete {
            chat_id: CHAT,
            message_id: MSG,
            can_revoke,
        })
    }

    #[test]
    fn nothing_rendered_when_no_modal_focused() {
        let state = fixture_state(Focus::ChatList, None);
        let rendered = render_to_string(60, 12, &state);
        assert_eq!(rendered.trim(), "");
    }

    #[test]
    fn delete_modal_without_revoke_offers_only_delete_for_me() {
        let state = fixture_state(confirm_delete(false), Some(ModalState::default()));
        let rendered = render_to_string(60, 12, &state);
        assert!(rendered.contains("Delete message?"));
        assert!(rendered.contains("Delete for me"));
        assert!(!rendered.contains("Delete for everyone"));
        assert!(rendered.contains("confirm"));
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn delete_modal_with_revoke_offers_both_options() {
        let modal_ui = ModalState { cursor: 1 };
        let state = fixture_state(confirm_delete(true), Some(modal_ui));
        let rendered = render_to_string(60, 12, &state);
        assert!(rendered.contains("Delete message?"));
        assert!(rendered.contains("Delete for me"));
        assert!(rendered.contains("Delete for everyone"));
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn send_file_modal_shows_the_filename() {
        let focus = Focus::Modal(ModalKind::ConfirmSendFile {
            path: PathBuf::from("/tmp/report.pdf"),
        });
        let state = fixture_state(focus, Some(ModalState::default()));
        let rendered = render_to_string(60, 12, &state);
        assert!(rendered.contains("Send file?"));
        assert!(rendered.contains("report.pdf"));
        assert!(rendered.contains("send"));
    }
}
