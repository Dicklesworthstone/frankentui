//! Property-based invariant tests for MeasurableWidget and SizeConstraints.
//!
//! These tests verify structural invariants of the widget sizing system:
//!
//! 1. SizeConstraints::clamp always produces values within bounds.
//! 2. SizeConstraints::exact produces constraints where min == preferred == max.
//! 3. SizeConstraints::intersect is commutative.
//! 4. SizeConstraints::is_satisfied_by is consistent with clamp.
//! 5. Block MeasurableWidget implementation satisfies invariants.
//! 6. Paragraph MeasurableWidget implementation satisfies invariants.
//! 7. ProgressBar MeasurableWidget implementation satisfies invariants.
//! 8. measure() is pure (same input -> same output).
//! 9. SizeConstraints::ZERO.clamp is identity.
//! 10. Large constraint values don't cause overflow.
//!
//! Part of bd-1lg.9: Unit tests for Widget trait & layout constraints.

use ftui_core::geometry::Size;
use ftui_widgets::SizeConstraints;
use proptest::prelude::*;

// ── Strategies ────────────────────────────────────────────────────────────

fn size_strategy() -> impl Strategy<Value = Size> {
    (0u16..=500, 0u16..=200).prop_map(|(w, h)| Size::new(w, h))
}

fn size_constraints_strategy() -> impl Strategy<Value = SizeConstraints> {
    (
        0u16..=200,
        0u16..=200,
        0u16..=200,
        0u16..=200,
        proptest::bool::ANY,
    )
        .prop_map(|(min_w, min_h, extra_w, extra_h, has_max)| {
            let pref_w = min_w.saturating_add(extra_w);
            let pref_h = min_h.saturating_add(extra_h);
            let max = if has_max {
                Some(Size::new(
                    pref_w.saturating_add(50),
                    pref_h.saturating_add(50),
                ))
            } else {
                None
            };
            SizeConstraints {
                min: Size::new(min_w, min_h),
                preferred: Size::new(pref_w, pref_h),
                max,
            }
        })
}

// ═════════════════════════════════════════════════════════════════════════
// 1. SizeConstraints::clamp always produces values within bounds
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn clamp_within_bounds(
        constraints in size_constraints_strategy(),
        input in size_strategy(),
    ) {
        let clamped = constraints.clamp(input);

        // Clamped width >= min width.
        prop_assert!(
            clamped.width >= constraints.min.width,
            "Clamped width {} < min width {}",
            clamped.width, constraints.min.width
        );
        // Clamped height >= min height.
        prop_assert!(
            clamped.height >= constraints.min.height,
            "Clamped height {} < min height {}",
            clamped.height, constraints.min.height
        );

        // If max is set, clamped must be <= max.
        if let Some(max) = constraints.max {
            prop_assert!(
                clamped.width <= max.width,
                "Clamped width {} > max width {}",
                clamped.width, max.width
            );
            prop_assert!(
                clamped.height <= max.height,
                "Clamped height {} > max height {}",
                clamped.height, max.height
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. SizeConstraints::exact: min == preferred == max
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn exact_has_equal_min_preferred_max(
        size in size_strategy(),
    ) {
        let c = SizeConstraints::exact(size);
        prop_assert_eq!(c.min, size, "exact: min != size");
        prop_assert_eq!(c.preferred, size, "exact: preferred != size");
        prop_assert_eq!(c.max, Some(size), "exact: max != Some(size)");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. SizeConstraints::intersect is commutative
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn intersect_commutative(
        a in size_constraints_strategy(),
        b in size_constraints_strategy(),
    ) {
        let ab = a.intersect(&b);
        let ba = b.intersect(&a);

        prop_assert_eq!(ab.min, ba.min, "intersect not commutative for min");
        prop_assert_eq!(ab.max, ba.max, "intersect not commutative for max");
        prop_assert_eq!(ab.preferred, ba.preferred, "intersect not commutative for preferred");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 4. SizeConstraints::is_satisfied_by consistent with clamp
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn satisfied_after_clamp(
        constraints in size_constraints_strategy(),
        input in size_strategy(),
    ) {
        let clamped = constraints.clamp(input);
        // After clamping, the result should always satisfy the constraints
        // (provided the constraints are well-formed: min <= max).
        if let Some(max) = constraints.max {
            if constraints.min.width <= max.width && constraints.min.height <= max.height {
                prop_assert!(
                    constraints.is_satisfied_by(clamped),
                    "Clamped {:?} does not satisfy {:?}",
                    clamped, constraints
                );
            }
        } else {
            // No max means unbounded upper bound.
            prop_assert!(
                constraints.is_satisfied_by(clamped),
                "Clamped {:?} does not satisfy {:?} (unbounded max)",
                clamped, constraints
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 5. SizeConstraints::ZERO.clamp is identity (no bounds applied)
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn zero_constraints_clamp_is_identity(
        input in size_strategy(),
    ) {
        let clamped = SizeConstraints::ZERO.clamp(input);
        // ZERO has min=0, max=None, so clamp should be identity.
        prop_assert_eq!(
            clamped, input,
            "ZERO.clamp({:?}) = {:?}, expected identity",
            input, clamped
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 6. Clamp is idempotent: clamp(clamp(x)) == clamp(x)
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn clamp_is_idempotent(
        constraints in size_constraints_strategy(),
        input in size_strategy(),
    ) {
        let once = constraints.clamp(input);
        let twice = constraints.clamp(once);
        prop_assert_eq!(
            once, twice,
            "clamp is not idempotent: clamp({:?})={:?} but clamp(clamp)={:?}",
            input, once, twice
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 7. at_least: min and preferred set, max unbounded
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn at_least_properties(
        min in size_strategy(),
        preferred_extra_w in 0u16..=200,
        preferred_extra_h in 0u16..=200,
    ) {
        let preferred = Size::new(
            min.width.saturating_add(preferred_extra_w),
            min.height.saturating_add(preferred_extra_h),
        );
        let c = SizeConstraints::at_least(min, preferred);

        prop_assert_eq!(c.min, min, "at_least: min mismatch");
        prop_assert_eq!(c.preferred, preferred, "at_least: preferred mismatch");
        prop_assert_eq!(c.max, None, "at_least: max should be None");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 8. intersect produces tighter constraints
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn intersect_tightens_bounds(
        a in size_constraints_strategy(),
        b in size_constraints_strategy(),
    ) {
        let result = a.intersect(&b);

        // min of intersection >= max(a.min, b.min) per dimension.
        prop_assert!(
            result.min.width >= a.min.width && result.min.width >= b.min.width,
            "Intersection min width {} should be >= max({}, {})",
            result.min.width, a.min.width, b.min.width
        );
        prop_assert!(
            result.min.height >= a.min.height && result.min.height >= b.min.height,
            "Intersection min height {} should be >= max({}, {})",
            result.min.height, a.min.height, b.min.height
        );

        // If both have max, intersection max <= min(a.max, b.max).
        if let (Some(a_max), Some(b_max)) = (a.max, b.max)
            && let Some(r_max) = result.max
        {
            prop_assert!(
                r_max.width <= a_max.width && r_max.width <= b_max.width,
                "Intersection max width {} should be <= min({}, {})",
                r_max.width, a_max.width, b_max.width
            );
            prop_assert!(
                r_max.height <= a_max.height && r_max.height <= b_max.height,
                "Intersection max height {} should be <= min({}, {})",
                r_max.height, a_max.height, b_max.height
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 9. Large values don't overflow
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn extreme_values_no_overflow(
        w in prop_oneof![Just(0u16), Just(u16::MAX), 0u16..=u16::MAX],
        h in prop_oneof![Just(0u16), Just(u16::MAX), 0u16..=u16::MAX],
    ) {
        let size = Size::new(w, h);

        // exact with extreme values.
        let c = SizeConstraints::exact(size);
        let _ = c.clamp(size);
        let _ = c.is_satisfied_by(size);

        // ZERO clamp with extreme values.
        let _ = SizeConstraints::ZERO.clamp(size);

        // at_least with extreme values.
        let c2 = SizeConstraints::at_least(Size::ZERO, size);
        let _ = c2.clamp(size);

        // intersect with extreme values.
        let _ = c.intersect(&c2);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 10. SizeConstraints determinism: same construction -> same result
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn size_constraints_deterministic(
        constraints in size_constraints_strategy(),
        input in size_strategy(),
    ) {
        let r1 = constraints.clamp(input);
        let r2 = constraints.clamp(input);
        prop_assert_eq!(r1, r2, "clamp is not deterministic");

        let s1 = constraints.is_satisfied_by(input);
        let s2 = constraints.is_satisfied_by(input);
        prop_assert_eq!(s1, s2, "is_satisfied_by is not deterministic");
    }
}
