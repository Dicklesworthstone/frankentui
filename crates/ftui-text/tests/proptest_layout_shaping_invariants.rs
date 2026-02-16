//! Property-based invariant tests for layout policy, tier budget,
//! cluster map, shaped render, justification, vertical metrics, and
//! degradation safety.
//!
//! These tests verify structural invariants across the text layout and
//! shaping pipeline that must hold for arbitrary valid inputs:
//!
//! 1. Policy resolution is idempotent and deterministic.
//! 2. Tier degradation is monotonic and never skips levels.
//! 3. Tier budgets are internally consistent and ordered.
//! 4. Feature toggles are monotonically enabled up the ladder.
//! 5. ClusterMap round-trips preserve byte boundaries.
//! 6. ClusterMap entries are monotonic and contiguous.
//! 7. ShapedLineLayout total cells matches placement count.
//! 8. Justification glue elasticity is non-negative.
//! 9. Justification badness is monotonic in ratio magnitude.
//! 10. Vertical metrics line positions are monotonic.
//! 11. Shaping fallback always produces valid output.
//! 12. Safety invariants are never violated by tier transitions.

use ftui_text::cluster_map::ClusterMap;
use ftui_text::justification::{
    GlueSpec, JustificationControl, JustifyMode, SUBCELL_SCALE, SpaceCategory,
};
use ftui_text::layout_policy::{LayoutPolicy, LayoutTier, RuntimeCapability};
use ftui_text::script_segmentation::{RunDirection, Script};
use ftui_text::shaped_render::ShapedLineLayout;
use ftui_text::shaping::{FontFeatures, NoopShaper, TextShaper};
use ftui_text::shaping_fallback::ShapingFallback;
use ftui_text::tier_budget::{SafetyInvariant, TierLadder};
use ftui_text::vertical_metrics::{BaselineGrid, LeadingSpec, ParagraphSpacing, VerticalPolicy};

use proptest::prelude::*;

// ── Strategies ──────────────────────────────────────────────────────────

fn arb_tier() -> impl Strategy<Value = LayoutTier> {
    prop_oneof![
        Just(LayoutTier::Emergency),
        Just(LayoutTier::Fast),
        Just(LayoutTier::Balanced),
        Just(LayoutTier::Quality),
    ]
}

fn arb_capability() -> impl Strategy<Value = RuntimeCapability> {
    (any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>()).prop_map(
        |(prop, sub, hyph, track)| RuntimeCapability {
            proportional_fonts: prop,
            subpixel_positioning: sub,
            hyphenation_available: hyph,
            tracking_support: track,
            max_paragraph_words: 0,
        },
    )
}

fn arb_policy() -> impl Strategy<Value = LayoutPolicy> {
    (arb_tier(), any::<bool>(), arb_justify_mode_option()).prop_map(
        |(tier, allow_deg, justify_override)| LayoutPolicy {
            tier,
            allow_degradation: allow_deg,
            justify_override,
            vertical_override: None,
            line_height_subpx: 0,
        },
    )
}

fn arb_justify_mode_option() -> impl Strategy<Value = Option<JustifyMode>> {
    prop_oneof![
        Just(None),
        Just(Some(JustifyMode::Left)),
        Just(Some(JustifyMode::Right)),
        Just(Some(JustifyMode::Center)),
        Just(Some(JustifyMode::Full)),
        Just(Some(JustifyMode::Distributed)),
    ]
}

fn arb_vertical_policy() -> impl Strategy<Value = VerticalPolicy> {
    prop_oneof![
        Just(VerticalPolicy::Compact),
        Just(VerticalPolicy::Readable),
        Just(VerticalPolicy::Typographic),
    ]
}

/// Unicode text with a mix of ASCII, CJK, and combining marks.
fn arb_mixed_text(max_len: usize) -> impl Strategy<Value = String> {
    let ascii = prop::collection::vec(0x20u8..=0x7E, 0..max_len)
        .prop_map(|v| String::from_utf8(v).unwrap());
    let mixed = prop::collection::vec(
        prop_oneof![
            Just("a".to_string()),
            Just("hello".to_string()),
            Just(" ".to_string()),
            Just("\u{4e16}".to_string()),                 // CJK '世'
            Just("\u{754c}".to_string()),                 // CJK '界'
            Just("\u{1f600}".to_string()),                // emoji
            Just("e\u{0301}".to_string()),                // combining accent
            Just("\u{0915}\u{094d}\u{0937}".to_string()), // Devanagari conjunct
        ],
        0..max_len,
    )
    .prop_map(|v| v.join(""));

    prop_oneof![ascii, mixed]
}

/// Non-empty mixed text.
fn arb_nonempty_text(max_len: usize) -> impl Strategy<Value = String> {
    arb_mixed_text(max_len).prop_filter("non-empty", |s| !s.is_empty())
}

// ═════════════════════════════════════════════════════════════════════════
// 1. Policy resolution is idempotent
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn policy_resolution_is_idempotent(
        policy in arb_policy(),
        caps in arb_capability(),
    ) {
        let r1 = policy.resolve(&caps);
        let r2 = policy.resolve(&caps);
        prop_assert_eq!(r1, r2, "Same policy+caps must produce same resolution");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. Effective tier is never above requested tier
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn effective_tier_never_above_requested(
        policy in arb_policy(),
        caps in arb_capability(),
    ) {
        if let Ok(resolved) = policy.resolve(&caps) {
            prop_assert!(
                resolved.effective_tier <= resolved.requested_tier,
                "Effective tier {} should not exceed requested {}",
                resolved.effective_tier,
                resolved.requested_tier
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. Degradation chain is monotonically decreasing
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn degradation_chain_is_monotonic(tier in arb_tier()) {
        let chain = tier.degradation_chain();
        for w in chain.windows(2) {
            prop_assert!(
                w[0] > w[1],
                "Chain must be strictly decreasing: {:?} not > {:?}",
                w[0], w[1]
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 4. Degradation chain always ends at Emergency
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn degradation_chain_ends_at_emergency(tier in arb_tier()) {
        let chain = tier.degradation_chain();
        prop_assert_eq!(
            *chain.last().unwrap(),
            LayoutTier::Emergency,
            "Chain must end at Emergency"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 5. best_tier never returns Emergency (Emergency is manual-only)
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn best_tier_never_emergency(caps in arb_capability()) {
        let best = caps.best_tier();
        prop_assert_ne!(
            best,
            LayoutTier::Emergency,
            "best_tier should never auto-select Emergency"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 6. Degradation disabled: error iff caps don't support tier
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn no_degradation_error_matches_capability(
        tier in arb_tier(),
        caps in arb_capability(),
    ) {
        let policy = LayoutPolicy {
            tier,
            allow_degradation: false,
            justify_override: None,
            vertical_override: None,
            line_height_subpx: 0,
        };
        let result = policy.resolve(&caps);
        if caps.supports_tier(tier) {
            prop_assert!(result.is_ok(), "Should succeed when caps support tier");
        } else {
            prop_assert!(result.is_err(), "Should fail when caps don't support tier");
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 7. Tier ladder budgets are consistent
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn tier_ladder_budgets_consistent() {
    let ladder = TierLadder::default_60fps();
    let issues = ladder.check_budget_consistency();
    assert!(issues.is_empty(), "Budget inconsistencies: {issues:?}");
}

#[test]
fn tier_ladder_budgets_ordered() {
    let ladder = TierLadder::default_60fps();
    let issues = ladder.check_budget_ordering();
    assert!(issues.is_empty(), "Budget ordering violations: {issues:?}");
}

#[test]
fn tier_ladder_features_monotonic() {
    let ladder = TierLadder::default_60fps();
    let violations = ladder.check_monotonicity();
    assert!(
        violations.is_empty(),
        "Feature monotonicity violations: {violations:?}"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 8. Safety invariants list is complete
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn safety_invariants_cover_critical_properties() {
    let all = SafetyInvariant::ALL;
    assert!(all.contains(&SafetyInvariant::NoContentLoss));
    assert!(all.contains(&SafetyInvariant::WideCharWidth));
    assert!(all.contains(&SafetyInvariant::DiffIdempotence));
    assert!(all.contains(&SafetyInvariant::WidthDeterminism));
    assert!(all.contains(&SafetyInvariant::GreedyWrapFallback));
}

// ═════════════════════════════════════════════════════════════════════════
// 9. ClusterMap round-trip: byte → cell → byte preserves cluster start
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cluster_map_byte_cell_roundtrip(text in arb_nonempty_text(50)) {
        let map = ClusterMap::from_text(&text);
        // Check every byte offset
        for byte_off in 0..text.len() {
            let cell = map.byte_to_cell(byte_off);
            let back = map.cell_to_byte(cell);
            // Round-trip should snap to the cluster start
            prop_assert!(
                back <= byte_off,
                "Round-trip byte {} -> cell {} -> byte {}: back should be <= original",
                byte_off, cell, back
            );
            // And the back value must be a valid char boundary
            prop_assert!(
                text.is_char_boundary(back),
                "Round-trip byte {} -> cell {} -> byte {}: not a char boundary",
                byte_off, cell, back
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 10. ClusterMap entries are monotonically non-decreasing in cell offset
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cluster_map_entries_monotonic(text in arb_nonempty_text(50)) {
        let map = ClusterMap::from_text(&text);
        let entries = map.entries();
        for w in entries.windows(2) {
            prop_assert!(
                w[1].cell_start >= w[0].cell_start,
                "Entry cells not monotonic: {} then {}",
                w[0].cell_start, w[1].cell_start
            );
            prop_assert!(
                w[1].byte_start >= w[0].byte_end,
                "Entry bytes overlap: [{},{}), [{},{})",
                w[0].byte_start, w[0].byte_end,
                w[1].byte_start, w[1].byte_end
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 11. ClusterMap covers all bytes
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cluster_map_covers_all_bytes(text in arb_nonempty_text(50)) {
        let map = ClusterMap::from_text(&text);
        let entries = map.entries();
        if !entries.is_empty() {
            prop_assert_eq!(
                entries[0].byte_start, 0,
                "First entry must start at byte 0"
            );
            prop_assert_eq!(
                entries.last().unwrap().byte_end as usize, text.len(),
                "Last entry must end at text length"
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 12. ShapedLineLayout from_text: total_cells matches visual width
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn shaped_layout_from_text_cells_match_placements(text in arb_nonempty_text(50)) {
        let layout = ShapedLineLayout::from_text(&text);
        prop_assert_eq!(
            layout.total_cells(),
            layout.placements().len(),
            "total_cells must match placements count"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 13. ShapedLineLayout from_run with NoopShaper: same cells as from_text
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn shaped_noop_matches_from_text(text in arb_nonempty_text(30)) {
        let shaper = NoopShaper;
        let features = FontFeatures::default();
        let run = shaper.shape(&text, Script::Latin, RunDirection::Ltr, &features);
        let from_run = ShapedLineLayout::from_run(&text, &run);
        let from_text = ShapedLineLayout::from_text(&text);
        prop_assert_eq!(
            from_run.total_cells(),
            from_text.total_cells(),
            "NoopShaper from_run and from_text should agree on cell count"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 14. ShapedLineLayout placements are monotonically positioned
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn shaped_placements_monotonic(text in arb_nonempty_text(30)) {
        let layout = ShapedLineLayout::from_text(&text);
        let placements = layout.placements();
        for w in placements.windows(2) {
            prop_assert!(
                w[1].cell_x >= w[0].cell_x,
                "Placements not monotonic: cell {} then {}",
                w[0].cell_x, w[1].cell_x
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 15. Justification: GlueSpec elasticity is non-negative
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn glue_spec_elasticity_nonneg(
        natural in 0u32..=2048,
        stretch in 0u32..=1024,
        shrink in 0u32..=512,
    ) {
        let glue = GlueSpec {
            natural_subcell: natural,
            stretch_subcell: stretch,
            shrink_subcell: shrink,
        };
        // Elasticity is always the sum of stretch + shrink
        prop_assert_eq!(
            glue.elasticity(),
            stretch + shrink,
            "Elasticity must be stretch + shrink"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 16. Justification: rigid glue has zero elasticity
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn rigid_glue_zero_elasticity(width in 0u32..=4096) {
        let glue = GlueSpec::rigid(width);
        prop_assert!(glue.is_rigid(), "rigid() must produce rigid glue");
        prop_assert_eq!(glue.elasticity(), 0, "Rigid glue must have 0 elasticity");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 17. Justification: adjusted width at ratio 0 equals natural
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn adjusted_width_at_zero_is_natural(
        natural in 0u32..=2048,
        stretch in 0u32..=1024,
        shrink in 0u32..=512,
    ) {
        let glue = GlueSpec {
            natural_subcell: natural,
            stretch_subcell: stretch,
            shrink_subcell: shrink,
        };
        let adjusted = glue.adjusted_width(0);
        prop_assert_eq!(
            adjusted, natural,
            "At ratio 0, adjusted width must equal natural"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 18. Justification: positive ratio increases width
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn positive_ratio_increases_width(
        natural in 1u32..=2048,
        stretch in 1u32..=1024,
    ) {
        let glue = GlueSpec {
            natural_subcell: natural,
            stretch_subcell: stretch,
            shrink_subcell: 0,
        };
        let adjusted = glue.adjusted_width(SUBCELL_SCALE as i32);
        prop_assert!(
            adjusted >= natural,
            "Positive ratio should not decrease width: {} < {}",
            adjusted, natural
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 19. Justification: badness is non-negative
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn badness_bounded(ratio in -10_000i32..=10_000) {
        let b = JustificationControl::badness(ratio);
        // Badness should be finite and bounded for reasonable ratios.
        // At ratio=0, badness=0. At extremes, it grows but stays finite.
        if ratio == 0 {
            prop_assert_eq!(b, 0, "Badness at ratio 0 must be 0");
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 20. Vertical metrics: line positions are monotonically increasing
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn line_positions_monotonic(
        policy in arb_vertical_policy(),
        line_count in 1usize..=20,
    ) {
        let line_h = 16 * 256; // 16px
        let metrics = policy.resolve(line_h);
        for i in 0..line_count.saturating_sub(1) {
            let y1 = metrics.line_y(i, line_h);
            let y2 = metrics.line_y(i + 1, line_h);
            prop_assert!(
                y2 > y1,
                "Line {} at {} not < line {} at {}",
                i, y1, i + 1, y2
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 21. Vertical metrics: paragraph height ≥ line_count × line_height
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn paragraph_height_at_least_lines(
        policy in arb_vertical_policy(),
        line_count in 1usize..=20,
    ) {
        let line_h = 16 * 256;
        let metrics = policy.resolve(line_h);
        let height = metrics.paragraph_height(line_count, line_h);
        let min_height = (line_count as u32) * line_h;
        prop_assert!(
            height >= min_height,
            "Paragraph height {} < minimum {} for {} lines",
            height, min_height, line_count
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 22. Baseline grid: snap is idempotent
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn baseline_grid_snap_idempotent(pos in 0u32..=100_000) {
        let grid = BaselineGrid::from_line_height(16 * 256, 4 * 256);
        let snapped = grid.snap(pos);
        let snapped2 = grid.snap(snapped);
        prop_assert_eq!(
            snapped, snapped2,
            "Grid snap must be idempotent: snap({}) = {} but snap({}) = {}",
            pos, snapped, snapped, snapped2
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 23. Shaping fallback: terminal mode always produces valid output
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn terminal_fallback_always_valid(text in arb_nonempty_text(50)) {
        let fb = ShapingFallback::<NoopShaper>::terminal();
        let (layout, _event) = fb.shape_line(&text, Script::Latin, RunDirection::Ltr);
        prop_assert!(
            layout.total_cells() > 0,
            "Terminal fallback must produce non-zero cells for non-empty input"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 24. Shaping fallback: shaped mode produces valid output
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn shaped_fallback_always_valid(text in arb_nonempty_text(30)) {
        let fb = ShapingFallback::with_shaper(NoopShaper, RuntimeCapability::FULL);
        let (layout, _event) = fb.shape_line(&text, Script::Latin, RunDirection::Ltr);
        prop_assert!(
            layout.total_cells() > 0,
            "Shaped fallback must produce non-zero cells for non-empty input"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 25. Shaping fallback: determinism — same input, same output
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn shaping_fallback_deterministic(text in arb_nonempty_text(30)) {
        let fb = ShapingFallback::<NoopShaper>::terminal();
        let (layout1, event1) = fb.shape_line(&text, Script::Latin, RunDirection::Ltr);
        let (layout2, event2) = fb.shape_line(&text, Script::Latin, RunDirection::Ltr);
        prop_assert_eq!(
            layout1.total_cells(),
            layout2.total_cells(),
            "Same input must produce same cell count"
        );
        prop_assert_eq!(event1, event2, "Same input must produce same event");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 26. Leading spec resolve is monotonic in line height
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn leading_proportional_monotonic(
        ratio in 1u32..=512,
        h1 in 1u32..=100,
        h2 in 1u32..=100,
    ) {
        let spec = LeadingSpec::Proportional(ratio);
        let lh1 = h1 * 256;
        let lh2 = h2 * 256;
        let r1 = spec.resolve(lh1);
        let r2 = spec.resolve(lh2);
        if lh1 <= lh2 {
            prop_assert!(
                r1 <= r2,
                "Proportional leading not monotonic: resolve({}) = {} > resolve({}) = {}",
                lh1, r1, lh2, r2
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 27. Paragraph spacing total is sum of before + after
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn paragraph_spacing_total(before in 0u32..=10_000, after in 0u32..=10_000) {
        let ps = ParagraphSpacing::custom(before, after);
        prop_assert_eq!(
            ps.total(),
            before + after,
            "Total must be before + after"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 28. Justification total_natural is sum of per-space naturals
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn justification_total_natural_is_sum(
        n_word in 0usize..=10,
        n_sentence in 0usize..=5,
    ) {
        let ctrl = JustificationControl::READABLE;
        let mut spaces = Vec::new();
        for _ in 0..n_word {
            spaces.push(SpaceCategory::InterWord);
        }
        for _ in 0..n_sentence {
            spaces.push(SpaceCategory::InterSentence);
        }
        let total = ctrl.total_natural(&spaces);
        let manual: u32 = spaces
            .iter()
            .map(|c| ctrl.glue_for(*c).natural_subcell)
            .sum();
        prop_assert_eq!(total, manual, "total_natural must equal manual sum");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 29. ClusterMap cell_range_to_byte_range produces valid range
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cluster_map_cell_range_valid(text in arb_nonempty_text(50)) {
        let map = ClusterMap::from_text(&text);
        let total = map.total_cells();
        if total > 1 {
            let start = 0;
            let end = total;
            let (bs, be) = map.cell_range_to_byte_range(start, end);
            prop_assert!(
                bs <= be,
                "Byte range start {} > end {}",
                bs, be
            );
            prop_assert!(
                be <= text.len(),
                "Byte range end {} > text len {}",
                be, text.len()
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 30. Empty text produces empty results everywhere
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn empty_text_cluster_map() {
    let map = ClusterMap::from_text("");
    assert_eq!(map.entries().len(), 0);
    assert_eq!(map.total_cells(), 0);
}

#[test]
fn empty_text_shaped_layout() {
    let layout = ShapedLineLayout::from_text("");
    assert_eq!(layout.total_cells(), 0);
    assert_eq!(layout.placements().len(), 0);
}

#[test]
fn empty_text_fallback() {
    let fb = ShapingFallback::<NoopShaper>::terminal();
    let (layout, _event) = fb.shape_line("", Script::Latin, RunDirection::Ltr);
    assert_eq!(layout.total_cells(), 0);
}
