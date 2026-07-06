//! E2E driver for the render gauntlet (bd-40lhe): render equivalence,
//! deterministic replay, tail-latency capture, certificate shadow-safety,
//! adversarial challenge fixtures, and negative controls over the canonical
//! fixture registry.

use ftui_harness::baseline_capture::{
    FixtureFamily, MetricBaseline, MetricCategory, Percentiles, StabilityClass,
};
use ftui_harness::fixture_suite::{FixtureRegistry, SuitePartition};
use ftui_harness::render_gauntlet::{
    FailureCategory, GauntletConfig, GauntletGate, GauntletSuite, compare_tail_latency,
};

fn latency_metric(name: &str, p95: f64, p99: f64) -> MetricBaseline {
    MetricBaseline {
        metric: name.to_string(),
        category: MetricCategory::Latency,
        unit: "us".to_string(),
        sample_count: 30,
        mean: p95 * 0.6,
        stddev: p95 * 0.02,
        cv: 0.02,
        stability: StabilityClass::Stable,
        percentiles: Percentiles {
            p50: p95 * 0.5,
            p95,
            p99,
            p999: p99 * 1.1,
            min: p95 * 0.2,
            max: p99 * 1.2,
        },
    }
}

#[test]
fn strict_gauntlet_passes_all_six_gates() {
    let suite = GauntletSuite::new(GauntletConfig::default_strict());
    let report = suite.run_all();
    assert_eq!(report.gate_results.len(), 6, "{}", report.summary());
    assert!(report.passed(), "{}", report.summary());
    assert!(
        report.real_failures().is_empty(),
        "real failures: {:?}",
        report.real_failures()
    );
    for result in &report.gate_results {
        assert!(
            result.fixtures_tested > 0,
            "{} tested nothing",
            result.gate.label()
        );
    }
}

#[test]
fn fast_config_activates_four_gates() {
    let suite = GauntletSuite::new(GauntletConfig::fast());
    let gates = suite.active_gates();
    assert_eq!(gates.len(), 4);
    assert!(!gates.contains(&GauntletGate::Challenge));
    assert!(!gates.contains(&GauntletGate::Certificate));
    let report = suite.run_all();
    assert_eq!(report.gate_results.len(), 4);
    assert!(report.passed(), "{}", report.summary());
}

#[test]
fn gauntlet_report_is_replay_stable() {
    let suite = GauntletSuite::new(GauntletConfig::fast());
    let first = suite.run_all();
    let second = suite.run_all();
    // Wall-clock durations differ; the verdict surface must not.
    let verdicts = |report: &ftui_harness::render_gauntlet::GauntletReport| {
        report
            .gate_results
            .iter()
            .map(|r| (r.gate, r.passed, r.fixtures_tested, r.fixtures_passed))
            .collect::<Vec<_>>()
    };
    assert_eq!(verdicts(&first), verdicts(&second));
}

#[test]
fn tail_latency_comparator_flags_regressions_deterministically() {
    let config = GauntletConfig::default_strict();
    let baseline = vec![latency_metric("frame_pipeline_total", 100.0, 150.0)];

    // Identical candidate: clean.
    let clean = compare_tail_latency("fixture-a", &baseline, &baseline, &config);
    assert!(clean.is_empty(), "{clean:?}");

    // Within threshold (10% p95 / 15% p99): clean.
    let within = vec![latency_metric("frame_pipeline_total", 109.0, 172.0)];
    assert!(compare_tail_latency("fixture-a", &baseline, &within, &config).is_empty());

    // Beyond threshold: flagged as TailRegression on both percentiles.
    let regressed = vec![latency_metric("frame_pipeline_total", 130.0, 200.0)];
    let failures = compare_tail_latency("fixture-a", &baseline, &regressed, &config);
    assert_eq!(failures.len(), 2, "{failures:?}");
    assert!(
        failures
            .iter()
            .all(|f| f.category == FailureCategory::TailRegression)
    );
    assert!(failures.iter().all(|f| f.category.is_real_failure()));
    assert!(failures.iter().any(|f| f.reason.contains("p95")));
    assert!(failures.iter().any(|f| f.reason.contains("p99")));

    // Missing candidate metric: observability gap, never silent.
    let missing = compare_tail_latency("fixture-a", &baseline, &[], &config);
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].category, FailureCategory::ObservabilityGap);
}

#[test]
fn failure_artifacts_are_diagnostic_per_gate() {
    for gate in GauntletGate::ALL {
        assert!(
            !gate.failure_artifacts().is_empty(),
            "{} has no failure artifacts",
            gate.label()
        );
    }
    // The tail gate names replayable diagnostics, not just a boolean.
    assert!(
        GauntletGate::TailLatency
            .failure_artifacts()
            .contains(&"p99_regression_detail.json")
    );
    assert!(
        GauntletGate::Certificate
            .failure_artifacts()
            .contains(&"stale_frame_evidence.json")
    );
}

#[test]
fn registry_covers_all_three_partitions_for_the_gauntlet() {
    let registry = FixtureRegistry::canonical();
    assert!(
        registry
            .by_family(FixtureFamily::Render)
            .iter()
            .any(|s| s.partition == SuitePartition::Canonical)
    );
    assert!(!registry.by_partition(SuitePartition::Challenge).is_empty());
    assert!(
        !registry
            .by_partition(SuitePartition::NegativeControl)
            .is_empty()
    );
}

#[test]
fn report_json_names_every_gate() {
    let suite = GauntletSuite::new(GauntletConfig::fast());
    let report = suite.run_all();
    let json = report.to_json();
    for result in &report.gate_results {
        assert!(
            json.contains(result.gate.label()),
            "missing {}",
            result.gate.label()
        );
    }
}
