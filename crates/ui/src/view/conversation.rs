//! Conversation viewport (design spec §7.1 grouped mock, §5.2 paging-trigger
//! context; docs/plan.md T23). Renders the open chat's loaded window
//! bottom-up from the scroll anchor (`state/conversation.rs::Scroll`),
//! pulling laid-out lines from the shared `LayoutCache` (T21) that
//! `layout_message` / `layout_message_grouped` (T20) fill.
//!
//! ## Grouped-cache resolution
//!
//! `LayoutKey` (T21) has no "grouped" field, and this task owns neither that
//! struct nor `cache.rs`. So this module caches only the **full**
//! (`with_header = true`) rendering of every message under
//! `LayoutKey{message_id, width, theme_generation, spoilers_revealed}` and
//! derives the grouped rendering by slicing the header off at draw time:
//! T20's own test (`grouped_variant_omits_header` in `message_layout.rs`)
//! proves `layout_message(m)[1..] == layout_message_grouped(m)` — the header
//! costs exactly one leading line. [`slice_grouped`] is that slice, done in
//! one place. This is unsound only at pathologically narrow widths where
//! wrapping "Sender · HH:MM" itself would spill onto a second line (no width
//! this milestone renders at does: single-pane stack still has dozens of
//! columns to spare). A future cache-key change that folds grouping in is
//! the right long-term fix; it is out of this task's scope (owns
//! `cache.rs`, not this file).
//!
//! ## Bottom-up fill algorithm
//!
//! 1. Resolve the scroll anchor to a `(message index, line_offset)` pair
//!    ([`resolve_anchor`]): `Scroll::Bottom` is the newest loaded message;
//!    `Scroll::At` looks its message up in the window and falls back to the
//!    newest loaded message if it is not there (evicted or never loaded).
//! 2. Walk `convo.messages` backward (newest → oldest) from that index,
//!    pulling each message's lines from the cache and prepending them to a
//!    growing row buffer, so the buffer is always in correct top-to-bottom
//!    order ([`build_window`]).
//! 3. Whether a message groups under its immediate predecessor
//!    (`groups_with`, spec §7.1) is a purely structural fact about the
//!    message list — it does not depend on where the viewport happens to be
//!    scrolled to. The same boolean answers two questions at once: does
//!    this message get its own header (ungrouped), and does a blank
//!    block-boundary separator belong directly above it (spec §7.1's mock:
//!    one blank row between blocks, none inside a group).
//! 4. The anchor's own `line_offset` trims lines off the *tail* of its
//!    rendering before anything else is added — i.e. it is the number of
//!    the anchor message's own lines scrolled past the bottom edge. Only
//!    the anchor gets this treatment; every older message contributes its
//!    lines in full (subject to the final top clip).
//! 5. Once the buffer holds at least `height` rows (or history runs out),
//!    stop. If it overshot, drop rows from the *front* (the oldest end) —
//!    this is what lets the top-most visible message be a partial block,
//!    clipped mid-render, exactly like a real scrolled chat view. If it
//!    fell short (a short conversation), pad the front with blank rows so
//!    the real content still sits at the bottom of the pane instead of the
//!    top.
//!
//! ## File cards: two lines, on purpose
//!
//! A file-bearing message renders two rows, not one (T40's v1 look):
//!
//! ```text
//! 📎 spec.pdf · 2.4 MB          ← cached identity line (message_layout::file_card)
//! 📎 spec.pdf · ⏎ download      ← per-frame status line (file_card_line)
//! ```
//!
//! The cached line can never carry the affordance or a progress bar:
//! `LayoutKey` is `(message_id, width, theme_generation, spoilers_revealed)`
//! and download progress changes none of them, so anything live baked into
//! it would freeze at whatever it read the first time that message was laid
//! out (`render::message_layout`'s "File cards: the static/dynamic split").
//! Suppressing the cached line instead would mean re-laying-out the whole
//! message every frame — the one thing the cache exists to avoid. So the
//! live line is appended below it, like reactions and receipts, and the
//! name is repeated. A single-line card needs the cache key to grow a
//! "has a live suffix" notion; that is `cache.rs`'s to add, not this file's.
//!
//! Inline images (T38's `render::image::ImageArea`) are not wired here:
//! `ImageArea` is per-message mutable state that has to outlive a frame, and
//! this view owns nothing that lives that long — only the `LayoutCache` it
//! is handed does. Photos therefore render as placeholder cards, which spec
//! §8.3 requires to always work anyway. See the `T55/polish` marker below.
//!
//! ## Deferred seams
//!
//! Selection highlighting (T26) and in-chat search-hit highlighting (T47)
//! are both out of scope for this milestone. [`apply_selection_highlight`]
//! and [`apply_search_highlight`] are the marked seams: both are identity
//! functions today, called from the one place each content row is built,
//! ready for a later task to fill in without re-deriving the walk.

use std::collections::VecDeque;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use tgt_core::app::AppState;
use tgt_core::model::ids::{FileId, MessageId};
use tgt_core::model::message::{MessageContent, MessageView, ReactionView, SendState};
use tgt_core::state::conversation::{ConversationState, Scroll};
use tgt_core::state::media::MediaState;
use unicode_width::UnicodeWidthStr;

use crate::render::cache::{LayoutCache, LayoutKey};
use crate::render::message_layout::{
    LayoutOptions, file_card_line, file_card_upload_line, groups_with, layout_message_opts,
};
use crate::theme::Theme;

/// Renders the open chat's message window into `area`, border included (the
/// same convention as `view::header` / `view::chat_list`: each pane owns its
/// own frame). No open chat, or an open chat whose window is empty, renders
/// a dim centered placeholder instead of a message list.
pub fn draw(area: Rect, state: &AppState, theme: &Theme, f: &mut Frame, cache: &mut LayoutCache) {
    let block = Block::bordered().border_style(Style::new().fg(theme.text_muted));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(convo) = open_conversation(state) else {
        draw_centered(inner, "select a chat", theme, f);
        return;
    };
    if convo.messages.is_empty() {
        draw_centered(inner, "no messages yet", theme, f);
        return;
    }

    let rows = build_window(
        convo,
        &state.media,
        state.theme_generation,
        inner.width,
        inner.height,
        theme,
        cache,
    );
    let lines: Vec<Line<'static>> = rows.into_iter().map(|row| row.line).collect();
    f.render_widget(Paragraph::new(lines), inner);
}

/// `(oldest_visible, newest_visible)` message ids currently laid out inside
/// `area`, for T38's inline-image download-priority ordering (closer to the
/// scroll anchor downloads first). Runs the same bottom-up walk `draw` does,
/// so it agrees with what is actually on screen — a message clipped to a
/// partial block at the very top of the pane still counts as visible.
/// `None` when there is no open chat, the window is empty, or `area` is too
/// small to show anything.
pub fn visible_range(
    state: &AppState,
    area: Rect,
    cache: &mut LayoutCache,
) -> Option<(MessageId, MessageId)> {
    let convo = open_conversation(state)?;
    if convo.messages.is_empty() {
        return None;
    }
    let inner = Block::bordered().inner(area);
    if inner.width == 0 || inner.height == 0 {
        return None;
    }

    let theme = fallback_theme();
    let rows = build_window(
        convo,
        &state.media,
        state.theme_generation,
        inner.width,
        inner.height,
        &theme,
        cache,
    );

    let mut oldest = None;
    let mut newest = None;
    for row in &rows {
        if let Some(id) = row.message_id {
            oldest.get_or_insert(id);
            newest = Some(id);
        }
    }
    oldest.zip(newest)
}

fn open_conversation(state: &AppState) -> Option<&ConversationState> {
    let chat_id = state.open_chat?;
    state.conversations.get(&chat_id)
}

/// `visible_range` has no `Theme` parameter — T38 only needs message ids,
/// never styled output — but a cache miss during its walk still has to call
/// `layout_message`, which requires one. This built-in dark theme exists
/// purely to satisfy that call. Colors never affect line count or wrapping
/// (`message_layout`'s hazards are both about width, not color), so the
/// walk's *result* — which message ids are visible — is identical no matter
/// which theme happened to fill the cache. The one edge case this accepts:
/// if `visible_range` runs before `draw` has ever populated the cache for
/// the same `(message_id, width, theme_generation, spoilers_revealed)` key,
/// the entry it inserts carries this theme's colors rather than the
/// configured one, and a later `draw` call would then hit that entry
/// instead of recomputing with the real theme. That is a cosmetic,
/// one-frame risk, never a structural one, and self-heals on the next theme
/// or width change (both change the cache key). In the steady-state render
/// loop `draw` runs every frame for the visible chat, so in practice this
/// path is a cache hit almost always.
fn fallback_theme() -> Theme {
    Theme::default_dark()
}

/// One rendered terminal row. `message_id` is `None` for a blank separator
/// or top-padding row; `Some` ties a content row back to the message it
/// came from, which is all `visible_range` needs.
struct WindowRow {
    message_id: Option<MessageId>,
    line: Line<'static>,
}

/// The bottom-up walk described in the module docs. Always returns exactly
/// `height` rows (padded with blanks at the front if the loaded window is
/// shorter than the pane) unless `width`, `height`, or the window itself is
/// empty, in which case it returns nothing.
#[allow(clippy::too_many_arguments)]
fn build_window(
    convo: &ConversationState,
    media: &MediaState,
    theme_generation: u64,
    width: u16,
    height: u16,
    theme: &Theme,
    cache: &mut LayoutCache,
) -> VecDeque<WindowRow> {
    let mut rows: VecDeque<WindowRow> = VecDeque::new();
    if width == 0 || height == 0 || convo.messages.is_empty() {
        return rows;
    }
    let height = height as usize;

    let (anchor_idx, line_offset) = resolve_anchor(convo);
    let mut idx = anchor_idx;

    loop {
        let msg = &convo.messages[idx];
        // See the module docs: this one boolean decides both whether `msg`
        // gets its own header and whether a separator belongs above it.
        let grouped = idx > 0 && groups_with(&convo.messages[idx - 1], msg);
        let revealed = convo.revealed_spoilers.contains(&msg.id);
        let mut msg_lines = rendered_lines(
            msg,
            grouped,
            revealed,
            width,
            theme_generation,
            theme,
            cache,
        );

        if idx == anchor_idx && line_offset > 0 {
            let keep = msg_lines.len().saturating_sub(line_offset as usize);
            msg_lines.truncate(keep);
        }

        // Reactions and receipts are appended here, per frame, after the
        // cache lookup — never folded into the cached lines themselves.
        // Both can change (a reaction toggled, `last_read_outbox` advancing)
        // without `message_id` or `width` changing, which are the only
        // things that invalidate a `LayoutKey` entry; caching them would
        // leave stale reaction counts and checkmarks on screen. See the
        // module docs' "Grouped-cache resolution" for the same reasoning
        // applied to grouping.
        // T55/polish: wire `render::image::ImageArea` here for a downloaded
        // photo when the terminal has a graphics protocol (see the module
        // docs for why it can't live in this frame-local walk today).
        append_file_card(&mut msg_lines, msg, media, width, theme);
        append_reactions(&mut msg_lines, msg, width, theme);
        if msg.is_outgoing {
            append_receipt(&mut msg_lines, msg, convo.last_read_outbox, width, theme);
        }

        let mut block: Vec<WindowRow> = Vec::with_capacity(msg_lines.len() + 1);
        if idx > 0 && !grouped {
            block.push(WindowRow {
                message_id: None,
                line: Line::default(),
            });
        }
        block.extend(msg_lines.into_iter().map(|line| WindowRow {
            message_id: Some(msg.id),
            line: apply_search_highlight(apply_selection_highlight(line, msg.id), msg.id),
        }));

        // `block` is in top-to-bottom order already; pushing it to the
        // front in reverse keeps that order at the front of `rows`.
        for row in block.into_iter().rev() {
            rows.push_front(row);
        }

        if rows.len() >= height || idx == 0 {
            break;
        }
        idx -= 1;
    }

    while rows.len() > height {
        rows.pop_front();
    }
    while rows.len() < height {
        rows.push_front(WindowRow {
            message_id: None,
            line: Line::default(),
        });
    }

    rows
}

/// Selection-mode highlight seam (T26). A no-op today: returns `line`
/// unmodified. Wire the real highlight in here once `state/selection.rs`
/// exists, rather than threading selection state through the whole walk.
fn apply_selection_highlight(line: Line<'static>, _message_id: MessageId) -> Line<'static> {
    line
}

/// In-chat search-hit highlight seam (T47). A no-op today: returns `line`
/// unmodified. Wire `ConversationState::search_hits` matching in here.
fn apply_search_highlight(line: Line<'static>, _message_id: MessageId) -> Line<'static> {
    line
}

/// Resolves `convo.scroll` to a concrete `(index, line_offset)` into
/// `convo.messages` (ascending by id, so index 0 is the oldest loaded
/// message). `Scroll::At` naming a message id that fell out of the loaded
/// window (evicted, or never loaded) falls back to the newest message with
/// a zero offset — the same recovery `state/conversation.rs` itself takes
/// when an anchor disappears out from under it.
fn resolve_anchor(convo: &ConversationState) -> (usize, u16) {
    let last = convo.messages.len() - 1;
    match convo.scroll {
        Scroll::Bottom => (last, 0),
        Scroll::At {
            message_id,
            line_offset,
        } => match index_of(convo, message_id) {
            Some(idx) => (idx, line_offset),
            None => (last, 0),
        },
    }
}

/// Binary search for `id` in the ascending-by-id window (mirrors
/// `state/conversation.rs`'s private helper of the same name — that one
/// isn't public, and duplicating a five-line binary search is cheaper than
/// a cross-crate visibility change for it).
fn index_of(convo: &ConversationState, id: MessageId) -> Option<usize> {
    let idx = convo.messages.partition_point(|m| m.id < id);
    match convo.messages.get(idx) {
        Some(m) if m.id == id => Some(idx),
        _ => None,
    }
}

/// The cached full (`with_header = true`) rendering of `msg`, sliced down to
/// the grouped variant when `grouped`. See the module doc comment's
/// "Grouped-cache resolution".
fn rendered_lines(
    msg: &MessageView,
    grouped: bool,
    spoilers_revealed: bool,
    width: u16,
    theme_generation: u64,
    theme: &Theme,
    cache: &mut LayoutCache,
) -> Vec<Line<'static>> {
    let key = LayoutKey {
        message_id: msg.id,
        width,
        theme_generation,
        spoilers_revealed,
    };
    let full = cache.get_or_insert_with(key, || {
        layout_message_opts(
            msg,
            width,
            theme,
            LayoutOptions {
                grouped: false,
                spoilers_revealed,
            },
        )
    });
    if grouped {
        slice_grouped(full)
    } else {
        full.clone()
    }
}

/// The entire "grouped" transform: drop `layout_message`'s single leading
/// header line. See the module doc comment for why this is sound at every
/// width this milestone renders at.
fn slice_grouped(full: &[Line<'static>]) -> Vec<Line<'static>> {
    if full.is_empty() {
        Vec::new()
    } else {
        full[1..].to_vec()
    }
}

/// Pushes a `👍 3  ❤ 1`-style row onto `lines` when `msg` carries reactions;
/// a no-op otherwise, so a message without reactions costs nothing (same
/// discipline as an absent caption in `message_layout`). A reaction the
/// viewer chose (`chosen_by_me`) is bolded in the accent color; the rest are
/// muted. The row aligns with the message's side: right for own messages
/// (flush with the pane's right edge, like the rail), left with a two-column
/// indent for incoming ones (matching the rail-plus-space inset the cached
/// body lines carry).
fn append_reactions(lines: &mut Vec<Line<'static>>, msg: &MessageView, width: u16, theme: &Theme) {
    if msg.reactions.is_empty() {
        return;
    }
    lines.push(aligned_row(
        reaction_spans(&msg.reactions, theme),
        msg.is_outgoing,
        width,
    ));
}

/// A per-frame row under a message's cached block, aligned to the message's
/// own side: flush right for own messages (like the rail), indented two
/// columns for incoming ones (matching the rail-plus-space inset the cached
/// body lines carry).
fn aligned_row(content: Vec<Span<'static>>, is_outgoing: bool, width: u16) -> Line<'static> {
    let mut spans = Vec::with_capacity(content.len() + 1);
    if is_outgoing {
        let used = Line::from(content.clone()).width() as u16;
        spans.push(Span::raw(" ".repeat(width.saturating_sub(used) as usize)));
    } else {
        spans.push(Span::raw("  "));
    }
    spans.extend(content);
    Line::from(spans)
}

/// Pushes the live status row for a file-bearing message — the per-frame half
/// of the two-line card described in the module docs. A message with no file
/// costs nothing here.
///
/// An outgoing message with an upload still tracked under its id shows the
/// upload bar instead of the download affordance: until the send completes
/// there is no downloadable file on the other end to offer.
fn append_file_card(
    lines: &mut Vec<Line<'static>>,
    msg: &MessageView,
    media: &MediaState,
    width: u16,
    theme: &Theme,
) {
    let line = match media.uploads.get(&msg.id) {
        Some(progress) => file_card_upload_line(&msg.content, progress, theme),
        None => {
            let file = file_id_of(&msg.content).and_then(|id| media.files.get(&id));
            file_card_line(&msg.content, file, theme)
        }
    };
    if let Some(line) = line {
        lines.push(aligned_row(line.spans, msg.is_outgoing, width));
    }
}

/// The file a message's content carries, if any (mirrors the private helpers
/// of the same shape in `state/selection.rs` and `state/media.rs`).
fn file_id_of(content: &MessageContent) -> Option<FileId> {
    match content {
        MessageContent::Photo { file_id, .. }
        | MessageContent::Video { file_id, .. }
        | MessageContent::Audio { file_id, .. }
        | MessageContent::Document { file_id, .. } => Some(*file_id),
        MessageContent::Text(_)
        | MessageContent::Sticker { .. }
        | MessageContent::Unsupported { .. } => None,
    }
}

fn reaction_spans(reactions: &[ReactionView], theme: &Theme) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(reactions.len() * 2);
    for (i, reaction) in reactions.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        let style = if reaction.chosen_by_me {
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.text_muted)
        };
        spans.push(Span::styled(
            format!("{} {}", reaction.emoji, reaction.count),
            style,
        ));
    }
    spans
}

/// Pushes this own message's read-receipt marker: `⋯` while sending, `✗` on
/// failure (danger), else `✓`/`✓✓` from `last_read_outbox` (spec: "Sent" vs
/// "read"). Only called for `msg.is_outgoing` messages — incoming messages
/// have no receipt of our own to show.
///
/// Tries to tack the marker onto the trailing blank space of `lines`' last
/// row first (own message rows are usually padded flush to `width` already,
/// so this rarely fires — see the module docs on cache/uncached rows); when
/// there is no room it gets a row of its own, right-aligned the same way.
fn append_receipt(
    lines: &mut Vec<Line<'static>>,
    msg: &MessageView,
    last_read_outbox: MessageId,
    width: u16,
    theme: &Theme,
) {
    let (marker, style) = receipt_marker(msg, last_read_outbox, theme);
    let marker_cols = marker.width() as u16;

    let fits_on_last_row = lines
        .last()
        .map(|last| last.width() as u16 + 1 + marker_cols <= width)
        .unwrap_or(false);

    if fits_on_last_row {
        let last = lines.last_mut().expect("checked above");
        let used = last.width() as u16;
        let pad = width.saturating_sub(used + marker_cols);
        last.spans.push(Span::raw(" ".repeat(pad as usize)));
        last.spans.push(Span::styled(marker, style));
    } else {
        let pad = width.saturating_sub(marker_cols);
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(pad as usize)),
            Span::styled(marker, style),
        ]));
    }
}

fn receipt_marker(
    msg: &MessageView,
    last_read_outbox: MessageId,
    theme: &Theme,
) -> (&'static str, Style) {
    match &msg.send_state {
        SendState::Sending => ("⋯", Style::new().fg(theme.text_muted)),
        SendState::Failed(_) => ("✗", Style::new().fg(theme.danger)),
        SendState::Sent => {
            if msg.id <= last_read_outbox {
                ("✓✓", Style::new().fg(theme.text_muted))
            } else {
                ("✓", Style::new().fg(theme.text_muted))
            }
        }
    }
}

fn draw_centered(area: Rect, text: &str, theme: &Theme, f: &mut Frame) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let row = Rect {
        x: area.x,
        y: area.y + area.height / 2,
        width: area.width,
        height: 1,
    };
    let line = Line::from(Span::styled(text, Style::new().fg(theme.text_muted))).centered();
    f.render_widget(Paragraph::new(line), row);
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tgt_core::app::{AppState, Screen};
    use tgt_core::effect::TelemetryMode;
    use tgt_core::model::entity::FormattedText;
    use tgt_core::model::ids::{ChatId, FileId, MessageId, UserId};
    use tgt_core::model::key::KeyBindings;
    use tgt_core::model::message::{
        FileSnapshot, MessageCaps, MessageContent, ReactionView, ReplyPreview, SendState, Sender,
    };
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
    const BASE_DATE: i64 = 1_700_000_000;

    #[allow(clippy::too_many_arguments)]
    fn text_msg(
        id: i64,
        sender: Sender,
        sender_name: &str,
        outgoing: bool,
        date_offset: i64,
        text: &str,
        reply: Option<(i64, &str, &str)>,
    ) -> MessageView {
        MessageView {
            id: MessageId(id),
            chat_id: CHAT,
            sender,
            sender_name: sender_name.to_string(),
            is_outgoing: outgoing,
            date: BASE_DATE + date_offset,
            content: MessageContent::Text(FormattedText {
                text: text.to_string(),
                entities: Vec::new(),
            }),
            reply_to: reply.map(|(rid, rsender, excerpt)| ReplyPreview {
                message_id: MessageId(rid),
                sender_name: rsender.to_string(),
                excerpt: excerpt.to_string(),
            }),
            send_state: SendState::Sent,
            reactions: Vec::new(),
            caps: MessageCaps::default(),
            is_edited: false,
        }
    }

    fn doc_msg(
        id: i64,
        sender: Sender,
        sender_name: &str,
        date_offset: i64,
        file_name: &str,
        size: u64,
    ) -> MessageView {
        MessageView {
            id: MessageId(id),
            chat_id: CHAT,
            sender,
            sender_name: sender_name.to_string(),
            is_outgoing: false,
            date: BASE_DATE + date_offset,
            content: MessageContent::Document {
                file_id: FileId(1),
                file_name: file_name.to_string(),
                size,
                caption: FormattedText {
                    text: String::new(),
                    entities: Vec::new(),
                },
            },
            reply_to: None,
            send_state: SendState::Sent,
            reactions: Vec::new(),
            caps: MessageCaps::default(),
            is_edited: false,
        }
    }

    /// Ten messages: Alice/Bob incoming, "You" outgoing, a reply preview
    /// (msg 4 -> msg 3), and a document (msg 8). Some pairs sit inside
    /// `GROUP_WINDOW_SECS` of their predecessor (grouped), some don't.
    fn mixed_history() -> Vec<MessageView> {
        let alice = Sender::User(UserId(1));
        let bob = Sender::User(UserId(2));
        let me = Sender::User(UserId(3));
        vec![
            text_msg(
                1,
                alice,
                "Alice",
                false,
                0,
                "hey, did you see the PR?",
                None,
            ),
            text_msg(2, alice, "Alice", false, 60, "also CI is red on main", None),
            text_msg(3, me, "You", true, 120, "yeah, reviewing it now", None),
            text_msg(
                4,
                alice,
                "Alice",
                false,
                300,
                "take your time, no rush",
                Some((3, "You", "yeah, reviewing it now")),
            ),
            text_msg(5, alice, "Alice", false, 360, "🙏", None),
            text_msg(6, bob, "Bob", false, 500, "hey team", None),
            text_msg(7, me, "You", true, 560, "hi bob", None),
            doc_msg(8, bob, "Bob", 620, "architecture.pdf", 2_516_582),
            text_msg(9, bob, "Bob", false, 630, "take a look", None),
            text_msg(10, me, "You", true, 900, "will do", None),
        ]
    }

    fn conversation(messages: Vec<MessageView>, scroll: Scroll) -> ConversationState {
        ConversationState {
            chat_id: CHAT,
            messages: messages.into_iter().collect(),
            paging: PagingState::Idle,
            scroll,
            revealed_spoilers: BTreeSet::new(),
            last_read_inbox: MessageId(0),
            last_read_outbox: MessageId(0),
            search_hits: Vec::new(),
            selection: None,
        }
    }

    fn fixture_state(convo: Option<ConversationState>) -> AppState {
        let mut conversations = HashMap::new();
        let open_chat = convo.as_ref().map(|_| CHAT);
        if let Some(c) = convo {
            conversations.insert(CHAT, c);
        }
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
            conversations,
            open_chat,
            composer: ComposerState::default(),
            modal_ui: None,
            palette: None,
            chat_search: None,
            toasts: ToastState::default(),
            media: MediaState::default(),
            presence: PresenceState::default(),
            width: 120,
            height: 40,
            layout_breakpoint_cols: 100,
            theme_name: "dark".to_string(),
            theme_generation: 0,
            bindings: KeyBindings::default(),
            telemetry_mode: TelemetryMode::Off,
            telemetry_salt: [0u8; 32],
            now: Millis(0),
        }
    }

    fn render_to_string(width: u16, height: u16, state: &AppState) -> String {
        let theme = Theme::default_dark();
        let mut cache = LayoutCache::new();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                draw(area, state, &theme, f, &mut cache);
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

    fn theme() -> Theme {
        Theme::default_dark()
    }

    // --- snapshots -------------------------------------------------------

    #[test]
    fn mixed_grouped_history_120x40() {
        let state = fixture_state(Some(conversation(mixed_history(), Scroll::Bottom)));
        insta::assert_snapshot!(render_to_string(120, 40, &state));
    }

    #[test]
    fn scrolled_to_middle_anchor_80x24() {
        let state = fixture_state(Some(conversation(
            mixed_history(),
            Scroll::At {
                message_id: MessageId(4),
                line_offset: 0,
            },
        )));
        let rendered = render_to_string(80, 24, &state);
        assert!(rendered.contains("take your time"), "older content missing");
        assert!(
            !rendered.contains("will do"),
            "newest message must not be visible"
        );
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn empty_conversation_120x40() {
        let state = fixture_state(Some(conversation(Vec::new(), Scroll::Bottom)));
        let rendered = render_to_string(120, 40, &state);
        assert!(rendered.contains("no messages yet"));
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn no_open_chat_120x40() {
        let state = fixture_state(None);
        let rendered = render_to_string(120, 40, &state);
        assert!(rendered.contains("select a chat"));
        insta::assert_snapshot!(rendered);
    }

    // --- file cards (T40) -------------------------------------------------

    /// The two-line card the module docs describe: the cached identity line,
    /// then a live status line that redraws from `MediaState` every frame.
    #[test]
    fn file_card_status_line_follows_the_cached_identity_line() {
        let convo = conversation(
            vec![doc_msg(
                1,
                Sender::User(UserId(2)),
                "Bob",
                0,
                "spec.pdf",
                2_400,
            )],
            Scroll::Bottom,
        );
        let mut state = fixture_state(Some(convo));

        let rendered = render_to_string(80, 12, &state);
        assert!(
            rendered.contains("spec.pdf · 2.3 KB"),
            "the cached identity line is missing:\n{rendered}"
        );
        assert!(
            rendered.contains("⏎ download"),
            "an untouched file offers to download:\n{rendered}"
        );

        // Halfway through: the same message, a different `MediaState`. The
        // cache cannot invalidate on this (nothing in `LayoutKey` changed),
        // which is exactly why the line is rebuilt per frame.
        state.media.files.insert(
            FileId(1),
            FileSnapshot {
                id: FileId(1),
                expected_size: 2_400,
                downloaded_size: 1_200,
                is_downloading: true,
                is_completed: false,
                local_path: None,
            },
        );
        let rendered = render_to_string(80, 12, &state);
        assert!(
            rendered.contains("50%"),
            "a running download shows its progress:\n{rendered}"
        );
        assert!(
            !rendered.contains("⏎ download"),
            "a running download is not offered again:\n{rendered}"
        );

        state.media.files.insert(
            FileId(1),
            FileSnapshot {
                id: FileId(1),
                expected_size: 2_400,
                downloaded_size: 2_400,
                is_downloading: false,
                is_completed: true,
                local_path: Some(std::path::PathBuf::from("/tmp/spec.pdf")),
            },
        );
        let rendered = render_to_string(80, 12, &state);
        assert!(
            rendered.contains("⏎ open"),
            "a downloaded file offers to open:\n{rendered}"
        );
    }

    /// An outgoing message with an upload still in flight shows the upload
    /// bar instead of a download affordance — there is nothing to fetch back
    /// from a file that hasn't finished going out.
    #[test]
    fn tracked_upload_replaces_the_download_affordance() {
        let mut msg = doc_msg(1, Sender::User(UserId(3)), "You", 0, "report.pdf", 4_000);
        msg.is_outgoing = true;
        msg.send_state = SendState::Sending;
        let mut state = fixture_state(Some(conversation(vec![msg], Scroll::Bottom)));
        state.media.uploads.insert(
            MessageId(1),
            tgt_core::state::media::UploadProgress {
                chat_id: CHAT,
                uploaded: 1_000,
                total: 4_000,
            },
        );

        let rendered = render_to_string(80, 12, &state);
        assert!(
            rendered.contains("↑ report.pdf"),
            "the upload card is missing:\n{rendered}"
        );
        assert!(
            rendered.contains("25%"),
            "the upload's progress is missing:\n{rendered}"
        );
        assert!(
            !rendered.contains("⏎ download"),
            "an in-flight upload offers no download:\n{rendered}"
        );
    }

    // --- bottom-up fill (unit) --------------------------------------------

    #[test]
    fn tall_message_clips_at_the_top_not_the_bottom() {
        let long_text = "word ".repeat(200);
        let msg = text_msg(
            1,
            Sender::User(UserId(1)),
            "Alice",
            false,
            0,
            long_text.trim(),
            None,
        );
        let convo = conversation(vec![msg], Scroll::Bottom);
        let mut cache = LayoutCache::new();

        let rows = build_window(
            &convo,
            &MediaState::default(),
            0,
            20,
            5,
            &theme(),
            &mut cache,
        );

        assert_eq!(rows.len(), 5);
        assert!(rows.iter().all(|r| r.message_id == Some(MessageId(1))));
        let first_row_text: String = rows[0]
            .line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            !first_row_text.contains("Alice"),
            "header must be clipped away, not the tail: {first_row_text:?}"
        );
    }

    #[test]
    fn short_history_bottom_aligns_with_blank_padding_on_top() {
        let msg = text_msg(1, Sender::User(UserId(1)), "Alice", false, 0, "hi", None);
        let convo = conversation(vec![msg], Scroll::Bottom);
        let mut cache = LayoutCache::new();

        let rows = build_window(
            &convo,
            &MediaState::default(),
            0,
            40,
            10,
            &theme(),
            &mut cache,
        );

        assert_eq!(rows.len(), 10);
        assert!(
            rows[0].message_id.is_none(),
            "top row must be blank padding"
        );
        assert_eq!(rows.back().unwrap().message_id, Some(MessageId(1)));
    }

    #[test]
    fn grouped_message_has_no_separator_but_a_new_block_does() {
        let alice = Sender::User(UserId(1));
        let bob = Sender::User(UserId(2));
        let messages = vec![
            text_msg(1, alice, "Alice", false, 0, "one", None),
            text_msg(2, alice, "Alice", false, 10, "two", None), // groups with 1
            text_msg(3, bob, "Bob", false, 20, "three", None),   // new block
        ];
        let convo = conversation(messages, Scroll::Bottom);
        let mut cache = LayoutCache::new();

        let rows = build_window(
            &convo,
            &MediaState::default(),
            0,
            40,
            40,
            &theme(),
            &mut cache,
        );
        let rendered: Vec<String> = rows
            .iter()
            .map(|r| {
                r.line
                    .spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();

        let idx_two = rendered.iter().position(|l| l.contains("two")).unwrap();
        assert!(
            rendered[idx_two - 1].contains("one"),
            "no separator between grouped messages: {rendered:#?}"
        );

        let idx_bob_header = rendered.iter().position(|l| l.contains("Bob")).unwrap();
        assert!(
            rendered[idx_bob_header - 1].trim().is_empty(),
            "a blank separator must precede a new block's header: {rendered:#?}"
        );
    }

    #[test]
    fn line_offset_trims_the_anchors_own_tail() {
        let msg = text_msg(
            1,
            Sender::User(UserId(1)),
            "Alice",
            false,
            0,
            "hi there",
            None,
        );
        let convo_no_offset = conversation(vec![msg.clone()], Scroll::Bottom);
        let convo_offset = conversation(
            vec![msg],
            Scroll::At {
                message_id: MessageId(1),
                line_offset: 1,
            },
        );
        let mut cache = LayoutCache::new();

        let full = build_window(
            &convo_no_offset,
            &MediaState::default(),
            0,
            40,
            10,
            &theme(),
            &mut cache,
        );
        let trimmed = build_window(
            &convo_offset,
            &MediaState::default(),
            0,
            40,
            10,
            &theme(),
            &mut cache,
        );

        let full_count = full.iter().filter(|r| r.message_id.is_some()).count();
        let trimmed_count = trimmed.iter().filter(|r| r.message_id.is_some()).count();
        assert_eq!(trimmed_count, full_count - 1);
    }

    #[test]
    fn resolve_anchor_falls_back_to_newest_when_anchor_missing() {
        let convo = conversation(
            vec![text_msg(
                1,
                Sender::User(UserId(1)),
                "Alice",
                false,
                0,
                "hi",
                None,
            )],
            Scroll::At {
                message_id: MessageId(999),
                line_offset: 3,
            },
        );
        assert_eq!(resolve_anchor(&convo), (0, 0));
    }

    // --- reactions and receipts (T35) --------------------------------

    /// Spec: a reaction the viewer picked renders differently from one they
    /// didn't. This checks the actual `Style`s, not just the text, so a
    /// regression that drops the highlight but keeps the right characters on
    /// screen still fails.
    #[test]
    fn own_reaction_is_bold_accent_others_are_muted() {
        let mut msg = text_msg(1, Sender::User(UserId(1)), "Alice", false, 0, "nice", None);
        msg.reactions = vec![
            ReactionView {
                emoji: "👍".to_string(),
                count: 3,
                chosen_by_me: true,
            },
            ReactionView {
                emoji: "❤".to_string(),
                count: 1,
                chosen_by_me: false,
            },
        ];
        let convo = conversation(vec![msg], Scroll::Bottom);
        let mut cache = LayoutCache::new();

        let rows = build_window(
            &convo,
            &MediaState::default(),
            0,
            40,
            10,
            &theme(),
            &mut cache,
        );
        let reaction_row = rows
            .iter()
            .find(|r| r.line.spans.iter().any(|s| s.content.contains('👍')))
            .expect("reaction row missing");

        let mine = reaction_row
            .line
            .spans
            .iter()
            .find(|s| s.content.contains('👍'))
            .unwrap();
        assert_eq!(mine.content.as_ref(), "👍 3");
        assert_eq!(mine.style.fg, Some(theme().accent));
        assert!(mine.style.add_modifier.contains(Modifier::BOLD));

        let others = reaction_row
            .line
            .spans
            .iter()
            .find(|s| s.content.contains('❤'))
            .unwrap();
        assert_eq!(others.content.as_ref(), "❤ 1");
        assert_eq!(others.style.fg, Some(theme().text_muted));
        assert!(!others.style.add_modifier.contains(Modifier::BOLD));
    }

    /// Two own messages straddling `last_read_outbox`: the older one was
    /// read (`✓✓`), the newer one has only been sent so far (`✓`).
    #[test]
    fn sent_vs_read_checkmarks_straddle_last_read_outbox() {
        let me = Sender::User(UserId(3));
        let older = text_msg(1, me, "You", true, 0, "on it", None);
        let newer = text_msg(2, me, "You", true, 60, "done", None);
        let mut convo = conversation(vec![older, newer], Scroll::Bottom);
        convo.last_read_outbox = MessageId(1);
        let mut cache = LayoutCache::new();

        let rows = build_window(
            &convo,
            &MediaState::default(),
            0,
            40,
            10,
            &theme(),
            &mut cache,
        );
        let texts: Vec<String> = rows
            .iter()
            .map(|r| r.line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        let read_row = texts
            .iter()
            .find(|t| t.trim_end().ends_with("✓✓"))
            .expect("read message must show ✓✓");
        assert!(
            !texts
                .iter()
                .any(|t| t != read_row && t.trim_end().ends_with("✓✓")),
            "only the read message should show ✓✓: {texts:#?}"
        );
        assert!(
            texts
                .iter()
                .any(|t| t.trim_end().ends_with('✓') && !t.trim_end().ends_with("✓✓")),
            "the unread-by-peer message must show a single ✓: {texts:#?}"
        );
    }

    /// Full-frame snapshot: an incoming message with a mixed own/other
    /// reaction row, and two own messages straddling `last_read_outbox` so
    /// both checkmark states show up in the same frame.
    #[test]
    fn reactions_and_receipts_120x30() {
        let alice = Sender::User(UserId(1));
        let me = Sender::User(UserId(3));
        let mut liked = text_msg(1, alice, "Alice", false, 0, "final answer", None);
        liked.reactions = vec![
            ReactionView {
                emoji: "👍".to_string(),
                count: 3,
                chosen_by_me: true,
            },
            ReactionView {
                emoji: "❤".to_string(),
                count: 1,
                chosen_by_me: false,
            },
        ];
        let read = text_msg(2, me, "You", true, 60, "on it", None);
        let unread = text_msg(3, me, "You", true, 120, "done", None);

        let mut convo = conversation(vec![liked, read, unread], Scroll::Bottom);
        convo.last_read_outbox = MessageId(2);
        let state = fixture_state(Some(convo));

        insta::assert_snapshot!(render_to_string(120, 30, &state));
    }

    // --- visible_range ------------------------------------------------

    #[test]
    fn visible_range_reports_the_newest_shown_message_at_the_bottom() {
        let state = fixture_state(Some(conversation(mixed_history(), Scroll::Bottom)));
        let mut cache = LayoutCache::new();
        let area = Rect::new(0, 0, 80, 24);

        let (oldest, newest) = visible_range(&state, area, &mut cache).unwrap();
        assert_eq!(newest, MessageId(10));
        assert!(oldest <= newest);
    }

    #[test]
    fn visible_range_none_without_open_chat() {
        let state = fixture_state(None);
        let mut cache = LayoutCache::new();
        assert!(visible_range(&state, Rect::new(0, 0, 80, 24), &mut cache).is_none());
    }

    #[test]
    fn visible_range_none_for_empty_conversation() {
        let state = fixture_state(Some(conversation(Vec::new(), Scroll::Bottom)));
        let mut cache = LayoutCache::new();
        assert!(visible_range(&state, Rect::new(0, 0, 80, 24), &mut cache).is_none());
    }
}
