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
//! ## The per-frame half of a block (T62)
//!
//! `LayoutKey` is `(message_id, width, theme_generation, spoilers_revealed)`.
//! Three things a message shows change without touching any of those, so
//! none of them may live in a cached line: its reactions, its read receipt,
//! and an attachment's download state. All three are drawn here instead,
//! once per frame, on top of the block the cache handed back.
//!
//! - **Attachments** are exactly one row (design-language §4). The cached
//!   layout contributes no file-identity line at all now, so
//!   [`append_attachment`] is the only thing that draws one and there is
//!   nothing to render twice. It slots in above the caption, where the media
//!   belongs, and carries the block's rail like every other row.
//! - **Reactions** get their own row under the block ([`append_reactions`]).
//! - **Receipts** never get a row. [`append_receipt`] appends the marker to
//!   the last row the block already has, spending the gutter
//!   `message_layout::RECEIPT_COLS` reserved for exactly this (see that
//!   module's "The receipt gutter"). A column of ticks down the pane edge is
//!   the defect this replaced.
//!
//! ## Inline images (T63, design-language §6)
//!
//! A downloaded photo on a terminal with a graphics protocol replaces its §4
//! line with the picture itself. The per-message `ImageArea`s that hold the
//! encoded protocol live in the `RenderState` this view is handed
//! (architecture §4.9.1), which is what makes this possible at all — they
//! have to outlive the frame, and nothing in this walk does.
//!
//! It happens in two passes, because an image is not a `Line` and cannot
//! travel through the row buffer:
//!
//! 1. While laying a block out, [`append_attachment`] asks
//!    `ImageArea::plan` how many rows the picture will need — a question
//!    answered from the file's *header*, not a full decode — and inserts
//!    that many blank, railed rows where the §4 line would have gone. Each
//!    carries an [`ImageTag`], so the bottom-up walk's clipping and padding
//!    move the reservation around exactly like any other row (a block
//!    clipped at the top of the pane simply keeps fewer of them, and the
//!    image draws smaller into what is left).
//! 2. [`draw`] renders the row buffer as usual, then draws each image over
//!    its own run of reserved rows, through [`placement_rect`] — one place,
//!    clipped to the pane, so a picture can never reach the sidebar however
//!    the file's aspect ratio worked out. Those rows are recorded in the hit
//!    map like every other row of the block, so a click on a photo still
//!    resolves to its message.
//!
//! Both passes size the picture through `ImageArea`, from the same measured
//! cell size, so the rows step 1 reserves are the cells step 2 fills. Where
//! they legitimately differ is clipping: a block cut off at the top of the
//! pane keeps fewer of its reserved rows, and step 2 fits the picture into
//! what is left rather than drawing past it.
//!
//! Every gate is a reason to skip step 1 and keep the §4 line: no protocol
//! (`rs.graphics` is `None`, which is also how `[app].inline_images = false`
//! arrives here), not a photo, not finished downloading, no local path, an
//! upload still in flight, or a file whose header won't parse. The one
//! failure that cannot be seen coming — bytes that decode no further than
//! their header — is caught in step 2, where the §4 line is drawn back over
//! the first reserved row and the path is not planned again. So the fallback
//! is automatic and silent in all cases, which is what spec §8.3 asks for.
//!
//! ## Search-hit highlighting (T47)
//!
//! `apply_search_highlight` (formerly the identity seam T23 left) now reads
//! `ConversationState::search_hits` and `ChatSearchState::current_hit`
//! (T42). **Fidelity note:** `searchChatMessages` (architecture §4.3/§4.7)
//! answers with matching message ids only — no per-message offset/length of
//! the substring TDLib actually matched — so there is no matched *text
//! range* to underline within a message body. This highlights at message
//! granularity instead: every hit gets a `theme.surface_raised` tint across
//! all of its rendered lines, and the current hit's first line additionally
//! goes bold `theme.warning` so it reads as distinct from the rest. Both are
//! style-only changes (no inserted characters), so no line's width and no
//! wrapping decision changes — the walk, the cache, and every non-search
//! snapshot are unaffected.
//!
//! The query box itself (`Focus::ChatSearch`) renders as a one-line bar at
//! the top of the conversation pane, the same treatment `view::chat_list`
//! gives its `/` filter line (`draw_search_input` mirrors
//! `chat_list::draw_filter_input`).

use std::collections::VecDeque;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use tgt_core::app::AppState;
use tgt_core::model::hit::HitTarget;
use tgt_core::model::ids::{FileId, MessageId};
use tgt_core::model::message::{MessageContent, MessageView, ReactionView, SendState};
use tgt_core::state::conversation::{ConversationState, Scroll};
use tgt_core::state::focus::Focus;
use tgt_core::state::media::MediaState;
use tgt_core::state::search::ChatSearchState;
use unicode_width::UnicodeWidthStr;

use crate::render::cache::{LayoutCache, LayoutKey};
use crate::render::hit::HitMap;
use crate::render::message_layout::{
    LayoutOptions, RAIL_COLS, RECEIPT_COLS, append_marker_inline, file_card_line,
    file_card_upload_line, groups_with, layout_message_opts, place_row, rail_style,
};
use crate::render::state::RenderState;
use crate::theme::Theme;

/// Renders the open chat's message window into `area`, border included (the
/// same convention as `view::header` / `view::chat_list`: each pane owns its
/// own frame). No open chat, or an open chat whose window is empty, renders
/// a dim centered placeholder instead of a message list.
///
/// Every row that came from a message is recorded in `hits` as it is laid
/// out, so a right-click resolves to the message whose block covers that
/// cell — including the partial block clipped at the top of the pane. Blank
/// separator and padding rows carry no message and stay unclickable.
pub fn draw(
    area: Rect,
    state: &AppState,
    theme: &Theme,
    f: &mut Frame,
    rs: &mut RenderState,
    hits: &mut HitMap,
) {
    // No box: the conversation is a region, not a widget. `view::root` already
    // padded this rect and drew the one rule that separates it from the
    // sidebar (docs/design-language.md §1).
    let messages_area = draw_search_bar_if_active(area, state, theme, f);

    // Before a single row is laid out: if this pane, the open chat, the
    // scroll anchor or the loaded window moved since the last frame, every
    // image the last frame placed is now in the wrong place and has to go
    // (see `render::state`'s "Ghosting"). Ahead of the early returns below,
    // so closing a chat drops its images too.
    rs.note_viewport(state, messages_area);

    let Some(convo) = open_conversation(state) else {
        draw_centered(messages_area, "select a chat", theme, f);
        return;
    };
    if convo.messages.is_empty() {
        draw_centered(messages_area, "no messages yet", theme, f);
        return;
    }

    let rows = build_window(
        convo,
        &state.media,
        state.theme_generation,
        messages_area.width,
        messages_area.height,
        theme,
        rs,
        state.chat_search.as_ref(),
    );
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(rows.len());
    let mut placements: Vec<Placement> = Vec::new();
    for (i, row) in rows.into_iter().enumerate() {
        if let Some(message_id) = row.message_id {
            hits.push(
                Rect {
                    x: messages_area.x,
                    y: messages_area.y + i as u16,
                    width: messages_area.width,
                    height: 1,
                },
                HitTarget::Message(message_id),
            );
            // Sub-row targets (architecture §7.5.1, T77), pushed after the
            // row-wide one above so `HitMap::target_at`'s existing
            // last-pushed-wins rule resolves a click on the narrower range
            // to these instead — no new resolution logic, same mechanism.
            for range in &row.spoiler_cols {
                hits.push(
                    Rect {
                        x: messages_area.x + range.start,
                        y: messages_area.y + i as u16,
                        width: range.end - range.start,
                        height: 1,
                    },
                    HitTarget::Spoiler(message_id),
                );
            }
            if let Some(quoted) = row.reply_quote {
                // Full row width, unlike `Spoiler`: a reply-quote row is
                // never shared with ordinary body text the way a spoiler
                // run can be, so there is no narrower range to compute.
                hits.push(
                    Rect {
                        x: messages_area.x,
                        y: messages_area.y + i as u16,
                        width: messages_area.width,
                        height: 1,
                    },
                    HitTarget::ReplyQuote {
                        containing: message_id,
                        quoted,
                    },
                );
            }
        }
        if let Some(tag) = row.image {
            extend_placement(&mut placements, tag, i as u16);
        }
        lines.push(row.line);
    }
    f.render_widget(Paragraph::new(lines), messages_area);
    draw_inline_images(messages_area, &placements, rs, f);

    // Anything the walk above didn't touch has scrolled out of the pane.
    // Dropping it now is what keeps the store to a viewport's worth of
    // encoded images — and, more importantly, means nothing off-screen is
    // still holding a protocol placement.
    rs.images.sweep();
}

/// One message's inline image and the rows it ended up occupying, in pane
/// coordinates. Built during the same pass that flattens rows into lines,
/// since that pass is where a row's final `y` is known.
struct Placement {
    tag: Rc<ImageTag>,
    top: u16,
    bottom: u16,
}

/// Grows the current run, or starts a new one. A message's reserved rows are
/// always contiguous, so only the last placement can ever be extended.
fn extend_placement(placements: &mut Vec<Placement>, tag: Rc<ImageTag>, y: u16) {
    let same_run = placements
        .last()
        .is_some_and(|p| p.tag.message_id == tag.message_id);
    if same_run {
        if let Some(last) = placements.last_mut() {
            last.bottom = y;
        }
    } else {
        placements.push(Placement {
            tag,
            top: y,
            bottom: y,
        });
    }
}

/// Draws each planned image over the rows reserved for it. A `render` that
/// comes back `false` means the file's bytes did not survive a full decode
/// after its header promised they would; the §4 line goes back over the
/// first reserved row so the frame still says something true about the
/// attachment, and `ImageArea` remembers not to plan that path again.
fn draw_inline_images(pane: Rect, placements: &[Placement], rs: &mut RenderState, f: &mut Frame) {
    let graphics = rs.graphics;
    let cell = rs.cell_size();
    for placement in placements {
        let tag = &placement.tag;
        let rect = placement_rect(pane, tag.inset, tag.cols, placement.top, placement.bottom);
        let drawn = rs
            .images
            .area(tag.message_id, graphics, cell)
            .render(rect, &tag.path, f);
        if !drawn {
            f.render_widget(
                Paragraph::new(tag.fallback.clone()),
                Rect {
                    x: pane.x,
                    y: pane.y + placement.top,
                    width: pane.width,
                    height: 1,
                },
            );
        }
    }
}

/// Where one placement's picture goes, in absolute frame coordinates, always
/// inside `pane`.
///
/// The intersection is not defensive tidiness. A graphics protocol places
/// pixels, not cells: a rect that runs one column past the pane draws over
/// the sidebar, and the terminal keeps drawing it there until something
/// rewrites those cells — which the conversation pane, by definition, never
/// will. Clipping here is what makes "an image never draws a cell outside
/// the pane" a property of the geometry rather than of every input that
/// feeds it.
/// An overhang so complete that nothing is left comes back as a rect at the
/// pane's own origin rather than `Rect::intersection`'s zero-sized one out at
/// the overhanging coordinate: both draw nothing, but only one of them is
/// still describable as "inside the pane".
fn placement_rect(pane: Rect, inset: u16, cols: u16, top: u16, bottom: u16) -> Rect {
    let rect = Rect {
        x: pane.x.saturating_add(inset),
        y: pane.y.saturating_add(top),
        width: cols,
        height: bottom.saturating_sub(top).saturating_add(1),
    }
    .intersection(pane);
    if rect.is_empty() {
        return Rect {
            width: 0,
            height: 0,
            ..pane
        };
    }
    rect
}

/// Reserves and draws the one-line search query bar at the top of `inner`
/// when `Focus::ChatSearch` is active, returning the area left over for the
/// message list (unchanged from `inner` when search isn't active — zero
/// visual change to every non-search render, including every existing
/// snapshot).
fn draw_search_bar_if_active(inner: Rect, state: &AppState, theme: &Theme, f: &mut Frame) -> Rect {
    let Some(search) = state.chat_search.as_ref() else {
        return inner;
    };
    if !matches!(state.focus.current(), Focus::ChatSearch) {
        return inner;
    }
    if inner.height == 0 {
        return inner;
    }
    let areas = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(inner);
    draw_search_input(areas[0], search, theme, f);
    areas[1]
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
    rs: &mut RenderState,
) -> Option<(MessageId, MessageId)> {
    let convo = open_conversation(state)?;
    if convo.messages.is_empty() {
        return None;
    }
    // Mirrors `draw`: no border is subtracted, the rect arrives already padded.
    let inner = area;
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    let messages_area =
        if state.chat_search.is_some() && matches!(state.focus.current(), Focus::ChatSearch) {
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(inner)[1]
        } else {
            inner
        };
    if messages_area.width == 0 || messages_area.height == 0 {
        return None;
    }

    let theme = fallback_theme();
    let rows = build_window(
        convo,
        &state.media,
        state.theme_generation,
        messages_area.width,
        messages_area.height,
        &theme,
        rs,
        state.chat_search.as_ref(),
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
    /// Set on rows reserved for an inline image (module docs). Shared by
    /// every row of one message's run, so the run can be recognized after
    /// the walk has moved, clipped and padded its rows.
    image: Option<Rc<ImageTag>>,
    /// Column ranges within `line` rendering a masked spoiler block —
    /// sub-row hit targets (architecture §7.5.1, T77), narrower than the
    /// row-wide `Message` target `draw` also pushes for this row.
    spoiler_cols: Vec<Range<u16>>,
    /// Set only on the row rendering a reply-quote excerpt: the id of the
    /// message it names (architecture §7.5.1, T77's jump target).
    reply_quote: Option<MessageId>,
}

impl WindowRow {
    /// A row that belongs to no message: a block separator or the padding
    /// that bottom-aligns a short history.
    fn blank() -> Self {
        WindowRow {
            message_id: None,
            line: Line::default(),
            image: None,
            spoiler_cols: Vec::new(),
            reply_quote: None,
        }
    }
}

/// Sub-row hit metadata for one already-laid-out `Line` (architecture
/// §7.5.1, T77). Computed by scanning the line's spans for two stable,
/// intentional content signatures — a masked spoiler run is rendered as
/// `'█'`-only glyphs and nothing else in this pipeline ever produces that
/// character (progress bars use `▓`/`░`); a reply-quote line is rendered as
/// exactly one span starting `↳` and nothing else does — rather than
/// duplicating `message_layout.rs`'s wrap/entity logic here, or changing
/// its return type and rippling into the layout cache for facts the view
/// can already recover for free.
#[derive(Default, Clone)]
struct RowHits {
    spoiler_cols: Vec<Range<u16>>,
    reply_quote: Option<MessageId>,
}

fn scan_row_hits(line: &Line<'static>, reply_to: Option<MessageId>) -> RowHits {
    let mut hits = RowHits::default();
    let mut col: u16 = 0;
    for span in &line.spans {
        let w = span.content.width() as u16;
        if !span.content.is_empty() && span.content.chars().all(|c| c == '█') {
            hits.spoiler_cols.push(col..col + w);
        } else if hits.reply_quote.is_none() && reply_to.is_some() && span.content.starts_with('↳')
        {
            hits.reply_quote = reply_to;
        }
        col += w;
    }
    hits
}

/// What one message's reserved image rows need at draw time. Shared behind
/// an `Rc` rather than cloned per row: the path and the fallback line are
/// the same for every row of the run, and a tall photo reserves up to
/// `MAX_IMAGE_ROWS` of them.
struct ImageTag {
    message_id: MessageId,
    /// The downloaded file, from `MediaState`.
    path: PathBuf,
    /// Columns between the pane's left edge and the image's first column.
    inset: u16,
    /// Planned width in cells. `ImageArea::render` fits inside it.
    cols: u16,
    /// The design-language §4 line the image replaced, kept for the one
    /// failure mode planning cannot foresee (see [`draw_inline_images`]).
    fallback: Line<'static>,
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
    rs: &mut RenderState,
    chat_search: Option<&ChatSearchState>,
) -> VecDeque<WindowRow> {
    let mut rows: VecDeque<WindowRow> = VecDeque::new();
    if width == 0 || height == 0 || convo.messages.is_empty() {
        return rows;
    }
    // An image never gets more rows than the pane itself has, whatever
    // `MAX_IMAGE_ROWS` would otherwise allow: a photo that fills the entire
    // viewport leaves no room for the conversation it is part of.
    let image_rows_budget = height;
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
            &mut rs.cache,
        );

        if idx == anchor_idx && line_offset > 0 {
            let keep = msg_lines.len().saturating_sub(line_offset as usize);
            msg_lines.truncate(keep);
        }

        // Scanned from the cached lines only, before the per-frame ones
        // below are appended — a spoiler block or a reply-quote line only
        // ever comes from `rendered_lines`, never from the attachment,
        // reaction or receipt lines (module docs on `RowHits`).
        let reply_target = msg.reply_to.as_ref().map(|r| r.message_id);
        let row_hits: Vec<RowHits> = msg_lines
            .iter()
            .map(|line| scan_row_hits(line, reply_target))
            .collect();

        // The attachment line, reactions, and the receipt are drawn here,
        // per frame, on top of what the cache handed back — never folded
        // into the cached lines themselves. All three change (a download
        // progresses, a reaction is toggled, `last_read_outbox` advances)
        // without `message_id` or `width` changing, which are the only
        // things that invalidate a `LayoutKey` entry; caching them would
        // leave stale percentages, counts, and checkmarks on screen. See
        // the module docs' "The per-frame half of a block".
        let reserved = append_attachment(
            &mut msg_lines,
            msg,
            grouped,
            media,
            width,
            image_rows_budget,
            theme,
            rs,
        );
        append_reactions(&mut msg_lines, msg, width, theme);
        if msg.is_outgoing {
            append_receipt(&mut msg_lines, msg, convo.last_read_outbox, width, theme);
        }

        let selected = convo
            .selection
            .as_ref()
            .is_some_and(|s| s.message_id == msg.id);
        let hit_kind = search_hit_kind(convo, chat_search, msg.id);
        let mut block: Vec<WindowRow> = Vec::with_capacity(msg_lines.len() + 1);
        if idx > 0 && !grouped {
            block.push(WindowRow::blank());
        }
        block.extend(msg_lines.into_iter().enumerate().map(|(i, line)| {
            // `row_hits` only has entries for the cached lines scanned
            // above; the attachment/reaction/receipt rows appended after
            // that scan fall through to the `unwrap_or_default()` (neither
            // ever renders a spoiler block or a reply-quote line).
            let hits = row_hits.get(i).cloned().unwrap_or_default();
            WindowRow {
                message_id: Some(msg.id),
                image: reserved
                    .as_ref()
                    .filter(|r| r.covers(i))
                    .map(|r| Rc::clone(&r.tag)),
                spoiler_cols: hits.spoiler_cols,
                reply_quote: hits.reply_quote,
                line: apply_search_highlight(
                    apply_selection_highlight(line, selected, width, theme),
                    hit_kind,
                    i == 0,
                    theme,
                ),
            }
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
        rows.push_front(WindowRow::blank());
    }

    rows
}

/// Selection highlight (design-language §5): a `surface_raised` background
/// across every line of the selected message, and nothing else — no border,
/// no full-width inverse block, no color change to the body text. The row is
/// first padded out to `width` so the background reads as a band rather than
/// stopping wherever the text happens to end.
///
/// Style-only apart from that trailing pad, so no wrapping decision the
/// cached layout made is disturbed.
fn apply_selection_highlight(
    line: Line<'static>,
    selected: bool,
    width: u16,
    theme: &Theme,
) -> Line<'static> {
    if !selected {
        return line;
    }
    let pad = width.saturating_sub(line.width() as u16);
    let mut spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .map(|span| {
            let style = span.style.bg(theme.surface_raised);
            Span::styled(span.content, style)
        })
        .collect();
    if pad > 0 {
        spans.push(Span::styled(
            " ".repeat(pad as usize),
            Style::new().bg(theme.surface_raised),
        ));
    }
    Line::from(spans)
}

/// Where `msg_id` sits relative to the open chat's search hits (module docs'
/// "Search-hit highlighting" section). `None` whenever search isn't active
/// (`chat_search` is `None`) or the message just isn't a hit — including
/// while `chat_search` is `Some` but `handle_td_result` hasn't answered yet,
/// since `search_hits` is empty until then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchHitKind {
    None,
    Other,
    Current,
}

fn search_hit_kind(
    convo: &ConversationState,
    chat_search: Option<&ChatSearchState>,
    msg_id: MessageId,
) -> SearchHitKind {
    let Some(search) = chat_search else {
        return SearchHitKind::None;
    };
    match convo.search_hits.iter().position(|&id| id == msg_id) {
        Some(idx) if idx == search.current_hit => SearchHitKind::Current,
        Some(_) => SearchHitKind::Other,
        None => SearchHitKind::None,
    }
}

/// In-chat search-hit highlight seam (T47). See the module docs' "Search-hit
/// highlighting" section for the fidelity note this design accepts. Every
/// line of a hit's block gets a `theme.surface_raised` tint; the current
/// hit's `is_first_line` additionally goes bold `theme.warning`, the one
/// visual cue that tells it apart from the other hits. Both are style-only
/// edits — no span's text changes — so no line's rendered width moves and no
/// wrapping decision made upstream is invalidated.
fn apply_search_highlight(
    line: Line<'static>,
    kind: SearchHitKind,
    is_first_line: bool,
    theme: &Theme,
) -> Line<'static> {
    if kind == SearchHitKind::None {
        return line;
    }
    let emphasize = kind == SearchHitKind::Current && is_first_line;
    Line::from(
        line.spans
            .into_iter()
            .map(|span| tint_search_hit(span, emphasize, theme))
            .collect::<Vec<_>>(),
    )
}

fn tint_search_hit(span: Span<'static>, emphasize: bool, theme: &Theme) -> Span<'static> {
    let style = span.style.bg(theme.surface_raised);
    let style = if emphasize {
        style.fg(theme.warning).add_modifier(Modifier::BOLD)
    } else {
        style
    };
    Span::styled(span.content, style)
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
/// muted. The row carries the block's rail, so a reacted-to message still
/// reads as one unbroken block.
fn append_reactions(lines: &mut Vec<Line<'static>>, msg: &MessageView, width: u16, theme: &Theme) {
    if msg.reactions.is_empty() {
        return;
    }
    lines.push(place_row(
        reaction_spans(&msg.reactions, theme),
        width,
        msg.is_outgoing,
        rail_style(msg, theme),
    ));
}

/// Inserts the one attachment row (design-language §4) for a file-bearing
/// message. A message with no attachment costs nothing here.
///
/// The row goes *above* the caption — media first, words about it after —
/// and below the header and reply quote, which is what
/// [`attachment_index`] computes. It is railed like every cached line, so
/// the block reads as one unit.
///
/// An outgoing message with an upload still tracked under its id shows the
/// upload bar instead of the download affordance: until the send completes
/// there is no downloadable file on the other end to offer.
#[allow(clippy::too_many_arguments)]
fn append_attachment(
    lines: &mut Vec<Line<'static>>,
    msg: &MessageView,
    grouped: bool,
    media: &MediaState,
    width: u16,
    image_rows_budget: u16,
    theme: &Theme,
    rs: &mut RenderState,
) -> Option<ReservedImage> {
    let line = match media.uploads.get(&msg.id) {
        Some(progress) => file_card_upload_line(&msg.content, progress, theme),
        None => {
            let file = file_id_of(&msg.content).and_then(|id| media.files.get(&id));
            file_card_line(&msg.content, file, theme)
        }
    };
    let line = line?;
    let rail = rail_style(msg, theme);
    let row = place_row(line.spans, width, msg.is_outgoing, rail);
    let at = attachment_index(msg, grouped).min(lines.len());

    // The inline image (design-language §6) replaces that row outright when
    // every gate in the module docs opens; otherwise the §4 line stands.
    if let Some(planned) = plan_inline_image(msg, media, width, image_rows_budget, row.clone(), rs)
    {
        let blank = place_row(Vec::new(), width, msg.is_outgoing, rail);
        for offset in 0..planned.rows {
            lines.insert(at + offset as usize, blank.clone());
        }
        return Some(ReservedImage {
            at,
            rows: planned.rows,
            tag: Rc::new(planned.tag),
        });
    }

    lines.insert(at, row);
    None
}

/// The rows [`append_attachment`] reserved, in indices into the message's
/// own line vector — which is what the block-building loop tags rows by.
struct ReservedImage {
    at: usize,
    rows: u16,
    tag: Rc<ImageTag>,
}

impl ReservedImage {
    fn covers(&self, line_index: usize) -> bool {
        line_index >= self.at && line_index < self.at + self.rows as usize
    }
}

/// A planned image plus how many rows it needs.
struct PlannedImage {
    tag: ImageTag,
    rows: u16,
}

/// Decides whether `msg` gets an inline image this frame, and where it goes.
/// `None` — the common answer — means the §4 card line stands, and every
/// reason for it is silent by design (module docs).
///
/// The image sits in the block's text column: one rail width in on an
/// incoming message, right-aligned to the text edge on an own one. Own
/// messages additionally keep `RECEIPT_COLS` free at that edge, because a
/// photo with no caption ends its block on a reserved row and that is
/// exactly where `append_receipt` puts the tick — an image drawn over it
/// would erase the only confirmation the sender gets.
fn plan_inline_image(
    msg: &MessageView,
    media: &MediaState,
    width: u16,
    image_rows_budget: u16,
    fallback: Line<'static>,
    rs: &mut RenderState,
) -> Option<PlannedImage> {
    let capability = rs.graphics?;
    let MessageContent::Photo { file_id, .. } = &msg.content else {
        return None;
    };
    // An upload still in flight owns this row: it is showing progress on a
    // file that is on its way out, and TDLib reports the local copy as a
    // completed file the whole time, so without this the progress bar would
    // be replaced by the picture the moment the send starts.
    if media.uploads.contains_key(&msg.id) {
        return None;
    }
    let file = media.files.get(file_id)?;
    if !file.is_completed {
        return None;
    }
    let path = file.local_path.as_ref()?;

    let text_cols = width.saturating_sub(RAIL_COLS);
    let text_cols = if msg.is_outgoing {
        text_cols.saturating_sub(RECEIPT_COLS)
    } else {
        text_cols
    };
    let cell = rs.cell_size();
    let footprint =
        rs.images
            .area(msg.id, Some(capability), cell)
            .plan(path, text_cols, image_rows_budget)?;

    // An own block is right-aligned, so its image ends where its text ends:
    // at `text_cols`, which the subtraction above already pulled clear of
    // the receipt gutter and the trailing rail.
    let inset = if msg.is_outgoing {
        text_cols.saturating_sub(footprint.cols)
    } else {
        RAIL_COLS
    };
    Some(PlannedImage {
        rows: footprint.rows,
        tag: ImageTag {
            message_id: msg.id,
            path: path.clone(),
            inset,
            cols: footprint.cols,
            fallback,
        },
    })
}

/// How many rows of `msg`'s cached block precede its attachment: the header
/// (absent when the message groups under the one above) and the reply quote.
/// Everything after that is caption text, which the attachment belongs
/// above.
///
/// This mirrors `message_layout::layout`'s composition order, and shares its
/// one assumption — that a header occupies a single row — with
/// [`slice_grouped`] and the module docs' "Grouped-cache resolution".
fn attachment_index(msg: &MessageView, grouped: bool) -> usize {
    usize::from(!grouped) + usize::from(msg.reply_to.is_some())
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

/// Appends this own message's read-receipt marker **inline**, to the last
/// row the block already has: `⋯` while sending, `✗` on failure (danger),
/// else `✓`/`✓✓` from `last_read_outbox` (spec: "Sent" vs "read"). Only
/// called for `msg.is_outgoing` messages — incoming messages have no receipt
/// of our own to show.
///
/// design-language §3 is explicit that this never occupies a row of its own,
/// and `message_layout::append_marker_inline` guarantees it never widens one
/// either: an own message wraps `RECEIPT_COLS` narrower than the pane
/// precisely so the marker has somewhere to go. A message with no rows at
/// all (impossible: every own message has at least a header) is skipped
/// rather than given one.
fn append_receipt(
    lines: &mut Vec<Line<'static>>,
    msg: &MessageView,
    last_read_outbox: MessageId,
    width: u16,
    theme: &Theme,
) {
    let Some(last) = lines.last_mut() else {
        return;
    };
    let (marker, style) = receipt_marker(msg, last_read_outbox, theme);
    append_marker_inline(last, Span::styled(marker, style), width);
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

/// Renders the in-chat search query line with a reverse-video cursor cell,
/// mirroring `view::chat_list::draw_filter_input`'s cursor treatment (this
/// file's own `InputField`-editing UI, same shape, distinct label since
/// `/` here means "search this chat" rather than "filter the chat list").
fn draw_search_input(area: Rect, search: &ChatSearchState, theme: &Theme, f: &mut Frame) {
    let text = &search.input.text;
    let chars: Vec<char> = text.chars().collect();
    let cursor_chars = text[..search.input.cursor].chars().count().min(chars.len());
    let base = Style::new().fg(theme.text);
    let cursor_style = Style::new().fg(theme.surface).bg(theme.accent);

    let mut spans = vec![Span::styled("search: ", Style::new().fg(theme.text_muted))];
    let before: String = chars[..cursor_chars].iter().collect();
    if !before.is_empty() {
        spans.push(Span::styled(before, base));
    }
    if cursor_chars < chars.len() {
        spans.push(Span::styled(chars[cursor_chars].to_string(), cursor_style));
        let after: String = chars[cursor_chars + 1..].iter().collect();
        if !after.is_empty() {
            spans.push(Span::styled(after, base));
        }
    } else {
        spans.push(Span::styled(" ", cursor_style));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
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
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tgt_core::app::{AppState, Screen};
    use tgt_core::effect::TelemetryMode;
    use tgt_core::model::entity::{EntityKind, FormattedText, TextEntity};
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
    use tgt_core::state::search::ChatSearchState;
    use tgt_core::state::toasts::ToastState;
    use tgt_core::td::update::{AuthPhase, ConnectionPhase};

    use super::*;
    use crate::render::image::{Capability, MAX_IMAGE_ROWS};

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
            pending_view: None,
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
            crash_reports_available: false,
            telemetry_salt: [0u8; 32],
            now: Millis(0),
        }
    }

    fn render_to_string(width: u16, height: u16, state: &AppState) -> String {
        let theme = Theme::default_dark();
        let mut rs = RenderState::new(None);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                draw(area, state, &theme, f, &mut rs, &mut HitMap::new());
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

    // --- search highlighting and query bar (T47) --------------------------

    /// Active `Focus::ChatSearch` with three hits and a query bar showing
    /// "pr": the search bar renders at the top of the pane, the current hit
    /// (msg 5, `current_hit == 1`) is bold-warning on its first line, and
    /// the other two hits (msg 2, msg 9) get the subtler `surface_raised`
    /// tint across their lines. Fixed state, so the snapshot is stable.
    #[test]
    fn search_active_with_current_and_other_hits_120x40() {
        let mut convo = conversation(mixed_history(), Scroll::Bottom);
        convo.search_hits = vec![MessageId(9), MessageId(5), MessageId(2)];
        let mut state = fixture_state(Some(convo));
        state.focus = FocusStack::new(Focus::Composer);
        state.focus.push(Focus::ChatSearch);
        state.chat_search = Some(ChatSearchState {
            input: InputField {
                text: "pr".to_string(),
                cursor: 2,
            },
            current_hit: 1,
            in_flight: false,
        });

        let rendered = render_to_string(120, 40, &state);
        assert!(
            rendered.contains("search: pr"),
            "query bar missing:\n{rendered}"
        );
        insta::assert_snapshot!(rendered);
    }

    /// Search inactive (`chat_search: None`, the default fixture) must look
    /// pixel-identical to the pre-T47 render: no query bar, no tint on any
    /// message, even though this conversation's messages are the same as
    /// `mixed_grouped_history_120x40`'s.
    #[test]
    fn search_inactive_matches_pre_search_render() {
        let with_hits_but_inactive = {
            let mut convo = conversation(mixed_history(), Scroll::Bottom);
            // A stale `search_hits` list from a closed search (module docs:
            // `search::close` clears it, but this proves the highlight seam
            // itself also gates on `chat_search.is_some()`, not just
            // `search_hits` being non-empty).
            convo.search_hits = vec![MessageId(9), MessageId(5), MessageId(2)];
            fixture_state(Some(convo))
        };
        let baseline = fixture_state(Some(conversation(mixed_history(), Scroll::Bottom)));

        assert_eq!(
            render_to_string(120, 40, &with_hits_but_inactive),
            render_to_string(120, 40, &baseline),
            "stale search_hits must not paint a highlight once chat_search is None"
        );
    }

    /// `render_to_string`'s snapshot only captures cell text, not `Style` —
    /// this asserts the styles directly, driving `build_window` the same
    /// way the other bottom-up-fill unit tests do. Msg 8 (the doc message)
    /// is the current hit and ungrouped (its own header plus the two-line
    /// file card = a 3-line block), so it exercises "first line bold
    /// warning, later lines tinted but not bold/warning". Msg 2 is the
    /// other hit: tinted throughout, never the current-hit emphasis. Msg 1
    /// is not a hit at all and must carry neither.
    #[test]
    fn search_hit_styles_distinguish_current_from_other_hits() {
        let mut convo = conversation(mixed_history(), Scroll::Bottom);
        convo.search_hits = vec![MessageId(8), MessageId(2)];
        let search = ChatSearchState {
            input: InputField::default(),
            current_hit: 0,
            in_flight: false,
        };
        let mut rs = RenderState::new(None);
        let rows = build_window(
            &convo,
            &MediaState::default(),
            0,
            120,
            40,
            &theme(),
            &mut rs,
            Some(&search),
        );

        let current_rows: Vec<&WindowRow> = rows
            .iter()
            .filter(|r| r.message_id == Some(MessageId(8)))
            .collect();
        assert!(
            current_rows.len() > 1,
            "expected msg 8's block to span multiple lines: {} rows",
            current_rows.len()
        );
        for span in &current_rows[0].line.spans {
            assert_eq!(span.style.bg, Some(theme().surface_raised));
            assert_eq!(span.style.fg, Some(theme().warning));
            assert!(span.style.add_modifier.contains(Modifier::BOLD));
        }
        // Later lines of the current hit's own block are tinted like any
        // other hit, but not bold — bold is `emphasize`'s signal, reserved
        // for the block's first line. (Not asserting `fg != warning` here:
        // `theme.sender_palette[2]` — Bob's rail color, since msg 8 is
        // Bob's — happens to equal `theme.warning`'s RGB by coincidence of
        // the built-in palette, so fg-equality is not a reliable
        // "was this emphasized" signal; the BOLD modifier is.)
        for row in &current_rows[1..] {
            for span in &row.line.spans {
                assert_eq!(span.style.bg, Some(theme().surface_raised));
                assert!(!span.style.add_modifier.contains(Modifier::BOLD));
            }
        }

        let other_rows: Vec<&WindowRow> = rows
            .iter()
            .filter(|r| r.message_id == Some(MessageId(2)))
            .collect();
        assert!(!other_rows.is_empty());
        for row in &other_rows {
            for span in &row.line.spans {
                assert_eq!(span.style.bg, Some(theme().surface_raised));
                assert!(!span.style.add_modifier.contains(Modifier::BOLD));
            }
        }

        let non_hit_rows: Vec<&WindowRow> = rows
            .iter()
            .filter(|r| r.message_id == Some(MessageId(1)))
            .collect();
        assert!(!non_hit_rows.is_empty());
        for row in &non_hit_rows {
            for span in &row.line.spans {
                assert_ne!(span.style.bg, Some(theme().surface_raised));
            }
        }
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

    /// A file message renders exactly one attachment row, above its
    /// caption, and the file is named once (the T62 defect: a cached
    /// identity line plus a per-frame status line named it twice).
    #[test]
    fn attachment_is_one_row_above_the_caption() {
        let mut msg = doc_msg(1, Sender::User(UserId(2)), "Bob", 0, "spec.pdf", 2_400);
        if let MessageContent::Document { caption, .. } = &mut msg.content {
            *caption = FormattedText {
                text: "have a look".to_string(),
                entities: Vec::new(),
            };
        }
        let convo = conversation(vec![msg], Scroll::Bottom);
        let mut rs = RenderState::new(None);

        let rows = build_window(
            &convo,
            &MediaState::default(),
            0,
            60,
            10,
            &theme(),
            &mut rs,
            None,
        );
        let texts: Vec<String> = rows
            .iter()
            .filter(|r| r.message_id.is_some())
            .map(|r| r.line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        assert_eq!(
            texts.iter().filter(|t| t.contains("spec.pdf")).count(),
            1,
            "the file must be named exactly once: {texts:#?}"
        );
        let attachment = texts
            .iter()
            .position(|t| t.contains("spec.pdf"))
            .expect("no attachment row");
        let caption = texts
            .iter()
            .position(|t| t.contains("have a look"))
            .expect("no caption row");
        assert!(
            attachment < caption,
            "the attachment belongs above its caption: {texts:#?}"
        );
        assert!(
            texts[attachment].starts_with("▏ "),
            "the attachment row carries the block's rail: {:?}",
            texts[attachment]
        );
    }

    /// design-language §5: the selected message gets a `surface_raised`
    /// band across every one of its rows, and nothing else — no border, no
    /// inverse block, no change to the body's foreground.
    #[test]
    fn selected_message_gets_a_raised_background_band() {
        let alice = Sender::User(UserId(1));
        let mut convo = conversation(
            vec![
                text_msg(1, alice, "Alice", false, 0, "first", None),
                text_msg(
                    2,
                    Sender::User(UserId(2)),
                    "Bob",
                    false,
                    400,
                    "second",
                    None,
                ),
            ],
            Scroll::Bottom,
        );
        convo.selection = Some(tgt_core::state::selection::SelectionState {
            message_id: MessageId(2),
            chips: Vec::new(),
            chip_cursor: 0,
            chip_scroll: 0,
        });
        let mut rs = RenderState::new(None);

        let width = 40;
        let rows = build_window(
            &convo,
            &MediaState::default(),
            0,
            width,
            10,
            &theme(),
            &mut rs,
            None,
        );

        for row in rows.iter().filter(|r| r.message_id == Some(MessageId(2))) {
            assert_eq!(
                row.line.width(),
                width as usize,
                "the band spans the row: {:?}",
                row.line
            );
            for span in &row.line.spans {
                assert_eq!(span.style.bg, Some(theme().surface_raised));
            }
        }
        for row in rows.iter().filter(|r| r.message_id == Some(MessageId(1))) {
            for span in &row.line.spans {
                assert_ne!(span.style.bg, Some(theme().surface_raised));
            }
        }
    }

    // --- inline images (T63, design-language §6) --------------------------

    static TEST_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

    const PHOTO_FILE: FileId = FileId(9);

    /// Writes a synthetic PNG under the OS temp dir and returns its path.
    /// `tgt-ui` carries no `tempfile` dev-dependency (see
    /// `crates/ui/Cargo.toml`), so this mirrors `render::image`'s own tests:
    /// a counter-suffixed name, so tests running in parallel cannot collide.
    fn scratch_png(width: u32, height: u32) -> PathBuf {
        let n = TEST_FILE_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        let path = std::env::temp_dir().join(format!("tgt-ui-conversation-image-{n}.png"));
        let img = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 200])
        });
        image::DynamicImage::ImageRgb8(img)
            .save(&path)
            .expect("write test PNG");
        path
    }

    fn photo_msg(id: i64, width: u32, height: u32) -> MessageView {
        MessageView {
            id: MessageId(id),
            chat_id: CHAT,
            sender: Sender::User(UserId(2)),
            sender_name: "Bob".to_string(),
            is_outgoing: false,
            date: BASE_DATE,
            content: MessageContent::Photo {
                file_id: PHOTO_FILE,
                width,
                height,
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

    /// One photo message, plus the `MediaState` entry that decides whether
    /// it can render inline: `Some(path)` is a finished download, `None` is
    /// a photo nobody has fetched yet.
    fn photo_state(pixels: (u32, u32), downloaded: Option<&Path>) -> AppState {
        let mut state = fixture_state(Some(conversation(
            vec![photo_msg(1, pixels.0, pixels.1)],
            Scroll::Bottom,
        )));
        state.media.files.insert(
            PHOTO_FILE,
            FileSnapshot {
                id: PHOTO_FILE,
                expected_size: 4_096,
                downloaded_size: if downloaded.is_some() { 4_096 } else { 0 },
                is_downloading: false,
                is_completed: downloaded.is_some(),
                local_path: downloaded.map(Path::to_path_buf),
            },
        );
        state
    }

    /// Drives the real `draw` against a caller-owned `RenderState`, so a
    /// test can inspect what the frame left behind.
    fn draw_frame(
        width: u16,
        height: u16,
        state: &AppState,
        rs: &mut RenderState,
    ) -> (String, HitMap) {
        let theme = Theme::default_dark();
        let mut hits = HitMap::new();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| draw(f.area(), state, &theme, f, rs, &mut hits))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut out = String::with_capacity(buffer.content.len() + buffer.area.height as usize);
        for row in buffer.content.chunks(buffer.area.width as usize) {
            for cell in row {
                out.push_str(cell.symbol());
            }
            out.push('\n');
        }
        (out, hits)
    }

    /// How many of `height`'s rows resolve to `message_id` when clicked.
    fn clickable_rows(hits: &HitMap, height: u16, message_id: MessageId) -> usize {
        (0..height)
            .filter(|&y| hits.target_at(1, y) == Some(HitTarget::Message(message_id)))
            .count()
    }

    /// The headline of design-language §6: a downloaded photo on a terminal
    /// with a protocol *is* the picture — the §4 line is gone, rows are
    /// reserved in its place, and every one of them still belongs to the
    /// message as far as a click is concerned.
    #[test]
    fn image_rows_replace_the_card_when_downloaded_and_supported() {
        let path = scratch_png(200, 100);
        let state = photo_state((200, 100), Some(&path));

        let mut plain = RenderState::new(None);
        let (without, plain_hits) = draw_frame(60, 20, &state, &mut plain);
        assert!(
            without.contains("🖼 photo · 200×100 · ⏎ open"),
            "the §4 line is what a terminal without a protocol shows:\n{without}"
        );

        let mut rs = RenderState::new(Some(Capability::Kitty));
        let (with, hits) = draw_frame(60, 20, &state, &mut rs);
        assert!(
            !with.contains("🖼"),
            "the image replaces the card outright, it does not join it:\n{with}"
        );
        assert!(!with.contains("⏎ open"), "…affordance included:\n{with}");
        assert_eq!(rs.images.len(), 1, "the message got an ImageArea");

        // The rows it took: more than the one row the card had, capped by
        // MAX_IMAGE_ROWS, and all of them clickable.
        let reserved = clickable_rows(&hits, 20, MessageId(1))
            - clickable_rows(&plain_hits, 20, MessageId(1))
            + 1;
        assert!(
            (2..=MAX_IMAGE_ROWS as usize).contains(&reserved),
            "a 200x100 photo should occupy several bounded rows, got {reserved}"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// The two bounds design-language §6 puts on the picture: the hard
    /// `MAX_IMAGE_ROWS` cap, and the pane it has to fit inside — a photo may
    /// never squeeze the conversation it is part of off the screen.
    #[test]
    fn reserved_rows_are_bounded_by_the_cap_and_by_the_pane() {
        let path = scratch_png(100, 4_000);
        let state = photo_state((100, 4_000), Some(&path));
        let convo = state.conversations.get(&CHAT).unwrap();

        for (pane_height, bound) in [(40u16, MAX_IMAGE_ROWS), (8, 8)] {
            let mut rs = RenderState::new(Some(Capability::Kitty));
            let rows = build_window(
                convo,
                &state.media,
                0,
                60,
                pane_height,
                &theme(),
                &mut rs,
                None,
            );
            let reserved = rows.iter().filter(|r| r.image.is_some()).count();
            assert!(
                reserved > 0 && reserved <= bound as usize,
                "pane height {pane_height}: reserved {reserved} rows, bound is {bound}"
            );
            assert!(
                rows.iter()
                    .filter(|r| r.image.is_some())
                    .all(|r| r.message_id == Some(MessageId(1))),
                "a reserved row belongs to its message, or a click on the photo hits nothing"
            );
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn falls_back_to_card_when_unsupported() {
        let path = scratch_png(200, 100);
        let state = photo_state((200, 100), Some(&path));

        let mut rs = RenderState::new(None);
        let (rendered, _) = draw_frame(60, 20, &state, &mut rs);

        assert!(
            rendered.contains("🖼 photo · 200×100 · ⏎ open"),
            "a downloaded photo with no protocol keeps its §4 line:\n{rendered}"
        );
        assert!(
            rs.images.is_empty(),
            "no capability means nothing is ever planned, not even an empty slot"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Card and picture are alternatives, never both, and the choice is per
    /// message: three photos, one of them fetched, must produce exactly two
    /// `⏎ download` lines and one run of reserved rows. Two card lines above
    /// a picture is what two un-fetched photos look like — the bug it would
    /// be is a *single* message rendering as both, which this pins shut.
    #[test]
    fn each_photo_renders_as_a_card_or_as_a_picture_and_never_as_both() {
        let path = scratch_png(200, 100);
        let mut fetched = photo_msg(1, 200, 100);
        fetched.content = MessageContent::Photo {
            file_id: PHOTO_FILE,
            width: 200,
            height: 100,
            caption: FormattedText {
                text: String::new(),
                entities: Vec::new(),
            },
        };
        // Two more photos on files nobody has fetched, so they keep their
        // §4 line while the first becomes a picture.
        let pending_file = FileId(10);
        let mut pending_msgs = Vec::new();
        for id in [2, 3] {
            let mut msg = photo_msg(id, 640, 480);
            msg.content = MessageContent::Photo {
                file_id: pending_file,
                width: 640,
                height: 480,
                caption: FormattedText {
                    text: String::new(),
                    entities: Vec::new(),
                },
            };
            pending_msgs.push(msg);
        }

        let mut state = fixture_state(Some(conversation(
            std::iter::once(fetched).chain(pending_msgs).collect(),
            Scroll::Bottom,
        )));
        state.media.files.insert(
            PHOTO_FILE,
            FileSnapshot {
                id: PHOTO_FILE,
                expected_size: 4_096,
                downloaded_size: 4_096,
                is_downloading: false,
                is_completed: true,
                local_path: Some(path.clone()),
            },
        );
        state.media.files.insert(
            pending_file,
            FileSnapshot {
                id: pending_file,
                expected_size: 4_096,
                downloaded_size: 0,
                is_downloading: false,
                is_completed: false,
                local_path: None,
            },
        );

        let mut rs = RenderState::new(Some(Capability::Kitty));
        let (rendered, _) = draw_frame(60, 30, &state, &mut rs);

        assert_eq!(
            rendered.matches("⏎ download").count(),
            2,
            "one card per un-fetched photo, and none for the fetched one:\n{rendered}"
        );
        assert_eq!(
            rendered.matches("640×480").count(),
            2,
            "both un-fetched photos keep their §4 identity line:\n{rendered}"
        );
        assert!(
            !rendered.contains("200×100"),
            "the fetched photo is the picture, so its card is gone entirely:\n{rendered}"
        );

        // And structurally: the reserved rows all belong to the one message
        // that became a picture, and that message contributed no card row.
        let convo = state.conversations.get(&CHAT).unwrap();
        let mut rs = RenderState::new(Some(Capability::Kitty));
        let rows = build_window(convo, &state.media, 0, 60, 30, &theme(), &mut rs, None);
        assert!(
            rows.iter()
                .filter(|r| r.image.is_some())
                .all(|r| r.message_id == Some(MessageId(1))),
            "only the fetched photo reserves image rows"
        );
        assert!(
            rows.iter()
                .filter(|r| r.message_id == Some(MessageId(1)))
                .all(|r| !r.line.to_string().contains("⏎")),
            "the message drawn as a picture contributes no affordance row"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// A picture is pixels, not cells: a rect that runs past the pane draws
    /// over the sidebar and the terminal keeps drawing it there, because
    /// nothing in the conversation pane will ever rewrite those cells. So
    /// the rect has to be clipped, in both axes, before it is handed over.
    #[test]
    fn a_placement_never_reaches_outside_its_pane() {
        let pane = Rect::new(30, 4, 40, 12);
        for (inset, cols, top, bottom) in [
            (0u16, 40u16, 0u16, 11u16), // exactly the pane
            (0, 200, 0, 11),            // absurdly wide
            (35, 20, 0, 11),            // inset far enough right to overhang
            (0, 40, 0, 200),            // taller than the pane
            (39, 1, 11, 11),            // the last cell
            (60, 10, 0, 3),             // inset past the right edge entirely
        ] {
            let rect = placement_rect(pane, inset, cols, top, bottom);
            assert!(
                rect.right() <= pane.right() && rect.bottom() <= pane.bottom(),
                "({inset}, {cols}, {top}, {bottom}) produced {rect:?}, outside {pane:?}"
            );
            assert!(
                rect.x >= pane.x && rect.y >= pane.y,
                "({inset}, {cols}, {top}, {bottom}) produced {rect:?}, before {pane:?}"
            );
        }
    }

    /// A photo that has not been downloaded has no local file to decode, so
    /// the §4 line stands even where the terminal could draw one — and it
    /// still offers the download that would change that.
    #[test]
    fn falls_back_to_card_when_the_file_is_not_downloaded() {
        let state = photo_state((200, 100), None);

        let mut rs = RenderState::new(Some(Capability::Kitty));
        let (rendered, _) = draw_frame(60, 20, &state, &mut rs);

        assert!(
            rendered.contains("🖼 photo · 200×100 · ⏎ download"),
            "an un-fetched photo shows the card and its affordance:\n{rendered}"
        );
    }

    /// One way the pane's content can move: a label, what it changes about
    /// the state, and the pane the next frame would draw into.
    struct MoveCase(&'static str, fn(&mut AppState), Rect);

    /// Ghosting (spec §8.3): protocol cells are terminal-side state, so an
    /// image whose rows are about to move has to be dropped rather than
    /// redrawn elsewhere. Every input that moves them is fingerprinted; this
    /// checks each one.
    #[test]
    fn scroll_invalidates_placed_images() {
        let path = scratch_png(200, 100);
        let pane = Rect::new(0, 0, 60, 20);
        let state = photo_state((200, 100), Some(&path));

        let mut rs = RenderState::new(Some(Capability::Kitty));
        draw_frame(60, 20, &state, &mut rs);
        assert_eq!(rs.images.len(), 1, "the first frame placed an image");

        // An identical frame keeps it: invalidating on every draw would
        // re-encode every image at the draw rate for no reason at all.
        draw_frame(60, 20, &state, &mut rs);
        assert_eq!(rs.images.len(), 1, "an unchanged frame keeps its images");

        // Everything that moves a message's rows, each starting from a
        // freshly drawn frame that has an image placed.
        let cases = [
            MoveCase(
                "a moved scroll anchor",
                |s| {
                    s.conversations.get_mut(&CHAT).unwrap().scroll = Scroll::At {
                        message_id: MessageId(1),
                        line_offset: 2,
                    }
                },
                pane,
            ),
            // A new message pushes everything above it up even though the
            // anchor (`Scroll::Bottom`) itself never changed — the case a
            // naive "did the anchor move" check misses, and the one a user
            // hits most often.
            MoveCase(
                "an arriving message",
                |s| {
                    s.conversations
                        .get_mut(&CHAT)
                        .unwrap()
                        .messages
                        .push_back(text_msg(
                            2,
                            Sender::User(UserId(1)),
                            "Alice",
                            false,
                            900,
                            "nice shot",
                            None,
                        ))
                },
                pane,
            ),
            MoveCase("a resized pane", |_| {}, Rect::new(0, 0, 40, 20)),
            MoveCase("a theme switch", |s| s.theme_generation += 1, pane),
            MoveCase("closing the chat", |s| s.open_chat = None, pane),
        ];

        for MoveCase(label, mutate, moved_pane) in cases {
            let mut rs = RenderState::new(Some(Capability::Kitty));
            draw_frame(60, 20, &state, &mut rs);
            assert_eq!(rs.images.len(), 1, "{label}: nothing placed to begin with");

            let mut moved = photo_state((200, 100), Some(&path));
            mutate(&mut moved);
            rs.note_viewport(&moved, moved_pane);
            assert!(rs.images.is_empty(), "{label} must drop placed images");
        }

        let _ = std::fs::remove_file(&path);
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
        let mut rs = RenderState::new(None);

        let rows = build_window(
            &convo,
            &MediaState::default(),
            0,
            20,
            5,
            &theme(),
            &mut rs,
            None,
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
        let mut rs = RenderState::new(None);

        let rows = build_window(
            &convo,
            &MediaState::default(),
            0,
            40,
            10,
            &theme(),
            &mut rs,
            None,
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
        let mut rs = RenderState::new(None);

        let rows = build_window(
            &convo,
            &MediaState::default(),
            0,
            40,
            40,
            &theme(),
            &mut rs,
            None,
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
        let mut rs = RenderState::new(None);

        let full = build_window(
            &convo_no_offset,
            &MediaState::default(),
            0,
            40,
            10,
            &theme(),
            &mut rs,
            None,
        );
        let trimmed = build_window(
            &convo_offset,
            &MediaState::default(),
            0,
            40,
            10,
            &theme(),
            &mut rs,
            None,
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
        let mut rs = RenderState::new(None);

        let rows = build_window(
            &convo,
            &MediaState::default(),
            0,
            40,
            10,
            &theme(),
            &mut rs,
            None,
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

    /// design-language §3: a receipt is appended to the last line of its
    /// message, never given a row. The regression this replaces put every
    /// tick on its own row, so the pane edge grew a column of them.
    #[test]
    fn receipts_render_inline_on_the_message_row() {
        let me = Sender::User(UserId(3));
        let convo = conversation(
            vec![text_msg(1, me, "You", true, 0, "on it", None)],
            Scroll::Bottom,
        );
        let mut rs = RenderState::new(None);

        let rows = build_window(
            &convo,
            &MediaState::default(),
            0,
            40,
            10,
            &theme(),
            &mut rs,
            None,
        );
        let texts: Vec<String> = rows
            .iter()
            .map(|r| r.line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        assert!(
            texts.iter().any(|t| t.contains("on it ✓ ▏")),
            "the tick belongs on the text's own row, ahead of the rail: {texts:#?}"
        );
        assert!(
            !texts.iter().any(|t| t.trim().starts_with('✓')),
            "no row may consist of a receipt alone: {texts:#?}"
        );
        for (row, text) in rows.iter().zip(&texts) {
            assert!(
                row.line.width() <= 40,
                "the receipt widened a row past the pane: {text:?} ({} columns)",
                row.line.width()
            );
        }
    }

    /// The inline marker still fits when the message's last line is long
    /// enough to be wrapped: the reserved `RECEIPT_COLS` gutter is what
    /// guarantees it, at 80 and at 140 columns alike.
    #[test]
    fn receipt_fits_within_the_pane_at_every_width() {
        let me = Sender::User(UserId(3));
        // Deliberately unbroken-ish filler so the wrap lands near the edge.
        let body = "status update ".repeat(20);
        let convo = conversation(
            vec![text_msg(1, me, "You", true, 0, body.trim(), None)],
            Scroll::Bottom,
        );
        let mut rs = RenderState::new(None);

        for width in [40u16, 80, 140] {
            let rows = build_window(
                &convo,
                &MediaState::default(),
                0,
                width,
                20,
                &theme(),
                &mut rs,
                None,
            );
            let marker_rows = rows
                .iter()
                .filter(|r| {
                    r.line
                        .spans
                        .iter()
                        .any(|s| s.content.as_ref() == "✓" || s.content.as_ref() == "✓✓")
                })
                .count();
            assert_eq!(marker_rows, 1, "exactly one row carries the receipt");
            for row in &rows {
                assert!(
                    row.line.width() <= width as usize,
                    "width {width}: row overflows at {} columns",
                    row.line.width()
                );
            }
        }
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
        let mut rs = RenderState::new(None);

        let rows = build_window(
            &convo,
            &MediaState::default(),
            0,
            40,
            10,
            &theme(),
            &mut rs,
            None,
        );
        let texts: Vec<String> = rows
            .iter()
            .map(|r| r.line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        assert_eq!(
            texts.iter().filter(|t| t.contains("on it ✓✓ ▏")).count(),
            1,
            "the read message shows ✓✓ inline: {texts:#?}"
        );
        assert_eq!(
            texts.iter().filter(|t| t.contains("done ✓ ▏")).count(),
            1,
            "the unread-by-peer message shows a single ✓ inline: {texts:#?}"
        );
        assert!(
            !texts.iter().any(|t| t.contains("done ✓✓")),
            "only the read message should show ✓✓: {texts:#?}"
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
        let mut rs = RenderState::new(None);
        let area = Rect::new(0, 0, 80, 24);

        let (oldest, newest) = visible_range(&state, area, &mut rs).unwrap();
        assert_eq!(newest, MessageId(10));
        assert!(oldest <= newest);
    }

    #[test]
    fn visible_range_none_without_open_chat() {
        let state = fixture_state(None);
        let mut rs = RenderState::new(None);
        assert!(visible_range(&state, Rect::new(0, 0, 80, 24), &mut rs).is_none());
    }

    #[test]
    fn visible_range_none_for_empty_conversation() {
        let state = fixture_state(Some(conversation(Vec::new(), Scroll::Bottom)));
        let mut rs = RenderState::new(None);
        assert!(visible_range(&state, Rect::new(0, 0, 80, 24), &mut rs).is_none());
    }

    // --- sub-row hit targets (architecture §7.5.1, T77) --------------------

    fn spoiler_msg(id: i64) -> MessageView {
        MessageView {
            id: MessageId(id),
            chat_id: CHAT,
            sender: Sender::User(UserId(1)),
            sender_name: "Alice".to_string(),
            is_outgoing: false,
            date: BASE_DATE,
            content: MessageContent::Text(FormattedText {
                text: "before secret after".to_string(),
                entities: vec![TextEntity {
                    offset_utf16: 7,
                    length_utf16: 6,
                    kind: EntityKind::Spoiler,
                }],
            }),
            reply_to: None,
            send_state: SendState::Sent,
            reactions: Vec::new(),
            caps: MessageCaps::default(),
            is_edited: false,
        }
    }

    /// A masked spoiler block gets a narrower hit target than the row it
    /// sits in, layered over the row-wide `Message` target rather than
    /// replacing it (`HitMap::target_at`'s existing last-pushed-wins rule).
    /// Locates the block by its actual rendered `'█'` glyphs rather than a
    /// hardcoded column, so the test fails if the column math is ever wrong
    /// in either direction.
    #[test]
    fn spoiler_click_target_is_narrower_than_the_message_row() {
        let state = fixture_state(Some(conversation(vec![spoiler_msg(1)], Scroll::Bottom)));
        let mut rs = RenderState::new(None);
        let (rendered, hits) = draw_frame(60, 20, &state, &mut rs);

        let row = rendered
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains('█'))
            .map(|(i, _)| i as u16)
            .unwrap_or_else(|| panic!("expected a masked spoiler block on screen:\n{rendered}"));
        let line_text = rendered.lines().nth(row as usize).unwrap();
        let byte_idx = line_text.find('█').unwrap();
        // Byte offset -> display column: the row can carry the rail glyph
        // (multi-byte, one column) before the block, so a byte count would
        // overshoot.
        let block_col = line_text[..byte_idx].width() as u16;

        assert_eq!(
            hits.target_at(block_col, row),
            Some(HitTarget::Spoiler(MessageId(1))),
            "row {row} col {block_col} (a masked block cell):\n{rendered}"
        );
        // Column 0 is the rail, never inside the block: the row-wide
        // `Message` target must still win there.
        assert_eq!(
            hits.target_at(0, row),
            Some(HitTarget::Message(MessageId(1))),
            "row {row} col 0 (outside the block):\n{rendered}"
        );
    }

    /// Left/right routing on `Spoiler`/`ReplyQuote` is `app.rs`'s job
    /// (tested there); this only proves the *view* hands back the right
    /// target at the right cells in the first place, using the message
    /// `mixed_history` already carries a reply on (msg 4 quotes msg 3).
    #[test]
    fn reply_quote_click_target_names_the_quoted_message() {
        let state = fixture_state(Some(conversation(mixed_history(), Scroll::Bottom)));
        let mut rs = RenderState::new(None);
        let (rendered, hits) = draw_frame(60, 30, &state, &mut rs);

        let row = rendered
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains('↳'))
            .map(|(i, _)| i as u16)
            .unwrap_or_else(|| panic!("expected the reply-quote line on screen:\n{rendered}"));

        assert_eq!(
            hits.target_at(5, row),
            Some(HitTarget::ReplyQuote {
                containing: MessageId(4),
                quoted: MessageId(3),
            }),
            "row {row}:\n{rendered}"
        );
    }
}
