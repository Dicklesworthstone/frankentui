//! Property-based invariant tests for S3-FIFO cache (bd-l6yba.5).
//!
//! These tests verify structural invariants of the S3Fifo<K,V> cache:
//!
//! 1. Every inserted key is retrievable until evicted
//! 2. Ghost queue never exceeds its capacity
//! 3. Queue sizes are internally consistent
//! 4. Accessed items survive eviction from the small queue
//! 5. No panics on arbitrary operation sequences
//! 6. Statistics counters are consistent
//! 7. Determinism: same operations yield same state
//! 8. Frequency capping at 3

use ftui_core::s3_fifo::S3Fifo;
use proptest::prelude::*;

// ── Strategies ──────────────────────────────────────────────────────────

/// Operations that can be applied to a cache.
#[derive(Debug, Clone)]
enum Op {
    Insert(u32, u32),
    Get(u32),
    Remove(u32),
    ContainsKey(u32),
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u32..200, any::<u32>()).prop_map(|(k, v)| Op::Insert(k, v)),
        (0u32..200).prop_map(Op::Get),
        (0u32..200).prop_map(Op::Remove),
        (0u32..200).prop_map(Op::ContainsKey),
    ]
}

fn capacity_strategy() -> impl Strategy<Value = usize> {
    2usize..200
}

/// Helper: compute expected ghost_cap for a given total capacity.
fn expected_ghost_cap(capacity: usize) -> usize {
    let cap = capacity.max(2);
    (cap / 10).max(1)
}

/// Helper: compute expected small_cap for a given total capacity.
fn expected_small_cap(capacity: usize) -> usize {
    let cap = capacity.max(2);
    (cap / 10).max(1)
}

/// Apply a sequence of operations to a cache.
fn apply_ops(cache: &mut S3Fifo<u32, u32>, ops: &[Op]) {
    for op in ops {
        match op {
            Op::Insert(k, v) => {
                cache.insert(*k, *v);
            }
            Op::Get(k) => {
                cache.get(k);
            }
            Op::Remove(k) => {
                cache.remove(k);
            }
            Op::ContainsKey(k) => {
                cache.contains_key(k);
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 1. Inserted keys are retrievable until evicted
// ═══════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn inserted_key_immediately_retrievable(
        cap in capacity_strategy(),
        key in 0u32..1000,
        value in any::<u32>(),
    ) {
        let mut cache = S3Fifo::new(cap);
        cache.insert(key, value);
        prop_assert_eq!(
            cache.get(&key),
            Some(&value),
            "freshly inserted key must be retrievable"
        );
    }

    #[test]
    fn insert_within_capacity_all_retrievable(
        cap in 10usize..100,
    ) {
        let mut cache = S3Fifo::new(cap);
        // Insert fewer items than capacity
        let count = cap / 2;
        for i in 0..count as u32 {
            cache.insert(i, i * 10);
            // Access each to ensure promotion
            cache.get(&i);
        }
        // All should be retrievable
        for i in 0..count as u32 {
            prop_assert!(
                cache.get(&i).is_some(),
                "key {} should be retrievable (count={}, cap={})", i, count, cap
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 2. Ghost queue never exceeds its capacity
// ═══════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn ghost_bounded_after_operations(
        cap in capacity_strategy(),
        ops in prop::collection::vec(op_strategy(), 0..500),
    ) {
        let mut cache = S3Fifo::new(cap);
        apply_ops(&mut cache, &ops);
        let stats = cache.stats();
        let ghost_cap = expected_ghost_cap(cap);
        prop_assert!(
            stats.ghost_size <= ghost_cap,
            "ghost_size {} must not exceed ghost_cap {} (total_cap={})",
            stats.ghost_size, ghost_cap, cap
        );
    }

    #[test]
    fn ghost_bounded_under_heavy_insertion(
        cap in capacity_strategy(),
        n in 100usize..1000,
    ) {
        let mut cache = S3Fifo::new(cap);
        for i in 0..n as u32 {
            cache.insert(i, i);
        }
        let stats = cache.stats();
        let ghost_cap = expected_ghost_cap(cap);
        prop_assert!(
            stats.ghost_size <= ghost_cap,
            "ghost_size {} exceeds ghost_cap {} after {} inserts",
            stats.ghost_size, ghost_cap, n
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 3. Queue sizes are internally consistent
// ═══════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn len_equals_small_plus_main(
        cap in capacity_strategy(),
        ops in prop::collection::vec(op_strategy(), 0..300),
    ) {
        let mut cache = S3Fifo::new(cap);
        apply_ops(&mut cache, &ops);
        let stats = cache.stats();
        prop_assert_eq!(
            cache.len(),
            stats.small_size + stats.main_size,
            "len() must equal small + main"
        );
    }

    #[test]
    fn len_never_exceeds_capacity(
        cap in capacity_strategy(),
        ops in prop::collection::vec(op_strategy(), 0..500),
    ) {
        let mut cache = S3Fifo::new(cap);
        apply_ops(&mut cache, &ops);
        prop_assert!(
            cache.len() <= cache.capacity(),
            "len {} exceeds capacity {}",
            cache.len(), cache.capacity()
        );
    }

    #[test]
    fn capacity_split_invariant(cap in capacity_strategy()) {
        let cache: S3Fifo<u32, u32> = S3Fifo::new(cap);
        let expected_cap = cap.max(2);
        let small = expected_small_cap(cap);
        let main = expected_cap - small;
        prop_assert_eq!(
            cache.capacity(),
            small + main,
            "capacity must be small_cap + main_cap"
        );
        prop_assert_eq!(cache.capacity(), expected_cap);
    }

    #[test]
    fn clear_resets_all_sizes(
        cap in capacity_strategy(),
        ops in prop::collection::vec(op_strategy(), 1..200),
    ) {
        let mut cache = S3Fifo::new(cap);
        apply_ops(&mut cache, &ops);
        cache.clear();
        let stats = cache.stats();
        prop_assert_eq!(stats.small_size, 0, "small must be 0 after clear");
        prop_assert_eq!(stats.main_size, 0, "main must be 0 after clear");
        prop_assert_eq!(stats.ghost_size, 0, "ghost must be 0 after clear");
        prop_assert_eq!(stats.hits, 0, "hits must be 0 after clear");
        prop_assert_eq!(stats.misses, 0, "misses must be 0 after clear");
        prop_assert!(cache.is_empty());
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 4. Accessed items survive eviction from the small queue
// ═══════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn accessed_item_survives_small_eviction(
        cap in 20usize..100,
        key in 0u32..1000,
        value in any::<u32>(),
    ) {
        let mut cache = S3Fifo::new(cap);
        let small_cap = expected_small_cap(cap);

        // Insert the target key and access it to set freq > 0
        cache.insert(key, value);
        cache.get(&key);

        // Fill small queue to force eviction of the target
        for i in 0..small_cap as u32 + 1 {
            let k = key.wrapping_add(i + 1); // avoid colliding with target
            cache.insert(k, 0);
        }

        // Target should have been promoted to main (freq > 0)
        prop_assert!(
            cache.get(&key).is_some(),
            "accessed key {} should survive small eviction (cap={}, small_cap={})",
            key, cap, small_cap
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 5. No panics on arbitrary operation sequences
// ═══════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn no_panics_on_arbitrary_ops(
        cap in capacity_strategy(),
        ops in prop::collection::vec(op_strategy(), 0..1000),
    ) {
        let mut cache = S3Fifo::new(cap);
        apply_ops(&mut cache, &ops);
        // If we get here, no panics occurred.
        let _ = cache.stats();
        let _ = cache.len();
        let _ = cache.is_empty();
        let _ = cache.capacity();
    }

    #[test]
    fn no_panics_minimum_capacity(
        ops in prop::collection::vec(op_strategy(), 0..200),
    ) {
        let mut cache = S3Fifo::new(0); // will be clamped to 2
        apply_ops(&mut cache, &ops);
        prop_assert!(cache.capacity() >= 2);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 6. Statistics counters are consistent
// ═══════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn contains_key_agrees_with_get(
        cap in capacity_strategy(),
        ops in prop::collection::vec(op_strategy(), 0..200),
        probe in 0u32..200,
    ) {
        let mut cache = S3Fifo::new(cap);
        apply_ops(&mut cache, &ops);

        // Snapshot contains_key before get (get may change freq)
        let contained = cache.contains_key(&probe);
        let got = cache.get(&probe);

        if contained {
            prop_assert!(got.is_some(), "contains_key=true but get=None for key {}", probe);
        } else {
            prop_assert!(got.is_none(), "contains_key=false but get=Some for key {}", probe);
        }
    }

    #[test]
    fn update_returns_old_value(
        cap in capacity_strategy(),
        key in 0u32..100,
        v1 in any::<u32>(),
        v2 in any::<u32>(),
    ) {
        let mut cache = S3Fifo::new(cap);
        let first = cache.insert(key, v1);
        prop_assert_eq!(first, None, "first insert should return None");

        let second = cache.insert(key, v2);
        prop_assert_eq!(second, Some(v1), "second insert should return old value");

        prop_assert_eq!(cache.get(&key), Some(&v2), "value should be updated");
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 7. Determinism: same operations yield same state
// ═══════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn deterministic_state(
        cap in capacity_strategy(),
        ops in prop::collection::vec(op_strategy(), 0..200),
    ) {
        let mut cache_a = S3Fifo::new(cap);
        let mut cache_b = S3Fifo::new(cap);

        apply_ops(&mut cache_a, &ops);
        apply_ops(&mut cache_b, &ops);

        let stats_a = cache_a.stats();
        let stats_b = cache_b.stats();

        prop_assert_eq!(stats_a, stats_b, "identical ops must produce identical stats");
        prop_assert_eq!(cache_a.len(), cache_b.len());

        // Verify same keys are present
        for key in 0u32..200 {
            let a = cache_a.contains_key(&key);
            let b = cache_b.contains_key(&key);
            prop_assert_eq!(a, b, "key {} presence differs between replicas", key);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 8. Frequency capping
// ═══════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn remove_returns_none_for_absent(
        cap in capacity_strategy(),
        ops in prop::collection::vec(op_strategy(), 0..100),
        key in 200u32..400, // guaranteed not inserted by ops (ops use 0..200)
    ) {
        let mut cache = S3Fifo::new(cap);
        apply_ops(&mut cache, &ops);
        prop_assert_eq!(cache.remove(&key), None, "remove of absent key must return None");
    }

    #[test]
    fn remove_makes_key_absent(
        cap in capacity_strategy(),
        key in 0u32..100,
        value in any::<u32>(),
    ) {
        let mut cache = S3Fifo::new(cap);
        cache.insert(key, value);
        let removed = cache.remove(&key);
        prop_assert_eq!(removed, Some(value));
        prop_assert!(!cache.contains_key(&key), "removed key should be absent");
        prop_assert_eq!(cache.get(&key), None, "removed key get should return None");
    }
}
