#![forbid(unsafe_code)]

//! ASCII diagram detection and border alignment correction.
//!
//! This module provides automatic detection and correction of ASCII art diagrams,
//! fixing misaligned right borders by adding appropriate padding.
//!
//! # Features
//!
//! - Automatic detection of ASCII/Unicode box-drawing diagrams
//! - Border alignment correction (adds padding, never removes content)
//! - Support for both ASCII (`+`, `-`, `|`) and Unicode box-drawing characters
//! - CJK and emoji width handling for correct alignment
//! - Confidence scoring for diagram detection
//!
//! # Example
//!
//! ```
//! use ftui_extras::diagram::{correct_diagram, is_likely_diagram};
//!
//! let input = "\
//! +--------+
//! | Short|
//! | Longer text |
//! +--------+";
//!
//! if is_likely_diagram(input) {
//!     let corrected = correct_diagram(input);
//!     // Borders are now aligned
//! }
//! ```
//!
//! # Algorithm
//!
//! The correction algorithm:
//! 1. Detects diagram blocks (consecutive lines with box-drawing characters)
//! 2. Finds the maximum right border column
//! 3. Pads shorter lines to align with the longest
//! 4. Iterates until alignment converges

use unicode_display_width::width as unicode_display_width;
use unicode_segmentation::UnicodeSegmentation;

// ---------------------------------------------------------------------------
// Box-Drawing Character Detection
// ---------------------------------------------------------------------------

/// Check if character is a corner piece (ASCII or Unicode).
#[inline]
#[must_use]
pub fn is_corner(c: char) -> bool {
    matches!(
        c,
        '+' | '┌' | '┐' | '└' | '┘' | '╔' | '╗' | '╚' | '╝' | '╭' | '╮' | '╯' | '╰'
    )
}

/// Check if character is a horizontal fill (for borders).
#[inline]
#[must_use]
pub fn is_horizontal_fill(c: char) -> bool {
    matches!(
        c,
        '-' | '─' | '━' | '═' | '╌' | '╍' | '┄' | '┅' | '┈' | '┉' | '~' | '='
    )
}

/// Check if character is a vertical border.
#[inline]
#[must_use]
pub fn is_vertical_border(c: char) -> bool {
    matches!(c, '|' | '│' | '┃' | '║' | '╎' | '╏' | '┆' | '┇' | '┊' | '┋')
}

/// Check if character is a T-junction.
#[inline]
#[must_use]
pub fn is_junction(c: char) -> bool {
    matches!(
        c,
        '┬' | '┴'
            | '├'
            | '┤'
            | '┼'
            | '╦'
            | '╩'
            | '╠'
            | '╣'
            | '╬'
            | '╤'
            | '╧'
            | '╟'
            | '╢'
            | '╫'
            | '╪'
    )
}

/// Check if character could be part of a box drawing.
#[inline]
#[must_use]
pub fn is_box_char(c: char) -> bool {
    is_corner(c) || is_horizontal_fill(c) || is_vertical_border(c) || is_junction(c)
}

/// Check if character can terminate a line border.
#[inline]
#[must_use]
pub fn is_border_char(c: char) -> bool {
    is_vertical_border(c) || is_corner(c) || is_junction(c)
}

// ---------------------------------------------------------------------------
// Character Width Calculation
// ---------------------------------------------------------------------------

#[inline]
fn ascii_display_width(text: &str) -> usize {
    let mut width = 0;
    for b in text.bytes() {
        match b {
            b'\t' | b'\n' | b'\r' => width += 1,
            0x20..=0x7E => width += 1,
            _ => {}
        }
    }
    width
}

/// Calculate the visual width of a single character in terminal columns.
///
/// Uses Unicode display width via the `unicode-display-width` crate.
/// Zero-width characters (combining marks, etc.) return 0.
#[inline]
#[must_use]
pub fn char_width(c: char) -> usize {
    if matches!(c, '\t' | '\n' | '\r') {
        return 1;
    }
    if is_zero_width_codepoint(c) {
        return 0;
    }
    let mut buf = [0u8; 4];
    usize::try_from(unicode_display_width(c.encode_utf8(&mut buf)))
        .expect("unicode display width should fit in usize")
}

/// Calculate the visual width of a single grapheme cluster.
///
/// Grapheme clusters can contain multiple code points (emoji sequences,
/// combining marks, etc.). We treat the cluster as a single terminal glyph
/// using Unicode display width rules.
#[inline]
#[must_use]
pub fn grapheme_width(grapheme: &str) -> usize {
    if grapheme.is_ascii() {
        return ascii_display_width(grapheme);
    }
    if grapheme.chars().all(is_zero_width_codepoint) {
        return 0;
    }
    usize::try_from(unicode_display_width(grapheme))
        .expect("unicode display width should fit in usize")
}

/// Calculate the visual width of a string in terminal columns.
///
/// Handles different character widths using grapheme cluster boundaries:
/// - ASCII characters: 1 column each
/// - CJK characters (Chinese, Japanese, Korean): 2 columns each
/// - Emoji ZWJ/flag sequences: 2 columns (cluster width, not sum of code points)
#[must_use]
pub fn visual_width(s: &str) -> usize {
    if s.is_ascii() && s.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
        return s.len();
    }
    if s.is_ascii() {
        return ascii_display_width(s);
    }

    if !s.chars().any(is_zero_width_codepoint) {
        return usize::try_from(unicode_display_width(s))
            .expect("unicode display width should fit in usize");
    }
    s.graphemes(true).map(grapheme_width).sum()
}

#[inline]
fn is_zero_width_codepoint(c: char) -> bool {
    let u = c as u32;
    matches!(u, 0x0000..=0x001F | 0x007F..=0x009F)
        || matches!(u, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF)
        || matches!(u, 0xFE20..=0xFE2F)
        || matches!(u, 0xFE00..=0xFE0F | 0xE0100..=0xE01EF)
        || matches!(
            u,
            0x00AD | 0x034F | 0x180E | 0x200B | 0x200C | 0x200D | 0x200E | 0x200F | 0x2060 | 0xFEFF
        )
        || matches!(u, 0x202A..=0x202E | 0x2066..=0x2069 | 0x206A..=0x206F)
}

// ---------------------------------------------------------------------------
// Line Classification
// ---------------------------------------------------------------------------

/// Classification of a line's role in a diagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// Empty or whitespace-only line.
    Blank,
    /// A line with no detected diagram structure.
    None,
    /// A line with vertical borders but no horizontal structure (content row).
    Weak,
    /// A line with strong horizontal structure (top/bottom border).
    Strong,
}

impl LineKind {
    /// Returns true if this line appears to be part of a diagram.
    #[must_use]
    pub fn is_boxy(self) -> bool {
        matches!(self, Self::Weak | Self::Strong)
    }
}

/// Classify a single line based on its box-drawing character content.
#[must_use]
pub fn classify_line(line: &str) -> LineKind {
    let trimmed = line.trim();

    if trimmed.is_empty() {
        return LineKind::Blank;
    }

    let box_chars: usize = trimmed.chars().filter(|&c| is_box_char(c)).count();

    if box_chars == 0 {
        return LineKind::None;
    }

    let has_corner = trimmed.chars().any(is_corner);
    let has_horizontal = trimmed.chars().any(is_horizontal_fill);

    // Strong: has corners or horizontal fill (top/bottom border lines)
    // High box char ratio alone isn't enough - need actual horizontal structure
    if has_corner || has_horizontal {
        LineKind::Strong
    } else {
        // Has box chars but only vertical borders → content row
        LineKind::Weak
    }
}

// ---------------------------------------------------------------------------
// Diagram Detection
// ---------------------------------------------------------------------------

/// A detected ASCII diagram block within text.
#[derive(Debug, Clone)]
pub struct DiagramBlock {
    /// Starting line index (0-based, inclusive).
    pub start: usize,
    /// Ending line index (exclusive).
    pub end: usize,
    /// Confidence that this is an actual diagram (0.0-1.0).
    pub confidence: f64,
}

/// Check if text is likely an ASCII diagram.
///
/// Returns true if at least 2 lines have box-drawing characters.
#[must_use]
pub fn is_likely_diagram(text: &str) -> bool {
    let boxy_lines = text.lines().filter(|l| classify_line(l).is_boxy()).count();
    boxy_lines >= 2
}

/// Find diagram blocks in the input text.
///
/// Returns blocks of consecutive lines containing box-drawing characters.
#[must_use]
pub fn find_diagram_blocks(lines: &[&str]) -> Vec<DiagramBlock> {
    let mut blocks = Vec::new();
    let mut block_start: Option<usize> = None;
    let mut strong_count = 0usize;
    let mut weak_count = 0usize;

    for (i, line) in lines.iter().enumerate() {
        let kind = classify_line(line);

        match kind {
            LineKind::Strong => {
                if block_start.is_none() {
                    block_start = Some(i);
                }
                strong_count = strong_count.saturating_add(1);
            }
            LineKind::Weak => {
                if block_start.is_none() {
                    block_start = Some(i);
                }
                weak_count = weak_count.saturating_add(1);
            }
            LineKind::Blank => {
                // Allow single blank lines within blocks
                if block_start.is_some() {
                    // Check if there's more boxy content ahead
                    let next_boxy = lines
                        .iter()
                        .skip(i + 1)
                        .take(2)
                        .any(|l| classify_line(l).is_boxy());
                    if !next_boxy {
                        // End the block
                        if let Some(start) = block_start.take() {
                            let total = strong_count + weak_count;
                            let confidence = if total > 0 {
                                let strong_ratio = strong_count as f64 / total as f64;
                                (strong_ratio * 0.8 + (total as f64 / 20.0).min(0.2)).min(1.0)
                            } else {
                                0.0
                            };
                            blocks.push(DiagramBlock {
                                start,
                                end: i,
                                confidence,
                            });
                        }
                        strong_count = 0;
                        weak_count = 0;
                    }
                }
            }
            LineKind::None => {
                // End any active block
                if let Some(start) = block_start.take() {
                    let total = strong_count + weak_count;
                    let confidence = if total > 0 {
                        let strong_ratio = strong_count as f64 / total as f64;
                        (strong_ratio * 0.8 + (total as f64 / 20.0).min(0.2)).min(1.0)
                    } else {
                        0.0
                    };
                    blocks.push(DiagramBlock {
                        start,
                        end: i,
                        confidence,
                    });
                }
                strong_count = 0;
                weak_count = 0;
            }
        }
    }

    // Close any remaining block
    if let Some(start) = block_start {
        let total = strong_count + weak_count;
        let confidence = if total > 0 {
            let strong_ratio = strong_count as f64 / total as f64;
            (strong_ratio * 0.8 + (total as f64 / 20.0).min(0.2)).min(1.0)
        } else {
            0.0
        };
        blocks.push(DiagramBlock {
            start,
            end: lines.len(),
            confidence,
        });
    }

    blocks
}

// ---------------------------------------------------------------------------
// Border Detection and Correction
// ---------------------------------------------------------------------------

/// Detect the right-side border position in a line.
fn detect_suffix_border(line: &str) -> Option<(usize, char)> {
    let trimmed = line.trim_end();
    if trimmed.is_empty() {
        return None;
    }

    let last_char = trimmed.chars().next_back()?;

    if is_border_char(last_char) {
        let prefix = &trimmed[..trimmed.len() - last_char.len_utf8()];
        let column = visual_width(prefix);
        Some((column, last_char))
    } else {
        None
    }
}

/// Detect the most common vertical border character in lines.
fn detect_vertical_border(lines: &[&str]) -> char {
    let mut counts = std::collections::HashMap::new();

    for line in lines {
        for c in line.chars() {
            if is_vertical_border(c) {
                *counts.entry(c).or_insert(0) += 1;
            }
        }
    }

    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(c, _)| c)
        .unwrap_or('|')
}

/// Correct border alignment in a single diagram block.
fn correct_block(lines: &mut [String], max_iterations: usize) {
    for _ in 0..max_iterations {
        let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();

        // Find maximum border column
        let mut max_column = 0usize;
        for line in &line_refs {
            if let Some((col, _)) = detect_suffix_border(line) {
                max_column = max_column.max(col);
            }
        }

        if max_column == 0 {
            break;
        }

        // Determine the border character to use
        let border_char = detect_vertical_border(&line_refs);

        let mut any_changes = false;

        for line in lines.iter_mut() {
            let kind = classify_line(line);
            if !kind.is_boxy() {
                continue;
            }

            if let Some((col, existing_char)) = detect_suffix_border(line) {
                if col < max_column {
                    // Need to pad before the border
                    let trimmed = line.trim_end();
                    let prefix = &trimmed[..trimmed.len() - existing_char.len_utf8()];
                    let padding = max_column - col;
                    *line = format!("{}{:padding$}{}", prefix, "", existing_char);
                    any_changes = true;
                }
            } else {
                // No border detected, but line is boxy - add one
                let trimmed = line.trim_end();
                let current_width = visual_width(trimmed);
                if current_width < max_column {
                    let padding = max_column - current_width;
                    *line = format!("{}{:padding$}{}", trimmed, "", border_char);
                    any_changes = true;
                }
            }
        }

        if !any_changes {
            break;
        }
    }
}

/// Correct border alignment in ASCII diagram text.
///
/// Detects diagram blocks and aligns their right borders by adding padding.
/// This operation is safe: it only adds spaces, never removes content.
///
/// # Arguments
///
/// * `text` - The input text potentially containing ASCII diagrams
///
/// # Returns
///
/// The corrected text with aligned borders.
///
/// # Example
///
/// ```
/// use ftui_extras::diagram::correct_diagram;
///
/// let input = "\
/// +------+
/// | Hi|
/// | Hello |
/// +------+";
///
/// let output = correct_diagram(input);
/// // Output has aligned right borders
/// ```
#[must_use]
pub fn correct_diagram(text: &str) -> String {
    correct_diagram_with_options(text, 10, 0.3)
}

/// Correct border alignment with custom options.
///
/// # Arguments
///
/// * `text` - The input text
/// * `max_iterations` - Maximum correction passes per block
/// * `min_confidence` - Minimum confidence to correct a block (0.0-1.0)
#[must_use]
pub fn correct_diagram_with_options(
    text: &str,
    max_iterations: usize,
    min_confidence: f64,
) -> String {
    let line_vec: Vec<&str> = text.lines().collect();

    // Quick check: if very few box characters, skip processing
    let box_char_count = text.chars().filter(|&c| is_box_char(c)).count();
    if box_char_count < 4 {
        return text.to_string();
    }

    let blocks = find_diagram_blocks(&line_vec);

    if blocks.is_empty() {
        return text.to_string();
    }

    let mut lines: Vec<String> = line_vec.iter().map(|s| (*s).to_string()).collect();

    for block in blocks {
        if block.confidence >= min_confidence {
            correct_block(&mut lines[block.start..block.end], max_iterations);
        }
    }

    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_corner() {
        assert!(is_corner('+'));
        assert!(is_corner('┌'));
        assert!(is_corner('╭'));
        assert!(!is_corner('-'));
        assert!(!is_corner('|'));
    }

    #[test]
    fn test_is_box_char() {
        assert!(is_box_char('+'));
        assert!(is_box_char('-'));
        assert!(is_box_char('|'));
        assert!(is_box_char('─'));
        assert!(is_box_char('│'));
        assert!(!is_box_char('a'));
        assert!(!is_box_char(' '));
    }

    #[test]
    fn test_visual_width() {
        assert_eq!(visual_width("Hello"), 5);
        assert_eq!(visual_width("你好"), 4); // CJK = 2 each
        assert_eq!(visual_width("Hi世界"), 6); // 2 ASCII + 2 CJK
        assert_eq!(visual_width("┌──┐"), 4); // Box drawing = 1 each
        assert_eq!(visual_width("👍🏻"), 2); // Emoji + skin tone modifier = 2 cells
        assert_eq!(visual_width("🇺🇸"), 2); // Flag emoji = 2 cells
        assert_eq!(visual_width("👨‍👩‍👧"), 2); // ZWJ sequence = 2 cells
    }

    #[test]
    fn test_ascii_display_width_controls() {
        assert_eq!(ascii_display_width("a\tb\nc\rd"), 7);
    }

    #[test]
    fn test_char_width_zero_width_codepoints() {
        assert_eq!(char_width('\n'), 1);
        assert_eq!(char_width('\u{0301}'), 0); // combining acute accent
        assert_eq!(char_width('好'), 2);
    }

    #[test]
    fn test_grapheme_width_zero_width_cluster() {
        assert_eq!(grapheme_width("\u{0301}"), 0);
        assert_eq!(grapheme_width("a"), 1);
    }

    #[test]
    fn test_visual_width_ascii_controls() {
        assert_eq!(visual_width("a\tb"), 3);
        assert_eq!(visual_width("a\nb"), 3);
    }

    #[test]
    fn test_classify_line() {
        assert_eq!(classify_line(""), LineKind::Blank);
        assert_eq!(classify_line("   "), LineKind::Blank);
        assert_eq!(classify_line("hello"), LineKind::None);
        assert_eq!(classify_line("+----+"), LineKind::Strong);
        assert_eq!(classify_line("| hi |"), LineKind::Weak);
        assert_eq!(classify_line("┌────┐"), LineKind::Strong);
    }

    #[test]
    fn test_is_likely_diagram() {
        assert!(is_likely_diagram("+--+\n|  |\n+--+"));
        assert!(!is_likely_diagram("hello\nworld"));
        assert!(!is_likely_diagram("+--+")); // Only 1 boxy line
    }

    #[test]
    fn test_correct_simple_diagram() {
        let input = "+------+\n| Hi|\n| Hello |\n+------+";
        let output = correct_diagram(input);

        // All lines should have same right border position
        let lines: Vec<&str> = output.lines().collect();
        let positions: Vec<Option<usize>> = lines
            .iter()
            .map(|l| detect_suffix_border(l).map(|(col, _)| col))
            .collect();

        // Filter to lines with borders
        let border_positions: Vec<usize> = positions.into_iter().flatten().collect();
        assert!(!border_positions.is_empty());

        // All border positions should be equal
        let first = border_positions[0];
        assert!(border_positions.iter().all(|&p| p == first));
    }

    #[test]
    fn test_detect_suffix_border() {
        let line = "| hi |  ";
        assert_eq!(detect_suffix_border(line), Some((5, '|')));
        assert_eq!(detect_suffix_border("no border"), None);
    }

    #[test]
    fn test_detect_vertical_border_prefers_most_common() {
        let lines = vec!["| a |", "| b |", "│ c │"];
        assert_eq!(detect_vertical_border(&lines), '|');
    }

    #[test]
    fn test_correct_diagram_adds_missing_right_border() {
        let input = "+----+\n| Hi\n| Hello |\n+----+";
        let output = correct_diagram(input);

        let lines: Vec<&str> = output.lines().collect();
        let borders: Vec<usize> = lines
            .iter()
            .filter_map(|l| detect_suffix_border(l).map(|(col, _)| col))
            .collect();
        assert!(!borders.is_empty());
        let first = borders[0];
        assert!(borders.iter().all(|&p| p == first));
        assert!(lines[1].trim_end().ends_with('|'));
    }

    #[test]
    fn test_correct_diagram_skips_low_confidence() {
        let input = "+--+\n|x|\n+--+";
        let output = correct_diagram_with_options(input, 5, 1.0);
        assert_eq!(output, input);
    }

    #[test]
    fn test_correct_diagram_skips_few_box_chars() {
        let input = "+-+";
        let output = correct_diagram_with_options(input, 5, 0.0);
        assert_eq!(output, input);
    }

    #[test]
    fn test_correct_unicode_diagram() {
        let input = "┌────┐\n│Hi│\n│Hello│\n└────┘";
        let output = correct_diagram(input);

        // Should not panic and should produce output
        assert!(!output.is_empty());
    }

    #[test]
    fn test_no_change_needed() {
        let input = "+------+\n| Hi   |\n| Hello|\n+------+";
        let output = correct_diagram(input);

        // Output should be similar (might add trailing spaces)
        assert!(output.contains("Hi"));
        assert!(output.contains("Hello"));
    }

    #[test]
    fn test_mixed_content() {
        let input = "Some text\n\n+--+\n|Hi|\n+--+\n\nMore text";
        let output = correct_diagram(input);

        // Should preserve non-diagram content
        assert!(output.contains("Some text"));
        assert!(output.contains("More text"));
    }

    #[test]
    fn test_find_blocks() {
        let lines = vec![
            "text",
            "+--+",
            "|  |",
            "+--+",
            "more text",
            "┌──┐",
            "│  │",
            "└──┘",
        ];
        let blocks = find_diagram_blocks(&lines);

        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].start, 1);
        assert_eq!(blocks[0].end, 4);
        assert_eq!(blocks[1].start, 5);
        assert_eq!(blocks[1].end, 8);
    }

    // --- Direct character classification tests ---

    #[test]
    fn test_is_horizontal_fill() {
        assert!(is_horizontal_fill('-'));
        assert!(is_horizontal_fill('─'));
        assert!(is_horizontal_fill('━'));
        assert!(is_horizontal_fill('═'));
        assert!(is_horizontal_fill('~'));
        assert!(is_horizontal_fill('='));
        assert!(is_horizontal_fill('╌'));
        assert!(is_horizontal_fill('╍'));
        assert!(is_horizontal_fill('┄'));
        assert!(is_horizontal_fill('┅'));
        assert!(is_horizontal_fill('┈'));
        assert!(is_horizontal_fill('┉'));
        assert!(!is_horizontal_fill('|'));
        assert!(!is_horizontal_fill('+'));
        assert!(!is_horizontal_fill('a'));
    }

    #[test]
    fn test_is_vertical_border() {
        assert!(is_vertical_border('|'));
        assert!(is_vertical_border('│'));
        assert!(is_vertical_border('┃'));
        assert!(is_vertical_border('║'));
        assert!(is_vertical_border('╎'));
        assert!(is_vertical_border('╏'));
        assert!(is_vertical_border('┆'));
        assert!(is_vertical_border('┇'));
        assert!(is_vertical_border('┊'));
        assert!(is_vertical_border('┋'));
        assert!(!is_vertical_border('-'));
        assert!(!is_vertical_border('+'));
        assert!(!is_vertical_border('a'));
    }

    #[test]
    fn test_is_junction() {
        assert!(is_junction('┬'));
        assert!(is_junction('┴'));
        assert!(is_junction('├'));
        assert!(is_junction('┤'));
        assert!(is_junction('┼'));
        assert!(is_junction('╦'));
        assert!(is_junction('╩'));
        assert!(is_junction('╠'));
        assert!(is_junction('╣'));
        assert!(is_junction('╬'));
        assert!(is_junction('╤'));
        assert!(is_junction('╧'));
        assert!(is_junction('╟'));
        assert!(is_junction('╢'));
        assert!(is_junction('╫'));
        assert!(is_junction('╪'));
        assert!(!is_junction('+'));
        assert!(!is_junction('─'));
        assert!(!is_junction('a'));
    }

    #[test]
    fn test_is_border_char() {
        // is_border_char = is_vertical_border || is_corner || is_junction
        assert!(is_border_char('|'));
        assert!(is_border_char('+'));
        assert!(is_border_char('┼'));
        assert!(is_border_char('│'));
        assert!(is_border_char('┌'));
        assert!(!is_border_char('-'));
        assert!(!is_border_char('─'));
        assert!(!is_border_char('a'));
    }

    // --- find_diagram_blocks edge cases ---

    #[test]
    fn test_find_blocks_empty_input() {
        let blocks = find_diagram_blocks(&[]);
        assert!(blocks.is_empty());
    }

    #[test]
    fn test_find_blocks_no_diagrams() {
        let lines = vec!["hello", "world", "no diagrams"];
        let blocks = find_diagram_blocks(&lines);
        assert!(blocks.is_empty());
    }

    #[test]
    fn test_find_blocks_block_at_end() {
        let lines = vec!["text", "+--+", "|  |", "+--+"];
        let blocks = find_diagram_blocks(&lines);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].start, 1);
        assert_eq!(blocks[0].end, 4);
    }

    #[test]
    fn test_find_blocks_blank_line_continuation() {
        // Blank line in middle with boxy content within 2 lines ahead
        let lines = vec!["+--+", "|  |", "", "|  |", "+--+"];
        let blocks = find_diagram_blocks(&lines);
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn test_find_blocks_blank_line_terminates() {
        // Blank line followed by non-boxy content ends the block
        let lines = vec!["+--+", "|  |", "", "text", "more text"];
        let blocks = find_diagram_blocks(&lines);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].start, 0);
        assert_eq!(blocks[0].end, 2);
    }

    #[test]
    fn test_find_blocks_confidence_all_strong() {
        let lines = vec!["+--+", "+--+", "+--+"];
        let blocks = find_diagram_blocks(&lines);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].confidence > 0.8);
    }

    #[test]
    fn test_find_blocks_confidence_all_weak() {
        let lines = vec!["| a |", "| b |", "| c |"];
        let blocks = find_diagram_blocks(&lines);
        assert_eq!(blocks.len(), 1);
        // All weak → strong_ratio = 0 → confidence = 0 + min(3/20, 0.2) = 0.15
        assert!(blocks[0].confidence < 0.3);
    }

    #[test]
    fn test_find_blocks_starts_at_first_line() {
        let lines = vec!["+--+", "|  |", "+--+"];
        let blocks = find_diagram_blocks(&lines);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].start, 0);
        assert_eq!(blocks[0].end, 3);
    }

    // --- correct_diagram edge cases ---

    #[test]
    fn test_correct_diagram_empty() {
        let output = correct_diagram("");
        assert_eq!(output, "");
    }

    #[test]
    fn test_correct_diagram_no_diagrams() {
        let input = "just plain text\nno diagrams here";
        let output = correct_diagram(input);
        assert_eq!(output, input);
    }

    #[test]
    fn test_correct_diagram_idempotent() {
        let input = "+------+\n| Hi|\n| Hello |\n+------+";
        let first = correct_diagram(input);
        let second = correct_diagram(&first);
        assert_eq!(first, second, "correction should be idempotent");
    }

    #[test]
    fn test_correct_diagram_with_cjk() {
        let input = "+--------+\n|你好|\n|Hello   |\n+--------+";
        let output = correct_diagram(input);
        assert!(output.contains("你好"));
        assert!(output.contains("Hello"));
    }

    #[test]
    fn test_correct_diagram_with_options_zero_iterations() {
        let input = "+------+\n| Hi|\n| Hello |\n+------+";
        let output = correct_diagram_with_options(input, 0, 0.0);
        assert_eq!(output, input);
    }

    // --- detect_vertical_border edge cases ---

    #[test]
    fn test_detect_vertical_border_empty() {
        let lines: Vec<&str> = vec![];
        assert_eq!(detect_vertical_border(&lines), '|');
    }

    #[test]
    fn test_detect_vertical_border_no_vertical_chars() {
        let lines = vec!["hello", "world"];
        assert_eq!(detect_vertical_border(&lines), '|');
    }

    #[test]
    fn test_detect_vertical_border_unicode_majority() {
        let lines = vec!["│ a │", "│ b │", "│ c │", "| d |"];
        assert_eq!(detect_vertical_border(&lines), '│');
    }

    // --- LineKind ---

    #[test]
    fn test_line_kind_is_boxy() {
        assert!(!LineKind::Blank.is_boxy());
        assert!(!LineKind::None.is_boxy());
        assert!(LineKind::Weak.is_boxy());
        assert!(LineKind::Strong.is_boxy());
    }

    // --- classify_line edge cases ---

    #[test]
    fn test_classify_line_junction_only() {
        // Junction without horizontal or corner → Weak
        assert_eq!(classify_line("┼"), LineKind::Weak);
    }

    #[test]
    fn test_classify_line_double_border() {
        assert_eq!(classify_line("╔════╗"), LineKind::Strong);
    }

    #[test]
    fn test_classify_line_rounded_corners() {
        assert_eq!(classify_line("╭──╮"), LineKind::Strong);
        assert_eq!(classify_line("╰──╯"), LineKind::Strong);
    }

    // --- detect_suffix_border edge cases ---

    #[test]
    fn test_detect_suffix_border_empty() {
        assert_eq!(detect_suffix_border(""), None);
        assert_eq!(detect_suffix_border("   "), None);
    }

    #[test]
    fn test_detect_suffix_border_unicode() {
        assert_eq!(detect_suffix_border("│ hi │"), Some((5, '│')));
    }

    #[test]
    fn test_detect_suffix_border_corner() {
        // Corners are border chars
        assert_eq!(detect_suffix_border("+--+"), Some((3, '+')));
    }

    #[test]
    fn test_detect_suffix_border_junction() {
        assert_eq!(detect_suffix_border("──┤"), Some((2, '┤')));
    }

    #[test]
    fn test_detect_suffix_border_trailing_spaces() {
        // Trailing spaces are trimmed, so border is still found
        assert_eq!(detect_suffix_border("| hi |   "), Some((5, '|')));
    }

    // --- is_zero_width_codepoint ---

    #[test]
    fn test_zero_width_chars() {
        // Combining marks
        assert_eq!(char_width('\u{0300}'), 0); // combining grave accent
        assert_eq!(char_width('\u{0301}'), 0); // combining acute accent
        assert_eq!(char_width('\u{036F}'), 0); // combining latin small letter x
        // Variation selectors
        assert_eq!(char_width('\u{FE00}'), 0);
        assert_eq!(char_width('\u{FE0F}'), 0);
        // Zero-width space/joiner
        assert_eq!(char_width('\u{200B}'), 0); // zero-width space
        assert_eq!(char_width('\u{200D}'), 0); // zero-width joiner
        assert_eq!(char_width('\u{FEFF}'), 0); // BOM / zero-width no-break space
    }

    // --- visual_width edge cases ---

    #[test]
    fn test_visual_width_empty() {
        assert_eq!(visual_width(""), 0);
    }

    #[test]
    fn test_visual_width_pure_ascii_printable() {
        assert_eq!(visual_width("abc"), 3);
        assert_eq!(visual_width("Hello, World!"), 13);
    }

    #[test]
    fn test_visual_width_box_drawing_chars() {
        assert_eq!(visual_width("┌─┐"), 3);
        assert_eq!(visual_width("╔═══╗"), 5);
    }

    #[test]
    fn test_visual_width_with_combining() {
        // e + combining acute = visually 1 character
        assert_eq!(visual_width("e\u{0301}"), 1);
    }

    // --- Multiple block correction ---

    #[test]
    fn test_correct_diagram_multiple_blocks() {
        let input = "+----+\n| a|\n+----+\ntext\n+------+\n| b   |\n+------+";
        let output = correct_diagram(input);
        assert!(output.contains("a"));
        assert!(output.contains("b"));
        assert!(output.contains("text"));
    }
}
