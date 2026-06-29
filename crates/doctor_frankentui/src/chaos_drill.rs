//! End-to-end reverse-round chaos drill and portfolio-fallback validation
//! pipeline (bd-3bxhj.10.20).
//!
//! This is the chaos-validation *producer* behind
//! `scripts/doctor_frankentui_chaos_drill_e2e.sh`. It stress-tests the three
//! alien-governance decision kernels under adversarial conditions and proves they
//! **degrade safely** — never silently promoting an unsafe change:
//!
//! - the reverse-round one-lever governance gate ([`crate::reverse_round_governance`])
//!   — must block multi-lever merges without an override, refuse a behavior-
//!   changing (non-isomorphic) lever, and trigger an automatic rollback on a
//!   percentile regression;
//! - the expected-loss portfolio scheduler ([`crate::portfolio_scheduler`]) — must
//!   fall back to the minimax-safe primitive (or defer) under budget exhaustion,
//!   an uncertainty spike, or a drift signal;
//! - the formal guarantee layer ([`crate::guarantee_layer`]) — must recommend a
//!   conservative fallback under a calibration (coverage) failure, and must *not*
//!   raise a false discovery under an optional-stopping perturbation
//!   (anytime-validity).
//!
//! # Chaos injection
//!
//! Each scenario injects a synthetic perturbation (extra enabled levers,
//! contradictory isomorphism evidence, a percentile-regression drift, a budget
//! squeeze, a maximally-uncertain posterior, a drift signal, a conformal coverage
//! shortfall, or an optional-stopping observation stream) and records the
//! kernel's actual response. A single **baseline** scenario (no perturbation)
//! proves the drill still *promotes* a safe change — so the gate is not vacuously
//! green by always blocking.
//!
//! # Safe-degradation gate
//!
//! Every scenario declares its `expected_path`; the observed `action_path` must
//! match it. The gate fails closed if any scenario's safe outcome was *not*
//! observed (an unsafe merge that wasn't blocked, a budget exhaustion that didn't
//! defer, a calibration failure that didn't fall back, an optional-stopping
//! stream that falsely rejected), or if any ledger line is missing a mandated
//! field. The ledger is float-free, so it derives [`Eq`] and replays
//! byte-identically (explainability reconstruction fidelity).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decision_loss_policy::RiskTier;
use crate::golden_isomorphism::ChecksumVerdict;
use crate::guarantee_layer::{
    EProcessConfig, GuaranteeEvaluationInput, GuaranteeLayerEngine, GuaranteeReport, PacBayesInput,
    run_eprocess,
};
use crate::milestone_policy::{MathFamily, QualityBar};
use crate::portfolio_scheduler::{
    PortfolioScheduler, PortfolioSchedulerConfig, PortfolioSchedulerReport, PrimitiveCandidate,
    ScheduleStage, SchedulerDecision, SchedulerMilestone,
};
use crate::posterior_core::{
    ChannelEvidence, EvidenceChannel, PosteriorCoreConfig, PosteriorEngine,
};
use crate::reverse_round_governance::{
    BaselineComparator, ComparatorRegistry, DecisionWindow, MetricFamily, OptimizationLever,
    PercentileDeltas, ReverseRoundFindingCode, ReverseRoundGate, ReverseRoundGovernanceReport,
    ReverseRoundRequest, RollbackDecision, RollbackPolicy, UpliftClaim,
};
use crate::test_budget_allocator::BetaPosterior;

/// Schema version for the in-memory chaos-drill report.
pub const CHAOS_DRILL_SCHEMA_VERSION: &str = "chaos-drill-v1";

/// Schema version for the materialized chaos-drill pipeline artifacts.
pub const CHAOS_DRILL_PIPELINE_SCHEMA_VERSION: &str = "chaos-drill-pipeline-v1";

// ── Hashing helpers ──────────────────────────────────────────────────────────

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

// ── Vocabulary ───────────────────────────────────────────────────────────────

/// Which decision kernel a chaos scenario exercises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChaosKernel {
    /// The reverse-round one-lever governance gate.
    ReverseRound,
    /// The expected-loss portfolio scheduler.
    Portfolio,
    /// The formal guarantee layer.
    Guarantee,
}

impl ChaosKernel {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReverseRound => "reverse_round",
            Self::Portfolio => "portfolio",
            Self::Guarantee => "guarantee",
        }
    }

    /// The policy id governing this kernel.
    #[must_use]
    pub fn policy_id(self) -> String {
        format!("alien-uplift/{}", self.as_str())
    }
}

/// A chaos scenario kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChaosScenarioKind {
    /// Control: a safe change with no perturbation must still promote.
    BaselinePromote,
    /// Two enabled levers in one window without an override must be blocked.
    MultiLeverMerge,
    /// A behavior-changing (non-isomorphic) lever must be blocked.
    ContradictoryEvidence,
    /// A percentile regression must trigger an automatic rollback.
    PerformanceDrift,
    /// A budget squeeze must force a conservative defer.
    BudgetExhaustion,
    /// A maximally-uncertain posterior must force a conservative fallback.
    UncertaintySpike,
    /// A drift signal must force a conservative fallback.
    PortfolioDrift,
    /// A conformal coverage shortfall must recommend a conservative fallback.
    CalibrationFailure,
    /// An optional-stopping stream must not raise a false discovery.
    OptionalStopping,
}

impl ChaosScenarioKind {
    /// All scenarios in canonical order.
    pub const ALL: [ChaosScenarioKind; 9] = [
        ChaosScenarioKind::BaselinePromote,
        ChaosScenarioKind::MultiLeverMerge,
        ChaosScenarioKind::ContradictoryEvidence,
        ChaosScenarioKind::PerformanceDrift,
        ChaosScenarioKind::BudgetExhaustion,
        ChaosScenarioKind::UncertaintySpike,
        ChaosScenarioKind::PortfolioDrift,
        ChaosScenarioKind::CalibrationFailure,
        ChaosScenarioKind::OptionalStopping,
    ];

    /// Stable lowercase tag (the `chaos_scenario_id`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BaselinePromote => "baseline_promote",
            Self::MultiLeverMerge => "multi_lever_merge",
            Self::ContradictoryEvidence => "contradictory_evidence",
            Self::PerformanceDrift => "performance_drift",
            Self::BudgetExhaustion => "budget_exhaustion",
            Self::UncertaintySpike => "uncertainty_spike",
            Self::PortfolioDrift => "portfolio_drift",
            Self::CalibrationFailure => "calibration_failure",
            Self::OptionalStopping => "optional_stopping",
        }
    }

    /// Which kernel this scenario exercises.
    #[must_use]
    pub fn kernel(self) -> ChaosKernel {
        match self {
            Self::BaselinePromote
            | Self::MultiLeverMerge
            | Self::ContradictoryEvidence
            | Self::PerformanceDrift => ChaosKernel::ReverseRound,
            Self::BudgetExhaustion | Self::UncertaintySpike | Self::PortfolioDrift => {
                ChaosKernel::Portfolio
            }
            Self::CalibrationFailure | Self::OptionalStopping => ChaosKernel::Guarantee,
        }
    }

    /// The action path the kernel must take for a safe outcome.
    #[must_use]
    pub fn expected_path(self) -> &'static str {
        match self {
            Self::BaselinePromote => "promote",
            Self::MultiLeverMerge | Self::ContradictoryEvidence => "blocked",
            Self::PerformanceDrift => "rollback",
            Self::BudgetExhaustion => "defer",
            Self::UncertaintySpike | Self::PortfolioDrift => "conservative",
            Self::CalibrationFailure => "fallback",
            Self::OptionalStopping => "holds",
        }
    }
}

// ── Ledger ───────────────────────────────────────────────────────────────────

/// One chaos-drill ledger line (AC1). All fields are strings/enums/bools, so the
/// ledger derives `Eq` and replays byte-identically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChaosLedgerEntry {
    /// Ledger schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// The chaos scenario id.
    pub chaos_scenario_id: String,
    /// Which kernel the scenario exercised.
    pub kernel: ChaosKernel,
    /// The governing policy id.
    pub policy_id: String,
    /// The subject claim / milestone id.
    pub claim_id: String,
    /// The observed action path.
    pub action_path: String,
    /// The expected (safe) action path.
    pub expected_path: String,
    /// The formal-guarantee status (`holds` / `fallback` / `rejected` / `n/a`).
    pub guarantee_status: String,
    /// The fallback / block reason.
    pub fallback_reason: String,
    /// The rollback verdict (`n/a` off the reverse-round kernel).
    pub rollback_verdict: String,
    /// Whether the post-rollback incumbent baseline was identified for restore.
    pub baseline_restored: bool,
    /// Whether the scenario degraded safely (observed == expected path).
    pub safe_degradation_ok: bool,
    /// Human-readable detail.
    pub detail: String,
    /// Remediation for an unsafe outcome (empty when safe).
    pub remediation: Vec<String>,
    /// Deterministic single-command replay reference.
    pub reproduction_command: String,
}

fn required_fields_present(line: &ChaosLedgerEntry) -> bool {
    !line.schema_version.is_empty()
        && !line.run_id.is_empty()
        && !line.chaos_scenario_id.is_empty()
        && !line.policy_id.is_empty()
        && !line.claim_id.is_empty()
        && !line.action_path.is_empty()
        && !line.expected_path.is_empty()
        && !line.guarantee_status.is_empty()
        && !line.fallback_reason.is_empty()
        && !line.rollback_verdict.is_empty()
        && !line.detail.is_empty()
        && !line.reproduction_command.is_empty()
}

fn render_ledger_jsonl(ledger: &[ChaosLedgerEntry]) -> String {
    let mut out = String::new();
    for entry in ledger {
        match serde_json::to_string(entry) {
            Ok(line) => out.push_str(&line),
            Err(error) => out.push_str(&error.to_string()),
        }
        out.push('\n');
    }
    out
}

/// The raw outcome of one scenario (pre-ledger).
struct ScenarioOutcome {
    claim_id: String,
    action_path: String,
    guarantee_status: String,
    fallback_reason: String,
    rollback_verdict: String,
    baseline_restored: bool,
    detail: String,
}

// ── Construction helpers (reused across scenarios) ───────────────────────────

fn posterior(claim_id: &str) -> crate::posterior_core::PosteriorRecord {
    let engine = PosteriorEngine::new(PosteriorCoreConfig::default());
    engine.infer(
        claim_id,
        &[
            ChannelEvidence::new(EvidenceChannel::Semantic, 3.0).claim(claim_id),
            ChannelEvidence::new(EvidenceChannel::Visual, 2.5).claim(claim_id),
            ChannelEvidence::new(EvidenceChannel::Performance, 2.0).claim(claim_id),
            ChannelEvidence::new(EvidenceChannel::Accessibility, 1.5).claim(claim_id),
            ChannelEvidence::new(EvidenceChannel::Determinism, 1.5).claim(claim_id),
        ],
    )
}

fn registered_comparator(claim_id: &str) -> ComparatorRegistry {
    let comparator = BaselineComparator::new(
        format!("cmp-{claim_id}"),
        claim_id,
        format!("baseline-{claim_id}"),
        MetricFamily::Latency,
    );
    let mut registry = ComparatorRegistry::new();
    let _ = registry.register(comparator);
    registry
}

fn uplift_claim(
    claim_id: &str,
    equation_id: &str,
    verdict: ChecksumVerdict,
    deltas: PercentileDeltas,
    rollback: RollbackPolicy,
) -> UpliftClaim {
    UpliftClaim::new(
        claim_id,
        format!("cmp-{claim_id}"),
        OptimizationLever::new(
            format!("lever-{claim_id}"),
            equation_id,
            "isolated one-lever change",
        ),
        posterior(claim_id),
        verdict,
        deltas,
        rollback,
    )
}

fn candidate(
    id: &str,
    milestone: &str,
    family: MathFamily,
    alpha: f64,
    beta: f64,
    cost: f64,
) -> PrimitiveCandidate {
    PrimitiveCandidate::new(id, milestone, family, BetaPosterior::new(alpha, beta), cost)
}

fn reverse_round_action(
    report: &ReverseRoundGovernanceReport,
    claim_id: &str,
) -> (String, String, bool) {
    let rollback = report.decision_log(claim_id).map(|l| l.rollback_decision);
    let rolled_back = matches!(rollback, Some(RollbackDecision::RollbackTriggered));
    // Per-claim block: this *specific* claim carries a blocking finding (not the
    // report-global gate, which would mislabel a passing claim in a multi-claim
    // request).
    let blocked = report.blocked_claim_ids.iter().any(|c| c == claim_id);
    let action_path = if rolled_back {
        "rollback".to_string()
    } else if blocked {
        "blocked".to_string()
    } else {
        "promote".to_string()
    };
    let rollback_verdict = rollback.map_or_else(|| "n/a".to_string(), |r| r.as_str().to_string());
    // The incumbent baseline is "restored" only when a rollback actually
    // triggers; a registered comparator id (present on every ledger entry) does
    // not by itself mean a restoration occurred.
    let baseline_restored = rolled_back
        && report
            .ledger_entry(claim_id)
            .is_some_and(|e| !e.incumbent_baseline_id.is_empty());
    (action_path, rollback_verdict, baseline_restored)
}

fn portfolio_chaos_outcome(report: &PortfolioSchedulerReport, milestone: &str) -> (String, String) {
    let line = report
        .ledger
        .iter()
        .find(|l| l.stage == ScheduleStage::Govern && l.milestone_id == milestone);
    match line {
        Some(l) if l.decision == SchedulerDecision::Conservative => {
            let path = if l.primitive_id == "defer" {
                "defer".to_string()
            } else {
                "conservative".to_string()
            };
            (path, l.safety_trigger.as_str().to_string())
        }
        Some(_) => ("promote".to_string(), "none".to_string()),
        None => ("promote".to_string(), "none".to_string()),
    }
}

fn guarantee_status(report: &GuaranteeReport) -> &'static str {
    if report.fallback.triggered {
        "fallback"
    } else if report.e_process.rejected {
        "rejected"
    } else {
        "holds"
    }
}

// ── Scenario runners ─────────────────────────────────────────────────────────

fn run_scenario(kind: ChaosScenarioKind) -> ScenarioOutcome {
    match kind {
        ChaosScenarioKind::BaselinePromote => run_baseline_promote(),
        ChaosScenarioKind::MultiLeverMerge => run_multi_lever(),
        ChaosScenarioKind::ContradictoryEvidence => run_contradictory_evidence(),
        ChaosScenarioKind::PerformanceDrift => run_performance_drift(),
        ChaosScenarioKind::BudgetExhaustion => run_budget_exhaustion(),
        ChaosScenarioKind::UncertaintySpike => run_uncertainty_spike(),
        ChaosScenarioKind::PortfolioDrift => run_portfolio_drift(),
        ChaosScenarioKind::CalibrationFailure => run_calibration_failure(),
        ChaosScenarioKind::OptionalStopping => run_optional_stopping(),
    }
}

fn run_baseline_promote() -> ScenarioOutcome {
    let claim_id = "chaos-baseline";
    let claim = uplift_claim(
        claim_id,
        "graveyard-eq-baseline",
        ChecksumVerdict::Match,
        PercentileDeltas::improvement(0.10),
        RollbackPolicy::AutomaticOnRegression,
    );
    let request = ReverseRoundRequest::new(
        registered_comparator(claim_id),
        vec![DecisionWindow::new(
            "chaos-window-baseline",
            vec![claim_id.to_string()],
        )],
        vec![claim],
    );
    let report = ReverseRoundGate::default().evaluate(&request);
    let (action_path, rollback_verdict, baseline_restored) =
        reverse_round_action(&report, claim_id);
    ScenarioOutcome {
        claim_id: claim_id.to_string(),
        action_path,
        guarantee_status: "holds".to_string(),
        fallback_reason: "none".to_string(),
        rollback_verdict,
        baseline_restored,
        detail: format!(
            "baseline safe change promotes (gate_passes={})",
            report.gate_passes
        ),
    }
}

fn run_multi_lever() -> ScenarioOutcome {
    let a = uplift_claim(
        "chaos-multi-a",
        "graveyard-eq-100",
        ChecksumVerdict::Match,
        PercentileDeltas::improvement(0.08),
        RollbackPolicy::AutomaticOnRegression,
    );
    let b = uplift_claim(
        "chaos-multi-b",
        "graveyard-eq-101",
        ChecksumVerdict::Match,
        PercentileDeltas::improvement(0.07),
        RollbackPolicy::AutomaticOnRegression,
    );
    let mut registry = registered_comparator("chaos-multi-a");
    let _ = registry.register(BaselineComparator::new(
        "cmp-chaos-multi-b",
        "chaos-multi-b",
        "baseline-chaos-multi-b",
        MetricFamily::Latency,
    ));
    // Two enabled levers competing in one window with no override artifact.
    let window = DecisionWindow::new(
        "chaos-window-multi",
        vec!["chaos-multi-a".to_string(), "chaos-multi-b".to_string()],
    );
    let request = ReverseRoundRequest::new(registry, vec![window], vec![a, b]);
    let report = ReverseRoundGate::default().evaluate(&request);
    let multi = report
        .findings
        .iter()
        .any(|f| f.code == ReverseRoundFindingCode::MultiLeverWithoutOverride);
    let action_path = if !report.gate_passes && multi {
        "blocked".to_string()
    } else {
        "promote".to_string()
    };
    ScenarioOutcome {
        claim_id: "chaos-multi-a".to_string(),
        action_path,
        guarantee_status: "n/a".to_string(),
        fallback_reason: "multi_lever_without_override".to_string(),
        rollback_verdict: "n/a".to_string(),
        baseline_restored: false,
        detail: format!(
            "two enabled levers in one window blocked (multi_lever_windows={})",
            report.summary.multi_lever_windows
        ),
    }
}

fn run_contradictory_evidence() -> ScenarioOutcome {
    let claim_id = "chaos-iso";
    // Contradictory evidence: the lever's behavior changed (non-isomorphic). The
    // hold-no-rollback policy keeps the action a clean isomorphism-gate BLOCK
    // (rollback suppressed / escalate) rather than an auto-rollback, so this
    // scenario exercises the block path distinctly from `performance_drift`.
    let claim = uplift_claim(
        claim_id,
        "graveyard-eq-200",
        ChecksumVerdict::Mismatch,
        PercentileDeltas::improvement(0.05),
        RollbackPolicy::HoldNoRollback,
    );
    let request = ReverseRoundRequest::new(
        registered_comparator(claim_id),
        vec![DecisionWindow::new(
            "chaos-window-iso",
            vec![claim_id.to_string()],
        )],
        vec![claim],
    );
    let report = ReverseRoundGate::default().evaluate(&request);
    let (action_path, rollback_verdict, baseline_restored) =
        reverse_round_action(&report, claim_id);
    let iso = report
        .findings
        .iter()
        .any(|f| f.code == ReverseRoundFindingCode::IsomorphismViolation);
    ScenarioOutcome {
        claim_id: claim_id.to_string(),
        action_path,
        guarantee_status: "n/a".to_string(),
        fallback_reason: if iso {
            "isomorphism_violation".to_string()
        } else {
            "none".to_string()
        },
        rollback_verdict,
        baseline_restored,
        detail: "contradictory (non-isomorphic) evidence blocked".to_string(),
    }
}

fn run_performance_drift() -> ScenarioOutcome {
    let claim_id = "chaos-drift-perf";
    // Synthetic drift: a real percentile regression beyond budget. The
    // automatic-on-regression policy must trigger a rollback to the incumbent.
    let claim = uplift_claim(
        claim_id,
        "graveyard-eq-300",
        ChecksumVerdict::Match,
        PercentileDeltas::new(0.6, 0.6, 0.6),
        RollbackPolicy::AutomaticOnRegression,
    );
    let request = ReverseRoundRequest::new(
        registered_comparator(claim_id),
        vec![DecisionWindow::new(
            "chaos-window-drift",
            vec![claim_id.to_string()],
        )],
        vec![claim],
    );
    let report = ReverseRoundGate::default().evaluate(&request);
    let (action_path, rollback_verdict, baseline_restored) =
        reverse_round_action(&report, claim_id);
    ScenarioOutcome {
        claim_id: claim_id.to_string(),
        action_path,
        guarantee_status: "n/a".to_string(),
        fallback_reason: "percentile_regression_beyond_budget".to_string(),
        rollback_verdict,
        baseline_restored,
        detail: "percentile-regression drift triggers automatic rollback to incumbent baseline"
            .to_string(),
    }
}

fn run_budget_exhaustion() -> ScenarioOutcome {
    let config = PortfolioSchedulerConfig {
        budget: 3.0,
        ..PortfolioSchedulerConfig::default()
    };
    let milestones = vec![
        SchedulerMilestone::new(
            "chaos.cheap",
            QualityBar::Bronze,
            RiskTier::Medium,
            vec![candidate(
                "cheap",
                "chaos.cheap",
                MathFamily::Symbolic,
                40.0,
                4.0,
                1.0,
            )],
        ),
        SchedulerMilestone::new(
            "chaos.budget",
            QualityBar::Bronze,
            RiskTier::Medium,
            vec![candidate(
                "expensive",
                "chaos.budget",
                MathFamily::FormalAnalysis,
                40.0,
                4.0,
                5.0,
            )],
        ),
    ];
    let report = PortfolioScheduler::new("chaos/budget", config, milestones).run(None);
    let (action_path, fallback_reason) = portfolio_chaos_outcome(&report, "chaos.budget");
    ScenarioOutcome {
        claim_id: "chaos.budget".to_string(),
        action_path,
        guarantee_status: "n/a".to_string(),
        fallback_reason,
        rollback_verdict: "n/a".to_string(),
        baseline_restored: false,
        detail: "budget exhaustion forces a conservative defer".to_string(),
    }
}

fn run_uncertainty_spike() -> ScenarioOutcome {
    let milestones = vec![
        SchedulerMilestone::new(
            "chaos.confident",
            QualityBar::Bronze,
            RiskTier::Medium,
            vec![candidate(
                "confident",
                "chaos.confident",
                MathFamily::Symbolic,
                46.0,
                4.0,
                2.0,
            )],
        ),
        SchedulerMilestone::new(
            "chaos.uncertain",
            QualityBar::Bronze,
            RiskTier::Medium,
            // Beta(2, 2): variance 0.05 >= the 0.02 uncertainty threshold.
            vec![candidate(
                "uncertain",
                "chaos.uncertain",
                MathFamily::FormalAnalysis,
                2.0,
                2.0,
                2.0,
            )],
        ),
    ];
    let report = PortfolioScheduler::new(
        "chaos/uncertain",
        PortfolioSchedulerConfig::default(),
        milestones,
    )
    .run(None);
    let (action_path, fallback_reason) = portfolio_chaos_outcome(&report, "chaos.uncertain");
    ScenarioOutcome {
        claim_id: "chaos.uncertain".to_string(),
        action_path,
        guarantee_status: "n/a".to_string(),
        fallback_reason,
        rollback_verdict: "n/a".to_string(),
        baseline_restored: false,
        detail: "uncertainty spike forces a conservative fallback".to_string(),
    }
}

fn run_portfolio_drift() -> ScenarioOutcome {
    let milestones = vec![
        SchedulerMilestone::new(
            "chaos.steady",
            QualityBar::Bronze,
            RiskTier::Medium,
            vec![candidate(
                "steady",
                "chaos.steady",
                MathFamily::Symbolic,
                46.0,
                4.0,
                2.0,
            )],
        ),
        SchedulerMilestone::new(
            "chaos.drift",
            QualityBar::Bronze,
            RiskTier::Medium,
            vec![
                candidate(
                    "drift",
                    "chaos.drift",
                    MathFamily::FormalAnalysis,
                    45.0,
                    5.0,
                    2.0,
                )
                .with_drift(0.8),
            ],
        ),
    ];
    let report = PortfolioScheduler::new(
        "chaos/drift",
        PortfolioSchedulerConfig::default(),
        milestones,
    )
    .run(None);
    let (action_path, fallback_reason) = portfolio_chaos_outcome(&report, "chaos.drift");
    ScenarioOutcome {
        claim_id: "chaos.drift".to_string(),
        action_path,
        guarantee_status: "n/a".to_string(),
        fallback_reason,
        rollback_verdict: "n/a".to_string(),
        baseline_restored: false,
        detail: "drift signal forces a conservative fallback".to_string(),
    }
}

fn run_calibration_failure() -> ScenarioOutcome {
    let claim_id = "chaos-calibration";
    // A conformal coverage shortfall: validation residuals far exceed the
    // calibrated quantile, so the forecast must recommend a fallback.
    let input = GuaranteeEvaluationInput::new(claim_id, 0.8, PacBayesInput::new(0.1, 1.0, 1000))
        .with_calibration((0..50).map(|i| f64::from(i) * 0.01))
        .with_validation((0..20).map(|_| (0.0, 0.9)))
        .with_observations(vec![0.1; 20]);
    match GuaranteeLayerEngine::default().evaluate(&input) {
        Ok(report) => {
            let status = guarantee_status(&report);
            let action_path = if report.fallback.triggered {
                "fallback".to_string()
            } else {
                "promote".to_string()
            };
            let reason = if report.fallback.reasons.is_empty() {
                "calibration_failure".to_string()
            } else {
                report.fallback.reasons.join("|")
            };
            ScenarioOutcome {
                claim_id: claim_id.to_string(),
                action_path,
                guarantee_status: status.to_string(),
                fallback_reason: reason,
                rollback_verdict: "n/a".to_string(),
                baseline_restored: false,
                detail: "calibration (coverage) failure recommends a conservative fallback"
                    .to_string(),
            }
        }
        Err(error) => ScenarioOutcome {
            claim_id: claim_id.to_string(),
            action_path: "error".to_string(),
            guarantee_status: "error".to_string(),
            fallback_reason: error.to_string(),
            rollback_verdict: "n/a".to_string(),
            baseline_restored: false,
            detail: "guarantee evaluation failed".to_string(),
        },
    }
}

fn run_optional_stopping() -> ScenarioOutcome {
    let claim_id = "chaos-optional-stopping";
    // Optional-stopping perturbation: a null-consistent stream whose empirical
    // mean equals the null mean mu0 but that genuinely FLUCTUATES (alternating
    // runs above and below mu0), peeked at 40 times. The e-process wealth wanders
    // up and down as a test supermartingale; an anytime-valid e-process must NOT
    // raise a false discovery (the wealth never crosses 1/alpha) despite the
    // peeking. (A flat stream pinned at mu0 would leave the wealth at exactly 1
    // and test nothing.)
    let config = EProcessConfig::default().with_alpha(0.05).with_mu0(0.1);
    // Repeating [0.2, 0.2, 0.0, 0.0] (mean 0.1 = mu0) over 40 peeks.
    let observations: Vec<f64> = [0.2_f64, 0.2, 0.0, 0.0]
        .into_iter()
        .cycle()
        .take(40)
        .collect();
    match run_eprocess(&config, &observations) {
        Ok(result) => {
            let action_path = if result.rejected {
                "false_reject".to_string()
            } else {
                "holds".to_string()
            };
            ScenarioOutcome {
                claim_id: claim_id.to_string(),
                action_path,
                guarantee_status: if result.rejected { "rejected" } else { "holds" }.to_string(),
                fallback_reason: "anytime_valid_no_false_discovery".to_string(),
                rollback_verdict: "n/a".to_string(),
                baseline_restored: false,
                detail: format!(
                    "optional-stopping over {} peeks holds (rejected={}, peak_wealth={:.4})",
                    observations.len(),
                    result.rejected,
                    result.peak_wealth
                ),
            }
        }
        Err(error) => ScenarioOutcome {
            claim_id: claim_id.to_string(),
            action_path: "error".to_string(),
            guarantee_status: "error".to_string(),
            fallback_reason: error.to_string(),
            rollback_verdict: "n/a".to_string(),
            baseline_restored: false,
            detail: "e-process evaluation failed".to_string(),
        },
    }
}

// ── Drill (assembles the ledger + gate) ──────────────────────────────────────

/// The deterministic chaos-drill engine.
#[derive(Debug, Clone)]
pub struct ChaosDrill {
    run_id: String,
    label: String,
}

impl ChaosDrill {
    /// Construct a drill with a deterministic run id derived from its label.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        let run_id = format!(
            "chaos-drill-{}",
            short_hash(&stable_hash(&format!(
                "{CHAOS_DRILL_SCHEMA_VERSION}|{label}"
            )))
        );
        Self { run_id, label }
    }

    /// The deterministic run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Run all chaos scenarios and assemble the report.
    #[must_use]
    pub fn run(&self) -> ChaosDrillReport {
        let mut ledger: Vec<ChaosLedgerEntry> = Vec::new();
        for kind in ChaosScenarioKind::ALL {
            let outcome = run_scenario(kind);
            let safe = outcome.action_path == kind.expected_path();
            let mut remediation = Vec::new();
            if !safe {
                remediation.push(format!(
                    "scenario '{}' did not degrade safely: expected '{}', observed '{}'",
                    kind.as_str(),
                    kind.expected_path(),
                    outcome.action_path
                ));
            }
            ledger.push(ChaosLedgerEntry {
                schema_version: CHAOS_DRILL_SCHEMA_VERSION.to_string(),
                run_id: self.run_id.clone(),
                chaos_scenario_id: kind.as_str().to_string(),
                kernel: kind.kernel(),
                policy_id: kind.kernel().policy_id(),
                claim_id: outcome.claim_id,
                action_path: outcome.action_path,
                expected_path: kind.expected_path().to_string(),
                guarantee_status: outcome.guarantee_status,
                fallback_reason: outcome.fallback_reason,
                rollback_verdict: outcome.rollback_verdict,
                baseline_restored: outcome.baseline_restored,
                safe_degradation_ok: safe,
                detail: outcome.detail,
                remediation,
                reproduction_command: format!(
                    "cargo run -p doctor_frankentui -- chaos-drill --label '{}' # run {}",
                    self.label, self.run_id
                ),
            });
        }

        let evidence_checksum = sha256_hex(render_ledger_jsonl(&ledger).as_bytes());
        let report_id = format!(
            "chaos-drill-report-{}",
            short_hash(&stable_hash(&format!(
                "{}|{evidence_checksum}",
                self.run_id
            )))
        );
        let summary = self.summarize(&ledger, &report_id, &evidence_checksum);
        let gate_passes = summary.gate_passes;
        let exported_json_stats = export_json_stats(&report_id, &summary, &ledger);
        let replay_command = format!(
            "cargo run -p doctor_frankentui -- chaos-drill --label '{}' # run {}",
            self.label, self.run_id
        );

        ChaosDrillReport {
            schema_version: CHAOS_DRILL_SCHEMA_VERSION.to_string(),
            report_id,
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum,
            ledger,
            summary,
            gate_passes,
            replay_command,
            exported_json_stats,
        }
    }

    fn summarize(
        &self,
        ledger: &[ChaosLedgerEntry],
        report_id: &str,
        evidence_checksum: &str,
    ) -> ChaosDrillSummary {
        let required_fields_complete = ledger.iter().all(required_fields_present);
        let safe_scenarios = ledger.iter().filter(|l| l.safe_degradation_ok).count();
        let unsafe_scenarios = ledger.len() - safe_scenarios;
        let all_safe = unsafe_scenarios == 0;
        // Each AC2 red-path family must be exercised and degrade safely.
        let degraded_families: std::collections::BTreeSet<&str> = ledger
            .iter()
            .filter(|l| l.safe_degradation_ok && l.chaos_scenario_id != "baseline_promote")
            .map(|l| l.chaos_scenario_id.as_str())
            .collect();
        let red_paths_covered = [
            "budget_exhaustion",
            "calibration_failure",
            "uncertainty_spike",
        ]
        .iter()
        .all(|s| degraded_families.contains(s));
        let rollback_restored = ledger
            .iter()
            .filter(|l| l.chaos_scenario_id == "performance_drift")
            .all(|l| l.action_path == "rollback" && l.baseline_restored);
        let kernels: std::collections::BTreeSet<&str> =
            ledger.iter().map(|l| l.kernel.as_str()).collect();
        let gate_passes =
            required_fields_complete && all_safe && red_paths_covered && rollback_restored;
        ChaosDrillSummary {
            schema_version: CHAOS_DRILL_SCHEMA_VERSION.to_string(),
            report_id: report_id.to_string(),
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum: evidence_checksum.to_string(),
            total_scenarios: ledger.len(),
            safe_scenarios,
            unsafe_scenarios,
            kernels_covered: kernels.len(),
            required_fields_complete,
            all_safe,
            red_paths_covered,
            rollback_restored,
            gate_passes,
            replay_command: format!(
                "cargo run -p doctor_frankentui -- chaos-drill --label '{}' # run {}",
                self.label, self.run_id
            ),
        }
    }
}

// ── Report + summary + stats ─────────────────────────────────────────────────

/// Machine-readable summary of one chaos-drill run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChaosDrillSummary {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum of the emitted ledger.
    pub evidence_checksum: String,
    /// Total chaos scenarios.
    pub total_scenarios: usize,
    /// Scenarios that degraded safely.
    pub safe_scenarios: usize,
    /// Scenarios that did NOT degrade safely (fail the gate).
    pub unsafe_scenarios: usize,
    /// Distinct kernels exercised.
    pub kernels_covered: usize,
    /// Whether every ledger line has all mandated fields (AC1).
    pub required_fields_complete: bool,
    /// Whether every scenario degraded safely.
    pub all_safe: bool,
    /// Whether the budget/calibration/uncertainty red paths all degraded safely (AC2).
    pub red_paths_covered: bool,
    /// Whether the performance-drift rollback restored the incumbent baseline.
    pub rollback_restored: bool,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChaosDrillStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The in-memory chaos-drill report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChaosDrillReport {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum of the emitted ledger.
    pub evidence_checksum: String,
    /// The emitted evidence ledger (float-free).
    pub ledger: Vec<ChaosLedgerEntry>,
    /// Aggregate summary.
    pub summary: ChaosDrillSummary,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: ChaosDrillStatsArtifact,
}

impl ChaosDrillReport {
    /// Render the evidence ledger as JSONL (one entry per line).
    #[must_use]
    pub fn render_ledger_jsonl(&self) -> String {
        render_ledger_jsonl(&self.ledger)
    }
}

fn export_json_stats(
    report_id: &str,
    summary: &ChaosDrillSummary,
    ledger: &[ChaosLedgerEntry],
) -> ChaosDrillStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        summary: &'a ChaosDrillSummary,
        ledger: &'a [ChaosLedgerEntry],
    }
    let content = match serde_json::to_string_pretty(&Export {
        schema_version: CHAOS_DRILL_SCHEMA_VERSION,
        report_id,
        summary,
        ledger,
    }) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    ChaosDrillStatsArtifact {
        path: format!("{report_id}/chaos_drill_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

/// Run the chaos drill over all scenarios with the given label.
#[must_use]
pub fn run_chaos_drill_report(label: &str) -> ChaosDrillReport {
    ChaosDrill::new(label).run()
}

// ── Pipeline (materialized artifacts) ────────────────────────────────────────

/// Configuration for the materialized chaos-drill pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct ChaosDrillPipelineConfig {
    /// Run directory name under the run-root.
    pub run_name: String,
    /// Run label used for deterministic ids.
    pub label: String,
}

impl Default for ChaosDrillPipelineConfig {
    fn default() -> Self {
        Self {
            run_name: "chaos_drill".to_string(),
            label: "chaos-drill/e2e".to_string(),
        }
    }
}

/// A materialized pipeline artifact (path + integrity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChaosDrillArtifact {
    /// Logical artifact name.
    pub name: String,
    /// Relative file name within the run directory.
    pub file: String,
    /// SHA-256 of the file content.
    pub sha256: String,
    /// Byte length of the file content.
    pub bytes: u64,
}

/// The outcome of running and materializing the pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChaosDrillPipelineOutcome {
    /// Absolute run directory.
    pub run_dir: String,
    /// Absolute path to the JSONL evidence ledger.
    pub ledger_path: String,
    /// Absolute path to the pipeline summary JSON.
    pub summary_path: String,
    /// Absolute path to the artifact manifest JSON.
    pub manifest_path: String,
    /// Absolute path to the JSON-stats artifact.
    pub stats_path: String,
    /// The machine-readable summary.
    pub summary: ChaosDrillSummary,
    /// All generated artifacts (with integrity hashes).
    pub artifacts: Vec<ChaosDrillArtifact>,
}

fn artifact_of(file: &str, content: &str) -> ChaosDrillArtifact {
    ChaosDrillArtifact {
        name: file
            .trim_end_matches(".json")
            .trim_end_matches(".jsonl")
            .to_string(),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// Run the chaos drill and materialize the full evidence pipeline under
/// `run_root/<run_name>/`.
///
/// # Errors
/// Returns an error if a run directory or artifact cannot be created/serialized.
pub fn run_chaos_drill_pipeline(
    run_root: &Path,
    config: &ChaosDrillPipelineConfig,
) -> crate::error::Result<ChaosDrillPipelineOutcome> {
    let report = ChaosDrill::new(&config.label).run();

    let ledger_content = report.render_ledger_jsonl();
    let stats_content = report.exported_json_stats.content.clone();
    let summary_content = serde_json::to_string_pretty(&report.summary)?;

    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let ledger_file = "evidence_ledger.jsonl";
    let stats_file = "chaos_drill_stats.json";
    let summary_file = "pipeline_summary.json";
    let manifest_file = "artifact_manifest.json";

    crate::util::write_string(&run_dir.join(ledger_file), &ledger_content)?;
    crate::util::write_string(&run_dir.join(stats_file), &stats_content)?;
    crate::util::write_string(&run_dir.join(summary_file), &summary_content)?;

    let artifacts = vec![
        artifact_of(ledger_file, &ledger_content),
        artifact_of(stats_file, &stats_content),
        artifact_of(summary_file, &summary_content),
    ];

    #[derive(Serialize)]
    struct Manifest<'a> {
        schema_version: &'a str,
        run_name: &'a str,
        report_id: &'a str,
        gate_passes: bool,
        artifacts: &'a [ChaosDrillArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: CHAOS_DRILL_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        gate_passes: report.gate_passes,
        artifacts: &artifacts,
    })?;
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(ChaosDrillPipelineOutcome {
        run_dir: run_dir.display().to_string(),
        ledger_path: run_dir.join(ledger_file).display().to_string(),
        summary_path: run_dir.join(summary_file).display().to_string(),
        manifest_path: run_dir.join(manifest_file).display().to_string(),
        stats_path: run_dir.join(stats_file).display().to_string(),
        summary: report.summary,
        artifacts,
    })
}

// ── CLI ──────────────────────────────────────────────────────────────────────

/// CLI arguments for the `chaos-drill` command.
#[derive(Debug, clap::Args)]
pub struct ChaosDrillArgs {
    /// Run-root directory; artifacts land under `<run-root>/<run-name>/`.
    #[arg(
        long = "run-root",
        default_value = "/tmp/doctor_frankentui/chaos_drill"
    )]
    pub run_root: PathBuf,

    /// Run directory name.
    #[arg(long = "run-name", default_value = "chaos_drill")]
    pub run_name: String,

    /// Run label used for deterministic ids.
    #[arg(long = "label", default_value = "chaos-drill/e2e")]
    pub label: String,
}

/// Run the `chaos-drill` command: materialize the chaos pipeline and apply the
/// safe-degradation gate.
///
/// # Errors
/// Returns [`crate::error::DoctorError::Exit`] with a non-zero code when the gate
/// fails (an unsafe scenario, a missing field, an uncovered red path, or a
/// non-restoring rollback), or an I/O error if artifacts cannot be materialized.
pub fn run_chaos_drill(args: ChaosDrillArgs) -> crate::error::Result<()> {
    let config = ChaosDrillPipelineConfig {
        run_name: args.run_name,
        label: args.label,
    };
    let outcome = run_chaos_drill_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("reverse-round chaos drill"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!(
            "scenarios: {} | safe: {} | unsafe: {} | kernels: {}",
            summary.total_scenarios,
            summary.safe_scenarios,
            summary.unsafe_scenarios,
            summary.kernels_covered
        ));
        ui.info(&format!(
            "red paths covered: {} | rollback restored: {}",
            summary.red_paths_covered, summary.rollback_restored
        ));
        if summary.gate_passes {
            ui.success("chaos-drill gate PASSED (all scenarios degraded safely)");
        } else {
            ui.error("chaos-drill gate FAILED");
        }
    }

    if summary.gate_passes {
        Ok(())
    } else {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "chaos-drill gate failed: unsafe_scenarios={}, required_fields_complete={}, red_paths_covered={}, rollback_restored={}",
                summary.unsafe_scenarios,
                summary.required_fields_complete,
                summary.red_paths_covered,
                summary.rollback_restored
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scenario_line<'a>(report: &'a ChaosDrillReport, id: &str) -> &'a ChaosLedgerEntry {
        report
            .ledger
            .iter()
            .find(|l| l.chaos_scenario_id == id)
            .expect("scenario present")
    }

    #[test]
    fn drill_gate_passes_and_covers_all_scenarios() {
        let report = run_chaos_drill_report("chaos/test");
        assert!(report.gate_passes, "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_scenarios, ChaosScenarioKind::ALL.len());
        assert_eq!(report.summary.unsafe_scenarios, 0);
        assert_eq!(report.summary.kernels_covered, 3);
        assert!(report.summary.red_paths_covered);
        assert!(report.summary.rollback_restored);
        assert!(report.ledger.iter().all(required_fields_present));
    }

    #[test]
    fn every_scenario_degrades_to_its_expected_path() {
        let report = run_chaos_drill_report("chaos/test");
        for kind in ChaosScenarioKind::ALL {
            let line = scenario_line(&report, kind.as_str());
            assert_eq!(
                line.action_path,
                kind.expected_path(),
                "scenario {} took the wrong path",
                kind.as_str()
            );
            assert!(line.safe_degradation_ok);
        }
    }

    #[test]
    fn multi_lever_merge_is_blocked() {
        let report = run_chaos_drill_report("chaos/test");
        let line = scenario_line(&report, "multi_lever_merge");
        assert_eq!(line.action_path, "blocked");
        assert_eq!(line.kernel, ChaosKernel::ReverseRound);
    }

    #[test]
    fn contradictory_evidence_is_blocked_not_rolled_back() {
        // A non-isomorphic lever is blocked by the isomorphism gate; the
        // hold-no-rollback policy escalates rather than auto-reverting.
        let report = run_chaos_drill_report("chaos/test");
        let line = scenario_line(&report, "contradictory_evidence");
        assert_eq!(line.action_path, "blocked");
        assert_eq!(line.fallback_reason, "isomorphism_violation");
        assert_eq!(line.kernel, ChaosKernel::ReverseRound);
    }

    #[test]
    fn performance_drift_rolls_back_and_restores_baseline() {
        let report = run_chaos_drill_report("chaos/test");
        let line = scenario_line(&report, "performance_drift");
        assert_eq!(line.action_path, "rollback");
        assert_eq!(line.rollback_verdict, "rollback_triggered");
        assert!(line.baseline_restored);
    }

    #[test]
    fn budget_exhaustion_defers() {
        let report = run_chaos_drill_report("chaos/test");
        let line = scenario_line(&report, "budget_exhaustion");
        assert_eq!(line.action_path, "defer");
        assert_eq!(line.fallback_reason, "budget_risk");
    }

    #[test]
    fn uncertainty_and_drift_fall_back_conservatively() {
        let report = run_chaos_drill_report("chaos/test");
        let unc = scenario_line(&report, "uncertainty_spike");
        assert_eq!(unc.action_path, "conservative");
        assert_eq!(unc.fallback_reason, "uncertainty");
        let drift = scenario_line(&report, "portfolio_drift");
        assert_eq!(drift.action_path, "conservative");
        assert_eq!(drift.fallback_reason, "drift");
    }

    #[test]
    fn calibration_failure_triggers_guarantee_fallback() {
        let report = run_chaos_drill_report("chaos/test");
        let line = scenario_line(&report, "calibration_failure");
        assert_eq!(line.action_path, "fallback");
        assert_eq!(line.guarantee_status, "fallback");
        assert_ne!(line.fallback_reason, "none");
    }

    #[test]
    fn optional_stopping_holds_without_false_discovery() {
        let report = run_chaos_drill_report("chaos/test");
        let line = scenario_line(&report, "optional_stopping");
        assert_eq!(line.action_path, "holds");
        assert_eq!(line.guarantee_status, "holds");
    }

    #[test]
    fn baseline_change_still_promotes() {
        // The gate is not vacuously green: a safe change is not blocked.
        let report = run_chaos_drill_report("chaos/test");
        let line = scenario_line(&report, "baseline_promote");
        assert_eq!(line.action_path, "promote");
        // No rollback occurred, so no baseline was restored (a registered
        // comparator id alone must not read as a restoration).
        assert!(!line.baseline_restored);
        assert_eq!(line.rollback_verdict, "no_rollback_needed");
    }

    #[test]
    fn report_is_deterministic_and_replay_identical() {
        let a = run_chaos_drill_report("chaos/test");
        let b = run_chaos_drill_report("chaos/test");
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.ledger, b.ledger);
        assert_eq!(a.render_ledger_jsonl(), b.render_ledger_jsonl());
    }

    #[test]
    fn pipeline_materializes_consistent_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let outcome =
            run_chaos_drill_pipeline(dir.path(), &ChaosDrillPipelineConfig::default()).unwrap();
        assert!(outcome.summary.gate_passes);
        for artifact in &outcome.artifacts {
            let path = std::path::Path::new(&outcome.run_dir).join(&artifact.file);
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(sha256_hex(&bytes), artifact.sha256);
        }
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = run_chaos_drill_report("chaos/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }
}
