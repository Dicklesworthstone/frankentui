#![forbid(unsafe_code)]

//! Comprehensive reflow snapshot tests for responsive terminal layouts.
//!
//! These tests verify:
//! - Multi-step resize sequences maintain content integrity
//! - Edge cases (minimal/extreme sizes) handle gracefully
//! - Responsive breakpoints trigger expected layout changes
//! - Property-based invariants hold across all size transitions
//!
//! # Running Tests
//!
//! ```sh
//! cargo test -p ftui-harness reflow_
//! ```
//!
//! # Updating Snapshots
//!
//! ```sh
//! BLESS=1 cargo test -p ftui-harness reflow_
//! ```
//!
//! # Deterministic Mode
//!
//! ```sh
//! GOLDEN_SEED=42 cargo test -p ftui-harness reflow_
//! ```

use ftui_core::geometry::Rect;
use ftui_harness::golden::{
    GoldenEnv, GoldenLogger, GoldenOutcome, ResizeScenario, compute_buffer_checksum,
    verify_checksums,
};
use ftui_render::buffer::Buffer;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_text::Text;
use ftui_widgets::Widget;
use ftui_widgets::block::Block;
use ftui_widgets::borders::Borders;
use ftui_widgets::paragraph::Paragraph;
use std::time::Instant;

// ============================================================================
// Test Infrastructure
// ============================================================================

/// A reflow test case with multiple resize steps.
struct ReflowTestCase {
    initial_size: (u16, u16),
    resize_steps: Vec<(u16, u16)>,
    content: &'static str,
}

impl ReflowTestCase {
    fn new(_name: &'static str, initial: (u16, u16), content: &'static str) -> Self {
        Self {
            initial_size: initial,
            resize_steps: Vec::new(),
            content,
        }
    }

    fn then_resize(mut self, width: u16, height: u16) -> Self {
        self.resize_steps.push((width, height));
        self
    }
}

/// Render content into a buffer with a bordered paragraph.
fn render_content(buf: &mut Buffer, content: &str) {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(buf.width(), buf.height(), &mut pool);

    let para = Paragraph::new(Text::raw(content))
        .block(Block::default().borders(Borders::ALL).title("Reflow Test"));

    let rect = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    para.render(rect, &mut frame);
    *buf = frame.buffer;
}

/// Run a reflow test case and collect checksums.
fn run_reflow_test(case: &ReflowTestCase) -> Vec<String> {
    let mut checksums = Vec::new();

    // Initial render
    let (w, h) = case.initial_size;
    let mut buf = Buffer::new(w, h);
    render_content(&mut buf, case.content);
    checksums.push(compute_buffer_checksum(&buf));

    // Apply resize steps
    for &(new_w, new_h) in &case.resize_steps {
        let mut new_buf = Buffer::new(new_w, new_h);
        render_content(&mut new_buf, case.content);
        checksums.push(compute_buffer_checksum(&new_buf));
    }

    checksums
}

// ============================================================================
// Multi-Step Resize Sequence Tests
// ============================================================================

#[test]
fn reflow_multi_step_grow_shrink_grow() {
    let case = ReflowTestCase::new(
        "grow_shrink_grow",
        (40, 10),
        "This content should reflow smoothly across multiple resize operations.",
    )
    .then_resize(80, 24) // grow
    .then_resize(40, 10) // shrink back
    .then_resize(120, 40); // grow larger

    let checksums = run_reflow_test(&case);

    // Verify we got checksums for all steps
    assert_eq!(checksums.len(), 4);

    // First and third should match (same size)
    assert_eq!(
        checksums[0], checksums[2],
        "Same size should produce same checksum"
    );

    // All should be valid checksums
    for cs in &checksums {
        assert!(cs.starts_with("blake3:"));
    }
}

#[test]
fn reflow_step_by_step_width_increase() {
    let case = ReflowTestCase::new(
        "step_width_increase",
        (40, 10),
        "Width increases step by step.",
    )
    .then_resize(50, 10)
    .then_resize(60, 10)
    .then_resize(70, 10)
    .then_resize(80, 10);

    let checksums = run_reflow_test(&case);
    assert_eq!(checksums.len(), 5);

    // Each width change should produce a different checksum
    for i in 0..checksums.len() - 1 {
        assert_ne!(
            checksums[i],
            checksums[i + 1],
            "Width change should affect checksum"
        );
    }
}

#[test]
fn reflow_step_by_step_height_increase() {
    let case = ReflowTestCase::new(
        "step_height_increase",
        (80, 10),
        "Height increases.\nLine 2.\nLine 3.",
    )
    .then_resize(80, 15)
    .then_resize(80, 20)
    .then_resize(80, 25)
    .then_resize(80, 30);

    let checksums = run_reflow_test(&case);
    assert_eq!(checksums.len(), 5);

    // Each height change should produce a different checksum
    for i in 0..checksums.len() - 1 {
        assert_ne!(
            checksums[i],
            checksums[i + 1],
            "Height change should affect checksum"
        );
    }
}

#[test]
fn reflow_diagonal_resize_sequence() {
    let case = ReflowTestCase::new(
        "diagonal_resize",
        (40, 10),
        "Diagonal resize: both dimensions change.",
    )
    .then_resize(50, 15)
    .then_resize(60, 20)
    .then_resize(70, 25)
    .then_resize(80, 30);

    let checksums = run_reflow_test(&case);
    assert_eq!(checksums.len(), 5);

    // All checksums should be unique
    let unique: std::collections::HashSet<_> = checksums.iter().collect();
    assert_eq!(unique.len(), checksums.len(), "All sizes should be unique");
}

// ============================================================================
// Edge Case Tests - Minimal Sizes
// ============================================================================

#[test]
fn reflow_minimal_width_3x10() {
    let mut buf = Buffer::new(3, 10);
    render_content(&mut buf, "X");
    let checksum = compute_buffer_checksum(&buf);
    assert!(checksum.starts_with("blake3:"));
}

#[test]
fn reflow_minimal_height_80x3() {
    let mut buf = Buffer::new(80, 3);
    render_content(&mut buf, "Content in minimal height");
    let checksum = compute_buffer_checksum(&buf);
    assert!(checksum.starts_with("blake3:"));
}

#[test]
fn reflow_minimal_both_3x3() {
    let mut buf = Buffer::new(3, 3);
    render_content(&mut buf, "T");
    let checksum = compute_buffer_checksum(&buf);
    assert!(checksum.starts_with("blake3:"));
}

#[test]
fn reflow_width_2_boundary() {
    // Width 2 is the absolute minimum for borders
    let mut buf = Buffer::new(2, 5);
    // With borders, no space for content
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(buf.width(), buf.height(), &mut pool);
    let block = Block::default().borders(Borders::ALL);
    block.render(Rect::new(0, 0, 2, 5), &mut frame);
    buf = frame.buffer;
    let checksum = compute_buffer_checksum(&buf);
    assert!(checksum.starts_with("blake3:"));
}

#[test]
fn reflow_height_2_boundary() {
    // Height 2 is the absolute minimum for borders
    let mut buf = Buffer::new(80, 2);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(buf.width(), buf.height(), &mut pool);
    let block = Block::default().borders(Borders::ALL);
    block.render(Rect::new(0, 0, 80, 2), &mut frame);
    buf = frame.buffer;
    let checksum = compute_buffer_checksum(&buf);
    assert!(checksum.starts_with("blake3:"));
}

// ============================================================================
// Edge Case Tests - Extreme Sizes
// ============================================================================

#[test]
fn reflow_extreme_width_500x24() {
    let mut buf = Buffer::new(500, 24);
    render_content(&mut buf, "Very wide terminal");
    let checksum = compute_buffer_checksum(&buf);
    assert!(checksum.starts_with("blake3:"));
}

#[test]
fn reflow_extreme_height_80x200() {
    let mut buf = Buffer::new(80, 200);
    render_content(&mut buf, "Very tall terminal\n".repeat(50).as_str());
    let checksum = compute_buffer_checksum(&buf);
    assert!(checksum.starts_with("blake3:"));
}

#[test]
fn reflow_extreme_both_300x100() {
    let mut buf = Buffer::new(300, 100);
    render_content(&mut buf, "Large terminal");
    let checksum = compute_buffer_checksum(&buf);
    assert!(checksum.starts_with("blake3:"));
}

// ============================================================================
// Responsive Breakpoint Tests
// ============================================================================

/// Common responsive breakpoints for terminal UIs.
const BREAKPOINTS: [(u16, &str); 5] = [
    (40, "mobile"),
    (60, "tablet"),
    (80, "standard"),
    (120, "wide"),
    (200, "ultrawide"),
];

#[test]
fn reflow_across_all_breakpoints() {
    let content = "Responsive content that adapts to different terminal widths.";

    let mut checksums = Vec::new();
    for (width, _name) in BREAKPOINTS {
        let mut buf = Buffer::new(width, 24);
        render_content(&mut buf, content);
        checksums.push(compute_buffer_checksum(&buf));
    }

    // All breakpoints should produce different checksums
    let unique: std::collections::HashSet<_> = checksums.iter().collect();
    assert_eq!(unique.len(), checksums.len());
}

#[test]
fn reflow_breakpoint_transition_up() {
    let content = "Content for breakpoint transitions.";

    let case = ReflowTestCase::new("breakpoint_up", (40, 24), content)
        .then_resize(60, 24)
        .then_resize(80, 24)
        .then_resize(120, 24)
        .then_resize(200, 24);

    let checksums = run_reflow_test(&case);
    assert_eq!(checksums.len(), 5);
}

#[test]
fn reflow_breakpoint_transition_down() {
    let content = "Content for breakpoint transitions.";

    let case = ReflowTestCase::new("breakpoint_down", (200, 24), content)
        .then_resize(120, 24)
        .then_resize(80, 24)
        .then_resize(60, 24)
        .then_resize(40, 24);

    let checksums = run_reflow_test(&case);
    assert_eq!(checksums.len(), 5);
}

// ============================================================================
// Property-Based Invariant Tests
// ============================================================================

#[test]
fn reflow_property_deterministic_rendering() {
    // Same size + content should always produce same checksum
    let content = "Deterministic content test.";

    for _ in 0..3 {
        let mut buf1 = Buffer::new(80, 24);
        let mut buf2 = Buffer::new(80, 24);

        render_content(&mut buf1, content);
        render_content(&mut buf2, content);

        let cs1 = compute_buffer_checksum(&buf1);
        let cs2 = compute_buffer_checksum(&buf2);

        assert_eq!(cs1, cs2, "Deterministic rendering violated");
    }
}

#[test]
fn reflow_property_resize_reversible() {
    // A -> B -> A should produce same checksum as original A
    let content = "Reversibility test content.";

    let mut buf_a = Buffer::new(80, 24);
    render_content(&mut buf_a, content);
    let cs_a = compute_buffer_checksum(&buf_a);

    // Resize to B
    let mut buf_b = Buffer::new(120, 40);
    render_content(&mut buf_b, content);
    let cs_b = compute_buffer_checksum(&buf_b);

    // Resize back to A
    let mut buf_a2 = Buffer::new(80, 24);
    render_content(&mut buf_a2, content);
    let cs_a2 = compute_buffer_checksum(&buf_a2);

    assert_ne!(cs_a, cs_b, "Different sizes should differ");
    assert_eq!(cs_a, cs_a2, "Return to same size should match");
}

#[test]
fn reflow_property_size_affects_checksum() {
    // Different sizes should produce different checksums
    let content = "Size sensitivity test.";
    let sizes = [(80, 24), (81, 24), (80, 25), (79, 23)];

    let checksums: Vec<_> = sizes
        .iter()
        .map(|&(w, h)| {
            let mut buf = Buffer::new(w, h);
            render_content(&mut buf, content);
            compute_buffer_checksum(&buf)
        })
        .collect();

    // All should be unique
    let unique: std::collections::HashSet<_> = checksums.iter().collect();
    assert_eq!(unique.len(), checksums.len());
}

#[test]
fn reflow_property_content_affects_checksum() {
    // Different content at same size should produce different checksums
    let contents = ["Content A", "Content B", "Content C", "Different text"];

    let checksums: Vec<_> = contents
        .iter()
        .map(|content| {
            let mut buf = Buffer::new(80, 24);
            render_content(&mut buf, content);
            compute_buffer_checksum(&buf)
        })
        .collect();

    // All should be unique
    let unique: std::collections::HashSet<_> = checksums.iter().collect();
    assert_eq!(unique.len(), checksums.len());
}

#[test]
fn reflow_property_checksum_format_valid() {
    // All checksums should have correct format
    let sizes = [(40, 10), (80, 24), (120, 40), (200, 60)];

    for (w, h) in sizes {
        let mut buf = Buffer::new(w, h);
        render_content(&mut buf, "Test");
        let checksum = compute_buffer_checksum(&buf);

        assert!(checksum.starts_with("blake3:"), "Missing prefix");
        assert_eq!(
            checksum.len(),
            7 + 16,
            "Wrong length: expected 23, got {}",
            checksum.len()
        );

        // Hex chars only after prefix
        let hex_part = &checksum[7..];
        assert!(
            hex_part.chars().all(|c| c.is_ascii_hexdigit()),
            "Non-hex chars in checksum"
        );
    }
}

// ============================================================================
// Multi-Content Reflow Tests
// ============================================================================

#[test]
fn reflow_multiline_content_preservation() {
    let content = "Line 1: Header\nLine 2: Content\nLine 3: More content\nLine 4: Footer";

    let case = ReflowTestCase::new("multiline_preservation", (80, 10), content)
        .then_resize(40, 20) // Narrower, taller
        .then_resize(120, 8); // Wider, shorter

    let checksums = run_reflow_test(&case);
    assert_eq!(checksums.len(), 3);

    // All sizes should produce valid, different checksums
    let unique: std::collections::HashSet<_> = checksums.iter().collect();
    assert_eq!(unique.len(), 3);
}

#[test]
fn reflow_long_line_wrapping() {
    let long_line = "This is a very long line that should wrap differently at different widths. \
                     It contains enough text to require wrapping even at wide terminal widths.";

    let case = ReflowTestCase::new("long_line_wrap", (40, 10), long_line)
        .then_resize(60, 10)
        .then_resize(80, 10)
        .then_resize(120, 10);

    let checksums = run_reflow_test(&case);
    assert_eq!(checksums.len(), 4);
}

#[test]
fn reflow_empty_content() {
    let case = ReflowTestCase::new("empty_content", (80, 24), "")
        .then_resize(40, 10)
        .then_resize(120, 40);

    let checksums = run_reflow_test(&case);
    assert_eq!(checksums.len(), 3);

    // Empty content should still produce valid checksums
    for cs in &checksums {
        assert!(cs.starts_with("blake3:"));
    }
}

#[test]
fn reflow_unicode_content() {
    let content = "Unicode: \u{1F600} \u{1F4BB} \u{2764}\nChinese: \u{4E2D}\u{6587}\nJapanese: \u{65E5}\u{672C}\u{8A9E}";

    let case = ReflowTestCase::new("unicode_content", (80, 10), content)
        .then_resize(40, 15)
        .then_resize(120, 8);

    let checksums = run_reflow_test(&case);
    assert_eq!(checksums.len(), 3);
}

// ============================================================================
// Rapid Resize Sequence Tests (Stress Testing)
// ============================================================================

#[test]
fn reflow_rapid_oscillation() {
    let content = "Rapid size oscillation test.";

    let mut case = ReflowTestCase::new("rapid_oscillation", (80, 24), content);
    for i in 0..10 {
        if i % 2 == 0 {
            case = case.then_resize(40, 10);
        } else {
            case = case.then_resize(120, 40);
        }
    }

    let checksums = run_reflow_test(&case);
    assert_eq!(checksums.len(), 11);

    // Odd indices (40x10) should all match
    let small_checksums: Vec<_> = checksums.iter().skip(1).step_by(2).collect();
    assert!(
        small_checksums.windows(2).all(|w| w[0] == w[1]),
        "Same size should produce same checksum"
    );
}

#[test]
fn reflow_incremental_grow() {
    let content = "Incremental growth test.";

    let mut case = ReflowTestCase::new("incremental_grow", (20, 5), content);
    for i in 1..=10 {
        case = case.then_resize(20 + i * 10, 5 + i * 2);
    }

    let checksums = run_reflow_test(&case);
    assert_eq!(checksums.len(), 11);

    // All should be unique (monotonic growth)
    let unique: std::collections::HashSet<_> = checksums.iter().collect();
    assert_eq!(unique.len(), checksums.len());
}

// ============================================================================
// Golden Output Comparison Tests
// ============================================================================

#[test]
fn reflow_golden_standard_scenario() {
    let scenario = ResizeScenario::resize("reflow_standard", 80, 24, 120, 40);

    let mut buf = Buffer::new(80, 24);
    render_content(&mut buf, "Standard reflow scenario");
    let cs1 = compute_buffer_checksum(&buf);

    let mut buf2 = Buffer::new(120, 40);
    render_content(&mut buf2, "Standard reflow scenario");
    let cs2 = compute_buffer_checksum(&buf2);

    assert_ne!(cs1, cs2);
    assert!(scenario.resize_steps.len() == 1);
}

#[test]
fn reflow_verify_checksums_pass() {
    let actual = vec!["sha256:abc123".to_string(), "sha256:def456".to_string()];
    let expected = actual.clone();
    let (outcome, idx) = verify_checksums(&actual, &expected);
    assert_eq!(outcome, GoldenOutcome::Pass);
    assert!(idx.is_none());
}

#[test]
fn reflow_verify_checksums_fail() {
    let actual = vec!["sha256:abc123".to_string(), "sha256:different".to_string()];
    let expected = vec!["sha256:abc123".to_string(), "sha256:def456".to_string()];
    let (outcome, idx) = verify_checksums(&actual, &expected);
    assert_eq!(outcome, GoldenOutcome::Fail);
    assert_eq!(idx, Some(1));
}

// ============================================================================
// Performance Sanity Tests
// ============================================================================

#[test]
fn reflow_performance_large_resize_sequence() {
    let content = "Performance test content.";
    let start = Instant::now();

    let mut case = ReflowTestCase::new("performance", (80, 24), content);
    for i in 1..=20 {
        case = case.then_resize(40 + i * 5, 10 + i * 2);
    }

    let checksums = run_reflow_test(&case);
    let elapsed = start.elapsed();

    assert_eq!(checksums.len(), 21);
    // Should complete in reasonable time (< 1 second for 21 renders)
    assert!(elapsed.as_secs() < 1, "Reflow took too long: {:?}", elapsed);
}

#[test]
fn reflow_performance_checksum_computation() {
    let start = Instant::now();

    for size in [(80, 24), (120, 40), (200, 60), (300, 100)] {
        let mut buf = Buffer::new(size.0, size.1);
        render_content(&mut buf, "Checksum performance test");
        let _ = compute_buffer_checksum(&buf);
    }

    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 500,
        "Checksum computation too slow: {:?}",
        elapsed
    );
}

// ============================================================================
// JSONL Logging Integration Tests
// ============================================================================

#[test]
fn reflow_logger_integration() {
    let log_dir = std::env::temp_dir().join("ftui_reflow_logger_test");
    let _ = std::fs::remove_dir_all(&log_dir);
    std::fs::create_dir_all(&log_dir).unwrap();

    let log_path = log_dir.join("reflow_test.jsonl");
    let mut logger = GoldenLogger::new(&log_path).unwrap();

    let env = GoldenEnv::capture();
    logger.log_start("reflow_test_case", &env);

    // Simulate reflow sequence
    let content = "Logger integration test.";
    let sizes = [(80, 24), (120, 40), (60, 15)];

    for (i, &(w, h)) in sizes.iter().enumerate() {
        let mut buf = Buffer::new(w, h);
        render_content(&mut buf, content);
        let checksum = compute_buffer_checksum(&buf);
        logger.log_frame(i as u32, w, h, &checksum, 10);

        if i > 0 {
            let (prev_w, prev_h) = sizes[i - 1];
            logger.log_resize(prev_w, prev_h, w, h, 5);
        }
    }

    logger.log_complete(GoldenOutcome::Pass);

    // Verify log file
    let content = std::fs::read_to_string(&log_path).unwrap();
    assert!(content.contains("\"event\":\"start\""));
    assert!(content.contains("\"event\":\"frame\""));
    assert!(content.contains("\"event\":\"complete\""));

    let _ = std::fs::remove_dir_all(&log_dir);
}
