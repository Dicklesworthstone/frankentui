#![forbid(unsafe_code)]

//! Grapheme pooling and interning.
//!
//! The `GraphemePool` stores complex grapheme clusters (emoji, ZWJ sequences, etc.)
//! that don't fit in `CellContent`'s 4-byte inline storage. It provides:
//!
//! - Compact `GraphemeId` references (4 bytes) instead of heap strings per cell
//! - Reference counting for automatic cleanup
//! - Deduplication via hash lookup
//! - Slot reuse via free list
//!
//! # When to Use
//!
//! Most cells use simple characters that fit inline in `CellContent`. The pool
//! is only needed for:
//! - Multi-codepoint emoji (👨‍👩‍👧‍👦, 🧑🏽‍💻, etc.)
//! - ZWJ sequences
//! - Complex combining character sequences
//!
//! # Usage
//!
//! ```
//! use ftui_render::grapheme_pool::GraphemePool;
//!
//! let mut pool = GraphemePool::new();
//!
//! // Intern a grapheme
//! let id = pool.intern("👨‍👩‍👧‍👦", 2); // Family emoji, width 2
//!
//! // Look it up
//! assert_eq!(pool.get(id), Some("👨‍👩‍👧‍👦"));
//! assert_eq!(id.width(), 2);
//!
//! // Increment reference count when copied to another cell
//! pool.retain(id);
//!
//! // Release when cell is overwritten
//! pool.release(id);
//! pool.release(id);
//!
//! // After all references released, slot is freed
//! assert_eq!(pool.get(id), None);
//! ```

use crate::buffer::Buffer;
use crate::cell::GraphemeId;
use std::collections::HashMap;

/// A slot in the grapheme pool.
#[derive(Debug, Clone)]
struct GraphemeSlot {
    /// The grapheme cluster string.
    text: String,
    /// Display width (cached from GraphemeId).
    /// Note: Width is also embedded in GraphemeId, but kept here for debugging.
    #[allow(dead_code)]
    width: u8,
    /// Reference count.
    refcount: u32,
}

/// A reference-counted pool for complex grapheme clusters.
///
/// Stores multi-codepoint strings and returns compact `GraphemeId` references.
#[derive(Debug, Clone)]
pub struct GraphemePool {
    /// Slot storage. `None` indicates a free slot.
    slots: Vec<Option<GraphemeSlot>>,
    /// Lookup table for deduplication.
    lookup: HashMap<String, GraphemeId>,
    /// Free slot indices for reuse.
    free_list: Vec<u32>,
}

impl GraphemePool {
    /// Create a new empty grapheme pool.
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            lookup: HashMap::new(),
            free_list: Vec::new(),
        }
    }

    /// Create a pool with pre-allocated capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: Vec::with_capacity(capacity),
            lookup: HashMap::with_capacity(capacity),
            free_list: Vec::new(),
        }
    }

    /// Number of active (non-free) slots.
    pub fn len(&self) -> usize {
        self.slots.len() - self.free_list.len()
    }

    /// Check if the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Total capacity (including free slots).
    pub fn capacity(&self) -> usize {
        self.slots.capacity()
    }

    /// Intern a grapheme string and return its ID.
    ///
    /// If the string is already interned, returns the existing ID and
    /// increments the reference count.
    ///
    /// # Parameters
    ///
    /// - `text`: The grapheme cluster string
    /// - `width`: Display width (0-127)
    ///
    /// # Panics
    ///
    /// Panics if width > 127 or if the pool exceeds capacity (16M slots).
    pub fn intern(&mut self, text: &str, width: u8) -> GraphemeId {
        assert!(width <= GraphemeId::MAX_WIDTH, "width overflow");

        // Check if already interned
        if let Some(&id) = self.lookup.get(text) {
            self.retain(id);
            return id;
        }

        // Allocate a new slot
        let slot_idx = self.alloc_slot();
        let id = GraphemeId::new(slot_idx, width);

        // Store the grapheme
        let slot = GraphemeSlot {
            text: text.to_string(),
            width,
            refcount: 1,
        };

        if (slot_idx as usize) < self.slots.len() {
            self.slots[slot_idx as usize] = Some(slot);
        } else {
            debug_assert_eq!(slot_idx as usize, self.slots.len());
            self.slots.push(Some(slot));
        }

        self.lookup.insert(text.to_string(), id);
        id
    }

    /// Get the string for a grapheme ID.
    ///
    /// Returns `None` if the ID is invalid or has been freed.
    pub fn get(&self, id: GraphemeId) -> Option<&str> {
        self.slots
            .get(id.slot())
            .and_then(|slot| slot.as_ref())
            .map(|slot| slot.text.as_str())
    }

    /// Increment the reference count for a grapheme.
    ///
    /// Call this when a cell containing this grapheme is copied.
    pub fn retain(&mut self, id: GraphemeId) {
        if let Some(Some(slot)) = self.slots.get_mut(id.slot()) {
            slot.refcount = slot.refcount.saturating_add(1);
        }
    }

    /// Decrement the reference count for a grapheme.
    ///
    /// Call this when a cell containing this grapheme is overwritten or freed.
    /// When the reference count reaches zero, the slot is freed for reuse.
    pub fn release(&mut self, id: GraphemeId) {
        let slot_idx = id.slot();
        if let Some(Some(slot)) = self.slots.get_mut(slot_idx) {
            slot.refcount = slot.refcount.saturating_sub(1);
            if slot.refcount == 0 {
                // Remove from lookup
                self.lookup.remove(&slot.text);
                // Clear the slot
                self.slots[slot_idx] = None;
                // Add to free list
                self.free_list.push(slot_idx as u32);
            }
        }
    }

    /// Get the reference count for a grapheme.
    ///
    /// Returns 0 if the ID is invalid or freed.
    pub fn refcount(&self, id: GraphemeId) -> u32 {
        self.slots
            .get(id.slot())
            .and_then(|slot| slot.as_ref())
            .map(|slot| slot.refcount)
            .unwrap_or(0)
    }

    /// Clear all entries from the pool.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.lookup.clear();
        self.free_list.clear();
    }

    /// Allocate a slot index, reusing from free list if possible.
    fn alloc_slot(&mut self) -> u32 {
        if let Some(idx) = self.free_list.pop() {
            idx
        } else {
            let idx = self.slots.len() as u32;
            assert!(
                idx <= GraphemeId::MAX_SLOT,
                "grapheme pool capacity exceeded"
            );
            idx
        }
    }

    /// Garbage collect graphemes not referenced by the given buffers.
    ///
    /// This implements a Mark-and-Sweep algorithm:
    /// 1. Reset all internal refcounts to 0.
    /// 2. Scan provided buffers and increment refcounts for referenced graphemes.
    /// 3. Free any slots that remain with refcount 0.
    ///
    /// This should be called periodically (e.g. every N frames) passing the
    /// current front and back buffers to prevent memory leaks in long-running apps.
    pub fn gc(&mut self, buffers: &[&Buffer]) {
        // 1. Reset
        for slot in self.slots.iter_mut().flatten() {
            slot.refcount = 0;
        }

        // 2. Mark
        for buf in buffers {
            for cell in buf.cells() {
                if let Some(id) = cell.content.grapheme_id() {
                    // We access via slot index directly.
                    // Note: id.slot() returns usize.
                    if let Some(Some(slot)) = self.slots.get_mut(id.slot()) {
                        slot.refcount = slot.refcount.saturating_add(1);
                    }
                }
            }
        }

        // 3. Sweep
        // We collect keys to remove to avoid borrow conflicts with self.lookup
        let mut keys_to_remove = Vec::new();

        for (idx, slot_opt) in self.slots.iter_mut().enumerate() {
            // Check refcount without holding a mutable borrow for too long
            let should_free = slot_opt.as_ref().is_some_and(|s| s.refcount == 0);

            if should_free {
                // Take the slot to own the string (no clone needed)
                if let Some(dead_slot) = slot_opt.take() {
                    keys_to_remove.push(dead_slot.text);
                    self.free_list.push(idx as u32);
                }
            }
        }

        for text in keys_to_remove {
            self.lookup.remove(&text);
        }
    }
}

impl Default for GraphemePool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_and_get() {
        let mut pool = GraphemePool::new();
        let id = pool.intern("👨‍👩‍👧‍👦", 2);

        assert_eq!(pool.get(id), Some("👨‍👩‍👧‍👦"));
        assert_eq!(id.width(), 2);
    }

    #[test]
    fn deduplication() {
        let mut pool = GraphemePool::new();
        let id1 = pool.intern("🎉", 2);
        let id2 = pool.intern("🎉", 2);

        // Same ID returned
        assert_eq!(id1, id2);
        // Refcount is 2
        assert_eq!(pool.refcount(id1), 2);
        // Only one slot used
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn retain_and_release() {
        let mut pool = GraphemePool::new();
        let id = pool.intern("🚀", 2);
        assert_eq!(pool.refcount(id), 1);

        pool.retain(id);
        assert_eq!(pool.refcount(id), 2);

        pool.release(id);
        assert_eq!(pool.refcount(id), 1);

        pool.release(id);
        // Slot is now freed
        assert_eq!(pool.get(id), None);
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn slot_reuse() {
        let mut pool = GraphemePool::new();

        // Intern and release
        let id1 = pool.intern("A", 1);
        pool.release(id1);
        assert_eq!(pool.len(), 0);

        // Intern again - should reuse the slot
        let id2 = pool.intern("B", 1);
        assert_eq!(id1.slot(), id2.slot());
        assert_eq!(pool.get(id2), Some("B"));
    }

    #[test]
    fn empty_pool() {
        let pool = GraphemePool::new();
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn multiple_graphemes() {
        let mut pool = GraphemePool::new();

        let id1 = pool.intern("👨‍💻", 2);
        let id2 = pool.intern("👩‍🔬", 2);
        let id3 = pool.intern("🧑🏽‍🚀", 2);

        assert_eq!(pool.len(), 3);
        assert_ne!(id1, id2);
        assert_ne!(id2, id3);

        assert_eq!(pool.get(id1), Some("👨‍💻"));
        assert_eq!(pool.get(id2), Some("👩‍🔬"));
        assert_eq!(pool.get(id3), Some("🧑🏽‍🚀"));
    }

    #[test]
    fn width_preserved() {
        let mut pool = GraphemePool::new();

        // Various widths
        let id1 = pool.intern("👋", 2);
        let id2 = pool.intern("A", 1);
        let id3 = pool.intern("日", 2);

        assert_eq!(id1.width(), 2);
        assert_eq!(id2.width(), 1);
        assert_eq!(id3.width(), 2);
    }

    #[test]
    fn clear_pool() {
        let mut pool = GraphemePool::new();
        pool.intern("A", 1);
        pool.intern("B", 1);
        pool.intern("C", 1);

        assert_eq!(pool.len(), 3);

        pool.clear();
        assert!(pool.is_empty());
    }

    #[test]
    fn invalid_id_returns_none() {
        let pool = GraphemePool::new();
        let fake_id = GraphemeId::new(999, 1);
        assert_eq!(pool.get(fake_id), None);
    }

    #[test]
    fn release_invalid_id_is_safe() {
        let mut pool = GraphemePool::new();
        let fake_id = GraphemeId::new(999, 1);
        pool.release(fake_id); // Should not panic
    }

    #[test]
    fn retain_invalid_id_is_safe() {
        let mut pool = GraphemePool::new();
        let fake_id = GraphemeId::new(999, 1);
        pool.retain(fake_id); // Should not panic
    }

    #[test]
    #[should_panic(expected = "width overflow")]
    fn width_overflow_panics() {
        let mut pool = GraphemePool::new();
        pool.intern("X", 128); // Max is 127
    }

    #[test]
    fn with_capacity() {
        let pool = GraphemePool::with_capacity(100);
        assert!(pool.capacity() >= 100);
        assert!(pool.is_empty());
    }

    mod gc_tests {
        use super::*;
        use crate::buffer::Buffer;
        use crate::cell::{Cell, CellContent};

        /// Helper: create a buffer with a grapheme cell at (0,0).
        fn buf_with_grapheme(id: GraphemeId) -> Buffer {
            let mut buf = Buffer::new(4, 1);
            let content = CellContent::from_grapheme(id);
            buf.set(0, 0, Cell::new(content));
            buf
        }

        #[test]
        fn gc_retains_referenced_grapheme() {
            let mut pool = GraphemePool::new();
            let id = pool.intern("🚀", 2);

            let buf = buf_with_grapheme(id);
            pool.gc(&[&buf]);

            assert_eq!(pool.get(id), Some("🚀"));
            assert_eq!(pool.refcount(id), 1);
        }

        #[test]
        fn gc_frees_unreferenced_grapheme() {
            let mut pool = GraphemePool::new();
            let id = pool.intern("🚀", 2);

            // Empty buffer — no references
            let buf = Buffer::new(4, 1);
            pool.gc(&[&buf]);

            assert_eq!(pool.get(id), None);
            assert_eq!(pool.refcount(id), 0);
            assert!(pool.is_empty());
        }

        #[test]
        fn gc_with_multiple_buffers() {
            let mut pool = GraphemePool::new();
            let id1 = pool.intern("🎉", 2);
            let id2 = pool.intern("🧪", 2);
            let id3 = pool.intern("🔥", 2);

            // buf1 references id1, buf2 references id3
            let buf1 = buf_with_grapheme(id1);
            let buf2 = buf_with_grapheme(id3);

            pool.gc(&[&buf1, &buf2]);

            assert_eq!(pool.get(id1), Some("🎉"));
            assert_eq!(pool.get(id2), None); // freed
            assert_eq!(pool.get(id3), Some("🔥"));
            assert_eq!(pool.len(), 2);
        }

        #[test]
        fn gc_with_multiple_references_in_buffer() {
            let mut pool = GraphemePool::new();
            let id = pool.intern("👨‍👩‍👧", 2);

            // Buffer with the same grapheme in two cells
            let mut buf = Buffer::new(4, 1);
            let content = CellContent::from_grapheme(id);
            buf.set(0, 0, Cell::new(content));
            buf.set(2, 0, Cell::new(content));

            pool.gc(&[&buf]);

            assert_eq!(pool.get(id), Some("👨‍👩‍👧"));
            assert_eq!(pool.refcount(id), 2);
        }

        #[test]
        fn gc_with_empty_pool() {
            let mut pool = GraphemePool::new();
            let buf = Buffer::new(4, 1);
            pool.gc(&[&buf]); // should not panic
            assert!(pool.is_empty());
        }

        #[test]
        fn gc_with_no_buffers() {
            let mut pool = GraphemePool::new();
            let id = pool.intern("test", 1);
            pool.gc(&[]);
            // No buffers means no references — everything freed
            assert_eq!(pool.get(id), None);
            assert!(pool.is_empty());
        }

        #[test]
        fn gc_freed_slots_are_reusable() {
            let mut pool = GraphemePool::new();
            let id1 = pool.intern("A", 1);
            let _id2 = pool.intern("B", 1);
            let slot1 = id1.slot();

            // Keep only id1
            let buf = buf_with_grapheme(id1);
            pool.gc(&[&buf]);

            // B was freed, its slot should be reusable
            let id3 = pool.intern("C", 1);
            // The freed slot from B should be reused (it was at slot index 1)
            assert_eq!(pool.get(id3), Some("C"));
            assert_eq!(pool.len(), 2); // A and C

            // id1 should still work
            assert_eq!(pool.get(id1), Some("A"));
            assert_eq!(id1.slot(), slot1);
        }

        #[test]
        fn gc_resets_refcounts_accurately() {
            let mut pool = GraphemePool::new();
            let id = pool.intern("🚀", 2);

            // Artificially inflate refcount
            pool.retain(id);
            pool.retain(id);
            assert_eq!(pool.refcount(id), 3);

            // Buffer has one reference
            let buf = buf_with_grapheme(id);
            pool.gc(&[&buf]);

            // GC resets then counts actual references
            assert_eq!(pool.refcount(id), 1);
        }

        #[test]
        fn gc_lookup_table_stays_consistent() {
            let mut pool = GraphemePool::new();
            let _id1 = pool.intern("A", 1);
            let id2 = pool.intern("B", 1);

            // Keep only B
            let buf = buf_with_grapheme(id2);
            pool.gc(&[&buf]);

            // A was freed from lookup, so interning A again should work
            let id_new = pool.intern("A", 1);
            assert_eq!(pool.get(id_new), Some("A"));

            // B should still be deduped
            let id_b2 = pool.intern("B", 1);
            assert_eq!(id_b2, id2);
        }
    }

    mod property {
        use super::*;
        use proptest::prelude::*;

        /// Generate a non-empty string suitable for interning.
        fn arb_grapheme() -> impl Strategy<Value = String> {
            prop::string::string_regex(".{1,8}")
                .unwrap()
                .prop_filter("non-empty", |s| !s.is_empty())
        }

        /// Generate a valid width (0..=127).
        fn arb_width() -> impl Strategy<Value = u8> {
            0u8..=GraphemeId::MAX_WIDTH
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            /// Intern followed by get always returns the original string.
            #[test]
            fn intern_get_roundtrip(s in arb_grapheme(), w in arb_width()) {
                let mut pool = GraphemePool::new();
                let id = pool.intern(&s, w);
                prop_assert_eq!(pool.get(id), Some(s.as_str()));
            }

            /// Width is preserved through intern.
            #[test]
            fn intern_preserves_width(s in arb_grapheme(), w in arb_width()) {
                let mut pool = GraphemePool::new();
                let id = pool.intern(&s, w);
                prop_assert_eq!(id.width(), w as usize);
            }

            /// Interning the same string twice returns the same id.
            #[test]
            fn deduplication_same_id(s in arb_grapheme(), w in arb_width()) {
                let mut pool = GraphemePool::new();
                let id1 = pool.intern(&s, w);
                let id2 = pool.intern(&s, w);
                prop_assert_eq!(id1, id2);
                prop_assert_eq!(pool.len(), 1);
            }

            /// After N interns of the same string, refcount equals N.
            #[test]
            fn deduplication_refcount(s in arb_grapheme(), w in arb_width(), extra in 0u32..10) {
                let mut pool = GraphemePool::new();
                let id = pool.intern(&s, w);
                for _ in 0..extra {
                    pool.intern(&s, w);
                }
                prop_assert_eq!(pool.refcount(id), 1 + extra);
            }

            /// Retain increments refcount, release decrements it.
            #[test]
            fn retain_release_refcount(
                s in arb_grapheme(),
                w in arb_width(),
                retains in 0u32..10,
                releases in 0u32..10
            ) {
                let mut pool = GraphemePool::new();
                let id = pool.intern(&s, w);
                // Start at refcount 1
                for _ in 0..retains {
                    pool.retain(id);
                }
                let expected_after_retain = 1 + retains;
                prop_assert_eq!(pool.refcount(id), expected_after_retain);

                let actual_releases = releases.min(expected_after_retain - 1);
                for _ in 0..actual_releases {
                    pool.release(id);
                }
                prop_assert_eq!(pool.refcount(id), expected_after_retain - actual_releases);
                // Entry should still be alive
                prop_assert_eq!(pool.get(id), Some(s.as_str()));
            }

            /// Releasing all references frees the slot.
            #[test]
            fn release_to_zero_frees(s in arb_grapheme(), w in arb_width(), extra in 0u32..5) {
                let mut pool = GraphemePool::new();
                let id = pool.intern(&s, w);
                for _ in 0..extra {
                    pool.retain(id);
                }
                // Release all: 1 (initial) + extra (retains)
                for _ in 0..=extra {
                    pool.release(id);
                }
                prop_assert_eq!(pool.get(id), None);
                prop_assert_eq!(pool.refcount(id), 0);
                prop_assert!(pool.is_empty());
            }

            /// Freed slots are reused by subsequent interns.
            #[test]
            fn slot_reuse_after_free(
                s1 in arb_grapheme(),
                s2 in arb_grapheme(),
                w in arb_width()
            ) {
                let mut pool = GraphemePool::new();
                let id1 = pool.intern(&s1, w);
                let slot1 = id1.slot();
                pool.release(id1);

                // s2 should reuse slot1's index
                let id2 = pool.intern(&s2, w);
                prop_assert_eq!(id2.slot(), slot1);
                prop_assert_eq!(pool.get(id2), Some(s2.as_str()));
            }

            /// len() tracks active entries correctly across operations.
            #[test]
            fn len_invariant(count in 1usize..20) {
                let mut pool = GraphemePool::new();
                let mut ids = Vec::new();
                for i in 0..count {
                    let s = format!("g{i}");
                    ids.push(pool.intern(&s, 1));
                }
                prop_assert_eq!(pool.len(), count);

                // Release half
                let release_count = count / 2;
                for id in &ids[..release_count] {
                    pool.release(*id);
                }
                prop_assert_eq!(pool.len(), count - release_count);
            }

            /// Multiple distinct strings produce distinct ids.
            #[test]
            fn distinct_strings_distinct_ids(count in 2usize..15) {
                let mut pool = GraphemePool::new();
                let mut ids = Vec::new();
                for i in 0..count {
                    let s = format!("unique_{i}");
                    ids.push(pool.intern(&s, 1));
                }
                // All ids should be distinct
                for i in 0..ids.len() {
                    for j in (i + 1)..ids.len() {
                        prop_assert_ne!(ids[i], ids[j]);
                    }
                }
            }

            /// Clear resets the pool entirely regardless of contents.
            #[test]
            fn clear_resets_all(count in 1usize..20) {
                let mut pool = GraphemePool::new();
                let mut ids = Vec::new();
                for i in 0..count {
                    let s = format!("c{i}");
                    ids.push(pool.intern(&s, 1));
                }
                pool.clear();
                prop_assert!(pool.is_empty());
                prop_assert_eq!(pool.len(), 0);
                for id in &ids {
                    prop_assert_eq!(pool.get(*id), None);
                }
            }

            // --- Executable Invariant Tests (bd-10i.13.2) ---

            /// Invariant: refcount > 0 implies get() returns Some (slot is valid).
            #[test]
            fn positive_refcount_implies_valid_slot(
                count in 1usize..10,
                retains in proptest::collection::vec(0u32..5, 1..10),
            ) {
                let mut pool = GraphemePool::new();
                let mut ids = Vec::new();
                for i in 0..count {
                    let s = format!("inv_{i}");
                    ids.push(pool.intern(&s, 1));
                }

                // Apply random retains
                for (i, &extra) in retains.iter().enumerate() {
                    let id = ids[i % count];
                    for _ in 0..extra {
                        pool.retain(id);
                    }
                }

                // Invariant check: every id with refcount > 0 must be gettable
                for (i, &id) in ids.iter().enumerate() {
                    let rc = pool.refcount(id);
                    if rc > 0 {
                        prop_assert!(pool.get(id).is_some(),
                            "slot {} has refcount {} but get() returned None", i, rc);
                    }
                }
            }

            /// Invariant: each release() decrements refcount by exactly 1.
            #[test]
            fn release_decrements_by_one(s in arb_grapheme(), w in arb_width(), retains in 1u32..8) {
                let mut pool = GraphemePool::new();
                let id = pool.intern(&s, w);
                for _ in 0..retains {
                    pool.retain(id);
                }
                let rc_before = pool.refcount(id);
                pool.release(id);
                let rc_after = pool.refcount(id);
                prop_assert_eq!(rc_after, rc_before - 1,
                    "release should decrement refcount by exactly 1");
            }

            /// Invariant: releasing a freed slot does not corrupt pool state.
            #[test]
            fn over_release_does_not_corrupt(count in 1usize..5) {
                let mut pool = GraphemePool::new();
                let mut ids = Vec::new();
                for i in 0..count {
                    let s = format!("or_{i}");
                    ids.push(pool.intern(&s, 1));
                }

                // Free the first entry
                let victim = ids[0];
                pool.release(victim);
                prop_assert_eq!(pool.refcount(victim), 0);
                prop_assert_eq!(pool.get(victim), None);

                // Double-release should be safe (saturating)
                pool.release(victim);
                prop_assert_eq!(pool.refcount(victim), 0);

                // Other entries must be unaffected
                for &id in &ids[1..] {
                    prop_assert!(pool.get(id).is_some(),
                        "over-release corrupted unrelated slot");
                    prop_assert!(pool.refcount(id) > 0);
                }
            }

            /// Invariant: GraphemeId from one pool is not valid in a different pool.
            #[test]
            fn cross_pool_id_is_invalid(s in arb_grapheme(), w in arb_width()) {
                let mut pool_a = GraphemePool::new();
                let pool_b = GraphemePool::new();
                let id = pool_a.intern(&s, w);

                // id from pool_a should not resolve in empty pool_b
                prop_assert_eq!(pool_b.get(id), None,
                    "GraphemeId from pool A should not be valid in pool B");
            }
        }
    }
}
