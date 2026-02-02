#![forbid(unsafe_code)]

//! Unicode Bidirectional Algorithm (UAX#9) support.
//!
//! This module provides functions to reorder mixed LTR/RTL text for
//! visual display, wrapping the [`unicode_bidi`] crate.
//!
//! # Example
//!
//! ```rust
//! use ftui_text::bidi::{reorder, ParagraphDirection};
//!
//! // Pure LTR text passes through unchanged.
//! let result = reorder("Hello, world!", ParagraphDirection::Auto);
//! assert_eq!(result, "Hello, world!");
//!
//! // You can also force a paragraph direction.
//! let result = reorder("Hello", ParagraphDirection::Ltr);
//! assert_eq!(result, "Hello");
//! ```
//!
//! # Feature gate
//!
//! This module is only available when the `bidi` feature is enabled.

use unicode_bidi::{BidiInfo, Level};

/// Paragraph base direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParagraphDirection {
    /// Auto-detect from the first strong directional character (UAX#9 default).
    #[default]
    Auto,
    /// Force left-to-right paragraph level.
    Ltr,
    /// Force right-to-left paragraph level.
    Rtl,
}

/// Reorder a single line of text for visual display according to UAX#9.
///
/// Returns the visually reordered string. Characters are rearranged so that
/// when rendered left-to-right on screen, the text appears correctly for
/// mixed-direction content.
///
/// Explicit directional marks (LRM U+200E, RLM U+200F, LRE U+202A, etc.)
/// are processed and removed from the output by the underlying algorithm.
pub fn reorder(text: &str, direction: ParagraphDirection) -> String {
    if text.is_empty() {
        return String::new();
    }

    let level = match direction {
        ParagraphDirection::Auto => None,
        ParagraphDirection::Ltr => Some(Level::ltr()),
        ParagraphDirection::Rtl => Some(Level::rtl()),
    };

    let bidi_info = BidiInfo::new(text, level);

    // BidiInfo splits by paragraph; we process each and join.
    let mut result = String::with_capacity(text.len());
    for para in &bidi_info.paragraphs {
        let line = para.range.clone();
        let reordered = bidi_info.reorder_line(para, line);
        result.push_str(&reordered);
    }

    result
}

/// Classify each character's resolved bidi level in a line of text.
///
/// Returns a vector of [`Level`] values, one per byte of the input (matching
/// the `unicode-bidi` convention). Even levels are LTR, odd levels are RTL.
///
/// This is useful for applying per-character styling (e.g., highlighting RTL
/// runs differently) without performing the full reorder.
pub fn resolve_levels(text: &str, direction: ParagraphDirection) -> Vec<Level> {
    if text.is_empty() {
        return Vec::new();
    }

    let level = match direction {
        ParagraphDirection::Auto => None,
        ParagraphDirection::Ltr => Some(Level::ltr()),
        ParagraphDirection::Rtl => Some(Level::rtl()),
    };

    let bidi_info = BidiInfo::new(text, level);
    bidi_info.levels.clone()
}

/// Returns `true` if the text contains any characters with RTL bidi class.
///
/// This is a cheap check to avoid calling [`reorder`] on pure-LTR text.
pub fn has_rtl(text: &str) -> bool {
    // Quick scan: any character in the RTL Unicode ranges?
    // This covers Arabic (U+0600–U+06FF), Hebrew (U+0590–U+05FF),
    // and other RTL scripts without running the full bidi algorithm.
    text.chars().any(is_rtl_char)
}

/// Returns `true` if the character has an RTL bidi class.
fn is_rtl_char(c: char) -> bool {
    matches!(c,
        '\u{0590}'..='\u{05FF}' |  // Hebrew
        '\u{0600}'..='\u{06FF}' |  // Arabic
        '\u{0700}'..='\u{074F}' |  // Syriac
        '\u{0780}'..='\u{07BF}' |  // Thaana
        '\u{07C0}'..='\u{07FF}' |  // NKo
        '\u{0800}'..='\u{083F}' |  // Samaritan
        '\u{0840}'..='\u{085F}' |  // Mandaic
        '\u{08A0}'..='\u{08FF}' |  // Arabic Extended-A
        '\u{FB1D}'..='\u{FB4F}' |  // Hebrew Presentation Forms
        '\u{FB50}'..='\u{FDFF}' |  // Arabic Presentation Forms-A
        '\u{FE70}'..='\u{FEFF}' |  // Arabic Presentation Forms-B
        '\u{10800}'..='\u{1083F}' | // Cypriot
        '\u{10840}'..='\u{1085F}' | // Imperial Aramaic
        '\u{10900}'..='\u{1091F}' | // Phoenician
        '\u{10920}'..='\u{1093F}' | // Lydian
        '\u{10A00}'..='\u{10A5F}' | // Kharoshthi
        '\u{10B00}'..='\u{10B3F}' | // Avestan
        '\u{1EE00}'..='\u{1EEFF}' | // Arabic Mathematical Symbols
        '\u{200F}' |               // RLM
        '\u{202B}' |               // RLE
        '\u{202E}' |               // RLO
        '\u{2067}'                  // RLI
    )
}

/// Returns the dominant direction of the text (the base paragraph level).
pub fn paragraph_level(text: &str) -> ParagraphDirection {
    if text.is_empty() {
        return ParagraphDirection::Ltr;
    }

    let bidi_info = BidiInfo::new(text, None);
    if let Some(para) = bidi_info.paragraphs.first() {
        if para.level.is_rtl() {
            ParagraphDirection::Rtl
        } else {
            ParagraphDirection::Ltr
        }
    } else {
        ParagraphDirection::Ltr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- reorder tests ---

    #[test]
    fn reorder_empty() {
        assert_eq!(reorder("", ParagraphDirection::Auto), "");
    }

    #[test]
    fn reorder_pure_ltr() {
        let text = "Hello, world!";
        assert_eq!(reorder(text, ParagraphDirection::Auto), text);
    }

    #[test]
    fn reorder_pure_rtl_hebrew() {
        // Hebrew text: "שלום" (shalom)
        let text = "\u{05E9}\u{05DC}\u{05D5}\u{05DD}";
        let result = reorder(text, ParagraphDirection::Auto);
        // Pure RTL text is reversed for visual display
        assert_eq!(result, "\u{05DD}\u{05D5}\u{05DC}\u{05E9}");
    }

    #[test]
    fn reorder_pure_rtl_arabic() {
        // Arabic text: "مرحبا" (marhaba)
        let text = "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}";
        let result = reorder(text, ParagraphDirection::Auto);
        // Pure RTL text gets reversed
        assert_eq!(result, "\u{0627}\u{0628}\u{062D}\u{0631}\u{0645}");
    }

    #[test]
    fn reorder_mixed_ltr_rtl() {
        // "Hello שלום World"
        let text = "Hello \u{05E9}\u{05DC}\u{05D5}\u{05DD} World";
        let result = reorder(text, ParagraphDirection::Ltr);
        // LTR paragraph: "Hello" stays, Hebrew reversed, "World" stays
        assert_eq!(
            result,
            "Hello \u{05DD}\u{05D5}\u{05DC}\u{05E9} World"
        );
    }

    #[test]
    fn reorder_forced_ltr() {
        let text = "Hello";
        assert_eq!(reorder(text, ParagraphDirection::Ltr), "Hello");
    }

    #[test]
    fn reorder_forced_rtl_on_ltr_text() {
        // When forcing RTL paragraph direction on LTR text,
        // the LTR text becomes an embedded LTR run in an RTL paragraph
        let text = "ABC";
        let result = reorder(text, ParagraphDirection::Rtl);
        // In an RTL paragraph, LTR text keeps its internal order
        assert_eq!(result, "ABC");
    }

    #[test]
    fn reorder_with_numbers() {
        // Numbers are "weak" LTR - they maintain LTR order even in RTL context
        let text = "\u{05E9}\u{05DC}\u{05D5}\u{05DD} 123";
        let result = reorder(text, ParagraphDirection::Auto);
        // RTL paragraph: Hebrew reversed, numbers stay LTR
        assert!(result.contains("123"));
    }

    #[test]
    fn reorder_with_lrm_mark() {
        // LRM (U+200E) should be processed
        let text = "A\u{200E}B";
        let result = reorder(text, ParagraphDirection::Auto);
        assert!(result.contains('A'));
        assert!(result.contains('B'));
    }

    #[test]
    fn reorder_with_rlm_mark() {
        // RLM (U+200F) should be processed
        let text = "A\u{200F}B";
        let result = reorder(text, ParagraphDirection::Auto);
        assert!(result.contains('A'));
        assert!(result.contains('B'));
    }

    // --- has_rtl tests ---

    #[test]
    fn has_rtl_empty() {
        assert!(!has_rtl(""));
    }

    #[test]
    fn has_rtl_pure_ltr() {
        assert!(!has_rtl("Hello, world!"));
    }

    #[test]
    fn has_rtl_hebrew() {
        assert!(has_rtl("\u{05E9}\u{05DC}\u{05D5}\u{05DD}"));
    }

    #[test]
    fn has_rtl_arabic() {
        assert!(has_rtl("\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}"));
    }

    #[test]
    fn has_rtl_mixed() {
        assert!(has_rtl("Hello \u{05E9}\u{05DC}\u{05D5}\u{05DD}"));
    }

    #[test]
    fn has_rtl_with_rlm() {
        assert!(has_rtl("A\u{200F}B"));
    }

    #[test]
    fn has_rtl_numbers_only() {
        assert!(!has_rtl("12345"));
    }

    // --- resolve_levels tests ---

    #[test]
    fn resolve_levels_empty() {
        assert!(resolve_levels("", ParagraphDirection::Auto).is_empty());
    }

    #[test]
    fn resolve_levels_pure_ltr() {
        let levels = resolve_levels("ABC", ParagraphDirection::Auto);
        assert!(!levels.is_empty());
        // All bytes should have even (LTR) levels
        for level in &levels {
            assert!(level.is_ltr(), "Expected LTR level, got {:?}", level);
        }
    }

    #[test]
    fn resolve_levels_pure_rtl() {
        let levels = resolve_levels("\u{05E9}\u{05DC}\u{05D5}\u{05DD}", ParagraphDirection::Auto);
        assert!(!levels.is_empty());
        // All bytes should have odd (RTL) levels
        for level in &levels {
            assert!(level.is_rtl(), "Expected RTL level, got {:?}", level);
        }
    }

    // --- paragraph_level tests ---

    #[test]
    fn paragraph_level_empty() {
        assert_eq!(paragraph_level(""), ParagraphDirection::Ltr);
    }

    #[test]
    fn paragraph_level_ltr() {
        assert_eq!(paragraph_level("Hello"), ParagraphDirection::Ltr);
    }

    #[test]
    fn paragraph_level_rtl() {
        assert_eq!(
            paragraph_level("\u{05E9}\u{05DC}\u{05D5}\u{05DD}"),
            ParagraphDirection::Rtl
        );
    }

    #[test]
    fn paragraph_level_mixed_starts_ltr() {
        assert_eq!(
            paragraph_level("Hello \u{05E9}\u{05DC}\u{05D5}\u{05DD}"),
            ParagraphDirection::Ltr
        );
    }

    #[test]
    fn paragraph_level_mixed_starts_rtl() {
        assert_eq!(
            paragraph_level("\u{05E9}\u{05DC}\u{05D5}\u{05DD} Hello"),
            ParagraphDirection::Rtl
        );
    }

    // --- is_rtl_char tests ---

    #[test]
    fn is_rtl_char_covers_ranges() {
        assert!(is_rtl_char('\u{05D0}')); // Hebrew Alef
        assert!(is_rtl_char('\u{0627}')); // Arabic Alif
        assert!(is_rtl_char('\u{200F}')); // RLM
        assert!(!is_rtl_char('A'));
        assert!(!is_rtl_char('1'));
        assert!(!is_rtl_char(' '));
    }
}
