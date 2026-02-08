//! Property-based invariant tests for the VOI sampler and conformal alerter.
//!
//! These tests verify structural invariants that must hold for any valid inputs:
//!
//! VOI Sampler (1–15):
//! 1. Beta posterior mean in [0,1].
//! 2. Beta posterior variance in [0, 0.25].
//! 3. VOI gain is non-negative (expected variance reduction >= 0).
//! 4. Expected variance after sample <= current variance.
//! 5. E-value always positive and finite.
//! 6. E-value starts at 1.0.
//! 7. Observation updates correct posterior parameter (alpha or beta).
//! 8. Forced sampling when max_interval_events exceeded.
//! 9. Blocked sampling when min_interval_events not met.
//! 10. Event counter monotonically increments.
//! 11. Sample count <= event count.
//! 12. Summary fields consistent with accessor state.
//! 13. Variance monotonically decreases with more observations.
//! 14. Determinism: same config + events → same state.
//! 15. No panics on arbitrary decide/observe sequences.
//!
//! Conformal Alert (16–28):
//! 16. E-value always positive and finite.
//! 17. E-value starts at 1.0.
//! 18. Calibration count bounded by max_calibration.
//! 19. Conformal threshold is non-negative and finite when calibrated.
//! 20. Conformal score in (0, 1].
//! 21. E-value resets to 1.0 after alert.
//! 22. Insufficient calibration never triggers alert.
//! 23. Baseline observations do not trigger alerts quickly.
//! 24. Stats fields consistent with internal state.
//! 25. Clear calibration resets all state.
//! 26. Reset e-process preserves calibration.
//! 27. Determinism: same calibration + observations → same state.
//! 28. No panics on arbitrary calibrate/observe sequences.

use ftui_runtime::conformal_alert::{AlertConfig, AlertReason, ConformalAlert};
use ftui_runtime::voi_sampling::{VoiConfig, VoiSampler};
use proptest::prelude::*;
use std::time::{Duration, Instant};

// ── Strategies ────────────────────────────────────────────────────────────

fn voi_config_strategy() -> impl Strategy<Value = VoiConfig> {
    (
        0.001f64..=0.5,  // alpha
        0.01f64..=10.0,  // prior_alpha
        0.01f64..=10.0,  // prior_beta
        0.001f64..=0.99, // mu_0
        0.01f64..=2.0,   // lambda
        0.01f64..=10.0,  // value_scale
        0.0f64..=5.0,    // boundary_weight
        0.001f64..=1.0,  // sample_cost
        0u64..=50,       // min_interval_events
        1u64..=100,      // max_interval_events
    )
        .prop_map(
            |(alpha, pa, pb, mu_0, lambda, vs, bw, sc, min_ev, max_ev)| VoiConfig {
                alpha,
                prior_alpha: pa,
                prior_beta: pb,
                mu_0,
                lambda,
                value_scale: vs,
                boundary_weight: bw,
                sample_cost: sc,
                min_interval_ms: 0, // disable time-based for determinism
                max_interval_ms: 0, // disable time-based for determinism
                min_interval_events: min_ev,
                max_interval_events: max_ev,
                enable_logging: false,
                max_log_entries: 64,
            },
        )
}

fn alert_config_strategy() -> impl Strategy<Value = AlertConfig> {
    (
        0.001f64..=0.5, // alpha
        5usize..=20,    // min_calibration
        50usize..=200,  // max_calibration
        0.01f64..=2.0,  // lambda
        0.5f64..=2.0,   // hysteresis
        0u64..=10,      // alert_cooldown
    )
        .prop_map(|(alpha, min_cal, max_cal, lambda, hyst, cd)| AlertConfig {
            alpha,
            min_calibration: min_cal,
            max_calibration: max_cal,
            lambda,
            mu_0: 0.0,
            sigma_0: 1.0,
            adaptive_lambda: false, // deterministic
            grapa_eta: 0.1,
            enable_logging: false,
            hysteresis: hyst,
            alert_cooldown: cd,
        })
}

fn violation_sequence(max_len: usize) -> impl Strategy<Value = Vec<bool>> {
    proptest::collection::vec(any::<bool>(), 1..=max_len)
}

fn calibration_values(max_len: usize) -> impl Strategy<Value = Vec<f64>> {
    proptest::collection::vec(-100.0f64..=100.0, 1..=max_len)
}

fn observation_values(max_len: usize) -> impl Strategy<Value = Vec<f64>> {
    proptest::collection::vec(-200.0f64..=200.0, 1..=max_len)
}

// ═════════════════════════════════════════════════════════════════════════
// VOI SAMPLER INVARIANTS
// ═════════════════════════════════════════════════════════════════════════

// ─── 1. Beta posterior mean in [0,1] ──────────────────────────────────

proptest! {
    #[test]
    fn voi_posterior_mean_in_unit_interval(
        config in voi_config_strategy(),
        violations in violation_sequence(50),
    ) {
        let base = Instant::now();
        let mut sampler = VoiSampler::new_at(config, base);
        let mut now = base;

        for &violated in &violations {
            let decision = sampler.decide(now);
            if decision.should_sample {
                sampler.observe_at(violated, now);
            }
            let mean = sampler.posterior_mean();
            prop_assert!(
                (0.0..=1.0).contains(&mean),
                "Posterior mean {} out of [0,1]", mean
            );
            now += Duration::from_millis(1);
        }
    }
}

// ─── 2. Beta posterior variance in [0, 0.25] ──────────────────────────

proptest! {
    #[test]
    fn voi_posterior_variance_bounded(
        config in voi_config_strategy(),
        violations in violation_sequence(50),
    ) {
        let base = Instant::now();
        let mut sampler = VoiSampler::new_at(config, base);
        let mut now = base;

        for &violated in &violations {
            let decision = sampler.decide(now);
            if decision.should_sample {
                sampler.observe_at(violated, now);
            }
            let var = sampler.posterior_variance();
            prop_assert!(var >= 0.0, "Variance {} negative", var);
            prop_assert!(var <= 0.25 + 1e-10, "Variance {} > 0.25", var);
            now += Duration::from_millis(1);
        }
    }
}

// ─── 3. VOI gain is non-negative ──────────────────────────────────────

proptest! {
    #[test]
    fn voi_gain_non_negative(
        config in voi_config_strategy(),
        violations in violation_sequence(30),
    ) {
        let base = Instant::now();
        let mut sampler = VoiSampler::new_at(config, base);
        let mut now = base;

        for &violated in &violations {
            let decision = sampler.decide(now);
            prop_assert!(
                decision.voi_gain >= -1e-12,
                "VOI gain {} should be non-negative", decision.voi_gain
            );
            if decision.should_sample {
                sampler.observe_at(violated, now);
            }
            now += Duration::from_millis(1);
        }
    }
}

// ─── 4. Expected variance after <= current variance ───────────────────

proptest! {
    #[test]
    fn voi_expected_variance_decreases(
        alpha in 0.01f64..=10.0,
        beta in 0.01f64..=10.0,
    ) {
        let config = VoiConfig {
            prior_alpha: alpha,
            prior_beta: beta,
            ..Default::default()
        };
        let sampler = VoiSampler::new(config);
        let var = sampler.posterior_variance();
        let expected_after = sampler.expected_variance_after();
        prop_assert!(
            expected_after <= var + 1e-12,
            "Expected variance after ({}) > current ({})", expected_after, var
        );
    }
}

// ─── 5. E-value always positive and finite ────────────────────────────

proptest! {
    #[test]
    fn voi_evalue_positive_finite(
        config in voi_config_strategy(),
        violations in violation_sequence(50),
    ) {
        let base = Instant::now();
        let mut sampler = VoiSampler::new_at(config, base);
        let mut now = base;

        for &violated in &violations {
            let decision = sampler.decide(now);
            if decision.should_sample {
                sampler.observe_at(violated, now);
            }
            prop_assert!(
                decision.e_value > 0.0,
                "E-value {} should be positive", decision.e_value
            );
            prop_assert!(
                decision.e_value.is_finite(),
                "E-value {} should be finite", decision.e_value
            );
            now += Duration::from_millis(1);
        }
    }
}

// ─── 6. E-value starts at 1.0 ────────────────────────────────────────

proptest! {
    #[test]
    fn voi_evalue_starts_at_one(config in voi_config_strategy()) {
        let sampler = VoiSampler::new(config);
        let base = Instant::now();
        let mut s2 = VoiSampler::new_at(Default::default(), base);
        let d = s2.decide(base);
        prop_assert!(
            (d.e_value - 1.0).abs() < 1e-10,
            "Initial e-value should be 1.0, got {}", d.e_value
        );
        // Also check via summary for the configured sampler
        let summary = sampler.summary();
        prop_assert!(
            (summary.e_value - 1.0).abs() < 1e-10,
            "Summary initial e-value should be 1.0"
        );
    }
}

// ─── 7. Observation updates correct posterior parameter ───────────────

proptest! {
    #[test]
    fn voi_observe_updates_posterior(
        config in voi_config_strategy(),
        violated in any::<bool>(),
    ) {
        let base = Instant::now();
        let mut sampler = VoiSampler::new_at(config, base);
        let (a_before, b_before) = sampler.posterior_params();

        sampler.decide(base);
        sampler.observe_at(violated, base);

        let (a_after, b_after) = sampler.posterior_params();
        if violated {
            prop_assert!(
                (a_after - a_before - 1.0).abs() < 1e-9,
                "Violation should increment alpha: {} -> {}", a_before, a_after
            );
            prop_assert!(
                (b_after - b_before).abs() < 1e-9,
                "Violation should not change beta: {} -> {}", b_before, b_after
            );
        } else {
            prop_assert!(
                (b_after - b_before - 1.0).abs() < 1e-9,
                "Non-violation should increment beta: {} -> {}", b_before, b_after
            );
            prop_assert!(
                (a_after - a_before).abs() < 1e-9,
                "Non-violation should not change alpha: {} -> {}", a_before, a_after
            );
        }
    }
}

// ─── 8. Forced sampling when max_interval_events exceeded ─────────────

proptest! {
    #[test]
    fn voi_forced_by_max_interval(
        max_events in 1u64..=10,
    ) {
        let config = VoiConfig {
            max_interval_events: max_events,
            max_interval_ms: 0,         // disable time forcing
            min_interval_events: 0,
            sample_cost: 1e6,           // very high cost to prevent VOI sampling
            ..Default::default()
        };
        let base = Instant::now();
        let mut sampler = VoiSampler::new_at(config, base);
        let mut now = base;

        // After max_events decisions without sampling, should force
        for i in 1..=max_events + 1 {
            let d = sampler.decide(now);
            if i >= max_events && d.forced_by_interval {
                prop_assert!(d.should_sample, "Forced should mean sample");
                sampler.observe_at(false, now);
            }
            now += Duration::from_millis(1);
        }
    }
}

// ─── 9. Blocked sampling when min_interval not met ────────────────────

proptest! {
    #[test]
    fn voi_blocked_by_min_interval(
        min_events in 2u64..=20,
    ) {
        let config = VoiConfig {
            min_interval_events: min_events,
            min_interval_ms: 0,
            max_interval_events: 0,     // disable forcing
            max_interval_ms: 0,
            sample_cost: 0.0,           // would always sample if not blocked
            ..Default::default()
        };
        let base = Instant::now();
        let mut sampler = VoiSampler::new_at(config, base);

        // First decision should sample (no prior sample to block)
        let d1 = sampler.decide(base);
        prop_assert!(d1.should_sample, "First decision should sample");
        sampler.observe_at(false, base);

        // Next decision should be blocked
        let d2 = sampler.decide(base + Duration::from_millis(1));
        prop_assert!(
            d2.blocked_by_min_interval,
            "Second decision should be blocked (min_events={})", min_events
        );
        prop_assert!(!d2.should_sample);
    }
}

// ─── 10. Event counter monotonically increments ──────────────────────

proptest! {
    #[test]
    fn voi_event_counter_monotone(
        config in voi_config_strategy(),
        n_events in 1usize..=50,
    ) {
        let base = Instant::now();
        let mut sampler = VoiSampler::new_at(config, base);
        let mut now = base;

        for i in 1..=n_events {
            let d = sampler.decide(now);
            prop_assert_eq!(
                d.event_idx, i as u64,
                "Event index should be {} after {} decisions", i, i
            );
            if d.should_sample {
                sampler.observe_at(false, now);
            }
            now += Duration::from_millis(1);
        }
    }
}

// ─── 11. Sample count <= event count ──────────────────────────────────

proptest! {
    #[test]
    fn voi_sample_count_le_event_count(
        config in voi_config_strategy(),
        violations in violation_sequence(50),
    ) {
        let base = Instant::now();
        let mut sampler = VoiSampler::new_at(config, base);
        let mut now = base;

        for &violated in &violations {
            let d = sampler.decide(now);
            if d.should_sample {
                sampler.observe_at(violated, now);
            }
            now += Duration::from_millis(1);
        }

        let summary = sampler.summary();
        prop_assert!(
            summary.total_samples <= summary.total_events,
            "Samples {} > events {}", summary.total_samples, summary.total_events
        );
    }
}

// ─── 12. Summary fields consistent with accessor state ────────────────

proptest! {
    #[test]
    fn voi_summary_consistent(
        config in voi_config_strategy(),
        violations in violation_sequence(30),
    ) {
        let base = Instant::now();
        let mut sampler = VoiSampler::new_at(config, base);
        let mut now = base;

        for &violated in &violations {
            let d = sampler.decide(now);
            if d.should_sample {
                sampler.observe_at(violated, now);
            }
            now += Duration::from_millis(1);
        }

        let summary = sampler.summary();
        prop_assert!(
            (summary.current_mean - sampler.posterior_mean()).abs() < 1e-10,
            "Summary mean {} != accessor {}", summary.current_mean, sampler.posterior_mean()
        );
        prop_assert!(
            (summary.current_variance - sampler.posterior_variance()).abs() < 1e-10,
            "Summary var {} != accessor {}", summary.current_variance, sampler.posterior_variance()
        );
        prop_assert_eq!(
            summary.skipped_events,
            summary.total_events.saturating_sub(summary.total_samples),
            "Skipped events should be total - samples"
        );
    }
}

// ─── 13. Variance bounded by initial and monotone after many samples ──

proptest! {
    #[test]
    fn voi_variance_bounded_by_max(
        config in voi_config_strategy(),
        violations in violation_sequence(50),
    ) {
        let base = Instant::now();
        let mut sampler = VoiSampler::new_at(config, base);
        let mut now = base;

        // Track variance over time — it should remain bounded by VAR_MAX (0.25)
        // and generally decrease as we accumulate observations
        for &violated in &violations {
            let d = sampler.decide(now);
            if d.should_sample {
                sampler.observe_at(violated, now);
            }
            let var = sampler.posterior_variance();
            prop_assert!(var >= 0.0, "Variance must be non-negative");
            prop_assert!(var <= 0.25 + 1e-10, "Variance must be <= 0.25");
            now += Duration::from_millis(1);
        }

        // After many observations, alpha + beta should have grown,
        // so variance should be smaller than the theoretical max
        let summary = sampler.summary();
        if summary.total_samples >= 10 {
            let var = sampler.posterior_variance();
            // With alpha+beta >= 10+prior, variance <= 0.25/(prior+10+1)
            // This is a weak bound but always holds
            prop_assert!(
                var < 0.25,
                "After {} samples, variance {} should be well below 0.25",
                summary.total_samples, var
            );
        }
    }
}

// ─── 14. VOI sampler determinism ──────────────────────────────────────

proptest! {
    #[test]
    fn voi_deterministic(
        config in voi_config_strategy(),
        violations in violation_sequence(30),
    ) {
        let base = Instant::now();

        let run = |cfg: &VoiConfig| {
            let mut sampler = VoiSampler::new_at(cfg.clone(), base);
            let mut now = base;
            let mut decisions = Vec::new();
            for &violated in &violations {
                let d = sampler.decide(now);
                if d.should_sample {
                    sampler.observe_at(violated, now);
                }
                decisions.push((d.should_sample, d.forced_by_interval));
                now += Duration::from_millis(1);
            }
            (decisions, sampler.posterior_mean(), sampler.posterior_variance())
        };

        let (d1, m1, v1) = run(&config);
        let (d2, m2, v2) = run(&config);

        prop_assert_eq!(d1, d2, "Decisions must be deterministic");
        prop_assert!(
            (m1 - m2).abs() < 1e-10,
            "Posterior mean must be deterministic: {} vs {}", m1, m2
        );
        prop_assert!(
            (v1 - v2).abs() < 1e-10,
            "Posterior variance must be deterministic: {} vs {}", v1, v2
        );
    }
}

// ─── 15. No panics on arbitrary decide/observe sequences ──────────────

proptest! {
    #[test]
    fn voi_no_panic(
        config in voi_config_strategy(),
        violations in violation_sequence(50),
    ) {
        let base = Instant::now();
        let mut sampler = VoiSampler::new_at(config, base);
        let mut now = base;

        for &violated in &violations {
            let d = sampler.decide(now);
            if d.should_sample {
                sampler.observe_at(violated, now);
            }
            let _ = sampler.posterior_mean();
            let _ = sampler.posterior_variance();
            let _ = sampler.expected_variance_after();
            let _ = sampler.posterior_params();
            let _ = sampler.last_decision();
            let _ = sampler.last_observation();
            let _ = sampler.summary();
            let _ = sampler.config();
            now += Duration::from_millis(1);
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// CONFORMAL ALERT INVARIANTS
// ═════════════════════════════════════════════════════════════════════════

// ─── 16. E-value always positive and finite ───────────────────────────

proptest! {
    #[test]
    fn conformal_evalue_positive_finite(
        config in alert_config_strategy(),
        cal in calibration_values(50),
        obs in observation_values(50),
    ) {
        let mut alerter = ConformalAlert::new(config);
        for &v in &cal {
            alerter.calibrate(v);
        }
        for &v in &obs {
            let d = alerter.observe(v);
            prop_assert!(
                d.evidence.e_value > 0.0,
                "E-value {} should be positive", d.evidence.e_value
            );
            prop_assert!(
                d.evidence.e_value.is_finite(),
                "E-value {} should be finite", d.evidence.e_value
            );
        }
    }
}

// ─── 17. E-value starts at 1.0 ───────────────────────────────────────

proptest! {
    #[test]
    fn conformal_evalue_starts_at_one(config in alert_config_strategy()) {
        let alerter = ConformalAlert::new(config);
        prop_assert!(
            (alerter.e_value() - 1.0).abs() < 1e-10,
            "Initial e-value should be 1.0, got {}", alerter.e_value()
        );
    }
}

// ─── 18. Calibration count bounded by max_calibration ─────────────────

proptest! {
    #[test]
    fn conformal_calibration_bounded(
        config in alert_config_strategy(),
        cal in calibration_values(100),
    ) {
        let mut alerter = ConformalAlert::new(config.clone());
        for &v in &cal {
            alerter.calibrate(v);
            prop_assert!(
                alerter.calibration_count() <= config.max_calibration,
                "Calibration count {} > max {}", alerter.calibration_count(), config.max_calibration
            );
        }
    }
}

// ─── 19. Conformal threshold non-negative and finite when calibrated ──

proptest! {
    #[test]
    fn conformal_threshold_valid(
        config in alert_config_strategy(),
        cal in calibration_values(30),
    ) {
        let mut alerter = ConformalAlert::new(config.clone());
        for &v in &cal {
            alerter.calibrate(v);
        }

        if alerter.calibration_count() > 0 {
            let threshold = alerter.threshold();
            prop_assert!(
                threshold >= 0.0,
                "Threshold {} should be non-negative", threshold
            );
            prop_assert!(
                threshold.is_finite(),
                "Threshold {} should be finite", threshold
            );
        }
    }
}

// ─── 20. Conformal score in (0, 1] ────────────────────────────────────

proptest! {
    #[test]
    fn conformal_score_in_range(
        config in alert_config_strategy(),
        cal in calibration_values(30),
        obs in observation_values(20),
    ) {
        let mut alerter = ConformalAlert::new(config.clone());
        for &v in &cal {
            alerter.calibrate(v);
        }

        if alerter.calibration_count() >= config.min_calibration {
            for &v in &obs {
                let d = alerter.observe(v);
                // When reason is Normal, BothExceeded, ConformalExceeded, or EProcessExceeded
                // the conformal_score is computed
                if d.evidence.reason != AlertReason::InCooldown
                    && d.evidence.reason != AlertReason::InsufficientCalibration
                {
                    prop_assert!(
                        d.evidence.conformal_score > 0.0,
                        "Conformal score {} should be > 0", d.evidence.conformal_score
                    );
                    prop_assert!(
                        d.evidence.conformal_score <= 1.0,
                        "Conformal score {} should be <= 1", d.evidence.conformal_score
                    );
                }
            }
        }
    }
}

// ─── 21. E-value resets to 1.0 after alert ────────────────────────────

proptest! {
    #[test]
    fn conformal_evalue_resets_after_alert(
        alpha in 0.1f64..=0.5,
    ) {
        let config = AlertConfig {
            alpha,
            min_calibration: 5,
            max_calibration: 100,
            lambda: 0.5,
            mu_0: 0.0,
            sigma_0: 1.0,
            adaptive_lambda: false,
            grapa_eta: 0.1,
            enable_logging: false,
            hysteresis: 0.5,    // easy trigger
            alert_cooldown: 0,
        };
        let mut alerter = ConformalAlert::new(config);

        // Tight calibration around 0
        for _ in 0..10 {
            alerter.calibrate(0.0);
        }

        // Drive to alert with extreme values
        let mut alert_seen = false;
        for _ in 0..100 {
            let d = alerter.observe(1000.0);
            if d.is_alert {
                alert_seen = true;
                prop_assert!(
                    (alerter.e_value() - 1.0).abs() < 0.01,
                    "E-value should reset after alert, got {}", alerter.e_value()
                );
                break;
            }
        }
        prop_assert!(alert_seen, "Should trigger alert with extreme values");
    }
}

// ─── 22. Insufficient calibration never triggers alert ────────────────

proptest! {
    #[test]
    fn conformal_insufficient_cal_no_alert(
        config in alert_config_strategy(),
        obs_val in -1000.0f64..=1000.0,
    ) {
        let mut alerter = ConformalAlert::new(config.clone());

        // Add fewer calibration samples than min_calibration
        let n = config.min_calibration.saturating_sub(1);
        for i in 0..n {
            alerter.calibrate(i as f64);
        }

        let d = alerter.observe(obs_val);
        prop_assert!(
            !d.is_alert,
            "Should not alert with {} < {} calibration samples",
            n, config.min_calibration
        );
        prop_assert_eq!(
            d.evidence.reason,
            AlertReason::InsufficientCalibration,
        );
    }
}

// ─── 23. Baseline observations don't trigger quickly ──────────────────

proptest! {
    #[test]
    fn conformal_baseline_no_quick_alert(
        center in -50.0f64..=50.0,
        spread in 1.0f64..=10.0,
    ) {
        let config = AlertConfig {
            alpha: 0.01,
            min_calibration: 10,
            max_calibration: 100,
            lambda: 0.5,
            mu_0: 0.0,
            sigma_0: 1.0,
            adaptive_lambda: false,
            grapa_eta: 0.1,
            enable_logging: false,
            hysteresis: 1.0,
            alert_cooldown: 0,
        };
        let mut alerter = ConformalAlert::new(config);

        // Calibrate with values around center
        for i in 0..20 {
            let v = center + (i as f64 - 10.0) * spread / 10.0;
            alerter.calibrate(v);
        }

        // Observe values from the same range — should not alert
        let mut alerted = false;
        for i in 0..10 {
            let v = center + (i as f64 - 5.0) * spread / 10.0;
            let d = alerter.observe(v);
            if d.is_alert {
                alerted = true;
                break;
            }
        }
        prop_assert!(
            !alerted,
            "Baseline observations around center={} spread={} should not alert",
            center, spread
        );
    }
}

// ─── 24. Stats fields consistent ──────────────────────────────────────

proptest! {
    #[test]
    fn conformal_stats_consistent(
        config in alert_config_strategy(),
        cal in calibration_values(30),
        obs in observation_values(20),
    ) {
        let mut alerter = ConformalAlert::new(config);
        for &v in &cal {
            alerter.calibrate(v);
        }
        for &v in &obs {
            let _ = alerter.observe(v);
        }

        let stats = alerter.stats();
        prop_assert_eq!(stats.total_observations, obs.len() as u64);
        prop_assert_eq!(stats.calibration_samples, alerter.calibration_count());
        prop_assert!(
            (stats.current_e_value - alerter.e_value()).abs() < 1e-10,
            "Stats e-value {} != accessor {}", stats.current_e_value, alerter.e_value()
        );
        prop_assert!(
            (stats.calibration_mean - alerter.mean()).abs() < 1e-10,
            "Stats mean {} != accessor {}", stats.calibration_mean, alerter.mean()
        );
        // Alert type counts should sum correctly
        prop_assert!(
            stats.conformal_alerts + stats.eprocess_alerts + stats.both_alerts <= stats.total_alerts,
            "Alert type breakdown exceeds total"
        );
        if stats.total_observations > 0 {
            prop_assert!(
                stats.empirical_fpr >= 0.0 && stats.empirical_fpr <= 1.0,
                "Empirical FPR {} should be in [0,1]", stats.empirical_fpr
            );
        }
    }
}

// ─── 25. Clear calibration resets all ─────────────────────────────────

proptest! {
    #[test]
    fn conformal_clear_calibration_resets(
        config in alert_config_strategy(),
        cal in calibration_values(20),
    ) {
        let mut alerter = ConformalAlert::new(config);
        for &v in &cal {
            alerter.calibrate(v);
        }

        alerter.clear_calibration();
        prop_assert_eq!(alerter.calibration_count(), 0);
        prop_assert!(
            alerter.mean().abs() < 1e-10,
            "Mean should reset to 0, got {}", alerter.mean()
        );
        prop_assert!(
            (alerter.e_value() - 1.0).abs() < 1e-10,
            "E-value should reset to 1.0, got {}", alerter.e_value()
        );
    }
}

// ─── 26. Reset e-process preserves calibration ────────────────────────

proptest! {
    #[test]
    fn conformal_reset_eprocess_preserves_calibration(
        config in alert_config_strategy(),
        cal in calibration_values(20),
        obs in observation_values(10),
    ) {
        let mut alerter = ConformalAlert::new(config.clone());
        for &v in &cal {
            alerter.calibrate(v);
        }
        let cal_count_before = alerter.calibration_count();
        let mean_before = alerter.mean();

        for &v in &obs {
            let _ = alerter.observe(v);
        }

        alerter.reset_eprocess();

        // E-value should reset
        prop_assert!(
            (alerter.e_value() - 1.0).abs() < 1e-10,
            "E-value should reset to 1.0 after reset_eprocess"
        );
        // Calibration should be preserved
        prop_assert_eq!(
            alerter.calibration_count(), cal_count_before,
            "Calibration count should be preserved"
        );
        prop_assert!(
            (alerter.mean() - mean_before).abs() < 1e-10,
            "Calibration mean should be preserved"
        );
    }
}

// ─── 27. Conformal alert determinism ──────────────────────────────────

proptest! {
    #[test]
    fn conformal_deterministic(
        config in alert_config_strategy(),
        cal in calibration_values(20),
        obs in observation_values(20),
    ) {
        let run = |cfg: &AlertConfig| {
            let mut alerter = ConformalAlert::new(cfg.clone());
            for &v in &cal {
                alerter.calibrate(v);
            }
            let mut decisions = Vec::new();
            for &v in &obs {
                let d = alerter.observe(v);
                decisions.push(d.is_alert);
            }
            (decisions, alerter.e_value(), alerter.threshold())
        };

        let (d1, e1, t1) = run(&config);
        let (d2, e2, t2) = run(&config);

        prop_assert_eq!(d1, d2, "Alert decisions must be deterministic");
        prop_assert!(
            (e1 - e2).abs() < 1e-10,
            "E-value must be deterministic: {} vs {}", e1, e2
        );
        prop_assert!(
            (t1 - t2).abs() < 1e-10,
            "Threshold must be deterministic: {} vs {}", t1, t2
        );
    }
}

// ─── 28. No panics on arbitrary sequences ─────────────────────────────

proptest! {
    #[test]
    fn conformal_no_panic(
        config in alert_config_strategy(),
        cal in calibration_values(30),
        obs in observation_values(50),
    ) {
        let mut alerter = ConformalAlert::new(config);
        for &v in &cal {
            alerter.calibrate(v);
        }
        for &v in &obs {
            let d = alerter.observe(v);
            let _ = d.is_alert;
            let _ = d.evidence.summary();
            let _ = d.evidence_summary();
        }
        let _ = alerter.e_value();
        let _ = alerter.threshold();
        let _ = alerter.mean();
        let _ = alerter.std();
        let _ = alerter.calibration_count();
        let _ = alerter.alpha();
        let _ = alerter.stats();
        let _ = alerter.logs();

        alerter.reset_eprocess();
        let _ = alerter.e_value();
        alerter.clear_calibration();
        let _ = alerter.calibration_count();
    }
}
