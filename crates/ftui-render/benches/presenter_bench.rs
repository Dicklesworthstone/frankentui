//! Benchmarks for the Presenter ANSI output pipeline (bd-19x)
//!
//! Run with: cargo bench -p ftui-render --bench presenter_bench
//!
//! Measures end-to-end present performance at various terminal sizes
//! and change percentages. Writes to a Vec<u8> to isolate CPU cost
//! from I/O.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use ftui_core::terminal_capabilities::{ColorDepth, TerminalCapabilities};
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::diff::BufferDiff;
use ftui_render::presenter::Presenter;
use std::hint::black_box;

const BENCHMARK_COLOR_DEPTH: ColorDepth = ColorDepth::TrueColor;

fn benchmark_capabilities() -> TerminalCapabilities {
    TerminalCapabilities::builder()
        .color_depth(BENCHMARK_COLOR_DEPTH)
        .build()
}

/// Create a pair of buffers where exactly `change_pct` percent of cells have changed,
/// with varied styles to exercise the presenter's state tracking.
fn make_styled_pair(width: u16, height: u16, change_pct: u8) -> (Buffer, Buffer) {
    assert!(width > 0 && height > 0, "fixture dimensions must be nonzero");
    assert!(change_pct <= 100, "fixture density cannot exceed 100%");
    let total = u64::from(width) * u64::from(height);
    let scaled = total * u64::from(change_pct);
    assert_eq!(scaled % 100, 0, "fixture density must select whole cells");
    let to_change = scaled / 100;
    let old = Buffer::new(width, height);
    let mut new = old.clone();

    let colors = [
        PackedRgba::rgb(255, 0, 0),
        PackedRgba::rgb(0, 255, 0),
        PackedRgba::rgb(0, 0, 255),
        PackedRgba::rgb(255, 255, 0),
        PackedRgba::rgb(255, 0, 255),
    ];

    for i in 0..to_change {
        // Evenly spaced row-major positions are unique because total >= to_change.
        // Both factors are below 2^32, so their product fits in u64.
        let index = i * total / to_change;
        let x = u16::try_from(index % u64::from(width)).unwrap();
        let y = u16::try_from(index / u64::from(width)).unwrap();
        let ch = char::from(b'A' + u8::try_from(i % 26).unwrap());
        let color_index = usize::try_from(i % u64::try_from(colors.len()).unwrap()).unwrap();
        let fg = colors[color_index];
        let bg = colors[(color_index + 2) % colors.len()];
        new.set_raw(x, y, Cell::from_char(ch).with_fg(fg).with_bg(bg));
    }

    // Untimed fixture preflight: inspect cells directly, not through BufferDiff.
    let actual = old
        .cells()
        .iter()
        .zip(new.cells())
        .filter(|(old, new)| old != new)
        .count();
    assert_eq!(u64::try_from(actual).unwrap(), to_change);
    (old, new)
}

/// Present to a sink and return the byte count.
/// Isolates the presenter borrow from the sink length read.
fn present_to_vec(
    new: &Buffer,
    diff: &BufferDiff,
    caps: &TerminalCapabilities,
    capacity: usize,
) -> usize {
    let mut sink = Vec::with_capacity(capacity);
    {
        let mut presenter = Presenter::new(&mut sink, *caps);
        let _ = presenter.present(new, diff);
    }
    sink.len()
}

fn bench_present_sparse(c: &mut Criterion) {
    let mut group = c.benchmark_group("present/truecolor/sparse_5pct");
    let caps = benchmark_capabilities();

    for (w, h) in [(80, 24), (120, 40), (200, 60)] {
        let (old, new) = make_styled_pair(w, h, 5);
        let diff = BufferDiff::compute(&old, &new);

        group.throughput(Throughput::Elements(diff.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("present", format!("{w}x{h}")),
            &(),
            |b, _| b.iter(|| black_box(present_to_vec(&new, &diff, &caps, 16384))),
        );
    }

    group.finish();
}

fn bench_present_heavy(c: &mut Criterion) {
    let mut group = c.benchmark_group("present/truecolor/heavy_50pct");
    let caps = benchmark_capabilities();

    for (w, h) in [(80, 24), (120, 40), (200, 60)] {
        let (old, new) = make_styled_pair(w, h, 50);
        let diff = BufferDiff::compute(&old, &new);

        group.throughput(Throughput::Elements(diff.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("present", format!("{w}x{h}")),
            &(),
            |b, _| b.iter(|| black_box(present_to_vec(&new, &diff, &caps, 65536))),
        );
    }

    group.finish();
}

fn bench_present_full(c: &mut Criterion) {
    let mut group = c.benchmark_group("present/truecolor/full_100pct");
    let caps = benchmark_capabilities();

    for (w, h) in [(80, 24), (200, 60)] {
        let (old, new) = make_styled_pair(w, h, 100);
        let diff = BufferDiff::compute(&old, &new);

        group.throughput(Throughput::Elements(diff.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("present", format!("{w}x{h}")),
            &(),
            |b, _| b.iter(|| black_box(present_to_vec(&new, &diff, &caps, 65536))),
        );
    }

    group.finish();
}

/// Measure the full pipeline: diff + present (the hot path in real usage).
fn bench_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("pipeline/truecolor/diff_and_present");
    let caps = benchmark_capabilities();

    for (w, h, pct) in [(80, 24, 5), (80, 24, 50), (200, 60, 5), (200, 60, 50)] {
        let (old, new) = make_styled_pair(w, h, pct);

        group.throughput(Throughput::Elements(w as u64 * h as u64));
        group.bench_with_input(
            BenchmarkId::new("full", format!("{w}x{h}@{pct}%")),
            &(),
            |b, _| {
                b.iter(|| {
                    let diff = BufferDiff::compute(&old, &new);
                    black_box(present_to_vec(&new, &diff, &caps, 65536))
                })
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_present_sparse,
    bench_present_heavy,
    bench_present_full,
    bench_pipeline,
);

criterion_main!(benches);
