//! Benchmarks for the Presenter ANSI output pipeline (bd-19x)
//!
//! Run with: cargo bench -p ftui-render --bench presenter_bench
//!
//! Measures end-to-end present performance at various terminal sizes
//! and change percentages. Writes to a Vec<u8> to isolate CPU cost
//! from I/O.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::diff::BufferDiff;
use ftui_render::presenter::Presenter;
use std::hint::black_box;

/// Create a pair of buffers where `pct` percent of cells have changed,
/// with varied styles to exercise the presenter's state tracking.
fn make_styled_pair(width: u16, height: u16, change_pct: f64) -> (Buffer, Buffer) {
    let old = Buffer::new(width, height);
    let mut new = old.clone();

    let total = width as usize * height as usize;
    let to_change = ((total as f64) * change_pct / 100.0) as usize;

    let colors = [
        PackedRgba::rgb(255, 0, 0),
        PackedRgba::rgb(0, 255, 0),
        PackedRgba::rgb(0, 0, 255),
        PackedRgba::rgb(255, 255, 0),
        PackedRgba::rgb(255, 0, 255),
    ];

    for i in 0..to_change {
        let x = (i * 7 + 3) as u16 % width;
        let y = (i * 11 + 5) as u16 % height;
        let ch = char::from_u32(('A' as u32) + (i as u32 % 26)).unwrap();
        let fg = colors[i % colors.len()];
        let bg = colors[(i + 2) % colors.len()];
        new.set_raw(x, y, Cell::from_char(ch).with_fg(fg).with_bg(bg));
    }

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
    let mut group = c.benchmark_group("present/sparse_5pct");
    let caps = TerminalCapabilities::default();

    for (w, h) in [(80, 24), (120, 40), (200, 60)] {
        let (old, new) = make_styled_pair(w, h, 5.0);
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
    let mut group = c.benchmark_group("present/heavy_50pct");
    let caps = TerminalCapabilities::default();

    for (w, h) in [(80, 24), (120, 40), (200, 60)] {
        let (old, new) = make_styled_pair(w, h, 50.0);
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
    let mut group = c.benchmark_group("present/full_100pct");
    let caps = TerminalCapabilities::default();

    for (w, h) in [(80, 24), (200, 60)] {
        let (old, new) = make_styled_pair(w, h, 100.0);
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
    let mut group = c.benchmark_group("pipeline/diff_and_present");
    let caps = TerminalCapabilities::default();

    for (w, h, pct) in [
        (80, 24, 5.0),
        (80, 24, 50.0),
        (200, 60, 5.0),
        (200, 60, 50.0),
    ] {
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
