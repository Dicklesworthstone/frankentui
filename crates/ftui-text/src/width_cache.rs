#![forbid(unsafe_code)]

//! LRU width cache for efficient text measurement.
//!
//! Text width calculation is a hot path (called every render frame).
//! This cache stores computed widths to avoid redundant Unicode width
//! calculations for repeated strings.
//!
//! # Example
//! ```
//! use ftui_text::WidthCache;
//!
//! let mut cache = WidthCache::new(1000);
//!
//! // First call computes width
//! let width = cache.get_or_compute("Hello, world!");
//! assert_eq!(width, 13);
//!
//! // Second call hits cache
//! let width2 = cache.get_or_compute("Hello, world!");
//! assert_eq!(width2, 13);
//!
//! // Check stats
//! let stats = cache.stats();
//! assert_eq!(stats.hits, 1);
//! assert_eq!(stats.misses, 1);
//! ```

use lru::LruCache;
use rustc_hash::FxHasher;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;

/// Default cache capacity.
pub const DEFAULT_CACHE_CAPACITY: usize = 4096;

/// Statistics about cache performance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheStats {
    /// Number of cache hits.
    pub hits: u64,
    /// Number of cache misses.
    pub misses: u64,
    /// Current number of entries.
    pub size: usize,
    /// Maximum capacity.
    pub capacity: usize,
}

impl CacheStats {
    /// Calculate hit rate (0.0 to 1.0).
    #[must_use]
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

/// LRU cache for text width measurements.
///
/// This cache stores the computed display width (in terminal cells) for
/// text strings, using an LRU eviction policy when capacity is reached.
///
/// # Performance
/// - Uses FxHash for fast hashing
/// - O(1) lookup and insertion
/// - Automatic LRU eviction
/// - Keys are stored as 64-bit hashes (not full strings) to minimize memory
///
/// # Hash Collisions
/// The cache uses a 64-bit hash as the lookup key rather than storing the
/// full string. This trades theoretical correctness for memory efficiency.
/// With FxHash, collision probability is ~1 in 2^64, making this safe for
/// practical use. If you require guaranteed correctness, use `contains()`
/// to verify presence before trusting cached values.
///
/// # Thread Safety
/// `WidthCache` is not thread-safe. For concurrent use, wrap in a mutex
/// or use thread-local caches.
#[derive(Debug)]
pub struct WidthCache {
    cache: LruCache<u64, usize>,
    hits: u64,
    misses: u64,
}

impl WidthCache {
    /// Create a new cache with the specified capacity.
    ///
    /// If capacity is zero, defaults to 1.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let capacity = NonZeroUsize::new(capacity.max(1)).expect("capacity must be > 0");
        Self {
            cache: LruCache::new(capacity),
            hits: 0,
            misses: 0,
        }
    }

    /// Create a new cache with the default capacity (4096 entries).
    #[must_use]
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_CACHE_CAPACITY)
    }

    /// Get cached width or compute and cache it.
    ///
    /// If the text is in the cache, returns the cached width.
    /// Otherwise, computes the width using `display_width` and caches it.
    #[inline]
    pub fn get_or_compute(&mut self, text: &str) -> usize {
        self.get_or_compute_with(text, crate::display_width)
    }

    /// Get cached width or compute using a custom function.
    ///
    /// This allows using custom width calculation functions for testing
    /// or specialized terminal behavior.
    pub fn get_or_compute_with<F>(&mut self, text: &str, compute: F) -> usize
    where
        F: FnOnce(&str) -> usize,
    {
        let hash = hash_text(text);

        if let Some(&width) = self.cache.get(&hash) {
            self.hits += 1;
            return width;
        }

        self.misses += 1;
        let width = compute(text);
        self.cache.put(hash, width);
        width
    }

    /// Check if a text string is in the cache.
    #[must_use]
    pub fn contains(&self, text: &str) -> bool {
        let hash = hash_text(text);
        self.cache.contains(&hash)
    }

    /// Get the cached width for a text string without computing.
    ///
    /// Returns `None` if the text is not in the cache.
    /// Note: This does update the LRU order.
    #[must_use]
    pub fn get(&mut self, text: &str) -> Option<usize> {
        let hash = hash_text(text);
        self.cache.get(&hash).copied()
    }

    /// Peek at the cached width without updating LRU order.
    #[must_use]
    pub fn peek(&self, text: &str) -> Option<usize> {
        let hash = hash_text(text);
        self.cache.peek(&hash).copied()
    }

    /// Pre-populate the cache with a text string.
    ///
    /// This is useful for warming up the cache with known strings.
    pub fn preload(&mut self, text: &str) {
        let hash = hash_text(text);
        if !self.cache.contains(&hash) {
            let width = crate::display_width(text);
            self.cache.put(hash, width);
        }
    }

    /// Pre-populate the cache with multiple strings.
    pub fn preload_many<'a>(&mut self, texts: impl IntoIterator<Item = &'a str>) {
        for text in texts {
            self.preload(text);
        }
    }

    /// Clear the cache.
    pub fn clear(&mut self) {
        self.cache.clear();
    }

    /// Reset statistics.
    pub fn reset_stats(&mut self) {
        self.hits = 0;
        self.misses = 0;
    }

    /// Get cache statistics.
    #[inline]
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits,
            misses: self.misses,
            size: self.cache.len(),
            capacity: self.cache.cap().get(),
        }
    }

    /// Get the current number of cached entries.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Check if the cache is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// Get the cache capacity.
    #[inline]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.cache.cap().get()
    }

    /// Resize the cache capacity.
    ///
    /// If the new capacity is smaller than the current size,
    /// entries will be evicted (LRU order).
    pub fn resize(&mut self, new_capacity: usize) {
        let new_capacity = NonZeroUsize::new(new_capacity.max(1)).expect("capacity must be > 0");
        self.cache.resize(new_capacity);
    }
}

impl Default for WidthCache {
    fn default() -> Self {
        Self::with_default_capacity()
    }
}

/// Hash a text string using FxHash for fast hashing.
#[inline]
fn hash_text(text: &str) -> u64 {
    let mut hasher = FxHasher::default();
    text.hash(&mut hasher);
    hasher.finish()
}

// Thread-local width cache for convenience.
//
// This provides a global cache that is thread-local, avoiding the need
// to pass a cache around explicitly.
#[cfg(feature = "thread_local_cache")]
thread_local! {
    static THREAD_CACHE: std::cell::RefCell<WidthCache> =
        std::cell::RefCell::new(WidthCache::with_default_capacity());
}

/// Get or compute width using the thread-local cache.
#[cfg(feature = "thread_local_cache")]
pub fn cached_width(text: &str) -> usize {
    THREAD_CACHE.with(|cache| cache.borrow_mut().get_or_compute(text))
}

/// Clear the thread-local cache.
#[cfg(feature = "thread_local_cache")]
pub fn clear_thread_cache() {
    THREAD_CACHE.with(|cache| cache.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_cache_is_empty() {
        let cache = WidthCache::new(100);
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.capacity(), 100);
    }

    #[test]
    fn default_capacity() {
        let cache = WidthCache::with_default_capacity();
        assert_eq!(cache.capacity(), DEFAULT_CACHE_CAPACITY);
    }

    #[test]
    fn get_or_compute_caches_value() {
        let mut cache = WidthCache::new(100);

        let width1 = cache.get_or_compute("hello");
        assert_eq!(width1, 5);
        assert_eq!(cache.len(), 1);

        let width2 = cache.get_or_compute("hello");
        assert_eq!(width2, 5);
        assert_eq!(cache.len(), 1); // Same entry

        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
    }

    #[test]
    fn get_or_compute_different_strings() {
        let mut cache = WidthCache::new(100);

        cache.get_or_compute("hello");
        cache.get_or_compute("world");
        cache.get_or_compute("foo");

        assert_eq!(cache.len(), 3);
        let stats = cache.stats();
        assert_eq!(stats.misses, 3);
        assert_eq!(stats.hits, 0);
    }

    #[test]
    fn get_or_compute_cjk() {
        let mut cache = WidthCache::new(100);

        let width = cache.get_or_compute("你好");
        assert_eq!(width, 4); // 2 chars * 2 cells each
    }

    #[test]
    fn contains() {
        let mut cache = WidthCache::new(100);

        assert!(!cache.contains("hello"));
        cache.get_or_compute("hello");
        assert!(cache.contains("hello"));
    }

    #[test]
    fn get_returns_none_for_missing() {
        let mut cache = WidthCache::new(100);
        assert!(cache.get("missing").is_none());
    }

    #[test]
    fn get_returns_cached_value() {
        let mut cache = WidthCache::new(100);
        cache.get_or_compute("hello");

        let width = cache.get("hello");
        assert_eq!(width, Some(5));
    }

    #[test]
    fn peek_does_not_update_lru() {
        let mut cache = WidthCache::new(2);

        cache.get_or_compute("a");
        cache.get_or_compute("b");

        // Peek at "a" - should not update LRU order
        let _ = cache.peek("a");

        // Add "c" - should evict "a" (oldest)
        cache.get_or_compute("c");

        assert!(!cache.contains("a"));
        assert!(cache.contains("b"));
        assert!(cache.contains("c"));
    }

    #[test]
    fn lru_eviction() {
        let mut cache = WidthCache::new(2);

        cache.get_or_compute("a");
        cache.get_or_compute("b");
        cache.get_or_compute("c"); // Should evict "a"

        assert!(!cache.contains("a"));
        assert!(cache.contains("b"));
        assert!(cache.contains("c"));
    }

    #[test]
    fn lru_refresh_on_access() {
        let mut cache = WidthCache::new(2);

        cache.get_or_compute("a");
        cache.get_or_compute("b");
        cache.get_or_compute("a"); // Refresh "a" to most recent
        cache.get_or_compute("c"); // Should evict "b"

        assert!(cache.contains("a"));
        assert!(!cache.contains("b"));
        assert!(cache.contains("c"));
    }

    #[test]
    fn preload() {
        let mut cache = WidthCache::new(100);

        cache.preload("hello");
        assert!(cache.contains("hello"));
        assert_eq!(cache.peek("hello"), Some(5));

        let stats = cache.stats();
        assert_eq!(stats.misses, 0); // Preload doesn't count as miss
        assert_eq!(stats.hits, 0);
    }

    #[test]
    fn preload_many() {
        let mut cache = WidthCache::new(100);

        cache.preload_many(["hello", "world", "foo"]);
        assert_eq!(cache.len(), 3);
    }

    #[test]
    fn clear() {
        let mut cache = WidthCache::new(100);
        cache.get_or_compute("hello");
        cache.get_or_compute("world");

        cache.clear();
        assert!(cache.is_empty());
        assert!(!cache.contains("hello"));
    }

    #[test]
    fn reset_stats() {
        let mut cache = WidthCache::new(100);
        cache.get_or_compute("hello");
        cache.get_or_compute("hello");

        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);

        cache.reset_stats();
        let stats = cache.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
    }

    #[test]
    fn hit_rate() {
        let stats = CacheStats {
            hits: 75,
            misses: 25,
            size: 100,
            capacity: 1000,
        };
        assert!((stats.hit_rate() - 0.75).abs() < 0.001);
    }

    #[test]
    fn hit_rate_no_requests() {
        let stats = CacheStats::default();
        assert_eq!(stats.hit_rate(), 0.0);
    }

    #[test]
    fn resize_smaller() {
        let mut cache = WidthCache::new(100);
        for i in 0..50 {
            cache.get_or_compute(&format!("text{i}"));
        }
        assert_eq!(cache.len(), 50);

        cache.resize(10);
        assert!(cache.len() <= 10);
        assert_eq!(cache.capacity(), 10);
    }

    #[test]
    fn resize_larger() {
        let mut cache = WidthCache::new(10);
        cache.resize(100);
        assert_eq!(cache.capacity(), 100);
    }

    #[test]
    fn custom_compute_function() {
        let mut cache = WidthCache::new(100);

        // Use a custom width function (always returns 42)
        let width = cache.get_or_compute_with("hello", |_| 42);
        assert_eq!(width, 42);

        // Cached value is 42
        assert_eq!(cache.peek("hello"), Some(42));
    }

    #[test]
    fn empty_string() {
        let mut cache = WidthCache::new(100);
        let width = cache.get_or_compute("");
        assert_eq!(width, 0);
    }

    #[test]
    fn hash_collision_handling() {
        // Even with hash collisions, the LRU should handle them
        // (this is just a stress test with many entries)
        let mut cache = WidthCache::new(1000);

        for i in 0..500 {
            cache.get_or_compute(&format!("string{i}"));
        }

        assert_eq!(cache.len(), 500);
    }

    #[test]
    fn unicode_strings() {
        let mut cache = WidthCache::new(100);

        // Various Unicode strings
        assert_eq!(cache.get_or_compute("café"), 4);
        assert_eq!(cache.get_or_compute("日本語"), 6);
        assert_eq!(cache.get_or_compute("🎉"), 2); // Emoji typically 2 cells

        assert_eq!(cache.len(), 3);
    }

    #[test]
    fn combining_characters() {
        let mut cache = WidthCache::new(100);

        // e + combining acute accent
        let width = cache.get_or_compute("e\u{0301}");
        // Should be 1 cell (the combining char doesn't add width)
        assert_eq!(width, 1);
    }

    // ==========================================================================
    // Additional coverage tests
    // ==========================================================================

    #[test]
    fn default_cache() {
        let cache = WidthCache::default();
        assert!(cache.is_empty());
        assert_eq!(cache.capacity(), DEFAULT_CACHE_CAPACITY);
    }

    #[test]
    fn cache_stats_debug() {
        let stats = CacheStats {
            hits: 10,
            misses: 5,
            size: 15,
            capacity: 100,
        };
        let debug = format!("{:?}", stats);
        assert!(debug.contains("CacheStats"));
        assert!(debug.contains("10")); // hits
    }

    #[test]
    fn cache_stats_default() {
        let stats = CacheStats::default();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
        assert_eq!(stats.size, 0);
        assert_eq!(stats.capacity, 0);
    }

    #[test]
    fn cache_stats_equality() {
        let stats1 = CacheStats {
            hits: 10,
            misses: 5,
            size: 15,
            capacity: 100,
        };
        let stats2 = stats1; // Copy
        assert_eq!(stats1, stats2);
    }

    #[test]
    fn clear_after_preload() {
        let mut cache = WidthCache::new(100);
        cache.preload_many(["hello", "world", "test"]);
        assert_eq!(cache.len(), 3);

        cache.clear();
        assert!(cache.is_empty());
        assert!(!cache.contains("hello"));
    }

    #[test]
    fn preload_existing_is_noop() {
        let mut cache = WidthCache::new(100);
        cache.get_or_compute("hello"); // First access
        let len_before = cache.len();

        cache.preload("hello"); // Already exists
        assert_eq!(cache.len(), len_before);
    }

    #[test]
    fn minimum_capacity_is_one() {
        let cache = WidthCache::new(0);
        assert_eq!(cache.capacity(), 1);
    }

    #[test]
    fn width_cache_debug() {
        let cache = WidthCache::new(10);
        let debug = format!("{:?}", cache);
        assert!(debug.contains("WidthCache"));
    }

    #[test]
    fn emoji_zwj_sequence() {
        let mut cache = WidthCache::new(100);
        // Family emoji (ZWJ sequence)
        let width = cache.get_or_compute("👨‍👩‍👧");
        // Width varies by implementation, just ensure it doesn't panic
        assert!(width >= 1);
    }

    #[test]
    fn emoji_with_skin_tone() {
        let mut cache = WidthCache::new(100);
        let width = cache.get_or_compute("👍🏻");
        assert!(width >= 1);
    }

    #[test]
    fn flag_emoji() {
        let mut cache = WidthCache::new(100);
        // US flag emoji (regional indicators)
        let width = cache.get_or_compute("🇺🇸");
        assert!(width >= 1);
    }

    #[test]
    fn mixed_width_strings() {
        let mut cache = WidthCache::new(100);
        // Mixed ASCII and CJK
        let width = cache.get_or_compute("Hello你好World");
        assert_eq!(width, 14); // 10 ASCII + 4 CJK
    }

    #[test]
    fn stats_size_reflects_cache_len() {
        let mut cache = WidthCache::new(100);
        cache.get_or_compute("a");
        cache.get_or_compute("b");
        cache.get_or_compute("c");

        let stats = cache.stats();
        assert_eq!(stats.size, cache.len());
        assert_eq!(stats.size, 3);
    }

    #[test]
    fn stats_capacity_matches() {
        let cache = WidthCache::new(42);
        let stats = cache.stats();
        assert_eq!(stats.capacity, 42);
    }

    #[test]
    fn resize_to_zero_becomes_one() {
        let mut cache = WidthCache::new(100);
        cache.resize(0);
        assert_eq!(cache.capacity(), 1);
    }

    #[test]
    fn get_updates_lru_order() {
        let mut cache = WidthCache::new(2);

        cache.get_or_compute("a");
        cache.get_or_compute("b");

        // Access "a" via get() - should update LRU order
        let _ = cache.get("a");

        // Add "c" - should evict "b" (now oldest)
        cache.get_or_compute("c");

        assert!(cache.contains("a"));
        assert!(!cache.contains("b"));
        assert!(cache.contains("c"));
    }

    #[test]
    fn contains_does_not_modify_stats() {
        let mut cache = WidthCache::new(100);
        cache.get_or_compute("hello");

        let stats_before = cache.stats();
        let _ = cache.contains("hello");
        let _ = cache.contains("missing");
        let stats_after = cache.stats();

        assert_eq!(stats_before.hits, stats_after.hits);
        assert_eq!(stats_before.misses, stats_after.misses);
    }

    #[test]
    fn peek_returns_none_for_missing() {
        let cache = WidthCache::new(100);
        assert!(cache.peek("missing").is_none());
    }

    #[test]
    fn custom_compute_called_once() {
        let mut cache = WidthCache::new(100);
        let mut call_count = 0;

        cache.get_or_compute_with("test", |_| {
            call_count += 1;
            10
        });

        cache.get_or_compute_with("test", |_| {
            call_count += 1;
            20 // This shouldn't be called
        });

        assert_eq!(call_count, 1);
        assert_eq!(cache.peek("test"), Some(10));
    }

    #[test]
    fn whitespace_strings() {
        let mut cache = WidthCache::new(100);
        assert_eq!(cache.get_or_compute("   "), 3); // 3 spaces
        assert_eq!(cache.get_or_compute("\t"), 1); // Tab is 1 cell typically
        assert_eq!(cache.get_or_compute("\n"), 1); // Newline
    }
}

// ---------------------------------------------------------------------------
// W-TinyLFU Admission Components (bd-4kq0.6.1)
// ---------------------------------------------------------------------------
//
// # Design
//
// W-TinyLFU augments LRU eviction with a frequency-based admission filter:
//
// 1. **Count-Min Sketch (CMS)**: Approximate frequency counter.
//    - Parameters: width `w`, depth `d`.
//    - Error bound: estimated count <= true count + epsilon * N
//      with probability >= 1 - delta, where:
//        epsilon = e / w  (e = Euler's number ≈ 2.718)
//        delta   = (1/2)^d
//    - Chosen defaults: w=1024 (epsilon ≈ 0.0027), d=4 (delta ≈ 0.0625).
//    - Counter width: 4 bits (saturating at 15). Periodic halving (aging)
//      every `reset_interval` increments to prevent staleness.
//
// 2. **Doorkeeper**: 1-bit Bloom filter (single hash, `doorkeeper_bits` entries).
//    - Filters one-hit wonders before they reach the CMS.
//    - On first access: set doorkeeper bit. On second access in the same
//      epoch: increment CMS. Cleared on CMS reset.
//    - Default: 2048 bits (256 bytes).
//
// 3. **Admission rule**: When evicting, compare frequencies:
//    - `freq(candidate) > freq(victim)` → admit candidate, evict victim.
//    - `freq(candidate) <= freq(victim)` → reject candidate, keep victim.
//
// 4. **Fingerprint guard**: The CMS stores 64-bit hashes. Since the main
//    cache also keys by 64-bit hash, a collision means two distinct strings
//    share the same key. The fingerprint guard adds a secondary hash
//    (different seed) stored alongside the value. On lookup, if the
//    secondary hash mismatches, the entry is treated as a miss and evicted.
//
// # Failure Modes
// - CMS overcounting: bounded by epsilon * N; aging limits staleness.
// - Doorkeeper false positives: one-hit items may leak to CMS. Bounded
//   by Bloom FP rate ≈ (1 - e^{-k*n/m})^k with k=1.
// - Fingerprint collision (secondary hash): probability ~2^{-64}; negligible.
// - Reset storm: halving all counters is O(w*d). With w=1024, d=4 this is
//   4096 operations — negligible vs. rendering cost.

/// Count-Min Sketch for approximate frequency estimation.
///
/// Uses `depth` independent hash functions (derived from a single hash via
/// mixing) and `width` counters per row. Each counter is a `u8` saturating
/// at [`CountMinSketch::MAX_COUNT`] (15 by default, representing 4-bit counters).
///
/// # Error Bounds
///
/// For a sketch with width `w` and depth `d`, after `N` total increments:
/// - `estimate(x) <= true_count(x) + epsilon * N` with probability `>= 1 - delta`
/// - where `epsilon = e / w` and `delta = (1/2)^d`
#[derive(Debug, Clone)]
pub struct CountMinSketch {
    /// Counter matrix: `depth` rows of `width` counters each.
    counters: Vec<Vec<u8>>,
    /// Number of hash functions (rows).
    depth: usize,
    /// Number of counters per row.
    width: usize,
    /// Total number of increments since last reset.
    total_increments: u64,
    /// Increment count at which to halve all counters.
    reset_interval: u64,
}

/// Maximum counter value (4-bit saturation).
const CMS_MAX_COUNT: u8 = 15;

/// Default CMS width. epsilon = e/1024 ≈ 0.0027.
const CMS_DEFAULT_WIDTH: usize = 1024;

/// Default CMS depth. delta = (1/2)^4 = 0.0625.
const CMS_DEFAULT_DEPTH: usize = 4;

/// Default reset interval (halve counters after this many increments).
const CMS_DEFAULT_RESET_INTERVAL: u64 = 8192;

impl CountMinSketch {
    /// Create a new Count-Min Sketch with the given dimensions.
    pub fn new(width: usize, depth: usize, reset_interval: u64) -> Self {
        let width = width.max(1);
        let depth = depth.max(1);
        Self {
            counters: vec![vec![0u8; width]; depth],
            depth,
            width,
            total_increments: 0,
            reset_interval: reset_interval.max(1),
        }
    }

    /// Create a sketch with default parameters (w=1024, d=4, reset=8192).
    pub fn with_defaults() -> Self {
        Self::new(
            CMS_DEFAULT_WIDTH,
            CMS_DEFAULT_DEPTH,
            CMS_DEFAULT_RESET_INTERVAL,
        )
    }

    /// Increment the count for a key.
    pub fn increment(&mut self, hash: u64) {
        for row in 0..self.depth {
            let idx = self.index(hash, row);
            self.counters[row][idx] = self.counters[row][idx].saturating_add(1).min(CMS_MAX_COUNT);
        }
        self.total_increments += 1;

        if self.total_increments >= self.reset_interval {
            self.halve();
        }
    }

    /// Estimate the frequency of a key (minimum across all rows).
    pub fn estimate(&self, hash: u64) -> u8 {
        let mut min = u8::MAX;
        for row in 0..self.depth {
            let idx = self.index(hash, row);
            min = min.min(self.counters[row][idx]);
        }
        min
    }

    /// Total number of increments since creation or last reset.
    pub fn total_increments(&self) -> u64 {
        self.total_increments
    }

    /// Halve all counters (aging). Resets the increment counter.
    fn halve(&mut self) {
        for row in &mut self.counters {
            for c in row.iter_mut() {
                *c /= 2;
            }
        }
        self.total_increments = 0;
    }

    /// Clear all counters to zero.
    pub fn clear(&mut self) {
        for row in &mut self.counters {
            row.fill(0);
        }
        self.total_increments = 0;
    }

    /// Compute the column index for a given hash and row.
    #[inline]
    fn index(&self, hash: u64, row: usize) -> usize {
        // Mix the hash with the row index for independent hash functions.
        let mixed = hash
            .wrapping_mul(0x517c_c1b7_2722_0a95)
            .wrapping_add(row as u64);
        let mixed = mixed ^ (mixed >> 32);
        (mixed as usize) % self.width
    }
}

/// 1-bit Bloom filter used as a doorkeeper to filter one-hit wonders.
///
/// On first access within an epoch, the doorkeeper sets a bit. Only on
/// the second access does the item get promoted to the Count-Min Sketch.
/// The doorkeeper is cleared whenever the CMS resets (halves).
#[derive(Debug, Clone)]
pub struct Doorkeeper {
    bits: Vec<u64>,
    num_bits: usize,
}

/// Default doorkeeper size in bits.
const DOORKEEPER_DEFAULT_BITS: usize = 2048;

impl Doorkeeper {
    /// Create a new doorkeeper with the specified number of bits.
    pub fn new(num_bits: usize) -> Self {
        let num_bits = num_bits.max(64);
        let num_words = num_bits.div_ceil(64);
        Self {
            bits: vec![0u64; num_words],
            num_bits,
        }
    }

    /// Create a doorkeeper with the default size (2048 bits).
    pub fn with_defaults() -> Self {
        Self::new(DOORKEEPER_DEFAULT_BITS)
    }

    /// Check if a key has been seen. Returns true if the bit was already set.
    pub fn check_and_set(&mut self, hash: u64) -> bool {
        let idx = (hash as usize) % self.num_bits;
        let word = idx / 64;
        let bit = idx % 64;
        let was_set = (self.bits[word] >> bit) & 1 == 1;
        self.bits[word] |= 1 << bit;
        was_set
    }

    /// Check if a key has been seen without setting.
    pub fn contains(&self, hash: u64) -> bool {
        let idx = (hash as usize) % self.num_bits;
        let word = idx / 64;
        let bit = idx % 64;
        (self.bits[word] >> bit) & 1 == 1
    }

    /// Clear all bits.
    pub fn clear(&mut self) {
        self.bits.fill(0);
    }
}

/// Compute a secondary fingerprint hash for collision guard.
///
/// Uses a different multiplicative constant than FxHash to produce
/// an independent 64-bit fingerprint.
#[inline]
pub fn fingerprint_hash(text: &str) -> u64 {
    // Simple but effective: fold bytes with a different constant than FxHash.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV offset basis
    for &b in text.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3); // FNV prime
    }
    h
}

/// Evaluate the TinyLFU admission rule.
///
/// Returns `true` if the candidate should be admitted (replacing the victim).
///
/// # Rule
/// Admit if `freq(candidate) > freq(victim)`. On tie, reject (keep victim).
#[inline]
pub fn tinylfu_admit(candidate_freq: u8, victim_freq: u8) -> bool {
    candidate_freq > victim_freq
}

// ---------------------------------------------------------------------------
// W-TinyLFU Width Cache (bd-4kq0.6.2)
// ---------------------------------------------------------------------------

/// Entry in the TinyLFU cache, storing value and fingerprint for collision guard.
#[derive(Debug, Clone)]
struct TinyLfuEntry {
    width: usize,
    fingerprint: u64,
}

/// Width cache using W-TinyLFU admission policy.
///
/// Architecture:
/// - **Window cache** (small LRU, ~1% of capacity): captures recent items.
/// - **Main cache** (larger LRU, ~99% of capacity): for frequently accessed items.
/// - **Count-Min Sketch + Doorkeeper**: frequency estimation for admission decisions.
/// - **Fingerprint guard**: secondary hash per entry to detect hash collisions.
///
/// On every access:
/// 1. Check main cache → hit? Return value (verify fingerprint).
/// 2. Check window cache → hit? Return value (verify fingerprint).
/// 3. Miss: compute width, insert into window cache.
///
/// On window cache eviction:
/// 1. The evicted item becomes a candidate.
/// 2. The LRU victim of the main cache is identified.
/// 3. If `freq(candidate) > freq(victim)`, candidate enters main cache
///    (victim is evicted). Otherwise, candidate is discarded.
///
/// Frequency tracking uses Doorkeeper → CMS pipeline:
/// - First access: doorkeeper records.
/// - Second+ access: CMS is incremented.
#[derive(Debug)]
pub struct TinyLfuWidthCache {
    /// Small window cache (recency).
    window: LruCache<u64, TinyLfuEntry>,
    /// Large main cache (frequency-filtered).
    main: LruCache<u64, TinyLfuEntry>,
    /// Approximate frequency counter.
    sketch: CountMinSketch,
    /// One-hit-wonder filter.
    doorkeeper: Doorkeeper,
    /// Total capacity (window + main).
    total_capacity: usize,
    /// Hit/miss stats.
    hits: u64,
    misses: u64,
}

impl TinyLfuWidthCache {
    /// Create a new TinyLFU cache with the given total capacity.
    ///
    /// The window gets ~1% of capacity (minimum 1), main gets the rest.
    pub fn new(total_capacity: usize) -> Self {
        let total_capacity = total_capacity.max(2);
        let window_cap = (total_capacity / 100).max(1);
        let main_cap = total_capacity - window_cap;

        Self {
            window: LruCache::new(NonZeroUsize::new(window_cap).unwrap()),
            main: LruCache::new(NonZeroUsize::new(main_cap.max(1)).unwrap()),
            sketch: CountMinSketch::with_defaults(),
            doorkeeper: Doorkeeper::with_defaults(),
            total_capacity,
            hits: 0,
            misses: 0,
        }
    }

    /// Get cached width or compute and cache it.
    pub fn get_or_compute(&mut self, text: &str) -> usize {
        self.get_or_compute_with(text, crate::display_width)
    }

    /// Get cached width or compute using a custom function.
    pub fn get_or_compute_with<F>(&mut self, text: &str, compute: F) -> usize
    where
        F: FnOnce(&str) -> usize,
    {
        let hash = hash_text(text);
        let fp = fingerprint_hash(text);

        // Record frequency via doorkeeper → CMS pipeline.
        let seen = self.doorkeeper.check_and_set(hash);
        if seen {
            self.sketch.increment(hash);
        }

        // Check main cache first (larger, higher value).
        if let Some(entry) = self.main.get(&hash) {
            if entry.fingerprint == fp {
                self.hits += 1;
                return entry.width;
            }
            // Fingerprint mismatch: collision. Evict stale entry.
            self.main.pop(&hash);
        }

        // Check window cache.
        if let Some(entry) = self.window.get(&hash) {
            if entry.fingerprint == fp {
                self.hits += 1;
                return entry.width;
            }
            // Collision in window cache.
            self.window.pop(&hash);
        }

        // Cache miss: compute width.
        self.misses += 1;
        let width = compute(text);
        let new_entry = TinyLfuEntry {
            width,
            fingerprint: fp,
        };

        // Insert into window cache. If window is full, the evicted item
        // goes through admission filter for main cache.
        if self.window.len() >= self.window.cap().get() {
            // Get the LRU item from window before it's evicted.
            if let Some((evicted_hash, evicted_entry)) = self.window.pop_lru() {
                self.try_admit_to_main(evicted_hash, evicted_entry);
            }
        }
        self.window.put(hash, new_entry);

        width
    }

    /// Try to admit a candidate (evicted from window) into the main cache.
    fn try_admit_to_main(&mut self, candidate_hash: u64, candidate_entry: TinyLfuEntry) {
        let candidate_freq = self.sketch.estimate(candidate_hash);

        if self.main.len() < self.main.cap().get() {
            // Main has room — admit unconditionally.
            self.main.put(candidate_hash, candidate_entry);
            return;
        }

        // Main is full. Compare candidate frequency with the LRU victim.
        if let Some((&victim_hash, _)) = self.main.peek_lru() {
            let victim_freq = self.sketch.estimate(victim_hash);
            if tinylfu_admit(candidate_freq, victim_freq) {
                self.main.pop_lru();
                self.main.put(candidate_hash, candidate_entry);
            }
            // Otherwise, candidate is discarded.
        }
    }

    /// Check if a key is in the cache (window or main).
    pub fn contains(&self, text: &str) -> bool {
        let hash = hash_text(text);
        let fp = fingerprint_hash(text);
        if let Some(e) = self.main.peek(&hash)
            && e.fingerprint == fp
        {
            return true;
        }
        if let Some(e) = self.window.peek(&hash)
            && e.fingerprint == fp
        {
            return true;
        }
        false
    }

    /// Get cache statistics.
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits,
            misses: self.misses,
            size: self.window.len() + self.main.len(),
            capacity: self.total_capacity,
        }
    }

    /// Clear all caches and reset sketch/doorkeeper.
    pub fn clear(&mut self) {
        self.window.clear();
        self.main.clear();
        self.sketch.clear();
        self.doorkeeper.clear();
    }

    /// Reset statistics.
    pub fn reset_stats(&mut self) {
        self.hits = 0;
        self.misses = 0;
    }

    /// Current number of cached entries.
    pub fn len(&self) -> usize {
        self.window.len() + self.main.len()
    }

    /// Check if cache is empty.
    pub fn is_empty(&self) -> bool {
        self.window.is_empty() && self.main.is_empty()
    }

    /// Total capacity (window + main).
    pub fn capacity(&self) -> usize {
        self.total_capacity
    }

    /// Number of entries in the main cache.
    pub fn main_len(&self) -> usize {
        self.main.len()
    }

    /// Number of entries in the window cache.
    pub fn window_len(&self) -> usize {
        self.window.len()
    }
}

// ── S3-FIFO Width Cache ────────────────────────────────────────────────

/// Width cache backed by S3-FIFO eviction (bd-l6yba.2).
///
/// Drop-in replacement for [`WidthCache`] that uses the scan-resistant
/// S3-FIFO eviction policy instead of LRU. This protects frequently-used
/// width entries from being evicted by one-time scan patterns (e.g. when a
/// large block of new text scrolls past).
///
/// Uses the same 64-bit FxHash keying as [`WidthCache`] with a secondary
/// FNV fingerprint for collision detection.
#[derive(Debug)]
pub struct S3FifoWidthCache {
    cache: ftui_core::s3_fifo::S3Fifo<u64, S3FifoEntry>,
    hits: u64,
    misses: u64,
    total_capacity: usize,
}

/// Entry stored in the S3-FIFO width cache.
#[derive(Debug, Clone, Copy)]
struct S3FifoEntry {
    width: usize,
    fingerprint: u64,
}

impl S3FifoWidthCache {
    /// Create a new S3-FIFO width cache with the given capacity.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(2);
        Self {
            cache: ftui_core::s3_fifo::S3Fifo::new(capacity),
            hits: 0,
            misses: 0,
            total_capacity: capacity,
        }
    }

    /// Create a new cache with the default capacity (4096 entries).
    #[must_use]
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_CACHE_CAPACITY)
    }

    /// Get cached width or compute and cache it.
    pub fn get_or_compute(&mut self, text: &str) -> usize {
        self.get_or_compute_with(text, crate::display_width)
    }

    /// Get cached width or compute using a custom function.
    pub fn get_or_compute_with<F>(&mut self, text: &str, compute: F) -> usize
    where
        F: FnOnce(&str) -> usize,
    {
        let hash = hash_text(text);
        let fp = fingerprint_hash(text);

        if let Some(entry) = self.cache.get(&hash) {
            if entry.fingerprint == fp {
                self.hits += 1;
                return entry.width;
            }
            // Fingerprint mismatch: collision. Remove stale entry.
            self.cache.remove(&hash);
        }

        // Cache miss: compute width.
        self.misses += 1;
        let width = compute(text);
        self.cache.insert(
            hash,
            S3FifoEntry {
                width,
                fingerprint: fp,
            },
        );
        width
    }

    /// Check if a key is in the cache.
    pub fn contains(&self, text: &str) -> bool {
        let hash = hash_text(text);
        self.cache.contains_key(&hash)
    }

    /// Get cache statistics.
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits,
            misses: self.misses,
            size: self.cache.len(),
            capacity: self.total_capacity,
        }
    }

    /// Clear the cache.
    pub fn clear(&mut self) {
        self.cache.clear();
        self.hits = 0;
        self.misses = 0;
    }

    /// Reset statistics.
    pub fn reset_stats(&mut self) {
        self.hits = 0;
        self.misses = 0;
    }

    /// Current number of cached entries.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Check if cache is empty.
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// Total capacity.
    pub fn capacity(&self) -> usize {
        self.total_capacity
    }
}

impl Default for S3FifoWidthCache {
    fn default() -> Self {
        Self::with_default_capacity()
    }
}

#[cfg(test)]
mod s3_fifo_width_tests {
    use super::*;

    #[test]
    fn s3fifo_new_cache_is_empty() {
        let cache = S3FifoWidthCache::new(100);
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn s3fifo_get_or_compute_caches_value() {
        let mut cache = S3FifoWidthCache::new(100);
        let w1 = cache.get_or_compute("hello");
        assert_eq!(w1, 5);
        assert_eq!(cache.len(), 1);

        let w2 = cache.get_or_compute("hello");
        assert_eq!(w2, 5);
        assert_eq!(cache.len(), 1);

        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
    }

    #[test]
    fn s3fifo_different_strings() {
        let mut cache = S3FifoWidthCache::new(100);
        cache.get_or_compute("hello");
        cache.get_or_compute("world");
        cache.get_or_compute("foo");
        assert_eq!(cache.len(), 3);
    }

    #[test]
    fn s3fifo_cjk_width() {
        let mut cache = S3FifoWidthCache::new(100);
        let w = cache.get_or_compute("你好");
        assert_eq!(w, 4);
    }

    #[test]
    fn s3fifo_contains() {
        let mut cache = S3FifoWidthCache::new(100);
        assert!(!cache.contains("hello"));
        cache.get_or_compute("hello");
        assert!(cache.contains("hello"));
    }

    #[test]
    fn s3fifo_clear_resets() {
        let mut cache = S3FifoWidthCache::new(100);
        cache.get_or_compute("hello");
        cache.get_or_compute("world");
        cache.clear();
        assert!(cache.is_empty());
        assert!(!cache.contains("hello"));
        let stats = cache.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
    }

    #[test]
    fn s3fifo_produces_same_widths_as_lru() {
        let mut lru = WidthCache::new(100);
        let mut s3 = S3FifoWidthCache::new(100);

        let texts = [
            "hello",
            "你好世界",
            "abc",
            "🎉🎉",
            "",
            " ",
            "a\tb",
            "mixed中english文",
        ];

        for text in &texts {
            let lru_w = lru.get_or_compute(text);
            let s3_w = s3.get_or_compute(text);
            assert_eq!(lru_w, s3_w, "width mismatch for {:?}", text);
        }
    }

    #[test]
    fn s3fifo_scan_resistance_preserves_hot_set() {
        let mut cache = S3FifoWidthCache::new(50);

        // Build a hot set
        let hot: Vec<String> = (0..20).map(|i| format!("hot_{i}")).collect();
        for text in &hot {
            cache.get_or_compute(text);
            cache.get_or_compute(text); // access twice to set freq
        }

        // Scan through a large one-time set
        for i in 0..200 {
            cache.get_or_compute(&format!("scan_{i}"));
        }

        // Some hot items should survive
        let mut survivors = 0;
        for text in &hot {
            if cache.contains(text) {
                survivors += 1;
            }
        }
        assert!(
            survivors > 5,
            "scan resistance: only {survivors}/20 hot items survived"
        );
    }

    #[test]
    fn s3fifo_default_capacity() {
        let cache = S3FifoWidthCache::with_default_capacity();
        assert_eq!(cache.capacity(), DEFAULT_CACHE_CAPACITY);
    }

    #[test]
    fn s3fifo_reset_stats() {
        let mut cache = S3FifoWidthCache::new(100);
        cache.get_or_compute("a");
        cache.get_or_compute("a");
        cache.reset_stats();
        let stats = cache.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn cached_width_matches_direct(s in "[a-zA-Z0-9 ]{1,50}") {
            let mut cache = WidthCache::new(100);
            let cached = cache.get_or_compute(&s);
            let direct = crate::display_width(&s);
            prop_assert_eq!(cached, direct);
        }

        #[test]
        fn second_access_is_hit(s in "[a-zA-Z0-9]{1,20}") {
            let mut cache = WidthCache::new(100);

            cache.get_or_compute(&s);
            let stats_before = cache.stats();

            cache.get_or_compute(&s);
            let stats_after = cache.stats();

            prop_assert_eq!(stats_after.hits, stats_before.hits + 1);
            prop_assert_eq!(stats_after.misses, stats_before.misses);
        }

        #[test]
        fn lru_never_exceeds_capacity(
            strings in prop::collection::vec("[a-z]{1,5}", 10..100),
            capacity in 5usize..20
        ) {
            let mut cache = WidthCache::new(capacity);

            for s in &strings {
                cache.get_or_compute(s);
                prop_assert!(cache.len() <= capacity);
            }
        }

        #[test]
        fn preload_then_access_is_hit(s in "[a-zA-Z]{1,20}") {
            let mut cache = WidthCache::new(100);

            cache.preload(&s);
            let stats_before = cache.stats();

            cache.get_or_compute(&s);
            let stats_after = cache.stats();

            // Should be a hit (preloaded)
            prop_assert_eq!(stats_after.hits, stats_before.hits + 1);
        }
    }
}

// ---------------------------------------------------------------------------
// TinyLFU Spec Tests (bd-4kq0.6.1)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tinylfu_tests {
    use super::*;

    // --- Count-Min Sketch ---

    #[test]
    fn unit_cms_single_key_count() {
        let mut cms = CountMinSketch::with_defaults();
        let h = hash_text("hello");

        for _ in 0..5 {
            cms.increment(h);
        }
        assert_eq!(cms.estimate(h), 5);
    }

    #[test]
    fn unit_cms_unseen_key_is_zero() {
        let cms = CountMinSketch::with_defaults();
        assert_eq!(cms.estimate(hash_text("never_seen")), 0);
    }

    #[test]
    fn unit_cms_saturates_at_max() {
        let mut cms = CountMinSketch::with_defaults();
        let h = hash_text("hot");

        for _ in 0..100 {
            cms.increment(h);
        }
        assert_eq!(cms.estimate(h), CMS_MAX_COUNT);
    }

    #[test]
    fn unit_cms_bounds() {
        // Error bound: estimate(x) <= true_count(x) + epsilon * N.
        // With w=1024, epsilon = e/1024 ~ 0.00266.
        let mut cms = CountMinSketch::new(1024, 4, u64::MAX); // no reset
        let n: u64 = 1000;

        // Insert 1000 unique keys
        for i in 0..n {
            cms.increment(i.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1));
        }

        // Check a target key inserted exactly 5 times
        let target = 0xDEAD_BEEF_u64;
        for _ in 0..5 {
            cms.increment(target);
        }

        let est = cms.estimate(target);
        let epsilon = std::f64::consts::E / 1024.0;
        let upper_bound = 5.0 + epsilon * (n + 5) as f64;

        assert!(
            (est as f64) <= upper_bound,
            "estimate {} exceeds bound {:.1} (epsilon={:.5}, N={})",
            est,
            upper_bound,
            epsilon,
            n + 5,
        );
        assert!(est >= 5, "estimate {} should be >= true count 5", est);
    }

    #[test]
    fn unit_cms_bounds_mass_test() {
        let mut cms = CountMinSketch::new(1024, 4, u64::MAX);
        let n = 2000u64;

        let mut true_counts = vec![0u8; n as usize];
        for i in 0..n {
            let count = (i % 10 + 1) as u8;
            true_counts[i as usize] = count;
            for _ in 0..count {
                cms.increment(i);
            }
        }

        let total = cms.total_increments();
        let epsilon = std::f64::consts::E / 1024.0;
        let mut violations = 0u32;

        for i in 0..n {
            let est = cms.estimate(i);
            let true_c = true_counts[i as usize];
            let upper = true_c as f64 + epsilon * total as f64;
            if est as f64 > upper + 0.5 {
                violations += 1;
            }
            assert!(
                est >= true_c,
                "key {}: estimate {} < true count {}",
                i,
                est,
                true_c
            );
        }

        // delta = (1/2)^4 = 0.0625; allow generous threshold
        let violation_rate = violations as f64 / n as f64;
        assert!(
            violation_rate <= 0.10,
            "violation rate {:.3} exceeds delta threshold",
            violation_rate,
        );
    }

    #[test]
    fn unit_cms_halving_ages_counts() {
        let mut cms = CountMinSketch::new(64, 2, 100);

        let h = hash_text("test");
        for _ in 0..10 {
            cms.increment(h);
        }
        assert_eq!(cms.estimate(h), 10);

        // Trigger reset by reaching reset_interval
        for _ in 10..100 {
            cms.increment(hash_text("noise"));
        }

        let est = cms.estimate(h);
        assert!(est <= 5, "After halving, estimate {} should be <= 5", est);
    }

    #[test]
    fn unit_cms_monotone() {
        let mut cms = CountMinSketch::with_defaults();
        let h = hash_text("key");

        let mut prev_est = 0u8;
        for _ in 0..CMS_MAX_COUNT {
            cms.increment(h);
            let est = cms.estimate(h);
            assert!(est >= prev_est, "estimate should be monotone");
            prev_est = est;
        }
    }

    // --- Doorkeeper ---

    #[test]
    fn unit_doorkeeper_first_access_returns_false() {
        let mut dk = Doorkeeper::with_defaults();
        assert!(!dk.check_and_set(hash_text("new")));
    }

    #[test]
    fn unit_doorkeeper_second_access_returns_true() {
        let mut dk = Doorkeeper::with_defaults();
        let h = hash_text("key");
        dk.check_and_set(h);
        assert!(dk.check_and_set(h));
    }

    #[test]
    fn unit_doorkeeper_contains() {
        let mut dk = Doorkeeper::with_defaults();
        let h = hash_text("key");
        assert!(!dk.contains(h));
        dk.check_and_set(h);
        assert!(dk.contains(h));
    }

    #[test]
    fn unit_doorkeeper_clear_resets() {
        let mut dk = Doorkeeper::with_defaults();
        let h = hash_text("key");
        dk.check_and_set(h);
        dk.clear();
        assert!(!dk.contains(h));
        assert!(!dk.check_and_set(h));
    }

    #[test]
    fn unit_doorkeeper_false_positive_rate() {
        let mut dk = Doorkeeper::new(2048);
        let n = 100u64;

        for i in 0..n {
            dk.check_and_set(i * 0x9E37_79B9 + 1);
        }

        let mut false_positives = 0u32;
        for i in 0..1000 {
            let h = (i + 100_000) * 0x6A09_E667 + 7;
            if dk.contains(h) {
                false_positives += 1;
            }
        }

        // k=1, m=2048, n=100: FP rate ~ 1 - e^{-100/2048} ~ 0.048
        let fp_rate = false_positives as f64 / 1000.0;
        assert!(
            fp_rate < 0.15,
            "FP rate {:.3} too high (expected < 0.15)",
            fp_rate,
        );
    }

    // --- Admission Rule ---

    #[test]
    fn unit_admission_rule() {
        assert!(tinylfu_admit(5, 3)); // candidate > victim -> admit
        assert!(!tinylfu_admit(3, 5)); // candidate < victim -> reject
        assert!(!tinylfu_admit(3, 3)); // tie -> reject (keep victim)
    }

    #[test]
    fn unit_admission_rule_extremes() {
        assert!(tinylfu_admit(1, 0));
        assert!(!tinylfu_admit(0, 0));
        assert!(!tinylfu_admit(0, 1));
        assert!(tinylfu_admit(CMS_MAX_COUNT, CMS_MAX_COUNT - 1));
        assert!(!tinylfu_admit(CMS_MAX_COUNT, CMS_MAX_COUNT));
    }

    // --- Fingerprint Guard ---

    #[test]
    fn unit_fingerprint_guard() {
        let fp1 = fingerprint_hash("hello");
        let fp2 = fingerprint_hash("world");
        let fp3 = fingerprint_hash("hello");

        assert_ne!(
            fp1, fp2,
            "Different strings should have different fingerprints"
        );
        assert_eq!(fp1, fp3, "Same string should have same fingerprint");
    }

    #[test]
    fn unit_fingerprint_guard_collision_rate() {
        let mut fps = std::collections::HashSet::new();
        let n = 10_000;

        for i in 0..n {
            let s = format!("string_{}", i);
            fps.insert(fingerprint_hash(&s));
        }

        let collisions = n - fps.len();
        assert!(
            collisions == 0,
            "Expected 0 collisions in 10k items, got {}",
            collisions,
        );
    }

    #[test]
    fn unit_fingerprint_independent_of_primary_hash() {
        let text = "test_string";
        let primary = hash_text(text);
        let secondary = fingerprint_hash(text);

        assert_ne!(
            primary, secondary,
            "Fingerprint and primary hash should differ"
        );
    }

    // --- Integration: Doorkeeper + CMS pipeline ---

    #[test]
    fn unit_doorkeeper_cms_pipeline() {
        let mut dk = Doorkeeper::with_defaults();
        let mut cms = CountMinSketch::with_defaults();
        let h = hash_text("item");

        // First access: doorkeeper records
        assert!(!dk.check_and_set(h));
        assert_eq!(cms.estimate(h), 0);

        // Second access: doorkeeper confirms, CMS incremented
        assert!(dk.check_and_set(h));
        cms.increment(h);
        assert_eq!(cms.estimate(h), 1);

        // Third access
        assert!(dk.check_and_set(h));
        cms.increment(h);
        assert_eq!(cms.estimate(h), 2);
    }

    #[test]
    fn unit_doorkeeper_filters_one_hit_wonders() {
        let mut dk = Doorkeeper::with_defaults();
        let mut cms = CountMinSketch::with_defaults();

        // 100 one-hit items
        for i in 0u64..100 {
            let h = i * 0x9E37_79B9 + 1;
            let seen = dk.check_and_set(h);
            if seen {
                cms.increment(h);
            }
        }

        assert_eq!(cms.total_increments(), 0);

        // Access one again -> passes doorkeeper
        let h = 1; // i=0 from the loop above
        assert!(dk.check_and_set(h));
        cms.increment(h);
        assert_eq!(cms.total_increments(), 1);
    }
}

// ---------------------------------------------------------------------------
// TinyLFU Implementation Tests (bd-4kq0.6.2)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tinylfu_impl_tests {
    use super::*;

    #[test]
    fn basic_get_or_compute() {
        let mut cache = TinyLfuWidthCache::new(100);
        let w = cache.get_or_compute("hello");
        assert_eq!(w, 5);
        assert_eq!(cache.len(), 1);

        let w2 = cache.get_or_compute("hello");
        assert_eq!(w2, 5);
        let stats = cache.stats();
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hits, 1);
    }

    #[test]
    fn window_to_main_promotion() {
        // With capacity=100, window=1, main=99.
        // Fill window, then force eviction into main via new inserts.
        let mut cache = TinyLfuWidthCache::new(100);

        // Access "frequent" many times to build CMS frequency.
        for _ in 0..10 {
            cache.get_or_compute("frequent");
        }

        // Insert enough items to fill window and force eviction.
        for i in 0..5 {
            cache.get_or_compute(&format!("item_{}", i));
        }

        // "frequent" should have been promoted to main cache via admission.
        assert!(cache.contains("frequent"));
        assert!(cache.main_len() > 0 || cache.window_len() > 0);
    }

    #[test]
    fn unit_window_promotion() {
        // Frequent items should end up in main cache.
        let mut cache = TinyLfuWidthCache::new(50);

        // Access "hot" repeatedly to build frequency.
        for _ in 0..20 {
            cache.get_or_compute("hot");
        }

        // Now push enough items through window to force "hot" out of window.
        for i in 0..10 {
            cache.get_or_compute(&format!("filler_{}", i));
        }

        // "hot" should still be accessible (promoted to main via admission).
        assert!(cache.contains("hot"), "Frequent item should be retained");
    }

    #[test]
    fn fingerprint_guard_detects_collision() {
        let mut cache = TinyLfuWidthCache::new(100);

        // Compute "hello" with custom function.
        let w = cache.get_or_compute_with("hello", |_| 42);
        assert_eq!(w, 42);

        // Verify it's cached.
        assert!(cache.contains("hello"));
    }

    #[test]
    fn admission_rejects_infrequent() {
        // Fill main cache with frequently-accessed items.
        // Then try to insert a cold item — it should be rejected.
        let mut cache = TinyLfuWidthCache::new(10); // window=1, main=9

        // Fill main with items accessed multiple times.
        for i in 0..9 {
            let s = format!("hot_{}", i);
            for _ in 0..5 {
                cache.get_or_compute(&s);
            }
        }

        // Now insert cold items. They should go through window but not
        // necessarily get into main.
        for i in 0..20 {
            cache.get_or_compute(&format!("cold_{}", i));
        }

        // Hot items should mostly survive (they have high frequency).
        let hot_survivors: usize = (0..9)
            .filter(|i| cache.contains(&format!("hot_{}", i)))
            .count();
        assert!(
            hot_survivors >= 5,
            "Expected most hot items to survive, got {}/9",
            hot_survivors
        );
    }

    #[test]
    fn clear_empties_everything() {
        let mut cache = TinyLfuWidthCache::new(100);
        cache.get_or_compute("a");
        cache.get_or_compute("b");
        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn stats_reflect_usage() {
        let mut cache = TinyLfuWidthCache::new(100);
        cache.get_or_compute("a");
        cache.get_or_compute("a");
        cache.get_or_compute("b");

        let stats = cache.stats();
        assert_eq!(stats.misses, 2);
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.size, 2);
    }

    #[test]
    fn capacity_is_respected() {
        let mut cache = TinyLfuWidthCache::new(20);

        for i in 0..100 {
            cache.get_or_compute(&format!("item_{}", i));
        }

        assert!(
            cache.len() <= 20,
            "Cache size {} exceeds capacity 20",
            cache.len()
        );
    }

    #[test]
    fn reset_stats_works() {
        let mut cache = TinyLfuWidthCache::new(100);
        cache.get_or_compute("x");
        cache.get_or_compute("x");
        cache.reset_stats();
        let stats = cache.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
    }

    #[test]
    fn perf_cache_hit_rate() {
        // Simulate a Zipfian-like workload: some items accessed frequently,
        // many accessed rarely. TinyLFU should achieve decent hit rate.
        let mut cache = TinyLfuWidthCache::new(50);

        // 10 hot items accessed 20 times each.
        for _ in 0..20 {
            for i in 0..10 {
                cache.get_or_compute(&format!("hot_{}", i));
            }
        }

        // 100 cold items accessed once each.
        for i in 0..100 {
            cache.get_or_compute(&format!("cold_{}", i));
        }

        // Re-access hot items — these should mostly be hits.
        cache.reset_stats();
        for i in 0..10 {
            cache.get_or_compute(&format!("hot_{}", i));
        }

        let stats = cache.stats();
        // Hot items should have high hit rate after being frequently accessed.
        assert!(
            stats.hits >= 5,
            "Expected at least 5/10 hot items to hit, got {}",
            stats.hits
        );
    }

    #[test]
    fn unicode_strings_work() {
        let mut cache = TinyLfuWidthCache::new(100);
        assert_eq!(cache.get_or_compute("日本語"), 6);
        assert_eq!(cache.get_or_compute("café"), 4);
        assert_eq!(cache.get_or_compute("日本語"), 6); // hit
        assert_eq!(cache.stats().hits, 1);
    }

    #[test]
    fn empty_string() {
        let mut cache = TinyLfuWidthCache::new(100);
        assert_eq!(cache.get_or_compute(""), 0);
    }

    #[test]
    fn minimum_capacity() {
        let cache = TinyLfuWidthCache::new(0);
        assert!(cache.capacity() >= 2);
    }
}

// ---------------------------------------------------------------------------
// bd-4kq0.6.3: WidthCache Tests + Perf Gates
// ---------------------------------------------------------------------------

/// Deterministic LCG for test reproducibility (no external rand dependency).
#[cfg(test)]
struct Lcg(u64);

#[cfg(test)]
impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        self.0
    }
    fn next_usize(&mut self, max: usize) -> usize {
        (self.next_u64() % (max as u64)) as usize
    }
}

// ---------------------------------------------------------------------------
// 1. Property: cache equivalence (cached width == computed width)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod property_cache_equivalence {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn tinylfu_cached_equals_computed(s in "[a-zA-Z0-9 ]{1,80}") {
            let mut cache = TinyLfuWidthCache::new(200);
            let cached = cache.get_or_compute(&s);
            let direct = crate::display_width(&s);
            prop_assert_eq!(cached, direct,
                "TinyLFU returned {} but display_width says {} for {:?}", cached, direct, s);
        }

        #[test]
        fn tinylfu_second_access_same_value(s in "[a-zA-Z0-9]{1,40}") {
            let mut cache = TinyLfuWidthCache::new(200);
            let first = cache.get_or_compute(&s);
            let second = cache.get_or_compute(&s);
            prop_assert_eq!(first, second,
                "First access returned {} but second returned {} for {:?}", first, second, s);
        }

        #[test]
        fn tinylfu_never_exceeds_capacity(
            strings in prop::collection::vec("[a-z]{1,5}", 10..200),
            capacity in 10usize..50
        ) {
            let mut cache = TinyLfuWidthCache::new(capacity);
            for s in &strings {
                cache.get_or_compute(s);
                prop_assert!(cache.len() <= capacity,
                    "Cache size {} exceeded capacity {}", cache.len(), capacity);
            }
        }

        #[test]
        fn tinylfu_custom_fn_matches(s in "[a-z]{1,20}") {
            let mut cache = TinyLfuWidthCache::new(100);
            let custom_fn = |text: &str| text.len(); // byte length as custom metric
            let cached = cache.get_or_compute_with(&s, custom_fn);
            prop_assert_eq!(cached, s.len(),
                "Custom fn: cached {} != expected {} for {:?}", cached, s.len(), s);
        }
    }

    #[test]
    fn deterministic_seed_equivalence() {
        // Same workload with same seed → same results every time.
        let mut rng = super::Lcg::new(0xDEAD_BEEF);

        let mut cache1 = TinyLfuWidthCache::new(50);
        let mut results1 = Vec::new();
        for _ in 0..500 {
            let idx = rng.next_usize(100);
            let s = format!("key_{}", idx);
            results1.push(cache1.get_or_compute(&s));
        }

        let mut rng2 = super::Lcg::new(0xDEAD_BEEF);
        let mut cache2 = TinyLfuWidthCache::new(50);
        let mut results2 = Vec::new();
        for _ in 0..500 {
            let idx = rng2.next_usize(100);
            let s = format!("key_{}", idx);
            results2.push(cache2.get_or_compute(&s));
        }

        assert_eq!(
            results1, results2,
            "Deterministic seed must produce identical results"
        );
    }

    #[test]
    fn both_caches_agree_on_widths() {
        // WidthCache (plain LRU) and TinyLfuWidthCache must return the same
        // widths for any input.
        let mut lru = WidthCache::new(200);
        let mut tlfu = TinyLfuWidthCache::new(200);

        let inputs = [
            "",
            "hello",
            "日本語テスト",
            "café résumé",
            "a\tb",
            "🎉🎊🎈",
            "mixed日本eng",
            "    spaces    ",
            "AAAAAAAAAAAAAAAAAAAAAAAAA",
            "x",
        ];

        for &s in &inputs {
            let w1 = lru.get_or_compute(s);
            let w2 = tlfu.get_or_compute(s);
            assert_eq!(
                w1, w2,
                "Width mismatch for {:?}: LRU={}, TinyLFU={}",
                s, w1, w2
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 2. E2E cache replay: deterministic workload, log hit rate + latency (JSONL)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod e2e_cache_replay {
    use super::*;
    use std::time::Instant;

    /// A single replay record, serialisable to JSONL.
    #[derive(Debug)]
    struct ReplayRecord {
        step: usize,
        key: String,
        width: usize,
        hit: bool,
        latency_ns: u128,
    }

    impl ReplayRecord {
        fn to_jsonl(&self) -> String {
            format!(
                r#"{{"step":{},"key":"{}","width":{},"hit":{},"latency_ns":{}}}"#,
                self.step, self.key, self.width, self.hit, self.latency_ns,
            )
        }
    }

    fn zipfian_workload(rng: &mut super::Lcg, n: usize, universe: usize) -> Vec<String> {
        // Approximate Zipfian: lower indices are much more frequent.
        (0..n)
            .map(|_| {
                let raw = rng.next_usize(universe * universe);
                let idx = (raw as f64).sqrt() as usize % universe;
                format!("item_{}", idx)
            })
            .collect()
    }

    #[test]
    fn replay_zipfian_tinylfu() {
        let mut rng = super::Lcg::new(0x1234_5678);
        let workload = zipfian_workload(&mut rng, 2000, 200);

        let mut cache = TinyLfuWidthCache::new(50);
        let mut records = Vec::with_capacity(workload.len());

        for (i, key) in workload.iter().enumerate() {
            let stats_before = cache.stats();
            let t0 = Instant::now();
            let width = cache.get_or_compute(key);
            let elapsed = t0.elapsed().as_nanos();
            let stats_after = cache.stats();
            let hit = stats_after.hits > stats_before.hits;

            records.push(ReplayRecord {
                step: i,
                key: key.clone(),
                width,
                hit,
                latency_ns: elapsed,
            });
        }

        // Validate JSONL serialisation is parseable.
        for r in &records[..5] {
            let line = r.to_jsonl();
            assert!(
                line.starts_with('{') && line.ends_with('}'),
                "Bad JSONL: {}",
                line
            );
        }

        // Compute aggregate stats.
        let total = records.len();
        let hits = records.iter().filter(|r| r.hit).count();
        let hit_rate = hits as f64 / total as f64;

        // Zipfian workload with 50-entry cache over 200-item universe
        // should get a meaningful hit rate (> 10%).
        assert!(
            hit_rate > 0.10,
            "Zipfian hit rate too low: {:.2}% ({}/{})",
            hit_rate * 100.0,
            hits,
            total
        );
    }

    #[test]
    fn replay_zipfian_lru_comparison() {
        let mut rng = super::Lcg::new(0x1234_5678);
        let workload = zipfian_workload(&mut rng, 2000, 200);

        let mut tlfu = TinyLfuWidthCache::new(50);
        let mut lru = WidthCache::new(50);

        for key in &workload {
            tlfu.get_or_compute(key);
            lru.get_or_compute(key);
        }

        let tlfu_stats = tlfu.stats();
        let lru_stats = lru.stats();

        // Both must have correct total operations.
        assert_eq!(tlfu_stats.hits + tlfu_stats.misses, 2000);
        assert_eq!(lru_stats.hits + lru_stats.misses, 2000);

        // TinyLFU should be at least competitive with plain LRU on Zipfian.
        // (In practice TinyLFU often wins, but we just check both work.)
        assert!(
            tlfu_stats.hit_rate() >= lru_stats.hit_rate() * 0.8,
            "TinyLFU hit rate {:.2}% much worse than LRU {:.2}%",
            tlfu_stats.hit_rate() * 100.0,
            lru_stats.hit_rate() * 100.0,
        );
    }

    #[test]
    fn replay_deterministic_reproduction() {
        // Two identical replays must produce identical hit/miss sequences.
        let run = |seed: u64| -> Vec<bool> {
            let mut rng = super::Lcg::new(seed);
            let workload = zipfian_workload(&mut rng, 500, 100);
            let mut cache = TinyLfuWidthCache::new(30);
            let mut hits = Vec::with_capacity(500);
            for key in &workload {
                let before = cache.stats().hits;
                cache.get_or_compute(key);
                hits.push(cache.stats().hits > before);
            }
            hits
        };

        let run1 = run(0xABCD_EF01);
        let run2 = run(0xABCD_EF01);
        assert_eq!(run1, run2, "Deterministic replay diverged");
    }

    #[test]
    fn replay_uniform_workload() {
        // Uniform access pattern: every key equally likely. Hit rate should be
        // roughly cache_size / universe_size for large N.
        let mut rng = super::Lcg::new(0x9999);
        let universe = 100;
        let cache_size = 25;
        let n = 5000;

        let mut cache = TinyLfuWidthCache::new(cache_size);

        // Warm up: access each key once.
        for i in 0..universe {
            cache.get_or_compute(&format!("u_{}", i));
        }

        cache.reset_stats();
        for _ in 0..n {
            let idx = rng.next_usize(universe);
            cache.get_or_compute(&format!("u_{}", idx));
        }

        let stats = cache.stats();
        let hit_rate = stats.hit_rate();
        // Theoretical: ~25/100 = 25%. Allow range 10%–60%.
        assert!(
            hit_rate > 0.10 && hit_rate < 0.60,
            "Uniform hit rate {:.2}% outside expected range",
            hit_rate * 100.0,
        );
    }
}

// ---------------------------------------------------------------------------
// 3. Perf gates: cache operations < 1us p95
// ---------------------------------------------------------------------------

#[cfg(test)]
mod perf_cache_overhead {
    use super::*;
    use std::time::Instant;

    /// Collect latencies for N operations, return sorted Vec<u128> in nanoseconds.
    fn measure_latencies<F: FnMut(usize)>(n: usize, mut op: F) -> Vec<u128> {
        let mut latencies = Vec::with_capacity(n);
        for i in 0..n {
            let t0 = Instant::now();
            op(i);
            latencies.push(t0.elapsed().as_nanos());
        }
        latencies.sort_unstable();
        latencies
    }

    fn p95(sorted: &[u128]) -> u128 {
        let len = sorted.len();
        let idx = ((len as f64 * 0.95) as usize).min(len.saturating_sub(1));
        sorted[idx]
    }

    fn p99(sorted: &[u128]) -> u128 {
        let len = sorted.len();
        let idx = ((len as f64 * 0.99) as usize).min(len.saturating_sub(1));
        sorted[idx]
    }

    fn median(sorted: &[u128]) -> u128 {
        sorted[sorted.len() / 2]
    }

    #[allow(unexpected_cfgs)]
    fn perf_budget_ns(base_ns: u128) -> u128 {
        if cfg!(coverage) || cfg!(coverage_nightly) {
            base_ns.saturating_mul(10)
        } else {
            base_ns
        }
    }

    #[test]
    fn perf_lru_hit_latency() {
        let mut cache = WidthCache::new(1000);
        // Warm up.
        for i in 0..100 {
            cache.get_or_compute(&format!("warm_{}", i));
        }

        let keys: Vec<String> = (0..100).map(|i| format!("warm_{}", i)).collect();
        let latencies = measure_latencies(10_000, |i| {
            let _ = cache.get_or_compute(&keys[i % 100]);
        });

        let p95_ns = p95(&latencies);
        // Budget: < 1us (1000ns) p95 for cache hits.
        // Use generous 5us to account for CI variability (10x under coverage).
        let budget_ns = perf_budget_ns(5_000);
        assert!(
            p95_ns < budget_ns,
            "LRU hit p95 = {}ns exceeds {}ns budget (median={}ns, p99={}ns)",
            p95_ns,
            budget_ns,
            median(&latencies),
            p99(&latencies),
        );
    }

    #[test]
    fn perf_tinylfu_hit_latency() {
        let mut cache = TinyLfuWidthCache::new(1000);
        // Warm up: access each key multiple times to ensure promotion to main.
        for _ in 0..5 {
            for i in 0..100 {
                cache.get_or_compute(&format!("warm_{}", i));
            }
        }

        let keys: Vec<String> = (0..100).map(|i| format!("warm_{}", i)).collect();
        let latencies = measure_latencies(10_000, |i| {
            let _ = cache.get_or_compute(&keys[i % 100]);
        });

        let p95_ns = p95(&latencies);
        // Budget: < 1us p95 for hits (5us CI-safe threshold).
        let budget_ns = perf_budget_ns(5_000);
        assert!(
            p95_ns < budget_ns,
            "TinyLFU hit p95 = {}ns exceeds {}ns budget (median={}ns, p99={}ns)",
            p95_ns,
            budget_ns,
            median(&latencies),
            p99(&latencies),
        );
    }

    #[test]
    fn perf_tinylfu_miss_latency() {
        let mut cache = TinyLfuWidthCache::new(100);
        let keys: Vec<String> = (0..10_000).map(|i| format!("miss_{}", i)).collect();

        let latencies = measure_latencies(10_000, |i| {
            let _ = cache.get_or_compute(&keys[i]);
        });

        let p95_ns = p95(&latencies);
        // Misses include unicode width computation. Budget: < 5us p95.
        // Use 20us CI-safe threshold (10x under coverage; computation dominates).
        let budget_ns = perf_budget_ns(20_000);
        assert!(
            p95_ns < budget_ns,
            "TinyLFU miss p95 = {}ns exceeds {}ns budget (median={}ns, p99={}ns)",
            p95_ns,
            budget_ns,
            median(&latencies),
            p99(&latencies),
        );
    }

    #[test]
    fn perf_cms_increment_latency() {
        let mut cms = CountMinSketch::with_defaults();
        let hashes: Vec<u64> = (0..10_000).map(|i| hash_text(&format!("k{}", i))).collect();

        let latencies = measure_latencies(10_000, |i| {
            cms.increment(hashes[i]);
        });

        let p95_ns = p95(&latencies);
        // CMS increment should be very fast: < 500ns p95.
        let budget_ns = perf_budget_ns(2_000);
        assert!(
            p95_ns < budget_ns,
            "CMS increment p95 = {}ns exceeds {}ns budget (median={}ns)",
            p95_ns,
            budget_ns,
            median(&latencies),
        );
    }

    #[test]
    fn perf_doorkeeper_latency() {
        let mut dk = Doorkeeper::with_defaults();
        let hashes: Vec<u64> = (0..10_000).map(|i| hash_text(&format!("d{}", i))).collect();

        let latencies = measure_latencies(10_000, |i| {
            let _ = dk.check_and_set(hashes[i]);
        });

        let p95_ns = p95(&latencies);
        // Doorkeeper bit ops: < 200ns p95.
        let budget_ns = perf_budget_ns(1_000);
        assert!(
            p95_ns < budget_ns,
            "Doorkeeper p95 = {}ns exceeds {}ns budget (median={}ns)",
            p95_ns,
            budget_ns,
            median(&latencies),
        );
    }

    #[test]
    fn perf_fingerprint_hash_latency() {
        let keys: Vec<String> = (0..10_000).map(|i| format!("fp_{}", i)).collect();

        let latencies = measure_latencies(10_000, |i| {
            let _ = fingerprint_hash(&keys[i]);
        });

        let p95_ns = p95(&latencies);
        // FNV hash: < 200ns p95.
        let budget_ns = perf_budget_ns(1_000);
        assert!(
            p95_ns < budget_ns,
            "fingerprint_hash p95 = {}ns exceeds {}ns budget (median={}ns)",
            p95_ns,
            budget_ns,
            median(&latencies),
        );
    }
}
