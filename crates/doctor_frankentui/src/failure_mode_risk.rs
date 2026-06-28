// SPDX-License-Identifier: Apache-2.0
//! Failure-mode risk assessment matrix and countermeasure enforcement gate.
//!
//! The alien-graveyard taxonomy catalogues *why good ideas stay buried*: not
//! because the math is wrong, but because of eight recurring failure modes that
//! kill adoption regardless of technical merit. This module turns that taxonomy
//! into a **mandatory risk gate** that every candidate primitive, composition
//! plan, and recommendation card must clear before promotion.
//!
//! # The eight failure-mode classes
//!
//! [`FailureModeClass`] enumerates the graveyard taxonomy:
//!
//! | class | the graveyard pattern |
//! | --- | --- |
//! | [`FailureModeClass::WholeStackBarrier`] | benefit requires rewriting the whole stack (all-or-nothing adoption) |
//! | [`FailureModeClass::InstalledBaseInertia`] | the installed base and its tooling will not switch |
//! | [`FailureModeClass::ErgonomicsCliff`] | correct use is too hard; the API is a foot-gun |
//! | [`FailureModeClass::Unpredictability`] | worst-case behavior is unbounded or nondeterministic |
//! | [`FailureModeClass::OperabilityGaps`] | cannot be observed, debugged, or operated in production |
//! | [`FailureModeClass::HiddenConstants`] | great asymptotics, ruinous constant factors / hidden cost |
//! | [`FailureModeClass::IncentiveMismatch`] | nobody is rewarded for adopting it |
//! | [`FailureModeClass::LegalIpDrag`] | patent / license / IP encumbrance |
//!
//! These classes are the *taxonomy* axis; the *severity* axis reuses the shared
//! [`RiskClass`] scale (`Low`/`Medium`/`High`/`Critical`) so this gate composes
//! with the composition-matrix dangerous-combination gate
//! ([`crate::composition_matrix`]) and the recommendation-contract linter
//! ([`crate::recommendation_contract`]) instead of inventing a parallel scale.
//!
//! # What the gate enforces
//!
//! A [`RiskMatrix`] records one [`RiskAssessment`] per relevant class for a
//! single entry, in a chosen [`RolloutPhase`]. [`RiskGate`] then enforces three
//! acceptance criteria deterministically:
//!
//! - **AC1 — countermeasure completeness.** No entry passes without a *primary*
//!   risk flagged, and every primary (or otherwise high-severity) class must
//!   carry both an explicit `countermeasure` and an explicit `fallback`.
//! - **AC2 — severity-aware escalation.** `High`/`Critical` classes require
//!   additional proof artifacts before promotion; the requirement is
//!   phase-gated (advisory in [`RolloutPhase::Shadow`], blocking from
//!   [`RolloutPhase::Canary`] onward), [`FailureModeClass::LegalIpDrag`] blocks
//!   in every phase, and a *dangerous combination* of co-occurring high-severity
//!   classes escalates to a hard block until every class in the combination is
//!   proven.
//! - **AC3 — auditable logs.** Every assessed `(entry, class)` pair emits a
//!   [`RiskGateLogRecord`] carrying `entry_id`, `risk_class`,
//!   `severity`, `countermeasure_status`, and the per-class `gate_outcome`,
//!   renderable as JSONL via [`RiskGateReport::render_log_jsonl`].
//!
//! # Determinism
//!
//! All ids and checksums are SHA-256 of a canonical serialization; assessments
//! are sorted by class before hashing or rendering, and there are no clocks or
//! randomness. There are no floating-point fields anywhere in this module, so
//! every artifact derives `Eq` and round-trips byte-identically across reruns.
//!
//! # Presentation-only
//!
//! The gate *reads* candidate / contract artifacts and *reports*; it never
//! mutates an input or returns a migration decision. It decides only whether an
//! entry is admissible, leaving the decision itself to
//! [`crate::decision_loss_policy`] and the rollout machinery.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::adversarial_fixtures::RiskClass;
use crate::recommendation_contract::{ContractSeverity, RecommendationContract};
use crate::semantic_contract::IpArtifactStatus;

// ── Schema version & constants ───────────────────────────────────────────────

/// Schema version for failure-mode risk-gate artifacts.
pub const FAILURE_MODE_RISK_SCHEMA_VERSION: &str = "failure-mode-risk-v1";

// ── Errors ───────────────────────────────────────────────────────────────────

/// Errors raised while assembling a [`RiskMatrix`].
///
/// These signal a *structurally malformed* matrix (a programming/data fault) —
/// an empty entry id, no assessments, or two assessments for the same class.
/// A risk that is merely *unmitigated* is never an error: it is reported as a
/// blocking [`RiskGateFinding`] by [`RiskGate::evaluate`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FailureModeRiskError {
    /// A required field was empty or a structural invariant was violated.
    #[error("invalid failure-mode risk input: {detail}")]
    InvalidInput {
        /// Human-readable explanation.
        detail: String,
    },
}

/// Convenience result alias for this module.
type Result<T> = std::result::Result<T, FailureModeRiskError>;

// ── Taxonomy: failure-mode classes ───────────────────────────────────────────

/// One of the eight alien-graveyard failure-mode classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureModeClass {
    /// Benefit requires rewriting the whole stack (all-or-nothing adoption).
    WholeStackBarrier,
    /// The installed base and its tooling will not switch.
    InstalledBaseInertia,
    /// Correct use is too hard; the surface is a foot-gun.
    ErgonomicsCliff,
    /// Worst-case behavior is unbounded or nondeterministic.
    Unpredictability,
    /// Cannot be observed, debugged, or operated in production.
    OperabilityGaps,
    /// Great asymptotics, ruinous constant factors / hidden cost.
    HiddenConstants,
    /// Nobody is rewarded for adopting it.
    IncentiveMismatch,
    /// Patent / license / IP encumbrance.
    LegalIpDrag,
}

impl FailureModeClass {
    /// All classes in canonical (declaration) order.
    pub const ALL: [FailureModeClass; 8] = [
        FailureModeClass::WholeStackBarrier,
        FailureModeClass::InstalledBaseInertia,
        FailureModeClass::ErgonomicsCliff,
        FailureModeClass::Unpredictability,
        FailureModeClass::OperabilityGaps,
        FailureModeClass::HiddenConstants,
        FailureModeClass::IncentiveMismatch,
        FailureModeClass::LegalIpDrag,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WholeStackBarrier => "whole_stack_barrier",
            Self::InstalledBaseInertia => "installed_base_inertia",
            Self::ErgonomicsCliff => "ergonomics_cliff",
            Self::Unpredictability => "unpredictability",
            Self::OperabilityGaps => "operability_gaps",
            Self::HiddenConstants => "hidden_constants",
            Self::IncentiveMismatch => "incentive_mismatch",
            Self::LegalIpDrag => "legal_ip_drag",
        }
    }

    /// Canonical ordering index (matches [`FailureModeClass::ALL`]).
    #[must_use]
    pub fn order_index(self) -> u8 {
        match self {
            Self::WholeStackBarrier => 0,
            Self::InstalledBaseInertia => 1,
            Self::ErgonomicsCliff => 2,
            Self::Unpredictability => 3,
            Self::OperabilityGaps => 4,
            Self::HiddenConstants => 5,
            Self::IncentiveMismatch => 6,
            Self::LegalIpDrag => 7,
        }
    }

    /// Stable clause-id prefix used in forensic finding ids.
    #[must_use]
    pub fn clause_prefix(self) -> &'static str {
        match self {
            Self::WholeStackBarrier => "FMR-WSB",
            Self::InstalledBaseInertia => "FMR-IBI",
            Self::ErgonomicsCliff => "FMR-ERG",
            Self::Unpredictability => "FMR-UNP",
            Self::OperabilityGaps => "FMR-OPS",
            Self::HiddenConstants => "FMR-CON",
            Self::IncentiveMismatch => "FMR-INC",
            Self::LegalIpDrag => "FMR-LEG",
        }
    }

    /// One-line description of the graveyard pattern.
    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            Self::WholeStackBarrier => {
                "benefit requires rewriting the whole stack (all-or-nothing adoption)"
            }
            Self::InstalledBaseInertia => "the installed base and its tooling will not switch",
            Self::ErgonomicsCliff => "correct use is too hard; the surface is a foot-gun",
            Self::Unpredictability => "worst-case behavior is unbounded or nondeterministic",
            Self::OperabilityGaps => "cannot be observed, debugged, or operated in production",
            Self::HiddenConstants => "great asymptotics, ruinous constant factors / hidden cost",
            Self::IncentiveMismatch => "nobody is rewarded for adopting it",
            Self::LegalIpDrag => "patent / license / IP encumbrance",
        }
    }

    /// Pointer to the subsystem / bead that typically supplies the countermeasure
    /// evidence for this class.
    #[must_use]
    pub fn dependency_hint(self) -> &'static str {
        match self {
            Self::WholeStackBarrier => {
                "adoption-wedge + flagship migration (bd-3bxhj.9.4 / .10.33)"
            }
            Self::InstalledBaseInertia => {
                "support matrix + compatibility commitment (bd-3bxhj.9.5)"
            }
            Self::ErgonomicsCliff => "ergonomics scorer + galaxy-brain cards (bd-3bxhj.10.26)",
            Self::Unpredictability => {
                "conformal/e-process/PAC-Bayes guarantee layer (bd-3bxhj.10.23)"
            }
            Self::OperabilityGaps => "evidence ledger + drift monitor (drift_monitor)",
            Self::HiddenConstants => {
                "opportunity-matrix scorer + benchmark pipeline (bd-3bxhj.8.16)"
            }
            Self::IncentiveMismatch => {
                "archetype-to-project map + decision contracts (bd-3bxhj.10.33)"
            }
            Self::LegalIpDrag => "paper-verification LEGAL/IP pack (bd-3bxhj.10.13)",
        }
    }
}

// ── Rollout phase (escalation axis) ──────────────────────────────────────────

/// Rollout phase an entry is being evaluated for.
///
/// Strictness increases monotonically with the variant order: the gate tolerates
/// more uncovered risk early (shadow) and demands full proof coverage by general
/// availability. This is the *escalation* axis for [`RiskGate`]; it is distinct
/// from the entry-lifecycle status on a recommendation card
/// ([`crate::recommendation_contract::EntryStatus`]) and from the runtime
/// rollout lanes documented in the README — it controls only *how hard* an
/// uncovered high-severity risk blocks promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RolloutPhase {
    /// Shadow comparison only; uncovered proof is advisory.
    Shadow,
    /// Limited canary exposure; uncovered high-severity proof blocks.
    Canary,
    /// Broadly enabled.
    Enabled,
    /// General availability; the strictest bar.
    GeneralAvailability,
}

impl RolloutPhase {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shadow => "shadow",
            Self::Canary => "canary",
            Self::Enabled => "enabled",
            Self::GeneralAvailability => "general_availability",
        }
    }
}

// ── Entry kind ───────────────────────────────────────────────────────────────

/// What the risk matrix is being computed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// A single candidate primitive.
    CandidatePrimitive,
    /// A composition plan combining several primitives.
    CompositionPlan,
    /// A compiled recommendation card / contract.
    RecommendationCard,
}

impl EntryKind {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CandidatePrimitive => "candidate_primitive",
            Self::CompositionPlan => "composition_plan",
            Self::RecommendationCard => "recommendation_card",
        }
    }
}

// ── Countermeasure status ────────────────────────────────────────────────────

/// Per-class countermeasure coverage, derived during gate evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CountermeasureStatus {
    /// Countermeasure, fallback, and (where required) proof are all present.
    Satisfied,
    /// The required countermeasure text is empty.
    MissingCountermeasure,
    /// The countermeasure is present but the explicit fallback is empty.
    MissingFallback,
    /// Countermeasure and fallback are present, but a high-severity class lacks
    /// the proof artifacts required at this phase.
    InsufficientProof,
}

impl CountermeasureStatus {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Satisfied => "satisfied",
            Self::MissingCountermeasure => "missing_countermeasure",
            Self::MissingFallback => "missing_fallback",
            Self::InsufficientProof => "insufficient_proof",
        }
    }
}

// ── Gate outcome ─────────────────────────────────────────────────────────────

/// Per-class (or overall) gate outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateOutcome {
    /// No findings for this class.
    Pass,
    /// Advisory finding only.
    Warn,
    /// At least one blocking finding.
    Block,
}

impl GateOutcome {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Block => "block",
        }
    }
}

// ── Risk assessment ──────────────────────────────────────────────────────────

/// A single failure-mode class assessed for one entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskAssessment {
    /// The failure-mode class.
    pub class: FailureModeClass,
    /// Severity on the shared [`RiskClass`] scale.
    pub severity: RiskClass,
    /// Whether this is the entry's dominant (primary) risk.
    pub is_primary: bool,
    /// Explicit mitigation for the risk.
    pub countermeasure: String,
    /// Explicit fallback strategy if the mitigation fails or is unavailable.
    pub fallback: String,
    /// References to proof artifacts (formal guarantee / repro / provenance ids),
    /// kept sorted and deduplicated.
    pub proof_artifact_refs: Vec<String>,
    /// Plain-English rationale for the severity assignment.
    pub rationale: String,
}

impl RiskAssessment {
    /// Build a bare assessment for `class` at `severity` with empty
    /// countermeasure/fallback/proofs (to be filled by the builder methods).
    #[must_use]
    pub fn new(class: FailureModeClass, severity: RiskClass) -> Self {
        Self {
            class,
            severity,
            is_primary: false,
            countermeasure: String::new(),
            fallback: String::new(),
            proof_artifact_refs: Vec::new(),
            rationale: String::new(),
        }
    }

    /// Flag this assessment as the entry's primary risk.
    #[must_use]
    pub fn primary(mut self) -> Self {
        self.is_primary = true;
        self
    }

    /// Attach the countermeasure text.
    #[must_use]
    pub fn with_countermeasure(mut self, countermeasure: impl Into<String>) -> Self {
        self.countermeasure = countermeasure.into();
        self
    }

    /// Attach the explicit fallback text.
    #[must_use]
    pub fn with_fallback(mut self, fallback: impl Into<String>) -> Self {
        self.fallback = fallback.into();
        self
    }

    /// Attach a proof-artifact reference, keeping the set sorted and deduplicated.
    #[must_use]
    pub fn with_proof_artifact(mut self, artifact_ref: impl Into<String>) -> Self {
        self.proof_artifact_refs.push(artifact_ref.into());
        self.proof_artifact_refs.sort();
        self.proof_artifact_refs.dedup();
        self
    }

    /// Attach the severity rationale.
    #[must_use]
    pub fn with_rationale(mut self, rationale: impl Into<String>) -> Self {
        self.rationale = rationale.into();
        self
    }

    /// Whether the severity is `High` or `Critical`.
    #[must_use]
    pub fn is_high_severity(&self) -> bool {
        self.severity >= RiskClass::High
    }
}

// ── Risk matrix ──────────────────────────────────────────────────────────────

/// The failure-mode risk matrix for a single entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskMatrix {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic, content-addressed matrix id.
    pub matrix_id: String,
    /// The entry this matrix assesses.
    pub entry_id: String,
    /// What kind of entry this is.
    pub entry_kind: EntryKind,
    /// The rollout phase the entry is being evaluated for.
    pub phase: RolloutPhase,
    /// Per-class assessments, sorted by [`FailureModeClass::order_index`].
    pub assessments: Vec<RiskAssessment>,
    /// The primary risk class (the highest-severity assessment flagged primary),
    /// or `None` if no assessment is flagged primary.
    pub primary_class: Option<FailureModeClass>,
}

impl RiskMatrix {
    /// Build a risk matrix from explicit assessments.
    ///
    /// # Errors
    ///
    /// Returns [`FailureModeRiskError::InvalidInput`] when `entry_id` is empty,
    /// `assessments` is empty, or two assessments share a class.
    pub fn new(
        entry_id: impl Into<String>,
        entry_kind: EntryKind,
        phase: RolloutPhase,
        assessments: impl IntoIterator<Item = RiskAssessment>,
    ) -> Result<Self> {
        let entry_id = entry_id.into();
        if entry_id.trim().is_empty() {
            return Err(FailureModeRiskError::InvalidInput {
                detail: "entry_id is empty".to_string(),
            });
        }

        let mut assessments: Vec<RiskAssessment> = assessments.into_iter().collect();
        if assessments.is_empty() {
            return Err(FailureModeRiskError::InvalidInput {
                detail: format!("entry {entry_id} has no risk assessments"),
            });
        }

        assessments.sort_by(|a, b| {
            a.class
                .order_index()
                .cmp(&b.class.order_index())
                .then_with(|| a.severity.cmp(&b.severity))
        });
        for window in assessments.windows(2) {
            if window[0].class == window[1].class {
                return Err(FailureModeRiskError::InvalidInput {
                    detail: format!(
                        "entry {entry_id} has duplicate assessment for class {}",
                        window[0].class.as_str()
                    ),
                });
            }
        }

        let primary_class = assessments
            .iter()
            .filter(|a| a.is_primary)
            .max_by(|a, b| {
                a.severity
                    .cmp(&b.severity)
                    .then_with(|| b.class.order_index().cmp(&a.class.order_index()))
            })
            .map(|a| a.class);

        let matrix_id = format!(
            "fmr-matrix-{}",
            short_hash(&stable_hash(&CanonicalMatrix {
                schema_version: FAILURE_MODE_RISK_SCHEMA_VERSION,
                entry_id: &entry_id,
                entry_kind,
                phase,
                assessments: &assessments,
                primary_class,
            }))
        );

        Ok(Self {
            schema_version: FAILURE_MODE_RISK_SCHEMA_VERSION.to_string(),
            matrix_id,
            entry_id,
            entry_kind,
            phase,
            assessments,
            primary_class,
        })
    }

    /// Build a recommendation-card risk matrix from a compiled contract.
    ///
    /// The contract's [`crate::recommendation_contract::FailureMode`] becomes the
    /// `primary_class` assessment (severity, countermeasure, repro/provenance
    /// proofs), the budgeted-mode conservative fallback becomes the explicit
    /// `fallback`, and the legal/IP status is folded into a
    /// [`FailureModeClass::LegalIpDrag`] assessment whenever that is not already
    /// the primary class. The caller names which of the eight classes the
    /// contract's primary failure mode belongs to (the contract records a
    /// severity, not a class).
    ///
    /// # Errors
    ///
    /// Propagates [`RiskMatrix::new`] validation errors.
    pub fn from_contract(
        contract: &RecommendationContract,
        primary_class: FailureModeClass,
        phase: RolloutPhase,
    ) -> Result<Self> {
        let fm = &contract.failure_mode;

        let mut primary = RiskAssessment::new(primary_class, fm.risk_class)
            .primary()
            .with_countermeasure(fm.countermeasure.clone())
            .with_fallback(contract.budgeted_mode.conservative_fallback_trigger.clone())
            .with_rationale(format!(
                "primary failure mode derived from recommendation contract {}",
                contract.card_id
            ));
        for artifact in [fm.repro_artifact.clone(), fm.provenance_artifact.clone()]
            .into_iter()
            .flatten()
        {
            primary = primary.with_proof_artifact(artifact);
        }

        let mut assessments = vec![primary];
        if primary_class != FailureModeClass::LegalIpDrag {
            let mut legal = RiskAssessment::new(
                FailureModeClass::LegalIpDrag,
                legal_status_severity(fm.legal_status),
            )
            .with_rationale(format!(
                "auto-derived from recommendation contract {} legal/IP status",
                contract.card_id
            ));
            if let Some(provenance) = fm.provenance_artifact.clone() {
                legal = legal.with_proof_artifact(provenance);
            }
            assessments.push(legal);
        }

        Self::new(
            contract.card_id.clone(),
            EntryKind::RecommendationCard,
            phase,
            assessments,
        )
    }

    /// The assessment for `class`, if present.
    #[must_use]
    pub fn assessment_for(&self, class: FailureModeClass) -> Option<&RiskAssessment> {
        self.assessments.iter().find(|a| a.class == class)
    }

    /// The primary-risk assessment, if any.
    #[must_use]
    pub fn primary_assessment(&self) -> Option<&RiskAssessment> {
        self.primary_class
            .and_then(|class| self.assessment_for(class))
    }

    /// High-severity (`High`/`Critical`) classes, in canonical order.
    #[must_use]
    pub fn high_severity_classes(&self) -> Vec<FailureModeClass> {
        self.assessments
            .iter()
            .filter(|a| a.is_high_severity())
            .map(|a| a.class)
            .collect()
    }
}

/// Canonical, id-free view of a matrix used for content-addressing.
#[derive(Serialize)]
struct CanonicalMatrix<'a> {
    schema_version: &'a str,
    entry_id: &'a str,
    entry_kind: EntryKind,
    phase: RolloutPhase,
    assessments: &'a [RiskAssessment],
    primary_class: Option<FailureModeClass>,
}

// ── Gate policy ──────────────────────────────────────────────────────────────

/// Tunable thresholds for the risk gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskGatePolicy {
    /// Severity at or above which a class must carry countermeasure + fallback
    /// even when it is not the primary risk.
    pub fallback_required_severity: RiskClass,
    /// Severity at or above which a class must carry proof artifacts.
    pub proof_required_severity: RiskClass,
    /// Phase at or beyond which missing proof on a high-severity class blocks
    /// (advisory before this phase). [`FailureModeClass::LegalIpDrag`] always
    /// blocks regardless of phase.
    pub proof_required_from_phase: RolloutPhase,
    /// Number of co-occurring high-severity classes that triggers
    /// dangerous-combination escalation.
    pub combination_escalation_threshold: usize,
}

impl Default for RiskGatePolicy {
    fn default() -> Self {
        Self {
            fallback_required_severity: RiskClass::Medium,
            proof_required_severity: RiskClass::High,
            proof_required_from_phase: RolloutPhase::Canary,
            combination_escalation_threshold: 2,
        }
    }
}

impl RiskGatePolicy {
    /// Override the severity at which countermeasure + fallback are required.
    #[must_use]
    pub fn with_fallback_required_severity(mut self, severity: RiskClass) -> Self {
        self.fallback_required_severity = severity;
        self
    }

    /// Override the severity at which proof artifacts are required.
    #[must_use]
    pub fn with_proof_required_severity(mut self, severity: RiskClass) -> Self {
        self.proof_required_severity = severity;
        self
    }

    /// Override the phase from which missing proof becomes blocking.
    #[must_use]
    pub fn with_proof_required_from_phase(mut self, phase: RolloutPhase) -> Self {
        self.proof_required_from_phase = phase;
        self
    }

    /// Override the dangerous-combination escalation threshold.
    #[must_use]
    pub fn with_combination_escalation_threshold(mut self, threshold: usize) -> Self {
        self.combination_escalation_threshold = threshold;
        self
    }
}

// ── Findings, logs, report ───────────────────────────────────────────────────

/// Machine-readable code for a risk-gate finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskGateCode {
    /// No assessment is flagged as the primary risk.
    MissingPrimaryRisk,
    /// A required class has no countermeasure.
    MissingCountermeasure,
    /// A required class has a countermeasure but no explicit fallback.
    MissingFallback,
    /// A high-severity class lacks proof artifacts required before promotion.
    HighSeverityMissingProof,
    /// A high-severity legal/IP-drag class is unresolved (blocks every phase).
    LegalIpDragUnresolved,
    /// Co-occurring high-severity classes need additional proof before promotion.
    DangerousCombinationEscalation,
}

impl RiskGateCode {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingPrimaryRisk => "missing_primary_risk",
            Self::MissingCountermeasure => "missing_countermeasure",
            Self::MissingFallback => "missing_fallback",
            Self::HighSeverityMissingProof => "high_severity_missing_proof",
            Self::LegalIpDragUnresolved => "legal_ip_drag_unresolved",
            Self::DangerousCombinationEscalation => "dangerous_combination_escalation",
        }
    }

    /// Stable clause identifier for forensics.
    #[must_use]
    pub fn clause_id(self) -> &'static str {
        match self {
            Self::MissingPrimaryRisk => "FMR-GATE-001",
            Self::MissingCountermeasure => "FMR-GATE-002",
            Self::MissingFallback => "FMR-GATE-003",
            Self::HighSeverityMissingProof => "FMR-GATE-004",
            Self::LegalIpDragUnresolved => "FMR-GATE-005",
            Self::DangerousCombinationEscalation => "FMR-GATE-006",
        }
    }
}

/// A structured risk-gate finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskGateFinding {
    /// Machine-readable code.
    pub code: RiskGateCode,
    /// Severity (`Block` fails the gate, `Warn` is advisory).
    pub severity: ContractSeverity,
    /// Stable clause id.
    pub clause_id: String,
    /// Entry this pertains to.
    pub entry_id: String,
    /// Failure-mode class, when class-scoped (`None` for matrix-level findings).
    pub class: Option<FailureModeClass>,
    /// Human-readable detail.
    pub detail: String,
    /// Actionable remediation clause.
    pub remediation: String,
}

impl RiskGateFinding {
    fn new(
        code: RiskGateCode,
        severity: ContractSeverity,
        entry_id: impl Into<String>,
        class: Option<FailureModeClass>,
        detail: impl Into<String>,
        remediation: impl Into<String>,
    ) -> Self {
        let entry_id = entry_id.into();
        // Class-scoped findings carry the class clause prefix for forensic grep;
        // matrix-level findings use the gate clause id.
        let clause_id = match class {
            Some(c) => format!("{}-{}", c.clause_prefix(), code.clause_id()),
            None => code.clause_id().to_string(),
        };
        Self {
            code,
            severity,
            clause_id,
            entry_id,
            class,
            detail: detail.into(),
            remediation: remediation.into(),
        }
    }
}

/// One auditable log record per assessed `(entry, class)` pair (AC3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskGateLogRecord {
    /// The entry id.
    pub entry_id: String,
    /// The failure-mode class.
    pub risk_class: FailureModeClass,
    /// Severity as a stable lowercase tag.
    pub severity: String,
    /// Countermeasure coverage status.
    pub countermeasure_status: CountermeasureStatus,
    /// The per-class gate outcome.
    pub gate_outcome: GateOutcome,
}

/// Aggregate counts for a risk-gate report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskGateSummary {
    /// Total entries evaluated.
    pub total_entries: usize,
    /// Entries with at least one blocking finding.
    pub blocked_entries: usize,
    /// Total blocking findings.
    pub blocking_finding_count: usize,
    /// Total advisory findings.
    pub advisory_finding_count: usize,
    /// Total high-severity classes across all entries.
    pub high_severity_class_count: usize,
}

/// Deterministic JSON-stats artifact (content + checksum).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskGateJsonStatsArtifact {
    /// Suggested relative output path.
    pub path: String,
    /// SHA-256 of `content`.
    pub sha256: String,
    /// Serialized JSON content.
    pub content: String,
}

/// Full failure-mode risk-gate report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskGateReport {
    /// Schema version constant.
    pub schema_version: String,
    /// Deterministic, content-addressed report id.
    pub report_id: String,
    /// Policy used.
    pub policy: RiskGatePolicy,
    /// Entry ids evaluated (input order preserved).
    pub entry_ids: Vec<String>,
    /// Findings (in evaluation order).
    pub findings: Vec<RiskGateFinding>,
    /// Per-`(entry, class)` log records (AC3).
    pub log_records: Vec<RiskGateLogRecord>,
    /// Aggregate summary.
    pub summary: RiskGateSummary,
    /// Whether the gate passes (no blocking findings).
    pub gate_passes: bool,
    /// Entry ids blocked by the gate (sorted, deduplicated).
    pub blocked_entry_ids: Vec<String>,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: RiskGateJsonStatsArtifact,
    /// Replay command for the report.
    pub replay_command: String,
}

impl RiskGateReport {
    /// Every blocking remediation clause, in report order.
    #[must_use]
    pub fn blocking_remediations(&self) -> Vec<String> {
        self.findings
            .iter()
            .filter(|f| f.severity == ContractSeverity::Block)
            .map(|f| f.remediation.clone())
            .collect()
    }

    /// Findings for a specific entry.
    #[must_use]
    pub fn findings_for(&self, entry_id: &str) -> Vec<&RiskGateFinding> {
        self.findings
            .iter()
            .filter(|f| f.entry_id == entry_id)
            .collect()
    }

    /// Render the per-`(entry, class)` log records as JSONL (AC3).
    #[must_use]
    pub fn render_log_jsonl(&self) -> String {
        self.log_records
            .iter()
            .map(|record| serde_json::to_string(record).unwrap_or_else(|error| error.to_string()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Render a compact human-readable Markdown summary.
    #[must_use]
    pub fn render_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "# Failure-Mode Risk Gate — {}\n\n",
            self.report_id
        ));
        out.push_str(&format!(
            "- entries: {} ({} blocked)\n- gate: {}\n- blocking findings: {}\n- advisory findings: {}\n- high-severity classes: {}\n\n",
            self.summary.total_entries,
            self.summary.blocked_entries,
            if self.gate_passes { "PASS" } else { "BLOCK" },
            self.summary.blocking_finding_count,
            self.summary.advisory_finding_count,
            self.summary.high_severity_class_count,
        ));
        if self.findings.is_empty() {
            out.push_str("No findings.\n");
            return out;
        }
        out.push_str("| severity | code | entry | class | clause | detail |\n");
        out.push_str("| --- | --- | --- | --- | --- | --- |\n");
        for f in &self.findings {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                f.severity.as_str(),
                f.code.as_str(),
                f.entry_id,
                f.class.map_or("-", FailureModeClass::as_str),
                f.clause_id,
                f.detail,
            ));
        }
        out
    }
}

// ── The gate ─────────────────────────────────────────────────────────────────

/// Enforces the failure-mode risk policy over risk matrices.
#[derive(Debug, Clone, Default)]
pub struct RiskGate {
    policy: RiskGatePolicy,
}

impl RiskGate {
    /// Build a gate with an explicit policy.
    #[must_use]
    pub fn new(policy: RiskGatePolicy) -> Self {
        Self { policy }
    }

    /// The policy in force.
    #[must_use]
    pub fn policy(&self) -> &RiskGatePolicy {
        &self.policy
    }

    /// Evaluate a single risk matrix.
    #[must_use]
    pub fn evaluate(&self, matrix: &RiskMatrix) -> RiskGateReport {
        self.evaluate_all(std::slice::from_ref(matrix))
    }

    /// Evaluate a batch of risk matrices into one report.
    #[must_use]
    pub fn evaluate_all(&self, matrices: &[RiskMatrix]) -> RiskGateReport {
        let mut findings = Vec::new();
        let mut log_records = Vec::new();
        for matrix in matrices {
            self.assess_matrix(matrix, &mut findings, &mut log_records);
        }

        let entry_ids: Vec<String> = matrices.iter().map(|m| m.entry_id.clone()).collect();
        let blocked: BTreeSet<String> = findings
            .iter()
            .filter(|f| f.severity == ContractSeverity::Block)
            .map(|f| f.entry_id.clone())
            .collect();
        let blocking_finding_count = findings
            .iter()
            .filter(|f| f.severity == ContractSeverity::Block)
            .count();
        let advisory_finding_count = findings
            .iter()
            .filter(|f| f.severity == ContractSeverity::Warn)
            .count();
        let high_severity_class_count = matrices
            .iter()
            .map(|m| m.high_severity_classes().len())
            .sum();

        let summary = RiskGateSummary {
            total_entries: matrices.len(),
            blocked_entries: blocked.len(),
            blocking_finding_count,
            advisory_finding_count,
            high_severity_class_count,
        };
        let gate_passes = blocking_finding_count == 0;
        let blocked_entry_ids: Vec<String> = blocked.into_iter().collect();

        let report_id = format!(
            "fmr-report-{}",
            short_hash(&stable_hash(&CanonicalReport {
                schema_version: FAILURE_MODE_RISK_SCHEMA_VERSION,
                policy: &self.policy,
                entry_ids: &entry_ids,
                findings: &findings,
                log_records: &log_records,
                summary: &summary,
            }))
        );
        let exported_json_stats = export_json_stats(&report_id, matrices, &findings);
        let replay_command = format!(
            "cargo test -p doctor_frankentui --lib failure_mode_risk -- --nocapture # report {report_id}"
        );

        RiskGateReport {
            schema_version: FAILURE_MODE_RISK_SCHEMA_VERSION.to_string(),
            report_id,
            policy: self.policy.clone(),
            entry_ids,
            findings,
            log_records,
            summary,
            gate_passes,
            blocked_entry_ids,
            exported_json_stats,
            replay_command,
        }
    }

    #[allow(clippy::too_many_lines)]
    fn assess_matrix(
        &self,
        matrix: &RiskMatrix,
        findings: &mut Vec<RiskGateFinding>,
        log_records: &mut Vec<RiskGateLogRecord>,
    ) {
        let entry = matrix.entry_id.as_str();

        // AC1: an entry must declare a primary risk.
        if matrix.primary_class.is_none() {
            findings.push(RiskGateFinding::new(
                RiskGateCode::MissingPrimaryRisk,
                ContractSeverity::Block,
                entry,
                None,
                "no failure-mode assessment is flagged as the primary risk",
                "flag the dominant failure-mode class with `is_primary` and populate its countermeasure + fallback",
            ));
        }

        let high_classes = matrix.high_severity_classes();
        let combination = high_classes.len() >= self.policy.combination_escalation_threshold;

        for assessment in &matrix.assessments {
            let mut blocked = false;
            let mut warned = false;
            let mut status = CountermeasureStatus::Satisfied;

            // AC1: primary (or otherwise high-enough) risks need countermeasure + fallback.
            let needs_countermeasure = assessment.is_primary
                || assessment.severity >= self.policy.fallback_required_severity;
            if needs_countermeasure {
                if assessment.countermeasure.trim().is_empty() {
                    findings.push(RiskGateFinding::new(
                        RiskGateCode::MissingCountermeasure,
                        ContractSeverity::Block,
                        entry,
                        Some(assessment.class),
                        format!(
                            "{} risk ({}) has no countermeasure",
                            assessment.class.as_str(),
                            risk_class_tag(assessment.severity)
                        ),
                        format!(
                            "supply an explicit mitigation; see {}",
                            assessment.class.dependency_hint()
                        ),
                    ));
                    blocked = true;
                    status = CountermeasureStatus::MissingCountermeasure;
                } else if assessment.fallback.trim().is_empty() {
                    findings.push(RiskGateFinding::new(
                        RiskGateCode::MissingFallback,
                        ContractSeverity::Block,
                        entry,
                        Some(assessment.class),
                        format!(
                            "{} risk ({}) has a countermeasure but no explicit fallback",
                            assessment.class.as_str(),
                            risk_class_tag(assessment.severity)
                        ),
                        "declare the fallback strategy taken when the mitigation fails or is unavailable",
                    ));
                    blocked = true;
                    status = CountermeasureStatus::MissingFallback;
                }
            }

            // AC2: high-severity classes require proof artifacts before promotion.
            if assessment.severity >= self.policy.proof_required_severity
                && assessment.proof_artifact_refs.is_empty()
            {
                let legal = assessment.class == FailureModeClass::LegalIpDrag;
                let phase_blocks = legal || matrix.phase >= self.policy.proof_required_from_phase;
                let (code, remediation): (RiskGateCode, &str) = if legal {
                    (
                        RiskGateCode::LegalIpDragUnresolved,
                        "attach the LEGAL/IP artifact pack (license/provenance) before any promotion",
                    )
                } else {
                    (
                        RiskGateCode::HighSeverityMissingProof,
                        "attach proof artifacts (formal-guarantee / repro / provenance refs) before promotion",
                    )
                };
                let severity = if phase_blocks {
                    ContractSeverity::Block
                } else {
                    ContractSeverity::Warn
                };
                findings.push(RiskGateFinding::new(
                    code,
                    severity,
                    entry,
                    Some(assessment.class),
                    format!(
                        "{} risk is {} in phase {} but carries no proof artifacts",
                        assessment.class.as_str(),
                        risk_class_tag(assessment.severity),
                        matrix.phase.as_str()
                    ),
                    remediation,
                ));
                if phase_blocks {
                    blocked = true;
                } else {
                    warned = true;
                }
                if status == CountermeasureStatus::Satisfied {
                    status = CountermeasureStatus::InsufficientProof;
                }
            }

            let gate_outcome = if blocked {
                GateOutcome::Block
            } else if warned {
                GateOutcome::Warn
            } else {
                GateOutcome::Pass
            };
            log_records.push(RiskGateLogRecord {
                entry_id: matrix.entry_id.clone(),
                risk_class: assessment.class,
                severity: risk_class_tag(assessment.severity).to_string(),
                countermeasure_status: status,
                gate_outcome,
            });
        }

        // AC2 escalation: a dangerous combination of high-severity classes must
        // be fully proven before promotion, regardless of phase.
        if combination {
            let unproven: Vec<FailureModeClass> = high_classes
                .iter()
                .copied()
                .filter(|class| {
                    matrix
                        .assessment_for(*class)
                        .is_none_or(|a| a.proof_artifact_refs.is_empty())
                })
                .collect();
            if !unproven.is_empty() {
                let listed = unproven
                    .iter()
                    .map(|c| c.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                findings.push(RiskGateFinding::new(
                    RiskGateCode::DangerousCombinationEscalation,
                    ContractSeverity::Block,
                    entry,
                    None,
                    format!(
                        "{} co-occurring high-severity classes require proof; unproven: {listed}",
                        high_classes.len()
                    ),
                    "attach proof artifacts to every high-severity class in the combination before promotion",
                ));
            }
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Map the shared [`RiskClass`] scale to a stable lowercase tag (the shared enum
/// serializes in PascalCase; the gate logs use the snake_case convention).
fn risk_class_tag(risk: RiskClass) -> &'static str {
    match risk {
        RiskClass::Low => "low",
        RiskClass::Medium => "medium",
        RiskClass::High => "high",
        RiskClass::Critical => "critical",
    }
}

/// Map a legal/IP artifact status to a failure-mode severity.
fn legal_status_severity(status: IpArtifactStatus) -> RiskClass {
    match status {
        IpArtifactStatus::Clear => RiskClass::Low,
        IpArtifactStatus::Expired | IpArtifactStatus::Unknown => RiskClass::Medium,
        IpArtifactStatus::NeedsCounsel => RiskClass::High,
        IpArtifactStatus::Blocked => RiskClass::Critical,
    }
}

/// Build the deterministic JSON-stats artifact for a report.
fn export_json_stats(
    report_id: &str,
    matrices: &[RiskMatrix],
    findings: &[RiskGateFinding],
) -> RiskGateJsonStatsArtifact {
    #[derive(Serialize)]
    struct EntryStat<'a> {
        entry_id: &'a str,
        entry_kind: EntryKind,
        phase: RolloutPhase,
        matrix_id: &'a str,
        primary_class: Option<FailureModeClass>,
        assessment_count: usize,
        high_severity_count: usize,
        blocking_findings: usize,
    }
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        entry_count: usize,
        finding_count: usize,
        entries: Vec<EntryStat<'a>>,
    }
    let entries = matrices
        .iter()
        .map(|m| EntryStat {
            entry_id: &m.entry_id,
            entry_kind: m.entry_kind,
            phase: m.phase,
            matrix_id: &m.matrix_id,
            primary_class: m.primary_class,
            assessment_count: m.assessments.len(),
            high_severity_count: m.high_severity_classes().len(),
            blocking_findings: findings
                .iter()
                .filter(|f| f.entry_id == m.entry_id && f.severity == ContractSeverity::Block)
                .count(),
        })
        .collect();
    let payload = Export {
        schema_version: FAILURE_MODE_RISK_SCHEMA_VERSION,
        report_id,
        entry_count: matrices.len(),
        finding_count: findings.len(),
        entries,
    };
    let content = serde_json::to_string_pretty(&payload).unwrap_or_else(|error| error.to_string());
    RiskGateJsonStatsArtifact {
        path: format!("{report_id}/failure_mode_risk.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

/// Canonical, id-free view of a report used for content-addressing.
#[derive(Serialize)]
struct CanonicalReport<'a> {
    schema_version: &'a str,
    policy: &'a RiskGatePolicy,
    entry_ids: &'a [String],
    findings: &'a [RiskGateFinding],
    log_records: &'a [RiskGateLogRecord],
    summary: &'a RiskGateSummary,
}

/// SHA-256 (hex) of a canonical JSON serialization.
fn stable_hash<T: Serialize + ?Sized>(value: &T) -> String {
    let mut hasher = Sha256::new();
    match serde_json::to_vec(value) {
        Ok(bytes) => hasher.update(bytes),
        Err(error) => hasher.update(error.to_string().as_bytes()),
    }
    crate::util::hex_encode(&hasher.finalize())
}

/// SHA-256 (hex) of raw bytes.
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    crate::util::hex_encode(&hasher.finalize())
}

/// First 16 hex chars of a hash, for compact ids.
fn short_hash(value: &str) -> String {
    value.chars().take(16).collect()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recommendation_contract::example_complete_contract;

    // ── Fixtures ─────────────────────────────────────────────────────────────

    fn proven(class: FailureModeClass, severity: RiskClass) -> RiskAssessment {
        RiskAssessment::new(class, severity)
            .with_countermeasure("mitigation")
            .with_fallback("fallback")
            .with_proof_artifact("proof-1")
            .with_rationale("rationale")
    }

    fn passing_matrix() -> RiskMatrix {
        RiskMatrix::new(
            "cand-001",
            EntryKind::CandidatePrimitive,
            RolloutPhase::GeneralAvailability,
            vec![
                proven(FailureModeClass::WholeStackBarrier, RiskClass::High).primary(),
                RiskAssessment::new(FailureModeClass::ErgonomicsCliff, RiskClass::Low)
                    .with_rationale("minor"),
            ],
        )
        .expect("valid matrix")
    }

    // ── Taxonomy ─────────────────────────────────────────────────────────────

    #[test]
    fn all_classes_have_distinct_order_indices_and_tags() {
        let mut indices: Vec<u8> = FailureModeClass::ALL
            .iter()
            .map(|c| c.order_index())
            .collect();
        indices.sort_unstable();
        indices.dedup();
        assert_eq!(indices.len(), 8);

        let mut tags: Vec<&str> = FailureModeClass::ALL.iter().map(|c| c.as_str()).collect();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), 8);

        let mut prefixes: Vec<&str> = FailureModeClass::ALL
            .iter()
            .map(|c| c.clause_prefix())
            .collect();
        prefixes.sort_unstable();
        prefixes.dedup();
        assert_eq!(prefixes.len(), 8);
    }

    #[test]
    fn class_serde_round_trips() {
        for class in FailureModeClass::ALL {
            let json = serde_json::to_string(&class).expect("serialize");
            let back: FailureModeClass = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(class, back);
            assert_eq!(json, format!("\"{}\"", class.as_str()));
        }
    }

    #[test]
    fn risk_class_tags_are_lowercase() {
        assert_eq!(risk_class_tag(RiskClass::Low), "low");
        assert_eq!(risk_class_tag(RiskClass::Medium), "medium");
        assert_eq!(risk_class_tag(RiskClass::High), "high");
        assert_eq!(risk_class_tag(RiskClass::Critical), "critical");
    }

    // ── Matrix construction ──────────────────────────────────────────────────

    #[test]
    fn matrix_sorts_assessments_and_picks_primary() {
        let matrix = RiskMatrix::new(
            "cand-002",
            EntryKind::CandidatePrimitive,
            RolloutPhase::Shadow,
            vec![
                proven(FailureModeClass::LegalIpDrag, RiskClass::Medium),
                proven(FailureModeClass::WholeStackBarrier, RiskClass::High).primary(),
            ],
        )
        .expect("valid");
        // sorted by class order index: WholeStackBarrier (0) before LegalIpDrag (7).
        assert_eq!(
            matrix.assessments[0].class,
            FailureModeClass::WholeStackBarrier
        );
        assert_eq!(matrix.assessments[1].class, FailureModeClass::LegalIpDrag);
        assert_eq!(
            matrix.primary_class,
            Some(FailureModeClass::WholeStackBarrier)
        );
    }

    #[test]
    fn matrix_id_is_deterministic_and_order_independent() {
        let a = RiskMatrix::new(
            "cand-003",
            EntryKind::CompositionPlan,
            RolloutPhase::Canary,
            vec![
                proven(FailureModeClass::Unpredictability, RiskClass::High).primary(),
                proven(FailureModeClass::HiddenConstants, RiskClass::Medium),
            ],
        )
        .expect("valid");
        let b = RiskMatrix::new(
            "cand-003",
            EntryKind::CompositionPlan,
            RolloutPhase::Canary,
            vec![
                proven(FailureModeClass::HiddenConstants, RiskClass::Medium),
                proven(FailureModeClass::Unpredictability, RiskClass::High).primary(),
            ],
        )
        .expect("valid");
        assert_eq!(a, b);
        assert_eq!(a.matrix_id, b.matrix_id);
        assert!(a.matrix_id.starts_with("fmr-matrix-"));
    }

    #[test]
    fn matrix_rejects_empty_entry_empty_assessments_and_duplicate_class() {
        assert!(matches!(
            RiskMatrix::new(
                "   ",
                EntryKind::CandidatePrimitive,
                RolloutPhase::Shadow,
                vec![proven(FailureModeClass::WholeStackBarrier, RiskClass::Low)],
            ),
            Err(FailureModeRiskError::InvalidInput { .. })
        ));
        assert!(matches!(
            RiskMatrix::new(
                "cand",
                EntryKind::CandidatePrimitive,
                RolloutPhase::Shadow,
                Vec::<RiskAssessment>::new(),
            ),
            Err(FailureModeRiskError::InvalidInput { .. })
        ));
        assert!(matches!(
            RiskMatrix::new(
                "cand",
                EntryKind::CandidatePrimitive,
                RolloutPhase::Shadow,
                vec![
                    proven(FailureModeClass::ErgonomicsCliff, RiskClass::Low),
                    proven(FailureModeClass::ErgonomicsCliff, RiskClass::High),
                ],
            ),
            Err(FailureModeRiskError::InvalidInput { .. })
        ));
    }

    // ── AC1: primary risk + countermeasure + fallback ────────────────────────

    #[test]
    fn ac1_passing_matrix_clears_the_gate() {
        let report = RiskGate::default().evaluate(&passing_matrix());
        assert!(report.gate_passes, "{:?}", report.findings);
        assert!(report.blocked_entry_ids.is_empty());
    }

    #[test]
    fn ac1_missing_primary_risk_blocks() {
        let matrix = RiskMatrix::new(
            "cand-noprimary",
            EntryKind::CandidatePrimitive,
            RolloutPhase::Shadow,
            // no assessment is flagged primary
            vec![proven(FailureModeClass::ErgonomicsCliff, RiskClass::Low)],
        )
        .expect("valid");
        assert_eq!(matrix.primary_class, None);
        let report = RiskGate::default().evaluate(&matrix);
        assert!(!report.gate_passes);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == RiskGateCode::MissingPrimaryRisk)
        );
    }

    #[test]
    fn ac1_missing_countermeasure_blocks() {
        let matrix = RiskMatrix::new(
            "cand-nocm",
            EntryKind::CandidatePrimitive,
            RolloutPhase::Shadow,
            vec![
                RiskAssessment::new(FailureModeClass::WholeStackBarrier, RiskClass::High)
                    .primary()
                    .with_fallback("fallback")
                    .with_proof_artifact("proof"),
            ],
        )
        .expect("valid");
        let report = RiskGate::default().evaluate(&matrix);
        assert!(!report.gate_passes);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == RiskGateCode::MissingCountermeasure)
        );
    }

    #[test]
    fn ac1_missing_fallback_blocks() {
        let matrix = RiskMatrix::new(
            "cand-nofb",
            EntryKind::CandidatePrimitive,
            RolloutPhase::Shadow,
            vec![
                RiskAssessment::new(FailureModeClass::WholeStackBarrier, RiskClass::High)
                    .primary()
                    .with_countermeasure("mitigation")
                    .with_proof_artifact("proof"),
            ],
        )
        .expect("valid");
        let report = RiskGate::default().evaluate(&matrix);
        assert!(!report.gate_passes);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == RiskGateCode::MissingFallback)
        );
    }

    // ── AC2: severity-aware escalation ───────────────────────────────────────

    #[test]
    fn ac2_high_severity_missing_proof_is_advisory_in_shadow_blocking_in_canary() {
        let make = |phase| {
            RiskMatrix::new(
                "cand-proof",
                EntryKind::CandidatePrimitive,
                phase,
                vec![
                    RiskAssessment::new(FailureModeClass::Unpredictability, RiskClass::High)
                        .primary()
                        .with_countermeasure("mitigation")
                        .with_fallback("fallback"),
                ],
            )
            .expect("valid")
        };
        let shadow = RiskGate::default().evaluate(&make(RolloutPhase::Shadow));
        assert!(shadow.gate_passes, "shadow should warn, not block");
        assert!(
            shadow
                .findings
                .iter()
                .any(|f| f.code == RiskGateCode::HighSeverityMissingProof
                    && f.severity == ContractSeverity::Warn)
        );

        let canary = RiskGate::default().evaluate(&make(RolloutPhase::Canary));
        assert!(!canary.gate_passes, "canary should block");
        assert!(
            canary
                .findings
                .iter()
                .any(|f| f.code == RiskGateCode::HighSeverityMissingProof
                    && f.severity == ContractSeverity::Block)
        );
    }

    #[test]
    fn ac2_legal_ip_drag_blocks_even_in_shadow() {
        let matrix = RiskMatrix::new(
            "cand-legal",
            EntryKind::CandidatePrimitive,
            RolloutPhase::Shadow,
            vec![
                RiskAssessment::new(FailureModeClass::LegalIpDrag, RiskClass::Critical)
                    .primary()
                    .with_countermeasure("design-around")
                    .with_fallback("drop the feature"),
            ],
        )
        .expect("valid");
        let report = RiskGate::default().evaluate(&matrix);
        assert!(
            !report.gate_passes,
            "legal/IP drag must block in every phase"
        );
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == RiskGateCode::LegalIpDragUnresolved
                    && f.severity == ContractSeverity::Block)
        );
    }

    #[test]
    fn ac2_dangerous_combination_escalates() {
        let matrix = RiskMatrix::new(
            "cand-combo",
            EntryKind::CompositionPlan,
            RolloutPhase::Shadow,
            vec![
                // two high-severity classes; one is unproven -> escalation
                proven(FailureModeClass::WholeStackBarrier, RiskClass::High).primary(),
                RiskAssessment::new(FailureModeClass::Unpredictability, RiskClass::Critical)
                    .with_countermeasure("mitigation")
                    .with_fallback("fallback"),
            ],
        )
        .expect("valid");
        let report = RiskGate::default().evaluate(&matrix);
        assert!(!report.gate_passes);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == RiskGateCode::DangerousCombinationEscalation)
        );
    }

    #[test]
    fn ac2_fully_proven_combination_passes() {
        let matrix = RiskMatrix::new(
            "cand-combo-ok",
            EntryKind::CompositionPlan,
            RolloutPhase::GeneralAvailability,
            vec![
                proven(FailureModeClass::WholeStackBarrier, RiskClass::High).primary(),
                proven(FailureModeClass::Unpredictability, RiskClass::Critical),
            ],
        )
        .expect("valid");
        let report = RiskGate::default().evaluate(&matrix);
        assert!(report.gate_passes, "{:?}", report.findings);
    }

    // ── AC3: auditable logs ──────────────────────────────────────────────────

    #[test]
    fn ac3_log_records_carry_required_fields_and_render_jsonl() {
        let report = RiskGate::default().evaluate(&passing_matrix());
        // one log record per assessed class
        assert_eq!(report.log_records.len(), 2);
        let primary = report
            .log_records
            .iter()
            .find(|r| r.risk_class == FailureModeClass::WholeStackBarrier)
            .expect("primary log");
        assert_eq!(primary.entry_id, "cand-001");
        assert_eq!(primary.severity, "high");
        assert_eq!(
            primary.countermeasure_status,
            CountermeasureStatus::Satisfied
        );
        assert_eq!(primary.gate_outcome, GateOutcome::Pass);

        let jsonl = report.render_log_jsonl();
        assert_eq!(jsonl.lines().count(), 2);
        for line in jsonl.lines() {
            let value: serde_json::Value = serde_json::from_str(line).expect("valid json line");
            for key in [
                "entry_id",
                "risk_class",
                "severity",
                "countermeasure_status",
                "gate_outcome",
            ] {
                assert!(value.get(key).is_some(), "missing key {key} in {line}");
            }
        }
    }

    #[test]
    fn ac3_status_reflects_failure_kind() {
        let matrix = RiskMatrix::new(
            "cand-status",
            EntryKind::CandidatePrimitive,
            RolloutPhase::Canary,
            vec![
                RiskAssessment::new(FailureModeClass::HiddenConstants, RiskClass::High)
                    .primary()
                    .with_countermeasure("mitigation")
                    .with_fallback("fallback"),
            ],
        )
        .expect("valid");
        let report = RiskGate::default().evaluate(&matrix);
        let record = &report.log_records[0];
        assert_eq!(
            record.countermeasure_status,
            CountermeasureStatus::InsufficientProof
        );
        assert_eq!(record.gate_outcome, GateOutcome::Block);
    }

    // ── Report determinism + artifacts ───────────────────────────────────────

    #[test]
    fn report_is_deterministic_and_round_trips() {
        let matrix = passing_matrix();
        let a = RiskGate::default().evaluate(&matrix);
        let b = RiskGate::default().evaluate(&matrix);
        assert_eq!(a, b);
        assert_eq!(a.report_id, b.report_id);
        assert!(a.report_id.starts_with("fmr-report-"));

        let json = serde_json::to_string(&a).expect("serialize");
        let back: RiskGateReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(a, back);

        // json-stats checksum matches its content
        assert_eq!(
            a.exported_json_stats.sha256,
            sha256_hex(a.exported_json_stats.content.as_bytes())
        );
        assert!(a.replay_command.contains(&a.report_id));
    }

    #[test]
    fn batch_evaluation_reports_blocked_entries_sorted() {
        let bad = RiskMatrix::new(
            "zzz-bad",
            EntryKind::CandidatePrimitive,
            RolloutPhase::Shadow,
            vec![RiskAssessment::new(FailureModeClass::ErgonomicsCliff, RiskClass::High).primary()],
        )
        .expect("valid");
        let good = passing_matrix();
        let report = RiskGate::default().evaluate_all(&[bad, good]);
        assert!(!report.gate_passes);
        assert_eq!(report.blocked_entry_ids, vec!["zzz-bad".to_string()]);
        assert_eq!(report.summary.total_entries, 2);
        assert_eq!(report.summary.blocked_entries, 1);
    }

    // ── Contract bridge ──────────────────────────────────────────────────────

    #[test]
    fn from_contract_builds_a_recommendation_card_matrix() {
        let contract = example_complete_contract();
        let matrix = RiskMatrix::from_contract(
            &contract,
            FailureModeClass::WholeStackBarrier,
            RolloutPhase::Canary,
        )
        .expect("valid");
        assert_eq!(matrix.entry_kind, EntryKind::RecommendationCard);
        assert_eq!(matrix.entry_id, contract.card_id);
        assert_eq!(
            matrix.primary_class,
            Some(FailureModeClass::WholeStackBarrier)
        );
        // an auto legal/IP-drag assessment is folded in
        assert!(
            matrix
                .assessment_for(FailureModeClass::LegalIpDrag)
                .is_some()
        );

        let primary = matrix.primary_assessment().expect("primary");
        assert_eq!(primary.countermeasure, contract.failure_mode.countermeasure);
        assert_eq!(
            primary.fallback,
            contract.budgeted_mode.conservative_fallback_trigger
        );
    }

    #[test]
    fn from_contract_with_blocked_legal_status_blocks() {
        let mut contract = example_complete_contract();
        contract.failure_mode.legal_status = IpArtifactStatus::Blocked;
        contract.failure_mode.provenance_artifact = None;
        let matrix = RiskMatrix::from_contract(
            &contract,
            FailureModeClass::HiddenConstants,
            RolloutPhase::Shadow,
        )
        .expect("valid");
        let report = RiskGate::default().evaluate(&matrix);
        assert!(
            !report.gate_passes,
            "blocked legal status must fail the gate"
        );
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.class == Some(FailureModeClass::LegalIpDrag)
                    && f.severity == ContractSeverity::Block)
        );
    }

    // ── Presentation-only ────────────────────────────────────────────────────

    #[test]
    fn evaluating_does_not_mutate_the_matrix() {
        let matrix = passing_matrix();
        let before = matrix.clone();
        let _ = RiskGate::default().evaluate(&matrix);
        assert_eq!(matrix, before);
    }

    #[test]
    fn markdown_render_is_stable_and_nonempty() {
        let report = RiskGate::default().evaluate(&passing_matrix());
        let a = report.render_markdown();
        let b = report.render_markdown();
        assert_eq!(a, b);
        assert!(a.contains(&report.report_id));
        assert!(a.contains("PASS"));
    }
}
