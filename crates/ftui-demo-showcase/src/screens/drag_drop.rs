#![forbid(unsafe_code)]

//! Drag-and-Drop Demo screen (bd-1csc.5).
//!
//! Showcases the drag-and-drop framework including:
//! - Sortable list with mouse drag
//! - Keyboard drag accessibility
//! - Cross-container drag between lists
//! - Various payload types

use std::cell::Cell;

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEventKind,
};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::{Style, StyleFlags};
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::keyboard_drag::{
    Direction, DropTargetInfo, KeyboardDragConfig, KeyboardDragKey, KeyboardDragManager,
    KeyboardDragMode,
};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::rule::Rule;

use super::{HelpEntry, Screen};
use crate::theme;

/// Number of items in each demo list.
const LIST_SIZE: usize = 8;

/// Demo mode for the drag-drop screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DemoMode {
    /// Sortable list demonstration.
    #[default]
    SortableList,
    /// Cross-container drag demonstration.
    CrossContainer,
    /// Keyboard drag accessibility demonstration.
    KeyboardDrag,
}

impl DemoMode {
    fn next(self) -> Self {
        match self {
            Self::SortableList => Self::CrossContainer,
            Self::CrossContainer => Self::KeyboardDrag,
            Self::KeyboardDrag => Self::SortableList,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::SortableList => Self::KeyboardDrag,
            Self::CrossContainer => Self::SortableList,
            Self::KeyboardDrag => Self::CrossContainer,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::SortableList => "Sortable List",
            Self::CrossContainer => "Cross-Container",
            Self::KeyboardDrag => "Keyboard Drag",
        }
    }
}

/// A draggable item in the demo lists.
#[derive(Debug, Clone)]
struct DragItem {
    id: usize,
    label: String,
    color: ftui_render::cell::PackedRgba,
}

impl DragItem {
    fn new(id: usize, label: impl Into<String>, color: ftui_render::cell::PackedRgba) -> Self {
        Self {
            id,
            label: label.into(),
            color,
        }
    }
}

/// Drag-and-Drop Demo screen state.
pub struct DragDropDemo {
    /// Current demo mode.
    mode: DemoMode,
    /// Items in the first (left) list.
    left_list: Vec<DragItem>,
    /// Items in the second (right) list.
    right_list: Vec<DragItem>,
    /// Selected item index in the focused list.
    selected_index: usize,
    /// Which list is focused (0 = left, 1 = right).
    focused_list: usize,
    /// Keyboard drag manager for accessibility.
    keyboard_drag: KeyboardDragManager,
    /// Current tick for animations.
    tick_count: u64,
    /// Announcements for screen readers.
    announcements: Vec<String>,
    /// Cached area for mode tabs.
    layout_tabs: Cell<Rect>,
    /// Cached area for the left list.
    layout_left: Cell<Rect>,
    /// Cached area for the right list.
    layout_right: Cell<Rect>,
}

impl Default for DragDropDemo {
    fn default() -> Self {
        Self::new()
    }
}

impl DragDropDemo {
    pub fn new() -> Self {
        let colors = [
            theme::accent::PRIMARY,
            theme::accent::SECONDARY,
            theme::accent::SUCCESS,
            theme::accent::WARNING,
            theme::accent::ERROR,
            theme::accent::INFO,
            theme::accent::LINK,
            theme::fg::PRIMARY,
        ];

        let left_items: Vec<DragItem> = (0..LIST_SIZE)
            .map(|i| {
                DragItem::new(
                    i,
                    format!("Item {}", i + 1),
                    colors[i % colors.len()].into(),
                )
            })
            .collect();

        let right_items: Vec<DragItem> = (LIST_SIZE..LIST_SIZE * 2)
            .map(|i| {
                let display_idx = i - LIST_SIZE + 1;
                DragItem::new(
                    i,
                    format!("File {}", display_idx),
                    colors[i % colors.len()].into(),
                )
            })
            .collect();

        Self {
            mode: DemoMode::default(),
            left_list: left_items,
            right_list: right_items,
            selected_index: 0,
            focused_list: 0,
            keyboard_drag: KeyboardDragManager::new(KeyboardDragConfig::default()),
            tick_count: 0,
            announcements: Vec::new(),
            layout_tabs: Cell::new(Rect::default()),
            layout_left: Cell::new(Rect::default()),
            layout_right: Cell::new(Rect::default()),
        }
    }

    // -----------------------------------------------------------------------
    // Public getters for testing
    // -----------------------------------------------------------------------

    /// Get the current selected index (for testing).
    #[must_use]
    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    /// Get the focused list index (0 = left, 1 = right) (for testing).
    #[must_use]
    pub fn focused_list(&self) -> usize {
        self.focused_list
    }

    /// Get the left list length (for testing).
    #[must_use]
    pub fn left_list_len(&self) -> usize {
        self.left_list.len()
    }

    /// Get the right list length (for testing).
    #[must_use]
    pub fn right_list_len(&self) -> usize {
        self.right_list.len()
    }

    /// Get an item ID from the left list (for testing).
    #[must_use]
    pub fn left_item_id(&self, index: usize) -> Option<usize> {
        self.left_list.get(index).map(|item| item.id)
    }

    /// Get an item ID from the right list (for testing).
    #[must_use]
    pub fn right_item_id(&self, index: usize) -> Option<usize> {
        self.right_list.get(index).map(|item| item.id)
    }

    /// Get the last item ID from the right list (for testing).
    #[must_use]
    pub fn right_last_item_id(&self) -> Option<usize> {
        self.right_list.last().map(|item| item.id)
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Get the currently focused list.
    fn focused_items(&self) -> &[DragItem] {
        if self.focused_list == 0 {
            &self.left_list
        } else {
            &self.right_list
        }
    }

    /// Get the currently focused list mutably.
    fn focused_items_mut(&mut self) -> &mut Vec<DragItem> {
        if self.focused_list == 0 {
            &mut self.left_list
        } else {
            &mut self.right_list
        }
    }

    /// Move selection up.
    fn select_up(&mut self) {
        let len = self.focused_items().len();
        if len > 0 {
            self.selected_index = (self.selected_index + len - 1) % len;
        }
    }

    /// Move selection down.
    fn select_down(&mut self) {
        let len = self.focused_items().len();
        if len > 0 {
            self.selected_index = (self.selected_index + 1) % len;
        }
    }

    /// Switch focus between lists.
    fn switch_list(&mut self) {
        self.focused_list = 1 - self.focused_list;
        let len = self.focused_items().len();
        if self.selected_index >= len {
            self.selected_index = len.saturating_sub(1);
        }
    }

    /// Move the selected item up in the list (reorder).
    fn move_item_up(&mut self) {
        let idx = self.selected_index;
        if idx == 0 {
            return;
        }
        let items = self.focused_items_mut();
        items.swap(idx, idx - 1);
        self.selected_index = idx - 1;
        self.announcements.push(format!(
            "Moved item to position {}",
            self.selected_index + 1
        ));
    }

    /// Move the selected item down in the list (reorder).
    fn move_item_down(&mut self) {
        let idx = self.selected_index;
        let len = self.focused_items().len();
        if idx >= len.saturating_sub(1) {
            return;
        }
        let items = self.focused_items_mut();
        items.swap(idx, idx + 1);
        self.selected_index = idx + 1;
        self.announcements.push(format!(
            "Moved item to position {}",
            self.selected_index + 1
        ));
    }

    /// Transfer selected item to the other list.
    fn transfer_item(&mut self) {
        let idx = self.selected_index;

        // Check source list validity
        let from_len = if self.focused_list == 0 {
            self.left_list.len()
        } else {
            self.right_list.len()
        };

        if from_len == 0 || idx >= from_len {
            return;
        }

        // Remove from source, add to target
        let item = if self.focused_list == 0 {
            self.left_list.remove(idx)
        } else {
            self.right_list.remove(idx)
        };

        if self.focused_list == 0 {
            self.right_list.push(item);
        } else {
            self.left_list.push(item);
        }

        // Adjust selection
        let new_from_len = if self.focused_list == 0 {
            self.left_list.len()
        } else {
            self.right_list.len()
        };

        if self.selected_index >= new_from_len && new_from_len > 0 {
            self.selected_index = new_from_len - 1;
        }
        self.announcements
            .push("Item transferred to other list".to_string());
    }

    /// Handle keyboard drag events.
    fn handle_keyboard_drag(&mut self, event: &Event) -> bool {
        if let Event::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            modifiers,
            ..
        }) = event
        {
            // Map keys to keyboard drag actions
            let key = match (code, modifiers.contains(Modifiers::SHIFT)) {
                (KeyCode::Char(' '), false) | (KeyCode::Enter, false) => {
                    // Activate either starts or completes a drag depending on state
                    Some(KeyboardDragKey::Activate)
                }
                (KeyCode::Escape, _) => Some(KeyboardDragKey::Cancel),
                (KeyCode::Up, _) => Some(KeyboardDragKey::Navigate(Direction::Up)),
                (KeyCode::Down, _) => Some(KeyboardDragKey::Navigate(Direction::Down)),
                (KeyCode::Left, _) => Some(KeyboardDragKey::Navigate(Direction::Left)),
                (KeyCode::Right, _) => Some(KeyboardDragKey::Navigate(Direction::Right)),
                _ => None,
            };

            if let Some(key) = key {
                // Build drop targets from both lists
                let targets = self.build_drop_targets();

                if !self.keyboard_drag.is_active() && key == KeyboardDragKey::Activate {
                    // Start drag from selected item - copy data before borrowing keyboard_drag
                    let (item_label, item_id) = {
                        let item = &self.focused_items()[self.selected_index];
                        (item.label.clone(), item.id)
                    };
                    let payload = ftui_widgets::drag::DragPayload::text(&item_label);
                    let source_id = ftui_widgets::measure_cache::WidgetId(item_id as u64);
                    if self.keyboard_drag.start_drag(source_id, payload) {
                        self.announcements.push(format!(
                            "Started dragging {}. Use arrows to navigate, Enter to drop.",
                            item_label
                        ));
                        return true;
                    }
                } else {
                    // Handle navigation and other keys
                    match key {
                        KeyboardDragKey::Navigate(dir) => {
                            if let Some(target) = self.keyboard_drag.navigate_targets(dir, &targets)
                            {
                                self.announcements.push(format!("Target: {}", target.name));
                            }
                        }
                        KeyboardDragKey::Activate => {
                            // Try to complete the drag
                            if let Some(result) = self.keyboard_drag.drop_on_target(&targets) {
                                self.announcements.push(format!(
                                    "Dropped {} at target {}",
                                    result.payload.display_text.as_deref().unwrap_or("item"),
                                    result.target_index
                                ));
                            }
                        }
                        KeyboardDragKey::Cancel => {
                            let _ = self.keyboard_drag.cancel_drag();
                            self.announcements.push("Drag cancelled".to_string());
                        }
                    }
                }

                // Collect and display announcements
                for ann in self.keyboard_drag.drain_announcements() {
                    self.announcements.push(ann.text);
                }

                return true;
            }
        }
        false
    }

    /// Build drop targets from both lists.
    fn build_drop_targets(&self) -> Vec<DropTargetInfo> {
        let mut targets = Vec::new();

        // Left list items as targets
        for (i, item) in self.left_list.iter().enumerate() {
            targets.push(DropTargetInfo::new(
                ftui_widgets::measure_cache::WidgetId(item.id as u64),
                format!("Left: {}", item.label),
                Rect::new(0, i as u16, 30, 1), // Simplified bounds
            ));
        }

        // Right list items as targets
        for (i, item) in self.right_list.iter().enumerate() {
            targets.push(DropTargetInfo::new(
                ftui_widgets::measure_cache::WidgetId(item.id as u64),
                format!("Right: {}", item.label),
                Rect::new(40, i as u16, 30, 1), // Simplified bounds
            ));
        }

        targets
    }
}

impl Screen for DragDropDemo {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Mouse(me) = event {
            self.handle_mouse(me.kind, me.x, me.y);
            return Cmd::None;
        }

        // In keyboard drag mode, handle drag events first
        if self.mode == DemoMode::KeyboardDrag && self.handle_keyboard_drag(event) {
            return Cmd::None;
        }

        if let Event::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            modifiers,
            ..
        }) = event
        {
            match (code, modifiers.contains(Modifiers::SHIFT)) {
                // Mode navigation
                (KeyCode::Tab, false) => self.mode = self.mode.next(),
                (KeyCode::Tab, true) | (KeyCode::BackTab, _) => self.mode = self.mode.prev(),

                // List navigation
                (KeyCode::Up | KeyCode::Char('k'), _) => self.select_up(),
                (KeyCode::Down | KeyCode::Char('j'), _) => self.select_down(),
                (KeyCode::Left | KeyCode::Char('h'), _) => {
                    if self.mode == DemoMode::CrossContainer {
                        self.switch_list();
                    }
                }
                (KeyCode::Right | KeyCode::Char('l'), _) => {
                    if self.mode == DemoMode::CrossContainer {
                        self.switch_list();
                    }
                }

                // Reorder within list (sortable mode)
                (KeyCode::Char('K'), true) | (KeyCode::Char('u'), _) => {
                    if self.mode == DemoMode::SortableList {
                        self.move_item_up();
                    }
                }
                (KeyCode::Char('J'), true) | (KeyCode::Char('d'), _) => {
                    if self.mode == DemoMode::SortableList {
                        self.move_item_down();
                    }
                }

                // Transfer between lists (cross-container mode)
                (KeyCode::Enter, false) => {
                    if self.mode == DemoMode::CrossContainer {
                        self.transfer_item();
                    }
                }

                _ => {}
            }
        }

        Cmd::None
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
        self.keyboard_drag.tick();

        // Clear old announcements
        if self.announcements.len() > 3 {
            self.announcements.remove(0);
        }
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.height < 8 || area.width < 40 {
            Paragraph::new("Terminal too small for drag demo")
                .style(theme::muted())
                .render(area, frame);
            return;
        }

        // Layout: mode tabs (1) + content + instructions (3)
        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Min(6),
                Constraint::Fixed(3),
            ])
            .split(area);

        self.layout_tabs.set(rows[0]);
        self.render_mode_tabs(frame, rows[0]);

        match self.mode {
            DemoMode::SortableList => self.render_sortable_list(frame, rows[1]),
            DemoMode::CrossContainer => self.render_cross_container(frame, rows[1]),
            DemoMode::KeyboardDrag => self.render_keyboard_drag(frame, rows[1]),
        }

        self.render_instructions(frame, rows[2]);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        let mut bindings = vec![
            HelpEntry {
                key: "Tab",
                action: "Next mode",
            },
            HelpEntry {
                key: "j/k",
                action: "Navigate list",
            },
        ];

        bindings.push(HelpEntry {
            key: "Click",
            action: "Select item",
        });
        bindings.push(HelpEntry {
            key: "Scroll",
            action: "Navigate list",
        });

        match self.mode {
            DemoMode::SortableList => {
                bindings.push(HelpEntry {
                    key: "u/d",
                    action: "Move item up/down",
                });
            }
            DemoMode::CrossContainer => {
                bindings.push(HelpEntry {
                    key: "h/l",
                    action: "Switch list",
                });
                bindings.push(HelpEntry {
                    key: "Enter",
                    action: "Transfer item",
                });
            }
            DemoMode::KeyboardDrag => {
                bindings.push(HelpEntry {
                    key: "Space",
                    action: "Pick up/Drop",
                });
                bindings.push(HelpEntry {
                    key: "Esc",
                    action: "Cancel drag",
                });
            }
        }

        bindings
    }

    fn title(&self) -> &'static str {
        "Drag & Drop"
    }

    fn tab_label(&self) -> &'static str {
        "DnD"
    }
}

impl DragDropDemo {
    fn render_mode_tabs(&self, frame: &mut Frame, area: Rect) {
        let modes = [
            DemoMode::SortableList,
            DemoMode::CrossContainer,
            DemoMode::KeyboardDrag,
        ];

        let mut text = String::new();
        for (i, mode) in modes.iter().enumerate() {
            if i > 0 {
                text.push_str(" | ");
            }
            if *mode == self.mode {
                text.push_str(&format!("[{}]", mode.label()));
            } else {
                text.push_str(&format!(" {} ", mode.label()));
            }
        }

        let style = Style::new()
            .fg(theme::screen_accent::DASHBOARD)
            .attrs(StyleFlags::BOLD);
        Paragraph::new(text).style(style).render(area, frame);
    }

    fn render_sortable_list(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Sortable List")
            .title_alignment(Alignment::Center)
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        self.layout_left.set(inner);

        if inner.is_empty() {
            return;
        }

        // Only show the left list in sortable mode
        self.render_list(&self.left_list, inner, frame, true);
    }

    fn render_cross_container(&self, frame: &mut Frame, area: Rect) {
        let cols = Flex::horizontal()
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(area);

        // Left panel
        let left_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Source List")
            .style(if self.focused_list == 0 {
                Style::new().fg(theme::accent::PRIMARY)
            } else {
                theme::content_border()
            });
        let left_inner = left_block.inner(cols[0]);
        left_block.render(cols[0], frame);

        // Right panel
        let right_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Target List")
            .style(if self.focused_list == 1 {
                Style::new().fg(theme::accent::PRIMARY)
            } else {
                theme::content_border()
            });
        let right_inner = right_block.inner(cols[1]);
        right_block.render(cols[1], frame);

        self.layout_left.set(left_inner);
        self.layout_right.set(right_inner);
        self.render_list(&self.left_list, left_inner, frame, self.focused_list == 0);
        self.render_list(&self.right_list, right_inner, frame, self.focused_list == 1);
    }

    fn render_keyboard_drag(&self, frame: &mut Frame, area: Rect) {
        let rows = Flex::vertical()
            .constraints([Constraint::Min(4), Constraint::Fixed(3)])
            .split(area);

        let cols = Flex::horizontal()
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(rows[0]);

        // Left panel
        let left_highlight = self.keyboard_drag.is_active() && self.focused_list == 0;
        let left_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("List A")
            .style(if left_highlight {
                Style::new().fg(theme::accent::SUCCESS)
            } else if self.focused_list == 0 {
                Style::new().fg(theme::accent::PRIMARY)
            } else {
                theme::content_border()
            });
        let left_inner = left_block.inner(cols[0]);
        left_block.render(cols[0], frame);

        // Right panel
        let right_highlight = self.keyboard_drag.is_active() && self.focused_list == 1;
        let right_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("List B")
            .style(if right_highlight {
                Style::new().fg(theme::accent::SUCCESS)
            } else if self.focused_list == 1 {
                Style::new().fg(theme::accent::PRIMARY)
            } else {
                theme::content_border()
            });
        let right_inner = right_block.inner(cols[1]);
        right_block.render(cols[1], frame);

        self.layout_left.set(left_inner);
        self.layout_right.set(right_inner);
        self.render_list(&self.left_list, left_inner, frame, self.focused_list == 0);
        self.render_list(&self.right_list, right_inner, frame, self.focused_list == 1);

        // Drag status and announcements
        self.render_drag_status(frame, rows[1]);
    }

    fn render_list(&self, items: &[DragItem], area: Rect, frame: &mut Frame, is_focused: bool) {
        if area.is_empty() {
            return;
        }

        for (i, item) in items.iter().enumerate() {
            if i as u16 >= area.height {
                break;
            }

            let is_selected = is_focused && i == self.selected_index;
            let is_dragging = self.keyboard_drag.is_active()
                && self
                    .keyboard_drag
                    .state()
                    .map(|s| s.source_id == ftui_widgets::measure_cache::WidgetId(item.id as u64))
                    .unwrap_or(false);

            let row_area = Rect::new(area.x, area.y + i as u16, area.width, 1);

            let prefix = if is_dragging {
                "  "
            } else if is_selected {
                "> "
            } else {
                "  "
            };

            let text = format!("{}{}", prefix, item.label);
            let style = if is_dragging {
                Style::new()
                    .fg(theme::fg::MUTED)
                    .attrs(StyleFlags::DIM | StyleFlags::ITALIC)
            } else if is_selected {
                Style::new()
                    .fg(item.color)
                    .attrs(StyleFlags::BOLD)
                    .bg(theme::alpha::SURFACE)
            } else {
                Style::new().fg(item.color)
            };

            Paragraph::new(text).style(style).render(row_area, frame);
        }
    }

    fn render_drag_status(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Drag Status")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let status = if self.keyboard_drag.is_active() {
            let mode = match self.keyboard_drag.mode() {
                KeyboardDragMode::Holding => "Holding item",
                KeyboardDragMode::Navigating => "Navigating targets",
                KeyboardDragMode::Inactive => "Inactive",
            };
            format!("Mode: {} | Press Enter to drop, Esc to cancel", mode)
        } else {
            "Press Space/Enter on an item to start dragging".to_string()
        };

        let style = if self.keyboard_drag.is_active() {
            Style::new().fg(theme::accent::SUCCESS)
        } else {
            Style::new().fg(theme::fg::MUTED)
        };

        Paragraph::new(status).style(style).render(inner, frame);
    }

    fn render_instructions(&self, frame: &mut Frame, area: Rect) {
        Rule::new()
            .title("Instructions")
            .title_alignment(Alignment::Left)
            .style(theme::muted())
            .render(Rect::new(area.x, area.y, area.width, 1), frame);

        let instructions = match self.mode {
            DemoMode::SortableList => {
                "Use j/k to navigate, u/d to reorder items within the list.\n\
                 This simulates a sortable list where items can be moved up/down."
            }
            DemoMode::CrossContainer => {
                "Use h/l to switch between lists, j/k to navigate, Enter to transfer.\n\
                 This demonstrates dragging items between different containers."
            }
            DemoMode::KeyboardDrag => {
                "Press Space/Enter to pick up an item, arrows to navigate targets, Enter to drop.\n\
                 Escape cancels the drag. Fully keyboard-accessible drag-and-drop!"
            }
        };

        let text_area = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(1),
        );
        Paragraph::new(instructions)
            .style(Style::new().fg(theme::fg::SECONDARY))
            .render(text_area, frame);
    }
}

impl DragDropDemo {
    fn handle_mouse(&mut self, kind: MouseEventKind, x: u16, y: u16) {
        let tabs = self.layout_tabs.get();
        let left = self.layout_left.get();
        let right = self.layout_right.get();

        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if tabs.contains(x, y) {
                    // Click on mode tabs — determine which mode was clicked
                    let rel_x = x.saturating_sub(tabs.x);
                    let label_0 = DemoMode::SortableList.label().len() as u16 + 4; // " [..] "
                    let label_1 = label_0 + 3 + DemoMode::CrossContainer.label().len() as u16 + 2;
                    if rel_x < label_0 {
                        self.mode = DemoMode::SortableList;
                    } else if rel_x < label_1 {
                        self.mode = DemoMode::CrossContainer;
                    } else {
                        self.mode = DemoMode::KeyboardDrag;
                    }
                } else if left.contains(x, y) {
                    // Click in left list — select item
                    if self.mode == DemoMode::CrossContainer || self.mode == DemoMode::KeyboardDrag
                    {
                        self.focused_list = 0;
                    }
                    let row = y.saturating_sub(left.y) as usize;
                    let len = self.left_list.len();
                    if row < len {
                        self.selected_index = row;
                    }
                } else if right.contains(x, y) {
                    // Click in right list — select item (cross-container and keyboard modes)
                    if self.mode == DemoMode::CrossContainer || self.mode == DemoMode::KeyboardDrag
                    {
                        self.focused_list = 1;
                    }
                    let row = y.saturating_sub(right.y) as usize;
                    let len = self.right_list.len();
                    if row < len {
                        self.selected_index = row;
                    }
                }
            }
            MouseEventKind::ScrollUp => {
                if left.contains(x, y) || right.contains(x, y) {
                    self.select_up();
                }
            }
            MouseEventKind::ScrollDown => {
                if left.contains(x, y) || right.contains(x, y) {
                    self.select_down();
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                // Right-click in sortable mode: move selected item up
                if self.mode == DemoMode::SortableList && left.contains(x, y) {
                    self.move_item_up();
                }
                // Right-click in cross-container mode: transfer
                if self.mode == DemoMode::CrossContainer
                    && (left.contains(x, y) || right.contains(x, y))
                {
                    self.transfer_item();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn demo_initial_state() {
        let demo = DragDropDemo::new();
        assert_eq!(demo.mode, DemoMode::SortableList);
        assert_eq!(demo.selected_index, 0);
        assert_eq!(demo.focused_list, 0);
        assert_eq!(demo.left_list.len(), LIST_SIZE);
        assert_eq!(demo.right_list.len(), LIST_SIZE);
    }

    #[test]
    fn demo_mode_navigation() {
        assert_eq!(DemoMode::SortableList.next(), DemoMode::CrossContainer);
        assert_eq!(DemoMode::CrossContainer.next(), DemoMode::KeyboardDrag);
        assert_eq!(DemoMode::KeyboardDrag.next(), DemoMode::SortableList);

        assert_eq!(DemoMode::SortableList.prev(), DemoMode::KeyboardDrag);
        assert_eq!(DemoMode::KeyboardDrag.prev(), DemoMode::CrossContainer);
        assert_eq!(DemoMode::CrossContainer.prev(), DemoMode::SortableList);
    }

    #[test]
    fn demo_select_navigation() {
        let mut demo = DragDropDemo::new();
        assert_eq!(demo.selected_index, 0);

        demo.select_down();
        assert_eq!(demo.selected_index, 1);

        demo.select_down();
        assert_eq!(demo.selected_index, 2);

        demo.select_up();
        assert_eq!(demo.selected_index, 1);

        demo.select_up();
        assert_eq!(demo.selected_index, 0);

        // Wrap around
        demo.select_up();
        assert_eq!(demo.selected_index, LIST_SIZE - 1);
    }

    #[test]
    fn demo_switch_list() {
        let mut demo = DragDropDemo::new();
        assert_eq!(demo.focused_list, 0);

        demo.switch_list();
        assert_eq!(demo.focused_list, 1);

        demo.switch_list();
        assert_eq!(demo.focused_list, 0);
    }

    #[test]
    fn demo_move_item() {
        let mut demo = DragDropDemo::new();
        let first_item_id = demo.left_list[0].id;
        let second_item_id = demo.left_list[1].id;

        demo.select_down(); // Select second item
        demo.move_item_up();

        // Items should be swapped
        assert_eq!(demo.left_list[0].id, second_item_id);
        assert_eq!(demo.left_list[1].id, first_item_id);
        assert_eq!(demo.selected_index, 0);
    }

    #[test]
    fn demo_transfer_item() {
        let mut demo = DragDropDemo::new();
        demo.mode = DemoMode::CrossContainer;
        let left_len = demo.left_list.len();
        let right_len = demo.right_list.len();
        let item_id = demo.left_list[0].id;

        demo.transfer_item();

        assert_eq!(demo.left_list.len(), left_len - 1);
        assert_eq!(demo.right_list.len(), right_len + 1);
        assert_eq!(demo.right_list.last().unwrap().id, item_id);
    }

    #[test]
    fn demo_renders_all_modes() {
        let mut demo = DragDropDemo::new();
        let mut pool = GraphemePool::new();

        for mode in [
            DemoMode::SortableList,
            DemoMode::CrossContainer,
            DemoMode::KeyboardDrag,
        ] {
            demo.mode = mode;
            let mut frame = Frame::new(80, 24, &mut pool);
            demo.view(&mut frame, Rect::new(0, 0, 80, 24));
            // No panic = success
        }
    }

    #[test]
    fn demo_handles_small_terminal() {
        let demo = DragDropDemo::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 5, &mut pool);
        demo.view(&mut frame, Rect::new(0, 0, 30, 5));
        // Should show "too small" message without panic
    }

    #[test]
    fn demo_build_drop_targets() {
        let demo = DragDropDemo::new();
        let targets = demo.build_drop_targets();
        assert_eq!(targets.len(), LIST_SIZE * 2);
    }

    #[test]
    fn click_left_list_selects_item() {
        let mut demo = DragDropDemo::new();
        demo.layout_left.set(Rect::new(0, 2, 30, 10));
        demo.handle_mouse(MouseEventKind::Down(MouseButton::Left), 10, 5);
        assert_eq!(demo.selected_index, 3);
    }

    #[test]
    fn click_right_list_in_cross_container_switches_focus() {
        let mut demo = DragDropDemo::new();
        demo.mode = DemoMode::CrossContainer;
        demo.layout_right.set(Rect::new(40, 2, 30, 10));
        assert_eq!(demo.focused_list, 0);
        demo.handle_mouse(MouseEventKind::Down(MouseButton::Left), 50, 4);
        assert_eq!(demo.focused_list, 1);
        assert_eq!(demo.selected_index, 2);
    }

    #[test]
    fn scroll_navigates_list() {
        let mut demo = DragDropDemo::new();
        demo.layout_left.set(Rect::new(0, 0, 30, 10));
        assert_eq!(demo.selected_index, 0);
        demo.handle_mouse(MouseEventKind::ScrollDown, 10, 5);
        assert_eq!(demo.selected_index, 1);
        demo.handle_mouse(MouseEventKind::ScrollUp, 10, 5);
        assert_eq!(demo.selected_index, 0);
    }

    #[test]
    fn right_click_sortable_moves_up() {
        let mut demo = DragDropDemo::new();
        demo.mode = DemoMode::SortableList;
        demo.layout_left.set(Rect::new(0, 0, 30, 10));
        demo.selected_index = 2;
        let original_id = demo.left_list[2].id;
        demo.handle_mouse(MouseEventKind::Down(MouseButton::Right), 10, 5);
        assert_eq!(demo.left_list[1].id, original_id);
        assert_eq!(demo.selected_index, 1);
    }

    #[test]
    fn mouse_move_ignored() {
        let mut demo = DragDropDemo::new();
        demo.layout_left.set(Rect::new(0, 0, 30, 10));
        demo.handle_mouse(MouseEventKind::Moved, 10, 5);
        assert_eq!(demo.selected_index, 0);
    }
}
