//! Property-based invariant tests for text search utilities.
//!
//! These tests verify structural invariants of search functions that must hold
//! for any valid haystack/needle combination:
//!
//! 1. Result ranges are within haystack bounds.
//! 2. Result ranges start at char boundaries.
//! 3. Result text equals the needle (exact search).
//! 4. Non-overlapping results don't overlap.
//! 5. Overlapping search finds >= non-overlapping search.
//! 6. Empty needle always returns empty.
//! 7. Needle longer than haystack returns empty.
//! 8. ASCII case-insensitive is superset of exact for lowercase.
//! 9. Exact search finds all manually constructed occurrences.
//! 10. Results are sorted by position.
//! 11. No panics on arbitrary string inputs.

use ftui_text::search::{search_ascii_case_insensitive, search_exact, search_exact_overlapping};
use proptest::prelude::*;

// ── Helpers ─────────────────────────────────────────────────────────────

fn ascii_string(max_len: usize) -> impl Strategy<Value = String> {
    proptest::collection::vec(0x20u8..=0x7E, 0..max_len)
        .prop_map(|bytes| String::from_utf8(bytes).unwrap())
}

fn ascii_word() -> impl Strategy<Value = String> {
    proptest::collection::vec(b'a'..=b'z', 1..=8)
        .prop_map(|bytes| String::from_utf8(bytes).unwrap())
}

// ═════════════════════════════════════════════════════════════════════════
// 1. Result ranges are within haystack bounds
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn exact_results_in_bounds(
        haystack in ascii_string(200),
        needle in ascii_string(20),
    ) {
        let results = search_exact(&haystack, &needle);
        for r in &results {
            prop_assert!(
                r.range.start <= r.range.end,
                "Invalid range: start {} > end {}",
                r.range.start, r.range.end
            );
            prop_assert!(
                r.range.end <= haystack.len(),
                "Range end {} exceeds haystack len {}",
                r.range.end, haystack.len()
            );
        }
    }

    #[test]
    fn overlapping_results_in_bounds(
        haystack in ascii_string(200),
        needle in ascii_string(20),
    ) {
        let results = search_exact_overlapping(&haystack, &needle);
        for r in &results {
            prop_assert!(r.range.start <= r.range.end);
            prop_assert!(r.range.end <= haystack.len());
        }
    }

    #[test]
    fn ascii_ci_results_in_bounds(
        haystack in ascii_string(200),
        needle in ascii_string(20),
    ) {
        let results = search_ascii_case_insensitive(&haystack, &needle);
        for r in &results {
            prop_assert!(r.range.start <= r.range.end);
            prop_assert!(r.range.end <= haystack.len());
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. Result ranges are at char boundaries
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn exact_results_at_char_boundaries(
        haystack in ".*",
        needle in ".{0,10}",
    ) {
        let results = search_exact(&haystack, &needle);
        for r in &results {
            prop_assert!(
                haystack.is_char_boundary(r.range.start),
                "Start {} not at char boundary in {:?}",
                r.range.start, haystack
            );
            prop_assert!(
                haystack.is_char_boundary(r.range.end),
                "End {} not at char boundary in {:?}",
                r.range.end, haystack
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. Result text equals the needle (exact search)
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn exact_result_text_equals_needle(
        haystack in ascii_string(200),
        needle in ascii_string(20),
    ) {
        let results = search_exact(&haystack, &needle);
        for r in &results {
            let matched = r.text(&haystack);
            prop_assert_eq!(
                matched, &needle,
                "Result text {:?} != needle {:?}",
                matched, needle
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 4. Non-overlapping results don't overlap
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn exact_results_non_overlapping(
        haystack in ascii_string(200),
        needle in ascii_string(20),
    ) {
        let results = search_exact(&haystack, &needle);
        for window in results.windows(2) {
            prop_assert!(
                window[0].range.end <= window[1].range.start,
                "Overlap: [{:?}] and [{:?}]",
                window[0].range, window[1].range
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 5. Overlapping search finds >= non-overlapping
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn overlapping_superset_of_exact(
        haystack in ascii_string(100),
        needle in ascii_string(10),
    ) {
        let exact = search_exact(&haystack, &needle);
        let overlapping = search_exact_overlapping(&haystack, &needle);

        prop_assert!(
            overlapping.len() >= exact.len(),
            "Overlapping ({}) < exact ({})",
            overlapping.len(), exact.len()
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 6. Empty needle always returns empty
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn empty_needle_returns_empty(haystack in ascii_string(100)) {
        prop_assert!(search_exact(&haystack, "").is_empty());
        prop_assert!(search_exact_overlapping(&haystack, "").is_empty());
        prop_assert!(search_ascii_case_insensitive(&haystack, "").is_empty());
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 7. Needle longer than haystack returns empty
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn long_needle_returns_empty(
        haystack in ascii_string(10),
        extra in ascii_string(20),
    ) {
        // Needle = haystack + extra (always longer when extra is non-empty)
        if !extra.is_empty() {
            let needle = format!("{}{}", haystack, extra);
            prop_assert!(
                search_exact(&haystack, &needle).is_empty(),
                "Should not find {:?} in shorter {:?}",
                needle, haystack
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 8. ASCII case-insensitive is superset of exact (for lowercase)
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn ascii_ci_superset_of_exact_for_lowercase(
        haystack in ascii_string(100),
        needle in ascii_word(),
    ) {
        // For a lowercase-only needle, every exact match should also be
        // found by case-insensitive search.
        let exact = search_exact(&haystack, &needle);
        let ci = search_ascii_case_insensitive(&haystack, &needle);

        prop_assert!(
            ci.len() >= exact.len(),
            "Case-insensitive ({}) found fewer than exact ({}) for needle {:?}",
            ci.len(), exact.len(), needle
        );

        // Every exact match position should appear in ci results
        for er in &exact {
            let found = ci.iter().any(|cr| cr.range.start == er.range.start);
            prop_assert!(
                found,
                "Exact match at {} not found in case-insensitive results",
                er.range.start
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 9. Constructed occurrences are found
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn finds_constructed_occurrence(
        prefix in ascii_string(50),
        needle in ascii_word(),
        suffix in ascii_string(50),
    ) {
        let haystack = format!("{}{}{}", prefix, needle, suffix);
        let results = search_exact(&haystack, &needle);
        prop_assert!(
            !results.is_empty(),
            "Should find {:?} in constructed haystack {:?}",
            needle, haystack
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 10. Results are sorted by start position
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn exact_results_sorted(
        haystack in ascii_string(200),
        needle in ascii_string(10),
    ) {
        let results = search_exact(&haystack, &needle);
        for window in results.windows(2) {
            prop_assert!(
                window[0].range.start < window[1].range.start,
                "Results not sorted: {} >= {}",
                window[0].range.start, window[1].range.start
            );
        }
    }

    #[test]
    fn overlapping_results_sorted(
        haystack in ascii_string(200),
        needle in ascii_string(10),
    ) {
        let results = search_exact_overlapping(&haystack, &needle);
        for window in results.windows(2) {
            prop_assert!(
                window[0].range.start < window[1].range.start,
                "Overlapping results not sorted: {} >= {}",
                window[0].range.start, window[1].range.start
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 11. No panics on arbitrary inputs
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn no_panic_exact(haystack in ".*", needle in ".{0,20}") {
        let _ = search_exact(&haystack, &needle);
    }

    #[test]
    fn no_panic_overlapping(haystack in ".*", needle in ".{0,20}") {
        let _ = search_exact_overlapping(&haystack, &needle);
    }

    #[test]
    fn no_panic_ascii_ci(haystack in ".*", needle in ".{0,20}") {
        let _ = search_ascii_case_insensitive(&haystack, &needle);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 12. Determinism: same inputs always produce same results
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn exact_search_deterministic(
        haystack in ascii_string(100),
        needle in ascii_string(15),
    ) {
        let r1 = search_exact(&haystack, &needle);
        let r2 = search_exact(&haystack, &needle);
        prop_assert_eq!(r1, r2, "Exact search is non-deterministic");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 13. Overlapping results contain all exact match positions
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn overlapping_contains_all_exact_positions(
        haystack in ascii_string(100),
        needle in ascii_string(10),
    ) {
        let exact = search_exact(&haystack, &needle);
        let overlapping = search_exact_overlapping(&haystack, &needle);

        let overlap_starts: std::collections::HashSet<usize> =
            overlapping.iter().map(|r| r.range.start).collect();

        for er in &exact {
            prop_assert!(
                overlap_starts.contains(&er.range.start),
                "Exact match at {} missing from overlapping results",
                er.range.start
            );
        }
    }
}
