//! Chat header strip. Once a chat is open, later tasks (T22, T35) fill in the
//! chat title, presence, and typing indicator; for now this renders the app
//! title and the TDLib connection indicator (spec §14: "connecting…" /
//! "updating…" must be visible rather than manifesting as silence).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use tgt_core::app::AppState;
use tgt_core::td::update::ConnectionPhase;

use crate::theme::Theme;

const TITLE: &str = "telegram-tui";

pub fn draw(area: Rect, state: &AppState, theme: &Theme, f: &mut Frame) {
    let block = Block::bordered().border_style(Style::new().fg(theme.text_muted));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut spans = vec![Span::styled(TITLE, Style::new().fg(theme.text))];
    if let Some(label) = connection_label(state.connection) {
        let used = TITLE.len() + label.len();
        let pad = (inner.width as usize).saturating_sub(used);
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(label, Style::new().fg(theme.warning)));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), inner);
}

fn connection_label(phase: ConnectionPhase) -> Option<&'static str> {
    match phase {
        ConnectionPhase::WaitingForNetwork => Some("waiting for network…"),
        ConnectionPhase::Connecting => Some("connecting…"),
        ConnectionPhase::ConnectingToProxy => Some("connecting to proxy…"),
        ConnectionPhase::Updating => Some("updating…"),
        ConnectionPhase::Ready => None,
    }
}
