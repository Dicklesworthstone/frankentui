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

use crate::budget::DegradationLevel;
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
    /// Current degradation level for this frame.
    ///
    /// Widgets read this during rendering to decide how much visual fidelity
    /// to provide. Set by the runtime before calling `Model::view()`.
    pub degradation: DegradationLevel,
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
            degradation: DegradationLevel::Full,
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

    /// Helper to clean up overlapping multi-width cells before writing.
    fn cleanup_overlap(&mut self, x: u16, y: u16, new_cell: &Cell) {
        let Some(idx) = self.index(x, y) else { return };
        let current = self.cells[idx];

        // Case 1: Overwriting a Wide Head
        if current.content.width() > 1 {
            let width = current.content.width();
            // Clear the head
            // self.cells[idx] = Cell::default(); // Caller (set) will overwrite this, but for correctness/safety we could.
            // Actually, `set` overwrites `cells[idx]` immediately after.
            // But we must clear the tails.
            for i in 1..width {
                if let Some(tail_idx) = self.index(x + i as u16, y)
                    && self.cells[tail_idx].is_continuation()
                {
                    self.cells[tail_idx] = Cell::default();
                }
            }
        }
        // Case 2: Overwriting a Continuation
        else if current.is_continuation() && !new_cell.is_continuation() {
            let mut back_x = x;
            while back_x > 0 {
                back_x -= 1;
                if let Some(h_idx) = self.index(back_x, y) {
                    let h_cell = self.cells[h_idx];
                    if !h_cell.is_continuation() {
                        // Found the potential head
                        let width = h_cell.content.width();
                        if (back_x as usize + width) > x as usize {
                            // This head owns the cell we are overwriting.
                            // Clear the head.
                            self.cells[h_idx] = Cell::default();

                            // Clear all its tails (except the one we're about to write, effectively)
                            // We just iterate 1..width and clear CONTs.
                            for i in 1..width {
                                if let Some(tail_idx) = self.index(back_x + i as u16, y) {
                                    // Note: tail_idx might be our current `idx`.
                                    // We can clear it; `set` will overwrite it in a moment.
                                    if self.cells[tail_idx].is_continuation() {
                                        self.cells[tail_idx] = Cell::default();
                                    }
                                }
                            }
                        }
                        break;
                    }
                }
            }
        }
    }

    /// Set the cell at (x, y).
    ///
    /// This method:
    /// - Respects the current scissor region (skips if outside)
    /// - Applies the current opacity stack to cell colors
    /// - Does nothing if coordinates are out of bounds
    /// - **Automatically sets CONTINUATION cells** for multi-width content
    /// - **Atomic wide writes**: If a wide character doesn't fully fit in the
    ///   scissor region/bounds, NOTHING is written.
    ///
    /// For bulk operations without scissor/opacity/safety, use [`set_raw`].
    #[inline]
    pub fn set(&mut self, x: u16, y: u16, cell: Cell) {
        let width = cell.content.width();

        // Single cell fast path (width 0 or 1)
        if width <= 1 {
            // Check bounds
            let Some(idx) = self.index(x, y) else {
                return;
            };

            // Check scissor region
            if !self.current_scissor().contains(x, y) {
                return;
            }

            // Cleanup overlaps
            self.cleanup_overlap(x, y, &cell);

            // Composite background: new cell's bg over existing cell's bg
            let existing_bg = self.cells[idx].bg;
            let composited_bg = cell.bg.over(existing_bg);

            // Apply opacity
            let final_cell = if self.current_opacity() < 1.0 {
                let opacity = self.current_opacity();
                Cell {
                    fg: cell.fg.with_opacity(opacity),
                    bg: composited_bg.with_opacity(opacity),
                    ..cell
                }
            } else {
                Cell {
                    bg: composited_bg,
                    ..cell
                }
            };

            self.cells[idx] = final_cell;
            return;
        }

        // Multi-width character atomicity check
        // Ensure ALL cells (head + tail) are within bounds and scissor
        let scissor = self.current_scissor();
        for i in 0..width {
            let cx = x + i as u16;
            // Check bounds
            if cx >= self.width || y >= self.height {
                return;
            }
            // Check scissor
            if !scissor.contains(cx, y) {
                return;
            }
        }

        // If we get here, it's safe to write everything.

        // Cleanup overlaps for all cells
        self.cleanup_overlap(x, y, &cell);
        for i in 1..width {
            self.cleanup_overlap(x + i as u16, y, &Cell::CONTINUATION);
        }

        // 1. Write Head
        let idx = self.index_unchecked(x, y);
        let old_cell = self.cells[idx];
        let mut final_cell = if self.current_opacity() < 1.0 {
            let opacity = self.current_opacity();
            Cell {
                fg: cell.fg.with_opacity(opacity),
                bg: cell.bg.with_opacity(opacity),
                ..cell
            }
        } else {
            cell
        };

        // Composite background (src over dst)
        final_cell.bg = final_cell.bg.over(old_cell.bg);

        self.cells[idx] = final_cell;

        // 2. Write Tail (Continuation cells)
        // We can use set_raw-like access because we already verified bounds
        for i in 1..width {
            let idx = self.index_unchecked(x + i as u16, y);
            self.cells[idx] = Cell::CONTINUATION;
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

    /// Get the cells for a single row as a slice.
    ///
    /// # Panics
    ///
    /// Panics if `y >= height`.
    #[inline]
    pub fn row_cells(&self, y: u16) -> &[Cell] {
        let start = y as usize * self.width as usize;
        &self.cells[start..start + self.width as usize]
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
    fn set_composites_background() {
        let mut buf = Buffer::new(1, 1);

        // Set background to RED
        let red = PackedRgba::rgb(255, 0, 0);
        buf.set(0, 0, Cell::default().with_bg(red));

        // Write 'X' with transparent background
        let cell = Cell::from_char('X'); // Default bg is TRANSPARENT
        buf.set(0, 0, cell);

        let result = buf.get(0, 0).unwrap();
        assert_eq!(result.content.as_char(), Some('X'));
        assert_eq!(
            result.bg, red,
            "Background should be preserved (composited)"
        );
    }

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

        // Set wide char at 0,0. Tail at x=1 is outside scissor.
        // Atomic rejection: entire write is rejected because tail doesn't fit.
        buf.set(0, 0, Cell::from_char('中'));

        // Head should NOT be written (atomic rejection)
        assert!(buf.get(0, 0).unwrap().is_empty());
        // Tail position should also be unmodified
        assert!(buf.get(1, 0).unwrap().is_empty());
    }

    // --- get_mut ---

    #[test]
    fn get_mut_modifies_cell() {
        let mut buf = Buffer::new(10, 10);
        buf.set(3, 3, Cell::from_char('A'));

        if let Some(cell) = buf.get_mut(3, 3) {
            *cell = Cell::from_char('B');
        }

        assert_eq!(buf.get(3, 3).unwrap().content.as_char(), Some('B'));
    }

    #[test]
    fn get_mut_out_of_bounds() {
        let mut buf = Buffer::new(5, 5);
        assert!(buf.get_mut(10, 10).is_none());
    }

    // --- clear_with ---

    #[test]
    fn clear_with_fills_all_cells() {
        let mut buf = Buffer::new(5, 3);
        let fill_cell = Cell::from_char('*');
        buf.clear_with(fill_cell);

        for y in 0..3 {
            for x in 0..5 {
                assert_eq!(buf.get(x, y).unwrap().content.as_char(), Some('*'));
            }
        }
    }

    // --- cells / cells_mut ---

    #[test]
    fn cells_slice_has_correct_length() {
        let buf = Buffer::new(10, 5);
        assert_eq!(buf.cells().len(), 50);
    }

    #[test]
    fn cells_mut_allows_direct_modification() {
        let mut buf = Buffer::new(3, 2);
        let cells = buf.cells_mut();
        cells[0] = Cell::from_char('Z');

        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('Z'));
    }

    // --- row_cells ---

    #[test]
    fn row_cells_returns_correct_row() {
        let mut buf = Buffer::new(5, 3);
        buf.set(2, 1, Cell::from_char('R'));

        let row = buf.row_cells(1);
        assert_eq!(row.len(), 5);
        assert_eq!(row[2].content.as_char(), Some('R'));
    }

    #[test]
    #[should_panic]
    fn row_cells_out_of_bounds_panics() {
        let buf = Buffer::new(5, 3);
        let _ = buf.row_cells(5);
    }

    // --- is_empty ---

    #[test]
    fn buffer_is_not_empty() {
        let buf = Buffer::new(1, 1);
        assert!(!buf.is_empty());
    }

    // --- set_raw out of bounds ---

    #[test]
    fn set_raw_out_of_bounds_is_safe() {
        let mut buf = Buffer::new(5, 5);
        buf.set_raw(100, 100, Cell::from_char('X'));
        // Should not panic, just be ignored
    }

    // --- copy_from with offset ---

    #[test]
    fn copy_from_out_of_bounds_partial() {
        let mut src = Buffer::new(5, 5);
        src.set(0, 0, Cell::from_char('A'));
        src.set(4, 4, Cell::from_char('B'));

        let mut dst = Buffer::new(5, 5);
        // Copy entire src with offset that puts part out of bounds
        dst.copy_from(&src, Rect::new(0, 0, 5, 5), 3, 3);

        // (0,0) in src → (3,3) in dst = inside
        assert_eq!(dst.get(3, 3).unwrap().content.as_char(), Some('A'));
        // (4,4) in src → (7,7) in dst = outside, should be ignored
        assert!(dst.get(4, 4).unwrap().is_empty());
    }

    // --- content_eq with different dimensions ---

    #[test]
    fn content_eq_different_dimensions() {
        let buf1 = Buffer::new(5, 5);
        let buf2 = Buffer::new(10, 10);
        // Different dimensions should not be equal (different cell counts)
        assert!(!buf1.content_eq(&buf2));
    }

    // ====== Property tests (proptest) ======

    mod property {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn buffer_dimensions_are_preserved(width in 1u16..200, height in 1u16..200) {
                let buf = Buffer::new(width, height);
                prop_assert_eq!(buf.width(), width);
                prop_assert_eq!(buf.height(), height);
                prop_assert_eq!(buf.len(), width as usize * height as usize);
            }

            #[test]
            fn buffer_get_in_bounds_always_succeeds(width in 1u16..100, height in 1u16..100) {
                let buf = Buffer::new(width, height);
                for x in 0..width {
                    for y in 0..height {
                        prop_assert!(buf.get(x, y).is_some(), "get({x},{y}) failed for {width}x{height} buffer");
                    }
                }
            }

            #[test]
            fn buffer_get_out_of_bounds_returns_none(width in 1u16..50, height in 1u16..50) {
                let buf = Buffer::new(width, height);
                prop_assert!(buf.get(width, 0).is_none());
                prop_assert!(buf.get(0, height).is_none());
                prop_assert!(buf.get(width, height).is_none());
            }

            #[test]
            fn buffer_set_get_roundtrip(
                width in 5u16..50,
                height in 5u16..50,
                x in 0u16..5,
                y in 0u16..5,
                ch_idx in 0u32..26,
            ) {
                let x = x % width;
                let y = y % height;
                let ch = char::from_u32('A' as u32 + ch_idx).unwrap();
                let mut buf = Buffer::new(width, height);
                buf.set(x, y, Cell::from_char(ch));
                let got = buf.get(x, y).unwrap();
                prop_assert_eq!(got.content.as_char(), Some(ch));
            }

            #[test]
            fn scissor_push_pop_stack_depth(
                width in 10u16..50,
                height in 10u16..50,
                push_count in 1usize..10,
            ) {
                let mut buf = Buffer::new(width, height);
                prop_assert_eq!(buf.scissor_depth(), 1); // base

                for i in 0..push_count {
                    buf.push_scissor(Rect::new(0, 0, width, height));
                    prop_assert_eq!(buf.scissor_depth(), i + 2);
                }

                for i in (0..push_count).rev() {
                    buf.pop_scissor();
                    prop_assert_eq!(buf.scissor_depth(), i + 1);
                }

                // Base cannot be popped
                buf.pop_scissor();
                prop_assert_eq!(buf.scissor_depth(), 1);
            }

            #[test]
            fn scissor_monotonic_intersection(
                width in 20u16..60,
                height in 20u16..60,
            ) {
                // Scissor stack always shrinks or stays the same
                let mut buf = Buffer::new(width, height);
                let outer = Rect::new(2, 2, width - 4, height - 4);
                buf.push_scissor(outer);
                let s1 = buf.current_scissor();

                let inner = Rect::new(5, 5, 10, 10);
                buf.push_scissor(inner);
                let s2 = buf.current_scissor();

                // Inner scissor must be contained within or equal to outer
                prop_assert!(s2.width <= s1.width, "inner width {} > outer width {}", s2.width, s1.width);
                prop_assert!(s2.height <= s1.height, "inner height {} > outer height {}", s2.height, s1.height);
            }

            #[test]
            fn opacity_push_pop_stack_depth(
                width in 5u16..20,
                height in 5u16..20,
                push_count in 1usize..10,
            ) {
                let mut buf = Buffer::new(width, height);
                prop_assert_eq!(buf.opacity_depth(), 1);

                for i in 0..push_count {
                    buf.push_opacity(0.9);
                    prop_assert_eq!(buf.opacity_depth(), i + 2);
                }

                for i in (0..push_count).rev() {
                    buf.pop_opacity();
                    prop_assert_eq!(buf.opacity_depth(), i + 1);
                }

                buf.pop_opacity();
                prop_assert_eq!(buf.opacity_depth(), 1);
            }

            #[test]
            fn opacity_multiplication_is_monotonic(
                opacity1 in 0.0f32..=1.0,
                opacity2 in 0.0f32..=1.0,
            ) {
                let mut buf = Buffer::new(5, 5);
                buf.push_opacity(opacity1);
                let after_first = buf.current_opacity();
                buf.push_opacity(opacity2);
                let after_second = buf.current_opacity();

                // Effective opacity can only decrease (or stay same at 0 or 1)
                prop_assert!(after_second <= after_first + f32::EPSILON,
                    "opacity increased: {} -> {}", after_first, after_second);
            }

            #[test]
            fn clear_resets_all_cells(width in 1u16..30, height in 1u16..30) {
                let mut buf = Buffer::new(width, height);
                // Write some data
                for x in 0..width {
                    buf.set_raw(x, 0, Cell::from_char('X'));
                }
                buf.clear();
                // All cells should be default (empty)
                for y in 0..height {
                    for x in 0..width {
                        prop_assert!(buf.get(x, y).unwrap().is_empty(),
                            "cell ({x},{y}) not empty after clear");
                    }
                }
            }

            #[test]
            fn content_eq_is_reflexive(width in 1u16..30, height in 1u16..30) {
                let buf = Buffer::new(width, height);
                prop_assert!(buf.content_eq(&buf));
            }

            #[test]
            fn content_eq_detects_single_change(
                width in 5u16..30,
                height in 5u16..30,
                x in 0u16..5,
                y in 0u16..5,
            ) {
                let x = x % width;
                let y = y % height;
                let buf1 = Buffer::new(width, height);
                let mut buf2 = Buffer::new(width, height);
                buf2.set_raw(x, y, Cell::from_char('Z'));
                prop_assert!(!buf1.content_eq(&buf2));
            }

            // --- Executable Invariant Tests (bd-10i.13.2) ---

            #[test]
            fn dimensions_immutable_through_operations(
                width in 5u16..30,
                height in 5u16..30,
            ) {
                let mut buf = Buffer::new(width, height);

                // Operations that must not change dimensions
                buf.set(0, 0, Cell::from_char('A'));
                prop_assert_eq!(buf.width(), width);
                prop_assert_eq!(buf.height(), height);
                prop_assert_eq!(buf.len(), width as usize * height as usize);

                buf.push_scissor(Rect::new(1, 1, 3, 3));
                prop_assert_eq!(buf.width(), width);
                prop_assert_eq!(buf.height(), height);

                buf.push_opacity(0.5);
                prop_assert_eq!(buf.width(), width);
                prop_assert_eq!(buf.height(), height);

                buf.pop_scissor();
                buf.pop_opacity();
                prop_assert_eq!(buf.width(), width);
                prop_assert_eq!(buf.height(), height);

                buf.clear();
                prop_assert_eq!(buf.width(), width);
                prop_assert_eq!(buf.height(), height);
                prop_assert_eq!(buf.len(), width as usize * height as usize);
            }

            #[test]
            fn scissor_area_never_increases_random_rects(
                width in 20u16..60,
                height in 20u16..60,
                rects in proptest::collection::vec(
                    (0u16..20, 0u16..20, 1u16..15, 1u16..15),
                    1..8
                ),
            ) {
                let mut buf = Buffer::new(width, height);
                let mut prev_area = (width as u32) * (height as u32);

                for (x, y, w, h) in rects {
                    buf.push_scissor(Rect::new(x, y, w, h));
                    let s = buf.current_scissor();
                    let area = (s.width as u32) * (s.height as u32);
                    prop_assert!(area <= prev_area,
                        "scissor area increased: {} -> {} after push({},{},{},{})",
                        prev_area, area, x, y, w, h);
                    prev_area = area;
                }
            }

            #[test]
            fn opacity_range_invariant_random_sequence(
                opacities in proptest::collection::vec(0.0f32..=1.0, 1..15),
            ) {
                let mut buf = Buffer::new(5, 5);

                for &op in &opacities {
                    buf.push_opacity(op);
                    let current = buf.current_opacity();
                    prop_assert!(current >= 0.0, "opacity below 0: {}", current);
                    prop_assert!(current <= 1.0 + f32::EPSILON,
                        "opacity above 1: {}", current);
                }

                // Pop everything and verify we get back to 1.0
                for _ in &opacities {
                    buf.pop_opacity();
                }
                // After popping all pushed, should be back to base (1.0)
                prop_assert!((buf.current_opacity() - 1.0).abs() < f32::EPSILON);
            }

            #[test]
            fn opacity_clamp_out_of_range(
                neg in -100.0f32..0.0,
                over in 1.01f32..100.0,
            ) {
                let mut buf = Buffer::new(5, 5);

                buf.push_opacity(neg);
                prop_assert!(buf.current_opacity() >= 0.0,
                    "negative opacity not clamped: {}", buf.current_opacity());
                buf.pop_opacity();

                buf.push_opacity(over);
                prop_assert!(buf.current_opacity() <= 1.0 + f32::EPSILON,
                    "over-1 opacity not clamped: {}", buf.current_opacity());
            }

            #[test]
            fn scissor_stack_always_has_base(
                pushes in 0usize..10,
                pops in 0usize..15,
            ) {
                let mut buf = Buffer::new(10, 10);

                for _ in 0..pushes {
                    buf.push_scissor(Rect::new(0, 0, 5, 5));
                }
                for _ in 0..pops {
                    buf.pop_scissor();
                }

                // Invariant: depth is always >= 1
                prop_assert!(buf.scissor_depth() >= 1,
                    "scissor depth dropped below 1 after {} pushes, {} pops",
                    pushes, pops);
            }

            #[test]
            fn opacity_stack_always_has_base(
                pushes in 0usize..10,
                pops in 0usize..15,
            ) {
                let mut buf = Buffer::new(10, 10);

                for _ in 0..pushes {
                    buf.push_opacity(0.5);
                }
                for _ in 0..pops {
                    buf.pop_opacity();
                }

                // Invariant: depth is always >= 1
                prop_assert!(buf.opacity_depth() >= 1,
                    "opacity depth dropped below 1 after {} pushes, {} pops",
                    pushes, pops);
            }

            #[test]
            fn cells_len_invariant_always_holds(
                width in 1u16..50,
                height in 1u16..50,
            ) {
                let mut buf = Buffer::new(width, height);
                let expected = width as usize * height as usize;

                prop_assert_eq!(buf.cells().len(), expected);

                // After mutations
                buf.set(0, 0, Cell::from_char('X'));
                prop_assert_eq!(buf.cells().len(), expected);

                buf.clear();
                prop_assert_eq!(buf.cells().len(), expected);
            }

            #[test]
            fn set_outside_scissor_is_noop(
                width in 10u16..30,
                height in 10u16..30,
            ) {
                let mut buf = Buffer::new(width, height);
                buf.push_scissor(Rect::new(2, 2, 3, 3));

                // Write outside scissor region
                buf.set(0, 0, Cell::from_char('X'));
                // Should be unmodified (still empty)
                let cell = buf.get(0, 0).unwrap();
                prop_assert!(cell.is_empty(),
                    "cell (0,0) modified outside scissor region");

                // Write inside scissor region should work
                buf.set(3, 3, Cell::from_char('Y'));
                let cell = buf.get(3, 3).unwrap();
                prop_assert_eq!(cell.content.as_char(), Some('Y'));
            }
        }
    }
}
