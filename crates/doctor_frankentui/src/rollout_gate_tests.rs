//! Unit/property test-evidence harness for the rollout release gates, readiness
//! rubric evaluators, and feedback scoring (bd-3bxhj.9.8).
//!
//! Release/readiness scoring is the final policy choke point before
//! release-candidate (RC) promotion: it must integrate conflicting
//! quality/perf/risk/assurance signals consistently and expose a full rationale
//! for every gate verdict. The production logic lives in three places:
//!
//! * the **readiness rubric** ([`ftui_harness::rollout_scorecard::MigrationReadinessRubric`])
//!   maps a quantitative evidence snapshot to an Advance / Hold / EmergencyHold
//!   verdict per rollout stage;
//! * the **release gate** ([`ftui_harness::rollout_scorecard::MigrationReleaseGateEvaluator`])
//!   composes that readiness verdict with certification, determinism, performance,
//!   critical-gap, and artifact-traceability clauses into a Pass / Fail verdict
//!   that blocks release only in Enforce mode;
//! * the **feedback scoring** ([`crate::feedback_ingestion::FeedbackIngestion`]) admits
//!   privacy/provenance-clean operator signals and emits a fail-closed hygiene
//!   gate plus a periodic prioritized action report.
//!
//! Those three carry their own inline unit tests; this module is the dedicated
//! *test-evidence* harness that drives all three through one fixed fixture corpus,
//! projects every per-fixture decision into a single deterministic
//! [`RolloutGateDiagnostic`] envelope, checks each fixture against an
//! expected-traffic-light oracle, and emits an auditable validation report with a
//! fail-closed gate. The unified diagnostics schema is the contract shared with
//! the `.9.9` RC-gate E2E script (AC4).
//!
//! Coverage (AC1) spans **boundary conditions** (evidence sitting exactly on a
//! pass threshold), **conflicting signals** (readiness advances while the release
//! gate fails, or vice versa — the composite takes the worst traffic light),
//! **stale / missing signals** (an active emergency hold, missing immutable
//! artifacts, a feedback corpus that never exercises a rejection), and
//! **policy-version transitions** (the same evidence evaluated against the Alpha
//! vs. GA stage gate yields a different verdict and a different `rubric_id`).
//! Determinism (AC2) is asserted by property tests that re-run the report and
//! compare it byte-for-byte for fixed inputs, and that equivalent inputs produce
//! identical verdicts and score vectors. Every diagnostic carries `policy_version`,
//! `rubric_id`, `signal_vector_hash`, `decision_output`, `violated_rule`, and a
//! `replay_cmd` (AC3).
//!
//! Like the [`crate::tier_escalator_tests`] and [`crate::portfolio_governance_tests`]
//! precedents, this is a `pub mod` compiled into the lib; all `proptest` usage is
//! confined to the `#[cfg(test)]` block so the dev-only dependency never leaks into
//! the library build. The diagnostic is **float-free** (every ratio is a
//! fixed-decimal string via [`fmt6`]), so it derives [`Eq`] and the report replays
//! byte-identically.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use ftui_harness::rollout_scorecard::{
    EmergencyHold, EmergencyHoldReason, MigrationReadinessDecision, MigrationReadinessEvidence,
    MigrationReadinessRubric, MigrationReadinessVerdict, MigrationReleaseGateArtifact,
    MigrationReleaseGateDecision, MigrationReleaseGateEvaluator, MigrationReleaseGateEvidence,
    MigrationReleaseGateMode, MigrationReleaseGatePolicy, MigrationReleaseGateVerdict,
    MigrationRolloutStage, OperatorAuthority,
};

use crate::feedback_ingestion::{
    FEEDBACK_INGESTION_SCHEMA_VERSION, FeedbackIngestion, FeedbackReport, FeedbackSignal,
    SignalKind, default_feedback_signals,
};

/// Schema version for the rollout-gate test artifacts.
pub const ROLLOUT_GATE_TESTS_SCHEMA_VERSION: &str = "rollout-gate-tests-v1";

/// The readiness rubric policy generation tested here.
pub const READINESS_RUBRIC_VERSION: &str = "readiness-rubric-v1";

/// The release gate policy generation tested here.
pub const RELEASE_GATE_VERSION: &str = "release-gate-v1";

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

/// Fixed-decimal rendering with six places (float-free, locale-independent).
fn fmt6(x: f64) -> String {
    if x.is_finite() {
        format!("{x:.6}")
    } else {
        "0.000000".to_string()
    }
}

/// The combined policy version string spanning all three evaluated subsystems.
#[must_use]
pub fn rollout_policy_version() -> String {
    format!(
        "{ROLLOUT_GATE_TESTS_SCHEMA_VERSION}+{READINESS_RUBRIC_VERSION}+{RELEASE_GATE_VERSION}+{FEEDBACK_INGESTION_SCHEMA_VERSION}"
    )
}

// ── Traffic-light composition ────────────────────────────────────────────────

/// A release-readiness traffic light. `Red` dominates `Yellow` dominates `Green`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseTrafficLight {
    /// All gates clear; the candidate may advance.
    Green,
    /// At least one non-blocking gate needs work, but nothing halts the rollout.
    Yellow,
    /// A release-blocking failure or an active emergency hold.
    Red,
}

impl ReleaseTrafficLight {
    /// Stable machine-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::Yellow => "yellow",
            Self::Red => "red",
        }
    }

    /// Severity rank (`Green` < `Yellow` < `Red`).
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Green => 0,
            Self::Yellow => 1,
            Self::Red => 2,
        }
    }

    /// The worse (higher-severity) of two lights.
    #[must_use]
    pub fn worst(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

// ── Diagnostic envelope (AC3) ────────────────────────────────────────────────

/// The unified, float-free rollout-gate diagnostic for one fixture.
///
/// This is the schema shared with the `.9.9` RC-gate E2E script. It carries the
/// AC3-mandated audit fields (`policy_version`, `rubric_id`, `signal_vector_hash`,
/// `decision_output`, `violated_rule`, `replay_cmd`) plus the per-subsystem verdict
/// breakdown that drove the composite traffic light.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutGateDiagnostic {
    /// Diagnostics schema version.
    pub schema_version: String,
    /// Stable fixture identifier.
    pub fixture_id: String,
    /// The expected traffic-light class declared by the fixture.
    pub scenario_class: String,
    /// Combined policy version across the three evaluated subsystems (AC3).
    pub policy_version: String,
    /// Stable identifier of the readiness rubric / release-gate config in force (AC3).
    pub rubric_id: String,
    /// Stable hash over the full float-free input signal vector (AC3).
    pub signal_vector_hash: String,
    /// Target rollout stage.
    pub target_stage: String,
    /// Release gate operating mode (`dry-run` / `enforce`).
    pub gate_mode: String,
    /// Readiness verdict label (`advance` / `hold` / `emergency-hold`).
    pub readiness_verdict: String,
    /// Readiness traffic light.
    pub readiness_light: String,
    /// Readiness hold reasons (empty when advancing).
    pub readiness_reasons: Vec<String>,
    /// Release gate verdict label (`pass` / `fail`).
    pub release_verdict: String,
    /// Release gate traffic light.
    pub release_light: String,
    /// Whether the release gate blocks release (Enforce + Fail).
    pub blocks_release: bool,
    /// Failed release gate clause names (empty when passing).
    pub failed_clauses: Vec<String>,
    /// Whether the feedback hygiene gate passes.
    pub feedback_gate_passes: bool,
    /// Feedback traffic light.
    pub feedback_light: String,
    /// Count of admitted feedback signals.
    pub feedback_admitted: usize,
    /// Count of rejected feedback signals.
    pub feedback_rejected: usize,
    /// Top-priority feedback category across the corpus (`none` when empty).
    pub feedback_top_category: String,
    /// The composite decision (the worst traffic light) — the gate output (AC3).
    pub decision_output: String,
    /// The first violated rule driving the composite light (empty when green) (AC3).
    pub violated_rule: String,
    /// Deterministic replay command (AC3).
    pub replay_cmd: String,
}

// ── Fixture inputs (float-free canonical form for hashing) ───────────────────

/// A readiness evidence specification (basis-point ratios keep the input
/// signal vector float-free for deterministic hashing).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessSpec {
    /// Certification pass ratio in basis points (`9000` = `0.90`).
    pub certification_bps: u32,
    /// Corpus coverage ratio in basis points.
    pub corpus_coverage_bps: u32,
    /// Operational reliability pass ratio in basis points.
    pub reliability_bps: u32,
    /// Deterministic artifact class count.
    pub deterministic_artifacts: usize,
    /// Whether the benchmark regression gate passed.
    pub benchmark_gate_passed: bool,
    /// Unresolved release blocker count.
    pub open_blockers: usize,
    /// Operator authority requesting the transition.
    pub authority: String,
    /// Optional active emergency hold reason.
    pub emergency_hold: Option<String>,
}

/// Release-gate-specific evidence beyond the shared readiness snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseSpec {
    /// Deterministic comparison pass ratio in basis points.
    pub determinism_bps: u32,
    /// Whether the performance budget gate passed.
    pub performance_budget_passed: bool,
    /// Unresolved critical capability gap count.
    pub unresolved_critical_gaps: usize,
    /// Artifact kinds for which an immutable artifact is supplied.
    pub artifact_kinds: Vec<String>,
}

/// Which canonical feedback corpus a fixture drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackCorpus {
    /// The default mixed corpus (quant + qual + three rejections) — gate passes.
    Default,
    /// An admissible-only corpus that never exercises a rejection — gate fails.
    NoRejection,
    /// A single-modality corpus (quantitative only) — gate fails on coverage.
    SingleModality,
}

impl FeedbackCorpus {
    fn signals(self) -> Vec<FeedbackSignal> {
        match self {
            Self::Default => default_feedback_signals(),
            Self::NoRejection => admissible_only_signals(),
            Self::SingleModality => quantitative_only_signals(),
        }
    }
}

/// One end-to-end rollout-gate fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutGateFixture {
    /// Stable fixture id.
    pub fixture_id: String,
    /// Human-readable scenario description.
    pub description: String,
    /// Expected composite traffic light (oracle).
    pub expected_class: ReleaseTrafficLight,
    /// Target rollout stage.
    pub target_stage: String,
    /// Release gate operating mode.
    pub gate_mode: String,
    /// Readiness evidence specification.
    pub readiness: ReadinessSpec,
    /// Release evidence specification.
    pub release: ReleaseSpec,
    /// Feedback corpus selector.
    pub feedback_corpus: FeedbackCorpus,
}

// ── Per-fixture result + report ──────────────────────────────────────────────

/// The evaluation of one fixture: the diagnostic plus its oracle verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutGateFixtureResult {
    /// The unified diagnostic envelope.
    pub diagnostic: RolloutGateDiagnostic,
    /// Expected traffic-light class.
    pub expected_class: String,
    /// Observed traffic-light class.
    pub observed_class: String,
    /// Whether the observed class matched the oracle.
    pub oracle_pass: bool,
}

/// The aggregate validation report with a fail-closed gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutGateTestReport {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Combined policy version.
    pub policy_version: String,
    /// Per-fixture results.
    pub fixtures: Vec<RolloutGateFixtureResult>,
    /// Total fixtures evaluated.
    pub total: usize,
    /// Count of fixtures observed green.
    pub green: usize,
    /// Count of fixtures observed yellow.
    pub yellow: usize,
    /// Count of fixtures observed red.
    pub red: usize,
    /// Count of fixtures whose observed class diverged from the oracle.
    pub oracle_failures: usize,
    /// Whether every oracle matched (the fail-closed gate).
    pub gate_passes: bool,
    /// Stable checksum over every diagnostic.
    pub evidence_checksum: String,
    /// Deterministic replay command.
    pub replay_command: String,
}

impl RolloutGateTestReport {
    /// Render the diagnostic ledger as JSONL (one diagnostic per line).
    #[must_use]
    pub fn render_ledger_jsonl(&self) -> String {
        let mut out = String::new();
        for result in &self.fixtures {
            match serde_json::to_string(&result.diagnostic) {
                Ok(line) => out.push_str(&line),
                Err(error) => out.push_str(&format!("{{\"error\":\"{error}\"}}")),
            }
            out.push('\n');
        }
        out
    }

    /// The diagnostics for every fixture observed at or above `light` severity
    /// (used by the `.9.9` RC gate to surface blockers).
    #[must_use]
    pub fn diagnostics_at_least(&self, light: ReleaseTrafficLight) -> Vec<&RolloutGateDiagnostic> {
        self.fixtures
            .iter()
            .filter(|r| traffic_light_from_label(&r.observed_class).rank() >= light.rank())
            .map(|r| &r.diagnostic)
            .collect()
    }
}

// ── Input → production-type construction ─────────────────────────────────────

fn bps_to_ratio(bps: u32) -> f64 {
    f64::from(bps) / 10_000.0
}

fn parse_authority(label: &str) -> OperatorAuthority {
    match label {
        "automation" => OperatorAuthority::Automation,
        "on-call" => OperatorAuthority::OnCall,
        "maintainer-quorum" => OperatorAuthority::MaintainerQuorum,
        // Default and canonical "release-owner".
        _ => OperatorAuthority::ReleaseOwner,
    }
}

fn parse_stage(label: &str) -> MigrationRolloutStage {
    match label {
        "beta" => MigrationRolloutStage::Beta,
        "ga" => MigrationRolloutStage::Ga,
        _ => MigrationRolloutStage::Alpha,
    }
}

fn parse_mode(label: &str) -> MigrationReleaseGateMode {
    match label {
        "enforce" => MigrationReleaseGateMode::Enforce,
        _ => MigrationReleaseGateMode::DryRun,
    }
}

fn parse_hold_reason(label: &str) -> EmergencyHoldReason {
    match label {
        "determinism-divergence" => EmergencyHoldReason::DeterminismDivergence,
        "security-incident" => EmergencyHoldReason::SecurityIncident,
        "reliability-breach" => EmergencyHoldReason::ReliabilityBreach,
        "missing-evidence" => EmergencyHoldReason::MissingEvidence,
        "operator-override" => EmergencyHoldReason::OperatorOverride,
        _ => EmergencyHoldReason::CertificationRegression,
    }
}

fn build_readiness_evidence(spec: &ReadinessSpec) -> MigrationReadinessEvidence {
    let mut evidence = MigrationReadinessEvidence::new(parse_authority(&spec.authority))
        .certification_pass_ratio(bps_to_ratio(spec.certification_bps))
        .corpus_coverage_ratio(bps_to_ratio(spec.corpus_coverage_bps))
        .reliability_pass_ratio(bps_to_ratio(spec.reliability_bps))
        .deterministic_artifact_count(spec.deterministic_artifacts)
        .benchmark_gate_passed(spec.benchmark_gate_passed)
        .open_blocker_count(spec.open_blockers);
    if let Some(reason) = &spec.emergency_hold {
        evidence = evidence.emergency_hold(EmergencyHold::new(
            parse_hold_reason(reason),
            OperatorAuthority::OnCall,
        ));
    }
    evidence
}

/// Construct a deterministic immutable artifact for one required kind. The
/// `index` keeps the artifact id unique even when a kind repeats (padding the
/// traceable-artifact count for stricter stages).
fn fixture_artifact(fixture_id: &str, index: usize, kind: &str) -> MigrationReleaseGateArtifact {
    let artifact_id = format!("art.{fixture_id}.{index:02}.{kind}");
    let digest = format!("sha256:{}", sha256_hex(artifact_id.as_bytes()));
    let uri = format!("cas://{fixture_id}/{index:02}/{kind}");
    MigrationReleaseGateArtifact::new(&artifact_id, kind, &digest, &uri)
}

fn build_release_evidence(
    fixture_id: &str,
    readiness: &ReadinessSpec,
    release: &ReleaseSpec,
) -> MigrationReleaseGateEvidence {
    let mut evidence =
        MigrationReleaseGateEvidence::new(fixture_id, build_readiness_evidence(readiness))
            .determinism_pass_ratio(bps_to_ratio(release.determinism_bps))
            .performance_budget_passed(release.performance_budget_passed)
            .unresolved_critical_gap_count(release.unresolved_critical_gaps);
    for (index, kind) in release.artifact_kinds.iter().enumerate() {
        evidence = evidence.artifact(fixture_artifact(fixture_id, index, kind));
    }
    evidence
}

// ── Stable rubric identifier (policy-version discriminator) ───────────────────

/// Float-free canonical projection of the readiness gate + release policy
/// thresholds for one stage; hashing it yields the `rubric_id`.
#[derive(Serialize)]
struct RubricFingerprint {
    readiness_rubric_version: &'static str,
    release_gate_version: &'static str,
    stage: &'static str,
    min_certification: String,
    min_corpus_coverage: String,
    min_reliability: String,
    min_deterministic_artifacts: usize,
    require_benchmark_gate: bool,
    max_open_blockers: usize,
    required_authority: &'static str,
    gate_min_certification: String,
    gate_min_determinism: String,
    gate_require_performance: bool,
    gate_max_critical_gaps: usize,
    gate_min_artifacts: usize,
    gate_required_kinds: Vec<&'static str>,
}

/// The stable `rubric_id` for a stage: a hash over the readiness gate and release
/// policy thresholds. Two stages necessarily produce different ids, so this is the
/// policy-version discriminator exercised by the policy-transition fixtures.
#[must_use]
pub fn rubric_id_for(stage: MigrationRolloutStage) -> String {
    let rubric = MigrationReadinessRubric::opentui_default();
    let gate = rubric
        .gate(stage)
        .expect("opentui_default rubric covers every stage");
    let policy = MigrationReleaseGatePolicy::for_stage(stage, MigrationReleaseGateMode::Enforce);
    let fingerprint = RubricFingerprint {
        readiness_rubric_version: READINESS_RUBRIC_VERSION,
        release_gate_version: RELEASE_GATE_VERSION,
        stage: stage.label(),
        min_certification: fmt6(gate.min_certification_pass_ratio),
        min_corpus_coverage: fmt6(gate.min_corpus_coverage_ratio),
        min_reliability: fmt6(gate.min_reliability_pass_ratio),
        min_deterministic_artifacts: gate.min_deterministic_artifacts,
        require_benchmark_gate: gate.require_benchmark_gate,
        max_open_blockers: gate.max_open_blockers,
        required_authority: gate.required_authority.label(),
        gate_min_certification: fmt6(policy.min_certification_pass_ratio),
        gate_min_determinism: fmt6(policy.min_determinism_pass_ratio),
        gate_require_performance: policy.require_performance_budget_pass,
        gate_max_critical_gaps: policy.max_unresolved_critical_gaps,
        gate_min_artifacts: policy.min_traceable_artifacts,
        gate_required_kinds: policy.required_artifact_kinds.clone(),
    };
    format!(
        "rubric-{}-{}",
        stage.label(),
        short_hash(&stable_hash(&fingerprint))
    )
}

// ── Composition / classification ─────────────────────────────────────────────

fn readiness_light(decision: &MigrationReadinessDecision) -> ReleaseTrafficLight {
    match decision.verdict {
        MigrationReadinessVerdict::Advance => ReleaseTrafficLight::Green,
        MigrationReadinessVerdict::Hold => ReleaseTrafficLight::Yellow,
        MigrationReadinessVerdict::EmergencyHold => ReleaseTrafficLight::Red,
    }
}

fn release_light(decision: &MigrationReleaseGateDecision) -> ReleaseTrafficLight {
    match decision.verdict {
        MigrationReleaseGateVerdict::Pass => ReleaseTrafficLight::Green,
        MigrationReleaseGateVerdict::Fail => {
            if decision.blocks_release() {
                ReleaseTrafficLight::Red
            } else {
                ReleaseTrafficLight::Yellow
            }
        }
    }
}

fn feedback_light(report: &FeedbackReport) -> ReleaseTrafficLight {
    if report.gate_passes {
        ReleaseTrafficLight::Green
    } else {
        ReleaseTrafficLight::Yellow
    }
}

fn traffic_light_from_label(label: &str) -> ReleaseTrafficLight {
    match label {
        "red" => ReleaseTrafficLight::Red,
        "yellow" => ReleaseTrafficLight::Yellow,
        _ => ReleaseTrafficLight::Green,
    }
}

/// Determine the first violated rule in a fixed priority order
/// (readiness → release → feedback). Empty when everything is green.
fn first_violated_rule(
    readiness: &MigrationReadinessDecision,
    release: &MigrationReleaseGateDecision,
    feedback: &FeedbackReport,
) -> String {
    // Emergency hold and release-blocking failures dominate.
    if readiness.verdict == MigrationReadinessVerdict::EmergencyHold {
        let reason = readiness.reasons.first().copied().unwrap_or("unknown");
        return format!("emergency-hold:{reason}");
    }
    if release.blocks_release() {
        let clause = release
            .failed_clauses()
            .first()
            .copied()
            .unwrap_or("unknown");
        return format!("release-blocked:{clause}");
    }
    // Non-blocking issues, in the same priority order.
    if readiness.verdict == MigrationReadinessVerdict::Hold {
        let reason = readiness.reasons.first().copied().unwrap_or("unknown");
        return format!("readiness-hold:{reason}");
    }
    if release.verdict == MigrationReleaseGateVerdict::Fail {
        let clause = release
            .failed_clauses()
            .first()
            .copied()
            .unwrap_or("unknown");
        return format!("release-clause:{clause}");
    }
    if !feedback.gate_passes {
        return "feedback-gate".to_string();
    }
    String::new()
}

// ── Fixture evaluation ───────────────────────────────────────────────────────

/// Evaluate one fixture end-to-end across the three subsystems.
#[must_use]
pub fn evaluate_fixture(fixture: &RolloutGateFixture) -> RolloutGateFixtureResult {
    let stage = parse_stage(&fixture.target_stage);
    let mode = parse_mode(&fixture.gate_mode);

    // Readiness rubric.
    let rubric = MigrationReadinessRubric::opentui_default();
    let readiness_evidence = build_readiness_evidence(&fixture.readiness);
    let readiness_decision = rubric.evaluate(stage, &readiness_evidence);

    // Release gate.
    let policy = MigrationReleaseGatePolicy::for_stage(stage, mode);
    let evaluator = MigrationReleaseGateEvaluator::new(policy);
    let release_evidence =
        build_release_evidence(&fixture.fixture_id, &fixture.readiness, &fixture.release);
    let release_decision = evaluator.evaluate(&release_evidence);

    // Feedback scoring.
    let feedback_label = format!("rollout-gate-{}", fixture.fixture_id);
    let feedback_signals = fixture.feedback_corpus.signals();
    let feedback_report = FeedbackIngestion::new(feedback_label).run(&feedback_signals);

    // Composition.
    let r_light = readiness_light(&readiness_decision);
    let g_light = release_light(&release_decision);
    let f_light = feedback_light(&feedback_report);
    let observed = r_light.worst(g_light).worst(f_light);

    let violated_rule =
        first_violated_rule(&readiness_decision, &release_decision, &feedback_report);

    let feedback_top_category = feedback_report
        .action_reports
        .first()
        .map_or_else(|| "none".to_string(), |r| r.top_category.clone());

    let signal_vector_hash = short_hash(&stable_hash(fixture));
    let replay_cmd = format!(
        "cargo test -p doctor_frankentui --lib rollout_gate_tests -- {} # {}",
        fixture.fixture_id,
        observed.label()
    );

    let diagnostic = RolloutGateDiagnostic {
        schema_version: ROLLOUT_GATE_TESTS_SCHEMA_VERSION.to_string(),
        fixture_id: fixture.fixture_id.clone(),
        scenario_class: fixture.expected_class.label().to_string(),
        policy_version: rollout_policy_version(),
        rubric_id: rubric_id_for(stage),
        signal_vector_hash,
        target_stage: stage.label().to_string(),
        gate_mode: mode.label().to_string(),
        readiness_verdict: readiness_decision.verdict.label().to_string(),
        readiness_light: r_light.label().to_string(),
        readiness_reasons: readiness_decision
            .reasons
            .iter()
            .map(|r| (*r).to_string())
            .collect(),
        release_verdict: release_decision.verdict.label().to_string(),
        release_light: g_light.label().to_string(),
        blocks_release: release_decision.blocks_release(),
        failed_clauses: release_decision
            .failed_clauses()
            .iter()
            .map(|c| (*c).to_string())
            .collect(),
        feedback_gate_passes: feedback_report.gate_passes,
        feedback_light: f_light.label().to_string(),
        feedback_admitted: feedback_report.summary.admitted_signals,
        feedback_rejected: feedback_report.summary.rejected_signals,
        feedback_top_category,
        decision_output: observed.label().to_string(),
        violated_rule,
        replay_cmd,
    };

    let oracle_pass = observed == fixture.expected_class;
    RolloutGateFixtureResult {
        diagnostic,
        expected_class: fixture.expected_class.label().to_string(),
        observed_class: observed.label().to_string(),
        oracle_pass,
    }
}

/// Evaluate a fixture corpus and aggregate into a fail-closed report.
#[must_use]
pub fn run_rollout_gate_tests_with(fixtures: &[RolloutGateFixture]) -> RolloutGateTestReport {
    let results: Vec<RolloutGateFixtureResult> = fixtures.iter().map(evaluate_fixture).collect();

    let mut green = 0usize;
    let mut yellow = 0usize;
    let mut red = 0usize;
    let mut oracle_failures = 0usize;
    for result in &results {
        match traffic_light_from_label(&result.observed_class) {
            ReleaseTrafficLight::Green => green += 1,
            ReleaseTrafficLight::Yellow => yellow += 1,
            ReleaseTrafficLight::Red => red += 1,
        }
        if !result.oracle_pass {
            oracle_failures += 1;
        }
    }

    let diagnostics: Vec<&RolloutGateDiagnostic> = results.iter().map(|r| &r.diagnostic).collect();
    let evidence_checksum = stable_hash(&diagnostics);
    let report_id = format!(
        "rollout-gate-tests-{}",
        short_hash(&stable_hash(&format!(
            "{ROLLOUT_GATE_TESTS_SCHEMA_VERSION}|{evidence_checksum}"
        )))
    );
    // Non-vacuous: an empty corpus proves nothing and must fail closed —
    // "zero oracle failures" over zero fixtures is absence of evidence.
    let gate_passes = !results.is_empty() && oracle_failures == 0;

    RolloutGateTestReport {
        schema_version: ROLLOUT_GATE_TESTS_SCHEMA_VERSION.to_string(),
        report_id,
        policy_version: rollout_policy_version(),
        total: results.len(),
        green,
        yellow,
        red,
        oracle_failures,
        gate_passes,
        evidence_checksum,
        replay_command: "cargo test -p doctor_frankentui --lib rollout_gate_tests".to_string(),
        fixtures: results,
    }
}

/// Evaluate the default green/yellow/red fixture matrix.
#[must_use]
pub fn run_rollout_gate_tests() -> RolloutGateTestReport {
    run_rollout_gate_tests_with(&default_rollout_gate_fixtures())
}

// ── Degraded feedback corpora ────────────────────────────────────────────────

/// An admissible-only feedback corpus: quant + qual signals across two periods
/// with valid provenance/anonymization, but *no* inadmissible signal — so the
/// feedback gate fails on `rejection_exercised` (a stale-self-test signal).
fn admissible_only_signals() -> Vec<FeedbackSignal> {
    default_feedback_signals()
        .into_iter()
        .filter(|s| !s.signal_id.starts_with("fb.bad."))
        .collect()
}

/// A single-modality (quantitative-only) corpus: it never represents the
/// qualitative modality, so `schema_covers_both` fails.
fn quantitative_only_signals() -> Vec<FeedbackSignal> {
    default_feedback_signals()
        .into_iter()
        .filter(|s| matches!(s.kind, SignalKind::Quantitative { .. }))
        .collect()
}

// ── Default fixture matrix ───────────────────────────────────────────────────

/// The canonical green / yellow / red fixture matrix.
///
/// The matrix exercises boundary conditions (`alpha-green-boundary` sits exactly
/// on every Alpha threshold), conflicting signals (`alpha-yellow-determinism`
/// advances readiness but fails the determinism clause in dry-run; `beta-red-enforce-gaps`
/// advances readiness but is release-blocked under Enforce), stale/missing signals
/// (`alpha-red-emergency-hold`, `alpha-yellow-feedback-hygiene`,
/// `beta-red-enforce-artifacts`), and policy-version transitions (`policy-alpha-advance`
/// vs. `policy-ga-hold` evaluate identical evidence at different stages).
#[must_use]
pub fn default_rollout_gate_fixtures() -> Vec<RolloutGateFixture> {
    // Every required release-gate artifact kind, so the traceability clause is
    // satisfied whenever a fixture supplies the full set.
    let all_kinds = || {
        vec![
            "certification".to_string(),
            "critical-gaps".to_string(),
            "determinism".to_string(),
            "performance".to_string(),
            "readiness".to_string(),
        ]
    };
    // Beta requires >= 6 immutable artifacts; pad the 5 required kinds with one
    // extra determinism artifact so the traceability count clears.
    let beta_kinds = || {
        let mut kinds = all_kinds();
        kinds.push("determinism".to_string());
        kinds
    };

    vec![
        // ── GREEN ────────────────────────────────────────────────────────────
        // Boundary: every value sits exactly on the Alpha pass threshold.
        RolloutGateFixture {
            fixture_id: "alpha-green-boundary".to_string(),
            description: "Alpha evidence on the exact pass boundary advances and clears the gate"
                .to_string(),
            expected_class: ReleaseTrafficLight::Green,
            target_stage: "alpha".to_string(),
            gate_mode: "enforce".to_string(),
            readiness: ReadinessSpec {
                certification_bps: 9000,
                corpus_coverage_bps: 2500,
                reliability_bps: 9500,
                deterministic_artifacts: 3,
                benchmark_gate_passed: true,
                open_blockers: 2,
                authority: "release-owner".to_string(),
                emergency_hold: None,
            },
            release: ReleaseSpec {
                determinism_bps: 9900,
                performance_budget_passed: true,
                unresolved_critical_gaps: 2,
                artifact_kinds: all_kinds(),
            },
            feedback_corpus: FeedbackCorpus::Default,
        },
        // Comfortably-above-threshold Beta promotion.
        RolloutGateFixture {
            fixture_id: "beta-green-strong".to_string(),
            description: "Beta evidence comfortably above every threshold clears the gate"
                .to_string(),
            expected_class: ReleaseTrafficLight::Green,
            target_stage: "beta".to_string(),
            gate_mode: "enforce".to_string(),
            readiness: ReadinessSpec {
                certification_bps: 9900,
                corpus_coverage_bps: 7500,
                reliability_bps: 9900,
                deterministic_artifacts: 6,
                benchmark_gate_passed: true,
                open_blockers: 0,
                authority: "release-owner".to_string(),
                emergency_hold: None,
            },
            release: ReleaseSpec {
                determinism_bps: 10000,
                performance_budget_passed: true,
                unresolved_critical_gaps: 0,
                // Beta requires >= 6 immutable artifacts; pad past the 5 kinds.
                artifact_kinds: beta_kinds(),
            },
            feedback_corpus: FeedbackCorpus::Default,
        },
        // ── YELLOW ───────────────────────────────────────────────────────────
        // Conflicting: readiness holds (cert one bp below) in dry-run, so the
        // release gate fails non-blocking and the worst light is yellow.
        RolloutGateFixture {
            fixture_id: "alpha-yellow-readiness-hold".to_string(),
            description:
                "Certification one bp below threshold holds readiness; dry-run keeps it yellow"
                    .to_string(),
            expected_class: ReleaseTrafficLight::Yellow,
            target_stage: "alpha".to_string(),
            gate_mode: "dry-run".to_string(),
            readiness: ReadinessSpec {
                certification_bps: 8999,
                corpus_coverage_bps: 2500,
                reliability_bps: 9500,
                deterministic_artifacts: 3,
                benchmark_gate_passed: true,
                open_blockers: 2,
                authority: "release-owner".to_string(),
                emergency_hold: None,
            },
            release: ReleaseSpec {
                determinism_bps: 9900,
                performance_budget_passed: true,
                unresolved_critical_gaps: 2,
                artifact_kinds: all_kinds(),
            },
            feedback_corpus: FeedbackCorpus::Default,
        },
        // Conflicting: readiness advances but determinism is one bp short in dry-run.
        RolloutGateFixture {
            fixture_id: "alpha-yellow-determinism".to_string(),
            description: "Readiness advances but determinism one bp short fails the dry-run gate"
                .to_string(),
            expected_class: ReleaseTrafficLight::Yellow,
            target_stage: "alpha".to_string(),
            gate_mode: "dry-run".to_string(),
            readiness: ReadinessSpec {
                certification_bps: 9500,
                corpus_coverage_bps: 3000,
                reliability_bps: 9700,
                deterministic_artifacts: 4,
                benchmark_gate_passed: true,
                open_blockers: 1,
                authority: "release-owner".to_string(),
                emergency_hold: None,
            },
            release: ReleaseSpec {
                determinism_bps: 9899,
                performance_budget_passed: true,
                unresolved_critical_gaps: 0,
                artifact_kinds: all_kinds(),
            },
            feedback_corpus: FeedbackCorpus::Default,
        },
        // Stale signal: readiness + release both pass, but the feedback corpus
        // never exercises a rejection, so the hygiene gate fails (yellow).
        RolloutGateFixture {
            fixture_id: "alpha-yellow-feedback-hygiene".to_string(),
            description:
                "All release gates pass but the feedback corpus never exercises a rejection"
                    .to_string(),
            expected_class: ReleaseTrafficLight::Yellow,
            target_stage: "alpha".to_string(),
            gate_mode: "enforce".to_string(),
            readiness: ReadinessSpec {
                certification_bps: 9500,
                corpus_coverage_bps: 3000,
                reliability_bps: 9700,
                deterministic_artifacts: 5,
                benchmark_gate_passed: true,
                open_blockers: 0,
                authority: "release-owner".to_string(),
                emergency_hold: None,
            },
            release: ReleaseSpec {
                determinism_bps: 10000,
                performance_budget_passed: true,
                unresolved_critical_gaps: 0,
                artifact_kinds: all_kinds(),
            },
            feedback_corpus: FeedbackCorpus::NoRejection,
        },
        // ── RED ──────────────────────────────────────────────────────────────
        // Stale/critical: an active emergency hold halts the rollout outright.
        RolloutGateFixture {
            fixture_id: "alpha-red-emergency-hold".to_string(),
            description: "An active emergency hold halts the rollout regardless of other evidence"
                .to_string(),
            expected_class: ReleaseTrafficLight::Red,
            target_stage: "alpha".to_string(),
            gate_mode: "enforce".to_string(),
            readiness: ReadinessSpec {
                certification_bps: 9900,
                corpus_coverage_bps: 9000,
                reliability_bps: 9900,
                deterministic_artifacts: 8,
                benchmark_gate_passed: true,
                open_blockers: 0,
                authority: "release-owner".to_string(),
                emergency_hold: Some("security-incident".to_string()),
            },
            release: ReleaseSpec {
                determinism_bps: 10000,
                performance_budget_passed: true,
                unresolved_critical_gaps: 0,
                artifact_kinds: all_kinds(),
            },
            feedback_corpus: FeedbackCorpus::Default,
        },
        // Conflicting + enforce: readiness advances but unresolved critical gaps
        // exceed the Beta budget under Enforce, which blocks release (red).
        RolloutGateFixture {
            fixture_id: "beta-red-enforce-gaps".to_string(),
            description:
                "Readiness advances but an unresolved critical gap blocks release under Enforce"
                    .to_string(),
            expected_class: ReleaseTrafficLight::Red,
            target_stage: "beta".to_string(),
            gate_mode: "enforce".to_string(),
            readiness: ReadinessSpec {
                certification_bps: 9800,
                corpus_coverage_bps: 7000,
                reliability_bps: 9900,
                deterministic_artifacts: 6,
                benchmark_gate_passed: true,
                open_blockers: 0,
                authority: "release-owner".to_string(),
                emergency_hold: None,
            },
            release: ReleaseSpec {
                determinism_bps: 10000,
                performance_budget_passed: true,
                unresolved_critical_gaps: 1,
                // Full Beta artifact set, so only the critical-gaps clause fails.
                artifact_kinds: beta_kinds(),
            },
            feedback_corpus: FeedbackCorpus::Default,
        },
        // Missing signal + enforce: a required artifact kind is absent, failing the
        // traceability clause and blocking release (red).
        RolloutGateFixture {
            fixture_id: "beta-red-enforce-artifacts".to_string(),
            description: "A missing immutable artifact kind fails traceability and blocks release"
                .to_string(),
            expected_class: ReleaseTrafficLight::Red,
            target_stage: "beta".to_string(),
            gate_mode: "enforce".to_string(),
            readiness: ReadinessSpec {
                certification_bps: 9800,
                corpus_coverage_bps: 7000,
                reliability_bps: 9900,
                deterministic_artifacts: 6,
                benchmark_gate_passed: true,
                open_blockers: 0,
                authority: "release-owner".to_string(),
                emergency_hold: None,
            },
            release: ReleaseSpec {
                determinism_bps: 10000,
                performance_budget_passed: true,
                unresolved_critical_gaps: 0,
                // Drop "readiness" — the traceability clause fails on the missing kind.
                artifact_kinds: vec![
                    "certification".to_string(),
                    "critical-gaps".to_string(),
                    "determinism".to_string(),
                    "performance".to_string(),
                ],
            },
            feedback_corpus: FeedbackCorpus::SingleModality,
        },
        // ── POLICY-VERSION TRANSITION ─────────────────────────────────────────
        // Identical evidence evaluated at Alpha (advances → green) ...
        RolloutGateFixture {
            fixture_id: "policy-alpha-advance".to_string(),
            description: "Mid-grade evidence advances at the permissive Alpha gate".to_string(),
            expected_class: ReleaseTrafficLight::Green,
            target_stage: "alpha".to_string(),
            gate_mode: "dry-run".to_string(),
            readiness: ReadinessSpec {
                certification_bps: 9200,
                corpus_coverage_bps: 3000,
                reliability_bps: 9600,
                deterministic_artifacts: 4,
                benchmark_gate_passed: true,
                open_blockers: 1,
                authority: "release-owner".to_string(),
                emergency_hold: None,
            },
            release: ReleaseSpec {
                determinism_bps: 9900,
                performance_budget_passed: true,
                unresolved_critical_gaps: 1,
                artifact_kinds: all_kinds(),
            },
            feedback_corpus: FeedbackCorpus::Default,
        },
        // ... and the same evidence held at the stricter GA gate (hold → yellow).
        RolloutGateFixture {
            fixture_id: "policy-ga-hold".to_string(),
            description: "The same mid-grade evidence is held at the stricter GA gate".to_string(),
            expected_class: ReleaseTrafficLight::Yellow,
            target_stage: "ga".to_string(),
            gate_mode: "dry-run".to_string(),
            readiness: ReadinessSpec {
                certification_bps: 9200,
                corpus_coverage_bps: 3000,
                reliability_bps: 9600,
                deterministic_artifacts: 4,
                benchmark_gate_passed: true,
                open_blockers: 1,
                authority: "maintainer-quorum".to_string(),
                emergency_hold: None,
            },
            release: ReleaseSpec {
                determinism_bps: 9900,
                performance_budget_passed: true,
                unresolved_critical_gaps: 1,
                artifact_kinds: all_kinds(),
            },
            feedback_corpus: FeedbackCorpus::Default,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn fixture(id: &str) -> RolloutGateFixture {
        default_rollout_gate_fixtures()
            .into_iter()
            .find(|f| f.fixture_id == id)
            .expect("fixture id not found in the default rollout-gate matrix")
    }

    // ── AC4: the default matrix passes its own oracle (gate green) ────────────

    #[test]
    fn empty_corpus_fails_closed() {
        // Zero oracle failures over zero fixtures is absence of evidence, not
        // a green gate.
        let report = run_rollout_gate_tests_with(&[]);
        assert_eq!(report.total, 0);
        assert!(!report.gate_passes, "empty corpus must not pass the gate");
    }

    #[test]
    fn default_matrix_gate_passes() {
        let report = run_rollout_gate_tests();
        assert!(
            report.gate_passes,
            "oracle mismatches: {:#?}",
            report
                .fixtures
                .iter()
                .filter(|r| !r.oracle_pass)
                .map(|r| (
                    r.diagnostic.fixture_id.clone(),
                    r.expected_class.clone(),
                    r.observed_class.clone()
                ))
                .collect::<Vec<_>>()
        );
        assert_eq!(report.oracle_failures, 0);
        assert_eq!(report.total, default_rollout_gate_fixtures().len());
    }

    #[test]
    fn matrix_covers_all_three_lights() {
        let report = run_rollout_gate_tests();
        assert!(
            report.green >= 2,
            "expected green fixtures: {}",
            report.green
        );
        assert!(
            report.yellow >= 2,
            "expected yellow fixtures: {}",
            report.yellow
        );
        assert!(report.red >= 2, "expected red fixtures: {}", report.red);
        assert_eq!(report.green + report.yellow + report.red, report.total);
    }

    // ── AC1: boundary conditions ──────────────────────────────────────────────

    #[test]
    fn exact_boundary_advances_and_passes() {
        let result = evaluate_fixture(&fixture("alpha-green-boundary"));
        assert_eq!(result.observed_class, "green");
        assert_eq!(result.diagnostic.readiness_verdict, "advance");
        assert_eq!(result.diagnostic.release_verdict, "pass");
        assert!(result.diagnostic.violated_rule.is_empty());
        assert!(result.diagnostic.readiness_reasons.is_empty());
        assert!(result.diagnostic.failed_clauses.is_empty());
    }

    #[test]
    fn one_bp_below_readiness_threshold_holds() {
        let result = evaluate_fixture(&fixture("alpha-yellow-readiness-hold"));
        assert_eq!(result.observed_class, "yellow");
        assert_eq!(result.diagnostic.readiness_verdict, "hold");
        assert!(
            result
                .diagnostic
                .readiness_reasons
                .iter()
                .any(|r| r == "certification-threshold"),
            "reasons: {:?}",
            result.diagnostic.readiness_reasons
        );
        // Dry-run keeps the failing release clause non-blocking.
        assert!(!result.diagnostic.blocks_release);
        assert!(
            result
                .diagnostic
                .violated_rule
                .starts_with("readiness-hold:")
        );
    }

    #[test]
    fn one_bp_below_determinism_threshold_fails_clause() {
        let result = evaluate_fixture(&fixture("alpha-yellow-determinism"));
        assert_eq!(result.observed_class, "yellow");
        assert_eq!(result.diagnostic.readiness_verdict, "advance");
        assert_eq!(result.diagnostic.release_verdict, "fail");
        assert!(
            result
                .diagnostic
                .failed_clauses
                .iter()
                .any(|c| c == "determinism"),
            "clauses: {:?}",
            result.diagnostic.failed_clauses
        );
        assert_eq!(
            result.diagnostic.violated_rule,
            "release-clause:determinism"
        );
    }

    // ── AC1: conflicting signals (worst-light wins) ───────────────────────────

    #[test]
    fn enforce_blocking_release_dominates_advancing_readiness() {
        let result = evaluate_fixture(&fixture("beta-red-enforce-gaps"));
        assert_eq!(result.observed_class, "red");
        // Readiness advanced — the conflict resolves to the worst (red) light.
        assert_eq!(result.diagnostic.readiness_verdict, "advance");
        assert!(result.diagnostic.blocks_release);
        assert!(
            result
                .diagnostic
                .failed_clauses
                .iter()
                .any(|c| c == "critical-gaps")
        );
        assert!(
            result
                .diagnostic
                .violated_rule
                .starts_with("release-blocked:")
        );
    }

    #[test]
    fn dry_run_never_blocks_release() {
        for f in default_rollout_gate_fixtures() {
            if f.gate_mode == "dry-run" {
                let result = evaluate_fixture(&f);
                assert!(
                    !result.diagnostic.blocks_release,
                    "dry-run fixture {} must not block release",
                    f.fixture_id
                );
            }
        }
    }

    // ── AC1: stale / missing signals ──────────────────────────────────────────

    #[test]
    fn emergency_hold_forces_red_despite_perfect_evidence() {
        let result = evaluate_fixture(&fixture("alpha-red-emergency-hold"));
        assert_eq!(result.observed_class, "red");
        assert_eq!(result.diagnostic.readiness_verdict, "emergency-hold");
        assert!(
            result
                .diagnostic
                .violated_rule
                .starts_with("emergency-hold:")
        );
        assert!(
            result
                .diagnostic
                .readiness_reasons
                .iter()
                .any(|r| r == "security-incident")
        );
    }

    #[test]
    fn missing_artifact_kind_blocks_release() {
        let result = evaluate_fixture(&fixture("beta-red-enforce-artifacts"));
        assert_eq!(result.observed_class, "red");
        assert!(
            result
                .diagnostic
                .failed_clauses
                .iter()
                .any(|c| c == "artifact-traceability"),
            "clauses: {:?}",
            result.diagnostic.failed_clauses
        );
        assert!(result.diagnostic.blocks_release);
    }

    #[test]
    fn feedback_hygiene_failure_is_yellow_not_blocking() {
        let result = evaluate_fixture(&fixture("alpha-yellow-feedback-hygiene"));
        assert_eq!(result.observed_class, "yellow");
        assert_eq!(result.diagnostic.readiness_verdict, "advance");
        assert_eq!(result.diagnostic.release_verdict, "pass");
        assert!(!result.diagnostic.feedback_gate_passes);
        assert_eq!(result.diagnostic.violated_rule, "feedback-gate");
    }

    #[test]
    fn single_modality_corpus_fails_feedback_gate() {
        // The red-artifacts fixture also drives a single-modality corpus; confirm
        // the corpus itself fails the feedback gate in isolation.
        let report = FeedbackIngestion::new("iso-single").run(&quantitative_only_signals());
        assert!(!report.gate_passes);
        assert!(!report.summary.schema_covers_both);
    }

    #[test]
    fn no_rejection_corpus_fails_on_rejection_exercised() {
        let report = FeedbackIngestion::new("iso-norej").run(&admissible_only_signals());
        assert!(!report.gate_passes);
        assert!(!report.summary.rejection_exercised);
        // Everything admitted is still privacy/provenance clean.
        assert!(report.summary.privacy_enforced || report.summary.admitted_signals == 0);
        assert_eq!(report.summary.rejected_signals, 0);
    }

    #[test]
    fn default_corpus_passes_feedback_gate() {
        let report = FeedbackIngestion::new("iso-default").run(&default_feedback_signals());
        assert!(report.gate_passes);
        assert!(report.summary.rejection_exercised);
        assert!(report.summary.schema_covers_both);
    }

    // ── AC1: policy-version transitions ───────────────────────────────────────

    #[test]
    fn same_evidence_shifts_verdict_across_stage_policies() {
        let alpha = evaluate_fixture(&fixture("policy-alpha-advance"));
        let ga = evaluate_fixture(&fixture("policy-ga-hold"));
        assert_eq!(alpha.observed_class, "green");
        assert_eq!(ga.observed_class, "yellow");
        assert_eq!(alpha.diagnostic.readiness_verdict, "advance");
        assert_eq!(ga.diagnostic.readiness_verdict, "hold");
    }

    #[test]
    fn rubric_id_is_distinct_per_stage() {
        let alpha = rubric_id_for(MigrationRolloutStage::Alpha);
        let beta = rubric_id_for(MigrationRolloutStage::Beta);
        let ga = rubric_id_for(MigrationRolloutStage::Ga);
        assert_ne!(alpha, beta);
        assert_ne!(beta, ga);
        assert_ne!(alpha, ga);
        assert!(alpha.starts_with("rubric-alpha-"));
        assert!(ga.starts_with("rubric-ga-"));
    }

    #[test]
    fn rubric_id_is_stable_across_calls() {
        assert_eq!(
            rubric_id_for(MigrationRolloutStage::Beta),
            rubric_id_for(MigrationRolloutStage::Beta)
        );
    }

    // ── AC3: every diagnostic carries the mandated audit fields ────────────────

    #[test]
    fn diagnostics_carry_all_audit_fields() {
        let report = run_rollout_gate_tests();
        let expected_policy = rollout_policy_version();
        for result in &report.fixtures {
            let d = &result.diagnostic;
            assert_eq!(d.policy_version, expected_policy);
            assert!(!d.rubric_id.is_empty());
            assert_eq!(d.signal_vector_hash.len(), 16);
            assert!(!d.decision_output.is_empty());
            assert!(d.replay_cmd.contains(&d.fixture_id));
            assert!(d.replay_cmd.contains("doctor_frankentui"));
            // A non-green decision must name the violated rule; green must not.
            if d.decision_output == "green" {
                assert!(d.violated_rule.is_empty(), "{}", d.fixture_id);
            } else {
                assert!(!d.violated_rule.is_empty(), "{}", d.fixture_id);
            }
        }
    }

    #[test]
    fn signal_vector_hash_distinguishes_fixtures() {
        let report = run_rollout_gate_tests();
        let mut hashes: Vec<&str> = report
            .fixtures
            .iter()
            .map(|r| r.diagnostic.signal_vector_hash.as_str())
            .collect();
        hashes.sort_unstable();
        let unique = {
            let mut h = hashes.clone();
            h.dedup();
            h.len()
        };
        assert_eq!(unique, hashes.len(), "signal vector hashes collide");
    }

    // ── AC2: determinism / replay fidelity ────────────────────────────────────

    #[test]
    fn report_is_byte_identical_across_runs() {
        let a = run_rollout_gate_tests();
        let b = run_rollout_gate_tests();
        assert_eq!(a, b);
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
    }

    #[test]
    fn report_id_and_checksum_are_stable() {
        let a = run_rollout_gate_tests();
        let b = run_rollout_gate_tests();
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
    }

    #[test]
    fn ledger_jsonl_round_trips() {
        let report = run_rollout_gate_tests();
        let jsonl = report.render_ledger_jsonl();
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), report.fixtures.len());
        for (line, result) in lines.iter().zip(&report.fixtures) {
            let parsed: RolloutGateDiagnostic = serde_json::from_str(line).unwrap();
            assert_eq!(parsed, result.diagnostic);
        }
    }

    #[test]
    fn diagnostics_at_least_filters_by_severity() {
        let report = run_rollout_gate_tests();
        let reds = report.diagnostics_at_least(ReleaseTrafficLight::Red);
        assert_eq!(reds.len(), report.red);
        let yellow_and_up = report.diagnostics_at_least(ReleaseTrafficLight::Yellow);
        assert_eq!(yellow_and_up.len(), report.yellow + report.red);
        let all = report.diagnostics_at_least(ReleaseTrafficLight::Green);
        assert_eq!(all.len(), report.total);
    }

    #[test]
    fn traffic_light_worst_is_commutative_and_monotonic() {
        use ReleaseTrafficLight::{Green, Red, Yellow};
        for &a in &[Green, Yellow, Red] {
            for &b in &[Green, Yellow, Red] {
                assert_eq!(a.worst(b), b.worst(a));
                assert_eq!(a.worst(b).rank(), a.rank().max(b.rank()));
            }
        }
    }

    // ── Property tests ────────────────────────────────────────────────────────

    proptest! {
        // AC2: evaluating any fixture twice yields an identical diagnostic.
        #[test]
        fn prop_fixture_evaluation_is_deterministic(idx in 0usize..10) {
            let fixtures = default_rollout_gate_fixtures();
            let f = &fixtures[idx % fixtures.len()];
            let a = evaluate_fixture(f);
            let b = evaluate_fixture(f);
            prop_assert_eq!(&a, &b);
        }

        // AC2: equivalent inputs (same spec, different id only changes the hash
        // and replay command) produce the same verdict and component lights.
        #[test]
        fn prop_equivalent_evidence_yields_same_verdict(
            cert in 0u32..=10_000u32,
            det in 0u32..=10_000u32,
            gaps in 0usize..4usize,
        ) {
            let make = |id: &str| RolloutGateFixture {
                fixture_id: id.to_string(),
                description: "prop".to_string(),
                expected_class: ReleaseTrafficLight::Green,
                target_stage: "alpha".to_string(),
                gate_mode: "enforce".to_string(),
                readiness: ReadinessSpec {
                    certification_bps: cert,
                    corpus_coverage_bps: 3000,
                    reliability_bps: 9700,
                    deterministic_artifacts: 5,
                    benchmark_gate_passed: true,
                    open_blockers: 0,
                    authority: "release-owner".to_string(),
                    emergency_hold: None,
                },
                release: ReleaseSpec {
                    determinism_bps: det,
                    performance_budget_passed: true,
                    unresolved_critical_gaps: gaps,
                    artifact_kinds: vec![
                        "certification".to_string(),
                        "critical-gaps".to_string(),
                        "determinism".to_string(),
                        "performance".to_string(),
                        "readiness".to_string(),
                    ],
                },
                feedback_corpus: FeedbackCorpus::Default,
            };
            let a = evaluate_fixture(&make("prop-a"));
            let b = evaluate_fixture(&make("prop-b"));
            // Same evidence ⇒ same verdicts and lights, even with different ids.
            prop_assert_eq!(&a.observed_class, &b.observed_class);
            prop_assert_eq!(&a.diagnostic.readiness_verdict, &b.diagnostic.readiness_verdict);
            prop_assert_eq!(&a.diagnostic.release_verdict, &b.diagnostic.release_verdict);
            prop_assert_eq!(&a.diagnostic.readiness_light, &b.diagnostic.readiness_light);
            prop_assert_eq!(&a.diagnostic.release_light, &b.diagnostic.release_light);
            // The signal-vector hash, however, separates the two distinct ids.
            prop_assert_ne!(
                &a.diagnostic.signal_vector_hash,
                &b.diagnostic.signal_vector_hash
            );
        }

        // The composite is always the worst of the three component lights.
        #[test]
        fn prop_composite_is_worst_component(idx in 0usize..10) {
            let fixtures = default_rollout_gate_fixtures();
            let f = &fixtures[idx % fixtures.len()];
            let result = evaluate_fixture(f);
            let r = traffic_light_from_label(&result.diagnostic.readiness_light);
            let g = traffic_light_from_label(&result.diagnostic.release_light);
            let fb = traffic_light_from_label(&result.diagnostic.feedback_light);
            let expected = r.worst(g).worst(fb);
            prop_assert_eq!(result.observed_class, expected.label().to_string());
        }

        // A green decision never names a violated rule; a non-green always does.
        #[test]
        fn prop_violated_rule_consistent_with_decision(idx in 0usize..10) {
            let fixtures = default_rollout_gate_fixtures();
            let f = &fixtures[idx % fixtures.len()];
            let d = evaluate_fixture(f).diagnostic;
            if d.decision_output == "green" {
                prop_assert!(d.violated_rule.is_empty());
            } else {
                prop_assert!(!d.violated_rule.is_empty());
            }
        }
    }
}
