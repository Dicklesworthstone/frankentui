//! Cache-friendly Fenwick tree (Binary Indexed Tree) for prefix sums.
//!
//! Provides O(log n) point update and prefix query over a contiguous `Vec<u32>`,
//! with a batch update API that amortises multiple deltas into a single pass.
//!
//! # Layout
//!
//! The tree is stored 1-indexed in a contiguous `Vec<u32>` of length `n + 1`
//! (index 0 unused). This gives cache-friendly sequential access and avoids
//! indirection. For a typical 100k-item list with `u32` heights, the tree
//! occupies ~400 KB — well within L2 cache on modern CPUs.
//!
//! # Operations
//!
//! | Operation | Time | Allocations |
//! |-----------|------|-------------|
//! | `new(n)` | O(n) | 1 Vec |
//! | `update(i, delta)` | O(log n) | 0 |
//! | `prefix(i)` | O(log n) | 0 |
//! | `range(l, r)` | O(log n) | 0 |
//! | `batch_update(deltas)` | O(m log n) | 0 |
//! | `rebuild(values)` | O(n) | 0 |
//! | `find_prefix(target)` | O(log n) | 0 |
//!
//! # Invariants
//!
//! 1. `tree[i]` stores the sum of elements in a range determined by `lowbit(i)`.
//! 2. `prefix(n) == sum of all values`.
//! 3. After `rebuild`, the tree exactly represents the given values.
//! 4. `batch_update` produces identical results to sequential `update` calls.

/// Fenwick tree (Binary Indexed Tree) for prefix sum queries over `u32` values.
///
/// Designed for virtualized list height layout: each entry `i` stores the height
/// of item `i`, and `prefix(i)` gives the y-offset of item `i+1`.
#[derive(Debug, Clone)]
pub struct FenwickTree {
    /// 1-indexed tree storage. `tree[0]` is unused.
    tree: Vec<u32>,
    /// Number of elements (not including index 0).
    n: usize,
}

impl FenwickTree {
    /// Create a Fenwick tree of size `n` initialised to all zeros.
    pub fn new(n: usize) -> Self {
        Self {
            tree: vec![0u32; n + 1],
            n,
        }
    }

    /// Create a Fenwick tree from an initial array of values in O(n).
    ///
    /// This is faster than calling `update` n times (which would be O(n log n)).
    pub fn from_values(values: &[u32]) -> Self {
        let n = values.len();
        let mut tree = vec![0u32; n + 1];

        // Copy values into 1-indexed positions.
        for (i, &v) in values.iter().enumerate() {
            tree[i + 1] = v;
        }

        // Build tree in O(n) using the parent-propagation trick.
        for i in 1..=n {
            let parent = i + lowbit(i);
            if parent <= n {
                tree[parent] = tree[parent].wrapping_add(tree[i]);
            }
        }

        Self { tree, n }
    }

    /// Number of elements.
    #[inline]
    pub fn len(&self) -> usize {
        self.n
    }

    /// Whether the tree is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Add `delta` to element at position `i` (0-indexed). O(log n), zero alloc.
    ///
    /// # Panics
    /// Panics if `i >= n`.
    pub fn update(&mut self, i: usize, delta: i32) {
        // Cast to u32 directly: two's complement makes wrapping_add correct
        // for both positive and negative deltas (e.g. -5i32 as u32 = u32::MAX-4,
        // and wrapping_add with that is equivalent to wrapping_sub(5)).
        // This also avoids a panic on `-i32::MIN` which the old negation had.
        self.update_u32(i, delta as u32);
    }

    /// Set element at position `i` to `value` (0-indexed). O(log n).
    pub fn set(&mut self, i: usize, value: u32) {
        let current = self.get(i);
        // Use wrapping_sub in u32 space to avoid i64→i32 truncation
        // for large deltas that don't fit in i32 range.
        self.update_u32(i, value.wrapping_sub(current));
    }

    /// Internal: add a u32 delta to position `i` using wrapping arithmetic.
    fn update_u32(&mut self, i: usize, delta: u32) {
        assert!(i < self.n, "index {i} out of bounds (n={})", self.n);
        let mut idx = i + 1; // convert to 1-indexed
        while idx <= self.n {
            self.tree[idx] = self.tree[idx].wrapping_add(delta);
            idx += lowbit(idx);
        }
    }

    /// Get the value at position `i` (0-indexed). O(log n).
    ///
    /// Computed as `prefix(i) - prefix(i-1)`.
    pub fn get(&self, i: usize) -> u32 {
        if i == 0 {
            self.prefix(0)
        } else {
            self.prefix(i).wrapping_sub(self.prefix(i - 1))
        }
    }

    /// Prefix sum of elements [0..=i] (0-indexed). O(log n), zero alloc.
    ///
    /// # Panics
    /// Panics if `i >= n`.
    pub fn prefix(&self, i: usize) -> u32 {
        assert!(i < self.n, "index {i} out of bounds (n={})", self.n);
        let mut sum = 0u32;
        let mut idx = i + 1; // convert to 1-indexed
        while idx > 0 {
            sum = sum.wrapping_add(self.tree[idx]);
            idx -= lowbit(idx);
        }
        sum
    }

    /// Range sum of elements [left..=right] (0-indexed). O(log n).
    ///
    /// # Panics
    /// Panics if `left > right` or `right >= n`.
    pub fn range(&self, left: usize, right: usize) -> u32 {
        assert!(left <= right, "left {left} > right {right}");
        if left == 0 {
            self.prefix(right)
        } else {
            self.prefix(right).wrapping_sub(self.prefix(left - 1))
        }
    }

    /// Total sum of all elements. O(log n).
    pub fn total(&self) -> u32 {
        if self.n == 0 {
            0
        } else {
            self.prefix(self.n - 1)
        }
    }

    /// Apply multiple updates in a single pass. O(m log n), zero alloc.
    ///
    /// Each `(index, delta)` pair is applied sequentially. This produces
    /// identical results to calling `update` for each pair individually.
    pub fn batch_update(&mut self, deltas: &[(usize, i32)]) {
        for &(i, delta) in deltas {
            self.update(i, delta);
        }
    }

    /// Rebuild the tree from a fresh array of values in O(n).
    ///
    /// Requires `values.len() == self.n`.
    ///
    /// # Panics
    /// Panics if `values.len() != self.n`.
    pub fn rebuild(&mut self, values: &[u32]) {
        assert_eq!(values.len(), self.n, "rebuild size mismatch");

        // Zero out.
        self.tree.fill(0);

        // Copy into 1-indexed positions.
        for (i, &v) in values.iter().enumerate() {
            self.tree[i + 1] = v;
        }

        // Parent propagation in O(n).
        for i in 1..=self.n {
            let parent = i + lowbit(i);
            if parent <= self.n {
                self.tree[parent] = self.tree[parent].wrapping_add(self.tree[i]);
            }
        }
    }

    /// Find the largest index `i` such that `prefix(i) <= target`.
    ///
    /// Returns `None` if all prefix sums exceed `target` (i.e., `values[0] > target`).
    /// This is useful for binary-search-by-offset in virtualized lists.
    /// O(log n), zero alloc.
    pub fn find_prefix(&self, target: u32) -> Option<usize> {
        if self.n == 0 {
            return None;
        }

        let mut pos = 0usize;
        let mut remaining = target;
        let mut bit_mask = most_significant_bit(self.n);

        while bit_mask > 0 {
            let next = pos + bit_mask;
            if next <= self.n && self.tree[next] <= remaining {
                remaining -= self.tree[next];
                pos = next;
            }
            bit_mask >>= 1;
        }

        if pos == 0 && self.tree.get(1).copied().unwrap_or(u32::MAX) > target {
            None
        } else {
            Some(pos.saturating_sub(1)) // convert back to 0-indexed
        }
    }

    /// Resize the tree. If growing, new elements are initialised to 0.
    /// If shrinking, excess elements are dropped. O(new_n).
    pub fn resize(&mut self, new_n: usize) {
        if new_n == self.n {
            return;
        }
        // Extract current values, resize, rebuild.
        let mut values: Vec<u32> = (0..self.n).map(|i| self.get(i)).collect();
        values.resize(new_n, 0);
        self.n = new_n;
        self.tree = vec![0u32; new_n + 1];
        self.rebuild(&values);
    }
}

/// Lowest set bit of `x`. E.g., `lowbit(6) = 2`, `lowbit(4) = 4`.
#[inline]
fn lowbit(x: usize) -> usize {
    x & x.wrapping_neg()
}

/// Most significant bit that fits within `n`.
#[inline]
fn most_significant_bit(n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    1 << (usize::BITS - 1 - n.leading_zeros())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Basic construction ───────────────────────────────────────

    #[test]
    fn new_creates_zeroed_tree() {
        let ft = FenwickTree::new(10);
        assert_eq!(ft.len(), 10);
        assert!(!ft.is_empty());
        assert_eq!(ft.total(), 0);
    }

    #[test]
    fn empty_tree() {
        let ft = FenwickTree::new(0);
        assert!(ft.is_empty());
        assert_eq!(ft.total(), 0);
    }

    #[test]
    fn from_values_matches_sequential() {
        let values = vec![3, 1, 4, 1, 5, 9, 2, 6];
        let ft = FenwickTree::from_values(&values);

        // Check prefix sums.
        assert_eq!(ft.prefix(0), 3);
        assert_eq!(ft.prefix(1), 4);
        assert_eq!(ft.prefix(2), 8);
        assert_eq!(ft.prefix(7), 31);
        assert_eq!(ft.total(), 31);
    }

    // ─── Point operations ─────────────────────────────────────────

    #[test]
    fn update_and_query() {
        let mut ft = FenwickTree::new(5);
        ft.update(0, 10);
        ft.update(2, 20);
        ft.update(4, 30);

        assert_eq!(ft.prefix(0), 10);
        assert_eq!(ft.prefix(2), 30);
        assert_eq!(ft.prefix(4), 60);
        assert_eq!(ft.total(), 60);
    }

    #[test]
    fn set_overwrites_value() {
        let mut ft = FenwickTree::from_values(&[5, 10, 15]);
        ft.set(1, 20);
        assert_eq!(ft.get(0), 5);
        assert_eq!(ft.get(1), 20);
        assert_eq!(ft.get(2), 15);
        assert_eq!(ft.total(), 40);
    }

    #[test]
    fn get_retrieves_individual_values() {
        let values = vec![7, 3, 8, 2, 6];
        let ft = FenwickTree::from_values(&values);
        for (i, &v) in values.iter().enumerate() {
            assert_eq!(ft.get(i), v, "mismatch at index {i}");
        }
    }

    // ─── Range queries ────────────────────────────────────────────

    #[test]
    fn range_sum() {
        let ft = FenwickTree::from_values(&[1, 2, 3, 4, 5]);
        assert_eq!(ft.range(0, 4), 15);
        assert_eq!(ft.range(1, 3), 9);
        assert_eq!(ft.range(2, 2), 3);
        assert_eq!(ft.range(0, 0), 1);
    }

    // ─── Batch update ─────────────────────────────────────────────

    #[test]
    fn unit_batch_update_equivalence() {
        let values = vec![10, 20, 30, 40, 50];
        let deltas = vec![(0, 5), (2, -3), (4, 10), (1, 7)];

        // Sequential.
        let mut ft_seq = FenwickTree::from_values(&values);
        for &(i, d) in &deltas {
            ft_seq.update(i, d);
        }

        // Batch.
        let mut ft_batch = FenwickTree::from_values(&values);
        ft_batch.batch_update(&deltas);

        // Must match.
        for i in 0..5 {
            assert_eq!(ft_seq.get(i), ft_batch.get(i), "mismatch at index {i}");
        }
    }

    // ─── Rebuild ──────────────────────────────────────────────────

    #[test]
    fn rebuild_matches_from_values() {
        let v1 = vec![1, 2, 3, 4, 5];
        let v2 = vec![10, 20, 30, 40, 50];

        let ft1 = FenwickTree::from_values(&v2);
        let mut ft2 = FenwickTree::from_values(&v1);
        ft2.rebuild(&v2);

        for i in 0..5 {
            assert_eq!(ft1.get(i), ft2.get(i));
        }
    }

    // ─── find_prefix ──────────────────────────────────────────────

    #[test]
    fn find_prefix_scroll_offset() {
        // Heights: [20, 30, 10, 40, 25]
        // Prefix sums: [20, 50, 60, 100, 125]
        let ft = FenwickTree::from_values(&[20, 30, 10, 40, 25]);

        // Target 0: first item's offset starts at 0, so prefix(0)=20 > 0 → None?
        // Actually, find_prefix finds largest i where prefix(i) <= target.
        // prefix(0) = 20 > 0, so no valid i.
        assert_eq!(ft.find_prefix(0), None);

        // Target 20: prefix(0)=20 ≤ 20. prefix(1)=50 > 20. → i=0.
        assert_eq!(ft.find_prefix(20), Some(0));

        // Target 50: prefix(1)=50 ≤ 50. prefix(2)=60 > 50. → i=1.
        assert_eq!(ft.find_prefix(50), Some(1));

        // Target 99: prefix(2)=60 ≤ 99. prefix(3)=100 > 99. → i=2.
        assert_eq!(ft.find_prefix(99), Some(2));

        // Target 125: prefix(4)=125 ≤ 125 → i=4.
        assert_eq!(ft.find_prefix(125), Some(4));
    }

    // ─── Resize ───────────────────────────────────────────────────

    #[test]
    fn resize_grow_preserves() {
        let mut ft = FenwickTree::from_values(&[1, 2, 3]);
        ft.resize(5);
        assert_eq!(ft.len(), 5);
        assert_eq!(ft.get(0), 1);
        assert_eq!(ft.get(1), 2);
        assert_eq!(ft.get(2), 3);
        assert_eq!(ft.get(3), 0);
        assert_eq!(ft.get(4), 0);
    }

    #[test]
    fn resize_shrink_drops() {
        let mut ft = FenwickTree::from_values(&[1, 2, 3, 4, 5]);
        ft.resize(3);
        assert_eq!(ft.len(), 3);
        assert_eq!(ft.total(), 6); // 1+2+3
    }

    // ─── Property: prefix sum correctness ─────────────────────────

    #[test]
    fn property_prefix_sum_correct() {
        // Deterministic PRNG for random updates.
        let mut seed: u64 = 0xCAFE_BABE_0000_0001;
        let n = 100;
        let mut naive = vec![0u32; n];
        let mut ft = FenwickTree::new(n);

        for _ in 0..500 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let idx = (seed >> 33) as usize % n;
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let delta = ((seed >> 33) as i32 % 100).abs();

            naive[idx] = naive[idx].wrapping_add(delta as u32);
            ft.update(idx, delta);
        }

        // Verify all prefix sums match naive computation.
        let mut naive_prefix = 0u32;
        for (i, value) in naive.iter().enumerate() {
            naive_prefix = naive_prefix.wrapping_add(*value);
            assert_eq!(ft.prefix(i), naive_prefix, "prefix mismatch at index {i}");
        }
    }

    // ─── Edge cases ───────────────────────────────────────────────

    #[test]
    fn update_i32_min_does_not_panic() {
        // i32::MIN previously caused a panic via `-i32::MIN` overflow.
        let mut ft = FenwickTree::from_values(&[0, 0, 0]);
        ft.update(0, i32::MIN); // should not panic
        // wrapping semantics: 0u32.wrapping_add(i32::MIN as u32) = 2147483648
        assert_eq!(ft.get(0), i32::MIN as u32);
    }

    #[test]
    fn set_large_u32_value() {
        // set() previously truncated delta via i64→i32 cast for large values.
        let mut ft = FenwickTree::from_values(&[0, 100, 200]);
        ft.set(0, u32::MAX);
        assert_eq!(ft.get(0), u32::MAX);
        assert_eq!(ft.get(1), 100);
        assert_eq!(ft.get(2), 200);
    }

    // ─── Performance test ─────────────────────────────────────────

    #[test]
    fn perf_fenwick_hotpath() {
        let n = 100_000;
        let values: Vec<u32> = (0..n).map(|i| (i % 50 + 1) as u32).collect();

        // Build.
        let start = std::time::Instant::now();
        let mut ft = FenwickTree::from_values(&values);
        let build_time = start.elapsed();

        // 10k point updates.
        let start = std::time::Instant::now();
        for i in 0..10_000 {
            ft.update(i * 10, 5);
        }
        let update_time = start.elapsed();

        // 10k prefix queries.
        let start = std::time::Instant::now();
        let mut _sink = 0u32;
        for i in 0..10_000 {
            _sink = _sink.wrapping_add(ft.prefix(i * 10));
        }
        let query_time = start.elapsed();

        // Batch update: 10k deltas.
        let deltas: Vec<(usize, i32)> = (0..10_000).map(|i| (i * 10, 3)).collect();
        let start = std::time::Instant::now();
        ft.batch_update(&deltas);
        let batch_time = start.elapsed();

        // Log results.
        eprintln!("=== Fenwick Tree Performance (n={n}) ===");
        eprintln!("Build (from_values):  {:?}", build_time);
        eprintln!("10k point updates:    {:?}", update_time);
        eprintln!("10k prefix queries:   {:?}", query_time);
        eprintln!("10k batch updates:    {:?}", batch_time);
        eprintln!("Query p50 (approx):   {:?}", query_time / 10_000);

        // Budget assertions.
        assert!(
            query_time < std::time::Duration::from_millis(50),
            "10k queries too slow: {query_time:?}"
        );
        assert!(
            build_time < std::time::Duration::from_millis(100),
            "build too slow: {build_time:?}"
        );
    }

    // ─── Helpers ──────────────────────────────────────────────────

    // ─── Edge-case tests (bd-1i1vn) ────────────────────────────────────

    #[test]
    fn single_element_tree() {
        let mut ft = FenwickTree::new(1);
        assert_eq!(ft.len(), 1);
        assert!(!ft.is_empty());
        assert_eq!(ft.total(), 0);
        ft.update(0, 42);
        assert_eq!(ft.get(0), 42);
        assert_eq!(ft.prefix(0), 42);
        assert_eq!(ft.total(), 42);
    }

    #[test]
    fn from_values_empty() {
        let ft = FenwickTree::from_values(&[]);
        assert!(ft.is_empty());
        assert_eq!(ft.len(), 0);
        assert_eq!(ft.total(), 0);
    }

    #[test]
    fn from_values_single() {
        let ft = FenwickTree::from_values(&[99]);
        assert_eq!(ft.len(), 1);
        assert_eq!(ft.get(0), 99);
        assert_eq!(ft.total(), 99);
    }

    #[test]
    fn update_last_element() {
        let mut ft = FenwickTree::new(5);
        ft.update(4, 100);
        assert_eq!(ft.get(4), 100);
        assert_eq!(ft.total(), 100);
    }

    #[test]
    fn update_negative_delta() {
        let mut ft = FenwickTree::from_values(&[10, 20, 30]);
        ft.update(1, -5);
        assert_eq!(ft.get(1), 15);
        assert_eq!(ft.total(), 55);
    }

    #[test]
    fn update_wraps_below_zero() {
        let mut ft = FenwickTree::from_values(&[5]);
        ft.update(0, -10);
        // 5u32.wrapping_add((-10i32) as u32) wraps
        let expected = 5u32.wrapping_add((-10i32) as u32);
        assert_eq!(ft.get(0), expected);
    }

    #[test]
    fn get_at_zero_single_element() {
        let ft = FenwickTree::from_values(&[42]);
        assert_eq!(ft.get(0), 42);
    }

    #[test]
    fn get_after_multiple_updates() {
        let mut ft = FenwickTree::new(3);
        ft.update(1, 10);
        ft.update(1, 20);
        ft.update(1, -5);
        assert_eq!(ft.get(1), 25);
    }

    #[test]
    fn prefix_zero_after_update() {
        let mut ft = FenwickTree::new(5);
        ft.update(0, 7);
        assert_eq!(ft.prefix(0), 7);
    }

    #[test]
    fn range_single_element() {
        let ft = FenwickTree::from_values(&[10, 20, 30]);
        assert_eq!(ft.range(1, 1), 20);
    }

    #[test]
    fn range_full_equals_total() {
        let ft = FenwickTree::from_values(&[1, 2, 3, 4, 5]);
        assert_eq!(ft.range(0, 4), ft.total());
    }

    #[test]
    fn range_all_zeros() {
        let ft = FenwickTree::new(5);
        assert_eq!(ft.range(0, 4), 0);
        assert_eq!(ft.range(2, 3), 0);
    }

    #[test]
    fn batch_update_empty() {
        let mut ft = FenwickTree::from_values(&[1, 2, 3]);
        ft.batch_update(&[]);
        assert_eq!(ft.total(), 6);
    }

    #[test]
    fn batch_update_same_index_multiple_times() {
        let mut ft = FenwickTree::new(3);
        ft.batch_update(&[(0, 10), (0, 20), (0, -5)]);
        assert_eq!(ft.get(0), 25);
        assert_eq!(ft.get(1), 0);
    }

    #[test]
    fn rebuild_same_values_idempotent() {
        let values = vec![5, 10, 15];
        let mut ft = FenwickTree::from_values(&values);
        let total_before = ft.total();
        ft.rebuild(&values);
        assert_eq!(ft.total(), total_before);
        for (i, &v) in values.iter().enumerate() {
            assert_eq!(ft.get(i), v);
        }
    }

    #[test]
    fn rebuild_all_zeros() {
        let mut ft = FenwickTree::from_values(&[10, 20, 30]);
        ft.rebuild(&[0, 0, 0]);
        assert_eq!(ft.total(), 0);
        assert_eq!(ft.get(0), 0);
        assert_eq!(ft.get(1), 0);
        assert_eq!(ft.get(2), 0);
    }

    #[test]
    fn find_prefix_empty_tree() {
        let ft = FenwickTree::new(0);
        assert_eq!(ft.find_prefix(0), None);
        assert_eq!(ft.find_prefix(100), None);
    }

    #[test]
    fn find_prefix_single_element() {
        let ft = FenwickTree::from_values(&[10]);
        assert_eq!(ft.find_prefix(0), None);
        assert_eq!(ft.find_prefix(10), Some(0));
        assert_eq!(ft.find_prefix(100), Some(0));
    }

    #[test]
    fn find_prefix_target_exceeds_total() {
        let ft = FenwickTree::from_values(&[1, 2, 3]);
        // total = 6, target = 1000 → should return last index
        assert_eq!(ft.find_prefix(1000), Some(2));
    }

    #[test]
    fn find_prefix_target_equals_total() {
        let ft = FenwickTree::from_values(&[5, 5, 5]);
        // total = 15
        assert_eq!(ft.find_prefix(15), Some(2));
    }

    #[test]
    fn find_prefix_exact_boundaries() {
        let ft = FenwickTree::from_values(&[10, 10, 10]);
        // prefix(0) = 10, prefix(1) = 20, prefix(2) = 30
        assert_eq!(ft.find_prefix(10), Some(0));
        assert_eq!(ft.find_prefix(20), Some(1));
        assert_eq!(ft.find_prefix(30), Some(2));
    }

    #[test]
    fn resize_to_zero() {
        let mut ft = FenwickTree::from_values(&[1, 2, 3]);
        ft.resize(0);
        assert!(ft.is_empty());
        assert_eq!(ft.total(), 0);
    }

    #[test]
    fn resize_same_size_noop() {
        let mut ft = FenwickTree::from_values(&[1, 2, 3]);
        ft.resize(3);
        assert_eq!(ft.len(), 3);
        assert_eq!(ft.total(), 6);
    }

    #[test]
    fn resize_grow_from_zero() {
        let mut ft = FenwickTree::new(0);
        ft.resize(3);
        assert_eq!(ft.len(), 3);
        assert_eq!(ft.total(), 0);
        ft.update(0, 5);
        assert_eq!(ft.get(0), 5);
    }

    #[test]
    fn clone_independence() {
        let mut ft = FenwickTree::from_values(&[1, 2, 3]);
        let cloned = ft.clone();
        ft.update(0, 100);
        // Clone unaffected
        assert_eq!(cloned.get(0), 1);
        assert_eq!(ft.get(0), 101);
    }

    #[test]
    fn debug_format_contains_name() {
        let ft = FenwickTree::new(3);
        let dbg = format!("{ft:?}");
        assert!(dbg.contains("FenwickTree"));
    }

    #[test]
    fn set_to_zero() {
        let mut ft = FenwickTree::from_values(&[10, 20, 30]);
        ft.set(1, 0);
        assert_eq!(ft.get(1), 0);
        assert_eq!(ft.total(), 40);
    }

    #[test]
    fn set_same_value_is_noop() {
        let mut ft = FenwickTree::from_values(&[10, 20, 30]);
        ft.set(1, 20);
        assert_eq!(ft.get(1), 20);
        assert_eq!(ft.total(), 60);
    }

    #[test]
    fn set_to_max_u32() {
        let mut ft = FenwickTree::from_values(&[0, 0, 0]);
        ft.set(0, u32::MAX);
        assert_eq!(ft.get(0), u32::MAX);
    }

    #[test]
    fn total_on_single_element() {
        let ft = FenwickTree::from_values(&[42]);
        assert_eq!(ft.total(), 42);
    }

    #[test]
    fn from_values_preserves_all() {
        let values: Vec<u32> = (1..=20).collect();
        let ft = FenwickTree::from_values(&values);
        for (i, &v) in values.iter().enumerate() {
            assert_eq!(ft.get(i), v, "mismatch at index {i}");
        }
        assert_eq!(ft.total(), 210); // sum(1..=20)
    }

    #[test]
    fn range_first_to_middle() {
        let ft = FenwickTree::from_values(&[10, 20, 30, 40, 50]);
        assert_eq!(ft.range(0, 2), 60);
    }

    #[test]
    fn range_middle_to_end() {
        let ft = FenwickTree::from_values(&[10, 20, 30, 40, 50]);
        assert_eq!(ft.range(3, 4), 90);
    }

    #[test]
    #[should_panic(expected = "out of bounds")]
    fn update_out_of_bounds_panics() {
        let mut ft = FenwickTree::new(3);
        ft.update(3, 1);
    }

    #[test]
    #[should_panic(expected = "out of bounds")]
    fn prefix_out_of_bounds_panics() {
        let ft = FenwickTree::new(3);
        ft.prefix(3);
    }

    #[test]
    #[should_panic(expected = "left")]
    fn range_left_greater_than_right_panics() {
        let ft = FenwickTree::from_values(&[1, 2, 3]);
        ft.range(2, 1);
    }

    #[test]
    #[should_panic(expected = "rebuild size mismatch")]
    fn rebuild_wrong_size_panics() {
        let mut ft = FenwickTree::new(3);
        ft.rebuild(&[1, 2]);
    }

    // ─── End edge-case tests (bd-1i1vn) ──────────────────────────────

    #[test]
    fn lowbit_correctness() {
        assert_eq!(lowbit(1), 1);
        assert_eq!(lowbit(2), 2);
        assert_eq!(lowbit(3), 1);
        assert_eq!(lowbit(4), 4);
        assert_eq!(lowbit(6), 2);
        assert_eq!(lowbit(8), 8);
        assert_eq!(lowbit(12), 4);
    }

    #[test]
    fn msb_correctness() {
        assert_eq!(most_significant_bit(0), 0);
        assert_eq!(most_significant_bit(1), 1);
        assert_eq!(most_significant_bit(5), 4);
        assert_eq!(most_significant_bit(8), 8);
        assert_eq!(most_significant_bit(100), 64);
        assert_eq!(most_significant_bit(1000), 512);
    }
}
