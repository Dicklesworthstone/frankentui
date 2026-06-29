//! Cross-module unit/property test-evidence harness for the optimization-control
//! plane (bd-3bxhj.8.20).
//!
//! The optimization pipeline is a chain of independent kernels — deterministic
//! baseline capture, profiler hotspot extraction, the opportunity-matrix scorer,
//! the golden-output isomorphism oracle, the one-lever / rollback policy, and the
//! iterative re-profile loop. Each carries its own inline tests; this module is
//! the *cross-component* evidence harness that exercises all of them through a
//! single deterministic [`OptimizationControlDiagnostic`] envelope, projects each
//! kernel's public output into that envelope, checks it against an expected-
//! outcome oracle, and emits an auditable validation report.
//!
//! Coverage (AC1) spans happy paths, malformed artifacts (insufficient baseline
//! measurements, missing evidence pointers), and adversarial metric outliers
//! (NaN score terms, a non-finite re-profile). Determinism (AC2) is asserted by
//! property tests that re-run the report and compare byte-for-byte. Every failure
//! log carries `run_id`, `hotspot_id`, `score_terms`, `proof_id`, `lever_count`,
//! and a replay command (AC3).
//!
//! Like the [`crate::optimization_model_tests`] and
//! [`crate::portfolio_governance_tests`] precedents, this is a `pub mod` compiled
//! into the lib; all `proptest` usage is confined to the `#[cfg(test)]` block so
//! the dev-only dependency never leaks into the library build. The diagnostic is
//! **float-free** (every numeric term is a fixed-decimal string via [`fmt6`]), so
//! it derives [`Eq`] and the report replays byte-identically.

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
use crate::opportunity_scorer::OpportunityConfig;
use crate::opportunity_scorer::{OptimizationLever, run_opportunity_scoring};
use crate::profile_orchestrator::{
    ProfileModality, ProfileOrchestrationConfig, ProfileOrchestrationPlan, ProfileOrchestrator,
    ProfileSample, ProfilerInvocation,
};
use crate::recommendation_contract::EffortSize;
use crate::reprofile_loop::ReprofileConfig;
use crate::reprofile_loop::{
    BacklogCandidate, HotspotCost, OptimizationRound, ProfileSnapshot, run_reprofile_loop,
};

/// Schema version for the optimization-control test artifacts.
pub const OPTIMIZATION_CONTROL_SCHEMA_VERSION: &str = "optimization-control-tests-v1";

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

/// Deterministic fixed-decimal rendering so the diagnostic stays float-free and
/// derives `Eq`.
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

// ── Kernels + diagnostic envelope ────────────────────────────────────────────

/// The optimization-control kernel a diagnostic came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKernel {
    /// Deterministic baseline capture (percentiles / aggregation / fingerprint).
    Baseline,
    /// Profiler orchestration + hotspot extraction.
    Profiler,
    /// Opportunity-matrix scorer.
    Scoring,
    /// Golden-output isomorphism oracle.
    Isomorphism,
    /// One-lever / rollback policy.
    OneLever,
    /// Iterative re-profile loop.
    Reprofile,
}

impl ControlKernel {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Profiler => "profiler",
            Self::Scoring => "scoring",
            Self::Isomorphism => "isomorphism",
            Self::OneLever => "one_lever",
            Self::Reprofile => "reprofile",
        }
    }
}

/// A unified diagnostic projected from any kernel's output (float-free; `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptimizationControlDiagnostic {
    /// The source kernel.
    pub kernel: ControlKernel,
    /// Deterministic run / artifact id (AC3).
    pub run_id: String,
    /// The subject under test (candidate / fixture / change id).
    pub subject_id: String,
    /// The hotspot the subject addresses, or `"n/a"` (AC3).
    pub hotspot_id: String,
    /// Machine-readable score terms (fixed-decimal), kernel-specific (AC3).
    pub score_terms: Vec<String>,
    /// The isomorphism proof id, or `"n/a"` (AC3).
    pub proof_id: String,
    /// The lever count (one-lever kernel; `0` elsewhere, AC3).
    pub lever_count: usize,
    /// The kernel's outcome tag.
    pub outcome: String,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic replay command (AC3).
    pub replay_cmd: String,
}

impl OptimizationControlDiagnostic {
    /// Whether every mandated field is present (AC3).
    #[must_use]
    pub fn has_required_fields(&self) -> bool {
        !self.run_id.is_empty()
            && !self.subject_id.is_empty()
            && !self.hotspot_id.is_empty()
            && !self.score_terms.is_empty()
            && !self.proof_id.is_empty()
            && !self.outcome.is_empty()
            && !self.detail.is_empty()
            && !self.replay_cmd.is_empty()
    }

    /// Project the AC3 failure-log schema.
    #[must_use]
    pub fn failure_log(&self) -> OptimizationControlFailureLog {
        OptimizationControlFailureLog {
            run_id: self.run_id.clone(),
            hotspot_id: self.hotspot_id.clone(),
            score_terms: self.score_terms.clone(),
            proof_id: self.proof_id.clone(),
            lever_count: self.lever_count,
            replay_cmd: self.replay_cmd.clone(),
        }
    }
}

/// The AC3 failure-log projection: the mandated fields for a failing case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptimizationControlFailureLog {
    /// Run / artifact id.
    pub run_id: String,
    /// Hotspot id.
    pub hotspot_id: String,
    /// Score terms (fixed-decimal).
    pub score_terms: Vec<String>,
    /// Isomorphism proof id.
    pub proof_id: String,
    /// Lever count.
    pub lever_count: usize,
    /// Replay command.
    pub replay_cmd: String,
}

/// A fixture's pass/fail verdict against its oracle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeVerdict {
    /// The fixture label.
    pub fixture_label: String,
    /// The kernel.
    pub kernel: ControlKernel,
    /// The expectation, as a stable string.
    pub expectation: String,
    /// Whether the observed outcome matched the expectation.
    pub matches_expected: bool,
    /// Any mismatch detail.
    pub mismatch: String,
}

/// One fixture's evaluation: its diagnostics + verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelEvaluation {
    /// The fixture label.
    pub label: String,
    /// The kernel.
    pub kernel: ControlKernel,
    /// The projected diagnostics.
    pub diagnostics: Vec<OptimizationControlDiagnostic>,
    /// The verdict.
    pub verdict: OutcomeVerdict,
}

fn verdict(
    label: &str,
    kernel: ControlKernel,
    expectation: &str,
    matches_expected: bool,
    mismatch: impl Into<String>,
) -> OutcomeVerdict {
    OutcomeVerdict {
        fixture_label: label.to_string(),
        kernel,
        expectation: expectation.to_string(),
        matches_expected,
        mismatch: mismatch.into(),
    }
}

fn replay(name: &str) -> String {
    format!("cargo test -p doctor_frankentui --lib optimization_control_tests # {name}")
}

// ── Baseline kernel fixtures ─────────────────────────────────────────────────

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

/// Green: a clean baseline run with the required number of measurement samples.
fn eval_baseline_clean() -> KernelEvaluation {
    let measurements = vec![
        BaselineRawMeasurement::warmup(0, 5.0),
        BaselineRawMeasurement::measurement(1, 1.0, 2.0, 3.0, 1000.0, 1_000_000),
        BaselineRawMeasurement::measurement(2, 1.0, 2.0, 3.0, 1010.0, 1_000_000),
        BaselineRawMeasurement::measurement(3, 1.0, 2.0, 3.0, 990.0, 1_000_000),
        BaselineRawMeasurement::measurement(4, 1.0, 2.0, 3.0, 1005.0, 1_000_000),
        BaselineRawMeasurement::measurement(5, 1.0, 2.0, 3.0, 995.0, 1_000_000),
    ];
    let input = BaselineRunInput::new("run-clean", baseline_scenario(), measurements);
    let report = BaselineCaptureRunner::new(baseline_manifest()).capture(vec![input]);

    let diagnostics: Vec<OptimizationControlDiagnostic> = report
        .records
        .iter()
        .map(|r| OptimizationControlDiagnostic {
            kernel: ControlKernel::Baseline,
            run_id: r.run_id.clone(),
            subject_id: r.scenario_id.clone(),
            hotspot_id: "n/a".to_string(),
            score_terms: vec![
                fmt6(r.summary.latency_p50_ms),
                fmt6(r.summary.latency_p95_ms),
                fmt6(r.summary.latency_p99_ms),
                fmt6(r.summary.throughput_ops_per_sec),
            ],
            proof_id: "n/a".to_string(),
            lever_count: 0,
            outcome: if r.variance_violations.is_empty() {
                "clean".to_string()
            } else {
                "variance_violation".to_string()
            },
            detail: format!(
                "p50/p95/p99 = {:.3}/{:.3}/{:.3} ms over {} runs; fingerprint {}",
                r.summary.latency_p50_ms,
                r.summary.latency_p95_ms,
                r.summary.latency_p99_ms,
                r.summary.measurement_runs,
                r.command.fingerprint_sha256,
            ),
            replay_cmd: replay("baseline_clean"),
        })
        .collect();

    let clean = report.variance_violations.is_empty()
        && diagnostics.len() == 1
        // median p99 across the 5 identical-p99 samples is exactly 3.0.
        && diagnostics[0].score_terms.get(2).map(String::as_str) == Some(&fmt6(3.0));
    let v = verdict(
        "baseline_clean",
        ControlKernel::Baseline,
        "clean capture, p99=3.0, no variance violations",
        clean,
        if clean {
            ""
        } else {
            "unexpected violations or aggregation"
        },
    );
    KernelEvaluation {
        label: "baseline_clean".to_string(),
        kernel: ControlKernel::Baseline,
        diagnostics,
        verdict: v,
    }
}

/// Malformed: too few measurement samples -> an insufficient-measurements
/// variance violation is recorded (AC1 malformed artifact).
fn eval_baseline_insufficient() -> KernelEvaluation {
    let measurements = vec![
        BaselineRawMeasurement::warmup(0, 5.0),
        BaselineRawMeasurement::measurement(1, 1.0, 2.0, 3.0, 1000.0, 1_000_000),
    ];
    let input = BaselineRunInput::new("run-thin", baseline_scenario(), measurements);
    let report = BaselineCaptureRunner::new(baseline_manifest()).capture(vec![input]);

    let diagnostics = vec![OptimizationControlDiagnostic {
        kernel: ControlKernel::Baseline,
        run_id: "run-thin".to_string(),
        subject_id: "scn-1".to_string(),
        hotspot_id: "n/a".to_string(),
        score_terms: vec![fmt6(report.variance_violations.len() as f64)],
        proof_id: "n/a".to_string(),
        lever_count: 0,
        outcome: if report.variance_violations.is_empty() {
            "clean".to_string()
        } else {
            "variance_violation".to_string()
        },
        detail: format!(
            "{} variance violation(s) on a thin run",
            report.variance_violations.len()
        ),
        replay_cmd: replay("baseline_insufficient"),
    }];

    let flagged = !report.variance_violations.is_empty();
    let v = verdict(
        "baseline_insufficient",
        ControlKernel::Baseline,
        "thin run flagged with >=1 variance violation",
        flagged,
        if flagged {
            ""
        } else {
            "insufficient measurements not flagged"
        },
    );
    KernelEvaluation {
        label: "baseline_insufficient".to_string(),
        kernel: ControlKernel::Baseline,
        diagnostics,
        verdict: v,
    }
}

// ── Profiler kernel fixtures ─────────────────────────────────────────────────

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

/// Green: profiler hotspot extraction is deterministic and ranks a top hotspot.
fn eval_profiler_deterministic() -> KernelEvaluation {
    let cfg = ProfileOrchestrationConfig::default().with_top_n(5);
    let a = ProfileOrchestrator::new(cfg).orchestrate(profiler_plan(), profiler_samples());
    let b = ProfileOrchestrator::new(cfg).orchestrate(profiler_plan(), profiler_samples());

    let diagnostics: Vec<OptimizationControlDiagnostic> = a
        .top_hotspots
        .iter()
        .map(|h| OptimizationControlDiagnostic {
            kernel: ControlKernel::Profiler,
            run_id: a.orchestration_id.clone(),
            subject_id: h.symbol.clone(),
            hotspot_id: h.hotspot_id.clone(),
            score_terms: vec![
                fmt6(h.opportunity_score),
                fmt6(h.normalized_attribution_percent),
            ],
            proof_id: "n/a".to_string(),
            lever_count: 0,
            outcome: format!("rank_{}", h.rank),
            detail: format!(
                "{} @ {} attribution {:.2}% opportunity {:.4}",
                h.symbol, h.source_location, h.normalized_attribution_percent, h.opportunity_score
            ),
            replay_cmd: replay("profiler_deterministic"),
        })
        .collect();

    // Determinism: identical inputs -> identical orchestration id + hotspot ids.
    let deterministic = a.orchestration_id == b.orchestration_id
        && a.top_hotspots
            .iter()
            .map(|h| &h.hotspot_id)
            .collect::<Vec<_>>()
            == b.top_hotspots
                .iter()
                .map(|h| &h.hotspot_id)
                .collect::<Vec<_>>()
        && !a.top_hotspots.is_empty();
    let v = verdict(
        "profiler_deterministic",
        ControlKernel::Profiler,
        "identical inputs yield identical ranked hotspots",
        deterministic,
        if deterministic {
            ""
        } else {
            "non-deterministic hotspot extraction"
        },
    );
    KernelEvaluation {
        label: "profiler_deterministic".to_string(),
        kernel: ControlKernel::Profiler,
        diagnostics,
        verdict: v,
    }
}

/// Missing evidence: an empty sample set yields no hotspots without panicking.
fn eval_profiler_empty() -> KernelEvaluation {
    let report = ProfileOrchestrator::new(ProfileOrchestrationConfig::default())
        .orchestrate(profiler_plan(), vec![]);
    let diagnostics = vec![OptimizationControlDiagnostic {
        kernel: ControlKernel::Profiler,
        run_id: report.orchestration_id.clone(),
        subject_id: "empty".to_string(),
        hotspot_id: "n/a".to_string(),
        score_terms: vec![fmt6(report.hotspots.len() as f64)],
        proof_id: "n/a".to_string(),
        lever_count: 0,
        outcome: "no_hotspots".to_string(),
        detail: format!(
            "{} hotspots from an empty sample set",
            report.hotspots.len()
        ),
        replay_cmd: replay("profiler_empty"),
    }];
    let ok = report.hotspots.is_empty();
    let v = verdict(
        "profiler_empty",
        ControlKernel::Profiler,
        "empty samples -> zero hotspots, no panic",
        ok,
        if ok {
            ""
        } else {
            "empty input produced hotspots"
        },
    );
    KernelEvaluation {
        label: "profiler_empty".to_string(),
        kernel: ControlKernel::Profiler,
        diagnostics,
        verdict: v,
    }
}

// ── Scoring kernel fixtures ──────────────────────────────────────────────────

/// Green: a strong lever clears the Score>=2.0 gate; an adversarial NaN-impact
/// lever is clamped to a zero score and blocked.
fn eval_scoring() -> KernelEvaluation {
    let levers = vec![
        OptimizationLever::new(
            "l.strong",
            "hot.render",
            "simd diff",
            9.0,
            0.9,
            EffortSize::Small,
        )
        .with_evidence(["baseline.render".to_string()]),
        // Adversarial outlier: NaN impact is clamped to 0 -> score 0 -> blocked.
        OptimizationLever::new(
            "l.nan",
            "hot.x",
            "garbage",
            f64::NAN,
            0.9,
            EffortSize::Small,
        )
        .with_evidence(["baseline.x".to_string()]),
    ];
    let report = run_opportunity_scoring("oc/scoring", &levers, &[], &OpportunityConfig::default());

    let diagnostics: Vec<OptimizationControlDiagnostic> = report
        .cards
        .iter()
        .map(|c| OptimizationControlDiagnostic {
            kernel: ControlKernel::Scoring,
            run_id: c.run_id.clone(),
            subject_id: c.lever_id.clone(),
            hotspot_id: c.hotspot_id.clone(),
            score_terms: vec![
                c.impact.clone(),
                c.confidence.clone(),
                c.effort_cost.clone(),
                c.score.clone(),
            ],
            proof_id: "n/a".to_string(),
            lever_count: 0,
            outcome: c.status.as_str().to_string(),
            detail: c.rationale.clone(),
            replay_cmd: replay("scoring"),
        })
        .collect();

    let strong = report.card("l.strong").unwrap();
    let nan = report.card("l.nan").unwrap();
    let ok = strong.activated
        && strong.score.parse::<f64>().unwrap() >= 2.0
        && !nan.activated
        && nan.score == fmt6(0.0);
    let v = verdict(
        "scoring",
        ControlKernel::Scoring,
        "strong lever activates (score>=2.0); NaN-impact lever clamps to 0 and blocks",
        ok,
        if ok {
            ""
        } else {
            "score math / outlier handling wrong"
        },
    );
    KernelEvaluation {
        label: "scoring".to_string(),
        kernel: ControlKernel::Scoring,
        diagnostics,
        verdict: v,
    }
}

/// Missing evidence: a lever with no evidence pointers fails the AC1 contract.
fn eval_scoring_missing_evidence() -> KernelEvaluation {
    let levers = vec![OptimizationLever::new(
        "l.bare",
        "hot.bare",
        "no evidence",
        9.0,
        0.9,
        EffortSize::Small,
    )];
    let report = run_opportunity_scoring("oc/bare", &levers, &[], &OpportunityConfig::default());
    let card = report.card("l.bare").unwrap();
    let diagnostics = vec![OptimizationControlDiagnostic {
        kernel: ControlKernel::Scoring,
        run_id: card.run_id.clone(),
        subject_id: card.lever_id.clone(),
        hotspot_id: card.hotspot_id.clone(),
        score_terms: vec![
            card.impact.clone(),
            card.confidence.clone(),
            card.score.clone(),
        ],
        proof_id: "n/a".to_string(),
        lever_count: 0,
        outcome: "missing_evidence".to_string(),
        detail: format!("evidence_refs={}", card.evidence_refs.len()),
        replay_cmd: replay("scoring_missing_evidence"),
    }];
    // The score-terms gate flags the missing evidence (required_fields fails).
    let flagged = !report.summary.score_terms_present;
    let v = verdict(
        "scoring_missing_evidence",
        ControlKernel::Scoring,
        "a lever without evidence fails the score-terms-present gate",
        flagged,
        if flagged {
            ""
        } else {
            "missing evidence not flagged"
        },
    );
    KernelEvaluation {
        label: "scoring_missing_evidence".to_string(),
        kernel: ControlKernel::Scoring,
        diagnostics,
        verdict: v,
    }
}

// ── Isomorphism kernel fixtures ──────────────────────────────────────────────

fn golden_output() -> Vec<GoldenOutput> {
    vec![GoldenOutput::new(
        "fix-a",
        "wl-1",
        vec![GoldenRecord::new("row1", 0).with_floats(vec![1.230001])],
    )]
}

/// Green: a candidate differing only within a declared float tolerance is a
/// normalized match and is promotion-allowed.
fn eval_isomorphism_match() -> KernelEvaluation {
    let candidate = vec![GoldenOutput::new(
        "fix-a",
        "wl-1",
        vec![GoldenRecord::new("row1", 0).with_floats(vec![1.230004])],
    )];
    let change = GoldenChange::new("chg-ok", "base-0", "vectorize").with_invariant(
        IsomorphismInvariant::FloatingPoint {
            tolerance_decimals: 3,
        },
    );
    let report = GoldenIsomorphismGate::new(GoldenIsomorphismConfig::default()).verify(
        &change,
        &golden_output(),
        &candidate,
    );

    let diagnostics = vec![OptimizationControlDiagnostic {
        kernel: ControlKernel::Isomorphism,
        run_id: report.oracle_id.clone(),
        subject_id: report.change_id.clone(),
        hotspot_id: "n/a".to_string(),
        score_terms: vec![
            fmt6(report.summary.matched as f64),
            fmt6(report.summary.mismatched as f64),
        ],
        proof_id: report.proof.proof_id.clone(),
        lever_count: 0,
        outcome: if report.promotion_allowed {
            "preserved".to_string()
        } else {
            "blocked".to_string()
        },
        detail: format!(
            "behavior_preserved={} promotion_allowed={}",
            report.proof.behavior_preserved, report.promotion_allowed
        ),
        replay_cmd: replay("isomorphism_match"),
    }];
    let ok = report.promotion_allowed && report.proof.behavior_preserved;
    let v = verdict(
        "isomorphism_match",
        ControlKernel::Isomorphism,
        "within-tolerance candidate is a normalized match, promotion allowed",
        ok,
        if ok {
            ""
        } else {
            "tolerant match not promoted"
        },
    );
    KernelEvaluation {
        label: "isomorphism_match".to_string(),
        kernel: ControlKernel::Isomorphism,
        diagnostics,
        verdict: v,
    }
}

/// Red: a token change with no covering invariant is a mismatch and is blocked
/// with a counterexample.
fn eval_isomorphism_mismatch() -> KernelEvaluation {
    let candidate = vec![GoldenOutput::new(
        "fix-a",
        "wl-1",
        vec![GoldenRecord::new("row1", 0).with_tokens(vec!["CHANGED".to_string()])],
    )];
    let change = GoldenChange::new("chg-bad", "base-0", "reorder");
    let report = GoldenIsomorphismGate::new(GoldenIsomorphismConfig::default()).verify(
        &change,
        &golden_output(),
        &candidate,
    );

    let diagnostics = vec![OptimizationControlDiagnostic {
        kernel: ControlKernel::Isomorphism,
        run_id: report.oracle_id.clone(),
        subject_id: report.change_id.clone(),
        hotspot_id: "n/a".to_string(),
        score_terms: vec![
            fmt6(report.summary.mismatched as f64),
            fmt6(report.summary.blocking_findings as f64),
        ],
        proof_id: report.proof.proof_id.clone(),
        lever_count: 0,
        outcome: if report.promotion_allowed {
            "preserved".to_string()
        } else {
            "blocked".to_string()
        },
        detail: format!(
            "mismatched={} blocking={}",
            report.summary.mismatched, report.summary.blocking_findings
        ),
        replay_cmd: replay("isomorphism_mismatch"),
    }];
    let ok = !report.promotion_allowed && report.proof.counterexample.is_some();
    let v = verdict(
        "isomorphism_mismatch",
        ControlKernel::Isomorphism,
        "an uncovered behavior change is blocked with a counterexample",
        ok,
        if ok {
            ""
        } else {
            "uncovered change was not blocked"
        },
    );
    KernelEvaluation {
        label: "isomorphism_mismatch".to_string(),
        kernel: ControlKernel::Isomorphism,
        diagnostics,
        verdict: v,
    }
}

// ── One-lever kernel fixtures ────────────────────────────────────────────────

fn ready_rollback() -> RollbackPlan {
    RollbackPlan::new(
        "rbk-1",
        "git revert <sha>",
        RiskTier::Low,
        ["cargo test -p ftui-render --lib".to_string()],
    )
}

/// Green: a single-lever change with a complete rollback is accepted. Red: a
/// bare multi-lever change is rejected.
fn eval_one_lever() -> KernelEvaluation {
    let changes = vec![
        OptimizationChange::new("chg.single", ["lever.a".to_string()])
            .with_rollback(ready_rollback()),
        OptimizationChange::new("chg.multi", ["lever.a".to_string(), "lever.b".to_string()])
            .with_rollback(ready_rollback()),
    ];
    let report = run_one_lever_policy("oc/one_lever", &changes);

    let diagnostics: Vec<OptimizationControlDiagnostic> = report
        .cards
        .iter()
        .map(|c| OptimizationControlDiagnostic {
            kernel: ControlKernel::OneLever,
            run_id: c.run_id.clone(),
            subject_id: c.change_id.clone(),
            hotspot_id: "n/a".to_string(),
            score_terms: vec![fmt6(c.lever_count as f64)],
            proof_id: "n/a".to_string(),
            lever_count: c.lever_count,
            outcome: if c.accepted {
                "accepted".to_string()
            } else {
                "rejected".to_string()
            },
            detail: c.detail.clone(),
            replay_cmd: replay("one_lever"),
        })
        .collect();

    let single = report.card("chg.single").unwrap();
    let multi = report.card("chg.multi").unwrap();
    let ok =
        single.accepted && single.lever_count == 1 && !multi.accepted && multi.lever_count == 2;
    let v = verdict(
        "one_lever",
        ControlKernel::OneLever,
        "single+rollback accepted; bare multi-lever rejected",
        ok,
        if ok {
            ""
        } else {
            "one-lever policy not enforced"
        },
    );
    KernelEvaluation {
        label: "one_lever".to_string(),
        kernel: ControlKernel::OneLever,
        diagnostics,
        verdict: v,
    }
}

// ── Reprofile kernel fixtures ────────────────────────────────────────────────

fn snapshot(p99: f64, hot: &str, share: f64, stable: bool) -> ProfileSnapshot {
    ProfileSnapshot {
        p50_us: 32.0,
        p95_us: 70.0,
        p99_us: p99,
        throughput: 11000.0,
        memory_bytes: 4_900_000.0,
        hotspots: vec![HotspotCost {
            hotspot_id: hot.to_string(),
            cost_share: share,
        }],
        stable,
    }
}

fn reprofile_backlog() -> Vec<BacklogCandidate> {
    vec![BacklogCandidate {
        candidate_id: "cand.layout".to_string(),
        hotspot_id: "hot.layout".to_string(),
        confidence: 0.85,
        effort: EffortSize::Medium,
    }]
}

/// Green: an improving round continues and re-ranks the backlog. Adversarial: a
/// non-finite re-profile pauses with a triage hint.
fn eval_reprofile() -> KernelEvaluation {
    let rounds = vec![
        OptimizationRound {
            round_number: 1,
            change_id: "chg.win".to_string(),
            lever_id: "lever.win".to_string(),
            before: snapshot(120.0, "hot.render", 0.55, true),
            after: snapshot(85.0, "hot.layout", 0.45, true),
            backlog: reprofile_backlog(),
        },
        OptimizationRound {
            round_number: 2,
            change_id: "chg.nan".to_string(),
            lever_id: "lever.nan".to_string(),
            before: snapshot(85.0, "hot.layout", 0.45, true),
            after: snapshot(f64::NAN, "hot.layout", 0.45, true),
            backlog: reprofile_backlog(),
        },
    ];
    let report = run_reprofile_loop("oc/reprofile", &rounds, &ReprofileConfig::default());

    let diagnostics: Vec<OptimizationControlDiagnostic> = report
        .ledger
        .iter()
        .map(|e| OptimizationControlDiagnostic {
            kernel: ControlKernel::Reprofile,
            run_id: e.run_id.clone(),
            subject_id: e.change_id.clone(),
            hotspot_id: e.bottleneck_after.clone(),
            score_terms: vec![
                e.p99_before.clone(),
                e.p99_after.clone(),
                e.p99_delta.clone(),
            ],
            proof_id: "n/a".to_string(),
            lever_count: 0,
            outcome: e.verdict.as_str().to_string(),
            detail: e.detail.clone(),
            replay_cmd: replay("reprofile"),
        })
        .collect();

    let r1 = report.round(1).unwrap();
    let r2 = report.round(2).unwrap();
    let ok = matches!(r1.verdict, crate::reprofile_loop::RoundVerdict::Continue)
        && r1.reranked_backlog.len() == 1
        && matches!(r2.verdict, crate::reprofile_loop::RoundVerdict::Pause)
        && r2.triage_hint.contains("non-finite");
    let v = verdict(
        "reprofile",
        ControlKernel::Reprofile,
        "improving round continues + re-ranks; non-finite re-profile pauses",
        ok,
        if ok {
            ""
        } else {
            "re-profile loop / backlog recalc wrong"
        },
    );
    KernelEvaluation {
        label: "reprofile".to_string(),
        kernel: ControlKernel::Reprofile,
        diagnostics,
        verdict: v,
    }
}

/// The full fixture corpus across every kernel.
#[must_use]
pub fn optimization_control_corpus() -> Vec<KernelEvaluation> {
    let mut all = vec![
        eval_baseline_clean(),
        eval_baseline_insufficient(),
        eval_profiler_deterministic(),
        eval_profiler_empty(),
        eval_scoring(),
        eval_scoring_missing_evidence(),
        eval_isomorphism_match(),
        eval_isomorphism_mismatch(),
        eval_one_lever(),
        eval_reprofile(),
    ];
    all.sort_by(|a, b| a.label.cmp(&b.label));
    all
}

// ── Validation report ────────────────────────────────────────────────────────

/// Roll-up of the optimization-control validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptimizationControlSummary {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum over the diagnostics + verdicts.
    pub evidence_checksum: String,
    /// Total fixtures evaluated.
    pub total_fixtures: usize,
    /// Total diagnostics projected.
    pub total_diagnostics: usize,
    /// Distinct kernels covered.
    pub kernels_covered: usize,
    /// Fixtures whose outcome matched their oracle.
    pub matched_fixtures: usize,
    /// Whether every diagnostic carries all mandated fields (AC3).
    pub required_fields_complete: bool,
    /// Whether every fixture matched its expected oracle (AC1).
    pub all_expectations_met: bool,
    /// Whether all six kernels are represented.
    pub all_kernels_covered: bool,
    /// Whether the fail-closed gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptimizationControlStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// A full optimization-control validation report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptimizationControlValidationReport {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Run label.
    pub label: String,
    /// All projected diagnostics (sorted).
    pub diagnostics: Vec<OptimizationControlDiagnostic>,
    /// All fixture verdicts (sorted).
    pub verdicts: Vec<OutcomeVerdict>,
    /// The roll-up summary + gate.
    pub summary: OptimizationControlSummary,
    /// The deterministic JSON-stats artifact.
    pub exported_json_stats: OptimizationControlStatsArtifact,
    /// Evidence checksum.
    pub evidence_checksum: String,
}

impl OptimizationControlValidationReport {
    /// Failure logs for any failing diagnostic (AC3).
    #[must_use]
    pub fn failure_logs(&self) -> Vec<OptimizationControlFailureLog> {
        self.diagnostics
            .iter()
            .filter(|d| !d.has_required_fields())
            .map(OptimizationControlDiagnostic::failure_log)
            .collect()
    }

    /// Verdicts that did not match their oracle.
    #[must_use]
    pub fn failing_verdicts(&self) -> Vec<&OutcomeVerdict> {
        self.verdicts
            .iter()
            .filter(|v| !v.matches_expected)
            .collect()
    }

    /// Whether the gate passes.
    #[must_use]
    pub fn gate_passes(&self) -> bool {
        self.summary.gate_passes
    }
}

#[derive(Serialize)]
struct EvidenceInput<'a> {
    diagnostics: &'a [OptimizationControlDiagnostic],
    verdicts: &'a [OutcomeVerdict],
}

#[derive(Serialize)]
struct ReportIdInput<'a> {
    schema_version: &'a str,
    label: &'a str,
    evidence_checksum: &'a str,
}

/// Run the full optimization-control validation.
#[must_use]
pub fn run_optimization_control_validation(label: &str) -> OptimizationControlValidationReport {
    let corpus = optimization_control_corpus();

    let mut diagnostics: Vec<OptimizationControlDiagnostic> =
        corpus.iter().flat_map(|e| e.diagnostics.clone()).collect();
    diagnostics.sort_by(|a, b| {
        a.kernel
            .cmp(&b.kernel)
            .then_with(|| a.subject_id.cmp(&b.subject_id))
            .then_with(|| a.run_id.cmp(&b.run_id))
            .then_with(|| a.outcome.cmp(&b.outcome))
    });
    let mut verdicts: Vec<OutcomeVerdict> = corpus.iter().map(|e| e.verdict.clone()).collect();
    verdicts.sort_by(|a, b| {
        a.kernel
            .cmp(&b.kernel)
            .then_with(|| a.fixture_label.cmp(&b.fixture_label))
    });

    let evidence_checksum = stable_hash(&EvidenceInput {
        diagnostics: &diagnostics,
        verdicts: &verdicts,
    });
    let report_id = format!(
        "optimization-control-tests-{}",
        short_hash(&stable_hash(&ReportIdInput {
            schema_version: OPTIMIZATION_CONTROL_SCHEMA_VERSION,
            label,
            evidence_checksum: &evidence_checksum,
        }))
    );

    let kernels_covered = {
        let mut k: Vec<ControlKernel> = diagnostics.iter().map(|d| d.kernel).collect();
        k.sort();
        k.dedup();
        k.len()
    };
    let matched_fixtures = verdicts.iter().filter(|v| v.matches_expected).count();
    let required_fields_complete = diagnostics
        .iter()
        .all(OptimizationControlDiagnostic::has_required_fields);
    let all_expectations_met = verdicts.iter().all(|v| v.matches_expected);
    let all_kernels_covered = kernels_covered == 6;
    let gate_passes = required_fields_complete && all_expectations_met && all_kernels_covered;

    let summary = OptimizationControlSummary {
        schema_version: OPTIMIZATION_CONTROL_SCHEMA_VERSION.to_string(),
        report_id: report_id.clone(),
        label: label.to_string(),
        evidence_checksum: evidence_checksum.clone(),
        total_fixtures: verdicts.len(),
        total_diagnostics: diagnostics.len(),
        kernels_covered,
        matched_fixtures,
        required_fields_complete,
        all_expectations_met,
        all_kernels_covered,
        gate_passes,
        replay_command: format!(
            "cargo test -p doctor_frankentui --lib optimization_control_tests # report {report_id}"
        ),
    };

    let exported_json_stats = {
        #[derive(Serialize)]
        struct Export<'a> {
            schema_version: &'a str,
            report_id: &'a str,
            summary: &'a OptimizationControlSummary,
        }
        let content = serde_json::to_string_pretty(&Export {
            schema_version: OPTIMIZATION_CONTROL_SCHEMA_VERSION,
            report_id: &report_id,
            summary: &summary,
        })
        .unwrap_or_default();
        let sha256 = sha256_hex(content.as_bytes());
        OptimizationControlStatsArtifact {
            path: format!("optimization_control_tests/{report_id}.json"),
            sha256,
            content,
        }
    };

    OptimizationControlValidationReport {
        schema_version: OPTIMIZATION_CONTROL_SCHEMA_VERSION.to_string(),
        report_id,
        label: label.to_string(),
        diagnostics,
        verdicts,
        summary,
        exported_json_stats,
        evidence_checksum,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn baseline_clean_aggregates_and_fingerprints() {
        let e = eval_baseline_clean();
        assert!(e.verdict.matches_expected, "{:?}", e.verdict);
        // Identical commands fingerprint identically (fingerprint stability).
        assert_eq!(
            baseline_scenario().command.fingerprint_sha256,
            baseline_scenario().command.fingerprint_sha256
        );
    }

    #[test]
    fn baseline_insufficient_is_flagged() {
        let e = eval_baseline_insufficient();
        assert!(e.verdict.matches_expected, "{:?}", e.verdict);
    }

    #[test]
    fn profiler_is_deterministic_and_empty_safe() {
        assert!(eval_profiler_deterministic().verdict.matches_expected);
        assert!(eval_profiler_empty().verdict.matches_expected);
    }

    #[test]
    fn scoring_math_and_outliers() {
        assert!(eval_scoring().verdict.matches_expected);
        assert!(eval_scoring_missing_evidence().verdict.matches_expected);
    }

    #[test]
    fn isomorphism_match_and_mismatch() {
        assert!(eval_isomorphism_match().verdict.matches_expected);
        assert!(eval_isomorphism_mismatch().verdict.matches_expected);
    }

    #[test]
    fn one_lever_policy_enforced() {
        assert!(eval_one_lever().verdict.matches_expected);
    }

    #[test]
    fn reprofile_continue_and_pause() {
        assert!(eval_reprofile().verdict.matches_expected);
    }

    #[test]
    fn full_validation_passes_gate_and_covers_all_kernels() {
        let report = run_optimization_control_validation("oc/test");
        assert!(report.gate_passes(), "summary: {:?}", report.summary);
        assert_eq!(report.summary.kernels_covered, 6);
        assert!(report.summary.all_kernels_covered);
        assert!(report.summary.all_expectations_met);
        assert!(report.summary.required_fields_complete);
        assert!(report.failing_verdicts().is_empty());
        assert!(report.failure_logs().is_empty());
        for d in &report.diagnostics {
            assert!(d.has_required_fields(), "missing fields: {d:?}");
        }
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = run_optimization_control_validation("oc/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}") {
            let first = run_optimization_control_validation(&label);
            let second = run_optimization_control_validation(&label);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.diagnostics, &second.diagnostics);
            prop_assert_eq!(&first.verdicts, &second.verdicts);
            prop_assert_eq!(&first.exported_json_stats.sha256, &second.exported_json_stats.sha256);
        }

        #[test]
        fn prop_diagnostics_label_independent(a in "[a-z]{1,8}", b in "[a-z]{1,8}") {
            // The fixtures are fixed, so only the report id embeds the label —
            // the diagnostics + verdicts are identical regardless of label.
            let ra = run_optimization_control_validation(&a);
            let rb = run_optimization_control_validation(&b);
            prop_assert_eq!(&ra.diagnostics, &rb.diagnostics);
            prop_assert_eq!(&ra.verdicts, &rb.verdicts);
            prop_assert_eq!(&ra.evidence_checksum, &rb.evidence_checksum);
        }

        #[test]
        fn prop_gate_always_passes(label in "[a-z]{1,8}") {
            // The harness's own corpus is green by construction; the gate must hold.
            let report = run_optimization_control_validation(&label);
            prop_assert!(report.gate_passes());
            prop_assert_eq!(report.summary.kernels_covered, 6);
        }
    }
}
