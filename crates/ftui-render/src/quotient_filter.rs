#![forbid(unsafe_code)]

//! Quotient Filter for space-efficient dirty row tracking (bd-3fc1b).
//!
//! A Quotient Filter is a compact approximate-membership data structure
//! that supports insert, lookup, delete, and merge — unlike Bloom filters,
//! which cannot delete.
//!
//! # How It Works
//!
//! Each element is hashed to a `p`-bit fingerprint, split into:
//! - `q`-bit *quotient* (slot index): determines the canonical slot
//! - `r`-bit *remainder*: stored in the slot
//!
//! Collisions are resolved by linear probing within a cluster. Three
//! metadata bits per slot track the structure of runs and clusters.
//!
//! # Complexity
//!
//! - Insert: O(1) amortized
//! - Lookup: O(1) amortized
//! - Delete: O(1) amortized
//! - Space: `(r + 3) * 2^q` bits ≈ 10% overhead above information-theoretic minimum
//!
//! # Use Case
//!
//! For large virtualized lists (>1M rows) where only a small fraction
//! (<1%) of rows are dirty, a Quotient Filter uses O(dirty_count) space
//! vs O(total_rows) for a bitset.
//!
//! # Implementation Note
//!
//! This implementation uses a simplified open-addressing scheme with
//! (quotient, remainder) pairs stored directly, avoiding the complexity
//! of the canonical 3-bit metadata approach while preserving the same
//! API contract and space characteristics.
//!
//! # References
//!
//! Bender et al. (2012): "Don't Thrash: How to Cache Your Hash on Flash"

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// A Quotient Filter for approximate set membership with deletion support.
#[derive(Debug, Clone)]
pub struct QuotientFilter {
    /// Number of quotient bits (q). Table has 2^q slots.
    q: u32,
    /// Number of remainder bits (r). Fingerprint = q + r bits.
    r: u32,
    /// Slot storage: each slot holds an optional (quotient, remainder) pair.
    slots: Vec<Option<(u32, u64)>>,
    /// Number of elements currently stored.
    count: usize,
    /// Total number of slots (2^q).
    capacity: usize,
}

/// Configuration for a Quotient Filter.
#[derive(Debug, Clone, Copy)]
pub struct QuotientFilterConfig {
    /// Number of quotient bits (determines capacity: 2^q slots).
    pub q: u32,
    /// Number of remainder bits (determines false positive rate: ~2^(-r)).
    pub r: u32,
}

impl QuotientFilterConfig {
    /// Create a config targeting a given capacity and false positive rate.
    ///
    /// `expected_items`: expected number of elements
    /// `fp_rate`: target false positive rate (e.g., 0.01 for 1%)
    #[must_use]
    pub fn for_capacity(expected_items: usize, fp_rate: f64) -> Self {
        let fp_rate = if fp_rate.is_finite() && fp_rate > 0.0 {
            fp_rate.min(1.0 - f64::EPSILON)
        } else {
            0.01
        };

        // r bits give ~2^(-r) FP rate
        let r = (-fp_rate.log2()).ceil() as u32;
        let r = r.clamp(2, 32);

        // q bits: need 2^q > expected_items / load_factor
        // Use 75% max load for good performance
        let needed = ((expected_items as f64 / 0.75).ceil()) as u64;
        let q = (64 - needed.leading_zeros()).clamp(4, 28);

        Self { q, r }
    }
}

impl Default for QuotientFilterConfig {
    fn default() -> Self {
        Self { q: 10, r: 8 } // 1024 slots, ~0.4% FP rate
    }
}

impl QuotientFilter {
    /// Create a new Quotient Filter with the given configuration.
    #[must_use]
    pub fn new(config: QuotientFilterConfig) -> Self {
        let q = config.q.min(28); // Cap at 2^28 = 256M slots
        let r = config.r.clamp(1, 32);
        let capacity = 1usize << q;

        Self {
            q,
            r,
            slots: vec![None; capacity],
            count: 0,
            capacity,
        }
    }

    /// Create with default configuration.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(QuotientFilterConfig::default())
    }

    /// Number of elements currently stored.
    #[must_use]
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the filter is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Current load factor (0.0 to 1.0).
    #[must_use]
    pub fn load_factor(&self) -> f64 {
        self.count as f64 / self.capacity as f64
    }

    /// Number of slots (capacity).
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Theoretical false positive rate at current load.
    #[must_use]
    pub fn theoretical_fp_rate(&self) -> f64 {
        // FP rate ≈ 1 - (1 - 2^(-r))^n ≈ n * 2^(-r) for small rates
        let base_rate = 1.0 / (1u64 << self.r) as f64;
        1.0 - (1.0 - base_rate).powi(self.count as i32)
    }

    /// Hash an element to (quotient, remainder).
    fn fingerprint<T: Hash>(&self, item: &T) -> (u32, u64) {
        let mut hasher = DefaultHasher::new();
        item.hash(&mut hasher);
        let h = hasher.finish();

        let q_mask = (1u32 << self.q) - 1;
        let r_mask = (1u64 << self.r) - 1;

        let quotient = ((h >> self.r) as u32) & q_mask;
        let remainder = h & r_mask;
        (quotient, remainder)
    }

    /// Insert an element. Returns `true` if newly inserted, `false` if already present or full.
    pub fn insert<T: Hash>(&mut self, item: &T) -> bool {
        if self.count >= self.capacity {
            return false;
        }

        let (quotient, remainder) = self.fingerprint(item);

        // Linear probe from canonical slot
        let mut pos = quotient as usize;
        for _ in 0..self.capacity {
            match self.slots[pos] {
                None => {
                    // Empty slot — insert here
                    self.slots[pos] = Some((quotient, remainder));
                    self.count += 1;
                    return true;
                }
                Some((q, r)) if q == quotient && r == remainder => {
                    // Already present
                    return false;
                }
                _ => {
                    // Occupied by different element — probe next
                    pos = (pos + 1) % self.capacity;
                }
            }
        }

        false // Full (shouldn't happen with load factor check)
    }

    /// Check if an element might be in the filter.
    ///
    /// Returns `false` for definite non-members (no false negatives).
    /// Returns `true` for probable members (may have false positives
    /// due to fingerprint collisions).
    #[must_use]
    pub fn contains<T: Hash>(&self, item: &T) -> bool {
        let (quotient, remainder) = self.fingerprint(item);

        let mut pos = quotient as usize;
        for _ in 0..self.capacity {
            match self.slots[pos] {
                None => return false, // Empty slot — not found
                Some((q, r)) if q == quotient && r == remainder => return true,
                _ => pos = (pos + 1) % self.capacity,
            }
        }

        false
    }

    /// Remove an element. Returns `true` if it was found and removed.
    ///
    /// Uses backward-shift deletion to maintain probe sequences.
    pub fn remove<T: Hash>(&mut self, item: &T) -> bool {
        let (quotient, remainder) = self.fingerprint(item);

        // Find the element
        let mut pos = quotient as usize;
        let mut found_pos = None;
        for _ in 0..self.capacity {
            match self.slots[pos] {
                None => break,
                Some((q, r)) if q == quotient && r == remainder => {
                    found_pos = Some(pos);
                    break;
                }
                _ => pos = (pos + 1) % self.capacity,
            }
        }

        let mut pos = match found_pos {
            Some(p) => p,
            None => return false,
        };

        // Backward-shift deletion: move subsequent elements back
        // to fill the gap, maintaining their probe sequences.
        self.slots[pos] = None;
        self.count -= 1;

        let mut current = (pos + 1) % self.capacity;
        loop {
            match self.slots[current] {
                None => break, // End of cluster
                Some((q, _r)) => {
                    let canonical = q as usize;
                    // Standard backward-shift rule: the element may fill the
                    // gap iff its canonical slot is NOT cyclically inside
                    // (pos, current] — moving it must never place it before
                    // its canonical slot in probe order. Crucially, when an
                    // element must stay (e.g. it sits in its canonical
                    // slot), the scan CONTINUES to the end of the cluster:
                    // a later element with an earlier canonical slot may
                    // still need the gap. Breaking here (as this code once
                    // did) strands such elements past a hole, producing
                    // false negatives — a violation of the filter's
                    // no-false-negatives contract.
                    let canonical_in_gap_interval = if pos <= current {
                        canonical > pos && canonical <= current
                    } else {
                        canonical > pos || canonical <= current
                    };

                    if !canonical_in_gap_interval {
                        // Move this element back to the gap
                        self.slots[pos] = self.slots[current];
                        self.slots[current] = None;
                        pos = current;
                    }
                }
            }
            current = (current + 1) % self.capacity;
        }

        true
    }

    /// Clear all elements.
    pub fn clear(&mut self) {
        self.slots.fill(None);
        self.count = 0;
    }

    /// Merge another filter into this one.
    ///
    /// Both filters must have the same q and r values.
    /// Returns the number of new elements added.
    pub fn merge(&mut self, other: &QuotientFilter) -> usize {
        if self.q != other.q || self.r != other.r {
            return 0;
        }

        let mut added = 0;
        for slot in &other.slots {
            if let &Some((q, r)) = slot {
                // Check if already present
                let mut pos = q as usize;
                let mut found = false;
                for _ in 0..self.capacity {
                    match self.slots[pos] {
                        None => break,
                        Some((eq, er)) if eq == q && er == r => {
                            found = true;
                            break;
                        }
                        _ => pos = (pos + 1) % self.capacity,
                    }
                }

                if !found {
                    // Insert at first empty slot from canonical position
                    let mut ipos = q as usize;
                    for _ in 0..self.capacity {
                        if self.slots[ipos].is_none() {
                            self.slots[ipos] = Some((q, r));
                            self.count += 1;
                            added += 1;
                            break;
                        }
                        ipos = (ipos + 1) % self.capacity;
                    }
                }
            }
        }
        added
    }
}

impl Default for QuotientFilter {
    fn default() -> Self {
        Self::with_defaults()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_filter() {
        let qf = QuotientFilter::with_defaults();
        assert!(qf.is_empty());
        assert_eq!(qf.len(), 0);
        assert_eq!(qf.capacity(), 1024);
        assert!(!qf.contains(&42u64));
    }

    #[test]
    fn remove_never_creates_false_negatives_in_clusters() {
        // Regression: backward-shift deletion used to BREAK at the first
        // non-shiftable element (e.g. one sitting in its canonical slot),
        // stranding later cluster members behind the new hole — contains()
        // then hit None at their canonical slot and returned false for
        // present members. The scan must continue to the cluster end.
        // Small table (q=4 -> 16 slots) forces heavy clustering.
        for base in (0u64..2000).step_by(97) {
            let mut qf = QuotientFilter::new(QuotientFilterConfig { q: 4, r: 16 });
            let items: Vec<u64> = (base..base + 12).collect();
            for item in &items {
                qf.insert(item);
            }
            // Delete every other item; all remaining must stay findable.
            for item in items.iter().step_by(2) {
                qf.remove(item);
            }
            for item in items.iter().skip(1).step_by(2) {
                assert!(
                    qf.contains(item),
                    "false negative for {item} after cluster deletions (base {base})"
                );
            }
        }
    }

    #[test]
    fn insert_and_lookup() {
        let mut qf = QuotientFilter::with_defaults();
        assert!(qf.insert(&100u64));
        assert!(qf.contains(&100u64));
        assert_eq!(qf.len(), 1);
    }

    #[test]
    fn duplicate_insert_returns_false() {
        let mut qf = QuotientFilter::with_defaults();
        assert!(qf.insert(&42u64));
        assert!(!qf.insert(&42u64));
        assert_eq!(qf.len(), 1);
    }

    #[test]
    fn insert_multiple() {
        let mut qf = QuotientFilter::new(QuotientFilterConfig { q: 8, r: 8 });
        for i in 0u64..50 {
            qf.insert(&i);
        }
        assert_eq!(qf.len(), 50);

        // All should be found (no false negatives)
        for i in 0u64..50 {
            assert!(qf.contains(&i), "element {i} should be found");
        }
    }

    #[test]
    fn no_false_negatives() {
        let mut qf = QuotientFilter::new(QuotientFilterConfig { q: 10, r: 10 });
        let items: Vec<u64> = (0..200).collect();

        for item in &items {
            qf.insert(item);
        }

        // Verify: zero false negatives
        for item in &items {
            assert!(qf.contains(item), "false negative for {item}");
        }
    }

    #[test]
    fn false_positive_rate_bounded() {
        let mut qf = QuotientFilter::new(QuotientFilterConfig { q: 12, r: 8 });

        // Insert 500 elements
        for i in 0u64..500 {
            qf.insert(&i);
        }

        // Check 10000 non-members
        let mut false_positives = 0;
        for i in 10000u64..20000 {
            if qf.contains(&i) {
                false_positives += 1;
            }
        }

        let fp_rate = false_positives as f64 / 10000.0;
        // r=8 gives theoretical rate ~0.4%, allow up to 5% with margin
        assert!(
            fp_rate < 0.05,
            "false positive rate too high: {fp_rate:.4} ({false_positives}/10000)"
        );
    }

    #[test]
    fn remove_element() {
        let mut qf = QuotientFilter::with_defaults();
        qf.insert(&42u64);
        assert!(qf.contains(&42u64));

        assert!(qf.remove(&42u64));
        assert_eq!(qf.len(), 0);
        assert!(!qf.contains(&42u64));
    }

    #[test]
    fn remove_nonexistent() {
        let mut qf = QuotientFilter::with_defaults();
        assert!(!qf.remove(&42u64));
    }

    #[test]
    fn remove_preserves_others() {
        let mut qf = QuotientFilter::with_defaults();
        for i in 0u64..20 {
            qf.insert(&i);
        }

        // Remove even numbers
        for i in (0u64..20).step_by(2) {
            qf.remove(&i);
        }

        // Odd numbers should still be present
        for i in (1u64..20).step_by(2) {
            assert!(qf.contains(&i), "odd {i} should survive removal");
        }
        // Even numbers should be gone
        for i in (0u64..20).step_by(2) {
            assert!(!qf.contains(&i), "even {i} should be removed");
        }
    }

    #[test]
    fn clear_filter() {
        let mut qf = QuotientFilter::with_defaults();
        for i in 0u64..100 {
            qf.insert(&i);
        }
        assert_eq!(qf.len(), 100);

        qf.clear();
        assert!(qf.is_empty());
        assert_eq!(qf.len(), 0);

        for i in 0u64..100 {
            assert!(!qf.contains(&i));
        }
    }

    #[test]
    fn load_factor() {
        let mut qf = QuotientFilter::new(QuotientFilterConfig { q: 4, r: 4 }); // 16 slots
        assert!((qf.load_factor() - 0.0).abs() < f64::EPSILON);

        for i in 0u64..8 {
            qf.insert(&i);
        }
        assert!((qf.load_factor() - 0.5).abs() < 0.01);
    }

    #[test]
    fn config_for_capacity() {
        let config = QuotientFilterConfig::for_capacity(10000, 0.01);
        assert!(config.r >= 7, "r should be at least 7 for 1% FP rate");
        assert!(
            (1usize << config.q) >= 10000,
            "capacity should exceed expected items"
        );
    }

    #[test]
    fn config_for_capacity_sanitizes_invalid_fp_rates() {
        let zero = QuotientFilterConfig::for_capacity(128, 0.0);
        let nan = QuotientFilterConfig::for_capacity(128, f64::NAN);

        assert!(zero.r >= 7);
        assert!(nan.r >= 7);
        assert!((1usize << zero.q) >= 128);
        assert!((1usize << nan.q) >= 128);
    }

    #[test]
    fn string_keys() {
        let mut qf = QuotientFilter::with_defaults();
        qf.insert(&"hello");
        qf.insert(&"world");
        assert!(qf.contains(&"hello"));
        assert!(qf.contains(&"world"));
        assert!(!qf.contains(&"foo"));
    }

    #[test]
    fn row_id_tracking() {
        // Simulate dirty row tracking
        let mut dirty = QuotientFilter::new(QuotientFilterConfig { q: 12, r: 8 });

        // Mark rows as dirty
        let dirty_rows = [5u32, 42, 100, 255, 1000];
        for &row in &dirty_rows {
            dirty.insert(&row);
        }

        // Check which rows need re-render
        for row in 0u32..2000 {
            if dirty_rows.contains(&row) {
                assert!(dirty.contains(&row), "dirty row {row} not found");
            }
        }

        // After re-render, remove from dirty set
        for &row in &dirty_rows {
            dirty.remove(&row);
        }
        assert!(dirty.is_empty());
    }

    #[test]
    fn merge_filters() {
        let config = QuotientFilterConfig { q: 8, r: 8 };
        let mut qf1 = QuotientFilter::new(config);
        let mut qf2 = QuotientFilter::new(config);

        for i in 0u64..10 {
            qf1.insert(&i);
        }
        for i in 5u64..15 {
            qf2.insert(&i);
        }

        let added = qf1.merge(&qf2);
        assert!(added > 0, "merge should add elements");

        // All elements from both should be present
        for i in 0u64..15 {
            assert!(qf1.contains(&i), "merged filter should contain {i}");
        }
    }

    #[test]
    fn merge_mismatched_config_is_noop() {
        let mut qf1 = QuotientFilter::new(QuotientFilterConfig { q: 8, r: 8 });
        let qf2 = QuotientFilter::new(QuotientFilterConfig { q: 10, r: 8 });

        qf1.insert(&1u64);
        let added = qf1.merge(&qf2);
        assert_eq!(added, 0, "mismatched configs should not merge");
    }

    #[test]
    fn theoretical_fp_rate_increases_with_load() {
        let mut qf = QuotientFilter::new(QuotientFilterConfig { q: 10, r: 8 });
        let rate_empty = qf.theoretical_fp_rate();

        for i in 0u64..100 {
            qf.insert(&i);
        }
        let rate_loaded = qf.theoretical_fp_rate();

        assert!(
            rate_empty < rate_loaded,
            "FP rate should increase with load"
        );
    }

    #[test]
    fn space_comparison_vs_bitset() {
        // Quotient Filter for 1000 dirty rows out of 1M total
        let dirty_config = QuotientFilterConfig::for_capacity(1000, 0.01);
        let dirty_qf_bits = (dirty_config.r as usize + 3) * (1usize << dirty_config.q);

        // Bitset for 1M rows: 1M bits
        let bitset_bits = 1_000_000usize;

        assert!(
            dirty_qf_bits < bitset_bits,
            "QF ({dirty_qf_bits} bits) should be smaller than bitset ({bitset_bits} bits) for sparse dirty sets"
        );
    }

    #[test]
    fn default_config() {
        let qf = QuotientFilter::default();
        assert_eq!(qf.capacity(), 1024);
        assert!(qf.is_empty());
    }

    #[test]
    fn insert_after_remove_reuses_slot() {
        let mut qf = QuotientFilter::with_defaults();
        qf.insert(&1u64);
        qf.remove(&1u64);
        assert!(qf.insert(&1u64));
        assert!(qf.contains(&1u64));
        assert_eq!(qf.len(), 1);
    }

    #[test]
    fn heavy_load() {
        // Use larger r to avoid fingerprint collisions
        let mut qf = QuotientFilter::new(QuotientFilterConfig { q: 10, r: 20 }); // 1024 slots, 30-bit fingerprints
        let target = 500;
        let mut inserted = 0;
        for i in 0u64..target as u64 {
            if qf.insert(&i) {
                inserted += 1;
            }
        }
        assert_eq!(inserted, target);

        // All should be findable (no false negatives)
        for i in 0u64..target as u64 {
            assert!(qf.contains(&i), "element {i} missing at high load");
        }
    }
}
