#![forbid(unsafe_code)]

//! Buffer grid storage.
//!
//! The `Buffer` is a 2D grid of [`Cell`]s representing the terminal display.
//! It provides efficient cell access, scissor (clipping) regions, and opacity
//! stacks for compositing.
//!
//! # Layout
//!
//! Cells are stored in row-major order: `index = y * width + x`.
//!
//! # Invariants
//!
//! 1. `cells.len() == width * height`
//! 2. Width and height never change after creation
//! 3. Scissor stack intersection monotonically decreases on push
//! 4. Opacity stack product stays in `[0.0, 1.0]`
//! 5. Scissor/opacity stacks always have at least one element

use crate::cell::Cell;
use ftui_core::geometry::Rect;

/// A 2D grid of terminal cells.
///
/// # Example
///
/// ```
/// use ftui_render::buffer::Buffer;
/// use ftui_render::cell::Cell;
///
/// let mut buffer = Buffer::new(80, 24);
/// buffer.set(0, 0, Cell::from_char('H'));
/// buffer.set(1, 0, Cell::from_char('i'));
/// ```
#[derive(Debug, Clone)]
pub struct Buffer {
    width: u16,
    height: u16,
    cells: Vec<Cell>,
    scissor_stack: Vec<Rect>,
    opacity_stack: Vec<f32>,
}

impl Buffer {
    /// Create a new buffer with the given dimensions.
    ///
    /// All cells are initialized to the default (empty cell with white
    /// foreground and transparent background).
    ///
    /// # Panics
    ///
    /// Panics if width or height is 0.
    pub fn new(width: u16, height: u16) -> Self {
        assert!(width > 0, "buffer width must be > 0");
        assert!(height > 0, "buffer height must be > 0");

        let size = width as usize * height as usize;
        let cells = vec![Cell::default(); size];

        Self {
            width,
            height,
            cells,
            scissor_stack: vec![Rect::from_size(width, height)],
            opacity_stack: vec![1.0],
        }
    }

    /// Buffer width in cells.
    #[inline]
    pub const fn width(&self) -> u16 {
        self.width
    }

    /// Buffer height in cells.
    #[inline]
    pub const fn height(&self) -> u16 {
        self.height
    }

    /// Total number of cells.
    #[inline]
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// Check if the buffer is empty (should never be true for valid buffers).
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Bounding rect of the entire buffer.
    #[inline]
    pub const fn bounds(&self) -> Rect {
        Rect::from_size(self.width, self.height)
    }

    /// Convert (x, y) coordinates to a linear index.
    ///
    /// Returns `None` if coordinates are out of bounds.
    #[inline]
    fn index(&self, x: u16, y: u16) -> Option<usize> {
        if x < self.width && y < self.height {
            Some(y as usize * self.width as usize + x as usize)
        } else {
            None
        }
    }

    /// Convert (x, y) coordinates to a linear index without bounds checking.
    ///
    /// # Safety
    ///
    /// Caller must ensure x < width and y < height.
    #[inline]
    fn index_unchecked(&self, x: u16, y: u16) -> usize {
        debug_assert!(x < self.width && y < self.height);
        y as usize * self.width as usize + x as usize
    }

    /// Get a reference to the cell at (x, y).
    ///
    /// Returns `None` if coordinates are out of bounds.
    #[inline]
    pub fn get(&self, x: u16, y: u16) -> Option<&Cell> {
        self.index(x, y).map(|i| &self.cells[i])
    }

    /// Get a mutable reference to the cell at (x, y).
    ///
    /// Returns `None` if coordinates are out of bounds.
    #[inline]
    pub fn get_mut(&mut self, x: u16, y: u16) -> Option<&mut Cell> {
        self.index(x, y).map(|i| &mut self.cells[i])
    }

    /// Get a reference to the cell at (x, y) without bounds checking.
    ///
    /// # Panics
    ///
    /// Panics in debug mode if coordinates are out of bounds.
    /// May cause undefined behavior in release mode if out of bounds.
    #[inline]
    pub fn get_unchecked(&self, x: u16, y: u16) -> &Cell {
        let i = self.index_unchecked(x, y);
        &self.cells[i]
    }

    /// Set the cell at (x, y).
    ///
    /// This method:
    /// - Respects the current scissor region (skips if outside)
    /// - Applies the current opacity stack to cell colors
    /// - Does nothing if coordinates are out of bounds
    /// - **Automatically sets CONTINUATION cells** for multi-width content
    ///
    /// For bulk operations without scissor/opacity/safety, use [`set_raw`].
    #[inline]
    pub fn set(&mut self, x: u16, y: u16, cell: Cell) {
        // Check bounds
        let Some(idx) = self.index(x, y) else {
            return;
        };

        // Check scissor region
        if !self.current_scissor().contains(x, y) {
            return;
        }

        // Apply opacity
        let final_cell = if self.current_opacity() < 1.0 {
            let opacity = self.current_opacity();
            Cell {
                fg: cell.fg.with_opacity(opacity),
                bg: cell.bg.with_opacity(opacity),
                ..cell
            }
        } else {
            cell
        };

        self.cells[idx] = final_cell;

        // Handle multi-width characters
        // We use the accurate width calculation (not width_hint) because wide CJK characters
        // need proper continuation cells. width_hint() returns 1 for direct chars as a fast path.
        let width = cell.content.width();
        if width > 1 {
            for i in 1..width {
                // Recursively set continuation cells.
                // This ensures they are also bounds-checked and scissor-clipped.
                // Cell::CONTINUATION has width 0, so this won't recurse infinitely.
                self.set(x + i as u16, y, Cell::CONTINUATION);
            }
        }
    }

    /// Set the cell at (x, y) without scissor or opacity processing.
    ///
    /// This is faster but bypasses clipping and transparency.
    /// Does nothing if coordinates are out of bounds.
    #[inline]
    pub fn set_raw(&mut self, x: u16, y: u16, cell: Cell) {
        if let Some(idx) = self.index(x, y) {
            self.cells[idx] = cell;
        }
    }

    /// Fill a rectangular region with the given cell.
    ///
    /// Respects scissor region and applies opacity.
    pub fn fill(&mut self, rect: Rect, cell: Cell) {
        let clipped = self.current_scissor().intersection(&rect);
        if clipped.is_empty() {
            return;
        }

        for y in clipped.y..clipped.bottom() {
            for x in clipped.x..clipped.right() {
                self.set(x, y, cell);
            }
        }
    }

    /// Clear all cells to the default.
    pub fn clear(&mut self) {
        self.cells.fill(Cell::default());
    }

    /// Clear all cells to the given cell.
    pub fn clear_with(&mut self, cell: Cell) {
        self.cells.fill(cell);
    }

    /// Get raw access to the cell slice.
    ///
    /// This is useful for diffing against another buffer.
    #[inline]
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// Get mutable raw access to the cell slice.
    #[inline]
    pub fn cells_mut(&mut self) -> &mut [Cell] {
        &mut self.cells
    }

    // ========== Scissor Stack ==========

    /// Push a scissor (clipping) region onto the stack.
    ///
    /// The effective scissor is the intersection of all pushed rects.
    /// If the intersection is empty, no cells will be drawn.
    pub fn push_scissor(&mut self, rect: Rect) {
        let current = self.current_scissor();
        let intersected = current.intersection(&rect);
        self.scissor_stack.push(intersected);
    }

    /// Pop a scissor region from the stack.
    ///
    /// Does nothing if only the base scissor remains.
    pub fn pop_scissor(&mut self) {
        if self.scissor_stack.len() > 1 {
            self.scissor_stack.pop();
        }
    }

    /// Get the current effective scissor region.
    #[inline]
    pub fn current_scissor(&self) -> Rect {
        // Safe: stack always has at least one element
        *self.scissor_stack.last().unwrap()
    }

    /// Get the scissor stack depth.
    #[inline]
    pub fn scissor_depth(&self) -> usize {
        self.scissor_stack.len()
    }

    // ========== Opacity Stack ==========

    /// Push an opacity multiplier onto the stack.
    ///
    /// The effective opacity is the product of all pushed values.
    /// Values are clamped to `[0.0, 1.0]`.
    pub fn push_opacity(&mut self, opacity: f32) {
        let clamped = opacity.clamp(0.0, 1.0);
        let current = self.current_opacity();
        self.opacity_stack.push(current * clamped);
    }

    /// Pop an opacity value from the stack.
    ///
    /// Does nothing if only the base opacity remains.
    pub fn pop_opacity(&mut self) {
        if self.opacity_stack.len() > 1 {
            self.opacity_stack.pop();
        }
    }

    /// Get the current effective opacity.
    #[inline]
    pub fn current_opacity(&self) -> f32 {
        // Safe: stack always has at least one element
        *self.opacity_stack.last().unwrap()
    }

    /// Get the opacity stack depth.
    #[inline]
    pub fn opacity_depth(&self) -> usize {
        self.opacity_stack.len()
    }

    // ========== Copying and Diffing ==========

    /// Copy a rectangular region from another buffer.
    ///
    /// Copies cells from `src` at `src_rect` to this buffer at `dst_pos`.
    /// Respects scissor region.
    pub fn copy_from(&mut self, src: &Buffer, src_rect: Rect, dst_x: u16, dst_y: u16) {
        for dy in 0..src_rect.height {
            for dx in 0..src_rect.width {
                let sx = src_rect.x + dx;
                let sy = src_rect.y + dy;
                if let Some(cell) = src.get(sx, sy) {
                    self.set(dst_x + dx, dst_y + dy, *cell);
                }
            }
        }
    }

    /// Check if two buffers have identical content.
    pub fn content_eq(&self, other: &Buffer) -> bool {
        self.width == other.width && self.height == other.height && self.cells == other.cells
    }
}

impl Default for Buffer {
    /// Create a 1x1 buffer (minimum size).
    fn default() -> Self {
        Self::new(1, 1)
    }
}

impl PartialEq for Buffer {
    fn eq(&self, other: &Self) -> bool {
        self.content_eq(other)
    }
}

impl Eq for Buffer {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::PackedRgba;

    #[test]
    fn rect_contains() {
        let r = Rect::new(5, 5, 10, 10);
        assert!(r.contains(5, 5)); // Top-left corner
        assert!(r.contains(14, 14)); // Bottom-right inside
        assert!(!r.contains(4, 5)); // Left of rect
        assert!(!r.contains(15, 5)); // Right of rect (exclusive)
        assert!(!r.contains(5, 15)); // Below rect (exclusive)
    }

    #[test]
    fn rect_intersection() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        let i = a.intersection(&b);
        assert_eq!(i, Rect::new(5, 5, 5, 5));

        // Non-overlapping
        let c = Rect::new(20, 20, 5, 5);
        assert_eq!(a.intersection(&c), Rect::default());
    }

    #[test]
    fn buffer_creation() {
        let buf = Buffer::new(80, 24);
        assert_eq!(buf.width(), 80);
        assert_eq!(buf.height(), 24);
        assert_eq!(buf.len(), 80 * 24);
    }

    #[test]
    #[should_panic(expected = "width must be > 0")]
    fn buffer_zero_width_panics() {
        Buffer::new(0, 24);
    }

    #[test]
    #[should_panic(expected = "height must be > 0")]
    fn buffer_zero_height_panics() {
        Buffer::new(80, 0);
    }

    #[test]
    fn buffer_get_and_set() {
        let mut buf = Buffer::new(10, 10);
        let cell = Cell::from_char('X');
        buf.set(5, 5, cell);
        assert_eq!(buf.get(5, 5).unwrap().content.as_char(), Some('X'));
    }

    #[test]
    fn buffer_out_of_bounds_get() {
        let buf = Buffer::new(10, 10);
        assert!(buf.get(10, 0).is_none());
        assert!(buf.get(0, 10).is_none());
        assert!(buf.get(100, 100).is_none());
    }

    #[test]
    fn buffer_out_of_bounds_set_ignored() {
        let mut buf = Buffer::new(10, 10);
        buf.set(100, 100, Cell::from_char('X')); // Should not panic
        assert_eq!(buf.cells().iter().filter(|c| !c.is_empty()).count(), 0);
    }

    #[test]
    fn buffer_clear() {
        let mut buf = Buffer::new(10, 10);
        buf.set(5, 5, Cell::from_char('X'));
        buf.clear();
        assert!(buf.get(5, 5).unwrap().is_empty());
    }

    #[test]
    fn scissor_stack_basic() {
        let mut buf = Buffer::new(20, 20);

        // Default scissor covers entire buffer
        assert_eq!(buf.current_scissor(), Rect::from_size(20, 20));
        assert_eq!(buf.scissor_depth(), 1);

        // Push smaller scissor
        buf.push_scissor(Rect::new(5, 5, 10, 10));
        assert_eq!(buf.current_scissor(), Rect::new(5, 5, 10, 10));
        assert_eq!(buf.scissor_depth(), 2);

        // Set inside scissor works
        buf.set(7, 7, Cell::from_char('I'));
        assert_eq!(buf.get(7, 7).unwrap().content.as_char(), Some('I'));

        // Set outside scissor is ignored
        buf.set(0, 0, Cell::from_char('O'));
        assert!(buf.get(0, 0).unwrap().is_empty());

        // Pop scissor
        buf.pop_scissor();
        assert_eq!(buf.current_scissor(), Rect::from_size(20, 20));
        assert_eq!(buf.scissor_depth(), 1);

        // Now can set at (0, 0)
        buf.set(0, 0, Cell::from_char('N'));
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('N'));
    }

    #[test]
    fn scissor_intersection() {
        let mut buf = Buffer::new(20, 20);
        buf.push_scissor(Rect::new(5, 5, 10, 10));
        buf.push_scissor(Rect::new(8, 8, 10, 10));

        // Intersection: (8,8) to (15,15) intersected with (5,5) to (15,15)
        // Result: (8,8) to (15,15) -> width=7, height=7
        assert_eq!(buf.current_scissor(), Rect::new(8, 8, 7, 7));
    }

    #[test]
    fn scissor_base_cannot_be_popped() {
        let mut buf = Buffer::new(10, 10);
        buf.pop_scissor(); // Should be a no-op
        assert_eq!(buf.scissor_depth(), 1);
        buf.pop_scissor(); // Still no-op
        assert_eq!(buf.scissor_depth(), 1);
    }

    #[test]
    fn opacity_stack_basic() {
        let mut buf = Buffer::new(10, 10);

        // Default opacity is 1.0
        assert!((buf.current_opacity() - 1.0).abs() < f32::EPSILON);
        assert_eq!(buf.opacity_depth(), 1);

        // Push 0.5 opacity
        buf.push_opacity(0.5);
        assert!((buf.current_opacity() - 0.5).abs() < f32::EPSILON);
        assert_eq!(buf.opacity_depth(), 2);

        // Push another 0.5 -> effective 0.25
        buf.push_opacity(0.5);
        assert!((buf.current_opacity() - 0.25).abs() < f32::EPSILON);
        assert_eq!(buf.opacity_depth(), 3);

        // Pop back to 0.5
        buf.pop_opacity();
        assert!((buf.current_opacity() - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn opacity_applied_to_cells() {
        let mut buf = Buffer::new(10, 10);
        buf.push_opacity(0.5);

        let cell = Cell::from_char('X').with_fg(PackedRgba::rgba(100, 100, 100, 255));
        buf.set(5, 5, cell);

        let stored = buf.get(5, 5).unwrap();
        // Alpha should be reduced by 0.5
        assert_eq!(stored.fg.a(), 128);
    }

    #[test]
    fn opacity_clamped() {
        let mut buf = Buffer::new(10, 10);
        buf.push_opacity(2.0); // Should clamp to 1.0
        assert!((buf.current_opacity() - 1.0).abs() < f32::EPSILON);

        buf.push_opacity(-1.0); // Should clamp to 0.0
        assert!((buf.current_opacity() - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn opacity_base_cannot_be_popped() {
        let mut buf = Buffer::new(10, 10);
        buf.pop_opacity(); // No-op
        assert_eq!(buf.opacity_depth(), 1);
    }

    #[test]
    fn buffer_fill() {
        let mut buf = Buffer::new(10, 10);
        let cell = Cell::from_char('#');
        buf.fill(Rect::new(2, 2, 5, 5), cell);

        // Inside fill region
        assert_eq!(buf.get(3, 3).unwrap().content.as_char(), Some('#'));

        // Outside fill region
        assert!(buf.get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn buffer_fill_respects_scissor() {
        let mut buf = Buffer::new(10, 10);
        buf.push_scissor(Rect::new(3, 3, 4, 4));

        let cell = Cell::from_char('#');
        buf.fill(Rect::new(0, 0, 10, 10), cell);

        // Only scissor region should be filled
        assert_eq!(buf.get(3, 3).unwrap().content.as_char(), Some('#'));
        assert!(buf.get(0, 0).unwrap().is_empty());
        assert!(buf.get(7, 7).unwrap().is_empty());
    }

    #[test]
    fn buffer_copy_from() {
        let mut src = Buffer::new(10, 10);
        src.set(2, 2, Cell::from_char('S'));

        let mut dst = Buffer::new(10, 10);
        dst.copy_from(&src, Rect::new(0, 0, 5, 5), 3, 3);

        // Cell at (2,2) in src should be at (5,5) in dst (offset by 3,3)
        assert_eq!(dst.get(5, 5).unwrap().content.as_char(), Some('S'));
    }

    #[test]
    fn buffer_content_eq() {
        let mut buf1 = Buffer::new(10, 10);
        let mut buf2 = Buffer::new(10, 10);

        assert!(buf1.content_eq(&buf2));

        buf1.set(0, 0, Cell::from_char('X'));
        assert!(!buf1.content_eq(&buf2));

        buf2.set(0, 0, Cell::from_char('X'));
        assert!(buf1.content_eq(&buf2));
    }

    #[test]
    fn buffer_bounds() {
        let buf = Buffer::new(80, 24);
        let bounds = buf.bounds();
        assert_eq!(bounds.x, 0);
        assert_eq!(bounds.y, 0);
        assert_eq!(bounds.width, 80);
        assert_eq!(bounds.height, 24);
    }

    #[test]
    fn buffer_set_raw_bypasses_scissor() {
        let mut buf = Buffer::new(10, 10);
        buf.push_scissor(Rect::new(5, 5, 5, 5));

        // set() respects scissor - this should be ignored
        buf.set(0, 0, Cell::from_char('S'));
        assert!(buf.get(0, 0).unwrap().is_empty());

        // set_raw() bypasses scissor - this should work
        buf.set_raw(0, 0, Cell::from_char('R'));
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('R'));
    }

    #[test]
    fn set_handles_wide_chars() {
        let mut buf = Buffer::new(10, 10);
        
        // Set a wide character (width 2)
        buf.set(0, 0, Cell::from_char('中'));
        
        // Check head
        let head = buf.get(0, 0).unwrap();
        assert_eq!(head.content.as_char(), Some('中'));
        
        // Check continuation
        let cont = buf.get(1, 0).unwrap();
        assert!(cont.is_continuation());
        assert!(!cont.is_empty());
    }

    #[test]
    fn set_handles_wide_chars_clipped() {
        let mut buf = Buffer::new(10, 10);
        buf.push_scissor(Rect::new(0, 0, 1, 10)); // Only column 0 is visible
        
        // Set wide char at 0,0. Head is visible, tail is clipped.
        buf.set(0, 0, Cell::from_char('中'));
        
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('中'));
        // Continuation at (1,0) should be blocked by scissor
        assert!(buf.get(1, 0).unwrap().is_empty());
    }
}
