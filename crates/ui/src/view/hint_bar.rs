//! Bottom hint bar: the key legend for whichever pane currently has focus
//! (spec §6.1 mock, §6.2's per-context key table).
//!
//! [`hint_for`] is the pure mapping from `Focus` to hint text; `Selection`
//! deliberately maps to `None` because spec §6.3 replaces the hint bar with
//! the chip row while selection mode is active — this module only says "no
//! hint here", it does not decide what to draw instead. That decision (chip
//! row vs. hint bar) belongs to whichever view owns the frame layout.
//!
//! [`draw`] keeps its original three-argument shape — `crates/ui/src/view/
//! root.rs` (owned by a sibling task) already calls it that way, and always
//! wants the base `ChatList` hint until that task's root wiring is done —
//! so it renders exactly what it always has via [`hint_for(&Focus::ChatList)`].
//! [`draw_for`] is the real, context-aware entry point: it renders whatever
//! [`hint_for`] yields for the given focus, drawing nothing on `None`. T32
//! switches `root.rs`'s call over to it (and to the chip row) once selection
//! mode has somewhere to go.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use tgt_core::state::focus::Focus;

use crate::theme::Theme;

/// The `ChatList` hint — also today's only hint, until [`draw_for`] is wired
/// up. Spec §6.2: `↑` `↓` move, `⏎` open, `/` filter, plus the two global
/// bindings that are always live.
pub const HINT_TEXT: &str = "↑↓ move   ⏎ open   / filter   ctrl+p palette   ? help";

const COMPOSER_HINT: &str = "⏎ send   alt+⏎ newline   ↑ select messages";
const MODAL_HINT: &str = "⏎ confirm   esc cancel";
const CHAT_FILTER_HINT: &str = "type to filter   ⏎ open   esc cancel";
const CHAT_SEARCH_HINT: &str = "type to search   ⏎ jump   esc cancel";
const PALETTE_HINT: &str = "type to search   ⏎ run   esc cancel";
const HELP_HINT: &str = "esc close";

/// The hint line for `focus`, or `None` when the pane owning that focus
/// replaces the hint bar with something else entirely (selection mode's
/// chip row, spec §6.3).
pub fn hint_for(focus: &Focus) -> Option<&'static str> {
    match focus {
        Focus::ChatList => Some(HINT_TEXT),
        Focus::ChatFilter => Some(CHAT_FILTER_HINT),
        Focus::Composer => Some(COMPOSER_HINT),
        Focus::Selection => None,
        Focus::ChatSearch => Some(CHAT_SEARCH_HINT),
        Focus::Palette => Some(PALETTE_HINT),
        Focus::Help => Some(HELP_HINT),
        Focus::Modal(_) => Some(MODAL_HINT),
    }
}

/// Renders the base `ChatList` hint. Kept as the stable, context-free entry
/// point `root.rs` already calls; see the module doc comment for why.
pub fn draw(area: Rect, theme: &Theme, f: &mut Frame) {
    draw_for(area, &Focus::ChatList, theme, f);
}

/// Renders whatever [`hint_for`] yields for `focus`; draws nothing on
/// `None`.
///
/// No rule above it and no box around it (design language §1): the blank row
/// `view::root` reserves is the whole separation. Each entry is split into
/// its key (in `accent`, so the eye can scan the row for a key rather than
/// reading it as a sentence) and its description (`text_muted`), leaving the
/// rendered characters byte-identical to the constants above — the frame
/// tests assert on those strings appearing verbatim in the buffer.
pub fn draw_for(area: Rect, focus: &Focus, theme: &Theme, f: &mut Frame) {
    let Some(text) = hint_for(focus) else {
        return;
    };
    f.render_widget(Paragraph::new(hint_line(text, theme)), area);
}

/// Columns between two entries in a hint string. Wide enough that entries
/// read as separate items without a `·` or a `|` between them.
const ENTRY_GAP: &str = "   ";

/// Splits a hint into `key`/`description` spans. An entry is everything up
/// to the next [`ENTRY_GAP`]; its key is the first whitespace-delimited
/// word, which is exactly how every constant above is written.
fn hint_line(text: &'static str, theme: &Theme) -> Line<'static> {
    let key_style = Style::new().fg(theme.accent);
    let desc_style = Style::new().fg(theme.text_muted);

    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, entry) in text.split(ENTRY_GAP).enumerate() {
        if i > 0 {
            spans.push(Span::styled(ENTRY_GAP, desc_style));
        }
        match entry.split_once(' ') {
            Some((key, desc)) => {
                spans.push(Span::styled(key, key_style));
                spans.push(Span::styled(" ", desc_style));
                spans.push(Span::styled(desc, desc_style));
            }
            None => spans.push(Span::styled(entry, key_style)),
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tgt_core::model::ids::{ChatId, MessageId};
    use tgt_core::state::focus::ModalKind;

    use super::*;

    fn render_to_string(width: u16, height: u16, focus: &Focus) -> String {
        let theme = Theme::default_dark();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                draw_for(area, focus, &theme, f);
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

    #[test]
    fn chat_list_hint_matches_the_published_constant() {
        assert_eq!(hint_for(&Focus::ChatList), Some(HINT_TEXT));
        assert!(HINT_TEXT.contains("move"));
        assert!(HINT_TEXT.contains("open"));
        assert!(HINT_TEXT.contains("filter"));
        assert!(HINT_TEXT.contains("palette"));
        assert!(HINT_TEXT.contains("help"));
    }

    #[test]
    fn composer_hint_covers_send_newline_and_selection_entry() {
        let hint = hint_for(&Focus::Composer).expect("composer has a hint");
        assert!(hint.contains("send"));
        assert!(hint.contains("newline"));
        assert!(hint.contains("select messages"));
    }

    #[test]
    fn selection_mode_has_no_hint_bar_text() {
        assert_eq!(hint_for(&Focus::Selection), None);
    }

    #[test]
    fn modal_hint_covers_confirm_and_cancel() {
        let kind = ModalKind::ConfirmDelete {
            chat_id: ChatId(1),
            message_id: MessageId(1),
            can_revoke: true,
        };
        let hint = hint_for(&Focus::Modal(kind)).expect("modal has a hint");
        assert!(hint.contains("confirm"));
        assert!(hint.contains("cancel"));
    }

    #[test]
    fn draw_for_renders_nothing_during_selection() {
        let rendered = render_to_string(60, 1, &Focus::Selection);
        assert_eq!(rendered.trim(), "");
    }

    #[test]
    fn draw_renders_the_chat_list_hint_120x1() {
        let mut terminal = Terminal::new(TestBackend::new(120, 1)).unwrap();
        let theme = Theme::default_dark();
        terminal
            .draw(|f| {
                let area = f.area();
                draw(area, &theme, f);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for row in buffer.content.chunks(buffer.area.width as usize) {
            for cell in row {
                out.push_str(cell.symbol());
            }
        }
        insta::assert_snapshot!(out);
    }
}
