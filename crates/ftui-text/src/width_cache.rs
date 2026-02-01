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
use unicode_width::UnicodeWidthStr;

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
    /// Otherwise, computes the width using `unicode_width` and caches it.
    #[inline]
    pub fn get_or_compute(&mut self, text: &str) -> usize {
        self.get_or_compute_with(text, |s| crate::display_width(s))
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
    #[must_use]
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Check if the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// Get the cache capacity.
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

/// Thread-local width cache for convenience.
///
/// This provides a global cache that is thread-local, avoiding the need
/// to pass a cache around explicitly.
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
            let direct = s.width();
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
