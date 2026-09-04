#![cfg(feature = "experimental")]
#![forbid(unsafe_code)]

//! bd-2xj.2: CI test — validates slo.yaml schema and exercises safe-mode replay.
//!
//! Run:
//!   cargo test -p ftui-runtime --test slo_yaml_validation

use ftui_runtime::slo::{
    BreachSeverity, MetricType, SafeModeDecision, check_breach, check_safe_mode, parse_slo_yaml,
    run_slo_check,
};

// ============================================================================
// Load the real slo.yaml from project root
// ============================================================================

fn load_slo_yaml() -> &'static str {
    include_str!("../../../slo.yaml")
}

// ============================================================================
// Schema Validation
// ============================================================================

#[test]
fn slo_yaml_parses_without_errors() {
    let yaml = load_slo_yaml();
    let schema = parse_slo_yaml(yaml).unwrap_or_else(|errors| {
        panic!(
            "slo.yaml has {} validation errors:\n{}",
            errors.len(),
            errors
                .iter()
                .map(|e| format!("  - {e}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    });

    assert!(
        !schema.metrics.is_empty(),
        "slo.yaml should define at least one metric"
    );
}

#[test]
fn slo_yaml_has_data_plane_metrics() {
    let schema = parse_slo_yaml(load_slo_yaml()).unwrap();

    let data_plane_prefixes = [
        "render_frame_",
        "layout_compute_",
        "diff_strategy_",
        "ansi_present_",
        "heap_rss_bytes",
        "allocations_per_frame",
    ];

    for prefix in &data_plane_prefixes {
        let found = schema.metrics.keys().any(|k| k.starts_with(prefix));
        assert!(
            found,
            "slo.yaml should define data-plane metric with prefix '{prefix}'"
        );
    }
}

#[test]
fn slo_yaml_has_decision_plane_metrics() {
    let schema = parse_slo_yaml(load_slo_yaml()).unwrap();

    let decision_plane_prefixes = [
        "posterior_update_",
        "voi_computation_",
        "conformal_predict_",
        "eprocess_update_",
        "bocpd_update_",
        "cascade_decision_",
    ];

    for prefix in &decision_plane_prefixes {
        let found = schema.metrics.keys().any(|k| k.starts_with(prefix));
        assert!(
            found,
            "slo.yaml should define decision-plane metric with prefix '{prefix}'"
        );
    }
}

#[test]
fn slo_yaml_has_error_rate_metrics() {
    let schema = parse_slo_yaml(load_slo_yaml()).unwrap();

    let error_metrics: Vec<_> = schema
        .metrics
        .iter()
        .filter(|(_, v)| v.metric_type == MetricType::ErrorRate)
        .collect();

    assert!(
        !error_metrics.is_empty(),
        "slo.yaml should define at least one error_rate metric"
    );

    // Error rate max_value should be <= 1.0
    for (name, slo) in &error_metrics {
        if let Some(max_val) = slo.max_value {
            assert!(
                max_val <= 1.0,
                "error_rate metric '{name}' has max_value {max_val} > 1.0"
            );
        }
    }
}

#[test]
fn slo_yaml_has_safe_mode_triggers() {
    let schema = parse_slo_yaml(load_slo_yaml()).unwrap();

    let trigger_count = schema
        .metrics
        .values()
        .filter(|v| v.safe_mode_trigger)
        .count();

    assert!(
        trigger_count >= 2,
        "slo.yaml should have at least 2 safe-mode trigger metrics, found {trigger_count}"
    );
}

#[test]
fn slo_yaml_all_latency_metrics_have_percentile_suffix() {
    let schema = parse_slo_yaml(load_slo_yaml()).unwrap();

    for (name, slo) in &schema.metrics {
        if slo.metric_type == MetricType::Latency {
            let valid_suffix = name.ends_with("_us")
                || name.ends_with("_ms")
                || name.contains("_p50")
                || name.contains("_p95")
                || name.contains("_p99")
                || name.contains("_p999");
            assert!(
                valid_suffix,
                "latency metric '{name}' should have a percentile or unit suffix"
            );
        }
    }
}

#[test]
fn slo_yaml_metric_count_minimum() {
    let schema = parse_slo_yaml(load_slo_yaml()).unwrap();
    assert!(
        schema.metrics.len() >= 20,
        "slo.yaml should define at least 20 metrics for comprehensive coverage, found {}",
        schema.metrics.len()
    );
}

// ============================================================================
// Deterministic Replay: Normal Operation
// ============================================================================

#[test]
fn replay_all_within_slo() {
    let schema = parse_slo_yaml(load_slo_yaml()).unwrap();

    // Simulate all metrics at 90% of their max_value (well within bounds)
    let observations: Vec<(&str, f64, f64)> = schema
        .metrics
        .iter()
        .filter_map(|(name, slo)| {
            slo.max_value.map(|max_val| {
                let baseline = max_val * 0.7;
                let current = max_val * 0.7; // no change
                (name.as_str(), baseline, current)
            })
        })
        .collect();

    let (breaches, decision) = run_slo_check(&schema, &observations);

    assert_eq!(
        decision,
        SafeModeDecision::Normal,
        "all metrics within SLO should not trigger safe-mode"
    );

    for b in &breaches {
        assert_eq!(
            b.severity,
            BreachSeverity::None,
            "metric '{}' should be within SLO (ratio={:.3})",
            b.metric_name,
            b.ratio
        );
    }
}

// ============================================================================
// Deterministic Replay: Breach Detection
// ============================================================================

#[test]
fn replay_single_absolute_breach() {
    let schema = parse_slo_yaml(load_slo_yaml()).unwrap();

    // Find the first metric with a max_value and exceed it
    let (name, slo) = schema
        .metrics
        .iter()
        .find(|(_, v)| v.max_value.is_some())
        .expect("should have at least one metric with max_value");

    let max_val = slo.max_value.unwrap();
    let result = check_breach(name, max_val * 0.8, max_val * 1.1, &schema);

    assert_eq!(
        result.severity,
        BreachSeverity::AbsoluteBreach,
        "exceeding max_value should produce AbsoluteBreach for '{name}'"
    );
}

#[test]
fn replay_ratio_breach() {
    let schema = parse_slo_yaml(load_slo_yaml()).unwrap();

    // Find a metric with max_ratio and exceed just the ratio (not absolute)
    let (name, slo) = schema
        .metrics
        .iter()
        .find(|(_, v)| v.max_ratio.is_some() && v.max_value.is_some())
        .expect("should have metric with both max_ratio and max_value");

    let max_ratio = slo.max_ratio.unwrap();
    let max_val = slo.max_value.unwrap();
    // Set baseline low enough that ratio breach fires before absolute breach
    let baseline = max_val * 0.3;
    let current = baseline * (max_ratio + 0.05); // just over ratio

    if current < max_val {
        let result = check_breach(name, baseline, current, &schema);
        assert_eq!(
            result.severity,
            BreachSeverity::Breach,
            "exceeding max_ratio (but not max_value) should produce Breach for '{name}'"
        );
    }
}

// ============================================================================
// Deterministic Replay: Safe-Mode Trigger
// ============================================================================

#[test]
fn replay_safe_mode_via_trigger_flag() {
    let schema = parse_slo_yaml(load_slo_yaml()).unwrap();

    // Find a metric with safe_mode_trigger=true and breach it
    let (name, slo) = schema
        .metrics
        .iter()
        .find(|(_, v)| v.safe_mode_trigger && v.max_value.is_some())
        .expect("should have safe_mode_trigger metric with max_value");

    let max_val = slo.max_value.unwrap();
    let observations = vec![(name.as_str(), max_val * 0.8, max_val * 1.2)];

    let (_, decision) = run_slo_check(&schema, &observations);

    assert!(
        matches!(decision, SafeModeDecision::Triggered(ref r) if r.contains("safe_mode_trigger")),
        "breaching a safe_mode_trigger metric should trigger safe-mode, got: {decision:?}"
    );
}

#[test]
fn replay_safe_mode_via_breach_count() {
    let schema = parse_slo_yaml(load_slo_yaml()).unwrap();

    // Breach enough metrics to exceed safe_mode_breach_count
    let n = schema.safe_mode_breach_count;
    let observations: Vec<(&str, f64, f64)> = schema
        .metrics
        .iter()
        .filter(|(_, v)| !v.safe_mode_trigger && v.max_value.is_some())
        .take(n)
        .map(|(name, slo)| {
            let max_val = slo.max_value.unwrap();
            (name.as_str(), max_val * 0.8, max_val * 1.5)
        })
        .collect();

    if observations.len() >= n {
        let breaches: Vec<_> = observations
            .iter()
            .map(|(name, baseline, current)| check_breach(name, *baseline, *current, &schema))
            .collect();

        let decision = check_safe_mode(&breaches, &schema);

        assert!(
            matches!(decision, SafeModeDecision::Triggered(ref r) if r.contains("simultaneous")),
            "breaching {n} metrics should trigger safe-mode via breach count, got: {decision:?}"
        );
    }
}

#[test]
fn replay_safe_mode_via_error_rate() {
    let schema = parse_slo_yaml(load_slo_yaml()).unwrap();

    // Find an error_rate metric WITHOUT safe_mode_trigger to test the error rate path
    let error_metric = schema
        .metrics
        .iter()
        .find(|(_, v)| v.metric_type == MetricType::ErrorRate && !v.safe_mode_trigger);

    if let Some((name, _)) = error_metric {
        let observations = [(
            name.as_str(),
            0.01,
            schema.safe_mode_error_rate + 0.05, // exceed global error rate threshold
        )];

        let breaches: Vec<_> = observations
            .iter()
            .map(|(n, b, c)| check_breach(n, *b, *c, &schema))
            .collect();

        let decision = check_safe_mode(&breaches, &schema);

        assert!(
            matches!(decision, SafeModeDecision::Triggered(ref r) if r.contains("error rate")),
            "exceeding safe_mode_error_rate should trigger safe-mode, got: {decision:?}"
        );
    }
}

// ============================================================================
// Deterministic Replay: Safe-Mode NOT Triggered
// ============================================================================

#[test]
fn replay_single_breach_no_safe_mode() {
    let schema = parse_slo_yaml(load_slo_yaml()).unwrap();

    // Breach exactly one non-trigger metric (below safe_mode_breach_count)
    let (name, slo) = schema
        .metrics
        .iter()
        .find(|(_, v)| !v.safe_mode_trigger && v.max_value.is_some())
        .expect("should have non-trigger metric");

    let max_val = slo.max_value.unwrap();
    let observations = vec![(name.as_str(), max_val * 0.8, max_val * 1.2)];

    let (_, decision) = run_slo_check(&schema, &observations);

    assert_eq!(
        decision,
        SafeModeDecision::Normal,
        "single non-trigger breach should not trigger safe-mode"
    );
}

// ============================================================================
// Schema Consistency Checks
// ============================================================================

#[test]
fn slo_yaml_regression_threshold_sensible() {
    let schema = parse_slo_yaml(load_slo_yaml()).unwrap();
    assert!(
        schema.regression_threshold >= 0.05 && schema.regression_threshold <= 0.30,
        "regression_threshold should be between 5% and 30%, got {:.0}%",
        schema.regression_threshold * 100.0
    );
}

#[test]
fn slo_yaml_noise_tolerance_less_than_regression() {
    let schema = parse_slo_yaml(load_slo_yaml()).unwrap();
    assert!(
        schema.noise_tolerance < schema.regression_threshold,
        "noise_tolerance ({}) must be < regression_threshold ({})",
        schema.noise_tolerance,
        schema.regression_threshold
    );
}

#[test]
fn slo_yaml_max_ratios_above_one() {
    let schema = parse_slo_yaml(load_slo_yaml()).unwrap();
    for (name, slo) in &schema.metrics {
        if let Some(max_ratio) = slo.max_ratio {
            assert!(
                max_ratio > 1.0,
                "metric '{name}' has max_ratio {max_ratio} <= 1.0 (would always breach)"
            );
        }
    }
}

#[test]
fn slo_yaml_max_values_positive() {
    let schema = parse_slo_yaml(load_slo_yaml()).unwrap();
    for (name, slo) in &schema.metrics {
        if let Some(max_val) = slo.max_value {
            assert!(
                max_val > 0.0,
                "metric '{name}' has max_value {max_val} <= 0.0"
            );
        }
    }
}

#[test]
fn slo_yaml_latency_ordering_consistent() {
    let schema = parse_slo_yaml(load_slo_yaml()).unwrap();

    // For each family, p50 < p95 < p99 < p999 max_values
    let families = [
        "render_frame",
        "layout_compute",
        "diff_strategy",
        "ansi_present",
        "posterior_update",
        "voi_computation",
    ];

    for family in &families {
        let percentiles = ["p50", "p95", "p99", "p999"];
        let mut prev_max = 0.0f64;

        for pct in &percentiles {
            let key = format!("{family}_{pct}_us");
            if let Some(slo) = schema.metrics.get(&key)
                && let Some(max_val) = slo.max_value
            {
                assert!(
                    max_val >= prev_max,
                    "metric '{key}' max_value {max_val} < previous percentile {prev_max}"
                );
                prev_max = max_val;
            }
        }
    }
}

// ============================================================================
// Idempotency: parsing is deterministic
// ============================================================================

#[test]
fn slo_yaml_parse_is_deterministic() {
    let yaml = load_slo_yaml();
    let schema1 = parse_slo_yaml(yaml).unwrap();
    let schema2 = parse_slo_yaml(yaml).unwrap();

    assert_eq!(schema1.regression_threshold, schema2.regression_threshold);
    assert_eq!(schema1.noise_tolerance, schema2.noise_tolerance);
    assert_eq!(
        schema1.safe_mode_breach_count,
        schema2.safe_mode_breach_count
    );
    assert_eq!(schema1.metrics.len(), schema2.metrics.len());

    for (name, slo1) in &schema1.metrics {
        let slo2 = schema2.metrics.get(name).unwrap();
        assert_eq!(slo1.metric_type, slo2.metric_type);
        assert_eq!(slo1.max_value, slo2.max_value);
        assert_eq!(slo1.max_ratio, slo2.max_ratio);
        assert_eq!(slo1.safe_mode_trigger, slo2.safe_mode_trigger);
    }
}
