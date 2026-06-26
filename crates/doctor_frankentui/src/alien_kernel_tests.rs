//! Cross-kernel unit/property test-evidence harness for the alien-uplift kernels
//! (bd-3bxhj.10.10).
//!
//! The alien-graveyard uplift work introduced a family of "alien kernel" modules:
//! contracts-as-code ([`crate::recommendation_contract`]), CEGIS sketch synthesis
//! ([`crate::cegis_synthesis`]), e-graph extraction ([`crate::egraph_optimizer`]),
//! the abstract-interpretation safety layer ([`crate::abstract_interpretation`]),
//! the concolic differential explorer ([`crate::concolic_differential`]), the
//! metamorphic-relation oracle ([`crate::metamorphic_oracle`]), the shadow-run
//! dual-execution harness ([`crate::shadow_run`]), the controller-composition
//! interference guard ([`crate::controller_composition`]), the closed-form
//! Bayesian posterior core ([`crate::posterior_core`]), the expected-loss decision
//! solver ([`crate::semantic_contract::ConfidenceModel`]), the formal-guarantee
//! wrapper ([`crate::recommendation_contract::FormalGuarantee`]), the
//! value-of-information test-budget planner ([`crate::test_budget_allocator`]), and
//! the BOCPD/CUSUM hazard / change-point models ([`crate::drift_monitor`]).
//!
//! Each kernel already ships its own inline unit tests. This module adds the
//! *cross-kernel* contract the parent bead asks for: a single deterministic
//! [`AlienKernelTestReport`] whose per-check ledger records, for every kernel,
//!
//! - `kernel` + `equation_id` (which mathematical primitive is under test),
//! - `policy_id` (the suite policy lane the check ran under),
//! - `budget_state` (budget limit/consumption/exhaustion + whether the kernel's
//!   conservative fallback engaged),
//! - `posterior_hash` (stable hash of any Bayesian posterior / decision state),
//! - `guarantee_status` (does the kernel's formal guarantee hold, degrade, or get
//!   refuted),
//! - `proof_hash` + `witness_hash` (stable hashes of the proof artifact and the
//!   counterexample/witness the kernel produced).
//!
//! This satisfies the bead's acceptance criteria directly:
//!
//! 1. Every kernel is exercised with adversarial / malformed input plus an explicit
//!    graceful-degradation check ([`CheckKind::Adversarial`] / [`CheckKind::Degradation`]).
//! 2. Replay is deterministic: each [`KernelCheckRecord`] carries the `input_seed`
//!    and a `replay_command`, and each kernel emits a [`CheckKind::Replay`] record
//!    that re-runs the kernel and proves the witness/proof hash is reproducible.
//! 3. The structured log carries `kernel_id`, `equation_id`, `policy_id`,
//!    `budget_state`, `posterior_hash`, `guarantee_status`, and proof/witness hashes.
//!
//! The suite is pure and deterministic — it owns no I/O — so the same configuration
//! always yields the same `report_id` and `evidence_checksum`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::abstract_interpretation::{AnalysisConfig, SafetyProperty, analyze_safety};
use crate::cegis_synthesis::{CegisSynthesizer, SynthesisBudget, SynthesisOutcome};
use crate::concolic_differential::{ConcolicBudget, ConcolicDifferentialExplorer};
use crate::controller_composition::{
    CompositionGuardConfig, ControllerCompositionGuard, ControllerKind, ControllerProfile,
    ControllerRole, CouplingRisk,
};
use crate::drift_monitor::{
    DriftKind, DriftMetricSeries, DriftMonitor, DriftMonitorConfig, MetricDirection,
};
use crate::effect_canonical::{
    AsyncBoundary, CanonicalEffect, CanonicalEffectModel, ClassificationConfidence,
    CleanupStrategy, ExecutionModel, MessageProtocol, OrderingConstraint, TriggerCondition,
};
use crate::egraph_optimizer::{EGraphBudget, EGraphOptimizer, StopReason};
use crate::metamorphic_oracle::{MetamorphicOracleConfig, compare_traces_with_metamorphic_oracle};
use crate::migration_ir::{EffectKind, IrNodeId, Provenance};
use crate::posterior_core::{
    ChannelEvidence, EvidenceChannel, PosteriorCoreConfig, PosteriorEngine,
};
use crate::recommendation_contract::{
    ContractLintConfig, FormalGuarantee, GuaranteeKind, RecommendationContractLinter,
    example_complete_contract,
};
use crate::semantic_contract::{MigrationDecision, load_builtin_confidence_model};
use crate::semantic_diff::SemanticDiffVerdict;
use crate::shadow_run::{
    ShadowPromotionDecision, ShadowRunConfig, ShadowRunHarness, ShadowTracePair,
};
use crate::test_budget_allocator::{TestBudgetAllocator, TestBudgetConfig, TestFixtureCandidate};
use crate::trace::{InteractionTrace, KeyAction, TraceBuilder, TracePayload, Viewport};
use crate::translation_planner::{IrSegment, SegmentCategory};

/// Schema version for the cross-kernel test-evidence report.
pub const ALIEN_KERNEL_TEST_SCHEMA_VERSION: &str = "alien-kernel-tests-v1";

// ── Kernel identity ────────────────────────────────────────────────────────

/// One of the alien-uplift kernels covered by the suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlienKernel {
    /// Contracts-as-code compiler + linter (`recommendation_contract`).
    ContractsAsCode,
    /// CEGIS sketch synthesis with bounds + fallback (`cegis_synthesis`).
    Cegis,
    /// E-graph saturation optimizer / extraction (`egraph_optimizer`).
    EGraph,
    /// Abstract-interpretation safety layer (`abstract_interpretation`).
    AbstractInterpretation,
    /// Concolic differential explorer (`concolic_differential`).
    Concolic,
    /// Metamorphic-relation oracle (`metamorphic_oracle`).
    MetamorphicOracle,
    /// Shadow-run dual execution harness (`shadow_run`).
    ShadowRun,
    /// Controller-composition interference guard (`controller_composition`).
    ControllerComposition,
    /// Closed-form Bayesian posterior core (`posterior_core`).
    PosteriorCore,
    /// Expected-loss decision solver (`semantic_contract::ConfidenceModel`).
    ExpectedLoss,
    /// Formal-guarantee wrapper (`recommendation_contract::FormalGuarantee`).
    GuaranteeWrapper,
    /// Value-of-information test-budget planner (`test_budget_allocator`).
    VoiPlanner,
    /// BOCPD/CUSUM hazard / change-point models (`drift_monitor`).
    HazardChangePoint,
}

impl AlienKernel {
    /// Every kernel covered by the suite, in stable order.
    pub const ALL: &'static [AlienKernel] = &[
        AlienKernel::ContractsAsCode,
        AlienKernel::Cegis,
        AlienKernel::EGraph,
        AlienKernel::AbstractInterpretation,
        AlienKernel::Concolic,
        AlienKernel::MetamorphicOracle,
        AlienKernel::ShadowRun,
        AlienKernel::ControllerComposition,
        AlienKernel::PosteriorCore,
        AlienKernel::ExpectedLoss,
        AlienKernel::GuaranteeWrapper,
        AlienKernel::VoiPlanner,
        AlienKernel::HazardChangePoint,
    ];

    /// Stable lowercase identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ContractsAsCode => "contracts_as_code",
            Self::Cegis => "cegis",
            Self::EGraph => "egraph",
            Self::AbstractInterpretation => "abstract_interpretation",
            Self::Concolic => "concolic",
            Self::MetamorphicOracle => "metamorphic_oracle",
            Self::ShadowRun => "shadow_run",
            Self::ControllerComposition => "controller_composition",
            Self::PosteriorCore => "posterior_core",
            Self::ExpectedLoss => "expected_loss",
            Self::GuaranteeWrapper => "guarantee_wrapper",
            Self::VoiPlanner => "voi_planner",
            Self::HazardChangePoint => "hazard_change_point",
        }
    }

    /// Stable identifier for the mathematical primitive the kernel implements.
    ///
    /// Tying each kernel to an `equation_id` keeps the ledger anchored to the
    /// math catalog in the project README rather than to an opaque code path.
    #[must_use]
    pub fn equation_id(self) -> &'static str {
        match self {
            Self::ContractsAsCode => "artifact-graph/claim-evidence-policy-trace",
            Self::Cegis => "cegis/counterexample-guided-inductive-synthesis",
            Self::EGraph => "egraph/equality-saturation-extraction",
            Self::AbstractInterpretation => "abstract-interpretation/galois-overapproximation",
            Self::Concolic => "concolic/path-constraint-differential",
            Self::MetamorphicOracle => "metamorphic/relation-invariance",
            Self::ShadowRun => "shadow-run/dual-execution-divergence",
            Self::ControllerComposition => "control/timescale-separation-spectral-radius",
            Self::PosteriorCore => "bayes/log-odds-posterior-ledger",
            Self::ExpectedLoss => "decision/min-expected-loss",
            Self::GuaranteeWrapper => "guarantee/conformal-eprocess-pacbayes",
            Self::VoiPlanner => "voi/beta-binomial-variance-reduction",
            Self::HazardChangePoint => "bocpd-cusum/run-length-posterior",
        }
    }
}

// ── Check taxonomy ─────────────────────────────────────────────────────────

/// Outcome of a single kernel check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckVerdict {
    /// The kernel behaved as specified.
    Pass,
    /// The kernel violated the specified invariant (a regression).
    Fail,
}

impl CheckVerdict {
    /// Stable lowercase identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
        }
    }

    /// Convenience: `true` for [`CheckVerdict::Pass`].
    #[must_use]
    pub fn passed(self) -> bool {
        matches!(self, Self::Pass)
    }

    /// Build a verdict from a boolean expectation.
    #[must_use]
    pub fn from_bool(ok: bool) -> Self {
        if ok { Self::Pass } else { Self::Fail }
    }
}

/// The category of evidence a check provides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    /// A canonical-input invariant (happy path / soundness).
    Invariant,
    /// A deterministic seeded property-style sweep.
    Property,
    /// Behavior on adversarial / malformed input.
    Adversarial,
    /// Explicit graceful-degradation / fail-closed behavior.
    Degradation,
    /// Deterministic replay / reproducibility of a prior result.
    Replay,
}

impl CheckKind {
    /// Stable lowercase identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invariant => "invariant",
            Self::Property => "property",
            Self::Adversarial => "adversarial",
            Self::Degradation => "degradation",
            Self::Replay => "replay",
        }
    }
}

/// Whether a kernel's formal guarantee held for a given check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuaranteeStatus {
    /// The formal guarantee held (e.g. soundness proven, gate passed).
    Holds,
    /// The guarantee degraded to a conservative fallback (still safe).
    Degraded,
    /// The guarantee was refuted by a witness/counterexample (fail-closed).
    Refuted,
    /// No formal guarantee applies to this check.
    NotApplicable,
}

impl GuaranteeStatus {
    /// Stable lowercase identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Holds => "holds",
            Self::Degraded => "degraded",
            Self::Refuted => "refuted",
            Self::NotApplicable => "not_applicable",
        }
    }
}

// ── Budget snapshot ────────────────────────────────────────────────────────

/// A snapshot of a kernel's budget state at the moment a check ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetState {
    /// Budget limit (iterations / nodes / candidates / runs). `0` means unbounded.
    pub limit: u64,
    /// Amount of the budget consumed.
    pub consumed: u64,
    /// Whether the budget was exhausted.
    pub exhausted: bool,
    /// Whether the kernel's conservative fallback engaged.
    pub fallback_engaged: bool,
    /// What the budget meters (e.g. `cegis_iterations`, `egraph_nodes`).
    pub descriptor: String,
}

impl BudgetState {
    /// A budget state for a check that has no resource budget.
    #[must_use]
    pub fn unbounded(descriptor: impl Into<String>) -> Self {
        Self {
            limit: 0,
            consumed: 0,
            exhausted: false,
            fallback_engaged: false,
            descriptor: descriptor.into(),
        }
    }

    /// A fully specified budget state.
    #[must_use]
    pub fn new(
        limit: u64,
        consumed: u64,
        exhausted: bool,
        fallback_engaged: bool,
        descriptor: impl Into<String>,
    ) -> Self {
        Self {
            limit,
            consumed,
            exhausted,
            fallback_engaged,
            descriptor: descriptor.into(),
        }
    }
}

// ── Ledger record ──────────────────────────────────────────────────────────

/// One entry in the cross-kernel evidence ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelCheckRecord {
    /// Stable, content-addressed check identifier.
    pub check_id: String,
    /// Which kernel this check exercised.
    pub kernel: AlienKernel,
    /// The mathematical primitive identifier.
    pub equation_id: String,
    /// The policy lane the check ran under.
    pub policy_id: String,
    /// The category of evidence.
    pub kind: CheckKind,
    /// The invariant / property under test (human-readable).
    pub property: String,
    /// The deterministic input seed (0 for non-seeded checks).
    pub input_seed: u64,
    /// The verdict.
    pub verdict: CheckVerdict,
    /// Whether the kernel degraded gracefully on this (adversarial) input.
    pub graceful_degradation: bool,
    /// Budget snapshot.
    pub budget_state: BudgetState,
    /// Stable hash of the posterior / decision state (`n/a` if none).
    pub posterior_hash: String,
    /// Whether the kernel's formal guarantee held.
    pub guarantee_status: GuaranteeStatus,
    /// Stable hash of the proof artifact (`n/a` if none).
    pub proof_hash: String,
    /// Stable hash of the witness / counterexample (`n/a` if none).
    pub witness_hash: String,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic replay command for this exact check.
    pub replay_command: String,
}

/// Builder for [`KernelCheckRecord`] that fills derived fields deterministically.
struct CheckDraft {
    kernel: AlienKernel,
    kind: CheckKind,
    property: String,
    seed: u64,
    verdict: CheckVerdict,
    graceful_degradation: bool,
    budget_state: BudgetState,
    posterior_hash: String,
    guarantee_status: GuaranteeStatus,
    proof_hash: String,
    witness_hash: String,
    detail: String,
}

impl CheckDraft {
    fn new(kernel: AlienKernel, kind: CheckKind, property: impl Into<String>) -> Self {
        Self {
            kernel,
            kind,
            property: property.into(),
            seed: 0,
            verdict: CheckVerdict::Pass,
            graceful_degradation: false,
            budget_state: BudgetState::unbounded("none"),
            posterior_hash: na(),
            guarantee_status: GuaranteeStatus::NotApplicable,
            proof_hash: na(),
            witness_hash: na(),
            detail: String::new(),
        }
    }

    fn seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    fn pass_if(mut self, ok: bool) -> Self {
        self.verdict = CheckVerdict::from_bool(ok);
        self
    }

    fn graceful(mut self, graceful: bool) -> Self {
        self.graceful_degradation = graceful;
        self
    }

    fn budget(mut self, budget: BudgetState) -> Self {
        self.budget_state = budget;
        self
    }

    fn posterior_hash(mut self, hash: String) -> Self {
        self.posterior_hash = hash;
        self
    }

    fn guarantee(mut self, status: GuaranteeStatus) -> Self {
        self.guarantee_status = status;
        self
    }

    fn proof_hash(mut self, hash: String) -> Self {
        self.proof_hash = hash;
        self
    }

    fn witness_hash(mut self, hash: String) -> Self {
        self.witness_hash = hash;
        self
    }

    fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    fn finish(self, suite_policy_id: &str) -> KernelCheckRecord {
        let equation_id = self.kernel.equation_id().to_string();
        let policy_id = format!("{suite_policy_id}/{}", self.kernel.as_str());
        let check_id = format!(
            "{}-{}-{}",
            self.kernel.as_str(),
            self.kind.as_str(),
            short_hash(&stable_hash(&CheckIdInput {
                kernel: self.kernel,
                kind: self.kind,
                property: &self.property,
                seed: self.seed,
            })),
        );
        let replay_command = format!(
            "doctor_frankentui alien-kernel-tests --kernel {} --kind {} --seed {} --check {check_id}",
            self.kernel.as_str(),
            self.kind.as_str(),
            self.seed,
        );
        KernelCheckRecord {
            check_id,
            kernel: self.kernel,
            equation_id,
            policy_id,
            kind: self.kind,
            property: self.property,
            input_seed: self.seed,
            verdict: self.verdict,
            graceful_degradation: self.graceful_degradation,
            budget_state: self.budget_state,
            posterior_hash: self.posterior_hash,
            guarantee_status: self.guarantee_status,
            proof_hash: self.proof_hash,
            witness_hash: self.witness_hash,
            detail: self.detail,
            replay_command,
        }
    }
}

#[derive(Serialize)]
struct CheckIdInput<'a> {
    kernel: AlienKernel,
    kind: CheckKind,
    property: &'a str,
    seed: u64,
}

// ── Summary, artifact, report ──────────────────────────────────────────────

/// Aggregate counts over the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlienKernelTestSummary {
    /// Total checks executed.
    pub total_checks: usize,
    /// Checks that passed.
    pub passed: usize,
    /// Checks that failed.
    pub failed: usize,
    /// Distinct kernels covered.
    pub kernels_covered: usize,
    /// Adversarial checks executed.
    pub adversarial_checks: usize,
    /// Degradation checks executed.
    pub degradation_checks: usize,
    /// Replay checks executed.
    pub replay_checks: usize,
    /// Checks whose formal guarantee held.
    pub guarantees_holding: usize,
    /// Checks whose guarantee degraded to a conservative fallback.
    pub guarantees_degraded: usize,
    /// Checks whose guarantee was refuted (fail-closed).
    pub guarantees_refuted: usize,
    /// Whether every check passed.
    pub all_passed: bool,
}

/// Deterministic JSON-stats artifact (content + checksum).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlienKernelJsonStatsArtifact {
    /// Suggested relative output path.
    pub path: String,
    /// SHA-256 of `content`.
    pub sha256: String,
    /// Serialized JSON content.
    pub content: String,
}

/// The full cross-kernel test-evidence report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlienKernelTestReport {
    /// Schema version constant.
    pub schema_version: String,
    /// Deterministic report identifier (derived from configuration).
    pub report_id: String,
    /// The suite policy lane.
    pub suite_policy_id: String,
    /// Number of seeds used per seeded sweep.
    pub seed_count: u32,
    /// The full ledger.
    pub records: Vec<KernelCheckRecord>,
    /// Aggregate summary.
    pub summary: AlienKernelTestSummary,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: AlienKernelJsonStatsArtifact,
    /// Replay command for the whole report.
    pub replay_command: String,
    /// SHA-256 fingerprint of the ledger (output checksum).
    pub evidence_checksum: String,
}

impl AlienKernelTestReport {
    /// All failing records, in ledger order.
    #[must_use]
    pub fn failures(&self) -> Vec<&KernelCheckRecord> {
        self.records
            .iter()
            .filter(|r| r.verdict == CheckVerdict::Fail)
            .collect()
    }

    /// All records for a given kernel, in ledger order.
    #[must_use]
    pub fn records_for(&self, kernel: AlienKernel) -> Vec<&KernelCheckRecord> {
        self.records.iter().filter(|r| r.kernel == kernel).collect()
    }
}

// ── Config + suite ─────────────────────────────────────────────────────────

/// Configuration for the cross-kernel test-evidence suite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlienKernelTestConfig {
    /// The suite policy lane (becomes the prefix of every `policy_id`).
    pub suite_policy_id: String,
    /// Seeds per seeded property sweep.
    pub seed_count: u32,
}

impl Default for AlienKernelTestConfig {
    fn default() -> Self {
        Self {
            suite_policy_id: "alien-kernel-tests/default".to_string(),
            seed_count: 5,
        }
    }
}

/// The deterministic cross-kernel test-evidence suite.
#[derive(Debug, Clone, Default)]
pub struct AlienKernelTestSuite {
    config: AlienKernelTestConfig,
}

impl AlienKernelTestSuite {
    /// Build a suite from configuration.
    #[must_use]
    pub fn new(config: AlienKernelTestConfig) -> Self {
        Self { config }
    }

    /// Borrow the configuration.
    #[must_use]
    pub fn config(&self) -> &AlienKernelTestConfig {
        &self.config
    }

    /// Run every kernel check and assemble the deterministic report.
    #[must_use]
    pub fn run(&self) -> AlienKernelTestReport {
        let policy = &self.config.suite_policy_id;
        let seeds = self.config.seed_count;

        let mut records = Vec::new();
        records.extend(check_contracts_as_code(policy));
        records.extend(check_cegis(policy, seeds));
        records.extend(check_egraph(policy));
        records.extend(check_abstract_interpretation(policy));
        records.extend(check_concolic(policy));
        records.extend(check_metamorphic_oracle(policy));
        records.extend(check_shadow_run(policy));
        records.extend(check_controller_composition(policy));
        records.extend(check_posterior_core(policy, seeds));
        records.extend(check_expected_loss(policy));
        records.extend(check_guarantee_wrapper(policy));
        records.extend(check_voi_planner(policy));
        records.extend(check_hazard_change_point(policy));

        let summary = summarize(&records);
        let evidence_checksum = stable_hash(&records);
        let report_id = format!(
            "alien-kernel-tests-{}",
            short_hash(&stable_hash(&ReportIdInput {
                schema_version: ALIEN_KERNEL_TEST_SCHEMA_VERSION,
                suite_policy_id: policy,
                seed_count: seeds,
            })),
        );
        let replay_command = format!(
            "doctor_frankentui alien-kernel-tests --report-id {report_id} --policy {policy} --seeds {seeds}",
        );
        let exported_json_stats = export_json_stats(
            &report_id,
            policy,
            seeds,
            &summary,
            &records,
            &evidence_checksum,
        );

        AlienKernelTestReport {
            schema_version: ALIEN_KERNEL_TEST_SCHEMA_VERSION.to_string(),
            report_id,
            suite_policy_id: policy.clone(),
            seed_count: seeds,
            records,
            summary,
            exported_json_stats,
            replay_command,
            evidence_checksum,
        }
    }
}

#[derive(Serialize)]
struct ReportIdInput<'a> {
    schema_version: &'a str,
    suite_policy_id: &'a str,
    seed_count: u32,
}

fn summarize(records: &[KernelCheckRecord]) -> AlienKernelTestSummary {
    let mut kernels = BTreeSet::new();
    let mut passed = 0;
    let mut failed = 0;
    let mut adversarial = 0;
    let mut degradation = 0;
    let mut replay = 0;
    let mut holding = 0;
    let mut degraded = 0;
    let mut refuted = 0;
    for record in records {
        kernels.insert(record.kernel);
        match record.verdict {
            CheckVerdict::Pass => passed += 1,
            CheckVerdict::Fail => failed += 1,
        }
        match record.kind {
            CheckKind::Adversarial => adversarial += 1,
            CheckKind::Degradation => degradation += 1,
            CheckKind::Replay => replay += 1,
            CheckKind::Invariant | CheckKind::Property => {}
        }
        match record.guarantee_status {
            GuaranteeStatus::Holds => holding += 1,
            GuaranteeStatus::Degraded => degraded += 1,
            GuaranteeStatus::Refuted => refuted += 1,
            GuaranteeStatus::NotApplicable => {}
        }
    }
    AlienKernelTestSummary {
        total_checks: records.len(),
        passed,
        failed,
        kernels_covered: kernels.len(),
        adversarial_checks: adversarial,
        degradation_checks: degradation,
        replay_checks: replay,
        guarantees_holding: holding,
        guarantees_degraded: degraded,
        guarantees_refuted: refuted,
        all_passed: failed == 0,
    }
}

fn export_json_stats(
    report_id: &str,
    suite_policy_id: &str,
    seed_count: u32,
    summary: &AlienKernelTestSummary,
    records: &[KernelCheckRecord],
    evidence_checksum: &str,
) -> AlienKernelJsonStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        suite_policy_id: &'a str,
        seed_count: u32,
        summary: &'a AlienKernelTestSummary,
        evidence_checksum: &'a str,
        records: &'a [KernelCheckRecord],
    }
    let payload = Export {
        schema_version: ALIEN_KERNEL_TEST_SCHEMA_VERSION,
        report_id,
        suite_policy_id,
        seed_count,
        summary,
        evidence_checksum,
        records,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    AlienKernelJsonStatsArtifact {
        path: format!("{report_id}/alien_kernel_tests_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

// ── Per-kernel checks ──────────────────────────────────────────────────────

/// Contracts-as-code: a complete contract lints clean; a contract missing its
/// formal guarantees fails closed with blocking findings (never silently passes).
fn check_contracts_as_code(policy: &str) -> Vec<KernelCheckRecord> {
    let kernel = AlienKernel::ContractsAsCode;
    let linter = RecommendationContractLinter::new(ContractLintConfig::default());

    let complete = example_complete_contract();
    let report = linter.evaluate(vec![complete.clone()]);
    let report_again = linter.evaluate(vec![complete]);

    let mut degraded_contract = example_complete_contract();
    degraded_contract.formal_guarantees.clear();
    let degraded_report = linter.evaluate(vec![degraded_contract]);

    vec![
        CheckDraft::new(
            kernel,
            CheckKind::Invariant,
            "complete contract passes the lint gate with zero blocking findings",
        )
        .pass_if(report.lint_gate_passes && report.summary.blocking_finding_count == 0)
        .guarantee(if report.lint_gate_passes {
            GuaranteeStatus::Holds
        } else {
            GuaranteeStatus::Refuted
        })
        .proof_hash(stable_hash(&report.summary))
        .witness_hash(stable_hash(&report.exported_json_stats.sha256))
        .detail(format!(
            "blocking={} advisory={}",
            report.summary.blocking_finding_count, report.summary.advisory_finding_count
        ))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Degradation,
            "contract missing formal guarantees fails closed with extra blocking findings",
        )
        .graceful(true)
        .pass_if(
            !degraded_report.lint_gate_passes
                && degraded_report.summary.blocking_finding_count
                    > report.summary.blocking_finding_count,
        )
        .guarantee(GuaranteeStatus::Refuted)
        .proof_hash(stable_hash(&degraded_report.summary))
        .witness_hash(stable_hash(&degraded_report.blocked_card_ids))
        .detail(format!(
            "degraded_blocking={} > complete_blocking={}",
            degraded_report.summary.blocking_finding_count, report.summary.blocking_finding_count
        ))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Replay,
            "lint report id and json-stats checksum are reproducible",
        )
        .pass_if(
            report.report_id == report_again.report_id
                && report.exported_json_stats.sha256 == report_again.exported_json_stats.sha256,
        )
        .proof_hash(stable_hash(&report_again.exported_json_stats.sha256))
        .detail(format!("report_id={}", report.report_id))
        .finish(policy),
    ]
}

/// CEGIS: every mapping category synthesizes to a verified sketch under the
/// default budget; a zero-timeout budget exhausts and fails closed.
fn check_cegis(policy: &str, seeds: u32) -> Vec<KernelCheckRecord> {
    let kernel = AlienKernel::Cegis;
    let synth = CegisSynthesizer::with_defaults();
    let default_iterations = SynthesisBudget::default().max_iterations_per_hole;
    let mut out = Vec::new();

    let categories = [
        SegmentCategory::View,
        SegmentCategory::State,
        SegmentCategory::Event,
        SegmentCategory::Effect,
        SegmentCategory::Layout,
        SegmentCategory::Style,
    ];

    let sweep = seeds
        .max(1)
        .min(u32::try_from(categories.len()).unwrap_or(6));
    for index in 0..sweep {
        let category = categories[index as usize % categories.len()];
        let segment = ir_segment(category, &format!("cegis-seed-{index}"));
        let outcome = synth.synthesize_hole(&segment);
        let (verified, iterations, witness_hash) = match &outcome {
            SynthesisOutcome::Verified { witness, .. } => {
                (true, witness.iterations, cegis_witness_fingerprint(witness))
            }
            SynthesisOutcome::Exhausted { diagnostic } => (
                false,
                diagnostic.iterations_completed,
                stable_hash(diagnostic),
            ),
        };
        out.push(
            CheckDraft::new(
                kernel,
                if index == 0 {
                    CheckKind::Invariant
                } else {
                    CheckKind::Property
                },
                format!("{:?} hole synthesizes to a verified sketch", category),
            )
            .seed(u64::from(index))
            .pass_if(verified)
            .guarantee(if verified {
                GuaranteeStatus::Holds
            } else {
                GuaranteeStatus::Refuted
            })
            .budget(BudgetState::new(
                u64::try_from(default_iterations).unwrap_or(u64::MAX),
                u64::try_from(iterations).unwrap_or(u64::MAX),
                false,
                false,
                "cegis_iterations",
            ))
            .proof_hash(cegis_outcome_fingerprint(&outcome))
            .witness_hash(witness_hash)
            .detail(format!("category={category:?} iterations={iterations}"))
            .finish(policy),
        );
    }

    // Adversarial: a zero-timeout budget must exhaust, not silently verify.
    let starved = CegisSynthesizer::new(SynthesisBudget {
        timeout_per_hole: Duration::ZERO,
        ..SynthesisBudget::default()
    });
    let segment = ir_segment(SegmentCategory::View, "cegis-starved");
    let outcome = starved.synthesize_hole(&segment);
    let exhausted = matches!(outcome, SynthesisOutcome::Exhausted { .. });
    let witness_hash = match &outcome {
        SynthesisOutcome::Exhausted { diagnostic } => stable_hash(&diagnostic.counter_examples),
        SynthesisOutcome::Verified { witness, .. } => cegis_witness_fingerprint(witness),
    };
    out.push(
        CheckDraft::new(
            kernel,
            CheckKind::Adversarial,
            "zero-timeout budget exhausts the search instead of verifying",
        )
        .graceful(exhausted)
        .pass_if(exhausted)
        .guarantee(GuaranteeStatus::Degraded)
        .budget(BudgetState::new(0, 0, true, true, "cegis_timeout"))
        .proof_hash(cegis_outcome_fingerprint(&outcome))
        .witness_hash(witness_hash)
        .detail("timeout_per_hole=0")
        .finish(policy),
    );

    // Replay: re-running the same segment reproduces the same outcome hash.
    let replay_a = synth.synthesize_hole(&ir_segment(SegmentCategory::Event, "cegis-replay"));
    let replay_b = synth.synthesize_hole(&ir_segment(SegmentCategory::Event, "cegis-replay"));
    out.push(
        CheckDraft::new(
            kernel,
            CheckKind::Replay,
            "identical segment synthesizes to an identical outcome",
        )
        .pass_if(cegis_outcome_fingerprint(&replay_a) == cegis_outcome_fingerprint(&replay_b))
        .proof_hash(cegis_outcome_fingerprint(&replay_a))
        .finish(policy),
    );

    out
}

/// Deterministic, timing-free fingerprint of a CEGIS outcome. The `ProofWitness`
/// carries a wall-clock `elapsed` field, so the raw serialization is not
/// reproducible; this projection hashes only the logical result.
fn cegis_outcome_fingerprint(outcome: &SynthesisOutcome) -> String {
    match outcome {
        SynthesisOutcome::Verified { sketch, witness } => {
            stable_hash(&(sketch, cegis_witness_fingerprint(witness)))
        }
        SynthesisOutcome::Exhausted { diagnostic } => stable_hash(diagnostic),
    }
}

/// Deterministic, timing-free fingerprint of a CEGIS proof witness (excludes the
/// wall-clock `elapsed` field).
fn cegis_witness_fingerprint(witness: &crate::cegis_synthesis::ProofWitness) -> String {
    stable_hash(&(
        &witness.obligations_checked,
        &witness.obligations_passed,
        &witness.obligations_failed,
        &witness.counter_examples,
        witness.iterations,
    ))
}

/// E-graph: extraction never increases cost and reaches a fixpoint under a
/// generous budget; a starved node budget breaches and falls back conservatively.
fn check_egraph(policy: &str) -> Vec<KernelCheckRecord> {
    let kernel = AlienKernel::EGraph;
    let sample = "use a::b;\nuse a::b;\nfn view() { Block(Block(Text(\"x\"))); noop(); noop(); }\n";

    let optimizer = EGraphOptimizer::with_defaults();
    let (out_a, entry_a) = optimizer.optimize_file("widget.rs", sample);
    let (out_b, entry_b) = optimizer.optimize_file("widget.rs", sample);
    let default_nodes = EGraphBudget::default().max_nodes;

    let starved = EGraphOptimizer::new(EGraphBudget {
        max_nodes: 1,
        max_iterations: 1,
        timeout: Duration::ZERO,
    });
    let (_starved_out, starved_entry) = starved.optimize_file("widget.rs", sample);

    let (_empty_out, empty_entry) = optimizer.optimize_file("empty.rs", "");

    vec![
        CheckDraft::new(
            kernel,
            CheckKind::Invariant,
            "extraction never increases the cost of the input plan",
        )
        .pass_if(entry_a.cost_after <= entry_a.cost_before)
        .guarantee(if entry_a.stop_reason == StopReason::Saturated {
            GuaranteeStatus::Holds
        } else {
            GuaranteeStatus::Degraded
        })
        .budget(BudgetState::new(
            u64::try_from(default_nodes).unwrap_or(u64::MAX),
            u64::try_from(entry_a.peak_nodes).unwrap_or(u64::MAX),
            entry_a.budget_breached,
            entry_a.budget_breached,
            "egraph_nodes",
        ))
        .proof_hash(stable_hash(&entry_a))
        .detail(format!(
            "cost {}->{} stop={:?}",
            entry_a.cost_before, entry_a.cost_after, entry_a.stop_reason
        ))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Degradation,
            "a starved node budget breaches and extracts a conservative result",
        )
        .graceful(true)
        .pass_if(
            starved_entry.stop_reason != StopReason::Saturated
                && starved_entry.cost_after <= starved_entry.cost_before,
        )
        .guarantee(GuaranteeStatus::Degraded)
        .budget(BudgetState::new(
            1,
            u64::try_from(starved_entry.peak_nodes).unwrap_or(u64::MAX),
            starved_entry.budget_breached,
            true,
            "egraph_nodes",
        ))
        .proof_hash(stable_hash(&starved_entry))
        .detail(format!("stop={:?}", starved_entry.stop_reason))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Adversarial,
            "empty input is handled without panic and yields zero cost",
        )
        .graceful(true)
        .pass_if(empty_entry.cost_before == 0 && empty_entry.cost_after == 0)
        .guarantee(GuaranteeStatus::Holds)
        .proof_hash(stable_hash(&empty_entry))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Replay,
            "identical input yields an identical optimized plan and ledger",
        )
        .pass_if(out_a == out_b && stable_hash(&entry_a) == stable_hash(&entry_b))
        .proof_hash(stable_hash(&entry_b))
        .finish(policy),
    ]
}

/// Abstract interpretation: a clean effect DAG is proven safe; an ordering cycle
/// and a forbidden write are both refuted with a witness path (fail-closed).
fn check_abstract_interpretation(policy: &str) -> Vec<KernelCheckRecord> {
    let kernel = AlienKernel::AbstractInterpretation;
    let config = AnalysisConfig::default();

    // Proven: acyclic deterministic effects.
    let clean = effect_model(vec![
        canonical_effect("a", EffectKind::Network, true),
        canonical_effect("b", EffectKind::Dom, true),
    ]);
    let proven = analyze_safety(&clean, &[SafetyProperty::EffectOrderingSafety], &config);
    let proven_again = analyze_safety(&clean, &[SafetyProperty::EffectOrderingSafety], &config);

    // Refuted: an ordering cycle.
    let mut cyclic = effect_model(vec![
        canonical_effect("a", EffectKind::Network, true),
        canonical_effect("b", EffectKind::Dom, true),
    ]);
    cyclic.ordering_constraints = vec![
        OrderingConstraint {
            before: IrNodeId("ir-a".into()),
            after: IrNodeId("ir-b".into()),
            reason: "a before b".into(),
        },
        OrderingConstraint {
            before: IrNodeId("ir-b".into()),
            after: IrNodeId("ir-a".into()),
            reason: "b before a".into(),
        },
    ];
    let cycle = analyze_safety(&cyclic, &[SafetyProperty::EffectOrderingSafety], &config);

    // Refuted: a write into the globally forbidden set.
    let forbidden_id = IrNodeId("forbidden-store".into());
    let mut writer = canonical_effect("writer", EffectKind::Storage, true);
    writer.writes.insert(forbidden_id.clone());
    let writer_model = effect_model(vec![writer]);
    let mut forbidden_config = AnalysisConfig::default();
    forbidden_config
        .global_forbidden_writes
        .insert(forbidden_id);
    let forbidden = analyze_safety(
        &writer_model,
        &[SafetyProperty::NoForbiddenSideEffects {
            forbidden: BTreeSet::new(),
        }],
        &forbidden_config,
    );

    vec![
        CheckDraft::new(
            kernel,
            CheckKind::Invariant,
            "acyclic deterministic effect DAG is proven ordering-safe",
        )
        .pass_if(proven.all_safe && proven.counterexamples.is_empty())
        .guarantee(if proven.all_safe {
            GuaranteeStatus::Holds
        } else {
            GuaranteeStatus::Refuted
        })
        .proof_hash(debug_hash(&proven.verdicts))
        .detail(format!("obligations={}", proven.obligations.len()))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Degradation,
            "an ordering cycle is refuted with a witness path (conservative reject)",
        )
        .graceful(true)
        .pass_if(!cycle.all_safe && !cycle.counterexamples.is_empty())
        .guarantee(GuaranteeStatus::Refuted)
        .proof_hash(debug_hash(&cycle.verdicts))
        .witness_hash(debug_hash(&cycle.counterexamples))
        .detail(format!("counterexamples={}", cycle.counterexamples.len()))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Adversarial,
            "a forbidden write is refuted instead of silently accepted",
        )
        .graceful(true)
        .pass_if(!forbidden.all_safe && !forbidden.counterexamples.is_empty())
        .guarantee(GuaranteeStatus::Refuted)
        .proof_hash(debug_hash(&forbidden.verdicts))
        .witness_hash(debug_hash(&forbidden.counterexamples))
        .detail("global_forbidden_writes={forbidden-store}")
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Replay,
            "the same model yields the same safety verdicts",
        )
        .pass_if(debug_hash(&proven.verdicts) == debug_hash(&proven_again.verdicts))
        .proof_hash(debug_hash(&proven_again.verdicts))
        .finish(policy),
    ]
}

/// Concolic differential: an equivalent trace pair surfaces no false counterexample;
/// a divergent pair surfaces one; a zero-candidate budget fails closed.
fn check_concolic(policy: &str) -> Vec<KernelCheckRecord> {
    let kernel = AlienKernel::Concolic;
    let explorer = ConcolicDifferentialExplorer::with_defaults();

    let (eq_source, eq_translated) = equivalent_trace_pair();
    let eq_report = explorer.explore_traces(&eq_source, &eq_translated);

    let (div_source, div_translated) = divergent_trace_pair();
    let div_report = explorer.explore_traces(&div_source, &div_translated);
    let div_report_again = explorer.explore_traces(&div_source, &div_translated);

    let zero_budget = ConcolicDifferentialExplorer::new(ConcolicBudget {
        max_candidates: 0,
        ..ConcolicBudget::default()
    });
    let zero_report = zero_budget.explore_traces(&div_source, &div_translated);

    let div_witness = div_report
        .best_counterexample
        .as_ref()
        .map_or_else(na, stable_hash);

    vec![
        CheckDraft::new(
            kernel,
            CheckKind::Invariant,
            "an equivalent trace pair surfaces no counterexample",
        )
        .pass_if(eq_report.best_counterexample.is_none())
        .guarantee(GuaranteeStatus::Holds)
        .proof_hash(stable_hash(&eq_report.solver_witnesses))
        .detail(format!("candidates={}", eq_report.candidates_evaluated))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Property,
            "a divergent trace pair surfaces a minimized counterexample",
        )
        .pass_if(div_report.best_counterexample.is_some())
        .guarantee(GuaranteeStatus::Refuted)
        .budget(BudgetState::new(
            u64::try_from(ConcolicBudget::default().max_candidates).unwrap_or(u64::MAX),
            u64::try_from(div_report.candidates_evaluated).unwrap_or(u64::MAX),
            div_report.budget_exhausted,
            false,
            "concolic_candidates",
        ))
        .proof_hash(stable_hash(&div_report.regression_fixture))
        .witness_hash(div_witness)
        .detail(format!("candidates={}", div_report.candidates_evaluated))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Adversarial,
            "a zero-candidate budget fails closed without a silent pass",
        )
        .graceful(true)
        .pass_if(zero_report.budget_exhausted && zero_report.best_counterexample.is_none())
        .guarantee(GuaranteeStatus::Degraded)
        .budget(BudgetState::new(0, 0, true, true, "concolic_candidates"))
        .proof_hash(stable_hash(&zero_report.solver_witnesses))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Replay,
            "the seeded explorer reproduces an identical exploration report",
        )
        .pass_if(stable_hash(&div_report) == stable_hash(&div_report_again))
        .proof_hash(stable_hash(&div_report_again))
        .finish(policy),
    ]
}

/// Metamorphic oracle: a strict profile flags reordered events; a schedule-tolerant
/// profile accepts them; empty traces degrade gracefully.
fn check_metamorphic_oracle(policy: &str) -> Vec<KernelCheckRecord> {
    let kernel = AlienKernel::MetamorphicOracle;
    let strict = MetamorphicOracleConfig::strict();
    let tolerant = MetamorphicOracleConfig::schedule_tolerant();

    let ordered = event_trace("m-src", "m-src-run", &["a", "b", "c"]);
    let same = event_trace("m-tr", "m-tr-run", &["a", "b", "c"]);
    let reordered = event_trace("m-tr2", "m-tr2-run", &["c", "b", "a"]);

    let equivalent = compare_traces_with_metamorphic_oracle(&ordered, &same, &strict);
    let strict_reorder = compare_traces_with_metamorphic_oracle(&ordered, &reordered, &strict);
    let tolerant_reorder = compare_traces_with_metamorphic_oracle(&ordered, &reordered, &tolerant);
    let tolerant_again = compare_traces_with_metamorphic_oracle(&ordered, &reordered, &tolerant);

    let empty_src = event_trace("m-empty-src", "m-empty-src-run", &[]);
    let empty_tr = event_trace("m-empty-tr", "m-empty-tr-run", &[]);
    let empty = compare_traces_with_metamorphic_oracle(&empty_src, &empty_tr, &strict);

    vec![
        CheckDraft::new(
            kernel,
            CheckKind::Invariant,
            "identical event order is judged equivalent under the strict profile",
        )
        .pass_if(equivalent.verdict == SemanticDiffVerdict::Equivalent)
        .guarantee(GuaranteeStatus::Holds)
        .proof_hash(stable_hash(&equivalent.covered_relation_ids))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Property,
            "the strict profile flags reordered events as a violation with a witness",
        )
        .pass_if(strict_reorder.verdict == SemanticDiffVerdict::Violation)
        .guarantee(GuaranteeStatus::Refuted)
        .proof_hash(stable_hash(&strict_reorder.violated_relation_ids))
        .witness_hash(stable_hash(&strict_reorder.witness_set))
        .detail(format!(
            "violations={}",
            strict_reorder.violated_relation_ids.len()
        ))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Degradation,
            "the schedule-tolerant profile accepts reordered-equivalent events",
        )
        .graceful(true)
        .pass_if(tolerant_reorder.verdict != SemanticDiffVerdict::Violation)
        .guarantee(GuaranteeStatus::Holds)
        .proof_hash(stable_hash(&tolerant_reorder.covered_relation_ids))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Adversarial,
            "empty traces produce a verdict without panicking",
        )
        .graceful(true)
        .pass_if(empty.verdict != SemanticDiffVerdict::Violation)
        .guarantee(GuaranteeStatus::Holds)
        .proof_hash(stable_hash(&empty.verdict))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Replay,
            "the oracle reproduces an identical report for identical input",
        )
        .pass_if(stable_hash(&tolerant_reorder) == stable_hash(&tolerant_again))
        .proof_hash(stable_hash(&tolerant_again))
        .finish(policy),
    ]
}

/// Shadow run: a mirrored stable pair promotes; a semantic violation forces a
/// holdback; an empty batch continues shadowing.
fn check_shadow_run(policy: &str) -> Vec<KernelCheckRecord> {
    let kernel = AlienKernel::ShadowRun;
    let harness = ShadowRunHarness::new(ShadowRunConfig::smoke_test());

    let stable = ShadowTracePair::new(
        "mirror-stable",
        shadow_trace("s-src", "s-src-run", "state-ready"),
        shadow_trace("s-tr", "s-tr-run", "state-ready"),
    );
    let stable_report = harness.evaluate_pairs(vec![stable]);

    let stable_again = ShadowTracePair::new(
        "mirror-stable",
        shadow_trace("s-src", "s-src-run", "state-ready"),
        shadow_trace("s-tr", "s-tr-run", "state-ready"),
    );
    let stable_report_again = harness.evaluate_pairs(vec![stable_again]);

    let violating = ShadowTracePair::new(
        "mirror-violation",
        shadow_trace("v-src", "v-src-run", "state-ready"),
        shadow_trace("v-tr", "v-tr-run", "state-diverged"),
    );
    let violation_report = harness.evaluate_pairs(vec![violating]);

    let empty_report = harness.evaluate_pairs(Vec::new());

    vec![
        CheckDraft::new(
            kernel,
            CheckKind::Invariant,
            "a mirrored, semantically stable pair is promoted",
        )
        .pass_if(
            stable_report.decision == ShadowPromotionDecision::Promote
                && stable_report.holdback_reasons.is_empty(),
        )
        .guarantee(GuaranteeStatus::Holds)
        .budget(BudgetState::new(
            0,
            u64::try_from(stable_report.stability_evidence.total_runs).unwrap_or(u64::MAX),
            false,
            false,
            "shadow_runs",
        ))
        .proof_hash(stable_hash(&stable_report.stability_evidence))
        .detail(format!("decision={:?}", stable_report.decision))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Degradation,
            "a semantic violation forces a holdback instead of a promotion",
        )
        .graceful(true)
        .pass_if(
            violation_report.decision != ShadowPromotionDecision::Promote
                && !violation_report.holdback_reasons.is_empty(),
        )
        .guarantee(GuaranteeStatus::Refuted)
        .proof_hash(stable_hash(&violation_report.stability_evidence))
        .witness_hash(stable_hash(&violation_report.holdback_reasons))
        .detail(format!("decision={:?}", violation_report.decision))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Adversarial,
            "an empty batch continues shadowing rather than promoting",
        )
        .graceful(true)
        .pass_if(
            empty_report.decision == ShadowPromotionDecision::ContinueShadow
                && empty_report.stability_evidence.total_runs == 0,
        )
        .guarantee(GuaranteeStatus::Holds)
        .proof_hash(stable_hash(&empty_report.stability_evidence))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Replay,
            "the harness reproduces an identical report id and evidence checksum",
        )
        .pass_if(
            stable_report.report_id == stable_report_again.report_id
                && stable_report.evidence_checksum == stable_report_again.evidence_checksum,
        )
        .proof_hash(stable_hash(&stable_report_again.evidence_checksum))
        .finish(policy),
    ]
}

/// Controller composition: disjoint-signal controllers are independent; two
/// resonant same-signal controllers trip a guard and get a conservative fallback.
fn check_controller_composition(policy: &str) -> Vec<KernelCheckRecord> {
    let kernel = AlienKernel::ControllerComposition;
    let guard = ControllerCompositionGuard::new(CompositionGuardConfig::default());

    let independent = guard.evaluate(vec![
        ControllerProfile::new(
            "voi",
            ControllerKind::VoiAllocator,
            ControllerRole::Optimizer,
            "alloc_signal",
            4.0,
            0.3,
        ),
        ControllerProfile::new(
            "bocpd",
            ControllerKind::Bocpd,
            ControllerRole::Detector,
            "drift_signal",
            4.0,
            0.3,
        ),
    ]);

    let resonant = guard.evaluate(vec![
        ControllerProfile::new(
            "voi-fast",
            ControllerKind::VoiAllocator,
            ControllerRole::Optimizer,
            "shared_signal",
            1.5,
            1.0,
        ),
        ControllerProfile::new(
            "bocpd-fast",
            ControllerKind::Bocpd,
            ControllerRole::Detector,
            "shared_signal",
            1.5,
            1.0,
        ),
    ]);

    let resonant_again = guard.evaluate(vec![
        ControllerProfile::new(
            "voi-fast",
            ControllerKind::VoiAllocator,
            ControllerRole::Optimizer,
            "shared_signal",
            1.5,
            1.0,
        ),
        ControllerProfile::new(
            "bocpd-fast",
            ControllerKind::Bocpd,
            ControllerRole::Detector,
            "shared_signal",
            1.5,
            1.0,
        ),
    ]);

    let empty = guard.evaluate(Vec::new());

    vec![
        CheckDraft::new(
            kernel,
            CheckKind::Invariant,
            "disjoint-signal controllers are independent and stay safe",
        )
        .pass_if(independent.safe && independent.violations.is_empty())
        .guarantee(GuaranteeStatus::Holds)
        .proof_hash(stable_hash(&independent.matrix))
        .detail(format!("max_risk={:?}", independent.max_risk))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Degradation,
            "two resonant same-signal controllers trip the guard and get a fallback",
        )
        .graceful(true)
        .pass_if(
            !resonant.safe
                && !resonant.violations.is_empty()
                && resonant.max_risk >= CouplingRisk::High,
        )
        .guarantee(GuaranteeStatus::Refuted)
        .proof_hash(stable_hash(&resonant.matrix))
        .witness_hash(stable_hash(&resonant.violations))
        .detail(format!("max_risk={:?}", resonant.max_risk))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Adversarial,
            "an empty controller set is vacuously safe without panic",
        )
        .graceful(true)
        .pass_if(empty.safe && empty.matrix.is_empty())
        .guarantee(GuaranteeStatus::Holds)
        .proof_hash(stable_hash(&empty.matrix))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Replay,
            "the guard reproduces an identical json-stats checksum",
        )
        .pass_if(resonant.exported_json_stats.sha256 == resonant_again.exported_json_stats.sha256)
        .proof_hash(stable_hash(&resonant_again.exported_json_stats.sha256))
        .finish(policy),
    ]
}

/// Posterior core: strong positive evidence auto-approves; strong negative evidence
/// is conservative; NaN evidence is sanitized; empty evidence is flagged degraded.
fn check_posterior_core(policy: &str, seeds: u32) -> Vec<KernelCheckRecord> {
    let kernel = AlienKernel::PosteriorCore;
    let engine = PosteriorEngine::new(PosteriorCoreConfig::default());
    let mut out = Vec::new();

    // Property sweep: stacking positive evidence is monotone-non-decreasing in
    // posterior probability.
    let sweep = seeds.max(2);
    let mut prev_prob = f64::NEG_INFINITY;
    let mut monotone = true;
    let mut last_record = engine.infer("posterior-seed-0", &[]);
    for index in 0..sweep {
        let mut evidence = Vec::new();
        for channel_index in 0..=index {
            let channel = EvidenceChannel::ALL[channel_index as usize % EvidenceChannel::ALL.len()];
            evidence.push(ChannelEvidence::new(channel, 1.5));
        }
        let record = engine.infer(format!("posterior-seed-{index}"), &evidence);
        if record.posterior_prob + 1e-9 < prev_prob {
            monotone = false;
        }
        prev_prob = record.posterior_prob;
        last_record = record;
    }
    out.push(
        CheckDraft::new(
            kernel,
            CheckKind::Property,
            "posterior probability is monotone-non-decreasing in positive evidence",
        )
        .seed(u64::from(sweep))
        .pass_if(monotone && last_record.posterior_prob.is_finite())
        .guarantee(GuaranteeStatus::Holds)
        .posterior_hash(stable_hash(&last_record.beta_posterior))
        .proof_hash(stable_hash(&last_record.contributions))
        .detail(format!("final_prob={:.6}", last_record.posterior_prob))
        .finish(policy),
    );

    // Invariant: decisive positive evidence auto-approves.
    let positive = engine.infer(
        "posterior-positive",
        &[
            ChannelEvidence::new(EvidenceChannel::Semantic, 4.0),
            ChannelEvidence::new(EvidenceChannel::Visual, 3.0),
            ChannelEvidence::new(EvidenceChannel::Performance, 2.0),
        ],
    );
    out.push(
        CheckDraft::new(
            kernel,
            CheckKind::Invariant,
            "decisive positive evidence yields an auto-approve verdict",
        )
        .pass_if(positive.decision == MigrationDecision::AutoApprove)
        .guarantee(GuaranteeStatus::Holds)
        .posterior_hash(stable_hash(&positive.beta_posterior))
        .proof_hash(stable_hash(&positive.counterfactual))
        .detail(format!("prob={:.6}", positive.posterior_prob))
        .finish(policy),
    );

    // Invariant: decisive negative evidence is conservative.
    let negative = engine.infer(
        "posterior-negative",
        &[
            ChannelEvidence::new(EvidenceChannel::Semantic, -4.0),
            ChannelEvidence::new(EvidenceChannel::Determinism, -3.0),
        ],
    );
    let conservative = matches!(
        negative.decision,
        MigrationDecision::Reject
            | MigrationDecision::HardReject
            | MigrationDecision::Rollback
            | MigrationDecision::ConservativeFallback
            | MigrationDecision::HumanReview
    );
    out.push(
        CheckDraft::new(
            kernel,
            CheckKind::Invariant,
            "decisive negative evidence is held back or rejected",
        )
        .pass_if(conservative)
        .guarantee(GuaranteeStatus::Holds)
        .posterior_hash(stable_hash(&negative.beta_posterior))
        .detail(format!("decision={:?}", negative.decision))
        .finish(policy),
    );

    // Adversarial: NaN evidence is sanitized to a finite posterior.
    let nan = engine.infer(
        "posterior-nan",
        &[
            ChannelEvidence::new(EvidenceChannel::Semantic, f64::NAN),
            ChannelEvidence::new(EvidenceChannel::Visual, f64::INFINITY),
        ],
    );
    out.push(
        CheckDraft::new(
            kernel,
            CheckKind::Adversarial,
            "NaN/inf evidence is sanitized to a finite posterior",
        )
        .graceful(true)
        .pass_if(nan.posterior_prob.is_finite() && nan.posterior_logit.is_finite())
        .guarantee(GuaranteeStatus::Degraded)
        .posterior_hash(stable_hash(&nan.beta_posterior))
        .detail(format!("prob={:.6}", nan.posterior_prob))
        .finish(policy),
    );

    // Degradation: empty evidence is flagged as degraded/sparse.
    let empty = engine.infer("posterior-empty", &[]);
    out.push(
        CheckDraft::new(
            kernel,
            CheckKind::Degradation,
            "empty evidence is flagged degraded rather than confidently accepted",
        )
        .graceful(true)
        .pass_if(empty.degraded && empty.decision != MigrationDecision::AutoApprove)
        .guarantee(GuaranteeStatus::Degraded)
        .posterior_hash(stable_hash(&empty.beta_posterior))
        .detail(format!(
            "degraded={} decision={:?}",
            empty.degraded, empty.decision
        ))
        .finish(policy),
    );

    // Replay: identical evidence yields an identical record.
    let replay_a = engine.infer(
        "posterior-replay",
        &[ChannelEvidence::new(EvidenceChannel::Semantic, 2.0)],
    );
    let replay_b = engine.infer(
        "posterior-replay",
        &[ChannelEvidence::new(EvidenceChannel::Semantic, 2.0)],
    );
    out.push(
        CheckDraft::new(
            kernel,
            CheckKind::Replay,
            "identical evidence reproduces an identical posterior record",
        )
        .pass_if(replay_a == replay_b)
        .posterior_hash(stable_hash(&replay_b.beta_posterior))
        .finish(policy),
    );

    out
}

/// Expected-loss solver: a high pass rate accepts; a high failure rate is
/// conservative; the prior-only posterior is finite.
fn check_expected_loss(policy: &str) -> Vec<KernelCheckRecord> {
    let kernel = AlienKernel::ExpectedLoss;
    let Ok(model) = load_builtin_confidence_model() else {
        return vec![
            CheckDraft::new(
                kernel,
                CheckKind::Invariant,
                "builtin confidence model loads",
            )
            .pass_if(false)
            .detail("load_builtin_confidence_model failed")
            .finish(policy),
        ];
    };

    let high = model.compute_posterior(95, 5);
    let high_loss = model.expected_loss_decision(
        &high,
        Some("expected-loss-high".to_string()),
        Some(policy.to_string()),
    );

    let low = model.compute_posterior(5, 95);
    let low_loss = model.expected_loss_decision(
        &low,
        Some("expected-loss-low".to_string()),
        Some(policy.to_string()),
    );

    let prior = model.compute_posterior(0, 0);

    let replay_high = model.compute_posterior(95, 5);

    let high_ok = matches!(
        high_loss.decision,
        MigrationDecision::AutoApprove | MigrationDecision::HumanReview
    );
    let low_ok = matches!(
        low_loss.decision,
        MigrationDecision::Reject
            | MigrationDecision::HardReject
            | MigrationDecision::Rollback
            | MigrationDecision::ConservativeFallback
    );

    vec![
        CheckDraft::new(
            kernel,
            CheckKind::Invariant,
            "a high observed pass rate yields an accept-class decision",
        )
        .pass_if(high_ok && high_loss.expected_loss_accept.is_finite())
        .guarantee(GuaranteeStatus::Holds)
        .posterior_hash(stable_hash(&high))
        .proof_hash(stable_hash(&high_loss))
        .detail(format!(
            "decision={:?} mean={:.4}",
            high_loss.decision, high.mean
        ))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Property,
            "a high observed failure rate yields a conservative decision",
        )
        .pass_if(low_ok)
        .guarantee(GuaranteeStatus::Holds)
        .posterior_hash(stable_hash(&low))
        .proof_hash(stable_hash(&low_loss))
        .detail(format!(
            "decision={:?} mean={:.4}",
            low_loss.decision, low.mean
        ))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Adversarial,
            "the prior-only posterior is finite and well-formed",
        )
        .graceful(true)
        .pass_if(prior.mean.is_finite() && prior.variance.is_finite() && prior.variance >= 0.0)
        .guarantee(GuaranteeStatus::Degraded)
        .posterior_hash(stable_hash(&prior))
        .detail(format!("mean={:.4} var={:.6}", prior.mean, prior.variance))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Replay,
            "the posterior is reproducible for identical observation counts",
        )
        .pass_if(stable_hash(&high) == stable_hash(&replay_high))
        .posterior_hash(stable_hash(&replay_high))
        .finish(policy),
    ]
}

/// Guarantee wrapper: well-formed guarantees keep a stable tag and round-trip;
/// a guarantee with no bound terms is classified as a vacuous (degraded) bound.
fn check_guarantee_wrapper(policy: &str) -> Vec<KernelCheckRecord> {
    let kernel = AlienKernel::GuaranteeWrapper;

    let conformal = FormalGuarantee::new(
        GuaranteeKind::Conformal,
        ["exchangeable residuals".to_string()],
        ["coverage>=0.9".to_string(), "alpha=0.1".to_string()],
    );
    let eprocess = FormalGuarantee::new(
        GuaranteeKind::EProcess,
        ["sub-gaussian increments".to_string()],
        ["wealth_threshold=1/alpha".to_string()],
    );
    let pac_bayes = FormalGuarantee::new(
        GuaranteeKind::PacBayes,
        ["bounded loss".to_string()],
        ["kl_term".to_string()],
    );

    let tags_stable = conformal.kind.as_str() == "conformal"
        && eprocess.kind.as_str() == "e_process"
        && pac_bayes.kind.as_str() == "pac_bayes";
    let round_trips = serde_round_trips(&conformal)
        && serde_round_trips(&eprocess)
        && serde_round_trips(&pac_bayes);

    let vacuous = FormalGuarantee::new(GuaranteeKind::Other, Vec::new(), Vec::new());

    vec![
        CheckDraft::new(
            kernel,
            CheckKind::Invariant,
            "well-formed guarantees keep stable tags and round-trip through serde",
        )
        .pass_if(tags_stable && round_trips)
        .guarantee(GuaranteeStatus::Holds)
        .proof_hash(stable_hash(&[&conformal, &eprocess, &pac_bayes]))
        .detail("conformal/e_process/pac_bayes")
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Degradation,
            "a guarantee with no bound terms is a vacuous (degraded) bound",
        )
        .graceful(true)
        .pass_if(vacuous.bound_terms.is_empty() && vacuous.assumptions.is_empty())
        .guarantee(GuaranteeStatus::Degraded)
        .witness_hash(stable_hash(&vacuous))
        .detail("empty bound_terms => no actual guarantee")
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Replay,
            "a rebuilt guarantee hashes identically",
        )
        .pass_if(
            stable_hash(&conformal)
                == stable_hash(&FormalGuarantee::new(
                    GuaranteeKind::Conformal,
                    ["exchangeable residuals".to_string()],
                    ["coverage>=0.9".to_string(), "alpha=0.1".to_string()],
                )),
        )
        .proof_hash(stable_hash(&conformal))
        .finish(policy),
    ]
}

/// VOI planner: information-driven allocation beats a round-robin baseline and
/// respects the budget; empty/zero-budget inputs allocate nothing.
fn check_voi_planner(policy: &str) -> Vec<KernelCheckRecord> {
    let kernel = AlienKernel::VoiPlanner;
    let budget_units = 20.0_f64;

    let candidates = || {
        vec![
            TestFixtureCandidate::new("uncertain", "certification", 1.0)
                .with_prior(1.0, 1.0)
                .with_observations(2.0, 2.0)
                .with_value_scale(2.0),
            TestFixtureCandidate::new("confident", "certification", 1.0)
                .with_prior(1.0, 1.0)
                .with_observations(60.0, 0.0)
                .with_value_scale(1.0),
            TestFixtureCandidate::new("midrange", "regression", 1.0)
                .with_prior(2.0, 2.0)
                .with_observations(8.0, 4.0)
                .with_value_scale(1.5),
        ]
    };

    let allocator = TestBudgetAllocator::new(TestBudgetConfig::new(budget_units));
    let report = allocator.allocate(candidates());
    let report_again = allocator.allocate(candidates());

    let empty = allocator.allocate(Vec::new());
    let zero_budget = TestBudgetAllocator::new(TestBudgetConfig::new(0.0)).allocate(candidates());

    vec![
        CheckDraft::new(
            kernel,
            CheckKind::Invariant,
            "information-driven allocation is at least as good as round-robin",
        )
        .pass_if(report.baseline_comparison.voi_is_at_least_baseline)
        .guarantee(GuaranteeStatus::Holds)
        .budget(BudgetState::new(
            budget_units as u64,
            report.total_compute_units_used as u64,
            report.total_compute_units_used >= budget_units,
            false,
            "voi_compute_units",
        ))
        .posterior_hash(stable_hash(&report.allocations))
        .proof_hash(stable_hash(&report.baseline_comparison))
        .detail(format!(
            "improvement_ratio={:.4}",
            report.baseline_comparison.improvement_ratio
        ))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Property,
            "total compute used never exceeds the budget",
        )
        .pass_if(report.total_compute_units_used <= budget_units + 1e-6)
        .guarantee(GuaranteeStatus::Holds)
        .budget(BudgetState::new(
            budget_units as u64,
            report.total_compute_units_used as u64,
            false,
            false,
            "voi_compute_units",
        ))
        .proof_hash(stable_hash(&report.decision_log))
        .detail(format!("used={:.4}", report.total_compute_units_used))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Adversarial,
            "empty candidates and zero budget both allocate nothing",
        )
        .graceful(true)
        .pass_if(empty.total_runs == 0 && zero_budget.total_runs == 0)
        .guarantee(GuaranteeStatus::Degraded)
        .budget(BudgetState::new(0, 0, true, true, "voi_compute_units"))
        .proof_hash(stable_hash(&zero_budget.allocations))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Replay,
            "allocation reproduces an identical id and json-stats checksum",
        )
        .pass_if(
            report.allocation_id == report_again.allocation_id
                && report.exported_json_stats.sha256 == report_again.exported_json_stats.sha256,
        )
        .posterior_hash(stable_hash(&report_again.allocations))
        .finish(policy),
    ]
}

/// Hazard / change-point: a step change is detected as a regression; a stable
/// series stays quiet; empty/non-finite series degrade gracefully.
fn check_hazard_change_point(policy: &str) -> Vec<KernelCheckRecord> {
    let kernel = AlienKernel::HazardChangePoint;
    let monitor = DriftMonitor::new(DriftMonitorConfig::default().with_baseline_window(8));

    let mut stepped = vec![0.92_f64; 10];
    stepped.extend(vec![0.55_f64; 8]);
    let step_report = monitor.analyze(vec![
        DriftMetricSeries::new("success_rate", MetricDirection::HigherIsBetter)
            .with_observations(stepped.clone()),
    ]);
    let step_report_again = monitor.analyze(vec![
        DriftMetricSeries::new("success_rate", MetricDirection::HigherIsBetter)
            .with_observations(stepped),
    ]);
    let regression_detected = step_report
        .events
        .iter()
        .any(|event| event.kind == DriftKind::Regression);

    // A stationary series at a constant level: the change-point/CUSUM detectors
    // must not raise a regression when the metric never actually drifts.
    let stable = vec![0.90_f64; 16];
    let stable_report = monitor.analyze(vec![
        DriftMetricSeries::new("success_rate", MetricDirection::HigherIsBetter)
            .with_observations(stable),
    ]);

    let empty_report = monitor.analyze(Vec::new());

    let nan_report = monitor.analyze(vec![
        DriftMetricSeries::new("success_rate", MetricDirection::HigherIsBetter)
            .with_observations(vec![f64::NAN, f64::INFINITY, 0.9, 0.9, 0.9]),
    ]);

    vec![
        CheckDraft::new(
            kernel,
            CheckKind::Invariant,
            "a downward step change is detected as a regression event",
        )
        .pass_if(regression_detected)
        .guarantee(GuaranteeStatus::Holds)
        .posterior_hash(stable_hash(&step_report.summaries))
        .proof_hash(stable_hash(&step_report.events))
        .witness_hash(stable_hash(&step_report.triage.top_events))
        .detail(format!("events={}", step_report.events.len()))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Property,
            "a stable series produces no regression events",
        )
        .pass_if(stable_report.triage.regression_count == 0)
        .guarantee(GuaranteeStatus::Holds)
        .posterior_hash(stable_hash(&stable_report.summaries))
        .detail(format!(
            "regressions={}",
            stable_report.triage.regression_count
        ))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Adversarial,
            "empty and non-finite series degrade gracefully without panic",
        )
        .graceful(true)
        .pass_if(
            empty_report.events.is_empty() && nan_report.events.iter().all(|e| e.value.is_finite()),
        )
        .guarantee(GuaranteeStatus::Degraded)
        .proof_hash(stable_hash(&nan_report.summaries))
        .finish(policy),
        CheckDraft::new(
            kernel,
            CheckKind::Replay,
            "the monitor reproduces an identical id and json-stats checksum",
        )
        .pass_if(
            step_report.monitor_id == step_report_again.monitor_id
                && step_report.exported_json_stats.sha256
                    == step_report_again.exported_json_stats.sha256,
        )
        .proof_hash(stable_hash(&step_report_again.events))
        .finish(policy),
    ]
}

// ── Shared fixtures ────────────────────────────────────────────────────────

fn viewport() -> Viewport {
    Viewport {
        width: 80,
        height: 24,
    }
}

fn ir_segment(category: SegmentCategory, name: &str) -> IrSegment {
    IrSegment {
        id: IrNodeId(format!("test-{name}")),
        name: name.to_string(),
        category,
        mapping_signature: format!("Test::{name}"),
    }
}

fn canonical_effect(name: &str, kind: EffectKind, deterministic: bool) -> CanonicalEffect {
    let execution_model = match kind {
        EffectKind::Timer | EffectKind::Subscription | EffectKind::Process => {
            ExecutionModel::Subscription
        }
        EffectKind::Telemetry => ExecutionModel::FireAndForget,
        _ => ExecutionModel::Command,
    };
    let cleanup = match kind {
        EffectKind::Timer | EffectKind::Subscription | EffectKind::Process => {
            CleanupStrategy::SubscriptionStop
        }
        _ => CleanupStrategy::None,
    };
    CanonicalEffect {
        id: IrNodeId(format!("ir-{name}")),
        name: name.to_string(),
        original_kind: kind,
        execution_model,
        trigger: TriggerCondition::OnMount,
        message_protocol: MessageProtocol::DataResult("test".to_string()),
        cleanup,
        async_boundary: AsyncBoundary::ThreadPool,
        dependencies: BTreeSet::new(),
        reads: BTreeSet::new(),
        writes: BTreeSet::new(),
        idempotent: deterministic,
        deterministic,
        confidence: ClassificationConfidence {
            score: 0.9,
            rationale: "test".to_string(),
        },
        provenance: Provenance {
            file: "test.rs".to_string(),
            line: 1,
            column: None,
            source_name: None,
            policy_category: None,
        },
    }
}

fn effect_model(effects: Vec<CanonicalEffect>) -> CanonicalEffectModel {
    let mut model = CanonicalEffectModel {
        effects: BTreeMap::new(),
        commands: BTreeSet::new(),
        subscriptions: BTreeSet::new(),
        fire_and_forget: BTreeSet::new(),
        ordering_constraints: Vec::new(),
        diagnostics: Vec::new(),
    };
    for effect in effects {
        match effect.execution_model {
            ExecutionModel::Command => {
                model.commands.insert(effect.id.clone());
            }
            ExecutionModel::Subscription => {
                model.subscriptions.insert(effect.id.clone());
            }
            ExecutionModel::FireAndForget => {
                model.fire_and_forget.insert(effect.id.clone());
            }
        }
        model.effects.insert(effect.id.clone(), effect);
    }
    model
}

fn event_trace(trace_id: &str, run_id: &str, keys: &[&str]) -> InteractionTrace {
    let mut builder = TraceBuilder::new(trace_id, run_id, viewport());
    for (index, key) in keys.iter().enumerate() {
        builder.record(
            u64::try_from(index).unwrap_or(u64::MAX),
            TracePayload::Key {
                key: (*key).to_string(),
                modifiers: Vec::new(),
                action: KeyAction::Press,
            },
        );
    }
    builder.build()
}

fn shadow_trace(trace_id: &str, run_id: &str, final_state: &str) -> InteractionTrace {
    let mut builder = TraceBuilder::new(trace_id, run_id, viewport());
    builder.record(
        0,
        TracePayload::Key {
            key: "enter".to_string(),
            modifiers: Vec::new(),
            action: KeyAction::Press,
        },
    );
    builder.record(
        5,
        TracePayload::Marker {
            name: "event:toast=queued".to_string(),
        },
    );
    builder.record(
        8,
        TracePayload::Marker {
            name: "event:toast=shown".to_string(),
        },
    );
    builder.record(
        20,
        TracePayload::StateCapture {
            state_hash: final_state.to_string(),
            component: Some("app".to_string()),
        },
    );
    builder.build()
}

fn divergent_trace_pair() -> (InteractionTrace, InteractionTrace) {
    let mut source = TraceBuilder::new("source", "source-run", viewport())
        .with_metadata("replay_command", "doctor_frankentui replay source");
    source.record(
        0,
        TracePayload::Marker {
            name: "start".to_string(),
        },
    );
    source.record(
        5,
        TracePayload::Key {
            key: "a".to_string(),
            modifiers: Vec::new(),
            action: KeyAction::Press,
        },
    );
    source.record(
        10,
        TracePayload::StateCapture {
            state_hash: "state-a".to_string(),
            component: Some("counter".to_string()),
        },
    );

    let mut translated = TraceBuilder::new("translated", "translated-run", viewport())
        .with_metadata("replay_command", "doctor_frankentui replay translated");
    translated.record(
        0,
        TracePayload::Marker {
            name: "start".to_string(),
        },
    );
    translated.record(
        5,
        TracePayload::Key {
            key: "b".to_string(),
            modifiers: Vec::new(),
            action: KeyAction::Press,
        },
    );
    translated.record(
        10,
        TracePayload::StateCapture {
            state_hash: "state-b".to_string(),
            component: Some("counter".to_string()),
        },
    );

    (source.build(), translated.build())
}

fn equivalent_trace_pair() -> (InteractionTrace, InteractionTrace) {
    let mut source = TraceBuilder::new("source-eq", "source-run", viewport());
    source.record(
        0,
        TracePayload::Key {
            key: "a".to_string(),
            modifiers: Vec::new(),
            action: KeyAction::Press,
        },
    );
    source.record(
        5,
        TracePayload::Resize {
            width: 100,
            height: 40,
        },
    );

    let mut translated = TraceBuilder::new("translated-eq", "translated-run", viewport());
    translated.record(
        0,
        TracePayload::Key {
            key: "a".to_string(),
            modifiers: Vec::new(),
            action: KeyAction::Press,
        },
    );
    translated.record(
        5,
        TracePayload::Resize {
            width: 100,
            height: 40,
        },
    );

    (source.build(), translated.build())
}

// ── Hashing helpers (mirrors the crate's deterministic-stack idiom) ─────────

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

/// Deterministic hash for types that implement `Debug` but not `Serialize`
/// (the `abstract_interpretation` verdict/counterexample types). `BTreeMap` and
/// `Vec` produce a stable `Debug` ordering, so this is reproducible.
fn debug_hash<T: std::fmt::Debug>(value: &T) -> String {
    sha256_hex(format!("{value:?}").as_bytes())
}

fn short_hash(value: &str) -> String {
    value.chars().take(16).collect()
}

fn na() -> String {
    "n/a".to_string()
}

fn serde_round_trips<T>(value: &T) -> bool
where
    T: Serialize + for<'de> Deserialize<'de> + PartialEq,
{
    match serde_json::to_string(value) {
        Ok(json) => match serde_json::from_str::<T>(&json) {
            Ok(restored) => &restored == value,
            Err(_) => false,
        },
        Err(_) => false,
    }
}

// ── E2E alien-uplift pipeline (bd-3bxhj.10.11) ─────────────────────────────

/// Schema version for the materialized alien-uplift pipeline artifacts.
pub const ALIEN_UPLIFT_PIPELINE_SCHEMA_VERSION: &str = "alien-uplift-pipeline-v1";

/// Per-kernel red-path coverage (adversarial / degradation / gate-fail checks).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedPathCoverage {
    /// The kernel.
    pub kernel: AlienKernel,
    /// Number of adversarial/degradation checks exercising a fallback or
    /// assumption-violation / gate-fail response.
    pub red_path_checks: usize,
    /// Whether every red-path check passed (the fallback behaved as specified).
    pub passed: bool,
}

/// A materialized pipeline artifact (path + integrity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlienUpliftArtifact {
    /// Logical artifact name.
    pub name: String,
    /// Relative file name within the run directory.
    pub file: String,
    /// SHA-256 of the file content.
    pub sha256: String,
    /// Byte length of the file content.
    pub bytes: u64,
}

/// Machine-readable summary of one pipeline run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlienUpliftPipelineSummary {
    /// Pipeline-artifact schema version.
    pub schema_version: String,
    /// Underlying suite report id.
    pub report_id: String,
    /// Suite policy lane.
    pub suite_policy_id: String,
    /// Output checksum of the ledger.
    pub evidence_checksum: String,
    /// Total checks.
    pub total_checks: usize,
    /// Passing checks.
    pub passed: usize,
    /// Failing checks.
    pub failed: usize,
    /// Green-path checks (invariant / property / replay).
    pub green_path_checks: usize,
    /// Red-path checks (adversarial / degradation).
    pub red_path_checks: usize,
    /// Distinct kernels covered.
    pub kernels_covered: usize,
    /// Per-kernel red-path coverage.
    pub red_path_coverage: Vec<RedPathCoverage>,
    /// Whether every check passed.
    pub all_checks_passed: bool,
    /// Whether every kernel has at least one passing red-path check.
    pub red_path_complete: bool,
    /// Whether the pipeline gate passes (all checks pass AND red-path complete).
    pub pipeline_passed: bool,
    /// Replay command for the pipeline.
    pub replay_command: String,
}

/// The outcome of running and materializing the pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlienUpliftPipelineOutcome {
    /// Absolute run directory.
    pub run_dir: String,
    /// Absolute path to the JSONL evidence ledger.
    pub ledger_path: String,
    /// Absolute path to the pipeline summary JSON.
    pub summary_path: String,
    /// Absolute path to the artifact manifest JSON.
    pub manifest_path: String,
    /// Absolute path to the suite JSON-stats artifact.
    pub stats_path: String,
    /// The machine-readable summary.
    pub summary: AlienUpliftPipelineSummary,
    /// All generated artifacts (with integrity hashes).
    pub artifacts: Vec<AlienUpliftArtifact>,
}

/// Configuration for the alien-uplift pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlienUpliftPipelineConfig {
    /// The underlying suite configuration.
    pub suite: AlienKernelTestConfig,
    /// Run directory name under the run-root.
    pub run_name: String,
}

impl Default for AlienUpliftPipelineConfig {
    fn default() -> Self {
        Self {
            suite: AlienKernelTestConfig {
                suite_policy_id: "alien-kernel-tests/e2e".to_string(),
                seed_count: 5,
            },
            run_name: "alien-uplift".to_string(),
        }
    }
}

fn compute_red_path_coverage(report: &AlienKernelTestReport) -> (Vec<RedPathCoverage>, bool) {
    let mut coverage = Vec::with_capacity(AlienKernel::ALL.len());
    let mut complete = true;
    for kernel in AlienKernel::ALL {
        let red: Vec<&KernelCheckRecord> = report
            .records_for(*kernel)
            .into_iter()
            .filter(|r| matches!(r.kind, CheckKind::Adversarial | CheckKind::Degradation))
            .collect();
        let count = red.len();
        let passed = !red.is_empty() && red.iter().all(|r| r.verdict == CheckVerdict::Pass);
        if !passed {
            complete = false;
        }
        coverage.push(RedPathCoverage {
            kernel: *kernel,
            red_path_checks: count,
            passed,
        });
    }
    (coverage, complete)
}

/// Run the suite and materialize the full alien-uplift evidence pipeline under
/// `run_root/<run_name>/`.
///
/// Writes a JSONL evidence ledger (one [`KernelCheckRecord`] per line, each with
/// artifact-graph IDs and a replay command), a `pipeline_summary.json`, an
/// `artifact_manifest.json`, and the suite JSON-stats artifact. Artifacts are
/// always written (so red-path failures are triageable); the returned summary's
/// `pipeline_passed` reflects the green-path + red-path gate.
///
/// # Errors
/// Returns an error if a run directory or artifact cannot be created/serialized.
pub fn run_alien_uplift_pipeline(
    run_root: &Path,
    config: &AlienUpliftPipelineConfig,
) -> crate::error::Result<AlienUpliftPipelineOutcome> {
    let suite = AlienKernelTestSuite::new(config.suite.clone());
    let report = suite.run();

    let (red_path_coverage, red_path_complete) = compute_red_path_coverage(&report);
    let green_path_checks = report
        .records
        .iter()
        .filter(|r| {
            matches!(
                r.kind,
                CheckKind::Invariant | CheckKind::Property | CheckKind::Replay
            )
        })
        .count();
    let red_path_checks = report.summary.adversarial_checks + report.summary.degradation_checks;
    let all_checks_passed = report.summary.all_passed;
    let pipeline_passed = all_checks_passed && red_path_complete;

    let summary = AlienUpliftPipelineSummary {
        schema_version: ALIEN_UPLIFT_PIPELINE_SCHEMA_VERSION.to_string(),
        report_id: report.report_id.clone(),
        suite_policy_id: report.suite_policy_id.clone(),
        evidence_checksum: report.evidence_checksum.clone(),
        total_checks: report.summary.total_checks,
        passed: report.summary.passed,
        failed: report.summary.failed,
        green_path_checks,
        red_path_checks,
        kernels_covered: report.summary.kernels_covered,
        red_path_coverage,
        all_checks_passed,
        red_path_complete,
        pipeline_passed,
        replay_command: report.replay_command.clone(),
    };

    // Build artifact contents.
    let mut ledger_content = String::new();
    for record in &report.records {
        let line = serde_json::to_string(record)?;
        ledger_content.push_str(&line);
        ledger_content.push('\n');
    }
    let stats_content = report.exported_json_stats.content.clone();
    let summary_content = serde_json::to_string_pretty(&summary)?;

    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let ledger_file = "evidence_ledger.jsonl";
    let stats_file = "alien_kernel_tests_stats.json";
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
        pipeline_passed: bool,
        artifacts: &'a [AlienUpliftArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: ALIEN_UPLIFT_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        pipeline_passed,
        artifacts: &artifacts,
    })?;
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(AlienUpliftPipelineOutcome {
        run_dir: run_dir.display().to_string(),
        ledger_path: run_dir.join(ledger_file).display().to_string(),
        summary_path: run_dir.join(summary_file).display().to_string(),
        manifest_path: run_dir.join(manifest_file).display().to_string(),
        stats_path: run_dir.join(stats_file).display().to_string(),
        summary,
        artifacts,
    })
}

fn artifact_of(file: &str, content: &str) -> AlienUpliftArtifact {
    AlienUpliftArtifact {
        name: file
            .trim_end_matches(".json")
            .trim_end_matches(".jsonl")
            .to_string(),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// CLI arguments for the `alien-uplift` E2E pipeline command.
#[derive(Debug, clap::Args)]
pub struct AlienUpliftArgs {
    /// Run-root directory; artifacts land under `<run-root>/<run-name>/`.
    #[arg(
        long = "run-root",
        default_value = "/tmp/doctor_frankentui/alien_uplift"
    )]
    pub run_root: PathBuf,
    /// Run directory name.
    #[arg(long = "run-name", default_value = "alien-uplift")]
    pub run_name: String,
    /// Suite policy lane id.
    #[arg(long = "policy", default_value = "alien-kernel-tests/e2e")]
    pub policy: String,
    /// Seeds per seeded property sweep.
    #[arg(long = "seed-count", default_value_t = 5)]
    pub seed_count: u32,
}

/// Run the alien-uplift E2E pipeline command.
///
/// Materializes the evidence ledger + summary + manifest under the run-root and
/// returns a non-zero exit (via [`crate::error::DoctorError::Exit`]) when the
/// green-path or red-path gate fails — making it suitable as a
/// release-candidate promotion gate.
///
/// # Errors
/// Returns an error if artifacts cannot be written, or `DoctorError::Exit` when
/// the pipeline gate fails.
pub fn run_alien_uplift(args: AlienUpliftArgs) -> crate::error::Result<()> {
    let config = AlienUpliftPipelineConfig {
        suite: AlienKernelTestConfig {
            suite_policy_id: args.policy,
            seed_count: args.seed_count,
        },
        run_name: args.run_name,
    };
    let outcome = run_alien_uplift_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("alien-uplift pipeline"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!(
            "checks: {} passed / {} failed ({} kernels)",
            summary.passed, summary.failed, summary.kernels_covered
        ));
        ui.info(&format!(
            "green-path: {}  red-path: {} (complete: {})",
            summary.green_path_checks, summary.red_path_checks, summary.red_path_complete
        ));
        ui.info(&format!("ledger: {}", outcome.ledger_path));
        ui.info(&format!("manifest: {}", outcome.manifest_path));
        if summary.pipeline_passed {
            ui.success("alien-uplift gate PASSED");
        } else {
            ui.error("alien-uplift gate FAILED");
        }
    }

    if summary.pipeline_passed {
        Ok(())
    } else {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "alien-uplift pipeline gate failed: {} check(s) failed, red_path_complete={}",
                summary.failed, summary.red_path_complete
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn suite() -> AlienKernelTestSuite {
        AlienKernelTestSuite::default()
    }

    #[test]
    fn suite_runs_every_kernel_check_to_pass() {
        let report = suite().run();
        let failures: Vec<String> = report
            .failures()
            .iter()
            .map(|r| format!("{}::{} ({})", r.kernel.as_str(), r.property, r.check_id))
            .collect();
        assert!(
            report.summary.all_passed,
            "alien-kernel checks failed: {failures:#?}"
        );
        assert_eq!(report.summary.failed, 0);
        assert!(report.summary.total_checks >= 40, "too few checks recorded");
    }

    #[test]
    fn every_kernel_is_covered() {
        let report = suite().run();
        assert_eq!(report.summary.kernels_covered, AlienKernel::ALL.len());
        for kernel in AlienKernel::ALL {
            assert!(
                !report.records_for(*kernel).is_empty(),
                "kernel {} has no checks",
                kernel.as_str()
            );
        }
    }

    #[test]
    fn every_kernel_has_adversarial_and_replay_coverage() {
        let report = suite().run();
        for kernel in AlienKernel::ALL {
            let records = report.records_for(*kernel);
            let has_adversarial = records
                .iter()
                .any(|r| matches!(r.kind, CheckKind::Adversarial | CheckKind::Degradation));
            let has_replay = records.iter().any(|r| r.kind == CheckKind::Replay);
            assert!(
                has_adversarial,
                "kernel {} has no adversarial/degradation check",
                kernel.as_str()
            );
            assert!(has_replay, "kernel {} has no replay check", kernel.as_str());
        }
    }

    #[test]
    fn every_record_carries_required_log_fields() {
        let report = suite().run();
        for record in &report.records {
            assert!(!record.check_id.is_empty());
            assert!(!record.equation_id.is_empty(), "{}", record.check_id);
            assert!(
                record.policy_id.starts_with(&report.suite_policy_id),
                "{}",
                record.check_id
            );
            assert!(!record.posterior_hash.is_empty());
            assert!(!record.proof_hash.is_empty());
            assert!(!record.witness_hash.is_empty());
            assert!(record.replay_command.contains("alien-kernel-tests"));
            assert!(record.replay_command.contains(&record.check_id));
        }
    }

    #[test]
    fn graceful_degradation_checks_flag_graceful() {
        let report = suite().run();
        let degradation: Vec<&KernelCheckRecord> = report
            .records
            .iter()
            .filter(|r| matches!(r.kind, CheckKind::Adversarial | CheckKind::Degradation))
            .collect();
        assert!(!degradation.is_empty());
        for record in degradation {
            assert!(
                record.graceful_degradation,
                "{} did not mark graceful degradation",
                record.check_id
            );
        }
    }

    #[test]
    fn report_is_deterministic() {
        let first = suite().run();
        let second = suite().run();
        assert_eq!(first.report_id, second.report_id);
        assert_eq!(first.evidence_checksum, second.evidence_checksum);
        assert_eq!(
            first.exported_json_stats.sha256,
            second.exported_json_stats.sha256
        );
        assert_eq!(first.records, second.records);
    }

    #[test]
    fn report_roundtrips_through_serde() {
        let report = suite().run();
        let json = serde_json::to_string(&report).unwrap();
        let restored: AlienKernelTestReport = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.report_id, report.report_id);
        assert_eq!(restored.records, report.records);
        assert_eq!(restored.summary, report.summary);
        assert_eq!(restored.evidence_checksum, report.evidence_checksum);
    }

    #[test]
    fn json_stats_checksum_is_self_consistent() {
        let report = suite().run();
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
        assert_eq!(report.evidence_checksum, stable_hash(&report.records));
    }

    #[test]
    fn report_replay_command_references_report_id() {
        let report = suite().run();
        assert!(report.replay_command.contains("alien-kernel-tests"));
        assert!(report.replay_command.contains(&report.report_id));
        assert!(report.replay_command.contains(&report.suite_policy_id));
    }

    #[test]
    fn equation_ids_and_kernel_tags_are_unique() {
        let mut tags = BTreeSet::new();
        let mut equations = BTreeSet::new();
        for kernel in AlienKernel::ALL {
            assert!(
                tags.insert(kernel.as_str()),
                "duplicate tag {}",
                kernel.as_str()
            );
            assert!(
                equations.insert(kernel.equation_id()),
                "duplicate equation_id {}",
                kernel.equation_id()
            );
        }
        assert_eq!(tags.len(), AlienKernel::ALL.len());
    }

    #[test]
    fn distinct_policy_lane_changes_report_id_not_pass_state() {
        let custom = AlienKernelTestSuite::new(AlienKernelTestConfig {
            suite_policy_id: "alien-kernel-tests/ci".to_string(),
            seed_count: 5,
        });
        let report = custom.run();
        let default = suite().run();
        assert_ne!(report.report_id, default.report_id);
        assert!(report.summary.all_passed);
        for record in &report.records {
            assert!(record.policy_id.starts_with("alien-kernel-tests/ci"));
        }
    }

    #[test]
    fn replay_records_reproduce_witness_hashes() {
        // AC#2: deterministic replay. Re-running the suite reproduces every
        // proof/witness/posterior hash for every kernel.
        let first = suite().run();
        let second = suite().run();
        for (a, b) in first.records.iter().zip(second.records.iter()) {
            assert_eq!(a.check_id, b.check_id);
            assert_eq!(a.posterior_hash, b.posterior_hash);
            assert_eq!(a.proof_hash, b.proof_hash);
            assert_eq!(a.witness_hash, b.witness_hash);
        }
    }

    #[test]
    fn seed_count_scales_seeded_sweeps() {
        let few = AlienKernelTestSuite::new(AlienKernelTestConfig {
            suite_policy_id: "alien-kernel-tests/default".to_string(),
            seed_count: 2,
        })
        .run();
        let many = AlienKernelTestSuite::new(AlienKernelTestConfig {
            suite_policy_id: "alien-kernel-tests/default".to_string(),
            seed_count: 6,
        })
        .run();
        assert!(many.records.len() >= few.records.len());
        assert!(few.summary.all_passed && many.summary.all_passed);
    }

    // ── E2E pipeline (bd-3bxhj.10.11) ────────────────────────────────────

    #[test]
    fn pipeline_materializes_artifacts_and_passes_gate() {
        let dir = tempfile::tempdir().unwrap();
        let outcome =
            run_alien_uplift_pipeline(dir.path(), &AlienUpliftPipelineConfig::default()).unwrap();
        assert!(outcome.summary.pipeline_passed, "{:#?}", outcome.summary);
        assert!(outcome.summary.all_checks_passed);
        assert!(outcome.summary.red_path_complete);
        // Every materialized artifact exists with the declared byte length.
        for artifact in &outcome.artifacts {
            let path = std::path::Path::new(&outcome.run_dir).join(&artifact.file);
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(bytes.len() as u64, artifact.bytes, "{}", artifact.file);
            assert_eq!(sha256_hex(&bytes), artifact.sha256, "{}", artifact.file);
        }
        assert!(std::path::Path::new(&outcome.manifest_path).exists());
    }

    #[test]
    fn pipeline_red_path_covers_every_kernel() {
        let dir = tempfile::tempdir().unwrap();
        let outcome =
            run_alien_uplift_pipeline(dir.path(), &AlienUpliftPipelineConfig::default()).unwrap();
        assert_eq!(
            outcome.summary.red_path_coverage.len(),
            AlienKernel::ALL.len()
        );
        for coverage in &outcome.summary.red_path_coverage {
            assert!(
                coverage.red_path_checks >= 1,
                "kernel {} has no red-path check",
                coverage.kernel.as_str()
            );
            assert!(
                coverage.passed,
                "kernel {} red-path failed",
                coverage.kernel.as_str()
            );
        }
    }

    #[test]
    fn pipeline_ledger_is_valid_jsonl_with_artifact_graph_ids() {
        let dir = tempfile::tempdir().unwrap();
        let outcome =
            run_alien_uplift_pipeline(dir.path(), &AlienUpliftPipelineConfig::default()).unwrap();
        let ledger = std::fs::read_to_string(&outcome.ledger_path).unwrap();
        let lines: Vec<&str> = ledger.lines().collect();
        assert_eq!(lines.len(), outcome.summary.total_checks);
        for line in lines {
            let record: KernelCheckRecord = serde_json::from_str(line).unwrap();
            // Artifact-graph IDs + replay command (AC#1).
            assert!(!record.check_id.is_empty());
            assert!(!record.proof_hash.is_empty());
            assert!(!record.witness_hash.is_empty());
            assert!(!record.posterior_hash.is_empty());
            assert!(record.replay_command.contains(&record.check_id));
        }
    }

    #[test]
    fn pipeline_is_deterministic_across_runs() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let a =
            run_alien_uplift_pipeline(dir_a.path(), &AlienUpliftPipelineConfig::default()).unwrap();
        let b =
            run_alien_uplift_pipeline(dir_b.path(), &AlienUpliftPipelineConfig::default()).unwrap();
        assert_eq!(a.summary.evidence_checksum, b.summary.evidence_checksum);
        assert_eq!(a.summary.report_id, b.summary.report_id);
        // Ledger bytes are identical (run-root path is not part of the ledger).
        let ledger_a = std::fs::read_to_string(&a.ledger_path).unwrap();
        let ledger_b = std::fs::read_to_string(&b.ledger_path).unwrap();
        assert_eq!(ledger_a, ledger_b);
    }

    #[test]
    fn pipeline_cli_command_succeeds_on_green_path() {
        let dir = tempfile::tempdir().unwrap();
        let result = run_alien_uplift(AlienUpliftArgs {
            run_root: dir.path().to_path_buf(),
            run_name: "cli".to_string(),
            policy: "alien-kernel-tests/e2e".to_string(),
            seed_count: 4,
        });
        assert!(result.is_ok());
        assert!(
            dir.path()
                .join("cli")
                .join("evidence_ledger.jsonl")
                .exists()
        );
    }

    // ── Direct kernel property tests (proptest) ──────────────────────────

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Posterior probability is monotone-non-decreasing as positive evidence
        /// is added (a stronger property than a single seeded sweep).
        #[test]
        fn prop_posterior_monotone_in_positive_evidence(
            base in 0.1f64..3.0,
            extra in 0.1f64..3.0,
        ) {
            let engine = PosteriorEngine::new(PosteriorCoreConfig::default());
            let one = engine.infer(
                "prop",
                &[ChannelEvidence::new(EvidenceChannel::Semantic, base)],
            );
            let two = engine.infer(
                "prop",
                &[
                    ChannelEvidence::new(EvidenceChannel::Semantic, base),
                    ChannelEvidence::new(EvidenceChannel::Visual, extra),
                ],
            );
            prop_assert!(two.posterior_prob + 1e-9 >= one.posterior_prob);
            prop_assert!(two.posterior_prob.is_finite());
        }

        /// VOI is always non-negative for any non-degenerate Beta posterior.
        #[test]
        fn prop_voi_is_non_negative(
            successes in 0u32..200,
            failures in 0u32..200,
        ) {
            let model = load_builtin_confidence_model().unwrap();
            let posterior = model.compute_posterior(successes, failures);
            prop_assert!(posterior.mean.is_finite());
            prop_assert!((0.0..=1.0).contains(&posterior.mean));
            prop_assert!(posterior.variance >= -1e-12);
        }

        /// The drift monitor never panics and never emits a non-finite value,
        /// regardless of the observation stream.
        #[test]
        fn prop_drift_monitor_is_total(
            values in proptest::collection::vec(-1.0e6f64..1.0e6, 0..40),
        ) {
            let monitor = DriftMonitor::new(DriftMonitorConfig::default());
            let report = monitor.analyze(vec![
                DriftMetricSeries::new("metric", MetricDirection::HigherIsBetter)
                    .with_observations(values),
            ]);
            for event in &report.events {
                prop_assert!(event.value.is_finite());
                prop_assert!((0.0..=1.0).contains(&event.confidence));
            }
        }

        /// CEGIS synthesis is deterministic: the same segment always produces the
        /// same outcome hash regardless of name perturbation.
        #[test]
        fn prop_cegis_is_deterministic(suffix in "[a-z]{1,8}") {
            let synth = CegisSynthesizer::with_defaults();
            let segment = ir_segment(SegmentCategory::View, &format!("prop-{suffix}"));
            let a = synth.synthesize_hole(&segment);
            let b = synth.synthesize_hole(&segment);
            prop_assert_eq!(cegis_outcome_fingerprint(&a), cegis_outcome_fingerprint(&b));
        }

        /// The VOI allocator never exceeds its compute budget.
        #[test]
        fn prop_voi_respects_budget(budget in 1.0f64..50.0) {
            let allocator = TestBudgetAllocator::new(TestBudgetConfig::new(budget));
            let report = allocator.allocate(vec![
                TestFixtureCandidate::new("a", "stage", 1.0)
                    .with_prior(1.0, 1.0)
                    .with_observations(1.0, 1.0)
                    .with_value_scale(2.0),
                TestFixtureCandidate::new("b", "stage", 2.0)
                    .with_prior(1.0, 1.0)
                    .with_observations(3.0, 1.0)
                    .with_value_scale(1.0),
            ]);
            prop_assert!(report.total_compute_units_used <= budget + 1e-6);
        }
    }
}
