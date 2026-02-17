//! Configurable Unicode character width policy.
//!
//! Terminal emulators disagree on how to measure the display width of certain
//! Unicode code points — most notably the East Asian Ambiguous (EA) category
//! (box drawing, arrows, Greek letters, etc.). CJK locales typically treat
//! these as double-width, while Western locales use single-width.
//!
//! [`WidthPolicy`] lets the engine caller choose which convention to follow.

use unicode_width::UnicodeWidthChar;

/// Unicode character width measurement policy.
///
/// Controls how the terminal engine computes display widths — in particular
/// for East Asian Ambiguous characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WidthPolicy {
    /// Standard Unicode width (`UnicodeWidthChar::width`).
    ///
    /// East Asian Ambiguous characters are single-width. This is the default
    /// and matches the behavior of most Western terminal emulators.
    #[default]
    Standard,

    /// CJK-aware width (`UnicodeWidthChar::width_cjk`).
    ///
    /// East Asian Ambiguous characters are treated as double-width, matching
    /// the expectations of CJK locale users.
    CjkAmbiguousWide,
}

impl WidthPolicy {
    /// Compute the terminal display width of a single Unicode scalar.
    ///
    /// Returns:
    /// - `0` for non-spacing marks / format controls (combining marks, ZWJ, etc.)
    /// - `1` for narrow characters
    /// - `2` for wide characters (CJK ideographs, emoji presentation, or
    ///   EA Ambiguous characters under [`CjkAmbiguousWide`](WidthPolicy::CjkAmbiguousWide))
    ///
    /// Widths above 2 are clamped to 2 for terminal cell semantics.
    #[inline]
    pub fn char_width(self, ch: char) -> u8 {
        let w = match self {
            WidthPolicy::Standard => UnicodeWidthChar::width(ch).unwrap_or(0),
            WidthPolicy::CjkAmbiguousWide => UnicodeWidthChar::width_cjk(ch).unwrap_or(0),
        };
        w.min(2) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::Cell;

    // ── ASCII ──────────────────────────────────────────────────────────

    #[test]
    fn ascii_width_is_one_for_both_policies() {
        for ch in ['a', 'Z', '0', '~', ' '] {
            assert_eq!(WidthPolicy::Standard.char_width(ch), 1, "Standard: {ch:?}");
            assert_eq!(
                WidthPolicy::CjkAmbiguousWide.char_width(ch),
                1,
                "CjkAmbiguousWide: {ch:?}"
            );
        }
    }

    // ── CJK ideographs ────────────────────────────────────────────────

    #[test]
    fn cjk_ideograph_is_wide_for_both_policies() {
        for ch in ['中', '国', '字'] {
            assert_eq!(WidthPolicy::Standard.char_width(ch), 2, "Standard: {ch:?}");
            assert_eq!(
                WidthPolicy::CjkAmbiguousWide.char_width(ch),
                2,
                "CjkAmbiguousWide: {ch:?}"
            );
        }
    }

    // ── Combining marks (zero-width) ──────────────────────────────────

    #[test]
    fn combining_marks_are_zero_width_for_both_policies() {
        for ch in ['\u{0300}', '\u{0301}', '\u{0302}'] {
            assert_eq!(WidthPolicy::Standard.char_width(ch), 0, "Standard: {ch:?}");
            assert_eq!(
                WidthPolicy::CjkAmbiguousWide.char_width(ch),
                0,
                "CjkAmbiguousWide: {ch:?}"
            );
        }
    }

    // ── East Asian Ambiguous characters ───────────────────────────────

    #[test]
    fn ea_ambiguous_box_drawing_standard_is_narrow() {
        // Box drawing: U+2500 ─, U+2502 │, U+250C ┌
        for ch in ['─', '│', '┌'] {
            assert_eq!(WidthPolicy::Standard.char_width(ch), 1, "Standard: {ch:?}");
        }
    }

    #[test]
    fn ea_ambiguous_box_drawing_cjk_is_wide() {
        for ch in ['─', '│', '┌'] {
            assert_eq!(
                WidthPolicy::CjkAmbiguousWide.char_width(ch),
                2,
                "CjkAmbiguousWide: {ch:?}"
            );
        }
    }

    #[test]
    fn ea_ambiguous_arrows_standard_is_narrow() {
        // Arrows: U+2190 ←, U+2191 ↑, U+2192 →, U+2193 ↓
        for ch in ['→', '←', '↑', '↓'] {
            assert_eq!(WidthPolicy::Standard.char_width(ch), 1, "Standard: {ch:?}");
        }
    }

    #[test]
    fn ea_ambiguous_arrows_cjk_is_wide() {
        for ch in ['→', '←', '↑', '↓'] {
            assert_eq!(
                WidthPolicy::CjkAmbiguousWide.char_width(ch),
                2,
                "CjkAmbiguousWide: {ch:?}"
            );
        }
    }

    #[test]
    fn ea_ambiguous_misc_standard_is_narrow() {
        // Degree sign U+00B0, multiplication sign U+00D7, registered sign U+00AE
        for ch in ['°', '×', '®'] {
            assert_eq!(WidthPolicy::Standard.char_width(ch), 1, "Standard: {ch:?}");
        }
    }

    #[test]
    fn ea_ambiguous_misc_cjk_is_wide() {
        for ch in ['°', '×', '®'] {
            assert_eq!(
                WidthPolicy::CjkAmbiguousWide.char_width(ch),
                2,
                "CjkAmbiguousWide: {ch:?}"
            );
        }
    }

    // ── Emoji ─────────────────────────────────────────────────────────

    #[test]
    fn emoji_is_wide_for_both_policies() {
        let ch = '\u{1F680}'; // 🚀
        assert_eq!(WidthPolicy::Standard.char_width(ch), 2);
        assert_eq!(WidthPolicy::CjkAmbiguousWide.char_width(ch), 2);
    }

    // ── Standard matches Cell::display_width ──────────────────────────

    #[test]
    fn standard_matches_cell_display_width() {
        let cases = [
            'a',
            'Z',
            '中',
            '\u{1F680}',
            '\u{0301}',
            '\u{200D}',
            '\u{FE0F}',
            '─',
            '→',
            '°',
        ];
        for ch in cases {
            assert_eq!(
                WidthPolicy::Standard.char_width(ch),
                Cell::display_width(ch),
                "Mismatch for {ch:?}"
            );
        }
    }

    // ── Width clamping ────────────────────────────────────────────────

    #[test]
    fn widths_are_clamped_to_two() {
        // All known code points should already be <= 2, but verify the clamp
        // logic by testing wide characters whose raw width is exactly 2.
        assert!(WidthPolicy::Standard.char_width('中') <= 2);
        assert!(WidthPolicy::CjkAmbiguousWide.char_width('中') <= 2);
    }

    // ── Default ───────────────────────────────────────────────────────

    #[test]
    fn default_is_standard() {
        assert_eq!(WidthPolicy::default(), WidthPolicy::Standard);
    }
}
