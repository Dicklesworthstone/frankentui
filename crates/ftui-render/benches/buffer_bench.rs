//! Benchmarks for Buffer operations (bd-19x)
//!
//! Performance budgets:
//! - Row comparison (80 cols): < 100ns
//! - Buffer diff (80x24): < 10µs
//!
//! Run with: cargo bench -p ftui-render --bench buffer_bench

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use std::hint::black_box;

// =============================================================================
// Buffer allocation
// =============================================================================

fn bench_buffer_new(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer/new");

    for (w, h) in [(80, 24), (120, 40), (200, 60)] {
        let cells = w as u64 * h as u64;
        group.throughput(Throughput::Elements(cells));
        group.bench_with_input(
            BenchmarkId::new("alloc", format!("{w}x{h}")),
            &(),
            |b, _| b.iter(|| black_box(Buffer::new(w, h))),
        );
    }

    group.finish();
}

// =============================================================================
// Buffer clone
// =============================================================================

fn bench_buffer_clone(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer/clone");

    for (w, h) in [(80, 24), (120, 40), (200, 60)] {
        let cells = w as u64 * h as u64;
        group.throughput(Throughput::Elements(cells));
        let buf = Buffer::new(w, h);
        group.bench_with_input(
            BenchmarkId::new("clone", format!("{w}x{h}")),
            &buf,
            |b, buf| b.iter(|| black_box(buf.clone())),
        );
    }

    group.finish();
}

// =============================================================================
// Cell access: set vs set_raw
// =============================================================================

fn bench_buffer_set(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer/set");
    let cell = Cell::from_char('X').with_fg(PackedRgba::rgb(255, 0, 0));

    // set_raw: no scissor/opacity check
    group.bench_function("set_raw_single", |b| {
        let mut buf = Buffer::new(80, 24);
        b.iter(|| {
            buf.set_raw(black_box(40), black_box(12), cell);
            black_box(&buf);
        })
    });

    // set: with scissor/opacity
    group.bench_function("set_single", |b| {
        let mut buf = Buffer::new(80, 24);
        b.iter(|| {
            buf.set(black_box(40), black_box(12), cell);
            black_box(&buf);
        })
    });

    // set_raw: fill a full row
    group.bench_function("set_raw_row_80", |b| {
        let mut buf = Buffer::new(80, 24);
        b.iter(|| {
            for x in 0..80u16 {
                buf.set_raw(x, 12, cell);
            }
            black_box(&buf);
        })
    });

    // set: fill a full row
    group.bench_function("set_row_80", |b| {
        let mut buf = Buffer::new(80, 24);
        b.iter(|| {
            for x in 0..80u16 {
                buf.set(x, 12, cell);
            }
            black_box(&buf);
        })
    });

    group.finish();
}

// =============================================================================
// Buffer fill
// =============================================================================

fn bench_buffer_fill(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer/fill");
    let cell = Cell::from_char('.').with_bg(PackedRgba::rgb(0, 0, 64));

    for (w, h) in [(80, 24), (120, 40), (200, 60)] {
        let cells = w as u64 * h as u64;
        group.throughput(Throughput::Elements(cells));
        group.bench_with_input(
            BenchmarkId::new("fill_all", format!("{w}x{h}")),
            &(),
            |b, _| {
                let mut buf = Buffer::new(w, h);
                let rect = Rect::from_size(w, h);
                b.iter(|| {
                    buf.fill(rect, cell);
                    black_box(&buf);
                })
            },
        );
    }

    // Partial fill (25% of buffer)
    group.bench_function("fill_quarter_80x24", |b| {
        let mut buf = Buffer::new(80, 24);
        let rect = Rect::new(0, 0, 40, 12);
        b.iter(|| {
            buf.fill(rect, cell);
            black_box(&buf);
        })
    });

    group.finish();
}

// =============================================================================
// Row access
// =============================================================================

fn bench_buffer_row_access(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer/row_access");

    for (w, h) in [(80, 24), (200, 60)] {
        let buf = Buffer::new(w, h);
        group.bench_with_input(
            BenchmarkId::new("row_cells_all", format!("{w}x{h}")),
            &buf,
            |b, buf| {
                b.iter(|| {
                    for y in 0..h {
                        black_box(buf.row_cells(y));
                    }
                })
            },
        );
    }

    // Single row access (typical use in diff)
    let buf = Buffer::new(80, 24);
    group.bench_function("row_cells_single_80", |b| {
        b.iter(|| black_box(buf.row_cells(black_box(12))))
    });

    group.finish();
}

// =============================================================================
// Scissor stack
// =============================================================================

fn bench_buffer_scissor(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer/scissor");

    group.bench_function("push_pop_single", |b| {
        let mut buf = Buffer::new(80, 24);
        let scissor = Rect::new(10, 5, 60, 14);
        b.iter(|| {
            buf.push_scissor(scissor);
            black_box(&buf);
            buf.pop_scissor();
        })
    });

    group.bench_function("push_pop_nested_3", |b| {
        let mut buf = Buffer::new(80, 24);
        let s1 = Rect::new(5, 2, 70, 20);
        let s2 = Rect::new(10, 5, 60, 14);
        let s3 = Rect::new(20, 8, 40, 8);
        b.iter(|| {
            buf.push_scissor(s1);
            buf.push_scissor(s2);
            buf.push_scissor(s3);
            black_box(&buf);
            buf.pop_scissor();
            buf.pop_scissor();
            buf.pop_scissor();
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_buffer_new,
    bench_buffer_clone,
    bench_buffer_set,
    bench_buffer_fill,
    bench_buffer_row_access,
    bench_buffer_scissor,
);
criterion_main!(benches);
