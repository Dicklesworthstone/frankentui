//! E2E adaptive-scheduling + drift-alert pipeline (bd-3bxhj.8.13).
//!
//! This is the producer behind `scripts/doctor_frankentui_adaptive_schedule_e2e.sh`.
//! It compares the **adaptive** value-of-information schedule
//! ([`crate::test_budget_allocator`]) against the **static** round-robin baseline
//! on identical datasets, screens each dataset for drift
//! ([`crate::drift_monitor`]), and re-ranks the optimization backlog with a
//! re-profile round ([`crate::reprofile_loop`]):
//!
//! - **Adaptive vs static** (AC1): the allocator computes the confidence-gain
//!   per compute unit for the VOI schedule and for the round-robin baseline on the
//!   *same* candidates; the ledger reports both plus the percentile / memory
//!   tradeoff for the round.
//! - **Drift timeline + allocation decisions + hotspot movement +
//!   reprioritization** (AC2): each dataset records the drift event indices, the
//!   number of scheduled fixtures, whether the dominant hotspot shifted, and
//!   whether the backlog was reprioritized.
//! - **Confidence-per-compute gate** (AC3): the gate fails closed if the adaptive
//!   schedule *regresses* confidence-per-compute below the static baseline on any
//!   dataset (the core optimization-policy invariant), or if any record is missing
//!   a mandated field.
//!
//! The ledger is **float-free** (numeric terms are fixed-decimal strings via
//! [`fmt6`]), so it derives [`Eq`] and replays byte-identically. The pipeline is
//! materialized to disk (ledger + stats + summary + manifest) for the E2E gate.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::drift_monitor::{DriftMetricSeries, DriftMonitor, DriftMonitorConfig, MetricDirection};
use crate::recommendation_contract::EffortSize;
use crate::reprofile_loop::{
    BacklogCandidate, HotspotCost, OptimizationRound, ProfileSnapshot, ReprofileConfig,
    run_reprofile_loop,
};
use crate::test_budget_allocator::{TestBudgetAllocator, TestBudgetConfig, TestFixtureCandidate};

/// Schema version for the in-memory adaptive-schedule report.
pub const ADAPTIVE_SCHEDULE_SCHEMA_VERSION: &str = "adaptive-schedule-v1";

/// Schema version for the materialized adaptive-schedule pipeline artifacts.
pub const ADAPTIVE_SCHEDULE_PIPELINE_SCHEMA_VERSION: &str = "adaptive-schedule-pipeline-v1";

/// The VOI floor below which a fixture is not scheduled.
const VOI_FLOOR: f64 = 0.005;

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

/// Deterministic fixed-decimal rendering so the ledger stays float-free.
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

// ── Dataset input ──────────────────────────────────────────────────────────────

/// One candidate within a dataset (VOI inputs + a metric series for drift).
#[derive(Debug, Clone, PartialEq)]
pub struct DatasetCandidate {
    /// Fixture id.
    pub fixture_id: String,
    /// Stage id.
    pub stage_id: String,
    /// VOI compute cost.
    pub compute_cost: f64,
    /// VOI prior alpha.
    pub prior_alpha: f64,
    /// VOI prior beta.
    pub prior_beta: f64,
    /// VOI value scale.
    pub value_scale: f64,
    /// Latency observations (lower-is-better) for drift screening.
    pub metric_series: Vec<f64>,
}

/// One adaptive-scheduling dataset: a candidate set plus a representative
/// before/after profile pair and backlog for the re-profile round.
#[derive(Debug, Clone, PartialEq)]
pub struct AdaptiveDataset {
    /// Dataset id.
    pub dataset_id: String,
    /// The candidate set scheduled adaptively vs statically.
    pub candidates: Vec<DatasetCandidate>,
    /// The pre-change profile snapshot for the round.
    pub before: ProfileSnapshot,
    /// The post-change profile snapshot for the round.
    pub after: ProfileSnapshot,
    /// The backlog re-ranked by the re-profile round.
    pub backlog: Vec<BacklogCandidate>,
}

fn snapshot(
    p50: f64,
    p95: f64,
    p99: f64,
    throughput: f64,
    memory: f64,
    hotspots: &[(&str, f64)],
) -> ProfileSnapshot {
    ProfileSnapshot {
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
        throughput,
        memory_bytes: memory,
        hotspots: hotspots
            .iter()
            .map(|(id, share)| HotspotCost {
                hotspot_id: (*id).to_string(),
                cost_share: *share,
            })
            .collect(),
        stable: true,
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

fn cand(
    fixture: &str,
    stage: &str,
    cost: f64,
    alpha: f64,
    beta: f64,
    value: f64,
    series: Vec<f64>,
) -> DatasetCandidate {
    DatasetCandidate {
        fixture_id: fixture.to_string(),
        stage_id: stage.to_string(),
        compute_cost: cost,
        prior_alpha: alpha,
        prior_beta: beta,
        value_scale: value,
        metric_series: series,
    }
}

/// The default representative datasets: a balanced spread, a skewed spread where
/// adaptive scheduling strictly beats round-robin, and a drifting dataset whose
/// metric series triggers the drift monitor.
#[must_use]
pub fn default_datasets() -> Vec<AdaptiveDataset> {
    let flat: Vec<f64> = vec![
        10.0, 11.0, 9.0, 10.0, 11.0, 9.0, 10.0, 11.0, 9.0, 10.0, 11.0, 9.0,
    ];
    let drifting: Vec<f64> = vec![
        10.0, 11.0, 9.0, 10.0, 11.0, 9.0, 10.0, 11.0, 60.0, 62.0, 58.0, 61.0,
    ];
    vec![
        // Balanced: moderate VOI spread; adaptive is at least as good as static.
        AdaptiveDataset {
            dataset_id: "ds.balanced".to_string(),
            candidates: vec![
                cand("b.a", "render", 1.0, 2.0, 2.0, 1.0, flat.clone()),
                cand("b.b", "layout", 1.0, 3.0, 2.0, 1.0, flat.clone()),
                cand("b.c", "alloc", 1.0, 2.0, 3.0, 1.0, flat.clone()),
                cand("b.d", "text", 1.0, 2.0, 2.0, 1.0, flat.clone()),
            ],
            before: snapshot(
                40.0,
                90.0,
                120.0,
                10_000.0,
                5_000_000.0,
                &[("hot.render", 0.6), ("hot.layout", 0.4)],
            ),
            after: snapshot(
                34.0,
                74.0,
                92.0,
                11_000.0,
                4_700_000.0,
                &[("hot.render", 0.55), ("hot.layout", 0.45)],
            ),
            backlog: backlog(&[("cand.render", "hot.render"), ("cand.layout", "hot.layout")]),
        },
        // Skewed: one high-VOI fixture, three near-certain (sub-floor) ones; the
        // adaptive schedule concentrates budget -> strictly beats round-robin.
        AdaptiveDataset {
            dataset_id: "ds.skewed".to_string(),
            candidates: vec![
                cand("s.hot", "render", 1.0, 1.0, 1.0, 4.0, flat.clone()),
                cand("s.cold1", "layout", 1.0, 80.0, 2.0, 1.0, flat.clone()),
                cand("s.cold2", "alloc", 1.0, 80.0, 2.0, 1.0, flat.clone()),
                cand("s.cold3", "text", 1.0, 80.0, 2.0, 1.0, flat.clone()),
            ],
            before: snapshot(
                38.0,
                88.0,
                118.0,
                10_200.0,
                5_100_000.0,
                &[("hot.render", 0.6), ("hot.layout", 0.4)],
            ),
            after: snapshot(
                30.0,
                66.0,
                82.0,
                12_000.0,
                4_600_000.0,
                &[("hot.render", 0.3), ("hot.layout", 0.55)],
            ),
            backlog: backlog(&[("cand.render", "hot.render"), ("cand.layout", "hot.layout")]),
        },
        // Drifting: a metric series regresses -> the drift monitor populates a
        // timeline; adaptive scheduling is still at least as good as static.
        AdaptiveDataset {
            dataset_id: "ds.drifting".to_string(),
            candidates: vec![
                cand("d.a", "render", 1.0, 2.0, 2.0, 1.0, flat.clone()),
                cand("d.b", "layout", 1.0, 2.0, 2.0, 1.0, drifting),
                cand("d.c", "alloc", 1.0, 3.0, 2.0, 1.0, flat.clone()),
                cand("d.d", "vfx", 1.0, 2.0, 2.0, 1.0, flat),
            ],
            before: snapshot(
                50.0,
                120.0,
                160.0,
                8_000.0,
                6_000_000.0,
                &[("hot.vfx", 0.72)],
            ),
            after: snapshot(
                44.0,
                100.0,
                128.0,
                9_000.0,
                5_700_000.0,
                &[("hot.vfx", 0.7)],
            ),
            backlog: backlog(&[("cand.vfx", "hot.vfx")]),
        },
    ]
}

// ── Ledger ───────────────────────────────────────────────────────────────────

/// One adaptive-schedule ledger line (float-free; derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdaptiveLedgerEntry {
    /// Ledger schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// The dataset id.
    pub dataset_id: String,
    /// Total candidates in the dataset.
    pub total_candidates: usize,
    /// Number of fixtures the adaptive schedule selected (allocation decisions).
    pub scheduled_count: usize,
    /// Adaptive (VOI) confidence-gain per compute unit (fixed-decimal).
    pub adaptive_confidence_per_compute: String,
    /// Static (round-robin) confidence-gain per compute unit (fixed-decimal).
    pub static_confidence_per_compute: String,
    /// Adaptive / static improvement ratio (fixed-decimal).
    pub improvement_ratio: String,
    /// Whether adaptive is at least as good as static per compute (the AC3 gate).
    pub adaptive_at_least_static: bool,
    /// Number of drift events detected across the dataset.
    pub drift_event_count: usize,
    /// The drift timeline: the observation indices at which events fired (AC2).
    pub drift_timeline: Vec<usize>,
    /// The pre-change dominant hotspot.
    pub bottleneck_before: String,
    /// The post-change dominant hotspot (hotspot movement, AC2).
    pub bottleneck_after: String,
    /// Whether the dominant hotspot shifted.
    pub bottleneck_shifted: bool,
    /// Whether the optimization backlog was reprioritized (AC2).
    pub reprioritized: bool,
    /// p95 latency tradeoff delta after-before (fixed-decimal).
    pub p95_delta: String,
    /// p99 latency tradeoff delta after-before (fixed-decimal).
    pub p99_delta: String,
    /// Peak-memory tradeoff delta after-before (fixed-decimal).
    pub memory_delta: String,
    /// Whether the optimization-policy invariant held (adaptive not worse).
    pub policy_invariant_ok: bool,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic single-command replay reference.
    pub reproduction_command: String,
}

fn required_fields_present(line: &AdaptiveLedgerEntry) -> bool {
    !line.schema_version.is_empty()
        && !line.run_id.is_empty()
        && !line.dataset_id.is_empty()
        && !line.adaptive_confidence_per_compute.is_empty()
        && !line.static_confidence_per_compute.is_empty()
        && !line.improvement_ratio.is_empty()
        && !line.bottleneck_before.is_empty()
        && !line.bottleneck_after.is_empty()
        && !line.p95_delta.is_empty()
        && !line.p99_delta.is_empty()
        && !line.memory_delta.is_empty()
        && !line.detail.is_empty()
        && !line.reproduction_command.is_empty()
}

fn render_ledger_jsonl(ledger: &[AdaptiveLedgerEntry]) -> String {
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

// ── Dataset evaluation ─────────────────────────────────────────────────────────

fn evaluate_dataset(run_id: &str, label: &str, dataset: &AdaptiveDataset) -> AdaptiveLedgerEntry {
    // Adaptive vs static scheduling on the identical candidate set.
    let candidates: Vec<TestFixtureCandidate> = dataset
        .candidates
        .iter()
        .map(|c| {
            TestFixtureCandidate::new(c.fixture_id.as_str(), c.stage_id.as_str(), c.compute_cost)
                .with_prior(c.prior_alpha, c.prior_beta)
                .with_value_scale(c.value_scale)
        })
        .collect();
    let report = TestBudgetAllocator::new(TestBudgetConfig::new(100.0).with_voi_floor(VOI_FLOOR))
        .allocate(candidates);
    let comparison = &report.baseline_comparison;
    let scheduled_count = report.scheduled_fixture_ids().len();

    // Drift screening across the dataset's metric series.
    let series: Vec<DriftMetricSeries> = dataset
        .candidates
        .iter()
        .map(|c| {
            DriftMetricSeries::new(c.fixture_id.as_str(), MetricDirection::LowerIsBetter)
                .with_observations(c.metric_series.clone())
        })
        .collect();
    let drift = DriftMonitor::new(DriftMonitorConfig::default()).analyze(series);
    let mut drift_timeline: Vec<usize> = drift
        .events
        .iter()
        .map(|e| e.observation_index as usize)
        .collect();
    drift_timeline.sort_unstable();
    let drift_event_count = drift.events.len();

    // Re-profile round: hotspot movement + backlog reprioritization.
    let round = OptimizationRound {
        round_number: 1,
        change_id: format!("chg.{}", dataset.dataset_id),
        lever_id: format!("lever.{}", dataset.dataset_id),
        before: dataset.before.clone(),
        after: dataset.after.clone(),
        backlog: dataset.backlog.clone(),
    };
    let reprofile = run_reprofile_loop(
        &format!("{label}/{}", dataset.dataset_id),
        &[round],
        &ReprofileConfig::default(),
    );
    let round_entry = reprofile.round(1);
    let bottleneck_shifted = round_entry.is_some_and(|e| e.bottleneck_shifted);
    let reranked_first = round_entry
        .and_then(|e| e.reranked_backlog.first())
        .map(|c| c.candidate_id.clone());
    let input_first = dataset.backlog.first().map(|c| c.candidate_id.clone());
    let reprioritized =
        !dataset.backlog.is_empty() && (bottleneck_shifted || reranked_first != input_first);

    let p95_delta = dataset.after.p95_us - dataset.before.p95_us;
    let p99_delta = dataset.after.p99_us - dataset.before.p99_us;
    let memory_delta = dataset.after.memory_bytes - dataset.before.memory_bytes;

    let adaptive_at_least_static = comparison.voi_is_at_least_baseline;
    let policy_invariant_ok =
        adaptive_at_least_static && scheduled_count <= dataset.candidates.len();

    AdaptiveLedgerEntry {
        schema_version: ADAPTIVE_SCHEDULE_SCHEMA_VERSION.to_string(),
        run_id: run_id.to_string(),
        dataset_id: dataset.dataset_id.clone(),
        total_candidates: dataset.candidates.len(),
        scheduled_count,
        adaptive_confidence_per_compute: fmt6(report.confidence_gain_per_unit),
        static_confidence_per_compute: fmt6(comparison.baseline_confidence_gain_per_unit),
        improvement_ratio: fmt6(comparison.improvement_ratio),
        adaptive_at_least_static,
        drift_event_count,
        drift_timeline,
        bottleneck_before: dominant_hotspot(&dataset.before),
        bottleneck_after: dominant_hotspot(&dataset.after),
        bottleneck_shifted,
        reprioritized,
        p95_delta: fmt6(p95_delta),
        p99_delta: fmt6(p99_delta),
        memory_delta: fmt6(memory_delta),
        policy_invariant_ok,
        detail: format!(
            "adaptive cgpu {:.6} vs static {:.6} (ratio {:.4}); scheduled {}/{}; drift events {}; bottleneck {}->{}{}",
            report.confidence_gain_per_unit,
            comparison.baseline_confidence_gain_per_unit,
            comparison.improvement_ratio,
            scheduled_count,
            dataset.candidates.len(),
            drift_event_count,
            dominant_hotspot(&dataset.before),
            dominant_hotspot(&dataset.after),
            if bottleneck_shifted { " (shifted)" } else { "" },
        ),
        reproduction_command: format!(
            "cargo run -p doctor_frankentui -- adaptive-schedule --label '{label}' # run {run_id} dataset {}",
            dataset.dataset_id
        ),
    }
}

// ── Engine ───────────────────────────────────────────────────────────────────

/// The deterministic adaptive-schedule engine.
#[derive(Debug, Clone)]
pub struct AdaptiveSchedule {
    run_id: String,
    label: String,
}

impl AdaptiveSchedule {
    /// Construct an engine with a deterministic run id derived from its label.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        let run_id = format!(
            "adaptive-schedule-{}",
            short_hash(&stable_hash(&format!(
                "{ADAPTIVE_SCHEDULE_SCHEMA_VERSION}|{label}"
            )))
        );
        Self { run_id, label }
    }

    /// The deterministic run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Run the adaptive-schedule comparison over `datasets`.
    #[must_use]
    pub fn run(&self, datasets: &[AdaptiveDataset]) -> AdaptiveScheduleReport {
        let mut ordered: Vec<&AdaptiveDataset> = datasets.iter().collect();
        ordered.sort_by(|a, b| a.dataset_id.cmp(&b.dataset_id));

        let ledger: Vec<AdaptiveLedgerEntry> = ordered
            .iter()
            .map(|d| evaluate_dataset(&self.run_id, &self.label, d))
            .collect();

        let evidence_checksum = sha256_hex(render_ledger_jsonl(&ledger).as_bytes());
        let report_id = format!(
            "adaptive-schedule-report-{}",
            short_hash(&stable_hash(&format!(
                "{}|{evidence_checksum}",
                self.run_id
            )))
        );
        let summary = self.summarize(&ledger, &report_id, &evidence_checksum);
        let gate_passes = summary.gate_passes;
        let exported_json_stats = export_json_stats(&report_id, &summary, &ledger);
        let replay_command = format!(
            "cargo run -p doctor_frankentui -- adaptive-schedule --label '{}' # run {}",
            self.label, self.run_id
        );

        AdaptiveScheduleReport {
            schema_version: ADAPTIVE_SCHEDULE_SCHEMA_VERSION.to_string(),
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
        ledger: &[AdaptiveLedgerEntry],
        report_id: &str,
        evidence_checksum: &str,
    ) -> AdaptiveScheduleSummary {
        let required_fields_complete = ledger.iter().all(required_fields_present);
        let all_adaptive_at_least_static = ledger.iter().all(|l| l.adaptive_at_least_static);
        let all_policy_invariants_ok = ledger.iter().all(|l| l.policy_invariant_ok);
        let drift_exercised = ledger.iter().any(|l| l.drift_event_count > 0);
        let reprioritization_exercised = ledger.iter().any(|l| l.reprioritized);
        // Non-vacuous: at least one dataset where adaptive strictly beats static.
        let improvement_demonstrated = ledger
            .iter()
            .any(|l| l.improvement_ratio.parse::<f64>().unwrap_or(0.0) > 1.0 + 1e-9);
        let gate_passes = required_fields_complete
            && all_adaptive_at_least_static
            && all_policy_invariants_ok
            && drift_exercised
            && reprioritization_exercised
            && improvement_demonstrated;

        AdaptiveScheduleSummary {
            schema_version: ADAPTIVE_SCHEDULE_SCHEMA_VERSION.to_string(),
            report_id: report_id.to_string(),
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum: evidence_checksum.to_string(),
            total_datasets: ledger.len(),
            required_fields_complete,
            all_adaptive_at_least_static,
            all_policy_invariants_ok,
            drift_exercised,
            reprioritization_exercised,
            improvement_demonstrated,
            gate_passes,
            replay_command: format!(
                "cargo run -p doctor_frankentui -- adaptive-schedule --label '{}' # run {}",
                self.label, self.run_id
            ),
        }
    }
}

/// Run the adaptive-schedule comparison over `datasets`.
#[must_use]
pub fn run_adaptive_schedule(label: &str, datasets: &[AdaptiveDataset]) -> AdaptiveScheduleReport {
    AdaptiveSchedule::new(label).run(datasets)
}

/// Run the adaptive-schedule comparison over the default datasets.
#[must_use]
pub fn run_default_adaptive_schedule(label: &str) -> AdaptiveScheduleReport {
    run_adaptive_schedule(label, &default_datasets())
}

// ── Report + summary + stats ─────────────────────────────────────────────────

/// Machine-readable summary of one adaptive-schedule run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdaptiveScheduleSummary {
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
    /// Total datasets compared.
    pub total_datasets: usize,
    /// Whether every ledger line has all mandated fields.
    pub required_fields_complete: bool,
    /// Whether adaptive is at least as good as static on every dataset (AC3).
    pub all_adaptive_at_least_static: bool,
    /// Whether the optimization-policy invariant held on every dataset.
    pub all_policy_invariants_ok: bool,
    /// Whether at least one dataset exercised drift detection (AC2 timeline).
    pub drift_exercised: bool,
    /// Whether at least one dataset reprioritized its backlog (AC2).
    pub reprioritization_exercised: bool,
    /// Whether adaptive strictly beat static on at least one dataset (non-vacuous).
    pub improvement_demonstrated: bool,
    /// Whether the fail-closed gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdaptiveScheduleStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The in-memory adaptive-schedule report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdaptiveScheduleReport {
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
    pub ledger: Vec<AdaptiveLedgerEntry>,
    /// Aggregate summary.
    pub summary: AdaptiveScheduleSummary,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: AdaptiveScheduleStatsArtifact,
}

impl AdaptiveScheduleReport {
    /// Render the evidence ledger as JSONL (one entry per line).
    #[must_use]
    pub fn render_ledger_jsonl(&self) -> String {
        render_ledger_jsonl(&self.ledger)
    }
}

fn export_json_stats(
    report_id: &str,
    summary: &AdaptiveScheduleSummary,
    ledger: &[AdaptiveLedgerEntry],
) -> AdaptiveScheduleStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        summary: &'a AdaptiveScheduleSummary,
        ledger: &'a [AdaptiveLedgerEntry],
    }
    let content = serde_json::to_string_pretty(&Export {
        schema_version: ADAPTIVE_SCHEDULE_SCHEMA_VERSION,
        report_id,
        summary,
        ledger,
    })
    .unwrap_or_else(|error| error.to_string());
    AdaptiveScheduleStatsArtifact {
        path: format!("{report_id}/adaptive_schedule_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

// ── Pipeline (materialized artifacts) ────────────────────────────────────────

/// Configuration for the materialized adaptive-schedule pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct AdaptiveSchedulePipelineConfig {
    /// Run directory name under the run-root.
    pub run_name: String,
    /// Run label used for deterministic ids.
    pub label: String,
}

impl Default for AdaptiveSchedulePipelineConfig {
    fn default() -> Self {
        Self {
            run_name: "adaptive_schedule".to_string(),
            label: "adaptive-schedule/e2e".to_string(),
        }
    }
}

/// A materialized pipeline artifact (path + integrity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdaptiveScheduleArtifact {
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
pub struct AdaptiveSchedulePipelineOutcome {
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
    pub summary: AdaptiveScheduleSummary,
    /// All generated artifacts (with integrity hashes).
    pub artifacts: Vec<AdaptiveScheduleArtifact>,
}

fn artifact_of(file: &str, content: &str) -> AdaptiveScheduleArtifact {
    AdaptiveScheduleArtifact {
        name: file
            .trim_end_matches(".json")
            .trim_end_matches(".jsonl")
            .to_string(),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// Run the adaptive-schedule comparison and materialize the full evidence pipeline
/// under `run_root/<run_name>/`.
///
/// # Errors
/// Returns an error if a run directory or artifact cannot be created/serialized.
pub fn run_adaptive_schedule_pipeline(
    run_root: &Path,
    config: &AdaptiveSchedulePipelineConfig,
) -> crate::error::Result<AdaptiveSchedulePipelineOutcome> {
    let report = AdaptiveSchedule::new(config.label.as_str()).run(&default_datasets());

    let ledger_content = report.render_ledger_jsonl();
    let stats_content = report.exported_json_stats.content.clone();
    let summary_content = serde_json::to_string_pretty(&report.summary)?;

    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let ledger_file = "evidence_ledger.jsonl";
    let stats_file = "adaptive_schedule_stats.json";
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
        artifacts: &'a [AdaptiveScheduleArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: ADAPTIVE_SCHEDULE_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        gate_passes: report.gate_passes,
        artifacts: &artifacts,
    })?;
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(AdaptiveSchedulePipelineOutcome {
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

/// CLI arguments for the `adaptive-schedule` command.
#[derive(Debug, clap::Args)]
pub struct AdaptiveScheduleArgs {
    /// Run-root directory; artifacts land under `<run-root>/<run-name>/`.
    #[arg(
        long = "run-root",
        default_value = "/tmp/doctor_frankentui/adaptive_schedule"
    )]
    pub run_root: PathBuf,

    /// Run directory name.
    #[arg(long = "run-name", default_value = "adaptive_schedule")]
    pub run_name: String,

    /// Run label used for deterministic ids.
    #[arg(long = "label", default_value = "adaptive-schedule/e2e")]
    pub label: String,
}

/// Run the `adaptive-schedule` command: materialize the pipeline and apply the
/// confidence-per-compute gate.
///
/// # Errors
/// Returns [`crate::error::DoctorError::Exit`] with a non-zero code when the gate
/// fails (adaptive regresses confidence-per-compute, a policy invariant is
/// violated, or a field is missing), or an I/O error if artifacts cannot be
/// materialized.
pub fn run_adaptive_schedule_command(args: AdaptiveScheduleArgs) -> crate::error::Result<()> {
    let config = AdaptiveSchedulePipelineConfig {
        run_name: args.run_name,
        label: args.label,
    };
    let outcome = run_adaptive_schedule_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("adaptive scheduling + drift alert"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!(
            "datasets: {} | adaptive>=static: {} | drift exercised: {} | reprioritized: {}",
            summary.total_datasets,
            summary.all_adaptive_at_least_static,
            summary.drift_exercised,
            summary.reprioritization_exercised
        ));
        if summary.gate_passes {
            ui.success("adaptive-schedule gate PASSED (confidence-per-compute not regressed)");
        } else {
            ui.error("adaptive-schedule gate FAILED");
        }
    }

    if summary.gate_passes {
        Ok(())
    } else {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "adaptive-schedule gate failed: required_fields_complete={}, all_adaptive_at_least_static={}, all_policy_invariants_ok={}, drift_exercised={}, reprioritization_exercised={}, improvement_demonstrated={}",
                summary.required_fields_complete,
                summary.all_adaptive_at_least_static,
                summary.all_policy_invariants_ok,
                summary.drift_exercised,
                summary.reprioritization_exercised,
                summary.improvement_demonstrated
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn line<'a>(report: &'a AdaptiveScheduleReport, dataset: &str) -> &'a AdaptiveLedgerEntry {
        report
            .ledger
            .iter()
            .find(|l| l.dataset_id == dataset)
            .expect("dataset present")
    }

    #[test]
    fn default_report_passes_gate() {
        let report = run_default_adaptive_schedule("as/test");
        assert!(report.gate_passes, "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_datasets, 3);
        assert!(report.summary.all_adaptive_at_least_static);
        assert!(report.summary.all_policy_invariants_ok);
        assert!(report.summary.drift_exercised);
        assert!(report.summary.reprioritization_exercised);
        assert!(report.summary.improvement_demonstrated);
        assert!(report.ledger.iter().all(required_fields_present));
    }

    #[test]
    fn adaptive_never_regresses_confidence_per_compute() {
        // The core AC3 invariant on every dataset.
        let report = run_default_adaptive_schedule("as/test");
        for l in &report.ledger {
            assert!(
                l.adaptive_at_least_static,
                "dataset {} regressed confidence-per-compute",
                l.dataset_id
            );
            assert!(l.policy_invariant_ok);
        }
    }

    #[test]
    fn skewed_dataset_strictly_beats_static() {
        let report = run_default_adaptive_schedule("as/test");
        let l = line(&report, "ds.skewed");
        let ratio: f64 = l.improvement_ratio.parse().unwrap();
        assert!(
            ratio > 1.0,
            "skewed adaptive ratio {ratio} should exceed 1.0"
        );
        // The adaptive schedule concentrates budget on the single high-VOI fixture.
        assert!(l.scheduled_count < l.total_candidates);
    }

    #[test]
    fn drifting_dataset_populates_timeline() {
        let report = run_default_adaptive_schedule("as/test");
        let l = line(&report, "ds.drifting");
        assert!(l.drift_event_count > 0, "drift should be detected");
        assert!(!l.drift_timeline.is_empty());
        // The timeline indices land in the back half (after the baseline window).
        assert!(l.drift_timeline.iter().all(|&i| i >= 8));
    }

    #[test]
    fn skewed_dataset_reprioritizes_on_hotspot_shift() {
        let report = run_default_adaptive_schedule("as/test");
        let l = line(&report, "ds.skewed");
        assert!(l.bottleneck_shifted);
        assert_eq!(l.bottleneck_before, "hot.render");
        assert_eq!(l.bottleneck_after, "hot.layout");
        assert!(l.reprioritized);
    }

    #[test]
    fn report_is_deterministic_and_replay_identical() {
        let a = run_default_adaptive_schedule("as/test");
        let b = run_default_adaptive_schedule("as/test");
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.ledger, b.ledger);
        assert_eq!(a.render_ledger_jsonl(), b.render_ledger_jsonl());
    }

    #[test]
    fn run_is_independent_of_dataset_order() {
        let mut datasets = default_datasets();
        let forward = run_adaptive_schedule("as/test", &datasets);
        datasets.reverse();
        let reversed = run_adaptive_schedule("as/test", &datasets);
        assert_eq!(forward.ledger, reversed.ledger);
    }

    #[test]
    fn pipeline_materializes_consistent_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let outcome =
            run_adaptive_schedule_pipeline(dir.path(), &AdaptiveSchedulePipelineConfig::default())
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
        let report = run_default_adaptive_schedule("as/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}") {
            let first = run_default_adaptive_schedule(&label);
            let second = run_default_adaptive_schedule(&label);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.ledger, &second.ledger);
        }

        #[test]
        fn prop_gate_always_passes(label in "[a-z]{1,8}") {
            let report = run_default_adaptive_schedule(&label);
            prop_assert!(report.gate_passes);
            prop_assert!(report.summary.all_adaptive_at_least_static);
        }
    }
}
