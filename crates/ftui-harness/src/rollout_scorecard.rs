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

        let mut reasons = Vec::new();
        if evidence.certification_pass_ratio < gate.min_certification_pass_ratio {
            reasons.push("certification-threshold");
        }
        if evidence.corpus_coverage_ratio < gate.min_corpus_coverage_ratio {
            reasons.push("corpus-coverage-threshold");
        }
        if evidence.reliability_pass_ratio < gate.min_reliability_pass_ratio {
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
            return 1.0;
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
}
