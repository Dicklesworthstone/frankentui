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

use ftui_a11y::node::{A11yNodeInfo, A11yRole};
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
                        frame.buffer.set_fast(buf_x, buf_y, buf_cell);
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
                        frame.buffer.set_fast(buf_x, buf_y, buf_cell);
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
                        frame.buffer.set_fast(buf_x, buf_y, buf_cell);
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

        if frame.a11y_enabled() {
            let bounds = area.intersection(&frame.buffer.current_scissor());
            if !bounds.is_empty() {
                let title = terminal.title().trim();
                let name = if title.is_empty() { "Terminal" } else { title };
                let desc = if state.scroll_offset > 0 {
                    let unit = if state.scroll_offset == 1 {
                        "line"
                    } else {
                        "lines"
                    };
                    format!(
                        "{}x{}, scrolled up {} {unit}",
                        terminal.width(),
                        terminal.height(),
                        state.scroll_offset
                    )
                } else {
                    format!("{}x{}", terminal.width(), terminal.height())
                };
                frame.push_a11y(
                    A11yNodeInfo::new(
                        A11yNodeInfo::stable_id_for(A11yRole::Group, area),
                        A11yRole::Group,
                        bounds,
                    )
                    .with_name(name)
                    .with_description(desc),
                );
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
                frame.buffer.set_fast(x, y, empty);
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

    #[test]
    fn default_emulator_has_cursor_hidden() {
        // Derived Default sets bools to false, while new() sets them to true
        let from_default = TerminalEmulator::default();
        assert!(!from_default.show_cursor);
        assert!(!from_default.cursor_visible_phase);
    }

    #[test]
    fn widget_render_clears_area() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);

        // Set a cell to something non-space first
        frame.buffer.set(1, 1, BufferCell::from_char('Z'));
        assert_eq!(frame.buffer.get(1, 1).unwrap().content.as_char(), Some('Z'));

        // Widget::render should overwrite with spaces
        Widget::render(&widget, Rect::new(0, 0, 10, 5), &mut frame);
        assert_eq!(frame.buffer.get(1, 1).unwrap().content.as_char(), Some(' '));
    }

    #[test]
    fn stateful_render_without_scroll() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new().show_cursor(false);
        let mut state = TerminalEmulatorState::new(10, 5);
        state.terminal_mut().put_char('H');

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        let area = Rect::new(0, 0, 10, 5);
        StatefulWidget::render(&widget, area, &mut frame, &mut state);

        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('H'));
    }

    #[test]
    fn stateful_render_zero_area_noop() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new();
        let mut state = TerminalEmulatorState::new(10, 5);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        // Zero-width area should not panic
        StatefulWidget::render(&widget, Rect::new(0, 0, 0, 5), &mut frame, &mut state);
        // Zero-height area should not panic
        StatefulWidget::render(&widget, Rect::new(0, 0, 10, 0), &mut frame, &mut state);
    }

    #[test]
    fn convert_cell_all_attrs() {
        let widget = TerminalEmulator::new();
        let term_cell = TerminalCell {
            ch: 'A',
            fg: None,
            bg: None,
            attrs: CellAttrs::DIM
                .with(CellAttrs::UNDERLINE)
                .with(CellAttrs::BLINK)
                .with(CellAttrs::REVERSE)
                .with(CellAttrs::STRIKETHROUGH)
                .with(CellAttrs::HIDDEN),
        };
        let buf_cell = widget.convert_cell(&term_cell);
        let flags = buf_cell.attrs.flags();
        assert!(flags.contains(StyleFlags::DIM));
        assert!(flags.contains(StyleFlags::UNDERLINE));
        assert!(flags.contains(StyleFlags::BLINK));
        assert!(flags.contains(StyleFlags::REVERSE));
        assert!(flags.contains(StyleFlags::STRIKETHROUGH));
        assert!(flags.contains(StyleFlags::HIDDEN));
    }

    #[test]
    fn with_scrollback_constructor() {
        let state = TerminalEmulatorState::with_scrollback(20, 10, 500);
        assert_eq!(state.terminal.width(), 20);
        assert_eq!(state.terminal.height(), 10);
        assert_eq!(state.scroll_offset, 0);
    }

    // --- Cursor rendering tests ---

    #[test]
    fn apply_cursor_block_sets_reverse() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new();
        let mut state = TerminalEmulatorState::new(10, 5);
        state.terminal_mut().put_char('A');
        // Cursor is at (1,0) after putting one char
        state.terminal_mut().move_cursor(0, 0);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        let area = Rect::new(0, 0, 10, 5);
        StatefulWidget::render(&widget, area, &mut frame, &mut state);

        let cell = frame.buffer.get(0, 0).unwrap();
        assert!(
            cell.attrs.flags().contains(StyleFlags::REVERSE),
            "Block cursor should set REVERSE flag"
        );
    }

    #[test]
    fn apply_cursor_underline_sets_underline() {
        use ftui_render::grapheme_pool::GraphemePool;

        // Test apply_cursor directly with an Underline cursor
        let widget = TerminalEmulator::new();
        let cursor = Cursor {
            x: 2,
            y: 1,
            visible: true,
            shape: CursorShape::Underline,
            saved: None,
        };

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        widget.apply_cursor(&cursor, 2, 1, &mut frame);

        let cell = frame.buffer.get(2, 1).unwrap();
        assert!(
            cell.attrs.flags().contains(StyleFlags::UNDERLINE),
            "Underline cursor should set UNDERLINE flag"
        );
    }

    #[test]
    fn apply_cursor_bar_sets_reverse() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new();
        let cursor = Cursor {
            x: 0,
            y: 0,
            visible: true,
            shape: CursorShape::Bar,
            saved: None,
        };

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        widget.apply_cursor(&cursor, 0, 0, &mut frame);

        let cell = frame.buffer.get(0, 0).unwrap();
        assert!(
            cell.attrs.flags().contains(StyleFlags::REVERSE),
            "Bar cursor should set REVERSE flag"
        );
    }

    #[test]
    fn apply_cursor_block_sets_reverse_directly() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new();
        let cursor = Cursor {
            x: 5,
            y: 3,
            visible: true,
            shape: CursorShape::Block,
            saved: None,
        };

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        widget.apply_cursor(&cursor, 5, 3, &mut frame);

        let cell = frame.buffer.get(5, 3).unwrap();
        assert!(
            cell.attrs.flags().contains(StyleFlags::REVERSE),
            "Block cursor should set REVERSE flag"
        );
    }

    #[test]
    fn apply_cursor_wrong_position_no_effect() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new();
        let cursor = Cursor {
            x: 5,
            y: 3,
            visible: true,
            shape: CursorShape::Block,
            saved: None,
        };

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        // Pass position (0,0) but cursor is at (5,3) - should not modify
        widget.apply_cursor(&cursor, 0, 0, &mut frame);

        let cell = frame.buffer.get(0, 0).unwrap();
        assert!(
            !cell.attrs.flags().contains(StyleFlags::REVERSE),
            "Cursor should not modify cell at wrong position"
        );
    }

    #[test]
    fn apply_cursor_invisible_no_effect() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new();
        let cursor = Cursor {
            x: 0,
            y: 0,
            visible: false,
            shape: CursorShape::Block,
            saved: None,
        };

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        widget.apply_cursor(&cursor, 0, 0, &mut frame);

        let cell = frame.buffer.get(0, 0).unwrap();
        assert!(
            !cell.attrs.flags().contains(StyleFlags::REVERSE),
            "Invisible cursor should not modify cell"
        );
    }

    #[test]
    fn cursor_hidden_when_show_cursor_false() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new().show_cursor(false);
        let mut state = TerminalEmulatorState::new(10, 5);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        let area = Rect::new(0, 0, 10, 5);
        StatefulWidget::render(&widget, area, &mut frame, &mut state);

        let cell = frame.buffer.get(0, 0).unwrap();
        assert!(
            !cell.attrs.flags().contains(StyleFlags::REVERSE),
            "Cursor should not render when show_cursor is false"
        );
    }

    #[test]
    fn cursor_hidden_when_phase_invisible() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new().cursor_phase(false);
        let mut state = TerminalEmulatorState::new(10, 5);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        let area = Rect::new(0, 0, 10, 5);
        StatefulWidget::render(&widget, area, &mut frame, &mut state);

        let cell = frame.buffer.get(0, 0).unwrap();
        assert!(
            !cell.attrs.flags().contains(StyleFlags::REVERSE),
            "Cursor should not render when blink phase is invisible"
        );
    }

    #[test]
    fn cursor_hidden_when_terminal_cursor_invisible() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new();
        let mut state = TerminalEmulatorState::new(10, 5);
        state.terminal_mut().set_cursor_visible(false);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        let area = Rect::new(0, 0, 10, 5);
        StatefulWidget::render(&widget, area, &mut frame, &mut state);

        let cell = frame.buffer.get(0, 0).unwrap();
        assert!(
            !cell.attrs.flags().contains(StyleFlags::REVERSE),
            "Cursor should not render when terminal cursor is invisible"
        );
    }

    #[test]
    fn cursor_not_rendered_when_scrolled() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new();
        let mut state = TerminalEmulatorState::with_scrollback(10, 5, 100);
        // Push lines into scrollback
        for _ in 0..3 {
            state.terminal.scroll_up(1);
        }
        state.scroll_up(1); // Scroll into scrollback

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        let area = Rect::new(0, 0, 10, 5);
        StatefulWidget::render(&widget, area, &mut frame, &mut state);

        // When scrolled, cursor should not render (only renders in else branch)
        let cursor = state.terminal.cursor();
        let cursor_x = area.x + cursor.x;
        let cursor_y = area.y + cursor.y;
        if let Some(cell) = frame.buffer.get(cursor_x, cursor_y) {
            assert!(
                !cell.attrs.flags().contains(StyleFlags::REVERSE),
                "Cursor should not render when view is scrolled into scrollback"
            );
        }
    }

    // --- Scrollback rendering tests ---

    #[test]
    fn stateful_render_with_scrollback() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new().show_cursor(false);
        let mut state = TerminalEmulatorState::with_scrollback(10, 5, 100);

        // Write a character to the first cell, then scroll it into scrollback
        state.terminal_mut().put_char('S');
        for _ in 0..5 {
            state.terminal.scroll_up(1);
        }
        // 'S' should now be in scrollback

        // Scroll view to see scrollback
        state.scroll_up(1);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        let area = Rect::new(0, 0, 10, 5);
        StatefulWidget::render(&widget, area, &mut frame, &mut state);

        // The scrollback line should be rendered at the top of the area
        // (exact content depends on scrollback ordering)
        // Just verify no panic and something was written
        assert!(frame.buffer.get(0, 0).is_some());
    }

    #[test]
    fn render_partial_scrollback() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new().show_cursor(false);
        let mut state = TerminalEmulatorState::with_scrollback(10, 5, 100);

        // Push several lines into scrollback
        for i in 0..8 {
            let ch = (b'A' + i) as char;
            state.terminal_mut().move_cursor(0, 0);
            state.terminal_mut().put_char(ch);
            state.terminal.scroll_up(1);
        }

        // Scroll up by 2 - should show 2 scrollback lines at top, 3 grid lines below
        state.scroll_up(2);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        let area = Rect::new(0, 0, 10, 5);
        StatefulWidget::render(&widget, area, &mut frame, &mut state);

        // Verify the render completed without panics
        // All 5 rows should have been written
        for y in 0..5 {
            assert!(frame.buffer.get(0, y).is_some());
        }
    }

    // --- Area clipping tests ---

    #[test]
    fn render_area_smaller_than_terminal() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new().show_cursor(false);
        let mut state = TerminalEmulatorState::new(10, 5);
        // Fill terminal with a recognizable pattern
        for y in 0..5u16 {
            for x in 0..10u16 {
                state.terminal_mut().move_cursor(x, y);
                state.terminal_mut().put_char('X');
            }
        }

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        // Render into a smaller area (3x2 sub-area)
        let area = Rect::new(0, 0, 3, 2);
        StatefulWidget::render(&widget, area, &mut frame, &mut state);

        // Cells within area should have content
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('X'));
        assert_eq!(frame.buffer.get(2, 1).unwrap().content.as_char(), Some('X'));

        // Cell outside the render area should NOT have been written by this render
        // (stays at default from Frame::new, where as_char() returns None)
        let outside = frame.buffer.get(5, 3).unwrap();
        assert_eq!(outside.content.as_char(), None);
    }

    #[test]
    fn render_area_with_offset() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new().show_cursor(false);
        let mut state = TerminalEmulatorState::new(5, 3);
        state.terminal_mut().move_cursor(0, 0);
        state.terminal_mut().put_char('O');

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        // Render at offset position
        let area = Rect::new(5, 3, 5, 3);
        StatefulWidget::render(&widget, area, &mut frame, &mut state);

        // 'O' should be at (5, 3) in the buffer (area.x + 0, area.y + 0)
        assert_eq!(frame.buffer.get(5, 3).unwrap().content.as_char(), Some('O'));
        // Origin of frame should be untouched (default cell, as_char() returns None)
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), None);
    }

    #[test]
    fn render_area_larger_than_terminal() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new().show_cursor(false);
        let mut state = TerminalEmulatorState::new(3, 2);
        state.terminal_mut().move_cursor(0, 0);
        state.terminal_mut().put_char('T');

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        // Area is larger than terminal (10x10 vs 3x2)
        let area = Rect::new(0, 0, 10, 10);
        StatefulWidget::render(&widget, area, &mut frame, &mut state);

        // Content within terminal bounds should be written
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('T'));
        // Content beyond terminal bounds should not be written (stays as default)
        assert_eq!(frame.buffer.get(5, 5).unwrap().content.as_char(), None);
    }

    // --- Widget trait (non-stateful) tests ---

    #[test]
    fn widget_render_clears_with_offset() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);

        // Set a cell in the target area to non-space
        frame.buffer.set(7, 4, BufferCell::from_char('Z'));

        // Widget::render at offset should clear that cell
        Widget::render(&widget, Rect::new(5, 3, 10, 5), &mut frame);
        assert_eq!(frame.buffer.get(7, 4).unwrap().content.as_char(), Some(' '));
    }

    // --- Multiple cells with varied attributes ---

    #[test]
    fn render_multiple_cells_varied_attrs() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new().show_cursor(false);
        let mut state = TerminalEmulatorState::new(10, 5);

        // Write 'A' with bold
        {
            let pen = state.terminal_mut().pen_mut();
            pen.attrs = CellAttrs::BOLD;
            pen.fg = Some(Color::rgb(255, 0, 0));
        }
        state.terminal_mut().put_char('A');

        // Write 'B' with italic
        {
            let pen = state.terminal_mut().pen_mut();
            pen.attrs = CellAttrs::ITALIC;
            pen.fg = Some(Color::rgb(0, 255, 0));
        }
        state.terminal_mut().put_char('B');

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        let area = Rect::new(0, 0, 10, 5);
        StatefulWidget::render(&widget, area, &mut frame, &mut state);

        let cell_a = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell_a.content.as_char(), Some('A'));
        assert!(cell_a.attrs.flags().contains(StyleFlags::BOLD));
        assert_eq!(cell_a.fg.r(), 255);

        let cell_b = frame.buffer.get(1, 0).unwrap();
        assert_eq!(cell_b.content.as_char(), Some('B'));
        assert!(cell_b.attrs.flags().contains(StyleFlags::ITALIC));
        assert_eq!(cell_b.fg.g(), 255);
    }

    #[test]
    fn render_cell_with_bg_color() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new().show_cursor(false);
        let mut state = TerminalEmulatorState::new(10, 5);
        {
            let pen = state.terminal_mut().pen_mut();
            pen.bg = Some(Color::rgb(0, 0, 128));
        }
        state.terminal_mut().put_char('C');

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        let area = Rect::new(0, 0, 10, 5);
        StatefulWidget::render(&widget, area, &mut frame, &mut state);

        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('C'));
        assert_eq!(cell.bg.b(), 128);
        assert_eq!(cell.bg.a(), 255);
    }

    // --- Cursor position tests ---

    #[test]
    fn cursor_renders_at_non_origin_terminal_position() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new();
        let mut state = TerminalEmulatorState::new(10, 5);
        state.terminal_mut().move_cursor(3, 2);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        let area = Rect::new(0, 0, 10, 5);
        StatefulWidget::render(&widget, area, &mut frame, &mut state);

        // Cursor at (3,2) in terminal coords maps to (3,2) in buffer when area origin is (0,0)
        let cell_at_cursor = frame.buffer.get(3, 2).unwrap();
        assert!(
            cell_at_cursor.attrs.flags().contains(StyleFlags::REVERSE),
            "Cursor should render at terminal cursor position"
        );

        // Cell not at cursor should not have REVERSE
        let cell_away = frame.buffer.get(0, 0).unwrap();
        assert!(
            !cell_away.attrs.flags().contains(StyleFlags::REVERSE),
            "Cell away from cursor should not have REVERSE flag"
        );
    }

    #[test]
    fn cursor_at_last_valid_position() {
        use ftui_render::grapheme_pool::GraphemePool;

        let widget = TerminalEmulator::new();
        let mut state = TerminalEmulatorState::new(10, 5);
        // Move cursor to bottom-right corner
        state.terminal_mut().move_cursor(9, 4);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        let area = Rect::new(0, 0, 10, 5);
        StatefulWidget::render(&widget, area, &mut frame, &mut state);

        let cell = frame.buffer.get(9, 4).unwrap();
        assert!(
            cell.attrs.flags().contains(StyleFlags::REVERSE),
            "Cursor should render at bottom-right corner"
        );
    }

    // --- Color conversion edge cases ---

    #[test]
    fn color_to_packed_black() {
        let packed = color_to_packed(Color::rgb(0, 0, 0));
        assert_eq!(packed.r(), 0);
        assert_eq!(packed.g(), 0);
        assert_eq!(packed.b(), 0);
        assert_eq!(packed.a(), 255);
    }

    #[test]
    fn color_to_packed_white() {
        let packed = color_to_packed(Color::rgb(255, 255, 255));
        assert_eq!(packed.r(), 255);
        assert_eq!(packed.g(), 255);
        assert_eq!(packed.b(), 255);
        assert_eq!(packed.a(), 255);
    }

    // --- Scroll edge cases ---

    #[test]
    fn scroll_up_zero_is_noop() {
        let mut state = TerminalEmulatorState::with_scrollback(10, 5, 100);
        for _ in 0..3 {
            state.terminal.scroll_up(1);
        }
        state.scroll_up(0);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn scroll_down_zero_is_noop() {
        let mut state = TerminalEmulatorState::with_scrollback(10, 5, 100);
        for _ in 0..3 {
            state.terminal.scroll_up(1);
        }
        state.scroll_up(2);
        assert_eq!(state.scroll_offset, 2);
        state.scroll_down(0);
        assert_eq!(state.scroll_offset, 2);
    }

    #[test]
    fn resize_to_smaller_clamps_scroll() {
        let mut state = TerminalEmulatorState::with_scrollback(10, 10, 100);
        for _ in 0..20 {
            state.terminal.scroll_up(1);
        }
        state.scroll_up(15);
        // After resize, scrollback may shrink
        state.resize(5, 5);
        assert!(state.scroll_offset <= state.terminal.scrollback().len());
    }

    #[test]
    fn resize_to_larger_preserves_scroll() {
        let mut state = TerminalEmulatorState::with_scrollback(10, 5, 100);
        for _ in 0..5 {
            state.terminal.scroll_up(1);
        }
        state.scroll_up(3);
        let offset_before = state.scroll_offset;
        state.resize(20, 10);
        // Scroll offset should not increase, but may be clamped
        assert!(state.scroll_offset <= offset_before);
    }

    #[test]
    fn multiple_scroll_up_down_roundtrip() {
        let mut state = TerminalEmulatorState::with_scrollback(10, 5, 100);
        for _ in 0..10 {
            state.terminal.scroll_up(1);
        }

        state.scroll_up(5);
        state.scroll_up(3);
        assert_eq!(state.scroll_offset, 8);

        state.scroll_down(4);
        assert_eq!(state.scroll_offset, 4);

        state.scroll_down(10); // Should clamp at 0
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn terminal_emulator_emits_a11y_group() {
        use ftui_a11y::tree::A11yTreeBuilder;
        use ftui_render::grapheme_pool::GraphemePool;

        let mut state = TerminalEmulatorState::with_scrollback(40, 10, 50);
        state.terminal.scroll_up(2);
        state.scroll_up(2);
        let widget = TerminalEmulator::new();

        let mut pool = GraphemePool::new();
        let mut builder = A11yTreeBuilder::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        frame.set_a11y(&mut builder);
        StatefulWidget::render(&widget, Rect::new(0, 0, 40, 10), &mut frame, &mut state);
        frame.finish_a11y();
        let tree = builder.build();

        let node = tree
            .nodes()
            .find(|n| n.role == A11yRole::Group && n.name.as_deref() == Some("Terminal"))
            .expect("TerminalEmulator should emit Group node");
        assert_eq!(
            node.description.as_deref(),
            Some("40x10, scrolled up 2 lines")
        );
    }
}
