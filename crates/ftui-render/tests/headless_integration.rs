#![forbid(unsafe_code)]

//! HeadlessTerm integration smoke tests.
//!
//! NOTE: Full widget/layout integration and property tests live in
//! `crates/ftui-harness/tests/render_headless_integration.rs` to avoid
//! publish-time dev-dep cycles (bd-3abcg).

use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, CellAttrs, PackedRgba, StyleFlags};
use ftui_render::diff::BufferDiff;
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

// ============================================================================
// Snapshot-style smoke tests
// ============================================================================

#[test]
fn snapshot_workflow_basic() {
    let prev = Buffer::new(20, 5);
    let mut next = Buffer::new(20, 5);

    for (i, ch) in "Hello".chars().enumerate() {
        next.set(i as u16, 0, Cell::from_char(ch));
    }
    for (i, ch) in "World".chars().enumerate() {
        next.set(i as u16, 2, Cell::from_char(ch));
    }

    let term = present_into_headless(&prev, &next);
    term.assert_matches(&["Hello", "", "World", "", ""]);
}

#[test]
fn snapshot_workflow_incremental_update() {
    let prev1 = Buffer::new(20, 3);
    let mut next1 = Buffer::new(20, 3);
    for (i, ch) in "Frame One".chars().enumerate() {
        next1.set(i as u16, 0, Cell::from_char(ch));
    }

    let mut term = present_into_headless(&prev1, &next1);
    term.assert_row(0, "Frame One");

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
        let mut presenter = Presenter::new(&mut sink, caps);
        presenter.present(&next2, &diff).unwrap();
        drop(presenter);
        sink
    };
    term.process(&output);

    term.assert_row(0, "Frame Two");
    term.assert_row(1, "New Line");
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
fn style_reset_between_cells() {
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
