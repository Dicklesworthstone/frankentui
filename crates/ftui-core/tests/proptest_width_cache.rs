//! Property test: the per-thread grapheme width cache in
//! `ftui_core::text_width` never changes a measurement.
//!
//! For arbitrary Unicode text, the cached `display_width` must equal the sum of
//! `grapheme_width_uncached` over the same clusters, on the first call and on
//! every repeat (when the answer comes from the cache). A hash collision or a
//! stale entry would show up here as a width mismatch.

use ftui_core::text_width::{clear_width_cache, display_width, grapheme_width_uncached};
use proptest::prelude::*;
use unicode_segmentation::UnicodeSegmentation;

fn uncached_sum(text: &str) -> usize {
    text.graphemes(true).map(grapheme_width_uncached).sum()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn cached_display_width_equals_uncached_sum(text in "\\PC{0,40}") {
        let expected = uncached_sum(&text);
        prop_assert_eq!(display_width(&text), expected);
        prop_assert_eq!(display_width(&text), expected, "repeat lookup (cache hit)");
    }

    #[test]
    fn cache_survives_clear_between_lookups(text in "[\\u{4e00}-\\u{9fff}\\u{1f600}-\\u{1f64f}a-z ]{0,32}") {
        let expected = uncached_sum(&text);
        prop_assert_eq!(display_width(&text), expected);
        clear_width_cache();
        prop_assert_eq!(display_width(&text), expected);
    }
}
