//! Frame-time benchmarks for the FrankenTerm web renderer pipeline (bd-lff4p.2.10).
//!
//! Benchmarks the CPU-side patch pipeline that feeds the WebGPU renderer:
//! - Cell conversion (ftui-render Cell → GPU CellData)
//! - Diff → Patch coalescing at various change rates
//! - Full-buffer patch generation
//! - End-to-end: Buffer pair → Diff → Patches → serialized bytes
//!
//! Run with: cargo bench -p frankenterm-web --bench renderer_bench

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use frankenterm_web::frame_harness::{FrameRecord, FrameTimeCollector};
use frankenterm_web::patch_feed::{cell_from_render, diff_to_patches, full_buffer_patch};
use frankenterm_web::renderer::{CELL_DATA_BYTES, CellData};
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, CellAttrs, PackedRgba, StyleFlags};
use ftui_render::diff::BufferDiff;
use std::hint::black_box;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a pair of buffers where `pct` percent of cells differ.
fn make_pair(width: u16, height: u16, change_pct: f64) -> (Buffer, Buffer) {
    let old = Buffer::new(width, height);
    let mut new = Buffer::new(width, height);

    let total = width as usize * height as usize;
    let to_change = ((total as f64) * change_pct / 100.0) as usize;

    for i in 0..to_change {
        let x = (i * 7 + 3) as u16 % width;
        let y = (i * 11 + 5) as u16 % height;
        let ch = char::from_u32(('A' as u32) + (i as u32 % 26)).unwrap();
        new.set_raw(
            x,
            y,
            Cell::from_char(ch)
                .with_fg(PackedRgba::rgb(255, 0, 0))
                .with_bg(PackedRgba::rgb(0, 0, 128)),
        );
    }

    (old, new)
}

fn styled_cell() -> Cell {
    Cell::from_char('X')
        .with_fg(PackedRgba::rgb(0, 255, 0))
        .with_bg(PackedRgba::rgb(0, 0, 0))
        .with_attrs(CellAttrs::new(StyleFlags::BOLD | StyleFlags::ITALIC, 0))
}

// ---------------------------------------------------------------------------
// Cell conversion benchmarks
// ---------------------------------------------------------------------------

fn bench_cell_from_render(c: &mut Criterion) {
    let mut group = c.benchmark_group("web/cell_from_render");

    let empty = Cell::default();
    let ascii = Cell::from_char('A');
    let styled = styled_cell();

    group.bench_function("empty", |b| b.iter(|| black_box(cell_from_render(&empty))));
    group.bench_function("ascii", |b| b.iter(|| black_box(cell_from_render(&ascii))));
    group.bench_function("styled", |b| {
        b.iter(|| black_box(cell_from_render(&styled)))
    });

    group.finish();
}

fn bench_cell_to_bytes(c: &mut Criterion) {
    let mut group = c.benchmark_group("web/cell_to_bytes");

    let cell = CellData {
        bg_rgba: 0x000000FF,
        fg_rgba: 0xFF0000FF,
        glyph_id: 65,
        attrs: 0x03,
    };

    group.bench_function("single", |b| b.iter(|| black_box(cell.to_bytes())));

    // Batch serialization of 1920 cells (80x24).
    let cells: Vec<CellData> = vec![cell; 1920];
    group.throughput(Throughput::Bytes((1920 * CELL_DATA_BYTES) as u64));
    group.bench_function("batch_1920", |b| {
        b.iter(|| {
            let mut bytes = Vec::with_capacity(cells.len() * CELL_DATA_BYTES);
            for c in &cells {
                bytes.extend_from_slice(&c.to_bytes());
            }
            black_box(bytes.len())
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Diff → Patches benchmarks
// ---------------------------------------------------------------------------

fn bench_diff_to_patches(c: &mut Criterion) {
    let mut group = c.benchmark_group("web/diff_to_patches");

    for (w, h) in [(80u16, 24u16), (120, 40)] {
        let cells = w as u64 * h as u64;
        group.throughput(Throughput::Elements(cells));

        // Sparse 5% changes.
        let (old, new) = make_pair(w, h, 5.0);
        let diff = BufferDiff::compute(&old, &new);
        group.bench_with_input(
            BenchmarkId::new("sparse_5pct", format!("{w}x{h}")),
            &(&new, &diff),
            |b, (buf, diff)| b.iter(|| black_box(diff_to_patches(buf, diff))),
        );

        // Heavy 50% changes.
        let (old50, new50) = make_pair(w, h, 50.0);
        let diff50 = BufferDiff::compute(&old50, &new50);
        group.bench_with_input(
            BenchmarkId::new("heavy_50pct", format!("{w}x{h}")),
            &(&new50, &diff50),
            |b, (buf, diff)| b.iter(|| black_box(diff_to_patches(buf, diff))),
        );

        // Full 100% changes.
        let (old100, new100) = make_pair(w, h, 100.0);
        let diff100 = BufferDiff::compute(&old100, &new100);
        group.bench_with_input(
            BenchmarkId::new("full_100pct", format!("{w}x{h}")),
            &(&new100, &diff100),
            |b, (buf, diff)| b.iter(|| black_box(diff_to_patches(buf, diff))),
        );
    }

    group.finish();
}

fn bench_full_buffer_patch(c: &mut Criterion) {
    let mut group = c.benchmark_group("web/full_buffer_patch");

    for (w, h) in [(80u16, 24u16), (120, 40), (200, 60)] {
        let cells = w as u64 * h as u64;
        group.throughput(Throughput::Elements(cells));
        let buf = Buffer::new(w, h);
        group.bench_with_input(
            BenchmarkId::new("alloc", format!("{w}x{h}")),
            &buf,
            |b, buf| b.iter(|| black_box(full_buffer_patch(buf))),
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// End-to-end pipeline: Buffer → Diff → Patches → bytes
// ---------------------------------------------------------------------------

fn bench_e2e_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("web/e2e_pipeline");

    for (w, h) in [(80u16, 24u16), (120, 40)] {
        let cells = w as u64 * h as u64;
        group.throughput(Throughput::Elements(cells));

        for (label, pct) in [("sparse_5pct", 5.0), ("heavy_50pct", 50.0)] {
            let (old, new) = make_pair(w, h, pct);
            let bench_id = BenchmarkId::new(label, format!("{w}x{h}"));

            group.bench_with_input(bench_id, &(&old, &new), |b, (old, new)| {
                b.iter(|| {
                    let diff = BufferDiff::compute(old, new);
                    let patches = diff_to_patches(new, &diff);
                    let mut total_bytes = 0usize;
                    for patch in &patches {
                        for cell in &patch.cells {
                            total_bytes += cell.to_bytes().len();
                        }
                    }
                    black_box(total_bytes)
                })
            });
        }
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Frame-time harness integration bench (p50/p95/p99 stats with JSONL)
// ---------------------------------------------------------------------------

fn bench_frame_harness_stats(c: &mut Criterion) {
    let mut group = c.benchmark_group("web/frame_harness_stats");

    for (w, h) in [(80u16, 24u16), (120, 40)] {
        let cells = w as u64 * h as u64;
        group.throughput(Throughput::Elements(cells));

        for (label, pct) in [("sparse_5pct", 5.0), ("heavy_50pct", 50.0)] {
            let (old, new) = make_pair(w, h, pct);
            let bench_id = BenchmarkId::new(label, format!("{w}x{h}"));

            group.bench_with_input(bench_id, &(&old, &new), |b, (old, new)| {
                b.iter_custom(|iters| {
                    let mut collector =
                        FrameTimeCollector::new(&format!("{label}_{w}x{h}"), w, h);

                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let start = Instant::now();

                        let diff = BufferDiff::compute(old, new);
                        let patches = diff_to_patches(new, &diff);
                        let dirty: u32 = patches.iter().map(|p| p.cells.len() as u32).sum();
                        let bytes: u64 = dirty as u64 * CELL_DATA_BYTES as u64;
                        black_box(&patches);

                        let elapsed = start.elapsed();
                        total += elapsed;

                        collector.record_frame(FrameRecord {
                            elapsed,
                            dirty_cells: dirty,
                            patch_count: patches.len() as u32,
                            bytes_uploaded: bytes,
                        });
                    }

                    // Emit JSONL stats to stderr (captured by criterion).
                    let report = collector.report();
                    eprintln!(
                        "{{\"event\":\"web_frame_bench\",\"run_id\":\"{}\",\"cols\":{w},\"rows\":{h},\"iters\":{iters},\"p50_us\":{},\"p95_us\":{},\"p99_us\":{},\"avg_dirty\":{:.1},\"avg_patches\":{:.1},\"avg_bytes\":{:.0}}}",
                        report.run_id,
                        report.frame_time.p50_us,
                        report.frame_time.p95_us,
                        report.frame_time.p99_us,
                        report.patch_stats.avg_dirty_per_frame,
                        report.patch_stats.avg_patches_per_frame,
                        report.patch_stats.avg_bytes_per_frame,
                    );

                    total
                })
            });
        }
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// First-frame (full repaint) latency
// ---------------------------------------------------------------------------

fn bench_first_frame(c: &mut Criterion) {
    let mut group = c.benchmark_group("web/first_frame");

    for (w, h) in [(80u16, 24u16), (120, 40), (200, 60)] {
        let cells = w as u64 * h as u64;
        group.throughput(Throughput::Elements(cells));

        // Simulate first frame: populate buffer then full_buffer_patch + serialize.
        let mut buf = Buffer::new(w, h);
        // Fill with realistic content.
        for y in 0..h {
            for x in 0..w {
                let ch = char::from_u32((' ' as u32) + ((x + y) as u32 % 95)).unwrap();
                buf.set_raw(
                    x,
                    y,
                    Cell::from_char(ch).with_fg(PackedRgba::rgb(200, 200, 200)),
                );
            }
        }

        group.bench_with_input(
            BenchmarkId::new("patch_and_serialize", format!("{w}x{h}")),
            &buf,
            |b, buf| {
                b.iter(|| {
                    let patch = full_buffer_patch(buf);
                    let mut bytes = Vec::with_capacity(patch.cells.len() * CELL_DATA_BYTES);
                    for cell in &patch.cells {
                        bytes.extend_from_slice(&cell.to_bytes());
                    }
                    black_box(bytes.len())
                })
            },
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Criterion groups
// ---------------------------------------------------------------------------

criterion_group! {
    name = benches;
    config = Criterion::default().without_plots();
    targets =
        bench_cell_from_render,
        bench_cell_to_bytes,
        bench_diff_to_patches,
        bench_full_buffer_patch,
        bench_e2e_pipeline,
        bench_frame_harness_stats,
        bench_first_frame,
}

criterion_main!(benches);
