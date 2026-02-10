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

use std::cell::Cell as StdCell;
use std::collections::VecDeque;
use std::ops::Range;
use std::time::Duration;

// Imports for future rendering support (currently unused but planned)
#[allow(unused_imports)]
use crate::scrollbar::{Scrollbar, ScrollbarOrientation, ScrollbarState};
#[allow(unused_imports)]
use crate::{StatefulWidget, set_style_area};
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
    visible_count: StdCell<usize>,
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
    /// Items have variable height, cached lazily (linear scan).
    Variable(HeightCache),
    /// Items have variable height with O(log n) scroll-to-index via Fenwick tree.
    VariableFenwick(VariableHeightsFenwick),
}

/// LRU cache for measured item heights.
#[derive(Debug, Clone)]
pub struct HeightCache {
    /// Height measurements indexed by (item index - base_offset).
    cache: Vec<Option<u16>>,
    /// Offset of the first entry in the cache (cache[0] corresponds to this item index).
    base_offset: usize,
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
            visible_count: StdCell::new(0),
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
            storage: VirtualizedStorage::External {
                len,
                cache_capacity,
            },
            scroll_offset: 0,
            visible_count: StdCell::new(0),
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

    /// Set variable heights with O(log n) Fenwick tree tracking.
    ///
    /// This is more efficient than `Variable(HeightCache)` for large lists
    /// as scroll-to-index mapping is O(log n) instead of O(visible).
    #[must_use]
    pub fn with_variable_heights_fenwick(mut self, default_height: u16, capacity: usize) -> Self {
        self.item_height =
            ItemHeight::VariableFenwick(VariableHeightsFenwick::new(default_height, capacity));
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
        self.visible_count.get()
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
            self.visible_count.set(0);
            return 0..0;
        }

        let items_visible = match &self.item_height {
            ItemHeight::Fixed(h) if *h > 0 => (viewport_height / h) as usize,
            ItemHeight::Fixed(_) => viewport_height as usize,
            ItemHeight::Variable(cache) => {
                // Sum heights until the next item would exceed viewport (O(visible))
                let mut count = 0;
                let mut total_height = 0u16;
                let start = self.scroll_offset;
                while start + count < self.len() {
                    let next = cache.get(start + count);
                    let proposed = total_height.saturating_add(next);
                    if proposed > viewport_height {
                        break;
                    }
                    total_height = proposed;
                    count += 1;
                }
                count
            }
            ItemHeight::VariableFenwick(tracker) => {
                // O(log n) using Fenwick tree
                tracker.visible_count(self.scroll_offset, viewport_height)
            }
        };

        let start = self.scroll_offset;
        let end = (start + items_visible).min(self.len());
        self.visible_count.set(items_visible);
        start..end
    }

    /// Get render range with overscan for smooth scrolling.
    #[must_use]
    pub fn render_range(&self, viewport_height: u16) -> Range<usize> {
        let visible = self.visible_range(viewport_height);
        let start = visible.start.saturating_sub(self.overscan);
        let end = visible.end.saturating_add(self.overscan).min(self.len());
        start..end
    }

    /// Scroll by delta (positive = down/forward).
    pub fn scroll(&mut self, delta: i32) {
        if self.is_empty() {
            return;
        }
        let visible_count = self.visible_count.get();
        let max_offset = if visible_count > 0 {
            self.len().saturating_sub(visible_count)
        } else {
            self.len().saturating_sub(1)
        };
        let new_offset = (self.scroll_offset as i64 + delta as i64)
            .max(0)
            .min(max_offset as i64);
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
        let visible_count = self.visible_count.get();
        if self.len() > visible_count && visible_count > 0 {
            self.scroll_offset = self.len().saturating_sub(visible_count);
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
        let visible_count = self.visible_count.get();
        if visible_count > 0 {
            self.scroll(-(visible_count as i32));
        }
    }

    /// Page down (scroll by visible count).
    pub fn page_down(&mut self) {
        let visible_count = self.visible_count.get();
        if visible_count > 0 {
            self.scroll(visible_count as i32);
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
        let visible_count = self.visible_count.get();
        if self.len() <= visible_count {
            true
        } else {
            self.scroll_offset >= self.len().saturating_sub(visible_count)
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
        self.visible_count.set(count);
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
    #[must_use = "use the returned item (if any)"]
    pub fn get(&self, idx: usize) -> Option<&T> {
        if let VirtualizedStorage::Owned(items) = &self.storage {
            items.get(idx)
        } else {
            None
        }
    }

    /// Get mutable item by index (owned storage only).
    #[must_use = "use the returned item (if any)"]
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
        if let VirtualizedStorage::Owned(items) = &mut self.storage
            && items.len() > max
        {
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
            base_offset: 0,
            default_height,
            capacity,
        }
    }

    /// Get height for item, returning default if not cached.
    #[must_use]
    pub fn get(&self, idx: usize) -> u16 {
        if idx < self.base_offset {
            return self.default_height;
        }
        let local = idx - self.base_offset;
        self.cache
            .get(local)
            .and_then(|h| *h)
            .unwrap_or(self.default_height)
    }

    /// Set height for item.
    pub fn set(&mut self, idx: usize, height: u16) {
        if self.capacity == 0 {
            return;
        }
        if idx < self.base_offset {
            // Index has been trimmed away; ignore
            return;
        }
        let mut local = idx - self.base_offset;
        if local >= self.capacity {
            // Large index jump: reset window to avoid huge allocations.
            self.base_offset = idx.saturating_add(1).saturating_sub(self.capacity);
            self.cache.clear();
            local = idx - self.base_offset;
        }
        if local >= self.cache.len() {
            self.cache.resize(local + 1, None);
        }
        self.cache[local] = Some(height);

        // Trim if over capacity: remove oldest entries and adjust base_offset
        if self.cache.len() > self.capacity {
            let to_remove = self.cache.len() - self.capacity;
            self.cache.drain(0..to_remove);
            self.base_offset += to_remove;
        }
    }

    /// Clear cached heights.
    pub fn clear(&mut self) {
        self.cache.clear();
        self.base_offset = 0;
    }
}

// ============================================================================
// VariableHeightsFenwick - O(log n) scroll-to-index mapping
// ============================================================================

use crate::fenwick::FenwickTree;

/// Variable height tracker using Fenwick tree for O(log n) prefix sum queries.
///
/// This enables efficient scroll offset to item index mapping for virtualized
/// lists with variable height items.
///
/// # Operations
///
/// | Operation | Time |
/// |-----------|------|
/// | `find_item_at_offset` | O(log n) |
/// | `offset_of_item` | O(log n) |
/// | `set_height` | O(log n) |
/// | `total_height` | O(log n) |
///
/// # Invariants
///
/// 1. `tree.prefix(i)` == sum of heights [0..=i]
/// 2. `find_item_at_offset(offset)` returns largest i where prefix(i-1) < offset
/// 3. Heights are u32 internally (u16 input widened for large lists)
#[derive(Debug, Clone)]
pub struct VariableHeightsFenwick {
    /// Fenwick tree storing item heights.
    tree: FenwickTree,
    /// Default height for items not yet measured.
    default_height: u16,
    /// Number of items tracked.
    len: usize,
}

impl Default for VariableHeightsFenwick {
    fn default() -> Self {
        Self::new(1, 0)
    }
}

impl VariableHeightsFenwick {
    /// Create a new height tracker with given default height and initial capacity.
    #[must_use]
    pub fn new(default_height: u16, capacity: usize) -> Self {
        let tree = if capacity > 0 {
            // Initialize with default heights
            let heights: Vec<u32> = vec![u32::from(default_height); capacity];
            FenwickTree::from_values(&heights)
        } else {
            FenwickTree::new(0)
        };
        Self {
            tree,
            default_height,
            len: capacity,
        }
    }

    /// Create from a slice of heights.
    #[must_use]
    pub fn from_heights(heights: &[u16], default_height: u16) -> Self {
        let heights_u32: Vec<u32> = heights.iter().map(|&h| u32::from(h)).collect();
        Self {
            tree: FenwickTree::from_values(&heights_u32),
            default_height,
            len: heights.len(),
        }
    }

    /// Number of items tracked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether tracking is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Get the default height for unmeasured items.
    #[must_use]
    pub fn default_height(&self) -> u16 {
        self.default_height
    }

    /// Get height of a specific item. O(log n).
    #[must_use]
    pub fn get(&self, idx: usize) -> u16 {
        if idx >= self.len {
            return self.default_height;
        }
        // Fenwick get returns the individual value at idx
        self.tree.get(idx).min(u32::from(u16::MAX)) as u16
    }

    /// Set height of a specific item. O(log n).
    pub fn set(&mut self, idx: usize, height: u16) {
        if idx >= self.len {
            // Need to resize
            self.resize(idx + 1);
        }
        self.tree.set(idx, u32::from(height));
    }

    /// Get the y-offset (in pixels/rows) of an item. O(log n).
    ///
    /// Returns the sum of heights of all items before `idx`.
    #[must_use]
    pub fn offset_of_item(&self, idx: usize) -> u32 {
        if idx == 0 || self.len == 0 {
            return 0;
        }
        let clamped = idx.min(self.len);
        if clamped > 0 {
            self.tree.prefix(clamped - 1)
        } else {
            0
        }
    }

    /// Find the item index at a given scroll offset. O(log n).
    ///
    /// Returns the index of the item that occupies the given offset.
    /// If offset is beyond all items, returns `self.len`.
    ///
    /// Item i occupies offsets [offset_of_item(i), offset_of_item(i+1)).
    #[must_use]
    pub fn find_item_at_offset(&self, offset: u32) -> usize {
        if self.len == 0 {
            return 0;
        }
        if offset == 0 {
            return 0;
        }
        // find_prefix returns largest i where prefix(i) <= offset
        // prefix(i) = sum of heights [0..=i] = y-coordinate just past item i
        // If prefix(i) <= offset, then offset is at or past the end of item i,
        // so offset is in item i+1.
        //
        // We use offset - 1 to check: if prefix(i) <= offset - 1, then offset > prefix(i),
        // meaning we're strictly past item i.
        // But we also need to handle the case where offset == prefix(i) exactly
        // (offset is first row of item i+1).
        match self.tree.find_prefix(offset) {
            Some(i) => {
                // prefix(i) <= offset
                // Item i spans [prefix(i-1), prefix(i)), so offset >= prefix(i)
                // means offset is in item i+1 or beyond
                (i + 1).min(self.len)
            }
            None => {
                // offset < prefix(0), so offset is within item 0
                0
            }
        }
    }

    /// Count how many items fit within a viewport starting at `start_idx`. O(log n).
    ///
    /// Returns the number of items that fit completely within `viewport_height`.
    #[must_use]
    pub fn visible_count(&self, start_idx: usize, viewport_height: u16) -> usize {
        if self.len == 0 || viewport_height == 0 {
            return 0;
        }
        let start = start_idx.min(self.len);
        let start_offset = self.offset_of_item(start);
        let end_offset = start_offset + u32::from(viewport_height);

        // Find last item that fits
        let end_idx = self.find_item_at_offset(end_offset);

        // Count items from start to end (exclusive of partially visible)
        if end_idx > start {
            // Check if end_idx item is fully visible
            let end_item_start = self.offset_of_item(end_idx);
            if end_item_start + u32::from(self.get(end_idx)) <= end_offset {
                end_idx - start + 1
            } else {
                end_idx - start
            }
        } else {
            // At least show one item if viewport has space
            if viewport_height > 0 && start < self.len {
                1
            } else {
                0
            }
        }
    }

    /// Get total height of all items. O(log n).
    #[must_use]
    pub fn total_height(&self) -> u32 {
        self.tree.total()
    }

    /// Resize the tracker to accommodate `new_len` items.
    ///
    /// New items are initialized with default height.
    pub fn resize(&mut self, new_len: usize) {
        if new_len == self.len {
            return;
        }
        self.tree.resize(new_len);
        // Set default heights for new items
        if new_len > self.len {
            for i in self.len..new_len {
                self.tree.set(i, u32::from(self.default_height));
            }
        }
        self.len = new_len;
    }

    /// Clear all height data.
    pub fn clear(&mut self) {
        self.tree = FenwickTree::new(0);
        self.len = 0;
    }

    /// Rebuild from a fresh set of heights.
    pub fn rebuild(&mut self, heights: &[u16]) {
        let heights_u32: Vec<u32> = heights.iter().map(|&h| u32::from(h)).collect();
        self.tree = FenwickTree::from_values(&heights_u32);
        self.len = heights.len();
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
    /// Optional persistence ID for state saving/restoration.
    persistence_id: Option<String>,
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
            persistence_id: None,
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

    /// Create with a persistence ID for state saving.
    #[must_use]
    pub fn with_persistence_id(mut self, id: impl Into<String>) -> Self {
        self.persistence_id = Some(id.into());
        self
    }

    /// Get the persistence ID, if set.
    #[must_use = "use the persistence id (if any)"]
    pub fn persistence_id(&self) -> Option<&str> {
        self.persistence_id.as_deref()
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
        let max_offset = if self.visible_count > 0 {
            total_items.saturating_sub(self.visible_count)
        } else {
            total_items.saturating_sub(1)
        };
        let new_offset = (self.scroll_offset as i64 + delta as i64)
            .max(0)
            .min(max_offset as i64);
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

// ============================================================================
// Stateful Persistence Implementation for VirtualizedListState
// ============================================================================

/// Persistable state for a [`VirtualizedListState`].
///
/// Contains the user-facing scroll state that should survive sessions.
/// Transient values like scroll_velocity and visible_count are not persisted.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(
    feature = "state-persistence",
    derive(serde::Serialize, serde::Deserialize)
)]
pub struct VirtualizedListPersistState {
    /// Selected item index.
    pub selected: Option<usize>,
    /// Scroll offset (first visible item).
    pub scroll_offset: usize,
    /// Whether follow mode is enabled.
    pub follow_mode: bool,
}

impl crate::stateful::Stateful for VirtualizedListState {
    type State = VirtualizedListPersistState;

    fn state_key(&self) -> crate::stateful::StateKey {
        crate::stateful::StateKey::new(
            "VirtualizedList",
            self.persistence_id.as_deref().unwrap_or("default"),
        )
    }

    fn save_state(&self) -> VirtualizedListPersistState {
        VirtualizedListPersistState {
            selected: self.selected,
            scroll_offset: self.scroll_offset,
            follow_mode: self.follow_mode,
        }
    }

    fn restore_state(&mut self, state: VirtualizedListPersistState) {
        self.selected = state.selected;
        self.scroll_offset = state.scroll_offset;
        self.follow_mode = state.follow_mode;
        // Reset transient values
        self.scroll_velocity = 0.0;
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
        let items_per_viewport = (area.height / self.fixed_height.max(1)) as usize;
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
            // Use saturating_sub to handle empty list case (total_items = 0)
            state.selected = if total_items > 0 {
                Some(total_items - 1)
            } else {
                None
            };
        }

        // Ensure visible range includes selected item
        if let Some(selected) = state.selected {
            if selected >= state.scroll_offset + items_per_viewport {
                state.scroll_offset = selected.saturating_sub(items_per_viewport.saturating_sub(1));
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
        let render_end = state
            .scroll_offset
            .saturating_add(items_per_viewport)
            .saturating_add(state.overscan)
            .min(total_items);

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

            // Check if item starts off-screen top (terminal y < 0)
            // We cannot render at negative coordinates, and clamping to 0 causes artifacts
            // (drawing top of item instead of bottom). Skip such items.
            if i32::from(area.y) + y_offset < 0 {
                continue;
            }

            // Calculate actual render area
            // Use i32 arithmetic to avoid overflow when casting y_offset to i16
            let y = i32::from(area.y)
                .saturating_add(y_offset)
                .clamp(0, i32::from(u16::MAX)) as u16;
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
            let scrollbar_area = Rect::new(area.right().saturating_sub(1), area.y, 1, area.height);

            let mut scrollbar_state =
                ScrollbarState::new(total_items, state.scroll_offset, items_per_viewport);

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
            frame
                .buffer
                .set(area.x.saturating_add(i as u16), area.y, Cell::from_char(ch));
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
            frame
                .buffer
                .set(area.x.saturating_add(i as u16), area.y, Cell::from_char(ch));
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
    fn test_visible_range_variable_height_clamps() {
        let mut cache = HeightCache::new(1, 16);
        cache.set(0, 3);
        cache.set(1, 3);
        cache.set(2, 3);
        let mut virt: Virtualized<i32> =
            Virtualized::new(10).with_item_height(ItemHeight::Variable(cache));
        for i in 0..3 {
            virt.push(i);
        }
        let range = virt.visible_range(5);
        assert_eq!(range, 0..1);
    }

    #[test]
    fn test_visible_range_variable_height_exact_fit() {
        let mut cache = HeightCache::new(1, 16);
        cache.set(0, 2);
        cache.set(1, 3);
        let mut virt: Virtualized<i32> =
            Virtualized::new(10).with_item_height(ItemHeight::Variable(cache));
        for i in 0..2 {
            virt.push(i);
        }
        let range = virt.visible_range(5);
        assert_eq!(range, 0..2);
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
    fn test_visible_range_variable_height_excludes_partial() {
        let mut cache = HeightCache::new(1, 16);
        cache.set(0, 6);
        cache.set(1, 6);
        let mut virt: Virtualized<i32> =
            Virtualized::new(100).with_item_height(ItemHeight::Variable(cache));
        virt.push(1);
        virt.push(2);
        virt.push(3);

        let range = virt.visible_range(10);
        assert_eq!(range, 0..1);
    }

    #[test]
    fn test_visible_range_variable_height_exact_fit_larger() {
        let mut cache = HeightCache::new(1, 16);
        cache.set(0, 4);
        cache.set(1, 6);
        let mut virt: Virtualized<i32> =
            Virtualized::new(100).with_item_height(ItemHeight::Variable(cache));
        virt.push(1);
        virt.push(2);
        virt.push(3);

        let range = virt.visible_range(10);
        assert_eq!(range, 0..2);
    }

    #[test]
    fn test_visible_range_variable_height_default_for_unmeasured() {
        let cache = HeightCache::new(2, 16);
        let mut virt: Virtualized<i32> =
            Virtualized::new(10).with_item_height(ItemHeight::Variable(cache));
        for i in 0..3 {
            virt.push(i);
        }

        // Default height = 2, viewport 5 fits 2 items (2 + 2) but not the third.
        let range = virt.visible_range(5);
        assert_eq!(range, 0..2);
    }

    #[test]
    fn test_render_range_with_overscan() {
        let mut virt: Virtualized<i32> =
            Virtualized::new(100).with_fixed_height(1).with_overscan(2);
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
    fn test_height_cache_large_index_window() {
        let mut cache = HeightCache::new(1, 8);
        cache.set(10_000, 4);
        assert_eq!(cache.get(10_000), 4);
        assert_eq!(cache.get(0), 1);
        assert!(cache.cache.len() <= cache.capacity);
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
        use ftui_render::grapheme_pool::GraphemePool;
        use std::time::Instant;

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
    fn render_partially_offscreen_top_skips_item() {
        use ftui_render::grapheme_pool::GraphemePool;

        // Items with height 2, each rendering its index as a character
        struct IndexedItem(usize);
        impl RenderItem for IndexedItem {
            fn render(&self, area: Rect, frame: &mut Frame, _selected: bool) {
                let ch = char::from_digit(self.0 as u32, 10).unwrap();
                for y in area.y..area.bottom() {
                    frame.buffer.set(area.x, y, Cell::from_char(ch));
                }
            }
            fn height(&self) -> u16 {
                2
            }
        }

        // Need 4+ items so scroll_offset=1 is valid:
        // items_per_viewport = 5/2 = 2, max_offset = 4-2 = 2
        let items = vec![
            IndexedItem(0),
            IndexedItem(1),
            IndexedItem(2),
            IndexedItem(3),
        ];
        let list = VirtualizedList::new(&items).fixed_height(2);

        // Scroll so item 1 is at top, item 0 is in overscan (above viewport)
        let mut state = VirtualizedListState::new().with_overscan(1);
        state.scroll_offset = 1; // Item 1 is top visible. Item 0 is in overscan.

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);

        // Render at y=0 (terminal top edge)
        list.render(Rect::new(0, 0, 10, 5), &mut frame, &mut state);

        // With scroll_offset=1 and overscan=1:
        // - render_start = 1 - 1 = 0 (include item 0 in overscan)
        // - Item 0 would render at y_offset = (0-1)*2 = -2
        // - area.y + y_offset = 0 + (-2) = -2 < 0, so item 0 must be SKIPPED
        // - Item 1 renders at y_offset = (1-1)*2 = 0
        //
        // Row 0 should be '1' (from Item 1), NOT '0' (from Item 0 ghosting)
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('1'));
    }

    #[test]
    fn render_bottom_boundary_clips_partial_item() {
        use ftui_render::grapheme_pool::GraphemePool;

        struct IndexedItem(u16);
        impl RenderItem for IndexedItem {
            fn render(&self, area: Rect, frame: &mut Frame, _selected: bool) {
                let ch = char::from_digit(self.0 as u32, 10).unwrap();
                for y in area.y..area.bottom() {
                    frame.buffer.set(area.x, y, Cell::from_char(ch));
                }
            }
            fn height(&self) -> u16 {
                2
            }
        }

        let items = vec![IndexedItem(0), IndexedItem(1), IndexedItem(2)];
        let list = VirtualizedList::new(&items)
            .fixed_height(2)
            .show_scrollbar(false);
        let mut state = VirtualizedListState::new();

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 4, &mut pool);

        // Viewport height 3 means the second item is only partially visible.
        list.render(Rect::new(0, 0, 4, 3), &mut frame, &mut state);

        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('0'));
        assert_eq!(frame.buffer.get(0, 1).unwrap().content.as_char(), Some('0'));
        assert_eq!(frame.buffer.get(0, 2).unwrap().content.as_char(), Some('1'));
        // Row outside the viewport should remain empty.
        assert_eq!(frame.buffer.get(0, 3).unwrap().content.as_char(), None);
    }

    #[test]
    fn render_after_fling_advances_visible_rows() {
        use ftui_render::grapheme_pool::GraphemePool;

        struct IndexedItem(u16);
        impl RenderItem for IndexedItem {
            fn render(&self, area: Rect, frame: &mut Frame, _selected: bool) {
                let ch = char::from_digit(self.0 as u32, 10).unwrap();
                for y in area.y..area.bottom() {
                    frame.buffer.set(area.x, y, Cell::from_char(ch));
                }
            }
        }

        let items: Vec<IndexedItem> = (0..10).map(IndexedItem).collect();
        let list = VirtualizedList::new(&items)
            .fixed_height(1)
            .show_scrollbar(false);
        let mut state = VirtualizedListState::new();

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 3, &mut pool);
        let area = Rect::new(0, 0, 4, 3);

        // Initial render establishes visible_count and baseline top row.
        list.render(area, &mut frame, &mut state);
        assert_eq!(state.scroll_offset(), 0);
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('0'));

        // Momentum scroll: 40.0 * 0.1s = 4 rows.
        state.fling(40.0);
        state.tick(Duration::from_millis(100), items.len());
        assert_eq!(state.scroll_offset(), 4);

        frame.buffer.clear();
        list.render(area, &mut frame, &mut state);
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('4'));
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

    // ========================================================================
    // VariableHeightsFenwick tests (bd-2zbk.7)
    // ========================================================================

    #[test]
    fn test_variable_heights_fenwick_new() {
        let tracker = VariableHeightsFenwick::new(2, 10);
        assert_eq!(tracker.len(), 10);
        assert!(!tracker.is_empty());
        assert_eq!(tracker.default_height(), 2);
    }

    #[test]
    fn test_variable_heights_fenwick_empty() {
        let tracker = VariableHeightsFenwick::new(1, 0);
        assert!(tracker.is_empty());
        assert_eq!(tracker.total_height(), 0);
    }

    #[test]
    fn test_variable_heights_fenwick_from_heights() {
        let heights = vec![3, 2, 5, 1, 4];
        let tracker = VariableHeightsFenwick::from_heights(&heights, 1);

        assert_eq!(tracker.len(), 5);
        assert_eq!(tracker.get(0), 3);
        assert_eq!(tracker.get(1), 2);
        assert_eq!(tracker.get(2), 5);
        assert_eq!(tracker.get(3), 1);
        assert_eq!(tracker.get(4), 4);
        assert_eq!(tracker.total_height(), 15);
    }

    #[test]
    fn test_variable_heights_fenwick_offset_of_item() {
        // Heights: [3, 2, 5, 1, 4] -> offsets: [0, 3, 5, 10, 11]
        let heights = vec![3, 2, 5, 1, 4];
        let tracker = VariableHeightsFenwick::from_heights(&heights, 1);

        assert_eq!(tracker.offset_of_item(0), 0);
        assert_eq!(tracker.offset_of_item(1), 3);
        assert_eq!(tracker.offset_of_item(2), 5);
        assert_eq!(tracker.offset_of_item(3), 10);
        assert_eq!(tracker.offset_of_item(4), 11);
        assert_eq!(tracker.offset_of_item(5), 15); // beyond end
    }

    #[test]
    fn test_variable_heights_fenwick_find_item_at_offset() {
        // Heights: [3, 2, 5, 1, 4] -> cumulative: [3, 5, 10, 11, 15]
        let heights = vec![3, 2, 5, 1, 4];
        let tracker = VariableHeightsFenwick::from_heights(&heights, 1);

        // Offset 0 should be item 0
        assert_eq!(tracker.find_item_at_offset(0), 0);
        // Offset 1 should be item 0 (within first item)
        assert_eq!(tracker.find_item_at_offset(1), 0);
        // Offset 3 should be item 1 (starts at offset 3)
        assert_eq!(tracker.find_item_at_offset(3), 1);
        // Offset 5 should be item 2
        assert_eq!(tracker.find_item_at_offset(5), 2);
        // Offset 10 should be item 3
        assert_eq!(tracker.find_item_at_offset(10), 3);
        // Offset 11 should be item 4
        assert_eq!(tracker.find_item_at_offset(11), 4);
        // Offset 15 should be end (beyond all items)
        assert_eq!(tracker.find_item_at_offset(15), 5);
    }

    #[test]
    fn test_variable_heights_fenwick_visible_count() {
        // Heights: [3, 2, 5, 1, 4]
        let heights = vec![3, 2, 5, 1, 4];
        let tracker = VariableHeightsFenwick::from_heights(&heights, 1);

        // Viewport 5: items 0 (h=3) + 1 (h=2) = 5 exactly
        assert_eq!(tracker.visible_count(0, 5), 2);

        // Viewport 4: item 0 (h=3) fits, item 1 (h=2) doesn't fit fully
        assert_eq!(tracker.visible_count(0, 4), 1);

        // Viewport 10: items 0+1+2 = 10 exactly
        assert_eq!(tracker.visible_count(0, 10), 3);

        // From item 2, viewport 6: item 2 (h=5) + item 3 (h=1) = 6
        assert_eq!(tracker.visible_count(2, 6), 2);
    }

    #[test]
    fn test_variable_heights_fenwick_set() {
        let mut tracker = VariableHeightsFenwick::new(1, 5);

        // All items should start with default height
        assert_eq!(tracker.get(0), 1);
        assert_eq!(tracker.total_height(), 5);

        // Set item 2 to height 10
        tracker.set(2, 10);
        assert_eq!(tracker.get(2), 10);
        assert_eq!(tracker.total_height(), 14); // 1+1+10+1+1
    }

    #[test]
    fn test_variable_heights_fenwick_resize() {
        let mut tracker = VariableHeightsFenwick::new(2, 3);
        assert_eq!(tracker.len(), 3);
        assert_eq!(tracker.total_height(), 6);

        // Grow
        tracker.resize(5);
        assert_eq!(tracker.len(), 5);
        assert_eq!(tracker.total_height(), 10);
        assert_eq!(tracker.get(4), 2);

        // Shrink
        tracker.resize(2);
        assert_eq!(tracker.len(), 2);
        assert_eq!(tracker.total_height(), 4);
    }

    #[test]
    fn test_virtualized_with_variable_heights_fenwick() {
        let mut virt: Virtualized<i32> = Virtualized::new(100).with_variable_heights_fenwick(2, 10);

        for i in 0..10 {
            virt.push(i);
        }

        // All items height 2, viewport 6 -> 3 items visible
        let range = virt.visible_range(6);
        assert_eq!(range.end - range.start, 3);
    }

    #[test]
    fn test_variable_heights_fenwick_performance() {
        use std::time::Instant;

        // Create large tracker
        let n = 100_000;
        let heights: Vec<u16> = (0..n).map(|i| (i % 10 + 1) as u16).collect();
        let tracker = VariableHeightsFenwick::from_heights(&heights, 1);

        // Warm up
        let _ = tracker.find_item_at_offset(500_000);
        let _ = tracker.offset_of_item(50_000);

        // Benchmark find_item_at_offset (O(log n))
        let start = Instant::now();
        let mut _sink = 0usize;
        for i in 0..10_000 {
            _sink = _sink.wrapping_add(tracker.find_item_at_offset((i * 50) as u32));
        }
        let find_time = start.elapsed();

        // Benchmark offset_of_item (O(log n))
        let start = Instant::now();
        let mut _sink2 = 0u32;
        for i in 0..10_000 {
            _sink2 = _sink2.wrapping_add(tracker.offset_of_item((i * 10) % n));
        }
        let offset_time = start.elapsed();

        eprintln!("=== VariableHeightsFenwick Performance (n={n}) ===");
        eprintln!("10k find_item_at_offset: {:?}", find_time);
        eprintln!("10k offset_of_item:      {:?}", offset_time);

        // Both should be under 50ms for 10k operations
        assert!(
            find_time < std::time::Duration::from_millis(50),
            "find_item_at_offset too slow: {:?}",
            find_time
        );
        assert!(
            offset_time < std::time::Duration::from_millis(50),
            "offset_of_item too slow: {:?}",
            offset_time
        );
    }

    #[test]
    fn test_variable_heights_fenwick_scales_logarithmically() {
        use std::time::Instant;

        // Small dataset
        let small_n = 1_000;
        let small_heights: Vec<u16> = (0..small_n).map(|i| (i % 5 + 1) as u16).collect();
        let small_tracker = VariableHeightsFenwick::from_heights(&small_heights, 1);

        // Large dataset
        let large_n = 100_000;
        let large_heights: Vec<u16> = (0..large_n).map(|i| (i % 5 + 1) as u16).collect();
        let large_tracker = VariableHeightsFenwick::from_heights(&large_heights, 1);

        let iterations = 5_000;

        // Time small
        let start = Instant::now();
        for i in 0..iterations {
            let _ = small_tracker.find_item_at_offset((i * 2) as u32);
        }
        let small_time = start.elapsed();

        // Time large
        let start = Instant::now();
        for i in 0..iterations {
            let _ = large_tracker.find_item_at_offset((i * 200) as u32);
        }
        let large_time = start.elapsed();

        // Large should be within 5x of small (O(log n) vs O(n) would be 100x)
        assert!(
            large_time < small_time * 5,
            "Not O(log n): small={:?}, large={:?}",
            small_time,
            large_time
        );
    }

    // ========================================================================
    // Edge-case tests (bd-2f15w)
    // ========================================================================

    // ── Virtualized: construction & empty state ─────────────────────────

    #[test]
    fn new_zero_capacity() {
        let virt: Virtualized<i32> = Virtualized::new(0);
        assert_eq!(virt.len(), 0);
        assert!(virt.is_empty());
        assert_eq!(virt.scroll_offset(), 0);
        assert_eq!(virt.visible_count(), 0);
        assert!(!virt.follow_mode());
    }

    #[test]
    fn external_zero_len_zero_cache() {
        let virt: Virtualized<i32> = Virtualized::external(0, 0);
        assert_eq!(virt.len(), 0);
        assert!(virt.is_empty());
    }

    #[test]
    fn external_storage_returns_none_for_get() {
        let virt: Virtualized<i32> = Virtualized::external(100, 10);
        assert_eq!(virt.get(0), None);
        assert_eq!(virt.get(50), None);
    }

    #[test]
    fn external_storage_returns_none_for_get_mut() {
        let mut virt: Virtualized<i32> = Virtualized::external(100, 10);
        assert!(virt.get_mut(0).is_none());
    }

    #[test]
    fn push_on_external_is_noop() {
        let mut virt: Virtualized<i32> = Virtualized::external(5, 10);
        virt.push(42);
        // Length unchanged because push only works on Owned
        assert_eq!(virt.len(), 5);
    }

    #[test]
    fn iter_on_external_is_empty() {
        let virt: Virtualized<i32> = Virtualized::external(100, 10);
        assert_eq!(virt.iter().count(), 0);
    }

    #[test]
    fn set_external_len_on_owned_is_noop() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        virt.push(1);
        virt.set_external_len(999);
        assert_eq!(virt.len(), 1); // unchanged
    }

    // ── Virtualized: visible_range edge cases ───────────────────────────

    #[test]
    fn visible_range_zero_viewport() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        virt.push(1);
        let range = virt.visible_range(0);
        assert_eq!(range, 0..0);
        assert_eq!(virt.visible_count(), 0);
    }

    #[test]
    fn visible_range_empty_container() {
        let virt: Virtualized<i32> = Virtualized::new(100);
        let range = virt.visible_range(24);
        assert_eq!(range, 0..0);
    }

    #[test]
    fn visible_range_fixed_height_zero() {
        // Fixed(0) should not divide by zero; falls through to viewport_height items
        let mut virt: Virtualized<i32> = Virtualized::new(100).with_fixed_height(0);
        for i in 0..10 {
            virt.push(i);
        }
        let range = virt.visible_range(5);
        // ItemHeight::Fixed(0) → viewport_height as usize = 5
        assert_eq!(range, 0..5);
    }

    #[test]
    fn visible_range_fewer_items_than_viewport() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        for i in 0..3 {
            virt.push(i);
        }
        let range = virt.visible_range(24);
        // Only 3 items, viewport fits 24
        assert_eq!(range, 0..3);
    }

    #[test]
    fn visible_range_single_item() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        virt.push(42);
        let range = virt.visible_range(1);
        assert_eq!(range, 0..1);
    }

    // ── Virtualized: render_range edge cases ────────────────────────────

    #[test]
    fn render_range_at_start_clamps_overscan() {
        let mut virt: Virtualized<i32> =
            Virtualized::new(100).with_fixed_height(1).with_overscan(5);
        for i in 0..20 {
            virt.push(i);
        }
        // At scroll_offset=0, start.saturating_sub(5) = 0
        let range = virt.render_range(10);
        assert_eq!(range.start, 0);
    }

    #[test]
    fn render_range_at_end_clamps_overscan() {
        let mut virt: Virtualized<i32> =
            Virtualized::new(100).with_fixed_height(1).with_overscan(5);
        for i in 0..20 {
            virt.push(i);
        }
        virt.set_visible_count(10);
        virt.scroll_to(10); // offset=10, visible 10..20
        let range = virt.render_range(10);
        // end = min(20 + 5, 20) = 20
        assert_eq!(range.end, 20);
    }

    #[test]
    fn render_range_zero_overscan() {
        let mut virt: Virtualized<i32> =
            Virtualized::new(100).with_fixed_height(1).with_overscan(0);
        for i in 0..20 {
            virt.push(i);
        }
        virt.set_visible_count(10);
        virt.scroll_to(5);
        let range = virt.render_range(10);
        // No overscan: render_range == visible_range
        let visible = virt.visible_range(10);
        assert_eq!(range, visible);
    }

    // ── Virtualized: scroll edge cases ──────────────────────────────────

    #[test]
    fn scroll_on_empty_is_noop() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        virt.scroll(10);
        assert_eq!(virt.scroll_offset(), 0);
    }

    #[test]
    fn scroll_delta_zero_does_not_disable_follow() {
        let mut virt: Virtualized<i32> = Virtualized::new(100).with_follow(true);
        virt.push(1);
        virt.scroll(0);
        // delta=0 doesn't disable follow_mode
        assert!(virt.follow_mode());
    }

    #[test]
    fn scroll_negative_beyond_start() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        for i in 0..10 {
            virt.push(i);
        }
        virt.scroll(-1);
        assert_eq!(virt.scroll_offset(), 0);
    }

    #[test]
    fn scroll_to_on_empty() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        // scroll_to on empty: idx.min(0.saturating_sub(1)) = idx.min(0) = 0
        virt.scroll_to(100);
        assert_eq!(virt.scroll_offset(), 0);
    }

    #[test]
    fn scroll_to_top_already_at_top() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        virt.push(1);
        virt.scroll_to_top();
        assert_eq!(virt.scroll_offset(), 0);
    }

    #[test]
    fn scroll_to_bottom_fewer_items_than_visible() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        virt.set_visible_count(10);
        for i in 0..3 {
            virt.push(i);
        }
        virt.scroll_to_bottom();
        // len (3) <= visible_count (10), so offset = 0
        assert_eq!(virt.scroll_offset(), 0);
    }

    #[test]
    fn scroll_to_bottom_visible_count_zero() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        for i in 0..20 {
            virt.push(i);
        }
        // visible_count=0 (default), scroll_to_bottom goes to offset=0
        virt.scroll_to_bottom();
        assert_eq!(virt.scroll_offset(), 0);
    }

    // ── Virtualized: page navigation edge cases ─────────────────────────

    #[test]
    fn page_up_visible_count_zero_is_noop() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        for i in 0..20 {
            virt.push(i);
        }
        virt.scroll_to(10);
        // visible_count=0, page_up is no-op
        virt.page_up();
        assert_eq!(virt.scroll_offset(), 10);
    }

    #[test]
    fn page_down_visible_count_zero_is_noop() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        for i in 0..20 {
            virt.push(i);
        }
        // visible_count=0, page_down is no-op
        virt.page_down();
        assert_eq!(virt.scroll_offset(), 0);
    }

    // ── Virtualized: is_at_bottom edge cases ────────────────────────────

    #[test]
    fn is_at_bottom_fewer_items_than_visible() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        virt.set_visible_count(10);
        for i in 0..3 {
            virt.push(i);
        }
        assert!(virt.is_at_bottom());
    }

    #[test]
    fn is_at_bottom_empty() {
        let virt: Virtualized<i32> = Virtualized::new(100);
        // len=0 <= visible_count=0, so true
        assert!(virt.is_at_bottom());
    }

    // ── Virtualized: trim_front edge cases ──────────────────────────────

    #[test]
    fn trim_front_under_max_returns_zero() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        for i in 0..5 {
            virt.push(i);
        }
        let removed = virt.trim_front(10);
        assert_eq!(removed, 0);
        assert_eq!(virt.len(), 5);
    }

    #[test]
    fn trim_front_adjusts_scroll_offset() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        for i in 0..20 {
            virt.push(i);
        }
        virt.scroll_to(10);
        let removed = virt.trim_front(15);
        assert_eq!(removed, 5);
        assert_eq!(virt.len(), 15);
        // scroll_offset adjusted: 10 - 5 = 5
        assert_eq!(virt.scroll_offset(), 5);
    }

    #[test]
    fn trim_front_scroll_offset_saturates_to_zero() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        for i in 0..20 {
            virt.push(i);
        }
        virt.scroll_to(2);
        let removed = virt.trim_front(10);
        assert_eq!(removed, 10);
        // scroll_offset 2 - 10 saturates to 0
        assert_eq!(virt.scroll_offset(), 0);
    }

    #[test]
    fn trim_front_on_external_returns_zero() {
        let mut virt: Virtualized<i32> = Virtualized::external(100, 10);
        let removed = virt.trim_front(5);
        assert_eq!(removed, 0);
    }

    // ── Virtualized: clear edge cases ───────────────────────────────────

    #[test]
    fn clear_on_external_resets_scroll() {
        let mut virt: Virtualized<i32> = Virtualized::external(100, 10);
        virt.scroll_to(50);
        virt.clear();
        assert_eq!(virt.scroll_offset(), 0);
        // External len unchanged since clear only clears Owned
        assert_eq!(virt.len(), 100);
    }

    // ── Virtualized: momentum scrolling edge cases ──────────────────────

    #[test]
    fn tick_zero_velocity_is_noop() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        for i in 0..20 {
            virt.push(i);
        }
        virt.tick(Duration::from_millis(100));
        assert_eq!(virt.scroll_offset(), 0);
    }

    #[test]
    fn tick_below_threshold_stops_momentum() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        for i in 0..20 {
            virt.push(i);
        }
        virt.fling(0.05); // below 0.1 threshold
        virt.tick(Duration::from_millis(100));
        // velocity <= 0.1, so it's zeroed out
        assert_eq!(virt.scroll_offset(), 0);
    }

    #[test]
    fn tick_zero_duration_no_scroll() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        for i in 0..50 {
            virt.push(i);
        }
        virt.fling(100.0);
        virt.tick(Duration::ZERO);
        // delta = (100.0 * 0.0) as i32 = 0, no scroll
        assert_eq!(virt.scroll_offset(), 0);
    }

    #[test]
    fn fling_negative_scrolls_up() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        for i in 0..50 {
            virt.push(i);
        }
        virt.scroll(20);
        let before = virt.scroll_offset();
        virt.fling(-50.0);
        virt.tick(Duration::from_millis(100));
        assert!(virt.scroll_offset() < before);
    }

    // ── Virtualized: follow mode edge cases ─────────────────────────────

    #[test]
    fn follow_mode_auto_scrolls_on_push() {
        let mut virt: Virtualized<i32> = Virtualized::new(100).with_follow(true);
        virt.set_visible_count(5);
        for i in 0..20 {
            virt.push(i);
        }
        // With follow mode, should be at bottom
        assert!(virt.is_at_bottom());
        assert_eq!(virt.scroll_offset(), 15); // 20 - 5
    }

    #[test]
    fn set_follow_false_does_not_scroll() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        virt.set_visible_count(5);
        for i in 0..20 {
            virt.push(i);
        }
        virt.scroll_to(5);
        virt.set_follow(false);
        assert_eq!(virt.scroll_offset(), 5); // unchanged
    }

    #[test]
    fn scroll_to_start_disables_follow() {
        let mut virt: Virtualized<i32> = Virtualized::new(100).with_follow(true);
        virt.set_visible_count(5);
        for i in 0..20 {
            virt.push(i);
        }
        virt.scroll_to_start();
        assert!(!virt.follow_mode());
        assert_eq!(virt.scroll_offset(), 0);
    }

    #[test]
    fn scroll_to_end_enables_follow() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        virt.set_visible_count(5);
        for i in 0..20 {
            virt.push(i);
        }
        assert!(!virt.follow_mode());
        virt.scroll_to_end();
        assert!(virt.follow_mode());
        assert!(virt.is_at_bottom());
    }

    #[test]
    fn external_follow_mode_scrolls_on_set_external_len() {
        let mut virt: Virtualized<i32> = Virtualized::external(10, 100).with_follow(true);
        virt.set_visible_count(5);
        virt.set_external_len(20);
        assert_eq!(virt.len(), 20);
        assert!(virt.is_at_bottom());
    }

    // ── Virtualized: builder chain ──────────────────────────────────────

    #[test]
    fn builder_chain_all_options() {
        let virt: Virtualized<i32> = Virtualized::new(100)
            .with_fixed_height(3)
            .with_overscan(5)
            .with_follow(true);
        assert!(virt.follow_mode());
        // Verify visible_range uses height=3
        // (no items, so empty range regardless)
        let range = virt.visible_range(9);
        assert_eq!(range, 0..0);
    }

    // ── HeightCache edge cases ──────────────────────────────────────────

    #[test]
    fn height_cache_default() {
        let cache = HeightCache::default();
        assert_eq!(cache.get(0), 1); // default_height=1
        assert_eq!(cache.capacity, 1000);
    }

    #[test]
    fn height_cache_get_before_base_offset() {
        let mut cache = HeightCache::new(5, 100);
        // Set something to push base_offset forward
        cache.set(200, 10); // This resets window since 200 > capacity
        // Index 0 < base_offset, returns default
        assert_eq!(cache.get(0), 5);
    }

    #[test]
    fn height_cache_set_before_base_offset_ignored() {
        let mut cache = HeightCache::new(5, 100);
        cache.set(200, 10);
        let base = cache.base_offset;
        cache.set(0, 99); // before base_offset, should be ignored
        assert_eq!(cache.get(0), 5); // still default
        assert_eq!(cache.base_offset, base); // unchanged
    }

    #[test]
    fn height_cache_capacity_zero_ignores_all_sets() {
        let mut cache = HeightCache::new(3, 0);
        cache.set(0, 10);
        cache.set(5, 20);
        // Everything returns default since capacity=0
        assert_eq!(cache.get(0), 3);
        assert_eq!(cache.get(5), 3);
    }

    #[test]
    fn height_cache_clear_resets_base() {
        let mut cache = HeightCache::new(1, 100);
        cache.set(50, 10);
        cache.clear();
        assert_eq!(cache.base_offset, 0);
        assert_eq!(cache.get(50), 1); // back to default
    }

    #[test]
    fn height_cache_eviction_trims_oldest() {
        let mut cache = HeightCache::new(1, 4);
        // Set indices 0..6 to fill and trigger eviction
        for i in 0..6 {
            cache.set(i, (i + 10) as u16);
        }
        // Cache capacity=4, so indices 0-1 should be evicted
        assert!(cache.cache.len() <= cache.capacity);
        // Recent indices should be accessible
        assert_eq!(cache.get(5), 15);
        // Old indices return default
        assert_eq!(cache.get(0), 1);
    }

    // ── VariableHeightsFenwick edge cases ───────────────────────────────

    #[test]
    fn fenwick_default_is_empty() {
        let tracker = VariableHeightsFenwick::default();
        assert!(tracker.is_empty());
        assert_eq!(tracker.len(), 0);
        assert_eq!(tracker.total_height(), 0);
        assert_eq!(tracker.default_height(), 1);
    }

    #[test]
    fn fenwick_get_beyond_len_returns_default() {
        let tracker = VariableHeightsFenwick::new(3, 5);
        assert_eq!(tracker.get(5), 3); // beyond len
        assert_eq!(tracker.get(100), 3);
    }

    #[test]
    fn fenwick_set_beyond_len_resizes() {
        let mut tracker = VariableHeightsFenwick::new(2, 3);
        assert_eq!(tracker.len(), 3);
        tracker.set(10, 7);
        assert!(tracker.len() > 10);
        assert_eq!(tracker.get(10), 7);
    }

    #[test]
    fn fenwick_offset_of_item_zero_always_zero() {
        let tracker = VariableHeightsFenwick::new(5, 10);
        assert_eq!(tracker.offset_of_item(0), 0);

        let empty = VariableHeightsFenwick::new(5, 0);
        assert_eq!(empty.offset_of_item(0), 0);
    }

    #[test]
    fn fenwick_find_item_at_offset_empty() {
        let tracker = VariableHeightsFenwick::new(1, 0);
        assert_eq!(tracker.find_item_at_offset(0), 0);
        assert_eq!(tracker.find_item_at_offset(100), 0);
    }

    #[test]
    fn fenwick_visible_count_zero_viewport() {
        let tracker = VariableHeightsFenwick::new(2, 10);
        assert_eq!(tracker.visible_count(0, 0), 0);
    }

    #[test]
    fn fenwick_visible_count_start_beyond_len() {
        let tracker = VariableHeightsFenwick::new(2, 5);
        // start_idx clamped to len
        let count = tracker.visible_count(100, 10);
        // start=5 (clamped), offset=total, no items visible
        assert_eq!(count, 0);
    }

    #[test]
    fn fenwick_clear_then_operations() {
        let mut tracker = VariableHeightsFenwick::new(3, 5);
        assert_eq!(tracker.total_height(), 15);
        tracker.clear();
        assert_eq!(tracker.len(), 0);
        assert_eq!(tracker.total_height(), 0);
        assert_eq!(tracker.find_item_at_offset(0), 0);
    }

    #[test]
    fn fenwick_rebuild_replaces_data() {
        let mut tracker = VariableHeightsFenwick::new(1, 10);
        assert_eq!(tracker.total_height(), 10);
        tracker.rebuild(&[5, 3, 2]);
        assert_eq!(tracker.len(), 3);
        assert_eq!(tracker.total_height(), 10);
        assert_eq!(tracker.get(0), 5);
        assert_eq!(tracker.get(1), 3);
        assert_eq!(tracker.get(2), 2);
    }

    #[test]
    fn fenwick_resize_same_size_is_noop() {
        let mut tracker = VariableHeightsFenwick::new(2, 5);
        tracker.set(2, 10);
        tracker.resize(5);
        // Item 2 still has custom height
        assert_eq!(tracker.get(2), 10);
        assert_eq!(tracker.len(), 5);
    }

    // ── VirtualizedListState edge cases ─────────────────────────────────

    #[test]
    fn list_state_default_matches_new() {
        let d = VirtualizedListState::default();
        let n = VirtualizedListState::new();
        assert_eq!(d.selected, n.selected);
        assert_eq!(d.scroll_offset(), n.scroll_offset());
        assert_eq!(d.visible_count(), n.visible_count());
        assert_eq!(d.follow_mode(), n.follow_mode());
    }

    #[test]
    fn list_state_select_next_on_empty() {
        let mut state = VirtualizedListState::new();
        state.select_next(0);
        assert_eq!(state.selected, None);
    }

    #[test]
    fn list_state_select_previous_on_empty() {
        let mut state = VirtualizedListState::new();
        state.select_previous(0);
        assert_eq!(state.selected, None);
    }

    #[test]
    fn list_state_select_previous_from_none() {
        let mut state = VirtualizedListState::new();
        state.select_previous(10);
        assert_eq!(state.selected, Some(0));
    }

    #[test]
    fn list_state_select_next_from_none() {
        let mut state = VirtualizedListState::new();
        state.select_next(10);
        assert_eq!(state.selected, Some(0));
    }

    #[test]
    fn list_state_scroll_zero_items() {
        let mut state = VirtualizedListState::new();
        state.scroll(10, 0);
        assert_eq!(state.scroll_offset(), 0);
    }

    #[test]
    fn list_state_scroll_to_clamps() {
        let mut state = VirtualizedListState::new();
        state.scroll_to(100, 10);
        assert_eq!(state.scroll_offset(), 9);
    }

    #[test]
    fn list_state_scroll_to_bottom_zero_items() {
        let mut state = VirtualizedListState::new();
        state.scroll_to_bottom(0);
        assert_eq!(state.scroll_offset(), 0);
    }

    #[test]
    fn list_state_is_at_bottom_zero_items() {
        let state = VirtualizedListState::new();
        assert!(state.is_at_bottom(0));
    }

    #[test]
    fn list_state_page_up_visible_count_zero() {
        let mut state = VirtualizedListState::new();
        state.scroll_offset = 5;
        state.page_up(20);
        // visible_count=0, no-op
        assert_eq!(state.scroll_offset(), 5);
    }

    #[test]
    fn list_state_page_down_visible_count_zero() {
        let mut state = VirtualizedListState::new();
        state.page_down(20);
        // visible_count=0, no-op
        assert_eq!(state.scroll_offset(), 0);
    }

    #[test]
    fn list_state_set_follow_false_no_scroll() {
        let mut state = VirtualizedListState::new();
        state.scroll_offset = 5;
        state.set_follow(false, 20);
        assert_eq!(state.scroll_offset(), 5); // unchanged
        assert!(!state.follow_mode());
    }

    #[test]
    fn list_state_persistence_id() {
        let state = VirtualizedListState::new().with_persistence_id("my-list");
        assert_eq!(state.persistence_id(), Some("my-list"));
    }

    #[test]
    fn list_state_persistence_id_none() {
        let state = VirtualizedListState::new();
        assert_eq!(state.persistence_id(), None);
    }

    #[test]
    fn list_state_momentum_tick_zero_items() {
        let mut state = VirtualizedListState::new();
        state.fling(50.0);
        state.tick(Duration::from_millis(100), 0);
        // total_items=0, scroll is no-op
        assert_eq!(state.scroll_offset(), 0);
    }

    // ── VirtualizedListPersistState edge cases ──────────────────────────

    #[test]
    fn persist_state_default() {
        let ps = VirtualizedListPersistState::default();
        assert_eq!(ps.selected, None);
        assert_eq!(ps.scroll_offset, 0);
        assert!(!ps.follow_mode);
    }

    #[test]
    fn persist_state_eq() {
        let a = VirtualizedListPersistState {
            selected: Some(5),
            scroll_offset: 10,
            follow_mode: true,
        };
        let b = a.clone();
        assert_eq!(a, b);
    }

    // ── Stateful trait impl edge cases ──────────────────────────────────

    #[test]
    fn stateful_state_key_with_persistence_id() {
        use crate::stateful::Stateful;
        let state = VirtualizedListState::new().with_persistence_id("logs");
        let key = state.state_key();
        assert_eq!(key.widget_type, "VirtualizedList");
        assert_eq!(key.instance_id, "logs");
    }

    #[test]
    fn stateful_state_key_default_instance() {
        use crate::stateful::Stateful;
        let state = VirtualizedListState::new();
        let key = state.state_key();
        assert_eq!(key.instance_id, "default");
    }

    #[test]
    fn stateful_save_restore_roundtrip() {
        use crate::stateful::Stateful;
        let mut state = VirtualizedListState::new();
        state.selected = Some(7);
        state.scroll_offset = 15;
        state.follow_mode = true;
        state.scroll_velocity = 42.0; // transient — not persisted

        let saved = state.save_state();
        assert_eq!(saved.selected, Some(7));
        assert_eq!(saved.scroll_offset, 15);
        assert!(saved.follow_mode);

        let mut restored = VirtualizedListState::new();
        restored.scroll_velocity = 99.0;
        restored.restore_state(saved);
        assert_eq!(restored.selected, Some(7));
        assert_eq!(restored.scroll_offset, 15);
        assert!(restored.follow_mode);
        // velocity reset to 0 on restore
        assert_eq!(restored.scroll_velocity, 0.0);
    }

    // ── VirtualizedList widget edge cases ───────────────────────────────

    #[test]
    fn virtualized_list_builder() {
        let items: Vec<String> = vec!["a".into()];
        let list = VirtualizedList::new(&items)
            .style(Style::default())
            .highlight_style(Style::default())
            .show_scrollbar(false)
            .fixed_height(3);
        assert_eq!(list.fixed_height, 3);
        assert!(!list.show_scrollbar);
    }

    // ── VirtualizedStorage Debug/Clone ───────────────────────────────────

    #[test]
    fn virtualized_storage_debug() {
        let storage: VirtualizedStorage<i32> = VirtualizedStorage::Owned(VecDeque::new());
        let dbg = format!("{:?}", storage);
        assert!(dbg.contains("Owned"));

        let ext: VirtualizedStorage<i32> = VirtualizedStorage::External {
            len: 100,
            cache_capacity: 10,
        };
        let dbg = format!("{:?}", ext);
        assert!(dbg.contains("External"));
    }

    #[test]
    fn virtualized_clone() {
        let mut virt: Virtualized<i32> = Virtualized::new(100);
        virt.push(1);
        virt.push(2);
        let cloned = virt.clone();
        assert_eq!(cloned.len(), 2);
        assert_eq!(cloned.get(0), Some(&1));
    }
}
