#![forbid(unsafe_code)]

//! 2D Grid layout system for dashboard-style positioning.
//!
//! Grid provides constraint-based 2D positioning with support for:
//! - Row and column constraints
//! - Cell spanning (colspan, rowspan)
//! - Named areas for semantic layout references
//! - Gap configuration
//!
//! # Example
//!
//! ```
//! use ftui_layout::grid::Grid;
//! use ftui_layout::Constraint;
//! use ftui_core::geometry::Rect;
//!
//! // Create a 3x2 grid (3 rows, 2 columns)
//! let grid = Grid::new()
//!     .rows([
//!         Constraint::Fixed(3),      // Header
//!         Constraint::Min(10),       // Content
//!         Constraint::Fixed(1),      // Footer
//!     ])
//!     .columns([
//!         Constraint::Percentage(30.0),  // Sidebar
//!         Constraint::Min(20),            // Main
//!     ])
//!     .row_gap(1)
//!     .col_gap(2);
//!
//! let area = Rect::new(0, 0, 80, 24);
//! let layout = grid.split(area);
//!
//! // Access cell by (row, col)
//! let header_left = layout.cell(0, 0);
//! let content_main = layout.cell(1, 1);
//! ```

use crate::Constraint;
use ftui_core::geometry::Rect;
use std::collections::HashMap;

/// A 2D grid layout container.
#[derive(Debug, Clone, Default)]
pub struct Grid {
    /// Row constraints (height of each row).
    row_constraints: Vec<Constraint>,
    /// Column constraints (width of each column).
    col_constraints: Vec<Constraint>,
    /// Gap between rows.
    row_gap: u16,
    /// Gap between columns.
    col_gap: u16,
    /// Named areas mapping to (row, col, rowspan, colspan).
    named_areas: HashMap<String, GridArea>,
}

/// Definition of a named grid area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridArea {
    /// Starting row (0-indexed).
    pub row: usize,
    /// Starting column (0-indexed).
    pub col: usize,
    /// Number of rows this area spans.
    pub rowspan: usize,
    /// Number of columns this area spans.
    pub colspan: usize,
}

impl GridArea {
    /// Create a single-cell area.
    pub fn cell(row: usize, col: usize) -> Self {
        Self {
            row,
            col,
            rowspan: 1,
            colspan: 1,
        }
    }

    /// Create a spanning area.
    pub fn span(row: usize, col: usize, rowspan: usize, colspan: usize) -> Self {
        Self {
            row,
            col,
            rowspan: rowspan.max(1),
            colspan: colspan.max(1),
        }
    }
}

/// Result of solving a grid layout.
#[derive(Debug, Clone)]
pub struct GridLayout {
    /// Row heights.
    row_heights: Vec<u16>,
    /// Column widths.
    col_widths: Vec<u16>,
    /// Row Y positions (cumulative with gaps).
    row_positions: Vec<u16>,
    /// Column X positions (cumulative with gaps).
    col_positions: Vec<u16>,
    /// Named areas from the grid definition.
    named_areas: HashMap<String, GridArea>,
}

impl Grid {
    /// Create a new empty grid.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the row constraints.
    pub fn rows(mut self, constraints: impl IntoIterator<Item = Constraint>) -> Self {
        self.row_constraints = constraints.into_iter().collect();
        self
    }

    /// Set the column constraints.
    pub fn columns(mut self, constraints: impl IntoIterator<Item = Constraint>) -> Self {
        self.col_constraints = constraints.into_iter().collect();
        self
    }

    /// Set the gap between rows.
    pub fn row_gap(mut self, gap: u16) -> Self {
        self.row_gap = gap;
        self
    }

    /// Set the gap between columns.
    pub fn col_gap(mut self, gap: u16) -> Self {
        self.col_gap = gap;
        self
    }

    /// Set uniform gap for both rows and columns.
    pub fn gap(self, gap: u16) -> Self {
        self.row_gap(gap).col_gap(gap)
    }

    /// Define a named area in the grid.
    ///
    /// Named areas allow semantic access to grid regions:
    /// ```ignore
    /// let grid = Grid::new()
    ///     .rows([Constraint::Fixed(3), Constraint::Min(10)])
    ///     .columns([Constraint::Fixed(20), Constraint::Min(40)])
    ///     .area("sidebar", GridArea::span(0, 0, 2, 1))  // Left column, both rows
    ///     .area("content", GridArea::cell(0, 1))        // Top right
    ///     .area("footer", GridArea::cell(1, 1));        // Bottom right
    /// ```
    pub fn area(mut self, name: impl Into<String>, area: GridArea) -> Self {
        self.named_areas.insert(name.into(), area);
        self
    }

    /// Get the number of rows.
    pub fn num_rows(&self) -> usize {
        self.row_constraints.len()
    }

    /// Get the number of columns.
    pub fn num_cols(&self) -> usize {
        self.col_constraints.len()
    }

    /// Split the given area according to the grid configuration.
    pub fn split(&self, area: Rect) -> GridLayout {
        let num_rows = self.row_constraints.len();
        let num_cols = self.col_constraints.len();

        if num_rows == 0 || num_cols == 0 || area.is_empty() {
            return GridLayout {
                row_heights: vec![0; num_rows],
                col_widths: vec![0; num_cols],
                row_positions: vec![area.y; num_rows],
                col_positions: vec![area.x; num_cols],
                named_areas: self.named_areas.clone(),
            };
        }

        // Calculate total gaps
        let total_row_gap = if num_rows > 1 {
            self.row_gap.saturating_mul((num_rows - 1) as u16)
        } else {
            0
        };
        let total_col_gap = if num_cols > 1 {
            self.col_gap.saturating_mul((num_cols - 1) as u16)
        } else {
            0
        };

        // Available space after gaps
        let available_height = area.height.saturating_sub(total_row_gap);
        let available_width = area.width.saturating_sub(total_col_gap);

        // Solve constraints
        let row_heights = crate::solve_constraints(&self.row_constraints, available_height);
        let col_widths = crate::solve_constraints(&self.col_constraints, available_width);

        // Calculate positions
        let row_positions = self.calculate_positions(&row_heights, area.y, self.row_gap);
        let col_positions = self.calculate_positions(&col_widths, area.x, self.col_gap);

        GridLayout {
            row_heights,
            col_widths,
            row_positions,
            col_positions,
            named_areas: self.named_areas.clone(),
        }
    }

    /// Calculate cumulative positions from sizes.
    fn calculate_positions(&self, sizes: &[u16], start: u16, gap: u16) -> Vec<u16> {
        let mut positions = Vec::with_capacity(sizes.len());
        let mut pos = start;

        for (i, &size) in sizes.iter().enumerate() {
            positions.push(pos);
            pos = pos.saturating_add(size);
            if i < sizes.len() - 1 {
                pos = pos.saturating_add(gap);
            }
        }

        positions
    }
}

impl GridLayout {
    /// Get the rectangle for a specific cell.
    ///
    /// Returns an empty Rect if coordinates are out of bounds.
    pub fn cell(&self, row: usize, col: usize) -> Rect {
        self.span(row, col, 1, 1)
    }

    /// Get the rectangle for a spanning region.
    ///
    /// The region starts at (row, col) and spans rowspan rows and colspan columns.
    pub fn span(&self, row: usize, col: usize, rowspan: usize, colspan: usize) -> Rect {
        let rowspan = rowspan.max(1);
        let colspan = colspan.max(1);

        // Bounds check
        if row >= self.row_heights.len() || col >= self.col_widths.len() {
            return Rect::default();
        }

        let end_row = (row + rowspan).min(self.row_heights.len());
        let end_col = (col + colspan).min(self.col_widths.len());

        // Get starting position
        let x = self.col_positions[col];
        let y = self.row_positions[row];

        // Calculate total width (sum of widths + gaps between spanned columns)
        let mut width: u16 = 0;
        for c in col..end_col {
            width = width.saturating_add(self.col_widths[c]);
        }
        // Add gaps between columns (not after last)
        if end_col > col + 1 {
            let gap_count = (end_col - col - 1) as u16;
            let col_gap = if let Some(&last_pos) = self.col_positions.get(1) {
                let first_width = self.col_widths.first().copied().unwrap_or(0);
                last_pos
                    .saturating_sub(self.col_positions[0])
                    .saturating_sub(first_width)
            } else {
                0
            };
            width = width.saturating_add(col_gap.saturating_mul(gap_count));
        }

        // Calculate total height (sum of heights + gaps between spanned rows)
        let mut height: u16 = 0;
        for r in row..end_row {
            height = height.saturating_add(self.row_heights[r]);
        }
        if end_row > row + 1 {
            let gap_count = (end_row - row - 1) as u16;
            let row_gap = if let Some(&last_pos) = self.row_positions.get(1) {
                let first_height = self.row_heights.first().copied().unwrap_or(0);
                last_pos
                    .saturating_sub(self.row_positions[0])
                    .saturating_sub(first_height)
            } else {
                0
            };
            height = height.saturating_add(row_gap.saturating_mul(gap_count));
        }

        Rect::new(x, y, width, height)
    }

    /// Get the rectangle for a named area.
    ///
    /// Returns None if the area name is not defined.
    pub fn area(&self, name: &str) -> Option<Rect> {
        self.named_areas
            .get(name)
            .map(|a| self.span(a.row, a.col, a.rowspan, a.colspan))
    }

    /// Get the number of rows in this layout.
    pub fn num_rows(&self) -> usize {
        self.row_heights.len()
    }

    /// Get the number of columns in this layout.
    pub fn num_cols(&self) -> usize {
        self.col_widths.len()
    }

    /// Get the height of a specific row.
    pub fn row_height(&self, row: usize) -> u16 {
        self.row_heights.get(row).copied().unwrap_or(0)
    }

    /// Get the width of a specific column.
    pub fn col_width(&self, col: usize) -> u16 {
        self.col_widths.get(col).copied().unwrap_or(0)
    }

    /// Iterate over all cells, yielding (row, col, Rect).
    pub fn iter_cells(&self) -> impl Iterator<Item = (usize, usize, Rect)> + '_ {
        let num_rows = self.num_rows();
        let num_cols = self.num_cols();
        (0..num_rows)
            .flat_map(move |row| (0..num_cols).map(move |col| (row, col, self.cell(row, col))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_grid() {
        let grid = Grid::new();
        let layout = grid.split(Rect::new(0, 0, 100, 50));
        assert_eq!(layout.num_rows(), 0);
        assert_eq!(layout.num_cols(), 0);
    }

    #[test]
    fn simple_2x2_grid() {
        let grid = Grid::new()
            .rows([Constraint::Fixed(10), Constraint::Fixed(10)])
            .columns([Constraint::Fixed(20), Constraint::Fixed(20)]);

        let layout = grid.split(Rect::new(0, 0, 100, 50));

        assert_eq!(layout.num_rows(), 2);
        assert_eq!(layout.num_cols(), 2);

        // Check each cell
        assert_eq!(layout.cell(0, 0), Rect::new(0, 0, 20, 10));
        assert_eq!(layout.cell(0, 1), Rect::new(20, 0, 20, 10));
        assert_eq!(layout.cell(1, 0), Rect::new(0, 10, 20, 10));
        assert_eq!(layout.cell(1, 1), Rect::new(20, 10, 20, 10));
    }

    #[test]
    fn grid_with_gaps() {
        let grid = Grid::new()
            .rows([Constraint::Fixed(10), Constraint::Fixed(10)])
            .columns([Constraint::Fixed(20), Constraint::Fixed(20)])
            .row_gap(2)
            .col_gap(5);

        let layout = grid.split(Rect::new(0, 0, 100, 50));

        // First row, first col
        assert_eq!(layout.cell(0, 0), Rect::new(0, 0, 20, 10));
        // First row, second col (after col_gap of 5)
        assert_eq!(layout.cell(0, 1), Rect::new(25, 0, 20, 10));
        // Second row, first col (after row_gap of 2)
        assert_eq!(layout.cell(1, 0), Rect::new(0, 12, 20, 10));
        // Second row, second col
        assert_eq!(layout.cell(1, 1), Rect::new(25, 12, 20, 10));
    }

    #[test]
    fn percentage_constraints() {
        let grid = Grid::new()
            .rows([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .columns([Constraint::Percentage(30.0), Constraint::Percentage(70.0)]);

        let layout = grid.split(Rect::new(0, 0, 100, 50));

        assert_eq!(layout.row_height(0), 25);
        assert_eq!(layout.row_height(1), 25);
        assert_eq!(layout.col_width(0), 30);
        assert_eq!(layout.col_width(1), 70);
    }

    #[test]
    fn min_constraints_fill_space() {
        let grid = Grid::new()
            .rows([Constraint::Fixed(10), Constraint::Min(5)])
            .columns([Constraint::Fixed(20), Constraint::Min(10)]);

        let layout = grid.split(Rect::new(0, 0, 100, 50));

        // Min should expand to fill remaining space
        assert_eq!(layout.row_height(0), 10);
        assert_eq!(layout.row_height(1), 40); // 50 - 10 = 40
        assert_eq!(layout.col_width(0), 20);
        assert_eq!(layout.col_width(1), 80); // 100 - 20 = 80
    }

    #[test]
    fn cell_spanning() {
        let grid = Grid::new()
            .rows([
                Constraint::Fixed(10),
                Constraint::Fixed(10),
                Constraint::Fixed(10),
            ])
            .columns([
                Constraint::Fixed(20),
                Constraint::Fixed(20),
                Constraint::Fixed(20),
            ]);

        let layout = grid.split(Rect::new(0, 0, 100, 50));

        // Single cell
        assert_eq!(layout.span(0, 0, 1, 1), Rect::new(0, 0, 20, 10));

        // Horizontal span (2 columns)
        assert_eq!(layout.span(0, 0, 1, 2), Rect::new(0, 0, 40, 10));

        // Vertical span (2 rows)
        assert_eq!(layout.span(0, 0, 2, 1), Rect::new(0, 0, 20, 20));

        // 2x2 block
        assert_eq!(layout.span(0, 0, 2, 2), Rect::new(0, 0, 40, 20));
    }

    #[test]
    fn cell_spanning_with_gaps() {
        let grid = Grid::new()
            .rows([Constraint::Fixed(10), Constraint::Fixed(10)])
            .columns([Constraint::Fixed(20), Constraint::Fixed(20)])
            .row_gap(2)
            .col_gap(5);

        let layout = grid.split(Rect::new(0, 0, 100, 50));

        // 2x2 span should include the gaps
        let full = layout.span(0, 0, 2, 2);
        // Width: 20 + 5 (gap) + 20 = 45
        // Height: 10 + 2 (gap) + 10 = 22
        assert_eq!(full.width, 45);
        assert_eq!(full.height, 22);
    }

    #[test]
    fn named_areas() {
        let grid = Grid::new()
            .rows([
                Constraint::Fixed(5),
                Constraint::Min(10),
                Constraint::Fixed(3),
            ])
            .columns([Constraint::Fixed(20), Constraint::Min(30)])
            .area("header", GridArea::span(0, 0, 1, 2))
            .area("sidebar", GridArea::span(1, 0, 2, 1))
            .area("content", GridArea::cell(1, 1))
            .area("footer", GridArea::cell(2, 1));

        let layout = grid.split(Rect::new(0, 0, 80, 30));

        // Header spans both columns
        let header = layout.area("header").unwrap();
        assert_eq!(header.y, 0);
        assert_eq!(header.height, 5);

        // Sidebar spans rows 1 and 2
        let sidebar = layout.area("sidebar").unwrap();
        assert_eq!(sidebar.x, 0);
        assert_eq!(sidebar.width, 20);

        // Content is in the middle
        let content = layout.area("content").unwrap();
        assert_eq!(content.x, 20);
        assert_eq!(content.y, 5);

        // Footer is at the bottom right
        let footer = layout.area("footer").unwrap();
        assert_eq!(
            footer.y,
            layout.area("content").unwrap().y + layout.area("content").unwrap().height
        );
    }

    #[test]
    fn out_of_bounds_returns_empty() {
        let grid = Grid::new()
            .rows([Constraint::Fixed(10)])
            .columns([Constraint::Fixed(20)]);

        let layout = grid.split(Rect::new(0, 0, 100, 50));

        // Out of bounds
        assert_eq!(layout.cell(5, 5), Rect::default());
        assert_eq!(layout.cell(0, 5), Rect::default());
        assert_eq!(layout.cell(5, 0), Rect::default());
    }

    #[test]
    fn iter_cells() {
        let grid = Grid::new()
            .rows([Constraint::Fixed(10), Constraint::Fixed(10)])
            .columns([Constraint::Fixed(20), Constraint::Fixed(20)]);

        let layout = grid.split(Rect::new(0, 0, 100, 50));

        let cells: Vec<_> = layout.iter_cells().collect();
        assert_eq!(cells.len(), 4);
        assert_eq!(cells[0], (0, 0, Rect::new(0, 0, 20, 10)));
        assert_eq!(cells[1], (0, 1, Rect::new(20, 0, 20, 10)));
        assert_eq!(cells[2], (1, 0, Rect::new(0, 10, 20, 10)));
        assert_eq!(cells[3], (1, 1, Rect::new(20, 10, 20, 10)));
    }

    #[test]
    fn undefined_area_returns_none() {
        let grid = Grid::new()
            .rows([Constraint::Fixed(10)])
            .columns([Constraint::Fixed(20)]);

        let layout = grid.split(Rect::new(0, 0, 100, 50));

        assert!(layout.area("nonexistent").is_none());
    }

    #[test]
    fn empty_area_produces_empty_cells() {
        let grid = Grid::new()
            .rows([Constraint::Fixed(10)])
            .columns([Constraint::Fixed(20)]);

        let layout = grid.split(Rect::new(0, 0, 0, 0));

        assert_eq!(layout.cell(0, 0), Rect::new(0, 0, 0, 0));
    }

    #[test]
    fn offset_area() {
        let grid = Grid::new()
            .rows([Constraint::Fixed(10)])
            .columns([Constraint::Fixed(20)]);

        let layout = grid.split(Rect::new(10, 5, 100, 50));

        // Cell should be offset by the area origin
        assert_eq!(layout.cell(0, 0), Rect::new(10, 5, 20, 10));
    }

    #[test]
    fn ratio_constraints() {
        let grid = Grid::new()
            .rows([Constraint::Ratio(1, 3), Constraint::Ratio(2, 3)])
            .columns([Constraint::Fixed(30)]);

        let layout = grid.split(Rect::new(0, 0, 30, 30));

        // 1:2 ratio should give roughly 10:20 split
        assert_eq!(layout.row_height(0), 10);
        assert_eq!(layout.row_height(1), 20);
    }

    #[test]
    fn max_constraints() {
        // Test that Max(N) clamps the size to at most N
        let grid = Grid::new()
            .rows([Constraint::Max(5), Constraint::Fixed(20)])
            .columns([Constraint::Fixed(30)]);

        let layout = grid.split(Rect::new(0, 0, 30, 30));

        // Max(5) should get at most 5, but the remaining 5 (from 30-20=10 available)
        // is distributed to Max, giving 5 which is then clamped to 5
        assert!(layout.row_height(0) <= 5);
        // Fixed gets its exact size
        assert_eq!(layout.row_height(1), 20);
    }

    // --- Additional Grid tests ---

    #[test]
    fn uniform_gap_sets_both() {
        let grid = Grid::new()
            .rows([Constraint::Fixed(10), Constraint::Fixed(10)])
            .columns([Constraint::Fixed(20), Constraint::Fixed(20)])
            .gap(3);

        let layout = grid.split(Rect::new(0, 0, 100, 50));

        // Both row_gap and col_gap should be 3
        assert_eq!(layout.cell(0, 1).x, 23); // 20 + 3
        assert_eq!(layout.cell(1, 0).y, 13); // 10 + 3
    }

    #[test]
    fn grid_area_cell_is_1x1_span() {
        let a = GridArea::cell(2, 3);
        assert_eq!(a.row, 2);
        assert_eq!(a.col, 3);
        assert_eq!(a.rowspan, 1);
        assert_eq!(a.colspan, 1);
    }

    #[test]
    fn grid_area_span_clamps_zero() {
        // Zero spans should be clamped to 1
        let a = GridArea::span(0, 0, 0, 0);
        assert_eq!(a.rowspan, 1);
        assert_eq!(a.colspan, 1);
    }

    #[test]
    fn grid_num_rows_cols() {
        let grid = Grid::new()
            .rows([
                Constraint::Fixed(5),
                Constraint::Fixed(5),
                Constraint::Fixed(5),
            ])
            .columns([Constraint::Fixed(10), Constraint::Fixed(10)]);
        assert_eq!(grid.num_rows(), 3);
        assert_eq!(grid.num_cols(), 2);
    }

    #[test]
    fn grid_row_height_col_width_out_of_bounds() {
        let grid = Grid::new()
            .rows([Constraint::Fixed(10)])
            .columns([Constraint::Fixed(20)]);
        let layout = grid.split(Rect::new(0, 0, 100, 50));
        assert_eq!(layout.row_height(0), 10);
        assert_eq!(layout.row_height(99), 0); // Out of bounds returns 0
        assert_eq!(layout.col_width(0), 20);
        assert_eq!(layout.col_width(99), 0); // Out of bounds returns 0
    }

    #[test]
    fn grid_span_clamped_to_bounds() {
        let grid = Grid::new()
            .rows([Constraint::Fixed(10)])
            .columns([Constraint::Fixed(20)]);
        let layout = grid.split(Rect::new(0, 0, 100, 50));

        // Spanning beyond grid dimensions should clamp
        let r = layout.span(0, 0, 5, 5);
        // Should get the single cell (1x1 grid)
        assert_eq!(r, Rect::new(0, 0, 20, 10));
    }

    #[test]
    fn grid_with_all_constraint_types() {
        let grid = Grid::new()
            .rows([
                Constraint::Fixed(5),
                Constraint::Percentage(20.0),
                Constraint::Min(3),
                Constraint::Max(10),
                Constraint::Ratio(1, 4),
            ])
            .columns([Constraint::Fixed(30)]);

        let layout = grid.split(Rect::new(0, 0, 30, 50));

        // All rows should have non-negative heights
        let total: u16 = (0..layout.num_rows()).map(|r| layout.row_height(r)).sum();
        assert!(total <= 50);
    }

    // Property-like invariant tests
    #[test]
    fn invariant_total_size_within_bounds() {
        for (width, height) in [(50, 30), (100, 50), (80, 24)] {
            let grid = Grid::new()
                .rows([
                    Constraint::Fixed(10),
                    Constraint::Min(5),
                    Constraint::Percentage(20.0),
                ])
                .columns([
                    Constraint::Fixed(15),
                    Constraint::Min(10),
                    Constraint::Ratio(1, 2),
                ]);

            let layout = grid.split(Rect::new(0, 0, width, height));

            let total_height: u16 = (0..layout.num_rows()).map(|r| layout.row_height(r)).sum();
            let total_width: u16 = (0..layout.num_cols()).map(|c| layout.col_width(c)).sum();

            assert!(
                total_height <= height,
                "Total height {} exceeds available {}",
                total_height,
                height
            );
            assert!(
                total_width <= width,
                "Total width {} exceeds available {}",
                total_width,
                width
            );
        }
    }

    #[test]
    fn invariant_cells_within_area() {
        let area = Rect::new(10, 20, 80, 60);
        let grid = Grid::new()
            .rows([
                Constraint::Fixed(15),
                Constraint::Min(10),
                Constraint::Fixed(15),
            ])
            .columns([
                Constraint::Fixed(20),
                Constraint::Min(20),
                Constraint::Fixed(20),
            ])
            .row_gap(2)
            .col_gap(3);

        let layout = grid.split(area);

        for (row, col, cell) in layout.iter_cells() {
            assert!(
                cell.x >= area.x,
                "Cell ({},{}) x {} < area x {}",
                row,
                col,
                cell.x,
                area.x
            );
            assert!(
                cell.y >= area.y,
                "Cell ({},{}) y {} < area y {}",
                row,
                col,
                cell.y,
                area.y
            );
            assert!(
                cell.right() <= area.right(),
                "Cell ({},{}) right {} > area right {}",
                row,
                col,
                cell.right(),
                area.right()
            );
            assert!(
                cell.bottom() <= area.bottom(),
                "Cell ({},{}) bottom {} > area bottom {}",
                row,
                col,
                cell.bottom(),
                area.bottom()
            );
        }
    }
}
