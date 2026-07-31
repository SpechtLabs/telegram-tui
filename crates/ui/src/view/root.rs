//! Responsive root arrangement (spec §6.1). This task lays down the two-pane
//! frame only; the single-pane stack below `layout_breakpoint_cols` is T31's.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};
use tgt_core::app::AppState;

use crate::render::cache::LayoutCache;
use crate::theme::Theme;
use crate::view::{chat_list, conversation, header, hint_bar};

const SIDEBAR_WIDTH: u16 = 30;

/// `cache` is threaded down to the conversation pane, the only view that lays
/// messages out and therefore the only one that can hit or fill it.
pub fn draw(state: &AppState, theme: &Theme, f: &mut Frame, cache: &mut LayoutCache) {
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

    chat_list::draw(sidebar_area, state, theme, f);
    draw_main(main_area, state, theme, f, cache);
    hint_bar::draw(hint_area, theme, f);
}

fn draw_main(area: Rect, state: &AppState, theme: &Theme, f: &mut Frame, cache: &mut LayoutCache) {
    let [header_area, conversation_area, composer_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(3),
    ])
    .areas(area);

    header::draw(header_area, state, theme, f);
    conversation::draw(conversation_area, state, theme, f, cache);

    // Composer placeholder: real input handling lands in T30.
    let composer_block = Block::bordered().border_style(Style::new().fg(theme.text_muted));
    let composer_inner = composer_block.inner(composer_area);
    f.render_widget(composer_block, composer_area);
    f.render_widget(
        Paragraph::new("›  message…").style(Style::new().fg(theme.text_muted)),
        composer_inner,
    );
}
