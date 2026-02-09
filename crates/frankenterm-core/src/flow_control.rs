//! Deterministic flow-control and backpressure policy for FrankenTerm remote I/O.
//!
//! This module encodes the queueing and fairness policy described in
//! `docs/spec/frankenterm-websocket-protocol.md` section 4 so remote transport
//! implementations can share one deterministic decision engine.

use core::cmp::Ordering;

const KIB: u32 = 1024;
const ACTION_COUNT: usize = 4;

/// Cost weights for asymmetric backpressure loss.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LossWeights {
    /// Catastrophic memory growth risk.
    pub oom: f64,
    /// Keystroke latency risk.
    pub latency: f64,
    /// Throughput degradation cost.
    pub throughput: f64,
}

impl Default for LossWeights {
    fn default() -> Self {
        Self {
            oom: 1_000_000.0,
            latency: 10_000.0,
            throughput: 100.0,
        }
    }
}

/// Runtime policy configuration for flow control decisions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlowControlConfig {
    /// Input queue soft cap.
    pub input_soft_cap_bytes: u32,
    /// Input queue hard cap.
    pub input_hard_cap_bytes: u32,
    /// Output queue soft cap.
    pub output_soft_cap_bytes: u32,
    /// Output queue hard cap.
    pub output_hard_cap_bytes: u32,
    /// Fairness lower bound (Jain index).
    pub fairness_floor: f64,
    /// Keystroke p95 latency budget.
    pub key_latency_budget_ms: f64,
    /// Output batch when input queue is non-empty.
    pub output_batch_with_input_bytes: u32,
    /// Output batch when input queue is empty.
    pub output_batch_idle_bytes: u32,
    /// Output batch while recovering fairness/latency.
    pub output_batch_recovery_bytes: u32,
    /// Trigger window-based replenish at this elapsed interval.
    pub replenish_interval_ms: u64,
    /// Terminate if output queue stays at hard cap longer than this.
    pub hard_cap_terminate_ms: u64,
    /// Cost assigned to hard disconnect (`terminate_session`).
    pub terminate_throughput_loss: f64,
    /// Loss function weights.
    pub weights: LossWeights,
}

impl Default for FlowControlConfig {
    fn default() -> Self {
        Self {
            input_soft_cap_bytes: 12 * KIB,
            input_hard_cap_bytes: 16 * KIB,
            output_soft_cap_bytes: 192 * KIB,
            output_hard_cap_bytes: 256 * KIB,
            fairness_floor: 0.80,
            key_latency_budget_ms: 50.0,
            output_batch_with_input_bytes: 32 * KIB,
            output_batch_idle_bytes: 64 * KIB,
            output_batch_recovery_bytes: 8 * KIB,
            replenish_interval_ms: 10,
            hard_cap_terminate_ms: 5_000,
            terminate_throughput_loss: 6_000.0,
            weights: LossWeights::default(),
        }
    }
}

/// Queue depths used by the policy at decision time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueDepthBytes {
    /// Input bytes waiting for PTY write.
    pub input: u32,
    /// Output bytes waiting for websocket send.
    pub output: u32,
    /// Client render queue depth in frames.
    pub render_frames: u8,
}

/// Sliding-window throughput rates (bytes/sec).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateWindowBps {
    /// Arrival rate into input queue.
    pub lambda_in: u32,
    /// Arrival rate into output queue.
    pub lambda_out: u32,
    /// Service rate for PTY writes.
    pub mu_in: u32,
    /// Service rate for websocket output.
    pub mu_out: u32,
}

/// Keystroke latency summary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatencyWindowMs {
    /// p50 key latency (ms).
    pub key_p50_ms: f64,
    /// p95 key latency (ms).
    pub key_p95_ms: f64,
}

/// Deterministic snapshot consumed by the policy evaluator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlowControlSnapshot {
    /// Current queue depths.
    pub queues: QueueDepthBytes,
    /// Sliding-window rates.
    pub rates: RateWindowBps,
    /// Sliding-window latency summary.
    pub latency: LatencyWindowMs,
    /// Bytes serviced from input lane in the fairness window.
    pub serviced_input_bytes: u64,
    /// Bytes serviced from output lane in the fairness window.
    pub serviced_output_bytes: u64,
    /// How long output queue has continuously stayed at hard cap.
    pub output_hard_cap_duration_ms: u64,
}

impl FlowControlSnapshot {
    /// Jain fairness index over serviced input/output bytes in this window.
    #[must_use]
    pub fn fairness_index(self) -> f64 {
        jain_fairness_index(self.serviced_input_bytes, self.serviced_output_bytes)
    }
}

/// Candidate backpressure action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackpressureAction {
    /// Coalesce bursty non-interactive events.
    CoalesceNonInteractive,
    /// Throttle output emission/production.
    ThrottleOutput,
    /// Drop non-interactive input events.
    DropNonInteractive,
    /// Hard-stop the session to prevent unbounded growth.
    TerminateSession,
}

impl BackpressureAction {
    #[must_use]
    const fn tie_break_rank(self) -> u8 {
        match self {
            Self::CoalesceNonInteractive => 0,
            Self::ThrottleOutput => 1,
            Self::DropNonInteractive => 2,
            Self::TerminateSession => 3,
        }
    }
}

/// Scored loss entry for one backpressure action.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ActionLoss {
    /// Candidate action.
    pub action: BackpressureAction,
    /// Estimated expected loss.
    pub expected_loss: f64,
    /// Estimated OOM probability.
    pub oom_risk: f64,
    /// Estimated latency-budget violation probability.
    pub latency_risk: f64,
    /// Estimated throughput-loss term.
    pub throughput_loss: f64,
}

/// Reason code for the chosen policy outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionReason {
    /// No intervention needed.
    Stable,
    /// Queue pressure/rates require intervention.
    QueuePressure,
    /// Fairness or key latency breached budget.
    ProtectKeyLatencyBudget,
    /// Output hard-cap sustained for too long.
    HardCapExceeded,
}

/// Evaluated policy decision.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlowControlDecision {
    /// Selected action; `None` means maintain current behavior.
    pub chosen_action: Option<BackpressureAction>,
    /// Deterministic reason code for the decision.
    pub reason: DecisionReason,
    /// Jain fairness index for the current decision window.
    pub fairness_index: f64,
    /// Output-service budget to apply this loop.
    pub output_batch_budget_bytes: u32,
    /// Whether PTY reads should be paused due output hard-cap.
    pub should_pause_pty_reads: bool,
    /// Loss estimates for all candidate actions.
    pub losses: [ActionLoss; ACTION_COUNT],
}

/// Input event class for drop policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEventClass {
    /// Keystrokes/paste/focus transitions (must not drop).
    Interactive,
    /// Mouse move/drag and other coalescible signals.
    NonInteractive,
}

/// Deterministic policy evaluator for websocket remote flow control.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlowControlPolicy {
    /// Runtime policy configuration.
    pub config: FlowControlConfig,
}

impl Default for FlowControlPolicy {
    fn default() -> Self {
        Self::new(FlowControlConfig::default())
    }
}

impl FlowControlPolicy {
    /// Construct a policy with explicit configuration.
    #[must_use]
    pub const fn new(config: FlowControlConfig) -> Self {
        Self { config }
    }

    /// Evaluate one deterministic backpressure decision.
    #[must_use]
    pub fn evaluate(self, snapshot: FlowControlSnapshot) -> FlowControlDecision {
        let fairness_index = snapshot.fairness_index();
        let losses = self.score_actions(snapshot, fairness_index);
        let chosen_action = self.choose_action(snapshot, fairness_index, &losses);
        let reason = self.reason(snapshot, fairness_index, chosen_action);
        let output_batch_budget_bytes = self.output_batch_budget(
            snapshot.queues.input,
            fairness_index,
            snapshot.latency.key_p95_ms,
        );
        let should_pause_pty_reads = snapshot.queues.output >= self.config.output_hard_cap_bytes;
        FlowControlDecision {
            chosen_action,
            reason,
            fairness_index,
            output_batch_budget_bytes,
            should_pause_pty_reads,
            losses,
        }
    }

    /// Replenish flow-control window at 50% consumption or interval timeout.
    #[must_use]
    pub fn should_replenish(self, consumed_bytes: u32, window_bytes: u32, elapsed_ms: u64) -> bool {
        if window_bytes == 0 {
            return true;
        }
        consumed_bytes.saturating_mul(2) >= window_bytes
            || elapsed_ms >= self.config.replenish_interval_ms
    }

    /// Non-negotiable input drop rule: only non-interactive events are droppable.
    #[must_use]
    pub fn should_drop_input_event(self, queue_bytes: u32, class: InputEventClass) -> bool {
        match class {
            InputEventClass::Interactive => false,
            InputEventClass::NonInteractive => queue_bytes >= self.config.input_hard_cap_bytes,
        }
    }

    #[must_use]
    fn output_batch_budget(
        self,
        input_queue_bytes: u32,
        fairness_index: f64,
        key_p95_ms: f64,
    ) -> u32 {
        let baseline = if input_queue_bytes > 0 {
            self.config.output_batch_with_input_bytes
        } else {
            self.config.output_batch_idle_bytes
        };
        if fairness_index < self.config.fairness_floor
            || key_p95_ms > self.config.key_latency_budget_ms
        {
            baseline.min(self.config.output_batch_recovery_bytes)
        } else {
            baseline
        }
    }

    #[must_use]
    fn reason(
        self,
        snapshot: FlowControlSnapshot,
        fairness_index: f64,
        chosen_action: Option<BackpressureAction>,
    ) -> DecisionReason {
        if snapshot.output_hard_cap_duration_ms >= self.config.hard_cap_terminate_ms {
            return DecisionReason::HardCapExceeded;
        }
        if chosen_action.is_none() {
            return DecisionReason::Stable;
        }
        if fairness_index < self.config.fairness_floor
            || snapshot.latency.key_p95_ms > self.config.key_latency_budget_ms
        {
            DecisionReason::ProtectKeyLatencyBudget
        } else {
            DecisionReason::QueuePressure
        }
    }

    #[must_use]
    fn choose_action(
        self,
        snapshot: FlowControlSnapshot,
        fairness_index: f64,
        losses: &[ActionLoss; ACTION_COUNT],
    ) -> Option<BackpressureAction> {
        if snapshot.output_hard_cap_duration_ms >= self.config.hard_cap_terminate_ms {
            return Some(BackpressureAction::TerminateSession);
        }
        if !self.is_pressured(snapshot, fairness_index) {
            return None;
        }
        Some(select_best_action(losses))
    }

    #[must_use]
    fn is_pressured(self, snapshot: FlowControlSnapshot, fairness_index: f64) -> bool {
        let input_soft = snapshot.queues.input >= self.config.input_soft_cap_bytes;
        let output_soft = snapshot.queues.output >= self.config.output_soft_cap_bytes;
        let rho_in = ratio_u32(snapshot.rates.lambda_in, snapshot.rates.mu_in);
        let rho_out = ratio_u32(snapshot.rates.lambda_out, snapshot.rates.mu_out);
        input_soft
            || output_soft
            || rho_in > 1.0
            || rho_out > 1.0
            || fairness_index < self.config.fairness_floor
            || snapshot.latency.key_p95_ms > self.config.key_latency_budget_ms
    }

    #[must_use]
    fn score_actions(
        self,
        snapshot: FlowControlSnapshot,
        fairness_index: f64,
    ) -> [ActionLoss; ACTION_COUNT] {
        let signals = self.pressure_signals(snapshot, fairness_index);
        let actions = [
            BackpressureAction::CoalesceNonInteractive,
            BackpressureAction::ThrottleOutput,
            BackpressureAction::DropNonInteractive,
            BackpressureAction::TerminateSession,
        ];
        actions.map(|action| self.score_action(action, signals))
    }

    #[must_use]
    fn score_action(self, action: BackpressureAction, signals: PressureSignals) -> ActionLoss {
        let (oom_risk, latency_risk, throughput_loss) = match action {
            BackpressureAction::CoalesceNonInteractive => (
                0.35 * signals.oom_signal.powi(3),
                0.50 * signals.latency_signal.powi(2),
                0.08 + 0.18 * signals.throughput_signal,
            ),
            BackpressureAction::ThrottleOutput => (
                0.22 * signals.oom_signal.powi(3),
                0.28 * signals.latency_signal.powi(2),
                0.24 + 0.32 * signals.throughput_signal,
            ),
            BackpressureAction::DropNonInteractive => (
                0.15 * signals.oom_signal.powi(3),
                0.20 * signals.latency_signal.powi(2),
                0.42 + 0.45 * signals.throughput_signal,
            ),
            BackpressureAction::TerminateSession => {
                (0.0, 0.0, self.config.terminate_throughput_loss)
            }
        };
        let expected_loss = (self.config.weights.oom * oom_risk)
            + (self.config.weights.latency * latency_risk)
            + (self.config.weights.throughput * throughput_loss);
        ActionLoss {
            action,
            expected_loss,
            oom_risk,
            latency_risk,
            throughput_loss,
        }
    }

    #[must_use]
    fn pressure_signals(
        self,
        snapshot: FlowControlSnapshot,
        fairness_index: f64,
    ) -> PressureSignals {
        let out_hard_ratio = ratio_u32(snapshot.queues.output, self.config.output_hard_cap_bytes);
        let in_hard_ratio = ratio_u32(snapshot.queues.input, self.config.input_hard_cap_bytes);
        let out_soft_ratio = ratio_u32(snapshot.queues.output, self.config.output_soft_cap_bytes);
        let rho_in = ratio_u32(snapshot.rates.lambda_in, snapshot.rates.mu_in);
        let rho_out = ratio_u32(snapshot.rates.lambda_out, snapshot.rates.mu_out);

        let queue_pressure = out_hard_ratio.max(in_hard_ratio);
        let util_pressure = ((rho_in.max(rho_out) - 1.0).max(0.0) / 0.5).min(1.0);
        let oom_signal = clamp01(((queue_pressure - 0.70).max(0.0) / 0.30).max(util_pressure));

        let latency_over_budget =
            (snapshot.latency.key_p95_ms / self.config.key_latency_budget_ms - 1.0).max(0.0);
        let fairness_shortfall = if fairness_index < self.config.fairness_floor {
            (self.config.fairness_floor - fairness_index) / self.config.fairness_floor
        } else {
            0.0
        };
        let latency_signal = clamp01(
            latency_over_budget
                + fairness_shortfall
                + ((rho_in - 1.0).max(0.0))
                + (ratio_u32(snapshot.queues.input, self.config.input_soft_cap_bytes) - 1.0)
                    .max(0.0),
        );

        let throughput_signal = clamp01((rho_out - 1.0).max(0.0) + (out_soft_ratio - 1.0).max(0.0));
        PressureSignals {
            oom_signal,
            latency_signal,
            throughput_signal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PressureSignals {
    oom_signal: f64,
    latency_signal: f64,
    throughput_signal: f64,
}

#[must_use]
fn select_best_action(losses: &[ActionLoss; ACTION_COUNT]) -> BackpressureAction {
    let mut best = losses[0];
    for candidate in losses.iter().copied().skip(1) {
        let ordering = candidate.expected_loss.total_cmp(&best.expected_loss);
        if ordering == Ordering::Less
            || (ordering == Ordering::Equal
                && candidate.action.tie_break_rank() < best.action.tie_break_rank())
        {
            best = candidate;
        }
    }
    best.action
}

#[must_use]
fn ratio_u32(numerator: u32, denominator: u32) -> f64 {
    if denominator == 0 {
        return f64::INFINITY;
    }
    f64::from(numerator) / f64::from(denominator)
}

#[must_use]
fn clamp01(value: f64) -> f64 {
    value.clamp(0.0, 1.0)
}

/// Jain fairness index over two serviced-byte streams.
#[must_use]
pub fn jain_fairness_index(serviced_input_bytes: u64, serviced_output_bytes: u64) -> f64 {
    let input = serviced_input_bytes as f64;
    let output = serviced_output_bytes as f64;
    let denominator = 2.0 * (input * input + output * output);
    if denominator <= f64::EPSILON {
        return 1.0;
    }
    ((input + output) * (input + output)) / denominator
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jain_fairness_index_matches_expected_limits() {
        assert_close(jain_fairness_index(0, 0), 1.0, 1e-9);
        assert_close(jain_fairness_index(100, 100), 1.0, 1e-9);
        assert_close(jain_fairness_index(100, 0), 0.5, 1e-9);
    }

    #[test]
    fn output_batch_budget_respects_fairness_and_latency() {
        let policy = FlowControlPolicy::default();
        let baseline = policy.output_batch_budget(0, 0.95, 10.0);
        assert_eq!(baseline, 64 * KIB);

        let with_input = policy.output_batch_budget(1, 0.95, 10.0);
        assert_eq!(with_input, 32 * KIB);

        let fairness_recovery = policy.output_batch_budget(1, 0.60, 10.0);
        assert_eq!(fairness_recovery, 8 * KIB);

        let latency_recovery = policy.output_batch_budget(0, 0.95, 120.0);
        assert_eq!(latency_recovery, 8 * KIB);
    }

    #[test]
    fn stable_snapshot_emits_no_action() {
        let policy = FlowControlPolicy::default();
        let snapshot = FlowControlSnapshot {
            queues: QueueDepthBytes {
                input: 1024,
                output: 4096,
                render_frames: 0,
            },
            rates: RateWindowBps {
                lambda_in: 1_000,
                lambda_out: 20_000,
                mu_in: 10_000,
                mu_out: 100_000,
            },
            latency: LatencyWindowMs {
                key_p50_ms: 2.0,
                key_p95_ms: 8.0,
            },
            serviced_input_bytes: 40_000,
            serviced_output_bytes: 42_000,
            output_hard_cap_duration_ms: 0,
        };
        let decision = policy.evaluate(snapshot);
        assert_eq!(decision.chosen_action, None);
        assert_eq!(decision.reason, DecisionReason::Stable);
        assert_eq!(decision.output_batch_budget_bytes, 32 * KIB);
    }

    #[test]
    fn tie_break_order_is_deterministic() {
        let losses = [
            ActionLoss {
                action: BackpressureAction::CoalesceNonInteractive,
                expected_loss: 10.0,
                oom_risk: 0.0,
                latency_risk: 0.0,
                throughput_loss: 0.0,
            },
            ActionLoss {
                action: BackpressureAction::ThrottleOutput,
                expected_loss: 10.0,
                oom_risk: 0.0,
                latency_risk: 0.0,
                throughput_loss: 0.0,
            },
            ActionLoss {
                action: BackpressureAction::DropNonInteractive,
                expected_loss: 10.0,
                oom_risk: 0.0,
                latency_risk: 0.0,
                throughput_loss: 0.0,
            },
            ActionLoss {
                action: BackpressureAction::TerminateSession,
                expected_loss: 10.0,
                oom_risk: 0.0,
                latency_risk: 0.0,
                throughput_loss: 0.0,
            },
        ];
        assert_eq!(
            select_best_action(&losses),
            BackpressureAction::CoalesceNonInteractive
        );
    }

    #[test]
    fn hard_cap_duration_forces_terminate() {
        let policy = FlowControlPolicy::default();
        let snapshot = FlowControlSnapshot {
            queues: QueueDepthBytes {
                input: 0,
                output: policy.config.output_hard_cap_bytes,
                render_frames: 1,
            },
            rates: RateWindowBps {
                lambda_in: 0,
                lambda_out: 1_000_000,
                mu_in: 1,
                mu_out: 200_000,
            },
            latency: LatencyWindowMs {
                key_p50_ms: 10.0,
                key_p95_ms: 60.0,
            },
            serviced_input_bytes: 128,
            serviced_output_bytes: 64_000,
            output_hard_cap_duration_ms: policy.config.hard_cap_terminate_ms,
        };
        let decision = policy.evaluate(snapshot);
        assert_eq!(
            decision.chosen_action,
            Some(BackpressureAction::TerminateSession)
        );
        assert_eq!(decision.reason, DecisionReason::HardCapExceeded);
        assert!(decision.should_pause_pty_reads);
    }

    #[test]
    fn deterministic_stress_simulation_keeps_queues_bounded() {
        let policy = FlowControlPolicy::default();
        let dt_ms = 10_u64;
        let steps = 6_000_u32; // 60s

        let rates = RateWindowBps {
            lambda_in: 4_000,
            lambda_out: 1_000_000,
            mu_in: 80_000,
            mu_out: 300_000,
        };

        let mut q_in = 0_u32;
        let mut q_out = 0_u32;
        let mut hard_cap_duration_ms = 0_u64;
        let mut max_q_in = 0_u32;
        let mut max_q_out = 0_u32;
        let mut saw_intervention = false;
        let mut terminated = false;
        let mut key_latencies = Vec::with_capacity(steps as usize);

        for _ in 0..steps {
            let key_latency_ms = latency_from_queue(q_in, rates.mu_in);
            let snapshot = FlowControlSnapshot {
                queues: QueueDepthBytes {
                    input: q_in,
                    output: q_out,
                    render_frames: 1,
                },
                rates,
                latency: LatencyWindowMs {
                    key_p50_ms: (key_latency_ms / 2.0).max(1.0),
                    key_p95_ms: key_latency_ms,
                },
                serviced_input_bytes: u64::from(bytes_for_interval(rates.mu_in, dt_ms)),
                serviced_output_bytes: u64::from(bytes_for_interval(rates.mu_out, dt_ms)),
                output_hard_cap_duration_ms: hard_cap_duration_ms,
            };

            let decision = policy.evaluate(snapshot);
            if decision.chosen_action.is_some() {
                saw_intervention = true;
            }

            let mut input_arrival = bytes_for_interval(rates.lambda_in, dt_ms);
            let mut output_arrival = bytes_for_interval(rates.lambda_out, dt_ms);

            match decision.chosen_action {
                Some(BackpressureAction::CoalesceNonInteractive) => {
                    input_arrival = input_arrival.saturating_mul(7) / 10;
                    output_arrival = output_arrival.saturating_mul(8) / 10;
                }
                Some(BackpressureAction::ThrottleOutput) => {
                    output_arrival = output_arrival.saturating_mul(18) / 100;
                }
                Some(BackpressureAction::DropNonInteractive) => {
                    input_arrival /= 2;
                }
                Some(BackpressureAction::TerminateSession) => {
                    terminated = true;
                    break;
                }
                None => {}
            }

            if decision.should_pause_pty_reads {
                output_arrival = 0;
            }

            q_in = q_in.saturating_add(input_arrival);
            q_out = q_out.saturating_add(output_arrival);

            if q_in > policy.config.input_hard_cap_bytes {
                q_in = policy.config.input_hard_cap_bytes;
            }
            if q_out > policy.config.output_hard_cap_bytes {
                q_out = policy.config.output_hard_cap_bytes;
            }

            let input_service = bytes_for_interval(rates.mu_in, dt_ms).min(q_in);
            q_in -= input_service;

            let output_budget = bytes_for_interval(rates.mu_out, dt_ms)
                .min(decision.output_batch_budget_bytes)
                .min(q_out);
            q_out -= output_budget;

            max_q_in = max_q_in.max(q_in);
            max_q_out = max_q_out.max(q_out);
            hard_cap_duration_ms = if q_out >= policy.config.output_hard_cap_bytes {
                hard_cap_duration_ms.saturating_add(dt_ms)
            } else {
                0
            };
            key_latencies.push(latency_from_queue(q_in, rates.mu_in));
        }

        assert!(
            saw_intervention,
            "policy should intervene under output flood"
        );
        assert!(
            !terminated,
            "policy should recover before termination in this scenario"
        );
        assert!(max_q_in <= policy.config.input_hard_cap_bytes);
        assert!(max_q_out <= policy.config.output_hard_cap_bytes);
        let key_p95 = percentile(&key_latencies, 95);
        assert!(
            key_p95 <= 100.0,
            "expected p95 <= 100ms, got {key_p95:.2}ms"
        );
    }

    #[test]
    fn interactive_events_are_never_dropped() {
        let policy = FlowControlPolicy::default();
        assert!(!policy.should_drop_input_event(
            policy.config.input_hard_cap_bytes,
            InputEventClass::Interactive
        ));
        assert!(policy.should_drop_input_event(
            policy.config.input_hard_cap_bytes,
            InputEventClass::NonInteractive
        ));
    }

    fn bytes_for_interval(rate_bps: u32, dt_ms: u64) -> u32 {
        let bytes = u128::from(rate_bps) * u128::from(dt_ms) / 1_000_u128;
        u32::try_from(bytes).unwrap_or(u32::MAX)
    }

    fn latency_from_queue(queue_bytes: u32, service_bps: u32) -> f64 {
        if service_bps == 0 {
            return f64::INFINITY;
        }
        1_000.0 * (f64::from(queue_bytes) / f64::from(service_bps))
    }

    fn percentile(values: &[f64], pct: u8) -> f64 {
        if values.is_empty() {
            return 0.0;
        }
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        let last = sorted.len() - 1;
        let index = (last * usize::from(pct)) / 100;
        sorted[index]
    }

    fn assert_close(actual: f64, expected: f64, epsilon: f64) {
        let delta = (actual - expected).abs();
        assert!(
            delta <= epsilon,
            "expected {expected}, got {actual}, delta={delta}"
        );
    }

    // --- Helper: build a stable (no-pressure) snapshot ---
    fn stable_snapshot() -> FlowControlSnapshot {
        FlowControlSnapshot {
            queues: QueueDepthBytes {
                input: 1024,
                output: 4096,
                render_frames: 0,
            },
            rates: RateWindowBps {
                lambda_in: 1_000,
                lambda_out: 20_000,
                mu_in: 10_000,
                mu_out: 100_000,
            },
            latency: LatencyWindowMs {
                key_p50_ms: 2.0,
                key_p95_ms: 8.0,
            },
            serviced_input_bytes: 40_000,
            serviced_output_bytes: 42_000,
            output_hard_cap_duration_ms: 0,
        }
    }

    // ---- ratio_u32 ----

    #[test]
    fn ratio_u32_normal_division() {
        assert_close(ratio_u32(100, 200), 0.5, 1e-9);
        assert_close(ratio_u32(200, 100), 2.0, 1e-9);
        assert_close(ratio_u32(0, 100), 0.0, 1e-9);
    }

    #[test]
    fn ratio_u32_zero_denominator_is_infinity() {
        assert!(ratio_u32(100, 0).is_infinite());
        assert!(ratio_u32(0, 0).is_infinite());
    }

    // ---- clamp01 ----

    #[test]
    fn clamp01_bounds() {
        assert_close(clamp01(-1.0), 0.0, 1e-9);
        assert_close(clamp01(0.5), 0.5, 1e-9);
        assert_close(clamp01(1.5), 1.0, 1e-9);
        assert_close(clamp01(0.0), 0.0, 1e-9);
        assert_close(clamp01(1.0), 1.0, 1e-9);
    }

    // ---- jain_fairness_index additional cases ----

    #[test]
    fn jain_fairness_index_asymmetric() {
        // When one side dominates, fairness should be low
        let f = jain_fairness_index(1_000_000, 1);
        assert!(f < 0.6, "highly asymmetric should be near 0.5: got {f}");
        assert!(f >= 0.5, "Jain index for 2 flows is always >= 0.5: got {f}");
    }

    #[test]
    fn jain_fairness_index_symmetry() {
        // Order should not matter
        let a = jain_fairness_index(100, 200);
        let b = jain_fairness_index(200, 100);
        assert_close(a, b, 1e-9);
    }

    #[test]
    fn fairness_index_snapshot_convenience() {
        let snap = stable_snapshot();
        let direct = jain_fairness_index(snap.serviced_input_bytes, snap.serviced_output_bytes);
        assert_close(snap.fairness_index(), direct, 1e-12);
    }

    // ---- should_replenish ----

    #[test]
    fn should_replenish_always_when_window_zero() {
        let policy = FlowControlPolicy::default();
        assert!(policy.should_replenish(0, 0, 0));
    }

    #[test]
    fn should_replenish_at_50_percent_consumption() {
        let policy = FlowControlPolicy::default();
        // consumed*2 >= window → true
        assert!(policy.should_replenish(500, 1000, 0));
        // consumed*2 < window → false (if elapsed < interval)
        assert!(!policy.should_replenish(400, 1000, 0));
    }

    #[test]
    fn should_replenish_on_interval_timeout() {
        let policy = FlowControlPolicy::default();
        // Low consumption but elapsed >= replenish_interval_ms
        assert!(policy.should_replenish(0, 10_000, policy.config.replenish_interval_ms));
        assert!(!policy.should_replenish(0, 10_000, policy.config.replenish_interval_ms - 1));
    }

    // ---- should_drop_input_event ----

    #[test]
    fn non_interactive_dropped_only_at_hard_cap() {
        let policy = FlowControlPolicy::default();
        let below = policy.config.input_hard_cap_bytes - 1;
        assert!(!policy.should_drop_input_event(below, InputEventClass::NonInteractive));
        assert!(policy.should_drop_input_event(
            policy.config.input_hard_cap_bytes,
            InputEventClass::NonInteractive
        ));
    }

    #[test]
    fn interactive_never_dropped_even_at_max() {
        let policy = FlowControlPolicy::default();
        assert!(!policy.should_drop_input_event(u32::MAX, InputEventClass::Interactive));
    }

    // ---- BackpressureAction tie_break_rank ordering ----

    #[test]
    fn tie_break_ranks_are_ordered() {
        assert!(
            BackpressureAction::CoalesceNonInteractive.tie_break_rank()
                < BackpressureAction::ThrottleOutput.tie_break_rank()
        );
        assert!(
            BackpressureAction::ThrottleOutput.tie_break_rank()
                < BackpressureAction::DropNonInteractive.tie_break_rank()
        );
        assert!(
            BackpressureAction::DropNonInteractive.tie_break_rank()
                < BackpressureAction::TerminateSession.tie_break_rank()
        );
    }

    // ---- select_best_action ----

    #[test]
    fn select_best_action_picks_lowest_loss() {
        let losses = [
            ActionLoss {
                action: BackpressureAction::CoalesceNonInteractive,
                expected_loss: 50.0,
                oom_risk: 0.0,
                latency_risk: 0.0,
                throughput_loss: 0.0,
            },
            ActionLoss {
                action: BackpressureAction::ThrottleOutput,
                expected_loss: 10.0,
                oom_risk: 0.0,
                latency_risk: 0.0,
                throughput_loss: 0.0,
            },
            ActionLoss {
                action: BackpressureAction::DropNonInteractive,
                expected_loss: 30.0,
                oom_risk: 0.0,
                latency_risk: 0.0,
                throughput_loss: 0.0,
            },
            ActionLoss {
                action: BackpressureAction::TerminateSession,
                expected_loss: 100.0,
                oom_risk: 0.0,
                latency_risk: 0.0,
                throughput_loss: 0.0,
            },
        ];
        assert_eq!(
            select_best_action(&losses),
            BackpressureAction::ThrottleOutput
        );
    }

    // ---- output_batch_budget edge cases ----

    #[test]
    fn output_batch_budget_idle_no_pressure() {
        let policy = FlowControlPolicy::default();
        let budget = policy.output_batch_budget(0, 1.0, 10.0);
        assert_eq!(budget, policy.config.output_batch_idle_bytes);
    }

    #[test]
    fn output_batch_budget_with_input_no_pressure() {
        let policy = FlowControlPolicy::default();
        let budget = policy.output_batch_budget(100, 1.0, 10.0);
        assert_eq!(budget, policy.config.output_batch_with_input_bytes);
    }

    #[test]
    fn output_batch_budget_fairness_recovery_clamps() {
        let policy = FlowControlPolicy::default();
        // Fairness below floor triggers recovery
        let budget = policy.output_batch_budget(100, 0.5, 10.0);
        assert_eq!(budget, policy.config.output_batch_recovery_bytes);
    }

    #[test]
    fn output_batch_budget_latency_recovery_clamps() {
        let policy = FlowControlPolicy::default();
        // p95 above budget triggers recovery
        let budget = policy.output_batch_budget(0, 1.0, 200.0);
        assert_eq!(budget, policy.config.output_batch_recovery_bytes);
    }

    #[test]
    fn output_batch_budget_both_triggers_still_recovery() {
        let policy = FlowControlPolicy::default();
        // Both fairness and latency in violation
        let budget = policy.output_batch_budget(50, 0.3, 200.0);
        assert_eq!(budget, policy.config.output_batch_recovery_bytes);
    }

    // ---- is_pressured (via evaluate) ----

    #[test]
    fn pressured_when_input_at_soft_cap() {
        let policy = FlowControlPolicy::default();
        let mut snap = stable_snapshot();
        snap.queues.input = policy.config.input_soft_cap_bytes;
        let decision = policy.evaluate(snap);
        assert!(
            decision.chosen_action.is_some(),
            "should intervene when input queue at soft cap"
        );
    }

    #[test]
    fn pressured_when_output_at_soft_cap() {
        let policy = FlowControlPolicy::default();
        let mut snap = stable_snapshot();
        snap.queues.output = policy.config.output_soft_cap_bytes;
        let decision = policy.evaluate(snap);
        assert!(
            decision.chosen_action.is_some(),
            "should intervene when output queue at soft cap"
        );
    }

    #[test]
    fn pressured_when_input_rate_exceeds_service() {
        let policy = FlowControlPolicy::default();
        let mut snap = stable_snapshot();
        snap.rates.lambda_in = 100_000;
        snap.rates.mu_in = 50_000; // rho > 1
        let decision = policy.evaluate(snap);
        assert!(
            decision.chosen_action.is_some(),
            "should intervene when input arrival > service"
        );
    }

    #[test]
    fn pressured_when_output_rate_exceeds_service() {
        let policy = FlowControlPolicy::default();
        let mut snap = stable_snapshot();
        snap.rates.lambda_out = 200_000;
        snap.rates.mu_out = 100_000; // rho > 1
        let decision = policy.evaluate(snap);
        assert!(
            decision.chosen_action.is_some(),
            "should intervene when output arrival > service"
        );
    }

    #[test]
    fn pressured_when_latency_budget_breached() {
        let policy = FlowControlPolicy::default();
        let mut snap = stable_snapshot();
        snap.latency.key_p95_ms = policy.config.key_latency_budget_ms + 10.0;
        let decision = policy.evaluate(snap);
        assert!(
            decision.chosen_action.is_some(),
            "should intervene when latency exceeds budget"
        );
        assert_eq!(decision.reason, DecisionReason::ProtectKeyLatencyBudget);
    }

    #[test]
    fn pressured_when_fairness_below_floor() {
        let policy = FlowControlPolicy::default();
        let mut snap = stable_snapshot();
        // Make fairness very low: one side gets almost everything
        snap.serviced_input_bytes = 1;
        snap.serviced_output_bytes = 1_000_000;
        assert!(snap.fairness_index() < policy.config.fairness_floor);
        let decision = policy.evaluate(snap);
        assert!(
            decision.chosen_action.is_some(),
            "should intervene when fairness below floor"
        );
        assert_eq!(decision.reason, DecisionReason::ProtectKeyLatencyBudget);
    }

    // ---- reason codes ----

    #[test]
    fn reason_stable_when_no_pressure() {
        let policy = FlowControlPolicy::default();
        let decision = policy.evaluate(stable_snapshot());
        assert_eq!(decision.reason, DecisionReason::Stable);
    }

    #[test]
    fn reason_queue_pressure_without_latency_fairness_issue() {
        let policy = FlowControlPolicy::default();
        let mut snap = stable_snapshot();
        // Queue pressure without latency/fairness issues
        snap.queues.output = policy.config.output_soft_cap_bytes;
        let decision = policy.evaluate(snap);
        assert_eq!(decision.reason, DecisionReason::QueuePressure);
    }

    #[test]
    fn reason_hard_cap_exceeded_overrides_everything() {
        let policy = FlowControlPolicy::default();
        let mut snap = stable_snapshot();
        snap.output_hard_cap_duration_ms = policy.config.hard_cap_terminate_ms;
        let decision = policy.evaluate(snap);
        assert_eq!(decision.reason, DecisionReason::HardCapExceeded);
        assert_eq!(
            decision.chosen_action,
            Some(BackpressureAction::TerminateSession)
        );
    }

    // ---- should_pause_pty_reads ----

    #[test]
    fn pause_pty_reads_at_output_hard_cap() {
        let policy = FlowControlPolicy::default();
        let mut snap = stable_snapshot();
        snap.queues.output = policy.config.output_hard_cap_bytes;
        let decision = policy.evaluate(snap);
        assert!(decision.should_pause_pty_reads);
    }

    #[test]
    fn no_pause_pty_reads_below_hard_cap() {
        let policy = FlowControlPolicy::default();
        let mut snap = stable_snapshot();
        snap.queues.output = policy.config.output_hard_cap_bytes - 1;
        let decision = policy.evaluate(snap);
        assert!(!decision.should_pause_pty_reads);
    }

    // ---- score_action properties ----

    #[test]
    fn terminate_has_fixed_throughput_loss() {
        let policy = FlowControlPolicy::default();
        let signals = PressureSignals {
            oom_signal: 0.5,
            latency_signal: 0.5,
            throughput_signal: 0.5,
        };
        let loss = policy.score_action(BackpressureAction::TerminateSession, signals);
        assert_close(
            loss.throughput_loss,
            policy.config.terminate_throughput_loss,
            1e-9,
        );
        assert_close(loss.oom_risk, 0.0, 1e-9);
        assert_close(loss.latency_risk, 0.0, 1e-9);
    }

    #[test]
    fn zero_pressure_yields_minimal_non_terminate_losses() {
        let policy = FlowControlPolicy::default();
        let signals = PressureSignals {
            oom_signal: 0.0,
            latency_signal: 0.0,
            throughput_signal: 0.0,
        };
        for action in [
            BackpressureAction::CoalesceNonInteractive,
            BackpressureAction::ThrottleOutput,
            BackpressureAction::DropNonInteractive,
        ] {
            let loss = policy.score_action(action, signals);
            assert_close(loss.oom_risk, 0.0, 1e-9);
            assert_close(loss.latency_risk, 0.0, 1e-9);
            // throughput_loss has a baseline > 0 for non-terminate actions
            assert!(loss.throughput_loss > 0.0);
        }
    }

    #[test]
    fn coalesce_always_cheapest_under_zero_pressure() {
        let policy = FlowControlPolicy::default();
        let signals = PressureSignals {
            oom_signal: 0.0,
            latency_signal: 0.0,
            throughput_signal: 0.0,
        };
        let coalesce = policy.score_action(BackpressureAction::CoalesceNonInteractive, signals);
        let throttle = policy.score_action(BackpressureAction::ThrottleOutput, signals);
        let drop = policy.score_action(BackpressureAction::DropNonInteractive, signals);
        assert!(
            coalesce.expected_loss <= throttle.expected_loss,
            "coalesce should be <= throttle at zero pressure"
        );
        assert!(
            throttle.expected_loss <= drop.expected_loss,
            "throttle should be <= drop at zero pressure"
        );
    }

    #[test]
    fn high_pressure_increases_oom_and_latency_risk() {
        let policy = FlowControlPolicy::default();
        let low = PressureSignals {
            oom_signal: 0.1,
            latency_signal: 0.1,
            throughput_signal: 0.1,
        };
        let high = PressureSignals {
            oom_signal: 0.9,
            latency_signal: 0.9,
            throughput_signal: 0.9,
        };
        let action = BackpressureAction::CoalesceNonInteractive;
        let loss_low = policy.score_action(action, low);
        let loss_high = policy.score_action(action, high);
        assert!(loss_high.oom_risk > loss_low.oom_risk);
        assert!(loss_high.latency_risk > loss_low.latency_risk);
        assert!(loss_high.throughput_loss > loss_low.throughput_loss);
    }

    // ---- pressure_signals ----

    #[test]
    fn pressure_signals_all_zero_when_stable() {
        let policy = FlowControlPolicy::default();
        let snap = stable_snapshot();
        let fi = snap.fairness_index();
        let signals = policy.pressure_signals(snap, fi);
        assert_close(signals.oom_signal, 0.0, 1e-9);
        assert_close(signals.latency_signal, 0.0, 1e-9);
        assert_close(signals.throughput_signal, 0.0, 1e-9);
    }

    #[test]
    fn oom_signal_rises_with_queue_depth() {
        let policy = FlowControlPolicy::default();
        let mut snap = stable_snapshot();
        // Put output queue near hard cap (>70%)
        snap.queues.output = (policy.config.output_hard_cap_bytes as f64 * 0.9) as u32;
        let fi = snap.fairness_index();
        let signals = policy.pressure_signals(snap, fi);
        assert!(
            signals.oom_signal > 0.0,
            "oom_signal should rise at 90% of hard cap: got {}",
            signals.oom_signal
        );
    }

    #[test]
    fn oom_signal_rises_with_utilization_above_one() {
        let policy = FlowControlPolicy::default();
        let mut snap = stable_snapshot();
        snap.rates.lambda_out = 200_000;
        snap.rates.mu_out = 100_000; // rho = 2.0
        let fi = snap.fairness_index();
        let signals = policy.pressure_signals(snap, fi);
        assert!(
            signals.oom_signal > 0.0,
            "oom_signal should rise when rho > 1: got {}",
            signals.oom_signal
        );
    }

    #[test]
    fn latency_signal_rises_when_p95_exceeds_budget() {
        let policy = FlowControlPolicy::default();
        let mut snap = stable_snapshot();
        snap.latency.key_p95_ms = policy.config.key_latency_budget_ms * 2.0;
        let fi = snap.fairness_index();
        let signals = policy.pressure_signals(snap, fi);
        assert!(
            signals.latency_signal > 0.0,
            "latency_signal should rise when p95 > budget: got {}",
            signals.latency_signal
        );
    }

    #[test]
    fn latency_signal_rises_with_fairness_shortfall() {
        let policy = FlowControlPolicy::default();
        let mut snap = stable_snapshot();
        snap.serviced_input_bytes = 1;
        snap.serviced_output_bytes = 1_000_000;
        let fi = snap.fairness_index();
        assert!(fi < policy.config.fairness_floor);
        let signals = policy.pressure_signals(snap, fi);
        assert!(
            signals.latency_signal > 0.0,
            "latency_signal should rise when fairness < floor: got {}",
            signals.latency_signal
        );
    }

    #[test]
    fn throughput_signal_rises_with_output_utilization() {
        let policy = FlowControlPolicy::default();
        let mut snap = stable_snapshot();
        snap.rates.lambda_out = 200_000;
        snap.rates.mu_out = 100_000; // rho_out = 2.0
        let fi = snap.fairness_index();
        let signals = policy.pressure_signals(snap, fi);
        assert!(
            signals.throughput_signal > 0.0,
            "throughput_signal should rise when rho_out > 1: got {}",
            signals.throughput_signal
        );
    }

    #[test]
    fn throughput_signal_rises_with_output_soft_ratio() {
        let policy = FlowControlPolicy::default();
        let mut snap = stable_snapshot();
        snap.queues.output = policy.config.output_soft_cap_bytes * 2;
        let fi = snap.fairness_index();
        let signals = policy.pressure_signals(snap, fi);
        assert!(
            signals.throughput_signal > 0.0,
            "throughput_signal should rise when output > soft cap: got {}",
            signals.throughput_signal
        );
    }

    // ---- evaluate integration: losses array always has 4 entries ----

    #[test]
    fn evaluate_losses_array_covers_all_actions() {
        let policy = FlowControlPolicy::default();
        let decision = policy.evaluate(stable_snapshot());
        assert_eq!(decision.losses.len(), 4);
        let actions: Vec<_> = decision.losses.iter().map(|l| l.action).collect();
        assert!(actions.contains(&BackpressureAction::CoalesceNonInteractive));
        assert!(actions.contains(&BackpressureAction::ThrottleOutput));
        assert!(actions.contains(&BackpressureAction::DropNonInteractive));
        assert!(actions.contains(&BackpressureAction::TerminateSession));
    }

    // ---- evaluate integration: hard-cap just below threshold ----

    #[test]
    fn hard_cap_just_below_threshold_does_not_terminate() {
        let policy = FlowControlPolicy::default();
        let mut snap = stable_snapshot();
        snap.output_hard_cap_duration_ms = policy.config.hard_cap_terminate_ms - 1;
        snap.queues.output = policy.config.output_hard_cap_bytes;
        let decision = policy.evaluate(snap);
        assert_ne!(decision.reason, DecisionReason::HardCapExceeded);
        assert_ne!(
            decision.chosen_action,
            Some(BackpressureAction::TerminateSession)
        );
    }

    // ---- LossWeights and FlowControlConfig defaults ----

    #[test]
    fn default_weights_hierarchy() {
        let w = LossWeights::default();
        // OOM >> latency >> throughput
        assert!(w.oom > w.latency);
        assert!(w.latency > w.throughput);
    }

    #[test]
    fn default_config_cap_hierarchy() {
        let c = FlowControlConfig::default();
        assert!(c.input_soft_cap_bytes < c.input_hard_cap_bytes);
        assert!(c.output_soft_cap_bytes < c.output_hard_cap_bytes);
        assert!(c.output_batch_recovery_bytes < c.output_batch_with_input_bytes);
        assert!(c.output_batch_with_input_bytes < c.output_batch_idle_bytes);
    }

    // ---- custom config propagation ----

    #[test]
    fn custom_config_changes_behavior() {
        let config = FlowControlConfig {
            input_soft_cap_bytes: 100,
            input_hard_cap_bytes: 200,
            output_soft_cap_bytes: 100,
            output_hard_cap_bytes: 200,
            ..FlowControlConfig::default()
        };
        let policy = FlowControlPolicy::new(config);
        // Drop non-interactive at custom hard cap
        assert!(policy.should_drop_input_event(200, InputEventClass::NonInteractive));
        assert!(!policy.should_drop_input_event(199, InputEventClass::NonInteractive));
    }
}
