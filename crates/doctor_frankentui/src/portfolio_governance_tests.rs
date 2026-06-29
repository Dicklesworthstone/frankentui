//! Cross-kernel unit/property test-evidence harness for the reverse-round
//! governance loop and the expected-loss portfolio scheduler (bd-3bxhj.10.19).
//!
//! Two pure, deterministic decision kernels gate how the migration program
//! promotes optimizations and allocates alien primitives:
//!
//! - the reverse-round one-lever governance gate
//!   ([`crate::reverse_round_governance`]) — comparator registration, one-lever-
//!   per-window enforcement, golden-isomorphism proof, percentile-budget checks,
//!   and deterministic rollback resolution;
//! - the expected-loss portfolio scheduler ([`crate::portfolio_scheduler`]) —
//!   per-milestone argmin-expected-loss selection, branch-diversity, budget
//!   safety, and a deterministic conservative fallback under uncertainty/drift/
//!   budget risk.
//!
//! Each kernel already ships its own inline unit tests. This module adds the
//! *cross-kernel* contract the bead asks for: a single, host-agnostic
//! [`GovernanceDiagnostic`] envelope that normalizes every decision into one
//! structured schema, a library of green/red fixtures with *explicit expected
//! outcomes*, and a deterministic [`PortfolioGovernanceValidationReport`] that the
//! downstream E2E chaos-drill / portfolio-fallback gauntlet (bd-3bxhj.10.20) can
//! consume without adapter drift.
//!
//! Every diagnostic carries the exact fields the bead's acceptance criterion 3
//! mandates for failure logs:
//!
//! - `run_id` (the deterministic report/run identity),
//! - `claim_id` (the uplift claim id / milestone id),
//! - `comparator_id` (the registered baseline comparator, or `n/a`),
//! - `action_id` (the optimization-lever id / selected primitive id),
//! - `equation_id` (the technique/equation the lever realizes, or `n/a`),
//! - `posterior_hash` (a stable fingerprint of the governing posterior),
//! - `loss_components` (the expected-loss decomposition, fixed-decimal),
//! - `fallback_reason` (the rollback decision / safety trigger).
//!
//! These eight are projected verbatim by [`GovernanceDiagnostic::failure_log`]
//! into the [`GovernanceFailureLog`] record the E2E script ingests.
//!
//! Beyond raw diagnostics, every fixture is paired with an expected-outcome
//! oracle ([`ExpectedReverseRoundOutcome`] / [`ExpectedPortfolioOutcome`]) and the
//! harness emits an [`OutcomeVerdict`] comparing actual behavior to the
//! expectation. The whole harness is pure and deterministic (no RNG, no clock,
//! no filesystem): the JSON-stats artifact is part of the in-memory report, so a
//! repeated run is byte-identical.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decision_loss_policy::{RiskTier, action_str};
use crate::golden_isomorphism::ChecksumVerdict;
use crate::milestone_policy::{MathFamily, QualityBar};
use crate::portfolio_scheduler::{
    PortfolioScheduler, PortfolioSchedulerConfig, PortfolioSchedulerReport, PrimitiveCandidate,
    ScheduleStage, SchedulerMilestone, default_milestone_portfolio,
};
use crate::posterior_core::{
    ChannelEvidence, EvidenceChannel, PosteriorCoreConfig, PosteriorEngine, PosteriorRecord,
};
use crate::reverse_round_governance::{
    BaselineComparator, ComparatorRegistry, DecisionWindow, MetricFamily, OptimizationLever,
    PercentileDeltas, ReverseRoundFindingCode, ReverseRoundGate, ReverseRoundGovernanceReport,
    ReverseRoundRequest, RollbackPolicy, UpliftClaim,
};
use crate::test_budget_allocator::BetaPosterior;

/// Schema version for the portfolio-governance validation report.
pub const PORTFOLIO_GOVERNANCE_TESTS_SCHEMA_VERSION: &str = "portfolio-governance-tests-v1";

// ── Hashing / formatting helpers ─────────────────────────────────────────────

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

/// Deterministic fixed-decimal rendering (six fractional digits, `-0.0`/non-finite
/// normalized) so loss components stay float-free and the diagnostics derive `Eq`.
fn fmt6(x: f64) -> String {
    if !x.is_finite() {
        return "0.000000".to_string();
    }
    let rendered = format!("{x:.6}");
    if rendered == "-0.000000" {
        "0.000000".to_string()
    } else {
        rendered
    }
}

// ── Unified diagnostic envelope ──────────────────────────────────────────────

/// Which decision kernel produced a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GovernanceKernel {
    /// The reverse-round one-lever governance gate.
    ReverseRound,
    /// The expected-loss portfolio scheduler.
    Portfolio,
}

impl GovernanceKernel {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReverseRound => "reverse_round",
            Self::Portfolio => "portfolio",
        }
    }
}

/// A single normalized governance diagnostic.
///
/// This is the structured schema contract consumed by the downstream E2E
/// chaos-drill / portfolio-fallback gauntlet. Every field is always populated, so
/// failure logs are forensically rich regardless of which kernel produced them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceDiagnostic {
    /// Which kernel produced this diagnostic.
    pub kernel: GovernanceKernel,
    /// Deterministic run/report identity.
    pub run_id: String,
    /// The uplift claim id / milestone id.
    pub claim_id: String,
    /// The registered baseline comparator id (`n/a` for portfolio).
    pub comparator_id: String,
    /// The optimization-lever id / selected primitive id.
    pub action_id: String,
    /// The technique/equation the lever realizes (`n/a` for portfolio).
    pub equation_id: String,
    /// A stable fingerprint of the governing posterior.
    pub posterior_hash: String,
    /// The expected-loss decomposition (fixed-decimal terms, float-free).
    pub loss_components: Vec<String>,
    /// The rollback decision / safety trigger.
    pub fallback_reason: String,
    /// The decision outcome (verdict / decision label).
    pub outcome: String,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic single-command replay reference.
    pub replay_cmd: String,
}

impl GovernanceDiagnostic {
    /// Whether every required failure-log field is populated and non-empty (AC3).
    #[must_use]
    pub fn has_required_fields(&self) -> bool {
        !self.run_id.is_empty()
            && !self.claim_id.is_empty()
            && !self.comparator_id.is_empty()
            && !self.action_id.is_empty()
            && !self.equation_id.is_empty()
            && !self.posterior_hash.is_empty()
            && !self.loss_components.is_empty()
            && self.loss_components.iter().all(|c| !c.is_empty())
            && !self.fallback_reason.is_empty()
            && !self.outcome.is_empty()
            && !self.detail.is_empty()
            && !self.replay_cmd.is_empty()
    }

    /// Project this diagnostic into the bead-mandated failure-log record.
    #[must_use]
    pub fn failure_log(&self) -> GovernanceFailureLog {
        GovernanceFailureLog {
            run_id: self.run_id.clone(),
            claim_id: self.claim_id.clone(),
            comparator_id: self.comparator_id.clone(),
            action_id: self.action_id.clone(),
            equation_id: self.equation_id.clone(),
            posterior_hash: self.posterior_hash.clone(),
            loss_components: self.loss_components.clone(),
            fallback_reason: self.fallback_reason.clone(),
        }
    }
}

/// The exact failure-log schema mandated by acceptance criterion 3, consumed by
/// the bd-3bxhj.10.20 E2E chaos-drill / portfolio-fallback script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceFailureLog {
    pub run_id: String,
    pub claim_id: String,
    pub comparator_id: String,
    pub action_id: String,
    pub equation_id: String,
    pub posterior_hash: String,
    pub loss_components: Vec<String>,
    pub fallback_reason: String,
}

// ── Expected-outcome oracles ─────────────────────────────────────────────────

/// The expected outcome for one reverse-round fixture.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedReverseRoundOutcome {
    /// Whether the gate must pass (no blocking findings).
    pub gate_passes: bool,
    /// Finding codes that must appear at least once.
    pub required_codes: Vec<ReverseRoundFindingCode>,
    /// Finding codes that must never appear.
    pub forbidden_codes: Vec<ReverseRoundFindingCode>,
}

/// The expected outcome for one portfolio fixture.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedPortfolioOutcome {
    /// Whether the gate must pass.
    pub gate_passes: bool,
    /// Whether at least one safety-mode (conservative) event must fire.
    pub expect_conservative: bool,
    /// Whether a branch-diversity violation must be surfaced.
    pub expect_diversity_violation: bool,
    /// Whether at least one malformed candidate must be flagged.
    pub expect_invalid: bool,
    /// Minimum number of committed selections.
    pub min_final_selected: usize,
}

/// The verdict comparing one fixture's actual behavior to its expectation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeVerdict {
    /// The fixture's label.
    pub fixture_label: String,
    /// Which kernel the fixture exercises.
    pub kernel: GovernanceKernel,
    /// Whether the actual behavior met the expectation.
    pub expectation_met: bool,
    /// Detail on any mismatch (`ok` when met).
    pub detail: String,
}

// ── Fixtures ─────────────────────────────────────────────────────────────────

/// A reverse-round fixture: a request plus its expected outcome.
#[derive(Debug, Clone)]
pub struct ReverseRoundFixture {
    /// Stable fixture label.
    pub label: String,
    /// The reverse-round governance request.
    pub request: ReverseRoundRequest,
    /// The expected outcome oracle.
    pub expected: ExpectedReverseRoundOutcome,
}

/// A portfolio fixture: a milestone portfolio + config plus its expected outcome.
#[derive(Debug, Clone)]
pub struct PortfolioFixture {
    /// Stable fixture label.
    pub label: String,
    /// The scheduler configuration.
    pub config: PortfolioSchedulerConfig,
    /// The milestone portfolio.
    pub milestones: Vec<SchedulerMilestone>,
    /// The expected outcome oracle.
    pub expected: ExpectedPortfolioOutcome,
}

fn posterior_for(claim_id: &str) -> PosteriorRecord {
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

fn green_claim(claim_id: &str) -> UpliftClaim {
    UpliftClaim::new(
        claim_id,
        format!("cmp-{claim_id}"),
        OptimizationLever::new(
            format!("lever-{claim_id}"),
            "graveyard-eq-001",
            "swap inner loop to SAT tile-skip",
        ),
        posterior_for(claim_id),
        ChecksumVerdict::Match,
        PercentileDeltas::improvement(0.10),
        RollbackPolicy::AutomaticOnRegression,
    )
}

/// A green reverse-round request: one registered claim, one window, one enabled
/// lever, byte-identical isomorphism, improving percentiles.
#[must_use]
pub fn green_reverse_round_request(claim_id: &str) -> ReverseRoundRequest {
    let registry = registered_comparator(claim_id);
    let claim = green_claim(claim_id);
    let window = DecisionWindow::new("rrg-window-green", vec![claim_id.to_string()]);
    ReverseRoundRequest::new(registry, vec![window], vec![claim])
}

/// The reverse-round fixture corpus: one green path and four red paths, each with
/// an explicit expected-outcome oracle.
#[must_use]
pub fn reverse_round_fixture_corpus() -> Vec<ReverseRoundFixture> {
    let mut out = Vec::new();

    // Green.
    out.push(ReverseRoundFixture {
        label: "reverse_round/green".to_string(),
        request: green_reverse_round_request("rr-green"),
        expected: ExpectedReverseRoundOutcome {
            gate_passes: true,
            required_codes: Vec::new(),
            forbidden_codes: vec![
                ReverseRoundFindingCode::MultiLeverWithoutOverride,
                ReverseRoundFindingCode::UnregisteredComparator,
                ReverseRoundFindingCode::IsomorphismViolation,
                ReverseRoundFindingCode::PercentileRegressionBeyondBudget,
            ],
        },
    });

    // Red: two enabled levers in one window without an override.
    {
        let claim_a = green_claim("rr-multi-a");
        let claim_b = UpliftClaim::new(
            "rr-multi-b",
            "cmp-rr-multi-b",
            OptimizationLever::new("lever-rr-multi-b", "graveyard-eq-002", "second lever"),
            posterior_for("rr-multi-b"),
            ChecksumVerdict::Match,
            PercentileDeltas::improvement(0.08),
            RollbackPolicy::AutomaticOnRegression,
        );
        let mut registry = registered_comparator("rr-multi-a");
        let _ = registry.register(BaselineComparator::new(
            "cmp-rr-multi-b",
            "rr-multi-b",
            "baseline-rr-multi-b",
            MetricFamily::Latency,
        ));
        let window = DecisionWindow::new(
            "rrg-window-multi",
            vec!["rr-multi-a".to_string(), "rr-multi-b".to_string()],
        );
        out.push(ReverseRoundFixture {
            label: "reverse_round/multi_lever".to_string(),
            request: ReverseRoundRequest::new(registry, vec![window], vec![claim_a, claim_b]),
            expected: ExpectedReverseRoundOutcome {
                gate_passes: false,
                required_codes: vec![ReverseRoundFindingCode::MultiLeverWithoutOverride],
                forbidden_codes: Vec::new(),
            },
        });
    }

    // Red: claim references a comparator that is not registered.
    {
        let claim = green_claim("rr-unreg");
        let window = DecisionWindow::new("rrg-window-unreg", vec!["rr-unreg".to_string()]);
        out.push(ReverseRoundFixture {
            label: "reverse_round/unregistered_comparator".to_string(),
            request: ReverseRoundRequest::new(ComparatorRegistry::new(), vec![window], vec![claim]),
            expected: ExpectedReverseRoundOutcome {
                gate_passes: false,
                required_codes: vec![ReverseRoundFindingCode::UnregisteredComparator],
                forbidden_codes: Vec::new(),
            },
        });
    }

    // Red: the lever's isomorphism proof is a behavior mismatch.
    {
        let claim = UpliftClaim::new(
            "rr-iso",
            "cmp-rr-iso",
            OptimizationLever::new("lever-rr-iso", "graveyard-eq-003", "unsafe rewrite"),
            posterior_for("rr-iso"),
            ChecksumVerdict::Mismatch,
            PercentileDeltas::improvement(0.05),
            RollbackPolicy::AutomaticOnRegression,
        );
        let window = DecisionWindow::new("rrg-window-iso", vec!["rr-iso".to_string()]);
        out.push(ReverseRoundFixture {
            label: "reverse_round/isomorphism_mismatch".to_string(),
            request: ReverseRoundRequest::new(
                registered_comparator("rr-iso"),
                vec![window],
                vec![claim],
            ),
            expected: ExpectedReverseRoundOutcome {
                gate_passes: false,
                required_codes: vec![ReverseRoundFindingCode::IsomorphismViolation],
                forbidden_codes: Vec::new(),
            },
        });
    }

    // Red: percentile regression beyond budget (a real performance regression).
    {
        let claim = UpliftClaim::new(
            "rr-perf",
            "cmp-rr-perf",
            OptimizationLever::new("lever-rr-perf", "graveyard-eq-004", "slower variant"),
            posterior_for("rr-perf"),
            ChecksumVerdict::Match,
            PercentileDeltas::new(0.6, 0.6, 0.6),
            RollbackPolicy::AutomaticOnRegression,
        );
        let window = DecisionWindow::new("rrg-window-perf", vec!["rr-perf".to_string()]);
        out.push(ReverseRoundFixture {
            label: "reverse_round/percentile_regression".to_string(),
            request: ReverseRoundRequest::new(
                registered_comparator("rr-perf"),
                vec![window],
                vec![claim],
            ),
            expected: ExpectedReverseRoundOutcome {
                gate_passes: false,
                required_codes: vec![ReverseRoundFindingCode::PercentileRegressionBeyondBudget],
                forbidden_codes: Vec::new(),
            },
        });
    }

    out
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

/// The portfolio fixture corpus: a green portfolio plus adversarial-uncertainty,
/// malformed, and branch-diversity-violation red paths.
#[must_use]
pub fn portfolio_fixture_corpus() -> Vec<PortfolioFixture> {
    let mut out = Vec::new();

    // Green: the canonical balanced portfolio.
    out.push(PortfolioFixture {
        label: "portfolio/green".to_string(),
        config: PortfolioSchedulerConfig::default(),
        milestones: default_milestone_portfolio(),
        expected: ExpectedPortfolioOutcome {
            gate_passes: true,
            expect_conservative: false,
            expect_diversity_violation: false,
            expect_invalid: false,
            min_final_selected: 4,
        },
    });

    // Adversarial uncertainty: a high-variance milestone forces a surfaced
    // conservative fallback (the gate still passes — the event is surfaced).
    out.push(PortfolioFixture {
        label: "portfolio/adversarial_uncertainty".to_string(),
        config: PortfolioSchedulerConfig::default(),
        milestones: vec![
            SchedulerMilestone::new(
                "m.confident",
                QualityBar::Gold,
                RiskTier::Medium,
                vec![candidate(
                    "confident",
                    "m.confident",
                    MathFamily::Symbolic,
                    46.0,
                    4.0,
                    2.0,
                )],
            ),
            SchedulerMilestone::new(
                "m.uncertain",
                QualityBar::Bronze,
                RiskTier::Medium,
                vec![candidate(
                    "uncertain",
                    "m.uncertain",
                    MathFamily::FormalAnalysis,
                    2.0,
                    2.0,
                    2.0,
                )],
            ),
        ],
        expected: ExpectedPortfolioOutcome {
            gate_passes: true,
            expect_conservative: true,
            expect_diversity_violation: false,
            expect_invalid: false,
            min_final_selected: 2,
        },
    });

    // Malformed: a non-positive posterior is rejected (gate fails closed).
    {
        let mut bad = candidate(
            "bad",
            "m.malformed",
            MathFamily::FormalAnalysis,
            40.0,
            4.0,
            2.0,
        );
        bad.posterior = BetaPosterior {
            alpha: -1.0,
            beta: 4.0,
        };
        // A second, different-family valid milestone keeps the committed set
        // diversity-balanced so this fixture isolates the malformed-candidate
        // failure (a single-milestone portfolio would also trip the diversity cap).
        out.push(PortfolioFixture {
            label: "portfolio/malformed".to_string(),
            config: PortfolioSchedulerConfig::default(),
            milestones: vec![
                SchedulerMilestone::new(
                    "m.malformed",
                    QualityBar::Bronze,
                    RiskTier::Medium,
                    vec![
                        candidate("good", "m.malformed", MathFamily::Symbolic, 40.0, 4.0, 2.0),
                        bad,
                    ],
                ),
                SchedulerMilestone::new(
                    "m.other",
                    QualityBar::Bronze,
                    RiskTier::Medium,
                    vec![candidate(
                        "other",
                        "m.other",
                        MathFamily::FormalAnalysis,
                        40.0,
                        4.0,
                        2.0,
                    )],
                ),
            ],
            expected: ExpectedPortfolioOutcome {
                gate_passes: false,
                expect_conservative: false,
                expect_diversity_violation: false,
                expect_invalid: true,
                min_final_selected: 1,
            },
        });
    }

    // Branch-diversity violation: two milestones whose argmins both land in the
    // Symbolic family breach the diversity cap (gate fails, violation surfaced).
    out.push(PortfolioFixture {
        label: "portfolio/diversity_violation".to_string(),
        config: PortfolioSchedulerConfig::default(),
        milestones: vec![
            SchedulerMilestone::new(
                "m1",
                QualityBar::Gold,
                RiskTier::Medium,
                vec![
                    candidate("sym1", "m1", MathFamily::Symbolic, 46.0, 4.0, 2.0),
                    candidate("prob1", "m1", MathFamily::Probabilistic, 12.0, 10.0, 2.0),
                ],
            ),
            SchedulerMilestone::new(
                "m2",
                QualityBar::Gold,
                RiskTier::Medium,
                vec![
                    candidate("sym2", "m2", MathFamily::Symbolic, 45.0, 5.0, 2.0),
                    candidate("prob2", "m2", MathFamily::Probabilistic, 12.0, 10.0, 2.0),
                ],
            ),
        ],
        expected: ExpectedPortfolioOutcome {
            gate_passes: false,
            expect_conservative: false,
            expect_diversity_violation: true,
            expect_invalid: false,
            min_final_selected: 2,
        },
    });

    out
}

// ── Evaluation (fixture -> diagnostics + verdict) ────────────────────────────

/// The result of evaluating one reverse-round fixture.
#[derive(Debug, Clone)]
pub struct ReverseRoundFixtureEvaluation {
    /// The fixture label.
    pub label: String,
    /// Normalized diagnostics (one per claim).
    pub diagnostics: Vec<GovernanceDiagnostic>,
    /// The expectation verdict.
    pub verdict: OutcomeVerdict,
    /// The underlying gate report.
    pub report: ReverseRoundGovernanceReport,
}

/// Evaluate one reverse-round fixture into diagnostics + a verdict.
#[must_use]
pub fn evaluate_reverse_round_fixture(
    fixture: &ReverseRoundFixture,
) -> ReverseRoundFixtureEvaluation {
    let report = ReverseRoundGate::default().evaluate(&fixture.request);

    let posterior_by_claim: std::collections::BTreeMap<&str, &PosteriorRecord> = fixture
        .request
        .claims
        .iter()
        .map(|c| (c.claim_id.as_str(), &c.posterior))
        .collect();

    let mut diagnostics: Vec<GovernanceDiagnostic> = Vec::new();
    for log in &report.decision_logs {
        let ledger = report.ledger_entry(&log.claim_id);
        let action = ledger.map_or("n/a".to_string(), |e| {
            action_str(e.expected_loss_action).to_string()
        });
        let posterior_hash = posterior_by_claim
            .get(log.claim_id.as_str())
            .map_or_else(|| short_hash(&log.claim_id), |p| stable_hash(*p));
        diagnostics.push(GovernanceDiagnostic {
            kernel: GovernanceKernel::ReverseRound,
            run_id: report.report_id.clone(),
            claim_id: log.claim_id.clone(),
            comparator_id: non_empty(&log.comparator_id),
            action_id: non_empty(&log.lever_id),
            equation_id: non_empty(&log.equation_id),
            posterior_hash,
            loss_components: vec![
                format!("action={action}"),
                format!("p50={}", fmt6(log.p50_delta)),
                format!("p95={}", fmt6(log.p95_delta)),
                format!("p99={}", fmt6(log.p99_delta)),
            ],
            fallback_reason: log.rollback_decision.as_str().to_string(),
            outcome: log.isomorphism_verdict.clone(),
            detail: format!("promotable={}", ledger.is_none_or(|e| e.promotable)),
            replay_cmd: report.replay_command.clone(),
        });
    }
    // Fixtures with no claims (impossible here) would still need one diagnostic;
    // every fixture in the corpus has at least one claim, so the gate-level
    // diagnostic is emitted when there are zero decision logs.
    if diagnostics.is_empty() {
        diagnostics.push(gate_level_reverse_round_diagnostic(&report));
    }

    let verdict = reverse_round_verdict(fixture, &report);
    ReverseRoundFixtureEvaluation {
        label: fixture.label.clone(),
        diagnostics,
        verdict,
        report,
    }
}

fn gate_level_reverse_round_diagnostic(
    report: &ReverseRoundGovernanceReport,
) -> GovernanceDiagnostic {
    GovernanceDiagnostic {
        kernel: GovernanceKernel::ReverseRound,
        run_id: report.report_id.clone(),
        claim_id: "n/a".to_string(),
        comparator_id: "n/a".to_string(),
        action_id: "n/a".to_string(),
        equation_id: "n/a".to_string(),
        posterior_hash: short_hash(&report.report_id),
        loss_components: vec!["action=n/a".to_string()],
        fallback_reason: "no_claims".to_string(),
        outcome: format!("gate_passes={}", report.gate_passes),
        detail: "empty reverse-round request".to_string(),
        replay_cmd: report.replay_command.clone(),
    }
}

fn reverse_round_verdict(
    fixture: &ReverseRoundFixture,
    report: &ReverseRoundGovernanceReport,
) -> OutcomeVerdict {
    let mut problems: Vec<String> = Vec::new();
    if report.gate_passes != fixture.expected.gate_passes {
        problems.push(format!(
            "gate_passes={} expected={}",
            report.gate_passes, fixture.expected.gate_passes
        ));
    }
    let present: std::collections::BTreeSet<ReverseRoundFindingCode> =
        report.findings.iter().map(|f| f.code).collect();
    for code in &fixture.expected.required_codes {
        if !present.contains(code) {
            problems.push(format!("missing required finding {}", code.as_str()));
        }
    }
    for code in &fixture.expected.forbidden_codes {
        if present.contains(code) {
            problems.push(format!("forbidden finding present {}", code.as_str()));
        }
    }
    OutcomeVerdict {
        fixture_label: fixture.label.clone(),
        kernel: GovernanceKernel::ReverseRound,
        expectation_met: problems.is_empty(),
        detail: if problems.is_empty() {
            "ok".to_string()
        } else {
            problems.join("; ")
        },
    }
}

/// The result of evaluating one portfolio fixture.
#[derive(Debug, Clone)]
pub struct PortfolioFixtureEvaluation {
    /// The fixture label.
    pub label: String,
    /// Normalized diagnostics (one per governed milestone).
    pub diagnostics: Vec<GovernanceDiagnostic>,
    /// The expectation verdict.
    pub verdict: OutcomeVerdict,
    /// The underlying scheduler report.
    pub report: PortfolioSchedulerReport,
}

/// Evaluate one portfolio fixture into diagnostics + a verdict.
#[must_use]
pub fn evaluate_portfolio_fixture(fixture: &PortfolioFixture) -> PortfolioFixtureEvaluation {
    let report = PortfolioScheduler::new(
        format!("portfolio-governance/{}", fixture.label),
        fixture.config,
        fixture.milestones.clone(),
    )
    .run(None);

    let mut diagnostics: Vec<GovernanceDiagnostic> = Vec::new();
    for line in report
        .ledger
        .iter()
        .filter(|l| l.stage == ScheduleStage::Govern)
    {
        let posterior_hash = sha256_hex(
            format!(
                "{}|{}|{}",
                line.milestone_id, line.posterior_mean, line.posterior_variance
            )
            .as_bytes(),
        );
        diagnostics.push(GovernanceDiagnostic {
            kernel: GovernanceKernel::Portfolio,
            run_id: report.run_id.clone(),
            claim_id: non_empty(&line.milestone_id),
            comparator_id: "n/a".to_string(),
            action_id: non_empty(&line.primitive_id),
            equation_id: "n/a".to_string(),
            posterior_hash,
            loss_components: vec![
                format!("action={}", line.selected_action),
                format!("expected_loss={}", line.expected_loss),
                format!("worst_case={}", line.worst_case_loss),
                format!("posterior_mean={}", line.posterior_mean),
            ],
            fallback_reason: line.safety_trigger.as_str().to_string(),
            outcome: line.decision.as_str().to_string(),
            detail: non_empty(&line.detail),
            replay_cmd: non_empty(&line.reproduction_command),
        });
    }
    if diagnostics.is_empty() {
        diagnostics.push(GovernanceDiagnostic {
            kernel: GovernanceKernel::Portfolio,
            run_id: report.run_id.clone(),
            claim_id: "portfolio".to_string(),
            comparator_id: "n/a".to_string(),
            action_id: "n/a".to_string(),
            equation_id: "n/a".to_string(),
            posterior_hash: short_hash(&report.run_id),
            loss_components: vec!["action=n/a".to_string()],
            fallback_reason: "none".to_string(),
            outcome: format!("gate_passes={}", report.gate_passes),
            detail: "empty portfolio".to_string(),
            replay_cmd: report.replay_command.clone(),
        });
    }

    let verdict = portfolio_verdict(fixture, &report);
    PortfolioFixtureEvaluation {
        label: fixture.label.clone(),
        diagnostics,
        verdict,
        report,
    }
}

fn portfolio_verdict(
    fixture: &PortfolioFixture,
    report: &PortfolioSchedulerReport,
) -> OutcomeVerdict {
    let s = &report.summary;
    let mut problems: Vec<String> = Vec::new();
    if report.gate_passes != fixture.expected.gate_passes {
        problems.push(format!(
            "gate_passes={} expected={}",
            report.gate_passes, fixture.expected.gate_passes
        ));
    }
    if fixture.expected.expect_conservative && s.conservative_events == 0 {
        problems.push("expected a conservative event, found none".to_string());
    }
    if !fixture.expected.expect_conservative && s.conservative_events > 0 {
        problems.push(format!(
            "unexpected conservative events: {}",
            s.conservative_events
        ));
    }
    if fixture.expected.expect_diversity_violation && s.diversity_violations == 0 {
        problems.push("expected a diversity violation, found none".to_string());
    }
    if !fixture.expected.expect_diversity_violation && s.diversity_violations > 0 {
        problems.push(format!(
            "unexpected diversity violations: {}",
            s.diversity_violations
        ));
    }
    if fixture.expected.expect_invalid && s.invalid == 0 {
        problems.push("expected a malformed candidate, found none".to_string());
    }
    if !fixture.expected.expect_invalid && s.invalid > 0 {
        problems.push(format!("unexpected invalid candidates: {}", s.invalid));
    }
    if s.final_selected < fixture.expected.min_final_selected {
        problems.push(format!(
            "final_selected={} < min={}",
            s.final_selected, fixture.expected.min_final_selected
        ));
    }
    // Conservative integrity + safety monotonicity must always hold.
    if !s.conservative_integrity_ok {
        problems.push("conservative integrity violated (a silent safety event)".to_string());
    }
    if !s.safety_monotone_ok {
        problems.push("safety monotonicity violated (governor raised risk)".to_string());
    }
    OutcomeVerdict {
        fixture_label: fixture.label.clone(),
        kernel: GovernanceKernel::Portfolio,
        expectation_met: problems.is_empty(),
        detail: if problems.is_empty() {
            "ok".to_string()
        } else {
            problems.join("; ")
        },
    }
}

fn non_empty(value: &str) -> String {
    if value.trim().is_empty() {
        "n/a".to_string()
    } else {
        value.to_string()
    }
}

// ── Validation report ────────────────────────────────────────────────────────

/// Aggregate summary for a portfolio-governance validation run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortfolioGovernanceValidationSummary {
    /// Total diagnostics emitted.
    pub total_diagnostics: usize,
    /// Reverse-round diagnostics.
    pub reverse_round_diagnostics: usize,
    /// Portfolio diagnostics.
    pub portfolio_diagnostics: usize,
    /// Total fixtures evaluated.
    pub total_fixtures: usize,
    /// Fixtures whose verdict met its expectation.
    pub verdicts_met: usize,
    /// Whether every diagnostic has all AC3 fields populated.
    pub all_required_fields: bool,
    /// Whether every fixture met its expectation.
    pub all_expectations_met: bool,
    /// Whether the validation gate passes (fields complete + all expectations met).
    pub gate_passes: bool,
}

/// Self-contained JSON-stats artifact for CI consumption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortfolioGovernanceJsonStatsArtifact {
    /// Suggested relative output path.
    pub path: String,
    /// SHA-256 of `content`.
    pub sha256: String,
    /// Serialized JSON content.
    pub content: String,
}

/// The full portfolio-governance validation report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortfolioGovernanceValidationReport {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// The scenario label.
    pub scenario_label: String,
    /// All diagnostics (sorted deterministically).
    pub diagnostics: Vec<GovernanceDiagnostic>,
    /// All fixture verdicts (sorted deterministically).
    pub verdicts: Vec<OutcomeVerdict>,
    /// Aggregate summary.
    pub summary: PortfolioGovernanceValidationSummary,
    /// Whether the validation gate passes.
    pub gate_passes: bool,
    /// Exported JSON-stats artifact.
    pub exported_json_stats: PortfolioGovernanceJsonStatsArtifact,
    /// Deterministic replay command.
    pub replay_command: String,
    /// Evidence checksum over diagnostics + verdicts + summary.
    pub evidence_checksum: String,
}

impl PortfolioGovernanceValidationReport {
    /// The failure-log projections for every diagnostic (the AC3 envelope set).
    #[must_use]
    pub fn failure_logs(&self) -> Vec<GovernanceFailureLog> {
        self.diagnostics
            .iter()
            .map(GovernanceDiagnostic::failure_log)
            .collect()
    }

    /// The verdicts that did not meet their expectation.
    #[must_use]
    pub fn failing_verdicts(&self) -> Vec<&OutcomeVerdict> {
        self.verdicts
            .iter()
            .filter(|v| !v.expectation_met)
            .collect()
    }
}

/// Run the full cross-kernel validation over the green/red fixture corpora and
/// return a deterministic, replay-identical report.
#[must_use]
pub fn run_portfolio_governance_validation(label: &str) -> PortfolioGovernanceValidationReport {
    let mut diagnostics: Vec<GovernanceDiagnostic> = Vec::new();
    let mut verdicts: Vec<OutcomeVerdict> = Vec::new();

    for fixture in reverse_round_fixture_corpus() {
        let eval = evaluate_reverse_round_fixture(&fixture);
        diagnostics.extend(eval.diagnostics);
        verdicts.push(eval.verdict);
    }
    for fixture in portfolio_fixture_corpus() {
        let eval = evaluate_portfolio_fixture(&fixture);
        diagnostics.extend(eval.diagnostics);
        verdicts.push(eval.verdict);
    }

    // Deterministic ordering.
    diagnostics.sort_by(|a, b| {
        a.kernel
            .cmp(&b.kernel)
            .then_with(|| a.claim_id.cmp(&b.claim_id))
            .then_with(|| a.action_id.cmp(&b.action_id))
            .then_with(|| a.run_id.cmp(&b.run_id))
    });
    verdicts.sort_by(|a, b| {
        a.kernel
            .cmp(&b.kernel)
            .then_with(|| a.fixture_label.cmp(&b.fixture_label))
    });

    let reverse_round_diagnostics = diagnostics
        .iter()
        .filter(|d| d.kernel == GovernanceKernel::ReverseRound)
        .count();
    let portfolio_diagnostics = diagnostics.len() - reverse_round_diagnostics;
    let all_required_fields = diagnostics
        .iter()
        .all(GovernanceDiagnostic::has_required_fields);
    let verdicts_met = verdicts.iter().filter(|v| v.expectation_met).count();
    let all_expectations_met = verdicts_met == verdicts.len();
    let gate_passes = all_required_fields && all_expectations_met;

    let summary = PortfolioGovernanceValidationSummary {
        total_diagnostics: diagnostics.len(),
        reverse_round_diagnostics,
        portfolio_diagnostics,
        total_fixtures: verdicts.len(),
        verdicts_met,
        all_required_fields,
        all_expectations_met,
        gate_passes,
    };

    let evidence_checksum = {
        #[derive(Serialize)]
        struct EvidenceInput<'a> {
            diagnostics: &'a [GovernanceDiagnostic],
            verdicts: &'a [OutcomeVerdict],
            summary: &'a PortfolioGovernanceValidationSummary,
        }
        stable_hash(&EvidenceInput {
            diagnostics: &diagnostics,
            verdicts: &verdicts,
            summary: &summary,
        })
    };
    let report_id = {
        #[derive(Serialize)]
        struct ReportIdInput<'a> {
            schema_version: &'a str,
            label: &'a str,
            evidence_checksum: &'a str,
        }
        format!(
            "portfolio-governance-report-{}",
            short_hash(&stable_hash(&ReportIdInput {
                schema_version: PORTFOLIO_GOVERNANCE_TESTS_SCHEMA_VERSION,
                label,
                evidence_checksum: &evidence_checksum,
            }))
        )
    };
    let replay_command = format!(
        "cargo test -p doctor_frankentui --lib portfolio_governance_tests -- --nocapture # report {report_id}"
    );

    let exported_json_stats = {
        #[derive(Serialize)]
        struct Export<'a> {
            schema_version: &'a str,
            report_id: &'a str,
            summary: &'a PortfolioGovernanceValidationSummary,
            diagnostics: &'a [GovernanceDiagnostic],
            verdicts: &'a [OutcomeVerdict],
        }
        let content = serde_json::to_string_pretty(&Export {
            schema_version: PORTFOLIO_GOVERNANCE_TESTS_SCHEMA_VERSION,
            report_id: &report_id,
            summary: &summary,
            diagnostics: &diagnostics,
            verdicts: &verdicts,
        })
        .unwrap_or_else(|error| error.to_string());
        PortfolioGovernanceJsonStatsArtifact {
            path: format!("{report_id}/portfolio_governance_stats.json"),
            sha256: sha256_hex(content.as_bytes()),
            content,
        }
    };

    PortfolioGovernanceValidationReport {
        schema_version: PORTFOLIO_GOVERNANCE_TESTS_SCHEMA_VERSION.to_string(),
        report_id,
        scenario_label: label.to_string(),
        diagnostics,
        verdicts,
        summary,
        gate_passes,
        exported_json_stats,
        replay_command,
        evidence_checksum,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ── Validation harness (green) ─────────────────────────────────────────────

    #[test]
    fn validation_gate_passes_and_every_diagnostic_is_complete() {
        let report = run_portfolio_governance_validation("portfolio-governance/test");
        assert!(
            report.gate_passes,
            "verdicts: {:?}",
            report.failing_verdicts()
        );
        assert!(report.summary.all_required_fields);
        assert!(report.summary.all_expectations_met);
        assert!(report.summary.total_diagnostics > 0);
        assert!(report.summary.reverse_round_diagnostics > 0);
        assert!(report.summary.portfolio_diagnostics > 0);
        // AC3: every diagnostic projects a complete failure-log envelope.
        for log in report.failure_logs() {
            assert!(!log.run_id.is_empty());
            assert!(!log.claim_id.is_empty());
            assert!(!log.comparator_id.is_empty());
            assert!(!log.action_id.is_empty());
            assert!(!log.equation_id.is_empty());
            assert!(!log.posterior_hash.is_empty());
            assert!(!log.loss_components.is_empty());
            assert!(!log.fallback_reason.is_empty());
        }
    }

    #[test]
    fn validation_report_is_deterministic_and_replay_identical() {
        let a = run_portfolio_governance_validation("portfolio-governance/test");
        let b = run_portfolio_governance_validation("portfolio-governance/test");
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.diagnostics, b.diagnostics);
        assert_eq!(a.exported_json_stats.sha256, b.exported_json_stats.sha256);
    }

    #[test]
    fn exported_stats_checksum_matches_content() {
        let report = run_portfolio_governance_validation("portfolio-governance/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }

    // ── Reverse-round: happy + red paths ───────────────────────────────────────

    #[test]
    fn reverse_round_green_passes_with_no_blocking_findings() {
        let request = green_reverse_round_request("rr-green");
        let report = ReverseRoundGate::default().evaluate(&request);
        assert!(report.gate_passes, "findings: {:?}", report.findings);
        assert_eq!(report.summary.blocking_findings, 0);
        assert_eq!(report.decision_logs.len(), 1);
    }

    #[test]
    fn reverse_round_red_fixtures_block_with_expected_codes() {
        for fixture in reverse_round_fixture_corpus() {
            let eval = evaluate_reverse_round_fixture(&fixture);
            assert!(
                eval.verdict.expectation_met,
                "fixture {} mismatch: {}",
                fixture.label, eval.verdict.detail
            );
            // Every emitted diagnostic is forensically complete.
            assert!(
                eval.diagnostics
                    .iter()
                    .all(GovernanceDiagnostic::has_required_fields)
            );
        }
    }

    #[test]
    fn reverse_round_multi_lever_is_detected() {
        let corpus = reverse_round_fixture_corpus();
        let fixture = corpus
            .iter()
            .find(|f| f.label == "reverse_round/multi_lever")
            .unwrap();
        let report = ReverseRoundGate::default().evaluate(&fixture.request);
        assert!(!report.gate_passes);
        assert!(report.summary.multi_lever_windows >= 1);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == ReverseRoundFindingCode::MultiLeverWithoutOverride)
        );
    }

    #[test]
    fn reverse_round_rollback_is_deterministic() {
        // Rollback resolution is a pure function of the request: two runs agree.
        let request = green_reverse_round_request("rr-determinism");
        let first = ReverseRoundGate::default().evaluate(&request);
        let second = ReverseRoundGate::default().evaluate(&request);
        assert_eq!(first.evidence_checksum, second.evidence_checksum);
        assert_eq!(first.ledger, second.ledger);
    }

    // ── Portfolio: happy + red paths ───────────────────────────────────────────

    #[test]
    fn portfolio_fixtures_meet_expectations() {
        for fixture in portfolio_fixture_corpus() {
            let eval = evaluate_portfolio_fixture(&fixture);
            assert!(
                eval.verdict.expectation_met,
                "fixture {} mismatch: {}",
                fixture.label, eval.verdict.detail
            );
            assert!(
                eval.diagnostics
                    .iter()
                    .all(GovernanceDiagnostic::has_required_fields)
            );
        }
    }

    #[test]
    fn portfolio_adversarial_uncertainty_forces_surfaced_fallback() {
        let corpus = portfolio_fixture_corpus();
        let fixture = corpus
            .iter()
            .find(|f| f.label == "portfolio/adversarial_uncertainty")
            .unwrap();
        let eval = evaluate_portfolio_fixture(fixture);
        // The uncertain milestone's diagnostic surfaces a non-`none` fallback.
        assert!(
            eval.diagnostics
                .iter()
                .any(|d| d.claim_id == "m.uncertain" && d.fallback_reason != "none"),
            "fallback was not surfaced: {:?}",
            eval.diagnostics
        );
        assert!(eval.report.summary.conservative_integrity_ok);
    }

    // ── Loss-model sanity ──────────────────────────────────────────────────────

    #[test]
    fn expected_loss_decreases_with_higher_posterior_mean() {
        // A single milestone with two candidates; the higher-mean one must carry
        // the lower expected loss (loss-model monotonicity, read off the ledger).
        let milestones = vec![SchedulerMilestone::new(
            "m.mono",
            QualityBar::Bronze,
            RiskTier::Medium,
            vec![
                candidate("hi", "m.mono", MathFamily::Symbolic, 40.0, 4.0, 1.0),
                candidate("lo", "m.mono", MathFamily::Probabilistic, 6.0, 10.0, 1.0),
            ],
        )];
        let report =
            PortfolioScheduler::new("loss-mono", PortfolioSchedulerConfig::default(), milestones)
                .run(None);
        let mut hi = f64::NAN;
        let mut lo = f64::NAN;
        for line in report
            .ledger
            .iter()
            .filter(|l| l.stage == ScheduleStage::Score)
        {
            let loss: f64 = line.expected_loss.parse().unwrap();
            if line.primitive_id == "hi" {
                hi = loss;
            } else if line.primitive_id == "lo" {
                lo = loss;
            }
        }
        assert!(hi.is_finite() && lo.is_finite());
        assert!(hi <= lo + 1e-9, "higher-mean loss {hi} should be <= {lo}");
    }

    // ── Property tests ─────────────────────────────────────────────────────────

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// The full cross-kernel validation report is byte-stable across runs
        /// (AC2: deterministic replay + stable decision hashes).
        #[test]
        fn prop_validation_report_is_byte_stable(label in "[a-z]{1,8}") {
            let first = run_portfolio_governance_validation(&label);
            let second = run_portfolio_governance_validation(&label);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.diagnostics, &second.diagnostics);
            prop_assert_eq!(
                &first.exported_json_stats.sha256,
                &second.exported_json_stats.sha256
            );
        }

        /// The portfolio scheduler's committed selections are a pure function of
        /// its inputs: the same portfolio replays to the same decision hash.
        #[test]
        fn prop_portfolio_decision_hash_is_stable(
            a1 in 20u32..60, c1 in 1u32..6, c2 in 1u32..6,
        ) {
            let milestones = vec![
                SchedulerMilestone::new(
                    "p1", QualityBar::Bronze, RiskTier::Medium,
                    vec![candidate("a", "p1", MathFamily::Symbolic, f64::from(a1), 4.0, f64::from(c1))],
                ),
                SchedulerMilestone::new(
                    "p2", QualityBar::Bronze, RiskTier::Medium,
                    vec![candidate("b", "p2", MathFamily::FormalAnalysis, 40.0, 4.0, f64::from(c2))],
                ),
            ];
            let first = PortfolioScheduler::new("prop", PortfolioSchedulerConfig::default(), milestones.clone()).run(None);
            let second = PortfolioScheduler::new("prop", PortfolioSchedulerConfig::default(), milestones).run(None);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.report_id, &second.report_id);
        }

        /// A maximally-uncertain candidate (Beta(2,2), variance 0.05) always
        /// forces a surfaced conservative event — deterministic fallback under
        /// high uncertainty, regardless of the confident milestone's parameters.
        #[test]
        fn prop_high_uncertainty_always_falls_back(
            alpha in 30u32..60, beta in 2u32..8,
        ) {
            let milestones = vec![
                SchedulerMilestone::new(
                    "confident", QualityBar::Bronze, RiskTier::Medium,
                    vec![candidate("c", "confident", MathFamily::Symbolic, f64::from(alpha), f64::from(beta), 2.0)],
                ),
                SchedulerMilestone::new(
                    "uncertain", QualityBar::Bronze, RiskTier::Medium,
                    vec![candidate("u", "uncertain", MathFamily::FormalAnalysis, 2.0, 2.0, 2.0)],
                ),
            ];
            let report = PortfolioScheduler::new("prop-unc", PortfolioSchedulerConfig::default(), milestones).run(None);
            prop_assert!(report.summary.conservative_events >= 1);
            prop_assert!(report.summary.conservative_integrity_ok);
            prop_assert!(report.summary.safety_monotone_ok);
        }

        /// Branch-diversity is enforced: when every milestone's argmin lands in
        /// the same family, the concentration is detected as a violation.
        #[test]
        fn prop_single_family_portfolio_violates_diversity(n in 2usize..5) {
            let milestones: Vec<SchedulerMilestone> = (0..n)
                .map(|i| {
                    SchedulerMilestone::new(
                        format!("m{i}"), QualityBar::Bronze, RiskTier::Medium,
                        vec![candidate(
                            &format!("sym{i}"), &format!("m{i}"),
                            MathFamily::Symbolic, 45.0, 5.0, 1.0,
                        )],
                    )
                })
                .collect();
            let report = PortfolioScheduler::new("prop-div", PortfolioSchedulerConfig::default(), milestones).run(None);
            // All selections in one family => 100% share => over the 0.5 cap.
            prop_assert!(report.summary.diversity_violations >= 1);
            prop_assert!(!report.summary.diversity_ok);
            prop_assert!(report.summary.diversity_integrity_ok);
        }

        /// VOI is non-negative for any non-degenerate Beta posterior (the
        /// exploration signal the scheduler reads is always well-formed).
        #[test]
        fn prop_voi_is_non_negative(successes in 0u32..200, failures in 0u32..200) {
            let posterior = BetaPosterior::new(f64::from(successes) + 1.0, f64::from(failures) + 1.0);
            prop_assert!(posterior.voi() >= -1e-12);
            prop_assert!(posterior.variance() >= -1e-12);
            prop_assert!((0.0..=1.0).contains(&posterior.mean()));
        }
    }
}
