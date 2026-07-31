//! Screen-coordinate → semantic-target lookup (architecture §7.5).
//!
//! `update()` is pure and never sees a `Rect`, so a mouse event has to be
//! resolved to a [`HitTarget`] before it can become an `Action`. The view is
//! the only place that knows where anything landed, so every draw records the
//! regions it just painted into a `HitMap` and hands it back; the runtime
//! loop keeps the latest one and consults it when crossterm reports a click
//! or a wheel step.
//!
//! The map is rebuilt from scratch on every frame — it is a *description of
//! the frame that was just drawn*, not accumulated state — so there is no
//! `clear()` and no invalidation to get wrong. A caller that wants to publish
//! "nothing is clickable" returns a fresh `HitMap::default()` instead
//! (`view::root` does exactly that while an overlay is up).
//!
//! Regions may overlap; **the last one pushed wins**. Views therefore push
//! from the bottom layer upward, the same order they draw in, and an overlay
//! that painted over a pane can mask that pane's regions by pushing its own
//! on top.

use ratatui::layout::{Position, Rect};
use tgt_core::model::hit::{HitTarget, ScrollArea};

/// The clickable and scrollable regions of one rendered frame.
///
/// Click targets ([`HitMap::push`]) and scrollable panes
/// ([`HitMap::push_area`]) are kept apart rather than merged into one list:
/// a wheel step over a chat row scrolls the sidebar, it does not click the
/// row, so the two lookups must not shadow each other.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HitMap {
    entries: Vec<(Rect, HitTarget)>,
    panes: Vec<(Rect, ScrollArea)>,
}

impl HitMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, rect: Rect, target: HitTarget) {
        self.entries.push((rect, target));
    }

    pub fn push_area(&mut self, rect: Rect, area: ScrollArea) {
        self.panes.push((rect, area));
    }

    /// The click target at cell `(x, y)`, or `None` where nothing clickable
    /// was drawn. Later pushes shadow earlier ones (see the module docs).
    pub fn target_at(&self, x: u16, y: u16) -> Option<HitTarget> {
        last_containing(&self.entries, x, y)
    }

    /// The scrollable pane containing cell `(x, y)`, same last-pushed-wins
    /// rule as [`HitMap::target_at`].
    pub fn area_at(&self, x: u16, y: u16) -> Option<ScrollArea> {
        last_containing(&self.panes, x, y)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.panes.is_empty()
    }
}

/// Zero-width and zero-height rects contain nothing (`Rect::contains`
/// already answers `false` for them), so a pane clipped out of existence by a
/// tiny terminal simply never matches.
fn last_containing<T: Copy>(regions: &[(Rect, T)], x: u16, y: u16) -> Option<T> {
    regions
        .iter()
        .rev()
        .find(|(rect, _)| rect.contains(Position::new(x, y)))
        .map(|(_, value)| *value)
}

#[cfg(test)]
mod tests {
    use tgt_core::model::chat::ChatListId;
    use tgt_core::model::ids::{ChatId, MessageId};

    use super::*;

    #[test]
    fn target_at_resolves_the_region_a_cell_falls_in() {
        let mut hits = HitMap::new();
        hits.push(Rect::new(0, 3, 30, 1), HitTarget::ChatRow(ChatId(7)));
        hits.push(Rect::new(0, 4, 30, 1), HitTarget::ArchiveRow);
        hits.push(Rect::new(30, 20, 90, 3), HitTarget::Composer);

        assert_eq!(hits.target_at(0, 3), Some(HitTarget::ChatRow(ChatId(7))));
        assert_eq!(hits.target_at(29, 3), Some(HitTarget::ChatRow(ChatId(7))));
        assert_eq!(hits.target_at(15, 4), Some(HitTarget::ArchiveRow));
        assert_eq!(hits.target_at(31, 21), Some(HitTarget::Composer));
    }

    #[test]
    fn a_cell_outside_every_region_resolves_to_nothing() {
        let mut hits = HitMap::new();
        hits.push(Rect::new(0, 3, 30, 1), HitTarget::ChatRow(ChatId(7)));

        // One past the right edge, one past the bottom edge, and the
        // untouched rest of the frame.
        assert_eq!(hits.target_at(30, 3), None);
        assert_eq!(hits.target_at(0, 4), None);
        assert_eq!(hits.target_at(90, 30), None);
        assert_eq!(HitMap::new().target_at(0, 0), None);
    }

    #[test]
    fn the_last_region_pushed_over_a_cell_wins() {
        // The layering an overlay would produce: a pane's rows first, the
        // thing painted over them second.
        let mut hits = HitMap::new();
        hits.push(Rect::new(0, 0, 40, 10), HitTarget::ChatRow(ChatId(1)));
        hits.push(Rect::new(5, 5, 10, 1), HitTarget::Message(MessageId(9)));

        assert_eq!(hits.target_at(6, 5), Some(HitTarget::Message(MessageId(9))));
        assert_eq!(hits.target_at(6, 6), Some(HitTarget::ChatRow(ChatId(1))));
    }

    #[test]
    fn scroll_areas_are_looked_up_independently_of_click_targets() {
        let mut hits = HitMap::new();
        hits.push_area(Rect::new(0, 0, 30, 20), ScrollArea::ChatList);
        hits.push_area(Rect::new(30, 0, 90, 20), ScrollArea::Conversation);
        hits.push(Rect::new(0, 3, 30, 1), HitTarget::ChatRow(ChatId(7)));

        // The same cell is both a chat row (click) and inside the sidebar
        // (wheel); neither lookup shadows the other.
        assert_eq!(hits.target_at(2, 3), Some(HitTarget::ChatRow(ChatId(7))));
        assert_eq!(hits.area_at(2, 3), Some(ScrollArea::ChatList));
        assert_eq!(hits.area_at(50, 10), Some(ScrollArea::Conversation));
        assert_eq!(hits.area_at(2, 25), None);
        assert_eq!(hits.target_at(50, 10), None);
    }

    #[test]
    fn tabs_pushed_side_by_side_resolve_to_the_one_under_the_cell() {
        let mut hits = HitMap::new();
        hits.push(
            Rect::new(1, 1, 4, 1),
            HitTarget::FolderTab(ChatListId::Main),
        );
        hits.push(
            Rect::new(8, 1, 8, 1),
            HitTarget::FolderTab(ChatListId::Folder(2)),
        );

        assert_eq!(
            hits.target_at(2, 1),
            Some(HitTarget::FolderTab(ChatListId::Main))
        );
        // The `·` separator between two tabs belongs to neither.
        assert_eq!(hits.target_at(6, 1), None);
        assert_eq!(
            hits.target_at(9, 1),
            Some(HitTarget::FolderTab(ChatListId::Folder(2)))
        );
    }
}
