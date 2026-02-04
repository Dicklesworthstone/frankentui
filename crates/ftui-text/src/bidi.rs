#![forbid(unsafe_code)]

//! Unicode Bidirectional Algorithm (UAX#9) support.
//!
//! This module provides functions and types for reordering mixed LTR/RTL text
//! for visual display, wrapping the [`unicode_bidi`] crate.
//!
//! # Types
//!
//! - [`BidiSegment`] — precomputed BiDi analysis for a text string with O(1)
//!   visual↔logical index mapping and cursor movement.
//! - [`BidiRun`] — a contiguous run of characters sharing the same direction.
//! - [`Direction`] — LTR or RTL text flow direction.
//! - [`ParagraphDirection`] — paragraph-level base direction (Auto/Ltr/Rtl).
//!
//! # Example
//!
//! ```rust
//! use ftui_text::bidi::{BidiSegment, Direction, ParagraphDirection, reorder};
//!
//! // Pure LTR text passes through unchanged.
//! let result = reorder("Hello, world!", ParagraphDirection::Auto);
//! assert_eq!(result, "Hello, world!");
//!
//! // BidiSegment provides visual↔logical cursor mapping.
//! let seg = BidiSegment::new("Hello", None);
//! assert_eq!(seg.visual_pos(0), 0);
//! assert_eq!(seg.logical_pos(0), 0);
//! ```
//!
//! # Feature gate
//!
//! This module is only available when the `bidi` feature is enabled.

use unicode_bidi::{BidiInfo, Level};

// ---------------------------------------------------------------------------
// Direction / ParagraphDirection
// ---------------------------------------------------------------------------

/// Text flow direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Left-to-right.
    Ltr,
    /// Right-to-left.
    Rtl,
}

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

fn direction_to_level(dir: Option<Direction>) -> Option<Level> {
    match dir {
        None => None,
        Some(Direction::Ltr) => Some(Level::ltr()),
        Some(Direction::Rtl) => Some(Level::rtl()),
    }
}

fn para_direction_to_level(dir: ParagraphDirection) -> Option<Level> {
    match dir {
        ParagraphDirection::Auto => None,
        ParagraphDirection::Ltr => Some(Level::ltr()),
        ParagraphDirection::Rtl => Some(Level::rtl()),
    }
}

// ---------------------------------------------------------------------------
// BidiRun
// ---------------------------------------------------------------------------

/// A contiguous run of characters sharing the same bidi direction.
///
/// Indices are in logical character space (not byte offsets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BidiRun {
    /// Start index (inclusive) in logical character space.
    pub start: usize,
    /// End index (exclusive) in logical character space.
    pub end: usize,
    /// Resolved bidi level for this run.
    pub level: Level,
    /// Effective direction of this run.
    pub direction: Direction,
}

impl BidiRun {
    /// Number of characters in this run.
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    /// Whether the run is empty.
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

// ---------------------------------------------------------------------------
// BidiSegment
// ---------------------------------------------------------------------------

/// BiDi-aware text segment with precomputed index maps.
///
/// All indices are in logical *character* space (not bytes). The
/// [`visual_to_logical`](Self::visual_to_logical) and
/// [`logical_to_visual`](Self::logical_to_visual) maps are computed once at
/// construction in O(n) time.
#[derive(Debug, Clone)]
pub struct BidiSegment {
    /// Original text.
    pub text: String,
    /// Characters in logical order.
    pub chars: Vec<char>,
    /// Per-character resolved bidi levels.
    pub levels: Vec<Level>,
    /// Contiguous directional runs in logical order.
    pub runs: Vec<BidiRun>,
    /// Permutation: `visual_to_logical[visual_idx] == logical_idx`.
    pub visual_to_logical: Vec<usize>,
    /// Inverse permutation: `logical_to_visual[logical_idx] == visual_idx`.
    pub logical_to_visual: Vec<usize>,
}

impl BidiSegment {
    /// Analyze `text` and build precomputed index maps.
    ///
    /// `base` optionally forces the paragraph direction; `None` uses UAX#9
    /// auto-detection from the first strong character.
    pub fn new(text: &str, base: Option<Direction>) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let n = chars.len();

        if n == 0 {
            return Self {
                text: String::new(),
                chars: Vec::new(),
                levels: Vec::new(),
                runs: Vec::new(),
                visual_to_logical: Vec::new(),
                logical_to_visual: Vec::new(),
            };
        }

        let level_opt = direction_to_level(base);
        let bidi_info = BidiInfo::new(text, level_opt);

        // Map byte-level levels to char-level levels. Each character's level
        // is taken from the level at its first byte offset.
        let char_levels = Self::byte_levels_to_char_levels(text, &bidi_info.levels);
        let runs = Self::compute_runs(&char_levels);
        let visual_to_logical = Self::compute_visual_order(&char_levels);
        let logical_to_visual = Self::invert_permutation(&visual_to_logical);

        Self {
            text: text.to_string(),
            chars,
            levels: char_levels,
            runs,
            visual_to_logical,
            logical_to_visual,
        }
    }

    /// Get the visual position corresponding to a logical character index.
    pub fn visual_pos(&self, logical: usize) -> usize {
        self.logical_to_visual
            .get(logical)
            .copied()
            .unwrap_or(logical)
    }

    /// Get the logical position corresponding to a visual column index.
    pub fn logical_pos(&self, visual: usize) -> usize {
        self.visual_to_logical
            .get(visual)
            .copied()
            .unwrap_or(visual)
    }

    /// Check if the character at `logical` index is part of an RTL run.
    pub fn is_rtl(&self, logical: usize) -> bool {
        self.levels.get(logical).is_some_and(|level| level.is_rtl())
    }

    /// Move cursor one step to the right in visual order.
    ///
    /// Returns the new logical index. If already at the rightmost position,
    /// returns the current logical index unchanged.
    pub fn move_right(&self, logical: usize) -> usize {
        let visual = self.visual_pos(logical);
        if visual + 1 < self.visual_to_logical.len() {
            self.logical_pos(visual + 1)
        } else {
            logical
        }
    }

    /// Move cursor one step to the left in visual order.
    ///
    /// Returns the new logical index. If already at the leftmost position,
    /// returns the current logical index unchanged.
    pub fn move_left(&self, logical: usize) -> usize {
        let visual = self.visual_pos(logical);
        if visual > 0 {
            self.logical_pos(visual - 1)
        } else {
            logical
        }
    }

    /// Number of characters in the segment.
    pub fn len(&self) -> usize {
        self.chars.len()
    }

    /// Whether the segment is empty.
    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// The base paragraph direction detected or forced for this segment.
    pub fn base_direction(&self) -> Direction {
        if self.levels.first().is_some_and(|l| l.is_rtl()) {
            // Check if paragraph level is RTL (first char's embedding level
            // may differ, but the paragraph level sets the overall direction).
            // For a simple heuristic: use the minimum even/odd level.
            Direction::Rtl
        } else {
            Direction::Ltr
        }
    }

    /// Get the character at a visual position.
    pub fn char_at_visual(&self, visual: usize) -> Option<char> {
        self.visual_to_logical
            .get(visual)
            .and_then(|&logical| self.chars.get(logical))
            .copied()
    }

    /// Build the visually reordered string.
    pub fn visual_string(&self) -> String {
        self.visual_to_logical
            .iter()
            .filter_map(|&logical| self.chars.get(logical))
            .collect()
    }

    // -- internal helpers --

    /// Convert byte-level levels to char-level levels by sampling each
    /// character's level at its starting byte offset.
    fn byte_levels_to_char_levels(text: &str, byte_levels: &[Level]) -> Vec<Level> {
        text.char_indices()
            .map(|(byte_offset, _)| byte_levels[byte_offset])
            .collect()
    }

    /// Group consecutive characters with the same level into runs.
    fn compute_runs(char_levels: &[Level]) -> Vec<BidiRun> {
        if char_levels.is_empty() {
            return Vec::new();
        }

        let mut runs = Vec::new();
        let mut start = 0;
        let mut current_level = char_levels[0];

        for (i, &level) in char_levels.iter().enumerate().skip(1) {
            if level != current_level {
                runs.push(BidiRun {
                    start,
                    end: i,
                    level: current_level,
                    direction: if current_level.is_rtl() {
                        Direction::Rtl
                    } else {
                        Direction::Ltr
                    },
                });
                start = i;
                current_level = level;
            }
        }

        // Final run.
        runs.push(BidiRun {
            start,
            end: char_levels.len(),
            level: current_level,
            direction: if current_level.is_rtl() {
                Direction::Rtl
            } else {
                Direction::Ltr
            },
        });

        runs
    }

    /// UAX#9 rule L2: compute visual ordering from per-character levels.
    ///
    /// "From the highest level found in the text to the lowest odd level on
    /// each line, reverse any contiguous sequence of characters that are at
    /// that level or higher."
    fn compute_visual_order(char_levels: &[Level]) -> Vec<usize> {
        let n = char_levels.len();
        if n == 0 {
            return Vec::new();
        }

        // Start with identity permutation.
        let mut order: Vec<usize> = (0..n).collect();

        let max_level = char_levels.iter().map(|l| l.number()).max().unwrap_or(0);

        // Find lowest odd level (minimum 1 since we only reverse odd+ levels).
        let min_odd_level = char_levels
            .iter()
            .map(|l| l.number())
            .filter(|&n| n % 2 == 1)
            .min()
            .unwrap_or(max_level + 1); // no odd levels → skip loop

        // Reverse contiguous runs at or above each level, from max down.
        for level in (min_odd_level..=max_level).rev() {
            let mut i = 0;
            while i < n {
                // levels are indexed by original logical position stored in order[i]
                if char_levels[order[i]].number() >= level {
                    let start = i;
                    while i < n && char_levels[order[i]].number() >= level {
                        i += 1;
                    }
                    order[start..i].reverse();
                } else {
                    i += 1;
                }
            }
        }

        order
    }

    /// Invert a permutation: if `perm[visual] == logical`, produce a map
    /// where `inverse[logical] == visual`.
    fn invert_permutation(perm: &[usize]) -> Vec<usize> {
        let mut inverse = vec![0; perm.len()];
        for (visual, &logical) in perm.iter().enumerate() {
            inverse[logical] = visual;
        }
        inverse
    }
}

// ---------------------------------------------------------------------------
// Standalone utility functions (pre-existing API preserved)
// ---------------------------------------------------------------------------

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

    let level = para_direction_to_level(direction);
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

    let level = para_direction_to_level(direction);
    let bidi_info = BidiInfo::new(text, level);
    bidi_info.levels.clone()
}

/// Returns `true` if the text contains any characters with RTL bidi class.
///
/// This is a cheap check to avoid calling [`reorder`] on pure-LTR text.
pub fn has_rtl(text: &str) -> bool {
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

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- reorder tests (pre-existing) ---

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
        assert_eq!(result, "\u{05DD}\u{05D5}\u{05DC}\u{05E9}");
    }

    #[test]
    fn reorder_pure_rtl_arabic() {
        // Arabic text: "مرحبا" (marhaba)
        let text = "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}";
        let result = reorder(text, ParagraphDirection::Auto);
        assert_eq!(result, "\u{0627}\u{0628}\u{062D}\u{0631}\u{0645}");
    }

    #[test]
    fn reorder_mixed_ltr_rtl() {
        let text = "Hello \u{05E9}\u{05DC}\u{05D5}\u{05DD} World";
        let result = reorder(text, ParagraphDirection::Ltr);
        assert_eq!(result, "Hello \u{05DD}\u{05D5}\u{05DC}\u{05E9} World");
    }

    #[test]
    fn reorder_forced_ltr() {
        let text = "Hello";
        assert_eq!(reorder(text, ParagraphDirection::Ltr), "Hello");
    }

    #[test]
    fn reorder_forced_rtl_on_ltr_text() {
        let text = "ABC";
        let result = reorder(text, ParagraphDirection::Rtl);
        assert_eq!(result, "ABC");
    }

    #[test]
    fn reorder_with_numbers() {
        let text = "\u{05E9}\u{05DC}\u{05D5}\u{05DD} 123";
        let result = reorder(text, ParagraphDirection::Auto);
        assert!(result.contains("123"));
    }

    #[test]
    fn reorder_with_lrm_mark() {
        let text = "A\u{200E}B";
        let result = reorder(text, ParagraphDirection::Auto);
        assert!(result.contains('A'));
        assert!(result.contains('B'));
    }

    #[test]
    fn reorder_with_rlm_mark() {
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
        for level in &levels {
            assert!(level.is_ltr(), "Expected LTR level, got {:?}", level);
        }
    }

    #[test]
    fn resolve_levels_pure_rtl() {
        let levels = resolve_levels("\u{05E9}\u{05DC}\u{05D5}\u{05DD}", ParagraphDirection::Auto);
        assert!(!levels.is_empty());
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

    #[test]
    fn is_rtl_char_additional_ranges() {
        let samples = [
            '\u{FB1D}',  // Hebrew Presentation Forms
            '\u{FB50}',  // Arabic Presentation Forms-A
            '\u{FE70}',  // Arabic Presentation Forms-B
            '\u{10800}', // Cypriot
            '\u{10840}', // Imperial Aramaic
            '\u{10900}', // Phoenician
            '\u{10920}', // Lydian
            '\u{10A00}', // Kharoshthi
            '\u{10B00}', // Avestan
            '\u{1EE00}', // Arabic Mathematical Symbols
        ];

        for sample in samples {
            assert!(is_rtl_char(sample), "Expected RTL for {sample:?}");
        }
    }

    // ===================================================================
    // BidiSegment tests (bd-ic6i.4)
    // ===================================================================

    #[test]
    fn run_is_empty() {
        let empty = BidiRun {
            start: 2,
            end: 2,
            level: Level::ltr(),
            direction: Direction::Ltr,
        };
        assert!(empty.is_empty());

        let non_empty = BidiRun {
            start: 2,
            end: 3,
            level: Level::ltr(),
            direction: Direction::Ltr,
        };
        assert!(!non_empty.is_empty());
    }

    #[test]
    fn segment_base_direction() {
        let ltr = BidiSegment::new("Hello", None);
        assert_eq!(ltr.base_direction(), Direction::Ltr);

        let rtl_text = "\u{05E9}\u{05DC}\u{05D5}\u{05DD}";
        let rtl = BidiSegment::new(rtl_text, None);
        assert_eq!(rtl.base_direction(), Direction::Rtl);
    }

    #[test]
    fn segment_compute_helpers_empty() {
        let runs = BidiSegment::compute_runs(&[]);
        assert!(runs.is_empty());

        let order = BidiSegment::compute_visual_order(&[]);
        assert!(order.is_empty());
    }

    #[test]
    fn segment_empty() {
        let seg = BidiSegment::new("", None);
        assert!(seg.is_empty());
        assert_eq!(seg.len(), 0);
        assert!(seg.runs.is_empty());
        assert!(seg.visual_to_logical.is_empty());
        assert!(seg.logical_to_visual.is_empty());
    }

    #[test]
    fn segment_ltr_only() {
        let seg = BidiSegment::new("Hello", None);
        assert_eq!(seg.len(), 5);
        assert_eq!(seg.chars, vec!['H', 'e', 'l', 'l', 'o']);

        // LTR text: visual == logical order.
        for i in 0..5 {
            assert_eq!(seg.visual_pos(i), i);
            assert_eq!(seg.logical_pos(i), i);
            assert!(!seg.is_rtl(i));
        }

        // Single LTR run.
        assert_eq!(seg.runs.len(), 1);
        assert_eq!(seg.runs[0].direction, Direction::Ltr);
        assert_eq!(seg.runs[0].start, 0);
        assert_eq!(seg.runs[0].end, 5);

        assert_eq!(seg.visual_string(), "Hello");
    }

    #[test]
    fn segment_rtl_only() {
        // Hebrew "שלום" (4 chars)
        let text = "\u{05E9}\u{05DC}\u{05D5}\u{05DD}";
        let seg = BidiSegment::new(text, None);
        assert_eq!(seg.len(), 4);

        // Pure RTL: visual order is reversed from logical.
        // Logical: [ש(0), ל(1), ו(2), ם(3)]
        // Visual:  [ם(3), ו(2), ל(1), ש(0)]
        assert_eq!(seg.visual_to_logical, vec![3, 2, 1, 0]);
        assert_eq!(seg.logical_to_visual, vec![3, 2, 1, 0]);

        for i in 0..4 {
            assert!(seg.is_rtl(i));
        }

        assert_eq!(seg.runs.len(), 1);
        assert_eq!(seg.runs[0].direction, Direction::Rtl);

        // Visual string should match reorder() output.
        assert_eq!(seg.visual_string(), "\u{05DD}\u{05D5}\u{05DC}\u{05E9}");
    }

    #[test]
    fn segment_mixed_ltr_rtl() {
        // "Hello שלום World" in LTR paragraph
        let text = "Hello \u{05E9}\u{05DC}\u{05D5}\u{05DD} World";
        let seg = BidiSegment::new(text, Some(Direction::Ltr));

        // Logical chars: H(0) e(1) l(2) l(3) o(4) ' '(5)
        //                ש(6) ל(7) ו(8) ם(9)
        //                ' '(10) W(11) o(12) r(13) l(14) d(15)
        assert_eq!(seg.len(), 16);

        // LTR chars stay in place, Hebrew reversed.
        // Visual: H e l l o ' ' ם ו ל ש ' ' W o r l d
        //         0 1 2 3 4 5  9 8 7 6  10 11 12 13 14 15
        assert_eq!(seg.visual_pos(0), 0); // H
        assert_eq!(seg.visual_pos(5), 5); // space
        assert_eq!(seg.visual_pos(6), 9); // ש at visual pos 9
        assert_eq!(seg.visual_pos(9), 6); // ם at visual pos 6
        assert_eq!(seg.visual_pos(11), 11); // W

        // Hebrew chars are RTL, others are LTR.
        assert!(!seg.is_rtl(0)); // H
        assert!(seg.is_rtl(6)); // ש
        assert!(seg.is_rtl(9)); // ם
        assert!(!seg.is_rtl(11)); // W

        // Multiple runs: LTR, RTL, LTR (spaces attach to adjacent runs per UAX#9)
        assert!(seg.runs.len() >= 2);
    }

    #[test]
    fn segment_numbers_in_rtl() {
        // Numbers stay LTR even in RTL context: "שלום 123"
        let text = "\u{05E9}\u{05DC}\u{05D5}\u{05DD} 123";
        let seg = BidiSegment::new(text, None);

        // In an RTL paragraph, numbers maintain LTR order.
        let visual = seg.visual_string();
        // The visual string should have 123 in correct order.
        assert!(
            visual.contains("123"),
            "Numbers should stay in LTR order: {visual}"
        );

        // The number characters themselves should resolve to LTR embedding.
        // (Their levels are even = LTR, but within an RTL paragraph they
        // are still displayed in correct numeric order.)
        let num_start = text.chars().position(|c| c == '1').unwrap();
        // Numbers are weak directional — they keep LTR internal order.
        assert!(
            !seg.is_rtl(num_start),
            "Digit '1' should resolve to LTR level"
        );
    }

    #[test]
    fn segment_brackets_pairing() {
        // UAX#9 N0 handles bracket pairing. In an RTL paragraph,
        // matching brackets should mirror: (foo) becomes (foo) visually
        // but the bracket pair stays correctly matched.
        let text = "\u{05D0}(\u{05D1})\u{05D2}"; // א(ב)ג
        let seg = BidiSegment::new(text, Some(Direction::Rtl));

        // The visual string should have brackets correctly paired.
        let visual = seg.visual_string();
        // In RTL display: ג(ב)א — brackets are mirrored by the terminal,
        // but the algorithm preserves pairing. The important thing is no
        // panic and the mapping is a valid permutation.
        assert_eq!(visual.chars().count(), text.chars().count());

        // Verify it's a valid permutation.
        let mut sorted_vtl = seg.visual_to_logical.clone();
        sorted_vtl.sort();
        let expected: Vec<usize> = (0..seg.len()).collect();
        assert_eq!(
            sorted_vtl, expected,
            "visual_to_logical must be a valid permutation"
        );
    }

    #[test]
    fn segment_explicit_markers() {
        // LRM (U+200E) and RLM (U+200F) are directional marks.
        // They affect level resolution but may be included/excluded
        // depending on the algorithm. Verify no panic and valid mapping.
        let text = "A\u{200E}B\u{200F}C";
        let seg = BidiSegment::new(text, None);

        // The segment should handle markers without panicking.
        assert!(!seg.is_empty());

        // Verify permutation validity.
        let mut sorted_vtl = seg.visual_to_logical.clone();
        sorted_vtl.sort();
        let expected: Vec<usize> = (0..seg.len()).collect();
        assert_eq!(sorted_vtl, expected);
    }

    #[test]
    fn segment_cursor_movement() {
        // "Hello שלום" with LTR paragraph
        // Logical: H(0) e(1) l(2) l(3) o(4) ' '(5) ש(6) ל(7) ו(8) ם(9)
        // Visual:  H(0) e(1) l(2) l(3) o(4) ' '(5) ם(9) ו(8) ל(7) ש(6)
        let text = "Hello \u{05E9}\u{05DC}\u{05D5}\u{05DD}";
        let seg = BidiSegment::new(text, Some(Direction::Ltr));

        // Start at logical 0 (H, visual 0).
        let mut pos = 0;

        // Move right 6 times: should go through LTR then into RTL region.
        for _ in 0..6 {
            pos = seg.move_right(pos);
        }
        // After 6 right moves from visual 0, we should be at visual 6.
        assert_eq!(seg.visual_pos(pos), 6);

        // Move left once.
        pos = seg.move_left(pos);
        assert_eq!(seg.visual_pos(pos), 5);

        // Move left all the way to 0.
        for _ in 0..5 {
            pos = seg.move_left(pos);
        }
        assert_eq!(seg.visual_pos(pos), 0);

        // At visual 0, moving left should stay at 0.
        let same = seg.move_left(pos);
        assert_eq!(seg.visual_pos(same), 0);
    }

    #[test]
    fn segment_cursor_at_boundary() {
        // At the rightmost position, move_right should be a no-op.
        let seg = BidiSegment::new("ABC", None);
        let last = seg.move_right(seg.move_right(0)); // visual pos 2
        assert_eq!(seg.visual_pos(last), 2);
        let still_last = seg.move_right(last);
        assert_eq!(still_last, last);
    }

    #[test]
    fn segment_double_toggle() {
        // Invariant: moving right then left returns to original position.
        let text = "Hello \u{05E9}\u{05DC}\u{05D5}\u{05DD} World";
        let seg = BidiSegment::new(text, Some(Direction::Ltr));

        for start in 0..seg.len() {
            let right = seg.move_right(start);
            if right != start {
                let back = seg.move_left(right);
                assert_eq!(
                    back, start,
                    "move_left(move_right({start})) should return {start}, got {back}"
                );
            }
        }
    }

    #[test]
    fn segment_visual_string_matches_reorder() {
        // BidiSegment.visual_string() should produce the same result as
        // the standalone reorder() for the same text and direction.
        let text = "Hello \u{05E9}\u{05DC}\u{05D5}\u{05DD} World";
        let seg = BidiSegment::new(text, Some(Direction::Ltr));
        let reordered = reorder(text, ParagraphDirection::Ltr);
        assert_eq!(seg.visual_string(), reordered);
    }

    #[test]
    fn segment_run_coverage() {
        // Runs should cover every character exactly once.
        let text = "Hello \u{05E9}\u{05DC}\u{05D5}\u{05DD} World";
        let seg = BidiSegment::new(text, Some(Direction::Ltr));

        let total_chars: usize = seg.runs.iter().map(|r| r.len()).sum();
        assert_eq!(total_chars, seg.len());

        // Runs should be contiguous and non-overlapping.
        for window in seg.runs.windows(2) {
            assert_eq!(window[0].end, window[1].start);
        }
        if let Some(first) = seg.runs.first() {
            assert_eq!(first.start, 0);
        }
        if let Some(last) = seg.runs.last() {
            assert_eq!(last.end, seg.len());
        }
    }

    #[test]
    fn segment_permutation_validity() {
        // Both maps must be valid permutations and inverses of each other.
        let texts = [
            "Hello",
            "\u{05E9}\u{05DC}\u{05D5}\u{05DD}",
            "Hello \u{05E9}\u{05DC}\u{05D5}\u{05DD} World",
            "ABC 123 \u{0645}\u{0631}\u{062D}\u{0628}\u{0627}",
            "",
        ];

        for text in texts {
            let seg = BidiSegment::new(text, None);
            let n = seg.len();

            // Check sizes.
            assert_eq!(seg.visual_to_logical.len(), n);
            assert_eq!(seg.logical_to_visual.len(), n);

            // Check that they are inverse permutations.
            for i in 0..n {
                assert_eq!(
                    seg.logical_to_visual[seg.visual_to_logical[i]], i,
                    "vtl->ltv roundtrip failed for text={text:?} at visual={i}"
                );
                assert_eq!(
                    seg.visual_to_logical[seg.logical_to_visual[i]], i,
                    "ltv->vtl roundtrip failed for text={text:?} at logical={i}"
                );
            }
        }
    }

    #[test]
    fn segment_char_at_visual() {
        let seg = BidiSegment::new("ABC", None);
        assert_eq!(seg.char_at_visual(0), Some('A'));
        assert_eq!(seg.char_at_visual(1), Some('B'));
        assert_eq!(seg.char_at_visual(2), Some('C'));
        assert_eq!(seg.char_at_visual(3), None);
    }

    #[test]
    fn segment_out_of_bounds_graceful() {
        let seg = BidiSegment::new("AB", None);
        // Out-of-bounds lookups should return the index unchanged (fallback).
        assert_eq!(seg.visual_pos(99), 99);
        assert_eq!(seg.logical_pos(99), 99);
    }
}
