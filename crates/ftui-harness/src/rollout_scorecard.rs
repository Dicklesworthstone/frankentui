#![forbid(unsafe_code)]

//! Rollout go/no-go scorecard for the Asupersync migration (bd-2crbt).
//!
//! Combines shadow-run determinism evidence with benchmark-gate performance
//! evidence into a single structured verdict that operators can use for
//! release decisions.
//!
//! # Design
//!
//! A [`RolloutScorecard`] aggregates:
//! - One or more [`ShadowRunResult`]s proving frame-level determinism.
//! - An optional [`GateResult`] proving performance budgets are met.
//! - Policy-configurable thresholds (minimum shadow match ratio, required
//!   scenario coverage).
//!
//! The scorecard emits structured JSONL evidence and produces a [`RolloutVerdict`]
//! that is either `Go`, `NoGo`, or `Inconclusive` (not enough evidence).
//!
//! # Example
//!
//! ```ignore
//! use ftui_harness::rollout_scorecard::{RolloutScorecard, RolloutScorecardConfig};
//!
//! let config = RolloutScorecardConfig::default()
//!     .min_shadow_scenarios(3)
//!     .min_match_ratio(1.0);
//!
//! let mut scorecard = RolloutScorecard::new(config);
//! scorecard.add_shadow_result(shadow_result_1);
//! scorecard.add_shadow_result(shadow_result_2);
//! scorecard.set_benchmark_gate(gate_result);
//!
//! let verdict = scorecard.evaluate();
//! assert!(verdict.is_go());
//! ```

use crate::benchmark_gate::GateResult;
use crate::shadow_run::{ShadowRunResult, ShadowVerdict};
use ftui_runtime::effect_system::QueueTelemetry;

#[must_use]
fn normalized_ratio(ratio: f64) -> f64 {
    if ratio.is_finite() {
        ratio.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[must_use]
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[must_use]
fn json_string_array(values: &[String]) -> String {
    values
        .iter()
        .map(|value| json_string(value))
        .collect::<Vec<_>>()
        .join(",")
}

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for rollout scorecard evaluation.
#[derive(Debug, Clone)]
pub struct RolloutScorecardConfig {
    /// Minimum number of shadow-run scenarios required for a `Go` verdict.
    /// Default: 1.
    pub min_shadow_scenarios: usize,
    /// Minimum frame match ratio (0.0–1.0) across all shadow runs.
    /// Default: 1.0 (100% match required).
    pub min_match_ratio: f64,
    /// Whether a passing benchmark gate is required for `Go`.
    /// Default: false (benchmark evidence is informational, not blocking).
    pub require_benchmark_pass: bool,
}

impl Default for RolloutScorecardConfig {
    fn default() -> Self {
        Self {
            min_shadow_scenarios: 1,
            min_match_ratio: 1.0,
            require_benchmark_pass: false,
        }
    }
}

impl RolloutScorecardConfig {
    /// Set the minimum number of shadow scenarios required.
    #[must_use]
    pub fn min_shadow_scenarios(mut self, n: usize) -> Self {
        self.min_shadow_scenarios = n;
        self
    }

    /// Set the minimum frame match ratio (0.0–1.0).
    #[must_use]
    pub fn min_match_ratio(mut self, ratio: f64) -> Self {
        self.min_match_ratio = ratio.clamp(0.0, 1.0);
        self
    }

    /// Require a passing benchmark gate for `Go` verdict.
    #[must_use]
    pub fn require_benchmark_pass(mut self, required: bool) -> Self {
        self.require_benchmark_pass = required;
        self
    }
}

// ============================================================================
// Verdict
// ============================================================================

/// Go/no-go verdict from the rollout scorecard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RolloutVerdict {
    /// All evidence meets thresholds — safe to proceed with rollout.
    Go,
    /// Evidence shows determinism failure or performance regression.
    NoGo,
    /// Not enough evidence to make a decision.
    Inconclusive,
}

impl RolloutVerdict {
    /// Whether the verdict is `Go`.
    #[must_use]
    pub fn is_go(self) -> bool {
        matches!(self, Self::Go)
    }

    /// Human-readable label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Go => "GO",
            Self::NoGo => "NO-GO",
            Self::Inconclusive => "INCONCLUSIVE",
        }
    }
}

impl std::fmt::Display for RolloutVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

// ============================================================================
// Migration readiness rubric (bd-3bxhj.9.1)
// ============================================================================

/// Rollout stage controlled by the OpenTUI migration readiness rubric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MigrationRolloutStage {
    /// Internal trials against narrow fixtures.
    Alpha,
    /// Limited production-adjacent migrations with operator supervision.
    Beta,
    /// General availability for the declared support matrix.
    Ga,
}

impl MigrationRolloutStage {
    /// Stable machine-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Alpha => "alpha",
            Self::Beta => "beta",
            Self::Ga => "ga",
        }
    }

    /// Ordered stages from least to most permissive.
    pub const ALL: &'static [Self] = &[Self::Alpha, Self::Beta, Self::Ga];
}

/// Operator authority required to advance or hold a rollout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OperatorAuthority {
    /// Automated CI or scheduled release gate evaluator.
    Automation,
    /// On-call operator may hold or roll back a rollout.
    OnCall,
    /// Release owner may advance alpha/beta when evidence passes.
    ReleaseOwner,
    /// Maintainer quorum may approve GA.
    MaintainerQuorum,
}

impl OperatorAuthority {
    /// Stable machine-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Automation => "automation",
            Self::OnCall => "on-call",
            Self::ReleaseOwner => "release-owner",
            Self::MaintainerQuorum => "maintainer-quorum",
        }
    }
}

/// Emergency hold reason. Any active hold blocks stage advancement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmergencyHoldReason {
    /// Certification or shadow-run evidence detected semantic drift.
    CertificationRegression,
    /// Deterministic replay, hashes, or stage evidence diverged.
    DeterminismDivergence,
    /// Security, provenance, or sandbox policy was breached.
    SecurityIncident,
    /// Runtime reliability or SLO checks breached policy.
    ReliabilityBreach,
    /// Required deterministic artifacts are missing or unverifiable.
    MissingEvidence,
    /// Human operator explicitly held rollout.
    OperatorOverride,
}

impl EmergencyHoldReason {
    /// Stable machine-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::CertificationRegression => "certification-regression",
            Self::DeterminismDivergence => "determinism-divergence",
            Self::SecurityIncident => "security-incident",
            Self::ReliabilityBreach => "reliability-breach",
            Self::MissingEvidence => "missing-evidence",
            Self::OperatorOverride => "operator-override",
        }
    }
}

/// Active emergency hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmergencyHold {
    /// Why the rollout is held.
    pub reason: EmergencyHoldReason,
    /// Authority that placed the hold.
    pub authority: OperatorAuthority,
}

impl EmergencyHold {
    /// Construct an emergency hold.
    #[must_use]
    pub const fn new(reason: EmergencyHoldReason, authority: OperatorAuthority) -> Self {
        Self { reason, authority }
    }
}

/// Quantitative evidence snapshot used to evaluate rollout readiness.
#[derive(Debug, Clone)]
pub struct MigrationReadinessEvidence {
    /// Fraction of certification cases passing with accepted verdicts.
    pub certification_pass_ratio: f64,
    /// Fraction of declared corpus families covered by deterministic evidence.
    pub corpus_coverage_ratio: f64,
    /// Fraction of operational reliability checks passing.
    pub reliability_pass_ratio: f64,
    /// Count of deterministic artifact classes present and hash-verifiable.
    pub deterministic_artifact_count: usize,
    /// Whether the benchmark regression gate passed.
    pub benchmark_gate_passed: bool,
    /// Number of release-blocking unresolved defects.
    pub open_blocker_count: usize,
    /// Authority currently requesting the transition.
    pub operator_authority: OperatorAuthority,
    /// Optional active emergency hold.
    pub emergency_hold: Option<EmergencyHold>,
}

impl MigrationReadinessEvidence {
    /// Construct an empty evidence snapshot.
    #[must_use]
    pub const fn new(operator_authority: OperatorAuthority) -> Self {
        Self {
            certification_pass_ratio: 0.0,
            corpus_coverage_ratio: 0.0,
            reliability_pass_ratio: 0.0,
            deterministic_artifact_count: 0,
            benchmark_gate_passed: false,
            open_blocker_count: 0,
            operator_authority,
            emergency_hold: None,
        }
    }

    /// Set certification pass ratio.
    #[must_use]
    pub fn certification_pass_ratio(mut self, ratio: f64) -> Self {
        self.certification_pass_ratio = normalized_ratio(ratio);
        self
    }

    /// Set corpus coverage ratio.
    #[must_use]
    pub fn corpus_coverage_ratio(mut self, ratio: f64) -> Self {
        self.corpus_coverage_ratio = normalized_ratio(ratio);
        self
    }

    /// Set operational reliability pass ratio.
    #[must_use]
    pub fn reliability_pass_ratio(mut self, ratio: f64) -> Self {
        self.reliability_pass_ratio = normalized_ratio(ratio);
        self
    }

    /// Set deterministic artifact count.
    #[must_use]
    pub const fn deterministic_artifact_count(mut self, count: usize) -> Self {
        self.deterministic_artifact_count = count;
        self
    }

    /// Set benchmark gate result.
    #[must_use]
    pub const fn benchmark_gate_passed(mut self, passed: bool) -> Self {
        self.benchmark_gate_passed = passed;
        self
    }

    /// Set unresolved release blocker count.
    #[must_use]
    pub const fn open_blocker_count(mut self, count: usize) -> Self {
        self.open_blocker_count = count;
        self
    }

    /// Attach an emergency hold.
    #[must_use]
    pub const fn emergency_hold(mut self, hold: EmergencyHold) -> Self {
        self.emergency_hold = Some(hold);
        self
    }
}

/// Quantitative gate for one rollout stage.
#[derive(Debug, Clone)]
pub struct MigrationStageGate {
    /// Target stage.
    pub stage: MigrationRolloutStage,
    /// Minimum certification pass ratio.
    pub min_certification_pass_ratio: f64,
    /// Minimum deterministic corpus coverage.
    pub min_corpus_coverage_ratio: f64,
    /// Minimum operational reliability pass ratio.
    pub min_reliability_pass_ratio: f64,
    /// Minimum deterministic artifact classes required.
    pub min_deterministic_artifacts: usize,
    /// Whether benchmark gate evidence is required.
    pub require_benchmark_gate: bool,
    /// Maximum unresolved release blockers.
    pub max_open_blockers: usize,
    /// Minimum authority required to approve this stage.
    pub required_authority: OperatorAuthority,
}

impl MigrationStageGate {
    /// Construct a stage gate.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub const fn new(
        stage: MigrationRolloutStage,
        min_certification_pass_ratio: f64,
        min_corpus_coverage_ratio: f64,
        min_reliability_pass_ratio: f64,
        min_deterministic_artifacts: usize,
        require_benchmark_gate: bool,
        max_open_blockers: usize,
        required_authority: OperatorAuthority,
    ) -> Self {
        Self {
            stage,
            min_certification_pass_ratio,
            min_corpus_coverage_ratio,
            min_reliability_pass_ratio,
            min_deterministic_artifacts,
            require_benchmark_gate,
            max_open_blockers,
            required_authority,
        }
    }
}

/// Stage evaluation verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationReadinessVerdict {
    /// All quantitative gates and authority checks pass.
    Advance,
    /// Evidence is insufficient but no emergency hold is active.
    Hold,
    /// Active emergency hold blocks advancement.
    EmergencyHold,
}

impl MigrationReadinessVerdict {
    /// Stable machine-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Advance => "advance",
            Self::Hold => "hold",
            Self::EmergencyHold => "emergency-hold",
        }
    }
}

/// Result of evaluating one target stage.
#[derive(Debug, Clone)]
pub struct MigrationReadinessDecision {
    /// Evaluated target stage.
    pub stage: MigrationRolloutStage,
    /// Stage verdict.
    pub verdict: MigrationReadinessVerdict,
    /// Machine-readable reasons explaining holds.
    pub reasons: Vec<&'static str>,
}

impl MigrationReadinessDecision {
    /// Whether the stage may advance.
    #[must_use]
    pub fn may_advance(&self) -> bool {
        self.verdict == MigrationReadinessVerdict::Advance
    }

    /// Serialize the decision for release gate artifacts.
    #[must_use]
    pub fn to_json(&self) -> String {
        let reasons = self
            .reasons
            .iter()
            .map(|reason| format!("\"{reason}\""))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            concat!(
                "{{",
                "\"stage\":\"{stage}\",",
                "\"verdict\":\"{verdict}\",",
                "\"reasons\":[{reasons}]",
                "}}"
            ),
            stage = self.stage.label(),
            verdict = self.verdict.label(),
            reasons = reasons,
        )
    }
}

/// Default OpenTUI migration readiness rubric.
#[derive(Debug, Clone)]
pub struct MigrationReadinessRubric {
    gates: Vec<MigrationStageGate>,
}

impl MigrationReadinessRubric {
    /// Construct a rubric from gates.
    #[must_use]
    pub fn new(gates: Vec<MigrationStageGate>) -> Self {
        Self { gates }
    }

    /// Default policy for OpenTUI import rollout stages.
    #[must_use]
    pub fn opentui_default() -> Self {
        Self::new(vec![
            MigrationStageGate::new(
                MigrationRolloutStage::Alpha,
                0.90,
                0.25,
                0.95,
                3,
                true,
                2,
                OperatorAuthority::ReleaseOwner,
            ),
            MigrationStageGate::new(
                MigrationRolloutStage::Beta,
                0.97,
                0.60,
                0.98,
                5,
                true,
                0,
                OperatorAuthority::ReleaseOwner,
            ),
            MigrationStageGate::new(
                MigrationRolloutStage::Ga,
                1.00,
                0.90,
                0.995,
                7,
                true,
                0,
                OperatorAuthority::MaintainerQuorum,
            ),
        ])
    }

    /// Return the configured gate for a stage.
    #[must_use]
    pub fn gate(&self, stage: MigrationRolloutStage) -> Option<&MigrationStageGate> {
        self.gates.iter().find(|gate| gate.stage == stage)
    }

    /// Evaluate one target stage against the supplied evidence snapshot.
    #[must_use]
    pub fn evaluate(
        &self,
        stage: MigrationRolloutStage,
        evidence: &MigrationReadinessEvidence,
    ) -> MigrationReadinessDecision {
        let Some(gate) = self.gate(stage) else {
            return MigrationReadinessDecision {
                stage,
                verdict: MigrationReadinessVerdict::Hold,
                reasons: vec!["stage-gate-missing"],
            };
        };

        if let Some(hold) = evidence.emergency_hold {
            return MigrationReadinessDecision {
                stage,
                verdict: MigrationReadinessVerdict::EmergencyHold,
                reasons: vec![hold.reason.label()],
            };
        }

        // Fail closed on non-finite evidence: the fields are `pub f64`, so a
        // directly-constructed/deserialized NaN would otherwise compare false
        // against every `<` threshold and sail through the gate.
        fn fails_min(value: f64, min: f64) -> bool {
            !value.is_finite() || value < min
        }

        let mut reasons = Vec::new();
        if fails_min(
            evidence.certification_pass_ratio,
            gate.min_certification_pass_ratio,
        ) {
            reasons.push("certification-threshold");
        }
        if fails_min(
            evidence.corpus_coverage_ratio,
            gate.min_corpus_coverage_ratio,
        ) {
            reasons.push("corpus-coverage-threshold");
        }
        if fails_min(
            evidence.reliability_pass_ratio,
            gate.min_reliability_pass_ratio,
        ) {
            reasons.push("operational-reliability-threshold");
        }
        if evidence.deterministic_artifact_count < gate.min_deterministic_artifacts {
            reasons.push("deterministic-artifact-threshold");
        }
        if gate.require_benchmark_gate && !evidence.benchmark_gate_passed {
            reasons.push("benchmark-gate");
        }
        if evidence.open_blocker_count > gate.max_open_blockers {
            reasons.push("release-blockers");
        }
        if evidence.operator_authority < gate.required_authority {
            reasons.push("operator-authority");
        }

        let verdict = if reasons.is_empty() {
            MigrationReadinessVerdict::Advance
        } else {
            MigrationReadinessVerdict::Hold
        };
        MigrationReadinessDecision {
            stage,
            verdict,
            reasons,
        }
    }

    /// Highest stage that may advance for this evidence snapshot.
    #[must_use]
    pub fn recommended_stage(
        &self,
        evidence: &MigrationReadinessEvidence,
    ) -> Option<MigrationRolloutStage> {
        MigrationRolloutStage::ALL
            .iter()
            .rev()
            .copied()
            .find(|stage| self.evaluate(*stage, evidence).may_advance())
    }
}

// ============================================================================
// Migration release gate evaluator (bd-3bxhj.9.2)
// ============================================================================

/// Release gate operating mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationReleaseGateMode {
    /// Evaluate and emit decision evidence without blocking release.
    DryRun,
    /// Evaluate and block release when any required clause fails.
    Enforce,
}

impl MigrationReleaseGateMode {
    /// Stable machine-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::DryRun => "dry-run",
            Self::Enforce => "enforce",
        }
    }
}

/// Release gate verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationReleaseGateVerdict {
    /// Every release gate clause passed.
    Pass,
    /// At least one release gate clause failed.
    Fail,
}

impl MigrationReleaseGateVerdict {
    /// Stable machine-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
        }
    }
}

/// Immutable artifact evidence referenced by release gate inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReleaseGateArtifact {
    /// Stable artifact identifier used by release evidence.
    pub artifact_id: String,
    /// Artifact class, for example `certification`, `determinism`, or `performance`.
    pub kind: String,
    /// Content digest. Must be a `sha256:` digest to count as immutable.
    pub digest: String,
    /// Artifact location or content-addressed URI.
    pub uri: String,
}

impl MigrationReleaseGateArtifact {
    /// Construct release gate artifact evidence.
    #[must_use]
    pub fn new(artifact_id: &str, kind: &str, digest: &str, uri: &str) -> Self {
        Self {
            artifact_id: artifact_id.to_string(),
            kind: kind.to_string(),
            digest: digest.to_string(),
            uri: uri.to_string(),
        }
    }

    /// Whether this artifact is content-addressed enough for gate traceability.
    #[must_use]
    pub fn is_immutable(&self) -> bool {
        let Some(hex_digest) = self.digest.strip_prefix("sha256:") else {
            return false;
        };
        !self.artifact_id.is_empty()
            && !self.kind.is_empty()
            && !self.uri.is_empty()
            && hex_digest.len() == 64
            && hex_digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    }
}

/// Evidence snapshot consumed by the migration release gate evaluator.
#[derive(Debug, Clone)]
pub struct MigrationReleaseGateEvidence {
    /// Migration service version under evaluation.
    pub migration_version: String,
    /// Readiness evidence for the target rollout stage.
    pub readiness: MigrationReadinessEvidence,
    /// Fraction of deterministic/certification comparison metrics that passed.
    pub determinism_pass_ratio: f64,
    /// Whether the performance budget gate passed.
    pub performance_budget_passed: bool,
    /// Count of unresolved critical migration capability gaps.
    pub unresolved_critical_gap_count: usize,
    /// Immutable artifacts proving each input clause.
    pub artifacts: Vec<MigrationReleaseGateArtifact>,
}

impl MigrationReleaseGateEvidence {
    /// Construct release gate evidence for a migration version.
    #[must_use]
    pub fn new(migration_version: &str, readiness: MigrationReadinessEvidence) -> Self {
        Self {
            migration_version: migration_version.to_string(),
            readiness,
            determinism_pass_ratio: 0.0,
            performance_budget_passed: false,
            unresolved_critical_gap_count: 0,
            artifacts: Vec::new(),
        }
    }

    /// Set deterministic comparison pass ratio.
    #[must_use]
    pub fn determinism_pass_ratio(mut self, ratio: f64) -> Self {
        self.determinism_pass_ratio = normalized_ratio(ratio);
        self
    }

    /// Set performance budget gate result.
    #[must_use]
    pub const fn performance_budget_passed(mut self, passed: bool) -> Self {
        self.performance_budget_passed = passed;
        self
    }

    /// Set unresolved critical capability gap count.
    #[must_use]
    pub const fn unresolved_critical_gap_count(mut self, count: usize) -> Self {
        self.unresolved_critical_gap_count = count;
        self
    }

    /// Attach immutable artifact evidence.
    #[must_use]
    pub fn artifact(mut self, artifact: MigrationReleaseGateArtifact) -> Self {
        self.artifacts.push(artifact);
        self
    }
}

/// Release gate policy for one target stage.
#[derive(Debug, Clone)]
pub struct MigrationReleaseGatePolicy {
    /// Gate operating mode.
    pub mode: MigrationReleaseGateMode,
    /// Target rollout stage.
    pub target_stage: MigrationRolloutStage,
    /// Minimum certification pass ratio.
    pub min_certification_pass_ratio: f64,
    /// Minimum deterministic comparison pass ratio.
    pub min_determinism_pass_ratio: f64,
    /// Whether the performance budget gate is release-blocking.
    pub require_performance_budget_pass: bool,
    /// Maximum unresolved critical capability gaps.
    pub max_unresolved_critical_gaps: usize,
    /// Minimum number of immutable artifacts required.
    pub min_traceable_artifacts: usize,
    /// Artifact kinds that must be present.
    pub required_artifact_kinds: Vec<&'static str>,
}

impl MigrationReleaseGatePolicy {
    /// Default release gate policy for a target stage.
    #[must_use]
    pub fn for_stage(target_stage: MigrationRolloutStage, mode: MigrationReleaseGateMode) -> Self {
        let (
            min_certification_pass_ratio,
            min_determinism_pass_ratio,
            max_unresolved_critical_gaps,
            min_traceable_artifacts,
        ) = match target_stage {
            MigrationRolloutStage::Alpha => (0.90, 0.99, 2, 5),
            MigrationRolloutStage::Beta => (0.97, 1.00, 0, 6),
            MigrationRolloutStage::Ga => (1.00, 1.00, 0, 8),
        };
        Self {
            mode,
            target_stage,
            min_certification_pass_ratio,
            min_determinism_pass_ratio,
            require_performance_budget_pass: true,
            max_unresolved_critical_gaps,
            min_traceable_artifacts,
            required_artifact_kinds: vec![
                "certification",
                "critical-gaps",
                "determinism",
                "performance",
                "readiness",
            ],
        }
    }

    /// Switch the same policy thresholds to a different mode.
    #[must_use]
    pub const fn mode(mut self, mode: MigrationReleaseGateMode) -> Self {
        self.mode = mode;
        self
    }
}

/// Clause-level release gate result.
#[derive(Debug, Clone)]
pub struct MigrationReleaseGateClauseResult {
    /// Machine-readable clause name.
    pub clause: &'static str,
    /// Whether the clause passed.
    pub passed: bool,
    /// Human-readable reason with threshold context.
    pub reason: String,
    /// Artifact ids that support this clause.
    pub artifact_ids: Vec<String>,
}

impl MigrationReleaseGateClauseResult {
    #[must_use]
    fn pass(clause: &'static str, reason: String, artifact_ids: Vec<String>) -> Self {
        Self {
            clause,
            passed: true,
            reason,
            artifact_ids,
        }
    }

    #[must_use]
    fn fail(clause: &'static str, reason: String, artifact_ids: Vec<String>) -> Self {
        Self {
            clause,
            passed: false,
            reason,
            artifact_ids,
        }
    }

    #[must_use]
    fn to_json(&self) -> String {
        format!(
            concat!(
                "{{",
                "\"clause\":{clause},",
                "\"passed\":{passed},",
                "\"reason\":{reason},",
                "\"artifact_ids\":[{artifact_ids}]",
                "}}"
            ),
            clause = json_string(self.clause),
            passed = self.passed,
            reason = json_string(&self.reason),
            artifact_ids = json_string_array(&self.artifact_ids),
        )
    }
}

/// Release gate decision for one migration service version.
#[derive(Debug, Clone)]
pub struct MigrationReleaseGateDecision {
    /// Migration service version evaluated.
    pub migration_version: String,
    /// Gate operating mode.
    pub mode: MigrationReleaseGateMode,
    /// Target rollout stage.
    pub target_stage: MigrationRolloutStage,
    /// Overall pass/fail verdict.
    pub verdict: MigrationReleaseGateVerdict,
    /// Clause-level evidence and reasoning.
    pub clauses: Vec<MigrationReleaseGateClauseResult>,
}

impl MigrationReleaseGateDecision {
    /// Whether every release gate clause passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.verdict == MigrationReleaseGateVerdict::Pass
    }

    /// Whether this decision blocks release.
    #[must_use]
    pub fn blocks_release(&self) -> bool {
        self.mode == MigrationReleaseGateMode::Enforce && !self.passed()
    }

    /// Failed clause names.
    #[must_use]
    pub fn failed_clauses(&self) -> Vec<&'static str> {
        self.clauses
            .iter()
            .filter(|clause| !clause.passed)
            .map(|clause| clause.clause)
            .collect()
    }

    /// Serialize the release gate decision for CI and operator artifacts.
    #[must_use]
    pub fn to_json(&self) -> String {
        let clauses = self
            .clauses
            .iter()
            .map(MigrationReleaseGateClauseResult::to_json)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            concat!(
                "{{",
                "\"schema_version\":\"1.0.0\",",
                "\"migration_version\":{version},",
                "\"mode\":\"{mode}\",",
                "\"target_stage\":\"{stage}\",",
                "\"verdict\":\"{verdict}\",",
                "\"blocks_release\":{blocks_release},",
                "\"clauses\":[{clauses}]",
                "}}"
            ),
            version = json_string(&self.migration_version),
            mode = self.mode.label(),
            stage = self.target_stage.label(),
            verdict = self.verdict.label(),
            blocks_release = self.blocks_release(),
            clauses = clauses,
        )
    }
}

/// Evaluates migration release eligibility from readiness, determinism,
/// performance, blocker, and artifact evidence.
#[derive(Debug, Clone)]
pub struct MigrationReleaseGateEvaluator {
    policy: MigrationReleaseGatePolicy,
    readiness_rubric: MigrationReadinessRubric,
}

impl MigrationReleaseGateEvaluator {
    /// Construct an evaluator using the default readiness rubric.
    #[must_use]
    pub fn new(policy: MigrationReleaseGatePolicy) -> Self {
        Self {
            policy,
            readiness_rubric: MigrationReadinessRubric::opentui_default(),
        }
    }

    /// Override the readiness rubric.
    #[must_use]
    pub fn with_readiness_rubric(mut self, rubric: MigrationReadinessRubric) -> Self {
        self.readiness_rubric = rubric;
        self
    }

    /// Evaluate release eligibility.
    #[must_use]
    pub fn evaluate(
        &self,
        evidence: &MigrationReleaseGateEvidence,
    ) -> MigrationReleaseGateDecision {
        let mut clauses = Vec::new();
        let readiness_decision = self
            .readiness_rubric
            .evaluate(self.policy.target_stage, &evidence.readiness);
        let readiness_artifact_ids = artifact_ids_for_kind(evidence, "readiness");
        if readiness_decision.may_advance() {
            clauses.push(MigrationReleaseGateClauseResult::pass(
                "readiness",
                format!(
                    "target stage {} readiness advanced",
                    self.policy.target_stage.label()
                ),
                readiness_artifact_ids,
            ));
        } else {
            clauses.push(MigrationReleaseGateClauseResult::fail(
                "readiness",
                format!("readiness held: {}", readiness_decision.reasons.join(",")),
                readiness_artifact_ids,
            ));
        }

        clauses.push(compare_ratio_clause(
            "certification",
            evidence.readiness.certification_pass_ratio,
            self.policy.min_certification_pass_ratio,
            artifact_ids_for_kind(evidence, "certification"),
        ));
        clauses.push(compare_ratio_clause(
            "determinism",
            evidence.determinism_pass_ratio,
            self.policy.min_determinism_pass_ratio,
            artifact_ids_for_kind(evidence, "determinism"),
        ));

        let performance_ids = artifact_ids_for_kind(evidence, "performance");
        if !self.policy.require_performance_budget_pass || evidence.performance_budget_passed {
            clauses.push(MigrationReleaseGateClauseResult::pass(
                "performance-budget",
                "performance budget gate passed".to_string(),
                performance_ids,
            ));
        } else {
            clauses.push(MigrationReleaseGateClauseResult::fail(
                "performance-budget",
                "performance budget gate failed".to_string(),
                performance_ids,
            ));
        }

        let critical_gap_ids = artifact_ids_for_kind(evidence, "critical-gaps");
        if evidence.unresolved_critical_gap_count <= self.policy.max_unresolved_critical_gaps {
            clauses.push(MigrationReleaseGateClauseResult::pass(
                "critical-gaps",
                format!(
                    "{} critical gaps <= {} allowed",
                    evidence.unresolved_critical_gap_count,
                    self.policy.max_unresolved_critical_gaps
                ),
                critical_gap_ids,
            ));
        } else {
            clauses.push(MigrationReleaseGateClauseResult::fail(
                "critical-gaps",
                format!(
                    "{} critical gaps > {} allowed",
                    evidence.unresolved_critical_gap_count,
                    self.policy.max_unresolved_critical_gaps
                ),
                critical_gap_ids,
            ));
        }

        clauses.push(self.evaluate_artifact_traceability(evidence));

        let verdict = if clauses.iter().all(|clause| clause.passed) {
            MigrationReleaseGateVerdict::Pass
        } else {
            MigrationReleaseGateVerdict::Fail
        };
        MigrationReleaseGateDecision {
            migration_version: evidence.migration_version.clone(),
            mode: self.policy.mode,
            target_stage: self.policy.target_stage,
            verdict,
            clauses,
        }
    }

    #[must_use]
    fn evaluate_artifact_traceability(
        &self,
        evidence: &MigrationReleaseGateEvidence,
    ) -> MigrationReleaseGateClauseResult {
        let traceable_ids = evidence
            .artifacts
            .iter()
            .filter(|artifact| artifact.is_immutable())
            .map(|artifact| artifact.artifact_id.clone())
            .collect::<Vec<_>>();
        let invalid_ids = evidence
            .artifacts
            .iter()
            .filter(|artifact| !artifact.is_immutable())
            .map(|artifact| artifact.artifact_id.clone())
            .collect::<Vec<_>>();
        let missing_kinds = self
            .policy
            .required_artifact_kinds
            .iter()
            .copied()
            .filter(|kind| {
                !evidence
                    .artifacts
                    .iter()
                    .any(|artifact| artifact.kind == *kind && artifact.is_immutable())
            })
            .collect::<Vec<_>>();

        if traceable_ids.len() >= self.policy.min_traceable_artifacts
            && invalid_ids.is_empty()
            && missing_kinds.is_empty()
        {
            return MigrationReleaseGateClauseResult::pass(
                "artifact-traceability",
                format!(
                    "{} immutable artifacts cover required input kinds",
                    traceable_ids.len()
                ),
                traceable_ids,
            );
        }

        let mut reasons = Vec::new();
        if traceable_ids.len() < self.policy.min_traceable_artifacts {
            reasons.push(format!(
                "{} immutable artifacts < {} required",
                traceable_ids.len(),
                self.policy.min_traceable_artifacts
            ));
        }
        if !invalid_ids.is_empty() {
            reasons.push(format!(
                "invalid immutable references: {}",
                invalid_ids.join(",")
            ));
        }
        if !missing_kinds.is_empty() {
            reasons.push(format!(
                "missing artifact kinds: {}",
                missing_kinds.join(",")
            ));
        }
        MigrationReleaseGateClauseResult::fail(
            "artifact-traceability",
            reasons.join("; "),
            traceable_ids,
        )
    }
}

#[must_use]
fn artifact_ids_for_kind(evidence: &MigrationReleaseGateEvidence, kind: &str) -> Vec<String> {
    evidence
        .artifacts
        .iter()
        .filter(|artifact| artifact.kind == kind && artifact.is_immutable())
        .map(|artifact| artifact.artifact_id.clone())
        .collect()
}

#[must_use]
fn compare_ratio_clause(
    clause: &'static str,
    actual: f64,
    required: f64,
    artifact_ids: Vec<String>,
) -> MigrationReleaseGateClauseResult {
    if actual >= required {
        MigrationReleaseGateClauseResult::pass(
            clause,
            format!("{actual:.6} >= {required:.6} required"),
            artifact_ids,
        )
    } else {
        MigrationReleaseGateClauseResult::fail(
            clause,
            format!("{actual:.6} < {required:.6} required"),
            artifact_ids,
        )
    }
}

// ============================================================================
// Migration incident response and rollback (bd-3bxhj.9.7)
// ============================================================================

/// Class of migration incident observed in production-like rollout.
///
/// Each class maps to a default severity, a rollout [`EmergencyHoldReason`],
/// and a canonical rollback spine so that the operational response is
/// deterministic rather than improvised during an incident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MigrationIncidentClass {
    /// Migrated app behaves differently from the source under equivalent input.
    SemanticRegression,
    /// Deterministic replay, hashes, or stage evidence diverged after release.
    DeterminismDivergence,
    /// Generated app breached its p95/p99 performance budget in the field.
    PerformanceRegression,
    /// An unsupported source construct shipped without a stub or guard.
    CapabilityGapEscape,
    /// Sandbox, provenance, or secret-handling policy was violated.
    SecurityBreach,
    /// Certification reported pass but field evidence contradicts the verdict.
    CertificationFalsePass,
    /// A rollback attempt itself failed and manual recovery is required.
    RollbackFailure,
}

impl MigrationIncidentClass {
    /// Stable machine-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::SemanticRegression => "semantic-regression",
            Self::DeterminismDivergence => "determinism-divergence",
            Self::PerformanceRegression => "performance-regression",
            Self::CapabilityGapEscape => "capability-gap-escape",
            Self::SecurityBreach => "security-breach",
            Self::CertificationFalsePass => "certification-false-pass",
            Self::RollbackFailure => "rollback-failure",
        }
    }

    /// All incident classes in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::SemanticRegression,
        Self::DeterminismDivergence,
        Self::PerformanceRegression,
        Self::CapabilityGapEscape,
        Self::SecurityBreach,
        Self::CertificationFalsePass,
        Self::RollbackFailure,
    ];

    /// Default severity assigned before signal-driven escalation.
    #[must_use]
    pub const fn default_severity(self) -> MigrationIncidentSeverity {
        match self {
            Self::DeterminismDivergence
            | Self::SecurityBreach
            | Self::CertificationFalsePass
            | Self::RollbackFailure => MigrationIncidentSeverity::Sev1,
            Self::SemanticRegression | Self::CapabilityGapEscape => MigrationIncidentSeverity::Sev2,
            Self::PerformanceRegression => MigrationIncidentSeverity::Sev3,
        }
    }

    /// Rollout emergency-hold reason triggered by this incident class.
    #[must_use]
    pub const fn emergency_hold_reason(self) -> EmergencyHoldReason {
        match self {
            Self::SemanticRegression | Self::CapabilityGapEscape | Self::CertificationFalsePass => {
                EmergencyHoldReason::CertificationRegression
            }
            Self::DeterminismDivergence => EmergencyHoldReason::DeterminismDivergence,
            Self::SecurityBreach => EmergencyHoldReason::SecurityIncident,
            Self::PerformanceRegression | Self::RollbackFailure => {
                EmergencyHoldReason::ReliabilityBreach
            }
        }
    }
}

/// Incident severity level. `Sev1` is the most severe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationIncidentSeverity {
    /// Critical: data/safety/determinism integrity at risk; rollback now.
    Sev1,
    /// High: visible correctness regression; rollback before further promotion.
    Sev2,
    /// Moderate: degraded but bounded impact; release-owner review required.
    Sev3,
    /// Low: informational; track and remediate without emergency action.
    Sev4,
}

impl MigrationIncidentSeverity {
    /// Stable machine-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sev1 => "sev1",
            Self::Sev2 => "sev2",
            Self::Sev3 => "sev3",
            Self::Sev4 => "sev4",
        }
    }

    /// Escalation rank where a higher value is more severe.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Sev1 => 4,
            Self::Sev2 => 3,
            Self::Sev3 => 2,
            Self::Sev4 => 1,
        }
    }

    /// Acknowledgement deadline in minutes.
    #[must_use]
    pub const fn ack_deadline_minutes(self) -> u32 {
        match self {
            Self::Sev1 => 15,
            Self::Sev2 => 60,
            Self::Sev3 => 240,
            Self::Sev4 => 1440,
        }
    }

    /// Whether this severity demands an immediate rollback before triage.
    #[must_use]
    pub const fn requires_immediate_rollback(self) -> bool {
        matches!(self, Self::Sev1 | Self::Sev2)
    }

    /// Operator authority empowered to execute the rollback at this severity.
    #[must_use]
    pub const fn rollback_authority(self) -> OperatorAuthority {
        match self {
            // High-severity incidents empower the on-call operator to act now.
            Self::Sev1 | Self::Sev2 => OperatorAuthority::OnCall,
            Self::Sev3 | Self::Sev4 => OperatorAuthority::ReleaseOwner,
        }
    }

    /// Return the more severe of two severities.
    #[must_use]
    pub const fn escalate(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

/// Observed signal contributing to an incident, optionally escalating severity.
#[derive(Debug, Clone)]
pub struct MigrationIncidentSignal {
    /// Machine-readable signal code, for example `p99-budget-breach`.
    pub code: String,
    /// Human-readable detail.
    pub detail: String,
    /// Severity this signal escalates the incident to, if any.
    pub escalates_to: Option<MigrationIncidentSeverity>,
}

impl MigrationIncidentSignal {
    /// Construct an informational signal that does not escalate severity.
    #[must_use]
    pub fn new(code: &str, detail: &str) -> Self {
        Self {
            code: code.to_string(),
            detail: detail.to_string(),
            escalates_to: None,
        }
    }

    /// Attach a severity escalation to this signal.
    #[must_use]
    pub const fn escalates_to(mut self, severity: MigrationIncidentSeverity) -> Self {
        self.escalates_to = Some(severity);
        self
    }

    #[must_use]
    fn to_json(&self) -> String {
        let escalates = match self.escalates_to {
            Some(severity) => format!("\"{}\"", severity.label()),
            None => "null".to_string(),
        };
        format!(
            concat!(
                "{{",
                "\"code\":{code},",
                "\"detail\":{detail},",
                "\"escalates_to\":{escalates}",
                "}}"
            ),
            code = json_string(&self.code),
            detail = json_string(&self.detail),
            escalates = escalates,
        )
    }
}

/// Incident report consumed by the response playbook.
#[derive(Debug, Clone)]
pub struct MigrationIncidentReport {
    /// Stable incident identifier.
    pub incident_id: String,
    /// Incident class.
    pub class: MigrationIncidentClass,
    /// Rollout stage where the incident surfaced.
    pub detected_stage: MigrationRolloutStage,
    /// Comparator id linking the incident to its baseline contract.
    pub comparator_id: String,
    /// Claim id linking the incident to the uplift/migration claim.
    pub claim_id: String,
    /// Observed signals, some of which may escalate severity.
    pub signals: Vec<MigrationIncidentSignal>,
    /// Immutable artifacts available for an artifact-driven rollback.
    pub available_artifacts: Vec<MigrationReleaseGateArtifact>,
}

impl MigrationIncidentReport {
    /// Construct a report with no signals or artifacts attached yet.
    #[must_use]
    pub fn new(
        incident_id: &str,
        class: MigrationIncidentClass,
        detected_stage: MigrationRolloutStage,
    ) -> Self {
        Self {
            incident_id: incident_id.to_string(),
            class,
            detected_stage,
            comparator_id: "n/a".to_string(),
            claim_id: "n/a".to_string(),
            signals: Vec::new(),
            available_artifacts: Vec::new(),
        }
    }

    /// Set the baseline comparator id for traceability.
    #[must_use]
    pub fn comparator_id(mut self, comparator_id: &str) -> Self {
        self.comparator_id = comparator_id.to_string();
        self
    }

    /// Set the uplift/migration claim id for traceability.
    #[must_use]
    pub fn claim_id(mut self, claim_id: &str) -> Self {
        self.claim_id = claim_id.to_string();
        self
    }

    /// Attach an observed signal.
    #[must_use]
    pub fn signal(mut self, signal: MigrationIncidentSignal) -> Self {
        self.signals.push(signal);
        self
    }

    /// Attach an immutable rollback artifact.
    #[must_use]
    pub fn artifact(mut self, artifact: MigrationReleaseGateArtifact) -> Self {
        self.available_artifacts.push(artifact);
        self
    }

    /// Effective severity after folding in every signal escalation.
    #[must_use]
    pub fn effective_severity(&self) -> MigrationIncidentSeverity {
        self.signals
            .iter()
            .filter_map(|signal| signal.escalates_to)
            .fold(self.class.default_severity(), |acc, escalated| {
                acc.escalate(escalated)
            })
    }
}

/// One ordered, artifact-anchored rollback step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationRollbackStep {
    /// 1-based execution order.
    pub order: usize,
    /// Machine-readable action code.
    pub action: &'static str,
    /// Operator-facing description.
    pub description: &'static str,
    /// Artifact kind this step consumes, or empty when none is required.
    pub required_artifact_kind: &'static str,
    /// Resolved immutable artifact id, `None` until resolved or if missing.
    pub artifact_id: Option<String>,
}

impl MigrationRollbackStep {
    #[must_use]
    const fn template(
        order: usize,
        action: &'static str,
        description: &'static str,
        required_artifact_kind: &'static str,
    ) -> Self {
        Self {
            order,
            action,
            description,
            required_artifact_kind,
            artifact_id: None,
        }
    }

    /// Whether this step has every artifact it requires.
    #[must_use]
    pub fn is_resolved(&self) -> bool {
        self.required_artifact_kind.is_empty() || self.artifact_id.is_some()
    }

    #[must_use]
    fn to_json(&self) -> String {
        let artifact_id = match &self.artifact_id {
            Some(id) => json_string(id),
            None => "null".to_string(),
        };
        format!(
            concat!(
                "{{",
                "\"order\":{order},",
                "\"action\":{action},",
                "\"description\":{description},",
                "\"required_artifact_kind\":{kind},",
                "\"artifact_id\":{artifact_id},",
                "\"resolved\":{resolved}",
                "}}"
            ),
            order = self.order,
            action = json_string(self.action),
            description = json_string(self.description),
            kind = json_string(self.required_artifact_kind),
            artifact_id = artifact_id,
            resolved = self.is_resolved(),
        )
    }
}

/// Rollback readiness verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationRollbackReadinessVerdict {
    /// Every artifact-bound rollback step resolved to an immutable artifact.
    Ready,
    /// At least one required artifact is missing or not immutable.
    Blocked,
}

impl MigrationRollbackReadinessVerdict {
    /// Stable machine-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Blocked => "blocked",
        }
    }
}

/// Result of checking whether a rollback plan can execute deterministically.
#[derive(Debug, Clone)]
pub struct MigrationRollbackReadiness {
    /// Overall readiness verdict.
    pub verdict: MigrationRollbackReadinessVerdict,
    /// Number of rollback steps total.
    pub total_steps: usize,
    /// Number of rollback steps with their required artifact resolved.
    pub resolved_steps: usize,
    /// Artifact kinds that are required but missing or not immutable.
    pub missing_artifact_kinds: Vec<&'static str>,
}

impl MigrationRollbackReadiness {
    /// Whether the rollback can execute immediately.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.verdict == MigrationRollbackReadinessVerdict::Ready
    }

    #[must_use]
    fn to_json(&self) -> String {
        let missing = self
            .missing_artifact_kinds
            .iter()
            .map(|kind| json_string(kind))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            concat!(
                "{{",
                "\"verdict\":\"{verdict}\",",
                "\"total_steps\":{total},",
                "\"resolved_steps\":{resolved},",
                "\"missing_artifact_kinds\":[{missing}]",
                "}}"
            ),
            verdict = self.verdict.label(),
            total = self.total_steps,
            resolved = self.resolved_steps,
            missing = missing,
        )
    }
}

/// Deterministic, artifact-driven rollback plan for one incident class.
#[derive(Debug, Clone)]
pub struct MigrationRollbackPlan {
    /// Incident class this plan rolls back.
    pub incident_class: MigrationIncidentClass,
    /// Rollout stage the rollback targets.
    pub target_stage: MigrationRolloutStage,
    /// Ordered rollback steps.
    pub steps: Vec<MigrationRollbackStep>,
}

impl MigrationRollbackPlan {
    /// Canonical rollback plan for an incident class and stage.
    ///
    /// The spine (steps 1–6) is shared by every class; security and
    /// rollback-failure incidents append a class-specific recovery step.
    /// Artifact ids are left unresolved until [`resolve`](Self::resolve).
    #[must_use]
    pub fn default_for(
        incident_class: MigrationIncidentClass,
        target_stage: MigrationRolloutStage,
    ) -> Self {
        let mut steps = vec![
            MigrationRollbackStep::template(
                1,
                "halt-promotion",
                "Freeze promotion and mark the affected migration version non-deployable.",
                "release-gate-decision",
            ),
            MigrationRollbackStep::template(
                2,
                "quarantine-version",
                "Quarantine the failing version using its locked source snapshot.",
                "source-snapshot",
            ),
            MigrationRollbackStep::template(
                3,
                "restore-last-good",
                "Restore the last-good migration release artifact.",
                "last-good-release",
            ),
            MigrationRollbackStep::template(
                4,
                "verify-determinism",
                "Re-verify determinism hashes against the recorded baseline.",
                "determinism-baseline",
            ),
            MigrationRollbackStep::template(
                5,
                "reverify-certification",
                "Re-run the certification smoke against the restored release.",
                "certification-report",
            ),
        ];
        match incident_class {
            MigrationIncidentClass::SecurityBreach => {
                steps.push(MigrationRollbackStep::template(
                    6,
                    "rotate-exposed-credentials",
                    "Rotate any credentials or tokens exposed by the breach.",
                    "secret-rotation-runbook",
                ));
            }
            MigrationIncidentClass::RollbackFailure => {
                steps.push(MigrationRollbackStep::template(
                    6,
                    "escalate-manual-recovery",
                    "Escalate to maintainer-led manual recovery using the recovery runbook.",
                    "manual-recovery-runbook",
                ));
            }
            _ => {}
        }
        // Final, artifact-free step: always record the postmortem.
        let order = steps.len() + 1;
        steps.push(MigrationRollbackStep::template(
            order,
            "record-postmortem",
            "Open a postmortem record linking the incident to backlog and prevention.",
            "",
        ));
        Self {
            incident_class,
            target_stage,
            steps,
        }
    }

    /// Resolve each artifact-bound step against the available artifacts.
    ///
    /// Resolution is deterministic: among immutable artifacts of the required
    /// kind, the lexicographically smallest `artifact_id` is selected.
    #[must_use]
    pub fn resolve(mut self, artifacts: &[MigrationReleaseGateArtifact]) -> Self {
        for step in &mut self.steps {
            if step.required_artifact_kind.is_empty() {
                continue;
            }
            let mut candidates = artifacts
                .iter()
                .filter(|artifact| {
                    artifact.kind == step.required_artifact_kind && artifact.is_immutable()
                })
                .map(|artifact| artifact.artifact_id.clone())
                .collect::<Vec<_>>();
            candidates.sort();
            step.artifact_id = candidates.into_iter().next();
        }
        self
    }

    /// Compute rollback readiness for the (resolved) plan.
    #[must_use]
    pub fn readiness(&self) -> MigrationRollbackReadiness {
        let resolved_steps = self.steps.iter().filter(|step| step.is_resolved()).count();
        let mut missing_artifact_kinds = self
            .steps
            .iter()
            .filter(|step| !step.is_resolved())
            .map(|step| step.required_artifact_kind)
            .collect::<Vec<_>>();
        missing_artifact_kinds.sort_unstable();
        missing_artifact_kinds.dedup();
        let verdict = if missing_artifact_kinds.is_empty() {
            MigrationRollbackReadinessVerdict::Ready
        } else {
            MigrationRollbackReadinessVerdict::Blocked
        };
        MigrationRollbackReadiness {
            verdict,
            total_steps: self.steps.len(),
            resolved_steps,
            missing_artifact_kinds,
        }
    }

    #[must_use]
    fn to_json(&self) -> String {
        let steps = self
            .steps
            .iter()
            .map(MigrationRollbackStep::to_json)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            concat!(
                "{{",
                "\"incident_class\":\"{class}\",",
                "\"target_stage\":\"{stage}\",",
                "\"steps\":[{steps}]",
                "}}"
            ),
            class = self.incident_class.label(),
            stage = self.target_stage.label(),
            steps = steps,
        )
    }
}

/// Postmortem template linking an incident to backlog and prevention actions.
#[derive(Debug, Clone)]
pub struct MigrationPostmortemTemplate {
    /// Incident this postmortem documents.
    pub incident_id: String,
    /// Incident class.
    pub incident_class: MigrationIncidentClass,
    /// Effective severity.
    pub severity: MigrationIncidentSeverity,
    /// Comparator id for traceability.
    pub comparator_id: String,
    /// Claim id for traceability.
    pub claim_id: String,
    /// Canonical timeline prompts the author must fill in.
    pub timeline_prompts: Vec<&'static str>,
    /// Backlog item ids linked to this incident.
    pub backlog_item_ids: Vec<String>,
    /// Prevention actions seeded for the incident class plus author additions.
    pub prevention_actions: Vec<String>,
}

impl MigrationPostmortemTemplate {
    /// Build a postmortem template for an incident report and severity.
    ///
    /// Prevention actions are seeded deterministically from the incident class.
    #[must_use]
    pub fn for_incident(
        report: &MigrationIncidentReport,
        severity: MigrationIncidentSeverity,
    ) -> Self {
        Self {
            incident_id: report.incident_id.clone(),
            incident_class: report.class,
            severity,
            comparator_id: report.comparator_id.clone(),
            claim_id: report.claim_id.clone(),
            timeline_prompts: vec![
                "detection: how and when the incident was detected",
                "impact: which migrations/versions were affected and for how long",
                "root-cause: the verified causal chain, not the first symptom",
                "resolution: the rollback executed and how recovery was verified",
            ],
            backlog_item_ids: Vec::new(),
            prevention_actions: default_prevention_actions(report.class),
        }
    }

    /// Link a backlog item id to this postmortem.
    #[must_use]
    pub fn backlog_item(mut self, backlog_item_id: &str) -> Self {
        self.backlog_item_ids.push(backlog_item_id.to_string());
        self
    }

    /// Add an extra prevention action.
    #[must_use]
    pub fn prevention_action(mut self, action: &str) -> Self {
        self.prevention_actions.push(action.to_string());
        self
    }

    /// Whether the postmortem links at least one backlog item.
    #[must_use]
    pub fn links_backlog(&self) -> bool {
        !self.backlog_item_ids.is_empty()
    }

    #[must_use]
    fn to_json(&self) -> String {
        let prompts = self
            .timeline_prompts
            .iter()
            .map(|prompt| json_string(prompt))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            concat!(
                "{{",
                "\"incident_id\":{incident_id},",
                "\"incident_class\":\"{class}\",",
                "\"severity\":\"{severity}\",",
                "\"comparator_id\":{comparator_id},",
                "\"claim_id\":{claim_id},",
                "\"timeline_prompts\":[{prompts}],",
                "\"backlog_item_ids\":[{backlog}],",
                "\"prevention_actions\":[{prevention}],",
                "\"links_backlog\":{links_backlog}",
                "}}"
            ),
            incident_id = json_string(&self.incident_id),
            class = self.incident_class.label(),
            severity = self.severity.label(),
            comparator_id = json_string(&self.comparator_id),
            claim_id = json_string(&self.claim_id),
            prompts = prompts,
            backlog = json_string_array(&self.backlog_item_ids),
            prevention = json_string_array(&self.prevention_actions),
            links_backlog = self.links_backlog(),
        )
    }
}

/// Complete operational response to a migration incident.
#[derive(Debug, Clone)]
pub struct MigrationIncidentResponse {
    /// Incident id.
    pub incident_id: String,
    /// Incident class.
    pub class: MigrationIncidentClass,
    /// Effective severity after signal escalation.
    pub severity: MigrationIncidentSeverity,
    /// Rollout emergency-hold reason this incident triggers.
    pub emergency_hold_reason: EmergencyHoldReason,
    /// Authority empowered to execute the rollback.
    pub rollback_authority: OperatorAuthority,
    /// Whether immediate rollback is required before triage.
    pub requires_immediate_rollback: bool,
    /// Observed signals that justified the effective severity.
    pub signals: Vec<MigrationIncidentSignal>,
    /// Resolved rollback plan.
    pub rollback: MigrationRollbackPlan,
    /// Rollback readiness.
    pub readiness: MigrationRollbackReadiness,
    /// Postmortem template.
    pub postmortem: MigrationPostmortemTemplate,
}

impl MigrationIncidentResponse {
    /// Whether the incident halts further promotion of the affected version.
    ///
    /// Any incident at `Sev3` or worse blocks promotion until resolved.
    #[must_use]
    pub fn blocks_promotion(&self) -> bool {
        self.severity.rank() >= MigrationIncidentSeverity::Sev3.rank()
    }

    /// Serialize the full incident response as a deterministic evidence record.
    #[must_use]
    pub fn to_json(&self) -> String {
        let signals = self
            .signals
            .iter()
            .map(MigrationIncidentSignal::to_json)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            concat!(
                "{{",
                "\"schema_version\":\"1.0.0\",",
                "\"incident_id\":{incident_id},",
                "\"class\":\"{class}\",",
                "\"severity\":\"{severity}\",",
                "\"ack_deadline_minutes\":{ack_deadline},",
                "\"emergency_hold_reason\":\"{hold}\",",
                "\"rollback_authority\":\"{authority}\",",
                "\"requires_immediate_rollback\":{immediate},",
                "\"blocks_promotion\":{blocks},",
                "\"signals\":[{signals}],",
                "\"rollback\":{rollback},",
                "\"readiness\":{readiness},",
                "\"postmortem\":{postmortem}",
                "}}"
            ),
            incident_id = json_string(&self.incident_id),
            class = self.class.label(),
            severity = self.severity.label(),
            ack_deadline = self.severity.ack_deadline_minutes(),
            hold = self.emergency_hold_reason.label(),
            authority = self.rollback_authority.label(),
            immediate = self.requires_immediate_rollback,
            blocks = self.blocks_promotion(),
            signals = signals,
            rollback = self.rollback.to_json(),
            readiness = self.readiness.to_json(),
            postmortem = self.postmortem.to_json(),
        )
    }
}

/// Deterministic playbook that resolves an incident report into a response.
#[derive(Debug, Clone, Copy, Default)]
pub struct MigrationIncidentResponsePlaybook;

impl MigrationIncidentResponsePlaybook {
    /// Construct the stateless playbook.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Resolve an incident report into a complete, deterministic response.
    #[must_use]
    pub fn resolve(&self, report: &MigrationIncidentReport) -> MigrationIncidentResponse {
        let severity = report.effective_severity();
        let rollback = MigrationRollbackPlan::default_for(report.class, report.detected_stage)
            .resolve(&report.available_artifacts);
        let readiness = rollback.readiness();
        let postmortem = MigrationPostmortemTemplate::for_incident(report, severity);
        MigrationIncidentResponse {
            incident_id: report.incident_id.clone(),
            class: report.class,
            severity,
            emergency_hold_reason: report.class.emergency_hold_reason(),
            rollback_authority: severity.rollback_authority(),
            requires_immediate_rollback: severity.requires_immediate_rollback(),
            signals: report.signals.clone(),
            rollback,
            readiness,
            postmortem,
        }
    }
}

/// Default prevention actions seeded by incident class.
#[must_use]
fn default_prevention_actions(class: MigrationIncidentClass) -> Vec<String> {
    let actions: &[&str] = match class {
        MigrationIncidentClass::SemanticRegression => &[
            "add-metamorphic-oracle-case",
            "expand-golden-isomorphism-corpus",
        ],
        MigrationIncidentClass::DeterminismDivergence => &[
            "add-determinism-soak-fixture",
            "pin-nondeterministic-input-source",
        ],
        MigrationIncidentClass::PerformanceRegression => &[
            "add-tail-latency-budget-gate",
            "capture-baseline-profile-artifact",
        ],
        MigrationIncidentClass::CapabilityGapEscape => &[
            "add-capability-gap-guard",
            "fail-closed-on-unsupported-construct",
        ],
        MigrationIncidentClass::SecurityBreach => {
            &["tighten-sandbox-policy", "add-secret-redaction-fixture"]
        }
        MigrationIncidentClass::CertificationFalsePass => &[
            "strengthen-certification-oracle",
            "add-field-evidence-cross-check",
        ],
        MigrationIncidentClass::RollbackFailure => {
            &["add-rollback-readiness-precheck", "rehearse-rollback-drill"]
        }
    };
    actions.iter().map(|action| (*action).to_string()).collect()
}

// ============================================================================
// Scorecard
// ============================================================================

/// Rollout go/no-go scorecard.
///
/// Collects shadow-run and benchmark evidence, then evaluates against
/// configured thresholds to produce a [`RolloutVerdict`].
#[derive(Debug)]
pub struct RolloutScorecard {
    config: RolloutScorecardConfig,
    shadow_results: Vec<ShadowRunResult>,
    benchmark_gate: Option<GateResult>,
}

impl RolloutScorecard {
    /// Create a new scorecard with the given configuration.
    pub fn new(config: RolloutScorecardConfig) -> Self {
        Self {
            config,
            shadow_results: Vec::new(),
            benchmark_gate: None,
        }
    }

    /// Add a shadow-run comparison result.
    pub fn add_shadow_result(&mut self, result: ShadowRunResult) {
        self.shadow_results.push(result);
    }

    /// Set the benchmark gate result.
    pub fn set_benchmark_gate(&mut self, result: GateResult) {
        self.benchmark_gate = Some(result);
    }

    /// Number of shadow scenarios recorded.
    #[must_use]
    pub fn shadow_scenario_count(&self) -> usize {
        self.shadow_results.len()
    }

    /// Number of shadow scenarios that matched (all frames identical).
    #[must_use]
    pub fn shadow_match_count(&self) -> usize {
        self.shadow_results
            .iter()
            .filter(|r| r.verdict == ShadowVerdict::Match)
            .count()
    }

    /// Aggregate frame match ratio across all shadow runs.
    #[must_use]
    pub fn aggregate_match_ratio(&self) -> f64 {
        if self.shadow_results.is_empty() {
            return 0.0;
        }
        let total_frames: usize = self.shadow_results.iter().map(|r| r.frames_compared).sum();
        if total_frames == 0 {
            // Zero compared frames is absence of evidence, not a perfect match
            // — a misconfigured/empty capture must not read as deterministic.
            return 0.0;
        }
        let matched_frames: usize = self
            .shadow_results
            .iter()
            .flat_map(|r| r.frame_comparisons.iter())
            .filter(|c| c.matched)
            .count();
        matched_frames as f64 / total_frames as f64
    }

    /// Evaluate the scorecard and produce a verdict.
    #[must_use]
    pub fn evaluate(&self) -> RolloutVerdict {
        // Check minimum scenario coverage
        if self.shadow_results.len() < self.config.min_shadow_scenarios {
            return RolloutVerdict::Inconclusive;
        }

        // A scorecard whose scenarios compared zero frames carries no
        // determinism evidence at all — it must not reach Go vacuously.
        let total_frames: usize = self.shadow_results.iter().map(|r| r.frames_compared).sum();
        if total_frames == 0 {
            return RolloutVerdict::Inconclusive;
        }

        // Check shadow determinism
        let match_ratio = self.aggregate_match_ratio();
        if match_ratio < self.config.min_match_ratio {
            return RolloutVerdict::NoGo;
        }

        // Check any shadow divergence
        if self
            .shadow_results
            .iter()
            .any(|r| r.verdict == ShadowVerdict::Diverged)
        {
            return RolloutVerdict::NoGo;
        }

        // Check benchmark gate if required
        if self.config.require_benchmark_pass {
            match &self.benchmark_gate {
                None => return RolloutVerdict::Inconclusive,
                Some(gate) if !gate.passed() => return RolloutVerdict::NoGo,
                _ => {}
            }
        }

        RolloutVerdict::Go
    }

    /// Produce a structured summary for operator review.
    #[must_use]
    pub fn summary(&self) -> RolloutSummary {
        let verdict = self.evaluate();
        RolloutSummary {
            verdict,
            shadow_scenarios: self.shadow_results.len(),
            shadow_matches: self.shadow_match_count(),
            aggregate_match_ratio: self.aggregate_match_ratio(),
            total_frames_compared: self.shadow_results.iter().map(|r| r.frames_compared).sum(),
            benchmark_passed: self.benchmark_gate.as_ref().map(|g| g.passed()),
            min_shadow_scenarios_required: self.config.min_shadow_scenarios,
            min_match_ratio_required: self.config.min_match_ratio,
            benchmark_required: self.config.require_benchmark_pass,
        }
    }
}

/// Structured summary of the rollout scorecard for operator review.
#[derive(Debug, Clone)]
pub struct RolloutSummary {
    /// Final verdict.
    pub verdict: RolloutVerdict,
    /// Number of shadow scenarios executed.
    pub shadow_scenarios: usize,
    /// Number of shadow scenarios that matched.
    pub shadow_matches: usize,
    /// Aggregate frame match ratio (0.0–1.0).
    pub aggregate_match_ratio: f64,
    /// Total frames compared across all shadow runs.
    pub total_frames_compared: usize,
    /// Benchmark gate result (None if not provided).
    pub benchmark_passed: Option<bool>,
    /// Configuration: minimum shadow scenarios required.
    pub min_shadow_scenarios_required: usize,
    /// Configuration: minimum match ratio required.
    pub min_match_ratio_required: f64,
    /// Configuration: whether benchmark is required.
    pub benchmark_required: bool,
}

impl RolloutSummary {
    /// Serialize the summary to a JSON string for machine consumption.
    ///
    /// This produces a self-contained evidence artifact that CI, operator
    /// dashboards, and go/no-go gates can consume without parsing human text.
    #[must_use]
    pub fn to_json(&self) -> String {
        let benchmark_str = match self.benchmark_passed {
            Some(true) => "\"pass\"",
            Some(false) => "\"fail\"",
            None => "null",
        };
        format!(
            concat!(
                "{{",
                "\"verdict\":\"{verdict}\",",
                "\"shadow_scenarios\":{scenarios},",
                "\"shadow_matches\":{matches},",
                "\"aggregate_match_ratio\":{ratio},",
                "\"total_frames_compared\":{frames},",
                "\"benchmark_passed\":{bench},",
                "\"config\":{{",
                "\"min_shadow_scenarios\":{min_scenarios},",
                "\"min_match_ratio\":{min_ratio},",
                "\"benchmark_required\":{bench_required}",
                "}}",
                "}}"
            ),
            verdict = self.verdict.label(),
            scenarios = self.shadow_scenarios,
            matches = self.shadow_matches,
            ratio = self.aggregate_match_ratio,
            frames = self.total_frames_compared,
            bench = benchmark_str,
            min_scenarios = self.min_shadow_scenarios_required,
            min_ratio = self.min_match_ratio_required,
            bench_required = self.benchmark_required,
        )
    }
}

impl std::fmt::Display for RolloutSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "=== Rollout Scorecard ===")?;
        writeln!(f, "Verdict: {}", self.verdict)?;
        writeln!(
            f,
            "Shadow: {}/{} scenarios matched ({} required)",
            self.shadow_matches, self.shadow_scenarios, self.min_shadow_scenarios_required,
        )?;
        writeln!(
            f,
            "Match ratio: {:.1}% (>= {:.1}% required)",
            self.aggregate_match_ratio * 100.0,
            self.min_match_ratio_required * 100.0,
        )?;
        writeln!(f, "Frames compared: {}", self.total_frames_compared)?;
        match self.benchmark_passed {
            Some(true) => writeln!(f, "Benchmark: PASS")?,
            Some(false) => writeln!(f, "Benchmark: FAIL")?,
            None if self.benchmark_required => writeln!(f, "Benchmark: MISSING (required)")?,
            None => writeln!(f, "Benchmark: not provided")?,
        }
        Ok(())
    }
}

// ============================================================================
// Evidence bundle (bd-2crbt AC #2, #3)
// ============================================================================

/// Self-contained rollout evidence bundle for release decisions.
///
/// Combines the scorecard verdict with queue telemetry and runtime lane
/// information so operators can make go/no-go decisions from a single
/// artifact without correlating across multiple logs.
#[derive(Debug, Clone)]
pub struct RolloutEvidenceBundle {
    /// Scorecard summary with verdict.
    pub scorecard: RolloutSummary,
    /// Queue telemetry snapshot at evidence-collection time.
    pub queue_telemetry: Option<QueueTelemetry>,
    /// Requested runtime lane.
    pub requested_lane: String,
    /// Resolved runtime lane (after fallback).
    pub resolved_lane: String,
    /// Rollout policy in effect.
    pub rollout_policy: String,
}

impl RolloutEvidenceBundle {
    /// Serialize the full evidence bundle to JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        let qt_json = match &self.queue_telemetry {
            Some(qt) => format!(
                concat!(
                    "{{",
                    "\"enqueued\":{e},",
                    "\"processed\":{p},",
                    "\"dropped\":{d},",
                    "\"high_water\":{hw},",
                    "\"in_flight\":{inf}",
                    "}}"
                ),
                e = qt.enqueued,
                p = qt.processed,
                d = qt.dropped,
                hw = qt.high_water,
                inf = qt.in_flight,
            ),
            None => "null".to_string(),
        };
        format!(
            concat!(
                "{{",
                "\"schema_version\":\"1.0.0\",",
                "\"scorecard\":{sc},",
                "\"queue_telemetry\":{qt},",
                "\"runtime\":{{",
                "\"requested_lane\":\"{rl}\",",
                "\"resolved_lane\":\"{rsl}\",",
                "\"rollout_policy\":\"{rp}\"",
                "}}",
                "}}"
            ),
            sc = self.scorecard.to_json(),
            qt = qt_json,
            rl = self.requested_lane,
            rsl = self.resolved_lane,
            rp = self.rollout_policy,
        )
    }
}

impl std::fmt::Display for RolloutEvidenceBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "=== Rollout Evidence Bundle ===")?;
        writeln!(
            f,
            "Lane: {} (resolved: {})",
            self.requested_lane, self.resolved_lane
        )?;
        writeln!(f, "Policy: {}", self.rollout_policy)?;
        write!(f, "{}", self.scorecard)?;
        if let Some(qt) = &self.queue_telemetry {
            writeln!(
                f,
                "Queue: enqueued={}, processed={}, dropped={}, high_water={}, in_flight={}",
                qt.enqueued, qt.processed, qt.dropped, qt.high_water, qt.in_flight
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shadow_run::{FrameComparison, ShadowRunResult, ShadowVerdict};

    use crate::lab_integration::LabOutput;

    fn empty_lab_output() -> LabOutput {
        LabOutput {
            frame_count: 0,
            frame_records: vec![],
            event_count: 0,
            event_log: vec![],
            tick_count: 0,
            anomaly_count: 0,
        }
    }

    fn make_shadow_result(verdict: ShadowVerdict, frames: usize) -> ShadowRunResult {
        let frame_comparisons: Vec<FrameComparison> = (0..frames)
            .map(|i| FrameComparison {
                index: i,
                baseline_checksum: 0xDEAD_BEEF,
                candidate_checksum: if verdict == ShadowVerdict::Match {
                    0xDEAD_BEEF
                } else {
                    0xCAFE_BABE
                },
                matched: verdict == ShadowVerdict::Match,
            })
            .collect();

        ShadowRunResult {
            verdict,
            scenario_name: "test".to_string(),
            seed: 42,
            frame_comparisons,
            first_divergence: if verdict == ShadowVerdict::Diverged {
                Some(0)
            } else {
                None
            },
            frames_compared: frames,
            baseline: empty_lab_output(),
            candidate: empty_lab_output(),
            baseline_label: "baseline".to_string(),
            candidate_label: "candidate".to_string(),
            run_total: 1,
        }
    }

    fn release_gate_artifact(kind: &str) -> MigrationReleaseGateArtifact {
        let artifact_id = format!("{kind}-artifact");
        let uri = format!("artifacts/{kind}.json");
        MigrationReleaseGateArtifact::new(
            &artifact_id,
            kind,
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            &uri,
        )
    }

    fn beta_release_gate_evidence() -> MigrationReleaseGateEvidence {
        let readiness = MigrationReadinessEvidence::new(OperatorAuthority::ReleaseOwner)
            .certification_pass_ratio(0.98)
            .corpus_coverage_ratio(0.70)
            .reliability_pass_ratio(0.99)
            .deterministic_artifact_count(5)
            .benchmark_gate_passed(true)
            .open_blocker_count(0);

        MigrationReleaseGateEvidence::new("migration-service-2026.05.08", readiness)
            .determinism_pass_ratio(1.0)
            .performance_budget_passed(true)
            .unresolved_critical_gap_count(0)
            .artifact(release_gate_artifact("certification"))
            .artifact(release_gate_artifact("critical-gaps"))
            .artifact(release_gate_artifact("determinism"))
            .artifact(release_gate_artifact("performance"))
            .artifact(release_gate_artifact("readiness"))
            .artifact(release_gate_artifact("corpus"))
    }

    #[test]
    fn scorecard_go_with_matching_shadows() {
        let config = RolloutScorecardConfig::default().min_shadow_scenarios(2);
        let mut sc = RolloutScorecard::new(config);
        sc.add_shadow_result(make_shadow_result(ShadowVerdict::Match, 10));
        sc.add_shadow_result(make_shadow_result(ShadowVerdict::Match, 15));

        let verdict = sc.evaluate();
        assert_eq!(verdict, RolloutVerdict::Go);
        assert!(verdict.is_go());
        assert_eq!(sc.aggregate_match_ratio(), 1.0);
    }

    #[test]
    fn scorecard_nogo_with_diverged_shadow() {
        let config = RolloutScorecardConfig::default();
        let mut sc = RolloutScorecard::new(config);
        sc.add_shadow_result(make_shadow_result(ShadowVerdict::Diverged, 10));

        assert_eq!(sc.evaluate(), RolloutVerdict::NoGo);
    }

    #[test]
    fn scorecard_inconclusive_without_enough_scenarios() {
        let config = RolloutScorecardConfig::default().min_shadow_scenarios(3);
        let mut sc = RolloutScorecard::new(config);
        sc.add_shadow_result(make_shadow_result(ShadowVerdict::Match, 10));
        sc.add_shadow_result(make_shadow_result(ShadowVerdict::Match, 10));

        assert_eq!(sc.evaluate(), RolloutVerdict::Inconclusive);
    }

    #[test]
    fn scorecard_inconclusive_when_benchmark_required_but_missing() {
        let config = RolloutScorecardConfig::default().require_benchmark_pass(true);
        let mut sc = RolloutScorecard::new(config);
        sc.add_shadow_result(make_shadow_result(ShadowVerdict::Match, 10));

        assert_eq!(sc.evaluate(), RolloutVerdict::Inconclusive);
    }

    #[test]
    fn scorecard_summary_display() {
        let config = RolloutScorecardConfig::default().min_shadow_scenarios(1);
        let mut sc = RolloutScorecard::new(config);
        sc.add_shadow_result(make_shadow_result(ShadowVerdict::Match, 10));

        let summary = sc.summary();
        let text = summary.to_string();
        assert!(text.contains("GO"));
        assert!(text.contains("100.0%"));
        assert!(text.contains("10"));
    }

    #[test]
    fn verdict_labels() {
        assert_eq!(RolloutVerdict::Go.label(), "GO");
        assert_eq!(RolloutVerdict::NoGo.label(), "NO-GO");
        assert_eq!(RolloutVerdict::Inconclusive.label(), "INCONCLUSIVE");
        assert_eq!(format!("{}", RolloutVerdict::Go), "GO");
    }

    #[test]
    fn readiness_rubric_allows_alpha_at_thresholds() {
        let rubric = MigrationReadinessRubric::opentui_default();
        let evidence = MigrationReadinessEvidence::new(OperatorAuthority::ReleaseOwner)
            .certification_pass_ratio(0.90)
            .corpus_coverage_ratio(0.25)
            .reliability_pass_ratio(0.95)
            .deterministic_artifact_count(3)
            .benchmark_gate_passed(true)
            .open_blocker_count(2);

        let decision = rubric.evaluate(MigrationRolloutStage::Alpha, &evidence);
        assert_eq!(decision.verdict, MigrationReadinessVerdict::Advance);
        assert!(
            decision.reasons.is_empty(),
            "passing evidence must not carry hold reasons"
        );
    }

    #[test]
    fn readiness_rubric_fails_closed_on_non_finite_evidence() {
        let rubric = MigrationReadinessRubric::opentui_default();
        let mut evidence = MigrationReadinessEvidence::new(OperatorAuthority::ReleaseOwner)
            .certification_pass_ratio(0.99)
            .corpus_coverage_ratio(0.80)
            .reliability_pass_ratio(0.99)
            .deterministic_artifact_count(4)
            .benchmark_gate_passed(true);
        // The fields are pub f64: a NaN smuggled in via direct construction or
        // deserialization must fail the gate, not sail past `<` comparisons.
        evidence.certification_pass_ratio = f64::NAN;
        let decision = rubric.evaluate(MigrationRolloutStage::Alpha, &evidence);
        assert_eq!(decision.verdict, MigrationReadinessVerdict::Hold);
        assert!(decision.reasons.contains(&"certification-threshold"));
    }

    #[test]
    fn scorecard_with_zero_compared_frames_is_not_go() {
        let mut scorecard =
            RolloutScorecard::new(RolloutScorecardConfig::default().min_shadow_scenarios(1));
        scorecard.add_shadow_result(make_shadow_result(ShadowVerdict::Match, 0));
        // Zero compared frames is missing evidence, not proof of determinism.
        assert_eq!(scorecard.aggregate_match_ratio(), 0.0);
        assert_eq!(scorecard.evaluate(), RolloutVerdict::Inconclusive);
    }

    #[test]
    fn readiness_rubric_blocks_beta_without_artifacts_and_benchmark() {
        let rubric = MigrationReadinessRubric::opentui_default();
        let evidence = MigrationReadinessEvidence::new(OperatorAuthority::ReleaseOwner)
            .certification_pass_ratio(0.99)
            .corpus_coverage_ratio(0.80)
            .reliability_pass_ratio(0.99)
            .deterministic_artifact_count(4)
            .benchmark_gate_passed(false);

        let decision = rubric.evaluate(MigrationRolloutStage::Beta, &evidence);
        assert_eq!(decision.verdict, MigrationReadinessVerdict::Hold);
        assert!(
            decision
                .reasons
                .contains(&"deterministic-artifact-threshold")
        );
        assert!(decision.reasons.contains(&"benchmark-gate"));
    }

    #[test]
    fn readiness_rubric_requires_maintainer_quorum_for_ga() {
        let rubric = MigrationReadinessRubric::opentui_default();
        let release_owner_evidence =
            MigrationReadinessEvidence::new(OperatorAuthority::ReleaseOwner)
                .certification_pass_ratio(1.0)
                .corpus_coverage_ratio(0.95)
                .reliability_pass_ratio(0.999)
                .deterministic_artifact_count(7)
                .benchmark_gate_passed(true);

        let decision = rubric.evaluate(MigrationRolloutStage::Ga, &release_owner_evidence);
        assert_eq!(decision.verdict, MigrationReadinessVerdict::Hold);
        assert_eq!(decision.reasons, vec!["operator-authority"]);

        let quorum_evidence = MigrationReadinessEvidence {
            operator_authority: OperatorAuthority::MaintainerQuorum,
            ..release_owner_evidence
        };
        assert!(
            rubric
                .evaluate(MigrationRolloutStage::Ga, &quorum_evidence)
                .may_advance()
        );
    }

    #[test]
    fn readiness_rubric_emergency_hold_overrides_clean_evidence() {
        let rubric = MigrationReadinessRubric::opentui_default();
        let evidence = MigrationReadinessEvidence::new(OperatorAuthority::MaintainerQuorum)
            .certification_pass_ratio(1.0)
            .corpus_coverage_ratio(1.0)
            .reliability_pass_ratio(1.0)
            .deterministic_artifact_count(8)
            .benchmark_gate_passed(true)
            .emergency_hold(EmergencyHold::new(
                EmergencyHoldReason::DeterminismDivergence,
                OperatorAuthority::OnCall,
            ));

        let decision = rubric.evaluate(MigrationRolloutStage::Ga, &evidence);
        assert_eq!(decision.verdict, MigrationReadinessVerdict::EmergencyHold);
        assert_eq!(decision.reasons, vec!["determinism-divergence"]);
    }

    #[test]
    fn readiness_rubric_recommends_highest_passing_stage() {
        let rubric = MigrationReadinessRubric::opentui_default();
        let beta_ready = MigrationReadinessEvidence::new(OperatorAuthority::ReleaseOwner)
            .certification_pass_ratio(0.98)
            .corpus_coverage_ratio(0.75)
            .reliability_pass_ratio(0.99)
            .deterministic_artifact_count(5)
            .benchmark_gate_passed(true);

        assert_eq!(
            rubric.recommended_stage(&beta_ready),
            Some(MigrationRolloutStage::Beta)
        );
    }

    #[test]
    fn readiness_decision_json_is_machine_readable() {
        let rubric = MigrationReadinessRubric::opentui_default();
        let evidence = MigrationReadinessEvidence::new(OperatorAuthority::Automation);
        let json = rubric
            .evaluate(MigrationRolloutStage::Alpha, &evidence)
            .to_json();

        assert!(json.contains("\"stage\":\"alpha\""));
        assert!(json.contains("\"verdict\":\"hold\""));
        assert!(json.contains("\"operator-authority\""));
    }

    #[test]
    fn readiness_rubric_rejects_non_finite_ratios() {
        let rubric = MigrationReadinessRubric::opentui_default();
        let evidence = MigrationReadinessEvidence::new(OperatorAuthority::MaintainerQuorum)
            .certification_pass_ratio(f64::NAN)
            .corpus_coverage_ratio(f64::INFINITY)
            .reliability_pass_ratio(f64::NEG_INFINITY)
            .deterministic_artifact_count(8)
            .benchmark_gate_passed(true);

        let decision = rubric.evaluate(MigrationRolloutStage::Ga, &evidence);
        assert_eq!(decision.verdict, MigrationReadinessVerdict::Hold);
        assert!(decision.reasons.contains(&"certification-threshold"));
        assert!(decision.reasons.contains(&"corpus-coverage-threshold"));
        assert!(
            decision
                .reasons
                .contains(&"operational-reliability-threshold")
        );
    }

    #[test]
    fn release_gate_enforce_passes_with_traceable_artifacts() {
        let policy = MigrationReleaseGatePolicy::for_stage(
            MigrationRolloutStage::Beta,
            MigrationReleaseGateMode::Enforce,
        );
        let evaluator = MigrationReleaseGateEvaluator::new(policy);
        let decision = evaluator.evaluate(&beta_release_gate_evidence());

        assert_eq!(decision.verdict, MigrationReleaseGateVerdict::Pass);
        assert!(decision.passed());
        assert!(!decision.blocks_release());
        assert!(decision.failed_clauses().is_empty());
        assert_eq!(
            decision
                .clauses
                .iter()
                .map(|clause| clause.clause)
                .collect::<Vec<_>>(),
            vec![
                "readiness",
                "certification",
                "determinism",
                "performance-budget",
                "critical-gaps",
                "artifact-traceability",
            ]
        );
        let critical_gap_artifacts = vec!["critical-gaps-artifact".to_string()];
        assert!(decision.clauses.iter().any(|clause| {
            clause.clause == "critical-gaps" && clause.artifact_ids == critical_gap_artifacts
        }));
    }

    #[test]
    fn release_gate_enforce_fails_with_clause_reasons() {
        let readiness = MigrationReadinessEvidence::new(OperatorAuthority::ReleaseOwner)
            .certification_pass_ratio(0.96)
            .corpus_coverage_ratio(0.70)
            .reliability_pass_ratio(0.99)
            .deterministic_artifact_count(5)
            .benchmark_gate_passed(true);
        let evidence = MigrationReleaseGateEvidence::new("migration-service-bad", readiness)
            .determinism_pass_ratio(0.95)
            .performance_budget_passed(false)
            .unresolved_critical_gap_count(2)
            .artifact(release_gate_artifact("certification"))
            .artifact(release_gate_artifact("critical-gaps"))
            .artifact(release_gate_artifact("determinism"))
            .artifact(release_gate_artifact("performance"))
            .artifact(release_gate_artifact("readiness"))
            .artifact(release_gate_artifact("corpus"));
        let policy = MigrationReleaseGatePolicy::for_stage(
            MigrationRolloutStage::Beta,
            MigrationReleaseGateMode::Enforce,
        );
        let decision = MigrationReleaseGateEvaluator::new(policy).evaluate(&evidence);

        assert_eq!(decision.verdict, MigrationReleaseGateVerdict::Fail);
        assert!(decision.blocks_release());
        let failed = decision.failed_clauses();
        assert!(failed.contains(&"readiness"));
        assert!(failed.contains(&"certification"));
        assert!(failed.contains(&"determinism"));
        assert!(failed.contains(&"performance-budget"));
        assert!(failed.contains(&"critical-gaps"));
        assert!(
            decision
                .clauses
                .iter()
                .any(|clause| clause.reason.contains("0.960000 < 0.970000"))
        );
    }

    #[test]
    fn release_gate_dry_run_failure_does_not_block_release() {
        let mut evidence = beta_release_gate_evidence();
        evidence.performance_budget_passed = false;
        let policy = MigrationReleaseGatePolicy::for_stage(
            MigrationRolloutStage::Beta,
            MigrationReleaseGateMode::DryRun,
        );
        let decision = MigrationReleaseGateEvaluator::new(policy).evaluate(&evidence);

        assert_eq!(decision.verdict, MigrationReleaseGateVerdict::Fail);
        assert!(!decision.blocks_release());
        assert!(decision.failed_clauses().contains(&"performance-budget"));
    }

    #[test]
    fn release_gate_rejects_mutable_artifact_evidence() {
        let readiness = MigrationReadinessEvidence::new(OperatorAuthority::ReleaseOwner)
            .certification_pass_ratio(0.98)
            .corpus_coverage_ratio(0.70)
            .reliability_pass_ratio(0.99)
            .deterministic_artifact_count(5)
            .benchmark_gate_passed(true);
        let mutable_performance = MigrationReleaseGateArtifact::new(
            "performance-artifact",
            "performance",
            "latest",
            "artifacts/performance.json",
        );
        let evidence = MigrationReleaseGateEvidence::new("migration-service-mutable", readiness)
            .determinism_pass_ratio(1.0)
            .performance_budget_passed(true)
            .artifact(release_gate_artifact("certification"))
            .artifact(release_gate_artifact("critical-gaps"))
            .artifact(release_gate_artifact("determinism"))
            .artifact(mutable_performance)
            .artifact(release_gate_artifact("readiness"))
            .artifact(release_gate_artifact("corpus"));
        let policy = MigrationReleaseGatePolicy::for_stage(
            MigrationRolloutStage::Beta,
            MigrationReleaseGateMode::Enforce,
        );
        let decision = MigrationReleaseGateEvaluator::new(policy).evaluate(&evidence);

        assert_eq!(decision.verdict, MigrationReleaseGateVerdict::Fail);
        assert!(decision.blocks_release());
        assert!(decision.clauses.iter().any(|clause| {
            clause.clause == "artifact-traceability"
                && !clause.passed
                && clause.reason.contains("invalid immutable references")
                && clause
                    .reason
                    .contains("missing artifact kinds: performance")
        }));
    }

    #[test]
    fn release_gate_json_is_machine_readable() -> serde_json::Result<()> {
        let policy = MigrationReleaseGatePolicy::for_stage(
            MigrationRolloutStage::Beta,
            MigrationReleaseGateMode::Enforce,
        );
        let mut evidence = beta_release_gate_evidence();
        evidence.migration_version = "migration-service-\"beta\"".to_string();
        let decision = MigrationReleaseGateEvaluator::new(policy).evaluate(&evidence);
        let json = decision.to_json();
        let parsed: serde_json::Value = serde_json::from_str(&json)?;

        assert_eq!(parsed["schema_version"], "1.0.0");
        assert_eq!(parsed["migration_version"], "migration-service-\"beta\"");
        assert_eq!(parsed["mode"], "enforce");
        assert_eq!(parsed["target_stage"], "beta");
        assert_eq!(parsed["verdict"], "pass");
        assert_eq!(parsed["blocks_release"], false);
        assert_eq!(parsed["clauses"].as_array().map(Vec::len), Some(6));
        Ok(())
    }

    #[test]
    fn scorecard_summary_json_go() {
        let config = RolloutScorecardConfig::default().min_shadow_scenarios(1);
        let mut sc = RolloutScorecard::new(config);
        sc.add_shadow_result(make_shadow_result(ShadowVerdict::Match, 10));

        let json = sc.summary().to_json();
        assert!(json.contains("\"verdict\":\"GO\""));
        assert!(json.contains("\"shadow_scenarios\":1"));
        assert!(json.contains("\"shadow_matches\":1"));
        assert!(json.contains("\"total_frames_compared\":10"));
        assert!(json.contains("\"aggregate_match_ratio\":1"));
        assert!(json.contains("\"benchmark_passed\":null"));
    }

    #[test]
    fn scorecard_summary_json_nogo() {
        let config = RolloutScorecardConfig::default();
        let mut sc = RolloutScorecard::new(config);
        sc.add_shadow_result(make_shadow_result(ShadowVerdict::Diverged, 5));

        let json = sc.summary().to_json();
        assert!(json.contains("\"verdict\":\"NO-GO\""));
        assert!(json.contains("\"shadow_matches\":0"));
    }

    #[test]
    fn scorecard_e2e_with_real_shadow_run() {
        use crate::shadow_run::{ShadowRun, ShadowRunConfig};
        use ftui_core::event::Event;
        use ftui_core::geometry::Rect;
        use ftui_render::frame::Frame;
        use ftui_runtime::program::{Cmd, Model};
        use ftui_widgets::Widget;
        use ftui_widgets::paragraph::Paragraph;

        struct RolloutModel {
            ticks: u64,
        }

        #[derive(Debug, Clone)]
        enum RolloutMsg {
            Tick,
            Quit,
        }

        impl From<Event> for RolloutMsg {
            fn from(e: Event) -> Self {
                match e {
                    Event::Tick => RolloutMsg::Tick,
                    _ => RolloutMsg::Quit,
                }
            }
        }

        impl Model for RolloutModel {
            type Message = RolloutMsg;

            fn update(&mut self, msg: RolloutMsg) -> Cmd<RolloutMsg> {
                match msg {
                    RolloutMsg::Tick => {
                        self.ticks += 1;
                        Cmd::none()
                    }
                    RolloutMsg::Quit => Cmd::quit(),
                }
            }

            fn view(&self, frame: &mut Frame) {
                let text = format!("Ticks: {}", self.ticks);
                let area = Rect::new(0, 0, frame.width(), 1);
                Paragraph::new(text).render(area, frame);
            }
        }

        // Run 3 shadow scenarios with different seeds
        let mut scorecard =
            RolloutScorecard::new(RolloutScorecardConfig::default().min_shadow_scenarios(3));

        for seed in [42, 99, 7] {
            let config = ShadowRunConfig::new("rollout_e2e", "tick_counter", seed).viewport(40, 10);
            let result = ShadowRun::compare(
                config,
                || RolloutModel { ticks: 0 },
                |session| {
                    session.init();
                    for _ in 0..5 {
                        session.tick();
                        session.capture_frame();
                    }
                },
            );
            scorecard.add_shadow_result(result);
        }

        // All scenarios should match (same deterministic model)
        let verdict = scorecard.evaluate();
        assert_eq!(verdict, RolloutVerdict::Go);

        let summary = scorecard.summary();
        assert_eq!(summary.shadow_scenarios, 3);
        assert_eq!(summary.shadow_matches, 3);
        assert_eq!(summary.total_frames_compared, 15); // 5 frames × 3 scenarios
        assert!((summary.aggregate_match_ratio - 1.0).abs() < f64::EPSILON);
        assert!(summary.to_string().contains("GO"));
    }

    #[test]
    fn evidence_bundle_json_contains_all_sections() {
        let config = RolloutScorecardConfig::default().min_shadow_scenarios(1);
        let mut sc = RolloutScorecard::new(config);
        sc.add_shadow_result(make_shadow_result(ShadowVerdict::Match, 5));

        let bundle = RolloutEvidenceBundle {
            scorecard: sc.summary(),
            queue_telemetry: Some(QueueTelemetry {
                enqueued: 10,
                processed: 8,
                dropped: 1,
                high_water: 4,
                in_flight: 1,
            }),
            requested_lane: "structured".to_string(),
            resolved_lane: "structured".to_string(),
            rollout_policy: "shadow".to_string(),
        };

        let json = bundle.to_json();
        assert!(json.contains("\"schema_version\":\"1.0.0\""));
        assert!(json.contains("\"scorecard\":{"));
        assert!(json.contains("\"verdict\":\"GO\""));
        assert!(json.contains("\"queue_telemetry\":{"));
        assert!(json.contains("\"enqueued\":10"));
        assert!(json.contains("\"dropped\":1"));
        assert!(json.contains("\"runtime\":{"));
        assert!(json.contains("\"requested_lane\":\"structured\""));
        assert!(json.contains("\"rollout_policy\":\"shadow\""));
    }

    #[test]
    fn evidence_bundle_display_readable() {
        let config = RolloutScorecardConfig::default().min_shadow_scenarios(1);
        let mut sc = RolloutScorecard::new(config);
        sc.add_shadow_result(make_shadow_result(ShadowVerdict::Match, 5));

        let bundle = RolloutEvidenceBundle {
            scorecard: sc.summary(),
            queue_telemetry: None,
            requested_lane: "asupersync".to_string(),
            resolved_lane: "structured".to_string(),
            rollout_policy: "off".to_string(),
        };

        let text = bundle.to_string();
        assert!(text.contains("Rollout Evidence Bundle"));
        assert!(text.contains("asupersync"));
        assert!(text.contains("structured"));
        assert!(text.contains("GO"));
    }

    // ------------------------------------------------------------------
    // Migration incident response and rollback (bd-3bxhj.9.7)
    // ------------------------------------------------------------------

    /// Build the full set of immutable artifacts a rollback spine can consume.
    fn rollback_artifact_set() -> Vec<MigrationReleaseGateArtifact> {
        [
            "release-gate-decision",
            "source-snapshot",
            "last-good-release",
            "determinism-baseline",
            "certification-report",
            "secret-rotation-runbook",
            "manual-recovery-runbook",
        ]
        .iter()
        .map(|kind| release_gate_artifact(kind))
        .collect()
    }

    #[test]
    fn incident_class_maps_to_severity_and_hold_reason() {
        assert_eq!(MigrationIncidentClass::ALL.len(), 7);
        // Every class has a non-empty, unique label.
        let mut labels: Vec<&str> = MigrationIncidentClass::ALL
            .iter()
            .map(|class| class.label())
            .collect();
        let count = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), count, "labels must be unique");

        assert_eq!(
            MigrationIncidentClass::DeterminismDivergence.default_severity(),
            MigrationIncidentSeverity::Sev1
        );
        assert_eq!(
            MigrationIncidentClass::PerformanceRegression.default_severity(),
            MigrationIncidentSeverity::Sev3
        );
        assert_eq!(
            MigrationIncidentClass::SecurityBreach.emergency_hold_reason(),
            EmergencyHoldReason::SecurityIncident
        );
        assert_eq!(
            MigrationIncidentClass::DeterminismDivergence.emergency_hold_reason(),
            EmergencyHoldReason::DeterminismDivergence
        );
    }

    #[test]
    fn severity_rank_escalation_and_response_metadata() {
        // Rank is strictly decreasing from Sev1 to Sev4.
        assert!(MigrationIncidentSeverity::Sev1.rank() > MigrationIncidentSeverity::Sev2.rank());
        assert!(MigrationIncidentSeverity::Sev2.rank() > MigrationIncidentSeverity::Sev3.rank());
        assert!(MigrationIncidentSeverity::Sev3.rank() > MigrationIncidentSeverity::Sev4.rank());
        // escalate() returns the worse severity regardless of argument order.
        assert_eq!(
            MigrationIncidentSeverity::Sev3.escalate(MigrationIncidentSeverity::Sev1),
            MigrationIncidentSeverity::Sev1
        );
        assert_eq!(
            MigrationIncidentSeverity::Sev1.escalate(MigrationIncidentSeverity::Sev4),
            MigrationIncidentSeverity::Sev1
        );
        // Higher severity has a tighter ack deadline.
        assert!(
            MigrationIncidentSeverity::Sev1.ack_deadline_minutes()
                < MigrationIncidentSeverity::Sev4.ack_deadline_minutes()
        );
        assert!(MigrationIncidentSeverity::Sev1.requires_immediate_rollback());
        assert!(!MigrationIncidentSeverity::Sev3.requires_immediate_rollback());
        assert_eq!(
            MigrationIncidentSeverity::Sev1.rollback_authority(),
            OperatorAuthority::OnCall
        );
        assert_eq!(
            MigrationIncidentSeverity::Sev4.rollback_authority(),
            OperatorAuthority::ReleaseOwner
        );
    }

    #[test]
    fn signals_escalate_effective_severity() {
        // Performance regression defaults to Sev3, but a Sev1 signal escalates.
        let report = MigrationIncidentReport::new(
            "INC-1",
            MigrationIncidentClass::PerformanceRegression,
            MigrationRolloutStage::Beta,
        )
        .signal(MigrationIncidentSignal::new(
            "p95-breach",
            "p95 over budget",
        ))
        .signal(
            MigrationIncidentSignal::new("data-corruption", "rows lost")
                .escalates_to(MigrationIncidentSeverity::Sev1),
        );
        assert_eq!(report.effective_severity(), MigrationIncidentSeverity::Sev1);

        // With no escalating signals, the class default holds.
        let plain = MigrationIncidentReport::new(
            "INC-2",
            MigrationIncidentClass::PerformanceRegression,
            MigrationRolloutStage::Beta,
        )
        .signal(MigrationIncidentSignal::new(
            "p95-breach",
            "p95 over budget",
        ));
        assert_eq!(plain.effective_severity(), MigrationIncidentSeverity::Sev3);
    }

    #[test]
    fn rollback_plan_has_spine_and_class_specific_steps() {
        // Default class uses the six-step spine (5 artifact-bound + postmortem).
        let plain = MigrationRollbackPlan::default_for(
            MigrationIncidentClass::SemanticRegression,
            MigrationRolloutStage::Beta,
        );
        assert_eq!(plain.steps.len(), 6);
        let last = plain.steps.last().expect("non-empty plan");
        assert_eq!(last.action, "record-postmortem");
        assert!(last.required_artifact_kind.is_empty());
        // The artifact-free step is always considered resolved.
        assert!(last.is_resolved());

        // Security breach inserts a credential-rotation step.
        let security = MigrationRollbackPlan::default_for(
            MigrationIncidentClass::SecurityBreach,
            MigrationRolloutStage::Ga,
        );
        assert_eq!(security.steps.len(), 7);
        assert!(
            security
                .steps
                .iter()
                .any(|step| step.action == "rotate-exposed-credentials")
        );

        // Rollback failure escalates to manual recovery.
        let recovery = MigrationRollbackPlan::default_for(
            MigrationIncidentClass::RollbackFailure,
            MigrationRolloutStage::Ga,
        );
        assert!(
            recovery
                .steps
                .iter()
                .any(|step| step.action == "escalate-manual-recovery")
        );

        // Steps are ordered 1..=n with no gaps.
        for (index, step) in plain.steps.iter().enumerate() {
            assert_eq!(step.order, index + 1);
        }
    }

    #[test]
    fn rollback_ready_when_all_artifacts_present() {
        let artifacts = rollback_artifact_set();
        let plan = MigrationRollbackPlan::default_for(
            MigrationIncidentClass::SecurityBreach,
            MigrationRolloutStage::Ga,
        )
        .resolve(&artifacts);
        let readiness = plan.readiness();
        assert!(readiness.is_ready());
        assert_eq!(readiness.verdict, MigrationRollbackReadinessVerdict::Ready);
        assert!(readiness.missing_artifact_kinds.is_empty());
        assert_eq!(readiness.resolved_steps, readiness.total_steps);
    }

    #[test]
    fn rollback_blocked_when_required_artifact_missing() {
        // Drop the determinism baseline artifact; the plan must be blocked.
        let artifacts: Vec<MigrationReleaseGateArtifact> = rollback_artifact_set()
            .into_iter()
            .filter(|artifact| artifact.kind != "determinism-baseline")
            .collect();
        let plan = MigrationRollbackPlan::default_for(
            MigrationIncidentClass::DeterminismDivergence,
            MigrationRolloutStage::Beta,
        )
        .resolve(&artifacts);
        let readiness = plan.readiness();
        assert!(!readiness.is_ready());
        assert_eq!(
            readiness.verdict,
            MigrationRollbackReadinessVerdict::Blocked
        );
        assert_eq!(
            readiness.missing_artifact_kinds,
            vec!["determinism-baseline"]
        );
    }

    #[test]
    fn rollback_ignores_mutable_artifacts() {
        // A non-sha256 (mutable) artifact of the right kind must not resolve.
        let mut artifacts = rollback_artifact_set();
        artifacts.retain(|artifact| artifact.kind != "last-good-release");
        artifacts.push(MigrationReleaseGateArtifact::new(
            "last-good-release-mutable",
            "last-good-release",
            "md5:not-a-real-sha",
            "artifacts/last-good.json",
        ));
        let plan = MigrationRollbackPlan::default_for(
            MigrationIncidentClass::SemanticRegression,
            MigrationRolloutStage::Beta,
        )
        .resolve(&artifacts);
        let readiness = plan.readiness();
        assert!(!readiness.is_ready());
        assert!(
            readiness
                .missing_artifact_kinds
                .contains(&"last-good-release")
        );
    }

    #[test]
    fn rollback_resolution_is_deterministic() {
        // Two immutable artifacts of the same kind: the smaller id wins.
        let mut artifacts = rollback_artifact_set();
        artifacts.push(MigrationReleaseGateArtifact::new(
            "aaa-source-snapshot",
            "source-snapshot",
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "artifacts/aaa.json",
        ));
        let plan = MigrationRollbackPlan::default_for(
            MigrationIncidentClass::SemanticRegression,
            MigrationRolloutStage::Beta,
        )
        .resolve(&artifacts);
        let step = plan
            .steps
            .iter()
            .find(|step| step.required_artifact_kind == "source-snapshot")
            .expect("source-snapshot step present");
        assert_eq!(step.artifact_id.as_deref(), Some("aaa-source-snapshot"));
    }

    #[test]
    fn postmortem_links_backlog_and_seeds_prevention() {
        let report = MigrationIncidentReport::new(
            "INC-9",
            MigrationIncidentClass::CertificationFalsePass,
            MigrationRolloutStage::Ga,
        )
        .comparator_id("cmp-cert-7")
        .claim_id("claim-42");
        let postmortem =
            MigrationPostmortemTemplate::for_incident(&report, MigrationIncidentSeverity::Sev1);
        assert!(!postmortem.links_backlog());
        assert!(!postmortem.prevention_actions.is_empty());
        assert!(!postmortem.timeline_prompts.is_empty());

        let linked = postmortem
            .backlog_item("br-555")
            .prevention_action("add-cross-validator");
        assert!(linked.links_backlog());
        assert!(
            linked
                .prevention_actions
                .iter()
                .any(|action| action == "add-cross-validator")
        );
        let json = linked.to_json();
        assert!(json.contains("\"claim_id\":\"claim-42\""));
        assert!(json.contains("\"comparator_id\":\"cmp-cert-7\""));
        assert!(json.contains("br-555"));
        assert!(json.contains("\"links_backlog\":true"));
    }

    #[test]
    fn playbook_resolves_complete_incident_response() {
        let report = MigrationIncidentReport::new(
            "INC-100",
            MigrationIncidentClass::SecurityBreach,
            MigrationRolloutStage::Ga,
        )
        .comparator_id("cmp-sec-1")
        .claim_id("claim-sec")
        .signal(MigrationIncidentSignal::new("sandbox-escape", "fs write"));
        // Attach every rollback artifact (artifact() consumes self, so fold).
        let report = rollback_artifact_set()
            .into_iter()
            .fold(report, MigrationIncidentReport::artifact);

        let playbook = MigrationIncidentResponsePlaybook::new();
        let response = playbook.resolve(&report);

        assert_eq!(response.class, MigrationIncidentClass::SecurityBreach);
        assert_eq!(response.severity, MigrationIncidentSeverity::Sev1);
        assert_eq!(
            response.emergency_hold_reason,
            EmergencyHoldReason::SecurityIncident
        );
        assert_eq!(response.rollback_authority, OperatorAuthority::OnCall);
        assert!(response.requires_immediate_rollback);
        assert!(response.blocks_promotion());
        assert!(response.readiness.is_ready());

        let json = response.to_json();
        assert!(json.contains("\"schema_version\":\"1.0.0\""));
        assert!(json.contains("\"class\":\"security-breach\""));
        assert!(json.contains("\"severity\":\"sev1\""));
        assert!(json.contains("\"emergency_hold_reason\":\"security-incident\""));
        assert!(json.contains("\"rollback_authority\":\"on-call\""));
        assert!(json.contains("\"requires_immediate_rollback\":true"));
        assert!(json.contains("rotate-exposed-credentials"));
        // The originating signal is carried into the evidence record.
        assert!(json.contains("sandbox-escape"));
    }

    #[test]
    fn incident_response_is_deterministic() {
        let build = || {
            let report = MigrationIncidentReport::new(
                "INC-200",
                MigrationIncidentClass::DeterminismDivergence,
                MigrationRolloutStage::Beta,
            )
            .comparator_id("cmp-det")
            .claim_id("claim-det");
            let report = rollback_artifact_set()
                .into_iter()
                .fold(report, MigrationIncidentReport::artifact);
            MigrationIncidentResponsePlaybook::new()
                .resolve(&report)
                .to_json()
        };
        assert_eq!(build(), build());
    }

    #[test]
    fn low_severity_incident_does_not_force_immediate_rollback() {
        let report = MigrationIncidentReport::new(
            "INC-300",
            MigrationIncidentClass::PerformanceRegression,
            MigrationRolloutStage::Alpha,
        );
        let response = MigrationIncidentResponsePlaybook::new().resolve(&report);
        assert_eq!(response.severity, MigrationIncidentSeverity::Sev3);
        assert!(!response.requires_immediate_rollback);
        // Sev3 still blocks further promotion until resolved.
        assert!(response.blocks_promotion());
        assert_eq!(response.rollback_authority, OperatorAuthority::ReleaseOwner);
    }
}
