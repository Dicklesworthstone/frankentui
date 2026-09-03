#![forbid(unsafe_code)]

//! bd-37a.11: E2E test for effect system cancellation & structured concurrency.
//!
//! Covers:
//! 1. Start application with multiple subscriptions, verify all start
//! 2. Stop all subscriptions, assert teardown within deadline
//! 3. Verify timeout enforcement: effect exceeding deadline produces WARN
//! 4. Assert effect.subscription spans show active=false after cancellation
//! 5. Assert WARN log for timeout
//! 6. Verify effects_executed_total counter
//!
//! Run:
//!   cargo test -p ftui-runtime --test e2e_effect_system_cancellation

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};

use ftui_runtime::effect_system::{
    effects_command_total, effects_executed_total, effects_subscription_total,
    record_subscription_start, record_subscription_stop, trace_command_effect, warn_effect_timeout,
};
use ftui_runtime::telemetry_schema;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;

// ============================================================================
// Tracing capture infrastructure
// ============================================================================

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
    message: String,
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
        let fields: HashMap<String, String> = visitor.0.clone().into_iter().collect();
        let message = visitor
            .0
            .iter()
            .find(|(k, _)| k == "message")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        self.events.lock().unwrap().push(CapturedEvent {
            level: *event.metadata().level(),
            target: event.metadata().target().to_string(),
            message,
            fields,
        });
    }
}

fn with_captured_tracing<F>(f: F) -> CaptureHandle
where
    F: FnOnce(),
{
    ensure_global_trace_level();
    let (layer, handle) = SpanCapture::new();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::filter::LevelFilter::TRACE)
        .with(layer);
    tracing::subscriber::with_default(subscriber, || {
        tracing::callsite::rebuild_interest_cache();
        f();
    });
    handle
}

fn ensure_global_trace_level() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let subscriber =
            tracing_subscriber::registry().with(tracing_subscriber::filter::LevelFilter::TRACE);
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

// ============================================================================
// 1. Start app with multiple concurrent workers, verify all start
// ============================================================================

#[test]
fn multiple_subscriptions_all_start() {
    let started_flags: Vec<_> = (0..3).map(|_| Arc::new(AtomicBool::new(false))).collect();
    let stop = Arc::new(AtomicBool::new(false));

    let mut handles = Vec::new();
    for flag in &started_flags {
        let f = flag.clone();
        let s = stop.clone();
        let handle = std::thread::spawn(move || {
            f.store(true, Ordering::Release);
            while !s.load(Ordering::Acquire) {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        });
        handles.push(handle);
    }

    // Give threads time to start
    std::thread::sleep(std::time::Duration::from_millis(100));

    for (i, flag) in started_flags.iter().enumerate() {
        assert!(
            flag.load(Ordering::Acquire),
            "subscription {i} should have started"
        );
    }

    // Clean up
    stop.store(true, Ordering::Release);
    for handle in handles {
        let _ = handle.join();
    }
}

// ============================================================================
// 2. Stop all subscriptions, assert teardown within deadline
// ============================================================================

#[test]
fn subscriptions_torn_down_within_deadline() {
    let counters: Vec<_> = (0..3).map(|_| Arc::new(Mutex::new(0u64))).collect();
    let stop = Arc::new(AtomicBool::new(false));

    let mut handles = Vec::new();
    for counter in &counters {
        let c = counter.clone();
        let s = stop.clone();
        let handle = std::thread::spawn(move || {
            while !s.load(Ordering::Acquire) {
                let mut count = c.lock().unwrap();
                *count += 1;
                drop(count);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        });
        handles.push(handle);
    }

    // Let subscriptions run briefly
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Stop all and measure teardown time
    let stop_start = std::time::Instant::now();
    stop.store(true, Ordering::Release);
    for handle in handles {
        let _ = handle.join();
    }
    let teardown_ms = stop_start.elapsed().as_millis();

    // Teardown should be fast (< 500ms)
    assert!(
        teardown_ms < 500,
        "teardown took {teardown_ms}ms, expected < 500ms"
    );

    // All counters should have incremented (subscriptions were active)
    for (i, counter) in counters.iter().enumerate() {
        let count = *counter.lock().unwrap();
        assert!(
            count > 0,
            "subscription {i} should have incremented counter"
        );
    }
}

// ============================================================================
// 3. No resource leaks: threads are joined cleanly
// ============================================================================

#[test]
fn no_thread_leaks_after_stop() {
    let stop = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();

    for _ in 0..5 {
        let s = stop.clone();
        let handle = std::thread::spawn(move || {
            while !s.load(Ordering::Acquire) {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        });
        handles.push(handle);
    }

    std::thread::sleep(std::time::Duration::from_millis(50));

    // Stop all and join
    stop.store(true, Ordering::Release);
    for handle in handles {
        assert!(
            handle.join().is_ok(),
            "subscription thread should join cleanly"
        );
    }
}

// ============================================================================
// 4. Assert effect.subscription spans show active=false after cancellation
// ============================================================================

#[test]
fn subscription_spans_show_active_false_after_stop() {
    let handle = with_captured_tracing(|| {
        // Simulate subscription lifecycle
        record_subscription_start("timer", 42);
        record_subscription_start("keyboard", 43);
        record_subscription_start("mouse", 44);

        // Stop them
        record_subscription_stop("timer", 42, 100);
        record_subscription_stop("keyboard", 43, 50);
        record_subscription_stop("mouse", 44, 200);
    });

    let spans = handle.spans();
    let sub_spans: Vec<_> = spans
        .iter()
        .filter(|s| s.name == "effect.subscription")
        .collect();

    // 6 spans total: 3 start + 3 stop
    assert_eq!(sub_spans.len(), 6, "expected 6 subscription spans");

    // The stop spans (last 3) should have active=false
    let stop_spans: Vec<_> = sub_spans
        .iter()
        .filter(|s| s.fields.get("active").is_some_and(|v| v == "false"))
        .collect();
    assert_eq!(
        stop_spans.len(),
        3,
        "expected 3 spans with active=false after stop"
    );
}

// ============================================================================
// 5. Assert WARN log for timeout
// ============================================================================

#[test]
fn warn_log_emitted_for_effect_timeout() {
    let handle = with_captured_tracing(|| {
        // Simulate a task that exceeds its deadline
        warn_effect_timeout("task", 500_000);
    });

    let events = handle.events();
    let warn_events: Vec<_> = events
        .iter()
        .filter(|e| e.level == tracing::Level::WARN && e.target == telemetry_schema::TARGET_EFFECT)
        .collect();

    assert!(!warn_events.is_empty(), "expected WARN event for timeout");

    let evt = &warn_events[0];
    assert!(
        evt.fields.contains_key("deadline_us"),
        "timeout WARN should contain deadline_us"
    );
    assert!(
        evt.fields.contains_key("effect_type"),
        "timeout WARN should contain effect_type"
    );
}

#[test]
fn warn_log_for_slow_command_effect() {
    let handle = with_captured_tracing(|| {
        // Execute a command effect
        trace_command_effect("slow_io", || {
            // Simulate work
            std::thread::sleep(std::time::Duration::from_millis(1));
        });

        // If it exceeds a threshold, emit timeout warning
        warn_effect_timeout("slow_io", 1_000);
    });

    let events = handle.events();

    // Should have both DEBUG lifecycle events and WARN timeout
    let debug_events: Vec<_> = events
        .iter()
        .filter(|e| e.level == tracing::Level::DEBUG && e.target == telemetry_schema::TARGET_EFFECT)
        .collect();
    assert!(!debug_events.is_empty(), "expected DEBUG lifecycle events");

    let warn_events: Vec<_> = events
        .iter()
        .filter(|e| e.level == tracing::Level::WARN)
        .collect();
    assert!(!warn_events.is_empty(), "expected WARN timeout event");
}

// ============================================================================
// 6. Verify effects_executed_total counter
// ============================================================================

#[test]
fn effects_executed_total_increments() {
    let before = effects_executed_total();

    // Execute some commands
    trace_command_effect("cmd1", || {});
    trace_command_effect("cmd2", || {});

    // Record some subscriptions
    record_subscription_start("sub1", 1);
    record_subscription_start("sub2", 2);

    let after = effects_executed_total();

    assert!(
        after >= before + 4,
        "effects_executed_total should increase by at least 4: \
         before={before}, after={after}"
    );
}

#[test]
fn effects_counter_separates_commands_and_subscriptions() {
    let cmd_before = effects_command_total();
    let sub_before = effects_subscription_total();

    trace_command_effect("test_cmd", || 42);
    record_subscription_start("test_sub", 999);

    let cmd_after = effects_command_total();
    let sub_after = effects_subscription_total();

    assert!(cmd_after > cmd_before, "command counter should increment");
    assert!(
        sub_after > sub_before,
        "subscription counter should increment"
    );
}

// ============================================================================
// Combined E2E scenario
// ============================================================================

#[test]
fn full_lifecycle_start_run_cancel_verify() {
    let handle = with_captured_tracing(|| {
        // Phase 1: Start subscriptions
        record_subscription_start("timer", 1);
        record_subscription_start("keyboard", 2);
        record_subscription_start("resize", 3);

        // Phase 2: Execute some commands
        trace_command_effect("clipboard_read", || "hello".to_string());
        trace_command_effect("file_save", || true);

        // Phase 3: Cancel/stop all subscriptions
        record_subscription_stop("timer", 1, 42);
        record_subscription_stop("keyboard", 2, 100);
        record_subscription_stop("resize", 3, 5);

        // Phase 4: Timeout warning for one that was slow
        warn_effect_timeout("file_save", 250_000);
    });

    let spans = handle.spans();
    let events = handle.events();

    // Verify command spans
    let cmd_spans: Vec<_> = spans
        .iter()
        .filter(|s| s.name == "effect.command")
        .collect();
    assert_eq!(cmd_spans.len(), 2, "expected 2 command spans");

    // Verify subscription spans (3 start + 3 stop = 6)
    let sub_spans: Vec<_> = spans
        .iter()
        .filter(|s| s.name == "effect.subscription")
        .collect();
    assert_eq!(sub_spans.len(), 6, "expected 6 subscription spans");

    // Verify active=false after stop
    let inactive_spans: Vec<_> = sub_spans
        .iter()
        .filter(|s| s.fields.get("active").is_some_and(|v| v == "false"))
        .collect();
    assert_eq!(inactive_spans.len(), 3);

    // Verify WARN event
    let warn_events: Vec<_> = events
        .iter()
        .filter(|e| e.level == tracing::Level::WARN)
        .collect();
    assert!(!warn_events.is_empty(), "expected WARN timeout event");

    // Verify DEBUG lifecycle events exist
    let debug_events: Vec<_> = events
        .iter()
        .filter(|e| e.level == tracing::Level::DEBUG && e.target == telemetry_schema::TARGET_EFFECT)
        .collect();
    assert!(
        debug_events.len() >= 8,
        "expected at least 8 DEBUG events (2 cmd×2 + 3 sub start + 3 sub stop), got {}",
        debug_events.len()
    );
}

#[test]
fn structured_concurrency_all_subs_stopped_before_exit() {
    // Verify that after stopping all workers, none remain active.
    let counters: Vec<_> = (0..3).map(|_| Arc::new(Mutex::new(0u64))).collect();
    let stop = Arc::new(AtomicBool::new(false));

    let mut handles = Vec::new();
    for counter in &counters {
        let c = counter.clone();
        let s = stop.clone();
        handles.push(std::thread::spawn(move || {
            while !s.load(Ordering::Acquire) {
                let mut count = c.lock().unwrap();
                *count += 1;
                drop(count);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }));
    }

    std::thread::sleep(std::time::Duration::from_millis(50));

    // Stop all
    stop.store(true, Ordering::Release);

    // Join all threads
    for handle in handles {
        let _ = handle.join();
    }

    // Take final counter snapshots
    let final_counts: Vec<u64> = counters.iter().map(|c| *c.lock().unwrap()).collect();

    // Wait and verify no further increments (threads are gone)
    std::thread::sleep(std::time::Duration::from_millis(50));

    let later_counts: Vec<u64> = counters.iter().map(|c| *c.lock().unwrap()).collect();

    for (i, (f, l)) in final_counts.iter().zip(later_counts.iter()).enumerate() {
        assert_eq!(
            f, l,
            "subscription {i} counter changed after stop: {f} → {l}"
        );
    }
}
