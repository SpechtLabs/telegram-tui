//! Composer view: rounded input box, reply/edit banners (spec §6.1 mock).
//!
//! ```text
//!   ↳ Alice: hey, did you see the PR?    <- reply banner (only when reply_to)
//!   ✎ editing message                   <- edit banner (only when editing)
//! ╭──────────────────────────────────────╮
//! │  ›  message…                         │
//! ╰──────────────────────────────────────╯
//! ```
//!
//! ## Layout
//!
//! The caller (`view::root`, T31) hands us a fixed `area`; we never grow it.
//! Banner lines stack above the input box, each exactly one row, then the
//! rounded box fills the remainder. Multi-line input is split on `'\n'` with
//! no further word-wrap (that is `render::wrap`'s job for message bodies, not
//! this free-form composer); if the buffer has more logical lines than the
//! box has rows, the view scrolls just enough to keep the cursor's line
//! visible.
//!
//! ## Cursor
//!
//! `InputField.cursor` is a byte offset into the whole (possibly multi-line)
//! buffer. [`cursor_position`] turns that into `(line index, display
//! column)` — display column, not char count, so a wide grapheme (CJK, most
//! emoji) before the cursor still lands it in the right cell. The cursor
//! itself renders as a manually painted reverse-video cell (same convention
//! as `view::auth`'s `field_line`), not the terminal's hardware cursor.
//!
//! ## Upload progress is not drawn here
//!
//! An earlier note in this module promised a banner line for file-send
//! progress, "keyed off `AppState.media` for the chat's in-flight upload".
//! That was wrong about where it belongs and it sent a later audit looking
//! in this file for something that already exists elsewhere.
//!
//! Spec §10: "Uploads render as a pending message with a progress bar and
//! are cancellable" — on the message, not on the composer. It is built and
//! wired: `view/conversation.rs` calls `message_layout::file_card_upload_line`
//! for any message with a `MediaState::uploads` entry, per-frame and outside
//! the layout cache, beside the download line it mirrors.
//!
//! Do not add a second, composer-side rendering of the same fact. The
//! composer's banners are about what the *composer* is holding — a reply
//! target, an edit, an unsent draft — and an upload in flight is a property
//! of the message it belongs to.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Padding, Paragraph};
use tgt_core::app::AppState;
use tgt_core::model::ids::MessageId;
use tgt_core::model::message::MessageContent;
use tgt_core::state::auth::InputField;
use tgt_core::state::focus::Focus;
use tgt_core::state::selection::REPLY_EXCERPT_MAX_CHARS;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

const PROMPT: &str = "› ";
// Leading space matches the spec §6.1 mock's `›  message…` (arrow, two
// spaces, placeholder) and `view::root`'s existing pre-T30 stub text.
const PLACEHOLDER: &str = " message…";
/// Border column plus the box's one column of interior padding: what a
/// banner has to clear to sit above the draft rather than beside it.
const BANNER_INDENT: usize = 2;

pub fn draw(area: Rect, state: &AppState, theme: &Theme, f: &mut Frame) {
    let composer = &state.composer;

    let mut banners: Vec<Line<'static>> = Vec::new();
    if let Some(reply_id) = composer.reply_to {
        banners.push(reply_banner_line(state, reply_id, theme));
    }
    if composer.editing.is_some() {
        banners.push(banner_line("✎ editing message".to_string(), theme.warning));
    }
    if composer.pending_send.is_some() {
        banners.push(banner_line("sending…".to_string(), theme.text_muted));
    }

    let banner_height = banners.len() as u16;
    let [banner_area, box_area] =
        Layout::vertical([Constraint::Length(banner_height), Constraint::Min(0)]).areas(area);

    if !banners.is_empty() {
        f.render_widget(Paragraph::new(banners), banner_area);
    }

    let focused = matches!(state.focus.current(), Focus::Composer);
    draw_input_box(box_area, &composer.input, focused, theme, f);
}

/// A banner above the box, indented by the box's own border and padding so
/// its text lines up with the draft underneath it.
fn banner_line(text: String, color: ratatui::style::Color) -> Line<'static> {
    Line::from(vec![
        Span::raw(" ".repeat(BANNER_INDENT)),
        Span::styled(text, Style::new().fg(color)),
    ])
}

/// The composer is the one box the design language keeps: a border is the
/// clearest way to say "type here". It stays in `theme.border` and brightens
/// to `accent` only while the composer holds focus, so focus is shown by the
/// affordance rather than by recoloring the text inside it
/// (docs/design-language.md §1).
fn draw_input_box(area: Rect, field: &InputField, focused: bool, theme: &Theme, f: &mut Frame) {
    let border = if focused { theme.accent } else { theme.border };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    if field.text.is_empty() {
        let line = Line::from(vec![
            Span::styled(PROMPT, Style::new().fg(theme.text_muted)),
            Span::styled(PLACEHOLDER, Style::new().fg(theme.text_muted)),
        ]);
        f.render_widget(Paragraph::new(line), inner);
        return;
    }

    let lines: Vec<&str> = field.text.split('\n').collect();
    let (cursor_line, cursor_col) = cursor_position(&field.text, field.cursor);

    let visible = inner.height as usize;
    let total = lines.len();
    let top = if total <= visible {
        0
    } else {
        let max_top = total - visible;
        cursor_line.saturating_sub(visible - 1).min(max_top)
    };

    let rendered: Vec<Line<'static>> = lines
        .into_iter()
        .enumerate()
        .skip(top)
        .take(visible)
        .map(|(idx, text)| render_input_line(idx, text, cursor_line, cursor_col, theme))
        .collect();

    f.render_widget(Paragraph::new(rendered), inner);
}

/// Byte offset `cursor` into `text` -> `(line index, display column)`. Line
/// index counts `'\n'`s before the cursor; column is the display width
/// (`unicode-width`, not char count) of the cursor's line up to the cursor.
fn cursor_position(text: &str, cursor: usize) -> (usize, usize) {
    let cursor = cursor.min(text.len());
    let before = &text[..cursor];
    let line_idx = before.matches('\n').count();
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let col = UnicodeWidthStr::width(&before[line_start..]);
    (line_idx, col)
}

/// The prompt (`"› "`) prefixes only the buffer's first logical line;
/// continuation lines are padded to the same width so wrapped/typed text
/// still lines up under it.
fn render_input_line(
    idx: usize,
    text: &str,
    cursor_line: usize,
    cursor_col: usize,
    theme: &Theme,
) -> Line<'static> {
    let prefix = if idx == 0 {
        Span::styled(PROMPT, Style::new().fg(theme.text_muted))
    } else {
        Span::raw(" ".repeat(UnicodeWidthStr::width(PROMPT)))
    };
    if idx == cursor_line {
        render_cursor_line(prefix, text, cursor_col, theme)
    } else {
        let mut spans = vec![prefix];
        if !text.is_empty() {
            spans.push(Span::styled(text.to_string(), Style::new().fg(theme.text)));
        }
        Line::from(spans)
    }
}

/// Renders `text` with a reverse-video cell at display column `cursor_col`.
/// Grapheme-cluster aware, so a cursor landing on a multi-codepoint emoji or
/// a combining-mark sequence highlights the whole cluster, never half of it.
/// `cursor_col` past the end of the line (the common case: cursor after the
/// last character) renders as a single reverse-video space, matching
/// `view::auth`'s `field_line` convention.
fn render_cursor_line(
    prefix: Span<'static>,
    text: &str,
    cursor_col: usize,
    theme: &Theme,
) -> Line<'static> {
    let base = Style::new().fg(theme.text);
    let cursor_style = Style::new().fg(theme.surface).bg(theme.text);

    let mut before = String::new();
    let mut cursor_grapheme: Option<String> = None;
    let mut after = String::new();
    let mut col = 0usize;
    for g in text.graphemes(true) {
        if cursor_grapheme.is_none() && col == cursor_col {
            cursor_grapheme = Some(g.to_string());
        } else if cursor_grapheme.is_some() {
            after.push_str(g);
        } else {
            before.push_str(g);
        }
        col += UnicodeWidthStr::width(g);
    }

    let mut spans = vec![prefix];
    if !before.is_empty() {
        spans.push(Span::styled(before, base));
    }
    match cursor_grapheme {
        Some(g) => spans.push(Span::styled(g, cursor_style)),
        None => spans.push(Span::styled(" ", cursor_style)),
    }
    if !after.is_empty() {
        spans.push(Span::styled(after, base));
    }
    Line::from(spans)
}

/// Dimmed `↳ SenderName: excerpt`, resolved from the open chat's loaded
/// window. `reply_to` on `ComposerState` is only ever a `MessageId` (unlike
/// `MessageView.reply_to`'s already-filled `ReplyPreview`), so this looks the
/// source message up itself; a message not currently in the window (evicted,
/// or history not paged in that far) falls back to the same bare `↳ …` the
/// message layout engine uses for an empty excerpt (`render::message_layout`).
fn reply_banner_line(state: &AppState, reply_id: MessageId, theme: &Theme) -> Line<'static> {
    let message = state
        .open_chat
        .and_then(|chat_id| state.conversations.get(&chat_id))
        .and_then(|convo| convo.messages.iter().find(|m| m.id == reply_id));

    let text = match message {
        Some(msg) => format!("↳ {}: {}", msg.sender_name, excerpt_of(&msg.content)),
        None => "↳ …".to_string(),
    };
    banner_line(text, theme.text_muted)
}

/// One line, capped at [`REPLY_EXCERPT_MAX_CHARS`] characters. Mirrors
/// `state::selection`'s private `excerpt_of` (same cap, same per-kind
/// fallback text) since that helper isn't exposed across the crate boundary;
/// duplicated here rather than made `pub` there to keep this the only ui-side
/// caller.
fn excerpt_of(content: &MessageContent) -> String {
    let raw: &str = match content {
        MessageContent::Text(f) => &f.text,
        MessageContent::Photo { caption, .. } if !caption.text.is_empty() => &caption.text,
        MessageContent::Photo { .. } => "Photo",
        MessageContent::Video {
            caption, file_name, ..
        } => {
            if caption.text.is_empty() {
                file_name
            } else {
                &caption.text
            }
        }
        MessageContent::Audio { file_name, .. } => file_name,
        MessageContent::Document {
            caption, file_name, ..
        } => {
            if caption.text.is_empty() {
                file_name
            } else {
                &caption.text
            }
        }
        MessageContent::Sticker { emoji } => emoji,
        MessageContent::Unsupported { description } => description,
    };
    let line = raw.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= REPLY_EXCERPT_MAX_CHARS {
        line.to_string()
    } else {
        let mut out: String = line.chars().take(REPLY_EXCERPT_MAX_CHARS - 1).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tgt_core::app::{AppState, Screen};
    use tgt_core::effect::TelemetryMode;
    use tgt_core::model::entity::FormattedText;
    use tgt_core::model::ids::{ChatId, UserId};
    use tgt_core::model::key::KeyBindings;
    use tgt_core::model::message::{MessageCaps, MessageView, SendState, Sender};
    use tgt_core::model::time::Millis;
    use tgt_core::state::auth::{AuthField, AuthState, InputField};
    use tgt_core::state::chat_list::ChatListState;
    use tgt_core::state::composer::ComposerState;
    use tgt_core::state::consent::{ConsentChoice, ConsentState};
    use tgt_core::state::conversation::{ConversationState, Scroll};
    use tgt_core::state::focus::{Focus, FocusStack};
    use tgt_core::state::history::PagingState;
    use tgt_core::state::media::MediaState;
    use tgt_core::state::presence::PresenceState;
    use tgt_core::state::toasts::ToastState;
    use tgt_core::td::update::{AuthPhase, ConnectionPhase};

    use super::*;

    const CHAT: ChatId = ChatId(1);

    fn fixture_state() -> AppState {
        AppState {
            screen: Screen::Main,
            focus: FocusStack::new(Focus::Composer),
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
            modal_ui: None,
            palette: None,
            chat_search: None,
            toasts: ToastState::default(),
            media: MediaState::default(),
            presence: PresenceState::default(),
            width: 40,
            height: 8,
            layout_breakpoint_cols: 100,
            theme_name: "dark".to_string(),
            theme_generation: 0,
            bindings: KeyBindings::default(),
            telemetry_mode: TelemetryMode::Off,
            crash_reports_available: false,
            telemetry_salt: [0u8; 32],
            now: Millis(0),
        }
    }

    fn sample_message(id: i64, sender_name: &str, text: &str) -> MessageView {
        MessageView {
            id: tgt_core::model::ids::MessageId(id),
            chat_id: CHAT,
            sender: Sender::User(UserId(1)),
            sender_name: sender_name.to_string(),
            is_outgoing: false,
            date: 0,
            content: MessageContent::Text(FormattedText {
                text: text.to_string(),
                entities: Vec::new(),
            }),
            reply_to: None,
            send_state: SendState::Sent,
            reactions: Vec::new(),
            caps: MessageCaps::default(),
            is_edited: false,
        }
    }

    fn open_conversation(state: &mut AppState, messages: Vec<MessageView>) {
        state.conversations.insert(
            CHAT,
            ConversationState {
                chat_id: CHAT,
                messages: VecDeque::from(messages),
                paging: PagingState::Idle,
                scroll: Scroll::Bottom,
                revealed_spoilers: Default::default(),
                last_read_inbox: tgt_core::model::ids::MessageId(0),
                last_read_outbox: tgt_core::model::ids::MessageId(0),
                pending_view: None,
                search_hits: Vec::new(),
                selection: None,
            },
        );
    }

    fn render_to_string(width: u16, height: u16, state: &AppState) -> String {
        let theme = Theme::default_dark();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f.area(), state, &theme, f)).unwrap();
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
    fn empty_placeholder_40x4() {
        let state = fixture_state();
        insta::assert_snapshot!(render_to_string(40, 4, &state));
    }

    #[test]
    fn multiline_content_with_cursor_mid_text_40x6() {
        let mut state = fixture_state();
        // Cursor sits right after "fir" on the first line — mid-text, not at
        // an edge, and on a non-final line so the box must show both lines.
        state.composer.input.text = "first line\nsecond line".to_string();
        state.composer.input.cursor = 3;
        insta::assert_snapshot!(render_to_string(40, 6, &state));
    }

    #[test]
    fn reply_banner_40x5() {
        let mut state = fixture_state();
        open_conversation(
            &mut state,
            vec![sample_message(1, "Alice", "hey, did you see the PR?")],
        );
        state.composer.reply_to = Some(tgt_core::model::ids::MessageId(1));
        insta::assert_snapshot!(render_to_string(40, 5, &state));
    }

    #[test]
    fn edit_banner_40x5() {
        let mut state = fixture_state();
        state.composer.editing = Some(tgt_core::model::ids::MessageId(7));
        state.composer.input.text = "edited text".to_string();
        state.composer.input.cursor = 11;
        insta::assert_snapshot!(render_to_string(40, 5, &state));
    }

    #[test]
    fn reply_banner_falls_back_to_ellipsis_when_message_not_in_window() {
        let mut state = fixture_state();
        open_conversation(&mut state, Vec::new());
        state.composer.reply_to = Some(tgt_core::model::ids::MessageId(99));
        let rendered = render_to_string(40, 5, &state);
        assert!(rendered.contains("↳ …"), "buffer:\n{rendered}");
    }

    #[test]
    fn pending_send_renders_sending_indicator() {
        let mut state = fixture_state();
        state.composer.pending_send = Some("hello".to_string());
        let rendered = render_to_string(40, 5, &state);
        assert!(rendered.contains("sending…"), "buffer:\n{rendered}");
    }

    #[test]
    fn multiline_content_scrolls_to_keep_cursor_visible() {
        let mut state = fixture_state();
        state.composer.input.text = "one\ntwo\nthree\nfour\nfive".to_string();
        state.composer.input.cursor = state.composer.input.text.len(); // end, on "five"
        // Only 2 inner rows: the box must scroll so the cursor's line ("five")
        // is visible, not stuck showing "one"/"two" while the cursor is
        // off-screen below.
        let rendered = render_to_string(40, 4, &state);
        assert!(rendered.contains("five"), "buffer:\n{rendered}");
        assert!(!rendered.contains("one"), "buffer:\n{rendered}");
    }
}
