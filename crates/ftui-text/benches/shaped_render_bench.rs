//! Benchmarks for shaped rendering pipeline: ClusterMap, ShapedLineLayout, ShapingFallback.
//!
//! Run with: `cargo bench --package ftui-text --bench shaped_render_bench`
//!
//! # What This Measures
//!
//! The shaped rendering pipeline converts text + shaping data into cell placements
//! used by the terminal renderer. Key operations:
//!
//! | Component          | Hot Path                                          |
//! |--------------------|---------------------------------------------------|
//! | ClusterMap         | `from_text`, `byte_to_cell`, `cell_to_byte`       |
//! | ShapedLineLayout   | `from_text`, `from_run`, `apply_justification`    |
//! | ShapingFallback    | `shape_line` (terminal & shaped paths)             |
//!
//! # Performance Baselines (bd-2vr05.15.3.6)
//!
//! Measured on Contabo VPS worker (2026-02-16):
//!
//! | Operation                               | Latency (10K) | Throughput     |
//! |-----------------------------------------|---------------|----------------|
//! | ClusterMap::from_text (latin)            | ~480µs        | 19.8 MiB/s    |
//! | ClusterMap::from_text (cjk)             | ~343µs        | 27.8 MiB/s    |
//! | ClusterMap byte_to_cell (270 lookups)    | ~7µs          | —              |
//! | ClusterMap cell_to_byte (270 lookups)    | ~8.4µs        | —              |
//! | ShapedLineLayout::from_text (latin)      | ~609µs        | 15.6 MiB/s    |
//! | ShapedLineLayout::from_text (cjk)       | ~462µs        | 20.6 MiB/s    |
//! | ShapedLineLayout::from_run (latin) ⚠️   | ~53ms         | 183 KiB/s     |
//! | ShapedLineLayout::from_run (cjk)        | ~7.2ms        | 1.3 MiB/s     |
//! | apply_justification                      | ~83µs         | 115 MiB/s     |
//! | apply_tracking                           | ~47µs         | 201 MiB/s     |
//! | fallback/terminal (latin)                | ~612µs        | 15.5 MiB/s    |
//! | fallback/shaped_noop (latin) ⚠️          | ~60.8ms       | 160 KiB/s     |
//! | fallback/batch (40 lines)                | ~209µs        | 15.2 MiB/s    |
//!
//! ⚠️ `from_run` has O(n²) behavior for Latin text (1 glyph per byte); needs optimization.
//!    CJK is 7x faster because fewer glyphs per byte. Terminal fallback (from_text) is fast.
//!
//! # Performance Budgets (Regression Gates)
//!
//! Budget = ~1.5x baseline with headroom for variance:
//!
//! | Benchmark                              | Budget    | Rationale                           |
//! |----------------------------------------|-----------|-------------------------------------|
//! | cluster_map/from_text/latin/10K        | ≤ 750µs   | Per-frame text layout budget        |
//! | cluster_map/lookup/byte_to_cell        | ≤ 12µs    | Cursor positioning (60fps = 16ms)   |
//! | cluster_map/lookup/cell_to_byte        | ≤ 14µs    | Selection extraction hot path       |
//! | shaped_layout/from_text/latin/10K      | ≤ 950µs   | Full layout construction budget     |
//! | shaped_layout/from_text/cjk/10K        | ≤ 700µs   | Wide char layout (2x placements)    |
//! | shaped_layout/justify/justification/10K| ≤ 130µs   | Post-layout adjustment              |
//! | shaped_layout/justify/tracking/10K     | ≤ 75µs    | Uniform spacing pass                |
//! | fallback/terminal/latin/10K            | ≤ 950µs   | Terminal mode full pipeline          |
//! | fallback/batch/40_lines                | ≤ 320µs   | Screenful of text at 60fps          |

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use ftui_text::cluster_map::ClusterMap;
use ftui_text::justification::GlueSpec;
use ftui_text::layout_policy::RuntimeCapability;
use ftui_text::script_segmentation::{RunDirection, Script};
use ftui_text::shaped_render::ShapedLineLayout;
use ftui_text::shaping::NoopShaper;
use ftui_text::shaping_fallback::ShapingFallback;
use std::hint::black_box;

// ============================================================================
// Test Data
// ============================================================================

/// ASCII Latin text — most common case in terminal workflows.
const LATIN: &str = "The quick brown fox jumps over the lazy dog. \
    Pack my box with five dozen liquor jugs. ";

/// CJK text — each character occupies 2 cells.
const CJK: &str = "天地玄黄宇宙洪荒日月盈昃辰宿列张寒来暑往秋收冬藏";

/// Mixed script text — Latin + CJK + emoji.
const MIXED: &str = "Hello 世界! 🚀 Rust は最高 café résumé naïve ";

/// Generate repeated text of approximately the given size.
fn generate_text(base: &str, target_size: usize) -> String {
    let repeats = (target_size / base.len()).max(1);
    base.repeat(repeats)
}

// ============================================================================
// ClusterMap Benchmarks
// ============================================================================

fn bench_cluster_map_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("cluster_map/from_text");

    for size in [1_000, 10_000, 100_000] {
        let text = generate_text(LATIN, size);
        group.throughput(Throughput::Bytes(text.len() as u64));

        group.bench_with_input(BenchmarkId::new("latin", size), &text, |b, text| {
            b.iter(|| ClusterMap::from_text(black_box(text)));
        });

        let cjk_text = generate_text(CJK, size);
        group.bench_with_input(BenchmarkId::new("cjk", size), &cjk_text, |b, text| {
            b.iter(|| ClusterMap::from_text(black_box(text)));
        });

        let mixed_text = generate_text(MIXED, size);
        group.bench_with_input(BenchmarkId::new("mixed", size), &mixed_text, |b, text| {
            b.iter(|| ClusterMap::from_text(black_box(text)));
        });
    }

    group.finish();
}

fn bench_cluster_map_lookup(c: &mut Criterion) {
    let text = generate_text(LATIN, 10_000);
    let map = ClusterMap::from_text(&text);
    let total_bytes = map.total_bytes();
    let total_cells = map.total_cells();

    let mut group = c.benchmark_group("cluster_map/lookup");

    // byte_to_cell — random access pattern (binary search path).
    group.bench_function("byte_to_cell/sequential", |b| {
        b.iter(|| {
            for byte in (0..total_bytes).step_by(37) {
                black_box(map.byte_to_cell(byte));
            }
        });
    });

    // cell_to_byte — reverse lookup.
    group.bench_function("cell_to_byte/sequential", |b| {
        b.iter(|| {
            for cell in (0..total_cells).step_by(37) {
                black_box(map.cell_to_byte(cell));
            }
        });
    });

    // byte_range_to_cell_range — selection mapping.
    group.bench_function("byte_range_to_cell_range", |b| {
        b.iter(|| {
            for start in (0..total_bytes.saturating_sub(100)).step_by(97) {
                black_box(map.byte_range_to_cell_range(start, start + 100));
            }
        });
    });

    // cell_range_to_byte_range — text extraction.
    group.bench_function("cell_range_to_byte_range", |b| {
        b.iter(|| {
            for start in (0..total_cells.saturating_sub(50)).step_by(47) {
                black_box(map.cell_range_to_byte_range(start, start + 50));
            }
        });
    });

    // CJK lookups (wider clusters, different distribution).
    let cjk_text = generate_text(CJK, 10_000);
    let cjk_map = ClusterMap::from_text(&cjk_text);
    let cjk_cells = cjk_map.total_cells();

    group.bench_function("byte_to_cell/cjk", |b| {
        b.iter(|| {
            for byte in (0..cjk_text.len()).step_by(37) {
                black_box(cjk_map.byte_to_cell(byte));
            }
        });
    });

    group.bench_function("cell_to_byte/cjk", |b| {
        b.iter(|| {
            for cell in (0..cjk_cells).step_by(37) {
                black_box(cjk_map.cell_to_byte(cell));
            }
        });
    });

    group.finish();
}

fn bench_cluster_map_extract(c: &mut Criterion) {
    let text = generate_text(MIXED, 10_000);
    let map = ClusterMap::from_text(&text);
    let total_cells = map.total_cells();

    let mut group = c.benchmark_group("cluster_map/extract_text");

    group.bench_function("small_range", |b| {
        b.iter(|| {
            for start in (0..total_cells.saturating_sub(10)).step_by(43) {
                black_box(map.extract_text_for_cells(&text, start, start + 10));
            }
        });
    });

    group.bench_function("large_range", |b| {
        b.iter(|| {
            let mid = total_cells / 4;
            let end = (3 * total_cells) / 4;
            black_box(map.extract_text_for_cells(&text, mid, end));
        });
    });

    group.finish();
}

// ============================================================================
// ShapedLineLayout Benchmarks
// ============================================================================

fn bench_shaped_layout_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("shaped_layout/from_text");

    for size in [1_000, 10_000, 100_000] {
        let text = generate_text(LATIN, size);
        group.throughput(Throughput::Bytes(text.len() as u64));

        group.bench_with_input(BenchmarkId::new("latin", size), &text, |b, text| {
            b.iter(|| ShapedLineLayout::from_text(black_box(text)));
        });

        let cjk_text = generate_text(CJK, size);
        group.bench_with_input(BenchmarkId::new("cjk", size), &cjk_text, |b, text| {
            b.iter(|| ShapedLineLayout::from_text(black_box(text)));
        });

        let mixed_text = generate_text(MIXED, size);
        group.bench_with_input(BenchmarkId::new("mixed", size), &mixed_text, |b, text| {
            b.iter(|| ShapedLineLayout::from_text(black_box(text)));
        });
    }

    group.finish();
}

fn bench_shaped_layout_from_run(c: &mut Criterion) {
    let mut group = c.benchmark_group("shaped_layout/from_run");

    // Use NoopShaper to generate runs, then benchmark from_run construction.
    let shaper = NoopShaper;

    for size in [1_000, 10_000] {
        let text = generate_text(LATIN, size);
        let run = shaper.shape(&text, Script::Latin, RunDirection::Ltr, &Default::default());
        group.throughput(Throughput::Bytes(text.len() as u64));

        group.bench_with_input(
            BenchmarkId::new("latin", size),
            &(&text, &run),
            |b, (text, run)| {
                b.iter(|| ShapedLineLayout::from_run(black_box(text), black_box(run)));
            },
        );

        let cjk_text = generate_text(CJK, size);
        let cjk_run = shaper.shape(
            &cjk_text,
            Script::Han,
            RunDirection::Ltr,
            &Default::default(),
        );

        group.bench_with_input(
            BenchmarkId::new("cjk", size),
            &(&cjk_text, &cjk_run),
            |b, (text, run)| {
                b.iter(|| ShapedLineLayout::from_run(black_box(text), black_box(run)));
            },
        );
    }

    group.finish();
}

fn bench_shaped_layout_justification(c: &mut Criterion) {
    let mut group = c.benchmark_group("shaped_layout/justify");

    for size in [1_000, 10_000] {
        let text = generate_text(LATIN, size);
        group.throughput(Throughput::Bytes(text.len() as u64));

        // Pre-build layout, then benchmark justification pass.
        group.bench_with_input(
            BenchmarkId::new("apply_justification", size),
            &text,
            |b, text| {
                b.iter_batched(
                    || ShapedLineLayout::from_text(text),
                    |mut layout| {
                        layout.apply_justification(
                            black_box(text),
                            black_box(128), // ratio_fixed: 50% stretch
                            black_box(&GlueSpec {
                                natural_subcell: 256,
                                stretch_subcell: 128,
                                shrink_subcell: 64,
                            }),
                        );
                        layout
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("apply_tracking", size),
            &text,
            |b, text| {
                b.iter_batched(
                    || ShapedLineLayout::from_text(text),
                    |mut layout| {
                        layout.apply_tracking(black_box(32)); // +32/256 cell per glyph
                        layout
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

fn bench_shaped_layout_queries(c: &mut Criterion) {
    let text = generate_text(LATIN, 10_000);
    let layout = ShapedLineLayout::from_text(&text);
    let total = layout.total_cells();

    let mut group = c.benchmark_group("shaped_layout/queries");

    group.bench_function("placement_at_cell/sequential", |b| {
        b.iter(|| {
            for cell in (0..total).step_by(37) {
                black_box(layout.placement_at_cell(cell));
            }
        });
    });

    group.bench_function("extract_text/small", |b| {
        b.iter(|| {
            for start in (0..total.saturating_sub(20)).step_by(43) {
                black_box(layout.extract_text(&text, start, start + 20));
            }
        });
    });

    group.bench_function("has_spacing_deltas", |b| {
        b.iter(|| black_box(layout.has_spacing_deltas()));
    });

    group.finish();
}

// ============================================================================
// ShapingFallback Benchmarks
// ============================================================================

fn bench_fallback_terminal(c: &mut Criterion) {
    let mut group = c.benchmark_group("fallback/terminal");

    let fb = ShapingFallback::terminal();

    for size in [1_000, 10_000] {
        let text = generate_text(LATIN, size);
        group.throughput(Throughput::Bytes(text.len() as u64));

        group.bench_with_input(BenchmarkId::new("latin", size), &text, |b, text| {
            b.iter(|| fb.shape_line(black_box(text), Script::Latin, RunDirection::Ltr));
        });

        let cjk_text = generate_text(CJK, size);
        group.bench_with_input(BenchmarkId::new("cjk", size), &cjk_text, |b, text| {
            b.iter(|| fb.shape_line(black_box(text), Script::Han, RunDirection::Ltr));
        });
    }

    group.finish();
}

fn bench_fallback_shaped(c: &mut Criterion) {
    let mut group = c.benchmark_group("fallback/shaped_noop");

    let fb = ShapingFallback::with_shaper(NoopShaper, RuntimeCapability::FULL);

    for size in [1_000, 10_000] {
        let text = generate_text(LATIN, size);
        group.throughput(Throughput::Bytes(text.len() as u64));

        group.bench_with_input(BenchmarkId::new("latin", size), &text, |b, text| {
            b.iter(|| fb.shape_line(black_box(text), Script::Latin, RunDirection::Ltr));
        });

        let mixed_text = generate_text(MIXED, size);
        group.bench_with_input(BenchmarkId::new("mixed", size), &mixed_text, |b, text| {
            b.iter(|| fb.shape_line(black_box(text), Script::Latin, RunDirection::Ltr));
        });
    }

    group.finish();
}

fn bench_fallback_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("fallback/batch");

    let fb = ShapingFallback::terminal();

    // Simulate rendering a screenful of text (40 lines × ~80 chars).
    let lines: Vec<String> = (0..40)
        .map(|i| format!("Line {:>4}: {}", i, &LATIN[..LATIN.len().min(72)]))
        .collect();
    let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let total_bytes: u64 = line_refs.iter().map(|l| l.len() as u64).sum();

    group.throughput(Throughput::Bytes(total_bytes));

    group.bench_function("40_lines", |b| {
        b.iter(|| fb.shape_lines(black_box(&line_refs), Script::Latin, RunDirection::Ltr));
    });

    group.finish();
}

// ============================================================================
// Criterion Configuration
// ============================================================================

use ftui_text::shaping::TextShaper;

criterion_group!(
    benches,
    bench_cluster_map_construction,
    bench_cluster_map_lookup,
    bench_cluster_map_extract,
    bench_shaped_layout_construction,
    bench_shaped_layout_from_run,
    bench_shaped_layout_justification,
    bench_shaped_layout_queries,
    bench_fallback_terminal,
    bench_fallback_shaped,
    bench_fallback_batch,
);

criterion_main!(benches);
