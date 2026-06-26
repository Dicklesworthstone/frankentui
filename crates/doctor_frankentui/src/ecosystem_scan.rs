// SPDX-License-Identifier: Apache-2.0
//! Ecosystem-scan decision records and the crate-adoption governance gate.
//!
//! Operationalizes the alien-graveyard ecosystem-scan rule: before any S/A
//! uplift primitive ships, there must be an explicit adopt-vs-build record tied
//! to FrankenTUI's hard constraints (`#![forbid(unsafe_code)]`, determinism,
//! zero native toolchain dependencies, portability, license posture).
//!
//! # Required outputs
//!
//! - **Machine-readable scan ledger**: one [`EcosystemScanRecord`] per uplift
//!   candidate, carrying considered Rust crates, benchmark deltas, determinism
//!   risk, unsafe-policy fit, license/IP posture, portability, and the final
//!   adopt/build/hybrid decision.
//! - **Human summary**: each decision maps to a migration-architecture stage
//!   (ingestion / IR / translation / certification / cross-cutting) via
//!   [`ArchitectureStage`].
//!
//! # Invariants
//!
//! - Every [`UpliftCandidate`] has a scan record (coverage gate).
//! - Every record lists at least one considered Rust crate, or carries an
//!   explicit none-found statement.
//! - Every decision that rejects full adoption (build / hybrid / deferred)
//!   carries constraint-grounded reject rationale across the required axes
//!   plus a conservative fallback plan.
//! - No crate is adopted if it violates the unsafe-code policy or exceeds the
//!   adopted-determinism-risk ceiling.
//!
//! The gate is consumed by the milestone/release governance layer: the report
//! produced here is the evidence backing the `Ecosystem` quality artifact in
//! [`crate::milestone_policy`], so no S/A alien milestone can be marked passing
//! without scan coverage.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ── Schema Version ───────────────────────────────────────────────────────

/// Schema version for ecosystem-scan reports.
pub const ECOSYSTEM_SCAN_SCHEMA_VERSION: &str = "ecosystem-scan-v1";

// ── Candidate Registry ───────────────────────────────────────────────────

/// An S/A-tier alien-uplift primitive that must carry an adoption decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpliftCandidate {
    /// Contracts-as-code engine and artifact-graph IDs.
    ContractsAsCode,
    /// CEGIS sketch synthesis for unmapped translation holes.
    CegisSynthesis,
    /// E-graph saturation optimizer for generated code.
    EGraphOptimizer,
    /// Abstract-interpretation safety layer (Galois-linked over-approximation).
    AbstractInterpretation,
    /// Concolic differential explorer for source-vs-translated divergence.
    ConcolicDifferential,
    /// Metamorphic-relation oracle library for equivalence checks.
    MetamorphicOracle,
    /// Shadow-run dual execution harness (production adoption wedge).
    ShadowRun,
    /// Controller-composition interference and timescale-separation guardrails.
    ControllerInterferenceGuardrails,
}

impl UpliftCandidate {
    /// Every candidate that must be covered by the scan ledger.
    pub const ALL: &'static [UpliftCandidate] = &[
        UpliftCandidate::ContractsAsCode,
        UpliftCandidate::CegisSynthesis,
        UpliftCandidate::EGraphOptimizer,
        UpliftCandidate::AbstractInterpretation,
        UpliftCandidate::ConcolicDifferential,
        UpliftCandidate::MetamorphicOracle,
        UpliftCandidate::ShadowRun,
        UpliftCandidate::ControllerInterferenceGuardrails,
    ];

    /// Human-readable primitive name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::ContractsAsCode => "Contracts-as-Code Engine",
            Self::CegisSynthesis => "CEGIS Sketch Synthesis",
            Self::EGraphOptimizer => "E-Graph Saturation Optimizer",
            Self::AbstractInterpretation => "Abstract-Interpretation Safety Layer",
            Self::ConcolicDifferential => "Concolic Differential Explorer",
            Self::MetamorphicOracle => "Metamorphic-Relation Oracle",
            Self::ShadowRun => "Shadow-Run Dual Execution Harness",
            Self::ControllerInterferenceGuardrails => "Controller-Interference Guardrails",
        }
    }

    /// Stable kebab-case identifier (used as the record's `candidate_id`).
    #[must_use]
    pub fn candidate_id(self) -> &'static str {
        match self {
            Self::ContractsAsCode => "contracts-as-code",
            Self::CegisSynthesis => "cegis-synthesis",
            Self::EGraphOptimizer => "egraph-optimizer",
            Self::AbstractInterpretation => "abstract-interpretation",
            Self::ConcolicDifferential => "concolic-differential",
            Self::MetamorphicOracle => "metamorphic-oracle",
            Self::ShadowRun => "shadow-run",
            Self::ControllerInterferenceGuardrails => "controller-interference-guardrails",
        }
    }

    /// Alien-graveyard relevance tier.
    #[must_use]
    pub fn tier(self) -> UpliftTier {
        match self {
            Self::ContractsAsCode
            | Self::ShadowRun
            | Self::MetamorphicOracle
            | Self::AbstractInterpretation => UpliftTier::S,
            Self::CegisSynthesis
            | Self::EGraphOptimizer
            | Self::ConcolicDifferential
            | Self::ControllerInterferenceGuardrails => UpliftTier::A,
        }
    }

    /// Migration-architecture stage this primitive serves.
    #[must_use]
    pub fn architecture_stage(self) -> ArchitectureStage {
        match self {
            Self::ContractsAsCode | Self::ShadowRun => ArchitectureStage::CrossCutting,
            Self::CegisSynthesis | Self::EGraphOptimizer => ArchitectureStage::Translation,
            Self::AbstractInterpretation | Self::ConcolicDifferential | Self::MetamorphicOracle => {
                ArchitectureStage::Certification
            }
            Self::ControllerInterferenceGuardrails => ArchitectureStage::CrossCutting,
        }
    }

    /// In-tree module that implements the build/hybrid decision.
    #[must_use]
    pub fn in_tree_module(self) -> &'static str {
        match self {
            Self::ContractsAsCode => "doctor_frankentui::semantic_contract",
            Self::CegisSynthesis => "doctor_frankentui::cegis_synthesis",
            Self::EGraphOptimizer => "doctor_frankentui::egraph_optimizer",
            Self::AbstractInterpretation => "doctor_frankentui::abstract_interpretation",
            Self::ConcolicDifferential => "doctor_frankentui::concolic_differential",
            Self::MetamorphicOracle => "doctor_frankentui::metamorphic_oracle",
            Self::ShadowRun => "doctor_frankentui::shadow_run",
            Self::ControllerInterferenceGuardrails => "doctor_frankentui::controller_composition",
        }
    }
}

/// Alien-graveyard relevance tier (`S` highest, `A` next).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpliftTier {
    /// S-tier: highest-leverage primitive.
    S,
    /// A-tier: high-leverage primitive.
    A,
}

impl UpliftTier {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::S => "s",
            Self::A => "a",
        }
    }
}

/// Migration-architecture stage a primitive maps onto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchitectureStage {
    /// Source ingestion and static/dynamic understanding.
    Ingestion,
    /// Canonical IR and semantic normalization.
    Ir,
    /// Translation engine and code generation.
    Translation,
    /// Validation, certification, and confidence scoring.
    Certification,
    /// Cross-cutting concern (provenance, governance, supervision).
    CrossCutting,
}

impl ArchitectureStage {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ingestion => "ingestion",
            Self::Ir => "ir",
            Self::Translation => "translation",
            Self::Certification => "certification",
            Self::CrossCutting => "cross_cutting",
        }
    }
}

/// A constraint axis that a reject (build/hybrid/deferred) rationale must
/// address to be machine-valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintAxis {
    /// Performance / benchmark posture.
    Performance,
    /// Determinism and replayability.
    Determinism,
    /// `#![forbid(unsafe_code)]` policy fit.
    UnsafePolicy,
    /// License / IP posture.
    License,
    /// Portability and native-toolchain footprint.
    Portability,
}

impl ConstraintAxis {
    /// Every constraint axis the gate can require.
    pub const ALL: &'static [ConstraintAxis] = &[
        ConstraintAxis::Performance,
        ConstraintAxis::Determinism,
        ConstraintAxis::UnsafePolicy,
        ConstraintAxis::License,
        ConstraintAxis::Portability,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Performance => "performance",
            Self::Determinism => "determinism",
            Self::UnsafePolicy => "unsafe_policy",
            Self::License => "license",
            Self::Portability => "portability",
        }
    }
}

/// The adopt-vs-build verdict for a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdoptionDecision {
    /// Adopt an external crate wholesale.
    Adopt,
    /// Build a bespoke in-tree implementation.
    Build,
    /// Adopt a substrate crate but build the domain logic on top.
    Hybrid,
    /// Decision deferred pending more evidence.
    Deferred,
}

impl AdoptionDecision {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Adopt => "adopt",
            Self::Build => "build",
            Self::Hybrid => "hybrid",
            Self::Deferred => "deferred",
        }
    }

    /// Whether this decision rejects full external adoption and therefore
    /// requires constraint-grounded rationale plus a fallback plan.
    #[must_use]
    pub fn rejects_full_adoption(self) -> bool {
        !matches!(self, Self::Adopt)
    }

    /// Whether this decision ships in-tree code (build or hybrid).
    #[must_use]
    pub fn ships_in_tree_code(self) -> bool {
        matches!(self, Self::Build | Self::Hybrid)
    }

    /// Whether this decision relies on at least one adopted crate.
    #[must_use]
    pub fn relies_on_adopted_crate(self) -> bool {
        matches!(self, Self::Adopt | Self::Hybrid)
    }
}

/// Per-crate consideration verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrateVerdict {
    /// Selected for adoption (whole or as a hybrid substrate).
    Selected,
    /// Considered but rejected.
    Rejected,
    /// Still under evaluation (only valid for a `Deferred` decision).
    Considered,
}

impl CrateVerdict {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Selected => "selected",
            Self::Rejected => "rejected",
            Self::Considered => "considered",
        }
    }
}

/// Determinism risk of a candidate crate (`None` lowest, `High` highest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeterminismRisk {
    /// Fully deterministic given fixed inputs.
    None,
    /// Deterministic under a documented seed/config discipline.
    Low,
    /// Determinism depends on environment or hidden global state.
    Medium,
    /// Inherently non-deterministic (threads, clocks, solver internals).
    High,
}

impl DeterminismRisk {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// How a candidate crate fits the workspace `#![forbid(unsafe_code)]` policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsafePolicyFit {
    /// No `unsafe` anywhere in the dependency surface.
    Clean,
    /// `unsafe` exists but is contained behind a safe, audited boundary.
    Contained,
    /// Requires `unsafe`/FFI in a way that conflicts with the policy.
    Violates,
    /// Unsafe footprint not yet assessed.
    Unknown,
}

impl UnsafePolicyFit {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Contained => "contained",
            Self::Violates => "violates",
            Self::Unknown => "unknown",
        }
    }
}

/// License / IP posture of a candidate crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LicensePosture {
    /// Permissive (MIT / Apache-2.0 / BSD).
    Permissive,
    /// Weak copyleft (MPL / LGPL).
    WeakCopyleft,
    /// Strong copyleft (GPL / AGPL).
    StrongCopyleft,
    /// Proprietary / closed.
    Proprietary,
    /// Public domain / unlicensed-permissive.
    PublicDomain,
    /// License not assessed.
    Unknown,
    /// No external license applies (no crate considered).
    NotApplicable,
}

impl LicensePosture {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Permissive => "permissive",
            Self::WeakCopyleft => "weak_copyleft",
            Self::StrongCopyleft => "strong_copyleft",
            Self::Proprietary => "proprietary",
            Self::PublicDomain => "public_domain",
            Self::Unknown => "unknown",
            Self::NotApplicable => "not_applicable",
        }
    }
}

/// Portability / native-toolchain footprint of a candidate crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortabilityPosture {
    /// Pure Rust, builds everywhere the workspace builds.
    Portable,
    /// Mostly portable with minor platform caveats.
    MostlyPortable,
    /// Tied to a specific platform.
    PlatformSpecific,
    /// Requires a non-cargo native toolchain (LLVM, Z3, C build).
    RequiresNativeToolchain,
    /// Portability not assessed.
    Unknown,
}

impl PortabilityPosture {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Portable => "portable",
            Self::MostlyPortable => "mostly_portable",
            Self::PlatformSpecific => "platform_specific",
            Self::RequiresNativeToolchain => "requires_native_toolchain",
            Self::Unknown => "unknown",
        }
    }
}

// ── Record Components ─────────────────────────────────────────────────────

/// A benchmark/cost delta observed (or projected) for a candidate crate
/// relative to the in-tree baseline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkDelta {
    /// Metric name (e.g. `synthesis_latency`, `extraction_allocs`).
    pub metric: String,
    /// Human-readable summary of the delta.
    pub summary: String,
    /// Signed percentage delta vs baseline, if quantified
    /// (positive = candidate is more expensive).
    pub delta_pct: Option<f64>,
}

impl BenchmarkDelta {
    /// Build a qualitative delta with no numeric value.
    #[must_use]
    pub fn qualitative(metric: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            metric: metric.into(),
            summary: summary.into(),
            delta_pct: None,
        }
    }

    /// Build a quantified delta.
    #[must_use]
    pub fn quantified(
        metric: impl Into<String>,
        summary: impl Into<String>,
        delta_pct: f64,
    ) -> Self {
        Self {
            metric: metric.into(),
            summary: summary.into(),
            delta_pct: Some(delta_pct),
        }
    }
}

/// A single considered Rust crate for an uplift candidate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CrateCandidate {
    /// Crate name as published.
    pub crate_name: String,
    /// Version or semver range considered, if any.
    pub considered_version: Option<String>,
    /// Source/evidence URL (crates.io, repo, paper).
    pub source_url: Option<String>,
    /// Benchmark/cost deltas vs the in-tree baseline.
    pub benchmark_deltas: Vec<BenchmarkDelta>,
    /// Determinism risk assessment.
    pub determinism_risk: DeterminismRisk,
    /// Unsafe-policy fit.
    pub unsafe_policy_fit: UnsafePolicyFit,
    /// License / IP posture.
    pub license_posture: LicensePosture,
    /// Portability posture.
    pub portability: PortabilityPosture,
    /// Verdict for this specific crate.
    pub verdict: CrateVerdict,
    /// Constraint-grounded rationale for the verdict.
    pub rationale: String,
}

impl CrateCandidate {
    /// Build a crate-candidate record.
    #[must_use]
    pub fn new(crate_name: impl Into<String>, verdict: CrateVerdict) -> Self {
        Self {
            crate_name: crate_name.into(),
            considered_version: None,
            source_url: None,
            benchmark_deltas: Vec::new(),
            // Conservative default until `posture` is set: treat an unassessed
            // crate as worst-case so it cannot be silently adopted.
            determinism_risk: DeterminismRisk::High,
            unsafe_policy_fit: UnsafePolicyFit::Unknown,
            license_posture: LicensePosture::Unknown,
            portability: PortabilityPosture::Unknown,
            verdict,
            rationale: String::new(),
        }
    }

    /// Set the considered version.
    #[must_use]
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.considered_version = Some(version.into());
        self
    }

    /// Set the source/evidence URL.
    #[must_use]
    pub fn source(mut self, url: impl Into<String>) -> Self {
        self.source_url = Some(url.into());
        self
    }

    /// Push a benchmark delta.
    #[must_use]
    pub fn with_benchmark(mut self, delta: BenchmarkDelta) -> Self {
        self.benchmark_deltas.push(delta);
        self
    }

    /// Set the determinism / unsafe / license / portability posture in one call.
    #[must_use]
    pub fn posture(
        mut self,
        determinism_risk: DeterminismRisk,
        unsafe_policy_fit: UnsafePolicyFit,
        license_posture: LicensePosture,
        portability: PortabilityPosture,
    ) -> Self {
        self.determinism_risk = determinism_risk;
        self.unsafe_policy_fit = unsafe_policy_fit;
        self.license_posture = license_posture;
        self.portability = portability;
        self
    }

    /// Set the verdict rationale.
    #[must_use]
    pub fn rationale(mut self, rationale: impl Into<String>) -> Self {
        self.rationale = rationale.into();
        self
    }
}

/// Constraint-grounded rationale for rejecting full adoption.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConstraintRationale {
    /// The constraint axis addressed.
    pub axis: ConstraintAxis,
    /// The rationale tied to that axis.
    pub rationale: String,
}

impl ConstraintRationale {
    /// Build a constraint rationale.
    #[must_use]
    pub fn new(axis: ConstraintAxis, rationale: impl Into<String>) -> Self {
        Self {
            axis,
            rationale: rationale.into(),
        }
    }
}

// ── Scan Record ──────────────────────────────────────────────────────────

/// One adopt-vs-build decision record for an uplift candidate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EcosystemScanRecord {
    /// The candidate this record covers.
    pub candidate: UpliftCandidate,
    /// Stable candidate identifier (must equal `candidate.candidate_id()`).
    pub candidate_id: String,
    /// Relevance tier.
    pub tier: UpliftTier,
    /// Migration-architecture stage served.
    pub architecture_stage: ArchitectureStage,
    /// The adopt-vs-build decision.
    pub decision: AdoptionDecision,
    /// Human summary of the decision.
    pub decision_summary: String,
    /// Considered Rust crates (may be empty iff `none_found_statement` is set).
    pub considered_crates: Vec<CrateCandidate>,
    /// Explicit statement when no suitable crate exists.
    pub none_found_statement: Option<String>,
    /// Crates actually adopted (for `Adopt`/`Hybrid` decisions).
    pub adopted_crates: Vec<String>,
    /// Constraint-grounded rationale for rejecting full adoption.
    pub reject_rationale: Vec<ConstraintRationale>,
    /// Conservative fallback plan when adoption is rejected.
    pub fallback_plan: Option<String>,
    /// In-tree module implementing the build/hybrid decision.
    pub in_tree_module: Option<String>,
}

impl EcosystemScanRecord {
    /// Start a record for a candidate, deriving identity/tier/stage/module
    /// from the candidate itself.
    #[must_use]
    pub fn new(candidate: UpliftCandidate, decision: AdoptionDecision) -> Self {
        let in_tree_module = if decision.ships_in_tree_code() {
            Some(candidate.in_tree_module().to_string())
        } else {
            None
        };
        Self {
            candidate,
            candidate_id: candidate.candidate_id().to_string(),
            tier: candidate.tier(),
            architecture_stage: candidate.architecture_stage(),
            decision,
            decision_summary: String::new(),
            considered_crates: Vec::new(),
            none_found_statement: None,
            adopted_crates: Vec::new(),
            reject_rationale: Vec::new(),
            fallback_plan: None,
            in_tree_module,
        }
    }

    /// Set the decision summary.
    #[must_use]
    pub fn summary(mut self, summary: impl Into<String>) -> Self {
        self.decision_summary = summary.into();
        self
    }

    /// Push a considered crate.
    #[must_use]
    pub fn with_crate(mut self, candidate: CrateCandidate) -> Self {
        self.considered_crates.push(candidate);
        self
    }

    /// Record an explicit none-found statement.
    #[must_use]
    pub fn none_found(mut self, statement: impl Into<String>) -> Self {
        self.none_found_statement = Some(statement.into());
        self
    }

    /// Mark a crate as adopted.
    #[must_use]
    pub fn adopt_crate(mut self, name: impl Into<String>) -> Self {
        self.adopted_crates.push(name.into());
        self
    }

    /// Push a constraint-grounded reject rationale.
    #[must_use]
    pub fn with_reject_rationale(mut self, rationale: ConstraintRationale) -> Self {
        self.reject_rationale.push(rationale);
        self
    }

    /// Set the conservative fallback plan.
    #[must_use]
    pub fn fallback(mut self, plan: impl Into<String>) -> Self {
        self.fallback_plan = Some(plan.into());
        self
    }

    /// Whether the record lists at least one considered crate.
    #[must_use]
    pub fn has_considered_crate(&self) -> bool {
        !self.considered_crates.is_empty()
    }

    /// The set of constraint axes covered by the reject rationale.
    #[must_use]
    pub fn covered_axes(&self) -> Vec<ConstraintAxis> {
        let mut axes: Vec<ConstraintAxis> = self.reject_rationale.iter().map(|r| r.axis).collect();
        axes.sort();
        axes.dedup();
        axes
    }
}

// ── Violations ───────────────────────────────────────────────────────────

/// Severity of a scan violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanSeverity {
    /// Blocks the scan gate.
    Block,
    /// Advisory only.
    Warn,
}

impl ScanSeverity {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Warn => "warn",
        }
    }
}

/// Machine-readable code for a scan-readiness violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanViolationCode {
    /// A candidate has no scan record (coverage gap).
    MissingCandidateCoverage,
    /// Two records cover the same candidate.
    DuplicateCandidate,
    /// `candidate_id` is empty.
    EmptyCandidateId,
    /// `candidate_id` does not match the candidate's canonical id.
    CandidateIdMismatch,
    /// No considered crate and no none-found statement.
    NoCrateConsideredWithoutNoneFound,
    /// None-found statement present but empty.
    NoneFoundStatementEmpty,
    /// A rejected crate has no rationale.
    RejectedCrateMissingRationale,
    /// A crate is marked `Selected` but the decision is `Build`.
    SelectedCrateContradictsBuild,
    /// An adopt/hybrid decision names no adopted crate.
    AdoptWithoutAdoptedCrate,
    /// A reject decision is missing a required constraint axis.
    MissingRequiredConstraintAxis,
    /// A constraint rationale has empty text.
    ConstraintRationaleEmpty,
    /// A reject decision has no fallback plan.
    MissingFallbackPlan,
    /// A build/hybrid decision names no in-tree module.
    MissingInTreeModule,
    /// An adopted crate violates the unsafe-code policy.
    UnsafePolicyViolationAdopted,
    /// An adopted crate exceeds the determinism-risk ceiling.
    AdoptedCrateDeterminismRiskTooHigh,
    /// A decision summary is empty.
    EmptyDecisionSummary,
}

impl ScanViolationCode {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingCandidateCoverage => "missing_candidate_coverage",
            Self::DuplicateCandidate => "duplicate_candidate",
            Self::EmptyCandidateId => "empty_candidate_id",
            Self::CandidateIdMismatch => "candidate_id_mismatch",
            Self::NoCrateConsideredWithoutNoneFound => "no_crate_considered_without_none_found",
            Self::NoneFoundStatementEmpty => "none_found_statement_empty",
            Self::RejectedCrateMissingRationale => "rejected_crate_missing_rationale",
            Self::SelectedCrateContradictsBuild => "selected_crate_contradicts_build",
            Self::AdoptWithoutAdoptedCrate => "adopt_without_adopted_crate",
            Self::MissingRequiredConstraintAxis => "missing_required_constraint_axis",
            Self::ConstraintRationaleEmpty => "constraint_rationale_empty",
            Self::MissingFallbackPlan => "missing_fallback_plan",
            Self::MissingInTreeModule => "missing_in_tree_module",
            Self::UnsafePolicyViolationAdopted => "unsafe_policy_violation_adopted",
            Self::AdoptedCrateDeterminismRiskTooHigh => "adopted_crate_determinism_risk_too_high",
            Self::EmptyDecisionSummary => "empty_decision_summary",
        }
    }
}

/// A structured scan-readiness diagnostic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanViolation {
    /// Machine-readable code.
    pub code: ScanViolationCode,
    /// Severity.
    pub severity: ScanSeverity,
    /// Candidate this pertains to.
    pub candidate_id: String,
    /// Human-readable description.
    pub detail: String,
    /// Actionable remediation clause.
    pub remediation: String,
}

impl ScanViolation {
    fn block(
        code: ScanViolationCode,
        candidate_id: impl Into<String>,
        detail: impl Into<String>,
        remediation: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity: ScanSeverity::Block,
            candidate_id: candidate_id.into(),
            detail: detail.into(),
            remediation: remediation.into(),
        }
    }
}

// ── Configuration ────────────────────────────────────────────────────────

/// Configuration for the ecosystem-scan gate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EcosystemScanConfig {
    /// Constraint axes a reject rationale must address.
    pub required_constraint_axes: Vec<ConstraintAxis>,
    /// Require full coverage of every `UpliftCandidate`.
    pub require_full_candidate_coverage: bool,
    /// Require a fallback plan for non-adopt decisions.
    pub require_fallback_for_non_adopt: bool,
    /// Require an in-tree module for build/hybrid decisions.
    pub require_in_tree_module_for_build: bool,
    /// Forbid adopting crates that violate the unsafe-code policy.
    pub forbid_unsafe_adoption: bool,
    /// Maximum determinism risk allowed for an adopted crate.
    pub max_adopted_determinism_risk: DeterminismRisk,
}

impl Default for EcosystemScanConfig {
    fn default() -> Self {
        Self {
            required_constraint_axes: ConstraintAxis::ALL.to_vec(),
            require_full_candidate_coverage: true,
            require_fallback_for_non_adopt: true,
            require_in_tree_module_for_build: true,
            forbid_unsafe_adoption: true,
            max_adopted_determinism_risk: DeterminismRisk::Low,
        }
    }
}

impl EcosystemScanConfig {
    /// Override the required constraint axes.
    #[must_use]
    pub fn with_required_axes(mut self, axes: impl IntoIterator<Item = ConstraintAxis>) -> Self {
        let mut axes: Vec<ConstraintAxis> = axes.into_iter().collect();
        axes.sort();
        axes.dedup();
        self.required_constraint_axes = axes;
        self
    }

    /// Toggle full-coverage enforcement.
    #[must_use]
    pub fn with_full_coverage(mut self, require: bool) -> Self {
        self.require_full_candidate_coverage = require;
        self
    }

    /// Set the adopted-crate determinism-risk ceiling.
    #[must_use]
    pub fn with_max_adopted_determinism_risk(mut self, risk: DeterminismRisk) -> Self {
        self.max_adopted_determinism_risk = risk;
        self
    }
}

// ── Report ───────────────────────────────────────────────────────────────

/// Aggregate counts for a scan report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EcosystemScanSummary {
    /// Total records evaluated.
    pub total_records: usize,
    /// Records with at least one blocking violation.
    pub blocked_records: usize,
    /// Total blocking violations.
    pub blocking_violation_count: usize,
    /// Total advisory violations.
    pub advisory_violation_count: usize,
    /// Count of candidates with a missing scan record.
    pub missing_candidate_count: usize,
    /// Adopt decisions.
    pub adopt_count: usize,
    /// Build decisions.
    pub build_count: usize,
    /// Hybrid decisions.
    pub hybrid_count: usize,
    /// Deferred decisions.
    pub deferred_count: usize,
}

/// Deterministic JSON-stats artifact (content + checksum).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EcosystemScanJsonStatsArtifact {
    /// Suggested relative output path.
    pub path: String,
    /// SHA-256 of `content`.
    pub sha256: String,
    /// Serialized JSON content.
    pub content: String,
}

/// Full ecosystem-scan report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EcosystemScanReport {
    /// Schema version constant.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Configuration used.
    pub config: EcosystemScanConfig,
    /// Scan records (input order preserved).
    pub records: Vec<EcosystemScanRecord>,
    /// Violations found (coverage + per-record).
    pub violations: Vec<ScanViolation>,
    /// Aggregate summary.
    pub summary: EcosystemScanSummary,
    /// Whether every candidate is covered by a record.
    pub coverage_complete: bool,
    /// Whether the scan gate passes (no blocking violations).
    pub scan_gate_passes: bool,
    /// Candidate ids blocked by the gate.
    pub blocked_candidate_ids: Vec<String>,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: EcosystemScanJsonStatsArtifact,
    /// Replay command for the report.
    pub replay_command: String,
}

impl EcosystemScanReport {
    /// Every blocking remediation clause, in report order.
    #[must_use]
    pub fn blocking_remediations(&self) -> Vec<String> {
        self.violations
            .iter()
            .filter(|v| v.severity == ScanSeverity::Block)
            .map(|v| v.remediation.clone())
            .collect()
    }

    /// Candidates with no scan record (empty when coverage is complete).
    #[must_use]
    pub fn missing_candidates(&self) -> Vec<UpliftCandidate> {
        UpliftCandidate::ALL
            .iter()
            .copied()
            .filter(|candidate| {
                !self
                    .records
                    .iter()
                    .any(|record| record.candidate == *candidate)
            })
            .collect()
    }

    /// Whether this report can back the `Ecosystem` quality artifact consumed
    /// by the milestone/release governance gate.
    ///
    /// True only when coverage is complete and the gate passes — this is the
    /// hook that prevents an S/A alien milestone from being marked passing
    /// without scan coverage.
    #[must_use]
    pub fn serves_quality_artifact(&self) -> bool {
        self.coverage_complete && self.scan_gate_passes
    }
}

// ── Gate ─────────────────────────────────────────────────────────────────

/// Validates scan records against the ecosystem-scan governance policy.
#[derive(Debug, Clone, Default)]
pub struct EcosystemScanGate {
    config: EcosystemScanConfig,
}

impl EcosystemScanGate {
    /// Build a gate from configuration.
    #[must_use]
    pub fn new(config: EcosystemScanConfig) -> Self {
        Self { config }
    }

    /// Borrow the configuration.
    #[must_use]
    pub fn config(&self) -> &EcosystemScanConfig {
        &self.config
    }

    /// Evaluate scan records and produce a deterministic report.
    #[must_use]
    pub fn evaluate(&self, records: Vec<EcosystemScanRecord>) -> EcosystemScanReport {
        let mut violations: Vec<ScanViolation> = Vec::new();

        let missing_candidate_count = self.check_coverage(&records, &mut violations);
        self.check_duplicates(&records, &mut violations);
        for record in &records {
            self.check_record(record, &mut violations);
        }

        let summary = summarize(&records, &violations, missing_candidate_count);
        let coverage_complete = missing_candidate_count == 0;
        let scan_gate_passes = violations.iter().all(|v| v.severity != ScanSeverity::Block);

        let mut blocked_candidate_ids: Vec<String> = violations
            .iter()
            .filter(|v| v.severity == ScanSeverity::Block)
            .map(|v| v.candidate_id.clone())
            .collect();
        blocked_candidate_ids.sort();
        blocked_candidate_ids.dedup();

        let report_id = report_id_for(&self.config, &records);
        let exported_json_stats =
            export_json_stats(&report_id, &self.config, &records, &violations, &summary);
        let replay_command = format!(
            "doctor_frankentui ecosystem-scan --report-id {report_id} \
             --required-axes {} --max-adopted-determinism-risk {}",
            self.config.required_constraint_axes.len(),
            self.config.max_adopted_determinism_risk.as_str()
        );

        EcosystemScanReport {
            schema_version: ECOSYSTEM_SCAN_SCHEMA_VERSION.to_string(),
            report_id,
            config: self.config.clone(),
            records,
            violations,
            summary,
            coverage_complete,
            scan_gate_passes,
            blocked_candidate_ids,
            exported_json_stats,
            replay_command,
        }
    }

    /// Returns the number of candidates with no record.
    fn check_coverage(
        &self,
        records: &[EcosystemScanRecord],
        violations: &mut Vec<ScanViolation>,
    ) -> usize {
        if !self.config.require_full_candidate_coverage {
            return 0;
        }
        let mut missing = 0;
        for candidate in UpliftCandidate::ALL.iter().copied() {
            if !records.iter().any(|record| record.candidate == candidate) {
                missing += 1;
                violations.push(ScanViolation::block(
                    ScanViolationCode::MissingCandidateCoverage,
                    candidate.candidate_id(),
                    format!(
                        "candidate {} has no ecosystem-scan record",
                        candidate.name()
                    ),
                    "add an adopt-vs-build scan record for this candidate",
                ));
            }
        }
        missing
    }

    fn check_duplicates(
        &self,
        records: &[EcosystemScanRecord],
        violations: &mut Vec<ScanViolation>,
    ) {
        for candidate in UpliftCandidate::ALL.iter().copied() {
            let count = records
                .iter()
                .filter(|record| record.candidate == candidate)
                .count();
            if count > 1 {
                violations.push(ScanViolation::block(
                    ScanViolationCode::DuplicateCandidate,
                    candidate.candidate_id(),
                    format!(
                        "candidate {} has {count} scan records; expected exactly one",
                        candidate.name()
                    ),
                    "merge the duplicate records into a single decision record",
                ));
            }
        }
    }

    fn check_record(&self, record: &EcosystemScanRecord, violations: &mut Vec<ScanViolation>) {
        let id = record.candidate_id.clone();

        if record.candidate_id.trim().is_empty() {
            violations.push(ScanViolation::block(
                ScanViolationCode::EmptyCandidateId,
                record.candidate.candidate_id(),
                "candidate_id must not be empty",
                "set candidate_id to the candidate's canonical id",
            ));
        } else if record.candidate_id != record.candidate.candidate_id() {
            violations.push(ScanViolation::block(
                ScanViolationCode::CandidateIdMismatch,
                &id,
                format!(
                    "candidate_id `{}` does not match canonical id `{}`",
                    record.candidate_id,
                    record.candidate.candidate_id()
                ),
                "use EcosystemScanRecord::new to keep the id in sync with the candidate",
            ));
        }

        if record.decision_summary.trim().is_empty() {
            violations.push(ScanViolation::block(
                ScanViolationCode::EmptyDecisionSummary,
                &id,
                "decision_summary must not be empty",
                "summarize the adopt-vs-build rationale for operators",
            ));
        }

        self.check_crate_coverage(record, &id, violations);
        self.check_crate_verdicts(record, &id, violations);
        self.check_decision_obligations(record, &id, violations);
        self.check_adopted_crate_constraints(record, &id, violations);
    }

    fn check_crate_coverage(
        &self,
        record: &EcosystemScanRecord,
        id: &str,
        violations: &mut Vec<ScanViolation>,
    ) {
        if record.considered_crates.is_empty() {
            match &record.none_found_statement {
                None => violations.push(ScanViolation::block(
                    ScanViolationCode::NoCrateConsideredWithoutNoneFound,
                    id,
                    "no considered crate and no none-found statement",
                    "list at least one considered Rust crate, or add an explicit none-found statement",
                )),
                Some(statement) if statement.trim().is_empty() => {
                    violations.push(ScanViolation::block(
                        ScanViolationCode::NoneFoundStatementEmpty,
                        id,
                        "none_found_statement is present but empty",
                        "explain why no suitable crate exists in the Rust ecosystem",
                    ));
                }
                Some(_) => {}
            }
        }
    }

    fn check_crate_verdicts(
        &self,
        record: &EcosystemScanRecord,
        id: &str,
        violations: &mut Vec<ScanViolation>,
    ) {
        for candidate in &record.considered_crates {
            if candidate.verdict == CrateVerdict::Rejected && candidate.rationale.trim().is_empty()
            {
                violations.push(ScanViolation::block(
                    ScanViolationCode::RejectedCrateMissingRationale,
                    id,
                    format!("rejected crate `{}` has no rationale", candidate.crate_name),
                    "give a constraint-grounded reason for rejecting this crate",
                ));
            }
            if candidate.verdict == CrateVerdict::Selected
                && record.decision == AdoptionDecision::Build
            {
                violations.push(ScanViolation::block(
                    ScanViolationCode::SelectedCrateContradictsBuild,
                    id,
                    format!(
                        "crate `{}` is Selected but the decision is Build",
                        candidate.crate_name
                    ),
                    "switch the decision to Adopt/Hybrid, or mark the crate Rejected",
                ));
            }
        }
    }

    fn check_decision_obligations(
        &self,
        record: &EcosystemScanRecord,
        id: &str,
        violations: &mut Vec<ScanViolation>,
    ) {
        if record.decision.relies_on_adopted_crate() && record.adopted_crates.is_empty() {
            violations.push(ScanViolation::block(
                ScanViolationCode::AdoptWithoutAdoptedCrate,
                id,
                format!(
                    "decision {} relies on adoption but names no adopted crate",
                    record.decision.as_str()
                ),
                "list the crate(s) being adopted in adopted_crates",
            ));
        }

        if record.decision.rejects_full_adoption() {
            let covered = record.covered_axes();
            for axis in &self.config.required_constraint_axes {
                if !covered.contains(axis) {
                    violations.push(ScanViolation::block(
                        ScanViolationCode::MissingRequiredConstraintAxis,
                        id,
                        format!(
                            "reject decision {} is missing the `{}` constraint axis",
                            record.decision.as_str(),
                            axis.as_str()
                        ),
                        format!(
                            "add a constraint-grounded rationale for the `{}` axis",
                            axis.as_str()
                        ),
                    ));
                }
            }
            for rationale in &record.reject_rationale {
                if rationale.rationale.trim().is_empty() {
                    violations.push(ScanViolation::block(
                        ScanViolationCode::ConstraintRationaleEmpty,
                        id,
                        format!(
                            "constraint axis `{}` has empty rationale",
                            rationale.axis.as_str()
                        ),
                        "fill in the rationale for this constraint axis",
                    ));
                }
            }
            if self.config.require_fallback_for_non_adopt
                && record
                    .fallback_plan
                    .as_ref()
                    .is_none_or(|plan| plan.trim().is_empty())
            {
                violations.push(ScanViolation::block(
                    ScanViolationCode::MissingFallbackPlan,
                    id,
                    "reject decision has no conservative fallback plan",
                    "describe the conservative fallback if the in-tree primitive fails",
                ));
            }
        }

        if self.config.require_in_tree_module_for_build
            && record.decision.ships_in_tree_code()
            && record
                .in_tree_module
                .as_ref()
                .is_none_or(|module| module.trim().is_empty())
        {
            violations.push(ScanViolation::block(
                ScanViolationCode::MissingInTreeModule,
                id,
                format!(
                    "decision {} ships in-tree code but names no module",
                    record.decision.as_str()
                ),
                "set in_tree_module to the implementing module path",
            ));
        }
    }

    fn check_adopted_crate_constraints(
        &self,
        record: &EcosystemScanRecord,
        id: &str,
        violations: &mut Vec<ScanViolation>,
    ) {
        for candidate in &record.considered_crates {
            if candidate.verdict != CrateVerdict::Selected {
                continue;
            }
            if self.config.forbid_unsafe_adoption
                && candidate.unsafe_policy_fit == UnsafePolicyFit::Violates
            {
                violations.push(ScanViolation::block(
                    ScanViolationCode::UnsafePolicyViolationAdopted,
                    id,
                    format!(
                        "adopted crate `{}` violates the unsafe-code policy",
                        candidate.crate_name
                    ),
                    "do not adopt a crate that requires policy-violating unsafe/FFI",
                ));
            }
            if candidate.determinism_risk > self.config.max_adopted_determinism_risk {
                violations.push(ScanViolation::block(
                    ScanViolationCode::AdoptedCrateDeterminismRiskTooHigh,
                    id,
                    format!(
                        "adopted crate `{}` has determinism risk `{}`, above the `{}` ceiling",
                        candidate.crate_name,
                        candidate.determinism_risk.as_str(),
                        self.config.max_adopted_determinism_risk.as_str()
                    ),
                    "only adopt crates whose determinism risk is within the configured ceiling",
                ));
            }
        }
    }
}

// ── Canonical Ledger ─────────────────────────────────────────────────────

/// The canonical adopt-vs-build ledger for every S/A uplift candidate, tied to
/// FrankenTUI's actual constraints. This is the baseline the governance gate
/// consumes; it passes a default [`EcosystemScanGate`].
#[must_use]
pub fn canonical_ledger() -> Vec<EcosystemScanRecord> {
    vec![
        contracts_as_code_record(),
        cegis_record(),
        egraph_record(),
        abstract_interpretation_record(),
        concolic_record(),
        metamorphic_record(),
        shadow_run_record(),
        controller_guardrails_record(),
    ]
}

/// Evaluate the [`canonical_ledger`] under a default gate.
#[must_use]
pub fn canonical_scan_report() -> EcosystemScanReport {
    EcosystemScanGate::default().evaluate(canonical_ledger())
}

fn contracts_as_code_record() -> EcosystemScanRecord {
    EcosystemScanRecord::new(UpliftCandidate::ContractsAsCode, AdoptionDecision::Hybrid)
        .summary(
            "Adopt serde/serde_json/sha2 as the serialization + content-hash substrate; \
             build the claim/evidence/policy/trace artifact graph in-tree.",
        )
        .adopt_crate("serde")
        .adopt_crate("serde_json")
        .adopt_crate("sha2")
        .with_crate(
            CrateCandidate::new("serde", CrateVerdict::Selected)
                .version("1.0")
                .source("https://crates.io/crates/serde")
                .posture(
                    DeterminismRisk::None,
                    UnsafePolicyFit::Contained,
                    LicensePosture::Permissive,
                    PortabilityPosture::Portable,
                )
                .rationale("De-facto serialization substrate; deterministic, pure-Rust."),
        )
        .with_crate(
            CrateCandidate::new("sha2", CrateVerdict::Selected)
                .version("0.11")
                .source("https://crates.io/crates/sha2")
                .posture(
                    DeterminismRisk::None,
                    UnsafePolicyFit::Contained,
                    LicensePosture::Permissive,
                    PortabilityPosture::Portable,
                )
                .rationale("Content-addressed artifact IDs need a stable, portable hash."),
        )
        .with_crate(
            CrateCandidate::new("contracts", CrateVerdict::Rejected)
                .version("0.6")
                .source("https://crates.io/crates/contracts")
                .posture(
                    DeterminismRisk::None,
                    UnsafePolicyFit::Clean,
                    LicensePosture::Permissive,
                    PortabilityPosture::Portable,
                )
                .rationale(
                    "Only pre/post-condition macros; provides no artifact-graph, \
                     provenance ledger, or claim/evidence/policy/trace linkage.",
                ),
        )
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::Performance,
            "Artifact-graph traversal is not a hotspot; bespoke code avoids macro overhead.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::Determinism,
            "Provenance IDs must be content-addressed and replayable; in-tree hashing guarantees this.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::UnsafePolicy,
            "All adopted substrate crates are unsafe-contained; graph logic itself is unsafe-free.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::License,
            "No copyleft pulled in; serde/serde_json/sha2 are permissive.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::Portability,
            "Artifact graph is pure Rust over the migration IR; no native deps.",
        ))
        .fallback(
            "If the artifact graph regresses, fall back to flat JSONL evidence records \
             keyed by content hash.",
        )
}

fn cegis_record() -> EcosystemScanRecord {
    EcosystemScanRecord::new(UpliftCandidate::CegisSynthesis, AdoptionDecision::Build)
        .summary(
            "Build the counterexample-guided synthesis loop in-tree over migration IR; \
             SMT-solver bindings are rejected on portability/unsafe/determinism grounds.",
        )
        .with_crate(
            CrateCandidate::new("z3", CrateVerdict::Rejected)
                .version("0.12")
                .source("https://crates.io/crates/z3")
                .with_benchmark(BenchmarkDelta::qualitative(
                    "build_footprint",
                    "Adds a multi-MB native Z3 toolchain dependency.",
                ))
                .posture(
                    DeterminismRisk::High,
                    UnsafePolicyFit::Violates,
                    LicensePosture::Permissive,
                    PortabilityPosture::RequiresNativeToolchain,
                )
                .rationale(
                    "FFI to native Z3 requires unsafe and a non-cargo toolchain; \
                     solver internals are not bit-for-bit reproducible across versions.",
                ),
        )
        .with_crate(
            CrateCandidate::new("rsmt2", CrateVerdict::Rejected)
                .version("0.17")
                .source("https://crates.io/crates/rsmt2")
                .posture(
                    DeterminismRisk::High,
                    UnsafePolicyFit::Contained,
                    LicensePosture::Permissive,
                    PortabilityPosture::RequiresNativeToolchain,
                )
                .rationale(
                    "Spawns an external SMT solver process; determinism and portability \
                     depend on an out-of-tree binary we cannot pin.",
                ),
        )
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::Performance,
            "Translation holes are small and bounded; a domain sketch search beats general SMT latency.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::Determinism,
            "SMT solver internals vary across versions/platforms, breaking replayable counterexamples.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::UnsafePolicy,
            "Native solver FFI conflicts with workspace #![forbid(unsafe_code)].",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::License,
            "Bindings are permissive, but the underlying native toolchain widens the IP surface.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::Portability,
            "Requires a native Z3/solver toolchain absent from the pure-cargo build.",
        ))
        .fallback(
            "If a hole resists the bounded sketch search, emit a typed TODO stub plus a \
             certification gap rather than an unproven synthesis.",
        )
}

fn egraph_record() -> EcosystemScanRecord {
    EcosystemScanRecord::new(UpliftCandidate::EGraphOptimizer, AdoptionDecision::Build)
        .summary(
            "Build a node-budgeted e-graph optimizer over migration codegen IR; the canonical \
             `egg` crate is rejected on integration-cost and extraction-control grounds.",
        )
        .with_crate(
            CrateCandidate::new("egg", CrateVerdict::Rejected)
                .version("0.9")
                .source("https://crates.io/crates/egg")
                .with_benchmark(BenchmarkDelta::qualitative(
                    "extraction_control",
                    "Strong saturation, but extraction cost-model and budget control are less granular than needed.",
                ))
                .posture(
                    DeterminismRisk::Low,
                    UnsafePolicyFit::Clean,
                    LicensePosture::Permissive,
                    PortabilityPosture::Portable,
                )
                .rationale(
                    "Excellent library, but its E-node model would require adapting all codegen IR \
                     into egg's Language trait, and we need explicit node-budget fallback the in-tree \
                     optimizer already provides (shared with ftui-layout::egraph).",
                ),
        )
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::Performance,
            "Codegen graphs are small; saturation is not the bottleneck, integration overhead would be.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::Determinism,
            "We need a fixed, budgeted saturation schedule with deterministic extraction tie-breaks.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::UnsafePolicy,
            "egg is unsafe-clean, so this axis is not a blocker; in-tree code stays unsafe-free too.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::License,
            "egg is permissive; no license blocker, decision is integration-cost-driven.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::Portability,
            "egg is portable; the decision rests on reusing the existing in-tree e-graph design.",
        ))
        .fallback(
            "If saturation exceeds the node budget, fall back to the untransformed codegen output \
             and flag a missed-optimization note.",
        )
}

fn abstract_interpretation_record() -> EcosystemScanRecord {
    EcosystemScanRecord::new(
        UpliftCandidate::AbstractInterpretation,
        AdoptionDecision::Build,
    )
    .summary(
        "Build the Galois-linked over-approximation layer in-tree; the only mature abstract-domain \
         libraries are native (Apron/ELINA) and rejected on portability/unsafe grounds.",
    )
    .with_crate(
        CrateCandidate::new("elina", CrateVerdict::Rejected)
            .source("https://crates.io/crates/elina")
            .posture(
                DeterminismRisk::Medium,
                UnsafePolicyFit::Violates,
                LicensePosture::WeakCopyleft,
                PortabilityPosture::RequiresNativeToolchain,
            )
            .rationale(
                "Rust binding over the native ELINA/Apron C library; requires unsafe FFI, a native \
                 build, and carries LGPL terms — all three conflict with workspace policy.",
            ),
    )
    .none_found("No pure-Rust, unsafe-free general-purpose abstract-interpretation domain library exists; \
         native Apron/ELINA bindings are the only mature option and are policy-incompatible.")
    .with_reject_rationale(ConstraintRationale::new(
        ConstraintAxis::Performance,
        "Migration-time analysis runs offline; bespoke interval/sign domains are fast enough.",
    ))
    .with_reject_rationale(ConstraintRationale::new(
        ConstraintAxis::Determinism,
        "Galois connections and widening must be deterministic; in-tree domains guarantee fixed iteration order.",
    ))
    .with_reject_rationale(ConstraintRationale::new(
        ConstraintAxis::UnsafePolicy,
        "Apron/ELINA bindings require unsafe FFI, violating #![forbid(unsafe_code)].",
    ))
    .with_reject_rationale(ConstraintRationale::new(
        ConstraintAxis::License,
        "ELINA is LGPL; a copyleft native dependency is unacceptable for the workspace.",
    ))
    .with_reject_rationale(ConstraintRationale::new(
        ConstraintAxis::Portability,
        "Native Apron toolchain is absent from the pure-cargo build and CI.",
    ))
    .fallback(
        "If a construct falls outside the supported abstract domains, widen to Top and emit a \
         conservative certification gap rather than an unsound result.",
    )
}

fn concolic_record() -> EcosystemScanRecord {
    EcosystemScanRecord::new(UpliftCandidate::ConcolicDifferential, AdoptionDecision::Build)
        .summary(
            "Build a source-level concolic differential explorer in-tree; existing Rust symbolic \
             executors operate on LLVM IR and are rejected as the wrong abstraction.",
        )
        .with_crate(
            CrateCandidate::new("haybale", CrateVerdict::Rejected)
                .source("https://crates.io/crates/haybale")
                .posture(
                    DeterminismRisk::Medium,
                    UnsafePolicyFit::Violates,
                    LicensePosture::Permissive,
                    PortabilityPosture::RequiresNativeToolchain,
                )
                .rationale(
                    "Symbolic-executes LLVM bitcode via the Boolector SMT solver; wrong abstraction \
                     level (we compare TSX source vs generated Rust) and pulls native solver + LLVM.",
                ),
        )
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::Performance,
            "We only need bounded divergence probes over translated fragments, not full LLVM symbolic execution.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::Determinism,
            "Underlying SMT/LLVM path enumeration is not reproducible across versions.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::UnsafePolicy,
            "LLVM + native solver dependencies require unsafe FFI, violating workspace policy.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::License,
            "Permissive licenses, but the native LLVM/Boolector toolchain widens supply-chain risk.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::Portability,
            "Requires LLVM + native solver toolchain unavailable in the pure-cargo build.",
        ))
        .fallback(
            "If concolic exploration cannot bound a divergence, fall back to seeded differential \
             fuzzing over the metamorphic oracle.",
        )
}

fn metamorphic_record() -> EcosystemScanRecord {
    EcosystemScanRecord::new(UpliftCandidate::MetamorphicOracle, AdoptionDecision::Hybrid)
        .summary(
            "Adopt proptest as the deterministic input-generation substrate; build the \
             metamorphic-relation library (equivalence under permutation/idempotence/refinement) in-tree.",
        )
        .adopt_crate("proptest")
        .with_crate(
            CrateCandidate::new("proptest", CrateVerdict::Selected)
                .version("1.7")
                .source("https://crates.io/crates/proptest")
                .posture(
                    DeterminismRisk::Low,
                    UnsafePolicyFit::Contained,
                    LicensePosture::Permissive,
                    PortabilityPosture::Portable,
                )
                .rationale(
                    "Seeded, shrinking property generation is deterministic under a fixed seed and \
                     already a workspace dev-dependency; ideal generation substrate.",
                ),
        )
        .with_crate(
            CrateCandidate::new("quickcheck", CrateVerdict::Rejected)
                .source("https://crates.io/crates/quickcheck")
                .posture(
                    DeterminismRisk::Medium,
                    UnsafePolicyFit::Clean,
                    LicensePosture::Permissive,
                    PortabilityPosture::Portable,
                )
                .rationale("Weaker shrinking and seed control than proptest; no metamorphic relations either."),
        )
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::Performance,
            "Relation checks are cheap; generation cost dominates and proptest handles it.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::Determinism,
            "proptest seeds make failing cases replayable; the relation library adds no nondeterminism.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::UnsafePolicy,
            "proptest's unsafe is contained; the relation library is unsafe-free.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::License,
            "proptest is permissive (MIT/Apache); no IP risk.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::Portability,
            "proptest is pure Rust; no native dependency.",
        ))
        .fallback(
            "If a relation cannot be expressed as a metamorphic transform, fall back to fixed \
             golden-output differential checks.",
        )
}

fn shadow_run_record() -> EcosystemScanRecord {
    EcosystemScanRecord::new(UpliftCandidate::ShadowRun, AdoptionDecision::Build)
        .summary(
            "Build the dual-execution shadow-run harness in-tree over the migration's own run model; \
             shadow execution is an architecture pattern, not a publishable crate.",
        )
        .none_found(
            "No general-purpose Rust crate provides dual-execution shadow comparison over an \
             arbitrary deterministic run model; this is an architectural pattern realized in-tree.",
        )
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::Performance,
            "Shadow runs are gated and budgeted; in-tree control avoids unnecessary dual execution.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::Determinism,
            "Frame/output checksums must match bit-for-bit; only an in-tree harness over our buffers can guarantee this.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::UnsafePolicy,
            "Harness is pure Rust over existing deterministic execution paths; no unsafe.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::License,
            "No external dependency, so no license exposure.",
        ))
        .with_reject_rationale(ConstraintRationale::new(
            ConstraintAxis::Portability,
            "Pure Rust; runs anywhere the workspace runs.",
        ))
        .fallback(
            "If shadow comparison cannot be completed within budget, fall back to recording a \
             single-path run plus a deferred-comparison evidence note.",
        )
}

fn controller_guardrails_record() -> EcosystemScanRecord {
    EcosystemScanRecord::new(
        UpliftCandidate::ControllerInterferenceGuardrails,
        AdoptionDecision::Build,
    )
    .summary(
        "Build controller-composition interference and timescale-separation guardrails in-tree; \
         Rust control-theory crates target numeric simulation, not discrete UI controller composition.",
    )
    .with_crate(
        CrateCandidate::new("control_systems", CrateVerdict::Rejected)
            .source("https://crates.io/crates/control_systems")
            .posture(
                DeterminismRisk::Low,
                UnsafePolicyFit::Clean,
                LicensePosture::Permissive,
                PortabilityPosture::Portable,
            )
            .rationale(
                "Continuous-time transfer-function/state-space simulation; does not model discrete \
                 UI controller interaction, interference matrices, or timescale-separation guardrails.",
            ),
    )
    .with_reject_rationale(ConstraintRationale::new(
        ConstraintAxis::Performance,
        "Interference checks run at composition time and are cheap; no need for a numeric simulation engine.",
    ))
    .with_reject_rationale(ConstraintRationale::new(
        ConstraintAxis::Determinism,
        "Timescale-separation guardrails must produce identical verdicts across runs; in-tree logic ensures it.",
    ))
    .with_reject_rationale(ConstraintRationale::new(
        ConstraintAxis::UnsafePolicy,
        "Guardrail logic is pure Rust; no unsafe required.",
    ))
    .with_reject_rationale(ConstraintRationale::new(
        ConstraintAxis::License,
        "Candidate crate is permissive; rejection is domain-fit-driven, not license-driven.",
    ))
    .with_reject_rationale(ConstraintRationale::new(
        ConstraintAxis::Portability,
        "Candidate is portable; the guardrails stay pure Rust regardless.",
    ))
    .fallback(
        "If two controllers cannot be proven non-interfering, fall back to serializing them under a \
         single-owner lock and flag the composition as degraded.",
    )
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn summarize(
    records: &[EcosystemScanRecord],
    violations: &[ScanViolation],
    missing_candidate_count: usize,
) -> EcosystemScanSummary {
    let mut blocking_violation_count = 0;
    let mut advisory_violation_count = 0;
    for violation in violations {
        match violation.severity {
            ScanSeverity::Block => blocking_violation_count += 1,
            ScanSeverity::Warn => advisory_violation_count += 1,
        }
    }

    let blocked_records = records
        .iter()
        .filter(|record| {
            violations
                .iter()
                .any(|v| v.severity == ScanSeverity::Block && v.candidate_id == record.candidate_id)
        })
        .count();

    let mut adopt_count = 0;
    let mut build_count = 0;
    let mut hybrid_count = 0;
    let mut deferred_count = 0;
    for record in records {
        match record.decision {
            AdoptionDecision::Adopt => adopt_count += 1,
            AdoptionDecision::Build => build_count += 1,
            AdoptionDecision::Hybrid => hybrid_count += 1,
            AdoptionDecision::Deferred => deferred_count += 1,
        }
    }

    EcosystemScanSummary {
        total_records: records.len(),
        blocked_records,
        blocking_violation_count,
        advisory_violation_count,
        missing_candidate_count,
        adopt_count,
        build_count,
        hybrid_count,
        deferred_count,
    }
}

fn export_json_stats(
    report_id: &str,
    config: &EcosystemScanConfig,
    records: &[EcosystemScanRecord],
    violations: &[ScanViolation],
    summary: &EcosystemScanSummary,
) -> EcosystemScanJsonStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        config: &'a EcosystemScanConfig,
        summary: &'a EcosystemScanSummary,
        records: &'a [EcosystemScanRecord],
        violations: &'a [ScanViolation],
    }

    let payload = Export {
        schema_version: ECOSYSTEM_SCAN_SCHEMA_VERSION,
        report_id,
        config,
        summary,
        records,
        violations,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    EcosystemScanJsonStatsArtifact {
        path: format!("{report_id}/ecosystem_scan_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

fn report_id_for(config: &EcosystemScanConfig, records: &[EcosystemScanRecord]) -> String {
    #[derive(Serialize)]
    struct IdInput<'a> {
        config: &'a EcosystemScanConfig,
        records: &'a [EcosystemScanRecord],
    }
    let hash = stable_hash(&IdInput { config, records });
    format!("ecosystem-scan-{}", short_hash(&hash))
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

    fn clean_build_record(candidate: UpliftCandidate) -> EcosystemScanRecord {
        let mut record = EcosystemScanRecord::new(candidate, AdoptionDecision::Build)
            .summary("built in-tree")
            .with_crate(
                CrateCandidate::new("some-crate", CrateVerdict::Rejected)
                    .posture(
                        DeterminismRisk::None,
                        UnsafePolicyFit::Clean,
                        LicensePosture::Permissive,
                        PortabilityPosture::Portable,
                    )
                    .rationale("does not fit the domain"),
            )
            .fallback("conservative fallback");
        for axis in ConstraintAxis::ALL.iter().copied() {
            record = record.with_reject_rationale(ConstraintRationale::new(
                axis,
                "constraint-grounded reason",
            ));
        }
        record
    }

    fn full_clean_ledger() -> Vec<EcosystemScanRecord> {
        UpliftCandidate::ALL
            .iter()
            .copied()
            .map(clean_build_record)
            .collect()
    }

    #[test]
    fn candidate_metadata_is_consistent() {
        for candidate in UpliftCandidate::ALL.iter().copied() {
            assert!(!candidate.name().is_empty());
            assert!(!candidate.candidate_id().is_empty());
            assert!(!candidate.in_tree_module().is_empty());
            // candidate_id is kebab-case and unique-shaped.
            assert!(
                candidate
                    .candidate_id()
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '-')
            );
        }
        // All candidate ids are distinct.
        let mut ids: Vec<&str> = UpliftCandidate::ALL
            .iter()
            .map(|c| c.candidate_id())
            .collect();
        ids.sort_unstable();
        let unique = ids.len();
        ids.dedup();
        assert_eq!(unique, ids.len());
    }

    #[test]
    fn canonical_ledger_covers_every_candidate() {
        let ledger = canonical_ledger();
        assert_eq!(ledger.len(), UpliftCandidate::ALL.len());
        for candidate in UpliftCandidate::ALL.iter().copied() {
            assert!(
                ledger.iter().any(|r| r.candidate == candidate),
                "missing candidate {candidate:?}"
            );
        }
    }

    #[test]
    fn canonical_ledger_passes_the_default_gate() {
        let report = canonical_scan_report();
        assert!(
            report.scan_gate_passes,
            "canonical ledger should pass; violations: {:?}",
            report.violations
        );
        assert!(report.coverage_complete);
        assert!(report.blocked_candidate_ids.is_empty());
        assert!(report.serves_quality_artifact());
        assert!(report.missing_candidates().is_empty());
    }

    #[test]
    fn canonical_records_satisfy_acceptance_criteria() {
        for record in canonical_ledger() {
            // AC#1: at least one considered crate OR an explicit none-found statement.
            assert!(
                record.has_considered_crate()
                    || record
                        .none_found_statement
                        .as_ref()
                        .is_some_and(|s| !s.trim().is_empty()),
                "{} lacks crate coverage",
                record.candidate_id
            );
            // AC#2: reject decisions carry constraint-grounded rationale + fallback.
            if record.decision.rejects_full_adoption() {
                let axes = record.covered_axes();
                for axis in ConstraintAxis::ALL {
                    assert!(
                        axes.contains(axis),
                        "{} missing axis {axis:?}",
                        record.candidate_id
                    );
                }
                assert!(
                    record
                        .fallback_plan
                        .as_ref()
                        .is_some_and(|p| !p.trim().is_empty()),
                    "{} missing fallback",
                    record.candidate_id
                );
            }
        }
    }

    #[test]
    fn missing_coverage_blocks_the_gate() {
        let mut ledger = full_clean_ledger();
        ledger.pop();
        let report = EcosystemScanGate::default().evaluate(ledger);
        assert!(!report.scan_gate_passes);
        assert!(!report.coverage_complete);
        assert_eq!(report.summary.missing_candidate_count, 1);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.code == ScanViolationCode::MissingCandidateCoverage)
        );
        assert!(!report.serves_quality_artifact());
    }

    #[test]
    fn coverage_can_be_disabled() {
        let config = EcosystemScanConfig::default().with_full_coverage(false);
        let report = EcosystemScanGate::new(config)
            .evaluate(vec![clean_build_record(UpliftCandidate::ShadowRun)]);
        // No coverage violation, but coverage_complete reflects reality.
        assert!(
            !report
                .violations
                .iter()
                .any(|v| v.code == ScanViolationCode::MissingCandidateCoverage)
        );
        assert!(report.scan_gate_passes);
    }

    #[test]
    fn empty_decision_summary_is_caught() {
        let mut record = clean_build_record(UpliftCandidate::ShadowRun);
        record.decision_summary = String::new();
        let config = EcosystemScanConfig::default().with_full_coverage(false);
        let report = EcosystemScanGate::new(config).evaluate(vec![record]);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.code == ScanViolationCode::EmptyDecisionSummary)
        );
    }

    #[test]
    fn no_crate_and_no_none_found_is_blocked() {
        let mut record =
            EcosystemScanRecord::new(UpliftCandidate::ShadowRun, AdoptionDecision::Build)
                .summary("built");
        for axis in ConstraintAxis::ALL.iter().copied() {
            record = record.with_reject_rationale(ConstraintRationale::new(axis, "reason"));
        }
        record = record.fallback("fallback");
        let config = EcosystemScanConfig::default().with_full_coverage(false);
        let report = EcosystemScanGate::new(config).evaluate(vec![record]);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.code == ScanViolationCode::NoCrateConsideredWithoutNoneFound)
        );
    }

    #[test]
    fn empty_none_found_statement_is_blocked() {
        let mut record = clean_build_record(UpliftCandidate::ShadowRun);
        record.considered_crates.clear();
        record.none_found_statement = Some("   ".to_string());
        let config = EcosystemScanConfig::default().with_full_coverage(false);
        let report = EcosystemScanGate::new(config).evaluate(vec![record]);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.code == ScanViolationCode::NoneFoundStatementEmpty)
        );
    }

    #[test]
    fn reject_decision_missing_axis_is_blocked() {
        let mut record = clean_build_record(UpliftCandidate::ShadowRun);
        // Drop the determinism axis.
        record
            .reject_rationale
            .retain(|r| r.axis != ConstraintAxis::Determinism);
        let config = EcosystemScanConfig::default().with_full_coverage(false);
        let report = EcosystemScanGate::new(config).evaluate(vec![record]);
        assert!(report.violations.iter().any(|v| {
            v.code == ScanViolationCode::MissingRequiredConstraintAxis
                && v.detail.contains("determinism")
        }));
    }

    #[test]
    fn reject_decision_missing_fallback_is_blocked() {
        let mut record = clean_build_record(UpliftCandidate::ShadowRun);
        record.fallback_plan = None;
        let config = EcosystemScanConfig::default().with_full_coverage(false);
        let report = EcosystemScanGate::new(config).evaluate(vec![record]);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.code == ScanViolationCode::MissingFallbackPlan)
        );
    }

    #[test]
    fn build_decision_requires_in_tree_module() {
        let mut record = clean_build_record(UpliftCandidate::ShadowRun);
        record.in_tree_module = None;
        let config = EcosystemScanConfig::default().with_full_coverage(false);
        let report = EcosystemScanGate::new(config).evaluate(vec![record]);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.code == ScanViolationCode::MissingInTreeModule)
        );
    }

    #[test]
    fn rejected_crate_without_rationale_is_blocked() {
        let mut record = clean_build_record(UpliftCandidate::ShadowRun);
        record.considered_crates[0].rationale = String::new();
        let config = EcosystemScanConfig::default().with_full_coverage(false);
        let report = EcosystemScanGate::new(config).evaluate(vec![record]);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.code == ScanViolationCode::RejectedCrateMissingRationale)
        );
    }

    #[test]
    fn selected_crate_contradicting_build_is_blocked() {
        let mut record = clean_build_record(UpliftCandidate::ShadowRun);
        record.considered_crates[0].verdict = CrateVerdict::Selected;
        let config = EcosystemScanConfig::default().with_full_coverage(false);
        let report = EcosystemScanGate::new(config).evaluate(vec![record]);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.code == ScanViolationCode::SelectedCrateContradictsBuild)
        );
    }

    #[test]
    fn adopting_unsafe_violating_crate_is_blocked() {
        let record =
            EcosystemScanRecord::new(UpliftCandidate::ContractsAsCode, AdoptionDecision::Adopt)
                .summary("adopt")
                .adopt_crate("bad-crate")
                .with_crate(
                    CrateCandidate::new("bad-crate", CrateVerdict::Selected).posture(
                        DeterminismRisk::None,
                        UnsafePolicyFit::Violates,
                        LicensePosture::Permissive,
                        PortabilityPosture::Portable,
                    ),
                );
        let config = EcosystemScanConfig::default().with_full_coverage(false);
        let report = EcosystemScanGate::new(config).evaluate(vec![record]);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.code == ScanViolationCode::UnsafePolicyViolationAdopted)
        );
    }

    #[test]
    fn adopting_high_determinism_risk_crate_is_blocked() {
        let record =
            EcosystemScanRecord::new(UpliftCandidate::ContractsAsCode, AdoptionDecision::Adopt)
                .summary("adopt")
                .adopt_crate("risky")
                .with_crate(
                    CrateCandidate::new("risky", CrateVerdict::Selected).posture(
                        DeterminismRisk::High,
                        UnsafePolicyFit::Clean,
                        LicensePosture::Permissive,
                        PortabilityPosture::Portable,
                    ),
                );
        let config = EcosystemScanConfig::default().with_full_coverage(false);
        let report = EcosystemScanGate::new(config).evaluate(vec![record]);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.code == ScanViolationCode::AdoptedCrateDeterminismRiskTooHigh)
        );
    }

    #[test]
    fn adopt_without_adopted_crate_is_blocked() {
        let record =
            EcosystemScanRecord::new(UpliftCandidate::ContractsAsCode, AdoptionDecision::Adopt)
                .summary("adopt but no crate named");
        let config = EcosystemScanConfig::default().with_full_coverage(false);
        let report = EcosystemScanGate::new(config).evaluate(vec![record]);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.code == ScanViolationCode::AdoptWithoutAdoptedCrate)
        );
    }

    #[test]
    fn candidate_id_mismatch_is_blocked() {
        let mut record = clean_build_record(UpliftCandidate::ShadowRun);
        record.candidate_id = "wrong-id".to_string();
        let config = EcosystemScanConfig::default().with_full_coverage(false);
        let report = EcosystemScanGate::new(config).evaluate(vec![record]);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.code == ScanViolationCode::CandidateIdMismatch)
        );
    }

    #[test]
    fn duplicate_candidate_is_blocked() {
        let report = EcosystemScanGate::default().evaluate(vec![
            clean_build_record(UpliftCandidate::ShadowRun),
            clean_build_record(UpliftCandidate::ShadowRun),
        ]);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.code == ScanViolationCode::DuplicateCandidate)
        );
    }

    #[test]
    fn report_is_deterministic() {
        let first = EcosystemScanGate::default().evaluate(full_clean_ledger());
        let second = EcosystemScanGate::default().evaluate(full_clean_ledger());
        assert_eq!(first.report_id, second.report_id);
        assert_eq!(
            first.exported_json_stats.sha256,
            second.exported_json_stats.sha256
        );
        // Canonical ledger is also stable across calls.
        assert_eq!(
            canonical_scan_report().report_id,
            canonical_scan_report().report_id
        );
    }

    #[test]
    fn different_inputs_produce_different_report_ids() {
        let full = EcosystemScanGate::default().evaluate(full_clean_ledger());
        let mut trimmed = full_clean_ledger();
        trimmed.pop();
        let partial = EcosystemScanGate::default().evaluate(trimmed);
        assert_ne!(full.report_id, partial.report_id);
    }

    #[test]
    fn report_roundtrips_through_serde() {
        let report = canonical_scan_report();
        let json = serde_json::to_string(&report).unwrap();
        let restored: EcosystemScanReport = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.report_id, report.report_id);
        assert_eq!(restored.schema_version, report.schema_version);
        assert_eq!(restored.scan_gate_passes, report.scan_gate_passes);
        assert_eq!(restored.records, report.records);
        assert_eq!(restored.violations, report.violations);
    }

    #[test]
    fn json_stats_artifact_is_self_consistent() {
        let report = canonical_scan_report();
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
        assert!(
            report
                .exported_json_stats
                .path
                .ends_with("ecosystem_scan_stats.json")
        );
    }

    #[test]
    fn replay_command_references_module() {
        let report = canonical_scan_report();
        assert!(report.replay_command.contains("ecosystem-scan"));
        assert!(report.replay_command.contains(&report.report_id));
    }

    #[test]
    fn blocking_remediations_are_actionable() {
        let mut ledger = full_clean_ledger();
        ledger.pop();
        let report = EcosystemScanGate::default().evaluate(ledger);
        let remediations = report.blocking_remediations();
        assert!(!remediations.is_empty());
        assert!(remediations.iter().all(|r| !r.trim().is_empty()));
    }

    #[test]
    fn summary_counts_decisions() {
        let report = canonical_scan_report();
        let s = &report.summary;
        assert_eq!(
            s.adopt_count + s.build_count + s.hybrid_count + s.deferred_count,
            UpliftCandidate::ALL.len()
        );
        assert_eq!(s.total_records, UpliftCandidate::ALL.len());
        // Canonical ledger has two hybrids (contracts-as-code, metamorphic).
        assert_eq!(s.hybrid_count, 2);
    }

    #[test]
    fn config_roundtrips_and_required_axes_dedup() {
        let config = EcosystemScanConfig::default().with_required_axes([
            ConstraintAxis::Determinism,
            ConstraintAxis::Determinism,
            ConstraintAxis::License,
        ]);
        assert_eq!(config.required_constraint_axes.len(), 2);
        let json = serde_json::to_string(&config).unwrap();
        let restored: EcosystemScanConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, config);
    }
}
