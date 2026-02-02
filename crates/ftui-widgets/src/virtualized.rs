#![forbid(unsafe_code)]

//! Virtualization primitives for efficient rendering of large content.
//!
//! This module provides the foundational types for rendering only visible
//! portions of large datasets, enabling smooth performance with 100K+ items.
//!
//! # Core Types
//!
//! - [`Virtualized<T>`] - Generic container with visible range calculation
//! - [`VirtualizedStorage`] - Owned vs external storage abstraction
//! - [`ItemHeight`] - Fixed vs variable height support
//! - [`HeightCache`] - LRU cache for measured item heights
//!
//! # Example
//!
//! ```ignore
//! use ftui_widgets::virtualized::{Virtualized, ItemHeight};
//!
//! // Create with owned storage
//! let mut virt: Virtualized<String> = Virtualized::new(10_000);
//!
//! // Add items
//! for i in 0..1000 {
//!     virt.push(format!("Line {}", i));
//! }
//!
//! // Get visible range for viewport height
//! let range = virt.visible_range(24);
//! println!("Visible: {}..{}", range.start, range.end);
//! ```

use std::collections::VecDeque;
use std::ops::Range;
use std::time::Duration;

// Imports for future rendering support (currently unused but planned)
#[allow(unused_imports)]
use crate::scrollbar::{Scrollbar, ScrollbarOrientation, ScrollbarState};
#[allow(unused_imports)]
use crate::{set_style_area, StatefulWidget};
#[allow(unused_imports)]
use ftui_core::geometry::Rect;
#[allow(unused_imports)]
use ftui_render::cell::Cell;
#[allow(unused_imports)]
use ftui_render::frame::Frame;
#[allow(unused_imports)]
use ftui_style::Style;

/// A virtualized content container that tracks scroll state and computes visible ranges.
///
/// # Design Rationale
/// - Generic over item type for flexibility
/// - Supports both owned storage and external data sources
/// - Computes visible ranges for O(visible) rendering
/// - Optional overscan for smooth scrolling
/// - Momentum scrolling support
#[derive(Debug, Clone)]
pub struct Virtualized<T> {
    /// The stored items (or external storage reference).
    storage: VirtualizedStorage<T>,
    /// Current scroll offset (in items).
    scroll_offset: usize,
    /// Number of visible items (cached from last render).
    visible_count: usize,
    /// Overscan: extra items rendered above/below visible.
    overscan: usize,
    /// Height calculation strategy.
    item_height: ItemHeight,
    /// Whether to auto-scroll on new items.
    follow_mode: bool,
    /// Scroll velocity for momentum scrolling.
    scroll_velocity: f32,
}

/// Storage strategy for virtualized items.
#[derive(Debug, Clone)]
pub enum VirtualizedStorage<T> {
    /// Owned vector of items.
    Owned(VecDeque<T>),
    /// External storage with known length.
    /// Note: External fetch is handled at the widget level.
    External {
        /// Total number of items available.
        len: usize,
        /// Maximum items to keep in local cache.
        cache_capacity: usize,
    },
}

/// Height calculation strategy for items.
#[derive(Debug, Clone)]
pub enum ItemHeight {
    /// All items have fixed height.
    Fixed(u16),
    /// Items have variable height, cached lazily.
    Variable(HeightCache),
}

/// LRU cache for measured item heights.
#[derive(Debug, Clone)]
pub struct HeightCache {
    /// Height measurements indexed by item index.
    cache: Vec<Option<u16>>,
    /// Default height for unmeasured items.
    default_height: u16,
    /// Maximum entries to cache (for memory bounds).
    capacity: usize,
}

impl<T> Virtualized<T> {
    /// Create a new virtualized container with owned storage.
    ///
    /// # Arguments
    /// * `capacity` - Maximum items to retain in memory.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            storage: VirtualizedStorage::Owned(VecDeque::with_capacity(capacity.min(1024))),
            scroll_offset: 0,
            visible_count: 0,
            overscan: 2,
            item_height: ItemHeight::Fixed(1),
            follow_mode: false,
            scroll_velocity: 0.0,
        }
    }

    /// Create with external storage reference.
    #[must_use]
    pub fn external(len: usize, cache_capacity: usize) -> Self {
        Self {
            storage: VirtualizedStorage::External { len, cache_capacity },
            scroll_offset: 0,
            visible_count: 0,
            overscan: 2,
            item_height: ItemHeight::Fixed(1),
            follow_mode: false,
            scroll_velocity: 0.0,
        }
    }

    /// Set item height strategy.
    #[must_use]
    pub fn with_item_height(mut self, height: ItemHeight) -> Self {
        self.item_height = height;
        self
    }

    /// Set fixed item height.
    #[must_use]
    pub fn with_fixed_height(mut self, height: u16) -> Self {
        self.item_height = ItemHeight::Fixed(height);
        self
    }

    /// Set overscan amount.
    #[must_use]
    pub fn with_overscan(mut self, overscan: usize) -> Self {
        self.overscan = overscan;
        self
    }

    /// Enable follow mode.
    #[must_use]
    pub fn with_follow(mut self, follow: bool) -> Self {
        self.follow_mode = follow;
        self
    }

    /// Get total number of items.
    #[must_use]
    pub fn len(&self) -> usize {
        match &self.storage {
            VirtualizedStorage::Owned(items) => items.len(),
            VirtualizedStorage::External { len, .. } => *len,
        }
    }

    /// Check if empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get current scroll offset.
    #[must_use]
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Get current visible count (from last render).
    #[must_use]
    pub fn visible_count(&self) -> usize {
        self.visible_count
    }

    /// Check if follow mode is enabled.
    #[must_use]
    pub fn follow_mode(&self) -> bool {
        self.follow_mode
    }

    /// Calculate visible range for given viewport height.
    #[must_use]
    pub fn visible_range(&self, viewport_height: u16) -> Range<usize> {
        if self.is_empty() || viewport_height == 0 {
            return 0..0;
        }

        let items_visible = match &self.item_height {
            ItemHeight::Fixed(h) if *h > 0 => (viewport_height / h) as usize,
            ItemHeight::Fixed(_) => viewport_height as usize,
            ItemHeight::Variable(cache) => {
                // Sum heights until we exceed viewport
                let mut count = 0;
                let mut total_height = 0u16;
                let start = self.scroll_offset;
                while total_height < viewport_height && start + count < self.len() {
                    total_height = total_height.saturating_add(cache.get(start + count));
                    count += 1;
                }
                count
            }
        };

        let start = self.scroll_offset;
        let end = (start + items_visible).min(self.len());
        start..end
    }

    /// Get render range with overscan for smooth scrolling.
    #[must_use]
    pub fn render_range(&self, viewport_height: u16) -> Range<usize> {
        let visible = self.visible_range(viewport_height);
        let start = visible.start.saturating_sub(self.overscan);
        let end = (visible.end + self.overscan).min(self.len());
        start..end
    }

    /// Scroll by delta (positive = down/forward).
    pub fn scroll(&mut self, delta: i32) {
        if self.is_empty() {
            return;
        }
        let new_offset = (self.scroll_offset as i64 + delta as i64)
            .max(0)
            .min(self.len().saturating_sub(1) as i64);
        self.scroll_offset = new_offset as usize;

        // Disable follow mode on manual scroll
        if delta != 0 {
            self.follow_mode = false;
        }
    }

    /// Scroll to specific item index.
    pub fn scroll_to(&mut self, idx: usize) {
        self.scroll_offset = idx.min(self.len().saturating_sub(1));
        self.follow_mode = false;
    }

    /// Scroll to bottom.
    pub fn scroll_to_bottom(&mut self) {
        if self.len() > self.visible_count && self.visible_count > 0 {
            self.scroll_offset = self.len() - self.visible_count;
        } else {
            self.scroll_offset = 0;
        }
    }

    /// Scroll to top.
    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
        self.follow_mode = false;
    }

    /// Alias for scroll_to_top (Home key).
    pub fn scroll_to_start(&mut self) {
        self.scroll_to_top();
    }

    /// Scroll to bottom and enable follow mode (End key).
    pub fn scroll_to_end(&mut self) {
        self.scroll_to_bottom();
        self.follow_mode = true;
    }

    /// Page up (scroll by visible count).
    pub fn page_up(&mut self) {
        if self.visible_count > 0 {
            self.scroll(-(self.visible_count as i32));
        }
    }

    /// Page down (scroll by visible count).
    pub fn page_down(&mut self) {
        if self.visible_count > 0 {
            self.scroll(self.visible_count as i32);
        }
    }

    /// Set follow mode.
    pub fn set_follow(&mut self, follow: bool) {
        self.follow_mode = follow;
        if follow {
            self.scroll_to_bottom();
        }
    }

    /// Check if scrolled to bottom.
    #[must_use]
    pub fn is_at_bottom(&self) -> bool {
        if self.len() <= self.visible_count {
            true
        } else {
            self.scroll_offset >= self.len() - self.visible_count
        }
    }

    /// Start momentum scroll.
    pub fn fling(&mut self, velocity: f32) {
        self.scroll_velocity = velocity;
    }

    /// Apply momentum scroll tick.
    pub fn tick(&mut self, dt: Duration) {
        if self.scroll_velocity.abs() > 0.1 {
            let delta = (self.scroll_velocity * dt.as_secs_f32()) as i32;
            if delta != 0 {
                self.scroll(delta);
            }
            // Apply friction
            self.scroll_velocity *= 0.95;
        } else {
            self.scroll_velocity = 0.0;
        }
    }

    /// Update visible count (called during render).
    pub fn set_visible_count(&mut self, count: usize) {
        self.visible_count = count;
    }
}

impl<T> Virtualized<T> {
    /// Push an item (owned storage only).
    pub fn push(&mut self, item: T) {
        if let VirtualizedStorage::Owned(items) = &mut self.storage {
            items.push_back(item);
            if self.follow_mode {
                self.scroll_to_bottom();
            }
        }
    }

    /// Get item by index (owned storage only).
    #[must_use]
    pub fn get(&self, idx: usize) -> Option<&T> {
        if let VirtualizedStorage::Owned(items) = &self.storage {
            items.get(idx)
        } else {
            None
        }
    }

    /// Get mutable item by index (owned storage only).
    pub fn get_mut(&mut self, idx: usize) -> Option<&mut T> {
        if let VirtualizedStorage::Owned(items) = &mut self.storage {
            items.get_mut(idx)
        } else {
            None
        }
    }

    /// Clear all items (owned storage only).
    pub fn clear(&mut self) {
        if let VirtualizedStorage::Owned(items) = &mut self.storage {
            items.clear();
        }
        self.scroll_offset = 0;
    }

    /// Trim items from the front to keep at most `max` items (owned storage only).
    ///
    /// Returns the number of items removed.
    pub fn trim_front(&mut self, max: usize) -> usize {
        if let VirtualizedStorage::Owned(items) = &mut self.storage && items.len() > max {
            let to_remove = items.len() - max;
            items.drain(..to_remove);
            // Adjust scroll_offset if it was pointing beyond the new start
            self.scroll_offset = self.scroll_offset.saturating_sub(to_remove);
            return to_remove;
        }
        0
    }

    /// Iterate over items (owned storage only).
    /// Returns empty iterator for external storage.
    pub fn iter(&self) -> Box<dyn Iterator<Item = &T> + '_> {
        match &self.storage {
            VirtualizedStorage::Owned(items) => Box::new(items.iter()),
            VirtualizedStorage::External { .. } => Box::new(std::iter::empty()),
        }
    }

    /// Update external storage length.
    pub fn set_external_len(&mut self, len: usize) {
        if let VirtualizedStorage::External { len: l, .. } = &mut self.storage {
            *l = len;
            if self.follow_mode {
                self.scroll_to_bottom();
            }
        }
    }
}

impl Default for HeightCache {
    fn default() -> Self {
        Self::new(1, 1000)
    }
}

impl HeightCache {
    /// Create a new height cache.
    #[must_use]
    pub fn new(default_height: u16, capacity: usize) -> Self {
        Self {
            cache: Vec::new(),
            default_height,
            capacity,
        }
    }

    /// Get height for item, returning default if not cached.
    #[must_use]
    pub fn get(&self, idx: usize) -> u16 {
        self.cache.get(idx).and_then(|h| *h).unwrap_or(self.default_height)
    }

    /// Set height for item.
    pub fn set(&mut self, idx: usize, height: u16) {
        if idx >= self.cache.len() {
            self.cache.resize(idx + 1, None);
        }
        self.cache[idx] = Some(height);

        // Trim if over capacity
        if self.cache.len() > self.capacity {
            // Remove oldest entries
            let to_remove = self.cache.len() - self.capacity;
            self.cache.drain(0..to_remove);
        }
    }

    /// Clear cached heights.
    pub fn clear(&mut self) {
        self.cache.clear();
    }
}

// ============================================================================
// VirtualizedList Widget
// ============================================================================

/// Trait for items that can render themselves.
///
/// Implement this trait for item types that should render in a `VirtualizedList`.
pub trait RenderItem {
    /// Render the item into the frame at the given area.
    fn render(&self, area: Rect, frame: &mut Frame, selected: bool);

    /// Height of this item in terminal rows.
    fn height(&self) -> u16 {
        1
    }
}

/// State for the VirtualizedList widget.
#[derive(Debug, Clone)]
pub struct VirtualizedListState {
    /// Currently selected index.
    pub selected: Option<usize>,
    /// Scroll offset.
    scroll_offset: usize,
    /// Visible count (from last render).
    visible_count: usize,
    /// Overscan amount.
    overscan: usize,
    /// Whether follow mode is enabled.
    follow_mode: bool,
    /// Scroll velocity for momentum.
    scroll_velocity: f32,
}

impl Default for VirtualizedListState {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualizedListState {
    /// Create a new state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            selected: None,
            scroll_offset: 0,
            visible_count: 0,
            overscan: 2,
            follow_mode: false,
            scroll_velocity: 0.0,
        }
    }

    /// Create with overscan.
    #[must_use]
    pub fn with_overscan(mut self, overscan: usize) -> Self {
        self.overscan = overscan;
        self
    }

    /// Create with follow mode enabled.
    #[must_use]
    pub fn with_follow(mut self, follow: bool) -> Self {
        self.follow_mode = follow;
        self
    }

    /// Get current scroll offset.
    #[must_use]
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Get visible item count (from last render).
    #[must_use]
    pub fn visible_count(&self) -> usize {
        self.visible_count
    }

    /// Scroll by delta (positive = down).
    pub fn scroll(&mut self, delta: i32, total_items: usize) {
        if total_items == 0 {
            return;
        }
        let new_offset = (self.scroll_offset as i64 + delta as i64)
            .max(0)
            .min(total_items.saturating_sub(1) as i64);
        self.scroll_offset = new_offset as usize;

        if delta != 0 {
            self.follow_mode = false;
        }
    }

    /// Scroll to specific index.
    pub fn scroll_to(&mut self, idx: usize, total_items: usize) {
        self.scroll_offset = idx.min(total_items.saturating_sub(1));
        self.follow_mode = false;
    }

    /// Scroll to top.
    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
        self.follow_mode = false;
    }

    /// Scroll to bottom.
    pub fn scroll_to_bottom(&mut self, total_items: usize) {
        if total_items > self.visible_count && self.visible_count > 0 {
            self.scroll_offset = total_items - self.visible_count;
        } else {
            self.scroll_offset = 0;
        }
    }

    /// Page up (scroll by visible count).
    pub fn page_up(&mut self, total_items: usize) {
        if self.visible_count > 0 {
            self.scroll(-(self.visible_count as i32), total_items);
        }
    }

    /// Page down (scroll by visible count).
    pub fn page_down(&mut self, total_items: usize) {
        if self.visible_count > 0 {
            self.scroll(self.visible_count as i32, total_items);
        }
    }

    /// Select an item.
    pub fn select(&mut self, index: Option<usize>) {
        self.selected = index;
    }

    /// Select previous item.
    pub fn select_previous(&mut self, total_items: usize) {
        if total_items == 0 {
            self.selected = None;
            return;
        }
        self.selected = Some(match self.selected {
            Some(i) if i > 0 => i - 1,
            Some(_) => 0,
            None => 0,
        });
    }

    /// Select next item.
    pub fn select_next(&mut self, total_items: usize) {
        if total_items == 0 {
            self.selected = None;
            return;
        }
        self.selected = Some(match self.selected {
            Some(i) if i < total_items - 1 => i + 1,
            Some(i) => i,
            None => 0,
        });
    }

    /// Check if at bottom.
    #[must_use]
    pub fn is_at_bottom(&self, total_items: usize) -> bool {
        if total_items <= self.visible_count {
            true
        } else {
            self.scroll_offset >= total_items - self.visible_count
        }
    }

    /// Enable/disable follow mode.
    pub fn set_follow(&mut self, follow: bool, total_items: usize) {
        self.follow_mode = follow;
        if follow {
            self.scroll_to_bottom(total_items);
        }
    }

    /// Check if follow mode is enabled.
    #[must_use]
    pub fn follow_mode(&self) -> bool {
        self.follow_mode
    }

    /// Start momentum scroll.
    pub fn fling(&mut self, velocity: f32) {
        self.scroll_velocity = velocity;
    }

    /// Apply momentum scrolling tick.
    pub fn tick(&mut self, dt: Duration, total_items: usize) {
        if self.scroll_velocity.abs() > 0.1 {
            let delta = (self.scroll_velocity * dt.as_secs_f32()) as i32;
            if delta != 0 {
                self.scroll(delta, total_items);
            }
            self.scroll_velocity *= 0.95;
        } else {
            self.scroll_velocity = 0.0;
        }
    }
}

/// A virtualized list widget that renders only visible items.
///
/// This widget efficiently renders large lists by only drawing items
/// that are currently visible in the viewport, with optional overscan
/// for smooth scrolling.
#[derive(Debug)]
pub struct VirtualizedList<'a, T> {
    /// Items to render.
    items: &'a [T],
    /// Base style.
    style: Style,
    /// Style for selected item.
    highlight_style: Style,
    /// Whether to show scrollbar.
    show_scrollbar: bool,
    /// Fixed item height.
    fixed_height: u16,
}

impl<'a, T> VirtualizedList<'a, T> {
    /// Create a new virtualized list.
    #[must_use]
    pub fn new(items: &'a [T]) -> Self {
        Self {
            items,
            style: Style::default(),
            highlight_style: Style::default(),
            show_scrollbar: true,
            fixed_height: 1,
        }
    }

    /// Set base style.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set highlight style for selected item.
    #[must_use]
    pub fn highlight_style(mut self, style: Style) -> Self {
        self.highlight_style = style;
        self
    }

    /// Enable/disable scrollbar.
    #[must_use]
    pub fn show_scrollbar(mut self, show: bool) -> Self {
        self.show_scrollbar = show;
        self
    }

    /// Set fixed item height.
    #[must_use]
    pub fn fixed_height(mut self, height: u16) -> Self {
        self.fixed_height = height;
        self
    }
}

impl<T: RenderItem> StatefulWidget for VirtualizedList<'_, T> {
    type State = VirtualizedListState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "VirtualizedList",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height,
            items = self.items.len()
        )
        .entered();

        if area.is_empty() {
            return;
        }

        // Apply base style
        set_style_area(&mut frame.buffer, area, self.style);

        let total_items = self.items.len();
        if total_items == 0 {
            return;
        }

        // Reserve space for scrollbar if needed
        let items_per_viewport = (area.height / self.fixed_height) as usize;
        let needs_scrollbar = self.show_scrollbar && total_items > items_per_viewport;
        let content_width = if needs_scrollbar {
            area.width.saturating_sub(1)
        } else {
            area.width
        };

        // Ensure selection is within bounds
        if let Some(selected) = state.selected
            && selected >= total_items
        {
            state.selected = Some(total_items - 1);
        }

        // Ensure visible range includes selected item
        if let Some(selected) = state.selected {
            if selected >= state.scroll_offset + items_per_viewport {
                state.scroll_offset = selected.saturating_sub(items_per_viewport - 1);
            } else if selected < state.scroll_offset {
                state.scroll_offset = selected;
            }
        }

        // Clamp scroll offset
        let max_offset = total_items.saturating_sub(items_per_viewport);
        state.scroll_offset = state.scroll_offset.min(max_offset);

        // Update visible count
        state.visible_count = items_per_viewport.min(total_items);

        // Calculate render range with overscan
        let render_start = state.scroll_offset.saturating_sub(state.overscan);
        let render_end = (state.scroll_offset + items_per_viewport + state.overscan).min(total_items);

        // Render visible items
        for idx in render_start..render_end {
            // Calculate Y position relative to viewport
            let relative_idx = idx as i32 - state.scroll_offset as i32;
            let y_offset = relative_idx * self.fixed_height as i32;

            // Skip items above viewport
            if y_offset + self.fixed_height as i32 <= 0 {
                continue;
            }

            // Stop if below viewport
            if y_offset >= area.height as i32 {
                break;
            }

            // Calculate actual render area
            let y = area.y.saturating_add_signed(y_offset as i16);
            if y >= area.bottom() {
                break;
            }

            let visible_height = self.fixed_height.min(area.bottom().saturating_sub(y));
            if visible_height == 0 {
                continue;
            }

            let row_area = Rect::new(area.x, y, content_width, visible_height);

            let is_selected = state.selected == Some(idx);

            // Apply highlight style to selected row
            if is_selected {
                set_style_area(&mut frame.buffer, row_area, self.highlight_style);
            }

            // Render the item
            self.items[idx].render(row_area, frame, is_selected);
        }

        // Render scrollbar
        if needs_scrollbar {
            let scrollbar_area = Rect::new(
                area.right().saturating_sub(1),
                area.y,
                1,
                area.height,
            );

            let mut scrollbar_state = ScrollbarState::new(
                total_items,
                state.scroll_offset,
                items_per_viewport,
            );

            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            scrollbar.render(scrollbar_area, frame, &mut scrollbar_state);
        }
    }
}

// ============================================================================
// Simple RenderItem implementations for common types
// ============================================================================

impl RenderItem for String {
    fn render(&self, area: Rect, frame: &mut Frame, _selected: bool) {
        if area.is_empty() {
            return;
        }
        let max_chars = area.width as usize;
        for (i, ch) in self.chars().take(max_chars).enumerate() {
            frame.buffer.set(area.x + i as u16, area.y, Cell::from_char(ch));
        }
    }
}

impl RenderItem for &str {
    fn render(&self, area: Rect, frame: &mut Frame, _selected: bool) {
        if area.is_empty() {
            return;
        }
        let max_chars = area.width as usize;
        for (i, ch) in self.chars().take(max_chars).enumerate() {
            frame.buffer.set(area.x + i as u16, area.y, Cell::from_char(ch));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_virtualized() {
        let virt: Virtualized<String> = Virtualized::new(100);
        assert_eq!(virt.len(), 0);
        assert!(virt.is_empty());
    }

    #[test]
    fn test_push_and_len() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        virt.push(1);
        virt.push(2);
        virt.push(3);
        assert_eq!(virt.len(), 3);
        assert!(!virt.is_empty());
    }

    #[test]
    fn test_visible_range_fixed_height() {
        let mut virt: Virtualized<i32> = Virtualized::new(100).with_fixed_height(2);
        for i in 0..20 {
            virt.push(i);
        }
        // 10 items visible with height 2 in viewport 20
        let range = virt.visible_range(20);
        assert_eq!(range, 0..10);
    }

    #[test]
    fn test_visible_range_with_scroll() {
        let mut virt: Virtualized<i32> = Virtualized::new(100).with_fixed_height(1);
        for i in 0..50 {
            virt.push(i);
        }
        virt.scroll(10);
        let range = virt.visible_range(10);
        assert_eq!(range, 10..20);
    }

    #[test]
    fn test_render_range_with_overscan() {
        let mut virt: Virtualized<i32> = Virtualized::new(100)
            .with_fixed_height(1)
            .with_overscan(2);
        for i in 0..50 {
            virt.push(i);
        }
        virt.scroll(10);
        let range = virt.render_range(10);
        // Visible: 10..20, Overscan: 2
        // Render: 8..22
        assert_eq!(range, 8..22);
    }

    #[test]
    fn test_scroll_bounds() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        for i in 0..10 {
            virt.push(i);
        }

        // Can't scroll negative
        virt.scroll(-100);
        assert_eq!(virt.scroll_offset(), 0);

        // Can't scroll past end
        virt.scroll(100);
        assert_eq!(virt.scroll_offset(), 9);
    }

    #[test]
    fn test_scroll_to() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        for i in 0..20 {
            virt.push(i);
        }

        virt.scroll_to(15);
        assert_eq!(virt.scroll_offset(), 15);

        // Clamps to max
        virt.scroll_to(100);
        assert_eq!(virt.scroll_offset(), 19);
    }

    #[test]
    fn test_follow_mode() {
        let mut virt: Virtualized<i32> = Virtualized::new(100).with_follow(true);
        virt.set_visible_count(5);

        for i in 0..10 {
            virt.push(i);
        }

        // Should be at bottom
        assert!(virt.is_at_bottom());

        // Manual scroll disables follow
        virt.scroll(-5);
        assert!(!virt.follow_mode());
    }

    #[test]
    fn test_scroll_to_start_and_end() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        virt.set_visible_count(5);
        for i in 0..20 {
            virt.push(i);
        }

        // scroll_to_start goes to top and disables follow
        virt.scroll_to(10);
        virt.set_follow(true);
        virt.scroll_to_start();
        assert_eq!(virt.scroll_offset(), 0);
        assert!(!virt.follow_mode());

        // scroll_to_end goes to bottom and enables follow
        virt.scroll_to_end();
        assert!(virt.is_at_bottom());
        assert!(virt.follow_mode());
    }

    #[test]
    fn test_virtualized_page_navigation() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        virt.set_visible_count(5);
        for i in 0..30 {
            virt.push(i);
        }

        virt.scroll_to(15);
        virt.page_up();
        assert_eq!(virt.scroll_offset(), 10);

        virt.page_down();
        assert_eq!(virt.scroll_offset(), 15);

        // Page up at start clamps to 0
        virt.scroll_to(2);
        virt.page_up();
        assert_eq!(virt.scroll_offset(), 0);
    }

    #[test]
    fn test_height_cache() {
        let mut cache = HeightCache::new(1, 100);

        // Default value
        assert_eq!(cache.get(0), 1);
        assert_eq!(cache.get(50), 1);

        // Set value
        cache.set(5, 3);
        assert_eq!(cache.get(5), 3);

        // Other indices still default
        assert_eq!(cache.get(4), 1);
        assert_eq!(cache.get(6), 1);
    }

    #[test]
    fn test_clear() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        for i in 0..10 {
            virt.push(i);
        }
        virt.scroll(5);

        virt.clear();
        assert_eq!(virt.len(), 0);
        assert_eq!(virt.scroll_offset(), 0);
    }

    #[test]
    fn test_get_item() {
        let mut virt: Virtualized<String> = Virtualized::new(100);
        virt.push("hello".to_string());
        virt.push("world".to_string());

        assert_eq!(virt.get(0), Some(&"hello".to_string()));
        assert_eq!(virt.get(1), Some(&"world".to_string()));
        assert_eq!(virt.get(2), None);
    }

    #[test]
    fn test_external_storage_len() {
        let mut virt: Virtualized<i32> = Virtualized::external(1000, 100);
        assert_eq!(virt.len(), 1000);

        virt.set_external_len(2000);
        assert_eq!(virt.len(), 2000);
    }

    #[test]
    fn test_momentum_scrolling() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        for i in 0..50 {
            virt.push(i);
        }

        virt.fling(10.0);

        // Simulate tick
        virt.tick(Duration::from_millis(100));

        // Should have scrolled
        assert!(virt.scroll_offset() > 0);
    }

    // ========================================================================
    // VirtualizedListState tests
    // ========================================================================

    #[test]
    fn test_virtualized_list_state_new() {
        let state = VirtualizedListState::new();
        assert_eq!(state.selected, None);
        assert_eq!(state.scroll_offset(), 0);
        assert_eq!(state.visible_count(), 0);
    }

    #[test]
    fn test_virtualized_list_state_select_next() {
        let mut state = VirtualizedListState::new();

        state.select_next(10);
        assert_eq!(state.selected, Some(0));

        state.select_next(10);
        assert_eq!(state.selected, Some(1));

        // At last item, stays there
        state.selected = Some(9);
        state.select_next(10);
        assert_eq!(state.selected, Some(9));
    }

    #[test]
    fn test_virtualized_list_state_select_previous() {
        let mut state = VirtualizedListState::new();
        state.selected = Some(5);

        state.select_previous(10);
        assert_eq!(state.selected, Some(4));

        state.selected = Some(0);
        state.select_previous(10);
        assert_eq!(state.selected, Some(0));
    }

    #[test]
    fn test_virtualized_list_state_scroll() {
        let mut state = VirtualizedListState::new();

        state.scroll(5, 20);
        assert_eq!(state.scroll_offset(), 5);

        state.scroll(-3, 20);
        assert_eq!(state.scroll_offset(), 2);

        // Can't scroll negative
        state.scroll(-100, 20);
        assert_eq!(state.scroll_offset(), 0);

        // Can't scroll past end
        state.scroll(100, 20);
        assert_eq!(state.scroll_offset(), 19);
    }

    #[test]
    fn test_virtualized_list_state_follow_mode() {
        let mut state = VirtualizedListState::new().with_follow(true);
        assert!(state.follow_mode());

        // Manual scroll disables follow
        state.scroll(5, 20);
        assert!(!state.follow_mode());
    }

    #[test]
    fn test_render_item_string() {
        // Verify String implements RenderItem
        let s = String::from("hello");
        assert_eq!(s.height(), 1);
    }

    #[test]
    fn test_page_up_down() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        for i in 0..50 {
            virt.push(i);
        }
        virt.set_visible_count(10);

        // Start at top
        assert_eq!(virt.scroll_offset(), 0);

        // Page down
        virt.page_down();
        assert_eq!(virt.scroll_offset(), 10);

        // Page down again
        virt.page_down();
        assert_eq!(virt.scroll_offset(), 20);

        // Page up
        virt.page_up();
        assert_eq!(virt.scroll_offset(), 10);

        // Page up again
        virt.page_up();
        assert_eq!(virt.scroll_offset(), 0);

        // Page up at top stays at 0
        virt.page_up();
        assert_eq!(virt.scroll_offset(), 0);
    }

    // ========================================================================
    // Performance invariant tests (bd-uo6v)
    // ========================================================================

    #[test]
    fn test_render_scales_with_visible_not_total() {
        use std::time::Instant;
        use ftui_render::grapheme_pool::GraphemePool;

        // Setup: VirtualizedList with 1K items
        let small_items: Vec<String> = (0..1_000).map(|i| format!("Line {}", i)).collect();
        let small_list = VirtualizedList::new(&small_items);
        let mut small_state = VirtualizedListState::new();

        let area = Rect::new(0, 0, 80, 24);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);

        // Warm up
        small_list.render(area, &mut frame, &mut small_state);

        let start = Instant::now();
        for _ in 0..100 {
            frame.buffer.clear();
            small_list.render(area, &mut frame, &mut small_state);
        }
        let small_time = start.elapsed();

        // Setup: VirtualizedList with 100K items
        let large_items: Vec<String> = (0..100_000).map(|i| format!("Line {}", i)).collect();
        let large_list = VirtualizedList::new(&large_items);
        let mut large_state = VirtualizedListState::new();

        // Warm up
        large_list.render(area, &mut frame, &mut large_state);

        let start = Instant::now();
        for _ in 0..100 {
            frame.buffer.clear();
            large_list.render(area, &mut frame, &mut large_state);
        }
        let large_time = start.elapsed();

        // 100K should be within 3x of 1K (both render ~24 items)
        assert!(
            large_time < small_time * 3,
            "Render does not scale O(visible): 1K={:?}, 100K={:?}",
            small_time,
            large_time
        );
    }

    #[test]
    fn test_scroll_is_constant_time() {
        use std::time::Instant;

        let mut small: Virtualized<i32> = Virtualized::new(1_000);
        for i in 0..1_000 {
            small.push(i);
        }
        small.set_visible_count(24);

        let mut large: Virtualized<i32> = Virtualized::new(100_000);
        for i in 0..100_000 {
            large.push(i);
        }
        large.set_visible_count(24);

        let iterations = 10_000;

        let start = Instant::now();
        for _ in 0..iterations {
            small.scroll(1);
            small.scroll(-1);
        }
        let small_time = start.elapsed();

        let start = Instant::now();
        for _ in 0..iterations {
            large.scroll(1);
            large.scroll(-1);
        }
        let large_time = start.elapsed();

        // Should be within 3x (both are O(1) operations)
        assert!(
            large_time < small_time * 3,
            "Scroll is not O(1): 1K={:?}, 100K={:?}",
            small_time,
            large_time
        );
    }

    #[test]
    fn test_memory_bounded_by_ring_capacity() {
        use crate::log_ring::LogRing;

        let mut ring: LogRing<String> = LogRing::new(1_000);

        // Add 100K items
        for i in 0..100_000 {
            ring.push(format!("Line {}", i));
        }

        // Only 1K in memory
        assert_eq!(ring.len(), 1_000);
        assert_eq!(ring.total_count(), 100_000);
        assert_eq!(ring.first_index(), 99_000);

        // Can still access recent items
        assert!(ring.get(99_999).is_some());
        assert!(ring.get(99_000).is_some());
        // Old items evicted
        assert!(ring.get(0).is_none());
        assert!(ring.get(98_999).is_none());
    }

    #[test]
    fn test_visible_range_constant_regardless_of_total() {
        let mut small: Virtualized<i32> = Virtualized::new(100);
        for i in 0..100 {
            small.push(i);
        }
        let small_range = small.visible_range(24);

        let mut large: Virtualized<i32> = Virtualized::new(100_000);
        for i in 0..100_000 {
            large.push(i);
        }
        let large_range = large.visible_range(24);

        // Both should return exactly 24 visible items
        assert_eq!(small_range.end - small_range.start, 24);
        assert_eq!(large_range.end - large_range.start, 24);
    }

    #[test]
    fn test_virtualized_list_state_page_up_down() {
        let mut state = VirtualizedListState::new();
        state.visible_count = 10;

        // Page down
        state.page_down(50);
        assert_eq!(state.scroll_offset(), 10);

        // Page down again
        state.page_down(50);
        assert_eq!(state.scroll_offset(), 20);

        // Page up
        state.page_up(50);
        assert_eq!(state.scroll_offset(), 10);

        // Page up again
        state.page_up(50);
        assert_eq!(state.scroll_offset(), 0);
    }
}
