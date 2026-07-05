#![forbid(unsafe_code)]

//! Effect system observability and Cx-aware execution helpers.
//!
//! This module provides:
//!
//! - **Cx-aware task execution**: `run_task_with_cx` wraps a closure with
//!   a `Cx` context for cooperative cancellation and deadline enforcement.
//! - **Tracing spans**: `effect.command` and `effect.subscription` spans
//!   with structured fields for observability dashboards.
//! - **Metrics counters**: `effects_executed_total` (by type) and
//!   `effect_duration_us` histogram approximation.
//!
//! # bd-37a.6: Command/Subscription effect system with Cx capability threading

use std::sync::atomic::{AtomicU64, Ordering};
use web_time::Instant;

// ---------------------------------------------------------------------------
// Monotonic counters
// ---------------------------------------------------------------------------

static EFFECTS_COMMAND_TOTAL: AtomicU64 = AtomicU64::new(0);
static EFFECTS_SUBSCRIPTION_TOTAL: AtomicU64 = AtomicU64::new(0);
static EFFECTS_QUEUE_ENQUEUED: AtomicU64 = AtomicU64::new(0);
static EFFECTS_QUEUE_PROCESSED: AtomicU64 = AtomicU64::new(0);
static EFFECTS_QUEUE_DROPPED: AtomicU64 = AtomicU64::new(0);
static EFFECTS_QUEUE_HIGH_WATER: AtomicU64 = AtomicU64::new(0);

/// Total command effects executed (monotonic counter).
#[must_use]
pub fn effects_command_total() -> u64 {
    EFFECTS_COMMAND_TOTAL.load(Ordering::Relaxed)
}

/// Total subscription effects started (monotonic counter).
#[must_use]
pub fn effects_subscription_total() -> u64 {
    EFFECTS_SUBSCRIPTION_TOTAL.load(Ordering::Relaxed)
}

/// Combined total of all effects executed.
#[must_use]
pub fn effects_executed_total() -> u64 {
    effects_command_total() + effects_subscription_total()
}

// ---------------------------------------------------------------------------
// Queue telemetry (bd-2zd0a)
// ---------------------------------------------------------------------------

/// Total tasks enqueued to the effect queue (monotonic counter).
#[must_use]
pub fn effects_queue_enqueued() -> u64 {
    EFFECTS_QUEUE_ENQUEUED.load(Ordering::Relaxed)
}

/// Total tasks processed by the effect queue (monotonic counter).
#[must_use]
pub fn effects_queue_processed() -> u64 {
    EFFECTS_QUEUE_PROCESSED.load(Ordering::Relaxed)
}

/// Total tasks dropped due to backpressure or shutdown (monotonic counter).
#[must_use]
pub fn effects_queue_dropped() -> u64 {
    EFFECTS_QUEUE_DROPPED.load(Ordering::Relaxed)
}

/// High-water mark: maximum queue depth observed (ratchet — only increases).
#[must_use]
pub fn effects_queue_high_water() -> u64 {
    EFFECTS_QUEUE_HIGH_WATER.load(Ordering::Relaxed)
}

/// Record a task enqueue, updating counters and high-water mark.
pub fn record_queue_enqueue(current_depth: u64) {
    EFFECTS_QUEUE_ENQUEUED.fetch_add(1, Ordering::Relaxed);
    // Ratchet high-water mark upward.
    let mut prev = EFFECTS_QUEUE_HIGH_WATER.load(Ordering::Relaxed);
    while current_depth > prev {
        match EFFECTS_QUEUE_HIGH_WATER.compare_exchange_weak(
            prev,
            current_depth,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(actual) => prev = actual,
        }
    }
}

/// Record a task processed by the effect queue.
pub fn record_queue_processed() {
    EFFECTS_QUEUE_PROCESSED.fetch_add(1, Ordering::Relaxed);
}

/// Record a task dropped due to backpressure or shutdown.
pub fn record_queue_drop(reason: &str) {
    EFFECTS_QUEUE_DROPPED.fetch_add(1, Ordering::Relaxed);
    tracing::warn!(
        target: "ftui.effect",
        reason = reason,
        monotonic.counter.effects_queue_dropped_total = 1_u64,
        "effect queue task dropped"
    );
}

/// Snapshot of queue telemetry for operator dashboards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueTelemetry {
    /// Total tasks enqueued (monotonic).
    pub enqueued: u64,
    /// Total tasks processed (monotonic).
    pub processed: u64,
    /// Total tasks dropped (monotonic).
    pub dropped: u64,
    /// Maximum queue depth observed.
    pub high_water: u64,
    /// Current in-flight: enqueued - processed. Drops are NOT subtracted:
    /// every drop path rejects a task *before* it is counted as enqueued, so
    /// subtracting `dropped` would permanently understate the backlog by the
    /// cumulative drop total.
    pub in_flight: u64,
}

/// Snapshot the current queue telemetry counters.
///
/// This is a lock-free, best-effort snapshot: the counters are loaded
/// independently, so under concurrent enqueue/process the derived `in_flight`
/// may transiently disagree with any single instantaneous state. The skew is
/// bounded and one-directional — `enqueued` is loaded first (the oldest value)
/// and `processed` last (the newest), so `in_flight` can only *under*estimate
/// the true backlog (floored at 0 by the saturating subtraction), never
/// overestimate it. That is the safe bias for the consumers (load-governor
/// pressure classification, backpressure), which read it per-frame and recover
/// on the next interval; do not treat it as an exact, linearizable count.
///
/// `dropped` is deliberately NOT subtracted from `in_flight`: drop paths
/// reject a task before it is ever counted as enqueued, so subtracting it
/// would double-penalize and permanently understate the backlog after any
/// overload episode.
#[must_use]
pub fn queue_telemetry() -> QueueTelemetry {
    let enqueued = effects_queue_enqueued();
    let processed = effects_queue_processed();
    let dropped = effects_queue_dropped();
    let in_flight = enqueued.saturating_sub(processed);
    QueueTelemetry {
        enqueued,
        processed,
        dropped,
        high_water: effects_queue_high_water(),
        in_flight,
    }
}

// ---------------------------------------------------------------------------
// Runtime dynamics instrumentation (bd-4flji)
//
// These metrics track the leading indicators of user-visible pain:
// subscription churn, shutdown latency, and reconcile frequency.
// ---------------------------------------------------------------------------

static SUBSCRIPTION_STARTS_TOTAL: AtomicU64 = AtomicU64::new(0);
static SUBSCRIPTION_STOPS_TOTAL: AtomicU64 = AtomicU64::new(0);
static SUBSCRIPTION_PANICS_TOTAL: AtomicU64 = AtomicU64::new(0);
static RECONCILE_COUNT: AtomicU64 = AtomicU64::new(0);
static RECONCILE_DURATION_US_TOTAL: AtomicU64 = AtomicU64::new(0);
static SHUTDOWN_DURATION_US_LAST: AtomicU64 = AtomicU64::new(0);
static SHUTDOWN_TIMED_OUT_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Total subscription starts (monotonic counter).
#[must_use]
pub fn subscription_starts_total() -> u64 {
    SUBSCRIPTION_STARTS_TOTAL.load(Ordering::Relaxed)
}

/// Total subscription stops (monotonic counter).
#[must_use]
pub fn subscription_stops_total() -> u64 {
    SUBSCRIPTION_STOPS_TOTAL.load(Ordering::Relaxed)
}

/// Total subscription panics caught (monotonic counter).
#[must_use]
pub fn subscription_panics_total() -> u64 {
    SUBSCRIPTION_PANICS_TOTAL.load(Ordering::Relaxed)
}

/// Total reconcile operations (monotonic counter).
#[must_use]
pub fn reconcile_count() -> u64 {
    RECONCILE_COUNT.load(Ordering::Relaxed)
}

/// Cumulative reconcile duration in microseconds.
#[must_use]
pub fn reconcile_duration_us_total() -> u64 {
    RECONCILE_DURATION_US_TOTAL.load(Ordering::Relaxed)
}

/// Most recent shutdown duration in microseconds (0 = no shutdown yet).
#[must_use]
pub fn shutdown_duration_us_last() -> u64 {
    SHUTDOWN_DURATION_US_LAST.load(Ordering::Relaxed)
}

/// Total subscription join timeouts during shutdown (monotonic counter).
#[must_use]
pub fn shutdown_timed_out_total() -> u64 {
    SHUTDOWN_TIMED_OUT_TOTAL.load(Ordering::Relaxed)
}

/// Record a subscription start event.
pub fn record_dynamics_sub_start() {
    SUBSCRIPTION_STARTS_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// Record a subscription stop event.
pub fn record_dynamics_sub_stop() {
    SUBSCRIPTION_STOPS_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// Record a subscription panic event.
pub fn record_dynamics_sub_panic() {
    SUBSCRIPTION_PANICS_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// Record a reconcile operation with its duration.
pub fn record_dynamics_reconcile(duration_us: u64) {
    RECONCILE_COUNT.fetch_add(1, Ordering::Relaxed);
    RECONCILE_DURATION_US_TOTAL.fetch_add(duration_us, Ordering::Relaxed);
}

/// Record a shutdown completion with its duration and timeout count.
pub fn record_dynamics_shutdown(duration_us: u64, timed_out: u64) {
    SHUTDOWN_DURATION_US_LAST.store(duration_us, Ordering::Relaxed);
    SHUTDOWN_TIMED_OUT_TOTAL.fetch_add(timed_out, Ordering::Relaxed);
}

/// Snapshot of runtime dynamics for operator dashboards and performance analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeDynamics {
    /// Total subscription starts.
    pub sub_starts: u64,
    /// Total subscription stops.
    pub sub_stops: u64,
    /// Total subscription panics caught.
    pub sub_panics: u64,
    /// Current subscription churn: starts - stops.
    pub sub_active_estimate: u64,
    /// Total reconcile operations.
    pub reconciles: u64,
    /// Average reconcile duration in microseconds (0 if no reconciles yet).
    pub reconcile_avg_us: u64,
    /// Most recent shutdown duration in microseconds.
    pub shutdown_last_us: u64,
    /// Total join timeouts during shutdowns.
    pub shutdown_timeouts: u64,
}

/// Snapshot the current runtime dynamics counters.
#[must_use]
pub fn runtime_dynamics() -> RuntimeDynamics {
    let sub_starts = subscription_starts_total();
    let sub_stops = subscription_stops_total();
    let reconciles = reconcile_count();
    let reconcile_total_us = reconcile_duration_us_total();
    RuntimeDynamics {
        sub_starts,
        sub_stops,
        sub_panics: subscription_panics_total(),
        sub_active_estimate: sub_starts.saturating_sub(sub_stops),
        reconciles,
        reconcile_avg_us: reconcile_total_us.checked_div(reconciles).unwrap_or(0),
        shutdown_last_us: shutdown_duration_us_last(),
        shutdown_timeouts: shutdown_timed_out_total(),
    }
}

// ---------------------------------------------------------------------------
// Command effect instrumentation
// ---------------------------------------------------------------------------

/// Execute a command effect with tracing instrumentation.
///
/// Wraps command execution with an `effect.command` span recording
/// `command_type`, `duration_us`, and `result`.
pub fn trace_command_effect<F, R>(command_type: &str, f: F) -> R
where
    F: FnOnce() -> R,
{
    EFFECTS_COMMAND_TOTAL.fetch_add(1, Ordering::Relaxed);

    let start = Instant::now();
    let _span = tracing::debug_span!(
        "effect.command",
        command_type = %command_type,
        duration_us = tracing::field::Empty,
        result = tracing::field::Empty,
    )
    .entered();

    tracing::debug!(
        target: "ftui.effect",
        command_type = %command_type,
        "command effect started"
    );

    let result = f();
    let duration_us = start.elapsed().as_micros() as u64;

    tracing::debug!(
        target: "ftui.effect",
        command_type = %command_type,
        duration_us = duration_us,
        effect_duration_us = duration_us,
        "command effect completed"
    );

    result
}

/// Record a command effect execution without wrapping (for inline instrumentation).
pub fn record_command_effect(command_type: &str, duration_us: u64) {
    EFFECTS_COMMAND_TOTAL.fetch_add(1, Ordering::Relaxed);

    let _span = tracing::debug_span!(
        "effect.command",
        command_type = %command_type,
        duration_us = duration_us,
        result = "ok",
    )
    .entered();

    tracing::debug!(
        target: "ftui.effect",
        command_type = %command_type,
        duration_us = duration_us,
        effect_duration_us = duration_us,
        "command effect recorded"
    );
}

// ---------------------------------------------------------------------------
// Subscription effect instrumentation
// ---------------------------------------------------------------------------

/// Record a subscription lifecycle event.
pub fn record_subscription_start(sub_type: &str, sub_id: u64) {
    EFFECTS_SUBSCRIPTION_TOTAL.fetch_add(1, Ordering::Relaxed);

    let _span = tracing::debug_span!(
        "effect.subscription",
        sub_type = %sub_type,
        event_count = 0u64,
        active = true,
    )
    .entered();

    tracing::debug!(
        target: "ftui.effect",
        sub_type = %sub_type,
        sub_id = sub_id,
        active = true,
        "subscription started"
    );
}

/// Record a subscription stop event.
pub fn record_subscription_stop(sub_type: &str, sub_id: u64, event_count: u64) {
    let _span = tracing::debug_span!(
        "effect.subscription",
        sub_type = %sub_type,
        event_count = event_count,
        active = false,
    )
    .entered();

    tracing::debug!(
        target: "ftui.effect",
        sub_type = %sub_type,
        sub_id = sub_id,
        event_count = event_count,
        active = false,
        "subscription stopped"
    );
}

/// Record an effect timeout warning.
pub fn warn_effect_timeout(effect_type: &str, deadline_us: u64) {
    tracing::warn!(
        target: "ftui.effect",
        effect_type = %effect_type,
        deadline_us = deadline_us,
        "effect timeout exceeded deadline"
    );
}

/// Record an effect panic error.
pub fn error_effect_panic(effect_type: &str, panic_msg: &str) {
    tracing::error!(
        target: "ftui.effect",
        effect_type = %effect_type,
        panic_msg = %panic_msg,
        "effect panicked during execution"
    );
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::registry::LookupSpan;

    // Tracing capture infrastructure
    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    struct CapturedSpan {
        name: String,
        fields: HashMap<String, String>,
    }

    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    struct CapturedEvent {
        level: tracing::Level,
        target: String,
        fields: HashMap<String, String>,
    }

    struct SpanCapture {
        spans: Arc<Mutex<Vec<CapturedSpan>>>,
        events: Arc<Mutex<Vec<CapturedEvent>>>,
    }

    impl SpanCapture {
        fn new() -> (Self, CaptureHandle) {
            let spans = Arc::new(Mutex::new(Vec::new()));
            let events = Arc::new(Mutex::new(Vec::new()));
            let handle = CaptureHandle {
                spans: spans.clone(),
                events: events.clone(),
            };
            (Self { spans, events }, handle)
        }
    }

    struct CaptureHandle {
        spans: Arc<Mutex<Vec<CapturedSpan>>>,
        events: Arc<Mutex<Vec<CapturedEvent>>>,
    }

    impl CaptureHandle {
        fn spans(&self) -> Vec<CapturedSpan> {
            self.spans.lock().unwrap().clone()
        }

        fn events(&self) -> Vec<CapturedEvent> {
            self.events.lock().unwrap().clone()
        }
    }

    struct FieldVisitor(Vec<(String, String)>);

    impl tracing::field::Visit for FieldVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0
                .push((field.name().to_string(), format!("{value:?}")));
        }
        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            self.0.push((field.name().to_string(), value.to_string()));
        }
        fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
            self.0.push((field.name().to_string(), value.to_string()));
        }
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.push((field.name().to_string(), value.to_string()));
        }
        fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
            self.0.push((field.name().to_string(), value.to_string()));
        }
    }

    impl<S> tracing_subscriber::Layer<S> for SpanCapture
    where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            _id: &tracing::span::Id,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut visitor = FieldVisitor(Vec::new());
            attrs.record(&mut visitor);
            let mut fields: HashMap<String, String> = visitor.0.into_iter().collect();
            for field in attrs.metadata().fields() {
                fields.entry(field.name().to_string()).or_default();
            }
            self.spans.lock().unwrap().push(CapturedSpan {
                name: attrs.metadata().name().to_string(),
                fields,
            });
        }

        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut visitor = FieldVisitor(Vec::new());
            event.record(&mut visitor);
            let fields: HashMap<String, String> = visitor.0.into_iter().collect();
            self.events.lock().unwrap().push(CapturedEvent {
                level: *event.metadata().level(),
                target: event.metadata().target().to_string(),
                fields,
            });
        }
    }

    fn with_captured_tracing<F>(f: F) -> CaptureHandle
    where
        F: FnOnce(),
    {
        let (layer, handle) = SpanCapture::new();
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, f);
        handle
    }

    // =====================================================================
    // Command effect tests
    // =====================================================================

    #[test]
    fn trace_command_effect_emits_span() {
        let handle = with_captured_tracing(|| {
            trace_command_effect("task", || 42);
        });

        let spans = handle.spans();
        let cmd_spans: Vec<_> = spans
            .iter()
            .filter(|s| s.name == "effect.command")
            .collect();
        assert!(!cmd_spans.is_empty(), "expected effect.command span");
        assert!(cmd_spans[0].fields.contains_key("command_type"));
    }

    #[test]
    fn trace_command_effect_returns_value() {
        let result = trace_command_effect("test", || 42);
        assert_eq!(result, 42);
    }

    #[test]
    fn trace_command_effect_debug_events() {
        let handle = with_captured_tracing(|| {
            trace_command_effect("file_io", || {});
        });

        let events = handle.events();
        let start_events: Vec<_> = events
            .iter()
            .filter(|e| {
                e.target == "ftui.effect"
                    && e.fields
                        .get("message")
                        .is_some_and(|m| m.contains("started"))
            })
            .collect();
        assert!(!start_events.is_empty(), "expected start event");

        let complete_events: Vec<_> = events
            .iter()
            .filter(|e| {
                e.target == "ftui.effect"
                    && e.fields
                        .get("message")
                        .is_some_and(|m| m.contains("completed"))
            })
            .collect();
        assert!(!complete_events.is_empty(), "expected complete event");

        let evt = &complete_events[0];
        assert!(
            evt.fields.contains_key("duration_us"),
            "missing duration_us"
        );
        assert!(
            evt.fields.contains_key("effect_duration_us"),
            "missing effect_duration_us histogram"
        );
    }

    #[test]
    fn record_command_effect_emits_span() {
        let handle = with_captured_tracing(|| {
            record_command_effect("clipboard", 150);
        });

        let spans = handle.spans();
        let cmd_spans: Vec<_> = spans
            .iter()
            .filter(|s| s.name == "effect.command")
            .collect();
        assert!(!cmd_spans.is_empty());
        assert_eq!(
            cmd_spans[0].fields.get("command_type").unwrap(),
            "clipboard"
        );
    }

    // =====================================================================
    // Subscription effect tests
    // =====================================================================

    #[test]
    fn record_subscription_start_emits_span() {
        let handle = with_captured_tracing(|| {
            record_subscription_start("timer", 42);
        });

        let spans = handle.spans();
        let sub_spans: Vec<_> = spans
            .iter()
            .filter(|s| s.name == "effect.subscription")
            .collect();
        assert!(!sub_spans.is_empty(), "expected effect.subscription span");
        assert!(sub_spans[0].fields.contains_key("sub_type"));
        assert!(sub_spans[0].fields.contains_key("active"));
    }

    #[test]
    fn record_subscription_stop_emits_span() {
        let handle = with_captured_tracing(|| {
            record_subscription_stop("keyboard", 7, 100);
        });

        let spans = handle.spans();
        let sub_spans: Vec<_> = spans
            .iter()
            .filter(|s| s.name == "effect.subscription")
            .collect();
        assert!(!sub_spans.is_empty());
        assert!(sub_spans[0].fields.contains_key("event_count"));
    }

    // =====================================================================
    // Warning/error log tests
    // =====================================================================

    #[test]
    fn warn_effect_timeout_emits_warn_event() {
        let handle = with_captured_tracing(|| {
            warn_effect_timeout("task", 500_000);
        });

        let events = handle.events();
        let warn_events: Vec<_> = events
            .iter()
            .filter(|e| e.level == tracing::Level::WARN && e.target == "ftui.effect")
            .collect();
        assert!(!warn_events.is_empty(), "expected WARN event for timeout");
    }

    #[test]
    fn error_effect_panic_emits_error_event() {
        let handle = with_captured_tracing(|| {
            error_effect_panic("subscription", "thread panicked");
        });

        let events = handle.events();
        let error_events: Vec<_> = events
            .iter()
            .filter(|e| e.level == tracing::Level::ERROR && e.target == "ftui.effect")
            .collect();
        assert!(!error_events.is_empty(), "expected ERROR event for panic");
    }

    // =====================================================================
    // Counter tests
    // =====================================================================

    #[test]
    fn counter_accessors_callable() {
        let cmd = effects_command_total();
        let sub = effects_subscription_total();
        let total = effects_executed_total();
        assert_eq!(total, cmd + sub);
    }

    #[test]
    fn counters_increment_on_command() {
        let before = effects_command_total();
        trace_command_effect("test", || {});
        let after = effects_command_total();
        assert!(
            after > before,
            "command counter should increment: {before} → {after}"
        );
    }

    #[test]
    fn counters_increment_on_subscription() {
        let before = effects_subscription_total();
        record_subscription_start("test", 1);
        let after = effects_subscription_total();
        assert!(
            after > before,
            "subscription counter should increment: {before} → {after}"
        );
    }

    // =========================================================================
    // Queue telemetry tests (bd-2zd0a)
    // =========================================================================

    #[test]
    fn queue_enqueue_increments_counter() {
        let before = effects_queue_enqueued();
        record_queue_enqueue(1);
        let after = effects_queue_enqueued();
        assert!(after > before, "enqueued counter should increment");
    }

    #[test]
    fn queue_processed_increments_counter() {
        let before = effects_queue_processed();
        record_queue_processed();
        let after = effects_queue_processed();
        assert!(after > before, "processed counter should increment");
    }

    #[test]
    fn queue_drop_increments_counter() {
        let before = effects_queue_dropped();
        record_queue_drop("test");
        let after = effects_queue_dropped();
        assert!(after > before, "dropped counter should increment");
    }

    #[test]
    fn queue_high_water_ratchets_upward() {
        let before = effects_queue_high_water();
        let new_mark = before + 100;
        record_queue_enqueue(new_mark);
        assert!(
            effects_queue_high_water() >= new_mark,
            "high-water should ratchet to at least {new_mark}"
        );
        // Lower value should NOT reduce the high-water mark
        record_queue_enqueue(1);
        assert!(
            effects_queue_high_water() >= new_mark,
            "high-water should not decrease"
        );
    }

    #[test]
    fn queue_telemetry_snapshot_consistent() {
        let snap = queue_telemetry();
        // in_flight = enqueued - processed (saturating). Drops are rejected
        // before being counted as enqueued, so they must NOT be subtracted —
        // doing so would permanently understate the backlog after any
        // overload episode.
        assert_eq!(
            snap.in_flight,
            snap.enqueued.saturating_sub(snap.processed),
            "in_flight should be enqueued - processed"
        );
    }

    // =========================================================================
    // Runtime dynamics tests (bd-4flji)
    // =========================================================================

    #[test]
    fn dynamics_sub_start_increments() {
        let before = subscription_starts_total();
        record_dynamics_sub_start();
        let after = subscription_starts_total();
        assert!(after > before);
    }

    #[test]
    fn dynamics_sub_stop_increments() {
        let before = subscription_stops_total();
        record_dynamics_sub_stop();
        let after = subscription_stops_total();
        assert!(after > before);
    }

    #[test]
    fn dynamics_sub_panic_increments() {
        let before = subscription_panics_total();
        record_dynamics_sub_panic();
        let after = subscription_panics_total();
        assert!(after > before);
    }

    #[test]
    fn dynamics_reconcile_records_count_and_duration() {
        let before_count = reconcile_count();
        let before_dur = reconcile_duration_us_total();
        record_dynamics_reconcile(500);
        assert!(reconcile_count() > before_count);
        assert!(reconcile_duration_us_total() >= before_dur + 500);
    }

    #[test]
    fn dynamics_shutdown_records_duration() {
        record_dynamics_shutdown(1234, 2);
        assert_eq!(shutdown_duration_us_last(), 1234);
        let timeouts = shutdown_timed_out_total();
        assert!(timeouts >= 2);
    }

    #[test]
    fn dynamics_snapshot_consistent() {
        let snap = runtime_dynamics();
        assert_eq!(
            snap.sub_active_estimate,
            snap.sub_starts.saturating_sub(snap.sub_stops),
            "active estimate = starts - stops"
        );
        if snap.reconciles > 0 {
            assert!(
                snap.reconcile_avg_us > 0 || reconcile_duration_us_total() == 0,
                "avg should be > 0 when reconciles happened with non-zero duration"
            );
        }
    }
}
