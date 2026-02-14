//! Benchmarks comparing S3-FIFO vs W-TinyLFU vs LRU width caches (bd-l6yba.4).
//!
//! Run with: cargo bench -p ftui-text --bench cache_bench
//!
//! Workloads:
//! - **Zipfian**: Power-law access (80/20 rule). Hot strings accessed often.
//! - **Scan**: Sequential unique strings, simulating one-time scroll.
//! - **Mixed**: Hot working set + scan noise.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use ftui_text::{S3FifoWidthCache, TinyLfuWidthCache, WidthCache};
use std::hint::black_box;

// ── Workload Generators ─────────────────────────────────────────────────

/// Generate strings following a Zipfian-like distribution (hot keys hit often).
fn zipfian_workload(size: usize) -> Vec<String> {
    let mut work = Vec::with_capacity(size);
    for i in 0..size {
        // 80% of accesses hit 20% of keys
        let key = if i % 5 == 0 {
            // Cold: unique keys
            format!("cold_string_{i}")
        } else {
            // Hot: cycle through 20 frequent keys
            format!("hot_key_{}", i % 20)
        };
        work.push(key);
    }
    work
}

/// Generate sequential unique strings (scan workload — worst case for LRU).
fn scan_workload(size: usize) -> Vec<String> {
    (0..size).map(|i| format!("scan_unique_{i}")).collect()
}

/// Mixed workload: hot working set + scan noise.
fn mixed_workload(size: usize) -> Vec<String> {
    let mut work = Vec::with_capacity(size);
    for i in 0..size {
        if i % 3 == 0 {
            // Scan noise
            work.push(format!("noise_{i}"));
        } else {
            // Hot set of 50 keys
            work.push(format!("working_set_{}", i % 50));
        }
    }
    work
}

// ── Benchmark Functions ─────────────────────────────────────────────────

fn bench_lru(c: &mut Criterion) {
    let mut group = c.benchmark_group("width_cache/lru");

    for (name, workload_fn) in [
        ("zipfian", zipfian_workload as fn(usize) -> Vec<String>),
        ("scan", scan_workload),
        ("mixed", mixed_workload),
    ] {
        for &cap in &[1_000, 10_000, 100_000] {
            let work = workload_fn(cap);
            group.bench_with_input(BenchmarkId::new(name, cap), &work, |b, work| {
                b.iter(|| {
                    let mut cache = WidthCache::new(cap / 10);
                    for s in work {
                        black_box(cache.get_or_compute(s));
                    }
                    black_box(cache.stats())
                });
            });
        }
    }
    group.finish();
}

fn bench_tinylfu(c: &mut Criterion) {
    let mut group = c.benchmark_group("width_cache/tinylfu");

    for (name, workload_fn) in [
        ("zipfian", zipfian_workload as fn(usize) -> Vec<String>),
        ("scan", scan_workload),
        ("mixed", mixed_workload),
    ] {
        for &cap in &[1_000, 10_000, 100_000] {
            let work = workload_fn(cap);
            group.bench_with_input(BenchmarkId::new(name, cap), &work, |b, work| {
                b.iter(|| {
                    let mut cache = TinyLfuWidthCache::new(cap / 10);
                    for s in work {
                        black_box(cache.get_or_compute(s));
                    }
                    black_box(cache.stats())
                });
            });
        }
    }
    group.finish();
}

fn bench_s3fifo(c: &mut Criterion) {
    let mut group = c.benchmark_group("width_cache/s3fifo");

    for (name, workload_fn) in [
        ("zipfian", zipfian_workload as fn(usize) -> Vec<String>),
        ("scan", scan_workload),
        ("mixed", mixed_workload),
    ] {
        for &cap in &[1_000, 10_000, 100_000] {
            let work = workload_fn(cap);
            group.bench_with_input(BenchmarkId::new(name, cap), &work, |b, work| {
                b.iter(|| {
                    let mut cache = S3FifoWidthCache::new(cap / 10);
                    for s in work {
                        black_box(cache.get_or_compute(s));
                    }
                    black_box(cache.stats())
                });
            });
        }
    }
    group.finish();
}

/// Hit rate comparison (not timed — reports hit rates for analysis).
fn bench_hit_rates(c: &mut Criterion) {
    let mut group = c.benchmark_group("width_cache/hit_rate_comparison");
    group.sample_size(10); // Fewer samples since we care about the stats, not timing

    for &cap in &[1_000, 10_000] {
        let zipf = zipfian_workload(cap);
        let scan = scan_workload(cap);
        let mixed = mixed_workload(cap);

        for (name, work) in [("zipfian", &zipf), ("scan", &scan), ("mixed", &mixed)] {
            group.bench_with_input(
                BenchmarkId::new(format!("all/{name}"), cap),
                work,
                |b, work| {
                    b.iter(|| {
                        let cache_cap = cap / 10;

                        let mut lru = WidthCache::new(cache_cap);
                        let mut tlfu = TinyLfuWidthCache::new(cache_cap);
                        let mut s3 = S3FifoWidthCache::new(cache_cap);

                        for s in work.iter() {
                            lru.get_or_compute(s);
                            tlfu.get_or_compute(s);
                            s3.get_or_compute(s);
                        }

                        black_box((lru.stats(), tlfu.stats(), s3.stats()))
                    });
                },
            );
        }
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_lru,
    bench_tinylfu,
    bench_s3fifo,
    bench_hit_rates
);
criterion_main!(benches);
