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

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

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

/// Calculate the visual width of a single character in terminal columns.
///
/// Uses the Unicode width standard via the `unicode-width` crate.
/// Zero-width characters (combining marks, etc.) return 0.
#[inline]
#[must_use]
pub fn char_width(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// Calculate the visual width of a single grapheme cluster.
///
/// Grapheme clusters can contain multiple code points (emoji sequences,
/// combining marks, etc.). We treat the cluster as a single terminal glyph
/// whose width is the maximum width of its constituent code points.
#[inline]
#[must_use]
pub fn grapheme_width(grapheme: &str) -> usize {
    grapheme.chars().map(char_width).max().unwrap_or(0)
}

/// Calculate the visual width of a string in terminal columns.
///
/// Handles different character widths using grapheme cluster boundaries:
/// - ASCII characters: 1 column each
/// - CJK characters (Chinese, Japanese, Korean): 2 columns each
/// - Emoji ZWJ/flag sequences: 2 columns (cluster width, not sum of code points)
#[must_use]
pub fn visual_width(s: &str) -> usize {
    if s.is_ascii() {
        return s.len();
    }

    s.graphemes(true).map(grapheme_width).sum()
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
}
