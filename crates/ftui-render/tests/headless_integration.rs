//! Integration tests for HeadlessTerm (bd-mevj)
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
use ftui_render::presenter::{Presenter, TerminalCapabilities};

// ============================================================================
// Helper: render a buffer through the presenter pipeline into a HeadlessTerm
// ============================================================================

/// Render `next` buffer (diffed against `prev`) through the presenter,
/// feed the ANSI output into a HeadlessTerm, and return it.
fn present_into_headless(prev: &Buffer, next: &Buffer) -> HeadlessTerm {
    let diff = BufferDiff::compute(prev, next);
    let caps = TerminalCapabilities::default();
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
    let left_char = term.model().cell(0, 1).map(|c| c.ch);
    assert!(left_char.is_some(), "left border should have a character");

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
    assert_eq!(cell.ch, 'B');
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
    assert_eq!(cell.ch, 'R');
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
    assert_eq!(cell.ch, 'X');
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
            // Just verifying no panic — also check invariants
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
            // CUP uses 1-indexed parameters
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
            // Verify basic invariants
            let text = term.screen_text();
            prop_assert_eq!(text.len(), 24);
            let (col, row) = term.cursor();
            prop_assert!(col < 80);
            prop_assert!(row < 24);
        }
    }
}
