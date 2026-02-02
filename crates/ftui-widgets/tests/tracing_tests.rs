#![forbid(unsafe_code)]

//! Performance tracing integration tests.
//!
//! These tests verify that tracing span instrumentation in ftui-widgets
//! and ftui-render works correctly.
//!
//! Widget spans enabled:
//!   cargo test -p ftui-widgets --features tracing --test tracing_tests
//!
//! Zero-overhead verification (no feature):
//!   cargo test -p ftui-widgets --test tracing_tests -- zero_overhead

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ftui_core::geometry::Rect;
use ftui_layout::Constraint;
#[cfg(feature = "tracing")]
use ftui_render::buffer::Buffer;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
#[cfg(feature = "tracing")]
use ftui_widgets::StatefulWidget;
use ftui_widgets::Widget;
use ftui_widgets::block::Block;
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::table::{Row, Table};

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;

// ============================================================================
// Test Infrastructure
// ============================================================================

/// A captured span with its metadata and parent info.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct CapturedSpan {
    name: String,
    fields: HashMap<String, String>,
    parent_name: Option<String>,
}

/// Timing record for a span.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct SpanTiming {
    name: String,
    enter: Instant,
    duration: Option<std::time::Duration>,
}

/// A tracing Layer that captures span metadata and timing.
struct SpanCapture {
    spans: Arc<Mutex<Vec<CapturedSpan>>>,
    timings: Arc<Mutex<HashMap<tracing::span::Id, SpanTiming>>>,
    completed_timings: Arc<Mutex<Vec<SpanTiming>>>,
}

impl SpanCapture {
    fn new() -> (Self, CaptureHandle) {
        let spans = Arc::new(Mutex::new(Vec::new()));
        let timings = Arc::new(Mutex::new(HashMap::new()));
        let completed_timings = Arc::new(Mutex::new(Vec::new()));

        let handle = CaptureHandle {
            spans: spans.clone(),
            completed_timings: completed_timings.clone(),
        };

        let layer = Self {
            spans,
            timings,
            completed_timings,
        };

        (layer, handle)
    }
}

/// Handle to read captured spans after rendering.
#[allow(dead_code)]
struct CaptureHandle {
    spans: Arc<Mutex<Vec<CapturedSpan>>>,
    completed_timings: Arc<Mutex<Vec<SpanTiming>>>,
}

impl CaptureHandle {
    fn spans(&self) -> Vec<CapturedSpan> {
        self.spans.lock().unwrap().clone()
    }

    #[allow(dead_code)]
    fn timings(&self) -> Vec<SpanTiming> {
        self.completed_timings.lock().unwrap().clone()
    }
}

/// Visitor that extracts span fields.
struct FieldVisitor(Vec<(String, String)>);

impl tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0
            .push((field.name().to_string(), format!("{value:?}")));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.0.push((field.name().to_string(), value.to_string()));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
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
            fields,
            parent_name,
        });
    }

    fn on_enter(&self, id: &tracing::span::Id, ctx: tracing_subscriber::layer::Context<'_, S>) {
        if let Some(span_ref) = ctx.span(id) {
            self.timings.lock().unwrap().insert(
                id.clone(),
                SpanTiming {
                    name: span_ref.name().to_string(),
                    enter: Instant::now(),
                    duration: None,
                },
            );
        }
    }

    fn on_exit(&self, id: &tracing::span::Id, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        let mut timings = self.timings.lock().unwrap();
        if let Some(timing) = timings.get_mut(id) {
            timing.duration = Some(timing.enter.elapsed());
        }
    }

    fn on_close(&self, id: tracing::span::Id, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        if let Some(timing) = self.timings.lock().unwrap().remove(&id) {
            self.completed_timings.lock().unwrap().push(timing);
        }
    }
}

/// Set up a tracing subscriber with span capture and run a closure.
fn with_captured_spans<F>(f: F) -> CaptureHandle
where
    F: FnOnce(),
{
    let (layer, handle) = SpanCapture::new();
    let subscriber = tracing_subscriber::registry().with(layer);
    tracing::subscriber::with_default(subscriber, f);
    handle
}

// ============================================================================
// Unit Tests
// ============================================================================

/// Verify that widget render spans are created for all render phases.
///
/// Tests: spans_created_for_render_phases
#[test]
#[cfg(feature = "tracing")]
fn spans_created_for_render_phases() {
    let handle = with_captured_spans(|| {
        let area = Rect::new(0, 0, 20, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);

        // Block
        Block::bordered().render(area, &mut frame);

        // Paragraph
        Paragraph::new(ftui_text::Text::raw("Hello")).render(area, &mut frame);

        // Table (delegates to StatefulWidget internally)
        let table = Table::new(
            [Row::new(["A", "B"])],
            [Constraint::Fixed(5), Constraint::Fixed(5)],
        );
        Widget::render(&table, area, &mut frame);
    });

    let spans = handle.spans();
    let widget_spans: Vec<_> = spans.iter().filter(|s| s.name == "widget_render").collect();

    // Should have at least Block, Paragraph, Table spans
    assert!(
        widget_spans.len() >= 3,
        "Expected at least 3 widget_render spans, got {}",
        widget_spans.len()
    );

    let widget_names: Vec<_> = widget_spans
        .iter()
        .filter_map(|s| s.fields.get("widget"))
        .collect();

    assert!(
        widget_names.iter().any(|n| n.contains("Block")),
        "Should have a Block span, got: {widget_names:?}"
    );
    assert!(
        widget_names.iter().any(|n| n.contains("Paragraph")),
        "Should have a Paragraph span, got: {widget_names:?}"
    );
    assert!(
        widget_names.iter().any(|n| n.contains("Table")),
        "Should have a Table span, got: {widget_names:?}"
    );
}

/// Verify that span timing is recorded and reasonable.
///
/// Tests: span_timing_accurate
#[test]
#[cfg(feature = "tracing")]
fn span_timing_accurate() {
    let handle = with_captured_spans(|| {
        let area = Rect::new(0, 0, 80, 24);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);

        // Render a reasonably complex table
        let table = Table::new(
            (0..20).map(|i| Row::new([format!("Row {i}"), format!("Value {}", i * 2)])),
            [Constraint::Fixed(20), Constraint::Fixed(20)],
        )
        .header(Row::new(["Name", "Value"]))
        .block(Block::bordered());

        Widget::render(&table, area, &mut frame);
    });

    let timings = handle.timings();
    let widget_timings: Vec<_> = timings
        .iter()
        .filter(|t| t.name == "widget_render")
        .collect();

    assert!(
        !widget_timings.is_empty(),
        "Should have widget_render timing records"
    );

    for timing in &widget_timings {
        let duration = timing.duration.expect("span should have a duration");
        // Duration should be reasonable (less than 1 second for widget rendering)
        assert!(
            duration < std::time::Duration::from_secs(1),
            "Widget render took unreasonably long: {duration:?}",
        );
    }
}

/// Verify that parent-child span relationships are preserved.
///
/// Tests: spans_nest_correctly
#[test]
#[cfg(feature = "tracing")]
fn spans_nest_correctly() {
    let handle = with_captured_spans(|| {
        let area = Rect::new(0, 0, 20, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 5, &mut pool);

        // Table with a Block — Block's span should be child of Table's span
        let table =
            Table::new([Row::new(["Data"])], [Constraint::Fixed(10)]).block(Block::bordered());

        Widget::render(&table, area, &mut frame);
    });

    let spans = handle.spans();
    let widget_spans: Vec<_> = spans.iter().filter(|s| s.name == "widget_render").collect();

    // Find the Block span that should be a child of Table
    let block_span = widget_spans
        .iter()
        .find(|s| s.fields.get("widget").is_some_and(|w| w.contains("Block")));

    assert!(block_span.is_some(), "Should have a Block widget span");
    let block_span = block_span.unwrap();

    // Block's parent should be the Table's widget_render span
    assert_eq!(
        block_span.parent_name.as_deref(),
        Some("widget_render"),
        "Block span should be nested under Table's widget_render span"
    );
}

/// Verify zero overhead when the tracing feature is disabled.
///
/// Tests: zero_overhead_when_disabled
///
/// When compiled WITHOUT `--features tracing`, the `#[cfg(feature = "tracing")]`
/// blocks are entirely removed by the compiler. This test verifies that no
/// widget_render spans appear in that case.
#[test]
fn zero_overhead_when_disabled() {
    let handle = with_captured_spans(|| {
        let area = Rect::new(0, 0, 20, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 5, &mut pool);

        Block::bordered().render(area, &mut frame);
        Paragraph::new(ftui_text::Text::raw("test")).render(area, &mut frame);

        let table = Table::new([Row::new(["X"])], [Constraint::Fixed(5)]);
        Widget::render(&table, area, &mut frame);
    });

    let spans = handle.spans();
    let widget_spans: Vec<_> = spans.iter().filter(|s| s.name == "widget_render").collect();

    #[cfg(feature = "tracing")]
    assert!(
        !widget_spans.is_empty(),
        "With tracing feature, widget_render spans should be present"
    );

    #[cfg(not(feature = "tracing"))]
    assert!(
        widget_spans.is_empty(),
        "Without tracing feature, no widget_render spans should exist (got {})",
        widget_spans.len()
    );
}

// ============================================================================
// Integration Tests
// ============================================================================

/// Verify all widget types emit spans to the subscriber.
///
/// Tests: tracing_subscriber_receives_all_spans
#[test]
#[cfg(feature = "tracing")]
fn tracing_subscriber_receives_all_spans() {
    let handle = with_captured_spans(|| {
        let area = Rect::new(0, 0, 40, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);

        // Block
        Block::bordered().render(area, &mut frame);

        // Paragraph
        Paragraph::new(ftui_text::Text::raw("Hello")).render(area, &mut frame);

        // Table
        let table = Table::new([Row::new(["A"])], [Constraint::Fixed(10)]);
        Widget::render(&table, area, &mut frame);

        // Spinner
        use ftui_widgets::spinner::{Spinner, SpinnerState};
        let spinner = Spinner::new();
        let mut state = SpinnerState::default();
        StatefulWidget::render(&spinner, area, &mut frame, &mut state);

        // ProgressBar
        use ftui_widgets::progress::ProgressBar;
        ProgressBar::new().ratio(0.5).render(area, &mut frame);

        // Rule
        use ftui_widgets::rule::Rule;
        Rule::new().render(area, &mut frame);

        // Scrollbar
        use ftui_widgets::scrollbar::{Scrollbar, ScrollbarOrientation, ScrollbarState};
        let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        let mut sb_state = ScrollbarState::new(100, 0, 10);
        StatefulWidget::render(&sb, area, &mut frame, &mut sb_state);

        // List
        use ftui_widgets::list::{List, ListItem, ListState};
        let list = List::new([ListItem::new("item")]);
        let mut list_state = ListState::default();
        StatefulWidget::render(&list, area, &mut frame, &mut list_state);

        // TextInput
        use ftui_widgets::input::TextInput;
        TextInput::new().with_value("hello").render(area, &mut frame);
    });

    let spans = handle.spans();
    let widget_names: Vec<String> = spans
        .iter()
        .filter(|s| s.name == "widget_render")
        .filter_map(|s| s.fields.get("widget").cloned())
        .collect();

    let expected = [
        "Block",
        "Paragraph",
        "Table",
        "Spinner",
        "ProgressBar",
        "Rule",
        "Scrollbar",
        "List",
        "TextInput",
    ];

    for name in &expected {
        assert!(
            widget_names.iter().any(|n| n.contains(name)),
            "Missing widget_render span for {name}. Got: {widget_names:?}"
        );
    }
}

/// Full frame render produces expected spans across the pipeline.
///
/// Tests: real_render_loop_traced
#[test]
#[cfg(feature = "tracing")]
fn real_render_loop_traced() {
    use ftui_render::diff::BufferDiff;

    let handle = with_captured_spans(|| {
        let area = Rect::new(0, 0, 40, 10);

        // Simulate a real render loop: widget render → diff
        let current = Buffer::new(40, 10);
        let mut next = Buffer::new(40, 10);

        // Render widgets into the next buffer via Frame
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);

        let table = Table::new(
            [Row::new(["Name", "Value"]), Row::new(["foo", "42"])],
            [Constraint::Fixed(15), Constraint::Fixed(15)],
        )
        .header(Row::new(["Col A", "Col B"]))
        .block(Block::bordered());

        Widget::render(&table, area, &mut frame);

        // Copy frame buffer into next for diffing
        next = frame.buffer;

        // Compute diff (has tracing spans in ftui-render via feature propagation)
        let diff = BufferDiff::compute(&current, &next);
        assert!(!diff.is_empty(), "Should have changes");

        // Coalesce into runs (also traced)
        let _runs = diff.runs();
    });

    let spans = handle.spans();

    // Widget render spans
    let widget_spans: Vec<_> = spans.iter().filter(|s| s.name == "widget_render").collect();
    assert!(
        !widget_spans.is_empty(),
        "Should have widget_render spans from rendering"
    );

    // Diff compute span (from ftui-render with tracing feature propagation)
    let diff_spans: Vec<_> = spans.iter().filter(|s| s.name == "diff_compute").collect();
    assert!(
        !diff_spans.is_empty(),
        "Should have diff_compute span from BufferDiff::compute"
    );

    // Verify diff span has dimension fields
    let diff_span = &diff_spans[0];
    assert!(
        diff_span.fields.contains_key("width"),
        "diff_compute span should have width field"
    );
    assert!(
        diff_span.fields.contains_key("height"),
        "diff_compute span should have height field"
    );

    // Diff runs span
    let runs_spans: Vec<_> = spans.iter().filter(|s| s.name == "diff_runs").collect();
    assert!(
        !runs_spans.is_empty(),
        "Should have diff_runs span from runs()"
    );
}

/// Widget render spans include correct area dimensions.
#[test]
#[cfg(feature = "tracing")]
fn span_fields_contain_area_dimensions() {
    let handle = with_captured_spans(|| {
        let area = Rect::new(5, 10, 30, 15);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 30, &mut pool);

        Block::bordered().render(area, &mut frame);
    });

    let spans = handle.spans();
    let block_span = spans
        .iter()
        .find(|s| {
            s.name == "widget_render" && s.fields.get("widget").is_some_and(|w| w.contains("Block"))
        })
        .expect("Should have a Block widget_render span");

    assert_eq!(block_span.fields.get("x").map(String::as_str), Some("5"));
    assert_eq!(block_span.fields.get("y").map(String::as_str), Some("10"));
    assert_eq!(block_span.fields.get("w").map(String::as_str), Some("30"));
    assert_eq!(block_span.fields.get("h").map(String::as_str), Some("15"));
}
