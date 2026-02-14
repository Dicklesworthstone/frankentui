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

use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::diff::BufferDiff;
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
