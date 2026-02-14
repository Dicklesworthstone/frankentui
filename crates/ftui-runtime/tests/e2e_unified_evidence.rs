#![forbid(unsafe_code)]

//! E2E tests for the Unified Evidence Ledger (bd-fp38v).
//!
//! Validates that:
//! 1. All 7 decision domains can emit through the unified schema.
//! 2. JSONL output is valid and contains required fields.
//! 3. Ring buffer semantics work under realistic load.
//! 4. Domain-level queries return correct results.
//! 5. Builder correctly selects top-3 evidence terms.
//! 6. Evidence replay (JSONL round-trip) preserves all fields.

use ftui_runtime::unified_evidence::{
    DecisionDomain, EvidenceEntry, EvidenceEntryBuilder, UnifiedEvidenceLedger,
};

// ============================================================================
// Helpers
// ============================================================================

/// Simulated decision from the diff strategy controller.
fn diff_strategy_decision(_frame: u64, ts: u64) -> EvidenceEntry {
    EvidenceEntryBuilder::new(DecisionDomain::DiffStrategy, 0, ts)
        .log_posterior(1.386) // ~80% → dirty_rows
        .evidence("change_fraction", 4.0)
        .evidence("dirty_rows_ratio", 2.5)
        .evidence("frame_time_headroom", 1.2)
        .action("dirty_rows")
        .loss_avoided(0.35)
        .confidence_interval(0.72, 0.88)
        .build()
}

/// Simulated decision from the resize coalescer.
fn resize_coalescing_decision(ts: u64) -> EvidenceEntry {
    EvidenceEntryBuilder::new(DecisionDomain::ResizeCoalescing, 0, ts)
        .log_posterior(-0.405) // ~40% → coalesce (wait)
        .evidence("event_rate", 0.3)
        .evidence("bocpd_regime", 1.5)
        .action("coalesce")
        .loss_avoided(0.10)
        .confidence_interval(0.30, 0.55)
        .build()
}

/// Simulated decision from the frame budget PID controller.
fn frame_budget_decision(ts: u64) -> EvidenceEntry {
    EvidenceEntryBuilder::new(DecisionDomain::FrameBudget, 0, ts)
        .log_posterior(0.0) // ~50% → hold current level
        .evidence("pid_output", 1.1)
        .evidence("integral_term", 0.8)
        .evidence("derivative_term", 0.9)
        .action("hold")
        .loss_avoided(0.0)
        .confidence_interval(0.40, 0.60)
        .build()
}

/// Simulated degradation decision.
fn degradation_decision(ts: u64) -> EvidenceEntry {
    EvidenceEntryBuilder::new(DecisionDomain::Degradation, 0, ts)
        .log_posterior(2.197) // ~90% → degrade
        .evidence("frame_miss_rate", 8.0)
        .evidence("latency_spike", 3.0)
        .action("degrade_1")
        .loss_avoided(0.50)
        .confidence_interval(0.85, 0.95)
        .build()
}

/// Simulated VOI sampling decision.
fn voi_sampling_decision(ts: u64) -> EvidenceEntry {
    EvidenceEntryBuilder::new(DecisionDomain::VoiSampling, 0, ts)
        .log_posterior(0.693) // ~67% → sample
        .evidence("voi_score", 2.0)
        .evidence("boundary_score", 1.5)
        .action("sample")
        .loss_avoided(0.20)
        .confidence_interval(0.55, 0.78)
        .build()
}

/// Simulated hint ranking decision.
fn hint_ranking_decision(ts: u64) -> EvidenceEntry {
    EvidenceEntryBuilder::new(DecisionDomain::HintRanking, 0, ts)
        .log_posterior(1.099) // ~75% → rank_2
        .evidence("expected_utility", 3.0)
        .evidence("voi_exploration", 1.8)
        .evidence("space_cost", 0.5) // negative evidence (BF < 1)
        .action("rank_2")
        .loss_avoided(0.12)
        .confidence_interval(0.65, 0.82)
        .build()
}

/// Simulated command palette scoring decision.
fn palette_scoring_decision(ts: u64) -> EvidenceEntry {
    EvidenceEntryBuilder::new(DecisionDomain::PaletteScoring, 0, ts)
        .log_posterior(4.595) // ~99% → exact match
        .evidence("match_type", 99.0)
        .evidence("word_boundary", 2.0)
        .evidence("position_bonus", 1.5)
        .evidence("gap_penalty", 0.3) // negative
        .evidence("tag_match", 3.0)
        .action("exact")
        .loss_avoided(0.95)
        .confidence_interval(0.97, 0.99)
        .build()
}

// ============================================================================
// Test 1: All 7 domains emit through unified schema
// ============================================================================

#[test]
fn all_seven_domains_emit() {
    let mut ledger = UnifiedEvidenceLedger::new(100);

    let ts_base = 1_000_000u64;
    ledger.record(diff_strategy_decision(0, ts_base));
    ledger.record(resize_coalescing_decision(ts_base + 16_000));
    ledger.record(frame_budget_decision(ts_base + 32_000));
    ledger.record(degradation_decision(ts_base + 48_000));
    ledger.record(voi_sampling_decision(ts_base + 64_000));
    ledger.record(hint_ranking_decision(ts_base + 80_000));
    ledger.record(palette_scoring_decision(ts_base + 96_000));

    assert_eq!(ledger.len(), 7);
    assert_eq!(ledger.total_recorded(), 7);

    // Each domain has exactly 1 entry.
    for domain in DecisionDomain::ALL {
        assert_eq!(
            ledger.domain_count(domain),
            1,
            "{:?} should have 1 entry",
            domain
        );
    }
}

// ============================================================================
// Test 2: JSONL output contains required fields
// ============================================================================

#[test]
fn jsonl_contains_required_fields() {
    let mut ledger = UnifiedEvidenceLedger::new(100);
    ledger.record(diff_strategy_decision(0, 1_000_000));
    ledger.record(palette_scoring_decision(2_000_000));

    let jsonl = ledger.export_jsonl();
    let lines: Vec<&str> = jsonl.lines().collect();
    assert_eq!(lines.len(), 2);

    for line in &lines {
        // Required fields per bead spec.
        assert!(
            line.contains("\"schema\":\"ftui-evidence-v2\""),
            "missing schema"
        );
        assert!(line.contains("\"id\":"), "missing id");
        assert!(line.contains("\"ts_ns\":"), "missing timestamp");
        assert!(line.contains("\"domain\":"), "missing domain");
        assert!(line.contains("\"log_posterior\":"), "missing log_posterior");
        assert!(line.contains("\"evidence\":"), "missing evidence");
        assert!(line.contains("\"action\":"), "missing action");
        assert!(line.contains("\"loss_avoided\":"), "missing loss_avoided");
        assert!(line.contains("\"ci\":"), "missing confidence_interval");
    }

    // Diff strategy line should have domain-specific values.
    assert!(lines[0].contains("\"domain\":\"diff_strategy\""));
    assert!(lines[0].contains("\"action\":\"dirty_rows\""));

    // Palette line should reference exact match.
    assert!(lines[1].contains("\"domain\":\"palette_scoring\""));
    assert!(lines[1].contains("\"action\":\"exact\""));
}

// ============================================================================
// Test 3: Evidence terms are top-3 by |log(BF)|
// ============================================================================

#[test]
fn top_3_evidence_selection() {
    // Palette scoring has 5 evidence terms; only top-3 should survive.
    let entry = palette_scoring_decision(1_000_000);
    assert_eq!(entry.evidence_count(), 3);

    let labels: Vec<&str> = entry
        .top_evidence
        .iter()
        .flatten()
        .map(|t| t.label)
        .collect();

    // match_type has BF=99 → |log(99)| = 4.595 (largest)
    assert_eq!(labels[0], "match_type");
    // gap_penalty BF=0.3 → |log(0.3)| = 1.204 (second, negative evidence)
    assert_eq!(labels[1], "gap_penalty");
    // tag_match has BF=3 → |log(3)| = 1.099 (third)
    assert_eq!(labels[2], "tag_match");
}

// ============================================================================
// Test 4: Ring buffer under realistic load
// ============================================================================

#[test]
fn realistic_load_simulation() {
    // Simulate 60fps for 10 seconds = 600 frames.
    // Each frame makes 1-3 decisions across domains.
    let mut ledger = UnifiedEvidenceLedger::new(2000);
    let mut total = 0u64;

    for frame in 0..600u64 {
        let ts = frame * 16_667; // ~16.667ms per frame

        // Every frame: diff strategy decision.
        ledger.record(diff_strategy_decision(frame, ts));
        total += 1;

        // Every 10 frames: frame budget check.
        if frame % 10 == 0 {
            ledger.record(frame_budget_decision(ts + 1000));
            total += 1;
        }

        // Occasional resize (frames 100-110).
        if (100..110).contains(&frame) {
            ledger.record(resize_coalescing_decision(ts + 2000));
            total += 1;
        }

        // Palette scoring on "user interaction" frames.
        if frame % 50 == 0 {
            ledger.record(palette_scoring_decision(ts + 5000));
            total += 1;
        }
    }

    assert_eq!(ledger.total_recorded(), total);
    // With 2000 capacity, all entries should be retained (total < 2000).
    assert!(total < 2000, "test assumes total < capacity");
    assert_eq!(ledger.len(), total as usize);

    // Verify domain distribution.
    assert_eq!(ledger.domain_count(DecisionDomain::DiffStrategy), 600);
    assert_eq!(ledger.domain_count(DecisionDomain::FrameBudget), 60);
    assert_eq!(ledger.domain_count(DecisionDomain::ResizeCoalescing), 10);
    assert_eq!(ledger.domain_count(DecisionDomain::PaletteScoring), 12);

    // Summary should reflect all active domains.
    let summary = ledger.summary();
    assert!(summary.domains.len() >= 4);
}

// ============================================================================
// Test 5: Ring buffer wrap preserves ordering
// ============================================================================

#[test]
fn ring_buffer_wrap_preserves_order() {
    let mut ledger = UnifiedEvidenceLedger::new(50);
    for i in 0..200u64 {
        let mut e = diff_strategy_decision(i, i * 16_667);
        e.timestamp_ns = i * 16_667;
        ledger.record(e);
    }

    assert_eq!(ledger.len(), 50);
    assert_eq!(ledger.total_recorded(), 200);

    // Entries should be in ascending decision_id order.
    let ids: Vec<u64> = ledger.entries().map(|e| e.decision_id).collect();
    assert_eq!(ids.len(), 50);
    for window in ids.windows(2) {
        assert!(
            window[0] < window[1],
            "IDs not ascending: {} >= {}",
            window[0],
            window[1]
        );
    }

    // Oldest should be id 150 (200 - 50).
    assert_eq!(ids[0], 150);
    assert_eq!(ids[49], 199);
}

// ============================================================================
// Test 6: JSONL round-trip (parse and verify fields)
// ============================================================================

#[test]
fn jsonl_round_trip_fields() {
    let entry = EvidenceEntryBuilder::new(DecisionDomain::HintRanking, 42, 999_999)
        .log_posterior(1.099)
        .evidence("expected_utility", 3.0)
        .evidence("voi_bonus", 1.8)
        .action("rank_1")
        .loss_avoided(0.25)
        .confidence_interval(0.65, 0.82)
        .build();

    let jsonl = entry.to_jsonl();

    // Parse with serde_json to verify structure.
    let parsed: serde_json::Value = serde_json::from_str(&jsonl).expect("valid JSON");

    assert_eq!(parsed["schema"], "ftui-evidence-v2");
    assert_eq!(parsed["domain"], "hint_ranking");
    assert_eq!(parsed["action"], "rank_1");
    assert!(parsed["log_posterior"].as_f64().unwrap() > 1.0);
    assert!(parsed["loss_avoided"].as_f64().unwrap() > 0.0);

    let ci = parsed["ci"].as_array().unwrap();
    assert_eq!(ci.len(), 2);
    let lower = ci[0].as_f64().unwrap();
    let upper = ci[1].as_f64().unwrap();
    assert!(lower < upper);

    let evidence = parsed["evidence"].as_array().unwrap();
    assert_eq!(evidence.len(), 2); // only 2 terms provided
    assert_eq!(evidence[0]["label"], "expected_utility");
    assert!(evidence[0]["bf"].as_f64().unwrap() > 0.0);
}

// ============================================================================
// Test 7: Domain filtering
// ============================================================================

#[test]
fn domain_filtering() {
    let mut ledger = UnifiedEvidenceLedger::new(100);

    // Interleave decisions from different domains.
    for i in 0..30u64 {
        let ts = i * 1000;
        match i % 3 {
            0 => {
                ledger.record(diff_strategy_decision(i, ts));
            }
            1 => {
                ledger.record(hint_ranking_decision(ts));
            }
            _ => {
                ledger.record(voi_sampling_decision(ts));
            }
        }
    }

    assert_eq!(ledger.len(), 30);

    // Filter by domain.
    let diff_count = ledger
        .entries_for_domain(DecisionDomain::DiffStrategy)
        .count();
    let hint_count = ledger
        .entries_for_domain(DecisionDomain::HintRanking)
        .count();
    let voi_count = ledger
        .entries_for_domain(DecisionDomain::VoiSampling)
        .count();

    assert_eq!(diff_count, 10);
    assert_eq!(hint_count, 10);
    assert_eq!(voi_count, 10);

    // No entries for unused domains.
    assert_eq!(
        ledger
            .entries_for_domain(DecisionDomain::FrameBudget)
            .count(),
        0
    );
}

// ============================================================================
// Test 8: Posterior probability correctness
// ============================================================================

#[test]
fn posterior_probability_values() {
    let test_cases: Vec<(f64, f64)> = vec![
        (0.0, 0.5),     // log-odds 0 → 50%
        (1.386, 0.8),   // log-odds ~ln(4) → 80%
        (-1.386, 0.2),  // log-odds ~-ln(4) → 20%
        (2.197, 0.9),   // log-odds ~ln(9) → 90%
        (4.595, 0.99),  // log-odds ~ln(99) → 99%
        (-4.595, 0.01), // log-odds ~-ln(99) → 1%
    ];

    for (log_posterior, expected_prob) in test_cases {
        let entry = EvidenceEntry {
            decision_id: 0,
            timestamp_ns: 0,
            domain: DecisionDomain::DiffStrategy,
            log_posterior,
            top_evidence: [None, None, None],
            action: "test",
            loss_avoided: 0.0,
            confidence_interval: (0.0, 1.0),
        };

        let prob = entry.posterior_probability();
        assert!(
            (prob - expected_prob).abs() < 0.02,
            "log_posterior={} → prob={}, expected ~{}",
            log_posterior,
            prob,
            expected_prob
        );
    }
}

// ============================================================================
// Test 9: Summary statistics
// ============================================================================

#[test]
fn summary_statistics() {
    let mut ledger = UnifiedEvidenceLedger::new(1000);

    // 100 diff strategy decisions with varying loss.
    for i in 0..100u64 {
        let mut e = diff_strategy_decision(i, i * 16_667);
        e.loss_avoided = (i as f64) * 0.01; // 0.0 to 0.99
        ledger.record(e);
    }

    // 50 hint ranking decisions.
    for i in 0..50u64 {
        ledger.record(hint_ranking_decision(i * 16_667));
    }

    let summary = ledger.summary();
    assert_eq!(summary.total_decisions, 150);
    assert_eq!(summary.stored_decisions, 150);
    assert_eq!(summary.domains.len(), 2);

    let diff = summary
        .domains
        .iter()
        .find(|d| d.domain == DecisionDomain::DiffStrategy)
        .unwrap();
    assert_eq!(diff.decision_count, 100);
    // Mean loss avoided: (0 + 0.01 + ... + 0.99) / 100 = 0.495
    assert!(
        (diff.mean_loss_avoided - 0.495).abs() < 0.01,
        "mean loss avoided: {}",
        diff.mean_loss_avoided
    );
    assert!(diff.mean_posterior > 0.0);
}

// ============================================================================
// Test 10: Full audit trail with evidence bridges (bd-xox.12)
// ============================================================================

/// E2E test using actual evidence bridges to convert domain-specific types
/// into unified entries, verifying the complete pipeline.
#[test]
fn full_audit_trail_via_bridges() {
    let mut ledger = UnifiedEvidenceLedger::new(1000);
    let mut ts = 1_000_000u64;

    // 1. Diff strategy decision (via bridge).
    let diff_evidence = ftui_render::diff_strategy::StrategyEvidence {
        strategy: ftui_render::diff_strategy::DiffStrategy::DirtyRows,
        cost_full: 1.0,
        cost_dirty: 0.5,
        cost_redraw: 2.0,
        posterior_mean: 0.05,
        posterior_variance: 0.001,
        alpha: 2.0,
        beta: 38.0,
        dirty_rows: 3,
        total_rows: 24,
        total_cells: 1920,
        guard_reason: "none",
        hysteresis_applied: false,
        hysteresis_ratio: 0.05,
    };
    ledger.record(ftui_runtime::evidence_bridges::from_diff_strategy(
        &diff_evidence,
        ts,
    ));
    ts += 16_667;

    // 2. E-process throttle decision (via bridge).
    let throttle_decision = ftui_runtime::eprocess_throttle::ThrottleDecision {
        should_recompute: true,
        wealth: 25.0,
        lambda: 0.3,
        empirical_rate: 0.4,
        forced_by_deadline: false,
        observations_since_recompute: 50,
    };
    ledger.record(ftui_runtime::evidence_bridges::from_eprocess(
        &throttle_decision,
        ts,
    ));
    ts += 16_667;

    // 3. VOI sampling decision (via bridge).
    let voi_decision = ftui_runtime::voi_sampling::VoiDecision {
        event_idx: 100,
        should_sample: true,
        forced_by_interval: false,
        blocked_by_min_interval: false,
        voi_gain: 0.05,
        score: 0.8,
        cost: 0.3,
        log_bayes_factor: 1.5,
        posterior_mean: 0.1,
        posterior_variance: 0.005,
        e_value: 5.0,
        e_threshold: 20.0,
        boundary_score: 0.7,
        events_since_sample: 30,
        time_since_sample_ms: 500.0,
        reason: "voi_ge_cost",
    };
    ledger.record(ftui_runtime::evidence_bridges::from_voi(&voi_decision, ts));
    ts += 16_667;

    // 4. Conformal prediction decision (via bridge).
    let conformal_prediction = ftui_runtime::conformal_predictor::ConformalPrediction {
        upper_us: 18_000.0,
        risk: true,
        confidence: 0.95,
        bucket: ftui_runtime::conformal_predictor::BucketKey {
            mode: ftui_runtime::conformal_predictor::ModeBucket::AltScreen,
            diff: ftui_runtime::conformal_predictor::DiffBucket::Full,
            size_bucket: 2,
        },
        sample_count: 50,
        quantile: 15_000.0,
        fallback_level: 0,
        window_size: 100,
        reset_count: 0,
        y_hat: 12_000.0,
        budget_us: 16_666.0,
    };
    ledger.record(ftui_runtime::evidence_bridges::from_conformal(
        &conformal_prediction,
        ts,
    ));
    ts += 16_667;

    // 5. BOCPD regime change (via bridge).
    let bocpd_evidence = ftui_runtime::bocpd::BocpdEvidence {
        p_burst: 0.85,
        log_bayes_factor: 2.3,
        observation_ms: 5.0,
        regime: ftui_runtime::bocpd::BocpdRegime::Burst,
        likelihood_steady: 0.01,
        likelihood_burst: 0.5,
        expected_run_length: 3.0,
        run_length_variance: 2.0,
        run_length_mode: 2,
        run_length_p95: 8,
        run_length_tail_mass: 0.02,
        recommended_delay_ms: Some(50),
        hard_deadline_forced: None,
        observation_count: 100,
        timestamp: std::time::Instant::now(),
    };
    ledger.record(ftui_runtime::evidence_bridges::from_bocpd(
        &bocpd_evidence,
        ts,
    ));

    // Verify: exactly 5 records, one per decision type.
    assert_eq!(ledger.len(), 5);
    assert_eq!(ledger.total_recorded(), 5);

    // All 5 bridge domains present.
    assert_eq!(ledger.domain_count(DecisionDomain::DiffStrategy), 1);
    assert_eq!(ledger.domain_count(DecisionDomain::FrameBudget), 1);
    assert_eq!(ledger.domain_count(DecisionDomain::VoiSampling), 1);
    assert_eq!(ledger.domain_count(DecisionDomain::Degradation), 1);
    assert_eq!(ledger.domain_count(DecisionDomain::ResizeCoalescing), 1);

    // Verify chronological ordering via decision_id.
    let ids: Vec<u64> = ledger.entries().map(|e| e.decision_id).collect();
    assert_eq!(ids, vec![0, 1, 2, 3, 4]);

    // All JSONL lines are valid JSON matching the schema.
    let jsonl = ledger.export_jsonl();
    let lines: Vec<&str> = jsonl.lines().collect();
    assert_eq!(lines.len(), 5);

    let expected_domains = [
        "diff_strategy",
        "frame_budget",
        "voi_sampling",
        "degradation",
        "resize_coalescing",
    ];
    let mut found_domains = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let parsed: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|_| panic!("Line {} is not valid JSON", i));

        // Schema version.
        assert_eq!(parsed["schema"], "ftui-evidence-v2");

        // All required fields present.
        assert!(parsed["id"].as_u64().is_some(), "missing id on line {i}");
        assert!(
            parsed["ts_ns"].as_u64().is_some(),
            "missing ts_ns on line {i}"
        );
        assert!(
            parsed["domain"].as_str().is_some(),
            "missing domain on line {i}"
        );
        assert!(
            parsed["action"].as_str().is_some(),
            "missing action on line {i}"
        );
        assert!(
            parsed["evidence"].as_array().is_some(),
            "missing evidence on line {i}"
        );

        found_domains.push(parsed["domain"].as_str().unwrap().to_string());
    }

    // All expected domain types present in the audit trail.
    for domain in &expected_domains {
        assert!(
            found_domains.contains(&domain.to_string()),
            "audit trail missing domain: {domain}"
        );
    }
}

// ============================================================================
// Test 11: All JSONL lines are valid JSON
// ============================================================================

#[test]
fn all_jsonl_lines_valid_json() {
    let mut ledger = UnifiedEvidenceLedger::new(1000);

    // Record entries from all domains with varied evidence counts.
    ledger.record(diff_strategy_decision(0, 0));
    ledger.record(resize_coalescing_decision(1000));
    ledger.record(frame_budget_decision(2000));
    ledger.record(degradation_decision(3000));
    ledger.record(voi_sampling_decision(4000));
    ledger.record(hint_ranking_decision(5000));
    ledger.record(palette_scoring_decision(6000));

    // Also test entries with 0, 1, 2, 3 evidence terms.
    let zero_evidence = EvidenceEntry {
        decision_id: 0,
        timestamp_ns: 7000,
        domain: DecisionDomain::FrameBudget,
        log_posterior: 0.0,
        top_evidence: [None, None, None],
        action: "hold",
        loss_avoided: 0.0,
        confidence_interval: (0.45, 0.55),
    };
    ledger.record(zero_evidence);

    let one_evidence = EvidenceEntryBuilder::new(DecisionDomain::Degradation, 0, 8000)
        .evidence("frame_miss", 5.0)
        .action("degrade_2")
        .build();
    ledger.record(one_evidence);

    let jsonl = ledger.export_jsonl();
    for (i, line) in jsonl.lines().enumerate() {
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(line);
        assert!(
            parsed.is_ok(),
            "Line {} is not valid JSON: {}",
            i,
            &line[..line.len().min(100)]
        );
    }
}
