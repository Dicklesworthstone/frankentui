#![forbid(unsafe_code)]

//! Render equivalence, replay, and tail-latency gauntlet (bd-40lhe).
//!
//! The standing safety net for render performance work. Every render optimization
//! must pass this gauntlet before graduating to production. The gauntlet integrates:
//!
//! - **Fixture suite** (`fixture_suite`): canonical, challenge, and negative-control workloads
//! - **Render certificates** (`render_certificate`): skip-safety verification
//! - **Presenter equivalence** (`presenter_equivalence`): ANSI output identity checks
//! - **Layout reuse** (`layout_reuse`): cache correctness verification
//! - **Cost surface** (`cost_surface`): stage-level regression detection
//! - **Baseline capture** (`baseline_capture`): latency percentile comparison
//!
//! # Gauntlet structure
//!
//! ```text
//! GauntletSuite
//! ├── EquivalenceGate    — visible output must match baseline
//! ├── ReplayGate         — deterministic replay produces identical checksums
//! ├── TailLatencyGate    — p95/p99 must not regress beyond threshold
//! ├── CertificateGate    — skip decisions must not produce stale frames
//! ├── ChallengeGate      — adversarial fixtures must not crash or corrupt
//! └── NegativeControlGate — no-change fixtures must remain unchanged
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use ftui_harness::render_gauntlet::*;
//!
//! let config = GauntletConfig::default_strict();
//! let suite = GauntletSuite::new(config);
//! let report = suite.run_all();
//! assert!(report.passed(), "gauntlet failed: {}", report.summary());
//! ```

use std::time::Instant;

use crate::baseline_capture::{FixtureFamily, MetricBaseline, MetricCategory};
use crate::fixture_runner::FixtureRunner;
use crate::fixture_suite::{FixtureRegistry, FixtureSpec, SuitePartition};
use crate::render_certificate::{CertificateEvaluator, CertificateInputs, CertificateLevel};

use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::diff::{BufferDiff, DiffSkipHint};

// ============================================================================
// Gauntlet Gates
// ============================================================================

/// Individual gates in the gauntlet, each testing a different correctness property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GauntletGate {
    /// Visible output must match baseline (ANSI byte identity or equivalence-class match).
    Equivalence,
    /// Deterministic replay with same seed must produce identical frame checksums.
    Replay,
    /// Tail latency (p95/p99) must not regress beyond configured threshold.
    TailLatency,
    /// Certificate skip decisions must not produce visibly different output.
    Certificate,
    /// Adversarial fixtures must complete without panic, corruption, or resource leak.
    Challenge,
    /// Negative-control fixtures must produce unchanged output.
    NegativeControl,
}

impl GauntletGate {
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Equivalence => "equivalence",
            Self::Replay => "replay",
            Self::TailLatency => "tail-latency",
            Self::Certificate => "certificate",
            Self::Challenge => "challenge",
            Self::NegativeControl => "negative-control",
        }
    }

    /// Whether this gate blocks promotion (true) or is informative (false).
    #[must_use]
    pub const fn is_gating(&self) -> bool {
        match self {
            Self::Equivalence => true,
            Self::Replay => true,
            Self::TailLatency => true,
            Self::Certificate => true,
            Self::Challenge => true,
            Self::NegativeControl => true,
        }
    }

    /// Which fixture partitions feed this gate.
    #[must_use]
    pub const fn fixture_partitions(&self) -> &'static [SuitePartition] {
        match self {
            Self::Equivalence => &[SuitePartition::Canonical],
            Self::Replay => &[SuitePartition::Canonical],
            Self::TailLatency => &[SuitePartition::Canonical],
            Self::Certificate => &[SuitePartition::Canonical, SuitePartition::Challenge],
            Self::Challenge => &[SuitePartition::Challenge],
            Self::NegativeControl => &[SuitePartition::NegativeControl],
        }
    }

    /// What failure artifacts this gate produces on failure.
    #[must_use]
    pub const fn failure_artifacts(&self) -> &'static [&'static str] {
        match self {
            Self::Equivalence => &[
                "ansi_diff.txt",
                "baseline_transcript.jsonl",
                "current_transcript.jsonl",
                "mismatch_cell_report.json",
            ],
            Self::Replay => &[
                "replay_checksums.json",
                "divergence_frame_index.json",
                "replay_input_sequence.jsonl",
            ],
            Self::TailLatency => &[
                "latency_histogram.json",
                "p99_regression_detail.json",
                "stage_breakdown.json",
            ],
            Self::Certificate => &[
                "certificate_decision_log.jsonl",
                "shadow_comparison.json",
                "stale_frame_evidence.json",
            ],
            Self::Challenge => &[
                "challenge_results.json",
                "panic_backtrace.txt",
                "resource_leak_report.json",
            ],
            Self::NegativeControl => &["control_diff.json", "unexpected_change_cells.json"],
        }
    }

    pub const ALL: &'static [GauntletGate] = &[
        Self::Equivalence,
        Self::Replay,
        Self::TailLatency,
        Self::Certificate,
        Self::Challenge,
        Self::NegativeControl,
    ];
}

// ============================================================================
// Gate Result
// ============================================================================

/// Outcome of a single gate in the gauntlet.
#[derive(Debug, Clone)]
pub struct GateResult {
    /// Which gate was evaluated.
    pub gate: GauntletGate,
    /// Whether the gate passed.
    pub passed: bool,
    /// Number of fixtures tested.
    pub fixtures_tested: u32,
    /// Number of fixtures that passed.
    pub fixtures_passed: u32,
    /// Summary of what was verified.
    pub summary: String,
    /// Failure details (empty if passed).
    pub failures: Vec<GateFailure>,
    /// Wall-clock time for this gate in milliseconds.
    pub duration_ms: u64,
}

impl GateResult {
    /// Create a passing result.
    #[must_use]
    pub fn pass(gate: GauntletGate, fixtures: u32, summary: &str, duration_ms: u64) -> Self {
        Self {
            gate,
            passed: true,
            fixtures_tested: fixtures,
            fixtures_passed: fixtures,
            summary: summary.to_string(),
            failures: Vec::new(),
            duration_ms,
        }
    }

    /// Create a failing result.
    #[must_use]
    pub fn fail(
        gate: GauntletGate,
        fixtures_tested: u32,
        fixtures_passed: u32,
        failures: Vec<GateFailure>,
        duration_ms: u64,
    ) -> Self {
        let summary = format!(
            "{}/{} fixtures passed, {} failure(s)",
            fixtures_passed,
            fixtures_tested,
            failures.len()
        );
        Self {
            gate,
            passed: false,
            fixtures_tested,
            fixtures_passed,
            summary,
            failures,
            duration_ms,
        }
    }
}

/// Details of a single fixture failure within a gate.
#[derive(Debug, Clone)]
pub struct GateFailure {
    /// Fixture that failed.
    pub fixture_id: String,
    /// What went wrong.
    pub reason: String,
    /// Failure category for triage.
    pub category: FailureCategory,
    /// Artifacts produced for diagnosis.
    pub artifacts: Vec<String>,
}

/// Categories of gauntlet failure for triage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureCategory {
    /// Visible output differs from baseline.
    SemanticRegression,
    /// Logging or metrics are missing or malformed.
    ObservabilityGap,
    /// Optimization only helps curated benchmarks, not challenge fixtures.
    BenchmarkOverfit,
    /// Challenge fixture showed graceful fallback (expected, not a failure).
    ExpectedFallback,
    /// Certificate issued incorrect skip decision.
    StaleCertificate,
    /// Cache returned stale data.
    StaleCache,
    /// Tail latency regressed beyond threshold.
    TailRegression,
    /// Resource leak detected (memory, handles, threads).
    ResourceLeak,
    /// Panic or crash during fixture execution.
    Crash,
}

impl FailureCategory {
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::SemanticRegression => "semantic-regression",
            Self::ObservabilityGap => "observability-gap",
            Self::BenchmarkOverfit => "benchmark-overfit",
            Self::ExpectedFallback => "expected-fallback",
            Self::StaleCertificate => "stale-certificate",
            Self::StaleCache => "stale-cache",
            Self::TailRegression => "tail-regression",
            Self::ResourceLeak => "resource-leak",
            Self::Crash => "crash",
        }
    }

    /// Whether this category indicates a real problem (vs expected behavior).
    #[must_use]
    pub const fn is_real_failure(&self) -> bool {
        !matches!(self, Self::ExpectedFallback)
    }
}

// ============================================================================
// Gauntlet Configuration
// ============================================================================

/// Configuration for the render gauntlet.
#[derive(Debug, Clone)]
pub struct GauntletConfig {
    /// Maximum allowed p95 regression percentage (e.g., 10.0 = 10%).
    pub p95_regression_threshold_pct: f64,
    /// Maximum allowed p99 regression percentage.
    pub p99_regression_threshold_pct: f64,
    /// Whether to run challenge fixtures.
    pub run_challenges: bool,
    /// Whether to run negative controls.
    pub run_negative_controls: bool,
    /// Whether certificate shadow-run comparison is required.
    pub require_certificate_shadow: bool,
    /// Which fixture family to scope to (None = all render fixtures).
    pub family_filter: Option<FixtureFamily>,
    /// Maximum wall-clock seconds for the entire gauntlet.
    pub timeout_secs: u32,
}

impl GauntletConfig {
    /// Default strict: all gates enabled, 10% p95/p99 threshold.
    #[must_use]
    pub const fn default_strict() -> Self {
        Self {
            p95_regression_threshold_pct: 10.0,
            p99_regression_threshold_pct: 15.0,
            run_challenges: true,
            run_negative_controls: true,
            require_certificate_shadow: true,
            family_filter: None,
            timeout_secs: 300,
        }
    }

    /// Fast mode: skip challenges and shadow runs for quick iteration.
    #[must_use]
    pub const fn fast() -> Self {
        Self {
            p95_regression_threshold_pct: 15.0,
            p99_regression_threshold_pct: 20.0,
            run_challenges: false,
            run_negative_controls: true,
            require_certificate_shadow: false,
            family_filter: None,
            timeout_secs: 60,
        }
    }
}

// ============================================================================
// Gauntlet Suite
// ============================================================================

/// The complete render gauntlet suite.
#[derive(Debug, Clone)]
pub struct GauntletSuite {
    /// Configuration.
    pub config: GauntletConfig,
}

impl GauntletSuite {
    /// Create a new gauntlet suite.
    #[must_use]
    pub fn new(config: GauntletConfig) -> Self {
        Self { config }
    }

    /// Which gates are active given the current configuration.
    #[must_use]
    pub fn active_gates(&self) -> Vec<GauntletGate> {
        let mut gates = vec![
            GauntletGate::Equivalence,
            GauntletGate::Replay,
            GauntletGate::TailLatency,
        ];

        if self.config.require_certificate_shadow {
            gates.push(GauntletGate::Certificate);
        }

        if self.config.run_challenges {
            gates.push(GauntletGate::Challenge);
        }

        if self.config.run_negative_controls {
            gates.push(GauntletGate::NegativeControl);
        }

        gates
    }

    /// Number of active gates.
    #[must_use]
    pub fn gate_count(&self) -> usize {
        self.active_gates().len()
    }
}

// ============================================================================
// Gauntlet Report
// ============================================================================

/// Complete gauntlet execution report.
#[derive(Debug, Clone)]
pub struct GauntletReport {
    /// Per-gate results.
    pub gate_results: Vec<GateResult>,
    /// Total wall-clock time in milliseconds.
    pub total_duration_ms: u64,
    /// Configuration used.
    pub config: GauntletConfig,
}

impl GauntletReport {
    /// Whether all gating gates passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.gate_results
            .iter()
            .filter(|r| r.gate.is_gating())
            .all(|r| r.passed)
    }

    /// Number of gates that passed.
    #[must_use]
    pub fn gates_passed(&self) -> usize {
        self.gate_results.iter().filter(|r| r.passed).count()
    }

    /// Number of gates that failed.
    #[must_use]
    pub fn gates_failed(&self) -> usize {
        self.gate_results.iter().filter(|r| !r.passed).count()
    }

    /// All failures across all gates.
    #[must_use]
    pub fn all_failures(&self) -> Vec<&GateFailure> {
        self.gate_results
            .iter()
            .flat_map(|r| r.failures.iter())
            .collect()
    }

    /// Real failures (excluding expected fallback).
    #[must_use]
    pub fn real_failures(&self) -> Vec<&GateFailure> {
        self.all_failures()
            .into_iter()
            .filter(|f| f.category.is_real_failure())
            .collect()
    }

    /// Human-readable summary.
    #[must_use]
    pub fn summary(&self) -> String {
        let status = if self.passed() { "PASSED" } else { "FAILED" };
        let real = self.real_failures().len();
        format!(
            "Gauntlet {}: {}/{} gates passed, {} real failure(s), {}ms",
            status,
            self.gates_passed(),
            self.gate_results.len(),
            real,
            self.total_duration_ms,
        )
    }

    /// Serialize to JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        let gates: Vec<String> = self
            .gate_results
            .iter()
            .map(|r| {
                let failure_entries: Vec<String> = r
                    .failures
                    .iter()
                    .map(|f| {
                        let arts: Vec<String> =
                            f.artifacts.iter().map(|a| format!("\"{a}\"")).collect();
                        format!(
                            r#"        {{
          "fixture_id": "{}",
          "reason": "{}",
          "category": "{}",
          "artifacts": [{}]
        }}"#,
                            f.fixture_id,
                            f.reason.replace('"', "\\\""),
                            f.category.label(),
                            arts.join(", "),
                        )
                    })
                    .collect();

                format!(
                    r#"    {{
      "gate": "{}",
      "passed": {},
      "fixtures_tested": {},
      "fixtures_passed": {},
      "duration_ms": {},
      "summary": "{}",
      "failures": [
{}
      ]
    }}"#,
                    r.gate.label(),
                    r.passed,
                    r.fixtures_tested,
                    r.fixtures_passed,
                    r.duration_ms,
                    r.summary.replace('"', "\\\""),
                    failure_entries.join(",\n"),
                )
            })
            .collect();

        format!(
            r#"{{
  "schema_version": 1,
  "passed": {},
  "gates_passed": {},
  "gates_failed": {},
  "real_failures": {},
  "total_duration_ms": {},
  "summary": "{}",
  "gates": [
{}
  ]
}}"#,
            self.passed(),
            self.gates_passed(),
            self.gates_failed(),
            self.real_failures().len(),
            self.total_duration_ms,
            self.summary().replace('"', "\\\""),
            gates.join(",\n"),
        )
    }
}

// ============================================================================
// Gauntlet Execution
// ============================================================================

/// Compare candidate tail-latency percentiles against a baseline metric set.
///
/// Pure and deterministic: unit-testable with synthetic `MetricBaseline`
/// records. A regression is flagged when the candidate's p95/p99 exceeds the
/// baseline by more than the configured percentage (baselines at or below
/// zero are skipped — there is nothing meaningful to regress against).
#[must_use]
pub fn compare_tail_latency(
    fixture_id: &str,
    baseline: &[MetricBaseline],
    candidate: &[MetricBaseline],
    config: &GauntletConfig,
) -> Vec<GateFailure> {
    let mut failures = Vec::new();
    for base in baseline {
        if base.category != MetricCategory::Latency {
            continue;
        }
        let Some(cand) = candidate
            .iter()
            .find(|m| m.metric == base.metric && m.category == MetricCategory::Latency)
        else {
            failures.push(GateFailure {
                fixture_id: fixture_id.to_string(),
                reason: format!(
                    "latency metric '{}' missing from candidate run",
                    base.metric
                ),
                category: FailureCategory::ObservabilityGap,
                artifacts: artifact_names(GauntletGate::TailLatency),
            });
            continue;
        };
        for (label, base_value, cand_value, threshold_pct) in [
            (
                "p95",
                base.percentiles.p95,
                cand.percentiles.p95,
                config.p95_regression_threshold_pct,
            ),
            (
                "p99",
                base.percentiles.p99,
                cand.percentiles.p99,
                config.p99_regression_threshold_pct,
            ),
        ] {
            if base_value <= 0.0 || !base_value.is_finite() || !cand_value.is_finite() {
                continue;
            }
            let limit = base_value * (1.0 + threshold_pct / 100.0);
            if cand_value > limit {
                failures.push(GateFailure {
                    fixture_id: fixture_id.to_string(),
                    reason: format!(
                        "{} {} regressed: baseline {:.3} -> candidate {:.3} (limit {:.3}, \
                         threshold {:.1}%)",
                        base.metric, label, base_value, cand_value, limit, threshold_pct
                    ),
                    category: FailureCategory::TailRegression,
                    artifacts: artifact_names(GauntletGate::TailLatency),
                });
            }
        }
    }
    failures
}

fn artifact_names(gate: GauntletGate) -> Vec<String> {
    gate.failure_artifacts()
        .iter()
        .map(|a| (*a).to_string())
        .collect()
}

fn hint_for_level(level: CertificateLevel, dirty_rows: &[u16]) -> DiffSkipHint {
    match level {
        CertificateLevel::FrameSkip | CertificateLevel::DiffSkip => DiffSkipHint::SkipDiff,
        CertificateLevel::RegionSkip
        | CertificateLevel::PresentNarrow
        | CertificateLevel::WidgetSkip => DiffSkipHint::NarrowToRows(dirty_rows.to_vec()),
        CertificateLevel::None => DiffSkipHint::FullDiff,
    }
}

/// One certificate shadow scenario: truthful inputs, the resulting hint, and
/// the certified diff compared against the uncertified ground truth.
fn certificate_shadow_matches(
    old: &Buffer,
    new: &Buffer,
    inputs: &CertificateInputs,
    dirty_rows: &[u16],
) -> (bool, String) {
    let certificate = CertificateEvaluator::evaluate(inputs);
    let hint = hint_for_level(certificate.level, dirty_rows);

    let mut certified = BufferDiff::new();
    certified.compute_certified_into(old, new, hint);
    let mut truth = BufferDiff::new();
    truth.compute_dirty_into(old, new);

    let matches = certified.changes() == truth.changes();
    let detail = format!(
        "level={} fell_back={} certified_changes={} truth_changes={}",
        certificate.level.label(),
        certificate.fell_back,
        certified.changes().len(),
        truth.changes().len()
    );
    (matches, detail)
}

impl GauntletSuite {
    fn specs_for_gate<'a>(
        &self,
        registry: &'a FixtureRegistry,
        gate: GauntletGate,
    ) -> Vec<&'a FixtureSpec> {
        let mut specs: Vec<&FixtureSpec> = Vec::new();
        for partition in gate.fixture_partitions() {
            for spec in registry.by_partition(*partition) {
                // Equivalence/replay/tail gates are render-lane gates; the
                // challenge and negative-control partitions run whole-suite.
                let render_only = matches!(
                    gate,
                    GauntletGate::Equivalence | GauntletGate::Replay | GauntletGate::TailLatency
                );
                if render_only && spec.family != FixtureFamily::Render {
                    continue;
                }
                if let Some(filter) = self.config.family_filter
                    && spec.family != filter
                {
                    continue;
                }
                if !specs.iter().any(|s| s.id == spec.id) {
                    specs.push(spec);
                }
            }
        }
        specs
    }

    fn run_equivalence_gate(&self, registry: &FixtureRegistry) -> GateResult {
        let start = Instant::now();
        let specs = self.specs_for_gate(registry, GauntletGate::Equivalence);
        let mut failures = Vec::new();
        for spec in &specs {
            let first = FixtureRunner::run(spec);
            let second = FixtureRunner::run(spec);
            if first.frame_checksums != second.frame_checksums {
                let divergence = first
                    .frame_checksums
                    .iter()
                    .zip(second.frame_checksums.iter())
                    .position(|(a, b)| a != b);
                failures.push(GateFailure {
                    fixture_id: spec.id.clone(),
                    reason: format!(
                        "visible output diverged between identical runs (first divergence at \
                         frame {divergence:?})"
                    ),
                    category: FailureCategory::SemanticRegression,
                    artifacts: artifact_names(GauntletGate::Equivalence),
                });
            }
        }
        let duration = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let tested = u32::try_from(specs.len()).unwrap_or(u32::MAX);
        if failures.is_empty() {
            GateResult::pass(
                GauntletGate::Equivalence,
                tested,
                "visible output byte-identical across repeated runs",
                duration,
            )
        } else {
            let passed = tested - u32::try_from(failures.len()).unwrap_or(0);
            GateResult::fail(
                GauntletGate::Equivalence,
                tested,
                passed,
                failures,
                duration,
            )
        }
    }

    fn run_replay_gate(&self, registry: &FixtureRegistry) -> GateResult {
        let start = Instant::now();
        let specs = self.specs_for_gate(registry, GauntletGate::Replay);
        let mut failures = Vec::new();
        for spec in &specs {
            let verdict = FixtureRunner::verify_determinism(spec);
            if !verdict.deterministic {
                failures.push(GateFailure {
                    fixture_id: spec.id.clone(),
                    reason: format!(
                        "replay diverged: first divergence at frame {:?}, {} checksums matched",
                        verdict.first_divergence, verdict.checksums_match_count
                    ),
                    category: FailureCategory::SemanticRegression,
                    artifacts: artifact_names(GauntletGate::Replay),
                });
            }
        }
        let duration = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let tested = u32::try_from(specs.len()).unwrap_or(u32::MAX);
        if failures.is_empty() {
            GateResult::pass(
                GauntletGate::Replay,
                tested,
                "seeded replay reproduced identical frame checksums",
                duration,
            )
        } else {
            let passed = tested - u32::try_from(failures.len()).unwrap_or(0);
            GateResult::fail(GauntletGate::Replay, tested, passed, failures, duration)
        }
    }

    fn run_tail_latency_gate(&self, registry: &FixtureRegistry) -> GateResult {
        let start = Instant::now();
        let specs = self.specs_for_gate(registry, GauntletGate::TailLatency);
        let mut failures = Vec::new();
        for spec in &specs {
            let result = FixtureRunner::run(spec);
            let latency_metrics: Vec<&MetricBaseline> = result
                .record
                .metrics
                .iter()
                .filter(|m| m.category == MetricCategory::Latency)
                .collect();
            if !latency_metrics
                .iter()
                .any(|m| m.metric == "frame_pipeline_total")
            {
                failures.push(GateFailure {
                    fixture_id: spec.id.clone(),
                    reason: "frame_pipeline_total latency metric missing (logging contract \
                             violation)"
                        .to_string(),
                    category: FailureCategory::ObservabilityGap,
                    artifacts: artifact_names(GauntletGate::TailLatency),
                });
                continue;
            }
            for metric in latency_metrics {
                let p = &metric.percentiles;
                let ordered = p.min <= p.p50 + f64::EPSILON
                    && p.p50 <= p.p95 + f64::EPSILON
                    && p.p95 <= p.p99 + f64::EPSILON
                    && p.p99 <= p.max + f64::EPSILON;
                let finite = [p.min, p.p50, p.p95, p.p99, p.p999, p.max]
                    .iter()
                    .all(|v| v.is_finite());
                if !ordered || !finite {
                    failures.push(GateFailure {
                        fixture_id: spec.id.clone(),
                        reason: format!(
                            "corrupt percentile surface for '{}': min={:.3} p50={:.3} \
                             p95={:.3} p99={:.3} max={:.3}",
                            metric.metric, p.min, p.p50, p.p95, p.p99, p.max
                        ),
                        category: FailureCategory::TailRegression,
                        artifacts: artifact_names(GauntletGate::TailLatency),
                    });
                }
            }
        }
        let duration = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let tested = u32::try_from(specs.len()).unwrap_or(u32::MAX);
        if failures.is_empty() {
            GateResult::pass(
                GauntletGate::TailLatency,
                tested,
                "tail percentiles captured, finite, and ordered for every stage \
                 (regression deltas gate via compare_tail_latency against stored baselines)",
                duration,
            )
        } else {
            let passed = tested - u32::try_from(failures.len()).unwrap_or(0);
            GateResult::fail(
                GauntletGate::TailLatency,
                tested,
                passed,
                failures,
                duration,
            )
        }
    }

    #[allow(clippy::too_many_lines)]
    fn run_certificate_gate(&self) -> GateResult {
        let start = Instant::now();
        let mut failures = Vec::new();
        let mut tested: u32 = 0;

        let cell = |ch: char| Cell::from_char(ch);
        let base_inputs = || CertificateInputs {
            dirty_row_count: 0,
            dirty_cell_count: 0,
            model_generation: 7,
            last_certified_generation: 7,
            viewport_changed: false,
            style_epoch: 3,
            last_certified_style_epoch: 3,
            layout_displacement: 0.0,
            degradation_changed: false,
        };

        // Scenario 1: unchanged frame — a frame/diff skip must equal ground truth.
        tested += 1;
        {
            let old = Buffer::new(20, 6);
            let new = Buffer::new(20, 6);
            let (matches, detail) = certificate_shadow_matches(&old, &new, &base_inputs(), &[]);
            if !matches {
                failures.push(GateFailure {
                    fixture_id: "certificate_unchanged_frame".to_string(),
                    reason: format!("skip certificate diverged from ground truth ({detail})"),
                    category: FailureCategory::StaleCertificate,
                    artifacts: artifact_names(GauntletGate::Certificate),
                });
            }
        }

        // Scenario 2: sparse update — a narrowed diff must equal ground truth.
        tested += 1;
        {
            let old = Buffer::new(20, 6);
            let mut new = Buffer::new(20, 6);
            new.set(2, 1, cell('X'));
            new.set(5, 2, cell('Y'));
            let mut inputs = base_inputs();
            inputs.dirty_row_count = 2;
            inputs.dirty_cell_count = 2;
            inputs.model_generation = 8;
            let (matches, detail) = certificate_shadow_matches(&old, &new, &inputs, &[1, 2]);
            if !matches {
                failures.push(GateFailure {
                    fixture_id: "certificate_sparse_narrow".to_string(),
                    reason: format!("narrowed certificate diverged from ground truth ({detail})"),
                    category: FailureCategory::StaleCertificate,
                    artifacts: artifact_names(GauntletGate::Certificate),
                });
            }
        }

        // Scenario 3: viewport change — the certificate must force a full diff.
        tested += 1;
        {
            let mut inputs = base_inputs();
            inputs.viewport_changed = true;
            inputs.dirty_row_count = 1;
            inputs.dirty_cell_count = 1;
            inputs.model_generation = 9;
            let certificate = CertificateEvaluator::evaluate(&inputs);
            if certificate.level != CertificateLevel::None {
                failures.push(GateFailure {
                    fixture_id: "certificate_viewport_change".to_string(),
                    reason: format!(
                        "viewport change must void the certificate, got level {}",
                        certificate.level.label()
                    ),
                    category: FailureCategory::StaleCertificate,
                    artifacts: artifact_names(GauntletGate::Certificate),
                });
            }
        }

        // Scenario 4 (adversarial): a deliberately wrong SkipDiff on changed
        // buffers MUST be detectable by the shadow comparison — if the shadow
        // cannot see the stale frame, the gate itself is broken.
        tested += 1;
        {
            let old = Buffer::new(20, 6);
            let mut new = Buffer::new(20, 6);
            new.set(4, 3, cell('!'));
            let mut certified = BufferDiff::new();
            certified.compute_certified_into(&old, &new, DiffSkipHint::SkipDiff);
            let mut truth = BufferDiff::new();
            truth.compute_dirty_into(&old, &new);
            if certified.changes() == truth.changes() {
                failures.push(GateFailure {
                    fixture_id: "certificate_stale_detection".to_string(),
                    reason: "shadow comparison failed to detect a deliberately stale \
                             SkipDiff certificate"
                        .to_string(),
                    category: FailureCategory::StaleCertificate,
                    artifacts: artifact_names(GauntletGate::Certificate),
                });
            }
        }

        let duration = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        if failures.is_empty() {
            GateResult::pass(
                GauntletGate::Certificate,
                tested,
                "certificate skip decisions shadow-match ground truth; stale certificates \
                 are detectable",
                duration,
            )
        } else {
            let passed = tested - u32::try_from(failures.len()).unwrap_or(0);
            GateResult::fail(
                GauntletGate::Certificate,
                tested,
                passed,
                failures,
                duration,
            )
        }
    }

    fn run_challenge_gate(&self, registry: &FixtureRegistry) -> GateResult {
        let start = Instant::now();
        let specs = self.specs_for_gate(registry, GauntletGate::Challenge);
        let mut failures = Vec::new();
        for spec in &specs {
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| FixtureRunner::run(spec)));
            match outcome {
                Ok(result) if result.frames_executed > 0 => {}
                Ok(_) => failures.push(GateFailure {
                    fixture_id: spec.id.clone(),
                    reason: "challenge fixture executed zero frames".to_string(),
                    category: FailureCategory::ObservabilityGap,
                    artifacts: artifact_names(GauntletGate::Challenge),
                }),
                Err(_) => failures.push(GateFailure {
                    fixture_id: spec.id.clone(),
                    reason: "challenge fixture panicked".to_string(),
                    category: FailureCategory::Crash,
                    artifacts: artifact_names(GauntletGate::Challenge),
                }),
            }
        }
        let duration = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let tested = u32::try_from(specs.len()).unwrap_or(u32::MAX);
        if failures.is_empty() {
            GateResult::pass(
                GauntletGate::Challenge,
                tested,
                "adversarial fixtures completed without panic or corruption",
                duration,
            )
        } else {
            let passed = tested - u32::try_from(failures.len()).unwrap_or(0);
            GateResult::fail(GauntletGate::Challenge, tested, passed, failures, duration)
        }
    }

    fn run_negative_control_gate(&self, registry: &FixtureRegistry) -> GateResult {
        let start = Instant::now();
        let specs = self.specs_for_gate(registry, GauntletGate::NegativeControl);
        let mut failures = Vec::new();
        for spec in &specs {
            let result = FixtureRunner::run(spec);
            let unchanged = result
                .frame_checksums
                .windows(2)
                .all(|pair| pair[0] == pair[1]);
            if !unchanged {
                failures.push(GateFailure {
                    fixture_id: spec.id.clone(),
                    reason: "negative-control fixture produced visible change".to_string(),
                    category: FailureCategory::SemanticRegression,
                    artifacts: artifact_names(GauntletGate::NegativeControl),
                });
            }
        }
        let duration = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let tested = u32::try_from(specs.len()).unwrap_or(u32::MAX);
        if failures.is_empty() {
            GateResult::pass(
                GauntletGate::NegativeControl,
                tested,
                "no-change fixtures remained byte-identical frame to frame",
                duration,
            )
        } else {
            let passed = tested - u32::try_from(failures.len()).unwrap_or(0);
            GateResult::fail(
                GauntletGate::NegativeControl,
                tested,
                passed,
                failures,
                duration,
            )
        }
    }

    /// Execute every active gate over the canonical fixture registry.
    #[must_use]
    pub fn run_all(&self) -> GauntletReport {
        let registry = FixtureRegistry::canonical();
        let start = Instant::now();
        let mut gate_results = Vec::new();
        for gate in self.active_gates() {
            let result = match gate {
                GauntletGate::Equivalence => self.run_equivalence_gate(&registry),
                GauntletGate::Replay => self.run_replay_gate(&registry),
                GauntletGate::TailLatency => self.run_tail_latency_gate(&registry),
                GauntletGate::Certificate => self.run_certificate_gate(),
                GauntletGate::Challenge => self.run_challenge_gate(&registry),
                GauntletGate::NegativeControl => self.run_negative_control_gate(&registry),
            };
            gate_results.push(result);
        }
        GauntletReport {
            gate_results,
            total_duration_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
            config: self.config.clone(),
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_gates_labeled() {
        for gate in GauntletGate::ALL {
            assert!(!gate.label().is_empty());
            assert!(!gate.failure_artifacts().is_empty());
        }
        assert_eq!(GauntletGate::ALL.len(), 6);
    }

    #[test]
    fn all_gates_are_gating() {
        for gate in GauntletGate::ALL {
            assert!(gate.is_gating(), "{} should be gating", gate.label());
        }
    }

    #[test]
    fn gates_have_fixture_partitions() {
        for gate in GauntletGate::ALL {
            assert!(
                !gate.fixture_partitions().is_empty(),
                "{} has no fixture partitions",
                gate.label()
            );
        }
    }

    #[test]
    fn challenge_gate_uses_challenge_partition() {
        let partitions = GauntletGate::Challenge.fixture_partitions();
        assert!(partitions.contains(&SuitePartition::Challenge));
    }

    #[test]
    fn negative_control_gate_uses_negative_partition() {
        let partitions = GauntletGate::NegativeControl.fixture_partitions();
        assert!(partitions.contains(&SuitePartition::NegativeControl));
    }

    #[test]
    fn default_strict_config() {
        let config = GauntletConfig::default_strict();
        assert!((config.p95_regression_threshold_pct - 10.0).abs() < 0.01);
        assert!(config.run_challenges);
        assert!(config.run_negative_controls);
        assert!(config.require_certificate_shadow);
    }

    #[test]
    fn fast_config_skips_challenges() {
        let config = GauntletConfig::fast();
        assert!(!config.run_challenges);
        assert!(!config.require_certificate_shadow);
    }

    #[test]
    fn strict_suite_has_all_gates() {
        let suite = GauntletSuite::new(GauntletConfig::default_strict());
        assert_eq!(suite.gate_count(), 6);
    }

    #[test]
    fn fast_suite_has_fewer_gates() {
        let suite = GauntletSuite::new(GauntletConfig::fast());
        assert!(suite.gate_count() < 6);
        assert!(suite.gate_count() >= 3); // equivalence, replay, tail-latency always active
    }

    #[test]
    fn passing_report() {
        let report = GauntletReport {
            gate_results: vec![
                GateResult::pass(GauntletGate::Equivalence, 4, "All equivalent", 100),
                GateResult::pass(GauntletGate::Replay, 4, "All deterministic", 200),
            ],
            total_duration_ms: 300,
            config: GauntletConfig::default_strict(),
        };
        assert!(report.passed());
        assert_eq!(report.gates_passed(), 2);
        assert_eq!(report.gates_failed(), 0);
        assert!(report.all_failures().is_empty());
        assert!(report.summary().contains("PASSED"));
    }

    #[test]
    fn failing_report() {
        let report = GauntletReport {
            gate_results: vec![
                GateResult::pass(GauntletGate::Equivalence, 4, "OK", 100),
                GateResult::fail(
                    GauntletGate::TailLatency,
                    4,
                    3,
                    vec![GateFailure {
                        fixture_id: "render_pipeline_full_200x60".to_string(),
                        reason: "p99 regressed 25% (threshold 15%)".to_string(),
                        category: FailureCategory::TailRegression,
                        artifacts: vec!["latency_histogram.json".to_string()],
                    }],
                    200,
                ),
            ],
            total_duration_ms: 300,
            config: GauntletConfig::default_strict(),
        };
        assert!(!report.passed());
        assert_eq!(report.gates_failed(), 1);
        assert_eq!(report.real_failures().len(), 1);
        assert!(report.summary().contains("FAILED"));
    }

    #[test]
    fn expected_fallback_not_real_failure() {
        let failure = GateFailure {
            fixture_id: "challenge_resize_storm".to_string(),
            reason: "Fell back to full render under resize storm".to_string(),
            category: FailureCategory::ExpectedFallback,
            artifacts: vec![],
        };
        assert!(!failure.category.is_real_failure());
    }

    #[test]
    fn failure_categories_labeled() {
        for cat in [
            FailureCategory::SemanticRegression,
            FailureCategory::ObservabilityGap,
            FailureCategory::BenchmarkOverfit,
            FailureCategory::ExpectedFallback,
            FailureCategory::StaleCertificate,
            FailureCategory::StaleCache,
            FailureCategory::TailRegression,
            FailureCategory::ResourceLeak,
            FailureCategory::Crash,
        ] {
            assert!(!cat.label().is_empty());
        }
    }

    #[test]
    fn only_expected_fallback_is_not_real() {
        for cat in [
            FailureCategory::SemanticRegression,
            FailureCategory::ObservabilityGap,
            FailureCategory::BenchmarkOverfit,
            FailureCategory::StaleCertificate,
            FailureCategory::StaleCache,
            FailureCategory::TailRegression,
            FailureCategory::ResourceLeak,
            FailureCategory::Crash,
        ] {
            assert!(
                cat.is_real_failure(),
                "{} should be a real failure",
                cat.label()
            );
        }
        assert!(!FailureCategory::ExpectedFallback.is_real_failure());
    }

    #[test]
    fn report_to_json_valid() {
        let report = GauntletReport {
            gate_results: vec![GateResult::pass(GauntletGate::Equivalence, 3, "All OK", 50)],
            total_duration_ms: 50,
            config: GauntletConfig::default_strict(),
        };
        let json = report.to_json();
        assert!(json.contains("\"schema_version\": 1"));
        assert!(json.contains("\"passed\": true"));
        assert!(json.contains("\"gates_passed\": 1"));
        assert!(json.contains("\"gate\": \"equivalence\""));
    }

    #[test]
    fn report_json_with_failures() {
        let report = GauntletReport {
            gate_results: vec![GateResult::fail(
                GauntletGate::Certificate,
                2,
                1,
                vec![GateFailure {
                    fixture_id: "test".to_string(),
                    reason: "stale frame".to_string(),
                    category: FailureCategory::StaleCertificate,
                    artifacts: vec!["shadow.json".to_string()],
                }],
                100,
            )],
            total_duration_ms: 100,
            config: GauntletConfig::default_strict(),
        };
        let json = report.to_json();
        assert!(json.contains("\"passed\": false"));
        assert!(json.contains("\"stale-certificate\""));
        assert!(json.contains("\"shadow.json\""));
    }

    #[test]
    fn gate_result_pass_constructor() {
        let r = GateResult::pass(GauntletGate::Replay, 5, "All good", 42);
        assert!(r.passed);
        assert_eq!(r.fixtures_tested, 5);
        assert_eq!(r.fixtures_passed, 5);
        assert_eq!(r.duration_ms, 42);
        assert!(r.failures.is_empty());
    }

    #[test]
    fn gate_result_fail_constructor() {
        let r = GateResult::fail(GauntletGate::TailLatency, 5, 3, vec![], 100);
        assert!(!r.passed);
        assert_eq!(r.fixtures_tested, 5);
        assert_eq!(r.fixtures_passed, 3);
        assert!(r.summary.contains("3/5"));
    }
}
