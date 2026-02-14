//! Benchmarks for cell, buffer, and diff operations (bd-19x, bd-2m5)
//!
//! Run with: cargo bench -p ftui-render --bench diff_bench

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, CellAttrs, PackedRgba, StyleFlags};
use ftui_render::diff::{BufferDiff, TileDiffStats};
use ftui_render::diff_strategy::{DiffStrategy, DiffStrategySelector};
use std::hint::black_box;
use std::time::{Duration, Instant};

type DiffFn = fn(&Buffer, &Buffer) -> BufferDiff;
type DiffMethod = (&'static str, DiffFn);

struct DiffStats {
    p50_us: u64,
    p95_us: u64,
    total_us: u128,
}

/// Create a pair of buffers where only `pct` percent of cells differ.
fn make_pair(width: u16, height: u16, change_pct: f64) -> (Buffer, Buffer) {
    let mut old = Buffer::new(width, height);
    let mut new = old.clone();
    old.clear_dirty();
    new.clear_dirty();

    let total = width as usize * height as usize;
    let to_change = ((total as f64) * change_pct / 100.0) as usize;

    for i in 0..to_change {
        let x = (i * 7 + 3) as u16 % width;
        let y = (i * 11 + 5) as u16 % height;
        let ch = char::from_u32(('A' as u32) + (i as u32 % 26)).unwrap();
        new.set_raw(
            x,
            y,
            Cell::from_char(ch).with_fg(PackedRgba::rgb(255, 0, 0)),
        );
    }

    (old, new)
}

fn measure_diff_stats(iters: u64, old: &Buffer, new: &Buffer, diff_fn: DiffFn) -> DiffStats {
    let mut times = Vec::with_capacity(iters as usize);
    let mut total_us: u128 = 0;

    for _ in 0..iters {
        let start = Instant::now();
        let diff = diff_fn(old, new);
        black_box(diff.len());
        let elapsed = start.elapsed().as_micros() as u64;
        total_us += elapsed as u128;
        times.push(elapsed);
    }

    times.sort_unstable();
    let len = times.len().max(1);
    let p50_idx = len / 2;
    let p95_idx = ((len as f64 * 0.95) as usize).min(len.saturating_sub(1));

    DiffStats {
        p50_us: times[p50_idx],
        p95_us: times[p95_idx],
        total_us,
    }
}

fn measure_diff_stats_dirty(
    iters: u64,
    old: &Buffer,
    new: &Buffer,
    diff: &mut BufferDiff,
) -> (DiffStats, Option<TileDiffStats>) {
    let mut times = Vec::with_capacity(iters as usize);
    let mut total_us: u128 = 0;
    let mut last_tile_stats = None;

    for _ in 0..iters {
        let start = Instant::now();
        diff.compute_dirty_into(old, new);
        black_box(diff.len());
        let elapsed = start.elapsed().as_micros() as u64;
        total_us += elapsed as u128;
        times.push(elapsed);
        last_tile_stats = diff.last_tile_stats();
    }

    times.sort_unstable();
    let len = times.len().max(1);
    let p50_idx = len / 2;
    let p95_idx = ((len as f64 * 0.95) as usize).min(len.saturating_sub(1));

    (
        DiffStats {
            p50_us: times[p50_idx],
            p95_us: times[p95_idx],
            total_us,
        },
        last_tile_stats,
    )
}

fn measure_diff_stats_dense_gate(
    iters: u64,
    old: &Buffer,
    new: &Buffer,
    diff: &mut BufferDiff,
) -> (DiffStats, DiffStats, Option<TileDiffStats>) {
    // Interleave full vs dirty measurements to reduce drift (turbo/thermal/load)
    // from causing flaky ratio assertions in short-run CI benches.
    let mut full_times = Vec::with_capacity(iters as usize);
    let mut dirty_times = Vec::with_capacity(iters as usize);
    let mut full_total_us: u128 = 0;
    let mut dirty_total_us: u128 = 0;
    let mut last_tile_stats = None;

    for i in 0..iters {
        let full_first = (i & 1) == 0;

        if full_first {
            let start = Instant::now();
            let full = BufferDiff::compute(old, new);
            black_box(full.len());
            let elapsed = start.elapsed().as_micros() as u64;
            full_total_us += elapsed as u128;
            full_times.push(elapsed);

            let start = Instant::now();
            diff.compute_dirty_into(old, new);
            black_box(diff.len());
            let elapsed = start.elapsed().as_micros() as u64;
            dirty_total_us += elapsed as u128;
            dirty_times.push(elapsed);
            last_tile_stats = diff.last_tile_stats();
        } else {
            let start = Instant::now();
            diff.compute_dirty_into(old, new);
            black_box(diff.len());
            let elapsed = start.elapsed().as_micros() as u64;
            dirty_total_us += elapsed as u128;
            dirty_times.push(elapsed);
            last_tile_stats = diff.last_tile_stats();

            let start = Instant::now();
            let full = BufferDiff::compute(old, new);
            black_box(full.len());
            let elapsed = start.elapsed().as_micros() as u64;
            full_total_us += elapsed as u128;
            full_times.push(elapsed);
        }
    }

    full_times.sort_unstable();
    dirty_times.sort_unstable();

    let len = full_times.len().max(1);
    let p50_idx = len / 2;
    let p95_idx = ((len as f64 * 0.95) as usize).min(len.saturating_sub(1));

    (
        DiffStats {
            p50_us: full_times[p50_idx],
            p95_us: full_times[p95_idx],
            total_us: full_total_us,
        },
        DiffStats {
            p50_us: dirty_times[p50_idx],
            p95_us: dirty_times[p95_idx],
            total_us: dirty_total_us,
        },
        last_tile_stats,
    )
}

fn bench_diff_identical(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff/identical");

    for (w, h) in [(80, 24), (120, 40), (200, 60)] {
        let cells = w as u64 * h as u64;
        group.throughput(Throughput::Elements(cells));
        let (old, new) = make_pair(w, h, 0.0);
        group.bench_with_input(
            BenchmarkId::new("compute", format!("{w}x{h}")),
            &(),
            |b, _| b.iter(|| black_box(BufferDiff::compute(&old, &new))),
        );
    }

    group.finish();
}

fn bench_diff_sparse(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff/sparse_5pct");

    for (w, h) in [(80, 24), (120, 40), (200, 60)] {
        let cells = w as u64 * h as u64;
        group.throughput(Throughput::Elements(cells));
        let (old, new) = make_pair(w, h, 5.0);
        group.bench_with_input(
            BenchmarkId::new("compute", format!("{w}x{h}")),
            &(),
            |b, _| b.iter(|| black_box(BufferDiff::compute(&old, &new))),
        );
    }

    group.finish();
}

fn bench_diff_heavy(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff/heavy_50pct");

    for (w, h) in [(80, 24), (120, 40), (200, 60)] {
        let cells = w as u64 * h as u64;
        group.throughput(Throughput::Elements(cells));
        let (old, new) = make_pair(w, h, 50.0);
        group.bench_with_input(
            BenchmarkId::new("compute", format!("{w}x{h}")),
            &(),
            |b, _| b.iter(|| black_box(BufferDiff::compute(&old, &new))),
        );
    }

    group.finish();
}

fn bench_diff_full(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff/full_100pct");

    for (w, h) in [(80, 24), (120, 40), (200, 60)] {
        let cells = w as u64 * h as u64;
        group.throughput(Throughput::Elements(cells));
        let (old, new) = make_pair(w, h, 100.0);
        group.bench_with_input(
            BenchmarkId::new("compute", format!("{w}x{h}")),
            &(),
            |b, _| b.iter(|| black_box(BufferDiff::compute(&old, &new))),
        );
    }

    group.finish();
}

fn bench_diff_runs(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff/runs");

    for (w, h, pct) in [(80, 24, 5.0), (80, 24, 50.0), (200, 60, 5.0)] {
        let (old, new) = make_pair(w, h, pct);
        let diff = BufferDiff::compute(&old, &new);
        group.bench_with_input(
            BenchmarkId::new("coalesce", format!("{w}x{h}@{pct}%")),
            &diff,
            |b, diff| b.iter(|| black_box(diff.runs())),
        );
    }

    group.finish();
}

// ============================================================================
// Full vs Dirty diff comparison (bd-3e1t.1.6)
// ============================================================================

/// Compare compute() vs compute_dirty() on sparse changes.
/// This validates that dirty-row optimization provides speedup on large screens.
fn bench_full_vs_dirty(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff/full_vs_dirty");

    // Large screen sizes as specified in bd-3e1t.1.6
    for (w, h) in [(200, 60), (240, 80)] {
        let cells = w as u64 * h as u64;
        group.throughput(Throughput::Elements(cells));

        // Sparse 2% changes - representative of large-screen micro-updates
        let (old_sparse, new_sparse) = make_pair(w, h, 2.0);

        group.bench_with_input(
            BenchmarkId::new("compute", format!("{w}x{h}@2%")),
            &(&old_sparse, &new_sparse),
            |b, (old, new)| b.iter(|| black_box(BufferDiff::compute(old, new))),
        );

        group.bench_with_input(
            BenchmarkId::new("compute_dirty", format!("{w}x{h}@2%")),
            &(&old_sparse, &new_sparse),
            |b, (old, new)| b.iter(|| black_box(BufferDiff::compute_dirty(old, new))),
        );

        // Sparse 5% changes - dirty diff should win
        let (old, new) = make_pair(w, h, 5.0);

        group.bench_with_input(
            BenchmarkId::new("compute", format!("{w}x{h}@5%")),
            &(&old, &new),
            |b, (old, new)| b.iter(|| black_box(BufferDiff::compute(old, new))),
        );

        group.bench_with_input(
            BenchmarkId::new("compute_dirty", format!("{w}x{h}@5%")),
            &(&old, &new),
            |b, (old, new)| b.iter(|| black_box(BufferDiff::compute_dirty(old, new))),
        );

        // Single-row change - dirty diff should massively win
        let mut single_row = old.clone();
        for x in 0..w {
            single_row.set_raw(x, 0, Cell::from_char('X').with_fg(PackedRgba::RED));
        }

        group.bench_with_input(
            BenchmarkId::new("compute", format!("{w}x{h}@1row")),
            &(&old, &single_row),
            |b, (old, new)| b.iter(|| black_box(BufferDiff::compute(old, new))),
        );

        group.bench_with_input(
            BenchmarkId::new("compute_dirty", format!("{w}x{h}@1row")),
            &(&old, &single_row),
            |b, (old, new)| b.iter(|| black_box(BufferDiff::compute_dirty(old, new))),
        );
    }

    group.finish();
}

// ============================================================================
// Selector overhead + selector vs fixed strategy benches (bd-3e1t.8.4)
// ============================================================================

fn bench_selector_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff/selector_overhead");

    let scenarios = [
        (200u16, 60u16, 2usize, 0.02f64, "200x60@2%"),
        (200u16, 60u16, 30usize, 0.5f64, "200x60@50%"),
        (240u16, 80u16, 3usize, 0.02f64, "240x80@2%"),
        (240u16, 80u16, 40usize, 0.35f64, "240x80@35%"),
    ];

    for (w, h, dirty_rows, p_actual, label) in scenarios {
        group.bench_with_input(BenchmarkId::new("select_observe", label), &(), |b, _| {
            b.iter_batched(
                DiffStrategySelector::with_defaults,
                |mut selector| {
                    let total_cells = w as usize * h as usize;
                    let changed =
                        ((p_actual * total_cells as f64).round() as usize).min(total_cells);
                    for _ in 0..64 {
                        let strategy = selector.select(w, h, dirty_rows);
                        let scanned = match strategy {
                            DiffStrategy::Full => total_cells,
                            DiffStrategy::DirtyRows => dirty_rows.saturating_mul(w as usize),
                            DiffStrategy::FullRedraw => 0,
                        };
                        if strategy != DiffStrategy::FullRedraw {
                            selector.observe(scanned, changed);
                        }
                        black_box(strategy);
                    }
                },
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

fn bench_selector_vs_fixed(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff/selector_vs_fixed");

    let scenarios = [
        (200u16, 60u16, 2.0f64),
        (200u16, 60u16, 50.0f64),
        (240u16, 80u16, 2.0f64),
        (240u16, 80u16, 35.0f64),
    ];

    for (w, h, pct) in scenarios {
        let (old, new) = make_pair(w, h, pct);
        let dirty_rows = new.dirty_row_count();
        let label = format!("{w}x{h}@{pct}%");

        group.bench_with_input(BenchmarkId::new("fixed_full", &label), &(), |b, _| {
            b.iter(|| black_box(BufferDiff::compute(&old, &new)));
        });

        group.bench_with_input(BenchmarkId::new("fixed_dirty", &label), &(), |b, _| {
            b.iter(|| black_box(BufferDiff::compute_dirty(&old, &new)));
        });

        group.bench_with_input(BenchmarkId::new("selector", &label), &(), |b, _| {
            b.iter_batched(
                DiffStrategySelector::with_defaults,
                |mut selector| {
                    let total_cells = w as usize * h as usize;
                    let strategy = selector.select(w, h, dirty_rows);
                    match strategy {
                        DiffStrategy::Full => {
                            let diff = BufferDiff::compute(&old, &new);
                            selector.observe(total_cells, diff.len());
                            black_box(diff.len());
                        }
                        DiffStrategy::DirtyRows => {
                            let diff = BufferDiff::compute_dirty(&old, &new);
                            let scanned = dirty_rows.saturating_mul(w as usize);
                            selector.observe(scanned, diff.len());
                            black_box(diff.len());
                        }
                        DiffStrategy::FullRedraw => {
                            black_box(0usize);
                        }
                    }
                },
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

// ============================================================================
// Span-aware dirty diff stats + regression gate (bd-3e1t.6.5)
// ============================================================================

/// Record p50/p95 and throughput for compute vs compute_dirty on sparse cases.
fn bench_diff_span_sparse_stats(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff/span_sparse_stats");
    let methods: &[DiffMethod] = &[
        ("compute", BufferDiff::compute),
        ("compute_dirty", BufferDiff::compute_dirty),
    ];

    for (w, h) in [(200, 60), (240, 80)] {
        let cells = w as u64 * h as u64;
        group.throughput(Throughput::Elements(cells));

        for (label, pct) in [("sparse_5pct", Some(5.0)), ("single_row", None)] {
            let (old, new) = if let Some(pct) = pct {
                make_pair(w, h, pct)
            } else {
                let old = Buffer::new(w, h);
                let mut new = old.clone();
                for x in 0..w {
                    new.set_raw(x, 0, Cell::from_char('X'));
                }
                (old, new)
            };

            for (name, diff_fn) in methods {
                let bench_id = BenchmarkId::new(*name, format!("{w}x{h}@{label}"));
                group.bench_with_input(bench_id, &(&old, &new), |b, (old, new)| {
                    b.iter_custom(|iters| {
                        let stats = measure_diff_stats(iters, old, new, *diff_fn);
                        let throughput = if stats.total_us > 0 {
                            (cells as f64 * iters as f64) / (stats.total_us as f64 / 1_000_000.0)
                        } else {
                            0.0
                        };
                        eprintln!(
                            "{{\"event\":\"diff_span_bench\",\"case\":\"{label}\",\"method\":\"{name}\",\"width\":{w},\"height\":{h},\"iters\":{iters},\"p50_us\":{},\"p95_us\":{},\"throughput_cells_per_s\":{:.2}}}",
                            stats.p50_us,
                            stats.p95_us,
                            throughput
                        );
                        let total_us = stats.total_us.min(u128::from(u64::MAX)) as u64;
                        Duration::from_micros(total_us)
                    })
                });
            }
        }
    }

    group.finish();
}

/// Dense-case regression gate: dirty diff must stay within a small overhead.
fn bench_diff_span_dense_regression(c: &mut Criterion) {
    let max_overhead = std::env::var("FTUI_SPAN_DENSE_MAX_OVERHEAD")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(1.02);

    let mut group = c.benchmark_group("diff/span_dense_regression");

    for (w, h) in [(200, 60), (240, 80)] {
        let cells = w as u64 * h as u64;
        group.throughput(Throughput::Elements(cells));
        let (old, new) = make_pair(w, h, 50.0);

        group.bench_with_input(
            BenchmarkId::new("dense_gate", format!("{w}x{h}@50%")),
            &(&old, &new),
            |b, (old, new)| {
                b.iter_custom(|iters| {
                    let full_stats = measure_diff_stats(iters, old, new, BufferDiff::compute);
                    let dirty_stats =
                        measure_diff_stats(iters, old, new, BufferDiff::compute_dirty);

                    let denom = full_stats.p50_us.max(1) as f64;
                    let ratio = dirty_stats.p50_us as f64 / denom;

                    eprintln!(
                        "{{\"event\":\"diff_span_dense_gate\",\"width\":{w},\"height\":{h},\"iters\":{iters},\"full_p50_us\":{},\"dirty_p50_us\":{},\"ratio\":{:.3},\"max_overhead\":{:.3}}}",
                        full_stats.p50_us,
                        dirty_stats.p50_us,
                        ratio,
                        max_overhead
                    );

                    assert!(
                        ratio <= max_overhead,
                        "span dirty diff regression: {w}x{h} dense ratio {ratio:.3} exceeds {max_overhead:.3}"
                    );

                    let total_us =
                        (full_stats.total_us + dirty_stats.total_us).min(u128::from(u64::MAX))
                            as u64;
                    Duration::from_micros(total_us)
                })
            },
        );
    }

    group.finish();
}

// ============================================================================
// Tile-skip large-screen stats + dense regression gate (bd-3e1t.7.5)
// ============================================================================

fn bench_diff_tile_sparse_stats(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff/tile_sparse_stats");

    for (w, h) in [(320u16, 90u16), (400u16, 100u16)] {
        let cells = w as u64 * h as u64;
        group.throughput(Throughput::Elements(cells));

        for pct in [1.0f64, 2.0f64] {
            let (old, new) = make_pair(w, h, pct);
            let label = format!("{w}x{h}@{pct}%");

            group.bench_with_input(BenchmarkId::new("compute", &label), &(&old, &new), |b, (old, new)| {
                b.iter_custom(|iters| {
                    let stats = measure_diff_stats(iters, old, new, BufferDiff::compute);
                    let throughput = if stats.total_us > 0 {
                        (cells as f64 * iters as f64) / (stats.total_us as f64 / 1_000_000.0)
                    } else {
                        0.0
                    };
                    eprintln!(
                        "{{\"event\":\"diff_tile_sparse_bench\",\"method\":\"compute\",\"width\":{w},\"height\":{h},\"pct\":{pct},\"iters\":{iters},\"p50_us\":{},\"p95_us\":{},\"throughput_cells_per_s\":{:.2}}}",
                        stats.p50_us,
                        stats.p95_us,
                        throughput
                    );
                    let total_us = stats.total_us.min(u128::from(u64::MAX)) as u64;
                    Duration::from_micros(total_us)
                })
            });

            group.bench_with_input(
                BenchmarkId::new("compute_dirty", &label),
                &(&old, &new),
                |b, (old, new)| {
                    b.iter_custom(|iters| {
                        let mut diff = BufferDiff::new();
                        let (stats, tile_stats) = measure_diff_stats_dirty(iters, old, new, &mut diff);
                        let throughput = if stats.total_us > 0 {
                            (cells as f64 * iters as f64) / (stats.total_us as f64 / 1_000_000.0)
                        } else {
                            0.0
                        };
                        let (tile_w, tile_h, tiles_x, tiles_y, dirty_tiles, skipped_tiles, dirty_tile_ratio, fallback) =
                            if let Some(tile_stats) = tile_stats {
                                (
                                    tile_stats.tile_w,
                                    tile_stats.tile_h,
                                    tile_stats.tiles_x,
                                    tile_stats.tiles_y,
                                    tile_stats.dirty_tiles,
                                    tile_stats.skipped_tiles,
                                    tile_stats.dirty_tile_ratio,
                                    tile_stats
                                        .fallback
                                        .map(|reason| reason.as_str())
                                        .unwrap_or("none"),
                                )
                            } else {
                                (0, 0, 0, 0, 0, 0, 0.0, "none")
                            };
                        eprintln!(
                            "{{\"event\":\"diff_tile_sparse_bench\",\"method\":\"compute_dirty\",\"width\":{w},\"height\":{h},\"pct\":{pct},\"iters\":{iters},\"p50_us\":{},\"p95_us\":{},\"throughput_cells_per_s\":{:.2},\"tile_w\":{tile_w},\"tile_h\":{tile_h},\"tiles_x\":{tiles_x},\"tiles_y\":{tiles_y},\"dirty_tiles\":{dirty_tiles},\"skipped_tiles\":{skipped_tiles},\"dirty_tile_ratio\":{dirty_tile_ratio:.4},\"fallback\":\"{fallback}\"}}",
                            stats.p50_us,
                            stats.p95_us,
                            throughput
                        );
                        let total_us = stats.total_us.min(u128::from(u64::MAX)) as u64;
                        Duration::from_micros(total_us)
                    })
                },
            );
        }
    }

    group.finish();
}

fn bench_diff_tile_dense_regression(c: &mut Criterion) {
    let max_overhead = std::env::var("FTUI_TILE_DENSE_MAX_OVERHEAD")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(1.02);

    let mut group = c.benchmark_group("diff/tile_dense_regression");

    for (w, h) in [(320u16, 90u16), (400u16, 100u16)] {
        let cells = w as u64 * h as u64;
        group.throughput(Throughput::Elements(cells));
        let (old, new) = make_pair(w, h, 50.0);

        group.bench_with_input(
            BenchmarkId::new("dense_gate", format!("{w}x{h}@50%")),
            &(&old, &new),
            |b, (old, new)| {
                b.iter_custom(|iters| {
                    let mut diff = BufferDiff::new();
                    let (full_stats, dirty_stats, tile_stats) =
                        measure_diff_stats_dense_gate(iters, old, new, &mut diff);

                    let denom = full_stats.p50_us.max(1) as f64;
                    let ratio = dirty_stats.p50_us as f64 / denom;
                    let fallback = tile_stats
                        .and_then(|stats| stats.fallback.map(|reason| reason.as_str()))
                        .unwrap_or("none");

                    eprintln!(
                        "{{\"event\":\"diff_tile_dense_gate\",\"width\":{w},\"height\":{h},\"iters\":{iters},\"full_p50_us\":{},\"dirty_p50_us\":{},\"ratio\":{ratio:.3},\"max_overhead\":{max_overhead:.3},\"fallback\":\"{fallback}\"}}",
                        full_stats.p50_us,
                        dirty_stats.p50_us
                    );

                    assert!(
                        ratio <= max_overhead,
                        "tile dirty diff regression: {w}x{h} dense ratio {ratio:.3} exceeds {max_overhead:.3}"
                    );

                    let total_us =
                        (full_stats.total_us + dirty_stats.total_us).min(u128::from(u64::MAX))
                            as u64;
                    Duration::from_micros(total_us)
                })
            },
        );
    }

    group.finish();
}

/// Large screen benchmarks for regression detection.
fn bench_diff_large_screen(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff/large_screen");

    // Test 4K-like terminal sizes
    for (w, h) in [(320, 90), (400, 100)] {
        let cells = w as u64 * h as u64;
        group.throughput(Throughput::Elements(cells));

        // Sparse changes (typical use case)
        let (old, new) = make_pair(w, h, 2.0);

        group.bench_with_input(
            BenchmarkId::new("compute", format!("{w}x{h}@2%")),
            &(&old, &new),
            |b, (old, new)| b.iter(|| black_box(BufferDiff::compute(old, new))),
        );

        group.bench_with_input(
            BenchmarkId::new("compute_dirty", format!("{w}x{h}@2%")),
            &(&old, &new),
            |b, (old, new)| b.iter(|| black_box(BufferDiff::compute_dirty(old, new))),
        );
    }

    group.finish();
}

fn bench_bits_eq(c: &mut Criterion) {
    let mut group = c.benchmark_group("cell/bits_eq");

    let cell_a = Cell::from_char('A').with_fg(PackedRgba::rgb(255, 0, 0));
    let cell_b = Cell::from_char('A').with_fg(PackedRgba::rgb(255, 0, 0));
    let cell_c = Cell::from_char('B').with_fg(PackedRgba::rgb(0, 255, 0));

    group.bench_function("equal", |b| b.iter(|| black_box(cell_a.bits_eq(&cell_b))));

    group.bench_function("different", |b| {
        b.iter(|| black_box(cell_a.bits_eq(&cell_c)))
    });

    group.finish();
}

fn bench_row_cells(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer/row_cells");
    let buf = Buffer::new(200, 60);

    group.bench_function("200x60_all_rows", |b| {
        b.iter(|| {
            for y in 0..60 {
                black_box(buf.row_cells(y));
            }
        })
    });

    group.finish();
}

// ============================================================================
// Cell construction benchmarks
// ============================================================================

fn bench_cell_from_char(c: &mut Criterion) {
    let mut group = c.benchmark_group("cell/from_char");

    group.bench_function("ascii", |b| b.iter(|| black_box(Cell::from_char('A'))));

    group.bench_function("cjk", |b| b.iter(|| black_box(Cell::from_char('\u{4E2D}'))));

    group.bench_function("styled", |b| {
        b.iter(|| {
            black_box(
                Cell::from_char('A')
                    .with_fg(PackedRgba::rgb(255, 100, 50))
                    .with_bg(PackedRgba::rgb(0, 0, 0))
                    .with_attrs(CellAttrs::new(StyleFlags::BOLD | StyleFlags::ITALIC, 0)),
            )
        })
    });

    group.finish();
}

fn bench_packed_rgba(c: &mut Criterion) {
    let mut group = c.benchmark_group("cell/packed_rgba");

    let fg = PackedRgba::rgb(255, 100, 50);
    let bg = PackedRgba::rgba(0, 0, 0, 128);

    group.bench_function("rgb_construct", |b| {
        b.iter(|| black_box(PackedRgba::rgb(255, 100, 50)))
    });

    group.bench_function("over_blend", |b| b.iter(|| black_box(fg.over(bg))));

    group.finish();
}

// ============================================================================
// Buffer operation benchmarks
// ============================================================================

fn bench_buffer_new(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer/new");

    for (w, h) in [(80, 24), (120, 40), (200, 60)] {
        group.throughput(Throughput::Elements(w as u64 * h as u64));
        group.bench_with_input(
            BenchmarkId::new("alloc", format!("{w}x{h}")),
            &(),
            |b, _| b.iter(|| black_box(Buffer::new(w, h))),
        );
    }

    group.finish();
}

fn bench_buffer_clone(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer/clone");

    for (w, h) in [(80, 24), (200, 60)] {
        let buf = Buffer::new(w, h);
        group.throughput(Throughput::Elements(w as u64 * h as u64));
        group.bench_with_input(
            BenchmarkId::new("clone", format!("{w}x{h}")),
            &buf,
            |b, buf| b.iter(|| black_box(buf.clone())),
        );
    }

    group.finish();
}

fn bench_buffer_fill(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer/fill");
    let fill_cell = Cell::from_char('#').with_fg(PackedRgba::rgb(255, 0, 0));

    for (w, h) in [(80, 24), (200, 60)] {
        let mut buf = Buffer::new(w, h);
        let rect = Rect::from_size(w, h);
        group.throughput(Throughput::Elements(w as u64 * h as u64));
        group.bench_with_input(BenchmarkId::new("full", format!("{w}x{h}")), &(), |b, _| {
            b.iter(|| {
                buf.fill(rect, fill_cell);
                black_box(&buf);
            })
        });
    }

    group.finish();
}

fn bench_buffer_clear(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer/clear");

    for (w, h) in [(80, 24), (200, 60)] {
        let mut buf = Buffer::new(w, h);
        group.throughput(Throughput::Elements(w as u64 * h as u64));
        group.bench_with_input(
            BenchmarkId::new("clear", format!("{w}x{h}")),
            &(),
            |b, _| {
                b.iter(|| {
                    buf.clear();
                    black_box(&buf);
                })
            },
        );
    }

    group.finish();
}

fn bench_buffer_set(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer/set");
    let cell = Cell::from_char('X').with_fg(PackedRgba::rgb(0, 255, 0));

    let mut buf = Buffer::new(80, 24);
    group.bench_function("single_cell_80x24", |b| {
        b.iter(|| {
            buf.set(40, 12, cell);
            black_box(&buf);
        })
    });

    // Set cells across a whole row
    group.bench_function("full_row_80", |b| {
        b.iter(|| {
            for x in 0..80 {
                buf.set(x, 0, cell);
            }
            black_box(&buf);
        })
    });

    group.finish();
}

fn bench_buffer_scissor(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer/scissor");
    let fill_cell = Cell::from_char('.').with_fg(PackedRgba::rgb(128, 128, 128));

    let mut buf = Buffer::new(200, 60);
    let inner = Rect::new(10, 5, 100, 40);

    group.bench_function("push_fill_pop_200x60", |b| {
        b.iter(|| {
            buf.push_scissor(inner);
            buf.fill(Rect::from_size(200, 60), fill_cell);
            buf.pop_scissor();
            black_box(&buf);
        })
    });

    group.finish();
}

// ============================================================================
// Cells-per-second throughput (bd-1tssj)
// ============================================================================

/// Measure raw diff throughput in cells/second across varied change densities.
fn bench_diff_cells_per_second(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff/cells_per_second");

    for (w, h) in [(80u16, 24u16), (200, 60), (320, 90)] {
        let cells = w as u64 * h as u64;
        group.throughput(Throughput::Elements(cells));

        for pct in [0.0, 5.0, 50.0, 100.0] {
            let (old, new) = make_pair(w, h, pct);
            let label = format!("{w}x{h}@{pct}%");

            group.bench_with_input(
                BenchmarkId::new("compute", &label),
                &(&old, &new),
                |b, (old, new)| b.iter(|| black_box(BufferDiff::compute(old, new))),
            );

            group.bench_with_input(
                BenchmarkId::new("compute_dirty", &label),
                &(&old, &new),
                |b, (old, new)| b.iter(|| black_box(BufferDiff::compute_dirty(old, new))),
            );
        }
    }

    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().without_plots();
    targets =
        // Diff benchmarks
        bench_diff_identical,
        bench_diff_sparse,
        bench_diff_heavy,
        bench_diff_full,
        bench_diff_runs,
        // Full vs dirty comparison (bd-3e1t.1.6)
        bench_full_vs_dirty,
        // Selector overhead + selector vs fixed (bd-3e1t.8.4)
        bench_selector_overhead,
        bench_selector_vs_fixed,
        bench_diff_span_sparse_stats,
        bench_diff_span_dense_regression,
        bench_diff_tile_sparse_stats,
        bench_diff_tile_dense_regression,
        bench_diff_large_screen,
        // Cells/second throughput (bd-1tssj)
        bench_diff_cells_per_second,
        // Cell benchmarks
        bench_bits_eq,
        bench_cell_from_char,
        bench_packed_rgba,
        // Buffer benchmarks
        bench_row_cells,
        bench_buffer_new,
        bench_buffer_clone,
        bench_buffer_fill,
        bench_buffer_clear,
        bench_buffer_set,
        bench_buffer_scissor,
}

criterion_main!(benches);
