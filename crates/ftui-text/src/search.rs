#![forbid(unsafe_code)]

//! Unicode-aware text search utilities.
//!
//! Feature-gated behind the `normalization` feature flag for normalization-aware
//! and case-folded search. Basic exact search is always available.
//!
//! # Example
//! ```
//! use ftui_text::search::{SearchResult, search_exact};
//!
//! let results = search_exact("hello world hello", "hello");
//! assert_eq!(results.len(), 2);
//! assert_eq!(results[0].range, 0..5);
//! assert_eq!(results[1].range, 12..17);
//! ```

/// A single search match with its byte range in the source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    /// Byte offset range of the match in the source string.
    pub range: std::ops::Range<usize>,
}

impl SearchResult {
    /// Create a new search result.
    #[must_use]
    pub fn new(start: usize, end: usize) -> Self {
        Self { range: start..end }
    }

    /// Extract the matched text from the source.
    #[must_use]
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.range.clone()]
    }
}

/// Find all exact substring matches (byte-level, no normalization).
///
/// Returns non-overlapping matches from left to right.
#[must_use]
pub fn search_exact(haystack: &str, needle: &str) -> Vec<SearchResult> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut results = Vec::new();
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        let abs_pos = start + pos;
        results.push(SearchResult::new(abs_pos, abs_pos + needle.len()));
        start = abs_pos + needle.len();
    }
    results
}

/// Find all exact substring matches, allowing overlapping results.
///
/// Advances by one byte after each match start.
#[must_use]
pub fn search_exact_overlapping(haystack: &str, needle: &str) -> Vec<SearchResult> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut results = Vec::new();
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        let abs_pos = start + pos;
        results.push(SearchResult::new(abs_pos, abs_pos + needle.len()));
        // Advance by one char (not byte) to handle multi-byte chars correctly
        start = abs_pos + 1;
        // Ensure we're at a char boundary
        while start < haystack.len() && !haystack.is_char_boundary(start) {
            start += 1;
        }
    }
    results
}

/// Case-insensitive search using simple ASCII lowering.
///
/// For full Unicode case folding, use [`search_case_insensitive`] with the
/// `normalization` feature enabled.
#[must_use]
pub fn search_ascii_case_insensitive(haystack: &str, needle: &str) -> Vec<SearchResult> {
    if needle.is_empty() {
        return Vec::new();
    }
    let haystack_lower = haystack.to_ascii_lowercase();
    let needle_lower = needle.to_ascii_lowercase();
    let mut results = Vec::new();
    let mut start = 0;
    while let Some(pos) = haystack_lower[start..].find(&needle_lower) {
        let abs_pos = start + pos;
        results.push(SearchResult::new(abs_pos, abs_pos + needle.len()));
        start = abs_pos + needle.len();
    }
    results
}

/// Case-insensitive search using full Unicode case folding.
///
/// Uses NFKC normalization + lowercase for both haystack and needle,
/// then maps result positions back to the original string.
#[cfg(feature = "normalization")]
#[must_use]
pub fn search_case_insensitive(haystack: &str, needle: &str) -> Vec<SearchResult> {
    if needle.is_empty() {
        return Vec::new();
    }
    let needle_norm = crate::normalization::normalize_for_search(needle);
    if needle_norm.is_empty() {
        return Vec::new();
    }

    use unicode_segmentation::UnicodeSegmentation;

    // Build mapping using grapheme clusters for correct normalization boundaries.
    let mut norm_to_orig: Vec<usize> = Vec::new();
    let mut normalized = String::new();

    for (orig_byte, grapheme) in haystack.grapheme_indices(true) {
        let chunk = crate::normalization::normalize_for_search(grapheme);
        for _ in chunk.bytes() {
            norm_to_orig.push(orig_byte);
        }
        normalized.push_str(&chunk);
    }
    norm_to_orig.push(haystack.len());

    let mut results = Vec::new();
    let mut start = 0;
    while let Some(pos) = normalized[start..].find(&needle_norm) {
        let norm_start = start + pos;
        let norm_end = norm_start + needle_norm.len();

        let orig_start = norm_to_orig
            .get(norm_start)
            .copied()
            .unwrap_or(haystack.len());
        let orig_end = norm_to_orig
            .get(norm_end)
            .copied()
            .unwrap_or(haystack.len());

        results.push(SearchResult::new(orig_start, orig_end));
        start = norm_end;
    }
    results
}

/// Normalization-aware search: treats composed and decomposed forms as equal.
///
/// Normalizes both haystack and needle to the given form before searching,
/// then maps results back to original string positions using grapheme clusters.
#[cfg(feature = "normalization")]
#[must_use]
pub fn search_normalized(
    haystack: &str,
    needle: &str,
    form: crate::normalization::NormForm,
) -> Vec<SearchResult> {
    use crate::normalization::normalize;
    use unicode_segmentation::UnicodeSegmentation;

    if needle.is_empty() {
        return Vec::new();
    }
    let needle_norm = normalize(needle, form);
    if needle_norm.is_empty() {
        return Vec::new();
    }

    // Normalize per grapheme cluster to maintain position tracking.
    // Grapheme clusters are the correct unit because normalization
    // operates within grapheme boundaries.
    let mut norm_to_orig: Vec<usize> = Vec::new();
    let mut normalized = String::new();

    for (orig_byte, grapheme) in haystack.grapheme_indices(true) {
        let chunk = normalize(grapheme, form);
        for _ in chunk.bytes() {
            norm_to_orig.push(orig_byte);
        }
        normalized.push_str(&chunk);
    }
    norm_to_orig.push(haystack.len());

    let mut results = Vec::new();
    let mut start = 0;
    while let Some(pos) = normalized[start..].find(&needle_norm) {
        let norm_start = start + pos;
        let norm_end = norm_start + needle_norm.len();

        let orig_start = norm_to_orig
            .get(norm_start)
            .copied()
            .unwrap_or(haystack.len());
        let orig_end = norm_to_orig
            .get(norm_end)
            .copied()
            .unwrap_or(haystack.len());

        results.push(SearchResult::new(orig_start, orig_end));
        start = norm_end;
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==========================================================
    // Exact search
    // ==========================================================

    #[test]
    fn exact_basic() {
        let results = search_exact("hello world hello", "hello");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].range, 0..5);
        assert_eq!(results[1].range, 12..17);
    }

    #[test]
    fn exact_no_match() {
        let results = search_exact("hello world", "xyz");
        assert!(results.is_empty());
    }

    #[test]
    fn exact_empty_needle() {
        let results = search_exact("hello", "");
        assert!(results.is_empty());
    }

    #[test]
    fn exact_empty_haystack() {
        let results = search_exact("", "hello");
        assert!(results.is_empty());
    }

    #[test]
    fn exact_needle_equals_haystack() {
        let results = search_exact("hello", "hello");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].range, 0..5);
    }

    #[test]
    fn exact_needle_longer() {
        let results = search_exact("hi", "hello");
        assert!(results.is_empty());
    }

    #[test]
    fn exact_adjacent_matches() {
        let results = search_exact("aaa", "a");
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn exact_text_extraction() {
        let haystack = "foo bar baz";
        let results = search_exact(haystack, "bar");
        assert_eq!(results[0].text(haystack), "bar");
    }

    #[test]
    fn exact_unicode() {
        let results = search_exact("café résumé café", "café");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn exact_cjk() {
        let results = search_exact("你好世界你好", "你好");
        assert_eq!(results.len(), 2);
    }

    // ==========================================================
    // Overlapping search
    // ==========================================================

    #[test]
    fn overlapping_basic() {
        let results = search_exact_overlapping("aaa", "aa");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].range, 0..2);
        assert_eq!(results[1].range, 1..3);
    }

    #[test]
    fn overlapping_no_overlap() {
        let results = search_exact_overlapping("abcabc", "abc");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn overlapping_empty_needle() {
        let results = search_exact_overlapping("abc", "");
        assert!(results.is_empty());
    }

    // ==========================================================
    // ASCII case-insensitive search
    // ==========================================================

    #[test]
    fn ascii_ci_basic() {
        let results = search_ascii_case_insensitive("Hello World HELLO", "hello");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn ascii_ci_mixed_case() {
        let results = search_ascii_case_insensitive("FoO BaR fOo", "foo");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn ascii_ci_no_match() {
        let results = search_ascii_case_insensitive("hello", "xyz");
        assert!(results.is_empty());
    }

    // ==========================================================
    // Valid range property tests
    // ==========================================================

    #[test]
    fn results_have_valid_ranges() {
        let test_cases = [
            ("hello world", "o"),
            ("aaaa", "aa"),
            ("", "x"),
            ("x", ""),
            ("café", "é"),
            ("🌍 world 🌍", "🌍"),
        ];
        for (haystack, needle) in test_cases {
            let results = search_exact(haystack, needle);
            for r in &results {
                assert!(
                    r.range.start <= r.range.end,
                    "Invalid range for '{needle}' in '{haystack}'"
                );
                assert!(
                    r.range.end <= haystack.len(),
                    "Out of bounds for '{needle}' in '{haystack}'"
                );
                assert!(
                    haystack.is_char_boundary(r.range.start),
                    "Not char boundary at start"
                );
                assert!(
                    haystack.is_char_boundary(r.range.end),
                    "Not char boundary at end"
                );
            }
        }
    }

    #[test]
    fn emoji_search() {
        let results = search_exact("hello 🌍 world 🌍 end", "🌍");
        assert_eq!(results.len(), 2);
        for r in &results {
            assert_eq!(&"hello 🌍 world 🌍 end"[r.range.clone()], "🌍");
        }
    }
}

#[cfg(all(test, feature = "normalization"))]
mod normalization_tests {
    use super::*;

    #[test]
    fn case_insensitive_unicode() {
        // German sharp-s: ß should match ss in case-folded search
        let results = search_case_insensitive("Straße Strasse", "strasse");
        assert!(!results.is_empty(), "Should find case-folded match");
    }

    #[test]
    fn case_insensitive_accented() {
        let results = search_case_insensitive("CAFÉ café Café", "café");
        // All three should match
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn case_insensitive_empty() {
        let results = search_case_insensitive("hello", "");
        assert!(results.is_empty());
    }

    #[test]
    fn case_insensitive_fullwidth() {
        // Fullwidth "HELLO" should match "hello" under NFKC normalization
        let results = search_case_insensitive("\u{FF28}\u{FF25}\u{FF2C}\u{FF2C}\u{FF2F}", "hello");
        assert!(!results.is_empty(), "Fullwidth should match via NFKC");
    }

    #[test]
    fn normalized_composed_vs_decomposed() {
        use crate::normalization::NormForm;
        // Search for composed é in text with decomposed e+combining acute
        let haystack = "caf\u{0065}\u{0301}"; // e + combining acute
        let needle = "caf\u{00E9}"; // precomposed é
        let results = search_normalized(haystack, needle, NormForm::Nfc);
        assert_eq!(results.len(), 1, "Should find NFC-equivalent match");
    }

    #[test]
    fn normalized_no_false_positive() {
        use crate::normalization::NormForm;
        let results = search_normalized("hello", "world", NormForm::Nfc);
        assert!(results.is_empty());
    }

    #[test]
    fn normalized_result_ranges_valid() {
        use crate::normalization::NormForm;
        let haystack = "café résumé café";
        let needle = "café";
        let results = search_normalized(haystack, needle, NormForm::Nfc);
        for r in &results {
            assert!(r.range.start <= r.range.end);
            assert!(r.range.end <= haystack.len());
            assert!(haystack.is_char_boundary(r.range.start));
            assert!(haystack.is_char_boundary(r.range.end));
        }
    }

    #[test]
    fn case_insensitive_result_ranges_valid() {
        let haystack = "Hello WORLD hello";
        let results = search_case_insensitive(haystack, "hello");
        for r in &results {
            assert!(r.range.start <= r.range.end);
            assert!(r.range.end <= haystack.len());
            assert!(haystack.is_char_boundary(r.range.start));
            assert!(haystack.is_char_boundary(r.range.end));
        }
    }
}
