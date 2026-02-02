use crate::block::Block;
use crate::{StatefulWidget, Widget, set_style_area};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::{Frame, HitId, HitRegion};
use ftui_style::Style;
use ftui_text::Text;

/// A row in a table.
#[derive(Debug, Clone, Default)]
pub struct Row {
    cells: Vec<Text>,
    height: u16,
    style: Style,
    bottom_margin: u16,
}

impl Row {
    pub fn new(cells: impl IntoIterator<Item = impl Into<Text>>) -> Self {
        Self {
            cells: cells.into_iter().map(|c| c.into()).collect(),
            height: 1,
            style: Style::default(),
            bottom_margin: 0,
        }
    }

    pub fn height(mut self, height: u16) -> Self {
        self.height = height;
        self
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn bottom_margin(mut self, margin: u16) -> Self {
        self.bottom_margin = margin;
        self
    }
}

/// A widget to display data in a table.
#[derive(Debug, Clone, Default)]
pub struct Table<'a> {
    rows: Vec<Row>,
    widths: Vec<Constraint>,
    header: Option<Row>,
    block: Option<Block<'a>>,
    style: Style,
    highlight_style: Style,
    column_spacing: u16,
    /// Optional hit ID for mouse interaction.
    /// When set, each table row registers a hit region with the hit grid.
    hit_id: Option<HitId>,
}

impl<'a> Table<'a> {
    pub fn new(
        rows: impl IntoIterator<Item = Row>,
        widths: impl IntoIterator<Item = Constraint>,
    ) -> Self {
        Self {
            rows: rows.into_iter().collect(),
            widths: widths.into_iter().collect(),
            header: None,
            block: None,
            style: Style::default(),
            highlight_style: Style::default(),
            column_spacing: 1,
            hit_id: None,
        }
    }

    pub fn header(mut self, header: Row) -> Self {
        self.header = Some(header);
        self
    }

    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn highlight_style(mut self, style: Style) -> Self {
        self.highlight_style = style;
        self
    }

    pub fn column_spacing(mut self, spacing: u16) -> Self {
        self.column_spacing = spacing;
        self
    }

    /// Set a hit ID for mouse interaction.
    ///
    /// When set, each table row will register a hit region with the frame's
    /// hit grid (if enabled). The hit data will be the row's index, allowing
    /// click handlers to determine which row was clicked.
    pub fn hit_id(mut self, id: HitId) -> Self {
        self.hit_id = Some(id);
        self
    }
}

impl<'a> Widget for Table<'a> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        let mut state = TableState::default();
        StatefulWidget::render(self, area, frame, &mut state);
    }
}

#[derive(Debug, Clone, Default)]
pub struct TableState {
    pub selected: Option<usize>,
    pub offset: usize,
}

impl TableState {
    pub fn select(&mut self, index: Option<usize>) {
        self.selected = index;
        if index.is_none() {
            self.offset = 0;
        }
    }
}

impl<'a> StatefulWidget for Table<'a> {
    type State = TableState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "Table",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        if area.is_empty() {
            return;
        }

        // Render block if present
        let table_area = match &self.block {
            Some(b) => {
                b.render(area, frame);
                b.inner(area)
            }
            None => area,
        };

        if table_area.is_empty() {
            return;
        }

        let deg = frame.degradation;

        // Apply base style to the entire table area (clears gaps/empty space)
        if deg.apply_styling() {
            set_style_area(&mut frame.buffer, table_area, self.style);
        }

        let header_height = self
            .header
            .as_ref()
            .map(|h| h.height + h.bottom_margin)
            .unwrap_or(0);

        if let Some(header) = &self.header
            && header.height > table_area.height
        {
            return;
        }

        let rows_top = table_area.y.saturating_add(header_height);
        let rows_max_y = table_area.bottom();
        let rows_height = rows_max_y.saturating_sub(rows_top);

        if let Some(selected) = state.selected {
            if self.rows.is_empty() {
                state.selected = None;
            } else if selected >= self.rows.len() {
                state.selected = Some(self.rows.len() - 1);
            }
        }

        // Ensure visible range includes selected item
        if let Some(selected) = state.selected {
            if selected < state.offset {
                state.offset = selected;
            } else {
                // Check if selected is visible; if not, scroll down
                // 1. Find the index of the last currently visible row
                let mut current_y = rows_top;
                let max_y = rows_max_y;
                let mut last_visible = state.offset;

                // Iterate forward to find visibility boundary
                for (i, row) in self.rows.iter().enumerate().skip(state.offset) {
                    if current_y + row.height > max_y {
                        break;
                    }
                    current_y += row.height + row.bottom_margin;
                    last_visible = i;
                }

                if selected > last_visible {
                    // Selected is below viewport. Find new offset to make it visible at bottom.
                    let mut new_offset = selected;
                    let mut accumulated_height: u16 = 0;
                    let available_height = rows_height;

                    // Iterate backwards from selected
                    for i in (0..=selected).rev() {
                        let row = &self.rows[i];
                        let total_row_height = row.height + row.bottom_margin;

                        if accumulated_height + row.height > available_height {
                            // This row doesn't fit fully.
                            // If it's the selected row itself, we must show it (at top).
                            // Otherwise, the *next* row (i+1) is our start.
                            if i < selected {
                                new_offset = i + 1;
                            } else {
                                new_offset = selected;
                            }
                            break;
                        }

                        accumulated_height += total_row_height;
                        new_offset = i;
                    }
                    state.offset = new_offset;
                }
            }
        }

        // Calculate column widths
        let flex = Flex::horizontal()
            .constraints(self.widths.clone())
            .gap(self.column_spacing);

        // We need a dummy rect with correct width to solve horizontal constraints
        let column_rects = flex.split(Rect::new(table_area.x, table_area.y, table_area.width, 1));

        let mut y = table_area.y;
        let max_y = table_area.bottom();

        // Render header
        if let Some(header) = &self.header {
            if y + header.height > max_y {
                return;
            }
            let row_area = Rect::new(table_area.x, y, table_area.width, header.height);
            let header_style = if deg.apply_styling() {
                set_style_area(&mut frame.buffer, row_area, header.style);
                header.style
            } else {
                Style::default()
            };
            render_row(header, &column_rects, frame, y, header_style);
            y += header.height + header.bottom_margin;
        }

        // Render rows
        if self.rows.is_empty() {
            return;
        }

        // Handle scrolling/offset?
        // For v1 basic Table, we just render from state.offset

        for (i, row) in self.rows.iter().enumerate().skip(state.offset) {
            if y + row.height > max_y {
                break;
            }

            let is_selected = state.selected == Some(i);
            let row_area = Rect::new(table_area.x, y, table_area.width, row.height);
            let style = if deg.apply_styling() {
                let s = if is_selected {
                    self.highlight_style
                } else {
                    row.style
                };
                set_style_area(&mut frame.buffer, row_area, s);
                s
            } else {
                Style::default()
            };

            render_row(row, &column_rects, frame, y, style);

            // Register hit region for this row (if hit testing enabled)
            if let Some(id) = self.hit_id {
                frame.register_hit(row_area, id, HitRegion::Content, i as u64);
            }

            y += row.height + row.bottom_margin;
        }
    }
}

fn render_row(row: &Row, col_rects: &[Rect], frame: &mut Frame, y: u16, style: Style) {
    let apply_styling = frame.degradation.apply_styling();

    for (i, cell_text) in row.cells.iter().enumerate() {
        if i >= col_rects.len() {
            break;
        }
        let rect = col_rects[i];
        let cell_area = Rect::new(rect.x, y, rect.width, row.height);

        let styled_text = if apply_styling {
            cell_text.clone().with_base_style(style)
        } else {
            cell_text.clone()
        };

        for (line_idx, line) in styled_text.lines().iter().enumerate() {
            if line_idx as u16 >= row.height {
                break;
            }

            let mut x = cell_area.x;
            for span in line.spans() {
                // At NoStyling+, ignore span-level styles
                let span_style = if apply_styling {
                    span.style.unwrap_or(style)
                } else {
                    Style::default()
                };
                x = crate::draw_text_span_with_link(
                    frame,
                    x,
                    cell_area.y + line_idx as u16,
                    &span.content,
                    span_style,
                    cell_area.right(),
                    span.link.as_deref(),
                );
                if x >= cell_area.right() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::buffer::Buffer;
    use ftui_render::grapheme_pool::GraphemePool;

    fn cell_char(buf: &Buffer, x: u16, y: u16) -> Option<char> {
        buf.get(x, y).and_then(|c| c.content.as_char())
    }

    // --- Row builder tests ---

    #[test]
    fn row_new_from_strings() {
        let row = Row::new(["A", "B", "C"]);
        assert_eq!(row.cells.len(), 3);
        assert_eq!(row.height, 1);
        assert_eq!(row.bottom_margin, 0);
    }

    #[test]
    fn row_builder_methods() {
        let row = Row::new(["X"])
            .height(3)
            .bottom_margin(1)
            .style(Style::new().bold());
        assert_eq!(row.height, 3);
        assert_eq!(row.bottom_margin, 1);
        assert!(row.style.has_attr(ftui_style::StyleFlags::BOLD));
    }

    // --- TableState tests ---

    #[test]
    fn table_state_default() {
        let state = TableState::default();
        assert_eq!(state.selected, None);
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn table_state_select() {
        let mut state = TableState::default();
        state.select(Some(5));
        assert_eq!(state.selected, Some(5));
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn table_state_deselect_resets_offset() {
        let mut state = TableState {
            offset: 10,
            ..Default::default()
        };
        state.select(Some(3));
        assert_eq!(state.selected, Some(3));
        state.select(None);
        assert_eq!(state.selected, None);
        assert_eq!(state.offset, 0);
    }

    // --- Table rendering tests ---

    #[test]
    fn render_zero_area() {
        let table = Table::new([Row::new(["A"])], [Constraint::Fixed(5)]);
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        Widget::render(&table, area, &mut frame);
        // Should not panic
    }

    #[test]
    fn render_empty_rows() {
        let table = Table::new(Vec::<Row>::new(), [Constraint::Fixed(5)]);
        let area = Rect::new(0, 0, 10, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        Widget::render(&table, area, &mut frame);
        // Should not panic; no content rendered
    }

    #[test]
    fn render_single_row_single_column() {
        let table = Table::new([Row::new(["Hello"])], [Constraint::Fixed(10)]);
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);
        Widget::render(&table, area, &mut frame);

        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('H'));
        assert_eq!(cell_char(&frame.buffer, 1, 0), Some('e'));
        assert_eq!(cell_char(&frame.buffer, 4, 0), Some('o'));
    }

    #[test]
    fn render_multiple_rows() {
        let table = Table::new(
            [Row::new(["AA", "BB"]), Row::new(["CC", "DD"])],
            [Constraint::Fixed(4), Constraint::Fixed(4)],
        );
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);
        Widget::render(&table, area, &mut frame);

        // First row
        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('A'));
        // Second row
        assert_eq!(cell_char(&frame.buffer, 0, 1), Some('C'));
    }

    #[test]
    fn render_with_header() {
        let header = Row::new(["Name", "Val"]);
        let table = Table::new(
            [Row::new(["foo", "42"])],
            [Constraint::Fixed(5), Constraint::Fixed(4)],
        )
        .header(header);

        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);
        Widget::render(&table, area, &mut frame);

        // Header on row 0
        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('N'));
        // Data on row 1
        assert_eq!(cell_char(&frame.buffer, 0, 1), Some('f'));
    }

    #[test]
    fn render_with_block() {
        let table = Table::new([Row::new(["X"])], [Constraint::Fixed(5)]).block(Block::bordered());

        let area = Rect::new(0, 0, 10, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        Widget::render(&table, area, &mut frame);

        // Content should be inside the block border
        // Border chars are at row 0, content starts at row 1
        assert_eq!(cell_char(&frame.buffer, 1, 1), Some('X'));
    }

    #[test]
    fn stateful_render_with_selection() {
        let table = Table::new(
            [Row::new(["A"]), Row::new(["B"]), Row::new(["C"])],
            [Constraint::Fixed(5)],
        )
        .highlight_style(Style::new().bold());

        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        let mut state = TableState::default();
        state.select(Some(1));

        StatefulWidget::render(&table, area, &mut frame, &mut state);
        // Selected row should have the highlight style applied
        // Row 1 (index 1) should render "B"
        assert_eq!(cell_char(&frame.buffer, 0, 1), Some('B'));
    }

    #[test]
    fn selection_below_offset_adjusts_offset() {
        let mut state = TableState {
            offset: 5,
            selected: Some(2), // Selected is below offset
        };

        let table = Table::new(
            (0..10).map(|i| Row::new([format!("Row {i}")])),
            [Constraint::Fixed(10)],
        );
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);
        StatefulWidget::render(&table, area, &mut frame, &mut state);

        // Offset should have been adjusted down to selected
        assert_eq!(state.offset, 2);
    }

    #[test]
    fn selection_out_of_bounds_clamps_to_last_row() {
        let table = Table::new([Row::new(["A"]), Row::new(["B"])], [Constraint::Fixed(5)]);
        let area = Rect::new(0, 0, 5, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 2, &mut pool);
        let mut state = TableState {
            offset: 0,
            selected: Some(99),
        };

        StatefulWidget::render(&table, area, &mut frame, &mut state);
        assert_eq!(state.selected, Some(1));
    }

    #[test]
    fn selection_with_header_accounts_for_header_height() {
        let header = Row::new(["H"]);
        let table =
            Table::new([Row::new(["A"]), Row::new(["B"])], [Constraint::Fixed(5)]).header(header);

        let area = Rect::new(0, 0, 5, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 2, &mut pool);
        let mut state = TableState {
            offset: 0,
            selected: Some(1),
        };

        StatefulWidget::render(&table, area, &mut frame, &mut state);
        assert_eq!(state.offset, 1);
    }

    #[test]
    fn rows_overflow_area_truncated() {
        let table = Table::new(
            (0..20).map(|i| Row::new([format!("R{i}")])),
            [Constraint::Fixed(5)],
        );
        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        Widget::render(&table, area, &mut frame);

        // Only first 3 rows fit
        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('R'));
        assert_eq!(cell_char(&frame.buffer, 1, 0), Some('0'));
        assert_eq!(cell_char(&frame.buffer, 1, 2), Some('2'));
    }

    #[test]
    fn column_spacing_applied() {
        let table = Table::new(
            [Row::new(["A", "B"])],
            [Constraint::Fixed(3), Constraint::Fixed(3)],
        )
        .column_spacing(2);

        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        Widget::render(&table, area, &mut frame);

        // "A" starts at x=0, "B" starts at x=3+2=5 (column width + gap)
        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('A'));
    }

    #[test]
    fn more_cells_than_columns_truncated() {
        let table = Table::new(
            [Row::new(["A", "B", "C", "D"])],
            [Constraint::Fixed(3), Constraint::Fixed(3)],
        );
        let area = Rect::new(0, 0, 8, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(8, 1, &mut pool);
        Widget::render(&table, area, &mut frame);
        // Should not panic; extra cells beyond column count are skipped
    }

    #[test]
    fn header_too_tall_for_area() {
        let header = Row::new(["H"]).height(10);
        let table = Table::new([Row::new(["X"])], [Constraint::Fixed(5)]).header(header);

        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        Widget::render(&table, area, &mut frame);
        // Header doesn't fit; should return early without rendering data
    }

    #[test]
    fn row_with_bottom_margin() {
        let table = Table::new(
            [Row::new(["A"]).bottom_margin(1), Row::new(["B"])],
            [Constraint::Fixed(5)],
        );
        let area = Rect::new(0, 0, 5, 4);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 4, &mut pool);
        Widget::render(&table, area, &mut frame);

        // Row "A" at y=0, margin leaves y=1 empty, row "B" at y=2
        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('A'));
        assert_eq!(cell_char(&frame.buffer, 0, 2), Some('B'));
    }

    #[test]
    fn table_registers_hit_regions() {
        let table = Table::new(
            [Row::new(["A"]), Row::new(["B"]), Row::new(["C"])],
            [Constraint::Fixed(5)],
        )
        .hit_id(HitId::new(99));

        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(5, 3, &mut pool);
        let mut state = TableState::default();
        StatefulWidget::render(&table, area, &mut frame, &mut state);

        // Each row should have a hit region with the row index as data
        let hit0 = frame.hit_test(2, 0);
        let hit1 = frame.hit_test(2, 1);
        let hit2 = frame.hit_test(2, 2);

        assert_eq!(hit0, Some((HitId::new(99), HitRegion::Content, 0)));
        assert_eq!(hit1, Some((HitId::new(99), HitRegion::Content, 1)));
        assert_eq!(hit2, Some((HitId::new(99), HitRegion::Content, 2)));
    }

    #[test]
    fn table_no_hit_without_hit_id() {
        let table = Table::new([Row::new(["A"])], [Constraint::Fixed(5)]);
        let area = Rect::new(0, 0, 5, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(5, 1, &mut pool);
        let mut state = TableState::default();
        StatefulWidget::render(&table, area, &mut frame, &mut state);

        // No hit region should be registered
        assert!(frame.hit_test(2, 0).is_none());
    }

    #[test]
    fn table_no_hit_without_hit_grid() {
        let table = Table::new([Row::new(["A"])], [Constraint::Fixed(5)]).hit_id(HitId::new(1));
        let area = Rect::new(0, 0, 5, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool); // No hit grid
        let mut state = TableState::default();
        StatefulWidget::render(&table, area, &mut frame, &mut state);

        // hit_test returns None when no hit grid
        assert!(frame.hit_test(2, 0).is_none());
    }
}
