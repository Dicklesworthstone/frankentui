//! Scripted validation for the tail-risk / regime-shift monitors (bd-zzfhe).
//!
//! This is the E2E proof the bead requires: alerting, warning, and hard-gate
//! behavior are each demonstrated end to end, the monitor self-test (which
//! runs every challenge fixture and negative control) passes, and the
//! machine-readable outputs are parseable, explained, and byte-identical
//! across runs. `scripts/perf_tail_regime_monitor_e2e.sh` wraps this test and
//! harvests the printed report JSONL into operator artifacts.

#![forbid(unsafe_code)]

use ftui_harness::tail_regime_monitor::{
    CheckKind, MetricSeries, MetricUnit, MonitorReport, TailRegimeMonitor, Verdict,
    challenge_fixtures, run_self_test,
};
use ftui_harness::validation_matrix::PerfLane;

fn series(lane: PerfLane, samples: Vec<u64>) -> MetricSeries {
    MetricSeries::new(lane, "frame_time_us", MetricUnit::Micros, samples)
}

fn emit(report: &MonitorReport, scenario: &str) {
    // Harvested by the E2E wrapper into artifacts/monitor_reports.jsonl.
    println!("MONITOR_REPORT scenario={scenario} {}", report.to_json());
}

/// Pass path: a healthy candidate proceeds without findings.
#[test]
fn e2e_pass_path_proceeds() {
    let monitor = TailRegimeMonitor::default();
    let base = series(PerfLane::Render, (0..50).map(|i| 100 + (i % 8)).collect());
    let cand = series(
        PerfLane::Render,
        (0..50).map(|i| 100 + ((i + 2) % 8)).collect(),
    );
    let report = monitor.evaluate(&base, &cand);
    emit(&report, "pass_path");
    assert_eq!(report.overall, Verdict::Pass);
    assert_eq!(report.gate_action(), "proceed");
}

/// Warning path: a mild tail regression proceeds only with review, and the
/// finding carries a human explanation naming the crossed threshold.
#[test]
fn e2e_warning_path_requires_review() {
    let monitor = TailRegimeMonitor::default();
    let base = series(PerfLane::Runtime, vec![200; 50]);
    let cand = series(
        PerfLane::Runtime,
        (0..50).map(|i| if i >= 45 { 230 } else { 200 }).collect(),
    );
    let report = monitor.evaluate(&base, &cand);
    emit(&report, "warning_path");
    assert_eq!(report.overall, Verdict::Warn);
    assert_eq!(report.gate_action(), "proceed_with_review");
    let active = report.active_findings();
    assert!(!active.is_empty());
    for finding in active {
        assert!(
            !finding.explanation.is_empty(),
            "every non-pass finding must explain itself"
        );
    }
}

/// Hard-gate path: a serious tail regression blocks rollout outright.
#[test]
fn e2e_hard_gate_blocks_rollout() {
    let monitor = TailRegimeMonitor::default();
    let base = series(PerfLane::Doctor, vec![500; 50]);
    let cand = series(
        PerfLane::Doctor,
        (0..50).map(|i| if i >= 45 { 900 } else { 500 }).collect(),
    );
    let report = monitor.evaluate(&base, &cand);
    emit(&report, "hard_gate_path");
    assert_eq!(report.overall, Verdict::HardFail);
    assert_eq!(report.gate_action(), "block_rollout");
    let hard = report
        .findings
        .iter()
        .find(|f| f.verdict == Verdict::HardFail)
        .expect("a hard finding");
    assert!(hard.explanation.contains("hard gate"));
}

/// The full self-test (challenge fixtures + negative controls) must pass:
/// every monitor fires on the failure mode it exists for and stays quiet on
/// the clean control. This is the "prove the alarm rings" gate for CI.
#[test]
fn e2e_self_test_all_fixtures_behave_as_designed() {
    let report = run_self_test();
    println!("MONITOR_SELFTEST {}", report.to_json());
    for case in &report.cases {
        assert!(
            case.passed,
            "fixture {} expected {:?} got {:?} (active: {:?})",
            case.name, case.expected, case.observed, case.active_checks
        );
        emit(&case.report, case.name);
    }
    assert!(report.passed());
}

/// Reports must be byte-identical across repeated evaluation (replay
/// determinism) and parseable as JSON with the stable vocabulary.
#[test]
fn e2e_reports_are_replayable_and_machine_readable() {
    let monitor = TailRegimeMonitor::default();
    for fixture in challenge_fixtures() {
        let a = monitor
            .evaluate(&fixture.baseline, &fixture.candidate)
            .to_json();
        let b = monitor
            .evaluate(&fixture.baseline, &fixture.candidate)
            .to_json();
        assert_eq!(
            a, b,
            "fixture {} must replay byte-identically",
            fixture.name
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&a).unwrap_or_else(|e| panic!("{} JSON: {e}", fixture.name));
        assert_eq!(
            parsed["schema_version"].as_str(),
            Some("tail-regime-monitor-v1")
        );
        let overall = parsed["overall"].as_str().expect("overall");
        assert!(["pass", "warn", "hard_fail"].contains(&overall));
        let action = parsed["gate_action"].as_str().expect("gate_action");
        assert!(["proceed", "proceed_with_review", "block_rollout"].contains(&action));
        for finding in parsed["findings"].as_array().expect("findings") {
            let check = finding["check"].as_str().expect("check");
            assert!(
                [
                    "tail_p95",
                    "tail_p99",
                    "tail_max",
                    "envelope_shift",
                    "sequential_drift",
                    "insufficient_samples",
                ]
                .contains(&check),
                "check `{check}` outside stable vocabulary"
            );
            assert!(
                finding["explanation"]
                    .as_str()
                    .is_some_and(|e| !e.is_empty()),
                "finding must carry a threshold explanation"
            );
        }
    }
}

/// The monitors cover every performance lane the rollout consumes.
#[test]
fn e2e_lanes_round_trip_through_reports() {
    let monitor = TailRegimeMonitor::default();
    for lane in [PerfLane::Render, PerfLane::Runtime, PerfLane::Doctor] {
        let base = series(lane, (0..40).map(|i| 100 + (i % 6)).collect());
        let cand = series(lane, (0..40).map(|i| 100 + ((i + 1) % 6)).collect());
        let report = monitor.evaluate(&base, &cand);
        let parsed: serde_json::Value =
            serde_json::from_str(&report.to_json()).expect("report parses");
        assert_eq!(parsed["lane"].as_str(), Some(lane.label()));
    }
}

/// Negative control for the scripted path itself: verify a report that
/// SHOULD alert cannot be mistaken for a pass by the harvesting layer.
#[test]
fn e2e_negative_control_alerting_is_visible_in_json() {
    let monitor = TailRegimeMonitor::default();
    let fixtures = challenge_fixtures();
    let regression = fixtures
        .iter()
        .find(|f| f.name == "mean_masked_tail_regression")
        .expect("mean-masked fixture");
    let report = monitor.evaluate(&regression.baseline, &regression.candidate);
    let parsed: serde_json::Value = serde_json::from_str(&report.to_json()).expect("report parses");
    assert_eq!(parsed["overall"].as_str(), Some("hard_fail"));
    assert_eq!(parsed["gate_action"].as_str(), Some("block_rollout"));
    let has_hard_tail = parsed["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .any(|f| {
            f["check"].as_str() == Some("tail_p99") && f["verdict"].as_str() == Some("hard_fail")
        });
    assert!(
        has_hard_tail,
        "the mean-masked regression must be visible as a hard p99 finding in JSON"
    );
    assert_eq!(
        report
            .active_findings()
            .iter()
            .filter(|f| f.check == CheckKind::TailP99)
            .count(),
        1
    );
}
