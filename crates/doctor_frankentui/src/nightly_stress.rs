//! E2E nightly stress pipeline (bd-3bxhj.8.9).
//!
//! This is the producer behind `scripts/doctor_frankentui_nightly_stress_e2e.sh`.
//! It validates the full optimization-aware evaluation flow over a mixed corpus
//! with robust per-fixture logging, **resume/retry**, and **behavior-preservation
//! proofs**:
//!
//! - **Resume/retry + determinism metadata + lineage** (AC1): the run carries a
//!   determinism fingerprint and per-fixture baseline/profile lineage ids; a
//!   resume run (given a prior checkpoint with a matching fingerprint) skips the
//!   already-completed fixtures, re-using their lineage byte-for-byte rather than
//!   re-running them.
//! - **Per-fixture lifecycle JSONL** (AC2): each fixture records its stage
//!   timeline (acquire → baseline → profile → score → optimize → verify) with
//!   stage timings, a hotspot table, the score decision (from the real
//!   [`crate::opportunity_scorer`]), and a terminal outcome class.
//! - **Artifact completeness + replay readiness + behavior-preservation proof**
//!   (AC3): every optimized path carries a golden-output isomorphism proof (from
//!   [`crate::golden_isomorphism`]); a behavior regression is detected, logged,
//!   and refused promotion (never silently optimized).
//!
//! The ledger is **float-free** (numeric terms are fixed-decimal strings via
//! [`fmt6`]), so it derives [`Eq`] and replays byte-identically. The pipeline is
//! materialized to disk (ledger + checkpoint + stats + summary + manifest) so the
//! E2E gate can drive resume and validate completeness.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::golden_isomorphism::{
    GoldenIsomorphismConfig, GoldenIsomorphismGate, GoldenOutput, GoldenRecord,
    IsomorphismInvariant, OptimizationChange as GoldenChange,
};
use crate::opportunity_scorer::{OpportunityConfig, OptimizationLever, run_opportunity_scoring};
use crate::recommendation_contract::EffortSize;

/// Schema version for the in-memory nightly-stress report.
pub const NIGHTLY_STRESS_SCHEMA_VERSION: &str = "nightly-stress-v1";

/// Schema version for the materialized nightly-stress pipeline artifacts.
pub const NIGHTLY_STRESS_PIPELINE_SCHEMA_VERSION: &str = "nightly-stress-pipeline-v1";

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

/// A small deterministic per-(fixture, stage) jitter in the range `[0, 49]`,
/// derived from a stable hash so stage timings are varied yet reproducible.
fn jitter(seed: &str) -> u64 {
    let digest = stable_hash(seed);
    let prefix: String = digest.chars().take(4).collect();
    u64::from_str_radix(&prefix, 16).unwrap_or(0) % 50
}

// ── Lifecycle vocabulary ─────────────────────────────────────────────────────

/// A stage in a fixture's nightly-stress lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleStage {
    /// Acquire the fixture from the corpus.
    Acquire,
    /// Capture the baseline measurement.
    Baseline,
    /// Profile + extract hotspots.
    Profile,
    /// Score the optimization lever.
    Score,
    /// Apply the optimization.
    Optimize,
    /// Verify behavior preservation.
    Verify,
    /// Short-circuited from a prior run's checkpoint.
    Resumed,
}

impl LifecycleStage {
    /// The full forward lifecycle, in order.
    pub const FULL: [LifecycleStage; 6] = [
        Self::Acquire,
        Self::Baseline,
        Self::Profile,
        Self::Score,
        Self::Optimize,
        Self::Verify,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Acquire => "acquire",
            Self::Baseline => "baseline",
            Self::Profile => "profile",
            Self::Score => "score",
            Self::Optimize => "optimize",
            Self::Verify => "verify",
            Self::Resumed => "resumed",
        }
    }

    /// The base stage duration (microseconds) before per-fixture jitter.
    fn base_duration_us(self) -> f64 {
        match self {
            Self::Acquire => 5.0,
            Self::Baseline => 120.0,
            Self::Profile => 200.0,
            Self::Score => 30.0,
            Self::Optimize => 150.0,
            Self::Verify => 80.0,
            Self::Resumed => 0.0,
        }
    }
}

/// The terminal outcome class of a fixture's nightly-stress lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StressOutcome {
    /// Score cleared the gate and the optimized output preserved behavior.
    Optimized,
    /// Score below the opportunity gate; no optimization attempted.
    BelowThreshold,
    /// The optimized output failed the isomorphism proof; refused promotion.
    BehaviorRegression,
    /// Skipped via a prior run's checkpoint.
    Resumed,
}

impl StressOutcome {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Optimized => "optimized",
            Self::BelowThreshold => "below_threshold",
            Self::BehaviorRegression => "behavior_regression",
            Self::Resumed => "resumed",
        }
    }

    /// Whether this outcome is a detected stress failure.
    #[must_use]
    pub fn is_failure(self) -> bool {
        matches!(self, Self::BehaviorRegression)
    }
}

// ── Fixture input ──────────────────────────────────────────────────────────────

/// One nightly-stress fixture.
#[derive(Debug, Clone, PartialEq)]
pub struct StressFixture {
    /// Stable fixture id.
    pub fixture_id: String,
    /// The pipeline stage id.
    pub stage_id: String,
    /// The hotspot the lever addresses.
    pub hotspot_id: String,
    /// Lever impact (for the opportunity score).
    pub impact: f64,
    /// Lever confidence (for the opportunity score).
    pub confidence: f64,
    /// Lever effort (for the opportunity score).
    pub effort: EffortSize,
    /// Whether the optimized candidate preserves the golden output.
    pub behavior_preserved: bool,
    /// Whether the change carries a complete rollback plan.
    pub has_rollback: bool,
}

/// A representative mixed stress corpus: optimized (behavior-preserving) wins, a
/// below-threshold fixture, and a behavior regression that must be refused.
#[must_use]
pub fn default_stress_fixtures() -> Vec<StressFixture> {
    vec![
        StressFixture {
            fixture_id: "sf.render".to_string(),
            stage_id: "render".to_string(),
            hotspot_id: "hot.render".to_string(),
            impact: 9.0,
            confidence: 0.9,
            effort: EffortSize::Small,
            behavior_preserved: true,
            has_rollback: true,
        },
        StressFixture {
            fixture_id: "sf.layout".to_string(),
            stage_id: "layout".to_string(),
            hotspot_id: "hot.layout".to_string(),
            impact: 8.0,
            confidence: 0.85,
            effort: EffortSize::Small,
            behavior_preserved: true,
            has_rollback: true,
        },
        // Below threshold: a weak lever (3 * 0.4 / 4 = 0.3 < 2.0) is not activated.
        StressFixture {
            fixture_id: "sf.text".to_string(),
            stage_id: "text".to_string(),
            hotspot_id: "hot.text".to_string(),
            impact: 3.0,
            confidence: 0.4,
            effort: EffortSize::Large,
            behavior_preserved: true,
            has_rollback: true,
        },
        // Behavior regression: a strong lever activates, but the optimized output
        // changes behavior -> the isomorphism proof fails and promotion is refused.
        StressFixture {
            fixture_id: "sf.alloc".to_string(),
            stage_id: "alloc".to_string(),
            hotspot_id: "hot.alloc".to_string(),
            impact: 9.0,
            confidence: 0.9,
            effort: EffortSize::Small,
            behavior_preserved: false,
            has_rollback: true,
        },
        StressFixture {
            fixture_id: "sf.vfx".to_string(),
            stage_id: "vfx".to_string(),
            hotspot_id: "hot.vfx".to_string(),
            impact: 7.0,
            confidence: 0.8,
            effort: EffortSize::Medium,
            behavior_preserved: true,
            has_rollback: true,
        },
    ]
}

// ── Ledger ───────────────────────────────────────────────────────────────────

/// One stage timing entry (fixed-decimal duration).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageTiming {
    /// The lifecycle stage.
    pub stage: LifecycleStage,
    /// The stage duration in microseconds (fixed-decimal).
    pub duration_us: String,
}

/// One hotspot-table row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotspotRow {
    /// The hotspot id.
    pub hotspot_id: String,
    /// The hotspot rank (1-based).
    pub rank: usize,
    /// The hotspot cost share (fixed-decimal).
    pub cost_share: String,
}

/// One nightly-stress per-fixture ledger line (float-free; derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NightlyStressLedgerEntry {
    /// Ledger schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// The fixture id.
    pub fixture_id: String,
    /// The pipeline stage id.
    pub stage_id: String,
    /// Whether this fixture was resumed from a prior checkpoint.
    pub resumed: bool,
    /// Baseline lineage id.
    pub baseline_id: String,
    /// Profile lineage id.
    pub profile_id: String,
    /// The per-fixture stage timeline with timings (AC2).
    pub stage_timings: Vec<StageTiming>,
    /// The hotspot table (AC2).
    pub hotspot_table: Vec<HotspotRow>,
    /// Whether the opportunity score activated the lever (AC2 score decision).
    pub score_activated: bool,
    /// The opportunity score (fixed-decimal).
    pub score_value: String,
    /// The score decision tag (`activated` / `blocked` / `resumed`).
    pub score_decision: String,
    /// The terminal outcome class (AC2).
    pub outcome_class: StressOutcome,
    /// The behavior-preservation proof id, or `"n/a"` (AC3).
    pub proof_id: String,
    /// Whether a behavior-preservation proof is available (AC3).
    pub proof_available: bool,
    /// Whether the optimized output preserved behavior.
    pub behavior_preserved: bool,
    /// Whether the optimized path is replay-ready (AC3).
    pub replay_ready: bool,
    /// Whether the change carries a complete rollback plan.
    pub rollback_ready: bool,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic single-command replay reference.
    pub reproduction_command: String,
}

fn required_fields_present(line: &NightlyStressLedgerEntry) -> bool {
    !line.schema_version.is_empty()
        && !line.run_id.is_empty()
        && !line.fixture_id.is_empty()
        && !line.stage_id.is_empty()
        && !line.baseline_id.is_empty()
        && !line.profile_id.is_empty()
        && !line.stage_timings.is_empty()
        && !line.score_value.is_empty()
        && !line.score_decision.is_empty()
        && !line.proof_id.is_empty()
        && !line.detail.is_empty()
        && !line.reproduction_command.is_empty()
}

fn render_ledger_jsonl(ledger: &[NightlyStressLedgerEntry]) -> String {
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

// ── Checkpoint (resume/retry) ──────────────────────────────────────────────────

/// A resume checkpoint: the determinism fingerprint plus the fixtures already
/// completed by a prior run (AC1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NightlyStressCheckpoint {
    /// Checkpoint schema version.
    pub schema_version: String,
    /// The determinism fingerprint; a resume is honored only when it matches.
    pub determinism_fingerprint: String,
    /// The fixture ids already completed.
    pub completed_fixture_ids: Vec<String>,
}

// ── Lineage helpers ────────────────────────────────────────────────────────────

fn baseline_id(fixture_id: &str) -> String {
    format!("baseline-{fixture_id}")
}

fn profile_id(fixture_id: &str) -> String {
    format!("profile-{fixture_id}")
}

fn hotspot_table(fixture: &StressFixture) -> Vec<HotspotRow> {
    // A small deterministic two-row table dominated by the lever's hotspot.
    vec![
        HotspotRow {
            hotspot_id: fixture.hotspot_id.clone(),
            rank: 1,
            cost_share: fmt6(0.62),
        },
        HotspotRow {
            hotspot_id: format!("{}.tail", fixture.hotspot_id),
            rank: 2,
            cost_share: fmt6(0.21),
        },
    ]
}

fn full_stage_timings(fixture_id: &str) -> Vec<StageTiming> {
    LifecycleStage::FULL
        .iter()
        .map(|stage| {
            let seed = format!("{fixture_id}|{}", stage.as_str());
            let duration = stage.base_duration_us() + jitter(&seed) as f64;
            StageTiming {
                stage: *stage,
                duration_us: fmt6(duration),
            }
        })
        .collect()
}

// ── Per-fixture evaluation ─────────────────────────────────────────────────────

fn lever_id(fixture_id: &str) -> String {
    format!("lever.{fixture_id}")
}

fn evaluate_fixture(
    run_id: &str,
    label: &str,
    fixture: &StressFixture,
    resumed: bool,
    score_activated: bool,
    score_value: &str,
) -> NightlyStressLedgerEntry {
    let reproduction_command = format!(
        "cargo run -p doctor_frankentui -- nightly-stress --label '{label}' # run {run_id} fixture {}",
        fixture.fixture_id
    );
    let base = baseline_id(&fixture.fixture_id);
    let prof = profile_id(&fixture.fixture_id);

    if resumed {
        let detail = format!(
            "fixture {} resumed from checkpoint (lineage {base}/{prof} preserved)",
            fixture.fixture_id
        );
        return NightlyStressLedgerEntry {
            schema_version: NIGHTLY_STRESS_SCHEMA_VERSION.to_string(),
            run_id: run_id.to_string(),
            fixture_id: fixture.fixture_id.clone(),
            stage_id: fixture.stage_id.clone(),
            resumed: true,
            baseline_id: base,
            profile_id: prof,
            stage_timings: vec![StageTiming {
                stage: LifecycleStage::Resumed,
                duration_us: fmt6(0.0),
            }],
            hotspot_table: hotspot_table(fixture),
            score_activated,
            score_value: score_value.to_string(),
            score_decision: "resumed".to_string(),
            outcome_class: StressOutcome::Resumed,
            proof_id: "resumed-checkpoint".to_string(),
            proof_available: true,
            behavior_preserved: true,
            replay_ready: true,
            rollback_ready: fixture.has_rollback,
            detail,
            reproduction_command,
        };
    }

    // Below-threshold fixtures stop after the score stage (no optimize/verify).
    if !score_activated {
        let mut timings = full_stage_timings(&fixture.fixture_id);
        timings.truncate(4); // acquire, baseline, profile, score
        return NightlyStressLedgerEntry {
            schema_version: NIGHTLY_STRESS_SCHEMA_VERSION.to_string(),
            run_id: run_id.to_string(),
            fixture_id: fixture.fixture_id.clone(),
            stage_id: fixture.stage_id.clone(),
            resumed: false,
            baseline_id: base,
            profile_id: prof,
            stage_timings: timings,
            hotspot_table: hotspot_table(fixture),
            score_activated,
            score_value: score_value.to_string(),
            score_decision: "blocked".to_string(),
            outcome_class: StressOutcome::BelowThreshold,
            proof_id: "n/a".to_string(),
            proof_available: false,
            behavior_preserved: true,
            replay_ready: false,
            rollback_ready: fixture.has_rollback,
            detail: format!(
                "fixture {} scored below the opportunity gate; no optimization attempted",
                fixture.fixture_id
            ),
            reproduction_command,
        };
    }

    // Activated: optimize + verify behavior preservation via the isomorphism oracle.
    let golden = vec![GoldenOutput::new(
        &fixture.fixture_id,
        "wl-1",
        vec![GoldenRecord::new("row1", 0).with_floats(vec![1.230001])],
    )];
    let (candidate, change) = if fixture.behavior_preserved {
        (
            vec![GoldenOutput::new(
                &fixture.fixture_id,
                "wl-1",
                vec![GoldenRecord::new("row1", 0).with_floats(vec![1.230004])],
            )],
            GoldenChange::new(format!("chg.{}", fixture.fixture_id), "base-0", "vectorize")
                .with_invariant(IsomorphismInvariant::FloatingPoint {
                    tolerance_decimals: 3,
                }),
        )
    } else {
        (
            vec![GoldenOutput::new(
                &fixture.fixture_id,
                "wl-1",
                vec![GoldenRecord::new("row1", 0).with_tokens(vec!["CHANGED".to_string()])],
            )],
            GoldenChange::new(format!("chg.{}", fixture.fixture_id), "base-0", "reorder"),
        )
    };
    let report = GoldenIsomorphismGate::new(GoldenIsomorphismConfig::default())
        .verify(&change, &golden, &candidate);
    let preserved = report.promotion_allowed && report.proof.behavior_preserved;
    let outcome_class = if preserved {
        StressOutcome::Optimized
    } else {
        StressOutcome::BehaviorRegression
    };

    NightlyStressLedgerEntry {
        schema_version: NIGHTLY_STRESS_SCHEMA_VERSION.to_string(),
        run_id: run_id.to_string(),
        fixture_id: fixture.fixture_id.clone(),
        stage_id: fixture.stage_id.clone(),
        resumed: false,
        baseline_id: base,
        profile_id: prof,
        stage_timings: full_stage_timings(&fixture.fixture_id),
        hotspot_table: hotspot_table(fixture),
        score_activated,
        score_value: score_value.to_string(),
        score_decision: "activated".to_string(),
        outcome_class,
        proof_id: report.proof.proof_id.clone(),
        proof_available: true,
        behavior_preserved: preserved,
        replay_ready: preserved,
        rollback_ready: fixture.has_rollback,
        detail: format!(
            "fixture {} optimized: behavior_preserved={} promotion_allowed={} (proof {})",
            fixture.fixture_id,
            report.proof.behavior_preserved,
            report.promotion_allowed,
            report.proof.proof_id
        ),
        reproduction_command,
    }
}

// ── Engine ───────────────────────────────────────────────────────────────────

/// The deterministic nightly-stress engine.
#[derive(Debug, Clone)]
pub struct NightlyStress {
    run_id: String,
    label: String,
}

impl NightlyStress {
    /// Construct an engine with a deterministic run id derived from its label.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        let run_id = format!(
            "nightly-stress-{}",
            short_hash(&stable_hash(&format!(
                "{NIGHTLY_STRESS_SCHEMA_VERSION}|{label}"
            )))
        );
        Self { run_id, label }
    }

    /// The deterministic run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// The determinism fingerprint over the schema + the full canonical fixture
    /// definitions; a resume is honored only when this matches the checkpoint.
    ///
    /// The fingerprint covers every field of every fixture (not just the ids),
    /// so a checkpoint is discarded — and everything re-runs — whenever any
    /// fixture's inputs change between runs.
    #[must_use]
    pub fn determinism_fingerprint(fixtures: &[StressFixture]) -> String {
        let mut canon: Vec<String> = fixtures
            .iter()
            .map(|f| {
                format!(
                    "{}|{}|{}|{}|{}|{:?}|{}|{}",
                    f.fixture_id,
                    f.stage_id,
                    f.hotspot_id,
                    fmt6(f.impact),
                    fmt6(f.confidence),
                    f.effort,
                    f.behavior_preserved,
                    f.has_rollback
                )
            })
            .collect();
        canon.sort_unstable();
        short_hash(&stable_hash(&format!(
            "{NIGHTLY_STRESS_SCHEMA_VERSION}|{}",
            canon.join(",")
        )))
    }

    /// Run the nightly stress over `fixtures`, optionally resuming from a prior
    /// checkpoint (skipping fixtures it already completed).
    #[must_use]
    pub fn run(
        &self,
        fixtures: &[StressFixture],
        resume: Option<&NightlyStressCheckpoint>,
    ) -> NightlyStressReport {
        let fingerprint = Self::determinism_fingerprint(fixtures);
        let resumed_ids: BTreeSet<String> = match resume {
            Some(cp) if cp.determinism_fingerprint == fingerprint => {
                cp.completed_fixture_ids.iter().cloned().collect()
            }
            _ => BTreeSet::new(),
        };

        // Score every fixture's lever in one deterministic pass.
        let levers: Vec<OptimizationLever> = {
            let mut ordered: Vec<&StressFixture> = fixtures.iter().collect();
            ordered.sort_by(|a, b| a.fixture_id.cmp(&b.fixture_id));
            ordered
                .iter()
                .map(|f| {
                    OptimizationLever::new(
                        lever_id(&f.fixture_id),
                        f.hotspot_id.as_str(),
                        "nightly-stress optimization",
                        f.impact,
                        f.confidence,
                        f.effort,
                    )
                    .with_evidence([format!("baseline.{}", f.fixture_id)])
                })
                .collect()
        };
        let score_report = run_opportunity_scoring(
            &format!("{}/score", self.label),
            &levers,
            &[],
            &OpportunityConfig::default(),
        );

        let mut ordered: Vec<&StressFixture> = fixtures.iter().collect();
        ordered.sort_by(|a, b| a.fixture_id.cmp(&b.fixture_id));

        let ledger: Vec<NightlyStressLedgerEntry> = ordered
            .iter()
            .map(|f| {
                let card = score_report.card(&lever_id(&f.fixture_id));
                let activated = card.is_some_and(|c| c.activated);
                let score_value = card.map_or_else(|| fmt6(0.0), |c| c.score.clone());
                let resumed = resumed_ids.contains(&f.fixture_id);
                evaluate_fixture(
                    &self.run_id,
                    &self.label,
                    f,
                    resumed,
                    activated,
                    &score_value,
                )
            })
            .collect();

        // A checkpoint may only mark fixtures whose outcome is safe to skip on a
        // resume: proven-optimized work (or a prior resume of it). A detected
        // behavior regression or a below-threshold stop must RE-RUN on resume so
        // the red path is re-detected instead of being laundered into a green
        // `resumed` entry with `behavior_preserved=true`.
        let checkpoint = NightlyStressCheckpoint {
            schema_version: NIGHTLY_STRESS_SCHEMA_VERSION.to_string(),
            determinism_fingerprint: fingerprint.clone(),
            completed_fixture_ids: ledger
                .iter()
                .filter(|l| {
                    matches!(
                        l.outcome_class,
                        StressOutcome::Optimized | StressOutcome::Resumed
                    )
                })
                .map(|l| l.fixture_id.clone())
                .collect(),
        };

        let evidence_checksum = sha256_hex(render_ledger_jsonl(&ledger).as_bytes());
        let report_id = format!(
            "nightly-stress-report-{}",
            short_hash(&stable_hash(&format!(
                "{}|{evidence_checksum}",
                self.run_id
            )))
        );
        let summary = self.summarize(&ledger, &fingerprint, &report_id, &evidence_checksum);
        let gate_passes = summary.gate_passes;
        let exported_json_stats = export_json_stats(&report_id, &summary, &ledger);
        let replay_command = format!(
            "cargo run -p doctor_frankentui -- nightly-stress --label '{}' # run {}",
            self.label, self.run_id
        );

        NightlyStressReport {
            schema_version: NIGHTLY_STRESS_SCHEMA_VERSION.to_string(),
            report_id,
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            determinism_fingerprint: fingerprint,
            evidence_checksum,
            ledger,
            checkpoint,
            summary,
            gate_passes,
            replay_command,
            exported_json_stats,
        }
    }

    fn summarize(
        &self,
        ledger: &[NightlyStressLedgerEntry],
        fingerprint: &str,
        report_id: &str,
        evidence_checksum: &str,
    ) -> NightlyStressSummary {
        let required_fields_complete = ledger.iter().all(required_fields_present);
        let lineage_complete = ledger
            .iter()
            .all(|l| !l.baseline_id.is_empty() && !l.profile_id.is_empty());
        // AC3: every optimized path carries a proof and is replay-ready.
        let optimized_proven = ledger
            .iter()
            .filter(|l| l.outcome_class == StressOutcome::Optimized)
            .all(|l| l.proof_available && l.behavior_preserved && l.replay_ready);
        // AC3: a behavior regression is detected and refused promotion.
        let no_silent_regression = ledger
            .iter()
            .filter(|l| l.outcome_class == StressOutcome::BehaviorRegression)
            .all(|l| !l.behavior_preserved && !l.replay_ready);
        let determinism_metadata_present = !fingerprint.is_empty();
        let optimized = ledger
            .iter()
            .filter(|l| l.outcome_class == StressOutcome::Optimized)
            .count();
        let below_threshold = ledger
            .iter()
            .filter(|l| l.outcome_class == StressOutcome::BelowThreshold)
            .count();
        let regressions = ledger
            .iter()
            .filter(|l| l.outcome_class == StressOutcome::BehaviorRegression)
            .count();
        let resumed = ledger.iter().filter(|l| l.resumed).count();
        // Non-vacuous red path: the stress corpus must actually exercise (and,
        // because regressions are never checkpointed, re-detect on every run
        // including resumes) at least one behavior regression. Without this
        // clause, an isomorphism oracle that regressed to always-allow would
        // leave `optimized_proven`/`no_silent_regression` vacuously green.
        let regression_exercised = regressions >= 1;
        let gate_passes = required_fields_complete
            && lineage_complete
            && optimized_proven
            && no_silent_regression
            && regression_exercised
            && determinism_metadata_present;

        NightlyStressSummary {
            schema_version: NIGHTLY_STRESS_SCHEMA_VERSION.to_string(),
            report_id: report_id.to_string(),
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            determinism_fingerprint: fingerprint.to_string(),
            evidence_checksum: evidence_checksum.to_string(),
            total_fixtures: ledger.len(),
            optimized,
            below_threshold,
            regressions,
            resumed,
            required_fields_complete,
            lineage_complete,
            optimized_proven,
            no_silent_regression,
            regression_exercised,
            determinism_metadata_present,
            gate_passes,
            replay_command: format!(
                "cargo run -p doctor_frankentui -- nightly-stress --label '{}' # run {}",
                self.label, self.run_id
            ),
        }
    }
}

/// Run the nightly stress over `fixtures` (no resume).
#[must_use]
pub fn run_nightly_stress(label: &str, fixtures: &[StressFixture]) -> NightlyStressReport {
    NightlyStress::new(label).run(fixtures, None)
}

/// Run the nightly stress over the default corpus (no resume).
#[must_use]
pub fn run_default_nightly_stress(label: &str) -> NightlyStressReport {
    run_nightly_stress(label, &default_stress_fixtures())
}

// ── Report + summary + stats ─────────────────────────────────────────────────

/// Machine-readable summary of one nightly-stress run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NightlyStressSummary {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// The determinism fingerprint (AC1).
    pub determinism_fingerprint: String,
    /// Evidence checksum of the emitted ledger.
    pub evidence_checksum: String,
    /// Total fixtures processed.
    pub total_fixtures: usize,
    /// Fixtures optimized (behavior-preserving).
    pub optimized: usize,
    /// Fixtures below the opportunity gate.
    pub below_threshold: usize,
    /// Behavior regressions detected + refused.
    pub regressions: usize,
    /// Fixtures resumed from a checkpoint.
    pub resumed: usize,
    /// Whether every ledger line has all mandated fields.
    pub required_fields_complete: bool,
    /// Whether every fixture has baseline/profile lineage (AC1).
    pub lineage_complete: bool,
    /// Whether every optimized path is proven + replay-ready (AC3).
    pub optimized_proven: bool,
    /// Whether every behavior regression was refused promotion (AC3).
    pub no_silent_regression: bool,
    /// Whether at least one behavior regression was exercised + re-detected
    /// (non-vacuity for the AC3 clauses).
    pub regression_exercised: bool,
    /// Whether the determinism fingerprint is present (AC1).
    pub determinism_metadata_present: bool,
    /// Whether the fail-closed gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NightlyStressStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The in-memory nightly-stress report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NightlyStressReport {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// The determinism fingerprint.
    pub determinism_fingerprint: String,
    /// Evidence checksum of the emitted ledger.
    pub evidence_checksum: String,
    /// The emitted per-fixture evidence ledger (float-free).
    pub ledger: Vec<NightlyStressLedgerEntry>,
    /// The resume checkpoint produced by this run.
    pub checkpoint: NightlyStressCheckpoint,
    /// Aggregate summary.
    pub summary: NightlyStressSummary,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: NightlyStressStatsArtifact,
}

impl NightlyStressReport {
    /// Render the evidence ledger as JSONL (one entry per line).
    #[must_use]
    pub fn render_ledger_jsonl(&self) -> String {
        render_ledger_jsonl(&self.ledger)
    }
}

fn export_json_stats(
    report_id: &str,
    summary: &NightlyStressSummary,
    ledger: &[NightlyStressLedgerEntry],
) -> NightlyStressStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        summary: &'a NightlyStressSummary,
        ledger: &'a [NightlyStressLedgerEntry],
    }
    let content = serde_json::to_string_pretty(&Export {
        schema_version: NIGHTLY_STRESS_SCHEMA_VERSION,
        report_id,
        summary,
        ledger,
    })
    .unwrap_or_else(|error| error.to_string());
    NightlyStressStatsArtifact {
        path: format!("{report_id}/nightly_stress_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

// ── Pipeline (materialized artifacts) ────────────────────────────────────────

/// Configuration for the materialized nightly-stress pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct NightlyStressPipelineConfig {
    /// Run directory name under the run-root.
    pub run_name: String,
    /// Run label used for deterministic ids.
    pub label: String,
    /// Whether to resume from a checkpoint in the run directory if present.
    pub resume: bool,
}

impl Default for NightlyStressPipelineConfig {
    fn default() -> Self {
        Self {
            run_name: "nightly_stress".to_string(),
            label: "nightly-stress/e2e".to_string(),
            resume: false,
        }
    }
}

/// A materialized pipeline artifact (path + integrity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NightlyStressArtifact {
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
pub struct NightlyStressPipelineOutcome {
    /// Absolute run directory.
    pub run_dir: String,
    /// Absolute path to the JSONL evidence ledger.
    pub ledger_path: String,
    /// Absolute path to the checkpoint JSON.
    pub checkpoint_path: String,
    /// Absolute path to the pipeline summary JSON.
    pub summary_path: String,
    /// Absolute path to the artifact manifest JSON.
    pub manifest_path: String,
    /// Absolute path to the JSON-stats artifact.
    pub stats_path: String,
    /// Whether this run resumed from a prior checkpoint.
    pub resumed_from_checkpoint: bool,
    /// The machine-readable summary.
    pub summary: NightlyStressSummary,
    /// All generated artifacts (with integrity hashes).
    pub artifacts: Vec<NightlyStressArtifact>,
}

fn artifact_of(file: &str, content: &str) -> NightlyStressArtifact {
    NightlyStressArtifact {
        name: file
            .trim_end_matches(".json")
            .trim_end_matches(".jsonl")
            .to_string(),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// Run the nightly stress and materialize the full evidence pipeline under
/// `run_root/<run_name>/`. When `config.resume` is set and a `checkpoint.json`
/// exists in the run directory, the run resumes from it.
///
/// # Errors
/// Returns an error if a run directory or artifact cannot be created/serialized.
pub fn run_nightly_stress_pipeline(
    run_root: &Path,
    config: &NightlyStressPipelineConfig,
) -> crate::error::Result<NightlyStressPipelineOutcome> {
    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let checkpoint_file = "checkpoint.json";
    let checkpoint_path = run_dir.join(checkpoint_file);
    let prior_checkpoint: Option<NightlyStressCheckpoint> = if config.resume {
        std::fs::read_to_string(&checkpoint_path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
    } else {
        None
    };
    let resumed_from_checkpoint = prior_checkpoint.is_some();

    let fixtures = default_stress_fixtures();
    let report =
        NightlyStress::new(config.label.as_str()).run(&fixtures, prior_checkpoint.as_ref());

    let ledger_content = report.render_ledger_jsonl();
    let checkpoint_content = serde_json::to_string_pretty(&report.checkpoint)?;
    let stats_content = report.exported_json_stats.content.clone();
    let summary_content = serde_json::to_string_pretty(&report.summary)?;

    let ledger_file = "evidence_ledger.jsonl";
    let stats_file = "nightly_stress_stats.json";
    let summary_file = "pipeline_summary.json";
    let manifest_file = "artifact_manifest.json";

    crate::util::write_string(&run_dir.join(ledger_file), &ledger_content)?;
    crate::util::write_string(&checkpoint_path, &checkpoint_content)?;
    crate::util::write_string(&run_dir.join(stats_file), &stats_content)?;
    crate::util::write_string(&run_dir.join(summary_file), &summary_content)?;

    let artifacts = vec![
        artifact_of(ledger_file, &ledger_content),
        artifact_of(checkpoint_file, &checkpoint_content),
        artifact_of(stats_file, &stats_content),
        artifact_of(summary_file, &summary_content),
    ];

    #[derive(Serialize)]
    struct Manifest<'a> {
        schema_version: &'a str,
        run_name: &'a str,
        report_id: &'a str,
        resumed_from_checkpoint: bool,
        gate_passes: bool,
        artifacts: &'a [NightlyStressArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: NIGHTLY_STRESS_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        resumed_from_checkpoint,
        gate_passes: report.gate_passes,
        artifacts: &artifacts,
    })?;
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(NightlyStressPipelineOutcome {
        run_dir: run_dir.display().to_string(),
        ledger_path: run_dir.join(ledger_file).display().to_string(),
        checkpoint_path: checkpoint_path.display().to_string(),
        summary_path: run_dir.join(summary_file).display().to_string(),
        manifest_path: run_dir.join(manifest_file).display().to_string(),
        stats_path: run_dir.join(stats_file).display().to_string(),
        resumed_from_checkpoint,
        summary: report.summary,
        artifacts,
    })
}

// ── CLI ──────────────────────────────────────────────────────────────────────

/// CLI arguments for the `nightly-stress` command.
#[derive(Debug, clap::Args)]
pub struct NightlyStressArgs {
    /// Run-root directory; artifacts land under `<run-root>/<run-name>/`.
    #[arg(
        long = "run-root",
        default_value = "/tmp/doctor_frankentui/nightly_stress"
    )]
    pub run_root: PathBuf,

    /// Run directory name.
    #[arg(long = "run-name", default_value = "nightly_stress")]
    pub run_name: String,

    /// Run label used for deterministic ids.
    #[arg(long = "label", default_value = "nightly-stress/e2e")]
    pub label: String,

    /// Resume from a `checkpoint.json` in the run directory if present.
    #[arg(long = "resume", default_value_t = false)]
    pub resume: bool,
}

/// Run the `nightly-stress` command: materialize the pipeline and apply the
/// fail-closed gate.
///
/// # Errors
/// Returns [`crate::error::DoctorError::Exit`] with a non-zero code when the gate
/// fails (a missing field, incomplete lineage, an unproven optimization, a
/// silently-promoted regression, or absent determinism metadata), or an I/O error
/// if artifacts cannot be materialized.
pub fn run_nightly_stress_command(args: NightlyStressArgs) -> crate::error::Result<()> {
    let config = NightlyStressPipelineConfig {
        run_name: args.run_name,
        label: args.label,
        resume: args.resume,
    };
    let outcome = run_nightly_stress_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("nightly stress"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!(
            "fixtures: {} | optimized: {} | below: {} | regressions: {} | resumed: {}",
            summary.total_fixtures,
            summary.optimized,
            summary.below_threshold,
            summary.regressions,
            summary.resumed
        ));
        ui.info(&format!(
            "lineage: {} | optimized proven: {} | no silent regression: {} | resumed-from-checkpoint: {}",
            summary.lineage_complete,
            summary.optimized_proven,
            summary.no_silent_regression,
            outcome.resumed_from_checkpoint
        ));
        if summary.gate_passes {
            ui.success("nightly-stress gate PASSED");
        } else {
            ui.error("nightly-stress gate FAILED");
        }
    }

    if summary.gate_passes {
        Ok(())
    } else {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "nightly-stress gate failed: required_fields_complete={}, lineage_complete={}, optimized_proven={}, no_silent_regression={}, regression_exercised={}, determinism_metadata_present={}",
                summary.required_fields_complete,
                summary.lineage_complete,
                summary.optimized_proven,
                summary.no_silent_regression,
                summary.regression_exercised,
                summary.determinism_metadata_present
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn line<'a>(report: &'a NightlyStressReport, fixture: &str) -> &'a NightlyStressLedgerEntry {
        report
            .ledger
            .iter()
            .find(|l| l.fixture_id == fixture)
            .expect("fixture present")
    }

    #[test]
    fn default_report_passes_gate() {
        let report = run_default_nightly_stress("ns/test");
        assert!(report.gate_passes, "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_fixtures, 5);
        assert!(report.summary.lineage_complete);
        assert!(report.summary.optimized_proven);
        assert!(report.summary.no_silent_regression);
        assert!(report.summary.determinism_metadata_present);
        assert!(report.ledger.iter().all(required_fields_present));
    }

    #[test]
    fn corpus_exercises_all_outcome_classes() {
        let report = run_default_nightly_stress("ns/test");
        assert!(report.summary.optimized >= 2);
        assert_eq!(report.summary.below_threshold, 1);
        assert_eq!(report.summary.regressions, 1);
    }

    #[test]
    fn optimized_fixture_carries_behavior_proof() {
        let report = run_default_nightly_stress("ns/test");
        let l = line(&report, "sf.render");
        assert_eq!(l.outcome_class, StressOutcome::Optimized);
        assert!(l.proof_available);
        assert!(l.behavior_preserved);
        assert!(l.replay_ready);
        assert_ne!(l.proof_id, "n/a");
        // Full six-stage lifecycle recorded.
        assert_eq!(l.stage_timings.len(), 6);
    }

    #[test]
    fn behavior_regression_is_detected_and_refused() {
        let report = run_default_nightly_stress("ns/test");
        let l = line(&report, "sf.alloc");
        assert_eq!(l.outcome_class, StressOutcome::BehaviorRegression);
        assert!(
            l.score_activated,
            "the lever activated; the OUTPUT regressed"
        );
        assert!(!l.behavior_preserved);
        assert!(!l.replay_ready, "a regression must not be promoted");
        assert!(l.proof_available, "the failing proof is still recorded");
    }

    #[test]
    fn below_threshold_fixture_stops_after_score() {
        let report = run_default_nightly_stress("ns/test");
        let l = line(&report, "sf.text");
        assert_eq!(l.outcome_class, StressOutcome::BelowThreshold);
        assert!(!l.score_activated);
        assert_eq!(l.score_decision, "blocked");
        // Lifecycle stops after the score stage (acquire/baseline/profile/score).
        assert_eq!(l.stage_timings.len(), 4);
        assert!(!l.replay_ready);
    }

    #[test]
    fn resume_skips_completed_fixtures_preserving_lineage() {
        // AC1: a fresh run, then a resume that skips the completed fixtures while
        // re-using their lineage byte-for-byte.
        let fixtures = default_stress_fixtures();
        let fresh = run_nightly_stress("ns/test", &fixtures);
        // Resume with the first three fixtures (sorted) marked completed.
        let mut completed: Vec<String> = fresh.checkpoint.completed_fixture_ids.clone();
        completed.truncate(3);
        let checkpoint = NightlyStressCheckpoint {
            schema_version: NIGHTLY_STRESS_SCHEMA_VERSION.to_string(),
            determinism_fingerprint: fresh.determinism_fingerprint.clone(),
            completed_fixture_ids: completed.clone(),
        };
        let resumed = NightlyStress::new("ns/test").run(&fixtures, Some(&checkpoint));
        let resumed_count = resumed.ledger.iter().filter(|l| l.resumed).count();
        assert_eq!(resumed_count, 3);
        assert!(resumed.gate_passes, "summary: {:?}", resumed.summary);
        // Lineage is identical whether a fixture was resumed or freshly evaluated.
        for id in &completed {
            let r = resumed.ledger.iter().find(|l| &l.fixture_id == id).unwrap();
            let f = fresh.ledger.iter().find(|l| &l.fixture_id == id).unwrap();
            assert_eq!(r.baseline_id, f.baseline_id);
            assert_eq!(r.profile_id, f.profile_id);
            assert!(r.resumed && !f.resumed);
        }
    }

    #[test]
    fn resume_with_mismatched_fingerprint_runs_fresh() {
        let fixtures = default_stress_fixtures();
        let bogus = NightlyStressCheckpoint {
            schema_version: NIGHTLY_STRESS_SCHEMA_VERSION.to_string(),
            determinism_fingerprint: "deadbeef".to_string(),
            completed_fixture_ids: vec!["sf.render".to_string()],
        };
        let report = NightlyStress::new("ns/test").run(&fixtures, Some(&bogus));
        // The fingerprint does not match, so nothing is resumed.
        assert_eq!(report.ledger.iter().filter(|l| l.resumed).count(), 0);
    }

    #[test]
    fn report_is_deterministic_and_replay_identical() {
        let a = run_default_nightly_stress("ns/test");
        let b = run_default_nightly_stress("ns/test");
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.ledger, b.ledger);
        assert_eq!(a.determinism_fingerprint, b.determinism_fingerprint);
        assert_eq!(a.render_ledger_jsonl(), b.render_ledger_jsonl());
    }

    #[test]
    fn run_is_independent_of_input_order() {
        let mut fixtures = default_stress_fixtures();
        let forward = run_nightly_stress("ns/test", &fixtures);
        fixtures.reverse();
        let reversed = run_nightly_stress("ns/test", &fixtures);
        assert_eq!(forward.ledger, reversed.ledger);
    }

    #[test]
    fn pipeline_materializes_and_resumes() {
        let dir = tempfile::tempdir().unwrap();
        // First run: fresh, writes a checkpoint.
        let first =
            run_nightly_stress_pipeline(dir.path(), &NightlyStressPipelineConfig::default())
                .unwrap();
        assert!(first.summary.gate_passes);
        assert!(!first.resumed_from_checkpoint);
        // ledger + checkpoint + stats + summary (the manifest is a file but is not
        // self-listed in its own artifacts array).
        assert_eq!(first.artifacts.len(), 4);
        for artifact in &first.artifacts {
            let path = std::path::Path::new(&first.run_dir).join(&artifact.file);
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(sha256_hex(&bytes), artifact.sha256);
        }
        // Second run with resume: only the proven-optimized fixtures were
        // checkpointed; the behavior regression and the below-threshold stop
        // re-run so their outcomes are re-detected, never laundered.
        let second = run_nightly_stress_pipeline(
            dir.path(),
            &NightlyStressPipelineConfig {
                resume: true,
                ..NightlyStressPipelineConfig::default()
            },
        )
        .unwrap();
        assert!(second.summary.gate_passes);
        assert!(second.resumed_from_checkpoint);
        assert_eq!(second.summary.resumed, first.summary.optimized);
        assert_eq!(second.summary.regressions, 1);
        assert_eq!(second.summary.below_threshold, 1);
    }

    #[test]
    fn regression_is_not_checkpointed_and_reruns_on_resume() {
        let fixtures = default_stress_fixtures();
        let fresh = run_nightly_stress("ns/test", &fixtures);
        // Neither the regression nor the below-threshold stop is checkpointed.
        assert!(
            !fresh
                .checkpoint
                .completed_fixture_ids
                .contains(&"sf.alloc".to_string())
        );
        assert!(
            !fresh
                .checkpoint
                .completed_fixture_ids
                .contains(&"sf.text".to_string())
        );
        // A resume from that checkpoint re-detects the regression.
        let resumed = NightlyStress::new("ns/test").run(&fixtures, Some(&fresh.checkpoint));
        let l = resumed
            .ledger
            .iter()
            .find(|l| l.fixture_id == "sf.alloc")
            .unwrap();
        assert_eq!(l.outcome_class, StressOutcome::BehaviorRegression);
        assert!(!l.behavior_preserved);
        assert!(!l.replay_ready);
        assert_eq!(resumed.summary.regressions, 1);
        assert!(resumed.summary.regression_exercised);
        assert!(resumed.gate_passes, "summary: {:?}", resumed.summary);
    }

    #[test]
    fn fingerprint_covers_full_fixture_definitions() {
        let mut fixtures = default_stress_fixtures();
        let original = NightlyStress::determinism_fingerprint(&fixtures);
        // Changing an input (not the id) must invalidate any prior checkpoint.
        fixtures[0].impact += 1.0;
        assert_ne!(original, NightlyStress::determinism_fingerprint(&fixtures));
        // A flipped behavior-preservation flag must invalidate it too.
        let mut flipped = default_stress_fixtures();
        flipped[3].behavior_preserved = true;
        assert_ne!(original, NightlyStress::determinism_fingerprint(&flipped));
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = run_default_nightly_stress("ns/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}") {
            let first = run_default_nightly_stress(&label);
            let second = run_default_nightly_stress(&label);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.ledger, &second.ledger);
        }

        #[test]
        fn prop_gate_always_passes(label in "[a-z]{1,8}") {
            let report = run_default_nightly_stress(&label);
            prop_assert!(report.gate_passes);
            prop_assert!(report.summary.optimized_proven);
            prop_assert!(report.summary.no_silent_regression);
        }
    }
}
