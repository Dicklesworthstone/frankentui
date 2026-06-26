// SPDX-License-Identifier: Apache-2.0
//! Recommendation-contract compiler/linter and gate enforcement.
//!
//! Compiles alien-uplift recommendation cards into enforceable machine
//! contracts so no advanced migration feature ships with a missing decision,
//! proof, guarantee, safety, or executable-metadata field. The linter is the
//! quality firewall consumed by the certification, rollout, and alien-uplift
//! release gates.
//!
//! # Pipeline
//!
//! 1. [`compile_recommendation_contract`] lifts a
//!    [`crate::milestone_policy::RecommendationCard`] into a
//!    [`RecommendationContract`] skeleton (derivable identity/scoring fields).
//! 2. Caller attaches the remaining governance evidence with the `with_*`
//!    builders (change/hotspot evidence, entry header, budgeted mode, inference
//!    model, formal guarantees, performance/isomorphism plan, composition risk,
//!    failure-mode, graduation, demo linkage).
//! 3. [`RecommendationContractLinter`] validates the contract(s) and emits a
//!    deterministic [`ContractLintReport`] whose findings carry clause IDs,
//!    dependency hints, and a one-command replay reference.
//!
//! # Invariants
//!
//! - Every required contract clause is present, non-empty, and internally
//!   consistent (e.g. declared tier matches the card tier; EV clears the tier
//!   floor; latency targets are monotone).
//! - Each finding maps to a stable clause id and an actionable remediation.
//! - The report id and JSON-stats checksum are deterministic functions of the
//!   contracts and configuration (no clocks, no randomness).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::adversarial_fixtures::RiskClass;
use crate::milestone_policy::{PriorityTier, RecommendationCard, TierEvThresholds};
use crate::semantic_contract::IpArtifactStatus;

// ── Schema Version ───────────────────────────────────────────────────────

/// Schema version for recommendation-contract reports.
pub const RECOMMENDATION_CONTRACT_SCHEMA_VERSION: &str = "recommendation-contract-v1";

/// Tolerance for EV-floor comparisons.
const EV_EPS: f64 = 1e-9;

// ── Supporting Enums ─────────────────────────────────────────────────────

/// Lifecycle status of a graveyard entry-header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryStatus {
    /// Proposed but not yet built.
    Proposed,
    /// Implementation in progress.
    Building,
    /// Integrated into the codebase.
    Integrated,
    /// Graduated to production quality.
    Graduated,
    /// Archived / withdrawn.
    Archived,
}

impl EntryStatus {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Building => "building",
            Self::Integrated => "integrated",
            Self::Graduated => "graduated",
            Self::Archived => "archived",
        }
    }
}

/// Family of a formal guarantee attached to a contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuaranteeKind {
    /// Distribution-free conformal prediction bound.
    Conformal,
    /// Anytime-valid e-process / wealth bound.
    EProcess,
    /// PAC-Bayes generalization bound.
    PacBayes,
    /// Another formally stated guarantee.
    Other,
}

impl GuaranteeKind {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Conformal => "conformal",
            Self::EProcess => "e_process",
            Self::PacBayes => "pac_bayes",
            Self::Other => "other",
        }
    }
}

/// Status of a composition-risk test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestStatus {
    /// Not yet run.
    NotRun,
    /// Ran and passed.
    Passed,
    /// Ran and failed.
    Failed,
    /// Explicitly waived with rationale.
    Waived,
}

impl TestStatus {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotRun => "not_run",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Waived => "waived",
        }
    }
}

/// Coarse engineering-effort estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffortSize {
    /// Small change.
    Small,
    /// Medium change.
    Medium,
    /// Large change.
    Large,
    /// Extra-large change.
    XLarge,
}

impl EffortSize {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
            Self::XLarge => "xlarge",
        }
    }
}

/// Result of the graveyard-verify gate for this entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraveyardVerifyStatus {
    /// Not verified.
    NotVerified,
    /// Verified clean.
    Verified,
    /// Verification failed.
    Failed,
}

impl GraveyardVerifyStatus {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotVerified => "not_verified",
            Self::Verified => "verified",
            Self::Failed => "failed",
        }
    }
}

// ── Field-Group Structs ──────────────────────────────────────────────────

/// Change + hotspot evidence and mapped graveyard / artifact-coding sections.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeHotspotEvidence {
    /// References to the change(s) that motivate this uplift.
    pub change_refs: Vec<String>,
    /// References to the profiled hotspot(s).
    pub hotspot_refs: Vec<String>,
    /// Mapped graveyard / artifact-coding section identifiers.
    pub mapped_sections: Vec<String>,
}

/// Entry-header metadata for the graveyard provenance system.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntryHeaderMetadata {
    /// Lifecycle status.
    pub status: EntryStatus,
    /// Declared priority tier (must match the contract tier).
    pub tier: PriorityTier,
    /// Reality-check note (honest assessment of maturity).
    pub reality_check: String,
    /// Archetype label.
    pub archetype: String,
    /// Free-form tags.
    pub tags: Vec<String>,
    /// Integration targets (modules/subsystems).
    pub integration_targets: Vec<String>,
}

/// Budgeted-mode behavior and conservative fallback trigger.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BudgetedModeSpec {
    /// Whether the primitive runs in a budgeted mode.
    pub budgeted: bool,
    /// What happens when the budget is exhausted.
    pub exhaustion_behavior: String,
    /// Trigger that forces the conservative fallback.
    pub conservative_fallback_trigger: String,
}

/// Posterior / decision-model metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct InferenceModelSpec {
    /// Posterior model family (e.g. `beta_bernoulli`, `normal_normal`).
    pub posterior_model_family: String,
    /// References to the Bayes-factor evidence ledger entries.
    pub bayes_factor_refs: Vec<String>,
    /// Expected-loss model description.
    pub expected_loss_model: String,
    /// Calibration metadata (coverage targets, calibration set).
    pub calibration_metadata: String,
}

/// A single formal guarantee.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FormalGuarantee {
    /// Guarantee family.
    pub kind: GuaranteeKind,
    /// Stated assumptions the bound relies on.
    pub assumptions: Vec<String>,
    /// Bound terms / parameters (e.g. coverage, alpha, wealth threshold).
    pub bound_terms: Vec<String>,
}

impl FormalGuarantee {
    /// Build a formal guarantee.
    #[must_use]
    pub fn new(
        kind: GuaranteeKind,
        assumptions: impl IntoIterator<Item = String>,
        bound_terms: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            kind,
            assumptions: assumptions.into_iter().collect(),
            bound_terms: bound_terms.into_iter().collect(),
        }
    }
}

/// Isomorphism-proof plan and latency targets.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PerformanceContract {
    /// Plan for proving behavioral isomorphism vs the baseline.
    pub isomorphism_proof_plan: String,
    /// p50 latency target (ms).
    pub p50_target_ms: Option<f64>,
    /// p95 latency target (ms).
    pub p95_target_ms: Option<f64>,
    /// p99 latency target (ms).
    pub p99_target_ms: Option<f64>,
    /// Baseline comparator (must match the card's baseline).
    pub baseline_comparator: String,
}

/// Composition-risk test status across the interference dimensions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompositionRisk {
    /// Dependency-analysis test status.
    pub dependency_status: TestStatus,
    /// Conflict-analysis test status.
    pub conflict_status: TestStatus,
    /// Synergy-analysis test status.
    pub synergy_status: TestStatus,
    /// Interference-matrix test status.
    pub interference_status: TestStatus,
}

impl Default for CompositionRisk {
    fn default() -> Self {
        Self {
            dependency_status: TestStatus::NotRun,
            conflict_status: TestStatus::NotRun,
            synergy_status: TestStatus::NotRun,
            interference_status: TestStatus::NotRun,
        }
    }
}

/// Failure-mode risk, countermeasure, and repro/legal/provenance artifacts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FailureMode {
    /// Failure-mode risk class.
    pub risk_class: RiskClass,
    /// Countermeasure for the risk.
    pub countermeasure: String,
    /// Reproduction artifact pointer.
    pub repro_artifact: Option<String>,
    /// Legal / IP status.
    pub legal_status: IpArtifactStatus,
    /// Provenance artifact pointer.
    pub provenance_artifact: Option<String>,
}

impl Default for FailureMode {
    fn default() -> Self {
        Self {
            risk_class: RiskClass::Medium,
            countermeasure: String::new(),
            repro_artifact: None,
            legal_status: IpArtifactStatus::Unknown,
            provenance_artifact: None,
        }
    }
}

/// Graduation criteria, effort/size estimate, and graveyard-verify status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraduationSpec {
    /// Criteria that must be met to graduate.
    pub graduation_criteria: Vec<String>,
    /// Coarse effort estimate.
    pub effort_estimate: EffortSize,
    /// Estimated size in lines of code.
    pub size_estimate_loc: Option<u32>,
    /// Graveyard-verify gate status.
    pub graveyard_verify_status: GraveyardVerifyStatus,
}

impl Default for GraduationSpec {
    fn default() -> Self {
        Self {
            graduation_criteria: Vec::new(),
            effort_estimate: EffortSize::Medium,
            size_estimate_loc: None,
            graveyard_verify_status: GraveyardVerifyStatus::NotVerified,
        }
    }
}

/// Killer-demo linkage, rollback policy, and galaxy-brain references.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DemoLinkage {
    /// Demo identifier (`demo_id`).
    pub demo_id: String,
    /// Claim identifier (`claim_id`).
    pub claim_id: String,
    /// Rollback policy description.
    pub rollback_policy: String,
    /// Galaxy-brain transparency-card references.
    pub galaxy_brain_refs: Vec<String>,
}

// ── Recommendation Contract ──────────────────────────────────────────────

/// A compiled, machine-enforceable recommendation contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecommendationContract {
    /// Stable contract / card identifier.
    pub card_id: String,
    /// Human-readable title.
    pub title: String,
    /// Priority tier (S/A/B/C).
    pub priority_tier: PriorityTier,
    /// Expected-value score.
    pub ev_score: f64,
    /// Relevance score in `[0, 1]`.
    pub relevance_score: f64,
    /// Production adoption wedge.
    pub adoption_wedge: String,
    /// Canonical graveyard entry id this contract was compiled from.
    pub source_canonical_entry_id: String,
    /// Change + hotspot evidence and mapped sections.
    pub change_hotspot_evidence: ChangeHotspotEvidence,
    /// Entry-header metadata.
    pub entry_header: EntryHeaderMetadata,
    /// Budgeted-mode behavior.
    pub budgeted_mode: BudgetedModeSpec,
    /// Inference-model metadata.
    pub inference_model: InferenceModelSpec,
    /// Formal guarantees.
    pub formal_guarantees: Vec<FormalGuarantee>,
    /// Isomorphism-proof plan + latency targets.
    pub performance: PerformanceContract,
    /// Composition-risk test status.
    pub composition_risk: CompositionRisk,
    /// Failure-mode risk + artifacts.
    pub failure_mode: FailureMode,
    /// Graduation criteria + estimates.
    pub graduation: GraduationSpec,
    /// Demo linkage + rollback + galaxy-brain references.
    pub demo_linkage: DemoLinkage,
}

impl RecommendationContract {
    /// The set of guarantee kinds present, sorted and deduplicated.
    #[must_use]
    pub fn guarantee_kinds(&self) -> Vec<GuaranteeKind> {
        let mut kinds: Vec<GuaranteeKind> = self.formal_guarantees.iter().map(|g| g.kind).collect();
        kinds.sort();
        kinds.dedup();
        kinds
    }

    /// Attach change/hotspot evidence.
    #[must_use]
    pub fn with_change_hotspot_evidence(mut self, evidence: ChangeHotspotEvidence) -> Self {
        self.change_hotspot_evidence = evidence;
        self
    }

    /// Attach the entry-header metadata.
    #[must_use]
    pub fn with_entry_header(mut self, header: EntryHeaderMetadata) -> Self {
        self.entry_header = header;
        self
    }

    /// Attach the budgeted-mode spec.
    #[must_use]
    pub fn with_budgeted_mode(mut self, spec: BudgetedModeSpec) -> Self {
        self.budgeted_mode = spec;
        self
    }

    /// Attach the inference-model spec.
    #[must_use]
    pub fn with_inference_model(mut self, spec: InferenceModelSpec) -> Self {
        self.inference_model = spec;
        self
    }

    /// Attach a formal guarantee.
    #[must_use]
    pub fn with_formal_guarantee(mut self, guarantee: FormalGuarantee) -> Self {
        self.formal_guarantees.push(guarantee);
        self
    }

    /// Attach the performance / isomorphism contract.
    #[must_use]
    pub fn with_performance(mut self, performance: PerformanceContract) -> Self {
        self.performance = performance;
        self
    }

    /// Attach the composition-risk status.
    #[must_use]
    pub fn with_composition_risk(mut self, risk: CompositionRisk) -> Self {
        self.composition_risk = risk;
        self
    }

    /// Attach the failure-mode spec.
    #[must_use]
    pub fn with_failure_mode(mut self, failure_mode: FailureMode) -> Self {
        self.failure_mode = failure_mode;
        self
    }

    /// Attach the graduation spec.
    #[must_use]
    pub fn with_graduation(mut self, graduation: GraduationSpec) -> Self {
        self.graduation = graduation;
        self
    }

    /// Attach the demo linkage.
    #[must_use]
    pub fn with_demo_linkage(mut self, linkage: DemoLinkage) -> Self {
        self.demo_linkage = linkage;
        self
    }

    /// Set the relevance score.
    #[must_use]
    pub fn with_relevance_score(mut self, score: f64) -> Self {
        self.relevance_score = score;
        self
    }
}

/// Compile a recommendation card into a contract skeleton.
///
/// Derivable identity/scoring fields are filled from the card; the remaining
/// governance evidence is attached by the caller with the `with_*` builders
/// before linting.
#[must_use]
pub fn compile_recommendation_contract(card: &RecommendationCard) -> RecommendationContract {
    RecommendationContract {
        card_id: card.card_id.clone(),
        title: card.title.clone(),
        priority_tier: card.tier,
        ev_score: card.ev_score,
        relevance_score: 0.0,
        adoption_wedge: card.adoption_wedge.clone(),
        source_canonical_entry_id: card.canonical_entry_id.clone(),
        change_hotspot_evidence: ChangeHotspotEvidence::default(),
        entry_header: EntryHeaderMetadata {
            status: EntryStatus::Proposed,
            tier: card.tier,
            reality_check: String::new(),
            archetype: String::new(),
            tags: Vec::new(),
            integration_targets: Vec::new(),
        },
        budgeted_mode: BudgetedModeSpec::default(),
        inference_model: InferenceModelSpec::default(),
        formal_guarantees: Vec::new(),
        performance: PerformanceContract {
            baseline_comparator: card.baseline_comparator.clone(),
            ..PerformanceContract::default()
        },
        composition_risk: CompositionRisk::default(),
        failure_mode: FailureMode::default(),
        graduation: GraduationSpec::default(),
        demo_linkage: DemoLinkage::default(),
    }
}

// ── Lint Codes & Findings ────────────────────────────────────────────────

/// Severity of a contract-lint finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractSeverity {
    /// Blocks the release gate.
    Block,
    /// Advisory only.
    Warn,
}

impl ContractSeverity {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Warn => "warn",
        }
    }
}

/// Machine-readable code for a contract-lint finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractLintCode {
    /// `card_id` is empty.
    MissingCardId,
    /// `title` is empty.
    MissingTitle,
    /// EV score is below the tier floor.
    EvBelowTierFloor,
    /// EV score is not finite.
    InvalidEvScore,
    /// Relevance score is outside `[0, 1]`.
    InvalidRelevanceScore,
    /// Adoption wedge is empty.
    MissingAdoptionWedge,
    /// No change evidence.
    MissingChangeEvidence,
    /// No hotspot evidence.
    MissingHotspotEvidence,
    /// No mapped graveyard/artifact-coding sections.
    MissingMappedSections,
    /// Reality-check note is empty.
    MissingRealityCheck,
    /// Archetype label is empty.
    MissingArchetype,
    /// No tags.
    MissingTags,
    /// No integration targets.
    MissingIntegrationTargets,
    /// Entry-header tier contradicts the contract tier.
    EntryHeaderTierContradiction,
    /// Budgeted mode lacks exhaustion behavior.
    MissingExhaustionBehavior,
    /// Budgeted mode lacks a conservative fallback trigger.
    MissingFallbackTrigger,
    /// Posterior model family is empty.
    MissingPosteriorModel,
    /// No Bayes-factor references.
    MissingBayesFactorRefs,
    /// Expected-loss model is empty.
    MissingExpectedLossModel,
    /// Calibration metadata is empty.
    MissingCalibrationMetadata,
    /// No formal guarantees attached.
    MissingFormalGuarantees,
    /// A guarantee has no assumptions.
    GuaranteeMissingAssumptions,
    /// A guarantee has no bound terms.
    GuaranteeMissingBounds,
    /// A required guarantee kind is absent.
    MissingRequiredGuaranteeKind,
    /// Isomorphism-proof plan is empty.
    MissingIsomorphismPlan,
    /// Latency targets are missing.
    MissingLatencyTargets,
    /// Latency targets are not monotone (`p50 <= p95 <= p99`).
    NonMonotonicLatencyTargets,
    /// Baseline comparator is empty.
    MissingBaselineComparator,
    /// A composition-risk test was not run.
    CompositionTestNotRun,
    /// The interference test failed.
    CompositionInterferenceFailed,
    /// Failure-mode countermeasure is empty.
    MissingCountermeasure,
    /// Legal/IP status is blocked.
    LegalStatusBlocked,
    /// Legal/IP status needs counsel.
    LegalNeedsCounsel,
    /// Reproduction artifact is missing.
    MissingReproArtifact,
    /// Provenance artifact is missing.
    MissingProvenanceArtifact,
    /// No graduation criteria.
    MissingGraduationCriteria,
    /// Graveyard-verify status is not verified.
    GraveyardNotVerified,
    /// Graveyard-verify status is failed.
    GraveyardVerifyFailed,
    /// Size estimate is missing.
    MissingSizeEstimate,
    /// Demo id is empty.
    MissingDemoId,
    /// Claim id is empty.
    MissingClaimId,
    /// Rollback policy is empty.
    MissingRollbackPolicy,
    /// No galaxy-brain references.
    MissingGalaxyBrainRefs,
    /// Two contracts share a card id.
    DuplicateContract,
}

impl ContractLintCode {
    /// Stable lowercase tag.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingCardId => "missing_card_id",
            Self::MissingTitle => "missing_title",
            Self::EvBelowTierFloor => "ev_below_tier_floor",
            Self::InvalidEvScore => "invalid_ev_score",
            Self::InvalidRelevanceScore => "invalid_relevance_score",
            Self::MissingAdoptionWedge => "missing_adoption_wedge",
            Self::MissingChangeEvidence => "missing_change_evidence",
            Self::MissingHotspotEvidence => "missing_hotspot_evidence",
            Self::MissingMappedSections => "missing_mapped_sections",
            Self::MissingRealityCheck => "missing_reality_check",
            Self::MissingArchetype => "missing_archetype",
            Self::MissingTags => "missing_tags",
            Self::MissingIntegrationTargets => "missing_integration_targets",
            Self::EntryHeaderTierContradiction => "entry_header_tier_contradiction",
            Self::MissingExhaustionBehavior => "missing_exhaustion_behavior",
            Self::MissingFallbackTrigger => "missing_fallback_trigger",
            Self::MissingPosteriorModel => "missing_posterior_model",
            Self::MissingBayesFactorRefs => "missing_bayes_factor_refs",
            Self::MissingExpectedLossModel => "missing_expected_loss_model",
            Self::MissingCalibrationMetadata => "missing_calibration_metadata",
            Self::MissingFormalGuarantees => "missing_formal_guarantees",
            Self::GuaranteeMissingAssumptions => "guarantee_missing_assumptions",
            Self::GuaranteeMissingBounds => "guarantee_missing_bounds",
            Self::MissingRequiredGuaranteeKind => "missing_required_guarantee_kind",
            Self::MissingIsomorphismPlan => "missing_isomorphism_plan",
            Self::MissingLatencyTargets => "missing_latency_targets",
            Self::NonMonotonicLatencyTargets => "non_monotonic_latency_targets",
            Self::MissingBaselineComparator => "missing_baseline_comparator",
            Self::CompositionTestNotRun => "composition_test_not_run",
            Self::CompositionInterferenceFailed => "composition_interference_failed",
            Self::MissingCountermeasure => "missing_countermeasure",
            Self::LegalStatusBlocked => "legal_status_blocked",
            Self::LegalNeedsCounsel => "legal_needs_counsel",
            Self::MissingReproArtifact => "missing_repro_artifact",
            Self::MissingProvenanceArtifact => "missing_provenance_artifact",
            Self::MissingGraduationCriteria => "missing_graduation_criteria",
            Self::GraveyardNotVerified => "graveyard_not_verified",
            Self::GraveyardVerifyFailed => "graveyard_verify_failed",
            Self::MissingSizeEstimate => "missing_size_estimate",
            Self::MissingDemoId => "missing_demo_id",
            Self::MissingClaimId => "missing_claim_id",
            Self::MissingRollbackPolicy => "missing_rollback_policy",
            Self::MissingGalaxyBrainRefs => "missing_galaxy_brain_refs",
            Self::DuplicateContract => "duplicate_contract",
        }
    }

    /// Stable clause identifier (e.g. `RC-EVID-001`) for forensics.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn clause_id(self) -> &'static str {
        match self {
            Self::MissingCardId => "RC-ID-001",
            Self::MissingTitle => "RC-ID-002",
            Self::DuplicateContract => "RC-ID-003",
            Self::EvBelowTierFloor => "RC-SCORE-001",
            Self::InvalidEvScore => "RC-SCORE-002",
            Self::InvalidRelevanceScore => "RC-SCORE-003",
            Self::MissingAdoptionWedge => "RC-SCORE-004",
            Self::MissingChangeEvidence => "RC-EVID-001",
            Self::MissingHotspotEvidence => "RC-EVID-002",
            Self::MissingMappedSections => "RC-EVID-003",
            Self::MissingRealityCheck => "RC-HEADER-001",
            Self::MissingArchetype => "RC-HEADER-002",
            Self::MissingTags => "RC-HEADER-003",
            Self::MissingIntegrationTargets => "RC-HEADER-004",
            Self::EntryHeaderTierContradiction => "RC-HEADER-005",
            Self::MissingExhaustionBehavior => "RC-BUDGET-001",
            Self::MissingFallbackTrigger => "RC-BUDGET-002",
            Self::MissingPosteriorModel => "RC-INFER-001",
            Self::MissingBayesFactorRefs => "RC-INFER-002",
            Self::MissingExpectedLossModel => "RC-INFER-003",
            Self::MissingCalibrationMetadata => "RC-INFER-004",
            Self::MissingFormalGuarantees => "RC-GUAR-001",
            Self::GuaranteeMissingAssumptions => "RC-GUAR-002",
            Self::GuaranteeMissingBounds => "RC-GUAR-003",
            Self::MissingRequiredGuaranteeKind => "RC-GUAR-004",
            Self::MissingIsomorphismPlan => "RC-PERF-001",
            Self::MissingLatencyTargets => "RC-PERF-002",
            Self::NonMonotonicLatencyTargets => "RC-PERF-003",
            Self::MissingBaselineComparator => "RC-PERF-004",
            Self::CompositionTestNotRun => "RC-COMP-001",
            Self::CompositionInterferenceFailed => "RC-COMP-002",
            Self::MissingCountermeasure => "RC-FAIL-001",
            Self::LegalStatusBlocked => "RC-FAIL-002",
            Self::LegalNeedsCounsel => "RC-FAIL-003",
            Self::MissingReproArtifact => "RC-FAIL-004",
            Self::MissingProvenanceArtifact => "RC-FAIL-005",
            Self::MissingGraduationCriteria => "RC-GRAD-001",
            Self::GraveyardNotVerified => "RC-GRAD-002",
            Self::GraveyardVerifyFailed => "RC-GRAD-003",
            Self::MissingSizeEstimate => "RC-GRAD-004",
            Self::MissingDemoId => "RC-DEMO-001",
            Self::MissingClaimId => "RC-DEMO-002",
            Self::MissingRollbackPolicy => "RC-DEMO-003",
            Self::MissingGalaxyBrainRefs => "RC-DEMO-004",
        }
    }

    /// Dependency hint: which upstream track/artifact satisfies this clause.
    #[must_use]
    pub fn dependency_hint(self) -> &'static str {
        match self {
            Self::MissingChangeEvidence
            | Self::MissingHotspotEvidence
            | Self::MissingMappedSections => {
                "ingestion + hotspot analysis (bd-3bxhj.2 / profile_orchestrator)"
            }
            Self::MissingFormalGuarantees
            | Self::GuaranteeMissingAssumptions
            | Self::GuaranteeMissingBounds
            | Self::MissingRequiredGuaranteeKind => {
                "conformal/e-process/PAC-Bayes layer (bd-3bxhj.10.23)"
            }
            Self::MissingPosteriorModel
            | Self::MissingBayesFactorRefs
            | Self::MissingExpectedLossModel
            | Self::MissingCalibrationMetadata => {
                "Bayesian posterior + expected-loss core (bd-3bxhj.10.21 / .10.22)"
            }
            Self::MissingIsomorphismPlan
            | Self::MissingLatencyTargets
            | Self::NonMonotonicLatencyTargets => {
                "isomorphism-proof + benchmark pipeline (bd-3bxhj.8.17)"
            }
            Self::CompositionTestNotRun | Self::CompositionInterferenceFailed => {
                "controller-composition interference matrix (bd-3bxhj.10.8)"
            }
            Self::LegalStatusBlocked
            | Self::LegalNeedsCounsel
            | Self::MissingReproArtifact
            | Self::MissingProvenanceArtifact => {
                "paper-verification LEGAL/IP pack (bd-3bxhj.10.13)"
            }
            Self::MissingDemoId | Self::MissingClaimId | Self::MissingGalaxyBrainRefs => {
                "killer-demo contract + galaxy-brain cards (bd-3bxhj.10.34 / .10.26)"
            }
            _ => "recommendation-card authoring (bd-3bxhj.10.9 milestone policy)",
        }
    }
}

/// A structured contract-lint finding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractLintFinding {
    /// Machine-readable code.
    pub code: ContractLintCode,
    /// Severity.
    pub severity: ContractSeverity,
    /// Stable clause id.
    pub clause_id: String,
    /// Card this pertains to.
    pub card_id: String,
    /// Human-readable detail.
    pub detail: String,
    /// Actionable remediation clause.
    pub remediation: String,
    /// Dependency hint for satisfying this clause.
    pub dependency_hint: String,
}

impl ContractLintFinding {
    fn new(
        code: ContractLintCode,
        severity: ContractSeverity,
        card_id: impl Into<String>,
        detail: impl Into<String>,
        remediation: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity,
            clause_id: code.clause_id().to_string(),
            card_id: card_id.into(),
            detail: detail.into(),
            remediation: remediation.into(),
            dependency_hint: code.dependency_hint().to_string(),
        }
    }
}

// ── Configuration ────────────────────────────────────────────────────────

/// Configuration for the recommendation-contract linter.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContractLintConfig {
    /// EV floors per tier.
    pub tier_ev_thresholds: TierEvThresholds,
    /// Guarantee kinds every contract must include.
    pub required_guarantee_kinds: Vec<GuaranteeKind>,
    /// Require all composition-risk tests to have run.
    pub require_composition_tests: bool,
    /// Require demo linkage (`demo_id` + `claim_id`).
    pub require_demo_linkage: bool,
    /// Treat a non-verified graveyard status as blocking (else advisory).
    pub graveyard_unverified_blocks: bool,
    /// Require a reproduction artifact.
    pub require_repro_artifact: bool,
    /// Require a provenance artifact.
    pub require_provenance_artifact: bool,
}

impl Default for ContractLintConfig {
    fn default() -> Self {
        Self {
            tier_ev_thresholds: TierEvThresholds::default(),
            required_guarantee_kinds: vec![GuaranteeKind::Conformal, GuaranteeKind::EProcess],
            require_composition_tests: true,
            require_demo_linkage: true,
            graveyard_unverified_blocks: false,
            require_repro_artifact: true,
            require_provenance_artifact: true,
        }
    }
}

impl ContractLintConfig {
    /// Override the required guarantee kinds.
    #[must_use]
    pub fn with_required_guarantee_kinds(
        mut self,
        kinds: impl IntoIterator<Item = GuaranteeKind>,
    ) -> Self {
        let mut kinds: Vec<GuaranteeKind> = kinds.into_iter().collect();
        kinds.sort();
        kinds.dedup();
        self.required_guarantee_kinds = kinds;
        self
    }

    /// Toggle whether a non-verified graveyard status blocks the gate.
    #[must_use]
    pub fn with_graveyard_unverified_blocks(mut self, blocks: bool) -> Self {
        self.graveyard_unverified_blocks = blocks;
        self
    }

    /// Toggle composition-test enforcement.
    #[must_use]
    pub fn with_require_composition_tests(mut self, require: bool) -> Self {
        self.require_composition_tests = require;
        self
    }
}

// ── Report ───────────────────────────────────────────────────────────────

/// Aggregate counts for a contract-lint report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractLintSummary {
    /// Total contracts evaluated.
    pub total_contracts: usize,
    /// Contracts with at least one blocking finding.
    pub blocked_contracts: usize,
    /// Total blocking findings.
    pub blocking_finding_count: usize,
    /// Total advisory findings.
    pub advisory_finding_count: usize,
}

/// Deterministic JSON-stats artifact (content + checksum).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractLintJsonStatsArtifact {
    /// Suggested relative output path.
    pub path: String,
    /// SHA-256 of `content`.
    pub sha256: String,
    /// Serialized JSON content.
    pub content: String,
}

/// Full recommendation-contract lint report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContractLintReport {
    /// Schema version constant.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Configuration used.
    pub config: ContractLintConfig,
    /// Card ids linted (input order preserved).
    pub contract_ids: Vec<String>,
    /// Findings (in evaluation order).
    pub findings: Vec<ContractLintFinding>,
    /// Aggregate summary.
    pub summary: ContractLintSummary,
    /// Whether the lint gate passes (no blocking findings).
    pub lint_gate_passes: bool,
    /// Card ids blocked by the gate.
    pub blocked_card_ids: Vec<String>,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: ContractLintJsonStatsArtifact,
    /// Replay command for the report.
    pub replay_command: String,
}

impl ContractLintReport {
    /// Every blocking remediation clause, in report order.
    #[must_use]
    pub fn blocking_remediations(&self) -> Vec<String> {
        self.findings
            .iter()
            .filter(|f| f.severity == ContractSeverity::Block)
            .map(|f| f.remediation.clone())
            .collect()
    }

    /// Findings for a specific card.
    #[must_use]
    pub fn findings_for(&self, card_id: &str) -> Vec<&ContractLintFinding> {
        self.findings
            .iter()
            .filter(|f| f.card_id == card_id)
            .collect()
    }
}

// ── Linter ───────────────────────────────────────────────────────────────

/// Lints recommendation contracts against the governance policy.
#[derive(Debug, Clone, Default)]
pub struct RecommendationContractLinter {
    config: ContractLintConfig,
}

impl RecommendationContractLinter {
    /// Build a linter from configuration.
    #[must_use]
    pub fn new(config: ContractLintConfig) -> Self {
        Self { config }
    }

    /// Borrow the configuration.
    #[must_use]
    pub fn config(&self) -> &ContractLintConfig {
        &self.config
    }

    /// Lint a single contract, returning its findings.
    #[must_use]
    pub fn lint(&self, contract: &RecommendationContract) -> Vec<ContractLintFinding> {
        let mut findings = Vec::new();
        self.check_identity_and_scoring(contract, &mut findings);
        self.check_evidence(contract, &mut findings);
        self.check_entry_header(contract, &mut findings);
        self.check_budgeted_mode(contract, &mut findings);
        self.check_inference_model(contract, &mut findings);
        self.check_formal_guarantees(contract, &mut findings);
        self.check_performance(contract, &mut findings);
        self.check_composition_risk(contract, &mut findings);
        self.check_failure_mode(contract, &mut findings);
        self.check_graduation(contract, &mut findings);
        self.check_demo_linkage(contract, &mut findings);
        findings
    }

    /// Evaluate a batch of contracts into a deterministic report.
    #[must_use]
    pub fn evaluate(&self, contracts: Vec<RecommendationContract>) -> ContractLintReport {
        let mut findings = Vec::new();
        self.check_duplicates(&contracts, &mut findings);
        for contract in &contracts {
            findings.extend(self.lint(contract));
        }

        let blocking_finding_count = findings
            .iter()
            .filter(|f| f.severity == ContractSeverity::Block)
            .count();
        let advisory_finding_count = findings.len() - blocking_finding_count;

        let mut blocked_card_ids: Vec<String> = findings
            .iter()
            .filter(|f| f.severity == ContractSeverity::Block)
            .map(|f| f.card_id.clone())
            .collect();
        blocked_card_ids.sort();
        blocked_card_ids.dedup();

        let blocked_contracts = contracts
            .iter()
            .filter(|c| blocked_card_ids.contains(&c.card_id))
            .count();

        let summary = ContractLintSummary {
            total_contracts: contracts.len(),
            blocked_contracts,
            blocking_finding_count,
            advisory_finding_count,
        };
        let lint_gate_passes = blocking_finding_count == 0;

        let contract_ids: Vec<String> = contracts.iter().map(|c| c.card_id.clone()).collect();
        let report_id = report_id_for(&self.config, &contracts);
        let exported_json_stats =
            export_json_stats(&report_id, &self.config, &contract_ids, &findings, &summary);
        let replay_command = format!(
            "doctor_frankentui recommendation-contract-lint --report-id {report_id} \
             --required-guarantees {}",
            self.config.required_guarantee_kinds.len()
        );

        ContractLintReport {
            schema_version: RECOMMENDATION_CONTRACT_SCHEMA_VERSION.to_string(),
            report_id,
            config: self.config.clone(),
            contract_ids,
            findings,
            summary,
            lint_gate_passes,
            blocked_card_ids,
            exported_json_stats,
            replay_command,
        }
    }

    fn check_duplicates(
        &self,
        contracts: &[RecommendationContract],
        findings: &mut Vec<ContractLintFinding>,
    ) {
        let mut seen: Vec<&str> = Vec::new();
        let mut reported: Vec<&str> = Vec::new();
        for contract in contracts {
            let id = contract.card_id.as_str();
            if id.is_empty() {
                continue;
            }
            if seen.contains(&id) && !reported.contains(&id) {
                findings.push(ContractLintFinding::new(
                    ContractLintCode::DuplicateContract,
                    ContractSeverity::Block,
                    contract.card_id.clone(),
                    format!("card id `{id}` appears in more than one contract"),
                    "give each contract a unique card id",
                ));
                reported.push(id);
            }
            seen.push(id);
        }
    }

    fn check_identity_and_scoring(
        &self,
        contract: &RecommendationContract,
        findings: &mut Vec<ContractLintFinding>,
    ) {
        let id = contract.card_id.clone();
        if contract.card_id.trim().is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingCardId,
                ContractSeverity::Block,
                "",
                "card_id is empty",
                "assign a stable card id",
            ));
        }
        if contract.title.trim().is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingTitle,
                ContractSeverity::Block,
                &id,
                "title is empty",
                "give the recommendation a human-readable title",
            ));
        }
        if !contract.ev_score.is_finite() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::InvalidEvScore,
                ContractSeverity::Block,
                &id,
                "ev_score is not finite",
                "set a finite expected-value score",
            ));
        } else {
            let floor = self.config.tier_ev_thresholds.floor(contract.priority_tier);
            if contract.ev_score < floor - EV_EPS {
                findings.push(ContractLintFinding::new(
                    ContractLintCode::EvBelowTierFloor,
                    ContractSeverity::Block,
                    &id,
                    format!(
                        "ev_score {:.3} is below the {} floor {:.3}",
                        contract.ev_score,
                        contract.priority_tier.as_str(),
                        floor
                    ),
                    "raise the EV evidence or lower the declared tier",
                ));
            }
        }
        if !(contract.relevance_score.is_finite()
            && (0.0..=1.0).contains(&contract.relevance_score))
        {
            findings.push(ContractLintFinding::new(
                ContractLintCode::InvalidRelevanceScore,
                ContractSeverity::Block,
                &id,
                format!(
                    "relevance_score {} is outside [0, 1]",
                    contract.relevance_score
                ),
                "set relevance_score within [0, 1]",
            ));
        }
        if contract.adoption_wedge.trim().is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingAdoptionWedge,
                ContractSeverity::Block,
                &id,
                "adoption_wedge is empty",
                "describe the production adoption wedge",
            ));
        }
    }

    fn check_evidence(
        &self,
        contract: &RecommendationContract,
        findings: &mut Vec<ContractLintFinding>,
    ) {
        let id = &contract.card_id;
        let evidence = &contract.change_hotspot_evidence;
        if evidence.change_refs.is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingChangeEvidence,
                ContractSeverity::Block,
                id,
                "no change evidence references",
                "link the change(s) that motivate this uplift",
            ));
        }
        if evidence.hotspot_refs.is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingHotspotEvidence,
                ContractSeverity::Block,
                id,
                "no hotspot evidence references",
                "link the profiled hotspot(s)",
            ));
        }
        if evidence.mapped_sections.is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingMappedSections,
                ContractSeverity::Block,
                id,
                "no mapped graveyard/artifact-coding sections",
                "map this uplift to its graveyard/artifact-coding section(s)",
            ));
        }
    }

    fn check_entry_header(
        &self,
        contract: &RecommendationContract,
        findings: &mut Vec<ContractLintFinding>,
    ) {
        let id = &contract.card_id;
        let header = &contract.entry_header;
        if header.reality_check.trim().is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingRealityCheck,
                ContractSeverity::Block,
                id,
                "entry-header reality_check is empty",
                "write an honest reality-check note",
            ));
        }
        if header.archetype.trim().is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingArchetype,
                ContractSeverity::Block,
                id,
                "entry-header archetype is empty",
                "set the archetype label",
            ));
        }
        if header.tags.is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingTags,
                ContractSeverity::Warn,
                id,
                "entry-header has no tags",
                "add classification tags",
            ));
        }
        if header.integration_targets.is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingIntegrationTargets,
                ContractSeverity::Block,
                id,
                "entry-header has no integration targets",
                "list the modules/subsystems this integrates with",
            ));
        }
        if header.tier != contract.priority_tier {
            findings.push(ContractLintFinding::new(
                ContractLintCode::EntryHeaderTierContradiction,
                ContractSeverity::Block,
                id,
                format!(
                    "entry-header tier `{}` contradicts contract tier `{}`",
                    header.tier.as_str(),
                    contract.priority_tier.as_str()
                ),
                "align the entry-header tier with the contract tier",
            ));
        }
    }

    fn check_budgeted_mode(
        &self,
        contract: &RecommendationContract,
        findings: &mut Vec<ContractLintFinding>,
    ) {
        if !contract.budgeted_mode.budgeted {
            return;
        }
        let id = &contract.card_id;
        if contract.budgeted_mode.exhaustion_behavior.trim().is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingExhaustionBehavior,
                ContractSeverity::Block,
                id,
                "budgeted mode without exhaustion behavior",
                "describe what happens when the budget is exhausted",
            ));
        }
        if contract
            .budgeted_mode
            .conservative_fallback_trigger
            .trim()
            .is_empty()
        {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingFallbackTrigger,
                ContractSeverity::Block,
                id,
                "budgeted mode without a conservative fallback trigger",
                "define the trigger that forces the conservative fallback",
            ));
        }
    }

    fn check_inference_model(
        &self,
        contract: &RecommendationContract,
        findings: &mut Vec<ContractLintFinding>,
    ) {
        let id = &contract.card_id;
        let model = &contract.inference_model;
        if model.posterior_model_family.trim().is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingPosteriorModel,
                ContractSeverity::Block,
                id,
                "posterior model family is empty",
                "declare the posterior model family",
            ));
        }
        if model.bayes_factor_refs.is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingBayesFactorRefs,
                ContractSeverity::Block,
                id,
                "no Bayes-factor references",
                "reference the Bayes-factor evidence ledger entries",
            ));
        }
        if model.expected_loss_model.trim().is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingExpectedLossModel,
                ContractSeverity::Block,
                id,
                "expected-loss model is empty",
                "describe the expected-loss / action policy model",
            ));
        }
        if model.calibration_metadata.trim().is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingCalibrationMetadata,
                ContractSeverity::Block,
                id,
                "calibration metadata is empty",
                "record coverage targets and the calibration set",
            ));
        }
    }

    fn check_formal_guarantees(
        &self,
        contract: &RecommendationContract,
        findings: &mut Vec<ContractLintFinding>,
    ) {
        let id = &contract.card_id;
        if contract.formal_guarantees.is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingFormalGuarantees,
                ContractSeverity::Block,
                id,
                "no formal guarantees attached",
                "attach at least one formal guarantee with assumptions and bounds",
            ));
        }
        for guarantee in &contract.formal_guarantees {
            if guarantee.assumptions.is_empty() {
                findings.push(ContractLintFinding::new(
                    ContractLintCode::GuaranteeMissingAssumptions,
                    ContractSeverity::Block,
                    id,
                    format!("{} guarantee has no assumptions", guarantee.kind.as_str()),
                    "state the assumptions the bound relies on",
                ));
            }
            if guarantee.bound_terms.is_empty() {
                findings.push(ContractLintFinding::new(
                    ContractLintCode::GuaranteeMissingBounds,
                    ContractSeverity::Block,
                    id,
                    format!("{} guarantee has no bound terms", guarantee.kind.as_str()),
                    "state the bound terms/parameters",
                ));
            }
        }
        let present = contract.guarantee_kinds();
        for kind in &self.config.required_guarantee_kinds {
            if !present.contains(kind) {
                findings.push(ContractLintFinding::new(
                    ContractLintCode::MissingRequiredGuaranteeKind,
                    ContractSeverity::Block,
                    id,
                    format!("required guarantee kind `{}` is absent", kind.as_str()),
                    format!("add a `{}` formal guarantee", kind.as_str()),
                ));
            }
        }
    }

    fn check_performance(
        &self,
        contract: &RecommendationContract,
        findings: &mut Vec<ContractLintFinding>,
    ) {
        let id = &contract.card_id;
        let perf = &contract.performance;
        if perf.isomorphism_proof_plan.trim().is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingIsomorphismPlan,
                ContractSeverity::Block,
                id,
                "isomorphism-proof plan is empty",
                "describe the isomorphism-proof plan vs the baseline",
            ));
        }
        match (perf.p50_target_ms, perf.p95_target_ms, perf.p99_target_ms) {
            (Some(p50), Some(p95), Some(p99))
                if p50.is_finite() && p95.is_finite() && p99.is_finite() =>
            {
                if !(p50 <= p95 + EV_EPS && p95 <= p99 + EV_EPS) {
                    findings.push(ContractLintFinding::new(
                        ContractLintCode::NonMonotonicLatencyTargets,
                        ContractSeverity::Block,
                        id,
                        format!(
                            "latency targets are not monotone: p50={p50}, p95={p95}, p99={p99}"
                        ),
                        "ensure p50 <= p95 <= p99",
                    ));
                }
            }
            _ => findings.push(ContractLintFinding::new(
                ContractLintCode::MissingLatencyTargets,
                ContractSeverity::Block,
                id,
                "p50/p95/p99 latency targets are incomplete",
                "set finite p50, p95, and p99 latency targets",
            )),
        }
        if perf.baseline_comparator.trim().is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingBaselineComparator,
                ContractSeverity::Block,
                id,
                "baseline comparator is empty",
                "declare the baseline comparator",
            ));
        }
    }

    fn check_composition_risk(
        &self,
        contract: &RecommendationContract,
        findings: &mut Vec<ContractLintFinding>,
    ) {
        let id = &contract.card_id;
        let risk = &contract.composition_risk;
        if risk.interference_status == TestStatus::Failed {
            findings.push(ContractLintFinding::new(
                ContractLintCode::CompositionInterferenceFailed,
                ContractSeverity::Block,
                id,
                "controller-interference test failed",
                "resolve the interference (re-time or disable a redundant controller)",
            ));
        }
        if self.config.require_composition_tests {
            let unrun = [
                ("dependency", risk.dependency_status),
                ("conflict", risk.conflict_status),
                ("synergy", risk.synergy_status),
                ("interference", risk.interference_status),
            ];
            for (name, status) in unrun {
                if status == TestStatus::NotRun {
                    findings.push(ContractLintFinding::new(
                        ContractLintCode::CompositionTestNotRun,
                        ContractSeverity::Block,
                        id,
                        format!("composition {name} test was not run"),
                        format!("run the {name} composition test or waive it with rationale"),
                    ));
                }
            }
        }
    }

    fn check_failure_mode(
        &self,
        contract: &RecommendationContract,
        findings: &mut Vec<ContractLintFinding>,
    ) {
        let id = &contract.card_id;
        let failure = &contract.failure_mode;
        if failure.countermeasure.trim().is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingCountermeasure,
                ContractSeverity::Block,
                id,
                "failure-mode countermeasure is empty",
                "describe the countermeasure for the declared risk class",
            ));
        }
        match failure.legal_status {
            IpArtifactStatus::Blocked => findings.push(ContractLintFinding::new(
                ContractLintCode::LegalStatusBlocked,
                ContractSeverity::Block,
                id,
                "legal/IP status is blocked",
                "resolve the IP blocker or document a clean design-around",
            )),
            IpArtifactStatus::NeedsCounsel => findings.push(ContractLintFinding::new(
                ContractLintCode::LegalNeedsCounsel,
                ContractSeverity::Warn,
                id,
                "legal/IP status needs counsel",
                "obtain legal sign-off before promotion",
            )),
            IpArtifactStatus::Clear | IpArtifactStatus::Expired | IpArtifactStatus::Unknown => {}
        }
        if self.config.require_repro_artifact && failure.repro_artifact.is_none() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingReproArtifact,
                ContractSeverity::Block,
                id,
                "no reproduction artifact",
                "attach a reproduction artifact pointer",
            ));
        }
        if self.config.require_provenance_artifact && failure.provenance_artifact.is_none() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingProvenanceArtifact,
                ContractSeverity::Block,
                id,
                "no provenance artifact",
                "attach a provenance artifact pointer",
            ));
        }
    }

    fn check_graduation(
        &self,
        contract: &RecommendationContract,
        findings: &mut Vec<ContractLintFinding>,
    ) {
        let id = &contract.card_id;
        let grad = &contract.graduation;
        if grad.graduation_criteria.is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingGraduationCriteria,
                ContractSeverity::Block,
                id,
                "no graduation criteria",
                "list the criteria required to graduate",
            ));
        }
        match grad.graveyard_verify_status {
            GraveyardVerifyStatus::Failed => findings.push(ContractLintFinding::new(
                ContractLintCode::GraveyardVerifyFailed,
                ContractSeverity::Block,
                id,
                "graveyard-verify status is failed",
                "fix the graveyard-verify failures before promotion",
            )),
            GraveyardVerifyStatus::NotVerified => {
                let severity = if self.config.graveyard_unverified_blocks {
                    ContractSeverity::Block
                } else {
                    ContractSeverity::Warn
                };
                findings.push(ContractLintFinding::new(
                    ContractLintCode::GraveyardNotVerified,
                    severity,
                    id,
                    "graveyard-verify status is not verified",
                    "run graveyardctl verify for this entry",
                ));
            }
            GraveyardVerifyStatus::Verified => {}
        }
        if grad.size_estimate_loc.is_none() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingSizeEstimate,
                ContractSeverity::Warn,
                id,
                "no LOC size estimate",
                "add an estimated size in lines of code",
            ));
        }
    }

    fn check_demo_linkage(
        &self,
        contract: &RecommendationContract,
        findings: &mut Vec<ContractLintFinding>,
    ) {
        if !self.config.require_demo_linkage {
            return;
        }
        let id = &contract.card_id;
        let demo = &contract.demo_linkage;
        if demo.demo_id.trim().is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingDemoId,
                ContractSeverity::Block,
                id,
                "demo_id is empty",
                "link a killer-demo (demo_id)",
            ));
        }
        if demo.claim_id.trim().is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingClaimId,
                ContractSeverity::Block,
                id,
                "claim_id is empty",
                "link the demonstrated claim (claim_id)",
            ));
        }
        if demo.rollback_policy.trim().is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingRollbackPolicy,
                ContractSeverity::Block,
                id,
                "rollback_policy is empty",
                "declare the rollback policy",
            ));
        }
        if demo.galaxy_brain_refs.is_empty() {
            findings.push(ContractLintFinding::new(
                ContractLintCode::MissingGalaxyBrainRefs,
                ContractSeverity::Warn,
                id,
                "no galaxy-brain references",
                "reference the galaxy-brain transparency card(s)",
            ));
        }
    }
}

// ── Example / Fixtures ───────────────────────────────────────────────────

/// A fully-populated contract that passes the default linter. Useful as a
/// green-path fixture and as a template for authoring real contracts.
#[must_use]
pub fn example_complete_contract() -> RecommendationContract {
    let card = RecommendationCard::new(
        "rc-shadow-run",
        "Shadow-run dual execution wedge",
        PriorityTier::S,
        9.0,
        crate::milestone_policy::MathFamily::FormalAnalysis,
    )
    .with_baseline_comparator("single-path replay")
    .with_adoption_wedge("default-on shadow comparison behind a budget gate");

    compile_recommendation_contract(&card)
        .with_relevance_score(0.92)
        .with_change_hotspot_evidence(ChangeHotspotEvidence {
            change_refs: vec!["bd-3bxhj.10.7".to_string()],
            hotspot_refs: vec!["replay-divergence".to_string()],
            mapped_sections: vec!["graveyard §0.30".to_string()],
        })
        .with_entry_header(EntryHeaderMetadata {
            status: EntryStatus::Integrated,
            tier: PriorityTier::S,
            reality_check: "Implemented and budgeted; default-on behind a gate.".to_string(),
            archetype: "dual-execution-wedge".to_string(),
            tags: vec!["shadow-run".to_string(), "determinism".to_string()],
            integration_targets: vec!["doctor_frankentui::shadow_run".to_string()],
        })
        .with_budgeted_mode(BudgetedModeSpec {
            budgeted: true,
            exhaustion_behavior: "skip the shadow pass and record a deferred-comparison note"
                .to_string(),
            conservative_fallback_trigger: "budget exceeded or checksum mismatch".to_string(),
        })
        .with_inference_model(InferenceModelSpec {
            posterior_model_family: "beta_bernoulli".to_string(),
            bayes_factor_refs: vec!["lbf:shadow-divergence".to_string()],
            expected_loss_model: "0/1 mismatch loss with asymmetric regression weight".to_string(),
            calibration_metadata: "coverage target 0.99 over the replay corpus".to_string(),
        })
        .with_formal_guarantee(FormalGuarantee::new(
            GuaranteeKind::Conformal,
            ["exchangeable replay residuals".to_string()],
            ["alpha=0.01".to_string(), "split-conformal".to_string()],
        ))
        .with_formal_guarantee(FormalGuarantee::new(
            GuaranteeKind::EProcess,
            ["bounded per-frame divergence".to_string()],
            ["wealth threshold 1/alpha".to_string()],
        ))
        .with_performance(PerformanceContract {
            isomorphism_proof_plan: "bit-for-bit frame-checksum equality across both lanes"
                .to_string(),
            p50_target_ms: Some(0.4),
            p95_target_ms: Some(1.2),
            p99_target_ms: Some(2.5),
            baseline_comparator: "single-path replay".to_string(),
        })
        .with_composition_risk(CompositionRisk {
            dependency_status: TestStatus::Passed,
            conflict_status: TestStatus::Passed,
            synergy_status: TestStatus::Passed,
            interference_status: TestStatus::Passed,
        })
        .with_failure_mode(FailureMode {
            risk_class: RiskClass::Medium,
            countermeasure: "fall back to single-path run and flag a deferred comparison"
                .to_string(),
            repro_artifact: Some("shadow_run/repro.lock".to_string()),
            legal_status: IpArtifactStatus::Clear,
            provenance_artifact: Some("shadow_run/provenance.json".to_string()),
        })
        .with_graduation(GraduationSpec {
            graduation_criteria: vec!["3 green shadow scenarios".to_string()],
            effort_estimate: EffortSize::Large,
            size_estimate_loc: Some(900),
            graveyard_verify_status: GraveyardVerifyStatus::Verified,
        })
        .with_demo_linkage(DemoLinkage {
            demo_id: "demo-shadow-run".to_string(),
            claim_id: "claim-shadow-determinism".to_string(),
            rollback_policy: "disable the shadow lane via FTUI_ROLLOUT_POLICY=off".to_string(),
            galaxy_brain_refs: vec!["gb:shadow-run-decision".to_string()],
        })
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn export_json_stats(
    report_id: &str,
    config: &ContractLintConfig,
    contract_ids: &[String],
    findings: &[ContractLintFinding],
    summary: &ContractLintSummary,
) -> ContractLintJsonStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        config: &'a ContractLintConfig,
        summary: &'a ContractLintSummary,
        contract_ids: &'a [String],
        findings: &'a [ContractLintFinding],
    }

    let payload = Export {
        schema_version: RECOMMENDATION_CONTRACT_SCHEMA_VERSION,
        report_id,
        config,
        summary,
        contract_ids,
        findings,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    ContractLintJsonStatsArtifact {
        path: format!("{report_id}/recommendation_contract_lint_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

fn report_id_for(config: &ContractLintConfig, contracts: &[RecommendationContract]) -> String {
    #[derive(Serialize)]
    struct IdInput<'a> {
        config: &'a ContractLintConfig,
        contracts: &'a [RecommendationContract],
    }
    let hash = stable_hash(&IdInput { config, contracts });
    format!("recommendation-contract-{}", short_hash(&hash))
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

    fn lint_one(contract: &RecommendationContract) -> Vec<ContractLintFinding> {
        RecommendationContractLinter::default().lint(contract)
    }

    fn has_code(findings: &[ContractLintFinding], code: ContractLintCode) -> bool {
        findings.iter().any(|f| f.code == code)
    }

    #[test]
    fn example_contract_passes_default_linter() {
        let report =
            RecommendationContractLinter::default().evaluate(vec![example_complete_contract()]);
        assert!(
            report.lint_gate_passes,
            "example should pass; findings: {:?}",
            report.findings
        );
        assert!(report.blocked_card_ids.is_empty());
        assert_eq!(report.summary.total_contracts, 1);
        assert_eq!(report.summary.blocking_finding_count, 0);
    }

    #[test]
    fn compile_from_card_carries_identity() {
        let card = RecommendationCard::new(
            "rc-1",
            "Test card",
            PriorityTier::A,
            6.0,
            crate::milestone_policy::MathFamily::Probabilistic,
        )
        .with_baseline_comparator("baseline-x")
        .with_adoption_wedge("wedge-y")
        .with_canonical_entry_id("entry-z");
        let contract = compile_recommendation_contract(&card);
        assert_eq!(contract.card_id, "rc-1");
        assert_eq!(contract.title, "Test card");
        assert_eq!(contract.priority_tier, PriorityTier::A);
        assert_eq!(contract.ev_score, 6.0);
        assert_eq!(contract.adoption_wedge, "wedge-y");
        assert_eq!(contract.source_canonical_entry_id, "entry-z");
        assert_eq!(contract.performance.baseline_comparator, "baseline-x");
        assert_eq!(contract.entry_header.tier, PriorityTier::A);
    }

    #[test]
    fn skeleton_contract_fails_many_clauses() {
        let card = RecommendationCard::new(
            "rc-bare",
            "Bare",
            PriorityTier::S,
            9.0,
            crate::milestone_policy::MathFamily::Symbolic,
        );
        let contract = compile_recommendation_contract(&card);
        let findings = lint_one(&contract);
        // A bare skeleton should trip a broad set of required-field clauses.
        assert!(findings.len() > 10);
        assert!(has_code(&findings, ContractLintCode::MissingChangeEvidence));
        assert!(has_code(
            &findings,
            ContractLintCode::MissingFormalGuarantees
        ));
        assert!(has_code(&findings, ContractLintCode::MissingDemoId));
        assert!(has_code(
            &findings,
            ContractLintCode::MissingIsomorphismPlan
        ));
    }

    #[test]
    fn ev_below_tier_floor_is_blocked() {
        let mut contract = example_complete_contract();
        contract.ev_score = 1.0; // below the S floor (8.0)
        let findings = lint_one(&contract);
        assert!(has_code(&findings, ContractLintCode::EvBelowTierFloor));
    }

    #[test]
    fn non_finite_ev_is_blocked() {
        let mut contract = example_complete_contract();
        contract.ev_score = f64::NAN;
        let findings = lint_one(&contract);
        assert!(has_code(&findings, ContractLintCode::InvalidEvScore));
    }

    #[test]
    fn relevance_score_out_of_range_is_blocked() {
        let mut contract = example_complete_contract();
        contract.relevance_score = 1.5;
        let findings = lint_one(&contract);
        assert!(has_code(&findings, ContractLintCode::InvalidRelevanceScore));
    }

    #[test]
    fn entry_header_tier_contradiction_is_blocked() {
        let mut contract = example_complete_contract();
        contract.entry_header.tier = PriorityTier::B;
        let findings = lint_one(&contract);
        assert!(has_code(
            &findings,
            ContractLintCode::EntryHeaderTierContradiction
        ));
    }

    #[test]
    fn non_monotonic_latency_is_blocked() {
        let mut contract = example_complete_contract();
        contract.performance.p95_target_ms = Some(5.0);
        contract.performance.p99_target_ms = Some(1.0); // p99 < p95
        let findings = lint_one(&contract);
        assert!(has_code(
            &findings,
            ContractLintCode::NonMonotonicLatencyTargets
        ));
    }

    #[test]
    fn missing_latency_targets_is_blocked() {
        let mut contract = example_complete_contract();
        contract.performance.p99_target_ms = None;
        let findings = lint_one(&contract);
        assert!(has_code(&findings, ContractLintCode::MissingLatencyTargets));
    }

    #[test]
    fn missing_required_guarantee_kind_is_blocked() {
        let mut contract = example_complete_contract();
        // Drop the e-process guarantee, leaving only conformal.
        contract
            .formal_guarantees
            .retain(|g| g.kind != GuaranteeKind::EProcess);
        let findings = lint_one(&contract);
        assert!(has_code(
            &findings,
            ContractLintCode::MissingRequiredGuaranteeKind
        ));
    }

    #[test]
    fn guarantee_without_assumptions_is_blocked() {
        let mut contract = example_complete_contract();
        contract.formal_guarantees[0].assumptions.clear();
        let findings = lint_one(&contract);
        assert!(has_code(
            &findings,
            ContractLintCode::GuaranteeMissingAssumptions
        ));
    }

    #[test]
    fn composition_not_run_is_blocked_when_required() {
        let mut contract = example_complete_contract();
        contract.composition_risk.synergy_status = TestStatus::NotRun;
        let findings = lint_one(&contract);
        assert!(has_code(&findings, ContractLintCode::CompositionTestNotRun));
    }

    #[test]
    fn composition_can_be_relaxed() {
        let mut contract = example_complete_contract();
        contract.composition_risk.synergy_status = TestStatus::NotRun;
        let config = ContractLintConfig::default().with_require_composition_tests(false);
        let findings = RecommendationContractLinter::new(config).lint(&contract);
        assert!(!has_code(
            &findings,
            ContractLintCode::CompositionTestNotRun
        ));
    }

    #[test]
    fn interference_failure_is_blocked_even_when_relaxed() {
        let mut contract = example_complete_contract();
        contract.composition_risk.interference_status = TestStatus::Failed;
        let config = ContractLintConfig::default().with_require_composition_tests(false);
        let findings = RecommendationContractLinter::new(config).lint(&contract);
        assert!(has_code(
            &findings,
            ContractLintCode::CompositionInterferenceFailed
        ));
    }

    #[test]
    fn blocked_legal_status_is_blocked() {
        let mut contract = example_complete_contract();
        contract.failure_mode.legal_status = IpArtifactStatus::Blocked;
        let findings = lint_one(&contract);
        assert!(has_code(&findings, ContractLintCode::LegalStatusBlocked));
    }

    #[test]
    fn needs_counsel_is_advisory_only() {
        let mut contract = example_complete_contract();
        contract.failure_mode.legal_status = IpArtifactStatus::NeedsCounsel;
        let report = RecommendationContractLinter::default().evaluate(vec![contract]);
        // Advisory: appears as a finding but does not block.
        assert!(report.lint_gate_passes);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == ContractLintCode::LegalNeedsCounsel
                    && f.severity == ContractSeverity::Warn)
        );
    }

    #[test]
    fn missing_repro_and_provenance_blocked() {
        let mut contract = example_complete_contract();
        contract.failure_mode.repro_artifact = None;
        contract.failure_mode.provenance_artifact = None;
        let findings = lint_one(&contract);
        assert!(has_code(&findings, ContractLintCode::MissingReproArtifact));
        assert!(has_code(
            &findings,
            ContractLintCode::MissingProvenanceArtifact
        ));
    }

    #[test]
    fn graveyard_unverified_is_warn_by_default_block_when_configured() {
        let mut contract = example_complete_contract();
        contract.graduation.graveyard_verify_status = GraveyardVerifyStatus::NotVerified;
        // Default: advisory.
        let default_report =
            RecommendationContractLinter::default().evaluate(vec![contract.clone()]);
        assert!(default_report.lint_gate_passes);
        // Strict: blocking.
        let strict = ContractLintConfig::default().with_graveyard_unverified_blocks(true);
        let strict_report = RecommendationContractLinter::new(strict).evaluate(vec![contract]);
        assert!(!strict_report.lint_gate_passes);
    }

    #[test]
    fn duplicate_card_id_is_blocked() {
        let report = RecommendationContractLinter::default().evaluate(vec![
            example_complete_contract(),
            example_complete_contract(),
        ]);
        assert!(has_code(
            &report.findings,
            ContractLintCode::DuplicateContract
        ));
        assert!(!report.lint_gate_passes);
    }

    #[test]
    fn findings_carry_clause_ids_and_dependency_hints() {
        let card = RecommendationCard::new(
            "rc-x",
            "x",
            PriorityTier::S,
            9.0,
            crate::milestone_policy::MathFamily::Symbolic,
        );
        let findings = lint_one(&compile_recommendation_contract(&card));
        assert!(!findings.is_empty());
        for finding in &findings {
            assert!(!finding.clause_id.is_empty());
            assert!(finding.clause_id.starts_with("RC-"));
            assert!(!finding.dependency_hint.is_empty());
            assert!(!finding.remediation.is_empty());
            assert_eq!(finding.clause_id, finding.code.clause_id());
        }
    }

    #[test]
    fn all_lint_codes_have_distinct_clause_ids() {
        let codes = [
            ContractLintCode::MissingCardId,
            ContractLintCode::MissingTitle,
            ContractLintCode::EvBelowTierFloor,
            ContractLintCode::InvalidEvScore,
            ContractLintCode::InvalidRelevanceScore,
            ContractLintCode::MissingAdoptionWedge,
            ContractLintCode::MissingChangeEvidence,
            ContractLintCode::MissingHotspotEvidence,
            ContractLintCode::MissingMappedSections,
            ContractLintCode::MissingRealityCheck,
            ContractLintCode::MissingArchetype,
            ContractLintCode::MissingTags,
            ContractLintCode::MissingIntegrationTargets,
            ContractLintCode::EntryHeaderTierContradiction,
            ContractLintCode::MissingExhaustionBehavior,
            ContractLintCode::MissingFallbackTrigger,
            ContractLintCode::MissingPosteriorModel,
            ContractLintCode::MissingBayesFactorRefs,
            ContractLintCode::MissingExpectedLossModel,
            ContractLintCode::MissingCalibrationMetadata,
            ContractLintCode::MissingFormalGuarantees,
            ContractLintCode::GuaranteeMissingAssumptions,
            ContractLintCode::GuaranteeMissingBounds,
            ContractLintCode::MissingRequiredGuaranteeKind,
            ContractLintCode::MissingIsomorphismPlan,
            ContractLintCode::MissingLatencyTargets,
            ContractLintCode::NonMonotonicLatencyTargets,
            ContractLintCode::MissingBaselineComparator,
            ContractLintCode::CompositionTestNotRun,
            ContractLintCode::CompositionInterferenceFailed,
            ContractLintCode::MissingCountermeasure,
            ContractLintCode::LegalStatusBlocked,
            ContractLintCode::LegalNeedsCounsel,
            ContractLintCode::MissingReproArtifact,
            ContractLintCode::MissingProvenanceArtifact,
            ContractLintCode::MissingGraduationCriteria,
            ContractLintCode::GraveyardNotVerified,
            ContractLintCode::GraveyardVerifyFailed,
            ContractLintCode::MissingSizeEstimate,
            ContractLintCode::MissingDemoId,
            ContractLintCode::MissingClaimId,
            ContractLintCode::MissingRollbackPolicy,
            ContractLintCode::MissingGalaxyBrainRefs,
            ContractLintCode::DuplicateContract,
        ];
        let mut clause_ids: Vec<&str> = codes.iter().map(|c| c.clause_id()).collect();
        let total = clause_ids.len();
        clause_ids.sort_unstable();
        clause_ids.dedup();
        assert_eq!(clause_ids.len(), total, "clause ids must be distinct");
    }

    #[test]
    fn lint_is_idempotent_and_deterministic() {
        let contracts = vec![example_complete_contract()];
        let first = RecommendationContractLinter::default().evaluate(contracts.clone());
        let second = RecommendationContractLinter::default().evaluate(contracts);
        assert_eq!(first.report_id, second.report_id);
        assert_eq!(
            first.exported_json_stats.sha256,
            second.exported_json_stats.sha256
        );
        assert_eq!(first.findings, second.findings);
    }

    #[test]
    fn report_roundtrips_through_serde() {
        let report = RecommendationContractLinter::default().evaluate(vec![
            example_complete_contract(),
            compile_recommendation_contract(&RecommendationCard::new(
                "rc-bare",
                "bare",
                PriorityTier::A,
                6.0,
                crate::milestone_policy::MathFamily::Other,
            )),
        ]);
        let json = serde_json::to_string(&report).unwrap();
        let restored: ContractLintReport = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.report_id, report.report_id);
        assert_eq!(restored.schema_version, report.schema_version);
        assert_eq!(restored.lint_gate_passes, report.lint_gate_passes);
        assert_eq!(restored.findings, report.findings);
        assert_eq!(restored.blocked_card_ids, report.blocked_card_ids);
    }

    #[test]
    fn json_stats_checksum_is_self_consistent() {
        let report =
            RecommendationContractLinter::default().evaluate(vec![example_complete_contract()]);
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }

    #[test]
    fn replay_command_references_report() {
        let report =
            RecommendationContractLinter::default().evaluate(vec![example_complete_contract()]);
        assert!(
            report
                .replay_command
                .contains("recommendation-contract-lint")
        );
        assert!(report.replay_command.contains(&report.report_id));
    }

    #[test]
    fn blocking_remediations_are_non_empty() {
        let card = RecommendationCard::new(
            "rc-empty",
            "empty",
            PriorityTier::S,
            9.0,
            crate::milestone_policy::MathFamily::Symbolic,
        );
        let report = RecommendationContractLinter::default()
            .evaluate(vec![compile_recommendation_contract(&card)]);
        assert!(!report.lint_gate_passes);
        let remediations = report.blocking_remediations();
        assert!(!remediations.is_empty());
        assert!(remediations.iter().all(|r| !r.trim().is_empty()));
    }

    #[test]
    fn findings_for_filters_by_card() {
        let bare = compile_recommendation_contract(&RecommendationCard::new(
            "rc-bare",
            "bare",
            PriorityTier::A,
            6.0,
            crate::milestone_policy::MathFamily::Other,
        ));
        let report = RecommendationContractLinter::default()
            .evaluate(vec![example_complete_contract(), bare]);
        let bare_findings = report.findings_for("rc-bare");
        assert!(!bare_findings.is_empty());
        assert!(bare_findings.iter().all(|f| f.card_id == "rc-bare"));
        // The complete example contributes no blocking findings.
        assert!(report.findings_for("rc-shadow-run").is_empty());
    }

    #[test]
    fn config_required_guarantee_kinds_dedup_and_roundtrip() {
        let config = ContractLintConfig::default().with_required_guarantee_kinds([
            GuaranteeKind::Conformal,
            GuaranteeKind::Conformal,
            GuaranteeKind::PacBayes,
        ]);
        assert_eq!(config.required_guarantee_kinds.len(), 2);
        let json = serde_json::to_string(&config).unwrap();
        let restored: ContractLintConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, config);
    }
}
