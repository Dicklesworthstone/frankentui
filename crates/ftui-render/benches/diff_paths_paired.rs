//! G49 (bd-g00-root-epic-ewths.48.1 / .48.2): the diff scans on identical
//! buffer pairs, interleaved in one invocation.
//!
//! - `certified`: what `TerminalWriter` runs by default (`certified_skips:
//!   true` under `DiffStrategy::DirtyRows`): `evaluate_render_certificate_for_buffers`
//!   cross-checking `dirty_rows`, `dirty_spans`, `dirty_cells`, and baseline
//!   generation, then `compute_certified_into(.., into_hint())` which narrows
//!   each dirty row to `new.dirty_span_row(row)` without a redundant full-row
//!   equality pre-scan.
//! - `incumbent_row_slice`: the pre-`.48.2` `compute_certified_into`
//!   implementation (whole-row equality pre-check + full-row scan, ignoring
//!   `dirty_spans`) retained live in the same invocation so the `.48.2`
//!   before/after delta is measured directly under identical load.
//! - `dirty`: `compute_dirty_into` (dirty spans + SAT tile skip), the
//!   `certified_skips: false` path.
//! - `full`: `compute_into`, the `DiffStrategy::Full` path.
//! - `certified_present`: `certified` plus presenting the diff into a `Vec`
//!   (diff + emit), timed per frame for the founding plan's v1 budgets.
//!
//! `new` carries dirty marks for the cells written (including realistic
//! widget over-marking in `overmarked_chrome`). Every path's change list is
//! checked against `full` before timing.
//!
//! Each row also records what the strategy selector predicts for the frame,
//! fed as `TerminalWriter` feeds it (span coverage as the DirtyRows scan
//! estimate), from a fresh selector with the default config.
//!
//! Each round times one batch of every path, in an order rotated per round;
//! a sample is a batch's mean ns per call. Ratios are of medians, with a 95%
//! bootstrap interval over rounds resampled as pairs. One JSON line per
//! workload on stdout; host, load and commit are the runner's to record.
//!
//! Run with: cargo bench -p ftui-render --bench diff_paths_paired

use std::hint::black_box;
use std::time::Instant;

use ftui_core::terminal_capabilities::{ColorDepth, TerminalCapabilities};
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::diff::BufferDiff;
use ftui_render::diff_strategy::{DiffStrategyConfig, DiffStrategySelector};
use ftui_render::presenter::Presenter;
use ftui_render::render_certificate::{
    RenderCertificateInputs, evaluate_render_certificate, evaluate_render_certificate_for_buffers,
};

const SIZES: [(u16, u16); 4] = [(80, 24), (120, 40), (200, 60), (300, 100)];
const ROUNDS: usize = 41;
/// Wall time one timed batch aims for.
const BATCH_TARGET_NS: f64 = 300_000.0;
const BOOTSTRAP_RESAMPLES: usize = 2_000;
const PATHS: [&str; 5] = [
    "certified",
    "incumbent_row_slice",
    "dirty",
    "full",
    "certified_present",
];

#[derive(Clone, Copy)]
enum Pattern {
    SingleCell,
    Scattered5,
    Rows5,
    OneRow,
    Spinner,
    StatusLine,
    OvermarkedChrome,
    LogScroll,
    Dense50,
    Full100,
}

impl Pattern {
    const ALL: [Self; 10] = [
        Self::SingleCell,
        Self::Scattered5,
        Self::Rows5,
        Self::OneRow,
        Self::Spinner,
        Self::StatusLine,
        Self::OvermarkedChrome,
        Self::LogScroll,
        Self::Dense50,
        Self::Full100,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::SingleCell => "single_cell",
            Self::Scattered5 => "scattered_5pct",
            Self::Rows5 => "rows_5pct",
            Self::OneRow => "one_full_row",
            Self::Spinner => "spinner_tick",
            Self::StatusLine => "status_line",
            Self::OvermarkedChrome => "overmarked_chrome",
            Self::LogScroll => "log_scroll",
            Self::Dense50 => "dense_50pct",
            Self::Full100 => "full_100pct",
        }
    }
}

fn text_cell(x: u16, y: u16) -> Cell {
    // Distinct per row, so a one-row shift changes every cell.
    let index = (u32::from(x) + 3 * u32::from(y)) % 26;
    Cell::from_char(char::from(b'a' + u8::try_from(index).unwrap()))
}

fn changed_cell(x: u16, y: u16) -> Cell {
    text_cell(x, y).with_fg(PackedRgba::rgb(255, 64, 0))
}

fn make_pair(pattern: Pattern, width: u16, height: u16) -> (Buffer, Buffer) {
    let mut old = Buffer::new(width, height);
    for y in 0..height {
        for x in 0..width {
            old.set_raw(x, y, text_cell(x, y));
        }
    }
    old.clear_dirty();
    let mut new = old.clone();
    new.clear_dirty();

    let total = u32::from(width) * u32::from(height);
    let at = |index: u32| {
        (
            u16::try_from(index % u32::from(width)).unwrap(),
            u16::try_from(index / u32::from(width)).unwrap(),
        )
    };
    match pattern {
        Pattern::SingleCell => {
            new.set_raw(width / 2, height / 2, changed_cell(width / 2, height / 2))
        }
        Pattern::Scattered5 => {
            let count = total.div_ceil(20);
            for i in 0..count {
                let (x, y) = at(i * total / count);
                new.set_raw(x, y, changed_cell(x, y));
            }
        }
        Pattern::Rows5 => {
            let rows = u32::from(height).div_ceil(20);
            for i in 0..rows {
                let y = u16::try_from(i * u32::from(height) / rows).unwrap();
                for x in 0..width {
                    new.set_raw(x, y, changed_cell(x, y));
                }
            }
        }
        Pattern::OneRow => {
            for x in 0..width {
                new.set_raw(x, height / 2, changed_cell(x, height / 2));
            }
        }
        Pattern::Spinner => new.set_raw(1, height - 1, changed_cell(1, height - 1)),
        Pattern::StatusLine => {
            let start = width / 4;
            for x in start..start + (width / 2).min(30) {
                new.set_raw(x, height - 1, changed_cell(x, height - 1));
            }
        }
        Pattern::OvermarkedChrome => {
            // Realistic widget redraw: widgets re-emit unchanged left/right
            // border chrome on 25% of rows and unchanged header cells on row 0,
            // while only 4 status counter cells on the last row actually change.
            for x in 0..width.min(24) {
                new.set_raw(x, 0, text_cell(x, 0));
            }
            for y in (1..height.saturating_sub(1)).step_by(4) {
                new.set_raw(0, y, text_cell(0, y));
                new.set_raw(width - 1, y, text_cell(width - 1, y));
            }
            let status_y = height - 1;
            for x in 2..6u16.min(width) {
                new.set_raw(x, status_y, changed_cell(x, status_y));
            }
        }
        Pattern::LogScroll => {
            for y in 0..height {
                for x in 0..width {
                    let cell = if y + 1 < height {
                        text_cell(x, y + 1)
                    } else {
                        changed_cell(x, y)
                    };
                    new.set_raw(x, y, cell);
                }
            }
        }
        Pattern::Dense50 => {
            for y in 0..height {
                for x in 0..width {
                    if (x + y) % 2 == 0 {
                        new.set_raw(x, y, changed_cell(x, y));
                    }
                }
            }
        }
        Pattern::Full100 => {
            for y in 0..height {
                for x in 0..width {
                    new.set_raw(x, y, changed_cell(x, y));
                }
            }
        }
    }
    (old, new)
}

fn certified(diff: &mut BufferDiff, old: &Buffer, new: &Buffer) {
    // The production sequence (terminal_writer.rs, DiffStrategy::DirtyRows).
    let certificate = evaluate_render_certificate_for_buffers(Some(old), new, false);
    diff.compute_certified_into(old, new, certificate.into_hint());
}

/// The pre-`.48.2` incumbent `compute_certified_into` implementation kept live
/// in the same invocation for direct A/B comparison.
fn incumbent_row_slice(diff: &mut BufferDiff, old: &Buffer, new: &Buffer) {
    let dirty_row_count = new.dirty_row_count();
    let certificate = evaluate_render_certificate(
        &RenderCertificateInputs {
            prev_available: true,
            dims_changed: false,
            full_redraw_due: false,
            dirty_row_count,
            span_dirty_rows: dirty_row_count,
            total_rows: new.height(),
            baseline_valid: true,
        },
        new.dirty_row_indices(),
    );
    diff.compute_certified_row_slice_into(old, new, certificate.to_hint());
}

struct Workload {
    old: Buffer,
    new: Buffer,
    diff: BufferDiff,
    caps: TerminalCapabilities,
    sink: Vec<u8>,
}

impl Workload {
    fn run(&mut self, path: usize) {
        match path {
            0 => certified(&mut self.diff, &self.old, &self.new),
            1 => incumbent_row_slice(&mut self.diff, &self.old, &self.new),
            2 => self.diff.compute_dirty_into(&self.old, &self.new),
            3 => self.diff.compute_into(&self.old, &self.new),
            _ => {
                certified(&mut self.diff, &self.old, &self.new);
                self.sink.clear();
                let mut presenter = Presenter::new(&mut self.sink, self.caps);
                let _ = presenter.present(&self.new, &self.diff);
            }
        }
        black_box(self.diff.len());
    }
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    // Nearest rank.
    let rank = (q * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    percentile(values, 0.5)
}

fn summary(samples: &[f64]) -> String {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    format!(
        r#"{{"p50":{:.1},"p95":{:.1},"p99":{:.1}}}"#,
        percentile(&sorted, 0.50),
        percentile(&sorted, 0.95),
        percentile(&sorted, 0.99)
    )
}

/// Ratio of medians `a / b` with a paired bootstrap 95% interval.
fn ratio(a: &[f64], b: &[f64], seed: u64) -> String {
    let point = median(&mut a.to_vec()) / median(&mut b.to_vec());
    let mut state = seed | 1;
    let mut next = || {
        // xorshift64*: deterministic, so a rerun reproduces the interval.
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };
    let mut ratios = Vec::with_capacity(BOOTSTRAP_RESAMPLES);
    let mut pick_a = vec![0.0; a.len()];
    let mut pick_b = vec![0.0; b.len()];
    for _ in 0..BOOTSTRAP_RESAMPLES {
        for slot in 0..a.len() {
            let index = usize::try_from(next() % a.len() as u64).unwrap();
            pick_a[slot] = a[index];
            pick_b[slot] = b[index];
        }
        ratios.push(median(&mut pick_a) / median(&mut pick_b));
    }
    ratios.sort_by(f64::total_cmp);
    format!(
        r#"{{"median":{point:.3},"ci95":[{:.3},{:.3}]}}"#,
        percentile(&ratios, 0.025),
        percentile(&ratios, 0.975)
    )
}

fn main() {
    // `cargo bench` passes `--bench`; nothing here takes arguments.
    let caps = TerminalCapabilities::builder()
        .color_depth(ColorDepth::TrueColor)
        .build();
    println!(
        r#"{{"bench":"diff_paths_paired","rounds":{ROUNDS},"batch_target_ns":{BATCH_TARGET_NS},"bootstrap_resamples":{BOOTSTRAP_RESAMPLES},"paths":["certified","incumbent_row_slice","dirty","full","certified_present"]}}"#
    );
    for (width, height) in SIZES {
        for pattern in Pattern::ALL {
            let (old, new) = make_pair(pattern, width, height);
            let mut workload = Workload {
                old,
                new,
                diff: BufferDiff::new(),
                caps,
                sink: Vec::with_capacity(usize::from(width) * usize::from(height) * 32),
            };

            // Correctness preflight: every path finds the same changes.
            workload.run(3);
            let expected = workload.diff.changes().to_vec();
            let full_scanned_cells = workload.diff.last_scanned_cells();

            workload.run(0);
            assert_eq!(
                workload.diff.changes(),
                expected.as_slice(),
                "{} {}x{}: certified disagrees with full",
                pattern.name(),
                width,
                height,
            );
            let certified_scanned_cells = workload.diff.last_scanned_cells();

            workload.run(1);
            assert_eq!(
                workload.diff.changes(),
                expected.as_slice(),
                "{} {}x{}: incumbent_row_slice disagrees with full",
                pattern.name(),
                width,
                height,
            );
            let incumbent_scanned_cells = workload.diff.last_scanned_cells();

            workload.run(2);
            assert_eq!(
                workload.diff.changes(),
                expected.as_slice(),
                "{} {}x{}: dirty disagrees with full",
                pattern.name(),
                width,
                height,
            );
            let dirty_scanned_cells = workload.diff.last_scanned_cells();
            let tile = workload.diff.last_tile_stats();

            let dirty_rows = workload.new.dirty_row_indices();
            let width_cells = usize::from(width);
            workload.run(4);
            let present_bytes = workload.sink.len();

            // The writer's selector input (terminal_writer.rs, bayesian path).
            let span_cells = workload.new.dirty_span_stats().span_coverage_cells;
            let scan_estimate = if span_cells > 0 {
                span_cells
            } else {
                dirty_rows.len() * width_cells
            };
            let mut selector = DiffStrategySelector::new(DiffStrategyConfig::default());
            let selected =
                selector.select_with_scan_estimate(width, height, dirty_rows.len(), scan_estimate);
            let selector_json = selector.last_evidence().map_or_else(
                || "null".to_owned(),
                |e| {
                    format!(
                        r#"{{"selected":"{selected:?}","scan_estimate_cells":{scan_estimate},"cost_full":{:.1},"cost_dirty":{:.1},"cost_redraw":{:.1},"guard":"{}"}}"#,
                        e.cost_full, e.cost_dirty, e.cost_redraw, e.guard_reason
                    )
                },
            );

            // Warm-up and batch sizing.
            let mut iterations = [0u32; 5];
            for (path, slot) in iterations.iter_mut().enumerate() {
                let start = Instant::now();
                for _ in 0..64 {
                    workload.run(path);
                }
                let per_call = start.elapsed().as_nanos() as f64 / 64.0;
                *slot = (BATCH_TARGET_NS / per_call.max(1.0)).clamp(1.0, 1_000_000.0) as u32;
            }

            let mut batches: [Vec<f64>; 5] = Default::default();
            let mut frames = Vec::new();
            for round in 0..ROUNDS {
                for offset in 0..PATHS.len() {
                    let path = (round + offset) % PATHS.len();
                    let start = Instant::now();
                    for _ in 0..iterations[path] {
                        if path == 4 {
                            let frame = Instant::now();
                            workload.run(path);
                            frames.push(frame.elapsed().as_nanos() as f64);
                        } else {
                            workload.run(path);
                        }
                    }
                    batches[path]
                        .push(start.elapsed().as_nanos() as f64 / f64::from(iterations[path]));
                }
            }

            let seed = (u64::from(width) << 32) | (u64::from(height) << 8) | pattern as u64;
            let tile_json = tile.map_or_else(
                || "null".to_owned(),
                |t| {
                    format!(
                        r#"{{"fallback":"{}","scanned_tiles":{},"skipped_tile_rows":{},"scan_cells_estimate":{}}}"#,
                        t.fallback.map_or("none", |f| f.as_str()),
                        t.scanned_tiles,
                        t.skipped_tile_rows,
                        t.scan_cells_estimate
                    )
                },
            );
            println!(
                r#"{{"size":"{width}x{height}","pattern":"{}","changed_cells":{},"dirty_rows":{},"span_coverage_cells":{},"certified_scan_cells":{certified_scanned_cells},"incumbent_row_slice_scan_cells":{incumbent_scanned_cells},"dirty_scan_cells":{dirty_scanned_cells},"full_scan_cells":{full_scanned_cells},"dirty_tile_stats":{tile_json},"selector":{selector_json},"certified_ns":{},"incumbent_row_slice_ns":{},"dirty_ns":{},"full_ns":{},"certified_present_ns":{},"certified_present_frame_ns":{},"present_bytes":{present_bytes},"ratio_certified_over_incumbent":{},"ratio_certified_over_dirty":{},"ratio_certified_over_full":{},"ratio_dirty_over_full":{}}}"#,
                pattern.name(),
                expected.len(),
                dirty_rows.len(),
                span_cells,
                summary(&batches[0]),
                summary(&batches[1]),
                summary(&batches[2]),
                summary(&batches[3]),
                summary(&batches[4]),
                summary(&frames),
                ratio(&batches[0], &batches[1], seed ^ 0x517C),
                ratio(&batches[0], &batches[2], seed),
                ratio(&batches[0], &batches[3], seed ^ 0x9E37),
                ratio(&batches[2], &batches[3], seed ^ 0x7F4A),
            );
        }
    }
}
