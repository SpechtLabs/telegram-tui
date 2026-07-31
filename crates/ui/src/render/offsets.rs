//! Constraint 5's isolated pure function: converting a Telegram UTF-16
//! code-unit span into a byte range into a Rust `&str`.
//!
//! Telegram entity offsets (`text_entity`'s `offset`/`length`) are counted in
//! UTF-16 code units, not bytes and not `char`s. Any message containing an
//! emoji or other non-BMP character before a styled span mis-slices unless
//! this conversion runs first. See docs/architecture.md §4.9 and spec §8.1
//! hazard 1.

use std::ops::Range;

/// Convert a Telegram UTF-16 code-unit span into a byte range into `text`.
///
/// Returns `None` (caller renders the message unstyled and logs locally) when:
/// - the span falls outside the text,
/// - an endpoint lands inside a surrogate pair (mid-astral-character),
/// - `offset_utf16 + length_utf16` overflows.
///
/// Never panics. Never slices on a non-char-boundary: every byte index this
/// function returns falls exactly on a `char` boundary, because it is only
/// ever taken before or after fully consuming a `char` while walking `text`.
pub fn utf16_span_to_byte_range(
    text: &str,
    offset_utf16: u32,
    length_utf16: u32,
) -> Option<Range<usize>> {
    let end_utf16 = offset_utf16.checked_add(length_utf16)?;
    let start = utf16_pos_to_byte_index(text, offset_utf16)?;
    let end = utf16_pos_to_byte_index(text, end_utf16)?;
    Some(start..end)
}

/// Find the byte index in `text` corresponding to UTF-16 code-unit position
/// `target`, or `None` if `target` lands inside a surrogate pair or beyond
/// the end of `text`.
fn utf16_pos_to_byte_index(text: &str, target: u32) -> Option<usize> {
    let mut byte_pos = 0usize;
    let mut utf16_pos = 0u32;

    for ch in text.chars() {
        if utf16_pos == target {
            return Some(byte_pos);
        }
        // char::len_utf16() is 1 for BMP characters, 2 for astral characters
        // (encoded as a surrogate pair in UTF-16). If `target` is strictly
        // less than the far edge of this char but didn't match its near
        // edge above, it lands inside the surrogate pair.
        let ch_utf16_len = ch.len_utf16() as u32;
        if target < utf16_pos + ch_utf16_len {
            return None;
        }
        byte_pos += ch.len_utf8();
        utf16_pos += ch_utf16_len;
    }

    if utf16_pos == target {
        Some(byte_pos)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Row 1: `"hello world"`, (0, 5) -> `Some(0..5)`
    #[test]
    fn row_01_ascii_prefix() {
        assert_eq!(utf16_span_to_byte_range("hello world", 0, 5), Some(0..5));
    }

    /// Row 2: `"müller"`, (1, 1) -> `Some(1..3)`
    #[test]
    fn row_02_two_byte_bmp_char() {
        assert_eq!(utf16_span_to_byte_range("müller", 1, 1), Some(1..3));
    }

    /// Row 3: `"🙂 ok"`, (3, 2) -> `Some(5..7)`
    #[test]
    fn row_03_after_astral_char() {
        assert_eq!(utf16_span_to_byte_range("🙂 ok", 3, 2), Some(5..7));
    }

    /// Row 4: `"你好 hi"`, (3, 2) -> `Some(7..9)`
    #[test]
    fn row_04_after_cjk() {
        assert_eq!(utf16_span_to_byte_range("你好 hi", 3, 2), Some(7..9));
    }

    /// Row 5: `"e\u{0301}x"` (e + combining acute), (0, 2) -> `Some(0..3)`
    #[test]
    fn row_05_combining_mark() {
        assert_eq!(utf16_span_to_byte_range("e\u{0301}x", 0, 2), Some(0..3));
    }

    /// Row 6: `"👨\u{200D}👩\u{200D}👧"` (ZWJ family), (0, 8) -> `Some(0..18)`
    #[test]
    fn row_06_zwj_family() {
        assert_eq!(
            utf16_span_to_byte_range("👨\u{200D}👩\u{200D}👧", 0, 8),
            Some(0..18)
        );
    }

    /// Row 7: `"🇩🇪!"`, (4, 1) -> `Some(8..9)`
    #[test]
    fn row_07_regional_indicator_flag() {
        assert_eq!(utf16_span_to_byte_range("🇩🇪!", 4, 1), Some(8..9));
    }

    /// Row 8: `"🙂"`, (1, 1) — starts mid-surrogate -> `None`
    #[test]
    fn row_08_start_mid_surrogate() {
        assert_eq!(utf16_span_to_byte_range("🙂", 1, 1), None);
    }

    /// Row 9: `"a🙂b"`, (0, 2) — ends mid-surrogate -> `None`
    #[test]
    fn row_09_end_mid_surrogate() {
        assert_eq!(utf16_span_to_byte_range("a🙂b", 0, 2), None);
    }

    /// Row 10: `"hi"`, (5, 1) — offset past end -> `None`
    #[test]
    fn row_10_offset_past_end() {
        assert_eq!(utf16_span_to_byte_range("hi", 5, 1), None);
    }

    /// Row 11: `"hi"`, (0, 5) — length past end -> `None`
    #[test]
    fn row_11_length_past_end() {
        assert_eq!(utf16_span_to_byte_range("hi", 0, 5), None);
    }

    /// Row 12: `"hi"`, (0, 0) -> `Some(0..0)`
    #[test]
    fn row_12_zero_length_span() {
        assert_eq!(utf16_span_to_byte_range("hi", 0, 0), Some(0..0));
    }

    /// Row 13: `"🙂"`, (0, 2) -> `Some(0..4)`
    #[test]
    fn row_13_whole_astral_char() {
        assert_eq!(utf16_span_to_byte_range("🙂", 0, 2), Some(0..4));
    }

    /// Row 14: `"a𝕏b"`, (0, 4) -> `Some(0..6)`
    #[test]
    fn row_14_astral_char_mid_string() {
        assert_eq!(utf16_span_to_byte_range("a𝕏b", 0, 4), Some(0..6));
    }

    /// offset + length overflowing u32 must return None, never panic.
    #[test]
    fn overflowing_span_returns_none() {
        assert_eq!(utf16_span_to_byte_range("hi", u32::MAX, 1), None);
    }

    /// Property test (plain loop, no proptest dependency): for a corpus
    /// mixing ASCII, CJK, emoji and combining marks, every prefix UTF-16
    /// length from 0 to the corpus's total UTF-16 length either lands
    /// mid-surrogate (`None`) or yields a byte range whose end sits on a
    /// `char` boundary.
    #[test]
    fn every_prefix_ends_on_char_boundary_or_is_none() {
        let corpus = "Hello 你好 🙂 e\u{0301}x 👨\u{200D}👩\u{200D}👧 🇩🇪 done";
        let total_utf16_len = corpus.encode_utf16().count() as u32;

        for prefix_len in 0..=total_utf16_len {
            match utf16_span_to_byte_range(corpus, 0, prefix_len) {
                None => {}
                Some(range) => {
                    assert!(
                        corpus.is_char_boundary(range.start),
                        "prefix_len {prefix_len}: start {} not a char boundary",
                        range.start
                    );
                    assert!(
                        corpus.is_char_boundary(range.end),
                        "prefix_len {prefix_len}: end {} not a char boundary",
                        range.end
                    );
                }
            }
        }
    }
}
