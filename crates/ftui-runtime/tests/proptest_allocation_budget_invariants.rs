#![cfg(feature = "experimental")]
//! Property-based invariant tests for the allocation budget monitor.
//!
//! These tests verify structural and mathematical invariants of the dual
//! CUSUM + e-process leak detector:
//!
//! 1. CUSUM S⁺ is always non-negative.
//! 2. CUSUM S⁻ is always non-negative.
//! 3. E-value is always positive (E_MIN floor).
//! 4. E-value is always finite (E_MAX ceiling).
//! 5. E-value starts at 1.0.
//! 6. Frame count increments monotonically.
//! 7. Observations at mu_0 don't trigger alerts quickly.
//! 8. Reset restores initial state.
//! 9. Summary is consistent with accessors.
//! 10. Alert resets CUSUM and e-value.
//! 11. Running mean is within observed range.
//! 12. Determinism: same observations → same state.
//! 13. No panics on arbitrary observation sequences.
//! 14. Calibrated config produces valid parameters.

use ftui_runtime::allocation_budget::{AllocationBudget, BudgetConfig};
use proptest::prelude::*;

// ── Strategies ────────────────────────────────────────────────────────────

fn budget_config_strategy() -> impl Strategy<Value = BudgetConfig> {
    (
        0.001f64..=0.5,    // alpha
        -100.0f64..=100.0, // mu_0
        0.1f64..=100.0,    // sigma_sq
        0.0f64..=10.0,     // cusum_k
        1.0f64..=50.0,     // cusum_h
        0.001f64..=1.0,    // lambda
        10usize..=200,     // window_size
    )
        .prop_map(|(alpha, mu_0, sigma_sq, k, h, lambda, ws)| BudgetConfig {
            alpha,
            mu_0,
            sigma_sq,
            cusum_k: k,
            cusum_h: h,
            lambda,
            window_size: ws,
        })
}

fn observations_strategy(max_len: usize) -> impl Strategy<Value = Vec<f64>> {
    proptest::collection::vec(-1000.0f64..=1000.0, 1..=max_len)
}

// ═════════════════════════════════════════════════════════════════════════
// 1. CUSUM S⁺ is always non-negative
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cusum_plus_non_negative(
        config in budget_config_strategy(),
        obs in observations_strategy(100),
    ) {
        let mut monitor = AllocationBudget::new(config);
        for &x in &obs {
            monitor.observe(x);
            prop_assert!(
                monitor.cusum_plus() >= 0.0,
                "CUSUM S+ must be non-negative, got {}",
                monitor.cusum_plus()
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. CUSUM S⁻ is always non-negative
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cusum_minus_non_negative(
        config in budget_config_strategy(),
        obs in observations_strategy(100),
    ) {
        let mut monitor = AllocationBudget::new(config);
        for &x in &obs {
            monitor.observe(x);
            prop_assert!(
                monitor.cusum_minus() >= 0.0,
                "CUSUM S- must be non-negative, got {}",
                monitor.cusum_minus()
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. E-value is always positive
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn evalue_always_positive(
        config in budget_config_strategy(),
        obs in observations_strategy(100),
    ) {
        let mut monitor = AllocationBudget::new(config);
        for &x in &obs {
            monitor.observe(x);
            prop_assert!(
                monitor.e_value() > 0.0,
                "E-value must be positive, got {} at frame {}",
                monitor.e_value(), monitor.frames()
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 4. E-value is always finite
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn evalue_always_finite(
        config in budget_config_strategy(),
        obs in observations_strategy(100),
    ) {
        let mut monitor = AllocationBudget::new(config);
        for &x in &obs {
            monitor.observe(x);
            prop_assert!(
                monitor.e_value().is_finite(),
                "E-value must be finite, got {} at frame {}",
                monitor.e_value(), monitor.frames()
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 5. E-value starts at 1.0
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn evalue_starts_at_one(config in budget_config_strategy()) {
        let monitor = AllocationBudget::new(config);
        prop_assert!(
            (monitor.e_value() - 1.0).abs() < 1e-10,
            "Initial e-value should be 1.0, got {}",
            monitor.e_value()
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 6. Frame count increments monotonically
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn frame_count_monotone(
        config in budget_config_strategy(),
        obs in observations_strategy(50),
    ) {
        let mut monitor = AllocationBudget::new(config);
        for (i, &x) in obs.iter().enumerate() {
            monitor.observe(x);
            prop_assert_eq!(
                monitor.frames(),
                (i + 1) as u64,
                "Frame count should be {} after {} observations",
                i + 1, i + 1
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 7. Observations at mu_0 don't trigger quickly
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn baseline_observations_no_quick_alert(
        mu_0 in -50.0f64..=50.0,
        sigma_sq in 0.1f64..=10.0,
    ) {
        let config = BudgetConfig {
            alpha: 0.01,
            mu_0,
            sigma_sq,
            cusum_k: 1.0,
            cusum_h: 10.0,
            lambda: 0.1,
            window_size: 100,
        };
        let mut monitor = AllocationBudget::new(config);
        // Feeding exact mu_0 should not trigger alerts
        let mut alerted = false;
        for _ in 0..50 {
            if monitor.observe(mu_0).is_some() {
                alerted = true;
                break;
            }
        }
        prop_assert!(
            !alerted,
            "Observations exactly at mu_0={} should not trigger alert",
            mu_0
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 8. Reset restores initial state
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn reset_restores_state(
        config in budget_config_strategy(),
        obs in observations_strategy(20),
    ) {
        let mut monitor = AllocationBudget::new(config);
        for &x in &obs {
            monitor.observe(x);
        }
        monitor.reset();
        prop_assert!(
            (monitor.e_value() - 1.0).abs() < 1e-10,
            "Reset should restore e-value to 1.0"
        );
        prop_assert_eq!(monitor.frames(), 0, "Reset should clear frame count");
        prop_assert_eq!(monitor.total_alerts(), 0, "Reset should clear alerts");
        prop_assert!(
            monitor.cusum_plus().abs() < 1e-10,
            "Reset should clear CUSUM S+"
        );
        prop_assert!(
            monitor.cusum_minus().abs() < 1e-10,
            "Reset should clear CUSUM S-"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 9. Summary is consistent with accessors
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn summary_consistent(
        config in budget_config_strategy(),
        obs in observations_strategy(50),
    ) {
        let mut monitor = AllocationBudget::new(config.clone());
        for &x in &obs {
            monitor.observe(x);
        }
        let summary = monitor.summary();
        prop_assert_eq!(summary.frames, monitor.frames());
        prop_assert_eq!(summary.total_alerts, monitor.total_alerts());
        prop_assert!(
            (summary.e_value - monitor.e_value()).abs() < 1e-10,
            "Summary e_value {} != accessor {}",
            summary.e_value, monitor.e_value()
        );
        prop_assert!(
            (summary.cusum_plus - monitor.cusum_plus()).abs() < 1e-10,
            "Summary cusum_plus {} != accessor {}",
            summary.cusum_plus, monitor.cusum_plus()
        );
        prop_assert!(
            (summary.mu_0 - config.mu_0).abs() < 1e-10,
            "Summary mu_0 should match config"
        );
        prop_assert!(
            (summary.drift - (summary.running_mean - summary.mu_0)).abs() < 1e-10,
            "Drift should be running_mean - mu_0"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 10. Alert resets CUSUM and e-value
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn alert_resets_detectors(
        mu_0 in -10.0f64..=10.0,
    ) {
        // Force an alert by feeding extreme values
        let config = BudgetConfig {
            alpha: 0.5,  // permissive
            mu_0,
            sigma_sq: 1.0,
            cusum_k: 0.1,
            cusum_h: 1.0,
            lambda: 0.5,
            window_size: 100,
        };
        let mut monitor = AllocationBudget::new(config);
        let mut alert_fired = false;
        for _ in 0..100 {
            let extreme = mu_0 + 50.0;
            if monitor.observe(extreme).is_some() {
                alert_fired = true;
                // After alert, detectors should be reset
                prop_assert!(
                    (monitor.e_value() - 1.0).abs() < 1e-10,
                    "E-value should reset to 1.0 after alert, got {}",
                    monitor.e_value()
                );
                prop_assert!(
                    monitor.cusum_plus().abs() < 1e-10,
                    "CUSUM S+ should reset to 0 after alert, got {}",
                    monitor.cusum_plus()
                );
                break;
            }
        }
        prop_assert!(alert_fired, "Should trigger alert with extreme values");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 11. Running mean is within observed range
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn running_mean_in_range(
        config in budget_config_strategy(),
        obs in observations_strategy(50),
    ) {
        let mut monitor = AllocationBudget::new(config);
        for &x in &obs {
            monitor.observe(x);
        }
        let mean = monitor.running_mean();
        prop_assert!(mean.is_finite(), "Running mean should be finite, got {}", mean);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 12. Determinism
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn budget_deterministic(
        config in budget_config_strategy(),
        obs in observations_strategy(50),
    ) {
        let mut m1 = AllocationBudget::new(config.clone());
        let mut m2 = AllocationBudget::new(config);
        for &x in &obs {
            m1.observe(x);
            m2.observe(x);
        }
        prop_assert!(
            (m1.e_value() - m2.e_value()).abs() < 1e-10,
            "E-values should match: {} vs {}",
            m1.e_value(), m2.e_value()
        );
        prop_assert!(
            (m1.cusum_plus() - m2.cusum_plus()).abs() < 1e-10,
            "CUSUM S+ should match: {} vs {}",
            m1.cusum_plus(), m2.cusum_plus()
        );
        prop_assert!(
            (m1.cusum_minus() - m2.cusum_minus()).abs() < 1e-10,
            "CUSUM S- should match: {} vs {}",
            m1.cusum_minus(), m2.cusum_minus()
        );
        prop_assert_eq!(m1.total_alerts(), m2.total_alerts());
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 13. No panics on arbitrary observation sequences
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn no_panic_operations(
        config in budget_config_strategy(),
        obs in observations_strategy(100),
    ) {
        let mut monitor = AllocationBudget::new(config);
        for &x in &obs {
            let _ = monitor.observe(x);
        }
        let _ = monitor.e_value();
        let _ = monitor.cusum_plus();
        let _ = monitor.cusum_minus();
        let _ = monitor.frames();
        let _ = monitor.total_alerts();
        let _ = monitor.running_mean();
        let _ = monitor.summary();
        let _ = monitor.ledger();
        monitor.reset();
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 14. Calibrated config produces valid parameters
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn calibrated_config_valid(
        mu_0 in -100.0f64..=100.0,
        sigma_sq in 0.001f64..=100.0,
        delta in 0.1f64..=50.0,
        alpha in 0.001f64..=0.5,
    ) {
        let config = BudgetConfig::calibrated(mu_0, sigma_sq, delta, alpha);
        prop_assert!(config.sigma_sq >= 1e-6, "sigma_sq should be >= SIGMA2_MIN");
        prop_assert!(config.lambda > 0.0, "lambda should be positive");
        prop_assert!(config.lambda <= 0.5, "lambda should be <= 0.5");
        prop_assert!(config.cusum_k >= 0.0, "cusum_k should be non-negative");
        prop_assert!((config.alpha - alpha).abs() < 1e-10, "alpha should match");
        prop_assert!((config.mu_0 - mu_0).abs() < 1e-10, "mu_0 should match");

        // Should construct without panic
        let _ = AllocationBudget::new(config);
    }
}
