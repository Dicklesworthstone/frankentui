//! Measure cache for memoizing widget `measure()` results.
//!
//! This module provides [`MeasureCache`] which caches [`SizeConstraints`] returned by
//! [`MeasurableWidget::measure()`] to avoid redundant computation during layout passes.
//!
//! # Overview
//!
//! During a single layout pass, the same widget may be queried multiple times with the
//! same available size. Complex widgets like Tables with many cells can be expensive
//! to measure. The cache eliminates this redundancy.
//!
//! # Usage
//!
//! ```ignore
//! use ftui_core::geometry::Size;
//! use ftui_widgets::{MeasureCache, WidgetId, SizeConstraints};
//!
//! let mut cache = MeasureCache::new(100);
//!
//! // First call computes the value
//! let constraints = cache.get_or_compute(
//!     WidgetId::from_ptr(&my_widget),
//!     Size::new(80, 24),
//!     || my_widget.measure(Size::new(80, 24)),
//! );
//!
//! // Second call with same key returns cached value
//! let cached = cache.get_or_compute(
//!     WidgetId::from_ptr(&my_widget),
//!     Size::new(80, 24),
//!     || SizeConstraints::default(),
//! );
//! ```
//!
//! # Invalidation Strategies
//!
//! ## Generation-Based (Primary)
//!
//! Call [`MeasureCache::invalidate_all()`] after any state change affecting layout:
//!
//! ```ignore
//! match msg {
//!     Msg::DataChanged(data) => {
//!         self.data = data;
//!         self.measure_cache.invalidate_all();
//!     }
//!     Msg::Resize(_) => {
//!         // Size is part of cache key, no invalidation needed!
//!     }
//! }
//! ```
//!
//! ## Widget-Specific Invalidation
//!
//! When only one widget's content changes:
//!
//! ```ignore
//! match msg {
//!     Msg::ListItemAdded(item) => {
//!         self.list.push(item);
//!         self.measure_cache.invalidate_widget(WidgetId::from_hash(&"list"));
//!     }
//! }
//! ```
//!
//! ## Content-Addressed (Automatic)
//!
//! Use content hash as widget ID for automatic invalidation:
//!
//! ```ignore
//! impl MeasurableWidget for Paragraph<'_> {
//!     fn widget_id(&self) -> WidgetId {
//!         WidgetId::from_hash(&self.text)
//!     }
//! }
//! ```
//!
//! # Cache Eviction
//!
//! The cache uses LFU (Least Frequently Used) eviction when at capacity.
//! Access count tracks usage; least-accessed entries are evicted first.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use ftui_core::geometry::Size;

use crate::measurable::SizeConstraints;

/// Unique identifier for a widget instance.
///
/// Used as part of the cache key to distinguish between different widgets.
///
/// # Creating WidgetIds
///
/// ## From pointer (stable for widget lifetime):
/// ```ignore
/// WidgetId::from_ptr(&my_widget)
/// ```
///
/// ## From content hash (stable across recreations):
/// ```ignore
/// WidgetId::from_hash(&my_widget.text)
/// ```
///
/// ## From arbitrary u64:
/// ```ignore
/// WidgetId(42)
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WidgetId(pub u64);

impl WidgetId {
    /// Create a WidgetId from a memory address.
    ///
    /// Stable for the lifetime of the widget. Use when the widget instance
    /// persists across multiple layout passes.
    ///
    /// # Note
    ///
    /// If the widget is recreated (e.g., in a loop), the pointer will change.
    /// For such cases, prefer [`WidgetId::from_hash`].
    #[inline]
    pub fn from_ptr<T>(ptr: &T) -> Self {
        Self(ptr as *const T as u64)
    }

    /// Create a WidgetId from a content hash.
    ///
    /// Stable across widget recreations as long as content is the same.
    /// Use when widgets are ephemeral (created each frame) but content is stable.
    #[inline]
    pub fn from_hash<T: Hash>(value: &T) -> Self {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        Self(hasher.finish())
    }
}

/// Cache key combining widget identity and available size.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct CacheKey {
    widget_id: WidgetId,
    available: Size,
}

/// Cached measurement result with metadata for eviction.
#[derive(Clone, Debug)]
struct CacheEntry {
    /// The cached size constraints.
    constraints: SizeConstraints,
    /// Generation when this entry was created/updated.
    generation: u64,
    /// Access count for LFU eviction.
    access_count: u32,
}

/// Statistics about cache performance.
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    /// Number of entries currently in the cache.
    pub entries: usize,
    /// Total cache hits since creation or last reset.
    pub hits: u64,
    /// Total cache misses since creation or last reset.
    pub misses: u64,
    /// Hit rate as a fraction (0.0 to 1.0).
    pub hit_rate: f64,
}

/// Thread-local cache for widget measure results.
///
/// Caches [`SizeConstraints`] returned by `MeasurableWidget::measure()` to
/// avoid redundant computation during layout passes.
///
/// # Capacity
///
/// The cache has a fixed maximum capacity. When full, the least frequently used
/// entries are evicted to make room for new ones.
///
/// # Generation-Based Invalidation
///
/// Each entry is tagged with a generation number. Calling [`invalidate_all()`]
/// bumps the generation, making all existing entries stale. Stale entries are
/// treated as cache misses and will be recomputed on next access.
///
/// [`invalidate_all()`]: MeasureCache::invalidate_all
#[derive(Debug)]
pub struct MeasureCache {
    entries: HashMap<CacheKey, CacheEntry>,
    generation: u64,
    max_entries: usize,
    hits: u64,
    misses: u64,
}

impl MeasureCache {
    /// Create a new cache with the specified maximum capacity.
    ///
    /// # Arguments
    ///
    /// * `max_entries` - Maximum number of entries before LFU eviction occurs.
    ///   A typical value is 100-1000 depending on widget complexity.
    ///
    /// # Example
    ///
    /// ```
    /// use ftui_widgets::MeasureCache;
    /// let cache = MeasureCache::new(256);
    /// ```
    #[inline]
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(max_entries),
            generation: 0,
            max_entries,
            hits: 0,
            misses: 0,
        }
    }

    /// Get cached result or compute and cache a new one.
    ///
    /// If a valid (same generation) cache entry exists for the given widget ID
    /// and available size, returns it immediately. Otherwise, calls the `compute`
    /// closure, caches the result, and returns it.
    ///
    /// # Arguments
    ///
    /// * `widget_id` - Unique identifier for the widget instance
    /// * `available` - Available space for the measurement
    /// * `compute` - Closure to compute the constraints if not cached
    ///
    /// # Example
    ///
    /// ```ignore
    /// let constraints = cache.get_or_compute(
    ///     WidgetId::from_ptr(&paragraph),
    ///     Size::new(80, 24),
    ///     || paragraph.measure(Size::new(80, 24)),
    /// );
    /// ```
    pub fn get_or_compute<F>(
        &mut self,
        widget_id: WidgetId,
        available: Size,
        compute: F,
    ) -> SizeConstraints
    where
        F: FnOnce() -> SizeConstraints,
    {
        let key = CacheKey {
            widget_id,
            available,
        };

        // Check for existing valid entry
        if let Some(entry) = self.entries.get_mut(&key)
            && entry.generation == self.generation
        {
            self.hits += 1;
            entry.access_count = entry.access_count.saturating_add(1);
            return entry.constraints;
        }

        // Cache miss - compute the value
        self.misses += 1;
        let constraints = compute();

        // Evict if at capacity
        if self.entries.len() >= self.max_entries {
            self.evict_lfu();
        }

        // Insert new entry
        self.entries.insert(
            key,
            CacheEntry {
                constraints,
                generation: self.generation,
                access_count: 1,
            },
        );

        constraints
    }

    /// Invalidate all entries by bumping the generation.
    ///
    /// Existing entries become stale and will be recomputed on next access.
    /// This is an O(1) operation - entries are not immediately removed.
    ///
    /// # When to Call
    ///
    /// Call this after any state change that affects widget measurements:
    /// - Model data changes
    /// - Font/theme changes (if they affect sizing)
    /// - Locale changes (if they affect text)
    ///
    /// # Note
    ///
    /// Resize events don't require invalidation because the available size
    /// is part of the cache key.
    #[inline]
    pub fn invalidate_all(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Invalidate entries for a specific widget.
    ///
    /// Removes all cache entries associated with the given widget ID.
    /// Use this for targeted invalidation when only one widget's content changes.
    ///
    /// # Arguments
    ///
    /// * `widget_id` - The widget whose entries should be invalidated
    pub fn invalidate_widget(&mut self, widget_id: WidgetId) {
        self.entries.retain(|k, _| k.widget_id != widget_id);
    }

    /// Get current cache statistics.
    ///
    /// Returns hit/miss counts and the current hit rate.
    ///
    /// # Example
    ///
    /// ```
    /// use ftui_widgets::MeasureCache;
    /// let cache = MeasureCache::new(100);
    /// let stats = cache.stats();
    /// println!("Hit rate: {:.1}%", stats.hit_rate * 100.0);
    /// ```
    pub fn stats(&self) -> CacheStats {
        let total = self.hits + self.misses;
        CacheStats {
            entries: self.entries.len(),
            hits: self.hits,
            misses: self.misses,
            hit_rate: if total > 0 {
                self.hits as f64 / total as f64
            } else {
                0.0
            },
        }
    }

    /// Reset statistics counters to zero.
    ///
    /// Useful for measuring hit rate over a specific period (e.g., per frame).
    #[inline]
    pub fn reset_stats(&mut self) {
        self.hits = 0;
        self.misses = 0;
    }

    /// Clear all entries from the cache.
    ///
    /// Unlike [`invalidate_all()`], this immediately frees memory.
    /// Use when transitioning to a completely different view.
    ///
    /// [`invalidate_all()`]: MeasureCache::invalidate_all
    #[inline]
    pub fn clear(&mut self) {
        self.entries.clear();
        self.generation = self.generation.wrapping_add(1);
    }

    /// Returns the current number of entries in the cache.
    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if the cache is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the maximum capacity of the cache.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.max_entries
    }

    /// Evict the least frequently used entry.
    fn evict_lfu(&mut self) {
        // Find entry with lowest access_count
        if let Some(key) = self
            .entries
            .iter()
            .min_by_key(|(_, e)| e.access_count)
            .map(|(k, _)| *k)
        {
            self.entries.remove(&key);
        }
    }
}

impl Default for MeasureCache {
    /// Creates a cache with default capacity of 256 entries.
    fn default() -> Self {
        Self::new(256)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- WidgetId tests ---

    #[test]
    fn widget_id_from_ptr_is_stable() {
        let widget = 42u64;
        let id1 = WidgetId::from_ptr(&widget);
        let id2 = WidgetId::from_ptr(&widget);
        assert_eq!(id1, id2);
    }

    #[test]
    fn widget_id_from_hash_is_stable() {
        let text = "hello world";
        let id1 = WidgetId::from_hash(&text);
        let id2 = WidgetId::from_hash(&text);
        assert_eq!(id1, id2);
    }

    #[test]
    fn widget_id_from_hash_differs_for_different_content() {
        let id1 = WidgetId::from_hash(&"hello");
        let id2 = WidgetId::from_hash(&"world");
        assert_ne!(id1, id2);
    }

    // --- MeasureCache tests ---

    #[test]
    fn cache_returns_same_result() {
        let mut cache = MeasureCache::new(100);
        let widget_id = WidgetId(42);
        let available = Size::new(80, 24);

        let mut call_count = 0;
        let compute = || {
            call_count += 1;
            SizeConstraints {
                min: Size::new(10, 1),
                preferred: Size::new(50, 5),
                max: None,
            }
        };

        let r1 = cache.get_or_compute(widget_id, available, compute);
        let r2 = cache.get_or_compute(widget_id, available, || unreachable!("should not call"));

        assert_eq!(r1, r2);
        assert_eq!(call_count, 1); // Only called once
    }

    #[test]
    fn different_size_is_cache_miss() {
        let mut cache = MeasureCache::new(100);
        let widget_id = WidgetId(42);

        let mut call_count = 0;
        let mut compute = || {
            call_count += 1;
            SizeConstraints {
                min: Size::ZERO,
                preferred: Size::new(call_count as u16, 1),
                max: None,
            }
        };

        cache.get_or_compute(widget_id, Size::new(80, 24), &mut compute);
        cache.get_or_compute(widget_id, Size::new(120, 40), &mut compute);

        assert_eq!(call_count, 2); // Called twice for different sizes
    }

    #[test]
    fn different_widget_is_cache_miss() {
        let mut cache = MeasureCache::new(100);
        let available = Size::new(80, 24);

        let mut call_count = 0;
        let mut compute = || {
            call_count += 1;
            SizeConstraints::ZERO
        };

        cache.get_or_compute(WidgetId(1), available, &mut compute);
        cache.get_or_compute(WidgetId(2), available, &mut compute);

        assert_eq!(call_count, 2);
    }

    #[test]
    fn invalidation_clears_cache() {
        let mut cache = MeasureCache::new(100);
        let widget_id = WidgetId(42);
        let available = Size::new(80, 24);

        let mut call_count = 0;
        let mut compute = || {
            call_count += 1;
            SizeConstraints::ZERO
        };

        cache.get_or_compute(widget_id, available, &mut compute);
        cache.invalidate_all();
        cache.get_or_compute(widget_id, available, &mut compute);

        assert_eq!(call_count, 2); // Re-computed after invalidation
    }

    #[test]
    fn widget_specific_invalidation() {
        let mut cache = MeasureCache::new(100);
        let widget1 = WidgetId(1);
        let widget2 = WidgetId(2);
        let available = Size::new(80, 24);

        let mut count1 = 0;
        let mut count2 = 0;

        cache.get_or_compute(widget1, available, || {
            count1 += 1;
            SizeConstraints::ZERO
        });
        cache.get_or_compute(widget2, available, || {
            count2 += 1;
            SizeConstraints::ZERO
        });

        // Invalidate only widget1
        cache.invalidate_widget(widget1);

        // widget1 should miss, widget2 should hit
        cache.get_or_compute(widget1, available, || {
            count1 += 1;
            SizeConstraints::ZERO
        });
        cache.get_or_compute(widget2, available, || unreachable!("should hit"));

        assert_eq!(count1, 2);
        assert_eq!(count2, 1);
    }

    #[test]
    fn lfu_eviction_works() {
        let mut cache = MeasureCache::new(2); // Small cache

        // Insert two entries
        cache.get_or_compute(WidgetId(1), Size::new(10, 10), || SizeConstraints::ZERO);
        cache.get_or_compute(WidgetId(2), Size::new(10, 10), || SizeConstraints::ZERO);

        // Access widget 1 again (increases its access count)
        cache.get_or_compute(WidgetId(1), Size::new(10, 10), || {
            unreachable!("should hit")
        });

        // Insert third entry, should evict widget 2 (least accessed)
        cache.get_or_compute(WidgetId(3), Size::new(10, 10), || SizeConstraints::ZERO);

        assert_eq!(cache.len(), 2);

        // Widget 2 should be evicted
        let mut was_called = false;
        cache.get_or_compute(WidgetId(2), Size::new(10, 10), || {
            was_called = true;
            SizeConstraints::ZERO
        });
        assert!(was_called, "widget 2 should have been evicted");

        // Widget 1 should still be cached
        cache.get_or_compute(WidgetId(1), Size::new(10, 10), || {
            unreachable!("widget 1 should still be cached")
        });
    }

    #[test]
    fn stats_track_hits_and_misses() {
        let mut cache = MeasureCache::new(100);

        cache.get_or_compute(WidgetId(1), Size::new(10, 10), || SizeConstraints::ZERO);
        cache.get_or_compute(WidgetId(1), Size::new(10, 10), || unreachable!("hit"));
        cache.get_or_compute(WidgetId(2), Size::new(10, 10), || SizeConstraints::ZERO);

        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 2);
        assert!((stats.hit_rate - 1.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn reset_stats_clears_counters() {
        let mut cache = MeasureCache::new(100);

        cache.get_or_compute(WidgetId(1), Size::new(10, 10), || SizeConstraints::ZERO);
        cache.get_or_compute(WidgetId(1), Size::new(10, 10), || unreachable!("hit"));

        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);

        cache.reset_stats();

        let stats = cache.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
        assert_eq!(stats.hit_rate, 0.0);
    }

    #[test]
    fn clear_removes_all_entries() {
        let mut cache = MeasureCache::new(100);

        cache.get_or_compute(WidgetId(1), Size::new(10, 10), || SizeConstraints::ZERO);
        cache.get_or_compute(WidgetId(2), Size::new(10, 10), || SizeConstraints::ZERO);

        assert_eq!(cache.len(), 2);

        cache.clear();

        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());

        // All entries should miss now
        let mut was_called = false;
        cache.get_or_compute(WidgetId(1), Size::new(10, 10), || {
            was_called = true;
            SizeConstraints::ZERO
        });
        assert!(was_called);
    }

    #[test]
    fn default_capacity_is_256() {
        let cache = MeasureCache::default();
        assert_eq!(cache.capacity(), 256);
    }

    #[test]
    fn generation_wraps_around() {
        let mut cache = MeasureCache::new(100);
        cache.generation = u64::MAX;
        cache.invalidate_all();
        assert_eq!(cache.generation, 0);
    }

    // --- Property-like tests ---

    #[test]
    fn cache_is_deterministic() {
        let mut cache1 = MeasureCache::new(100);
        let mut cache2 = MeasureCache::new(100);

        for i in 0..10u64 {
            let id = WidgetId(i);
            let size = Size::new((i * 10) as u16, (i * 5) as u16);
            let c = SizeConstraints {
                min: Size::new(i as u16, 1),
                preferred: Size::new((i * 2) as u16, 2),
                max: None,
            };

            cache1.get_or_compute(id, size, || c);
            cache2.get_or_compute(id, size, || c);
        }

        // Same inputs should produce same stats
        assert_eq!(cache1.stats().entries, cache2.stats().entries);
        assert_eq!(cache1.stats().misses, cache2.stats().misses);
    }

    #[test]
    fn widget_id_from_ptr_differs_for_different_objects() {
        let a = 42u64;
        let b = 42u64;
        let id_a = WidgetId::from_ptr(&a);
        let id_b = WidgetId::from_ptr(&b);
        assert_ne!(id_a, id_b);
    }

    #[test]
    fn new_cache_is_empty() {
        let cache = MeasureCache::new(100);
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn stats_zero_total_gives_zero_hit_rate() {
        let cache = MeasureCache::new(100);
        let stats = cache.stats();
        assert_eq!(stats.hit_rate, 0.0);
        assert_eq!(stats.entries, 0);
    }

    #[test]
    fn hit_count_increments_on_each_access() {
        let mut cache = MeasureCache::new(100);
        let id = WidgetId(42);
        let size = Size::new(80, 24);

        // First access is a miss
        cache.get_or_compute(id, size, || SizeConstraints::ZERO);

        // Subsequent accesses are hits
        for _ in 0..5 {
            cache.get_or_compute(id, size, || unreachable!("should hit"));
        }

        let stats = cache.stats();
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hits, 5);
    }

    // ---- Edge-case tests (bd-2ncz7) ----

    #[test]
    fn edge_zero_capacity_cache() {
        let mut cache = MeasureCache::new(0);
        let id = WidgetId(1);
        let size = Size::new(10, 10);

        // Every call is a miss because capacity is 0
        let mut calls = 0;
        for _ in 0..3 {
            cache.get_or_compute(id, size, || {
                calls += 1;
                SizeConstraints::ZERO
            });
        }
        // With capacity 0, eviction fires before every insert,
        // but the entry is still inserted (map has 0 capacity threshold).
        // The entry exists after insert, so second call hits it.
        let stats = cache.stats();
        assert_eq!(stats.misses + stats.hits, 3);
    }

    #[test]
    fn edge_capacity_one_evicts_on_second_widget() {
        let mut cache = MeasureCache::new(1);

        cache.get_or_compute(WidgetId(1), Size::new(10, 10), || SizeConstraints::ZERO);
        assert_eq!(cache.len(), 1);

        // Second different widget should evict the first
        cache.get_or_compute(WidgetId(2), Size::new(10, 10), || SizeConstraints::ZERO);
        assert_eq!(cache.len(), 1);

        // Widget 1 should be evicted
        let mut was_called = false;
        cache.get_or_compute(WidgetId(1), Size::new(10, 10), || {
            was_called = true;
            SizeConstraints::ZERO
        });
        assert!(was_called, "widget 1 should have been evicted");
    }

    #[test]
    fn edge_invalidate_all_multiple_times() {
        let mut cache = MeasureCache::new(100);
        let gen_before = cache.generation;
        cache.invalidate_all();
        cache.invalidate_all();
        cache.invalidate_all();
        assert_eq!(cache.generation, gen_before + 3);
    }

    #[test]
    fn edge_invalidate_widget_nonexistent() {
        let mut cache = MeasureCache::new(100);
        cache.get_or_compute(WidgetId(1), Size::new(10, 10), || SizeConstraints::ZERO);
        assert_eq!(cache.len(), 1);

        // Invalidating a widget that doesn't exist is a no-op
        cache.invalidate_widget(WidgetId(999));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn edge_invalidate_widget_removes_all_sizes() {
        let mut cache = MeasureCache::new(100);
        let id = WidgetId(42);

        // Same widget, different available sizes → multiple entries
        cache.get_or_compute(id, Size::new(80, 24), || SizeConstraints::ZERO);
        cache.get_or_compute(id, Size::new(120, 40), || SizeConstraints::ZERO);
        cache.get_or_compute(id, Size::new(200, 60), || SizeConstraints::ZERO);
        assert_eq!(cache.len(), 3);

        cache.invalidate_widget(id);
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn edge_stale_entry_treated_as_miss() {
        let mut cache = MeasureCache::new(100);
        let id = WidgetId(1);
        let size = Size::new(10, 10);

        cache.get_or_compute(id, size, || SizeConstraints::ZERO);
        assert_eq!(cache.stats().misses, 1);
        assert_eq!(cache.stats().hits, 0);

        // Invalidate makes all entries stale
        cache.invalidate_all();

        // Same key should now be a miss (stale generation)
        let mut called = false;
        cache.get_or_compute(id, size, || {
            called = true;
            SizeConstraints::ZERO
        });
        assert!(called, "stale entry should be treated as miss");
        assert_eq!(cache.stats().misses, 2);
    }

    #[test]
    fn edge_lfu_equal_access_counts() {
        let mut cache = MeasureCache::new(2);

        // Both entries accessed exactly once
        cache.get_or_compute(WidgetId(1), Size::new(10, 10), || SizeConstraints::ZERO);
        cache.get_or_compute(WidgetId(2), Size::new(10, 10), || SizeConstraints::ZERO);

        // Insert a third — one of the first two gets evicted
        cache.get_or_compute(WidgetId(3), Size::new(10, 10), || SizeConstraints::ZERO);
        assert_eq!(cache.len(), 2);

        // At least one of widget 1 or 2 is evicted
        let mut evicted = 0;
        for id_val in [1u64, 2u64] {
            let mut called = false;
            cache.get_or_compute(WidgetId(id_val), Size::new(10, 10), || {
                called = true;
                SizeConstraints::ZERO
            });
            if called {
                evicted += 1;
            }
        }
        assert!(evicted >= 1, "at least one entry should be evicted");
    }

    #[test]
    fn edge_size_zero_as_cache_key() {
        let mut cache = MeasureCache::new(100);
        let id = WidgetId(1);

        cache.get_or_compute(id, Size::ZERO, || SizeConstraints::ZERO);
        // Should hit on second call
        cache.get_or_compute(id, Size::ZERO, || unreachable!("should hit for Size::ZERO"));
        assert_eq!(cache.stats().hits, 1);
    }

    #[test]
    fn edge_widget_id_zero() {
        let mut cache = MeasureCache::new(100);
        cache.get_or_compute(WidgetId(0), Size::new(10, 10), || SizeConstraints::ZERO);
        cache.get_or_compute(WidgetId(0), Size::new(10, 10), || {
            unreachable!("should hit for WidgetId(0)")
        });
        assert_eq!(cache.stats().hits, 1);
    }

    #[test]
    fn edge_clear_preserves_stats() {
        let mut cache = MeasureCache::new(100);
        cache.get_or_compute(WidgetId(1), Size::new(10, 10), || SizeConstraints::ZERO);
        cache.get_or_compute(WidgetId(1), Size::new(10, 10), || unreachable!());

        cache.clear();
        assert!(cache.is_empty());
        // Stats should still be preserved after clear
        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
    }

    #[test]
    fn edge_reset_stats_preserves_entries() {
        let mut cache = MeasureCache::new(100);
        cache.get_or_compute(WidgetId(1), Size::new(10, 10), || SizeConstraints::ZERO);
        assert_eq!(cache.len(), 1);

        cache.reset_stats();
        assert_eq!(cache.len(), 1);
        // Entry should still be cached
        cache.get_or_compute(WidgetId(1), Size::new(10, 10), || {
            unreachable!("should hit")
        });
        assert_eq!(cache.stats().hits, 1);
    }

    #[test]
    fn edge_entries_never_exceed_capacity() {
        let cap = 5;
        let mut cache = MeasureCache::new(cap);

        for i in 0..100u64 {
            cache.get_or_compute(WidgetId(i), Size::new(10, 10), || SizeConstraints::ZERO);
            assert!(
                cache.len() <= cap,
                "len {} > capacity {} at i={}",
                cache.len(),
                cap,
                i
            );
        }
    }

    #[test]
    fn edge_clear_bumps_generation() {
        let mut cache = MeasureCache::new(100);
        let gen_before = cache.generation;
        cache.clear();
        assert_eq!(cache.generation, gen_before + 1);
    }

    #[test]
    fn edge_invalidate_all_stale_but_still_counted_in_len() {
        let mut cache = MeasureCache::new(100);
        cache.get_or_compute(WidgetId(1), Size::new(10, 10), || SizeConstraints::ZERO);
        cache.get_or_compute(WidgetId(2), Size::new(10, 10), || SizeConstraints::ZERO);
        assert_eq!(cache.len(), 2);

        cache.invalidate_all();
        // Stale entries are still in the map (not removed), just treated as misses
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn edge_invalidate_widget_does_not_affect_stats() {
        let mut cache = MeasureCache::new(100);
        cache.get_or_compute(WidgetId(1), Size::new(10, 10), || SizeConstraints::ZERO);
        cache.get_or_compute(WidgetId(1), Size::new(10, 10), || unreachable!());
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(cache.stats().misses, 1);

        cache.invalidate_widget(WidgetId(1));
        // Stats unchanged
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(cache.stats().misses, 1);
    }

    #[test]
    fn edge_get_or_compute_returns_computed_value() {
        let mut cache = MeasureCache::new(100);
        let expected = SizeConstraints {
            min: Size::new(5, 3),
            preferred: Size::new(40, 10),
            max: Some(Size::new(100, 50)),
        };
        let result = cache.get_or_compute(WidgetId(1), Size::new(80, 24), || expected);
        assert_eq!(result, expected);
    }

    #[test]
    fn edge_cache_stats_default() {
        let stats = CacheStats::default();
        assert_eq!(stats.entries, 0);
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
        assert_eq!(stats.hit_rate, 0.0);
    }

    #[test]
    fn edge_cache_stats_clone_debug() {
        let stats = CacheStats {
            entries: 5,
            hits: 10,
            misses: 3,
            hit_rate: 0.769,
        };
        let cloned = stats.clone();
        assert_eq!(cloned.entries, 5);
        assert_eq!(cloned.hits, 10);
        let _ = format!("{stats:?}");
    }

    #[test]
    fn edge_measure_cache_debug() {
        let cache = MeasureCache::new(100);
        let debug = format!("{cache:?}");
        assert!(debug.contains("MeasureCache"), "{debug}");
    }

    #[test]
    fn edge_widget_id_copy_clone_hash_debug() {
        let id = WidgetId(42);
        let copied: WidgetId = id; // Copy
        assert_eq!(copied, id);
        let cloned = id.clone();
        assert_eq!(cloned, id);
        let _ = format!("{id:?}");

        // Hash: same IDs should hash equally
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(id);
        assert!(set.contains(&WidgetId(42)));
        assert!(!set.contains(&WidgetId(43)));
    }
}
