//! Leading, baseline-grid, and paragraph spacing system.
//!
//! This module defines the vertical layout model for text rendering,
//! providing deterministic control over:
//!
//! - **Leading**: extra vertical space distributed between lines within
//!   a paragraph (analogous to CSS `line-height`).
//! - **Baseline grid**: snap line positions to a regular grid to maintain
//!   vertical rhythm across columns and pages.
//! - **Paragraph spacing**: configurable space before/after paragraphs.
//!
//! # Design
//!
//! All measurements are in sub-pixel units (1/256 px), matching the
//! fixed-point convention used by `ftui-render::fit_metrics`. Terminal
//! renderers can convert to cell rows by dividing by cell height.
//!
//! # Policy tiers
//!
//! Three quality tiers provide progressive enhancement:
//! - [`VerticalPolicy::Compact`]: zero leading, no baseline grid (terminal default).
//! - [`VerticalPolicy::Readable`]: moderate leading, paragraph spacing.
//! - [`VerticalPolicy::Typographic`]: baseline-grid alignment, fine-grained control.

use std::fmt;

/// Sub-pixel units per pixel (must match fit_metrics::SUBPX_SCALE).
const SUBPX_SCALE: u32 = 256;

// =========================================================================
// LeadingSpec
// =========================================================================

/// How leading (inter-line spacing) is specified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LeadingSpec {
    /// No extra leading — lines are packed at cell height.
    #[default]
    None,
    /// Fixed leading in sub-pixel units (1/256 px) added between lines.
    Fixed(u32),
    /// Leading as a fraction of line height (256 = 100% = double spacing).
    ///
    /// Common values:
    /// - 0 = single spacing (no extra)
    /// - 51 ≈ 20% extra (1.2x line height, CSS default for body text)
    /// - 128 = 50% extra (1.5x line height)
    /// - 256 = 100% extra (double spacing)
    Proportional(u32),
}

impl LeadingSpec {
    /// Compute the actual leading in sub-pixel units for a given line height.
    #[must_use]
    pub fn resolve(&self, line_height_subpx: u32) -> u32 {
        match *self {
            LeadingSpec::None => 0,
            LeadingSpec::Fixed(v) => v,
            LeadingSpec::Proportional(frac) => {
                // leading = line_height * frac / SUBPX_SCALE
                let product = (line_height_subpx as u64) * (frac as u64);
                (product / SUBPX_SCALE as u64) as u32
            }
        }
    }

    /// CSS-style 1.2x line height (20% extra leading).
    pub const CSS_DEFAULT: Self = Self::Proportional(51);

    /// 1.5x line height.
    pub const ONE_HALF: Self = Self::Proportional(128);

    /// Double spacing.
    pub const DOUBLE: Self = Self::Proportional(256);
}

impl fmt::Display for LeadingSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::Fixed(v) => write!(f, "fixed({:.1}px)", *v as f64 / SUBPX_SCALE as f64),
            Self::Proportional(frac) => {
                write!(f, "{:.0}%", *frac as f64 / SUBPX_SCALE as f64 * 100.0)
            }
        }
    }
}

// =========================================================================
// ParagraphSpacing
// =========================================================================

/// Configurable space before and after paragraphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParagraphSpacing {
    /// Space before the first line of a paragraph (sub-pixel units).
    pub before_subpx: u32,
    /// Space after the last line of a paragraph (sub-pixel units).
    pub after_subpx: u32,
}

impl ParagraphSpacing {
    /// No extra paragraph spacing.
    pub const NONE: Self = Self {
        before_subpx: 0,
        after_subpx: 0,
    };

    /// One full line of spacing between paragraphs (at given line height).
    #[must_use]
    pub fn one_line(line_height_subpx: u32) -> Self {
        Self {
            before_subpx: 0,
            after_subpx: line_height_subpx,
        }
    }

    /// Half-line spacing between paragraphs.
    #[must_use]
    pub fn half_line(line_height_subpx: u32) -> Self {
        Self {
            before_subpx: 0,
            after_subpx: line_height_subpx / 2,
        }
    }

    /// Custom spacing in sub-pixel units.
    #[must_use]
    pub const fn custom(before: u32, after: u32) -> Self {
        Self {
            before_subpx: before,
            after_subpx: after,
        }
    }

    /// Total paragraph overhead (before + after) in sub-pixel units.
    #[must_use]
    pub const fn total(&self) -> u32 {
        self.before_subpx.saturating_add(self.after_subpx)
    }
}

impl Default for ParagraphSpacing {
    fn default() -> Self {
        Self::NONE
    }
}

impl fmt::Display for ParagraphSpacing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "before={:.1}px after={:.1}px",
            self.before_subpx as f64 / SUBPX_SCALE as f64,
            self.after_subpx as f64 / SUBPX_SCALE as f64,
        )
    }
}

// =========================================================================
// BaselineGrid
// =========================================================================

/// Baseline grid alignment configuration.
///
/// When active, line positions are snapped to a regular vertical grid
/// to maintain consistent vertical rhythm. This is essential for
/// multi-column layouts where lines should align across columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BaselineGrid {
    /// Grid interval in sub-pixel units.
    ///
    /// All line positions are rounded up to the nearest multiple of this value.
    /// Typically set to the line height (line_height + leading).
    pub interval_subpx: u32,
    /// Offset from the top of the text area (sub-pixel units).
    ///
    /// Shifts the grid to align with an arbitrary starting position.
    pub offset_subpx: u32,
}

impl BaselineGrid {
    /// No baseline grid (disabled).
    pub const NONE: Self = Self {
        interval_subpx: 0,
        offset_subpx: 0,
    };

    /// Create a grid from line height and leading.
    #[must_use]
    pub const fn from_line_height(line_height_subpx: u32, leading_subpx: u32) -> Self {
        Self {
            interval_subpx: line_height_subpx.saturating_add(leading_subpx),
            offset_subpx: 0,
        }
    }

    /// Whether the grid is active (non-zero interval).
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.interval_subpx > 0
    }

    /// Snap a vertical position to the grid.
    ///
    /// Rounds up to the nearest grid line at or above `pos`.
    #[must_use]
    pub const fn snap(&self, pos_subpx: u32) -> u32 {
        if self.interval_subpx == 0 {
            return pos_subpx;
        }
        let adjusted = pos_subpx.saturating_sub(self.offset_subpx);
        let remainder = adjusted % self.interval_subpx;
        if remainder == 0 {
            pos_subpx
        } else {
            pos_subpx.saturating_add(self.interval_subpx - remainder)
        }
    }
}

impl Default for BaselineGrid {
    fn default() -> Self {
        Self::NONE
    }
}

// =========================================================================
// VerticalPolicy
// =========================================================================

/// Pre-configured vertical layout policy tiers.
///
/// These provide progressive enhancement from compact terminal rendering
/// to high-quality typographic output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum VerticalPolicy {
    /// Zero leading, no baseline grid, no paragraph spacing.
    /// Suitable for terminal UIs where every row counts.
    #[default]
    Compact,
    /// Moderate leading (20% of line height), half-line paragraph spacing.
    /// Good for readable text content within a terminal.
    Readable,
    /// Baseline-grid alignment with configurable leading and paragraph spacing.
    /// For high-quality proportional text rendering.
    Typographic,
}

impl VerticalPolicy {
    /// Resolve this policy into a concrete [`VerticalMetrics`] configuration
    /// given the line height.
    #[must_use]
    pub fn resolve(&self, line_height_subpx: u32) -> VerticalMetrics {
        match self {
            Self::Compact => VerticalMetrics {
                leading: LeadingSpec::None,
                paragraph_spacing: ParagraphSpacing::NONE,
                baseline_grid: BaselineGrid::NONE,
                first_line_indent_subpx: 0,
            },
            Self::Readable => {
                let leading = LeadingSpec::CSS_DEFAULT;
                let leading_val = leading.resolve(line_height_subpx);
                VerticalMetrics {
                    leading,
                    paragraph_spacing: ParagraphSpacing::half_line(
                        line_height_subpx.saturating_add(leading_val),
                    ),
                    baseline_grid: BaselineGrid::NONE,
                    first_line_indent_subpx: 0,
                }
            }
            Self::Typographic => {
                let leading = LeadingSpec::CSS_DEFAULT;
                let leading_val = leading.resolve(line_height_subpx);
                let total_line = line_height_subpx.saturating_add(leading_val);
                VerticalMetrics {
                    leading,
                    paragraph_spacing: ParagraphSpacing::one_line(total_line),
                    baseline_grid: BaselineGrid::from_line_height(line_height_subpx, leading_val),
                    first_line_indent_subpx: 2 * SUBPX_SCALE, // 2px indent
                }
            }
        }
    }
}

impl fmt::Display for VerticalPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compact => write!(f, "compact"),
            Self::Readable => write!(f, "readable"),
            Self::Typographic => write!(f, "typographic"),
        }
    }
}

// =========================================================================
// VerticalMetrics
// =========================================================================

/// Resolved vertical layout configuration.
///
/// All values are in sub-pixel units (1/256 px).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct VerticalMetrics {
    /// Inter-line leading specification.
    pub leading: LeadingSpec,
    /// Paragraph spacing configuration.
    pub paragraph_spacing: ParagraphSpacing,
    /// Baseline grid alignment.
    pub baseline_grid: BaselineGrid,
    /// First-line indent in sub-pixel units.
    pub first_line_indent_subpx: u32,
}

impl VerticalMetrics {
    /// Compute the total height of a paragraph in sub-pixel units.
    ///
    /// Given `line_count` lines at `line_height_subpx` base height:
    ///   total = before + sum(line_heights) + (line_count-1)*leading + after
    ///
    /// If baseline grid is active, the result is snapped to the grid.
    #[must_use]
    pub fn paragraph_height(&self, line_count: usize, line_height_subpx: u32) -> u32 {
        if line_count == 0 {
            return 0;
        }

        let leading_val = self.leading.resolve(line_height_subpx);
        let lines_height = (line_count as u32) * line_height_subpx;
        let inter_leading = if line_count > 1 {
            ((line_count - 1) as u32) * leading_val
        } else {
            0
        };

        let content_height = lines_height.saturating_add(inter_leading);
        let total = self
            .paragraph_spacing
            .before_subpx
            .saturating_add(content_height)
            .saturating_add(self.paragraph_spacing.after_subpx);

        if self.baseline_grid.is_active() {
            self.baseline_grid.snap(total)
        } else {
            total
        }
    }

    /// Compute the Y position of line `n` (0-indexed) within a paragraph.
    ///
    /// Accounts for `before` spacing, leading, and baseline grid snapping.
    #[must_use]
    pub fn line_y(&self, line_index: usize, line_height_subpx: u32) -> u32 {
        let leading_val = self.leading.resolve(line_height_subpx);
        let line_step = line_height_subpx.saturating_add(leading_val);

        let raw_y = self
            .paragraph_spacing
            .before_subpx
            .saturating_add((line_index as u32) * line_step);

        if self.baseline_grid.is_active() {
            self.baseline_grid.snap(raw_y)
        } else {
            raw_y
        }
    }

    /// Total height for a multi-paragraph document.
    ///
    /// `paragraphs` is a slice of line counts per paragraph.
    #[must_use]
    pub fn document_height(&self, paragraphs: &[usize], line_height_subpx: u32) -> u32 {
        let mut total = 0u32;
        for (idx, &line_count) in paragraphs.iter().enumerate() {
            if idx > 0 {
                // Collapse paragraph spacing: use max of previous after and current before.
                // CSS-style margin collapsing.
                let collapsed = self
                    .paragraph_spacing
                    .after_subpx
                    .max(self.paragraph_spacing.before_subpx);
                total = total.saturating_add(collapsed);
            } else {
                total = total.saturating_add(self.paragraph_spacing.before_subpx);
            }

            let leading_val = self.leading.resolve(line_height_subpx);
            let lines_height = (line_count as u32) * line_height_subpx;
            let inter_leading = if line_count > 1 {
                ((line_count - 1) as u32) * leading_val
            } else {
                0
            };
            total = total
                .saturating_add(lines_height)
                .saturating_add(inter_leading);

            // Add after spacing for last paragraph
            if idx == paragraphs.len() - 1 {
                total = total.saturating_add(self.paragraph_spacing.after_subpx);
            }
        }

        if self.baseline_grid.is_active() {
            self.baseline_grid.snap(total)
        } else {
            total
        }
    }

    /// Convert total sub-pixel height to terminal cell rows.
    ///
    /// Rounds up: any partial row counts as a full row.
    #[must_use]
    pub fn to_cell_rows(height_subpx: u32, cell_height_subpx: u32) -> u16 {
        if cell_height_subpx == 0 {
            return 0;
        }
        let rows = height_subpx.div_ceil(cell_height_subpx);
        rows.min(u16::MAX as u32) as u16
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const LINE_H: u32 = 16 * SUBPX_SCALE; // 16px line height

    // ── LeadingSpec ───────────────────────────────────────────────────

    #[test]
    fn leading_none() {
        assert_eq!(LeadingSpec::None.resolve(LINE_H), 0);
    }

    #[test]
    fn leading_fixed() {
        let spec = LeadingSpec::Fixed(2 * SUBPX_SCALE); // 2px
        assert_eq!(spec.resolve(LINE_H), 2 * SUBPX_SCALE);
    }

    #[test]
    fn leading_proportional_20_percent() {
        let spec = LeadingSpec::CSS_DEFAULT; // ~20%
        let leading = spec.resolve(LINE_H);
        // 16px * 51/256 ≈ 3.18px → 3 * 256 = 816 subpx
        assert_eq!(leading, 816);
    }

    #[test]
    fn leading_proportional_50_percent() {
        let leading = LeadingSpec::ONE_HALF.resolve(LINE_H);
        // 16px * 128/256 = 8px = 8 * 256 = 2048 subpx
        assert_eq!(leading, 2048);
    }

    #[test]
    fn leading_proportional_double() {
        let leading = LeadingSpec::DOUBLE.resolve(LINE_H);
        // 16px * 256/256 = 16px = 16 * 256 = 4096 subpx
        assert_eq!(leading, LINE_H);
    }

    #[test]
    fn leading_display() {
        assert_eq!(format!("{}", LeadingSpec::None), "none");
        let fixed = LeadingSpec::Fixed(2 * SUBPX_SCALE);
        assert!(format!("{fixed}").contains("2.0px"));
    }

    // ── ParagraphSpacing ──────────────────────────────────────────────

    #[test]
    fn spacing_none() {
        assert_eq!(ParagraphSpacing::NONE.total(), 0);
    }

    #[test]
    fn spacing_one_line() {
        let sp = ParagraphSpacing::one_line(LINE_H);
        assert_eq!(sp.before_subpx, 0);
        assert_eq!(sp.after_subpx, LINE_H);
        assert_eq!(sp.total(), LINE_H);
    }

    #[test]
    fn spacing_half_line() {
        let sp = ParagraphSpacing::half_line(LINE_H);
        assert_eq!(sp.after_subpx, LINE_H / 2);
    }

    #[test]
    fn spacing_custom() {
        let sp = ParagraphSpacing::custom(100, 200);
        assert_eq!(sp.before_subpx, 100);
        assert_eq!(sp.after_subpx, 200);
        assert_eq!(sp.total(), 300);
    }

    #[test]
    fn spacing_display() {
        let s = format!("{}", ParagraphSpacing::NONE);
        assert!(s.contains("0.0px"));
    }

    // ── BaselineGrid ──────────────────────────────────────────────────

    #[test]
    fn grid_none_is_inactive() {
        assert!(!BaselineGrid::NONE.is_active());
    }

    #[test]
    fn grid_from_line_height() {
        let grid = BaselineGrid::from_line_height(LINE_H, 2 * SUBPX_SCALE);
        assert!(grid.is_active());
        assert_eq!(grid.interval_subpx, LINE_H + 2 * SUBPX_SCALE);
    }

    #[test]
    fn grid_snap_exact() {
        let grid = BaselineGrid {
            interval_subpx: 1000,
            offset_subpx: 0,
        };
        assert_eq!(grid.snap(2000), 2000);
        assert_eq!(grid.snap(3000), 3000);
    }

    #[test]
    fn grid_snap_rounds_up() {
        let grid = BaselineGrid {
            interval_subpx: 1000,
            offset_subpx: 0,
        };
        assert_eq!(grid.snap(1), 1000);
        assert_eq!(grid.snap(999), 1000);
        assert_eq!(grid.snap(1001), 2000);
    }

    #[test]
    fn grid_snap_with_offset() {
        let grid = BaselineGrid {
            interval_subpx: 1000,
            offset_subpx: 200,
        };
        // pos=200 → adjusted=0 → remainder=0 → stays at 200
        assert_eq!(grid.snap(200), 200);
        // pos=500 → adjusted=300 → remainder=300 → snap to 500 + (1000-300) = 1200
        assert_eq!(grid.snap(500), 1200);
    }

    #[test]
    fn grid_snap_disabled() {
        assert_eq!(BaselineGrid::NONE.snap(42), 42);
    }

    // ── VerticalPolicy ────────────────────────────────────────────────

    #[test]
    fn policy_compact() {
        let m = VerticalPolicy::Compact.resolve(LINE_H);
        assert_eq!(m.leading, LeadingSpec::None);
        assert_eq!(m.paragraph_spacing, ParagraphSpacing::NONE);
        assert!(!m.baseline_grid.is_active());
        assert_eq!(m.first_line_indent_subpx, 0);
    }

    #[test]
    fn policy_readable() {
        let m = VerticalPolicy::Readable.resolve(LINE_H);
        assert_eq!(m.leading, LeadingSpec::CSS_DEFAULT);
        assert!(!m.baseline_grid.is_active());
        assert!(m.paragraph_spacing.after_subpx > 0);
    }

    #[test]
    fn policy_typographic() {
        let m = VerticalPolicy::Typographic.resolve(LINE_H);
        assert_eq!(m.leading, LeadingSpec::CSS_DEFAULT);
        assert!(m.baseline_grid.is_active());
        assert!(m.paragraph_spacing.after_subpx > 0);
        assert!(m.first_line_indent_subpx > 0);
    }

    #[test]
    fn policy_display() {
        assert_eq!(format!("{}", VerticalPolicy::Compact), "compact");
        assert_eq!(format!("{}", VerticalPolicy::Readable), "readable");
        assert_eq!(format!("{}", VerticalPolicy::Typographic), "typographic");
    }

    #[test]
    fn policy_default_is_compact() {
        assert_eq!(VerticalPolicy::default(), VerticalPolicy::Compact);
    }

    // ── VerticalMetrics ───────────────────────────────────────────────

    #[test]
    fn paragraph_height_zero_lines() {
        let m = VerticalPolicy::Compact.resolve(LINE_H);
        assert_eq!(m.paragraph_height(0, LINE_H), 0);
    }

    #[test]
    fn paragraph_height_single_line_compact() {
        let m = VerticalPolicy::Compact.resolve(LINE_H);
        assert_eq!(m.paragraph_height(1, LINE_H), LINE_H);
    }

    #[test]
    fn paragraph_height_multi_line_compact() {
        let m = VerticalPolicy::Compact.resolve(LINE_H);
        // 3 lines, no leading: 3 * 16px = 48px
        assert_eq!(m.paragraph_height(3, LINE_H), 3 * LINE_H);
    }

    #[test]
    fn paragraph_height_with_leading() {
        let mut m = VerticalPolicy::Compact.resolve(LINE_H);
        m.leading = LeadingSpec::Fixed(2 * SUBPX_SCALE); // 2px between lines
        // 3 lines: 3*16 + 2*2 = 52px = 52 * 256 = 13312 subpx
        assert_eq!(
            m.paragraph_height(3, LINE_H),
            3 * LINE_H + 2 * 2 * SUBPX_SCALE
        );
    }

    #[test]
    fn paragraph_height_with_spacing() {
        let mut m = VerticalPolicy::Compact.resolve(LINE_H);
        m.paragraph_spacing = ParagraphSpacing::custom(SUBPX_SCALE, SUBPX_SCALE);
        // 1 line + 1px before + 1px after = 18px
        assert_eq!(m.paragraph_height(1, LINE_H), LINE_H + 2 * SUBPX_SCALE);
    }

    #[test]
    fn line_y_compact() {
        let m = VerticalPolicy::Compact.resolve(LINE_H);
        assert_eq!(m.line_y(0, LINE_H), 0);
        assert_eq!(m.line_y(1, LINE_H), LINE_H);
        assert_eq!(m.line_y(2, LINE_H), 2 * LINE_H);
    }

    #[test]
    fn line_y_with_leading() {
        let mut m = VerticalPolicy::Compact.resolve(LINE_H);
        m.leading = LeadingSpec::Fixed(SUBPX_SCALE); // 1px
        assert_eq!(m.line_y(0, LINE_H), 0);
        assert_eq!(m.line_y(1, LINE_H), LINE_H + SUBPX_SCALE);
        assert_eq!(m.line_y(2, LINE_H), 2 * (LINE_H + SUBPX_SCALE));
    }

    #[test]
    fn line_y_with_before_spacing() {
        let mut m = VerticalPolicy::Compact.resolve(LINE_H);
        m.paragraph_spacing.before_subpx = SUBPX_SCALE; // 1px before
        assert_eq!(m.line_y(0, LINE_H), SUBPX_SCALE);
        assert_eq!(m.line_y(1, LINE_H), SUBPX_SCALE + LINE_H);
    }

    #[test]
    fn document_height_single_paragraph() {
        let m = VerticalPolicy::Compact.resolve(LINE_H);
        assert_eq!(m.document_height(&[3], LINE_H), 3 * LINE_H);
    }

    #[test]
    fn document_height_multi_paragraph() {
        let m = VerticalPolicy::Compact.resolve(LINE_H);
        // No spacing: 3+2 = 5 lines = 5 * 16px
        assert_eq!(m.document_height(&[3, 2], LINE_H), 5 * LINE_H);
    }

    #[test]
    fn document_height_with_spacing() {
        let mut m = VerticalPolicy::Compact.resolve(LINE_H);
        m.paragraph_spacing = ParagraphSpacing::custom(0, SUBPX_SCALE);
        // Two paragraphs: [3, 2]
        // Para 0: 0 before + 3*LINE_H + after folded into collapse
        // Between: collapsed = max(after=256, before=0) = 256
        // Para 1: 2*LINE_H + 256 after
        // Total: 3*LINE_H + 256 + 2*LINE_H + 256 = 5*LINE_H + 512
        assert_eq!(
            m.document_height(&[3, 2], LINE_H),
            5 * LINE_H + 2 * SUBPX_SCALE
        );
    }

    #[test]
    fn document_height_empty() {
        let m = VerticalPolicy::Compact.resolve(LINE_H);
        assert_eq!(m.document_height(&[], LINE_H), 0);
    }

    #[test]
    fn to_cell_rows_exact() {
        assert_eq!(VerticalMetrics::to_cell_rows(LINE_H * 3, LINE_H), 3);
    }

    #[test]
    fn to_cell_rows_rounds_up() {
        assert_eq!(VerticalMetrics::to_cell_rows(LINE_H * 3 + 1, LINE_H), 4);
    }

    #[test]
    fn to_cell_rows_zero_height() {
        assert_eq!(VerticalMetrics::to_cell_rows(0, LINE_H), 0);
    }

    #[test]
    fn to_cell_rows_zero_cell_height() {
        assert_eq!(VerticalMetrics::to_cell_rows(LINE_H, 0), 0);
    }

    // ── Determinism ────────────────────────────────────────────────────

    #[test]
    fn same_inputs_same_outputs() {
        let m1 = VerticalPolicy::Typographic.resolve(LINE_H);
        let m2 = VerticalPolicy::Typographic.resolve(LINE_H);
        assert_eq!(
            m1.paragraph_height(5, LINE_H),
            m2.paragraph_height(5, LINE_H)
        );
        assert_eq!(m1.line_y(3, LINE_H), m2.line_y(3, LINE_H));
    }

    #[test]
    fn baseline_grid_deterministic() {
        let grid = BaselineGrid::from_line_height(LINE_H, SUBPX_SCALE);
        let a = grid.snap(1234);
        let b = grid.snap(1234);
        assert_eq!(a, b);
    }
}
