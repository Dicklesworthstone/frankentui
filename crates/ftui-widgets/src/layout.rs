#![forbid(unsafe_code)]

//! Layout composition widget.
//!
//! A 2D grid-based layout container that places child widgets using
//! [`Grid`] constraints. Each child is assigned to a grid cell or span,
//! and the grid solver computes the final placement rects.
//!
//! This widget is glue over `ftui_layout::Grid` — it does not implement
//! a parallel constraint solver.
//!
//! # Example
//!
//! ```ignore
//! use ftui_widgets::layout::Layout;
//! use ftui_layout::Constraint;
//!
//! let layout = Layout::new()
//!     .rows([Constraint::Fixed(1), Constraint::Min(0), Constraint::Fixed(1)])
//!     .columns([Constraint::Fixed(20), Constraint::Min(0)])
//!     .child(header_widget, 0, 0, 1, 2)  // row 0, col 0, span 1x2
//!     .child(sidebar_widget, 1, 0, 1, 1)
//!     .child(content_widget, 1, 1, 1, 1)
//!     .child(footer_widget, 2, 0, 1, 2);
//! ```

use crate::Widget;
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Grid};
use ftui_render::frame::Frame;

/// A child entry in the layout grid.
pub struct LayoutChild<'a> {
    widget: Box<dyn Widget + 'a>,
    row: usize,
    col: usize,
    rowspan: usize,
    colspan: usize,
}

impl std::fmt::Debug for LayoutChild<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayoutChild")
            .field("row", &self.row)
            .field("col", &self.col)
            .field("rowspan", &self.rowspan)
            .field("colspan", &self.colspan)
            .finish()
    }
}

/// A 2D grid-based layout container.
///
/// Children are placed at grid coordinates with optional spanning.
/// The grid solver distributes space according to row/column constraints.
#[derive(Debug)]
pub struct Layout<'a> {
    children: Vec<LayoutChild<'a>>,
    row_constraints: Vec<Constraint>,
    col_constraints: Vec<Constraint>,
    row_gap: u16,
    col_gap: u16,
}

impl Default for Layout<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> Layout<'a> {
    /// Create a new empty layout.
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            row_constraints: Vec::new(),
            col_constraints: Vec::new(),
            row_gap: 0,
            col_gap: 0,
        }
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
    pub fn gap(mut self, gap: u16) -> Self {
        self.row_gap = gap;
        self.col_gap = gap;
        self
    }

    /// Add a child widget at a specific grid position with spanning.
    pub fn child(
        mut self,
        widget: impl Widget + 'a,
        row: usize,
        col: usize,
        rowspan: usize,
        colspan: usize,
    ) -> Self {
        self.children.push(LayoutChild {
            widget: Box::new(widget),
            row,
            col,
            rowspan: rowspan.max(1),
            colspan: colspan.max(1),
        });
        self
    }

    /// Add a child widget at a single grid cell (1x1).
    pub fn cell(self, widget: impl Widget + 'a, row: usize, col: usize) -> Self {
        self.child(widget, row, col, 1, 1)
    }

    /// Number of children.
    pub fn len(&self) -> usize {
        self.children.len()
    }

    /// Whether the layout has no children.
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }
}

impl Widget for Layout<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() || self.children.is_empty() {
            return;
        }

        let grid = Grid::new()
            .rows(self.row_constraints.iter().copied())
            .columns(self.col_constraints.iter().copied())
            .row_gap(self.row_gap)
            .col_gap(self.col_gap);

        let grid_layout = grid.split(area);

        for child in &self.children {
            let rect = grid_layout.span(child.row, child.col, child.rowspan, child.colspan);
            if !rect.is_empty() {
                child.widget.render(rect, frame);
            }
        }
    }

    fn is_essential(&self) -> bool {
        self.children.iter().any(|c| c.widget.is_essential())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::Cell;
    use ftui_render::grapheme_pool::GraphemePool;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn buf_to_lines(buf: &ftui_render::buffer::Buffer) -> Vec<String> {
        let mut lines = Vec::new();
        for y in 0..buf.height() {
            let mut row = String::with_capacity(buf.width() as usize);
            for x in 0..buf.width() {
                let ch = buf
                    .get(x, y)
                    .and_then(|c| c.content.as_char())
                    .unwrap_or(' ');
                row.push(ch);
            }
            lines.push(row);
        }
        lines
    }

    #[derive(Debug, Clone, Copy)]
    struct Fill(char);

    impl Widget for Fill {
        fn render(&self, area: Rect, frame: &mut Frame) {
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    frame.buffer.set(x, y, Cell::from_char(self.0));
                }
            }
        }
    }

    /// Records the rect it receives during render.
    #[derive(Clone, Debug)]
    struct Recorder {
        rects: Rc<RefCell<Vec<Rect>>>,
    }

    impl Recorder {
        fn new() -> (Self, Rc<RefCell<Vec<Rect>>>) {
            let rects = Rc::new(RefCell::new(Vec::new()));
            (
                Self {
                    rects: rects.clone(),
                },
                rects,
            )
        }
    }

    impl Widget for Recorder {
        fn render(&self, area: Rect, _frame: &mut Frame) {
            self.rects.borrow_mut().push(area);
        }
    }

    #[test]
    fn empty_layout_is_noop() {
        let layout = Layout::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        layout.render(Rect::new(0, 0, 10, 10), &mut frame);

        for y in 0..10 {
            for x in 0..10u16 {
                assert!(frame.buffer.get(x, y).unwrap().is_empty());
            }
        }
    }

    #[test]
    fn single_cell_layout() {
        let layout = Layout::new()
            .rows([Constraint::Min(0)])
            .columns([Constraint::Min(0)])
            .cell(Fill('X'), 0, 0);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        layout.render(Rect::new(0, 0, 5, 3), &mut frame);

        assert_eq!(buf_to_lines(&frame.buffer), vec!["XXXXX", "XXXXX", "XXXXX"]);
    }

    #[test]
    fn two_by_two_grid() {
        let layout = Layout::new()
            .rows([Constraint::Fixed(1), Constraint::Fixed(1)])
            .columns([Constraint::Fixed(3), Constraint::Fixed(3)])
            .cell(Fill('A'), 0, 0)
            .cell(Fill('B'), 0, 1)
            .cell(Fill('C'), 1, 0)
            .cell(Fill('D'), 1, 1);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(6, 2, &mut pool);
        layout.render(Rect::new(0, 0, 6, 2), &mut frame);

        assert_eq!(buf_to_lines(&frame.buffer), vec!["AAABBB", "CCCDDD"]);
    }

    #[test]
    fn column_spanning() {
        let layout = Layout::new()
            .rows([Constraint::Fixed(1), Constraint::Fixed(1)])
            .columns([Constraint::Fixed(3), Constraint::Fixed(3)])
            .child(Fill('H'), 0, 0, 1, 2) // span both columns
            .cell(Fill('L'), 1, 0)
            .cell(Fill('R'), 1, 1);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(6, 2, &mut pool);
        layout.render(Rect::new(0, 0, 6, 2), &mut frame);

        assert_eq!(buf_to_lines(&frame.buffer), vec!["HHHHHH", "LLLRRR"]);
    }

    #[test]
    fn row_spanning() {
        let layout = Layout::new()
            .rows([Constraint::Fixed(1), Constraint::Fixed(1)])
            .columns([Constraint::Fixed(2), Constraint::Fixed(2)])
            .child(Fill('S'), 0, 0, 2, 1) // span both rows
            .cell(Fill('A'), 0, 1)
            .cell(Fill('B'), 1, 1);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 2, &mut pool);
        layout.render(Rect::new(0, 0, 4, 2), &mut frame);

        assert_eq!(buf_to_lines(&frame.buffer), vec!["SSAA", "SSBB"]);
    }

    #[test]
    fn layout_with_gap() {
        let (a, a_rects) = Recorder::new();
        let (b, b_rects) = Recorder::new();

        let layout = Layout::new()
            .rows([Constraint::Fixed(1)])
            .columns([Constraint::Fixed(3), Constraint::Fixed(3)])
            .col_gap(2)
            .cell(a, 0, 0)
            .cell(b, 0, 1);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        layout.render(Rect::new(0, 0, 10, 1), &mut frame);

        let a_rect = a_rects.borrow()[0];
        let b_rect = b_rects.borrow()[0];

        assert_eq!(a_rect.width, 3);
        assert_eq!(b_rect.width, 3);
        // Gap of 2 between columns
        assert!(b_rect.x >= a_rect.right());
    }

    #[test]
    fn fixed_and_flexible_rows() {
        let (header, header_rects) = Recorder::new();
        let (content, content_rects) = Recorder::new();
        let (footer, footer_rects) = Recorder::new();

        let layout = Layout::new()
            .rows([
                Constraint::Fixed(1),
                Constraint::Min(0),
                Constraint::Fixed(1),
            ])
            .columns([Constraint::Min(0)])
            .cell(header, 0, 0)
            .cell(content, 1, 0)
            .cell(footer, 2, 0);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        layout.render(Rect::new(0, 0, 20, 10), &mut frame);

        let h = header_rects.borrow()[0];
        let c = content_rects.borrow()[0];
        let f = footer_rects.borrow()[0];

        assert_eq!(h.height, 1);
        assert_eq!(f.height, 1);
        assert_eq!(c.height, 8); // 10 - 1 (header) - 1 (footer)
        assert_eq!(h.y, 0);
        assert_eq!(f.y, 9);
    }

    #[test]
    fn zero_area_is_noop() {
        let (rec, rects) = Recorder::new();
        let layout = Layout::new()
            .rows([Constraint::Min(0)])
            .columns([Constraint::Min(0)])
            .cell(rec, 0, 0);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 5, &mut pool);
        layout.render(Rect::new(0, 0, 0, 0), &mut frame);

        assert!(rects.borrow().is_empty());
    }

    #[test]
    fn len_and_is_empty() {
        assert!(Layout::new().is_empty());
        assert_eq!(Layout::new().len(), 0);

        let layout = Layout::new()
            .rows([Constraint::Min(0)])
            .columns([Constraint::Min(0)])
            .cell(Fill('X'), 0, 0);
        assert!(!layout.is_empty());
        assert_eq!(layout.len(), 1);
    }

    #[test]
    fn is_essential_delegates() {
        struct Essential;
        impl Widget for Essential {
            fn render(&self, _: Rect, _: &mut Frame) {}
            fn is_essential(&self) -> bool {
                true
            }
        }

        let not_essential = Layout::new()
            .rows([Constraint::Min(0)])
            .columns([Constraint::Min(0)])
            .cell(Fill('X'), 0, 0);
        assert!(!not_essential.is_essential());

        let essential = Layout::new()
            .rows([Constraint::Min(0)])
            .columns([Constraint::Min(0)])
            .cell(Essential, 0, 0);
        assert!(essential.is_essential());
    }

    #[test]
    fn deterministic_render_order() {
        // Later children overwrite earlier ones when placed in the same cell
        let layout = Layout::new()
            .rows([Constraint::Fixed(1)])
            .columns([Constraint::Fixed(3)])
            .cell(Fill('A'), 0, 0)
            .cell(Fill('B'), 0, 0); // same cell, overwrites A

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 1, &mut pool);
        layout.render(Rect::new(0, 0, 3, 1), &mut frame);

        assert_eq!(buf_to_lines(&frame.buffer), vec!["BBB"]);
    }

    #[test]
    fn layout_with_offset_area() {
        let (rec, rects) = Recorder::new();
        let layout = Layout::new()
            .rows([Constraint::Fixed(2)])
            .columns([Constraint::Fixed(3)])
            .cell(rec, 0, 0);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        layout.render(Rect::new(3, 4, 5, 5), &mut frame);

        let r = rects.borrow()[0];
        assert_eq!(r.x, 3);
        assert_eq!(r.y, 4);
        assert_eq!(r.width, 3);
        assert_eq!(r.height, 2);
    }

    #[test]
    fn three_by_three_grid() {
        let layout = Layout::new()
            .rows([
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Fixed(1),
            ])
            .columns([
                Constraint::Fixed(2),
                Constraint::Fixed(2),
                Constraint::Fixed(2),
            ])
            .cell(Fill('1'), 0, 0)
            .cell(Fill('2'), 0, 1)
            .cell(Fill('3'), 0, 2)
            .cell(Fill('4'), 1, 0)
            .cell(Fill('5'), 1, 1)
            .cell(Fill('6'), 1, 2)
            .cell(Fill('7'), 2, 0)
            .cell(Fill('8'), 2, 1)
            .cell(Fill('9'), 2, 2);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(6, 3, &mut pool);
        layout.render(Rect::new(0, 0, 6, 3), &mut frame);

        assert_eq!(
            buf_to_lines(&frame.buffer),
            vec!["112233", "445566", "778899"]
        );
    }

    #[test]
    fn layout_default_equals_new() {
        let def: Layout<'_> = Layout::default();
        assert!(def.is_empty());
        assert_eq!(def.len(), 0);
    }

    #[test]
    fn gap_sets_both_row_and_col() {
        let (a, a_rects) = Recorder::new();
        let (b, b_rects) = Recorder::new();

        let layout = Layout::new()
            .rows([Constraint::Fixed(2), Constraint::Fixed(2)])
            .columns([Constraint::Fixed(3)])
            .gap(1)
            .cell(a, 0, 0)
            .cell(b, 1, 0);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        layout.render(Rect::new(0, 0, 10, 10), &mut frame);

        let a_rect = a_rects.borrow()[0];
        let b_rect = b_rects.borrow()[0];
        // Gap of 1 between rows
        assert!(b_rect.y >= a_rect.bottom());
    }

    #[test]
    fn child_clamps_zero_span_to_one() {
        let (rec, rects) = Recorder::new();
        let layout = Layout::new()
            .rows([Constraint::Fixed(3)])
            .columns([Constraint::Fixed(4)])
            .child(rec, 0, 0, 0, 0); // both spans 0 -> clamped to 1

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        layout.render(Rect::new(0, 0, 10, 10), &mut frame);

        let r = rects.borrow()[0];
        assert!(r.width > 0 && r.height > 0);
    }

    #[test]
    fn layout_child_debug() {
        let layout = Layout::new()
            .rows([Constraint::Fixed(1)])
            .columns([Constraint::Fixed(1)])
            .child(Fill('X'), 2, 3, 4, 5);

        let dbg = format!("{:?}", layout);
        assert!(dbg.contains("Layout"));
        assert!(dbg.contains("LayoutChild"));
    }
}
