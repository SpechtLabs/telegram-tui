//! Responsive root arrangement (spec §6.1). This task lays down the two-pane
//! frame only; the single-pane stack below `layout_breakpoint_cols` is T31's.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};
use tgt_core::app::AppState;

use crate::theme::Theme;
use crate::view::{header, hint_bar};

const SIDEBAR_WIDTH: u16 = 30;

pub fn draw(state: &AppState, theme: &Theme, f: &mut Frame) {
    let area = f.area();
    f.render_widget(Block::new().style(Style::new().bg(theme.surface)), area);

    let outer = Block::bordered()
        .title(Line::from(" telegram-tui ").left_aligned())
        .border_style(Style::new().fg(theme.text_muted));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let [content_area, hint_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);

    let [sidebar_area, main_area] =
        Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)])
            .areas(content_area);

    draw_sidebar(sidebar_area, theme, f);
    draw_main(main_area, state, theme, f);
    hint_bar::draw(hint_area, theme, f);
}

fn draw_sidebar(area: Rect, theme: &Theme, f: &mut Frame) {
    let block = Block::bordered()
        .title("CHATS")
        .title_style(Style::new().fg(theme.text))
        .border_style(Style::new().fg(theme.text_muted));
    f.render_widget(block, area);
}

fn draw_main(area: Rect, state: &AppState, theme: &Theme, f: &mut Frame) {
    let [header_area, conversation_area, composer_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(3),
    ])
    .areas(area);

    header::draw(header_area, state, theme, f);

    // Message viewport: empty until T23 wires the conversation view in.
    let conversation_block = Block::bordered().border_style(Style::new().fg(theme.text_muted));
    f.render_widget(conversation_block, conversation_area);

    // Composer placeholder: real input handling lands in T30.
    let composer_block = Block::bordered().border_style(Style::new().fg(theme.text_muted));
    let composer_inner = composer_block.inner(composer_area);
    f.render_widget(composer_block, composer_area);
    f.render_widget(
        Paragraph::new("›  message…").style(Style::new().fg(theme.text_muted)),
        composer_inner,
    );
}
