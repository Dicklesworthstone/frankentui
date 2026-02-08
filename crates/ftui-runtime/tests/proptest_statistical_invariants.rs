//! Property-based invariant tests for statistical detector modules.
//!
//! These tests verify mathematical invariants of the flake detector (e-process)
//! and e-process throttle that must hold for any valid inputs:
//!
//! **Flake Detector:**
//! 1. E-value is always positive (E_MIN floor prevents zero-lock).
//! 2. E-value is always finite (E_MAX ceiling prevents overflow).
//! 3. E-value starts at 1.0 (identity element).
//! 4. Variance estimate is non-negative.
//! 5. Observation count increments monotonically.
//! 6. Zero residuals never trigger flakiness (e_increment < 1 under H₀).
//! 7. Reset restores initial state.
//! 8. Batch observe processes all elements or stops early.
//! 9. Determinism: same residuals → same e-value.
//! 10. Config builder clamping preserves valid ranges.
//! 11. No panics on arbitrary finite residuals.
//!
//! **E-Process Throttle:**
//! 12. Wealth is always positive (W_MIN floor prevents zero-lock).
//! 13. Wealth starts at 1.0.
//! 14. Lambda stays bounded in (0, lambda_max].
//! 15. Empirical match rate is in [0, 1].
//! 16. No-match decreases wealth (with fixed lambda).
//! 17. Match increases wealth (with fixed lambda, when mu_0 < 1).
//! 18. Observation count increments correctly.
//! 19. Stats counters are consistent.
//! 20. Determinism: same observations → same wealth.
//! 21. No panics on arbitrary match sequences.

use ftui_runtime::eprocess_throttle::{EProcessThrottle, ThrottleConfig};
use ftui_runtime::flake_detector::{FlakeConfig, FlakeDetector};
use proptest::prelude::*;
use std::time::{Duration, Instant};

// ── Strategies ────────────────────────────────────────────────────────────

fn flake_config_strategy() -> impl Strategy<Value = FlakeConfig> {
    (
        0.001f64..=0.5,  // alpha
        0.01f64..=2.0,   // lambda
        0.001f64..=10.0, // sigma
        0usize..=100,    // variance_window
        1usize..=20,     // min_observations
    )
        .prop_map(|(alpha, lambda, sigma, vw, min_obs)| {
            FlakeConfig::new(alpha)
                .with_lambda(lambda)
                .with_sigma(sigma)
                .with_variance_window(vw)
                .with_min_observations(min_obs)
        })
}

fn residuals_strategy(max_len: usize) -> impl Strategy<Value = Vec<f64>> {
    proptest::collection::vec(-100.0f64..=100.0, 1..=max_len)
}

fn throttle_config_strategy() -> impl Strategy<Value = ThrottleConfig> {
    (
        0.001f64..=0.5,  // alpha
        0.001f64..=0.99, // mu_0
        0.01f64..=2.0,   // initial_lambda
        0.001f64..=1.0,  // grapa_eta
        1u64..=100,      // min_observations_between
        4usize..=128,    // rate_window_size
    )
        .prop_map(|(alpha, mu_0, lambda, eta, min_obs, win)| ThrottleConfig {
            alpha,
            mu_0,
            initial_lambda: lambda,
            grapa_eta: eta,
            hard_deadline_ms: u64::MAX, // disable deadline for deterministic tests
            min_observations_between: min_obs,
            rate_window_size: win,
            enable_logging: false,
        })
}

fn match_sequence_strategy(max_len: usize) -> impl Strategy<Value = Vec<bool>> {
    proptest::collection::vec(proptest::bool::ANY, 1..=max_len)
}

// ═════════════════════════════════════════════════════════════════════════
// Flake Detector Tests
// ═════════════════════════════════════════════════════════════════════════

// 1. E-value is always positive

proptest! {
    #[test]
    fn flake_evalue_always_positive(
        config in flake_config_strategy(),
        residuals in residuals_strategy(100),
    ) {
        let mut detector = FlakeDetector::new(config);
        for &r in &residuals {
            let decision = detector.observe(r);
            prop_assert!(
                decision.e_value > 0.0,
                "E-value must be positive, got {} at obs {}",
                decision.e_value, decision.observation_count
            );
        }
    }
}

// 2. E-value is always finite

proptest! {
    #[test]
    fn flake_evalue_always_finite(
        config in flake_config_strategy(),
        residuals in residuals_strategy(100),
    ) {
        let mut detector = FlakeDetector::new(config);
        for &r in &residuals {
            let decision = detector.observe(r);
            prop_assert!(
                decision.e_value.is_finite(),
                "E-value must be finite, got {} at obs {}",
                decision.e_value, decision.observation_count
            );
        }
    }
}

// 3. E-value starts at 1.0

proptest! {
    #[test]
    fn flake_initial_evalue_is_one(config in flake_config_strategy()) {
        let detector = FlakeDetector::new(config);
        prop_assert!(
            (detector.e_value() - 1.0).abs() < 1e-10,
            "Initial e-value should be 1.0, got {}",
            detector.e_value()
        );
    }
}

// 4. Variance estimate is non-negative

proptest! {
    #[test]
    fn flake_variance_non_negative(
        config in flake_config_strategy(),
        residuals in residuals_strategy(50),
    ) {
        let mut detector = FlakeDetector::new(config);
        for &r in &residuals {
            let decision = detector.observe(r);
            prop_assert!(
                decision.variance_estimate >= 0.0,
                "Variance must be non-negative, got {}",
                decision.variance_estimate
            );
        }
    }
}

// 5. Observation count increments monotonically

proptest! {
    #[test]
    fn flake_obs_count_monotone(
        config in flake_config_strategy(),
        residuals in residuals_strategy(50),
    ) {
        let mut detector = FlakeDetector::new(config);
        let mut prev_count = 0;
        for &r in &residuals {
            let decision = detector.observe(r);
            prop_assert!(
                decision.observation_count == prev_count + 1,
                "Count should increment: prev={}, cur={}",
                prev_count, decision.observation_count
            );
            prev_count = decision.observation_count;
        }
    }
}

// 6. Zero residuals never trigger flakiness

proptest! {
    #[test]
    fn flake_zero_residuals_no_trigger(config in flake_config_strategy()) {
        let mut detector = FlakeDetector::new(config);
        // Zero residuals: e_increment = exp(-λ²σ²/2) < 1, so E_t decreases
        for _ in 0..200 {
            let decision = detector.observe(0.0);
            prop_assert!(
                !decision.should_fail(),
                "Zero residuals should never trigger: e_value={}, threshold={}",
                decision.e_value, decision.threshold
            );
        }
    }
}

// 7. Reset restores initial state

proptest! {
    #[test]
    fn flake_reset_restores_state(
        config in flake_config_strategy(),
        residuals in residuals_strategy(20),
    ) {
        let mut detector = FlakeDetector::new(config);
        for &r in &residuals {
            detector.observe(r);
        }
        detector.reset();
        prop_assert_eq!(
            detector.observation_count(), 0,
            "Reset should clear observation count"
        );
        prop_assert!(
            (detector.e_value() - 1.0).abs() < 1e-10,
            "Reset should restore e-value to 1.0, got {}",
            detector.e_value()
        );
    }
}

// 8. Batch observe processes all or stops early

proptest! {
    #[test]
    fn flake_batch_processes_correctly(
        config in flake_config_strategy(),
        residuals in residuals_strategy(50),
    ) {
        let mut detector = FlakeDetector::new(config);
        let decision = detector.observe_batch(&residuals);
        // Either processed all, or stopped early on flaky detection
        prop_assert!(
            decision.observation_count <= residuals.len(),
            "Should not exceed total residuals: {} > {}",
            decision.observation_count, residuals.len()
        );
        if decision.should_fail() {
            // Stopped early — count should be <= total
            prop_assert!(decision.observation_count <= residuals.len());
        } else {
            // Processed all
            prop_assert_eq!(decision.observation_count, residuals.len());
        }
    }
}

// 9. Determinism

proptest! {
    #[test]
    fn flake_deterministic(
        config in flake_config_strategy(),
        residuals in residuals_strategy(50),
    ) {
        let mut d1 = FlakeDetector::new(config.clone());
        let mut d2 = FlakeDetector::new(config);
        for &r in &residuals {
            d1.observe(r);
            d2.observe(r);
        }
        prop_assert!(
            (d1.e_value() - d2.e_value()).abs() < 1e-10,
            "Same inputs should give same e-value: {} vs {}",
            d1.e_value(), d2.e_value()
        );
    }
}

// 10. Config builder clamping

proptest! {
    #[test]
    fn flake_config_clamping(
        alpha in -1.0f64..=2.0,
        lambda in -1.0f64..=100.0,
        sigma in -1.0f64..=100.0,
        min_obs in 0usize..=100,
    ) {
        let config = FlakeConfig::new(alpha)
            .with_lambda(lambda)
            .with_sigma(sigma)
            .with_min_observations(min_obs);
        // All values should be in valid ranges after clamping
        prop_assert!((1e-10..=0.5).contains(&config.alpha));
        prop_assert!((0.01..=2.0).contains(&config.lambda));
        prop_assert!(config.sigma >= 1e-9);
        prop_assert!(config.min_observations >= 1);
        prop_assert!(config.threshold() > 0.0);
        prop_assert!(config.threshold().is_finite());
    }
}

// 11. No panics on arbitrary finite residuals

proptest! {
    #[test]
    fn flake_no_panic(
        config in flake_config_strategy(),
        residuals in residuals_strategy(100),
    ) {
        let mut detector = FlakeDetector::new(config);
        for &r in &residuals {
            let _ = detector.observe(r);
        }
        let _ = detector.e_value();
        let _ = detector.observation_count();
        let _ = detector.is_warmed_up();
        let _ = detector.current_sigma();
        let _ = detector.config();
        detector.reset();
    }
}

// ═════════════════════════════════════════════════════════════════════════
// E-Process Throttle Tests
// ═════════════════════════════════════════════════════════════════════════

// 12. Wealth is always positive

proptest! {
    #[test]
    fn throttle_wealth_always_positive(
        config in throttle_config_strategy(),
        matches in match_sequence_strategy(200),
    ) {
        let base = Instant::now();
        let mut throttle = EProcessThrottle::new_at(config, base);
        for (i, &m) in matches.iter().enumerate() {
            let d = throttle.observe_at(m, base + Duration::from_millis(i as u64 + 1));
            prop_assert!(
                d.wealth > 0.0,
                "Wealth must be positive, got {} at obs {}",
                d.wealth, i + 1
            );
        }
    }
}

// 13. Wealth starts at 1.0

proptest! {
    #[test]
    fn throttle_initial_wealth_is_one(config in throttle_config_strategy()) {
        let throttle = EProcessThrottle::new_at(config, Instant::now());
        prop_assert!(
            (throttle.wealth() - 1.0).abs() < 1e-10,
            "Initial wealth should be 1.0, got {}",
            throttle.wealth()
        );
    }
}

// 14. Lambda stays bounded

proptest! {
    #[test]
    fn throttle_lambda_bounded(
        config in throttle_config_strategy(),
        matches in match_sequence_strategy(100),
    ) {
        let base = Instant::now();
        let mut throttle = EProcessThrottle::new_at(config, base);
        for (i, &m) in matches.iter().enumerate() {
            let d = throttle.observe_at(m, base + Duration::from_millis(i as u64 + 1));
            prop_assert!(
                d.lambda > 0.0,
                "Lambda must be positive, got {} at obs {}",
                d.lambda, i + 1
            );
            prop_assert!(
                d.lambda.is_finite(),
                "Lambda must be finite, got {} at obs {}",
                d.lambda, i + 1
            );
        }
    }
}

// 15. Empirical match rate is in [0, 1]

proptest! {
    #[test]
    fn throttle_empirical_rate_bounded(
        config in throttle_config_strategy(),
        matches in match_sequence_strategy(100),
    ) {
        let base = Instant::now();
        let mut throttle = EProcessThrottle::new_at(config, base);
        for (i, &m) in matches.iter().enumerate() {
            let d = throttle.observe_at(m, base + Duration::from_millis(i as u64 + 1));
            prop_assert!(
                (0.0..=1.0).contains(&d.empirical_rate),
                "Empirical rate must be in [0, 1], got {} at obs {}",
                d.empirical_rate, i + 1
            );
        }
    }
}

// 16. No-match decreases wealth (fixed lambda, no GRAPA, from initial state)

proptest! {
    #[test]
    fn throttle_no_match_decreases_wealth(
        alpha in 0.001f64..=0.5,
        mu_0 in 0.01f64..=0.99,
    ) {
        let base = Instant::now();
        let config = ThrottleConfig {
            alpha,
            mu_0,
            initial_lambda: 0.5_f64.min(1.0 / mu_0 - 0.01),
            grapa_eta: 0.0, // disable adaptation for clean test
            hard_deadline_ms: u64::MAX,
            min_observations_between: u64::MAX,
            rate_window_size: 64,
            enable_logging: false,
        };
        let mut throttle = EProcessThrottle::new_at(config, base);
        let d = throttle.observe_at(false, base + Duration::from_millis(1));
        // W_1 = 1 * (1 + λ * (0 - μ₀)) = 1 - λ*μ₀ < 1
        prop_assert!(
            d.wealth < 1.0,
            "No-match should decrease wealth from 1.0: got {}",
            d.wealth
        );
    }
}

// 17. Match increases wealth (fixed lambda, mu_0 < 1, from initial state)

proptest! {
    #[test]
    fn throttle_match_increases_wealth(
        alpha in 0.001f64..=0.5,
        mu_0 in 0.01f64..=0.99,
    ) {
        let base = Instant::now();
        let config = ThrottleConfig {
            alpha,
            mu_0,
            initial_lambda: 0.5_f64.min(1.0 / mu_0 - 0.01),
            grapa_eta: 0.0,
            hard_deadline_ms: u64::MAX,
            min_observations_between: u64::MAX,
            rate_window_size: 64,
            enable_logging: false,
        };
        let mut throttle = EProcessThrottle::new_at(config, base);
        let d = throttle.observe_at(true, base + Duration::from_millis(1));
        // W_1 = 1 * (1 + λ * (1 - μ₀)) > 1 since μ₀ < 1 and λ > 0
        prop_assert!(
            d.wealth > 1.0,
            "Match should increase wealth from 1.0: got {} (mu_0={})",
            d.wealth, mu_0
        );
    }
}

// 18. Observation count increments correctly

proptest! {
    #[test]
    fn throttle_obs_count_correct(
        config in throttle_config_strategy(),
        matches in match_sequence_strategy(50),
    ) {
        let base = Instant::now();
        let mut throttle = EProcessThrottle::new_at(config, base);
        for (i, &m) in matches.iter().enumerate() {
            throttle.observe_at(m, base + Duration::from_millis(i as u64 + 1));
        }
        prop_assert_eq!(
            throttle.observation_count(),
            matches.len() as u64,
            "Observation count should match"
        );
    }
}

// 19. Stats counters are consistent

proptest! {
    #[test]
    fn throttle_stats_consistent(
        config in throttle_config_strategy(),
        matches in match_sequence_strategy(100),
    ) {
        let base = Instant::now();
        let mut throttle = EProcessThrottle::new_at(config, base);
        for (i, &m) in matches.iter().enumerate() {
            throttle.observe_at(m, base + Duration::from_millis(i as u64 + 1));
        }
        let stats = throttle.stats();
        prop_assert_eq!(stats.total_observations, matches.len() as u64);
        // eprocess + forced = total
        prop_assert_eq!(
            stats.eprocess_recomputes + stats.forced_recomputes,
            stats.total_recomputes,
            "eprocess {} + forced {} != total {}",
            stats.eprocess_recomputes, stats.forced_recomputes, stats.total_recomputes
        );
        prop_assert!(stats.current_wealth > 0.0);
        prop_assert!(stats.current_lambda > 0.0);
        prop_assert!((0.0..=1.0).contains(&stats.empirical_rate));
    }
}

// 20. Determinism

proptest! {
    #[test]
    fn throttle_deterministic(
        config in throttle_config_strategy(),
        matches in match_sequence_strategy(50),
    ) {
        let base = Instant::now();
        let mut t1 = EProcessThrottle::new_at(config.clone(), base);
        let mut t2 = EProcessThrottle::new_at(config, base);
        for (i, &m) in matches.iter().enumerate() {
            let ts = base + Duration::from_millis(i as u64 + 1);
            t1.observe_at(m, ts);
            t2.observe_at(m, ts);
        }
        prop_assert!(
            (t1.wealth() - t2.wealth()).abs() < 1e-10,
            "Same inputs should give same wealth: {} vs {}",
            t1.wealth(), t2.wealth()
        );
        prop_assert!(
            (t1.lambda() - t2.lambda()).abs() < 1e-10,
            "Same inputs should give same lambda: {} vs {}",
            t1.lambda(), t2.lambda()
        );
    }
}

// 21. No panics on arbitrary match sequences

proptest! {
    #[test]
    fn throttle_no_panic(
        config in throttle_config_strategy(),
        matches in match_sequence_strategy(100),
    ) {
        let base = Instant::now();
        let mut throttle = EProcessThrottle::new_at(config, base);
        for (i, &m) in matches.iter().enumerate() {
            let _ = throttle.observe_at(m, base + Duration::from_millis(i as u64 + 1));
        }
        let _ = throttle.wealth();
        let _ = throttle.lambda();
        let _ = throttle.threshold();
        let _ = throttle.observation_count();
        let _ = throttle.empirical_match_rate();
        let _ = throttle.stats();
        throttle.reset_at(base + Duration::from_millis(1000));
    }
}
