#![forbid(unsafe_code)]

//! Bounded circular buffer for log storage.
//!
//! [`LogRing`] provides memory-efficient storage for log lines that evicts
//! oldest entries when full. It supports absolute indexing across the entire
//! history (even for evicted items) and optional overflow file persistence.
//!
//! # Example
//!
//! ```
//! use ftui_widgets::LogRing;
//!
//! let mut ring = LogRing::new(3);
//! ring.push("line 1");
//! ring.push("line 2");
//! ring.push("line 3");
//! ring.push("line 4"); // evicts "line 1"
//!
//! assert_eq!(ring.len(), 3);
//! assert_eq!(ring.total_count(), 4);
//! assert_eq!(ring.get(3), Some(&"line 4"));
//! assert_eq!(ring.get(0), None); // evicted
//! ```

use std::collections::VecDeque;
use std::ops::Range;

/// Circular buffer for log storage with FIFO eviction.
///
/// Memory-efficient storage that maintains a sliding window of the most recent
/// items. Older items are evicted when capacity is reached.
#[derive(Debug, Clone)]
pub struct LogRing<T> {
    /// Circular buffer storage
    ring: VecDeque<T>,

    /// Maximum capacity
    capacity: usize,

    /// Total items ever added (for accurate absolute indexing)
    total_count: usize,
}

impl<T> LogRing<T> {
    /// Create a new LogRing with the specified capacity.
    ///
    /// # Panics
    ///
    /// Panics if capacity is 0.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "LogRing capacity must be greater than 0");
        Self {
            ring: VecDeque::with_capacity(capacity),
            capacity,
            total_count: 0,
        }
    }

    /// Add an item to the ring.
    ///
    /// If the ring is at capacity, the oldest item is evicted first.
    pub fn push(&mut self, item: T) {
        self.total_count = self.total_count.saturating_add(1);

        if self.ring.len() >= self.capacity {
            self.ring.pop_front();
        }

        self.ring.push_back(item);
    }

    /// Add multiple items efficiently.
    pub fn extend(&mut self, items: impl IntoIterator<Item = T>) {
        for item in items {
            self.push(item);
        }
    }

    /// Get item by absolute index (across entire history).
    ///
    /// Returns `None` if the index is out of range or the item has been evicted.
    #[must_use]
    pub fn get(&self, absolute_idx: usize) -> Option<&T> {
        let ring_start = self.first_index();

        if absolute_idx >= ring_start && absolute_idx < self.total_count {
            self.ring.get(absolute_idx - ring_start)
        } else {
            None
        }
    }

    /// Get mutable reference by absolute index.
    #[must_use]
    pub fn get_mut(&mut self, absolute_idx: usize) -> Option<&mut T> {
        let ring_start = self.first_index();

        if absolute_idx >= ring_start && absolute_idx < self.total_count {
            self.ring.get_mut(absolute_idx - ring_start)
        } else {
            None
        }
    }

    /// Get a range of items by absolute indices.
    ///
    /// Returns references to items that are still in memory within the range.
    /// Items that have been evicted are skipped.
    pub fn get_range(&self, range: Range<usize>) -> impl Iterator<Item = &T> {
        let ring_start = self.first_index();
        let ring_end = self.total_count;

        // Clamp range to what's in memory
        let start = range.start.max(ring_start);
        let end = range.end.min(ring_end);

        (start..end).filter_map(move |i| self.get(i))
    }

    /// Total items ever added (including evicted).
    #[must_use]
    pub const fn total_count(&self) -> usize {
        self.total_count
    }

    /// Number of items currently in memory.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    /// Check if the ring is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// Maximum capacity of the ring.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// First absolute index still in memory.
    #[must_use]
    pub fn first_index(&self) -> usize {
        self.total_count.saturating_sub(self.ring.len())
    }

    /// Last absolute index (most recent item).
    ///
    /// Returns `None` if the ring is empty.
    #[must_use]
    pub fn last_index(&self) -> Option<usize> {
        if self.total_count > 0 {
            Some(self.total_count - 1)
        } else {
            None
        }
    }

    /// Check if an absolute index is still in memory.
    #[must_use]
    pub fn is_in_memory(&self, absolute_idx: usize) -> bool {
        absolute_idx >= self.first_index() && absolute_idx < self.total_count
    }

    /// Number of items that have been evicted.
    #[must_use]
    pub fn evicted_count(&self) -> usize {
        self.first_index()
    }

    /// Clear all items.
    ///
    /// Note: `total_count` is preserved for consistency with absolute indexing.
    pub fn clear(&mut self) {
        self.ring.clear();
    }

    /// Clear all items and reset counters.
    pub fn reset(&mut self) {
        self.ring.clear();
        self.total_count = 0;
    }

    /// Get the most recent item.
    #[must_use]
    pub fn back(&self) -> Option<&T> {
        self.ring.back()
    }

    /// Get the oldest item still in memory.
    #[must_use]
    pub fn front(&self) -> Option<&T> {
        self.ring.front()
    }

    /// Iterate over items currently in memory (oldest to newest).
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> {
        self.ring.iter()
    }

    /// Iterate over items with their absolute indices.
    pub fn iter_indexed(&self) -> impl DoubleEndedIterator<Item = (usize, &T)> {
        let start = self.first_index();
        self.ring
            .iter()
            .enumerate()
            .map(move |(i, item)| (start + i, item))
    }

    /// Drain all items from the ring.
    pub fn drain(&mut self) -> impl Iterator<Item = T> + '_ {
        self.ring.drain(..)
    }
}

impl<T> Default for LogRing<T> {
    fn default() -> Self {
        Self::new(1024) // Reasonable default capacity
    }
}

impl<T> Extend<T> for LogRing<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for item in iter {
            self.push(item);
        }
    }
}

impl<T> FromIterator<T> for LogRing<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let items: Vec<T> = iter.into_iter().collect();
        let capacity = items.len().max(1);
        let mut ring = Self::new(capacity);
        ring.extend(items);
        ring
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_empty_ring() {
        let ring: LogRing<i32> = LogRing::new(10);
        assert!(ring.is_empty());
        assert_eq!(ring.len(), 0);
        assert_eq!(ring.total_count(), 0);
        assert_eq!(ring.capacity(), 10);
    }

    #[test]
    #[should_panic(expected = "capacity must be greater than 0")]
    fn new_panics_on_zero_capacity() {
        let _ring: LogRing<i32> = LogRing::new(0);
    }

    #[test]
    fn push_adds_items() {
        let mut ring = LogRing::new(5);
        ring.push("a");
        ring.push("b");
        ring.push("c");

        assert_eq!(ring.len(), 3);
        assert_eq!(ring.total_count(), 3);
        assert_eq!(ring.get(0), Some(&"a"));
        assert_eq!(ring.get(1), Some(&"b"));
        assert_eq!(ring.get(2), Some(&"c"));
    }

    #[test]
    fn push_evicts_oldest_when_full() {
        let mut ring = LogRing::new(3);
        ring.push(1);
        ring.push(2);
        ring.push(3);
        ring.push(4); // evicts 1
        ring.push(5); // evicts 2

        assert_eq!(ring.len(), 3);
        assert_eq!(ring.total_count(), 5);
        assert_eq!(ring.get(0), None); // evicted
        assert_eq!(ring.get(1), None); // evicted
        assert_eq!(ring.get(2), Some(&3));
        assert_eq!(ring.get(3), Some(&4));
        assert_eq!(ring.get(4), Some(&5));
    }

    #[test]
    fn first_and_last_index() {
        let mut ring = LogRing::new(3);
        assert_eq!(ring.first_index(), 0);
        assert_eq!(ring.last_index(), None);

        ring.push("a");
        ring.push("b");
        assert_eq!(ring.first_index(), 0);
        assert_eq!(ring.last_index(), Some(1));

        ring.push("c");
        ring.push("d"); // evicts "a"
        assert_eq!(ring.first_index(), 1);
        assert_eq!(ring.last_index(), Some(3));
    }

    #[test]
    fn get_range_returns_available_items() {
        let mut ring = LogRing::new(3);
        ring.push("a");
        ring.push("b");
        ring.push("c");
        ring.push("d"); // evicts "a"

        let items: Vec<_> = ring.get_range(0..5).collect();
        assert_eq!(items, vec![&"b", &"c", &"d"]);

        let items: Vec<_> = ring.get_range(2..4).collect();
        assert_eq!(items, vec![&"c", &"d"]);
    }

    #[test]
    fn is_in_memory() {
        let mut ring = LogRing::new(2);
        ring.push(1);
        ring.push(2);
        ring.push(3); // evicts 1

        assert!(!ring.is_in_memory(0));
        assert!(ring.is_in_memory(1));
        assert!(ring.is_in_memory(2));
        assert!(!ring.is_in_memory(3));
    }

    #[test]
    fn evicted_count() {
        let mut ring = LogRing::new(2);
        assert_eq!(ring.evicted_count(), 0);

        ring.push(1);
        ring.push(2);
        assert_eq!(ring.evicted_count(), 0);

        ring.push(3); // evicts 1
        assert_eq!(ring.evicted_count(), 1);

        ring.push(4); // evicts 2
        assert_eq!(ring.evicted_count(), 2);
    }

    #[test]
    fn clear_preserves_total_count() {
        let mut ring = LogRing::new(5);
        ring.push(1);
        ring.push(2);
        ring.push(3);

        ring.clear();
        assert!(ring.is_empty());
        assert_eq!(ring.total_count(), 3);
        assert_eq!(ring.first_index(), 3);
    }

    #[test]
    fn reset_clears_everything() {
        let mut ring = LogRing::new(5);
        ring.push(1);
        ring.push(2);
        ring.push(3);

        ring.reset();
        assert!(ring.is_empty());
        assert_eq!(ring.total_count(), 0);
        assert_eq!(ring.first_index(), 0);
    }

    #[test]
    fn front_and_back() {
        let mut ring = LogRing::new(3);
        assert_eq!(ring.front(), None);
        assert_eq!(ring.back(), None);

        ring.push("first");
        ring.push("middle");
        ring.push("last");

        assert_eq!(ring.front(), Some(&"first"));
        assert_eq!(ring.back(), Some(&"last"));

        ring.push("newest"); // evicts "first"
        assert_eq!(ring.front(), Some(&"middle"));
        assert_eq!(ring.back(), Some(&"newest"));
    }

    #[test]
    fn iter_yields_oldest_to_newest() {
        let mut ring = LogRing::new(3);
        ring.push(1);
        ring.push(2);
        ring.push(3);

        let items: Vec<_> = ring.iter().copied().collect();
        assert_eq!(items, vec![1, 2, 3]);
    }

    #[test]
    fn iter_indexed_includes_absolute_indices() {
        let mut ring = LogRing::new(2);
        ring.push("a");
        ring.push("b");
        ring.push("c"); // evicts "a"

        let indexed: Vec<_> = ring.iter_indexed().collect();
        assert_eq!(indexed, vec![(1, &"b"), (2, &"c")]);
    }

    #[test]
    fn extend_adds_multiple_items() {
        let mut ring = LogRing::new(5);
        ring.extend(vec![1, 2, 3]);

        assert_eq!(ring.len(), 3);
        assert_eq!(ring.total_count(), 3);
    }

    #[test]
    fn from_iter_creates_ring() {
        let ring: LogRing<i32> = vec![1, 2, 3, 4, 5].into_iter().collect();
        assert_eq!(ring.len(), 5);
        assert_eq!(ring.capacity(), 5);
    }

    #[test]
    fn default_has_reasonable_capacity() {
        let ring: LogRing<i32> = LogRing::default();
        assert_eq!(ring.capacity(), 1024);
    }

    #[test]
    fn get_mut_allows_modification() {
        let mut ring = LogRing::new(3);
        ring.push(1);
        ring.push(2);

        if let Some(item) = ring.get_mut(0) {
            *item = 10;
        }

        assert_eq!(ring.get(0), Some(&10));
    }

    #[test]
    fn drain_removes_all_items() {
        let mut ring = LogRing::new(5);
        ring.push(1);
        ring.push(2);
        ring.push(3);

        let drained: Vec<_> = ring.drain().collect();
        assert_eq!(drained, vec![1, 2, 3]);
        assert!(ring.is_empty());
        assert_eq!(ring.total_count(), 3); // preserved
    }

    #[test]
    fn handles_large_total_count() {
        let mut ring = LogRing::new(2);
        for i in 0..1000 {
            ring.push(i);
        }

        assert_eq!(ring.len(), 2);
        assert_eq!(ring.total_count(), 1000);
        assert_eq!(ring.first_index(), 998);
        assert_eq!(ring.get(998), Some(&998));
        assert_eq!(ring.get(999), Some(&999));
    }

    // ── Edge-case tests (bd-2n2oo) ──────────────────────────

    #[test]
    fn capacity_one_ring() {
        let mut ring = LogRing::new(1);
        ring.push("a");
        assert_eq!(ring.len(), 1);
        assert_eq!(ring.get(0), Some(&"a"));

        ring.push("b"); // evicts "a"
        assert_eq!(ring.len(), 1);
        assert_eq!(ring.total_count(), 2);
        assert_eq!(ring.get(0), None);
        assert_eq!(ring.get(1), Some(&"b"));
        assert_eq!(ring.first_index(), 1);
    }

    #[test]
    fn get_mut_evicted_returns_none() {
        let mut ring = LogRing::new(2);
        ring.push(1);
        ring.push(2);
        ring.push(3); // evicts 1
        assert!(ring.get_mut(0).is_none());
    }

    #[test]
    fn get_mut_beyond_total_returns_none() {
        let mut ring = LogRing::new(5);
        ring.push(1);
        assert!(ring.get_mut(1).is_none());
        assert!(ring.get_mut(100).is_none());
    }

    #[test]
    fn get_beyond_total_count() {
        let mut ring = LogRing::new(5);
        ring.push(1);
        assert_eq!(ring.get(1), None); // total_count=1, idx 1 is out of range
        assert_eq!(ring.get(usize::MAX), None);
    }

    #[test]
    fn get_range_empty_range() {
        let mut ring = LogRing::new(5);
        ring.push(1);
        ring.push(2);
        let items: Vec<_> = ring.get_range(1..1).collect();
        assert!(items.is_empty());
    }

    #[test]
    fn get_range_inverted_start_gt_end() {
        let mut ring = LogRing::new(5);
        ring.push(1);
        ring.push(2);
        // Range 5..2 is empty since start > end
        let items: Vec<_> = ring.get_range(5..2).collect();
        assert!(items.is_empty());
    }

    #[test]
    fn get_range_fully_evicted() {
        let mut ring = LogRing::new(2);
        ring.push(1);
        ring.push(2);
        ring.push(3);
        ring.push(4);
        // indices 0..2 are evicted, first_index=2
        let items: Vec<_> = ring.get_range(0..2).collect();
        assert!(items.is_empty());
    }

    #[test]
    fn get_range_fully_future() {
        let mut ring = LogRing::new(5);
        ring.push(1);
        // total_count=1, range 5..10 is entirely future
        let items: Vec<_> = ring.get_range(5..10).collect();
        assert!(items.is_empty());
    }

    #[test]
    fn get_range_partial_overlap() {
        let mut ring = LogRing::new(3);
        ring.push(10);
        ring.push(20);
        ring.push(30);
        ring.push(40); // evicts 10, first_index=1
        // Request range 0..5, only 1..4 is in memory
        let items: Vec<_> = ring.get_range(0..5).collect();
        assert_eq!(items, vec![&20, &30, &40]);
    }

    #[test]
    fn iter_empty_ring() {
        let ring: LogRing<i32> = LogRing::new(5);
        assert_eq!(ring.iter().count(), 0);
    }

    #[test]
    fn iter_indexed_empty_ring() {
        let ring: LogRing<i32> = LogRing::new(5);
        assert_eq!(ring.iter_indexed().count(), 0);
    }

    #[test]
    fn iter_reverse() {
        let mut ring = LogRing::new(3);
        ring.push(1);
        ring.push(2);
        ring.push(3);
        let rev: Vec<_> = ring.iter().rev().copied().collect();
        assert_eq!(rev, vec![3, 2, 1]);
    }

    #[test]
    fn iter_indexed_reverse() {
        let mut ring = LogRing::new(2);
        ring.push("a");
        ring.push("b");
        ring.push("c"); // evicts "a"
        let rev: Vec<_> = ring.iter_indexed().rev().collect();
        assert_eq!(rev, vec![(2, &"c"), (1, &"b")]);
    }

    #[test]
    fn extend_empty_iterator() {
        let mut ring = LogRing::new(5);
        ring.push(1);
        ring.extend(std::iter::empty::<i32>());
        assert_eq!(ring.len(), 1);
        assert_eq!(ring.total_count(), 1);
    }

    #[test]
    fn extend_trait_impl() {
        let mut ring = LogRing::new(5);
        Extend::extend(&mut ring, vec![1, 2, 3]);
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.total_count(), 3);
    }

    #[test]
    fn from_iter_empty() {
        let ring: LogRing<i32> = std::iter::empty().collect();
        assert_eq!(ring.capacity(), 1); // max(0, 1)
        assert!(ring.is_empty());
    }

    #[test]
    fn from_iter_single() {
        let ring: LogRing<i32> = std::iter::once(42).collect();
        assert_eq!(ring.capacity(), 1);
        assert_eq!(ring.len(), 1);
        assert_eq!(ring.get(0), Some(&42));
    }

    #[test]
    fn clone_independence() {
        let mut ring = LogRing::new(5);
        ring.push(1);
        ring.push(2);
        let mut cloned = ring.clone();
        cloned.push(3);
        assert_eq!(ring.len(), 2);
        assert_eq!(cloned.len(), 3);
        assert_eq!(ring.total_count(), 2);
        assert_eq!(cloned.total_count(), 3);
    }

    #[test]
    fn debug_format() {
        let mut ring = LogRing::new(3);
        ring.push(1);
        let dbg = format!("{:?}", ring);
        assert!(dbg.contains("LogRing"));
    }

    #[test]
    fn drain_empty_ring() {
        let mut ring: LogRing<i32> = LogRing::new(5);
        let drained: Vec<_> = ring.drain().collect();
        assert!(drained.is_empty());
        assert_eq!(ring.total_count(), 0);
    }

    #[test]
    fn clear_then_push_continues_absolute_index() {
        let mut ring = LogRing::new(5);
        ring.push("a");
        ring.push("b");
        ring.clear();
        assert_eq!(ring.total_count(), 2);
        assert_eq!(ring.first_index(), 2);

        ring.push("c");
        assert_eq!(ring.total_count(), 3);
        assert_eq!(ring.first_index(), 2);
        assert_eq!(ring.get(2), Some(&"c"));
        assert_eq!(ring.get(0), None); // old indices gone
        assert_eq!(ring.get(1), None);
    }

    #[test]
    fn reset_then_push_starts_fresh() {
        let mut ring = LogRing::new(5);
        ring.push("a");
        ring.push("b");
        ring.reset();
        ring.push("c");
        assert_eq!(ring.total_count(), 1);
        assert_eq!(ring.first_index(), 0);
        assert_eq!(ring.get(0), Some(&"c"));
    }

    #[test]
    fn last_index_after_clear() {
        let mut ring = LogRing::new(5);
        ring.push(1);
        ring.clear();
        assert_eq!(ring.last_index(), Some(0));
        // total_count is 1, so last_index = 0
    }

    #[test]
    fn last_index_after_reset() {
        let mut ring = LogRing::new(5);
        ring.push(1);
        ring.reset();
        assert_eq!(ring.last_index(), None);
    }

    #[test]
    fn front_back_after_clear() {
        let mut ring = LogRing::new(5);
        ring.push(1);
        ring.clear();
        assert_eq!(ring.front(), None);
        assert_eq!(ring.back(), None);
    }

    #[test]
    fn is_in_memory_at_exact_boundaries() {
        let mut ring = LogRing::new(3);
        ring.push(10);
        ring.push(20);
        ring.push(30);
        // first_index=0, total_count=3
        assert!(ring.is_in_memory(0));
        assert!(ring.is_in_memory(2));
        assert!(!ring.is_in_memory(3)); // == total_count, out of range
    }

    #[test]
    fn extend_causes_eviction() {
        let mut ring = LogRing::new(3);
        ring.extend(vec![1, 2, 3, 4, 5]);
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.total_count(), 5);
        assert_eq!(ring.get(2), Some(&3));
        assert_eq!(ring.get(4), Some(&5));
        assert_eq!(ring.get(0), None); // evicted
    }

    #[test]
    fn get_range_exact_memory_window() {
        let mut ring = LogRing::new(3);
        ring.push(1);
        ring.push(2);
        ring.push(3);
        ring.push(4); // first_index=1
        let items: Vec<_> = ring.get_range(1..4).collect();
        assert_eq!(items, vec![&2, &3, &4]);
    }

    #[test]
    fn push_with_string_types() {
        let mut ring = LogRing::new(2);
        ring.push(String::from("hello"));
        ring.push(String::from("world"));
        ring.push(String::from("foo")); // evicts "hello"
        assert_eq!(ring.get(1), Some(&String::from("world")));
        assert_eq!(ring.get(2), Some(&String::from("foo")));
    }
}
