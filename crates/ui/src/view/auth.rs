//! Auth wizard screens (spec §9, architecture §4.6). The whole frame belongs
//! to the wizard: `draw` is the screen, not a pane inside the two-pane shell.
//!
//! Dispatch mirrors T11's `state::auth::route_auth_key` contract exactly, so
//! the rendered screen always matches which keys are actually live:
//! - `active_field ∈ {ApiId, ApiHash}` means the credentials wizard owns the
//!   screen, regardless of `phase` (T11's module docs on
//!   `crates/core/src/state/auth.rs`).
//! - `phase == WaitPhoneNumber && method != Some(Phone)` is the method-choice
//!   screen; it covers both "nothing picked yet" (`method: None`) and
//!   "QR armed but not confirmed" (`method: Some(Qr)`) — Up/Down/'q' can
//!   still flip the choice back to Phone until Enter fires the request.
//! - `phase == WaitPhoneNumber && method == Some(Phone)` means picking Phone
//!   already confirmed it: the screen goes straight to the phone field.
//!
//! `AuthPhase::Unsupported` renders its name rather than being swallowed
//! (spec §9.2's dead-end-screen requirement).

use qrcode::{EcLevel, QrCode};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use tgt_core::app::AppState;
use tgt_core::state::auth::{AuthField, AuthState, InputField, LoginMethod};
use tgt_core::td::update::AuthPhase;

use crate::theme::Theme;

/// Quiet zone around the QR matrix, in modules. The spec calls for a QR that
/// fits small terminals; a 2-module border (half the usual 4) keeps the
/// rendered size down while still being scannable.
const QR_QUIET_ZONE: usize = 2;

pub fn draw(state: &AppState, theme: &Theme, f: &mut Frame) {
    let area = f.area();
    f.render_widget(Block::new().style(Style::new().bg(theme.surface)), area);

    let auth = &state.auth;

    if matches!(auth.active_field, AuthField::ApiId | AuthField::ApiHash) {
        draw_credentials(area, auth, theme, f);
        return;
    }

    match &auth.phase {
        AuthPhase::WaitTdlibParameters => draw_status(area, theme, f, "Starting Telegram client…"),
        AuthPhase::WaitPhoneNumber if auth.method != Some(LoginMethod::Phone) => {
            draw_method_choice(area, auth, theme, f);
        }
        AuthPhase::WaitPhoneNumber => draw_phone(area, state, theme, f),
        AuthPhase::WaitCode {
            delivery_hint,
            length,
        } => draw_code(area, state, theme, f, delivery_hint, *length),
        AuthPhase::WaitPassword { hint } => draw_password(area, state, theme, f, hint.as_deref()),
        AuthPhase::WaitOtherDeviceConfirmation { link } => draw_qr(area, theme, f, link),
        AuthPhase::Ready => draw_status(area, theme, f, "Signed in"),
        AuthPhase::LoggingOut => draw_status(area, theme, f, "Logging out…"),
        AuthPhase::Closing => draw_status(area, theme, f, "Closing…"),
        AuthPhase::Closed => draw_status(area, theme, f, "Closed"),
        AuthPhase::Unsupported { name } => {
            draw_status(area, theme, f, &format!("Unsupported auth state: {name}"));
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

fn panel(area: Rect, theme: &Theme, f: &mut Frame, width: u16, height: u16, title: &str) -> Rect {
    let outer = centered(area, width, height);
    let block = Block::bordered()
        .title(Line::from(format!(" {title} ")).centered())
        .border_style(Style::new().fg(theme.accent));
    let inner = block.inner(outer);
    f.render_widget(block, outer);
    inner
}

fn draw_status(area: Rect, theme: &Theme, f: &mut Frame, text: &str) {
    let width = (text.chars().count() as u16 + 4).min(area.width);
    let inner = panel(area, theme, f, width, 3, "telegram-tui");
    f.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .style(Style::new().fg(theme.text)),
        inner,
    );
}

/// Renders one text/cursor line: the active field shows a reverse-video
/// cursor cell, inactive fields render plainly. `display` and the cursor
/// index are both counted in chars so masked (bullet) fields line up with
/// the original `field.cursor` byte offset.
fn field_line(display: &str, cursor_chars: usize, active: bool, theme: &Theme) -> Line<'static> {
    if !active {
        return Line::from(Span::styled(
            display.to_string(),
            Style::new().fg(theme.text),
        ));
    }
    let base = Style::new().fg(theme.text);
    let cursor_style = Style::new().fg(theme.surface).bg(theme.accent);
    let chars: Vec<char> = display.chars().collect();
    let mut spans = Vec::with_capacity(3);
    let before: String = chars[..cursor_chars.min(chars.len())].iter().collect();
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
    Line::from(spans)
}

fn draw_input_block(
    f: &mut Frame,
    area: Rect,
    label: &str,
    field: &InputField,
    active: bool,
    theme: &Theme,
    mask: bool,
) {
    let border_style = Style::new().fg(if active {
        theme.accent
    } else {
        theme.text_muted
    });
    let block = Block::bordered()
        .title(format!(" {label} "))
        .title_style(border_style)
        .border_style(border_style);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let display = if mask {
        "•".repeat(field.text.chars().count())
    } else {
        field.text.clone()
    };
    let cursor_chars = field.text[..field.cursor].chars().count();
    f.render_widget(
        Paragraph::new(field_line(&display, cursor_chars, active, theme)),
        inner,
    );
}

fn draw_field_error(f: &mut Frame, area: Rect, auth: &AuthState, field: AuthField, theme: &Theme) {
    let Some(err) = &auth.field_error else {
        return;
    };
    if err.field != field {
        return;
    }
    f.render_widget(
        Paragraph::new(err.error.to_string()).style(Style::new().fg(theme.danger)),
        area,
    );
}

/// Flood-wait countdown against `AppState.now` — render only, never reads a
/// clock (architecture's global constraint on `update()` applies to views
/// too: time only ever arrives as already-cached state).
fn draw_flood_wait(f: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let Some(until) = state.auth.flood_wait_until else {
        return;
    };
    if until <= state.now {
        return;
    }
    let remaining_ms = until.0.saturating_sub(state.now.0);
    let secs = remaining_ms.div_ceil(1_000);
    f.render_widget(
        Paragraph::new(format!("Too many attempts — retry in {secs}s"))
            .style(Style::new().fg(theme.warning)),
        area,
    );
}

fn in_flight_marker(auth: &AuthState) -> &'static str {
    if auth.in_flight { "  …" } else { "" }
}

fn draw_credentials(area: Rect, auth: &AuthState, theme: &Theme, f: &mut Frame) {
    let inner = panel(area, theme, f, 70, 14, "Connect to Telegram");

    let [
        explain_area,
        _gap1,
        api_id_area,
        _gap2,
        api_hash_area,
        error_area,
        _gap3,
        hint_area,
    ] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(inner);

    let explain = Paragraph::new(vec![
        Line::from("Telegram apps need an API id and hash issued to a developer account."),
        Line::from("Visit my.telegram.org, log in with your phone number, then open"),
        Line::from("\"API development tools\" to create one."),
    ])
    .wrap(Wrap { trim: true })
    .style(Style::new().fg(theme.text_muted));
    f.render_widget(explain, explain_area);

    draw_input_block(
        f,
        api_id_area,
        "API id",
        &auth.api_id,
        auth.active_field == AuthField::ApiId,
        theme,
        false,
    );
    draw_input_block(
        f,
        api_hash_area,
        "API hash",
        &auth.api_hash,
        auth.active_field == AuthField::ApiHash,
        theme,
        false,
    );
    draw_field_error(f, error_area, auth, AuthField::ApiId, theme);

    f.render_widget(
        Paragraph::new("Tab/⏎ next field · ⏎ on API hash to continue")
            .style(Style::new().fg(theme.text_muted)),
        hint_area,
    );
}

fn option_line(label: &str, active: bool) -> Line<'static> {
    let marker = if active { "▶ " } else { "  " };
    Line::from(Span::raw(format!("{marker}{label}")))
}

fn draw_method_choice(area: Rect, auth: &AuthState, theme: &Theme, f: &mut Frame) {
    let inner = panel(area, theme, f, 50, 10, "Sign in");
    // `method: None` has no confirmed choice yet, but Up/Down's first press
    // always lands on Phone (state::auth::handle_method_choice_key), so
    // Phone is the visual default cursor position until something is armed.
    let selected = auth.method.unwrap_or(LoginMethod::Phone);

    let [intro_area, _gap1, phone_area, qr_area, _gap2, hint_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(inner);

    f.render_widget(
        Paragraph::new("How would you like to sign in?").style(Style::new().fg(theme.text)),
        intro_area,
    );

    let phone_style = option_style(selected == LoginMethod::Phone, theme);
    let qr_style = option_style(selected == LoginMethod::Qr, theme);
    f.render_widget(
        Paragraph::new(option_line("Phone number", selected == LoginMethod::Phone))
            .style(phone_style),
        phone_area,
    );
    f.render_widget(
        Paragraph::new(option_line("QR code", selected == LoginMethod::Qr)).style(qr_style),
        qr_area,
    );

    f.render_widget(
        Paragraph::new(format!(
            "↑↓ choose · p phone · q qr · ⏎ continue{}",
            in_flight_marker(auth)
        ))
        .style(Style::new().fg(theme.text_muted)),
        hint_area,
    );
}

fn option_style(active: bool, theme: &Theme) -> Style {
    if active {
        Style::new()
            .fg(theme.surface)
            .bg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(theme.text)
    }
}

fn draw_phone(area: Rect, state: &AppState, theme: &Theme, f: &mut Frame) {
    let inner = panel(area, theme, f, 50, 10, "Phone number");
    let [field_area, error_area, flood_area, hint_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(inner);

    draw_input_block(
        f,
        field_area,
        "Phone (with country code)",
        &state.auth.phone,
        true,
        theme,
        false,
    );
    draw_field_error(f, error_area, &state.auth, AuthField::Phone, theme);
    draw_flood_wait(f, flood_area, state, theme);

    f.render_widget(
        Paragraph::new(format!("⏎ send code{}", in_flight_marker(&state.auth)))
            .style(Style::new().fg(theme.text_muted)),
        hint_area,
    );
}

fn draw_code(
    area: Rect,
    state: &AppState,
    theme: &Theme,
    f: &mut Frame,
    delivery_hint: &str,
    length: u8,
) {
    let inner = panel(area, theme, f, 50, 11, "Enter code");
    let [
        hint_line_area,
        field_area,
        error_area,
        flood_area,
        hint_area,
    ] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(inner);

    f.render_widget(
        Paragraph::new(format!("{delivery_hint} ({length} digits)"))
            .style(Style::new().fg(theme.text_muted)),
        hint_line_area,
    );
    draw_input_block(f, field_area, "Code", &state.auth.code, true, theme, false);
    draw_field_error(f, error_area, &state.auth, AuthField::Code, theme);
    draw_flood_wait(f, flood_area, state, theme);

    f.render_widget(
        Paragraph::new(format!("⏎ confirm{}", in_flight_marker(&state.auth)))
            .style(Style::new().fg(theme.text_muted)),
        hint_area,
    );
}

fn draw_password(area: Rect, state: &AppState, theme: &Theme, f: &mut Frame, hint: Option<&str>) {
    let inner = panel(area, theme, f, 50, 11, "Two-step verification");
    let [
        hint_line_area,
        field_area,
        error_area,
        flood_area,
        hint_area,
    ] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(inner);

    let hint_text = match hint {
        Some(h) if !h.is_empty() => format!("Hint: {h}"),
        _ => "Enter your password".to_string(),
    };
    f.render_widget(
        Paragraph::new(hint_text).style(Style::new().fg(theme.text_muted)),
        hint_line_area,
    );
    draw_input_block(
        f,
        field_area,
        "Password",
        &state.auth.password,
        true,
        theme,
        true,
    );
    draw_field_error(f, error_area, &state.auth, AuthField::Password, theme);
    draw_flood_wait(f, flood_area, state, theme);

    f.render_widget(
        Paragraph::new(format!("⏎ confirm{}", in_flight_marker(&state.auth)))
            .style(Style::new().fg(theme.text_muted)),
        hint_area,
    );
}

fn draw_qr(area: Rect, theme: &Theme, f: &mut Frame, link: &str) {
    let [title_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .areas(area);

    f.render_widget(
        Paragraph::new("Scan this QR code with Telegram on another device")
            .alignment(Alignment::Center)
            .style(Style::new().fg(theme.text)),
        title_area,
    );

    let lines = build_qr_lines(link, theme).filter(|lines| fits(lines, body_area));
    match lines {
        Some(lines) => {
            let qr_width = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
            let qr_height = lines.len() as u16;
            let qr_rect = centered(body_area, qr_width, qr_height);
            f.render_widget(Paragraph::new(lines), qr_rect);
        }
        None => draw_qr_fallback(body_area, theme, f, link),
    }

    f.render_widget(
        Paragraph::new("Settings → Devices → Link Desktop Device")
            .alignment(Alignment::Center)
            .style(Style::new().fg(theme.text_muted)),
        footer_area,
    );
}

fn fits(lines: &[Line<'static>], area: Rect) -> bool {
    let height = lines.len() as u16;
    let width = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    height <= area.height && width <= area.width
}

fn draw_qr_fallback(area: Rect, theme: &Theme, f: &mut Frame, link: &str) {
    let text = vec![
        Line::from(Span::styled(
            "Terminal too small for QR code.",
            Style::new().fg(theme.warning),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Open this link on another device:",
            Style::new().fg(theme.text_muted),
        )),
        Line::from(Span::styled(
            link.to_string(),
            Style::new().fg(theme.accent),
        )),
    ];
    f.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        area,
    );
}

/// Renders a QR matrix as Unicode half-blocks: two QR module rows share one
/// terminal row (▀ = top dark, ▄ = bottom dark, █ = both, space = neither).
/// Returns `None` only if the link fails to encode (never for a size reason
/// — sizing is `fits`'s job, checked separately by the caller).
fn build_qr_lines(link: &str, theme: &Theme) -> Option<Vec<Line<'static>>> {
    let code = QrCode::with_error_correction_level(link.as_bytes(), EcLevel::L).ok()?;
    let module_width = code.width();
    let colors = code.to_colors();
    let total = module_width + QR_QUIET_ZONE * 2;

    let is_dark = |x: usize, y: usize| -> bool {
        if x < QR_QUIET_ZONE || y < QR_QUIET_ZONE {
            return false;
        }
        let (mx, my) = (x - QR_QUIET_ZONE, y - QR_QUIET_ZONE);
        mx < module_width
            && my < module_width
            && colors[my * module_width + mx] == qrcode::Color::Dark
    };

    let dark = Style::new().fg(theme.text);
    let dark_on_light = Style::new().fg(theme.text).bg(theme.surface);
    let light = Style::new().bg(theme.surface);

    let mut lines = Vec::with_capacity(total.div_ceil(2));
    let mut row = 0usize;
    while row < total {
        let mut spans = Vec::with_capacity(total);
        for col in 0..total {
            let top = is_dark(col, row);
            let bottom = row + 1 < total && is_dark(col, row + 1);
            spans.push(match (top, bottom) {
                (true, true) => Span::styled("█", dark),
                (true, false) => Span::styled("▀", dark_on_light),
                (false, true) => Span::styled("▄", dark_on_light),
                (false, false) => Span::styled(" ", light),
            });
        }
        lines.push(Line::from(spans));
        row += 2;
    }
    Some(lines)
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
    use tgt_core::state::auth::FieldError;
    use tgt_core::state::chat_list::ChatListState;
    use tgt_core::state::composer::ComposerState;
    use tgt_core::state::consent::{ConsentChoice, ConsentState};
    use tgt_core::state::focus::{Focus, FocusStack};
    use tgt_core::state::media::MediaState;
    use tgt_core::state::presence::PresenceState;
    use tgt_core::state::toasts::ToastState;
    use tgt_core::td::error::TdError;
    use tgt_core::td::update::ConnectionPhase;

    use super::*;

    /// Short enough that its QR (EcLevel::L) fits an 70x20 terminal, so both
    /// the "renders a QR" and "too small, falls back" paths get genuinely
    /// exercised at different viewport sizes.
    const FIXED_QR_LINK: &str = "tg://login?token=AAAABBBBCCCC";

    fn fixture_state() -> AppState {
        AppState {
            screen: Screen::Auth,
            focus: FocusStack::new(Focus::ChatList),
            connection: ConnectionPhase::Ready,
            consent: ConsentState {
                selected: ConsentChoice::Enable,
                acknowledged: true,
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
    fn method_choice_120x40() {
        let mut state = fixture_state();
        state.auth.phase = AuthPhase::WaitPhoneNumber;
        state.auth.method = None;
        insta::assert_snapshot!(render_to_string(120, 40, &state));
    }

    #[test]
    fn method_choice_70x20() {
        let mut state = fixture_state();
        state.auth.phase = AuthPhase::WaitPhoneNumber;
        state.auth.method = Some(LoginMethod::Qr);
        insta::assert_snapshot!(render_to_string(70, 20, &state));
    }

    #[test]
    fn code_entry_with_error_120x40() {
        let mut state = fixture_state();
        state.auth.phase = AuthPhase::WaitCode {
            delivery_hint: "SMS to +1***34".to_string(),
            length: 5,
        };
        state.auth.active_field = AuthField::Code;
        state.auth.code.text = "0000".to_string();
        state.auth.code.cursor = 4;
        state.auth.field_error = Some(FieldError {
            field: AuthField::Code,
            error: TdError::CodeInvalid,
        });
        insta::assert_snapshot!(render_to_string(120, 40, &state));
    }

    #[test]
    fn code_entry_with_error_70x20() {
        let mut state = fixture_state();
        state.auth.phase = AuthPhase::WaitCode {
            delivery_hint: "SMS to +1***34".to_string(),
            length: 5,
        };
        state.auth.active_field = AuthField::Code;
        state.auth.code.text = "0000".to_string();
        state.auth.code.cursor = 4;
        state.auth.field_error = Some(FieldError {
            field: AuthField::Code,
            error: TdError::CodeInvalid,
        });
        insta::assert_snapshot!(render_to_string(70, 20, &state));
    }

    #[test]
    fn qr_screen_120x40() {
        let mut state = fixture_state();
        state.auth.phase = AuthPhase::WaitOtherDeviceConfirmation {
            link: FIXED_QR_LINK.to_string(),
        };
        state.auth.method = Some(LoginMethod::Qr);
        insta::assert_snapshot!(render_to_string(120, 40, &state));
    }

    #[test]
    fn qr_screen_70x20() {
        let mut state = fixture_state();
        state.auth.phase = AuthPhase::WaitOtherDeviceConfirmation {
            link: FIXED_QR_LINK.to_string(),
        };
        state.auth.method = Some(LoginMethod::Qr);
        insta::assert_snapshot!(render_to_string(70, 20, &state));
    }

    #[test]
    fn qr_screen_too_small_falls_back_to_link_40x12() {
        let mut state = fixture_state();
        state.auth.phase = AuthPhase::WaitOtherDeviceConfirmation {
            link: FIXED_QR_LINK.to_string(),
        };
        state.auth.method = Some(LoginMethod::Qr);
        let rendered = render_to_string(40, 12, &state);
        assert!(
            rendered.contains("too small"),
            "expected the too-small fallback note:\n{rendered}"
        );
        assert!(
            rendered.contains(FIXED_QR_LINK),
            "expected the raw link as a fallback:\n{rendered}"
        );
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn credentials_wizard_shows_explainer_and_fields() {
        let mut state = fixture_state();
        state.auth.active_field = AuthField::ApiId;
        let rendered = render_to_string(120, 40, &state);
        assert!(rendered.contains("my.telegram.org"));
        assert!(rendered.contains("API id"));
        assert!(rendered.contains("API hash"));
    }

    #[test]
    fn unsupported_phase_renders_state_name() {
        let mut state = fixture_state();
        state.auth.phase = AuthPhase::Unsupported {
            name: "authorizationStateWaitRegistration".to_string(),
        };
        let rendered = render_to_string(120, 40, &state);
        assert!(rendered.contains("authorizationStateWaitRegistration"));
    }

    #[test]
    fn flood_wait_renders_countdown() {
        let mut state = fixture_state();
        state.auth.phase = AuthPhase::WaitPhoneNumber;
        state.auth.method = Some(LoginMethod::Phone);
        state.now = Millis(1_000);
        state.auth.flood_wait_until = Some(Millis(3_500));
        let rendered = render_to_string(120, 40, &state);
        assert!(rendered.contains("retry in 3s"), "buffer:\n{rendered}");
    }
}
