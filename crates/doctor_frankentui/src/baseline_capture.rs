//! Deterministic performance baseline capture artifacts.
//!
//! This module turns hyperfine-style warmup and repeated measurement records
//! into stable baseline manifests, JSON stats, evidence logs, and
//! [`performance_diff`](crate::performance_diff)-compatible samples. Process
//! execution stays outside the library boundary so CI and E2E scripts can feed
//! observed resource measurements from their preferred runner.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::performance_diff::{
    PerformanceMetricKind, PerformanceRun, PerformanceSample, PerformanceWorkloadTrace,
};

/// Schema version for baseline capture manifests and reports.
pub const BASELINE_CAPTURE_SCHEMA_VERSION: &str = "baseline-capture-v1";

/// Reproducibility envelope for repeated baseline measurements.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BaselineVarianceEnvelope {
    /// Maximum relative range for repeated p99 observations before flagging.
    pub max_p99_relative_range: f64,
    /// Maximum relative range for throughput observations before flagging.
    pub max_throughput_relative_range: f64,
    /// Minimum non-warmup measurements required for a complete baseline.
    pub min_measurement_runs: usize,
}

impl Default for BaselineVarianceEnvelope {
    fn default() -> Self {
        Self {
            max_p99_relative_range: 0.10,
            max_throughput_relative_range: 0.10,
            min_measurement_runs: 5,
        }
    }
}

/// Deterministic seed policy recorded in every baseline manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BaselineSeedPolicy {
    pub deterministic_seed: u64,
    pub seed_family: String,
    pub replay_seed_material: Vec<String>,
}

impl BaselineSeedPolicy {
    #[must_use]
    pub fn new(deterministic_seed: u64, seed_family: impl Into<String>) -> Self {
        Self {
            deterministic_seed,
            seed_family: seed_family.into(),
            replay_seed_material: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_replay_seed_material(mut self, replay_seed_material: Vec<String>) -> Self {
        self.replay_seed_material = sorted_unique(replay_seed_material);
        self
    }
}

/// Host and tool fingerprint for reproducible baselines.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BaselineEnvironmentFingerprint {
    pub os: String,
    pub arch: String,
    pub cpu_model: String,
    pub cpu_cores: u32,
    pub memory_limit_bytes: Option<u64>,
    pub tool_versions: BTreeMap<String, String>,
    pub host_constraints: Vec<String>,
    pub fingerprint_sha256: String,
}

impl BaselineEnvironmentFingerprint {
    #[must_use]
    pub fn new(
        os: impl Into<String>,
        arch: impl Into<String>,
        cpu_model: impl Into<String>,
        cpu_cores: u32,
        memory_limit_bytes: Option<u64>,
        tool_versions: BTreeMap<String, String>,
        host_constraints: Vec<String>,
    ) -> Self {
        let mut env = Self {
            os: os.into(),
            arch: arch.into(),
            cpu_model: cpu_model.into(),
            cpu_cores,
            memory_limit_bytes,
            tool_versions,
            host_constraints: sorted_unique(host_constraints),
            fingerprint_sha256: String::new(),
        };
        env.fingerprint_sha256 = stable_hash(&EnvironmentHashInput {
            os: env.os.as_str(),
            arch: env.arch.as_str(),
            cpu_model: env.cpu_model.as_str(),
            cpu_cores: env.cpu_cores,
            memory_limit_bytes: env.memory_limit_bytes,
            tool_versions: &env.tool_versions,
            host_constraints: &env.host_constraints,
        });
        env
    }
}

#[derive(Serialize)]
struct EnvironmentHashInput<'a> {
    os: &'a str,
    arch: &'a str,
    cpu_model: &'a str,
    cpu_cores: u32,
    memory_limit_bytes: Option<u64>,
    tool_versions: &'a BTreeMap<String, String>,
    host_constraints: &'a [String],
}

/// Corpus slice covered by this baseline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BaselineCorpusSlice {
    pub corpus_id: String,
    pub fixture_ids: Vec<String>,
    pub stage_ids: Vec<String>,
    pub selection_policy: String,
}

impl BaselineCorpusSlice {
    #[must_use]
    pub fn new(
        corpus_id: impl Into<String>,
        fixture_ids: Vec<String>,
        stage_ids: Vec<String>,
        selection_policy: impl Into<String>,
    ) -> Self {
        Self {
            corpus_id: corpus_id.into(),
            fixture_ids: sorted_unique(fixture_ids),
            stage_ids: sorted_unique(stage_ids),
            selection_policy: selection_policy.into(),
        }
    }
}

/// Baseline manifest tying measurements to environment, corpus, and seed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BaselineManifest {
    pub schema_version: String,
    pub manifest_id: String,
    pub environment: BaselineEnvironmentFingerprint,
    pub corpus_slice: BaselineCorpusSlice,
    pub seed_policy: BaselineSeedPolicy,
    pub variance_envelope: BaselineVarianceEnvelope,
}

impl BaselineManifest {
    #[must_use]
    pub fn new(
        environment: BaselineEnvironmentFingerprint,
        corpus_slice: BaselineCorpusSlice,
        seed_policy: BaselineSeedPolicy,
        variance_envelope: BaselineVarianceEnvelope,
    ) -> Self {
        let manifest_seed = ManifestHashInput {
            schema_version: BASELINE_CAPTURE_SCHEMA_VERSION,
            environment: &environment,
            corpus_slice: &corpus_slice,
            seed_policy: &seed_policy,
            variance_envelope: &variance_envelope,
        };
        let manifest_id = format!(
            "baseline-manifest-{}",
            short_hash(&stable_hash(&manifest_seed))
        );
        Self {
            schema_version: BASELINE_CAPTURE_SCHEMA_VERSION.to_string(),
            manifest_id,
            environment,
            corpus_slice,
            seed_policy,
            variance_envelope,
        }
    }
}

#[derive(Serialize)]
struct ManifestHashInput<'a> {
    schema_version: &'a str,
    environment: &'a BaselineEnvironmentFingerprint,
    corpus_slice: &'a BaselineCorpusSlice,
    seed_policy: &'a BaselineSeedPolicy,
    variance_envelope: &'a BaselineVarianceEnvelope,
}

/// Command identity for one baseline scenario.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BaselineCommand {
    pub executable: String,
    pub args: Vec<String>,
    pub working_dir: String,
    pub env: BTreeMap<String, String>,
    pub fingerprint_sha256: String,
}

impl BaselineCommand {
    #[must_use]
    pub fn new(
        executable: impl Into<String>,
        args: Vec<String>,
        working_dir: impl Into<String>,
        env: BTreeMap<String, String>,
    ) -> Self {
        let executable = executable.into();
        let working_dir = working_dir.into();
        let fingerprint_sha256 = stable_hash(&CommandHashInput {
            executable: executable.as_str(),
            args: &args,
            working_dir: working_dir.as_str(),
            env: &env,
        });
        Self {
            executable,
            args,
            working_dir,
            env,
            fingerprint_sha256,
        }
    }

    #[must_use]
    pub fn shell_display(&self) -> String {
        std::iter::once(self.executable.clone())
            .chain(self.args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Serialize)]
struct CommandHashInput<'a> {
    executable: &'a str,
    args: &'a [String],
    working_dir: &'a str,
    env: &'a BTreeMap<String, String>,
}

/// One benchmark fixture/stage command to capture.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BaselineScenario {
    pub stage_id: String,
    pub fixture_id: String,
    pub scenario_id: String,
    pub workload_id: String,
    pub operation_count: u64,
    pub controlled_inputs: Vec<String>,
    pub command: BaselineCommand,
}

impl BaselineScenario {
    #[must_use]
    pub fn new(
        stage_id: impl Into<String>,
        fixture_id: impl Into<String>,
        scenario_id: impl Into<String>,
        workload_id: impl Into<String>,
        operation_count: u64,
        controlled_inputs: Vec<String>,
        command: BaselineCommand,
    ) -> Self {
        Self {
            stage_id: stage_id.into(),
            fixture_id: fixture_id.into(),
            scenario_id: scenario_id.into(),
            workload_id: workload_id.into(),
            operation_count,
            controlled_inputs: sorted_unique(controlled_inputs),
            command,
        }
    }

    #[must_use]
    pub fn workload_trace(&self, deterministic_seed: u64) -> PerformanceWorkloadTrace {
        PerformanceWorkloadTrace::new(
            self.workload_id.clone(),
            self.scenario_id.clone(),
            deterministic_seed,
            stable_hash(&ScenarioTraceHashInput {
                stage_id: self.stage_id.as_str(),
                fixture_id: self.fixture_id.as_str(),
                scenario_id: self.scenario_id.as_str(),
                command_fingerprint: self.command.fingerprint_sha256.as_str(),
                controlled_inputs: &self.controlled_inputs,
            }),
            self.operation_count,
        )
        .with_controlled_inputs(self.controlled_inputs.clone())
    }
}

#[derive(Serialize)]
struct ScenarioTraceHashInput<'a> {
    stage_id: &'a str,
    fixture_id: &'a str,
    scenario_id: &'a str,
    command_fingerprint: &'a str,
    controlled_inputs: &'a [String],
}

/// Warmup versus measured phase.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BaselineSamplePhase {
    Warmup,
    Measurement,
}

/// One observed benchmark run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BaselineRawMeasurement {
    pub sample_index: u32,
    pub phase: BaselineSamplePhase,
    pub latency_p50_ms: f64,
    pub latency_p95_ms: f64,
    pub latency_p99_ms: f64,
    pub throughput_ops_per_sec: f64,
    pub peak_rss_bytes: u64,
    pub cpu_user_ms: f64,
    pub cpu_system_ms: f64,
    pub syscall_count: u64,
}

impl BaselineRawMeasurement {
    #[must_use]
    pub fn measurement(
        sample_index: u32,
        latency_p50_ms: f64,
        latency_p95_ms: f64,
        latency_p99_ms: f64,
        throughput_ops_per_sec: f64,
        peak_rss_bytes: u64,
    ) -> Self {
        Self {
            sample_index,
            phase: BaselineSamplePhase::Measurement,
            latency_p50_ms,
            latency_p95_ms,
            latency_p99_ms,
            throughput_ops_per_sec,
            peak_rss_bytes,
            cpu_user_ms: 0.0,
            cpu_system_ms: 0.0,
            syscall_count: 0,
        }
    }

    #[must_use]
    pub fn warmup(sample_index: u32, latency_p99_ms: f64) -> Self {
        Self {
            sample_index,
            phase: BaselineSamplePhase::Warmup,
            latency_p50_ms: latency_p99_ms,
            latency_p95_ms: latency_p99_ms,
            latency_p99_ms,
            throughput_ops_per_sec: 0.0,
            peak_rss_bytes: 0,
            cpu_user_ms: 0.0,
            cpu_system_ms: 0.0,
            syscall_count: 0,
        }
    }
}

/// Raw input for one scenario capture.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BaselineRunInput {
    pub run_id: String,
    pub scenario: BaselineScenario,
    pub measurements: Vec<BaselineRawMeasurement>,
}

impl BaselineRunInput {
    #[must_use]
    pub fn new(
        run_id: impl Into<String>,
        scenario: BaselineScenario,
        measurements: Vec<BaselineRawMeasurement>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            scenario,
            measurements,
        }
    }
}

/// Deterministic summary of one baseline run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BaselineMetricSummary {
    pub measurement_runs: usize,
    pub warmup_runs: usize,
    pub latency_p50_ms: f64,
    pub latency_p95_ms: f64,
    pub latency_p99_ms: f64,
    pub throughput_ops_per_sec: f64,
    pub peak_rss_bytes: u64,
    pub cpu_user_ms: f64,
    pub cpu_system_ms: f64,
    pub syscall_count: u64,
    pub measurement_hash: String,
}

/// Resource/correctness issue discovered during baseline capture.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BaselineVarianceViolation {
    InsufficientMeasurements {
        run_id: String,
        observed: usize,
        required: usize,
    },
    P99Variance {
        run_id: String,
        observed_relative_range: f64,
        max_relative_range: f64,
    },
    ThroughputVariance {
        run_id: String,
        observed_relative_range: f64,
        max_relative_range: f64,
    },
}

/// Structured JSONL-ready evidence record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BaselineEvidenceLog {
    pub baseline_id: String,
    pub run_id: String,
    pub stage_id: String,
    pub fixture_id: String,
    pub scenario_id: String,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub throughput_ops_per_sec: f64,
    pub peak_rss_bytes: u64,
    pub command_fingerprint: String,
}

/// Reference attached to an optimization candidate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OptimizationBaselineRef {
    pub candidate_id: String,
    pub baseline_id: String,
    pub stage_id: String,
    pub fixture_id: String,
    pub scenario_id: String,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub throughput_ops_per_sec: f64,
    pub peak_rss_bytes: u64,
    pub command_fingerprint: String,
}

/// Captured record for one scenario.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BaselineRunRecord {
    pub baseline_id: String,
    pub run_id: String,
    pub stage_id: String,
    pub fixture_id: String,
    pub scenario_id: String,
    pub workload_id: String,
    pub deterministic_seed: u64,
    pub command: BaselineCommand,
    pub workload_trace: PerformanceWorkloadTrace,
    pub warmup_samples: Vec<BaselineRawMeasurement>,
    pub measurement_samples: Vec<BaselineRawMeasurement>,
    pub summary: BaselineMetricSummary,
    pub variance_violations: Vec<BaselineVarianceViolation>,
}

impl BaselineRunRecord {
    #[must_use]
    pub fn evidence_log(&self) -> BaselineEvidenceLog {
        BaselineEvidenceLog {
            baseline_id: self.baseline_id.clone(),
            run_id: self.run_id.clone(),
            stage_id: self.stage_id.clone(),
            fixture_id: self.fixture_id.clone(),
            scenario_id: self.scenario_id.clone(),
            p50_ms: self.summary.latency_p50_ms,
            p95_ms: self.summary.latency_p95_ms,
            p99_ms: self.summary.latency_p99_ms,
            throughput_ops_per_sec: self.summary.throughput_ops_per_sec,
            peak_rss_bytes: self.summary.peak_rss_bytes,
            command_fingerprint: self.command.fingerprint_sha256.clone(),
        }
    }

    #[must_use]
    pub fn optimization_ref(&self, candidate_id: impl Into<String>) -> OptimizationBaselineRef {
        OptimizationBaselineRef {
            candidate_id: candidate_id.into(),
            baseline_id: self.baseline_id.clone(),
            stage_id: self.stage_id.clone(),
            fixture_id: self.fixture_id.clone(),
            scenario_id: self.scenario_id.clone(),
            p50_ms: self.summary.latency_p50_ms,
            p95_ms: self.summary.latency_p95_ms,
            p99_ms: self.summary.latency_p99_ms,
            throughput_ops_per_sec: self.summary.throughput_ops_per_sec,
            peak_rss_bytes: self.summary.peak_rss_bytes,
            command_fingerprint: self.command.fingerprint_sha256.clone(),
        }
    }
}

/// Exported JSON stats artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BaselineJsonStatsArtifact {
    pub path: String,
    pub sha256: String,
    pub content: String,
}

/// Full capture report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BaselineCaptureReport {
    pub schema_version: String,
    pub baseline_id: String,
    pub manifest: BaselineManifest,
    pub records: Vec<BaselineRunRecord>,
    pub evidence_logs: Vec<BaselineEvidenceLog>,
    pub variance_violations: Vec<BaselineVarianceViolation>,
    pub exported_json_stats: BaselineJsonStatsArtifact,
}

impl BaselineCaptureReport {
    #[must_use]
    pub fn to_performance_run(&self, run_id: impl Into<String>) -> PerformanceRun {
        let mut workloads = Vec::new();
        let mut samples = Vec::new();

        for record in &self.records {
            workloads.push(record.workload_trace.clone());
            samples.extend(performance_samples_for_record(record));
        }

        PerformanceRun::new(run_id, workloads, samples).with_replay_command(format!(
            "doctor_frankentui baseline-capture --baseline-id {}",
            self.baseline_id
        ))
    }
}

/// Baseline capture runner.
#[derive(Debug, Clone)]
pub struct BaselineCaptureRunner {
    manifest: BaselineManifest,
}

impl BaselineCaptureRunner {
    #[must_use]
    pub fn new(manifest: BaselineManifest) -> Self {
        Self { manifest }
    }

    #[must_use]
    pub fn capture(&self, inputs: Vec<BaselineRunInput>) -> BaselineCaptureReport {
        let mut records = inputs
            .into_iter()
            .map(|input| capture_record(input, &self.manifest))
            .collect::<Vec<_>>();
        records.sort_by(|left, right| {
            left.stage_id
                .cmp(&right.stage_id)
                .then_with(|| left.fixture_id.cmp(&right.fixture_id))
                .then_with(|| left.scenario_id.cmp(&right.scenario_id))
                .then_with(|| left.run_id.cmp(&right.run_id))
        });

        let baseline_id = baseline_id_for(&self.manifest, &records);
        for record in &mut records {
            record.baseline_id = baseline_id.clone();
        }

        let evidence_logs = records
            .iter()
            .map(BaselineRunRecord::evidence_log)
            .collect::<Vec<_>>();
        let variance_violations = records
            .iter()
            .flat_map(|record| record.variance_violations.iter().cloned())
            .collect::<Vec<_>>();
        let exported_json_stats = export_json_stats(&baseline_id, &self.manifest, &records);

        BaselineCaptureReport {
            schema_version: BASELINE_CAPTURE_SCHEMA_VERSION.to_string(),
            baseline_id,
            manifest: self.manifest.clone(),
            records,
            evidence_logs,
            variance_violations,
            exported_json_stats,
        }
    }
}

fn capture_record(input: BaselineRunInput, manifest: &BaselineManifest) -> BaselineRunRecord {
    let measurement_samples = measurement_runs(&input.measurements);
    let warmup_samples = warmup_runs(&input.measurements);
    let summary = metric_summary(&measurement_samples, warmup_samples.len());
    let workload_trace = input
        .scenario
        .workload_trace(manifest.seed_policy.deterministic_seed);
    let variance_violations = variance_violations(&input.run_id, &measurement_samples, manifest);
    BaselineRunRecord {
        baseline_id: String::new(),
        run_id: input.run_id,
        stage_id: input.scenario.stage_id,
        fixture_id: input.scenario.fixture_id,
        scenario_id: input.scenario.scenario_id,
        workload_id: input.scenario.workload_id,
        deterministic_seed: manifest.seed_policy.deterministic_seed,
        command: input.scenario.command,
        workload_trace,
        warmup_samples,
        measurement_samples,
        summary,
        variance_violations,
    }
}

fn measurement_runs(measurements: &[BaselineRawMeasurement]) -> Vec<BaselineRawMeasurement> {
    let mut runs = measurements
        .iter()
        .filter(|measurement| matches!(measurement.phase, BaselineSamplePhase::Measurement))
        .cloned()
        .collect::<Vec<_>>();
    runs.sort_by_key(|measurement| measurement.sample_index);
    runs
}

fn warmup_runs(measurements: &[BaselineRawMeasurement]) -> Vec<BaselineRawMeasurement> {
    let mut runs = measurements
        .iter()
        .filter(|measurement| matches!(measurement.phase, BaselineSamplePhase::Warmup))
        .cloned()
        .collect::<Vec<_>>();
    runs.sort_by_key(|measurement| measurement.sample_index);
    runs
}

fn metric_summary(
    measurements: &[BaselineRawMeasurement],
    warmup_runs: usize,
) -> BaselineMetricSummary {
    let p50_values = measurements
        .iter()
        .map(|measurement| measurement.latency_p50_ms)
        .collect::<Vec<_>>();
    let p95_values = measurements
        .iter()
        .map(|measurement| measurement.latency_p95_ms)
        .collect::<Vec<_>>();
    let p99_values = measurements
        .iter()
        .map(|measurement| measurement.latency_p99_ms)
        .collect::<Vec<_>>();
    let throughput_values = measurements
        .iter()
        .map(|measurement| measurement.throughput_ops_per_sec)
        .collect::<Vec<_>>();
    let peak_rss_bytes = measurements
        .iter()
        .map(|measurement| measurement.peak_rss_bytes)
        .max()
        .unwrap_or(0);
    let cpu_user_ms = measurements
        .iter()
        .map(|measurement| measurement.cpu_user_ms)
        .sum();
    let cpu_system_ms = measurements
        .iter()
        .map(|measurement| measurement.cpu_system_ms)
        .sum();
    let syscall_count = measurements
        .iter()
        .map(|measurement| measurement.syscall_count)
        .sum();
    let measurement_hash = stable_hash(measurements);

    BaselineMetricSummary {
        measurement_runs: measurements.len(),
        warmup_runs,
        latency_p50_ms: percentile(p50_values, 50),
        latency_p95_ms: percentile(p95_values, 50),
        latency_p99_ms: percentile(p99_values, 50),
        throughput_ops_per_sec: percentile(throughput_values, 50),
        peak_rss_bytes,
        cpu_user_ms,
        cpu_system_ms,
        syscall_count,
        measurement_hash,
    }
}

fn variance_violations(
    run_id: &str,
    measurements: &[BaselineRawMeasurement],
    manifest: &BaselineManifest,
) -> Vec<BaselineVarianceViolation> {
    let envelope = &manifest.variance_envelope;
    let mut violations = Vec::new();
    if measurements.len() < envelope.min_measurement_runs {
        violations.push(BaselineVarianceViolation::InsufficientMeasurements {
            run_id: run_id.to_string(),
            observed: measurements.len(),
            required: envelope.min_measurement_runs,
        });
    }

    let p99_range = relative_range(
        measurements
            .iter()
            .map(|measurement| measurement.latency_p99_ms)
            .collect(),
    );
    if p99_range > envelope.max_p99_relative_range {
        violations.push(BaselineVarianceViolation::P99Variance {
            run_id: run_id.to_string(),
            observed_relative_range: p99_range,
            max_relative_range: envelope.max_p99_relative_range,
        });
    }

    let throughput_range = relative_range(
        measurements
            .iter()
            .map(|measurement| measurement.throughput_ops_per_sec)
            .collect(),
    );
    if throughput_range > envelope.max_throughput_relative_range {
        violations.push(BaselineVarianceViolation::ThroughputVariance {
            run_id: run_id.to_string(),
            observed_relative_range: throughput_range,
            max_relative_range: envelope.max_throughput_relative_range,
        });
    }

    violations
}

fn performance_samples_for_record(record: &BaselineRunRecord) -> Vec<PerformanceSample> {
    let mut samples = Vec::new();
    for measurement in &record.measurement_samples {
        let metrics = [
            (
                PerformanceMetricKind::LatencyP95Ms,
                measurement.latency_p95_ms,
            ),
            (
                PerformanceMetricKind::LatencyP99Ms,
                measurement.latency_p99_ms,
            ),
            (
                PerformanceMetricKind::ThroughputOpsPerSec,
                measurement.throughput_ops_per_sec,
            ),
            (
                PerformanceMetricKind::AllocationBytes,
                u64_to_f64(measurement.peak_rss_bytes),
            ),
        ];
        samples.extend(metrics.into_iter().map(|(metric, value)| {
            let sample_index = measurement.sample_index;
            PerformanceSample::new(
                record.scenario_id.clone(),
                metric,
                sample_index,
                value,
                record.deterministic_seed,
                record.workload_id.clone(),
            )
            .with_artifact_id(record.baseline_id.clone())
        }));
    }
    samples
}

fn baseline_id_for(manifest: &BaselineManifest, records: &[BaselineRunRecord]) -> String {
    let record_hashes = records
        .iter()
        .map(|record| {
            format!(
                "{}:{}:{}:{}:{}",
                record.run_id,
                record.stage_id,
                record.fixture_id,
                record.command.fingerprint_sha256,
                record.summary.measurement_hash
            )
        })
        .collect::<Vec<_>>();
    let hash = stable_hash(&BaselineIdInput {
        manifest_id: manifest.manifest_id.as_str(),
        record_hashes: &record_hashes,
    });
    format!("baseline-{}", short_hash(&hash))
}

#[derive(Serialize)]
struct BaselineIdInput<'a> {
    manifest_id: &'a str,
    record_hashes: &'a [String],
}

fn export_json_stats(
    baseline_id: &str,
    manifest: &BaselineManifest,
    records: &[BaselineRunRecord],
) -> BaselineJsonStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        baseline_id: &'a str,
        manifest_id: &'a str,
        records: &'a [BaselineRunRecord],
    }

    let payload = Export {
        schema_version: BASELINE_CAPTURE_SCHEMA_VERSION,
        baseline_id,
        manifest_id: manifest.manifest_id.as_str(),
        records,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    BaselineJsonStatsArtifact {
        path: format!("{baseline_id}/baseline_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

fn percentile(mut values: Vec<f64>, percentile: usize) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    let last_index = values.len().saturating_sub(1);
    let rounded_index = percentile
        .saturating_mul(last_index)
        .saturating_add(50)
        .checked_div(100)
        .unwrap_or(0);
    values.get(rounded_index).copied().unwrap_or(0.0)
}

fn relative_range(values: Vec<f64>) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let min = values.iter().copied().reduce(f64::min).unwrap_or(0.0);
    let max = values.iter().copied().reduce(f64::max).unwrap_or(0.0);
    let midpoint = (min.abs() + max.abs()) / 2.0;
    if midpoint <= f64::EPSILON {
        0.0
    } else {
        (max - min).abs() / midpoint
    }
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

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    crate::util::hex_encode(&hasher.finalize())
}

fn short_hash(value: &str) -> String {
    value.chars().take(16).collect()
}

fn u64_to_f64(value: u64) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(f64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> BaselineManifest {
        let mut tool_versions = BTreeMap::new();
        tool_versions.insert("cargo".to_string(), "1.90.0-nightly".to_string());
        tool_versions.insert("hyperfine".to_string(), "1.18.0".to_string());
        BaselineManifest::new(
            BaselineEnvironmentFingerprint::new(
                "linux",
                "x86_64",
                "ci-cpu",
                16,
                Some(32 * 1024 * 1024 * 1024),
                tool_versions,
                vec![
                    "governor=performance".to_string(),
                    "isolated=true".to_string(),
                ],
            ),
            BaselineCorpusSlice::new(
                "opentui-smoke",
                vec!["fixture-scroll".to_string()],
                vec!["baseline_capture".to_string()],
                "fixed-seed-smoke",
            ),
            BaselineSeedPolicy::new(42, "fixed")
                .with_replay_seed_material(vec!["fixture-scroll".to_string()]),
            BaselineVarianceEnvelope {
                max_p99_relative_range: 0.20,
                max_throughput_relative_range: 0.20,
                min_measurement_runs: 5,
            },
        )
    }

    fn scenario(args: Vec<String>) -> BaselineScenario {
        BaselineScenario::new(
            "baseline_capture",
            "fixture-scroll",
            "scroll-heavy",
            "workload-scroll-heavy",
            1_000,
            vec!["viewport=80x24".to_string(), "seed=42".to_string()],
            BaselineCommand::new("doctor_frankentui", args, "/repo", BTreeMap::new()),
        )
    }

    fn measurements() -> Vec<BaselineRawMeasurement> {
        let mut values = vec![
            BaselineRawMeasurement::warmup(0, 12.0),
            BaselineRawMeasurement::warmup(1, 11.8),
        ];
        values.extend([
            BaselineRawMeasurement::measurement(0, 8.0, 10.0, 12.0, 2_000.0, 10_000_000),
            BaselineRawMeasurement::measurement(1, 8.2, 10.1, 12.1, 2_010.0, 10_100_000),
            BaselineRawMeasurement::measurement(2, 8.1, 10.2, 12.2, 2_020.0, 10_050_000),
            BaselineRawMeasurement::measurement(3, 8.3, 10.1, 12.1, 2_030.0, 10_025_000),
            BaselineRawMeasurement::measurement(4, 8.1, 10.0, 12.0, 2_025.0, 10_075_000),
        ]);
        values
    }

    #[test]
    fn baseline_capture_is_deterministic_for_fixed_inputs() {
        let runner = BaselineCaptureRunner::new(manifest());
        let input = BaselineRunInput::new(
            "run-a",
            scenario(vec!["benchmark".to_string(), "scroll".to_string()]),
            measurements(),
        );
        let first = runner.capture(vec![input.clone()]);
        let second = runner.capture(vec![input]);

        assert!(
            first
                .baseline_id
                .as_str()
                .cmp(second.baseline_id.as_str())
                .is_eq()
        );
        assert!(
            first
                .exported_json_stats
                .sha256
                .as_str()
                .cmp(second.exported_json_stats.sha256.as_str())
                .is_eq()
        );
        assert!(first.variance_violations.is_empty());
    }

    #[test]
    fn evidence_log_contains_required_triage_fields() {
        let report = BaselineCaptureRunner::new(manifest()).capture(vec![BaselineRunInput::new(
            "run-a",
            scenario(vec!["benchmark".to_string(), "scroll".to_string()]),
            measurements(),
        )]);
        let log = report.evidence_logs.first();
        assert!(log.is_some());
        let Some(log) = log else {
            return;
        };

        assert!(
            log.run_id.as_str().cmp("run-a").is_eq()
                && log.stage_id.as_str().cmp("baseline_capture").is_eq()
                && log.fixture_id.as_str().cmp("fixture-scroll").is_eq()
        );
        assert!(log.p50_ms > 0.0);
        assert!(log.p95_ms > 0.0);
        assert!(log.p99_ms > 0.0);
        assert!(log.throughput_ops_per_sec > 0.0);
        assert!(log.peak_rss_bytes > 0);
        assert!(!log.command_fingerprint.is_empty());
        assert!(
            report
                .exported_json_stats
                .path
                .ends_with("baseline_stats.json")
        );
        assert!(
            report
                .exported_json_stats
                .content
                .contains("\"baseline_id\"")
        );
    }

    #[test]
    fn performance_run_exports_baseline_id_and_metric_samples() {
        let report = BaselineCaptureRunner::new(manifest()).capture(vec![BaselineRunInput::new(
            "run-a",
            scenario(vec!["benchmark".to_string(), "scroll".to_string()]),
            measurements(),
        )]);
        let perf_run = report.to_performance_run("source-baseline");

        assert_eq!(perf_run.workload_traces.len(), 1);
        assert_eq!(perf_run.samples.len(), 20);
        assert!(perf_run.samples.iter().all(|sample| {
            sample
                .artifact_id
                .as_deref()
                .is_some_and(|artifact_id| artifact_id.cmp(report.baseline_id.as_str()).is_eq())
        }));
        assert!(
            perf_run
                .samples
                .iter()
                .any(|sample| matches!(sample.metric, PerformanceMetricKind::LatencyP99Ms))
        );
        assert!(
            perf_run
                .samples
                .iter()
                .any(|sample| matches!(sample.metric, PerformanceMetricKind::ThroughputOpsPerSec))
        );
    }

    #[test]
    fn optimization_candidate_refs_include_baseline_metrics() {
        let report = BaselineCaptureRunner::new(manifest()).capture(vec![BaselineRunInput::new(
            "run-a",
            scenario(vec!["benchmark".to_string(), "scroll".to_string()]),
            measurements(),
        )]);
        let record = report.records.first();
        assert!(record.is_some());
        let Some(record) = record else {
            return;
        };
        let candidate_ref = record.optimization_ref("candidate-fast-scroll");

        assert!(
            candidate_ref
                .baseline_id
                .as_str()
                .cmp(report.baseline_id.as_str())
                .is_eq()
        );
        assert!(candidate_ref.p50_ms > 0.0);
        assert!(candidate_ref.p95_ms > 0.0);
        assert!(candidate_ref.p99_ms > 0.0);
        assert!(candidate_ref.throughput_ops_per_sec > 0.0);
        assert!(candidate_ref.peak_rss_bytes > 0);
    }

    #[test]
    fn variance_envelope_violations_are_reported() {
        let mut noisy = measurements();
        noisy.push(BaselineRawMeasurement::measurement(
            5, 8.0, 10.0, 40.0, 900.0, 11_000_000,
        ));
        let report = BaselineCaptureRunner::new(manifest()).capture(vec![BaselineRunInput::new(
            "run-noisy",
            scenario(vec!["benchmark".to_string(), "scroll".to_string()]),
            noisy,
        )]);

        assert!(
            report.variance_violations.iter().any(|violation| matches!(
                violation,
                BaselineVarianceViolation::P99Variance { .. }
            ))
        );
        assert!(report.variance_violations.iter().any(|violation| matches!(
            violation,
            BaselineVarianceViolation::ThroughputVariance { .. }
        )));
    }
}
