#![forbid(unsafe_code)]

//! Bidirectional cluster↔cell mapping for shaped text.
//!
//! This module defines the [`ClusterMap`] — a precomputed index that maps
//! between source byte offsets, grapheme indices, and visual cell columns
//! in both directions. It enables correct cursor movement, selection,
//! copy extraction, and search highlighting over shaped text.
//!
//! # Invariants
//!
//! The cluster map guarantees:
//!
//! 1. **Round-trip preservation**: `byte → cell → byte` returns the original
//!    cluster start (never a mid-cluster position).
//! 2. **Monotonicity**: visual cell offsets increase with byte offsets.
//! 3. **Boundary alignment**: lookups always snap to grapheme cluster
//!    boundaries — never splitting a grapheme or shaped glyph cluster.
//! 4. **Continuation cell handling**: wide characters that span 2+ cells
//!    map back to the same source byte offset.
//! 5. **Completeness**: every source byte offset and every visual cell
//!    column has a defined mapping.
//!
//! # Example
//!
//! ```
//! use ftui_text::cluster_map::ClusterMap;
//!
//! // Build a cluster map from plain text
//! let map = ClusterMap::from_text("Hello 世界!");
//!
//! // Forward: byte offset → visual cell column
//! assert_eq!(map.byte_to_cell(0), 0);  // 'H' at cell 0
//! assert_eq!(map.byte_to_cell(6), 6);  // '世' at cell 6
//! assert_eq!(map.byte_to_cell(9), 8);  // '界' at cell 8
//!
//! // Reverse: visual cell column → byte offset
//! assert_eq!(map.cell_to_byte(0), 0);  // cell 0 → 'H'
//! assert_eq!(map.cell_to_byte(6), 6);  // cell 6 → '世'
//! assert_eq!(map.cell_to_byte(7), 6);  // cell 7 → '世' (continuation)
//!
//! // Selection: cell range → byte range
//! let (start, end) = map.cell_range_to_byte_range(6, 10);
//! assert_eq!(start, 6);   // '世'
//! assert_eq!(end, 12);    // end of '界'
//! ```

use crate::shaping::ShapedRun;
use unicode_segmentation::UnicodeSegmentation;

// ---------------------------------------------------------------------------
// ClusterEntry — per-grapheme-cluster record
// ---------------------------------------------------------------------------

/// A single entry in the cluster map, representing one grapheme cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClusterEntry {
    /// Start byte offset in the source string (inclusive).
    pub byte_start: u32,
    /// End byte offset in the source string (exclusive).
    pub byte_end: u32,
    /// Grapheme index (0-based position among graphemes).
    pub grapheme_index: u32,
    /// Start visual cell column (inclusive).
    pub cell_start: u32,
    /// Display width in cells (1 for normal, 2 for wide CJK/emoji).
    pub cell_width: u8,
}

impl ClusterEntry {
    /// The byte range of this cluster.
    #[inline]
    pub fn byte_range(&self) -> std::ops::Range<usize> {
        self.byte_start as usize..self.byte_end as usize
    }

    /// The cell range of this cluster (start..start+width).
    #[inline]
    pub fn cell_range(&self) -> std::ops::Range<usize> {
        self.cell_start as usize..(self.cell_start as usize + self.cell_width as usize)
    }

    /// End cell column (exclusive).
    #[inline]
    pub fn cell_end(&self) -> u32 {
        self.cell_start + self.cell_width as u32
    }
}

// ---------------------------------------------------------------------------
// ClusterMap
// ---------------------------------------------------------------------------

/// Bidirectional mapping between source byte offsets and visual cell columns.
///
/// Built from text (optionally with shaped glyph data) and provides O(log n)
/// lookups in both directions via binary search over the sorted cluster array.
///
/// # Construction
///
/// - [`from_text`](Self::from_text) — build from plain text (uses grapheme
///   cluster widths, suitable for terminal/monospace rendering).
/// - [`from_shaped_run`](Self::from_shaped_run) — build from a shaped glyph
///   run (uses glyph cluster byte offsets and advances, suitable for
///   proportional/shaped rendering).
#[derive(Debug, Clone)]
pub struct ClusterMap {
    /// Sorted by byte_start (and equivalently by cell_start due to monotonicity).
    entries: Vec<ClusterEntry>,
    /// Total visual width in cells.
    total_cells: u32,
    /// Total byte length of the source text.
    total_bytes: u32,
}

impl ClusterMap {
    /// Build a cluster map from plain text using grapheme cluster boundaries.
    ///
    /// Each grapheme cluster maps to 1 or 2 cells based on `display_width`.
    /// This is the appropriate constructor for terminal/monospace rendering.
    pub fn from_text(text: &str) -> Self {
        if text.is_empty() {
            return Self {
                entries: Vec::new(),
                total_cells: 0,
                total_bytes: 0,
            };
        }

        let mut entries = Vec::new();
        let mut cell_offset = 0u32;

        for (grapheme_idx, (byte_offset, grapheme)) in text.grapheme_indices(true).enumerate() {
            let width = crate::grapheme_width(grapheme) as u8;
            let byte_end = byte_offset + grapheme.len();

            entries.push(ClusterEntry {
                byte_start: byte_offset as u32,
                byte_end: byte_end as u32,
                grapheme_index: grapheme_idx as u32,
                cell_start: cell_offset,
                cell_width: width,
            });

            cell_offset += width as u32;
        }

        Self {
            entries,
            total_cells: cell_offset,
            total_bytes: text.len() as u32,
        }
    }

    /// Build a cluster map from a shaped glyph run.
    ///
    /// Uses glyph cluster byte offsets from the `ShapedRun` to determine
    /// cluster boundaries, with advances determining cell widths.
    ///
    /// For terminal rendering (NoopShaper), each glyph maps to one grapheme
    /// cluster. For proportional rendering, multiple glyphs may share a
    /// cluster (ligatures) or one glyph may span multiple characters.
    pub fn from_shaped_run(text: &str, run: &ShapedRun) -> Self {
        if text.is_empty() || run.is_empty() {
            return Self {
                entries: Vec::new(),
                total_cells: 0,
                total_bytes: 0,
            };
        }

        // Group glyphs by cluster (byte offset).
        // Shaped glyphs share a `cluster` value when they form a ligature
        // or complex glyph group.
        let mut entries = Vec::new();
        let mut cell_offset = 0u32;
        let mut grapheme_idx = 0u32;

        let mut i = 0;
        while i < run.glyphs.len() {
            let cluster_byte = run.glyphs[i].cluster as usize;
            let mut cluster_advance = 0i32;

            // Accumulate all glyphs sharing this cluster.
            let mut j = i;
            while j < run.glyphs.len() && run.glyphs[j].cluster as usize == cluster_byte {
                cluster_advance += run.glyphs[j].x_advance;
                j += 1;
            }

            // Find the next cluster's byte offset to determine this cluster's byte range.
            let next_byte = if j < run.glyphs.len() {
                run.glyphs[j].cluster as usize
            } else {
                text.len()
            };

            // Use advance as cell width (for terminal, this is already in cells).
            let width = cluster_advance.unsigned_abs().min(255) as u8;

            entries.push(ClusterEntry {
                byte_start: cluster_byte as u32,
                byte_end: next_byte as u32,
                grapheme_index: grapheme_idx,
                cell_start: cell_offset,
                cell_width: width,
            });

            cell_offset += width as u32;
            grapheme_idx += 1;
            i = j;
        }

        Self {
            entries,
            total_cells: cell_offset,
            total_bytes: text.len() as u32,
        }
    }

    // -----------------------------------------------------------------------
    // Forward lookups (byte → cell)
    // -----------------------------------------------------------------------

    /// Map a byte offset to its visual cell column.
    ///
    /// If the byte offset falls mid-cluster, it snaps to the cluster's
    /// start cell. Returns `total_cells` for offsets at or past the end.
    pub fn byte_to_cell(&self, byte_offset: usize) -> usize {
        if self.entries.is_empty() || byte_offset >= self.total_bytes as usize {
            return self.total_cells as usize;
        }

        match self
            .entries
            .binary_search_by_key(&(byte_offset as u32), |e| e.byte_start)
        {
            Ok(idx) => self.entries[idx].cell_start as usize,
            Err(idx) => {
                // byte_offset is mid-cluster — snap to containing cluster.
                if idx > 0 {
                    self.entries[idx - 1].cell_start as usize
                } else {
                    0
                }
            }
        }
    }

    /// Map a byte offset to the containing `ClusterEntry`.
    ///
    /// Returns `None` for empty maps or offsets past the end.
    pub fn byte_to_entry(&self, byte_offset: usize) -> Option<&ClusterEntry> {
        if self.entries.is_empty() {
            return None;
        }

        match self
            .entries
            .binary_search_by_key(&(byte_offset as u32), |e| e.byte_start)
        {
            Ok(idx) => Some(&self.entries[idx]),
            Err(idx) => {
                if idx > 0 && (byte_offset as u32) < self.entries[idx - 1].byte_end {
                    Some(&self.entries[idx - 1])
                } else {
                    None
                }
            }
        }
    }

    /// Map a byte range to a visual cell range.
    ///
    /// Returns `(cell_start, cell_end)` covering all clusters that overlap
    /// the given byte range.
    pub fn byte_range_to_cell_range(&self, byte_start: usize, byte_end: usize) -> (usize, usize) {
        if self.entries.is_empty() || byte_start >= byte_end {
            return (0, 0);
        }

        let start_cell = self.byte_to_cell(byte_start);

        // Find the cell_end for the cluster containing byte_end - 1.
        let end_cell = if byte_end >= self.total_bytes as usize {
            self.total_cells as usize
        } else {
            match self
                .entries
                .binary_search_by_key(&(byte_end as u32), |e| e.byte_start)
            {
                Ok(idx) => self.entries[idx].cell_start as usize,
                Err(idx) => {
                    if idx > 0 {
                        self.entries[idx - 1].cell_end() as usize
                    } else {
                        0
                    }
                }
            }
        };

        (start_cell, end_cell)
    }

    // -----------------------------------------------------------------------
    // Reverse lookups (cell → byte)
    // -----------------------------------------------------------------------

    /// Map a visual cell column to a source byte offset.
    ///
    /// Continuation cells (cells within a wide character) map back to the
    /// cluster's start byte. Returns `total_bytes` for cells at or past
    /// the total width.
    pub fn cell_to_byte(&self, cell_col: usize) -> usize {
        if self.entries.is_empty() || cell_col >= self.total_cells as usize {
            return self.total_bytes as usize;
        }

        match self
            .entries
            .binary_search_by_key(&(cell_col as u32), |e| e.cell_start)
        {
            Ok(idx) => self.entries[idx].byte_start as usize,
            Err(idx) => {
                // cell_col is a continuation cell — snap to containing cluster.
                if idx > 0 {
                    self.entries[idx - 1].byte_start as usize
                } else {
                    0
                }
            }
        }
    }

    /// Map a visual cell column to the containing `ClusterEntry`.
    ///
    /// Returns `None` for empty maps or cells past the total width.
    pub fn cell_to_entry(&self, cell_col: usize) -> Option<&ClusterEntry> {
        if self.entries.is_empty() || cell_col >= self.total_cells as usize {
            return None;
        }

        match self
            .entries
            .binary_search_by_key(&(cell_col as u32), |e| e.cell_start)
        {
            Ok(idx) => Some(&self.entries[idx]),
            Err(idx) => {
                if idx > 0 {
                    let entry = &self.entries[idx - 1];
                    if (cell_col as u32) < entry.cell_end() {
                        Some(entry)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
        }
    }

    /// Map a visual cell range to a source byte range.
    ///
    /// Returns `(byte_start, byte_end)` covering all clusters that overlap
    /// the given cell range. Continuation cells are resolved to their
    /// owning cluster.
    pub fn cell_range_to_byte_range(&self, cell_start: usize, cell_end: usize) -> (usize, usize) {
        if self.entries.is_empty() || cell_start >= cell_end {
            return (0, 0);
        }

        let start_byte = self.cell_to_byte(cell_start);

        let end_byte = if cell_end >= self.total_cells as usize {
            self.total_bytes as usize
        } else {
            // Find the cluster containing the last included cell and use its
            // byte_end as the exclusive bound. This ensures wide characters
            // partially covered by the cell range are fully included.
            match self.cell_to_entry(cell_end.saturating_sub(1)) {
                Some(entry) => entry.byte_end as usize,
                None => self.total_bytes as usize,
            }
        };

        (start_byte, end_byte.max(start_byte))
    }

    // -----------------------------------------------------------------------
    // Grapheme-level accessors
    // -----------------------------------------------------------------------

    /// Map a grapheme index to a visual cell column.
    pub fn grapheme_to_cell(&self, grapheme_index: usize) -> usize {
        self.entries
            .get(grapheme_index)
            .map_or(self.total_cells as usize, |e| e.cell_start as usize)
    }

    /// Map a visual cell column to a grapheme index.
    pub fn cell_to_grapheme(&self, cell_col: usize) -> usize {
        self.cell_to_entry(cell_col)
            .map_or(self.entries.len(), |e| e.grapheme_index as usize)
    }

    /// Map a grapheme index to a byte offset.
    pub fn grapheme_to_byte(&self, grapheme_index: usize) -> usize {
        self.entries
            .get(grapheme_index)
            .map_or(self.total_bytes as usize, |e| e.byte_start as usize)
    }

    /// Map a byte offset to a grapheme index.
    pub fn byte_to_grapheme(&self, byte_offset: usize) -> usize {
        self.byte_to_entry(byte_offset)
            .map_or(self.entries.len(), |e| e.grapheme_index as usize)
    }

    // -----------------------------------------------------------------------
    // Aggregate accessors
    // -----------------------------------------------------------------------

    /// Total visual width in cells.
    #[inline]
    pub fn total_cells(&self) -> usize {
        self.total_cells as usize
    }

    /// Total byte length of the source text.
    #[inline]
    pub fn total_bytes(&self) -> usize {
        self.total_bytes as usize
    }

    /// Number of grapheme clusters.
    #[inline]
    pub fn cluster_count(&self) -> usize {
        self.entries.len()
    }

    /// Whether the map is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over all cluster entries.
    #[inline]
    pub fn entries(&self) -> &[ClusterEntry] {
        &self.entries
    }

    /// Get the cluster entry at a grapheme index.
    #[inline]
    pub fn get(&self, grapheme_index: usize) -> Option<&ClusterEntry> {
        self.entries.get(grapheme_index)
    }

    /// Extract text from the source string for a cell range.
    ///
    /// Returns the substring covering all clusters that overlap the
    /// given visual cell range.
    pub fn extract_text_for_cells<'a>(
        &self,
        source: &'a str,
        cell_start: usize,
        cell_end: usize,
    ) -> &'a str {
        let (byte_start, byte_end) = self.cell_range_to_byte_range(cell_start, cell_end);
        if byte_start >= source.len() {
            return "";
        }
        let end = byte_end.min(source.len());
        &source[byte_start..end]
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Construction tests
    // -----------------------------------------------------------------------

    #[test]
    fn empty_text() {
        let map = ClusterMap::from_text("");
        assert!(map.is_empty());
        assert_eq!(map.total_cells(), 0);
        assert_eq!(map.total_bytes(), 0);
        assert_eq!(map.cluster_count(), 0);
    }

    #[test]
    fn ascii_text() {
        let map = ClusterMap::from_text("Hello");
        assert_eq!(map.cluster_count(), 5);
        assert_eq!(map.total_cells(), 5);
        assert_eq!(map.total_bytes(), 5);

        // Each ASCII char is 1 byte, 1 cell.
        for i in 0..5 {
            let e = map.get(i).unwrap();
            assert_eq!(e.byte_start, i as u32);
            assert_eq!(e.byte_end, (i + 1) as u32);
            assert_eq!(e.cell_start, i as u32);
            assert_eq!(e.cell_width, 1);
        }
    }

    #[test]
    fn wide_chars() {
        // "世界" — 2 CJK chars, each 3 bytes and 2 cells wide
        let text = "\u{4E16}\u{754C}";
        let map = ClusterMap::from_text(text);

        assert_eq!(map.cluster_count(), 2);
        assert_eq!(map.total_bytes(), 6);
        assert_eq!(map.total_cells(), 4);

        let e0 = map.get(0).unwrap();
        assert_eq!(e0.byte_start, 0);
        assert_eq!(e0.byte_end, 3);
        assert_eq!(e0.cell_start, 0);
        assert_eq!(e0.cell_width, 2);

        let e1 = map.get(1).unwrap();
        assert_eq!(e1.byte_start, 3);
        assert_eq!(e1.byte_end, 6);
        assert_eq!(e1.cell_start, 2);
        assert_eq!(e1.cell_width, 2);
    }

    #[test]
    fn mixed_ascii_and_wide() {
        // "Hi世界!" — 2 ASCII + 2 CJK + 1 ASCII
        let text = "Hi\u{4E16}\u{754C}!";
        let map = ClusterMap::from_text(text);

        assert_eq!(map.cluster_count(), 5);
        assert_eq!(map.total_bytes(), 9); // 2 + 3 + 3 + 1
        assert_eq!(map.total_cells(), 7); // 1+1+2+2+1

        // Verify cell starts.
        assert_eq!(map.get(0).unwrap().cell_start, 0); // 'H'
        assert_eq!(map.get(1).unwrap().cell_start, 1); // 'i'
        assert_eq!(map.get(2).unwrap().cell_start, 2); // '世'
        assert_eq!(map.get(3).unwrap().cell_start, 4); // '界'
        assert_eq!(map.get(4).unwrap().cell_start, 6); // '!'
    }

    #[test]
    fn combining_marks() {
        // "é" as e + combining acute: single grapheme, 2 bytes, 1 cell
        let text = "e\u{0301}";
        let map = ClusterMap::from_text(text);

        assert_eq!(map.cluster_count(), 1);
        assert_eq!(map.total_bytes(), 3); // 'e' (1) + U+0301 (2)
        assert_eq!(map.total_cells(), 1);

        let e = map.get(0).unwrap();
        assert_eq!(e.byte_start, 0);
        assert_eq!(e.byte_end, 3);
        assert_eq!(e.cell_width, 1);
    }

    // -----------------------------------------------------------------------
    // Forward lookup tests (byte → cell)
    // -----------------------------------------------------------------------

    #[test]
    fn byte_to_cell_ascii() {
        let map = ClusterMap::from_text("Hello");
        for i in 0..5 {
            assert_eq!(map.byte_to_cell(i), i);
        }
        assert_eq!(map.byte_to_cell(5), 5); // past end
    }

    #[test]
    fn byte_to_cell_wide() {
        let text = "Hi\u{4E16}\u{754C}!";
        let map = ClusterMap::from_text(text);

        assert_eq!(map.byte_to_cell(0), 0); // 'H'
        assert_eq!(map.byte_to_cell(1), 1); // 'i'
        assert_eq!(map.byte_to_cell(2), 2); // '世' start
        assert_eq!(map.byte_to_cell(5), 4); // '界' start
        assert_eq!(map.byte_to_cell(8), 6); // '!'
    }

    #[test]
    fn byte_to_cell_mid_cluster_snaps() {
        let text = "\u{4E16}"; // '世' is 3 bytes
        let map = ClusterMap::from_text(text);

        // Mid-byte offsets snap to cluster start.
        assert_eq!(map.byte_to_cell(0), 0);
        assert_eq!(map.byte_to_cell(1), 0); // mid-cluster → cluster start
        assert_eq!(map.byte_to_cell(2), 0); // mid-cluster → cluster start
    }

    #[test]
    fn byte_to_entry() {
        let text = "AB\u{4E16}C";
        let map = ClusterMap::from_text(text);

        let e = map.byte_to_entry(0).unwrap();
        assert_eq!(e.byte_start, 0); // 'A'

        let e = map.byte_to_entry(2).unwrap();
        assert_eq!(e.byte_start, 2); // '世'

        // Mid-cluster lookup.
        let e = map.byte_to_entry(3).unwrap();
        assert_eq!(e.byte_start, 2); // still '世'

        assert!(map.byte_to_entry(100).is_none());
    }

    // -----------------------------------------------------------------------
    // Reverse lookup tests (cell → byte)
    // -----------------------------------------------------------------------

    #[test]
    fn cell_to_byte_ascii() {
        let map = ClusterMap::from_text("Hello");
        for i in 0..5 {
            assert_eq!(map.cell_to_byte(i), i);
        }
        assert_eq!(map.cell_to_byte(5), 5); // past end
    }

    #[test]
    fn cell_to_byte_wide() {
        let text = "Hi\u{4E16}\u{754C}!";
        let map = ClusterMap::from_text(text);

        assert_eq!(map.cell_to_byte(0), 0); // 'H'
        assert_eq!(map.cell_to_byte(1), 1); // 'i'
        assert_eq!(map.cell_to_byte(2), 2); // '世'
        assert_eq!(map.cell_to_byte(3), 2); // continuation → same '世'
        assert_eq!(map.cell_to_byte(4), 5); // '界'
        assert_eq!(map.cell_to_byte(5), 5); // continuation → same '界'
        assert_eq!(map.cell_to_byte(6), 8); // '!'
    }

    #[test]
    fn cell_to_entry_continuation() {
        let text = "\u{4E16}"; // '世' — 2 cells
        let map = ClusterMap::from_text(text);

        // Both cells map to the same entry.
        let e0 = map.cell_to_entry(0).unwrap();
        let e1 = map.cell_to_entry(1).unwrap();
        assert_eq!(e0, e1);
        assert_eq!(e0.byte_start, 0);
        assert_eq!(e0.cell_width, 2);
    }

    // -----------------------------------------------------------------------
    // Range conversion tests
    // -----------------------------------------------------------------------

    #[test]
    fn byte_range_to_cell_range_ascii() {
        let map = ClusterMap::from_text("Hello World");
        assert_eq!(map.byte_range_to_cell_range(0, 5), (0, 5)); // "Hello"
        assert_eq!(map.byte_range_to_cell_range(6, 11), (6, 11)); // "World"
    }

    #[test]
    fn byte_range_to_cell_range_wide() {
        let text = "Hi\u{4E16}\u{754C}!"; // cells: H(0) i(1) 世(2,3) 界(4,5) !(6)
        let map = ClusterMap::from_text(text);

        // Byte range covering '世界' (bytes 2..8)
        assert_eq!(map.byte_range_to_cell_range(2, 8), (2, 6));
    }

    #[test]
    fn cell_range_to_byte_range_ascii() {
        let map = ClusterMap::from_text("Hello World");
        assert_eq!(map.cell_range_to_byte_range(0, 5), (0, 5));
    }

    #[test]
    fn cell_range_to_byte_range_wide() {
        let text = "Hi\u{4E16}\u{754C}!";
        let map = ClusterMap::from_text(text);

        // Cell range [2, 6) covers 世界
        assert_eq!(map.cell_range_to_byte_range(2, 6), (2, 8));

        // Cell range [3, 5) starts on continuation → snaps to 世, ends including 界
        assert_eq!(map.cell_range_to_byte_range(3, 5), (2, 8));
    }

    // -----------------------------------------------------------------------
    // Grapheme-level accessors
    // -----------------------------------------------------------------------

    #[test]
    fn grapheme_to_cell_and_back() {
        let text = "Hi\u{4E16}\u{754C}!";
        let map = ClusterMap::from_text(text);

        assert_eq!(map.grapheme_to_cell(0), 0); // 'H'
        assert_eq!(map.grapheme_to_cell(2), 2); // '世'
        assert_eq!(map.grapheme_to_cell(4), 6); // '!'
        assert_eq!(map.grapheme_to_cell(5), 7); // past end

        assert_eq!(map.cell_to_grapheme(0), 0); // 'H'
        assert_eq!(map.cell_to_grapheme(2), 2); // '世'
        assert_eq!(map.cell_to_grapheme(3), 2); // continuation → '世'
    }

    #[test]
    fn grapheme_to_byte_and_back() {
        let text = "A\u{4E16}B";
        let map = ClusterMap::from_text(text);

        assert_eq!(map.grapheme_to_byte(0), 0); // 'A'
        assert_eq!(map.grapheme_to_byte(1), 1); // '世'
        assert_eq!(map.grapheme_to_byte(2), 4); // 'B'

        assert_eq!(map.byte_to_grapheme(0), 0); // 'A'
        assert_eq!(map.byte_to_grapheme(1), 1); // '世'
        assert_eq!(map.byte_to_grapheme(4), 2); // 'B'
    }

    // -----------------------------------------------------------------------
    // Extract text
    // -----------------------------------------------------------------------

    #[test]
    fn extract_text_for_cells_ascii() {
        let text = "Hello World";
        let map = ClusterMap::from_text(text);
        assert_eq!(map.extract_text_for_cells(text, 0, 5), "Hello");
        assert_eq!(map.extract_text_for_cells(text, 6, 11), "World");
    }

    #[test]
    fn extract_text_for_cells_wide() {
        let text = "Hi\u{4E16}\u{754C}!";
        let map = ClusterMap::from_text(text);

        // Extract just the CJK chars (cells 2..6).
        assert_eq!(map.extract_text_for_cells(text, 2, 6), "\u{4E16}\u{754C}");

        // Extract including continuation cell.
        assert_eq!(map.extract_text_for_cells(text, 3, 5), "\u{4E16}\u{754C}");
    }

    #[test]
    fn extract_text_empty_range() {
        let text = "Hello";
        let map = ClusterMap::from_text(text);
        assert_eq!(map.extract_text_for_cells(text, 3, 3), "");
    }

    // -----------------------------------------------------------------------
    // Invariant: round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn roundtrip_byte_cell_byte() {
        let texts = [
            "Hello",
            "\u{4E16}\u{754C}",
            "Hi\u{4E16}\u{754C}!",
            "e\u{0301}f",
            "\u{05E9}\u{05DC}\u{05D5}\u{05DD}",
            "",
        ];

        for text in texts {
            let map = ClusterMap::from_text(text);

            for entry in map.entries() {
                let byte = entry.byte_start as usize;
                let cell = map.byte_to_cell(byte);
                let back = map.cell_to_byte(cell);
                assert_eq!(
                    back, byte,
                    "Round-trip failed for text={text:?} byte={byte}"
                );
            }
        }
    }

    #[test]
    fn roundtrip_cell_byte_cell() {
        let texts = [
            "Hello",
            "\u{4E16}\u{754C}",
            "Hi\u{4E16}\u{754C}!",
            "e\u{0301}f",
        ];

        for text in texts {
            let map = ClusterMap::from_text(text);

            for entry in map.entries() {
                let cell = entry.cell_start as usize;
                let byte = map.cell_to_byte(cell);
                let back = map.byte_to_cell(byte);
                assert_eq!(
                    back, cell,
                    "Round-trip failed for text={text:?} cell={cell}"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Invariant: monotonicity
    // -----------------------------------------------------------------------

    #[test]
    fn monotonicity() {
        let texts = [
            "Hello World",
            "Hi\u{4E16}\u{754C}! \u{05E9}\u{05DC}\u{05D5}\u{05DD}",
            "e\u{0301}\u{0302}",
        ];

        for text in texts {
            let map = ClusterMap::from_text(text);

            for window in map.entries().windows(2) {
                assert!(
                    window[0].byte_start < window[1].byte_start,
                    "Byte monotonicity violated: {:?}",
                    window
                );
                assert!(
                    window[0].cell_start < window[1].cell_start,
                    "Cell monotonicity violated: {:?}",
                    window
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Invariant: contiguity
    // -----------------------------------------------------------------------

    #[test]
    fn contiguity() {
        let text = "Hi\u{4E16}\u{754C}!";
        let map = ClusterMap::from_text(text);

        // Byte ranges are contiguous.
        for window in map.entries().windows(2) {
            assert_eq!(
                window[0].byte_end, window[1].byte_start,
                "Byte gap: {:?}",
                window
            );
        }

        // Cell ranges are contiguous.
        for window in map.entries().windows(2) {
            assert_eq!(
                window[0].cell_end(),
                window[1].cell_start,
                "Cell gap: {:?}",
                window
            );
        }

        // First entry starts at 0.
        assert_eq!(map.entries()[0].byte_start, 0);
        assert_eq!(map.entries()[0].cell_start, 0);

        // Last entry ends at total.
        let last = map.entries().last().unwrap();
        assert_eq!(last.byte_end, map.total_bytes() as u32);
        assert_eq!(last.cell_end(), map.total_cells() as u32);
    }

    // -----------------------------------------------------------------------
    // Shaped run integration
    // -----------------------------------------------------------------------

    #[test]
    fn from_shaped_run_noop() {
        use crate::script_segmentation::{RunDirection, Script};
        use crate::shaping::{FontFeatures, NoopShaper, TextShaper};

        let text = "Hi\u{4E16}!";
        let shaper = NoopShaper;
        let ff = FontFeatures::default();
        let run = shaper.shape(text, Script::Latin, RunDirection::Ltr, &ff);

        let map = ClusterMap::from_shaped_run(text, &run);

        // Same result as from_text for NoopShaper.
        let text_map = ClusterMap::from_text(text);

        assert_eq!(map.cluster_count(), text_map.cluster_count());
        assert_eq!(map.total_cells(), text_map.total_cells());
        assert_eq!(map.total_bytes(), text_map.total_bytes());
    }

    #[test]
    fn from_shaped_run_empty() {
        use crate::shaping::ShapedRun;

        let map = ClusterMap::from_shaped_run(
            "",
            &ShapedRun {
                glyphs: vec![],
                total_advance: 0,
            },
        );
        assert!(map.is_empty());
    }
}
