#![forbid(unsafe_code)]

//! Unicode normalization utilities (NFC, NFD, NFKC, NFKD).
//!
//! Feature-gated behind the `normalization` feature flag.
//! Does not affect default rendering behavior.
//!
//! # Example
//! ```
//! use ftui_text::normalization::{NormForm, normalize};
//!
//! // Composed form (NFC): e + combining acute -> é
//! let nfc = normalize("e\u{0301}", NormForm::Nfc);
//! assert_eq!(nfc, "\u{00E9}");
//!
//! // Decomposed form (NFD): é -> e + combining acute
//! let nfd = normalize("\u{00E9}", NormForm::Nfd);
//! assert_eq!(nfd, "e\u{0301}");
//! ```

use unicode_normalization::UnicodeNormalization;

/// Unicode normalization form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NormForm {
    /// Canonical Decomposition, followed by Canonical Composition (NFC).
    Nfc,
    /// Canonical Decomposition (NFD).
    Nfd,
    /// Compatibility Decomposition, followed by Canonical Composition (NFKC).
    Nfkc,
    /// Compatibility Decomposition (NFKD).
    Nfkd,
}

/// Normalize a string to the specified Unicode normalization form.
#[must_use]
pub fn normalize(s: &str, form: NormForm) -> String {
    match form {
        NormForm::Nfc => s.nfc().collect(),
        NormForm::Nfd => s.nfd().collect(),
        NormForm::Nfkc => s.nfkc().collect(),
        NormForm::Nfkd => s.nfkd().collect(),
    }
}

/// Check if a string is already in the specified normalization form.
///
/// Uses the quick-check algorithm when available (NFC/NFKC), falling back
/// to full comparison for NFD/NFKD.
#[must_use]
pub fn is_normalized(s: &str, form: NormForm) -> bool {
    match form {
        NormForm::Nfc => unicode_normalization::is_nfc(s),
        NormForm::Nfd => unicode_normalization::is_nfd(s),
        NormForm::Nfkc => unicode_normalization::is_nfkc(s),
        NormForm::Nfkd => unicode_normalization::is_nfkd(s),
    }
}

/// Normalize a string for case-insensitive comparison.
///
/// Uses NFKC normalization combined with Unicode case folding (lowercase).
/// Suitable for search matching where accented characters should be compared
/// in their canonical forms.
#[must_use]
pub fn normalize_for_search(s: &str) -> String {
    s.nfkc().collect::<String>().to_lowercase()
}

/// Check if two strings are equivalent under a given normalization form.
#[must_use]
pub fn eq_normalized(a: &str, b: &str, form: NormForm) -> bool {
    normalize(a, form) == normalize(b, form)
}

/// Streaming normalization iterator for NFC.
///
/// Returns an iterator that yields normalized characters without
/// allocating the full result string.
pub fn nfc_iter(s: &str) -> impl Iterator<Item = char> + '_ {
    s.nfc()
}

/// Streaming normalization iterator for NFD.
pub fn nfd_iter(s: &str) -> impl Iterator<Item = char> + '_ {
    s.nfd()
}

/// Streaming normalization iterator for NFKC.
pub fn nfkc_iter(s: &str) -> impl Iterator<Item = char> + '_ {
    s.nfkc()
}

/// Streaming normalization iterator for NFKD.
pub fn nfkd_iter(s: &str) -> impl Iterator<Item = char> + '_ {
    s.nfkd()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==========================================================
    // NFC tests
    // ==========================================================

    #[test]
    fn nfc_composes_combining_characters() {
        // U+0065 (e) + U+0301 (combining acute) -> U+00E9 (é)
        let input = "e\u{0301}";
        let result = normalize(input, NormForm::Nfc);
        assert_eq!(result, "\u{00E9}");
    }

    #[test]
    fn nfc_preserves_already_composed() {
        let input = "\u{00E9}"; // é
        let result = normalize(input, NormForm::Nfc);
        assert_eq!(result, "\u{00E9}");
    }

    #[test]
    fn nfc_multiple_combining() {
        // a + combining tilde + combining acute
        let input = "a\u{0303}\u{0301}";
        let result = normalize(input, NormForm::Nfc);
        // Should compose into a single character if possible
        assert!(!result.is_empty());
        // Verify it's a valid NFC string
        assert!(is_normalized(&result, NormForm::Nfc));
    }

    // ==========================================================
    // NFD tests
    // ==========================================================

    #[test]
    fn nfd_decomposes_precomposed() {
        // U+00E9 (é) -> U+0065 (e) + U+0301 (combining acute)
        let input = "\u{00E9}";
        let result = normalize(input, NormForm::Nfd);
        assert_eq!(result, "e\u{0301}");
    }

    #[test]
    fn nfd_preserves_ascii() {
        let input = "hello world";
        let result = normalize(input, NormForm::Nfd);
        assert_eq!(result, "hello world");
    }

    // ==========================================================
    // NFKC tests
    // ==========================================================

    #[test]
    fn nfkc_normalizes_compatibility() {
        // U+FB01 (fi ligature) -> "fi"
        let input = "\u{FB01}";
        let result = normalize(input, NormForm::Nfkc);
        assert_eq!(result, "fi");
    }

    #[test]
    fn nfkc_normalizes_fullwidth() {
        // U+FF21 (fullwidth A) -> U+0041 (A)
        let input = "\u{FF21}";
        let result = normalize(input, NormForm::Nfkc);
        assert_eq!(result, "A");
    }

    #[test]
    fn nfkc_normalizes_superscript() {
        // U+00B2 (superscript 2) -> "2"
        let input = "\u{00B2}";
        let result = normalize(input, NormForm::Nfkc);
        assert_eq!(result, "2");
    }

    // ==========================================================
    // NFKD tests
    // ==========================================================

    #[test]
    fn nfkd_decomposes_compatibility() {
        // U+FB01 (fi ligature) -> "fi"
        let input = "\u{FB01}";
        let result = normalize(input, NormForm::Nfkd);
        assert_eq!(result, "fi");
    }

    #[test]
    fn nfkd_decomposes_and_does_not_compose() {
        // é in NFKD should be decomposed
        let input = "\u{00E9}";
        let result = normalize(input, NormForm::Nfkd);
        assert_eq!(result, "e\u{0301}");
    }

    // ==========================================================
    // is_normalized tests
    // ==========================================================

    #[test]
    fn is_nfc_on_composed() {
        assert!(is_normalized("\u{00E9}", NormForm::Nfc));
    }

    #[test]
    fn is_nfc_on_decomposed() {
        assert!(!is_normalized("e\u{0301}", NormForm::Nfc));
    }

    #[test]
    fn is_nfd_on_decomposed() {
        assert!(is_normalized("e\u{0301}", NormForm::Nfd));
    }

    #[test]
    fn is_nfd_on_composed() {
        assert!(!is_normalized("\u{00E9}", NormForm::Nfd));
    }

    #[test]
    fn ascii_is_all_forms() {
        let ascii = "hello world 123";
        assert!(is_normalized(ascii, NormForm::Nfc));
        assert!(is_normalized(ascii, NormForm::Nfd));
        assert!(is_normalized(ascii, NormForm::Nfkc));
        assert!(is_normalized(ascii, NormForm::Nfkd));
    }

    // ==========================================================
    // eq_normalized tests
    // ==========================================================

    #[test]
    fn eq_normalized_composed_vs_decomposed() {
        assert!(eq_normalized("\u{00E9}", "e\u{0301}", NormForm::Nfc));
        assert!(eq_normalized("\u{00E9}", "e\u{0301}", NormForm::Nfd));
    }

    #[test]
    fn eq_normalized_different_strings() {
        assert!(!eq_normalized("a", "b", NormForm::Nfc));
    }

    // ==========================================================
    // normalize_for_search tests
    // ==========================================================

    #[test]
    fn search_normalization_case_folds() {
        assert_eq!(normalize_for_search("Hello"), normalize_for_search("hello"));
    }

    #[test]
    fn search_normalization_handles_accents() {
        // é (composed) and e+combining acute should match
        let a = normalize_for_search("\u{00E9}");
        let b = normalize_for_search("e\u{0301}");
        assert_eq!(a, b);
    }

    #[test]
    fn search_normalization_compatibility() {
        // Fullwidth A should match regular a
        let a = normalize_for_search("\u{FF21}");
        let b = normalize_for_search("a");
        assert_eq!(a, b);
    }

    // ==========================================================
    // Streaming iterator tests
    // ==========================================================

    #[test]
    fn nfc_iter_matches_normalize() {
        let input = "e\u{0301} cafe\u{0301}";
        let iter_result: String = nfc_iter(input).collect();
        let norm_result = normalize(input, NormForm::Nfc);
        assert_eq!(iter_result, norm_result);
    }

    #[test]
    fn nfd_iter_matches_normalize() {
        let input = "\u{00E9} caf\u{00E9}";
        let iter_result: String = nfd_iter(input).collect();
        let norm_result = normalize(input, NormForm::Nfd);
        assert_eq!(iter_result, norm_result);
    }

    // ==========================================================
    // Edge cases
    // ==========================================================

    #[test]
    fn empty_string_all_forms() {
        assert_eq!(normalize("", NormForm::Nfc), "");
        assert_eq!(normalize("", NormForm::Nfd), "");
        assert_eq!(normalize("", NormForm::Nfkc), "");
        assert_eq!(normalize("", NormForm::Nfkd), "");
        assert!(is_normalized("", NormForm::Nfc));
        assert!(is_normalized("", NormForm::Nfd));
    }

    #[test]
    fn hangul_composition() {
        // Hangul Jamo: ᄒ + ᅡ + ᆫ -> 한
        let decomposed = "\u{1112}\u{1161}\u{11AB}";
        let composed = normalize(decomposed, NormForm::Nfc);
        assert_eq!(composed, "\u{D55C}"); // 한
    }

    #[test]
    fn hangul_decomposition() {
        let composed = "\u{D55C}"; // 한
        let decomposed = normalize(composed, NormForm::Nfd);
        assert_eq!(decomposed, "\u{1112}\u{1161}\u{11AB}");
    }

    #[test]
    fn mixed_script_normalization() {
        // Mix of Latin, CJK, emoji - should pass through safely
        let input = "Hello 世界 🌍";
        let result = normalize(input, NormForm::Nfc);
        assert_eq!(result, input); // Already NFC
    }

    #[test]
    fn long_combining_sequence() {
        // Multiple combining marks on one base
        let mut input = String::from("a");
        for _ in 0..20 {
            input.push('\u{0300}'); // combining grave
        }
        let result = normalize(&input, NormForm::Nfc);
        // Should not panic and should produce valid output
        assert!(!result.is_empty());
        assert!(is_normalized(&result, NormForm::Nfc));
    }

    #[test]
    fn canonical_ordering() {
        // NFC should produce canonical ordering of combining marks
        // U+0041 + U+0327 (cedilla, ccc=202) + U+0301 (acute, ccc=230)
        // vs
        // U+0041 + U+0301 (acute, ccc=230) + U+0327 (cedilla, ccc=202)
        let a = normalize("A\u{0327}\u{0301}", NormForm::Nfc);
        let b = normalize("A\u{0301}\u{0327}", NormForm::Nfc);
        assert_eq!(a, b, "Canonical ordering should make these equivalent");
    }
}
