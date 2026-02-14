#![forbid(unsafe_code)]

//! Property-based invariant tests for the Roaring Bitmap.
//!
//! These tests verify structural invariants that must hold for **any**
//! set of insertions:
//!
//! 1. Union matches naive set union.
//! 2. Intersection matches naive set intersection.
//! 3. Iteration produces the same sorted sequence as a dense `HashSet`.
//! 4. Insert/clear round-trips correctly.
//! 5. Cardinality is always consistent with iteration count.
//! 6. `contains` is consistent with `iter` membership.
//! 7. Duplicate inserts are idempotent.
//! 8. `insert_range` matches individual inserts.
//! 9. Union is commutative.
//! 10. Intersection is commutative.

use ftui_render::roaring_bitmap::RoaringBitmap;
use proptest::prelude::*;
use std::collections::BTreeSet;

// ── Strategies ──────────────────────────────────────────────────────────

/// Values spanning multiple containers (high16 buckets).
fn value_set() -> impl Strategy<Value = Vec<u32>> {
    proptest::collection::vec(
        prop_oneof![
            // Same container (container 0): small values.
            0u32..1000,
            // Spread across first few containers.
            0u32..200_000,
            // Occasional large values in distant containers.
            0u32..1_000_000,
        ],
        0..300,
    )
}

/// A pair of value sets for binary operations.
fn two_value_sets() -> impl Strategy<Value = (Vec<u32>, Vec<u32>)> {
    (value_set(), value_set())
}

/// Dense value sets that may trigger array→bitmap promotion.
fn dense_value_set() -> impl Strategy<Value = Vec<u32>> {
    proptest::collection::vec(0u32..65536, 0..5000)
}

/// Small ranges for insert_range tests.
fn range_set() -> impl Strategy<Value = Vec<(u32, u32)>> {
    proptest::collection::vec(
        (0u32..100_000, 0u32..500).prop_map(|(start, len)| (start, start + len)),
        0..20,
    )
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn build_bitmap(values: &[u32]) -> RoaringBitmap {
    let mut bm = RoaringBitmap::new();
    for &v in values {
        bm.insert(v);
    }
    bm
}

fn build_naive(values: &[u32]) -> BTreeSet<u32> {
    values.iter().copied().collect()
}

// ═════════════════════════════════════════════════════════════════════════
// 1. Union matches naive set union
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn union_matches_naive((a_vals, b_vals) in two_value_sets()) {
        let a = build_bitmap(&a_vals);
        let b = build_bitmap(&b_vals);
        let result = a.union(&b);

        let mut naive = build_naive(&a_vals);
        for &v in &b_vals {
            naive.insert(v);
        }

        let roaring_set: BTreeSet<u32> = result.iter().collect();
        prop_assert_eq!(&roaring_set, &naive,
            "Union mismatch: roaring has {} items, naive has {}",
            roaring_set.len(), naive.len());
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. Intersection matches naive set intersection
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn intersection_matches_naive((a_vals, b_vals) in two_value_sets()) {
        let a = build_bitmap(&a_vals);
        let b = build_bitmap(&b_vals);
        let result = a.intersection(&b);

        let naive_a = build_naive(&a_vals);
        let naive_b = build_naive(&b_vals);
        let naive: BTreeSet<u32> = naive_a.intersection(&naive_b).copied().collect();

        let roaring_set: BTreeSet<u32> = result.iter().collect();
        prop_assert_eq!(&roaring_set, &naive,
            "Intersection mismatch: roaring has {} items, naive has {}",
            roaring_set.len(), naive.len());
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. Iteration produces sorted sequence matching BTreeSet
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn iter_matches_naive_sorted(values in value_set()) {
        let bm = build_bitmap(&values);
        let naive = build_naive(&values);

        let roaring_vec: Vec<u32> = bm.iter().collect();
        let naive_vec: Vec<u32> = naive.iter().copied().collect();

        prop_assert_eq!(&roaring_vec, &naive_vec,
            "Iteration order mismatch");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 4. Insert/clear round-trips correctly
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn insert_clear_roundtrip(values in value_set()) {
        let mut bm = build_bitmap(&values);

        // Verify non-empty if we inserted anything unique.
        let naive = build_naive(&values);
        prop_assert_eq!(bm.cardinality(), naive.len(),
            "Cardinality before clear");

        bm.clear();
        prop_assert!(bm.is_empty(), "Bitmap should be empty after clear");
        prop_assert_eq!(bm.cardinality(), 0, "Cardinality should be 0 after clear");
        prop_assert_eq!(bm.iter().count(), 0, "Iter should yield nothing after clear");

        // No value should be found.
        for &v in &values {
            prop_assert!(!bm.contains(v),
                "Value {} still found after clear", v);
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 5. Cardinality is consistent with iteration count
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cardinality_matches_iter_count(values in value_set()) {
        let bm = build_bitmap(&values);
        let iter_count = bm.iter().count();
        prop_assert_eq!(bm.cardinality(), iter_count,
            "cardinality() = {} but iter().count() = {}", bm.cardinality(), iter_count);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 6. contains is consistent with iter membership
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn contains_consistent_with_iter(values in value_set()) {
        let bm = build_bitmap(&values);
        let iter_set: BTreeSet<u32> = bm.iter().collect();

        // Every value we inserted (deduplicated) should be in both.
        let naive = build_naive(&values);
        for &v in &naive {
            prop_assert!(bm.contains(v),
                "contains({}) = false but value was inserted", v);
            prop_assert!(iter_set.contains(&v),
                "iter() missed value {}", v);
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 7. Duplicate inserts are idempotent
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn duplicate_inserts_idempotent(values in value_set()) {
        let bm1 = build_bitmap(&values);

        // Insert everything twice.
        let mut bm2 = build_bitmap(&values);
        for &v in &values {
            bm2.insert(v);
        }

        prop_assert_eq!(bm1.cardinality(), bm2.cardinality(),
            "Double insertion changed cardinality");

        let v1: Vec<u32> = bm1.iter().collect();
        let v2: Vec<u32> = bm2.iter().collect();
        prop_assert_eq!(v1, v2, "Double insertion changed iteration order");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 8. insert_range matches individual inserts
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn insert_range_matches_individual(ranges in range_set()) {
        let mut bm_range = RoaringBitmap::new();
        let mut bm_individual = RoaringBitmap::new();

        for &(start, end) in &ranges {
            bm_range.insert_range(start, end);
            for v in start..end {
                bm_individual.insert(v);
            }
        }

        let v_range: Vec<u32> = bm_range.iter().collect();
        let v_individual: Vec<u32> = bm_individual.iter().collect();
        prop_assert_eq!(v_range, v_individual,
            "insert_range disagrees with individual inserts");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 9. Union is commutative
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn union_commutative((a_vals, b_vals) in two_value_sets()) {
        let a = build_bitmap(&a_vals);
        let b = build_bitmap(&b_vals);

        let ab: Vec<u32> = a.union(&b).iter().collect();
        let ba: Vec<u32> = b.union(&a).iter().collect();
        prop_assert_eq!(ab, ba, "union(a,b) != union(b,a)");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 10. Intersection is commutative
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn intersection_commutative((a_vals, b_vals) in two_value_sets()) {
        let a = build_bitmap(&a_vals);
        let b = build_bitmap(&b_vals);

        let ab: Vec<u32> = a.intersection(&b).iter().collect();
        let ba: Vec<u32> = b.intersection(&a).iter().collect();
        prop_assert_eq!(ab, ba, "intersection(a,b) != intersection(b,a)");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 11. Dense sets: array→bitmap promotion preserves all values
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn promotion_preserves_values(values in dense_value_set()) {
        let bm = build_bitmap(&values);
        let naive = build_naive(&values);

        // All values must survive promotion.
        for &v in &naive {
            prop_assert!(bm.contains(v),
                "Value {} lost after possible promotion", v);
        }

        prop_assert_eq!(bm.cardinality(), naive.len(),
            "Cardinality mismatch after promotion");
    }
}
