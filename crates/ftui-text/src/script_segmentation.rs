#![forbid(unsafe_code)]

//! Script segmentation and bidi-safe text run partitioning.
//!
//! This module provides deterministic text-run segmentation by Unicode script,
//! bidi direction, and style — preparing robust shaping inputs and consistent
//! cache keys for the downstream HarfBuzz shaping pipeline.
//!
//! # Design
//!
//! Text shaping engines (HarfBuzz, CoreText, DirectWrite) require input to be
//! split into runs that share the same **script**, **direction**, and **style**.
//! Mixing scripts in a single shaping call produces incorrect glyph selection
//! and positioning.
//!
//! This module implements a three-phase algorithm:
//!
//! 1. **Raw classification** — assign each character its Unicode script via
//!    block-range lookup (`char_script`).
//! 2. **Common/Inherited resolution** — resolve `Common` and `Inherited`
//!    characters by propagating adjacent specific scripts (UAX#24-inspired).
//! 3. **Run grouping** — collect contiguous characters sharing the same
//!    resolved script into [`ScriptRun`] spans.
//!
//! The [`TextRun`] type further subdivides by direction and style, producing
//! the atomic units suitable for shaping. [`RunCacheKey`] provides a
//! deterministic, hashable identifier for caching shaped glyph output.
//!
//! # Example
//!
//! ```
//! use ftui_text::script_segmentation::{Script, ScriptRun, partition_by_script};
//!
//! let runs = partition_by_script("Hello مرحبا World");
//! assert!(runs.len() >= 2); // At least Latin and Arabic runs
//! assert_eq!(runs[0].script, Script::Latin);
//! ```

use std::hash::{Hash, Hasher};

// ---------------------------------------------------------------------------
// Script enum
// ---------------------------------------------------------------------------

/// Unicode script classification for shaping.
///
/// Covers the major scripts encountered in terminal and UI text rendering.
/// Scripts not explicitly listed fall under `Unknown`.
///
/// `Common` represents script-neutral characters (spaces, digits, ASCII
/// punctuation) and `Inherited` represents combining marks that inherit
/// the script of their base character. Both are resolved to a specific
/// script during run partitioning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Script {
    /// Script-neutral: spaces, digits, basic punctuation, symbols.
    Common = 0,
    /// Combining marks that inherit the base character's script.
    Inherited,
    /// Latin script (English, French, German, Vietnamese, etc.).
    Latin,
    /// Greek script.
    Greek,
    /// Cyrillic script (Russian, Ukrainian, Bulgarian, etc.).
    Cyrillic,
    /// Armenian script.
    Armenian,
    /// Hebrew script.
    Hebrew,
    /// Arabic script (Arabic, Persian, Urdu, etc.).
    Arabic,
    /// Syriac script.
    Syriac,
    /// Thaana script (Maldivian).
    Thaana,
    /// Devanagari script (Hindi, Sanskrit, Marathi, etc.).
    Devanagari,
    /// Bengali script.
    Bengali,
    /// Gurmukhi script (Punjabi).
    Gurmukhi,
    /// Gujarati script.
    Gujarati,
    /// Oriya script.
    Oriya,
    /// Tamil script.
    Tamil,
    /// Telugu script.
    Telugu,
    /// Kannada script.
    Kannada,
    /// Malayalam script.
    Malayalam,
    /// Sinhala script.
    Sinhala,
    /// Thai script.
    Thai,
    /// Lao script.
    Lao,
    /// Tibetan script.
    Tibetan,
    /// Myanmar script (Burmese).
    Myanmar,
    /// Georgian script.
    Georgian,
    /// Hangul script (Korean).
    Hangul,
    /// Ethiopic script (Amharic, Tigrinya, etc.).
    Ethiopic,
    /// CJK Unified Ideographs (Chinese, Japanese Kanji, Korean Hanja).
    Han,
    /// Hiragana (Japanese).
    Hiragana,
    /// Katakana (Japanese).
    Katakana,
    /// Bopomofo (Chinese phonetic).
    Bopomofo,
    /// Unknown or unrecognized script.
    Unknown,
}

impl Script {
    /// Whether this is a "weak" script that should be resolved from context.
    #[inline]
    pub const fn is_common_or_inherited(self) -> bool {
        matches!(self, Script::Common | Script::Inherited)
    }

    /// Whether this script is typically written right-to-left.
    #[inline]
    pub const fn is_rtl(self) -> bool {
        matches!(
            self,
            Script::Arabic | Script::Hebrew | Script::Syriac | Script::Thaana
        )
    }
}

// ---------------------------------------------------------------------------
// Character-to-script detection
// ---------------------------------------------------------------------------

/// Classify a character's Unicode script via block-range lookup.
///
/// This uses hardcoded Unicode block ranges rather than an external crate,
/// keeping the dependency footprint minimal. Coverage targets the scripts
/// most commonly encountered in terminal/UI text. Characters outside
/// recognized ranges return `Script::Unknown`.
#[inline]
pub fn char_script(c: char) -> Script {
    let cp = c as u32;
    match cp {
        // ASCII and Basic Latin
        // Letters are Latin; digits, punctuation, symbols are Common
        0x0000..=0x0040 => Script::Common, // Controls, space, ! " # $ % & ' ( ) * + , - . / 0-9 : ; < = > ? @
        0x0041..=0x005A => Script::Latin,  // A-Z
        0x005B..=0x0060 => Script::Common, // [ \ ] ^ _ `
        0x0061..=0x007A => Script::Latin,  // a-z
        0x007B..=0x00BF => Script::Common, // { | } ~ DEL, Latin-1 Supplement (controls, symbols, punctuation)
        0x00C0..=0x00D6 => Script::Latin,  // À-Ö
        0x00D7 => Script::Common,          // ×
        0x00D8..=0x00F6 => Script::Latin,  // Ø-ö
        0x00F7 => Script::Common,          // ÷
        0x00F8..=0x024F => Script::Latin,  // ø-ɏ (Latin Extended-A & B)
        0x0250..=0x02AF => Script::Latin,  // IPA Extensions (Latin)
        0x02B0..=0x02FF => Script::Common, // Spacing Modifier Letters
        0x0300..=0x036F => Script::Inherited, // Combining Diacritical Marks

        // Greek and Coptic
        0x0370..=0x03FF => Script::Greek,
        0x1F00..=0x1FFF => Script::Greek, // Greek Extended

        // Cyrillic
        0x0400..=0x04FF => Script::Cyrillic,
        0x0500..=0x052F => Script::Cyrillic, // Cyrillic Supplement
        0x2DE0..=0x2DFF => Script::Cyrillic, // Cyrillic Extended-A
        0xA640..=0xA69F => Script::Cyrillic, // Cyrillic Extended-B
        0x1C80..=0x1C8F => Script::Cyrillic, // Cyrillic Extended-C

        // Armenian
        0x0530..=0x058F => Script::Armenian,
        0xFB13..=0xFB17 => Script::Armenian, // Armenian ligatures

        // Hebrew
        0x0590..=0x05FF => Script::Hebrew,
        0xFB1D..=0xFB4F => Script::Hebrew, // Hebrew Presentation Forms

        // Arabic
        0x0600..=0x06FF => Script::Arabic,
        0x0750..=0x077F => Script::Arabic, // Arabic Supplement
        0x08A0..=0x08FF => Script::Arabic, // Arabic Extended-A
        0xFB50..=0xFDFF => Script::Arabic, // Arabic Presentation Forms-A
        0xFE70..=0xFEFF => Script::Arabic, // Arabic Presentation Forms-B

        // Syriac
        0x0700..=0x074F => Script::Syriac,
        0x0860..=0x086F => Script::Syriac, // Syriac Supplement

        // Thaana
        0x0780..=0x07BF => Script::Thaana,

        // Devanagari
        0x0900..=0x097F => Script::Devanagari,
        0xA8E0..=0xA8FF => Script::Devanagari, // Devanagari Extended

        // Bengali
        0x0980..=0x09FF => Script::Bengali,

        // Gurmukhi
        0x0A00..=0x0A7F => Script::Gurmukhi,

        // Gujarati
        0x0A80..=0x0AFF => Script::Gujarati,

        // Oriya
        0x0B00..=0x0B7F => Script::Oriya,

        // Tamil
        0x0B80..=0x0BFF => Script::Tamil,

        // Telugu
        0x0C00..=0x0C7F => Script::Telugu,

        // Kannada
        0x0C80..=0x0CFF => Script::Kannada,

        // Malayalam
        0x0D00..=0x0D7F => Script::Malayalam,

        // Sinhala
        0x0D80..=0x0DFF => Script::Sinhala,

        // Thai
        0x0E00..=0x0E7F => Script::Thai,

        // Lao
        0x0E80..=0x0EFF => Script::Lao,

        // Tibetan
        0x0F00..=0x0FFF => Script::Tibetan,

        // Myanmar
        0x1000..=0x109F => Script::Myanmar,
        0xAA60..=0xAA7F => Script::Myanmar, // Myanmar Extended-A

        // Georgian
        0x10A0..=0x10FF => Script::Georgian,
        0x2D00..=0x2D2F => Script::Georgian, // Georgian Supplement
        0x1C90..=0x1CBF => Script::Georgian, // Georgian Extended

        // Hangul
        0x1100..=0x11FF => Script::Hangul, // Hangul Jamo
        0x3130..=0x318F => Script::Hangul, // Hangul Compatibility Jamo
        0xA960..=0xA97F => Script::Hangul, // Hangul Jamo Extended-A
        0xAC00..=0xD7AF => Script::Hangul, // Hangul Syllables
        0xD7B0..=0xD7FF => Script::Hangul, // Hangul Jamo Extended-B

        // Ethiopic
        0x1200..=0x137F => Script::Ethiopic,
        0x1380..=0x139F => Script::Ethiopic, // Ethiopic Supplement
        0x2D80..=0x2DDF => Script::Ethiopic, // Ethiopic Extended
        0xAB00..=0xAB2F => Script::Ethiopic, // Ethiopic Extended-A

        // Latin Extended Additional / Extended-C / Extended-D / Extended-E
        0x1E00..=0x1EFF => Script::Latin, // Latin Extended Additional
        0x2C60..=0x2C7F => Script::Latin, // Latin Extended-C
        0xA720..=0xA7FF => Script::Latin, // Latin Extended-D
        0xAB30..=0xAB6F => Script::Latin, // Latin Extended-E
        0xFB00..=0xFB06 => Script::Latin, // Latin ligatures

        // CJK / Han
        0x2E80..=0x2EFF => Script::Han,   // CJK Radicals Supplement
        0x2F00..=0x2FDF => Script::Han,   // Kangxi Radicals
        0x3400..=0x4DBF => Script::Han,   // CJK Unified Ideographs Extension A
        0x4E00..=0x9FFF => Script::Han,   // CJK Unified Ideographs
        0xF900..=0xFAFF => Script::Han,   // CJK Compatibility Ideographs
        0x20000..=0x2A6DF => Script::Han, // CJK Extension B
        0x2A700..=0x2B73F => Script::Han, // CJK Extension C
        0x2B740..=0x2B81F => Script::Han, // CJK Extension D
        0x2B820..=0x2CEAF => Script::Han, // CJK Extension E
        0x2CEB0..=0x2EBEF => Script::Han, // CJK Extension F
        0x30000..=0x3134F => Script::Han, // CJK Extension G

        // Hiragana
        0x3040..=0x309F => Script::Hiragana,
        0x1B001..=0x1B11F => Script::Hiragana, // Hiragana Extended

        // Katakana
        0x30A0..=0x30FF => Script::Katakana,
        0x31F0..=0x31FF => Script::Katakana, // Katakana Phonetic Extensions
        0xFF65..=0xFF9F => Script::Katakana, // Halfwidth Katakana

        // Bopomofo
        0x3100..=0x312F => Script::Bopomofo,
        0x31A0..=0x31BF => Script::Bopomofo, // Bopomofo Extended

        // CJK symbols and punctuation — Common (shared across CJK scripts)
        0x3000..=0x303F => Script::Common,

        // General Punctuation, Superscripts, Currency, Letterlike, Number Forms
        0x2000..=0x206F => Script::Common, // General Punctuation
        0x2070..=0x209F => Script::Common, // Superscripts and Subscripts
        0x20A0..=0x20CF => Script::Common, // Currency Symbols
        0x20D0..=0x20FF => Script::Inherited, // Combining Marks for Symbols
        0x2100..=0x214F => Script::Common, // Letterlike Symbols
        0x2150..=0x218F => Script::Common, // Number Forms
        0x2190..=0x21FF => Script::Common, // Arrows
        0x2200..=0x22FF => Script::Common, // Mathematical Operators
        0x2300..=0x23FF => Script::Common, // Miscellaneous Technical
        0x2400..=0x243F => Script::Common, // Control Pictures
        0x2440..=0x245F => Script::Common, // OCR
        0x2460..=0x24FF => Script::Common, // Enclosed Alphanumerics
        0x2500..=0x257F => Script::Common, // Box Drawing
        0x2580..=0x259F => Script::Common, // Block Elements
        0x25A0..=0x25FF => Script::Common, // Geometric Shapes
        0x2600..=0x26FF => Script::Common, // Miscellaneous Symbols
        0x2700..=0x27BF => Script::Common, // Dingbats
        0x27C0..=0x27EF => Script::Common, // Misc Mathematical Symbols-A
        0x27F0..=0x27FF => Script::Common, // Supplemental Arrows-A
        0x2800..=0x28FF => Script::Common, // Braille Patterns
        0x2900..=0x297F => Script::Common, // Supplemental Arrows-B
        0x2980..=0x29FF => Script::Common, // Misc Mathematical Symbols-B
        0x2A00..=0x2AFF => Script::Common, // Supplemental Mathematical Operators
        0x2B00..=0x2BFF => Script::Common, // Miscellaneous Symbols and Arrows

        // Halfwidth and Fullwidth Forms (Latin part)
        0xFF01..=0xFF5E => Script::Latin, // Fullwidth ASCII variants
        0xFF61..=0xFF64 => Script::Common, // Halfwidth CJK punctuation

        // Emoji and symbols (Common)
        0xFE00..=0xFE0F => Script::Inherited, // Variation Selectors
        0xE0100..=0xE01EF => Script::Inherited, // Variation Selectors Supplement
        0x1F000..=0x1FAFF => Script::Common,  // Emoji and symbols blocks
        0xFE10..=0xFE1F => Script::Common,    // Vertical Forms
        0xFE20..=0xFE2F => Script::Inherited, // Combining Half Marks
        0xFE30..=0xFE4F => Script::Common,    // CJK Compatibility Forms
        0xFE50..=0xFE6F => Script::Common,    // Small Form Variants

        // NKo
        0x07C0..=0x07FF => Script::Arabic, // Treat NKo as Arabic for shaping

        // Fallback
        _ => Script::Unknown,
    }
}

// ---------------------------------------------------------------------------
// ScriptRun
// ---------------------------------------------------------------------------

/// A contiguous run of characters sharing the same resolved script.
///
/// Indices are byte offsets into the source string for efficient slicing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptRun {
    /// Start byte offset (inclusive) in the source string.
    pub start: usize,
    /// End byte offset (exclusive) in the source string.
    pub end: usize,
    /// Resolved script for this run.
    pub script: Script,
}

impl ScriptRun {
    /// The byte length of this run.
    #[inline]
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    /// Whether the run is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Extract the text slice from the source string.
    #[inline]
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.start..self.end]
    }
}

// ---------------------------------------------------------------------------
// Script resolution (Common/Inherited → specific)
// ---------------------------------------------------------------------------

/// Resolve Common and Inherited scripts to the nearest specific script.
///
/// Uses a two-pass approach:
/// 1. Forward pass: Inherited characters take the script of the preceding
///    specific character.
/// 2. Backward pass: Common characters at the start take the script of
///    the first following specific character. Common characters between
///    specific runs are assigned to the preceding run.
fn resolve_scripts(chars: &[char]) -> Vec<Script> {
    let n = chars.len();
    if n == 0 {
        return Vec::new();
    }

    let mut scripts: Vec<Script> = chars.iter().map(|&c| char_script(c)).collect();

    // Forward pass: resolve Inherited from the left.
    // Also resolve Common that follows a specific script.
    let mut last_specific = Script::Common;
    for script in &mut scripts {
        if *script == Script::Inherited {
            *script = if last_specific.is_common_or_inherited() {
                Script::Common // will be resolved in backward pass
            } else {
                last_specific
            };
        } else if !script.is_common_or_inherited() {
            last_specific = *script;
        }
    }

    // Backward pass: resolve remaining Common characters.
    // Find the first specific script and backfill leading Common chars.
    let first_specific = scripts
        .iter()
        .find(|s| !s.is_common_or_inherited())
        .copied()
        .unwrap_or(Script::Latin); // All-Common text defaults to Latin

    // Assign leading Common characters the first specific script.
    for script in &mut scripts {
        if script.is_common_or_inherited() {
            *script = first_specific;
        } else {
            break;
        }
    }

    // Forward pass again: remaining Common chars take the preceding specific.
    let mut current = first_specific;
    for script in &mut scripts {
        if script.is_common_or_inherited() {
            *script = current;
        } else {
            current = *script;
        }
    }

    scripts
}

// ---------------------------------------------------------------------------
// partition_by_script
// ---------------------------------------------------------------------------

/// Partition text into contiguous runs of the same Unicode script.
///
/// Common characters (spaces, digits, punctuation) and Inherited characters
/// (combining marks) are resolved to their surrounding script context using
/// a UAX#24-inspired algorithm, preventing unnecessary run breaks at
/// whitespace and punctuation boundaries.
///
/// Returns an empty vec for empty input.
///
/// # Example
///
/// ```
/// use ftui_text::script_segmentation::{Script, partition_by_script};
///
/// let runs = partition_by_script("Hello World");
/// assert_eq!(runs.len(), 1);
/// assert_eq!(runs[0].script, Script::Latin);
///
/// // Mixed scripts produce multiple runs
/// let runs = partition_by_script("Helloこんにちは");
/// assert!(runs.len() >= 2);
/// ```
pub fn partition_by_script(text: &str) -> Vec<ScriptRun> {
    if text.is_empty() {
        return Vec::new();
    }

    let chars: Vec<char> = text.chars().collect();
    let resolved = resolve_scripts(&chars);

    let mut runs = Vec::new();
    let mut byte_offset = 0;
    let mut run_start = 0;
    let mut current_script = resolved[0];

    for (i, ch) in chars.iter().enumerate() {
        let char_len = ch.len_utf8();

        if resolved[i] != current_script {
            runs.push(ScriptRun {
                start: run_start,
                end: byte_offset,
                script: current_script,
            });
            run_start = byte_offset;
            current_script = resolved[i];
        }

        byte_offset += char_len;
    }

    // Final run.
    runs.push(ScriptRun {
        start: run_start,
        end: byte_offset,
        script: current_script,
    });

    runs
}

// ---------------------------------------------------------------------------
// TextRun (script + direction + style)
// ---------------------------------------------------------------------------

/// A text direction for run partitioning.
///
/// This is a local enum to avoid a hard dependency on the `bidi` feature.
/// When bidi is enabled, use `Direction::from_bidi()` for conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunDirection {
    /// Left-to-right.
    Ltr,
    /// Right-to-left.
    Rtl,
}

/// A fully partitioned text run suitable for shaping.
///
/// Combines script, direction, and style identity into the atomic unit
/// that a shaping engine processes. Each field boundary triggers a new run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextRun {
    /// Start byte offset (inclusive) in the source string.
    pub start: usize,
    /// End byte offset (exclusive) in the source string.
    pub end: usize,
    /// Resolved Unicode script.
    pub script: Script,
    /// Text direction for this run.
    pub direction: RunDirection,
    /// Opaque style discriminant for cache keying.
    /// Two runs with different styles must be shaped separately even if
    /// script and direction match (e.g., bold vs regular affects glyph selection).
    pub style_id: u64,
}

impl TextRun {
    /// The byte length of this run.
    #[inline]
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    /// Whether the run is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Extract the text slice from the source string.
    #[inline]
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.start..self.end]
    }

    /// Produce a deterministic cache key for this run's shaped output.
    #[inline]
    pub fn cache_key<'a>(&self, source: &'a str) -> RunCacheKey<'a> {
        RunCacheKey {
            text: self.text(source),
            script: self.script,
            direction: self.direction,
            style_id: self.style_id,
        }
    }
}

// ---------------------------------------------------------------------------
// RunCacheKey
// ---------------------------------------------------------------------------

/// Deterministic, hashable cache key for shaped glyph output.
///
/// Two runs producing equal `RunCacheKey` values can share the same
/// shaped glyph buffer, enabling efficient caching of shaping results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunCacheKey<'a> {
    /// The text content of the run.
    pub text: &'a str,
    /// The resolved script.
    pub script: Script,
    /// The text direction.
    pub direction: RunDirection,
    /// Style discriminant (e.g., hash of font weight + style + size).
    pub style_id: u64,
}

impl Hash for RunCacheKey<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.text.hash(state);
        self.script.hash(state);
        self.direction.hash(state);
        self.style_id.hash(state);
    }
}

// ---------------------------------------------------------------------------
// partition_text_runs — full run partitioning
// ---------------------------------------------------------------------------

/// Partition text into fully-resolved text runs by script and direction.
///
/// This is the primary entry point for preparing shaping input. Each
/// returned [`TextRun`] has a uniform script, direction, and style,
/// suitable for passing directly to a shaping engine.
///
/// `direction_fn` provides per-byte-offset direction resolution. If `None`,
/// direction is inferred from the script's natural direction (RTL for
/// Arabic/Hebrew/Syriac/Thaana, LTR for everything else).
///
/// `style_fn` provides a style discriminant for each byte offset. Runs
/// are split whenever the style changes. If `None`, all text is treated
/// as having the same style (style_id = 0).
///
/// # Example
///
/// ```
/// use ftui_text::script_segmentation::{partition_text_runs, Script, RunDirection};
///
/// let runs = partition_text_runs("Hello World", None, None);
/// assert_eq!(runs.len(), 1);
/// assert_eq!(runs[0].script, Script::Latin);
/// assert_eq!(runs[0].direction, RunDirection::Ltr);
/// ```
pub fn partition_text_runs(
    text: &str,
    direction_fn: Option<&dyn Fn(usize) -> RunDirection>,
    style_fn: Option<&dyn Fn(usize) -> u64>,
) -> Vec<TextRun> {
    if text.is_empty() {
        return Vec::new();
    }

    let script_runs = partition_by_script(text);

    let default_direction = |script: Script| -> RunDirection {
        if script.is_rtl() {
            RunDirection::Rtl
        } else {
            RunDirection::Ltr
        }
    };

    let mut runs = Vec::new();

    for sr in &script_runs {
        // Further subdivide each script run by direction and style.
        let sub_text = &text[sr.start..sr.end];
        let mut sub_start = sr.start;

        let first_dir = direction_fn
            .as_ref()
            .map_or_else(|| default_direction(sr.script), |f| f(sr.start));
        let first_style = style_fn.as_ref().map_or(0u64, |f| f(sr.start));

        let mut current_dir = first_dir;
        let mut current_style = first_style;

        for (i, ch) in sub_text.char_indices() {
            let byte_pos = sr.start + i;
            let dir = direction_fn
                .as_ref()
                .map_or_else(|| default_direction(sr.script), |f| f(byte_pos));
            let style = style_fn.as_ref().map_or(0u64, |f| f(byte_pos));

            if dir != current_dir || style != current_style {
                // Emit run up to this point.
                if byte_pos > sub_start {
                    runs.push(TextRun {
                        start: sub_start,
                        end: byte_pos,
                        script: sr.script,
                        direction: current_dir,
                        style_id: current_style,
                    });
                }
                sub_start = byte_pos;
                current_dir = dir;
                current_style = style;
            }

            // Advance past this character (handled by char_indices).
            let _ = ch;
        }

        // Final sub-run.
        if sr.end > sub_start {
            runs.push(TextRun {
                start: sub_start,
                end: sr.end,
                script: sr.script,
                direction: current_dir,
                style_id: current_style,
            });
        }
    }

    runs
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // char_script tests
    // -----------------------------------------------------------------------

    #[test]
    fn script_ascii_letters() {
        assert_eq!(char_script('A'), Script::Latin);
        assert_eq!(char_script('z'), Script::Latin);
        assert_eq!(char_script('M'), Script::Latin);
    }

    #[test]
    fn script_ascii_digits_are_common() {
        for d in '0'..='9' {
            assert_eq!(char_script(d), Script::Common, "digit {d}");
        }
    }

    #[test]
    fn script_ascii_punctuation_is_common() {
        for &c in &[' ', '!', '.', ',', ':', ';', '?', '-', '(', ')', '[', ']'] {
            assert_eq!(char_script(c), Script::Common, "char {c:?}");
        }
    }

    #[test]
    fn script_latin_extended() {
        assert_eq!(char_script('\u{00C0}'), Script::Latin); // À
        assert_eq!(char_script('\u{00E9}'), Script::Latin); // é
        assert_eq!(char_script('\u{0148}'), Script::Latin); // ň
        assert_eq!(char_script('\u{1E00}'), Script::Latin); // Latin Extended Additional
    }

    #[test]
    fn script_greek() {
        assert_eq!(char_script('\u{0391}'), Script::Greek); // Α
        assert_eq!(char_script('\u{03B1}'), Script::Greek); // α
        assert_eq!(char_script('\u{03C9}'), Script::Greek); // ω
    }

    #[test]
    fn script_cyrillic() {
        assert_eq!(char_script('\u{0410}'), Script::Cyrillic); // А
        assert_eq!(char_script('\u{044F}'), Script::Cyrillic); // я
    }

    #[test]
    fn script_hebrew() {
        assert_eq!(char_script('\u{05D0}'), Script::Hebrew); // א
        assert_eq!(char_script('\u{05EA}'), Script::Hebrew); // ת
    }

    #[test]
    fn script_arabic() {
        assert_eq!(char_script('\u{0627}'), Script::Arabic); // ا
        assert_eq!(char_script('\u{0645}'), Script::Arabic); // م
    }

    #[test]
    fn script_devanagari() {
        assert_eq!(char_script('\u{0905}'), Script::Devanagari); // अ
        assert_eq!(char_script('\u{0939}'), Script::Devanagari); // ह
    }

    #[test]
    fn script_thai() {
        assert_eq!(char_script('\u{0E01}'), Script::Thai); // ก
        assert_eq!(char_script('\u{0E3F}'), Script::Thai); // ฿
    }

    #[test]
    fn script_hangul() {
        assert_eq!(char_script('\u{AC00}'), Script::Hangul); // 가
        assert_eq!(char_script('\u{D7A3}'), Script::Hangul); // 힣
    }

    #[test]
    fn script_cjk_han() {
        assert_eq!(char_script('\u{4E00}'), Script::Han); // 一
        assert_eq!(char_script('\u{9FFF}'), Script::Han); // last CJK Unified
    }

    #[test]
    fn script_hiragana_katakana() {
        assert_eq!(char_script('\u{3042}'), Script::Hiragana); // あ
        assert_eq!(char_script('\u{30A2}'), Script::Katakana); // ア
    }

    #[test]
    fn script_combining_marks_are_inherited() {
        assert_eq!(char_script('\u{0300}'), Script::Inherited); // combining grave
        assert_eq!(char_script('\u{0301}'), Script::Inherited); // combining acute
        assert_eq!(char_script('\u{036F}'), Script::Inherited); // last combining diacritical
    }

    #[test]
    fn script_rtl_detection() {
        assert!(Script::Arabic.is_rtl());
        assert!(Script::Hebrew.is_rtl());
        assert!(Script::Syriac.is_rtl());
        assert!(Script::Thaana.is_rtl());
        assert!(!Script::Latin.is_rtl());
        assert!(!Script::Han.is_rtl());
        assert!(!Script::Common.is_rtl());
    }

    #[test]
    fn script_common_or_inherited() {
        assert!(Script::Common.is_common_or_inherited());
        assert!(Script::Inherited.is_common_or_inherited());
        assert!(!Script::Latin.is_common_or_inherited());
        assert!(!Script::Arabic.is_common_or_inherited());
    }

    // -----------------------------------------------------------------------
    // resolve_scripts tests
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_empty() {
        assert!(resolve_scripts(&[]).is_empty());
    }

    #[test]
    fn resolve_pure_latin() {
        let chars: Vec<char> = "Hello".chars().collect();
        let resolved = resolve_scripts(&chars);
        assert!(resolved.iter().all(|&s| s == Script::Latin));
    }

    #[test]
    fn resolve_common_absorbed_by_latin() {
        // "Hi 42!" — spaces, digits, and punctuation should resolve to Latin
        let chars: Vec<char> = "Hi 42!".chars().collect();
        let resolved = resolve_scripts(&chars);
        assert!(
            resolved.iter().all(|&s| s == Script::Latin),
            "All should be Latin: {resolved:?}"
        );
    }

    #[test]
    fn resolve_leading_space() {
        // " Hello" — leading space should resolve to Latin
        let chars: Vec<char> = " Hello".chars().collect();
        let resolved = resolve_scripts(&chars);
        assert_eq!(resolved[0], Script::Latin);
    }

    #[test]
    fn resolve_combining_mark_inherits() {
        // "é" as e + combining acute (U+0301)
        let chars: Vec<char> = "e\u{0301}".chars().collect();
        let resolved = resolve_scripts(&chars);
        assert_eq!(resolved[0], Script::Latin);
        assert_eq!(
            resolved[1],
            Script::Latin,
            "combining mark should inherit Latin"
        );
    }

    #[test]
    fn resolve_mixed_scripts() {
        // "Hello مرحبا" — Latin then Arabic with space between
        let text = "Hello \u{0645}\u{0631}\u{062D}\u{0628}\u{0627}";
        let chars: Vec<char> = text.chars().collect();
        let resolved = resolve_scripts(&chars);

        // H, e, l, l, o should be Latin
        for i in 0..5 {
            assert_eq!(resolved[i], Script::Latin, "char {i}");
        }
        // Space should be Latin (preceding script)
        assert_eq!(resolved[5], Script::Latin, "space");
        // Arabic chars
        for i in 6..11 {
            assert_eq!(resolved[i], Script::Arabic, "char {i}");
        }
    }

    #[test]
    fn resolve_all_common_defaults_to_latin() {
        let chars: Vec<char> = "123 !?".chars().collect();
        let resolved = resolve_scripts(&chars);
        assert!(
            resolved.iter().all(|&s| s == Script::Latin),
            "All-Common should default to Latin"
        );
    }

    // -----------------------------------------------------------------------
    // partition_by_script tests
    // -----------------------------------------------------------------------

    #[test]
    fn partition_empty() {
        assert!(partition_by_script("").is_empty());
    }

    #[test]
    fn partition_pure_latin() {
        let runs = partition_by_script("Hello World");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].script, Script::Latin);
        assert_eq!(runs[0].start, 0);
        assert_eq!(runs[0].end, 11);
        assert_eq!(runs[0].text("Hello World"), "Hello World");
    }

    #[test]
    fn partition_pure_arabic() {
        let text = "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}";
        let runs = partition_by_script(text);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].script, Script::Arabic);
    }

    #[test]
    fn partition_latin_then_arabic() {
        let text = "Hello \u{0645}\u{0631}\u{062D}\u{0628}\u{0627}";
        let runs = partition_by_script(text);
        assert!(runs.len() >= 2, "runs: {runs:?}");

        // First run should be Latin (including the space)
        assert_eq!(runs[0].script, Script::Latin);
        assert!(runs[0].text(text).starts_with("Hello"));

        // Last run should be Arabic
        let last = runs.last().unwrap();
        assert_eq!(last.script, Script::Arabic);
    }

    #[test]
    fn partition_latin_cjk_latin() {
        let text = "Hello\u{4E16}\u{754C}World";
        let runs = partition_by_script(text);
        assert_eq!(runs.len(), 3, "runs: {runs:?}");
        assert_eq!(runs[0].script, Script::Latin);
        assert_eq!(runs[1].script, Script::Han);
        assert_eq!(runs[2].script, Script::Latin);
    }

    #[test]
    fn partition_japanese_mixed() {
        // Hiragana + Kanji + Katakana
        let text = "\u{3053}\u{3093}\u{306B}\u{3061}\u{306F}\u{4E16}\u{754C}\u{30A2}";
        let runs = partition_by_script(text);
        assert!(runs.len() >= 2, "runs: {runs:?}");

        // Should have Hiragana, Han, Katakana runs
        let scripts: Vec<Script> = runs.iter().map(|r| r.script).collect();
        assert!(scripts.contains(&Script::Hiragana));
        assert!(scripts.contains(&Script::Han));
        assert!(scripts.contains(&Script::Katakana));
    }

    #[test]
    fn partition_runs_cover_full_text() {
        let text = "Hello \u{05E9}\u{05DC}\u{05D5}\u{05DD} World \u{4E16}\u{754C}";
        let runs = partition_by_script(text);

        // Runs should be contiguous and cover the full string.
        assert_eq!(runs[0].start, 0);
        assert_eq!(runs.last().unwrap().end, text.len());
        for window in runs.windows(2) {
            assert_eq!(
                window[0].end, window[1].start,
                "runs must be contiguous: {:?}",
                window
            );
        }
    }

    #[test]
    fn partition_run_text_slicing() {
        let text = "ABCdef";
        let runs = partition_by_script(text);
        let reconstructed: String = runs.iter().map(|r| r.text(text)).collect();
        assert_eq!(reconstructed, text);
    }

    #[test]
    fn partition_combining_mark_stays_with_base() {
        // "é" as e + combining acute should be a single Latin run
        let text = "e\u{0301}";
        let runs = partition_by_script(text);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].script, Script::Latin);
    }

    #[test]
    fn partition_digits_absorbed() {
        // "Item 42" should be a single Latin run
        let runs = partition_by_script("Item 42");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].script, Script::Latin);
    }

    // -----------------------------------------------------------------------
    // TextRun and partition_text_runs tests
    // -----------------------------------------------------------------------

    #[test]
    fn text_runs_empty() {
        assert!(partition_text_runs("", None, None).is_empty());
    }

    #[test]
    fn text_runs_simple_latin() {
        let runs = partition_text_runs("Hello World", None, None);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].script, Script::Latin);
        assert_eq!(runs[0].direction, RunDirection::Ltr);
        assert_eq!(runs[0].style_id, 0);
    }

    #[test]
    fn text_runs_arabic_direction() {
        let text = "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}";
        let runs = partition_text_runs(text, None, None);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].script, Script::Arabic);
        assert_eq!(runs[0].direction, RunDirection::Rtl);
    }

    #[test]
    fn text_runs_mixed_scripts() {
        let text = "Hello\u{4E16}\u{754C}World";
        let runs = partition_text_runs(text, None, None);
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].direction, RunDirection::Ltr);
        assert_eq!(runs[1].direction, RunDirection::Ltr);
        assert_eq!(runs[2].direction, RunDirection::Ltr);
    }

    #[test]
    fn text_runs_style_split() {
        let text = "Hello World";
        // Style changes at byte offset 5 (the space)
        let style_fn = |offset: usize| -> u64 { if offset < 5 { 1 } else { 2 } };
        let runs = partition_text_runs(text, None, Some(&style_fn));
        assert_eq!(runs.len(), 2, "runs: {runs:?}");
        assert_eq!(runs[0].style_id, 1);
        assert_eq!(runs[0].text(text), "Hello");
        assert_eq!(runs[1].style_id, 2);
        assert_eq!(runs[1].text(text), " World");
    }

    #[test]
    fn text_runs_direction_override() {
        let text = "ABC";
        // Force RTL direction
        let dir_fn = |_offset: usize| -> RunDirection { RunDirection::Rtl };
        let runs = partition_text_runs(text, Some(&dir_fn), None);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].direction, RunDirection::Rtl);
    }

    #[test]
    fn text_runs_cover_full_text() {
        let text = "Hello \u{05E9}\u{05DC}\u{05D5}\u{05DD} World";
        let runs = partition_text_runs(text, None, None);

        assert_eq!(runs[0].start, 0);
        assert_eq!(runs.last().unwrap().end, text.len());
        for window in runs.windows(2) {
            assert_eq!(window[0].end, window[1].start);
        }

        let reconstructed: String = runs.iter().map(|r| r.text(text)).collect();
        assert_eq!(reconstructed, text);
    }

    // -----------------------------------------------------------------------
    // RunCacheKey tests
    // -----------------------------------------------------------------------

    #[test]
    fn cache_key_equality() {
        let text = "Hello";
        let run = TextRun {
            start: 0,
            end: 5,
            script: Script::Latin,
            direction: RunDirection::Ltr,
            style_id: 0,
        };

        let k1 = run.cache_key(text);
        let k2 = run.cache_key(text);
        assert_eq!(k1, k2);
    }

    #[test]
    fn cache_key_differs_by_script() {
        let k1 = RunCacheKey {
            text: "abc",
            script: Script::Latin,
            direction: RunDirection::Ltr,
            style_id: 0,
        };
        let k2 = RunCacheKey {
            text: "abc",
            script: Script::Greek,
            direction: RunDirection::Ltr,
            style_id: 0,
        };
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_differs_by_direction() {
        let k1 = RunCacheKey {
            text: "abc",
            script: Script::Latin,
            direction: RunDirection::Ltr,
            style_id: 0,
        };
        let k2 = RunCacheKey {
            text: "abc",
            script: Script::Latin,
            direction: RunDirection::Rtl,
            style_id: 0,
        };
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_differs_by_style() {
        let k1 = RunCacheKey {
            text: "abc",
            script: Script::Latin,
            direction: RunDirection::Ltr,
            style_id: 0,
        };
        let k2 = RunCacheKey {
            text: "abc",
            script: Script::Latin,
            direction: RunDirection::Ltr,
            style_id: 1,
        };
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_hashable() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        let k = RunCacheKey {
            text: "hello",
            script: Script::Latin,
            direction: RunDirection::Ltr,
            style_id: 0,
        };
        set.insert(k.clone());
        assert!(set.contains(&k));
    }

    // -----------------------------------------------------------------------
    // Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn single_char() {
        let runs = partition_by_script("A");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].script, Script::Latin);
        assert_eq!(runs[0].start, 0);
        assert_eq!(runs[0].end, 1);
    }

    #[test]
    fn only_spaces() {
        let runs = partition_by_script("   ");
        assert_eq!(runs.len(), 1);
        // All-Common defaults to Latin
        assert_eq!(runs[0].script, Script::Latin);
    }

    #[test]
    fn emoji_is_common() {
        // Emoji should be Common, absorbed into surrounding script
        let text = "Hello \u{1F600} World";
        let runs = partition_by_script(text);
        // Should be a single Latin run (emoji is Common, absorbed)
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].script, Script::Latin);
    }

    #[test]
    fn multibyte_utf8_offsets() {
        // Ensure byte offsets are correct for multi-byte chars
        // é (2 bytes) + 一 (3 bytes)
        let text = "\u{00E9}\u{4E00}";
        let runs = partition_by_script(text);
        assert!(runs.len() >= 2);
        assert_eq!(runs[0].end, 2); // é is 2 bytes
        assert_eq!(runs[1].start, 2);
        assert_eq!(runs[1].end, 5); // 一 is 3 bytes
    }

    #[test]
    fn text_run_len_and_empty() {
        let run = TextRun {
            start: 5,
            end: 10,
            script: Script::Latin,
            direction: RunDirection::Ltr,
            style_id: 0,
        };
        assert_eq!(run.len(), 5);
        assert!(!run.is_empty());

        let empty = TextRun {
            start: 5,
            end: 5,
            script: Script::Latin,
            direction: RunDirection::Ltr,
            style_id: 0,
        };
        assert_eq!(empty.len(), 0);
        assert!(empty.is_empty());
    }

    #[test]
    fn script_run_len_and_empty() {
        let run = ScriptRun {
            start: 0,
            end: 5,
            script: Script::Latin,
        };
        assert_eq!(run.len(), 5);
        assert!(!run.is_empty());
    }

    #[test]
    fn script_enum_ord() {
        // Script has PartialOrd/Ord derived — verify it's usable for sorting
        let mut scripts = vec![Script::Arabic, Script::Latin, Script::Common];
        scripts.sort();
        assert_eq!(scripts[0], Script::Common);
    }

    #[test]
    fn many_script_transitions() {
        // Latin + Greek + Cyrillic + Hebrew + Arabic
        let text = "Hello\u{0391}\u{0392}\u{0410}\u{0411}\u{05D0}\u{05D1}\u{0627}\u{0628}";
        let runs = partition_by_script(text);

        let scripts: Vec<Script> = runs.iter().map(|r| r.script).collect();
        assert!(scripts.contains(&Script::Latin));
        assert!(scripts.contains(&Script::Greek));
        assert!(scripts.contains(&Script::Cyrillic));
        assert!(scripts.contains(&Script::Hebrew));
        assert!(scripts.contains(&Script::Arabic));

        // Verify contiguity
        for window in runs.windows(2) {
            assert_eq!(window[0].end, window[1].start);
        }
    }
}
