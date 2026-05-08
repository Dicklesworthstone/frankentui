use doctor_frankentui::performance_diff::{
    MetricComparisonVerdict, PerformanceDiffConfig, PerformanceDiffVerdict,
    PerformanceDifferenceKind, PerformanceMetricKind, PerformanceRun, PerformanceSample,
    PerformanceWorkloadTrace, compare_performance_runs,
};
use doctor_frankentui::semantic_contract::TransformationRiskLevel;

fn workload(scenario_id: &str, seed: u64) -> PerformanceWorkloadTrace {
    PerformanceWorkloadTrace::new(
        format!("workload-{scenario_id}"),
        scenario_id,
        seed,
        format!("trace-hash-{scenario_id}"),
        128,
    )
    .with_controlled_inputs(vec![
        "viewport=80x24".to_string(),
        "theme=default".to_string(),
    ])
}

fn samples(
    scenario_id: &str,
    metric: PerformanceMetricKind,
    seed: u64,
    values: &[f64],
) -> Vec<PerformanceSample> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            PerformanceSample::new(
                scenario_id,
                metric,
                u32::try_from(index).expect("test sample index fits u32"),
                *value,
                seed,
                format!("workload-{scenario_id}"),
            )
        })
        .collect()
}

fn run(
    run_id: &str,
    scenario_id: &str,
    seed: u64,
    metric: PerformanceMetricKind,
    values: &[f64],
) -> PerformanceRun {
    PerformanceRun::new(
        run_id,
        vec![workload(scenario_id, seed)],
        samples(scenario_id, metric, seed, values),
    )
    .with_replay_command(format!("doctor_frankentui perf-replay --run-id {run_id}"))
}

#[test]
fn deterministic_benchmark_improvement_passes_certification() {
    let source = run(
        "source-run",
        "scroll-heavy",
        42,
        PerformanceMetricKind::LatencyP99Ms,
        &[100.0; 8],
    );
    let translated = run(
        "translated-run",
        "scroll-heavy",
        42,
        PerformanceMetricKind::LatencyP99Ms,
        &[80.0; 8],
    );

    let report = compare_performance_runs(
        &source,
        &translated,
        &PerformanceDiffConfig::certification_default(),
    );

    assert_eq!(report.verdict, PerformanceDiffVerdict::Improvement);
    assert!(report.certification_passed);
    assert!(report.differences.is_empty());
    assert_eq!(report.comparisons.len(), 1);
    assert_eq!(
        report.comparisons[0].verdict,
        MetricComparisonVerdict::SignificantImprovement
    );
    assert!(
        report
            .controlled_workload_ids
            .contains(&"workload-scroll-heavy".to_string())
    );
    assert_eq!(
        report.expected_loss.policy_id.as_deref(),
        Some("performance_diff_validator")
    );
}

#[test]
fn policy_threshold_regression_fails_certification_with_artifacts() {
    let source = run(
        "source-run",
        "render-grid",
        7,
        PerformanceMetricKind::LatencyP99Ms,
        &[100.0; 10],
    );
    let translated = run(
        "translated-run",
        "render-grid",
        7,
        PerformanceMetricKind::LatencyP99Ms,
        &[140.0; 10],
    );

    let report = compare_performance_runs(
        &source,
        &translated,
        &PerformanceDiffConfig::certification_default(),
    );

    assert_eq!(report.verdict, PerformanceDiffVerdict::PolicyRegression);
    assert!(!report.certification_passed);
    assert_eq!(report.risk_level, TransformationRiskLevel::Critical);
    assert_eq!(report.differences.len(), 1);
    assert_eq!(
        report.differences[0].difference_kind,
        PerformanceDifferenceKind::PolicyRegression
    );
    assert!(report.violated_policy_ids.contains(&"PD-003".to_string()));
    assert_eq!(report.expected_loss.claim_id.as_deref(), Some("PD-003"));

    let bundle = report
        .artifact_bundle
        .expect("policy regression emits replayable artifact bundle");
    assert!(bundle.replay_command.contains("source-run"));
    assert!(bundle.replay_command.contains("translated-run"));
    assert!(bundle.files.iter().any(|file| file.path == "replay.sh"));
    assert!(
        bundle
            .files
            .iter()
            .any(|file| file.path == "performance_diffs.jsonl")
    );
}

#[test]
fn significant_regression_inside_policy_is_classified_without_failure() {
    let source = run(
        "source-run",
        "layout-small",
        11,
        PerformanceMetricKind::LatencyMeanMs,
        &[100.0; 6],
    );
    let translated = run(
        "translated-run",
        "layout-small",
        11,
        PerformanceMetricKind::LatencyMeanMs,
        &[103.0; 6],
    );

    let report = compare_performance_runs(
        &source,
        &translated,
        &PerformanceDiffConfig::certification_default(),
    );

    assert_eq!(
        report.verdict,
        PerformanceDiffVerdict::RegressionWithinPolicy
    );
    assert!(report.certification_passed);
    assert!(report.differences.is_empty());
    assert_eq!(
        report.comparisons[0].verdict,
        MetricComparisonVerdict::SignificantRegressionWithinPolicy
    );
    assert!(report.comparisons[0].significant);
    assert!(report.comparisons[0].effective_relative_regression > 0.0);
}

#[test]
fn throughput_direction_treats_higher_values_as_improvement() {
    let source = run(
        "source-run",
        "input-burst",
        99,
        PerformanceMetricKind::ThroughputOpsPerSec,
        &[1_000.0; 6],
    );
    let translated = run(
        "translated-run",
        "input-burst",
        99,
        PerformanceMetricKind::ThroughputOpsPerSec,
        &[1_200.0; 6],
    );

    let report = compare_performance_runs(
        &source,
        &translated,
        &PerformanceDiffConfig::certification_default(),
    );

    assert_eq!(report.verdict, PerformanceDiffVerdict::Improvement);
    assert!(report.certification_passed);
    assert!(
        report.comparisons[0].effective_relative_regression < 0.0,
        "higher throughput should be scored as negative effective regression"
    );
}

#[test]
fn insufficient_samples_require_more_evidence() {
    let source = run(
        "source-run",
        "resize-storm",
        5,
        PerformanceMetricKind::FrameJitterMs,
        &[2.0, 2.1],
    );
    let translated = run(
        "translated-run",
        "resize-storm",
        5,
        PerformanceMetricKind::FrameJitterMs,
        &[2.0, 2.2],
    );

    let report = compare_performance_runs(
        &source,
        &translated,
        &PerformanceDiffConfig::certification_default(),
    );

    assert_eq!(report.verdict, PerformanceDiffVerdict::NeedsMoreEvidence);
    assert!(!report.certification_passed);
    assert_eq!(
        report.differences[0].difference_kind,
        PerformanceDifferenceKind::InsufficientSamples
    );
    assert!(report.violated_policy_ids.contains(&"PD-002".to_string()));
}

#[test]
fn workload_seed_mismatch_fails_as_uncontrolled_benchmark() {
    let source = run(
        "source-run",
        "pane-drag",
        1,
        PerformanceMetricKind::LatencyMeanMs,
        &[10.0; 5],
    );
    let translated = run(
        "translated-run",
        "pane-drag",
        2,
        PerformanceMetricKind::LatencyMeanMs,
        &[10.0; 5],
    );

    let report = compare_performance_runs(
        &source,
        &translated,
        &PerformanceDiffConfig::certification_default(),
    );

    assert_eq!(report.verdict, PerformanceDiffVerdict::PolicyRegression);
    assert!(!report.certification_passed);
    assert_eq!(
        report.differences[0].difference_kind,
        PerformanceDifferenceKind::UncontrolledWorkload
    );
    assert_eq!(
        report.differences[0].risk_level,
        TransformationRiskLevel::Critical
    );
    assert!(report.violated_policy_ids.contains(&"PD-001".to_string()));
}
