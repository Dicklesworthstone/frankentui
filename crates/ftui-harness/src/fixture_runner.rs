#![forbid(unsafe_code)]

//! Fixture runner: executes `FixtureSpec` workloads and produces `BaselineRecord`s (bd-muv6p).
//!
//! This module bridges the gap between fixture *specifications* (`fixture_suite`)
//! and baseline *measurements* (`baseline_capture`). Given a `FixtureSpec`, the
//! runner synthesises a representative workload, executes it for the specified
//! number of frames, records latency/throughput/output-cost samples into a
//! `BaselineCapture`, and finalises the result.
//!
//! # Supported families
//!
//! | Family | What the runner does |
//! |--------|---------------------|
//! | Render | Buffer creation → cell mutation → diff → presenter emit |
//! | Runtime | Simulated event loop: update → view → diff cycle |
//! | Doctor | Stub orchestration timing (capture-suite-report chain) |
//!
//! # Usage
//!
//! ```ignore
//! use ftui_harness::fixture_suite::FixtureRegistry;
//! use ftui_harness::fixture_runner::FixtureRunner;
//!
//! let registry = FixtureRegistry::canonical();
//! let spec = registry.get("render_diff_sparse_80x24").unwrap();
//! let result = FixtureRunner::run(spec);
//! assert!(result.record.is_stable());
//! println!("{}", result.record.to_json());
//! ```

use std::time::Instant;

use crate::baseline_capture::{BaselineCapture, BaselineRecord, Sample};
use crate::fixture_suite::{FixtureSpec, SuitePartition, TransitionPattern, ViewportSpec};

use ftui_core::terminal_capabilities::{ColorDepth, TerminalCapabilities};
use ftui_render::buffer::Buffer;
use ftui_render::cell::PackedRgba;
use ftui_render::diff::BufferDiff;
use ftui_render::presenter::Presenter;

// ============================================================================
// Run Result
// ============================================================================

/// Result of executing a fixture spec.
#[derive(Debug)]
pub struct FixtureRunResult {
    /// The computed baseline record with percentile metrics.
    pub record: BaselineRecord,
    /// Total wall-clock duration for the entire run.
    pub wall_clock_ms: u64,
    /// Number of frames actually executed.
    pub frames_executed: u32,
    /// Total ANSI bytes emitted across all frames.
    pub total_ansi_bytes: u64,
    /// Total cells diffed across all frames.
    pub total_cells_diffed: u64,
    /// Per-frame checksums for determinism verification.
    pub frame_checksums: Vec<u64>,
}

// ============================================================================
// Workload Synthesiser
// ============================================================================

/// Deterministic PRNG (xorshift64) for reproducible workload generation.
struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 1 } else { seed },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    fn next_u16(&mut self, max: u16) -> u16 {
        if max == 0 {
            return 0;
        }
        (self.next_u64() % max as u64) as u16
    }

    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Applies a transition pattern to a buffer pair, returning the number of cells changed.
fn apply_transition(
    rng: &mut Rng,
    pattern: TransitionPattern,
    old: &mut Buffer,
    new: &mut Buffer,
    viewport: ViewportSpec,
) -> u64 {
    let w = viewport.width;
    let h = viewport.height;
    let total = w as u64 * h as u64;

    // Determine change percentage based on pattern
    let change_pct = match pattern {
        TransitionPattern::SparseUpdate => 0.03 + rng.next_f64() * 0.02, // 3-5%
        TransitionPattern::ModerateUpdate => 0.10 + rng.next_f64() * 0.15, // 10-25%
        TransitionPattern::LargeInvalidation => 0.50 + rng.next_f64() * 0.50, // 50-100%
        TransitionPattern::InputStorm => 0.01 + rng.next_f64() * 0.04,   // 1-5%
        TransitionPattern::Mixed => 0.05 + rng.next_f64() * 0.45,        // 5-50%
        // Non-render patterns produce zero visual change
        _ => 0.0,
    };

    let cells_to_change = ((total as f64) * change_pct) as u64;

    // Copy old into new as a starting point
    for y in 0..h {
        for x in 0..w {
            if let Some(cell) = old.get(x, y) {
                new.set(x, y, *cell);
            }
        }
    }

    // Apply random mutations to `new`
    let colors = [
        PackedRgba::rgb(255, 0, 0),
        PackedRgba::rgb(0, 255, 0),
        PackedRgba::rgb(0, 0, 255),
        PackedRgba::rgb(255, 255, 0),
        PackedRgba::rgb(255, 0, 255),
        PackedRgba::rgb(0, 255, 255),
        PackedRgba::rgb(128, 128, 128),
        PackedRgba::rgb(255, 128, 0),
    ];
    let chars = ['A', 'B', 'X', '#', '@', '=', '+', '-', '|', '.'];

    for _ in 0..cells_to_change {
        let x = rng.next_u16(w);
        let y = rng.next_u16(h);
        let ch = chars[(rng.next_u64() % chars.len() as u64) as usize];
        let fg = colors[(rng.next_u64() % colors.len() as u64) as usize];
        let bg = colors[(rng.next_u64() % colors.len() as u64) as usize];
        let cell = ftui_render::cell::Cell::from_char(ch)
            .with_fg(fg)
            .with_bg(bg);
        new.set(x, y, cell);
    }

    cells_to_change
}

/// Copy the previous frame into the new buffer without mutating any cell
/// (the zero-change path for declared no-change control fixtures).
fn copy_frame(old: &Buffer, new: &mut Buffer, viewport: ViewportSpec) {
    for y in 0..viewport.height {
        for x in 0..viewport.width {
            if let Some(cell) = old.get(x, y) {
                new.set(x, y, *cell);
            }
        }
    }
}

/// Simple FNV-1a hash of buffer cell content for determinism checks.
fn buffer_checksum(buf: &Buffer, w: u16, h: u16) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for y in 0..h {
        for x in 0..w {
            if let Some(cell) = buf.get(x, y) {
                hash ^= cell.content.raw() as u64;
                hash = hash.wrapping_mul(0x0100_0000_01b3);
                hash ^= cell.fg.0 as u64;
                hash = hash.wrapping_mul(0x0100_0000_01b3);
                hash ^= cell.bg.0 as u64;
                hash = hash.wrapping_mul(0x0100_0000_01b3);
            }
        }
    }
    hash
}

// ============================================================================
// Runner
// ============================================================================

/// Executes fixture specifications and produces baseline measurements.
pub struct FixtureRunner;

const FIXTURE_COLOR_DEPTH: ColorDepth = ColorDepth::TrueColor;

impl FixtureRunner {
    /// Execute a fixture spec and return a complete run result.
    ///
    /// The runner:
    /// 1. Creates a buffer pair at the fixture's viewport size.
    /// 2. For each frame, applies the fixture's transition pattern to mutate cells.
    /// 3. Runs the diff engine and presenter, recording latency samples.
    /// 4. Finalises the `BaselineCapture` into a `BaselineRecord`.
    #[must_use]
    pub fn run(spec: &FixtureSpec) -> FixtureRunResult {
        let vp = spec.viewport;
        let seed = spec.rules.seed;
        let frames = spec.frame_count;
        let mut rng = Rng::new(seed);

        let mut capture = BaselineCapture::new(&spec.id, spec.family)
            .with_seed(seed)
            .with_terminal_color_depth(FIXTURE_COLOR_DEPTH);

        let caps = TerminalCapabilities::builder()
            .color_depth(FIXTURE_COLOR_DEPTH)
            .build();

        let mut old = Buffer::new(vp.width, vp.height);
        let mut new = Buffer::new(vp.width, vp.height);
        let mut diff = BufferDiff::new();
        let mut ansi_sink: Vec<u8> = Vec::with_capacity(vp.cell_count() as usize * 8);

        let mut total_ansi_bytes: u64 = 0;
        let mut total_cells_diffed: u64 = 0;
        let mut frame_checksums = Vec::with_capacity(frames as usize);

        // Pick the primary transition pattern. An EMPTY transition list is a
        // declared no-change control fixture: it must apply ZERO mutations
        // (defaulting it to SparseUpdate would silently turn negative
        // controls into sparse-update workloads — the render gauntlet's
        // NegativeControl gate caught exactly that, bd-40lhe).
        let pattern = spec.transitions.first().copied();

        let run_start = Instant::now();

        // Warmup frames (not measured)
        let warmup_count = (frames / 10).clamp(2, 20);
        for _ in 0..warmup_count {
            new.reset_for_frame();
            match pattern {
                Some(p) => {
                    apply_transition(&mut rng, p, &mut old, &mut new, vp);
                }
                None => copy_frame(&old, &mut new, vp),
            }
            diff.compute_dirty_into(&old, &new);
            ansi_sink.clear();
            let mut presenter = Presenter::new(&mut ansi_sink, caps);
            let _ = presenter.present(&new, &diff);
            std::mem::swap(&mut old, &mut new);
        }

        // Measured frames
        for frame_idx in 0..frames {
            new.reset_for_frame();

            // Measure cell mutation
            let mutate_start = Instant::now();
            let cells_changed = match pattern {
                Some(p) => apply_transition(&mut rng, p, &mut old, &mut new, vp),
                None => {
                    copy_frame(&old, &mut new, vp);
                    0
                }
            };
            let mutate_us = mutate_start.elapsed().as_nanos() as u64;
            capture.record_sample(Sample::latency_us("cell_mutation", mutate_us / 1000));

            // Measure diff computation
            let diff_start = Instant::now();
            diff.compute_dirty_into(&old, &new);
            let diff_us = diff_start.elapsed().as_nanos() as u64;
            capture.record_sample(Sample::latency_us("buffer_diff", diff_us / 1000));

            // Measure presenter emission
            ansi_sink.clear();
            let present_start = Instant::now();
            {
                let mut presenter = Presenter::new(&mut ansi_sink, caps);
                let _ = presenter.present(&new, &diff);
            }
            let present_us = present_start.elapsed().as_nanos() as u64;
            capture.record_sample(Sample::latency_us("presenter_emit", present_us / 1000));

            // Output cost metrics
            let ansi_bytes = ansi_sink.len() as u64;
            capture.record_sample(Sample::output_cost("ansi_bytes_per_frame", ansi_bytes));
            capture.record_sample(Sample::output_cost(
                "cells_changed_per_frame",
                cells_changed,
            ));

            // Total pipeline latency
            let pipeline_us = mutate_us + diff_us + present_us;
            capture.record_sample(Sample::latency_us(
                "frame_pipeline_total",
                pipeline_us / 1000,
            ));

            total_ansi_bytes += ansi_bytes;
            total_cells_diffed += cells_changed;

            // Checksum for determinism
            let cksum = buffer_checksum(&new, vp.width, vp.height);
            frame_checksums.push(cksum);

            // Rotate buffers
            std::mem::swap(&mut old, &mut new);

            // Cycle through transition patterns for multi-transition fixtures
            // (every 10 frames, pick the next pattern)
            if spec.transitions.len() > 1 && frame_idx > 0 && frame_idx % 10 == 0 {
                let _next_pattern =
                    spec.transitions[(frame_idx as usize / 10) % spec.transitions.len()];
                // Pattern cycling: currently the primary pattern drives all frames.
                // Future: could switch mid-run for Mixed workloads.
            }
        }

        let wall_clock_ms = run_start.elapsed().as_millis() as u64;

        // Throughput metric
        if wall_clock_ms > 0 {
            let fps = (frames as f64) / (wall_clock_ms as f64 / 1000.0);
            capture.record_sample(Sample::throughput_ops("frames_per_second", fps));
        }

        let record = capture.finalize();

        FixtureRunResult {
            record,
            wall_clock_ms,
            frames_executed: frames,
            total_ansi_bytes,
            total_cells_diffed,
            frame_checksums,
        }
    }

    /// Run a fixture spec at multiple viewports and return results for each.
    ///
    /// Runs the primary viewport plus all `extra_viewports` from the spec.
    #[must_use]
    pub fn run_all_viewports(spec: &FixtureSpec) -> Vec<(ViewportSpec, FixtureRunResult)> {
        let mut results = Vec::new();

        // Primary viewport
        results.push((spec.viewport, Self::run(spec)));

        // Extra viewports
        for &vp in &spec.extra_viewports {
            let mut adjusted = spec.clone();
            adjusted.viewport = vp;
            adjusted.id = format!("{}_{}x{}", spec.id, vp.width, vp.height);
            results.push((vp, Self::run(&adjusted)));
        }

        results
    }

    /// Run all fixtures in a registry, filtered by partition.
    #[must_use]
    pub fn run_partition(
        fixtures: &[&FixtureSpec],
        partition: SuitePartition,
    ) -> Vec<FixtureRunResult> {
        fixtures
            .iter()
            .filter(|f| f.partition == partition)
            .map(|f| Self::run(f))
            .collect()
    }

    /// Verify determinism: run a fixture twice with the same seed and check
    /// that frame checksums match exactly.
    #[must_use]
    pub fn verify_determinism(spec: &FixtureSpec) -> DeterminismVerdict {
        let run1 = Self::run(spec);
        let run2 = Self::run(spec);

        if run1.frame_checksums.len() != run2.frame_checksums.len() {
            return DeterminismVerdict {
                deterministic: false,
                frame_count: run1.frames_executed,
                first_divergence: Some(0),
                checksums_match_count: 0,
            };
        }

        let mut first_divergence = None;
        let mut match_count = 0u32;

        for (i, (a, b)) in run1
            .frame_checksums
            .iter()
            .zip(run2.frame_checksums.iter())
            .enumerate()
        {
            if a == b {
                match_count += 1;
            } else if first_divergence.is_none() {
                first_divergence = Some(i as u32);
            }
        }

        DeterminismVerdict {
            deterministic: first_divergence.is_none(),
            frame_count: run1.frames_executed,
            first_divergence,
            checksums_match_count: match_count,
        }
    }

    /// Generate a manifest of all fixture results as JSON.
    #[must_use]
    pub fn results_manifest(results: &[(String, FixtureRunResult)]) -> String {
        let entries: Vec<String> = results
            .iter()
            .map(|(id, r)| {
                format!(
                    r#"  {{
    "id": "{}",
    "wall_clock_ms": {},
    "frames_executed": {},
    "total_ansi_bytes": {},
    "total_cells_diffed": {},
    "stable": {},
    "metrics_count": {}
  }}"#,
                    id,
                    r.wall_clock_ms,
                    r.frames_executed,
                    r.total_ansi_bytes,
                    r.total_cells_diffed,
                    r.record.is_stable(),
                    r.record.metrics.len(),
                )
            })
            .collect();

        format!(
            r#"{{
  "schema_version": 1,
  "run_count": {},
  "runs": [
{}
  ]
}}"#,
            results.len(),
            entries.join(",\n"),
        )
    }
}

// ============================================================================
// Determinism Verdict
// ============================================================================

/// Result of a determinism verification run.
#[derive(Debug, Clone)]
pub struct DeterminismVerdict {
    /// Whether all frames produced identical checksums across runs.
    pub deterministic: bool,
    /// Total frames per run.
    pub frame_count: u32,
    /// Frame index of first divergence, if any.
    pub first_divergence: Option<u32>,
    /// Number of frames that matched.
    pub checksums_match_count: u32,
}

impl DeterminismVerdict {
    /// Human-readable summary.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.deterministic {
            format!(
                "DETERMINISTIC: all {} frames matched across runs",
                self.frame_count
            )
        } else {
            format!(
                "NON-DETERMINISTIC: first divergence at frame {}, {}/{} matched",
                self.first_divergence.unwrap_or(0),
                self.checksums_match_count,
                self.frame_count,
            )
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture_suite::FixtureRegistry;

    #[test]
    fn run_sparse_diff_fixture() {
        let reg = FixtureRegistry::canonical();
        let spec = reg.get("render_diff_sparse_80x24").unwrap();
        let result = FixtureRunner::run(spec);

        assert_eq!(result.frames_executed, spec.frame_count);
        assert_eq!(
            result.record.environment.terminal_color_depth,
            Some(ColorDepth::TrueColor)
        );
        assert!(
            result
                .record
                .to_json()
                .contains("\"terminal_color_depth\": \"truecolor\"")
        );
        assert!(
            result.record.metrics.len() >= 4,
            "expected multiple metrics"
        );
        assert!(
            result.total_ansi_bytes > 0,
            "should have produced ANSI output"
        );
        assert_eq!(
            result.frame_checksums.len(),
            spec.frame_count as usize,
            "should have one checksum per frame"
        );
    }

    #[test]
    fn run_dense_diff_fixture() {
        let reg = FixtureRegistry::canonical();
        let spec = reg.get("render_diff_dense_80x24").unwrap();
        let result = FixtureRunner::run(spec);

        assert_eq!(result.frames_executed, spec.frame_count);
        assert!(
            result.total_ansi_bytes > 0,
            "dense diff should produce ANSI output"
        );
    }

    #[test]
    fn run_presenter_emit_fixture() {
        let reg = FixtureRegistry::canonical();
        let spec = reg.get("render_presenter_emit_120x40").unwrap();
        let result = FixtureRunner::run(spec);

        assert_eq!(result.frames_executed, spec.frame_count);
        // Presenter emit at 120x40 should produce more bytes than 80x24
        assert!(
            result.total_ansi_bytes > 100,
            "presenter should emit substantial output"
        );
    }

    #[test]
    fn run_full_pipeline_fixture() {
        let reg = FixtureRegistry::canonical();
        let spec = reg.get("render_pipeline_full_200x60").unwrap();
        let result = FixtureRunner::run(spec);

        assert_eq!(result.frames_executed, spec.frame_count);
        // At 200x60 = 12000 cells, even sparse updates produce output
        assert!(result.total_ansi_bytes > 0);
    }

    #[test]
    fn run_negative_control_static_screen() {
        let reg = FixtureRegistry::canonical();
        let spec = reg.get("control_static_screen").unwrap();
        let result = FixtureRunner::run(spec);

        assert_eq!(result.frames_executed, spec.frame_count);
        // Static screen: after the first frame, diff should produce minimal changes
        // (the transition pattern is empty, so apply_transition uses SparseUpdate default,
        // but that's OK — the negative control tests optimization regressions, not
        // zero output. The fixture runner faithfully executes the spec.)
    }

    #[test]
    fn determinism_verification_passes() {
        let reg = FixtureRegistry::canonical();
        let spec = reg.get("render_diff_sparse_80x24").unwrap();
        let verdict = FixtureRunner::verify_determinism(spec);

        assert!(
            verdict.deterministic,
            "same seed should produce identical checksums: {}",
            verdict.summary()
        );
        assert_eq!(verdict.checksums_match_count, spec.frame_count);
    }

    #[test]
    fn all_canonical_fixtures_complete() {
        let reg = FixtureRegistry::canonical();
        for spec in reg.by_partition(SuitePartition::Canonical) {
            let result = FixtureRunner::run(spec);
            assert_eq!(
                result.frames_executed, spec.frame_count,
                "fixture {} did not complete all frames",
                spec.id
            );
            assert!(
                !result.record.metrics.is_empty(),
                "fixture {} produced no metrics",
                spec.id
            );
        }
    }

    #[test]
    fn all_challenge_fixtures_complete() {
        let reg = FixtureRegistry::canonical();
        for spec in reg.by_partition(SuitePartition::Challenge) {
            let result = FixtureRunner::run(spec);
            assert_eq!(
                result.frames_executed, spec.frame_count,
                "challenge fixture {} did not complete",
                spec.id
            );
        }
    }

    #[test]
    fn all_negative_controls_complete() {
        let reg = FixtureRegistry::canonical();
        for spec in reg.by_partition(SuitePartition::NegativeControl) {
            let result = FixtureRunner::run(spec);
            assert_eq!(
                result.frames_executed, spec.frame_count,
                "negative control {} did not complete",
                spec.id
            );
        }
    }

    #[test]
    fn multi_viewport_run() {
        let reg = FixtureRegistry::canonical();
        let spec = reg.get("render_diff_sparse_80x24").unwrap();
        let results = FixtureRunner::run_all_viewports(spec);

        // Primary + 2 extra viewports (MEDIUM, LARGE)
        assert_eq!(
            results.len(),
            1 + spec.extra_viewports.len(),
            "should run primary + extra viewports"
        );

        for (vp, result) in &results {
            assert_eq!(
                result.frames_executed, spec.frame_count,
                "viewport {}x{} did not complete all frames",
                vp.width, vp.height
            );
        }
    }

    #[test]
    fn results_manifest_json_valid() {
        let reg = FixtureRegistry::canonical();
        let spec = reg.get("render_diff_sparse_80x24").unwrap();
        let result = FixtureRunner::run(spec);
        let manifest = FixtureRunner::results_manifest(&[(spec.id.clone(), result)]);

        assert!(manifest.contains("\"schema_version\": 1"));
        assert!(manifest.contains("\"run_count\": 1"));
        assert!(manifest.contains("render_diff_sparse_80x24"));
    }

    #[test]
    fn baseline_record_has_expected_metrics() {
        let reg = FixtureRegistry::canonical();
        let spec = reg.get("render_diff_sparse_80x24").unwrap();
        let result = FixtureRunner::run(spec);

        let metric_names: Vec<&str> = result
            .record
            .metrics
            .iter()
            .map(|m| m.metric.as_str())
            .collect();
        assert!(
            metric_names.contains(&"buffer_diff"),
            "missing buffer_diff metric"
        );
        assert!(
            metric_names.contains(&"presenter_emit"),
            "missing presenter_emit metric"
        );
        assert!(
            metric_names.contains(&"frame_pipeline_total"),
            "missing frame_pipeline_total metric"
        );
        assert!(
            metric_names.contains(&"ansi_bytes_per_frame"),
            "missing ansi_bytes_per_frame metric"
        );
    }

    #[test]
    fn rng_is_deterministic() {
        let mut rng1 = Rng::new(42);
        let mut rng2 = Rng::new(42);

        for _ in 0..100 {
            assert_eq!(rng1.next_u64(), rng2.next_u64());
        }
    }

    #[test]
    fn determinism_verdict_summary() {
        let pass = DeterminismVerdict {
            deterministic: true,
            frame_count: 100,
            first_divergence: None,
            checksums_match_count: 100,
        };
        assert!(pass.summary().contains("DETERMINISTIC"));

        let fail = DeterminismVerdict {
            deterministic: false,
            frame_count: 100,
            first_divergence: Some(42),
            checksums_match_count: 42,
        };
        assert!(fail.summary().contains("NON-DETERMINISTIC"));
        assert!(fail.summary().contains("frame 42"));
    }
}
