//! Layout cache for memoizing layout computation results.
//!
//! This module provides [`LayoutCache`] which caches the `Vec<Rect>` results from
//! layout computations to avoid redundant constraint solving during rendering.
//!
//! # Overview
//!
//! Layout computation (constraint solving, flex distribution) can be expensive for
//! complex nested layouts. During a single frame, the same layout may be queried
//! multiple times with identical parameters. The cache eliminates this redundancy.
//!
//! # Usage
//!
//! ```ignore
//! use ftui_layout::{Flex, Constraint, LayoutCache, LayoutCacheKey, Direction};
//! use ftui_core::geometry::Rect;
//!
//! let mut cache = LayoutCache::new(64);
//!
//! let flex = Flex::horizontal()
//!     .constraints([Constraint::Percentage(50.0), Constraint::Fill]);
//!
//! let area = Rect::new(0, 0, 80, 24);
//!
//! // First call computes and caches
//! let rects = flex.split_cached(area, &mut cache);
//!
//! // Second call returns cached result
//! let cached = flex.split_cached(area, &mut cache);
//! ```
//!
//! # Invalidation
//!
//! ## Generation-Based (Primary)
//!
//! Call [`LayoutCache::invalidate_all()`] after any state change affecting layouts:
//!
//! ```ignore
//! match msg {
//!     Msg::DataChanged(_) => {
//!         self.layout_cache.invalidate_all();
//!     }
//!     Msg::Resize(_) => {
//!         // Area is part of cache key, no invalidation needed!
//!     }
//! }
//! ```
//!
//! # Cache Eviction
//!
//! The cache uses LRU (Least Recently Used) eviction when at capacity.

use std::hash::{Hash, Hasher};

use ftui_core::geometry::Rect;
use rustc_hash::{FxHashMap, FxHasher};

use crate::{Constraint, Direction, LayoutSizeHint};

/// Key for layout cache lookups.
///
/// Includes all parameters that affect layout computation:
/// - The available area (stored as components for Hash)
/// - A fingerprint of all constraints
/// - The layout direction
/// - Optionally, a fingerprint of intrinsic size hints
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct LayoutCacheKey {
    /// Area x-coordinate.
    pub area_x: u16,
    /// Area y-coordinate.
    pub area_y: u16,
    /// Area width.
    pub area_width: u16,
    /// Area height.
    pub area_height: u16,
    /// Hash fingerprint of constraints.
    pub constraints_hash: u64,
    /// Layout direction.
    pub direction: Direction,
    /// Hash fingerprint of intrinsic sizes (if using FitContent).
    pub intrinsics_hash: Option<u64>,
}

impl LayoutCacheKey {
    /// Create a new cache key from layout parameters.
    ///
    /// # Arguments
    ///
    /// * `area` - The available rectangle for layout
    /// * `constraints` - The constraint list
    /// * `direction` - Horizontal or Vertical layout
    /// * `intrinsics` - Optional size hints for FitContent constraints
    pub fn new(
        area: Rect,
        constraints: &[Constraint],
        direction: Direction,
        intrinsics: Option<&[LayoutSizeHint]>,
    ) -> Self {
        Self {
            area_x: area.x,
            area_y: area.y,
            area_width: area.width,
            area_height: area.height,
            constraints_hash: Self::hash_constraints(constraints),
            direction,
            intrinsics_hash: intrinsics.map(Self::hash_intrinsics),
        }
    }

    /// Reconstruct the area Rect from cached components.
    #[inline]
    pub fn area(&self) -> Rect {
        Rect::new(self.area_x, self.area_y, self.area_width, self.area_height)
    }

    /// Hash a slice of constraints.
    fn hash_constraints(constraints: &[Constraint]) -> u64 {
        let mut hasher = FxHasher::default();
        for c in constraints {
            // Hash each constraint's discriminant and value
            std::mem::discriminant(c).hash(&mut hasher);
            match c {
                Constraint::Fixed(v) => v.hash(&mut hasher),
                Constraint::Percentage(p) => p.to_bits().hash(&mut hasher),
                Constraint::Min(v) => v.hash(&mut hasher),
                Constraint::Max(v) => v.hash(&mut hasher),
                Constraint::Ratio(n, d) => {
                    fn gcd(mut a: u32, mut b: u32) -> u32 {
                        while b != 0 {
                            let t = b;
                            b = a % b;
                            a = t;
                        }
                        a
                    }
                    let divisor = gcd(*n, *d);
                    if let (Some(n_div), Some(d_div)) =
                        (n.checked_div(divisor), d.checked_div(divisor))
                    {
                        n_div.hash(&mut hasher);
                        d_div.hash(&mut hasher);
                    } else {
                        n.hash(&mut hasher);
                        d.hash(&mut hasher);
                    }
                }
                Constraint::Fill => {}
                Constraint::FitContent => {}
                Constraint::FitContentBounded { min, max } => {
                    min.hash(&mut hasher);
                    max.hash(&mut hasher);
                }
                Constraint::FitMin => {}
            }
        }
        hasher.finish()
    }

    /// Hash a slice of intrinsic size hints.
    fn hash_intrinsics(intrinsics: &[LayoutSizeHint]) -> u64 {
        let mut hasher = FxHasher::default();
        for hint in intrinsics {
            hint.min.hash(&mut hasher);
            hint.preferred.hash(&mut hasher);
            hint.max.hash(&mut hasher);
        }
        hasher.finish()
    }
}

/// Cached layout result with metadata for eviction.
#[derive(Clone, Debug)]
struct CachedLayoutEntry {
    /// The cached layout rectangles.
    chunks: Vec<Rect>,
    /// Generation when this entry was created/updated.
    generation: u64,
    /// Access count for LRU eviction.
    access_count: u32,
}

/// Statistics about layout cache performance.
#[derive(Debug, Clone, Default)]
pub struct LayoutCacheStats {
    /// Number of entries currently in the cache.
    pub entries: usize,
    /// Total cache hits since creation or last reset.
    pub hits: u64,
    /// Total cache misses since creation or last reset.
    pub misses: u64,
    /// Hit rate as a fraction (0.0 to 1.0).
    pub hit_rate: f64,
}

/// Cache for layout computation results.
///
/// Stores `Vec<Rect>` results keyed by [`LayoutCacheKey`] to avoid redundant
/// constraint solving during rendering.
///
/// # Capacity
///
/// The cache has a fixed maximum capacity. When full, the least recently used
/// entries are evicted to make room for new ones.
///
/// # Generation-Based Invalidation
///
/// Each entry is tagged with a generation number. Calling [`invalidate_all()`]
/// bumps the generation, making all existing entries stale.
///
/// [`invalidate_all()`]: LayoutCache::invalidate_all
#[derive(Debug)]
pub struct LayoutCache {
    entries: FxHashMap<LayoutCacheKey, CachedLayoutEntry>,
    generation: u64,
    max_entries: usize,
    hits: u64,
    misses: u64,
}

impl LayoutCache {
    /// Create a new cache with the specified maximum capacity.
    ///
    /// # Arguments
    ///
    /// * `max_entries` - Maximum number of entries before LRU eviction occurs.
    ///   A typical value is 64-256 for most UIs.
    ///
    /// # Example
    ///
    /// ```
    /// use ftui_layout::LayoutCache;
    /// let cache = LayoutCache::new(64);
    /// ```
    #[inline]
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: FxHashMap::with_capacity_and_hasher(max_entries, Default::default()),
            generation: 0,
            max_entries,
            hits: 0,
            misses: 0,
        }
    }

    /// Get cached layout or compute and cache a new one.
    ///
    /// If a valid (same generation) cache entry exists for the given key,
    /// returns a clone of it. Otherwise, calls the `compute` closure,
    /// caches the result, and returns it.
    ///
    /// # Arguments
    ///
    /// * `key` - The cache key identifying this layout computation
    /// * `compute` - Closure to compute the layout if not cached
    ///
    /// # Example
    ///
    /// ```ignore
    /// let key = LayoutCacheKey::new(area, &constraints, Direction::Horizontal, None);
    /// let rects = cache.get_or_compute(key, || flex.split(area));
    /// ```
    pub fn get_or_compute<F>(&mut self, key: LayoutCacheKey, compute: F) -> Vec<Rect>
    where
        F: FnOnce() -> Vec<Rect>,
    {
        // Check for existing valid entry
        if let Some(entry) = self.entries.get_mut(&key)
            && entry.generation == self.generation
        {
            self.hits += 1;
            entry.access_count = entry.access_count.saturating_add(1);
            return entry.chunks.clone();
        }

        // Cache miss - compute the value
        self.misses += 1;
        let chunks = compute();

        // Evict if at capacity
        if self.entries.len() >= self.max_entries {
            self.evict_lru();
        }

        // Insert new entry
        self.entries.insert(
            key,
            CachedLayoutEntry {
                chunks: chunks.clone(),
                generation: self.generation,
                access_count: 1,
            },
        );

        chunks
    }

    /// Invalidate all entries by bumping the generation.
    ///
    /// Existing entries become stale and will be recomputed on next access.
    /// This is an O(1) operation - entries are not immediately removed.
    ///
    /// # When to Call
    ///
    /// Call this after any state change that affects layout:
    /// - Model data changes that affect widget content
    /// - Theme/font changes that affect sizing
    ///
    /// # Note
    ///
    /// Resize events don't require invalidation because the area
    /// is part of the cache key.
    #[inline]
    pub fn invalidate_all(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Get current cache statistics.
    ///
    /// Returns hit/miss counts and the current hit rate.
    pub fn stats(&self) -> LayoutCacheStats {
        let total = self.hits + self.misses;
        LayoutCacheStats {
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
    ///
    /// [`invalidate_all()`]: LayoutCache::invalidate_all
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

    /// Evict the least recently used entry.
    fn evict_lru(&mut self) {
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

impl Default for LayoutCache {
    /// Creates a cache with default capacity of 64 entries.
    fn default() -> Self {
        Self::new(64)
    }
}

// ---------------------------------------------------------------------------
// S3-FIFO Layout Cache (bd-l6yba.3)
// ---------------------------------------------------------------------------

/// Layout cache backed by S3-FIFO eviction.
///
/// Drop-in replacement for [`LayoutCache`] that uses the scan-resistant
/// S3-FIFO eviction policy instead of the HashMap-based LRU. This protects
/// frequently-accessed layout computations from being evicted by transient
/// layouts (e.g. popup menus or tooltips that appear once).
///
/// Supports the same generation-based invalidation as [`LayoutCache`].
#[derive(Debug)]
pub struct S3FifoLayoutCache {
    cache: ftui_core::s3_fifo::S3Fifo<LayoutCacheKey, CachedLayoutEntry>,
    generation: u64,
    max_entries: usize,
    hits: u64,
    misses: u64,
}

impl S3FifoLayoutCache {
    /// Create a new S3-FIFO layout cache with the given capacity.
    #[inline]
    pub fn new(max_entries: usize) -> Self {
        Self {
            cache: ftui_core::s3_fifo::S3Fifo::new(max_entries.max(2)),
            generation: 0,
            max_entries: max_entries.max(2),
            hits: 0,
            misses: 0,
        }
    }

    /// Get cached layout or compute and cache a new one.
    ///
    /// Same semantics as [`LayoutCache::get_or_compute`]: entries from
    /// a previous generation are treated as misses.
    pub fn get_or_compute<F>(&mut self, key: LayoutCacheKey, compute: F) -> Vec<Rect>
    where
        F: FnOnce() -> Vec<Rect>,
    {
        if let Some(entry) = self.cache.get(&key) {
            if entry.generation == self.generation {
                self.hits += 1;
                return entry.chunks.clone();
            }
            // Stale entry (old generation) — remove and recompute.
            self.cache.remove(&key);
        }

        self.misses += 1;
        let chunks = compute();

        self.cache.insert(
            key,
            CachedLayoutEntry {
                chunks: chunks.clone(),
                generation: self.generation,
                access_count: 1,
            },
        );

        chunks
    }

    /// Invalidate all entries by bumping the generation (O(1)).
    #[inline]
    pub fn invalidate_all(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Get current cache statistics.
    pub fn stats(&self) -> LayoutCacheStats {
        let total = self.hits + self.misses;
        LayoutCacheStats {
            entries: self.cache.len(),
            hits: self.hits,
            misses: self.misses,
            hit_rate: if total > 0 {
                self.hits as f64 / total as f64
            } else {
                0.0
            },
        }
    }

    /// Reset statistics counters.
    #[inline]
    pub fn reset_stats(&mut self) {
        self.hits = 0;
        self.misses = 0;
    }

    /// Clear all entries.
    #[inline]
    pub fn clear(&mut self) {
        self.cache.clear();
        self.generation = self.generation.wrapping_add(1);
    }

    /// Current number of entries.
    #[inline]
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Whether the cache is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// Maximum capacity.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.max_entries
    }
}

impl Default for S3FifoLayoutCache {
    fn default() -> Self {
        Self::new(64)
    }
}

// ---------------------------------------------------------------------------
// Coherence Cache: Temporal Stability for Layout Rounding
// ---------------------------------------------------------------------------

/// Identity for a layout computation, used as key in the coherence cache.
///
/// This is a subset of [`LayoutCacheKey`] that identifies a *layout slot*
/// independently of the available area. Two computations with the same
/// `CoherenceId` but different area sizes represent the same logical layout
/// being re-rendered at a different terminal size.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct CoherenceId {
    /// Hash fingerprint of constraints.
    pub constraints_hash: u64,
    /// Layout direction.
    pub direction: Direction,
}

impl CoherenceId {
    /// Create a coherence ID from layout parameters.
    pub fn new(constraints: &[Constraint], direction: Direction) -> Self {
        Self {
            constraints_hash: LayoutCacheKey::hash_constraints(constraints),
            direction,
        }
    }

    /// Create a coherence ID from an existing cache key.
    pub fn from_cache_key(key: &LayoutCacheKey) -> Self {
        Self {
            constraints_hash: key.constraints_hash,
            direction: key.direction,
        }
    }
}

/// Stores previous layout allocations for temporal coherence.
///
/// When terminal size changes, the layout solver re-computes positions from
/// scratch. Without coherence, rounding tie-breaks can cause widgets to
/// "jump" between frames even when the total size change is small.
///
/// The `CoherenceCache` feeds previous allocations to
/// [`round_layout_stable`](crate::round_layout_stable) so that tie-breaking
/// favours keeping elements where they were.
///
/// # Usage
///
/// ```ignore
/// use ftui_layout::{CoherenceCache, CoherenceId, round_layout_stable, Constraint, Direction};
///
/// let mut coherence = CoherenceCache::new(64);
///
/// let id = CoherenceId::new(&constraints, Direction::Horizontal);
/// let prev = coherence.get(&id);
/// let alloc = round_layout_stable(&targets, total, prev);
/// coherence.store(id, alloc.clone());
/// ```
///
/// # Eviction
///
/// Entries are evicted on a least-recently-stored basis when the cache
/// reaches capacity. The cache does not grow unboundedly.
#[derive(Debug, Clone)]
pub struct CoherenceCache {
    entries: FxHashMap<CoherenceId, CoherenceEntry>,
    max_entries: usize,
    /// Monotonic counter for LRU eviction.
    tick: u64,
}

#[derive(Debug, Clone)]
struct CoherenceEntry {
    /// Previous allocation sizes.
    allocation: Vec<u16>,
    /// Tick when this entry was last stored.
    last_stored: u64,
}

impl CoherenceCache {
    /// Create a new coherence cache with the specified capacity.
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: FxHashMap::with_capacity_and_hasher(max_entries.min(256), Default::default()),
            max_entries,
            tick: 0,
        }
    }

    /// Retrieve the previous allocation for a layout, if available.
    ///
    /// Returns `Some(allocation)` suitable for passing directly to
    /// [`round_layout_stable`](crate::round_layout_stable).
    #[inline]
    pub fn get(&self, id: &CoherenceId) -> Option<Vec<u16>> {
        self.entries.get(id).map(|e| e.allocation.clone())
    }

    /// Store a layout allocation for future coherence lookups.
    ///
    /// If the cache is at capacity, evicts the oldest entry.
    pub fn store(&mut self, id: CoherenceId, allocation: Vec<u16>) {
        self.tick = self.tick.wrapping_add(1);

        if self.entries.len() >= self.max_entries && !self.entries.contains_key(&id) {
            self.evict_oldest();
        }

        self.entries.insert(
            id,
            CoherenceEntry {
                allocation,
                last_stored: self.tick,
            },
        );
    }

    /// Clear all stored allocations.
    #[inline]
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Number of entries in the cache.
    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if the cache is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Compute displacement between a new allocation and the previously stored one.
    ///
    /// Returns `(sum_displacement, max_displacement)` in cells.
    /// If no previous allocation exists for the ID, returns `(0, 0)`.
    pub fn displacement(&self, id: &CoherenceId, new_alloc: &[u16]) -> (u64, u32) {
        match self.entries.get(id) {
            Some(entry) => {
                let prev = &entry.allocation;
                let len = prev.len().min(new_alloc.len());
                let mut sum: u64 = 0;
                let mut max: u32 = 0;
                for i in 0..len {
                    let d = (new_alloc[i] as i32 - prev[i] as i32).unsigned_abs();
                    sum += u64::from(d);
                    max = max.max(d);
                }
                // If lengths differ, count the extra elements as full displacement
                for &v in &prev[len..] {
                    sum += u64::from(v);
                    max = max.max(u32::from(v));
                }
                for &v in &new_alloc[len..] {
                    sum += u64::from(v);
                    max = max.max(u32::from(v));
                }
                (sum, max)
            }
            None => (0, 0),
        }
    }

    fn evict_oldest(&mut self) {
        if let Some(key) = self
            .entries
            .iter()
            .min_by_key(|(_, e)| e.last_stored)
            .map(|(k, _)| *k)
        {
            self.entries.remove(&key);
        }
    }
}

impl Default for CoherenceCache {
    fn default() -> Self {
        Self::new(64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_key(width: u16, height: u16) -> LayoutCacheKey {
        LayoutCacheKey::new(
            Rect::new(0, 0, width, height),
            &[Constraint::Percentage(50.0), Constraint::Fill],
            Direction::Horizontal,
            None,
        )
    }

    fn should_not_call(label: &str) -> Vec<Rect> {
        unreachable!("{label}");
    }

    // --- LayoutCacheKey tests ---

    #[test]
    fn same_params_produce_same_key() {
        let k1 = make_key(80, 24);
        let k2 = make_key(80, 24);
        assert_eq!(k1, k2);
    }

    #[test]
    fn different_area_different_key() {
        let k1 = make_key(80, 24);
        let k2 = make_key(120, 40);
        assert_ne!(k1, k2);
    }

    #[test]
    fn different_constraints_different_key() {
        let k1 = LayoutCacheKey::new(
            Rect::new(0, 0, 80, 24),
            &[Constraint::Fixed(20)],
            Direction::Horizontal,
            None,
        );
        let k2 = LayoutCacheKey::new(
            Rect::new(0, 0, 80, 24),
            &[Constraint::Fixed(30)],
            Direction::Horizontal,
            None,
        );
        assert_ne!(k1, k2);
    }

    #[test]
    fn different_direction_different_key() {
        let k1 = LayoutCacheKey::new(
            Rect::new(0, 0, 80, 24),
            &[Constraint::Fill],
            Direction::Horizontal,
            None,
        );
        let k2 = LayoutCacheKey::new(
            Rect::new(0, 0, 80, 24),
            &[Constraint::Fill],
            Direction::Vertical,
            None,
        );
        assert_ne!(k1, k2);
    }

    #[test]
    fn different_intrinsics_different_key() {
        let hints1 = [LayoutSizeHint {
            min: 10,
            preferred: 20,
            max: None,
        }];
        let hints2 = [LayoutSizeHint {
            min: 10,
            preferred: 30,
            max: None,
        }];

        let k1 = LayoutCacheKey::new(
            Rect::new(0, 0, 80, 24),
            &[Constraint::FitContent],
            Direction::Horizontal,
            Some(&hints1),
        );
        let k2 = LayoutCacheKey::new(
            Rect::new(0, 0, 80, 24),
            &[Constraint::FitContent],
            Direction::Horizontal,
            Some(&hints2),
        );
        assert_ne!(k1, k2);
    }

    // --- LayoutCache tests ---

    #[test]
    fn cache_returns_same_result() {
        let mut cache = LayoutCache::new(100);
        let key = make_key(80, 24);

        let mut compute_count = 0;
        let compute = || {
            compute_count += 1;
            vec![Rect::new(0, 0, 40, 24), Rect::new(40, 0, 40, 24)]
        };

        let r1 = cache.get_or_compute(key, compute);
        let r2 = cache.get_or_compute(key, || should_not_call("should not call"));

        assert_eq!(r1, r2);
        assert_eq!(compute_count, 1);
    }

    #[test]
    fn different_area_is_cache_miss() {
        let mut cache = LayoutCache::new(100);

        let mut compute_count = 0;
        let mut compute = || {
            compute_count += 1;
            vec![Rect::default()]
        };

        let k1 = make_key(80, 24);
        let k2 = make_key(120, 40);

        cache.get_or_compute(k1, &mut compute);
        cache.get_or_compute(k2, &mut compute);

        assert_eq!(compute_count, 2);
    }

    #[test]
    fn invalidation_clears_cache() {
        let mut cache = LayoutCache::new(100);
        let key = make_key(80, 24);

        let mut compute_count = 0;
        let mut compute = || {
            compute_count += 1;
            vec![]
        };

        cache.get_or_compute(key, &mut compute);
        cache.invalidate_all();
        cache.get_or_compute(key, &mut compute);

        assert_eq!(compute_count, 2);
    }

    #[test]
    fn lru_eviction_works() {
        let mut cache = LayoutCache::new(2);

        let k1 = make_key(10, 10);
        let k2 = make_key(20, 20);
        let k3 = make_key(30, 30);

        // Insert two entries
        cache.get_or_compute(k1, || vec![Rect::new(0, 0, 10, 10)]);
        cache.get_or_compute(k2, || vec![Rect::new(0, 0, 20, 20)]);

        // Access k1 again (increases access count)
        cache.get_or_compute(k1, || should_not_call("k1 should hit"));

        // Insert k3, should evict k2 (least accessed)
        cache.get_or_compute(k3, || vec![Rect::new(0, 0, 30, 30)]);

        assert_eq!(cache.len(), 2);

        // k2 should be evicted
        let mut was_called = false;
        cache.get_or_compute(k2, || {
            was_called = true;
            vec![]
        });
        assert!(was_called, "k2 should have been evicted");

        // k1 should still be cached
        cache.get_or_compute(k1, || should_not_call("k1 should still be cached"));
    }

    #[test]
    fn stats_track_hits_and_misses() {
        let mut cache = LayoutCache::new(100);

        let k1 = make_key(80, 24);
        let k2 = make_key(120, 40);

        cache.get_or_compute(k1, Vec::new); // miss
        cache.get_or_compute(k1, || should_not_call("hit")); // hit
        cache.get_or_compute(k2, Vec::new); // miss

        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 2);
        assert!((stats.hit_rate - 1.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn reset_stats_clears_counters() {
        let mut cache = LayoutCache::new(100);
        let key = make_key(80, 24);

        cache.get_or_compute(key, Vec::new);
        cache.get_or_compute(key, || should_not_call("hit"));

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
        let mut cache = LayoutCache::new(100);

        cache.get_or_compute(make_key(80, 24), Vec::new);
        cache.get_or_compute(make_key(120, 40), Vec::new);

        assert_eq!(cache.len(), 2);

        cache.clear();

        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());

        // All entries should miss now
        let mut was_called = false;
        cache.get_or_compute(make_key(80, 24), || {
            was_called = true;
            vec![]
        });
        assert!(was_called);
    }

    #[test]
    fn default_capacity_is_64() {
        let cache = LayoutCache::default();
        assert_eq!(cache.capacity(), 64);
    }

    #[test]
    fn generation_wraps_around() {
        let mut cache = LayoutCache::new(100);
        cache.generation = u64::MAX;
        cache.invalidate_all();
        assert_eq!(cache.generation, 0);
    }

    // --- Constraint hashing tests ---

    #[test]
    fn constraint_hash_is_stable() {
        let constraints = [
            Constraint::Fixed(20),
            Constraint::Percentage(50.0),
            Constraint::Min(10),
        ];

        let h1 = LayoutCacheKey::hash_constraints(&constraints);
        let h2 = LayoutCacheKey::hash_constraints(&constraints);

        assert_eq!(h1, h2);
    }

    #[test]
    fn different_constraint_values_different_hash() {
        let c1 = [Constraint::Fixed(20)];
        let c2 = [Constraint::Fixed(30)];

        let h1 = LayoutCacheKey::hash_constraints(&c1);
        let h2 = LayoutCacheKey::hash_constraints(&c2);

        assert_ne!(h1, h2);
    }

    #[test]
    fn different_constraint_types_different_hash() {
        let c1 = [Constraint::Fixed(20)];
        let c2 = [Constraint::Min(20)];

        let h1 = LayoutCacheKey::hash_constraints(&c1);
        let h2 = LayoutCacheKey::hash_constraints(&c2);

        assert_ne!(h1, h2);
    }

    #[test]
    fn fit_content_bounded_values_in_hash() {
        let c1 = [Constraint::FitContentBounded { min: 10, max: 50 }];
        let c2 = [Constraint::FitContentBounded { min: 10, max: 60 }];

        let h1 = LayoutCacheKey::hash_constraints(&c1);
        let h2 = LayoutCacheKey::hash_constraints(&c2);

        assert_ne!(h1, h2);
    }

    // --- Intrinsics hashing tests ---

    #[test]
    fn intrinsics_hash_is_stable() {
        let hints = [
            LayoutSizeHint {
                min: 10,
                preferred: 20,
                max: Some(30),
            },
            LayoutSizeHint {
                min: 5,
                preferred: 15,
                max: None,
            },
        ];

        let h1 = LayoutCacheKey::hash_intrinsics(&hints);
        let h2 = LayoutCacheKey::hash_intrinsics(&hints);

        assert_eq!(h1, h2);
    }

    #[test]
    fn different_intrinsics_different_hash() {
        let h1 = [LayoutSizeHint {
            min: 10,
            preferred: 20,
            max: None,
        }];
        let h2 = [LayoutSizeHint {
            min: 10,
            preferred: 25,
            max: None,
        }];

        let hash1 = LayoutCacheKey::hash_intrinsics(&h1);
        let hash2 = LayoutCacheKey::hash_intrinsics(&h2);

        assert_ne!(hash1, hash2);
    }

    // --- Property-like tests ---

    #[test]
    fn cache_is_deterministic() {
        let mut cache1 = LayoutCache::new(100);
        let mut cache2 = LayoutCache::new(100);

        for i in 0..10u16 {
            let key = make_key(i * 10, i * 5);
            let result = vec![Rect::new(0, 0, i, i)];

            cache1.get_or_compute(key, || result.clone());
            cache2.get_or_compute(key, || result);
        }

        assert_eq!(cache1.stats().entries, cache2.stats().entries);
        assert_eq!(cache1.stats().misses, cache2.stats().misses);
    }

    #[test]
    fn hit_count_increments_on_each_access() {
        let mut cache = LayoutCache::new(100);
        let key = make_key(80, 24);

        // First access is a miss
        cache.get_or_compute(key, Vec::new);

        // Subsequent accesses are hits
        for _ in 0..5 {
            cache.get_or_compute(key, || should_not_call("should hit"));
        }

        let stats = cache.stats();
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hits, 5);
    }

    // -----------------------------------------------------------------------
    // Coherence Cache Tests (bd-4kq0.4.2)
    // -----------------------------------------------------------------------

    fn make_coherence_id(n: u16) -> CoherenceId {
        CoherenceId::new(
            &[Constraint::Fixed(n), Constraint::Fill],
            Direction::Horizontal,
        )
    }

    #[test]
    fn coherence_store_and_get() {
        let mut cc = CoherenceCache::new(64);
        let id = make_coherence_id(1);

        assert!(cc.get(&id).is_none());

        cc.store(id, vec![30, 50]);
        assert_eq!(cc.get(&id), Some(vec![30, 50]));
    }

    #[test]
    fn coherence_update_replaces_allocation() {
        let mut cc = CoherenceCache::new(64);
        let id = make_coherence_id(1);

        cc.store(id, vec![30, 50]);
        cc.store(id, vec![31, 49]);

        assert_eq!(cc.get(&id), Some(vec![31, 49]));
        assert_eq!(cc.len(), 1);
    }

    #[test]
    fn coherence_different_ids_are_separate() {
        let mut cc = CoherenceCache::new(64);
        let id1 = make_coherence_id(1);
        let id2 = make_coherence_id(2);

        cc.store(id1, vec![40, 40]);
        cc.store(id2, vec![30, 50]);

        assert_eq!(cc.get(&id1), Some(vec![40, 40]));
        assert_eq!(cc.get(&id2), Some(vec![30, 50]));
    }

    #[test]
    fn coherence_eviction_at_capacity() {
        let mut cc = CoherenceCache::new(2);

        let id1 = make_coherence_id(1);
        let id2 = make_coherence_id(2);
        let id3 = make_coherence_id(3);

        cc.store(id1, vec![10]);
        cc.store(id2, vec![20]);
        cc.store(id3, vec![30]);

        assert_eq!(cc.len(), 2);
        // id1 should be evicted (oldest)
        assert!(cc.get(&id1).is_none());
        assert_eq!(cc.get(&id2), Some(vec![20]));
        assert_eq!(cc.get(&id3), Some(vec![30]));
    }

    #[test]
    fn coherence_clear() {
        let mut cc = CoherenceCache::new(64);
        let id = make_coherence_id(1);

        cc.store(id, vec![10, 20]);
        assert_eq!(cc.len(), 1);

        cc.clear();
        assert!(cc.is_empty());
        assert!(cc.get(&id).is_none());
    }

    #[test]
    fn coherence_displacement_with_previous() {
        let mut cc = CoherenceCache::new(64);
        let id = make_coherence_id(1);

        cc.store(id, vec![30, 50]);

        // New allocation differs by 2 cells in each slot
        let (sum, max) = cc.displacement(&id, &[32, 48]);
        assert_eq!(sum, 4); // |32-30| + |48-50| = 2 + 2
        assert_eq!(max, 2);
    }

    #[test]
    fn coherence_displacement_without_previous() {
        let cc = CoherenceCache::new(64);
        let id = make_coherence_id(1);

        let (sum, max) = cc.displacement(&id, &[30, 50]);
        assert_eq!(sum, 0);
        assert_eq!(max, 0);
    }

    #[test]
    fn coherence_displacement_different_lengths() {
        let mut cc = CoherenceCache::new(64);
        let id = make_coherence_id(1);

        cc.store(id, vec![30, 50]);

        // New allocation has 3 elements (extra element counted as displacement)
        let (sum, max) = cc.displacement(&id, &[30, 50, 10]);
        assert_eq!(sum, 10);
        assert_eq!(max, 10);
    }

    #[test]
    fn coherence_from_cache_key() {
        let key = make_key(80, 24);
        let id = CoherenceId::from_cache_key(&key);

        // Same constraints + direction should produce same ID regardless of area
        let key2 = make_key(120, 40);
        let id2 = CoherenceId::from_cache_key(&key2);

        assert_eq!(id, id2);
    }

    // --- unit_cache_reuse ---

    #[test]
    fn unit_cache_reuse_unchanged_constraints_yield_identical_layout() {
        use crate::round_layout_stable;

        let mut cc = CoherenceCache::new(64);
        let id = make_coherence_id(1);

        // First layout at width 80
        let targets = [26.67, 26.67, 26.66];
        let total = 80;
        let alloc1 = round_layout_stable(&targets, total, cc.get(&id));
        cc.store(id, alloc1.clone());

        // Same constraints, same width → should produce identical result
        let alloc2 = round_layout_stable(&targets, total, cc.get(&id));
        assert_eq!(alloc1, alloc2, "Same inputs should yield identical layout");
    }

    // --- e2e_resize_sweep ---

    #[test]
    fn e2e_resize_sweep_bounded_displacement() {
        use crate::round_layout_stable;

        let mut cc = CoherenceCache::new(64);
        let id = make_coherence_id(1);

        // Equal three-way split: targets are total/3 each
        // Sweep terminal width from 60 to 120 in steps of 1
        let mut max_displacement_ever: u32 = 0;
        let mut total_displacement_sum: u64 = 0;
        let steps = 61; // 60..=120

        for width in 60u16..=120 {
            let third = f64::from(width) / 3.0;
            let targets = [third, third, third];

            let prev = cc.get(&id);
            let alloc = round_layout_stable(&targets, width, prev);

            let (d_sum, d_max) = cc.displacement(&id, &alloc);
            total_displacement_sum += d_sum;
            max_displacement_ever = max_displacement_ever.max(d_max);

            cc.store(id, alloc);
        }

        // Each 1-cell width change should cause at most 1 cell of displacement
        // in each slot (ideal: only 1 slot changes by 1).
        assert!(
            max_displacement_ever <= 2,
            "Max single-step displacement should be <=2 cells, got {}",
            max_displacement_ever
        );

        // Average displacement per step should be ~1 (one extra cell redistributed)
        let avg = total_displacement_sum as f64 / steps as f64;
        assert!(
            avg < 3.0,
            "Average displacement per step should be <3 cells, got {:.2}",
            avg
        );
    }

    #[test]
    fn e2e_resize_sweep_deterministic() {
        use crate::round_layout_stable;

        // Two identical sweeps should produce identical displacement logs
        let sweep = |seed: u16| -> Vec<(u16, Vec<u16>, u64, u32)> {
            let mut cc = CoherenceCache::new(64);
            let id = CoherenceId::new(
                &[Constraint::Percentage(30.0), Constraint::Fill],
                Direction::Horizontal,
            );

            let mut log = Vec::new();
            for width in (40 + seed)..(100 + seed) {
                let targets = [f64::from(width) * 0.3, f64::from(width) * 0.7];
                let prev = cc.get(&id);
                let alloc = round_layout_stable(&targets, width, prev);
                let (d_sum, d_max) = cc.displacement(&id, &alloc);
                cc.store(id, alloc.clone());
                log.push((width, alloc, d_sum, d_max));
            }
            log
        };

        let log1 = sweep(0);
        let log2 = sweep(0);
        assert_eq!(log1, log2, "Identical sweeps should produce identical logs");
    }

    #[test]
    fn default_coherence_cache_capacity_is_64() {
        let cc = CoherenceCache::default();
        assert_eq!(cc.max_entries, 64);
    }

    // ── S3-FIFO Layout Cache ────────────────────────────────────────

    fn s3_fifo_test_key(x: u16, w: u16) -> LayoutCacheKey {
        LayoutCacheKey {
            area_x: x,
            area_y: 0,
            area_width: w,
            area_height: 24,
            constraints_hash: 42,
            direction: Direction::Horizontal,
            intrinsics_hash: None,
        }
    }

    #[test]
    fn s3fifo_layout_new_is_empty() {
        let cache = S3FifoLayoutCache::new(64);
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.capacity(), 64);
    }

    #[test]
    fn s3fifo_layout_default_capacity() {
        let cache = S3FifoLayoutCache::default();
        assert_eq!(cache.capacity(), 64);
    }

    #[test]
    fn s3fifo_layout_get_or_compute_caches() {
        let mut cache = S3FifoLayoutCache::new(64);
        let key = s3_fifo_test_key(0, 80);
        let rects1 = cache.get_or_compute(key, || vec![Rect::new(0, 0, 40, 24)]);
        let rects2 = cache.get_or_compute(key, || panic!("should not recompute"));
        assert_eq!(rects1, rects2);
        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
    }

    #[test]
    fn s3fifo_layout_generation_invalidation() {
        let mut cache = S3FifoLayoutCache::new(64);
        let key = s3_fifo_test_key(0, 80);
        cache.get_or_compute(key, || vec![Rect::new(0, 0, 40, 24)]);

        cache.invalidate_all();

        // After invalidation, should recompute
        let rects = cache.get_or_compute(key, || vec![Rect::new(0, 0, 80, 24)]);
        assert_eq!(rects, vec![Rect::new(0, 0, 80, 24)]);
        let stats = cache.stats();
        assert_eq!(stats.misses, 2);
    }

    #[test]
    fn s3fifo_layout_clear() {
        let mut cache = S3FifoLayoutCache::new(64);
        let key = s3_fifo_test_key(0, 80);
        cache.get_or_compute(key, || vec![Rect::new(0, 0, 40, 24)]);
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn s3fifo_layout_different_keys() {
        let mut cache = S3FifoLayoutCache::new(64);
        let k1 = s3_fifo_test_key(0, 80);
        let k2 = s3_fifo_test_key(0, 120);
        cache.get_or_compute(k1, || vec![Rect::new(0, 0, 40, 24)]);
        cache.get_or_compute(k2, || vec![Rect::new(0, 0, 60, 24)]);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn s3fifo_layout_reset_stats() {
        let mut cache = S3FifoLayoutCache::new(64);
        let key = s3_fifo_test_key(0, 80);
        cache.get_or_compute(key, Vec::new);
        cache.get_or_compute(key, Vec::new);
        cache.reset_stats();
        let stats = cache.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
    }

    #[test]
    fn s3fifo_layout_produces_same_results_as_lru() {
        let mut lru = LayoutCache::new(64);
        let mut s3 = S3FifoLayoutCache::new(64);

        for w in [80, 100, 120, 160, 200] {
            let key = s3_fifo_test_key(0, w);
            let expected = vec![Rect::new(0, 0, w / 2, 24)];
            let lru_r = lru.get_or_compute(key, || expected.clone());
            let s3_r = s3.get_or_compute(key, || expected.clone());
            assert_eq!(lru_r, s3_r, "mismatch for width={w}");
        }
    }
}
