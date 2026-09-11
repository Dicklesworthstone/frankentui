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

// Synthetic comparator inputs; these are not measured performance evidence.
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
fn tail_latency_comparator_requires_a_nonempty_latency_baseline() {
    let config = GauntletConfig::default_strict();
    let latency = latency_metric("frame_pipeline_total", 100.0, 150.0);
    let diagnostic = MetricBaseline {
        category: MetricCategory::OutputCost,
        ..latency.clone()
    };
    for baseline in [vec![], vec![diagnostic]] {
        let failures =
            compare_tail_latency("empty", &baseline, std::slice::from_ref(&latency), &config);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(failures[0].category, FailureCategory::ObservabilityGap);
        assert!(failures[0].reason.contains("no latency metrics"));
    }
}

#[test]
fn tail_latency_comparator_requires_every_baseline_metric() {
    let config = GauntletConfig::default_strict();
    let first = latency_metric("first", 100.0, 150.0);
    let second = latency_metric("second", 100.0, 150.0);
    let baseline = [first.clone(), second.clone()];
    let unrelated = latency_metric("diagnostic", 100.0, 150.0);
    let wrong_category = MetricBaseline {
        category: MetricCategory::Throughput,
        ..second.clone()
    };
    for replacement in [unrelated, wrong_category] {
        let failures = compare_tail_latency(
            "coverage",
            &baseline,
            &[first.clone(), replacement],
            &config,
        );
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(failures[0].category, FailureCategory::ObservabilityGap);
        assert!(failures[0].reason.contains("'second' missing"));
    }
    // Input order is irrelevant; additional valid latency diagnostics are allowed.
    let complete = [second, latency_metric("extra", 10.0, 20.0), first];
    assert!(compare_tail_latency("coverage", &baseline, &complete, &config).is_empty());
}

#[test]
fn tail_latency_comparator_rejects_nonfinite_or_negative_statistics() {
    let config = GauntletConfig::default_strict();
    let valid = latency_metric("frame_pipeline_total", 100.0, 150.0);
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
        for field in [
            "min", "p50", "p95", "p99", "p999", "max", "mean", "stddev", "cv",
        ] {
            let mut invalid = valid.clone();
            match field {
                "min" => invalid.percentiles.min = value,
                "p50" => invalid.percentiles.p50 = value,
                "p95" => invalid.percentiles.p95 = value,
                "p99" => invalid.percentiles.p99 = value,
                "p999" => invalid.percentiles.p999 = value,
                "max" => invalid.percentiles.max = value,
                "mean" => invalid.mean = value,
                "stddev" => invalid.stddev = value,
                "cv" => invalid.cv = value,
                _ => unreachable!(),
            }
            for (baseline, candidate) in [(&invalid, &valid), (&valid, &invalid)] {
                let failures = compare_tail_latency(
                    "invalid-statistic",
                    std::slice::from_ref(baseline),
                    std::slice::from_ref(candidate),
                    &config,
                );
                assert_eq!(failures.len(), 1, "{field}={value}: {failures:?}");
                assert_eq!(failures[0].category, FailureCategory::ObservabilityGap);
            }
        }
    }
}

#[test]
fn tail_latency_comparator_rejects_malformed_latency_records() {
    let config = GauntletConfig::default_strict();
    let valid = latency_metric("frame_pipeline_total", 100.0, 150.0);
    for defect in ["name", "unit", "samples", "order"] {
        let mut invalid = valid.clone();
        match defect {
            "name" => invalid.metric = " ".to_string(),
            "unit" => invalid.unit.clear(),
            "samples" => invalid.sample_count = 0,
            "order" => invalid.percentiles.p999 = invalid.percentiles.p95,
            _ => unreachable!(),
        }
        for (baseline, candidate) in [(&invalid, &valid), (&valid, &invalid)] {
            let failures = compare_tail_latency(
                "malformed",
                std::slice::from_ref(baseline),
                std::slice::from_ref(candidate),
                &config,
            );
            assert!(!failures.is_empty(), "{defect}");
            assert!(
                failures
                    .iter()
                    .all(|f| f.category == FailureCategory::ObservabilityGap)
            );
            assert!(
                failures.iter().any(|f| f.reason.contains("invalid")),
                "{defect}: {failures:?}"
            );
        }
    }
}

#[test]
fn tail_latency_comparator_rejects_ambiguous_duplicates_and_units() {
    let config = GauntletConfig::default_strict();
    let valid = latency_metric("frame_pipeline_total", 100.0, 150.0);
    for other in [
        valid.clone(),
        latency_metric("frame_pipeline_total", 10.0, 20.0),
    ] {
        for duplicates in [[valid.clone(), other.clone()], [other, valid.clone()]] {
            for (baseline, candidate) in [
                (duplicates.as_slice(), std::slice::from_ref(&valid)),
                (std::slice::from_ref(&valid), duplicates.as_slice()),
            ] {
                let failures = compare_tail_latency("duplicate", baseline, candidate, &config);
                assert_eq!(failures.len(), 1, "{failures:?}");
                assert_eq!(failures[0].category, FailureCategory::ObservabilityGap);
                assert!(failures[0].reason.contains("duplicate"));
            }
        }
    }
    let candidate = MetricBaseline {
        unit: "ms".to_string(),
        ..valid.clone()
    };
    let failures = compare_tail_latency("units", &[valid], &[candidate], &config);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].category, FailureCategory::ObservabilityGap);
    assert!(failures[0].reason.contains("mismatched units"));
}

#[test]
fn tail_latency_comparator_zero_baseline_is_a_zero_budget() {
    let config = GauntletConfig::default_strict();
    let zero = [latency_metric("frame_pipeline_total", 0.0, 0.0)];
    assert!(compare_tail_latency("zero", &zero, &zero, &config).is_empty());
    let positive = [latency_metric("frame_pipeline_total", 1.0, 2.0)];
    let failures = compare_tail_latency("zero", &zero, &positive, &config);
    assert_eq!(failures.len(), 2, "{failures:?}");
    assert!(
        failures
            .iter()
            .all(|f| f.category == FailureCategory::TailRegression)
    );
    assert!(compare_tail_latency("improved", &positive, &zero, &config).is_empty());
}

#[test]
fn tail_latency_comparator_rejects_invalid_thresholds_and_overflow() {
    let valid = [latency_metric("frame_pipeline_total", 100.0, 150.0)];
    for threshold in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
        for p95 in [false, true] {
            let mut config = GauntletConfig::default_strict();
            if p95 {
                config.p95_regression_threshold_pct = threshold;
            } else {
                config.p99_regression_threshold_pct = threshold;
            }
            let failures = compare_tail_latency("threshold", &valid, &valid, &config);
            assert_eq!(failures.len(), 1, "{failures:?}");
            assert_eq!(failures[0].category, FailureCategory::ObservabilityGap);
            assert!(failures[0].reason.contains("threshold"));
        }
    }
    let mut extreme = valid[0].clone();
    extreme.percentiles.p99 = f64::MAX;
    extreme.percentiles.p999 = f64::MAX;
    extreme.percentiles.max = f64::MAX;
    let baseline = [extreme];
    let failures = compare_tail_latency(
        "overflow",
        &baseline,
        &baseline,
        &GauntletConfig::default_strict(),
    );
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].category, FailureCategory::ObservabilityGap);
    assert!(failures[0].reason.contains("ceiling"));

    let config = GauntletConfig {
        p95_regression_threshold_pct: 0.0,
        p99_regression_threshold_pct: 0.0,
        ..GauntletConfig::default_strict()
    };
    assert!(compare_tail_latency("finite-max", &baseline, &baseline, &config).is_empty());
}

#[test]
fn tail_latency_comparator_preserves_finite_interpolation_roundoff() {
    let mut metric = latency_metric("fractional", 0.1, 0.1);
    metric.percentiles = Percentiles {
        min: 0.1,
        p50: 0.1,
        p95: 0.1,
        p99: 0.1 * (1.0 + f64::EPSILON),
        p999: 0.1,
        max: 0.1,
    };
    metric.mean = 0.1;
    metric.stddev = 0.0;
    metric.cv = 0.0;
    let metrics = [metric];
    let failures = compare_tail_latency(
        "roundoff",
        &metrics,
        &metrics,
        &GauntletConfig::default_strict(),
    );
    assert!(failures.is_empty(), "{failures:?}");
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
