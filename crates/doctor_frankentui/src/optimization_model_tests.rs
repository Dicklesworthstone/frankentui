//! Cross-model unit/property test-evidence harness for the adaptive-optimization
//! models (bd-3bxhj.8.12).
//!
//! Two pure, deterministic models drive how the migration program spends its
//! finite test-compute budget and how it decides a regime has shifted:
//!
//! - the VOI-driven adaptive test-budget allocator
//!   ([`crate::test_budget_allocator`]) — Beta-posterior value-of-information
//!   scoring, per-unit ranking, budget/floor/cap constraints, deterministic
//!   tie-breaks, and a round-robin baseline comparison;
//! - the BOCPD + CUSUM drift monitors ([`crate::drift_monitor`]) — baseline
//!   estimation, sustained-shift detection (CUSUM), regime-change detection
//!   (BOCPD), and false-positive calibration.
//!
//! Each module already ships its own inline unit tests. This module adds the
//! *cross-model* contract the parent bead asks for: a single, host-agnostic
//! [`OptimizationDiagnostic`] envelope that normalizes every model's salient
//! decision into one structured schema, a library of synthetic regime-shift
//! fixtures with *explicit expected outcomes*, and a deterministic
//! [`OptimizationValidationReport`] that the downstream E2E adaptive-scheduling
//! gauntlets (bd-3bxhj.8.13 / .8.21) can consume without adapter drift.
//!
//! Every diagnostic carries the exact fields the bead's acceptance criteria
//! mandate for failure logs (criterion 3):
//!
//! - `model_id` (the allocation id or monitor id),
//! - `prior_state_hash` (hash of the pre-decision model state),
//! - `posterior_state_hash` (hash of the post-decision model state),
//! - `threshold_set` (the canonical threshold descriptor that governed the
//!   decision),
//! - `detection_outcome` (what the model decided), and
//! - `replay_cmd` (a deterministic single-command replay reference).
//!
//! These six are projected verbatim by [`OptimizationDiagnostic::failure_log`]
//! into the [`OptimizationFailureLog`] record that the E2E scripts ingest.
//!
//! Beyond raw diagnostics, every fixture is paired with an
//! [`ExpectedVoiOutcome`] / [`ExpectedDriftOutcome`] oracle, and the harness
//! emits an [`OutcomeVerdict`] comparing the model's actual behavior against the
//! expectation. This is what proves the models behave correctly under
//! representative *and* adversarial regime-shift datasets (criterion 1) rather
//! than merely "runs without panicking".
//!
//! The harness is pure and owns no I/O, so the same fixture corpus always yields
//! the same `report_id`, `evidence_checksum`, model ids, and state hashes; fixed
//! inputs produce byte-identical scheduling and detection outputs (criterion 2).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::drift_monitor::{
    BocpdConfig, CusumConfig, DriftDetector, DriftKind, DriftMetricSeries, DriftMonitor,
    DriftMonitorConfig, DriftMonitorReport, MetricDirection,
};
use crate::test_budget_allocator::{
    TestBudgetAllocator, TestBudgetConfig, TestBudgetReport, TestFixtureCandidate,
};

/// Schema version for the cross-model optimization diagnostic contract.
pub const OPTIMIZATION_MODEL_SCHEMA_VERSION: &str = "optimization-model-tests-v1";

/// Lower clamp matching the optimization modules' epsilon.
const EPS: f64 = 1e-9;

// ── Model identity ───────────────────────────────────────────────────────────

/// Which optimization model produced a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizationModelClass {
    /// VOI-driven adaptive test-budget allocator (`test_budget_allocator`).
    VoiAllocator,
    /// CUSUM control-chart drift detector (`drift_monitor`).
    CusumDrift,
    /// BOCPD regime-change drift detector (`drift_monitor`).
    BocpdDrift,
}

impl OptimizationModelClass {
    /// Every model class, in stable order.
    pub const ALL: &'static [OptimizationModelClass] = &[
        OptimizationModelClass::VoiAllocator,
        OptimizationModelClass::CusumDrift,
        OptimizationModelClass::BocpdDrift,
    ];

    /// Stable lowercase identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VoiAllocator => "voi_allocator",
            Self::CusumDrift => "cusum_drift",
            Self::BocpdDrift => "bocpd_drift",
        }
    }
}

/// The normalized decision a model reached for one subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionOutcome {
    /// VOI allocator funded this fixture with at least one run.
    Scheduled,
    /// VOI allocator gave this fixture zero runs (floor/budget/cap starvation).
    Starved,
    /// Drift detector flagged a move to the *bad* side of the metric direction.
    Regression,
    /// Drift detector flagged a move to the *good* side of the metric direction.
    Improvement,
    /// Drift detector flagged a regime change inside the baseline dead-band.
    Neutral,
}

impl DetectionOutcome {
    /// Stable lowercase identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Starved => "starved",
            Self::Regression => "regression",
            Self::Improvement => "improvement",
            Self::Neutral => "neutral",
        }
    }
}

impl From<DriftKind> for DetectionOutcome {
    fn from(kind: DriftKind) -> Self {
        match kind {
            DriftKind::Regression => Self::Regression,
            DriftKind::Improvement => Self::Improvement,
            DriftKind::Neutral => Self::Neutral,
        }
    }
}

// ── Unified diagnostic envelope ──────────────────────────────────────────────

/// A single normalized optimization-model diagnostic.
///
/// This is the structured schema contract consumed by the downstream E2E
/// adaptive-scheduling gauntlets. Every field is always populated, so failure
/// logs are forensically rich regardless of which model produced them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptimizationDiagnostic {
    /// Which model produced this diagnostic.
    pub model_class: OptimizationModelClass,
    /// The deterministic model identity (allocation id / monitor id).
    pub model_id: String,
    /// The universal subject handle (fixture id / metric id).
    pub subject_id: String,
    /// SHA-256 of the model state *before* this decision.
    pub prior_state_hash: String,
    /// SHA-256 of the model state *after* this decision.
    pub posterior_state_hash: String,
    /// Canonical descriptor of the thresholds that governed this decision.
    pub threshold_set: String,
    /// What the model decided for this subject.
    pub detection_outcome: DetectionOutcome,
    /// Observation index for drift events; `-1` for budget decisions.
    pub observation_index: i64,
    /// Primary scalar statistic (initial VOI / detector statistic).
    pub statistic: f64,
    /// Decision confidence in `[0, 1]`.
    pub confidence: f64,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic single-command replay reference.
    pub replay_cmd: String,
}

impl OptimizationDiagnostic {
    /// Whether every required failure-log field is populated and non-empty.
    ///
    /// Mirrors the bead's acceptance criterion that failure logs always emit
    /// `model_id`, `prior_state_hash`, `posterior_state_hash`, `threshold_set`,
    /// `detection_outcome`, and `replay_cmd`.
    #[must_use]
    pub fn has_required_fields(&self) -> bool {
        !self.model_id.is_empty()
            && !self.subject_id.is_empty()
            && !self.prior_state_hash.is_empty()
            && !self.posterior_state_hash.is_empty()
            && !self.threshold_set.is_empty()
            && !self.detail.is_empty()
            && !self.replay_cmd.is_empty()
    }

    /// Whether this diagnostic records a *funded* / *fired* decision (i.e. a
    /// state-changing event) rather than starvation.
    #[must_use]
    pub fn is_active_decision(&self) -> bool {
        self.detection_outcome != DetectionOutcome::Starved
    }

    /// Whether this diagnostic represents a regression detection.
    #[must_use]
    pub fn is_regression(&self) -> bool {
        self.detection_outcome == DetectionOutcome::Regression
    }

    /// Project this diagnostic into the bead-mandated failure-log record.
    #[must_use]
    pub fn failure_log(&self) -> OptimizationFailureLog {
        OptimizationFailureLog {
            model_id: self.model_id.clone(),
            prior_state_hash: self.prior_state_hash.clone(),
            posterior_state_hash: self.posterior_state_hash.clone(),
            threshold_set: self.threshold_set.clone(),
            detection_outcome: self.detection_outcome.as_str().to_string(),
            replay_cmd: self.replay_cmd.clone(),
        }
    }
}

/// The exact failure-log schema mandated by acceptance criterion 3, consumed by
/// the `.8.13` / `.8.21` E2E adaptive-scheduling scripts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptimizationFailureLog {
    pub model_id: String,
    pub prior_state_hash: String,
    pub posterior_state_hash: String,
    pub threshold_set: String,
    pub detection_outcome: String,
    pub replay_cmd: String,
}

// ── Expected-outcome oracles ─────────────────────────────────────────────────

/// The expected scheduling outcome for one VOI fixture.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedVoiOutcome {
    /// The fixture that should receive the most runs (ties broken by id), if any.
    pub top_fixture_id: Option<String>,
    /// Fixtures that must each receive at least one run.
    pub scheduled_fixtures: Vec<String>,
    /// Fixtures that must receive zero runs.
    pub starved_fixtures: Vec<String>,
    /// Whether the VOI strategy must beat or match round-robin confidence/unit.
    pub expect_beats_round_robin: bool,
}

/// The expected detection outcome for one drift fixture.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedDriftOutcome {
    /// Metrics that must register at least one regression.
    pub regressed_metrics: Vec<String>,
    /// Metrics that must raise zero drift events.
    pub clean_metrics: Vec<String>,
    /// Whether at least one CUSUM alarm must fire.
    pub expect_cusum: bool,
    /// Whether at least one BOCPD changepoint must fire.
    pub expect_bocpd: bool,
    /// Whether at least one improvement event must fire.
    pub expect_improvement: bool,
    /// Inclusive observation-index window a BOCPD changepoint must land in.
    pub changepoint_index_range: Option<(u32, u32)>,
}

/// The verdict comparing one fixture's actual model behavior to its expectation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeVerdict {
    /// The fixture's label.
    pub fixture_label: String,
    /// Which model class the fixture exercises.
    pub model_class_label: String,
    /// The deterministic model id that produced the outcome.
    pub model_id: String,
    /// Human-readable description of what was expected.
    pub expectation: String,
    /// Whether every expectation was satisfied.
    pub matches_expected: bool,
    /// Specific expectation violations (empty when `matches_expected`).
    pub mismatches: Vec<String>,
}

// ── Fixtures ─────────────────────────────────────────────────────────────────

/// One VOI-allocator fixture paired with its expected scheduling outcome.
#[derive(Debug, Clone)]
pub struct VoiFixture {
    /// Scenario label (must be unique within a corpus).
    pub label: String,
    /// Candidate fixtures competing for budget.
    pub candidates: Vec<TestFixtureCandidate>,
    /// Allocator configuration.
    pub config: TestBudgetConfig,
    /// Expected scheduling outcome oracle.
    pub expected: ExpectedVoiOutcome,
}

/// One drift-monitor fixture paired with its expected detection outcome.
#[derive(Debug, Clone)]
pub struct DriftFixture {
    /// Scenario label (must be unique within a corpus).
    pub label: String,
    /// Metric series to analyze.
    pub series: Vec<DriftMetricSeries>,
    /// Monitor configuration.
    pub config: DriftMonitorConfig,
    /// Expected detection outcome oracle.
    pub expected: ExpectedDriftOutcome,
}

/// A labelled corpus of VOI and drift fixtures.
#[derive(Debug, Clone)]
pub struct OptimizationFixtureCorpus {
    /// Scenario label (becomes the report's `scenario_label`).
    pub label: String,
    /// VOI-allocator fixtures.
    pub voi_fixtures: Vec<VoiFixture>,
    /// Drift-monitor fixtures.
    pub drift_fixtures: Vec<DriftFixture>,
}

// ── Per-fixture evaluation results ───────────────────────────────────────────

/// The result of evaluating one VOI fixture.
#[derive(Debug, Clone)]
pub struct VoiFixtureEvaluation {
    /// Normalized diagnostics (one per candidate).
    pub diagnostics: Vec<OptimizationDiagnostic>,
    /// Expected-vs-actual verdict.
    pub verdict: OutcomeVerdict,
    /// The underlying allocator report (full forensic detail).
    pub report: TestBudgetReport,
}

/// The result of evaluating one drift fixture.
#[derive(Debug, Clone)]
pub struct DriftFixtureEvaluation {
    /// Normalized diagnostics (one per detected event).
    pub diagnostics: Vec<OptimizationDiagnostic>,
    /// Expected-vs-actual verdict.
    pub verdict: OutcomeVerdict,
    /// The underlying monitor report (full forensic detail).
    pub report: DriftMonitorReport,
}

// ── Summary + artifact + report ──────────────────────────────────────────────

/// Aggregate counts over the unified diagnostics and verdicts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptimizationValidationSummary {
    /// Total diagnostics emitted.
    pub total_diagnostics: usize,
    /// Diagnostics from the VOI allocator.
    pub voi_diagnostics: usize,
    /// Diagnostics from the CUSUM detector.
    pub cusum_diagnostics: usize,
    /// Diagnostics from the BOCPD detector.
    pub bocpd_diagnostics: usize,
    /// VOI fixtures funded at least one run.
    pub scheduled_count: usize,
    /// VOI fixtures starved of budget.
    pub starved_count: usize,
    /// Drift regressions detected.
    pub regression_count: usize,
    /// Drift improvements detected.
    pub improvement_count: usize,
    /// Total fixture verdicts.
    pub total_verdicts: usize,
    /// Verdicts whose expectation was met.
    pub passing_verdicts: usize,
    /// Whether every fixture's expectation was met.
    pub all_expectations_met: bool,
}

/// Deterministic JSON-stats artifact (content + checksum).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptimizationJsonStatsArtifact {
    /// Suggested relative output path.
    pub path: String,
    /// SHA-256 of `content`.
    pub sha256: String,
    /// Serialized JSON content.
    pub content: String,
}

/// The full cross-model optimization validation report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptimizationValidationReport {
    /// Schema version constant.
    pub schema_version: String,
    /// Deterministic report identifier (derived from the evidence).
    pub report_id: String,
    /// Scenario label.
    pub scenario_label: String,
    /// The full diagnostic ledger.
    pub diagnostics: Vec<OptimizationDiagnostic>,
    /// The per-fixture verdicts.
    pub verdicts: Vec<OutcomeVerdict>,
    /// Aggregate summary.
    pub summary: OptimizationValidationSummary,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: OptimizationJsonStatsArtifact,
    /// Replay command for the whole report.
    pub replay_command: String,
    /// SHA-256 fingerprint of the diagnostics + verdicts (output checksum).
    pub evidence_checksum: String,
}

impl OptimizationValidationReport {
    /// All diagnostics from a given model class, in ledger order.
    #[must_use]
    pub fn diagnostics_for(&self, class: OptimizationModelClass) -> Vec<&OptimizationDiagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.model_class == class)
            .collect()
    }

    /// All failing verdicts, in ledger order.
    #[must_use]
    pub fn failing_verdicts(&self) -> Vec<&OutcomeVerdict> {
        self.verdicts
            .iter()
            .filter(|v| !v.matches_expected)
            .collect()
    }

    /// Every diagnostic projected into the mandated failure-log schema.
    #[must_use]
    pub fn failure_logs(&self) -> Vec<OptimizationFailureLog> {
        self.diagnostics
            .iter()
            .map(OptimizationDiagnostic::failure_log)
            .collect()
    }
}

// ── Evaluation ───────────────────────────────────────────────────────────────

/// Run the VOI allocator over `fixture` and normalize its decision into
/// diagnostics plus an expected-vs-actual verdict.
#[must_use]
pub fn evaluate_voi_fixture(fixture: &VoiFixture) -> VoiFixtureEvaluation {
    let report = TestBudgetAllocator::new(fixture.config).allocate(fixture.candidates.clone());
    let threshold_set = voi_threshold_set(&fixture.config);

    let candidate_by_id: BTreeMap<&str, &TestFixtureCandidate> = fixture
        .candidates
        .iter()
        .map(|candidate| (candidate.fixture_id.as_str(), candidate))
        .collect();

    let mut diagnostics = Vec::new();
    for allocation in &report.allocations {
        let prior = candidate_by_id
            .get(allocation.fixture_id.as_str())
            .map(|candidate| {
                let posterior = candidate.posterior();
                (posterior.alpha, posterior.beta)
            })
            .unwrap_or((allocation.posterior_alpha, allocation.posterior_beta));
        let prior_state_hash = stable_hash(&BetaState {
            alpha: prior.0,
            beta: prior.1,
        });
        let posterior_state_hash = stable_hash(&BetaState {
            alpha: allocation.posterior_alpha,
            beta: allocation.posterior_beta,
        });
        let outcome = if allocation.runs_allocated > 0 {
            DetectionOutcome::Scheduled
        } else {
            DetectionOutcome::Starved
        };
        let confidence = if allocation.voi_initial > EPS {
            ((allocation.voi_initial - allocation.voi_final) / allocation.voi_initial)
                .clamp(0.0, 1.0)
        } else {
            0.0
        };
        diagnostics.push(OptimizationDiagnostic {
            model_class: OptimizationModelClass::VoiAllocator,
            model_id: report.allocation_id.clone(),
            subject_id: allocation.fixture_id.clone(),
            prior_state_hash,
            posterior_state_hash,
            threshold_set: threshold_set.clone(),
            detection_outcome: outcome,
            observation_index: -1,
            statistic: allocation.voi_initial,
            confidence,
            detail: format!(
                "{} runs={} compute={:.4} voi {:.6}->{:.6}",
                allocation.fixture_id,
                allocation.runs_allocated,
                allocation.compute_units,
                allocation.voi_initial,
                allocation.voi_final
            ),
            replay_cmd: report.replay_command.clone(),
        });
    }
    sort_diagnostics(&mut diagnostics);

    let verdict = voi_verdict(fixture, &report);
    VoiFixtureEvaluation {
        diagnostics,
        verdict,
        report,
    }
}

/// Run the drift monitor over `fixture` and normalize its events into
/// diagnostics plus an expected-vs-actual verdict.
#[must_use]
pub fn evaluate_drift_fixture(fixture: &DriftFixture) -> DriftFixtureEvaluation {
    let report = DriftMonitor::new(fixture.config).analyze(fixture.series.clone());

    let mut diagnostics = Vec::new();
    for event in &report.events {
        let model_class = match event.detector {
            DriftDetector::BocpdChangepoint => OptimizationModelClass::BocpdDrift,
            DriftDetector::CusumUpward | DriftDetector::CusumDownward => {
                OptimizationModelClass::CusumDrift
            }
        };
        let threshold_set = if model_class == OptimizationModelClass::BocpdDrift {
            bocpd_threshold_set(&fixture.config)
        } else {
            cusum_threshold_set(&fixture.config)
        };
        let prior_state_hash = stable_hash(&DriftPriorState {
            metric_id: &event.metric_id,
            baseline_mean: event.baseline_mean,
            baseline_std: event.baseline_std,
            threshold_set: &threshold_set,
        });
        let posterior_state_hash = stable_hash(&DriftPosteriorState {
            detector: event.detector.as_str(),
            kind: event.kind.as_str(),
            value: event.value,
            statistic: event.statistic,
            confidence: event.confidence,
            observation_index: event.observation_index,
        });
        diagnostics.push(OptimizationDiagnostic {
            model_class,
            model_id: report.monitor_id.clone(),
            subject_id: event.metric_id.clone(),
            prior_state_hash,
            posterior_state_hash,
            threshold_set,
            detection_outcome: DetectionOutcome::from(event.kind),
            observation_index: i64::from(event.observation_index),
            statistic: event.statistic,
            confidence: event.confidence,
            detail: event.description.clone(),
            replay_cmd: report.replay_command.clone(),
        });
    }
    sort_diagnostics(&mut diagnostics);

    let verdict = drift_verdict(fixture, &report);
    DriftFixtureEvaluation {
        diagnostics,
        verdict,
        report,
    }
}

/// Run every fixture in `corpus` and assemble a deterministic, normalized report.
#[must_use]
pub fn run_optimization_validation(
    corpus: &OptimizationFixtureCorpus,
) -> OptimizationValidationReport {
    let mut voi_fixtures = corpus.voi_fixtures.clone();
    voi_fixtures.sort_by(|left, right| left.label.cmp(&right.label));
    let mut drift_fixtures = corpus.drift_fixtures.clone();
    drift_fixtures.sort_by(|left, right| left.label.cmp(&right.label));

    let mut diagnostics = Vec::new();
    let mut verdicts = Vec::new();
    for fixture in &voi_fixtures {
        let evaluation = evaluate_voi_fixture(fixture);
        diagnostics.extend(evaluation.diagnostics);
        verdicts.push(evaluation.verdict);
    }
    for fixture in &drift_fixtures {
        let evaluation = evaluate_drift_fixture(fixture);
        diagnostics.extend(evaluation.diagnostics);
        verdicts.push(evaluation.verdict);
    }
    sort_diagnostics(&mut diagnostics);
    verdicts.sort_by(|left, right| {
        left.model_class_label
            .cmp(&right.model_class_label)
            .then_with(|| left.fixture_label.cmp(&right.fixture_label))
    });

    let summary = summarize(&diagnostics, &verdicts);
    let evidence_checksum = stable_hash(&EvidenceInput {
        diagnostics: &diagnostics,
        verdicts: &verdicts,
    });
    let report_id = format!(
        "optimization-model-tests-{}",
        short_hash(&stable_hash(&ReportIdInput {
            schema_version: OPTIMIZATION_MODEL_SCHEMA_VERSION,
            scenario_label: &corpus.label,
            evidence_checksum: &evidence_checksum,
        })),
    );
    let replay_command = format!(
        "doctor_frankentui optimization-validate --report-id {report_id} --scenario {}",
        corpus.label
    );
    let exported_json_stats = export_json_stats(
        &report_id,
        &corpus.label,
        &summary,
        &diagnostics,
        &verdicts,
        &evidence_checksum,
    );

    OptimizationValidationReport {
        schema_version: OPTIMIZATION_MODEL_SCHEMA_VERSION.to_string(),
        report_id,
        scenario_label: corpus.label.clone(),
        diagnostics,
        verdicts,
        summary,
        exported_json_stats,
        replay_command,
        evidence_checksum,
    }
}

fn voi_verdict(fixture: &VoiFixture, report: &TestBudgetReport) -> OutcomeVerdict {
    let expected = &fixture.expected;
    let mut mismatches = Vec::new();

    // Top fixture: most runs, ties broken by lexically-smaller id (deterministic).
    let actual_top = report
        .allocations
        .iter()
        .filter(|allocation| allocation.runs_allocated > 0)
        .max_by(|left, right| {
            left.runs_allocated
                .cmp(&right.runs_allocated)
                .then_with(|| right.fixture_id.cmp(&left.fixture_id))
        })
        .map(|allocation| allocation.fixture_id.clone());
    if let Some(expected_top) = &expected.top_fixture_id
        && actual_top.as_deref() != Some(expected_top.as_str())
    {
        mismatches.push(format!(
            "expected top fixture {expected_top:?}, got {actual_top:?}"
        ));
    }

    let scheduled: BTreeSet<&str> = report
        .allocations
        .iter()
        .filter(|allocation| allocation.runs_allocated > 0)
        .map(|allocation| allocation.fixture_id.as_str())
        .collect();
    for fixture_id in &expected.scheduled_fixtures {
        if !scheduled.contains(fixture_id.as_str()) {
            mismatches.push(format!(
                "expected {fixture_id} scheduled, but it was starved"
            ));
        }
    }
    for fixture_id in &expected.starved_fixtures {
        if scheduled.contains(fixture_id.as_str()) {
            mismatches.push(format!(
                "expected {fixture_id} starved, but it was scheduled"
            ));
        }
    }
    if expected.expect_beats_round_robin && !report.baseline_comparison.voi_is_at_least_baseline {
        mismatches
            .push("expected VOI to beat/match round-robin per unit, but it did not".to_string());
    }

    OutcomeVerdict {
        fixture_label: fixture.label.clone(),
        model_class_label: OptimizationModelClass::VoiAllocator.as_str().to_string(),
        model_id: report.allocation_id.clone(),
        expectation: describe_voi_expectation(expected),
        matches_expected: mismatches.is_empty(),
        mismatches,
    }
}

fn drift_verdict(fixture: &DriftFixture, report: &DriftMonitorReport) -> OutcomeVerdict {
    let expected = &fixture.expected;
    let mut mismatches = Vec::new();

    let regressed: BTreeSet<&str> = report
        .triage
        .metrics_with_regression
        .iter()
        .map(String::as_str)
        .collect();
    for metric_id in &expected.regressed_metrics {
        if !regressed.contains(metric_id.as_str()) {
            mismatches.push(format!("expected regression on {metric_id}, none detected"));
        }
    }
    for metric_id in &expected.clean_metrics {
        let events = report
            .events
            .iter()
            .filter(|event| &event.metric_id == metric_id)
            .count();
        if events > 0 {
            mismatches.push(format!(
                "expected {metric_id} clean, but {events} drift event(s) fired"
            ));
        }
    }

    let has_cusum = report.events.iter().any(|event| {
        matches!(
            event.detector,
            DriftDetector::CusumUpward | DriftDetector::CusumDownward
        )
    });
    let has_bocpd = report
        .events
        .iter()
        .any(|event| event.detector == DriftDetector::BocpdChangepoint);
    let has_improvement = report
        .events
        .iter()
        .any(|event| event.kind == DriftKind::Improvement);
    if expected.expect_cusum && !has_cusum {
        mismatches.push("expected a CUSUM alarm, none fired".to_string());
    }
    if expected.expect_bocpd && !has_bocpd {
        mismatches.push("expected a BOCPD changepoint, none fired".to_string());
    }
    if expected.expect_improvement && !has_improvement {
        mismatches.push("expected an improvement event, none fired".to_string());
    }
    if let Some((low, high)) = expected.changepoint_index_range {
        let in_range = report.events.iter().any(|event| {
            event.detector == DriftDetector::BocpdChangepoint
                && event.observation_index >= low
                && event.observation_index <= high
        });
        if !in_range {
            mismatches.push(format!(
                "expected a BOCPD changepoint in [{low},{high}], none landed there"
            ));
        }
    }

    OutcomeVerdict {
        fixture_label: fixture.label.clone(),
        model_class_label: "drift_monitor".to_string(),
        model_id: report.monitor_id.clone(),
        expectation: describe_drift_expectation(expected),
        matches_expected: mismatches.is_empty(),
        mismatches,
    }
}

fn summarize(
    diagnostics: &[OptimizationDiagnostic],
    verdicts: &[OutcomeVerdict],
) -> OptimizationValidationSummary {
    let mut voi = 0;
    let mut cusum = 0;
    let mut bocpd = 0;
    let mut scheduled = 0;
    let mut starved = 0;
    let mut regression = 0;
    let mut improvement = 0;
    for diagnostic in diagnostics {
        match diagnostic.model_class {
            OptimizationModelClass::VoiAllocator => voi += 1,
            OptimizationModelClass::CusumDrift => cusum += 1,
            OptimizationModelClass::BocpdDrift => bocpd += 1,
        }
        match diagnostic.detection_outcome {
            DetectionOutcome::Scheduled => scheduled += 1,
            DetectionOutcome::Starved => starved += 1,
            DetectionOutcome::Regression => regression += 1,
            DetectionOutcome::Improvement => improvement += 1,
            DetectionOutcome::Neutral => {}
        }
    }
    let passing = verdicts.iter().filter(|v| v.matches_expected).count();
    OptimizationValidationSummary {
        total_diagnostics: diagnostics.len(),
        voi_diagnostics: voi,
        cusum_diagnostics: cusum,
        bocpd_diagnostics: bocpd,
        scheduled_count: scheduled,
        starved_count: starved,
        regression_count: regression,
        improvement_count: improvement,
        total_verdicts: verdicts.len(),
        passing_verdicts: passing,
        all_expectations_met: passing == verdicts.len(),
    }
}

fn export_json_stats(
    report_id: &str,
    scenario_label: &str,
    summary: &OptimizationValidationSummary,
    diagnostics: &[OptimizationDiagnostic],
    verdicts: &[OutcomeVerdict],
    evidence_checksum: &str,
) -> OptimizationJsonStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        scenario_label: &'a str,
        summary: &'a OptimizationValidationSummary,
        evidence_checksum: &'a str,
        diagnostics: &'a [OptimizationDiagnostic],
        verdicts: &'a [OutcomeVerdict],
    }
    let payload = Export {
        schema_version: OPTIMIZATION_MODEL_SCHEMA_VERSION,
        report_id,
        scenario_label,
        summary,
        evidence_checksum,
        diagnostics,
        verdicts,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    OptimizationJsonStatsArtifact {
        path: format!("{report_id}/optimization_model_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

// ── Fixture library: VOI allocator ───────────────────────────────────────────

/// Representative fixture: a high-uncertainty, cheap, important fixture must win
/// the most budget over a nearly-certain (low-VOI) fixture and an expensive one.
#[must_use]
pub fn voi_priority_fixture() -> VoiFixture {
    let candidates = vec![
        TestFixtureCandidate::new("fixture-flaky", "certification", 1.0)
            .with_prior(1.0, 1.0)
            .with_value_scale(2.0),
        TestFixtureCandidate::new("fixture-stable", "certification", 1.0)
            .with_prior(1.0, 1.0)
            .with_observations(40.0, 0.0),
        TestFixtureCandidate::new("fixture-expensive", "regression", 8.0)
            .with_prior(1.0, 1.0)
            .with_value_scale(1.0),
    ];
    VoiFixture {
        label: "voi-priority".to_string(),
        candidates,
        config: TestBudgetConfig::new(20.0),
        expected: ExpectedVoiOutcome {
            top_fixture_id: Some("fixture-flaky".to_string()),
            scheduled_fixtures: vec!["fixture-flaky".to_string()],
            starved_fixtures: Vec::new(),
            expect_beats_round_robin: true,
        },
    }
}

/// Adversarial fixture: a very high VOI floor starves every candidate, proving
/// the floor constraint blocks low-value runs deterministically.
#[must_use]
pub fn voi_starvation_fixture() -> VoiFixture {
    let candidates = vec![
        TestFixtureCandidate::new("starve-a", "certification", 1.0).with_prior(1.0, 1.0),
        TestFixtureCandidate::new("starve-b", "regression", 1.0).with_prior(1.0, 1.0),
    ];
    VoiFixture {
        label: "voi-starvation".to_string(),
        candidates,
        config: TestBudgetConfig::new(20.0).with_voi_floor(1.0),
        expected: ExpectedVoiOutcome {
            top_fixture_id: None,
            scheduled_fixtures: Vec::new(),
            starved_fixtures: vec!["starve-a".to_string(), "starve-b".to_string()],
            expect_beats_round_robin: false,
        },
    }
}

/// Adversarial fixture: three identical candidates exercise the deterministic
/// tie-break (id ascending) and even budget spread. With nine units and unit
/// cost, each gets exactly three runs, so the lexically-smallest id is "top".
#[must_use]
pub fn voi_tie_break_fixture() -> VoiFixture {
    let candidates = vec![
        TestFixtureCandidate::new("tie-a", "certification", 1.0).with_prior(1.0, 1.0),
        TestFixtureCandidate::new("tie-b", "certification", 1.0).with_prior(1.0, 1.0),
        TestFixtureCandidate::new("tie-c", "certification", 1.0).with_prior(1.0, 1.0),
    ];
    VoiFixture {
        label: "voi-tie-break".to_string(),
        candidates,
        config: TestBudgetConfig::new(9.0),
        expected: ExpectedVoiOutcome {
            top_fixture_id: Some("tie-a".to_string()),
            scheduled_fixtures: vec![
                "tie-a".to_string(),
                "tie-b".to_string(),
                "tie-c".to_string(),
            ],
            starved_fixtures: Vec::new(),
            expect_beats_round_robin: true,
        },
    }
}

/// Adversarial fixture: a tiny budget that only affords the cheap candidate, so
/// the expensive candidate is starved purely by the budget constraint.
#[must_use]
pub fn voi_budget_constrained_fixture() -> VoiFixture {
    let candidates = vec![
        TestFixtureCandidate::new("cheap", "certification", 1.0).with_prior(1.0, 1.0),
        TestFixtureCandidate::new("pricey", "regression", 50.0).with_prior(1.0, 1.0),
    ];
    VoiFixture {
        label: "voi-budget-constrained".to_string(),
        candidates,
        config: TestBudgetConfig::new(2.0),
        expected: ExpectedVoiOutcome {
            top_fixture_id: Some("cheap".to_string()),
            scheduled_fixtures: vec!["cheap".to_string()],
            starved_fixtures: vec!["pricey".to_string()],
            expect_beats_round_robin: false,
        },
    }
}

// ── Fixture library: drift monitor ───────────────────────────────────────────

fn monitor_config() -> DriftMonitorConfig {
    DriftMonitorConfig::default().with_baseline_window(8)
}

/// Representative regression: a success rate that drops sharply partway through.
#[must_use]
pub fn drift_dropping_success_fixture() -> DriftFixture {
    let mut values = vec![0.90, 0.91, 0.89, 0.90, 0.92, 0.88, 0.91, 0.90, 0.89, 0.91];
    values.extend([0.60, 0.58, 0.61, 0.59, 0.60, 0.62, 0.59, 0.60]);
    let series = DriftMetricSeries::new("migration_success_rate", MetricDirection::HigherIsBetter)
        .with_observations(values);
    DriftFixture {
        label: "drift-dropping-success".to_string(),
        series: vec![series],
        config: monitor_config(),
        expected: ExpectedDriftOutcome {
            regressed_metrics: vec!["migration_success_rate".to_string()],
            clean_metrics: Vec::new(),
            expect_cusum: true,
            expect_bocpd: true,
            expect_improvement: false,
            changepoint_index_range: Some((9, 13)),
        },
    }
}

/// Representative regression: a latency that rises sharply (lower-is-better).
#[must_use]
pub fn drift_rising_latency_fixture() -> DriftFixture {
    let mut values = vec![10.0, 10.2, 9.8, 10.1, 9.9, 10.0, 10.3, 9.7, 10.0, 10.1];
    values.extend([18.0, 18.5, 17.9, 18.2, 18.1, 18.0]);
    let series = DriftMetricSeries::new("p99_latency_ms", MetricDirection::LowerIsBetter)
        .with_observations(values);
    DriftFixture {
        label: "drift-rising-latency".to_string(),
        series: vec![series],
        config: monitor_config(),
        expected: ExpectedDriftOutcome {
            regressed_metrics: vec!["p99_latency_ms".to_string()],
            clean_metrics: Vec::new(),
            expect_cusum: true,
            expect_bocpd: false,
            expect_improvement: false,
            changepoint_index_range: None,
        },
    }
}

/// Representative improvement: a success rate that rises sharply (good drift).
#[must_use]
pub fn drift_improving_success_fixture() -> DriftFixture {
    let mut values = vec![0.60, 0.61, 0.59, 0.60, 0.62, 0.58, 0.61, 0.60, 0.59, 0.61];
    values.extend([0.90, 0.88, 0.91, 0.89, 0.90, 0.92, 0.89, 0.90]);
    let series = DriftMetricSeries::new("recovery_success_rate", MetricDirection::HigherIsBetter)
        .with_observations(values);
    DriftFixture {
        label: "drift-improving-success".to_string(),
        series: vec![series],
        config: monitor_config(),
        expected: ExpectedDriftOutcome {
            regressed_metrics: Vec::new(),
            clean_metrics: Vec::new(),
            expect_cusum: true,
            expect_bocpd: false,
            expect_improvement: true,
            changepoint_index_range: None,
        },
    }
}

/// Adversarial: a noisy-but-stable series that must raise zero drift events.
#[must_use]
pub fn drift_stable_fixture() -> DriftFixture {
    let values = vec![
        0.90, 0.91, 0.89, 0.90, 0.92, 0.88, 0.91, 0.90, 0.89, 0.91, 0.90, 0.92, 0.89, 0.90, 0.91,
        0.90,
    ];
    let series = DriftMetricSeries::new("stable_success_rate", MetricDirection::HigherIsBetter)
        .with_observations(values);
    DriftFixture {
        label: "drift-stable".to_string(),
        series: vec![series],
        config: monitor_config(),
        expected: ExpectedDriftOutcome {
            regressed_metrics: Vec::new(),
            clean_metrics: vec!["stable_success_rate".to_string()],
            expect_cusum: false,
            expect_bocpd: false,
            expect_improvement: false,
            changepoint_index_range: None,
        },
    }
}

/// Adversarial: a perfectly flat (zero-variance) series exercises the floored
/// baseline sigma and must neither panic nor raise spurious events.
#[must_use]
pub fn drift_degenerate_fixture() -> DriftFixture {
    let series = DriftMetricSeries::new("flat_metric", MetricDirection::HigherIsBetter)
        .with_observations(vec![0.5; 14]);
    DriftFixture {
        label: "drift-degenerate".to_string(),
        series: vec![series],
        config: monitor_config(),
        expected: ExpectedDriftOutcome {
            regressed_metrics: Vec::new(),
            clean_metrics: vec!["flat_metric".to_string()],
            expect_cusum: false,
            expect_bocpd: false,
            expect_improvement: false,
            changepoint_index_range: None,
        },
    }
}

/// Adversarial: two successive downward level shifts; both detectors must
/// re-acquire between shifts and flag each one (no permanent stuck state).
#[must_use]
pub fn drift_two_step_fixture() -> DriftFixture {
    let mut values = vec![0.90, 0.91, 0.89, 0.90, 0.91, 0.89, 0.90, 0.91];
    values.extend([0.60; 10]);
    values.extend([0.30; 10]);
    let series = DriftMetricSeries::new("two_step", MetricDirection::HigherIsBetter)
        .with_observations(values);
    DriftFixture {
        label: "drift-two-step".to_string(),
        series: vec![series],
        config: monitor_config(),
        expected: ExpectedDriftOutcome {
            regressed_metrics: vec!["two_step".to_string()],
            clean_metrics: Vec::new(),
            expect_cusum: true,
            expect_bocpd: true,
            expect_improvement: false,
            changepoint_index_range: None,
        },
    }
}

// ── Corpus builders ──────────────────────────────────────────────────────────

/// A clean corpus: well-behaved VOI scheduling plus stable/degenerate drift.
/// Every model behaves as expected and no problematic drift is detected.
#[must_use]
pub fn green_corpus() -> OptimizationFixtureCorpus {
    OptimizationFixtureCorpus {
        label: "green".to_string(),
        voi_fixtures: vec![voi_priority_fixture(), voi_tie_break_fixture()],
        drift_fixtures: vec![drift_stable_fixture(), drift_degenerate_fixture()],
    }
}

/// The comprehensive regime-shift corpus: representative + adversarial fixtures
/// across both models, every one paired with an explicit expected outcome.
#[must_use]
pub fn regime_shift_corpus() -> OptimizationFixtureCorpus {
    OptimizationFixtureCorpus {
        label: "regime-shift".to_string(),
        voi_fixtures: vec![
            voi_priority_fixture(),
            voi_starvation_fixture(),
            voi_tie_break_fixture(),
            voi_budget_constrained_fixture(),
        ],
        drift_fixtures: vec![
            drift_dropping_success_fixture(),
            drift_rising_latency_fixture(),
            drift_improving_success_fixture(),
            drift_stable_fixture(),
            drift_degenerate_fixture(),
            drift_two_step_fixture(),
        ],
    }
}

// ── Description helpers ──────────────────────────────────────────────────────

fn describe_voi_expectation(expected: &ExpectedVoiOutcome) -> String {
    let top = expected
        .top_fixture_id
        .clone()
        .unwrap_or_else(|| "none".to_string());
    format!(
        "top={top}; scheduled={:?}; starved={:?}; beats_round_robin={}",
        expected.scheduled_fixtures, expected.starved_fixtures, expected.expect_beats_round_robin
    )
}

fn describe_drift_expectation(expected: &ExpectedDriftOutcome) -> String {
    format!(
        "regressed={:?}; clean={:?}; cusum={}; bocpd={}; improvement={}; cp_range={:?}",
        expected.regressed_metrics,
        expected.clean_metrics,
        expected.expect_cusum,
        expected.expect_bocpd,
        expected.expect_improvement,
        expected.changepoint_index_range
    )
}

// ── Threshold descriptors ────────────────────────────────────────────────────

fn voi_threshold_set(config: &TestBudgetConfig) -> String {
    format!(
        "voi_floor={:.6};max_runs_per_fixture={};total_budget_units={:.6}",
        config.voi_floor, config.max_runs_per_fixture, config.total_budget_units
    )
}

fn cusum_threshold_set(config: &DriftMonitorConfig) -> String {
    let cusum: CusumConfig = config.cusum;
    format!(
        "cusum_k={:.6};cusum_h={:.6};baseline_window={}",
        cusum.allowance_k, cusum.threshold_h, config.baseline_window
    )
}

fn bocpd_threshold_set(config: &DriftMonitorConfig) -> String {
    let bocpd: BocpdConfig = config.bocpd;
    format!(
        "bocpd_hazard_lambda={:.6};bocpd_changepoint_threshold={:.6};bocpd_recent_window={};baseline_window={}",
        bocpd.hazard_lambda,
        bocpd.changepoint_threshold,
        bocpd.recent_window,
        config.baseline_window
    )
}

// ── State-hash payloads ──────────────────────────────────────────────────────

#[derive(Serialize)]
struct BetaState {
    alpha: f64,
    beta: f64,
}

#[derive(Serialize)]
struct DriftPriorState<'a> {
    metric_id: &'a str,
    baseline_mean: f64,
    baseline_std: f64,
    threshold_set: &'a str,
}

#[derive(Serialize)]
struct DriftPosteriorState<'a> {
    detector: &'a str,
    kind: &'a str,
    value: f64,
    statistic: f64,
    confidence: f64,
    observation_index: u32,
}

#[derive(Serialize)]
struct ReportIdInput<'a> {
    schema_version: &'a str,
    scenario_label: &'a str,
    evidence_checksum: &'a str,
}

#[derive(Serialize)]
struct EvidenceInput<'a> {
    diagnostics: &'a [OptimizationDiagnostic],
    verdicts: &'a [OutcomeVerdict],
}

// ── Ordering + hashing helpers (mirrors the crate's deterministic-stack idiom) ─

fn sort_diagnostics(diagnostics: &mut [OptimizationDiagnostic]) {
    diagnostics.sort_by(|left, right| {
        left.model_class
            .as_str()
            .cmp(right.model_class.as_str())
            .then_with(|| left.model_id.cmp(&right.model_id))
            .then_with(|| left.subject_id.cmp(&right.subject_id))
            .then_with(|| left.observation_index.cmp(&right.observation_index))
            .then_with(|| {
                left.detection_outcome
                    .as_str()
                    .cmp(right.detection_outcome.as_str())
            })
            .then_with(|| left.posterior_state_hash.cmp(&right.posterior_state_hash))
    });
}

fn stable_hash<T: Serialize + ?Sized>(value: &T) -> String {
    let mut hasher = Sha256::new();
    match serde_json::to_vec(value) {
        Ok(bytes) => hasher.update(bytes),
        Err(error) => hasher.update(error.to_string().as_bytes()),
    }
    crate::util::hex_encode(&hasher.finalize())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    crate::util::hex_encode(&hasher.finalize())
}

fn short_hash(value: &str) -> String {
    value.chars().take(16).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ── VOI allocator: representative + adversarial ──────────────────────

    #[test]
    fn voi_priority_schedules_flaky_first() {
        let evaluation = evaluate_voi_fixture(&voi_priority_fixture());
        assert!(
            evaluation.verdict.matches_expected,
            "mismatches: {:?}",
            evaluation.verdict.mismatches
        );
        let flaky = evaluation
            .diagnostics
            .iter()
            .find(|d| d.subject_id == "fixture-flaky")
            .expect("flaky diagnostic");
        assert_eq!(flaky.detection_outcome, DetectionOutcome::Scheduled);
        assert_eq!(flaky.model_class, OptimizationModelClass::VoiAllocator);
        assert_eq!(flaky.observation_index, -1);
    }

    #[test]
    fn voi_starvation_starves_everything_under_a_high_floor() {
        let evaluation = evaluate_voi_fixture(&voi_starvation_fixture());
        assert!(
            evaluation.verdict.matches_expected,
            "mismatches: {:?}",
            evaluation.verdict.mismatches
        );
        assert!(
            evaluation
                .diagnostics
                .iter()
                .all(|d| d.detection_outcome == DetectionOutcome::Starved)
        );
    }

    #[test]
    fn voi_tie_break_spreads_budget_evenly_and_deterministically() {
        let evaluation = evaluate_voi_fixture(&voi_tie_break_fixture());
        assert!(
            evaluation.verdict.matches_expected,
            "mismatches: {:?}",
            evaluation.verdict.mismatches
        );
        // All three identical candidates were funded.
        assert_eq!(
            evaluation
                .diagnostics
                .iter()
                .filter(|d| d.detection_outcome == DetectionOutcome::Scheduled)
                .count(),
            3
        );
    }

    #[test]
    fn voi_budget_constraint_starves_the_expensive_candidate() {
        let evaluation = evaluate_voi_fixture(&voi_budget_constrained_fixture());
        assert!(
            evaluation.verdict.matches_expected,
            "mismatches: {:?}",
            evaluation.verdict.mismatches
        );
        let pricey = evaluation
            .diagnostics
            .iter()
            .find(|d| d.subject_id == "pricey")
            .expect("pricey diagnostic");
        assert_eq!(pricey.detection_outcome, DetectionOutcome::Starved);
    }

    #[test]
    fn voi_scheduled_changes_state_but_starved_does_not() {
        // Invariant: a funded run must move the Beta posterior (prior != posterior
        // hash); a starved candidate's state is untouched (hashes equal).
        let evaluation = evaluate_voi_fixture(&voi_budget_constrained_fixture());
        for diagnostic in &evaluation.diagnostics {
            match diagnostic.detection_outcome {
                DetectionOutcome::Scheduled => assert_ne!(
                    diagnostic.prior_state_hash, diagnostic.posterior_state_hash,
                    "scheduled fixture {} did not change state",
                    diagnostic.subject_id
                ),
                DetectionOutcome::Starved => assert_eq!(
                    diagnostic.prior_state_hash, diagnostic.posterior_state_hash,
                    "starved fixture {} changed state",
                    diagnostic.subject_id
                ),
                other => panic!("unexpected VOI outcome {other:?}"),
            }
        }
    }

    // ── Drift monitor: representative + adversarial ──────────────────────

    #[test]
    fn drift_dropping_success_flags_cusum_and_bocpd_regression() {
        let evaluation = evaluate_drift_fixture(&drift_dropping_success_fixture());
        assert!(
            evaluation.verdict.matches_expected,
            "mismatches: {:?}",
            evaluation.verdict.mismatches
        );
        assert!(
            evaluation
                .diagnostics
                .iter()
                .any(|d| d.model_class == OptimizationModelClass::CusumDrift && d.is_regression())
        );
        let changepoint = evaluation
            .diagnostics
            .iter()
            .find(|d| d.model_class == OptimizationModelClass::BocpdDrift)
            .expect("a BOCPD changepoint diagnostic");
        assert!((9..=13).contains(&changepoint.observation_index));
    }

    #[test]
    fn drift_rising_latency_flags_a_regression() {
        let evaluation = evaluate_drift_fixture(&drift_rising_latency_fixture());
        assert!(
            evaluation.verdict.matches_expected,
            "mismatches: {:?}",
            evaluation.verdict.mismatches
        );
        assert!(evaluation.diagnostics.iter().any(|d| d.is_regression()));
    }

    #[test]
    fn drift_improving_success_flags_improvement_not_regression() {
        let evaluation = evaluate_drift_fixture(&drift_improving_success_fixture());
        assert!(
            evaluation.verdict.matches_expected,
            "mismatches: {:?}",
            evaluation.verdict.mismatches
        );
        assert!(
            evaluation
                .diagnostics
                .iter()
                .any(|d| d.detection_outcome == DetectionOutcome::Improvement)
        );
        assert!(evaluation.diagnostics.iter().all(|d| !d.is_regression()));
    }

    #[test]
    fn drift_stable_series_raises_no_events() {
        let evaluation = evaluate_drift_fixture(&drift_stable_fixture());
        assert!(
            evaluation.verdict.matches_expected,
            "mismatches: {:?}",
            evaluation.verdict.mismatches
        );
        assert!(evaluation.diagnostics.is_empty());
    }

    #[test]
    fn drift_degenerate_series_does_not_panic_or_fire() {
        let evaluation = evaluate_drift_fixture(&drift_degenerate_fixture());
        assert!(
            evaluation.verdict.matches_expected,
            "mismatches: {:?}",
            evaluation.verdict.mismatches
        );
        assert!(evaluation.diagnostics.is_empty());
    }

    #[test]
    fn drift_two_step_detects_both_successive_shifts() {
        let evaluation = evaluate_drift_fixture(&drift_two_step_fixture());
        assert!(
            evaluation.verdict.matches_expected,
            "mismatches: {:?}",
            evaluation.verdict.mismatches
        );
        let cusum = evaluation
            .diagnostics
            .iter()
            .filter(|d| d.model_class == OptimizationModelClass::CusumDrift)
            .count();
        assert_eq!(cusum, 2, "expected one CUSUM alarm per shift");
        let bocpd = evaluation
            .diagnostics
            .iter()
            .filter(|d| d.model_class == OptimizationModelClass::BocpdDrift)
            .count();
        assert!(bocpd >= 2, "expected a BOCPD changepoint per shift");
    }

    // ── Oracle non-vacuity: the verdict must actually catch mismatches ────

    #[test]
    fn verdict_detects_a_wrong_voi_expectation() {
        let mut fixture = voi_priority_fixture();
        fixture.expected.top_fixture_id = Some("fixture-stable".to_string());
        let evaluation = evaluate_voi_fixture(&fixture);
        assert!(!evaluation.verdict.matches_expected);
        assert!(!evaluation.verdict.mismatches.is_empty());
    }

    #[test]
    fn verdict_detects_a_wrong_drift_expectation() {
        let mut fixture = drift_stable_fixture();
        // Claim the stable series should regress — it must not.
        fixture.expected.regressed_metrics = vec!["stable_success_rate".to_string()];
        fixture.expected.clean_metrics = Vec::new();
        let evaluation = evaluate_drift_fixture(&fixture);
        assert!(!evaluation.verdict.matches_expected);
    }

    // ── Acceptance criterion 3: required failure-log fields ───────────────

    #[test]
    fn every_diagnostic_carries_required_fields() {
        let report = run_optimization_validation(&regime_shift_corpus());
        assert!(report.summary.total_diagnostics > 0);
        for diagnostic in &report.diagnostics {
            assert!(
                diagnostic.has_required_fields(),
                "diagnostic missing a required field: {diagnostic:?}"
            );
            assert!(diagnostic.replay_cmd.contains("doctor_frankentui"));
        }
    }

    #[test]
    fn failure_log_projects_the_mandated_schema() {
        let report = run_optimization_validation(&regime_shift_corpus());
        for diagnostic in &report.diagnostics {
            let log = diagnostic.failure_log();
            assert_eq!(log.model_id, diagnostic.model_id);
            assert_eq!(log.prior_state_hash, diagnostic.prior_state_hash);
            assert_eq!(log.posterior_state_hash, diagnostic.posterior_state_hash);
            assert_eq!(log.threshold_set, diagnostic.threshold_set);
            assert_eq!(log.detection_outcome, diagnostic.detection_outcome.as_str());
            assert_eq!(log.replay_cmd, diagnostic.replay_cmd);
            assert!(!log.model_id.is_empty());
            assert!(!log.prior_state_hash.is_empty());
            assert!(!log.posterior_state_hash.is_empty());
            assert!(!log.threshold_set.is_empty());
            assert!(!log.detection_outcome.is_empty());
            assert!(!log.replay_cmd.is_empty());
        }
        assert_eq!(report.failure_logs().len(), report.diagnostics.len());
    }

    #[test]
    fn threshold_set_is_model_appropriate() {
        let report = run_optimization_validation(&regime_shift_corpus());
        for diagnostic in &report.diagnostics {
            match diagnostic.model_class {
                OptimizationModelClass::VoiAllocator => {
                    assert!(diagnostic.threshold_set.contains("voi_floor"));
                }
                OptimizationModelClass::CusumDrift => {
                    assert!(diagnostic.threshold_set.contains("cusum_k"));
                }
                OptimizationModelClass::BocpdDrift => {
                    assert!(diagnostic.threshold_set.contains("bocpd_hazard_lambda"));
                }
            }
        }
    }

    // ── Corpus-level expectations + spread ────────────────────────────────

    #[test]
    fn regime_shift_corpus_meets_every_expectation() {
        let report = run_optimization_validation(&regime_shift_corpus());
        assert!(
            report.summary.all_expectations_met,
            "failing verdicts: {:?}",
            report.failing_verdicts()
        );
        assert_eq!(
            report.summary.passing_verdicts,
            report.summary.total_verdicts
        );
    }

    #[test]
    fn green_corpus_meets_every_expectation() {
        let report = run_optimization_validation(&green_corpus());
        assert!(
            report.summary.all_expectations_met,
            "failing verdicts: {:?}",
            report.failing_verdicts()
        );
    }

    #[test]
    fn regime_shift_corpus_spans_all_three_model_classes() {
        let report = run_optimization_validation(&regime_shift_corpus());
        for class in OptimizationModelClass::ALL {
            assert!(
                !report.diagnostics_for(*class).is_empty(),
                "no diagnostics from {}",
                class.as_str()
            );
        }
    }

    #[test]
    fn summary_counts_are_internally_consistent() {
        let report = run_optimization_validation(&regime_shift_corpus());
        let summary = &report.summary;
        assert_eq!(
            summary.voi_diagnostics + summary.cusum_diagnostics + summary.bocpd_diagnostics,
            summary.total_diagnostics
        );
        assert_eq!(
            summary.scheduled_count + summary.starved_count,
            summary.voi_diagnostics
        );
        assert!(summary.regression_count >= 2);
        assert!(summary.improvement_count >= 1);
    }

    // ── Acceptance criterion 2: byte-stable deterministic outputs ─────────

    #[test]
    fn report_is_deterministic() {
        let corpus = regime_shift_corpus();
        let first = run_optimization_validation(&corpus);
        let second = run_optimization_validation(&corpus);
        assert_eq!(first.report_id, second.report_id);
        assert_eq!(first.evidence_checksum, second.evidence_checksum);
        assert_eq!(first.diagnostics, second.diagnostics);
        assert_eq!(first.verdicts, second.verdicts);
        assert_eq!(
            first.exported_json_stats.sha256,
            second.exported_json_stats.sha256
        );
    }

    #[test]
    fn report_roundtrips_through_serde() {
        let report = run_optimization_validation(&regime_shift_corpus());
        let json = serde_json::to_string(&report).expect("serialize");
        let restored: OptimizationValidationReport =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.report_id, report.report_id);
        assert_eq!(restored.diagnostics, report.diagnostics);
        assert_eq!(restored.verdicts, report.verdicts);
        assert_eq!(restored.summary, report.summary);
        assert_eq!(restored.evidence_checksum, report.evidence_checksum);
    }

    #[test]
    fn json_stats_checksum_is_self_consistent() {
        let report = run_optimization_validation(&regime_shift_corpus());
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }

    #[test]
    fn replay_command_references_report_id() {
        let report = run_optimization_validation(&green_corpus());
        assert!(report.replay_command.contains(&report.report_id));
        assert!(report.replay_command.contains("optimization-validate"));
    }

    #[test]
    fn distinct_scenario_label_changes_report_id() {
        let green = run_optimization_validation(&green_corpus());
        let regime = run_optimization_validation(&regime_shift_corpus());
        assert_ne!(green.report_id, regime.report_id);
        assert_ne!(green.evidence_checksum, regime.evidence_checksum);
    }

    // ── Property tests ────────────────────────────────────────────────────

    /// Build a corpus deterministically from a 4-bit selection mask so the same
    /// mask always yields a content-stable corpus.
    fn corpus_from_mask(label: &str, mask: u8) -> OptimizationFixtureCorpus {
        let mut voi_fixtures = vec![voi_priority_fixture()];
        if mask & 0b0001 != 0 {
            voi_fixtures.push(voi_starvation_fixture());
        }
        if mask & 0b0010 != 0 {
            voi_fixtures.push(voi_budget_constrained_fixture());
        }
        let mut drift_fixtures = vec![drift_stable_fixture()];
        if mask & 0b0100 != 0 {
            drift_fixtures.push(drift_dropping_success_fixture());
        }
        if mask & 0b1000 != 0 {
            drift_fixtures.push(drift_two_step_fixture());
        }
        OptimizationFixtureCorpus {
            label: format!("{label}-{mask}"),
            voi_fixtures,
            drift_fixtures,
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// AC#2: equivalent inputs produce byte-identical reports — including the
        /// diagnostic ordering, model ids, state hashes, and every checksum.
        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}", mask in 0u8..16) {
            let corpus = corpus_from_mask(&label, mask);
            let first = run_optimization_validation(&corpus);
            let second = run_optimization_validation(&corpus);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.diagnostics, &second.diagnostics);
            prop_assert_eq!(&first.verdicts, &second.verdicts);
            prop_assert_eq!(
                &first.exported_json_stats.sha256,
                &second.exported_json_stats.sha256
            );
        }

        /// Every emitted diagnostic always carries the full required field set and
        /// a `doctor_frankentui` replay command, regardless of corpus shape.
        #[test]
        fn prop_every_diagnostic_has_required_fields(mask in 0u8..16) {
            let report = run_optimization_validation(&corpus_from_mask("fields", mask));
            for diagnostic in &report.diagnostics {
                prop_assert!(diagnostic.has_required_fields());
                prop_assert!(diagnostic.replay_cmd.contains("doctor_frankentui"));
            }
        }

        /// Every fixture in a mask-built corpus meets its expectation: the encoded
        /// expected outcomes match the models' actual behavior across all shapes.
        #[test]
        fn prop_corpus_expectations_always_hold(mask in 0u8..16) {
            let report = run_optimization_validation(&corpus_from_mask("expect", mask));
            prop_assert!(report.summary.all_expectations_met);
        }

        /// No cross-model state leakage: adding VOI fixtures never changes the
        /// drift diagnostics, and the models stay independent.
        #[test]
        fn prop_models_do_not_leak_state(extra in 0u8..3) {
            let mut base = OptimizationFixtureCorpus {
                label: "leak-base".to_string(),
                voi_fixtures: vec![voi_priority_fixture()],
                drift_fixtures: vec![drift_dropping_success_fixture()],
            };
            let report_base = run_optimization_validation(&base);
            base.label = "leak-extra".to_string();
            for _ in 0..extra {
                base.voi_fixtures.push(voi_tie_break_fixture());
            }
            let report_extra = run_optimization_validation(&base);
            let drift_base: Vec<_> = report_base
                .diagnostics
                .iter()
                .filter(|d| d.model_class != OptimizationModelClass::VoiAllocator)
                .collect();
            let drift_extra: Vec<_> = report_extra
                .diagnostics
                .iter()
                .filter(|d| d.model_class != OptimizationModelClass::VoiAllocator)
                .collect();
            prop_assert_eq!(drift_base, drift_extra);
        }

        /// Re-evaluating the same VOI fixture is stateless: identical allocation
        /// ids and diagnostics every time.
        #[test]
        fn prop_voi_evaluation_is_stateless(repeats in 1u8..5) {
            let fixture = voi_priority_fixture();
            let baseline = evaluate_voi_fixture(&fixture);
            for _ in 0..repeats {
                let again = evaluate_voi_fixture(&fixture);
                prop_assert_eq!(&again.report.allocation_id, &baseline.report.allocation_id);
                prop_assert_eq!(&again.diagnostics, &baseline.diagnostics);
            }
        }
    }
}
