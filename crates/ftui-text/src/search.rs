#![forbid(unsafe_code)]

//! Unicode-aware text search utilities.
//!
//! Feature-gated behind the `normalization` feature flag for normalization-aware
//! and case-folded search. Basic exact search is always available.
//!
//! # Policy-Aware Search
//!
//! When the `normalization` feature is enabled, [`search_with_policy`] provides a
//! unified entry point that combines configurable normalization form, case folding,
//! and width measurement into a single [`SearchPolicy`]. Results include display
//! column offsets computed under the chosen [`WidthMode`].
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

use unicode_width::UnicodeWidthChar;

/// Unicode character width measurement mode for search results.
///
/// Controls how display column offsets are computed — in particular for East
/// Asian Ambiguous characters whose width differs between CJK and Western
/// terminals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WidthMode {
    /// Standard Unicode width: EA Ambiguous characters are single-width (1 cell).
    #[default]
    Standard,

    /// CJK-aware width: EA Ambiguous characters are double-width (2 cells).
    CjkAmbiguousWide,
}

impl WidthMode {
    /// Compute the terminal display width of a single Unicode scalar.
    #[inline]
    #[must_use]
    pub fn char_width(self, ch: char) -> usize {
        let w = match self {
            Self::Standard => UnicodeWidthChar::width(ch).unwrap_or(0),
            Self::CjkAmbiguousWide => UnicodeWidthChar::width_cjk(ch).unwrap_or(0),
        };
        w.min(2)
    }

    /// Compute the display width of a string by summing per-character widths.
    #[must_use]
    pub fn str_width(self, s: &str) -> usize {
        s.chars().map(|ch| self.char_width(ch)).sum()
    }
}

/// Compute the display column at a byte offset in a string.
///
/// Returns the sum of character widths for all characters before the given byte
/// offset, using the specified width mode.
///
/// # Panics
/// Panics if `byte_offset` is not at a char boundary in `s`.
#[must_use]
pub fn display_col_at(s: &str, byte_offset: usize, mode: WidthMode) -> usize {
    debug_assert!(s.is_char_boundary(byte_offset));
    s[..byte_offset].chars().map(|ch| mode.char_width(ch)).sum()
}

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
    // Track both start and end byte offsets for each normalized byte so
    // matches that land inside a grapheme expansion still map to a full
    // grapheme range in the original string.
    let mut norm_start_map: Vec<usize> = Vec::new();
    let mut norm_end_map: Vec<usize> = Vec::new();
    let mut normalized = String::new();

    for (orig_byte, grapheme) in haystack.grapheme_indices(true) {
        let chunk = crate::normalization::normalize_for_search(grapheme);
        if chunk.is_empty() {
            continue;
        }
        let orig_end = orig_byte + grapheme.len();
        for _ in chunk.bytes() {
            norm_start_map.push(orig_byte);
            norm_end_map.push(orig_end);
        }
        normalized.push_str(&chunk);
    }
    if normalized.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();
    let mut start = 0;
    while let Some(pos) = normalized[start..].find(&needle_norm) {
        let norm_start = start + pos;
        let norm_end = norm_start + needle_norm.len();

        let orig_start = norm_start_map
            .get(norm_start)
            .copied()
            .unwrap_or(haystack.len());
        let orig_end = if norm_end == 0 {
            orig_start
        } else {
            norm_end_map
                .get(norm_end - 1)
                .copied()
                .unwrap_or(haystack.len())
        };

        // Avoid duplicate ranges when a single grapheme expands into multiple
        // normalized bytes (e.g., fullwidth "Ａ" -> "a" under NFKC).
        if results
            .last()
            .is_some_and(|r: &SearchResult| r.range.start == orig_start && r.range.end == orig_end)
        {
            start = norm_end;
            continue;
        }

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
    let mut norm_start_map: Vec<usize> = Vec::new();
    let mut norm_end_map: Vec<usize> = Vec::new();
    let mut normalized = String::new();

    for (orig_byte, grapheme) in haystack.grapheme_indices(true) {
        let chunk = normalize(grapheme, form);
        if chunk.is_empty() {
            continue;
        }
        let orig_end = orig_byte + grapheme.len();
        for _ in chunk.bytes() {
            norm_start_map.push(orig_byte);
            norm_end_map.push(orig_end);
        }
        normalized.push_str(&chunk);
    }
    if normalized.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();
    let mut start = 0;
    while let Some(pos) = normalized[start..].find(&needle_norm) {
        let norm_start = start + pos;
        let norm_end = norm_start + needle_norm.len();

        let orig_start = norm_start_map
            .get(norm_start)
            .copied()
            .unwrap_or(haystack.len());
        let orig_end = if norm_end == 0 {
            orig_start
        } else {
            norm_end_map
                .get(norm_end - 1)
                .copied()
                .unwrap_or(haystack.len())
        };

        if results
            .last()
            .is_some_and(|r: &SearchResult| r.range.start == orig_start && r.range.end == orig_end)
        {
            start = norm_end;
            continue;
        }

        results.push(SearchResult::new(orig_start, orig_end));
        start = norm_end;
    }
    results
}

// =============================================================================
// Policy-aware search
// =============================================================================

/// Search policy controlling normalization, case folding, and width measurement.
///
/// Bundles the three knobs a terminal search feature needs:
/// 1. Which Unicode normalization form to apply before matching.
/// 2. Whether to fold case (Unicode-aware lowercase after normalization).
/// 3. Which width mode to use for computing display column offsets in results.
///
/// # Presets
///
/// | Preset | Norm | Case | Width |
/// |--------|------|------|-------|
/// | [`STANDARD`](Self::STANDARD) | NFKC | insensitive | Standard |
/// | [`CJK`](Self::CJK) | NFKC | insensitive | CjkAmbiguousWide |
/// | [`EXACT_NFC`](Self::EXACT_NFC) | NFC | sensitive | Standard |
#[cfg(feature = "normalization")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchPolicy {
    /// Normalization form applied to both haystack and needle before matching.
    pub norm_form: crate::normalization::NormForm,
    /// If `true`, apply Unicode case folding (lowercase) after normalization.
    pub case_insensitive: bool,
    /// Width mode for computing [`PolicySearchResult::col_start`] / [`PolicySearchResult::col_end`].
    pub width_mode: WidthMode,
}

#[cfg(feature = "normalization")]
impl SearchPolicy {
    /// Default Western terminal preset: NFKC + case-insensitive + standard width.
    pub const STANDARD: Self = Self {
        norm_form: crate::normalization::NormForm::Nfkc,
        case_insensitive: true,
        width_mode: WidthMode::Standard,
    };

    /// CJK terminal preset: NFKC + case-insensitive + CJK ambiguous-wide.
    pub const CJK: Self = Self {
        norm_form: crate::normalization::NormForm::Nfkc,
        case_insensitive: true,
        width_mode: WidthMode::CjkAmbiguousWide,
    };

    /// Exact NFC matching: NFC + case-sensitive + standard width.
    pub const EXACT_NFC: Self = Self {
        norm_form: crate::normalization::NormForm::Nfc,
        case_insensitive: false,
        width_mode: WidthMode::Standard,
    };
}

/// A search result enriched with display-column offsets.
///
/// Extends [`SearchResult`] with terminal column positions computed under the
/// [`WidthMode`] specified by the [`SearchPolicy`].
#[cfg(feature = "normalization")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicySearchResult {
    /// Byte offset range of the match in the source string.
    pub range: std::ops::Range<usize>,
    /// Display column (0-indexed) where the match starts.
    pub col_start: usize,
    /// Display column (0-indexed) where the match ends (exclusive).
    pub col_end: usize,
}

#[cfg(feature = "normalization")]
impl PolicySearchResult {
    /// Extract the matched text from the source.
    #[must_use]
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.range.clone()]
    }

    /// Display width of the matched text (col_end - col_start).
    #[must_use]
    pub fn display_width(&self) -> usize {
        self.col_end - self.col_start
    }
}

/// Search with explicit policy controlling normalization, case folding, and width
/// measurement.
///
/// This is the unified entry point for policy-aware search. It:
/// 1. Normalizes both haystack and needle to [`SearchPolicy::norm_form`].
/// 2. Optionally applies Unicode case folding.
/// 3. Finds all non-overlapping matches.
/// 4. Computes display column offsets using [`SearchPolicy::width_mode`].
///
/// # Example
/// ```
/// use ftui_text::search::{SearchPolicy, search_with_policy};
///
/// let results = search_with_policy("Hello World", "hello", &SearchPolicy::STANDARD);
/// assert_eq!(results.len(), 1);
/// assert_eq!(results[0].col_start, 0);
/// assert_eq!(results[0].col_end, 5);
/// ```
#[cfg(feature = "normalization")]
#[must_use]
pub fn search_with_policy(
    haystack: &str,
    needle: &str,
    policy: &SearchPolicy,
) -> Vec<PolicySearchResult> {
    use crate::normalization::normalize;
    use unicode_segmentation::UnicodeSegmentation;

    if needle.is_empty() {
        return Vec::new();
    }

    // Normalize the needle.
    let needle_norm = if policy.case_insensitive {
        normalize(needle, policy.norm_form).to_lowercase()
    } else {
        normalize(needle, policy.norm_form)
    };
    if needle_norm.is_empty() {
        return Vec::new();
    }

    // Normalize haystack per grapheme cluster and build byte-offset mapping.
    let mut norm_start_map: Vec<usize> = Vec::new();
    let mut norm_end_map: Vec<usize> = Vec::new();
    let mut normalized = String::new();

    for (orig_byte, grapheme) in haystack.grapheme_indices(true) {
        let chunk = if policy.case_insensitive {
            normalize(grapheme, policy.norm_form).to_lowercase()
        } else {
            normalize(grapheme, policy.norm_form)
        };
        if chunk.is_empty() {
            continue;
        }
        let orig_end = orig_byte + grapheme.len();
        for _ in chunk.bytes() {
            norm_start_map.push(orig_byte);
            norm_end_map.push(orig_end);
        }
        normalized.push_str(&chunk);
    }
    if normalized.is_empty() {
        return Vec::new();
    }

    // Find matches in normalized text.
    let mut results = Vec::new();
    let mut start = 0;
    while let Some(pos) = normalized[start..].find(&needle_norm) {
        let norm_start = start + pos;
        let norm_end = norm_start + needle_norm.len();

        let orig_start = norm_start_map
            .get(norm_start)
            .copied()
            .unwrap_or(haystack.len());
        let orig_end = if norm_end == 0 {
            orig_start
        } else {
            norm_end_map
                .get(norm_end - 1)
                .copied()
                .unwrap_or(haystack.len())
        };

        // Deduplicate when a grapheme expansion produces duplicate ranges.
        if results.last().is_some_and(|r: &PolicySearchResult| {
            r.range.start == orig_start && r.range.end == orig_end
        }) {
            start = norm_end;
            continue;
        }

        let col_start = display_col_at(haystack, orig_start, policy.width_mode);
        let col_end = display_col_at(haystack, orig_end, policy.width_mode);

        results.push(PolicySearchResult {
            range: orig_start..orig_end,
            col_start,
            col_end,
        });
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
        // Case-insensitive search finds "Strasse" (literal match in haystack)
        // Note: ß does NOT fold to ss with to_lowercase(); this tests the literal match
        let results = search_case_insensitive("Straße Strasse", "strasse");
        assert!(
            !results.is_empty(),
            "Should find literal case-insensitive match"
        );
    }

    #[test]
    fn case_insensitive_expansion_range_maps_to_grapheme() {
        // Test that grapheme boundaries are preserved in results
        // Note: ß does NOT case-fold to ss (that would require Unicode case folding)
        let haystack = "STRAßE";
        let results = search_case_insensitive(haystack, "straße");
        assert_eq!(results.len(), 1);
        let result = &results[0];
        assert_eq!(result.text(haystack), "STRAßE");
        assert!(haystack.is_char_boundary(result.range.start));
        assert!(haystack.is_char_boundary(result.range.end));
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

// =============================================================================
// Policy-aware search tests
// =============================================================================

#[cfg(all(test, feature = "normalization"))]
mod policy_tests {
    use super::*;
    use crate::normalization::NormForm;

    // ── WidthMode basic tests ──────────────────────────────────────────

    #[test]
    fn width_mode_ascii_is_one() {
        for ch in ['a', 'Z', '0', ' ', '~'] {
            assert_eq!(WidthMode::Standard.char_width(ch), 1);
            assert_eq!(WidthMode::CjkAmbiguousWide.char_width(ch), 1);
        }
    }

    #[test]
    fn width_mode_cjk_ideograph_is_two() {
        for ch in ['中', '国', '字'] {
            assert_eq!(WidthMode::Standard.char_width(ch), 2);
            assert_eq!(WidthMode::CjkAmbiguousWide.char_width(ch), 2);
        }
    }

    #[test]
    fn width_mode_ea_ambiguous_differs() {
        // Box drawing: EA Ambiguous
        for ch in ['─', '│', '┌'] {
            assert_eq!(WidthMode::Standard.char_width(ch), 1, "Standard: {ch:?}");
            assert_eq!(
                WidthMode::CjkAmbiguousWide.char_width(ch),
                2,
                "CjkWide: {ch:?}"
            );
        }
        // Arrows: EA Ambiguous
        for ch in ['→', '←', '↑', '↓'] {
            assert_eq!(WidthMode::Standard.char_width(ch), 1);
            assert_eq!(WidthMode::CjkAmbiguousWide.char_width(ch), 2);
        }
        // Misc symbols: EA Ambiguous
        for ch in ['°', '×', '®'] {
            assert_eq!(WidthMode::Standard.char_width(ch), 1);
            assert_eq!(WidthMode::CjkAmbiguousWide.char_width(ch), 2);
        }
    }

    #[test]
    fn width_mode_combining_marks_zero() {
        for ch in ['\u{0300}', '\u{0301}', '\u{0302}'] {
            assert_eq!(WidthMode::Standard.char_width(ch), 0);
            assert_eq!(WidthMode::CjkAmbiguousWide.char_width(ch), 0);
        }
    }

    #[test]
    fn width_mode_str_width() {
        assert_eq!(WidthMode::Standard.str_width("hello"), 5);
        assert_eq!(WidthMode::Standard.str_width("中国"), 4);
        assert_eq!(WidthMode::CjkAmbiguousWide.str_width("hello"), 5);
        // Arrow + space + CJK: 2+1+2 = 5 in CJK mode
        assert_eq!(WidthMode::CjkAmbiguousWide.str_width("→ 中"), 5);
        assert_eq!(WidthMode::Standard.str_width("→ 中"), 4); // 1+1+2
    }

    #[test]
    fn width_mode_default_is_standard() {
        assert_eq!(WidthMode::default(), WidthMode::Standard);
    }

    // ── display_col_at tests ───────────────────────────────────────────

    #[test]
    fn display_col_at_ascii() {
        let s = "hello world";
        assert_eq!(display_col_at(s, 0, WidthMode::Standard), 0);
        assert_eq!(display_col_at(s, 5, WidthMode::Standard), 5);
        assert_eq!(display_col_at(s, 11, WidthMode::Standard), 11);
    }

    #[test]
    fn display_col_at_cjk() {
        let s = "你好world";
        // "你" = 3 bytes, "好" = 3 bytes, each 2 cells wide
        assert_eq!(display_col_at(s, 0, WidthMode::Standard), 0);
        assert_eq!(display_col_at(s, 3, WidthMode::Standard), 2); // after 你
        assert_eq!(display_col_at(s, 6, WidthMode::Standard), 4); // after 好
        assert_eq!(display_col_at(s, 11, WidthMode::Standard), 9); // after world
    }

    #[test]
    fn display_col_at_ea_ambiguous_differs() {
        let s = "─→text";
        // ─ is 3 bytes, → is 3 bytes
        let after_box = 3; // byte offset after ─
        let after_arrow = 6; // byte offset after →
        assert_eq!(display_col_at(s, after_box, WidthMode::Standard), 1);
        assert_eq!(display_col_at(s, after_box, WidthMode::CjkAmbiguousWide), 2);
        assert_eq!(display_col_at(s, after_arrow, WidthMode::Standard), 2);
        assert_eq!(
            display_col_at(s, after_arrow, WidthMode::CjkAmbiguousWide),
            4
        );
    }

    #[test]
    fn display_col_at_combining_marks() {
        // "e" + combining acute: the combining mark has 0 width
        let s = "e\u{0301}x";
        // e = 1 byte, combining = 2 bytes, x = 1 byte
        assert_eq!(display_col_at(s, 0, WidthMode::Standard), 0);
        assert_eq!(display_col_at(s, 1, WidthMode::Standard), 1); // after e
        assert_eq!(display_col_at(s, 3, WidthMode::Standard), 1); // after combining
        assert_eq!(display_col_at(s, 4, WidthMode::Standard), 2); // after x
    }

    // ── SearchPolicy preset tests ──────────────────────────────────────

    #[test]
    fn policy_standard_preset() {
        let p = SearchPolicy::STANDARD;
        assert_eq!(p.norm_form, NormForm::Nfkc);
        assert!(p.case_insensitive);
        assert_eq!(p.width_mode, WidthMode::Standard);
    }

    #[test]
    fn policy_cjk_preset() {
        let p = SearchPolicy::CJK;
        assert_eq!(p.norm_form, NormForm::Nfkc);
        assert!(p.case_insensitive);
        assert_eq!(p.width_mode, WidthMode::CjkAmbiguousWide);
    }

    #[test]
    fn policy_exact_nfc_preset() {
        let p = SearchPolicy::EXACT_NFC;
        assert_eq!(p.norm_form, NormForm::Nfc);
        assert!(!p.case_insensitive);
        assert_eq!(p.width_mode, WidthMode::Standard);
    }

    // ── search_with_policy: basic matching ─────────────────────────────

    #[test]
    fn policy_search_basic_ascii() {
        let results = search_with_policy("hello world", "hello", &SearchPolicy::STANDARD);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].range, 0..5);
        assert_eq!(results[0].col_start, 0);
        assert_eq!(results[0].col_end, 5);
    }

    #[test]
    fn policy_search_case_insensitive() {
        let results = search_with_policy("Hello WORLD hello", "hello", &SearchPolicy::STANDARD);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].text("Hello WORLD hello"), "Hello");
        assert_eq!(results[1].text("Hello WORLD hello"), "hello");
    }

    #[test]
    fn policy_search_case_sensitive() {
        let results = search_with_policy("Hello hello", "hello", &SearchPolicy::EXACT_NFC);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].range.start, 6);
    }

    #[test]
    fn policy_search_empty_needle() {
        let results = search_with_policy("hello", "", &SearchPolicy::STANDARD);
        assert!(results.is_empty());
    }

    #[test]
    fn policy_search_empty_haystack() {
        let results = search_with_policy("", "hello", &SearchPolicy::STANDARD);
        assert!(results.is_empty());
    }

    #[test]
    fn policy_search_no_match() {
        let results = search_with_policy("hello", "world", &SearchPolicy::STANDARD);
        assert!(results.is_empty());
    }

    // ── search_with_policy: normalization alignment ────────────────────

    #[test]
    fn policy_search_composed_vs_decomposed() {
        // Composed é in haystack, decomposed e+acute in needle
        let haystack = "caf\u{00E9}";
        let needle = "caf\u{0065}\u{0301}";
        let results = search_with_policy(haystack, needle, &SearchPolicy::EXACT_NFC);
        assert_eq!(
            results.len(),
            1,
            "NFC should equate composed and decomposed"
        );
    }

    #[test]
    fn policy_search_fullwidth_nfkc() {
        // Fullwidth "HELLO" matches "hello" under NFKC + case-insensitive
        let haystack = "\u{FF28}\u{FF25}\u{FF2C}\u{FF2C}\u{FF2F}";
        let results = search_with_policy(haystack, "hello", &SearchPolicy::STANDARD);
        assert!(!results.is_empty(), "Fullwidth should match via NFKC");
    }

    #[test]
    fn policy_search_nfc_does_not_match_compatibility() {
        // fi ligature (U+FB01) should NOT match "fi" under NFC (only NFKC decomposes it)
        let haystack = "\u{FB01}le";
        let results = search_with_policy(haystack, "file", &SearchPolicy::EXACT_NFC);
        assert!(
            results.is_empty(),
            "NFC should not decompose compatibility chars"
        );
    }

    #[test]
    fn policy_search_nfkc_matches_compatibility() {
        // fi ligature should match "file" under NFKC
        let haystack = "\u{FB01}le";
        let results = search_with_policy(haystack, "file", &SearchPolicy::STANDARD);
        assert!(!results.is_empty(), "NFKC should decompose fi ligature");
    }

    // ── search_with_policy: column offsets with CJK text ───────────────

    #[test]
    fn policy_search_cjk_column_offsets() {
        // "你好world你好" — search for "world"
        let haystack = "你好world你好";
        let results = search_with_policy(haystack, "world", &SearchPolicy::STANDARD);
        assert_eq!(results.len(), 1);
        // "你" = 2 cols, "好" = 2 cols → world starts at col 4
        assert_eq!(results[0].col_start, 4);
        assert_eq!(results[0].col_end, 9); // 4 + 5 = 9
    }

    #[test]
    fn policy_search_cjk_in_cjk() {
        // Search for CJK in CJK
        let haystack = "你好世界你好";
        let results = search_with_policy(haystack, "世界", &SearchPolicy::STANDARD);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].col_start, 4); // 你(2) + 好(2) = 4
        assert_eq!(results[0].col_end, 8); // 4 + 世(2) + 界(2) = 8
    }

    // ── search_with_policy: EA Ambiguous differs by width mode ─────────

    #[test]
    fn policy_search_ea_ambiguous_column_divergence() {
        // "→hello" — arrow is EA Ambiguous
        let haystack = "→hello";
        let standard = search_with_policy(haystack, "hello", &SearchPolicy::STANDARD);
        let cjk = search_with_policy(haystack, "hello", &SearchPolicy::CJK);

        assert_eq!(standard.len(), 1);
        assert_eq!(cjk.len(), 1);
        // Same byte range
        assert_eq!(standard[0].range, cjk[0].range);
        // Different column offsets
        assert_eq!(standard[0].col_start, 1); // → = 1 col in Standard
        assert_eq!(cjk[0].col_start, 2); // → = 2 cols in CJK
        assert_eq!(standard[0].col_end, 6);
        assert_eq!(cjk[0].col_end, 7);
    }

    #[test]
    fn policy_search_box_drawing_column_divergence() {
        // "──text" — box drawing chars are EA Ambiguous
        let haystack = "──text";
        let standard = search_with_policy(haystack, "text", &SearchPolicy::STANDARD);
        let cjk = search_with_policy(haystack, "text", &SearchPolicy::CJK);

        assert_eq!(standard[0].col_start, 2); // 2 × 1 = 2
        assert_eq!(cjk[0].col_start, 4); // 2 × 2 = 4
    }

    // ── search_with_policy: combining marks and column offsets ──────────

    #[test]
    fn policy_search_combining_mark_offsets() {
        // "café" with composed é — search for "fé"
        let haystack = "café";
        let results = search_with_policy(haystack, "fé", &SearchPolicy::STANDARD);
        assert_eq!(results.len(), 1);
        // c=1, a=1 → fé starts at col 2
        assert_eq!(results[0].col_start, 2);
        // f=1, é=1 → ends at col 4
        assert_eq!(results[0].col_end, 4);
    }

    #[test]
    fn policy_search_decomposed_combining_offsets() {
        // "cafe\u{0301}" — decomposed é
        let haystack = "cafe\u{0301}";
        let needle = "f\u{00E9}"; // composed fé
        let results = search_with_policy(haystack, needle, &SearchPolicy::EXACT_NFC);
        assert_eq!(results.len(), 1);
        // c=1, a=1 → starts at col 2
        assert_eq!(results[0].col_start, 2);
        // f=1, e=1, combining=0 → ends at col 4
        assert_eq!(results[0].col_end, 4);
    }

    // ── search_with_policy: display_width on PolicySearchResult ────────

    #[test]
    fn policy_result_display_width() {
        let haystack = "你好hello";
        let results = search_with_policy(haystack, "hello", &SearchPolicy::STANDARD);
        assert_eq!(results[0].display_width(), 5);
    }

    #[test]
    fn policy_result_display_width_cjk_match() {
        let haystack = "abc你好def";
        let results = search_with_policy(haystack, "你好", &SearchPolicy::STANDARD);
        assert_eq!(results[0].display_width(), 4); // 2 + 2
    }

    // ── search_with_policy: text extraction ────────────────────────────

    #[test]
    fn policy_result_text_extraction() {
        let haystack = "Hello World";
        let results = search_with_policy(haystack, "world", &SearchPolicy::STANDARD);
        assert_eq!(results[0].text(haystack), "World");
    }

    // ── search_with_policy: multiple matches ───────────────────────────

    #[test]
    fn policy_search_multiple_matches() {
        let haystack = "foo bar foo baz foo";
        let results = search_with_policy(haystack, "foo", &SearchPolicy::STANDARD);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].col_start, 0);
        assert_eq!(results[1].col_start, 8);
        assert_eq!(results[2].col_start, 16);
    }

    // ── search_with_policy: range validity invariant ───────────────────

    #[test]
    fn policy_search_ranges_always_valid() {
        let test_cases = [
            ("hello world", "o", SearchPolicy::STANDARD),
            ("CAFÉ café", "café", SearchPolicy::STANDARD),
            ("你好世界", "世", SearchPolicy::CJK),
            ("─→text", "text", SearchPolicy::CJK),
            ("\u{FB01}le", "file", SearchPolicy::STANDARD),
            ("e\u{0301}", "\u{00E9}", SearchPolicy::EXACT_NFC),
        ];
        for (haystack, needle, policy) in &test_cases {
            let results = search_with_policy(haystack, needle, policy);
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
                assert!(
                    r.col_start <= r.col_end,
                    "col_start > col_end for '{needle}' in '{haystack}'"
                );
            }
        }
    }

    // ── search_with_policy: column monotonicity ────────────────────────

    #[test]
    fn policy_search_columns_monotonically_increasing() {
        let haystack = "aa bb aa cc aa";
        let results = search_with_policy(haystack, "aa", &SearchPolicy::STANDARD);
        assert_eq!(results.len(), 3);
        for w in results.windows(2) {
            assert!(
                w[0].col_end <= w[1].col_start,
                "Non-overlapping matches should have monotonically increasing columns"
            );
        }
    }

    // ── search_with_policy: custom policy construction ─────────────────

    #[test]
    fn policy_custom_nfd_case_sensitive() {
        let policy = SearchPolicy {
            norm_form: NormForm::Nfd,
            case_insensitive: false,
            width_mode: WidthMode::Standard,
        };
        // NFD decomposes é → e+combining acute
        let haystack = "\u{00E9}"; // composed é
        let needle = "e\u{0301}"; // decomposed
        let results = search_with_policy(haystack, needle, &policy);
        assert_eq!(results.len(), 1, "NFD should match decomposed forms");
    }

    #[test]
    fn policy_custom_nfkd_case_insensitive() {
        let policy = SearchPolicy {
            norm_form: NormForm::Nfkd,
            case_insensitive: true,
            width_mode: WidthMode::CjkAmbiguousWide,
        };
        // fi ligature should match "FI" under NFKD + case-insensitive
        let haystack = "\u{FB01}";
        let results = search_with_policy(haystack, "FI", &policy);
        assert!(!results.is_empty(), "NFKD + CI should match fi ligature");
    }

    // ── search_with_policy: consistency with existing functions ─────────

    #[test]
    fn policy_search_agrees_with_search_case_insensitive() {
        let test_cases = [
            ("Hello World HELLO", "hello"),
            ("CAFÉ café Café", "café"),
            ("\u{FF28}\u{FF25}\u{FF2C}\u{FF2C}\u{FF2F}", "hello"),
        ];
        for (haystack, needle) in &test_cases {
            let old = search_case_insensitive(haystack, needle);
            let new = search_with_policy(haystack, needle, &SearchPolicy::STANDARD);
            assert_eq!(
                old.len(),
                new.len(),
                "Match count mismatch for '{needle}' in '{haystack}'"
            );
            for (o, n) in old.iter().zip(new.iter()) {
                assert_eq!(
                    o.range, n.range,
                    "Byte range mismatch for '{needle}' in '{haystack}'"
                );
            }
        }
    }

    #[test]
    fn policy_search_agrees_with_search_normalized() {
        let haystack = "caf\u{0065}\u{0301} résumé";
        let needle = "caf\u{00E9}";
        let old = search_normalized(haystack, needle, NormForm::Nfc);
        let new = search_with_policy(haystack, needle, &SearchPolicy::EXACT_NFC);
        assert_eq!(old.len(), new.len());
        for (o, n) in old.iter().zip(new.iter()) {
            assert_eq!(o.range, n.range);
        }
    }
}
