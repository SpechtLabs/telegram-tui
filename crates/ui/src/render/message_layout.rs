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
//! ## Rails and alignment (design-language §3)
//!
//! Incoming messages carry the rail `▏` in the sender's deterministic accent
//! color as a left prefix on **every** line of the block — header, reply
//! quote, body, and every wrapped continuation — followed by one space, so
//! the text occupies `width - 2` columns. Own (outgoing) messages are right
//! aligned with a dim `rail_own` rail on the *right*: each line is
//! left-padded so that `pad + text + " " + "▏"` fills exactly `width`
//! columns. The rail is therefore an unbroken vertical bar down one side of
//! the whole group, which is what tells one block from the next now that
//! nothing is boxed.
//!
//! Below three columns there is no room for a rail, its space, and a column
//! of text; such widths still lay out without panicking, but the lines can
//! exceed the requested width. So can a single grapheme wider than the inner
//! width (`wrap_spans` gives it a line of its own) — there is no narrower way
//! to render either.
//!
//! ## The receipt gutter
//!
//! A read receipt renders *inline*, appended to the last line of an own
//! message (design-language §3: it never gets a row of its own and never
//! forms a column of ticks at the pane edge). The marker is live state that
//! `LayoutKey` does not cover, so the view appends it per frame with
//! [`append_marker_inline`] — but the room it needs has to exist in the
//! cached line already, or the row would grow past the pane.
//!
//! Hence [`RECEIPT_COLS`]: an own message wraps its text three columns
//! narrower than an incoming one (one separating space plus the widest
//! marker, `✓✓`), while [`place`] still right-aligns to the full inner
//! width. The reserved columns show up as padding on the left of the row
//! until the view spends them on the marker, so nothing about the alignment
//! changes when a receipt does or doesn't apply.
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
//! ## Full entity set (T33)
//!
//! Every [`EntityKind`] now renders (spec §8.1): bold, italic, underline,
//! strikethrough, code, url/text_url, mention, hashtag resolve through
//! [`entity_style`] and the inline cut-point mechanism in [`styled_spans`]
//! unchanged from T20. Two kinds need more than a `Style`:
//!
//! - **Spoiler** is inline but text-mutating: [`styled_spans`] takes a
//!   `spoilers_revealed` flag (see "Reveal state" below) and, for any run
//!   covered by a `Spoiler` entity while hidden, substitutes the rendered
//!   text with `'█'` (U+2588 FULL BLOCK) — one block per *display column* of
//!   the original grapheme (via `unicode-width`), so wrapping and alignment
//!   downstream never see a length mismatch. Revealed spoilers keep their
//!   real text and get `theme.surface_raised` as a background instead.
//! - **Pre** and **Blockquote** are block-level: a code block needs a
//!   language-label rule above and below it, a blockquote needs a prefix
//!   glyph on every wrapped line, and both need their content re-wrapped at
//!   a *narrower* width than the surrounding paragraph (their indent eats
//!   into `inner`). That does not fit the inline cut-point model (which
//!   produces spans, not extra lines/indentation), so [`text_lines`] first
//!   calls [`split_blocks`] to slice the message's `FormattedText` into an
//!   ordered run of plain-text and block segments — each block segment gets
//!   its own re-based `FormattedText` (entity offsets shifted to be relative
//!   to the slice) so nested inline formatting inside a quote or code block
//!   still resolves through the normal [`styled_spans`] path. Plain segments
//!   feed the pre-existing paragraph wrapper unchanged. This keeps the
//!   function pure (no state, no I/O) and keeps the inline mechanism free of
//!   block-layout concerns. A `Pre`/`Blockquote` nested inside another block
//!   is not split further (real Telegram entities do not nest that way); if
//!   one somehow slips through un-split, [`entity_style`] still gives it a
//!   reasonable inline fallback rather than rendering unstyled.
//!
//! ## Reveal state and the opts API
//!
//! Spoiler reveal is tracked per-*message* (`ConversationState::revealed_spoilers:
//! BTreeSet<MessageId>`, keyed into the layout cache as
//! `LayoutKey::spoilers_revealed`), not per-span — a message's spoilers are
//! all hidden or all shown together. But the architecture §4.9 signature
//! (`layout_message(msg, width, theme)`) has no way to receive that bit.
//! [`layout_message_opts`] is the real entry point now:
//!
//! ```text
//! layout_message_opts(msg, width, theme, LayoutOptions { grouped, spoilers_revealed })
//! ```
//!
//! [`layout_message`] and [`layout_message_grouped`] remain and keep their
//! exact prior behavior (`spoilers_revealed: false`) as thin wrappers, so
//! every existing call site keeps compiling and rendering unchanged. The
//! conversation view (T23/T35) is expected to switch its cache-filling call
//! to `layout_message_opts` with the real `revealed_spoilers` bit in its own
//! task; until then messages render with spoilers hidden regardless of
//! `ConversationState`, matching today's (pre-T33) on-screen behavior.
//!
//! ## Attachments are one line, rendered per frame (T62)
//!
//! [`layout_message`]'s output is cached (`render::cache`) keyed on
//! `(message_id, width, theme_generation, spoilers_revealed)`. Download and
//! upload progress change none of those, so a card that baked "34%" into the
//! cached lines would freeze at whatever percentage was on screen the first
//! time that message got laid out.
//!
//! T37 solved that by splitting the card in two: a cached identity line plus
//! a per-frame status line. Both named the file, so a photo rendered as two
//! rows saying nearly the same thing, and users read it as a bug.
//! design-language §4 settles it — **one line per attachment, never two** —
//! and since the live affordance is the part no cache key covers, that one
//! line is the per-frame one.
//!
//! So [`body`] contributes **no** file-identity line at all: a file-bearing
//! message's cached lines are its header, its reply quote, and its caption,
//! nothing else. [`file_card_line`] and [`file_card_upload_line`] are the
//! single source for the attachment row — pure functions of `(content, live
//! file/upload state, theme)` rendering icon, name, size or dimensions, and
//! the `⏎ download` / `⏎ open` / progress-bar affordance fresh from whatever
//! `MediaState` holds right now. Neither is called from this module or from
//! the cache-filling path; `view::conversation` calls one of them once per
//! frame and rails the result with [`place_row`].
//!
//! A downloaded photo's line is where an inline image later replaces the
//! text outright (design-language §6). That seam lives in
//! `view::conversation`, because an `ImageArea` has to outlive the frame
//! that draws it and nothing in this pure module can hold one.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::ops::Range;
use tgt_core::model::entity::{EntityKind, FormattedText, TextEntity};
use tgt_core::model::message::{FileSnapshot, MessageContent, MessageView};
use tgt_core::state::media::UploadProgress;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::render::offsets::utf16_span_to_byte_range;
use crate::render::wrap::wrap_spans;
use crate::theme::Theme;

/// Grouping decision made by the caller (conversation view): consecutive
/// same-sender messages within this window share one header line.
pub const GROUP_WINDOW_SECS: i64 = 300;

/// The accent rail (U+258F LEFT ONE EIGHTH BLOCK), design-language §3.
const RAIL: &str = "▏";

/// Columns the rail and its adjoining space take on every line of a block.
///
/// Public for the same reason [`RECEIPT_COLS`] is: the conversation view
/// draws things this module doesn't lay out — chiefly an inline image
/// (design-language §6), which has to start exactly where a block's text
/// starts or it reads as belonging to no message at all.
pub const RAIL_COLS: u16 = 2;

/// Columns an own message keeps free at the end of every line so the view can
/// append a read receipt inline: one separating space plus the widest marker
/// (`✓✓`). See the module docs' "The receipt gutter".
pub const RECEIPT_COLS: u16 = 3;

/// Per-call rendering choices [`layout_message_opts`] cannot infer from
/// `(msg, width, theme)` alone. See the module docs' "Reveal state and the
/// opts API".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayoutOptions {
    /// Omit the "Sender · HH:MM" header (this message groups under the
    /// preceding one). Equivalent to choosing [`layout_message_grouped`]
    /// over [`layout_message`].
    pub grouped: bool,
    /// Render this message's spoiler entities as real text (with a subtle
    /// background) instead of filled blocks.
    pub spoilers_revealed: bool,
}

/// Lay out a message including its "Sender · HH:MM" header.
///
/// This is the architecture §4.9 signature, verbatim: spoilers always render
/// hidden. Use [`layout_message_opts`] to control reveal state, or
/// [`layout_message_grouped`] for a message continuing the block above it.
pub fn layout_message(msg: &MessageView, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    layout_message_opts(msg, width, theme, LayoutOptions::default())
}

/// Lay out a message that groups under the preceding message's header: same
/// body, reply preview, and rail, no header line. Spoilers always render
/// hidden; use [`layout_message_opts`] to control reveal state.
pub fn layout_message_grouped(msg: &MessageView, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    layout_message_opts(
        msg,
        width,
        theme,
        LayoutOptions {
            grouped: true,
            ..LayoutOptions::default()
        },
    )
}

/// Lay out a message with full control over grouping and spoiler reveal
/// state. The real entry point behind [`layout_message`] and
/// [`layout_message_grouped`] — see the module docs.
pub fn layout_message_opts(
    msg: &MessageView,
    width: u16,
    theme: &Theme,
    opts: LayoutOptions,
) -> Vec<Line<'static>> {
    layout(msg, width, theme, !opts.grouped, opts.spoilers_revealed)
}

/// Whether `next` groups under `prev`'s header: same sender, same direction,
/// sent within [`GROUP_WINDOW_SECS`] of it. Messages arrive oldest-first, so a
/// negative delta (server-side clock skew) never groups.
pub fn groups_with(prev: &MessageView, next: &MessageView) -> bool {
    prev.sender == next.sender
        && prev.is_outgoing == next.is_outgoing
        && (0..=GROUP_WINDOW_SECS).contains(&next.date.saturating_sub(prev.date))
}

fn layout(
    msg: &MessageView,
    width: u16,
    theme: &Theme,
    with_header: bool,
    spoilers_revealed: bool,
) -> Vec<Line<'static>> {
    // Every line reserves the rail column plus its separating space, so the
    // text block is `width - 2` wide. `wrap_spans` treats 0 as 1, but the
    // padding arithmetic below reads more clearly with the floor applied here.
    let inner = width.saturating_sub(RAIL_COLS).max(1);
    let own = msg.is_outgoing;
    // Own lines wrap narrower still, leaving the view room to append a
    // receipt inline (module docs: "The receipt gutter"). Alignment is
    // unaffected — `place` right-aligns to `inner` either way.
    let text_width = if own {
        inner.saturating_sub(RECEIPT_COLS).max(1)
    } else {
        inner
    };
    let rail = rail_style(msg, theme);

    let mut lines: Vec<Line<'static>> = Vec::new();

    if with_header {
        for line in wrap_paragraphs(header_spans(msg, theme), text_width) {
            lines.push(place(line, inner, own, Some(rail)));
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
        if let Some(line) = wrap_spans(spans, text_width).into_iter().next() {
            lines.push(place(line, inner, own, Some(rail)));
        }
    }

    for line in body(msg, text_width, theme, spoilers_revealed) {
        lines.push(place(line, inner, own, Some(rail)));
    }

    lines
}

/// The rail color for `msg`'s block: the sender's deterministic hue for an
/// incoming message, the dim `rail_own` for an own one (design-language §3:
/// an own rail is never brighter than the body it borders).
///
/// Public because the view draws per-frame rows — the attachment line, the
/// reaction row — that have to carry the same rail as the cached lines they
/// sit under, or the bar breaks halfway down a block.
pub fn rail_style(msg: &MessageView, theme: &Theme) -> Style {
    if msg.is_outgoing {
        Style::new().fg(theme.rail_own)
    } else {
        Style::new().fg(theme.sender_color(msg.sender.color_seed()))
    }
}

/// Rail and align one row of per-frame content (the attachment line, a
/// reaction row) exactly as [`layout_message`] does its cached lines, so a
/// block's rail is one unbroken bar regardless of which half of the
/// static/dynamic split drew any given row.
///
/// `width` is the pane width, not the inner text width — the same number the
/// cached layout was built at.
pub fn place_row(content: Vec<Span<'static>>, width: u16, own: bool, rail: Style) -> Line<'static> {
    let inner = width.saturating_sub(RAIL_COLS).max(1);
    place(Line::from(content), inner, own, Some(rail))
}

/// Append `marker` to the end of `line`'s text, inside `width` — never past
/// it, and never as a row of its own (design-language §3: receipts are
/// inline; a column of ticks at the pane edge is the thing this replaces).
///
/// On an own (right-railed) row the marker goes *before* the trailing rail,
/// so the rail keeps the last column, and the space it needs comes out of
/// the row's left padding — the gutter [`RECEIPT_COLS`] reserved when the
/// text was wrapped. On any other row the marker is simply appended.
///
/// The row therefore keeps its exact width. The one case that cannot be
/// honored is a line already at or past `width` (a single grapheme wider
/// than the pane, which `wrap_spans` cannot break); the marker is appended
/// anyway there and the pane clips it, which is still preferable to giving
/// it a row.
pub fn append_marker_inline(line: &mut Line<'static>, marker: Span<'static>, width: u16) {
    let mut spans = std::mem::take(&mut line.spans);

    // An own row ends with `" "` + rail; the marker belongs ahead of that pair.
    let rail_suffix = if spans.len() >= 2 && spans[spans.len() - 1].content.as_ref() == RAIL {
        Some(spans.split_off(spans.len() - 2))
    } else {
        None
    };

    let mut out: Vec<Span<'static>> = Vec::with_capacity(spans.len() + 4);
    if let Some(rail) = rail_suffix {
        // `place` emits right-alignment padding as one leading unstyled span
        // of spaces. Drop it and recompute: the marker is paid for out of
        // that padding rather than by growing the row.
        if spans.first().is_some_and(is_padding_span) {
            spans.remove(0);
        }
        let content_cols: u16 = spans.iter().map(|s| s.content.width() as u16).sum();
        let marker_cols = marker.content.width() as u16;
        let pad = width.saturating_sub(content_cols + marker_cols + 1 + RAIL_COLS);
        if pad > 0 {
            out.push(Span::raw(" ".repeat(pad as usize)));
        }
        out.extend(spans);
        out.push(Span::raw(" "));
        out.push(marker);
        out.extend(rail);
    } else {
        out.extend(spans);
        out.push(Span::raw(" "));
        out.push(marker);
    }

    line.spans = out;
}

/// Whether `span` is alignment padding [`place`] inserted: unstyled, and
/// nothing but spaces. A blockquote's `▎ ` prefix is styled and so never
/// matches; a `pre` block's unstyled indent only ever follows the padding
/// span on an own row, since the reserved gutter guarantees one.
fn is_padding_span(span: &Span<'static>) -> bool {
    span.style == Style::default()
        && !span.content.is_empty()
        && span.content.chars().all(|c| c == ' ')
}

/// "Sender · HH:MM" plus a dim " (edited)" marker.
///
/// design-language §2's three weights, on one line: the sender is secondary
/// (its deterministic color, **bold**), the separator and the time are
/// tertiary (`text_muted`). A timestamp that reads as loudly as the message
/// under it is what makes a chat pane look like log output, so the bold on
/// the name is doing real work here — it is the contrast that pushes the
/// time back.
///
/// A grouped message has no header, so it shows no edited marker either — a
/// consequence of grouping rather than an oversight.
fn header_spans(msg: &MessageView, theme: &Theme) -> Vec<Span<'static>> {
    let mut spans = vec![
        Span::styled(
            msg.sender_name.clone(),
            Style::new()
                .fg(theme.sender_color(msg.sender.color_seed()))
                .add_modifier(Modifier::BOLD),
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
///
/// File-bearing content contributes **only its caption** here: the
/// attachment's own line is the view's per-frame [`file_card_line`] /
/// [`file_card_upload_line`], never a cached one (module docs:
/// "Attachments are one line, rendered per frame"). An audio message, which
/// has no caption in the model, therefore contributes nothing at all.
fn body(
    msg: &MessageView,
    inner: u16,
    theme: &Theme,
    spoilers_revealed: bool,
) -> Vec<Line<'static>> {
    let base = Style::new().fg(theme.text);

    match &msg.content {
        MessageContent::Text(text) => text_lines(text, base, inner, theme, spoilers_revealed),
        MessageContent::Photo { caption, .. }
        | MessageContent::Video { caption, .. }
        | MessageContent::Document { caption, .. } => {
            text_lines(caption, base, inner, theme, spoilers_revealed)
        }
        MessageContent::Audio { .. } => Vec::new(),
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
///
/// Splits into plain and block ([`EntityKind::Pre`] / [`EntityKind::Blockquote`])
/// segments first (see the module docs); each renders through its own path
/// and the results concatenate in document order.
fn text_lines(
    text: &FormattedText,
    base: Style,
    inner: u16,
    theme: &Theme,
    spoilers_revealed: bool,
) -> Vec<Line<'static>> {
    if text.text.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    for segment in split_blocks(text) {
        match segment.kind {
            SegmentKind::Plain => lines.extend(wrap_paragraphs(
                styled_spans(&segment.text, base, theme, spoilers_revealed),
                inner,
            )),
            SegmentKind::Pre(language) => {
                lines.extend(pre_block_lines(
                    &segment.text,
                    language,
                    inner,
                    theme,
                    spoilers_revealed,
                ));
            }
            SegmentKind::Blockquote => {
                lines.extend(blockquote_lines(
                    &segment.text,
                    inner,
                    theme,
                    spoilers_revealed,
                ));
            }
        }
    }
    lines
}

/// One contiguous run of a message's text: either ordinary inline-formatted
/// text, or the content of a `Pre`/`Blockquote` block (with a re-based
/// `FormattedText` — entity offsets shifted so they read as if the slice
/// were its own message).
struct Segment {
    text: FormattedText,
    kind: SegmentKind,
}

enum SegmentKind {
    Plain,
    Pre(Option<String>),
    Blockquote,
}

/// Slice `text` into an ordered run of [`Segment`]s at every top-level
/// `Pre`/`Blockquote` entity boundary. See the module docs' "Full entity set"
/// section for why this exists instead of folding these kinds into
/// [`styled_spans`].
///
/// A malformed block entity (invalid span, or overlapping a block already
/// claimed) is dropped — its text still renders, as part of whichever plain
/// segment covers it, exactly like any other invalid entity in this module.
/// `Pre`/`Blockquote` entities nested inside another block are not
/// recursively split; their text still renders (as part of the outer
/// block's re-based `FormattedText`), just without their own rule/prefix
/// treatment.
fn split_blocks(text: &FormattedText) -> Vec<Segment> {
    let raw = text.text.as_str();

    let mut blocks: Vec<(Range<usize>, u32, u32, SegmentKind)> = Vec::new();
    for entity in &text.entities {
        let kind = match &entity.kind {
            EntityKind::Pre { language } => SegmentKind::Pre(language.clone()),
            EntityKind::Blockquote => SegmentKind::Blockquote,
            _ => continue,
        };
        let Some(range) = utf16_span_to_byte_range(raw, entity.offset_utf16, entity.length_utf16)
        else {
            continue;
        };
        if range.is_empty() {
            continue;
        }
        blocks.push((range, entity.offset_utf16, entity.length_utf16, kind));
    }
    blocks.sort_by_key(|(range, ..)| range.start);

    let mut segments = Vec::new();
    let mut byte_pos = 0usize;
    let mut utf16_pos = 0u32;

    for (range, utf16_off, utf16_len, kind) in blocks {
        if range.start < byte_pos {
            continue; // overlaps a block already claimed; malformed, drop it.
        }

        // The plain run before this block, minus its one separating
        // newline (if any) so the block's own rule/prefix line supplies the
        // visual break instead of `wrap_paragraphs` adding a spurious blank
        // line for a paragraph that ends in "\n" with nothing after it.
        let mut plain_end = range.start;
        if plain_end > byte_pos && raw.as_bytes()[plain_end - 1] == b'\n' {
            plain_end -= 1;
        }
        if plain_end > byte_pos {
            segments.push(sub_segment(
                text,
                byte_pos,
                plain_end,
                utf16_pos,
                SegmentKind::Plain,
            ));
        }

        segments.push(sub_segment(text, range.start, range.end, utf16_off, kind));

        byte_pos = range.end;
        utf16_pos = utf16_off + utf16_len;
        // Symmetric trim on the way out: skip the newline that separates
        // this block from whatever follows.
        if raw.as_bytes().get(byte_pos) == Some(&b'\n') {
            byte_pos += 1;
            utf16_pos += 1;
        }
    }

    if byte_pos < raw.len() {
        segments.push(sub_segment(
            text,
            byte_pos,
            raw.len(),
            utf16_pos,
            SegmentKind::Plain,
        ));
    }
    if segments.is_empty() {
        segments.push(sub_segment(text, 0, raw.len(), 0, SegmentKind::Plain));
    }
    segments
}

/// Build one [`Segment`] covering `raw[start_byte..end_byte]`, re-basing
/// every entity fully contained in that byte range to be relative to it
/// (`start_utf16` is the UTF-16 offset of `start_byte`, known exactly from
/// the caller's walk rather than re-derived). `Pre`/`Blockquote` entities are
/// never copied into the re-based set — they define segments, they are not
/// inline styling within one (see `split_blocks`'s no-nesting note).
fn sub_segment(
    text: &FormattedText,
    start_byte: usize,
    end_byte: usize,
    start_utf16: u32,
    kind: SegmentKind,
) -> Segment {
    let raw = text.text.as_str();
    let sub_text = raw[start_byte..end_byte].to_string();

    let mut entities = Vec::new();
    for entity in &text.entities {
        if matches!(entity.kind, EntityKind::Pre { .. } | EntityKind::Blockquote) {
            continue;
        }
        let Some(range) = utf16_span_to_byte_range(raw, entity.offset_utf16, entity.length_utf16)
        else {
            continue;
        };
        if range.is_empty() || range.start < start_byte || range.end > end_byte {
            continue;
        }
        entities.push(TextEntity {
            offset_utf16: entity.offset_utf16.saturating_sub(start_utf16),
            length_utf16: entity.length_utf16,
            kind: entity.kind.clone(),
        });
    }

    Segment {
        text: FormattedText {
            text: sub_text,
            entities,
        },
        kind,
    }
}

/// A `pre` code block: a dim top rule carrying the language label (omitted
/// when `language` is `None`), the content in [`EntityKind::Code`] styling
/// wrapped at a narrower width (the block indent eats into `inner`), and a
/// dim bottom rule.
fn pre_block_lines(
    text: &FormattedText,
    language: Option<String>,
    inner: u16,
    theme: &Theme,
    spoilers_revealed: bool,
) -> Vec<Line<'static>> {
    const INDENT: u16 = 2;
    let dim = Style::new().fg(theme.text_muted);
    let code_style = Style::new().bg(theme.surface_raised).fg(theme.text);
    let code_width = inner.saturating_sub(INDENT).max(1);

    let mut lines = Vec::with_capacity(4);
    let rule = match language.filter(|lang| !lang.is_empty()) {
        Some(lang) => format!("── {lang} ──"),
        None => "──".to_string(),
    };
    lines.push(Line::from(vec![Span::styled(rule, dim)]));

    for line in wrap_paragraphs(
        styled_spans(text, code_style, theme, spoilers_revealed),
        code_width,
    ) {
        let mut spans = vec![Span::raw(" ".repeat(INDENT as usize))];
        spans.extend(line.spans);
        lines.push(Line::from(spans));
    }

    lines.push(Line::from(vec![Span::styled("──".to_string(), dim)]));
    lines
}

/// A blockquote: every wrapped line prefixed with a dim `▎ `, content in
/// `text_muted` (nested entities inside the quote still resolve through
/// [`styled_spans`] and merge over that base).
fn blockquote_lines(
    text: &FormattedText,
    inner: u16,
    theme: &Theme,
    spoilers_revealed: bool,
) -> Vec<Line<'static>> {
    const PREFIX: &str = "▎ ";
    let prefix_width = PREFIX.width() as u16;
    let muted = Style::new().fg(theme.text_muted);
    let content_width = inner.saturating_sub(prefix_width).max(1);

    wrap_paragraphs(
        styled_spans(text, muted, theme, spoilers_revealed),
        content_width,
    )
    .into_iter()
    .map(|line| {
        let mut spans = vec![Span::styled(PREFIX, muted)];
        spans.extend(line.spans);
        Line::from(spans)
    })
    .collect()
}

/// Binary-prefix size for the attachment line, one decimal above a kilobyte.
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

/// The kind icon, display name, and middle detail of a file-bearing
/// message's one attachment line — `None` for content that has no attachment
/// (`Text`, `Sticker`, `Unsupported`).
///
/// The detail is the humanized size for anything the model gives a size, and
/// a photo's `323×94` dimensions otherwise (design-language §4 shows both
/// forms). Shared by [`file_card_line`] and [`file_card_upload_line`] so the
/// download and upload renderings can never disagree on a message's name.
///
/// Icons: 📎 document, 🖼 photo, 🎞 video, 🎵 audio (spec §7.1 uses 📎 for
/// the one example it shows; the others are this task's addition).
fn file_card_identity(content: &MessageContent) -> Option<(&'static str, String, Option<String>)> {
    match content {
        MessageContent::Text(_)
        | MessageContent::Sticker { .. }
        | MessageContent::Unsupported { .. } => None,
        // A photo has no file name or size in the model; its dimensions are
        // the useful detail until a graphics protocol renders it inline.
        MessageContent::Photo { width, height, .. } => {
            Some(("🖼", "photo".to_string(), Some(format!("{width}×{height}"))))
        }
        MessageContent::Video {
            file_name, size, ..
        } => Some(("🎞", file_name.clone(), Some(format_size(*size)))),
        MessageContent::Audio {
            file_name, size, ..
        } => Some(("🎵", file_name.clone(), Some(format_size(*size)))),
        MessageContent::Document {
            file_name, size, ..
        } => Some(("📎", file_name.clone(), Some(format_size(*size)))),
    }
}

/// The number of filled cells (out of [`PROGRESS_BAR_CELLS`]) and a whole
/// percentage for `done / total`, or `None` when `total == 0` (TDLib has not
/// reported a size yet — the indeterminate case).
fn progress_fraction(done: u64, total: u64) -> Option<(usize, u64)> {
    if total == 0 {
        return None;
    }
    let ratio = (done as f64 / total as f64).clamp(0.0, 1.0);
    let filled = (ratio * PROGRESS_BAR_CELLS as f64).floor() as usize;
    let pct = (ratio * 100.0).round() as u64;
    Some((filled.min(PROGRESS_BAR_CELLS), pct))
}

/// Cells in a progress bar: `▓▓▓░░░░░░░ 34%` at `PROGRESS_BAR_CELLS = 10`.
/// Used for both download and upload bars, so the two always look alike.
const PROGRESS_BAR_CELLS: usize = 10;

/// `▓▓▓░░░░░░░ 34%`, or `…` when `total == 0` (indeterminate: TDLib has not
/// reported a size yet).
fn progress_bar_text(done: u64, total: u64) -> String {
    match progress_fraction(done, total) {
        None => "…".to_string(),
        Some((filled, pct)) => format!(
            "{}{} {pct}%",
            "▓".repeat(filled),
            "░".repeat(PROGRESS_BAR_CELLS - filled)
        ),
    }
}

/// The dim `· ⏎ download` affordance shared by the "no snapshot yet" and
/// "snapshot exists but isn't downloading or complete" cases below.
fn download_affordance_span(theme: &Theme) -> Span<'static> {
    Span::styled("⏎ download", Style::new().fg(theme.text_muted))
}

/// **The** attachment line (design-language §4: one line per attachment,
/// never two), rendered fresh every frame from live `MediaState`. Not called
/// from [`layout_message`] or the cache-filling path — see the module docs'
/// "Attachments are one line, rendered per frame" for why the cached layout
/// contributes nothing here.
///
/// Returns `None` for message content with no attachment (`Text`, `Sticker`,
/// `Unsupported`) — nothing to render.
///
/// `file` reflects [`FileSnapshot`] lookup by the content's `file_id`,
/// performed by the caller (this function has no access to `MediaState`):
///
/// - `None` (no download ever started): `📎 spec.pdf · 2.4 MB · ⏎ download`.
/// - `Some(f)` with `f.is_downloading`: `📎 spec.pdf · 2.4 MB · ▓▓▓░░░░░░░ 34%`
///   (10-cell bar from `downloaded_size / expected_size`; `expected_size ==
///   0` renders the indeterminate `…` in place of the bar and percentage).
/// - `Some(f)` with `f.is_completed`: `📎 spec.pdf · 2.4 MB · ⏎ open`, the
///   affordance in `theme.accent`.
/// - `Some(f)` otherwise (a snapshot exists — e.g. a cancelled download —
///   but is neither downloading nor complete): falls back to the "not
///   downloaded" `⏎ download` rendering.
///
/// A photo's middle detail is its `323×94` dimensions rather than a size,
/// which the model does not carry for photos.
pub fn file_card_line(
    content: &MessageContent,
    file: Option<&FileSnapshot>,
    theme: &Theme,
) -> Option<Line<'static>> {
    let (icon, name, detail) = file_card_identity(content)?;
    let text_style = Style::new().fg(theme.text);
    let muted_style = Style::new().fg(theme.text_muted);

    let mut spans = vec![Span::styled(format!("{icon} {name}"), text_style)];
    if let Some(detail) = detail {
        spans.push(Span::styled(" · ", muted_style));
        spans.push(Span::styled(detail, muted_style));
    }
    spans.push(Span::styled(" · ", muted_style));

    if let Some(file) = file
        && file.is_downloading
    {
        spans.push(Span::styled(
            progress_bar_text(file.downloaded_size, file.expected_size),
            text_style,
        ));
    } else if file.is_some_and(|f| f.is_completed) {
        spans.push(Span::styled("⏎ open", Style::new().fg(theme.accent)));
    } else {
        spans.push(download_affordance_span(theme));
    }

    Some(Line::from(spans))
}

/// The upload-side counterpart to [`file_card_line`], for a pending (own,
/// not-yet-sent) message backed by a `MediaState::uploads` entry: `↑ name ·
/// ▓▓░░░░░░░░ 20%`. Same static/dynamic split and per-frame-recompute
/// contract as [`file_card_line`] — see its docs and the module docs.
///
/// Always shows the `↑` glyph rather than the kind icon: an in-flight
/// upload is a state of its own, not a photo/video/audio/document
/// distinction. Uses the same 10-cell bar as [`file_card_line`] so the two
/// look alike; `progress.total == 0` renders the indeterminate `…`.
///
/// Returns `None` for content with no attachment, like [`file_card_line`].
pub fn file_card_upload_line(
    content: &MessageContent,
    progress: &UploadProgress,
    theme: &Theme,
) -> Option<Line<'static>> {
    let (_icon, name, _detail) = file_card_identity(content)?;
    let text_style = Style::new().fg(theme.text);
    let muted_style = Style::new().fg(theme.text_muted);

    Some(Line::from(vec![
        Span::styled(format!("↑ {name}"), text_style),
        Span::styled(" · ", muted_style),
        Span::styled(
            progress_bar_text(progress.uploaded, progress.total),
            text_style,
        ),
    ]))
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
/// link is bold *and* underlined) while conflicting ones are last-wins.
///
/// `Spoiler` is the one kind that mutates text rather than only styling it:
/// while `!spoilers_revealed`, any run fully covered by a `Spoiler` entity
/// has its text replaced with `'█'` — one block per display column of the
/// original grapheme (grapheme- and width-aware, like `render::wrap`, so a
/// hidden run costs exactly as many columns as the text it hides) — in
/// `theme.text_muted`, overriding whatever else `entity_style` computed for
/// that run (nested formatting inside a hidden spoiler is moot: the text
/// isn't shown). Revealed spoilers take the normal merge path.
fn styled_spans(
    text: &FormattedText,
    base: Style,
    theme: &Theme,
    spoilers_revealed: bool,
) -> Vec<Span<'static>> {
    let raw = text.text.as_str();

    let mut resolved: Vec<(Range<usize>, Style)> = Vec::new();
    let mut hidden_spoiler_ranges: Vec<Range<usize>> = Vec::new();
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
        if matches!(entity.kind, EntityKind::Spoiler) && !spoilers_revealed {
            hidden_spoiler_ranges.push(range.clone());
        }
        resolved.push((range, entity_style(&entity.kind, theme, spoilers_revealed)));
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

        let hidden = hidden_spoiler_ranges
            .iter()
            .any(|r| r.start <= start && end <= r.end);
        if hidden {
            let blocks: String = raw[start..end]
                .graphemes(true)
                .map(|g| "█".repeat(g.width().max(1)))
                .collect();
            spans.push(Span::styled(blocks, Style::new().fg(theme.text_muted)));
            continue;
        }

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

/// Entity kind -> inline style (spec §8.1's full entity set).
///
/// The arms are exhaustive (no wildcard) so a new `EntityKind` variant fails
/// to compile until someone decides how it renders. `Pre` and `Blockquote`
/// are normally handled structurally by `split_blocks` and never reach this
/// function during regular layout (see the module docs); their arms here are
/// a defensive fallback (still styled, never plain) for the case where one
/// slips through un-split. `Spoiler`'s hidden-text substitution is
/// `styled_spans`'s job, not this function's — this only supplies the style
/// for the (visible-either-way) revealed case and the un-substituted base
/// case the caller patches over.
fn entity_style(kind: &EntityKind, theme: &Theme, spoilers_revealed: bool) -> Style {
    match kind {
        EntityKind::Bold => Style::new().add_modifier(Modifier::BOLD),
        EntityKind::Italic => Style::new().add_modifier(Modifier::ITALIC),
        EntityKind::Underline => Style::new().add_modifier(Modifier::UNDERLINED),
        EntityKind::Strikethrough => Style::new().add_modifier(Modifier::CROSSED_OUT),
        // A terminal has no second font to switch to; the raised surface is
        // what sets inline code (and a `pre` block's content) apart from
        // body text.
        EntityKind::Code | EntityKind::Pre { .. } => {
            Style::new().bg(theme.surface_raised).fg(theme.text)
        }
        EntityKind::Url | EntityKind::TextUrl { .. } => Style::new()
            .fg(theme.accent)
            .add_modifier(Modifier::UNDERLINED),
        EntityKind::Mention | EntityKind::Hashtag => Style::new().fg(theme.accent),
        EntityKind::Blockquote => Style::new().fg(theme.text_muted),
        // Hidden spoilers never reach this style (`styled_spans` overrides
        // with the block-substitution path); revealed ones get a subtle
        // background so the (real) text still reads as "was a spoiler".
        EntityKind::Spoiler if spoilers_revealed => {
            Style::new().bg(theme.surface_raised).fg(theme.text)
        }
        EntityKind::Spoiler => Style::new().fg(theme.text_muted),
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
    /// Callers pass `&lines[1..]` when they mean "the body", since the
    /// header's sender name is bold by design (design-language §2).
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

        // The rail runs the whole block, header included (design-language §3).
        assert_eq!(line_text(&lines[0]), "▏ Alice · 22:13");
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

        assert_eq!(
            spans_with_modifier(&lines[1..], Modifier::BOLD),
            vec!["bold"]
        );
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

        // Header and body both end flush against the right rail, so the rail
        // is one unbroken bar down the block.
        assert_eq!(lines[0].width(), width as usize);
        assert!(line_text(&lines[0]).ends_with("You · 22:13 ▏"));

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
        assert!(spans_with_modifier(&lines[1..], Modifier::BOLD).is_empty());
        assert!(spans_with_modifier(&lines[1..], Modifier::ITALIC).is_empty());
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

        assert!(spans_with_modifier(&lines[1..], Modifier::BOLD).is_empty());
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

    // T62: the cached layout carries **no** file-identity line at all. The
    // whole attachment is one per-frame row (`file_card_line`), because the
    // affordance and progress it has to show are live state no `LayoutKey`
    // covers — see the module docs' "Attachments are one line, rendered per
    // frame". Pre-T62 these three tests asserted a cached `📎 name · size`
    // row; they now assert its absence, which is the property that keeps a
    // file from being named twice on screen.

    #[test]
    fn cached_layout_has_no_file_identity_line() {
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

        assert_eq!(lines.len(), 1, "only the header: {:#?}", rendered(&lines));
        assert_eq!(line_text(&lines[0]), "▏ Alice · 22:13");
    }

    #[test]
    fn document_caption_is_the_only_cached_body_line() {
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

        assert_eq!(lines.len(), 2);
        assert_eq!(line_text(&lines[1]), "▏ have a look");
        assert!(
            !rendered(&lines).contains("notes.txt"),
            "the file name belongs to the per-frame row alone: {:#?}",
            rendered(&lines)
        );
    }

    /// A photo's dimensions are its middle detail on the one attachment
    /// line (design-language §4's `🖼 photo · 323×94`), never a cached row.
    #[test]
    fn photo_line_shows_dimensions_as_its_detail() {
        let content = MessageContent::Photo {
            file_id: FileId(8),
            width: 800,
            height: 600,
            caption: FormattedText {
                text: String::new(),
                entities: Vec::new(),
            },
        };
        assert_eq!(
            layout_message(&message(content.clone()), 60, &theme()).len(),
            1
        );

        let line = file_card_line(&content, None, &theme()).expect("photos have an attachment");
        assert_eq!(line_text(&line), "🖼 photo · 800×600 · ⏎ download");
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
    fn styled_entity_kinds_preserve_their_text() {
        // Every T33 entity kind styles its run without mutating it, *except*
        // Spoiler, which deliberately hides its text until revealed — see
        // `spoiler_hidden_until_revealed_key_changes` below for that one.
        for kind in [
            EntityKind::Underline,
            EntityKind::Strikethrough,
            EntityKind::Mention,
            EntityKind::Hashtag,
        ] {
            let msg = text_message(
                "hidden entity text",
                vec![TextEntity {
                    offset_utf16: 7,
                    length_utf16: 6,
                    kind,
                }],
            );
            let lines = layout_message(&msg, 80, &theme());
            assert_eq!(without_rail(&lines[1]), "hidden entity text");
        }
    }

    #[test]
    fn underline_and_strikethrough_apply_their_modifiers() {
        let msg = text_message(
            "under strike plain",
            vec![
                TextEntity {
                    offset_utf16: 0,
                    length_utf16: 5,
                    kind: EntityKind::Underline,
                },
                TextEntity {
                    offset_utf16: 6,
                    length_utf16: 6,
                    kind: EntityKind::Strikethrough,
                },
            ],
        );
        let lines = layout_message(&msg, 80, &theme());
        assert_eq!(
            spans_with_modifier(&lines, Modifier::UNDERLINED),
            vec!["under"]
        );
        assert_eq!(
            spans_with_modifier(&lines, Modifier::CROSSED_OUT),
            vec!["strike"]
        );
    }

    #[test]
    fn mention_and_hashtag_get_the_accent_color() {
        let theme = theme();
        let msg = text_message(
            "ping alice about #topic",
            vec![
                TextEntity {
                    offset_utf16: 5,
                    length_utf16: 5,
                    kind: EntityKind::Mention,
                },
                TextEntity {
                    offset_utf16: 17,
                    length_utf16: 6,
                    kind: EntityKind::Hashtag,
                },
            ],
        );
        let lines = layout_message(&msg, 80, &theme);
        let accented: Vec<&str> = lines[1]
            .spans
            .iter()
            .filter(|s| s.style.fg == Some(theme.accent))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(accented, vec!["alice", "#topic"]);
    }

    /// spec §8.1: "Spoilers render as a filled block until revealed". Hidden
    /// and revealed renderings must differ, the hidden run must be made of
    /// `'█'` alone, and it must cost exactly as many display columns as the
    /// text it hides (grapheme- and width-aware, so a wide character hides
    /// behind two blocks, not one) — this is what keeps wrapping/alignment
    /// sound regardless of reveal state. Reveal is keyed per-message
    /// (`LayoutOptions::spoilers_revealed`), matching `LayoutKey`.
    #[test]
    fn spoiler_hidden_until_revealed_key_changes() {
        let theme = theme();
        let secret = "你好"; // two wide (display-width-2) graphemes.
        let raw = format!("word {secret} more");
        let offset = raw.find(secret).expect("fixture contains the secret") as u32;
        let msg = text_message(
            &raw,
            vec![TextEntity {
                offset_utf16: offset,
                length_utf16: secret.chars().count() as u32,
                kind: EntityKind::Spoiler,
            }],
        );

        let hidden = layout_message_opts(
            &msg,
            80,
            &theme,
            LayoutOptions {
                spoilers_revealed: false,
                ..LayoutOptions::default()
            },
        );
        let revealed = layout_message_opts(
            &msg,
            80,
            &theme,
            LayoutOptions {
                spoilers_revealed: true,
                ..LayoutOptions::default()
            },
        );

        assert_ne!(rendered(&hidden), rendered(&revealed));

        let hidden_span = hidden[1]
            .spans
            .iter()
            .find(|s| !s.content.is_empty() && s.content.chars().all(|c| c == '█'))
            .expect("no block-substituted span in the hidden rendering");
        assert_eq!(
            UnicodeWidthStr::width(hidden_span.content.as_ref()),
            UnicodeWidthStr::width(secret)
        );
        assert_eq!(hidden_span.style, Style::new().fg(theme.text_muted));

        let revealed_span = revealed[1]
            .spans
            .iter()
            .find(|s| s.content.as_ref() == secret)
            .expect("no revealed span carrying the real text");
        assert_eq!(revealed_span.style.bg, Some(theme.surface_raised));
    }

    #[test]
    fn pre_block_shows_language_label() {
        let theme = theme();
        let msg = text_message(
            "fn main() {}",
            vec![TextEntity {
                offset_utf16: 0,
                length_utf16: 12,
                kind: EntityKind::Pre {
                    language: Some("rust".to_string()),
                },
            }],
        );
        let lines = layout_message(&msg, 60, &theme);
        let text = rendered(&lines);

        assert!(text.contains("── rust ──"), "{text}");
        let code_span = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content.as_ref().contains("fn main"))
            .expect("no code-styled span");
        assert_eq!(code_span.style.bg, Some(theme.surface_raised));
    }

    #[test]
    fn pre_block_without_language_omits_the_label() {
        let msg = text_message(
            "echo hi",
            vec![TextEntity {
                offset_utf16: 0,
                length_utf16: 7,
                kind: EntityKind::Pre { language: None },
            }],
        );
        let lines = layout_message(&msg, 60, &theme());
        let text = rendered(&lines);
        assert!(text.contains("──"), "{text}");
        assert!(!text.contains("rust"));
    }

    #[test]
    fn blockquote_prefixes_every_line_with_a_dim_marker() {
        let theme = theme();
        let msg = text_message(
            "a wise quote",
            vec![TextEntity {
                offset_utf16: 0,
                length_utf16: 12,
                kind: EntityKind::Blockquote,
            }],
        );
        let lines = layout_message(&msg, 60, &theme);
        let quote_line = lines
            .iter()
            .find(|l| line_text(l).contains("a wise quote"))
            .expect("no blockquote line");
        assert!(line_text(quote_line).contains("▎"));
        let text_span = quote_line
            .spans
            .iter()
            .find(|s| s.content.as_ref().contains("wise"))
            .expect("no span carrying the quote text");
        assert_eq!(text_span.style.fg, Some(theme.text_muted));
    }

    /// spec §7.1: "a single dimmed line, truncated to one line". A very long
    /// excerpt at a narrow width must still cost exactly one reply-preview
    /// row, entirely in `theme.text_muted`.
    #[test]
    fn reply_quote_single_dimmed_line() {
        let theme = theme();
        let mut msg = text_message("ok", vec![]);
        msg.reply_to = Some(ReplyPreview {
            message_id: MessageId(0),
            sender_name: "You".to_string(),
            excerpt: "a very long excerpt that would wrap across several lines at this width if it were not truncated first".to_string(),
        });

        let lines = layout_message(&msg, 24, &theme);

        // lines[0] header, lines[1] the one reply-preview row, lines[2] the
        // first body row — if truncation failed the excerpt would still be
        // wrapping into lines[2].
        assert_eq!(line_text(&lines[2]), "▏ ok");
        let reply_line = &lines[1];
        assert!(line_text(reply_line).starts_with("▏ ↳ "));
        for span in &reply_line.spans[2..] {
            assert_eq!(span.style, Style::new().fg(theme.text_muted));
        }
    }

    #[test]
    fn nested_bold_italic_compose() {
        // "bold italic" is Bold; "italic" (its tail) is also Italic. The
        // overlap must come out both bold and italic, not one replacing the
        // other.
        let msg = text_message(
            "very bold italic text",
            vec![
                TextEntity {
                    offset_utf16: 5,
                    length_utf16: 11,
                    kind: EntityKind::Bold,
                },
                TextEntity {
                    offset_utf16: 10,
                    length_utf16: 6,
                    kind: EntityKind::Italic,
                },
            ],
        );
        let lines = layout_message(&msg, 80, &theme());

        let overlap = lines[1]
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "italic")
            .expect("no span for the overlap");
        assert!(overlap.style.add_modifier.contains(Modifier::BOLD));
        assert!(overlap.style.add_modifier.contains(Modifier::ITALIC));

        let bold_only = lines[1]
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "bold ")
            .expect("no span for the bold-only run");
        assert!(bold_only.style.add_modifier.contains(Modifier::BOLD));
        assert!(!bold_only.style.add_modifier.contains(Modifier::ITALIC));
    }

    /// Fixture exercising every `EntityKind` at once: the inline kinds on
    /// one line, a `pre` block and a `blockquote` each on their own line.
    fn every_entity_message() -> MessageView {
        let line1 = "bold italic underline strike spoiler code link url mention hashtag";
        let line2 = "fn main() {}";
        let line3 = "a wise quote";
        let raw = format!("{line1}\n{line2}\n{line3}");

        let find = |needle: &str| raw.find(needle).expect(needle) as u32;
        let len = |needle: &str| needle.chars().count() as u32; // ASCII fixture only.

        let entities = vec![
            TextEntity {
                offset_utf16: find("bold"),
                length_utf16: len("bold"),
                kind: EntityKind::Bold,
            },
            TextEntity {
                offset_utf16: find("italic"),
                length_utf16: len("italic"),
                kind: EntityKind::Italic,
            },
            TextEntity {
                offset_utf16: find("underline"),
                length_utf16: len("underline"),
                kind: EntityKind::Underline,
            },
            TextEntity {
                offset_utf16: find("strike"),
                length_utf16: len("strike"),
                kind: EntityKind::Strikethrough,
            },
            TextEntity {
                offset_utf16: find("spoiler"),
                length_utf16: len("spoiler"),
                kind: EntityKind::Spoiler,
            },
            TextEntity {
                offset_utf16: find("code"),
                length_utf16: len("code"),
                kind: EntityKind::Code,
            },
            TextEntity {
                offset_utf16: find("link"),
                length_utf16: len("link"),
                kind: EntityKind::TextUrl {
                    url: "https://example.com".to_string(),
                },
            },
            TextEntity {
                offset_utf16: find("url"),
                length_utf16: len("url"),
                kind: EntityKind::Url,
            },
            TextEntity {
                offset_utf16: find("mention"),
                length_utf16: len("mention"),
                kind: EntityKind::Mention,
            },
            TextEntity {
                offset_utf16: find("hashtag"),
                length_utf16: len("hashtag"),
                kind: EntityKind::Hashtag,
            },
            TextEntity {
                offset_utf16: find(line2),
                length_utf16: len(line2),
                kind: EntityKind::Pre {
                    language: Some("rust".to_string()),
                },
            },
            TextEntity {
                offset_utf16: find(line3),
                length_utf16: len(line3),
                kind: EntityKind::Blockquote,
            },
        ];

        text_message(&raw, entities)
    }

    #[test]
    fn every_entity_kind_snapshot_hidden_spoiler() {
        let lines = layout_message_opts(
            &every_entity_message(),
            60,
            &theme(),
            LayoutOptions {
                spoilers_revealed: false,
                ..LayoutOptions::default()
            },
        );
        insta::assert_snapshot!(rendered(&lines));
    }

    #[test]
    fn every_entity_kind_snapshot_revealed_spoiler() {
        let lines = layout_message_opts(
            &every_entity_message(),
            60,
            &theme(),
            LayoutOptions {
                spoilers_revealed: true,
                ..LayoutOptions::default()
            },
        );
        insta::assert_snapshot!(rendered(&lines));
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

        assert_eq!(line_text(&lines[0]), "▏ Alice · 22:13 (edited)");
    }

    #[test]
    fn header_time_is_utc_not_machine_local() {
        // 2023-11-14T22:13:20Z. If this ever starts reading $TZ, it breaks here.
        let msg = text_message("x", vec![]);
        assert_eq!(
            line_text(&layout_message(&msg, 40, &theme())[0]),
            "▏ Alice · 22:13"
        );
    }

    /// design-language §2: the sender is secondary (its color, bold), the
    /// separator and time tertiary (`text_muted`). Bolding the name is what
    /// pushes the timestamp back into the background.
    #[test]
    fn header_sender_is_bold_and_the_time_is_muted() {
        let theme = theme();
        let lines = layout_message(&text_message("x", vec![]), 40, &theme);
        let header = &lines[0];

        let name = header
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "Alice")
            .expect("no sender span");
        assert_eq!(name.style.fg, Some(theme.sender_color(3)));
        assert!(name.style.add_modifier.contains(Modifier::BOLD));

        for span in header
            .spans
            .iter()
            .filter(|s| matches!(s.content.as_ref(), " · " | "22:13"))
        {
            assert_eq!(span.style, Style::new().fg(theme.text_muted));
        }
    }

    #[test]
    fn out_of_range_timestamp_does_not_panic() {
        let mut msg = text_message("x", vec![]);
        msg.date = i64::MIN;
        assert_eq!(
            line_text(&layout_message(&msg, 40, &theme())[0]),
            "▏ Alice · --:--"
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
        // (input bytes, expected humanized string) — one row per unit
        // boundary, plus the "just under the next unit" cases that are
        // where an off-by-one in the threshold comparisons would show up.
        let cases: &[(u64, &str)] = &[
            (0, "0 B"),
            (1, "1 B"),
            (1023, "1023 B"),
            (1024, "1.0 KB"),
            (1536, "1.5 KB"),
            (1024 * 1024 - 1, "1024.0 KB"),
            (2_516_582, "2.4 MB"),
            (1024 * 1024 * 1024 - 1, "1024.0 MB"),
            (3 * 1024 * 1024 * 1024, "3.0 GB"),
        ];
        for (bytes, expected) in cases {
            assert_eq!(format_size(*bytes), *expected, "format_size({bytes})");
        }
    }

    // --- file_card_line / file_card_upload_line (T37) --------------------

    fn document_content() -> MessageContent {
        MessageContent::Document {
            file_id: FileId(7),
            file_name: "architecture.pdf".to_string(),
            size: 2_516_582,
            caption: FormattedText {
                text: String::new(),
                entities: Vec::new(),
            },
        }
    }

    fn file_snapshot(
        downloaded: u64,
        expected: u64,
        is_downloading: bool,
        is_completed: bool,
    ) -> FileSnapshot {
        FileSnapshot {
            id: FileId(7),
            expected_size: expected,
            downloaded_size: downloaded,
            uploaded_size: 0,
            is_downloading,
            is_completed,
            local_path: None,
        }
    }

    #[test]
    fn file_card_line_undownloaded_document_snapshot() {
        let content = document_content();
        let line = file_card_line(&content, None, &theme()).expect("document has a file card");
        insta::assert_snapshot!(line_text(&line));
    }

    #[test]
    fn file_card_line_forty_percent_download_progress_snapshot() {
        let content = document_content();
        // A round 400/1000 rather than the document's real size, so the
        // expected 40% is exact and the snapshot isn't hostage to
        // floating-point rounding on an arbitrary byte count.
        let file = file_snapshot(400, 1000, true, false);
        let line =
            file_card_line(&content, Some(&file), &theme()).expect("document has a file card");
        insta::assert_snapshot!(line_text(&line));
    }

    #[test]
    fn file_card_line_completed_snapshot() {
        let content = document_content();
        let file = file_snapshot(2_516_582, 2_516_582, false, true);
        let theme = theme();
        let line = file_card_line(&content, Some(&file), &theme).expect("document has a file card");
        insta::assert_snapshot!(line_text(&line));

        let affordance = line
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "⏎ open")
            .expect("no open affordance span");
        assert_eq!(affordance.style.fg, Some(theme.accent));
    }

    /// A snapshot that exists but is neither downloading nor complete (e.g.
    /// a cancelled download) falls back to the "not downloaded" rendering
    /// rather than showing a frozen 0% bar.
    #[test]
    fn file_card_line_cancelled_snapshot_falls_back_to_download_affordance() {
        let content = document_content();
        let file = file_snapshot(0, 2_516_582, false, false);
        let line =
            file_card_line(&content, Some(&file), &theme()).expect("document has a file card");
        assert_eq!(
            line_text(&line),
            "📎 architecture.pdf · 2.4 MB · ⏎ download"
        );
    }

    #[test]
    fn file_card_line_indeterminate_progress_when_expected_size_zero() {
        let content = document_content();
        let file = file_snapshot(0, 0, true, false);
        let line =
            file_card_line(&content, Some(&file), &theme()).expect("document has a file card");
        assert_eq!(line_text(&line), "📎 architecture.pdf · 2.4 MB · …");
    }

    #[test]
    fn file_card_upload_line_pending_snapshot() {
        let content = document_content();
        let progress = UploadProgress {
            chat_id: ChatId(2),
            uploaded: 200,
            total: 1000,
        };
        let line =
            file_card_upload_line(&content, &progress, &theme()).expect("document has a file card");
        insta::assert_snapshot!(line_text(&line));
    }

    #[test]
    fn file_card_upload_line_indeterminate_when_total_zero() {
        let content = document_content();
        let progress = UploadProgress {
            chat_id: ChatId(2),
            uploaded: 0,
            total: 0,
        };
        let line =
            file_card_upload_line(&content, &progress, &theme()).expect("document has a file card");
        assert_eq!(line_text(&line), "↑ architecture.pdf · …");
    }

    #[test]
    fn file_card_line_uses_kind_specific_icons() {
        let theme = theme();
        let video = MessageContent::Video {
            file_id: FileId(1),
            file_name: "clip.mp4".to_string(),
            size: 100,
            duration_secs: 5,
            caption: FormattedText {
                text: String::new(),
                entities: Vec::new(),
            },
        };
        let audio = MessageContent::Audio {
            file_id: FileId(2),
            file_name: "song.mp3".to_string(),
            size: 100,
            duration_secs: 5,
        };
        let photo = MessageContent::Photo {
            file_id: FileId(3),
            width: 10,
            height: 20,
            caption: FormattedText {
                text: String::new(),
                entities: Vec::new(),
            },
        };

        assert!(line_text(&file_card_line(&video, None, &theme).unwrap()).starts_with("🎞 "));
        assert!(line_text(&file_card_line(&audio, None, &theme).unwrap()).starts_with("🎵 "));
        assert!(line_text(&file_card_line(&photo, None, &theme).unwrap()).starts_with("🖼 "));
    }

    // --- inline markers and per-frame rows (T62) --------------------------

    /// The receipt is paid for out of the gutter `RECEIPT_COLS` reserved
    /// when the own message was wrapped, so the row keeps its exact width
    /// and the rail keeps the last column.
    #[test]
    fn inline_marker_spends_the_gutter_and_keeps_the_width() {
        let theme = theme();
        let mut msg = text_message("done", vec![]);
        msg.is_outgoing = true;
        msg.sender_name = "You".to_string();

        for width in [24u16, 40, 80, 140] {
            let mut lines = layout_message(&msg, width, &theme);
            let before = lines.last().unwrap().width();
            append_marker_inline(
                lines.last_mut().unwrap(),
                Span::styled("✓✓", Style::new().fg(theme.text_muted)),
                width,
            );
            let last = lines.last().unwrap();
            assert_eq!(
                last.width(),
                before,
                "width {width}: the marker grew the row"
            );
            assert_eq!(line_text(last).trim_start(), "done ✓✓ ▏");
            assert_eq!(
                last.spans.last().unwrap().content.as_ref(),
                RAIL,
                "the rail keeps the last column"
            );
        }
    }

    /// A left-aligned (incoming) row has no gutter and no rail to preserve,
    /// so the marker simply follows the text.
    #[test]
    fn inline_marker_on_an_incoming_row_appends_at_the_end() {
        let theme = theme();
        let mut lines = layout_message(&text_message("hi", vec![]), 40, &theme);
        append_marker_inline(
            lines.last_mut().unwrap(),
            Span::styled("✗", Style::new().fg(theme.danger)),
            40,
        );
        assert_eq!(line_text(lines.last().unwrap()), "▏ hi ✗");
    }

    /// `place_row` gives a per-frame row the same rail and alignment the
    /// cached lines carry, so a block's rail never breaks mid-way down.
    #[test]
    fn place_row_matches_the_cached_rail_and_alignment() {
        let theme = theme();
        let content = || {
            vec![Span::styled(
                "👍 3".to_string(),
                Style::new().fg(theme.accent),
            )]
        };

        // Any color: `place_row` is being checked for rail placement and
        // alignment, not for which token the caller chose.
        let incoming = place_row(content(), 40, false, Style::new().fg(theme.accent));
        assert_eq!(line_text(&incoming), "▏ 👍 3");

        let own = place_row(content(), 40, true, Style::new().fg(theme.rail_own));
        assert_eq!(own.width(), 40);
        assert!(line_text(&own).ends_with("👍 3 ▏"));
    }

    #[test]
    fn file_card_line_none_for_content_without_a_file() {
        let theme = theme();
        let text = MessageContent::Text(FormattedText {
            text: "hi".to_string(),
            entities: Vec::new(),
        });
        assert!(file_card_line(&text, None, &theme).is_none());
        let progress = UploadProgress {
            chat_id: ChatId(1),
            uploaded: 0,
            total: 0,
        };
        assert!(file_card_upload_line(&text, &progress, &theme).is_none());
    }
}
