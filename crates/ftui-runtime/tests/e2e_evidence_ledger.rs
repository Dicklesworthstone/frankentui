#![forbid(unsafe_code)]
#![cfg(feature = "telemetry")]
//! E2E tests for the Unified Evidence Ledger (bd-fp38v.1).
//!
//! Verifies that all 7 Bayesian decision points produce valid, parseable JSONL
//! output through the EvidenceSink, with correct schema fields and types.

use std::time::Instant;

use ftui_runtime::telemetry::{BayesianEvidence, DecisionDomain, EvidenceTerm};
use ftui_runtime::{
    ConformalPrediction, EvidenceSink, EvidenceSinkConfig, StrategyEvidence, ThrottleDecision,
    ThrottleLog, VoiDecision, VoiObservation,
};

use ftui_render::diff_strategy::DiffStrategy;
use ftui_runtime::bocpd::{BocpdEvidence, BocpdRegime};
use ftui_runtime::conformal_predictor::{BucketKey, DiffBucket, ModeBucket};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn tmp_sink() -> (tempfile::NamedTempFile, EvidenceSink) {
    let file = tempfile::NamedTempFile::new().unwrap();
    let config = EvidenceSinkConfig::enabled_file(file.path());
    let sink = EvidenceSink::from_config(&config).unwrap().unwrap();
    (file, sink)
}

fn read_lines(file: &tempfile::NamedTempFile) -> Vec<String> {
    let content = std::fs::read_to_string(file.path()).unwrap();
    content.lines().map(String::from).collect()
}

fn parse_json(line: &str) -> serde_json::Value {
    serde_json::from_str(line).unwrap_or_else(|e| panic!("Invalid JSON: {e}\nLine: {line}"))
}

// ---------------------------------------------------------------------------
// 1. BayesianEvidence (unified schema)
// ---------------------------------------------------------------------------

#[test]
fn e2e_bayesian_evidence_all_domains_roundtrip() {
    let (file, sink) = tmp_sink();

    let domains = [
        (DecisionDomain::DiffStrategy, "diff_strategy"),
        (DecisionDomain::ResizeCoalescing, "resize_coalescing"),
        (DecisionDomain::FrameBudget, "frame_budget"),
        (DecisionDomain::Degradation, "degradation"),
        (DecisionDomain::VOISampling, "voi_sampling"),
        (DecisionDomain::HintRanking, "hint_ranking"),
        (DecisionDomain::PaletteScoring, "palette_scoring"),
    ];

    for (i, (domain, _domain_str)) in domains.iter().enumerate() {
        let evidence = BayesianEvidence {
            decision_id: format!("test-{i}"),
            timestamp_ns: 1_000_000 * (i as u64 + 1),
            domain: *domain,
            prior_log_odds: -0.5 + i as f64 * 0.1,
            evidence_terms: vec![
                EvidenceTerm::new("term_a", 1.23),
                EvidenceTerm::new("term_b", -0.45),
                EvidenceTerm::new("term_c", 0.67),
            ],
            posterior_log_odds: 0.5 + i as f64 * 0.1,
            action: format!("action_{i}"),
            expected_loss: 0.01 * (i as f64 + 1.0),
            confidence_level: 0.95,
            fallback_triggered: i % 2 == 0,
        };
        sink.write_jsonl(&evidence.to_jsonl()).unwrap();
    }
    sink.flush().unwrap();

    let lines = read_lines(&file);
    assert_eq!(lines.len(), 7, "Expected one line per domain");

    for (i, line) in lines.iter().enumerate() {
        let v = parse_json(line);
        let (_, domain_str) = &domains[i];

        // Required fields
        assert_eq!(v["id"].as_str().unwrap(), format!("test-{i}"));
        assert_eq!(v["domain"].as_str().unwrap(), *domain_str);
        assert!(v["ts_ns"].is_u64(), "ts_ns must be integer");
        assert!(v["prior"].is_f64(), "prior must be float");
        assert!(v["posterior"].is_f64(), "posterior must be float");
        assert!(v["loss"].is_f64(), "loss must be float");
        assert!(v["confidence"].is_f64(), "confidence must be float");
        assert!(v["fallback"].is_boolean(), "fallback must be boolean");
        assert!(v["action"].is_string(), "action must be string");

        // Evidence terms array
        let evidence = v["evidence"].as_array().unwrap();
        assert_eq!(evidence.len(), 3, "Expected 3 evidence terms");
        for term in evidence {
            assert!(term["label"].is_string());
            assert!(term["llr"].is_f64());
        }
    }
}

#[test]
fn e2e_bayesian_evidence_quote_escaping() {
    let (file, sink) = tmp_sink();

    let evidence = BayesianEvidence {
        decision_id: r#"id-with-"quotes""#.to_string(),
        timestamp_ns: 42,
        domain: DecisionDomain::DiffStrategy,
        prior_log_odds: 0.0,
        evidence_terms: vec![EvidenceTerm::new(r#"term-"quoted""#, 1.0)],
        posterior_log_odds: 1.0,
        action: r#"action-"quoted""#.to_string(),
        expected_loss: 0.0,
        confidence_level: 0.99,
        fallback_triggered: false,
    };
    sink.write_jsonl(&evidence.to_jsonl()).unwrap();
    sink.flush().unwrap();

    let lines = read_lines(&file);
    let v = parse_json(&lines[0]);
    assert!(v["id"].as_str().unwrap().contains("quotes"));
    assert!(v["action"].as_str().unwrap().contains("quoted"));
}

// ---------------------------------------------------------------------------
// 2. StrategyEvidence (diff_strategy.rs)
// ---------------------------------------------------------------------------

#[test]
fn e2e_strategy_evidence_jsonl_schema() {
    let (file, sink) = tmp_sink();

    let evidence = StrategyEvidence {
        strategy: DiffStrategy::DirtyRows,
        cost_full: 1200.50,
        cost_dirty: 300.25,
        cost_redraw: 5000.0,
        posterior_mean: 0.75,
        posterior_variance: 0.001,
        alpha: 10.5,
        beta: 3.5,
        dirty_rows: 5,
        total_rows: 40,
        total_cells: 3200,
        guard_reason: "none",
        hysteresis_applied: false,
        hysteresis_ratio: 0.0,
    };
    sink.write_jsonl(&evidence.to_jsonl()).unwrap();
    sink.flush().unwrap();

    let lines = read_lines(&file);
    assert_eq!(lines.len(), 1);
    let v = parse_json(&lines[0]);

    assert_eq!(v["schema"].as_str().unwrap(), "diff-strategy-v1");
    assert_eq!(v["strategy"].as_str().unwrap(), "DirtyRows");
    assert!(v["cost_full"].is_f64());
    assert!(v["cost_dirty"].is_f64());
    assert!(v["cost_redraw"].is_f64());
    assert!(v["posterior_mean"].is_f64());
    assert!(v["posterior_var"].is_f64());
    assert!(v["alpha"].is_f64());
    assert!(v["beta"].is_f64());
    assert_eq!(v["dirty_rows"].as_u64().unwrap(), 5);
    assert_eq!(v["total_rows"].as_u64().unwrap(), 40);
    assert_eq!(v["total_cells"].as_u64().unwrap(), 3200);
    assert_eq!(v["guard"].as_str().unwrap(), "none");
    assert!(!v["hysteresis"].as_bool().unwrap());
    assert!(v["hysteresis_ratio"].is_f64());
}

// ---------------------------------------------------------------------------
// 3. BocpdEvidence (bocpd.rs)
// ---------------------------------------------------------------------------

#[test]
fn e2e_bocpd_evidence_jsonl_schema() {
    let (file, sink) = tmp_sink();

    let evidence = BocpdEvidence {
        p_burst: 0.85,
        log_bayes_factor: 2.3,
        observation_ms: 12.5,
        regime: BocpdRegime::Burst,
        likelihood_steady: -3.456,
        likelihood_burst: -1.234,
        expected_run_length: 5.5,
        run_length_variance: 2.1,
        run_length_mode: 4,
        run_length_p95: 12,
        run_length_tail_mass: 0.03,
        recommended_delay_ms: Some(50),
        hard_deadline_forced: Some(false),
        observation_count: 100,
        timestamp: Instant::now(),
    };
    sink.write_jsonl(&evidence.to_jsonl()).unwrap();
    sink.flush().unwrap();

    let lines = read_lines(&file);
    let v = parse_json(&lines[0]);

    assert_eq!(v["schema_version"].as_str().unwrap(), "bocpd-v1");
    assert_eq!(v["event"].as_str().unwrap(), "bocpd");
    assert!(v["p_burst"].is_f64());
    assert!(v["log_bf"].is_f64());
    assert!(v["obs_ms"].is_f64());
    assert_eq!(v["regime"].as_str().unwrap(), "burst");
    assert!(v["ll_steady"].is_f64());
    assert!(v["ll_burst"].is_f64());
    assert!(v["runlen_mean"].is_f64());
    assert!(v["runlen_var"].is_f64());
    assert_eq!(v["runlen_mode"].as_u64().unwrap(), 4);
    assert_eq!(v["runlen_p95"].as_u64().unwrap(), 12);
    assert!(v["runlen_tail"].is_f64());
    assert_eq!(v["delay_ms"].as_u64().unwrap(), 50);
    assert!(!v["forced_deadline"].as_bool().unwrap());
    assert_eq!(v["n_obs"].as_u64().unwrap(), 100);
}

#[test]
fn e2e_bocpd_evidence_null_optional_fields() {
    let (file, sink) = tmp_sink();

    let evidence = BocpdEvidence {
        p_burst: 0.1,
        log_bayes_factor: -1.0,
        observation_ms: 5.0,
        regime: BocpdRegime::Steady,
        likelihood_steady: -2.0,
        likelihood_burst: -4.0,
        expected_run_length: 10.0,
        run_length_variance: 3.0,
        run_length_mode: 8,
        run_length_p95: 20,
        run_length_tail_mass: 0.01,
        recommended_delay_ms: None,
        hard_deadline_forced: None,
        observation_count: 50,
        timestamp: Instant::now(),
    };
    sink.write_jsonl(&evidence.to_jsonl()).unwrap();
    sink.flush().unwrap();

    let lines = read_lines(&file);
    let v = parse_json(&lines[0]);

    assert!(v["delay_ms"].is_null(), "None should serialize as null");
    assert!(
        v["forced_deadline"].is_null(),
        "None should serialize as null"
    );
}

// ---------------------------------------------------------------------------
// 4. ConformalPrediction (conformal_predictor.rs)
// ---------------------------------------------------------------------------

#[test]
fn e2e_conformal_prediction_jsonl_schema() {
    let (file, sink) = tmp_sink();

    let prediction = ConformalPrediction {
        upper_us: 16666.7,
        risk: false,
        confidence: 0.95,
        bucket: BucketKey {
            mode: ModeBucket::AltScreen,
            diff: DiffBucket::Full,
            size_bucket: 2,
        },
        sample_count: 128,
        quantile: 0.95,
        fallback_level: 0,
        window_size: 64,
        reset_count: 0,
        y_hat: 8000.0,
        budget_us: 16666.0,
    };
    sink.write_jsonl(&prediction.to_jsonl()).unwrap();
    sink.flush().unwrap();

    let lines = read_lines(&file);
    let v = parse_json(&lines[0]);

    assert_eq!(v["schema"].as_str().unwrap(), "conformal-v1");
    assert!(v["upper_us"].is_f64());
    assert!(v["risk"].is_boolean());
    assert!(v["confidence"].is_f64());
    assert!(v["bucket"].is_string());
    assert_eq!(v["samples"].as_u64().unwrap(), 128);
    assert!(v["quantile"].is_f64());
    assert_eq!(v["fallback_level"].as_u64().unwrap(), 0);
    assert_eq!(v["window"].as_u64().unwrap(), 64);
    assert_eq!(v["resets"].as_u64().unwrap(), 0);
    assert!(v["y_hat"].is_f64());
    assert!(v["budget_us"].is_f64());
}

// ---------------------------------------------------------------------------
// 5. ThrottleDecision + ThrottleLog (eprocess_throttle.rs)
// ---------------------------------------------------------------------------

#[test]
fn e2e_throttle_decision_jsonl_schema() {
    let (file, sink) = tmp_sink();

    let decision = ThrottleDecision {
        should_recompute: true,
        wealth: 1.5,
        lambda: 0.1,
        empirical_rate: 0.05,
        forced_by_deadline: false,
        observations_since_recompute: 42,
    };
    sink.write_jsonl(&decision.to_jsonl()).unwrap();
    sink.flush().unwrap();

    let lines = read_lines(&file);
    let v = parse_json(&lines[0]);

    assert_eq!(v["schema"].as_str().unwrap(), "eprocess-throttle-v1");
    assert!(v["should_recompute"].as_bool().unwrap());
    assert!(v["wealth"].is_f64());
    assert!(v["lambda"].is_f64());
    assert!(v["empirical_rate"].is_f64());
    assert!(!v["forced_by_deadline"].as_bool().unwrap());
    assert_eq!(v["obs_since_recompute"].as_u64().unwrap(), 42);
}

#[test]
fn e2e_throttle_log_jsonl_schema() {
    let (file, sink) = tmp_sink();

    let log = ThrottleLog {
        timestamp: Instant::now(),
        observation_idx: 99,
        matched: true,
        wealth_before: 1.0,
        wealth_after: 1.1,
        lambda: 0.1,
        empirical_rate: 0.05,
        action: "recompute",
        time_since_recompute_ms: 150.5,
    };
    sink.write_jsonl(&log.to_jsonl()).unwrap();
    sink.flush().unwrap();

    let lines = read_lines(&file);
    let v = parse_json(&lines[0]);

    assert_eq!(v["schema"].as_str().unwrap(), "eprocess-log-v1");
    assert_eq!(v["obs_idx"].as_u64().unwrap(), 99);
    assert!(v["matched"].as_bool().unwrap());
    assert!(v["wealth_before"].is_f64());
    assert!(v["wealth_after"].is_f64());
    assert!(v["lambda"].is_f64());
    assert!(v["empirical_rate"].is_f64());
    assert_eq!(v["action"].as_str().unwrap(), "recompute");
    assert!(v["time_since_recompute_ms"].is_f64());
}

// ---------------------------------------------------------------------------
// 6. VoiDecision + VoiObservation (voi_sampling.rs)
// ---------------------------------------------------------------------------

#[test]
fn e2e_voi_decision_jsonl_schema() {
    let (file, sink) = tmp_sink();

    let decision = VoiDecision {
        event_idx: 42,
        should_sample: true,
        forced_by_interval: false,
        blocked_by_min_interval: false,
        voi_gain: 0.123,
        score: 0.456,
        cost: 0.01,
        log_bayes_factor: 2.5,
        posterior_mean: 0.7,
        posterior_variance: 0.02,
        e_value: 3.0,
        e_threshold: 2.0,
        boundary_score: 0.8,
        events_since_sample: 10,
        time_since_sample_ms: 250.5,
        reason: "voi_threshold",
    };
    sink.write_jsonl(&decision.to_jsonl()).unwrap();
    sink.flush().unwrap();

    let lines = read_lines(&file);
    let v = parse_json(&lines[0]);

    assert_eq!(v["event"].as_str().unwrap(), "voi_decision");
    assert_eq!(v["idx"].as_u64().unwrap(), 42);
    assert!(v["should_sample"].as_bool().unwrap());
    assert!(!v["forced"].as_bool().unwrap());
    assert!(!v["blocked"].as_bool().unwrap());
    assert!(v["voi_gain"].is_f64());
    assert!(v["score"].is_f64());
    assert!(v["cost"].is_f64());
    assert!(v["log_bayes_factor"].is_f64());
    assert!(v["posterior_mean"].is_f64());
    assert!(v["posterior_variance"].is_f64());
    assert!(v["e_value"].is_f64());
    assert!(v["e_threshold"].is_f64());
    assert!(v["boundary_score"].is_f64());
    assert_eq!(v["events_since_sample"].as_u64().unwrap(), 10);
    assert!(v["time_since_sample_ms"].is_f64());
    assert_eq!(v["reason"].as_str().unwrap(), "voi_threshold");
}

#[test]
fn e2e_voi_observation_jsonl_schema() {
    let (file, sink) = tmp_sink();

    let obs = VoiObservation {
        event_idx: 50,
        sample_idx: 5,
        violated: false,
        posterior_mean: 0.65,
        posterior_variance: 0.015,
        alpha: 10.5,
        beta: 3.5,
        e_value: 2.5,
        e_threshold: 2.0,
    };
    sink.write_jsonl(&obs.to_jsonl()).unwrap();
    sink.flush().unwrap();

    let lines = read_lines(&file);
    let v = parse_json(&lines[0]);

    assert_eq!(v["event"].as_str().unwrap(), "voi_observe");
    assert_eq!(v["idx"].as_u64().unwrap(), 50);
    assert_eq!(v["sample_idx"].as_u64().unwrap(), 5);
    assert!(!v["violated"].as_bool().unwrap());
    assert!(v["posterior_mean"].is_f64());
    assert!(v["posterior_variance"].is_f64());
    assert!(v["alpha"].is_f64());
    assert!(v["beta"].is_f64());
    assert!(v["e_value"].is_f64());
    assert!(v["e_threshold"].is_f64());
}

// ---------------------------------------------------------------------------
// 7. Mixed evidence stream (all 7 domains in one file)
// ---------------------------------------------------------------------------

#[test]
fn e2e_mixed_evidence_stream_all_domains() {
    let (file, sink) = tmp_sink();

    // 1. Diff strategy
    let strat = StrategyEvidence {
        strategy: DiffStrategy::Full,
        cost_full: 800.0,
        cost_dirty: 200.0,
        cost_redraw: 4000.0,
        posterior_mean: 0.6,
        posterior_variance: 0.005,
        alpha: 8.0,
        beta: 4.0,
        dirty_rows: 10,
        total_rows: 24,
        total_cells: 1920,
        guard_reason: "none",
        hysteresis_applied: true,
        hysteresis_ratio: 0.85,
    };
    sink.write_jsonl(&strat.to_jsonl()).unwrap();

    // 2. BOCPD
    let bocpd = BocpdEvidence {
        p_burst: 0.3,
        log_bayes_factor: -0.5,
        observation_ms: 8.0,
        regime: BocpdRegime::Transitional,
        likelihood_steady: -2.5,
        likelihood_burst: -3.0,
        expected_run_length: 15.0,
        run_length_variance: 5.0,
        run_length_mode: 12,
        run_length_p95: 30,
        run_length_tail_mass: 0.02,
        recommended_delay_ms: Some(25),
        hard_deadline_forced: None,
        observation_count: 200,
        timestamp: Instant::now(),
    };
    sink.write_jsonl(&bocpd.to_jsonl()).unwrap();

    // 3. Conformal
    let conformal = ConformalPrediction {
        upper_us: 12000.0,
        risk: true,
        confidence: 0.90,
        bucket: BucketKey {
            mode: ModeBucket::Inline,
            diff: DiffBucket::DirtyRows,
            size_bucket: 1,
        },
        sample_count: 64,
        quantile: 0.90,
        fallback_level: 1,
        window_size: 32,
        reset_count: 2,
        y_hat: 9000.0,
        budget_us: 11000.0,
    };
    sink.write_jsonl(&conformal.to_jsonl()).unwrap();

    // 4. E-process throttle
    let throttle = ThrottleDecision {
        should_recompute: false,
        wealth: 0.8,
        lambda: 0.15,
        empirical_rate: 0.08,
        forced_by_deadline: false,
        observations_since_recompute: 15,
    };
    sink.write_jsonl(&throttle.to_jsonl()).unwrap();

    // 5. Throttle log
    let tlog = ThrottleLog {
        timestamp: Instant::now(),
        observation_idx: 200,
        matched: false,
        wealth_before: 0.8,
        wealth_after: 0.75,
        lambda: 0.15,
        empirical_rate: 0.08,
        action: "hold",
        time_since_recompute_ms: 300.0,
    };
    sink.write_jsonl(&tlog.to_jsonl()).unwrap();

    // 6. VOI decision
    let voi = VoiDecision {
        event_idx: 100,
        should_sample: false,
        forced_by_interval: false,
        blocked_by_min_interval: true,
        voi_gain: 0.01,
        score: 0.1,
        cost: 0.05,
        log_bayes_factor: 0.3,
        posterior_mean: 0.5,
        posterior_variance: 0.1,
        e_value: 1.2,
        e_threshold: 2.0,
        boundary_score: 0.3,
        events_since_sample: 3,
        time_since_sample_ms: 50.0,
        reason: "min_interval",
    };
    sink.write_jsonl(&voi.to_jsonl()).unwrap();

    // 7. VOI observation
    let vobs = VoiObservation {
        event_idx: 101,
        sample_idx: 10,
        violated: true,
        posterior_mean: 0.45,
        posterior_variance: 0.12,
        alpha: 5.0,
        beta: 5.0,
        e_value: 2.5,
        e_threshold: 2.0,
    };
    sink.write_jsonl(&vobs.to_jsonl()).unwrap();

    // 8. Unified BayesianEvidence
    let unified = BayesianEvidence {
        decision_id: "e2e-mixed-1".into(),
        timestamp_ns: 999_999,
        domain: DecisionDomain::FrameBudget,
        prior_log_odds: -1.0,
        evidence_terms: vec![
            EvidenceTerm::new("frame_overrun", 2.0),
            EvidenceTerm::new("budget_margin", -0.5),
            EvidenceTerm::new("trend", 0.3),
        ],
        posterior_log_odds: 0.8,
        action: "degrade".to_string(),
        expected_loss: 0.05,
        confidence_level: 0.92,
        fallback_triggered: true,
    };
    sink.write_jsonl(&unified.to_jsonl()).unwrap();
    sink.flush().unwrap();

    let lines = read_lines(&file);
    assert_eq!(lines.len(), 8, "Expected 8 evidence lines total");

    // Verify each line is valid JSON
    for (i, line) in lines.iter().enumerate() {
        let v = parse_json(line);
        assert!(v.is_object(), "Line {i} must be a JSON object");
    }

    // Actually verify specific schema identifiers
    let v0 = parse_json(&lines[0]);
    assert_eq!(v0["schema"].as_str().unwrap(), "diff-strategy-v1");

    let v1 = parse_json(&lines[1]);
    assert_eq!(v1["schema_version"].as_str().unwrap(), "bocpd-v1");

    let v2 = parse_json(&lines[2]);
    assert_eq!(v2["schema"].as_str().unwrap(), "conformal-v1");

    let v3 = parse_json(&lines[3]);
    assert_eq!(v3["schema"].as_str().unwrap(), "eprocess-throttle-v1");

    let v4 = parse_json(&lines[4]);
    assert_eq!(v4["schema"].as_str().unwrap(), "eprocess-log-v1");

    let v5 = parse_json(&lines[5]);
    assert_eq!(v5["event"].as_str().unwrap(), "voi_decision");

    let v6 = parse_json(&lines[6]);
    assert_eq!(v6["event"].as_str().unwrap(), "voi_observe");

    let v7 = parse_json(&lines[7]);
    assert_eq!(v7["domain"].as_str().unwrap(), "frame_budget");
}

// ---------------------------------------------------------------------------
// 8. Float precision and special values
// ---------------------------------------------------------------------------

#[test]
fn e2e_evidence_float_precision_finite() {
    let (file, sink) = tmp_sink();

    // Test with extreme but finite float values
    let evidence = BayesianEvidence {
        decision_id: "precision-test".into(),
        timestamp_ns: 0,
        domain: DecisionDomain::Degradation,
        prior_log_odds: f64::MIN_POSITIVE,
        evidence_terms: vec![EvidenceTerm::new("tiny", f64::MIN_POSITIVE)],
        posterior_log_odds: 999999.999999,
        action: "none".into(),
        expected_loss: 0.0,
        confidence_level: 1.0,
        fallback_triggered: false,
    };
    sink.write_jsonl(&evidence.to_jsonl()).unwrap();
    sink.flush().unwrap();

    let lines = read_lines(&file);
    let v = parse_json(&lines[0]);
    assert!(v["prior"].is_f64());
    assert!(v["posterior"].is_f64());
    let terms = v["evidence"].as_array().unwrap();
    assert!(terms[0]["llr"].is_f64());
}

// ---------------------------------------------------------------------------
// 9. Deterministic ordering under sequential writes
// ---------------------------------------------------------------------------

#[test]
fn e2e_evidence_deterministic_ordering() {
    let (file, sink) = tmp_sink();

    for i in 0..100u64 {
        let evidence = BayesianEvidence {
            decision_id: format!("order-{i}"),
            timestamp_ns: i * 1000,
            domain: DecisionDomain::DiffStrategy,
            prior_log_odds: 0.0,
            evidence_terms: vec![],
            posterior_log_odds: 0.0,
            action: "test".into(),
            expected_loss: 0.0,
            confidence_level: 0.5,
            fallback_triggered: false,
        };
        sink.write_jsonl(&evidence.to_jsonl()).unwrap();
    }
    sink.flush().unwrap();

    let lines = read_lines(&file);
    assert_eq!(lines.len(), 100);

    for (i, line) in lines.iter().enumerate() {
        let v = parse_json(line);
        assert_eq!(
            v["id"].as_str().unwrap(),
            format!("order-{i}"),
            "Line {i} out of order"
        );
        assert_eq!(v["ts_ns"].as_u64().unwrap(), i as u64 * 1000);
    }
}

// ---------------------------------------------------------------------------
// 10. Empty evidence terms
// ---------------------------------------------------------------------------

#[test]
fn e2e_bayesian_evidence_empty_terms() {
    let (file, sink) = tmp_sink();

    let evidence = BayesianEvidence {
        decision_id: "empty-terms".into(),
        timestamp_ns: 1,
        domain: DecisionDomain::HintRanking,
        prior_log_odds: 0.0,
        evidence_terms: vec![],
        posterior_log_odds: 0.0,
        action: "pass".into(),
        expected_loss: 0.0,
        confidence_level: 0.5,
        fallback_triggered: false,
    };
    sink.write_jsonl(&evidence.to_jsonl()).unwrap();
    sink.flush().unwrap();

    let lines = read_lines(&file);
    let v = parse_json(&lines[0]);
    let terms = v["evidence"].as_array().unwrap();
    assert!(terms.is_empty(), "Empty evidence should serialize as []");
}

// ---------------------------------------------------------------------------
// 11. Sink disabled produces no output
// ---------------------------------------------------------------------------

#[test]
fn e2e_evidence_sink_disabled_noop() {
    let config = EvidenceSinkConfig::disabled();
    let sink = EvidenceSink::from_config(&config).unwrap();
    assert!(sink.is_none(), "Disabled config should return None");
}

// ---------------------------------------------------------------------------
// 12. Multiple flushes are idempotent
// ---------------------------------------------------------------------------

#[test]
fn e2e_evidence_sink_multiple_flushes() {
    let (file, sink) = tmp_sink();

    let evidence = BayesianEvidence {
        decision_id: "flush-test".into(),
        timestamp_ns: 1,
        domain: DecisionDomain::PaletteScoring,
        prior_log_odds: 0.0,
        evidence_terms: vec![EvidenceTerm::new("x", 1.0)],
        posterior_log_odds: 1.0,
        action: "select".into(),
        expected_loss: 0.0,
        confidence_level: 0.99,
        fallback_triggered: false,
    };
    sink.write_jsonl(&evidence.to_jsonl()).unwrap();

    // Multiple flushes should be safe and idempotent
    sink.flush().unwrap();
    sink.flush().unwrap();
    sink.flush().unwrap();

    let lines = read_lines(&file);
    assert_eq!(lines.len(), 1, "Only one line despite multiple flushes");
}

// ---------------------------------------------------------------------------
// 13. Concurrent writes (thread safety)
// ---------------------------------------------------------------------------

#[test]
fn e2e_evidence_sink_concurrent_writes() {
    let (file, sink) = tmp_sink();
    let num_threads = 4;
    let writes_per_thread = 50;

    std::thread::scope(|s| {
        for t in 0..num_threads {
            let sink = sink.clone();
            s.spawn(move || {
                for i in 0..writes_per_thread {
                    let evidence = BayesianEvidence {
                        decision_id: format!("t{t}-{i}"),
                        timestamp_ns: (t * writes_per_thread + i) as u64,
                        domain: DecisionDomain::DiffStrategy,
                        prior_log_odds: 0.0,
                        evidence_terms: vec![],
                        posterior_log_odds: 0.0,
                        action: "test".into(),
                        expected_loss: 0.0,
                        confidence_level: 0.5,
                        fallback_triggered: false,
                    };
                    sink.write_jsonl(&evidence.to_jsonl()).unwrap();
                }
            });
        }
    });
    sink.flush().unwrap();

    let lines = read_lines(&file);
    assert_eq!(
        lines.len(),
        num_threads * writes_per_thread,
        "All concurrent writes must appear"
    );

    // Every line must be valid JSON (no interleaving/corruption)
    for (i, line) in lines.iter().enumerate() {
        let v = parse_json(line);
        assert!(v.is_object(), "Line {i} must be valid JSON object");
        assert!(v["id"].is_string(), "Line {i} must have id field");
    }
}
