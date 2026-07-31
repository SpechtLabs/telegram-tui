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

use std::collections::HashMap;

use ratatui::layout::Rect;
use tgt_core::app::AppState;
use tgt_core::model::ids::{ChatId, MessageId};
use tgt_core::state::conversation::Scroll;

use crate::render::cache::LayoutCache;
use crate::render::image::{Capability, ImageArea};

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
    /// What the last drawn frame's conversation viewport looked like, for
    /// [`RenderState::note_viewport`].
    viewport: Option<ViewportKey>,
}

impl RenderState {
    pub fn new(graphics: Option<Capability>) -> Self {
        RenderState {
            cache: LayoutCache::new(),
            images: ImageStore::new(),
            graphics,
            viewport: None,
        }
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
        }
    }

    /// The slot for `message_id`, created with `capability` if this is the
    /// first time this message has been laid out, and marked as belonging
    /// to the frame being drawn.
    pub fn area(
        &mut self,
        message_id: MessageId,
        capability: Option<Capability>,
    ) -> &mut ImageArea {
        let slot = self.areas.entry(message_id).or_insert_with(|| Slot {
            area: ImageArea::new(capability),
            touched: false,
        });
        slot.touched = true;
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
        self.areas.retain(|_, slot| slot.touched);
        for slot in self.areas.values_mut() {
            slot.touched = false;
        }
    }

    pub fn clear(&mut self) {
        self.areas.clear();
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

    #[test]
    fn areas_are_created_once_per_message_and_swept_when_a_frame_skips_them() {
        let mut store = ImageStore::new();
        assert!(store.is_empty());

        store.area(MessageId(1), Some(Capability::Kitty));
        store.area(MessageId(2), Some(Capability::Kitty));
        store.area(MessageId(1), Some(Capability::Kitty));
        assert_eq!(store.len(), 2, "the same message reuses its slot");

        // A frame that only reaches message 2 keeps message 2 and nothing
        // else — but only from the *next* sweep on, since the sweep that
        // ends a frame must not drop what that frame just touched.
        store.sweep();
        assert_eq!(store.len(), 2, "a sweep never drops the frame it ends");
        store.area(MessageId(2), Some(Capability::Kitty));
        store.sweep();
        assert_eq!(store.len(), 1);
        assert!(store.areas.contains_key(&MessageId(2)));

        store.clear();
        assert!(store.is_empty());
    }
}
