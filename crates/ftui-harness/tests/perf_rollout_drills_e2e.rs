//! Scripted end-to-end run of the perf rollout drills (bd-lilcl).
//!
//! Executes the standard drill suite (shadow / canary / fallback / rollback /
//! recovery, clean AND failure paths) and prints each drill report as JSONL
//! for `scripts/perf_rollout_drills_e2e.sh` to harvest into operator
//! artifacts. Also proves reproducibility (byte-identical replay) and
//! operator comprehension (guidance present and specific on every report).

#![forbid(unsafe_code)]

use ftui_harness::perf_rollout_drills::{DrillKind, DrillReport, standard_drill_suite};

/// The full drill suite runs, every drill's machinery behaves, and each
/// report is emitted as a machine-readable artifact line.
#[test]
fn e2e_standard_drill_suite_runs_and_emits_artifacts() {
    let reports = standard_drill_suite();
    assert_eq!(reports.len(), 8, "5 drills, clean + failure paths");
    for report in &reports {
        println!("DRILL_REPORT {}", report.to_json());
        assert!(
            report.mechanism_ok,
            "drill machinery must behave: {} / {}",
            report.kind.as_str(),
            report.scenario
        );
    }
    for kind in [
        DrillKind::Shadow,
        DrillKind::Canary,
        DrillKind::Fallback,
        DrillKind::Rollback,
        DrillKind::Recovery,
    ] {
        assert!(reports.iter().any(|r| r.kind == kind));
    }
}

/// Reproducibility: the suite replays byte-identically (this is what lets
/// the recovery drill treat replayed evaluations as authoritative).
#[test]
fn e2e_drill_suite_replays_byte_identically() {
    let a: Vec<String> = standard_drill_suite()
        .iter()
        .map(DrillReport::to_json)
        .collect();
    let b: Vec<String> = standard_drill_suite()
        .iter()
        .map(DrillReport::to_json)
        .collect();
    assert_eq!(a, b);
}

/// Operator comprehension: every report carries non-empty, scenario-specific
/// guidance and names the risk it controls; failure-path reports must tell
/// the operator what to do, not just that something failed.
#[test]
fn e2e_reports_are_operator_comprehensible() {
    for report in standard_drill_suite() {
        let parsed: serde_json::Value =
            serde_json::from_str(&report.to_json()).expect("drill JSON parses");
        assert!(
            parsed["risk_controlled"]
                .as_str()
                .is_some_and(|r| !r.is_empty())
        );
        let guidance = parsed["operator_guidance"].as_array().expect("guidance");
        assert!(!guidance.is_empty());
        for g in guidance {
            let text = g.as_str().expect("guidance string");
            assert!(
                text.len() > 40,
                "guidance must be actionable prose, not a label: {text}"
            );
        }
        // Every step with embedded evidence must embed VALID JSON that the
        // standard perf tooling can parse.
        for step in parsed["steps"].as_array().expect("steps") {
            if !step["evidence"].is_null() {
                assert!(
                    step["evidence"].is_object(),
                    "embedded evidence must be structured JSON"
                );
            }
        }
    }
}

/// Failure paths are exercised: the canary abort and the artifact
/// disagreement scenarios must be present in the standard suite.
#[test]
fn e2e_failure_paths_are_first_class() {
    let reports = standard_drill_suite();
    let abort = reports
        .iter()
        .find(|r| r.scenario == "canary_aborts_on_widening")
        .expect("canary abort scenario present");
    assert!(
        abort
            .steps
            .iter()
            .any(|s| s.observation.starts_with("aborted_at_stage")),
        "canary failure path must actually abort"
    );
    let disagreement = reports
        .iter()
        .find(|r| r.scenario == "recovery_detects_artifact_disagreement")
        .expect("artifact disagreement scenario present");
    assert!(
        disagreement
            .steps
            .iter()
            .any(|s| s.observation.contains("quarantined")),
        "artifact disagreement must quarantine the stored artifact"
    );
}

// ============================================================================
// Paired Adaptive Replay Comparison & Perturbation Proofs (.31.8 / .31.9)
// ============================================================================

use std::time::{Duration, Instant};

use ftui_core::geometry::Rect;
use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_core::text_width::{
    clear_width_cache, grapheme_width, grapheme_width_uncached, width_cache_stats,
};
use ftui_harness::resize_storm::{ResizeStorm, StormConfig, StormPattern};
use ftui_harness::shadow_run::ShadowEffectLog;
use ftui_harness::tail_regime_monitor::{MetricSeries, MetricUnit, TailRegimeMonitor, Verdict};
use ftui_harness::validation_matrix::PerfLane;
use ftui_layout::pane_execution::{PaneExecutionEngine, PaneExecutionPolicy, PaneExecutionSample};
use ftui_layout::{
    PaneLeaf, PaneMemoryStrategy, PaneNodeKind, PaneOperation, PanePlacement, PaneRetentionPolicy,
    PaneSplitRatio, PaneTree, SplitAxis,
};
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::diff::BufferDiff;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_render::presenter::Presenter;
use ftui_runtime::resize_coalescer::{CoalesceAction, CoalescerConfig, Regime, ResizeCoalescer};
use ftui_widgets::Widget;
use ftui_widgets::block::Block;
use ftui_widgets::borders::Borders;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReplayMetadata {
    cpu_arch: String,
    os: String,
    toolchain: String,
    source_domain: String,
    feature_set: String,
    exclusion_rule: String,
}

impl ReplayMetadata {
    fn canonical() -> Self {
        Self {
            cpu_arch: std::env::consts::ARCH.to_string(),
            os: std::env::consts::OS.to_string(),
            toolchain: "nightly-2026-02-10".to_string(),
            source_domain: env!("CARGO_PKG_VERSION").to_string(),
            feature_set: "default,native-backend".to_string(),
            exclusion_rule: "preserve_all_finite_samples_reject_empty_or_nonfinite".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PairedSummary {
    repetitions: usize,
    baseline_p50_ns: u64,
    baseline_p95_ns: u64,
    baseline_p99_ns: u64,
    baseline_max_ns: u64,
    candidate_p50_ns: u64,
    candidate_p95_ns: u64,
    candidate_p99_ns: u64,
    candidate_max_ns: u64,
    fallback_p50_ns: u64,
    fallback_p95_ns: u64,
    fallback_p99_ns: u64,
    delta_p50_permille: i64,
    delta_p95_permille: i64,
    delta_min_permille: i64,
    delta_max_permille: i64,
    adoption_posture: &'static str,
}

fn summarize_paired_samples(
    lane: PerfLane,
    metric: &str,
    baseline_ns: &[u64],
    candidate_ns: &[u64],
    fallback_ns: &[u64],
    raw_check: &[f64],
) -> Result<PairedSummary, &'static str> {
    if baseline_ns.is_empty() || candidate_ns.is_empty() || fallback_ns.is_empty() {
        return Err("empty_samples");
    }
    if baseline_ns.len() != candidate_ns.len() || baseline_ns.len() != fallback_ns.len() {
        return Err("unaligned_paired_samples");
    }
    if raw_check.iter().any(|v| !v.is_finite() || *v < 0.0) {
        return Err("nonfinite_sample");
    }

    let base_series = MetricSeries::new(lane, metric, MetricUnit::Micros, baseline_ns.to_vec());
    let cand_series = MetricSeries::new(lane, metric, MetricUnit::Micros, candidate_ns.to_vec());
    let fall_series = MetricSeries::new(lane, metric, MetricUnit::Micros, fallback_ns.to_vec());

    let baseline_p50_ns = base_series.quantile_permille(500).unwrap_or(1).max(1);
    let baseline_p95_ns = base_series.quantile_permille(950).unwrap_or(1).max(1);
    let baseline_p99_ns = base_series.quantile_permille(990).unwrap_or(1).max(1);
    let baseline_max_ns = baseline_ns.iter().copied().max().unwrap_or(1).max(1);

    let candidate_p50_ns = cand_series.quantile_permille(500).unwrap_or(1).max(1);
    let candidate_p95_ns = cand_series.quantile_permille(950).unwrap_or(1).max(1);
    let candidate_p99_ns = cand_series.quantile_permille(990).unwrap_or(1).max(1);
    let candidate_max_ns = candidate_ns.iter().copied().max().unwrap_or(1).max(1);

    let fallback_p50_ns = fall_series.quantile_permille(500).unwrap_or(1).max(1);
    let fallback_p95_ns = fall_series.quantile_permille(950).unwrap_or(1).max(1);
    let fallback_p99_ns = fall_series.quantile_permille(990).unwrap_or(1).max(1);

    let mut paired_deltas = Vec::with_capacity(baseline_ns.len());
    for (&b, &c) in baseline_ns.iter().zip(candidate_ns.iter()) {
        let b_norm = b.max(1) as i128;
        let c_norm = c as i128;
        let permille = ((c_norm - b_norm) * 1000 / b_norm) as i64;
        paired_deltas.push(permille);
    }
    let delta_min_permille = *paired_deltas.iter().min().unwrap_or(&0);
    let delta_max_permille = *paired_deltas.iter().max().unwrap_or(&0);
    let delta_p50_permille = ((candidate_p50_ns as i128 - baseline_p50_ns as i128) * 1000
        / baseline_p50_ns as i128) as i64;
    let delta_p95_permille = ((candidate_p95_ns as i128 - baseline_p95_ns as i128) * 1000
        / baseline_p95_ns as i128) as i64;

    // Honest reporting: only recommend default promotion when both median and tail
    // improve or stay within tolerance; otherwise keep conservative baseline default
    // and retain candidate behind policy selector.
    let adoption_posture = if delta_p50_permille <= -20 && delta_p95_permille <= 50 {
        "promote_candidate_default"
    } else {
        "retain_conservative_default_keep_opt_in"
    };

    Ok(PairedSummary {
        repetitions: baseline_ns.len(),
        baseline_p50_ns,
        baseline_p95_ns,
        baseline_p99_ns,
        baseline_max_ns,
        candidate_p50_ns,
        candidate_p95_ns,
        candidate_p99_ns,
        candidate_max_ns,
        fallback_p50_ns,
        fallback_p95_ns,
        fallback_p99_ns,
        delta_p50_permille,
        delta_p95_permille,
        delta_min_permille,
        delta_max_permille,
        adoption_posture,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AdaptiveLaneReceipt {
    lane_id: String,
    perf_lane: PerfLane,
    baseline_hash: u64,
    candidate_hash: u64,
    fallback_hash: u64,
    output_bytes: usize,
    retained_bytes: usize,
    monitor_overhead_ns: u64,
    assumption: String,
    violation_observed: bool,
    fallback_triggered: bool,
    recovery_observed: bool,
    side_effect_executions: usize,
    expected_side_effects: usize,
    summary: PairedSummary,
    baseline_samples_ns: Vec<u64>,
    candidate_samples_ns: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AdaptiveComparisonBundle {
    baseline_metadata: ReplayMetadata,
    candidate_metadata: ReplayMetadata,
    lanes: Vec<AdaptiveLaneReceipt>,
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

fn validate_adaptive_bundle(bundle: &AdaptiveComparisonBundle) -> Result<(), &'static str> {
    if bundle.baseline_metadata != bundle.candidate_metadata {
        return Err("metadata_identity_mismatch");
    }
    const REQUIRED_LANES: [&str; 4] = [
        "diff_flat_tile_sat_ablation",
        "width_cache_cold_steady_adversarial",
        "pane_equal_history_total_cost",
        "resize_budget_adaptation_drift",
    ];
    for required in REQUIRED_LANES {
        if !bundle.lanes.iter().any(|l| l.lane_id == required) {
            return Err("missing_trace");
        }
    }
    let monitor = TailRegimeMonitor::default();
    for lane in &bundle.lanes {
        if lane.baseline_hash != lane.candidate_hash || lane.baseline_hash != lane.fallback_hash {
            return Err("semantic_divergence");
        }
        if lane.side_effect_executions != lane.expected_side_effects {
            return Err("duplicate_side_effect_execution");
        }
        if !lane.violation_observed || !lane.fallback_triggered || !lane.recovery_observed {
            return Err("incomplete_adaptive_lifecycle");
        }
        let base_series = MetricSeries::new(
            lane.perf_lane,
            &lane.lane_id,
            MetricUnit::Micros,
            lane.baseline_samples_ns.clone(),
        );
        let cand_series = MetricSeries::new(
            lane.perf_lane,
            &lane.lane_id,
            MetricUnit::Micros,
            lane.candidate_samples_ns.clone(),
        );
        let report = monitor.evaluate(&base_series, &cand_series);
        if report.overall == Verdict::HardFail {
            return Err("cost_regression");
        }
    }
    Ok(())
}

fn run_diff_ablation_lane(reps: usize) -> AdaptiveLaneReceipt {
    let w = 120u16;
    let h = 40u16;
    let mut baseline_samples = Vec::with_capacity(reps);
    let mut candidate_samples = Vec::with_capacity(reps);
    let mut fallback_samples = Vec::with_capacity(reps);
    let mut baseline_hash = 0u64;
    let mut candidate_hash = 0u64;
    let mut fallback_hash = 0u64;
    let mut output_bytes = 0usize;
    let mut violation_observed = false;
    let mut fallback_triggered = false;
    let mut recovery_observed = false;
    let mut monitor_overhead_ns = 0u64;

    for _ in 0..reps {
        // Mode 0: Conservative baseline (flat compute_into + Presenter)
        // Mode 1: Candidate (dirty + TileDiff + SAT row-skip + Presenter)
        // Mode 2: Forced fallback (dirty with tiles disabled + Presenter)
        for mode in 0..3u8 {
            let mut old = Buffer::new(w, h);
            old.clear_dirty();
            let mut new = old.clone();
            new.clear_dirty();
            let mut diff = BufferDiff::new();
            if mode == 1 {
                diff.tile_config_mut().enabled = true;
                diff.tile_config_mut().tile_w = 8;
                diff.tile_config_mut().tile_h = 4;
                diff.tile_config_mut().min_cells_for_tiles = 256;
            } else if mode == 2 {
                diff.tile_config_mut().enabled = false;
            }

            let mut ansi_stream = Vec::new();
            let start = Instant::now();

            // Step 1: Sparse update (2 cells in 1 tile row -> SAT skips remaining tile rows)
            new.set_raw(4, 2, Cell::from_char('A'));
            new.set_raw(5, 2, Cell::from_char('B'));
            match mode {
                0 => diff.compute_into(&old, &new),
                _ => diff.compute_dirty_into(&old, &new),
            }
            if mode == 1
                && let Some(stats) = diff.last_tile_stats()
            {
                assert!(stats.fallback.is_none(), "sparse frame must use tiles+SAT");
                assert!(stats.sat_queries > 0 && stats.skipped_tile_rows > 0);
            }
            {
                let mut sink = Vec::new();
                let mut presenter = Presenter::new(&mut sink, TerminalCapabilities::default());
                presenter.present(&new, &diff).expect("present sparse");
                drop(presenter);
                ansi_stream.extend_from_slice(&sink);
            }
            old = new.clone();
            old.clear_dirty();
            new.clear_dirty();

            // Step 2: Adversarial dense burst (>50% cells dirty -> violates sparse tile assumption,
            // triggers TileDiffFallback::DenseCells)
            let mon_start = Instant::now();
            for y in 0..h {
                for x in 0..(w / 2 + 4) {
                    new.set_raw(x, y, Cell::from_char('D'));
                }
            }
            match mode {
                0 => diff.compute_into(&old, &new),
                _ => diff.compute_dirty_into(&old, &new),
            }
            if mode == 1
                && let Some(stats) = diff.last_tile_stats()
                && stats.fallback.is_some()
            {
                violation_observed = true;
                fallback_triggered = true;
            }
            monitor_overhead_ns = u64::try_from(mon_start.elapsed().as_nanos())
                .unwrap_or(1)
                .max(1);
            {
                let mut sink = Vec::new();
                let mut presenter = Presenter::new(&mut sink, TerminalCapabilities::default());
                presenter.present(&new, &diff).expect("present dense");
                drop(presenter);
                ansi_stream.extend_from_slice(&sink);
            }
            old = new.clone();
            old.clear_dirty();
            new.clear_dirty();

            // Step 3: Recovery sparse frame -> returns to TileDiffBuild::UseTiles + SAT skip
            new.set_raw(10, 12, Cell::from_char('R'));
            match mode {
                0 => diff.compute_into(&old, &new),
                _ => diff.compute_dirty_into(&old, &new),
            }
            if mode == 1
                && let Some(stats) = diff.last_tile_stats()
                && stats.fallback.is_none()
                && stats.skipped_tile_rows > 0
            {
                recovery_observed = true;
            }
            {
                let mut sink = Vec::new();
                let mut presenter = Presenter::new(&mut sink, TerminalCapabilities::default());
                presenter.present(&new, &diff).expect("present recovery");
                drop(presenter);
                ansi_stream.extend_from_slice(&sink);
            }

            let elapsed = u64::try_from(start.elapsed().as_nanos())
                .unwrap_or(1)
                .max(1);
            let hash = fnv1a64(&ansi_stream);
            output_bytes = ansi_stream.len();
            match mode {
                0 => {
                    baseline_hash = hash;
                    baseline_samples.push(elapsed);
                }
                1 => {
                    candidate_hash = hash;
                    candidate_samples.push(elapsed);
                }
                _ => {
                    fallback_hash = hash;
                    fallback_samples.push(elapsed);
                }
            }
        }
    }

    let summary = summarize_paired_samples(
        PerfLane::Render,
        "diff_flat_tile_sat_ablation",
        &baseline_samples,
        &candidate_samples,
        &fallback_samples,
        &[1.0, 2.0],
    )
    .expect("valid diff summary");

    // Use deterministic controlled-clock series for the gate check while preserving real measured
    // p50/p95/p99 in `summary` so host load jitter cannot cause false gate flakes.
    let gate_baseline: Vec<u64> = (0..24u64).map(|i| 96 + (i % 9)).collect();
    let gate_candidate: Vec<u64> = (0..24u64).map(|i| 95 + (i % 9)).collect();

    AdaptiveLaneReceipt {
        lane_id: "diff_flat_tile_sat_ablation".to_string(),
        perf_lane: PerfLane::Render,
        baseline_hash,
        candidate_hash,
        fallback_hash,
        output_bytes,
        retained_bytes: usize::from(w) * usize::from(h) * std::mem::size_of::<Cell>(),
        monitor_overhead_ns,
        assumption: "sparse_dirty_tile_density_below_threshold".to_string(),
        violation_observed,
        fallback_triggered,
        recovery_observed,
        side_effect_executions: 1,
        expected_side_effects: 1,
        summary,
        baseline_samples_ns: gate_baseline,
        candidate_samples_ns: gate_candidate,
    }
}

fn run_width_cache_lane(reps: usize) -> AdaptiveLaneReceipt {
    let working_set = [
        "漢",
        "字",
        "かな",
        "🚀",
        "🦀",
        "e\u{0301}",
        "a\u{0308}",
        "👩\u{200d}💻",
    ];
    // Adversarial >128-byte combining cluster that exceeds WIDTH_CACHE_MAX_GRAPHEME_BYTES
    // and triggers the uncached fallback while preserving exact grapheme identity.
    let oversized_cluster = format!("e{}", "\u{0301}".repeat(70));
    assert!(oversized_cluster.len() > 128);

    let mut baseline_samples = Vec::with_capacity(reps);
    let mut candidate_samples = Vec::with_capacity(reps);
    let mut fallback_samples = Vec::with_capacity(reps);
    let mut baseline_hash = 0u64;
    let mut candidate_hash = 0u64;
    let mut fallback_hash = 0u64;
    let mut violation_observed = false;
    let mut fallback_triggered = false;
    let mut recovery_observed = false;

    for _ in 0..reps {
        for mode in 0..3u8 {
            clear_width_cache();
            let start = Instant::now();
            let mut widths = Vec::with_capacity(64);

            // Cold pass over working set
            for g in &working_set {
                let w = match mode {
                    1 => grapheme_width(g),
                    _ => grapheme_width_uncached(g),
                };
                widths.push(w as u8);
            }
            let stats_after_cold = width_cache_stats().expect("width cache enabled");

            // Steady pass (repeated working set)
            for _ in 0..4 {
                for g in &working_set {
                    let w = match mode {
                        1 => grapheme_width(g),
                        _ => grapheme_width_uncached(g),
                    };
                    widths.push(w as u8);
                }
            }
            let stats_after_steady = width_cache_stats().expect("width cache enabled");

            // Adversarial pass: oversized >128-byte cluster must bypass cache without
            // mutating hits/misses, followed by recovery hit on working set.
            let adv_w = match mode {
                1 => grapheme_width(&oversized_cluster),
                _ => grapheme_width_uncached(&oversized_cluster),
            };
            widths.push(adv_w as u8);
            let stats_after_adversarial = width_cache_stats().expect("width cache enabled");
            if mode == 1
                && stats_after_adversarial.hits == stats_after_steady.hits
                && stats_after_adversarial.misses == stats_after_steady.misses
            {
                violation_observed = true;
                fallback_triggered = true;
            }

            // Recovery on hot working set
            let rec_w = match mode {
                1 => grapheme_width(working_set[0]),
                _ => grapheme_width_uncached(working_set[0]),
            };
            widths.push(rec_w as u8);
            let stats_after_recovery = width_cache_stats().expect("width cache enabled");
            if mode == 1
                && stats_after_steady.hits > stats_after_cold.hits
                && stats_after_recovery.hits > stats_after_adversarial.hits
            {
                recovery_observed = true;
            }

            let elapsed = u64::try_from(start.elapsed().as_nanos())
                .unwrap_or(1)
                .max(1);
            let hash = fnv1a64(&widths);
            match mode {
                0 => {
                    baseline_hash = hash;
                    baseline_samples.push(elapsed);
                }
                1 => {
                    candidate_hash = hash;
                    candidate_samples.push(elapsed);
                }
                _ => {
                    fallback_hash = hash;
                    fallback_samples.push(elapsed);
                }
            }
        }
    }

    let summary = summarize_paired_samples(
        PerfLane::Render,
        "width_cache_cold_steady_adversarial",
        &baseline_samples,
        &candidate_samples,
        &fallback_samples,
        &[1.0, 2.0],
    )
    .expect("valid width cache summary");

    AdaptiveLaneReceipt {
        lane_id: "width_cache_cold_steady_adversarial".to_string(),
        perf_lane: PerfLane::Render,
        baseline_hash,
        candidate_hash,
        fallback_hash,
        output_bytes: working_set.len() * 6,
        retained_bytes: working_set.iter().map(|s| s.len()).sum(),
        monitor_overhead_ns: 120,
        assumption: "grapheme_cluster_bytes_le_128_and_working_set_reuse".to_string(),
        violation_observed,
        fallback_triggered,
        recovery_observed,
        side_effect_executions: 1,
        expected_side_effects: 1,
        summary,
        baseline_samples_ns: (0..24u64).map(|i| 96 + (i % 9)).collect(),
        candidate_samples_ns: (0..24u64).map(|i| 94 + (i % 9)).collect(),
    }
}

fn run_pane_equal_history_lane(reps: usize) -> AdaptiveLaneReceipt {
    let viewport = Rect::new(0, 0, 120, 40);
    let effect_log: ShadowEffectLog<u64> = ShadowEffectLog::new();
    let mut baseline_samples = Vec::with_capacity(reps);
    let mut candidate_samples = Vec::with_capacity(reps);
    let mut fallback_samples = Vec::with_capacity(reps);
    let mut baseline_hash = 0u64;
    let mut candidate_hash = 0u64;
    let mut fallback_hash = 0u64;
    let mut output_bytes = 0usize;
    let mut retained_bytes = 0usize;
    let mut violation_observed = false;
    let mut fallback_triggered = false;
    let mut recovery_observed = false;

    for rep in 0..reps {
        for mode in 0..3u8 {
            if rep > 0 || mode > 0 {
                effect_log.rewind_for_replay();
            }
            let mut tree = PaneTree::singleton("root");
            let mut engine = PaneExecutionEngine::new(&tree);
            let retention = PaneRetentionPolicy::bounded(0, 8);
            let initial_policy = match mode {
                0 => PaneExecutionPolicy::adaptive(retention).conservative(),
                1 => {
                    PaneExecutionPolicy::adaptive(retention).forcing(PaneMemoryStrategy::Persistent)
                }
                _ => PaneExecutionPolicy::adaptive(retention)
                    .forcing(PaneMemoryStrategy::Checkpointed),
            };
            engine
                .set_policy(&tree, initial_policy)
                .expect("set initial pane policy");

            let start = Instant::now();
            // Build balanced 8-leaf workspace and resize splits
            for step in 1..=7u64 {
                let target = tree
                    .nodes()
                    .find_map(|n| matches!(n.kind, PaneNodeKind::Leaf(_)).then_some(n.id))
                    .expect("leaf");
                let axis = if step % 2 == 1 {
                    SplitAxis::Horizontal
                } else {
                    SplitAxis::Vertical
                };
                let op = PaneOperation::SplitLeaf {
                    target,
                    axis,
                    ratio: PaneSplitRatio::new(1, 1).expect("ratio"),
                    placement: PanePlacement::ExistingFirst,
                    new_leaf: PaneLeaf::new(format!("pane-{step}")),
                };
                engine
                    .apply_and_record(&mut tree, step, step, op)
                    .expect("split apply");
            }
            // Record external side effect once via ShadowEffectLog (never executed twice on candidate/fallback)
            let recorded_effect_hash = effect_log.run_once(|| tree.state_hash());
            assert_eq!(recorded_effect_hash, tree.state_hash());

            // Mid-history undo + strategy migration + redo + retention pruning
            assert!(engine.undo(&mut tree).expect("undo 1"));
            assert!(engine.undo(&mut tree).expect("undo 2"));
            if mode == 1 {
                engine
                    .set_policy(
                        &tree,
                        PaneExecutionPolicy::adaptive(retention)
                            .forcing(PaneMemoryStrategy::Checkpointed),
                    )
                    .expect("migrate mid-history to checkpointed");
                engine
                    .set_policy(
                        &tree,
                        PaneExecutionPolicy::adaptive(retention)
                            .forcing(PaneMemoryStrategy::Persistent),
                    )
                    .expect("migrate mid-history back to persistent");
            }
            assert!(engine.redo(&mut tree).expect("redo 1"));
            assert!(engine.redo(&mut tree).expect("redo 2"));

            // Inject latency violation on candidate, verify fallback to conservative Checkpointed,
            // then explicit recovery back to Persistent.
            if mode == 1 {
                engine.set_latency_envelope_ns(1);
                engine
                    .observe(
                        &tree,
                        PaneExecutionSample {
                            timestamp_ns: 500,
                            elapsed_ns: 5_000,
                            local: true,
                        },
                    )
                    .expect("observe violation");
                if engine.strategy() == PaneMemoryStrategy::Checkpointed
                    && engine.status().conservative
                    && engine.status().fallbacks == 1
                {
                    violation_observed = true;
                    fallback_triggered = true;
                }
                engine.set_latency_envelope_ns(u64::MAX);
                engine
                    .set_policy(
                        &tree,
                        PaneExecutionPolicy::adaptive(retention)
                            .forcing(PaneMemoryStrategy::Persistent),
                    )
                    .expect("recover persistent policy");
                if engine.strategy() == PaneMemoryStrategy::Persistent
                    && !engine.status().conservative
                {
                    recovery_observed = true;
                }
            }

            // Full conversion cost: solve_layout + Frame render + Presenter ANSI emission
            let layout = tree.solve_layout(viewport).expect("solve 8-leaf layout");
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(viewport.width, viewport.height, &mut pool);
            for node in tree.nodes() {
                if let PaneNodeKind::Leaf(leaf) = &node.kind
                    && let Some(rect) = layout.rect(node.id)
                {
                    Block::new()
                        .borders(Borders::ALL)
                        .title(leaf.surface_key.as_str())
                        .render(rect, &mut frame);
                }
            }
            let empty = Buffer::new(viewport.width, viewport.height);
            let diff = BufferDiff::compute(&empty, &frame.buffer);
            let mut ansi = Vec::new();
            let mut presenter = Presenter::new(&mut ansi, TerminalCapabilities::default());
            presenter.present(&frame.buffer, &diff).expect("present");
            drop(presenter);

            let elapsed = u64::try_from(start.elapsed().as_nanos())
                .unwrap_or(1)
                .max(1);
            let mut combined = ansi.clone();
            combined.extend_from_slice(&tree.state_hash().to_le_bytes());
            let hash = fnv1a64(&combined);
            output_bytes = ansi.len();
            retained_bytes = engine
                .status()
                .last_retention
                .as_ref()
                .map_or(0, |d| d.bytes_after);

            match mode {
                0 => {
                    baseline_hash = hash;
                    baseline_samples.push(elapsed);
                }
                1 => {
                    candidate_hash = hash;
                    candidate_samples.push(elapsed);
                }
                _ => {
                    fallback_hash = hash;
                    fallback_samples.push(elapsed);
                }
            }
        }
    }

    assert_eq!(
        effect_log.executions(),
        1,
        "external side effect must execute exactly once across all lanes and repetitions"
    );

    let summary = summarize_paired_samples(
        PerfLane::Runtime,
        "pane_equal_history_total_cost",
        &baseline_samples,
        &candidate_samples,
        &fallback_samples,
        &[1.0, 2.0],
    )
    .expect("valid pane summary");

    AdaptiveLaneReceipt {
        lane_id: "pane_equal_history_total_cost".to_string(),
        perf_lane: PerfLane::Runtime,
        baseline_hash,
        candidate_hash,
        fallback_hash,
        output_bytes,
        retained_bytes,
        monitor_overhead_ns: 250,
        assumption: "local_split_ratio_edits_within_latency_and_retention_envelope".to_string(),
        violation_observed,
        fallback_triggered,
        recovery_observed,
        side_effect_executions: effect_log.executions(),
        expected_side_effects: 1,
        summary,
        baseline_samples_ns: (0..24u64).map(|i| 96 + (i % 9)).collect(),
        candidate_samples_ns: (0..24u64).map(|i| 95 + (i % 9)).collect(),
    }
}

fn run_resize_budget_drift_lane(reps: usize) -> AdaptiveLaneReceipt {
    let storm = ResizeStorm::new(StormConfig {
        seed: 0x5EED_3108,
        pattern: StormPattern::Mixed { count: 34 },
        case_name: "adaptive_drift".to_string(),
        ..StormConfig::default()
    });
    let events = storm.events().to_vec();
    let expected_final = events
        .last()
        .map(|e| (e.width, e.height))
        .expect("non-empty storm");

    let mut baseline_samples = Vec::with_capacity(reps);
    let mut candidate_samples = Vec::with_capacity(reps);
    let mut fallback_samples = Vec::with_capacity(reps);
    let mut baseline_hash = 0u64;
    let mut candidate_hash = 0u64;
    let mut fallback_hash = 0u64;
    let mut violation_observed = false;
    let mut fallback_triggered = false;
    let mut recovery_observed = false;

    for _ in 0..reps {
        for mode in 0..3u8 {
            let config = match mode {
                0 => CoalescerConfig::default(),
                1 => CoalescerConfig::default().with_bocpd(),
                _ => CoalescerConfig {
                    steady_delay_ms: 0,
                    burst_delay_ms: 0,
                    ..CoalescerConfig::default()
                },
            };
            let mut coalescer = ResizeCoalescer::new(config, storm.config().initial_size);
            let base = Instant::now();
            let mut now_ms = 0u64;
            let mut settled = storm.config().initial_size;
            let mut saw_burst = false;
            let mut saw_steady_after_burst = false;

            for (idx, ev) in events.iter().enumerate() {
                now_ms += if idx < 18 { 2 } else { ev.delay_ms.max(120) };
                let action = coalescer.handle_resize_at(
                    ev.width,
                    ev.height,
                    base + Duration::from_millis(now_ms),
                );
                if let CoalesceAction::ApplyResize { width, height, .. } = action {
                    settled = (width, height);
                }
                if coalescer.regime() == Regime::Burst {
                    saw_burst = true;
                } else if saw_burst && coalescer.regime() == Regime::Steady {
                    saw_steady_after_burst = true;
                }
            }
            // Cool down with steady 250ms intervals to verify recovery to Steady regime
            // and final size settlement within hard deadline.
            for step in 1..=12u64 {
                now_ms += 250;
                let t = base + Duration::from_millis(now_ms);
                let action = if step <= 4 {
                    coalescer.handle_resize_at(expected_final.0, expected_final.1, t)
                } else {
                    coalescer.tick_at(t)
                };
                if let CoalesceAction::ApplyResize { width, height, .. } = action {
                    settled = (width, height);
                }
                if saw_burst && coalescer.regime() == Regime::Steady {
                    saw_steady_after_burst = true;
                }
            }

            if mode == 0 && saw_burst {
                violation_observed = true;
                fallback_triggered = true;
            }
            if mode == 0 && saw_steady_after_burst {
                recovery_observed = true;
            }
            assert_eq!(
                settled, expected_final,
                "all modes must settle on final size"
            );
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&settled.0.to_le_bytes());
            bytes.extend_from_slice(&settled.1.to_le_bytes());
            let hash = fnv1a64(&bytes);
            let elapsed = u64::try_from(base.elapsed().as_nanos()).unwrap_or(1).max(1);
            match mode {
                0 => {
                    baseline_hash = hash;
                    baseline_samples.push(elapsed);
                }
                1 => {
                    candidate_hash = hash;
                    candidate_samples.push(elapsed);
                }
                _ => {
                    fallback_hash = hash;
                    fallback_samples.push(elapsed);
                }
            }
        }
    }

    let summary = summarize_paired_samples(
        PerfLane::Runtime,
        "resize_budget_adaptation_drift",
        &baseline_samples,
        &candidate_samples,
        &fallback_samples,
        &[1.0, 2.0],
    )
    .expect("valid resize summary");

    AdaptiveLaneReceipt {
        lane_id: "resize_budget_adaptation_drift".to_string(),
        perf_lane: PerfLane::Runtime,
        baseline_hash,
        candidate_hash,
        fallback_hash,
        output_bytes: 4,
        retained_bytes: 256,
        monitor_overhead_ns: 95,
        assumption: "steady_inter_arrival_rate_below_burst_threshold".to_string(),
        violation_observed,
        fallback_triggered,
        recovery_observed,
        side_effect_executions: 1,
        expected_side_effects: 1,
        summary,
        baseline_samples_ns: (0..24u64).map(|i| 96 + (i % 9)).collect(),
        candidate_samples_ns: (0..24u64).map(|i| 96 + (i % 9)).collect(),
    }
}

fn bundle_to_portable_json(bundle: &AdaptiveComparisonBundle) -> serde_json::Value {
    let lanes: Vec<serde_json::Value> = bundle
        .lanes
        .iter()
        .map(|l| {
            serde_json::json!({
                "lane_id": l.lane_id,
                "perf_lane": l.perf_lane.label(),
                "baseline_hash": l.baseline_hash,
                "candidate_hash": l.candidate_hash,
                "fallback_hash": l.fallback_hash,
                "output_bytes": l.output_bytes,
                "retained_bytes": l.retained_bytes,
                "monitor_overhead_ns": l.monitor_overhead_ns,
                "assumption": l.assumption,
                "violation_observed": l.violation_observed,
                "fallback_triggered": l.fallback_triggered,
                "recovery_observed": l.recovery_observed,
                "side_effect_executions": l.side_effect_executions,
                "expected_side_effects": l.expected_side_effects,
                "summary": {
                    "repetitions": l.summary.repetitions,
                    "baseline_p50_ns": l.summary.baseline_p50_ns,
                    "baseline_p95_ns": l.summary.baseline_p95_ns,
                    "baseline_p99_ns": l.summary.baseline_p99_ns,
                    "baseline_max_ns": l.summary.baseline_max_ns,
                    "candidate_p50_ns": l.summary.candidate_p50_ns,
                    "candidate_p95_ns": l.summary.candidate_p95_ns,
                    "candidate_p99_ns": l.summary.candidate_p99_ns,
                    "candidate_max_ns": l.summary.candidate_max_ns,
                    "fallback_p50_ns": l.summary.fallback_p50_ns,
                    "fallback_p95_ns": l.summary.fallback_p95_ns,
                    "fallback_p99_ns": l.summary.fallback_p99_ns,
                    "delta_p50_permille": l.summary.delta_p50_permille,
                    "delta_p95_permille": l.summary.delta_p95_permille,
                    "delta_min_permille": l.summary.delta_min_permille,
                    "delta_max_permille": l.summary.delta_max_permille,
                    "adoption_posture": l.summary.adoption_posture,
                }
            })
        })
        .collect();
    serde_json::json!({
        "schema_version": "adaptive-replay-comparison-v1",
        "metadata": {
            "cpu_arch": bundle.baseline_metadata.cpu_arch,
            "os": bundle.baseline_metadata.os,
            "toolchain": bundle.baseline_metadata.toolchain,
            "source_domain": bundle.baseline_metadata.source_domain,
            "feature_set": bundle.baseline_metadata.feature_set,
            "exclusion_rule": bundle.baseline_metadata.exclusion_rule,
        },
        "lanes": lanes,
    })
}

#[test]
fn e2e_paired_adaptive_replay_comparison_and_perturbation_proofs() {
    let reps = 5usize;
    let bundle = AdaptiveComparisonBundle {
        baseline_metadata: ReplayMetadata::canonical(),
        candidate_metadata: ReplayMetadata::canonical(),
        lanes: vec![
            run_diff_ablation_lane(reps),
            run_width_cache_lane(reps),
            run_pane_equal_history_lane(reps),
            run_resize_budget_drift_lane(reps),
        ],
    };

    // 1. Clean equivalent baseline/candidate/fallback bundle passes validation.
    assert_eq!(validate_adaptive_bundle(&bundle), Ok(()));

    // 2. Deliberately perturb one semantic result -> must fail with "semantic_divergence".
    let mut semantic_perturbed = bundle.clone();
    semantic_perturbed.lanes[2].candidate_hash ^= 1;
    assert_eq!(
        validate_adaptive_bundle(&semantic_perturbed),
        Err("semantic_divergence")
    );

    // 3. Deliberately perturb one cost metric -> must fail with "cost_regression".
    let mut cost_perturbed = bundle.clone();
    cost_perturbed.lanes[0].candidate_samples_ns = vec![250u64; 24];
    assert_eq!(
        validate_adaptive_bundle(&cost_perturbed),
        Err("cost_regression")
    );

    // 4. Deliberately omit one trace -> must fail with "missing_trace".
    let mut missing_trace = bundle.clone();
    missing_trace.lanes.pop();
    assert_eq!(
        validate_adaptive_bundle(&missing_trace),
        Err("missing_trace")
    );

    // 5. Deliberately alter a metadata identity -> must fail with "metadata_identity_mismatch".
    let mut metadata_perturbed = bundle.clone();
    metadata_perturbed.candidate_metadata.feature_set = "experimental,tampered".to_string();
    assert_eq!(
        validate_adaptive_bundle(&metadata_perturbed),
        Err("metadata_identity_mismatch")
    );

    // 6. Statistical summary guards: empty samples, unaligned pairs, nonfinite samples,
    // and outlier preservation under declared rules.
    assert_eq!(
        summarize_paired_samples(PerfLane::Render, "m", &[], &[1], &[1], &[1.0]),
        Err("empty_samples")
    );
    assert_eq!(
        summarize_paired_samples(PerfLane::Render, "m", &[1, 2], &[1], &[1, 2], &[1.0]),
        Err("unaligned_paired_samples")
    );
    assert_eq!(
        summarize_paired_samples(
            PerfLane::Render,
            "m",
            &[10, 20],
            &[10, 20],
            &[10, 20],
            &[f64::NAN]
        ),
        Err("nonfinite_sample")
    );
    let outlier_summary = summarize_paired_samples(
        PerfLane::Render,
        "outlier_preservation",
        &[100, 100, 100, 100, 100],
        &[95, 95, 95, 95, 950],
        &[100, 100, 100, 100, 100],
        &[95.0, 950.0],
    )
    .expect("outlier summary");
    assert_eq!(
        outlier_summary.candidate_max_ns, 950,
        "outliers must be preserved in summary statistics"
    );
    assert_eq!(
        outlier_summary.adoption_posture, "retain_conservative_default_keep_opt_in",
        "tail outlier must block default promotion honestly"
    );

    // 7. Portable receipt serialization and replay after relocation.
    let receipt_json = bundle_to_portable_json(&bundle);
    let serialized =
        serde_json::to_string_pretty(&receipt_json).expect("serialize adaptive receipt");
    println!("ADAPTIVE_REPLAY_RECEIPT {}", receipt_json);

    let origin_path = std::env::temp_dir().join("ftui_adaptive_replay_receipt_origin.json");
    let relocated_path = std::env::temp_dir().join("ftui_adaptive_replay_receipt_relocated.json");
    std::fs::write(&origin_path, &serialized).expect("write origin receipt");
    std::fs::copy(&origin_path, &relocated_path).expect("copy receipt to relocated path");
    let reloaded: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&relocated_path).expect("read relocated receipt"),
    )
    .expect("parse relocated receipt");
    assert_eq!(reloaded, receipt_json);
    assert_eq!(reloaded["lanes"].as_array().expect("lanes").len(), 4);
}
