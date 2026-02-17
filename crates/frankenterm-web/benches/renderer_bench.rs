//! Frame-time benchmarks for the FrankenTerm web renderer pipeline (bd-lff4p.2.10).
//!
//! Benchmarks the CPU-side patch pipeline that feeds the WebGPU renderer:
//! - Cell conversion (ftui-render Cell → GPU CellData)
//! - Diff → Patch coalescing at various change rates
//! - Full-buffer patch generation
//! - End-to-end: Buffer pair → Diff → Patches → serialized bytes
//! - Glyph atlas cache hot-path, miss-path, and eviction pressure behavior
//!
//! Run with: cargo bench -p frankenterm-web --bench renderer_bench

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use frankenterm_web::frame_harness::{FrameRecord, FrameTimeCollector};
use frankenterm_web::glyph_atlas::{GlyphAtlasCache, GlyphKey, GlyphRaster};
use frankenterm_web::patch_feed::{
    cell_from_render, diff_to_patches, full_buffer_patch, patch_batch_stats,
};
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

fn solid_glyph_raster(width: u16, height: u16) -> GlyphRaster {
    let len = usize::from(width) * usize::from(height);
    GlyphRaster {
        width,
        height,
        pixels: vec![255; len],
        metrics: Default::default(),
    }
}

#[derive(Debug, Clone, Copy)]
enum XtermLikeWorkload {
    PromptEdit,
    LogBurst,
    FullscreenRepaint,
}

impl XtermLikeWorkload {
    const ALL: [Self; 3] = [Self::PromptEdit, Self::LogBurst, Self::FullscreenRepaint];

    fn as_str(self) -> &'static str {
        match self {
            Self::PromptEdit => "prompt_edit",
            Self::LogBurst => "log_burst",
            Self::FullscreenRepaint => "fullscreen_repaint",
        }
    }
}

fn paint_text_row(
    buf: &mut Buffer,
    width: u16,
    row: u16,
    text: &str,
    fg: PackedRgba,
    bg: PackedRgba,
) {
    for (x, ch) in text.chars().take(width as usize).enumerate() {
        buf.set_raw(x as u16, row, Cell::from_char(ch).with_fg(fg).with_bg(bg));
    }
}

fn make_xterm_like_pair(width: u16, height: u16, workload: XtermLikeWorkload) -> (Buffer, Buffer) {
    let mut old = Buffer::new(width, height);
    let mut new = Buffer::new(width, height);

    match workload {
        XtermLikeWorkload::PromptEdit => {
            let shell_fg = PackedRgba::rgb(220, 220, 220);
            let shell_bg = PackedRgba::rgb(8, 8, 8);
            let status_fg = PackedRgba::rgb(80, 170, 255);
            let status_bg = PackedRgba::rgb(16, 16, 24);

            let row = height.saturating_sub(1);
            let status_row = height.saturating_sub(2);
            paint_text_row(
                &mut old,
                width,
                status_row,
                "main ✗ 1  cargo bench --quick",
                status_fg,
                status_bg,
            );
            paint_text_row(
                &mut new,
                width,
                status_row,
                "main ✓ 0  cargo bench --quick",
                status_fg,
                status_bg,
            );

            paint_text_row(
                &mut old,
                width,
                row,
                "$ cargo test --all-targets -- -D warnings",
                shell_fg,
                shell_bg,
            );
            paint_text_row(
                &mut new,
                width,
                row,
                "$ cargo test -p frankenterm-web -- --nocapture",
                shell_fg,
                shell_bg,
            );
        }
        XtermLikeWorkload::LogBurst => {
            let info_fg = PackedRgba::rgb(200, 200, 200);
            let warn_fg = PackedRgba::rgb(255, 210, 90);
            let bg = PackedRgba::rgb(4, 8, 12);

            for y in 0..height {
                let ts = 1000 + y as u32;
                let old_line = format!("INFO [{ts}] worker=pty bytes={} action=flush", 120 + y);
                let new_line = if y % 6 == 0 {
                    format!(
                        "WARN [{ts}] worker=pty bytes={} action=backpressure",
                        420 + y
                    )
                } else {
                    format!("INFO [{ts}] worker=pty bytes={} action=flush", 220 + y)
                };
                paint_text_row(&mut old, width, y, &old_line, info_fg, bg);
                paint_text_row(
                    &mut new,
                    width,
                    y,
                    &new_line,
                    if y % 6 == 0 { warn_fg } else { info_fg },
                    bg,
                );
            }
        }
        XtermLikeWorkload::FullscreenRepaint => {
            for y in 0..height {
                for x in 0..width {
                    let old_ch =
                        char::from_u32(u32::from('a') + ((x as u32 + y as u32 * 3) % 26)).unwrap();
                    let new_ch =
                        char::from_u32(u32::from('A') + ((x as u32 * 5 + y as u32 * 7 + 11) % 26))
                            .unwrap();
                    old.set_raw(
                        x,
                        y,
                        Cell::from_char(old_ch)
                            .with_fg(PackedRgba::rgb(90, 140, 220))
                            .with_bg(PackedRgba::rgb(10, 10, 28)),
                    );
                    new.set_raw(
                        x,
                        y,
                        Cell::from_char(new_ch)
                            .with_fg(PackedRgba::rgb(220, 140, 90))
                            .with_bg(PackedRgba::rgb(28, 10, 10)),
                    );
                }
            }
        }
    }

    (old, new)
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
                        let patch_stats = patch_batch_stats(&patches);
                        black_box(&patches);

                        let elapsed = start.elapsed();
                        total += elapsed;

                        collector.record_frame(FrameRecord {
                            elapsed,
                            cpu_submit: None,
                            gpu_time: None,
                            dirty_cells: patch_stats.dirty_cells,
                            patch_count: patch_stats.patch_count,
                            bytes_uploaded: patch_stats.bytes_uploaded,
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
// Comparative xterm-like workload harness (bd-2vr05.8.5)
// ---------------------------------------------------------------------------

fn bench_xterm_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("web/xterm_workloads");

    for (w, h) in [(80u16, 24u16), (120, 40)] {
        group.throughput(Throughput::Elements(u64::from(w) * u64::from(h)));

        for workload in XtermLikeWorkload::ALL {
            let (old, new) = make_xterm_like_pair(w, h, workload);
            let workload_name = workload.as_str();
            let bench_id = BenchmarkId::new(workload_name, format!("{w}x{h}"));

            group.bench_with_input(bench_id, &(&old, &new), |b, (old, new)| {
                b.iter_custom(|iters| {
                    let mut collector =
                        FrameTimeCollector::new(&format!("xterm_like_{workload_name}_{w}x{h}"), w, h);
                    let mut total = Duration::ZERO;

                    for _ in 0..iters {
                        let start = Instant::now();
                        let diff = BufferDiff::compute(old, new);
                        let patches = diff_to_patches(new, &diff);
                        let patch_stats = patch_batch_stats(&patches);
                        black_box(&patches);
                        let elapsed = start.elapsed();
                        total += elapsed;

                        collector.record_frame(FrameRecord {
                            elapsed,
                            cpu_submit: None,
                            gpu_time: None,
                            dirty_cells: patch_stats.dirty_cells,
                            patch_count: patch_stats.patch_count,
                            bytes_uploaded: patch_stats.bytes_uploaded,
                        });
                    }

                    let report = collector.report();
                    eprintln!(
                        "{{\"event\":\"web_xterm_workload_bench\",\"workload\":\"{workload_name}\",\"run_id\":\"{}\",\"cols\":{w},\"rows\":{h},\"iters\":{iters},\"p50_us\":{},\"p95_us\":{},\"p99_us\":{},\"avg_dirty\":{:.1},\"avg_patches\":{:.1},\"avg_bytes\":{:.0},\"memory_cells\":{},\"memory_cell_bytes\":{}}}",
                        report.run_id,
                        report.frame_time.p50_us,
                        report.frame_time.p95_us,
                        report.frame_time.p99_us,
                        report.patch_stats.avg_dirty_per_frame,
                        report.patch_stats.avg_patches_per_frame,
                        report.patch_stats.avg_bytes_per_frame,
                        u64::from(w) * u64::from(h),
                        (u64::from(w) * u64::from(h)) * (CELL_DATA_BYTES as u64),
                    );

                    total
                })
            });
        }
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Glyph atlas cache benchmarks
// ---------------------------------------------------------------------------

fn bench_glyph_atlas_cache(c: &mut Criterion) {
    let mut group = c.benchmark_group("web/glyph_atlas_cache");

    let key = GlyphKey::from_char('A', 16);
    group.throughput(Throughput::Elements(1));

    group.bench_function("miss_insert_single", |b| {
        b.iter_batched(
            || GlyphAtlasCache::new(128, 128, 128 * 128),
            |mut cache| {
                let placement = cache
                    .get_or_insert_with(key, |_| solid_glyph_raster(8, 16))
                    .expect("single insert should fit in atlas");
                black_box(placement);
                black_box(cache.stats());
                black_box(cache.objective());
            },
            BatchSize::SmallInput,
        )
    });

    let mut hot_cache = GlyphAtlasCache::new(128, 128, 128 * 128);
    hot_cache
        .get_or_insert_with(key, |_| solid_glyph_raster(8, 16))
        .expect("seed glyph should fit in atlas");
    group.bench_function("hit_hot_path", |b| {
        b.iter(|| {
            let placement = hot_cache.get(key).expect("seeded key should stay cached");
            black_box(placement);
            black_box(hot_cache.stats());
        })
    });

    // With this budget and slot size (~180 bytes per 8x16 glyph including padding),
    // cycling three keys forces frequent LRU eviction/reinsert behavior.
    let mut eviction_cache = GlyphAtlasCache::new(64, 64, 2 * 180);
    let eviction_keys = [
        GlyphKey::from_char('A', 16),
        GlyphKey::from_char('B', 16),
        GlyphKey::from_char('C', 16),
    ];
    let mut idx = 0usize;
    group.bench_function("eviction_cycle_3keys_budget2", |b| {
        b.iter(|| {
            let key = eviction_keys[idx % eviction_keys.len()];
            idx = idx.wrapping_add(1);
            let placement = eviction_cache
                .get_or_insert_with(key, |_| solid_glyph_raster(8, 16))
                .expect("eviction cycle inserts should fit in atlas");
            black_box(placement);
            black_box(eviction_cache.stats());
            black_box(eviction_cache.objective());
        })
    });

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
// Scrollback viewport virtualization (bd-lff4p.2.14)
// ---------------------------------------------------------------------------

fn bench_scroll_viewport(c: &mut Criterion) {
    use frankenterm_web::input::{Modifiers, WheelInput};
    use frankenterm_web::scroll::{ScrollFrameStats, ScrollState, WheelCoalescer};

    let mut group = c.benchmark_group("web/scroll_viewport");

    // Simulate scroll viewport computation on 100k-line scrollback.
    for scrollback_size in [1_000usize, 10_000, 100_000] {
        group.bench_with_input(
            BenchmarkId::new("viewport_compute", scrollback_size),
            &scrollback_size,
            |b, &size| {
                let mut state = ScrollState::with_defaults();
                state.set_offset(size / 2);
                b.iter(|| {
                    let snap = state.viewport(size, 24);
                    black_box(snap);
                })
            },
        );
    }

    // Benchmark wheel coalescing (10 events/frame).
    group.bench_function("coalesce_10_events", |b| {
        let events: Vec<WheelInput> = (0..10)
            .map(|i| WheelInput {
                x: 40,
                y: 12,
                dx: 0,
                dy: if i % 3 == 0 { -1 } else { 1 },
                mods: Modifiers::empty(),
            })
            .collect();

        b.iter(|| {
            let mut coalescer = WheelCoalescer::new();
            for ev in &events {
                coalescer.push(ev);
            }
            let (dy, count) = coalescer.drain();
            black_box((dy, count));
        })
    });

    // Full frame cycle: coalesce → apply_wheel → tick → viewport → stats.
    group.bench_with_input(
        BenchmarkId::new("full_frame_cycle", 100_000),
        &100_000usize,
        |b, &scrollback_size| {
            let wheel = WheelInput {
                x: 40,
                y: 12,
                dx: 0,
                dy: 1,
                mods: Modifiers::empty(),
            };
            b.iter(|| {
                let mut coalescer = WheelCoalescer::new();
                let mut state = ScrollState::with_defaults();
                state.set_offset(scrollback_size / 2);

                coalescer.push(&wheel);
                let (dy, count) = coalescer.drain();

                let max_off = scrollback_size.saturating_sub(24);
                state.apply_wheel(dy, max_off);
                state.tick(max_off);

                let snap = state.viewport(scrollback_size, 24);
                let stats = ScrollFrameStats::from_snapshot(&snap, count);
                black_box(stats);
            })
        },
    );

    // Inertia convergence: measure how many ticks until animation stops.
    group.bench_function("inertia_convergence", |b| {
        b.iter(|| {
            let mut state = ScrollState::with_defaults();
            state.apply_wheel(3, 100_000);
            let mut ticks = 0u32;
            while state.tick(100_000) {
                ticks += 1;
            }
            black_box(ticks);
        })
    });

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
        bench_xterm_workloads,
        bench_glyph_atlas_cache,
        bench_first_frame,
        bench_scroll_viewport,
}

criterion_main!(benches);
