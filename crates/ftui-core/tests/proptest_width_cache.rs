//! Property test: the per-thread grapheme width cache in
//! `ftui_core::text_width` never changes a measurement.
//!
//! For arbitrary Unicode text, the cached `display_width` must equal the sum of
//! `grapheme_width_uncached` over the same clusters, on the first call and on
//! every repeat. Direct cluster checks also exercise the cache when
//! `display_width` can take a whole-string fast path. Deterministic forced-hash
//! collision tests live beside the production lookup in `text_width`.

use ftui_core::text_width::{
    clear_width_cache, display_width, grapheme_width, grapheme_width_uncached,
};
use proptest::prelude::*;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::alloc::System;
use unicode_segmentation::UnicodeSegmentation;

#[global_allocator]
static ALLOCATOR: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[test]
fn width_cache_allocations_bounded() {
    // Isolate allocator counters from other properties and their runner.
    if std::env::var_os("FTUI_WIDTH_ALLOCATION_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "width_cache_allocations_bounded",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("FTUI_WIDTH_ALLOCATION_CHILD", "1")
            .output()
            .expect("execute isolated allocation measurement");
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        print!("{}", String::from_utf8_lossy(&output.stdout));
        assert!(output.status.success());
        return;
    }

    let clusters: Vec<String> = (0..10_000)
        .map(|index| {
            let base = char::from_u32(0x4e00 + index).unwrap();
            format!("{base}{}", "\u{0301}".repeat(61))
        })
        .collect();
    clear_width_cache();
    // Initialize TLS and the policy before measuring retained key storage.
    assert_eq!(grapheme_width("日"), 2);
    clear_width_cache();
    let region = Region::new(ALLOCATOR);
    for cluster in &clusters {
        assert_eq!(cluster.graphemes(true).count(), 1);
        assert_eq!(grapheme_width(cluster), grapheme_width_uncached(cluster));
        assert_eq!(grapheme_width(cluster), grapheme_width_uncached(cluster));
    }
    let scan = region.change();
    let retained_bytes = scan.bytes_allocated.saturating_sub(scan.bytes_deallocated);
    assert!(
        retained_bytes <= 1024 * 1024,
        "unbounded retained keys: {scan:?}"
    );
    if let Some(stats) = ftui_core::text_width::width_cache_stats() {
        assert_eq!(stats.misses, 10_000);
        assert_eq!(stats.small_size + stats.main_size, stats.capacity);
        assert!(scan.allocations >= 10_000, "measure actual key allocations");
    }
    let hit_region = Region::new(ALLOCATOR);
    for _ in 0..10_000 {
        assert_eq!(grapheme_width(clusters.last().unwrap()), 2);
    }
    let hits = hit_region.change();
    assert_eq!(hits.allocations, 0, "width hits must not allocate");
    assert_eq!(hits.reallocations, 0, "width hits must not reallocate");
    eprintln!("width-cache scan={scan:?} retained_bytes={retained_bytes} hits={hits:?}");
}

fn uncached_sum(text: &str) -> usize {
    text.graphemes(true).map(grapheme_width_uncached).sum()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn cached_graphemes_equal_uncached_on_first_and_repeat_lookup(text in "\\PC{0,40}") {
        for grapheme in text.graphemes(true) {
            let expected = grapheme_width_uncached(grapheme);
            prop_assert_eq!(grapheme_width(grapheme), expected);
            prop_assert_eq!(grapheme_width(grapheme), expected, "repeat of {:?}", grapheme);
        }
    }

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
