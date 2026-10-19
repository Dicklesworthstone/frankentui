#![forbid(unsafe_code)]

//! Benchmarks for the Presenter ANSI output pipeline (bd-19x, bd-g00-root-epic-ewths.31.2)
//!
//! Run with: cargo bench -p ftui-render --bench presenter_bench
//!
//! Measures end-to-end present performance at various terminal sizes
//! and change percentages. Writes to a Vec<u8> to isolate CPU cost
//! from I/O, and emits `bytes_report.json` via `CountingWriter`.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group};
use ftui_core::terminal_capabilities::{ColorDepth, TerminalCapabilities};
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::counting_writer::{
    CountingWriter, StatsCollector, expected_max_bytes, plan_bytes_budget,
};
use ftui_render::diff::BufferDiff;
use ftui_render::presenter::Presenter;
use serde::Serialize;
use std::hint::black_box;
use std::path::PathBuf;

const BENCHMARK_COLOR_DEPTH: ColorDepth = ColorDepth::TrueColor;

fn benchmark_capabilities() -> TerminalCapabilities {
    TerminalCapabilities::builder()
        .color_depth(BENCHMARK_COLOR_DEPTH)
        .build()
}

/// Create a pair of buffers where exactly `change_pct` percent of cells have changed,
/// with varied styles to exercise the presenter's state tracking.
fn make_styled_pair(width: u16, height: u16, change_pct: u8) -> (Buffer, Buffer) {
    assert!(
        width > 0 && height > 0,
        "fixture dimensions must be nonzero"
    );
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

/// Create a pair of buffers where `change_pct` percent of cells change across
/// contiguous horizontal text runs (realistic status/log line updates) for
/// byte-efficiency budget verification (`8 * cells_changed + 64`).
fn make_sparse_runs_pair(width: u16, height: u16, change_pct: u8) -> (Buffer, Buffer) {
    let total = usize::from(width) * usize::from(height);
    let to_change = (total * usize::from(change_pct)) / 100;
    let old = Buffer::new(width, height);
    let mut new = old.clone();

    let fg = PackedRgba::rgb(180, 220, 255);
    let bg = PackedRgba::rgb(20, 24, 32);
    let mut remaining = to_change;
    let mut row = 0u16;
    while remaining > 0 && row < height {
        let run_len = remaining.min(usize::from(width) / 2).max(1);
        for col in 0..run_len {
            let x = u16::try_from(col).unwrap();
            let ch = char::from(b'a' + u8::try_from((col + usize::from(row)) % 26).unwrap());
            new.set_raw(x, row, Cell::from_char(ch).with_fg(fg).with_bg(bg));
        }
        remaining -= run_len;
        row = row.saturating_add(2);
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

    for (w, h, pct) in [
        (80, 24, 5),
        (80, 24, 50),
        (120, 40, 5),
        (120, 40, 50),
        (200, 60, 5),
        (200, 60, 50),
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

#[derive(Debug, Serialize)]
struct BytesReportEntry {
    scene: String,
    cols: u16,
    rows: u16,
    cells_changed: usize,
    runs: usize,
    bytes: u64,
    budget: u64,
    expected_max_bytes: u64,
    within_budget: bool,
    within_expected_max_bytes: bool,
}

fn report_output_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FTUI_PERF_REPORT_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    if let Ok(dir) = std::env::var("PERF_GATE_RESULTS_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/regression-gate")
}

fn write_bytes_report() {
    let caps = benchmark_capabilities();
    let mut entries = Vec::with_capacity(3);

    for (w, h) in [(80, 24), (120, 40), (200, 60)] {
        let (old, new) = make_sparse_runs_pair(w, h, 5);
        let diff = BufferDiff::compute(&old, &new);
        let runs = diff.runs().len();
        let cells_changed = diff.len();

        let mut raw_buf = Vec::with_capacity(16384);
        let mut counting = CountingWriter::new(&mut raw_buf);
        let collector = StatsCollector::start(cells_changed, runs);
        {
            let mut presenter = Presenter::new(&mut counting, caps);
            presenter
                .present(&new, &diff)
                .expect("present sparse 5% frame through CountingWriter");
        }
        let bytes = counting.bytes_written();
        let stats = collector.finish(bytes);
        let budget = plan_bytes_budget(cells_changed);
        let max_bytes = expected_max_bytes(cells_changed, runs);
        let within_expected = stats.within_budget();
        let within_budget = bytes <= budget && within_expected;
        assert!(
            within_expected,
            "PresentStats::within_budget failed for {w}x{h}: bytes={bytes} > expected_max={max_bytes}"
        );
        assert!(
            within_budget,
            "plan_bytes_budget failed for {w}x{h}: bytes={bytes} > budget={budget}"
        );

        let scene = format!("present/bytes/sparse_5pct/{w}x{h}");
        eprintln!(
            r#"{{"bench":"presenter_bench","id":"{scene}","cells_changed":{cells_changed},"runs":{runs},"bytes":{bytes},"budget":{budget},"within_budget":{within_budget}}}"#
        );
        entries.push(BytesReportEntry {
            scene,
            cols: w,
            rows: h,
            cells_changed,
            runs,
            bytes,
            budget,
            expected_max_bytes: max_bytes,
            within_budget,
            within_expected_max_bytes: within_expected,
        });
    }

    let out_dir = report_output_dir();
    if std::fs::create_dir_all(&out_dir).is_ok() {
        let report_path = out_dir.join("bytes_report.json");
        if let Ok(json) = serde_json::to_string_pretty(&entries) {
            let _ = std::fs::write(report_path, format!("{json}\n"));
        }
    }
}

fn emit_criterion_jsonl(bench_name: &str, ids: &[&str]) {
    let target_dir = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".to_string());
    for &id in ids {
        let estimates_path = std::path::Path::new(&target_dir)
            .join("criterion")
            .join(id)
            .join("new")
            .join("estimates.json");
        if let Ok(text) = std::fs::read_to_string(&estimates_path)
            && let Some(mean_ns) = extract_point_estimate(&text, "mean")
        {
            let p50_ns = extract_point_estimate(&text, "median").unwrap_or(mean_ns);
            let p99_ns = extract_upper_ci(&text, "mean").unwrap_or(mean_ns);
            eprintln!(
                r#"{{"bench":"{bench_name}","id":"{id}","mean_ns":{mean_ns:.2},"p50_ns":{p50_ns:.2},"p99_ns":{p99_ns:.2}}}"#
            );
        }
    }
}

fn extract_point_estimate(json: &str, section: &str) -> Option<f64> {
    let key = format!("\"{section}\":");
    let start = json.find(&key)? + key.len();
    let rest = &json[start..];
    let pe_key = "\"point_estimate\":";
    let pe_start = rest.find(pe_key)? + pe_key.len();
    let tail = rest[pe_start..].trim_start();
    let end = tail.find([',', '}', ' ', '\n', '\r'])?;
    tail[..end].parse::<f64>().ok()
}

fn extract_upper_ci(json: &str, section: &str) -> Option<f64> {
    let key = format!("\"{section}\":");
    let start = json.find(&key)? + key.len();
    let rest = &json[start..];
    let ci_key = "\"upper_bound\":";
    let ci_start = rest.find(ci_key)? + ci_key.len();
    let tail = rest[ci_start..].trim_start();
    let end = tail.find([',', '}', ' ', '\n', '\r'])?;
    tail[..end].parse::<f64>().ok()
}

criterion_group!(
    benches,
    bench_present_sparse,
    bench_present_heavy,
    bench_present_full,
    bench_pipeline,
);

fn main() {
    benches();
    emit_criterion_jsonl(
        "presenter_bench",
        &[
            "present/truecolor/sparse_5pct/present/80x24",
            "present/truecolor/sparse_5pct/present/120x40",
            "present/truecolor/sparse_5pct/present/200x60",
            "pipeline/truecolor/diff_and_present/full/80x24@5%",
            "pipeline/truecolor/diff_and_present/full/120x40@5%",
            "pipeline/truecolor/diff_and_present/full/200x60@5%",
        ],
    );
    write_bytes_report();
}
