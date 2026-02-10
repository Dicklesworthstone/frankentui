#![forbid(unsafe_code)]

//! Columns widget: lays out children side-by-side using Flex constraints.

use crate::Widget;
use ftui_core::geometry::{Rect, Sides};
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;

/// A single column definition.
pub struct Column<'a> {
    widget: Box<dyn Widget + 'a>,
    constraint: Constraint,
    padding: Sides,
}

impl std::fmt::Debug for Column<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Column")
            .field("widget", &"<dyn Widget>")
            .field("constraint", &self.constraint)
            .field("padding", &self.padding)
            .finish()
    }
}

impl<'a> Column<'a> {
    /// Create a new column with a widget and constraint.
    pub fn new(widget: impl Widget + 'a, constraint: Constraint) -> Self {
        Self {
            widget: Box::new(widget),
            constraint,
            padding: Sides::default(),
        }
    }

    /// Set the column padding.
    #[must_use]
    pub fn padding(mut self, padding: Sides) -> Self {
        self.padding = padding;
        self
    }

    /// Set the column constraint.
    #[must_use]
    pub fn constraint(mut self, constraint: Constraint) -> Self {
        self.constraint = constraint;
        self
    }
}

/// A horizontal column layout container.
#[derive(Debug, Default)]
pub struct Columns<'a> {
    columns: Vec<Column<'a>>,
    gap: u16,
}

impl<'a> Columns<'a> {
    /// Create an empty columns container.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the gap between columns.
    #[must_use]
    pub fn gap(mut self, gap: u16) -> Self {
        self.gap = gap;
        self
    }

    /// Add a column definition.
    #[must_use]
    pub fn push(mut self, column: Column<'a>) -> Self {
        self.columns.push(column);
        self
    }

    /// Add a column with a widget and constraint.
    #[must_use]
    pub fn column(mut self, widget: impl Widget + 'a, constraint: Constraint) -> Self {
        self.columns.push(Column::new(widget, constraint));
        self
    }

    /// Add a column with equal ratio sizing.
    #[must_use]
    #[allow(clippy::should_implement_trait)] // Builder pattern, not std::ops::Add
    pub fn add(mut self, widget: impl Widget + 'a) -> Self {
        self.columns
            .push(Column::new(widget, Constraint::Ratio(1, 1)));
        self
    }
}

struct ScissorGuard<'a, 'pool> {
    frame: &'a mut Frame<'pool>,
}

impl<'a, 'pool> ScissorGuard<'a, 'pool> {
    fn new(frame: &'a mut Frame<'pool>, rect: Rect) -> Self {
        frame.buffer.push_scissor(rect);
        Self { frame }
    }

    fn frame_mut(&mut self) -> &mut Frame<'pool> {
        self.frame
    }
}

impl Drop for ScissorGuard<'_, '_> {
    fn drop(&mut self) {
        self.frame.buffer.pop_scissor();
    }
}

impl Widget for Columns<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "Columns",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        if area.is_empty() || self.columns.is_empty() {
            return;
        }

        if !frame.buffer.degradation.render_content() {
            return;
        }

        let flex = Flex::horizontal()
            .gap(self.gap)
            .constraints(self.columns.iter().map(|c| c.constraint));
        let rects = flex.split(area);

        for (col, rect) in self.columns.iter().zip(rects) {
            if rect.is_empty() {
                continue;
            }
            let inner = rect.inner(col.padding);
            if inner.is_empty() {
                continue;
            }

            let mut guard = ScissorGuard::new(frame, inner);
            col.widget.render(inner, guard.frame_mut());
        }
    }

    fn is_essential(&self) -> bool {
        self.columns.iter().any(|c| c.widget.is_essential())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Clone, Debug)]
    struct Record {
        rects: Rc<RefCell<Vec<Rect>>>,
    }

    impl Record {
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

    impl Widget for Record {
        fn render(&self, area: Rect, _frame: &mut Frame) {
            self.rects.borrow_mut().push(area);
        }
    }

    #[test]
    fn equal_columns_split_evenly() {
        let (a, a_rects) = Record::new();
        let (b, b_rects) = Record::new();
        let (c, c_rects) = Record::new();

        let columns = Columns::new().add(a).add(b).add(c).gap(0);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(12, 2, &mut pool);
        columns.render(Rect::new(0, 0, 12, 2), &mut frame);

        let a = a_rects.borrow()[0];
        let b = b_rects.borrow()[0];
        let c = c_rects.borrow()[0];

        assert_eq!(a, Rect::new(0, 0, 4, 2));
        assert_eq!(b, Rect::new(4, 0, 4, 2));
        assert_eq!(c, Rect::new(8, 0, 4, 2));
    }

    #[test]
    fn fixed_columns_with_gap() {
        let (a, a_rects) = Record::new();
        let (b, b_rects) = Record::new();

        let columns = Columns::new()
            .column(a, Constraint::Fixed(4))
            .column(b, Constraint::Fixed(4))
            .gap(2);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        columns.render(Rect::new(0, 0, 20, 1), &mut frame);

        let a = a_rects.borrow()[0];
        let b = b_rects.borrow()[0];

        assert_eq!(a, Rect::new(0, 0, 4, 1));
        assert_eq!(b, Rect::new(6, 0, 4, 1));
    }

    #[test]
    fn ratio_columns_split_proportionally() {
        let (a, a_rects) = Record::new();
        let (b, b_rects) = Record::new();

        let columns = Columns::new()
            .column(a, Constraint::Ratio(1, 3))
            .column(b, Constraint::Ratio(2, 3));

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 1, &mut pool);
        columns.render(Rect::new(0, 0, 30, 1), &mut frame);

        let a = a_rects.borrow()[0];
        let b = b_rects.borrow()[0];

        assert_eq!(a.width + b.width, 30);
        assert_eq!(a.width, 10);
        assert_eq!(b.width, 20);
    }

    #[test]
    fn column_padding_applies_to_child_area() {
        let (a, a_rects) = Record::new();
        let columns =
            Columns::new().push(Column::new(a, Constraint::Fixed(6)).padding(Sides::all(1)));

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(6, 3, &mut pool);
        columns.render(Rect::new(0, 0, 6, 3), &mut frame);

        let rect = a_rects.borrow()[0];
        assert_eq!(rect, Rect::new(1, 1, 4, 1));
    }

    #[test]
    fn empty_columns_does_not_panic() {
        let columns = Columns::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        columns.render(Rect::new(0, 0, 10, 5), &mut frame);
    }

    #[test]
    fn zero_area_does_not_panic() {
        let (a, a_rects) = Record::new();
        let columns = Columns::new().add(a);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        columns.render(Rect::new(0, 0, 0, 0), &mut frame);
        assert!(a_rects.borrow().is_empty());
    }

    #[test]
    fn single_column_gets_full_width() {
        let (a, a_rects) = Record::new();
        let columns = Columns::new().column(a, Constraint::Min(0));

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 3, &mut pool);
        columns.render(Rect::new(0, 0, 20, 3), &mut frame);

        let rect = a_rects.borrow()[0];
        assert_eq!(rect.width, 20);
        assert_eq!(rect.height, 3);
    }

    #[test]
    fn fixed_and_fill_columns() {
        let (a, a_rects) = Record::new();
        let (b, b_rects) = Record::new();

        let columns = Columns::new()
            .column(a, Constraint::Fixed(5))
            .column(b, Constraint::Min(0));

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        columns.render(Rect::new(0, 0, 20, 1), &mut frame);

        let a = a_rects.borrow()[0];
        let b = b_rects.borrow()[0];
        assert_eq!(a.width, 5);
        assert_eq!(b.width, 15);
    }

    #[test]
    fn is_essential_delegates_to_children() {
        struct Essential;
        impl Widget for Essential {
            fn render(&self, _area: Rect, _frame: &mut Frame) {}
            fn is_essential(&self) -> bool {
                true
            }
        }

        let columns = Columns::new().add(Essential);
        assert!(columns.is_essential());

        let (non_essential, _) = Record::new();
        let columns2 = Columns::new().add(non_essential);
        assert!(!columns2.is_essential());
    }

    #[test]
    fn column_constraint_setter() {
        let (a, _) = Record::new();
        let col = Column::new(a, Constraint::Fixed(5)).constraint(Constraint::Fixed(10));
        assert_eq!(col.constraint, Constraint::Fixed(10));
    }

    #[test]
    fn all_columns_receive_same_height() {
        let (a, a_rects) = Record::new();
        let (b, b_rects) = Record::new();
        let (c, c_rects) = Record::new();

        let columns = Columns::new().add(a).add(b).add(c);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(12, 5, &mut pool);
        columns.render(Rect::new(0, 0, 12, 5), &mut frame);

        let a = a_rects.borrow()[0];
        let b = b_rects.borrow()[0];
        let c = c_rects.borrow()[0];

        assert_eq!(a.height, 5);
        assert_eq!(b.height, 5);
        assert_eq!(c.height, 5);
    }

    #[test]
    fn column_debug_format() {
        let (a, _) = Record::new();
        let col = Column::new(a, Constraint::Fixed(5));
        let dbg = format!("{:?}", col);
        assert!(dbg.contains("Column"));
        assert!(dbg.contains("<dyn Widget>"));
    }

    #[test]
    fn columns_default_is_empty() {
        let cols = Columns::default();
        assert!(cols.columns.is_empty());
        assert_eq!(cols.gap, 0);
    }

    #[test]
    fn column_builder_chain() {
        let (a, _) = Record::new();
        let col = Column::new(a, Constraint::Fixed(5))
            .padding(Sides::all(2))
            .constraint(Constraint::Ratio(1, 3));
        assert_eq!(col.constraint, Constraint::Ratio(1, 3));
        assert_eq!(col.padding, Sides::all(2));
    }

    #[test]
    fn many_columns_with_gap() {
        let mut rects_all = Vec::new();
        let mut cols = Columns::new().gap(1);
        for _ in 0..5 {
            let (rec, rects) = Record::new();
            rects_all.push(rects);
            cols = cols.column(rec, Constraint::Fixed(2));
        }

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        cols.render(Rect::new(0, 0, 20, 1), &mut frame);

        // 5 fixed cols of width 2 with gap 1 between them
        for (i, rects) in rects_all.iter().enumerate() {
            let r = rects.borrow()[0];
            assert_eq!(r.width, 2, "column {i} should be width 2");
        }

        // Ensure no overlap
        for i in 0..4 {
            let a = rects_all[i].borrow()[0];
            let b = rects_all[i + 1].borrow()[0];
            assert!(
                b.x >= a.right(),
                "column {} (right={}) overlaps column {} (x={})",
                i,
                a.right(),
                i + 1,
                b.x
            );
        }
    }
}
