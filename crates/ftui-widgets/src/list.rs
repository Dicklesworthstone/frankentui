#![forbid(unsafe_code)]

//! List widget.
//!
//! A widget to display a list of items with selection support.

use crate::block::Block;
use crate::measurable::{MeasurableWidget, SizeConstraints};
use crate::mouse::MouseResult;
use crate::stateful::{StateKey, Stateful};
use crate::undo_support::{ListUndoExt, UndoSupport, UndoWidgetId};
use crate::{StatefulWidget, Widget, draw_text_span, draw_text_span_with_link, set_style_area};
use ftui_core::event::{KeyCode, KeyEvent, Modifiers, MouseButton, MouseEvent, MouseEventKind};
use ftui_core::geometry::{Rect, Size};
use ftui_render::frame::{Frame, HitId, HitRegion};
use ftui_style::Style;
use ftui_text::{Line, Span, Text as FtuiText, display_width};
use std::collections::BTreeSet;
#[cfg(feature = "tracing")]
use web_time::Instant;

type Text = FtuiText<'static>;

fn text_into_owned(text: FtuiText<'_>) -> FtuiText<'static> {
    FtuiText::from_lines(
        text.into_iter()
            .map(|line| Line::from_spans(line.into_iter().map(Span::into_owned))),
    )
}

/// A single item in a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem<'a> {
    content: Text,
    style: Style,
    marker: &'a str,
}

impl<'a> ListItem<'a> {
    /// Create a new list item with the given content.
    #[must_use]
    pub fn new<'t>(content: impl Into<FtuiText<'t>>) -> Self {
        Self {
            content: text_into_owned(content.into()),
            style: Style::default(),
            marker: "",
        }
    }

    /// Set the style for this list item.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set a prefix marker string for this item.
    #[must_use]
    pub fn marker(mut self, marker: &'a str) -> Self {
        self.marker = marker;
        self
    }
}

impl<'a> From<&'a str> for ListItem<'a> {
    fn from(s: &'a str) -> Self {
        Self::new(s)
    }
}

/// A widget to display a list of items.
#[derive(Debug, Clone, Default)]
pub struct List<'a> {
    block: Option<Block<'a>>,
    items: Vec<ListItem<'a>>,
    style: Style,
    highlight_style: Style,
    hover_style: Style,
    highlight_symbol: Option<&'a str>,
    /// Optional hit ID for mouse interaction.
    /// When set, each list item registers a hit region with the hit grid.
    hit_id: Option<HitId>,
}

impl<'a> List<'a> {
    /// Create a new list from the given items.
    #[must_use]
    pub fn new(items: impl IntoIterator<Item = impl Into<ListItem<'a>>>) -> Self {
        Self {
            block: None,
            items: items.into_iter().map(|i| i.into()).collect(),
            style: Style::default(),
            highlight_style: Style::default(),
            hover_style: Style::default(),
            highlight_symbol: None,
            hit_id: None,
        }
    }

    /// Wrap the list in a decorative block.
    #[must_use]
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// Set the base style for the list area.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set the style applied to the selected item.
    #[must_use]
    pub fn highlight_style(mut self, style: Style) -> Self {
        self.highlight_style = style;
        self
    }

    /// Set the style applied to the hovered item (mouse move).
    #[must_use]
    pub fn hover_style(mut self, style: Style) -> Self {
        self.hover_style = style;
        self
    }

    /// Set a symbol displayed before the selected item.
    #[must_use]
    pub fn highlight_symbol(mut self, symbol: &'a str) -> Self {
        self.highlight_symbol = Some(symbol);
        self
    }

    /// Set a hit ID for mouse interaction.
    ///
    /// When set, each list item will register a hit region with the frame's
    /// hit grid (if enabled). The hit data will be the item's index, allowing
    /// click handlers to determine which item was clicked.
    #[must_use]
    pub fn hit_id(mut self, id: HitId) -> Self {
        self.hit_id = Some(id);
        self
    }

    fn filtered_indices(&self, query: &str) -> Vec<usize> {
        let query = query.trim();
        if query.is_empty() {
            return (0..self.items.len()).collect();
        }
        let query_lower = query.to_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter_map(|(idx, item)| {
                // Optimization: check single-span content directly to avoid allocation
                // from to_plain_text().
                let line_text_cow;
                let line_text_ref = if let Some(line) = item.content.lines().first() {
                    if line.spans().len() == 1 {
                        &line.spans()[0].content
                    } else {
                        line_text_cow = std::borrow::Cow::Owned(line.to_plain_text());
                        &line_text_cow
                    }
                } else {
                    ""
                };

                let marker_matches = !item.marker.is_empty()
                    && crate::contains_ignore_case(item.marker, &query_lower);
                if marker_matches || crate::contains_ignore_case(line_text_ref, &query_lower) {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect()
    }

    fn apply_filtered_selection_guard(
        &self,
        state: &mut ListState,
        filtered: &[usize],
        force_select_first: bool,
    ) {
        if filtered.is_empty() {
            state.selected = None;
            state.hovered = None;
            state.offset = 0;
            if state.multi_select_enabled {
                state.multi_selected.clear();
            }
            return;
        }

        if let Some(selected) = state.selected {
            if filtered.binary_search(&selected).is_err() {
                state.selected = filtered.first().copied();
            }
        } else if force_select_first {
            state.selected = filtered.first().copied();
        }

        if state.multi_select_enabled {
            state.multi_selected.retain(|idx| filtered.contains(idx));
        }
    }

    fn move_selection_in_filtered(
        &self,
        state: &mut ListState,
        filtered: &[usize],
        direction: isize,
    ) -> bool {
        if filtered.is_empty() {
            if state.selected.is_some() {
                state.select(None);
                return true;
            }
            return false;
        }

        let max_pos = filtered.len().saturating_sub(1) as isize;

        let next_pos = if let Some(selected) = state.selected {
            // `filtered` is sorted by index (ascending), so we can use binary search
            // for O(log N) lookup instead of O(N) linear scan.
            let current_pos = filtered.binary_search(&selected).unwrap_or(0);
            (current_pos as isize + direction).clamp(0, max_pos) as usize
        } else if direction > 0 {
            0
        } else {
            max_pos as usize
        };

        let next_index = filtered[next_pos];

        if state.selected == Some(next_index) {
            return false;
        }

        state.selected = Some(next_index);
        if !state.multi_select_enabled {
            state.multi_selected.clear();
            state.multi_selected.insert(next_index);
        }
        state.scroll_into_view_requested = true;
        #[cfg(feature = "tracing")]
        state.log_selection_change("keyboard_move");
        true
    }

    /// Handle keyboard navigation and incremental filtering for this list.
    ///
    /// Supported keys:
    /// - Navigation: `Up`/`Down`, `k`/`j`
    /// - Incremental filter input: printable chars
    /// - Filter editing: `Backspace`, `Escape`
    /// - Multi-select toggle (when enabled): `Space`
    pub fn handle_key(&self, state: &mut ListState, key: &KeyEvent) -> bool {
        let nav_modifiers = key
            .modifiers
            .intersects(Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER);

        match key.code {
            KeyCode::Up if !nav_modifiers => {
                let filtered = self.filtered_indices(state.filter_query());
                self.move_selection_in_filtered(state, &filtered, -1)
            }
            KeyCode::Down if !nav_modifiers => {
                let filtered = self.filtered_indices(state.filter_query());
                self.move_selection_in_filtered(state, &filtered, 1)
            }
            KeyCode::Char('k') if !nav_modifiers => {
                let filtered = self.filtered_indices(state.filter_query());
                self.move_selection_in_filtered(state, &filtered, -1)
            }
            KeyCode::Char('j') if !nav_modifiers => {
                let filtered = self.filtered_indices(state.filter_query());
                self.move_selection_in_filtered(state, &filtered, 1)
            }
            KeyCode::Char(' ') if state.multi_select_enabled() => {
                if let Some(selected) = state.selected {
                    state.toggle_multi_selected(selected);
                    true
                } else {
                    false
                }
            }
            KeyCode::Backspace => {
                if state.filter_query.is_empty() {
                    return false;
                }
                state.filter_query.pop();
                state.offset = 0;
                state.scroll_into_view_requested = true;
                let filtered = self.filtered_indices(state.filter_query());
                self.apply_filtered_selection_guard(state, &filtered, true);
                #[cfg(feature = "tracing")]
                state.log_selection_change("filter_backspace");
                true
            }
            KeyCode::Escape => {
                if state.filter_query.is_empty() {
                    return false;
                }
                state.filter_query.clear();
                state.offset = 0;
                state.scroll_into_view_requested = true;
                let filtered = self.filtered_indices(state.filter_query());
                self.apply_filtered_selection_guard(state, &filtered, false);
                #[cfg(feature = "tracing")]
                state.log_selection_change("filter_clear");
                true
            }
            KeyCode::Char(ch)
                if !ch.is_control() && !key.ctrl() && !key.alt() && !key.super_key() =>
            {
                // Preserve uppercase input when Shift is held.
                state.filter_query.push(ch);
                state.offset = 0;
                state.scroll_into_view_requested = true;
                let filtered = self.filtered_indices(state.filter_query());
                self.apply_filtered_selection_guard(state, &filtered, true);
                #[cfg(feature = "tracing")]
                state.log_selection_change("filter_append");
                true
            }
            _ => false,
        }
    }
}

/// Mutable state for a [`List`] widget tracking selection and scroll offset.
#[derive(Debug, Clone)]
pub struct ListState {
    /// Unique ID for undo tracking.
    undo_id: UndoWidgetId,
    /// Index of the currently selected item, if any.
    pub selected: Option<usize>,
    /// Index of the currently hovered item, if any.
    pub hovered: Option<usize>,
    /// Scroll offset (first visible item index).
    pub offset: usize,
    /// Optional persistence ID for state saving/restoration.
    persistence_id: Option<String>,
    /// Whether to force the selected item into view on next render.
    scroll_into_view_requested: bool,
    /// Incremental filter query applied to items (case-insensitive).
    filter_query: String,
    /// Whether multi-select behavior is enabled.
    multi_select_enabled: bool,
    /// Set of selected indices when multi-select is enabled.
    multi_selected: BTreeSet<usize>,
}

impl Default for ListState {
    fn default() -> Self {
        Self {
            undo_id: UndoWidgetId::default(),
            selected: None,
            hovered: None,
            offset: 0,
            persistence_id: None,
            scroll_into_view_requested: true,
            filter_query: String::new(),
            multi_select_enabled: false,
            multi_selected: BTreeSet::new(),
        }
    }
}

impl ListState {
    /// Set the selected item index, or `None` to deselect.
    pub fn select(&mut self, index: Option<usize>) {
        self.selected = index;
        if index.is_none() {
            self.offset = 0;
            self.multi_selected.clear();
        } else if !self.multi_select_enabled
            && let Some(selected) = index
        {
            self.multi_selected.clear();
            self.multi_selected.insert(selected);
        }
        self.scroll_into_view_requested = true;
        #[cfg(feature = "tracing")]
        self.log_selection_change("select");
    }

    /// Return the currently selected item index.
    #[inline]
    #[must_use = "use the selected index (if any)"]
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Create a new ListState with a persistence ID for state saving.
    #[must_use]
    pub fn with_persistence_id(mut self, id: impl Into<String>) -> Self {
        self.persistence_id = Some(id.into());
        self
    }

    /// Get the persistence ID, if set.
    #[inline]
    #[must_use = "use the persistence id (if any)"]
    pub fn persistence_id(&self) -> Option<&str> {
        self.persistence_id.as_deref()
    }

    /// Enable or disable multi-select mode.
    pub fn set_multi_select(&mut self, enabled: bool) {
        if self.multi_select_enabled == enabled {
            return;
        }
        self.multi_select_enabled = enabled;
        if !enabled {
            self.multi_selected.clear();
            if let Some(selected) = self.selected {
                self.multi_selected.insert(selected);
            }
        }
    }

    /// Whether multi-select mode is enabled.
    #[must_use]
    pub const fn multi_select_enabled(&self) -> bool {
        self.multi_select_enabled
    }

    /// Current incremental filter query.
    #[must_use]
    pub fn filter_query(&self) -> &str {
        &self.filter_query
    }

    /// Replace the incremental filter query.
    pub fn set_filter_query(&mut self, query: impl Into<String>) {
        self.filter_query = query.into();
        self.offset = 0;
        self.scroll_into_view_requested = true;
    }

    /// Clear the current filter query.
    pub fn clear_filter_query(&mut self) {
        if !self.filter_query.is_empty() {
            self.filter_query.clear();
            self.offset = 0;
            self.scroll_into_view_requested = true;
        }
    }

    /// Number of selected rows (single or multi mode).
    #[must_use]
    pub fn selected_count(&self) -> usize {
        if self.multi_select_enabled {
            self.multi_selected.len()
        } else {
            usize::from(self.selected.is_some())
        }
    }

    /// Selected indices in multi-select mode.
    #[must_use]
    pub fn selected_indices(&self) -> &BTreeSet<usize> {
        &self.multi_selected
    }

    fn toggle_multi_selected(&mut self, index: usize) {
        if !self.multi_select_enabled {
            self.select(Some(index));
            return;
        }
        if !self.multi_selected.insert(index) {
            self.multi_selected.remove(&index);
        }
        self.selected = Some(index);
        self.scroll_into_view_requested = true;
        #[cfg(feature = "tracing")]
        self.log_selection_change("toggle_multi");
    }

    #[cfg(feature = "tracing")]
    fn log_selection_change(&self, action: &str) {
        tracing::debug!(
            message = "list.selection",
            action,
            selected = self.selected,
            selected_count = self.selected_count(),
            filter_active = !self.filter_query.trim().is_empty()
        );
    }

    /// Handle a mouse event for this list.
    ///
    /// # Hit data convention
    ///
    /// The hit data (`u64`) encodes the item index. When the list renders with
    /// a `hit_id`, each visible row registers `HitRegion::Content` with
    /// `data = item_index as u64`.
    ///
    /// # Arguments
    ///
    /// * `event` — the mouse event from the terminal
    /// * `hit` — result of `frame.hit_test(event.x, event.y)`, if available
    /// * `expected_id` — the `HitId` this list was rendered with
    /// * `item_count` — total number of items in the list
    pub fn handle_mouse(
        &mut self,
        event: &MouseEvent,
        hit: Option<(HitId, HitRegion, u64)>,
        expected_id: HitId,
        item_count: usize,
    ) -> MouseResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((id, HitRegion::Content, data)) = hit
                    && id == expected_id
                {
                    let index = data as usize;
                    if index < item_count {
                        if self.multi_select_enabled && event.modifiers.contains(Modifiers::CTRL) {
                            self.toggle_multi_selected(index);
                            return MouseResult::Selected(index);
                        }
                        if self.multi_select_enabled {
                            self.multi_selected.clear();
                            self.multi_selected.insert(index);
                        }
                        // Deterministic "double click": second click on the already-selected row activates.
                        if !self.multi_select_enabled && self.selected == Some(index) {
                            #[cfg(feature = "tracing")]
                            self.log_selection_change("activate");
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
                    if index < item_count {
                        let changed = self.hovered != Some(index);
                        self.hovered = Some(index);
                        return if changed {
                            MouseResult::HoverChanged
                        } else {
                            MouseResult::Ignored
                        };
                    }
                }

                // Mouse moved off the widget or to non-content region.
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
                self.scroll_down(3, item_count);
                MouseResult::Scrolled
            }
            _ => MouseResult::Ignored,
        }
    }

    /// Scroll the list up by the given number of lines.
    pub fn scroll_up(&mut self, lines: usize) {
        self.offset = self.offset.saturating_sub(lines);
    }

    /// Scroll the list down by the given number of lines.
    ///
    /// Clamps so that the last item can still appear at the top of the viewport.
    pub fn scroll_down(&mut self, lines: usize, item_count: usize) {
        self.offset = self
            .offset
            .saturating_add(lines)
            .min(item_count.saturating_sub(1));
    }

    /// Move selection to the next item.
    ///
    /// If nothing is selected, selects the first item. Clamps to the last item.
    pub fn select_next(&mut self, item_count: usize) {
        if item_count == 0 {
            return;
        }
        let next = match self.selected {
            Some(i) => (i + 1).min(item_count.saturating_sub(1)),
            None => 0,
        };
        self.selected = Some(next);
        if !self.multi_select_enabled {
            self.multi_selected.clear();
            self.multi_selected.insert(next);
        }
        self.scroll_into_view_requested = true;
        #[cfg(feature = "tracing")]
        self.log_selection_change("select_next");
    }

    /// Move selection to the previous item.
    ///
    /// If nothing is selected, selects the first item. Clamps to 0.
    pub fn select_previous(&mut self) {
        let prev = match self.selected {
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.selected = Some(prev);
        if !self.multi_select_enabled {
            self.multi_selected.clear();
            self.multi_selected.insert(prev);
        }
        self.scroll_into_view_requested = true;
        #[cfg(feature = "tracing")]
        self.log_selection_change("select_previous");
    }
}

// ============================================================================
// Stateful Persistence Implementation
// ============================================================================

/// Persistable state for a [`ListState`].
///
/// Contains the user-facing state that should survive sessions.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(
    feature = "state-persistence",
    derive(serde::Serialize, serde::Deserialize)
)]
pub struct ListPersistState {
    /// Selected item index.
    pub selected: Option<usize>,
    /// Scroll offset (first visible item).
    pub offset: usize,
    /// Incremental filter query.
    pub filter_query: String,
    /// Whether multi-select mode was enabled.
    pub multi_select_enabled: bool,
    /// Multi-selected indices when multi-select mode is enabled.
    pub multi_selected: Vec<usize>,
}

impl Stateful for ListState {
    type State = ListPersistState;

    fn state_key(&self) -> StateKey {
        StateKey::new("List", self.persistence_id.as_deref().unwrap_or("default"))
    }

    fn save_state(&self) -> ListPersistState {
        ListPersistState {
            selected: self.selected,
            offset: self.offset,
            filter_query: self.filter_query.clone(),
            multi_select_enabled: self.multi_select_enabled,
            multi_selected: self.multi_selected.iter().copied().collect(),
        }
    }

    fn restore_state(&mut self, state: ListPersistState) {
        self.selected = state.selected;
        self.hovered = None;
        self.offset = state.offset;
        self.filter_query = state.filter_query;
        self.multi_select_enabled = state.multi_select_enabled;
        self.multi_selected = state.multi_selected.into_iter().collect();
    }
}

impl<'a> StatefulWidget for List<'a> {
    type State = ListState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
        #[cfg(feature = "tracing")]
        let render_start = Instant::now();
        #[cfg(feature = "tracing")]
        let total_items = self.items.len();
        let filter_active = !state.filter_query.trim().is_empty();
        #[cfg(feature = "tracing")]
        let selected_count = state.selected_count();
        #[cfg(feature = "tracing")]
        let render_span = tracing::debug_span!(
            "list.render",
            total_items,
            visible_items = tracing::field::Empty,
            selected_count,
            filter_active,
            render_duration_us = tracing::field::Empty
        );
        #[cfg(feature = "tracing")]
        let _render_guard = render_span.enter();

        let list_area = match &self.block {
            Some(b) => {
                b.render(area, frame);
                b.inner(area)
            }
            None => area,
        };

        let mut rendered_visible_items = 0usize;

        if !list_area.is_empty() {
            // Apply base style
            set_style_area(&mut frame.buffer, list_area, self.style);

            if self.items.is_empty() {
                state.selected = None;
                state.hovered = None;
                state.offset = 0;
                state.multi_selected.clear();
                draw_text_span(
                    frame,
                    list_area.x,
                    list_area.y,
                    "No items",
                    self.style,
                    list_area.right(),
                );
            } else {
                // Clamp selection/hover to item bounds before applying filters.
                if let Some(selected) = state.selected
                    && selected >= self.items.len()
                {
                    state.selected = Some(self.items.len().saturating_sub(1));
                }
                if let Some(hovered) = state.hovered
                    && hovered >= self.items.len()
                {
                    state.hovered = None;
                }

                let filtered_indices = self.filtered_indices(state.filter_query());
                self.apply_filtered_selection_guard(state, &filtered_indices, filter_active);

                if filtered_indices.is_empty() {
                    draw_text_span(
                        frame,
                        list_area.x,
                        list_area.y,
                        "No matches",
                        self.style,
                        list_area.right(),
                    );
                } else {
                    let list_height = list_area.height as usize;
                    let max_offset = filtered_indices.len().saturating_sub(list_height.max(1));
                    state.offset = state.offset.min(max_offset);

                    if let Some(hovered) = state.hovered
                        && !filtered_indices.contains(&hovered)
                    {
                        state.hovered = None;
                    }

                    // Ensure visible range includes selected item.
                    if state.scroll_into_view_requested {
                        if let Some(selected) = state.selected
                            && let Some(selected_pos) =
                                filtered_indices.binary_search(&selected).ok()
                        {
                            if selected_pos >= state.offset + list_height {
                                state.offset = selected_pos - list_height + 1;
                            } else if selected_pos < state.offset {
                                state.offset = selected_pos;
                            }
                        }
                        state.scroll_into_view_requested = false;
                    }

                    for (row, item_index) in filtered_indices
                        .iter()
                        .skip(state.offset)
                        .take(list_height)
                        .enumerate()
                    {
                        let i = *item_index;
                        let item = &self.items[i];
                        let y = list_area.y.saturating_add(row as u16);
                        if y >= list_area.bottom() {
                            break;
                        }
                        let is_selected = state.selected == Some(i)
                            || (state.multi_select_enabled && state.multi_selected.contains(&i));
                        let is_hovered = state.hovered == Some(i);

                        // Determine style: merge highlight on top of item style so
                        // unset highlight properties inherit from the item.
                        let mut item_style = if is_hovered {
                            self.hover_style.merge(&item.style)
                        } else {
                            item.style
                        };
                        if is_selected {
                            item_style = self.highlight_style.merge(&item_style);
                        }

                        // Apply item background style to the whole row
                        let row_area = Rect::new(list_area.x, y, list_area.width, 1);
                        set_style_area(&mut frame.buffer, row_area, item_style);

                        // Determine symbol
                        let symbol = if is_selected {
                            self.highlight_symbol.unwrap_or(item.marker)
                        } else {
                            item.marker
                        };

                        let mut x = list_area.x;

                        // Draw symbol if present
                        if !symbol.is_empty() {
                            x = draw_text_span(frame, x, y, symbol, item_style, list_area.right());
                            // Add a space after symbol
                            x = draw_text_span(frame, x, y, " ", item_style, list_area.right());
                        }

                        // Draw content
                        // Note: List items are currently single-line for simplicity in v1
                        if let Some(line) = item.content.lines().first() {
                            for span in line.spans() {
                                let span_style = match span.style {
                                    Some(s) => s.merge(&item_style),
                                    None => item_style,
                                };
                                x = draw_text_span_with_link(
                                    frame,
                                    x,
                                    y,
                                    &span.content,
                                    span_style,
                                    list_area.right(),
                                    span.link.as_deref(),
                                );
                                if x >= list_area.right() {
                                    break;
                                }
                            }
                        }

                        // Register hit region for this item (if hit testing enabled)
                        if let Some(id) = self.hit_id {
                            frame.register_hit(row_area, id, HitRegion::Content, i as u64);
                        }

                        rendered_visible_items = rendered_visible_items.saturating_add(1);
                    }

                    if filtered_indices.len() > list_height && list_area.width > 0 {
                        let indicator_x = list_area.right().saturating_sub(1);
                        if state.offset > 0 {
                            draw_text_span(
                                frame,
                                indicator_x,
                                list_area.y,
                                "↑",
                                self.style,
                                list_area.right(),
                            );
                        }
                        if state.offset + list_height < filtered_indices.len() {
                            draw_text_span(
                                frame,
                                indicator_x,
                                list_area.bottom().saturating_sub(1),
                                "↓",
                                self.style,
                                list_area.right(),
                            );
                        }
                    }
                }
            }
        }

        #[cfg(feature = "tracing")]
        {
            let elapsed_us = render_start.elapsed().as_micros() as u64;
            render_span.record("visible_items", rendered_visible_items);
            render_span.record("render_duration_us", elapsed_us);
            tracing::debug!(
                message = "list.metrics",
                total_items,
                visible_items = rendered_visible_items,
                selected_count = state.selected_count(),
                filter_active,
                list_render_duration_us = elapsed_us
            );
        }
    }
}

impl<'a> Widget for List<'a> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        let mut state = ListState::default();
        StatefulWidget::render(self, area, frame, &mut state);
    }
}

impl MeasurableWidget for ListItem<'_> {
    fn measure(&self, _available: Size) -> SizeConstraints {
        // ListItem is a single line of text with optional marker
        let marker_width = display_width(self.marker) as u16;
        let space_after_marker = if self.marker.is_empty() { 0u16 } else { 1 };

        // Get text width from the first line (List currently renders only first line)
        let text_width = self
            .content
            .lines()
            .first()
            .map(|line| line.width())
            .unwrap_or(0)
            .min(u16::MAX as usize) as u16;

        let total_width = marker_width
            .saturating_add(space_after_marker)
            .saturating_add(text_width);

        // ListItem is always 1 line tall
        SizeConstraints::exact(Size::new(total_width, 1))
    }

    fn has_intrinsic_size(&self) -> bool {
        true
    }
}

impl MeasurableWidget for List<'_> {
    fn measure(&self, available: Size) -> SizeConstraints {
        // Get block chrome if present
        let (chrome_width, chrome_height) = self
            .block
            .as_ref()
            .map(|b| b.chrome_size())
            .unwrap_or((0, 0));

        if self.items.is_empty() {
            // Empty list: just the chrome
            return SizeConstraints {
                min: Size::new(chrome_width, chrome_height),
                preferred: Size::new(chrome_width, chrome_height),
                max: None,
            };
        }

        // Calculate inner available space
        let inner_available = Size::new(
            available.width.saturating_sub(chrome_width),
            available.height.saturating_sub(chrome_height),
        );

        // Measure all items
        let mut max_width: u16 = 0;
        let mut total_height: u16 = 0;

        for item in &self.items {
            let item_constraints = item.measure(inner_available);
            max_width = max_width.max(item_constraints.preferred.width);
            total_height = total_height.saturating_add(item_constraints.preferred.height);
        }

        // Add highlight symbol width if present
        if let Some(symbol) = self.highlight_symbol {
            let symbol_width = display_width(symbol) as u16 + 1; // +1 for space
            max_width = max_width.saturating_add(symbol_width);
        }

        // Add chrome
        let preferred_width = max_width.saturating_add(chrome_width);
        let preferred_height = total_height.saturating_add(chrome_height);

        // Minimum is chrome + 1 item height (can scroll)
        let min_height = chrome_height.saturating_add(1.min(total_height));

        SizeConstraints {
            min: Size::new(chrome_width, min_height),
            preferred: Size::new(preferred_width, preferred_height),
            max: None, // Lists can scroll, so no max
        }
    }

    fn has_intrinsic_size(&self) -> bool {
        !self.items.is_empty()
    }
}

// ============================================================================
// Undo Support Implementation
// ============================================================================

/// Snapshot of ListState for undo.
#[derive(Debug, Clone)]
pub struct ListStateSnapshot {
    selected: Option<usize>,
    offset: usize,
    filter_query: String,
    multi_select_enabled: bool,
    multi_selected: Vec<usize>,
}

impl UndoSupport for ListState {
    fn undo_widget_id(&self) -> UndoWidgetId {
        self.undo_id
    }

    fn create_snapshot(&self) -> Box<dyn std::any::Any + Send> {
        Box::new(ListStateSnapshot {
            selected: self.selected,
            offset: self.offset,
            filter_query: self.filter_query.clone(),
            multi_select_enabled: self.multi_select_enabled,
            multi_selected: self.multi_selected.iter().copied().collect(),
        })
    }

    fn restore_snapshot(&mut self, snapshot: &dyn std::any::Any) -> bool {
        if let Some(snap) = snapshot.downcast_ref::<ListStateSnapshot>() {
            self.selected = snap.selected;
            self.hovered = None;
            self.offset = snap.offset;
            self.filter_query = snap.filter_query.clone();
            self.multi_select_enabled = snap.multi_select_enabled;
            self.multi_selected = snap.multi_selected.iter().copied().collect();
            true
        } else {
            false
        }
    }
}

impl ListUndoExt for ListState {
    fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    fn set_selected_index(&mut self, index: Option<usize>) {
        self.selected = index;
        if index.is_none() {
            self.offset = 0;
            self.multi_selected.clear();
        } else if !self.multi_select_enabled
            && let Some(selected) = index
        {
            self.multi_selected.clear();
            self.multi_selected.insert(selected);
        }
    }
}

impl ListState {
    /// Get the undo widget ID.
    ///
    /// This can be used to associate undo commands with this state instance.
    #[must_use]
    pub fn undo_id(&self) -> UndoWidgetId {
        self.undo_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::event::{KeyCode, KeyEvent};
    use ftui_render::grapheme_pool::GraphemePool;
    #[cfg(feature = "tracing")]
    use std::sync::{Arc, Mutex};
    #[cfg(feature = "tracing")]
    use tracing::Subscriber;
    #[cfg(feature = "tracing")]
    use tracing_subscriber::Layer;
    #[cfg(feature = "tracing")]
    use tracing_subscriber::layer::{Context, SubscriberExt};

    fn row_text(frame: &Frame, y: u16) -> String {
        let width = frame.buffer.width();
        let mut actual = String::new();
        for x in 0..width {
            let ch = frame
                .buffer
                .get(x, y)
                .and_then(|cell| cell.content.as_char())
                .unwrap_or(' ');
            actual.push(ch);
        }
        actual.trim().to_string()
    }

    #[cfg(feature = "tracing")]
    #[derive(Debug, Default)]
    struct ListTraceState {
        list_render_seen: bool,
        has_total_items_field: bool,
        has_visible_items_field: bool,
        has_selected_count_field: bool,
        has_filter_active_field: bool,
        render_duration_recorded: bool,
        selection_events: usize,
    }

    #[cfg(feature = "tracing")]
    struct ListTraceCapture {
        state: Arc<Mutex<ListTraceState>>,
    }

    #[cfg(feature = "tracing")]
    impl<S> Layer<S> for ListTraceCapture
    where
        S: Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
    {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            _id: &tracing::Id,
            _ctx: Context<'_, S>,
        ) {
            if attrs.metadata().name() != "list.render" {
                return;
            }
            let fields = attrs.metadata().fields();
            let mut state = self.state.lock().expect("list trace state lock");
            state.list_render_seen = true;
            state.has_total_items_field |= fields.field("total_items").is_some();
            state.has_visible_items_field |= fields.field("visible_items").is_some();
            state.has_selected_count_field |= fields.field("selected_count").is_some();
            state.has_filter_active_field |= fields.field("filter_active").is_some();
        }

        fn on_record(
            &self,
            id: &tracing::Id,
            values: &tracing::span::Record<'_>,
            ctx: Context<'_, S>,
        ) {
            let Some(span) = ctx.span(id) else {
                return;
            };
            if span.metadata().name() != "list.render" {
                return;
            }
            struct DurationVisitor {
                saw_duration: bool,
            }
            impl tracing::field::Visit for DurationVisitor {
                fn record_u64(&mut self, field: &tracing::field::Field, _value: u64) {
                    if field.name() == "render_duration_us" {
                        self.saw_duration = true;
                    }
                }

                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    _value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "render_duration_us" {
                        self.saw_duration = true;
                    }
                }
            }
            let mut visitor = DurationVisitor {
                saw_duration: false,
            };
            values.record(&mut visitor);
            if visitor.saw_duration {
                self.state
                    .lock()
                    .expect("list trace state lock")
                    .render_duration_recorded = true;
            }
        }

        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            struct MessageVisitor {
                message: Option<String>,
            }
            impl tracing::field::Visit for MessageVisitor {
                fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                    if field.name() == "message" {
                        self.message = Some(value.to_owned());
                    }
                }

                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message" {
                        self.message = Some(format!("{value:?}").trim_matches('"').to_owned());
                    }
                }
            }
            let mut visitor = MessageVisitor { message: None };
            event.record(&mut visitor);
            if visitor.message.as_deref() == Some("list.selection") {
                let mut state = self.state.lock().expect("list trace state lock");
                state.selection_events = state.selection_events.saturating_add(1);
            }
        }
    }

    #[test]
    fn render_empty_list() {
        let list = List::new(Vec::<ListItem>::new());
        let area = Rect::new(0, 0, 10, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        Widget::render(&list, area, &mut frame);
    }

    #[test]
    fn render_simple_list() {
        let items = vec![
            ListItem::new("Item A"),
            ListItem::new("Item B"),
            ListItem::new("Item C"),
        ];
        let list = List::new(items);
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);
        let mut state = ListState::default();
        StatefulWidget::render(&list, area, &mut frame, &mut state);

        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('I'));
        assert_eq!(frame.buffer.get(5, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(frame.buffer.get(5, 1).unwrap().content.as_char(), Some('B'));
        assert_eq!(frame.buffer.get(5, 2).unwrap().content.as_char(), Some('C'));
    }

    #[test]
    fn list_state_select() {
        let mut state = ListState::default();
        assert_eq!(state.selected(), None);

        state.select(Some(2));
        assert_eq!(state.selected(), Some(2));

        state.select(None);
        assert_eq!(state.selected(), None);
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn list_scrolls_to_selected() {
        let items: Vec<ListItem> = (0..10)
            .map(|i| ListItem::new(format!("Item {i}")))
            .collect();
        let list = List::new(items);
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);
        let mut state = ListState::default();
        state.select(Some(5));

        StatefulWidget::render(&list, area, &mut frame, &mut state);
        // offset should have been adjusted so item 5 is visible
        assert!(state.offset <= 5);
        assert!(state.offset + 3 > 5);
    }

    #[test]
    fn list_clamps_selection() {
        let items = vec![ListItem::new("A"), ListItem::new("B")];
        let list = List::new(items);
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);
        let mut state = ListState::default();
        state.select(Some(10)); // out of bounds

        StatefulWidget::render(&list, area, &mut frame, &mut state);
        // should clamp to last item
        assert_eq!(state.selected(), Some(1));
    }

    #[test]
    fn render_list_with_highlight_symbol() {
        let items = vec![ListItem::new("A"), ListItem::new("B")];
        let list = List::new(items).highlight_symbol(">");
        let area = Rect::new(0, 0, 10, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 2, &mut pool);
        let mut state = ListState::default();
        state.select(Some(0));

        StatefulWidget::render(&list, area, &mut frame, &mut state);
        // First item should have ">" symbol
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('>'));
    }

    #[test]
    fn render_zero_area() {
        let list = List::new(vec![ListItem::new("A")]);
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        let mut state = ListState::default();
        StatefulWidget::render(&list, area, &mut frame, &mut state);
    }

    #[test]
    fn list_item_from_str() {
        let item: ListItem = "hello".into();
        assert_eq!(
            item.content.lines().first().unwrap().to_plain_text(),
            "hello"
        );
        assert_eq!(item.marker, "");
    }

    #[test]
    fn list_item_with_marker() {
        let items = vec![
            ListItem::new("A").marker("•"),
            ListItem::new("B").marker("•"),
        ];
        let list = List::new(items);
        let area = Rect::new(0, 0, 10, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 2, &mut pool);
        let mut state = ListState::default();
        StatefulWidget::render(&list, area, &mut frame, &mut state);

        // Marker should be rendered at the start
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('•'));
        assert_eq!(frame.buffer.get(0, 1).unwrap().content.as_char(), Some('•'));
    }

    #[test]
    fn list_state_deselect_resets_offset() {
        let mut state = ListState {
            offset: 5,
            ..Default::default()
        };
        state.select(Some(10));
        assert_eq!(state.offset, 5); // select doesn't reset offset

        state.select(None);
        assert_eq!(state.offset, 0); // deselect resets offset
    }

    #[test]
    fn list_scrolls_up_when_selection_above_viewport() {
        let items: Vec<ListItem> = (0..10)
            .map(|i| ListItem::new(format!("Item {i}")))
            .collect();
        let list = List::new(items);
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);
        let mut state = ListState::default();

        // First scroll down
        state.select(Some(8));
        StatefulWidget::render(&list, area, &mut frame, &mut state);
        assert!(state.offset > 0);

        // Now select item 0 - should scroll back up
        state.select(Some(0));
        StatefulWidget::render(&list, area, &mut frame, &mut state);
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn list_clamps_offset_to_fill_viewport_on_resize() {
        let items: Vec<ListItem> = (0..10)
            .map(|i| ListItem::new(format!("Item {i}")))
            .collect();
        let list = List::new(items);

        let mut pool = GraphemePool::new();
        let mut state = ListState {
            offset: 7,
            ..Default::default()
        };

        // Small viewport: show 7, 8, 9.
        let area_small = Rect::new(0, 0, 10, 3);
        let mut frame_small = Frame::new(10, 3, &mut pool);
        StatefulWidget::render(&list, area_small, &mut frame_small, &mut state);
        assert_eq!(state.offset, 7);
        assert!(row_text(&frame_small, 0).starts_with("Item 7"));
        assert!(row_text(&frame_small, 2).starts_with("Item 9"));

        // Larger viewport: offset should pull back to fill the viewport (5..9).
        let area_large = Rect::new(0, 0, 10, 5);
        let mut frame_large = Frame::new(10, 5, &mut pool);
        StatefulWidget::render(&list, area_large, &mut frame_large, &mut state);
        assert_eq!(state.offset, 5);
        assert!(row_text(&frame_large, 0).starts_with("Item 5"));
        assert!(row_text(&frame_large, 4).starts_with("Item 9"));
    }

    #[test]
    fn render_list_more_items_than_viewport() {
        let items: Vec<ListItem> = (0..20).map(|i| ListItem::new(format!("{i}"))).collect();
        let list = List::new(items);
        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        let mut state = ListState::default();
        StatefulWidget::render(&list, area, &mut frame, &mut state);

        // Only first 3 should render
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('0'));
        assert_eq!(frame.buffer.get(0, 1).unwrap().content.as_char(), Some('1'));
        assert_eq!(frame.buffer.get(0, 2).unwrap().content.as_char(), Some('2'));
    }

    #[test]
    fn widget_render_uses_default_state() {
        let items = vec![ListItem::new("X")];
        let list = List::new(items);
        let area = Rect::new(0, 0, 5, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        // Using Widget trait (not StatefulWidget)
        Widget::render(&list, area, &mut frame);
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('X'));
    }

    #[test]
    fn list_registers_hit_regions() {
        let items = vec![ListItem::new("A"), ListItem::new("B"), ListItem::new("C")];
        let list = List::new(items).hit_id(HitId::new(42));
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(10, 3, &mut pool);
        let mut state = ListState::default();
        StatefulWidget::render(&list, area, &mut frame, &mut state);

        // Each row should have a hit region with the item index as data
        let hit0 = frame.hit_test(5, 0);
        let hit1 = frame.hit_test(5, 1);
        let hit2 = frame.hit_test(5, 2);

        assert_eq!(hit0, Some((HitId::new(42), HitRegion::Content, 0)));
        assert_eq!(hit1, Some((HitId::new(42), HitRegion::Content, 1)));
        assert_eq!(hit2, Some((HitId::new(42), HitRegion::Content, 2)));
    }

    #[test]
    fn list_no_hit_without_hit_id() {
        let items = vec![ListItem::new("A")];
        let list = List::new(items); // No hit_id set
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(10, 1, &mut pool);
        let mut state = ListState::default();
        StatefulWidget::render(&list, area, &mut frame, &mut state);

        // No hit region should be registered
        assert!(frame.hit_test(5, 0).is_none());
    }

    #[test]
    fn list_no_hit_without_hit_grid() {
        let items = vec![ListItem::new("A")];
        let list = List::new(items).hit_id(HitId::new(1));
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool); // No hit grid
        let mut state = ListState::default();
        StatefulWidget::render(&list, area, &mut frame, &mut state);

        // hit_test returns None when no hit grid
        assert!(frame.hit_test(5, 0).is_none());
    }

    // --- MeasurableWidget tests ---

    use crate::MeasurableWidget;
    use ftui_core::geometry::Size;

    #[test]
    fn list_item_measure_simple() {
        let item = ListItem::new("Hello"); // 5 chars
        let constraints = item.measure(Size::MAX);

        assert_eq!(constraints.preferred, Size::new(5, 1));
        assert_eq!(constraints.min, Size::new(5, 1));
        assert_eq!(constraints.max, Some(Size::new(5, 1)));
    }

    #[test]
    fn list_item_measure_with_marker() {
        let item = ListItem::new("Hi").marker("•"); // • + space + Hi = 1 + 1 + 2 = 4
        let constraints = item.measure(Size::MAX);

        assert_eq!(constraints.preferred.width, 4);
        assert_eq!(constraints.preferred.height, 1);
    }

    #[test]
    fn list_item_has_intrinsic_size() {
        let item = ListItem::new("test");
        assert!(item.has_intrinsic_size());
    }

    #[test]
    fn list_measure_empty() {
        let list = List::new(Vec::<ListItem>::new());
        let constraints = list.measure(Size::MAX);

        assert_eq!(constraints.preferred, Size::new(0, 0));
        assert!(!list.has_intrinsic_size());
    }

    #[test]
    fn list_measure_single_item() {
        let items = vec![ListItem::new("Hello")]; // 5 chars, 1 line
        let list = List::new(items);
        let constraints = list.measure(Size::MAX);

        assert_eq!(constraints.preferred, Size::new(5, 1));
        assert_eq!(constraints.min.height, 1);
    }

    #[test]
    fn list_measure_multiple_items() {
        let items = vec![
            ListItem::new("Short"),      // 5 chars
            ListItem::new("LongerItem"), // 10 chars
            ListItem::new("Tiny"),       // 4 chars
        ];
        let list = List::new(items);
        let constraints = list.measure(Size::MAX);

        // Width is max of all items = 10
        assert_eq!(constraints.preferred.width, 10);
        // Height is sum of all items = 3
        assert_eq!(constraints.preferred.height, 3);
    }

    #[test]
    fn list_measure_with_block() {
        let block = crate::block::Block::bordered(); // 4x4 chrome (borders + padding)
        let items = vec![ListItem::new("Hi")]; // 2 chars, 1 line
        let list = List::new(items).block(block);
        let constraints = list.measure(Size::MAX);

        // 2 (text) + 4 (chrome) = 6 width
        // 1 (line) + 4 (chrome) = 5 height
        assert_eq!(constraints.preferred, Size::new(6, 5));
    }

    #[test]
    fn list_measure_with_highlight_symbol() {
        let items = vec![ListItem::new("Item")]; // 4 chars
        let list = List::new(items).highlight_symbol(">"); // 1 char + space = 2

        let constraints = list.measure(Size::MAX);

        // 4 (text) + 2 (symbol + space) = 6
        assert_eq!(constraints.preferred.width, 6);
    }

    #[test]
    fn list_has_intrinsic_size() {
        let items = vec![ListItem::new("X")];
        let list = List::new(items);
        assert!(list.has_intrinsic_size());
    }

    #[test]
    fn list_min_height_is_one_row() {
        let items: Vec<ListItem> = (0..100)
            .map(|i| ListItem::new(format!("Item {i}")))
            .collect();
        let list = List::new(items);
        let constraints = list.measure(Size::MAX);

        // Min height should be 1 (can scroll to see rest)
        assert_eq!(constraints.min.height, 1);
        // Preferred height is all items
        assert_eq!(constraints.preferred.height, 100);
    }

    #[test]
    fn list_measure_is_pure() {
        let items = vec![ListItem::new("Test")];
        let list = List::new(items);
        let a = list.measure(Size::new(100, 50));
        let b = list.measure(Size::new(100, 50));
        assert_eq!(a, b);
    }

    // --- Undo Support tests ---

    #[test]
    fn list_state_undo_id_is_stable() {
        let state = ListState::default();
        let id1 = state.undo_id();
        let id2 = state.undo_id();
        assert_eq!(id1, id2);
    }

    #[test]
    fn list_state_undo_id_unique_per_instance() {
        let state1 = ListState::default();
        let state2 = ListState::default();
        assert_ne!(state1.undo_id(), state2.undo_id());
    }

    #[test]
    fn list_state_snapshot_and_restore() {
        let mut state = ListState::default();
        state.select(Some(5));
        state.offset = 3;

        let snapshot = state.create_snapshot();

        // Modify state
        state.select(Some(10));
        state.offset = 8;
        assert_eq!(state.selected(), Some(10));
        assert_eq!(state.offset, 8);

        // Restore
        assert!(state.restore_snapshot(snapshot.as_ref()));
        assert_eq!(state.selected(), Some(5));
        assert_eq!(state.offset, 3);
    }

    #[test]
    fn list_state_undo_ext_methods() {
        let mut state = ListState::default();
        assert_eq!(state.selected_index(), None);

        state.set_selected_index(Some(3));
        assert_eq!(state.selected_index(), Some(3));

        state.set_selected_index(None);
        assert_eq!(state.selected_index(), None);
        assert_eq!(state.offset, 0); // reset on deselect
    }

    // --- Stateful Persistence tests ---

    use crate::stateful::Stateful;

    #[test]
    fn list_state_with_persistence_id() {
        let state = ListState::default().with_persistence_id("sidebar-menu");
        assert_eq!(state.persistence_id(), Some("sidebar-menu"));
    }

    #[test]
    fn list_state_default_no_persistence_id() {
        let state = ListState::default();
        assert_eq!(state.persistence_id(), None);
    }

    #[test]
    fn list_state_save_restore_round_trip() {
        let mut state = ListState::default().with_persistence_id("test");
        state.select(Some(7));
        state.offset = 4;

        let saved = state.save_state();
        assert_eq!(saved.selected, Some(7));
        assert_eq!(saved.offset, 4);

        // Reset state
        state.select(None);
        assert_eq!(state.selected, None);
        assert_eq!(state.offset, 0);

        // Restore
        state.restore_state(saved);
        assert_eq!(state.selected, Some(7));
        assert_eq!(state.offset, 4);
    }

    #[test]
    fn list_state_key_uses_persistence_id() {
        let state = ListState::default().with_persistence_id("file-browser");
        let key = state.state_key();
        assert_eq!(key.widget_type, "List");
        assert_eq!(key.instance_id, "file-browser");
    }

    #[test]
    fn list_state_key_default_when_no_id() {
        let state = ListState::default();
        let key = state.state_key();
        assert_eq!(key.widget_type, "List");
        assert_eq!(key.instance_id, "default");
    }

    #[test]
    fn list_persist_state_default() {
        let persist = ListPersistState::default();
        assert_eq!(persist.selected, None);
        assert_eq!(persist.offset, 0);
    }

    // --- Mouse handling tests ---

    use crate::mouse::MouseResult;
    use ftui_core::event::{MouseButton, MouseEvent, MouseEventKind};

    #[test]
    fn list_state_click_selects() {
        let mut state = ListState::default();
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 5, 2);
        let hit = Some((HitId::new(1), HitRegion::Content, 3u64));
        let result = state.handle_mouse(&event, hit, HitId::new(1), 10);
        assert_eq!(result, MouseResult::Selected(3));
        assert_eq!(state.selected(), Some(3));
    }

    #[test]
    fn list_state_click_wrong_id_ignored() {
        let mut state = ListState::default();
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 5, 2);
        let hit = Some((HitId::new(99), HitRegion::Content, 3u64));
        let result = state.handle_mouse(&event, hit, HitId::new(1), 10);
        assert_eq!(result, MouseResult::Ignored);
        assert_eq!(state.selected(), None);
    }

    #[test]
    fn list_state_click_out_of_range() {
        let mut state = ListState::default();
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 5, 2);
        let hit = Some((HitId::new(1), HitRegion::Content, 15u64));
        let result = state.handle_mouse(&event, hit, HitId::new(1), 10);
        assert_eq!(result, MouseResult::Ignored);
        assert_eq!(state.selected(), None);
    }

    #[test]
    fn list_state_click_no_hit_ignored() {
        let mut state = ListState::default();
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 5, 2);
        let result = state.handle_mouse(&event, None, HitId::new(1), 10);
        assert_eq!(result, MouseResult::Ignored);
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn list_state_scroll_up() {
        let mut state = {
            let mut s = ListState::default();
            s.offset = 10;
            s
        };
        state.scroll_up(3);
        assert_eq!(state.offset, 7);
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn list_state_scroll_up_clamps_to_zero() {
        let mut state = {
            let mut s = ListState::default();
            s.offset = 1;
            s
        };
        state.scroll_up(5);
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn list_state_scroll_down() {
        let mut state = ListState::default();
        state.scroll_down(3, 20);
        assert_eq!(state.offset, 3);
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn list_state_scroll_down_clamps() {
        let mut state = ListState::default();
        state.offset = 18;
        state.scroll_down(5, 20);
        assert_eq!(state.offset, 19); // item_count - 1
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn list_state_scroll_wheel_up() {
        let mut state = {
            let mut s = ListState::default();
            s.offset = 10;
            s
        };
        let event = MouseEvent::new(MouseEventKind::ScrollUp, 0, 0);
        let result = state.handle_mouse(&event, None, HitId::new(1), 20);
        assert_eq!(result, MouseResult::Scrolled);
        assert_eq!(state.offset, 7);
    }

    #[test]
    fn list_state_scroll_wheel_down() {
        let mut state = ListState::default();
        let event = MouseEvent::new(MouseEventKind::ScrollDown, 0, 0);
        let result = state.handle_mouse(&event, None, HitId::new(1), 20);
        assert_eq!(result, MouseResult::Scrolled);
        assert_eq!(state.offset, 3);
    }

    #[test]
    fn list_state_select_next() {
        let mut state = ListState::default();
        state.select_next(5);
        assert_eq!(state.selected(), Some(0));
        state.select_next(5);
        assert_eq!(state.selected(), Some(1));
    }

    #[test]
    fn list_state_select_next_clamps() {
        let mut state = ListState::default();
        state.select(Some(4));
        state.select_next(5);
        assert_eq!(state.selected(), Some(4)); // already at last
    }

    #[test]
    fn list_state_select_next_empty() {
        let mut state = ListState::default();
        state.select_next(0);
        assert_eq!(state.selected(), None); // no items, no change
    }

    #[test]
    fn list_state_select_previous() {
        let mut state = ListState::default();
        state.select(Some(3));
        state.select_previous();
        assert_eq!(state.selected(), Some(2));
    }

    #[test]
    fn list_state_select_previous_clamps() {
        let mut state = ListState::default();
        state.select(Some(0));
        state.select_previous();
        assert_eq!(state.selected(), Some(0)); // already at first
    }

    #[test]
    fn list_state_select_previous_from_none() {
        let mut state = ListState::default();
        state.select_previous();
        assert_eq!(state.selected(), Some(0));
    }

    #[test]
    fn list_handle_key_down_from_none_selects_first() {
        let list = List::new(vec![
            ListItem::new("a"),
            ListItem::new("b"),
            ListItem::new("c"),
        ]);
        let mut state = ListState::default();
        assert_eq!(state.selected(), None);

        // Press Down
        assert!(list.handle_key(&mut state, &KeyEvent::new(KeyCode::Down)));
        // Should select "a" (index 0)
        assert_eq!(state.selected(), Some(0));
    }

    #[test]
    fn list_handle_key_up_from_none_selects_last() {
        let list = List::new(vec![
            ListItem::new("a"),
            ListItem::new("b"),
            ListItem::new("c"),
        ]);
        let mut state = ListState::default();
        assert_eq!(state.selected(), None);

        // Press Up
        assert!(list.handle_key(&mut state, &KeyEvent::new(KeyCode::Up)));
        // Should select "c" (index 2)
        assert_eq!(state.selected(), Some(2));
    }

    #[test]
    fn list_handle_key_navigation_supports_jk_and_arrows() {
        let list = List::new(vec![
            ListItem::new("a"),
            ListItem::new("b"),
            ListItem::new("c"),
        ]);
        let mut state = ListState::default();
        state.select(Some(0));

        assert!(list.handle_key(&mut state, &KeyEvent::new(KeyCode::Down)));
        assert_eq!(state.selected(), Some(1));
        assert!(list.handle_key(&mut state, &KeyEvent::new(KeyCode::Char('j'))));
        assert_eq!(state.selected(), Some(2));
        assert!(list.handle_key(&mut state, &KeyEvent::new(KeyCode::Up)));
        assert_eq!(state.selected(), Some(1));
        assert!(list.handle_key(&mut state, &KeyEvent::new(KeyCode::Char('k'))));
        assert_eq!(state.selected(), Some(0));
    }

    #[test]
    fn list_handle_key_filter_is_incremental_and_editable() {
        let list = List::new(vec![
            ListItem::new("alpha"),
            ListItem::new("banana"),
            ListItem::new("beta"),
        ]);
        let mut state = ListState::default();

        assert!(list.handle_key(&mut state, &KeyEvent::new(KeyCode::Char('b'))));
        assert_eq!(state.filter_query(), "b");
        assert_eq!(state.selected(), Some(1));

        assert!(list.handle_key(&mut state, &KeyEvent::new(KeyCode::Char('e'))));
        assert_eq!(state.filter_query(), "be");
        assert_eq!(state.selected(), Some(2));

        assert!(list.handle_key(&mut state, &KeyEvent::new(KeyCode::Backspace)));
        assert_eq!(state.filter_query(), "b");

        assert!(list.handle_key(&mut state, &KeyEvent::new(KeyCode::Escape)));
        assert_eq!(state.filter_query(), "");
    }

    #[test]
    fn list_render_filter_no_matches_shows_empty_state() {
        let list = List::new(vec![ListItem::new("alpha"), ListItem::new("beta")]);
        let mut state = ListState::default();
        state.set_filter_query("zzz");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(14, 3, &mut pool);
        StatefulWidget::render(&list, Rect::new(0, 0, 14, 3), &mut frame, &mut state);

        assert_eq!(row_text(&frame, 0), "No matches");
    }

    #[test]
    fn list_multi_select_toggle_with_space() {
        let list = List::new(vec![
            ListItem::new("alpha"),
            ListItem::new("beta"),
            ListItem::new("gamma"),
        ]);
        let mut state = ListState::default();
        state.set_multi_select(true);
        state.select(Some(0));

        assert!(list.handle_key(&mut state, &KeyEvent::new(KeyCode::Char(' '))));
        assert!(state.selected_indices().contains(&0));

        assert!(list.handle_key(&mut state, &KeyEvent::new(KeyCode::Down)));
        assert!(list.handle_key(&mut state, &KeyEvent::new(KeyCode::Char(' '))));
        assert!(state.selected_indices().contains(&1));
        assert_eq!(state.selected_count(), 2);
    }

    #[test]
    fn list_render_draws_scroll_indicators() {
        let items: Vec<ListItem> = (0..8).map(|i| ListItem::new(format!("Item {i}"))).collect();
        let list = List::new(items);
        let mut state = ListState {
            selected: Some(4),
            offset: 2,
            scroll_into_view_requested: false,
            ..Default::default()
        };
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(8, 3, &mut pool);
        StatefulWidget::render(&list, Rect::new(0, 0, 8, 3), &mut frame, &mut state);

        assert_eq!(
            frame.buffer.get(7, 0).and_then(|c| c.content.as_char()),
            Some('↑')
        );
        assert_eq!(
            frame.buffer.get(7, 2).and_then(|c| c.content.as_char()),
            Some('↓')
        );
    }

    #[cfg(feature = "tracing")]
    #[test]
    fn list_tracing_span_and_selection_events_are_emitted() {
        let trace_state = Arc::new(Mutex::new(ListTraceState::default()));
        let subscriber = tracing_subscriber::registry().with(ListTraceCapture {
            state: Arc::clone(&trace_state),
        });
        let _guard = tracing::subscriber::set_default(subscriber);
        tracing::callsite::rebuild_interest_cache();

        let list = List::new(vec![
            ListItem::new("a"),
            ListItem::new("b"),
            ListItem::new("c"),
        ]);
        let mut state = ListState::default();
        state.select(Some(0));
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);
        StatefulWidget::render(&list, Rect::new(0, 0, 10, 3), &mut frame, &mut state);
        assert!(list.handle_key(&mut state, &KeyEvent::new(KeyCode::Down)));

        tracing::callsite::rebuild_interest_cache();
        let snapshot = trace_state.lock().expect("list trace state lock");
        assert!(snapshot.list_render_seen, "expected list.render span");
        assert!(
            snapshot.has_total_items_field,
            "list.render missing total_items"
        );
        assert!(
            snapshot.has_visible_items_field,
            "list.render missing visible_items"
        );
        assert!(
            snapshot.has_selected_count_field,
            "list.render missing selected_count"
        );
        assert!(
            snapshot.has_filter_active_field,
            "list.render missing filter_active"
        );
        assert!(
            snapshot.render_duration_recorded,
            "list.render did not record render_duration_us"
        );
        assert!(
            snapshot.selection_events >= 1,
            "expected list.selection debug event"
        );
    }

    #[test]
    fn list_state_right_click_ignored() {
        let mut state = ListState::default();
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Right), 5, 2);
        let hit = Some((HitId::new(1), HitRegion::Content, 3u64));
        let result = state.handle_mouse(&event, hit, HitId::new(1), 10);
        assert_eq!(result, MouseResult::Ignored);
    }

    #[test]
    fn list_state_click_border_region_ignored() {
        let mut state = ListState::default();
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 5, 2);
        let hit = Some((HitId::new(1), HitRegion::Border, 3u64));
        let result = state.handle_mouse(&event, hit, HitId::new(1), 10);
        assert_eq!(result, MouseResult::Ignored);
    }

    #[test]
    fn list_state_second_click_activates() {
        let mut state = ListState::default();
        state.select(Some(3));

        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 5, 2);
        let hit = Some((HitId::new(1), HitRegion::Content, 3u64));
        let result = state.handle_mouse(&event, hit, HitId::new(1), 10);
        assert_eq!(result, MouseResult::Activated(3));
        assert_eq!(state.selected(), Some(3));
    }

    #[test]
    fn list_state_hover_updates() {
        let mut state = ListState::default();
        let event = MouseEvent::new(MouseEventKind::Moved, 5, 2);
        let hit = Some((HitId::new(1), HitRegion::Content, 3u64));
        let result = state.handle_mouse(&event, hit, HitId::new(1), 10);
        assert_eq!(result, MouseResult::HoverChanged);
        assert_eq!(state.hovered, Some(3));
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn list_state_hover_same_index_ignored() {
        let mut state = {
            let mut s = ListState::default();
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
    fn list_state_hover_clears() {
        let mut state = {
            let mut s = ListState::default();
            s.hovered = Some(5);
            s
        };
        let event = MouseEvent::new(MouseEventKind::Moved, 5, 2);
        // No hit (mouse moved off the list)
        let result = state.handle_mouse(&event, None, HitId::new(1), 10);
        assert_eq!(result, MouseResult::HoverChanged);
        assert_eq!(state.hovered, None);
    }

    #[test]
    fn list_state_hover_clear_when_already_none() {
        let mut state = ListState::default();
        let event = MouseEvent::new(MouseEventKind::Moved, 5, 2);
        let result = state.handle_mouse(&event, None, HitId::new(1), 10);
        assert_eq!(result, MouseResult::Ignored);
    }
}
