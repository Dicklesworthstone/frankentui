//! Rich Core Infrastructure Integration Tests
//!
//! Exercises the cross-module pipeline: Console → Segments → Style stack → output,
//! verifying the one-writer safe routing pattern and segment-based text processing.

#![cfg(feature = "console")]

use ftui_extras::console::{CapturedLine, Console, ConsoleSink, WrapMode};
use ftui_render::cell::PackedRgba;
use ftui_style::Style;
use ftui_text::segment::Segment;

// ---------------------------------------------------------------------------
// One-writer safe routing: all output goes through Console, never raw stdout
// ---------------------------------------------------------------------------

#[test]
fn one_writer_console_captures_all_output() {
    let sink = ConsoleSink::capture();
    let mut console = Console::new(80, sink);

    console.print(Segment::text("line one"));
    console.newline();
    console.print(Segment::text("line two"));
    console.newline();

    let lines = console.into_captured_lines();
    assert_eq!(lines.len(), 2, "expected exactly 2 captured lines");
    assert_eq!(lines[0].plain_text(), "line one");
    assert_eq!(lines[1].plain_text(), "line two");
}

#[test]
fn one_writer_styled_segments_captured() {
    let sink = ConsoleSink::capture();
    let mut console = Console::new(80, sink);

    let bold = Style::new().bold();
    console.print(Segment::styled("bold text", bold));
    console.newline();

    let lines = console.into_captured_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].plain_text(), "bold text");
    // Style is preserved in captured segments - at least one segment should have non-default style
    assert!(
        !lines[0].segments.is_empty(),
        "should have at least one segment"
    );
    let has_styled = lines[0]
        .segments
        .iter()
        .any(|s| s.style != Style::default());
    assert!(
        has_styled,
        "at least one segment should have bold style applied"
    );
}

#[test]
fn one_writer_no_raw_writes_leak() {
    // Verify that Console with a capture sink collects everything.
    // If anything bypassed the Console, into_captured would miss it.
    let sink = ConsoleSink::capture();
    let mut console = Console::new(40, sink);

    console.print(Segment::text("A"));
    console.print(Segment::text("B"));
    console.print(Segment::text("C"));
    console.newline();

    let output = console.into_captured();
    assert!(
        output.contains("ABC"),
        "captured output should contain ABC, got: {output:?}"
    );
}

// ---------------------------------------------------------------------------
// Style stack: nested push/pop cascading
// ---------------------------------------------------------------------------

#[test]
fn style_stack_nested_inheritance() {
    let sink = ConsoleSink::capture();
    let mut console = Console::new(80, sink);

    let blue_fg = Style::new().fg(PackedRgba::rgb(0, 0, 255));
    let bold = Style::new().bold();

    // Push blue
    console.push_style(blue_fg);
    console.print(Segment::text("blue "));

    // Push bold (inherits blue)
    console.push_style(bold);
    console.print(Segment::text("blue+bold "));

    // Pop bold (back to blue)
    console.pop_style();
    console.print(Segment::text("blue again"));
    console.newline();

    // Pop blue (back to default)
    console.pop_style();
    console.print(Segment::text("default"));
    console.newline();

    let lines = console.into_captured_lines();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].plain_text(), "blue blue+bold blue again");
    assert_eq!(lines[1].plain_text(), "default");
}

#[test]
fn style_stack_clear_resets_all() {
    let sink = ConsoleSink::capture();
    let mut console = Console::new(80, sink);

    console.push_style(Style::new().bold());
    console.push_style(Style::new().italic());
    console.clear_styles();

    // After clear, current style should be default
    let current = console.current_style();
    assert_eq!(current, Style::default());
}

// ---------------------------------------------------------------------------
// Segment text processing: cell-aware operations
// ---------------------------------------------------------------------------

#[test]
fn segment_cell_length_ascii() {
    let seg = Segment::text("hello");
    assert_eq!(seg.cell_length(), 5);
}

#[test]
fn segment_cell_length_cjk() {
    // CJK characters are 2 cells wide
    let seg = Segment::text("日本");
    assert_eq!(seg.cell_length(), 4); // 2 chars * 2 cells each
}

#[test]
fn segment_cell_length_control() {
    use ftui_text::segment::ControlCode;
    let seg = Segment::control(ControlCode::LineFeed);
    assert_eq!(seg.cell_length(), 0);
}

#[test]
fn segment_split_at_cell_ascii() {
    let seg = Segment::text("hello world");
    let (left, right) = seg.split_at_cell(5);
    assert_eq!(left.as_str(), "hello");
    assert_eq!(right.as_str(), " world");
}

#[test]
fn segment_split_preserves_total_length() {
    let seg = Segment::text("abcdefghij");
    for split_point in 0..=10 {
        let (left, right) = seg.split_at_cell(split_point);
        assert_eq!(
            left.cell_length() + right.cell_length(),
            10,
            "split at {split_point} should preserve total cell length"
        );
    }
}

// ---------------------------------------------------------------------------
// Console wrapping with segments
// ---------------------------------------------------------------------------

#[test]
fn word_wrap_respects_width() {
    let sink = ConsoleSink::capture();
    let mut console = Console::with_options(20, sink, WrapMode::Word);

    // 30-char line should wrap at word boundary
    console.print(Segment::text("this is a test of word wrapping"));
    console.newline();

    let lines = console.into_captured_lines();
    for line in &lines {
        assert!(
            line.width() <= 20,
            "line '{}' exceeds width 20 (actual: {})",
            line.plain_text(),
            line.width()
        );
    }
    assert!(lines.len() >= 2, "should wrap into multiple lines");
}

#[test]
fn char_wrap_breaks_long_word() {
    let sink = ConsoleSink::capture();
    let mut console = Console::with_options(10, sink, WrapMode::Character);

    console.print(Segment::text("superlongword"));
    console.newline();

    let lines = console.into_captured_lines();
    for line in &lines {
        assert!(
            line.width() <= 10,
            "line '{}' exceeds width 10",
            line.plain_text()
        );
    }
    assert!(lines.len() >= 2, "long word should be broken");
}

// ---------------------------------------------------------------------------
// Console special output: rule, blank_line
// ---------------------------------------------------------------------------

#[test]
fn console_rule_fills_width() {
    let sink = ConsoleSink::capture();
    let mut console = Console::new(40, sink);

    console.rule('─');

    let lines = console.into_captured_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].width(), 40);
    assert!(lines[0].plain_text().chars().all(|c| c == '─'));
}

#[test]
fn console_blank_line_empty() {
    let sink = ConsoleSink::capture();
    let mut console = Console::new(80, sink);

    console.print(Segment::text("before"));
    console.newline();
    console.blank_line();
    console.print(Segment::text("after"));
    console.newline();

    let lines = console.into_captured_lines();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].plain_text(), "before");
    assert_eq!(lines[1].plain_text(), "");
    assert_eq!(lines[2].plain_text(), "after");
}

// ---------------------------------------------------------------------------
// Text measurement
// ---------------------------------------------------------------------------

#[test]
fn text_measurement_union_max_bounds() {
    use ftui_text::TextMeasurement;

    let a = TextMeasurement {
        minimum: 5,
        maximum: 10,
    };
    let b = TextMeasurement {
        minimum: 3,
        maximum: 15,
    };
    let union = a.union(b);
    assert_eq!(union.minimum, 5); // max of mins
    assert_eq!(union.maximum, 15); // max of maxes
}

#[test]
fn text_measurement_stack_adds_bounds() {
    use ftui_text::TextMeasurement;

    let a = TextMeasurement {
        minimum: 5,
        maximum: 10,
    };
    let b = TextMeasurement {
        minimum: 3,
        maximum: 8,
    };
    let stacked = a.stack(b);
    assert_eq!(stacked.minimum, 8); // sum of mins
    assert_eq!(stacked.maximum, 18); // sum of maxes
}

#[test]
fn text_measurement_clamp_enforces_bounds() {
    use ftui_text::TextMeasurement;

    let m = TextMeasurement {
        minimum: 5,
        maximum: 20,
    };
    let clamped = m.clamp(Some(10), Some(15));
    assert_eq!(clamped.minimum, 10);
    assert_eq!(clamped.maximum, 15);
}

// ---------------------------------------------------------------------------
// Border character sets
// ---------------------------------------------------------------------------

#[test]
fn border_presets_have_distinct_chars() {
    use ftui_render::drawing::BorderChars;

    let presets = [
        ("SQUARE", BorderChars::SQUARE),
        ("ROUNDED", BorderChars::ROUNDED),
        ("DOUBLE", BorderChars::DOUBLE),
        ("HEAVY", BorderChars::HEAVY),
        ("ASCII", BorderChars::ASCII),
    ];

    // Each preset should use distinct horizontal/vertical chars
    for (name, preset) in &presets {
        assert_ne!(
            preset.horizontal, preset.vertical,
            "{name}: horizontal and vertical should differ"
        );
    }

    // ASCII should use only ASCII characters
    let ascii = BorderChars::ASCII;
    assert!(
        ascii.horizontal.is_ascii(),
        "ASCII horizontal should be ASCII"
    );
    assert!(ascii.vertical.is_ascii(), "ASCII vertical should be ASCII");
    assert!(ascii.top_left.is_ascii(), "ASCII top_left should be ASCII");
}

// ---------------------------------------------------------------------------
// Filesize formatter
// ---------------------------------------------------------------------------

#[cfg(feature = "filesize")]
mod filesize_tests {
    use ftui_extras::filesize;

    #[test]
    fn binary_formatting() {
        assert_eq!(filesize::binary(0), "0 B");
        assert_eq!(filesize::binary(1023), "1023 B");
        assert_eq!(filesize::binary(1024), "1.0 KiB");
        assert_eq!(filesize::binary(1024 * 1024), "1.0 MiB");
    }

    #[test]
    fn decimal_formatting() {
        assert_eq!(filesize::decimal(0), "0 B");
        assert_eq!(filesize::decimal(999), "999 B");
        assert_eq!(filesize::decimal(1000), "1.0 KB");
        assert_eq!(filesize::decimal(1_000_000), "1.0 MB");
    }

    #[test]
    fn precision_control() {
        let result = filesize::binary_with_precision(1536, 2);
        assert_eq!(result, "1.50 KiB");
    }

    #[test]
    fn negative_sizes_via_format_size() {
        use ftui_extras::filesize::{SizeFormat, format_size};
        let result = format_size(-1024, SizeFormat::binary());
        assert_eq!(result, "-1.0 KiB");
    }
}

// ---------------------------------------------------------------------------
// Live display
// ---------------------------------------------------------------------------

#[cfg(feature = "live")]
mod live_tests {
    use ftui_extras::live::{Live, LiveConfig, VerticalOverflow};
    use std::io::Cursor;

    #[test]
    fn live_start_stop_idempotent() {
        let writer: Box<dyn std::io::Write + Send> = Box::new(Cursor::new(Vec::new()));
        let config = LiveConfig::default();
        let live = Live::with_config(writer, 80, config);

        let _ = live.start();
        let _ = live.start(); // second start is no-op
        assert!(live.is_started());

        let _ = live.stop();
        let _ = live.stop(); // second stop is no-op
        assert!(!live.is_started());
    }

    #[test]
    fn live_config_overflow_variants() {
        let configs = [
            LiveConfig {
                overflow: VerticalOverflow::Crop,
                ..Default::default()
            },
            LiveConfig {
                overflow: VerticalOverflow::Ellipsis,
                ..Default::default()
            },
            LiveConfig {
                overflow: VerticalOverflow::Visible,
                ..Default::default()
            },
        ];
        for config in configs {
            let writer: Box<dyn std::io::Write + Send> = Box::new(Cursor::new(Vec::new()));
            let live = Live::with_config(writer, 40, config);
            let _ = live.start();
            let _ = live.stop();
        }
    }
}

// ---------------------------------------------------------------------------
// Full pipeline integration: Console + Segments + Styles + Measurement
// ---------------------------------------------------------------------------

#[test]
fn full_pipeline_styled_multiline_output() {
    let sink = ConsoleSink::capture();
    let mut console = Console::new(60, sink);

    // Simulate a rich-style output: header + body + footer
    let header_style = Style::new().bold().fg(PackedRgba::rgb(0, 200, 0));
    let body_style = Style::new().fg(PackedRgba::rgb(200, 200, 200));
    let footer_style = Style::new().dim();

    // Header
    console.push_style(header_style);
    console.print(Segment::text("=== Status Report ==="));
    console.newline();
    console.pop_style();

    // Body with mixed styles
    console.push_style(body_style);
    console.print(Segment::text("Tests: "));
    console.print(Segment::styled(
        "31 passed",
        Style::new().fg(PackedRgba::rgb(0, 255, 0)),
    ));
    console.print(Segment::text(", "));
    console.print(Segment::styled(
        "0 failed",
        Style::new().fg(PackedRgba::rgb(255, 0, 0)),
    ));
    console.newline();
    console.pop_style();

    // Footer
    console.push_style(footer_style);
    console.print(Segment::text("Duration: 39s"));
    console.newline();
    console.pop_style();

    let lines = console.into_captured_lines();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].plain_text(), "=== Status Report ===");
    assert_eq!(lines[1].plain_text(), "Tests: 31 passed, 0 failed");
    assert_eq!(lines[2].plain_text(), "Duration: 39s");

    // Verify header has bold+green style applied
    assert_ne!(lines[0].segments[0].style, Style::default());
    assert!(
        lines[1].segments.len() >= 3,
        "body line should have multiple styled segments"
    );
}

#[test]
fn full_pipeline_wrapping_preserves_styles() {
    let sink = ConsoleSink::capture();
    let mut console = Console::with_options(25, sink, WrapMode::Word);

    console.push_style(Style::new().bold());
    console.print(Segment::text(
        "This bold text wraps at word boundaries cleanly",
    ));
    console.newline();
    console.pop_style();

    let lines = console.into_captured_lines();
    // All lines should fit within width
    for line in &lines {
        assert!(
            line.width() <= 25,
            "line '{}' exceeds width 25",
            line.plain_text()
        );
    }
    // Should have multiple lines
    assert!(lines.len() >= 2, "should wrap to multiple lines");
}

#[test]
fn captured_line_from_plain_roundtrip() {
    let line = CapturedLine::from_plain("hello world");
    assert_eq!(line.plain_text(), "hello world");
    assert_eq!(line.width(), 11);
}
