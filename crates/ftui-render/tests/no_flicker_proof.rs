#![forbid(unsafe_code)]

//! Render correctness smoke tests.
//!
//! NOTE: Extensive flicker/ghosting proofs moved to
//! `crates/ftui-harness/tests/render_no_flicker_proof.rs` to avoid
//! the ftui-render ↔ ftui-harness dev-dep cycle (bd-3abcg).

use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::diff::BufferDiff;
use ftui_render::presenter::{Presenter, TerminalCapabilities};
use ftui_render::render_certificate::evaluate_render_certificate_for_buffers;

#[test]
fn diff_is_empty_for_identical_buffers() {
    let buffer = Buffer::new(12, 4);
    let diff = BufferDiff::compute(&buffer, &buffer);
    assert!(
        diff.is_empty(),
        "diff should be empty for identical buffers"
    );
}

#[test]
fn presenter_handles_colored_cells() {
    let mut buffer = Buffer::new(8, 2);
    buffer.set_raw(
        0,
        0,
        Cell::from_char('C').with_fg(PackedRgba::rgb(200, 50, 50)),
    );
    let blank = Buffer::new(8, 2);
    let diff = BufferDiff::compute(&blank, &buffer);

    let mut sink = Vec::new();
    let mut presenter = Presenter::new(&mut sink, TerminalCapabilities::basic());
    presenter.present(&buffer, &diff).expect("presenter failed");
    drop(presenter);

    assert!(!sink.is_empty(), "presenter should emit bytes");
    assert!(
        sink.contains(&b'C'),
        "rendered content should appear in output"
    );
}

#[test]
fn certified_and_dirty_diff_match_full_diff_and_wrap_sync_brackets() {
    let mut old = Buffer::new(16, 6);
    old.set_raw(2, 1, Cell::from_char('A'));
    old.clear_dirty();

    let mut new = old.clone();
    new.set(
        5,
        3,
        Cell::from_char('X').with_fg(PackedRgba::rgb(10, 200, 40)),
    );
    new.set(6, 3, Cell::from_char('Y'));
    assert!(new.is_row_dirty(3));
    assert!(!new.is_row_dirty(0));

    let full = BufferDiff::compute(&old, &new);
    let dirty = BufferDiff::compute_dirty(&old, &new);
    let cert = evaluate_render_certificate_for_buffers(Some(&old), &new, false);
    assert!(!cert.fell_back);
    let mut certified = BufferDiff::new();
    certified.compute_certified_into(&old, &new, cert.into_hint());

    assert_eq!(dirty.changes(), full.changes());
    assert_eq!(certified.changes(), full.changes());
    assert_eq!(
        certified.last_scanned_cells(),
        new.dirty_span_stats().span_coverage_cells
    );

    let mut sink = Vec::new();
    let caps = TerminalCapabilities::builder().sync_output(true).build();
    let mut presenter = Presenter::new(&mut sink, caps);
    presenter
        .present(&new, &certified)
        .expect("presenter with sync_output should succeed");
    drop(presenter);

    assert!(
        sink.starts_with(b"\x1b[?2026h"),
        "sync_output must open DEC 2026 bracket"
    );
    assert!(
        sink.ends_with(b"\x1b[?2026l"),
        "sync_output must close DEC 2026 bracket"
    );
}
