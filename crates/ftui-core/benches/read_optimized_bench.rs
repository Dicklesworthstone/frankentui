//! Benchmark: ArcSwapStore vs RwLockStore vs MutexStore.
//!
//! Run with: `cargo bench -p ftui-core --bench read_optimized_bench`
//!
//! Measures single-threaded read latency and multi-threaded read throughput
//! under concurrent write pressure, matching the read-99%/write-1% pattern
//! of theme and capability data in FrankenTUI.

use std::sync::{Arc, Barrier};
use std::thread;

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use ftui_core::read_optimized::{ArcSwapStore, MutexStore, ReadOptimized, RwLockStore};

/// Simulates TerminalCapabilities (small Copy struct, ~20 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SmallCaps {
    true_color: bool,
    colors_256: bool,
    sync_output: bool,
    scroll_region: bool,
    mouse_sgr: bool,
}

impl SmallCaps {
    fn sample() -> Self {
        Self {
            true_color: true,
            colors_256: true,
            sync_output: true,
            scroll_region: false,
            mouse_sgr: true,
        }
    }
}

/// Simulates ResolvedTheme (medium Copy struct, ~76 bytes = 19 × 4-byte Color).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MediumTheme {
    slots: [u32; 19],
}

impl MediumTheme {
    fn sample() -> Self {
        Self {
            slots: [0xFF_AA_BB_CC; 19],
        }
    }
}

// ===========================================================================
// Single-threaded read latency
// ===========================================================================

fn bench_single_thread_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("single_thread_read");

    // -- Small type (TerminalCapabilities-like) --
    {
        let arcswap = ArcSwapStore::new(SmallCaps::sample());
        let rwlock = RwLockStore::new(SmallCaps::sample());
        let mutex = MutexStore::new(SmallCaps::sample());

        group.bench_function("small/arcswap", |b| {
            b.iter(|| black_box(arcswap.load()));
        });
        group.bench_function("small/rwlock", |b| {
            b.iter(|| black_box(rwlock.load()));
        });
        group.bench_function("small/mutex", |b| {
            b.iter(|| black_box(mutex.load()));
        });
    }

    // -- Medium type (ResolvedTheme-like) --
    {
        let arcswap = ArcSwapStore::new(MediumTheme::sample());
        let rwlock = RwLockStore::new(MediumTheme::sample());
        let mutex = MutexStore::new(MediumTheme::sample());

        group.bench_function("medium/arcswap", |b| {
            b.iter(|| black_box(arcswap.load()));
        });
        group.bench_function("medium/rwlock", |b| {
            b.iter(|| black_box(rwlock.load()));
        });
        group.bench_function("medium/mutex", |b| {
            b.iter(|| black_box(mutex.load()));
        });
    }

    group.finish();
}

// ===========================================================================
// Multi-threaded read throughput (8 readers, 0 writers)
// ===========================================================================

fn bench_multi_thread_read_only(c: &mut Criterion) {
    let mut group = c.benchmark_group("multi_thread_read_only_8r");
    let num_readers = 8;
    let reads_per_thread = 100_000;

    // -- ArcSwap --
    group.bench_function("small/arcswap", |b| {
        b.iter(|| {
            let store = Arc::new(ArcSwapStore::new(SmallCaps::sample()));
            let barrier = Arc::new(Barrier::new(num_readers));
            let handles: Vec<_> = (0..num_readers)
                .map(|_| {
                    let s = Arc::clone(&store);
                    let bar = Arc::clone(&barrier);
                    thread::spawn(move || {
                        bar.wait();
                        for _ in 0..reads_per_thread {
                            black_box(s.load());
                        }
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap();
            }
        });
    });

    // -- RwLock --
    group.bench_function("small/rwlock", |b| {
        b.iter(|| {
            let store = Arc::new(RwLockStore::new(SmallCaps::sample()));
            let barrier = Arc::new(Barrier::new(num_readers));
            let handles: Vec<_> = (0..num_readers)
                .map(|_| {
                    let s = Arc::clone(&store);
                    let bar = Arc::clone(&barrier);
                    thread::spawn(move || {
                        bar.wait();
                        for _ in 0..reads_per_thread {
                            black_box(s.load());
                        }
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap();
            }
        });
    });

    // -- Mutex --
    group.bench_function("small/mutex", |b| {
        b.iter(|| {
            let store = Arc::new(MutexStore::new(SmallCaps::sample()));
            let barrier = Arc::new(Barrier::new(num_readers));
            let handles: Vec<_> = (0..num_readers)
                .map(|_| {
                    let s = Arc::clone(&store);
                    let bar = Arc::clone(&barrier);
                    thread::spawn(move || {
                        bar.wait();
                        for _ in 0..reads_per_thread {
                            black_box(s.load());
                        }
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap();
            }
        });
    });

    group.finish();
}

// ===========================================================================
// Multi-threaded mixed: 8 readers + 1 writer (99:1 read/write ratio)
// ===========================================================================

fn bench_multi_thread_mixed(c: &mut Criterion) {
    let mut group = c.benchmark_group("multi_thread_mixed_8r1w");
    let num_readers = 8;
    let reads_per_thread = 100_000;
    let writes_total = reads_per_thread / 100; // 1% writes

    // -- ArcSwap --
    group.bench_function("small/arcswap", |b| {
        b.iter(|| {
            let store = Arc::new(ArcSwapStore::new(SmallCaps::sample()));
            let barrier = Arc::new(Barrier::new(num_readers + 1));

            let writer = {
                let s = Arc::clone(&store);
                let bar = Arc::clone(&barrier);
                thread::spawn(move || {
                    bar.wait();
                    for _ in 0..writes_total {
                        s.store(SmallCaps::sample());
                        // Space writes apart to simulate real usage.
                        thread::yield_now();
                    }
                })
            };

            let readers: Vec<_> = (0..num_readers)
                .map(|_| {
                    let s = Arc::clone(&store);
                    let bar = Arc::clone(&barrier);
                    thread::spawn(move || {
                        bar.wait();
                        for _ in 0..reads_per_thread {
                            black_box(s.load());
                        }
                    })
                })
                .collect();

            writer.join().unwrap();
            for h in readers {
                h.join().unwrap();
            }
        });
    });

    // -- RwLock --
    group.bench_function("small/rwlock", |b| {
        b.iter(|| {
            let store = Arc::new(RwLockStore::new(SmallCaps::sample()));
            let barrier = Arc::new(Barrier::new(num_readers + 1));

            let writer = {
                let s = Arc::clone(&store);
                let bar = Arc::clone(&barrier);
                thread::spawn(move || {
                    bar.wait();
                    for _ in 0..writes_total {
                        s.store(SmallCaps::sample());
                        thread::yield_now();
                    }
                })
            };

            let readers: Vec<_> = (0..num_readers)
                .map(|_| {
                    let s = Arc::clone(&store);
                    let bar = Arc::clone(&barrier);
                    thread::spawn(move || {
                        bar.wait();
                        for _ in 0..reads_per_thread {
                            black_box(s.load());
                        }
                    })
                })
                .collect();

            writer.join().unwrap();
            for h in readers {
                h.join().unwrap();
            }
        });
    });

    // -- Mutex --
    group.bench_function("small/mutex", |b| {
        b.iter(|| {
            let store = Arc::new(MutexStore::new(SmallCaps::sample()));
            let barrier = Arc::new(Barrier::new(num_readers + 1));

            let writer = {
                let s = Arc::clone(&store);
                let bar = Arc::clone(&barrier);
                thread::spawn(move || {
                    bar.wait();
                    for _ in 0..writes_total {
                        s.store(SmallCaps::sample());
                        thread::yield_now();
                    }
                })
            };

            let readers: Vec<_> = (0..num_readers)
                .map(|_| {
                    let s = Arc::clone(&store);
                    let bar = Arc::clone(&barrier);
                    thread::spawn(move || {
                        bar.wait();
                        for _ in 0..reads_per_thread {
                            black_box(s.load());
                        }
                    })
                })
                .collect();

            writer.join().unwrap();
            for h in readers {
                h.join().unwrap();
            }
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_single_thread_read,
    bench_multi_thread_read_only,
    bench_multi_thread_mixed,
);
criterion_main!(benches);
