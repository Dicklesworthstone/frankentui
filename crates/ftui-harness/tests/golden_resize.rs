#![forbid(unsafe_code)]

//! Golden output tests for resize scenarios.
//!
//! These tests verify that buffer rendering produces deterministic output
//! across different terminal sizes and resize transitions.
//!
//! # Running Tests
//!
//! ```sh
//! cargo test -p ftui-harness golden_
//! ```
//!
//! # Updating Golden Checksums
//!
//! ```sh
//! BLESS=1 cargo test -p ftui-harness golden_
//! ```
//!
//! # Deterministic Mode
//!
//! ```sh
//! GOLDEN_SEED=42 cargo test -p ftui-harness golden_
//! ```

use ftui_core::geometry::Rect;
use ftui_harness::golden::{
    GoldenEnv, GoldenLogger, GoldenOutcome, GoldenResult, ResizeScenario, compute_buffer_checksum,
    golden_checksum_path, is_bless_mode, load_golden_checksums, save_golden_checksums,
    standard_resize_scenarios, verify_checksums,
};
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_text::Text;
use ftui_text::display_width;
use ftui_widgets::Widget;
use ftui_widgets::block::Block;
use ftui_widgets::borders::Borders;
use ftui_widgets::paragraph::Paragraph;
use std::path::Path;

fn golden_base_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Render a simple test pattern into a buffer.
fn render_test_pattern(buf: &mut Buffer, width: u16, height: u16) {
    // Border
    for x in 0..width {
        buf.set(x, 0, Cell::from_char('─'));
        buf.set(x, height.saturating_sub(1), Cell::from_char('─'));
    }
    for y in 0..height {
        buf.set(0, y, Cell::from_char('│'));
        buf.set(width.saturating_sub(1), y, Cell::from_char('│'));
    }
    // Corners
    buf.set(0, 0, Cell::from_char('┌'));
    buf.set(width.saturating_sub(1), 0, Cell::from_char('┐'));
    buf.set(0, height.saturating_sub(1), Cell::from_char('└'));
    buf.set(
        width.saturating_sub(1),
        height.saturating_sub(1),
        Cell::from_char('┘'),
    );

    // Size label in center
    let label = format!("{}x{}", width, height);
    let start_x = (width.saturating_sub(display_width(&label) as u16)) / 2;
    let start_y = height / 2;
    for (i, c) in label.chars().enumerate() {
        let x = start_x + i as u16;
        if x < width.saturating_sub(1) && start_y < height.saturating_sub(1) && start_y > 0 {
            buf.set(x, start_y, Cell::from_char(c));
        }
    }
}

/// Run a golden test for a specific scenario.
fn run_golden_scenario(scenario: &ResizeScenario, base_dir: &Path) -> GoldenResult {
    let start = std::time::Instant::now();
    let mut checksums = Vec::new();

    // Render initial frame
    let mut buf = Buffer::new(scenario.initial_width, scenario.initial_height);
    render_test_pattern(&mut buf, scenario.initial_width, scenario.initial_height);
    checksums.push(compute_buffer_checksum(&buf));

    // Apply resize steps
    for &(new_w, new_h, _delay_ms) in &scenario.resize_steps {
        let mut new_buf = Buffer::new(new_w, new_h);
        render_test_pattern(&mut new_buf, new_w, new_h);
        checksums.push(compute_buffer_checksum(&new_buf));
    }

    // Load expected checksums
    let checksum_path = golden_checksum_path(base_dir, &scenario.name);
    let expected = load_golden_checksums(&checksum_path).unwrap_or_default();

    // Verify or update
    let (outcome, mismatch_index) = if is_bless_mode() {
        let _ = save_golden_checksums(&checksum_path, &checksums);
        (GoldenOutcome::Pass, None)
    } else if expected.is_empty() {
        // No golden file yet - pass but note it
        (GoldenOutcome::Pass, None)
    } else {
        verify_checksums(&checksums, &expected)
    };

    GoldenResult {
        scenario: scenario.name.clone(),
        outcome,
        checksums,
        expected_checksums: expected,
        mismatch_index,
        duration_ms: start.elapsed().as_millis() as u64,
    }
}

// ============================================================================
// Fixed Size Tests
// ============================================================================

#[test]
fn golden_fixed_80x24() {
    let scenario = ResizeScenario::fixed("fixed_80x24", 80, 24);
    let base_dir = golden_base_dir();
    let result = run_golden_scenario(&scenario, base_dir);
    assert!(result.is_pass(), "{}", result.format());
}

#[test]
fn golden_fixed_120x40() {
    let scenario = ResizeScenario::fixed("fixed_120x40", 120, 40);
    let base_dir = golden_base_dir();
    let result = run_golden_scenario(&scenario, base_dir);
    assert!(result.is_pass(), "{}", result.format());
}

#[test]
fn golden_fixed_60x15() {
    let scenario = ResizeScenario::fixed("fixed_60x15", 60, 15);
    let base_dir = golden_base_dir();
    let result = run_golden_scenario(&scenario, base_dir);
    assert!(result.is_pass(), "{}", result.format());
}

#[test]
fn golden_fixed_40x10() {
    let scenario = ResizeScenario::fixed("fixed_40x10", 40, 10);
    let base_dir = golden_base_dir();
    let result = run_golden_scenario(&scenario, base_dir);
    assert!(result.is_pass(), "{}", result.format());
}

#[test]
fn golden_fixed_200x60() {
    let scenario = ResizeScenario::fixed("fixed_200x60", 200, 60);
    let base_dir = golden_base_dir();
    let result = run_golden_scenario(&scenario, base_dir);
    assert!(result.is_pass(), "{}", result.format());
}

// ============================================================================
// Resize Transition Tests
// ============================================================================

#[test]
fn golden_resize_80x24_to_120x40() {
    let scenario = ResizeScenario::resize("resize_80x24_to_120x40", 80, 24, 120, 40);
    let base_dir = golden_base_dir();
    let result = run_golden_scenario(&scenario, base_dir);
    assert!(result.is_pass(), "{}", result.format());
}

#[test]
fn golden_resize_120x40_to_80x24() {
    let scenario = ResizeScenario::resize("resize_120x40_to_80x24", 120, 40, 80, 24);
    let base_dir = golden_base_dir();
    let result = run_golden_scenario(&scenario, base_dir);
    assert!(result.is_pass(), "{}", result.format());
}

#[test]
fn golden_resize_80x24_to_40x10() {
    let scenario = ResizeScenario::resize("resize_80x24_to_40x10", 80, 24, 40, 10);
    let base_dir = golden_base_dir();
    let result = run_golden_scenario(&scenario, base_dir);
    assert!(result.is_pass(), "{}", result.format());
}

#[test]
fn golden_resize_40x10_to_200x60() {
    let scenario = ResizeScenario::resize("resize_40x10_to_200x60", 40, 10, 200, 60);
    let base_dir = golden_base_dir();
    let result = run_golden_scenario(&scenario, base_dir);
    assert!(result.is_pass(), "{}", result.format());
}

// ============================================================================
// Isomorphism Property Tests
// ============================================================================

#[test]
fn golden_checksum_determinism() {
    // Verify that the same buffer produces the same checksum
    let mut buf = Buffer::new(80, 24);
    render_test_pattern(&mut buf, 80, 24);

    let checksum1 = compute_buffer_checksum(&buf);
    let checksum2 = compute_buffer_checksum(&buf);

    assert_eq!(checksum1, checksum2, "Checksums should be deterministic");
}

#[test]
fn golden_checksum_differs_on_content() {
    let mut buf1 = Buffer::new(80, 24);
    render_test_pattern(&mut buf1, 80, 24);

    let mut buf2 = Buffer::new(80, 24);
    render_test_pattern(&mut buf2, 80, 24);
    buf2.set(40, 12, Cell::from_char('X')); // Modify one cell

    let checksum1 = compute_buffer_checksum(&buf1);
    let checksum2 = compute_buffer_checksum(&buf2);

    assert_ne!(
        checksum1, checksum2,
        "Different content should produce different checksums"
    );
}

#[test]
fn golden_checksum_differs_on_size() {
    let mut buf1 = Buffer::new(80, 24);
    render_test_pattern(&mut buf1, 80, 24);

    let mut buf2 = Buffer::new(81, 24);
    render_test_pattern(&mut buf2, 81, 24);

    let checksum1 = compute_buffer_checksum(&buf1);
    let checksum2 = compute_buffer_checksum(&buf2);

    assert_ne!(
        checksum1, checksum2,
        "Different sizes should produce different checksums"
    );
}

// ============================================================================
// Widget Rendering Golden Tests
// ============================================================================

#[test]
fn golden_widget_block_80x24() {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);

    let block = Block::default().borders(Borders::ALL).title("Golden Test");
    block.render(Rect::new(0, 0, 80, 24), &mut frame);

    let checksum = compute_buffer_checksum(&frame.buffer);
    assert!(
        checksum.starts_with("blake3:"),
        "Checksum should have prefix"
    );
    assert_eq!(checksum.len(), 7 + 64, "Checksum should be 64 hex chars");
}

#[test]
fn golden_widget_paragraph_80x24() {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);

    let para = Paragraph::new(Text::raw("Hello, Golden World!\nThis is a test."))
        .block(Block::default().borders(Borders::ALL).title("Paragraph"));
    para.render(Rect::new(0, 0, 80, 24), &mut frame);

    let checksum = compute_buffer_checksum(&frame.buffer);
    assert!(checksum.starts_with("blake3:"));

    // Verify determinism
    let mut frame2 = Frame::new(80, 24, &mut pool);
    para.render(Rect::new(0, 0, 80, 24), &mut frame2);
    let checksum2 = compute_buffer_checksum(&frame2.buffer);

    assert_eq!(
        checksum, checksum2,
        "Widget rendering should be deterministic"
    );
}

// ============================================================================
// Standard Scenarios Test
// ============================================================================

#[test]
fn golden_all_standard_scenarios() {
    let scenarios = standard_resize_scenarios();
    let base_dir = golden_base_dir();

    let mut failures = Vec::new();

    for scenario in &scenarios {
        let result = run_golden_scenario(scenario, base_dir);
        if !result.is_pass() {
            failures.push(result.format());
        }
    }

    assert!(
        failures.is_empty(),
        "Golden test failures:\n{}",
        failures.join("\n")
    );
}

// ============================================================================
// JSONL Logger Tests
// ============================================================================

#[test]
fn golden_logger_creates_valid_jsonl() {
    let log_dir = std::env::temp_dir().join("ftui_golden_logger_test");
    let _ = std::fs::remove_dir_all(&log_dir);
    std::fs::create_dir_all(&log_dir).unwrap();

    let log_path = log_dir.join("test.jsonl");
    let mut logger = GoldenLogger::new(&log_path).unwrap();

    let env = GoldenEnv::capture();
    logger.log_start("test_case", &env);
    logger.log_frame(0, 80, 24, "sha256:abc123", 10);
    logger.log_resize(80, 24, 120, 40, 5);
    logger.log_frame(1, 120, 40, "sha256:def456", 12);
    logger.log_complete(GoldenOutcome::Pass);

    // Verify log file exists and has content
    let content = std::fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();

    assert!(!lines.is_empty(), "Log should have entries");

    // Each line should be valid JSON (basic check)
    for line in &lines {
        assert!(line.starts_with('{'), "Line should start with {{");
        assert!(line.ends_with('}'), "Line should end with }}");
    }

    // Verify we can find expected events
    assert!(
        content.contains("\"event\":\"start\""),
        "Should have start event"
    );
    assert!(
        content.contains("\"event\":\"frame\""),
        "Should have frame events"
    );
    assert!(
        content.contains("\"event\":\"resize\""),
        "Should have resize event"
    );
    assert!(
        content.contains("\"event\":\"complete\""),
        "Should have complete event"
    );

    let _ = std::fs::remove_dir_all(&log_dir);
}

#[test]
fn golden_logger_noop_does_not_crash() {
    let mut logger = GoldenLogger::noop();
    let env = GoldenEnv::capture();
    logger.log_start("test_case", &env);
    logger.log_frame(0, 80, 24, "sha256:abc123", 10);
    logger.log_complete(GoldenOutcome::Pass);
    // Should not panic
}
