//! Message layout: `MessageView` -> laid-out, styled, wrapped `Line`s.
//!
//! Spec §7.1 (grouped accent-rail rendering) and §8.1 (the two hazards). This
//! is a pure function: no application state, no clock, no I/O. The same
//! `(msg, width, theme)` always produces byte-identical output, which is what
//! makes the layout cache in `render::cache` sound.
//!
//! ## Composition
//!
//! ```text
//! layout_message
//!   +- header line          "Alice Müller · 14:02" (+ dim " (edited)")
//!   +- reply preview line   dim "↳ excerpt", truncated to one line
//!   +- body lines           content-dependent, wrapped to the inner width
//! ```
//!
//! Entity spans resolve through `render::offsets::utf16_span_to_byte_range`
//! (hazard 1: Telegram offsets are UTF-16 code units) and wrapping goes
//! through `render::wrap::wrap_spans` (hazard 2: width is display columns).
//! Both callees contain their hazard; what lives here is the composition
//! around them.
//!
//! ## Rails and alignment (spec §7.1)
//!
//! Incoming messages carry the rail `▏` in the sender's deterministic accent
//! color as a left prefix on every body line, followed by one space, so the
//! body occupies `width - 2` columns. Own (outgoing) messages are right
//! aligned with a dim `rail_own` rail on the *right*: each wrapped line is
//! left-padded so that `pad + text + " " + "▏"` fills exactly `width`
//! columns. Headers carry no rail; an own message's header is right aligned
//! to the body's right text edge (`width - 2`) so the two line up.
//!
//! Below three columns there is no room for a rail, its space, and a column
//! of text; such widths still lay out without panicking, but the lines can
//! exceed the requested width. So can a single grapheme wider than the inner
//! width (`wrap_spans` gives it a line of its own) — there is no narrower way
//! to render either.
//!
//! ## Grouping
//!
//! Consecutive same-sender messages inside [`GROUP_WINDOW_SECS`] share one
//! header (spec §7.1: this costs zero extra rows per message). The decision
//! belongs to the caller (the conversation view, T23), so this module exposes
//! two entry points rather than hiding a flag in state:
//!
//! - [`layout_message`] — the architecture §4.9 signature, verbatim; emits the
//!   header.
//! - [`layout_message_grouped`] — same layout, header line omitted.
//!
//! [`groups_with`] implements the grouping predicate itself, so the caller
//! does not re-derive it.
//!
//! ## Time zone
//!
//! Timestamps format as **UTC** (`%H:%M`). Rendering in the viewer's local
//! zone is a later polish concern; formatting local time here would make every
//! layout test depend on the machine's `TZ`.
//!
//! ## Milestone scope
//!
//! Styled entities this milestone: bold, italic, code, url/text_url. The
//! remaining kinds (underline, strikethrough, spoiler, pre, blockquote,
//! mention, hashtag) render as plain body text; T33 fills in the arms of
//! [`entity_style`] without touching the slicing around it. Delivery ticks,
//! reactions, and real download affordances on file cards are likewise later
//! tasks (T33, T37).

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::ops::Range;
use tgt_core::model::entity::{EntityKind, FormattedText};
use tgt_core::model::message::{MessageContent, MessageView};

use crate::render::offsets::utf16_span_to_byte_range;
use crate::render::wrap::wrap_spans;
use crate::theme::Theme;

/// Grouping decision made by the caller (conversation view): consecutive
/// same-sender messages within this window share one header line.
pub const GROUP_WINDOW_SECS: i64 = 300;

/// The accent rail (U+258F LEFT ONE EIGHTH BLOCK), spec §7.1.
const RAIL: &str = "▏";

/// Columns the rail and its adjoining space take on every body line.
const RAIL_COLS: u16 = 2;

/// Lay out a message including its "Sender · HH:MM" header.
///
/// This is the architecture §4.9 signature, verbatim. Use
/// [`layout_message_grouped`] for a message continuing the block above it.
pub fn layout_message(msg: &MessageView, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    layout(msg, width, theme, true)
}

/// Lay out a message that groups under the preceding message's header: same
/// body, reply preview, and rail, no header line.
pub fn layout_message_grouped(msg: &MessageView, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    layout(msg, width, theme, false)
}

/// Whether `next` groups under `prev`'s header: same sender, same direction,
/// sent within [`GROUP_WINDOW_SECS`] of it. Messages arrive oldest-first, so a
/// negative delta (server-side clock skew) never groups.
pub fn groups_with(prev: &MessageView, next: &MessageView) -> bool {
    prev.sender == next.sender
        && prev.is_outgoing == next.is_outgoing
        && (0..=GROUP_WINDOW_SECS).contains(&next.date.saturating_sub(prev.date))
}

fn layout(msg: &MessageView, width: u16, theme: &Theme, with_header: bool) -> Vec<Line<'static>> {
    // Every body line reserves the rail column plus its separating space, so
    // the text block is `width - 2` wide. `wrap_spans` treats 0 as 1, but the
    // padding arithmetic below reads more clearly with the floor applied here.
    let inner = width.saturating_sub(RAIL_COLS).max(1);
    let own = msg.is_outgoing;
    let rail_style = if own {
        Style::new().fg(theme.rail_own)
    } else {
        Style::new().fg(theme.sender_color(msg.sender.color_seed()))
    };

    let mut lines: Vec<Line<'static>> = Vec::new();

    if with_header {
        for line in wrap_paragraphs(header_spans(msg, theme), inner) {
            lines.push(place(line, inner, own, None));
        }
    }

    if let Some(reply) = &msg.reply_to {
        let excerpt = if reply.excerpt.is_empty() {
            "…".to_string()
        } else {
            reply.excerpt.replace('\n', " ")
        };
        let spans = vec![Span::styled(
            format!("↳ {excerpt}"),
            Style::new().fg(theme.text_muted),
        )];
        // Pre-truncated upstream to a single line; wrapping and keeping the
        // first line enforces that at this width too.
        if let Some(line) = wrap_spans(spans, inner).into_iter().next() {
            lines.push(place(line, inner, own, Some(rail_style)));
        }
    }

    for line in body(msg, inner, theme) {
        lines.push(place(line, inner, own, Some(rail_style)));
    }

    lines
}

/// "Sender · HH:MM" plus a dim " (edited)" marker. The sender takes its
/// deterministic accent color; separator and time are muted.
///
/// A grouped message has no header, so it shows no edited marker either — a
/// consequence of grouping rather than an oversight.
fn header_spans(msg: &MessageView, theme: &Theme) -> Vec<Span<'static>> {
    let mut spans = vec![
        Span::styled(
            msg.sender_name.clone(),
            Style::new().fg(theme.sender_color(msg.sender.color_seed())),
        ),
        Span::styled(" · ", Style::new().fg(theme.text_muted)),
        Span::styled(format_time_utc(msg.date), Style::new().fg(theme.text_muted)),
    ];
    if msg.is_edited {
        spans.push(Span::styled(" (edited)", Style::new().fg(theme.text_muted)));
    }
    spans
}

/// Unix seconds -> `HH:MM` in **UTC** (see the module docs on time zones). A
/// timestamp outside jiff's representable range renders `--:--` rather than
/// panicking.
fn format_time_utc(unix_secs: i64) -> String {
    match jiff::Timestamp::from_second(unix_secs) {
        Ok(ts) => ts.strftime("%H:%M").to_string(),
        Err(_) => "--:--".to_string(),
    }
}

/// Content-dependent body lines, wrapped to `inner` columns but not yet
/// railed or aligned.
fn body(msg: &MessageView, inner: u16, theme: &Theme) -> Vec<Line<'static>> {
    let base = Style::new().fg(theme.text);

    match &msg.content {
        MessageContent::Text(text) => text_lines(text, base, inner, theme),
        MessageContent::Photo {
            width,
            height,
            caption,
            ..
        } => {
            // A photo has no file name or size in the model; its dimensions
            // are the useful identifier until T38 renders it inline.
            let mut lines = file_card(&format!("photo {width}×{height}"), None, inner, theme);
            lines.extend(text_lines(caption, base, inner, theme));
            lines
        }
        MessageContent::Video {
            file_name,
            size,
            caption,
            ..
        } => {
            let mut lines = file_card(file_name, Some(*size), inner, theme);
            lines.extend(text_lines(caption, base, inner, theme));
            lines
        }
        MessageContent::Audio {
            file_name, size, ..
        } => file_card(file_name, Some(*size), inner, theme),
        MessageContent::Document {
            file_name,
            size,
            caption,
            ..
        } => {
            let mut lines = file_card(file_name, Some(*size), inner, theme);
            lines.extend(text_lines(caption, base, inner, theme));
            lines
        }
        MessageContent::Sticker { emoji } => {
            wrap_paragraphs(vec![Span::styled(emoji.clone(), base)], inner)
        }
        MessageContent::Unsupported { description } => wrap_paragraphs(
            vec![Span::styled(
                format!("[unsupported: {description}]"),
                Style::new().fg(theme.text_muted),
            )],
            inner,
        ),
    }
}

/// A `FormattedText` -> styled, wrapped lines. Empty text yields no lines (an
/// absent caption must not cost a blank row).
fn text_lines(text: &FormattedText, base: Style, inner: u16, theme: &Theme) -> Vec<Line<'static>> {
    if text.text.is_empty() {
        return Vec::new();
    }
    wrap_paragraphs(styled_spans(text, base, theme), inner)
}

/// The placeholder card of spec §7.1:
/// `📎 architecture.pdf · 2.4 MB · ⏎ download`. One line at any sane width;
/// T37 replaces it with download progress and real affordances.
fn file_card(name: &str, size: Option<u64>, inner: u16, theme: &Theme) -> Vec<Line<'static>> {
    let mut label = format!("📎 {name}");
    if let Some(size) = size {
        label.push_str(" · ");
        label.push_str(&format_size(size));
    }
    wrap_paragraphs(
        vec![
            Span::styled(label, Style::new().fg(theme.text)),
            Span::styled(" · ⏎ download", Style::new().fg(theme.text_muted)),
        ],
        inner,
    )
}

/// Binary-prefix size for the file card, one decimal above a kilobyte.
fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let size = bytes as f64;
    if size < KB {
        format!("{bytes} B")
    } else if size < MB {
        format!("{:.1} KB", size / KB)
    } else if size < GB {
        format!("{:.1} MB", size / MB)
    } else {
        format!("{:.1} GB", size / GB)
    }
}

/// Slice `text.text` into styled runs according to its entities.
///
/// Entity offsets convert through `utf16_span_to_byte_range`; an entity whose
/// span is invalid (past the end, mid-surrogate, overflowing) is **skipped**,
/// so its text still renders — unstyled — instead of panicking or vanishing.
///
/// Overlap and nesting resolve by cutting the text at every entity boundary
/// and patching the styles of all entities covering each resulting run, in
/// document order. Non-conflicting attributes therefore merge (bold inside a
/// link is bold *and* underlined) while conflicting ones are last-wins. T33
/// extends this with the kinds needing structural treatment (blockquote, pre
/// with a language label, spoilers).
fn styled_spans(text: &FormattedText, base: Style, theme: &Theme) -> Vec<Span<'static>> {
    let raw = text.text.as_str();

    let mut resolved: Vec<(Range<usize>, Style)> = Vec::new();
    for entity in &text.entities {
        let Some(range) = utf16_span_to_byte_range(raw, entity.offset_utf16, entity.length_utf16)
        else {
            tracing::debug!(
                offset = entity.offset_utf16,
                length = entity.length_utf16,
                "entity span does not resolve to a byte range; rendering that text unstyled"
            );
            continue;
        };
        if range.is_empty() {
            continue;
        }
        let Some(style) = entity_style(&entity.kind, theme) else {
            continue;
        };
        resolved.push((range, style));
    }

    if resolved.is_empty() {
        return vec![Span::styled(raw.to_string(), base)];
    }

    // Cut points: the text's ends plus every entity boundary. Each boundary
    // came out of `utf16_span_to_byte_range`, so all of them sit on a `char`
    // boundary and the slicing below cannot panic.
    let mut cuts = Vec::with_capacity(resolved.len() * 2 + 2);
    cuts.push(0);
    cuts.push(raw.len());
    for (range, _) in &resolved {
        cuts.push(range.start);
        cuts.push(range.end);
    }
    cuts.sort_unstable();
    cuts.dedup();

    let mut spans = Vec::with_capacity(cuts.len().saturating_sub(1));
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let mut style = base;
        for (range, covering) in &resolved {
            if range.start <= start && end <= range.end {
                style = style.patch(*covering);
            }
        }
        spans.push(Span::styled(raw[start..end].to_string(), style));
    }
    spans
}

/// Entity kind -> style, or `None` for the kinds this milestone renders plain.
///
/// T33 extends this match. The arms are exhaustive (no wildcard) so that a new
/// `EntityKind` variant fails to compile until someone decides how it renders.
fn entity_style(kind: &EntityKind, theme: &Theme) -> Option<Style> {
    match kind {
        EntityKind::Bold => Some(Style::new().add_modifier(Modifier::BOLD)),
        EntityKind::Italic => Some(Style::new().add_modifier(Modifier::ITALIC)),
        // A terminal has no second font to switch to; the raised surface is
        // what sets inline code apart from body text.
        EntityKind::Code => Some(Style::new().bg(theme.surface_raised).fg(theme.text)),
        EntityKind::Url | EntityKind::TextUrl { .. } => Some(
            Style::new()
                .fg(theme.accent)
                .add_modifier(Modifier::UNDERLINED),
        ),
        // TODO(T33): underline, strikethrough, spoiler (filled block until
        // revealed), pre with a language label, blockquote, mention, hashtag.
        EntityKind::Underline
        | EntityKind::Strikethrough
        | EntityKind::Spoiler
        | EntityKind::Pre { .. }
        | EntityKind::Blockquote
        | EntityKind::Mention
        | EntityKind::Hashtag => None,
    }
}

/// Wrap spans that may contain `\n`, which `wrap_spans` does not accept: split
/// into paragraphs on newlines, wrap each, and keep blank paragraphs as blank
/// lines so a deliberately spaced-out message stays spaced out.
fn wrap_paragraphs(spans: Vec<Span<'static>>, width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut paragraph: Vec<Span<'static>> = Vec::new();

    for span in spans {
        let style = span.style;
        let content = span.content.into_owned();
        let mut parts = content.split('\n');
        if let Some(first) = parts.next()
            && !first.is_empty()
        {
            paragraph.push(Span::styled(first.to_string(), style));
        }
        for part in parts {
            lines.extend(flush_paragraph(std::mem::take(&mut paragraph), width));
            if !part.is_empty() {
                paragraph.push(Span::styled(part.to_string(), style));
            }
        }
    }
    lines.extend(flush_paragraph(paragraph, width));
    lines
}

/// Wrap one paragraph, mapping "no content" to a single empty line:
/// `wrap_spans` returns nothing for empty input, but a blank line in the
/// middle of a message is content.
fn flush_paragraph(spans: Vec<Span<'static>>, width: u16) -> Vec<Line<'static>> {
    let lines = wrap_spans(spans, width);
    if lines.is_empty() {
        vec![Line::default()]
    } else {
        lines
    }
}

/// Attach the rail and alignment padding to one already-wrapped line.
///
/// - incoming, with rail: `▏` + `" "` + text.
/// - own, with rail: left padding + text + `" "` + `▏`, filling `inner + 2`
///   columns so the block abuts the right edge.
/// - own, no rail (header): left padding + text, ending at the body's right
///   text edge so header and body line up.
/// - incoming, no rail (header): unchanged.
///
/// A line already wider than `inner` takes no padding and simply overflows.
fn place(line: Line<'static>, inner: u16, own: bool, rail_style: Option<Style>) -> Line<'static> {
    let text_width = line.width();
    let mut spans: Vec<Span<'static>> = Vec::new();

    if own {
        let pad = (inner as usize).saturating_sub(text_width);
        if pad > 0 {
            spans.push(Span::raw(" ".repeat(pad)));
        }
        spans.extend(line.spans);
        if let Some(style) = rail_style {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(RAIL, style));
        }
    } else {
        if let Some(style) = rail_style {
            spans.push(Span::styled(RAIL, style));
            spans.push(Span::raw(" "));
        }
        spans.extend(line.spans);
    }

    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use tgt_core::model::entity::TextEntity;
    use tgt_core::model::ids::{ChatId, FileId, MessageId, UserId};
    use tgt_core::model::message::{MessageCaps, ReplyPreview, SendState, Sender};

    use super::*;

    /// 2023-11-14T22:13:20Z — the UTC time every header assertion expects.
    const FIXED_DATE: i64 = 1_700_000_000;

    fn theme() -> Theme {
        Theme::default_dark()
    }

    fn message(content: MessageContent) -> MessageView {
        MessageView {
            id: MessageId(1),
            chat_id: ChatId(2),
            sender: Sender::User(UserId(3)),
            sender_name: "Alice".to_string(),
            is_outgoing: false,
            date: FIXED_DATE,
            content,
            reply_to: None,
            send_state: SendState::Sent,
            reactions: Vec::new(),
            caps: MessageCaps::default(),
            is_edited: false,
        }
    }

    fn text_message(text: &str, entities: Vec<TextEntity>) -> MessageView {
        message(MessageContent::Text(FormattedText {
            text: text.to_string(),
            entities,
        }))
    }

    fn line_text(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn rendered(lines: &[Line<'static>]) -> String {
        lines.iter().map(line_text).collect::<Vec<_>>().join("\n")
    }

    /// The text of every span carrying `modifier`, flattened across lines.
    fn spans_with_modifier(lines: &[Line<'static>], modifier: Modifier) -> Vec<String> {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter(|span| span.style.add_modifier.contains(modifier))
            .map(|span| span.content.to_string())
            .collect()
    }

    /// A railed line with its `"▏ "` prefix removed.
    fn without_rail(line: &Line<'static>) -> String {
        line_text(line).chars().skip(2).collect()
    }

    #[test]
    fn plain_text_wraps_with_rail_prefix() {
        let theme = theme();
        let msg = text_message("hello world from the layout engine", vec![]);
        // Width 20 leaves 18 columns of text.
        let lines = layout_message(&msg, 20, &theme);

        assert_eq!(line_text(&lines[0]), "Alice · 22:13");
        assert!(lines.len() > 2, "expected the body to wrap: {lines:#?}");

        let sender_style = Style::new().fg(theme.sender_color(3));
        for line in &lines[1..] {
            assert_eq!(line.spans[0].content.as_ref(), RAIL);
            assert_eq!(line.spans[0].style, sender_style);
            assert_eq!(line.spans[1].content.as_ref(), " ");
            assert!(
                line.width() <= 20,
                "line overflowed the width: {:?}",
                line_text(line)
            );
        }

        // Soft wrapping dropped the break spaces and nothing else.
        let body = lines[1..]
            .iter()
            .map(without_rail)
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(body, "hello world from the layout engine");
    }

    /// The classic mis-slice regression (spec §8.1 hazard 1): an emoji ahead
    /// of the styled span. UTF-16 units are `🙂` = 0..2, `" "` = 2,
    /// `hello` = 3..8, `" "` = 8, `bold` = 9..13. Treating those offsets as
    /// bytes would style `"ell"` instead.
    #[test]
    fn bold_entity_after_emoji_styles_correct_slice() {
        let msg = text_message(
            "🙂 hello bold world",
            vec![TextEntity {
                offset_utf16: 9,
                length_utf16: 4,
                kind: EntityKind::Bold,
            }],
        );
        let lines = layout_message(&msg, 80, &theme());

        assert_eq!(spans_with_modifier(&lines, Modifier::BOLD), vec!["bold"]);
        assert_eq!(without_rail(&lines[1]), "🙂 hello bold world");
    }

    #[test]
    fn own_message_right_aligned() {
        let theme = theme();
        let mut msg = text_message("yeah, reviewing it now", vec![]);
        msg.is_outgoing = true;
        msg.sender_name = "You".to_string();

        let width = 40;
        let lines = layout_message(&msg, width, &theme);

        // The header ends at the body's right text edge, width - 2.
        assert_eq!(lines[0].width(), width as usize - 2);
        assert!(line_text(&lines[0]).ends_with("You · 22:13"));

        let body = &lines[1];
        assert_eq!(body.width(), width as usize);
        assert_eq!(
            line_text(body),
            format!("{}yeah, reviewing it now ▏", " ".repeat(16))
        );

        let rail = body.spans.last().unwrap();
        assert_eq!(rail.content.as_ref(), RAIL);
        assert_eq!(rail.style, Style::new().fg(theme.rail_own));
    }

    #[test]
    fn invalid_entity_renders_unstyled_not_panic() {
        let msg = text_message(
            "short",
            vec![
                // Offset past the end of the text.
                TextEntity {
                    offset_utf16: 99,
                    length_utf16: 3,
                    kind: EntityKind::Bold,
                },
                // Length running past the end.
                TextEntity {
                    offset_utf16: 0,
                    length_utf16: 99,
                    kind: EntityKind::Italic,
                },
                // offset + length overflows u32.
                TextEntity {
                    offset_utf16: u32::MAX,
                    length_utf16: 1,
                    kind: EntityKind::Code,
                },
            ],
        );
        let lines = layout_message(&msg, 40, &theme());

        assert_eq!(without_rail(&lines[1]), "short");
        assert!(spans_with_modifier(&lines, Modifier::BOLD).is_empty());
        assert!(spans_with_modifier(&lines, Modifier::ITALIC).is_empty());
    }

    /// An entity endpoint inside a surrogate pair (offsets.rs row 8) must take
    /// the same unstyled path.
    #[test]
    fn mid_surrogate_entity_renders_unstyled() {
        let msg = text_message(
            "🙂 hi",
            vec![TextEntity {
                offset_utf16: 1,
                length_utf16: 2,
                kind: EntityKind::Bold,
            }],
        );
        let lines = layout_message(&msg, 40, &theme());

        assert!(spans_with_modifier(&lines, Modifier::BOLD).is_empty());
        assert_eq!(without_rail(&lines[1]), "🙂 hi");
    }

    #[test]
    fn one_column_width_does_not_panic() {
        let msg = text_message(
            "🙂 wide 你好 text",
            vec![TextEntity {
                offset_utf16: 3,
                length_utf16: 4,
                kind: EntityKind::Bold,
            }],
        );
        for width in [0u16, 1, 2, 3] {
            let incoming = layout_message(&msg, width, &theme());
            assert!(!incoming.is_empty(), "width {width} produced nothing");

            let mut own = msg.clone();
            own.is_outgoing = true;
            let own = layout_message(&own, width, &theme());
            assert!(!own.is_empty(), "width {width} produced nothing (outgoing)");
        }
    }

    #[test]
    fn message_of_single_emoji_lays_out() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let msg = text_message(family, vec![]);
        let lines = layout_message(&msg, 40, &theme());

        assert_eq!(lines.len(), 2);
        assert_eq!(without_rail(&lines[1]), family);
    }

    #[test]
    fn grouped_variant_omits_header() {
        let theme = theme();
        let msg = text_message("second in the block", vec![]);

        let with_header = layout_message(&msg, 40, &theme);
        let grouped = layout_message_grouped(&msg, 40, &theme);

        assert_eq!(with_header.len(), grouped.len() + 1);
        assert_eq!(rendered(&grouped), "▏ second in the block");
        assert_eq!(with_header[1..], grouped[..]);
    }

    #[test]
    fn groups_with_same_sender_inside_the_window() {
        let first = text_message("one", vec![]);

        let mut inside = first.clone();
        inside.date = FIXED_DATE + GROUP_WINDOW_SECS;
        assert!(groups_with(&first, &inside));

        let mut outside = first.clone();
        outside.date = FIXED_DATE + GROUP_WINDOW_SECS + 1;
        assert!(!groups_with(&first, &outside));

        let mut other_sender = inside.clone();
        other_sender.sender = Sender::User(UserId(99));
        assert!(!groups_with(&first, &other_sender));

        let mut other_direction = inside.clone();
        other_direction.is_outgoing = true;
        assert!(!groups_with(&first, &other_direction));
    }

    #[test]
    fn reply_preview_renders_a_dim_arrow_line_above_the_body() {
        let theme = theme();
        let mut msg = text_message("take your time 🙏", vec![]);
        msg.reply_to = Some(ReplyPreview {
            message_id: MessageId(0),
            sender_name: "You".to_string(),
            excerpt: "yeah, reviewing it now".to_string(),
        });

        let lines = layout_message(&msg, 40, &theme);

        assert_eq!(
            rendered(&lines[1..]),
            "▏ ↳ yeah, reviewing it now\n▏ take your time 🙏"
        );
        assert_eq!(lines[1].spans[2].style, Style::new().fg(theme.text_muted));
    }

    #[test]
    fn empty_reply_excerpt_renders_an_ellipsis() {
        let mut msg = text_message("body", vec![]);
        msg.reply_to = Some(ReplyPreview {
            message_id: MessageId(0),
            sender_name: "You".to_string(),
            excerpt: String::new(),
        });

        let lines = layout_message(&msg, 40, &theme());
        assert_eq!(line_text(&lines[1]), "▏ ↳ …");
    }

    #[test]
    fn document_renders_a_file_card_line() {
        let msg = message(MessageContent::Document {
            file_id: FileId(7),
            file_name: "architecture.pdf".to_string(),
            size: 2_516_582,
            caption: FormattedText {
                text: String::new(),
                entities: Vec::new(),
            },
        });
        let lines = layout_message(&msg, 60, &theme());

        assert_eq!(lines.len(), 2);
        assert_eq!(
            line_text(&lines[1]),
            "▏ 📎 architecture.pdf · 2.4 MB · ⏎ download"
        );
    }

    #[test]
    fn document_caption_follows_the_card() {
        let msg = message(MessageContent::Document {
            file_id: FileId(7),
            file_name: "notes.txt".to_string(),
            size: 512,
            caption: FormattedText {
                text: "have a look".to_string(),
                entities: Vec::new(),
            },
        });
        let lines = layout_message(&msg, 60, &theme());

        assert_eq!(line_text(&lines[1]), "▏ 📎 notes.txt · 512 B · ⏎ download");
        assert_eq!(line_text(&lines[2]), "▏ have a look");
    }

    #[test]
    fn photo_card_shows_dimensions() {
        let msg = message(MessageContent::Photo {
            file_id: FileId(8),
            width: 800,
            height: 600,
            caption: FormattedText {
                text: String::new(),
                entities: Vec::new(),
            },
        });
        let lines = layout_message(&msg, 60, &theme());
        assert_eq!(line_text(&lines[1]), "▏ 📎 photo 800×600 · ⏎ download");
    }

    #[test]
    fn sticker_and_unsupported_content() {
        let theme = theme();

        let sticker = message(MessageContent::Sticker {
            emoji: "🙏".to_string(),
        });
        assert_eq!(line_text(&layout_message(&sticker, 40, &theme)[1]), "▏ 🙏");

        let unsupported = message(MessageContent::Unsupported {
            description: "poll".to_string(),
        });
        let lines = layout_message(&unsupported, 40, &theme);
        assert_eq!(line_text(&lines[1]), "▏ [unsupported: poll]");
        assert_eq!(lines[1].spans[2].style, Style::new().fg(theme.text_muted));
    }

    #[test]
    fn url_entity_is_underlined_accent() {
        let theme = theme();
        let msg = text_message(
            "see https://example.com ok",
            vec![TextEntity {
                offset_utf16: 4,
                length_utf16: 19,
                kind: EntityKind::Url,
            }],
        );
        let lines = layout_message(&msg, 80, &theme);

        let styled = lines[1]
            .spans
            .iter()
            .find(|span| span.style.add_modifier.contains(Modifier::UNDERLINED))
            .expect("no underlined span");
        assert_eq!(styled.content.as_ref(), "https://example.com");
        assert_eq!(styled.style.fg, Some(theme.accent));
    }

    #[test]
    fn code_entity_gets_the_raised_surface() {
        let theme = theme();
        let msg = text_message(
            "run cargo test now",
            vec![TextEntity {
                offset_utf16: 4,
                length_utf16: 10,
                kind: EntityKind::Code,
            }],
        );
        let lines = layout_message(&msg, 80, &theme);

        let styled = lines[1]
            .spans
            .iter()
            .find(|span| span.style.bg == Some(theme.surface_raised))
            .expect("no code-styled span");
        assert_eq!(styled.content.as_ref(), "cargo test");
    }

    #[test]
    fn nested_entities_merge_their_attributes() {
        // A link inside a bold run: the overlap must come out bold *and*
        // underlined rather than one winning outright.
        let msg = text_message(
            "click here now",
            vec![
                TextEntity {
                    offset_utf16: 6,
                    length_utf16: 4,
                    kind: EntityKind::TextUrl {
                        url: "https://example.com".to_string(),
                    },
                },
                TextEntity {
                    offset_utf16: 0,
                    length_utf16: 14,
                    kind: EntityKind::Bold,
                },
            ],
        );
        let lines = layout_message(&msg, 80, &theme());

        let merged = lines[1]
            .spans
            .iter()
            .find(|span| span.content.as_ref() == "here")
            .expect("no span for the link text");
        assert!(merged.style.add_modifier.contains(Modifier::BOLD));
        assert!(merged.style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn unstyled_entity_kinds_render_as_plain_text() {
        // T33's kinds must not swallow their own text this milestone.
        let msg = text_message(
            "hidden spoiler text",
            vec![TextEntity {
                offset_utf16: 7,
                length_utf16: 7,
                kind: EntityKind::Spoiler,
            }],
        );
        let lines = layout_message(&msg, 80, &theme());
        assert_eq!(without_rail(&lines[1]), "hidden spoiler text");
    }

    #[test]
    fn newlines_become_separate_lines() {
        let msg = text_message("first\n\nthird", vec![]);
        let lines = layout_message(&msg, 40, &theme());

        assert_eq!(rendered(&lines[1..]), "▏ first\n▏ \n▏ third");
    }

    #[test]
    fn edited_marker_appended_to_header() {
        let mut msg = text_message("typo fixed", vec![]);
        msg.is_edited = true;
        let lines = layout_message(&msg, 40, &theme());

        assert_eq!(line_text(&lines[0]), "Alice · 22:13 (edited)");
    }

    #[test]
    fn header_time_is_utc_not_machine_local() {
        // 2023-11-14T22:13:20Z. If this ever starts reading $TZ, it breaks here.
        let msg = text_message("x", vec![]);
        assert_eq!(
            line_text(&layout_message(&msg, 40, &theme())[0]),
            "Alice · 22:13"
        );
    }

    #[test]
    fn out_of_range_timestamp_does_not_panic() {
        let mut msg = text_message("x", vec![]);
        msg.date = i64::MIN;
        assert_eq!(
            line_text(&layout_message(&msg, 40, &theme())[0]),
            "Alice · --:--"
        );
    }

    #[test]
    fn layout_is_deterministic() {
        let msg = text_message(
            "🙂 deterministic output every time",
            vec![TextEntity {
                offset_utf16: 3,
                length_utf16: 13,
                kind: EntityKind::Bold,
            }],
        );
        let theme = theme();
        assert_eq!(
            layout_message(&msg, 30, &theme),
            layout_message(&msg, 30, &theme)
        );
    }

    #[test]
    fn wrapped_lines_stay_inside_the_width() {
        let text = "a much longer message that has to wrap across several lines 你好 🙂";
        for outgoing in [false, true] {
            let mut msg = text_message(text, vec![]);
            msg.is_outgoing = outgoing;
            for width in [8u16, 21, 40, 120] {
                for line in layout_message(&msg, width, &theme()) {
                    assert!(
                        line.width() <= width as usize,
                        "outgoing={outgoing} width={width}: {:?} is {} columns",
                        line_text(&line),
                        line.width()
                    );
                }
            }
        }
    }

    #[test]
    fn size_formatting() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(2_516_582), "2.4 MB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
