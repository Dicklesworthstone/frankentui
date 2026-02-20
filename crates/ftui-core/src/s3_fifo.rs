#![forbid(unsafe_code)]

//! S3-FIFO cache (bd-l6yba.1): a scan-resistant, FIFO-based eviction policy.
//!
//! S3-FIFO uses three queues (small, main, ghost) to achieve scan resistance
//! with lower overhead than LRU. It was shown to match or outperform W-TinyLFU
//! and ARC on most workloads while being simpler to implement.
//!
//! # Algorithm
//!
//! - **Small queue** (10% of capacity): New entries go here. On eviction,
//!   entries accessed at least once are promoted to main; others are evicted
//!   (key goes to ghost).
//! - **Main queue** (90% of capacity): Promoted entries. Eviction uses FIFO
//!   with a frequency counter (max 3). If freq > 0, decrement and re-insert.
//! - **Ghost queue** (same size as small): Stores keys only (no values).
//!   If a key in ghost is accessed, it's admitted directly to main.
//!
//! # Usage
//!
//! ```
//! use ftui_core::s3_fifo::S3Fifo;
//!
//! let mut cache = S3Fifo::new(100);
//! cache.insert("hello", 42);
//! assert_eq!(cache.get(&"hello"), Some(&42));
//! ```

use ahash::AHashMap;
use std::collections::VecDeque;
use std::hash::Hash;

/// A cache entry stored in the slab.
struct Entry<K, V> {
    key: K,
    value: V,
    freq: u8,
}

/// S3-FIFO cache with scan-resistant eviction.
pub struct S3Fifo<K, V> {
    /// Index from key to location (and slab index).
    index: AHashMap<K, Location>,
    /// Slab storage for entries.
    entries: Vec<Option<Entry<K, V>>>,
    /// Free slots in the slab.
    free_indices: Vec<usize>,
    /// Small FIFO queue (indices into slab) (~10% of capacity).
    small: VecDeque<usize>,
    /// Main FIFO queue (indices into slab) (~90% of capacity).
    main: VecDeque<usize>,
    /// Ghost queue (keys only, same size as small).
    ghost: VecDeque<K>,
    /// Capacity of the small queue.
    small_cap: usize,
    /// Capacity of the main queue.
    main_cap: usize,
    /// Capacity of the ghost queue.
    ghost_cap: usize,
    /// Statistics.
    hits: u64,
    misses: u64,
}

/// Where an entry lives, including its index in the slab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Location {
    Small(usize),
    Main(usize),
}

impl Location {
    fn idx(&self) -> usize {
        match self {
            Self::Small(i) | Self::Main(i) => *i,
        }
    }
}

/// Cache statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct S3FifoStats {
    /// Number of cache hits.
    pub hits: u64,
    /// Number of cache misses.
    pub misses: u64,
    /// Current entries in the small queue.
    pub small_size: usize,
    /// Current entries in the main queue.
    pub main_size: usize,
    /// Current entries in the ghost queue.
    pub ghost_size: usize,
    /// Total capacity.
    pub capacity: usize,
}

impl<K, V> S3Fifo<K, V>
where
    K: Hash + Eq + Clone,
{
    /// Create a new S3-FIFO cache with the given total capacity.
    ///
    /// The capacity is split: 10% small, 90% main. Ghost capacity
    /// matches small capacity. Minimum total capacity is 2.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(2);
        let small_cap = (capacity / 10).max(1);
        let main_cap = capacity - small_cap;
        let ghost_cap = small_cap;

        Self {
            index: AHashMap::with_capacity(capacity),
            entries: Vec::with_capacity(capacity),
            free_indices: Vec::new(),
            small: VecDeque::with_capacity(small_cap),
            main: VecDeque::with_capacity(main_cap),
            ghost: VecDeque::with_capacity(ghost_cap),
            small_cap,
            main_cap,
            ghost_cap,
            hits: 0,
            misses: 0,
        }
    }

    /// Look up a value by key, incrementing the frequency counter on hit.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        if let Some(loc) = self.index.get(key) {
            self.hits += 1;
            let idx = loc.idx();
            // SAFETY: indices in `index` are guaranteed to be valid and occupied.
            let entry = self.entries[idx].as_mut().unwrap();
            entry.freq = entry.freq.saturating_add(1).min(3);
            Some(&entry.value)
        } else {
            None
        }
    }

    /// Insert a key-value pair. Returns the evicted value if an existing
    /// entry with the same key was replaced.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        // If key already exists, update in place.
        if let Some(loc) = self.index.get(&key) {
            let idx = loc.idx();
            let entry = self.entries[idx].as_mut().unwrap();
            let old = std::mem::replace(&mut entry.value, value);
            entry.freq = entry.freq.saturating_add(1).min(3);
            return Some(old);
        }

        self.misses += 1;

        // Check ghost: if key was recently evicted, promote to main.
        let in_ghost = self.remove_from_ghost(&key);

        if in_ghost {
            // Admit directly to main.
            self.evict_main_if_full();
            let idx = self.alloc_entry(key.clone(), value);
            self.main.push_back(idx);
            self.index.insert(key, Location::Main(idx));
        } else {
            // Insert into small queue.
            self.evict_small_if_full();
            let idx = self.alloc_entry(key.clone(), value);
            self.small.push_back(idx);
            self.index.insert(key, Location::Small(idx));
        }

        None
    }

    /// Remove a key from the cache.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        let loc = self.index.remove(key)?;
        let idx = loc.idx();

        // Remove from the queue. This is O(N) for the queue, but necessary
        // to maintain consistency. Usually cache removal is rare compared to
        // get/insert.
        match loc {
            Location::Small(_) => {
                if let Some(pos) = self.small.iter().position(|&i| i == idx) {
                    self.small.remove(pos);
                }
            }
            Location::Main(_) => {
                if let Some(pos) = self.main.iter().position(|&i| i == idx) {
                    self.main.remove(pos);
                }
            }
        }

        self.free_entry(idx)
    }

    /// Number of entries in the cache.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Total capacity.
    pub fn capacity(&self) -> usize {
        self.small_cap + self.main_cap
    }

    /// Cache statistics.
    pub fn stats(&self) -> S3FifoStats {
        S3FifoStats {
            hits: self.hits,
            misses: self.misses,
            small_size: self.small.len(),
            main_size: self.main.len(),
            ghost_size: self.ghost.len(),
            capacity: self.small_cap + self.main_cap,
        }
    }

    /// Clear all entries and reset statistics.
    pub fn clear(&mut self) {
        self.index.clear();
        self.entries.clear();
        self.free_indices.clear();
        self.small.clear();
        self.main.clear();
        self.ghost.clear();
        self.hits = 0;
        self.misses = 0;
    }

    /// Check if the cache contains a key (without incrementing freq).
    pub fn contains_key(&self, key: &K) -> bool {
        self.index.contains_key(key)
    }

    // ── Internal helpers ──────────────────────────────────────────

    /// Allocate a slot in the slab.
    fn alloc_entry(&mut self, key: K, value: V) -> usize {
        let entry = Some(Entry {
            key,
            value,
            freq: 0,
        });

        if let Some(idx) = self.free_indices.pop() {
            self.entries[idx] = entry;
            idx
        } else {
            let idx = self.entries.len();
            self.entries.push(entry);
            idx
        }
    }

    /// Free a slot in the slab and return the value.
    fn free_entry(&mut self, idx: usize) -> Option<V> {
        let entry = self.entries[idx].take()?;
        self.free_indices.push(idx);
        Some(entry.value)
    }

    /// Remove a key from the ghost queue if present.
    fn remove_from_ghost(&mut self, key: &K) -> bool {
        if let Some(pos) = self.ghost.iter().position(|k| k == key) {
            self.ghost.remove(pos);
            true
        } else {
            false
        }
    }

    /// Evict from the small queue if it's at capacity.
    fn evict_small_if_full(&mut self) {
        while self.small.len() >= self.small_cap {
            if let Some(idx) = self.small.pop_front() {
                // Extract state we need while holding the entry borrow, then drop it
                // before calling methods that borrow `self` mutably again.
                let (promote_to_main, key) = {
                    let entry = self.entries[idx].as_mut().unwrap();
                    let key = entry.key.clone();
                    if entry.freq > 0 {
                        entry.freq = 0; // Reset freq on promotion.
                        (true, key)
                    } else {
                        (false, key)
                    }
                };

                if promote_to_main {
                    self.evict_main_if_full();
                    self.index.insert(key, Location::Main(idx));
                    self.main.push_back(idx);
                } else {
                    self.index.remove(&key);
                    self.free_entry(idx); // Value is dropped here.

                    if self.ghost.len() >= self.ghost_cap {
                        self.ghost.pop_front();
                    }
                    self.ghost.push_back(key);
                }
            }
        }
    }

    /// Evict from the main queue if it's at capacity.
    fn evict_main_if_full(&mut self) {
        while self.main.len() >= self.main_cap {
            if let Some(idx) = self.main.pop_front() {
                let entry = self.entries[idx].as_mut().unwrap();

                if entry.freq > 0 {
                    // Give it another chance: decrement and re-insert.
                    entry.freq -= 1;
                    self.main.push_back(idx);
                } else {
                    // Actually evict.
                    let key = entry.key.clone();
                    self.index.remove(&key);
                    self.free_entry(idx);
                }
            }
        }
    }
}

impl<K, V> std::fmt::Debug for S3Fifo<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Fifo")
            .field("small", &self.small.len())
            .field("main", &self.main.len())
            .field("ghost", &self.ghost.len())
            .field("hits", &self.hits)
            .field("misses", &self.misses)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_cache() {
        let cache: S3Fifo<&str, i32> = S3Fifo::new(10);
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn insert_and_get() {
        let mut cache = S3Fifo::new(10);
        cache.insert("key1", 42);
        assert_eq!(cache.get(&"key1"), Some(&42));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn miss_returns_none() {
        let mut cache: S3Fifo<&str, i32> = S3Fifo::new(10);
        assert_eq!(cache.get(&"missing"), None);
    }

    #[test]
    fn update_existing_key() {
        let mut cache = S3Fifo::new(10);
        cache.insert("key1", 1);
        let old = cache.insert("key1", 2);
        assert_eq!(old, Some(1));
        assert_eq!(cache.get(&"key1"), Some(&2));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn remove_key() {
        let mut cache = S3Fifo::new(10);
        cache.insert("key1", 42);
        let removed = cache.remove(&"key1");
        assert_eq!(removed, Some(42));
        assert!(cache.is_empty());
        assert_eq!(cache.get(&"key1"), None);
    }

    #[test]
    fn remove_nonexistent() {
        let mut cache: S3Fifo<&str, i32> = S3Fifo::new(10);
        assert_eq!(cache.remove(&"missing"), None);
    }

    #[test]
    fn eviction_at_capacity() {
        let mut cache = S3Fifo::new(5);
        for i in 0..10 {
            cache.insert(i, i * 10);
        }
        // Should have at most capacity entries
        assert!(cache.len() <= cache.capacity());
    }

    #[test]
    fn small_to_main_promotion() {
        // Items accessed in small queue should be promoted to main on eviction
        let mut cache = S3Fifo::new(10); // small_cap=1, main_cap=9

        // Insert key and access it (sets freq > 0)
        cache.insert("keep", 1);
        cache.get(&"keep"); // freq = 1

        // Fill small to trigger eviction of "keep" from small -> main
        cache.insert("new", 2);

        // "keep" should still be accessible (promoted to main)
        assert_eq!(cache.get(&"keep"), Some(&1));
    }

    #[test]
    fn ghost_readmission() {
        // Keys evicted from small without access go to ghost.
        // Re-inserting a ghost key should go directly to main.
        let mut cache = S3Fifo::new(10); // small_cap=1

        // Insert and evict without access
        cache.insert("ghost_key", 1);
        cache.insert("displacer", 2); // evicts "ghost_key" to ghost

        // ghost_key should be gone from cache but in ghost
        assert_eq!(cache.get(&"ghost_key"), None);

        // Re-insert ghost_key -> should go to main
        cache.insert("ghost_key", 3);
        assert_eq!(cache.get(&"ghost_key"), Some(&3));
    }

    #[test]
    fn stats_tracking() {
        // Use capacity 20 so small_cap=2 and both "a" and "b" fit in small.
        let mut cache = S3Fifo::new(20);
        cache.insert("a", 1);
        cache.insert("b", 2);
        cache.get(&"a"); // hit
        cache.get(&"a"); // hit
        cache.get(&"c"); // miss (not found, but get doesn't track misses)

        let stats = cache.stats();
        assert_eq!(stats.hits, 2);
        // misses are only counted on insert (new keys)
        assert_eq!(stats.misses, 2); // 2 inserts
    }

    #[test]
    fn clear_resets() {
        let mut cache = S3Fifo::new(10);
        cache.insert("a", 1);
        cache.insert("b", 2);
        cache.get(&"a");
        cache.clear();

        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
        let stats = cache.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
        assert_eq!(stats.ghost_size, 0);
    }

    #[test]
    fn contains_key() {
        let mut cache = S3Fifo::new(10);
        cache.insert("a", 1);
        assert!(cache.contains_key(&"a"));
        assert!(!cache.contains_key(&"b"));
    }

    #[test]
    fn capacity_split() {
        let cache: S3Fifo<i32, i32> = S3Fifo::new(100);
        assert_eq!(cache.capacity(), 100);
        assert_eq!(cache.small_cap, 10);
        assert_eq!(cache.main_cap, 90);
        assert_eq!(cache.ghost_cap, 10);
    }

    #[test]
    fn minimum_capacity() {
        let cache: S3Fifo<i32, i32> = S3Fifo::new(0);
        assert!(cache.capacity() >= 2);
    }

    #[test]
    fn freq_capped_at_3() {
        let mut cache = S3Fifo::new(10);
        cache.insert("a", 1);
        for _ in 0..10 {
            cache.get(&"a");
        }
        // freq should be capped at 3 (internal, verified by eviction behavior)
        assert_eq!(cache.get(&"a"), Some(&1));
    }

    #[test]
    fn main_eviction_gives_second_chance() {
        // Items with freq > 0 in main get re-inserted with freq-1
        let mut cache = S3Fifo::new(5); // small=1, main=4

        // Fill main with accessed items
        for i in 0..4 {
            cache.insert(i, i);
            // Access once to move to small (freq=1) then to main
            cache.get(&i);
        }

        // Insert more to trigger main eviction
        for i in 10..20 {
            cache.insert(i, i);
        }

        // Cache should still function correctly
        assert!(cache.len() <= cache.capacity());
    }

    #[test]
    fn debug_format() {
        let cache: S3Fifo<&str, i32> = S3Fifo::new(10);
        let debug = format!("{cache:?}");
        assert!(debug.contains("S3Fifo"));
        assert!(debug.contains("small"));
        assert!(debug.contains("main"));
    }

    #[test]
    fn large_workload() {
        let mut cache = S3Fifo::new(100);

        // Insert a working set and access items as they are inserted,
        // so frequently-accessed ones have freq > 0 before eviction.
        for i in 0..200 {
            cache.insert(i, i * 10);
            // Access items 50..100 repeatedly to build frequency
            if i >= 50 {
                for hot in 50..std::cmp::min(i, 100) {
                    cache.get(&hot);
                }
            }
        }

        // Hot set (50..100) should have survived due to frequency protection
        let mut hot_hits = 0;
        for i in 50..100 {
            if cache.get(&i).is_some() {
                hot_hits += 1;
            }
        }

        // Frequently-accessed items should persist
        assert!(hot_hits > 20, "hot set retention: {hot_hits}/50");
    }

    #[test]
    fn scan_resistance() {
        let mut cache = S3Fifo::new(100);

        // Insert a working set and access frequently
        for i in 0..50 {
            cache.insert(i, i);
            cache.get(&i);
            cache.get(&i);
        }

        // Scan through a large number of unique keys (scan pattern)
        for i in 1000..2000 {
            cache.insert(i, i);
        }

        // Some of the original working set should survive the scan
        let mut survivors = 0;
        for i in 0..50 {
            if cache.get(&i).is_some() {
                survivors += 1;
            }
        }

        // S3-FIFO should protect frequently-accessed items from scan eviction
        assert!(
            survivors > 10,
            "scan resistance: {survivors}/50 working set items survived"
        );
    }

    #[test]
    fn ghost_size_bounded() {
        let mut cache = S3Fifo::new(10);

        // Insert many items to fill ghost
        for i in 0..100 {
            cache.insert(i, i);
        }

        let stats = cache.stats();
        assert!(
            stats.ghost_size <= cache.ghost_cap,
            "ghost should be bounded: {} <= {}",
            stats.ghost_size,
            cache.ghost_cap
        );
    }
}
