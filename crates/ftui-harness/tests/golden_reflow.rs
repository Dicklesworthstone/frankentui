#![forbid(unsafe_code)]

//! Golden snapshot tests for responsive reflow behavior.
//!
//! These tests verify that layout reflow produces deterministic output
//! across different terminal sizes and breakpoint transitions.
//!
//! # Test Coverage
//!
//! - Breakpoint transitions (xs → sm → md → lg → xl)
//! - Resize transitions (grow/shrink at breakpoint boundaries)
//! - Layout stability during rapid resize sequences
//! - Content reflow consistency (text wrapping, column collapsing)
//!
//! # Running Tests
//!
//! ```sh
//! cargo test -p ftui-harness golden_reflow
//! ```
//!
//! # Updating Golden Checksums
//!
//! ```sh
//! BLESS=1 cargo test -p ftui-harness golden_reflow
//! ```

use ftui_core::geometry::Rect;
use ftui_harness::golden::{
    GoldenOutcome, compute_buffer_checksum, golden_checksum_path, is_bless_mode,
    load_golden_checksums, save_golden_checksums, verify_checksums,
};
use ftui_layout::{Breakpoint, Responsive};
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_text::Text;
use ftui_text::display_width;
use ftui_widgets::Widget;
use ftui_widgets::block::Block;
use ftui_widgets::borders::Borders;
use ftui_widgets::columns::Columns;
use ftui_widgets::paragraph::Paragraph;
use std::path::Path;

fn golden_base_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

// ============================================================================
// Breakpoint Test Utilities
// ============================================================================

/// Breakpoint configuration for tests.
struct BreakpointConfig {
    sm_width: u16,
    md_width: u16,
    lg_width: u16,
    xl_width: u16,
}

impl Default for BreakpointConfig {
    fn default() -> Self {
        // Standard TUI breakpoints
        Self {
            sm_width: 60,  // Small tablet
            md_width: 80,  // Standard terminal
            lg_width: 120, // Wide terminal
            xl_width: 160, // Ultra-wide
        }
    }
}

/// Determine breakpoint for a given width.
fn width_to_breakpoint(width: u16, config: &BreakpointConfig) -> Breakpoint {
    if width < config.sm_width {
        Breakpoint::Xs
    } else if width < config.md_width {
        Breakpoint::Sm
    } else if width < config.lg_width {
        Breakpoint::Md
    } else if width < config.xl_width {
        Breakpoint::Lg
    } else {
        Breakpoint::Xl
    }
}

// ============================================================================
// Reflow Rendering Helpers
// ============================================================================

/// Render a responsive layout that changes based on breakpoint.
fn render_responsive_layout(buf: &mut Buffer, width: u16, height: u16) {
    let config = BreakpointConfig::default();
    let bp = width_to_breakpoint(width, &config);

    // Responsive column count
    let columns = Responsive::new(1)
        .at(Breakpoint::Sm, 2)
        .at(Breakpoint::Md, 3)
        .at(Breakpoint::Lg, 4);
    let num_cols = *columns.resolve(bp);

    // Render border
    render_border(buf, width, height);

    // Render breakpoint label
    let label = format!("{:?} ({}x{}, cols={})", bp, width, height, num_cols);
    render_centered_text(buf, &label, width, 1);

    // Render column layout preview
    render_column_preview(buf, width, height, num_cols);
}

fn render_border(buf: &mut Buffer, width: u16, height: u16) {
    for x in 0..width {
        buf.set(x, 0, Cell::from_char('─'));
        buf.set(x, height.saturating_sub(1), Cell::from_char('─'));
    }
    for y in 0..height {
        buf.set(0, y, Cell::from_char('│'));
        buf.set(width.saturating_sub(1), y, Cell::from_char('│'));
    }
    buf.set(0, 0, Cell::from_char('┌'));
    buf.set(width.saturating_sub(1), 0, Cell::from_char('┐'));
    buf.set(0, height.saturating_sub(1), Cell::from_char('└'));
    buf.set(
        width.saturating_sub(1),
        height.saturating_sub(1),
        Cell::from_char('┘'),
    );
}

fn render_centered_text(buf: &mut Buffer, text: &str, width: u16, y: u16) {
    let start_x = (width.saturating_sub(display_width(text) as u16)) / 2;
    for (i, c) in text.chars().enumerate() {
        let x = start_x + i as u16;
        if x > 0 && x < width.saturating_sub(1) {
            buf.set(x, y, Cell::from_char(c));
        }
    }
}

fn render_column_preview(buf: &mut Buffer, width: u16, height: u16, num_cols: usize) {
    let inner_width = width.saturating_sub(2);
    let _inner_height = height.saturating_sub(4);
    let col_width = inner_width / num_cols as u16;

    for col in 0..num_cols {
        let col_start = 1 + col as u16 * col_width;
        let col_end = if col == num_cols - 1 {
            width.saturating_sub(2)
        } else {
            col_start + col_width.saturating_sub(1)
        };

        // Column header
        let header = format!("Col {}", col + 1);
        for (i, c) in header.chars().enumerate() {
            let x = col_start + i as u16;
            if x < col_end && x < width.saturating_sub(1) && 3 < height.saturating_sub(1) {
                buf.set(x, 3, Cell::from_char(c));
            }
        }

        // Column separator
        if col < num_cols - 1 {
            for y in 3..height.saturating_sub(1) {
                if col_end < width.saturating_sub(1) {
                    buf.set(col_end, y, Cell::from_char('│'));
                }
            }
        }
    }
}

// ============================================================================
// Golden Test Runner
// ============================================================================

struct ReflowTestResult {
    _scenario: String,
    _checksums: Vec<String>,
    _expected: Vec<String>,
    passed: bool,
    mismatch_index: Option<usize>,
}

fn run_reflow_test(name: &str, widths: &[(u16, u16)], base_dir: &Path) -> ReflowTestResult {
    let mut checksums = Vec::new();

    for &(width, height) in widths {
        let mut buf = Buffer::new(width, height);
        render_responsive_layout(&mut buf, width, height);
        checksums.push(compute_buffer_checksum(&buf));
    }

    let checksum_path = golden_checksum_path(base_dir, name);
    let expected = load_golden_checksums(&checksum_path).unwrap_or_default();

    if is_bless_mode() {
        let _ = save_golden_checksums(&checksum_path, &checksums);
        return ReflowTestResult {
            _scenario: name.to_string(),
            _checksums: checksums,
            _expected: vec![],
            passed: true,
            mismatch_index: None,
        };
    }

    let (outcome, mismatch_index) = verify_checksums(&checksums, &expected);
    ReflowTestResult {
        _scenario: name.to_string(),
        _checksums: checksums,
        _expected: expected,
        passed: outcome == GoldenOutcome::Pass,
        mismatch_index,
    }
}

// ============================================================================
// Breakpoint Boundary Tests
// ============================================================================

#[test]
fn golden_reflow_breakpoint_xs() {
    let sizes = [(40, 24), (35, 20), (45, 30)];
    let base_dir = golden_base_dir();
    let result = run_reflow_test("reflow_breakpoint_xs", &sizes, base_dir);
    assert!(
        result.passed,
        "Breakpoint Xs test failed at index {:?}",
        result.mismatch_index
    );
}

#[test]
fn golden_reflow_breakpoint_sm() {
    let sizes = [(60, 24), (55, 20), (75, 30)];
    let base_dir = golden_base_dir();
    let result = run_reflow_test("reflow_breakpoint_sm", &sizes, base_dir);
    assert!(
        result.passed,
        "Breakpoint Sm test failed at index {:?}",
        result.mismatch_index
    );
}

#[test]
fn golden_reflow_breakpoint_md() {
    let sizes = [(80, 24), (85, 25), (115, 35)];
    let base_dir = golden_base_dir();
    let result = run_reflow_test("reflow_breakpoint_md", &sizes, base_dir);
    assert!(
        result.passed,
        "Breakpoint Md test failed at index {:?}",
        result.mismatch_index
    );
}

#[test]
fn golden_reflow_breakpoint_lg() {
    let sizes = [(120, 40), (130, 45), (155, 50)];
    let base_dir = golden_base_dir();
    let result = run_reflow_test("reflow_breakpoint_lg", &sizes, base_dir);
    assert!(
        result.passed,
        "Breakpoint Lg test failed at index {:?}",
        result.mismatch_index
    );
}

#[test]
fn golden_reflow_breakpoint_xl() {
    let sizes = [(160, 50), (180, 55), (200, 60)];
    let base_dir = golden_base_dir();
    let result = run_reflow_test("reflow_breakpoint_xl", &sizes, base_dir);
    assert!(
        result.passed,
        "Breakpoint Xl test failed at index {:?}",
        result.mismatch_index
    );
}

// ============================================================================
// Resize Transition Tests
// ============================================================================

#[test]
fn golden_reflow_transition_grow_xs_to_md() {
    // Test growing from Xs through Sm to Md
    let sizes = [(40, 24), (60, 24), (80, 24)];
    let base_dir = golden_base_dir();
    let result = run_reflow_test("reflow_transition_grow_xs_to_md", &sizes, base_dir);
    assert!(
        result.passed,
        "Grow transition Xs→Md failed at index {:?}",
        result.mismatch_index
    );
}

#[test]
fn golden_reflow_transition_grow_md_to_xl() {
    // Test growing from Md through Lg to Xl
    let sizes = [(80, 24), (120, 40), (160, 50)];
    let base_dir = golden_base_dir();
    let result = run_reflow_test("reflow_transition_grow_md_to_xl", &sizes, base_dir);
    assert!(
        result.passed,
        "Grow transition Md→Xl failed at index {:?}",
        result.mismatch_index
    );
}

#[test]
fn golden_reflow_transition_shrink_xl_to_md() {
    // Test shrinking from Xl through Lg to Md
    let sizes = [(160, 50), (120, 40), (80, 24)];
    let base_dir = golden_base_dir();
    let result = run_reflow_test("reflow_transition_shrink_xl_to_md", &sizes, base_dir);
    assert!(
        result.passed,
        "Shrink transition Xl→Md failed at index {:?}",
        result.mismatch_index
    );
}

#[test]
fn golden_reflow_transition_shrink_md_to_xs() {
    // Test shrinking from Md through Sm to Xs
    let sizes = [(80, 24), (60, 24), (40, 24)];
    let base_dir = golden_base_dir();
    let result = run_reflow_test("reflow_transition_shrink_md_to_xs", &sizes, base_dir);
    assert!(
        result.passed,
        "Shrink transition Md→Xs failed at index {:?}",
        result.mismatch_index
    );
}

// ============================================================================
// Boundary Edge Cases
// ============================================================================

#[test]
fn golden_reflow_at_boundary_sm() {
    // Test exactly at and around the Sm boundary (60 pixels)
    let sizes = [(59, 24), (60, 24), (61, 24)];
    let base_dir = golden_base_dir();
    let result = run_reflow_test("reflow_boundary_sm", &sizes, base_dir);
    assert!(
        result.passed,
        "Boundary Sm test failed at index {:?}",
        result.mismatch_index
    );
}

#[test]
fn golden_reflow_at_boundary_md() {
    // Test exactly at and around the Md boundary (80 pixels)
    let sizes = [(79, 24), (80, 24), (81, 24)];
    let base_dir = golden_base_dir();
    let result = run_reflow_test("reflow_boundary_md", &sizes, base_dir);
    assert!(
        result.passed,
        "Boundary Md test failed at index {:?}",
        result.mismatch_index
    );
}

#[test]
fn golden_reflow_at_boundary_lg() {
    // Test exactly at and around the Lg boundary (120 pixels)
    let sizes = [(119, 40), (120, 40), (121, 40)];
    let base_dir = golden_base_dir();
    let result = run_reflow_test("reflow_boundary_lg", &sizes, base_dir);
    assert!(
        result.passed,
        "Boundary Lg test failed at index {:?}",
        result.mismatch_index
    );
}

#[test]
fn golden_reflow_at_boundary_xl() {
    // Test exactly at and around the Xl boundary (160 pixels)
    let sizes = [(159, 50), (160, 50), (161, 50)];
    let base_dir = golden_base_dir();
    let result = run_reflow_test("reflow_boundary_xl", &sizes, base_dir);
    assert!(
        result.passed,
        "Boundary Xl test failed at index {:?}",
        result.mismatch_index
    );
}

// ============================================================================
// Rapid Resize Stability Tests
// ============================================================================

#[test]
fn golden_reflow_rapid_resize_jitter() {
    // Test rapid resize back and forth (simulates window drag)
    let sizes = [(80, 24), (82, 24), (78, 24), (80, 24), (85, 24), (75, 24)];
    let base_dir = golden_base_dir();
    let result = run_reflow_test("reflow_rapid_jitter", &sizes, base_dir);
    assert!(
        result.passed,
        "Rapid jitter test failed at index {:?}",
        result.mismatch_index
    );
}

#[test]
fn golden_reflow_rapid_boundary_cross() {
    // Test rapid crossing of breakpoint boundaries
    let sizes = [(79, 24), (81, 24), (79, 24), (81, 24)];
    let base_dir = golden_base_dir();
    let result = run_reflow_test("reflow_rapid_boundary_cross", &sizes, base_dir);
    assert!(
        result.passed,
        "Rapid boundary cross test failed at index {:?}",
        result.mismatch_index
    );
}

// ============================================================================
// Height Variation Tests
// ============================================================================

#[test]
fn golden_reflow_height_variations() {
    // Test same width at different heights
    let sizes = [(80, 10), (80, 24), (80, 40), (80, 60)];
    let base_dir = golden_base_dir();
    let result = run_reflow_test("reflow_height_variations", &sizes, base_dir);
    assert!(
        result.passed,
        "Height variations test failed at index {:?}",
        result.mismatch_index
    );
}

#[test]
fn golden_reflow_minimum_viable_size() {
    // Test at minimum viable terminal sizes
    let sizes = [(20, 5), (30, 8), (40, 10)];
    let base_dir = golden_base_dir();
    let result = run_reflow_test("reflow_minimum_viable", &sizes, base_dir);
    assert!(
        result.passed,
        "Minimum viable size test failed at index {:?}",
        result.mismatch_index
    );
}

// ============================================================================
// Isomorphism Property Tests
// ============================================================================

#[test]
fn golden_reflow_determinism_same_size() {
    // Verify same size produces identical checksums
    let mut buf1 = Buffer::new(80, 24);
    render_responsive_layout(&mut buf1, 80, 24);

    let mut buf2 = Buffer::new(80, 24);
    render_responsive_layout(&mut buf2, 80, 24);

    let checksum1 = compute_buffer_checksum(&buf1);
    let checksum2 = compute_buffer_checksum(&buf2);

    assert_eq!(
        checksum1, checksum2,
        "Same size should produce identical checksums"
    );
}

#[test]
fn golden_reflow_idempotent_resize() {
    // Verify resize A→B→A produces same checksum as original A
    let mut buf_initial = Buffer::new(80, 24);
    render_responsive_layout(&mut buf_initial, 80, 24);
    let checksum_initial = compute_buffer_checksum(&buf_initial);

    // Resize to larger
    let mut buf_larger = Buffer::new(120, 40);
    render_responsive_layout(&mut buf_larger, 120, 40);

    // Resize back to original
    let mut buf_final = Buffer::new(80, 24);
    render_responsive_layout(&mut buf_final, 80, 24);
    let checksum_final = compute_buffer_checksum(&buf_final);

    assert_eq!(
        checksum_initial, checksum_final,
        "Resize A→B→A should produce same checksum as original"
    );
}

#[test]
fn golden_reflow_breakpoint_consistency() {
    // Verify all sizes within same breakpoint use same column count
    let config = BreakpointConfig::default();

    // All Md sizes should have same column count
    let md_sizes = [80, 85, 100, 115];
    let mut md_cols = Vec::new();

    let columns = Responsive::new(1)
        .at(Breakpoint::Sm, 2)
        .at(Breakpoint::Md, 3)
        .at(Breakpoint::Lg, 4);

    for width in md_sizes {
        let bp = width_to_breakpoint(width, &config);
        assert_eq!(
            bp,
            Breakpoint::Md,
            "Width {} should be Md breakpoint",
            width
        );
        md_cols.push(*columns.resolve(bp));
    }

    // All should be the same
    assert!(
        md_cols.iter().all(|&c| c == md_cols[0]),
        "All Md breakpoint sizes should have same column count"
    );
}

// ============================================================================
// Widget Integration Tests
// ============================================================================

#[test]
fn golden_reflow_widget_paragraph() {
    let mut pool = GraphemePool::new();

    // Test paragraph reflow at different widths
    let text = "This is a longer paragraph that should reflow differently at various terminal widths. The text wrapping behavior should be consistent and predictable.";

    let sizes = [(40, 10), (80, 10), (120, 10)];
    let mut checksums = Vec::new();

    for (width, height) in sizes {
        let mut frame = Frame::new(width, height, &mut pool);
        let para = Paragraph::new(Text::raw(text))
            .block(Block::default().borders(Borders::ALL).title("Paragraph"));
        para.render(Rect::new(0, 0, width, height), &mut frame);
        checksums.push(compute_buffer_checksum(&frame.buffer));
    }

    // Different widths should produce different checksums (text wraps differently)
    assert_ne!(
        checksums[0], checksums[1],
        "40-wide and 80-wide should wrap differently"
    );
    assert_ne!(
        checksums[1], checksums[2],
        "80-wide and 120-wide should wrap differently"
    );
}

#[test]
fn golden_reflow_widget_columns() {
    let mut pool = GraphemePool::new();

    // Test columns widget at different widths
    let sizes = [(60, 10), (100, 10), (140, 10)];
    let mut checksums = Vec::new();

    for (width, height) in sizes {
        let mut frame = Frame::new(width, height, &mut pool);
        let columns = Columns::new()
            .add(Paragraph::new(Text::raw("Left")))
            .add(Paragraph::new(Text::raw("Right")))
            .gap(1);
        columns.render(Rect::new(0, 0, width, height), &mut frame);
        checksums.push(compute_buffer_checksum(&frame.buffer));
    }

    // Each width should have deterministic output
    for i in 0..checksums.len() {
        for j in (i + 1)..checksums.len() {
            assert_ne!(
                checksums[i], checksums[j],
                "Different widths should produce different layouts"
            );
        }
    }
}
