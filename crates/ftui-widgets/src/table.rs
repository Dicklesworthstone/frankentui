use crate::block::Block;
#[allow(unused_imports)]
use crate::undo_support::{TableUndoExt, UndoSupport, UndoWidgetId};
use crate::{MeasurableWidget, SizeConstraints, StatefulWidget, Widget, set_style_area};
use ftui_core::geometry::{Rect, Size};
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::{Frame, HitId, HitRegion};
use ftui_style::Style;
use ftui_text::Text;
#[allow(unused_imports)]
use std::any::Any;

/// A row in a table.
#[derive(Debug, Clone, Default)]
pub struct Row {
    cells: Vec<Text>,
    height: u16,
    style: Style,
    bottom_margin: u16,
}

impl Row {
    /// Create a new row from an iterator of cell contents.
    pub fn new(cells: impl IntoIterator<Item = impl Into<Text>>) -> Self {
        Self {
            cells: cells.into_iter().map(|c| c.into()).collect(),
            height: 1,
            style: Style::default(),
            bottom_margin: 0,
        }
    }

    /// Set the row height in lines.
    pub fn height(mut self, height: u16) -> Self {
        self.height = height;
        self
    }

    /// Set the row style.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set the bottom margin after this row.
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
    /// Create a new table with the given rows and column width constraints.
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

    /// Set the header row.
    pub fn header(mut self, header: Row) -> Self {
        self.header = Some(header);
        self
    }

    /// Set the surrounding block.
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// Set the base table style.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set the style for the selected row.
    pub fn highlight_style(mut self, style: Style) -> Self {
        self.highlight_style = style;
        self
    }

    /// Set the spacing between columns.
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

/// Mutable state for a [`Table`] widget.
#[derive(Debug, Clone, Default)]
pub struct TableState {
    /// Unique ID for undo tracking.
    #[allow(dead_code)]
    undo_id: UndoWidgetId,
    /// Index of the currently selected row, if any.
    pub selected: Option<usize>,
    /// Scroll offset (first visible row index).
    pub offset: usize,
    /// Optional persistence ID for state saving/restoration.
    /// When set, this state can be persisted via the [`Stateful`] trait.
    persistence_id: Option<String>,
    /// Current sort column (for undo support).
    #[allow(dead_code)]
    sort_column: Option<usize>,
    /// Sort ascending (for undo support).
    #[allow(dead_code)]
    sort_ascending: bool,
    /// Filter text (for undo support).
    #[allow(dead_code)]
    filter: String,
}

impl TableState {
    /// Set the selected row index, resetting offset on deselect.
    pub fn select(&mut self, index: Option<usize>) {
        self.selected = index;
        if index.is_none() {
            self.offset = 0;
        }
    }

    /// Create a new TableState with a persistence ID for state saving.
    #[must_use]
    pub fn with_persistence_id(mut self, id: impl Into<String>) -> Self {
        self.persistence_id = Some(id.into());
        self
    }

    /// Get the persistence ID, if set.
    #[must_use]
    pub fn persistence_id(&self) -> Option<&str> {
        self.persistence_id.as_deref()
    }
}

// ============================================================================
// Stateful Persistence Implementation
// ============================================================================

/// Persistable state for a [`TableState`].
///
/// This struct contains only the fields that should be persisted across
/// sessions. Derived/cached values are not included.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(
    feature = "state-persistence",
    derive(serde::Serialize, serde::Deserialize)
)]
pub struct TablePersistState {
    /// Selected row index.
    pub selected: Option<usize>,
    /// Scroll offset (first visible row).
    pub offset: usize,
}

impl crate::stateful::Stateful for TableState {
    type State = TablePersistState;

    fn state_key(&self) -> crate::stateful::StateKey {
        crate::stateful::StateKey::new("Table", self.persistence_id.as_deref().unwrap_or("default"))
    }

    fn save_state(&self) -> TablePersistState {
        TablePersistState {
            selected: self.selected,
            offset: self.offset,
        }
    }

    fn restore_state(&mut self, state: TablePersistState) {
        // Restore values directly; clamping to valid ranges happens during render
        self.selected = state.selected;
        self.offset = state.offset;
    }
}

// ============================================================================
// Undo Support Implementation
// ============================================================================

/// Snapshot of TableState for undo.
#[derive(Debug, Clone)]
pub struct TableStateSnapshot {
    selected: Option<usize>,
    offset: usize,
    sort_column: Option<usize>,
    sort_ascending: bool,
    filter: String,
}

impl UndoSupport for TableState {
    fn undo_widget_id(&self) -> UndoWidgetId {
        self.undo_id
    }

    fn create_snapshot(&self) -> Box<dyn Any + Send> {
        Box::new(TableStateSnapshot {
            selected: self.selected,
            offset: self.offset,
            sort_column: self.sort_column,
            sort_ascending: self.sort_ascending,
            filter: self.filter.clone(),
        })
    }

    fn restore_snapshot(&mut self, snapshot: &dyn Any) -> bool {
        if let Some(snap) = snapshot.downcast_ref::<TableStateSnapshot>() {
            self.selected = snap.selected;
            self.offset = snap.offset;
            self.sort_column = snap.sort_column;
            self.sort_ascending = snap.sort_ascending;
            self.filter = snap.filter.clone();
            true
        } else {
            false
        }
    }
}

impl TableUndoExt for TableState {
    fn sort_state(&self) -> (Option<usize>, bool) {
        (self.sort_column, self.sort_ascending)
    }

    fn set_sort_state(&mut self, column: Option<usize>, ascending: bool) {
        self.sort_column = column;
        self.sort_ascending = ascending;
    }

    fn filter_text(&self) -> &str {
        &self.filter
    }

    fn set_filter_text(&mut self, filter: &str) {
        self.filter = filter.to_string();
    }
}

impl TableState {
    /// Get the undo widget ID.
    ///
    /// This can be used to associate undo commands with this state instance.
    #[must_use]
    pub fn undo_id(&self) -> UndoWidgetId {
        self.undo_id
    }

    /// Get the current sort column.
    #[must_use]
    pub fn sort_column(&self) -> Option<usize> {
        self.sort_column
    }

    /// Get whether the sort is ascending.
    #[must_use]
    pub fn sort_ascending(&self) -> bool {
        self.sort_ascending
    }

    /// Set the sort state.
    pub fn set_sort(&mut self, column: Option<usize>, ascending: bool) {
        self.sort_column = column;
        self.sort_ascending = ascending;
    }

    /// Get the filter text.
    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Set the filter text.
    pub fn set_filter(&mut self, filter: impl Into<String>) {
        self.filter = filter.into();
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
            .map(|h| h.height.saturating_add(h.bottom_margin))
            .unwrap_or(0);

        if header_height > table_area.height {
            return;
        }

        let rows_top = table_area.y.saturating_add(header_height);
        let rows_max_y = table_area.bottom();
        let rows_height = rows_max_y.saturating_sub(rows_top);

        // Clamp offset to valid range
        if self.rows.is_empty() {
            state.offset = 0;
        } else {
            state.offset = state.offset.min(self.rows.len().saturating_sub(1));
        }

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
                    if row.height > max_y.saturating_sub(current_y) {
                        break;
                    }
                    current_y = current_y
                        .saturating_add(row.height)
                        .saturating_add(row.bottom_margin);
                    last_visible = i;
                }

                if selected > last_visible {
                    // Selected is below viewport. Find new offset to make it visible at bottom.
                    let mut new_offset = selected;
                    let mut accumulated_height = 0;
                    let available_height = rows_height;

                    // Iterate backwards from selected to find the earliest start row that fits
                    for i in (0..=selected).rev() {
                        let row = &self.rows[i];
                        // The selected row is the last visible; its bottom_margin extends
                        // below the viewport and should not count toward required space.
                        let total_row_height = if i == selected {
                            row.height
                        } else {
                            row.height.saturating_add(row.bottom_margin)
                        };

                        if total_row_height > available_height.saturating_sub(accumulated_height) {
                            // Cannot fit this row (i) along with subsequent rows up to selected.
                            // So the previous row (i+1) was the earliest possible start offset.
                            // If selected itself doesn't fit (accumulated_height == 0), we must show it anyway (at top).
                            if i == selected {
                                new_offset = selected;
                            } else {
                                new_offset = i + 1;
                            }
                            break;
                        }

                        accumulated_height = accumulated_height.saturating_add(total_row_height);
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
            if header.height > max_y.saturating_sub(y) {
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
            y = y
                .saturating_add(header.height)
                .saturating_add(header.bottom_margin);
        }

        // Render rows
        if self.rows.is_empty() {
            return;
        }

        // Handle scrolling/offset?
        // For v1 basic Table, we just render from state.offset

        for (i, row) in self.rows.iter().enumerate().skip(state.offset) {
            if y >= max_y {
                break;
            }

            let is_selected = state.selected == Some(i);
            let row_area = Rect::new(table_area.x, y, table_area.width, row.height);
            let style = if deg.apply_styling() {
                let s = if is_selected {
                    self.highlight_style.merge(&row.style)
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

            y = y
                .saturating_add(row.height)
                .saturating_add(row.bottom_margin);
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
                    cell_area.y.saturating_add(line_idx as u16),
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

impl MeasurableWidget for Table<'_> {
    fn measure(&self, _available: Size) -> SizeConstraints {
        if self.rows.is_empty() && self.header.is_none() {
            return SizeConstraints::ZERO;
        }

        let col_count = self.widths.len();
        if col_count == 0 {
            return SizeConstraints::ZERO;
        }

        // Calculate column widths from cell content
        let mut col_widths: Vec<u16> = vec![0; col_count];

        // Measure header cells
        if let Some(header) = &self.header {
            for (i, cell) in header.cells.iter().enumerate() {
                if i >= col_count {
                    break;
                }
                let cell_width = cell.width() as u16;
                col_widths[i] = col_widths[i].max(cell_width);
            }
        }

        // Measure data cells
        for row in &self.rows {
            for (i, cell) in row.cells.iter().enumerate() {
                if i >= col_count {
                    break;
                }
                let cell_width = cell.width() as u16;
                col_widths[i] = col_widths[i].max(cell_width);
            }
        }

        // Total width = sum of column widths + column spacing
        // Use saturating arithmetic to prevent overflow with many/wide columns
        let separator_width = if col_count > 1 {
            ((col_count - 1) as u16).saturating_mul(self.column_spacing)
        } else {
            0
        };
        let content_width: u16 = col_widths
            .iter()
            .fold(0u16, |acc, &w| acc.saturating_add(w))
            .saturating_add(separator_width);

        // Total height = header height + row heights + margins
        // Use saturating arithmetic to prevent overflow with many rows
        let header_height = self
            .header
            .as_ref()
            .map(|h| h.height.saturating_add(h.bottom_margin))
            .unwrap_or(0);

        let rows_height: u16 = self.rows.iter().fold(0u16, |acc, r| {
            acc.saturating_add(r.height.saturating_add(r.bottom_margin))
        });

        let content_height = header_height.saturating_add(rows_height);

        // Add block overhead if present
        let (block_width, block_height) = self
            .block
            .as_ref()
            .map(|b| {
                let inner = b.inner(Rect::new(0, 0, 100, 100));
                let w_overhead = 100u16.saturating_sub(inner.width);
                let h_overhead = 100u16.saturating_sub(inner.height);
                (w_overhead, h_overhead)
            })
            .unwrap_or((0, 0));

        let total_width = content_width.saturating_add(block_width);
        let total_height = content_height.saturating_add(block_height);

        SizeConstraints {
            min: Size::new(col_count as u16, 1), // At least column count width, 1 row
            preferred: Size::new(total_width, total_height),
            max: Some(Size::new(total_width, total_height)), // Fixed content size
        }
    }

    fn has_intrinsic_size(&self) -> bool {
        !self.rows.is_empty() || self.header.is_some()
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
            persistence_id: None,
            ..Default::default()
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
            persistence_id: None,
            ..Default::default()
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
            persistence_id: None,
            ..Default::default()
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

    // --- MeasurableWidget tests ---

    #[test]
    fn measure_empty_table() {
        let table = Table::new(Vec::<Row>::new(), [Constraint::Fixed(5)]);
        let c = table.measure(Size::MAX);
        assert_eq!(c, SizeConstraints::ZERO);
    }

    #[test]
    fn measure_empty_columns() {
        let table = Table::new([Row::new(["A"])], Vec::<Constraint>::new());
        let c = table.measure(Size::MAX);
        assert_eq!(c, SizeConstraints::ZERO);
    }

    #[test]
    fn measure_single_row() {
        let table = Table::new([Row::new(["Hello"])], [Constraint::Fixed(10)]);
        let c = table.measure(Size::MAX);

        assert_eq!(c.preferred.width, 5); // "Hello" is 5 chars
        assert_eq!(c.preferred.height, 1); // 1 row
        assert!(table.has_intrinsic_size());
    }

    #[test]
    fn measure_multiple_columns() {
        let table = Table::new(
            [Row::new(["A", "BB", "CCC"])],
            [
                Constraint::Fixed(5),
                Constraint::Fixed(5),
                Constraint::Fixed(5),
            ],
        )
        .column_spacing(2);

        let c = table.measure(Size::MAX);

        // Widths: 1 + 2 + 3 = 6, plus 2 gaps of 2 = 4 → total 10
        assert_eq!(c.preferred.width, 10);
        assert_eq!(c.preferred.height, 1);
    }

    #[test]
    fn measure_with_header() {
        let header = Row::new(["Name", "Value"]);
        let table = Table::new(
            [Row::new(["foo", "42"])],
            [Constraint::Fixed(5), Constraint::Fixed(5)],
        )
        .header(header);

        let c = table.measure(Size::MAX);

        // Header "Name" and "Value" are wider than "foo" and "42"
        // Widths: max(4, 3) = 4, max(5, 2) = 5, plus 1 gap = 10
        assert_eq!(c.preferred.width, 10);
        // Height: 1 header + 1 data row = 2
        assert_eq!(c.preferred.height, 2);
    }

    #[test]
    fn measure_with_row_margins() {
        let table = Table::new(
            [
                Row::new(["A"]).bottom_margin(2),
                Row::new(["B"]).bottom_margin(1),
            ],
            [Constraint::Fixed(5)],
        );

        let c = table.measure(Size::MAX);

        // Heights: (1 + 2) + (1 + 1) = 5
        assert_eq!(c.preferred.height, 5);
    }

    #[test]
    fn measure_column_widths_from_max_cell() {
        let table = Table::new(
            [Row::new(["A", "BB"]), Row::new(["CCC", "D"])],
            [Constraint::Fixed(5), Constraint::Fixed(5)],
        )
        .column_spacing(1);

        let c = table.measure(Size::MAX);

        // Column 0: max(1, 3) = 3
        // Column 1: max(2, 1) = 2
        // Total: 3 + 2 + 1 gap = 6
        assert_eq!(c.preferred.width, 6);
        assert_eq!(c.preferred.height, 2);
    }

    #[test]
    fn measure_min_is_column_count() {
        let table = Table::new(
            [Row::new(["A", "B", "C"])],
            [
                Constraint::Fixed(5),
                Constraint::Fixed(5),
                Constraint::Fixed(5),
            ],
        );

        let c = table.measure(Size::MAX);

        // Minimum width should be at least the number of columns
        assert_eq!(c.min.width, 3);
        assert_eq!(c.min.height, 1);
    }

    #[test]
    fn measure_has_intrinsic_size() {
        let empty = Table::new(Vec::<Row>::new(), [Constraint::Fixed(5)]);
        assert!(!empty.has_intrinsic_size());

        let with_rows = Table::new([Row::new(["X"])], [Constraint::Fixed(5)]);
        assert!(with_rows.has_intrinsic_size());

        let header_only =
            Table::new(Vec::<Row>::new(), [Constraint::Fixed(5)]).header(Row::new(["Header"]));
        assert!(header_only.has_intrinsic_size());
    }

    // --- Stateful Persistence tests ---

    use crate::stateful::Stateful;

    #[test]
    fn table_state_with_persistence_id() {
        let state = TableState::default().with_persistence_id("my-table");
        assert_eq!(state.persistence_id(), Some("my-table"));
    }

    #[test]
    fn table_state_default_no_persistence_id() {
        let state = TableState::default();
        assert_eq!(state.persistence_id(), None);
    }

    #[test]
    fn table_state_save_restore_round_trip() {
        let mut state = TableState::default().with_persistence_id("test");
        state.select(Some(5));
        state.offset = 3;

        let saved = state.save_state();
        assert_eq!(saved.selected, Some(5));
        assert_eq!(saved.offset, 3);

        // Reset state
        state.select(None);
        state.offset = 0;
        assert_eq!(state.selected, None);
        assert_eq!(state.offset, 0);

        // Restore
        state.restore_state(saved);
        assert_eq!(state.selected, Some(5));
        assert_eq!(state.offset, 3);
    }

    #[test]
    fn table_state_key_uses_persistence_id() {
        let state = TableState::default().with_persistence_id("main-data-table");
        let key = state.state_key();
        assert_eq!(key.widget_type, "Table");
        assert_eq!(key.instance_id, "main-data-table");
    }

    #[test]
    fn table_state_key_default_when_no_id() {
        let state = TableState::default();
        let key = state.state_key();
        assert_eq!(key.widget_type, "Table");
        assert_eq!(key.instance_id, "default");
    }

    #[test]
    fn table_persist_state_default() {
        let persist = TablePersistState::default();
        assert_eq!(persist.selected, None);
        assert_eq!(persist.offset, 0);
    }

    // ============================================================================
    // Undo Support Tests
    // ============================================================================

    #[test]
    fn table_state_undo_widget_id_unique() {
        let state1 = TableState::default();
        let state2 = TableState::default();
        assert_ne!(state1.undo_id(), state2.undo_id());
    }

    #[test]
    fn table_state_undo_snapshot_and_restore() {
        let mut state = TableState::default();
        state.select(Some(5));
        state.offset = 2;
        state.set_sort(Some(1), false);
        state.set_filter("test filter");

        // Create snapshot
        let snapshot = state.create_snapshot();

        // Modify state
        state.select(Some(10));
        state.offset = 7;
        state.set_sort(Some(3), true);
        state.set_filter("new filter");

        assert_eq!(state.selected, Some(10));
        assert_eq!(state.offset, 7);
        assert_eq!(state.sort_column(), Some(3));
        assert!(state.sort_ascending());
        assert_eq!(state.filter(), "new filter");

        // Restore snapshot
        assert!(state.restore_snapshot(&*snapshot));

        // Verify restored state
        assert_eq!(state.selected, Some(5));
        assert_eq!(state.offset, 2);
        assert_eq!(state.sort_column(), Some(1));
        assert!(!state.sort_ascending());
        assert_eq!(state.filter(), "test filter");
    }

    #[test]
    fn table_state_undo_ext_sort() {
        let mut state = TableState::default();

        // Initial state
        assert_eq!(state.sort_state(), (None, false));

        // Set sort
        state.set_sort_state(Some(2), true);
        assert_eq!(state.sort_state(), (Some(2), true));

        // Change sort
        state.set_sort_state(Some(0), false);
        assert_eq!(state.sort_state(), (Some(0), false));
    }

    #[test]
    fn table_state_undo_ext_filter() {
        let mut state = TableState::default();

        // Initial state
        assert_eq!(state.filter_text(), "");

        // Set filter
        state.set_filter_text("search term");
        assert_eq!(state.filter_text(), "search term");

        // Clear filter
        state.set_filter_text("");
        assert_eq!(state.filter_text(), "");
    }

    #[test]
    fn table_state_restore_wrong_snapshot_type_fails() {
        use std::any::Any;
        let mut state = TableState::default();
        let wrong_snapshot: Box<dyn Any + Send> = Box::new(42i32);
        assert!(!state.restore_snapshot(&*wrong_snapshot));
    }
}
