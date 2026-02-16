//! Deterministic fuzz-style regression tests.
//!
//! These tests exercise the same code paths as the libfuzzer targets but
//! run deterministically in CI using proptest. Each test generates a large
//! number of random inputs and asserts that the code never panics and
//! maintains structural invariants.
//!
//! When a libfuzzer crash is found, the reproducing input should be added
//! as a concrete regression case in this file.

use ftui_text::WidthCache;
use ftui_text::cluster_map::ClusterMap;
use ftui_text::hyphenation::{break_penalties, english_dict_mini};
use ftui_text::script_segmentation::{RunDirection, Script};
use ftui_text::shaped_render::ShapedLineLayout;
use ftui_text::shaping::{FontFeatures, NoopShaper, TextShaper};
use ftui_text::shaping_fallback::ShapingFallback;
use ftui_text::wrap::{WrapMode, display_width, wrap_text};

use proptest::prelude::*;

// ── Strategies ──────────────────────────────────────────────────────────

/// Arbitrary valid UTF-8 with control characters, nulls, and high Unicode.
fn arb_fuzzy_text(max_len: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..max_len).prop_map(|chars| chars.into_iter().collect())
}

// ═════════════════════════════════════════════════════════════════════════
// Cluster map fuzz
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn fuzz_cluster_map_no_panic(text in arb_fuzzy_text(200)) {
        let map = ClusterMap::from_text(&text);
        let entries = map.entries();
        if !entries.is_empty() {
            assert_eq!(entries[0].byte_start, 0);
            assert_eq!(entries.last().unwrap().byte_end as usize, text.len());
            for w in entries.windows(2) {
                prop_assert!(w[1].cell_start >= w[0].cell_start);
            }
        }
        // Exercise lookups.
        for i in 0..text.len().min(50) {
            let _cell = map.byte_to_cell(i);
        }
        let total = map.total_cells();
        for c in 0..total.min(50) {
            let _byte = map.cell_to_byte(c);
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// Shaped layout fuzz
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn fuzz_shaped_layout_no_panic(text in arb_fuzzy_text(100)) {
        let layout = ShapedLineLayout::from_text(&text);
        // Zero-width chars (e.g. \0, combining marks) create placements but
        // contribute 0 to total_cells, so placements.len() >= total_cells().
        prop_assert!(layout.placements().len() >= layout.total_cells());
        let placements = layout.placements();
        for w in placements.windows(2) {
            prop_assert!(w[1].cell_x >= w[0].cell_x);
        }
    }

    #[test]
    fn fuzz_shaped_noop_no_panic(text in arb_fuzzy_text(60)) {
        if text.is_empty() {
            return Ok(());
        }
        let shaper = NoopShaper;
        let features = FontFeatures::default();
        let run = shaper.shape(&text, Script::Latin, RunDirection::Ltr, &features);
        let layout = ShapedLineLayout::from_run(&text, &run);
        prop_assert!(layout.placements().len() >= layout.total_cells());
    }
}

// ═════════════════════════════════════════════════════════════════════════
// Shaping fallback fuzz
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn fuzz_shaping_fallback_no_panic(text in arb_fuzzy_text(100)) {
        let fb = ShapingFallback::<NoopShaper>::terminal();
        let (layout, _event) = fb.shape_line(&text, Script::Latin, RunDirection::Ltr);
        if !text.is_empty() {
            prop_assert!(layout.total_cells() > 0);
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// Width computation fuzz
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn fuzz_width_no_panic(text in arb_fuzzy_text(200)) {
        let width = display_width(&text);
        let mut cache = WidthCache::new(100);
        let cached = cache.get_or_compute(&text);
        prop_assert_eq!(width, cached);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// Wrap fuzz
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn fuzz_wrap_no_panic(
        text in arb_fuzzy_text(100),
        width in 1usize..=200,
    ) {
        let _word = wrap_text(&text, width, WrapMode::Word);
        let _char = wrap_text(&text, width, WrapMode::Char);
        let _wordchar = wrap_text(&text, width, WrapMode::WordChar);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// Hyphenation fuzz
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn fuzz_hyphenation_no_panic(text in arb_fuzzy_text(100)) {
        let dict = english_dict_mini();
        for word in text.split_whitespace() {
            let points = dict.hyphenate(word);
            let _penalties = break_penalties(&points);
            for bp in &points {
                prop_assert!(bp.offset <= word.len());
            }
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// Regression cases from libfuzzer (add concrete inputs here)
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn regression_empty() {
    let map = ClusterMap::from_text("");
    assert_eq!(map.total_cells(), 0);
    let layout = ShapedLineLayout::from_text("");
    assert_eq!(layout.total_cells(), 0);
    assert_eq!(display_width(""), 0);
}

#[test]
fn regression_null_bytes() {
    let text = "\0\0\0";
    let _map = ClusterMap::from_text(text);
    let _layout = ShapedLineLayout::from_text(text);
    let _width = display_width(text);
}

#[test]
fn regression_mixed_wide_combining() {
    let text = "\u{4e16}\u{0301}\u{754c}\u{0300}abc";
    let map = ClusterMap::from_text(text);
    assert!(map.total_cells() > 0);
    let layout = ShapedLineLayout::from_text(text);
    assert_eq!(layout.total_cells(), layout.placements().len());
}

#[test]
fn regression_long_combining_sequence() {
    // Multiple combining marks on a single base character.
    let text = "a\u{0301}\u{0302}\u{0303}\u{0304}\u{0305}";
    let map = ClusterMap::from_text(text);
    let entries = map.entries();
    // Should be one grapheme cluster.
    assert_eq!(entries.len(), 1);
}

#[test]
fn regression_bidi_marks() {
    // LRM, RLM, and other bidi control characters.
    let text = "\u{200e}hello\u{200f}world";
    let _map = ClusterMap::from_text(text);
    let _layout = ShapedLineLayout::from_text(text);
}

#[test]
fn regression_emoji_zwj() {
    // ZWJ emoji sequence: family emoji.
    let text = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
    let _map = ClusterMap::from_text(text);
    let _layout = ShapedLineLayout::from_text(text);
}
