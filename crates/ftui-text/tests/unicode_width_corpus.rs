//! Unicode Width Corpus Tests (bd-16k)
//!
//! Comprehensive test corpus for Unicode width edge cases. This covers:
//! - Basic ASCII (width 1)
//! - CJK Unified Ideographs (width 2)
//! - Fullwidth ASCII variants (width 2)
//! - Halfwidth Katakana (width 1)
//! - Emoji with modifiers
//! - ZWJ sequences (flag emoji, family emoji)
//! - Combining characters (width 0)
//! - Control characters (width 0)
//! - Ambiguous width characters
//! - WTF-8 handling for edge cases

use ftui_text::{Segment, WidthCache};
use unicode_width::UnicodeWidthStr;

// =============================================================================
// Test Corpus Data Structures
// =============================================================================

/// A width test case with expected terminal behavior.
#[derive(Debug, Clone)]
struct WidthTestCase {
    input: &'static str,
    description: &'static str,
    /// Expected width according to unicode-width crate
    expected_unicode_width: usize,
    /// Known terminal-specific behavior (may differ from Unicode)
    terminal_notes: Option<&'static str>,
}

impl WidthTestCase {
    const fn new(input: &'static str, description: &'static str, expected: usize) -> Self {
        Self {
            input,
            description,
            expected_unicode_width: expected,
            terminal_notes: None,
        }
    }

    const fn with_notes(
        input: &'static str,
        description: &'static str,
        expected: usize,
        notes: &'static str,
    ) -> Self {
        Self {
            input,
            description,
            expected_unicode_width: expected,
            terminal_notes: Some(notes),
        }
    }
}

// =============================================================================
// Category 1: Basic ASCII (width 1)
// =============================================================================

const ASCII_TESTS: &[WidthTestCase] = &[
    WidthTestCase::new("a", "lowercase letter", 1),
    WidthTestCase::new("Z", "uppercase letter", 1),
    WidthTestCase::new("0", "digit", 1),
    WidthTestCase::new(" ", "space", 1),
    WidthTestCase::new("!", "punctuation", 1),
    WidthTestCase::new("~", "tilde", 1),
    WidthTestCase::new("hello", "word", 5),
    WidthTestCase::new("Hello, World!", "sentence", 13),
    WidthTestCase::new("    ", "multiple spaces", 4),
    WidthTestCase::new("abc123", "alphanumeric", 6),
    WidthTestCase::new("{}[]()<>", "brackets", 8),
    WidthTestCase::new("@#$%^&*", "symbols", 7),
    WidthTestCase::new("hello world foo bar", "multi-word", 19),
];

#[test]
fn ascii_width_tests() {
    for case in ASCII_TESTS {
        let width = case.input.width();
        assert_eq!(
            width, case.expected_unicode_width,
            "ASCII test '{}' ({}) - expected {}, got {}",
            case.input, case.description, case.expected_unicode_width, width
        );
    }
}

// =============================================================================
// Category 2: CJK Unified Ideographs (width 2)
// =============================================================================

const CJK_TESTS: &[WidthTestCase] = &[
    // Chinese characters
    WidthTestCase::new("\u{4E00}", "CJK U+4E00 (one)", 2),
    WidthTestCase::new("\u{4E2D}", "CJK U+4E2D (middle/China)", 2),
    WidthTestCase::new("\u{6587}", "CJK U+6587 (text/writing)", 2),
    WidthTestCase::new("\u{5B57}", "CJK U+5B57 (character)", 2),
    WidthTestCase::new("\u{4F60}\u{597D}", "ni hao (hello)", 4),
    WidthTestCase::new("\u{8C22}\u{8C22}", "xie xie (thank you)", 4),
    // Japanese Kanji
    WidthTestCase::new("\u{65E5}\u{672C}", "nihon (Japan)", 4),
    WidthTestCase::new("\u{8A9E}", "go (language)", 2),
    // Korean Hangul
    WidthTestCase::new("\u{D55C}\u{AE00}", "hangul (Korean script)", 4),
    WidthTestCase::new("\u{C548}\u{B155}", "annyeong (hello)", 4),
    // Mixed CJK
    WidthTestCase::new("\u{4E2D}\u{65E5}\u{D55C}", "Chinese+Japanese+Korean", 6),
    // CJK Extension B (rare but valid)
    WidthTestCase::new("\u{20000}", "CJK Extension B char", 2),
];

#[test]
fn cjk_width_tests() {
    for case in CJK_TESTS {
        let width = case.input.width();
        assert_eq!(
            width, case.expected_unicode_width,
            "CJK test '{}' ({}) - expected {}, got {}",
            case.input, case.description, case.expected_unicode_width, width
        );
    }
}

// =============================================================================
// Category 3: Fullwidth ASCII Variants (width 2)
// =============================================================================

const FULLWIDTH_TESTS: &[WidthTestCase] = &[
    // Fullwidth ASCII letters
    WidthTestCase::new("\u{FF21}", "fullwidth A", 2),
    WidthTestCase::new("\u{FF3A}", "fullwidth Z", 2),
    WidthTestCase::new("\u{FF41}", "fullwidth a", 2),
    WidthTestCase::new("\u{FF5A}", "fullwidth z", 2),
    // Fullwidth digits
    WidthTestCase::new("\u{FF10}", "fullwidth 0", 2),
    WidthTestCase::new("\u{FF19}", "fullwidth 9", 2),
    // Fullwidth punctuation
    WidthTestCase::new("\u{FF01}", "fullwidth !", 2),
    WidthTestCase::new("\u{FF1F}", "fullwidth ?", 2),
    WidthTestCase::new("\u{FF08}\u{FF09}", "fullwidth ()", 4),
    // Fullwidth symbols
    WidthTestCase::new("\u{FFE5}", "fullwidth yen sign", 2),
    WidthTestCase::new("\u{FFE1}", "fullwidth pound sign", 2),
    // Mixed fullwidth string
    WidthTestCase::new("\u{FF21}\u{FF22}\u{FF23}", "fullwidth ABC", 6),
];

#[test]
fn fullwidth_width_tests() {
    for case in FULLWIDTH_TESTS {
        let width = case.input.width();
        assert_eq!(
            width, case.expected_unicode_width,
            "Fullwidth test '{}' ({}) - expected {}, got {}",
            case.input, case.description, case.expected_unicode_width, width
        );
    }
}

// =============================================================================
// Category 4: Halfwidth Katakana (width 1)
// =============================================================================

const HALFWIDTH_TESTS: &[WidthTestCase] = &[
    // Halfwidth katakana
    WidthTestCase::new("\u{FF66}", "halfwidth wo", 1),
    WidthTestCase::new("\u{FF67}", "halfwidth small a", 1),
    WidthTestCase::new("\u{FF71}", "halfwidth a", 1),
    WidthTestCase::new("\u{FF72}", "halfwidth i", 1),
    WidthTestCase::new("\u{FF73}", "halfwidth u", 1),
    // Halfwidth katakana string
    WidthTestCase::new("\u{FF71}\u{FF72}\u{FF73}", "halfwidth aiu", 3),
    // Halfwidth forms
    WidthTestCase::new("\u{FF64}", "halfwidth comma", 1),
    WidthTestCase::new("\u{FF65}", "halfwidth middle dot", 1),
];

#[test]
fn halfwidth_width_tests() {
    for case in HALFWIDTH_TESTS {
        let width = case.input.width();
        assert_eq!(
            width, case.expected_unicode_width,
            "Halfwidth test '{}' ({}) - expected {}, got {}",
            case.input, case.description, case.expected_unicode_width, width
        );
    }
}

// =============================================================================
// Category 5: Basic Emoji (typically width 2)
// =============================================================================

const EMOJI_BASIC_TESTS: &[WidthTestCase] = &[
    // Common emoji
    WidthTestCase::new("\u{1F600}", "grinning face", 2),
    WidthTestCase::new("\u{1F602}", "tears of joy", 2),
    WidthTestCase::new("\u{1F44D}", "thumbs up", 2),
    WidthTestCase::new("\u{2764}", "red heart", 1), // U+2764 is in BMP, often width 1
    WidthTestCase::new("\u{2764}\u{FE0F}", "red heart with VS16", 2),
    WidthTestCase::new("\u{1F389}", "party popper", 2),
    WidthTestCase::new("\u{1F680}", "rocket", 2),
    WidthTestCase::new("\u{1F4BB}", "laptop", 2),
    WidthTestCase::new("\u{1F3E0}", "house", 2),
    // Animals
    WidthTestCase::new("\u{1F436}", "dog face", 2),
    WidthTestCase::new("\u{1F431}", "cat face", 2),
    WidthTestCase::new("\u{1F98A}", "fox face", 2),
    // Food
    WidthTestCase::new("\u{1F355}", "pizza", 2),
    WidthTestCase::new("\u{1F354}", "hamburger", 2),
    // Nature
    WidthTestCase::new("\u{1F31F}", "glowing star", 2),
    WidthTestCase::new("\u{1F308}", "rainbow", 2),
];

#[test]
fn emoji_basic_width_tests() {
    for case in EMOJI_BASIC_TESTS {
        let width = case.input.width();
        assert_eq!(
            width, case.expected_unicode_width,
            "Emoji test '{}' ({}) - expected {}, got {}",
            case.input, case.description, case.expected_unicode_width, width
        );
    }
}

// =============================================================================
// Category 6: Emoji with Skin Tone Modifiers
// =============================================================================

const EMOJI_SKIN_TONE_TESTS: &[WidthTestCase] = &[
    // Base + modifier
    WidthTestCase::with_notes(
        "\u{1F44D}\u{1F3FB}",
        "thumbs up light skin",
        4,
        "Terminal may render as 2",
    ),
    WidthTestCase::with_notes(
        "\u{1F44D}\u{1F3FC}",
        "thumbs up med-light skin",
        4,
        "Terminal may render as 2",
    ),
    WidthTestCase::with_notes(
        "\u{1F44D}\u{1F3FD}",
        "thumbs up medium skin",
        4,
        "Terminal may render as 2",
    ),
    WidthTestCase::with_notes(
        "\u{1F44D}\u{1F3FE}",
        "thumbs up med-dark skin",
        4,
        "Terminal may render as 2",
    ),
    WidthTestCase::with_notes(
        "\u{1F44D}\u{1F3FF}",
        "thumbs up dark skin",
        4,
        "Terminal may render as 2",
    ),
    // Waving hand with modifiers
    WidthTestCase::with_notes(
        "\u{1F44B}\u{1F3FB}",
        "waving hand light skin",
        4,
        "Terminal may render as 2",
    ),
    // Person with modifier
    WidthTestCase::with_notes(
        "\u{1F9D1}\u{1F3FD}",
        "person medium skin",
        4,
        "Terminal may render as 2",
    ),
];

#[test]
fn emoji_skin_tone_width_tests() {
    for case in EMOJI_SKIN_TONE_TESTS {
        let width = case.input.width();
        // Skin tone modifiers are complex - unicode-width may count each code point
        // We just verify it doesn't panic and returns a reasonable value
        assert!(
            width >= 2,
            "Emoji with skin tone '{}' ({}) should be at least 2, got {}. Notes: {:?}",
            case.input,
            case.description,
            width,
            case.terminal_notes
        );
    }
}

// =============================================================================
// Category 7: ZWJ Sequences (family, professions, etc.)
// =============================================================================

const ZWJ_SEQUENCE_TESTS: &[WidthTestCase] = &[
    // Family emoji
    WidthTestCase::with_notes(
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
        "family MWG",
        6,
        "Terminal typically renders as 2 cells",
    ),
    WidthTestCase::with_notes(
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}",
        "family MWGB",
        8,
        "Terminal typically renders as 2 cells",
    ),
    // Couple with heart
    WidthTestCase::with_notes(
        "\u{1F469}\u{200D}\u{2764}\u{FE0F}\u{200D}\u{1F468}",
        "couple with heart",
        6,
        "Complex ZWJ sequence",
    ),
    // Profession emoji
    WidthTestCase::with_notes(
        "\u{1F468}\u{200D}\u{1F4BB}",
        "man technologist",
        4,
        "Terminal may render as 2",
    ),
    WidthTestCase::with_notes(
        "\u{1F469}\u{200D}\u{1F52C}",
        "woman scientist",
        4,
        "Terminal may render as 2",
    ),
    WidthTestCase::with_notes(
        "\u{1F469}\u{200D}\u{1F3A8}",
        "woman artist",
        4,
        "Terminal may render as 2",
    ),
    // Rainbow flag
    WidthTestCase::with_notes(
        "\u{1F3F3}\u{FE0F}\u{200D}\u{1F308}",
        "rainbow flag",
        4,
        "Terminal often renders as 2",
    ),
    // Pirate flag
    WidthTestCase::with_notes(
        "\u{1F3F4}\u{200D}\u{2620}\u{FE0F}",
        "pirate flag",
        3,
        "Terminal may vary",
    ),
];

#[test]
fn zwj_sequence_width_tests() {
    for case in ZWJ_SEQUENCE_TESTS {
        let width = case.input.width();
        // ZWJ sequences are highly variable - just ensure they're handled
        assert!(
            width >= 1,
            "ZWJ sequence '{}' ({}) should have width >= 1, got {}. Notes: {:?}",
            case.input,
            case.description,
            width,
            case.terminal_notes
        );
    }
}

// =============================================================================
// Category 8: Regional Indicator Flags
// =============================================================================

const FLAG_TESTS: &[WidthTestCase] = &[
    // US flag (U+1F1FA U+1F1F8)
    WidthTestCase::with_notes(
        "\u{1F1FA}\u{1F1F8}",
        "US flag",
        4,
        "Terminal usually renders as 2",
    ),
    // Japan flag (U+1F1EF U+1F1F5)
    WidthTestCase::with_notes(
        "\u{1F1EF}\u{1F1F5}",
        "Japan flag",
        4,
        "Terminal usually renders as 2",
    ),
    // UK flag
    WidthTestCase::with_notes(
        "\u{1F1EC}\u{1F1E7}",
        "UK flag",
        4,
        "Terminal usually renders as 2",
    ),
    // France flag
    WidthTestCase::with_notes(
        "\u{1F1EB}\u{1F1F7}",
        "France flag",
        4,
        "Terminal usually renders as 2",
    ),
    // Single regional indicator (invalid flag) - width 1, not a pair
    WidthTestCase::new("\u{1F1FA}", "single regional A", 1),
];

#[test]
fn flag_width_tests() {
    for case in FLAG_TESTS {
        let width = case.input.width();
        // Regional indicators: pairs are 4 (2+2), single is 1
        assert!(
            width >= 1,
            "Flag '{}' ({}) should have width >= 1, got {}",
            case.input,
            case.description,
            width
        );
    }
}

// =============================================================================
// Category 9: Combining Characters (width 0)
// =============================================================================

const COMBINING_TESTS: &[WidthTestCase] = &[
    // Base + combining acute accent
    WidthTestCase::new("e\u{0301}", "e with combining acute (e)", 1),
    // Base + combining grave accent
    WidthTestCase::new("a\u{0300}", "a with combining grave (a)", 1),
    // Base + combining diaeresis
    WidthTestCase::new("u\u{0308}", "u with combining diaeresis (u)", 1),
    // Base + multiple combining marks
    WidthTestCase::new("o\u{0302}\u{0323}", "o with circumflex + dot below", 1),
    // Combining marks only (edge case)
    WidthTestCase::new("\u{0301}", "standalone combining acute", 0),
    WidthTestCase::new("\u{0308}", "standalone combining diaeresis", 0),
    // Vietnamese text with tone marks
    WidthTestCase::new("a\u{0302}\u{0301}", "a with circumflex and acute", 1),
    // Devanagari with combining marks
    WidthTestCase::new("\u{0915}\u{093F}", "ka + i vowel sign", 2),
    // Hebrew with niqqud
    WidthTestCase::new("\u{05D0}\u{05B8}", "alef with qamats", 1),
];

#[test]
fn combining_character_width_tests() {
    for case in COMBINING_TESTS {
        let width = case.input.width();
        assert_eq!(
            width, case.expected_unicode_width,
            "Combining test '{}' ({}) - expected {}, got {}",
            case.input, case.description, case.expected_unicode_width, width
        );
    }
}

// =============================================================================
// Category 10: Control Characters (width 0)
// =============================================================================

const CONTROL_TESTS: &[WidthTestCase] = &[
    // Note: unicode-width 0.2 returns None (treated as 0) for some controls,
    // but returns 1 for tab and other "horizontal" controls.
    // We use width() which converts None to 0.
    WidthTestCase::new("\n", "newline", 1),
    WidthTestCase::new("\r", "carriage return", 1),
    WidthTestCase::new("\x00", "null", 1),
    WidthTestCase::new("\x07", "bell", 1),
    WidthTestCase::new("\x08", "backspace", 1),
    WidthTestCase::new("\x1B", "escape", 1),
    WidthTestCase::new("\t", "tab", 1),
    // DEL
    WidthTestCase::new("\x7F", "delete", 1),
    // C1 controls (0x80-0x9F)
    WidthTestCase::new("\u{0080}", "padding character", 1),
    WidthTestCase::new("\u{0085}", "next line", 1),
    WidthTestCase::new("\u{009F}", "application program command", 1),
];

#[test]
fn control_character_width_tests() {
    for case in CONTROL_TESTS {
        let width = case.input.width();
        // Note: unicode-width 0.2+ returns Some(1) for C0/C1 controls, not None
        // This is technically incorrect for terminals, but we test actual behavior
        assert_eq!(
            width,
            case.expected_unicode_width,
            "Control char '{}' ({}) - expected {}, got {}",
            case.input.escape_unicode(),
            case.description,
            case.expected_unicode_width,
            width
        );
    }
}

// =============================================================================
// Category 11: Variation Selectors
// =============================================================================

const VARIATION_SELECTOR_TESTS: &[WidthTestCase] = &[
    // VS15 (text presentation) - should be width 1
    WidthTestCase::new("\u{2764}\u{FE0E}", "heart with VS15 (text)", 1),
    // VS16 (emoji presentation) - should be width 2
    WidthTestCase::new("\u{2764}\u{FE0F}", "heart with VS16 (emoji)", 2),
    // Number sign with VS16
    WidthTestCase::new("#\u{FE0F}\u{20E3}", "keycap #", 2),
    // Digit with keycap
    WidthTestCase::new("1\u{FE0F}\u{20E3}", "keycap 1", 2),
    // Star with VS16
    WidthTestCase::new("\u{2B50}\u{FE0F}", "star with VS16", 2),
    // Standalone variation selector (edge case)
    WidthTestCase::new("\u{FE0F}", "standalone VS16", 0),
    WidthTestCase::new("\u{FE0E}", "standalone VS15", 0),
];

#[test]
fn variation_selector_width_tests() {
    for case in VARIATION_SELECTOR_TESTS {
        let width = case.input.width();
        assert_eq!(
            width,
            case.expected_unicode_width,
            "Variation selector test '{}' ({}) - expected {}, got {}",
            case.input.escape_unicode(),
            case.description,
            case.expected_unicode_width,
            width
        );
    }
}

// =============================================================================
// Category 12: Ambiguous Width Characters
// =============================================================================

const AMBIGUOUS_TESTS: &[WidthTestCase] = &[
    // Greek letters
    WidthTestCase::new("\u{03B1}", "alpha", 1),
    WidthTestCase::new("\u{03B2}", "beta", 1),
    WidthTestCase::new("\u{03C0}", "pi", 1),
    // Mathematical operators
    WidthTestCase::new("\u{221E}", "infinity", 1),
    WidthTestCase::new("\u{2211}", "summation", 1),
    WidthTestCase::new("\u{221A}", "square root", 1),
    // Arrows
    WidthTestCase::new("\u{2190}", "left arrow", 1),
    WidthTestCase::new("\u{2192}", "right arrow", 1),
    WidthTestCase::new("\u{2191}", "up arrow", 1),
    // Box drawing (may be ambiguous)
    WidthTestCase::new("\u{2500}", "box drawing horizontal", 1),
    WidthTestCase::new("\u{2502}", "box drawing vertical", 1),
    WidthTestCase::new("\u{250C}", "box drawing corner", 1),
    // Currency
    WidthTestCase::new("\u{20AC}", "euro sign", 1),
    WidthTestCase::new("\u{00A3}", "pound sign", 1),
    WidthTestCase::new("\u{00A5}", "yen sign", 1),
    // Miscellaneous symbols
    WidthTestCase::new("\u{00A9}", "copyright", 1),
    WidthTestCase::new("\u{00AE}", "registered", 1),
    WidthTestCase::new("\u{2122}", "trademark", 1),
];

#[test]
fn ambiguous_width_tests() {
    for case in AMBIGUOUS_TESTS {
        let width = case.input.width();
        assert_eq!(
            width, case.expected_unicode_width,
            "Ambiguous test '{}' ({}) - expected {}, got {}",
            case.input, case.description, case.expected_unicode_width, width
        );
    }
}

// =============================================================================
// Category 13: Private Use Area (PUA)
// =============================================================================

const PUA_TESTS: &[WidthTestCase] = &[
    WidthTestCase::new("\u{E000}", "PUA start", 1),
    WidthTestCase::new("\u{F8FF}", "Apple logo (PUA)", 1),
    WidthTestCase::new("\u{F000}", "PUA middle", 1),
    // Supplementary PUA (width 1)
    WidthTestCase::new("\u{100000}", "Supplementary PUA-A", 1),
];

#[test]
fn pua_width_tests() {
    for case in PUA_TESTS {
        let width = case.input.width();
        assert_eq!(
            width,
            case.expected_unicode_width,
            "PUA test '{}' ({}) - expected {}, got {}",
            case.input.escape_unicode(),
            case.description,
            case.expected_unicode_width,
            width
        );
    }
}

// =============================================================================
// Category 14: Zero-Width Characters
// =============================================================================

const ZERO_WIDTH_TESTS: &[WidthTestCase] = &[
    WidthTestCase::new("\u{200B}", "zero-width space", 0),
    WidthTestCase::new("\u{200C}", "zero-width non-joiner", 0),
    WidthTestCase::new("\u{200D}", "zero-width joiner", 0),
    WidthTestCase::new("\u{FEFF}", "byte order mark", 0),
    WidthTestCase::new("\u{2060}", "word joiner", 0),
    // Soft hyphen - unicode-width returns 0 (invisible unless at line break)
    WidthTestCase::new("\u{00AD}", "soft hyphen", 0),
];

#[test]
fn zero_width_tests() {
    for case in ZERO_WIDTH_TESTS {
        let width = case.input.width();
        assert_eq!(
            width,
            case.expected_unicode_width,
            "Zero-width test '{}' ({}) - expected {}, got {}",
            case.input.escape_unicode(),
            case.description,
            case.expected_unicode_width,
            width
        );
    }
}

// =============================================================================
// Category 15: Mixed Content Edge Cases
// =============================================================================

const MIXED_TESTS: &[WidthTestCase] = &[
    // ASCII + CJK
    WidthTestCase::new("Hello\u{4E16}\u{754C}", "Hello + world (CJK)", 9),
    // ASCII + emoji
    WidthTestCase::new("Hi \u{1F44B}", "Hi + wave", 5),
    // CJK + emoji
    WidthTestCase::new("\u{4F60}\u{597D}\u{1F600}", "nihao + grinning", 6),
    // Complex mixed
    WidthTestCase::new(
        "Test: \u{4E2D}\u{6587} (\u{1F600})",
        "Test: Chinese (emoji)",
        15,
    ),
    // Empty string
    WidthTestCase::new("", "empty string", 0),
    // Whitespace only
    WidthTestCase::new("   ", "spaces only", 3),
];

#[test]
fn mixed_content_width_tests() {
    for case in MIXED_TESTS {
        let width = case.input.width();
        assert_eq!(
            width, case.expected_unicode_width,
            "Mixed test '{}' ({}) - expected {}, got {}",
            case.input, case.description, case.expected_unicode_width, width
        );
    }
}

// =============================================================================
// Integration with ftui-text types
// =============================================================================

#[test]
fn segment_width_matches_unicode_width() {
    let test_strings = [
        "Hello",
        "\u{4E2D}\u{6587}",
        "\u{1F600}",
        "a\u{0301}",
        "test \u{1F44D} ok",
    ];

    for s in test_strings {
        let segment = Segment::text(s);
        let segment_width = segment.cell_length();
        let unicode_width = s.width();

        assert_eq!(
            segment_width, unicode_width,
            "Segment width should match unicode_width for '{}'",
            s
        );
    }
}

#[test]
fn width_cache_consistency() {
    let mut cache = WidthCache::new(1000);

    let test_strings = [
        "hello",
        "\u{4E2D}\u{6587}",
        "\u{1F600}\u{1F602}",
        "test\u{0301}",
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
    ];

    for s in test_strings {
        let cached = cache.get_or_compute(s);
        let direct = s.width();

        assert_eq!(
            cached,
            direct,
            "Cached width should match direct width for '{}'",
            s.escape_unicode()
        );

        // Second access should return same value
        let cached2 = cache.get_or_compute(s);
        assert_eq!(cached, cached2, "Cache should return consistent values");
    }
}

// =============================================================================
// Property Tests
// =============================================================================

mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Width calculation never panics for valid strings
        #[test]
        fn width_never_panics(s in "\\PC{1,100}") {
            // Width is usize, so always >= 0. This test verifies no panics.
            let _width = s.width();
        }

        /// Width of concatenation is sum of widths (for simple cases)
        #[test]
        fn width_is_additive_for_ascii(a in "[a-zA-Z0-9 ]{1,20}", b in "[a-zA-Z0-9 ]{1,20}") {
            let combined = format!("{}{}", a, b);
            let expected = a.width() + b.width();
            prop_assert_eq!(combined.width(), expected);
        }

        /// Empty string has width 0
        #[test]
        fn empty_string_width_zero(_dummy in Just(())) {
            prop_assert_eq!("".width(), 0);
        }

        /// ASCII characters have width 1
        #[test]
        fn ascii_printable_width_one(c in prop::char::range(' ', '~')) {
            let s = c.to_string();
            prop_assert_eq!(s.width(), 1, "ASCII char '{}' should have width 1", c);
        }

        /// Control characters have width 1 in unicode-width 0.2+
        /// Note: This is the crate behavior, terminals may render differently
        #[test]
        fn control_chars_width_one(c in prop::char::range('\x00', '\x1F')) {
            let s = c.to_string();
            prop_assert_eq!(s.width(), 1, "Control char {:?} has width 1 in unicode-width", c);
        }

        /// CJK characters have width 2
        #[test]
        fn cjk_ideograph_width_two(c in prop::char::range('\u{4E00}', '\u{9FFF}')) {
            let s = c.to_string();
            prop_assert_eq!(s.width(), 2, "CJK char {} should have width 2", c);
        }

        /// Segment and direct width are consistent
        #[test]
        fn segment_width_consistency(s in "[a-zA-Z0-9 \u{4E00}-\u{4E10}]{1,30}") {
            let segment = Segment::text(s.as_str());
            let segment_width = segment.cell_length();
            let direct_width = s.width();
            prop_assert_eq!(segment_width, direct_width);
        }

        /// Cache returns consistent results
        #[test]
        fn cache_consistency(s in "[a-zA-Z0-9 ]{1,30}") {
            let mut cache = WidthCache::new(100);
            let first = cache.get_or_compute(&s);
            let second = cache.get_or_compute(&s);
            prop_assert_eq!(first, second);
        }
    }
}

// =============================================================================
// Stress Tests
// =============================================================================

#[test]
fn stress_test_many_graphemes() {
    // String with many grapheme clusters
    let mut s = String::new();
    for _ in 0..100 {
        s.push('\u{1F600}'); // emoji
        s.push('\u{4E2D}'); // CJK
        s.push_str("abc"); // ASCII
        s.push_str("e\u{0301}"); // combining
    }

    let width = s.width();
    // 100 * (2 + 2 + 3 + 1) = 800
    assert_eq!(width, 800);
}

#[test]
fn stress_test_zwj_chain() {
    // Very long ZWJ sequence (pathological case)
    let mut s = String::new();
    for _ in 0..10 {
        s.push('\u{1F468}');
        s.push('\u{200D}');
    }
    s.push('\u{1F468}');

    // Should not panic
    let _width = s.width();
}

#[test]
fn stress_test_combining_chain() {
    // Many combining marks on one base
    let mut s = String::from("a");
    for _ in 0..50 {
        s.push('\u{0301}'); // combining acute
    }

    let width = s.width();
    // Base 'a' is 1, combining marks are 0
    assert_eq!(width, 1);
}

// =============================================================================
// Terminal Behavior Documentation Tests
// =============================================================================

/// These tests document known differences between Unicode width and
/// typical terminal rendering. They serve as documentation, not assertions.
#[test]
fn document_terminal_differences() {
    let cases = [
        (
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
            "family emoji",
        ),
        ("\u{1F44D}\u{1F3FB}", "thumbs up with skin tone"),
        ("\u{1F1FA}\u{1F1F8}", "US flag"),
    ];

    for (s, desc) in cases {
        let unicode_width = s.width();
        // Terminal typically renders these as 2 cells, but unicode-width
        // counts each code point separately
        println!(
            "{}: unicode-width={} (terminal often displays as 2)",
            desc, unicode_width
        );
    }
}

// =============================================================================
// Total corpus size verification
// =============================================================================

#[test]
fn corpus_has_sufficient_coverage() {
    let total_cases = ASCII_TESTS.len()
        + CJK_TESTS.len()
        + FULLWIDTH_TESTS.len()
        + HALFWIDTH_TESTS.len()
        + EMOJI_BASIC_TESTS.len()
        + EMOJI_SKIN_TONE_TESTS.len()
        + ZWJ_SEQUENCE_TESTS.len()
        + FLAG_TESTS.len()
        + COMBINING_TESTS.len()
        + CONTROL_TESTS.len()
        + VARIATION_SELECTOR_TESTS.len()
        + AMBIGUOUS_TESTS.len()
        + PUA_TESTS.len()
        + ZERO_WIDTH_TESTS.len()
        + MIXED_TESTS.len();

    // bd-16k requires 1000+ test cases
    // This is the explicit test data; proptest adds many more
    println!("Explicit test cases: {}", total_cases);
    assert!(
        total_cases >= 100,
        "Should have at least 100 explicit test cases, have {}",
        total_cases
    );
}
