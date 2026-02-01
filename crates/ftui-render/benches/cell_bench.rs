//! Benchmarks for Cell operations (bd-19x)
//!
//! Performance budgets:
//! - Cell comparison: < 1ns
//! - Cell bits_eq SIMD: < 0.5ns
//!
//! Run with: cargo bench -p ftui-render --bench cell_bench

use criterion::{Criterion, criterion_group, criterion_main};
use ftui_render::cell::{Cell, CellAttrs, CellContent, PackedRgba, StyleFlags};
use std::hint::black_box;

// =============================================================================
// Cell creation
// =============================================================================

fn bench_cell_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("cell/create");

    group.bench_function("default", |b| b.iter(|| black_box(Cell::default())));

    group.bench_function("from_char_ascii", |b| {
        b.iter(|| black_box(Cell::from_char('A')))
    });

    group.bench_function("from_char_cjk", |b| {
        b.iter(|| black_box(Cell::from_char('\u{4E2D}')))
    });

    group.bench_function("with_fg_bg", |b| {
        b.iter(|| {
            black_box(
                Cell::from_char('X')
                    .with_fg(PackedRgba::rgb(255, 128, 0))
                    .with_bg(PackedRgba::rgb(0, 0, 128)),
            )
        })
    });

    group.finish();
}

// =============================================================================
// Cell comparison (the hot path for diffing)
// =============================================================================

fn bench_cell_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("cell/compare");

    let cell_a = Cell::from_char('A').with_fg(PackedRgba::rgb(255, 0, 0));
    let cell_b = Cell::from_char('A').with_fg(PackedRgba::rgb(255, 0, 0));
    let cell_c = Cell::from_char('B').with_fg(PackedRgba::rgb(0, 255, 0));

    // bits_eq: branchless SIMD-friendly comparison
    group.bench_function("bits_eq_same", |b| {
        b.iter(|| black_box(cell_a.bits_eq(black_box(&cell_b))))
    });

    group.bench_function("bits_eq_different", |b| {
        b.iter(|| black_box(cell_a.bits_eq(black_box(&cell_c))))
    });

    // PartialEq: standard Rust comparison
    group.bench_function("partial_eq_same", |b| {
        b.iter(|| black_box(black_box(&cell_a) == black_box(&cell_b)))
    });

    group.bench_function("partial_eq_different", |b| {
        b.iter(|| black_box(black_box(&cell_a) == black_box(&cell_c)))
    });

    // Compare default (empty) cells — common case
    let empty_a = Cell::default();
    let empty_b = Cell::default();
    group.bench_function("bits_eq_default", |b| {
        b.iter(|| black_box(empty_a.bits_eq(black_box(&empty_b))))
    });

    group.finish();
}

// =============================================================================
// CellContent operations
// =============================================================================

fn bench_cell_content(c: &mut Criterion) {
    let mut group = c.benchmark_group("cell/content");

    let ascii_content = CellContent::from_char('A');
    let cjk_content = CellContent::from_char('\u{4E2D}');

    group.bench_function("width_hint_ascii", |b| {
        b.iter(|| black_box(black_box(ascii_content).width_hint()))
    });

    group.bench_function("width_unicode", |b| {
        b.iter(|| black_box(black_box(cjk_content).width()))
    });

    group.bench_function("is_empty", |b| {
        b.iter(|| black_box(black_box(CellContent::EMPTY).is_empty()))
    });

    group.bench_function("is_continuation", |b| {
        b.iter(|| black_box(black_box(CellContent::CONTINUATION).is_continuation()))
    });

    group.bench_function("as_char", |b| {
        b.iter(|| black_box(black_box(ascii_content).as_char()))
    });

    group.finish();
}

// =============================================================================
// PackedRgba operations
// =============================================================================

fn bench_packed_rgba(c: &mut Criterion) {
    let mut group = c.benchmark_group("cell/packed_rgba");

    let src = PackedRgba::rgba(200, 100, 50, 180);
    let dst = PackedRgba::rgba(50, 100, 200, 255);

    group.bench_function("rgb_create", |b| {
        b.iter(|| black_box(PackedRgba::rgb(255, 128, 0)))
    });

    group.bench_function("rgba_create", |b| {
        b.iter(|| black_box(PackedRgba::rgba(255, 128, 0, 200)))
    });

    group.bench_function("channel_extract", |b| {
        b.iter(|| {
            let c = black_box(src);
            black_box((c.r(), c.g(), c.b(), c.a()))
        })
    });

    group.bench_function("over_partial", |b| {
        b.iter(|| black_box(black_box(src).over(black_box(dst))))
    });

    group.bench_function("over_opaque", |b| {
        let opaque = PackedRgba::rgb(200, 100, 50);
        b.iter(|| black_box(black_box(opaque).over(black_box(dst))))
    });

    group.bench_function("over_transparent", |b| {
        b.iter(|| black_box(black_box(PackedRgba::TRANSPARENT).over(black_box(dst))))
    });

    group.bench_function("with_opacity", |b| {
        b.iter(|| black_box(black_box(src).with_opacity(black_box(0.5))))
    });

    group.finish();
}

// =============================================================================
// Row-level comparison (simulating diff inner loop)
// =============================================================================

fn bench_row_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("cell/row_compare");

    // Simulate comparing two rows of 80 cells
    let row_a: Vec<Cell> = (0..80)
        .map(|i| Cell::from_char(char::from(b'A' + (i % 26) as u8)))
        .collect();
    let row_b = row_a.clone();
    let mut row_c = row_a.clone();
    row_c[40] = Cell::from_char('!').with_fg(PackedRgba::rgb(255, 0, 0));

    group.bench_function("80_cells_identical", |b| {
        b.iter(|| {
            let mut all_eq = true;
            for (a, bb) in black_box(&row_a).iter().zip(black_box(&row_b).iter()) {
                all_eq &= a.bits_eq(bb);
            }
            black_box(all_eq)
        })
    });

    group.bench_function("80_cells_one_diff", |b| {
        b.iter(|| {
            let mut all_eq = true;
            for (a, cc) in black_box(&row_a).iter().zip(black_box(&row_c).iter()) {
                all_eq &= a.bits_eq(cc);
            }
            black_box(all_eq)
        })
    });

    // 200-column row
    let wide_row: Vec<Cell> = (0..200)
        .map(|i| Cell::from_char(char::from(b'A' + (i % 26) as u8)))
        .collect();
    let wide_row_b = wide_row.clone();

    group.bench_function("200_cells_identical", |b| {
        b.iter(|| {
            let mut all_eq = true;
            for (a, bb) in black_box(&wide_row)
                .iter()
                .zip(black_box(&wide_row_b).iter())
            {
                all_eq &= a.bits_eq(bb);
            }
            black_box(all_eq)
        })
    });

    group.finish();
}

// =============================================================================
// StyleFlags and CellAttrs
// =============================================================================

fn bench_attrs(c: &mut Criterion) {
    let mut group = c.benchmark_group("cell/attrs");

    let flags = StyleFlags::BOLD | StyleFlags::ITALIC;
    let attrs = CellAttrs::new(flags, 42);

    group.bench_function("flags_extract", |b| {
        b.iter(|| black_box(black_box(attrs).flags()))
    });

    group.bench_function("link_id_extract", |b| {
        b.iter(|| black_box(black_box(attrs).link_id()))
    });

    group.bench_function("with_flags", |b| {
        b.iter(|| black_box(black_box(attrs).with_flags(black_box(flags))))
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_cell_creation,
    bench_cell_comparison,
    bench_cell_content,
    bench_packed_rgba,
    bench_row_comparison,
    bench_attrs,
);
criterion_main!(benches);
