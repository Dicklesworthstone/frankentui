#![forbid(unsafe_code)]

//! OSC 8 hyperlink registry.
//!
//! The `LinkRegistry` maps link IDs to URLs. This allows cells to store
//! compact 24-bit link IDs instead of full URL strings.
//!
//! # Usage
//!
//! ```
//! use ftui_render::link_registry::LinkRegistry;
//!
//! let mut registry = LinkRegistry::new();
//! let id = registry.register("https://example.com");
//! assert_eq!(registry.get(id), Some("https://example.com"));
//! ```

use ahash::{AHashMap, AHashSet};

use crate::buffer::Buffer;

const MAX_LINK_ID: u32 = 0x00FF_FFFF;
const MAX_URL_BYTES: usize = 4096;

#[inline]
fn is_safe_osc8_url(url: &str) -> bool {
    if url.len() > MAX_URL_BYTES {
        return false;
    }
    !url.chars().any(char::is_control)
}

/// Registry for OSC 8 hyperlink URLs.
#[derive(Debug, Clone)]
pub struct LinkRegistry {
    /// Link slots indexed by ID (0 reserved for "no link").
    links: Vec<Option<String>>,
    /// URL to ID lookup for deduplication.
    lookup: AHashMap<String, u32>,
    /// Reusable IDs from removed links.
    free_list: Vec<u32>,
    /// Optional admission and retention policy for frame-scoped links.
    frame: Option<FrameLinks>,
}

#[derive(Debug, Clone)]
struct FrameLinks {
    limit: usize,
    admitted: AHashSet<u32>,
    protected: AHashSet<u32>,
}

impl Default for LinkRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl LinkRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            links: vec![None],
            lookup: AHashMap::new(),
            free_list: Vec::new(),
            frame: None,
        }
    }

    /// Create a registry admitting at most `limit` distinct link IDs per frame.
    ///
    /// The limit is clamped to half the 24-bit ID range. At most twice that
    /// many nonzero slots are allocated, allowing disjoint previous/current
    /// frames to coexist. A zero limit disables registration. URLs retain the
    /// same 4096-byte limit as unmanaged registries.
    ///
    /// Call [`Self::begin_frame`] before building each frame, supplying every
    /// presented or pending buffer whose IDs must remain valid. Numeric IDs
    /// are not durable handles: keep the URL and register it again each frame.
    /// Omitted IDs may be reused for another URL. Additional retained buffers
    /// can exhaust the slot budget; registration then returns zero (plain text).
    #[must_use]
    pub fn with_frame_limit(limit: usize) -> Self {
        Self {
            frame: Some(FrameLinks {
                limit: limit.min(MAX_LINK_ID as usize / 2),
                admitted: AHashSet::new(),
                protected: AHashSet::new(),
            }),
            ..Self::new()
        }
    }

    /// Retain IDs in the supplied buffers and begin a new admission interval.
    ///
    /// This does nothing for registries created with [`Self::new`] or
    /// [`Self::default`]. Managed registries remove every other URL, permitting
    /// its slot to be reused. Include the last successfully presented buffer
    /// and every pending buffer that will still be compared or presented;
    /// registering a URL does not retain it across this call on its own.
    ///
    /// Call only when starting another frame, after abandoning or retaining
    /// any previous render attempt. In particular, do not omit the presented
    /// buffer after a skipped render: ID reuse could hide a changed URL from
    /// a cell diff when its visible label stays the same.
    pub fn begin_frame(&mut self, buffers: &[&Buffer]) {
        let Some(frame) = &mut self.frame else {
            return;
        };
        frame.protected.clear();
        for buffer in buffers {
            for cell in buffer.cells() {
                let id = cell.attrs.link_id();
                if self.links.get(id as usize).is_some_and(Option::is_some) {
                    frame.protected.insert(id);
                }
            }
        }
        for (index, slot) in self.links.iter_mut().enumerate().skip(1) {
            let id = index as u32;
            if !frame.protected.contains(&id)
                && let Some(url) = slot.take()
            {
                self.lookup.remove(&url);
                self.free_list.push(id);
            }
        }
        frame.admitted.clear();
    }

    /// Per-frame admission limit, or `None` for an unmanaged registry.
    #[must_use]
    pub fn frame_limit(&self) -> Option<usize> {
        self.frame.as_ref().map(|frame| frame.limit)
    }

    /// Number of allocated nonzero slots, including currently vacant slots.
    ///
    /// This measures slot growth, not the allocator capacity of the backing
    /// vector. Managed registries never exceed twice [`Self::frame_limit`].
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.links.len() - 1
    }

    /// Register a URL and return its link ID.
    ///
    /// If the URL is already registered, returns the existing ID when frame
    /// admission permits it. Repeating a successful registration within one
    /// frame costs no additional admission. Unsafe URLs, admission exhaustion,
    /// and slot exhaustion return zero without consuming admission.
    pub fn register(&mut self, url: &str) -> u32 {
        if !is_safe_osc8_url(url) {
            return 0;
        }

        let existing = self.lookup.get(url).copied();
        if let Some(frame) = &self.frame
            && !existing.is_some_and(|id| frame.admitted.contains(&id))
            && frame.admitted.len() >= frame.limit
        {
            return 0;
        }

        let id = if let Some(id) = existing {
            id
        } else {
            let id = if let Some(id) = self.free_list.pop() {
                id
            } else {
                let max_slots = self
                    .frame
                    .as_ref()
                    .map_or(MAX_LINK_ID as usize, |frame| frame.limit * 2);
                if self.slot_count() >= max_slots {
                    return 0;
                }
                let id = self.links.len() as u32;
                self.links.push(None);
                id
            };
            self.links[id as usize] = Some(url.to_string());
            self.lookup.insert(url.to_string(), id);
            id
        };

        if let Some(frame) = &mut self.frame {
            frame.admitted.insert(id);
        }
        id
    }

    /// Get the URL for a link ID.
    #[must_use]
    pub fn get(&self, id: u32) -> Option<&str> {
        self.links
            .get(id as usize)
            .and_then(|slot| slot.as_ref())
            .map(|s| s.as_str())
    }

    /// Unregister a link by ID.
    ///
    /// In a managed registry this has no effect on IDs protected by the last
    /// [`Self::begin_frame`] roots or admitted in the current frame. A later
    /// `begin_frame` naturally retires them if no supplied buffer retains them.
    /// Unmanaged callers must stop using an ID before unregistering it.
    pub fn unregister(&mut self, id: u32) {
        if id == 0 {
            return;
        }
        if let Some(frame) = &self.frame
            && (frame.protected.contains(&id) || frame.admitted.contains(&id))
        {
            return;
        }

        let Some(slot) = self.links.get_mut(id as usize) else {
            return;
        };

        if let Some(url) = slot.take() {
            self.lookup.remove(&url);
            self.free_list.push(id);
        }
    }

    /// Clear all links.
    ///
    /// This explicitly invalidates every numeric ID, including protected
    /// managed IDs. Discard retained buffers and invalidate any presentation
    /// diff baseline before reusing the registry. The frame limit is preserved,
    /// while admission and retention start fresh.
    pub fn clear(&mut self) {
        self.links.clear();
        self.links.push(None);
        self.lookup.clear();
        self.free_list.clear();
        if let Some(frame) = &mut self.frame {
            frame.admitted.clear();
            frame.protected.clear();
        }
    }

    /// Number of registered links.
    pub fn len(&self) -> usize {
        self.links.iter().filter(|slot| slot.is_some()).count()
    }

    /// Check if the registry is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Check if the registry contains a link ID.
    #[inline]
    pub fn contains(&self, id: u32) -> bool {
        self.get(id).is_some()
    }

    /// Estimate heap memory usage in bytes.
    ///
    /// This is an approximate accounting used by runtime guardrails/telemetry.
    #[must_use]
    pub fn estimate_memory(&self) -> usize {
        let mut total = 0usize;

        total += self.links.capacity() * core::mem::size_of::<Option<String>>();
        total += self
            .links
            .iter()
            .flatten()
            .map(String::capacity)
            .sum::<usize>();

        total += self.lookup.capacity() * core::mem::size_of::<(String, u32)>();
        total += self.lookup.keys().map(String::capacity).sum::<usize>();

        total += self.free_list.capacity() * core::mem::size_of::<u32>();
        if let Some(frame) = &self.frame {
            total += (frame.admitted.capacity() + frame.protected.capacity())
                * core::mem::size_of::<u32>();
        }

        total
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::Cell;
    use crate::diff::BufferDiff;

    fn linked_labels(ids: &[u32]) -> Buffer {
        let mut buffer = Buffer::new(ids.len() as u16, 1);
        for (x, &id) in ids.iter().enumerate() {
            let mut cell = Cell::from_char('X');
            cell.attrs = cell.attrs.with_link(id);
            buffer.set_raw(x as u16, 0, cell);
        }
        buffer
    }

    // These exercise the registry with real Buffer/BufferDiff operations.
    // They do not establish runtime root selection or terminal click behavior.
    #[test]
    fn managed_same_label_target_changes_remain_visible_to_diff() {
        let mut registry = LinkRegistry::with_frame_limit(1);
        let mut previous = linked_labels(&[0]);
        let mut previous_url = None;
        for url in [
            "https://example.com/a",
            "https://example.com/b",
            "https://example.com/c",
            "https://example.com/a",
        ] {
            registry.begin_frame(&[&previous]);
            let previous_id = previous.get(0, 0).unwrap().attrs.link_id();
            let id = registry.register(url);
            assert_ne!(id, 0);
            assert_ne!(id, previous_id);
            assert_eq!(registry.get(id), Some(url));
            assert_eq!(registry.get(previous_id), previous_url);
            let next = linked_labels(&[id]);
            assert_eq!(BufferDiff::compute(&previous, &next).len(), 1);
            previous = next;
            previous_url = Some(url);
        }
        assert_eq!(registry.slot_count(), 2);
    }

    #[test]
    fn managed_slot_high_water_stays_at_twice_limit_over_many_urls() {
        const LIMIT: usize = 4;
        let mut registry = LinkRegistry::with_frame_limit(LIMIT);
        let mut previous = linked_labels(&[0; LIMIT]);
        let mut previous_urls: Vec<String> = Vec::new();
        for frame in 0..300 {
            registry.begin_frame(&[&previous]);
            let urls: Vec<_> = (0..LIMIT)
                .map(|index| format!("https://example.com/{frame}/{index}"))
                .collect();
            let ids: Vec<_> = urls.iter().map(|url| registry.register(url)).collect();
            assert!(ids.iter().all(|id| *id != 0));
            for (index, url) in previous_urls.iter().enumerate() {
                let old_id = previous.get(index as u16, 0).unwrap().attrs.link_id();
                assert_eq!(registry.get(old_id), Some(url.as_str()));
                assert!(!ids.contains(&old_id));
            }
            for (&id, url) in ids.iter().zip(&urls) {
                assert_eq!(registry.get(id), Some(url.as_str()));
                assert_eq!(registry.register(url), id);
            }
            assert_eq!(registry.register("https://example.com/over-budget"), 0);
            assert_eq!(
                registry.slot_count(),
                if frame == 0 { LIMIT } else { 2 * LIMIT }
            );
            assert_eq!(registry.len(), if frame == 0 { LIMIT } else { 2 * LIMIT });
            let next = linked_labels(&ids);
            assert_eq!(BufferDiff::compute(&previous, &next).len(), LIMIT);
            previous = next;
            previous_urls = urls;
        }
    }

    #[test]
    fn managed_abandoned_frames_reclaim_slots_without_unpinning_presented_links() {
        let mut registry = LinkRegistry::with_frame_limit(1);
        let shown_id = registry.register("https://example.com/shown");
        let shown = linked_labels(&[shown_id]);
        for attempt in 0..200 {
            // The previous attempted frame was abandoned; only shown remains.
            registry.begin_frame(&[&shown]);
            registry.begin_frame(&[&shown]);
            let url = format!("https://example.com/attempt/{attempt}");
            let id = registry.register(&url);
            let abandoned = linked_labels(&[id]);
            assert_ne!(id, 0);
            assert_ne!(id, shown_id);
            assert_eq!(registry.get(shown_id), Some("https://example.com/shown"));
            assert_eq!(registry.get(id), Some(url.as_str()));
            assert_eq!(BufferDiff::compute(&shown, &abandoned).len(), 1);
            assert_eq!(registry.slot_count(), 2);
            assert_eq!(registry.len(), 2);
        }
        registry.begin_frame(&[&shown]);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.slot_count(), 2);
        assert_eq!(registry.register("https://example.com/shown"), shown_id);
        assert_eq!(
            BufferDiff::compute(&shown, &linked_labels(&[shown_id])).len(),
            0
        );
    }

    #[test]
    fn managed_pending_roots_prevent_reuse_and_slot_rejection_costs_no_admission() {
        let mut registry = LinkRegistry::with_frame_limit(1);
        let a = registry.register("https://example.com/a");
        let shown = linked_labels(&[a]);
        registry.begin_frame(&[&shown]);
        let b = registry.register("https://example.com/b");
        let pending = linked_labels(&[b]);
        assert_ne!(a, b);

        registry.begin_frame(&[&shown, &pending, &shown]);
        assert_eq!(registry.register("https://example.com/c"), 0);
        assert_eq!(registry.get(a), Some("https://example.com/a"));
        assert_eq!(registry.get(b), Some("https://example.com/b"));
        // The failed new slot did not consume the one available admission.
        assert_eq!(registry.register("https://example.com/b"), b);
        assert_eq!(registry.register("https://example.com/b"), b);
        assert_eq!(registry.register("https://example.com/a"), 0);
        assert_eq!(registry.slot_count(), 2);

        registry.begin_frame(&[&pending]);
        let c = registry.register("https://example.com/c");
        assert_eq!(c, a);
        assert_eq!(registry.get(b), Some("https://example.com/b"));
        assert_eq!(registry.get(c), Some("https://example.com/c"));
        assert_eq!(BufferDiff::compute(&pending, &linked_labels(&[c])).len(), 1);
    }

    #[test]
    fn managed_admission_counts_distinct_successful_ids_and_rejects_unsafe_urls() {
        let mut registry = LinkRegistry::with_frame_limit(2);
        for rejected in [
            "https://example.com/\x1b[2J".to_owned(),
            "https://example.com/\n".to_owned(),
            "a".repeat(MAX_URL_BYTES + 1),
        ] {
            assert_eq!(registry.register(&rejected), 0);
        }
        assert_eq!(registry.slot_count(), 0);
        let max_length = "a".repeat(MAX_URL_BYTES);
        let a = registry.register(&max_length);
        assert_ne!(a, 0);
        for _ in 0..20 {
            assert_eq!(registry.register(&max_length), a);
        }
        let b = registry.register("https://example.com/b");
        assert_ne!(b, 0);
        assert_ne!(a, b);
        assert_eq!(registry.register("https://example.com/c"), 0);
        assert_eq!(registry.slot_count(), 2);

        let shown = linked_labels(&[a, b]);
        registry.begin_frame(&[&shown]);
        assert_eq!(registry.register("https://example.com/b"), b);
        assert_eq!(registry.register(&max_length), a);
        assert_eq!(registry.register("https://example.com/b"), b);
        assert_eq!(registry.register("https://example.com/c"), 0);
    }

    #[test]
    fn managed_unregister_cannot_recycle_pinned_or_current_frame_ids() {
        let mut registry = LinkRegistry::with_frame_limit(1);
        {
            let a = registry.register("https://example.com/a");
            let shown = linked_labels(&[a]);
            registry.unregister(a);
            assert_eq!(registry.get(a), Some("https://example.com/a"));
            assert_eq!(registry.register("https://example.com/b"), 0);

            registry.begin_frame(&[&shown]);
            registry.unregister(a);
            let b = registry.register("https://example.com/b");
            let next = linked_labels(&[b]);
            registry.unregister(b);
            registry.unregister(u32::MAX);
            registry.unregister(0);
            assert_eq!(registry.get(a), Some("https://example.com/a"));
            assert_eq!(registry.get(b), Some("https://example.com/b"));
            assert_ne!(a, b);

            registry.begin_frame(&[&next]);
            assert_eq!(registry.get(a), None);
            assert_eq!(registry.get(b), Some("https://example.com/b"));
            assert_eq!(registry.register("https://example.com/c"), a);
        }
        // All buffers are discarded before this explicit invalidation.
        registry.clear();
        assert_eq!(registry.frame_limit(), Some(1));
        assert_eq!(registry.len(), 0);
        assert_eq!(registry.slot_count(), 0);
        assert_eq!(registry.register("https://example.com/fresh"), 1);
        assert_eq!(registry.register("https://example.com/overflow"), 0);
    }

    #[test]
    fn managed_zero_and_overflow_limits_are_bounded_without_preallocation() {
        let mut disabled = LinkRegistry::with_frame_limit(0);
        disabled.begin_frame(&[&linked_labels(&[0, MAX_LINK_ID])]);
        assert_eq!(disabled.frame_limit(), Some(0));
        assert_eq!(disabled.register("https://example.com"), 0);
        disabled.clear();
        assert_eq!(disabled.frame_limit(), Some(0));
        assert_eq!(disabled.slot_count(), 0);
        assert_eq!(disabled.register("https://example.com"), 0);

        let mut maximum = LinkRegistry::with_frame_limit(usize::MAX);
        assert_eq!(maximum.frame_limit(), Some(MAX_LINK_ID as usize / 2));
        assert_eq!(maximum.slot_count(), 0);
        assert_eq!(maximum.register("https://example.com"), 1);
        assert_eq!(maximum.slot_count(), 1);
    }

    #[test]
    fn unmanaged_frame_boundaries_leave_urls_and_reuse_behavior_unchanged() {
        for mut registry in [LinkRegistry::new(), LinkRegistry::default()] {
            assert_eq!(registry.frame_limit(), None);
            let a = registry.register("https://example.com/a");
            registry.begin_frame(&[]);
            assert_eq!(registry.get(a), Some("https://example.com/a"));
            assert_eq!(registry.register("https://example.com/a"), a);
            registry.unregister(a);
            assert_eq!(registry.register("https://example.com/b"), a);
            for index in 0..300 {
                assert_ne!(
                    registry.register(&format!("https://example.com/{index}")),
                    0
                );
            }
            assert_eq!(registry.slot_count(), 301);
            registry.begin_frame(&[]);
            assert_eq!(registry.len(), 301);
            registry.clear();
            assert_eq!(registry.frame_limit(), None);
            assert_eq!(registry.slot_count(), 0);
        }
    }

    #[test]
    fn managed_clone_preserves_admission_and_retention_independently() {
        let mut original = LinkRegistry::with_frame_limit(1);
        let a = original.register("https://example.com/a");
        let shown = linked_labels(&[a]);
        original.begin_frame(&[&shown]);
        let b = original.register("https://example.com/b");
        let mut cloned = original.clone();
        cloned.unregister(a);
        cloned.unregister(b);
        assert_eq!(cloned.get(a), Some("https://example.com/a"));
        assert_eq!(cloned.get(b), Some("https://example.com/b"));
        assert_eq!(cloned.register("https://example.com/c"), 0);
        cloned.begin_frame(&[]);
        assert!(cloned.is_empty());
        assert_ne!(cloned.register("https://example.com/c"), 0);
        assert_eq!(original.get(a), Some("https://example.com/a"));
        assert_eq!(original.get(b), Some("https://example.com/b"));
        assert_eq!(original.register("https://example.com/c"), 0);
    }

    #[test]
    fn register_and_get() {
        let mut registry = LinkRegistry::new();
        let id = registry.register("https://example.com");
        assert_eq!(registry.get(id), Some("https://example.com"));
    }

    #[test]
    fn deduplication() {
        let mut registry = LinkRegistry::new();
        let id1 = registry.register("https://example.com");
        let id2 = registry.register("https://example.com");
        assert_eq!(id1, id2);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn multiple_urls() {
        let mut registry = LinkRegistry::new();
        let id1 = registry.register("https://one.com");
        let id2 = registry.register("https://two.com");
        assert_ne!(id1, id2);
        assert_eq!(registry.get(id1), Some("https://one.com"));
        assert_eq!(registry.get(id2), Some("https://two.com"));
    }

    #[test]
    fn unregister_reuses_id() {
        let mut registry = LinkRegistry::new();
        let id = registry.register("https://example.com");
        assert!(registry.contains(id));
        registry.unregister(id);
        assert!(!registry.contains(id));
        let reused = registry.register("https://new.com");
        assert_eq!(reused, id);
    }

    #[test]
    fn clear() {
        let mut registry = LinkRegistry::new();
        registry.register("https://one.com");
        registry.register("https://two.com");
        assert_eq!(registry.len(), 2);
        registry.clear();
        assert!(registry.is_empty());
    }

    // --- Edge case tests ---

    #[test]
    fn id_zero_is_reserved() {
        let registry = LinkRegistry::new();
        assert_eq!(registry.get(0), None);
    }

    #[test]
    fn unregister_zero_is_noop() {
        let mut registry = LinkRegistry::new();
        registry.register("https://example.com");
        registry.unregister(0);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn get_out_of_bounds_returns_none() {
        let registry = LinkRegistry::new();
        assert_eq!(registry.get(999), None);
        assert_eq!(registry.get(u32::MAX), None);
    }

    #[test]
    fn unregister_out_of_bounds_is_safe() {
        let mut registry = LinkRegistry::new();
        registry.unregister(999);
        registry.unregister(u32::MAX);
        // No panic, no effect
        assert!(registry.is_empty());
    }

    #[test]
    fn unregister_twice_is_safe() {
        let mut registry = LinkRegistry::new();
        let id = registry.register("https://example.com");
        registry.unregister(id);
        registry.unregister(id); // Second call is no-op
        assert!(registry.is_empty());
    }

    #[test]
    fn register_returns_nonzero() {
        let mut registry = LinkRegistry::new();
        for i in 0..20 {
            let id = registry.register(&format!("https://example.com/{i}"));
            assert_ne!(id, 0, "register must never return id 0");
        }
    }

    #[test]
    fn contains_after_unregister() {
        let mut registry = LinkRegistry::new();
        let id = registry.register("https://example.com");
        assert!(registry.contains(id));
        registry.unregister(id);
        assert!(!registry.contains(id));
    }

    #[test]
    fn contains_invalid_id() {
        let registry = LinkRegistry::new();
        assert!(!registry.contains(0));
        assert!(!registry.contains(999));
    }

    #[test]
    fn dedup_after_unregister_gets_new_id() {
        let mut registry = LinkRegistry::new();
        let id1 = registry.register("https://example.com");
        registry.unregister(id1);
        // Re-register same URL — lookup cleared, so gets new (reused) id
        let id2 = registry.register("https://example.com");
        assert_eq!(id2, id1); // Reuses freed slot
        assert_eq!(registry.get(id2), Some("https://example.com"));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn free_list_lifo_order() {
        let mut registry = LinkRegistry::new();
        let a = registry.register("https://a.com");
        let b = registry.register("https://b.com");
        let c = registry.register("https://c.com");

        // Free in order a, b, c — free_list is [a, b, c]
        registry.unregister(a);
        registry.unregister(b);
        registry.unregister(c);

        // LIFO: next alloc pops c, then b, then a
        let new1 = registry.register("https://new1.com");
        assert_eq!(new1, c);
        let new2 = registry.register("https://new2.com");
        assert_eq!(new2, b);
        let new3 = registry.register("https://new3.com");
        assert_eq!(new3, a);
    }

    #[test]
    fn len_tracks_operations() {
        let mut registry = LinkRegistry::new();
        assert_eq!(registry.len(), 0);

        let id1 = registry.register("https://one.com");
        assert_eq!(registry.len(), 1);

        let id2 = registry.register("https://two.com");
        assert_eq!(registry.len(), 2);

        // Dedup doesn't increase len
        registry.register("https://one.com");
        assert_eq!(registry.len(), 2);

        registry.unregister(id1);
        assert_eq!(registry.len(), 1);

        registry.unregister(id2);
        assert_eq!(registry.len(), 0);
        assert!(registry.is_empty());
    }

    #[test]
    fn register_after_clear_works() {
        let mut registry = LinkRegistry::new();
        registry.register("https://one.com");
        registry.register("https://two.com");
        registry.clear();

        let id = registry.register("https://fresh.com");
        assert_ne!(id, 0);
        assert_eq!(registry.get(id), Some("https://fresh.com"));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn many_registrations() {
        let mut registry = LinkRegistry::new();
        let mut ids = Vec::new();
        for i in 0..100 {
            let url = format!("https://example.com/{i}");
            ids.push(registry.register(&url));
        }
        assert_eq!(registry.len(), 100);

        // All IDs unique and non-zero
        for (i, &id) in ids.iter().enumerate() {
            assert_ne!(id, 0);
            let url = format!("https://example.com/{i}");
            assert_eq!(registry.get(id), Some(url.as_str()));
        }

        // All IDs distinct
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
    }

    // ================================================================
    // Edge-case tests (bd-39nm2)
    // ================================================================

    #[test]
    fn default_trait_creates_empty_registry() {
        let mut registry = LinkRegistry::default();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert_eq!(registry.get(0), None);

        // Rejected input must not consume the first usable slot.
        assert_eq!(registry.register("https://example.com/\x1b[2J"), 0);
        let first = registry.register("https://example.com/first");
        assert_eq!(first, 1);
        assert_eq!(registry.get(first), Some("https://example.com/first"));
        assert_eq!(registry.register("https://example.com/first"), first);
        let second = registry.register("https://example.com/second");
        assert_eq!(second, 2);
        assert_eq!(registry.len(), 2);

        let cloned = registry.clone();
        registry.unregister(first);
        assert_eq!(registry.register("https://example.com/reused"), first);
        assert_eq!(cloned.get(first), Some("https://example.com/first"));
        assert_eq!(registry.get(0), None);
        registry.clear();
        assert!(registry.is_empty());
        assert_eq!(registry.register("https://example.com/after-clear"), 1);
    }

    #[test]
    fn clone_independence() {
        let mut original = LinkRegistry::new();
        let id = original.register("https://example.com");
        let mut cloned = original.clone();

        // Mutate clone
        cloned.unregister(id);
        assert!(!cloned.contains(id));

        // Original unaffected
        assert!(original.contains(id));
        assert_eq!(original.get(id), Some("https://example.com"));
    }

    #[test]
    fn debug_formatting() {
        let mut registry = LinkRegistry::new();
        registry.register("https://example.com");
        let dbg = format!("{:?}", registry);
        assert!(dbg.contains("LinkRegistry"));
        assert!(dbg.contains("example.com"));
    }

    #[test]
    fn register_empty_url() {
        let mut registry = LinkRegistry::new();
        let id = registry.register("");
        assert_ne!(id, 0);
        assert_eq!(registry.get(id), Some(""));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn register_url_with_special_chars() {
        let mut registry = LinkRegistry::new();
        let url = "https://example.com/path?q=hello world&foo=bar#section";
        let id = registry.register(url);
        assert_eq!(registry.get(id), Some(url));
    }

    #[test]
    fn register_url_with_unicode() {
        let mut registry = LinkRegistry::new();
        let url = "https://例え.jp/日本語";
        let id = registry.register(url);
        assert_eq!(registry.get(id), Some(url));
    }

    #[test]
    fn register_very_long_url() {
        let mut registry = LinkRegistry::new();
        let url = format!("https://example.com/{}", "a".repeat(10_000));
        let id = registry.register(&url);
        assert_eq!(id, 0);
        assert_eq!(registry.get(id), None);
    }

    #[test]
    fn register_max_length_url() {
        let mut registry = LinkRegistry::new();
        let prefix = "https://example.com/";
        let suffix_len = MAX_URL_BYTES.saturating_sub(prefix.len());
        let url = format!("{prefix}{}", "a".repeat(suffix_len));
        assert_eq!(url.len(), MAX_URL_BYTES);

        let id = registry.register(&url);
        assert_ne!(id, 0);
        assert_eq!(registry.get(id), Some(url.as_str()));
    }

    #[test]
    fn is_empty_on_fresh_registry() {
        let registry = LinkRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn is_empty_after_register_unregister() {
        let mut registry = LinkRegistry::new();
        let id = registry.register("https://test.com");
        assert!(!registry.is_empty());
        registry.unregister(id);
        assert!(registry.is_empty());
    }

    #[test]
    fn clear_multiple_times() {
        let mut registry = LinkRegistry::new();
        registry.register("https://a.com");
        registry.clear();
        assert!(registry.is_empty());
        registry.clear(); // Double clear
        assert!(registry.is_empty());

        // Still usable after double clear
        let id = registry.register("https://b.com");
        assert_ne!(id, 0);
        assert_eq!(registry.get(id), Some("https://b.com"));
    }

    #[test]
    fn ids_sequential_when_no_free_list() {
        let mut registry = LinkRegistry::new();
        let id1 = registry.register("https://a.com");
        let id2 = registry.register("https://b.com");
        let id3 = registry.register("https://c.com");
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
    }

    #[test]
    fn free_list_mixed_with_fresh_allocation() {
        let mut registry = LinkRegistry::new();
        let id1 = registry.register("https://a.com");
        let id2 = registry.register("https://b.com");
        let id3 = registry.register("https://c.com");

        // Free id2 only
        registry.unregister(id2);

        // Next register reuses id2 (from free list)
        let id4 = registry.register("https://d.com");
        assert_eq!(id4, id2);

        // Next register allocates fresh id4
        let id5 = registry.register("https://e.com");
        assert_eq!(id5, 4); // Fresh allocation
        assert_eq!(registry.len(), 4); // a, c, d, e

        // Verify all valid
        assert_eq!(registry.get(id1), Some("https://a.com"));
        assert_eq!(registry.get(id4), Some("https://d.com"));
        assert_eq!(registry.get(id3), Some("https://c.com"));
        assert_eq!(registry.get(id5), Some("https://e.com"));
    }

    #[test]
    fn unregister_does_not_affect_others() {
        let mut registry = LinkRegistry::new();
        let id1 = registry.register("https://a.com");
        let id2 = registry.register("https://b.com");
        let id3 = registry.register("https://c.com");

        registry.unregister(id2);

        assert_eq!(registry.get(id1), Some("https://a.com"));
        assert_eq!(registry.get(id2), None);
        assert_eq!(registry.get(id3), Some("https://c.com"));
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn dedup_still_works_after_cycle() {
        let mut registry = LinkRegistry::new();
        let id1 = registry.register("https://a.com");
        registry.unregister(id1);
        let id2 = registry.register("https://a.com");
        // Reuses slot
        assert_eq!(id1, id2);
        // Now dedup works for the re-registered URL
        let id3 = registry.register("https://a.com");
        assert_eq!(id2, id3);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn register_all_freed_then_register_new() {
        let mut registry = LinkRegistry::new();
        let ids: Vec<u32> = (0..5)
            .map(|i| registry.register(&format!("https://u{i}.com")))
            .collect();

        // Free all
        for &id in &ids {
            registry.unregister(id);
        }
        assert!(registry.is_empty());

        // Register new URLs — should reuse freed IDs
        let new_ids: Vec<u32> = (0..5)
            .map(|i| registry.register(&format!("https://new{i}.com")))
            .collect();
        assert_eq!(registry.len(), 5);

        // All new IDs should be from the original set (reused)
        for &new_id in &new_ids {
            assert!(ids.contains(&new_id));
        }
    }

    #[test]
    fn get_returns_none_after_clear() {
        let mut registry = LinkRegistry::new();
        let id = registry.register("https://example.com");
        registry.clear();
        assert_eq!(registry.get(id), None);
    }

    #[test]
    fn contains_zero_always_false() {
        let mut registry = LinkRegistry::new();
        assert!(!registry.contains(0));
        registry.register("https://example.com");
        assert!(!registry.contains(0));
    }

    #[test]
    fn clone_preserves_free_list() {
        let mut registry = LinkRegistry::new();
        let id1 = registry.register("https://a.com");
        let _id2 = registry.register("https://b.com");
        registry.unregister(id1);

        let mut cloned = registry.clone();
        // Clone should have id1 in free list
        let id3 = cloned.register("https://c.com");
        assert_eq!(id3, id1); // Reuses freed slot
    }

    #[test]
    fn register_rejects_control_chars() {
        let mut registry = LinkRegistry::new();
        let id = registry.register("https://example.com/\x1b]52;c;boom");
        assert_eq!(id, 0);
        assert!(registry.is_empty());
    }

    #[test]
    fn register_rejects_overlong_urls() {
        let mut registry = LinkRegistry::new();
        let overlong = format!("https://example.com/{}", "a".repeat(MAX_URL_BYTES));
        let id = registry.register(&overlong);
        assert_eq!(id, 0);
        assert!(registry.is_empty());
    }

    mod property {
        use super::*;
        use proptest::prelude::*;

        fn arb_url() -> impl Strategy<Value = String> {
            "[a-z]{3,12}".prop_map(|s| format!("https://{s}.com"))
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            /// Register/get roundtrip always returns the original URL.
            #[test]
            fn register_get_roundtrip(url in arb_url()) {
                let mut registry = LinkRegistry::new();
                let id = registry.register(&url);
                prop_assert_ne!(id, 0);
                prop_assert_eq!(registry.get(id), Some(url.as_str()));
            }

            /// Duplicate registration returns the same ID.
            #[test]
            fn dedup_same_id(url in arb_url()) {
                let mut registry = LinkRegistry::new();
                let id1 = registry.register(&url);
                let id2 = registry.register(&url);
                prop_assert_eq!(id1, id2);
                prop_assert_eq!(registry.len(), 1);
            }

            /// Distinct URLs produce distinct IDs.
            #[test]
            fn distinct_urls_distinct_ids(count in 2usize..20) {
                let mut registry = LinkRegistry::new();
                let mut ids = Vec::new();
                for i in 0..count {
                    ids.push(registry.register(&format!("https://u{i}.com")));
                }
                for i in 0..ids.len() {
                    for j in (i + 1)..ids.len() {
                        prop_assert_ne!(ids[i], ids[j]);
                    }
                }
            }

            /// len tracks correctly through register/unregister cycles.
            #[test]
            fn len_invariant(n_register in 1usize..15, n_unregister in 0usize..15) {
                let mut registry = LinkRegistry::new();
                let mut ids = Vec::new();
                for i in 0..n_register {
                    ids.push(registry.register(&format!("https://r{i}.com")));
                }
                prop_assert_eq!(registry.len(), n_register);

                let actual_unreg = n_unregister.min(n_register);
                for id in &ids[..actual_unreg] {
                    registry.unregister(*id);
                }
                prop_assert_eq!(registry.len(), n_register - actual_unreg);
            }

            /// Unregister + re-register reuses the freed slot.
            #[test]
            fn slot_reuse(url1 in arb_url(), url2 in arb_url()) {
                let mut registry = LinkRegistry::new();
                let id1 = registry.register(&url1);
                registry.unregister(id1);
                let id2 = registry.register(&url2);
                prop_assert_eq!(id1, id2);
                prop_assert_eq!(registry.get(id2), Some(url2.as_str()));
            }

            /// Clear resets everything; old IDs return None.
            #[test]
            fn clear_resets(count in 1usize..15) {
                let mut registry = LinkRegistry::new();
                let mut ids = Vec::new();
                for i in 0..count {
                    ids.push(registry.register(&format!("https://c{i}.com")));
                }
                registry.clear();
                prop_assert!(registry.is_empty());
                for id in &ids {
                    prop_assert_eq!(registry.get(*id), None);
                }
            }
        }
    }
}
