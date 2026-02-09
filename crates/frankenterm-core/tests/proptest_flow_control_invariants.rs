//! Property-based invariant tests for the flow-control policy engine.
//!
//! Verifies structural guarantees that must hold for any valid input snapshot:
//!
//! 1. Determinism: identical snapshots produce identical decisions.
//! 2. Jain fairness index is always in [0.5, 1.0] for two positive streams.
//! 3. Interactive input events are never dropped regardless of queue depth.
//! 4. Hard-cap duration override always produces TerminateSession.
//! 5. Stable decision when no pressure signals active.
//! 6. Output batch recovery budget <= normal budget.
//! 7. should_pause_pty_reads iff output >= output_hard_cap.
//! 8. All expected_loss values are non-negative.
//! 9. TerminateSession has highest tie-break rank (least preferred on tie).
//! 10. Evaluate never panics on any valid snapshot.

use frankenterm_core::{
    BackpressureAction, DecisionReason, FlowControlPolicy, FlowControlSnapshot,
    InputEventClass, LatencyWindowMs, QueueDepthBytes, RateWindowBps, jain_fairness_index,
};
use proptest::prelude::*;

// ── Strategy helpers ──────────────────────────────────────────────────

fn arb_queue_depth() -> impl Strategy<Value = QueueDepthBytes> {
    (0u32..=512_000, 0u32..=512_000, 0u8..=10).prop_map(|(input, output, render_frames)| {
        QueueDepthBytes {
            input,
            output,
            render_frames,
        }
    })
}

fn arb_rate_window() -> impl Strategy<Value = RateWindowBps> {
    (0u32..=2_000_000, 0u32..=2_000_000, 1u32..=2_000_000, 1u32..=2_000_000).prop_map(
        |(lambda_in, lambda_out, mu_in, mu_out)| RateWindowBps {
            lambda_in,
            lambda_out,
            mu_in,
            mu_out,
        },
    )
}

fn arb_latency() -> impl Strategy<Value = LatencyWindowMs> {
    (0.0f64..500.0, 0.0f64..500.0).prop_map(|(p50, p95)| LatencyWindowMs {
        key_p50_ms: p50.min(p95),
        key_p95_ms: p50.max(p95),
    })
}

fn arb_snapshot() -> impl Strategy<Value = FlowControlSnapshot> {
    (
        arb_queue_depth(),
        arb_rate_window(),
        arb_latency(),
        0u64..=200_000,
        0u64..=200_000,
        0u64..=10_000,
    )
        .prop_map(
            |(queues, rates, latency, svc_in, svc_out, hard_cap_dur)| FlowControlSnapshot {
                queues,
                rates,
                latency,
                serviced_input_bytes: svc_in,
                serviced_output_bytes: svc_out,
                output_hard_cap_duration_ms: hard_cap_dur,
            },
        )
}

fn default_policy() -> FlowControlPolicy {
    FlowControlPolicy::default()
}

// ═════════════════════════════════════════════════════════════════════════
// 1. Determinism: same snapshot → same decision
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn evaluate_is_deterministic(snapshot in arb_snapshot()) {
        let policy = default_policy();
        let d1 = policy.evaluate(snapshot);
        let d2 = policy.evaluate(snapshot);
        prop_assert_eq!(d1.chosen_action, d2.chosen_action);
        prop_assert_eq!(d1.reason, d2.reason);
        prop_assert_eq!(d1.output_batch_budget_bytes, d2.output_batch_budget_bytes);
        prop_assert_eq!(d1.should_pause_pty_reads, d2.should_pause_pty_reads);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. Jain fairness index bounds
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn jain_fairness_bounded(a in 0u64..=1_000_000, b in 0u64..=1_000_000) {
        let f = jain_fairness_index(a, b);
        prop_assert!(f >= 0.5 - 1e-9, "fairness {} < 0.5 for ({}, {})", f, a, b);
        prop_assert!(f <= 1.0 + 1e-9, "fairness {} > 1.0 for ({}, {})", f, a, b);
    }

    #[test]
    fn jain_fairness_symmetric(a in 0u64..=1_000_000, b in 0u64..=1_000_000) {
        let f1 = jain_fairness_index(a, b);
        let f2 = jain_fairness_index(b, a);
        prop_assert!(
            (f1 - f2).abs() < 1e-12,
            "fairness not symmetric: f({},{})={} vs f({},{})={}",
            a, b, f1, b, a, f2
        );
    }

    #[test]
    fn jain_fairness_equal_is_one(v in 0u64..=1_000_000) {
        let f = jain_fairness_index(v, v);
        prop_assert!(
            (f - 1.0).abs() < 1e-9,
            "fairness({},{}) = {} should be 1.0", v, v, f
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. Interactive events never dropped
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn interactive_never_dropped(queue_bytes in 0u32..=1_000_000) {
        let policy = default_policy();
        prop_assert!(
            !policy.should_drop_input_event(queue_bytes, InputEventClass::Interactive),
            "interactive event dropped at queue_bytes={}",
            queue_bytes
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 4. Hard-cap duration override → TerminateSession
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn hard_cap_forces_terminate(
        snapshot in arb_snapshot(),
        extra_ms in 0u64..=10_000,
    ) {
        let policy = default_policy();
        let mut s = snapshot;
        s.output_hard_cap_duration_ms = policy.config.hard_cap_terminate_ms + extra_ms;
        let decision = policy.evaluate(s);
        prop_assert_eq!(
            decision.chosen_action,
            Some(BackpressureAction::TerminateSession),
            "hard cap exceeded but action was {:?}",
            decision.chosen_action
        );
        prop_assert_eq!(decision.reason, DecisionReason::HardCapExceeded);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 5. Stable when no pressure
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn stable_when_low_pressure(
        in_bytes in 0u32..=100,
        out_bytes in 0u32..=1000,
    ) {
        let policy = default_policy();
        let snapshot = FlowControlSnapshot {
            queues: QueueDepthBytes {
                input: in_bytes,
                output: out_bytes,
                render_frames: 0,
            },
            rates: RateWindowBps {
                lambda_in: 100,
                lambda_out: 1_000,
                mu_in: 10_000,
                mu_out: 100_000,
            },
            latency: LatencyWindowMs {
                key_p50_ms: 1.0,
                key_p95_ms: 5.0,
            },
            serviced_input_bytes: 50_000,
            serviced_output_bytes: 50_000,
            output_hard_cap_duration_ms: 0,
        };
        let decision = policy.evaluate(snapshot);
        prop_assert_eq!(
            decision.chosen_action, None,
            "expected Stable for low-pressure snapshot, got {:?}",
            decision.chosen_action
        );
        prop_assert_eq!(decision.reason, DecisionReason::Stable);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 6. Recovery budget <= normal budget
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn recovery_budget_le_normal() {
    let policy = default_policy();
    let normal_idle = policy.config.output_batch_idle_bytes;
    let normal_with = policy.config.output_batch_with_input_bytes;
    let recovery = policy.config.output_batch_recovery_bytes;
    assert!(
        recovery <= normal_idle,
        "recovery {} > idle {}",
        recovery,
        normal_idle
    );
    assert!(
        recovery <= normal_with,
        "recovery {} > with_input {}",
        recovery,
        normal_with
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 7. should_pause_pty_reads iff output >= hard cap
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn pause_pty_reads_iff_hard_cap(snapshot in arb_snapshot()) {
        let policy = default_policy();
        let decision = policy.evaluate(snapshot);
        let at_hard_cap = snapshot.queues.output >= policy.config.output_hard_cap_bytes;
        prop_assert_eq!(
            decision.should_pause_pty_reads, at_hard_cap,
            "should_pause={} but output={} vs hard_cap={}",
            decision.should_pause_pty_reads,
            snapshot.queues.output,
            policy.config.output_hard_cap_bytes
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 8. All expected_loss values are non-negative
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn loss_values_non_negative(snapshot in arb_snapshot()) {
        let policy = default_policy();
        let decision = policy.evaluate(snapshot);
        for loss in &decision.losses {
            prop_assert!(
                loss.expected_loss >= 0.0,
                "negative expected_loss {:.4} for {:?}",
                loss.expected_loss,
                loss.action
            );
            prop_assert!(
                loss.oom_risk >= 0.0,
                "negative oom_risk {:.4} for {:?}",
                loss.oom_risk,
                loss.action
            );
            prop_assert!(
                loss.latency_risk >= 0.0,
                "negative latency_risk {:.4} for {:?}",
                loss.latency_risk,
                loss.action
            );
            prop_assert!(
                loss.throughput_loss >= 0.0,
                "negative throughput_loss {:.4} for {:?}",
                loss.throughput_loss,
                loss.action
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 9. TerminateSession always has highest throughput cost
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn terminate_has_highest_throughput_cost(snapshot in arb_snapshot()) {
        let policy = default_policy();
        let decision = policy.evaluate(snapshot);
        let terminate_loss = decision
            .losses
            .iter()
            .find(|l| l.action == BackpressureAction::TerminateSession)
            .unwrap();
        for other in &decision.losses {
            if other.action != BackpressureAction::TerminateSession {
                prop_assert!(
                    terminate_loss.throughput_loss >= other.throughput_loss,
                    "TerminateSession throughput_loss ({:.2}) < {:?} ({:.2})",
                    terminate_loss.throughput_loss,
                    other.action,
                    other.throughput_loss
                );
            }
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 10. Evaluate never panics
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn evaluate_never_panics(snapshot in arb_snapshot()) {
        let policy = default_policy();
        let _decision = policy.evaluate(snapshot);
        // If we get here, no panic occurred
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 11. Non-interactive events are droppable above hard cap
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn non_interactive_droppable_above_hard_cap(extra in 0u32..=100_000) {
        let policy = default_policy();
        let queue = policy.config.input_hard_cap_bytes.saturating_add(extra);
        prop_assert!(
            policy.should_drop_input_event(queue, InputEventClass::NonInteractive),
            "non-interactive not dropped at {} >= hard_cap {}",
            queue,
            policy.config.input_hard_cap_bytes
        );
    }

    #[test]
    fn non_interactive_kept_below_hard_cap(queue in 0u32..16_384) {
        let policy = default_policy();
        if queue < policy.config.input_hard_cap_bytes {
            prop_assert!(
                !policy.should_drop_input_event(queue, InputEventClass::NonInteractive),
                "non-interactive dropped at {} < hard_cap {}",
                queue,
                policy.config.input_hard_cap_bytes
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 12. Replenish logic
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn replenish_when_window_zero(consumed in any::<u32>(), elapsed in any::<u64>()) {
        let policy = default_policy();
        prop_assert!(
            policy.should_replenish(consumed, 0, elapsed),
            "should replenish when window_bytes=0"
        );
    }

    #[test]
    fn replenish_when_interval_exceeded(
        consumed in 0u32..=100,
        window in 1u32..=10_000,
    ) {
        let policy = default_policy();
        let elapsed = policy.config.replenish_interval_ms;
        prop_assert!(
            policy.should_replenish(consumed, window, elapsed),
            "should replenish when elapsed >= interval"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 13. Decision action is from the losses array
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn chosen_action_in_losses(snapshot in arb_snapshot()) {
        let policy = default_policy();
        let decision = policy.evaluate(snapshot);
        if let Some(action) = decision.chosen_action {
            prop_assert!(
                decision.losses.iter().any(|l| l.action == action),
                "chosen action {:?} not found in losses array",
                action
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 14. Fairness index in decision matches snapshot computation
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn decision_fairness_matches_snapshot(snapshot in arb_snapshot()) {
        let policy = default_policy();
        let decision = policy.evaluate(snapshot);
        let expected = snapshot.fairness_index();
        prop_assert!(
            (decision.fairness_index - expected).abs() < 1e-12,
            "decision fairness {} != snapshot fairness {}",
            decision.fairness_index,
            expected
        );
    }
}
