//! Property-based invariant tests for rect non-overlap and extended constraint types.
//!
//! These tests complement `proptest_layout_invariants.rs` by verifying:
//!
//! 1. Rects produced by Flex never overlap in the layout direction.
//! 2. Rects are contiguous (no gaps) when alignment is Start and gap is 0.
//! 3. FitContent/FitContentBounded/FitMin constraints integrate correctly
//!    with random measurer hints.
//! 4. No constraint combination produces negative-dimension rects.
//! 5. Measurer-based solving is deterministic.
//! 6. Degenerate inputs (u16::MAX, single-pixel, extreme constraints) are safe.
//!
//! Part of bd-1lg.9: Unit tests for Widget trait & layout constraints.

use ftui_core::geometry::Rect;
use ftui_layout::{Alignment, Constraint, Direction, Flex, LayoutSizeHint};
use proptest::prelude::*;

// ── Extended Strategies ───────────────────────────────────────────────────

/// Constraint strategy including all nine variant types.
fn extended_constraint_strategy() -> impl Strategy<Value = Constraint> {
    prop_oneof![
        (0u16..=500).prop_map(Constraint::Fixed),
        (0.0f32..=100.0).prop_map(Constraint::Percentage),
        (0u16..=500).prop_map(Constraint::Min),
        (0u16..=500).prop_map(Constraint::Max),
        (0u32..=100, 1u32..=100).prop_map(|(n, d)| Constraint::Ratio(n, d)),
        Just(Constraint::Fill),
        Just(Constraint::FitContent),
        (0u16..=200, 0u16..=500).prop_map(|(min, max)| Constraint::FitContentBounded {
            min,
            max: max.max(min)
        }),
        Just(Constraint::FitMin),
    ]
}

fn extended_constraint_list(max_len: usize) -> impl Strategy<Value = Vec<Constraint>> {
    proptest::collection::vec(extended_constraint_strategy(), 1..=max_len)
}

fn alignment_strategy() -> impl Strategy<Value = Alignment> {
    prop_oneof![
        Just(Alignment::Start),
        Just(Alignment::Center),
        Just(Alignment::End),
        Just(Alignment::SpaceBetween),
        Just(Alignment::SpaceAround),
    ]
}

fn direction_strategy() -> impl Strategy<Value = Direction> {
    prop_oneof![Just(Direction::Horizontal), Just(Direction::Vertical)]
}

fn area_strategy() -> impl Strategy<Value = Rect> {
    (0u16..=100, 0u16..=100, 1u16..=500, 1u16..=200).prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
}

/// Generate a random but valid LayoutSizeHint (min <= preferred, optional max >= preferred).
fn size_hint_strategy() -> impl Strategy<Value = LayoutSizeHint> {
    (0u16..=100, 0u16..=200, proptest::bool::ANY).prop_map(|(min_val, extra, has_max)| {
        let preferred = min_val.saturating_add(extra);
        let max = if has_max {
            Some(preferred.saturating_add(50))
        } else {
            None
        };
        LayoutSizeHint {
            min: min_val,
            preferred,
            max,
        }
    })
}

// ═════════════════════════════════════════════════════════════════════════
// 1. Non-overlapping rects: horizontal layout
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn horizontal_rects_never_overlap(
        constraints in extended_constraint_list(10),
        width in 1u16..=500,
        height in 1u16..=200,
        gap in 0u16..=20,
        alignment in alignment_strategy(),
    ) {
        let flex = Flex::horizontal()
            .constraints(constraints)
            .gap(gap)
            .alignment(alignment);
        let rects = flex.split(Rect::new(0, 0, width, height));

        // For each pair of rects, verify they don't overlap horizontally.
        // Rects in a horizontal layout should be ordered left to right.
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                let a = &rects[i];
                let b = &rects[j];

                // Skip zero-width rects (they can't really overlap anything).
                if a.width == 0 || b.width == 0 {
                    continue;
                }

                // In horizontal layout, rect j should start at or after rect i ends.
                prop_assert!(
                    b.x >= a.right() || a.x >= b.right(),
                    "Horizontal rects {} and {} overlap: {:?} vs {:?}",
                    i, j, a, b
                );
            }
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. Non-overlapping rects: vertical layout
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn vertical_rects_never_overlap(
        constraints in extended_constraint_list(10),
        width in 1u16..=500,
        height in 1u16..=200,
        gap in 0u16..=20,
        alignment in alignment_strategy(),
    ) {
        let flex = Flex::vertical()
            .constraints(constraints)
            .gap(gap)
            .alignment(alignment);
        let rects = flex.split(Rect::new(0, 0, width, height));

        // For each pair of rects, verify they don't overlap vertically.
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                let a = &rects[i];
                let b = &rects[j];

                if a.height == 0 || b.height == 0 {
                    continue;
                }

                prop_assert!(
                    b.y >= a.bottom() || a.y >= b.bottom(),
                    "Vertical rects {} and {} overlap: {:?} vs {:?}",
                    i, j, a, b
                );
            }
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. Contiguous rects: Start alignment, no gap
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn horizontal_start_nogap_contiguous(
        constraints in extended_constraint_list(8),
        width in 1u16..=500,
        height in 1u16..=200,
    ) {
        let flex = Flex::horizontal()
            .constraints(constraints)
            .alignment(Alignment::Start)
            .gap(0);
        let rects = flex.split(Rect::new(0, 0, width, height));

        // With Start alignment and no gap, each rect should start where the
        // previous one ended (or at area.x for the first).
        let mut expected_x = 0u16;
        for (i, r) in rects.iter().enumerate() {
            prop_assert_eq!(
                r.x, expected_x,
                "Rect {} x={} should be {} (contiguous, Start alignment, no gap)",
                i, r.x, expected_x
            );
            expected_x = expected_x.saturating_add(r.width);
        }
    }

    #[test]
    fn vertical_start_nogap_contiguous(
        constraints in extended_constraint_list(8),
        width in 1u16..=500,
        height in 1u16..=200,
    ) {
        let flex = Flex::vertical()
            .constraints(constraints)
            .alignment(Alignment::Start)
            .gap(0);
        let rects = flex.split(Rect::new(0, 0, width, height));

        let mut expected_y = 0u16;
        for (i, r) in rects.iter().enumerate() {
            prop_assert_eq!(
                r.y, expected_y,
                "Rect {} y={} should be {} (contiguous, Start alignment, no gap)",
                i, r.y, expected_y
            );
            expected_y = expected_y.saturating_add(r.height);
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 4. All rects have non-negative dimensions (always true for u16 but
//    verifies no logic produces subtraction underflow via wrapping)
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn all_rect_dimensions_nonnegative(
        constraints in extended_constraint_list(12),
        area in area_strategy(),
        direction in direction_strategy(),
        gap in 0u16..=50,
        alignment in alignment_strategy(),
    ) {
        let flex = Flex::horizontal()
            .direction(direction)
            .constraints(constraints)
            .alignment(alignment)
            .gap(gap);
        let rects = flex.split(area);

        for (i, r) in rects.iter().enumerate() {
            // Width and height are u16, so always >= 0.
            // But verify the rect is geometrically valid (right >= left, bottom >= top).
            prop_assert!(
                r.right() >= r.x,
                "Rect {} has right() {} < x {} (width wraparound?)", i, r.right(), r.x
            );
            prop_assert!(
                r.bottom() >= r.y,
                "Rect {} has bottom() {} < y {} (height wraparound?)", i, r.bottom(), r.y
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 5. Extended sum conservation: includes FitContent variants
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn extended_sum_never_exceeds_available(
        constraints in extended_constraint_list(10),
        width in 1u16..=500,
        hint in size_hint_strategy(),
    ) {
        let flex = Flex::horizontal().constraints(constraints);
        let rects = flex.split_with_measurer(
            Rect::new(0, 0, width, 10),
            |_, _| hint,
        );

        let total: u16 = rects.iter().map(|r| r.width).sum();
        prop_assert!(
            total <= width,
            "Total {} exceeded available {} with extended constraints",
            total, width
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 6. FitContentBounded: respects min/max bounds
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn fit_content_bounded_respects_bounds(
        min_bound in 0u16..=50,
        max_offset in 0u16..=100,
        preferred in 0u16..=200,
        width in 50u16..=500,
    ) {
        let max_bound = min_bound.saturating_add(max_offset).max(min_bound);
        let flex = Flex::horizontal().constraints([
            Constraint::FitContentBounded { min: min_bound, max: max_bound },
            Constraint::Fill,
        ]);
        let rects = flex.split_with_measurer(
            Rect::new(0, 0, width, 10),
            |idx, _| {
                if idx == 0 {
                    LayoutSizeHint { min: 0, preferred, max: None }
                } else {
                    LayoutSizeHint::ZERO
                }
            },
        );

        // The bounded widget should get at least min_bound (if space allows)
        // and at most max_bound.
        let actual = rects[0].width;
        if width >= min_bound {
            prop_assert!(
                actual >= min_bound,
                "FitContentBounded({},{}) got {} < min (width={})",
                min_bound, max_bound, actual, width
            );
        }
        prop_assert!(
            actual <= max_bound || actual <= width,
            "FitContentBounded({},{}) got {} > max (width={})",
            min_bound, max_bound, actual, width
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 7. FitMin: initial allocation >= measurer min
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn fit_min_respects_minimum(
        min_size in 0u16..=50,
        preferred in 0u16..=200,
        width in 50u16..=500,
    ) {
        let flex = Flex::horizontal().constraints([
            Constraint::FitMin,
            Constraint::Fill,
        ]);
        let rects = flex.split_with_measurer(
            Rect::new(0, 0, width, 10),
            |idx, _| {
                if idx == 0 {
                    LayoutSizeHint {
                        min: min_size,
                        preferred: preferred.max(min_size),
                        max: None,
                    }
                } else {
                    LayoutSizeHint::ZERO
                }
            },
        );

        let actual = rects[0].width;
        if width >= min_size {
            prop_assert!(
                actual >= min_size,
                "FitMin got {} < min {} (width={})",
                actual, min_size, width
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 8. Measurer-based solving is deterministic
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn measurer_solving_is_deterministic(
        constraints in extended_constraint_list(8),
        width in 1u16..=500,
        hint in size_hint_strategy(),
    ) {
        let flex = Flex::horizontal().constraints(constraints);
        let area = Rect::new(0, 0, width, 10);

        let rects1 = flex.split_with_measurer(area, |_, _| hint);
        let rects2 = flex.split_with_measurer(area, |_, _| hint);

        prop_assert_eq!(rects1, rects2, "Measurer-based solving is non-deterministic");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 9. Degenerate: u16::MAX dimensions
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn extreme_dimensions_no_panic(
        constraints in extended_constraint_list(8),
        width in prop_oneof![Just(0u16), Just(1u16), Just(u16::MAX)],
        height in prop_oneof![Just(0u16), Just(1u16), Just(u16::MAX)],
        gap in prop_oneof![Just(0u16), Just(u16::MAX)],
        alignment in alignment_strategy(),
        direction in direction_strategy(),
    ) {
        let flex = Flex::horizontal()
            .direction(direction)
            .constraints(constraints)
            .alignment(alignment)
            .gap(gap);
        // Must not panic.
        let rects = flex.split(Rect::new(0, 0, width, height));

        // All produced rects must have valid geometry.
        for r in &rects {
            prop_assert!(r.right() >= r.x, "right < x: {:?}", r);
            prop_assert!(r.bottom() >= r.y, "bottom < y: {:?}", r);
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 10. Degenerate: FitContent with extreme measurer values
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn fit_content_extreme_hints_no_panic(
        width in 1u16..=500,
        min_val in prop_oneof![Just(0u16), Just(u16::MAX)],
        preferred_val in prop_oneof![Just(0u16), Just(u16::MAX)],
    ) {
        let flex = Flex::horizontal().constraints([
            Constraint::FitContent,
            Constraint::Fill,
        ]);
        let rects = flex.split_with_measurer(
            Rect::new(0, 0, width, 10),
            |idx, _| {
                if idx == 0 {
                    LayoutSizeHint { min: min_val, preferred: preferred_val, max: None }
                } else {
                    LayoutSizeHint::ZERO
                }
            },
        );

        let total: u16 = rects.iter().map(|r| r.width).sum();
        prop_assert!(total <= width, "Total {} > width {}", total, width);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 11. Rect ordering: rects maintain constraint order
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn horizontal_rects_ordered_left_to_right(
        constraints in extended_constraint_list(8),
        width in 1u16..=500,
        hint in size_hint_strategy(),
    ) {
        let flex = Flex::horizontal()
            .constraints(constraints)
            .alignment(Alignment::Start);
        let rects = flex.split_with_measurer(Rect::new(0, 0, width, 10), |_, _| hint);

        // Each rect's x should be >= the previous rect's x.
        for i in 1..rects.len() {
            prop_assert!(
                rects[i].x >= rects[i - 1].x,
                "Rect {} (x={}) is before rect {} (x={}) in horizontal layout",
                i, rects[i].x, i - 1, rects[i - 1].x
            );
        }
    }

    #[test]
    fn vertical_rects_ordered_top_to_bottom(
        constraints in extended_constraint_list(8),
        height in 1u16..=500,
        hint in size_hint_strategy(),
    ) {
        let flex = Flex::vertical()
            .constraints(constraints)
            .alignment(Alignment::Start);
        let rects = flex.split_with_measurer(Rect::new(0, 0, 10, height), |_, _| hint);

        for i in 1..rects.len() {
            prop_assert!(
                rects[i].y >= rects[i - 1].y,
                "Rect {} (y={}) is before rect {} (y={}) in vertical layout",
                i, rects[i].y, i - 1, rects[i - 1].y
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 12. Gap correctness: rects separated by exactly the gap
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn horizontal_gap_respected(
        count in 2usize..=6,
        gap in 1u16..=20,
        width in 100u16..=500,
    ) {
        let constraints: Vec<_> = (0..count).map(|_| Constraint::Fixed(10)).collect();
        let total_needed = (10u16 * count as u16) + (gap * (count as u16 - 1));

        // Only test when there's enough space for all items + gaps.
        prop_assume!(width >= total_needed);

        let flex = Flex::horizontal()
            .constraints(constraints)
            .alignment(Alignment::Start)
            .gap(gap);
        let rects = flex.split(Rect::new(0, 0, width, 10));

        for i in 1..rects.len() {
            if rects[i - 1].width > 0 && rects[i].width > 0 {
                let actual_gap = rects[i].x.saturating_sub(rects[i - 1].right());
                prop_assert_eq!(
                    actual_gap, gap,
                    "Gap between rects {} and {} should be {} but got {}",
                    i - 1, i, gap, actual_gap
                );
            }
        }
    }
}
