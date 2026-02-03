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
//!
//! # Dirty Row Tracking (bd-4kq0.1.1)
//!
//! ## Mathematical Invariant
//!
//! Let D be the set of dirty rows. The fundamental soundness property:
//!
//! ```text
//! ∀ y ∈ [0, height): if ∃ x such that old(x, y) ≠ new(x, y), then y ∈ D
//! ```
//!
//! This ensures the diff algorithm can safely skip non-dirty rows without
//! missing any changes. The invariant is maintained by marking rows dirty
//! on every cell mutation.
//!
//! ## Bookkeeping Cost
//!
//! - O(1) per mutation (single array write)
//! - O(height) space for dirty bitmap
//! - Target: < 2% overhead vs baseline rendering

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
    /// Per-row dirty flags for diff optimization.
    ///
    /// When a row is marked dirty, the diff algorithm must compare it cell-by-cell.
    /// Clean rows can be skipped entirely.
    ///
    /// Invariant: `dirty_rows.len() == height`
    dirty_rows: Vec<bool>,
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
            // All rows start dirty to ensure initial diffs against this buffer
            // (e.g. from DoubleBuffer resize) correctly identify it as changed/empty.
            dirty_rows: vec![true; height as usize],
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

    /// Return the height of content (last non-empty row + 1).
    ///
    /// Rows are considered empty only if all cells are the default cell.
    /// Returns 0 if the buffer contains no content.
    pub fn content_height(&self) -> u16 {
        let default_cell = Cell::default();
        let width = self.width as usize;
        for y in (0..self.height).rev() {
            let row_start = y as usize * width;
            let row_end = row_start + width;
            if self.cells[row_start..row_end]
                .iter()
                .any(|cell| *cell != default_cell)
            {
                return y + 1;
            }
        }
        0
    }

    // ----- Dirty Row Tracking API -----

    /// Mark a row as dirty (modified since last clear).
    ///
    /// This is O(1) and must be called on every cell mutation to maintain
    /// the dirty-soundness invariant.
    #[inline]
    fn mark_dirty(&mut self, y: u16) {
        if let Some(slot) = self.dirty_rows.get_mut(y as usize) {
            *slot = true;
        }
    }

    /// Mark all rows as dirty (e.g., after a full clear or bulk write).
    #[inline]
    pub fn mark_all_dirty(&mut self) {
        self.dirty_rows.fill(true);
    }

    /// Reset all dirty flags to clean.
    ///
    /// Call this after the diff has consumed the dirty state (between frames).
    #[inline]
    pub fn clear_dirty(&mut self) {
        self.dirty_rows.fill(false);
    }

    /// Check if a specific row is dirty.
    #[inline]
    pub fn is_row_dirty(&self, y: u16) -> bool {
        self.dirty_rows.get(y as usize).copied().unwrap_or(false)
    }

    /// Get the dirty row flags as a slice.
    ///
    /// Each element corresponds to a row: `true` means the row was modified
    /// since the last `clear_dirty()` call.
    #[inline]
    pub fn dirty_rows(&self) -> &[bool] {
        &self.dirty_rows
    }

    /// Count the number of dirty rows.
    pub fn dirty_row_count(&self) -> usize {
        self.dirty_rows.iter().filter(|&&d| d).count()
    }

    // ----- Coordinate Helpers -----

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
    /// Proactively marks the row dirty since the caller may mutate the cell.
    #[inline]
    pub fn get_mut(&mut self, x: u16, y: u16) -> Option<&mut Cell> {
        self.mark_dirty(y);
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
                let Some(cx) = x.checked_add(i as u16) else {
                    break;
                };
                if let Some(tail_idx) = self.index(cx, y)
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
                                let Some(cx) = back_x.checked_add(i as u16) else {
                                    break;
                                };
                                if let Some(tail_idx) = self.index(cx, y) {
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

            let existing_bg = self.cells[idx].bg;

            // Apply opacity to the incoming cell, then composite over existing background.
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

            final_cell.bg = final_cell.bg.over(existing_bg);

            self.cells[idx] = final_cell;
            self.mark_dirty(y);
            return;
        }

        // Multi-width character atomicity check
        // Ensure ALL cells (head + tail) are within bounds and scissor
        let scissor = self.current_scissor();
        for i in 0..width {
            let Some(cx) = x.checked_add(i as u16) else {
                return;
            };
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
            // Safe: atomicity check above verified x + i fits in u16
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
        self.mark_dirty(y);

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
            self.mark_dirty(y);
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
        self.mark_all_dirty();
    }

    /// Reset per-frame state and clear all cells.
    ///
    /// This restores scissor/opacity stacks to their base values to ensure
    /// each frame starts from a clean rendering state.
    pub fn reset_for_frame(&mut self) {
        self.scissor_stack.truncate(1);
        if let Some(base) = self.scissor_stack.first_mut() {
            *base = Rect::from_size(self.width, self.height);
        } else {
            self.scissor_stack
                .push(Rect::from_size(self.width, self.height));
        }

        self.opacity_stack.truncate(1);
        if let Some(base) = self.opacity_stack.first_mut() {
            *base = 1.0;
        } else {
            self.opacity_stack.push(1.0);
        }

        self.clear();
    }

    /// Clear all cells to the given cell.
    pub fn clear_with(&mut self, cell: Cell) {
        self.cells.fill(cell);
        self.mark_all_dirty();
    }

    /// Get raw access to the cell slice.
    ///
    /// This is useful for diffing against another buffer.
    #[inline]
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// Get mutable raw access to the cell slice.
    ///
    /// Marks all rows dirty since caller may modify arbitrary cells.
    #[inline]
    pub fn cells_mut(&mut self) -> &mut [Cell] {
        self.mark_all_dirty();
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
        // Enforce strict bounds on the destination area to prevent wide characters
        // from leaking outside the requested copy region.
        let copy_bounds = Rect::new(dst_x, dst_y, src_rect.width, src_rect.height);
        self.push_scissor(copy_bounds);

        for dy in 0..src_rect.height {
            // Compute destination y with overflow check
            let Some(target_y) = dst_y.checked_add(dy) else {
                continue;
            };
            let Some(sy) = src_rect.y.checked_add(dy) else {
                continue;
            };

            let mut dx = 0u16;
            while dx < src_rect.width {
                // Compute coordinates with overflow checks
                let Some(target_x) = dst_x.checked_add(dx) else {
                    dx = dx.saturating_add(1);
                    continue;
                };
                let Some(sx) = src_rect.x.checked_add(dx) else {
                    dx = dx.saturating_add(1);
                    continue;
                };

                if let Some(cell) = src.get(sx, sy) {
                    // Handle dangling tail at start of copy region
                    if dx == 0 && cell.is_continuation() {
                        self.set(target_x, target_y, Cell::default());
                        dx = dx.saturating_add(1);
                        continue;
                    }

                    // Write the cell
                    self.set(target_x, target_y, *cell);

                    // If it was a wide char, skip the tails in the source iteration
                    // because `set` already handled them (or rejected the whole char).
                    // Use saturating_add to prevent infinite loop on overflow.
                    let width = cell.content.width();
                    if width > 1 {
                        dx = dx.saturating_add(width as u16);
                    } else {
                        dx = dx.saturating_add(1);
                    }
                } else {
                    dx = dx.saturating_add(1);
                }
            }
        }

        self.pop_scissor();
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

// ---------------------------------------------------------------------------
// DoubleBuffer: O(1) frame swap (bd-1rz0.4.4)
// ---------------------------------------------------------------------------

/// Double-buffered render target with O(1) swap.
///
/// Maintains two pre-allocated buffers and swaps between them by flipping an
/// index, avoiding the O(width × height) clone that a naive prev/current
/// pattern requires.
///
/// # Invariants
///
/// 1. Both buffers always have the same dimensions.
/// 2. `swap()` is O(1) — it only flips the index, never copies cells.
/// 3. After `swap()`, `current_mut().clear()` should be called to prepare
///    the new frame buffer.
/// 4. `resize()` discards both buffers and returns `true` so callers know
///    a full redraw is needed.
#[derive(Debug)]
pub struct DoubleBuffer {
    buffers: [Buffer; 2],
    /// Index of the *current* buffer (0 or 1).
    current_idx: u8,
}

// ---------------------------------------------------------------------------
// AdaptiveDoubleBuffer: Allocation-efficient resize (bd-1rz0.4.2)
// ---------------------------------------------------------------------------

/// Over-allocation factor for growth headroom (1.25x = 25% extra capacity).
const ADAPTIVE_GROWTH_FACTOR: f32 = 1.25;

/// Shrink threshold: only reallocate if new size < this fraction of capacity.
/// This prevents thrashing at size boundaries.
const ADAPTIVE_SHRINK_THRESHOLD: f32 = 0.50;

/// Maximum over-allocation per dimension (prevent excessive memory usage).
const ADAPTIVE_MAX_OVERAGE: u16 = 200;

/// Adaptive double-buffered render target with allocation efficiency.
///
/// Wraps `DoubleBuffer` with capacity tracking to minimize allocations during
/// resize storms. Key strategies:
///
/// 1. **Over-allocation headroom**: Allocate slightly more than needed to handle
///    minor size increases without reallocation.
/// 2. **Shrink threshold**: Only shrink if new size is significantly smaller
///    than allocated capacity (prevents thrashing at size boundaries).
/// 3. **Logical vs physical dimensions**: Track both the current view size
///    and the allocated capacity separately.
///
/// # Invariants
///
/// 1. `capacity_width >= logical_width` and `capacity_height >= logical_height`
/// 2. Logical dimensions represent the actual usable area for rendering.
/// 3. Physical capacity may exceed logical dimensions by up to `ADAPTIVE_GROWTH_FACTOR`.
/// 4. Shrink only occurs when logical size drops below `ADAPTIVE_SHRINK_THRESHOLD * capacity`.
///
/// # Failure Modes
///
/// | Condition | Behavior | Rationale |
/// |-----------|----------|-----------|
/// | Capacity overflow | Clamp to u16::MAX | Prevents panic on extreme sizes |
/// | Zero dimensions | Delegate to DoubleBuffer (panic) | Invalid state |
///
/// # Performance
///
/// - `resize()` is O(1) when the new size fits within capacity.
/// - `resize()` is O(width × height) when reallocation is required.
/// - Target: < 5% allocation overhead during resize storms.
#[derive(Debug)]
pub struct AdaptiveDoubleBuffer {
    /// The underlying double buffer (may have larger capacity than logical size).
    inner: DoubleBuffer,
    /// Logical width (the usable rendering area).
    logical_width: u16,
    /// Logical height (the usable rendering area).
    logical_height: u16,
    /// Allocated capacity width (>= logical_width).
    capacity_width: u16,
    /// Allocated capacity height (>= logical_height).
    capacity_height: u16,
    /// Statistics for observability.
    stats: AdaptiveStats,
}

/// Statistics for adaptive buffer allocation.
#[derive(Debug, Clone, Default)]
pub struct AdaptiveStats {
    /// Number of resize calls that avoided reallocation.
    pub resize_avoided: u64,
    /// Number of resize calls that required reallocation.
    pub resize_reallocated: u64,
    /// Number of resize calls for growth.
    pub resize_growth: u64,
    /// Number of resize calls for shrink.
    pub resize_shrink: u64,
}

impl AdaptiveStats {
    /// Reset statistics to zero.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Calculate the reallocation avoidance ratio (higher is better).
    pub fn avoidance_ratio(&self) -> f64 {
        let total = self.resize_avoided + self.resize_reallocated;
        if total == 0 {
            1.0
        } else {
            self.resize_avoided as f64 / total as f64
        }
    }
}

impl DoubleBuffer {
    /// Create a double buffer with the given dimensions.
    ///
    /// Both buffers are initialized to default (empty) cells.
    ///
    /// # Panics
    ///
    /// Panics if width or height is 0.
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            buffers: [Buffer::new(width, height), Buffer::new(width, height)],
            current_idx: 0,
        }
    }

    /// O(1) swap: the current buffer becomes previous, and vice versa.
    ///
    /// After swapping, call `current_mut().clear()` to prepare for the
    /// next frame.
    #[inline]
    pub fn swap(&mut self) {
        self.current_idx = 1 - self.current_idx;
    }

    /// Reference to the current (in-progress) frame buffer.
    #[inline]
    pub fn current(&self) -> &Buffer {
        &self.buffers[self.current_idx as usize]
    }

    /// Mutable reference to the current (in-progress) frame buffer.
    #[inline]
    pub fn current_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[self.current_idx as usize]
    }

    /// Reference to the previous (last-presented) frame buffer.
    #[inline]
    pub fn previous(&self) -> &Buffer {
        &self.buffers[(1 - self.current_idx) as usize]
    }

    /// Mutable reference to the previous (last-presented) frame buffer.
    #[inline]
    pub fn previous_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[(1 - self.current_idx) as usize]
    }

    /// Width of both buffers.
    #[inline]
    pub fn width(&self) -> u16 {
        self.buffers[0].width()
    }

    /// Height of both buffers.
    #[inline]
    pub fn height(&self) -> u16 {
        self.buffers[0].height()
    }

    /// Resize both buffers. Returns `true` if dimensions actually changed.
    ///
    /// Both buffers are replaced with fresh allocations and the index is
    /// reset. Callers should force a full redraw when this returns `true`.
    pub fn resize(&mut self, width: u16, height: u16) -> bool {
        if self.buffers[0].width() == width && self.buffers[0].height() == height {
            return false;
        }
        self.buffers = [Buffer::new(width, height), Buffer::new(width, height)];
        self.current_idx = 0;
        true
    }

    /// Check whether both buffers have the given dimensions.
    #[inline]
    pub fn dimensions_match(&self, width: u16, height: u16) -> bool {
        self.buffers[0].width() == width && self.buffers[0].height() == height
    }
}

// ---------------------------------------------------------------------------
// AdaptiveDoubleBuffer implementation (bd-1rz0.4.2)
// ---------------------------------------------------------------------------

impl AdaptiveDoubleBuffer {
    /// Create a new adaptive buffer with the given logical dimensions.
    ///
    /// Initial capacity is set with growth headroom applied.
    ///
    /// # Panics
    ///
    /// Panics if width or height is 0.
    pub fn new(width: u16, height: u16) -> Self {
        let (cap_w, cap_h) = Self::compute_capacity(width, height);
        Self {
            inner: DoubleBuffer::new(cap_w, cap_h),
            logical_width: width,
            logical_height: height,
            capacity_width: cap_w,
            capacity_height: cap_h,
            stats: AdaptiveStats::default(),
        }
    }

    /// Compute the capacity for a given logical size.
    ///
    /// Applies growth factor with clamping to prevent overflow.
    fn compute_capacity(width: u16, height: u16) -> (u16, u16) {
        let extra_w =
            ((width as f32 * (ADAPTIVE_GROWTH_FACTOR - 1.0)) as u16).min(ADAPTIVE_MAX_OVERAGE);
        let extra_h =
            ((height as f32 * (ADAPTIVE_GROWTH_FACTOR - 1.0)) as u16).min(ADAPTIVE_MAX_OVERAGE);

        let cap_w = width.saturating_add(extra_w);
        let cap_h = height.saturating_add(extra_h);

        (cap_w, cap_h)
    }

    /// Check if the new dimensions require reallocation.
    ///
    /// Returns `true` if reallocation is needed, `false` if current capacity suffices.
    fn needs_reallocation(&self, width: u16, height: u16) -> bool {
        // Growth beyond capacity always requires reallocation
        if width > self.capacity_width || height > self.capacity_height {
            return true;
        }

        // Shrink threshold: reallocate if new size is significantly smaller
        let shrink_threshold_w = (self.capacity_width as f32 * ADAPTIVE_SHRINK_THRESHOLD) as u16;
        let shrink_threshold_h = (self.capacity_height as f32 * ADAPTIVE_SHRINK_THRESHOLD) as u16;

        width < shrink_threshold_w || height < shrink_threshold_h
    }

    /// O(1) swap: the current buffer becomes previous, and vice versa.
    ///
    /// After swapping, call `current_mut().clear()` to prepare for the
    /// next frame.
    #[inline]
    pub fn swap(&mut self) {
        self.inner.swap();
    }

    /// Reference to the current (in-progress) frame buffer.
    ///
    /// Note: The buffer may have larger dimensions than the logical size.
    /// Use `logical_width()` and `logical_height()` for rendering bounds.
    #[inline]
    pub fn current(&self) -> &Buffer {
        self.inner.current()
    }

    /// Mutable reference to the current (in-progress) frame buffer.
    #[inline]
    pub fn current_mut(&mut self) -> &mut Buffer {
        self.inner.current_mut()
    }

    /// Reference to the previous (last-presented) frame buffer.
    #[inline]
    pub fn previous(&self) -> &Buffer {
        self.inner.previous()
    }

    /// Logical width (the usable rendering area).
    #[inline]
    pub fn width(&self) -> u16 {
        self.logical_width
    }

    /// Logical height (the usable rendering area).
    #[inline]
    pub fn height(&self) -> u16 {
        self.logical_height
    }

    /// Allocated capacity width (may be larger than logical width).
    #[inline]
    pub fn capacity_width(&self) -> u16 {
        self.capacity_width
    }

    /// Allocated capacity height (may be larger than logical height).
    #[inline]
    pub fn capacity_height(&self) -> u16 {
        self.capacity_height
    }

    /// Get allocation statistics.
    #[inline]
    pub fn stats(&self) -> &AdaptiveStats {
        &self.stats
    }

    /// Reset allocation statistics.
    pub fn reset_stats(&mut self) {
        self.stats.reset();
    }

    /// Resize the logical dimensions. Returns `true` if dimensions changed.
    ///
    /// This method minimizes allocations by:
    /// 1. Reusing existing capacity when the new size fits.
    /// 2. Only reallocating on significant shrink (below threshold).
    /// 3. Applying growth headroom to avoid immediate reallocation on growth.
    ///
    /// # Performance
    ///
    /// - O(1) when new size fits within existing capacity.
    /// - O(width × height) when reallocation is required.
    pub fn resize(&mut self, width: u16, height: u16) -> bool {
        // No change in logical dimensions
        if width == self.logical_width && height == self.logical_height {
            return false;
        }

        let is_growth = width > self.logical_width || height > self.logical_height;
        if is_growth {
            self.stats.resize_growth += 1;
        } else {
            self.stats.resize_shrink += 1;
        }

        if self.needs_reallocation(width, height) {
            // Reallocate with new capacity
            let (cap_w, cap_h) = Self::compute_capacity(width, height);
            self.inner = DoubleBuffer::new(cap_w, cap_h);
            self.capacity_width = cap_w;
            self.capacity_height = cap_h;
            self.stats.resize_reallocated += 1;
        } else {
            // Reuse existing capacity - just update logical dimensions
            // Clear both buffers to avoid stale content outside new bounds
            self.inner.current_mut().clear();
            self.inner.previous_mut().clear();
            self.stats.resize_avoided += 1;
        }

        self.logical_width = width;
        self.logical_height = height;
        true
    }

    /// Check whether logical dimensions match the given values.
    #[inline]
    pub fn dimensions_match(&self, width: u16, height: u16) -> bool {
        self.logical_width == width && self.logical_height == height
    }

    /// Get the logical bounding rect (for scissoring/rendering).
    #[inline]
    pub fn logical_bounds(&self) -> Rect {
        Rect::from_size(self.logical_width, self.logical_height)
    }

    /// Calculate memory efficiency (logical cells / capacity cells).
    pub fn memory_efficiency(&self) -> f64 {
        let logical = self.logical_width as u64 * self.logical_height as u64;
        let capacity = self.capacity_width as u64 * self.capacity_height as u64;
        if capacity == 0 {
            1.0
        } else {
            logical as f64 / capacity as f64
        }
    }
}

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
    fn content_height_empty_is_zero() {
        let buf = Buffer::new(8, 4);
        assert_eq!(buf.content_height(), 0);
    }

    #[test]
    fn content_height_tracks_last_non_empty_row() {
        let mut buf = Buffer::new(5, 4);
        buf.set(0, 0, Cell::from_char('A'));
        assert_eq!(buf.content_height(), 1);

        buf.set(2, 3, Cell::from_char('Z'));
        assert_eq!(buf.content_height(), 4);
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
    fn opacity_composites_background_before_storage() {
        let mut buf = Buffer::new(1, 1);

        let red = PackedRgba::rgb(255, 0, 0);
        let blue = PackedRgba::rgb(0, 0, 255);

        buf.set(0, 0, Cell::default().with_bg(red));
        buf.push_opacity(0.5);
        buf.set(0, 0, Cell::default().with_bg(blue));

        let stored = buf.get(0, 0).unwrap();
        let expected = blue.with_opacity(0.5).over(red);
        assert_eq!(stored.bg, expected);
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
    fn copy_from_clips_wide_char_at_boundary() {
        let mut src = Buffer::new(10, 1);
        // Wide char at x=0 (width 2)
        src.set(0, 0, Cell::from_char('中'));

        let mut dst = Buffer::new(10, 1);
        // Copy only the first column (x=0, width=1) from src to dst at (0,0)
        // This includes the head of '中' but EXCLUDES the tail.
        dst.copy_from(&src, Rect::new(0, 0, 1, 1), 0, 0);

        // The copy should be atomic: since the tail doesn't fit in the copy region,
        // the head should NOT be written (or at least the tail should not be written outside the region).

        // Check x=0: Should be empty (atomic rejection) or clipped?
        // With implicit scissor fix: atomic rejection means x=0 is empty.
        // Without fix: x=0 is '中', x=1 is CONTINUATION (leak).

        // Asserting the fix behavior (atomic rejection):
        assert!(
            dst.get(0, 0).unwrap().is_empty(),
            "Wide char head should not be written if tail is clipped"
        );
        assert!(
            dst.get(1, 0).unwrap().is_empty(),
            "Wide char tail should not be leaked outside copy region"
        );
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

    // ========== Wide Glyph Continuation Cleanup Tests ==========

    #[test]
    fn overwrite_wide_head_with_single_clears_tails() {
        let mut buf = Buffer::new(10, 1);

        // Write a wide character (width 2) at position 0
        buf.set(0, 0, Cell::from_char('中'));
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('中'));
        assert!(buf.get(1, 0).unwrap().is_continuation());

        // Overwrite the head with a single-width character
        buf.set(0, 0, Cell::from_char('A'));

        // Head should be replaced
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('A'));
        // Tail (continuation) should be cleared to default
        assert!(
            buf.get(1, 0).unwrap().is_empty(),
            "Continuation at x=1 should be cleared when head is overwritten"
        );
    }

    #[test]
    fn overwrite_continuation_with_single_clears_head_and_tails() {
        let mut buf = Buffer::new(10, 1);

        // Write a wide character at position 0
        buf.set(0, 0, Cell::from_char('中'));
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('中'));
        assert!(buf.get(1, 0).unwrap().is_continuation());

        // Overwrite the continuation (position 1) with a single-width char
        buf.set(1, 0, Cell::from_char('B'));

        // The head at position 0 should be cleared
        assert!(
            buf.get(0, 0).unwrap().is_empty(),
            "Head at x=0 should be cleared when its continuation is overwritten"
        );
        // Position 1 should have the new character
        assert_eq!(buf.get(1, 0).unwrap().content.as_char(), Some('B'));
    }

    #[test]
    fn overwrite_wide_with_another_wide() {
        let mut buf = Buffer::new(10, 1);

        // Write first wide character
        buf.set(0, 0, Cell::from_char('中'));
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('中'));
        assert!(buf.get(1, 0).unwrap().is_continuation());

        // Overwrite with another wide character
        buf.set(0, 0, Cell::from_char('日'));

        // Should have new wide character
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('日'));
        assert!(
            buf.get(1, 0).unwrap().is_continuation(),
            "Continuation should still exist for new wide char"
        );
    }

    #[test]
    fn overwrite_continuation_middle_of_wide_sequence() {
        let mut buf = Buffer::new(10, 1);

        // Write two adjacent wide characters: 中 at 0-1, 日 at 2-3
        buf.set(0, 0, Cell::from_char('中'));
        buf.set(2, 0, Cell::from_char('日'));

        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('中'));
        assert!(buf.get(1, 0).unwrap().is_continuation());
        assert_eq!(buf.get(2, 0).unwrap().content.as_char(), Some('日'));
        assert!(buf.get(3, 0).unwrap().is_continuation());

        // Overwrite position 1 (continuation of first wide char)
        buf.set(1, 0, Cell::from_char('X'));

        // First wide char's head should be cleared
        assert!(
            buf.get(0, 0).unwrap().is_empty(),
            "Head of first wide char should be cleared"
        );
        // Position 1 has new char
        assert_eq!(buf.get(1, 0).unwrap().content.as_char(), Some('X'));
        // Second wide char should be unaffected
        assert_eq!(buf.get(2, 0).unwrap().content.as_char(), Some('日'));
        assert!(buf.get(3, 0).unwrap().is_continuation());
    }

    #[test]
    fn wide_char_overlapping_previous_wide_char() {
        let mut buf = Buffer::new(10, 1);

        // Write wide char at position 0
        buf.set(0, 0, Cell::from_char('中'));
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('中'));
        assert!(buf.get(1, 0).unwrap().is_continuation());

        // Write another wide char at position 1 (overlaps with continuation)
        buf.set(1, 0, Cell::from_char('日'));

        // First wide char's head should be cleared (its continuation was overwritten)
        assert!(
            buf.get(0, 0).unwrap().is_empty(),
            "First wide char head should be cleared when continuation is overwritten by new wide"
        );
        // New wide char at positions 1-2
        assert_eq!(buf.get(1, 0).unwrap().content.as_char(), Some('日'));
        assert!(buf.get(2, 0).unwrap().is_continuation());
    }

    #[test]
    fn wide_char_at_end_of_buffer_atomic_reject() {
        let mut buf = Buffer::new(5, 1);

        // Try to write wide char at position 4 (would need position 5 for tail, out of bounds)
        buf.set(4, 0, Cell::from_char('中'));

        // Should be rejected atomically - nothing written
        assert!(
            buf.get(4, 0).unwrap().is_empty(),
            "Wide char should be rejected when tail would be out of bounds"
        );
    }

    #[test]
    fn three_wide_chars_sequential_cleanup() {
        let mut buf = Buffer::new(10, 1);

        // Write three wide chars: positions 0-1, 2-3, 4-5
        buf.set(0, 0, Cell::from_char('一'));
        buf.set(2, 0, Cell::from_char('二'));
        buf.set(4, 0, Cell::from_char('三'));

        // Verify initial state
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('一'));
        assert!(buf.get(1, 0).unwrap().is_continuation());
        assert_eq!(buf.get(2, 0).unwrap().content.as_char(), Some('二'));
        assert!(buf.get(3, 0).unwrap().is_continuation());
        assert_eq!(buf.get(4, 0).unwrap().content.as_char(), Some('三'));
        assert!(buf.get(5, 0).unwrap().is_continuation());

        // Overwrite middle wide char's continuation with single char
        buf.set(3, 0, Cell::from_char('M'));

        // First wide char should be unaffected
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('一'));
        assert!(buf.get(1, 0).unwrap().is_continuation());
        // Middle wide char's head should be cleared
        assert!(buf.get(2, 0).unwrap().is_empty());
        // Position 3 has new char
        assert_eq!(buf.get(3, 0).unwrap().content.as_char(), Some('M'));
        // Third wide char should be unaffected
        assert_eq!(buf.get(4, 0).unwrap().content.as_char(), Some('三'));
        assert!(buf.get(5, 0).unwrap().is_continuation());
    }

    #[test]
    fn overwrite_empty_cell_no_cleanup_needed() {
        let mut buf = Buffer::new(10, 1);

        // Write to an empty cell - no cleanup should be needed
        buf.set(5, 0, Cell::from_char('X'));

        assert_eq!(buf.get(5, 0).unwrap().content.as_char(), Some('X'));
        // Adjacent cells should still be empty
        assert!(buf.get(4, 0).unwrap().is_empty());
        assert!(buf.get(6, 0).unwrap().is_empty());
    }

    #[test]
    fn wide_char_cleanup_with_opacity() {
        let mut buf = Buffer::new(10, 1);

        // Set background
        buf.set(0, 0, Cell::default().with_bg(PackedRgba::rgb(255, 0, 0)));
        buf.set(1, 0, Cell::default().with_bg(PackedRgba::rgb(0, 255, 0)));

        // Write wide char
        buf.set(0, 0, Cell::from_char('中'));

        // Overwrite with opacity
        buf.push_opacity(0.5);
        buf.set(0, 0, Cell::from_char('A'));
        buf.pop_opacity();

        // Check head is replaced
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('A'));
        // Continuation should be cleared
        assert!(buf.get(1, 0).unwrap().is_empty());
    }

    #[test]
    fn wide_char_continuation_not_treated_as_head() {
        let mut buf = Buffer::new(10, 1);

        // Write a wide character
        buf.set(0, 0, Cell::from_char('中'));

        // Verify the continuation cell has zero width (not treated as a head)
        let cont = buf.get(1, 0).unwrap();
        assert!(cont.is_continuation());
        assert_eq!(cont.content.width(), 0);

        // Writing another wide char starting at position 1 should work correctly
        buf.set(1, 0, Cell::from_char('日'));

        // Original head should be cleared
        assert!(buf.get(0, 0).unwrap().is_empty());
        // New wide char at 1-2
        assert_eq!(buf.get(1, 0).unwrap().content.as_char(), Some('日'));
        assert!(buf.get(2, 0).unwrap().is_continuation());
    }

    #[test]
    fn wide_char_fill_region() {
        let mut buf = Buffer::new(10, 3);

        // Fill a 4x2 region with a wide character
        // Due to atomicity, only even x positions will have heads
        let wide_cell = Cell::from_char('中');
        buf.fill(Rect::new(0, 0, 4, 2), wide_cell);

        // Check row 0: positions 0,1 should have wide char, 2,3 should have another
        // Actually, fill calls set for each position, so:
        // - set(0,0) writes '中' at 0, CONT at 1
        // - set(1,0) overwrites CONT, clears head at 0, writes '中' at 1, CONT at 2
        // - set(2,0) overwrites CONT, clears head at 1, writes '中' at 2, CONT at 3
        // - set(3,0) overwrites CONT, clears head at 2, writes '中' at 3... but 4 is out of fill region
        // Wait, fill only goes to right() which is x + width = 0 + 4 = 4, so x in 0..4

        // Actually the behavior depends on whether the wide char fits.
        // Let me trace through: fill iterates x in 0..4, y in 0..2
        // For y=0: set(0,0), set(1,0), set(2,0), set(3,0) with wide char
        // Each set with wide char checks if x+1 is in bounds and scissor.
        // set(3,0) with '中' needs positions 3,4 - position 4 is in bounds (buf width 10)
        // So it should write.

        // The pattern should be: each write of a wide char disrupts previous
        // Final state after fill: position 3 has head, position 4 has continuation
        // (because set(3,0) is last and overwrites previous wide chars)

        // This is a complex interaction - let's just verify no panics and some structure
        // The final state at row 0, x=3 should have '中'
        assert_eq!(buf.get(3, 0).unwrap().content.as_char(), Some('中'));
    }

    #[test]
    fn default_buffer_dimensions() {
        let buf = Buffer::default();
        assert_eq!(buf.width(), 1);
        assert_eq!(buf.height(), 1);
        assert_eq!(buf.len(), 1);
    }

    #[test]
    fn buffer_partial_eq_impl() {
        let buf1 = Buffer::new(5, 5);
        let buf2 = Buffer::new(5, 5);
        let mut buf3 = Buffer::new(5, 5);
        buf3.set(0, 0, Cell::from_char('X'));

        assert_eq!(buf1, buf2);
        assert_ne!(buf1, buf3);
    }

    #[test]
    fn degradation_level_accessible() {
        let mut buf = Buffer::new(10, 10);
        assert_eq!(buf.degradation, DegradationLevel::Full);

        buf.degradation = DegradationLevel::SimpleBorders;
        assert_eq!(buf.degradation, DegradationLevel::SimpleBorders);
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

            // --- Wide Glyph Cleanup Property Tests ---

            #[test]
            fn wide_char_overwrites_cleanup_tails(
                width in 10u16..30,
                x in 0u16..8,
            ) {
                let x = x % (width.saturating_sub(2).max(1));
                let mut buf = Buffer::new(width, 1);

                // Write wide char
                buf.set(x, 0, Cell::from_char('中'));

                // If it fit, check structure
                if x + 1 < width {
                    let head = buf.get(x, 0).unwrap();
                    let tail = buf.get(x + 1, 0).unwrap();

                    if head.content.as_char() == Some('中') {
                        prop_assert!(tail.is_continuation(),
                            "tail at x+1={} should be continuation", x + 1);

                        // Overwrite head with single char
                        buf.set(x, 0, Cell::from_char('A'));
                        let new_head = buf.get(x, 0).unwrap();
                        let cleared_tail = buf.get(x + 1, 0).unwrap();

                        prop_assert_eq!(new_head.content.as_char(), Some('A'));
                        prop_assert!(cleared_tail.is_empty(),
                            "tail should be cleared after head overwrite");
                    }
                }
            }

            #[test]
            fn wide_char_atomic_rejection_at_boundary(
                width in 3u16..20,
            ) {
                let mut buf = Buffer::new(width, 1);

                // Try to write wide char at last position (needs x and x+1)
                let last_pos = width - 1;
                buf.set(last_pos, 0, Cell::from_char('中'));

                // Should be rejected - cell should remain empty
                let cell = buf.get(last_pos, 0).unwrap();
                prop_assert!(cell.is_empty(),
                    "wide char at boundary position {} (width {}) should be rejected",
                    last_pos, width);
            }

            // =====================================================================
            // DoubleBuffer property tests (bd-1rz0.4.4)
            // =====================================================================

            #[test]
            fn double_buffer_swap_is_involution(ops in proptest::collection::vec(proptest::bool::ANY, 0..100)) {
                let mut db = DoubleBuffer::new(10, 10);
                let initial_idx = db.current_idx;

                for do_swap in &ops {
                    if *do_swap {
                        db.swap();
                    }
                }

                let swap_count = ops.iter().filter(|&&x| x).count();
                let expected_idx = if swap_count % 2 == 0 { initial_idx } else { 1 - initial_idx };

                prop_assert_eq!(db.current_idx, expected_idx,
                    "After {} swaps, index should be {} but was {}",
                    swap_count, expected_idx, db.current_idx);
            }

            #[test]
            fn double_buffer_resize_preserves_invariant(
                init_w in 1u16..200,
                init_h in 1u16..100,
                new_w in 1u16..200,
                new_h in 1u16..100,
            ) {
                let mut db = DoubleBuffer::new(init_w, init_h);
                db.resize(new_w, new_h);

                prop_assert_eq!(db.width(), new_w);
                prop_assert_eq!(db.height(), new_h);
                prop_assert!(db.dimensions_match(new_w, new_h));
            }

            #[test]
            fn double_buffer_current_previous_disjoint(
                width in 1u16..50,
                height in 1u16..50,
            ) {
                let mut db = DoubleBuffer::new(width, height);

                // Write to current
                db.current_mut().set(0, 0, Cell::from_char('C'));

                // Previous should be unaffected
                prop_assert!(db.previous().get(0, 0).unwrap().is_empty(),
                    "Previous buffer should not reflect changes to current");

                // After swap, roles reverse
                db.swap();
                prop_assert_eq!(db.previous().get(0, 0).unwrap().content.as_char(), Some('C'),
                    "After swap, previous should have the 'C' we wrote");
            }

            #[test]
            fn double_buffer_swap_content_semantics(
                width in 5u16..30,
                height in 5u16..30,
            ) {
                let mut db = DoubleBuffer::new(width, height);

                // Write 'X' to current
                db.current_mut().set(0, 0, Cell::from_char('X'));
                db.swap();

                // Write 'Y' to current (now the other buffer)
                db.current_mut().set(0, 0, Cell::from_char('Y'));
                db.swap();

                // After two swaps, we're back to the buffer with 'X'
                prop_assert_eq!(db.current().get(0, 0).unwrap().content.as_char(), Some('X'));
                prop_assert_eq!(db.previous().get(0, 0).unwrap().content.as_char(), Some('Y'));
            }

            #[test]
            fn double_buffer_resize_clears_both(
                w1 in 5u16..30,
                h1 in 5u16..30,
                w2 in 5u16..30,
                h2 in 5u16..30,
            ) {
                // Skip if dimensions are the same (resize returns early)
                prop_assume!(w1 != w2 || h1 != h2);

                let mut db = DoubleBuffer::new(w1, h1);

                // Populate both buffers
                db.current_mut().set(0, 0, Cell::from_char('A'));
                db.swap();
                db.current_mut().set(0, 0, Cell::from_char('B'));

                // Resize
                db.resize(w2, h2);

                // Both should be empty
                prop_assert!(db.current().get(0, 0).unwrap().is_empty(),
                    "Current buffer should be empty after resize");
                prop_assert!(db.previous().get(0, 0).unwrap().is_empty(),
                    "Previous buffer should be empty after resize");
            }
        }
    }

    // ========== Dirty Row Tracking Tests (bd-4kq0.1.1) ==========

    #[test]
    fn dirty_rows_start_dirty() {
        // All rows start dirty to ensure initial diffs see all content.
        let buf = Buffer::new(10, 5);
        assert_eq!(buf.dirty_row_count(), 5);
        for y in 0..5 {
            assert!(buf.is_row_dirty(y));
        }
    }

    #[test]
    fn set_marks_row_dirty() {
        let mut buf = Buffer::new(10, 5);
        buf.clear_dirty(); // Reset initial dirty state
        buf.set(3, 2, Cell::from_char('X'));
        assert!(buf.is_row_dirty(2));
        assert!(!buf.is_row_dirty(0));
        assert!(!buf.is_row_dirty(1));
        assert!(!buf.is_row_dirty(3));
        assert!(!buf.is_row_dirty(4));
    }

    #[test]
    fn set_raw_marks_row_dirty() {
        let mut buf = Buffer::new(10, 5);
        buf.clear_dirty(); // Reset initial dirty state
        buf.set_raw(0, 4, Cell::from_char('Z'));
        assert!(buf.is_row_dirty(4));
        assert_eq!(buf.dirty_row_count(), 1);
    }

    #[test]
    fn clear_marks_all_dirty() {
        let mut buf = Buffer::new(10, 5);
        buf.clear();
        assert_eq!(buf.dirty_row_count(), 5);
    }

    #[test]
    fn clear_dirty_resets_flags() {
        let mut buf = Buffer::new(10, 5);
        // All rows start dirty; clear_dirty should reset all of them.
        assert_eq!(buf.dirty_row_count(), 5);
        buf.clear_dirty();
        assert_eq!(buf.dirty_row_count(), 0);

        // Now mark specific rows dirty and verify clear_dirty resets again.
        buf.set(0, 0, Cell::from_char('A'));
        buf.set(0, 3, Cell::from_char('B'));
        assert_eq!(buf.dirty_row_count(), 2);

        buf.clear_dirty();
        assert_eq!(buf.dirty_row_count(), 0);
    }

    #[test]
    fn fill_marks_affected_rows_dirty() {
        let mut buf = Buffer::new(10, 10);
        buf.clear_dirty(); // Reset initial dirty state
        buf.fill(Rect::new(0, 2, 5, 3), Cell::from_char('.'));
        // Rows 2, 3, 4 should be dirty
        assert!(!buf.is_row_dirty(0));
        assert!(!buf.is_row_dirty(1));
        assert!(buf.is_row_dirty(2));
        assert!(buf.is_row_dirty(3));
        assert!(buf.is_row_dirty(4));
        assert!(!buf.is_row_dirty(5));
    }

    #[test]
    fn get_mut_marks_row_dirty() {
        let mut buf = Buffer::new(10, 5);
        buf.clear_dirty(); // Reset initial dirty state
        if let Some(cell) = buf.get_mut(5, 3) {
            cell.fg = PackedRgba::rgb(255, 0, 0);
        }
        assert!(buf.is_row_dirty(3));
        assert_eq!(buf.dirty_row_count(), 1);
    }

    #[test]
    fn cells_mut_marks_all_dirty() {
        let mut buf = Buffer::new(10, 5);
        let _ = buf.cells_mut();
        assert_eq!(buf.dirty_row_count(), 5);
    }

    #[test]
    fn dirty_rows_slice_length_matches_height() {
        let buf = Buffer::new(10, 7);
        assert_eq!(buf.dirty_rows().len(), 7);
    }

    #[test]
    fn out_of_bounds_set_does_not_dirty() {
        let mut buf = Buffer::new(10, 5);
        buf.clear_dirty(); // Reset initial dirty state
        buf.set(100, 100, Cell::from_char('X'));
        assert_eq!(buf.dirty_row_count(), 0);
    }

    #[test]
    fn property_dirty_soundness() {
        // Randomized test: any mutation must mark its row.
        let mut buf = Buffer::new(20, 10);
        let positions = [(3, 0), (5, 2), (0, 9), (19, 5), (10, 7)];
        for &(x, y) in &positions {
            buf.set(x, y, Cell::from_char('*'));
        }
        for &(_, y) in &positions {
            assert!(
                buf.is_row_dirty(y),
                "Row {} should be dirty after set({}, {})",
                y,
                positions.iter().find(|(_, ry)| *ry == y).unwrap().0,
                y
            );
        }
    }

    #[test]
    fn dirty_clear_between_frames() {
        // Simulates frame transition: render, diff, clear, render again.
        let mut buf = Buffer::new(10, 5);

        // All rows start dirty (initial frame needs full diff).
        assert_eq!(buf.dirty_row_count(), 5);

        // Diff consumes dirty state after initial frame.
        buf.clear_dirty();
        assert_eq!(buf.dirty_row_count(), 0);

        // Frame 1: write to rows 0, 2
        buf.set(0, 0, Cell::from_char('A'));
        buf.set(0, 2, Cell::from_char('B'));
        assert_eq!(buf.dirty_row_count(), 2);

        // Diff consumes dirty state
        buf.clear_dirty();
        assert_eq!(buf.dirty_row_count(), 0);

        // Frame 2: write to row 4 only
        buf.set(0, 4, Cell::from_char('C'));
        assert_eq!(buf.dirty_row_count(), 1);
        assert!(buf.is_row_dirty(4));
        assert!(!buf.is_row_dirty(0));
    }

    // =====================================================================
    // DoubleBuffer tests (bd-1rz0.4.4)
    // =====================================================================

    #[test]
    fn double_buffer_new_has_matching_dimensions() {
        let db = DoubleBuffer::new(80, 24);
        assert_eq!(db.width(), 80);
        assert_eq!(db.height(), 24);
        assert!(db.dimensions_match(80, 24));
        assert!(!db.dimensions_match(120, 40));
    }

    #[test]
    fn double_buffer_swap_is_o1() {
        let mut db = DoubleBuffer::new(80, 24);

        // Write to current buffer
        db.current_mut().set(0, 0, Cell::from_char('A'));
        assert_eq!(db.current().get(0, 0).unwrap().content.as_char(), Some('A'));

        // Swap — previous should now have 'A', current should be clean
        db.swap();
        assert_eq!(
            db.previous().get(0, 0).unwrap().content.as_char(),
            Some('A')
        );
        // Current was the old "previous" (empty by default)
        assert!(db.current().get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn double_buffer_swap_round_trip() {
        let mut db = DoubleBuffer::new(10, 5);

        db.current_mut().set(0, 0, Cell::from_char('X'));
        db.swap();
        db.current_mut().set(0, 0, Cell::from_char('Y'));
        db.swap();

        // After two swaps, we're back to the buffer that had 'X'
        assert_eq!(db.current().get(0, 0).unwrap().content.as_char(), Some('X'));
        assert_eq!(
            db.previous().get(0, 0).unwrap().content.as_char(),
            Some('Y')
        );
    }

    #[test]
    fn double_buffer_resize_changes_dimensions() {
        let mut db = DoubleBuffer::new(80, 24);
        assert!(!db.resize(80, 24)); // No change
        assert!(db.resize(120, 40)); // Changed
        assert_eq!(db.width(), 120);
        assert_eq!(db.height(), 40);
        assert!(db.dimensions_match(120, 40));
    }

    #[test]
    fn double_buffer_resize_clears_content() {
        let mut db = DoubleBuffer::new(10, 5);
        db.current_mut().set(0, 0, Cell::from_char('Z'));
        db.swap();
        db.current_mut().set(0, 0, Cell::from_char('W'));

        db.resize(20, 10);

        // Both buffers should be fresh/empty
        assert!(db.current().get(0, 0).unwrap().is_empty());
        assert!(db.previous().get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn double_buffer_current_and_previous_are_distinct() {
        let mut db = DoubleBuffer::new(10, 5);
        db.current_mut().set(0, 0, Cell::from_char('C'));

        // Previous should not reflect changes to current
        assert!(db.previous().get(0, 0).unwrap().is_empty());
        assert_eq!(db.current().get(0, 0).unwrap().content.as_char(), Some('C'));
    }

    // =====================================================================
    // AdaptiveDoubleBuffer tests (bd-1rz0.4.2)
    // =====================================================================

    #[test]
    fn adaptive_buffer_new_has_over_allocation() {
        let adb = AdaptiveDoubleBuffer::new(80, 24);

        // Logical dimensions match requested size
        assert_eq!(adb.width(), 80);
        assert_eq!(adb.height(), 24);
        assert!(adb.dimensions_match(80, 24));

        // Capacity should be larger (1.25x growth factor, capped at 200)
        // 80 * 0.25 = 20, so capacity_width = 100
        // 24 * 0.25 = 6, so capacity_height = 30
        assert!(adb.capacity_width() > 80);
        assert!(adb.capacity_height() > 24);
        assert_eq!(adb.capacity_width(), 100); // 80 + 20
        assert_eq!(adb.capacity_height(), 30); // 24 + 6
    }

    #[test]
    fn adaptive_buffer_resize_avoids_reallocation_when_within_capacity() {
        let mut adb = AdaptiveDoubleBuffer::new(80, 24);

        // Small growth should be absorbed by over-allocation
        assert!(adb.resize(90, 28)); // Still within (100, 30) capacity
        assert_eq!(adb.width(), 90);
        assert_eq!(adb.height(), 28);
        assert_eq!(adb.stats().resize_avoided, 1);
        assert_eq!(adb.stats().resize_reallocated, 0);
        assert_eq!(adb.stats().resize_growth, 1);
    }

    #[test]
    fn adaptive_buffer_resize_reallocates_on_growth_beyond_capacity() {
        let mut adb = AdaptiveDoubleBuffer::new(80, 24);

        // Growth beyond capacity requires reallocation
        assert!(adb.resize(120, 40)); // Exceeds (100, 30) capacity
        assert_eq!(adb.width(), 120);
        assert_eq!(adb.height(), 40);
        assert_eq!(adb.stats().resize_reallocated, 1);
        assert_eq!(adb.stats().resize_avoided, 0);

        // New capacity should have headroom
        assert!(adb.capacity_width() > 120);
        assert!(adb.capacity_height() > 40);
    }

    #[test]
    fn adaptive_buffer_resize_reallocates_on_significant_shrink() {
        let mut adb = AdaptiveDoubleBuffer::new(100, 50);

        // Shrink below 50% threshold should reallocate
        // Threshold: 100 * 0.5 = 50, 50 * 0.5 = 25
        assert!(adb.resize(40, 20)); // Below 50% of capacity
        assert_eq!(adb.width(), 40);
        assert_eq!(adb.height(), 20);
        assert_eq!(adb.stats().resize_reallocated, 1);
        assert_eq!(adb.stats().resize_shrink, 1);
    }

    #[test]
    fn adaptive_buffer_resize_avoids_reallocation_on_minor_shrink() {
        let mut adb = AdaptiveDoubleBuffer::new(100, 50);

        // Shrink above 50% threshold should reuse capacity
        // Threshold: capacity ~125 * 0.5 = 62.5 for width
        // 100 > 62.5, so no reallocation
        assert!(adb.resize(80, 40));
        assert_eq!(adb.width(), 80);
        assert_eq!(adb.height(), 40);
        assert_eq!(adb.stats().resize_avoided, 1);
        assert_eq!(adb.stats().resize_reallocated, 0);
        assert_eq!(adb.stats().resize_shrink, 1);
    }

    #[test]
    fn adaptive_buffer_no_change_returns_false() {
        let mut adb = AdaptiveDoubleBuffer::new(80, 24);

        assert!(!adb.resize(80, 24)); // No change
        assert_eq!(adb.stats().resize_avoided, 0);
        assert_eq!(adb.stats().resize_reallocated, 0);
        assert_eq!(adb.stats().resize_growth, 0);
        assert_eq!(adb.stats().resize_shrink, 0);
    }

    #[test]
    fn adaptive_buffer_swap_works() {
        let mut adb = AdaptiveDoubleBuffer::new(10, 5);

        adb.current_mut().set(0, 0, Cell::from_char('A'));
        assert_eq!(
            adb.current().get(0, 0).unwrap().content.as_char(),
            Some('A')
        );

        adb.swap();
        assert_eq!(
            adb.previous().get(0, 0).unwrap().content.as_char(),
            Some('A')
        );
        assert!(adb.current().get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn adaptive_buffer_stats_reset() {
        let mut adb = AdaptiveDoubleBuffer::new(80, 24);

        adb.resize(90, 28);
        adb.resize(120, 40);
        assert!(adb.stats().resize_avoided > 0 || adb.stats().resize_reallocated > 0);

        adb.reset_stats();
        assert_eq!(adb.stats().resize_avoided, 0);
        assert_eq!(adb.stats().resize_reallocated, 0);
        assert_eq!(adb.stats().resize_growth, 0);
        assert_eq!(adb.stats().resize_shrink, 0);
    }

    #[test]
    fn adaptive_buffer_memory_efficiency() {
        let adb = AdaptiveDoubleBuffer::new(80, 24);

        let efficiency = adb.memory_efficiency();
        // 80*24 = 1920 logical cells
        // 100*30 = 3000 capacity cells
        // efficiency = 1920/3000 = 0.64
        assert!(efficiency > 0.5);
        assert!(efficiency < 1.0);
    }

    #[test]
    fn adaptive_buffer_logical_bounds() {
        let adb = AdaptiveDoubleBuffer::new(80, 24);

        let bounds = adb.logical_bounds();
        assert_eq!(bounds.x, 0);
        assert_eq!(bounds.y, 0);
        assert_eq!(bounds.width, 80);
        assert_eq!(bounds.height, 24);
    }

    #[test]
    fn adaptive_buffer_capacity_clamped_for_large_sizes() {
        // Test that over-allocation is capped at ADAPTIVE_MAX_OVERAGE (200)
        let adb = AdaptiveDoubleBuffer::new(1000, 500);

        // 1000 * 0.25 = 250, capped to 200
        // 500 * 0.25 = 125, not capped
        assert_eq!(adb.capacity_width(), 1000 + 200); // capped
        assert_eq!(adb.capacity_height(), 500 + 125); // not capped
    }

    #[test]
    fn adaptive_stats_avoidance_ratio() {
        let mut stats = AdaptiveStats::default();

        // Empty stats should return 1.0 (perfect avoidance)
        assert!((stats.avoidance_ratio() - 1.0).abs() < f64::EPSILON);

        // 3 avoided, 1 reallocated = 75% avoidance
        stats.resize_avoided = 3;
        stats.resize_reallocated = 1;
        assert!((stats.avoidance_ratio() - 0.75).abs() < f64::EPSILON);

        // All reallocations = 0% avoidance
        stats.resize_avoided = 0;
        stats.resize_reallocated = 5;
        assert!((stats.avoidance_ratio() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn adaptive_buffer_resize_storm_simulation() {
        // Simulate a resize storm (rapid size changes)
        let mut adb = AdaptiveDoubleBuffer::new(80, 24);

        // Simulate user resizing terminal in small increments
        for i in 1..=10 {
            adb.resize(80 + i, 24 + (i / 2));
        }

        // Most resizes should have avoided reallocation due to over-allocation
        let ratio = adb.stats().avoidance_ratio();
        assert!(
            ratio > 0.5,
            "Expected >50% avoidance ratio, got {:.2}",
            ratio
        );
    }

    #[test]
    fn adaptive_buffer_width_only_growth() {
        let mut adb = AdaptiveDoubleBuffer::new(80, 24);

        // Grow only width, within capacity
        assert!(adb.resize(95, 24)); // 95 < 100 capacity
        assert_eq!(adb.stats().resize_avoided, 1);
        assert_eq!(adb.stats().resize_growth, 1);
    }

    #[test]
    fn adaptive_buffer_height_only_growth() {
        let mut adb = AdaptiveDoubleBuffer::new(80, 24);

        // Grow only height, within capacity
        assert!(adb.resize(80, 28)); // 28 < 30 capacity
        assert_eq!(adb.stats().resize_avoided, 1);
        assert_eq!(adb.stats().resize_growth, 1);
    }

    #[test]
    fn adaptive_buffer_one_dimension_exceeds_capacity() {
        let mut adb = AdaptiveDoubleBuffer::new(80, 24);

        // One dimension exceeds capacity, should reallocate
        assert!(adb.resize(105, 24)); // 105 > 100 capacity, 24 < 30
        assert_eq!(adb.stats().resize_reallocated, 1);
    }

    #[test]
    fn adaptive_buffer_current_and_previous_distinct() {
        let mut adb = AdaptiveDoubleBuffer::new(10, 5);
        adb.current_mut().set(0, 0, Cell::from_char('X'));

        // Previous should not reflect changes to current
        assert!(adb.previous().get(0, 0).unwrap().is_empty());
        assert_eq!(
            adb.current().get(0, 0).unwrap().content.as_char(),
            Some('X')
        );
    }

    #[test]
    fn adaptive_buffer_resize_within_capacity_clears_previous() {
        let mut adb = AdaptiveDoubleBuffer::new(10, 5);
        adb.current_mut().set(9, 4, Cell::from_char('X'));
        adb.swap();

        // Shrink within capacity (no reallocation expected)
        assert!(adb.resize(8, 4));

        // Previous buffer should be cleared to avoid stale content outside bounds.
        assert!(adb.previous().get(9, 4).unwrap().is_empty());
    }

    // Property tests for AdaptiveDoubleBuffer invariants
    #[test]
    fn adaptive_buffer_invariant_capacity_geq_logical() {
        // Test across various sizes that capacity always >= logical
        for width in [1u16, 10, 80, 200, 1000, 5000] {
            for height in [1u16, 10, 24, 100, 500, 2000] {
                let adb = AdaptiveDoubleBuffer::new(width, height);
                assert!(
                    adb.capacity_width() >= adb.width(),
                    "capacity_width {} < logical_width {} for ({}, {})",
                    adb.capacity_width(),
                    adb.width(),
                    width,
                    height
                );
                assert!(
                    adb.capacity_height() >= adb.height(),
                    "capacity_height {} < logical_height {} for ({}, {})",
                    adb.capacity_height(),
                    adb.height(),
                    width,
                    height
                );
            }
        }
    }

    #[test]
    fn adaptive_buffer_invariant_resize_dimensions_correct() {
        let mut adb = AdaptiveDoubleBuffer::new(80, 24);

        // After any resize, logical dimensions should match requested
        let test_sizes = [
            (100, 50),
            (40, 20),
            (80, 24),
            (200, 100),
            (10, 5),
            (1000, 500),
        ];
        for (w, h) in test_sizes {
            adb.resize(w, h);
            assert_eq!(adb.width(), w, "width mismatch for ({}, {})", w, h);
            assert_eq!(adb.height(), h, "height mismatch for ({}, {})", w, h);
            assert!(
                adb.capacity_width() >= w,
                "capacity_width < width for ({}, {})",
                w,
                h
            );
            assert!(
                adb.capacity_height() >= h,
                "capacity_height < height for ({}, {})",
                w,
                h
            );
        }
    }

    // Property test: no-ghosting on shrink
    // When buffer shrinks without reallocation, the current buffer is cleared
    // to prevent stale content from appearing in the visible area.
    #[test]
    fn adaptive_buffer_no_ghosting_on_shrink() {
        let mut adb = AdaptiveDoubleBuffer::new(80, 24);

        // Fill the entire logical area with content
        for y in 0..adb.height() {
            for x in 0..adb.width() {
                adb.current_mut().set(x, y, Cell::from_char('X'));
            }
        }

        // Shrink to a smaller size (still above 50% threshold, so no reallocation)
        // 80 * 0.5 = 40, so 60 > 40 means no reallocation
        adb.resize(60, 20);

        // Verify current buffer is cleared after shrink (no stale 'X' visible)
        // The current buffer should be empty because resize() calls clear()
        for y in 0..adb.height() {
            for x in 0..adb.width() {
                let cell = adb.current().get(x, y).unwrap();
                assert!(
                    cell.is_empty(),
                    "Ghost content at ({}, {}): expected empty, got {:?}",
                    x,
                    y,
                    cell.content
                );
            }
        }
    }

    // Property test: shrink-reallocation clears all content
    // When buffer shrinks below threshold (requiring reallocation), both buffers
    // should be fresh/empty.
    #[test]
    fn adaptive_buffer_no_ghosting_on_reallocation_shrink() {
        let mut adb = AdaptiveDoubleBuffer::new(100, 50);

        // Fill both buffers with content
        for y in 0..adb.height() {
            for x in 0..adb.width() {
                adb.current_mut().set(x, y, Cell::from_char('A'));
            }
        }
        adb.swap();
        for y in 0..adb.height() {
            for x in 0..adb.width() {
                adb.current_mut().set(x, y, Cell::from_char('B'));
            }
        }

        // Shrink below 50% threshold, forcing reallocation
        adb.resize(30, 15);
        assert_eq!(adb.stats().resize_reallocated, 1);

        // Both buffers should be fresh/empty
        for y in 0..adb.height() {
            for x in 0..adb.width() {
                assert!(
                    adb.current().get(x, y).unwrap().is_empty(),
                    "Ghost in current at ({}, {})",
                    x,
                    y
                );
                assert!(
                    adb.previous().get(x, y).unwrap().is_empty(),
                    "Ghost in previous at ({}, {})",
                    x,
                    y
                );
            }
        }
    }

    // Property test: growth preserves no-ghosting guarantee
    // When buffer grows beyond capacity (requiring reallocation), the new
    // capacity area should be empty.
    #[test]
    fn adaptive_buffer_no_ghosting_on_growth_reallocation() {
        let mut adb = AdaptiveDoubleBuffer::new(80, 24);

        // Fill current buffer
        for y in 0..adb.height() {
            for x in 0..adb.width() {
                adb.current_mut().set(x, y, Cell::from_char('Z'));
            }
        }

        // Grow beyond capacity (100, 30) to force reallocation
        adb.resize(150, 60);
        assert_eq!(adb.stats().resize_reallocated, 1);

        // Entire new buffer should be empty
        for y in 0..adb.height() {
            for x in 0..adb.width() {
                assert!(
                    adb.current().get(x, y).unwrap().is_empty(),
                    "Ghost at ({}, {}) after growth reallocation",
                    x,
                    y
                );
            }
        }
    }

    // Property test: idempotence - same resize is no-op
    #[test]
    fn adaptive_buffer_resize_idempotent() {
        let mut adb = AdaptiveDoubleBuffer::new(80, 24);
        adb.current_mut().set(5, 5, Cell::from_char('K'));

        // Resize to same dimensions should be no-op
        let changed = adb.resize(80, 24);
        assert!(!changed);

        // Content should be preserved
        assert_eq!(
            adb.current().get(5, 5).unwrap().content.as_char(),
            Some('K')
        );
    }
}
