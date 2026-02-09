//! Property-based invariant tests for the visual FX sampling subsystem.
//!
//! Verifies structural guarantees of coordinate mapping and samplers:
//!
//! 1.  cell_to_normalized always in [0.0, 1.0] for valid inputs
//! 2.  cell_to_normalized is strictly monotonic
//! 3.  cell_to_normalized with total=0 returns 0.5
//! 4.  cell_to_normalized symmetry: first + last sum to 1.0
//! 5.  fill_normalized_coords values are monotonic and bounded
//! 6.  CoordCache matches cell_to_normalized
//! 7.  CoordCache grow-only: ensure_size never shrinks
//! 8.  CoordCache out-of-range returns 0.5
//! 9.  PlasmaSampler bounded in [0.0, 1.0] for all qualities
//! 10. PlasmaSampler deterministic
//! 11. PlasmaSampler Off quality returns exactly 0.0
//! 12. MetaballFieldSampler field is non-negative
//! 13. MetaballFieldSampler deterministic
//! 14. MetaballFieldSampler Off returns (0.0, 0.0)
//! 15. MetaballFieldSampler empty balls returns (0.0, 0.0)
//! 16. FnSampler passes quality through

use ftui_extras::visual_fx::FxQuality;
use ftui_extras::visual_fx::effects::sampling::{
    BallState, CoordCache, FnSampler, MetaballFieldSampler, PlasmaSampler, Sampler,
    cell_to_normalized, fill_normalized_coords,
};
use proptest::prelude::*;

// ── Helpers ──────────────────────────────────────────────────────────

fn arb_quality() -> impl Strategy<Value = FxQuality> {
    prop_oneof![
        Just(FxQuality::Off),
        Just(FxQuality::Minimal),
        Just(FxQuality::Reduced),
        Just(FxQuality::Full),
    ]
}

fn arb_ball_state() -> impl Strategy<Value = BallState> {
    (0.0f64..=1.0, 0.0f64..=1.0, 0.001f64..=0.1, 0.0f64..=1.0)
        .prop_map(|(x, y, r2, hue)| BallState { x, y, r2, hue })
}

// ═════════════════════════════════════════════════════════════════════════
// 1. cell_to_normalized always in [0.0, 1.0]
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cell_to_normalized_bounded(cell in 0u16..=1000, total in 1u16..=1000) {
        let cell = cell.min(total.saturating_sub(1));
        let v = cell_to_normalized(cell, total);
        prop_assert!(
            (0.0..=1.0).contains(&v),
            "cell_to_normalized({}, {}) = {} out of [0,1]",
            cell, total, v
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. cell_to_normalized is strictly monotonic
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cell_to_normalized_monotonic(total in 2u16..=500) {
        for cell in 1..total {
            let prev = cell_to_normalized(cell - 1, total);
            let curr = cell_to_normalized(cell, total);
            prop_assert!(
                curr > prev,
                "not monotonic at cell={}, total={}: {} >= {}",
                cell, total, prev, curr
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. cell_to_normalized with total=0 returns 0.5
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cell_to_normalized_zero_total(cell in 0u16..=100) {
        let v = cell_to_normalized(cell, 0);
        prop_assert!(
            (v - 0.5).abs() < 1e-10,
            "total=0 should return 0.5, got {}",
            v
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 4. cell_to_normalized symmetry
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cell_to_normalized_symmetric(total in 2u16..=500) {
        let first = cell_to_normalized(0, total);
        let last = cell_to_normalized(total - 1, total);
        prop_assert!(
            (first + last - 1.0).abs() < 1e-10,
            "first ({}) + last ({}) should sum to 1.0 for total={}",
            first, last, total
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 5. fill_normalized_coords values are monotonic and bounded
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn fill_normalized_coords_monotonic_bounded(total in 1u16..=200) {
        let mut coords = vec![0.0; total as usize];
        fill_normalized_coords(total, &mut coords);

        // All in [0, 1]
        for (i, &c) in coords.iter().enumerate() {
            prop_assert!(
                (0.0..=1.0).contains(&c),
                "coord[{}] = {} out of [0,1] for total={}",
                i, c, total
            );
        }

        // Strictly monotonic
        for w in coords.windows(2) {
            prop_assert!(
                w[1] > w[0],
                "not monotonic: {} >= {} for total={}",
                w[0], w[1], total
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 6. CoordCache matches cell_to_normalized
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn coord_cache_matches_direct(
        width in 1u16..=100,
        height in 1u16..=100,
    ) {
        let cache = CoordCache::new(width, height);
        for cell in 0..width {
            let cached = cache.x(cell);
            let direct = cell_to_normalized(cell, width);
            prop_assert!(
                (cached - direct).abs() < 1e-12,
                "x cache mismatch at cell={}: {} vs {}",
                cell, cached, direct
            );
        }
        for cell in 0..height {
            let cached = cache.y(cell);
            let direct = cell_to_normalized(cell, height);
            prop_assert!(
                (cached - direct).abs() < 1e-12,
                "y cache mismatch at cell={}: {} vs {}",
                cell, cached, direct
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 7. CoordCache grow-only
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn coord_cache_grow_only(
        w1 in 1u16..=100,
        h1 in 1u16..=100,
        w2 in 1u16..=100,
        h2 in 1u16..=100,
    ) {
        let mut cache = CoordCache::new(w1, h1);
        cache.ensure_size(w2, h2);
        // Length should be at least max(w1, w2) and max(h1, h2)
        prop_assert!(
            cache.x_coords().len() >= w1.max(w2) as usize,
            "x_coords shrunk: {} < max({}, {})",
            cache.x_coords().len(), w1, w2
        );
        prop_assert!(
            cache.y_coords().len() >= h1.max(h2) as usize,
            "y_coords shrunk: {} < max({}, {})",
            cache.y_coords().len(), h1, h2
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 8. CoordCache out-of-range returns 0.5
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn coord_cache_oob_returns_default(
        width in 1u16..=50,
        height in 1u16..=50,
        oob in 100u16..=500,
    ) {
        let cache = CoordCache::new(width, height);
        let x = cache.x(oob);
        let y = cache.y(oob);
        prop_assert!(
            (x - 0.5).abs() < 1e-10,
            "OOB x({}) should be 0.5, got {}",
            oob, x
        );
        prop_assert!(
            (y - 0.5).abs() < 1e-10,
            "OOB y({}) should be 0.5, got {}",
            oob, y
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 9. PlasmaSampler bounded in [0.0, 1.0] for all qualities
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn plasma_bounded(
        x in 0.0f64..=1.0,
        y in 0.0f64..=1.0,
        time in 0.0f64..=100.0,
        quality in arb_quality(),
    ) {
        let sampler = PlasmaSampler;
        let v = sampler.sample(x, y, time, quality);
        prop_assert!(
            (0.0..=1.0).contains(&v),
            "plasma({}, {}, {}, {:?}) = {} out of [0,1]",
            x, y, time, quality, v
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 10. PlasmaSampler deterministic
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn plasma_deterministic(
        x in 0.0f64..=1.0,
        y in 0.0f64..=1.0,
        time in 0.0f64..=100.0,
        quality in arb_quality(),
    ) {
        let sampler = PlasmaSampler;
        let v1 = sampler.sample(x, y, time, quality);
        let v2 = sampler.sample(x, y, time, quality);
        prop_assert!(
            (v1 - v2).abs() < 1e-15,
            "plasma not deterministic: {} vs {} at ({}, {}, {})",
            v1, v2, x, y, time
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 11. PlasmaSampler Off quality returns 0.0
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn plasma_off_is_zero(
        x in 0.0f64..=1.0,
        y in 0.0f64..=1.0,
        time in 0.0f64..=100.0,
    ) {
        let sampler = PlasmaSampler;
        let v = sampler.sample(x, y, time, FxQuality::Off);
        prop_assert!(
            v.abs() < 1e-15,
            "plasma Off should be 0.0, got {}",
            v
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 12. MetaballFieldSampler field is non-negative
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn metaball_field_non_negative(
        balls in proptest::collection::vec(arb_ball_state(), 1..=8),
        x in 0.0f64..=1.0,
        y in 0.0f64..=1.0,
        quality in arb_quality(),
    ) {
        let (field, _hue) = MetaballFieldSampler::sample_field_from_slice(&balls, x, y, quality);
        prop_assert!(
            field >= 0.0,
            "metaball field should be non-negative, got {}",
            field
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 13. MetaballFieldSampler deterministic
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn metaball_deterministic(
        balls in proptest::collection::vec(arb_ball_state(), 1..=5),
        x in 0.0f64..=1.0,
        y in 0.0f64..=1.0,
    ) {
        let (f1, h1): (f64, f64) = MetaballFieldSampler::sample_field_from_slice(&balls, x, y, FxQuality::Full);
        let (f2, h2): (f64, f64) = MetaballFieldSampler::sample_field_from_slice(&balls, x, y, FxQuality::Full);
        prop_assert!(
            (f1 - f2).abs() < 1e-15,
            "metaball field not deterministic: {} vs {}",
            f1, f2
        );
        prop_assert!(
            (h1 - h2).abs() < 1e-15,
            "metaball hue not deterministic: {} vs {}",
            h1, h2
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 14. MetaballFieldSampler Off returns (0.0, 0.0)
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn metaball_off_is_zero(
        balls in proptest::collection::vec(arb_ball_state(), 1..=5),
        x in 0.0f64..=1.0,
        y in 0.0f64..=1.0,
    ) {
        let (field, hue): (f64, f64) = MetaballFieldSampler::sample_field_from_slice(&balls, x, y, FxQuality::Off);
        prop_assert!(field.abs() < 1e-15, "Off field should be 0, got {}", field);
        prop_assert!(hue.abs() < 1e-15, "Off hue should be 0, got {}", hue);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 15. MetaballFieldSampler empty balls returns (0.0, 0.0)
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn metaball_empty_is_zero(
        x in 0.0f64..=1.0,
        y in 0.0f64..=1.0,
        quality in arb_quality(),
    ) {
        let (field, hue): (f64, f64) = MetaballFieldSampler::sample_field_from_slice(&[], x, y, quality);
        prop_assert!(field.abs() < 1e-15, "empty field should be 0, got {}", field);
        prop_assert!(hue.abs() < 1e-15, "empty hue should be 0, got {}", hue);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 16. FnSampler passes quality through
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn fn_sampler_quality_passthrough(
        x in 0.0f64..=1.0,
        y in 0.0f64..=1.0,
        time in 0.0f64..=10.0,
    ) {
        let sampler = FnSampler::new(
            |_x, _y, _t, q| match q {
                FxQuality::Full => 1.0,
                FxQuality::Reduced => 0.75,
                FxQuality::Minimal => 0.5,
                FxQuality::Off => 0.0,
            },
            "test",
        );
        prop_assert!((sampler.sample(x, y, time, FxQuality::Full) - 1.0).abs() < 1e-10);
        prop_assert!((sampler.sample(x, y, time, FxQuality::Reduced) - 0.75).abs() < 1e-10);
        prop_assert!((sampler.sample(x, y, time, FxQuality::Minimal) - 0.5).abs() < 1e-10);
        prop_assert!((sampler.sample(x, y, time, FxQuality::Off) - 0.0).abs() < 1e-10);
    }
}
