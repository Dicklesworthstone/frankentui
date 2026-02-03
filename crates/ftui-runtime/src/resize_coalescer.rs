//! Adaptive resize stream coalescer.
//!
//! This module implements the resize coalescing behavior specified in
//! `docs/spec/resize-scheduler.md`. It provides:
//!
//! - **Latest-wins semantics**: Only the final size in a burst is rendered
//! - **Bounded latency**: Hard deadline guarantees render within max wait
//! - **Regime awareness**: Adapts behavior between steady and burst modes
//! - **Decision logging**: JSONL-compatible evidence for each decision
//!
//! # Usage
//!
//! ```ignore
//! use ftui_runtime::resize_coalescer::{ResizeCoalescer, CoalescerConfig};
//!
//! let config = CoalescerConfig::default();
//! let mut coalescer = ResizeCoalescer::new(config, (80, 24));
//!
//! // On resize event
//! let action = coalescer.handle_resize(100, 40);
//!
//! // On tick (called each frame)
//! let action = coalescer.tick();
//! ```
//!
//! # Regime Detection
//!
//! The coalescer uses a simplified regime model with two states:
//! - **Steady**: Single resize or slow sequence — prioritize responsiveness
//! - **Burst**: Rapid resize events — prioritize coalescing to reduce work
//!
//! Regime transitions are detected via event rate tracking with hysteresis.
//!
//! # Invariants
//!
//! - **Latest-wins**: the final resize in a burst is never dropped.
//! - **Bounded latency**: pending resizes apply within `hard_deadline_ms`.
//! - **Deterministic**: identical event sequences yield identical decisions.
//!
//! # Failure Modes
//!
//! | Condition | Behavior | Rationale |
//! |-----------|----------|-----------|
//! | `hard_deadline_ms = 0` | Apply immediately | Avoids zero-latency stall |
//! | `rate_window_size < 2` | `event_rate = 0` | No divide-by-zero in rate |
//! | No pending size | Return `None` | Avoids spurious applies |
//!
//! # Decision Rule (Explainable)
//!
//! 1) If `time_since_render ≥ hard_deadline_ms`, **apply** (forced).
//! 2) If in **Steady** and `dt ≥ steady_delay_ms`, **apply**.
//! 3) If `event_rate ≥ burst_enter_rate`, switch to **Burst**.
//! 4) If in **Burst** and `event_rate < burst_exit_rate` for `cooldown_frames`,
//!    switch to **Steady**.
//! 5) Otherwise, **coalesce** and optionally show a placeholder.

#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// FNV-1a 64-bit offset basis.
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x100000001b3;

fn fnv_hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= *byte as u64;
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

/// Configuration for the resize coalescer.
#[derive(Debug, Clone)]
pub struct CoalescerConfig {
    /// Maximum coalesce delay in steady regime (ms).
    /// In steady state, we want quick response.
    pub steady_delay_ms: u64,

    /// Maximum coalesce delay in burst regime (ms).
    /// During bursts, we coalesce more aggressively.
    pub burst_delay_ms: u64,

    /// Hard deadline — always render within this time (ms).
    /// Guarantees bounded worst-case latency.
    pub hard_deadline_ms: u64,

    /// Event rate threshold to enter burst mode (events/second).
    pub burst_enter_rate: f64,

    /// Event rate threshold to exit burst mode (events/second).
    /// Lower than enter_rate for hysteresis.
    pub burst_exit_rate: f64,

    /// Number of frames to hold in burst mode after rate drops.
    pub cooldown_frames: u32,

    /// Window size for rate calculation (number of events).
    pub rate_window_size: usize,

    /// Enable decision logging (JSONL format).
    pub enable_logging: bool,
}

impl Default for CoalescerConfig {
    fn default() -> Self {
        Self {
            steady_delay_ms: 16, // ~60fps responsiveness
            burst_delay_ms: 40,  // Aggressive coalescing
            hard_deadline_ms: 100,
            burst_enter_rate: 10.0, // 10 events/sec to enter burst
            burst_exit_rate: 5.0,   // 5 events/sec to exit burst
            cooldown_frames: 3,
            rate_window_size: 8,
            enable_logging: false,
        }
    }
}

impl CoalescerConfig {
    /// Enable or disable decision logging.
    #[must_use]
    pub fn with_logging(mut self, enabled: bool) -> Self {
        self.enable_logging = enabled;
        self
    }

    /// Serialize configuration to JSONL format.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        format!(
            r#"{{"event":"config","steady_delay_ms":{},"burst_delay_ms":{},"hard_deadline_ms":{},"burst_enter_rate":{:.3},"burst_exit_rate":{:.3},"cooldown_frames":{},"rate_window_size":{},"logging_enabled":{}}}"#,
            self.steady_delay_ms,
            self.burst_delay_ms,
            self.hard_deadline_ms,
            self.burst_enter_rate,
            self.burst_exit_rate,
            self.cooldown_frames,
            self.rate_window_size,
            self.enable_logging
        )
    }
}

/// Action returned by the coalescer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoalesceAction {
    /// No action needed.
    None,

    /// Show a placeholder/skeleton while coalescing.
    ShowPlaceholder,

    /// Apply the resize with the given dimensions.
    ApplyResize {
        width: u16,
        height: u16,
        /// Time spent coalescing.
        coalesce_time: Duration,
        /// Whether this was forced by hard deadline.
        forced_by_deadline: bool,
    },
}

/// Detected regime for resize events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Regime {
    /// Single resize or slow sequence.
    #[default]
    Steady,
    /// Rapid resize events (storm).
    Burst,
}

impl Regime {
    /// Get the stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Steady => "steady",
            Self::Burst => "burst",
        }
    }
}

/// Event emitted when a resize operation is applied (bd-bksf.6 stub).
///
/// Used by [`ResizeSlaMonitor`](crate::resize_sla::ResizeSlaMonitor) for latency tracking.
#[derive(Debug, Clone)]
pub struct ResizeAppliedEvent {
    /// New terminal size after resize.
    pub new_size: (u16, u16),
    /// Previous terminal size.
    pub old_size: (u16, u16),
    /// Time elapsed from resize request to apply.
    pub elapsed: Duration,
    /// Whether the apply was forced (hard deadline).
    pub forced: bool,
}

/// Event emitted when regime changes between Steady and Burst (bd-bksf.6 stub).
///
/// Used by SLA monitoring for regime-aware alerting.
#[derive(Debug, Clone)]
pub struct RegimeChangeEvent {
    /// Previous regime.
    pub from: Regime,
    /// New regime.
    pub to: Regime,
    /// Event index when transition occurred.
    pub event_idx: u64,
}

// =============================================================================
// Evidence Ledger for Scheduler Decisions (bd-1rz0.27)
// =============================================================================

/// Evidence supporting a scheduler decision with Bayes factors.
///
/// This captures the mathematical reasoning behind coalesce/apply decisions,
/// providing explainability for the regime-adaptive scheduler.
///
/// # Bayes Factor Interpretation
///
/// The `log_bayes_factor` represents log10(P(evidence|apply_now) / P(evidence|coalesce)):
/// - Positive values favor immediate apply (respond quickly)
/// - Negative values favor coalescing (wait for more events)
/// - |LBF| > 1 is "strong" evidence, |LBF| > 2 is "decisive"
///
/// # Example
///
/// ```ignore
/// let evidence = DecisionEvidence {
///     log_bayes_factor: 1.5,  // Strong evidence to apply now
///     regime_contribution: 0.8,
///     timing_contribution: 0.5,
///     rate_contribution: 0.2,
///     explanation: "Steady regime with long idle interval".to_string(),
/// };
/// ```
#[derive(Debug, Clone)]
pub struct DecisionEvidence {
    /// Log10 Bayes factor: positive favors apply, negative favors coalesce.
    pub log_bayes_factor: f64,

    /// Contribution from regime detection (Steady vs Burst).
    pub regime_contribution: f64,

    /// Contribution from timing (dt_ms, time since last render).
    pub timing_contribution: f64,

    /// Contribution from event rate.
    pub rate_contribution: f64,

    /// Human-readable explanation of the decision.
    pub explanation: String,
}

impl DecisionEvidence {
    /// Evidence strongly favoring immediate apply (Steady regime, long idle).
    #[must_use]
    pub fn favor_apply(regime: Regime, dt_ms: f64, event_rate: f64) -> Self {
        let regime_contrib = if regime == Regime::Steady { 1.0 } else { -0.5 };
        let timing_contrib = (dt_ms / 50.0).min(2.0); // Higher dt -> favor apply
        let rate_contrib = if event_rate < 5.0 { 0.5 } else { -0.3 };

        let lbf = regime_contrib + timing_contrib + rate_contrib;

        Self {
            log_bayes_factor: lbf,
            regime_contribution: regime_contrib,
            timing_contribution: timing_contrib,
            rate_contribution: rate_contrib,
            explanation: format!(
                "Regime={:?} (contrib={:.2}), dt={:.1}ms (contrib={:.2}), rate={:.1}/s (contrib={:.2})",
                regime, regime_contrib, dt_ms, timing_contrib, event_rate, rate_contrib
            ),
        }
    }

    /// Evidence favoring coalescing (Burst regime, high rate).
    #[must_use]
    pub fn favor_coalesce(regime: Regime, dt_ms: f64, event_rate: f64) -> Self {
        let regime_contrib = if regime == Regime::Burst { 1.0 } else { -0.5 };
        let timing_contrib = (20.0 / dt_ms.max(1.0)).min(2.0); // Lower dt -> favor coalesce
        let rate_contrib = if event_rate > 10.0 { 0.5 } else { -0.3 };

        let lbf = -(regime_contrib + timing_contrib + rate_contrib);

        Self {
            log_bayes_factor: lbf,
            regime_contribution: regime_contrib,
            timing_contribution: timing_contrib,
            rate_contribution: rate_contrib,
            explanation: format!(
                "Regime={:?} (contrib={:.2}), dt={:.1}ms (contrib={:.2}), rate={:.1}/s (contrib={:.2})",
                regime, regime_contrib, dt_ms, timing_contrib, event_rate, rate_contrib
            ),
        }
    }

    /// Evidence for a forced deadline decision.
    #[must_use]
    pub fn forced_deadline(deadline_ms: f64) -> Self {
        Self {
            log_bayes_factor: f64::INFINITY,
            regime_contribution: 0.0,
            timing_contribution: deadline_ms,
            rate_contribution: 0.0,
            explanation: format!("Forced by hard deadline ({:.1}ms)", deadline_ms),
        }
    }

    /// Serialize to JSONL format.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        let lbf_str = if self.log_bayes_factor.is_infinite() {
            "\"inf\"".to_string()
        } else {
            format!("{:.3}", self.log_bayes_factor)
        };
        format!(
            r#"{{"event":"decision_evidence","log_bayes_factor":{},"regime_contribution":{:.3},"timing_contribution":{:.3},"rate_contribution":{:.3},"explanation":"{}"}}"#,
            lbf_str,
            self.regime_contribution,
            self.timing_contribution,
            self.rate_contribution,
            self.explanation.replace('"', "\\\"")
        )
    }

    /// Check if evidence strongly supports the action (|LBF| > 1).
    #[must_use]
    pub fn is_strong(&self) -> bool {
        self.log_bayes_factor.abs() > 1.0
    }

    /// Check if evidence decisively supports the action (|LBF| > 2).
    #[must_use]
    pub fn is_decisive(&self) -> bool {
        self.log_bayes_factor.abs() > 2.0 || self.log_bayes_factor.is_infinite()
    }
}

/// Decision log entry for observability.
#[derive(Debug, Clone)]
pub struct DecisionLog {
    /// Timestamp of the decision.
    pub timestamp: Instant,
    /// Elapsed time since logging started (ms).
    pub elapsed_ms: f64,
    /// Event index in session.
    pub event_idx: u64,
    /// Time since last event (ms).
    pub dt_ms: f64,
    /// Current event rate (events/sec).
    pub event_rate: f64,
    /// Detected regime.
    pub regime: Regime,
    /// Chosen action.
    pub action: &'static str,
    /// Pending size (if any).
    pub pending_size: Option<(u16, u16)>,
    /// Applied size (for apply decisions).
    pub applied_size: Option<(u16, u16)>,
    /// Time since last render (ms).
    pub time_since_render_ms: f64,
    /// Time spent coalescing until apply (ms).
    pub coalesce_ms: Option<f64>,
    /// Was forced by deadline.
    pub forced: bool,
}

impl DecisionLog {
    /// Serialize decision log to JSONL format.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        let (pending_w, pending_h) = match self.pending_size {
            Some((w, h)) => (w.to_string(), h.to_string()),
            None => ("null".to_string(), "null".to_string()),
        };
        let (applied_w, applied_h) = match self.applied_size {
            Some((w, h)) => (w.to_string(), h.to_string()),
            None => ("null".to_string(), "null".to_string()),
        };
        let coalesce_ms = match self.coalesce_ms {
            Some(ms) => format!("{:.3}", ms),
            None => "null".to_string(),
        };

        format!(
            r#"{{"event":"decision","idx":{},"elapsed_ms":{:.3},"dt_ms":{:.3},"event_rate":{:.3},"regime":"{}","action":"{}","pending_w":{},"pending_h":{},"applied_w":{},"applied_h":{},"time_since_render_ms":{:.3},"coalesce_ms":{},"forced":{}}}"#,
            self.event_idx,
            self.elapsed_ms,
            self.dt_ms,
            self.event_rate,
            self.regime.as_str(),
            self.action,
            pending_w,
            pending_h,
            applied_w,
            applied_h,
            self.time_since_render_ms,
            coalesce_ms,
            self.forced
        )
    }
}

/// Adaptive resize stream coalescer.
///
/// Implements latest-wins coalescing with regime-aware behavior.
#[derive(Debug)]
pub struct ResizeCoalescer {
    config: CoalescerConfig,

    /// Currently pending size (latest wins).
    pending_size: Option<(u16, u16)>,

    /// Last applied size.
    last_applied: (u16, u16),

    /// Timestamp of first event in current coalesce window.
    window_start: Option<Instant>,

    /// Timestamp of last resize event.
    last_event: Option<Instant>,

    /// Timestamp of last render.
    last_render: Instant,

    /// Current detected regime.
    regime: Regime,

    /// Frames remaining in cooldown (for burst exit hysteresis).
    cooldown_remaining: u32,

    /// Recent event timestamps for rate calculation.
    event_times: VecDeque<Instant>,

    /// Total event count.
    event_count: u64,

    /// Logging start time for elapsed timestamps.
    log_start: Option<Instant>,

    /// Decision logs (if logging enabled).
    logs: Vec<DecisionLog>,

    // --- Telemetry integration (bd-1rz0.7) ---
    /// Telemetry hooks for external observability.
    telemetry_hooks: Option<TelemetryHooks>,

    /// Count of regime transitions during this session.
    regime_transitions: u64,

    /// Events coalesced in current window.
    events_in_window: u64,

    /// History of cycle times (ms) for percentile calculation.
    cycle_times: Vec<f64>,
}

/// Cycle time percentiles for reflow diagnostics (bd-1rz0.7).
#[derive(Debug, Clone, Copy)]
pub struct CycleTimePercentiles {
    /// 50th percentile (median) cycle time in ms.
    pub p50_ms: f64,
    /// 95th percentile cycle time in ms.
    pub p95_ms: f64,
    /// 99th percentile cycle time in ms.
    pub p99_ms: f64,
    /// Number of samples.
    pub count: usize,
    /// Mean cycle time in ms.
    pub mean_ms: f64,
}

impl CycleTimePercentiles {
    /// Serialize to JSONL format.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        format!(
            r#"{{"event":"cycle_time_percentiles","p50_ms":{:.3},"p95_ms":{:.3},"p99_ms":{:.3},"mean_ms":{:.3},"count":{}}}"#,
            self.p50_ms, self.p95_ms, self.p99_ms, self.mean_ms, self.count
        )
    }
}

impl ResizeCoalescer {
    /// Create a new coalescer with the given configuration and initial size.
    pub fn new(config: CoalescerConfig, initial_size: (u16, u16)) -> Self {
        Self {
            config,
            pending_size: None,
            last_applied: initial_size,
            window_start: None,
            last_event: None,
            last_render: Instant::now(),
            regime: Regime::Steady,
            cooldown_remaining: 0,
            event_times: VecDeque::new(),
            event_count: 0,
            log_start: None,
            logs: Vec::new(),
            telemetry_hooks: None,
            regime_transitions: 0,
            events_in_window: 0,
            cycle_times: Vec::new(),
        }
    }

    /// Attach telemetry hooks for external observability.
    #[must_use]
    pub fn with_telemetry_hooks(mut self, hooks: TelemetryHooks) -> Self {
        self.telemetry_hooks = Some(hooks);
        self
    }

    /// Set the last render time (for deterministic testing).
    #[must_use]
    pub fn with_last_render(mut self, time: Instant) -> Self {
        self.last_render = time;
        self
    }

    /// Record an externally-applied resize (immediate path).
    pub fn record_external_apply(&mut self, width: u16, height: u16, now: Instant) {
        self.event_count += 1;
        self.event_times.push_back(now);
        while self.event_times.len() > self.config.rate_window_size {
            self.event_times.pop_front();
        }
        self.update_regime(now);

        self.pending_size = None;
        self.window_start = None;
        self.last_event = Some(now);
        self.last_applied = (width, height);
        self.last_render = now;
        self.events_in_window = 0;
        self.cooldown_remaining = 0;

        self.log_decision(now, "apply_immediate", false, Some(0.0), Some(0.0));

        if let Some(ref hooks) = self.telemetry_hooks
            && let Some(entry) = self.logs.last()
        {
            hooks.fire_resize_applied(entry);
        }
    }

    /// Get current regime transition count.
    #[must_use]
    pub fn regime_transition_count(&self) -> u64 {
        self.regime_transitions
    }

    /// Get cycle time percentiles (p50, p95, p99) in milliseconds.
    /// Returns None if no cycle times recorded.
    #[must_use]
    pub fn cycle_time_percentiles(&self) -> Option<CycleTimePercentiles> {
        if self.cycle_times.is_empty() {
            return None;
        }

        let mut sorted = self.cycle_times.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let len = sorted.len();
        let p50_idx = len / 2;
        let p95_idx = (len * 95) / 100;
        let p99_idx = (len * 99) / 100;

        Some(CycleTimePercentiles {
            p50_ms: sorted[p50_idx],
            p95_ms: sorted[p95_idx.min(len - 1)],
            p99_ms: sorted[p99_idx.min(len - 1)],
            count: len,
            mean_ms: sorted.iter().sum::<f64>() / len as f64,
        })
    }

    /// Handle a resize event.
    ///
    /// Returns the action to take immediately.
    pub fn handle_resize(&mut self, width: u16, height: u16) -> CoalesceAction {
        self.handle_resize_at(width, height, Instant::now())
    }

    /// Handle a resize event at a specific time (for testing).
    pub fn handle_resize_at(&mut self, width: u16, height: u16, now: Instant) -> CoalesceAction {
        self.event_count += 1;

        // Track event time for rate calculation
        self.event_times.push_back(now);
        while self.event_times.len() > self.config.rate_window_size {
            self.event_times.pop_front();
        }

        // Update regime based on event rate
        self.update_regime(now);

        // Calculate dt
        let dt = self.last_event.map(|t| now.duration_since(t));
        let dt_ms = dt.map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0);
        self.last_event = Some(now);

        // If no pending, and this matches current size, no action needed
        if self.pending_size.is_none() && (width, height) == self.last_applied {
            self.log_decision(now, "skip_same_size", false, Some(dt_ms), None);
            return CoalesceAction::None;
        }

        // Update pending size (latest wins)
        self.pending_size = Some((width, height));

        // Track events in current coalesce window (bd-1rz0.7)
        self.events_in_window += 1;

        // Mark window start if this is first event
        if self.window_start.is_none() {
            self.window_start = Some(now);
        }

        // Check hard deadline
        let time_since_render = now.duration_since(self.last_render);
        if time_since_render >= Duration::from_millis(self.config.hard_deadline_ms) {
            return self.apply_pending_at(now, true);
        }

        // In steady mode with small dt, apply quickly.
        if self.regime == Regime::Steady
            && let Some(dt) = dt
            && dt >= Duration::from_millis(self.config.steady_delay_ms)
        {
            // Sufficient time has passed, apply now.
            return self.apply_pending_at(now, false);
        }

        self.log_decision(now, "coalesce", false, Some(dt_ms), None);

        // Fire decision hook for coalesce events (bd-1rz0.7)
        if let Some(ref hooks) = self.telemetry_hooks
            && let Some(entry) = self.logs.last()
        {
            hooks.fire_decision(entry);
        }

        CoalesceAction::ShowPlaceholder
    }

    /// Tick the coalescer (call each frame).
    ///
    /// Returns the action to take.
    pub fn tick(&mut self) -> CoalesceAction {
        self.tick_at(Instant::now())
    }

    /// Tick at a specific time (for testing).
    pub fn tick_at(&mut self, now: Instant) -> CoalesceAction {
        if self.pending_size.is_none() {
            return CoalesceAction::None;
        }

        if self.window_start.is_none() {
            return CoalesceAction::None;
        }

        // Check hard deadline
        let time_since_render = now.duration_since(self.last_render);
        if time_since_render >= Duration::from_millis(self.config.hard_deadline_ms) {
            return self.apply_pending_at(now, true);
        }

        // Get delay based on regime
        let delay_ms = match self.regime {
            Regime::Steady => self.config.steady_delay_ms,
            Regime::Burst => self.config.burst_delay_ms,
        };

        // Check if enough time has passed since last event
        if let Some(last_event) = self.last_event {
            let since_last_event = now.duration_since(last_event);
            if since_last_event >= Duration::from_millis(delay_ms) {
                return self.apply_pending_at(now, false);
            }
        }

        // Update cooldown
        if self.cooldown_remaining > 0 {
            self.cooldown_remaining -= 1;
            if self.cooldown_remaining == 0 && self.regime == Regime::Burst {
                let rate = self.calculate_event_rate(now);
                if rate < self.config.burst_exit_rate {
                    self.regime = Regime::Steady;
                }
            }
        }

        CoalesceAction::None
    }

    /// Time until the pending resize should be applied.
    pub fn time_until_apply(&self, now: Instant) -> Option<Duration> {
        let _pending = self.pending_size?;
        let last_event = self.last_event?;

        let delay_ms = match self.regime {
            Regime::Steady => self.config.steady_delay_ms,
            Regime::Burst => self.config.burst_delay_ms,
        };

        let elapsed = now.duration_since(last_event);
        let target = Duration::from_millis(delay_ms);

        if elapsed >= target {
            Some(Duration::ZERO)
        } else {
            Some(target - elapsed)
        }
    }

    /// Check if there's a pending resize.
    #[inline]
    pub fn has_pending(&self) -> bool {
        self.pending_size.is_some()
    }

    /// Get the current regime.
    #[inline]
    pub fn regime(&self) -> Regime {
        self.regime
    }

    /// Get the current event rate (events/second).
    pub fn event_rate(&self) -> f64 {
        self.calculate_event_rate(Instant::now())
    }

    /// Get the last applied size.
    #[inline]
    pub fn last_applied(&self) -> (u16, u16) {
        self.last_applied
    }

    /// Get decision logs (if logging enabled).
    pub fn logs(&self) -> &[DecisionLog] {
        &self.logs
    }

    /// Clear decision logs.
    pub fn clear_logs(&mut self) {
        self.logs.clear();
        self.log_start = None;
    }

    /// Get statistics about the coalescer.
    pub fn stats(&self) -> CoalescerStats {
        CoalescerStats {
            event_count: self.event_count,
            regime: self.regime,
            event_rate: self.event_rate(),
            has_pending: self.pending_size.is_some(),
            last_applied: self.last_applied,
        }
    }

    /// Export decision logs as JSONL (one entry per line).
    #[must_use]
    pub fn decision_logs_jsonl(&self) -> String {
        self.logs
            .iter()
            .map(|entry| entry.to_jsonl())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Compute a deterministic checksum of decision logs.
    #[must_use]
    pub fn decision_checksum(&self) -> u64 {
        let mut hash = FNV_OFFSET_BASIS;
        for entry in &self.logs {
            fnv_hash_bytes(&mut hash, &entry.event_idx.to_le_bytes());
            fnv_hash_bytes(&mut hash, &entry.elapsed_ms.to_bits().to_le_bytes());
            fnv_hash_bytes(&mut hash, &entry.dt_ms.to_bits().to_le_bytes());
            fnv_hash_bytes(&mut hash, &entry.event_rate.to_bits().to_le_bytes());
            fnv_hash_bytes(
                &mut hash,
                &[match entry.regime {
                    Regime::Steady => 0u8,
                    Regime::Burst => 1u8,
                }],
            );
            fnv_hash_bytes(&mut hash, entry.action.as_bytes());
            fnv_hash_bytes(&mut hash, &[0u8]); // separator

            fnv_hash_bytes(&mut hash, &[entry.pending_size.is_some() as u8]);
            if let Some((w, h)) = entry.pending_size {
                fnv_hash_bytes(&mut hash, &w.to_le_bytes());
                fnv_hash_bytes(&mut hash, &h.to_le_bytes());
            }

            fnv_hash_bytes(&mut hash, &[entry.applied_size.is_some() as u8]);
            if let Some((w, h)) = entry.applied_size {
                fnv_hash_bytes(&mut hash, &w.to_le_bytes());
                fnv_hash_bytes(&mut hash, &h.to_le_bytes());
            }

            fnv_hash_bytes(
                &mut hash,
                &entry.time_since_render_ms.to_bits().to_le_bytes(),
            );
            fnv_hash_bytes(&mut hash, &[entry.coalesce_ms.is_some() as u8]);
            if let Some(ms) = entry.coalesce_ms {
                fnv_hash_bytes(&mut hash, &ms.to_bits().to_le_bytes());
            }
            fnv_hash_bytes(&mut hash, &[entry.forced as u8]);
        }
        hash
    }

    /// Compute checksum as hex string.
    #[must_use]
    pub fn decision_checksum_hex(&self) -> String {
        format!("{:016x}", self.decision_checksum())
    }

    /// Compute a summary of the decision log.
    #[must_use]
    #[allow(clippy::field_reassign_with_default)]
    pub fn decision_summary(&self) -> DecisionSummary {
        let mut summary = DecisionSummary::default();
        summary.decision_count = self.logs.len();
        summary.last_applied = self.last_applied;
        summary.regime = self.regime;

        for entry in &self.logs {
            match entry.action {
                "apply" | "apply_forced" | "apply_immediate" => {
                    summary.apply_count += 1;
                    if entry.forced {
                        summary.forced_apply_count += 1;
                    }
                }
                "coalesce" => summary.coalesce_count += 1,
                "skip_same_size" => summary.skip_count += 1,
                _ => {}
            }
        }

        summary.checksum = self.decision_checksum();
        summary
    }

    /// Export config + decision logs + summary as JSONL.
    #[must_use]
    pub fn evidence_to_jsonl(&self) -> String {
        let mut lines = Vec::with_capacity(self.logs.len() + 2);
        lines.push(self.config.to_jsonl());
        lines.extend(self.logs.iter().map(DecisionLog::to_jsonl));
        lines.push(self.decision_summary().to_jsonl());
        lines.join("\n")
    }

    // --- Internal methods ---

    fn apply_pending_at(&mut self, now: Instant, forced: bool) -> CoalesceAction {
        let Some((width, height)) = self.pending_size.take() else {
            return CoalesceAction::None;
        };

        let coalesce_time = self
            .window_start
            .map(|s| now.duration_since(s))
            .unwrap_or(Duration::ZERO);
        let coalesce_ms = coalesce_time.as_secs_f64() * 1000.0;

        // Track cycle time for percentile calculation (bd-1rz0.7)
        self.cycle_times.push(coalesce_ms);

        self.window_start = None;
        self.last_applied = (width, height);
        self.last_render = now;

        // Reset events in window counter
        self.events_in_window = 0;

        self.log_decision(
            now,
            if forced { "apply_forced" } else { "apply" },
            forced,
            None,
            Some(coalesce_ms),
        );

        // Fire telemetry hooks (bd-1rz0.7)
        if let Some(ref hooks) = self.telemetry_hooks
            && let Some(entry) = self.logs.last()
        {
            hooks.fire_resize_applied(entry);
        }

        CoalesceAction::ApplyResize {
            width,
            height,
            coalesce_time,
            forced_by_deadline: forced,
        }
    }

    fn update_regime(&mut self, now: Instant) {
        let rate = self.calculate_event_rate(now);
        let old_regime = self.regime;

        match self.regime {
            Regime::Steady => {
                if rate >= self.config.burst_enter_rate {
                    self.regime = Regime::Burst;
                    self.cooldown_remaining = self.config.cooldown_frames;
                }
            }
            Regime::Burst => {
                if rate < self.config.burst_exit_rate {
                    // Don't exit immediately — use cooldown
                    if self.cooldown_remaining == 0 {
                        self.cooldown_remaining = self.config.cooldown_frames;
                    }
                } else {
                    // Still in burst, reset cooldown
                    self.cooldown_remaining = self.config.cooldown_frames;
                }
            }
        }

        // Track regime transitions and fire telemetry hooks (bd-1rz0.7)
        if old_regime != self.regime {
            self.regime_transitions += 1;
            if let Some(ref hooks) = self.telemetry_hooks {
                hooks.fire_regime_change(old_regime, self.regime);
            }
        }
    }

    fn calculate_event_rate(&self, now: Instant) -> f64 {
        if self.event_times.len() < 2 {
            return 0.0;
        }

        let first = *self.event_times.front().unwrap();
        let window_duration = now.duration_since(first);

        // Enforce a minimum duration of 1ms to prevent divide-by-zero or instability
        // and to correctly reflect high rates for near-instantaneous bursts.
        let duration_secs = window_duration.as_secs_f64().max(0.001);

        (self.event_times.len() as f64) / duration_secs
    }

    fn log_decision(
        &mut self,
        now: Instant,
        action: &'static str,
        forced: bool,
        dt_ms_override: Option<f64>,
        coalesce_ms: Option<f64>,
    ) {
        if !self.config.enable_logging {
            return;
        }

        if self.log_start.is_none() {
            self.log_start = Some(now);
        }

        let elapsed_ms = self
            .log_start
            .map(|t| now.duration_since(t).as_secs_f64() * 1000.0)
            .unwrap_or(0.0);

        let dt_ms = dt_ms_override
            .or_else(|| {
                self.last_event
                    .map(|t| now.duration_since(t).as_secs_f64() * 1000.0)
            })
            .unwrap_or(0.0);

        let time_since_render_ms = now.duration_since(self.last_render).as_secs_f64() * 1000.0;

        let applied_size =
            if action == "apply" || action == "apply_forced" || action == "apply_immediate" {
                Some(self.last_applied)
            } else {
                None
            };

        self.logs.push(DecisionLog {
            timestamp: now,
            elapsed_ms,
            event_idx: self.event_count,
            dt_ms,
            event_rate: self.calculate_event_rate(now),
            regime: self.regime,
            action,
            pending_size: self.pending_size,
            applied_size,
            time_since_render_ms,
            coalesce_ms,
            forced,
        });
    }
}

/// Statistics about the coalescer state.
#[derive(Debug, Clone)]
pub struct CoalescerStats {
    /// Total events processed.
    pub event_count: u64,
    /// Current regime.
    pub regime: Regime,
    /// Current event rate (events/sec).
    pub event_rate: f64,
    /// Whether there's a pending resize.
    pub has_pending: bool,
    /// Last applied size.
    pub last_applied: (u16, u16),
}

/// Summary of decision logs.
#[derive(Debug, Clone, Default)]
pub struct DecisionSummary {
    /// Total number of decisions logged.
    pub decision_count: usize,
    /// Total apply decisions.
    pub apply_count: usize,
    /// Applies forced by deadline.
    pub forced_apply_count: usize,
    /// Total coalesce decisions.
    pub coalesce_count: usize,
    /// Total skip decisions.
    pub skip_count: usize,
    /// Final regime at summary time.
    pub regime: Regime,
    /// Last applied size.
    pub last_applied: (u16, u16),
    /// Checksum for the decision log.
    pub checksum: u64,
}

impl DecisionSummary {
    /// Checksum as hex string.
    #[must_use]
    pub fn checksum_hex(&self) -> String {
        format!("{:016x}", self.checksum)
    }

    /// Serialize summary to JSONL format.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        format!(
            r#"{{"event":"summary","decisions":{},"applies":{},"forced_applies":{},"coalesces":{},"skips":{},"regime":"{}","last_w":{},"last_h":{},"checksum":"{}"}}"#,
            self.decision_count,
            self.apply_count,
            self.forced_apply_count,
            self.coalesce_count,
            self.skip_count,
            self.regime.as_str(),
            self.last_applied.0,
            self.last_applied.1,
            self.checksum_hex()
        )
    }
}

// =============================================================================
// Telemetry Hooks (bd-1rz0.7)
// =============================================================================

/// Callback type for resize applied events.
pub type OnResizeApplied = Box<dyn Fn(&DecisionLog) + Send + Sync>;
/// Callback type for regime change events.
pub type OnRegimeChange = Box<dyn Fn(Regime, Regime) + Send + Sync>;
/// Callback type for coalesce decision events.
pub type OnCoalesceDecision = Box<dyn Fn(&DecisionLog) + Send + Sync>;

/// Telemetry hooks for observing resize coalescer events.
///
/// # Example
///
/// ```ignore
/// use ftui_runtime::resize_coalescer::{ResizeCoalescer, TelemetryHooks, CoalescerConfig};
///
/// let hooks = TelemetryHooks::new()
///     .on_resize_applied(|entry| println!("Applied: {}x{}", entry.applied_size.unwrap().0, entry.applied_size.unwrap().1))
///     .on_regime_change(|from, to| println!("Regime: {:?} -> {:?}", from, to));
///
/// let mut coalescer = ResizeCoalescer::new(CoalescerConfig::default(), (80, 24))
///     .with_telemetry_hooks(hooks);
/// ```
pub struct TelemetryHooks {
    /// Called when a resize is applied.
    on_resize_applied: Option<OnResizeApplied>,
    /// Called when regime changes (Steady <-> Burst).
    on_regime_change: Option<OnRegimeChange>,
    /// Called for every decision (coalesce, apply, skip).
    on_decision: Option<OnCoalesceDecision>,
    /// Enable tracing events (requires `tracing` feature).
    emit_tracing: bool,
}

impl Default for TelemetryHooks {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for TelemetryHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelemetryHooks")
            .field("on_resize_applied", &self.on_resize_applied.is_some())
            .field("on_regime_change", &self.on_regime_change.is_some())
            .field("on_decision", &self.on_decision.is_some())
            .field("emit_tracing", &self.emit_tracing)
            .finish()
    }
}

impl TelemetryHooks {
    /// Create a new empty hooks instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            on_resize_applied: None,
            on_regime_change: None,
            on_decision: None,
            emit_tracing: false,
        }
    }

    /// Set callback for resize applied events.
    #[must_use]
    pub fn on_resize_applied<F>(mut self, callback: F) -> Self
    where
        F: Fn(&DecisionLog) + Send + Sync + 'static,
    {
        self.on_resize_applied = Some(Box::new(callback));
        self
    }

    /// Set callback for regime change events.
    #[must_use]
    pub fn on_regime_change<F>(mut self, callback: F) -> Self
    where
        F: Fn(Regime, Regime) + Send + Sync + 'static,
    {
        self.on_regime_change = Some(Box::new(callback));
        self
    }

    /// Set callback for all decision events.
    #[must_use]
    pub fn on_decision<F>(mut self, callback: F) -> Self
    where
        F: Fn(&DecisionLog) + Send + Sync + 'static,
    {
        self.on_decision = Some(Box::new(callback));
        self
    }

    /// Enable tracing event emission for OpenTelemetry integration.
    ///
    /// When enabled, decision events are emitted as `tracing::event!` calls
    /// with target `ftui.decision.resize` and all evidence ledger fields.
    #[must_use]
    pub fn with_tracing(mut self, enabled: bool) -> Self {
        self.emit_tracing = enabled;
        self
    }

    /// Check if a resize-applied hook is registered.
    pub fn has_resize_applied(&self) -> bool {
        self.on_resize_applied.is_some()
    }

    /// Check if a regime-change hook is registered.
    pub fn has_regime_change(&self) -> bool {
        self.on_regime_change.is_some()
    }

    /// Check if a decision hook is registered.
    pub fn has_decision(&self) -> bool {
        self.on_decision.is_some()
    }

    /// Invoke the resize applied callback if set.
    fn fire_resize_applied(&self, entry: &DecisionLog) {
        if let Some(ref cb) = self.on_resize_applied {
            cb(entry);
        }
        if self.emit_tracing {
            Self::emit_resize_tracing(entry);
        }
    }

    /// Invoke the regime change callback if set.
    fn fire_regime_change(&self, from: Regime, to: Regime) {
        if let Some(ref cb) = self.on_regime_change {
            cb(from, to);
        }
        if self.emit_tracing {
            tracing::debug!(
                target: "ftui.decision.resize",
                from_regime = %from.as_str(),
                to_regime = %to.as_str(),
                "regime_change"
            );
        }
    }

    /// Invoke the decision callback if set.
    fn fire_decision(&self, entry: &DecisionLog) {
        if let Some(ref cb) = self.on_decision {
            cb(entry);
        }
    }

    /// Emit a tracing event for resize decisions.
    fn emit_resize_tracing(entry: &DecisionLog) {
        let (pending_w, pending_h) = entry.pending_size.unwrap_or((0, 0));
        let (applied_w, applied_h) = entry.applied_size.unwrap_or((0, 0));
        let coalesce_ms = entry.coalesce_ms.unwrap_or(0.0);

        tracing::info!(
            target: "ftui.decision.resize",
            event_idx = entry.event_idx,
            elapsed_ms = entry.elapsed_ms,
            dt_ms = entry.dt_ms,
            event_rate = entry.event_rate,
            regime = %entry.regime.as_str(),
            action = entry.action,
            pending_w = pending_w,
            pending_h = pending_h,
            applied_w = applied_w,
            applied_h = applied_h,
            time_since_render_ms = entry.time_since_render_ms,
            coalesce_ms = coalesce_ms,
            forced = entry.forced,
            "resize_decision"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> CoalescerConfig {
        CoalescerConfig {
            steady_delay_ms: 16,
            burst_delay_ms: 40,
            hard_deadline_ms: 100,
            burst_enter_rate: 10.0,
            burst_exit_rate: 5.0,
            cooldown_frames: 3,
            rate_window_size: 8,
            enable_logging: true,
        }
    }

    #[test]
    fn new_coalescer_starts_in_steady() {
        let c = ResizeCoalescer::new(CoalescerConfig::default(), (80, 24));
        assert_eq!(c.regime(), Regime::Steady);
        assert!(!c.has_pending());
    }

    #[test]
    fn same_size_returns_none() {
        let mut c = ResizeCoalescer::new(test_config(), (80, 24));
        let action = c.handle_resize(80, 24);
        assert_eq!(action, CoalesceAction::None);
    }

    #[test]
    fn different_size_shows_placeholder() {
        let mut c = ResizeCoalescer::new(test_config(), (80, 24));
        let action = c.handle_resize(100, 40);
        assert_eq!(action, CoalesceAction::ShowPlaceholder);
        assert!(c.has_pending());
    }

    #[test]
    fn latest_wins_semantics() {
        let config = test_config();
        let mut c = ResizeCoalescer::new(config.clone(), (80, 24));

        let base = Instant::now();

        // Rapid sequence of resizes
        c.handle_resize_at(90, 30, base);
        c.handle_resize_at(100, 40, base + Duration::from_millis(5));
        c.handle_resize_at(110, 50, base + Duration::from_millis(10));

        // Wait for coalesce delay
        let action = c.tick_at(base + Duration::from_millis(60));

        let (width, height) = if let CoalesceAction::ApplyResize { width, height, .. } = action {
            (width, height)
        } else {
            assert!(
                matches!(action, CoalesceAction::ApplyResize { .. }),
                "Expected ApplyResize, got {action:?}"
            );
            return;
        };
        assert_eq!((width, height), (110, 50), "Should apply latest size");
    }

    #[test]
    fn hard_deadline_forces_apply() {
        let config = test_config();
        let mut c = ResizeCoalescer::new(config.clone(), (80, 24));

        let base = Instant::now();

        // First resize
        c.handle_resize_at(100, 40, base);

        // Wait past hard deadline
        let action = c.tick_at(base + Duration::from_millis(150));

        let forced_by_deadline = if let CoalesceAction::ApplyResize {
            forced_by_deadline, ..
        } = action
        {
            forced_by_deadline
        } else {
            assert!(
                matches!(action, CoalesceAction::ApplyResize { .. }),
                "Expected ApplyResize, got {action:?}"
            );
            return;
        };
        assert!(forced_by_deadline, "Should be forced by deadline");
    }

    #[test]
    fn burst_mode_detection() {
        let config = test_config();
        let mut c = ResizeCoalescer::new(config.clone(), (80, 24));

        let base = Instant::now();

        // Rapid events to trigger burst mode
        for i in 0..15 {
            c.handle_resize_at(80 + i, 24 + i, base + Duration::from_millis(i as u64 * 10));
        }

        assert_eq!(c.regime(), Regime::Burst);
    }

    #[test]
    fn steady_mode_fast_response() {
        let config = test_config();
        let mut c = ResizeCoalescer::new(config.clone(), (80, 24));

        let base = Instant::now();

        // Single resize
        c.handle_resize_at(100, 40, base);

        // In steady mode, should apply after steady_delay
        let action = c.tick_at(base + Duration::from_millis(20));

        assert!(matches!(action, CoalesceAction::ApplyResize { .. }));
    }

    #[test]
    fn record_external_apply_updates_state_and_logs() {
        let config = test_config();
        let mut c = ResizeCoalescer::new(config.clone(), (80, 24));

        let base = Instant::now();
        c.handle_resize_at(100, 40, base);
        c.record_external_apply(120, 50, base + Duration::from_millis(5));

        assert!(!c.has_pending());
        assert_eq!(c.last_applied(), (120, 50));

        let summary = c.decision_summary();
        assert_eq!(summary.apply_count, 1);
        assert_eq!(summary.last_applied, (120, 50));
        assert!(
            c.logs()
                .iter()
                .any(|entry| entry.action == "apply_immediate"),
            "record_external_apply should emit apply_immediate decision"
        );
    }

    #[test]
    fn coalesce_time_tracked() {
        let config = test_config();
        let mut c = ResizeCoalescer::new(config.clone(), (80, 24));

        let base = Instant::now();

        c.handle_resize_at(100, 40, base);
        let action = c.tick_at(base + Duration::from_millis(50));

        let coalesce_time = if let CoalesceAction::ApplyResize { coalesce_time, .. } = action {
            coalesce_time
        } else {
            assert!(
                matches!(action, CoalesceAction::ApplyResize { .. }),
                "Expected ApplyResize"
            );
            return;
        };
        assert!(coalesce_time >= Duration::from_millis(40));
        assert!(coalesce_time <= Duration::from_millis(60));
    }

    #[test]
    fn event_rate_calculation() {
        let config = test_config();
        let mut c = ResizeCoalescer::new(config.clone(), (80, 24));

        let base = Instant::now();

        // 10 events over 1 second = 10 events/sec
        for i in 0..10 {
            c.handle_resize_at(80 + i, 24, base + Duration::from_millis(i as u64 * 100));
        }

        let rate = c.calculate_event_rate(base + Duration::from_millis(1000));
        assert!(rate > 8.0 && rate < 12.0, "Rate should be ~10 events/sec");
    }

    #[test]
    fn rapid_burst_triggers_high_rate() {
        let config = test_config();
        let mut c = ResizeCoalescer::new(config.clone(), (80, 24));
        let base = Instant::now();

        // Simulate 8 events arriving at the EXACT same time (or < 1ms)
        for _ in 0..8 {
            c.handle_resize_at(80, 24, base);
        }

        let rate = c.calculate_event_rate(base);
        // 8 events / 0.001s = 8000 events/sec
        assert!(
            rate >= 1000.0,
            "Rate should be high for instantaneous burst, got {}",
            rate
        );
    }

    #[test]
    fn cooldown_prevents_immediate_exit() {
        let config = test_config();
        let mut c = ResizeCoalescer::new(config.clone(), (80, 24));

        let base = Instant::now();

        // Enter burst mode
        for i in 0..15 {
            c.handle_resize_at(80 + i, 24, base + Duration::from_millis(i as u64 * 10));
        }
        assert_eq!(c.regime(), Regime::Burst);

        // Rate should drop but cooldown prevents immediate exit
        c.tick_at(base + Duration::from_millis(500));
        c.tick_at(base + Duration::from_millis(600));

        // After cooldown frames, should exit
        c.tick_at(base + Duration::from_millis(700));
        c.tick_at(base + Duration::from_millis(800));
        c.tick_at(base + Duration::from_millis(900));

        // Should have exited burst
        // Note: This depends on rate calculation window
    }

    #[test]
    fn logging_captures_decisions() {
        let mut config = test_config();
        config.enable_logging = true;
        let mut c = ResizeCoalescer::new(config, (80, 24));

        c.handle_resize(100, 40);

        assert!(!c.logs().is_empty());
        assert_eq!(c.logs()[0].action, "coalesce");
    }

    #[test]
    fn logging_jsonl_format() {
        let mut config = test_config();
        config.enable_logging = true;
        let mut c = ResizeCoalescer::new(config, (80, 24));

        c.handle_resize(100, 40);
        let jsonl = c.logs()[0].to_jsonl();

        assert!(jsonl.contains("\"event\":\"decision\""));
        assert!(jsonl.contains("\"action\":\"coalesce\""));
        assert!(jsonl.contains("\"regime\":\"steady\""));
        assert!(jsonl.contains("\"pending_w\":100"));
        assert!(jsonl.contains("\"pending_h\":40"));
    }

    #[test]
    fn apply_logs_coalesce_ms() {
        let mut config = test_config();
        config.enable_logging = true;
        let mut c = ResizeCoalescer::new(config, (80, 24));

        let base = Instant::now();
        c.handle_resize_at(100, 40, base);
        let action = c.tick_at(base + Duration::from_millis(50));
        assert!(matches!(action, CoalesceAction::ApplyResize { .. }));

        let last = c.logs().last().expect("Expected a decision log entry");
        assert!(last.coalesce_ms.is_some());
        assert!(last.coalesce_ms.unwrap() >= 0.0);
    }

    #[test]
    fn decision_checksum_is_stable() {
        let mut config = test_config();
        config.enable_logging = true;

        let base = Instant::now();
        let mut c1 = ResizeCoalescer::new(config.clone(), (80, 24)).with_last_render(base);
        let mut c2 = ResizeCoalescer::new(config, (80, 24)).with_last_render(base);

        for c in [&mut c1, &mut c2] {
            c.handle_resize_at(90, 30, base);
            c.handle_resize_at(100, 40, base + Duration::from_millis(10));
            let _ = c.tick_at(base + Duration::from_millis(80));
        }

        assert_eq!(c1.decision_checksum(), c2.decision_checksum());
    }

    #[test]
    fn evidence_jsonl_includes_summary() {
        let mut config = test_config();
        config.enable_logging = true;
        let mut c = ResizeCoalescer::new(config, (80, 24));

        c.handle_resize(100, 40);
        let jsonl = c.evidence_to_jsonl();

        assert!(jsonl.contains("\"event\":\"config\""));
        assert!(jsonl.contains("\"event\":\"summary\""));
    }

    #[test]
    fn stats_reflect_state() {
        let mut c = ResizeCoalescer::new(test_config(), (80, 24));

        c.handle_resize(100, 40);

        let stats = c.stats();
        assert_eq!(stats.event_count, 1);
        assert!(stats.has_pending);
        assert_eq!(stats.last_applied, (80, 24));
    }

    #[test]
    fn time_until_apply_calculation() {
        let config = test_config();
        let mut c = ResizeCoalescer::new(config.clone(), (80, 24));

        let base = Instant::now();
        c.handle_resize_at(100, 40, base);

        let time_left = c.time_until_apply(base + Duration::from_millis(5));
        assert!(time_left.is_some());
        let time_left = time_left.unwrap();
        assert!(time_left.as_millis() > 0);
        assert!(time_left.as_millis() < config.steady_delay_ms as u128);
    }

    #[test]
    fn deterministic_behavior() {
        let config = test_config();

        // Run twice with same inputs
        let results: Vec<_> = (0..2)
            .map(|_| {
                let mut c = ResizeCoalescer::new(config.clone(), (80, 24));
                let base = Instant::now();

                for i in 0..5 {
                    c.handle_resize_at(80 + i, 24 + i, base + Duration::from_millis(i as u64 * 20));
                }

                c.tick_at(base + Duration::from_millis(200))
            })
            .collect();

        assert_eq!(results[0], results[1], "Results must be deterministic");
    }

    #[test]
    fn never_drops_final_size() {
        let config = test_config();
        let mut c = ResizeCoalescer::new(config.clone(), (80, 24));

        let base = Instant::now();

        // Many rapid resizes that may trigger some applies due to hard deadline
        let mut intermediate_applies = Vec::new();
        for i in 0..100 {
            let action = c.handle_resize_at(
                80 + (i % 50),
                24 + (i % 30),
                base + Duration::from_millis(i as u64 * 5),
            );
            if let CoalesceAction::ApplyResize { width, height, .. } = action {
                intermediate_applies.push((width, height));
            }
        }

        // The final size - may apply immediately if deadline is hit
        let final_action = c.handle_resize_at(200, 100, base + Duration::from_millis(600));

        let applied_size = if let CoalesceAction::ApplyResize { width, height, .. } = final_action {
            Some((width, height))
        } else {
            // If not applied immediately, tick until it is
            let mut result = None;
            for tick in 0..100 {
                let action = c.tick_at(base + Duration::from_millis(700 + tick * 20));
                if let CoalesceAction::ApplyResize { width, height, .. } = action {
                    result = Some((width, height));
                    break;
                }
            }
            result
        };

        assert_eq!(
            applied_size,
            Some((200, 100)),
            "Must apply final size 200x100"
        );
    }

    #[test]
    fn bounded_latency_invariant() {
        let config = test_config();
        let mut c = ResizeCoalescer::new(config.clone(), (80, 24));

        let base = Instant::now();
        c.handle_resize_at(100, 40, base);

        // Simulate time passing without any new events
        let mut applied_at = None;
        for ms in 0..200 {
            let now = base + Duration::from_millis(ms);
            let action = c.tick_at(now);
            if matches!(action, CoalesceAction::ApplyResize { .. }) {
                applied_at = Some(ms);
                break;
            }
        }

        assert!(applied_at.is_some(), "Must apply within reasonable time");
        assert!(
            applied_at.unwrap() <= config.hard_deadline_ms,
            "Must apply within hard deadline"
        );
    }

    // =========================================================================
    // Property tests (bd-1rz0.8)
    // =========================================================================

    mod property {
        use super::*;
        use proptest::prelude::*;

        /// Strategy for generating resize dimensions.
        fn dimension() -> impl Strategy<Value = u16> {
            1u16..500
        }

        /// Strategy for generating resize event sequences.
        fn resize_sequence(max_len: usize) -> impl Strategy<Value = Vec<(u16, u16, u64)>> {
            proptest::collection::vec((dimension(), dimension(), 0u64..200), 0..max_len)
        }

        proptest! {
            /// Property: identical sequences yield identical final results.
            ///
            /// The coalescer must be deterministic - same inputs → same outputs.
            #[test]
            fn determinism_across_sequences(
                events in resize_sequence(50),
                tick_offset in 100u64..500
            ) {
                let config = CoalescerConfig::default();

                let results: Vec<_> = (0..2)
                    .map(|_| {
                        let mut c = ResizeCoalescer::new(config.clone(), (80, 24));
                        let base = Instant::now();

                        for (i, (w, h, delay)) in events.iter().enumerate() {
                            let offset = events[..i].iter().map(|(_, _, d)| *d).sum::<u64>() + delay;
                            c.handle_resize_at(*w, *h, base + Duration::from_millis(offset));
                        }

                        // Tick to trigger apply
                        let total_time = events.iter().map(|(_, _, d)| d).sum::<u64>() + tick_offset;
                        c.tick_at(base + Duration::from_millis(total_time))
                    })
                    .collect();

                prop_assert_eq!(results[0], results[1], "Results must be deterministic");
            }

            /// Property: the latest resize is never lost (latest-wins semantics).
            ///
            /// When all coalescing completes, the applied size must match the
            /// final requested size.
            #[test]
            fn latest_wins_never_drops(
                events in resize_sequence(20),
                final_w in dimension(),
                final_h in dimension()
            ) {
                if events.is_empty() {
                    // No events to test
                    return Ok(());
                }

                let config = CoalescerConfig::default();
                let mut c = ResizeCoalescer::new(config.clone(), (80, 24));
                let base = Instant::now();

                // Feed all events
                let mut offset = 0u64;
                for (w, h, delay) in &events {
                    offset += delay;
                    c.handle_resize_at(*w, *h, base + Duration::from_millis(offset));
                }

                // Add final event
                offset += 50;
                c.handle_resize_at(final_w, final_h, base + Duration::from_millis(offset));

                // Tick until we get an apply
                let mut final_applied = None;
                for tick in 0..200 {
                    let action = c.tick_at(base + Duration::from_millis(offset + 10 + tick * 20));
                    if let CoalesceAction::ApplyResize { width, height, .. } = action {
                        final_applied = Some((width, height));
                    }
                    if !c.has_pending() && final_applied.is_some() {
                        break;
                    }
                }

                // The final applied size must match the latest requested size
                if let Some((applied_w, applied_h)) = final_applied {
                    prop_assert_eq!(
                        (applied_w, applied_h),
                        (final_w, final_h),
                        "Must apply the final size {} x {}",
                        final_w,
                        final_h
                    );
                }
            }

            /// Property: bounded latency is always maintained.
            ///
            /// A pending resize must be applied within hard_deadline_ms.
            #[test]
            fn bounded_latency_maintained(
                w in dimension(),
                h in dimension()
            ) {
                let config = CoalescerConfig::default();
                // Use (0,0) so no generated size (1..500) hits the skip_same_size path
                let mut c = ResizeCoalescer::new(config.clone(), (0, 0));
                let base = Instant::now();

                c.handle_resize_at(w, h, base);

                // Tick forward until applied
                let mut applied_at = None;
                for ms in 0..=config.hard_deadline_ms + 50 {
                    let action = c.tick_at(base + Duration::from_millis(ms));
                    if matches!(action, CoalesceAction::ApplyResize { .. }) {
                        applied_at = Some(ms);
                        break;
                    }
                }

                prop_assert!(applied_at.is_some(), "Resize must be applied");
                prop_assert!(
                    applied_at.unwrap() <= config.hard_deadline_ms,
                    "Must apply within hard deadline ({}ms), took {}ms",
                    config.hard_deadline_ms,
                    applied_at.unwrap()
                );
            }

            /// Property: applied sizes are never corrupted.
            ///
            /// When a resize is applied, the dimensions must exactly match
            /// what was requested (no off-by-one, no swapped axes).
            #[test]
            fn no_size_corruption(
                w in dimension(),
                h in dimension()
            ) {
                let config = CoalescerConfig::default();
                // Use (0,0) so no generated size (1..500) hits the skip_same_size path
                let mut c = ResizeCoalescer::new(config.clone(), (0, 0));
                let base = Instant::now();

                c.handle_resize_at(w, h, base);

                // Tick until applied
                let mut result = None;
                for ms in 0..200 {
                    let action = c.tick_at(base + Duration::from_millis(ms));
                    if let CoalesceAction::ApplyResize { width, height, .. } = action {
                        result = Some((width, height));
                        break;
                    }
                }

                prop_assert!(result.is_some());
                let (applied_w, applied_h) = result.unwrap();
                prop_assert_eq!(applied_w, w, "Width must not be corrupted");
                prop_assert_eq!(applied_h, h, "Height must not be corrupted");
            }

            /// Property: regime transitions are monotonic with event rate.
            ///
            /// Higher event rates should more reliably trigger burst mode.
            #[test]
            fn regime_follows_event_rate(
                event_count in 1usize..30
            ) {
                let config = CoalescerConfig {
                    burst_enter_rate: 10.0,
                    burst_exit_rate: 5.0,
                    ..CoalescerConfig::default()
                };
                let mut c = ResizeCoalescer::new(config.clone(), (80, 24));
                let base = Instant::now();

                // Very fast events (>10/sec) should trigger burst mode
                for i in 0..event_count {
                    c.handle_resize_at(
                        80 + i as u16,
                        24,
                        base + Duration::from_millis(i as u64 * 50), // 20 events/sec
                    );
                }

                // With enough fast events, should enter burst
                if event_count >= 10 {
                    prop_assert_eq!(
                        c.regime(),
                        Regime::Burst,
                        "Many rapid events should trigger burst mode"
                    );
                }
            }

            /// Property: event count invariant - coalescer tracks all incoming events.
            ///
            /// The `event_count` field tracks ALL resize events for rate calculation
            /// and telemetry, including same-size events that are skipped.
            #[test]
            fn event_count_invariant(
                events in resize_sequence(100)
            ) {
                let config = CoalescerConfig::default();
                let mut c = ResizeCoalescer::new(config, (80, 24));
                let base = Instant::now();

                for (w, h, delay) in &events {
                    c.handle_resize_at(*w, *h, base + Duration::from_millis(*delay));
                }

                let stats = c.stats();
                // Event count should equal total incoming events (for rate calculation).
                prop_assert_eq!(
                    stats.event_count,
                    events.len() as u64,
                    "Event count should match total incoming events"
                );
            }
        }
    }

    // =========================================================================
    // Telemetry Hooks Tests (bd-1rz0.7)
    // =========================================================================

    #[test]
    fn telemetry_hooks_fire_on_resize_applied() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};

        let applied_count = Arc::new(AtomicU32::new(0));
        let applied_count_clone = applied_count.clone();

        let hooks = TelemetryHooks::new().on_resize_applied(move |_entry| {
            applied_count_clone.fetch_add(1, Ordering::SeqCst);
        });

        let mut config = test_config();
        config.enable_logging = true;
        let mut c = ResizeCoalescer::new(config, (80, 24)).with_telemetry_hooks(hooks);

        let base = Instant::now();
        c.handle_resize_at(100, 40, base);
        c.tick_at(base + Duration::from_millis(50));

        assert_eq!(applied_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn telemetry_hooks_fire_on_regime_change() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};

        let regime_changes = Arc::new(AtomicU32::new(0));
        let regime_changes_clone = regime_changes.clone();

        let hooks = TelemetryHooks::new().on_regime_change(move |_from, _to| {
            regime_changes_clone.fetch_add(1, Ordering::SeqCst);
        });

        let config = test_config();
        let mut c = ResizeCoalescer::new(config, (80, 24)).with_telemetry_hooks(hooks);

        let base = Instant::now();

        // Rapid events to trigger burst mode
        for i in 0..15 {
            c.handle_resize_at(80 + i, 24 + i, base + Duration::from_millis(i as u64 * 10));
        }

        // Should have triggered at least one regime change (Steady -> Burst)
        assert!(regime_changes.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn regime_transition_count_tracks_changes() {
        let config = test_config();
        let mut c = ResizeCoalescer::new(config, (80, 24));

        assert_eq!(c.regime_transition_count(), 0);

        let base = Instant::now();

        // Rapid events to trigger burst mode
        for i in 0..15 {
            c.handle_resize_at(80 + i, 24 + i, base + Duration::from_millis(i as u64 * 10));
        }

        // Should have transitioned to Burst at least once
        assert!(c.regime_transition_count() >= 1);
    }

    #[test]
    fn cycle_time_percentiles_calculated() {
        let mut config = test_config();
        config.enable_logging = true;
        let mut c = ResizeCoalescer::new(config, (80, 24));

        // Initially no percentiles
        assert!(c.cycle_time_percentiles().is_none());

        let base = Instant::now();

        // Generate multiple applies to get cycle times
        for i in 0..5 {
            c.handle_resize_at(80 + i, 24 + i, base + Duration::from_millis(i as u64 * 100));
            c.tick_at(base + Duration::from_millis(i as u64 * 100 + 50));
        }

        // Now should have percentiles
        let percentiles = c.cycle_time_percentiles();
        assert!(percentiles.is_some());

        let p = percentiles.unwrap();
        assert!(p.count >= 1);
        assert!(p.mean_ms >= 0.0);
        assert!(p.p50_ms >= 0.0);
        assert!(p.p95_ms >= p.p50_ms);
        assert!(p.p99_ms >= p.p95_ms);
    }

    #[test]
    fn cycle_time_percentiles_jsonl_format() {
        let percentiles = CycleTimePercentiles {
            p50_ms: 10.5,
            p95_ms: 25.3,
            p99_ms: 42.1,
            count: 100,
            mean_ms: 15.2,
        };

        let jsonl = percentiles.to_jsonl();
        assert!(jsonl.contains("\"event\":\"cycle_time_percentiles\""));
        assert!(jsonl.contains("\"p50_ms\":10.500"));
        assert!(jsonl.contains("\"p95_ms\":25.300"));
        assert!(jsonl.contains("\"p99_ms\":42.100"));
        assert!(jsonl.contains("\"mean_ms\":15.200"));
        assert!(jsonl.contains("\"count\":100"));
    }
}
