//! End-to-end optimization gauntlet (bd-3bxhj.8.21).
//!
//! This is the producer behind `scripts/doctor_frankentui_optimization_gauntlet_e2e.sh`.
//! It executes the full extreme-optimization loop —
//! baseline → profile → score → single-lever change → isomorphism verify →
//! re-profile → rollback drill — across a battery of scenarios, driving the real
//! kernels at each stage and proving the loop both *promotes* a safe optimization
//! and *refuses* every unsafe one:
//!
//! - **Positive path**: a measurable p95/p99 improvement with unchanged golden
//!   outputs and a ready rollback promotes.
//! - **Red paths**: a low opportunity score, a multi-lever change, a golden-output
//!   checksum mismatch, unstable profiling, and a post-change regression are each
//!   refused (rejected or rolled back) — never silently promoted.
//! - **Drift path**: a post-change hotspot shift reorders the next optimization
//!   candidates while still promoting.
//!
//! Each scenario emits one ledger line per loop *stage* (AC1) with upstream
//! artifact linkage and a replay command, plus a terminal `decision` line whose
//! observed decision is checked against the scenario's expected decision. The gate
//! fails closed if any scenario's decision is unsafe, any stage line is missing a
//! field, the positive path fails to promote, a red path is not refused, the
//! rollback does not restore the incumbent, or the drift path does not reorder
//! (AC2). The float-free ledger derives [`Eq`] and replays byte-identically; the
//! pipeline is materialized for release-candidate promotion gating (AC3).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::baseline_capture::{
    BaselineCaptureRunner, BaselineCommand, BaselineCorpusSlice, BaselineEnvironmentFingerprint,
    BaselineManifest, BaselineRawMeasurement, BaselineRunInput, BaselineScenario,
    BaselineSeedPolicy, BaselineVarianceEnvelope,
};
use crate::benchmark_regression_gate::BenchmarkProfileKey;
use crate::decision_loss_policy::RiskTier;
use crate::golden_isomorphism::{
    GoldenIsomorphismConfig, GoldenIsomorphismGate, GoldenOutput, GoldenRecord,
    IsomorphismInvariant, OptimizationChange as GoldenChange,
};
use crate::one_lever_policy::{OptimizationChange, RollbackPlan, run_one_lever_policy};
use crate::opportunity_scorer::{OpportunityConfig, OptimizationLever, run_opportunity_scoring};
use crate::profile_orchestrator::{
    ProfileModality, ProfileOrchestrationConfig, ProfileOrchestrationPlan, ProfileOrchestrator,
    ProfileSample, ProfilerInvocation,
};
use crate::recommendation_contract::EffortSize;
use crate::reprofile_loop::{
    BacklogCandidate, HotspotCost, OptimizationRound, ProfileSnapshot, ReprofileConfig,
    RoundVerdict, run_reprofile_loop,
};

/// Schema version for the in-memory optimization-gauntlet report.
pub const OPTIMIZATION_GAUNTLET_SCHEMA_VERSION: &str = "optimization-gauntlet-v1";

/// Schema version for the materialized optimization-gauntlet pipeline artifacts.
pub const OPTIMIZATION_GAUNTLET_PIPELINE_SCHEMA_VERSION: &str = "optimization-gauntlet-pipeline-v1";

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

// ── Loop-stage + scenario vocabulary ──────────────────────────────────────────

/// One stage of the optimization loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GauntletStage {
    /// Deterministic baseline capture.
    Baseline,
    /// Profiler hotspot extraction.
    Profile,
    /// Opportunity-matrix scoring (Score >= 2.0 gate).
    Score,
    /// One-lever change policy.
    OneLever,
    /// Golden-output isomorphism verification.
    Isomorphism,
    /// Iterative re-profile round.
    Reprofile,
    /// Rollback-readiness drill / rollback trigger.
    Rollback,
    /// The terminal decision (a summary pseudo-stage).
    Decision,
}

impl GauntletStage {
    /// The seven loop stages, in order (excludes the `Decision` pseudo-stage).
    pub const LOOP: [GauntletStage; 7] = [
        Self::Baseline,
        Self::Profile,
        Self::Score,
        Self::OneLever,
        Self::Isomorphism,
        Self::Reprofile,
        Self::Rollback,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Profile => "profile",
            Self::Score => "score",
            Self::OneLever => "one_lever",
            Self::Isomorphism => "isomorphism",
            Self::Reprofile => "reprofile",
            Self::Rollback => "rollback",
            Self::Decision => "decision",
        }
    }

    /// The kernel that backs this stage.
    fn kernel(self) -> &'static str {
        match self {
            Self::Baseline => "baseline_capture",
            Self::Profile => "profile_orchestrator",
            Self::Score => "opportunity_scorer",
            Self::OneLever => "one_lever_policy",
            Self::Isomorphism => "golden_isomorphism",
            Self::Reprofile => "reprofile_loop",
            Self::Rollback => "rollback_drill",
            Self::Decision => "gauntlet",
        }
    }

    /// The upstream stage that feeds this one (`"n/a"` for the first stage).
    fn upstream(self) -> &'static str {
        match self {
            Self::Baseline => "n/a",
            Self::Profile => "baseline",
            Self::Score => "profile",
            Self::OneLever => "score",
            Self::Isomorphism => "one_lever",
            Self::Reprofile => "isomorphism",
            Self::Rollback => "reprofile",
            Self::Decision => "rollback",
        }
    }
}

/// A gauntlet scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GauntletScenario {
    /// Positive path: a safe optimization promotes.
    PositivePromote,
    /// Red: a sub-threshold opportunity score is rejected.
    LowScoreReject,
    /// Red: a multi-lever change without an override is rejected.
    MultiLeverReject,
    /// Red: a golden-output checksum mismatch is rejected.
    ChecksumMismatch,
    /// Red: an unstable baseline (too few samples) is rejected.
    UnstableProfiling,
    /// Red: a post-change regression triggers a rollback to the incumbent.
    RegressionRollback,
    /// Drift: a post-change hotspot shift reorders the next candidates, still promoting.
    HotspotDrift,
}

impl GauntletScenario {
    /// All scenarios in canonical order.
    pub const ALL: [GauntletScenario; 7] = [
        Self::PositivePromote,
        Self::LowScoreReject,
        Self::MultiLeverReject,
        Self::ChecksumMismatch,
        Self::UnstableProfiling,
        Self::RegressionRollback,
        Self::HotspotDrift,
    ];

    /// Stable lowercase tag (the `scenario_id`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PositivePromote => "positive_promote",
            Self::LowScoreReject => "low_score_reject",
            Self::MultiLeverReject => "multi_lever_reject",
            Self::ChecksumMismatch => "checksum_mismatch",
            Self::UnstableProfiling => "unstable_profiling",
            Self::RegressionRollback => "regression_rollback",
            Self::HotspotDrift => "hotspot_drift",
        }
    }

    /// The decision the loop must reach for a safe outcome.
    #[must_use]
    pub fn expected_decision(self) -> &'static str {
        match self {
            Self::PositivePromote | Self::HotspotDrift => "promote",
            Self::LowScoreReject => "reject_low_score",
            Self::MultiLeverReject => "reject_multi_lever",
            Self::ChecksumMismatch => "reject_isomorphism",
            Self::UnstableProfiling => "reject_unstable",
            Self::RegressionRollback => "rollback",
        }
    }

    /// Whether this is a red (refusal) path.
    fn is_red_path(self) -> bool {
        matches!(
            self,
            Self::LowScoreReject
                | Self::MultiLeverReject
                | Self::ChecksumMismatch
                | Self::UnstableProfiling
                | Self::RegressionRollback
        )
    }
}

// ── Ledger ───────────────────────────────────────────────────────────────────

/// One optimization-gauntlet ledger line — a single loop stage, or the terminal
/// decision (float-free; derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GauntletLedgerEntry {
    /// Ledger schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// The scenario id.
    pub scenario_id: String,
    /// The loop stage (or `decision`).
    pub stage: GauntletStage,
    /// The 0-based stage index within the scenario.
    pub stage_index: usize,
    /// The kernel that produced this stage.
    pub kernel: String,
    /// The governing policy id.
    pub policy_id: String,
    /// The subject (candidate / change / hotspot id) under test.
    pub subject_id: String,
    /// The upstream stage feeding this one (artifact linkage).
    pub upstream_stage: String,
    /// The hotspot id, or `"n/a"`.
    pub hotspot_id: String,
    /// The isomorphism proof id, or `"n/a"`.
    pub proof_id: String,
    /// Machine-readable score terms (fixed-decimal), kernel-specific.
    pub score_terms: Vec<String>,
    /// The stage outcome tag.
    pub stage_outcome: String,
    /// Whether this stage's gate passed (the loop advances).
    pub gate_passed: bool,
    /// Whether this is the terminal loop stage that determined the decision.
    pub is_terminal_stage: bool,
    /// Whether the incumbent baseline was restored (rollback stage only).
    pub baseline_restored: bool,
    /// Whether the dominant hotspot shifted (re-profile stage only).
    pub bottleneck_shifted: bool,
    /// Whether the optimization backlog was reordered (re-profile stage only).
    pub backlog_reordered: bool,
    /// The scenario's observed terminal decision.
    pub scenario_decision: String,
    /// The scenario's expected terminal decision.
    pub expected_decision: String,
    /// Whether the observed decision matched the expected (safe).
    pub decision_safe: bool,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic single-command replay reference.
    pub reproduction_command: String,
}

fn required_fields_present(line: &GauntletLedgerEntry) -> bool {
    !line.schema_version.is_empty()
        && !line.run_id.is_empty()
        && !line.scenario_id.is_empty()
        && !line.kernel.is_empty()
        && !line.policy_id.is_empty()
        && !line.subject_id.is_empty()
        && !line.upstream_stage.is_empty()
        && !line.hotspot_id.is_empty()
        && !line.proof_id.is_empty()
        && !line.score_terms.is_empty()
        && !line.stage_outcome.is_empty()
        && !line.scenario_decision.is_empty()
        && !line.expected_decision.is_empty()
        && !line.detail.is_empty()
        && !line.reproduction_command.is_empty()
}

fn render_ledger_jsonl(ledger: &[GauntletLedgerEntry]) -> String {
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

// ── Stage record (internal) ────────────────────────────────────────────────────

struct StageRecord {
    stage: GauntletStage,
    subject_id: String,
    hotspot_id: String,
    proof_id: String,
    score_terms: Vec<String>,
    stage_outcome: String,
    gate_passed: bool,
    baseline_restored: bool,
    bottleneck_shifted: bool,
    backlog_reordered: bool,
    detail: String,
}

impl StageRecord {
    fn new(stage: GauntletStage, subject_id: impl Into<String>) -> Self {
        Self {
            stage,
            subject_id: subject_id.into(),
            hotspot_id: "n/a".to_string(),
            proof_id: "n/a".to_string(),
            score_terms: Vec::new(),
            stage_outcome: String::new(),
            gate_passed: true,
            baseline_restored: false,
            bottleneck_shifted: false,
            backlog_reordered: false,
            detail: String::new(),
        }
    }
}

struct ScenarioTrace {
    decision: String,
    stages: Vec<StageRecord>,
}

// ── Kernel-driving helpers (mirroring the optimization_control_tests fixtures) ─

fn baseline_manifest() -> BaselineManifest {
    use std::collections::BTreeMap;
    let mut tools = BTreeMap::new();
    tools.insert("cargo".to_string(), "1.90.0".to_string());
    BaselineManifest::new(
        BaselineEnvironmentFingerprint::new(
            "linux",
            "x86_64",
            "ci-cpu",
            16,
            Some(32 * 1024 * 1024 * 1024),
            tools,
            vec![],
        ),
        BaselineCorpusSlice::new(
            "corpus-1",
            vec!["fix-a".to_string()],
            vec!["stage-a".to_string()],
            "all",
        ),
        BaselineSeedPolicy::new(42, "baseline"),
        BaselineVarianceEnvelope::default(),
    )
}

fn baseline_scenario() -> BaselineScenario {
    use std::collections::BTreeMap;
    BaselineScenario::new(
        "stage-a",
        "fix-a",
        "scn-1",
        "wl-1",
        1000,
        vec![],
        BaselineCommand::new(
            "doctor_frankentui",
            vec!["replay".to_string()],
            "/repo",
            BTreeMap::new(),
        ),
    )
}

/// Run the baseline stage. `stable` controls whether enough samples are captured.
fn stage_baseline(stable: bool) -> StageRecord {
    let measurements = if stable {
        vec![
            BaselineRawMeasurement::warmup(0, 5.0),
            BaselineRawMeasurement::measurement(1, 1.0, 2.0, 3.0, 1000.0, 1_000_000),
            BaselineRawMeasurement::measurement(2, 1.0, 2.0, 3.0, 1010.0, 1_000_000),
            BaselineRawMeasurement::measurement(3, 1.0, 2.0, 3.0, 990.0, 1_000_000),
            BaselineRawMeasurement::measurement(4, 1.0, 2.0, 3.0, 1005.0, 1_000_000),
            BaselineRawMeasurement::measurement(5, 1.0, 2.0, 3.0, 995.0, 1_000_000),
        ]
    } else {
        // Too few samples -> an insufficient-measurements variance violation.
        vec![
            BaselineRawMeasurement::warmup(0, 5.0),
            BaselineRawMeasurement::measurement(1, 1.0, 2.0, 3.0, 1000.0, 1_000_000),
        ]
    };
    let input = BaselineRunInput::new("run-baseline", baseline_scenario(), measurements);
    let report = BaselineCaptureRunner::new(baseline_manifest()).capture(vec![input]);
    let ok = report.variance_violations.is_empty();
    let (p50, p95, p99, tput) = report.records.first().map_or((0.0, 0.0, 0.0, 0.0), |r| {
        (
            r.summary.latency_p50_ms,
            r.summary.latency_p95_ms,
            r.summary.latency_p99_ms,
            r.summary.throughput_ops_per_sec,
        )
    });
    let mut rec = StageRecord::new(GauntletStage::Baseline, "scn-1");
    rec.score_terms = vec![fmt6(p50), fmt6(p95), fmt6(p99), fmt6(tput)];
    rec.gate_passed = ok;
    rec.stage_outcome = if ok {
        "captured".to_string()
    } else {
        "unstable".to_string()
    };
    rec.detail = format!(
        "baseline p50/p95/p99 = {p50:.3}/{p95:.3}/{p99:.3} ms; variance_violations={}",
        report.variance_violations.len()
    );
    rec
}

fn profiler_plan() -> ProfileOrchestrationPlan {
    ProfileOrchestrationPlan::new(
        "target-x",
        BenchmarkProfileKey::new("stage-a", "fix-a"),
        42,
        vec![ProfilerInvocation::new(
            ProfileModality::Cpu,
            "perf",
            vec!["record".to_string()],
            "artifacts/cpu.folded",
        )],
    )
}

fn profiler_samples() -> Vec<ProfileSample> {
    vec![
        ProfileSample::new(
            ProfileModality::Cpu,
            "render",
            "ui/render.rs:42",
            800.0,
            1200,
        )
        .with_total_weight_hint(1000.0),
        ProfileSample::new(
            ProfileModality::Allocation,
            "render",
            "ui/render.rs:42",
            4096.0,
            1200,
        ),
        ProfileSample::new(ProfileModality::Cpu, "layout", "ui/layout.rs:7", 300.0, 600),
    ]
}

/// Run the profile stage. Returns the stage record and the top hotspot id.
fn stage_profile() -> (StageRecord, String) {
    let report = ProfileOrchestrator::new(ProfileOrchestrationConfig::default().with_top_n(5))
        .orchestrate(profiler_plan(), profiler_samples());
    let top = report.top_hotspots.first();
    let hotspot_id = top.map_or_else(|| "none".to_string(), |h| h.hotspot_id.clone());
    let symbol = top.map_or_else(|| "none".to_string(), |h| h.symbol.clone());
    let (opp, attr) = top.map_or((0.0, 0.0), |h| {
        (h.opportunity_score, h.normalized_attribution_percent)
    });
    let mut rec = StageRecord::new(GauntletStage::Profile, symbol.clone());
    rec.hotspot_id = hotspot_id.clone();
    rec.score_terms = vec![fmt6(opp), fmt6(attr)];
    rec.gate_passed = !report.top_hotspots.is_empty();
    rec.stage_outcome = "ranked".to_string();
    rec.detail =
        format!("top hotspot {symbol} ({hotspot_id}) attribution {attr:.2}% opportunity {opp:.4}");
    (rec, hotspot_id)
}

/// Run the score stage against the top hotspot. `strong` controls activation.
fn stage_score(strong: bool, hotspot_id: &str) -> StageRecord {
    let lever = if strong {
        OptimizationLever::new(
            "lever.gauntlet",
            hotspot_id,
            "simd diff fast-path",
            9.0,
            0.9,
            EffortSize::Small,
        )
        .with_evidence([
            "baseline.gauntlet".to_string(),
            "profile.gauntlet".to_string(),
        ])
    } else {
        OptimizationLever::new(
            "lever.gauntlet",
            hotspot_id,
            "marginal tweak",
            3.0,
            0.4,
            EffortSize::Large,
        )
        .with_evidence(["baseline.gauntlet".to_string()])
    };
    let report = run_opportunity_scoring(
        "gauntlet/score",
        &[lever],
        &[],
        &OpportunityConfig::default(),
    );
    let card = report.card("lever.gauntlet");
    let activated = card.is_some_and(|c| c.activated);
    let mut rec = StageRecord::new(GauntletStage::Score, "lever.gauntlet");
    rec.hotspot_id = hotspot_id.to_string();
    rec.score_terms = card.map_or_else(
        || vec![fmt6(0.0)],
        |c| {
            vec![
                c.impact.clone(),
                c.confidence.clone(),
                c.effort_cost.clone(),
                c.score.clone(),
            ]
        },
    );
    rec.gate_passed = activated;
    rec.stage_outcome = if activated {
        "activated".to_string()
    } else {
        "blocked".to_string()
    };
    rec.detail = card.map_or_else(
        || "no score card".to_string(),
        |c| format!("score {} status {}", c.score, c.status.as_str()),
    );
    rec
}

fn ready_rollback() -> RollbackPlan {
    RollbackPlan::new(
        "rbk.gauntlet",
        "git revert <sha>",
        RiskTier::Low,
        ["cargo test -p ftui-render --lib".to_string()],
    )
}

/// Run the one-lever stage. `multi` injects a second (un-overridden) lever.
fn stage_one_lever(multi: bool) -> (StageRecord, bool) {
    let change = if multi {
        OptimizationChange::new(
            "chg.gauntlet",
            ["lever.a".to_string(), "lever.b".to_string()],
        )
        .with_rollback(ready_rollback())
    } else {
        OptimizationChange::new("chg.gauntlet", ["lever.a".to_string()])
            .with_rollback(ready_rollback())
    };
    let report = run_one_lever_policy("gauntlet/one_lever", &[change]);
    let card = report.card("chg.gauntlet");
    let accepted = card.is_some_and(|c| c.accepted);
    let rollback_ready = card.is_some_and(|c| c.rollback_ready);
    let lever_count = card.map_or(0, |c| c.lever_count);
    let mut rec = StageRecord::new(GauntletStage::OneLever, "chg.gauntlet");
    rec.score_terms = vec![fmt6(lever_count as f64)];
    rec.gate_passed = accepted;
    rec.stage_outcome = if accepted {
        "accepted".to_string()
    } else {
        "rejected".to_string()
    };
    rec.detail = card.map_or_else(
        || "no policy card".to_string(),
        |c| {
            format!(
                "lever_count={} accepted={} rollback_ready={}",
                c.lever_count, c.accepted, c.rollback_ready
            )
        },
    );
    (rec, rollback_ready)
}

fn golden_output() -> Vec<GoldenOutput> {
    vec![GoldenOutput::new(
        "fix-a",
        "wl-1",
        vec![GoldenRecord::new("row1", 0).with_floats(vec![1.230001])],
    )]
}

/// Run the isomorphism stage. `preserved` controls behavior preservation.
fn stage_isomorphism(preserved: bool) -> StageRecord {
    let (candidate, change) = if preserved {
        (
            vec![GoldenOutput::new(
                "fix-a",
                "wl-1",
                vec![GoldenRecord::new("row1", 0).with_floats(vec![1.230004])],
            )],
            GoldenChange::new("chg.gauntlet", "base-0", "vectorize").with_invariant(
                IsomorphismInvariant::FloatingPoint {
                    tolerance_decimals: 3,
                },
            ),
        )
    } else {
        (
            vec![GoldenOutput::new(
                "fix-a",
                "wl-1",
                vec![GoldenRecord::new("row1", 0).with_tokens(vec!["CHANGED".to_string()])],
            )],
            GoldenChange::new("chg.gauntlet", "base-0", "reorder"),
        )
    };
    let report = GoldenIsomorphismGate::new(GoldenIsomorphismConfig::default()).verify(
        &change,
        &golden_output(),
        &candidate,
    );
    let allowed = report.promotion_allowed && report.proof.behavior_preserved;
    let mut rec = StageRecord::new(GauntletStage::Isomorphism, report.change_id.clone());
    rec.proof_id = report.proof.proof_id.clone();
    rec.score_terms = vec![
        fmt6(report.summary.matched as f64),
        fmt6(report.summary.mismatched as f64),
    ];
    rec.gate_passed = allowed;
    rec.stage_outcome = if allowed {
        "preserved".to_string()
    } else {
        "mismatch".to_string()
    };
    rec.detail = format!(
        "behavior_preserved={} promotion_allowed={} matched={} mismatched={}",
        report.proof.behavior_preserved,
        report.promotion_allowed,
        report.summary.matched,
        report.summary.mismatched
    );
    rec
}

fn snapshot(p99: f64, hotspots: &[(&str, f64)], stable: bool) -> ProfileSnapshot {
    ProfileSnapshot {
        p50_us: 32.0,
        p95_us: 70.0,
        p99_us: p99,
        throughput: 11000.0,
        memory_bytes: 4_900_000.0,
        hotspots: hotspots
            .iter()
            .map(|(id, share)| HotspotCost {
                hotspot_id: (*id).to_string(),
                cost_share: *share,
            })
            .collect(),
        stable,
    }
}

/// Which kind of re-profile round to run.
#[derive(Clone, Copy)]
enum ReprofileKind {
    ImproveNoShift,
    ImproveShift,
    Regress,
}

/// Run the re-profile stage. Returns the record, whether it regressed, and the
/// reordered/shift flags.
fn stage_reprofile(kind: ReprofileKind) -> (StageRecord, bool) {
    let (before, after) = match kind {
        ReprofileKind::ImproveNoShift => (
            snapshot(120.0, &[("hot.render", 0.6), ("hot.layout", 0.4)], true),
            snapshot(85.0, &[("hot.render", 0.55), ("hot.layout", 0.45)], true),
        ),
        ReprofileKind::ImproveShift => (
            snapshot(120.0, &[("hot.render", 0.6), ("hot.layout", 0.4)], true),
            snapshot(85.0, &[("hot.render", 0.3), ("hot.layout", 0.55)], true),
        ),
        ReprofileKind::Regress => (
            snapshot(85.0, &[("hot.render", 0.6), ("hot.layout", 0.4)], true),
            snapshot(130.0, &[("hot.render", 0.6), ("hot.layout", 0.4)], true),
        ),
    };
    let round = OptimizationRound {
        round_number: 1,
        change_id: "chg.gauntlet".to_string(),
        lever_id: "lever.gauntlet".to_string(),
        before,
        after,
        backlog: vec![
            BacklogCandidate {
                candidate_id: "cand.render".to_string(),
                hotspot_id: "hot.render".to_string(),
                confidence: 0.85,
                effort: EffortSize::Medium,
            },
            BacklogCandidate {
                candidate_id: "cand.layout".to_string(),
                hotspot_id: "hot.layout".to_string(),
                confidence: 0.85,
                effort: EffortSize::Medium,
            },
        ],
    };
    let report = run_reprofile_loop("gauntlet/reprofile", &[round], &ReprofileConfig::default());
    let entry = report.round(1);
    let verdict = entry.map_or(RoundVerdict::Pause, |e| e.verdict);
    let regressed = entry.is_some_and(|e| e.triage_hint.contains("regress"));
    let shifted = entry.is_some_and(|e| e.bottleneck_shifted);
    let reordered = entry.is_some_and(|e| {
        e.reranked_backlog.first().map(|c| c.candidate_id.as_str()) == Some("cand.layout")
    });
    let (p99b, p99a) = entry.map_or((String::new(), String::new()), |e| {
        (e.p99_before.clone(), e.p99_after.clone())
    });
    let mut rec = StageRecord::new(GauntletStage::Reprofile, "chg.gauntlet");
    rec.hotspot_id = entry.map_or_else(|| "n/a".to_string(), |e| e.bottleneck_after.clone());
    rec.score_terms = vec![
        if p99b.is_empty() { fmt6(0.0) } else { p99b },
        if p99a.is_empty() { fmt6(0.0) } else { p99a },
    ];
    rec.gate_passed = matches!(verdict, RoundVerdict::Continue);
    rec.bottleneck_shifted = shifted;
    rec.backlog_reordered = reordered;
    rec.stage_outcome = verdict.as_str().to_string();
    rec.detail = entry.map_or_else(
        || "no round".to_string(),
        |e| {
            format!(
                "verdict {} shifted={} reordered={} {}",
                e.verdict.as_str(),
                shifted,
                reordered,
                e.triage_hint
            )
        },
    );
    (rec, regressed)
}

/// Run the rollback drill stage. When `regressed`, the drill triggers a rollback
/// (restoring the incumbent iff a rollback plan is ready); otherwise it verifies
/// rollback readiness without triggering.
fn stage_rollback(regressed: bool, rollback_ready: bool) -> StageRecord {
    let mut rec = StageRecord::new(GauntletStage::Rollback, "rbk.gauntlet");
    rec.score_terms = vec![
        fmt6(f64::from(u8::from(rollback_ready))),
        fmt6(f64::from(u8::from(regressed))),
    ];
    if regressed {
        let restored = rollback_ready;
        rec.gate_passed = restored;
        rec.baseline_restored = restored;
        rec.stage_outcome = "rollback_triggered".to_string();
        rec.detail =
            format!("post-change regression -> rollback triggered; incumbent restored={restored}");
    } else {
        rec.gate_passed = rollback_ready;
        rec.stage_outcome = "rollback_ready".to_string();
        rec.detail = format!("rollback-readiness drill: ready={rollback_ready} (not triggered)");
    }
    rec
}

// ── Scenario runner (the full loop) ────────────────────────────────────────────

fn run_scenario(scenario: GauntletScenario) -> ScenarioTrace {
    let mut stages: Vec<StageRecord> = Vec::new();

    // Stage 1 — baseline.
    let baseline = stage_baseline(scenario != GauntletScenario::UnstableProfiling);
    let baseline_ok = baseline.gate_passed;
    stages.push(baseline);
    if !baseline_ok {
        return ScenarioTrace {
            decision: "reject_unstable".to_string(),
            stages,
        };
    }

    // Stage 2 — profile.
    let (profile, hotspot_id) = stage_profile();
    stages.push(profile);

    // Stage 3 — score.
    let score = stage_score(scenario != GauntletScenario::LowScoreReject, &hotspot_id);
    let score_ok = score.gate_passed;
    stages.push(score);
    if !score_ok {
        return ScenarioTrace {
            decision: "reject_low_score".to_string(),
            stages,
        };
    }

    // Stage 4 — one-lever.
    let (one_lever, rollback_ready) =
        stage_one_lever(scenario == GauntletScenario::MultiLeverReject);
    let lever_ok = one_lever.gate_passed;
    stages.push(one_lever);
    if !lever_ok {
        return ScenarioTrace {
            decision: "reject_multi_lever".to_string(),
            stages,
        };
    }

    // Stage 5 — isomorphism.
    let iso = stage_isomorphism(scenario != GauntletScenario::ChecksumMismatch);
    let iso_ok = iso.gate_passed;
    stages.push(iso);
    if !iso_ok {
        return ScenarioTrace {
            decision: "reject_isomorphism".to_string(),
            stages,
        };
    }

    // Stage 6 — re-profile.
    let kind = match scenario {
        GauntletScenario::RegressionRollback => ReprofileKind::Regress,
        GauntletScenario::HotspotDrift => ReprofileKind::ImproveShift,
        _ => ReprofileKind::ImproveNoShift,
    };
    let (reprofile, regressed) = stage_reprofile(kind);
    stages.push(reprofile);

    // Stage 7 — rollback drill.
    let rollback = stage_rollback(regressed, rollback_ready);
    stages.push(rollback);

    let decision = if regressed { "rollback" } else { "promote" };
    ScenarioTrace {
        decision: decision.to_string(),
        stages,
    }
}

// ── Engine ───────────────────────────────────────────────────────────────────

/// The deterministic optimization-gauntlet engine.
#[derive(Debug, Clone)]
pub struct OptimizationGauntlet {
    run_id: String,
    label: String,
}

impl OptimizationGauntlet {
    /// Construct a gauntlet with a deterministic run id derived from its label.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        let run_id = format!(
            "optimization-gauntlet-{}",
            short_hash(&stable_hash(&format!(
                "{OPTIMIZATION_GAUNTLET_SCHEMA_VERSION}|{label}"
            )))
        );
        Self { run_id, label }
    }

    /// The deterministic run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Run all gauntlet scenarios and assemble the report.
    #[must_use]
    pub fn run(&self) -> GauntletReport {
        let mut ledger: Vec<GauntletLedgerEntry> = Vec::new();
        for scenario in GauntletScenario::ALL {
            let trace = run_scenario(scenario);
            let expected = scenario.expected_decision();
            let safe = trace.decision == expected;
            let last_index = trace.stages.len().saturating_sub(1);
            for (idx, rec) in trace.stages.iter().enumerate() {
                ledger.push(self.stage_entry(
                    scenario,
                    rec,
                    idx,
                    idx == last_index,
                    &trace.decision,
                    expected,
                    safe,
                ));
            }
            // Terminal decision line.
            ledger.push(self.decision_entry(
                scenario,
                trace.stages.len(),
                &trace.decision,
                expected,
                safe,
            ));
        }

        let evidence_checksum = sha256_hex(render_ledger_jsonl(&ledger).as_bytes());
        let report_id = format!(
            "optimization-gauntlet-report-{}",
            short_hash(&stable_hash(&format!(
                "{}|{evidence_checksum}",
                self.run_id
            )))
        );
        let summary = self.summarize(&ledger, &report_id, &evidence_checksum);
        let gate_passes = summary.gate_passes;
        let exported_json_stats = export_json_stats(&report_id, &summary, &ledger);
        let replay_command = format!(
            "cargo run -p doctor_frankentui -- optimization-gauntlet --label '{}' # run {}",
            self.label, self.run_id
        );

        GauntletReport {
            schema_version: OPTIMIZATION_GAUNTLET_SCHEMA_VERSION.to_string(),
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

    #[allow(clippy::too_many_arguments)]
    fn stage_entry(
        &self,
        scenario: GauntletScenario,
        rec: &StageRecord,
        index: usize,
        is_terminal: bool,
        decision: &str,
        expected: &str,
        safe: bool,
    ) -> GauntletLedgerEntry {
        GauntletLedgerEntry {
            schema_version: OPTIMIZATION_GAUNTLET_SCHEMA_VERSION.to_string(),
            run_id: self.run_id.clone(),
            scenario_id: scenario.as_str().to_string(),
            stage: rec.stage,
            stage_index: index,
            kernel: rec.stage.kernel().to_string(),
            policy_id: format!("opt-gauntlet/{}", rec.stage.as_str()),
            subject_id: rec.subject_id.clone(),
            upstream_stage: rec.stage.upstream().to_string(),
            hotspot_id: rec.hotspot_id.clone(),
            proof_id: rec.proof_id.clone(),
            score_terms: rec.score_terms.clone(),
            stage_outcome: rec.stage_outcome.clone(),
            gate_passed: rec.gate_passed,
            is_terminal_stage: is_terminal,
            baseline_restored: rec.baseline_restored,
            bottleneck_shifted: rec.bottleneck_shifted,
            backlog_reordered: rec.backlog_reordered,
            scenario_decision: decision.to_string(),
            expected_decision: expected.to_string(),
            decision_safe: safe,
            detail: rec.detail.clone(),
            reproduction_command: format!(
                "cargo run -p doctor_frankentui -- optimization-gauntlet --label '{}' # run {} scenario {}",
                self.label,
                self.run_id,
                scenario.as_str()
            ),
        }
    }

    fn decision_entry(
        &self,
        scenario: GauntletScenario,
        stage_index: usize,
        decision: &str,
        expected: &str,
        safe: bool,
    ) -> GauntletLedgerEntry {
        GauntletLedgerEntry {
            schema_version: OPTIMIZATION_GAUNTLET_SCHEMA_VERSION.to_string(),
            run_id: self.run_id.clone(),
            scenario_id: scenario.as_str().to_string(),
            stage: GauntletStage::Decision,
            stage_index,
            kernel: GauntletStage::Decision.kernel().to_string(),
            policy_id: "opt-gauntlet/decision".to_string(),
            subject_id: scenario.as_str().to_string(),
            upstream_stage: GauntletStage::Decision.upstream().to_string(),
            hotspot_id: "n/a".to_string(),
            proof_id: "n/a".to_string(),
            score_terms: vec![fmt6(stage_index as f64)],
            stage_outcome: decision.to_string(),
            gate_passed: safe,
            is_terminal_stage: false,
            baseline_restored: false,
            bottleneck_shifted: false,
            backlog_reordered: false,
            scenario_decision: decision.to_string(),
            expected_decision: expected.to_string(),
            decision_safe: safe,
            detail: format!(
                "scenario '{}' decided '{decision}' (expected '{expected}', safe={safe})",
                scenario.as_str()
            ),
            reproduction_command: format!(
                "cargo run -p doctor_frankentui -- optimization-gauntlet --label '{}' # run {} scenario {}",
                self.label,
                self.run_id,
                scenario.as_str()
            ),
        }
    }

    fn summarize(
        &self,
        ledger: &[GauntletLedgerEntry],
        report_id: &str,
        evidence_checksum: &str,
    ) -> GauntletSummary {
        let required_fields_complete = ledger.iter().all(required_fields_present);
        // Per-scenario decisions come from the Decision lines.
        let decisions: Vec<&GauntletLedgerEntry> = ledger
            .iter()
            .filter(|l| l.stage == GauntletStage::Decision)
            .collect();
        let all_decisions_safe = decisions.iter().all(|d| d.decision_safe);
        // Every loop stage kind appears (positive path covers all seven).
        let stages_seen: BTreeSet<&str> = ledger
            .iter()
            .filter(|l| l.stage != GauntletStage::Decision)
            .map(|l| l.stage.as_str())
            .collect();
        let loop_stages_covered = GauntletStage::LOOP
            .iter()
            .all(|s| stages_seen.contains(s.as_str()));
        // Positive path promoted (not vacuous).
        let positive_promoted = decisions.iter().any(|d| {
            d.scenario_id == "positive_promote"
                && d.scenario_decision == "promote"
                && d.decision_safe
        });
        // Every red path reached its expected refusal.
        let red_paths_covered = GauntletScenario::ALL
            .iter()
            .filter(|s| s.is_red_path())
            .all(|s| {
                decisions.iter().any(|d| {
                    d.scenario_id == s.as_str()
                        && d.scenario_decision == s.expected_decision()
                        && d.decision_safe
                })
            });
        // The regression scenario rolled back AND restored the incumbent.
        let rollback_restored = ledger.iter().any(|l| {
            l.scenario_id == "regression_rollback"
                && l.stage == GauntletStage::Rollback
                && l.stage_outcome == "rollback_triggered"
                && l.baseline_restored
        }) && decisions
            .iter()
            .any(|d| d.scenario_id == "regression_rollback" && d.scenario_decision == "rollback");
        // The drift scenario shifted its bottleneck AND reordered the backlog.
        let drift_reordered = ledger.iter().any(|l| {
            l.scenario_id == "hotspot_drift"
                && l.stage == GauntletStage::Reprofile
                && l.bottleneck_shifted
                && l.backlog_reordered
        });
        let gate_passes = required_fields_complete
            && all_decisions_safe
            && loop_stages_covered
            && positive_promoted
            && red_paths_covered
            && rollback_restored
            && drift_reordered;

        GauntletSummary {
            schema_version: OPTIMIZATION_GAUNTLET_SCHEMA_VERSION.to_string(),
            report_id: report_id.to_string(),
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum: evidence_checksum.to_string(),
            total_scenarios: decisions.len(),
            total_stage_lines: ledger.len(),
            loop_stages_covered: stages_seen.len(),
            required_fields_complete,
            all_decisions_safe,
            all_loop_stages_present: loop_stages_covered,
            positive_promoted,
            red_paths_covered,
            rollback_restored,
            drift_reordered,
            gate_passes,
            replay_command: format!(
                "cargo run -p doctor_frankentui -- optimization-gauntlet --label '{}' # run {}",
                self.label, self.run_id
            ),
        }
    }
}

/// Run the optimization gauntlet over all scenarios with the given label.
#[must_use]
pub fn run_optimization_gauntlet(label: &str) -> GauntletReport {
    OptimizationGauntlet::new(label).run()
}

// ── Report + summary + stats ─────────────────────────────────────────────────

/// Machine-readable summary of one optimization-gauntlet run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GauntletSummary {
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
    /// Total scenarios.
    pub total_scenarios: usize,
    /// Total ledger lines (stages + decisions).
    pub total_stage_lines: usize,
    /// Distinct loop stages observed.
    pub loop_stages_covered: usize,
    /// Whether every ledger line has all mandated fields (AC1).
    pub required_fields_complete: bool,
    /// Whether every scenario's decision matched its expectation (AC2).
    pub all_decisions_safe: bool,
    /// Whether all seven loop stages were exercised.
    pub all_loop_stages_present: bool,
    /// Whether the positive path promoted (non-vacuous).
    pub positive_promoted: bool,
    /// Whether every red path was refused (AC2).
    pub red_paths_covered: bool,
    /// Whether the regression rollback restored the incumbent baseline.
    pub rollback_restored: bool,
    /// Whether the drift path reordered the next candidates.
    pub drift_reordered: bool,
    /// Whether the fail-closed gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GauntletStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The in-memory optimization-gauntlet report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GauntletReport {
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
    pub ledger: Vec<GauntletLedgerEntry>,
    /// Aggregate summary.
    pub summary: GauntletSummary,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: GauntletStatsArtifact,
}

impl GauntletReport {
    /// Render the evidence ledger as JSONL (one entry per line).
    #[must_use]
    pub fn render_ledger_jsonl(&self) -> String {
        render_ledger_jsonl(&self.ledger)
    }

    /// The terminal decision lines, one per scenario.
    #[must_use]
    pub fn decisions(&self) -> Vec<&GauntletLedgerEntry> {
        self.ledger
            .iter()
            .filter(|l| l.stage == GauntletStage::Decision)
            .collect()
    }
}

fn export_json_stats(
    report_id: &str,
    summary: &GauntletSummary,
    ledger: &[GauntletLedgerEntry],
) -> GauntletStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        summary: &'a GauntletSummary,
        ledger: &'a [GauntletLedgerEntry],
    }
    let content = serde_json::to_string_pretty(&Export {
        schema_version: OPTIMIZATION_GAUNTLET_SCHEMA_VERSION,
        report_id,
        summary,
        ledger,
    })
    .unwrap_or_else(|error| error.to_string());
    GauntletStatsArtifact {
        path: format!("{report_id}/optimization_gauntlet_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

// ── Pipeline (materialized artifacts) ────────────────────────────────────────

/// Configuration for the materialized optimization-gauntlet pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct GauntletPipelineConfig {
    /// Run directory name under the run-root.
    pub run_name: String,
    /// Run label used for deterministic ids.
    pub label: String,
}

impl Default for GauntletPipelineConfig {
    fn default() -> Self {
        Self {
            run_name: "optimization_gauntlet".to_string(),
            label: "optimization-gauntlet/e2e".to_string(),
        }
    }
}

/// A materialized pipeline artifact (path + integrity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GauntletArtifact {
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
pub struct GauntletPipelineOutcome {
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
    pub summary: GauntletSummary,
    /// All generated artifacts (with integrity hashes).
    pub artifacts: Vec<GauntletArtifact>,
}

fn artifact_of(file: &str, content: &str) -> GauntletArtifact {
    GauntletArtifact {
        name: file
            .trim_end_matches(".json")
            .trim_end_matches(".jsonl")
            .to_string(),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// Run the optimization gauntlet and materialize the full evidence pipeline under
/// `run_root/<run_name>/`.
///
/// # Errors
/// Returns an error if a run directory or artifact cannot be created/serialized.
pub fn run_optimization_gauntlet_pipeline(
    run_root: &Path,
    config: &GauntletPipelineConfig,
) -> crate::error::Result<GauntletPipelineOutcome> {
    let report = OptimizationGauntlet::new(config.label.as_str()).run();

    let ledger_content = report.render_ledger_jsonl();
    let stats_content = report.exported_json_stats.content.clone();
    let summary_content = serde_json::to_string_pretty(&report.summary)?;

    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let ledger_file = "evidence_ledger.jsonl";
    let stats_file = "optimization_gauntlet_stats.json";
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
        artifacts: &'a [GauntletArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: OPTIMIZATION_GAUNTLET_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        gate_passes: report.gate_passes,
        artifacts: &artifacts,
    })?;
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(GauntletPipelineOutcome {
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

/// CLI arguments for the `optimization-gauntlet` command.
#[derive(Debug, clap::Args)]
pub struct OptimizationGauntletArgs {
    /// Run-root directory; artifacts land under `<run-root>/<run-name>/`.
    #[arg(
        long = "run-root",
        default_value = "/tmp/doctor_frankentui/optimization_gauntlet"
    )]
    pub run_root: PathBuf,

    /// Run directory name.
    #[arg(long = "run-name", default_value = "optimization_gauntlet")]
    pub run_name: String,

    /// Run label used for deterministic ids.
    #[arg(long = "label", default_value = "optimization-gauntlet/e2e")]
    pub label: String,
}

/// Run the `optimization-gauntlet` command: materialize the pipeline and apply the
/// fail-closed gate.
///
/// # Errors
/// Returns [`crate::error::DoctorError::Exit`] with a non-zero code when the gate
/// fails (an unsafe decision, a missing field, an uncovered red path, a
/// non-restoring rollback, or a missing drift reorder), or an I/O error if
/// artifacts cannot be materialized.
pub fn run_optimization_gauntlet_command(
    args: OptimizationGauntletArgs,
) -> crate::error::Result<()> {
    let config = GauntletPipelineConfig {
        run_name: args.run_name,
        label: args.label,
    };
    let outcome = run_optimization_gauntlet_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("optimization gauntlet"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!(
            "scenarios: {} | loop stages: {} | positive promoted: {} | red paths covered: {}",
            summary.total_scenarios,
            summary.loop_stages_covered,
            summary.positive_promoted,
            summary.red_paths_covered
        ));
        ui.info(&format!(
            "rollback restored: {} | drift reordered: {} | all decisions safe: {}",
            summary.rollback_restored, summary.drift_reordered, summary.all_decisions_safe
        ));
        if summary.gate_passes {
            ui.success("optimization-gauntlet gate PASSED");
        } else {
            ui.error("optimization-gauntlet gate FAILED");
        }
    }

    if summary.gate_passes {
        Ok(())
    } else {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "optimization-gauntlet gate failed: required_fields_complete={}, all_decisions_safe={}, positive_promoted={}, red_paths_covered={}, rollback_restored={}, drift_reordered={}",
                summary.required_fields_complete,
                summary.all_decisions_safe,
                summary.positive_promoted,
                summary.red_paths_covered,
                summary.rollback_restored,
                summary.drift_reordered
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn decision(report: &GauntletReport, scenario: &str) -> String {
        report
            .decisions()
            .into_iter()
            .find(|d| d.scenario_id == scenario)
            .map(|d| d.scenario_decision.clone())
            .expect("scenario decision present")
    }

    #[test]
    fn gauntlet_gate_passes_and_covers_all_scenarios() {
        let report = run_optimization_gauntlet("og/test");
        assert!(report.gate_passes, "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_scenarios, GauntletScenario::ALL.len());
        assert!(report.summary.all_decisions_safe);
        assert!(report.summary.all_loop_stages_present);
        assert!(report.summary.positive_promoted);
        assert!(report.summary.red_paths_covered);
        assert!(report.summary.rollback_restored);
        assert!(report.summary.drift_reordered);
        assert!(report.ledger.iter().all(required_fields_present));
    }

    #[test]
    fn every_scenario_reaches_its_expected_decision() {
        let report = run_optimization_gauntlet("og/test");
        for scenario in GauntletScenario::ALL {
            assert_eq!(
                decision(&report, scenario.as_str()),
                scenario.expected_decision(),
                "scenario {} took the wrong decision",
                scenario.as_str()
            );
        }
    }

    #[test]
    fn positive_path_runs_full_loop_and_promotes() {
        let report = run_optimization_gauntlet("og/test");
        let stages: Vec<&str> = report
            .ledger
            .iter()
            .filter(|l| l.scenario_id == "positive_promote" && l.stage != GauntletStage::Decision)
            .map(|l| l.stage.as_str())
            .collect();
        assert_eq!(
            stages,
            vec![
                "baseline",
                "profile",
                "score",
                "one_lever",
                "isomorphism",
                "reprofile",
                "rollback"
            ]
        );
        assert_eq!(decision(&report, "positive_promote"), "promote");
    }

    #[test]
    fn low_score_short_circuits_at_score() {
        let report = run_optimization_gauntlet("og/test");
        let stages: Vec<&str> = report
            .ledger
            .iter()
            .filter(|l| l.scenario_id == "low_score_reject" && l.stage != GauntletStage::Decision)
            .map(|l| l.stage.as_str())
            .collect();
        assert_eq!(stages, vec!["baseline", "profile", "score"]);
        assert_eq!(decision(&report, "low_score_reject"), "reject_low_score");
    }

    #[test]
    fn unstable_profiling_short_circuits_at_baseline() {
        let report = run_optimization_gauntlet("og/test");
        let stages: Vec<&str> = report
            .ledger
            .iter()
            .filter(|l| l.scenario_id == "unstable_profiling" && l.stage != GauntletStage::Decision)
            .map(|l| l.stage.as_str())
            .collect();
        assert_eq!(stages, vec!["baseline"]);
        assert_eq!(decision(&report, "unstable_profiling"), "reject_unstable");
    }

    #[test]
    fn multi_lever_and_checksum_are_refused() {
        let report = run_optimization_gauntlet("og/test");
        assert_eq!(
            decision(&report, "multi_lever_reject"),
            "reject_multi_lever"
        );
        assert_eq!(decision(&report, "checksum_mismatch"), "reject_isomorphism");
    }

    #[test]
    fn regression_rolls_back_and_restores_incumbent() {
        let report = run_optimization_gauntlet("og/test");
        assert_eq!(decision(&report, "regression_rollback"), "rollback");
        let rb = report
            .ledger
            .iter()
            .find(|l| l.scenario_id == "regression_rollback" && l.stage == GauntletStage::Rollback)
            .unwrap();
        assert_eq!(rb.stage_outcome, "rollback_triggered");
        assert!(rb.baseline_restored);
    }

    #[test]
    fn drift_reorders_next_candidates_and_promotes() {
        let report = run_optimization_gauntlet("og/test");
        assert_eq!(decision(&report, "hotspot_drift"), "promote");
        let rp = report
            .ledger
            .iter()
            .find(|l| l.scenario_id == "hotspot_drift" && l.stage == GauntletStage::Reprofile)
            .unwrap();
        assert!(rp.bottleneck_shifted);
        assert!(rp.backlog_reordered);
    }

    #[test]
    fn stage_lines_carry_upstream_linkage() {
        let report = run_optimization_gauntlet("og/test");
        // The positive path's stage chain links baseline -> ... -> rollback.
        let iso = report
            .ledger
            .iter()
            .find(|l| l.scenario_id == "positive_promote" && l.stage == GauntletStage::Isomorphism)
            .unwrap();
        assert_eq!(iso.upstream_stage, "one_lever");
        assert_ne!(iso.proof_id, "n/a");
    }

    #[test]
    fn report_is_deterministic_and_replay_identical() {
        let a = run_optimization_gauntlet("og/test");
        let b = run_optimization_gauntlet("og/test");
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.ledger, b.ledger);
        assert_eq!(a.render_ledger_jsonl(), b.render_ledger_jsonl());
    }

    #[test]
    fn pipeline_materializes_consistent_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let outcome =
            run_optimization_gauntlet_pipeline(dir.path(), &GauntletPipelineConfig::default())
                .unwrap();
        assert!(outcome.summary.gate_passes);
        assert_eq!(outcome.artifacts.len(), 3);
        for artifact in &outcome.artifacts {
            let path = std::path::Path::new(&outcome.run_dir).join(&artifact.file);
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(sha256_hex(&bytes), artifact.sha256);
        }
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = run_optimization_gauntlet("og/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}") {
            let first = run_optimization_gauntlet(&label);
            let second = run_optimization_gauntlet(&label);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.ledger, &second.ledger);
        }

        #[test]
        fn prop_gate_always_passes(label in "[a-z]{1,8}") {
            let report = run_optimization_gauntlet(&label);
            prop_assert!(report.gate_passes);
            prop_assert!(report.summary.all_decisions_safe);
        }
    }
}
