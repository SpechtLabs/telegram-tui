//! Everything a draw needs that outlives the frame it is drawn in
//! (architecture.md §4.9.1): the layout cache, the per-message inline-image
//! handles, and the terminal's graphics capability.
//!
//! They travel as one value rather than three parameters because two of them
//! are invalidated by the same events. A resize throws away cached lines
//! *and* placed images; so does a theme change; so does the viewport moving
//! under them. Threading one `&mut RenderState` through `view` → `root` →
//! `conversation` means those rules live in one place instead of being
//! restated at every call site that happens to hold both.
//!
//! `graphics` arrives from outside: `tgt-app` probes the terminal once at
//! startup (`crates/app/src/graphics.rs`) and maps its `GraphicsProtocol`
//! into [`Capability`] when it builds this struct. `tgt-ui` never reads the
//! environment — see `render::image`'s module docs for why that boundary
//! exists.
//!
//! ## Ghosting
//!
//! Kitty and iTerm2 placements are terminal-side state addressed by cell,
//! not characters in ratatui's buffer. Redrawing a cell as a blank does not
//! reliably remove the pixels an earlier frame put there, so an image whose
//! rows have moved has to be invalidated rather than merely redrawn
//! elsewhere — otherwise fragments of it smear across whatever scrolled into
//! its old position (spec §8.3).
//!
//! [`RenderState::note_viewport`] is the one guard for that, and it is
//! deliberately blunt: it fingerprints everything that can move a message's
//! rows — the pane rect, which chat is open, the scroll anchor, the newest
//! loaded message and how many are loaded, and the theme generation — and
//! drops every placed image the moment any of it changes. Being wrong in the
//! cautious direction costs a re-encode of a handful of images; being wrong
//! in the other direction leaves garbage on the user's screen that only a
//! full repaint clears.
//!
//! ## Erasing, as opposed to forgetting
//!
//! Dropping an [`ImageArea`] drops *our* handle. It does not tell the
//! terminal to stop drawing what it was already told to draw, and it cannot:
//! the picture lives in the terminal's own layer, keyed by the cells it was
//! placed over. Those cells are only reclaimed when something writes to them
//! — and ratatui, diffing, does not write a cell it believes is unchanged.
//! So "invalidate" on its own means "stop drawing it", which is not "erase
//! it", and the difference is visible as fragments that survive scrolling
//! until an unrelated event (a theme switch restyling every cell, say)
//! happens to force a full repaint.
//!
//! [`RenderState::take_repaint_request`] closes that gap. Whenever the set of
//! placed images changes — a slot created, a sweep dropping one, a viewport
//! fingerprint moving, a resize, a screen without a conversation pane — the
//! flag is raised, and the runtime loop answers it with `Terminal::clear()`
//! and a second draw, which rewrites every cell and therefore reclaims every
//! one an image was ever placed over.
//!
//! `ratatui-image` 11.0.6 exposes no protocol-level delete (its Kitty backend
//! transmits with `U=1` and places via unicode placeholders precisely so that
//! kitty reclaims the placement when the placeholder characters go away; its
//! iTerm2 backend has no delete at all), so a full repaint is not merely the
//! conservative option here, it is the mechanism both protocols are built
//! around.

use std::collections::HashMap;

use ratatui::layout::Rect;
use tgt_core::app::AppState;
use tgt_core::model::ids::{ChatId, MessageId};
use tgt_core::state::conversation::Scroll;

use crate::render::cache::LayoutCache;
use crate::render::image::{Capability, CellSize, ImageArea};

/// The draw path's state between frames. See the module docs.
pub struct RenderState {
    pub cache: LayoutCache,
    /// Per-message [`ImageArea`]s. Protocol cells must be invalidated on
    /// scroll or resize or they ghost (spec §8.3).
    pub images: ImageStore,
    /// `None` on terminals without a graphics protocol, and whenever
    /// `[app].inline_images` is off: every photo then falls back to the
    /// one-line card (docs/design-language.md §4).
    pub graphics: Option<Capability>,
    /// The terminal's measured cell size, for sizing inline images. See
    /// [`CellSize`] for why a wrong one is not a cosmetic problem.
    cell: CellSize,
    /// What the last drawn frame's conversation viewport looked like, for
    /// [`RenderState::note_viewport`].
    viewport: Option<ViewportKey>,
    /// Set when something other than the image store itself demands a full
    /// repaint; see [`RenderState::take_repaint_request`].
    repaint: bool,
}

impl RenderState {
    /// Starts at [`CellSize::FALLBACK`]; `tgt-app` supplies the measured one
    /// with [`RenderState::set_cell_size`] as soon as it has it (and again
    /// after every resize, since a font can change under a running session).
    pub fn new(graphics: Option<Capability>) -> Self {
        RenderState {
            cache: LayoutCache::new(),
            images: ImageStore::new(),
            graphics,
            cell: CellSize::FALLBACK,
            viewport: None,
            repaint: false,
        }
    }

    /// Replaces the cell size images are encoded against. Every already
    /// placed image was encoded for the old one, so this invalidates them
    /// all; a no-op change costs nothing.
    pub fn set_cell_size(&mut self, cell: CellSize) {
        if self.cell == cell {
            return;
        }
        self.cell = cell;
        self.invalidate_images();
    }

    pub fn cell_size(&self) -> CellSize {
        self.cell
    }

    /// Drops every placed image. The next frame re-decodes and re-encodes
    /// from scratch, which is the only way to be sure no protocol cell
    /// outlives the rows it was drawn into.
    pub fn invalidate_images(&mut self) {
        self.images.clear();
        self.viewport = None;
    }

    /// Drops both halves: the cached layouts (they are wrapped at a column
    /// width that no longer applies) and the placed images (their rows have
    /// moved). This is what a resize calls.
    pub fn clear(&mut self) {
        self.cache.clear();
        self.invalidate_images();
    }

    /// Records what the conversation pane is about to draw, invalidating
    /// every placed image if anything about it moved since the last frame.
    /// Called by `view::conversation::draw` before it places anything —
    /// including on the frames where it draws no messages at all, so that
    /// closing a chat drops that chat's images too.
    pub fn note_viewport(&mut self, state: &AppState, pane: Rect) {
        let key = ViewportKey::of(state, pane);
        if self.viewport == Some(key) {
            return;
        }
        self.images.clear();
        self.viewport = Some(key);
    }

    /// Forces the next draw to be a full repaint rather than a diff. For
    /// callers that know a placement is stale for a reason this type cannot
    /// observe; everything this type *can* observe raises it on its own.
    pub fn request_repaint(&mut self) {
        self.repaint = true;
    }

    /// Whether the frame about to be drawn must be preceded by
    /// `Terminal::clear()`, clearing the request as it answers.
    ///
    /// True whenever the set of placed images changed since the last time it
    /// was asked: a slot created, a sweep dropping one, or any of the
    /// wholesale invalidations above. See the module docs' "Erasing, as
    /// opposed to forgetting" for why a diffed frame is not enough.
    ///
    /// Deliberately blunt in the same direction as [`Self::note_viewport`]:
    /// answering `true` when nothing was really stale costs one extra frame
    /// of drawing, answering `false` when something was leaves pixels on the
    /// user's screen that nothing else will remove.
    pub fn take_repaint_request(&mut self) -> bool {
        let placements_changed = self.images.take_changed();
        std::mem::take(&mut self.repaint) || placements_changed
    }
}

impl Default for RenderState {
    fn default() -> Self {
        Self::new(None)
    }
}

/// A fingerprint of everything that can move a message's rows within the
/// conversation pane. Compared for equality only — no field is ever read
/// back — so "what changed" never has to be answered, just "did anything".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ViewportKey {
    pane: Rect,
    chat: Option<ChatId>,
    scroll: Scroll,
    /// The newest loaded message: a new arrival pushes the whole window up
    /// even though the anchor (`Scroll::Bottom`) did not change.
    newest: Option<MessageId>,
    /// Paging in older history, or the window evicting its oldest messages,
    /// changes this without touching `newest`.
    loaded: usize,
    theme_generation: u64,
}

impl ViewportKey {
    fn of(state: &AppState, pane: Rect) -> Self {
        let convo = state
            .open_chat
            .and_then(|chat_id| state.conversations.get(&chat_id));
        ViewportKey {
            pane,
            chat: state.open_chat,
            scroll: convo.map_or(Scroll::Bottom, |c| c.scroll),
            newest: convo.and_then(|c| c.messages.back()).map(|m| m.id),
            loaded: convo.map_or(0, |c| c.messages.len()),
            theme_generation: state.theme_generation,
        }
    }
}

/// The live [`ImageArea`]s, one per message the draw path has considered
/// drawing an inline image for. Created on demand as messages are laid out
/// and dropped once a frame goes by without touching them, so the store
/// holds roughly a viewport's worth of encoded images rather than one per
/// photo ever scrolled past.
#[derive(Default)]
pub struct ImageStore {
    areas: HashMap<MessageId, Slot>,
    /// Whether the *set* of live slots changed since [`Self::take_changed`]
    /// was last asked — the signal `RenderState::take_repaint_request` turns
    /// into a `Terminal::clear()`. Not "whether a slot re-encoded": an image
    /// redrawn in the same cells needs no repaint, an image that appeared,
    /// vanished or moved does.
    changed: bool,
}

struct Slot {
    area: ImageArea,
    /// Whether [`ImageStore::area`] reached this slot since the last
    /// [`ImageStore::sweep`].
    touched: bool,
}

impl ImageStore {
    pub fn new() -> Self {
        ImageStore {
            areas: HashMap::new(),
            changed: false,
        }
    }

    /// The slot for `message_id`, created with `capability` and `cell` if
    /// this is the first time this message has been laid out, and marked as
    /// belonging to the frame being drawn.
    pub fn area(
        &mut self,
        message_id: MessageId,
        capability: Option<Capability>,
        cell: CellSize,
    ) -> &mut ImageArea {
        let mut created = false;
        let slot = self.areas.entry(message_id).or_insert_with(|| {
            created = true;
            Slot {
                area: ImageArea::new(capability, cell),
                touched: false,
            }
        });
        slot.touched = true;
        self.changed |= created;
        &mut slot.area
    }

    /// Drops every slot this frame did not reach — i.e. every message that
    /// scrolled out of the pane. Dropping is the strong form of
    /// `ImageArea::invalidate`: nothing encoded survives to be drawn
    /// somewhere it no longer belongs.
    ///
    /// "Reached" rather than "drew": a photo whose file is broken is
    /// considered every frame and drawn in none of them, and it has to keep
    /// its slot — that slot is where the record of the broken file lives,
    /// and dropping it would make the view retry (and re-reserve rows for)
    /// the same failure on the very next frame.
    pub fn sweep(&mut self) {
        let before = self.areas.len();
        self.areas.retain(|_, slot| slot.touched);
        self.changed |= self.areas.len() != before;
        for slot in self.areas.values_mut() {
            slot.touched = false;
        }
    }

    pub fn clear(&mut self) {
        self.changed |= !self.areas.is_empty();
        self.areas.clear();
    }

    /// Whether the set of live slots changed since the last call, clearing
    /// the record as it answers. See [`ImageStore::changed`].
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    pub fn len(&self) -> usize {
        self.areas.len()
    }

    pub fn is_empty(&self) -> bool {
        self.areas.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use tgt_core::model::ids::MessageId;

    use super::*;

    const KITTY: Option<Capability> = Some(Capability::Kitty);
    const CELL: CellSize = CellSize::FALLBACK;

    #[test]
    fn areas_are_created_once_per_message_and_swept_when_a_frame_skips_them() {
        let mut store = ImageStore::new();
        assert!(store.is_empty());

        store.area(MessageId(1), KITTY, CELL);
        store.area(MessageId(2), KITTY, CELL);
        store.area(MessageId(1), KITTY, CELL);
        assert_eq!(store.len(), 2, "the same message reuses its slot");

        // A frame that only reaches message 2 keeps message 2 and nothing
        // else — but only from the *next* sweep on, since the sweep that
        // ends a frame must not drop what that frame just touched.
        store.sweep();
        assert_eq!(store.len(), 2, "a sweep never drops the frame it ends");
        store.area(MessageId(2), KITTY, CELL);
        store.sweep();
        assert_eq!(store.len(), 1);
        assert!(store.areas.contains_key(&MessageId(2)));

        store.clear();
        assert!(store.is_empty());
    }

    /// The repaint signal is raised by every way the set of placed images
    /// can change, and by nothing else. A frame that redraws the same images
    /// in the same cells must not ask for a clear — that would make every
    /// frame a full repaint and the request meaningless.
    #[test]
    fn every_change_to_the_placed_set_asks_for_a_repaint_and_a_steady_frame_does_not() {
        let mut store = ImageStore::new();
        assert!(!store.take_changed(), "an empty store has changed nothing");

        store.area(MessageId(1), KITTY, CELL);
        assert!(store.take_changed(), "a new placement");
        assert!(!store.take_changed(), "taking clears the request");

        // The same message again, and a sweep that drops nothing: the
        // steady state of a conversation nobody is scrolling.
        store.area(MessageId(1), KITTY, CELL);
        store.sweep();
        assert!(
            !store.take_changed(),
            "redrawing the same image in the same place needs no repaint"
        );

        // A frame that does not reach message 1 sweeps it away.
        store.sweep();
        assert_eq!(store.len(), 0);
        assert!(store.take_changed(), "a sweep that dropped a slot");

        store.area(MessageId(2), KITTY, CELL);
        let _ = store.take_changed();
        store.clear();
        assert!(store.take_changed(), "a wholesale clear");
        store.clear();
        assert!(
            !store.take_changed(),
            "clearing an already empty store changes nothing"
        );
    }

    /// `RenderState` is where the loop asks, so its answer has to fold in
    /// both the store's own changes and the invalidations it performs on the
    /// store's behalf.
    #[test]
    fn render_state_reports_its_invalidations_as_repaint_requests() {
        let mut rs = RenderState::new(KITTY);
        assert!(!rs.take_repaint_request(), "a fresh state is not stale");

        rs.images.area(MessageId(1), KITTY, CELL);
        assert!(rs.take_repaint_request(), "placing an image");

        // A resize: cached layouts and placed images both go.
        rs.images.area(MessageId(1), KITTY, CELL);
        let _ = rs.take_repaint_request();
        rs.clear();
        assert!(rs.take_repaint_request(), "a resize dropped a placement");
        assert!(!rs.take_repaint_request(), "taking clears the request");

        // A new cell size re-encodes every image at a different pixel size,
        // so every placement on screen is the wrong one.
        rs.images.area(MessageId(1), KITTY, CELL);
        let _ = rs.take_repaint_request();
        rs.set_cell_size(CellSize::new(7, 15));
        assert!(rs.take_repaint_request(), "a new cell size");
        rs.images.area(MessageId(1), KITTY, CELL);
        let _ = rs.take_repaint_request();
        rs.set_cell_size(CellSize::new(7, 15));
        assert!(
            !rs.take_repaint_request(),
            "re-measuring the same cell size changes nothing"
        );

        // And the explicit escape hatch, with nothing placed at all.
        rs.images.clear();
        let _ = rs.take_repaint_request();
        rs.request_repaint();
        assert!(rs.take_repaint_request());
        assert!(!rs.take_repaint_request());
    }
}
