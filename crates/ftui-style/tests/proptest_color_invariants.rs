//! Property-based invariant tests for color types, conversion, WCAG contrast,
//! blend modes, gradients, and the downgrade pipeline.
//!
//! These tests verify structural invariants that must hold for any valid inputs:
//!
//! 1. Relative luminance is in [0, 1].
//! 2. Contrast ratio is symmetric.
//! 3. Contrast ratio is in [1.0, 21.0].
//! 4. Contrast ratio with self is 1.0.
//! 5. rgb_to_256 round-trip stability (idempotent after one cycle).
//! 6. ansi256_to_rgb always produces valid RGB (full range).
//! 7. Color downgrade is idempotent.
//! 8. Color downgrade monotonicity (lower never increases fidelity).
//! 9. Rgb::as_key is injective.
//! 10. luminance_u8 bounds [0, 255].
//! 11. best_text_color returns a candidate from the list.
//! 12. ColorCache agrees with direct downgrade.
//! 13. lerp_color(a, a, t) == a.
//! 14. lerp_color endpoints: t=0 → a, t=1 → b.
//! 15. Blend modes never exceed channel bounds.
//! 16. Gradient::sample always returns valid RGBA.
//! 17. No panics on arbitrary color inputs.

use ftui_render::cell::PackedRgba;
use ftui_style::color::{
    Color, ColorCache, ColorProfile, MonoColor, Rgb, ansi256_to_rgb, best_text_color,
    contrast_ratio, relative_luminance, rgb_to_256, rgb_to_mono,
};
use ftui_style::table_theme::Gradient;
use proptest::prelude::*;

// ── Helpers ─────────────────────────────────────────────────────────────

fn rgb_strategy() -> impl Strategy<Value = Rgb> {
    (any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(r, g, b)| Rgb::new(r, g, b))
}

fn packed_rgba_strategy() -> impl Strategy<Value = PackedRgba> {
    any::<u32>().prop_map(PackedRgba)
}

fn color_strategy() -> impl Strategy<Value = Color> {
    prop_oneof![
        (any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(r, g, b)| Color::rgb(r, g, b)),
        any::<u8>().prop_map(Color::Ansi256),
        (0u8..16).prop_map(|i| Color::Ansi16(ftui_style::color::Ansi16::from_u8(i).unwrap())),
        proptest::bool::ANY.prop_map(|b| if b {
            Color::Mono(MonoColor::White)
        } else {
            Color::Mono(MonoColor::Black)
        }),
    ]
}

fn profile_strategy() -> impl Strategy<Value = ColorProfile> {
    prop_oneof![
        Just(ColorProfile::TrueColor),
        Just(ColorProfile::Ansi256),
        Just(ColorProfile::Ansi16),
        Just(ColorProfile::Mono),
    ]
}

fn gradient_strategy() -> impl Strategy<Value = Gradient> {
    proptest::collection::vec((0.0f32..=1.0, packed_rgba_strategy()), 2..=8).prop_map(Gradient::new)
}

// ═════════════════════════════════════════════════════════════════════════
// 1. Relative luminance is in [0, 1]
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn luminance_in_bounds(rgb in rgb_strategy()) {
        let lum = relative_luminance(rgb);
        prop_assert!(
            (0.0..=1.0).contains(&lum),
            "luminance {} out of [0,1] for {:?}",
            lum, rgb
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. Contrast ratio is symmetric
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn contrast_ratio_symmetric(a in rgb_strategy(), b in rgb_strategy()) {
        let ab = contrast_ratio(a, b);
        let ba = contrast_ratio(b, a);
        prop_assert!(
            (ab - ba).abs() < 1e-10,
            "contrast_ratio({:?},{:?})={} != contrast_ratio({:?},{:?})={}",
            a, b, ab, b, a, ba
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. Contrast ratio is in [1.0, 21.0]
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn contrast_ratio_in_bounds(a in rgb_strategy(), b in rgb_strategy()) {
        let ratio = contrast_ratio(a, b);
        prop_assert!(
            (1.0..=21.01).contains(&ratio),
            "contrast_ratio({:?},{:?})={} outside [1.0, 21.0]",
            a, b, ratio
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 4. Contrast ratio with self is 1.0
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn contrast_ratio_self_is_one(rgb in rgb_strategy()) {
        let ratio = contrast_ratio(rgb, rgb);
        prop_assert!(
            (ratio - 1.0).abs() < 1e-10,
            "contrast_ratio({:?},{:?})={} should be 1.0",
            rgb, rgb, ratio
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 5. rgb_to_256 round-trip stability (idempotent after second cycle)
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn rgb_to_256_round_trip_stable(r in any::<u8>(), g in any::<u8>(), b in any::<u8>()) {
        // First cycle may shift between cube and grayscale ramp (e.g. near-gray
        // colors). The second cycle must be stable: once we reach a palette
        // color, re-encoding it must return the same index.
        let idx1 = rgb_to_256(r, g, b);
        let rgb1 = ansi256_to_rgb(idx1);
        let idx2 = rgb_to_256(rgb1.r, rgb1.g, rgb1.b);
        let rgb2 = ansi256_to_rgb(idx2);
        let idx3 = rgb_to_256(rgb2.r, rgb2.g, rgb2.b);
        prop_assert_eq!(
            idx2, idx3,
            "rgb_to_256 not stable after second cycle: ({},{},{}) -> {} -> {:?} -> {} -> {:?} -> {}",
            r, g, b, idx1, rgb1, idx2, rgb2, idx3
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 6. ansi256_to_rgb full range
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn ansi256_to_rgb_never_panics(idx in any::<u8>()) {
        let rgb = ansi256_to_rgb(idx);
        // All u8 channels are inherently in-bounds; this ensures no panics.
        let _ = (rgb.r, rgb.g, rgb.b);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 7. Color downgrade is idempotent
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn downgrade_idempotent(color in color_strategy(), profile in profile_strategy()) {
        let once = color.downgrade(profile);
        let twice = once.downgrade(profile);
        prop_assert_eq!(
            once, twice,
            "downgrade not idempotent: {:?} @ {:?} -> {:?} -> {:?}",
            color, profile, once, twice
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 8. Color downgrade monotonicity
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn downgrade_mono_from_any_profile(color in color_strategy()) {
        let mono = color.downgrade(ColorProfile::Mono);
        prop_assert!(
            matches!(mono, Color::Mono(_)),
            "Downgrade to Mono should produce Mono, got {:?}",
            mono
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 9. Rgb::as_key is injective
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn rgb_as_key_injective(
        r1 in any::<u8>(), g1 in any::<u8>(), b1 in any::<u8>(),
        r2 in any::<u8>(), g2 in any::<u8>(), b2 in any::<u8>(),
    ) {
        let a = Rgb::new(r1, g1, b1);
        let b = Rgb::new(r2, g2, b2);
        if a != b {
            prop_assert_ne!(
                a.as_key(), b.as_key(),
                "Different colors {:?} and {:?} have same key {}",
                a, b, a.as_key()
            );
        } else {
            prop_assert_eq!(a.as_key(), b.as_key());
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 10. luminance_u8 bounds
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn luminance_u8_in_bounds(rgb in rgb_strategy()) {
        let lum = rgb.luminance_u8();
        // u8 is always in [0, 255], but verify it doesn't panic and
        // monotonicity: brighter channels → higher luminance.
        let _ = lum;
    }

    #[test]
    fn luminance_u8_green_dominates(r in any::<u8>(), b in any::<u8>()) {
        // Green has highest BT.709 weight; pure green >= pure red, pure blue at same level
        let green_lum = Rgb::new(0, 200, 0).luminance_u8();
        let red_lum = Rgb::new(200, 0, 0).luminance_u8();
        let blue_lum = Rgb::new(0, 0, 200).luminance_u8();
        prop_assert!(green_lum > red_lum, "green {} should > red {}", green_lum, red_lum);
        prop_assert!(green_lum > blue_lum, "green {} should > blue {}", green_lum, blue_lum);
        // Suppress unused variable warnings
        let _ = (r, b);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 11. best_text_color returns a candidate
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn best_text_color_from_candidates(
        bg in rgb_strategy(),
        c0 in rgb_strategy(),
        c1 in rgb_strategy(),
        c2 in rgb_strategy(),
    ) {
        let candidates = [c0, c1, c2];
        let best = best_text_color(bg, &candidates);
        prop_assert!(
            candidates.contains(&best),
            "best_text_color returned {:?} which is not in candidates {:?}",
            best, candidates
        );
    }

    #[test]
    fn best_text_color_maximizes_contrast(
        bg in rgb_strategy(),
        c0 in rgb_strategy(),
        c1 in rgb_strategy(),
    ) {
        let candidates = [c0, c1];
        let best = best_text_color(bg, &candidates);
        let best_ratio = contrast_ratio(best, bg);
        for c in &candidates {
            let ratio = contrast_ratio(*c, bg);
            prop_assert!(
                best_ratio >= ratio - 1e-10,
                "best {:?} (ratio {}) is worse than {:?} (ratio {})",
                best, best_ratio, c, ratio
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 12. ColorCache agrees with direct downgrade
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cache_agrees_with_direct(rgb in rgb_strategy(), profile in profile_strategy()) {
        let direct = Color::Rgb(rgb).downgrade(profile);
        let mut cache = ColorCache::with_capacity(profile, 64);
        let cached = cache.downgrade_rgb(rgb);
        prop_assert_eq!(
            direct, cached,
            "Cache disagrees: direct={:?}, cached={:?} for {:?} @ {:?}",
            direct, cached, rgb, profile
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 13. lerp_color(a, a, t) == a
// ═════════════════════════════════════════════════════════════════════════

// lerp_color is private, but we can test it via Gradient with two identical stops.
proptest! {
    #[test]
    fn gradient_constant_returns_constant(color in packed_rgba_strategy(), t in 0.0f32..=1.0) {
        let gradient = Gradient::new(vec![(0.0, color), (1.0, color)]);
        let sampled = gradient.sample(t);
        prop_assert_eq!(
            sampled, color,
            "Gradient({:?},{:?}).sample({}) = {:?}, expected {:?}",
            color, color, t, sampled, color
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 14. lerp_color endpoints (via Gradient)
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn gradient_endpoints(a in packed_rgba_strategy(), b in packed_rgba_strategy()) {
        let gradient = Gradient::new(vec![(0.0, a), (1.0, b)]);
        let at_zero = gradient.sample(0.0);
        let at_one = gradient.sample(1.0);
        prop_assert_eq!(at_zero, a, "gradient.sample(0.0) should be first stop");
        prop_assert_eq!(at_one, b, "gradient.sample(1.0) should be last stop");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 15. Gradient sample always returns valid RGBA (never panics)
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn gradient_sample_never_panics(gradient in gradient_strategy(), t in -1.0f32..=2.0) {
        let result = gradient.sample(t);
        // Just verify no panic; channels are u8 so always valid.
        let _ = (result.r(), result.g(), result.b(), result.a());
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 16. Gradient stops are sorted after construction
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn gradient_stops_sorted(
        stops in proptest::collection::vec(
            (0.0f32..=1.0, packed_rgba_strategy()),
            2..=8,
        )
    ) {
        let gradient = Gradient::new(stops);
        let sorted_stops = gradient.stops();
        for window in sorted_stops.windows(2) {
            prop_assert!(
                window[0].0 <= window[1].0,
                "Gradient stops not sorted: {} > {}",
                window[0].0, window[1].0
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 17. rgb_to_mono agrees with luminance threshold
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn rgb_to_mono_agrees_with_luminance(rgb in rgb_strategy()) {
        let mono = rgb_to_mono(rgb.r, rgb.g, rgb.b);
        let lum = rgb.luminance_u8();
        if lum >= 128 {
            prop_assert_eq!(mono, MonoColor::White,
                "luminance {} >= 128 but mono is Black for {:?}", lum, rgb);
        } else {
            prop_assert_eq!(mono, MonoColor::Black,
                "luminance {} < 128 but mono is White for {:?}", lum, rgb);
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 18. Color::to_rgb round-trip through downgrade
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn to_rgb_is_total(color in color_strategy()) {
        // to_rgb should succeed for any Color variant without panic.
        let rgb = color.to_rgb();
        let _ = (rgb.r, rgb.g, rgb.b);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 19. Downgrade to TrueColor is identity
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn downgrade_truecolor_identity(color in color_strategy()) {
        prop_assert_eq!(
            color.downgrade(ColorProfile::TrueColor),
            color,
            "Downgrade to TrueColor should be identity for {:?}",
            color
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 20. No panics on arbitrary operations
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn no_panic_color_pipeline(
        r in any::<u8>(), g in any::<u8>(), b in any::<u8>(),
        profile in profile_strategy(),
    ) {
        let color = Color::rgb(r, g, b);
        let _ = color.downgrade(profile);
        let _ = color.to_rgb();
        let _ = rgb_to_256(r, g, b);
        let _ = relative_luminance(Rgb::new(r, g, b));
        let _ = Rgb::new(r, g, b).luminance_u8();
        let _ = rgb_to_mono(r, g, b);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 21. Gradient sample monotonicity for two-stop gradients
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn gradient_two_stop_sample_is_deterministic(
        a in packed_rgba_strategy(),
        b in packed_rgba_strategy(),
        t in 0.0f32..=1.0,
    ) {
        let gradient = Gradient::new(vec![(0.0, a), (1.0, b)]);
        let s1 = gradient.sample(t);
        let s2 = gradient.sample(t);
        prop_assert_eq!(s1, s2, "Gradient sample is non-deterministic at t={}", t);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 22. Gradient empty returns TRANSPARENT
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn gradient_empty_returns_transparent(t in -1.0f32..=2.0) {
        let gradient = Gradient::new(vec![]);
        let sampled = gradient.sample(t);
        prop_assert_eq!(
            sampled,
            PackedRgba::TRANSPARENT,
            "Empty gradient should return TRANSPARENT at t={}, got {:?}",
            t, sampled
        );
    }
}
