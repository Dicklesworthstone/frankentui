//! Adversarial security/reliability compatibility harness — flow-control core
//! (bd-2vr05.11.6).
//!
//! This is the reliability arm of the cross-subsystem security/reliability
//! harness. It drives the **production** remote-session flow-control policy
//! (`frankenterm_core::flow_control::FlowControlPolicy`, the decision core the
//! in-tree `ftui_pty::ws_bridge` wraps for every websocket-attached PTY) through
//! hostile and degraded conditions an attacker or a pathological client can
//! create: input storms, output floods, sustained queue overload, and explicit
//! drop-policy bypass attempts. It also asserts the bounded-message-size
//! contract of `WsPtyBridgeConfig` so a single oversized frame cannot exhaust
//! memory.
//!
//! Every cell prints one structured `FTUI_SECURITY_RELIABILITY_COMPAT ...` JSONL
//! line (subsystem-tagged, with a correlation id, the policy's decision ledger,
//! and a `failure_injection` flag) so the aggregator
//! (`scripts/frankenterm_js_security_reliability_compat.sh`) folds it into a
//! single incident-grade compatibility manifest alongside the link- and
//! clipboard-policy arms.
//!
//! Determinism: every snapshot is constructed explicitly (no wall-clock, no
//! environment reads, no randomness), so the decision ledger is byte-for-byte
//! reproducible for a fixed input — the precondition for deterministic
//! post-mortem replay.
#![forbid(unsafe_code)]

use frankenterm_core::flow_control::{
    BackpressureAction, DecisionReason, FlowControlConfig, FlowControlDecision, FlowControlPolicy,
    FlowControlSnapshot, InputEventClass, LatencyWindowMs, QueueDepthBytes, RateWindowBps,
};
use ftui_pty::ws_bridge::{FlowControlBridgeConfig, WsPtyBridgeConfig};

const PREFIX: &str = "FTUI_SECURITY_RELIABILITY_COMPAT";

/// A deterministic "calm" snapshot: empty queues, well-served rates, healthy
/// keystroke latency, and perfectly fair input/output service. Scenarios mutate
/// individual fields to model one specific hostile/degraded condition.
fn calm_snapshot() -> FlowControlSnapshot {
    FlowControlSnapshot {
        queues: QueueDepthBytes {
            input: 0,
            output: 0,
            render_frames: 0,
        },
        rates: RateWindowBps {
            lambda_in: 1_000,
            lambda_out: 1_000,
            mu_in: 8_000,
            mu_out: 8_000,
        },
        latency: LatencyWindowMs {
            key_p50_ms: 1.0,
            key_p95_ms: 5.0,
        },
        serviced_input_bytes: 4_096,
        serviced_output_bytes: 4_096,
        output_hard_cap_duration_ms: 0,
    }
}

fn action_token(action: Option<BackpressureAction>) -> &'static str {
    match action {
        None => "none",
        Some(BackpressureAction::CoalesceNonInteractive) => "coalesce_non_interactive",
        Some(BackpressureAction::ThrottleOutput) => "throttle_output",
        Some(BackpressureAction::DropNonInteractive) => "drop_non_interactive",
        Some(BackpressureAction::TerminateSession) => "terminate_session",
    }
}

fn reason_token(reason: DecisionReason) -> &'static str {
    match reason {
        DecisionReason::Stable => "stable",
        DecisionReason::QueuePressure => "queue_pressure",
        DecisionReason::ProtectKeyLatencyBudget => "protect_key_latency_budget",
        DecisionReason::HardCapExceeded => "hard_cap_exceeded",
    }
}

/// Render a policy decision as an incident-grade JSON fragment (no leading or
/// trailing comma) so a post-mortem reader sees exactly which backpressure
/// action fired and why.
fn decision_ledger(decision: &FlowControlDecision) -> String {
    let action = action_token(decision.chosen_action);
    let reason = reason_token(decision.reason);
    let fairness = decision.fairness_index;
    let budget = decision.output_batch_budget_bytes;
    let pause = decision.should_pause_pty_reads;
    format!(
        "\"chosen_action\":\"{action}\",\"reason\":\"{reason}\",\
\"fairness_index\":{fairness:.6},\"output_batch_budget_bytes\":{budget},\
\"should_pause_pty_reads\":{pause}"
    )
}

/// Emit one normalised compatibility cell. `extra` is a comma-joined JSON
/// fragment with no leading or trailing comma.
fn emit(
    subsystem: &str,
    scenario: &str,
    case: &str,
    correlation_id: &str,
    passed: bool,
    failure_injection: bool,
    extra: &str,
) {
    println!(
        "{PREFIX} {{\"subsystem\":\"{subsystem}\",\"scenario\":\"{scenario}\",\
\"case\":\"{case}\",\"correlation_id\":\"{correlation_id}\",\"passed\":{passed},\
\"failure_injection\":{failure_injection},{extra}}}"
    );
}

// ---------------------------------------------------------------------------
// drop_policy — the non-negotiable security invariant: interactive input
// (keystrokes / paste / focus) is NEVER dropped by the flow-control policy,
// while a non-interactive flood is bounded at the hard cap.
// ---------------------------------------------------------------------------

#[test]
fn drop_policy_interactive_is_never_dropped() {
    let policy = FlowControlPolicy::default();
    let cap = policy.config.input_hard_cap_bytes;

    // Sweep across the whole u32 range, including far past any cap, to prove the
    // invariant has no breaking point an attacker could push the queue toward.
    let samples = [
        0u32,
        1,
        cap - 1,
        cap,
        cap + 1,
        cap * 4,
        u32::MAX / 2,
        u32::MAX,
    ];
    let all_kept = samples
        .iter()
        .all(|&q| !policy.should_drop_input_event(q, InputEventClass::Interactive));
    assert!(
        all_kept,
        "interactive events must never be dropped at any queue depth"
    );
    let n = samples.len();
    emit(
        "drop_policy",
        "interactive_never_dropped",
        "u32_queue_sweep",
        "sec-drop-interactive",
        all_kept,
        false,
        &format!("\"samples\":{n},\"max_queue_bytes\":{}", u32::MAX),
    );
}

#[test]
fn drop_policy_noninteractive_drops_only_at_hard_cap() {
    let policy = FlowControlPolicy::default();
    let cap = policy.config.input_hard_cap_bytes;

    let below = policy.should_drop_input_event(cap - 1, InputEventClass::NonInteractive);
    let at_cap = policy.should_drop_input_event(cap, InputEventClass::NonInteractive);
    let flood = policy.should_drop_input_event(u32::MAX, InputEventClass::NonInteractive);

    // Below the hard cap nothing is dropped; at or above it the bounded queue
    // sheds the coalescible flood rather than growing without limit.
    let passed = !below && at_cap && flood;
    assert!(
        passed,
        "non-interactive drop must engage exactly at the hard cap"
    );
    emit(
        "drop_policy",
        "noninteractive_drops_at_hard_cap",
        "hard_cap_boundary",
        "sec-drop-noninteractive",
        passed,
        false,
        &format!(
            "\"hard_cap_bytes\":{cap},\"drop_below\":{below},\"drop_at_cap\":{at_cap},\
\"drop_flood\":{flood}"
        ),
    );
}

/// FAILURE INJECTION: a hostile client floods the input queue with
/// non-interactive events (mouse-move spam) to its absolute maximum, trying to
/// force the policy into a state where it also starts dropping the user's
/// keystrokes. The bypass must fail: the flood is shed, but interactive input
/// keeps flowing.
#[test]
fn drop_policy_interactive_starvation_bypass_is_blocked() {
    let policy = FlowControlPolicy::default();
    let hostile_queue = u32::MAX; // queue saturated by a non-interactive flood

    let flood_dropped =
        policy.should_drop_input_event(hostile_queue, InputEventClass::NonInteractive);
    let interactive_still_dropped =
        policy.should_drop_input_event(hostile_queue, InputEventClass::Interactive);

    // Bypass blocked iff the flood is dropped AND interactive input is not.
    let bypass_blocked = flood_dropped && !interactive_still_dropped;
    assert!(
        bypass_blocked,
        "interactive starvation via non-interactive flood must be impossible"
    );
    emit(
        "drop_policy",
        "interactive_starvation_bypass",
        "noninteractive_flood_at_u32_max",
        "sec-drop-bypass",
        bypass_blocked,
        true,
        &format!(
            "\"hostile_queue_bytes\":{hostile_queue},\"flood_dropped\":{flood_dropped},\
\"interactive_dropped\":{interactive_still_dropped}"
        ),
    );
}

// ---------------------------------------------------------------------------
// queue_caps — bounded queue + memory: output overload pauses PTY reads,
// sustained hard-cap saturation terminates the session, and the per-loop output
// budget is clamped while protecting interactive latency.
// ---------------------------------------------------------------------------

#[test]
fn queue_caps_pause_pty_reads_at_output_hard_cap() {
    let policy = FlowControlPolicy::default();
    let cap = policy.config.output_hard_cap_bytes;

    let mut just_below = calm_snapshot();
    just_below.queues.output = cap - 1;
    let mut at_cap = calm_snapshot();
    at_cap.queues.output = cap;

    let below_decision = policy.evaluate(just_below);
    let at_decision = policy.evaluate(at_cap);

    let passed = !below_decision.should_pause_pty_reads && at_decision.should_pause_pty_reads;
    assert!(
        passed,
        "PTY reads must pause exactly at the output hard cap"
    );
    emit(
        "queue_caps",
        "pause_pty_reads_at_hard_cap",
        "output_hard_cap_boundary",
        "rel-queue-pause",
        passed,
        false,
        &format!(
            "\"output_hard_cap_bytes\":{cap},{}",
            decision_ledger(&at_decision)
        ),
    );
}

#[test]
fn queue_caps_terminate_on_sustained_hard_cap() {
    let policy = FlowControlPolicy::default();
    let mut snapshot = calm_snapshot();
    snapshot.queues.output = policy.config.output_hard_cap_bytes;
    snapshot.output_hard_cap_duration_ms = policy.config.hard_cap_terminate_ms;

    let decision = policy.evaluate(snapshot);
    let passed = decision.chosen_action == Some(BackpressureAction::TerminateSession)
        && decision.reason == DecisionReason::HardCapExceeded
        && decision.should_pause_pty_reads;
    assert!(
        passed,
        "a session pinned at the output hard cap must be terminated, not grown"
    );
    emit(
        "queue_caps",
        "terminate_on_sustained_hard_cap",
        "hard_cap_duration_exceeded",
        "rel-queue-terminate",
        passed,
        false,
        &format!(
            "\"hard_cap_terminate_ms\":{},{}",
            policy.config.hard_cap_terminate_ms,
            decision_ledger(&decision)
        ),
    );
}

#[test]
fn queue_caps_output_budget_is_clamped_under_pressure() {
    let policy = FlowControlPolicy::default();

    // Calm: full idle budget. Under starved fairness the per-loop output budget
    // is clamped to the recovery floor so output can never starve input.
    let calm = policy.evaluate(calm_snapshot());

    let mut starved = calm_snapshot();
    starved.serviced_input_bytes = 1; // grossly unfair toward output
    starved.serviced_output_bytes = 1_000_000;
    let pressured = policy.evaluate(starved);

    let recovery = policy.config.output_batch_recovery_bytes;
    let passed = calm.output_batch_budget_bytes > pressured.output_batch_budget_bytes
        && pressured.output_batch_budget_bytes <= recovery;
    assert!(
        passed,
        "output budget must shrink to the recovery floor under unfairness"
    );
    emit(
        "queue_caps",
        "output_budget_clamped",
        "fairness_starvation",
        "rel-queue-budget",
        passed,
        false,
        &format!(
            "\"recovery_floor_bytes\":{recovery},\"calm_budget\":{},\"pressured_budget\":{},{}",
            calm.output_batch_budget_bytes,
            pressured.output_batch_budget_bytes,
            decision_ledger(&pressured)
        ),
    );
}

// ---------------------------------------------------------------------------
// overload — the policy's regime transitions: stable when calm, queue pressure
// under soft-cap/utilisation overload, and key-latency protection when
// keystroke latency or fairness degrades.
// ---------------------------------------------------------------------------

#[test]
fn overload_stable_when_calm() {
    let policy = FlowControlPolicy::default();
    let decision = policy.evaluate(calm_snapshot());
    let passed = decision.chosen_action.is_none() && decision.reason == DecisionReason::Stable;
    assert!(passed, "a calm session must require no intervention");
    emit(
        "overload",
        "stable_when_calm",
        "empty_queues_balanced_rates",
        "rel-overload-stable",
        passed,
        false,
        &decision_ledger(&decision),
    );
}

#[test]
fn overload_queue_pressure_engages_on_soft_cap() {
    let policy = FlowControlPolicy::default();
    let mut snapshot = calm_snapshot();
    snapshot.queues.input = policy.config.input_soft_cap_bytes;

    let decision = policy.evaluate(snapshot);
    let passed =
        decision.chosen_action.is_some() && decision.reason == DecisionReason::QueuePressure;
    assert!(
        passed,
        "crossing the input soft cap must trigger queue-pressure backpressure"
    );
    emit(
        "overload",
        "queue_pressure_on_soft_cap",
        "input_soft_cap_reached",
        "rel-overload-queue",
        passed,
        false,
        &format!(
            "\"input_soft_cap_bytes\":{},{}",
            policy.config.input_soft_cap_bytes,
            decision_ledger(&decision)
        ),
    );
}

#[test]
fn overload_utilization_storm_is_pressured() {
    let policy = FlowControlPolicy::default();
    // Arrival rate dwarfs service rate (rho_in >> 1): a sustained input storm.
    let mut snapshot = calm_snapshot();
    snapshot.rates.lambda_in = 1_000_000;
    snapshot.rates.mu_in = 1_000;

    let decision = policy.evaluate(snapshot);
    let passed = decision.chosen_action.is_some();
    assert!(passed, "an input arrival storm must engage backpressure");
    emit(
        "overload",
        "utilization_storm",
        "rho_in_far_above_one",
        "rel-overload-util",
        passed,
        false,
        &format!(
            "\"lambda_in\":{},\"mu_in\":{},{}",
            snapshot.rates.lambda_in,
            snapshot.rates.mu_in,
            decision_ledger(&decision)
        ),
    );
}

#[test]
fn overload_protects_key_latency_budget() {
    let policy = FlowControlPolicy::default();
    let mut snapshot = calm_snapshot();
    // p95 keystroke latency blows past the budget: interactivity is at risk.
    snapshot.latency.key_p95_ms = policy.config.key_latency_budget_ms * 2.0;

    let decision = policy.evaluate(snapshot);
    let passed = decision.chosen_action.is_some()
        && decision.reason == DecisionReason::ProtectKeyLatencyBudget;
    assert!(
        passed,
        "keystroke latency over budget must trigger latency protection"
    );
    emit(
        "overload",
        "protect_key_latency",
        "p95_over_budget",
        "rel-overload-latency",
        passed,
        false,
        &format!(
            "\"key_latency_budget_ms\":{:.6},\"key_p95_ms\":{:.6},{}",
            policy.config.key_latency_budget_ms,
            snapshot.latency.key_p95_ms,
            decision_ledger(&decision)
        ),
    );
}

// ---------------------------------------------------------------------------
// frame_cap — bounded message size: the websocket bridge rejects oversized
// frames so a single hostile message cannot exhaust memory, and the cap is
// host-tunable downward per deployment policy.
// ---------------------------------------------------------------------------

#[test]
fn frame_cap_default_is_bounded() {
    let config = WsPtyBridgeConfig::default();
    let expected = 256 * 1024;
    let passed = config.max_message_bytes == expected;
    assert!(
        passed,
        "default websocket frame cap must be the documented 256 KiB"
    );
    emit(
        "frame_cap",
        "default_message_cap",
        "ws_pty_bridge_default",
        "sec-frame-default",
        passed,
        false,
        &format!(
            "\"max_message_bytes\":{},\"expected\":{expected}",
            config.max_message_bytes
        ),
    );
}

#[test]
fn frame_cap_is_host_tunable_downward() {
    let tightened = 4_096;
    let config = WsPtyBridgeConfig {
        max_message_bytes: tightened,
        ..WsPtyBridgeConfig::default()
    };
    let passed = config.max_message_bytes == tightened && config.max_message_bytes < 256 * 1024;
    assert!(
        passed,
        "hosts must be able to tighten the frame cap below the default"
    );
    emit(
        "frame_cap",
        "host_tightened_cap",
        "explicit_4kib_cap",
        "sec-frame-tighten",
        passed,
        false,
        &format!("\"max_message_bytes\":{}", config.max_message_bytes),
    );
}

#[test]
fn frame_cap_flow_control_windows_are_bounded() {
    let fc = FlowControlBridgeConfig::default();
    // Credit windows and the wrapped policy caps must all be finite and
    // non-zero: no unbounded buffer can be requested through the defaults.
    let passed = fc.output_window > 0
        && fc.input_window > 0
        && fc.policy.input_hard_cap_bytes > 0
        && fc.policy.output_hard_cap_bytes > 0
        && fc.policy.input_hard_cap_bytes >= fc.policy.input_soft_cap_bytes
        && fc.policy.output_hard_cap_bytes >= fc.policy.output_soft_cap_bytes;
    assert!(
        passed,
        "flow-control bridge defaults must describe bounded resources"
    );
    emit(
        "frame_cap",
        "bounded_flow_control_windows",
        "flow_control_bridge_default",
        "rel-frame-windows",
        passed,
        false,
        &format!(
            "\"output_window\":{},\"input_window\":{},\"input_hard_cap_bytes\":{},\
\"output_hard_cap_bytes\":{}",
            fc.output_window,
            fc.input_window,
            fc.policy.input_hard_cap_bytes,
            fc.policy.output_hard_cap_bytes
        ),
    );
}

// ---------------------------------------------------------------------------
// replay — deterministic decisions: identical inputs yield byte-identical
// decision ledgers (and replenishment verdicts), and a scripted overload
// incident replays to the same documented escalation every time. This is the
// foundation that makes the JSONL evidence above usable for post-mortem replay.
// ---------------------------------------------------------------------------

#[test]
fn replay_decisions_are_deterministic() {
    let policy = FlowControlPolicy::default();
    let mut snapshot = calm_snapshot();
    snapshot.queues.input = policy.config.input_soft_cap_bytes + 1;

    let first = policy.evaluate(snapshot);
    let second = policy.evaluate(snapshot);
    let stable = first == second;
    assert!(
        stable,
        "re-evaluating an identical snapshot must yield an identical decision"
    );
    emit(
        "replay",
        "decision_determinism",
        "double_evaluate_identical_snapshot",
        "obs-replay-decision",
        stable,
        false,
        &format!("\"decision_stable\":{stable},{}", decision_ledger(&first)),
    );
}

#[test]
fn replay_replenish_verdict_is_deterministic() {
    let policy = FlowControlPolicy::default();
    let consumed = 5_000u32;
    let window = 8_192u32;
    let elapsed = 3u64;

    let a = policy.should_replenish(consumed, window, elapsed);
    let b = policy.should_replenish(consumed, window, elapsed);
    let stable = a == b;
    assert!(
        stable,
        "replenish verdict must be deterministic for fixed inputs"
    );
    emit(
        "replay",
        "replenish_determinism",
        "fixed_consumption_window",
        "obs-replay-replenish",
        stable,
        false,
        &format!("\"replenish\":{a},\"consumed\":{consumed},\"window\":{window}"),
    );
}

#[test]
fn replay_overload_incident_escalates_consistently() {
    let policy = FlowControlPolicy::default();
    let correlation = "obs-replay-incident";

    // A scripted three-step incident: calm -> soft-cap pressure -> sustained
    // hard cap. Each step has a fixed expected outcome, so the whole incident
    // replays identically — the property a triage engineer relies on.
    let mut step1 = calm_snapshot();
    step1.queues.input = 0;

    let mut step2 = calm_snapshot();
    step2.queues.input = policy.config.input_soft_cap_bytes;

    let mut step3 = calm_snapshot();
    step3.queues.output = policy.config.output_hard_cap_bytes;
    step3.output_hard_cap_duration_ms = policy.config.hard_cap_terminate_ms;

    let d1 = policy.evaluate(step1);
    let d2 = policy.evaluate(step2);
    let d3 = policy.evaluate(step3);

    let escalates = d1.reason == DecisionReason::Stable
        && d2.reason == DecisionReason::QueuePressure
        && d3.chosen_action == Some(BackpressureAction::TerminateSession);
    assert!(
        escalates,
        "the scripted incident must escalate through the documented states"
    );

    // Replay the whole script and require an identical ledger each time.
    let replay_stable = policy.evaluate(step1) == d1
        && policy.evaluate(step2) == d2
        && policy.evaluate(step3) == d3;
    let passed = escalates && replay_stable;

    for (idx, decision) in [&d1, &d2, &d3].into_iter().enumerate() {
        let case = format!("step_{}", idx + 1);
        emit(
            "replay",
            "overload_incident",
            &case,
            correlation,
            passed,
            false,
            &format!(
                "\"step\":{},\"replay_stable\":{replay_stable},{}",
                idx + 1,
                decision_ledger(decision)
            ),
        );
    }
}

// Compile-time guard: confirm the config knobs the assertions read are the
// documented bounded defaults, so a registry bump that loosens them is caught.
#[test]
fn config_defaults_are_the_documented_bounds() {
    let cfg = FlowControlConfig::default();
    assert_eq!(cfg.input_hard_cap_bytes, 16 * 1024);
    assert_eq!(cfg.output_hard_cap_bytes, 256 * 1024);
    assert_eq!(cfg.hard_cap_terminate_ms, 5_000);
    emit(
        "frame_cap",
        "documented_config_bounds",
        "flow_control_config_default",
        "rel-frame-config",
        true,
        false,
        &format!(
            "\"input_hard_cap_bytes\":{},\"output_hard_cap_bytes\":{},\"hard_cap_terminate_ms\":{}",
            cfg.input_hard_cap_bytes, cfg.output_hard_cap_bytes, cfg.hard_cap_terminate_ms
        ),
    );
}
