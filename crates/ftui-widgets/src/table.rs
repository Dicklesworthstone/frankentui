use crate::block::Block;
use crate::mouse::MouseResult;
use crate::undo_support::{TableUndoExt, UndoSupport, UndoWidgetId};
use crate::{
    MeasurableWidget, SizeConstraints, StatefulWidget, Widget, apply_style, set_style_area,
};
use ftui_core::event::{MouseButton, MouseEvent, MouseEventKind};
use ftui_core::geometry::{Rect, Size};
use ftui_layout::{Constraint, Flex};
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::frame::{Frame, HitId, HitRegion};
use ftui_style::{
    Style, TableEffectResolver, TableEffectScope, TableEffectTarget, TableSection, TableTheme,
};
use ftui_text::Text;
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
    intrinsic_col_widths: Vec<u16>,
    header: Option<Row>,
    block: Option<Block<'a>>,
    style: Style,
    highlight_style: Style,
    theme: TableTheme,
    theme_phase: f32,
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
        let rows: Vec<Row> = rows.into_iter().collect();
        let widths: Vec<Constraint> = widths.into_iter().collect();
        let col_count = widths.len();

        let intrinsic_col_widths = if Self::requires_measurement(&widths) {
            Self::compute_intrinsic_widths(&rows, None, col_count)
        } else {
            Vec::new()
        };

        Self {
            rows,
            widths,
            intrinsic_col_widths,
            header: None,
            block: None,
            style: Style::default(),
            highlight_style: Style::default(),
            theme: TableTheme::default(),
            theme_phase: 0.0,
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

    /// Set the table theme (base/states/effects).
    pub fn theme(mut self, theme: TableTheme) -> Self {
        self.theme = theme;
        self
    }

    /// Set the explicit animation phase for theme effects.
    ///
    /// Phase is deterministic and should be supplied by the caller (e.g. from tick count).
    pub fn theme_phase(mut self, phase: f32) -> Self {
        self.theme_phase = phase;
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

    fn requires_measurement(constraints: &[Constraint]) -> bool {
        constraints.iter().any(|c| {
            matches!(
                c,
                Constraint::FitContent | Constraint::FitContentBounded { .. } | Constraint::FitMin
            )
        })
    }

    fn compute_intrinsic_widths(rows: &[Row], header: Option<&Row>, col_count: usize) -> Vec<u16> {
        if col_count == 0 {
            return Vec::new();
        }

        let mut col_widths: Vec<u16> = vec![0; col_count];

        if let Some(header) = header {
            for (i, cell) in header.cells.iter().enumerate().take(col_count) {
                let cell_width = cell.width().min(u16::MAX as usize) as u16;
                col_widths[i] = col_widths[i].max(cell_width);
            }
        }

        for row in rows {
            for (i, cell) in row.cells.iter().enumerate().take(col_count) {
                let cell_width = cell.width().min(u16::MAX as usize) as u16;
                col_widths[i] = col_widths[i].max(cell_width);
            }
        }

        col_widths
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
    /// Index of the currently hovered row, if any.
    pub hovered: Option<usize>,
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
    /// Current sort column index.
    pub sort_column: Option<usize>,
    /// Sort direction (true = ascending, false = descending).
    pub sort_ascending: bool,
    /// Active filter text.
    pub filter: String,
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
            sort_column: self.sort_column,
            sort_ascending: self.sort_ascending,
            filter: self.filter.clone(),
        }
    }

    fn restore_state(&mut self, state: TablePersistState) {
        // Restore values directly; clamping to valid ranges happens during render
        self.selected = state.selected;
        self.hovered = None;
        self.offset = state.offset;
        self.sort_column = state.sort_column;
        self.sort_ascending = state.sort_ascending;
        self.filter = state.filter;
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
            self.hovered = None;
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

    /// Handle a mouse event for this table.
    ///
    /// # Hit data convention
    ///
    /// The hit data (`u64`) encodes the row index. When the table renders with
    /// a `hit_id`, each visible row registers `HitRegion::Content` with
    /// `data = row_index as u64`.
    ///
    /// # Arguments
    ///
    /// * `event` — the mouse event from the terminal
    /// * `hit` — result of `frame.hit_test(event.x, event.y)`, if available
    /// * `expected_id` — the `HitId` this table was rendered with
    /// * `row_count` — total number of rows in the table
    pub fn handle_mouse(
        &mut self,
        event: &MouseEvent,
        hit: Option<(HitId, HitRegion, u64)>,
        expected_id: HitId,
        row_count: usize,
    ) -> MouseResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((id, HitRegion::Content, data)) = hit
                    && id == expected_id
                {
                    let index = data as usize;
                    if index < row_count {
                        // Deterministic "double click": second click on the already-selected row activates.
                        if self.selected == Some(index) {
                            return MouseResult::Activated(index);
                        }
                        self.select(Some(index));
                        return MouseResult::Selected(index);
                    }
                }
                MouseResult::Ignored
            }
            MouseEventKind::Moved => {
                if let Some((id, HitRegion::Content, data)) = hit
                    && id == expected_id
                {
                    let index = data as usize;
                    if index < row_count {
                        let changed = self.hovered != Some(index);
                        self.hovered = Some(index);
                        return if changed {
                            MouseResult::HoverChanged
                        } else {
                            MouseResult::Ignored
                        };
                    }
                }
                // Mouse moved off the widget or to non-content region
                if self.hovered.is_some() {
                    self.hovered = None;
                    MouseResult::HoverChanged
                } else {
                    MouseResult::Ignored
                }
            }
            MouseEventKind::ScrollUp => {
                self.scroll_up(3);
                MouseResult::Scrolled
            }
            MouseEventKind::ScrollDown => {
                self.scroll_down(3, row_count);
                MouseResult::Scrolled
            }
            _ => MouseResult::Ignored,
        }
    }

    /// Scroll the table up by the given number of lines.
    pub fn scroll_up(&mut self, lines: usize) {
        self.offset = self.offset.saturating_sub(lines);
    }

    /// Scroll the table down by the given number of lines.
    ///
    /// Clamps so that the last row can still appear at the top of the viewport.
    pub fn scroll_down(&mut self, lines: usize, row_count: usize) {
        self.offset = (self.offset + lines).min(row_count.saturating_sub(1));
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

        let apply_styling = frame.degradation.apply_styling();
        let theme = &self.theme;
        let effects_enabled = apply_styling && !theme.effects.is_empty();
        let has_column_effects = effects_enabled && theme_has_column_effects(theme);
        let effect_resolver = theme.effect_resolver();
        let effects = if effects_enabled {
            Some((&effect_resolver, self.theme_phase))
        } else {
            None
        };

        // Render block if present
        let table_area = match &self.block {
            Some(b) => {
                let mut block = b.clone();
                if apply_styling {
                    block = block.border_style(theme.border);
                }
                block.render(area, frame);
                block.inner(area)
            }
            None => area,
        };

        if table_area.is_empty() {
            return;
        }

        // Push scissor to prevent rows from spilling out of the table area.
        // This is critical for rows with height > 1 that are partially visible at the bottom.
        frame.buffer.push_scissor(table_area);

        // Apply base style to the entire table area (clears gaps/empty space)
        if apply_styling {
            let fill_style = self.style.merge(&theme.row);
            set_style_area(&mut frame.buffer, table_area, fill_style);
        }

        let header_height = self
            .header
            .as_ref()
            .map(|h| h.height.saturating_add(h.bottom_margin))
            .unwrap_or(0);

        if header_height > table_area.height {
            frame.buffer.pop_scissor();
            return;
        }

        let rows_top = table_area.y.saturating_add(header_height);
        let rows_max_y = table_area.bottom();
        let rows_height = rows_max_y.saturating_sub(rows_top);

        // Clamp offset to valid range
        if self.rows.is_empty() {
            state.offset = 0;
        } else {
            let row_count = self.rows.len();
            state.offset = state.offset.min(row_count.saturating_sub(1));

            // If we're scrolled near the end and the viewport grows, keep the bottom
            // visible and pull the offset back to fill the viewport with as much
            // context as fits (avoids rendering a mostly-empty table).
            //
            // We treat the last row's bottom_margin as "optional" (it may be clipped
            // by the scissor), matching the selection-visibility logic below.
            let available_height = rows_height;
            let mut accumulated = 0u16;
            let mut bottom_offset = row_count.saturating_sub(1);
            for i in (0..row_count).rev() {
                let row = &self.rows[i];
                let total_row_height = if i == row_count - 1 {
                    row.height
                } else {
                    row.height.saturating_add(row.bottom_margin)
                };

                if total_row_height > available_height.saturating_sub(accumulated) {
                    // If even the last row doesn't fit, we still show it.
                    break;
                }

                accumulated = accumulated.saturating_add(total_row_height);
                bottom_offset = i;
            }

            state.offset = state.offset.min(bottom_offset);
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
        let column_rects = flex.split_with_measurer(
            Rect::new(table_area.x, table_area.y, table_area.width, 1),
            |idx, _| {
                // Use cached intrinsic widths (rows) and merge with header width
                let row_width = self.intrinsic_col_widths.get(idx).copied().unwrap_or(0);
                let header_width = self
                    .header
                    .as_ref()
                    .and_then(|h| h.cells.get(idx))
                    .map(|c| c.width().min(u16::MAX as usize) as u16)
                    .unwrap_or(0);
                ftui_layout::LayoutSizeHint::exact(row_width.max(header_width))
            },
        );

        let mut y = table_area.y;
        let max_y = table_area.bottom();
        let divider_char = divider_char(self.block.as_ref());

        // Render header
        if let Some(header) = &self.header {
            if header.height > max_y.saturating_sub(y) {
                frame.buffer.pop_scissor();
                return;
            }
            let row_area = Rect::new(table_area.x, y, table_area.width, header.height);
            let header_style = if apply_styling {
                let mut style = theme.header;
                style = self.style.merge(&style);
                header.style.merge(&style)
            } else {
                Style::default()
            };

            if apply_styling {
                set_style_area(&mut frame.buffer, row_area, header_style);
                if let Some((resolver, phase)) = effects {
                    for (col_idx, rect) in column_rects.iter().enumerate() {
                        let cell_area = Rect::new(rect.x, y, rect.width, header.height);
                        let scope = TableEffectScope {
                            section: TableSection::Header,
                            row: None,
                            column: Some(col_idx),
                        };
                        let style = resolver.resolve(header_style, scope, phase);
                        set_style_area(&mut frame.buffer, cell_area, style);
                    }
                }
            }

            let divider_style = if apply_styling {
                theme.divider.merge(&header_style)
            } else {
                Style::default()
            };
            draw_vertical_dividers(
                &mut frame.buffer,
                row_area,
                &column_rects,
                divider_char,
                divider_style,
            );

            render_row(
                header,
                &column_rects,
                frame,
                y,
                header_style,
                TableSection::Header,
                None,
                effects,
                effects.is_some(),
            );
            y = y
                .saturating_add(header.height)
                .saturating_add(header.bottom_margin);
        }

        // Render rows
        if self.rows.is_empty() {
            frame.buffer.pop_scissor();
            return;
        }

        // Handle scrolling/offset?
        // For v1 basic Table, we just render from state.offset

        for (i, row) in self.rows.iter().enumerate().skip(state.offset) {
            if y >= max_y {
                break;
            }

            let is_selected = state.selected == Some(i);
            let is_hovered = state.hovered == Some(i);
            let row_area = Rect::new(table_area.x, y, table_area.width, row.height);
            let row_style = if apply_styling {
                let mut style = if i % 2 == 0 { theme.row } else { theme.row_alt };
                if is_selected {
                    style = theme.row_selected.merge(&style);
                }
                if is_hovered {
                    style = theme.row_hover.merge(&style);
                }
                style = self.style.merge(&style);
                style = row.style.merge(&style);
                if is_selected {
                    style = self.highlight_style.merge(&style);
                }
                style
            } else {
                Style::default()
            };

            if apply_styling {
                if let Some((resolver, phase)) = effects {
                    if has_column_effects {
                        set_style_area(&mut frame.buffer, row_area, row_style);
                        for (col_idx, rect) in column_rects.iter().enumerate() {
                            let cell_area = Rect::new(rect.x, y, rect.width, row.height);
                            let scope = TableEffectScope {
                                section: TableSection::Body,
                                row: Some(i),
                                column: Some(col_idx),
                            };
                            let style = resolver.resolve(row_style, scope, phase);
                            set_style_area(&mut frame.buffer, cell_area, style);
                        }
                    } else {
                        let scope = TableEffectScope::row(TableSection::Body, i);
                        let style = resolver.resolve(row_style, scope, phase);
                        set_style_area(&mut frame.buffer, row_area, style);
                    }
                } else {
                    set_style_area(&mut frame.buffer, row_area, row_style);
                }
            }

            let divider_style = if apply_styling {
                theme.divider.merge(&row_style)
            } else {
                Style::default()
            };
            draw_vertical_dividers(
                &mut frame.buffer,
                row_area,
                &column_rects,
                divider_char,
                divider_style,
            );

            render_row(
                row,
                &column_rects,
                frame,
                y,
                row_style,
                TableSection::Body,
                Some(i),
                effects,
                has_column_effects,
            );

            // Register hit region for this row (if hit testing enabled)
            if let Some(id) = self.hit_id {
                frame.register_hit(row_area, id, HitRegion::Content, i as u64);
            }

            y = y
                .saturating_add(row.height)
                .saturating_add(row.bottom_margin);
        }

        frame.buffer.pop_scissor();
    }
}

#[allow(clippy::too_many_arguments)]
fn render_row(
    row: &Row,
    col_rects: &[Rect],
    frame: &mut Frame,
    y: u16,
    base_style: Style,
    section: TableSection,
    row_idx: Option<usize>,
    effects: Option<(&TableEffectResolver<'_>, f32)>,
    column_effects: bool,
) {
    let apply_styling = frame.degradation.apply_styling();
    let row_effect_base = if apply_styling {
        if let Some((resolver, phase)) = effects {
            if !column_effects {
                let scope = TableEffectScope {
                    section,
                    row: row_idx,
                    column: None,
                };
                Some(resolver.resolve(base_style, scope, phase))
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    for (col_idx, cell_text) in row.cells.iter().enumerate() {
        if col_idx >= col_rects.len() {
            break;
        }
        let rect = col_rects[col_idx];
        let cell_area = Rect::new(rect.x, y, rect.width, row.height);
        let scope = if effects.is_some() {
            Some(TableEffectScope {
                section,
                row: row_idx,
                column: if column_effects { Some(col_idx) } else { None },
            })
        } else {
            None
        };
        let column_effect_base = if apply_styling && column_effects {
            if let (Some((resolver, phase)), Some(scope)) = (effects, scope) {
                Some(resolver.resolve(base_style, scope, phase))
            } else {
                None
            }
        } else {
            None
        };

        for (line_idx, line) in cell_text.lines().iter().enumerate() {
            if line_idx as u16 >= row.height {
                break;
            }

            let mut x = cell_area.x;
            for span in line.spans() {
                // At NoStyling+, ignore span-level styles
                let mut span_style = if apply_styling {
                    match span.style {
                        Some(s) => s.merge(&base_style),
                        None => base_style,
                    }
                } else {
                    Style::default()
                };

                if let (Some((resolver, phase)), Some(scope)) = (effects, scope) {
                    if span.style.is_none() {
                        if let Some(base_effect) = column_effect_base.or(row_effect_base) {
                            span_style = base_effect;
                        } else {
                            span_style = resolver.resolve(span_style, scope, phase);
                        }
                    } else {
                        span_style = resolver.resolve(span_style, scope, phase);
                    }
                }

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

fn theme_has_column_effects(theme: &TableTheme) -> bool {
    theme.effects.iter().any(|rule| {
        matches!(
            rule.target,
            TableEffectTarget::Column(_) | TableEffectTarget::ColumnRange { .. }
        )
    })
}

fn divider_char(block: Option<&Block<'_>>) -> char {
    block
        .map(|b| b.border_set().vertical)
        .unwrap_or(crate::borders::BorderSet::SQUARE.vertical)
}

fn draw_vertical_dividers(
    buf: &mut Buffer,
    row_area: Rect,
    col_rects: &[Rect],
    divider_char: char,
    style: Style,
) {
    if col_rects.len() < 2 || row_area.is_empty() {
        return;
    }

    for pair in col_rects.windows(2) {
        let left = pair[0];
        let right = pair[1];
        let gap = right.x.saturating_sub(left.right());
        if gap == 0 {
            continue;
        }
        let x = left.right();
        if x >= row_area.right() {
            continue;
        }
        for y in row_area.y..row_area.bottom() {
            let mut cell = Cell::from_char(divider_char);
            apply_style(&mut cell, style);
            buf.set(x, y, cell);
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

        let fallback;
        let row_widths = if self.intrinsic_col_widths.len() == col_count {
            &self.intrinsic_col_widths
        } else {
            // Compute rows only (pass None for header) to match intrinsic_col_widths semantics
            fallback = Self::compute_intrinsic_widths(&self.rows, None, col_count);
            &fallback
        };

        // Total width = sum of max(row_width, header_width) + column spacing
        let separator_width = if col_count > 1 {
            ((col_count - 1) as u16).saturating_mul(self.column_spacing)
        } else {
            0
        };

        let mut summed_col_width = 0u16;
        for (i, &r_w) in row_widths.iter().enumerate() {
            let h_w = self
                .header
                .as_ref()
                .and_then(|h| h.cells.get(i))
                .map(|c| c.width().min(u16::MAX as usize) as u16)
                .unwrap_or(0);
            summed_col_width = summed_col_width.saturating_add(r_w.max(h_w));
        }

        let content_width = summed_col_width.saturating_add(separator_width);

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
    use ftui_render::cell::PackedRgba;
    use ftui_render::grapheme_pool::GraphemePool;
    use ftui_text::{Line, Span};

    fn cell_char(buf: &Buffer, x: u16, y: u16) -> Option<char> {
        buf.get(x, y).and_then(|c| c.content.as_char())
    }

    fn cell_fg(buf: &Buffer, x: u16, y: u16) -> Option<PackedRgba> {
        buf.get(x, y).map(|c| c.fg)
    }

    fn row_text(buf: &Buffer, y: u16) -> String {
        let width = buf.width();
        let mut actual = String::new();
        for x in 0..width {
            let ch = buf
                .get(x, y)
                .and_then(|cell| cell.content.as_char())
                .unwrap_or(' ');
            actual.push(ch);
        }
        actual.trim().to_string()
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
    fn row_style_merge_precedence_and_span_override() {
        let base_fg = PackedRgba::rgb(10, 0, 0);
        let selected_fg = PackedRgba::rgb(20, 0, 0);
        let hovered_fg = PackedRgba::rgb(30, 0, 0);
        let table_fg = PackedRgba::rgb(40, 0, 0);
        let row_fg = PackedRgba::rgb(50, 0, 0);
        let highlight_fg = PackedRgba::rgb(60, 0, 0);
        let span_fg = PackedRgba::rgb(70, 0, 0);

        let mut theme = TableTheme::default();
        theme.row = Style::new().fg(base_fg);
        theme.row_alt = theme.row;
        theme.row_selected = Style::new().fg(selected_fg);
        theme.row_hover = Style::new().fg(hovered_fg);

        let text = Text::from_line(Line::from_spans([
            Span::raw("A"),
            Span::styled("B", Style::new().fg(span_fg)),
        ]));

        let table = Table::new(
            [Row::new([text]).style(Style::new().fg(row_fg))],
            [Constraint::Fixed(2)],
        )
        .style(Style::new().fg(table_fg))
        .highlight_style(Style::new().fg(highlight_fg))
        .theme(theme);

        let area = Rect::new(0, 0, 2, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(2, 1, &mut pool);
        let mut state = TableState {
            selected: Some(0),
            hovered: Some(0),
            ..Default::default()
        };

        StatefulWidget::render(&table, area, &mut frame, &mut state);

        assert_eq!(cell_fg(&frame.buffer, 0, 0), Some(highlight_fg));
        assert_eq!(cell_fg(&frame.buffer, 1, 0), Some(span_fg));
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
    fn table_clamps_offset_to_fill_viewport_on_resize() {
        let rows: Vec<Row> = (0..10).map(|i| Row::new([format!("Row {i}")])).collect();
        let table = Table::new(rows, [Constraint::Min(10)]);

        let mut pool = GraphemePool::new();
        let mut state = TableState {
            offset: 7,
            ..Default::default()
        };

        // Small viewport: show 7, 8, 9.
        let area_small = Rect::new(0, 0, 10, 3);
        let mut frame_small = Frame::new(10, 3, &mut pool);
        StatefulWidget::render(&table, area_small, &mut frame_small, &mut state);
        assert_eq!(state.offset, 7);
        assert_eq!(row_text(&frame_small.buffer, 0), "Row 7");
        assert_eq!(row_text(&frame_small.buffer, 2), "Row 9");

        // Larger viewport: offset should pull back to fill (5..9).
        let area_large = Rect::new(0, 0, 10, 5);
        let mut frame_large = Frame::new(10, 5, &mut pool);
        StatefulWidget::render(&table, area_large, &mut frame_large, &mut state);
        assert_eq!(state.offset, 5);
        assert_eq!(row_text(&frame_large.buffer, 0), "Row 5");
        assert_eq!(row_text(&frame_large.buffer, 4), "Row 9");
    }

    #[test]
    fn table_clamps_offset_to_fill_viewport_with_variable_row_heights() {
        // Rows 0..8: height 1
        // Row 9: height 5
        // View height 10 should show rows 4..9 (with row 9 taking 5 lines).
        let mut rows: Vec<Row> = (0..9).map(|i| Row::new([format!("Row {i}")])).collect();
        rows.push(Row::new(["Row 9"]).height(5));
        let table = Table::new(rows, [Constraint::Min(10)]);

        let mut pool = GraphemePool::new();
        let mut state = TableState {
            offset: 9,
            ..Default::default()
        };

        let area = Rect::new(0, 0, 10, 10);
        let mut frame = Frame::new(10, 10, &mut pool);
        StatefulWidget::render(&table, area, &mut frame, &mut state);

        assert_eq!(state.offset, 4);
        assert_eq!(row_text(&frame.buffer, 0), "Row 4");
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
    fn divider_style_overrides_row_style() {
        let row_fg = PackedRgba::rgb(120, 10, 10);
        let divider_fg = PackedRgba::rgb(0, 200, 0);
        let mut theme = TableTheme::default();
        theme.row = Style::new().fg(row_fg);
        theme.row_alt = theme.row;
        theme.divider = Style::new().fg(divider_fg);

        let table = Table::new(
            [Row::new(["AA", "BB"])],
            [Constraint::Fixed(2), Constraint::Fixed(2)],
        )
        .theme(theme);

        let area = Rect::new(0, 0, 5, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        Widget::render(&table, area, &mut frame);

        assert_eq!(cell_fg(&frame.buffer, 2, 0), Some(divider_fg));
    }

    #[test]
    fn block_border_uses_theme_border_style() {
        let border_fg = PackedRgba::rgb(1, 2, 3);
        let theme = TableTheme {
            border: Style::new().fg(border_fg),
            ..Default::default()
        };

        let table = Table::new([Row::new(["X"])], [Constraint::Fixed(1)])
            .block(Block::bordered())
            .theme(theme);

        let area = Rect::new(0, 0, 3, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 3, &mut pool);
        Widget::render(&table, area, &mut frame);

        assert_eq!(cell_fg(&frame.buffer, 0, 0), Some(border_fg));
    }

    #[test]
    fn render_clips_long_cell_to_column_width() {
        let table = Table::new([Row::new(["ABCDE"])], [Constraint::Fixed(3)]);
        let area = Rect::new(0, 0, 3, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 1, &mut pool);
        Widget::render(&table, area, &mut frame);

        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('A'));
        assert_eq!(cell_char(&frame.buffer, 1, 0), Some('B'));
        assert_eq!(cell_char(&frame.buffer, 2, 0), Some('C'));
        assert_ne!(cell_char(&frame.buffer, 3, 0), Some('D'));
    }

    #[test]
    fn render_multiline_cell_respects_row_height() {
        let table = Table::new([Row::new(["A\nB"]).height(1)], [Constraint::Fixed(3)]);
        let area = Rect::new(0, 0, 3, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 2, &mut pool);
        Widget::render(&table, area, &mut frame);

        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('A'));
        assert_ne!(cell_char(&frame.buffer, 0, 1), Some('B'));
    }

    #[test]
    fn render_multiline_cell_draws_second_line_when_height_allows() {
        let table = Table::new([Row::new(["A\nB"]).height(2)], [Constraint::Fixed(3)]);
        let area = Rect::new(0, 0, 3, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 2, &mut pool);
        Widget::render(&table, area, &mut frame);

        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('A'));
        assert_eq!(cell_char(&frame.buffer, 0, 1), Some('B'));
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
    fn measure_respects_row_height_and_column_spacing() {
        let table = Table::new(
            [Row::new(["A", "BB"]).height(2)],
            [Constraint::FitContent, Constraint::FitContent],
        )
        .column_spacing(2);

        let c = table.measure(Size::MAX);

        assert_eq!(c.preferred.width, 5);
        assert_eq!(c.preferred.height, 2);
    }

    #[test]
    fn measure_accounts_for_wide_glyphs() {
        let table = Table::new(
            [Row::new(["界", "A"])],
            [Constraint::FitContent, Constraint::FitContent],
        )
        .column_spacing(1);

        let c = table.measure(Size::MAX);

        assert_eq!(c.preferred.width, 4);
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
        state.set_sort(Some(2), true);
        state.set_filter("search term");

        let saved = state.save_state();
        assert_eq!(saved.selected, Some(5));
        assert_eq!(saved.offset, 3);
        assert_eq!(saved.sort_column, Some(2));
        assert!(saved.sort_ascending);
        assert_eq!(saved.filter, "search term");

        // Reset state
        state.select(None);
        state.offset = 0;
        state.set_sort(None, false);
        state.set_filter("");
        assert_eq!(state.selected, None);
        assert_eq!(state.offset, 0);
        assert_eq!(state.sort_column(), None);
        assert!(!state.sort_ascending());
        assert!(state.filter().is_empty());

        // Restore
        state.restore_state(saved);
        assert_eq!(state.selected, Some(5));
        assert_eq!(state.offset, 3);
        assert_eq!(state.sort_column(), Some(2));
        assert!(state.sort_ascending());
        assert_eq!(state.filter(), "search term");
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
        assert_eq!(persist.sort_column, None);
        assert!(!persist.sort_ascending);
        assert!(persist.filter.is_empty());
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

    // --- Mouse handling tests ---

    use crate::mouse::MouseResult;
    use ftui_core::event::{MouseButton, MouseEvent, MouseEventKind};

    #[test]
    fn table_state_click_selects() {
        let mut state = TableState::default();
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 5, 2);
        let hit = Some((HitId::new(1), HitRegion::Content, 4u64));
        let result = state.handle_mouse(&event, hit, HitId::new(1), 10);
        assert_eq!(result, MouseResult::Selected(4));
        assert_eq!(state.selected, Some(4));
    }

    #[test]
    fn table_state_second_click_activates() {
        let mut state = TableState::default();
        state.select(Some(4));

        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 5, 2);
        let hit = Some((HitId::new(1), HitRegion::Content, 4u64));
        let result = state.handle_mouse(&event, hit, HitId::new(1), 10);
        assert_eq!(result, MouseResult::Activated(4));
        assert_eq!(state.selected, Some(4));
    }

    #[test]
    fn table_state_click_wrong_id_ignored() {
        let mut state = TableState::default();
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 5, 2);
        let hit = Some((HitId::new(99), HitRegion::Content, 4u64));
        let result = state.handle_mouse(&event, hit, HitId::new(1), 10);
        assert_eq!(result, MouseResult::Ignored);
    }

    #[test]
    fn table_state_hover_updates() {
        let mut state = TableState::default();
        let event = MouseEvent::new(MouseEventKind::Moved, 5, 2);
        let hit = Some((HitId::new(1), HitRegion::Content, 3u64));
        let result = state.handle_mouse(&event, hit, HitId::new(1), 10);
        assert_eq!(result, MouseResult::HoverChanged);
        assert_eq!(state.hovered, Some(3));
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn table_state_hover_same_index_ignored() {
        let mut state = {
            let mut s = TableState::default();
            s.hovered = Some(3);
            s
        };
        let event = MouseEvent::new(MouseEventKind::Moved, 5, 2);
        let hit = Some((HitId::new(1), HitRegion::Content, 3u64));
        let result = state.handle_mouse(&event, hit, HitId::new(1), 10);
        assert_eq!(result, MouseResult::Ignored);
        assert_eq!(state.hovered, Some(3));
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn table_state_hover_clears() {
        let mut state = {
            let mut s = TableState::default();
            s.hovered = Some(5);
            s
        };
        let event = MouseEvent::new(MouseEventKind::Moved, 5, 2);
        // No hit (mouse moved off the table)
        let result = state.handle_mouse(&event, None, HitId::new(1), 10);
        assert_eq!(result, MouseResult::HoverChanged);
        assert_eq!(state.hovered, None);
    }

    #[test]
    fn table_state_hover_clear_when_already_none() {
        let mut state = TableState::default();
        let event = MouseEvent::new(MouseEventKind::Moved, 5, 2);
        let result = state.handle_mouse(&event, None, HitId::new(1), 10);
        assert_eq!(result, MouseResult::Ignored);
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn table_state_scroll_wheel_up() {
        let mut state = {
            let mut s = TableState::default();
            s.offset = 10;
            s
        };
        let event = MouseEvent::new(MouseEventKind::ScrollUp, 0, 0);
        let result = state.handle_mouse(&event, None, HitId::new(1), 20);
        assert_eq!(result, MouseResult::Scrolled);
        assert_eq!(state.offset, 7);
    }

    #[test]
    fn table_state_scroll_wheel_down() {
        let mut state = TableState::default();
        let event = MouseEvent::new(MouseEventKind::ScrollDown, 0, 0);
        let result = state.handle_mouse(&event, None, HitId::new(1), 20);
        assert_eq!(result, MouseResult::Scrolled);
        assert_eq!(state.offset, 3);
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn table_state_scroll_down_clamps() {
        let mut state = {
            let mut s = TableState::default();
            s.offset = 18;
            s
        };
        state.scroll_down(5, 20);
        assert_eq!(state.offset, 19);
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn table_state_scroll_up_clamps() {
        let mut state = {
            let mut s = TableState::default();
            s.offset = 1;
            s
        };
        state.scroll_up(5);
        assert_eq!(state.offset, 0);
    }

    // ============================================================================
    // Edge-Case Tests (bd-2rvwb)
    // ============================================================================

    #[test]
    fn row_with_fewer_cells_than_columns() {
        // Row has 1 cell but table declares 3 columns — extra columns should be empty
        let table = Table::new(
            [Row::new(["A"])],
            [
                Constraint::Fixed(3),
                Constraint::Fixed(3),
                Constraint::Fixed(3),
            ],
        );
        let area = Rect::new(0, 0, 12, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(12, 1, &mut pool);
        Widget::render(&table, area, &mut frame);

        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('A'));
        // Columns 2 and 3 should not contain data characters
        assert_ne!(cell_char(&frame.buffer, 4, 0), Some('A'));
    }

    #[test]
    fn column_spacing_zero() {
        // No gap between columns — cells should be adjacent
        let table = Table::new(
            [Row::new(["AB", "CD"])],
            [Constraint::Fixed(2), Constraint::Fixed(2)],
        )
        .column_spacing(0);

        let area = Rect::new(0, 0, 4, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 1, &mut pool);
        Widget::render(&table, area, &mut frame);

        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('A'));
        assert_eq!(cell_char(&frame.buffer, 1, 0), Some('B'));
        assert_eq!(cell_char(&frame.buffer, 2, 0), Some('C'));
        assert_eq!(cell_char(&frame.buffer, 3, 0), Some('D'));
    }

    #[test]
    fn render_with_nonzero_origin() {
        // Table rendered at offset position, not (0,0)
        let table = Table::new([Row::new(["X"])], [Constraint::Fixed(3)]);
        let area = Rect::new(5, 3, 3, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 6, &mut pool);
        Widget::render(&table, area, &mut frame);

        assert_eq!(cell_char(&frame.buffer, 5, 3), Some('X'));
        // Nothing at (0,0)
        assert_ne!(cell_char(&frame.buffer, 0, 0), Some('X'));
    }

    #[test]
    fn single_row_height_exceeds_area() {
        // Row is taller than the viewport — should be clipped via scissor
        let table = Table::new([Row::new(["T"]).height(10)], [Constraint::Fixed(3)]);
        let area = Rect::new(0, 0, 3, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 2, &mut pool);
        Widget::render(&table, area, &mut frame);

        // First line of the row should still render
        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('T'));
    }

    #[test]
    fn selection_and_hover_on_same_row() {
        // Both selected and hovered on same row — both styles should merge
        let selected_fg = PackedRgba::rgb(100, 0, 0);
        let hovered_fg = PackedRgba::rgb(0, 100, 0);
        let highlight_fg = PackedRgba::rgb(0, 0, 100);

        let mut theme = TableTheme::default();
        theme.row_selected = Style::new().fg(selected_fg);
        theme.row_hover = Style::new().fg(hovered_fg);

        let table = Table::new([Row::new(["X"])], [Constraint::Fixed(3)])
            .highlight_style(Style::new().fg(highlight_fg))
            .theme(theme);

        let area = Rect::new(0, 0, 3, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 1, &mut pool);
        let mut state = TableState {
            selected: Some(0),
            hovered: Some(0),
            ..Default::default()
        };

        StatefulWidget::render(&table, area, &mut frame, &mut state);
        // Highlight style wins (applied last in merge chain)
        assert_eq!(cell_fg(&frame.buffer, 0, 0), Some(highlight_fg));
    }

    #[test]
    fn alternating_row_styles() {
        // Even/odd rows should get different theme styles
        let even_fg = PackedRgba::rgb(10, 10, 10);
        let odd_fg = PackedRgba::rgb(20, 20, 20);
        let mut theme = TableTheme::default();
        theme.row = Style::new().fg(even_fg);
        theme.row_alt = Style::new().fg(odd_fg);

        let table = Table::new(
            [Row::new(["E"]), Row::new(["O"]), Row::new(["E2"])],
            [Constraint::Fixed(3)],
        )
        .theme(theme);

        let area = Rect::new(0, 0, 3, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 3, &mut pool);
        Widget::render(&table, area, &mut frame);

        // Row 0 is even, row 1 is odd, row 2 is even
        assert_eq!(cell_fg(&frame.buffer, 0, 0), Some(even_fg));
        assert_eq!(cell_fg(&frame.buffer, 0, 1), Some(odd_fg));
        assert_eq!(cell_fg(&frame.buffer, 0, 2), Some(even_fg));
    }

    #[test]
    fn scroll_up_from_zero_stays_zero() {
        let mut state = TableState::default();
        state.scroll_up(10);
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn scroll_down_with_zero_rows() {
        let mut state = TableState::default();
        state.scroll_down(5, 0);
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn scroll_down_with_single_row() {
        let mut state = TableState::default();
        state.scroll_down(5, 1);
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn mouse_click_on_row_exceeding_row_count() {
        // Hit data row index >= row_count should be ignored
        let mut state = TableState::default();
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 0, 0);
        let hit = Some((HitId::new(1), HitRegion::Content, 100u64));
        let result = state.handle_mouse(&event, hit, HitId::new(1), 5);
        assert_eq!(result, MouseResult::Ignored);
        assert_eq!(state.selected, None);
    }

    #[test]
    fn mouse_right_click_ignored() {
        let mut state = TableState::default();
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Right), 0, 0);
        let hit = Some((HitId::new(1), HitRegion::Content, 2u64));
        let result = state.handle_mouse(&event, hit, HitId::new(1), 5);
        assert_eq!(result, MouseResult::Ignored);
    }

    #[test]
    fn mouse_hover_on_row_exceeding_row_count() {
        let mut state = TableState::default();
        let event = MouseEvent::new(MouseEventKind::Moved, 0, 0);
        let hit = Some((HitId::new(1), HitRegion::Content, 100u64));
        let result = state.handle_mouse(&event, hit, HitId::new(1), 5);
        // Moves off widget, hover cleared (was None, stays None)
        assert_eq!(result, MouseResult::Ignored);
        assert_eq!(state.hovered, None);
    }

    #[test]
    fn select_deselect_resets_offset_then_reselect() {
        let mut state = TableState::default();
        state.offset = 15;
        state.select(Some(20));
        assert_eq!(state.selected, Some(20));
        assert_eq!(state.offset, 15); // offset not reset on select

        state.select(None);
        assert_eq!(state.offset, 0); // reset on deselect

        state.select(Some(3));
        assert_eq!(state.selected, Some(3));
        assert_eq!(state.offset, 0); // still 0 after reselect
    }

    #[test]
    fn offset_clamped_when_rows_empty() {
        let table = Table::new(Vec::<Row>::new(), [Constraint::Fixed(5)]);
        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        let mut state = TableState {
            offset: 999,
            ..Default::default()
        };
        StatefulWidget::render(&table, area, &mut frame, &mut state);
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn selection_clamps_when_rows_empty() {
        let table = Table::new(Vec::<Row>::new(), [Constraint::Fixed(5)]);
        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        let mut state = TableState {
            selected: Some(5),
            ..Default::default()
        };
        StatefulWidget::render(&table, area, &mut frame, &mut state);
        assert_eq!(state.selected, None);
    }

    #[test]
    fn header_with_bottom_margin_offsets_rows() {
        let header = Row::new(["H"]).bottom_margin(2);
        let table = Table::new([Row::new(["D"])], [Constraint::Fixed(3)]).header(header);

        let area = Rect::new(0, 0, 3, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 5, &mut pool);
        Widget::render(&table, area, &mut frame);

        // Header at y=0, margin of 2, data at y=3
        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('H'));
        assert_eq!(cell_char(&frame.buffer, 0, 3), Some('D'));
    }

    #[test]
    fn block_plus_header_fill_entire_area() {
        // Block takes 2 rows (top/bottom border), header takes 1 row — 3 rows total.
        // With area height=3, no data rows should render.
        let header = Row::new(["H"]);
        let table = Table::new([Row::new(["X"])], [Constraint::Fixed(3)])
            .block(Block::bordered())
            .header(header);

        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        Widget::render(&table, area, &mut frame);

        // Header should render at (1,1) inside the border
        assert_eq!(cell_char(&frame.buffer, 1, 1), Some('H'));
        // Data row "X" should NOT appear (no room)
        let data_rendered =
            (0..5).any(|x| (0..3).any(|y| cell_char(&frame.buffer, x, y) == Some('X')));
        assert!(!data_rendered);
    }

    #[test]
    fn min_constraint_measure() {
        let table = Table::new([Row::new(["AB"])], [Constraint::Min(10)]);
        let c = table.measure(Size::MAX);
        // Preferred width based on content, not the constraint minimum
        assert_eq!(c.preferred.width, 2);
        assert_eq!(c.preferred.height, 1);
    }

    #[test]
    fn percentage_constraint_render() {
        // Percentage constraints should not panic and produce reasonable layout
        let table = Table::new(
            [Row::new(["A", "B"])],
            [Constraint::Percentage(50.0), Constraint::Percentage(50.0)],
        );
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        Widget::render(&table, area, &mut frame);

        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('A'));
    }

    #[test]
    fn fit_content_constraint_measure() {
        let table = Table::new(
            [Row::new(["Hello", "World"])],
            [Constraint::FitContent, Constraint::FitContent],
        )
        .column_spacing(1);

        let c = table.measure(Size::MAX);
        // "Hello" = 5, "World" = 5, spacing = 1 → 11
        assert_eq!(c.preferred.width, 11);
    }

    #[test]
    fn measure_with_block_adds_overhead() {
        let table_no_block = Table::new([Row::new(["X"])], [Constraint::Fixed(3)]);
        let table_with_block =
            Table::new([Row::new(["X"])], [Constraint::Fixed(3)]).block(Block::bordered());

        let c_no = table_no_block.measure(Size::MAX);
        let c_with = table_with_block.measure(Size::MAX);

        // Block border adds 2 to width and 2 to height
        assert_eq!(c_with.preferred.width, c_no.preferred.width + 2);
        assert_eq!(c_with.preferred.height, c_no.preferred.height + 2);
    }

    #[test]
    fn variable_height_rows_selection_scrolls_down() {
        // Rows: height 1, 1, 5, 1, 1. Viewport=4 rows.
        // Select row 4 (past the tall row) should adjust offset.
        let rows = vec![
            Row::new(["A"]),
            Row::new(["B"]),
            Row::new(["C"]).height(5),
            Row::new(["D"]),
            Row::new(["E"]),
        ];
        let table = Table::new(rows, [Constraint::Fixed(5)]);
        let area = Rect::new(0, 0, 5, 4);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 4, &mut pool);
        let mut state = TableState {
            selected: Some(4),
            ..Default::default()
        };
        StatefulWidget::render(&table, area, &mut frame, &mut state);

        // Selection should be visible; offset adjusted
        assert!(state.offset > 0);
        assert_eq!(state.selected, Some(4));
    }

    #[test]
    fn many_rows_with_margins_viewport_clamping() {
        // 20 rows each with bottom_margin=1, viewport=5 lines.
        // Each row occupies 2 lines (1 content + 1 margin). Max 2 rows visible.
        let rows: Vec<Row> = (0..20)
            .map(|i| Row::new([format!("R{i}")]).bottom_margin(1))
            .collect();
        let table = Table::new(rows, [Constraint::Fixed(5)]);
        let area = Rect::new(0, 0, 5, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 5, &mut pool);
        let mut state = TableState {
            offset: 19,
            ..Default::default()
        };
        StatefulWidget::render(&table, area, &mut frame, &mut state);

        // Offset should be clamped back to fill viewport
        assert!(state.offset < 19);
    }

    #[test]
    fn render_area_width_one() {
        // Extremely narrow area — should not panic
        let table = Table::new([Row::new(["Hello"])], [Constraint::Fixed(5)]);
        let area = Rect::new(0, 0, 1, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        Widget::render(&table, area, &mut frame);

        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('H'));
    }

    #[test]
    fn render_area_height_one() {
        // Minimal height — should show first row
        let table = Table::new([Row::new(["A"]), Row::new(["B"])], [Constraint::Fixed(3)]);
        let area = Rect::new(0, 0, 3, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 1, &mut pool);
        Widget::render(&table, area, &mut frame);

        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('A'));
    }

    #[test]
    fn hit_regions_with_offset() {
        // When scrolled, hit data should still encode logical row index
        let table = Table::new(
            (0..10).map(|i| Row::new([format!("R{i}")])),
            [Constraint::Fixed(5)],
        )
        .hit_id(HitId::new(42));

        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(5, 3, &mut pool);
        let mut state = TableState {
            offset: 5,
            ..Default::default()
        };
        StatefulWidget::render(&table, area, &mut frame, &mut state);

        // Row at y=0 should be logical row 5
        let hit0 = frame.hit_test(2, 0);
        assert_eq!(hit0, Some((HitId::new(42), HitRegion::Content, 5)));

        let hit1 = frame.hit_test(2, 1);
        assert_eq!(hit1, Some((HitId::new(42), HitRegion::Content, 6)));
    }

    #[test]
    fn table_state_sort_defaults() {
        let state = TableState::default();
        assert_eq!(state.sort_column(), None);
        assert!(!state.sort_ascending());
        assert!(state.filter().is_empty());
    }

    #[test]
    fn table_state_set_sort_toggle() {
        let mut state = TableState::default();
        state.set_sort(Some(0), true);
        assert_eq!(state.sort_column(), Some(0));
        assert!(state.sort_ascending());

        // Toggle direction
        state.set_sort(Some(0), false);
        assert!(!state.sort_ascending());

        // Change column
        state.set_sort(Some(3), true);
        assert_eq!(state.sort_column(), Some(3));

        // Clear sort
        state.set_sort(None, false);
        assert_eq!(state.sort_column(), None);
    }

    #[test]
    fn table_persist_round_trip_preserves_hovered_none() {
        let mut state = TableState::default().with_persistence_id("t");
        state.select(Some(3));
        state.hovered = Some(7);
        state.offset = 2;

        let saved = state.save_state();
        state.restore_state(saved);

        // hovered is deliberately NOT persisted (transient state)
        assert_eq!(state.hovered, None);
        assert_eq!(state.selected, Some(3));
        assert_eq!(state.offset, 2);
    }

    #[test]
    fn undo_snapshot_clears_hovered() {
        let mut state = TableState::default();
        state.select(Some(2));
        state.hovered = Some(5);

        let snap = state.create_snapshot();

        // Modify
        state.select(Some(9));
        state.hovered = Some(8);

        // Restore
        assert!(state.restore_snapshot(&*snap));
        assert_eq!(state.selected, Some(2));
        // hovered is cleared on restore (not preserved in snapshot)
        assert_eq!(state.hovered, None);
    }

    #[test]
    fn wide_chars_in_render() {
        // CJK characters are 2 cells wide — should clip correctly.
        // Wide chars may use the grapheme pool, so we check the cell is populated.
        let table = Table::new([Row::new(["界界界"])], [Constraint::Fixed(4)]);
        let area = Rect::new(0, 0, 4, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 1, &mut pool);
        Widget::render(&table, area, &mut frame);

        // "界界界" needs 6 cells but only 4 available — first two wide chars fit.
        // The cell at (0,0) should have content (not empty).
        let cell = frame.buffer.get(0, 0).unwrap();
        assert!(
            !cell.content.is_empty(),
            "first cell should contain CJK content, not be empty"
        );
        // Cell at (1,0) should be a continuation marker for the wide char
        let cell1 = frame.buffer.get(1, 0).unwrap();
        assert!(
            cell1.content.is_continuation(),
            "second cell should be continuation of wide char"
        );
    }

    #[test]
    fn empty_row_cells() {
        // Row with empty strings — should render without panic
        let table = Table::new(
            [Row::new(["", "", ""])],
            [
                Constraint::Fixed(3),
                Constraint::Fixed(3),
                Constraint::Fixed(3),
            ],
        );
        let area = Rect::new(0, 0, 11, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(11, 1, &mut pool);
        Widget::render(&table, area, &mut frame);
        // Should not panic; cells empty
    }

    #[test]
    fn measure_with_many_rows_saturates() {
        // Height computation should use saturating arithmetic
        let rows: Vec<Row> = (0..10000).map(|_| Row::new(["X"]).height(100)).collect();
        let table = Table::new(rows, [Constraint::Fixed(3)]);
        let c = table.measure(Size::MAX);

        // Should not overflow — saturates at u16::MAX
        assert!(c.preferred.height > 0);
    }
}
