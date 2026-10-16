#![forbid(unsafe_code)]

//! SOS barrier certificate evaluator for frame-budget admissibility.
//!
//! Evaluates a pre-computed polynomial barrier certificate B(x1, x2) to
//! determine whether the current frame-budget state is admissible (safe)
//! or has crossed into the degradation region.
//!
//! The barrier polynomial coefficients in `sos_barrier_coeffs.rs` are
//! hand-chosen to satisfy 8 verification points across `[0, 1]^2` (not
//! solver-generated; no `scripts/solve_sos_barrier.py` exists in this
//! repository). The Rust code here is the degree-4 polynomial evaluator.
//!
//! # State Space
//!
//! - `x1` = `budget_remaining`: fraction of frame budget remaining, \[0, 1\]
//! - `x2` = `change_rate`: normalized rate of cell changes per frame, \[0, 1\]
//!
//! # Barrier Properties
//!
//! - `B(x) > 0` → safe region (budget available, manageable load)
//! - `B(x) <= 0` → at or beyond unsafe boundary (trigger degradation)

// Hand-chosen degree-4 barrier polynomial coefficients (see provenance note in sos_barrier_coeffs.rs).
include!("sos_barrier_coeffs.rs");

/// Result of evaluating the barrier certificate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarrierResult {
    /// The barrier function value B(x1, x2).
    pub value: f64,
    /// Whether the state is in the safe region (B > 0).
    pub is_safe: bool,
    /// The budget_remaining input.
    pub budget_remaining: f64,
    /// The change_rate input.
    pub change_rate: f64,
}

/// Evaluate the barrier certificate at (budget_remaining, change_rate).
///
/// Both inputs should be in \[0, 1\]. Values outside this range are clamped.
///
/// Returns a [`BarrierResult`] with the barrier value and safety verdict.
///
/// # Performance
///
/// This evaluates a degree-4 polynomial with 15 terms using Horner-like
/// nested evaluation. Expected runtime is well under 30ns on modern hardware.
#[must_use]
pub fn evaluate(budget_remaining: f64, change_rate: f64) -> BarrierResult {
    let x1 = budget_remaining.clamp(0.0, 1.0);
    let x2 = change_rate.clamp(0.0, 1.0);

    let value = eval_polynomial(x1, x2);

    BarrierResult {
        value,
        is_safe: value > 0.0,
        budget_remaining: x1,
        change_rate: x2,
    }
}

/// Evaluate the bivariate polynomial B(x1, x2) = sum c[k] * x1^i * x2^j
/// using nested Horner's method.
///
/// For each x1-degree i, we evaluate the univariate polynomial in x2 via
/// Horner's method (innermost loop), then accumulate via Horner in x1
/// (outermost loop). Zero allocations, O(n_terms) multiplications.
#[inline]
fn eval_polynomial(x1: f64, x2: f64) -> f64 {
    // For each i = 0..=degree, the coefficients for that row are
    // BARRIER_COEFFS[offset..offset+(degree-i+1)] corresponding to
    // j = 0..=(degree-i).
    //
    // We evaluate each row as a univariate polynomial in x2 using Horner,
    // then combine with powers of x1.

    let mut result = 0.0;
    let mut offset = 0;
    let mut x1_power = 1.0;

    for i in 0..=BARRIER_DEGREE {
        let row_len = BARRIER_DEGREE - i + 1;
        // Horner in x2: evaluate c[i,row_len-1]*x2^(row_len-1) + ... + c[i,0]
        let mut row_val = BARRIER_COEFFS[offset + row_len - 1];
        for j in (0..row_len - 1).rev() {
            row_val = row_val * x2 + BARRIER_COEFFS[offset + j];
        }
        result += x1_power * row_val;
        x1_power *= x1;
        offset += row_len;
    }

    result
}

/// Margin of safety: how far inside the safe region the current state is.
///
/// Returns the barrier value directly — higher is safer, zero is the boundary,
/// negative means degradation territory.
#[must_use]
pub fn safety_margin(budget_remaining: f64, change_rate: f64) -> f64 {
    evaluate(budget_remaining, change_rate).value
}

/// Quick check: is the current state safe?
#[must_use]
pub fn is_admissible(budget_remaining: f64, change_rate: f64) -> bool {
    evaluate(budget_remaining, change_rate).is_safe
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Safe Region Tests ────────────────────────────────────────────────

    #[test]
    fn full_budget_no_changes_is_safe() {
        let r = evaluate(1.0, 0.0);
        assert!(r.is_safe);
        assert!(
            r.value > 1.0,
            "B(1,0) should be strongly positive: {}",
            r.value
        );
    }

    #[test]
    fn high_budget_low_change_is_safe() {
        let r = evaluate(0.8, 0.1);
        assert!(r.is_safe);
    }

    #[test]
    fn half_budget_moderate_change_is_safe() {
        let r = evaluate(0.5, 0.2);
        assert!(r.is_safe);
    }

    #[test]
    fn low_budget_low_change_is_safe() {
        let r = evaluate(0.3, 0.1);
        assert!(r.is_safe);
    }

    // ── Unsafe Region Tests ──────────────────────────────────────────────

    #[test]
    fn no_budget_high_change_is_unsafe() {
        let r = evaluate(0.0, 0.8);
        assert!(!r.is_safe);
        assert!(r.value < 0.0);
    }

    #[test]
    fn no_budget_max_change_is_unsafe() {
        let r = evaluate(0.0, 1.0);
        assert!(!r.is_safe);
        assert!(r.value < -0.5);
    }

    #[test]
    fn nearly_no_budget_very_high_change_is_unsafe() {
        let r = evaluate(0.05, 0.95);
        assert!(!r.is_safe);
    }

    // ── Boundary Tests ───────────────────────────────────────────────────

    #[test]
    fn origin_is_boundary() {
        let r = evaluate(0.0, 0.0);
        assert!(r.value.abs() < 1e-10, "B(0,0) should be ~0: {}", r.value);
    }

    // ── Input Clamping Tests ─────────────────────────────────────────────

    #[test]
    fn negative_budget_clamped_to_zero() {
        let r = evaluate(-0.5, 0.0);
        assert_eq!(r.budget_remaining, 0.0);
    }

    #[test]
    fn over_budget_clamped_to_one() {
        let r = evaluate(1.5, 0.0);
        assert_eq!(r.budget_remaining, 1.0);
    }

    #[test]
    fn negative_change_rate_clamped() {
        let r = evaluate(0.5, -0.1);
        assert_eq!(r.change_rate, 0.0);
    }

    #[test]
    fn over_change_rate_clamped() {
        let r = evaluate(0.5, 1.5);
        assert_eq!(r.change_rate, 1.0);
    }

    // ── API Tests ────────────────────────────────────────────────────────

    #[test]
    fn safety_margin_matches_evaluate() {
        let m = safety_margin(0.5, 0.2);
        let r = evaluate(0.5, 0.2);
        assert!((m - r.value).abs() < 1e-15);
    }

    #[test]
    fn is_admissible_matches_evaluate() {
        assert!(is_admissible(0.8, 0.1));
        assert!(!is_admissible(0.0, 0.9));
    }

    // ── Monotonicity Tests ───────────────────────────────────────────────

    #[test]
    fn increasing_budget_increases_safety() {
        let low = evaluate(0.2, 0.3);
        let high = evaluate(0.8, 0.3);
        assert!(
            high.value > low.value,
            "more budget should be safer: B(0.8,0.3)={} vs B(0.2,0.3)={}",
            high.value,
            low.value
        );
    }

    #[test]
    fn increasing_change_rate_decreases_safety() {
        let low = evaluate(0.5, 0.1);
        let high = evaluate(0.5, 0.8);
        assert!(
            low.value > high.value,
            "more change should be less safe: B(0.5,0.1)={} vs B(0.5,0.8)={}",
            low.value,
            high.value
        );
    }

    // ── Coefficient Integrity ────────────────────────────────────────────

    #[test]
    fn coefficients_match_expected_count() {
        assert_eq!(BARRIER_COEFFS.len(), BARRIER_N_TERMS);
        assert_eq!(BARRIER_N_TERMS, 15); // (4+1)(4+2)/2
    }

    #[test]
    fn degree_is_four() {
        assert_eq!(BARRIER_DEGREE, 4);
    }

    #[test]
    fn verification_points_reevaluate_consistently() {
        let safe_points = [(1.0, 0.0), (0.8, 0.2), (0.5, 0.2), (0.3, 0.1)];
        let unsafe_points = [(0.0, 0.8), (0.0, 1.0), (0.05, 0.95), (0.0, 0.0)];
        assert_eq!(
            safe_points.len() + unsafe_points.len(),
            BARRIER_VERIFICATION_POINTS
        );
        for (x1, x2) in safe_points {
            let r = evaluate(x1, x2);
            assert!(
                r.is_safe && r.value > 0.0,
                "Expected ({x1}, {x2}) to be safe (B > 0), got {}",
                r.value
            );
        }
        for (x1, x2) in unsafe_points {
            let r = evaluate(x1, x2);
            assert!(
                !r.is_safe && r.value <= 1e-10,
                "Expected ({x1}, {x2}) to be unsafe (B <= 0), got {}",
                r.value
            );
        }
    }

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn proptest_monotonicity_in_budget_and_change_rate(
                x1_a in 0.0f64..=1.0,
                x1_b in 0.0f64..=1.0,
                x2_a in 0.0f64..=1.0,
                x2_b in 0.0f64..=1.0,
            ) {
                let (x1_lo, x1_hi) = if x1_a <= x1_b { (x1_a, x1_b) } else { (x1_b, x1_a) };
                let (x2_lo, x2_hi) = if x2_a <= x2_b { (x2_a, x2_b) } else { (x2_b, x2_a) };

                // Monotonic in budget_remaining (x1): more budget is never less safe.
                let low_budget = evaluate(x1_lo, x2_lo);
                let high_budget = evaluate(x1_hi, x2_lo);
                prop_assert!(
                    low_budget.value <= high_budget.value + 1e-12,
                    "B({x1_lo}, {x2_lo})={} > B({x1_hi}, {x2_lo})={}",
                    low_budget.value,
                    high_budget.value
                );

                // Monotonic in change_rate (x2): higher change rate is never safer.
                let low_change = evaluate(x1_lo, x2_lo);
                let high_change = evaluate(x1_lo, x2_hi);
                prop_assert!(
                    low_change.value + 1e-12 >= high_change.value,
                    "B({x1_lo}, {x2_lo})={} < B({x1_lo}, {x2_hi})={}",
                    low_change.value,
                    high_change.value
                );

                // Boundary corners.
                prop_assert!(evaluate(1.0, 0.0).is_safe);
                prop_assert!(!evaluate(0.0, 1.0).is_safe);
            }
        }
    }
}
