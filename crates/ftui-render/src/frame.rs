#![forbid(unsafe_code)]

//! Frame = Buffer + metadata for a render pass.
//!
//! The `Frame` is the render target that `Model::view()` methods write to.
//! It bundles the cell grid ([`Buffer`]) with metadata for cursor and
//! mouse hit testing.
//!
//! # Design Rationale
//!
//! Frame does NOT own pools (GraphemePool, LinkRegistry) - those are passed
//! separately or accessed via RenderContext to allow sharing across frames.
//!
//! # Usage
//!
//! ```
//! use ftui_render::frame::Frame;
//! use ftui_render::cell::Cell;
//!
//! let mut frame = Frame::new(80, 24);
//!
//! // Draw content
//! frame.buffer.set_raw(0, 0, Cell::from_char('H'));
//! frame.buffer.set_raw(1, 0, Cell::from_char('i'));
//!
//! // Set cursor
//! frame.set_cursor(Some((2, 0)));
//! ```

use crate::buffer::Buffer;
use ftui_core::geometry::Rect;

/// Identifier for a clickable region in the hit grid.
///
/// Widgets register hit regions with unique IDs to enable mouse interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct HitId(pub u32);

impl HitId {
    /// Create a new hit ID from a raw value.
    #[inline]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// Get the raw ID value.
    #[inline]
pub const fn id(self) -> u32 {
    self.0
}
}

/// Opaque user data for hit callbacks.
pub type HitData = u64;

/// Regions within a widget for mouse interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HitRegion {
    /// No interactive region.
    #[default]
    None,
    /// Main content area.
    Content,
    /// Widget border area.
    Border,
    /// Scrollbar track or thumb.
    Scrollbar,
    /// Resize handle or drag target.
    Handle,
    /// Clickable button.
    Button,
    /// Hyperlink.
    Link,
    /// Custom region tag.
    Custom(u8),
}

/// A single hit cell in the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HitCell {
    pub widget_id: Option<HitId>,
    pub region: HitRegion,
    pub data: HitData,
}

impl HitCell {
    /// Create a populated hit cell.
    #[inline]
    pub const fn new(widget_id: HitId, region: HitRegion, data: HitData) -> Self {
        Self {
            widget_id: Some(widget_id),
            region,
            data,
        }
    }

    /// Check if the cell is empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.widget_id.is_none()
    }
}

/// Hit testing grid for mouse interaction.
///
/// Maps screen positions to widget IDs, enabling widgets to receive
/// mouse events for their regions.
#[derive(Debug, Clone)]
pub struct HitGrid {
    width: u16,
    height: u16,
    cells: Vec<HitCell>,
}

impl HitGrid {
    /// Create a new hit grid with the given dimensions.
    pub fn new(width: u16, height: u16) -> Self {
        let size = width as usize * height as usize;
        Self {
            width,
            height,
            cells: vec![HitCell::default(); size],
        }
    }

    /// Grid width.
    #[inline]
    pub const fn width(&self) -> u16 {
        self.width
    }

    /// Grid height.
    #[inline]
    pub const fn height(&self) -> u16 {
        self.height
    }

    /// Convert (x, y) to linear index.
    #[inline]
    fn index(&self, x: u16, y: u16) -> Option<usize> {
        if x < self.width && y < self.height {
            Some(y as usize * self.width as usize + x as usize)
        } else {
            None
        }
    }

    /// Get the hit cell at (x, y).
    #[inline]
    pub fn get(&self, x: u16, y: u16) -> Option<&HitCell> {
        self.index(x, y).map(|i| &self.cells[i])
    }

    /// Get mutable reference to hit cell at (x, y).
    #[inline]
    pub fn get_mut(&mut self, x: u16, y: u16) -> Option<&mut HitCell> {
        self.index(x, y).map(|i| &mut self.cells[i])
    }

    /// Register a clickable region with the given hit metadata.
    ///
    /// All cells within the rectangle will map to this hit cell.
    pub fn register(&mut self, rect: Rect, widget_id: HitId, region: HitRegion, data: HitData) {
        // Use usize to avoid overflow for large coordinates
        let x_end = (rect.x as usize + rect.width as usize).min(self.width as usize) as u16;
        let y_end = (rect.y as usize + rect.height as usize).min(self.height as usize) as u16;

        let hit_cell = HitCell::new(widget_id, region, data);
        for y in rect.y..y_end {
            for x in rect.x..x_end {
                if let Some(cell) = self.get_mut(x, y) {
                    *cell = hit_cell;
                }
            }
        }
    }

    /// Hit test at the given position.
    ///
    /// Returns the hit tuple if a region is registered at (x, y).
    pub fn hit_test(&self, x: u16, y: u16) -> Option<(HitId, HitRegion, HitData)> {
        self.get(x, y).and_then(|cell| {
            cell.widget_id
                .map(|id| (id, cell.region, cell.data))
        })
    }

    /// Return all hits within the given rectangle.
    pub fn hits_in(&self, rect: Rect) -> Vec<(HitId, HitRegion, HitData)> {
        let x_end = (rect.x as usize + rect.width as usize).min(self.width as usize) as u16;
        let y_end = (rect.y as usize + rect.height as usize).min(self.height as usize) as u16;
        let mut hits = Vec::new();

        for y in rect.y..y_end {
            for x in rect.x..x_end {
                if let Some((id, region, data)) = self.hit_test(x, y) {
                    hits.push((id, region, data));
                }
            }
        }

        hits
    }

    /// Clear all hit regions.
    pub fn clear(&mut self) {
        self.cells.fill(HitCell::default());
    }
}

/// Frame = Buffer + metadata for a render pass.
///
/// The Frame is passed to `Model::view()` and contains everything needed
/// to render a single frame. The Buffer holds cells; metadata controls
/// cursor and enables mouse hit testing.
#[derive(Debug, Clone)]
pub struct Frame {
    /// The cell grid for this render pass.
    pub buffer: Buffer,

    /// Optional hit grid for mouse hit testing.
    ///
    /// When `Some`, widgets can register clickable regions.
    pub hit_grid: Option<HitGrid>,

    /// Cursor position (if app wants to show cursor).
    ///
    /// Coordinates are relative to buffer (0-indexed).
    pub cursor_position: Option<(u16, u16)>,

    /// Whether cursor should be visible.
    pub cursor_visible: bool,
}

impl Frame {
    /// Create a new frame with given dimensions.
    ///
    /// The frame starts with no hit grid and visible cursor at no position.
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            buffer: Buffer::new(width, height),
            hit_grid: None,
            cursor_position: None,
            cursor_visible: true,
        }
    }

    /// Create a frame with hit testing enabled.
    ///
    /// The hit grid allows widgets to register clickable regions.
    pub fn with_hit_grid(width: u16, height: u16) -> Self {
        Self {
            buffer: Buffer::new(width, height),
            hit_grid: Some(HitGrid::new(width, height)),
            cursor_position: None,
            cursor_visible: true,
        }
    }

    /// Enable hit testing on an existing frame.
    pub fn enable_hit_testing(&mut self) {
        if self.hit_grid.is_none() {
            self.hit_grid = Some(HitGrid::new(self.width(), self.height()));
        }
    }

    /// Frame width in cells.
    #[inline]
    pub fn width(&self) -> u16 {
        self.buffer.width()
    }

    /// Frame height in cells.
    #[inline]
    pub fn height(&self) -> u16 {
        self.buffer.height()
    }

    /// Clear frame for next render.
    ///
    /// Resets both the buffer and hit grid (if present).
    pub fn clear(&mut self) {
        self.buffer.clear();
        if let Some(ref mut grid) = self.hit_grid {
            grid.clear();
        }
    }

    /// Set cursor position.
    ///
    /// Pass `None` to indicate no cursor should be shown at a specific position.
    #[inline]
    pub fn set_cursor(&mut self, position: Option<(u16, u16)>) {
        self.cursor_position = position;
    }

    /// Set cursor visibility.
    #[inline]
    pub fn set_cursor_visible(&mut self, visible: bool) {
        self.cursor_visible = visible;
    }

    /// Get the bounding rectangle of the frame.
    #[inline]
    pub fn bounds(&self) -> Rect {
        self.buffer.bounds()
    }

    /// Register a hit region (if hit grid is enabled).
    ///
    /// Returns `true` if the region was registered, `false` if no hit grid.
    pub fn register_hit(&mut self, rect: Rect, id: HitId, region: HitRegion, data: HitData) -> bool {
        if let Some(ref mut grid) = self.hit_grid {
            grid.register(rect, id, region, data);
            true
        } else {
            false
        }
    }

    /// Hit test at the given position (if hit grid is enabled).
    pub fn hit_test(&self, x: u16, y: u16) -> Option<(HitId, HitRegion, HitData)> {
        self.hit_grid.as_ref().and_then(|grid| grid.hit_test(x, y))
    }

    /// Register a hit region with default metadata (Content, data=0).
    pub fn register_hit_region(&mut self, rect: Rect, id: HitId) -> bool {
        self.register_hit(rect, id, HitRegion::Content, 0)
    }
}

impl Default for Frame {
    /// Create a 1x1 frame (minimum size).
    fn default() -> Self {
        Self::new(1, 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::Cell;

    #[test]
    fn frame_creation() {
        let frame = Frame::new(80, 24);
        assert_eq!(frame.width(), 80);
        assert_eq!(frame.height(), 24);
        assert!(frame.hit_grid.is_none());
        assert!(frame.cursor_position.is_none());
        assert!(frame.cursor_visible);
    }

    #[test]
    fn frame_with_hit_grid() {
        let frame = Frame::with_hit_grid(80, 24);
        assert!(frame.hit_grid.is_some());
        assert_eq!(frame.width(), 80);
        assert_eq!(frame.height(), 24);
    }

    #[test]
    fn frame_cursor() {
        let mut frame = Frame::new(80, 24);
        assert!(frame.cursor_position.is_none());
        assert!(frame.cursor_visible);

        frame.set_cursor(Some((10, 5)));
        assert_eq!(frame.cursor_position, Some((10, 5)));

        frame.set_cursor_visible(false);
        assert!(!frame.cursor_visible);

        frame.set_cursor(None);
        assert!(frame.cursor_position.is_none());
    }

    #[test]
    fn frame_clear() {
        let mut frame = Frame::with_hit_grid(10, 10);

        // Add some content
        frame.buffer.set_raw(5, 5, Cell::from_char('X'));
        frame.register_hit_region(Rect::new(0, 0, 5, 5), HitId::new(1));

        // Verify content exists
        assert_eq!(frame.buffer.get(5, 5).unwrap().content.as_char(), Some('X'));
        assert_eq!(
            frame.hit_test(2, 2),
            Some((HitId::new(1), HitRegion::Content, 0))
        );

        // Clear
        frame.clear();

        // Verify cleared
        assert!(frame.buffer.get(5, 5).unwrap().is_empty());
        assert!(frame.hit_test(2, 2).is_none());
    }

    #[test]
    fn frame_bounds() {
        let frame = Frame::new(80, 24);
        let bounds = frame.bounds();
        assert_eq!(bounds.x, 0);
        assert_eq!(bounds.y, 0);
        assert_eq!(bounds.width, 80);
        assert_eq!(bounds.height, 24);
    }

    #[test]
    fn hit_grid_creation() {
        let grid = HitGrid::new(80, 24);
        assert_eq!(grid.width(), 80);
        assert_eq!(grid.height(), 24);
    }

    #[test]
    fn hit_grid_registration() {
        let mut frame = Frame::with_hit_grid(80, 24);
        let hit_id = HitId::new(42);
        let rect = Rect::new(10, 5, 20, 3);

        frame.register_hit(rect, hit_id, HitRegion::Button, 99);

        // Inside rect
        assert_eq!(
            frame.hit_test(15, 6),
            Some((hit_id, HitRegion::Button, 99))
        );
        assert_eq!(
            frame.hit_test(10, 5),
            Some((hit_id, HitRegion::Button, 99))
        ); // Top-left corner
        assert_eq!(
            frame.hit_test(29, 7),
            Some((hit_id, HitRegion::Button, 99))
        ); // Bottom-right corner

        // Outside rect
        assert!(frame.hit_test(5, 5).is_none()); // Left of rect
        assert!(frame.hit_test(30, 6).is_none()); // Right of rect (exclusive)
        assert!(frame.hit_test(15, 8).is_none()); // Below rect
        assert!(frame.hit_test(15, 4).is_none()); // Above rect
    }

    #[test]
    fn hit_grid_overlapping_regions() {
        let mut frame = Frame::with_hit_grid(20, 20);

        // Register two overlapping regions
        frame.register_hit(Rect::new(0, 0, 10, 10), HitId::new(1), HitRegion::Content, 1);
        frame.register_hit(Rect::new(5, 5, 10, 10), HitId::new(2), HitRegion::Border, 2);

        // Non-overlapping region from first
        assert_eq!(
            frame.hit_test(2, 2),
            Some((HitId::new(1), HitRegion::Content, 1))
        );

        // Overlapping region - second wins (last registered)
        assert_eq!(
            frame.hit_test(7, 7),
            Some((HitId::new(2), HitRegion::Border, 2))
        );

        // Non-overlapping region from second
        assert_eq!(
            frame.hit_test(12, 12),
            Some((HitId::new(2), HitRegion::Border, 2))
        );
    }

    #[test]
    fn hit_grid_out_of_bounds() {
        let frame = Frame::with_hit_grid(10, 10);

        // Out of bounds returns None
        assert!(frame.hit_test(100, 100).is_none());
        assert!(frame.hit_test(10, 0).is_none()); // Exclusive bound
        assert!(frame.hit_test(0, 10).is_none()); // Exclusive bound
    }

    #[test]
    fn hit_id_properties() {
        let id = HitId::new(42);
        assert_eq!(id.id(), 42);
        assert_eq!(id, HitId(42));
    }

    #[test]
    fn register_hit_region_no_grid() {
        let mut frame = Frame::new(10, 10);
        let result = frame.register_hit_region(Rect::new(0, 0, 5, 5), HitId::new(1));
        assert!(!result); // No hit grid, returns false
    }

    #[test]
    fn register_hit_region_with_grid() {
        let mut frame = Frame::with_hit_grid(10, 10);
        let result = frame.register_hit_region(Rect::new(0, 0, 5, 5), HitId::new(1));
        assert!(result); // Has hit grid, returns true
    }

    #[test]
    fn hit_grid_clear() {
        let mut grid = HitGrid::new(10, 10);
        grid.register(Rect::new(0, 0, 5, 5), HitId::new(1), HitRegion::Content, 0);

        assert_eq!(
            grid.hit_test(2, 2),
            Some((HitId::new(1), HitRegion::Content, 0))
        );

        grid.clear();

        assert!(grid.hit_test(2, 2).is_none());
    }

    #[test]
    fn hit_grid_boundary_clipping() {
        let mut grid = HitGrid::new(10, 10);

        // Register region that extends beyond grid
        grid.register(Rect::new(8, 8, 10, 10), HitId::new(1), HitRegion::Content, 0);

        // Inside clipped region
        assert_eq!(
            grid.hit_test(9, 9),
            Some((HitId::new(1), HitRegion::Content, 0))
        );

        // Outside grid
        assert!(grid.hit_test(10, 10).is_none());
    }

    #[test]
    fn hit_grid_hits_in_area() {
        let mut grid = HitGrid::new(5, 5);
        grid.register(Rect::new(0, 0, 2, 2), HitId::new(1), HitRegion::Content, 10);
        grid.register(Rect::new(1, 1, 2, 2), HitId::new(2), HitRegion::Button, 20);

        let hits = grid.hits_in(Rect::new(0, 0, 3, 3));
        assert!(hits.contains(&(HitId::new(1), HitRegion::Content, 10)));
        assert!(hits.contains(&(HitId::new(2), HitRegion::Button, 20)));
    }

    #[test]
    fn frame_default() {
        let frame = Frame::default();
        assert_eq!(frame.width(), 1);
        assert_eq!(frame.height(), 1);
    }
}
