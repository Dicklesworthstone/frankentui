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
