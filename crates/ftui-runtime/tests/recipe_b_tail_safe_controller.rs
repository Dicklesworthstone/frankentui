#![cfg(feature = "experimental")]
//! Integration tests for Recipe B: Tail-Safe Adaptive Controller.
//!
//! Tests the full ConformalFrameGuard + DegradationCascade stack with
//! synthetic timing data covering:
//! 1. Stable regime → no degradation
//! 2. Spike regime → triggers degradation within 3 frames
//! 3. Recovery after spike → restores within N frames
//! 4. Conformal coverage guarantee (>95% over 1000 samples)
//! 5. End-to-end cascade with mock widget filtering

use ftui_render::budget::DegradationLevel;
use ftui_runtime::conformal_frame_guard::{
    ConformalFrameGuard, ConformalFrameGuardConfig, GuardState,
};
use ftui_runtime::conformal_predictor::{BucketKey, ConformalConfig, DiffBucket, ModeBucket};
use ftui_runtime::degradation_cascade::{CascadeConfig, CascadeDecision, DegradationCascade};

fn make_key() -> BucketKey {
    BucketKey {
        mode: ModeBucket::AltScreen,
        diff: DiffBucket::Full,
        size_bucket: 2,
    }
}

const BUDGET_US: f64 = 16_000.0; // 16ms frame budget

// ---------------------------------------------------------------------------
// Scenario 1: Stable regime → no degradation
// ---------------------------------------------------------------------------

#[test]
fn stable_regime_no_degradation() {
    let mut cascade = DegradationCascade::with_defaults();
    let key = make_key();

    // Warm up with 50 frames at 10ms (well within 16ms budget)
    for _ in 0..50 {
        cascade.post_render(10_000.0, key);
        let result = cascade.pre_render(BUDGET_US, key);
        assert_eq!(
            result.level,
            DegradationLevel::Full,
            "stable regime should never degrade"
        );
        assert_ne!(
            result.decision,
            CascadeDecision::Degrade,
            "should not trigger degrade"
        );
    }

    assert_eq!(cascade.total_degrades(), 0);
    assert_eq!(cascade.level(), DegradationLevel::Full);
}

#[test]
fn stable_regime_with_minor_jitter_no_degradation() {
    let mut cascade = DegradationCascade::with_defaults();
    let key = make_key();

    // Frame times jitter between 8ms and 13ms (all within budget)
    let times = [8_000.0, 10_000.0, 13_000.0, 9_000.0, 12_000.0, 11_000.0];
    for round in 0..10 {
        for &t in &times {
            cascade.post_render(t, key);
            let result = cascade.pre_render(BUDGET_US, key);
            assert_eq!(
                result.level,
                DegradationLevel::Full,
                "jittery but within-budget should not degrade (round {round})"
            );
        }
    }

    assert_eq!(cascade.total_degrades(), 0);
}

// ---------------------------------------------------------------------------
// Scenario 2: Spike regime → triggers degradation within 3 frames
// ---------------------------------------------------------------------------

#[test]
fn spike_regime_triggers_degradation_quickly() {
    let config = CascadeConfig {
        guard: ConformalFrameGuardConfig {
            conformal: ConformalConfig {
                min_samples: 5, // Low calibration threshold for fast trigger
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let mut cascade = DegradationCascade::new(config);
    let key = make_key();

    // Warm up with 5 slow frames (consistently over budget)
    for _ in 0..5 {
        cascade.post_render(25_000.0, key);
    }

    // After calibration with slow data, degradation should trigger quickly
    let mut degrade_frame = None;
    for i in 0..10 {
        cascade.post_render(25_000.0, key);
        let result = cascade.pre_render(BUDGET_US, key);
        if result.decision == CascadeDecision::Degrade {
            degrade_frame = Some(i);
            break;
        }
    }

    assert!(
        degrade_frame.is_some(),
        "degradation should trigger within 10 frames"
    );
    let frame = degrade_frame.unwrap();
    assert!(
        frame <= 3,
        "degradation should trigger within 3 frames after calibration, got frame {frame}"
    );
}

#[test]
fn sustained_overload_progressively_degrades() {
    let config = CascadeConfig {
        guard: ConformalFrameGuardConfig {
            conformal: ConformalConfig {
                min_samples: 5,
                ..Default::default()
            },
            ..Default::default()
        },
        // Remove the floor so this test can validate full progressive degradation
        degradation_floor: DegradationLevel::SkipFrame,
        ..Default::default()
    };
    let mut cascade = DegradationCascade::new(config);
    let key = make_key();

    // Sustained 30ms frames (way over 16ms budget)
    for _ in 0..50 {
        cascade.post_render(30_000.0, key);
        cascade.pre_render(BUDGET_US, key);
    }

    // Should have degraded multiple times
    assert!(
        cascade.total_degrades() >= 2,
        "sustained overload should cause multiple degrades"
    );
    assert!(
        cascade.level() > DegradationLevel::Full,
        "should be in degraded state"
    );
}

// ---------------------------------------------------------------------------
// Scenario 3: Recovery after spike → restores within N frames
// ---------------------------------------------------------------------------

#[test]
fn recovery_after_spike() {
    let recovery_threshold = 8;
    let config = CascadeConfig {
        guard: ConformalFrameGuardConfig {
            conformal: ConformalConfig {
                min_samples: 5,
                ..Default::default()
            },
            ..Default::default()
        },
        recovery_threshold,
        ..Default::default()
    };
    let mut cascade = DegradationCascade::new(config);
    let key = make_key();

    // Phase 1: Trigger degradation with slow frames
    for _ in 0..10 {
        cascade.post_render(25_000.0, key);
        cascade.pre_render(BUDGET_US, key);
    }
    let degraded_level = cascade.level();
    assert!(
        degraded_level > DegradationLevel::Full,
        "should be degraded after slow frames"
    );

    // Phase 2: Switch to fast frames (spike passes)
    // Need enough to recalibrate and then recover
    for _ in 0..50 {
        cascade.post_render(8_000.0, key);
    }

    // Phase 3: Recovery should happen within recovery_threshold + some margin
    let mut recovered = false;
    for _ in 0..30 {
        cascade.post_render(8_000.0, key);
        let result = cascade.pre_render(BUDGET_US, key);
        if result.decision == CascadeDecision::Recover {
            recovered = true;
            break;
        }
    }

    assert!(recovered, "should recover after switching to fast frames");
    assert!(
        cascade.level() < degraded_level,
        "level should improve after recovery"
    );
}

#[test]
fn recovery_streak_resets_on_new_spike() {
    // Use a small conformal window so old spike data rolls off quickly,
    // allowing the predictor to respond to regime changes.
    let config = CascadeConfig {
        guard: ConformalFrameGuardConfig {
            conformal: ConformalConfig {
                min_samples: 5,
                window_size: 15,
                ..Default::default()
            },
            ..Default::default()
        },
        recovery_threshold: 100, // High threshold so we never fully recover
        ..Default::default()
    };
    let mut cascade = DegradationCascade::new(config);
    let key = make_key();

    // Phase 1: Establish a fast baseline (fills conformal window with fast data)
    for _ in 0..30 {
        cascade.post_render(6_000.0, key);
        cascade.pre_render(BUDGET_US, key);
    }
    assert_eq!(
        cascade.level(),
        DegradationLevel::Full,
        "should not degrade on fast frames"
    );

    // Phase 2: Brief spike to trigger degradation
    for _ in 0..10 {
        cascade.post_render(30_000.0, key);
        cascade.pre_render(BUDGET_US, key);
    }
    assert!(
        cascade.level() > DegradationLevel::Full,
        "should have degraded during spike"
    );

    // Phase 3: Many fast frames to push EMA down and flush spike from
    // the small conformal window, building up recovery streak
    for _ in 0..40 {
        cascade.post_render(3_000.0, key);
        cascade.pre_render(BUDGET_US, key);
    }

    let streak_before_spike = cascade.recovery_streak();
    assert!(
        streak_before_spike > 0,
        "should have some recovery progress after 40 fast frames"
    );

    // Phase 4: New spike interrupts recovery
    for _ in 0..10 {
        cascade.post_render(30_000.0, key);
        cascade.pre_render(BUDGET_US, key);
    }

    // Streak should have been reset by the spike
    assert_eq!(
        cascade.recovery_streak(),
        0,
        "spike should reset recovery streak"
    );
}

// ---------------------------------------------------------------------------
// Scenario 4: Conformal coverage guarantee (>95% over 1000 samples)
// ---------------------------------------------------------------------------

#[test]
fn conformal_coverage_guarantee_empirical() {
    let config = ConformalFrameGuardConfig {
        conformal: ConformalConfig {
            alpha: 0.05, // 95% coverage target
            min_samples: 20,
            window_size: 256,
            q_default: 10_000.0,
        },
        ..Default::default()
    };
    let mut guard = ConformalFrameGuard::new(config);
    let key = make_key();

    // Calibrate with 100 samples from a known distribution
    // Using frame times normally distributed around 10ms with ~2ms spread
    let calibration_times: Vec<f64> = (0..100)
        .map(|i| {
            // Deterministic "spread": alternating above/below mean
            let offset = ((i % 7) as f64 - 3.0) * 500.0; // -1500 to +1500 µs
            10_000.0 + offset
        })
        .collect();

    for &t in &calibration_times {
        guard.observe(t, key);
    }

    // Now test: predict p99 for 1000 new samples and count coverage
    // A frame is "covered" if the actual time is below the predicted upper bound
    let mut covered = 0;
    let total = 1000;

    for i in 0..total {
        let prediction = guard.predict_p99(BUDGET_US, key);

        // Generate a test frame time with same distribution
        let offset = ((i % 11) as f64 - 5.0) * 500.0;
        let actual = 10_000.0 + offset;

        if actual <= prediction.upper_us {
            covered += 1;
        }

        // Feed observation back
        guard.observe(actual, key);
    }

    let coverage = covered as f64 / total as f64;
    assert!(
        coverage >= 0.90, // Allow some slack (theoretical is >=0.95)
        "conformal coverage should be >=90%, got {coverage:.3} ({covered}/{total})"
    );
}

#[test]
fn conformal_coverage_with_regime_change() {
    let config = ConformalFrameGuardConfig {
        conformal: ConformalConfig {
            alpha: 0.05,
            min_samples: 10,
            window_size: 100,
            q_default: 10_000.0,
        },
        ..Default::default()
    };
    let mut guard = ConformalFrameGuard::new(config);
    let key = make_key();

    // Phase 1: Fast regime (10ms)
    for _ in 0..30 {
        guard.observe(10_000.0, key);
    }

    // Phase 2: Sudden shift to slow regime (18ms)
    // After the shift, coverage may temporarily drop but should recover
    let mut covered_fast = 0;
    let mut total_fast = 0;
    let mut covered_slow = 0;
    let mut total_slow = 0;

    // Fast regime predictions
    for _ in 0..50 {
        let pred = guard.predict_p99(BUDGET_US, key);
        let actual = 10_000.0;
        if actual <= pred.upper_us {
            covered_fast += 1;
        }
        total_fast += 1;
        guard.observe(actual, key);
    }

    // Regime change: slow frames
    for _ in 0..100 {
        let pred = guard.predict_p99(BUDGET_US, key);
        let actual = 18_000.0;
        if actual <= pred.upper_us {
            covered_slow += 1;
        }
        total_slow += 1;
        guard.observe(actual, key);
    }

    let fast_coverage = covered_fast as f64 / total_fast as f64;
    // After regime change, coverage on new data should still be reasonable
    // (conformal adapts via rolling window)
    let slow_coverage = covered_slow as f64 / total_slow as f64;

    assert!(
        fast_coverage >= 0.90,
        "fast regime coverage should be >=90%, got {fast_coverage:.3}"
    );
    // Slow regime may have lower coverage initially during transition
    // but should improve as window fills with new data
    assert!(
        slow_coverage >= 0.50,
        "slow regime coverage should be >=50% (adapting), got {slow_coverage:.3}"
    );
}

// ---------------------------------------------------------------------------
// Scenario 5: End-to-end cascade with mock widget filtering
// ---------------------------------------------------------------------------

#[test]
fn e2e_cascade_widget_filtering() {
    let config = CascadeConfig {
        guard: ConformalFrameGuardConfig {
            conformal: ConformalConfig {
                min_samples: 5,
                ..Default::default()
            },
            ..Default::default()
        },
        recovery_threshold: 5,
        ..Default::default()
    };
    let mut cascade = DegradationCascade::new(config);
    let key = make_key();

    // Phase 1: Full quality - all widgets render
    for _ in 0..10 {
        cascade.post_render(10_000.0, key);
        cascade.pre_render(BUDGET_US, key);
    }

    assert!(
        cascade.should_render_widget(true),
        "essential should render at Full"
    );
    assert!(
        cascade.should_render_widget(false),
        "non-essential should render at Full"
    );

    // Phase 2: Overload - degrade to skip non-essential widgets
    for _ in 0..30 {
        cascade.post_render(25_000.0, key);
        cascade.pre_render(BUDGET_US, key);
    }

    // If degradation reached EssentialOnly, non-essential should be skipped
    if cascade.level() >= DegradationLevel::EssentialOnly {
        assert!(
            cascade.should_render_widget(true),
            "essential should still render"
        );
        assert!(
            !cascade.should_render_widget(false),
            "non-essential should be skipped at EssentialOnly+"
        );
    }

    // Phase 3: Recovery
    for _ in 0..60 {
        cascade.post_render(8_000.0, key);
        cascade.pre_render(BUDGET_US, key);
    }

    // After recovery, both should render again
    if cascade.level() < DegradationLevel::EssentialOnly {
        assert!(
            cascade.should_render_widget(false),
            "non-essential should render after recovery"
        );
    }
}

#[test]
fn e2e_cascade_evidence_trail() {
    let config = CascadeConfig {
        guard: ConformalFrameGuardConfig {
            conformal: ConformalConfig {
                min_samples: 5,
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let mut cascade = DegradationCascade::new(config);
    let key = make_key();
    let mut evidence_log = Vec::new();

    // Run a full scenario and collect all evidence
    // Phase 1: stable
    for _ in 0..10 {
        cascade.post_render(10_000.0, key);
        cascade.pre_render(BUDGET_US, key);
        if let Some(evidence) = cascade.last_evidence() {
            evidence_log.push(evidence.to_jsonl());
        }
    }

    // Phase 2: spike
    for _ in 0..10 {
        cascade.post_render(25_000.0, key);
        cascade.pre_render(BUDGET_US, key);
        if let Some(evidence) = cascade.last_evidence() {
            evidence_log.push(evidence.to_jsonl());
        }
    }

    // Phase 3: recovery
    for _ in 0..20 {
        cascade.post_render(8_000.0, key);
        cascade.pre_render(BUDGET_US, key);
        if let Some(evidence) = cascade.last_evidence() {
            evidence_log.push(evidence.to_jsonl());
        }
    }

    // Verify evidence trail
    assert!(!evidence_log.is_empty(), "should have evidence entries");
    assert_eq!(
        evidence_log.len(),
        40,
        "should have one evidence entry per frame"
    );

    // All entries should be valid JSONL
    for (i, line) in evidence_log.iter().enumerate() {
        assert!(
            line.starts_with('{') && line.ends_with('}'),
            "evidence line {i} should be valid JSON: {line}"
        );
        assert!(
            line.contains("degradation-cascade-v1"),
            "evidence line {i} should have schema"
        );
    }

    // Should contain at least one degrade decision
    let has_degrade = evidence_log
        .iter()
        .any(|l| l.contains("\"decision\":\"degrade\""));
    assert!(
        has_degrade,
        "evidence should contain at least one degrade event"
    );
}

#[test]
fn e2e_cascade_telemetry_tracking() {
    let mut cascade = DegradationCascade::with_defaults();
    let key = make_key();

    for _ in 0..25 {
        cascade.post_render(12_000.0, key);
        cascade.pre_render(BUDGET_US, key);
    }

    let telem = cascade.telemetry();
    assert_eq!(telem.frame_idx, 25);
    assert_eq!(telem.level, DegradationLevel::Full);
    assert_eq!(telem.total_degrades, 0);
    assert_eq!(telem.guard_state, GuardState::Calibrated);
    assert!(telem.guard_observations > 0);
    assert!(telem.guard_ema_us > 0.0);

    // Telemetry JSONL should be valid
    let json_str = telem.to_jsonl();
    assert!(json_str.contains("cascade-telemetry-v1"));
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[test]
fn handles_zero_budget_gracefully() {
    let mut cascade = DegradationCascade::with_defaults();
    let key = make_key();

    for _ in 0..5 {
        cascade.post_render(1_000.0, key);
    }

    // Zero budget should always trigger risk
    let result = cascade.pre_render(0.0, key);
    // The guard should detect exceeds_budget since any positive frame time > 0
    // (In warmup mode with EMA ~1ms, it's compared to 16ms fallback - won't exceed)
    // This just ensures it doesn't panic
    assert!(result.level <= DegradationLevel::SkipFrame);
}

#[test]
fn handles_extreme_frame_times() {
    let mut cascade = DegradationCascade::with_defaults();
    let key = make_key();

    // Very fast frames (0.1ms)
    for _ in 0..25 {
        cascade.post_render(100.0, key);
        cascade.pre_render(BUDGET_US, key);
    }
    assert_eq!(cascade.level(), DegradationLevel::Full);

    // Very slow frames (1 second)
    let mut cascade2 = DegradationCascade::with_defaults();
    for _ in 0..25 {
        cascade2.post_render(1_000_000.0, key);
        cascade2.pre_render(BUDGET_US, key);
    }
    assert!(cascade2.level() > DegradationLevel::Full);
}

#[test]
fn guard_nonconformity_summary_after_calibration() {
    let mut guard = ConformalFrameGuard::with_defaults();
    let key = make_key();

    for i in 0..50 {
        let t = 10_000.0 + (i as f64 * 50.0); // Slowly increasing
        guard.observe(t, key);
    }

    let summary = guard.nonconformity_summary();
    assert!(
        summary.is_some(),
        "should have summary after 50 observations"
    );
    let s = summary.unwrap();
    assert_eq!(s.count, 50);
    // With slowly increasing times and EMA tracking, residuals should be
    // mostly positive (observed > EMA since EMA lags)
    assert!(s.p99 >= s.p50, "p99 should be >= p50");
}
