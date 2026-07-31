//! Layout cache — architecture.md §4.9 / §8.2. `layout_message` (T20) is
//! pure but not free: a long message wrapped at a given width produces a
//! `Vec<Line>` that is cheap to clone-hold and expensive to recompute every
//! frame, and the conversation view (T23) walks dozens of messages per draw.
//! `LayoutCache` memoizes that work, keyed on everything that can change the
//! output.
//!
//! Eviction policy (judgment call, §8): LRU bounded by TOTAL LINE COUNT, not
//! entry count — a 200-line pasted log and a one-word "ok" cost what they
//! cost, not one cache slot apiece. On insert, least-recently-used entries
//! are popped until the sum of cached lines is <= `MAX_CACHED_LINES`. Width
//! or theme change clears the cache wholesale (the caller's job — see
//! `crates/app/src/runtime_loop.rs`'s Resize handling); `theme_generation`
//! lives inside `LayoutKey` so theme swaps evict lazily through the same
//! path instead of needing their own clear call.
//!
//! Eviction-edge behavior, spelled out because the spec leaves it to the
//! implementation: a single entry whose own line count exceeds
//! `MAX_CACHED_LINES` is allowed to live alone, over the bound, once inserted
//! — evicting it would defeat its own insert (get_or_insert_with must return
//! a reference to what it just computed). It gets popped on the *next*
//! insert like any other LRU entry, at which point the cache is briefly
//! empty and back under bound. This only matters for pathological single
//! messages (a many-thousand-line paste); MAX_CACHED_LINES is 50_000, so
//! ordinary chat history never gets close.

use lru::LruCache;
use ratatui::text::Line;
use tgt_core::model::ids::MessageId;

/// Total cached line budget across all entries, not an entry-count cap.
pub const MAX_CACHED_LINES: usize = 50_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayoutKey {
    pub message_id: MessageId,
    pub width: u16,
    pub theme_generation: u64,
    pub spoilers_revealed: bool,
}

pub struct LayoutCache {
    entries: LruCache<LayoutKey, Vec<Line<'static>>>,
    total_lines: usize,
}

impl Default for LayoutCache {
    fn default() -> Self {
        Self::new()
    }
}

impl LayoutCache {
    pub fn new() -> Self {
        LayoutCache {
            entries: LruCache::unbounded(),
            total_lines: 0,
        }
    }

    /// Returns the cached lines for `key`, computing and inserting them via
    /// `f` on a miss. A hit promotes `key` to most-recently-used and never
    /// calls `f`. A miss may evict other entries (oldest-used first) to keep
    /// `total_lines` at or under `MAX_CACHED_LINES`, but never evicts the
    /// entry it just inserted — see the module docs for what happens when a
    /// single entry's line count alone exceeds the bound.
    pub fn get_or_insert_with(
        &mut self,
        key: LayoutKey,
        f: impl FnOnce() -> Vec<Line<'static>>,
    ) -> &Vec<Line<'static>> {
        if self.entries.contains(&key) {
            return self
                .entries
                .get(&key)
                .expect("just verified key is present");
        }

        let lines = f();
        self.total_lines += lines.len();
        self.entries.put(key, lines);

        // `put` just made `key` the most-recently-used entry, so `pop_lru`
        // only ever reaches it once it is the sole remaining entry — the
        // `len() > 1` guard stops right there instead of evicting what this
        // call is about to return.
        while self.total_lines > MAX_CACHED_LINES && self.entries.len() > 1 {
            let Some((_, evicted)) = self.entries.pop_lru() else {
                break;
            };
            self.total_lines -= evicted.len();
        }

        self.entries.get(&key).expect("just inserted")
    }

    /// Drops every cached layout. The caller clears wholesale on a width
    /// (Resize) change; theme changes don't need this because
    /// `theme_generation` is part of `LayoutKey` and simply misses forward.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.total_lines = 0;
    }

    pub fn total_lines(&self) -> usize {
        self.total_lines
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn key(
        message_id: i64,
        width: u16,
        theme_generation: u64,
        spoilers_revealed: bool,
    ) -> LayoutKey {
        LayoutKey {
            message_id: MessageId(message_id),
            width,
            theme_generation,
            spoilers_revealed,
        }
    }

    fn lines(n: usize) -> Vec<Line<'static>> {
        (0..n).map(|_| Line::from("x")).collect()
    }

    #[test]
    fn hit_returns_without_calling_closure() {
        let mut cache = LayoutCache::new();
        let k = key(1, 80, 0, false);
        cache.get_or_insert_with(k, || lines(3));

        let calls = Cell::new(0);
        let result = cache.get_or_insert_with(k, || {
            calls.set(calls.get() + 1);
            lines(999)
        });

        assert_eq!(
            result.len(),
            3,
            "hit must return the originally cached value"
        );
        assert_eq!(calls.get(), 0, "closure must not run on a cache hit");
    }

    #[test]
    fn total_lines_tracks_inserts_and_evictions() {
        let mut cache = LayoutCache::new();
        assert_eq!(cache.total_lines(), 0);

        cache.get_or_insert_with(key(1, 80, 0, false), || lines(5));
        assert_eq!(cache.total_lines(), 5);

        cache.get_or_insert_with(key(2, 80, 0, false), || lines(7));
        assert_eq!(cache.total_lines(), 12);

        // Re-fetching an existing key (hit) must not double-count.
        cache.get_or_insert_with(key(1, 80, 0, false), || lines(999));
        assert_eq!(cache.total_lines(), 12);
    }

    #[test]
    fn eviction_pops_lru_until_under_bound() {
        let mut cache = LayoutCache::new();
        let per_entry = MAX_CACHED_LINES / 3 + 1; // 3 entries exceed the bound together.

        let k1 = key(1, 80, 0, false);
        let k2 = key(2, 80, 0, false);
        let k3 = key(3, 80, 0, false);

        cache.get_or_insert_with(k1, || lines(per_entry));
        cache.get_or_insert_with(k2, || lines(per_entry));
        // Touch k1 so k2 becomes the least-recently-used entry instead of k1.
        cache.get_or_insert_with(k1, || lines(per_entry));
        cache.get_or_insert_with(k3, || lines(per_entry));

        assert!(
            cache.total_lines() <= MAX_CACHED_LINES,
            "total_lines ({}) must settle at or under the bound",
            cache.total_lines()
        );
        assert!(
            !cache.entries.contains(&k2),
            "k2 was least-recently-used and should have been evicted first"
        );
        assert!(
            cache.entries.contains(&k3),
            "the just-inserted entry must never be evicted by its own insert"
        );
    }

    #[test]
    fn clear_resets_everything() {
        let mut cache = LayoutCache::new();
        cache.get_or_insert_with(key(1, 80, 0, false), || lines(10));
        cache.get_or_insert_with(key(2, 80, 0, false), || lines(20));
        assert!(cache.total_lines() > 0);

        cache.clear();

        assert_eq!(cache.total_lines(), 0);
        assert_eq!(cache.entries.len(), 0);

        // A key inserted before the clear must miss again afterward.
        let calls = Cell::new(0);
        cache.get_or_insert_with(key(1, 80, 0, false), || {
            calls.set(calls.get() + 1);
            lines(10)
        });
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn distinct_key_components_miss() {
        let mut cache = LayoutCache::new();
        let base = key(1, 80, 0, false);
        cache.get_or_insert_with(base, || lines(1));

        let variants = [
            key(1, 81, 0, false), // width differs
            key(1, 80, 1, false), // theme_generation differs
            key(1, 80, 0, true),  // spoilers_revealed differs
            key(2, 80, 0, false), // message_id differs
        ];

        for variant in variants {
            let calls = Cell::new(0);
            cache.get_or_insert_with(variant, || {
                calls.set(calls.get() + 1);
                lines(1)
            });
            assert_eq!(
                calls.get(),
                1,
                "key variant {variant:?} must miss against the base entry"
            );
        }
    }
}
