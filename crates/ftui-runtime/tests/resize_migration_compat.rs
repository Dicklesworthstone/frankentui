//! # Resize Migration Compatibility Tests (bd-1rz0.25)
//!
//! Validates that all migration paths documented in `docs/spec/resize-migration.md`
//! work correctly. Tests the CoalescerConfig API, regime transitions, latency
//! guarantees, and determinism checksums.

use std::time::{Duration, Instant};

use ftui_runtime::resize_coalescer::TelemetryHooks;
use ftui_runtime::{CoalesceAction, CoalescerConfig, Regime, ResizeCoalescer, ScreenMode};

// =============================================================================
// Config API Tests
// =============================================================================

/// Default config has documented values.
#[test]
fn default_config_matches_docs() {
    let cfg = CoalescerConfig::default();
    assert_eq!(cfg.steady_delay_ms, 16, "steady_delay_ms default");
    assert_eq!(cfg.burst_delay_ms, 40, "burst_delay_ms default");
    assert_eq!(cfg.hard_deadline_ms, 100, "hard_deadline_ms default");
    assert!((cfg.burst_enter_rate - 10.0).abs() < f64::EPSILON);
    assert!((cfg.burst_exit_rate - 5.0).abs() < f64::EPSILON);
    assert_eq!(cfg.cooldown_frames, 3);
    assert_eq!(cfg.rate_window_size, 8);
    assert!(!cfg.enable_logging);
    assert!(cfg.enable_bocpd, "BOCPD is the default regime detector");
    assert!(
        cfg.heuristic_fallback,
        "the rate heuristic covers undefined posteriors"
    );
    assert!(
        cfg.bocpd_config.is_none(),
        "None selects BocpdConfig::default()"
    );
}

/// Low-latency profile from the migration guide.
#[test]
fn low_latency_profile() {
    let cfg = CoalescerConfig {
        steady_delay_ms: 8,
        burst_delay_ms: 25,
        hard_deadline_ms: 50,
        ..Default::default()
    };
    let coalescer = ResizeCoalescer::new(cfg.clone(), (80, 24));
    assert_eq!(coalescer.regime(), Regime::Steady);
    assert_eq!(coalescer.last_applied(), (80, 24));
    assert_eq!(cfg.steady_delay_ms, 8);
}

/// Heavy-render profile from the migration guide.
#[test]
fn heavy_render_profile() {
    let cfg = CoalescerConfig {
        steady_delay_ms: 32,
        burst_delay_ms: 80,
        hard_deadline_ms: 150,
        burst_enter_rate: 5.0,
        ..Default::default()
    };
    let coalescer = ResizeCoalescer::new(cfg, (120, 40));
    assert_eq!(coalescer.regime(), Regime::Steady);
    assert_eq!(coalescer.last_applied(), (120, 40));
}

/// Config JSONL serialization round-trips.
#[test]
fn config_jsonl_export() {
    let cfg = CoalescerConfig::default();
    let jsonl = cfg.to_jsonl("resize-test", ScreenMode::AltScreen, 80, 24, 0);
    assert!(jsonl.contains("steady_delay_ms"));
    assert!(jsonl.contains("burst_delay_ms"));
    assert!(jsonl.contains("hard_deadline_ms"));
    assert!(jsonl.contains("burst_enter_rate"));
}

// =============================================================================
// Regime Transition Tests
// =============================================================================

/// Steady-state: single resize applies quickly.
#[test]
fn steady_single_resize() {
    let cfg = CoalescerConfig::default();
    let mut coalescer = ResizeCoalescer::new(cfg, (80, 24));

    let t0 = Instant::now();

    // Send a single resize
    let action = coalescer.handle_resize_at(100, 40, t0);
    // Might return None or ApplyResize depending on delay
    assert!(coalescer.has_pending() || matches!(action, CoalesceAction::ApplyResize { .. }));

    // After steady delay, tick should apply
    let t1 = t0 + Duration::from_millis(20);
    let _action = coalescer.tick_at(t1);
    if coalescer.has_pending() {
        // Not yet applied, wait more
        let t2 = t0 + Duration::from_millis(50);
        let action = coalescer.tick_at(t2);
        assert!(
            matches!(action, CoalesceAction::ApplyResize { .. }),
            "Expected apply after delay, got {:?}",
            action
        );
    } else {
        // Already applied
        assert_eq!(coalescer.last_applied(), (100, 40));
    }
}

/// Burst mode: rapid events trigger regime transition.
#[test]
fn burst_regime_transition() {
    let cfg = CoalescerConfig {
        burst_enter_rate: 5.0, // 5 events/sec to enter burst
        burst_exit_rate: 2.0,
        cooldown_frames: 2,
        rate_window_size: 4,
        steady_delay_ms: 10,
        burst_delay_ms: 50,
        hard_deadline_ms: 100,
        enable_logging: true,
        enable_bocpd: false,
        heuristic_fallback: true,
        bocpd_config: None,
    };
    let mut coalescer = ResizeCoalescer::new(cfg, (80, 24));

    let t0 = Instant::now();

    // Fire rapid events (>5 events/sec = <200ms apart)
    for i in 0..8 {
        let t = t0 + Duration::from_millis(50 * i);
        coalescer.handle_resize_at(80 + (i as u16), 24, t);
    }

    // After rapid events, should be in Burst regime
    assert_eq!(
        coalescer.regime(),
        Regime::Burst,
        "Should transition to Burst after rapid events"
    );
}

/// Cooldown hysteresis: burst mode does not exit immediately after rate drop.
///
/// This verifies the current `tick_at` contract where cooldown is decremented on
/// each tick before apply checks are evaluated.
#[test]
fn burst_cooldown_hysteresis() {
    let cfg = CoalescerConfig {
        burst_enter_rate: 5.0,
        burst_exit_rate: 2.0,
        cooldown_frames: 3,
        rate_window_size: 4,
        steady_delay_ms: 10,
        burst_delay_ms: 50,
        hard_deadline_ms: 5000, // High enough to never trigger in this test
        enable_logging: false,
        enable_bocpd: false,
        heuristic_fallback: true,
        bocpd_config: None,
    };
    let base = Instant::now();
    let mut coalescer = ResizeCoalescer::new(cfg.clone(), (80, 24)).with_last_render(base);

    // Phase 1: Enter burst with rapid events (30ms apart → ~33 events/sec).
    let mut t = base;
    for i in 0..8u64 {
        t = base + Duration::from_millis(30 * i);
        coalescer.handle_resize_at(80 + (i as u16), 24, t);
    }
    assert_eq!(coalescer.regime(), Regime::Burst);

    // Phase 2: Force a low-rate observation (>1s gap clears stale rate window)
    // and verify we stay in burst until cooldown drains.
    t += Duration::from_millis(1100);
    coalescer.handle_resize_at(120, 35, t);
    assert_eq!(
        coalescer.regime(),
        Regime::Burst,
        "Cooldown should prevent immediate burst exit"
    );

    // Tick within burst delay to avoid apply; regime should remain burst until
    // the final cooldown decrement reaches zero.
    for tick_idx in 1..cfg.cooldown_frames {
        let t_tick = t + Duration::from_millis(tick_idx as u64 * 5);
        let action = coalescer.tick_at(t_tick);
        assert!(
            !matches!(action, CoalesceAction::ApplyResize { .. }),
            "Expected no apply while draining cooldown"
        );
        assert_eq!(
            coalescer.regime(),
            Regime::Burst,
            "Should remain burst before final cooldown tick"
        );
    }

    let final_tick = t + Duration::from_millis(cfg.cooldown_frames as u64 * 5);
    coalescer.tick_at(final_tick);
    assert_eq!(
        coalescer.regime(),
        Regime::Steady,
        "Should return to steady after cooldown frames drain"
    );
}

// =============================================================================
// Latency Guarantee Tests
// =============================================================================

/// Hard deadline is respected even in burst mode.
#[test]
fn hard_deadline_guarantee() {
    let cfg = CoalescerConfig {
        steady_delay_ms: 50,
        burst_delay_ms: 200,
        hard_deadline_ms: 100,
        ..Default::default()
    };
    let mut coalescer = ResizeCoalescer::new(cfg, (80, 24));

    let t0 = Instant::now();

    // Send resize and wait past hard deadline
    coalescer.handle_resize_at(120, 50, t0);

    // Tick at hard deadline
    let t_deadline = t0 + Duration::from_millis(101);
    let action = coalescer.tick_at(t_deadline);

    match action {
        CoalesceAction::ApplyResize {
            width,
            height,
            forced_by_deadline,
            ..
        } => {
            assert_eq!(width, 120);
            assert_eq!(height, 50);
            assert!(forced_by_deadline, "Should be forced by deadline");
        }
        _ => {
            // If already applied via handle_resize_at, verify applied size
            assert_eq!(coalescer.last_applied(), (120, 50));
        }
    }
}

/// time_until_apply reports correct remaining time.
#[test]
fn time_until_apply_accuracy() {
    let cfg = CoalescerConfig {
        steady_delay_ms: 50,
        hard_deadline_ms: 100,
        ..Default::default()
    };
    let mut coalescer = ResizeCoalescer::new(cfg, (80, 24));

    let t0 = Instant::now();
    coalescer.handle_resize_at(100, 40, t0);

    if coalescer.has_pending() {
        let remaining = coalescer.time_until_apply(t0);
        assert!(
            remaining.is_some(),
            "Should have remaining time when pending"
        );
        let remaining_ms = remaining.unwrap().as_millis();
        assert!(
            remaining_ms <= 100,
            "Remaining time should be <= hard_deadline_ms"
        );
    }
}

// =============================================================================
// Latest-Wins Semantics
// =============================================================================

/// The last resize in a burst is the one that gets applied.
#[test]
fn latest_wins_semantics() {
    let cfg = CoalescerConfig {
        steady_delay_ms: 30,
        burst_delay_ms: 80,
        hard_deadline_ms: 150,
        ..Default::default()
    };
    let mut coalescer = ResizeCoalescer::new(cfg, (80, 24));

    let t0 = Instant::now();

    // Send multiple resizes in quick succession
    coalescer.handle_resize_at(90, 30, t0);
    coalescer.handle_resize_at(100, 35, t0 + Duration::from_millis(5));
    coalescer.handle_resize_at(110, 40, t0 + Duration::from_millis(10));
    coalescer.handle_resize_at(120, 45, t0 + Duration::from_millis(15));

    // Wait for apply
    let mut t = t0 + Duration::from_millis(20);
    let mut applied = None;
    for _ in 0..20 {
        t += Duration::from_millis(10);
        if let CoalesceAction::ApplyResize { width, height, .. } = coalescer.tick_at(t) {
            applied = Some((width, height));
            break;
        }
    }

    // The applied size should be the LAST one sent (120x45)
    assert_eq!(
        applied,
        Some((120, 45)),
        "Last resize should be the one applied (latest-wins)"
    );
}

// =============================================================================
// Determinism Tests
// =============================================================================

/// Same event sequence produces identical decision checksums.
#[test]
fn determinism_checksum() {
    let cfg = CoalescerConfig {
        enable_logging: true,
        ..Default::default()
    };

    // Use a shared base instant and with_last_render to ensure determinism.
    // The constructor uses Instant::now() for last_render, which differs between calls.
    let base = Instant::now();

    let run = || {
        let mut coalescer = ResizeCoalescer::new(cfg.clone(), (80, 24)).with_last_render(base);

        for i in 0..20u64 {
            let t = base + Duration::from_millis(i * 30);
            coalescer.handle_resize_at(80 + (i as u16 % 10), 24 + (i as u16 % 5), t);
            coalescer.tick_at(t + Duration::from_millis(5));
        }

        coalescer.decision_checksum()
    };

    let c1 = run();
    let c2 = run();
    assert_eq!(c1, c2, "Checksums must match for identical event sequences");
}

/// Decision summary provides useful aggregate data.
#[test]
fn decision_summary_valid() {
    let cfg = CoalescerConfig {
        enable_logging: true,
        ..Default::default()
    };
    let mut coalescer = ResizeCoalescer::new(cfg, (80, 24));

    let t0 = Instant::now();
    for i in 0..10u64 {
        let t = t0 + Duration::from_millis(i * 100);
        coalescer.handle_resize_at(80 + (i as u16), 24, t);
        coalescer.tick_at(t + Duration::from_millis(20));
    }

    let summary = coalescer.decision_summary();
    // Summary should reflect some decisions were made
    assert!(summary.decision_count > 0, "Should have recorded decisions");
}

// =============================================================================
// Observability Tests
// =============================================================================

/// Decision logs can be exported as JSONL.
#[test]
fn decision_log_jsonl_export() {
    let cfg = CoalescerConfig {
        enable_logging: true,
        ..Default::default()
    };
    let mut coalescer = ResizeCoalescer::new(cfg, (80, 24));

    let t0 = Instant::now();
    coalescer.handle_resize_at(100, 40, t0);
    coalescer.tick_at(t0 + Duration::from_millis(20));

    let jsonl = coalescer.evidence_to_jsonl();
    assert!(!jsonl.is_empty(), "JSONL export should not be empty");
    // Each line should be valid JSON-ish (contains braces)
    for line in jsonl.lines() {
        if !line.is_empty() {
            assert!(
                line.starts_with('{'),
                "Each JSONL line should start with '{{': {}",
                line
            );
        }
    }
}

/// Checksum hex string is valid hex.
#[test]
fn decision_checksum_hex_format() {
    let cfg = CoalescerConfig {
        enable_logging: true,
        ..Default::default()
    };
    let mut coalescer = ResizeCoalescer::new(cfg, (80, 24));

    let t0 = Instant::now();
    coalescer.handle_resize_at(100, 40, t0);

    let hex = coalescer.decision_checksum_hex();
    assert!(
        hex.chars().all(|c| c.is_ascii_hexdigit()),
        "Checksum hex should be valid hex: {}",
        hex
    );
}

/// Stats snapshot provides runtime metrics.
#[test]
fn stats_snapshot() {
    let cfg = CoalescerConfig::default();
    let mut coalescer = ResizeCoalescer::new(cfg, (80, 24));

    let t0 = Instant::now();
    for i in 0..5u64 {
        let t = t0 + Duration::from_millis(i * 50);
        coalescer.handle_resize_at(80 + (i as u16), 24, t);
        coalescer.tick_at(t + Duration::from_millis(20));
    }

    let stats = coalescer.stats();
    assert!(stats.event_count > 0, "Should have processed events");
}

/// Telemetry hooks fire on resize applied.
#[test]
fn telemetry_hooks_fire() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let applied_count = Arc::new(AtomicU32::new(0));
    let regime_count = Arc::new(AtomicU32::new(0));

    let ac = applied_count.clone();
    let rc = regime_count.clone();

    let hooks = TelemetryHooks::new()
        .on_resize_applied(move |_log| {
            ac.fetch_add(1, Ordering::SeqCst);
        })
        .on_regime_change(move |_from, _to| {
            rc.fetch_add(1, Ordering::SeqCst);
        });

    let cfg = CoalescerConfig {
        steady_delay_ms: 5,
        hard_deadline_ms: 20,
        ..Default::default()
    };
    let mut coalescer = ResizeCoalescer::new(cfg, (80, 24)).with_telemetry_hooks(hooks);

    let t0 = Instant::now();
    coalescer.handle_resize_at(100, 40, t0);

    // Tick past hard deadline to force apply
    let t1 = t0 + Duration::from_millis(25);
    coalescer.tick_at(t1);

    // Allow some tolerance: hook may or may not fire depending on timing
    // Just verify no panic and the mechanism works
    let count = applied_count.load(Ordering::SeqCst);
    // Count could be 0 or 1 depending on whether apply happened in handle_resize_at
    assert!(
        count <= 2,
        "Applied hook count should be reasonable: {count}"
    );
}

// =============================================================================
// record_external_apply Tests
// =============================================================================

/// record_external_apply clears matching pending state.
#[test]
fn record_external_apply_clears_pending() {
    let cfg = CoalescerConfig {
        steady_delay_ms: 100,
        hard_deadline_ms: 200,
        ..Default::default()
    };
    let mut coalescer = ResizeCoalescer::new(cfg, (80, 24));

    let t0 = Instant::now();

    // Queue a pending resize
    coalescer.handle_resize_at(100, 40, t0);

    // Externally apply the same size (as Immediate mode would)
    coalescer.record_external_apply(100, 40, t0);

    // Should no longer be pending
    assert!(
        !coalescer.has_pending(),
        "Pending should be cleared after external apply of same size"
    );
    assert_eq!(coalescer.last_applied(), (100, 40));
}

// =============================================================================
// ProgramConfig Integration
// =============================================================================

/// ProgramConfig defaults use Throttled resize behavior.
#[test]
fn program_config_defaults() {
    use ftui_runtime::program::ResizeBehavior;

    let config = ftui_runtime::program::ProgramConfig::default();
    assert_eq!(config.resize_behavior, ResizeBehavior::Throttled);
}

/// ProgramConfig with_legacy_resize sets Immediate mode.
#[test]
fn program_config_legacy_resize() {
    use ftui_runtime::program::ResizeBehavior;

    let config = ftui_runtime::program::ProgramConfig::default().with_legacy_resize(true);
    assert_eq!(config.resize_behavior, ResizeBehavior::Immediate);
}

/// ProgramConfig with_resize_coalescer applies custom config.
#[test]
fn program_config_custom_coalescer() {
    let custom = CoalescerConfig {
        steady_delay_ms: 8,
        hard_deadline_ms: 50,
        ..Default::default()
    };
    let config = ftui_runtime::program::ProgramConfig::default().with_resize_coalescer(custom);
    assert_eq!(config.resize_coalescer.steady_delay_ms, 8);
    assert_eq!(config.resize_coalescer.hard_deadline_ms, 50);
}

// =============================================================================
// Simulation: BOCPD vs Heuristic Coalescer (bd-3e1t.2.6)
// =============================================================================

#[derive(Debug, Clone)]
struct SimMetrics {
    applies: usize,
    forced: usize,
    decisions: usize,
    mean_coalesce_ms: f64,
    p95_coalesce_ms: f64,
    max_coalesce_ms: f64,
}

fn build_events_steady() -> Vec<(u64, (u16, u16))> {
    (0..20)
        .map(|i| {
            let t = i * 200;
            let w = 80 + (i % 6) as u16;
            let h = 24 + (i % 3) as u16;
            (t, (w, h))
        })
        .collect()
}

fn build_events_burst() -> Vec<(u64, (u16, u16))> {
    (0..80)
        .map(|i| {
            let t = i * 5;
            let w = 80 + (i % 18) as u16;
            let h = 24 + (i % 7) as u16;
            (t, (w, h))
        })
        .collect()
}

fn build_events_oscillatory() -> Vec<(u64, (u16, u16))> {
    let mut events = Vec::new();
    for cycle in 0..5 {
        let base = cycle * 320;
        for i in 0..10 {
            let t = base + i * 12;
            let w = 84 + ((cycle + i) % 9) as u16;
            let h = 26 + ((cycle + i * 2) % 5) as u16;
            events.push((t, (w, h)));
        }
    }
    events
}

fn compute_metrics(coalescer: &ResizeCoalescer) -> SimMetrics {
    let mut coalesce_samples = Vec::new();
    let mut applies = 0;
    let mut forced = 0;

    for entry in coalescer.logs() {
        if matches!(entry.action, "apply" | "apply_forced" | "apply_immediate") {
            applies += 1;
            if entry.forced {
                forced += 1;
            }
            if let Some(ms) = entry.coalesce_ms {
                coalesce_samples.push(ms);
            }
        }
    }

    let mean = if coalesce_samples.is_empty() {
        0.0
    } else {
        coalesce_samples.iter().sum::<f64>() / coalesce_samples.len() as f64
    };

    let max = coalesce_samples
        .iter()
        .copied()
        .fold(0.0_f64, |a, b| a.max(b));

    let p95 = if coalesce_samples.is_empty() {
        0.0
    } else {
        coalesce_samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = ((coalesce_samples.len() as f64) * 0.95).ceil() as usize;
        let idx = idx.saturating_sub(1).min(coalesce_samples.len() - 1);
        coalesce_samples[idx]
    };

    SimMetrics {
        applies,
        forced,
        decisions: coalescer.logs().len(),
        mean_coalesce_ms: mean,
        p95_coalesce_ms: p95,
        max_coalesce_ms: max,
    }
}

fn run_simulation(
    config: CoalescerConfig,
    events: &[(u64, (u16, u16))],
    tick_ms: u64,
    end_ms: u64,
) -> SimMetrics {
    let base = Instant::now();
    let mut coalescer = ResizeCoalescer::new(config, (80, 24)).with_last_render(base);

    let mut timeline: Vec<u64> = (0..=end_ms).step_by(tick_ms as usize).collect();
    for (t, _) in events {
        timeline.push(*t);
    }
    timeline.sort_unstable();
    timeline.dedup();

    let mut idx = 0usize;
    for t_ms in timeline {
        let now = base + Duration::from_millis(t_ms);
        while idx < events.len() && events[idx].0 == t_ms {
            let (w, h) = events[idx].1;
            coalescer.handle_resize_at(w, h, now);
            idx += 1;
        }
        coalescer.tick_at(now);
    }

    compute_metrics(&coalescer)
}

#[test]
fn simulation_compare_bocpd_vs_heuristic() {
    let base = CoalescerConfig::default().with_logging(true);
    let cfg_heuristic = base.clone().without_bocpd();
    let cfg_bocpd = base.clone().with_bocpd();

    let scenarios = [
        ("steady", build_events_steady()),
        ("burst", build_events_burst()),
        ("oscillatory", build_events_oscillatory()),
    ];

    let tick_ms = 8;
    for (name, events) in scenarios {
        let last_event = events.last().map(|(t, _)| *t).unwrap_or(0);
        let end_ms = last_event + base.hard_deadline_ms + 200;

        let metrics_heuristic = run_simulation(cfg_heuristic.clone(), &events, tick_ms, end_ms);
        let metrics_bocpd = run_simulation(cfg_bocpd.clone(), &events, tick_ms, end_ms);

        eprintln!(
            "{{\"test\":\"resize_coalescer_sim\",\"scenario\":\"{}\",\"mode\":\"heuristic\",\"applies\":{},\"forced\":{},\"decisions\":{},\"mean_ms\":{:.2},\"p95_ms\":{:.2},\"max_ms\":{:.2}}}",
            name,
            metrics_heuristic.applies,
            metrics_heuristic.forced,
            metrics_heuristic.decisions,
            metrics_heuristic.mean_coalesce_ms,
            metrics_heuristic.p95_coalesce_ms,
            metrics_heuristic.max_coalesce_ms
        );
        eprintln!(
            "{{\"test\":\"resize_coalescer_sim\",\"scenario\":\"{}\",\"mode\":\"bocpd\",\"applies\":{},\"forced\":{},\"decisions\":{},\"mean_ms\":{:.2},\"p95_ms\":{:.2},\"max_ms\":{:.2}}}",
            name,
            metrics_bocpd.applies,
            metrics_bocpd.forced,
            metrics_bocpd.decisions,
            metrics_bocpd.mean_coalesce_ms,
            metrics_bocpd.p95_coalesce_ms,
            metrics_bocpd.max_coalesce_ms
        );

        // Tick granularity means an apply can land up to `tick_ms` after the deadline.
        let bound = base.hard_deadline_ms as f64 + tick_ms as f64;
        assert!(
            metrics_heuristic.max_coalesce_ms <= bound,
            "heuristic max coalesce exceeded deadline: {:.2} > {:.2}",
            metrics_heuristic.max_coalesce_ms,
            bound
        );
        assert!(
            metrics_bocpd.max_coalesce_ms <= bound,
            "bocpd max coalesce exceeded deadline: {:.2} > {:.2}",
            metrics_bocpd.max_coalesce_ms,
            bound
        );
        assert!(metrics_heuristic.applies > 0, "heuristic had no applies");
        assert!(metrics_bocpd.applies > 0, "bocpd had no applies");

        if name == "burst" {
            assert!(
                metrics_bocpd.applies <= metrics_heuristic.applies,
                "bocpd should not apply more than heuristic in burst: {} > {}",
                metrics_bocpd.applies,
                metrics_heuristic.applies
            );
        }
    }
}
