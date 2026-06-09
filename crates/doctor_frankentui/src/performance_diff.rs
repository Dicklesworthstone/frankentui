//! Performance comparison and improvement scoring for migration certification.
//!
//! The comparator consumes deterministic benchmark samples from source and
//! translated runs, checks that both sides used the same controlled workload
//! traces and seeds, then classifies per-scenario metric deltas with confidence
//! intervals and policy thresholds.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::semantic_contract::{
    ExpectedLossResult, TransformationRiskLevel, load_builtin_confidence_model,
    load_builtin_semantic_contract,
};

pub const PERFORMANCE_DIFF_VALIDATOR_ID: &str = "performance_diff_validator";

const PERF_CONTROL_POLICY_ID: &str = "PD-001";
const PERF_STATISTICS_POLICY_ID: &str = "PD-002";
const PERF_THRESHOLD_POLICY_ID: &str = "PD-003";
const DEFAULT_CONFIDENCE_Z: f64 = 1.96;
const DEFAULT_MIN_SAMPLES: usize = 5;
const DEFAULT_MIN_SIGNIFICANT_RELATIVE_DELTA: f64 = 0.01;
const EPSILON: f64 = 1.0e-9;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PerformanceDiffVerdict {
    Equivalent,
    Improvement,
    RegressionWithinPolicy,
    NeedsMoreEvidence,
    PolicyRegression,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MetricComparisonVerdict {
    Equivalent,
    SignificantImprovement,
    SignificantRegressionWithinPolicy,
    Inconclusive,
    PolicyRegression,
    InsufficientSamples,
    UncontrolledWorkload,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PerformanceMetricKind {
    LatencyMeanMs,
    LatencyP95Ms,
    LatencyP99Ms,
    ThroughputOpsPerSec,
    AllocationBytes,
    FrameJitterMs,
    DroppedFrameRatio,
}

impl PerformanceMetricKind {
    #[must_use]
    pub fn direction(self) -> PerformanceMetricDirection {
        match self {
            Self::ThroughputOpsPerSec => PerformanceMetricDirection::HigherIsBetter,
            Self::LatencyMeanMs
            | Self::LatencyP95Ms
            | Self::LatencyP99Ms
            | Self::AllocationBytes
            | Self::FrameJitterMs
            | Self::DroppedFrameRatio => PerformanceMetricDirection::LowerIsBetter,
        }
    }

    #[must_use]
    pub fn unit(self) -> &'static str {
        match self {
            Self::LatencyMeanMs | Self::LatencyP95Ms | Self::LatencyP99Ms | Self::FrameJitterMs => {
                "ms"
            }
            Self::ThroughputOpsPerSec => "ops/sec",
            Self::AllocationBytes => "bytes",
            Self::DroppedFrameRatio => "ratio",
        }
    }

    #[must_use]
    pub fn default_threshold(self) -> PerformanceThreshold {
        let (max_relative_regression, max_absolute_regression, risk_level) = match self {
            Self::LatencyP99Ms => (0.15, Some(8.0), TransformationRiskLevel::Critical),
            Self::LatencyP95Ms => (0.12, Some(6.0), TransformationRiskLevel::High),
            Self::LatencyMeanMs => (0.10, Some(4.0), TransformationRiskLevel::High),
            Self::ThroughputOpsPerSec => (0.10, None, TransformationRiskLevel::High),
            Self::AllocationBytes => (0.15, Some(1_048_576.0), TransformationRiskLevel::High),
            Self::FrameJitterMs => (0.12, Some(2.0), TransformationRiskLevel::Critical),
            Self::DroppedFrameRatio => (0.02, Some(0.005), TransformationRiskLevel::Critical),
        };
        PerformanceThreshold {
            max_relative_regression,
            max_absolute_regression,
            min_significant_relative_delta: DEFAULT_MIN_SIGNIFICANT_RELATIVE_DELTA,
            require_significance: true,
            risk_level,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PerformanceMetricDirection {
    LowerIsBetter,
    HigherIsBetter,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PerformanceThreshold {
    pub max_relative_regression: f64,
    pub max_absolute_regression: Option<f64>,
    pub min_significant_relative_delta: f64,
    pub require_significance: bool,
    pub risk_level: TransformationRiskLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PerformanceDiffConfig {
    pub min_samples_per_metric: usize,
    pub confidence_z: f64,
    pub thresholds: BTreeMap<PerformanceMetricKind, PerformanceThreshold>,
}

impl PerformanceDiffConfig {
    #[must_use]
    pub fn certification_default() -> Self {
        let thresholds = [
            PerformanceMetricKind::LatencyMeanMs,
            PerformanceMetricKind::LatencyP95Ms,
            PerformanceMetricKind::LatencyP99Ms,
            PerformanceMetricKind::ThroughputOpsPerSec,
            PerformanceMetricKind::AllocationBytes,
            PerformanceMetricKind::FrameJitterMs,
            PerformanceMetricKind::DroppedFrameRatio,
        ]
        .into_iter()
        .map(|metric| (metric, metric.default_threshold()))
        .collect();
        Self {
            min_samples_per_metric: DEFAULT_MIN_SAMPLES,
            confidence_z: DEFAULT_CONFIDENCE_Z,
            thresholds,
        }
    }

    #[must_use]
    pub fn threshold_for(&self, metric: PerformanceMetricKind) -> PerformanceThreshold {
        self.thresholds
            .get(&metric)
            .cloned()
            .unwrap_or_else(|| metric.default_threshold())
    }
}

impl Default for PerformanceDiffConfig {
    fn default() -> Self {
        Self::certification_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PerformanceWorkloadTrace {
    pub workload_id: String,
    pub scenario_id: String,
    pub deterministic_seed: u64,
    pub trace_hash: String,
    pub operation_count: u64,
    pub controlled_inputs: Vec<String>,
}

impl PerformanceWorkloadTrace {
    #[must_use]
    pub fn new(
        workload_id: impl Into<String>,
        scenario_id: impl Into<String>,
        deterministic_seed: u64,
        trace_hash: impl Into<String>,
        operation_count: u64,
    ) -> Self {
        Self {
            workload_id: workload_id.into(),
            scenario_id: scenario_id.into(),
            deterministic_seed,
            trace_hash: trace_hash.into(),
            operation_count,
            controlled_inputs: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_controlled_inputs(mut self, controlled_inputs: Vec<String>) -> Self {
        self.controlled_inputs = sorted_unique(controlled_inputs);
        self
    }

    #[must_use]
    pub fn canonical_key(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.scenario_id,
            self.workload_id,
            self.deterministic_seed,
            self.trace_hash,
            self.operation_count
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PerformanceSample {
    pub scenario_id: String,
    pub metric: PerformanceMetricKind,
    pub sample_index: u32,
    pub value: f64,
    pub deterministic_seed: u64,
    pub workload_id: String,
    pub artifact_id: Option<String>,
}

impl PerformanceSample {
    #[must_use]
    pub fn new(
        scenario_id: impl Into<String>,
        metric: PerformanceMetricKind,
        sample_index: u32,
        value: f64,
        deterministic_seed: u64,
        workload_id: impl Into<String>,
    ) -> Self {
        Self {
            scenario_id: scenario_id.into(),
            metric,
            sample_index,
            value,
            deterministic_seed,
            workload_id: workload_id.into(),
            artifact_id: None,
        }
    }

    #[must_use]
    pub fn with_artifact_id(mut self, artifact_id: impl Into<String>) -> Self {
        self.artifact_id = Some(artifact_id.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PerformanceRun {
    pub run_id: String,
    pub replay_command: Option<String>,
    pub workload_traces: Vec<PerformanceWorkloadTrace>,
    pub samples: Vec<PerformanceSample>,
}

impl PerformanceRun {
    #[must_use]
    pub fn new(
        run_id: impl Into<String>,
        workload_traces: Vec<PerformanceWorkloadTrace>,
        samples: Vec<PerformanceSample>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            replay_command: None,
            workload_traces: canonicalize_workload_traces(workload_traces),
            samples: canonicalize_samples(samples),
        }
    }

    #[must_use]
    pub fn with_replay_command(mut self, replay_command: impl Into<String>) -> Self {
        self.replay_command = Some(replay_command.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricStats {
    pub sample_count: usize,
    pub mean: f64,
    pub std_dev: f64,
    pub min: f64,
    pub max: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfidenceInterval {
    pub lower: f64,
    pub upper: f64,
    pub confidence_z: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricComparison {
    pub scenario_id: String,
    pub workload_id: String,
    pub metric: PerformanceMetricKind,
    pub unit: String,
    pub source_stats: MetricStats,
    pub translated_stats: MetricStats,
    pub absolute_delta: f64,
    pub relative_delta: f64,
    pub effective_relative_regression: f64,
    pub relative_delta_interval: ConfidenceInterval,
    pub effective_regression_interval: ConfidenceInterval,
    pub significant: bool,
    pub threshold: PerformanceThreshold,
    pub verdict: MetricComparisonVerdict,
    pub policy_id: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PerformanceDifferenceKind {
    MissingScenarioMetric,
    UnexpectedScenarioMetric,
    UncontrolledWorkload,
    InsufficientSamples,
    PolicyRegression,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PerformanceDifference {
    pub difference_kind: PerformanceDifferenceKind,
    pub scenario_id: String,
    pub workload_id: Option<String>,
    pub metric: Option<PerformanceMetricKind>,
    pub source_value: Option<String>,
    pub translated_value: Option<String>,
    pub policy_id: String,
    pub risk_level: TransformationRiskLevel,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PerformanceDiffArtifactFile {
    pub path: String,
    pub sha256: String,
    pub byte_len: usize,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PerformanceDiffArtifactBundle {
    pub bundle_id: String,
    pub replay_command: String,
    pub files: Vec<PerformanceDiffArtifactFile>,
    pub bundle_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PerformanceDiffReport {
    pub validator_id: String,
    pub contract_id: String,
    pub source_run_id: String,
    pub translated_run_id: String,
    pub verdict: PerformanceDiffVerdict,
    pub certification_passed: bool,
    pub comparisons: Vec<MetricComparison>,
    pub differences: Vec<PerformanceDifference>,
    pub controlled_workload_ids: Vec<String>,
    pub covered_policy_ids: Vec<String>,
    pub violated_policy_ids: Vec<String>,
    pub risk_level: TransformationRiskLevel,
    pub risk_score: f64,
    pub expected_loss: ExpectedLossResult,
    pub artifact_bundle: Option<PerformanceDiffArtifactBundle>,
}

#[must_use]
pub fn compare_performance_runs(
    source_run: &PerformanceRun,
    translated_run: &PerformanceRun,
    config: &PerformanceDiffConfig,
) -> PerformanceDiffReport {
    let contract = load_builtin_semantic_contract().expect("built-in semantic contract parses");
    let source_workloads = workload_map(&source_run.workload_traces);
    let translated_workloads = workload_map(&translated_run.workload_traces);
    let source_groups = sample_groups(&source_run.samples);
    let translated_groups = sample_groups(&translated_run.samples);
    let mut group_keys = source_groups.keys().cloned().collect::<BTreeSet<_>>();
    group_keys.extend(translated_groups.keys().cloned());

    let mut comparisons = Vec::new();
    let mut differences = Vec::new();
    let mut covered_policy_ids = BTreeSet::new();
    let mut violated_policy_ids = BTreeSet::new();
    let mut controlled_workload_ids = BTreeSet::new();
    let mut successes = 0_u32;
    let mut weighted_failures = 0_u32;

    for key in group_keys {
        match (source_groups.get(&key), translated_groups.get(&key)) {
            (Some(source_samples), Some(translated_samples)) => {
                let control_result = compare_workload_controls(
                    &key,
                    source_samples,
                    translated_samples,
                    &source_workloads,
                    &translated_workloads,
                );
                if let Some(diff) = control_result.difference {
                    weighted_failures =
                        weighted_failures.saturating_add(failure_weight(diff.risk_level));
                    violated_policy_ids.insert(diff.policy_id.clone());
                    differences.push(diff);
                    continue;
                }
                if let Some(workload_id) = control_result.workload_id {
                    controlled_workload_ids.insert(workload_id);
                }

                if source_samples.len() < config.min_samples_per_metric
                    || translated_samples.len() < config.min_samples_per_metric
                {
                    let diff = insufficient_samples(
                        &key,
                        source_samples.len(),
                        translated_samples.len(),
                        config.min_samples_per_metric,
                    );
                    weighted_failures =
                        weighted_failures.saturating_add(failure_weight(diff.risk_level));
                    violated_policy_ids.insert(diff.policy_id.clone());
                    differences.push(diff);
                    continue;
                }

                let comparison =
                    compare_metric_samples(&key, source_samples, translated_samples, config);
                match comparison.verdict {
                    MetricComparisonVerdict::PolicyRegression => {
                        let diff = policy_regression_difference(&comparison);
                        weighted_failures =
                            weighted_failures.saturating_add(failure_weight(diff.risk_level));
                        violated_policy_ids.insert(diff.policy_id.clone());
                        differences.push(diff);
                    }
                    MetricComparisonVerdict::SignificantRegressionWithinPolicy
                    | MetricComparisonVerdict::Equivalent
                    | MetricComparisonVerdict::SignificantImprovement
                    | MetricComparisonVerdict::Inconclusive => {
                        successes = successes.saturating_add(1);
                        covered_policy_ids.insert(comparison.policy_id.clone());
                    }
                    MetricComparisonVerdict::InsufficientSamples
                    | MetricComparisonVerdict::UncontrolledWorkload => {}
                }
                comparisons.push(comparison);
            }
            (Some(source_samples), None) => {
                let diff = missing_or_unexpected_metric(
                    PerformanceDifferenceKind::MissingScenarioMetric,
                    &key,
                    Some(source_samples.len()),
                    None,
                );
                weighted_failures =
                    weighted_failures.saturating_add(failure_weight(diff.risk_level));
                violated_policy_ids.insert(diff.policy_id.clone());
                differences.push(diff);
            }
            (None, Some(translated_samples)) => {
                let diff = missing_or_unexpected_metric(
                    PerformanceDifferenceKind::UnexpectedScenarioMetric,
                    &key,
                    None,
                    Some(translated_samples.len()),
                );
                weighted_failures =
                    weighted_failures.saturating_add(failure_weight(diff.risk_level));
                violated_policy_ids.insert(diff.policy_id.clone());
                differences.push(diff);
            }
            (None, None) => {}
        }
    }

    let verdict = overall_verdict(&comparisons, &differences);
    let certification_passed = differences.is_empty()
        && !comparisons
            .iter()
            .any(|comparison| comparison.verdict == MetricComparisonVerdict::Inconclusive);
    let risk_level = differences
        .iter()
        .map(|diff| diff.risk_level)
        .max()
        .unwrap_or(TransformationRiskLevel::Low);
    let risk_score = risk_score(successes, weighted_failures);
    let first_violated_policy = violated_policy_ids.iter().next().cloned();
    let expected_loss = expected_loss(successes, weighted_failures, first_violated_policy);
    let artifact_bundle = if differences.is_empty() {
        None
    } else {
        Some(build_artifact_bundle(
            source_run,
            translated_run,
            &comparisons,
            &differences,
        ))
    };

    PerformanceDiffReport {
        validator_id: PERFORMANCE_DIFF_VALIDATOR_ID.to_string(),
        contract_id: contract.contract_id,
        source_run_id: source_run.run_id.clone(),
        translated_run_id: translated_run.run_id.clone(),
        verdict,
        certification_passed,
        comparisons,
        differences,
        controlled_workload_ids: controlled_workload_ids.into_iter().collect(),
        covered_policy_ids: covered_policy_ids.into_iter().collect(),
        violated_policy_ids: violated_policy_ids.into_iter().collect(),
        risk_level,
        risk_score,
        expected_loss,
        artifact_bundle,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct MetricKey {
    scenario_id: String,
    metric: PerformanceMetricKind,
}

#[derive(Debug, Default)]
struct WorkloadControlResult {
    workload_id: Option<String>,
    difference: Option<PerformanceDifference>,
}

fn compare_metric_samples(
    key: &MetricKey,
    source_samples: &[PerformanceSample],
    translated_samples: &[PerformanceSample],
    config: &PerformanceDiffConfig,
) -> MetricComparison {
    let source_stats = compute_stats(source_samples);
    let translated_stats = compute_stats(translated_samples);
    let absolute_delta = translated_stats.mean - source_stats.mean;
    let relative_delta = if source_stats.mean.abs() <= EPSILON {
        0.0
    } else {
        absolute_delta / source_stats.mean.abs()
    };
    let interval = relative_delta_interval(
        &source_stats,
        &translated_stats,
        source_samples.len(),
        translated_samples.len(),
        config.confidence_z,
    );
    let effective_interval = effective_interval(key.metric.direction(), &interval);
    let effective_relative_regression = effective_delta(key.metric.direction(), relative_delta);
    let threshold = config.threshold_for(key.metric);
    let significant = is_significant(
        &effective_interval,
        threshold.min_significant_relative_delta,
    );
    let policy_regression = exceeds_policy_threshold(
        effective_relative_regression,
        absolute_delta,
        key.metric.direction(),
        &effective_interval,
        &threshold,
        significant,
    );
    let verdict = if policy_regression {
        MetricComparisonVerdict::PolicyRegression
    } else if significant && effective_interval.lower > 0.0 {
        MetricComparisonVerdict::SignificantRegressionWithinPolicy
    } else if significant && effective_interval.upper < 0.0 {
        MetricComparisonVerdict::SignificantImprovement
    } else if effective_relative_regression.abs() <= threshold.min_significant_relative_delta {
        MetricComparisonVerdict::Equivalent
    } else {
        MetricComparisonVerdict::Inconclusive
    };
    let workload_id = common_workload_id(source_samples, translated_samples).unwrap_or_default();
    let message = comparison_message(
        key,
        verdict,
        effective_relative_regression,
        &effective_interval,
        &threshold,
    );

    MetricComparison {
        scenario_id: key.scenario_id.clone(),
        workload_id,
        metric: key.metric,
        unit: key.metric.unit().to_string(),
        source_stats,
        translated_stats,
        absolute_delta,
        relative_delta,
        effective_relative_regression,
        relative_delta_interval: interval,
        effective_regression_interval: effective_interval,
        significant,
        threshold,
        verdict,
        policy_id: PERF_STATISTICS_POLICY_ID.to_string(),
        message,
    }
}

fn compare_workload_controls(
    key: &MetricKey,
    source_samples: &[PerformanceSample],
    translated_samples: &[PerformanceSample],
    source_workloads: &BTreeMap<String, PerformanceWorkloadTrace>,
    translated_workloads: &BTreeMap<String, PerformanceWorkloadTrace>,
) -> WorkloadControlResult {
    let Some(workload_id) = common_workload_id(source_samples, translated_samples) else {
        return WorkloadControlResult {
            difference: Some(uncontrolled_workload(
                key,
                None,
                None,
                "source and translated samples do not share one workload_id",
            )),
            ..WorkloadControlResult::default()
        };
    };
    let Some(source_workload) = source_workloads.get(&workload_id) else {
        return WorkloadControlResult {
            difference: Some(uncontrolled_workload(
                key,
                Some(&workload_id),
                None,
                "source run is missing the workload trace descriptor",
            )),
            ..WorkloadControlResult::default()
        };
    };
    let Some(translated_workload) = translated_workloads.get(&workload_id) else {
        return WorkloadControlResult {
            difference: Some(uncontrolled_workload(
                key,
                Some(&workload_id),
                None,
                "translated run is missing the workload trace descriptor",
            )),
            ..WorkloadControlResult::default()
        };
    };

    if source_workload.scenario_id != key.scenario_id
        || translated_workload.scenario_id != key.scenario_id
    {
        return WorkloadControlResult {
            difference: Some(uncontrolled_workload(
                key,
                Some(&workload_id),
                Some(format!(
                    "source_scenario={};translated_scenario={}",
                    source_workload.scenario_id, translated_workload.scenario_id
                )),
                "workload trace scenario does not match sampled scenario",
            )),
            ..WorkloadControlResult::default()
        };
    }

    if source_workload.deterministic_seed != translated_workload.deterministic_seed {
        return WorkloadControlResult {
            difference: Some(uncontrolled_workload(
                key,
                Some(&workload_id),
                Some(format!(
                    "source_seed={};translated_seed={}",
                    source_workload.deterministic_seed, translated_workload.deterministic_seed
                )),
                "workload traces used different deterministic seeds",
            )),
            ..WorkloadControlResult::default()
        };
    }

    if source_workload.trace_hash != translated_workload.trace_hash
        || source_workload.operation_count != translated_workload.operation_count
        || source_workload.controlled_inputs != translated_workload.controlled_inputs
    {
        return WorkloadControlResult {
            difference: Some(uncontrolled_workload(
                key,
                Some(&workload_id),
                Some(format!(
                    "source={};translated={}",
                    source_workload.canonical_key(),
                    translated_workload.canonical_key()
                )),
                "workload traces are not byte-equivalent controlled inputs",
            )),
            ..WorkloadControlResult::default()
        };
    }

    let seed_mismatch = source_samples
        .iter()
        .chain(translated_samples.iter())
        .any(|sample| sample.deterministic_seed != source_workload.deterministic_seed);
    if seed_mismatch {
        return WorkloadControlResult {
            difference: Some(uncontrolled_workload(
                key,
                Some(&workload_id),
                Some(format!(
                    "expected_sample_seed={}",
                    source_workload.deterministic_seed
                )),
                "at least one sample was collected with a seed outside the controlled trace",
            )),
            ..WorkloadControlResult::default()
        };
    }

    WorkloadControlResult {
        workload_id: Some(workload_id),
        difference: None,
    }
}

fn compute_stats(samples: &[PerformanceSample]) -> MetricStats {
    let sample_count = samples.len();
    let values = samples
        .iter()
        .map(|sample| sample.value)
        .collect::<Vec<_>>();
    let count = usize_to_f64(sample_count);
    let mean = if sample_count == 0 {
        0.0
    } else {
        values.iter().sum::<f64>() / count
    };
    let variance = if sample_count <= 1 {
        0.0
    } else {
        values
            .iter()
            .map(|value| {
                let delta = value - mean;
                delta * delta
            })
            .sum::<f64>()
            / usize_to_f64(sample_count.saturating_sub(1))
    };
    let min = values.iter().copied().reduce(f64::min).unwrap_or(0.0);
    let max = values.iter().copied().reduce(f64::max).unwrap_or(0.0);
    MetricStats {
        sample_count,
        mean,
        std_dev: variance.sqrt(),
        min,
        max,
    }
}

fn relative_delta_interval(
    source_stats: &MetricStats,
    translated_stats: &MetricStats,
    source_count: usize,
    translated_count: usize,
    confidence_z: f64,
) -> ConfidenceInterval {
    let source_n = usize_to_f64(source_count).max(1.0);
    let translated_n = usize_to_f64(translated_count).max(1.0);
    let source_var = source_stats.std_dev * source_stats.std_dev;
    let translated_var = translated_stats.std_dev * translated_stats.std_dev;
    let standard_error = (source_var / source_n + translated_var / translated_n).sqrt();
    let absolute_delta = translated_stats.mean - source_stats.mean;
    let lower_abs = absolute_delta - confidence_z * standard_error;
    let upper_abs = absolute_delta + confidence_z * standard_error;
    let denominator = source_stats.mean.abs();

    if denominator <= EPSILON {
        ConfidenceInterval {
            lower: 0.0,
            upper: 0.0,
            confidence_z,
        }
    } else {
        ConfidenceInterval {
            lower: lower_abs / denominator,
            upper: upper_abs / denominator,
            confidence_z,
        }
    }
}

fn effective_interval(
    direction: PerformanceMetricDirection,
    interval: &ConfidenceInterval,
) -> ConfidenceInterval {
    match direction {
        PerformanceMetricDirection::LowerIsBetter => interval.clone(),
        PerformanceMetricDirection::HigherIsBetter => ConfidenceInterval {
            lower: -interval.upper,
            upper: -interval.lower,
            confidence_z: interval.confidence_z,
        },
    }
}

fn effective_delta(direction: PerformanceMetricDirection, relative_delta: f64) -> f64 {
    match direction {
        PerformanceMetricDirection::LowerIsBetter => relative_delta,
        PerformanceMetricDirection::HigherIsBetter => -relative_delta,
    }
}

fn is_significant(interval: &ConfidenceInterval, min_relative_delta: f64) -> bool {
    interval.lower > min_relative_delta || interval.upper < -min_relative_delta
}

fn exceeds_policy_threshold(
    effective_relative_regression: f64,
    absolute_delta: f64,
    direction: PerformanceMetricDirection,
    effective_interval: &ConfidenceInterval,
    threshold: &PerformanceThreshold,
    significant: bool,
) -> bool {
    let effective_absolute_delta = match direction {
        PerformanceMetricDirection::LowerIsBetter => absolute_delta,
        PerformanceMetricDirection::HigherIsBetter => -absolute_delta,
    };
    let relative_exceeds = if threshold.require_significance {
        significant && effective_interval.lower > threshold.max_relative_regression
    } else {
        effective_relative_regression > threshold.max_relative_regression
    };
    let absolute_exceeds = threshold
        .max_absolute_regression
        .is_some_and(|absolute_threshold| {
            if threshold.require_significance {
                significant && effective_absolute_delta > absolute_threshold
            } else {
                effective_absolute_delta > absolute_threshold
            }
        });

    relative_exceeds || absolute_exceeds
}

fn comparison_message(
    key: &MetricKey,
    verdict: MetricComparisonVerdict,
    effective_relative_regression: f64,
    interval: &ConfidenceInterval,
    threshold: &PerformanceThreshold,
) -> String {
    format!(
        "{:?} in scenario '{}' classified as {:?}: effective regression {:+.2}% (CI {:+.2}%..{:+.2}%, threshold {:+.2}%)",
        key.metric,
        key.scenario_id,
        verdict,
        effective_relative_regression * 100.0,
        interval.lower * 100.0,
        interval.upper * 100.0,
        threshold.max_relative_regression * 100.0
    )
}

fn overall_verdict(
    comparisons: &[MetricComparison],
    differences: &[PerformanceDifference],
) -> PerformanceDiffVerdict {
    if differences
        .iter()
        .any(|diff| diff.difference_kind == PerformanceDifferenceKind::PolicyRegression)
    {
        return PerformanceDiffVerdict::PolicyRegression;
    }
    if differences
        .iter()
        .any(|diff| diff.difference_kind == PerformanceDifferenceKind::InsufficientSamples)
        || comparisons
            .iter()
            .any(|comparison| comparison.verdict == MetricComparisonVerdict::Inconclusive)
    {
        return PerformanceDiffVerdict::NeedsMoreEvidence;
    }
    if !differences.is_empty() {
        return PerformanceDiffVerdict::PolicyRegression;
    }
    if comparisons.iter().any(|comparison| {
        comparison.verdict == MetricComparisonVerdict::SignificantRegressionWithinPolicy
    }) {
        return PerformanceDiffVerdict::RegressionWithinPolicy;
    }
    if comparisons
        .iter()
        .any(|comparison| comparison.verdict == MetricComparisonVerdict::SignificantImprovement)
    {
        return PerformanceDiffVerdict::Improvement;
    }
    PerformanceDiffVerdict::Equivalent
}

fn workload_map(
    workloads: &[PerformanceWorkloadTrace],
) -> BTreeMap<String, PerformanceWorkloadTrace> {
    workloads
        .iter()
        .cloned()
        .map(|workload| (workload.workload_id.clone(), workload))
        .collect()
}

fn sample_groups(samples: &[PerformanceSample]) -> BTreeMap<MetricKey, Vec<PerformanceSample>> {
    let mut groups: BTreeMap<MetricKey, Vec<PerformanceSample>> = BTreeMap::new();
    for sample in samples {
        groups
            .entry(MetricKey {
                scenario_id: sample.scenario_id.clone(),
                metric: sample.metric,
            })
            .or_default()
            .push(sample.clone());
    }
    for values in groups.values_mut() {
        *values = canonicalize_samples(std::mem::take(values));
    }
    groups
}

fn common_workload_id(
    source_samples: &[PerformanceSample],
    translated_samples: &[PerformanceSample],
) -> Option<String> {
    let mut ids = source_samples
        .iter()
        .chain(translated_samples.iter())
        .map(|sample| sample.workload_id.clone())
        .collect::<BTreeSet<_>>();
    if ids.len() == 1 {
        ids.pop_first()
    } else {
        None
    }
}

fn insufficient_samples(
    key: &MetricKey,
    source_count: usize,
    translated_count: usize,
    minimum: usize,
) -> PerformanceDifference {
    PerformanceDifference {
        difference_kind: PerformanceDifferenceKind::InsufficientSamples,
        scenario_id: key.scenario_id.clone(),
        workload_id: None,
        metric: Some(key.metric),
        source_value: Some(source_count.to_string()),
        translated_value: Some(translated_count.to_string()),
        policy_id: PERF_STATISTICS_POLICY_ID.to_string(),
        risk_level: TransformationRiskLevel::Medium,
        message: format!(
            "insufficient deterministic samples for {:?} in scenario '{}': source={source_count}, translated={translated_count}, required={minimum}",
            key.metric, key.scenario_id
        ),
    }
}

fn missing_or_unexpected_metric(
    difference_kind: PerformanceDifferenceKind,
    key: &MetricKey,
    source_count: Option<usize>,
    translated_count: Option<usize>,
) -> PerformanceDifference {
    let message = match difference_kind {
        PerformanceDifferenceKind::MissingScenarioMetric => {
            "translated run is missing a scenario metric present in source"
        }
        PerformanceDifferenceKind::UnexpectedScenarioMetric => {
            "translated run emitted a scenario metric absent from source"
        }
        _ => "scenario metric shape mismatch",
    };
    PerformanceDifference {
        difference_kind,
        scenario_id: key.scenario_id.clone(),
        workload_id: None,
        metric: Some(key.metric),
        source_value: source_count.map(|count| count.to_string()),
        translated_value: translated_count.map(|count| count.to_string()),
        policy_id: PERF_CONTROL_POLICY_ID.to_string(),
        risk_level: TransformationRiskLevel::High,
        message: format!(
            "{message}: scenario='{}', metric={:?}",
            key.scenario_id, key.metric
        ),
    }
}

fn uncontrolled_workload(
    key: &MetricKey,
    workload_id: Option<&str>,
    value: Option<String>,
    reason: &str,
) -> PerformanceDifference {
    PerformanceDifference {
        difference_kind: PerformanceDifferenceKind::UncontrolledWorkload,
        scenario_id: key.scenario_id.clone(),
        workload_id: workload_id.map(ToString::to_string),
        metric: Some(key.metric),
        source_value: value,
        translated_value: None,
        policy_id: PERF_CONTROL_POLICY_ID.to_string(),
        risk_level: TransformationRiskLevel::Critical,
        message: format!(
            "benchmark control violation for {:?} in scenario '{}': {reason}",
            key.metric, key.scenario_id
        ),
    }
}

fn policy_regression_difference(comparison: &MetricComparison) -> PerformanceDifference {
    PerformanceDifference {
        difference_kind: PerformanceDifferenceKind::PolicyRegression,
        scenario_id: comparison.scenario_id.clone(),
        workload_id: Some(comparison.workload_id.clone()),
        metric: Some(comparison.metric),
        source_value: Some(format!("{:.6}", comparison.source_stats.mean)),
        translated_value: Some(format!("{:.6}", comparison.translated_stats.mean)),
        policy_id: PERF_THRESHOLD_POLICY_ID.to_string(),
        risk_level: comparison.threshold.risk_level,
        message: format!(
            "performance regression exceeds policy for {:?} in scenario '{}': effective regression {:+.2}% exceeds threshold {:+.2}%",
            comparison.metric,
            comparison.scenario_id,
            comparison.effective_relative_regression * 100.0,
            comparison.threshold.max_relative_regression * 100.0
        ),
    }
}

fn risk_score(successes: u32, weighted_failures: u32) -> f64 {
    if weighted_failures == 0 {
        return 0.0;
    }
    let total = successes.saturating_add(weighted_failures);
    f64::from(weighted_failures) / f64::from(total)
}

fn failure_weight(risk: TransformationRiskLevel) -> u32 {
    match risk {
        TransformationRiskLevel::Low => 1,
        TransformationRiskLevel::Medium => 2,
        TransformationRiskLevel::High => 4,
        TransformationRiskLevel::Critical => 8,
    }
}

fn expected_loss(
    successes: u32,
    weighted_failures: u32,
    claim_id: Option<String>,
) -> ExpectedLossResult {
    let confidence_model =
        load_builtin_confidence_model().expect("built-in confidence model must parse");
    let posterior = confidence_model.compute_posterior(successes, weighted_failures);
    confidence_model.expected_loss_decision(
        &posterior,
        claim_id,
        Some(PERFORMANCE_DIFF_VALIDATOR_ID.to_string()),
    )
}

fn build_artifact_bundle(
    source_run: &PerformanceRun,
    translated_run: &PerformanceRun,
    comparisons: &[MetricComparison],
    differences: &[PerformanceDifference],
) -> PerformanceDiffArtifactBundle {
    let replay_command = replay_command(source_run, translated_run);
    let comparisons_json =
        serde_json::to_string_pretty(comparisons).expect("metric comparisons serialize");
    let differences_jsonl = differences
        .iter()
        .map(|diff| serde_json::to_string(diff).expect("performance difference serializes"))
        .collect::<Vec<_>>()
        .join("\n");
    let summary = serde_json::json!({
        "validator_id": PERFORMANCE_DIFF_VALIDATOR_ID,
        "source_run_id": source_run.run_id,
        "translated_run_id": translated_run.run_id,
        "difference_count": differences.len(),
        "comparison_count": comparisons.len(),
    })
    .to_string();
    let files = vec![
        artifact_file(
            "replay.sh",
            format!("#!/usr/bin/env bash\n{replay_command}\n"),
        ),
        artifact_file("metric_comparisons.json", comparisons_json),
        artifact_file("performance_diffs.jsonl", differences_jsonl),
        artifact_file("summary.json", summary),
    ];
    let bundle_hash_input = files
        .iter()
        .map(|file| format!("{}:{}:{}", file.path, file.sha256, file.byte_len))
        .collect::<Vec<_>>()
        .join("\n");
    let bundle_sha256 = sha256_hex(bundle_hash_input.as_bytes());
    let bundle_id = format!("performance-diff-{}", &bundle_sha256[..12]);

    PerformanceDiffArtifactBundle {
        bundle_id,
        replay_command,
        files,
        bundle_sha256,
    }
}

fn artifact_file(path: &str, content: String) -> PerformanceDiffArtifactFile {
    PerformanceDiffArtifactFile {
        path: path.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        byte_len: content.len(),
        content,
    }
}

fn replay_command(source_run: &PerformanceRun, translated_run: &PerformanceRun) -> String {
    match (&source_run.replay_command, &translated_run.replay_command) {
        (Some(source), Some(translated)) => format!("{source} && {translated}"),
        (Some(source), None) => source.clone(),
        (None, Some(translated)) => translated.clone(),
        (None, None) => format!(
            "doctor_frankentui perf-replay --source-run {} --translated-run {}",
            source_run.run_id, translated_run.run_id
        ),
    }
}

fn canonicalize_workload_traces(
    mut workload_traces: Vec<PerformanceWorkloadTrace>,
) -> Vec<PerformanceWorkloadTrace> {
    workload_traces.sort_by(|a, b| {
        a.scenario_id
            .cmp(&b.scenario_id)
            .then_with(|| a.workload_id.cmp(&b.workload_id))
            .then_with(|| a.deterministic_seed.cmp(&b.deterministic_seed))
            .then_with(|| a.trace_hash.cmp(&b.trace_hash))
    });
    workload_traces
}

fn canonicalize_samples(mut samples: Vec<PerformanceSample>) -> Vec<PerformanceSample> {
    samples.sort_by(|a, b| {
        a.scenario_id
            .cmp(&b.scenario_id)
            .then_with(|| a.metric.cmp(&b.metric))
            .then_with(|| a.workload_id.cmp(&b.workload_id))
            .then_with(|| a.deterministic_seed.cmp(&b.deterministic_seed))
            .then_with(|| a.sample_index.cmp(&b.sample_index))
            .then_with(|| a.value.total_cmp(&b.value))
    });
    samples
}

fn sorted_unique(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn usize_to_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    crate::util::hex_encode(&hasher.finalize())
}
