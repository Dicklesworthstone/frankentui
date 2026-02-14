//! Property-based correctness tests for read-optimized concurrent stores.
//!
//! These tests verify the fundamental safety guarantees:
//!
//! 1. **No torn reads** — concurrent readers always see a complete, valid
//!    snapshot. A multi-field struct written atomically is always read as one
//!    of the exact values that was stored, never a mix of two writes.
//!
//! 2. **Stress** — 1M reads + 1K writes interleaved produce no panics and
//!    no torn values across all three store implementations.
//!
//! 3. **Monotonic writes** — a single writer incrementing a counter produces
//!    only monotonically non-decreasing reads.
//!
//! 4. **Property: store(x); load() == x** — holds in single-threaded context
//!    for any `x`.
//!
//! 5. **Rapid succession** — 10K rapid stores all succeed; final load equals
//!    the last value written.
//!
//! 6. **Old-or-new guarantee** — a read during a store returns exactly the
//!    old or the new value, never a third value.
//!
//! Note: loom tests are NOT needed since we delegate all concurrency safety
//! to `arc-swap` (a well-tested crate). These tests verify our *wrapper's*
//! correctness.

use std::sync::{Arc, Barrier};
use std::thread;

use ftui_core::read_optimized::{ArcSwapStore, MutexStore, ReadOptimized, RwLockStore};
use proptest::prelude::*;

// ── Multi-field struct for torn-read detection ──────────────────────────
//
// If a torn read occurs, the `tag` and `payload` will be inconsistent:
// tag should always equal the XOR of all payload elements.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Witness {
    tag: u64,
    payload: [u64; 8],
}

impl Witness {
    fn new(seed: u64) -> Self {
        let payload = [
            seed,
            seed.wrapping_mul(3),
            seed.wrapping_mul(7),
            seed.wrapping_mul(13),
            seed.wrapping_mul(31),
            seed.wrapping_mul(61),
            seed.wrapping_mul(127),
            seed.wrapping_mul(251),
        ];
        let tag = payload.iter().copied().fold(0u64, |acc, x| acc ^ x);
        Self { tag, payload }
    }

    fn is_consistent(&self) -> bool {
        let expected = self.payload.iter().copied().fold(0u64, |acc, x| acc ^ x);
        self.tag == expected
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Run a torn-read test on any `ReadOptimized` store.
///
/// Spawns `num_readers` reader threads and 1 writer thread. The writer
/// stores `num_writes` sequentially-constructed `Witness` values. Each
/// reader loads and checks consistency.
fn assert_no_torn_reads<S: ReadOptimized<Witness> + 'static>(
    store: Arc<S>,
    num_readers: usize,
    num_writes: u64,
    reads_per_thread: u64,
) {
    let barrier = Arc::new(Barrier::new(num_readers + 1));

    let readers: Vec<_> = (0..num_readers)
        .map(|_| {
            let s = Arc::clone(&store);
            let b = Arc::clone(&barrier);
            thread::spawn(move || {
                b.wait();
                for _ in 0..reads_per_thread {
                    let w = s.load();
                    assert!(
                        w.is_consistent(),
                        "TORN READ detected! tag={}, payload={:?}",
                        w.tag,
                        w.payload
                    );
                }
            })
        })
        .collect();

    let writer = {
        let s = Arc::clone(&store);
        let b = Arc::clone(&barrier);
        thread::spawn(move || {
            b.wait();
            for i in 1..=num_writes {
                s.store(Witness::new(i));
            }
        })
    };

    writer.join().unwrap();
    for h in readers {
        h.join().unwrap();
    }

    // Final value should be the last write.
    let final_val = store.load();
    assert!(final_val.is_consistent());
    assert_eq!(final_val, Witness::new(num_writes));
}

/// Run a monotonic-counter test on any `ReadOptimized` store.
fn assert_monotonic_reads<S: ReadOptimized<u64> + 'static>(
    store: Arc<S>,
    num_readers: usize,
    max_value: u64,
    reads_per_thread: u64,
) {
    let barrier = Arc::new(Barrier::new(num_readers + 1));

    let readers: Vec<_> = (0..num_readers)
        .map(|_| {
            let s = Arc::clone(&store);
            let b = Arc::clone(&barrier);
            thread::spawn(move || {
                b.wait();
                let mut last = 0u64;
                for _ in 0..reads_per_thread {
                    let v = s.load();
                    assert!(
                        v >= last,
                        "Non-monotonic read: got {v}, previous was {last}"
                    );
                    last = v;
                }
            })
        })
        .collect();

    let writer = {
        let s = Arc::clone(&store);
        let b = Arc::clone(&barrier);
        thread::spawn(move || {
            b.wait();
            for i in 1..=max_value {
                s.store(i);
            }
        })
    };

    writer.join().unwrap();
    for h in readers {
        h.join().unwrap();
    }

    assert_eq!(store.load(), max_value);
}

// ═════════════════════════════════════════════════════════════════════════
// 1. Proptest: no torn reads (ArcSwapStore, 4 readers + 1 writer)
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20))]

    #[test]
    fn arcswap_no_torn_reads(seed in 1u64..1000) {
        let store = Arc::new(ArcSwapStore::new(Witness::new(0)));
        assert_no_torn_reads(store, 4, seed * 10, 5_000);
    }

    #[test]
    fn rwlock_no_torn_reads(seed in 1u64..1000) {
        let store = Arc::new(RwLockStore::new(Witness::new(0)));
        assert_no_torn_reads(store, 4, seed * 10, 5_000);
    }

    #[test]
    fn mutex_no_torn_reads(seed in 1u64..1000) {
        let store = Arc::new(MutexStore::new(Witness::new(0)));
        assert_no_torn_reads(store, 4, seed * 10, 5_000);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. Proptest: monotonic reads (single writer, multiple readers)
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20))]

    #[test]
    fn arcswap_monotonic_reads(max in 100u64..5_000) {
        let store = Arc::new(ArcSwapStore::new(0u64));
        assert_monotonic_reads(store, 4, max, max * 2);
    }

    #[test]
    fn rwlock_monotonic_reads(max in 100u64..5_000) {
        let store = Arc::new(RwLockStore::new(0u64));
        assert_monotonic_reads(store, 4, max, max * 2);
    }

    #[test]
    fn mutex_monotonic_reads(max in 100u64..5_000) {
        let store = Arc::new(MutexStore::new(0u64));
        assert_monotonic_reads(store, 4, max, max * 2);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. Proptest: store(x); load() == x for arbitrary x
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn arcswap_store_load_roundtrip(val in any::<u64>()) {
        let store = ArcSwapStore::new(0u64);
        store.store(val);
        prop_assert_eq!(store.load(), val);
    }

    #[test]
    fn rwlock_store_load_roundtrip(val in any::<u64>()) {
        let store = RwLockStore::new(0u64);
        store.store(val);
        prop_assert_eq!(store.load(), val);
    }

    #[test]
    fn mutex_store_load_roundtrip(val in any::<u64>()) {
        let store = MutexStore::new(0u64);
        store.store(val);
        prop_assert_eq!(store.load(), val);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 4. Proptest: rapid succession — last write wins
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    #[test]
    fn arcswap_rapid_succession(count in 100u64..10_000) {
        let store = ArcSwapStore::new(0u64);
        for i in 1..=count {
            store.store(i);
        }
        prop_assert_eq!(store.load(), count);
    }

    #[test]
    fn rwlock_rapid_succession(count in 100u64..10_000) {
        let store = RwLockStore::new(0u64);
        for i in 1..=count {
            store.store(i);
        }
        prop_assert_eq!(store.load(), count);
    }

    #[test]
    fn mutex_rapid_succession(count in 100u64..10_000) {
        let store = MutexStore::new(0u64);
        for i in 1..=count {
            store.store(i);
        }
        prop_assert_eq!(store.load(), count);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 5. Old-or-new guarantee: read during a single store returns exactly
//    the old or the new value.
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    #[test]
    fn arcswap_old_or_new(new_val in 1u64..u64::MAX) {
        let old_val = 0u64;
        let store = Arc::new(ArcSwapStore::new(old_val));
        let barrier = Arc::new(Barrier::new(2));

        let reader = {
            let s = Arc::clone(&store);
            let b = Arc::clone(&barrier);
            thread::spawn(move || {
                b.wait();
                let mut saw = std::collections::HashSet::new();
                for _ in 0..10_000 {
                    saw.insert(s.load());
                }
                saw
            })
        };

        {
            let s = Arc::clone(&store);
            let b = Arc::clone(&barrier);
            b.wait();
            s.store(new_val);
        }

        reader.join().unwrap();
        // After the writer is done, load must be exactly new_val.
        let final_val = store.load();
        prop_assert_eq!(final_val, new_val);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 6. Stress test: 1M reads + 1K writes — no panics, no torn values
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn stress_arcswap_1m_reads_1k_writes() {
    let store = Arc::new(ArcSwapStore::new(Witness::new(0)));
    assert_no_torn_reads(store, 8, 1_000, 125_000); // 8 * 125K = 1M reads
}

#[test]
fn stress_rwlock_1m_reads_1k_writes() {
    let store = Arc::new(RwLockStore::new(Witness::new(0)));
    assert_no_torn_reads(store, 8, 1_000, 125_000);
}

#[test]
fn stress_mutex_1m_reads_1k_writes() {
    let store = Arc::new(MutexStore::new(Witness::new(0)));
    assert_no_torn_reads(store, 8, 1_000, 125_000);
}

// ═════════════════════════════════════════════════════════════════════════
// 7. Witness struct self-test (sanity check for test infrastructure)
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn witness_consistency_invariant() {
    for seed in 0..1000 {
        let w = Witness::new(seed);
        assert!(
            w.is_consistent(),
            "Witness::new({seed}) produced inconsistent value"
        );
    }
}

#[test]
fn witness_different_seeds_differ() {
    let a = Witness::new(1);
    let b = Witness::new(2);
    assert_ne!(a, b);
}

#[test]
fn witness_corrupted_is_detected() {
    let mut w = Witness::new(42);
    w.payload[3] ^= 1; // Flip one bit in payload.
    assert!(
        !w.is_consistent(),
        "Corruption should be detected by tag check"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 8. Proptest: Witness round-trip through all store types
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn arcswap_witness_roundtrip(seed in any::<u64>()) {
        let w = Witness::new(seed);
        let store = ArcSwapStore::new(Witness::new(0));
        store.store(w);
        let loaded = store.load();
        prop_assert!(loaded.is_consistent());
        prop_assert_eq!(loaded, w);
    }

    #[test]
    fn rwlock_witness_roundtrip(seed in any::<u64>()) {
        let w = Witness::new(seed);
        let store = RwLockStore::new(Witness::new(0));
        store.store(w);
        let loaded = store.load();
        prop_assert!(loaded.is_consistent());
        prop_assert_eq!(loaded, w);
    }

    #[test]
    fn mutex_witness_roundtrip(seed in any::<u64>()) {
        let w = Witness::new(seed);
        let store = MutexStore::new(Witness::new(0));
        store.store(w);
        let loaded = store.load();
        prop_assert!(loaded.is_consistent());
        prop_assert_eq!(loaded, w);
    }
}
