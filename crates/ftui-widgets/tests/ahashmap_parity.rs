//! AHashMap vs std HashMap parity tests (bd-1uj0o.3).
//!
//! Validates that replacing std::collections::HashMap with ahash::AHashMap
//! produces identical behavior for all operations used in the widget registry,
//! focus graph, measure cache, and style cache hot paths.

use std::collections::HashMap;

use ahash::AHashMap;

// ---------------------------------------------------------------------------
// 1. Insertion and retrieval: all key types stored and retrieved correctly
// ---------------------------------------------------------------------------

#[test]
fn insert_retrieve_u64_keys() {
    let mut std_map = HashMap::new();
    let mut ahash_map = AHashMap::new();

    for i in 0..1000u64 {
        std_map.insert(i, i * 2);
        ahash_map.insert(i, i * 2);
    }

    for i in 0..1000u64 {
        assert_eq!(std_map.get(&i), ahash_map.get(&i), "mismatch at key {i}");
    }
    assert_eq!(std_map.len(), ahash_map.len());
}

#[test]
fn insert_retrieve_string_keys() {
    let mut std_map = HashMap::new();
    let mut ahash_map = AHashMap::new();

    let keys: Vec<String> = (0..200).map(|i| format!("style-{i}")).collect();
    for (i, k) in keys.iter().enumerate() {
        std_map.insert(k.clone(), i as u32);
        ahash_map.insert(k.clone(), i as u32);
    }

    for k in &keys {
        assert_eq!(
            std_map.get(k.as_str()),
            ahash_map.get(k.as_str()),
            "mismatch at key {k}"
        );
    }
}

#[test]
fn insert_retrieve_composite_keys() {
    // Matches (FocusId, NavDirection) key pattern in focus/graph.rs
    #[derive(Clone, Copy, PartialEq, Eq, Hash)]
    struct CompositeKey(u64, u8);

    let mut std_map = HashMap::new();
    let mut ahash_map = AHashMap::new();

    for id in 0..100u64 {
        for dir in 0..6u8 {
            let key = CompositeKey(id, dir);
            std_map.insert(key, id + dir as u64);
            ahash_map.insert(key, id + dir as u64);
        }
    }

    for id in 0..100u64 {
        for dir in 0..6u8 {
            let key = CompositeKey(id, dir);
            assert_eq!(std_map.get(&key), ahash_map.get(&key));
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Collision handling: verify correctness under high load factor
// ---------------------------------------------------------------------------

#[test]
fn high_load_factor_correctness() {
    // Insert with_capacity(10) but store 100 entries → forces rehashing.
    let mut ahash_map: AHashMap<u64, u64> = AHashMap::with_capacity(10);
    for i in 0..100u64 {
        ahash_map.insert(i, i * 3);
    }
    assert_eq!(ahash_map.len(), 100);
    for i in 0..100u64 {
        assert_eq!(ahash_map.get(&i), Some(&(i * 3)));
    }
}

// ---------------------------------------------------------------------------
// 3. Deletion: remove widgets, verify no ghost entries
// ---------------------------------------------------------------------------

#[test]
fn delete_no_ghost_entries() {
    let mut ahash_map: AHashMap<u64, String> = AHashMap::new();

    // Insert then remove even keys
    for i in 0..200u64 {
        ahash_map.insert(i, format!("val-{i}"));
    }
    for i in (0..200u64).step_by(2) {
        ahash_map.remove(&i);
    }

    assert_eq!(ahash_map.len(), 100);
    for i in 0..200u64 {
        if i % 2 == 0 {
            assert!(ahash_map.get(&i).is_none(), "ghost at even key {i}");
        } else {
            assert_eq!(ahash_map.get(&i), Some(&format!("val-{i}")));
        }
    }
}

// ---------------------------------------------------------------------------
// 4. Iteration: sorted results match (iteration order differs, values same)
// ---------------------------------------------------------------------------

#[test]
fn iteration_value_parity() {
    let mut std_map = HashMap::new();
    let mut ahash_map = AHashMap::new();

    for i in 0..500u64 {
        std_map.insert(i, i * 7);
        ahash_map.insert(i, i * 7);
    }

    // Iteration order may differ, but sorted key-value pairs must match.
    let mut std_pairs: Vec<_> = std_map.into_iter().collect();
    let mut ahash_pairs: Vec<_> = ahash_map.into_iter().collect();
    std_pairs.sort_by_key(|(k, _)| *k);
    ahash_pairs.sort_by_key(|(k, _)| *k);

    assert_eq!(std_pairs, ahash_pairs);
}

// ---------------------------------------------------------------------------
// 5. Capacity and resize: automatic growth matches expected behavior
// ---------------------------------------------------------------------------

#[test]
fn capacity_growth_no_panic() {
    let mut map: AHashMap<u64, u64> = AHashMap::new();
    // Insert many entries; verify no panic and all retrievable.
    for i in 0..50_000u64 {
        map.insert(i, i);
    }
    assert_eq!(map.len(), 50_000);
    assert!(map.capacity() >= 50_000);

    // Spot-check
    assert_eq!(map.get(&0), Some(&0));
    assert_eq!(map.get(&25_000), Some(&25_000));
    assert_eq!(map.get(&49_999), Some(&49_999));
}

// ---------------------------------------------------------------------------
// 6. Empty map edge cases
// ---------------------------------------------------------------------------

#[test]
fn empty_map_operations() {
    let map: AHashMap<u64, u64> = AHashMap::new();
    assert_eq!(map.len(), 0);
    assert!(map.is_empty());
    assert_eq!(map.get(&42), None);

    let mut map2: AHashMap<u64, u64> = AHashMap::new();
    assert_eq!(map2.remove(&0), None);
    assert!(map2.is_empty());
}

// ---------------------------------------------------------------------------
// 7. Default trait: AHashMap derives Default correctly (used in struct derives)
// ---------------------------------------------------------------------------

#[test]
fn default_trait_works() {
    // FocusGraph and FocusManager derive Default with AHashMap fields.
    #[derive(Default)]
    struct MockFocusGraph {
        nodes: AHashMap<u64, String>,
        edges: AHashMap<(u64, u8), u64>,
    }

    let g = MockFocusGraph::default();
    assert!(g.nodes.is_empty());
    assert!(g.edges.is_empty());
}

// ---------------------------------------------------------------------------
// 8. Integration: measure cache round-trip (insert, get, evict, get)
// ---------------------------------------------------------------------------

#[test]
fn measure_cache_round_trip() {
    // Simulates the MeasureCache lifecycle:
    // insert with_capacity → get_or_compute → invalidate → miss
    let mut cache: AHashMap<(u64, u16, u16), (u16, u16)> = AHashMap::with_capacity(100);

    // Populate
    for id in 0..100u64 {
        let key = (id, 80, 24);
        let value = ((id % 200) as u16, (id % 60) as u16);
        cache.insert(key, value);
    }

    // Hit
    for id in 0..100u64 {
        let key = (id, 80, 24);
        assert!(cache.contains_key(&key));
    }

    // Miss (different available size)
    for id in 0..100u64 {
        let key = (id, 100, 30);
        assert!(!cache.contains_key(&key));
    }

    // Evict (clear simulates generation bump)
    cache.clear();
    assert!(cache.is_empty());

    // Re-insert: allocation reuse
    for id in 0..50u64 {
        cache.insert((id, 80, 24), (10, 10));
    }
    assert_eq!(cache.len(), 50);
}

// ---------------------------------------------------------------------------
// 9. Integration: focus graph insert/remove/navigate round-trip
// ---------------------------------------------------------------------------

#[test]
fn focus_graph_round_trip() {
    let mut nodes: AHashMap<u64, (i32, bool)> = AHashMap::new();
    let mut edges: AHashMap<(u64, u8), u64> = AHashMap::new();

    // Build graph: 10 nodes with forward/backward edges
    for id in 0..10u64 {
        nodes.insert(id, (id as i32, true));
        if id > 0 {
            edges.insert((id, 4), id - 1); // Prev
            edges.insert((id - 1, 5), id); // Next from previous
        }
    }

    // Navigate forward from 0
    let mut current = 0u64;
    let mut visited = vec![current];
    while let Some(&next) = edges.get(&(current, 5)) {
        current = next;
        visited.push(current);
    }
    assert_eq!(visited, (0..10).collect::<Vec<_>>());

    // Remove node 5 and its edges
    nodes.remove(&5);
    edges.remove(&(5, 4));
    edges.remove(&(5, 5));
    edges.remove(&(4, 5)); // forward from 4 to 5
    edges.remove(&(6, 4)); // backward from 6 to 5
    assert!(!nodes.contains_key(&5));
}

// ---------------------------------------------------------------------------
// 10. Proptest-style: random insert/remove/get never panics
// ---------------------------------------------------------------------------

#[test]
fn random_operations_no_panic() {
    let mut map: AHashMap<u64, u64> = AHashMap::new();
    let mut lcg = 12345u64;

    for _ in 0..10_000 {
        lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1);
        let key = lcg % 1000;
        let op = (lcg >> 32) % 3;

        match op {
            0 => {
                map.insert(key, lcg);
            }
            1 => {
                map.remove(&key);
            }
            _ => {
                let _ = map.get(&key);
            }
        }
    }
    // No panic = pass
    assert!(map.len() <= 1000);
}
