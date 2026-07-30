//! Rich-text entities. See docs/architecture.md §4.2.

use serde::{Deserialize, Serialize};

/// Offsets are UTF-16 code units exactly as Telegram delivers them.
/// Conversion to byte offsets happens in ONE place: `tgt_ui::render::offsets`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormattedText {
    pub text: String,
    pub entities: Vec<TextEntity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextEntity {
    pub offset_utf16: u32,
    pub length_utf16: u32,
    pub kind: EntityKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntityKind {
    Bold,
    Italic,
    Underline,
    Strikethrough,
    Spoiler,
    Code,
    Pre { language: Option<String> },
    Blockquote,
    TextUrl { url: String },
    Url,
    Mention,
    Hashtag,
}
