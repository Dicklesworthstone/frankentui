//! End-to-end multi-round optimization drill (bd-3bxhj.8.24).
//!
//! This is the producer behind `scripts/doctor_frankentui_multi_round_drill_e2e.sh`.
//! It demonstrates correct progression across the three optimization tiers with
//! proof and rollback discipline preserved, one round per tier:
//!
//! - **Round 1 — low-hanging**: a cheap lever with a measurable gain and preserved
//!   golden outputs.
//! - **Round 2 — algorithmic**: an algorithmic lever, eligible only once Round 1 is
//!   exhausted.
//! - **Round 3 — exotic**: an exotic candidate with heightened safety checks and a
//!   rollback rehearsal, eligible only once Round 2 is exhausted.
//!
//! Each round drives the real kernels: the [`crate::tier_escalator`] enforces tier
//! eligibility (a round may proceed only when every lower tier is exhausted — the
//! transition evidence, AC1); [`crate::baseline_capture`] +
//! [`crate::profile_orchestrator`] produce the baseline/profile artifacts;
//! [`crate::opportunity_scorer`] activates the lever; [`crate::golden_isomorphism`]
//! produces the behavior-preservation proof; [`crate::reprofile_loop`] measures the
//! post-change delta (AC2); and Round 3 additionally rehearses a one-lever rollback
//! ([`crate::one_lever_policy`]). The gate fails closed on an invalid tier jump, a
//! missing proof, or a rollback-readiness failure (AC3).
//!
//! The ledger is **float-free** (numeric terms are fixed-decimal strings via
//! [`fmt6`]), so it derives [`Eq`] and replays byte-identically. The pipeline is
//! materialized for release-candidate promotion gating.

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
use crate::tier_escalator::{
    CandidateState, EligibilityVerdict, OptimizationTier, TierCandidate, run_tier_escalator,
};

/// Schema version for the in-memory multi-round drill report.
pub const MULTI_ROUND_DRILL_SCHEMA_VERSION: &str = "multi-round-drill-v1";

/// Schema version for the materialized multi-round drill pipeline artifacts.
pub const MULTI_ROUND_DRILL_PIPELINE_SCHEMA_VERSION: &str = "multi-round-drill-pipeline-v1";

/// p99 regression epsilon.
const EPS: f64 = 1e-9;

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

// ── Tier progression ───────────────────────────────────────────────────────────

/// The three tiers, in escalation order.
const TIERS: [OptimizationTier; 3] = [
    OptimizationTier::Round1,
    OptimizationTier::Round2,
    OptimizationTier::Round3,
];

/// Build the tier candidate set as it stands at round `active_idx`: every lower
/// tier's lever is already applied (exhausted), the active tier's lever is
/// available, and later tiers stay available (blocked behind the active tier).
fn drill_candidates(active_idx: usize) -> Vec<TierCandidate> {
    TIERS
        .iter()
        .enumerate()
        .map(|(i, &tier)| {
            let id = format!("c.{}", tier.as_str());
            let hot = format!("hot.{}", tier.as_str());
            if i < active_idx {
                // A prior tier: applied -> exhausted (transition evidence).
                TierCandidate::new(id, tier, hot, 5.0, CandidateState::Applied)
            } else if i == active_idx {
                let c = TierCandidate::new(id, tier, hot, 5.0, CandidateState::Available);
                if tier.is_advanced() {
                    c.proven_and_revertible()
                } else {
                    c
                }
            } else {
                let c = TierCandidate::new(id, tier, hot, 4.0, CandidateState::Available);
                if tier.is_advanced() {
                    c.proven_and_revertible()
                } else {
                    c
                }
            }
        })
        .collect()
}

// ── Per-round kernel drivers (reuse the optimization-gauntlet constructions) ──

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

/// Run the baseline stage; returns (baseline_id, clean).
fn round_baseline(round_number: usize) -> (String, bool) {
    let run_id = format!("baseline-r{round_number}");
    let measurements = vec![
        BaselineRawMeasurement::warmup(0, 5.0),
        BaselineRawMeasurement::measurement(1, 1.0, 2.0, 3.0, 1000.0, 1_000_000),
        BaselineRawMeasurement::measurement(2, 1.0, 2.0, 3.0, 1010.0, 1_000_000),
        BaselineRawMeasurement::measurement(3, 1.0, 2.0, 3.0, 990.0, 1_000_000),
        BaselineRawMeasurement::measurement(4, 1.0, 2.0, 3.0, 1005.0, 1_000_000),
        BaselineRawMeasurement::measurement(5, 1.0, 2.0, 3.0, 995.0, 1_000_000),
    ];
    let input = BaselineRunInput::new(run_id.clone(), baseline_scenario(), measurements);
    let report = BaselineCaptureRunner::new(baseline_manifest()).capture(vec![input]);
    (run_id, report.variance_violations.is_empty())
}

/// Run the profile stage; returns (profile_id, top_hotspot_id).
fn round_profile(round_number: usize) -> (String, String) {
    let plan = ProfileOrchestrationPlan::new(
        format!("target-r{round_number}"),
        BenchmarkProfileKey::new("stage-a", "fix-a"),
        42,
        vec![ProfilerInvocation::new(
            ProfileModality::Cpu,
            "perf",
            vec!["record".to_string()],
            "artifacts/cpu.folded",
        )],
    );
    let samples = vec![
        ProfileSample::new(
            ProfileModality::Cpu,
            "render",
            "ui/render.rs:42",
            800.0,
            1200,
        )
        .with_total_weight_hint(1000.0),
        ProfileSample::new(ProfileModality::Cpu, "layout", "ui/layout.rs:7", 300.0, 600),
    ];
    let report = ProfileOrchestrator::new(ProfileOrchestrationConfig::default().with_top_n(5))
        .orchestrate(plan, samples);
    let hotspot = report
        .top_hotspots
        .first()
        .map_or_else(|| "none".to_string(), |h| h.hotspot_id.clone());
    (report.orchestration_id, hotspot)
}

/// Run the score stage for a tier; returns (lever_id, activated, score).
fn round_score(tier: OptimizationTier, hotspot: &str) -> (String, bool, String) {
    let lever_id = format!("lever.{}", tier.as_str());
    // Effort climbs with the tier, but every lever stays above the 2.0 gate.
    let effort = match tier {
        OptimizationTier::Round1 => EffortSize::Small,
        OptimizationTier::Round2 => EffortSize::Medium,
        OptimizationTier::Round3 => EffortSize::Large,
    };
    let lever = OptimizationLever::new(&lever_id, hotspot, "drill lever", 9.0, 0.9, effort)
        .with_evidence([format!("baseline.{}", tier.as_str())]);
    let report = run_opportunity_scoring(
        &format!("drill/score/{}", tier.as_str()),
        &[lever],
        &[],
        &OpportunityConfig::default(),
    );
    let card = report.card(&lever_id);
    (
        lever_id,
        card.is_some_and(|c| c.activated),
        card.map_or_else(|| fmt6(0.0), |c| c.score.clone()),
    )
}

/// Run the isomorphism proof stage; returns (proof_id, behavior_preserved).
fn round_proof(round_number: usize) -> (String, bool) {
    let golden = vec![GoldenOutput::new(
        "fix-a",
        "wl-1",
        vec![GoldenRecord::new("row1", 0).with_floats(vec![1.230001])],
    )];
    let candidate = vec![GoldenOutput::new(
        "fix-a",
        "wl-1",
        vec![GoldenRecord::new("row1", 0).with_floats(vec![1.230004])],
    )];
    let change = GoldenChange::new(format!("chg.r{round_number}"), "base-0", "vectorize")
        .with_invariant(IsomorphismInvariant::FloatingPoint {
            tolerance_decimals: 3,
        });
    let report = GoldenIsomorphismGate::new(GoldenIsomorphismConfig::default())
        .verify(&change, &golden, &candidate);
    (
        report.proof.proof_id.clone(),
        report.promotion_allowed && report.proof.behavior_preserved,
    )
}

fn snapshot(p99: f64, hot: &str) -> ProfileSnapshot {
    ProfileSnapshot {
        p50_us: 32.0,
        p95_us: 70.0,
        p99_us: p99,
        throughput: 11000.0,
        memory_bytes: 4_900_000.0,
        hotspots: vec![HotspotCost {
            hotspot_id: hot.to_string(),
            cost_share: 0.6,
        }],
        stable: true,
    }
}

/// Run the re-profile stage; returns the KERNEL-reported
/// `(p99_before, p99_after, gain_pct)` fixed-decimal strings plus whether the
/// round continued.
///
/// The ledgered values are read from the reprofile_loop round entry — never
/// recomputed from this module's own tier constants — so a kernel math
/// regression shows up in the drill ledger (and fails `gains_measured`)
/// instead of being papered over by locally-derived numbers.
fn round_reprofile(
    tier: OptimizationTier,
    hotspot: &str,
    before: f64,
    after: f64,
) -> (String, String, String, bool) {
    let round = OptimizationRound {
        round_number: 1,
        change_id: format!("chg.{}", tier.as_str()),
        lever_id: format!("lever.{}", tier.as_str()),
        before: snapshot(before, hotspot),
        after: snapshot(after, hotspot),
        backlog: vec![BacklogCandidate {
            candidate_id: format!("cand.{}", tier.as_str()),
            hotspot_id: hotspot.to_string(),
            confidence: 0.85,
            effort: EffortSize::Medium,
        }],
    };
    let report = run_reprofile_loop(
        &format!("drill/reprofile/{}", tier.as_str()),
        &[round],
        &ReprofileConfig::default(),
    );
    match report.round(1) {
        Some(entry) => {
            // Drill convention: positive gain = improvement. The kernel's pct
            // is delta/before with delta = after - before (negative =
            // improvement for latency), so negate the kernel-reported value.
            let kernel_pct = entry.p99_pct.parse::<f64>().unwrap_or(0.0);
            (
                entry.p99_before.clone(),
                entry.p99_after.clone(),
                fmt6(-kernel_pct),
                matches!(entry.verdict, RoundVerdict::Continue),
            )
        }
        None => (fmt6(0.0), fmt6(0.0), fmt6(0.0), false),
    }
}

fn ready_rollback(round_number: usize) -> RollbackPlan {
    RollbackPlan::new(
        format!("rbk.r{round_number}"),
        "git revert <sha>",
        RiskTier::High,
        ["cargo test -p ftui-render --lib".to_string()],
    )
}

/// Validate a round's rollback plan through the one-lever policy; returns
/// (ready, rehearsed).
///
/// Every round's `rollback_ready` is derived from the kernel's card (a
/// complete, validated rollback plan), not asserted as a literal; only the
/// exotic round (Round3) additionally marks the rehearsal flag.
fn round_rollback(tier: OptimizationTier, round_number: usize) -> (bool, bool) {
    let change = OptimizationChange::new(
        format!("chg.r{round_number}"),
        [format!("lever.{}", tier.as_str())],
    )
    .with_rollback(ready_rollback(round_number));
    let report = run_one_lever_policy(&format!("drill/rollback/r{round_number}"), &[change]);
    let card = report.card(&format!("chg.r{round_number}"));
    let ready = card.is_some_and(|c| c.rollback_ready && c.accepted);
    // The rehearsal succeeds when a complete, validated rollback plan is attached.
    (ready, ready && tier == OptimizationTier::Round3)
}

// ── Ledger ───────────────────────────────────────────────────────────────────

/// One round's drill ledger entry (float-free; derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrillRoundEntry {
    /// Ledger schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// The 1-based round number.
    pub round_number: usize,
    /// The tier this round operates in.
    pub tier: OptimizationTier,
    /// The tier's eligibility verdict.
    pub eligibility: String,
    /// Whether every lower tier was exhausted (the transition evidence, AC1).
    pub prior_tiers_exhausted: bool,
    /// Whether the escalator's active tier matched this round's tier.
    pub active_tier_matched: bool,
    /// The baseline artifact id (AC2).
    pub baseline_id: String,
    /// Whether the baseline was clean.
    pub baseline_clean: bool,
    /// The profile artifact id (AC2).
    pub profile_id: String,
    /// The top hotspot the round addressed.
    pub hotspot_id: String,
    /// The activated lever id.
    pub lever_id: String,
    /// Whether the opportunity score activated the lever.
    pub lever_activated: bool,
    /// The opportunity score (fixed-decimal).
    pub score: String,
    /// The behavior-preservation proof id (AC2).
    pub proof_id: String,
    /// Whether behavior was preserved by the proof.
    pub behavior_preserved: bool,
    /// p99 before the change (fixed-decimal).
    pub p99_before: String,
    /// p99 after the change (fixed-decimal).
    pub p99_after: String,
    /// Measured gain percent (fixed-decimal).
    pub gain_pct: String,
    /// Whether the re-profile round continued (a healthy improvement).
    pub reprofile_continued: bool,
    /// Whether a rollback plan is ready (Round 3 rehearsal).
    pub rollback_ready: bool,
    /// Whether the rollback was rehearsed (Round 3).
    pub rollback_rehearsed: bool,
    /// The round outcome tag.
    pub outcome: String,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic single-command replay reference.
    pub reproduction_command: String,
}

fn required_fields_present(line: &DrillRoundEntry) -> bool {
    !line.schema_version.is_empty()
        && !line.run_id.is_empty()
        && !line.eligibility.is_empty()
        && !line.baseline_id.is_empty()
        && !line.profile_id.is_empty()
        && !line.hotspot_id.is_empty()
        && !line.lever_id.is_empty()
        && !line.score.is_empty()
        && !line.proof_id.is_empty()
        && !line.p99_before.is_empty()
        && !line.p99_after.is_empty()
        && !line.gain_pct.is_empty()
        && !line.outcome.is_empty()
        && !line.detail.is_empty()
        && !line.reproduction_command.is_empty()
}

fn render_ledger_jsonl(ledger: &[DrillRoundEntry]) -> String {
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

// ── Engine ───────────────────────────────────────────────────────────────────

/// The deterministic multi-round drill engine.
#[derive(Debug, Clone)]
pub struct MultiRoundDrill {
    run_id: String,
    label: String,
}

impl MultiRoundDrill {
    /// Construct a drill with a deterministic run id derived from its label.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        let run_id = format!(
            "multi-round-drill-{}",
            short_hash(&stable_hash(&format!(
                "{MULTI_ROUND_DRILL_SCHEMA_VERSION}|{label}"
            )))
        );
        Self { run_id, label }
    }

    /// The deterministic run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    fn run_round(&self, active_idx: usize) -> DrillRoundEntry {
        let tier = TIERS[active_idx];
        let round_number = active_idx + 1;

        // Tier eligibility + transition evidence (AC1).
        let escalator = run_tier_escalator(
            &format!("{}/round{round_number}", self.label),
            &drill_candidates(active_idx),
        );
        let eval = escalator.tier(tier);
        let eligibility = eval.map_or_else(
            || "unknown".to_string(),
            |e| e.eligibility.as_str().to_string(),
        );
        let eligible = eval.is_some_and(|e| matches!(e.eligibility, EligibilityVerdict::Eligible));
        let prior_tiers_exhausted = eval.is_some_and(|e| e.prior_tiers_exhausted);
        let active_tier_matched = escalator.summary.active_tier == Some(tier);

        // Baseline + profile artifacts (AC2).
        let (baseline_id, baseline_clean) = round_baseline(round_number);
        let (profile_id, hotspot_id) = round_profile(round_number);

        // Score + proof + re-profile delta (AC2).
        let (lever_id, lever_activated, score) = round_score(tier, &hotspot_id);
        let (proof_id, behavior_preserved) = round_proof(round_number);
        // Each tier yields a measurable gain; later tiers shave more off the tail.
        let (before, after) = match tier {
            OptimizationTier::Round1 => (120.0, 96.0),
            OptimizationTier::Round2 => (96.0, 74.0),
            OptimizationTier::Round3 => (74.0, 60.0),
        };
        let (p99_before, p99_after, gain_pct, reprofile_continued) =
            round_reprofile(tier, &hotspot_id, before, after);

        // Every round's rollback readiness comes from the one-lever kernel;
        // only Round 3 additionally rehearses it.
        let (rollback_ready, rollback_rehearsed) = round_rollback(tier, round_number);

        let outcome = if tier == OptimizationTier::Round3 {
            "promoted_with_rollback_rehearsal"
        } else {
            "promoted"
        };

        // Build the detail string before moving owned fields into the struct.
        let detail = format!(
            "round {round_number} [{}] eligible={eligible} prior_exhausted={prior_tiers_exhausted} gain={gain_pct}% proof={proof_id} rollback_ready={rollback_ready}",
            tier.as_str()
        );

        DrillRoundEntry {
            schema_version: MULTI_ROUND_DRILL_SCHEMA_VERSION.to_string(),
            run_id: self.run_id.clone(),
            round_number,
            tier,
            eligibility,
            prior_tiers_exhausted,
            active_tier_matched: active_tier_matched && eligible,
            baseline_id,
            baseline_clean,
            profile_id,
            hotspot_id,
            lever_id,
            lever_activated,
            score,
            proof_id,
            behavior_preserved,
            p99_before,
            p99_after,
            gain_pct,
            reprofile_continued,
            rollback_ready,
            rollback_rehearsed,
            outcome: outcome.to_string(),
            detail,
            reproduction_command: format!(
                "cargo run -p doctor_frankentui -- multi-round-drill --label '{}' # run {} round {round_number}",
                self.label, self.run_id
            ),
        }
    }

    /// Run all three rounds and assemble the report.
    #[must_use]
    pub fn run(&self) -> MultiRoundDrillReport {
        let ledger: Vec<DrillRoundEntry> = (0..TIERS.len()).map(|i| self.run_round(i)).collect();

        let evidence_checksum = sha256_hex(render_ledger_jsonl(&ledger).as_bytes());
        let report_id = format!(
            "multi-round-drill-report-{}",
            short_hash(&stable_hash(&format!(
                "{}|{evidence_checksum}",
                self.run_id
            )))
        );
        let summary = self.summarize(&ledger, &report_id, &evidence_checksum);
        let gate_passes = summary.gate_passes;
        let exported_json_stats = export_json_stats(&report_id, &summary, &ledger);
        let replay_command = format!(
            "cargo run -p doctor_frankentui -- multi-round-drill --label '{}' # run {}",
            self.label, self.run_id
        );

        MultiRoundDrillReport {
            schema_version: MULTI_ROUND_DRILL_SCHEMA_VERSION.to_string(),
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

    fn summarize(
        &self,
        ledger: &[DrillRoundEntry],
        report_id: &str,
        evidence_checksum: &str,
    ) -> MultiRoundDrillSummary {
        let required_fields_complete = ledger.iter().all(required_fields_present);
        // AC1: tier progression valid — Round1 eligible at rank 0; every later round
        // is eligible AND had its prior tiers exhausted AND matched the active tier.
        let tier_progression_valid = ledger.iter().enumerate().all(|(i, l)| {
            l.eligibility == "eligible"
                && l.active_tier_matched
                && (i == 0 || l.prior_tiers_exhausted)
                && l.tier == TIERS[i]
        });
        // AC2: every round carries baseline/profile/proof artifacts + a positive
        // re-profile delta, and the lever activated.
        let artifacts_complete = ledger.iter().all(|l| {
            !l.baseline_id.is_empty()
                && l.baseline_clean
                && !l.profile_id.is_empty()
                && !l.proof_id.is_empty()
                && l.lever_activated
        });
        // AC3: every round preserved behavior (proof) with a measured gain.
        let proofs_present = ledger
            .iter()
            .all(|l| l.behavior_preserved && l.reprofile_continued);
        let gains_measured = ledger
            .iter()
            .all(|l| l.gain_pct.parse::<f64>().unwrap_or(0.0) > EPS);
        // AC3: the exotic round rehearsed a ready rollback.
        let rollback_rehearsed = ledger
            .iter()
            .filter(|l| l.tier == OptimizationTier::Round3)
            .all(|l| l.rollback_ready && l.rollback_rehearsed);
        let rounds_complete = ledger.len() == TIERS.len();
        let gate_passes = required_fields_complete
            && tier_progression_valid
            && artifacts_complete
            && proofs_present
            && gains_measured
            && rollback_rehearsed
            && rounds_complete;

        MultiRoundDrillSummary {
            schema_version: MULTI_ROUND_DRILL_SCHEMA_VERSION.to_string(),
            report_id: report_id.to_string(),
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum: evidence_checksum.to_string(),
            total_rounds: ledger.len(),
            required_fields_complete,
            tier_progression_valid,
            artifacts_complete,
            proofs_present,
            gains_measured,
            rollback_rehearsed,
            rounds_complete,
            gate_passes,
            replay_command: format!(
                "cargo run -p doctor_frankentui -- multi-round-drill --label '{}' # run {}",
                self.label, self.run_id
            ),
        }
    }
}

/// Run the multi-round drill with the given label.
#[must_use]
pub fn run_multi_round_drill(label: &str) -> MultiRoundDrillReport {
    MultiRoundDrill::new(label).run()
}

// ── Report + summary + stats ─────────────────────────────────────────────────

/// Machine-readable summary of one multi-round drill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiRoundDrillSummary {
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
    /// Total rounds.
    pub total_rounds: usize,
    /// Whether every ledger line has all mandated fields.
    pub required_fields_complete: bool,
    /// Whether tier eligibility + progression is valid with transition evidence (AC1).
    pub tier_progression_valid: bool,
    /// Whether every round carries baseline/profile/proof artifacts (AC2).
    pub artifacts_complete: bool,
    /// Whether every round preserved behavior + continued (AC3).
    pub proofs_present: bool,
    /// Whether every round measured a positive gain (AC2).
    pub gains_measured: bool,
    /// Whether the exotic round rehearsed a ready rollback (AC3).
    pub rollback_rehearsed: bool,
    /// Whether all three rounds ran.
    pub rounds_complete: bool,
    /// Whether the fail-closed gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiRoundDrillStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The in-memory multi-round drill report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiRoundDrillReport {
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
    /// The per-round evidence ledger (float-free).
    pub ledger: Vec<DrillRoundEntry>,
    /// Aggregate summary.
    pub summary: MultiRoundDrillSummary,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: MultiRoundDrillStatsArtifact,
}

impl MultiRoundDrillReport {
    /// Render the evidence ledger as JSONL (one entry per line).
    #[must_use]
    pub fn render_ledger_jsonl(&self) -> String {
        render_ledger_jsonl(&self.ledger)
    }
}

fn export_json_stats(
    report_id: &str,
    summary: &MultiRoundDrillSummary,
    ledger: &[DrillRoundEntry],
) -> MultiRoundDrillStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        summary: &'a MultiRoundDrillSummary,
        ledger: &'a [DrillRoundEntry],
    }
    let content = serde_json::to_string_pretty(&Export {
        schema_version: MULTI_ROUND_DRILL_SCHEMA_VERSION,
        report_id,
        summary,
        ledger,
    })
    .unwrap_or_else(|error| error.to_string());
    MultiRoundDrillStatsArtifact {
        path: format!("{report_id}/multi_round_drill_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

// ── Pipeline (materialized artifacts) ────────────────────────────────────────

/// Configuration for the materialized multi-round drill pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct MultiRoundDrillPipelineConfig {
    /// Run directory name under the run-root.
    pub run_name: String,
    /// Run label used for deterministic ids.
    pub label: String,
}

impl Default for MultiRoundDrillPipelineConfig {
    fn default() -> Self {
        Self {
            run_name: "multi_round_drill".to_string(),
            label: "multi-round-drill/e2e".to_string(),
        }
    }
}

/// A materialized pipeline artifact (path + integrity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiRoundDrillArtifact {
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
pub struct MultiRoundDrillPipelineOutcome {
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
    pub summary: MultiRoundDrillSummary,
    /// All generated artifacts (with integrity hashes).
    pub artifacts: Vec<MultiRoundDrillArtifact>,
}

fn artifact_of(file: &str, content: &str) -> MultiRoundDrillArtifact {
    MultiRoundDrillArtifact {
        name: file
            .trim_end_matches(".json")
            .trim_end_matches(".jsonl")
            .to_string(),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// Run the multi-round drill and materialize the full evidence pipeline under
/// `run_root/<run_name>/`.
///
/// # Errors
/// Returns an error if a run directory or artifact cannot be created/serialized.
pub fn run_multi_round_drill_pipeline(
    run_root: &Path,
    config: &MultiRoundDrillPipelineConfig,
) -> crate::error::Result<MultiRoundDrillPipelineOutcome> {
    let report = MultiRoundDrill::new(config.label.as_str()).run();

    let ledger_content = report.render_ledger_jsonl();
    let stats_content = report.exported_json_stats.content.clone();
    let summary_content = serde_json::to_string_pretty(&report.summary)?;

    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let ledger_file = "evidence_ledger.jsonl";
    let stats_file = "multi_round_drill_stats.json";
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
        artifacts: &'a [MultiRoundDrillArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: MULTI_ROUND_DRILL_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        gate_passes: report.gate_passes,
        artifacts: &artifacts,
    })?;
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(MultiRoundDrillPipelineOutcome {
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

/// CLI arguments for the `multi-round-drill` command.
#[derive(Debug, clap::Args)]
pub struct MultiRoundDrillArgs {
    /// Run-root directory; artifacts land under `<run-root>/<run-name>/`.
    #[arg(
        long = "run-root",
        default_value = "/tmp/doctor_frankentui/multi_round_drill"
    )]
    pub run_root: PathBuf,

    /// Run directory name.
    #[arg(long = "run-name", default_value = "multi_round_drill")]
    pub run_name: String,

    /// Run label used for deterministic ids.
    #[arg(long = "label", default_value = "multi-round-drill/e2e")]
    pub label: String,
}

/// Run the `multi-round-drill` command: materialize the pipeline and apply the
/// fail-closed gate.
///
/// # Errors
/// Returns [`crate::error::DoctorError::Exit`] with a non-zero code when the gate
/// fails (an invalid tier jump, a missing proof/artifact, an unmeasured gain, or a
/// rollback-readiness failure), or an I/O error if artifacts cannot be materialized.
pub fn run_multi_round_drill_command(args: MultiRoundDrillArgs) -> crate::error::Result<()> {
    let config = MultiRoundDrillPipelineConfig {
        run_name: args.run_name,
        label: args.label,
    };
    let outcome = run_multi_round_drill_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("multi-round optimization drill"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!(
            "rounds: {} | tier progression: {} | artifacts: {}",
            summary.total_rounds, summary.tier_progression_valid, summary.artifacts_complete
        ));
        ui.info(&format!(
            "proofs: {} | gains: {} | rollback rehearsed: {}",
            summary.proofs_present, summary.gains_measured, summary.rollback_rehearsed
        ));
        if summary.gate_passes {
            ui.success("multi-round-drill gate PASSED");
        } else {
            ui.error("multi-round-drill gate FAILED");
        }
    }

    if summary.gate_passes {
        Ok(())
    } else {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "multi-round-drill gate failed: tier_progression_valid={}, artifacts_complete={}, proofs_present={}, gains_measured={}, rollback_rehearsed={}",
                summary.tier_progression_valid,
                summary.artifacts_complete,
                summary.proofs_present,
                summary.gains_measured,
                summary.rollback_rehearsed
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn round(report: &MultiRoundDrillReport, n: usize) -> &DrillRoundEntry {
        report
            .ledger
            .iter()
            .find(|l| l.round_number == n)
            .expect("round present")
    }

    #[test]
    fn reprofile_values_flow_through_the_kernel_report() {
        // The ledgered strings are the reprofile_loop entry's own fmt6
        // renderings (with the drill's positive-gain sign convention), not
        // locally recomputed floats — a kernel math regression must surface
        // in the drill ledger.
        let (before, after, gain, continued) =
            round_reprofile(OptimizationTier::Round1, "hot.render", 120.0, 96.0);
        assert!(continued);
        assert_eq!(before, "120.000000");
        assert_eq!(after, "96.000000");
        assert_eq!(gain, "20.000000");
        // Kernel-backed rollback readiness for every tier, rehearsal only on 3.
        assert_eq!(round_rollback(OptimizationTier::Round1, 1), (true, false));
        assert_eq!(round_rollback(OptimizationTier::Round3, 3), (true, true));
    }

    #[test]
    fn drill_gate_passes_with_three_rounds() {
        let report = run_multi_round_drill("mrd/test");
        assert!(report.gate_passes, "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_rounds, 3);
        assert!(report.summary.tier_progression_valid);
        assert!(report.summary.artifacts_complete);
        assert!(report.summary.proofs_present);
        assert!(report.summary.gains_measured);
        assert!(report.summary.rollback_rehearsed);
        assert!(report.ledger.iter().all(required_fields_present));
    }

    #[test]
    fn rounds_progress_through_the_three_tiers() {
        let report = run_multi_round_drill("mrd/test");
        assert_eq!(round(&report, 1).tier, OptimizationTier::Round1);
        assert_eq!(round(&report, 2).tier, OptimizationTier::Round2);
        assert_eq!(round(&report, 3).tier, OptimizationTier::Round3);
    }

    #[test]
    fn each_round_is_eligible_and_matches_active_tier() {
        let report = run_multi_round_drill("mrd/test");
        for n in 1..=3 {
            let r = round(&report, n);
            assert_eq!(r.eligibility, "eligible", "round {n}");
            assert!(r.active_tier_matched, "round {n} active tier");
        }
    }

    #[test]
    fn later_rounds_require_prior_tiers_exhausted() {
        // The transition evidence: Round 2 and Round 3 only proceed once the lower
        // tiers are exhausted.
        let report = run_multi_round_drill("mrd/test");
        assert!(round(&report, 2).prior_tiers_exhausted);
        assert!(round(&report, 3).prior_tiers_exhausted);
    }

    #[test]
    fn every_round_carries_baseline_profile_proof_and_gain() {
        let report = run_multi_round_drill("mrd/test");
        for n in 1..=3 {
            let r = round(&report, n);
            assert!(
                r.baseline_clean && !r.baseline_id.is_empty(),
                "round {n} baseline"
            );
            assert!(!r.profile_id.is_empty(), "round {n} profile");
            assert!(
                r.behavior_preserved && !r.proof_id.is_empty(),
                "round {n} proof"
            );
            assert!(r.lever_activated, "round {n} lever");
            let gain: f64 = r.gain_pct.parse().unwrap();
            assert!(gain > 0.0, "round {n} gain {gain}");
            assert!(r.reprofile_continued, "round {n} reprofile");
        }
    }

    #[test]
    fn round_three_rehearses_rollback() {
        let report = run_multi_round_drill("mrd/test");
        let r3 = round(&report, 3);
        assert_eq!(r3.outcome, "promoted_with_rollback_rehearsal");
        assert!(r3.rollback_ready);
        assert!(r3.rollback_rehearsed);
    }

    #[test]
    fn report_is_deterministic_and_replay_identical() {
        let a = run_multi_round_drill("mrd/test");
        let b = run_multi_round_drill("mrd/test");
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.ledger, b.ledger);
        assert_eq!(a.render_ledger_jsonl(), b.render_ledger_jsonl());
    }

    #[test]
    fn pipeline_materializes_consistent_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let outcome =
            run_multi_round_drill_pipeline(dir.path(), &MultiRoundDrillPipelineConfig::default())
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
        let report = run_multi_round_drill("mrd/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}") {
            let first = run_multi_round_drill(&label);
            let second = run_multi_round_drill(&label);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.ledger, &second.ledger);
        }

        #[test]
        fn prop_gate_always_passes(label in "[a-z]{1,8}") {
            let report = run_multi_round_drill(&label);
            prop_assert!(report.gate_passes);
            prop_assert!(report.summary.tier_progression_valid);
        }
    }
}
