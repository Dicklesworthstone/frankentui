//! Benchmarks comparing AHashMap (Swiss Tables) vs std HashMap for widget-registry operations.
//!
//! bd-1uj0o.2: Quantitative validation that AHashMap replacement yields expected speedup.
//!
//! Run with: cargo bench -p ftui-widgets -- hashmap

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::collections::HashMap;
use std::hint::black_box;

use ahash::AHashMap;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Simulated widget ID (matches WidgetId(u64) from measure_cache).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct WidgetId(u64);

/// Simulated focus ID (matches FocusId = u64 from focus/graph).
type FocusId = u64;

/// Lightweight payload (avoids dominating measurement with value copies).
#[derive(Clone, Debug)]
struct WidgetEntry {
    _tab_index: i32,
    _bounds: [u16; 4],
}

fn make_entry(id: u64) -> WidgetEntry {
    WidgetEntry {
        _tab_index: id as i32,
        _bounds: [0, 0, (id % 200) as u16, (id % 60) as u16],
    }
}

// ---------------------------------------------------------------------------
// 1. Lookup by widget ID
// ---------------------------------------------------------------------------

fn bench_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("hashmap/lookup");

    for count in [100, 1_000, 10_000] {
        group.throughput(Throughput::Elements(count));

        // Build std HashMap
        let std_map: HashMap<WidgetId, WidgetEntry> =
            (0..count).map(|i| (WidgetId(i), make_entry(i))).collect();
        // Build AHashMap
        let ahash_map: AHashMap<WidgetId, WidgetEntry> =
            (0..count).map(|i| (WidgetId(i), make_entry(i))).collect();

        // Pseudo-random access pattern (deterministic via simple LCG).
        let keys: Vec<WidgetId> = (0..count)
            .map(|i| WidgetId((i.wrapping_mul(6364136223846793005) + 1) % count))
            .collect();

        group.bench_with_input(BenchmarkId::new("std", count), &(), |b, _| {
            b.iter(|| {
                let mut sum = 0u64;
                for k in &keys {
                    if let Some(e) = std_map.get(k) {
                        sum = sum.wrapping_add(e._tab_index as u64);
                    }
                }
                black_box(sum)
            })
        });

        group.bench_with_input(BenchmarkId::new("ahash", count), &(), |b, _| {
            b.iter(|| {
                let mut sum = 0u64;
                for k in &keys {
                    if let Some(e) = ahash_map.get(k) {
                        sum = sum.wrapping_add(e._tab_index as u64);
                    }
                }
                black_box(sum)
            })
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// 2. Insert + lookup (build then query)
// ---------------------------------------------------------------------------

fn bench_insert_then_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("hashmap/insert_lookup");

    for count in [100, 1_000, 10_000] {
        group.throughput(Throughput::Elements(count * 2)); // insert + lookup

        group.bench_with_input(BenchmarkId::new("std", count), &(), |b, _| {
            b.iter(|| {
                let mut map = HashMap::with_capacity(count as usize);
                for i in 0..count {
                    map.insert(WidgetId(i), make_entry(i));
                }
                let mut sum = 0u64;
                for i in 0..count {
                    if let Some(e) = map.get(&WidgetId(i)) {
                        sum = sum.wrapping_add(e._tab_index as u64);
                    }
                }
                black_box(sum)
            })
        });

        group.bench_with_input(BenchmarkId::new("ahash", count), &(), |b, _| {
            b.iter(|| {
                let mut map = AHashMap::with_capacity(count as usize);
                for i in 0..count {
                    map.insert(WidgetId(i), make_entry(i));
                }
                let mut sum = 0u64;
                for i in 0..count {
                    if let Some(e) = map.get(&WidgetId(i)) {
                        sum = sum.wrapping_add(e._tab_index as u64);
                    }
                }
                black_box(sum)
            })
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// 3. Mixed workload: 80% lookup, 10% insert, 10% remove
// ---------------------------------------------------------------------------

fn bench_mixed_workload(c: &mut Criterion) {
    let mut group = c.benchmark_group("hashmap/mixed");

    for count in [100, 1_000, 10_000] {
        let ops = 1000u64;
        group.throughput(Throughput::Elements(ops));

        // Pre-populate half
        let half = (count / 2) as u64;

        group.bench_with_input(BenchmarkId::new("std", count), &(), |b, _| {
            b.iter(|| {
                let mut map: HashMap<WidgetId, WidgetEntry> =
                    (0..half).map(|i| (WidgetId(i), make_entry(i))).collect();
                let mut next_id = half;
                let mut sum = 0u64;
                for op in 0..ops {
                    let choice = op % 10;
                    if choice < 8 {
                        // Lookup
                        let key = WidgetId(op % half);
                        if let Some(e) = map.get(&key) {
                            sum = sum.wrapping_add(e._tab_index as u64);
                        }
                    } else if choice < 9 {
                        // Insert
                        map.insert(WidgetId(next_id), make_entry(next_id));
                        next_id += 1;
                    } else {
                        // Remove
                        map.remove(&WidgetId(op % half));
                    }
                }
                black_box(sum)
            })
        });

        group.bench_with_input(BenchmarkId::new("ahash", count), &(), |b, _| {
            b.iter(|| {
                let mut map: AHashMap<WidgetId, WidgetEntry> =
                    (0..half).map(|i| (WidgetId(i), make_entry(i))).collect();
                let mut next_id = half;
                let mut sum = 0u64;
                for op in 0..ops {
                    let choice = op % 10;
                    if choice < 8 {
                        let key = WidgetId(op % half);
                        if let Some(e) = map.get(&key) {
                            sum = sum.wrapping_add(e._tab_index as u64);
                        }
                    } else if choice < 9 {
                        map.insert(WidgetId(next_id), make_entry(next_id));
                        next_id += 1;
                    } else {
                        map.remove(&WidgetId(op % half));
                    }
                }
                black_box(sum)
            })
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// 4. Sequential scan (iterate all entries)
// ---------------------------------------------------------------------------

fn bench_scan(c: &mut Criterion) {
    let mut group = c.benchmark_group("hashmap/scan");

    for count in [100, 1_000, 10_000] {
        group.throughput(Throughput::Elements(count));

        let std_map: HashMap<FocusId, WidgetEntry> =
            (0..count).map(|i| (i, make_entry(i))).collect();
        let ahash_map: AHashMap<FocusId, WidgetEntry> =
            (0..count).map(|i| (i, make_entry(i))).collect();

        group.bench_with_input(BenchmarkId::new("std", count), &(), |b, _| {
            b.iter(|| {
                let mut sum = 0u64;
                for (k, v) in &std_map {
                    sum = sum.wrapping_add(*k).wrapping_add(v._tab_index as u64);
                }
                black_box(sum)
            })
        });

        group.bench_with_input(BenchmarkId::new("ahash", count), &(), |b, _| {
            b.iter(|| {
                let mut sum = 0u64;
                for (k, v) in &ahash_map {
                    sum = sum.wrapping_add(*k).wrapping_add(v._tab_index as u64);
                }
                black_box(sum)
            })
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// 5. Hot key (same widget looked up repeatedly)
// ---------------------------------------------------------------------------

fn bench_hot_key(c: &mut Criterion) {
    let mut group = c.benchmark_group("hashmap/hot_key");
    let count = 1_000u64;
    let hot_key = WidgetId(42);
    let repeats = 10_000u64;
    group.throughput(Throughput::Elements(repeats));

    let std_map: HashMap<WidgetId, WidgetEntry> =
        (0..count).map(|i| (WidgetId(i), make_entry(i))).collect();
    let ahash_map: AHashMap<WidgetId, WidgetEntry> =
        (0..count).map(|i| (WidgetId(i), make_entry(i))).collect();

    group.bench_function("std", |b| {
        b.iter(|| {
            let mut sum = 0u64;
            for _ in 0..repeats {
                if let Some(e) = std_map.get(&hot_key) {
                    sum = sum.wrapping_add(e._tab_index as u64);
                }
            }
            black_box(sum)
        })
    });

    group.bench_function("ahash", |b| {
        b.iter(|| {
            let mut sum = 0u64;
            for _ in 0..repeats {
                if let Some(e) = ahash_map.get(&hot_key) {
                    sum = sum.wrapping_add(e._tab_index as u64);
                }
            }
            black_box(sum)
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// 6. String-key lookup (matches grapheme_pool, link_registry, stylesheet)
// ---------------------------------------------------------------------------

fn bench_string_key_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("hashmap/string_key");

    for count in [100, 1_000] {
        group.throughput(Throughput::Elements(count));

        let keys: Vec<String> = (0..count).map(|i| format!("style-name-{i}")).collect();

        let std_map: HashMap<String, u32> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| (k.clone(), i as u32))
            .collect();
        let ahash_map: AHashMap<String, u32> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| (k.clone(), i as u32))
            .collect();

        group.bench_with_input(BenchmarkId::new("std", count), &(), |b, _| {
            b.iter(|| {
                let mut sum = 0u32;
                for k in &keys {
                    if let Some(&v) = std_map.get(k.as_str()) {
                        sum = sum.wrapping_add(v);
                    }
                }
                black_box(sum)
            })
        });

        group.bench_with_input(BenchmarkId::new("ahash", count), &(), |b, _| {
            b.iter(|| {
                let mut sum = 0u32;
                for k in &keys {
                    if let Some(&v) = ahash_map.get(k.as_str()) {
                        sum = sum.wrapping_add(v);
                    }
                }
                black_box(sum)
            })
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_lookup,
    bench_insert_then_lookup,
    bench_mixed_workload,
    bench_scan,
    bench_hot_key,
    bench_string_key_lookup,
);
criterion_main!(benches);
