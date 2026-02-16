#![forbid(unsafe_code)]

//! Shaped-run render path with spacing/kerning deltas.
//!
//! This module transforms a [`ShapedRun`] into a sequence of cell-ready
//! placements that a renderer can consume to produce output with correct
//! spacing, kerning, and ligature handling.
//!
//! # Design
//!
//! The render path operates in sub-cell units (1/256 cell column) for
//! precision, then quantizes to integer cell positions for the terminal
//! grid. This preserves the kerning and spacing fidelity from the shaping
//! engine while producing deterministic cell-grid output.
//!
//! # Pipeline
//!
//! ```text
//! ShapedRun + text
//!     → ClusterMap (byte↔cell mapping)
//!     → ShapedLineLayout (cell placements with sub-cell spacing)
//!     → apply justification/tracking deltas
//!     → quantized cell positions for buffer rendering
//! ```
//!
//! # Example
//!
//! ```
//! use ftui_text::shaped_render::{ShapedLineLayout, RenderHint};
//! use ftui_text::shaping::{NoopShaper, TextShaper, FontFeatures};
//! use ftui_text::script_segmentation::{Script, RunDirection};
//!
//! let text = "Hello!";
//! let shaper = NoopShaper;
//! let features = FontFeatures::default();
//! let run = shaper.shape(text, Script::Latin, RunDirection::Ltr, &features);
//!
//! let layout = ShapedLineLayout::from_run(text, &run);
//! assert_eq!(layout.total_cells(), 6);
//! assert_eq!(layout.placements().len(), 6);
//! assert_eq!(layout.placements()[0].render_hint, RenderHint::DirectChar('H'));
//! ```

use crate::cluster_map::{ClusterEntry, ClusterMap};
use crate::justification::{GlueSpec, SUBCELL_SCALE};
use crate::shaping::ShapedRun;

// ---------------------------------------------------------------------------
// SpacingDelta — sub-cell adjustment
// ---------------------------------------------------------------------------

/// A sub-cell spacing adjustment applied between or within clusters.
///
/// Positive values add space (kerning expansion, justification stretch);
/// negative values remove space (kerning tightening, shrink).
///
/// Units: 1/256 of a cell column (same as [`SUBCELL_SCALE`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpacingDelta {
    /// Horizontal offset from nominal position in sub-cell units.
    /// Positive = shift right, negative = shift left.
    pub x_subcell: i32,
    /// Vertical offset from nominal position in sub-cell units.
    /// Used for superscript/subscript adjustments.
    pub y_subcell: i32,
}

impl SpacingDelta {
    /// Zero delta (no adjustment).
    pub const ZERO: Self = Self {
        x_subcell: 0,
        y_subcell: 0,
    };

    /// Whether this delta has any effect.
    #[inline]
    pub const fn is_zero(&self) -> bool {
        self.x_subcell == 0 && self.y_subcell == 0
    }

    /// Convert x offset to whole cells (rounded towards zero).
    #[inline]
    pub const fn x_cells(&self) -> i32 {
        self.x_subcell / SUBCELL_SCALE as i32
    }
}

// ---------------------------------------------------------------------------
// RenderHint — how to render cell content
// ---------------------------------------------------------------------------

/// Hint for how to render a cell's content.
///
/// This allows the renderer to choose the most efficient path: direct char
/// encoding for simple characters, or grapheme pool interning for complex
/// clusters (combining marks, emoji sequences, ligatures).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderHint {
    /// A single Unicode character that can be stored directly in a cell.
    /// This is the fast path for ASCII and most BMP characters.
    DirectChar(char),
    /// A multi-codepoint grapheme cluster that requires pool interning.
    /// Contains the full cluster string and its display width.
    Grapheme {
        /// The grapheme cluster text.
        text: String,
        /// Display width in cells.
        width: u8,
    },
    /// A continuation cell for a wide character (no content to render).
    Continuation,
}

// ---------------------------------------------------------------------------
// CellPlacement — a positioned cell in the output
// ---------------------------------------------------------------------------

/// A single cell placement in the shaped output line.
///
/// Each placement represents one terminal cell position with its content,
/// spacing adjustment, and source metadata for interaction overlays
/// (cursor, selection, search highlighting).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellPlacement {
    /// Cell column index (0-based from line start).
    pub cell_x: u32,
    /// What to render in this cell.
    pub render_hint: RenderHint,
    /// Sub-cell spacing delta from nominal position.
    /// The renderer may use this for sub-pixel positioning (web/GPU)
    /// or accumulate into whole-cell shifts (terminal).
    pub spacing: SpacingDelta,
    /// Source byte range in the original text.
    pub byte_start: u32,
    pub byte_end: u32,
    /// Grapheme index in the original text.
    pub grapheme_index: u32,
}

// ---------------------------------------------------------------------------
// ShapedLineLayout
// ---------------------------------------------------------------------------

/// A line of shaped text ready for rendering.
///
/// Contains cell placements with spacing deltas, plus metadata for
/// cursor/selection overlay computation. Deterministic: the same input
/// always produces the same layout.
#[derive(Debug, Clone)]
pub struct ShapedLineLayout {
    /// Ordered cell placements (one per cell column).
    placements: Vec<CellPlacement>,
    /// Total width in cells.
    total_cells: u32,
    /// Accumulated sub-cell remainder from spacing deltas.
    /// Renderers that support sub-pixel positioning can use this
    /// for precise placement; terminal renderers can ignore it.
    subcell_remainder: i32,
    /// The cluster map for this line (retained for interaction queries).
    cluster_map: ClusterMap,
}

impl ShapedLineLayout {
    /// Build a layout from a shaped run and its source text.
    ///
    /// Uses the `ClusterMap` to map glyph clusters to cell positions,
    /// and extracts spacing deltas from glyph advance differences.
    pub fn from_run(text: &str, run: &ShapedRun) -> Self {
        if text.is_empty() || run.is_empty() {
            return Self {
                placements: Vec::new(),
                total_cells: 0,
                subcell_remainder: 0,
                cluster_map: ClusterMap::from_text(""),
            };
        }

        let cluster_map = ClusterMap::from_shaped_run(text, run);
        let mut placements = Vec::with_capacity(cluster_map.total_cells());
        let mut subcell_accumulator: i32 = 0;

        // Build placement for each cluster in the map.
        for entry in cluster_map.entries() {
            let cluster_text = &text[entry.byte_start as usize..entry.byte_end as usize];
            let nominal_width = entry.cell_width as i32;

            // Compute spacing delta from shaped glyph advances.
            let shaped_advance = sum_cluster_advance(run, entry);
            let delta_subcell = shaped_advance - (nominal_width * SUBCELL_SCALE as i32);
            subcell_accumulator += delta_subcell;

            let spacing = if delta_subcell != 0 {
                // Also check for y-offsets from the first glyph in this cluster.
                let y_offset = first_cluster_y_offset(run, entry);
                SpacingDelta {
                    x_subcell: delta_subcell,
                    y_subcell: y_offset,
                }
            } else {
                let y_offset = first_cluster_y_offset(run, entry);
                if y_offset != 0 {
                    SpacingDelta {
                        x_subcell: 0,
                        y_subcell: y_offset,
                    }
                } else {
                    SpacingDelta::ZERO
                }
            };

            // Determine render hint.
            let hint = render_hint_for_cluster(cluster_text, entry.cell_width);

            // Emit primary cell.
            placements.push(CellPlacement {
                cell_x: entry.cell_start,
                render_hint: hint,
                spacing,
                byte_start: entry.byte_start,
                byte_end: entry.byte_end,
                grapheme_index: entry.grapheme_index,
            });

            // Emit continuation cells for wide characters.
            for cont in 1..entry.cell_width {
                placements.push(CellPlacement {
                    cell_x: entry.cell_start + cont as u32,
                    render_hint: RenderHint::Continuation,
                    spacing: SpacingDelta::ZERO,
                    byte_start: entry.byte_start,
                    byte_end: entry.byte_end,
                    grapheme_index: entry.grapheme_index,
                });
            }
        }

        Self {
            placements,
            total_cells: cluster_map.total_cells() as u32,
            subcell_remainder: subcell_accumulator,
            cluster_map,
        }
    }

    /// Build a layout from plain text (no shaping, terminal mode).
    ///
    /// Equivalent to shaping with `NoopShaper` — each grapheme maps to
    /// cells based on display width, with no spacing deltas.
    pub fn from_text(text: &str) -> Self {
        if text.is_empty() {
            return Self {
                placements: Vec::new(),
                total_cells: 0,
                subcell_remainder: 0,
                cluster_map: ClusterMap::from_text(""),
            };
        }

        let cluster_map = ClusterMap::from_text(text);
        let mut placements = Vec::with_capacity(cluster_map.total_cells());

        for entry in cluster_map.entries() {
            let cluster_text = &text[entry.byte_start as usize..entry.byte_end as usize];
            let hint = render_hint_for_cluster(cluster_text, entry.cell_width);

            placements.push(CellPlacement {
                cell_x: entry.cell_start,
                render_hint: hint,
                spacing: SpacingDelta::ZERO,
                byte_start: entry.byte_start,
                byte_end: entry.byte_end,
                grapheme_index: entry.grapheme_index,
            });

            for cont in 1..entry.cell_width {
                placements.push(CellPlacement {
                    cell_x: entry.cell_start + cont as u32,
                    render_hint: RenderHint::Continuation,
                    spacing: SpacingDelta::ZERO,
                    byte_start: entry.byte_start,
                    byte_end: entry.byte_end,
                    grapheme_index: entry.grapheme_index,
                });
            }
        }

        Self {
            placements,
            total_cells: cluster_map.total_cells() as u32,
            subcell_remainder: 0,
            cluster_map,
        }
    }

    /// Apply justification spacing to inter-word gaps.
    ///
    /// `ratio_fixed` is in 1/256 sub-cell units (positive = stretch,
    /// negative = shrink). Space characters get their glue adjusted
    /// according to the ratio.
    pub fn apply_justification(&mut self, text: &str, ratio_fixed: i32, glue: &GlueSpec) {
        if ratio_fixed == 0 || self.placements.is_empty() {
            return;
        }

        let adjusted_width_subcell = glue.adjusted_width(ratio_fixed);
        let natural_subcell = glue.natural_subcell;
        let delta_per_space = adjusted_width_subcell as i32 - natural_subcell as i32;

        if delta_per_space == 0 {
            return;
        }

        for placement in &mut self.placements {
            if matches!(placement.render_hint, RenderHint::Continuation) {
                continue;
            }

            let byte_start = placement.byte_start as usize;
            let byte_end = placement.byte_end as usize;
            if byte_start < text.len() && byte_end <= text.len() {
                let cluster = &text[byte_start..byte_end];
                if cluster.chars().all(|c| c == ' ' || c == '\u{00A0}') {
                    placement.spacing.x_subcell += delta_per_space;
                    self.subcell_remainder += delta_per_space;
                }
            }
        }
    }

    /// Apply uniform letter-spacing (tracking) to all inter-cluster gaps.
    ///
    /// `tracking_subcell` is in 1/256 cell units. Positive = expand,
    /// negative = tighten. The last cluster does not get trailing space.
    pub fn apply_tracking(&mut self, tracking_subcell: i32) {
        if tracking_subcell == 0 || self.placements.is_empty() {
            return;
        }

        // Apply tracking to all primary cells except the last.
        let mut last_grapheme = u32::MAX;
        let primary_count = self
            .placements
            .iter()
            .filter(|p| !matches!(p.render_hint, RenderHint::Continuation))
            .count();

        if primary_count <= 1 {
            return;
        }

        let mut seen = 0;
        for placement in &mut self.placements {
            if matches!(placement.render_hint, RenderHint::Continuation) {
                continue;
            }
            seen += 1;
            if seen < primary_count && placement.grapheme_index != last_grapheme {
                placement.spacing.x_subcell += tracking_subcell;
                self.subcell_remainder += tracking_subcell;
                last_grapheme = placement.grapheme_index;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// The cell placements in order.
    #[inline]
    pub fn placements(&self) -> &[CellPlacement] {
        &self.placements
    }

    /// Total width in cells.
    #[inline]
    pub fn total_cells(&self) -> usize {
        self.total_cells as usize
    }

    /// Accumulated sub-cell remainder from all spacing deltas.
    ///
    /// Terminal renderers can ignore this. Web/GPU renderers can use it
    /// for sub-pixel positioning of subsequent content.
    #[inline]
    pub fn subcell_remainder(&self) -> i32 {
        self.subcell_remainder
    }

    /// The underlying cluster map (for interaction queries).
    #[inline]
    pub fn cluster_map(&self) -> &ClusterMap {
        &self.cluster_map
    }

    /// Whether the layout is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.placements.is_empty()
    }

    /// Get the placement for a cell column.
    pub fn placement_at_cell(&self, cell_x: usize) -> Option<&CellPlacement> {
        self.placements.iter().find(|p| p.cell_x as usize == cell_x)
    }

    /// Get all placements for a grapheme index.
    pub fn placements_for_grapheme(&self, grapheme_index: usize) -> Vec<&CellPlacement> {
        self.placements
            .iter()
            .filter(|p| p.grapheme_index as usize == grapheme_index)
            .collect()
    }

    /// Extract the source text for a cell range (delegates to ClusterMap).
    pub fn extract_text<'a>(&self, source: &'a str, cell_start: usize, cell_end: usize) -> &'a str {
        self.cluster_map
            .extract_text_for_cells(source, cell_start, cell_end)
    }

    /// Check if any placement has non-zero spacing deltas.
    pub fn has_spacing_deltas(&self) -> bool {
        self.placements.iter().any(|p| !p.spacing.is_zero())
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Sum the x_advance values for all glyphs in a cluster, in sub-cell units.
fn sum_cluster_advance(run: &ShapedRun, entry: &ClusterEntry) -> i32 {
    let byte_start = entry.byte_start;
    let mut total = 0i32;

    for glyph in &run.glyphs {
        if glyph.cluster == byte_start {
            total += glyph.x_advance * SUBCELL_SCALE as i32;
        }
    }

    total
}

/// Get the y_offset of the first glyph in a cluster, in sub-cell units.
fn first_cluster_y_offset(run: &ShapedRun, entry: &ClusterEntry) -> i32 {
    let byte_start = entry.byte_start;

    for glyph in &run.glyphs {
        if glyph.cluster == byte_start {
            return glyph.y_offset * SUBCELL_SCALE as i32;
        }
    }

    0
}

/// Determine the render hint for a grapheme cluster.
fn render_hint_for_cluster(cluster_text: &str, cell_width: u8) -> RenderHint {
    let mut chars = cluster_text.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return RenderHint::DirectChar(' '),
    };

    if chars.next().is_none() {
        // Single-codepoint cluster: use direct char encoding.
        RenderHint::DirectChar(first)
    } else {
        // Multi-codepoint cluster: needs grapheme pool interning.
        RenderHint::Grapheme {
            text: cluster_text.to_string(),
            width: cell_width,
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script_segmentation::{RunDirection, Script};
    use crate::shaping::{FontFeatures, NoopShaper, TextShaper};

    // -----------------------------------------------------------------------
    // Construction tests
    // -----------------------------------------------------------------------

    #[test]
    fn empty_layout() {
        let layout = ShapedLineLayout::from_text("");
        assert!(layout.is_empty());
        assert_eq!(layout.total_cells(), 0);
        assert_eq!(layout.subcell_remainder(), 0);
    }

    #[test]
    fn ascii_layout() {
        let layout = ShapedLineLayout::from_text("Hello");
        assert_eq!(layout.total_cells(), 5);
        assert_eq!(layout.placements().len(), 5);
        assert!(!layout.has_spacing_deltas());

        for (i, p) in layout.placements().iter().enumerate() {
            assert_eq!(p.cell_x, i as u32);
            assert_eq!(p.spacing, SpacingDelta::ZERO);
            match &p.render_hint {
                RenderHint::DirectChar(c) => {
                    assert_eq!(*c, "Hello".chars().nth(i).unwrap());
                }
                _ => panic!("Expected DirectChar for ASCII"),
            }
        }
    }

    #[test]
    fn wide_char_layout() {
        let layout = ShapedLineLayout::from_text("A\u{4E16}B");
        // A(1) + 世(2) + B(1) = 4 cells
        assert_eq!(layout.total_cells(), 4);
        // 3 graphemes → 3 primary + 1 continuation = 4 placements
        assert_eq!(layout.placements().len(), 4);

        // A at cell 0
        assert_eq!(layout.placements()[0].cell_x, 0);
        assert!(matches!(
            layout.placements()[0].render_hint,
            RenderHint::DirectChar('A')
        ));

        // 世 at cell 1
        assert_eq!(layout.placements()[1].cell_x, 1);
        assert!(matches!(
            layout.placements()[1].render_hint,
            RenderHint::DirectChar('\u{4E16}')
        ));

        // Continuation at cell 2
        assert_eq!(layout.placements()[2].cell_x, 2);
        assert!(matches!(
            layout.placements()[2].render_hint,
            RenderHint::Continuation
        ));

        // B at cell 3
        assert_eq!(layout.placements()[3].cell_x, 3);
        assert!(matches!(
            layout.placements()[3].render_hint,
            RenderHint::DirectChar('B')
        ));
    }

    #[test]
    fn combining_mark_uses_grapheme() {
        let layout = ShapedLineLayout::from_text("e\u{0301}");
        assert_eq!(layout.total_cells(), 1);
        assert_eq!(layout.placements().len(), 1);

        match &layout.placements()[0].render_hint {
            RenderHint::Grapheme { text, width } => {
                assert_eq!(text, "e\u{0301}");
                assert_eq!(*width, 1);
            }
            _ => panic!("Expected Grapheme for combining mark"),
        }
    }

    // -----------------------------------------------------------------------
    // Shaped run construction
    // -----------------------------------------------------------------------

    #[test]
    fn from_shaped_run_noop() {
        let text = "Hello!";
        let shaper = NoopShaper;
        let ff = FontFeatures::default();
        let run = shaper.shape(text, Script::Latin, RunDirection::Ltr, &ff);

        let layout = ShapedLineLayout::from_run(text, &run);
        assert_eq!(layout.total_cells(), 6);
        assert_eq!(layout.placements().len(), 6);

        // NoopShaper should produce no spacing deltas.
        assert!(!layout.has_spacing_deltas());
    }

    #[test]
    fn from_shaped_run_wide() {
        let text = "Hi\u{4E16}!";
        let shaper = NoopShaper;
        let ff = FontFeatures::default();
        let run = shaper.shape(text, Script::Latin, RunDirection::Ltr, &ff);

        let layout = ShapedLineLayout::from_run(text, &run);
        // H(1) + i(1) + 世(2) + !(1) = 5 cells
        assert_eq!(layout.total_cells(), 5);
    }

    #[test]
    fn from_run_empty() {
        let layout = ShapedLineLayout::from_run(
            "",
            &ShapedRun {
                glyphs: vec![],
                total_advance: 0,
            },
        );
        assert!(layout.is_empty());
    }

    // -----------------------------------------------------------------------
    // Interaction helpers
    // -----------------------------------------------------------------------

    #[test]
    fn placement_at_cell() {
        let layout = ShapedLineLayout::from_text("ABC");
        let p = layout.placement_at_cell(1).unwrap();
        assert_eq!(p.cell_x, 1);
        assert!(matches!(p.render_hint, RenderHint::DirectChar('B')));

        assert!(layout.placement_at_cell(5).is_none());
    }

    #[test]
    fn placements_for_grapheme_wide() {
        let layout = ShapedLineLayout::from_text("\u{4E16}");
        let ps = layout.placements_for_grapheme(0);
        assert_eq!(ps.len(), 2); // primary + continuation
    }

    #[test]
    fn extract_text_range() {
        let text = "Hello World";
        let layout = ShapedLineLayout::from_text(text);
        assert_eq!(layout.extract_text(text, 0, 5), "Hello");
        assert_eq!(layout.extract_text(text, 6, 11), "World");
    }

    // -----------------------------------------------------------------------
    // Justification
    // -----------------------------------------------------------------------

    #[test]
    fn apply_justification_stretch() {
        let text = "hello world";
        let mut layout = ShapedLineLayout::from_text(text);

        // Stretch the space to 1.5 cells.
        let ratio = SUBCELL_SCALE as i32; // ratio = 1.0 (full stretch)
        layout.apply_justification(text, ratio, &GlueSpec::WORD_SPACE);

        // The space at index 5 should have a positive delta.
        assert!(layout.has_spacing_deltas());

        let space_placement = layout
            .placements()
            .iter()
            .find(|p| p.byte_start == 5 && !matches!(p.render_hint, RenderHint::Continuation));
        assert!(space_placement.is_some());
        let sp = space_placement.unwrap();
        assert!(sp.spacing.x_subcell > 0);
    }

    #[test]
    fn apply_justification_no_ratio() {
        let text = "hello world";
        let mut layout = ShapedLineLayout::from_text(text);
        layout.apply_justification(text, 0, &GlueSpec::WORD_SPACE);
        assert!(!layout.has_spacing_deltas());
    }

    // -----------------------------------------------------------------------
    // Tracking
    // -----------------------------------------------------------------------

    #[test]
    fn apply_tracking_basic() {
        let text = "ABC";
        let mut layout = ShapedLineLayout::from_text(text);
        layout.apply_tracking(32); // 1/8 cell per gap

        // First two graphemes should have tracking, last should not.
        let primary: Vec<_> = layout
            .placements()
            .iter()
            .filter(|p| !matches!(p.render_hint, RenderHint::Continuation))
            .collect();

        assert_eq!(primary.len(), 3);
        assert_eq!(primary[0].spacing.x_subcell, 32);
        assert_eq!(primary[1].spacing.x_subcell, 32);
        assert_eq!(primary[2].spacing.x_subcell, 0); // last: no trailing
    }

    #[test]
    fn apply_tracking_single_char() {
        let text = "A";
        let mut layout = ShapedLineLayout::from_text(text);
        layout.apply_tracking(32);
        // Single char: no tracking applied.
        assert!(!layout.has_spacing_deltas());
    }

    // -----------------------------------------------------------------------
    // Source metadata
    // -----------------------------------------------------------------------

    #[test]
    fn placement_byte_ranges() {
        let text = "A\u{4E16}B"; // A(1 byte) + 世(3 bytes) + B(1 byte)
        let layout = ShapedLineLayout::from_text(text);

        let primary: Vec<_> = layout
            .placements()
            .iter()
            .filter(|p| !matches!(p.render_hint, RenderHint::Continuation))
            .collect();

        assert_eq!(primary[0].byte_start, 0);
        assert_eq!(primary[0].byte_end, 1);
        assert_eq!(primary[1].byte_start, 1);
        assert_eq!(primary[1].byte_end, 4);
        assert_eq!(primary[2].byte_start, 4);
        assert_eq!(primary[2].byte_end, 5);
    }

    #[test]
    fn grapheme_indices_sequential() {
        let text = "Hello";
        let layout = ShapedLineLayout::from_text(text);

        for (i, p) in layout.placements().iter().enumerate() {
            assert_eq!(p.grapheme_index, i as u32);
        }
    }

    // -----------------------------------------------------------------------
    // Determinism
    // -----------------------------------------------------------------------

    #[test]
    fn deterministic_output() {
        let text = "Hello \u{4E16}\u{754C}!";

        let layout1 = ShapedLineLayout::from_text(text);
        let layout2 = ShapedLineLayout::from_text(text);

        assert_eq!(layout1.total_cells(), layout2.total_cells());
        assert_eq!(layout1.placements().len(), layout2.placements().len());

        for (a, b) in layout1.placements().iter().zip(layout2.placements()) {
            assert_eq!(a.cell_x, b.cell_x);
            assert_eq!(a.render_hint, b.render_hint);
            assert_eq!(a.spacing, b.spacing);
            assert_eq!(a.byte_start, b.byte_start);
            assert_eq!(a.byte_end, b.byte_end);
        }
    }

    // -----------------------------------------------------------------------
    // Spacing delta invariants
    // -----------------------------------------------------------------------

    #[test]
    fn noop_shaper_no_deltas() {
        let texts = ["Hello", "世界", "e\u{0301}f", "ABC 123"];
        let shaper = NoopShaper;
        let ff = FontFeatures::default();

        for text in texts {
            let run = shaper.shape(text, Script::Latin, RunDirection::Ltr, &ff);
            let layout = ShapedLineLayout::from_run(text, &run);
            assert!(
                !layout.has_spacing_deltas(),
                "NoopShaper should produce no deltas for {text:?}"
            );
        }
    }

    #[test]
    fn cell_x_monotonic() {
        let text = "Hello \u{4E16}\u{754C}!";
        let layout = ShapedLineLayout::from_text(text);

        for window in layout.placements().windows(2) {
            assert!(
                window[0].cell_x <= window[1].cell_x,
                "Cell positions must be monotonically non-decreasing"
            );
        }
    }

    #[test]
    fn all_cells_covered() {
        let text = "Hi\u{4E16}!";
        let layout = ShapedLineLayout::from_text(text);

        // Every cell column from 0 to total_cells-1 should have a placement.
        for col in 0..layout.total_cells() {
            assert!(
                layout.placement_at_cell(col).is_some(),
                "Cell column {col} has no placement"
            );
        }
    }
}
