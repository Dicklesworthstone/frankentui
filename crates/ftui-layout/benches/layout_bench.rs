//! Benchmarks for the layout solver (bd-19x)
//!
//! Run with: cargo bench -p ftui-layout

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use ftui_core::geometry::Rect;
use ftui_layout::{Alignment, Constraint, Flex, Grid};
use std::hint::black_box;

/// Build a flex layout with `n` constraints of mixed types.
fn make_flex(n: usize) -> Flex {
    let constraints: Vec<Constraint> = (0..n)
        .map(|i| match i % 5 {
            0 => Constraint::Fixed(10),
            1 => Constraint::Percentage(20.0),
            2 => Constraint::Min(5),
            3 => Constraint::Max(30),
            4 => Constraint::Ratio(1, 3),
            _ => unreachable!(),
        })
        .collect();

    Flex::horizontal().constraints(constraints)
}

fn bench_flex_split(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/flex_split");
    let area = Rect::from_size(200, 60);

    for n in [3, 5, 10, 20, 50] {
        let flex = make_flex(n);
        group.bench_with_input(BenchmarkId::new("horizontal", n), &flex, |b, flex| {
            b.iter(|| black_box(flex.split(area)))
        });
    }

    group.finish();
}

fn bench_flex_vertical(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/flex_vertical");
    let area = Rect::from_size(80, 200);

    for n in [3, 10, 20, 50] {
        let constraints: Vec<Constraint> = (0..n)
            .map(|i| match i % 3 {
                0 => Constraint::Fixed(3),
                1 => Constraint::Min(1),
                2 => Constraint::Percentage(10.0),
                _ => unreachable!(),
            })
            .collect();

        let flex = Flex::vertical().constraints(constraints);
        group.bench_with_input(BenchmarkId::new("split", n), &flex, |b, flex| {
            b.iter(|| black_box(flex.split(area)))
        });
    }

    group.finish();
}

fn bench_flex_with_gap(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/flex_gap");
    let area = Rect::from_size(200, 60);

    for gap in [0, 1, 2, 4] {
        let flex = Flex::horizontal()
            .constraints(vec![Constraint::Percentage(25.0); 4])
            .gap(gap);

        group.bench_with_input(BenchmarkId::new("gap", gap), &flex, |b, flex| {
            b.iter(|| black_box(flex.split(area)))
        });
    }

    group.finish();
}

fn bench_flex_alignment(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/flex_alignment");
    let area = Rect::from_size(200, 60);

    for (name, alignment) in [
        ("start", Alignment::Start),
        ("center", Alignment::Center),
        ("end", Alignment::End),
        ("space_between", Alignment::SpaceBetween),
    ] {
        let flex = Flex::horizontal()
            .constraints(vec![Constraint::Fixed(20); 5])
            .alignment(alignment);

        group.bench_with_input(BenchmarkId::new("split", name), &flex, |b, flex| {
            b.iter(|| black_box(flex.split(area)))
        });
    }

    group.finish();
}

/// Nested layout: split horizontally, then each column vertically.
fn bench_nested_layout(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/nested");
    let area = Rect::from_size(200, 60);

    let outer = Flex::horizontal().constraints(vec![Constraint::Percentage(33.3); 3]);

    let inner = Flex::vertical().constraints(vec![Constraint::Fixed(5); 10]);

    group.bench_function("3col_x_10row", |b| {
        b.iter(|| {
            let columns = outer.split(area);
            let mut all_rects = Vec::new();
            for col in &columns {
                all_rects.extend(inner.split(*col));
            }
            black_box(all_rects)
        })
    });

    group.finish();
}

// =============================================================================
// Grid layout solving (budget: 10x10 < 500µs)
// =============================================================================

fn bench_grid_split(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/grid");
    let area = Rect::from_size(200, 60);

    // 3x3 grid
    let grid_3x3 = Grid::new()
        .rows(vec![
            Constraint::Fixed(10),
            Constraint::Min(20),
            Constraint::Fixed(10),
        ])
        .columns(vec![
            Constraint::Fixed(30),
            Constraint::Min(100),
            Constraint::Fixed(30),
        ]);
    group.bench_function("split_3x3", |b| {
        b.iter(|| black_box(grid_3x3.split(black_box(area))))
    });

    // 10x10 grid (budget target: < 500µs)
    let grid_10x10 = Grid::new()
        .rows(
            (0..10)
                .map(|_| Constraint::Ratio(1, 10))
                .collect::<Vec<_>>(),
        )
        .columns(
            (0..10)
                .map(|_| Constraint::Ratio(1, 10))
                .collect::<Vec<_>>(),
        );
    group.bench_function("split_10x10", |b| {
        b.iter(|| black_box(grid_10x10.split(black_box(area))))
    });

    // 20x20 grid (stress test)
    let grid_20x20 = Grid::new()
        .rows(
            (0..20)
                .map(|_| Constraint::Ratio(1, 20))
                .collect::<Vec<_>>(),
        )
        .columns(
            (0..20)
                .map(|_| Constraint::Ratio(1, 20))
                .collect::<Vec<_>>(),
        );
    group.bench_function("split_20x20", |b| {
        b.iter(|| black_box(grid_20x20.split(black_box(area))))
    });

    // Mixed constraints grid
    let grid_mixed = Grid::new()
        .rows(vec![
            Constraint::Fixed(3),
            Constraint::Percentage(60.0),
            Constraint::Min(5),
            Constraint::Fixed(1),
        ])
        .columns(vec![
            Constraint::Fixed(20),
            Constraint::Min(40),
            Constraint::Percentage(30.0),
        ]);
    group.bench_function("split_4x3_mixed", |b| {
        b.iter(|| black_box(grid_mixed.split(black_box(area))))
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_flex_split,
    bench_flex_vertical,
    bench_flex_with_gap,
    bench_flex_alignment,
    bench_nested_layout,
    bench_grid_split,
);

criterion_main!(benches);
