use crate::borders::Borders;
use crate::block::Block;
use crate::{Widget, StatefulWidget};
use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell as RenderCell;
use ftui_style::style::Style;
use ftui_layout::{Constraint, Flex, Direction};
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
    rows: Vec<Row<'a>>,
    widths: Vec<Constraint>,
    header: Option<Row<'a>>,
    block: Option<Block<'a>>,
    style: Style,
    highlight_style: Style,
    column_spacing: u16,
}

impl<'a> Table<'a> {
    pub fn new(rows: impl IntoIterator<Item = Row<'a>>, widths: impl IntoIterator<Item = Constraint>) -> Self {
        Self {
            rows: rows.into_iter().collect(),
            widths: widths.into_iter().collect(),
            header: None,
            block: None,
            style: Style::default(),
            highlight_style: Style::default(),
            column_spacing: 1,
        }
    }

    pub fn header(mut self, header: Row<'a>) -> Self {
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
}

impl<'a> Widget for Table<'a> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let mut state = TableState::default();
        StatefulWidget::render(self, area, buf, &mut state);
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

    fn render(&self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if area.is_empty() {
            return;
        }

        // Render block if present
        let table_area = match &self.block {
            Some(b) => {
                b.render(area, buf);
                b.inner(area)
            }
            None => area,
        };

        if table_area.is_empty() {
            return;
        }

        // Calculate column widths
        let flex = Flex::horizontal()
            .constraints(self.widths.clone())
            .gap(self.column_spacing);
        
        // We need a dummy rect with correct width to solve horizontal constraints
        // Height doesn't matter for width solving in standard flex
        let column_rects = flex.split(Rect::new(table_area.x, table_area.y, table_area.width, 1));
        
        let mut y = table_area.y;
        let max_y = table_area.bottom();

        // Render header
        if let Some(header) = &self.header {
            if y + header.height > max_y {
                return;
            }
            render_row(header, &column_rects, buf, y, header.style);
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
            
            let is_selected = state.selected.map_or(false, |s| s == i);
            let style = if is_selected {
                self.highlight_style
            } else {
                row.style
            };
            
            // Merge with table base style?
            // Usually specific row style overrides table style.
            
            render_row(row, &column_rects, buf, y, style);
            y += row.height + row.bottom_margin;
        }
    }
}

fn render_row(row: &Row, col_rects: &[Rect], buf: &mut Buffer, y: u16, style: Style) {
    for (i, cell_text) in row.cells.iter().enumerate() {
        if i >= col_rects.len() {
            break;
        }
        let rect = col_rects[i];
        let cell_area = Rect::new(rect.x, y, rect.width, row.height);
        
        // Fill background for row style
        // We need to fill the whole cell_area with style
        if !style.is_empty() {
            for cy in cell_area.y..cell_area.bottom() {
                for cx in cell_area.x..cell_area.right() {
                    if let Some(cell) = buf.get_mut(cx, cy) {
                        crate::apply_style(cell, style);
                    }
                }
            }
        }
        
        // Render text
        // Reuse Paragraph logic essentially
        // Ideally we would use Paragraph widget here, but we are inside Table.
        // For now, simple text rendering.
        
        let styled_text = cell_text.clone().with_base_style(style);
        
        for (line_idx, line) in styled_text.lines().iter().enumerate() {
            if line_idx as u16 >= row.height {
                break;
            }
            
            let mut x = cell_area.x;
            for span in line.spans() {
                for c in span.content.chars() {
                    if x >= cell_area.right() {
                        break;
                    }
                    if let Some(cell) = buf.get_mut(x, cell_area.y + line_idx as u16) {
                        cell.content = ftui_render::cell::CellContent::from_char(c);
                        if let Some(span_style) = span.style {
                             crate::apply_style(cell, span_style);
                        }
                    }
                    x += 1;
                }
            }
        }
    }
}
