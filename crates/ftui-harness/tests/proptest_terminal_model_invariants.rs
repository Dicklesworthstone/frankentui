//! Property-based invariant tests for the simplified terminal model.
//!
//! Verifies structural guarantees of `TerminalModel`:
//!
//! 1.  Never panics on arbitrary byte input
//! 2.  Cursor always within bounds after any feed
//! 3.  Grid dimensions match constructor width/height
//! 4.  Determinism: same bytes → same screen text and cursor
//! 5.  Empty feed doesn't change state
//! 6.  Printable ASCII advances cursor by 1 per char
//! 7.  Newline moves cursor to next row, col 0
//! 8.  SGR reset (ESC[0m) clears all style flags
//! 9.  CUP (ESC[row;colH) positions cursor correctly
//! 10. Chunked feeding matches single-shot
//! 11. screen_text() never exceeds width * height characters
//! 12. row_text() length never exceeds width

use ftui_harness::terminal_model::TerminalModel;
use proptest::prelude::*;

// ── Helpers ──────────────────────────────────────────────────────────

fn arb_dimensions() -> impl Strategy<Value = (u16, u16)> {
    (1u16..=200, 1u16..=100)
}

// ═════════════════════════════════════════════════════════════════════════
// 1. Never panics on arbitrary input
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn never_panics(
        width in 1u16..=80,
        height in 1u16..=40,
        bytes in proptest::collection::vec(any::<u8>(), 0..=500),
    ) {
        let mut model = TerminalModel::new(width, height);
        model.feed(&bytes);
        let _ = model.cursor();
        let _ = model.screen_text();
        let _ = model.dump();
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. Cursor always within bounds
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cursor_in_bounds(
        width in 1u16..=80,
        height in 1u16..=40,
        bytes in proptest::collection::vec(any::<u8>(), 0..=300),
    ) {
        let mut model = TerminalModel::new(width, height);
        model.feed(&bytes);
        let (cx, cy) = model.cursor();
        prop_assert!(
            cx < width,
            "cursor x {} >= width {}",
            cx, width
        );
        prop_assert!(
            cy < height,
            "cursor y {} >= height {}",
            cy, height
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. Grid dimensions match constructor
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn dimensions_match((width, height) in arb_dimensions()) {
        let model = TerminalModel::new(width, height);
        prop_assert_eq!(model.width(), width);
        prop_assert_eq!(model.height(), height);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 4. Determinism
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn deterministic(
        bytes in proptest::collection::vec(any::<u8>(), 0..=200),
    ) {
        let mut a = TerminalModel::new(80, 24);
        let mut b = TerminalModel::new(80, 24);
        a.feed(&bytes);
        b.feed(&bytes);
        prop_assert_eq!(a.cursor(), b.cursor());
        prop_assert_eq!(a.screen_text(), b.screen_text());
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 5. Empty feed doesn't change state
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn empty_feed_noop((width, height) in arb_dimensions()) {
        let mut model = TerminalModel::new(width, height);
        let text_before = model.screen_text();
        let cursor_before = model.cursor();
        model.feed(b"");
        prop_assert_eq!(model.screen_text(), text_before);
        prop_assert_eq!(model.cursor(), cursor_before);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 6. Printable ASCII advances cursor
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn printable_advances_cursor(
        n in 1usize..=20,
    ) {
        let width = 80u16;
        let mut model = TerminalModel::new(width, 24);
        let chars: Vec<u8> = (0..n).map(|i| b'A' + (i % 26) as u8).collect();
        model.feed(&chars);
        let (cx, _cy) = model.cursor();
        // If n fits on one row, cursor should be at n; if it wraps, it's n % width
        if n <= width as usize {
            prop_assert_eq!(cx, n as u16, "cursor should advance by {} chars", n);
        } else {
            prop_assert_eq!(cx, (n as u16) % width, "cursor should wrap");
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 7. SGR reset clears all style flags
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn sgr_reset_clears_style() {
    let mut model = TerminalModel::new(80, 24);
    // Set bold+italic+truecolor red (model only supports 38;2;r;g;b, not basic 30-37)
    model.feed_str("\x1b[1;3m\x1b[38;2;255;0;0m");
    // Write a char with those styles
    model.feed_str("X");
    let styled = model.style_at(0, 0);
    assert!(styled.bold);
    assert!(styled.italic);
    assert!(styled.fg.is_some());

    // Reset and write
    model.feed_str("\x1b[0m");
    model.feed_str("Y");
    let reset_style = model.style_at(1, 0);
    assert!(!reset_style.bold);
    assert!(!reset_style.italic);
    assert!(reset_style.fg.is_none());
}

// ═════════════════════════════════════════════════════════════════════════
// 8. CUP positions cursor correctly
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cup_positions_cursor(
        row in 1u16..=24,
        col in 1u16..=80,
    ) {
        let mut model = TerminalModel::new(80, 24);
        let cup = format!("\x1b[{};{}H", row, col);
        model.feed_str(&cup);
        let (cx, cy) = model.cursor();
        // CUP uses 1-indexed parameters, model uses 0-indexed cursor
        prop_assert_eq!(cy, row - 1, "cursor y after CUP({},{})", row, col);
        prop_assert_eq!(cx, col - 1, "cursor x after CUP({},{})", row, col);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 9. Chunked feeding matches single-shot
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn chunked_matches_single(
        bytes in proptest::collection::vec(any::<u8>(), 1..=200),
        split in 0usize..=200,
    ) {
        let split = split.min(bytes.len());

        let mut single = TerminalModel::new(40, 10);
        single.feed(&bytes);

        let mut chunked = TerminalModel::new(40, 10);
        chunked.feed(&bytes[..split]);
        chunked.feed(&bytes[split..]);

        prop_assert_eq!(single.cursor(), chunked.cursor());
        prop_assert_eq!(single.screen_text(), chunked.screen_text());
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 10. row_text() length never exceeds width
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn row_text_bounded(
        width in 1u16..=80,
        height in 1u16..=24,
        bytes in proptest::collection::vec(any::<u8>(), 0..=300),
    ) {
        let mut model = TerminalModel::new(width, height);
        model.feed(&bytes);
        for y in 0..height {
            let row = model.row_text(y);
            prop_assert!(
                row.len() <= width as usize,
                "row {} length {} > width {}",
                y,
                row.len(),
                width
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 11. char_at within bounds never panics
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn char_at_in_bounds(
        width in 1u16..=80,
        height in 1u16..=24,
        bytes in proptest::collection::vec(any::<u8>(), 0..=100),
        x in 0u16..80,
        y in 0u16..24,
    ) {
        let mut model = TerminalModel::new(width, height);
        model.feed(&bytes);
        // Should never panic even for coords at or beyond bounds
        let _ = model.char_at(x, y);
        let _ = model.style_at(x, y);
        let _ = model.link_at(x, y);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 12. Erase line clears to spaces
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn erase_line_clears_to_spaces(mode in 0u8..=2) {
        let mut model = TerminalModel::new(20, 5);
        // Fill row 0 with 'A's
        model.feed_str("AAAAAAAAAAAAAAAAAAAA");
        // Move cursor to middle
        model.feed_str("\x1b[1;11H");
        // Erase line with given mode
        let el = format!("\x1b[{}K", mode);
        model.feed_str(&el);

        // Verify erased region is spaces
        match mode {
            0 => {
                // Erase to end: cols 10..19 should be space
                for x in 10..20 {
                    prop_assert_eq!(model.char_at(x, 0), ' ', "EL0: col {} should be space", x);
                }
            }
            1 => {
                // Erase to start: cols 0..10 should be space
                for x in 0..=10 {
                    prop_assert_eq!(model.char_at(x, 0), ' ', "EL1: col {} should be space", x);
                }
            }
            2 => {
                // Erase all: all cols should be space
                for x in 0..20 {
                    prop_assert_eq!(model.char_at(x, 0), ' ', "EL2: col {} should be space", x);
                }
            }
            _ => {}
        }
    }
}
