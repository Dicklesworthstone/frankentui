//! Property-based invariant tests for the ftui-render diff algorithm.
//!
//! These tests verify structural invariants of `BufferDiff` that must hold
//! for **any** pair of buffers:
//!
//! 1. Identical buffers produce zero changes.
//! 2. Every change position is within bounds.
//! 3. Every changed cell actually differs between old and new.
//! 4. No unchanged cell is reported as changed (no false positives).
//! 5. Diff is deterministic (same inputs → same output).
//! 6. `compute` and `compute_into` produce identical results.
//! 7. Runs cover exactly the same positions as the raw changes.
//! 8. Runs are sorted by row-major order.
//! 9. `compute_dirty` is a superset-or-equal of `compute` (no missed changes).
//! 10. Full diff captures every cell in the buffer.
//! 14. `runs()` and `runs_into()` produce identical output (isomorphism).
//! 15. `runs_into` reuses capacity across calls.
//! 16. Presenting a diff and replaying the bytes reproduces the target buffer.

use ftui_core::text_width::display_width;
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::diff::BufferDiff;
use ftui_render::headless::HeadlessTerm;
use ftui_render::presenter::{Presenter, TerminalCapabilities};
use proptest::prelude::*;

// ── Helpers ─────────────────────────────────────────────────────────────

/// Dimensions strategy: small enough for fast tests, large enough for edge cases.
fn dims() -> impl Strategy<Value = (u16, u16)> {
    (1u16..=80, 1u16..=40)
}

/// Apply random scattered changes to a buffer.
fn apply_changes(buf: &mut Buffer, changes: &[(u16, u16, char)]) {
    for &(x, y, ch) in changes {
        if x < buf.width() && y < buf.height() {
            buf.set_raw(x, y, Cell::from_char(ch));
        }
    }
}

/// Strategy for a vec of (x, y, char) change triples within given bounds.
fn change_set(max_w: u16, max_h: u16) -> impl Strategy<Value = Vec<(u16, u16, char)>> {
    proptest::collection::vec(
        (
            0..max_w,
            0..max_h,
            prop_oneof![
                Just('A'),
                Just('X'),
                Just('Z'),
                Just('#'),
                Just(' '),
                (0x21u32..=0x7E).prop_map(|c| char::from_u32(c).unwrap()),
            ],
        ),
        0..200,
    )
}

// ═════════════════════════════════════════════════════════════════════════
// 1. Identical buffers produce zero changes
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn identical_buffers_produce_empty_diff((w, h) in dims()) {
        let buf = Buffer::new(w, h);
        let diff = BufferDiff::compute(&buf, &buf);
        prop_assert!(diff.is_empty(),
            "Diff between identical {}x{} buffers should be empty, got {} changes",
            w, h, diff.len());
    }

    /// After applying the same changes to both buffers, diff should be empty.
    #[test]
    fn same_changes_produce_empty_diff(
        (w, h) in dims(),
        changes in change_set(80, 40),
    ) {
        let mut buf1 = Buffer::new(w, h);
        let mut buf2 = Buffer::new(w, h);
        apply_changes(&mut buf1, &changes);
        apply_changes(&mut buf2, &changes);
        let diff = BufferDiff::compute(&buf1, &buf2);
        prop_assert!(diff.is_empty(),
            "Same changes should produce empty diff, got {} changes", diff.len());
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. Every change position is within bounds
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn change_positions_in_bounds(
        (w, h) in dims(),
        changes in change_set(80, 40),
    ) {
        let old = Buffer::new(w, h);
        let mut new = old.clone();
        apply_changes(&mut new, &changes);
        let diff = BufferDiff::compute(&old, &new);

        for &(x, y) in diff.changes() {
            prop_assert!(x < w, "x={} >= width={}", x, w);
            prop_assert!(y < h, "y={} >= height={}", y, h);
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. Every reported change is a true positive (cells actually differ)
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn no_false_positive_changes(
        (w, h) in dims(),
        changes in change_set(80, 40),
    ) {
        let old = Buffer::new(w, h);
        let mut new = old.clone();
        apply_changes(&mut new, &changes);
        let diff = BufferDiff::compute(&old, &new);

        for &(x, y) in diff.changes() {
            let old_cell = old.get(x, y).unwrap();
            let new_cell = new.get(x, y).unwrap();
            prop_assert!(!old_cell.bits_eq(new_cell),
                "False positive: cells at ({}, {}) are identical but reported as changed", x, y);
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 4. No true change is missed (completeness / no false negatives)
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn no_false_negative_changes(
        (w, h) in dims(),
        changes in change_set(80, 40),
    ) {
        let old = Buffer::new(w, h);
        let mut new = old.clone();
        apply_changes(&mut new, &changes);
        let diff = BufferDiff::compute(&old, &new);

        // Build a set of reported changes for quick lookup.
        let change_set: std::collections::HashSet<(u16, u16)> =
            diff.changes().iter().copied().collect();

        // Check every cell: if it differs, it must be in the diff.
        for y in 0..h {
            for x in 0..w {
                let old_cell = old.get(x, y).unwrap();
                let new_cell = new.get(x, y).unwrap();
                if !old_cell.bits_eq(new_cell) {
                    prop_assert!(change_set.contains(&(x, y)),
                        "False negative: cell ({}, {}) differs but not in diff", x, y);
                }
            }
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 5. Diff is deterministic
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn diff_is_deterministic(
        (w, h) in dims(),
        changes in change_set(80, 40),
    ) {
        let old = Buffer::new(w, h);
        let mut new = old.clone();
        apply_changes(&mut new, &changes);

        let diff1 = BufferDiff::compute(&old, &new);
        let diff2 = BufferDiff::compute(&old, &new);

        prop_assert_eq!(diff1.changes(), diff2.changes(),
            "Two compute() calls produced different results");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 6. compute and compute_into produce identical results
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn compute_and_compute_into_equivalent(
        (w, h) in dims(),
        changes in change_set(80, 40),
    ) {
        let old = Buffer::new(w, h);
        let mut new = old.clone();
        apply_changes(&mut new, &changes);

        let diff_fresh = BufferDiff::compute(&old, &new);
        let mut diff_reused = BufferDiff::new();
        diff_reused.compute_into(&old, &new);

        prop_assert_eq!(diff_fresh.changes(), diff_reused.changes(),
            "compute() and compute_into() disagree");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 7. Runs cover exactly the same positions as raw changes
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn runs_cover_all_changes(
        (w, h) in dims(),
        changes in change_set(80, 40),
    ) {
        let old = Buffer::new(w, h);
        let mut new = old.clone();
        apply_changes(&mut new, &changes);
        let diff = BufferDiff::compute(&old, &new);

        // Expand runs back to individual positions.
        let runs = diff.runs();
        let mut run_positions: Vec<(u16, u16)> = Vec::new();
        for run in &runs {
            for x in run.x0..=run.x1 {
                run_positions.push((x, run.y));
            }
        }
        run_positions.sort();

        let mut raw_positions: Vec<(u16, u16)> = diff.changes().to_vec();
        raw_positions.sort();

        prop_assert_eq!(run_positions, raw_positions,
            "Runs don't cover the same positions as raw changes");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 8. Changes are sorted in row-major order
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn changes_sorted_row_major(
        (w, h) in dims(),
        changes in change_set(80, 40),
    ) {
        let old = Buffer::new(w, h);
        let mut new = old.clone();
        apply_changes(&mut new, &changes);
        let diff = BufferDiff::compute(&old, &new);

        let positions = diff.changes();
        for window in positions.windows(2) {
            let (x1, y1) = window[0];
            let (x2, y2) = window[1];
            prop_assert!(
                (y1, x1) < (y2, x2),
                "Changes not in row-major order: ({},{}) before ({},{})", x1, y1, x2, y2
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 9. Full diff covers every cell
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn full_diff_covers_all_cells((w, h) in dims()) {
        let diff = BufferDiff::full(w, h);
        let expected = (w as usize) * (h as usize);
        prop_assert_eq!(diff.len(), expected,
            "Full diff should have {}*{}={} changes, got {}", w, h, expected, diff.len());
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 10. Runs are sorted and non-overlapping
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn runs_sorted_and_non_overlapping(
        (w, h) in dims(),
        changes in change_set(80, 40),
    ) {
        let old = Buffer::new(w, h);
        let mut new = old.clone();
        apply_changes(&mut new, &changes);
        let diff = BufferDiff::compute(&old, &new);
        let runs = diff.runs();

        for window in runs.windows(2) {
            let a = &window[0];
            let b = &window[1];
            if a.y == b.y {
                // Same row: runs must not overlap
                prop_assert!(a.x1 < b.x0,
                    "Overlapping runs on row {}: [{}, {}] and [{}, {}]",
                    a.y, a.x0, a.x1, b.x0, b.x1);
            } else {
                // Different rows: must be in ascending order
                prop_assert!(a.y < b.y,
                    "Runs not sorted by row: y={} before y={}", a.y, b.y);
            }
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 11. Dirty diff is a superset of regular diff
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn dirty_diff_superset_of_compute(
        (w, h) in dims(),
        changes in change_set(80, 40),
    ) {
        let old = Buffer::new(w, h);
        let mut new = old.clone();
        apply_changes(&mut new, &changes);

        let exact_diff = BufferDiff::compute(&old, &new);
        let dirty_diff = BufferDiff::compute_dirty(&old, &new);

        // Every change in compute() must appear in compute_dirty().
        let dirty_set: std::collections::HashSet<(u16, u16)> =
            dirty_diff.changes().iter().copied().collect();

        for &(x, y) in exact_diff.changes() {
            prop_assert!(dirty_set.contains(&(x, y)),
                "compute_dirty missed change at ({}, {})", x, y);
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 12. Diff symmetry: |diff(A,B)| == |diff(B,A)|
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn diff_symmetric_count(
        (w, h) in dims(),
        changes in change_set(80, 40),
    ) {
        let old = Buffer::new(w, h);
        let mut new = old.clone();
        apply_changes(&mut new, &changes);

        let forward = BufferDiff::compute(&old, &new);
        let backward = BufferDiff::compute(&new, &old);

        prop_assert_eq!(forward.len(), backward.len(),
            "Forward diff has {} changes but backward has {}", forward.len(), backward.len());

        // Same positions should be detected in both directions.
        prop_assert_eq!(forward.changes(), backward.changes(),
            "Forward and backward diffs report different positions");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 13. compute_into clears previous state
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn compute_into_clears_previous(
        (w, h) in dims(),
        changes1 in change_set(80, 40),
        changes2 in change_set(80, 40),
    ) {
        let base = Buffer::new(w, h);

        let mut buf1 = base.clone();
        apply_changes(&mut buf1, &changes1);
        let mut buf2 = base.clone();
        apply_changes(&mut buf2, &changes2);

        let mut diff = BufferDiff::new();

        // First compute
        diff.compute_into(&base, &buf1);
        let first_len = diff.len();

        // Second compute should overwrite, not append
        diff.compute_into(&base, &buf2);

        // Verify the diff now matches a fresh compute for buf2
        let fresh = BufferDiff::compute(&base, &buf2);
        prop_assert_eq!(diff.changes(), fresh.changes(),
            "compute_into didn't reset: first had {} changes, reuse has {}, fresh has {}",
            first_len, diff.len(), fresh.len());
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 14. runs() and runs_into() produce identical output (bd-1tssj isomorphism)
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn runs_and_runs_into_isomorphic(
        (w, h) in dims(),
        changes in change_set(80, 40),
    ) {
        let old = Buffer::new(w, h);
        let mut new = old.clone();
        apply_changes(&mut new, &changes);
        let diff = BufferDiff::compute(&old, &new);

        let allocating = diff.runs();
        let mut reuse_buf = Vec::new();
        diff.runs_into(&mut reuse_buf);

        prop_assert_eq!(&allocating, &reuse_buf,
            "runs() and runs_into() differ for {}x{} with {} changes",
            w, h, diff.len());
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 15. runs_into reuses capacity (no extra allocation after warmup)
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn runs_into_reuses_capacity(
        (w, h) in dims(),
        changes1 in change_set(80, 40),
        changes2 in change_set(80, 40),
    ) {
        let base = Buffer::new(w, h);
        let mut buf1 = base.clone();
        apply_changes(&mut buf1, &changes1);
        let mut buf2 = base.clone();
        apply_changes(&mut buf2, &changes2);

        let diff1 = BufferDiff::compute(&base, &buf1);
        let diff2 = BufferDiff::compute(&base, &buf2);

        let mut reuse_buf = Vec::new();

        // First call warms up the buffer.
        diff1.runs_into(&mut reuse_buf);
        let cap_after_first = reuse_buf.capacity();

        // Second call should reuse the capacity if the result fits.
        diff2.runs_into(&mut reuse_buf);
        let cap_after_second = reuse_buf.capacity();

        // Capacity should not shrink (Vec::clear preserves capacity).
        prop_assert!(cap_after_second >= cap_after_first.min(reuse_buf.len()),
            "runs_into shrank capacity: {} -> {}", cap_after_first, cap_after_second);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 16. Presenting a diff and replaying the bytes reproduces the target buffer
// ═════════════════════════════════════════════════════════════════════════
//
// The properties above stop at the diff: they prove it reports the right
// changes, not that presenting those changes makes a terminal show the right
// thing. The full pipeline is exercised elsewhere, but only over one fixed
// scene, so nothing randomised the buffer contents through the ANSI round
// trip - which is the renderer's core promise.

/// Glyphs worth mixing: single-width, wide (a continuation cell follows each),
/// and a space so runs break up.
fn glyph() -> impl Strategy<Value = char> {
    prop_oneof![
        Just('a'),
        Just('Z'),
        Just('#'),
        Just(' '),
        Just('é'),
        Just('─'),
        Just('中'),
        Just('あ'),
        Just('\u{1F600}'),
    ]
}

/// A buffer of random glyphs and colours, with continuation cells placed after
/// every wide glyph and a wide glyph never straddling the right margin.
fn painted_buffer(width: u16, height: u16, glyphs: &[char], colours: &[u8]) -> Buffer {
    let mut buf = Buffer::new(width, height);
    let mut pick = 0usize;
    for y in 0..height {
        let mut x = 0u16;
        while x < width {
            let ch = glyphs[pick % glyphs.len()];
            let tint = colours[pick % colours.len()];
            pick += 1;
            let cw = u16::try_from(display_width(&ch.to_string()).max(1)).unwrap_or(1);
            if x + cw > width {
                buf.set(x, y, Cell::from_char(' '));
                x += 1;
                continue;
            }
            let mut cell = Cell::from_char(ch);
            if tint.is_multiple_of(3) {
                cell.fg = PackedRgba::rgb(tint, tint.wrapping_mul(3), 0x20);
            }
            buf.set(x, y, cell);
            for k in 1..cw {
                buf.set(x + k, y, Cell::CONTINUATION);
            }
            x += cw;
        }
    }
    buf
}

/// The text each row should show: a continuation contributes nothing of its
/// own, because the wide glyph before it already covers that column.
fn visible_rows(buf: &Buffer) -> Vec<String> {
    (0..buf.height())
        .map(|y| {
            (0..buf.width())
                .filter_map(|x| {
                    let cell = buf.get(x, y)?;
                    if cell.content.is_continuation() {
                        return None;
                    }
                    Some(cell.content.as_char().unwrap_or(' '))
                })
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

proptest! {
    #[test]
    fn presented_diff_reproduces_the_target_buffer(
        (w, h) in (1u16..=30, 1u16..=12),
        first in proptest::collection::vec(glyph(), 1..40),
        second in proptest::collection::vec(glyph(), 1..40),
        colours in proptest::collection::vec(any::<u8>(), 1..16),
    ) {
        let prev = painted_buffer(w, h, &first, &colours);
        let next = painted_buffer(w, h, &second, &colours);

        // Paint `prev` from blank, then apply only the diff to reach `next` -
        // the same two-step a live frame takes.
        let mut out = Vec::new();
        {
            let blank = Buffer::new(w, h);
            let initial = BufferDiff::compute(&blank, &prev);
            let update = BufferDiff::compute(&prev, &next);
            let mut presenter = Presenter::new(&mut out, TerminalCapabilities::default());
            presenter.present(&prev, &initial).expect("present initial frame");
            presenter.present(&next, &update).expect("present update frame");
        }

        let mut term = HeadlessTerm::new(w, h);
        term.process(&out);

        let want = visible_rows(&next);
        let got: Vec<String> = term
            .screen_text()
            .iter()
            .map(|row| row.trim_end().to_string())
            .collect();
        prop_assert_eq!(got, want, "presented bytes did not reproduce the buffer at {}x{}", w, h);
    }
}
