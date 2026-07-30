//! Bottom hint bar. Context-dependent hints land in T29; for now it always
//! shows the base chat-list/global hints (spec §6.1 mock).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Paragraph;

use crate::theme::Theme;

pub const HINT_TEXT: &str = "↑↓ move   ⏎ open   ctrl+p palette   ? help";

pub fn draw(area: Rect, theme: &Theme, f: &mut Frame) {
    let hint = Paragraph::new(HINT_TEXT).style(Style::new().fg(theme.text_muted));
    f.render_widget(hint, area);
}
