#![forbid(unsafe_code)]

//! Integration tests for HeadlessTerm (bd-mevj).
//!
//! NOTE: Moved from `ftui-render/tests/headless_integration.rs` to avoid
//! publish-time dev-dep cycles while retaining full widget/layout coverage.
//!
//! Tests the headless terminal in realistic scenarios:
//! - Snapshot test workflow (diff + present → HeadlessTerm → assert)
//! - Complex widget layouts rendered through the full pipeline
//! - Style codes (SGR) producing correct cell attributes
//! - Property tests for robustness

use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, CellAttrs, PackedRgba, StyleFlags};
use ftui_render::diff::BufferDiff;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_render::headless::HeadlessTerm;
use ftui_render::presenter::{ColorDepth, Presenter, TerminalCapabilities};

// ============================================================================
// Helper: render a buffer through the presenter pipeline into a HeadlessTerm
// ============================================================================

/// Render `next` buffer (diffed against `prev`) through the presenter,
/// feed the ANSI output into a HeadlessTerm, and return it.
fn truecolor_capabilities() -> TerminalCapabilities {
    let capabilities = TerminalCapabilities::builder()
        .color_depth(ColorDepth::TrueColor)
        .build();
    assert_eq!(capabilities.color_depth, ColorDepth::TrueColor);
    capabilities
}

fn present_into_headless(prev: &Buffer, next: &Buffer) -> HeadlessTerm {
    let diff = BufferDiff::compute(prev, next);
    let caps = truecolor_capabilities();
    let output = {
        let mut sink = Vec::new();
        let mut presenter = Presenter::new(&mut sink, caps);
        presenter.present(next, &diff).unwrap();
        drop(presenter);
        sink
    };

    let mut term = HeadlessTerm::new(next.width(), next.height());
    term.process(&output);
    term
}

const WIDGET_CANARIES: [&str; 5] = [
    "INPUT_SECRET_450512_é日",
    "TEXTAREA_SECRET_450512_é日",
    "LABEL_SECRET_450512_é日",
    "MODAL_SECRET_450512_é日",
    "LIVE_SECRET_450512_é日",
];

fn exercise_widget_accessibility(enabled: bool, include_text: bool, focus_textarea: bool) {
    use ftui_a11y::node::LiveRegion;
    use ftui_core::event::Event;
    use ftui_runtime::evidence_sink::EvidenceSinkConfig;
    use ftui_runtime::program::{AccessibilityFrame, HeadlessEventSource, Program, ProgramConfig};
    use ftui_runtime::{
        BackendFeatures, Cmd, Model, ScreenMode, ScreenReaderPolicy, TerminalWriter, UiAnchor,
    };
    use ftui_widgets::Widget;
    use ftui_widgets::input::TextInput;
    use ftui_widgets::modal::Modal;
    use ftui_widgets::paragraph::Paragraph;
    use ftui_widgets::textarea::TextArea;
    use std::time::Duration;

    struct PrivateWidgets {
        input: TextInput,
        textarea: TextArea,
        local_tree: String,
        local_announcements: Vec<String>,
    }
    impl Model for PrivateWidgets {
        type Message = Event;

        fn init(&mut self) -> Cmd<Event> {
            Cmd::tick(Duration::from_millis(10))
        }

        fn update(&mut self, _: Event) -> Cmd<Event> {
            Cmd::Quit
        }

        fn view(&self, frame: &mut Frame) {
            self.input.render(Rect::new(0, 0, 80, 1), frame);
            self.textarea.render(Rect::new(0, 2, 80, 2), frame);
            Paragraph::new(WIDGET_CANARIES[2]).render(Rect::new(0, 5, 80, 1), frame);
            Modal::new(Paragraph::new(WIDGET_CANARIES[3])).render(Rect::new(0, 7, 80, 10), frame);
            Paragraph::new(WIDGET_CANARIES[4]).render(Rect::new(0, 18, 80, 1), frame);

            // Applications designate live regions through the public frame
            // builder. Decorate the nodes emitted by the actual widgets;
            // their names still come from widget rendering, not test nodes.
            let order = frame.a11y_order().to_vec();
            if let Some(builder) = frame.a11y.as_deref_mut() {
                for id in order {
                    if let Some(node) = builder.node_mut(id)
                        && node.name.as_deref().is_some_and(|name| {
                            WIDGET_CANARIES[2..]
                                .iter()
                                .any(|canary| name.contains(canary))
                        })
                    {
                        node.live_region = Some(LiveRegion::Polite);
                    }
                }
            }
        }

        fn on_accessibility(&mut self, a11y: AccessibilityFrame<'_>) -> Cmd<Event> {
            self.local_tree = a11y.dump();
            self.local_announcements
                .extend(a11y.announcements.iter().map(|item| item.text.clone()));
            Cmd::Quit
        }
    }

    let mut input = TextInput::new().with_focused(!focus_textarea);
    input.set_value(WIDGET_CANARIES[0]);
    let mut textarea =
        TextArea::new().with_text(&format!("{}\nsecond \"line\" \\ end", WIDGET_CANARIES[1]));
    textarea.set_focused(focus_textarea);
    let model = PrivateWidgets {
        input,
        textarea,
        local_tree: String::new(),
        local_announcements: Vec::new(),
    };
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "ftui-widget-privacy-{}-{suffix}-{enabled}-{include_text}-{focus_textarea}.jsonl",
        std::process::id()
    ));
    let features = BackendFeatures::default();
    let writer = TerminalWriter::new(
        Vec::new(),
        ScreenMode::AltScreen,
        UiAnchor::Bottom,
        TerminalCapabilities::default(),
    );
    let mut config = ProgramConfig::default()
        .with_accessibility_evidence_text(include_text)
        .with_evidence_sink(EvidenceSinkConfig::enabled_file(&path));
    if enabled {
        config = config.with_accessibility(ScreenReaderPolicy::default());
    }
    let mut program = Program::with_event_source(
        model,
        HeadlessEventSource::new(80, 20, features),
        features,
        writer,
        config,
    )
    .expect("construct actual runtime");
    program
        .run()
        .expect("run actual widget and presentation pipeline");
    let model = program.model();
    if enabled {
        for canary in WIDGET_CANARIES {
            assert!(
                model.local_tree.contains(canary),
                "missing actual widget node: {canary}"
            );
        }
        for index in [usize::from(focus_textarea), 2, 3, 4] {
            assert!(
                model
                    .local_announcements
                    .iter()
                    .any(|text| text.contains(WIDGET_CANARIES[index])),
                "local channel did not announce {}",
                WIDGET_CANARIES[index]
            );
        }
    } else {
        assert!(model.local_tree.is_empty());
        assert!(model.local_announcements.is_empty());
    }
    let evidence = std::fs::read_to_string(&path).expect("read runtime evidence");
    let announcements: Vec<serde_json::Value> = evidence
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid evidence JSONL"))
        .filter(|row: &serde_json::Value| row["event"] == "a11y_announcement")
        .collect();
    assert_eq!(announcements.len(), model.local_announcements.len());
    if enabled && include_text {
        for text in &model.local_announcements {
            assert!(
                announcements
                    .iter()
                    .any(|row| row["text"].as_str() == Some(text.as_str()))
            );
        }
    } else {
        assert_no_widget_content(&evidence);
        assert!(announcements.iter().all(|row| row["text"].is_null()));
    }
}

fn assert_no_widget_content(output: &str) {
    for canary in WIDGET_CANARIES {
        let escaped = serde_json::to_string(canary).expect("serialize canary");
        assert!(!output.contains(canary), "raw widget content leaked");
        assert!(
            !output.contains(escaped.trim_matches('"')),
            "escaped widget content leaked"
        );
        let marker = canary.split('_').next().unwrap();
        assert!(
            !output.contains(&format!("{marker}_SECRET_450512")),
            "widget content marker leaked"
        );
    }
}

#[derive(Clone, Default)]
struct WidgetTraceBuffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for WidgetTraceBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("trace buffer lock")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn widget_accessibility_content_stays_local_by_default() {
    for enabled in [false, true] {
        for include_text in [false, true] {
            for focus_textarea in [false, true] {
                let capture = WidgetTraceBuffer::default();
                let writer = capture.clone();
                let subscriber = tracing_subscriber::fmt()
                    .with_ansi(false)
                    .without_time()
                    .with_max_level(tracing::Level::TRACE)
                    .with_writer(move || writer.clone())
                    .finish();
                tracing::subscriber::with_default(subscriber, || {
                    exercise_widget_accessibility(enabled, include_text, focus_textarea);
                });
                let bytes = capture.0.lock().expect("trace buffer lock");
                let trace = std::str::from_utf8(&bytes).expect("UTF-8 tracing");
                assert_no_widget_content(trace);
                assert_eq!(trace.contains("screen reader announcement"), enabled);
            }
        }
    }
}

#[cfg(feature = "telemetry")]
#[test]
fn widget_accessibility_content_stays_out_of_real_otlp_exports() {
    use ftui_runtime::telemetry::{SpanProcessorKind, TelemetryConfig};
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    use tracing_subscriber::prelude::*;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local collector");
    listener.set_nonblocking(true).expect("bounded accept");
    let endpoint = format!("http://{}/v1/traces", listener.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let collector_stop = stop.clone();
    let collector = std::thread::spawn(move || {
        let start = Instant::now();
        let mut payloads = Vec::new();
        while !collector_stop.load(Ordering::Acquire) && start.elapsed() < Duration::from_secs(30) {
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("collector accept failed: {error}"),
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("bounded request read");
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).expect("read HTTP headers");
                headers.push(byte[0]);
                assert!(headers.len() <= 32_768, "bounded HTTP headers");
            }
            let headers = std::str::from_utf8(&headers).expect("HTTP header text");
            assert!(headers.starts_with("POST /v1/traces "));
            let length: usize = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .expect("OTLP content length")
                .1
                .trim()
                .parse()
                .expect("numeric content length");
            assert!(length <= 2 * 1024 * 1024, "bounded request body");
            let mut body = vec![0; length];
            stream
                .read_exact(&mut body)
                .expect("read actual OTLP protobuf");
            payloads.push(body);
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/x-protobuf\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").expect("acknowledge export");
        }
        payloads
    });

    let mut config = TelemetryConfig::from_env_with(|key| match key {
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT" => Some(endpoint.clone()),
        "OTEL_TRACES_EXPORTER" => Some("otlp".into()),
        _ => None,
    });
    config.processor = SpanProcessorKind::Simple;
    config.headers.push((
        "User-Agent".into(),
        "OpenAI File Downloader, XaiImageApiFetch/1.0".into(),
    ));
    let (layer, provider) = config.build_layer().expect("real OTLP exporter");
    let subscriber = tracing_subscriber::registry().with(layer);
    tracing::subscriber::with_default(subscriber, || {
        for include_text in [false, true] {
            for focus_textarea in [false, true] {
                let _span =
                    tracing::info_span!("widget_privacy_export", include_text, focus_textarea)
                        .entered();
                exercise_widget_accessibility(true, include_text, focus_textarea);
            }
        }
    });
    provider.force_flush().expect("flush actual export");
    provider.shutdown().expect("shutdown exporter");
    stop.store(true, Ordering::Release);
    let payloads = collector.join().expect("collector finished");
    assert!(!payloads.is_empty(), "zero exports is not a privacy pass");
    let exported = payloads.concat();
    let text = String::from_utf8_lossy(&exported);
    assert!(
        text.contains("widget_privacy_export"),
        "actual span must be exported"
    );
    assert!(
        text.contains("screen reader announcement"),
        "actual announcement metadata must be exported"
    );
    assert_no_widget_content(&text);
}

// ============================================================================
// Snapshot test workflow
// ============================================================================

#[test]
fn snapshot_workflow_basic() {
    // Simulate a full snapshot test: render → present → headless → assert
    let prev = Buffer::new(20, 5);
    let mut next = Buffer::new(20, 5);

    // Write "Hello" on row 0
    for (i, ch) in "Hello".chars().enumerate() {
        next.set(i as u16, 0, Cell::from_char(ch));
    }
    // Write "World" on row 2
    for (i, ch) in "World".chars().enumerate() {
        next.set(i as u16, 2, Cell::from_char(ch));
    }

    let term = present_into_headless(&prev, &next);

    // Snapshot assertion
    term.assert_matches(&["Hello", "", "World", "", ""]);
}

#[test]
fn snapshot_workflow_incremental_update() {
    // Frame 1: initial content
    let prev1 = Buffer::new(20, 3);
    let mut next1 = Buffer::new(20, 3);
    for (i, ch) in "Frame One".chars().enumerate() {
        next1.set(i as u16, 0, Cell::from_char(ch));
    }

    let mut term = present_into_headless(&prev1, &next1);
    term.assert_row(0, "Frame One");

    // Frame 2: incremental update (change row 0, add row 1)
    let prev2 = next1.clone();
    let mut next2 = next1;
    for (i, ch) in "Frame Two".chars().enumerate() {
        next2.set(i as u16, 0, Cell::from_char(ch));
    }
    for (i, ch) in "New Line".chars().enumerate() {
        next2.set(i as u16, 1, Cell::from_char(ch));
    }

    let diff = BufferDiff::compute(&prev2, &next2);
    let caps = TerminalCapabilities::default();
    let output = {
        let mut sink = Vec::new();
        let mut p = Presenter::new(&mut sink, caps);
        p.present(&next2, &diff).unwrap();
        drop(p);
        sink
    };
    term.process(&output);

    term.assert_row(0, "Frame Two");
    term.assert_row(1, "New Line");
}

#[test]
fn snapshot_diff_helper_detects_changes() {
    let prev = Buffer::new(10, 3);
    let mut next = Buffer::new(10, 3);
    for (i, ch) in "ABC".chars().enumerate() {
        next.set(i as u16, 0, Cell::from_char(ch));
    }

    let term = present_into_headless(&prev, &next);

    // Diff against expected
    assert!(
        term.diff(&["ABC", "", ""]).is_none(),
        "should match exactly"
    );

    // Diff against wrong content
    let diff = term.diff(&["XYZ", "", ""]).unwrap();
    assert_eq!(diff.mismatches.len(), 1);
    assert_eq!(diff.mismatches[0].line, 0);
    assert_eq!(diff.mismatches[0].got, "ABC");
    assert_eq!(diff.mismatches[0].want, "XYZ");
}

#[test]
fn snapshot_export_contains_content() {
    let prev = Buffer::new(15, 3);
    let mut next = Buffer::new(15, 3);
    for (i, ch) in "Export Test".chars().enumerate() {
        next.set(i as u16, 1, Cell::from_char(ch));
    }

    let term = present_into_headless(&prev, &next);
    let export = term.export_string();

    assert!(export.contains("15x3"));
    assert!(export.contains("Export Test"));
}

// ============================================================================
// Complex layout: widgets rendered through presenter pipeline
// ============================================================================

/// Helper: render a widget into a buffer, diff against blank, present into HeadlessTerm.
fn render_widget_into_headless<W: ftui_widgets::Widget>(
    widget: &W,
    width: u16,
    height: u16,
) -> HeadlessTerm {
    let prev = Buffer::new(width, height);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    let area = Rect::from_size(width, height);
    widget.render(area, &mut frame);
    present_into_headless(&prev, &frame.buffer)
}

#[test]
fn block_with_borders_renders_correctly() {
    use ftui_widgets::block::Block;
    use ftui_widgets::borders::Borders;

    let block = Block::new().borders(Borders::ALL).title("Test");
    let term = render_widget_into_headless(&block, 12, 5);

    // Top border should contain title and box-drawing chars
    let top = term.row_text(0);
    assert!(
        top.contains("Test"),
        "top row should contain title: {top:?}"
    );

    // Sides should have vertical border chars
    let left_text = term.model().cell(0, 1).map(|c| c.text.as_str());
    assert!(left_text.is_some(), "left border should have a character");

    // Bottom border should be present
    let bottom = term.row_text(4);
    assert!(!bottom.is_empty(), "bottom border should not be empty");
}

#[test]
fn paragraph_no_wrap_renders_correctly() {
    use ftui_text::Text;
    use ftui_widgets::paragraph::Paragraph;

    let text = Text::raw("Hello, world!");
    let para = Paragraph::new(text);
    let term = render_widget_into_headless(&para, 20, 3);

    term.assert_row(0, "Hello, world!");
    term.assert_row(1, "");
}

#[test]
fn paragraph_wraps_long_text() {
    use ftui_text::{Text, WrapMode};
    use ftui_widgets::paragraph::Paragraph;

    let text = Text::raw("ABCDEFGHIJ KLMNOPQRST");
    let para = Paragraph::new(text).wrap(WrapMode::Word);
    let term = render_widget_into_headless(&para, 15, 5);

    // The text "ABCDEFGHIJ KLMNOPQRST" should wrap at the space
    let row0 = term.row_text(0);
    let row1 = term.row_text(1);
    assert!(
        !row0.is_empty() && !row1.is_empty(),
        "word wrap should produce at least 2 lines: row0={row0:?}, row1={row1:?}"
    );
}

#[test]
fn nested_layout_block_in_columns() {
    use ftui_layout::{Constraint, Flex};
    use ftui_widgets::Widget;
    use ftui_widgets::block::Block;
    use ftui_widgets::borders::Borders;

    let width = 30u16;
    let height = 5u16;
    let area = Rect::from_size(width, height);

    // Split into 2 columns
    let flex = Flex::horizontal().constraints(vec![
        Constraint::Percentage(50.0),
        Constraint::Percentage(50.0),
    ]);
    let columns = flex.split(area);

    // Render a block into each column
    let prev = Buffer::new(width, height);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);

    let left_block = Block::new().borders(Borders::ALL).title("L");
    let right_block = Block::new().borders(Borders::ALL).title("R");

    left_block.render(columns[0], &mut frame);
    right_block.render(columns[1], &mut frame);

    let term = present_into_headless(&prev, &frame.buffer);

    // Both titles should appear on row 0
    let top = term.row_text(0);
    assert!(top.contains("L"), "should contain left title: {top:?}");
    assert!(top.contains("R"), "should contain right title: {top:?}");

    // Both blocks should have bottom borders on the last row
    let bottom = term.row_text(4);
    assert!(!bottom.is_empty(), "bottom row should have border chars");
}

#[test]
fn table_renders_header_and_rows() {
    use ftui_layout::Constraint;
    use ftui_widgets::Widget;
    use ftui_widgets::table::{Row, Table};

    let widths = vec![Constraint::Fixed(6), Constraint::Fixed(6)];
    let header = Row::new(vec!["Name", "Age"]);
    let rows = vec![Row::new(vec!["Alice", "30"]), Row::new(vec!["Bob", "25"])];

    let table = Table::new(rows, widths).header(header);

    let width = 20u16;
    let height = 10u16;
    let area = Rect::from_size(width, height);
    let prev = Buffer::new(width, height);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    table.render(area, &mut frame);

    let term = present_into_headless(&prev, &frame.buffer);

    // Header and data rows should be present
    let all_text = term.screen_string();
    assert!(
        all_text.contains("Name"),
        "should contain header: {all_text:?}"
    );
    assert!(
        all_text.contains("Alice"),
        "should contain data: {all_text:?}"
    );
    assert!(
        all_text.contains("Bob"),
        "should contain data: {all_text:?}"
    );
}

// ============================================================================
// Style codes: SGR attributes verified through HeadlessTerm
// ============================================================================

#[test]
fn style_bold_roundtrips() {
    let prev = Buffer::new(10, 1);
    let mut next = Buffer::new(10, 1);
    next.set(
        0,
        0,
        Cell::from_char('B').with_attrs(CellAttrs::new(StyleFlags::BOLD, 0)),
    );

    let term = present_into_headless(&prev, &next);
    let cell = term.model().cell(0, 0).expect("cell should exist");
    assert!(cell.attrs.has_flag(StyleFlags::BOLD), "cell should be bold");
    assert_eq!(cell.text.as_str(), "B");
}

#[test]
fn style_italic_roundtrips() {
    let prev = Buffer::new(10, 1);
    let mut next = Buffer::new(10, 1);
    next.set(
        0,
        0,
        Cell::from_char('I').with_attrs(CellAttrs::new(StyleFlags::ITALIC, 0)),
    );

    let term = present_into_headless(&prev, &next);
    let cell = term.model().cell(0, 0).expect("cell should exist");
    assert!(
        cell.attrs.has_flag(StyleFlags::ITALIC),
        "cell should be italic"
    );
}

#[test]
fn style_fg_color_roundtrips() {
    let red = PackedRgba::rgb(255, 0, 0);
    let prev = Buffer::new(10, 1);
    let mut next = Buffer::new(10, 1);
    next.set(0, 0, Cell::from_char('R').with_fg(red));

    let term = present_into_headless(&prev, &next);
    let cell = term.model().cell(0, 0).expect("cell should exist");
    assert_eq!(cell.text.as_str(), "R");
    assert_eq!(cell.fg, red, "foreground color should round-trip");
}

#[test]
fn style_bg_color_roundtrips() {
    let blue = PackedRgba::rgb(0, 0, 255);
    let prev = Buffer::new(10, 1);
    let mut next = Buffer::new(10, 1);
    next.set(0, 0, Cell::from_char('B').with_bg(blue));

    let term = present_into_headless(&prev, &next);
    let cell = term.model().cell(0, 0).expect("cell should exist");
    assert_eq!(cell.bg, blue, "background color should round-trip");
}

#[test]
fn style_combined_attrs_roundtrip() {
    let fg = PackedRgba::rgb(255, 128, 0);
    let bg = PackedRgba::rgb(0, 64, 128);
    let flags = StyleFlags::BOLD | StyleFlags::UNDERLINE;

    let prev = Buffer::new(10, 1);
    let mut next = Buffer::new(10, 1);
    next.set(
        0,
        0,
        Cell::from_char('X')
            .with_fg(fg)
            .with_bg(bg)
            .with_attrs(CellAttrs::new(flags, 0)),
    );

    let term = present_into_headless(&prev, &next);
    let cell = term.model().cell(0, 0).expect("cell should exist");
    assert_eq!(cell.text.as_str(), "X");
    assert_eq!(cell.fg, fg);
    assert_eq!(cell.bg, bg);
    assert!(cell.attrs.has_flag(StyleFlags::BOLD));
    assert!(cell.attrs.has_flag(StyleFlags::UNDERLINE));
}

#[test]
fn style_reset_between_cells() {
    // Cell 0: bold red, Cell 1: normal green — verify styles don't bleed
    let red = PackedRgba::rgb(255, 0, 0);
    let green = PackedRgba::rgb(0, 255, 0);

    let prev = Buffer::new(10, 1);
    let mut next = Buffer::new(10, 1);
    next.set(
        0,
        0,
        Cell::from_char('A')
            .with_fg(red)
            .with_attrs(CellAttrs::new(StyleFlags::BOLD, 0)),
    );
    next.set(1, 0, Cell::from_char('B').with_fg(green));

    let term = present_into_headless(&prev, &next);

    let cell_a = term.model().cell(0, 0).expect("cell A");
    let cell_b = term.model().cell(1, 0).expect("cell B");

    assert!(cell_a.attrs.has_flag(StyleFlags::BOLD), "A should be bold");
    assert_eq!(cell_a.fg, red);
    assert!(
        !cell_b.attrs.has_flag(StyleFlags::BOLD),
        "B should not be bold"
    );
    assert_eq!(cell_b.fg, green);
}

#[test]
fn multiple_styled_rows() {
    let prev = Buffer::new(10, 3);
    let mut next = Buffer::new(10, 3);

    // Row 0: red text
    let red = PackedRgba::rgb(255, 0, 0);
    for (i, ch) in "Red".chars().enumerate() {
        next.set(i as u16, 0, Cell::from_char(ch).with_fg(red));
    }

    // Row 1: blue bold text
    let blue = PackedRgba::rgb(0, 0, 255);
    for (i, ch) in "Blue".chars().enumerate() {
        next.set(
            i as u16,
            1,
            Cell::from_char(ch)
                .with_fg(blue)
                .with_attrs(CellAttrs::new(StyleFlags::BOLD, 0)),
        );
    }

    // Row 2: plain text
    for (i, ch) in "Plain".chars().enumerate() {
        next.set(i as u16, 2, Cell::from_char(ch));
    }

    let term = present_into_headless(&prev, &next);

    term.assert_row(0, "Red");
    term.assert_row(1, "Blue");
    term.assert_row(2, "Plain");

    // Verify styles
    let r0 = term.model().cell(0, 0).unwrap();
    assert_eq!(r0.fg, red);

    let r1 = term.model().cell(0, 1).unwrap();
    assert_eq!(r1.fg, blue);
    assert!(r1.attrs.has_flag(StyleFlags::BOLD));
}

// ============================================================================
// Property tests
// ============================================================================

mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Any byte sequence fed to HeadlessTerm::process must not panic.
        #[test]
        fn any_bytes_no_crash(bytes in proptest::collection::vec(any::<u8>(), 0..1024)) {
            let mut term = HeadlessTerm::new(80, 24);
            term.process(&bytes);
            let _ = term.screen_text();
            let _ = term.cursor();
        }

        /// After any sequence of cursor movement commands, cursor stays in bounds.
        #[test]
        fn cursor_stays_in_bounds(
            width in 1u16..200,
            height in 1u16..100,
            moves in proptest::collection::vec(
                (0u8..4, 1u16..100),
                0..50
            ),
        ) {
            let mut term = HeadlessTerm::new(width, height);

            for (direction, count) in moves {
                let seq = match direction {
                    0 => format!("\x1b[{}A", count), // up
                    1 => format!("\x1b[{}B", count), // down
                    2 => format!("\x1b[{}C", count), // forward
                    3 => format!("\x1b[{}D", count), // back
                    _ => unreachable!(),
                };
                term.process(seq.as_bytes());

                let (col, row) = term.cursor();
                prop_assert!(
                    col < width,
                    "cursor col {} >= width {} after move",
                    col,
                    width
                );
                prop_assert!(
                    row < height,
                    "cursor row {} >= height {} after move",
                    row,
                    height
                );
            }
        }

        /// CUP (absolute positioning) always clamps to valid bounds.
        #[test]
        fn cup_clamps_to_bounds(
            width in 1u16..200,
            height in 1u16..100,
            target_row in 0u16..500,
            target_col in 0u16..500,
        ) {
            let mut term = HeadlessTerm::new(width, height);
            let seq = format!("\x1b[{};{}H", target_row + 1, target_col + 1);
            term.process(seq.as_bytes());

            let (col, row) = term.cursor();
            prop_assert!(col < width, "col {} >= width {}", col, width);
            prop_assert!(row < height, "row {} >= height {}", row, height);
        }

        /// Mixed text and escape sequences never panic.
        #[test]
        fn mixed_content_no_crash(
            segments in proptest::collection::vec(
                prop_oneof![
                    // Plain ASCII text
                    "[A-Za-z0-9 ]{1,20}".prop_map(|s| s.into_bytes()),
                    // CSI sequences with random params
                    (1u16..100, any::<u8>()).prop_map(|(n, cmd)| {
                        let letter = b'A' + (cmd % 26);
                        format!("\x1b[{}{}", n, letter as char).into_bytes()
                    }),
                    // SGR sequences
                    (0u8..108).prop_map(|code| {
                        format!("\x1b[{}m", code).into_bytes()
                    }),
                    // Newlines and carriage returns
                    Just(b"\r\n".to_vec()),
                ],
                0..30
            ),
        ) {
            let mut term = HeadlessTerm::new(80, 24);
            for segment in &segments {
                term.process(segment);
            }
            let text = term.screen_text();
            prop_assert_eq!(text.len(), 24);
            let (col, row) = term.cursor();
            prop_assert!(col < 80);
            prop_assert!(row < 24);
        }
    }
}
