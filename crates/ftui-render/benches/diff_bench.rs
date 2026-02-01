//! Benchmarks for cell, buffer, and diff operations (bd-19x, bd-2m5)
//!
//! Run with: cargo bench -p ftui-render --bench diff_bench

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, CellAttrs, PackedRgba, StyleFlags};
use ftui_render::diff::BufferDiff;
use std::hint::black_box;

/// Create a pair of buffers where only `pct` percent of cells differ.
fn make_pair(width: u16, height: u16, change_pct: f64) -> (Buffer, Buffer) {
    let old = Buffer::new(width, height);
    let mut new = old.clone();

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

criterion_group!(
    benches,
    // Diff benchmarks
    bench_diff_identical,
    bench_diff_sparse,
    bench_diff_heavy,
    bench_diff_full,
    bench_diff_runs,
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
);

criterion_main!(benches);
