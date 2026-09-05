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
    assert_eq!(
        bundle.replay_command,
        "doctor_frankentui performance-diff --input replay-input.json"
    );
    let archive: serde_json::Value = serde_json::from_str(
        &bundle
            .files
            .iter()
            .find(|f| f.path == "replay-input.json")
            .expect("archived inputs")
            .content,
    )
    .expect("archive JSON");
    assert_eq!(archive["source_run"]["run_id"], "source-run");
    assert_eq!(archive["translated_run"]["run_id"], "translated-run");
    assert!(bundle.files.iter().any(|file| file.path == "replay.sh"));
    assert!(
        bundle
            .files
            .iter()
            .any(|file| file.path == "performance_diffs.jsonl")
    );
}

#[cfg(unix)]
#[test]
fn archived_performance_bundle_relocates_and_executes_real_comparator() {
    use std::{fs, process::Command};

    // These archived samples are controlled comparator inputs, not fresh
    // benchmark measurements. The generated script and CLI execute for real.
    let source = run(
        "source-run",
        "replay",
        41,
        PerformanceMetricKind::LatencyP99Ms,
        &[0.18181818181818182; 8],
    );
    let translated = run(
        "translated-run",
        "replay",
        41,
        PerformanceMetricKind::LatencyP99Ms,
        &[0.3; 8],
    );
    let mut config = PerformanceDiffConfig::certification_default();
    config.confidence_z = 2.5000000000000004;
    let report = compare_performance_runs(&source, &translated, &config);
    assert!(!report.certification_passed);
    let bundle = report
        .artifact_bundle
        .as_ref()
        .expect("real producer bundle");
    let temp = tempfile::tempdir().expect("tempdir");
    let original = temp.path().join("original");
    fs::create_dir(&original).expect("artifact directory");
    for file in &bundle.files {
        fs::write(original.join(&file.path), &file.content).expect("materialize producer artifact");
    }
    let relocated = temp.path().join("relocated archive with spaces");
    fs::rename(&original, &relocated).expect("relocate actual artifacts");
    let binary = env!("CARGO_BIN_EXE_doctor_frankentui");
    let rejected = Command::new("bash")
        .arg(relocated.join("replay.sh"))
        .env("DOCTOR_FRANKENTUI_BIN", binary)
        .current_dir(temp.path())
        .output()
        .expect("execute generated replay");
    assert_eq!(
        rejected.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&rejected.stdout).expect("real CLI report");
    assert_eq!(payload["replay_scope"], "archived_comparison");
    // Compare the complete wire bytes: the default JSON float parser can lose
    // one ULP, so decoding an expected native f64 would weaken this check.
    assert_eq!(
        rejected.stdout,
        format!(
            "{}\n",
            serde_json::json!({"replay_scope": "archived_comparison", "report": report})
        )
        .into_bytes()
    );

    let archive_path = relocated.join("replay-input.json");
    let mut archive: serde_json::Value =
        serde_json::from_slice(&fs::read(&archive_path).expect("archive")).expect("archive JSON");
    assert_eq!(
        serde_json::from_value::<PerformanceDiffConfig>(archive["config"].clone())
            .expect("archived config"),
        config
    );
    assert_eq!(
        serde_json::from_value::<PerformanceRun>(archive["source_run"].clone())
            .expect("archived source samples"),
        source
    );
    assert_eq!(
        serde_json::from_value::<PerformanceRun>(archive["translated_run"].clone())
            .expect("archived translated samples"),
        translated
    );
    assert_eq!(
        archive["source_run"]["replay_command"],
        source.replay_command.as_deref().expect("provenance")
    );
    archive["translated_run"] = archive["source_run"].clone();
    let equivalent = compare_performance_runs(&source, &source, &config);
    assert!(equivalent.certification_passed);
    fs::write(
        &archive_path,
        serde_json::to_vec(&archive).expect("archive JSON"),
    )
    .expect("positive archive");
    let passed = Command::new("bash")
        .arg(relocated.join("replay.sh"))
        .env("DOCTOR_FRANKENTUI_BIN", binary)
        .current_dir(temp.path())
        .output()
        .expect("execute positive comparison");
    assert!(
        passed.status.success(),
        "{}",
        String::from_utf8_lossy(&passed.stderr)
    );
    assert_eq!(
        passed.stdout,
        format!(
            "{}\n",
            serde_json::json!({"replay_scope": "archived_comparison", "report": equivalent})
        )
        .into_bytes()
    );

    let mut wrong_schema = archive.clone();
    wrong_schema["schema_version"] = serde_json::json!(999);
    let mut empty = archive;
    empty["source_run"]["samples"] = serde_json::json!([]);
    empty["translated_run"]["samples"] = serde_json::json!([]);
    for invalid in [serde_json::Value::Null, wrong_schema, empty] {
        fs::write(
            &archive_path,
            serde_json::to_vec(&invalid).expect("invalid JSON input"),
        )
        .expect("negative archive");
        let output = Command::new(binary)
            .args(["performance-diff", "--input"])
            .arg(&archive_path)
            .output()
            .expect("negative CLI");
        assert!(
            !output.status.success(),
            "invalid archive accepted: {invalid}"
        );
        assert!(
            output.stdout.is_empty(),
            "invalid archive must not produce a report"
        );
    }
}

#[cfg(unix)]
#[test]
fn archived_nonfinite_measurements_replay_without_losing_bits() {
    use std::{fs, process::Command};

    // Deliberately invalid archived measurements exercise real diagnostic replay,
    // not fresh measurements. Distinct NaN payloads and both infinities survive.
    let source = run(
        "source-nonfinite",
        "nonfinite",
        73,
        PerformanceMetricKind::LatencyP99Ms,
        &[
            f64::from_bits(0x7ff8_0000_0000_0042),
            f64::from_bits(0xfff8_0000_0000_0123),
            f64::INFINITY,
            f64::NEG_INFINITY,
            0.18181818181818182,
            1.0,
            2.0,
            3.0,
        ],
    );
    let translated = run(
        "translated-nonfinite",
        "nonfinite",
        73,
        PerformanceMetricKind::LatencyP99Ms,
        &[5.0; 8],
    );
    let config = PerformanceDiffConfig::certification_default();
    let report = compare_performance_runs(&source, &translated, &config);
    assert!(!report.certification_passed);
    assert_eq!(report.verdict, PerformanceDiffVerdict::NeedsMoreEvidence);
    assert_eq!(
        report.comparisons[0].verdict,
        MetricComparisonVerdict::Inconclusive
    );
    let bundle = report
        .artifact_bundle
        .as_ref()
        .expect("diagnostic replay bundle");
    let temp = tempfile::tempdir().expect("tempdir");
    let original = temp.path().join("original");
    fs::create_dir(&original).expect("artifact directory");
    for file in &bundle.files {
        fs::write(original.join(&file.path), &file.content).expect("actual producer artifact");
    }
    let relocated = temp.path().join("relocated nonfinite archive");
    fs::rename(&original, &relocated).expect("relocate bundle");
    let archive_path = relocated.join("replay-input.json");
    let archive: serde_json::Value =
        serde_json::from_slice(&fs::read(&archive_path).expect("archive")).expect("archive JSON");
    let decoded: PerformanceRun =
        serde_json::from_value(archive["source_run"].clone()).expect("nonfinite source roundtrip");
    assert_eq!(
        decoded
            .samples
            .iter()
            .map(|sample| sample.value.to_bits())
            .collect::<Vec<_>>(),
        source
            .samples
            .iter()
            .map(|sample| sample.value.to_bits())
            .collect::<Vec<_>>()
    );
    let binary = env!("CARGO_BIN_EXE_doctor_frankentui");
    let output = Command::new("bash")
        .arg(relocated.join("replay.sh"))
        .env("DOCTOR_FRANKENTUI_BIN", binary)
        .current_dir(temp.path())
        .output()
        .expect("actual nonfinite diagnostic replay");
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        format!(
            "{}\n",
            serde_json::json!({
                "replay_scope": "archived_comparison", "report": report
            })
        )
        .into_bytes()
    );

    for marker in [
        "nonfinite:0x3ff0000000000000",
        "nonfinite:0x7ff0",
        "nonfinite:0xgggggggggggggggg",
    ] {
        let mut invalid = archive.clone();
        invalid["source_run"]["samples"][0]["value"] = serde_json::json!(marker);
        fs::write(
            &archive_path,
            serde_json::to_vec(&invalid).expect("invalid archive"),
        )
        .expect("malformed marker");
        let output = Command::new(binary)
            .args(["performance-diff", "--input"])
            .arg(&archive_path)
            .output()
            .expect("actual malformed-marker CLI");
        assert!(!output.status.success(), "marker accepted: {marker}");
        assert!(output.stdout.is_empty());
    }

    for field in [
        "confidence_z",
        "max_relative_regression",
        "max_absolute_regression",
        "min_significant_relative_delta",
    ] {
        let mut invalid_config = config.clone();
        let value = f64::from_bits(0xfff8_0000_0000_0042);
        if field == "confidence_z" {
            invalid_config.confidence_z = value;
        } else {
            let threshold = invalid_config
                .thresholds
                .get_mut(&PerformanceMetricKind::LatencyP99Ms)
                .expect("threshold");
            match field {
                "max_relative_regression" => threshold.max_relative_regression = value,
                "max_absolute_regression" => threshold.max_absolute_regression = Some(value),
                _ => threshold.min_significant_relative_delta = value,
            }
        }
        let encoded = serde_json::to_value(&invalid_config).expect("invalid config provenance");
        let decoded: PerformanceDiffConfig = serde_json::from_value(encoded.clone())
            .expect("config preserves marker for explicit validation");
        assert_eq!(
            serde_json::to_value(decoded).expect("roundtrip config"),
            encoded
        );
        let mut invalid = archive.clone();
        invalid["config"] = encoded;
        fs::write(
            &archive_path,
            serde_json::to_vec(&invalid).expect("invalid archive"),
        )
        .expect("nonfinite config");
        let output = Command::new(binary)
            .args(["performance-diff", "--input"])
            .arg(&archive_path)
            .output()
            .expect("actual invalid-config CLI");
        assert!(
            !output.status.success(),
            "invalid configuration accepted: {field}"
        );
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("nonfinite configuration"));
    }
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
