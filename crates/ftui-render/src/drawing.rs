#![forbid(unsafe_code)]

//! Drawing primitives for the buffer.
//!
//! Provides ergonomic, well-tested helpers on top of `Buffer::set()` so
//! widgets can draw borders, lines, text, and filled regions without
//! duplicating low-level cell loops.
//!
//! All operations respect the buffer's scissor stack (clipping) and
//! opacity stack automatically via `Buffer::set()`.

use crate::buffer::Buffer;
use crate::cell::{Cell, CellContent};
use ftui_core::geometry::Rect;

/// Characters used to draw a border around a rectangle.
///
/// This is a render-level type that holds raw characters.
/// Higher-level crates (e.g. ftui-widgets) provide presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BorderChars {
    pub top_left: char,
    pub top_right: char,
    pub bottom_left: char,
    pub bottom_right: char,
    pub horizontal: char,
    pub vertical: char,
}

impl BorderChars {
    /// Simple box-drawing characters (U+250x).
    pub const SQUARE: Self = Self {
        top_left: '┌',
        top_right: '┐',
        bottom_left: '└',
        bottom_right: '┘',
        horizontal: '─',
        vertical: '│',
    };

    /// Rounded corners.
    pub const ROUNDED: Self = Self {
        top_left: '╭',
        top_right: '╮',
        bottom_left: '╰',
        bottom_right: '╯',
        horizontal: '─',
        vertical: '│',
    };

    /// Double-line border.
    pub const DOUBLE: Self = Self {
        top_left: '╔',
        top_right: '╗',
        bottom_left: '╚',
        bottom_right: '╝',
        horizontal: '═',
        vertical: '║',
    };

    /// Heavy (thick) border.
    pub const HEAVY: Self = Self {
        top_left: '┏',
        top_right: '┓',
        bottom_left: '┗',
        bottom_right: '┛',
        horizontal: '━',
        vertical: '┃',
    };

    /// ASCII-only border.
    pub const ASCII: Self = Self {
        top_left: '+',
        top_right: '+',
        bottom_left: '+',
        bottom_right: '+',
        horizontal: '-',
        vertical: '|',
    };
}

/// Extension trait for drawing on a Buffer.
pub trait Draw {
    /// Draw a horizontal line of cells.
    fn draw_horizontal_line(&mut self, x: u16, y: u16, width: u16, cell: Cell);

    /// Draw a vertical line of cells.
    fn draw_vertical_line(&mut self, x: u16, y: u16, height: u16, cell: Cell);

    /// Draw a filled rectangle.
    fn draw_rect_filled(&mut self, rect: Rect, cell: Cell);

    /// Draw a rectangle outline using a single cell character.
    fn draw_rect_outline(&mut self, rect: Rect, cell: Cell);

    /// Print text at the given coordinates using the cell's colors/attrs.
    ///
    /// Characters replace the cell content; fg/bg/attrs come from `base_cell`.
    /// Stops at the buffer edge. Returns the x position after the last character.
    fn print_text(&mut self, x: u16, y: u16, text: &str, base_cell: Cell) -> u16;

    /// Print text with a right-side clipping boundary.
    ///
    /// Like `print_text` but stops at `max_x` (exclusive) instead of the
    /// buffer edge. Returns the x position after the last character.
    fn print_text_clipped(
        &mut self,
        x: u16,
        y: u16,
        text: &str,
        base_cell: Cell,
        max_x: u16,
    ) -> u16;

    /// Draw a border around a rectangle using the given characters.
    ///
    /// The border is drawn inside the rectangle (edges + corners).
    /// The cell's fg/bg/attrs are applied to all border characters.
    fn draw_border(&mut self, rect: Rect, chars: BorderChars, base_cell: Cell);

    /// Draw a border and fill the interior.
    ///
    /// Draws a border using `border_chars` and fills the interior with
    /// `fill_cell`. If the rect is too small for an interior (width or
    /// height <= 2), only the border is drawn.
    fn draw_box(&mut self, rect: Rect, chars: BorderChars, border_cell: Cell, fill_cell: Cell);

    /// Set all cells in a rectangular area to the given fg/bg/attrs without
    /// changing cell content.
    ///
    /// Useful for painting backgrounds or selection highlights.
    fn paint_area(
        &mut self,
        rect: Rect,
        fg: Option<crate::cell::PackedRgba>,
        bg: Option<crate::cell::PackedRgba>,
    );
}

impl Draw for Buffer {
    fn draw_horizontal_line(&mut self, x: u16, y: u16, width: u16, cell: Cell) {
        for i in 0..width {
            self.set(x.saturating_add(i), y, cell);
        }
    }

    fn draw_vertical_line(&mut self, x: u16, y: u16, height: u16, cell: Cell) {
        for i in 0..height {
            self.set(x, y.saturating_add(i), cell);
        }
    }

    fn draw_rect_filled(&mut self, rect: Rect, cell: Cell) {
        self.fill(rect, cell);
    }

    fn draw_rect_outline(&mut self, rect: Rect, cell: Cell) {
        if rect.is_empty() {
            return;
        }

        // Top
        self.draw_horizontal_line(rect.x, rect.y, rect.width, cell);

        // Bottom
        if rect.height > 1 {
            self.draw_horizontal_line(rect.x, rect.bottom() - 1, rect.width, cell);
        }

        // Left (excluding corners)
        if rect.height > 2 {
            self.draw_vertical_line(rect.x, rect.y + 1, rect.height - 2, cell);
        }

        // Right (excluding corners)
        if rect.width > 1 && rect.height > 2 {
            self.draw_vertical_line(rect.right() - 1, rect.y + 1, rect.height - 2, cell);
        }
    }

    fn print_text(&mut self, x: u16, y: u16, text: &str, base_cell: Cell) -> u16 {
        self.print_text_clipped(x, y, text, base_cell, self.width())
    }

    fn print_text_clipped(
        &mut self,
        x: u16,
        y: u16,
        text: &str,
        base_cell: Cell,
        max_x: u16,
    ) -> u16 {
        use unicode_width::UnicodeWidthChar;

        let mut cx = x;
        for c in text.chars() {
            let width = UnicodeWidthChar::width(c).unwrap_or(0);
            if width == 0 {
                continue;
            }

            if cx >= max_x {
                break;
            }

            // Don't start a wide char if it won't fit
            if cx + width as u16 > max_x {
                break;
            }

            let cell = Cell {
                content: CellContent::from_char(c),
                fg: base_cell.fg,
                bg: base_cell.bg,
                attrs: base_cell.attrs,
            };
            self.set(cx, y, cell);
            cx = cx.saturating_add(width as u16);
        }
        cx
    }

    fn draw_border(&mut self, rect: Rect, chars: BorderChars, base_cell: Cell) {
        if rect.is_empty() {
            return;
        }

        let make_cell = |c: char| -> Cell {
            Cell {
                content: CellContent::from_char(c),
                fg: base_cell.fg,
                bg: base_cell.bg,
                attrs: base_cell.attrs,
            }
        };

        let h_cell = make_cell(chars.horizontal);
        let v_cell = make_cell(chars.vertical);

        // Top edge
        for x in rect.left()..rect.right() {
            self.set(x, rect.top(), h_cell);
        }

        // Bottom edge
        if rect.height > 1 {
            for x in rect.left()..rect.right() {
                self.set(x, rect.bottom() - 1, h_cell);
            }
        }

        // Left edge (excluding corners)
        if rect.height > 2 {
            for y in (rect.top() + 1)..(rect.bottom() - 1) {
                self.set(rect.left(), y, v_cell);
            }
        }

        // Right edge (excluding corners)
        if rect.width > 1 && rect.height > 2 {
            for y in (rect.top() + 1)..(rect.bottom() - 1) {
                self.set(rect.right() - 1, y, v_cell);
            }
        }

        // Corners (drawn last to overwrite edge chars at corners)
        self.set(rect.left(), rect.top(), make_cell(chars.top_left));

        if rect.width > 1 {
            self.set(rect.right() - 1, rect.top(), make_cell(chars.top_right));
        }

        if rect.height > 1 {
            self.set(rect.left(), rect.bottom() - 1, make_cell(chars.bottom_left));
        }

        if rect.width > 1 && rect.height > 1 {
            self.set(
                rect.right() - 1,
                rect.bottom() - 1,
                make_cell(chars.bottom_right),
            );
        }
    }

    fn draw_box(&mut self, rect: Rect, chars: BorderChars, border_cell: Cell, fill_cell: Cell) {
        if rect.is_empty() {
            return;
        }

        // Fill interior first
        if rect.width > 2 && rect.height > 2 {
            let inner = Rect::new(rect.x + 1, rect.y + 1, rect.width - 2, rect.height - 2);
            self.fill(inner, fill_cell);
        }

        // Draw border on top
        self.draw_border(rect, chars, border_cell);
    }

    fn paint_area(
        &mut self,
        rect: Rect,
        fg: Option<crate::cell::PackedRgba>,
        bg: Option<crate::cell::PackedRgba>,
    ) {
        for y in rect.y..rect.bottom() {
            for x in rect.x..rect.right() {
                if let Some(cell) = self.get_mut(x, y) {
                    if let Some(fg_color) = fg {
                        cell.fg = fg_color;
                    }
                    if let Some(bg_color) = bg {
                        cell.bg = bg_color;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::PackedRgba;

    // --- Helper ---

    fn char_at(buf: &Buffer, x: u16, y: u16) -> Option<char> {
        buf.get(x, y).and_then(|c| {
            if c.is_empty() {
                None
            } else {
                c.content.as_char()
            }
        })
    }

    // --- Horizontal line ---

    #[test]
    fn horizontal_line_basic() {
        let mut buf = Buffer::new(10, 1);
        let cell = Cell::from_char('─');
        buf.draw_horizontal_line(2, 0, 5, cell);
        assert_eq!(char_at(&buf, 1, 0), None);
        assert_eq!(char_at(&buf, 2, 0), Some('─'));
        assert_eq!(char_at(&buf, 6, 0), Some('─'));
        assert_eq!(char_at(&buf, 7, 0), None);
    }

    #[test]
    fn horizontal_line_zero_width() {
        let mut buf = Buffer::new(10, 1);
        buf.draw_horizontal_line(0, 0, 0, Cell::from_char('x'));
        // Nothing should be written
        assert!(buf.get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn horizontal_line_clipped_by_scissor() {
        let mut buf = Buffer::new(10, 1);
        buf.push_scissor(Rect::new(0, 0, 3, 1));
        buf.draw_horizontal_line(0, 0, 10, Cell::from_char('x'));
        assert_eq!(char_at(&buf, 0, 0), Some('x'));
        assert_eq!(char_at(&buf, 2, 0), Some('x'));
        // Outside scissor: not written (still empty)
        assert!(buf.get(3, 0).unwrap().is_empty());
    }

    // --- Vertical line ---

    #[test]
    fn vertical_line_basic() {
        let mut buf = Buffer::new(1, 10);
        let cell = Cell::from_char('│');
        buf.draw_vertical_line(0, 1, 4, cell);
        assert!(buf.get(0, 0).unwrap().is_empty());
        assert_eq!(char_at(&buf, 0, 1), Some('│'));
        assert_eq!(char_at(&buf, 0, 4), Some('│'));
        assert!(buf.get(0, 5).unwrap().is_empty());
    }

    #[test]
    fn vertical_line_zero_height() {
        let mut buf = Buffer::new(1, 10);
        buf.draw_vertical_line(0, 0, 0, Cell::from_char('x'));
        assert!(buf.get(0, 0).unwrap().is_empty());
    }

    // --- Rect filled ---

    #[test]
    fn rect_filled() {
        let mut buf = Buffer::new(5, 5);
        let cell = Cell::from_char('█');
        buf.draw_rect_filled(Rect::new(1, 1, 3, 3), cell);
        // Inside
        assert_eq!(char_at(&buf, 1, 1), Some('█'));
        assert_eq!(char_at(&buf, 3, 3), Some('█'));
        // Outside
        assert!(buf.get(0, 0).unwrap().is_empty());
        assert!(buf.get(4, 4).unwrap().is_empty());
    }

    #[test]
    fn rect_filled_empty() {
        let mut buf = Buffer::new(5, 5);
        buf.draw_rect_filled(Rect::new(0, 0, 0, 0), Cell::from_char('x'));
        assert!(buf.get(0, 0).unwrap().is_empty());
    }

    // --- Rect outline ---

    #[test]
    fn rect_outline_basic() {
        let mut buf = Buffer::new(5, 5);
        let cell = Cell::from_char('#');
        buf.draw_rect_outline(Rect::new(0, 0, 5, 5), cell);

        // Corners
        assert_eq!(char_at(&buf, 0, 0), Some('#'));
        assert_eq!(char_at(&buf, 4, 0), Some('#'));
        assert_eq!(char_at(&buf, 0, 4), Some('#'));
        assert_eq!(char_at(&buf, 4, 4), Some('#'));

        // Edges
        assert_eq!(char_at(&buf, 2, 0), Some('#'));
        assert_eq!(char_at(&buf, 0, 2), Some('#'));

        // Interior is empty
        assert!(buf.get(2, 2).unwrap().is_empty());
    }

    #[test]
    fn rect_outline_1x1() {
        let mut buf = Buffer::new(5, 5);
        buf.draw_rect_outline(Rect::new(1, 1, 1, 1), Cell::from_char('o'));
        assert_eq!(char_at(&buf, 1, 1), Some('o'));
    }

    #[test]
    fn rect_outline_2x2() {
        let mut buf = Buffer::new(5, 5);
        buf.draw_rect_outline(Rect::new(0, 0, 2, 2), Cell::from_char('#'));
        assert_eq!(char_at(&buf, 0, 0), Some('#'));
        assert_eq!(char_at(&buf, 1, 0), Some('#'));
        assert_eq!(char_at(&buf, 0, 1), Some('#'));
        assert_eq!(char_at(&buf, 1, 1), Some('#'));
    }

    // --- Print text ---

    #[test]
    fn print_text_basic() {
        let mut buf = Buffer::new(20, 1);
        let cell = Cell::from_char(' '); // base cell, content overridden
        let end_x = buf.print_text(2, 0, "Hello", cell);
        assert_eq!(char_at(&buf, 2, 0), Some('H'));
        assert_eq!(char_at(&buf, 3, 0), Some('e'));
        assert_eq!(char_at(&buf, 6, 0), Some('o'));
        assert_eq!(end_x, 7);
    }

    #[test]
    fn print_text_preserves_style() {
        let mut buf = Buffer::new(10, 1);
        let cell = Cell::from_char(' ')
            .with_fg(PackedRgba::rgb(255, 0, 0))
            .with_bg(PackedRgba::rgb(0, 0, 255));
        buf.print_text(0, 0, "AB", cell);
        let a = buf.get(0, 0).unwrap();
        assert_eq!(a.fg, PackedRgba::rgb(255, 0, 0));
        assert_eq!(a.bg, PackedRgba::rgb(0, 0, 255));
    }

    #[test]
    fn print_text_clips_at_buffer_edge() {
        let mut buf = Buffer::new(5, 1);
        let end_x = buf.print_text(0, 0, "Hello World", Cell::from_char(' '));
        assert_eq!(char_at(&buf, 4, 0), Some('o'));
        assert_eq!(end_x, 5);
    }

    #[test]
    fn print_text_clipped_stops_at_max_x() {
        let mut buf = Buffer::new(20, 1);
        let end_x = buf.print_text_clipped(0, 0, "Hello World", Cell::from_char(' '), 5);
        assert_eq!(char_at(&buf, 4, 0), Some('o'));
        assert_eq!(end_x, 5);
        // Beyond max_x not written
        assert!(buf.get(5, 0).unwrap().is_empty());
    }

    #[test]
    fn print_text_wide_chars() {
        let mut buf = Buffer::new(10, 1);
        let end_x = buf.print_text(0, 0, "AB", Cell::from_char(' '));
        // A=1w, B=1w
        assert_eq!(end_x, 2);
        assert_eq!(char_at(&buf, 0, 0), Some('A'));
        assert_eq!(char_at(&buf, 1, 0), Some('B'));
    }

    #[test]
    fn print_text_wide_char_clipped() {
        let mut buf = Buffer::new(10, 1);
        // Wide char '中' (width=2) at position 4 with max_x=5 won't fit
        let end_x = buf.print_text_clipped(4, 0, "中", Cell::from_char(' '), 5);
        // Can't fit: 4 + 2 > 5
        assert_eq!(end_x, 4);
    }

    #[test]
    fn print_text_empty_string() {
        let mut buf = Buffer::new(10, 1);
        let end_x = buf.print_text(0, 0, "", Cell::from_char(' '));
        assert_eq!(end_x, 0);
    }

    // --- Border drawing ---

    #[test]
    fn draw_border_square() {
        let mut buf = Buffer::new(5, 3);
        buf.draw_border(
            Rect::new(0, 0, 5, 3),
            BorderChars::SQUARE,
            Cell::from_char(' '),
        );

        // Corners
        assert_eq!(char_at(&buf, 0, 0), Some('┌'));
        assert_eq!(char_at(&buf, 4, 0), Some('┐'));
        assert_eq!(char_at(&buf, 0, 2), Some('└'));
        assert_eq!(char_at(&buf, 4, 2), Some('┘'));

        // Horizontal edges
        assert_eq!(char_at(&buf, 1, 0), Some('─'));
        assert_eq!(char_at(&buf, 2, 0), Some('─'));
        assert_eq!(char_at(&buf, 3, 0), Some('─'));

        // Vertical edges
        assert_eq!(char_at(&buf, 0, 1), Some('│'));
        assert_eq!(char_at(&buf, 4, 1), Some('│'));

        // Interior empty
        assert!(buf.get(2, 1).unwrap().is_empty());
    }

    #[test]
    fn draw_border_rounded() {
        let mut buf = Buffer::new(4, 3);
        buf.draw_border(
            Rect::new(0, 0, 4, 3),
            BorderChars::ROUNDED,
            Cell::from_char(' '),
        );
        assert_eq!(char_at(&buf, 0, 0), Some('╭'));
        assert_eq!(char_at(&buf, 3, 0), Some('╮'));
        assert_eq!(char_at(&buf, 0, 2), Some('╰'));
        assert_eq!(char_at(&buf, 3, 2), Some('╯'));
    }

    #[test]
    fn draw_border_1x1() {
        let mut buf = Buffer::new(5, 5);
        buf.draw_border(
            Rect::new(1, 1, 1, 1),
            BorderChars::SQUARE,
            Cell::from_char(' '),
        );
        // Only top-left corner drawn (since width=1, height=1)
        assert_eq!(char_at(&buf, 1, 1), Some('┌'));
    }

    #[test]
    fn draw_border_2x2() {
        let mut buf = Buffer::new(5, 5);
        buf.draw_border(
            Rect::new(0, 0, 2, 2),
            BorderChars::SQUARE,
            Cell::from_char(' '),
        );
        assert_eq!(char_at(&buf, 0, 0), Some('┌'));
        assert_eq!(char_at(&buf, 1, 0), Some('┐'));
        assert_eq!(char_at(&buf, 0, 1), Some('└'));
        assert_eq!(char_at(&buf, 1, 1), Some('┘'));
    }

    #[test]
    fn draw_border_empty_rect() {
        let mut buf = Buffer::new(5, 5);
        buf.draw_border(
            Rect::new(0, 0, 0, 0),
            BorderChars::SQUARE,
            Cell::from_char(' '),
        );
        // Nothing drawn
        assert!(buf.get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn draw_border_preserves_style() {
        let mut buf = Buffer::new(5, 3);
        let cell = Cell::from_char(' ')
            .with_fg(PackedRgba::rgb(0, 255, 0))
            .with_bg(PackedRgba::rgb(0, 0, 128));
        buf.draw_border(Rect::new(0, 0, 5, 3), BorderChars::SQUARE, cell);

        let corner = buf.get(0, 0).unwrap();
        assert_eq!(corner.fg, PackedRgba::rgb(0, 255, 0));
        assert_eq!(corner.bg, PackedRgba::rgb(0, 0, 128));

        let edge = buf.get(2, 0).unwrap();
        assert_eq!(edge.fg, PackedRgba::rgb(0, 255, 0));
    }

    #[test]
    fn draw_border_clipped_by_scissor() {
        let mut buf = Buffer::new(10, 5);
        buf.push_scissor(Rect::new(0, 0, 3, 3));
        buf.draw_border(
            Rect::new(0, 0, 6, 4),
            BorderChars::SQUARE,
            Cell::from_char(' '),
        );

        // Inside scissor: drawn
        assert_eq!(char_at(&buf, 0, 0), Some('┌'));
        assert_eq!(char_at(&buf, 2, 0), Some('─'));

        // Outside scissor: not drawn
        assert!(buf.get(5, 0).unwrap().is_empty());
        assert!(buf.get(0, 3).unwrap().is_empty());
    }

    // --- Draw box ---

    #[test]
    fn draw_box_basic() {
        let mut buf = Buffer::new(5, 4);
        let border = Cell::from_char(' ').with_fg(PackedRgba::rgb(255, 255, 255));
        let fill = Cell::from_char('.').with_bg(PackedRgba::rgb(50, 50, 50));
        buf.draw_box(Rect::new(0, 0, 5, 4), BorderChars::SQUARE, border, fill);

        // Border
        assert_eq!(char_at(&buf, 0, 0), Some('┌'));
        assert_eq!(char_at(&buf, 4, 3), Some('┘'));

        // Interior filled
        assert_eq!(char_at(&buf, 1, 1), Some('.'));
        assert_eq!(char_at(&buf, 3, 2), Some('.'));
        assert_eq!(buf.get(2, 1).unwrap().bg, PackedRgba::rgb(50, 50, 50));
    }

    #[test]
    fn draw_box_too_small_for_interior() {
        let mut buf = Buffer::new(5, 5);
        let border = Cell::from_char(' ');
        let fill = Cell::from_char('X');
        buf.draw_box(Rect::new(0, 0, 2, 2), BorderChars::SQUARE, border, fill);

        // Only border, no fill (width=2, height=2 → interior is 0x0)
        assert_eq!(char_at(&buf, 0, 0), Some('┌'));
        assert_eq!(char_at(&buf, 1, 0), Some('┐'));
    }

    #[test]
    fn draw_box_empty() {
        let mut buf = Buffer::new(5, 5);
        buf.draw_box(
            Rect::new(0, 0, 0, 0),
            BorderChars::SQUARE,
            Cell::from_char(' '),
            Cell::from_char('.'),
        );
        assert!(buf.get(0, 0).unwrap().is_empty());
    }

    // --- Paint area ---

    #[test]
    fn paint_area_sets_colors() {
        let mut buf = Buffer::new(5, 3);
        // Pre-fill with content
        buf.set(1, 1, Cell::from_char('X'));
        buf.set(2, 1, Cell::from_char('Y'));

        buf.paint_area(
            Rect::new(0, 0, 5, 3),
            None,
            Some(PackedRgba::rgb(30, 30, 30)),
        );

        // Content preserved
        assert_eq!(char_at(&buf, 1, 1), Some('X'));
        // Background changed
        assert_eq!(buf.get(1, 1).unwrap().bg, PackedRgba::rgb(30, 30, 30));
        assert_eq!(buf.get(0, 0).unwrap().bg, PackedRgba::rgb(30, 30, 30));
    }

    #[test]
    fn paint_area_sets_fg() {
        let mut buf = Buffer::new(3, 1);
        buf.set(0, 0, Cell::from_char('A'));

        buf.paint_area(
            Rect::new(0, 0, 3, 1),
            Some(PackedRgba::rgb(200, 100, 50)),
            None,
        );

        assert_eq!(buf.get(0, 0).unwrap().fg, PackedRgba::rgb(200, 100, 50));
    }

    #[test]
    fn paint_area_empty_rect() {
        let mut buf = Buffer::new(5, 5);
        buf.set(0, 0, Cell::from_char('A'));
        let original_fg = buf.get(0, 0).unwrap().fg;

        buf.paint_area(
            Rect::new(0, 0, 0, 0),
            Some(PackedRgba::rgb(255, 0, 0)),
            None,
        );

        // Nothing changed
        assert_eq!(buf.get(0, 0).unwrap().fg, original_fg);
    }

    // --- All border presets compile ---

    #[test]
    fn all_border_presets() {
        let mut buf = Buffer::new(6, 4);
        let cell = Cell::from_char(' ');
        let rect = Rect::new(0, 0, 6, 4);

        for chars in [
            BorderChars::SQUARE,
            BorderChars::ROUNDED,
            BorderChars::DOUBLE,
            BorderChars::HEAVY,
            BorderChars::ASCII,
        ] {
            buf.clear();
            buf.draw_border(rect, chars, cell);
            // Corners should be set
            assert!(buf.get(0, 0).unwrap().content.as_char().is_some());
            assert!(buf.get(5, 3).unwrap().content.as_char().is_some());
        }
    }

    // --- Wider integration tests ---

    #[test]
    fn draw_border_then_print_title() {
        let mut buf = Buffer::new(12, 3);
        let cell = Cell::from_char(' ');

        // Draw border
        buf.draw_border(Rect::new(0, 0, 12, 3), BorderChars::SQUARE, cell);

        // Print title inside top edge
        buf.print_text(1, 0, "Title", cell);

        assert_eq!(char_at(&buf, 0, 0), Some('┌'));
        assert_eq!(char_at(&buf, 1, 0), Some('T'));
        assert_eq!(char_at(&buf, 5, 0), Some('e'));
        assert_eq!(char_at(&buf, 6, 0), Some('─'));
        assert_eq!(char_at(&buf, 11, 0), Some('┐'));
    }

    #[test]
    fn draw_nested_borders() {
        let mut buf = Buffer::new(10, 6);
        let cell = Cell::from_char(' ');

        buf.draw_border(Rect::new(0, 0, 10, 6), BorderChars::DOUBLE, cell);
        buf.draw_border(Rect::new(1, 1, 8, 4), BorderChars::SQUARE, cell);

        // Outer corners
        assert_eq!(char_at(&buf, 0, 0), Some('╔'));
        assert_eq!(char_at(&buf, 9, 5), Some('╝'));

        // Inner corners
        assert_eq!(char_at(&buf, 1, 1), Some('┌'));
        assert_eq!(char_at(&buf, 8, 4), Some('┘'));
    }
}
