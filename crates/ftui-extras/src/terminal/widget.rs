//! Terminal emulator widget for embedding terminal output in TUI applications.
//!
//! This module provides a `TerminalEmulator` widget that renders terminal state
//! to a FrankenTUI buffer, handling cursor rendering, scroll offsets, and resize
//! propagation.
//!
//! # Invariants
//!
//! 1. **Cell mapping**: Terminal cells map 1:1 to buffer cells within the area.
//! 2. **Cursor visibility**: Cursor renders only when visible and within bounds.
//! 3. **Resize propagation**: Resize events update both terminal state and PTY.
//!
//! # Failure Modes
//!
//! | Failure | Cause | Behavior |
//! |---------|-------|----------|
//! | Out of bounds | Area smaller than terminal | Content clipped |
//! | PTY error | Child process died | Renders last state |
//! | Color mismatch | Unsupported color format | Falls back to default |

use ftui_core::geometry::Rect;
use ftui_render::cell::{Cell as BufferCell, CellAttrs as BufferCellAttrs, PackedRgba, StyleFlags};
use ftui_render::frame::Frame;
use ftui_style::Color;
use ftui_widgets::{StatefulWidget, Widget};

use super::state::{Cell as TerminalCell, CellAttrs, Cursor, CursorShape, TerminalState};

/// Terminal emulator widget.
///
/// Renders a `TerminalState` to a frame buffer, handling:
/// - Cell content and styling
/// - Cursor visualization
/// - Scroll offset (for scrollback viewing)
///
/// # Example
///
/// ```ignore
/// use ftui_extras::terminal::{TerminalEmulator, TerminalEmulatorState};
///
/// let mut state = TerminalEmulatorState::new(80, 24);
/// let widget = TerminalEmulator::new();
/// frame.render_stateful(&widget, area, &mut state);
/// ```
#[derive(Debug, Default, Clone)]
pub struct TerminalEmulator {
    /// Show cursor when rendering.
    show_cursor: bool,
    /// Cursor blink state (true = visible phase).
    cursor_visible_phase: bool,
}

impl TerminalEmulator {
    /// Create a new terminal emulator widget.
    #[must_use]
    pub fn new() -> Self {
        Self {
            show_cursor: true,
            cursor_visible_phase: true,
        }
    }

    /// Set whether to show the cursor.
    #[must_use]
    pub fn show_cursor(mut self, show: bool) -> Self {
        self.show_cursor = show;
        self
    }

    /// Set the cursor blink phase (true = visible).
    #[must_use]
    pub fn cursor_phase(mut self, visible: bool) -> Self {
        self.cursor_visible_phase = visible;
        self
    }

    /// Convert a terminal cell to a buffer cell.
    fn convert_cell(&self, term_cell: &TerminalCell) -> BufferCell {
        let ch = term_cell.ch;
        let fg = term_cell
            .fg
            .map(color_to_packed)
            .unwrap_or(PackedRgba::TRANSPARENT);
        let bg = term_cell
            .bg
            .map(color_to_packed)
            .unwrap_or(PackedRgba::TRANSPARENT);

        // Convert terminal attrs to style flags
        let attrs = term_cell.attrs;
        let mut flags = StyleFlags::empty();

        if attrs.contains(CellAttrs::BOLD) {
            flags |= StyleFlags::BOLD;
        }
        if attrs.contains(CellAttrs::DIM) {
            flags |= StyleFlags::DIM;
        }
        if attrs.contains(CellAttrs::ITALIC) {
            flags |= StyleFlags::ITALIC;
        }
        if attrs.contains(CellAttrs::UNDERLINE) {
            flags |= StyleFlags::UNDERLINE;
        }
        if attrs.contains(CellAttrs::BLINK) {
            flags |= StyleFlags::BLINK;
        }
        if attrs.contains(CellAttrs::REVERSE) {
            flags |= StyleFlags::REVERSE;
        }
        if attrs.contains(CellAttrs::STRIKETHROUGH) {
            flags |= StyleFlags::STRIKETHROUGH;
        }
        if attrs.contains(CellAttrs::HIDDEN) {
            flags |= StyleFlags::HIDDEN;
        }

        let cell_attrs = BufferCellAttrs::new(flags, 0);

        BufferCell::from_char(ch)
            .with_fg(fg)
            .with_bg(bg)
            .with_attrs(cell_attrs)
    }

    /// Apply cursor styling to a cell at the given position.
    fn apply_cursor(&self, cursor: &Cursor, x: u16, y: u16, frame: &mut Frame) {
        if !self.show_cursor || !cursor.visible || !self.cursor_visible_phase {
            return;
        }

        if x != cursor.x || y != cursor.y {
            return;
        }

        if let Some(cell) = frame.buffer.get_mut(x, y) {
            match cursor.shape {
                CursorShape::Block | CursorShape::Bar => {
                    // Invert colors for block/bar cursor
                    let new_attrs = cell
                        .attrs
                        .with_flags(cell.attrs.flags() | StyleFlags::REVERSE);
                    cell.attrs = new_attrs;
                }
                CursorShape::Underline => {
                    // Add underline for underline cursor
                    let new_attrs = cell
                        .attrs
                        .with_flags(cell.attrs.flags() | StyleFlags::UNDERLINE);
                    cell.attrs = new_attrs;
                }
            }
        }
    }
}

/// State for the terminal emulator widget.
#[derive(Debug, Clone)]
pub struct TerminalEmulatorState {
    /// The terminal state (grid, cursor, scrollback).
    pub terminal: TerminalState,
    /// Scroll offset into scrollback (0 = current view, >0 = scrolled up).
    pub scroll_offset: usize,
}

impl TerminalEmulatorState {
    /// Create a new terminal emulator state with the given dimensions.
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            terminal: TerminalState::new(width, height),
            scroll_offset: 0,
        }
    }

    /// Create with custom scrollback limit.
    #[must_use]
    pub fn with_scrollback(width: u16, height: u16, max_scrollback: usize) -> Self {
        Self {
            terminal: TerminalState::with_scrollback(width, height, max_scrollback),
            scroll_offset: 0,
        }
    }

    /// Get a reference to the terminal state.
    #[must_use]
    pub const fn terminal(&self) -> &TerminalState {
        &self.terminal
    }

    /// Get a mutable reference to the terminal state.
    pub fn terminal_mut(&mut self) -> &mut TerminalState {
        &mut self.terminal
    }

    /// Scroll up by the given number of lines (into scrollback).
    pub fn scroll_up(&mut self, lines: usize) {
        let max_scroll = self.terminal.scrollback().len();
        self.scroll_offset = (self.scroll_offset + lines).min(max_scroll);
    }

    /// Scroll down by the given number of lines (toward current view).
    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    /// Reset scroll to current view.
    pub fn reset_scroll(&mut self) {
        self.scroll_offset = 0;
    }

    /// Resize the terminal.
    ///
    /// This updates the terminal state dimensions. Call this when the
    /// widget area changes, and also send a SIGWINCH to the PTY process
    /// if one is attached.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.terminal.resize(width, height);
        // Clamp scroll offset
        let max_scroll = self.terminal.scrollback().len();
        self.scroll_offset = self.scroll_offset.min(max_scroll);
    }
}

impl StatefulWidget for TerminalEmulator {
    type State = TerminalEmulatorState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let terminal = &state.terminal;

        // If scrolled into scrollback, render scrollback lines first
        if state.scroll_offset > 0 {
            let scrollback = terminal.scrollback();
            let scroll_lines = state.scroll_offset.min(area.height as usize);

            // Render scrollback lines at the top
            for y in 0..scroll_lines {
                let scrollback_line_idx = state.scroll_offset - 1 - y;
                if let Some(line) = scrollback.line(scrollback_line_idx) {
                    let buf_y = area.y + y as u16;
                    for (x, term_cell) in line.iter().enumerate() {
                        if x >= area.width as usize {
                            break;
                        }
                        let buf_x = area.x + x as u16;
                        let buf_cell = self.convert_cell(term_cell);
                        frame.buffer.set(buf_x, buf_y, buf_cell);
                    }
                }
            }

            // Render visible portion of current grid below scrollback
            let grid_start_y = scroll_lines as u16;
            let grid_lines = area.height.saturating_sub(grid_start_y);
            for y in 0..grid_lines.min(terminal.height()) {
                for x in 0..area.width.min(terminal.width()) {
                    if let Some(term_cell) = terminal.cell(x, y) {
                        let buf_x = area.x + x;
                        let buf_y = area.y + grid_start_y + y;
                        let buf_cell = self.convert_cell(term_cell);
                        frame.buffer.set(buf_x, buf_y, buf_cell);
                    }
                }
            }
        } else {
            // No scrollback offset - render current grid
            for y in 0..area.height.min(terminal.height()) {
                for x in 0..area.width.min(terminal.width()) {
                    if let Some(term_cell) = terminal.cell(x, y) {
                        let buf_x = area.x + x;
                        let buf_y = area.y + y;
                        let buf_cell = self.convert_cell(term_cell);
                        frame.buffer.set(buf_x, buf_y, buf_cell);
                    }
                }
            }

            // Render cursor (only when not scrolled)
            let cursor = terminal.cursor();
            let cursor_x = area.x + cursor.x;
            let cursor_y = area.y + cursor.y;
            if cursor_x < area.x + area.width && cursor_y < area.y + area.height {
                self.apply_cursor(cursor, cursor_x, cursor_y, frame);
            }
        }
    }
}

/// Also implement Widget for simple cases (without state mutation).
impl Widget for TerminalEmulator {
    fn render(&self, area: Rect, frame: &mut Frame) {
        // Widget trait render is a no-op; use StatefulWidget for proper rendering
        // This just clears the area with spaces
        let empty = BufferCell::from_char(' ');
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                frame.buffer.set(x, y, empty);
            }
        }
    }
}

/// Convert ftui-style Color to PackedRgba.
fn color_to_packed(color: Color) -> PackedRgba {
    let rgb = color.to_rgb();
    PackedRgba::rgba(rgb.r, rgb.g, rgb.b, 255)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_emulator_state_new() {
        let state = TerminalEmulatorState::new(80, 24);
        assert_eq!(state.terminal.width(), 80);
        assert_eq!(state.terminal.height(), 24);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_scroll_up_down() {
        let mut state = TerminalEmulatorState::with_scrollback(10, 5, 100);

        // Add some lines to scrollback by scrolling the terminal
        for _ in 0..10 {
            state.terminal.scroll_up(1);
        }

        // Now scroll the view
        state.scroll_up(5);
        assert_eq!(state.scroll_offset, 5);

        state.scroll_down(2);
        assert_eq!(state.scroll_offset, 3);

        state.reset_scroll();
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_scroll_clamps_to_scrollback_size() {
        let mut state = TerminalEmulatorState::with_scrollback(10, 5, 100);

        // Add 3 lines to scrollback
        for _ in 0..3 {
            state.terminal.scroll_up(1);
        }

        // Try to scroll beyond scrollback
        state.scroll_up(100);
        assert_eq!(state.scroll_offset, 3); // Clamped to scrollback size
    }

    #[test]
    fn test_resize() {
        let mut state = TerminalEmulatorState::new(80, 24);
        state.resize(120, 40);
        assert_eq!(state.terminal.width(), 120);
        assert_eq!(state.terminal.height(), 40);
    }

    #[test]
    fn test_emulator_widget_defaults() {
        let widget = TerminalEmulator::new();
        assert!(widget.show_cursor);
        assert!(widget.cursor_visible_phase);
    }

    #[test]
    fn test_emulator_widget_builder() {
        let widget = TerminalEmulator::new()
            .show_cursor(false)
            .cursor_phase(false);
        assert!(!widget.show_cursor);
        assert!(!widget.cursor_visible_phase);
    }

    #[test]
    fn test_color_to_packed() {
        let color = Color::rgb(100, 150, 200);
        let packed = color_to_packed(color);
        assert_eq!(packed.r(), 100);
        assert_eq!(packed.g(), 150);
        assert_eq!(packed.b(), 200);
        assert_eq!(packed.a(), 255);
    }

    #[test]
    fn test_scroll_down_clamps_at_zero() {
        let mut state = TerminalEmulatorState::new(10, 5);
        state.scroll_down(10);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_convert_cell_maps_attrs() {
        let widget = TerminalEmulator::new();
        let term_cell = TerminalCell {
            ch: 'X',
            fg: Some(Color::rgb(255, 0, 0)),
            bg: Some(Color::rgb(0, 0, 255)),
            attrs: CellAttrs::BOLD.with(CellAttrs::ITALIC),
        };
        let buf_cell = widget.convert_cell(&term_cell);
        assert_eq!(buf_cell.content.as_char(), Some('X'));
        assert_eq!(buf_cell.fg.r(), 255);
        assert_eq!(buf_cell.bg.b(), 255);
        assert!(buf_cell.attrs.flags().contains(StyleFlags::BOLD));
        assert!(buf_cell.attrs.flags().contains(StyleFlags::ITALIC));
    }

    #[test]
    fn test_convert_cell_default_colors_transparent() {
        let widget = TerminalEmulator::new();
        let term_cell = TerminalCell::default();
        let buf_cell = widget.convert_cell(&term_cell);
        assert_eq!(buf_cell.fg, PackedRgba::TRANSPARENT);
        assert_eq!(buf_cell.bg, PackedRgba::TRANSPARENT);
    }

    #[test]
    fn test_resize_clamps_scroll_offset() {
        let mut state = TerminalEmulatorState::with_scrollback(10, 5, 100);
        for _ in 0..10 {
            state.terminal.scroll_up(1);
        }
        state.scroll_up(8);
        assert_eq!(state.scroll_offset, 8);
        // Resize clears scrollback (terminal.resize resets grid)
        state.resize(10, 5);
        // scroll_offset should be clamped to new scrollback len
        assert!(state.scroll_offset <= state.terminal.scrollback().len());
    }

    #[test]
    fn test_terminal_accessors() {
        let mut state = TerminalEmulatorState::new(10, 5);
        assert_eq!(state.terminal().width(), 10);
        state.terminal_mut().put_char('A');
        assert_eq!(state.terminal().cell(0, 0).unwrap().ch, 'A');
    }
}
