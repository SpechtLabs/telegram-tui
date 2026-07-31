//! Grapheme-cluster- and display-width-aware wrapping of styled spans.
//!
//! Wraps a paragraph (a `Vec<Span<'static>>` with no embedded newlines — the
//! caller splits on `\n` before calling) into `Line`s that never exceed
//! `width` display columns and never split a grapheme cluster (an emoji ZWJ
//! sequence, a flag, a base character plus its combining marks) across two
//! lines or two `Span`s. See architecture.md §4.9 and design spec §8.1
//! hazard 2: width is display columns, not `char` count.
//!
//! ## Wrapping rules (the contract this module implements)
//!
//! - Column cost per grapheme cluster comes from `unicode-width`'s
//!   [`UnicodeWidthStr::width`] applied to the whole cluster, not summed
//!   per-`char`; that is what makes combining marks (zero width, folded
//!   into the base character's cluster) and most emoji joiner sequences
//!   report a sane column count instead of the sum of their parts.
//! - `width == 0` is treated as `width == 1`: nothing useful fits in zero
//!   columns, and 1 is the smallest width that still makes progress.
//! - Soft wrap: while a line fills up, the position of the most recent
//!   plain ASCII space (`' '`, U+0020) is remembered. When the next
//!   grapheme would overflow the line, wrapping backs up to that space,
//!   emits everything before it as a finished line, and resumes with
//!   everything after it. The space itself is dropped: it neither ends the
//!   line before it nor starts the line after it. Only the ASCII space is a
//!   soft-wrap point — tabs, non-breaking spaces, and other whitespace are
//!   ordinary content that can only be hard-broken.
//! - Hard wrap: if there is no space to back up to (a single word longer
//!   than `width`), the line fills to capacity and wrapping continues at
//!   the next grapheme boundary — never mid-grapheme.
//! - A grapheme cluster wider than `width` on its own (e.g. a CJK character
//!   at `width == 1`) gets a line entirely to itself, even though that
//!   necessarily overflows `width` — there is no narrower way to render it.
//! - Backing up to a soft-wrap space can leave nothing before it (e.g. a
//!   run of spaces at `width == 1`, or a space immediately following a
//!   forced break). In that case no blank line is emitted for the empty
//!   remainder; the pending whitespace is dropped silently and wrapping
//!   resumes after the space.
//! - Adjacent graphemes that share a `Style` are coalesced into one `Span`
//!   per line rather than one `Span` per grapheme.
//! - Empty input (no spans, or spans with no graphemes) produces no lines.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// One grapheme cluster plus the style inherited from its source span.
struct Cluster {
    text: String,
    style: Style,
    width: usize,
    is_space: bool,
}

/// Wrap styled spans to `width` columns. Grapheme-cluster aware
/// (unicode-segmentation) and display-width aware (unicode-width): emoji, CJK
/// and combining marks never break column alignment. width >= 1 (0 is
/// treated as 1); a grapheme wider than `width` occupies its own line.
pub fn wrap_spans(spans: Vec<Span<'static>>, width: u16) -> Vec<Line<'static>> {
    let width = width.max(1) as usize;
    let clusters = flatten(spans);

    let mut lines = Vec::new();
    let mut current: Vec<Cluster> = Vec::new();
    let mut current_width = 0usize;
    let mut last_space: Option<usize> = None;

    for cluster in clusters {
        // A cluster that can never fit even alone on a line gets a line to
        // itself; flush whatever was pending first.
        if cluster.width > width {
            if !current.is_empty() {
                push_line(&mut lines, std::mem::take(&mut current));
                current_width = 0;
                last_space = None;
            }
            push_line(&mut lines, vec![cluster]);
            continue;
        }

        while current_width + cluster.width > width {
            match last_space {
                Some(pos) => {
                    // Back up to the last space: everything before it (if
                    // any) becomes a finished line, the space is dropped,
                    // and everything after it carries forward.
                    let mut remainder = current.split_off(pos + 1);
                    current.pop(); // the space itself
                    if !current.is_empty() {
                        push_line(&mut lines, std::mem::take(&mut current));
                    }
                    current_width = remainder.iter().map(|c| c.width).sum();
                    last_space = remainder.iter().rposition(|c| c.is_space);
                    current.append(&mut remainder);
                }
                None => {
                    // No space to back up to: hard-break at the grapheme
                    // boundary (the current line is already full).
                    push_line(&mut lines, std::mem::take(&mut current));
                    current_width = 0;
                    last_space = None;
                }
            }
        }

        if cluster.is_space {
            last_space = Some(current.len());
        }
        current_width += cluster.width;
        current.push(cluster);
    }

    if !current.is_empty() {
        push_line(&mut lines, current);
    }

    lines
}

/// Flatten spans into grapheme clusters, keeping each cluster's originating
/// span's style.
fn flatten(spans: Vec<Span<'static>>) -> Vec<Cluster> {
    let mut clusters = Vec::new();
    for span in spans {
        let style = span.style;
        for grapheme in span.content.as_ref().graphemes(true) {
            clusters.push(Cluster {
                text: grapheme.to_string(),
                style,
                width: grapheme.width(),
                is_space: grapheme == " ",
            });
        }
    }
    clusters
}

/// Coalesce a line's clusters into same-style spans and push the line.
fn push_line(lines: &mut Vec<Line<'static>>, clusters: Vec<Cluster>) {
    lines.push(Line::from(coalesce(clusters)));
}

/// Merge adjacent clusters that share a `Style` into single `Span`s.
fn coalesce(clusters: Vec<Cluster>) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut buf_style: Option<Style> = None;

    for cluster in clusters {
        match buf_style {
            Some(style) if style == cluster.style => buf.push_str(&cluster.text),
            Some(style) => {
                spans.push(Span::styled(std::mem::take(&mut buf), style));
                buf.push_str(&cluster.text);
                buf_style = Some(cluster.style);
            }
            None => {
                buf.push_str(&cluster.text);
                buf_style = Some(cluster.style);
            }
        }
    }
    if let Some(style) = buf_style {
        spans.push(Span::styled(buf, style));
    }
    spans
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;

    /// Concatenate every span's text in every line, in order — useful for
    /// asserting "nothing was lost or reordered" invariants.
    fn flatten_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn cjk_wraps_at_display_width() {
        // Six double-width CJK characters, width 10 columns => 5 chars
        // (10 columns) on the first line, 1 char (2 columns) on the second.
        let spans = vec![Span::raw("你好你好你好")];
        let lines = wrap_spans(spans, 10);

        assert_eq!(lines.len(), 2);
        assert_eq!(line_text(&lines[0]), "你好你好你");
        assert_eq!(line_text(&lines[1]), "好");
        assert_eq!(UnicodeWidthStr::width(line_text(&lines[0]).as_str()), 10);
    }

    #[test]
    fn emoji_grapheme_not_split() {
        // Family emoji: MAN ZWJ WOMAN ZWJ GIRL ZWJ BOY — one extended
        // grapheme cluster. Width is generous, so no wrapping happens; if
        // the cluster had been split, the reconstructed text would differ
        // from the input or the cluster would show up split across spans.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
        let text = format!("hi {family} there");
        let spans = vec![Span::raw(text.clone())];
        let lines = wrap_spans(spans, 80);

        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), text);
    }

    #[test]
    fn combining_mark_stays_with_base() {
        // "e" + COMBINING ACUTE ACCENT is a single grapheme cluster.
        let text = "cafe\u{0301}"; // c a f [e + combining acute]
        assert_eq!(text.graphemes(true).count(), 4);

        // Width 3 forces a break; the combining mark must travel with its
        // base character rather than being stranded alone.
        let spans = vec![Span::raw(text)];
        let lines = wrap_spans(spans, 3);

        assert_eq!(lines.len(), 2);
        assert_eq!(line_text(&lines[0]), "caf");
        assert_eq!(line_text(&lines[1]), "e\u{0301}");
    }

    #[test]
    fn width_one_column_yields_one_grapheme_per_line() {
        let spans = vec![Span::raw("abc")];
        let lines = wrap_spans(spans, 1);

        assert_eq!(lines.len(), 3);
        assert_eq!(line_text(&lines[0]), "a");
        assert_eq!(line_text(&lines[1]), "b");
        assert_eq!(line_text(&lines[2]), "c");
    }

    #[test]
    fn zero_width_joiner_family_single_cluster() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
        assert_eq!(family.graphemes(true).count(), 1);

        // Width 1 is narrower than the cluster: it must still render whole,
        // on a line of its own, rather than being split.
        let spans = vec![Span::raw(family)];
        let lines = wrap_spans(spans, 1);

        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), family);
    }

    #[test]
    fn style_preserved_across_break() {
        let red = Style::new().fg(Color::Red);
        let blue = Style::new().fg(Color::Blue);
        let spans = vec![Span::styled("hello ", red), Span::styled("world", blue)];
        let lines = wrap_spans(spans, 5);

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].content.as_ref(), "hello");
        assert_eq!(lines[0].spans[0].style, red);
        assert_eq!(lines[1].spans.len(), 1);
        assert_eq!(lines[1].spans[0].content.as_ref(), "world");
        assert_eq!(lines[1].spans[0].style, blue);
    }

    #[test]
    fn style_survives_within_a_line_by_coalescing_same_style_spans() {
        let style = Style::new().fg(Color::Green);
        // Two spans, same style, no break needed: must coalesce to one span.
        let spans = vec![Span::styled("foo", style), Span::styled("bar", style)];
        let lines = wrap_spans(spans, 80);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].content.as_ref(), "foobar");
    }

    #[test]
    fn spaces_break_preferred_over_mid_word() {
        // Without space preference, a naive width-8 fill would break
        // "defghij" into "defg"/"hij". With space preference, "abc" and
        // "defghij" split cleanly at the space instead.
        let spans = vec![Span::raw("abc defghij")];
        let lines = wrap_spans(spans, 8);

        assert_eq!(lines.len(), 2);
        assert_eq!(line_text(&lines[0]), "abc");
        assert_eq!(line_text(&lines[1]), "defghij");
    }

    #[test]
    fn empty_input_produces_no_lines() {
        assert_eq!(wrap_spans(vec![], 10), Vec::<Line<'static>>::new());
        assert_eq!(
            wrap_spans(vec![Span::raw("")], 10),
            Vec::<Line<'static>>::new()
        );
    }

    #[test]
    fn only_spaces_produce_a_single_line_of_spaces() {
        let spans = vec![Span::raw("   ")];
        let lines = wrap_spans(spans, 10);

        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "   ");
    }

    #[test]
    fn overflowing_run_of_spaces_at_width_one_drops_silently_without_blank_lines() {
        // Backing up to a space that has nothing before it must not emit a
        // blank line; it should just consume the space and move on.
        let spans = vec![Span::raw("o w")];
        let lines = wrap_spans(spans, 1);

        assert_eq!(lines.len(), 2);
        assert_eq!(line_text(&lines[0]), "o");
        assert_eq!(line_text(&lines[1]), "w");
    }

    #[test]
    fn zero_width_is_treated_as_one() {
        let spans = vec![Span::raw("ab")];
        assert_eq!(wrap_spans(spans.clone(), 0), wrap_spans(spans, 1));
    }

    #[test]
    fn no_grapheme_is_ever_split_across_lines() {
        // General invariant sweep: reconstructing every line's text and
        // re-segmenting it into graphemes must reproduce the exact input
        // grapheme sequence when nothing needed to be dropped (width large
        // enough that no soft-wrap space is ever consumed).
        let text = "plain café \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} text";
        let spans = vec![Span::raw(text)];
        let lines = wrap_spans(spans, 200);
        assert_eq!(flatten_text(&lines), text);
    }
}
