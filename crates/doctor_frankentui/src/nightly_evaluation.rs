//! Nightly continuous evaluation pipeline (bd-3bxhj.8.7).
//!
//! The nightly pipeline is the standing integration that combines corpus /
//! adversarial migration with optimization-loop checks and drift detection. It
//! shards a fixture corpus deterministically, runs each shard through the real
//! optimization kernels, and emits a single auditable evidence ledger plus a
//! historical time-series:
//!
//! - **Deterministic sharding** (AC1): every fixture is assigned to a shard by a
//!   stable hash of its id, so a re-run lands the identical partition; the gate
//!   re-derives the assignment independently.
//! - **VOI-gated re-profile rounds** (AC1): the [`crate::test_budget_allocator`]
//!   decides which fixtures are *worth* the expensive re-profile round (a fixture
//!   below the value-of-information floor is deferred); a scheduled, non-drifting
//!   fixture runs through the [`crate::reprofile_loop`].
//! - **Drift detection** (AC1): every fixture's metric series is screened by the
//!   [`crate::drift_monitor`] (BOCPD + CUSUM); a regressed fixture is investigated
//!   before any optimization is attempted.
//! - **Triage + rollback-readiness** (AC2): every failure (a drift regression or a
//!   re-profile pause) emits a concise triage summary with a replay pointer and
//!   the change's rollback-readiness status.
//! - **Historical metadata** (AC3): each fixture contributes a time-series point
//!   (latency percentiles, hotspot frontier movement, optimization efficacy) for
//!   downstream trend analysis.
//!
//! The evidence ledger is **float-free** (every numeric term is a fixed-decimal
//! string via [`fmt6`]), so it derives [`Eq`] and replays byte-identically — even
//! though the underlying kernels carry raw `f64`. The pipeline is materialized to
//! disk (ledger + stats + summary + manifest) so the nightly-stress E2E
//! (bd-3bxhj.8.9) can drive and validate it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::drift_monitor::{DriftMetricSeries, DriftMonitor, DriftMonitorConfig, MetricDirection};
use crate::recommendation_contract::EffortSize;
use crate::reprofile_loop::{
    BacklogCandidate, HotspotCost, OptimizationRound, ProfileSnapshot, ReprofileConfig,
    RoundVerdict, run_reprofile_loop,
};
use crate::test_budget_allocator::{TestBudgetAllocator, TestBudgetConfig, TestFixtureCandidate};

/// Schema version for the in-memory nightly-evaluation report.
pub const NIGHTLY_EVALUATION_SCHEMA_VERSION: &str = "nightly-evaluation-v1";

/// Schema version for the materialized nightly-evaluation pipeline artifacts.
pub const NIGHTLY_EVALUATION_PIPELINE_SCHEMA_VERSION: &str = "nightly-evaluation-pipeline-v1";

/// Numeric epsilon for guarded comparisons.
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

/// Deterministic fixed-decimal rendering so the ledger stays float-free and
/// derives `Eq`. Non-finite inputs (and negative zero) render as `0.000000`.
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

/// Deterministic shard assignment: a stable hash of the fixture id modulo the
/// shard count. A re-run lands the identical partition (AC1).
fn shard_of(fixture_id: &str, shard_count: usize) -> usize {
    let count = shard_count.max(1);
    let digest = stable_hash(fixture_id);
    let prefix: String = digest.chars().take(8).collect();
    let value = u64::from_str_radix(&prefix, 16).unwrap_or(0);
    usize::try_from(value % count as u64).unwrap_or(0)
}

fn shard_id(index: usize) -> String {
    format!("shard-{index:02}")
}

/// The dominant (highest-cost-share) hotspot of a profile snapshot, or `"none"`.
fn dominant_hotspot(snapshot: &ProfileSnapshot) -> String {
    snapshot
        .hotspots
        .iter()
        .max_by(|a, b| {
            a.cost_share
                .total_cmp(&b.cost_share)
                .then_with(|| b.hotspot_id.cmp(&a.hotspot_id))
        })
        .map_or_else(|| "none".to_string(), |h| h.hotspot_id.clone())
}

// ── Config ───────────────────────────────────────────────────────────────────

/// Configuration for a nightly-evaluation run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NightlyEvaluationConfig {
    /// Number of deterministic shards.
    pub shard_count: usize,
    /// Total VOI compute budget for re-profile scheduling.
    pub reprofile_budget_units: f64,
    /// Fixtures whose value-of-information is below this floor are deferred.
    pub voi_floor: f64,
}

impl Default for NightlyEvaluationConfig {
    fn default() -> Self {
        Self {
            shard_count: 4,
            reprofile_budget_units: 1000.0,
            voi_floor: 0.005,
        }
    }
}

// ── Outcome vocabulary ─────────────────────────────────────────────────────────

/// The classified outcome of one fixture's nightly evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeClass {
    /// Scheduled, no drift, re-profile round continued -> a safe optimization.
    Optimized,
    /// Below the VOI floor (or budget-deferred): the policy declined the round.
    Deferred,
    /// The drift monitor flagged a regression; investigate before optimizing.
    DriftRegression,
    /// A scheduled re-profile round paused (regression / unstable / non-finite).
    ReprofilePause,
}

impl OutcomeClass {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Optimized => "optimized",
            Self::Deferred => "deferred",
            Self::DriftRegression => "drift_regression",
            Self::ReprofilePause => "reprofile_pause",
        }
    }

    /// Whether this outcome is a pipeline failure (needs triage + rollback status).
    #[must_use]
    pub fn is_failure(self) -> bool {
        matches!(self, Self::DriftRegression | Self::ReprofilePause)
    }
}

// ── Fixture input ──────────────────────────────────────────────────────────────

/// One nightly-evaluation fixture: a metric series for drift screening, a
/// before/after profile pair for the re-profile round, a backlog to re-rank, and
/// VOI inputs for scheduling.
#[derive(Debug, Clone, PartialEq)]
pub struct NightlyFixture {
    /// Stable fixture id.
    pub fixture_id: String,
    /// The pipeline stage this fixture belongs to.
    pub stage_id: String,
    /// Latency observations (lower-is-better) screened by the drift monitor.
    pub metric_series: Vec<f64>,
    /// The pre-change profile snapshot.
    pub before: ProfileSnapshot,
    /// The post-change profile snapshot.
    pub after: ProfileSnapshot,
    /// The optimization backlog re-ranked by the re-profile round.
    pub backlog: Vec<BacklogCandidate>,
    /// VOI compute cost for scheduling.
    pub compute_cost: f64,
    /// VOI prior alpha.
    pub prior_alpha: f64,
    /// VOI prior beta.
    pub prior_beta: f64,
    /// VOI value scale.
    pub value_scale: f64,
    /// Whether the candidate change carries a complete rollback plan.
    pub has_rollback: bool,
}

fn snapshot(
    p50: f64,
    p95: f64,
    p99: f64,
    throughput: f64,
    hotspots: &[(&str, f64)],
    stable: bool,
) -> ProfileSnapshot {
    ProfileSnapshot {
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
        throughput,
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

fn backlog(pairs: &[(&str, &str)]) -> Vec<BacklogCandidate> {
    pairs
        .iter()
        .map(|(cand, hot)| BacklogCandidate {
            candidate_id: (*cand).to_string(),
            hotspot_id: (*hot).to_string(),
            confidence: 0.85,
            effort: EffortSize::Medium,
        })
        .collect()
}

/// A representative mixed nightly corpus: optimized passes, a VOI-deferred fixture,
/// a drift regression, and a re-profile pause — so the pipeline exercises both the
/// happy and the triage paths without being vacuously green.
#[must_use]
pub fn default_nightly_fixtures() -> Vec<NightlyFixture> {
    // A stable series with small deterministic baseline noise (non-zero std so the
    // CUSUM standardization is well-defined) and no late shift.
    let flat: Vec<f64> = vec![
        10.0, 11.0, 9.0, 10.0, 11.0, 9.0, 10.0, 11.0, 9.0, 10.0, 11.0, 9.0,
    ];
    // The same baseline, then a large upward jump -> a regression for lower-is-better.
    let regressed: Vec<f64> = vec![
        10.0, 11.0, 9.0, 10.0, 11.0, 9.0, 10.0, 11.0, 60.0, 62.0, 58.0, 61.0,
    ];
    vec![
        // Optimized: scheduled, no drift, p99 improves, bottleneck stays put.
        NightlyFixture {
            fixture_id: "f.render".to_string(),
            stage_id: "render".to_string(),
            metric_series: flat.clone(),
            before: snapshot(
                40.0,
                90.0,
                120.0,
                10_000.0,
                &[("hot.render", 0.6), ("hot.layout", 0.4)],
                true,
            ),
            after: snapshot(
                32.0,
                70.0,
                85.0,
                11_500.0,
                &[("hot.render", 0.55), ("hot.layout", 0.45)],
                true,
            ),
            backlog: backlog(&[("cand.render", "hot.render"), ("cand.layout", "hot.layout")]),
            compute_cost: 1.0,
            prior_alpha: 1.0,
            prior_beta: 1.0,
            value_scale: 1.0,
            has_rollback: true,
        },
        // Optimized + bottleneck shift (hotspot movement in the history series).
        NightlyFixture {
            fixture_id: "f.layout".to_string(),
            stage_id: "layout".to_string(),
            metric_series: flat.clone(),
            before: snapshot(
                38.0,
                88.0,
                118.0,
                10_200.0,
                &[("hot.render", 0.6), ("hot.layout", 0.4)],
                true,
            ),
            after: snapshot(
                30.0,
                66.0,
                82.0,
                12_000.0,
                &[("hot.render", 0.3), ("hot.layout", 0.55)],
                true,
            ),
            backlog: backlog(&[("cand.render", "hot.render"), ("cand.layout", "hot.layout")]),
            compute_cost: 1.0,
            prior_alpha: 1.0,
            prior_beta: 1.0,
            value_scale: 1.0,
            has_rollback: true,
        },
        // Drift regression: the latency series regresses -> investigate, do not optimize.
        NightlyFixture {
            fixture_id: "f.alloc".to_string(),
            stage_id: "alloc".to_string(),
            metric_series: regressed,
            before: snapshot(42.0, 92.0, 122.0, 9_800.0, &[("hot.alloc", 0.7)], true),
            after: snapshot(40.0, 88.0, 116.0, 10_000.0, &[("hot.alloc", 0.68)], true),
            backlog: backlog(&[("cand.alloc", "hot.alloc")]),
            compute_cost: 1.0,
            prior_alpha: 1.0,
            prior_beta: 1.0,
            value_scale: 1.0,
            has_rollback: true,
        },
        // Re-profile pause: scheduled, no drift, but the candidate p99 regresses.
        NightlyFixture {
            fixture_id: "f.syscall".to_string(),
            stage_id: "syscall".to_string(),
            metric_series: flat.clone(),
            before: snapshot(36.0, 80.0, 85.0, 11_000.0, &[("hot.syscall", 0.65)], true),
            after: snapshot(48.0, 110.0, 130.0, 9_000.0, &[("hot.syscall", 0.66)], true),
            backlog: backlog(&[("cand.syscall", "hot.syscall")]),
            compute_cost: 1.0,
            prior_alpha: 1.0,
            prior_beta: 1.0,
            value_scale: 1.0,
            has_rollback: true,
        },
        // Deferred: VOI is below the floor (near-certain posterior) -> policy skips.
        NightlyFixture {
            fixture_id: "f.text".to_string(),
            stage_id: "text".to_string(),
            metric_series: flat.clone(),
            before: snapshot(20.0, 40.0, 55.0, 14_000.0, &[("hot.text", 0.5)], true),
            after: snapshot(20.0, 40.0, 54.0, 14_100.0, &[("hot.text", 0.5)], true),
            backlog: backlog(&[("cand.text", "hot.text")]),
            compute_cost: 1.0,
            prior_alpha: 80.0,
            prior_beta: 2.0,
            value_scale: 1.0,
            has_rollback: true,
        },
        // Optimized: another safe win in a different shard.
        NightlyFixture {
            fixture_id: "f.vfx".to_string(),
            stage_id: "vfx".to_string(),
            metric_series: flat,
            before: snapshot(50.0, 120.0, 160.0, 8_000.0, &[("hot.vfx", 0.72)], true),
            after: snapshot(42.0, 96.0, 120.0, 9_500.0, &[("hot.vfx", 0.7)], true),
            backlog: backlog(&[("cand.vfx", "hot.vfx")]),
            compute_cost: 1.0,
            prior_alpha: 1.0,
            prior_beta: 1.0,
            value_scale: 1.0,
            has_rollback: true,
        },
    ]
}

// ── Ledger ───────────────────────────────────────────────────────────────────

/// One nightly-evaluation ledger line (float-free; derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NightlyLedgerEntry {
    /// Ledger schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// The shard id this fixture was assigned to.
    pub shard_id: String,
    /// The shard index (re-derivable from the fixture id).
    pub shard_index: usize,
    /// The fixture id.
    pub fixture_id: String,
    /// The pipeline stage id.
    pub stage_id: String,
    /// Whether the VOI policy scheduled a re-profile round for this fixture.
    pub scheduled: bool,
    /// Whether a re-profile round actually ran.
    pub reprofiled: bool,
    /// Whether the drift monitor flagged a regression.
    pub drift_regressed: bool,
    /// Number of drift events recorded for this fixture.
    pub drift_event_count: usize,
    /// The re-profile verdict (`continue` / `pause` / `n/a`).
    pub reprofile_verdict: String,
    /// The pre-change dominant hotspot.
    pub bottleneck_before: String,
    /// The post-change dominant hotspot.
    pub bottleneck_after: String,
    /// Whether the dominant hotspot shifted.
    pub bottleneck_shifted: bool,
    /// p95 latency before (fixed-decimal).
    pub p95_before: String,
    /// p95 latency after (fixed-decimal).
    pub p95_after: String,
    /// p99 latency before (fixed-decimal).
    pub p99_before: String,
    /// p99 latency after (fixed-decimal).
    pub p99_after: String,
    /// Optimization efficacy: p99 improvement percent (fixed-decimal; 0 if no round).
    pub optimization_efficacy_pct: String,
    /// The classified outcome.
    pub outcome_class: OutcomeClass,
    /// Whether the change carries a complete rollback plan (AC2).
    pub rollback_ready: bool,
    /// Triage summary for a failure (empty when the fixture passed).
    pub triage_hint: String,
    /// Deterministic single-command replay reference.
    pub reproduction_command: String,
}

fn required_fields_present(line: &NightlyLedgerEntry) -> bool {
    !line.schema_version.is_empty()
        && !line.run_id.is_empty()
        && !line.shard_id.is_empty()
        && !line.fixture_id.is_empty()
        && !line.stage_id.is_empty()
        && !line.reprofile_verdict.is_empty()
        && !line.bottleneck_before.is_empty()
        && !line.bottleneck_after.is_empty()
        && !line.p95_before.is_empty()
        && !line.p99_before.is_empty()
        && !line.optimization_efficacy_pct.is_empty()
        && !line.reproduction_command.is_empty()
}

fn render_ledger_jsonl(ledger: &[NightlyLedgerEntry]) -> String {
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

// ── Time-series history (AC3) ─────────────────────────────────────────────────

/// One historical time-series point for trend analysis (AC3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NightlyHistoryPoint {
    /// Deterministic sequence ordinal (sorted fixture order; no wall-clock).
    pub sequence: usize,
    /// The fixture id.
    pub fixture_id: String,
    /// The shard id.
    pub shard_id: String,
    /// p95 latency observed for the evaluated revision (fixed-decimal).
    pub p95_after: String,
    /// p99 latency observed for the evaluated revision (fixed-decimal).
    pub p99_after: String,
    /// Throughput observed for the evaluated revision (fixed-decimal).
    pub throughput_after: String,
    /// The dominant hotspot after the round (hotspot movement).
    pub bottleneck_after: String,
    /// Optimization efficacy: p99 improvement percent (fixed-decimal).
    pub optimization_efficacy_pct: String,
}

// ── Per-fixture evaluation ─────────────────────────────────────────────────────

struct FixtureOutcome {
    entry: NightlyLedgerEntry,
    history: NightlyHistoryPoint,
}

#[allow(clippy::too_many_arguments)]
fn evaluate_fixture(
    run_id: &str,
    label: &str,
    fixture: &NightlyFixture,
    shard_index: usize,
    scheduled: bool,
    drift_regressed: bool,
    drift_event_count: usize,
    sequence: usize,
) -> FixtureOutcome {
    let reproduction_command = format!(
        "cargo run -p doctor_frankentui -- nightly-eval --label '{label}' # run {run_id} fixture {}",
        fixture.fixture_id
    );
    let bottleneck_before = dominant_hotspot(&fixture.before);

    // The policy: run the re-profile round only when the VOI policy scheduled it
    // and the drift monitor has not flagged a regression to investigate first.
    let reprofiled = scheduled && !drift_regressed;

    let (verdict, bottleneck_after, bottleneck_shifted, triage_from_round, efficacy_pct) =
        if reprofiled {
            let round = OptimizationRound {
                round_number: 1,
                change_id: format!("chg.{}", fixture.fixture_id),
                lever_id: format!("lever.{}", fixture.fixture_id),
                before: fixture.before.clone(),
                after: fixture.after.clone(),
                backlog: fixture.backlog.clone(),
            };
            let report = run_reprofile_loop(
                &format!("{label}/{}", fixture.fixture_id),
                &[round],
                &ReprofileConfig::default(),
            );
            let entry = report.round(1);
            let verdict = entry.map_or(RoundVerdict::Pause, |e| e.verdict);
            let shifted = entry.is_some_and(|e| e.bottleneck_shifted);
            let triage = entry.map(|e| e.triage_hint.clone()).unwrap_or_default();
            let p99b = fixture.before.p99_us;
            let p99a = fixture.after.p99_us;
            let efficacy = if p99b.abs() > EPS {
                (p99b - p99a) / p99b * 100.0
            } else {
                0.0
            };
            (
                verdict.as_str().to_string(),
                dominant_hotspot(&fixture.after),
                shifted,
                triage,
                efficacy,
            )
        } else {
            // No round: report the pre-change bottleneck and zero efficacy.
            (
                "n/a".to_string(),
                bottleneck_before.clone(),
                false,
                String::new(),
                0.0,
            )
        };

    let outcome_class = if drift_regressed {
        OutcomeClass::DriftRegression
    } else if !scheduled {
        OutcomeClass::Deferred
    } else if verdict == "pause" {
        OutcomeClass::ReprofilePause
    } else {
        OutcomeClass::Optimized
    };

    let triage_hint = match outcome_class {
        OutcomeClass::DriftRegression => format!(
            "drift regression detected ({drift_event_count} event(s)); investigate the metric series before optimizing — rollback_ready={}",
            fixture.has_rollback
        ),
        OutcomeClass::ReprofilePause => {
            let base = if triage_from_round.is_empty() {
                "re-profile round paused".to_string()
            } else {
                triage_from_round
            };
            format!("{base}; rollback_ready={}", fixture.has_rollback)
        }
        OutcomeClass::Optimized | OutcomeClass::Deferred => String::new(),
    };

    let entry = NightlyLedgerEntry {
        schema_version: NIGHTLY_EVALUATION_SCHEMA_VERSION.to_string(),
        run_id: run_id.to_string(),
        shard_id: shard_id(shard_index),
        shard_index,
        fixture_id: fixture.fixture_id.clone(),
        stage_id: fixture.stage_id.clone(),
        scheduled,
        reprofiled,
        drift_regressed,
        drift_event_count,
        reprofile_verdict: verdict,
        bottleneck_before,
        bottleneck_after: bottleneck_after.clone(),
        bottleneck_shifted,
        p95_before: fmt6(fixture.before.p95_us),
        p95_after: fmt6(fixture.after.p95_us),
        p99_before: fmt6(fixture.before.p99_us),
        p99_after: fmt6(fixture.after.p99_us),
        optimization_efficacy_pct: fmt6(efficacy_pct),
        outcome_class,
        rollback_ready: fixture.has_rollback,
        triage_hint,
        reproduction_command,
    };

    let history = NightlyHistoryPoint {
        sequence,
        fixture_id: fixture.fixture_id.clone(),
        shard_id: shard_id(shard_index),
        p95_after: fmt6(fixture.after.p95_us),
        p99_after: fmt6(fixture.after.p99_us),
        throughput_after: fmt6(fixture.after.throughput),
        bottleneck_after,
        optimization_efficacy_pct: fmt6(efficacy_pct),
    };

    FixtureOutcome { entry, history }
}

// ── Pipeline driver ────────────────────────────────────────────────────────────

/// The deterministic nightly-evaluation engine.
#[derive(Debug, Clone)]
pub struct NightlyEvaluation {
    run_id: String,
    label: String,
    config: NightlyEvaluationConfig,
}

impl NightlyEvaluation {
    /// Construct an evaluation with a deterministic run id derived from its label.
    #[must_use]
    pub fn new(label: impl Into<String>, config: NightlyEvaluationConfig) -> Self {
        let label = label.into();
        let run_id = format!(
            "nightly-eval-{}",
            short_hash(&stable_hash(&format!(
                "{NIGHTLY_EVALUATION_SCHEMA_VERSION}|{label}|{}",
                config.shard_count
            )))
        );
        Self {
            run_id,
            label,
            config,
        }
    }

    /// The deterministic run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Run the nightly evaluation over `fixtures`.
    #[must_use]
    pub fn run(&self, fixtures: &[NightlyFixture]) -> NightlyEvaluationReport {
        // Deterministic ordering: sort fixtures by (shard, fixture_id) so the run
        // is independent of caller input order.
        let mut ordered: Vec<&NightlyFixture> = fixtures.iter().collect();
        ordered.sort_by(|a, b| {
            let sa = shard_of(&a.fixture_id, self.config.shard_count);
            let sb = shard_of(&b.fixture_id, self.config.shard_count);
            sa.cmp(&sb).then_with(|| a.fixture_id.cmp(&b.fixture_id))
        });

        // Drift screening over the whole corpus.
        let series: Vec<DriftMetricSeries> = ordered
            .iter()
            .map(|f| {
                DriftMetricSeries::new(f.fixture_id.as_str(), MetricDirection::LowerIsBetter)
                    .with_observations(f.metric_series.clone())
            })
            .collect();
        let drift_report = DriftMonitor::new(DriftMonitorConfig::default()).analyze(series);
        let regressed: BTreeSet<String> = drift_report.regressed_metric_ids().into_iter().collect();

        // VOI scheduling: which fixtures are worth the re-profile round.
        let candidates: Vec<TestFixtureCandidate> = ordered
            .iter()
            .map(|f| {
                TestFixtureCandidate::new(
                    f.fixture_id.as_str(),
                    f.stage_id.as_str(),
                    f.compute_cost,
                )
                .with_prior(f.prior_alpha, f.prior_beta)
                .with_value_scale(f.value_scale)
            })
            .collect();
        let budget_report = TestBudgetAllocator::new(
            TestBudgetConfig::new(self.config.reprofile_budget_units)
                .with_voi_floor(self.config.voi_floor),
        )
        .allocate(candidates);
        let scheduled: BTreeSet<String> =
            budget_report.scheduled_fixture_ids().into_iter().collect();

        let mut ledger: Vec<NightlyLedgerEntry> = Vec::new();
        let mut history: Vec<NightlyHistoryPoint> = Vec::new();
        for (sequence, fixture) in ordered.iter().enumerate() {
            let shard_index = shard_of(&fixture.fixture_id, self.config.shard_count);
            let drift_regressed = regressed.contains(&fixture.fixture_id);
            let drift_event_count = drift_report
                .events
                .iter()
                .filter(|e| e.metric_id == fixture.fixture_id)
                .count();
            let is_scheduled = scheduled.contains(&fixture.fixture_id);
            let outcome = evaluate_fixture(
                &self.run_id,
                &self.label,
                fixture,
                shard_index,
                is_scheduled,
                drift_regressed,
                drift_event_count,
                sequence,
            );
            ledger.push(outcome.entry);
            history.push(outcome.history);
        }

        let evidence_checksum = sha256_hex(render_ledger_jsonl(&ledger).as_bytes());
        let report_id = format!(
            "nightly-eval-report-{}",
            short_hash(&stable_hash(&format!(
                "{}|{evidence_checksum}",
                self.run_id
            )))
        );
        let summary = self.summarize(&ledger, &history, &report_id, &evidence_checksum);
        let gate_passes = summary.gate_passes;
        let exported_json_stats = export_json_stats(&report_id, &summary, &ledger, &history);
        let replay_command = format!(
            "cargo run -p doctor_frankentui -- nightly-eval --label '{}' # run {}",
            self.label, self.run_id
        );

        NightlyEvaluationReport {
            schema_version: NIGHTLY_EVALUATION_SCHEMA_VERSION.to_string(),
            report_id,
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum,
            ledger,
            history,
            summary,
            gate_passes,
            replay_command,
            exported_json_stats,
        }
    }

    fn summarize(
        &self,
        ledger: &[NightlyLedgerEntry],
        history: &[NightlyHistoryPoint],
        report_id: &str,
        evidence_checksum: &str,
    ) -> NightlyEvaluationSummary {
        let required_fields_complete = ledger.iter().all(required_fields_present);
        let optimized = ledger
            .iter()
            .filter(|l| l.outcome_class == OutcomeClass::Optimized)
            .count();
        let deferred = ledger
            .iter()
            .filter(|l| l.outcome_class == OutcomeClass::Deferred)
            .count();
        let failures = ledger
            .iter()
            .filter(|l| l.outcome_class.is_failure())
            .count();
        // AC2: every failure carries a triage hint AND a recorded rollback-readiness.
        let failures_triaged = ledger
            .iter()
            .filter(|l| l.outcome_class.is_failure())
            .all(|l| !l.triage_hint.is_empty() && l.triage_hint.contains("rollback_ready="));
        // AC1: the shard assignment is re-derivable from the fixture id.
        let sharding_deterministic = ledger
            .iter()
            .all(|l| l.shard_index == shard_of(&l.fixture_id, self.config.shard_count));
        // AC3: every fixture contributes a complete history point.
        let history_fixtures: BTreeSet<&str> =
            history.iter().map(|h| h.fixture_id.as_str()).collect();
        let ledger_fixtures: BTreeSet<&str> =
            ledger.iter().map(|l| l.fixture_id.as_str()).collect();
        let history_complete = history_fixtures == ledger_fixtures
            && history.iter().all(|h| {
                !h.fixture_id.is_empty()
                    && !h.p99_after.is_empty()
                    && !h.bottleneck_after.is_empty()
                    && !h.optimization_efficacy_pct.is_empty()
            });
        let shards_used: BTreeSet<usize> = ledger.iter().map(|l| l.shard_index).collect();
        // Non-vacuity: an empty corpus proves nothing, and the nightly corpus
        // must actually exercise the failure/triage red path — otherwise a
        // drift monitor or re-profile kernel that regressed to never-fail
        // would leave `failures_triaged` (an all-quantifier over failures)
        // vacuously green.
        let corpus_nonempty = !ledger.is_empty();
        let red_path_exercised = failures >= 1;
        let gate_passes = required_fields_complete
            && corpus_nonempty
            && red_path_exercised
            && failures_triaged
            && sharding_deterministic
            && history_complete;

        NightlyEvaluationSummary {
            schema_version: NIGHTLY_EVALUATION_SCHEMA_VERSION.to_string(),
            report_id: report_id.to_string(),
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum: evidence_checksum.to_string(),
            total_fixtures: ledger.len(),
            shard_count: self.config.shard_count,
            shards_used: shards_used.len(),
            optimized,
            deferred,
            failures,
            required_fields_complete,
            corpus_nonempty,
            red_path_exercised,
            failures_triaged,
            sharding_deterministic,
            history_complete,
            history_points: history.len(),
            gate_passes,
            replay_command: format!(
                "cargo run -p doctor_frankentui -- nightly-eval --label '{}' # run {}",
                self.label, self.run_id
            ),
        }
    }
}

/// Run the nightly evaluation over `fixtures` with the given label + config.
#[must_use]
pub fn run_nightly_evaluation(
    label: &str,
    config: NightlyEvaluationConfig,
    fixtures: &[NightlyFixture],
) -> NightlyEvaluationReport {
    NightlyEvaluation::new(label, config).run(fixtures)
}

/// Run the nightly evaluation over the default representative corpus.
#[must_use]
pub fn run_default_nightly_evaluation(label: &str) -> NightlyEvaluationReport {
    run_nightly_evaluation(
        label,
        NightlyEvaluationConfig::default(),
        &default_nightly_fixtures(),
    )
}

// ── Report + summary + stats ─────────────────────────────────────────────────

/// Machine-readable summary of one nightly-evaluation run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NightlyEvaluationSummary {
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
    /// Total fixtures evaluated.
    pub total_fixtures: usize,
    /// Configured shard count.
    pub shard_count: usize,
    /// Distinct shards actually populated.
    pub shards_used: usize,
    /// Fixtures that optimized safely.
    pub optimized: usize,
    /// Fixtures the policy deferred.
    pub deferred: usize,
    /// Fixtures that failed (drift regression or re-profile pause).
    pub failures: usize,
    /// Whether every ledger line has all mandated fields.
    pub required_fields_complete: bool,
    /// Whether the corpus was non-empty (non-vacuity).
    pub corpus_nonempty: bool,
    /// Whether at least one failure was exercised (non-vacuity for AC2 — a
    /// never-failing kernel must redden the gate, not green it).
    pub red_path_exercised: bool,
    /// Whether every failure carries a triage hint + rollback-readiness (AC2).
    pub failures_triaged: bool,
    /// Whether every shard assignment is re-derivable from the fixture id (AC1).
    pub sharding_deterministic: bool,
    /// Whether every fixture has a complete history point (AC3).
    pub history_complete: bool,
    /// Number of historical time-series points.
    pub history_points: usize,
    /// Whether the fail-closed gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NightlyEvaluationStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The in-memory nightly-evaluation report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NightlyEvaluationReport {
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
    pub ledger: Vec<NightlyLedgerEntry>,
    /// The historical time-series points (AC3).
    pub history: Vec<NightlyHistoryPoint>,
    /// Aggregate summary.
    pub summary: NightlyEvaluationSummary,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: NightlyEvaluationStatsArtifact,
}

impl NightlyEvaluationReport {
    /// Render the evidence ledger as JSONL (one entry per line).
    #[must_use]
    pub fn render_ledger_jsonl(&self) -> String {
        render_ledger_jsonl(&self.ledger)
    }

    /// The triage summaries for failing fixtures (AC2).
    #[must_use]
    pub fn triage_summaries(&self) -> Vec<&NightlyLedgerEntry> {
        self.ledger
            .iter()
            .filter(|l| l.outcome_class.is_failure())
            .collect()
    }
}

fn export_json_stats(
    report_id: &str,
    summary: &NightlyEvaluationSummary,
    ledger: &[NightlyLedgerEntry],
    history: &[NightlyHistoryPoint],
) -> NightlyEvaluationStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        summary: &'a NightlyEvaluationSummary,
        ledger: &'a [NightlyLedgerEntry],
        history: &'a [NightlyHistoryPoint],
    }
    let content = serde_json::to_string_pretty(&Export {
        schema_version: NIGHTLY_EVALUATION_SCHEMA_VERSION,
        report_id,
        summary,
        ledger,
        history,
    })
    .unwrap_or_else(|error| error.to_string());
    NightlyEvaluationStatsArtifact {
        path: format!("{report_id}/nightly_evaluation_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

// ── Pipeline (materialized artifacts) ────────────────────────────────────────

/// Configuration for the materialized nightly-evaluation pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct NightlyEvaluationPipelineConfig {
    /// Run directory name under the run-root.
    pub run_name: String,
    /// Run label used for deterministic ids.
    pub label: String,
    /// The evaluation config.
    pub evaluation: NightlyEvaluationConfig,
}

impl Default for NightlyEvaluationPipelineConfig {
    fn default() -> Self {
        Self {
            run_name: "nightly_evaluation".to_string(),
            label: "nightly-evaluation/e2e".to_string(),
            evaluation: NightlyEvaluationConfig::default(),
        }
    }
}

/// A materialized pipeline artifact (path + integrity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NightlyEvaluationArtifact {
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
pub struct NightlyEvaluationPipelineOutcome {
    /// Absolute run directory.
    pub run_dir: String,
    /// Absolute path to the JSONL evidence ledger.
    pub ledger_path: String,
    /// Absolute path to the history time-series JSON.
    pub history_path: String,
    /// Absolute path to the pipeline summary JSON.
    pub summary_path: String,
    /// Absolute path to the artifact manifest JSON.
    pub manifest_path: String,
    /// Absolute path to the JSON-stats artifact.
    pub stats_path: String,
    /// The machine-readable summary.
    pub summary: NightlyEvaluationSummary,
    /// All generated artifacts (with integrity hashes).
    pub artifacts: Vec<NightlyEvaluationArtifact>,
}

fn artifact_of(file: &str, content: &str) -> NightlyEvaluationArtifact {
    NightlyEvaluationArtifact {
        name: file
            .trim_end_matches(".json")
            .trim_end_matches(".jsonl")
            .to_string(),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// Run the nightly evaluation and materialize the full evidence pipeline under
/// `run_root/<run_name>/`.
///
/// # Errors
/// Returns an error if a run directory or artifact cannot be created/serialized.
pub fn run_nightly_evaluation_pipeline(
    run_root: &Path,
    config: &NightlyEvaluationPipelineConfig,
) -> crate::error::Result<NightlyEvaluationPipelineOutcome> {
    let report = NightlyEvaluation::new(config.label.as_str(), config.evaluation)
        .run(&default_nightly_fixtures());

    let ledger_content = report.render_ledger_jsonl();
    let history_content = serde_json::to_string_pretty(&report.history)?;
    let stats_content = report.exported_json_stats.content.clone();
    let summary_content = serde_json::to_string_pretty(&report.summary)?;

    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let ledger_file = "evidence_ledger.jsonl";
    let history_file = "history_timeseries.json";
    let stats_file = "nightly_evaluation_stats.json";
    let summary_file = "pipeline_summary.json";
    let manifest_file = "artifact_manifest.json";

    crate::util::write_string(&run_dir.join(ledger_file), &ledger_content)?;
    crate::util::write_string(&run_dir.join(history_file), &history_content)?;
    crate::util::write_string(&run_dir.join(stats_file), &stats_content)?;
    crate::util::write_string(&run_dir.join(summary_file), &summary_content)?;

    let artifacts = vec![
        artifact_of(ledger_file, &ledger_content),
        artifact_of(history_file, &history_content),
        artifact_of(stats_file, &stats_content),
        artifact_of(summary_file, &summary_content),
    ];

    #[derive(Serialize)]
    struct Manifest<'a> {
        schema_version: &'a str,
        run_name: &'a str,
        report_id: &'a str,
        gate_passes: bool,
        artifacts: &'a [NightlyEvaluationArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: NIGHTLY_EVALUATION_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        gate_passes: report.gate_passes,
        artifacts: &artifacts,
    })?;
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(NightlyEvaluationPipelineOutcome {
        run_dir: run_dir.display().to_string(),
        ledger_path: run_dir.join(ledger_file).display().to_string(),
        history_path: run_dir.join(history_file).display().to_string(),
        summary_path: run_dir.join(summary_file).display().to_string(),
        manifest_path: run_dir.join(manifest_file).display().to_string(),
        stats_path: run_dir.join(stats_file).display().to_string(),
        summary: report.summary,
        artifacts,
    })
}

// ── CLI ──────────────────────────────────────────────────────────────────────

/// CLI arguments for the `nightly-eval` command.
#[derive(Debug, clap::Args)]
pub struct NightlyEvalArgs {
    /// Run-root directory; artifacts land under `<run-root>/<run-name>/`.
    #[arg(
        long = "run-root",
        default_value = "/tmp/doctor_frankentui/nightly_evaluation"
    )]
    pub run_root: PathBuf,

    /// Run directory name.
    #[arg(long = "run-name", default_value = "nightly_evaluation")]
    pub run_name: String,

    /// Run label used for deterministic ids.
    #[arg(long = "label", default_value = "nightly-evaluation/e2e")]
    pub label: String,

    /// Number of deterministic shards.
    #[arg(long = "shard-count", default_value_t = 4)]
    pub shard_count: usize,
}

/// Run the `nightly-eval` command: materialize the pipeline and apply the
/// fail-closed gate.
///
/// # Errors
/// Returns [`crate::error::DoctorError::Exit`] with a non-zero code when the gate
/// fails (a missing field, an un-triaged failure, non-deterministic sharding, or
/// incomplete history), or an I/O error if artifacts cannot be materialized.
pub fn run_nightly_eval(args: NightlyEvalArgs) -> crate::error::Result<()> {
    let config = NightlyEvaluationPipelineConfig {
        run_name: args.run_name,
        label: args.label,
        evaluation: NightlyEvaluationConfig {
            shard_count: args.shard_count,
            ..NightlyEvaluationConfig::default()
        },
    };
    let outcome = run_nightly_evaluation_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("nightly continuous evaluation"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!(
            "fixtures: {} | optimized: {} | deferred: {} | failures: {} | shards: {}",
            summary.total_fixtures,
            summary.optimized,
            summary.deferred,
            summary.failures,
            summary.shards_used
        ));
        ui.info(&format!(
            "sharding deterministic: {} | failures triaged: {} | history complete: {}",
            summary.sharding_deterministic, summary.failures_triaged, summary.history_complete
        ));
        if summary.gate_passes {
            ui.success("nightly-eval gate PASSED");
        } else {
            ui.error("nightly-eval gate FAILED");
        }
    }

    if summary.gate_passes {
        Ok(())
    } else {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "nightly-eval gate failed: required_fields_complete={}, corpus_nonempty={}, red_path_exercised={}, failures_triaged={}, sharding_deterministic={}, history_complete={}",
                summary.required_fields_complete,
                summary.corpus_nonempty,
                summary.red_path_exercised,
                summary.failures_triaged,
                summary.sharding_deterministic,
                summary.history_complete
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn line<'a>(report: &'a NightlyEvaluationReport, fixture: &str) -> &'a NightlyLedgerEntry {
        report
            .ledger
            .iter()
            .find(|l| l.fixture_id == fixture)
            .expect("fixture present")
    }

    #[test]
    fn default_report_passes_gate() {
        let report = run_default_nightly_evaluation("ne/test");
        assert!(report.gate_passes, "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_fixtures, 6);
        assert!(report.summary.sharding_deterministic);
        assert!(report.summary.failures_triaged);
        assert!(report.summary.history_complete);
        assert_eq!(report.summary.history_points, 6);
        assert!(report.ledger.iter().all(required_fields_present));
    }

    #[test]
    fn empty_corpus_fails_closed() {
        let report = run_nightly_evaluation("ne/test", NightlyEvaluationConfig::default(), &[]);
        assert!(!report.summary.corpus_nonempty);
        assert!(
            !report.gate_passes,
            "an empty corpus must not pass the gate"
        );
    }

    #[test]
    fn never_failing_corpus_fails_red_path_clause() {
        // Strip the failing fixtures: with zero failures, `failures_triaged`
        // quantifies over an empty set — the red-path non-vacuity clause must
        // redden the gate instead of letting it pass vacuously (this is what
        // catches a drift/re-profile kernel that regressed to never-fail).
        let full = run_default_nightly_evaluation("ne/test");
        let failing: BTreeSet<String> = full
            .ledger
            .iter()
            .filter(|l| l.outcome_class.is_failure())
            .map(|l| l.fixture_id.clone())
            .collect();
        assert!(!failing.is_empty());
        let fixtures: Vec<NightlyFixture> = default_nightly_fixtures()
            .into_iter()
            .filter(|f| !failing.contains(&f.fixture_id))
            .collect();
        assert!(!fixtures.is_empty());
        let report =
            run_nightly_evaluation("ne/test", NightlyEvaluationConfig::default(), &fixtures);
        assert_eq!(report.summary.failures, 0);
        assert!(!report.summary.red_path_exercised);
        assert!(!report.gate_passes);
    }

    #[test]
    fn corpus_exercises_pass_and_triage_paths() {
        // Not vacuously green: at least one optimized, one deferred, and the two
        // failure classes are present and triaged.
        let report = run_default_nightly_evaluation("ne/test");
        assert!(report.summary.optimized >= 2);
        assert_eq!(report.summary.deferred, 1);
        assert_eq!(report.summary.failures, 2);
    }

    #[test]
    fn drift_regression_is_flagged_and_not_optimized() {
        let report = run_default_nightly_evaluation("ne/test");
        let l = line(&report, "f.alloc");
        assert_eq!(l.outcome_class, OutcomeClass::DriftRegression);
        assert!(l.drift_regressed);
        assert!(!l.reprofiled, "a drifting fixture must not be optimized");
        assert!(l.triage_hint.contains("rollback_ready="));
    }

    #[test]
    fn reprofile_pause_is_a_triaged_failure() {
        let report = run_default_nightly_evaluation("ne/test");
        let l = line(&report, "f.syscall");
        assert_eq!(l.outcome_class, OutcomeClass::ReprofilePause);
        assert_eq!(l.reprofile_verdict, "pause");
        assert!(l.reprofiled);
        assert!(!l.triage_hint.is_empty());
        assert!(l.triage_hint.contains("rollback_ready="));
    }

    #[test]
    fn low_voi_fixture_is_deferred_by_policy() {
        let report = run_default_nightly_evaluation("ne/test");
        let l = line(&report, "f.text");
        assert_eq!(l.outcome_class, OutcomeClass::Deferred);
        assert!(
            !l.scheduled,
            "a sub-floor VOI fixture must not be scheduled"
        );
        assert!(!l.reprofiled);
    }

    #[test]
    fn optimized_fixtures_record_efficacy_and_hotspot_movement() {
        let report = run_default_nightly_evaluation("ne/test");
        let render = line(&report, "f.render");
        assert_eq!(render.outcome_class, OutcomeClass::Optimized);
        assert_eq!(render.reprofile_verdict, "continue");
        // p99 120 -> 85 is a positive efficacy.
        assert_ne!(render.optimization_efficacy_pct, fmt6(0.0));
        // f.layout shifts its dominant hotspot render -> layout.
        let layout = line(&report, "f.layout");
        assert!(layout.bottleneck_shifted);
        assert_eq!(layout.bottleneck_before, "hot.render");
        assert_eq!(layout.bottleneck_after, "hot.layout");
    }

    #[test]
    fn sharding_is_deterministic_and_rederivable() {
        let report = run_default_nightly_evaluation("ne/test");
        for l in &report.ledger {
            assert_eq!(l.shard_index, shard_of(&l.fixture_id, 4));
            assert_eq!(l.shard_id, shard_id(l.shard_index));
        }
    }

    #[test]
    fn report_is_deterministic_and_replay_identical() {
        let a = run_default_nightly_evaluation("ne/test");
        let b = run_default_nightly_evaluation("ne/test");
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.ledger, b.ledger);
        assert_eq!(a.history, b.history);
        assert_eq!(a.render_ledger_jsonl(), b.render_ledger_jsonl());
    }

    #[test]
    fn run_is_independent_of_input_order() {
        let mut fixtures = default_nightly_fixtures();
        let forward =
            run_nightly_evaluation("ne/test", NightlyEvaluationConfig::default(), &fixtures);
        fixtures.reverse();
        let reversed =
            run_nightly_evaluation("ne/test", NightlyEvaluationConfig::default(), &fixtures);
        assert_eq!(forward.ledger, reversed.ledger);
        assert_eq!(forward.history, reversed.history);
    }

    #[test]
    fn triage_summaries_cover_all_failures() {
        let report = run_default_nightly_evaluation("ne/test");
        let triage = report.triage_summaries();
        assert_eq!(triage.len(), report.summary.failures);
        assert!(triage.iter().all(|t| !t.triage_hint.is_empty()));
    }

    #[test]
    fn pipeline_materializes_consistent_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = run_nightly_evaluation_pipeline(
            dir.path(),
            &NightlyEvaluationPipelineConfig::default(),
        )
        .unwrap();
        assert!(outcome.summary.gate_passes);
        assert_eq!(outcome.artifacts.len(), 4);
        for artifact in &outcome.artifacts {
            let path = std::path::Path::new(&outcome.run_dir).join(&artifact.file);
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(sha256_hex(&bytes), artifact.sha256);
        }
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = run_default_nightly_evaluation("ne/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}") {
            let first = run_default_nightly_evaluation(&label);
            let second = run_default_nightly_evaluation(&label);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.ledger, &second.ledger);
            prop_assert_eq!(&first.history, &second.history);
        }

        #[test]
        fn prop_gate_always_passes(label in "[a-z]{1,8}") {
            let report = run_default_nightly_evaluation(&label);
            prop_assert!(report.gate_passes);
            prop_assert!(report.summary.sharding_deterministic);
            prop_assert!(report.summary.failures_triaged);
        }
    }
}
