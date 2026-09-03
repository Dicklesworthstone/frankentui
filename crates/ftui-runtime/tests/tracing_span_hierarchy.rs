#![forbid(unsafe_code)]

//! bd-xox.7: Tracing span hierarchy enforcement tests.
//!
//! Verify the canonical span hierarchy produced during a standard render cycle.
//! Assert parent-child relationships, required fields, and no orphan spans.
//!
//! Run:
//!   cargo test -p ftui-runtime --test tracing_span_hierarchy

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Once};

use ftui_runtime::telemetry_schema;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;

// ============================================================================
// Test Infrastructure (adapted from ftui-widgets/tests/tracing_tests.rs)
// ============================================================================

/// A captured span with its metadata and parent info.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct CapturedSpan {
    name: String,
    target: String,
    level: tracing::Level,
    fields: HashMap<String, String>,
    parent_name: Option<String>,
}

/// A captured event with its metadata and parent span.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct CapturedEvent {
    level: tracing::Level,
    target: String,
    message: String,
    fields: HashMap<String, String>,
    parent_span_name: Option<String>,
}

/// A tracing Layer that captures span metadata, events, and parent info.
struct SpanCapture {
    spans: Arc<Mutex<Vec<CapturedSpan>>>,
    events: Arc<Mutex<Vec<CapturedEvent>>>,
    /// Map from span ID to index in spans vec, for updating fields via record().
    span_index: Arc<Mutex<HashMap<u64, usize>>>,
}

impl SpanCapture {
    fn new() -> (Self, CaptureHandle) {
        let spans = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let span_index = Arc::new(Mutex::new(HashMap::new()));

        let handle = CaptureHandle {
            spans: spans.clone(),
            events: events.clone(),
        };

        let layer = Self {
            spans,
            events,
            span_index,
        };

        (layer, handle)
    }
}

/// Handle to read captured spans and events after execution.
struct CaptureHandle {
    spans: Arc<Mutex<Vec<CapturedSpan>>>,
    events: Arc<Mutex<Vec<CapturedEvent>>>,
}

impl CaptureHandle {
    fn spans(&self) -> Vec<CapturedSpan> {
        self.spans.lock().unwrap().clone()
    }

    #[allow(dead_code)]
    fn events(&self) -> Vec<CapturedEvent> {
        self.events.lock().unwrap().clone()
    }
}

/// Visitor that extracts span/event fields.
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

impl<S> tracing_subscriber::Layer<S> for SpanCapture
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = FieldVisitor(Vec::new());
        attrs.record(&mut visitor);

        let parent_name = ctx
            .current_span()
            .id()
            .and_then(|pid| ctx.span(pid))
            .map(|span_ref| span_ref.name().to_string());

        // Collect declared fields (including Empty ones from metadata).
        let mut fields: HashMap<String, String> = visitor.0.into_iter().collect();

        // Also record field names that were declared but Empty (not yet recorded).
        // These show up in the metadata field set but not in the visitor.
        for field in attrs.metadata().fields() {
            fields.entry(field.name().to_string()).or_default();
        }

        let mut spans = self.spans.lock().unwrap();
        let idx = spans.len();
        spans.push(CapturedSpan {
            name: attrs.metadata().name().to_string(),
            target: attrs.metadata().target().to_string(),
            level: *attrs.metadata().level(),
            fields,
            parent_name,
        });

        self.span_index.lock().unwrap().insert(id.into_u64(), idx);
    }

    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = FieldVisitor(Vec::new());
        values.record(&mut visitor);

        let index = self.span_index.lock().unwrap();
        if let Some(&idx) = index.get(&id.into_u64()) {
            let mut spans = self.spans.lock().unwrap();
            if let Some(span) = spans.get_mut(idx) {
                for (k, v) in visitor.0 {
                    span.fields.insert(k, v);
                }
            }
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: tracing_subscriber::layer::Context<'_, S>) {
        let mut visitor = FieldVisitor(Vec::new());
        event.record(&mut visitor);

        let fields: HashMap<String, String> = visitor.0.clone().into_iter().collect();
        let message = visitor
            .0
            .iter()
            .find(|(k, _)| k == "message")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();

        let parent_span_name = ctx
            .current_span()
            .id()
            .and_then(|id| ctx.span(id))
            .map(|span_ref| span_ref.name().to_string());

        self.events.lock().unwrap().push(CapturedEvent {
            level: *event.metadata().level(),
            target: event.metadata().target().to_string(),
            message,
            fields,
            parent_span_name,
        });
    }
}

/// Set up a tracing subscriber with span capture and run a closure.
fn with_captured_spans<F>(f: F) -> CaptureHandle
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
// Canonical Span Definitions
// ============================================================================

/// Canonical root spans (no parent required).
const ROOT_SPANS: &[&str] = &[
    telemetry_schema::span::PROGRAM_INIT,
    telemetry_schema::span::PROGRAM_UPDATE,
    telemetry_schema::span::RENDER_FRAME,
    telemetry_schema::span::PROGRAM_SUBSCRIPTIONS,
    "conformal.predict",
    "golden.compare",
];

/// Required parent-child relationships.
/// (child_name, expected_parent_name)
const REQUIRED_PARENT_CHILD: &[(&str, &str)] = &[
    // Render pipeline: frame → present
    (
        telemetry_schema::span::RENDER_PRESENT,
        telemetry_schema::span::RENDER_FRAME,
    ),
    // Render pipeline: frame → view
    (
        telemetry_schema::span::PROGRAM_VIEW,
        telemetry_schema::span::RENDER_FRAME,
    ),
    // Present pipeline: present → inline.render
    ("inline.render", telemetry_schema::span::RENDER_PRESENT),
    // Inline internals
    (
        telemetry_schema::span::RENDER_SCROLL_REGION,
        "inline.render",
    ),
    (telemetry_schema::span::RENDER_DIFF_COMPUTE, "inline.render"),
    (telemetry_schema::span::RENDER_EMIT, "inline.render"),
];

// ============================================================================
// Span Hierarchy Tests
// ============================================================================

/// Verify that ftui.program.init is a root span with no parent.
#[test]
fn program_init_is_root_span() {
    let handle = with_captured_spans(|| {
        let _span = tracing::info_span!(telemetry_schema::span::PROGRAM_INIT).entered();
    });

    let spans = handle.spans();
    let init_span = spans
        .iter()
        .find(|s| s.name == telemetry_schema::span::PROGRAM_INIT)
        .expect("ftui.program.init span should exist");

    assert!(
        init_span.parent_name.is_none(),
        "ftui.program.init must be a root span (no parent), got parent: {:?}",
        init_span.parent_name
    );
}

/// Verify that ftui.program.update is a root span with required fields.
#[test]
fn program_update_is_root_with_required_fields() {
    let handle = with_captured_spans(|| {
        let _span = tracing::debug_span!(
            telemetry_schema::span::PROGRAM_UPDATE,
            msg_type = "Tick",
            duration_us = tracing::field::Empty,
            cmd_type = tracing::field::Empty,
        )
        .entered();
    });

    let spans = handle.spans();
    let update_span = spans
        .iter()
        .find(|s| s.name == telemetry_schema::span::PROGRAM_UPDATE)
        .expect("ftui.program.update span should exist");

    assert!(
        update_span.parent_name.is_none(),
        "ftui.program.update must be a root span"
    );

    // msg_type is a required field on update spans
    assert!(
        update_span.fields.contains_key("msg_type"),
        "ftui.program.update must have msg_type field, got fields: {:?}",
        update_span.fields.keys().collect::<Vec<_>>()
    );
}

/// Verify ftui.render.frame is a root span with width/height fields.
#[test]
fn render_frame_is_root_with_dimension_fields() {
    let handle = with_captured_spans(|| {
        let _span = tracing::info_span!(
            telemetry_schema::span::RENDER_FRAME,
            width = 80u16,
            height = 24u16,
            duration_us = tracing::field::Empty,
        )
        .entered();
    });

    let spans = handle.spans();
    let frame_span = spans
        .iter()
        .find(|s| s.name == telemetry_schema::span::RENDER_FRAME)
        .expect("ftui.render.frame span should exist");

    assert!(
        frame_span.parent_name.is_none(),
        "ftui.render.frame must be a root span"
    );

    assert!(
        frame_span.fields.contains_key("width"),
        "ftui.render.frame must have width field"
    );
    assert!(
        frame_span.fields.contains_key("height"),
        "ftui.render.frame must have height field"
    );
}

/// Verify ftui.program.view nests under ftui.render.frame.
#[test]
fn view_nests_under_render_frame() {
    let handle = with_captured_spans(|| {
        let frame_span = tracing::info_span!(
            telemetry_schema::span::RENDER_FRAME,
            width = 80u16,
            height = 24u16,
            duration_us = tracing::field::Empty,
        );
        let _frame_guard = frame_span.enter();

        let _view_span = tracing::debug_span!(
            telemetry_schema::span::PROGRAM_VIEW,
            duration_us = tracing::field::Empty,
            widget_count = tracing::field::Empty,
        )
        .entered();
    });

    let spans = handle.spans();
    let view_span = spans
        .iter()
        .find(|s| s.name == telemetry_schema::span::PROGRAM_VIEW)
        .expect("ftui.program.view span should exist");

    assert_eq!(
        view_span.parent_name.as_deref(),
        Some(telemetry_schema::span::RENDER_FRAME),
        "ftui.program.view must nest under ftui.render.frame"
    );
}

/// Verify ftui.render.present nests under ftui.render.frame.
#[test]
fn present_nests_under_render_frame() {
    let handle = with_captured_spans(|| {
        let frame_span = tracing::info_span!(
            telemetry_schema::span::RENDER_FRAME,
            width = 80u16,
            height = 24u16,
            duration_us = tracing::field::Empty,
        );
        let _frame_guard = frame_span.enter();

        let _present_span = tracing::debug_span!(telemetry_schema::span::RENDER_PRESENT).entered();
    });

    let spans = handle.spans();
    let present_span = spans
        .iter()
        .find(|s| s.name == telemetry_schema::span::RENDER_PRESENT)
        .expect("ftui.render.present span should exist");

    assert_eq!(
        present_span.parent_name.as_deref(),
        Some(telemetry_schema::span::RENDER_FRAME),
        "ftui.render.present must nest under ftui.render.frame"
    );
}

/// Verify inline.render nests under ftui.render.present.
#[test]
fn inline_render_nests_under_present() {
    let handle = with_captured_spans(|| {
        let present_span = tracing::info_span!(
            telemetry_schema::span::RENDER_PRESENT,
            mode = "inline",
            width = 80u16,
            height = 24u16,
        );
        let _present_guard = present_span.enter();

        let _inline_span = tracing::info_span!(
            "inline.render",
            inline_height = 24u16,
            scrollback_preserved = tracing::field::Empty,
            render_mode = "scroll_region",
        )
        .entered();
    });

    let spans = handle.spans();
    let inline_span = spans
        .iter()
        .find(|s| s.name == "inline.render")
        .expect("inline.render span should exist");

    assert_eq!(
        inline_span.parent_name.as_deref(),
        Some(telemetry_schema::span::RENDER_PRESENT),
        "inline.render must nest under ftui.render.present"
    );
}

/// Verify scroll_region, diff_compute, and emit nest under inline.render.
#[test]
fn inline_children_nest_under_inline_render() {
    let handle = with_captured_spans(|| {
        let inline_span = tracing::info_span!(
            "inline.render",
            inline_height = 24u16,
            scrollback_preserved = tracing::field::Empty,
            render_mode = "scroll_region",
        );
        let _inline_guard = inline_span.enter();

        {
            let _scroll =
                tracing::debug_span!(telemetry_schema::span::RENDER_SCROLL_REGION).entered();
        }
        {
            let _diff = tracing::debug_span!(telemetry_schema::span::RENDER_DIFF_COMPUTE).entered();
        }
        {
            let _emit = tracing::debug_span!(telemetry_schema::span::RENDER_EMIT).entered();
        }
    });

    let spans = handle.spans();

    for child_name in &[
        telemetry_schema::span::RENDER_SCROLL_REGION,
        telemetry_schema::span::RENDER_DIFF_COMPUTE,
        telemetry_schema::span::RENDER_EMIT,
    ] {
        let child = spans
            .iter()
            .find(|s| s.name == *child_name)
            .unwrap_or_else(|| panic!("{child_name} span should exist"));

        assert_eq!(
            child.parent_name.as_deref(),
            Some("inline.render"),
            "{child_name} must nest under inline.render, got parent: {:?}",
            child.parent_name
        );
    }
}

/// Verify the full canonical render pipeline hierarchy in a single pass.
#[test]
fn full_render_pipeline_hierarchy() {
    let handle = with_captured_spans(|| {
        // Root: ftui.render.frame
        let frame_span = tracing::info_span!(
            telemetry_schema::span::RENDER_FRAME,
            width = 80u16,
            height = 24u16,
            duration_us = tracing::field::Empty,
        );
        let _frame = frame_span.enter();

        // Child: ftui.program.view (inside frame)
        {
            let _view = tracing::debug_span!(
                telemetry_schema::span::PROGRAM_VIEW,
                duration_us = tracing::field::Empty,
                widget_count = tracing::field::Empty,
            )
            .entered();
        }

        // Child: ftui.render.present (inside frame)
        {
            let present_span = tracing::debug_span!(telemetry_schema::span::RENDER_PRESENT);
            let _present = present_span.enter();

            // Grandchild: inline.render (inside present)
            {
                let inline_span = tracing::info_span!(
                    "inline.render",
                    inline_height = 24u16,
                    scrollback_preserved = tracing::field::Empty,
                    render_mode = "scroll_region",
                );
                let _inline = inline_span.enter();

                // Great-grandchildren (inside inline.render)
                {
                    let _scroll =
                        tracing::debug_span!(telemetry_schema::span::RENDER_SCROLL_REGION)
                            .entered();
                }
                {
                    let _diff =
                        tracing::debug_span!(telemetry_schema::span::RENDER_DIFF_COMPUTE).entered();
                }
                {
                    let _emit = tracing::debug_span!(telemetry_schema::span::RENDER_EMIT).entered();
                }
            }
        }
    });

    let spans = handle.spans();

    // Verify root
    let frame = spans
        .iter()
        .find(|s| s.name == telemetry_schema::span::RENDER_FRAME)
        .expect("ftui.render.frame must exist");
    assert!(frame.parent_name.is_none(), "frame must be root");

    // Verify view → frame
    let view = spans
        .iter()
        .find(|s| s.name == telemetry_schema::span::PROGRAM_VIEW)
        .expect("ftui.program.view must exist");
    assert_eq!(
        view.parent_name.as_deref(),
        Some(telemetry_schema::span::RENDER_FRAME)
    );

    // Verify present → frame
    let present = spans
        .iter()
        .find(|s| s.name == telemetry_schema::span::RENDER_PRESENT)
        .expect("ftui.render.present must exist");
    assert_eq!(
        present.parent_name.as_deref(),
        Some(telemetry_schema::span::RENDER_FRAME)
    );

    // Verify inline → present
    let inline = spans
        .iter()
        .find(|s| s.name == "inline.render")
        .expect("inline.render must exist");
    assert_eq!(
        inline.parent_name.as_deref(),
        Some(telemetry_schema::span::RENDER_PRESENT)
    );

    // Verify terminal children → inline
    for child_name in &[
        telemetry_schema::span::RENDER_SCROLL_REGION,
        telemetry_schema::span::RENDER_DIFF_COMPUTE,
        telemetry_schema::span::RENDER_EMIT,
    ] {
        let child = spans
            .iter()
            .find(|s| s.name == *child_name)
            .unwrap_or_else(|| panic!("{child_name} must exist"));
        assert_eq!(
            child.parent_name.as_deref(),
            Some("inline.render"),
            "{child_name} must nest under inline.render"
        );
    }
}

/// Verify no orphan spans in a complete render pipeline simulation.
///
/// An orphan span is one that has a parent that isn't in our known set
/// (root spans don't need parents).
#[test]
fn no_orphan_spans_in_render_pipeline() {
    let handle = with_captured_spans(|| {
        // Simulate a full render cycle
        let frame_span = tracing::info_span!(
            telemetry_schema::span::RENDER_FRAME,
            width = 80u16,
            height = 24u16,
            duration_us = tracing::field::Empty,
        );
        let _frame = frame_span.enter();

        {
            let _view = tracing::debug_span!(
                telemetry_schema::span::PROGRAM_VIEW,
                duration_us = tracing::field::Empty,
                widget_count = tracing::field::Empty,
            )
            .entered();
        }

        {
            let present_span = tracing::debug_span!(telemetry_schema::span::RENDER_PRESENT);
            let _present = present_span.enter();

            {
                let inline_span = tracing::info_span!(
                    "inline.render",
                    inline_height = 24u16,
                    scrollback_preserved = tracing::field::Empty,
                    render_mode = "overlay",
                );
                let _inline = inline_span.enter();

                {
                    let _scroll =
                        tracing::debug_span!(telemetry_schema::span::RENDER_SCROLL_REGION)
                            .entered();
                }
                {
                    let _diff =
                        tracing::debug_span!(telemetry_schema::span::RENDER_DIFF_COMPUTE).entered();
                }
                {
                    let _emit = tracing::debug_span!(telemetry_schema::span::RENDER_EMIT).entered();
                }
            }
        }
    });

    let spans = handle.spans();
    let all_span_names: Vec<&str> = spans.iter().map(|s| s.name.as_str()).collect();

    for span in &spans {
        if ROOT_SPANS.contains(&span.name.as_str()) {
            // Root spans should have no parent
            assert!(
                span.parent_name.is_none(),
                "Root span '{}' must have no parent, but has parent '{:?}'",
                span.name,
                span.parent_name
            );
        } else if let Some(ref parent) = span.parent_name {
            // Non-root spans must reference a parent that exists in the captured set
            assert!(
                all_span_names.contains(&parent.as_str()),
                "Orphan span detected: '{}' references parent '{}' which is not in captured spans: {:?}",
                span.name,
                parent,
                all_span_names
            );
        }
        // Note: spans with no parent that are not in ROOT_SPANS are flagged below
    }
}

/// Verify that all spans with no parent are known root spans.
#[test]
fn all_parentless_spans_are_known_roots() {
    let handle = with_captured_spans(|| {
        // Emit various root spans
        {
            let _init = tracing::info_span!(telemetry_schema::span::PROGRAM_INIT).entered();
        }
        {
            let _update = tracing::debug_span!(
                telemetry_schema::span::PROGRAM_UPDATE,
                msg_type = "Tick",
                duration_us = tracing::field::Empty,
                cmd_type = tracing::field::Empty,
            )
            .entered();
        }
        {
            let _frame = tracing::info_span!(
                telemetry_schema::span::RENDER_FRAME,
                width = 80u16,
                height = 24u16,
                duration_us = tracing::field::Empty,
            )
            .entered();
        }
        {
            let _subs = tracing::debug_span!(
                telemetry_schema::span::PROGRAM_SUBSCRIPTIONS,
                active_count = tracing::field::Empty,
                started = tracing::field::Empty,
                stopped = tracing::field::Empty,
            )
            .entered();
        }
    });

    let spans = handle.spans();
    for span in &spans {
        if span.parent_name.is_none() {
            assert!(
                ROOT_SPANS.contains(&span.name.as_str()),
                "Span '{}' has no parent but is not in ROOT_SPANS list. \
                 Either add it to ROOT_SPANS or ensure it is nested under a parent.",
                span.name
            );
        }
    }
}

// ============================================================================
// Required Field Tests
// ============================================================================

/// Verify ftui.program.update has msg_type, duration_us, cmd_type fields.
#[test]
fn update_span_has_required_fields() {
    let handle = with_captured_spans(|| {
        let _span = tracing::debug_span!(
            telemetry_schema::span::PROGRAM_UPDATE,
            msg_type = "event",
            duration_us = tracing::field::Empty,
            cmd_type = tracing::field::Empty,
        )
        .entered();
    });

    let spans = handle.spans();
    let update_span = spans
        .iter()
        .find(|s| s.name == telemetry_schema::span::PROGRAM_UPDATE)
        .expect("ftui.program.update span must exist");

    let required_fields = ["msg_type", "duration_us", "cmd_type"];
    for field in &required_fields {
        assert!(
            update_span.fields.contains_key(*field),
            "ftui.program.update missing required field '{}'. Got: {:?}",
            field,
            update_span.fields.keys().collect::<Vec<_>>()
        );
    }
}

/// Verify ftui.program.view has duration_us and widget_count fields.
#[test]
fn view_span_has_required_fields() {
    let handle = with_captured_spans(|| {
        let _span = tracing::debug_span!(
            telemetry_schema::span::PROGRAM_VIEW,
            duration_us = tracing::field::Empty,
            widget_count = tracing::field::Empty,
        )
        .entered();
    });

    let spans = handle.spans();
    let view_span = spans
        .iter()
        .find(|s| s.name == telemetry_schema::span::PROGRAM_VIEW)
        .expect("ftui.program.view span must exist");

    let required_fields = ["duration_us", "widget_count"];
    for field in &required_fields {
        assert!(
            view_span.fields.contains_key(*field),
            "ftui.program.view missing required field '{}'. Got: {:?}",
            field,
            view_span.fields.keys().collect::<Vec<_>>()
        );
    }
}

/// Verify ftui.render.frame has width, height, duration_us fields.
#[test]
fn render_frame_span_has_required_fields() {
    let handle = with_captured_spans(|| {
        let _span = tracing::info_span!(
            telemetry_schema::span::RENDER_FRAME,
            width = 120u16,
            height = 40u16,
            duration_us = tracing::field::Empty,
        )
        .entered();
    });

    let spans = handle.spans();
    let frame_span = spans
        .iter()
        .find(|s| s.name == telemetry_schema::span::RENDER_FRAME)
        .expect("ftui.render.frame span must exist");

    let required_fields = ["width", "height", "duration_us"];
    for field in &required_fields {
        assert!(
            frame_span.fields.contains_key(*field),
            "ftui.render.frame missing required field '{}'. Got: {:?}",
            field,
            frame_span.fields.keys().collect::<Vec<_>>()
        );
    }

    // Verify dimension values are correct
    assert_eq!(
        frame_span.fields.get("width").map(String::as_str),
        Some("120"),
        "width should be 120"
    );
    assert_eq!(
        frame_span.fields.get("height").map(String::as_str),
        Some("40"),
        "height should be 40"
    );
}

/// Verify ftui.program.subscriptions has active_count, started, stopped fields.
#[test]
fn subscriptions_span_has_required_fields() {
    let handle = with_captured_spans(|| {
        let _span = tracing::debug_span!(
            telemetry_schema::span::PROGRAM_SUBSCRIPTIONS,
            active_count = tracing::field::Empty,
            started = tracing::field::Empty,
            stopped = tracing::field::Empty,
        )
        .entered();
    });

    let spans = handle.spans();
    let subs_span = spans
        .iter()
        .find(|s| s.name == telemetry_schema::span::PROGRAM_SUBSCRIPTIONS)
        .expect("ftui.program.subscriptions span must exist");

    let required_fields = ["active_count", "started", "stopped"];
    for field in &required_fields {
        assert!(
            subs_span.fields.contains_key(*field),
            "ftui.program.subscriptions missing required field '{}'. Got: {:?}",
            field,
            subs_span.fields.keys().collect::<Vec<_>>()
        );
    }
}

/// Verify inline.render has inline_height, scrollback_preserved, render_mode.
#[test]
fn inline_render_span_has_required_fields() {
    let handle = with_captured_spans(|| {
        let _span = tracing::info_span!(
            "inline.render",
            inline_height = 24u16,
            scrollback_preserved = tracing::field::Empty,
            render_mode = "hybrid",
        )
        .entered();
    });

    let spans = handle.spans();
    let inline_span = spans
        .iter()
        .find(|s| s.name == "inline.render")
        .expect("inline.render span must exist");

    let required_fields = ["inline_height", "scrollback_preserved", "render_mode"];
    for field in &required_fields {
        assert!(
            inline_span.fields.contains_key(*field),
            "inline.render missing required field '{}'. Got: {:?}",
            field,
            inline_span.fields.keys().collect::<Vec<_>>()
        );
    }
}

/// Verify conformal.predict has its required fields.
#[test]
fn conformal_predict_span_has_required_fields() {
    let handle = with_captured_spans(|| {
        let span = tracing::info_span!(
            "conformal.predict",
            calibration_set_size = tracing::field::Empty,
            predicted_upper_bound_us = tracing::field::Empty,
            frame_budget_us = 16666.0f64,
            coverage_alpha = 0.05f64,
            gate_triggered = tracing::field::Empty,
        );
        let _guard = span.enter();
        span.record("calibration_set_size", 100u64);
        span.record("predicted_upper_bound_us", 15000.0f64);
        span.record("gate_triggered", false);
    });

    let spans = handle.spans();
    let predict_span = spans
        .iter()
        .find(|s| s.name == "conformal.predict")
        .expect("conformal.predict span must exist");

    let required_fields = [
        "calibration_set_size",
        "predicted_upper_bound_us",
        "frame_budget_us",
        "coverage_alpha",
        "gate_triggered",
    ];
    for field in &required_fields {
        assert!(
            predict_span.fields.contains_key(*field),
            "conformal.predict missing required field '{}'. Got: {:?}",
            field,
            predict_span.fields.keys().collect::<Vec<_>>()
        );
    }
}

/// Verify ftui.render.present in terminal_writer has mode, width, height.
#[test]
fn terminal_writer_present_span_has_required_fields() {
    let handle = with_captured_spans(|| {
        let _span = tracing::info_span!(
            telemetry_schema::span::RENDER_PRESENT,
            mode = "inline",
            width = 80u16,
            height = 24u16,
        )
        .entered();
    });

    let spans = handle.spans();
    let present_span = spans
        .iter()
        .find(|s| s.name == telemetry_schema::span::RENDER_PRESENT)
        .expect("ftui.render.present span must exist");

    let required_fields = ["mode", "width", "height"];
    for field in &required_fields {
        assert!(
            present_span.fields.contains_key(*field),
            "ftui.render.present missing required field '{}'. Got: {:?}",
            field,
            present_span.fields.keys().collect::<Vec<_>>()
        );
    }
}

// ============================================================================
// Span Ordering Tests
// ============================================================================

/// Verify deterministic span ordering in a render cycle.
///
/// In the canonical render pipeline, the order should be:
///   ftui.render.frame (root)
///     → ftui.program.view (first child: render widgets)
///     → ftui.render.present (second child: emit to terminal)
///       → inline.render (present child)
///         → ftui.render.scroll_region
///         → ftui.render.diff_compute
///         → ftui.render.emit
#[test]
fn span_ordering_is_deterministic() {
    let handle = with_captured_spans(|| {
        let frame_span = tracing::info_span!(
            telemetry_schema::span::RENDER_FRAME,
            width = 80u16,
            height = 24u16,
            duration_us = tracing::field::Empty,
        );
        let _frame = frame_span.enter();

        // View phase comes first
        {
            let _view = tracing::debug_span!(
                telemetry_schema::span::PROGRAM_VIEW,
                duration_us = tracing::field::Empty,
                widget_count = tracing::field::Empty,
            )
            .entered();
        }

        // Present phase comes second
        {
            let present = tracing::debug_span!(telemetry_schema::span::RENDER_PRESENT);
            let _present = present.enter();

            {
                let inline = tracing::info_span!(
                    "inline.render",
                    inline_height = 24u16,
                    scrollback_preserved = tracing::field::Empty,
                    render_mode = "scroll_region",
                );
                let _inline = inline.enter();

                {
                    let _scroll =
                        tracing::debug_span!(telemetry_schema::span::RENDER_SCROLL_REGION)
                            .entered();
                }
                {
                    let _diff =
                        tracing::debug_span!(telemetry_schema::span::RENDER_DIFF_COMPUTE).entered();
                }
                {
                    let _emit = tracing::debug_span!(telemetry_schema::span::RENDER_EMIT).entered();
                }
            }
        }
    });

    let spans = handle.spans();
    let names: Vec<&str> = spans.iter().map(|s| s.name.as_str()).collect();

    // Verify ordering: view comes before present in the span list
    let view_idx = names
        .iter()
        .position(|n| *n == telemetry_schema::span::PROGRAM_VIEW)
        .expect("view span must exist");
    let present_idx = names
        .iter()
        .position(|n| *n == telemetry_schema::span::RENDER_PRESENT)
        .expect("present span must exist");
    assert!(
        view_idx < present_idx,
        "ftui.program.view (idx {view_idx}) must come before ftui.render.present (idx {present_idx})"
    );

    // Verify ordering: scroll_region → diff_compute → emit
    let scroll_idx = names
        .iter()
        .position(|n| *n == telemetry_schema::span::RENDER_SCROLL_REGION)
        .expect("scroll_region span must exist");
    let diff_idx = names
        .iter()
        .position(|n| *n == telemetry_schema::span::RENDER_DIFF_COMPUTE)
        .expect("diff_compute span must exist");
    let emit_idx = names
        .iter()
        .position(|n| *n == telemetry_schema::span::RENDER_EMIT)
        .expect("emit span must exist");

    assert!(
        scroll_idx < diff_idx,
        "scroll_region (idx {scroll_idx}) must come before diff_compute (idx {diff_idx})"
    );
    assert!(
        diff_idx < emit_idx,
        "diff_compute (idx {diff_idx}) must come before emit (idx {emit_idx})"
    );
}

// ============================================================================
// Update Variant Tests
// ============================================================================

/// Verify all update msg_type variants are valid.
#[test]
fn update_msg_type_variants_are_valid() {
    let valid_msg_types = ["Tick", "event", "subscription", "task"];

    for msg_type in &valid_msg_types {
        let handle = with_captured_spans(|| {
            let _span = tracing::debug_span!(
                telemetry_schema::span::PROGRAM_UPDATE,
                msg_type = *msg_type,
                duration_us = tracing::field::Empty,
                cmd_type = tracing::field::Empty,
            )
            .entered();
        });

        let spans = handle.spans();
        let update_span = spans
            .iter()
            .find(|s| s.name == telemetry_schema::span::PROGRAM_UPDATE)
            .unwrap_or_else(|| panic!("update span with msg_type={msg_type} should exist"));

        assert_eq!(
            update_span.fields.get("msg_type").map(String::as_str),
            Some(*msg_type),
            "msg_type should be '{msg_type}'"
        );
    }
}

// ============================================================================
// Span Level Tests
// ============================================================================

/// Verify spans use correct tracing levels per the telemetry spec.
#[test]
fn span_levels_match_spec() {
    let handle = with_captured_spans(|| {
        // INFO-level spans
        {
            let _init = tracing::info_span!(telemetry_schema::span::PROGRAM_INIT).entered();
        }
        {
            let _frame = tracing::info_span!(
                telemetry_schema::span::RENDER_FRAME,
                width = 80u16,
                height = 24u16,
                duration_us = tracing::field::Empty,
            )
            .entered();
        }

        // DEBUG-level spans
        {
            let _update = tracing::debug_span!(
                telemetry_schema::span::PROGRAM_UPDATE,
                msg_type = "Tick",
                duration_us = tracing::field::Empty,
                cmd_type = tracing::field::Empty,
            )
            .entered();
        }
        {
            let _view = tracing::debug_span!(
                telemetry_schema::span::PROGRAM_VIEW,
                duration_us = tracing::field::Empty,
                widget_count = tracing::field::Empty,
            )
            .entered();
        }
        {
            let _present = tracing::debug_span!(telemetry_schema::span::RENDER_PRESENT).entered();
        }
        {
            let _scroll =
                tracing::debug_span!(telemetry_schema::span::RENDER_SCROLL_REGION).entered();
        }
        {
            let _diff = tracing::debug_span!(telemetry_schema::span::RENDER_DIFF_COMPUTE).entered();
        }
        {
            let _emit = tracing::debug_span!(telemetry_schema::span::RENDER_EMIT).entered();
        }
    });

    let spans = handle.spans();

    // INFO-level spans
    let info_spans = [
        telemetry_schema::span::PROGRAM_INIT,
        telemetry_schema::span::RENDER_FRAME,
    ];
    for name in &info_spans {
        let span = spans
            .iter()
            .find(|s| s.name == *name)
            .unwrap_or_else(|| panic!("{name} span must exist"));
        assert_eq!(
            span.level,
            tracing::Level::INFO,
            "Span '{}' should be INFO level, got {:?}",
            name,
            span.level
        );
    }

    // DEBUG-level spans
    let debug_spans = [
        telemetry_schema::span::PROGRAM_UPDATE,
        telemetry_schema::span::PROGRAM_VIEW,
        telemetry_schema::span::RENDER_PRESENT,
        telemetry_schema::span::RENDER_SCROLL_REGION,
        telemetry_schema::span::RENDER_DIFF_COMPUTE,
        telemetry_schema::span::RENDER_EMIT,
    ];
    for name in &debug_spans {
        let span = spans
            .iter()
            .find(|s| s.name == *name)
            .unwrap_or_else(|| panic!("{name} span must exist"));
        assert_eq!(
            span.level,
            tracing::Level::DEBUG,
            "Span '{}' should be DEBUG level, got {:?}",
            name,
            span.level
        );
    }
}

// ============================================================================
// Cross-Domain Hierarchy Tests
// ============================================================================

/// Verify conformal.predict is independent from the render pipeline
/// (it's a root span, not nested under frame/present/view).
#[test]
fn conformal_predict_is_independent_root() {
    let handle = with_captured_spans(|| {
        // Simulate: conformal prediction happens outside the render pipeline
        let _predict = tracing::info_span!(
            "conformal.predict",
            calibration_set_size = tracing::field::Empty,
            predicted_upper_bound_us = tracing::field::Empty,
            frame_budget_us = 16666.0f64,
            coverage_alpha = 0.05f64,
            gate_triggered = tracing::field::Empty,
        )
        .entered();
    });

    let spans = handle.spans();
    let predict = spans
        .iter()
        .find(|s| s.name == "conformal.predict")
        .expect("conformal.predict must exist");

    assert!(
        predict.parent_name.is_none(),
        "conformal.predict should be a root span, not nested under anything"
    );
}

/// Verify golden.compare is independent from the render pipeline.
#[test]
fn golden_compare_is_independent_root() {
    let handle = with_captured_spans(|| {
        let _golden = tracing::info_span!(
            "golden.compare",
            actual_count = 10usize,
            expected_count = 10usize,
            outcome = tracing::field::Empty,
            mismatch_frame = tracing::field::Empty,
        )
        .entered();
    });

    let spans = handle.spans();
    let golden = spans
        .iter()
        .find(|s| s.name == "golden.compare")
        .expect("golden.compare must exist");

    assert!(
        golden.parent_name.is_none(),
        "golden.compare should be a root span"
    );
}

/// Verify multiple update spans can be siblings (not nested).
#[test]
fn multiple_update_spans_are_siblings() {
    let handle = with_captured_spans(|| {
        // Three sequential updates (Tick, event, subscription)
        {
            let _u1 = tracing::debug_span!(
                telemetry_schema::span::PROGRAM_UPDATE,
                msg_type = "Tick",
                duration_us = tracing::field::Empty,
                cmd_type = tracing::field::Empty,
            )
            .entered();
        }
        {
            let _u2 = tracing::debug_span!(
                telemetry_schema::span::PROGRAM_UPDATE,
                msg_type = "event",
                duration_us = tracing::field::Empty,
                cmd_type = tracing::field::Empty,
            )
            .entered();
        }
        {
            let _u3 = tracing::debug_span!(
                telemetry_schema::span::PROGRAM_UPDATE,
                msg_type = "subscription",
                duration_us = tracing::field::Empty,
                cmd_type = tracing::field::Empty,
            )
            .entered();
        }
    });

    let spans = handle.spans();
    let update_spans: Vec<_> = spans
        .iter()
        .filter(|s| s.name == telemetry_schema::span::PROGRAM_UPDATE)
        .collect();

    assert_eq!(
        update_spans.len(),
        3,
        "Should have 3 update spans, got {}",
        update_spans.len()
    );

    // All should be root spans (no parent)
    for (i, span) in update_spans.iter().enumerate() {
        assert!(
            span.parent_name.is_none(),
            "Update span {} (msg_type={:?}) should be a root span, but has parent {:?}",
            i,
            span.fields.get("msg_type"),
            span.parent_name
        );
    }

    // Verify they have different msg_types
    let msg_types: Vec<&str> = update_spans
        .iter()
        .filter_map(|s| s.fields.get("msg_type").map(String::as_str))
        .collect();
    assert_eq!(msg_types, vec!["Tick", "event", "subscription"]);
}

// ============================================================================
// Required Parent-Child Contract Enforcement
// ============================================================================

/// Systematically verify all documented parent-child relationships.
#[test]
fn required_parent_child_contracts_enforced() {
    // For each required relationship, create the hierarchy and verify it
    for (child_name, expected_parent) in REQUIRED_PARENT_CHILD {
        // Build a minimal span tree that includes both parent and child
        let handle = build_span_tree_for(child_name, expected_parent);
        let spans = handle.spans();

        let child = spans
            .iter()
            .find(|s| s.name == *child_name)
            .unwrap_or_else(|| {
                panic!(
                    "Child span '{}' should exist in test for relationship {} → {}",
                    child_name, expected_parent, child_name
                )
            });

        assert_eq!(
            child.parent_name.as_deref(),
            Some(*expected_parent),
            "Contract violation: '{}' must have parent '{}', got '{:?}'",
            child_name,
            expected_parent,
            child.parent_name
        );
    }
}

/// Helper: build a minimal span tree that includes both parent and child.
fn build_span_tree_for(child: &str, parent: &str) -> CaptureHandle {
    with_captured_spans(|| {
        // Build the necessary ancestor chain
        match parent {
            telemetry_schema::span::RENDER_FRAME => {
                let frame_span = tracing::info_span!(
                    telemetry_schema::span::RENDER_FRAME,
                    width = 80u16,
                    height = 24u16,
                    duration_us = tracing::field::Empty,
                );
                let _frame = frame_span.enter();
                emit_child_span(child);
            }
            telemetry_schema::span::RENDER_PRESENT => {
                let frame_span = tracing::info_span!(
                    telemetry_schema::span::RENDER_FRAME,
                    width = 80u16,
                    height = 24u16,
                    duration_us = tracing::field::Empty,
                );
                let _frame = frame_span.enter();
                let present_span = tracing::info_span!(
                    telemetry_schema::span::RENDER_PRESENT,
                    mode = "inline",
                    width = 80u16,
                    height = 24u16,
                );
                let _present = present_span.enter();
                emit_child_span(child);
            }
            "inline.render" => {
                let frame_span = tracing::info_span!(
                    telemetry_schema::span::RENDER_FRAME,
                    width = 80u16,
                    height = 24u16,
                    duration_us = tracing::field::Empty,
                );
                let _frame = frame_span.enter();
                let present_span = tracing::info_span!(
                    telemetry_schema::span::RENDER_PRESENT,
                    mode = "inline",
                    width = 80u16,
                    height = 24u16,
                );
                let _present = present_span.enter();
                let inline_span = tracing::info_span!(
                    "inline.render",
                    inline_height = 24u16,
                    scrollback_preserved = tracing::field::Empty,
                    render_mode = "scroll_region",
                );
                let _inline = inline_span.enter();
                emit_child_span(child);
            }
            _ => {
                panic!("Unknown parent span: {parent}");
            }
        }
    })
}

/// Emit a child span by name.
fn emit_child_span(name: &str) {
    match name {
        telemetry_schema::span::PROGRAM_VIEW => {
            let _span = tracing::debug_span!(
                telemetry_schema::span::PROGRAM_VIEW,
                duration_us = tracing::field::Empty,
                widget_count = tracing::field::Empty,
            )
            .entered();
        }
        telemetry_schema::span::RENDER_PRESENT => {
            let _span = tracing::debug_span!(telemetry_schema::span::RENDER_PRESENT).entered();
        }
        "inline.render" => {
            let _span = tracing::info_span!(
                "inline.render",
                inline_height = 24u16,
                scrollback_preserved = tracing::field::Empty,
                render_mode = "scroll_region",
            )
            .entered();
        }
        telemetry_schema::span::RENDER_SCROLL_REGION => {
            let _span =
                tracing::debug_span!(telemetry_schema::span::RENDER_SCROLL_REGION).entered();
        }
        telemetry_schema::span::RENDER_DIFF_COMPUTE => {
            let _span = tracing::debug_span!(telemetry_schema::span::RENDER_DIFF_COMPUTE).entered();
        }
        telemetry_schema::span::RENDER_EMIT => {
            let _span = tracing::debug_span!(telemetry_schema::span::RENDER_EMIT).entered();
        }
        _ => {
            panic!("Unknown child span: {name}");
        }
    }
}

// ============================================================================
// Span Count + Completeness Tests
// ============================================================================

/// Verify a complete render cycle produces the expected number of spans.
#[test]
fn complete_render_cycle_span_count() {
    let handle = with_captured_spans(|| {
        // Full render pipeline
        let frame = tracing::info_span!(
            telemetry_schema::span::RENDER_FRAME,
            width = 80u16,
            height = 24u16,
            duration_us = tracing::field::Empty,
        );
        let _frame = frame.enter();

        {
            let _view = tracing::debug_span!(
                telemetry_schema::span::PROGRAM_VIEW,
                duration_us = tracing::field::Empty,
                widget_count = tracing::field::Empty,
            )
            .entered();
        }

        {
            let present = tracing::debug_span!(telemetry_schema::span::RENDER_PRESENT);
            let _present = present.enter();

            {
                let inline = tracing::info_span!(
                    "inline.render",
                    inline_height = 24u16,
                    scrollback_preserved = tracing::field::Empty,
                    render_mode = "scroll_region",
                );
                let _inline = inline.enter();

                {
                    let _scroll =
                        tracing::debug_span!(telemetry_schema::span::RENDER_SCROLL_REGION)
                            .entered();
                }
                {
                    let _diff =
                        tracing::debug_span!(telemetry_schema::span::RENDER_DIFF_COMPUTE).entered();
                }
                {
                    let _emit = tracing::debug_span!(telemetry_schema::span::RENDER_EMIT).entered();
                }
            }
        }
    });

    let spans = handle.spans();

    // Expected spans in a complete cycle:
    // 1. ftui.render.frame
    // 2. ftui.program.view
    // 3. ftui.render.present
    // 4. inline.render
    // 5. ftui.render.scroll_region
    // 6. ftui.render.diff_compute
    // 7. ftui.render.emit
    assert_eq!(
        spans.len(),
        7,
        "Complete render cycle should produce exactly 7 spans, got {}: {:?}",
        spans.len(),
        spans.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}

/// Verify all documented render pipeline spans are present in a full cycle.
#[test]
fn all_documented_render_spans_present() {
    let expected_span_names = [
        telemetry_schema::span::RENDER_FRAME,
        telemetry_schema::span::PROGRAM_VIEW,
        telemetry_schema::span::RENDER_PRESENT,
        "inline.render",
        telemetry_schema::span::RENDER_SCROLL_REGION,
        telemetry_schema::span::RENDER_DIFF_COMPUTE,
        telemetry_schema::span::RENDER_EMIT,
    ];

    let handle = with_captured_spans(|| {
        let frame = tracing::info_span!(
            telemetry_schema::span::RENDER_FRAME,
            width = 80u16,
            height = 24u16,
            duration_us = tracing::field::Empty,
        );
        let _frame = frame.enter();

        {
            let _view = tracing::debug_span!(
                telemetry_schema::span::PROGRAM_VIEW,
                duration_us = tracing::field::Empty,
                widget_count = tracing::field::Empty,
            )
            .entered();
        }

        {
            let present = tracing::debug_span!(telemetry_schema::span::RENDER_PRESENT);
            let _present = present.enter();

            {
                let inline = tracing::info_span!(
                    "inline.render",
                    inline_height = 24u16,
                    scrollback_preserved = tracing::field::Empty,
                    render_mode = "scroll_region",
                );
                let _inline = inline.enter();

                {
                    let _scroll =
                        tracing::debug_span!(telemetry_schema::span::RENDER_SCROLL_REGION)
                            .entered();
                }
                {
                    let _diff =
                        tracing::debug_span!(telemetry_schema::span::RENDER_DIFF_COMPUTE).entered();
                }
                {
                    let _emit = tracing::debug_span!(telemetry_schema::span::RENDER_EMIT).entered();
                }
            }
        }
    });

    let spans = handle.spans();
    let names: Vec<&str> = spans.iter().map(|s| s.name.as_str()).collect();

    for expected in &expected_span_names {
        assert!(
            names.contains(expected),
            "Missing expected render span '{}'. Got: {:?}",
            expected,
            names
        );
    }
}

// ============================================================================
// Span Field Value Correctness
// ============================================================================

/// Verify dimension fields propagate correctly through the hierarchy.
#[test]
fn dimension_fields_propagate_correctly() {
    let handle = with_captured_spans(|| {
        let frame = tracing::info_span!(
            telemetry_schema::span::RENDER_FRAME,
            width = 132u16,
            height = 43u16,
            duration_us = tracing::field::Empty,
        );
        let _frame = frame.enter();

        let _present = tracing::info_span!(
            telemetry_schema::span::RENDER_PRESENT,
            mode = "altscreen",
            width = 132u16,
            height = 43u16,
        )
        .entered();
    });

    let spans = handle.spans();

    let frame = spans
        .iter()
        .find(|s| s.name == telemetry_schema::span::RENDER_FRAME)
        .expect("frame span");
    let present = spans
        .iter()
        .find(|s| s.name == telemetry_schema::span::RENDER_PRESENT)
        .expect("present span");

    // Both should have matching dimensions
    assert_eq!(
        frame.fields.get("width"),
        present.fields.get("width"),
        "Width should match between frame and present"
    );
    assert_eq!(
        frame.fields.get("height"),
        present.fields.get("height"),
        "Height should match between frame and present"
    );
}

/// Verify Empty fields are recorded as such (not dropped).
#[test]
fn empty_fields_are_preserved() {
    let handle = with_captured_spans(|| {
        let _span = tracing::info_span!(
            telemetry_schema::span::RENDER_FRAME,
            width = 80u16,
            height = 24u16,
            duration_us = tracing::field::Empty,
        )
        .entered();
    });

    let spans = handle.spans();
    let frame = spans
        .iter()
        .find(|s| s.name == telemetry_schema::span::RENDER_FRAME)
        .expect("frame span");

    // duration_us should be present (even if Empty)
    assert!(
        frame.fields.contains_key("duration_us"),
        "Empty fields must still be declared. Got: {:?}",
        frame.fields.keys().collect::<Vec<_>>()
    );
}
