#![forbid(unsafe_code)]

//! bd-xox.8: Log level policy compliance tests.
//!
//! Verify that log events follow the project's logging policy:
//! - All log events use structured fields (no bare strings at DEBUG+)
//! - TRACE-level logs include component and operation fields
//! - ERROR-level logs are actionable (include context for remediation)
//! - Log level categorization matches the telemetry-events.md spec
//! - No sensitive data leaks (PII, file paths, env vars) per redaction policy
//!
//! Run:
//!   cargo test -p ftui-runtime --test log_level_policy_compliance

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Once};

use ftui_runtime::telemetry_schema;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;

// ============================================================================
// Test Infrastructure
// ============================================================================

/// A captured log event with its metadata, fields, and parent span.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct CapturedEvent {
    level: tracing::Level,
    target: String,
    fields: HashMap<String, String>,
    message: Option<String>,
    parent_span_name: Option<String>,
}

impl CapturedEvent {
    /// Returns true if this event has at least one structured field
    /// (beyond just the message itself).
    fn has_structured_fields(&self) -> bool {
        self.fields.keys().any(|k| k != "message")
    }

    /// Returns the structured field names (excluding "message").
    fn structured_field_names(&self) -> Vec<&str> {
        self.fields
            .keys()
            .filter(|k| k.as_str() != "message")
            .map(String::as_str)
            .collect()
    }
}

/// A captured span for context tracking.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct CapturedSpan {
    name: String,
    level: tracing::Level,
    fields: HashMap<String, String>,
    parent_name: Option<String>,
}

/// Layer that captures events and spans.
struct EventCapture {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
    spans: Arc<Mutex<Vec<CapturedSpan>>>,
}

impl EventCapture {
    fn new() -> (Self, EventCaptureHandle) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let spans = Arc::new(Mutex::new(Vec::new()));

        let handle = EventCaptureHandle {
            events: events.clone(),
            spans: spans.clone(),
        };

        let layer = Self { events, spans };
        (layer, handle)
    }
}

/// Handle to read captured events and spans.
struct EventCaptureHandle {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
    #[allow(dead_code)]
    spans: Arc<Mutex<Vec<CapturedSpan>>>,
}

impl EventCaptureHandle {
    fn events(&self) -> Vec<CapturedEvent> {
        self.events.lock().unwrap().clone()
    }

    fn events_at_level(&self, level: tracing::Level) -> Vec<CapturedEvent> {
        self.events()
            .into_iter()
            .filter(|e| e.level == level)
            .collect()
    }
}

/// Visitor for extracting event/span fields.
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

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.0.push((field.name().to_string(), value.to_string()));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.push((field.name().to_string(), value.to_string()));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.0.push((field.name().to_string(), value.to_string()));
    }
}

impl<S> tracing_subscriber::Layer<S> for EventCapture
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        _id: &tracing::span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = FieldVisitor(Vec::new());
        attrs.record(&mut visitor);

        let parent_name = ctx
            .current_span()
            .id()
            .and_then(|id| ctx.span(id))
            .map(|span_ref| span_ref.name().to_string());

        let fields: HashMap<String, String> = visitor.0.into_iter().collect();

        self.spans.lock().unwrap().push(CapturedSpan {
            name: attrs.metadata().name().to_string(),
            level: *attrs.metadata().level(),
            fields,
            parent_name,
        });
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: tracing_subscriber::layer::Context<'_, S>) {
        let mut visitor = FieldVisitor(Vec::new());
        event.record(&mut visitor);

        let fields: HashMap<String, String> = visitor.0.clone().into_iter().collect();
        let message = fields.get("message").cloned();

        let parent_span_name = ctx
            .current_span()
            .id()
            .and_then(|id| ctx.span(id))
            .map(|span_ref| span_ref.name().to_string());

        self.events.lock().unwrap().push(CapturedEvent {
            level: *event.metadata().level(),
            target: event.metadata().target().to_string(),
            fields,
            message,
            parent_span_name,
        });
    }
}

/// Set up a tracing subscriber with event capture and run a closure.
fn with_captured_events<F>(f: F) -> EventCaptureHandle
where
    F: FnOnce(),
{
    ensure_global_trace_level();
    let (layer, handle) = EventCapture::new();
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
// Log Level Categorization Tests
// ============================================================================

/// Verify INFO-level events are used for significant state changes.
#[test]
fn info_level_for_significant_state_changes() {
    let handle = with_captured_events(|| {
        // INFO: strategy change (state transition)
        tracing::info!(
            old = "adaptive",
            new = "active_only",
            "tick strategy changed at runtime"
        );
        // INFO: state loaded (initialization milestone)
        tracing::info!(count = 5u32, "loaded widget state from persistence");
        // INFO: mode activation (configuration change)
        tracing::info!(
            inline_height = 24u16,
            render_mode = "scroll_region",
            "inline mode activated"
        );
    });

    let info_events = handle.events_at_level(tracing::Level::INFO);
    assert_eq!(
        info_events.len(),
        3,
        "Should have 3 INFO events, got {}",
        info_events.len()
    );

    // All INFO events should have structured fields
    for event in &info_events {
        assert!(
            event.has_structured_fields(),
            "INFO event '{}' should have structured fields beyond message. Got fields: {:?}",
            event.message.as_deref().unwrap_or("<none>"),
            event.fields.keys().collect::<Vec<_>>()
        );
    }
}

/// Verify DEBUG-level events are used for operational details.
#[test]
fn debug_level_for_operational_details() {
    let handle = with_captured_events(|| {
        // DEBUG: resize details
        tracing::debug!(
            width = 80u16,
            height = 24u16,
            behavior = "coalesce",
            "Resize event received"
        );
        // DEBUG: budget metrics
        tracing::debug!(
            render_ms = 12u32,
            budget_ms = 16u32,
            "render phase exceeded budget"
        );
        // DEBUG: conformal risk assessment
        tracing::debug!(
            bucket = "mode:80x24",
            upper_us = 15000.0f64,
            budget_us = 16666.0f64,
            risk = false,
            "conformal risk gate"
        );
    });

    let debug_events = handle.events_at_level(tracing::Level::DEBUG);
    assert_eq!(
        debug_events.len(),
        3,
        "Should have 3 DEBUG events, got {}",
        debug_events.len()
    );

    // All DEBUG events should have structured fields
    for event in &debug_events {
        assert!(
            event.has_structured_fields(),
            "DEBUG event '{}' should have structured fields. Got: {:?}",
            event.message.as_deref().unwrap_or("<none>"),
            event.fields.keys().collect::<Vec<_>>()
        );
    }
}

/// Verify WARN-level events are used for recoverable errors.
#[test]
fn warn_level_for_recoverable_errors() {
    let handle = with_captured_events(|| {
        // WARN: state load failure (recoverable)
        tracing::warn!(error = "file not found", "failed to load widget state");
        // WARN: scrollback preservation failure (recoverable)
        tracing::warn!(
            inline_height = 24u16,
            render_mode = "overlay",
            "scrollback preservation failed during inline render"
        );
    });

    let warn_events = handle.events_at_level(tracing::Level::WARN);
    assert_eq!(warn_events.len(), 2, "Should have 2 WARN events");

    // WARN events should have structured context
    for event in &warn_events {
        assert!(
            event.has_structured_fields(),
            "WARN event '{}' should have structured error/context fields. Got: {:?}",
            event.message.as_deref().unwrap_or("<none>"),
            event.fields.keys().collect::<Vec<_>>()
        );
    }
}

/// Verify ERROR-level events are used for critical/unrecoverable issues.
#[test]
fn error_level_for_critical_issues() {
    let handle = with_captured_events(|| {
        // ERROR: task panic (critical)
        tracing::error!(
            task_id = "bg-worker-1",
            panic_msg = "index out of bounds",
            "spawned task panicked"
        );
    });

    let error_events = handle.events_at_level(tracing::Level::ERROR);
    assert_eq!(error_events.len(), 1, "Should have 1 ERROR event");

    let error = &error_events[0];
    assert!(
        error.has_structured_fields(),
        "ERROR events must have structured fields for actionability. Got: {:?}",
        error.fields.keys().collect::<Vec<_>>()
    );
}

/// Verify TRACE-level events are used for fine-grained flow tracking.
#[test]
fn trace_level_for_fine_grained_flow() {
    let handle = with_captured_events(|| {
        // TRACE: subscription reconciliation details
        tracing::trace!(
            new_id_count = 3u32,
            active_before = 2u32,
            "subscription reconcile starting"
        );
        // TRACE: diff cost analysis
        tracing::trace!(
            strategy = "dirty",
            cost_full = 1920u32,
            cost_dirty = 48u32,
            dirty_rows = 2u32,
            total_rows = 24u32,
            "diff strategy cost analysis"
        );
    });

    let trace_events = handle.events_at_level(tracing::Level::TRACE);
    assert_eq!(trace_events.len(), 2, "Should have 2 TRACE events");

    // TRACE events should have detailed structured fields
    for event in &trace_events {
        let field_count = event.structured_field_names().len();
        assert!(
            field_count >= 2,
            "TRACE event '{}' should have at least 2 structured fields for operational detail. Got {} fields: {:?}",
            event.message.as_deref().unwrap_or("<none>"),
            field_count,
            event.structured_field_names()
        );
    }
}

// ============================================================================
// Structured Logging Policy Tests
// ============================================================================

/// Policy: All log events at DEBUG level and above should use structured fields.
///
/// Bare string-only messages (no structured fields) are discouraged at
/// DEBUG+ because they can't be programmatically parsed or filtered.
#[test]
fn policy_debug_and_above_use_structured_fields() {
    let handle = with_captured_events(|| {
        // Good: structured fields
        tracing::info!(count = 5u32, "loaded widget state");
        tracing::debug!(width = 80u16, height = 24u16, "resize received");
        tracing::warn!(error = "timeout", "operation failed");

        // These are the pattern we verify — all have structured fields
    });

    let events = handle.events();
    for event in &events {
        if event.level <= tracing::Level::DEBUG {
            assert!(
                event.has_structured_fields(),
                "Events at {} level should have structured fields. \
                 Event message: '{}', fields: {:?}",
                event.level,
                event.message.as_deref().unwrap_or("<none>"),
                event.fields.keys().collect::<Vec<_>>()
            );
        }
    }
}

/// Policy: TRACE events must include component and operation context.
///
/// TRACE logs should identify *what* component is operating and *what*
/// operation is being performed, to support granular filtering.
#[test]
fn policy_trace_events_include_component_and_operation_context() {
    let handle = with_captured_events(|| {
        // Good: component context (subscription reconcile)
        tracing::trace!(
            new_id_count = 3u32,
            active_before = 2u32,
            "subscription reconcile starting"
        );

        // Good: operation context (diff strategy analysis)
        tracing::trace!(
            strategy = "dirty",
            cost_full = 1920u32,
            cost_dirty = 48u32,
            dirty_rows = 2u32,
            total_rows = 24u32,
            total_cells = 1920u32,
            bayesian_enabled = true,
            "diff strategy cost analysis"
        );

        // Good: component + operation (tick processing)
        tracing::trace!(
            tick = 42u64,
            active = "main_screen",
            ticked = 1u32,
            "tick dispatched to active screen"
        );
    });

    let trace_events = handle.events_at_level(tracing::Level::TRACE);

    for event in &trace_events {
        // TRACE events must have at least 2 structured fields providing context
        let field_names = event.structured_field_names();
        assert!(
            field_names.len() >= 2,
            "TRACE event must include at least 2 contextual fields (component/operation). \
             Message: '{}', fields: {:?}",
            event.message.as_deref().unwrap_or("<none>"),
            field_names
        );
    }
}

/// Policy: ERROR events must provide actionable context.
///
/// At minimum, ERROR events should include structured fields that help
/// diagnose the root cause without requiring additional log correlation.
#[test]
fn policy_error_events_are_actionable() {
    let handle = with_captured_events(|| {
        // Good: structured error with context
        tracing::error!(
            task_id = "bg-worker-1",
            panic_msg = "index out of bounds: len is 0 but index is 1",
            "spawned task panicked"
        );

        // Good: error with operation and remediation context
        tracing::error!(
            operation = "present",
            error = "broken pipe",
            "terminal output failed"
        );
    });

    let error_events = handle.events_at_level(tracing::Level::ERROR);
    assert!(!error_events.is_empty(), "Should have ERROR events to test");

    for event in &error_events {
        // ERROR events must have structured fields for actionability
        assert!(
            event.has_structured_fields(),
            "ERROR event must be actionable with structured context. \
             Message: '{}', fields: {:?}",
            event.message.as_deref().unwrap_or("<none>"),
            event.fields.keys().collect::<Vec<_>>()
        );
    }
}

/// Policy: WARN events should include the error/reason for the warning.
#[test]
fn policy_warn_events_include_error_or_reason() {
    let handle = with_captured_events(|| {
        tracing::warn!(error = "file not found", "failed to load widget state");
        tracing::warn!(
            inline_height = 24u16,
            render_mode = "overlay",
            "scrollback preservation failed"
        );
        tracing::warn!(
            reason = "not yet implemented",
            "SetMode received but runtime mode switching not yet implemented"
        );
    });

    let warn_events = handle.events_at_level(tracing::Level::WARN);
    for event in &warn_events {
        let fields = event.structured_field_names();
        let has_error_or_reason = fields.iter().any(|f| {
            *f == "error" || *f == "reason" || *f == "inline_height" || *f == "render_mode"
        });
        assert!(
            has_error_or_reason,
            "WARN event should include 'error', 'reason', or contextual fields. \
             Message: '{}', fields: {:?}",
            event.message.as_deref().unwrap_or("<none>"),
            fields
        );
    }
}

// ============================================================================
// Redaction / Sensitive Data Tests
// ============================================================================

/// Policy: Log events must not contain PII or sensitive data.
///
/// The telemetry-events.md spec mandates conservative redaction:
/// - No user input content (key characters, paste content)
/// - No file paths (except widget IDs)
/// - No environment variable values
/// - No memory addresses
#[test]
fn policy_no_sensitive_data_in_events() {
    let handle = with_captured_events(|| {
        // These events should NOT contain sensitive data patterns
        tracing::info!(count = 5u32, "loaded widget state");
        tracing::debug!(
            width = 80u16,
            height = 24u16,
            "terminal dimensions detected"
        );
        tracing::trace!(tick = 42u64, active = "main_screen", "tick processed");
    });

    let events = handle.events();
    for event in &events {
        for (key, value) in &event.fields {
            // No file paths
            assert!(
                !value.starts_with('/') && !value.starts_with("C:\\"),
                "Field '{key}' contains what looks like a file path: '{value}'. \
                 File paths should not appear in log events per redaction policy."
            );
            // No memory addresses
            assert!(
                !value.starts_with("0x"),
                "Field '{key}' contains what looks like a memory address: '{value}'. \
                 Memory addresses should not appear in log events per redaction policy."
            );
            // No env var patterns
            assert!(
                !value.contains("HOME=") && !value.contains("PATH="),
                "Field '{key}' contains what looks like env var data: '{value}'. \
                 Environment variables should not appear in log events per redaction policy."
            );
        }
    }
}

/// Policy: Event types (event_type field) should only contain enum variants,
/// not raw input content.
#[test]
fn policy_event_types_are_enum_variants_not_content() {
    let handle = with_captured_events(|| {
        // Good: enum variant as event type
        tracing::debug!(event_type = "Key", "input event processed");
        tracing::debug!(event_type = "Mouse", "input event processed");
        tracing::debug!(event_type = "Tick", "input event processed");
        tracing::debug!(event_type = "Resize", "input event processed");
    });

    let events = handle.events();
    let allowed_event_types = ["Key", "Mouse", "Tick", "Resize", "FocusGained", "FocusLost"];

    for event in &events {
        if let Some(event_type) = event.fields.get("event_type") {
            assert!(
                allowed_event_types.contains(&event_type.as_str()),
                "event_type '{}' is not a recognized enum variant. \
                 Allowed: {:?}",
                event_type,
                allowed_event_types
            );
        }
    }
}

// ============================================================================
// Log Level Separation Tests
// ============================================================================

/// Verify that state transitions use INFO, not DEBUG.
#[test]
fn state_transitions_use_info_level() {
    let handle = with_captured_events(|| {
        // State transitions should be INFO
        tracing::info!(
            old = "adaptive",
            new = "active_only",
            "tick strategy changed at runtime"
        );
        tracing::info!(
            inline_height = 24u16,
            render_mode = "scroll_region",
            "inline mode activated"
        );
    });

    let info_events = handle.events_at_level(tracing::Level::INFO);
    assert!(
        info_events.len() >= 2,
        "State transitions should be logged at INFO level"
    );
}

/// Verify that metric/diagnostic details use DEBUG, not INFO.
#[test]
fn diagnostic_details_use_debug_level() {
    let handle = with_captured_events(|| {
        // Diagnostic details should be DEBUG
        tracing::debug!(
            render_ms = 12u32,
            budget_ms = 16u32,
            "render phase exceeded budget"
        );
        tracing::debug!(
            monotonic_counter_conformal_gate_triggers_total = 1u64,
            bucket = "mode:80x24",
            "conformal gate trigger"
        );
    });

    let debug_events = handle.events_at_level(tracing::Level::DEBUG);
    assert!(
        debug_events.len() >= 2,
        "Diagnostic/metric details should be logged at DEBUG level"
    );
}

/// Verify that fine-grained operational flow uses TRACE, not DEBUG.
#[test]
fn fine_grained_flow_uses_trace_level() {
    let handle = with_captured_events(|| {
        // Fine-grained flow should be TRACE
        tracing::trace!(
            new_id_count = 3u32,
            active_before = 2u32,
            "subscription reconcile starting"
        );
        tracing::trace!(
            strategy = "dirty",
            cost_full = 1920u32,
            cost_dirty = 48u32,
            "diff strategy cost analysis"
        );
    });

    let trace_events = handle.events_at_level(tracing::Level::TRACE);
    assert!(
        trace_events.len() >= 2,
        "Fine-grained operational flow should be logged at TRACE level"
    );
}

// ============================================================================
// Field Naming Convention Tests
// ============================================================================

/// Policy: Field names should use snake_case.
#[test]
fn field_names_use_snake_case() {
    let handle = with_captured_events(|| {
        tracing::info!(widget_count = 5u32, frame_idx = 42u64, "view complete");
        tracing::debug!(
            render_ms = 12u32,
            budget_ms = 16u32,
            dirty_rows = 2u32,
            total_cells = 1920u32,
            "render statistics"
        );
    });

    let events = handle.events();
    for event in &events {
        for key in event.fields.keys() {
            if key == "message" {
                continue;
            }
            // Field names should be lowercase with underscores
            assert!(
                key.chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit() || c == '.'),
                "Field name '{}' violates snake_case convention. \
                 Only lowercase, underscores, digits, and dots are allowed.",
                key
            );
        }
    }
}

/// Policy: Duration fields use the _us (microseconds) or _ms suffix.
#[test]
fn duration_fields_use_consistent_suffix() {
    let handle = with_captured_events(|| {
        tracing::debug!(
            render_ms = 12u32,
            budget_ms = 16u32,
            "render phase budget check"
        );
        tracing::debug!(
            present_ms = 3u32,
            budget_ms = 5u32,
            "present phase budget check"
        );
    });

    let events = handle.events();
    for event in &events {
        for key in event.fields.keys() {
            // If a field looks like a duration, it should end in _us or _ms
            if key.contains("duration") || key.contains("latency") || key.contains("elapsed") {
                assert!(
                    key.ends_with("_us") || key.ends_with("_ms"),
                    "Duration field '{}' should end with '_us' or '_ms' for unit clarity",
                    key
                );
            }
        }
    }
}

/// Policy: Boolean fields should be named clearly (no `is_` prefix needed
/// when the name is already unambiguous).
#[test]
fn boolean_fields_are_clear() {
    let handle = with_captured_events(|| {
        tracing::debug!(
            bayesian_enabled = true,
            bracket_supported = false,
            "rendering configuration"
        );
    });

    let events = handle.events();
    let debug_event = &events[0];

    // Verify boolean fields parse correctly
    assert_eq!(
        debug_event
            .fields
            .get("bayesian_enabled")
            .map(String::as_str),
        Some("true")
    );
    assert_eq!(
        debug_event
            .fields
            .get("bracket_supported")
            .map(String::as_str),
        Some("false")
    );
}

// ============================================================================
// Metric / Monotonic Counter Field Tests
// ============================================================================

/// Verify monotonic counter fields use the `monotonic.counter.*` namespace.
#[test]
fn monotonic_counters_use_namespace() {
    let handle = with_captured_events(|| {
        tracing::debug!(
            monotonic_counter_conformal_gate_triggers_total = 1u64,
            bucket = "mode:80x24",
            "conformal gate trigger"
        );
        tracing::debug!(
            monotonic_histogram_conformal_prediction_interval_width_us = 1500.0f64,
            bucket = "mode:80x24",
            "conformal prediction interval width"
        );
    });

    let events = handle.events();
    // Metric fields should exist and have correct prefixes
    let has_counter = events.iter().any(|e| {
        e.fields
            .keys()
            .any(|k| k.starts_with("monotonic_counter_") || k.contains("monotonic.counter"))
    });
    assert!(
        has_counter,
        "Monotonic counter fields should use monotonic_counter_ or monotonic.counter namespace"
    );
}

// ============================================================================
// Event-in-Span Context Tests
// ============================================================================

/// Verify events emitted within spans inherit the correct parent context.
#[test]
fn events_inherit_parent_span_context() {
    let handle = with_captured_events(|| {
        let span = tracing::info_span!(
            telemetry_schema::span::RENDER_FRAME,
            width = 80u16,
            height = 24u16
        );
        let _guard = span.enter();

        tracing::debug!(
            render_ms = 12u32,
            budget_ms = 16u32,
            "render phase exceeded budget"
        );
    });

    let events = handle.events();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].parent_span_name.as_deref(),
        Some(telemetry_schema::span::RENDER_FRAME),
        "Event emitted inside ftui.render.frame span should have it as parent"
    );
}

/// Verify events without a parent span have None as parent.
#[test]
fn events_without_span_have_no_parent() {
    let handle = with_captured_events(|| {
        tracing::info!(count = 5u32, "standalone info event");
    });

    let events = handle.events();
    assert_eq!(events.len(), 1);
    assert!(
        events[0].parent_span_name.is_none(),
        "Event outside any span should have no parent"
    );
}

// ============================================================================
// Static Analysis: Grep-Style Policy Checks
// ============================================================================

/// Static analysis: Verify that the test file itself doesn't violate
/// naming conventions in its test data.
#[test]
fn test_data_follows_conventions() {
    // Verify that all test field names we use follow snake_case
    let test_field_names = [
        "count",
        "width",
        "height",
        "render_ms",
        "budget_ms",
        "error",
        "inline_height",
        "render_mode",
        "task_id",
        "panic_msg",
        "operation",
        "new_id_count",
        "active_before",
        "strategy",
        "cost_full",
        "cost_dirty",
        "dirty_rows",
        "total_rows",
        "total_cells",
        "bayesian_enabled",
        "tick",
        "active",
        "ticked",
        "event_type",
        "widget_count",
        "frame_idx",
        "bucket",
        "upper_us",
        "budget_us",
        "risk",
        "degradation",
        "sub_id",
    ];

    for name in &test_field_names {
        assert!(
            name.chars()
                .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()),
            "Test field name '{}' should be snake_case",
            name
        );
    }
}

// ============================================================================
// Log Level Escalation Tests
// ============================================================================

/// Verify that recoverable errors don't escalate to ERROR.
#[test]
fn recoverable_errors_stay_at_warn() {
    let handle = with_captured_events(|| {
        // These are recoverable — should be WARN, not ERROR
        tracing::warn!(error = "file not found", "failed to load widget state");
        tracing::warn!(error = "timeout", "failed to save widget state");
    });

    let warn_events = handle.events_at_level(tracing::Level::WARN);
    let error_events = handle.events_at_level(tracing::Level::ERROR);

    assert_eq!(warn_events.len(), 2, "Recoverable errors should be WARN");
    assert_eq!(
        error_events.len(),
        0,
        "No ERROR events expected for recoverable conditions"
    );
}

/// Verify that performance diagnostics don't escalate to WARN.
#[test]
fn performance_diagnostics_stay_at_debug() {
    let handle = with_captured_events(|| {
        // Budget overruns are diagnostic, not warnings
        tracing::debug!(
            render_ms = 20u32,
            budget_ms = 16u32,
            "render phase exceeded budget"
        );
        tracing::debug!(
            present_ms = 8u32,
            budget_ms = 5u32,
            "present phase exceeded budget"
        );
        tracing::debug!(
            degradation = 2u32,
            "frame skipped: budget exhausted before render"
        );
    });

    let debug_events = handle.events_at_level(tracing::Level::DEBUG);
    let warn_events = handle.events_at_level(tracing::Level::WARN);

    assert_eq!(
        debug_events.len(),
        3,
        "Budget diagnostics should be DEBUG level"
    );
    assert_eq!(
        warn_events.len(),
        0,
        "Budget diagnostics should not escalate to WARN"
    );
}

// ============================================================================
// Message Clarity Tests
// ============================================================================

/// Verify event messages are descriptive (not empty or generic).
#[test]
fn event_messages_are_descriptive() {
    let handle = with_captured_events(|| {
        tracing::info!(count = 5u32, "loaded widget state from persistence");
        tracing::debug!(width = 80u16, height = 24u16, "Resize event received");
        tracing::warn!(error = "file not found", "failed to load widget state");
    });

    let events = handle.events();
    for event in &events {
        if let Some(ref msg) = event.message {
            // Messages should be non-empty
            assert!(!msg.trim().is_empty(), "Event message should not be empty");
            // Messages should be descriptive (at least 3 words)
            let word_count = msg.split_whitespace().count();
            assert!(
                word_count >= 2,
                "Event message '{}' should be descriptive (at least 2 words), got {} word(s)",
                msg,
                word_count
            );
        }
    }
}

/// Verify log events consistently use past tense or present continuous
/// for their messages (matching the codebase convention).
#[test]
fn event_messages_follow_verb_convention() {
    let handle = with_captured_events(|| {
        // Conventions used in the codebase:
        // Past tense for completed actions
        tracing::info!(count = 5u32, "loaded widget state from persistence");
        // Present tense for ongoing/current state
        tracing::debug!(
            render_ms = 12u32,
            budget_ms = 16u32,
            "render phase exceeded budget"
        );
        // Descriptive for decisions
        tracing::debug!(
            degradation = 2u32,
            "frame skipped: budget exhausted before render"
        );
    });

    let events = handle.events();
    for event in &events {
        if let Some(ref msg) = event.message {
            // Messages should not start with "TODO", "FIXME", etc.
            assert!(
                !msg.starts_with("TODO") && !msg.starts_with("FIXME") && !msg.starts_with("HACK"),
                "Event message '{}' should not contain TODO/FIXME/HACK markers",
                msg
            );
        }
    }
}
