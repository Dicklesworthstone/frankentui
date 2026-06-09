//! Regression gates and trend artifacts for benchmark harness output.
//!
//! This module consumes deterministic [`benchmark_harness`](crate::benchmark_harness)
//! reports. It does not execute benchmarks; callers provide candidate records,
//! hotspot evidence, and optional historical trend points.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::benchmark_harness::{BenchmarkHarnessReport, BenchmarkNormalizedRecord};

/// Schema version for benchmark regression gate reports.
pub const BENCHMARK_REGRESSION_GATE_SCHEMA_VERSION: &str = "benchmark-regression-gate-v1";

const DEFAULT_TREND_WINDOW_SIZE: usize = 5;
const DEFAULT_TREND_ANOMALY_RELATIVE_DELTA: f64 = 0.10;
const DEFAULT_OPPORTUNITY_SCORE: f64 = 2.0;
const EPSILON: f64 = 1.0e-9;

/// Fixture/stage profile key used for explicit threshold lookup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct BenchmarkProfileKey {
    pub stage_id: String,
    pub fixture_id: String,
}

impl BenchmarkProfileKey {
    #[must_use]
    pub fn new(stage_id: impl Into<String>, fixture_id: impl Into<String>) -> Self {
        Self {
            stage_id: stage_id.into(),
            fixture_id: fixture_id.into(),
        }
    }

    #[must_use]
    pub fn from_record(record: &BenchmarkNormalizedRecord) -> Self {
        Self::new(record.stage_id.clone(), record.fixture_id.clone())
    }

    #[must_use]
    pub fn profile_id(&self) -> String {
        format!("{}:{}", self.stage_id, self.fixture_id)
    }
}

/// Gate metric with explicit direction and units.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkGateMetric {
    LatencyP50Ms,
    LatencyP95Ms,
    LatencyP99Ms,
    ThroughputOpsPerSec,
    PeakRssBytes,
}

impl BenchmarkGateMetric {
    #[must_use]
    pub fn unit(self) -> &'static str {
        match self {
            Self::LatencyP50Ms | Self::LatencyP95Ms | Self::LatencyP99Ms => "ms",
            Self::ThroughputOpsPerSec => "ops/sec",
            Self::PeakRssBytes => "bytes",
        }
    }

    #[must_use]
    pub fn direction(self) -> BenchmarkMetricDirection {
        match self {
            Self::ThroughputOpsPerSec => BenchmarkMetricDirection::HigherIsBetter,
            Self::LatencyP50Ms | Self::LatencyP95Ms | Self::LatencyP99Ms | Self::PeakRssBytes => {
                BenchmarkMetricDirection::LowerIsBetter
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkMetricDirection {
    LowerIsBetter,
    HigherIsBetter,
}

/// Explicit gate thresholds for one fixture/stage profile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkProfileThresholds {
    pub profile: BenchmarkProfileKey,
    pub max_p50_relative_regression: f64,
    pub max_p95_relative_regression: f64,
    pub max_p99_relative_regression: f64,
    pub min_throughput_relative_retention: f64,
    pub max_peak_rss_relative_regression: f64,
    pub max_p95_ms_per_unit: Option<f64>,
    pub max_p99_ms_per_unit: Option<f64>,
    pub max_peak_rss_bytes_per_unit: Option<f64>,
    pub min_hotspot_opportunity_score: f64,
    pub linked_hotspot_ids: Vec<String>,
}

impl BenchmarkProfileThresholds {
    #[must_use]
    pub fn strict(profile: BenchmarkProfileKey) -> Self {
        Self {
            profile,
            max_p50_relative_regression: 0.05,
            max_p95_relative_regression: 0.08,
            max_p99_relative_regression: 0.10,
            min_throughput_relative_retention: 0.95,
            max_peak_rss_relative_regression: 0.10,
            max_p95_ms_per_unit: None,
            max_p99_ms_per_unit: None,
            max_peak_rss_bytes_per_unit: None,
            min_hotspot_opportunity_score: DEFAULT_OPPORTUNITY_SCORE,
            linked_hotspot_ids: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_p95_budget(mut self, max_p95_ms_per_unit: f64) -> Self {
        self.max_p95_ms_per_unit = Some(max_p95_ms_per_unit);
        self
    }

    #[must_use]
    pub fn with_p99_budget(mut self, max_p99_ms_per_unit: f64) -> Self {
        self.max_p99_ms_per_unit = Some(max_p99_ms_per_unit);
        self
    }

    #[must_use]
    pub fn with_peak_rss_budget(mut self, max_peak_rss_bytes_per_unit: f64) -> Self {
        self.max_peak_rss_bytes_per_unit = Some(max_peak_rss_bytes_per_unit);
        self
    }

    #[must_use]
    pub fn with_min_hotspot_score(mut self, min_hotspot_opportunity_score: f64) -> Self {
        self.min_hotspot_opportunity_score = min_hotspot_opportunity_score;
        self
    }

    #[must_use]
    pub fn with_linked_hotspots(mut self, linked_hotspot_ids: Vec<String>) -> Self {
        self.linked_hotspot_ids = sorted_unique(linked_hotspot_ids);
        self
    }
}

/// Regression gate configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkRegressionGateConfig {
    pub gate_id: String,
    pub profile_thresholds: Vec<BenchmarkProfileThresholds>,
    pub trend_window_size: usize,
    pub trend_anomaly_relative_delta: f64,
    pub require_profile_thresholds: bool,
}

impl BenchmarkRegressionGateConfig {
    #[must_use]
    pub fn new(
        gate_id: impl Into<String>,
        profile_thresholds: Vec<BenchmarkProfileThresholds>,
    ) -> Self {
        Self {
            gate_id: gate_id.into(),
            profile_thresholds,
            trend_window_size: DEFAULT_TREND_WINDOW_SIZE,
            trend_anomaly_relative_delta: DEFAULT_TREND_ANOMALY_RELATIVE_DELTA,
            require_profile_thresholds: true,
        }
    }

    #[must_use]
    pub fn with_trend_window_size(mut self, trend_window_size: usize) -> Self {
        self.trend_window_size = trend_window_size.max(2);
        self
    }

    #[must_use]
    pub fn with_trend_anomaly_relative_delta(mut self, relative_delta: f64) -> Self {
        self.trend_anomaly_relative_delta = relative_delta.max(0.0);
        self
    }
}

/// Candidate benchmark summary for one fixture/stage scenario.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkCandidateRecord {
    pub candidate_id: String,
    pub stage_id: String,
    pub fixture_id: String,
    pub scenario_id: String,
    pub workload_id: String,
    pub fixture_fingerprint: String,
    pub command_fingerprint: String,
    pub normalization_units: f64,
    pub latency_p50_ms_per_unit: f64,
    pub latency_p95_ms_per_unit: f64,
    pub latency_p99_ms_per_unit: f64,
    pub throughput_ops_per_sec_per_unit: f64,
    pub peak_rss_bytes_per_unit: f64,
    pub raw_latency_p50_ms: f64,
    pub raw_latency_p95_ms: f64,
    pub raw_latency_p99_ms: f64,
    pub raw_throughput_ops_per_sec: f64,
    pub raw_peak_rss_bytes: f64,
}

/// Multipliers used to synthesize candidate records from a baseline report.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkCandidateScale {
    pub latency_scale: f64,
    pub throughput_scale: f64,
    pub memory_scale: f64,
}

impl BenchmarkCandidateScale {
    #[must_use]
    pub fn new(latency_scale: f64, throughput_scale: f64, memory_scale: f64) -> Self {
        Self {
            latency_scale,
            throughput_scale,
            memory_scale,
        }
    }
}

impl BenchmarkCandidateRecord {
    #[must_use]
    pub fn from_scaled_baseline(
        candidate_id: impl Into<String>,
        baseline: &BenchmarkNormalizedRecord,
        latency_scale: f64,
        throughput_scale: f64,
        memory_scale: f64,
    ) -> Self {
        let candidate_id = candidate_id.into();
        let normalization_units = baseline.normalization_units.max(EPSILON);
        let raw_peak_rss_bytes = u64_to_f64(baseline.raw_peak_rss_bytes) * memory_scale;
        let raw_latency_p50_ms = baseline.raw_latency_p50_ms * latency_scale;
        let raw_latency_p95_ms = baseline.raw_latency_p95_ms * latency_scale;
        let raw_latency_p99_ms = baseline.raw_latency_p99_ms * latency_scale;
        let raw_throughput_ops_per_sec = baseline.raw_throughput_ops_per_sec * throughput_scale;
        Self {
            candidate_id,
            stage_id: baseline.stage_id.clone(),
            fixture_id: baseline.fixture_id.clone(),
            scenario_id: baseline.scenario_id.clone(),
            workload_id: baseline.workload_id.clone(),
            fixture_fingerprint: baseline.fixture_fingerprint.clone(),
            command_fingerprint: baseline.command_fingerprint.clone(),
            normalization_units,
            latency_p50_ms_per_unit: raw_latency_p50_ms / normalization_units,
            latency_p95_ms_per_unit: raw_latency_p95_ms / normalization_units,
            latency_p99_ms_per_unit: raw_latency_p99_ms / normalization_units,
            throughput_ops_per_sec_per_unit: raw_throughput_ops_per_sec / normalization_units,
            peak_rss_bytes_per_unit: raw_peak_rss_bytes / normalization_units,
            raw_latency_p50_ms,
            raw_latency_p95_ms,
            raw_latency_p99_ms,
            raw_throughput_ops_per_sec,
            raw_peak_rss_bytes,
        }
    }

    #[must_use]
    pub fn profile(&self) -> BenchmarkProfileKey {
        BenchmarkProfileKey::new(self.stage_id.clone(), self.fixture_id.clone())
    }
}

/// Profiled hotspot evidence linked to a gate decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkHotspotObservation {
    pub hotspot_id: String,
    pub profile: BenchmarkProfileKey,
    pub symbol_or_path: String,
    pub rank: u32,
    pub self_time_percent: f64,
    pub opportunity_score: f64,
    pub evidence_artifact: String,
}

impl BenchmarkHotspotObservation {
    #[must_use]
    pub fn new(
        hotspot_id: impl Into<String>,
        profile: BenchmarkProfileKey,
        symbol_or_path: impl Into<String>,
        rank: u32,
        self_time_percent: f64,
        opportunity_score: f64,
        evidence_artifact: impl Into<String>,
    ) -> Self {
        Self {
            hotspot_id: hotspot_id.into(),
            profile,
            symbol_or_path: symbol_or_path.into(),
            rank,
            self_time_percent,
            opportunity_score,
            evidence_artifact: evidence_artifact.into(),
        }
    }
}

/// Candidate data evaluated by the gate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkCandidateSnapshot {
    pub candidate_id: String,
    pub baseline_id: String,
    pub revision_id: String,
    pub sequence: u64,
    pub records: Vec<BenchmarkCandidateRecord>,
    pub hotspot_observations: Vec<BenchmarkHotspotObservation>,
}

impl BenchmarkCandidateSnapshot {
    #[must_use]
    pub fn new(
        candidate_id: impl Into<String>,
        baseline_id: impl Into<String>,
        revision_id: impl Into<String>,
        sequence: u64,
        records: Vec<BenchmarkCandidateRecord>,
        hotspot_observations: Vec<BenchmarkHotspotObservation>,
    ) -> Self {
        let candidate_id = candidate_id.into();
        Self {
            candidate_id,
            baseline_id: baseline_id.into(),
            revision_id: revision_id.into(),
            sequence,
            records,
            hotspot_observations,
        }
    }

    #[must_use]
    pub fn from_scaled_baseline(
        report: &BenchmarkHarnessReport,
        candidate_id: impl Into<String>,
        revision_id: impl Into<String>,
        sequence: u64,
        scale: BenchmarkCandidateScale,
        hotspot_observations: Vec<BenchmarkHotspotObservation>,
    ) -> Self {
        let candidate_id = candidate_id.into();
        let records = report
            .normalized_records
            .iter()
            .map(|record| {
                BenchmarkCandidateRecord::from_scaled_baseline(
                    candidate_id.clone(),
                    record,
                    scale.latency_scale,
                    scale.throughput_scale,
                    scale.memory_scale,
                )
            })
            .collect();
        Self::new(
            candidate_id,
            report.baseline_report.baseline_id.clone(),
            revision_id,
            sequence,
            records,
            hotspot_observations,
        )
    }
}

/// Historical point used by dashboard trend artifacts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkTrendPoint {
    pub revision_id: String,
    pub run_id: String,
    pub sequence: u64,
    pub profile: BenchmarkProfileKey,
    pub latency_p50_ms_per_unit: f64,
    pub latency_p95_ms_per_unit: f64,
    pub latency_p99_ms_per_unit: f64,
    pub throughput_ops_per_sec_per_unit: f64,
    pub peak_rss_bytes_per_unit: f64,
    pub hotspot_ids: Vec<String>,
}

impl BenchmarkTrendPoint {
    #[must_use]
    pub fn from_baseline_record(
        record: &BenchmarkNormalizedRecord,
        revision_id: impl Into<String>,
        sequence: u64,
        hotspot_ids: Vec<String>,
    ) -> Self {
        Self {
            revision_id: revision_id.into(),
            run_id: record.run_id.clone(),
            sequence,
            profile: BenchmarkProfileKey::from_record(record),
            latency_p50_ms_per_unit: record.latency_p50_ms_per_unit,
            latency_p95_ms_per_unit: record.latency_p95_ms_per_unit,
            latency_p99_ms_per_unit: record.latency_p99_ms_per_unit,
            throughput_ops_per_sec_per_unit: record.throughput_ops_per_sec_per_unit,
            peak_rss_bytes_per_unit: record.peak_rss_bytes_per_unit,
            hotspot_ids: sorted_unique(hotspot_ids),
        }
    }

    #[must_use]
    pub fn from_candidate_record(
        record: &BenchmarkCandidateRecord,
        revision_id: impl Into<String>,
        sequence: u64,
        hotspot_ids: Vec<String>,
    ) -> Self {
        Self {
            revision_id: revision_id.into(),
            run_id: format!("candidate:{}", record.candidate_id),
            sequence,
            profile: record.profile(),
            latency_p50_ms_per_unit: record.latency_p50_ms_per_unit,
            latency_p95_ms_per_unit: record.latency_p95_ms_per_unit,
            latency_p99_ms_per_unit: record.latency_p99_ms_per_unit,
            throughput_ops_per_sec_per_unit: record.throughput_ops_per_sec_per_unit,
            peak_rss_bytes_per_unit: record.peak_rss_bytes_per_unit,
            hotspot_ids: sorted_unique(hotspot_ids),
        }
    }

    #[must_use]
    pub fn metric_value(&self, metric: BenchmarkGateMetric) -> f64 {
        match metric {
            BenchmarkGateMetric::LatencyP50Ms => self.latency_p50_ms_per_unit,
            BenchmarkGateMetric::LatencyP95Ms => self.latency_p95_ms_per_unit,
            BenchmarkGateMetric::LatencyP99Ms => self.latency_p99_ms_per_unit,
            BenchmarkGateMetric::ThroughputOpsPerSec => self.throughput_ops_per_sec_per_unit,
            BenchmarkGateMetric::PeakRssBytes => self.peak_rss_bytes_per_unit,
        }
    }
}

/// Per-metric gate result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkMetricGateResult {
    pub metric: BenchmarkGateMetric,
    pub unit: String,
    pub baseline_value: f64,
    pub candidate_value: f64,
    pub relative_regression: f64,
    pub threshold: f64,
    pub budget_observed_value: Option<f64>,
    pub budget_value: Option<f64>,
    pub passed: bool,
    pub message: String,
}

/// Score-gate evidence for profile-driven optimization discipline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkScoreGateContext {
    pub profile_id: String,
    pub min_required_score: f64,
    pub observed_best_score: Option<f64>,
    pub passed: bool,
    pub linked_hotspot_ids: Vec<String>,
    pub evidence_artifacts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkGateFailureKind {
    MissingProfileThreshold,
    MissingCandidateRecord,
    LatencyP50Regression,
    LatencyP95Regression,
    LatencyP99Regression,
    ThroughputRegression,
    PeakRssRegression,
    LatencyP95Budget,
    LatencyP99Budget,
    PeakRssBudget,
    MissingHotspotScore,
    TrendAnomaly,
}

/// Triage-oriented failure payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkGateFailure {
    pub failure_kind: BenchmarkGateFailureKind,
    pub profile_id: String,
    pub stage_id: String,
    pub fixture_id: String,
    pub scenario_id: Option<String>,
    pub metric: Option<BenchmarkGateMetric>,
    pub baseline_value: Option<f64>,
    pub candidate_value: Option<f64>,
    pub relative_regression: Option<f64>,
    pub threshold: Option<f64>,
    pub culprit_stage_hint: String,
    pub linked_hotspot_ids: Vec<String>,
    pub score_gate_context: BenchmarkScoreGateContext,
    pub message: String,
}

/// Per-profile decision in the report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkProfileGateDecision {
    pub profile: BenchmarkProfileKey,
    pub thresholds: Option<BenchmarkProfileThresholds>,
    pub passed: bool,
    pub compared_scenarios: Vec<String>,
    pub metric_results: Vec<BenchmarkMetricGateResult>,
    pub score_gate_context: BenchmarkScoreGateContext,
    pub culprit_stage_hint: String,
    pub failures: Vec<BenchmarkGateFailure>,
}

/// Moving-window metric trend for dashboard artifacts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkTrendWindow {
    pub profile_id: String,
    pub metric: BenchmarkGateMetric,
    pub unit: String,
    pub window_size: usize,
    pub previous_window_values: Vec<f64>,
    pub previous_moving_average: Option<f64>,
    pub latest_value: f64,
    pub relative_regression_vs_window: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkTrendAnomalySeverity {
    Warning,
    HardGate,
}

/// Highlighted trend anomaly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkTrendAnomaly {
    pub profile_id: String,
    pub stage_id: String,
    pub fixture_id: String,
    pub metric: BenchmarkGateMetric,
    pub latest_value: f64,
    pub previous_moving_average: f64,
    pub relative_regression_vs_window: f64,
    pub threshold: f64,
    pub severity: BenchmarkTrendAnomalySeverity,
    pub message: String,
}

/// Hotspot movement between the prior trend point and current candidate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BenchmarkHotspotShift {
    pub profile_id: String,
    pub stage_id: String,
    pub fixture_id: String,
    pub previous_hotspot_ids: Vec<String>,
    pub current_hotspot_ids: Vec<String>,
    pub entered_hotspot_ids: Vec<String>,
    pub exited_hotspot_ids: Vec<String>,
    pub message: String,
}

/// Dashboard-ready trend artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkTrendArtifact {
    pub schema_version: String,
    pub gate_id: String,
    pub window_size: usize,
    pub windows: Vec<BenchmarkTrendWindow>,
    pub anomalies: Vec<BenchmarkTrendAnomaly>,
    pub hotspot_shifts: Vec<BenchmarkHotspotShift>,
}

/// Full gate report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkRegressionGateReport {
    pub schema_version: String,
    pub gate_id: String,
    pub baseline_id: String,
    pub candidate_id: String,
    pub revision_id: String,
    pub passed: bool,
    pub baseline_id_matched: bool,
    pub compared_record_count: usize,
    pub profile_decisions: Vec<BenchmarkProfileGateDecision>,
    pub failures: Vec<BenchmarkGateFailure>,
    pub trend_artifact: BenchmarkTrendArtifact,
    pub replay_command: String,
    pub report_sha256: String,
}

/// Deterministic benchmark regression gate.
#[derive(Debug, Clone)]
pub struct BenchmarkRegressionGate {
    config: BenchmarkRegressionGateConfig,
}

impl BenchmarkRegressionGate {
    #[must_use]
    pub fn new(config: BenchmarkRegressionGateConfig) -> Self {
        Self { config }
    }

    #[must_use]
    pub fn evaluate(
        &self,
        baseline: &BenchmarkHarnessReport,
        candidate: &BenchmarkCandidateSnapshot,
        trend_history: Vec<BenchmarkTrendPoint>,
    ) -> BenchmarkRegressionGateReport {
        let thresholds = threshold_map(&self.config.profile_thresholds);
        let candidate_records = candidate_record_map(&candidate.records);
        let hotspot_map = hotspot_map(&candidate.hotspot_observations);
        let baseline_id_matched = baseline.baseline_report.baseline_id == candidate.baseline_id;
        let mut profile_decisions = Vec::new();
        let mut failures = Vec::new();

        for (profile, baseline_records) in records_by_profile(&baseline.normalized_records) {
            let profile_thresholds = thresholds.get(&profile).cloned();
            let profile_hotspots = hotspot_map.get(&profile).cloned().unwrap_or_default();
            let threshold_for_context = profile_thresholds
                .as_ref()
                .map(|value| value.min_hotspot_opportunity_score)
                .unwrap_or(DEFAULT_OPPORTUNITY_SCORE);
            let score_context = score_gate_context(
                &profile,
                threshold_for_context,
                &profile_thresholds,
                &profile_hotspots,
            );
            let culprit_hint = culprit_stage_hint(&profile, &profile_hotspots);
            let mut decision_failures = Vec::new();
            let mut metric_results = Vec::new();
            let mut compared_scenarios = Vec::new();

            if profile_thresholds.is_none() && self.config.require_profile_thresholds {
                decision_failures.push(BenchmarkGateFailure {
                    failure_kind: BenchmarkGateFailureKind::MissingProfileThreshold,
                    profile_id: profile.profile_id(),
                    stage_id: profile.stage_id.clone(),
                    fixture_id: profile.fixture_id.clone(),
                    scenario_id: None,
                    metric: None,
                    baseline_value: None,
                    candidate_value: None,
                    relative_regression: None,
                    threshold: None,
                    culprit_stage_hint: culprit_hint.clone(),
                    linked_hotspot_ids: score_context.linked_hotspot_ids.clone(),
                    score_gate_context: score_context.clone(),
                    message: format!(
                        "profile {} has no explicit regression thresholds",
                        profile.profile_id()
                    ),
                });
            }

            if !score_context.passed {
                decision_failures.push(BenchmarkGateFailure {
                    failure_kind: BenchmarkGateFailureKind::MissingHotspotScore,
                    profile_id: profile.profile_id(),
                    stage_id: profile.stage_id.clone(),
                    fixture_id: profile.fixture_id.clone(),
                    scenario_id: None,
                    metric: None,
                    baseline_value: None,
                    candidate_value: None,
                    relative_regression: None,
                    threshold: Some(score_context.min_required_score),
                    culprit_stage_hint: culprit_hint.clone(),
                    linked_hotspot_ids: score_context.linked_hotspot_ids.clone(),
                    score_gate_context: score_context.clone(),
                    message: format!(
                        "profile {} lacks hotspot opportunity score >= {:.2}",
                        profile.profile_id(),
                        score_context.min_required_score
                    ),
                });
            }

            if let Some(profile_policy) = &profile_thresholds {
                for baseline_record in baseline_records {
                    let key = ScenarioRecordKey::from_baseline(baseline_record);
                    match candidate_records.get(&key) {
                        Some(candidate_record) => {
                            compared_scenarios.push(baseline_record.scenario_id.clone());
                            let results =
                                compare_record(profile_policy, baseline_record, candidate_record);
                            for result in &results {
                                if !result.passed {
                                    decision_failures.push(metric_failure(
                                        &profile,
                                        &baseline_record.scenario_id,
                                        result,
                                        &culprit_hint,
                                        &score_context,
                                    ));
                                }
                            }
                            metric_results.extend(results);
                        }
                        None => {
                            decision_failures.push(BenchmarkGateFailure {
                                failure_kind: BenchmarkGateFailureKind::MissingCandidateRecord,
                                profile_id: profile.profile_id(),
                                stage_id: profile.stage_id.clone(),
                                fixture_id: profile.fixture_id.clone(),
                                scenario_id: Some(baseline_record.scenario_id.clone()),
                                metric: None,
                                baseline_value: None,
                                candidate_value: None,
                                relative_regression: None,
                                threshold: None,
                                culprit_stage_hint: culprit_hint.clone(),
                                linked_hotspot_ids: score_context.linked_hotspot_ids.clone(),
                                score_gate_context: score_context.clone(),
                                message: format!(
                                    "candidate {} is missing scenario {} for profile {}",
                                    candidate.candidate_id,
                                    baseline_record.scenario_id,
                                    profile.profile_id()
                                ),
                            });
                        }
                    }
                }
            }

            compared_scenarios.sort();
            compared_scenarios.dedup();
            let passed = decision_failures.is_empty();
            failures.extend(decision_failures.clone());
            profile_decisions.push(BenchmarkProfileGateDecision {
                profile,
                thresholds: profile_thresholds,
                passed,
                compared_scenarios,
                metric_results,
                score_gate_context: score_context,
                culprit_stage_hint: culprit_hint,
                failures: decision_failures,
            });
        }

        let trend_artifact = build_trend_artifact(
            &self.config,
            candidate,
            trend_history,
            &thresholds,
            &hotspot_map,
        );
        for anomaly in &trend_artifact.anomalies {
            if anomaly.severity == BenchmarkTrendAnomalySeverity::HardGate {
                let profile =
                    BenchmarkProfileKey::new(anomaly.stage_id.clone(), anomaly.fixture_id.clone());
                let hotspots = hotspot_map.get(&profile).cloned().unwrap_or_default();
                let threshold = thresholds.get(&profile).cloned();
                let score_context = score_gate_context(
                    &profile,
                    threshold
                        .as_ref()
                        .map(|value| value.min_hotspot_opportunity_score)
                        .unwrap_or(DEFAULT_OPPORTUNITY_SCORE),
                    &threshold,
                    &hotspots,
                );
                failures.push(BenchmarkGateFailure {
                    failure_kind: BenchmarkGateFailureKind::TrendAnomaly,
                    profile_id: anomaly.profile_id.clone(),
                    stage_id: anomaly.stage_id.clone(),
                    fixture_id: anomaly.fixture_id.clone(),
                    scenario_id: None,
                    metric: Some(anomaly.metric),
                    baseline_value: Some(anomaly.previous_moving_average),
                    candidate_value: Some(anomaly.latest_value),
                    relative_regression: Some(anomaly.relative_regression_vs_window),
                    threshold: Some(anomaly.threshold),
                    culprit_stage_hint: culprit_stage_hint(&profile, &hotspots),
                    linked_hotspot_ids: score_context.linked_hotspot_ids.clone(),
                    score_gate_context: score_context,
                    message: anomaly.message.clone(),
                });
            }
        }

        if !baseline_id_matched {
            failures.push(baseline_mismatch_failure(baseline, candidate));
        }

        let mut report = BenchmarkRegressionGateReport {
            schema_version: BENCHMARK_REGRESSION_GATE_SCHEMA_VERSION.to_string(),
            gate_id: self.config.gate_id.clone(),
            baseline_id: baseline.baseline_report.baseline_id.clone(),
            candidate_id: candidate.candidate_id.clone(),
            revision_id: candidate.revision_id.clone(),
            passed: baseline_id_matched && failures.is_empty(),
            baseline_id_matched,
            compared_record_count: profile_decisions
                .iter()
                .map(|decision| decision.compared_scenarios.len())
                .sum(),
            profile_decisions,
            failures,
            trend_artifact,
            replay_command: baseline.replay_manifest.replay_command.clone(),
            report_sha256: String::new(),
        };
        report.report_sha256 = report_sha256(&report);
        report
    }
}

#[must_use]
pub fn regression_gate_report_sha256(report: &BenchmarkRegressionGateReport) -> String {
    report_sha256(report)
}

fn compare_record(
    thresholds: &BenchmarkProfileThresholds,
    baseline: &BenchmarkNormalizedRecord,
    candidate: &BenchmarkCandidateRecord,
) -> Vec<BenchmarkMetricGateResult> {
    vec![
        lower_is_better_result(
            BenchmarkGateMetric::LatencyP50Ms,
            baseline.raw_latency_p50_ms,
            candidate.raw_latency_p50_ms,
            thresholds.max_p50_relative_regression,
            None,
            None,
        ),
        lower_is_better_result(
            BenchmarkGateMetric::LatencyP95Ms,
            baseline.raw_latency_p95_ms,
            candidate.raw_latency_p95_ms,
            thresholds.max_p95_relative_regression,
            Some(candidate.latency_p95_ms_per_unit),
            thresholds.max_p95_ms_per_unit,
        ),
        lower_is_better_result(
            BenchmarkGateMetric::LatencyP99Ms,
            baseline.raw_latency_p99_ms,
            candidate.raw_latency_p99_ms,
            thresholds.max_p99_relative_regression,
            Some(candidate.latency_p99_ms_per_unit),
            thresholds.max_p99_ms_per_unit,
        ),
        higher_is_better_result(
            BenchmarkGateMetric::ThroughputOpsPerSec,
            baseline.raw_throughput_ops_per_sec,
            candidate.raw_throughput_ops_per_sec,
            thresholds.min_throughput_relative_retention,
        ),
        lower_is_better_result(
            BenchmarkGateMetric::PeakRssBytes,
            u64_to_f64(baseline.raw_peak_rss_bytes),
            candidate.raw_peak_rss_bytes,
            thresholds.max_peak_rss_relative_regression,
            Some(candidate.peak_rss_bytes_per_unit),
            thresholds.max_peak_rss_bytes_per_unit,
        ),
    ]
}

fn lower_is_better_result(
    metric: BenchmarkGateMetric,
    baseline_value: f64,
    candidate_value: f64,
    max_relative_regression: f64,
    budget_observed_value: Option<f64>,
    budget_value: Option<f64>,
) -> BenchmarkMetricGateResult {
    let relative_regression = relative_delta(candidate_value - baseline_value, baseline_value);
    let budget_failed = match (budget_observed_value, budget_value) {
        (Some(observed), Some(budget)) => observed > budget,
        _ => false,
    };
    let threshold_failed = relative_regression > max_relative_regression;
    let passed = !threshold_failed && !budget_failed;
    let budget_suffix = match (budget_observed_value, budget_value) {
        (Some(observed), Some(budget)) => {
            format!(", budget observed {:.4} limit {:.4}", observed, budget)
        }
        _ => String::new(),
    };
    let message = format!(
        "{metric:?}: baseline {:.4}, candidate {:.4}, relative regression {:.4}, threshold {:.4}{budget_suffix}, passed={passed}",
        baseline_value, candidate_value, relative_regression, max_relative_regression
    );
    BenchmarkMetricGateResult {
        metric,
        unit: metric.unit().to_string(),
        baseline_value,
        candidate_value,
        relative_regression,
        threshold: max_relative_regression,
        budget_observed_value,
        budget_value,
        passed,
        message,
    }
}

fn higher_is_better_result(
    metric: BenchmarkGateMetric,
    baseline_value: f64,
    candidate_value: f64,
    min_relative_retention: f64,
) -> BenchmarkMetricGateResult {
    let retention = if baseline_value.abs() <= EPSILON {
        1.0
    } else {
        candidate_value / baseline_value
    };
    let relative_regression = (1.0 - retention).max(0.0);
    let threshold = (1.0 - min_relative_retention).max(0.0);
    let passed = retention + EPSILON >= min_relative_retention;
    BenchmarkMetricGateResult {
        metric,
        unit: metric.unit().to_string(),
        baseline_value,
        candidate_value,
        relative_regression,
        threshold,
        budget_observed_value: Some(retention),
        budget_value: Some(min_relative_retention),
        passed,
        message: format!(
            "{metric:?}: candidate retention {:.4} vs required {:.4}",
            retention, min_relative_retention
        ),
    }
}

fn metric_failure(
    profile: &BenchmarkProfileKey,
    scenario_id: &str,
    result: &BenchmarkMetricGateResult,
    culprit_hint: &str,
    score_context: &BenchmarkScoreGateContext,
) -> BenchmarkGateFailure {
    BenchmarkGateFailure {
        failure_kind: failure_kind_for_metric(result),
        profile_id: profile.profile_id(),
        stage_id: profile.stage_id.clone(),
        fixture_id: profile.fixture_id.clone(),
        scenario_id: Some(scenario_id.to_string()),
        metric: Some(result.metric),
        baseline_value: Some(result.baseline_value),
        candidate_value: Some(result.candidate_value),
        relative_regression: Some(result.relative_regression),
        threshold: Some(result.threshold),
        culprit_stage_hint: culprit_hint.to_string(),
        linked_hotspot_ids: score_context.linked_hotspot_ids.clone(),
        score_gate_context: score_context.clone(),
        message: result.message.clone(),
    }
}

fn failure_kind_for_metric(result: &BenchmarkMetricGateResult) -> BenchmarkGateFailureKind {
    match result.metric {
        BenchmarkGateMetric::LatencyP50Ms => BenchmarkGateFailureKind::LatencyP50Regression,
        BenchmarkGateMetric::LatencyP95Ms => {
            if result
                .budget_value
                .map(|budget| {
                    result
                        .budget_observed_value
                        .map(|observed| observed > budget)
                        .unwrap_or(false)
                })
                .unwrap_or(false)
            {
                BenchmarkGateFailureKind::LatencyP95Budget
            } else {
                BenchmarkGateFailureKind::LatencyP95Regression
            }
        }
        BenchmarkGateMetric::LatencyP99Ms => {
            if result
                .budget_value
                .map(|budget| {
                    result
                        .budget_observed_value
                        .map(|observed| observed > budget)
                        .unwrap_or(false)
                })
                .unwrap_or(false)
            {
                BenchmarkGateFailureKind::LatencyP99Budget
            } else {
                BenchmarkGateFailureKind::LatencyP99Regression
            }
        }
        BenchmarkGateMetric::ThroughputOpsPerSec => BenchmarkGateFailureKind::ThroughputRegression,
        BenchmarkGateMetric::PeakRssBytes => {
            if result
                .budget_value
                .map(|budget| {
                    result
                        .budget_observed_value
                        .map(|observed| observed > budget)
                        .unwrap_or(false)
                })
                .unwrap_or(false)
            {
                BenchmarkGateFailureKind::PeakRssBudget
            } else {
                BenchmarkGateFailureKind::PeakRssRegression
            }
        }
    }
}

fn baseline_mismatch_failure(
    baseline: &BenchmarkHarnessReport,
    candidate: &BenchmarkCandidateSnapshot,
) -> BenchmarkGateFailure {
    let profile = candidate
        .records
        .first()
        .map(BenchmarkCandidateRecord::profile)
        .unwrap_or_else(|| BenchmarkProfileKey::new("unknown", "unknown"));
    let score_context = BenchmarkScoreGateContext {
        profile_id: profile.profile_id(),
        min_required_score: DEFAULT_OPPORTUNITY_SCORE,
        observed_best_score: None,
        passed: false,
        linked_hotspot_ids: Vec::new(),
        evidence_artifacts: Vec::new(),
    };
    BenchmarkGateFailure {
        failure_kind: BenchmarkGateFailureKind::MissingCandidateRecord,
        profile_id: profile.profile_id(),
        stage_id: profile.stage_id,
        fixture_id: profile.fixture_id,
        scenario_id: None,
        metric: None,
        baseline_value: None,
        candidate_value: None,
        relative_regression: None,
        threshold: None,
        culprit_stage_hint: "candidate baseline id must match replay manifest baseline".to_string(),
        linked_hotspot_ids: Vec::new(),
        score_gate_context: score_context,
        message: format!(
            "candidate baseline_id {} does not match benchmark baseline_id {}",
            candidate.baseline_id, baseline.baseline_report.baseline_id
        ),
    }
}

fn build_trend_artifact(
    config: &BenchmarkRegressionGateConfig,
    candidate: &BenchmarkCandidateSnapshot,
    mut history: Vec<BenchmarkTrendPoint>,
    thresholds: &BTreeMap<BenchmarkProfileKey, BenchmarkProfileThresholds>,
    hotspots: &BTreeMap<BenchmarkProfileKey, Vec<BenchmarkHotspotObservation>>,
) -> BenchmarkTrendArtifact {
    history.extend(candidate.records.iter().map(|record| {
        let profile = record.profile();
        let hotspot_ids = hotspots
            .get(&profile)
            .map(|items| items.iter().map(|item| item.hotspot_id.clone()).collect())
            .unwrap_or_default();
        BenchmarkTrendPoint::from_candidate_record(
            record,
            candidate.revision_id.clone(),
            candidate.sequence,
            hotspot_ids,
        )
    }));
    history.sort_by(|left, right| {
        left.profile
            .cmp(&right.profile)
            .then_with(|| left.sequence.cmp(&right.sequence))
            .then_with(|| left.revision_id.cmp(&right.revision_id))
            .then_with(|| left.run_id.cmp(&right.run_id))
    });

    let mut by_profile = BTreeMap::<BenchmarkProfileKey, Vec<BenchmarkTrendPoint>>::new();
    for point in history {
        by_profile
            .entry(point.profile.clone())
            .or_default()
            .push(point);
    }

    let mut windows = Vec::new();
    let mut anomalies = Vec::new();
    let mut hotspot_shifts = Vec::new();
    for (profile, mut points) in by_profile {
        points.sort_by(|left, right| {
            left.sequence
                .cmp(&right.sequence)
                .then_with(|| left.revision_id.cmp(&right.revision_id))
                .then_with(|| left.run_id.cmp(&right.run_id))
        });
        if let Some(shift) = hotspot_shift(&profile, &points) {
            hotspot_shifts.push(shift);
        }
        for metric in [
            BenchmarkGateMetric::LatencyP50Ms,
            BenchmarkGateMetric::LatencyP95Ms,
            BenchmarkGateMetric::LatencyP99Ms,
            BenchmarkGateMetric::ThroughputOpsPerSec,
            BenchmarkGateMetric::PeakRssBytes,
        ] {
            if let Some(window) = trend_window(&profile, &points, metric, config.trend_window_size)
            {
                if let Some(anomaly) = trend_anomaly(config, thresholds, &profile, metric, &window)
                {
                    anomalies.push(anomaly);
                }
                windows.push(window);
            }
        }
    }

    BenchmarkTrendArtifact {
        schema_version: BENCHMARK_REGRESSION_GATE_SCHEMA_VERSION.to_string(),
        gate_id: config.gate_id.clone(),
        window_size: config.trend_window_size,
        windows,
        anomalies,
        hotspot_shifts,
    }
}

fn trend_window(
    profile: &BenchmarkProfileKey,
    points: &[BenchmarkTrendPoint],
    metric: BenchmarkGateMetric,
    window_size: usize,
) -> Option<BenchmarkTrendWindow> {
    let latest = points.last()?;
    let previous_points = points
        .iter()
        .rev()
        .skip(1)
        .take(window_size)
        .map(|point| point.metric_value(metric))
        .collect::<Vec<_>>();
    let previous_window_values = previous_points.into_iter().rev().collect::<Vec<_>>();
    let latest_value = latest.metric_value(metric);
    let previous_moving_average = average(&previous_window_values);
    let relative_regression_vs_window = previous_moving_average
        .map(|average_value| relative_regression(metric.direction(), average_value, latest_value));
    Some(BenchmarkTrendWindow {
        profile_id: profile.profile_id(),
        metric,
        unit: metric.unit().to_string(),
        window_size,
        previous_window_values,
        previous_moving_average,
        latest_value,
        relative_regression_vs_window,
    })
}

fn trend_anomaly(
    config: &BenchmarkRegressionGateConfig,
    thresholds: &BTreeMap<BenchmarkProfileKey, BenchmarkProfileThresholds>,
    profile: &BenchmarkProfileKey,
    metric: BenchmarkGateMetric,
    window: &BenchmarkTrendWindow,
) -> Option<BenchmarkTrendAnomaly> {
    let previous = window.previous_moving_average?;
    let relative_regression = window.relative_regression_vs_window?;
    if relative_regression <= config.trend_anomaly_relative_delta {
        return None;
    }
    let hard_threshold = thresholds
        .get(profile)
        .map(|profile_thresholds| threshold_for_metric(profile_thresholds, metric))
        .unwrap_or(config.trend_anomaly_relative_delta);
    let severity = if relative_regression > hard_threshold {
        BenchmarkTrendAnomalySeverity::HardGate
    } else {
        BenchmarkTrendAnomalySeverity::Warning
    };
    Some(BenchmarkTrendAnomaly {
        profile_id: profile.profile_id(),
        stage_id: profile.stage_id.clone(),
        fixture_id: profile.fixture_id.clone(),
        metric,
        latest_value: window.latest_value,
        previous_moving_average: previous,
        relative_regression_vs_window: relative_regression,
        threshold: hard_threshold,
        severity,
        message: format!(
            "profile {} metric {:?} moved {:.4} worse than moving window",
            profile.profile_id(),
            metric,
            relative_regression
        ),
    })
}

fn hotspot_shift(
    profile: &BenchmarkProfileKey,
    points: &[BenchmarkTrendPoint],
) -> Option<BenchmarkHotspotShift> {
    let latest = points.last()?;
    let previous = points.iter().rev().skip(1).find(|point| {
        point.profile == *profile
            && (!point.hotspot_ids.is_empty() || !latest.hotspot_ids.is_empty())
    })?;
    let previous_set = previous
        .hotspot_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let current_set = latest.hotspot_ids.iter().cloned().collect::<BTreeSet<_>>();
    if previous_set == current_set {
        return None;
    }
    let entered_hotspot_ids = current_set
        .difference(&previous_set)
        .cloned()
        .collect::<Vec<_>>();
    let exited_hotspot_ids = previous_set
        .difference(&current_set)
        .cloned()
        .collect::<Vec<_>>();
    Some(BenchmarkHotspotShift {
        profile_id: profile.profile_id(),
        stage_id: profile.stage_id.clone(),
        fixture_id: profile.fixture_id.clone(),
        previous_hotspot_ids: previous.hotspot_ids.clone(),
        current_hotspot_ids: latest.hotspot_ids.clone(),
        entered_hotspot_ids,
        exited_hotspot_ids,
        message: format!(
            "profile {} hotspot set changed between {} and {}",
            profile.profile_id(),
            previous.revision_id,
            latest.revision_id
        ),
    })
}

fn threshold_for_metric(
    thresholds: &BenchmarkProfileThresholds,
    metric: BenchmarkGateMetric,
) -> f64 {
    match metric {
        BenchmarkGateMetric::LatencyP50Ms => thresholds.max_p50_relative_regression,
        BenchmarkGateMetric::LatencyP95Ms => thresholds.max_p95_relative_regression,
        BenchmarkGateMetric::LatencyP99Ms => thresholds.max_p99_relative_regression,
        BenchmarkGateMetric::ThroughputOpsPerSec => {
            (1.0 - thresholds.min_throughput_relative_retention).max(0.0)
        }
        BenchmarkGateMetric::PeakRssBytes => thresholds.max_peak_rss_relative_regression,
    }
}

fn score_gate_context(
    profile: &BenchmarkProfileKey,
    min_required_score: f64,
    thresholds: &Option<BenchmarkProfileThresholds>,
    hotspots: &[BenchmarkHotspotObservation],
) -> BenchmarkScoreGateContext {
    let threshold_hotspots = thresholds
        .as_ref()
        .map(|policy| policy.linked_hotspot_ids.clone())
        .unwrap_or_default();
    let observed_best_score = hotspots
        .iter()
        .map(|hotspot| hotspot.opportunity_score)
        .max_by(|left, right| left.total_cmp(right));
    let mut linked_hotspot_ids = hotspots
        .iter()
        .filter(|hotspot| {
            threshold_hotspots.is_empty() || threshold_hotspots.contains(&hotspot.hotspot_id)
        })
        .map(|hotspot| hotspot.hotspot_id.clone())
        .collect::<Vec<_>>();
    linked_hotspot_ids.extend(threshold_hotspots);
    linked_hotspot_ids = sorted_unique(linked_hotspot_ids);
    let evidence_artifacts = sorted_unique(
        hotspots
            .iter()
            .map(|hotspot| hotspot.evidence_artifact.clone())
            .collect(),
    );
    BenchmarkScoreGateContext {
        profile_id: profile.profile_id(),
        min_required_score,
        observed_best_score,
        passed: observed_best_score
            .map(|score| score + EPSILON >= min_required_score)
            .unwrap_or(min_required_score <= EPSILON),
        linked_hotspot_ids,
        evidence_artifacts,
    }
}

fn culprit_stage_hint(
    profile: &BenchmarkProfileKey,
    hotspots: &[BenchmarkHotspotObservation],
) -> String {
    if let Some(best) = hotspots
        .iter()
        .max_by(|left, right| left.opportunity_score.total_cmp(&right.opportunity_score))
    {
        format!(
            "stage {} fixture {}: inspect hotspot {} ({}, score {:.2})",
            profile.stage_id,
            profile.fixture_id,
            best.hotspot_id,
            best.symbol_or_path,
            best.opportunity_score
        )
    } else {
        format!(
            "stage {} fixture {}: no linked hotspot evidence; profile before changing thresholds",
            profile.stage_id, profile.fixture_id
        )
    }
}

fn threshold_map(
    thresholds: &[BenchmarkProfileThresholds],
) -> BTreeMap<BenchmarkProfileKey, BenchmarkProfileThresholds> {
    thresholds
        .iter()
        .cloned()
        .map(|threshold| (threshold.profile.clone(), threshold))
        .collect()
}

fn hotspot_map(
    hotspots: &[BenchmarkHotspotObservation],
) -> BTreeMap<BenchmarkProfileKey, Vec<BenchmarkHotspotObservation>> {
    let mut grouped = BTreeMap::<BenchmarkProfileKey, Vec<BenchmarkHotspotObservation>>::new();
    for hotspot in hotspots {
        grouped
            .entry(hotspot.profile.clone())
            .or_default()
            .push(hotspot.clone());
    }
    for values in grouped.values_mut() {
        values.sort_by(|left, right| {
            left.rank
                .cmp(&right.rank)
                .then_with(|| right.opportunity_score.total_cmp(&left.opportunity_score))
                .then_with(|| left.hotspot_id.cmp(&right.hotspot_id))
        });
    }
    grouped
}

fn records_by_profile(
    records: &[BenchmarkNormalizedRecord],
) -> BTreeMap<BenchmarkProfileKey, Vec<&BenchmarkNormalizedRecord>> {
    let mut grouped = BTreeMap::<BenchmarkProfileKey, Vec<&BenchmarkNormalizedRecord>>::new();
    for record in records {
        grouped
            .entry(BenchmarkProfileKey::from_record(record))
            .or_default()
            .push(record);
    }
    for values in grouped.values_mut() {
        values.sort_by(|left, right| {
            left.scenario_id
                .cmp(&right.scenario_id)
                .then_with(|| left.run_id.cmp(&right.run_id))
        });
    }
    grouped
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ScenarioRecordKey {
    stage_id: String,
    fixture_id: String,
    scenario_id: String,
}

impl ScenarioRecordKey {
    fn from_baseline(record: &BenchmarkNormalizedRecord) -> Self {
        Self {
            stage_id: record.stage_id.clone(),
            fixture_id: record.fixture_id.clone(),
            scenario_id: record.scenario_id.clone(),
        }
    }

    fn from_candidate(record: &BenchmarkCandidateRecord) -> Self {
        Self {
            stage_id: record.stage_id.clone(),
            fixture_id: record.fixture_id.clone(),
            scenario_id: record.scenario_id.clone(),
        }
    }
}

fn candidate_record_map(
    records: &[BenchmarkCandidateRecord],
) -> BTreeMap<ScenarioRecordKey, BenchmarkCandidateRecord> {
    records
        .iter()
        .cloned()
        .map(|record| (ScenarioRecordKey::from_candidate(&record), record))
        .collect()
}

fn relative_regression(
    direction: BenchmarkMetricDirection,
    baseline_value: f64,
    candidate_value: f64,
) -> f64 {
    match direction {
        BenchmarkMetricDirection::LowerIsBetter => {
            relative_delta(candidate_value - baseline_value, baseline_value)
        }
        BenchmarkMetricDirection::HigherIsBetter => {
            relative_delta(baseline_value - candidate_value, baseline_value)
        }
    }
}

fn relative_delta(delta: f64, baseline_value: f64) -> f64 {
    if baseline_value.abs() <= EPSILON {
        if delta <= 0.0 { 0.0 } else { f64::INFINITY }
    } else {
        (delta / baseline_value.abs()).max(0.0)
    }
}

fn average(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let sum = values.iter().sum::<f64>();
    Some(sum / usize_to_f64(values.len()))
}

fn sorted_unique(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn report_sha256(report: &BenchmarkRegressionGateReport) -> String {
    let mut stable_report = report.clone();
    stable_report.report_sha256.clear();
    stable_hash(&stable_report)
}

fn stable_hash<T: Serialize + ?Sized>(value: &T) -> String {
    let mut hasher = Sha256::new();
    match serde_json::to_vec(value) {
        Ok(bytes) => hasher.update(bytes),
        Err(error) => hasher.update(error.to_string().as_bytes()),
    }
    crate::util::hex_encode(&hasher.finalize())
}

fn u64_to_f64(value: u64) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(f64::MAX)
}

fn usize_to_f64(value: usize) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(f64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baseline_capture::{
        BaselineCommand, BaselineCorpusSlice, BaselineEnvironmentFingerprint, BaselineManifest,
        BaselineRawMeasurement, BaselineSeedPolicy, BaselineVarianceEnvelope,
    };
    use crate::benchmark_harness::{
        BenchmarkFixtureProfile, BenchmarkHarness, BenchmarkHarnessConfig, BenchmarkRunInput,
        BenchmarkScenarioPlan, BenchmarkStageProfile,
    };

    fn manifest() -> BaselineManifest {
        let mut tool_versions = BTreeMap::new();
        tool_versions.insert("cargo".to_string(), "1.90.0-nightly".to_string());
        tool_versions.insert("hyperfine".to_string(), "1.18.0".to_string());
        BaselineManifest::new(
            BaselineEnvironmentFingerprint::new(
                "linux",
                "x86_64",
                "ci-cpu",
                32,
                Some(64 * 1024 * 1024 * 1024),
                tool_versions,
                vec!["governor=performance".to_string()],
            ),
            BaselineCorpusSlice::new(
                "opentui-bench",
                vec!["fixture-dashboard".to_string()],
                vec!["migration-throughput".to_string()],
                "fixed-seed-benchmark",
            ),
            BaselineSeedPolicy::new(99, "fixed"),
            BaselineVarianceEnvelope {
                max_p99_relative_range: 0.25,
                max_throughput_relative_range: 0.25,
                min_measurement_runs: 5,
            },
        )
    }

    fn harness_report() -> BenchmarkHarnessReport {
        let fixture = BenchmarkFixtureProfile::new(
            "fixture-dashboard",
            "opentui-bench",
            16_384,
            12,
            8,
            3.5,
            vec!["interactive".to_string()],
        );
        let stage = BenchmarkStageProfile::new("migration-throughput", "opentui-import", 2_000);
        let command = BaselineCommand::new(
            "doctor_frankentui",
            vec![
                "bench".to_string(),
                "migration-throughput".to_string(),
                "--fixture".to_string(),
                "fixture-dashboard".to_string(),
            ],
            "/repo",
            BTreeMap::new(),
        );
        let plan = BenchmarkScenarioPlan::new(
            fixture,
            stage,
            "migration-throughput:fixture-dashboard",
            "workload:migration-throughput:fixture-dashboard",
            command,
            vec!["seed=99".to_string(), "viewport=100x30".to_string()],
        );
        let measurements = vec![
            BaselineRawMeasurement::measurement(0, 10.0, 20.0, 25.0, 110.0, 2_000_000),
            BaselineRawMeasurement::measurement(1, 10.5, 20.5, 25.5, 111.0, 2_050_000),
            BaselineRawMeasurement::measurement(2, 9.5, 19.5, 24.5, 109.0, 2_025_000),
            BaselineRawMeasurement::measurement(3, 10.2, 20.2, 25.2, 112.0, 2_020_000),
            BaselineRawMeasurement::measurement(4, 10.1, 20.1, 25.1, 110.5, 2_010_000),
        ];
        BenchmarkHarness::new(BenchmarkHarnessConfig::new("opentui-benchmark", manifest()))
            .capture(vec![BenchmarkRunInput::new("run-1", plan, measurements)])
    }

    fn profile(report: &BenchmarkHarnessReport) -> BenchmarkProfileKey {
        BenchmarkProfileKey::from_record(&report.normalized_records[0])
    }

    fn thresholds(report: &BenchmarkHarnessReport) -> BenchmarkProfileThresholds {
        BenchmarkProfileThresholds::strict(profile(report))
            .with_p95_budget(30.0)
            .with_p99_budget(35.0)
            .with_peak_rss_budget(3_000_000.0)
            .with_linked_hotspots(vec!["hot-render".to_string()])
    }

    fn hotspot(
        report: &BenchmarkHarnessReport,
        score: f64,
        id: &str,
    ) -> BenchmarkHotspotObservation {
        BenchmarkHotspotObservation::new(
            id,
            profile(report),
            "translate::render_loop",
            1,
            32.0,
            score,
            format!("profiles/{id}.json"),
        )
    }

    fn gate(report: &BenchmarkHarnessReport) -> BenchmarkRegressionGate {
        BenchmarkRegressionGate::new(
            BenchmarkRegressionGateConfig::new("gate-opentui", vec![thresholds(report)])
                .with_trend_window_size(3),
        )
    }

    fn scale(
        latency_scale: f64,
        throughput_scale: f64,
        memory_scale: f64,
    ) -> BenchmarkCandidateScale {
        BenchmarkCandidateScale::new(latency_scale, throughput_scale, memory_scale)
    }

    #[test]
    fn gate_passes_candidate_inside_explicit_thresholds() {
        let report = harness_report();
        let candidate = BenchmarkCandidateSnapshot::from_scaled_baseline(
            &report,
            "candidate-fast",
            "rev-fast",
            10,
            scale(1.01, 1.02, 1.01),
            vec![hotspot(&report, 3.0, "hot-render")],
        );

        let decision = gate(&report).evaluate(&report, &candidate, Vec::new());

        assert!(decision.passed);
        assert!(decision.baseline_id_matched);
        assert_eq!(decision.failures, Vec::new());
        assert_eq!(decision.profile_decisions.len(), 1);
        assert!(decision.profile_decisions[0].score_gate_context.passed);
    }

    #[test]
    fn gate_failures_include_culprit_hotspot_and_score_context() {
        let report = harness_report();
        let candidate = BenchmarkCandidateSnapshot::from_scaled_baseline(
            &report,
            "candidate-regressed",
            "rev-regressed",
            11,
            scale(1.30, 0.80, 1.40),
            vec![hotspot(&report, 3.2, "hot-render")],
        );

        let decision = gate(&report).evaluate(&report, &candidate, Vec::new());

        assert!(!decision.passed);
        assert!(decision.failures.iter().any(|failure| {
            failure.failure_kind == BenchmarkGateFailureKind::LatencyP95Regression
        }));
        assert!(decision.failures.iter().any(|failure| {
            failure.failure_kind == BenchmarkGateFailureKind::ThroughputRegression
        }));
        assert!(decision.failures.iter().any(|failure| {
            failure.failure_kind == BenchmarkGateFailureKind::PeakRssRegression
        }));
        for failure in &decision.failures {
            assert!(failure.culprit_stage_hint.contains("hot-render"));
            assert_eq!(failure.score_gate_context.observed_best_score, Some(3.2));
            assert!(
                failure
                    .linked_hotspot_ids
                    .contains(&"hot-render".to_string())
            );
        }
    }

    #[test]
    fn missing_hotspot_score_fails_optimization_policy_gate() {
        let report = harness_report();
        let candidate = BenchmarkCandidateSnapshot::from_scaled_baseline(
            &report,
            "candidate-no-profile",
            "rev-no-profile",
            12,
            scale(0.98, 1.01, 0.99),
            Vec::new(),
        );

        let decision = gate(&report).evaluate(&report, &candidate, Vec::new());

        assert!(!decision.passed);
        assert!(decision.failures.iter().any(|failure| {
            failure.failure_kind == BenchmarkGateFailureKind::MissingHotspotScore
        }));
        assert!(
            decision.failures[0]
                .culprit_stage_hint
                .contains("profile before changing thresholds")
        );
    }

    #[test]
    fn trend_artifact_records_moving_window_anomalies() {
        let report = harness_report();
        let baseline_record = &report.normalized_records[0];
        let historical_hotspots = vec!["hot-render".to_string()];
        let history = vec![
            BenchmarkTrendPoint::from_baseline_record(
                baseline_record,
                "rev-1",
                1,
                historical_hotspots.clone(),
            ),
            BenchmarkTrendPoint::from_baseline_record(
                baseline_record,
                "rev-2",
                2,
                historical_hotspots.clone(),
            ),
            BenchmarkTrendPoint::from_baseline_record(
                baseline_record,
                "rev-3",
                3,
                historical_hotspots,
            ),
        ];
        let candidate = BenchmarkCandidateSnapshot::from_scaled_baseline(
            &report,
            "candidate-tail-regressed",
            "rev-4",
            4,
            scale(1.18, 1.0, 1.0),
            vec![hotspot(&report, 3.5, "hot-render")],
        );

        let decision = gate(&report).evaluate(&report, &candidate, history);

        assert!(!decision.trend_artifact.windows.is_empty());
        assert!(decision.trend_artifact.anomalies.iter().any(|anomaly| {
            anomaly.metric == BenchmarkGateMetric::LatencyP95Ms
                && anomaly.severity == BenchmarkTrendAnomalySeverity::HardGate
        }));
        assert!(
            decision
                .failures
                .iter()
                .any(|failure| { failure.failure_kind == BenchmarkGateFailureKind::TrendAnomaly })
        );
    }

    #[test]
    fn trend_artifact_reports_hotspot_shift_context() {
        let report = harness_report();
        let baseline_record = &report.normalized_records[0];
        let history = vec![BenchmarkTrendPoint::from_baseline_record(
            baseline_record,
            "rev-1",
            1,
            vec!["old-hotspot".to_string()],
        )];
        let candidate = BenchmarkCandidateSnapshot::from_scaled_baseline(
            &report,
            "candidate-shift",
            "rev-2",
            2,
            scale(1.0, 1.0, 1.0),
            vec![hotspot(&report, 3.0, "hot-render")],
        );

        let decision = gate(&report).evaluate(&report, &candidate, history);

        assert_eq!(decision.trend_artifact.hotspot_shifts.len(), 1);
        let shift = &decision.trend_artifact.hotspot_shifts[0];
        assert_eq!(shift.entered_hotspot_ids, vec!["hot-render".to_string()]);
        assert_eq!(shift.exited_hotspot_ids, vec!["old-hotspot".to_string()]);
    }

    #[test]
    fn report_hash_is_deterministic_for_fixed_inputs() {
        let report = harness_report();
        let candidate = BenchmarkCandidateSnapshot::from_scaled_baseline(
            &report,
            "candidate-stable",
            "rev-stable",
            10,
            scale(1.0, 1.0, 1.0),
            vec![hotspot(&report, 3.0, "hot-render")],
        );

        let first = gate(&report).evaluate(&report, &candidate, Vec::new());
        let second = gate(&report).evaluate(&report, &candidate, Vec::new());

        assert_eq!(first, second);
        assert_eq!(first.report_sha256, regression_gate_report_sha256(&first));
    }
}
