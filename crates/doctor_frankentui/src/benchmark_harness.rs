//! Deterministic benchmark harness for migration throughput and generated-app performance.
//!
//! The harness composes [`baseline_capture`](crate::baseline_capture) rather
//! than owning process execution. Callers feed warmup and measurement samples
//! from hyperfine, CI scripts, or controlled runners; this module normalizes
//! those samples by fixture/stage complexity, emits replayable artifacts, and
//! prepares before/after performance comparisons for optimization rounds.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::baseline_capture::{
    BaselineCaptureReport, BaselineCaptureRunner, BaselineCommand, BaselineManifest,
    BaselineRawMeasurement, BaselineRunInput, BaselineRunRecord, BaselineScenario,
};
use crate::performance_diff::{
    PerformanceDiffConfig, PerformanceDiffReport, PerformanceMetricKind, PerformanceRun,
    PerformanceSample, compare_performance_runs,
};

/// Schema version for benchmark harness reports.
pub const BENCHMARK_HARNESS_SCHEMA_VERSION: &str = "benchmark-harness-v1";

/// Fixture-size and complexity weights used for cross-fixture normalization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkNormalizationPolicy {
    pub input_kib_weight: f64,
    pub source_file_weight: f64,
    pub component_weight: f64,
    pub complexity_weight: f64,
    pub min_normalization_units: f64,
}

impl Default for BenchmarkNormalizationPolicy {
    fn default() -> Self {
        Self {
            input_kib_weight: 1.0,
            source_file_weight: 0.25,
            component_weight: 0.50,
            complexity_weight: 1.0,
            min_normalization_units: 1.0,
        }
    }
}

/// Static complexity profile for one benchmark fixture.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkFixtureProfile {
    pub fixture_id: String,
    pub corpus_id: String,
    pub input_bytes: u64,
    pub source_files: usize,
    pub component_count: usize,
    pub complexity_score: f64,
    pub tags: Vec<String>,
    pub fingerprint_sha256: String,
}

impl BenchmarkFixtureProfile {
    #[must_use]
    pub fn new(
        fixture_id: impl Into<String>,
        corpus_id: impl Into<String>,
        input_bytes: u64,
        source_files: usize,
        component_count: usize,
        complexity_score: f64,
        tags: Vec<String>,
    ) -> Self {
        let mut profile = Self {
            fixture_id: fixture_id.into(),
            corpus_id: corpus_id.into(),
            input_bytes,
            source_files,
            component_count,
            complexity_score,
            tags: sorted_unique(tags),
            fingerprint_sha256: String::new(),
        };
        profile.fingerprint_sha256 = stable_hash(&FixtureHashInput {
            fixture_id: profile.fixture_id.as_str(),
            corpus_id: profile.corpus_id.as_str(),
            input_bytes: profile.input_bytes,
            source_files: profile.source_files,
            component_count: profile.component_count,
            complexity_score: profile.complexity_score,
            tags: &profile.tags,
        });
        profile
    }

    #[must_use]
    pub fn normalization_units(&self, policy: &BenchmarkNormalizationPolicy) -> f64 {
        let input_kib = u64_to_f64(self.input_bytes) / 1024.0;
        let source_files = usize_to_f64(self.source_files);
        let components = usize_to_f64(self.component_count);
        let complexity = self.complexity_score.max(0.0);
        let units = (input_kib * policy.input_kib_weight)
            + (source_files * policy.source_file_weight)
            + (components * policy.component_weight)
            + (complexity * policy.complexity_weight);
        units.max(policy.min_normalization_units.max(f64::EPSILON))
    }
}

#[derive(Serialize)]
struct FixtureHashInput<'a> {
    fixture_id: &'a str,
    corpus_id: &'a str,
    input_bytes: u64,
    source_files: usize,
    component_count: usize,
    complexity_score: f64,
    tags: &'a [String],
}

/// Benchmark stage metadata for a migration or generated-app performance step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkStageProfile {
    pub stage_id: String,
    pub stage_group: String,
    pub operation_count: u64,
    pub expected_output_fingerprint: Option<String>,
}

impl BenchmarkStageProfile {
    #[must_use]
    pub fn new(
        stage_id: impl Into<String>,
        stage_group: impl Into<String>,
        operation_count: u64,
    ) -> Self {
        Self {
            stage_id: stage_id.into(),
            stage_group: stage_group.into(),
            operation_count,
            expected_output_fingerprint: None,
        }
    }

    #[must_use]
    pub fn with_expected_output_fingerprint(
        mut self,
        expected_output_fingerprint: impl Into<String>,
    ) -> Self {
        self.expected_output_fingerprint = Some(expected_output_fingerprint.into());
        self
    }
}

/// Planned fixture/stage command that can be turned into a baseline scenario.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkScenarioPlan {
    pub fixture: BenchmarkFixtureProfile,
    pub stage: BenchmarkStageProfile,
    pub scenario_id: String,
    pub workload_id: String,
    pub command: BaselineCommand,
    pub controlled_inputs: Vec<String>,
}

impl BenchmarkScenarioPlan {
    #[must_use]
    pub fn new(
        fixture: BenchmarkFixtureProfile,
        stage: BenchmarkStageProfile,
        scenario_id: impl Into<String>,
        workload_id: impl Into<String>,
        command: BaselineCommand,
        controlled_inputs: Vec<String>,
    ) -> Self {
        Self {
            fixture,
            stage,
            scenario_id: scenario_id.into(),
            workload_id: workload_id.into(),
            command,
            controlled_inputs: sorted_unique(controlled_inputs),
        }
    }

    #[must_use]
    pub fn baseline_scenario(&self) -> BaselineScenario {
        let mut controlled_inputs = self.controlled_inputs.clone();
        controlled_inputs.extend([
            format!(
                "fixture_fingerprint={}",
                self.fixture.fingerprint_sha256.as_str()
            ),
            format!("stage_group={}", self.stage.stage_group.as_str()),
        ]);
        if let Some(fingerprint) = &self.stage.expected_output_fingerprint {
            controlled_inputs.push(format!("expected_output={fingerprint}"));
        }

        BaselineScenario::new(
            self.stage.stage_id.clone(),
            self.fixture.fixture_id.clone(),
            self.scenario_id.clone(),
            self.workload_id.clone(),
            self.stage.operation_count,
            controlled_inputs,
            self.command.clone(),
        )
    }
}

/// Raw benchmark observations for one fixture/stage scenario.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkRunInput {
    pub run_id: String,
    pub scenario_plan: BenchmarkScenarioPlan,
    pub measurements: Vec<BaselineRawMeasurement>,
}

impl BenchmarkRunInput {
    #[must_use]
    pub fn new(
        run_id: impl Into<String>,
        scenario_plan: BenchmarkScenarioPlan,
        measurements: Vec<BaselineRawMeasurement>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            scenario_plan,
            measurements,
        }
    }
}

/// Harness configuration tied to one reproducible baseline manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkHarnessConfig {
    pub harness_id: String,
    pub baseline_manifest: BaselineManifest,
    pub normalization_policy: BenchmarkNormalizationPolicy,
    pub replay_command_prefix: String,
}

impl BenchmarkHarnessConfig {
    #[must_use]
    pub fn new(harness_id: impl Into<String>, baseline_manifest: BaselineManifest) -> Self {
        Self {
            harness_id: harness_id.into(),
            baseline_manifest,
            normalization_policy: BenchmarkNormalizationPolicy::default(),
            replay_command_prefix: "doctor_frankentui benchmark-replay".to_string(),
        }
    }

    #[must_use]
    pub fn with_normalization_policy(mut self, policy: BenchmarkNormalizationPolicy) -> Self {
        self.normalization_policy = policy;
        self
    }

    #[must_use]
    pub fn with_replay_command_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.replay_command_prefix = prefix.into();
        self
    }
}

/// Per-fixture normalized benchmark metrics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkNormalizedRecord {
    pub baseline_id: String,
    pub manifest_id: String,
    pub run_id: String,
    pub stage_id: String,
    pub fixture_id: String,
    pub scenario_id: String,
    pub workload_id: String,
    pub fixture_fingerprint: String,
    pub command_fingerprint: String,
    pub environment_fingerprint: String,
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
    pub raw_peak_rss_bytes: u64,
}

impl BenchmarkNormalizedRecord {
    #[must_use]
    pub fn optimization_ref(&self, candidate_id: impl Into<String>) -> BenchmarkOptimizationRef {
        BenchmarkOptimizationRef {
            candidate_id: candidate_id.into(),
            baseline_id: self.baseline_id.clone(),
            manifest_id: self.manifest_id.clone(),
            stage_id: self.stage_id.clone(),
            fixture_id: self.fixture_id.clone(),
            scenario_id: self.scenario_id.clone(),
            p50_ms: self.raw_latency_p50_ms,
            p95_ms: self.raw_latency_p95_ms,
            p99_ms: self.raw_latency_p99_ms,
            throughput_ops_per_sec: self.raw_throughput_ops_per_sec,
            peak_rss_bytes: self.raw_peak_rss_bytes,
            normalized_p99_ms_per_unit: self.latency_p99_ms_per_unit,
            normalized_throughput_ops_per_sec_per_unit: self.throughput_ops_per_sec_per_unit,
            command_fingerprint: self.command_fingerprint.clone(),
        }
    }
}

/// Baseline reference that optimization candidates attach to their reports.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkOptimizationRef {
    pub candidate_id: String,
    pub baseline_id: String,
    pub manifest_id: String,
    pub stage_id: String,
    pub fixture_id: String,
    pub scenario_id: String,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub throughput_ops_per_sec: f64,
    pub peak_rss_bytes: u64,
    pub normalized_p99_ms_per_unit: f64,
    pub normalized_throughput_ops_per_sec_per_unit: f64,
    pub command_fingerprint: String,
}

/// JSONL-ready evidence record for benchmark harness runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkEvidenceLog {
    pub benchmark_id: String,
    pub baseline_id: String,
    pub manifest_id: String,
    pub run_id: String,
    pub stage_id: String,
    pub fixture_id: String,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub throughput_ops_per_sec: f64,
    pub peak_rss_bytes: u64,
    pub normalization_units: f64,
    pub command_fingerprint: String,
    pub environment_fingerprint: String,
}

/// Replay manifest for deterministic benchmark reproduction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkReplayManifest {
    pub schema_version: String,
    pub benchmark_id: String,
    pub baseline_id: String,
    pub baseline_manifest_id: String,
    pub environment_fingerprint: String,
    pub replay_command: String,
    pub baseline_stats_path: String,
    pub baseline_stats_sha256: String,
    pub run_ids: Vec<String>,
    pub fixture_fingerprints: Vec<BenchmarkFixtureReplayRef>,
}

/// Fixture fingerprint included in replay manifests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BenchmarkFixtureReplayRef {
    pub fixture_id: String,
    pub stage_id: String,
    pub run_id: String,
    pub fixture_fingerprint: String,
    pub command_fingerprint: String,
}

/// Full benchmark harness report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkHarnessReport {
    pub schema_version: String,
    pub benchmark_id: String,
    pub baseline_report: BaselineCaptureReport,
    pub normalized_records: Vec<BenchmarkNormalizedRecord>,
    pub evidence_logs: Vec<BenchmarkEvidenceLog>,
    pub replay_manifest: BenchmarkReplayManifest,
    pub performance_run: PerformanceRun,
}

impl BenchmarkHarnessReport {
    #[must_use]
    pub fn optimization_refs(
        &self,
        candidate_id: impl Into<String>,
    ) -> Vec<BenchmarkOptimizationRef> {
        let candidate_id = candidate_id.into();
        self.normalized_records
            .iter()
            .map(|record| record.optimization_ref(candidate_id.clone()))
            .collect()
    }

    #[must_use]
    pub fn compare_candidate(
        &self,
        candidate: BenchmarkCandidateRun,
        config: &PerformanceDiffConfig,
    ) -> BenchmarkComparisonReport {
        if !candidate
            .baseline_id
            .as_str()
            .cmp(self.baseline_report.baseline_id.as_str())
            .is_eq()
        {
            return BenchmarkComparisonReport {
                schema_version: BENCHMARK_HARNESS_SCHEMA_VERSION.to_string(),
                candidate_id: candidate.candidate_id,
                baseline_id: self.baseline_report.baseline_id.clone(),
                candidate_baseline_id: candidate.baseline_id,
                baseline_id_matched: false,
                certification_passed: false,
                blocked_reason: Some(
                    "candidate baseline_id does not match benchmark baseline".to_string(),
                ),
                performance_report: None,
                replay_command: self.replay_manifest.replay_command.clone(),
            };
        }

        let performance_report =
            compare_performance_runs(&self.performance_run, &candidate.performance_run, config);
        let certification_passed = performance_report.certification_passed;
        BenchmarkComparisonReport {
            schema_version: BENCHMARK_HARNESS_SCHEMA_VERSION.to_string(),
            candidate_id: candidate.candidate_id,
            baseline_id: self.baseline_report.baseline_id.clone(),
            candidate_baseline_id: candidate.baseline_id,
            baseline_id_matched: true,
            certification_passed,
            blocked_reason: None,
            performance_report: Some(performance_report),
            replay_command: self.replay_manifest.replay_command.clone(),
        }
    }
}

/// Candidate performance run tied to a previously captured benchmark baseline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkCandidateRun {
    pub candidate_id: String,
    pub baseline_id: String,
    pub performance_run: PerformanceRun,
}

impl BenchmarkCandidateRun {
    #[must_use]
    pub fn new(
        candidate_id: impl Into<String>,
        baseline_id: impl Into<String>,
        performance_run: PerformanceRun,
    ) -> Self {
        Self {
            candidate_id: candidate_id.into(),
            baseline_id: baseline_id.into(),
            performance_run,
        }
    }
}

/// Before/after comparison result for one optimization candidate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkComparisonReport {
    pub schema_version: String,
    pub candidate_id: String,
    pub baseline_id: String,
    pub candidate_baseline_id: String,
    pub baseline_id_matched: bool,
    pub certification_passed: bool,
    pub blocked_reason: Option<String>,
    pub performance_report: Option<PerformanceDiffReport>,
    pub replay_command: String,
}

/// Deterministic benchmark harness.
#[derive(Debug, Clone)]
pub struct BenchmarkHarness {
    config: BenchmarkHarnessConfig,
}

impl BenchmarkHarness {
    #[must_use]
    pub fn new(config: BenchmarkHarnessConfig) -> Self {
        Self { config }
    }

    #[must_use]
    pub fn capture(&self, inputs: Vec<BenchmarkRunInput>) -> BenchmarkHarnessReport {
        let scenario_plans = inputs
            .iter()
            .map(|input| (input.run_id.clone(), input.scenario_plan.clone()))
            .collect::<BTreeMap<_, _>>();
        let baseline_inputs = inputs
            .into_iter()
            .map(|input| {
                BaselineRunInput::new(
                    input.run_id,
                    input.scenario_plan.baseline_scenario(),
                    input.measurements,
                )
            })
            .collect::<Vec<_>>();
        let baseline_report = BaselineCaptureRunner::new(self.config.baseline_manifest.clone())
            .capture(baseline_inputs);
        let normalized_records = normalized_records(
            &baseline_report,
            &scenario_plans,
            &self.config.normalization_policy,
        );
        let evidence_logs = benchmark_evidence_logs(
            &self.config.harness_id,
            &baseline_report,
            &normalized_records,
        );
        let replay_manifest = replay_manifest(&self.config, &baseline_report, &normalized_records);
        let performance_run = baseline_report.to_performance_run(format!(
            "{}:{}:source",
            self.config.harness_id, baseline_report.baseline_id
        ));

        BenchmarkHarnessReport {
            schema_version: BENCHMARK_HARNESS_SCHEMA_VERSION.to_string(),
            benchmark_id: self.config.harness_id.clone(),
            baseline_report,
            normalized_records,
            evidence_logs,
            replay_manifest,
            performance_run,
        }
    }
}

fn normalized_records(
    baseline_report: &BaselineCaptureReport,
    scenario_plans: &BTreeMap<String, BenchmarkScenarioPlan>,
    policy: &BenchmarkNormalizationPolicy,
) -> Vec<BenchmarkNormalizedRecord> {
    let mut records = baseline_report
        .records
        .iter()
        .filter_map(|record| {
            let plan = scenario_plans.get(&record.run_id)?;
            Some(normalized_record(baseline_report, record, plan, policy))
        })
        .collect::<Vec<_>>();
    records.sort_by(|left, right| {
        left.stage_id
            .cmp(&right.stage_id)
            .then_with(|| left.fixture_id.cmp(&right.fixture_id))
            .then_with(|| left.scenario_id.cmp(&right.scenario_id))
            .then_with(|| left.run_id.cmp(&right.run_id))
    });
    records
}

fn normalized_record(
    baseline_report: &BaselineCaptureReport,
    record: &BaselineRunRecord,
    plan: &BenchmarkScenarioPlan,
    policy: &BenchmarkNormalizationPolicy,
) -> BenchmarkNormalizedRecord {
    let normalization_units = plan.fixture.normalization_units(policy);
    BenchmarkNormalizedRecord {
        baseline_id: baseline_report.baseline_id.clone(),
        manifest_id: baseline_report.manifest.manifest_id.clone(),
        run_id: record.run_id.clone(),
        stage_id: record.stage_id.clone(),
        fixture_id: record.fixture_id.clone(),
        scenario_id: record.scenario_id.clone(),
        workload_id: record.workload_id.clone(),
        fixture_fingerprint: plan.fixture.fingerprint_sha256.clone(),
        command_fingerprint: record.command.fingerprint_sha256.clone(),
        environment_fingerprint: baseline_report
            .manifest
            .environment
            .fingerprint_sha256
            .clone(),
        normalization_units,
        latency_p50_ms_per_unit: record.summary.latency_p50_ms / normalization_units,
        latency_p95_ms_per_unit: record.summary.latency_p95_ms / normalization_units,
        latency_p99_ms_per_unit: record.summary.latency_p99_ms / normalization_units,
        throughput_ops_per_sec_per_unit: record.summary.throughput_ops_per_sec
            / normalization_units,
        peak_rss_bytes_per_unit: u64_to_f64(record.summary.peak_rss_bytes) / normalization_units,
        raw_latency_p50_ms: record.summary.latency_p50_ms,
        raw_latency_p95_ms: record.summary.latency_p95_ms,
        raw_latency_p99_ms: record.summary.latency_p99_ms,
        raw_throughput_ops_per_sec: record.summary.throughput_ops_per_sec,
        raw_peak_rss_bytes: record.summary.peak_rss_bytes,
    }
}

fn benchmark_evidence_logs(
    benchmark_id: &str,
    baseline_report: &BaselineCaptureReport,
    normalized_records: &[BenchmarkNormalizedRecord],
) -> Vec<BenchmarkEvidenceLog> {
    normalized_records
        .iter()
        .map(|record| BenchmarkEvidenceLog {
            benchmark_id: benchmark_id.to_string(),
            baseline_id: baseline_report.baseline_id.clone(),
            manifest_id: baseline_report.manifest.manifest_id.clone(),
            run_id: record.run_id.clone(),
            stage_id: record.stage_id.clone(),
            fixture_id: record.fixture_id.clone(),
            p50_ms: record.raw_latency_p50_ms,
            p95_ms: record.raw_latency_p95_ms,
            p99_ms: record.raw_latency_p99_ms,
            throughput_ops_per_sec: record.raw_throughput_ops_per_sec,
            peak_rss_bytes: record.raw_peak_rss_bytes,
            normalization_units: record.normalization_units,
            command_fingerprint: record.command_fingerprint.clone(),
            environment_fingerprint: record.environment_fingerprint.clone(),
        })
        .collect()
}

fn replay_manifest(
    config: &BenchmarkHarnessConfig,
    baseline_report: &BaselineCaptureReport,
    normalized_records: &[BenchmarkNormalizedRecord],
) -> BenchmarkReplayManifest {
    let mut fixture_fingerprints = normalized_records
        .iter()
        .map(|record| BenchmarkFixtureReplayRef {
            fixture_id: record.fixture_id.clone(),
            stage_id: record.stage_id.clone(),
            run_id: record.run_id.clone(),
            fixture_fingerprint: record.fixture_fingerprint.clone(),
            command_fingerprint: record.command_fingerprint.clone(),
        })
        .collect::<Vec<_>>();
    fixture_fingerprints.sort_by(|left, right| {
        left.stage_id
            .cmp(&right.stage_id)
            .then_with(|| left.fixture_id.cmp(&right.fixture_id))
            .then_with(|| left.run_id.cmp(&right.run_id))
    });
    let run_ids = fixture_fingerprints
        .iter()
        .map(|fixture| fixture.run_id.clone())
        .collect::<Vec<_>>();
    BenchmarkReplayManifest {
        schema_version: BENCHMARK_HARNESS_SCHEMA_VERSION.to_string(),
        benchmark_id: config.harness_id.clone(),
        baseline_id: baseline_report.baseline_id.clone(),
        baseline_manifest_id: baseline_report.manifest.manifest_id.clone(),
        environment_fingerprint: baseline_report
            .manifest
            .environment
            .fingerprint_sha256
            .clone(),
        replay_command: format!(
            "{} --benchmark-id {} --baseline-id {} --manifest-id {}",
            config.replay_command_prefix,
            config.harness_id,
            baseline_report.baseline_id,
            baseline_report.manifest.manifest_id
        ),
        baseline_stats_path: baseline_report.exported_json_stats.path.clone(),
        baseline_stats_sha256: baseline_report.exported_json_stats.sha256.clone(),
        run_ids,
        fixture_fingerprints,
    }
}

#[must_use]
pub fn benchmark_artifact_sha256(report: &BenchmarkHarnessReport) -> String {
    stable_hash(report)
}

#[must_use]
pub fn candidate_from_scaled_baseline(
    report: &BenchmarkHarnessReport,
    candidate_id: impl Into<String>,
    latency_scale: f64,
    throughput_scale: f64,
    memory_scale: f64,
) -> BenchmarkCandidateRun {
    let candidate_id = candidate_id.into();
    let samples = report
        .performance_run
        .samples
        .iter()
        .map(|sample| {
            scaled_sample(
                sample,
                &candidate_id,
                latency_scale,
                throughput_scale,
                memory_scale,
            )
        })
        .collect::<Vec<_>>();
    let performance_run = PerformanceRun::new(
        format!(
            "{}:{}",
            report.performance_run.run_id.as_str(),
            candidate_id.as_str()
        ),
        report.performance_run.workload_traces.clone(),
        samples,
    )
    .with_replay_command(format!(
        "doctor_frankentui benchmark-replay --candidate-id {} --baseline-id {}",
        candidate_id, report.baseline_report.baseline_id
    ));
    BenchmarkCandidateRun::new(
        candidate_id,
        report.baseline_report.baseline_id.clone(),
        performance_run,
    )
}

fn scaled_sample(
    sample: &PerformanceSample,
    candidate_id: &str,
    latency_scale: f64,
    throughput_scale: f64,
    memory_scale: f64,
) -> PerformanceSample {
    let scale = match sample.metric {
        PerformanceMetricKind::LatencyMeanMs
        | PerformanceMetricKind::LatencyP95Ms
        | PerformanceMetricKind::LatencyP99Ms
        | PerformanceMetricKind::FrameJitterMs
        | PerformanceMetricKind::DroppedFrameRatio => latency_scale,
        PerformanceMetricKind::ThroughputOpsPerSec => throughput_scale,
        PerformanceMetricKind::AllocationBytes => memory_scale,
    };
    PerformanceSample::new(
        sample.scenario_id.clone(),
        sample.metric,
        sample.sample_index,
        sample.value * scale,
        sample.deterministic_seed,
        sample.workload_id.clone(),
    )
    .with_artifact_id(candidate_id)
}

fn sorted_unique(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
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
        BaselineCorpusSlice, BaselineEnvironmentFingerprint, BaselineSeedPolicy,
        BaselineVarianceEnvelope,
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
                vec![
                    "fixture-dashboard".to_string(),
                    "fixture-editor".to_string(),
                ],
                vec![
                    "migration-throughput".to_string(),
                    "generated-app-render".to_string(),
                ],
                "fixed-seed-benchmark",
            ),
            BaselineSeedPolicy::new(99, "fixed")
                .with_replay_seed_material(vec!["opentui-bench".to_string()]),
            BaselineVarianceEnvelope {
                max_p99_relative_range: 0.20,
                max_throughput_relative_range: 0.20,
                min_measurement_runs: 5,
            },
        )
    }

    fn harness() -> BenchmarkHarness {
        BenchmarkHarness::new(BenchmarkHarnessConfig::new("opentui-benchmark", manifest()))
    }

    fn command(stage: &str, fixture: &str) -> BaselineCommand {
        BaselineCommand::new(
            "doctor_frankentui",
            vec![
                "bench".to_string(),
                stage.to_string(),
                "--fixture".to_string(),
                fixture.to_string(),
            ],
            "/repo",
            BTreeMap::new(),
        )
    }

    fn fixture(fixture_id: &str, input_bytes: u64, components: usize) -> BenchmarkFixtureProfile {
        BenchmarkFixtureProfile::new(
            fixture_id,
            "opentui-bench",
            input_bytes,
            12,
            components,
            3.5,
            vec!["typescript".to_string(), "interactive".to_string()],
        )
    }

    fn plan(stage_id: &str, fixture_id: &str) -> BenchmarkScenarioPlan {
        let fixture = fixture(fixture_id, 16_384, 8);
        let stage = BenchmarkStageProfile::new(stage_id, "opentui-import", 2_000)
            .with_expected_output_fingerprint("sha256:render-ok");
        BenchmarkScenarioPlan::new(
            fixture,
            stage,
            format!("{stage_id}:{fixture_id}"),
            format!("workload:{stage_id}:{fixture_id}"),
            command(stage_id, fixture_id),
            vec!["viewport=100x30".to_string(), "seed=99".to_string()],
        )
    }

    fn measurements(offset: f64) -> Vec<BaselineRawMeasurement> {
        let mut values = vec![
            BaselineRawMeasurement::warmup(0, 20.0 + offset),
            BaselineRawMeasurement::warmup(1, 19.8 + offset),
        ];
        values.extend([
            BaselineRawMeasurement::measurement(
                0,
                10.0 + offset,
                12.0 + offset,
                14.0 + offset,
                4_000.0 - offset,
                20_000_000,
            ),
            BaselineRawMeasurement::measurement(
                1,
                10.2 + offset,
                12.1 + offset,
                14.2 + offset,
                4_010.0 - offset,
                20_100_000,
            ),
            BaselineRawMeasurement::measurement(
                2,
                10.1 + offset,
                12.2 + offset,
                14.1 + offset,
                4_020.0 - offset,
                20_050_000,
            ),
            BaselineRawMeasurement::measurement(
                3,
                10.3 + offset,
                12.1 + offset,
                14.0 + offset,
                4_030.0 - offset,
                20_025_000,
            ),
            BaselineRawMeasurement::measurement(
                4,
                10.1 + offset,
                12.0 + offset,
                14.1 + offset,
                4_025.0 - offset,
                20_075_000,
            ),
        ]);
        values
    }

    fn inputs() -> Vec<BenchmarkRunInput> {
        vec![
            BenchmarkRunInput::new(
                "run-migration",
                plan("migration-throughput", "fixture-dashboard"),
                measurements(0.0),
            ),
            BenchmarkRunInput::new(
                "run-render",
                plan("generated-app-render", "fixture-editor"),
                measurements(1.0),
            ),
        ]
    }

    #[test]
    fn benchmark_harness_is_deterministic_for_fixed_inputs() {
        let harness = harness();
        let first = harness.capture(inputs());
        let second = harness.capture(inputs());

        assert!(
            first
                .baseline_report
                .baseline_id
                .as_str()
                .cmp(second.baseline_report.baseline_id.as_str())
                .is_eq()
        );
        assert!(
            benchmark_artifact_sha256(&first)
                .as_str()
                .cmp(benchmark_artifact_sha256(&second).as_str())
                .is_eq()
        );
        assert!(first.baseline_report.variance_violations.is_empty());
    }

    #[test]
    fn evidence_logs_record_metrics_and_fingerprints_per_fixture_stage() {
        let report = harness().capture(inputs());

        assert_eq!(report.evidence_logs.len(), 2);
        assert!(report.evidence_logs.iter().all(|log| {
            !log.run_id.is_empty()
                && !log.stage_id.is_empty()
                && !log.fixture_id.is_empty()
                && log.p50_ms > 0.0
                && log.p95_ms > 0.0
                && log.p99_ms > 0.0
                && log.throughput_ops_per_sec > 0.0
                && log.peak_rss_bytes > 0
                && !log.command_fingerprint.is_empty()
                && !log.environment_fingerprint.is_empty()
        }));
    }

    #[test]
    fn normalization_makes_fixture_complexity_explicit() {
        let small = fixture("small", 1_024, 1);
        let large = fixture("large", 65_536, 32);
        let policy = BenchmarkNormalizationPolicy::default();

        assert!(large.normalization_units(&policy) > small.normalization_units(&policy));

        let report = harness().capture(inputs());
        assert!(report.normalized_records.iter().all(|record| {
            record.normalization_units > 1.0
                && record.latency_p99_ms_per_unit < record.raw_latency_p99_ms
                && record.peak_rss_bytes_per_unit < u64_to_f64(record.raw_peak_rss_bytes)
        }));
    }

    #[test]
    fn replay_manifest_links_baseline_artifacts_and_commands() {
        let report = harness().capture(inputs());

        assert!(
            report
                .replay_manifest
                .baseline_id
                .as_str()
                .cmp(report.baseline_report.baseline_id.as_str())
                .is_eq()
        );
        assert!(
            report
                .replay_manifest
                .replay_command
                .contains("--baseline-id")
        );
        assert!(
            report
                .replay_manifest
                .baseline_stats_path
                .ends_with("baseline_stats.json")
        );
        assert_eq!(report.replay_manifest.fixture_fingerprints.len(), 2);
    }

    #[test]
    fn optimization_candidates_reference_baseline_metrics() {
        let report = harness().capture(inputs());
        let refs = report.optimization_refs("candidate-fast-render");

        assert_eq!(refs.len(), 2);
        assert!(refs.iter().all(|reference| {
            reference
                .baseline_id
                .as_str()
                .cmp(report.baseline_report.baseline_id.as_str())
                .is_eq()
                && reference.p50_ms > 0.0
                && reference.p95_ms > 0.0
                && reference.p99_ms > 0.0
                && reference.throughput_ops_per_sec > 0.0
                && reference.peak_rss_bytes > 0
                && !reference.command_fingerprint.is_empty()
        }));
    }

    #[test]
    fn before_after_candidate_comparison_uses_baseline_run() {
        let report = harness().capture(inputs());
        let candidate =
            candidate_from_scaled_baseline(&report, "candidate-fast-render", 0.90, 1.10, 0.95);
        let comparison = report.compare_candidate(candidate, &PerformanceDiffConfig::default());

        assert!(comparison.baseline_id_matched);
        assert!(comparison.certification_passed);
        assert!(comparison.performance_report.is_some());
    }

    #[test]
    fn candidate_baseline_mismatch_blocks_comparison() {
        let report = harness().capture(inputs());
        let candidate = BenchmarkCandidateRun::new(
            "candidate-wrong-baseline",
            "baseline-other",
            report.performance_run.clone(),
        );
        let comparison = report.compare_candidate(candidate, &PerformanceDiffConfig::default());

        assert!(!comparison.baseline_id_matched);
        assert!(!comparison.certification_passed);
        assert!(comparison.performance_report.is_none());
        assert!(comparison.blocked_reason.is_some());
    }
}
