// SPDX-License-Identifier: Apache-2.0
//! Composability/interference matrix and dangerous-combination gate.
//!
//! Encodes the alien-graveyard §0.25 composability rules as machine-checkable
//! policy over the S/A uplift primitives ([`crate::ecosystem_scan::UpliftCandidate`]):
//! a relation matrix (requires / conflicts / synergy), a dangerous-combination
//! registry (severity + required mitigations), and a gate that blocks invalid or
//! unsafe primitive combinations before promotion.
//!
//! # Outputs
//!
//! - [`CompositionMatrix`]: per-pair relations with a [`canonical_matrix`].
//! - [`DangerousCombinationRegistry`]: risky subsets with severity and required
//!   mitigations, with a [`canonical_registry`].
//! - [`CompositionGate`]: evaluates a [`CompositionPlan`] into a deterministic
//!   [`CompositionGateReport`] carrying the composition id, detected hazards,
//!   mitigation status, and the gate verdict.
//!
//! # Multi-controller rule
//!
//! Any plan with two or more designated controllers must supply interference
//! artifacts (microbench / timescale-separation / replay proof) and an explicit
//! fast/slow split, mirroring the timescale-separation guardrails in
//! [`crate::controller_composition`].
//!
//! # Determinism
//!
//! Pairs and combinations are checked in canonical primitive order and hazards
//! are emitted deterministically, so the report id and checksum are stable.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::adversarial_fixtures::RiskClass;
use crate::ecosystem_scan::UpliftCandidate;

// ── Schema Version ───────────────────────────────────────────────────────

/// Schema version for composition-gate reports.
pub const COMPOSITION_MATRIX_SCHEMA_VERSION: &str = "composition-matrix-v1";

// ── Relations ────────────────────────────────────────────────────────────

/// A directed relation between two primitives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionRelation {
    /// `from` requires `to` to also be present.
    Requires,
    /// `from` and `to` must not be composed together.
    Conflicts,
    /// `from` and `to` reinforce each other.
    Synergy,
    /// No special relation.
    Neutral,
}

impl CompositionRelation {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Requires => "requires",
            Self::Conflicts => "conflicts",
            Self::Synergy => "synergy",
            Self::Neutral => "neutral",
        }
    }
}

/// A single relation edge in the composition matrix.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelationEdge {
    /// Source primitive.
    pub from: UpliftCandidate,
    /// Target primitive.
    pub to: UpliftCandidate,
    /// The relation.
    pub relation: CompositionRelation,
    /// Rationale note.
    pub note: String,
}

impl RelationEdge {
    /// Build an edge.
    #[must_use]
    pub fn new(
        from: UpliftCandidate,
        to: UpliftCandidate,
        relation: CompositionRelation,
        note: impl Into<String>,
    ) -> Self {
        Self {
            from,
            to,
            relation,
            note: note.into(),
        }
    }
}

/// The composability relation matrix.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompositionMatrix {
    /// Relation edges (order preserved).
    pub edges: Vec<RelationEdge>,
}

impl CompositionMatrix {
    /// Build an empty matrix.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an edge.
    #[must_use]
    pub fn with_edge(mut self, edge: RelationEdge) -> Self {
        self.edges.push(edge);
        self
    }

    /// The relation from `a` to `b`, if any non-neutral edge exists.
    ///
    /// `Conflicts` and `Synergy` are treated as symmetric; `Requires` is
    /// directional.
    #[must_use]
    pub fn relation(&self, a: UpliftCandidate, b: UpliftCandidate) -> CompositionRelation {
        // Directional requires first.
        if let Some(edge) = self
            .edges
            .iter()
            .find(|e| e.from == a && e.to == b && e.relation == CompositionRelation::Requires)
        {
            return edge.relation;
        }
        // Symmetric conflicts / synergy.
        for edge in &self.edges {
            let matches = (edge.from == a && edge.to == b) || (edge.from == b && edge.to == a);
            if matches
                && matches!(
                    edge.relation,
                    CompositionRelation::Conflicts | CompositionRelation::Synergy
                )
            {
                return edge.relation;
            }
        }
        CompositionRelation::Neutral
    }

    /// All primitives `a` requires.
    #[must_use]
    pub fn requirements(&self, a: UpliftCandidate) -> Vec<UpliftCandidate> {
        self.edges
            .iter()
            .filter(|e| e.from == a && e.relation == CompositionRelation::Requires)
            .map(|e| e.to)
            .collect()
    }
}

/// The canonical composition matrix over the eight uplift primitives.
#[must_use]
pub fn canonical_matrix() -> CompositionMatrix {
    use UpliftCandidate as P;
    CompositionMatrix::new()
        .with_edge(RelationEdge::new(
            P::EGraphOptimizer,
            P::MetamorphicOracle,
            CompositionRelation::Requires,
            "e-graph rewrites of generated code must be equivalence-checked",
        ))
        .with_edge(RelationEdge::new(
            P::CegisSynthesis,
            P::MetamorphicOracle,
            CompositionRelation::Requires,
            "synthesized code must pass metamorphic equivalence before promotion",
        ))
        .with_edge(RelationEdge::new(
            P::ShadowRun,
            P::ConcolicDifferential,
            CompositionRelation::Synergy,
            "shadow divergences seed concolic exploration and vice versa",
        ))
        .with_edge(RelationEdge::new(
            P::AbstractInterpretation,
            P::ConcolicDifferential,
            CompositionRelation::Synergy,
            "over-approximation prunes the concrete/symbolic search space",
        ))
        .with_edge(RelationEdge::new(
            P::ContractsAsCode,
            P::ShadowRun,
            CompositionRelation::Synergy,
            "provenance contracts make shadow comparisons auditable",
        ))
}

// ── Dangerous Combinations ───────────────────────────────────────────────

/// A registered dangerous combination of primitives.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DangerousCombination {
    /// Stable combination id.
    pub combination_id: String,
    /// The primitives that together form the hazard (order-normalized).
    pub primitives: Vec<UpliftCandidate>,
    /// Severity of the hazard.
    pub severity: RiskClass,
    /// What makes the combination hazardous.
    pub hazard: String,
    /// Mitigations that must be declared to clear the hazard.
    pub required_mitigations: Vec<String>,
}

impl DangerousCombination {
    /// Build a dangerous combination (primitives are order-normalized).
    #[must_use]
    pub fn new(
        combination_id: impl Into<String>,
        primitives: impl IntoIterator<Item = UpliftCandidate>,
        severity: RiskClass,
        hazard: impl Into<String>,
        required_mitigations: impl IntoIterator<Item = String>,
    ) -> Self {
        let mut primitives: Vec<UpliftCandidate> = primitives.into_iter().collect();
        primitives.sort();
        primitives.dedup();
        Self {
            combination_id: combination_id.into(),
            primitives,
            severity,
            hazard: hazard.into(),
            required_mitigations: required_mitigations.into_iter().collect(),
        }
    }

    /// Whether every primitive of this combination is present in `plan`.
    #[must_use]
    pub fn is_present_in(&self, plan_primitives: &[UpliftCandidate]) -> bool {
        self.primitives.iter().all(|p| plan_primitives.contains(p))
    }
}

/// A registry of dangerous combinations.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DangerousCombinationRegistry {
    /// Registered combinations.
    pub combinations: Vec<DangerousCombination>,
}

impl DangerousCombinationRegistry {
    /// Build an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a combination.
    #[must_use]
    pub fn with_combination(mut self, combination: DangerousCombination) -> Self {
        self.combinations.push(combination);
        self
    }
}

/// The canonical dangerous-combination registry.
#[must_use]
pub fn canonical_registry() -> DangerousCombinationRegistry {
    use UpliftCandidate as P;
    DangerousCombinationRegistry::new()
        .with_combination(DangerousCombination::new(
            "dc-synthesis-rewrite-no-safety",
            [P::CegisSynthesis, P::EGraphOptimizer],
            RiskClass::High,
            "synthesis followed by e-graph rewriting can drift semantics without a \
             safety over-approximation",
            [
                "include abstract_interpretation in the composition".to_string(),
                "require a metamorphic_oracle equivalence pass on the optimized output".to_string(),
            ],
        ))
        .with_combination(DangerousCombination::new(
            "dc-dual-hole-strategies",
            [P::CegisSynthesis, P::ConcolicDifferential],
            RiskClass::Medium,
            "CEGIS and concolic exploration racing on the same unmapped hole produce \
             non-deterministic resolutions",
            ["assign disjoint hole sets to each strategy".to_string()],
        ))
}

// ── Controllers & Plan ───────────────────────────────────────────────────

/// Timescale role of a controller in a multi-controller plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControllerRole {
    /// Fast (inner-loop) controller.
    Fast,
    /// Slow (outer-loop) controller.
    Slow,
}

impl ControllerRole {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Slow => "slow",
        }
    }
}

/// A controller designation within a plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControllerDesignation {
    /// The controller primitive.
    pub primitive: UpliftCandidate,
    /// Its timescale role.
    pub role: ControllerRole,
}

impl ControllerDesignation {
    /// Build a designation.
    #[must_use]
    pub fn new(primitive: UpliftCandidate, role: ControllerRole) -> Self {
        Self { primitive, role }
    }
}

/// A proposed composition of primitives.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompositionPlan {
    /// Stable composition id.
    pub composition_id: String,
    /// Primitives in the composition.
    pub primitives: Vec<UpliftCandidate>,
    /// Controller designations (timescale roles).
    pub controllers: Vec<ControllerDesignation>,
    /// Interference artifacts (microbench / timescale-separation / replay proof).
    pub interference_artifacts: Vec<String>,
    /// Mitigations declared as applied.
    pub applied_mitigations: Vec<String>,
}

impl CompositionPlan {
    /// Start a plan.
    #[must_use]
    pub fn new(
        composition_id: impl Into<String>,
        primitives: impl IntoIterator<Item = UpliftCandidate>,
    ) -> Self {
        Self {
            composition_id: composition_id.into(),
            primitives: primitives.into_iter().collect(),
            controllers: Vec::new(),
            interference_artifacts: Vec::new(),
            applied_mitigations: Vec::new(),
        }
    }

    /// Designate a controller.
    #[must_use]
    pub fn with_controller(mut self, primitive: UpliftCandidate, role: ControllerRole) -> Self {
        self.controllers
            .push(ControllerDesignation::new(primitive, role));
        self
    }

    /// Attach an interference artifact.
    #[must_use]
    pub fn with_interference_artifact(mut self, artifact: impl Into<String>) -> Self {
        self.interference_artifacts.push(artifact.into());
        self
    }

    /// Declare an applied mitigation.
    #[must_use]
    pub fn with_mitigation(mut self, mitigation: impl Into<String>) -> Self {
        self.applied_mitigations.push(mitigation.into());
        self
    }
}

// ── Hazards ──────────────────────────────────────────────────────────────

/// Severity of a composition hazard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HazardSeverity {
    /// Blocks the gate.
    Block,
    /// Advisory only.
    Warn,
}

impl HazardSeverity {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Warn => "warn",
        }
    }

    /// Map a dangerous-combination risk class onto a hazard severity.
    #[must_use]
    pub fn from_risk(risk: RiskClass) -> Self {
        match risk {
            RiskClass::Low | RiskClass::Medium => Self::Warn,
            RiskClass::High | RiskClass::Critical => Self::Block,
        }
    }
}

/// Machine-readable hazard code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HazardCode {
    /// A required primitive is absent.
    MissingRequiredPrimitive,
    /// Two primitives conflict.
    ConflictingPrimitives,
    /// A dangerous combination is present without its required mitigations.
    UnmitigatedDangerousCombination,
    /// A multi-controller plan lacks interference artifacts.
    MultiControllerMissingInterferenceArtifacts,
    /// A multi-controller plan lacks a fast/slow split.
    MultiControllerMissingFastSlowSplit,
    /// A designated controller is not in the plan's primitive set.
    UnknownControllerPrimitive,
    /// A controller primitive is designated more than once.
    DuplicateControllerDesignation,
    /// A primitive appears more than once.
    DuplicatePrimitive,
    /// The plan has no primitives.
    EmptyComposition,
}

impl HazardCode {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingRequiredPrimitive => "missing_required_primitive",
            Self::ConflictingPrimitives => "conflicting_primitives",
            Self::UnmitigatedDangerousCombination => "unmitigated_dangerous_combination",
            Self::MultiControllerMissingInterferenceArtifacts => {
                "multi_controller_missing_interference_artifacts"
            }
            Self::MultiControllerMissingFastSlowSplit => "multi_controller_missing_fast_slow_split",
            Self::UnknownControllerPrimitive => "unknown_controller_primitive",
            Self::DuplicateControllerDesignation => "duplicate_controller_designation",
            Self::DuplicatePrimitive => "duplicate_primitive",
            Self::EmptyComposition => "empty_composition",
        }
    }
}

/// A detected composition hazard.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompositionHazard {
    /// Machine-readable code.
    pub code: HazardCode,
    /// Severity.
    pub severity: HazardSeverity,
    /// Primitives involved.
    pub involved: Vec<UpliftCandidate>,
    /// Human-readable detail.
    pub detail: String,
    /// Actionable remediation.
    pub remediation: String,
}

impl CompositionHazard {
    fn new(
        code: HazardCode,
        severity: HazardSeverity,
        involved: Vec<UpliftCandidate>,
        detail: impl Into<String>,
        remediation: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity,
            involved,
            detail: detail.into(),
            remediation: remediation.into(),
        }
    }
}

/// Mitigation status of a triggered dangerous combination.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CombinationMitigationStatus {
    /// The combination id.
    pub combination_id: String,
    /// Severity.
    pub severity: RiskClass,
    /// Required mitigations not yet declared.
    pub missing_mitigations: Vec<String>,
    /// Whether the combination is fully mitigated.
    pub mitigated: bool,
}

// ── Gate & Report ────────────────────────────────────────────────────────

/// Evaluates composition plans against the matrix and registry.
#[derive(Debug, Clone, Default)]
pub struct CompositionGate;

impl CompositionGate {
    /// Evaluate a plan into a deterministic report.
    #[must_use]
    pub fn evaluate(
        &self,
        plan: &CompositionPlan,
        matrix: &CompositionMatrix,
        registry: &DangerousCombinationRegistry,
    ) -> CompositionGateReport {
        let mut hazards: Vec<CompositionHazard> = Vec::new();

        self.check_structure(plan, &mut hazards);
        self.check_requires_and_conflicts(plan, matrix, &mut hazards);
        let mitigation_status = self.check_dangerous_combinations(plan, registry, &mut hazards);
        self.check_controllers(plan, &mut hazards);

        let synergies_detected = detect_synergies(plan, matrix);

        let blocking_hazard_count = hazards
            .iter()
            .filter(|h| h.severity == HazardSeverity::Block)
            .count();
        let advisory_hazard_count = hazards.len() - blocking_hazard_count;
        let gate_passes = blocking_hazard_count == 0;

        let summary = CompositionGateSummary {
            primitive_count: plan.primitives.len(),
            controller_count: plan.controllers.len(),
            blocking_hazard_count,
            advisory_hazard_count,
            dangerous_combinations_triggered: mitigation_status.len(),
            unmitigated_combinations: mitigation_status.iter().filter(|m| !m.mitigated).count(),
            synergy_count: synergies_detected.len(),
        };

        let report_id = report_id_for(plan, matrix, registry);
        let exported_json_stats = export_json_stats(
            &report_id,
            plan,
            &hazards,
            &mitigation_status,
            &synergies_detected,
            &summary,
        );
        let replay_command = format!(
            "doctor_frankentui composition-gate --report-id {report_id} \
             --composition {}",
            plan.composition_id
        );

        CompositionGateReport {
            schema_version: COMPOSITION_MATRIX_SCHEMA_VERSION.to_string(),
            report_id,
            composition_id: plan.composition_id.clone(),
            primitives: plan.primitives.clone(),
            hazards,
            mitigation_status,
            synergies_detected,
            summary,
            gate_passes,
            exported_json_stats,
            replay_command,
        }
    }

    fn check_structure(&self, plan: &CompositionPlan, hazards: &mut Vec<CompositionHazard>) {
        if plan.primitives.is_empty() {
            hazards.push(CompositionHazard::new(
                HazardCode::EmptyComposition,
                HazardSeverity::Block,
                Vec::new(),
                "composition has no primitives",
                "add at least one primitive to the composition",
            ));
        }
        for candidate in UpliftCandidate::ALL.iter().copied() {
            let count = plan.primitives.iter().filter(|p| **p == candidate).count();
            if count > 1 {
                hazards.push(CompositionHazard::new(
                    HazardCode::DuplicatePrimitive,
                    HazardSeverity::Block,
                    vec![candidate],
                    format!(
                        "primitive {} appears {count} times",
                        candidate.candidate_id()
                    ),
                    "list each primitive at most once",
                ));
            }
            let designations = plan
                .controllers
                .iter()
                .filter(|c| c.primitive == candidate)
                .count();
            if designations > 1 {
                hazards.push(CompositionHazard::new(
                    HazardCode::DuplicateControllerDesignation,
                    HazardSeverity::Block,
                    vec![candidate],
                    format!(
                        "controller {} is designated {designations} times",
                        candidate.candidate_id()
                    ),
                    "designate each controller exactly once",
                ));
            }
        }
    }

    fn check_requires_and_conflicts(
        &self,
        plan: &CompositionPlan,
        matrix: &CompositionMatrix,
        hazards: &mut Vec<CompositionHazard>,
    ) {
        // Requires: directional, checked in canonical order.
        for primitive in UpliftCandidate::ALL.iter().copied() {
            if !plan.primitives.contains(&primitive) {
                continue;
            }
            for required in matrix.requirements(primitive) {
                if !plan.primitives.contains(&required) {
                    hazards.push(CompositionHazard::new(
                        HazardCode::MissingRequiredPrimitive,
                        HazardSeverity::Block,
                        vec![primitive, required],
                        format!(
                            "{} requires {} which is absent",
                            primitive.candidate_id(),
                            required.candidate_id()
                        ),
                        format!("add {} to the composition", required.candidate_id()),
                    ));
                }
            }
        }
        // Conflicts: symmetric, each unordered pair once.
        for (i, a) in UpliftCandidate::ALL.iter().copied().enumerate() {
            for b in UpliftCandidate::ALL.iter().copied().skip(i + 1) {
                if plan.primitives.contains(&a)
                    && plan.primitives.contains(&b)
                    && matrix.relation(a, b) == CompositionRelation::Conflicts
                {
                    hazards.push(CompositionHazard::new(
                        HazardCode::ConflictingPrimitives,
                        HazardSeverity::Block,
                        vec![a, b],
                        format!("{} conflicts with {}", a.candidate_id(), b.candidate_id()),
                        "remove one of the conflicting primitives",
                    ));
                }
            }
        }
    }

    fn check_dangerous_combinations(
        &self,
        plan: &CompositionPlan,
        registry: &DangerousCombinationRegistry,
        hazards: &mut Vec<CompositionHazard>,
    ) -> Vec<CombinationMitigationStatus> {
        let mut statuses = Vec::new();
        for combination in &registry.combinations {
            if !combination.is_present_in(&plan.primitives) {
                continue;
            }
            let missing_mitigations: Vec<String> = combination
                .required_mitigations
                .iter()
                .filter(|m| !plan.applied_mitigations.iter().any(|applied| applied == *m))
                .cloned()
                .collect();
            let mitigated = missing_mitigations.is_empty();
            if !mitigated {
                hazards.push(CompositionHazard::new(
                    HazardCode::UnmitigatedDangerousCombination,
                    HazardSeverity::from_risk(combination.severity),
                    combination.primitives.clone(),
                    format!(
                        "dangerous combination `{}` ({:?}): {}",
                        combination.combination_id, combination.severity, combination.hazard
                    ),
                    format!("declare mitigations: {}", missing_mitigations.join("; ")),
                ));
            }
            statuses.push(CombinationMitigationStatus {
                combination_id: combination.combination_id.clone(),
                severity: combination.severity,
                missing_mitigations,
                mitigated,
            });
        }
        statuses
    }

    fn check_controllers(&self, plan: &CompositionPlan, hazards: &mut Vec<CompositionHazard>) {
        for designation in &plan.controllers {
            if !plan.primitives.contains(&designation.primitive) {
                hazards.push(CompositionHazard::new(
                    HazardCode::UnknownControllerPrimitive,
                    HazardSeverity::Block,
                    vec![designation.primitive],
                    format!(
                        "controller {} is not in the composition primitives",
                        designation.primitive.candidate_id()
                    ),
                    "add the controller primitive to the composition or remove its designation",
                ));
            }
        }
        if plan.controllers.len() >= 2 {
            if plan.interference_artifacts.is_empty() {
                hazards.push(CompositionHazard::new(
                    HazardCode::MultiControllerMissingInterferenceArtifacts,
                    HazardSeverity::Block,
                    plan.controllers.iter().map(|c| c.primitive).collect(),
                    "multi-controller plan has no interference artifacts",
                    "attach interference microbench + timescale-separation + replay proof",
                ));
            }
            let has_fast = plan
                .controllers
                .iter()
                .any(|c| c.role == ControllerRole::Fast);
            let has_slow = plan
                .controllers
                .iter()
                .any(|c| c.role == ControllerRole::Slow);
            if !(has_fast && has_slow) {
                hazards.push(CompositionHazard::new(
                    HazardCode::MultiControllerMissingFastSlowSplit,
                    HazardSeverity::Block,
                    plan.controllers.iter().map(|c| c.primitive).collect(),
                    "multi-controller plan lacks an explicit fast/slow split",
                    "designate at least one fast and one slow controller for timescale separation",
                ));
            }
        }
    }
}

fn detect_synergies(plan: &CompositionPlan, matrix: &CompositionMatrix) -> Vec<RelationEdge> {
    let mut synergies = Vec::new();
    for (i, a) in UpliftCandidate::ALL.iter().copied().enumerate() {
        for b in UpliftCandidate::ALL.iter().copied().skip(i + 1) {
            if plan.primitives.contains(&a)
                && plan.primitives.contains(&b)
                && matrix.relation(a, b) == CompositionRelation::Synergy
            {
                synergies.push(RelationEdge::new(
                    a,
                    b,
                    CompositionRelation::Synergy,
                    matrix
                        .edges
                        .iter()
                        .find(|e| (e.from == a && e.to == b) || (e.from == b && e.to == a))
                        .map_or_else(String::new, |e| e.note.clone()),
                ));
            }
        }
    }
    synergies
}

/// Aggregate counts for a composition-gate report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompositionGateSummary {
    /// Number of primitives.
    pub primitive_count: usize,
    /// Number of controller designations.
    pub controller_count: usize,
    /// Blocking hazards.
    pub blocking_hazard_count: usize,
    /// Advisory hazards.
    pub advisory_hazard_count: usize,
    /// Dangerous combinations triggered.
    pub dangerous_combinations_triggered: usize,
    /// Unmitigated dangerous combinations.
    pub unmitigated_combinations: usize,
    /// Synergies detected.
    pub synergy_count: usize,
}

/// Deterministic JSON-stats artifact (content + checksum).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompositionJsonStatsArtifact {
    /// Suggested relative output path.
    pub path: String,
    /// SHA-256 of `content`.
    pub sha256: String,
    /// Serialized JSON content.
    pub content: String,
}

/// Full composition-gate report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompositionGateReport {
    /// Schema version constant.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// The composition id.
    pub composition_id: String,
    /// The composed primitives.
    pub primitives: Vec<UpliftCandidate>,
    /// Detected hazards.
    pub hazards: Vec<CompositionHazard>,
    /// Mitigation status of triggered dangerous combinations.
    pub mitigation_status: Vec<CombinationMitigationStatus>,
    /// Synergies detected (informational).
    pub synergies_detected: Vec<RelationEdge>,
    /// Aggregate summary.
    pub summary: CompositionGateSummary,
    /// Whether the gate passes (no blocking hazards).
    pub gate_passes: bool,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: CompositionJsonStatsArtifact,
    /// Replay command for the report.
    pub replay_command: String,
}

impl CompositionGateReport {
    /// Every blocking remediation clause, in report order.
    #[must_use]
    pub fn blocking_remediations(&self) -> Vec<String> {
        self.hazards
            .iter()
            .filter(|h| h.severity == HazardSeverity::Block)
            .map(|h| h.remediation.clone())
            .collect()
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn export_json_stats(
    report_id: &str,
    plan: &CompositionPlan,
    hazards: &[CompositionHazard],
    mitigation_status: &[CombinationMitigationStatus],
    synergies: &[RelationEdge],
    summary: &CompositionGateSummary,
) -> CompositionJsonStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        composition_id: &'a str,
        primitives: &'a [UpliftCandidate],
        summary: &'a CompositionGateSummary,
        hazards: &'a [CompositionHazard],
        mitigation_status: &'a [CombinationMitigationStatus],
        synergies: &'a [RelationEdge],
    }

    let payload = Export {
        schema_version: COMPOSITION_MATRIX_SCHEMA_VERSION,
        report_id,
        composition_id: &plan.composition_id,
        primitives: &plan.primitives,
        summary,
        hazards,
        mitigation_status,
        synergies,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    CompositionJsonStatsArtifact {
        path: format!("{report_id}/composition_gate_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

fn report_id_for(
    plan: &CompositionPlan,
    matrix: &CompositionMatrix,
    registry: &DangerousCombinationRegistry,
) -> String {
    #[derive(Serialize)]
    struct IdInput<'a> {
        plan: &'a CompositionPlan,
        matrix: &'a CompositionMatrix,
        registry: &'a DangerousCombinationRegistry,
    }
    let hash = stable_hash(&IdInput {
        plan,
        matrix,
        registry,
    });
    format!("composition-gate-{}", short_hash(&hash))
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
    use UpliftCandidate as P;

    fn gate() -> CompositionGate {
        CompositionGate
    }

    fn has(hazards: &[CompositionHazard], code: HazardCode) -> bool {
        hazards.iter().any(|h| h.code == code)
    }

    #[test]
    fn matrix_relation_is_symmetric_for_synergy_and_conflict() {
        let matrix = canonical_matrix();
        assert_eq!(
            matrix.relation(P::ShadowRun, P::ConcolicDifferential),
            CompositionRelation::Synergy
        );
        assert_eq!(
            matrix.relation(P::ConcolicDifferential, P::ShadowRun),
            CompositionRelation::Synergy
        );
    }

    #[test]
    fn matrix_requires_is_directional() {
        let matrix = canonical_matrix();
        assert_eq!(
            matrix.relation(P::EGraphOptimizer, P::MetamorphicOracle),
            CompositionRelation::Requires
        );
        // The reverse is not a requirement.
        assert_eq!(
            matrix.relation(P::MetamorphicOracle, P::EGraphOptimizer),
            CompositionRelation::Neutral
        );
        assert_eq!(
            matrix.requirements(P::EGraphOptimizer),
            vec![P::MetamorphicOracle]
        );
    }

    #[test]
    fn valid_composition_passes() {
        // e-graph + metamorphic (its requirement) + contracts. No hazards.
        let plan = CompositionPlan::new(
            "comp-ok",
            [P::EGraphOptimizer, P::MetamorphicOracle, P::ContractsAsCode],
        );
        let report = gate().evaluate(&plan, &canonical_matrix(), &canonical_registry());
        assert!(report.gate_passes, "hazards: {:?}", report.hazards);
        assert!(report.hazards.is_empty());
    }

    #[test]
    fn missing_requirement_is_blocked() {
        // e-graph without metamorphic → missing requirement.
        let plan = CompositionPlan::new("comp-bad", [P::EGraphOptimizer]);
        let report = gate().evaluate(&plan, &canonical_matrix(), &canonical_registry());
        assert!(!report.gate_passes);
        assert!(has(&report.hazards, HazardCode::MissingRequiredPrimitive));
    }

    #[test]
    fn conflict_is_blocked() {
        let matrix = CompositionMatrix::new().with_edge(RelationEdge::new(
            P::CegisSynthesis,
            P::ConcolicDifferential,
            CompositionRelation::Conflicts,
            "exclusive hole strategies",
        ));
        let plan = CompositionPlan::new(
            "comp-conflict",
            [
                P::CegisSynthesis,
                P::ConcolicDifferential,
                P::MetamorphicOracle,
            ],
        );
        let report = gate().evaluate(&plan, &matrix, &DangerousCombinationRegistry::new());
        assert!(!report.gate_passes);
        assert!(has(&report.hazards, HazardCode::ConflictingPrimitives));
    }

    #[test]
    fn unmitigated_dangerous_combination_is_blocked() {
        // cegis + egraph triggers dc-synthesis-rewrite-no-safety (High → Block).
        // Include metamorphic to satisfy both requires edges.
        let plan = CompositionPlan::new(
            "comp-danger",
            [P::CegisSynthesis, P::EGraphOptimizer, P::MetamorphicOracle],
        );
        let report = gate().evaluate(&plan, &canonical_matrix(), &canonical_registry());
        assert!(!report.gate_passes);
        assert!(has(
            &report.hazards,
            HazardCode::UnmitigatedDangerousCombination
        ));
        assert_eq!(report.summary.dangerous_combinations_triggered, 1);
        assert_eq!(report.summary.unmitigated_combinations, 1);
    }

    #[test]
    fn mitigated_dangerous_combination_passes() {
        let plan = CompositionPlan::new(
            "comp-mitigated",
            [P::CegisSynthesis, P::EGraphOptimizer, P::MetamorphicOracle],
        )
        .with_mitigation("include abstract_interpretation in the composition")
        .with_mitigation("require a metamorphic_oracle equivalence pass on the optimized output");
        let report = gate().evaluate(&plan, &canonical_matrix(), &canonical_registry());
        assert!(report.gate_passes, "hazards: {:?}", report.hazards);
        assert_eq!(report.summary.dangerous_combinations_triggered, 1);
        assert_eq!(report.summary.unmitigated_combinations, 0);
        assert!(report.mitigation_status[0].mitigated);
    }

    #[test]
    fn medium_dangerous_combination_is_advisory() {
        // cegis + concolic triggers dc-dual-hole-strategies (Medium → Warn).
        let plan = CompositionPlan::new(
            "comp-medium",
            [
                P::CegisSynthesis,
                P::ConcolicDifferential,
                P::MetamorphicOracle,
            ],
        );
        let report = gate().evaluate(&plan, &canonical_matrix(), &canonical_registry());
        // Medium severity is advisory, so the gate still passes.
        assert!(report.gate_passes);
        assert!(
            report
                .hazards
                .iter()
                .any(|h| h.code == HazardCode::UnmitigatedDangerousCombination
                    && h.severity == HazardSeverity::Warn)
        );
    }

    #[test]
    fn multi_controller_requires_interference_artifacts() {
        let plan = CompositionPlan::new(
            "comp-controllers",
            [
                P::ControllerInterferenceGuardrails,
                P::ShadowRun,
                P::ConcolicDifferential,
            ],
        )
        .with_controller(P::ControllerInterferenceGuardrails, ControllerRole::Fast)
        .with_controller(P::ShadowRun, ControllerRole::Slow);
        let report = gate().evaluate(
            &plan,
            &CompositionMatrix::new(),
            &DangerousCombinationRegistry::new(),
        );
        assert!(!report.gate_passes);
        assert!(has(
            &report.hazards,
            HazardCode::MultiControllerMissingInterferenceArtifacts
        ));
    }

    #[test]
    fn multi_controller_requires_fast_slow_split() {
        let plan = CompositionPlan::new(
            "comp-controllers",
            [P::ControllerInterferenceGuardrails, P::ShadowRun],
        )
        .with_controller(P::ControllerInterferenceGuardrails, ControllerRole::Fast)
        .with_controller(P::ShadowRun, ControllerRole::Fast) // both fast → no split
        .with_interference_artifact("interference_microbench.json");
        let report = gate().evaluate(
            &plan,
            &CompositionMatrix::new(),
            &DangerousCombinationRegistry::new(),
        );
        assert!(!report.gate_passes);
        assert!(has(
            &report.hazards,
            HazardCode::MultiControllerMissingFastSlowSplit
        ));
    }

    #[test]
    fn well_formed_multi_controller_passes() {
        let plan = CompositionPlan::new(
            "comp-controllers-ok",
            [P::ControllerInterferenceGuardrails, P::ShadowRun],
        )
        .with_controller(P::ControllerInterferenceGuardrails, ControllerRole::Fast)
        .with_controller(P::ShadowRun, ControllerRole::Slow)
        .with_interference_artifact("interference_microbench.json")
        .with_interference_artifact("timescale_separation_proof.json");
        let report = gate().evaluate(
            &plan,
            &CompositionMatrix::new(),
            &DangerousCombinationRegistry::new(),
        );
        assert!(report.gate_passes, "hazards: {:?}", report.hazards);
        assert_eq!(report.summary.controller_count, 2);
    }

    #[test]
    fn unknown_controller_primitive_is_blocked() {
        let plan = CompositionPlan::new("comp", [P::ShadowRun])
            .with_controller(P::ContractsAsCode, ControllerRole::Fast);
        let report = gate().evaluate(
            &plan,
            &CompositionMatrix::new(),
            &DangerousCombinationRegistry::new(),
        );
        assert!(has(&report.hazards, HazardCode::UnknownControllerPrimitive));
    }

    #[test]
    fn duplicate_primitive_is_blocked() {
        let plan = CompositionPlan::new("comp", [P::ShadowRun, P::ShadowRun]);
        let report = gate().evaluate(
            &plan,
            &CompositionMatrix::new(),
            &DangerousCombinationRegistry::new(),
        );
        assert!(has(&report.hazards, HazardCode::DuplicatePrimitive));
    }

    #[test]
    fn empty_composition_is_blocked() {
        let plan = CompositionPlan::new("comp", []);
        let report = gate().evaluate(
            &plan,
            &CompositionMatrix::new(),
            &DangerousCombinationRegistry::new(),
        );
        assert!(has(&report.hazards, HazardCode::EmptyComposition));
        assert!(!report.gate_passes);
    }

    #[test]
    fn synergies_are_detected() {
        let plan = CompositionPlan::new(
            "comp-synergy",
            [P::ShadowRun, P::ConcolicDifferential, P::ContractsAsCode],
        );
        let report = gate().evaluate(
            &plan,
            &canonical_matrix(),
            &DangerousCombinationRegistry::new(),
        );
        // shadow↔concolic and contracts↔shadow are synergies.
        assert!(report.summary.synergy_count >= 2);
        assert!(report.gate_passes);
    }

    #[test]
    fn report_is_deterministic() {
        let plan = CompositionPlan::new("comp", [P::EGraphOptimizer, P::MetamorphicOracle]);
        let matrix = canonical_matrix();
        let registry = canonical_registry();
        let first = gate().evaluate(&plan, &matrix, &registry);
        let second = gate().evaluate(&plan, &matrix, &registry);
        assert_eq!(first.report_id, second.report_id);
        assert_eq!(
            first.exported_json_stats.sha256,
            second.exported_json_stats.sha256
        );
        assert_eq!(first.hazards, second.hazards);
    }

    #[test]
    fn report_roundtrips_through_serde() {
        let plan = CompositionPlan::new(
            "comp",
            [P::CegisSynthesis, P::EGraphOptimizer, P::MetamorphicOracle],
        );
        let report = gate().evaluate(&plan, &canonical_matrix(), &canonical_registry());
        let json = serde_json::to_string(&report).unwrap();
        let restored: CompositionGateReport = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.report_id, report.report_id);
        assert_eq!(restored.hazards, report.hazards);
        assert_eq!(restored.mitigation_status, report.mitigation_status);
        assert_eq!(restored.gate_passes, report.gate_passes);
    }

    #[test]
    fn json_stats_checksum_is_self_consistent() {
        let plan = CompositionPlan::new("comp", [P::EGraphOptimizer, P::MetamorphicOracle]);
        let report = gate().evaluate(&plan, &canonical_matrix(), &canonical_registry());
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }

    #[test]
    fn replay_and_remediations_present() {
        let plan = CompositionPlan::new("comp-x", [P::EGraphOptimizer]);
        let report = gate().evaluate(&plan, &canonical_matrix(), &canonical_registry());
        assert!(report.replay_command.contains("composition-gate"));
        assert!(report.replay_command.contains("comp-x"));
        assert!(!report.blocking_remediations().is_empty());
    }

    #[test]
    fn dangerous_combination_normalizes_primitive_order() {
        let combo = DangerousCombination::new(
            "dc",
            [P::EGraphOptimizer, P::CegisSynthesis],
            RiskClass::High,
            "x",
            Vec::new(),
        );
        // Sorted: CegisSynthesis (index 1) before EGraphOptimizer (index 2).
        assert_eq!(combo.primitives[0], P::CegisSynthesis);
        assert!(combo.is_present_in(&[P::CegisSynthesis, P::EGraphOptimizer, P::ShadowRun]));
        assert!(!combo.is_present_in(&[P::CegisSynthesis]));
    }
}
