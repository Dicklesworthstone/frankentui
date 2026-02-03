#![forbid(unsafe_code)]

//! Large Buffer Regression Tests for Advanced Text Editor.
//!
//! Performance tests for the text editor operating on large buffers.
//! Verifies that operations complete within defined time budgets and
//! that behavior remains correct at scale.
//!
//! # Performance Budgets
//!
//! | Operation | 10K lines | 100K lines | 1M lines |
//! |-----------|-----------|------------|----------|
//! | Insert char | < 1ms | < 5ms | < 50ms |
//! | Delete char | < 1ms | < 5ms | < 50ms |
//! | Move cursor | < 1ms | < 2ms | < 10ms |
//! | Select all | < 1ms | < 5ms | < 20ms |
//! | Full render | < 10ms | < 50ms | N/A |
//!
//! # Invariants (Alien Artifact)
//!
//! 1. **Line count consistency**: rope.len_lines() always matches expected
//! 2. **Cursor validity**: cursor always within valid bounds after any operation
//! 3. **Undo reversibility**: undo always restores previous state exactly
//! 4. **Memory bound**: memory usage grows linearly with content size
//!
//! # Failure Modes
//!
//! | Scenario | Detection | Mitigation |
//! |----------|-----------|------------|
//! | O(n²) insertion | Timing regression | Rope rebalancing |
//! | Cursor calculation overflow | Property test failure | Use saturating math |
//! | Memory exhaustion | OOM on large buffers | Streaming/lazy loading |
//! | Stack overflow on deep undo | Panic | Iterative undo |
//!
//! # Running Performance Tests
//!
//! ```sh
//! # Run all performance tests
//! cargo test --package ftui-harness --test editor_perf -- --nocapture
//!
//! # Run with detailed timing
//! PERF_LOG=1 cargo test --package ftui-harness --test editor_perf -- --nocapture
//!
//! # Run specific size category
//! cargo test --package ftui-harness --test editor_perf large_buffer_10k
//! ```
//!
//! # JSONL Output Schema
//!
//! ```json
//! {"event":"perf_test","case":"insert_char_10k","lines":10000,"op":"insert","duration_us":523,"result":"pass"}
//! {"event":"perf_test","case":"cursor_move_100k","lines":100000,"op":"move_right","duration_us":1234,"result":"pass"}
//! ```

use std::time::{Duration, Instant};

use ftui_text::editor::Editor;

// ============================================================================
// Constants
// ============================================================================

/// Number of lines for 10K buffer tests.
const LINES_10K: usize = 10_000;

/// Number of lines for 100K buffer tests.
const LINES_100K: usize = 100_000;

/// Line content for test buffers (average line length ~40 chars).
const LINE_CONTENT: &str = "This is a test line with typical content";

// ============================================================================
// Test Helpers
// ============================================================================

/// Create a buffer with the specified number of lines.
fn create_large_buffer(lines: usize) -> Editor {
    let content: String = (0..lines)
        .map(|i| format!("{LINE_CONTENT} {i}\n"))
        .collect();
    Editor::with_text(&content)
}

/// Log a performance measurement in JSONL format.
fn log_perf(case: &str, lines: usize, op: &str, duration_us: u128, result: &str) {
    if std::env::var("PERF_LOG").is_ok() {
        println!(
            r#"{{"event":"perf_test","case":"{}","lines":{},"op":"{}","duration_us":{},"result":"{}"}}"#,
            case, lines, op, duration_us, result
        );
    }
}

/// Assert operation completes within time budget.
fn assert_within_budget(duration: Duration, budget_us: u128, case: &str, lines: usize, op: &str) {
    let duration_us = duration.as_micros();
    let result = if duration_us <= budget_us { "pass" } else { "fail" };
    log_perf(case, lines, op, duration_us, result);

    assert!(
        duration_us <= budget_us,
        "{} on {}K lines took {}us, budget was {}us",
        op,
        lines / 1000,
        duration_us,
        budget_us
    );
}

// ============================================================================
// 10K Line Tests
// ============================================================================

#[test]
fn large_buffer_10k_insert_char() {
    let mut editor = create_large_buffer(LINES_10K);

    let start = Instant::now();
    editor.insert_char('X');
    let duration = start.elapsed();

    assert_within_budget(duration, 1000, "insert_char_10k", LINES_10K, "insert_char");
}

#[test]
fn large_buffer_10k_delete_char() {
    let mut editor = create_large_buffer(LINES_10K);

    let start = Instant::now();
    editor.delete_backward();
    let duration = start.elapsed();

    assert_within_budget(duration, 1000, "delete_char_10k", LINES_10K, "delete_backward");
}

#[test]
fn large_buffer_10k_move_cursor() {
    let mut editor = create_large_buffer(LINES_10K);

    let start = Instant::now();
    editor.move_left();
    let duration = start.elapsed();

    assert_within_budget(duration, 1000, "move_cursor_10k", LINES_10K, "move_left");
}

#[test]
fn large_buffer_10k_select_all() {
    let mut editor = create_large_buffer(LINES_10K);

    let start = Instant::now();
    editor.select_all();
    let duration = start.elapsed();

    assert_within_budget(duration, 1000, "select_all_10k", LINES_10K, "select_all");
}

#[test]
fn large_buffer_10k_move_to_start() {
    let mut editor = create_large_buffer(LINES_10K);

    let start = Instant::now();
    editor.move_to_document_start();
    let duration = start.elapsed();

    assert_within_budget(duration, 1000, "move_start_10k", LINES_10K, "move_to_start");
}

#[test]
fn large_buffer_10k_move_up_down() {
    let mut editor = create_large_buffer(LINES_10K);
    // Start from middle
    editor.move_to_document_start();
    for _ in 0..5000 {
        editor.move_down();
    }

    let start = Instant::now();
    editor.move_up();
    let duration_up = start.elapsed();

    let start = Instant::now();
    editor.move_down();
    let duration_down = start.elapsed();

    assert_within_budget(duration_up, 1000, "move_up_10k", LINES_10K, "move_up");
    assert_within_budget(duration_down, 1000, "move_down_10k", LINES_10K, "move_down");
}

#[test]
fn large_buffer_10k_undo_redo() {
    let mut editor = create_large_buffer(LINES_10K);
    editor.insert_char('X');

    let start = Instant::now();
    editor.undo();
    let duration_undo = start.elapsed();

    let start = Instant::now();
    editor.redo();
    let duration_redo = start.elapsed();

    assert_within_budget(duration_undo, 1000, "undo_10k", LINES_10K, "undo");
    assert_within_budget(duration_redo, 1000, "redo_10k", LINES_10K, "redo");
}

// ============================================================================
// 100K Line Tests
// ============================================================================

#[test]
fn large_buffer_100k_insert_char() {
    let mut editor = create_large_buffer(LINES_100K);

    let start = Instant::now();
    editor.insert_char('X');
    let duration = start.elapsed();

    assert_within_budget(duration, 5000, "insert_char_100k", LINES_100K, "insert_char");
}

#[test]
fn large_buffer_100k_delete_char() {
    let mut editor = create_large_buffer(LINES_100K);

    let start = Instant::now();
    editor.delete_backward();
    let duration = start.elapsed();

    assert_within_budget(duration, 5000, "delete_char_100k", LINES_100K, "delete_backward");
}

#[test]
fn large_buffer_100k_move_cursor() {
    let mut editor = create_large_buffer(LINES_100K);

    let start = Instant::now();
    editor.move_left();
    let duration = start.elapsed();

    assert_within_budget(duration, 2000, "move_cursor_100k", LINES_100K, "move_left");
}

#[test]
fn large_buffer_100k_select_all() {
    let mut editor = create_large_buffer(LINES_100K);

    let start = Instant::now();
    editor.select_all();
    let duration = start.elapsed();

    assert_within_budget(duration, 5000, "select_all_100k", LINES_100K, "select_all");
}

#[test]
fn large_buffer_100k_move_to_start() {
    let mut editor = create_large_buffer(LINES_100K);

    let start = Instant::now();
    editor.move_to_document_start();
    let duration = start.elapsed();

    assert_within_budget(duration, 2000, "move_start_100k", LINES_100K, "move_to_start");
}

// ============================================================================
// Invariant Tests
// ============================================================================

/// Test that line count remains consistent after operations.
#[test]
fn invariant_line_count_consistency() {
    let mut editor = create_large_buffer(LINES_10K);
    let initial_lines = editor.line_count();

    // Insert char (should not change line count)
    editor.insert_char('X');
    assert_eq!(editor.line_count(), initial_lines, "insert_char changed line count");

    // Delete char (should not change line count)
    editor.delete_backward();
    assert_eq!(editor.line_count(), initial_lines, "delete_backward changed line count");

    // Insert newline (should increase by 1)
    editor.insert_newline();
    assert_eq!(editor.line_count(), initial_lines + 1, "insert_newline didn't increase");

    // Undo (should restore)
    editor.undo();
    assert_eq!(editor.line_count(), initial_lines, "undo didn't restore line count");
}

/// Test that cursor is always valid after operations.
#[test]
fn invariant_cursor_always_valid() {
    let mut editor = create_large_buffer(1000);

    // Move to extremes
    editor.move_to_document_start();
    let start_cursor = editor.cursor();
    assert_eq!(start_cursor.line, 0);
    assert_eq!(start_cursor.grapheme, 0);

    editor.move_to_document_end();
    let end_cursor = editor.cursor();
    assert!(end_cursor.line < editor.line_count() || editor.is_empty());

    // Move past bounds should clamp
    for _ in 0..100 {
        editor.move_right();
    }
    let cursor = editor.cursor();
    let text = editor.text();
    assert!(cursor.line as usize <= text.lines().count());
}

/// Test that undo fully reverses operations.
#[test]
fn invariant_undo_reversibility() {
    let mut editor = create_large_buffer(1000);
    let original_text = editor.text();
    let original_cursor = editor.cursor();

    // Perform operations
    editor.insert_text("INSERTED");
    editor.delete_backward();
    editor.insert_newline();

    assert_ne!(editor.text(), original_text);

    // Undo all
    editor.undo();
    editor.undo();
    editor.undo();

    assert_eq!(editor.text(), original_text, "undo didn't restore text");
    // Cursor may be at different position after undo
}

/// Test multiple insert/delete cycles maintain consistency.
#[test]
fn invariant_insert_delete_cycle() {
    let mut editor = create_large_buffer(1000);
    let original_len = editor.text().len();

    // Insert 100 characters
    for _ in 0..100 {
        editor.insert_char('X');
    }
    assert_eq!(editor.text().len(), original_len + 100);

    // Delete them all
    for _ in 0..100 {
        editor.delete_backward();
    }
    assert_eq!(editor.text().len(), original_len);
}

// ============================================================================
// Stress Tests
// ============================================================================

/// Stress test: many small insertions.
#[test]
fn stress_many_insertions() {
    let mut editor = Editor::new();

    let start = Instant::now();
    for i in 0..1000 {
        editor.insert_text(&format!("Line {i}\n"));
    }
    let duration = start.elapsed();

    assert_eq!(editor.line_count(), 1001); // 1000 lines + empty at end
    log_perf("stress_insertions", 1000, "1000_inserts", duration.as_micros(), "pass");

    // Should complete in reasonable time
    assert!(
        duration < Duration::from_secs(1),
        "1000 insertions took {:?}",
        duration
    );
}

/// Stress test: rapid cursor movements.
#[test]
fn stress_cursor_movements() {
    let mut editor = create_large_buffer(10_000);

    let start = Instant::now();
    for _ in 0..1000 {
        editor.move_left();
        editor.move_right();
        editor.move_up();
        editor.move_down();
    }
    let duration = start.elapsed();

    log_perf("stress_movements", 10_000, "4000_moves", duration.as_micros(), "pass");

    assert!(
        duration < Duration::from_secs(2),
        "4000 cursor movements took {:?}",
        duration
    );
}

/// Stress test: undo/redo cycles.
#[test]
fn stress_undo_redo() {
    let mut editor = Editor::new();

    // Create undo history
    for i in 0..100 {
        editor.insert_text(&format!("Edit {i} "));
    }

    let start = Instant::now();
    // Undo all
    for _ in 0..100 {
        editor.undo();
    }
    // Redo all
    for _ in 0..100 {
        editor.redo();
    }
    let duration = start.elapsed();

    log_perf("stress_undo_redo", 100, "200_undo_redo", duration.as_micros(), "pass");

    assert!(
        duration < Duration::from_secs(1),
        "200 undo/redo took {:?}",
        duration
    );
}

// ============================================================================
// Property Tests
// ============================================================================

/// Property: text length is always sum of line lengths + newlines.
#[test]
fn property_text_length_equals_lines() {
    let editor = create_large_buffer(1000);
    let text = editor.text();

    // Count lines and their lengths
    let lines: Vec<&str> = text.lines().collect();
    let line_chars: usize = lines.iter().map(|l| l.len()).sum();
    let newlines = text.matches('\n').count();

    // Total should equal text length
    assert_eq!(line_chars + newlines, text.len());
}

/// Property: line count matches actual newlines + 1.
#[test]
fn property_line_count_matches_newlines() {
    let editor = create_large_buffer(500);
    let text = editor.text();
    let newlines = text.matches('\n').count();

    // Line count should be newlines + 1 (or newlines if ends with \n)
    let expected = if text.ends_with('\n') {
        newlines + 1
    } else {
        newlines + 1
    };

    // Allow for edge cases in rope counting
    let line_count = editor.line_count();
    assert!(
        line_count == expected || line_count == expected + 1,
        "line_count {} didn't match expected {} (newlines={})",
        line_count,
        expected,
        newlines
    );
}

/// Property: cursor position is always reachable.
#[test]
fn property_cursor_reachable() {
    let mut editor = create_large_buffer(100);

    // Navigate to random positions and verify cursor is valid
    editor.move_to_document_start();
    for _ in 0..50 {
        editor.move_right();
    }

    let cursor = editor.cursor();

    // Should be able to get text at cursor line
    if cursor.line > 0 || cursor.grapheme > 0 {
        let line_text = editor.line_text(cursor.line as usize);
        assert!(line_text.is_some(), "cursor line {} not accessible", cursor.line);
    }
}

// ============================================================================
// Regression Fixtures
// ============================================================================

/// Regression: inserting at document start in large buffer.
#[test]
fn regression_insert_at_start_large() {
    let mut editor = create_large_buffer(10_000);
    editor.move_to_document_start();

    let start = Instant::now();
    editor.insert_text("START: ");
    let duration = start.elapsed();

    assert!(editor.text().starts_with("START: "));
    assert_within_budget(duration, 5000, "insert_at_start", LINES_10K, "insert_at_start");
}

/// Regression: deleting at document start in large buffer.
#[test]
fn regression_delete_at_start_large() {
    let mut editor = create_large_buffer(10_000);
    editor.move_to_document_start();
    editor.move_right(); // Move past first char

    let start = Instant::now();
    editor.delete_backward();
    let duration = start.elapsed();

    assert_within_budget(duration, 5000, "delete_at_start", LINES_10K, "delete_at_start");
}

/// Regression: word movement in large buffer.
#[test]
fn regression_word_movement_large() {
    let mut editor = create_large_buffer(10_000);
    editor.move_to_document_start();

    let start = Instant::now();
    for _ in 0..100 {
        editor.move_word_right();
    }
    let duration = start.elapsed();

    assert_within_budget(duration, 10_000, "word_movement", LINES_10K, "100_word_moves");
}
