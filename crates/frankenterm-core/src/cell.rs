//! Terminal cell: the fundamental unit of the grid.
//!
//! Each cell stores a character (or grapheme cluster) and its SGR attributes.
//! This is intentionally simpler than `ftui-render::Cell` — it models the
//! terminal's internal state rather than the rendering pipeline.

use bitflags::bitflags;

bitflags! {
    /// SGR text attribute flags.
    ///
    /// Maps directly to the ECMA-48 / VT100 SGR parameter values.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct SgrFlags: u16 {
        const BOLD          = 1 << 0;
        const DIM           = 1 << 1;
        const ITALIC        = 1 << 2;
        const UNDERLINE     = 1 << 3;
        const BLINK         = 1 << 4;
        const INVERSE       = 1 << 5;
        const HIDDEN        = 1 << 6;
        const STRIKETHROUGH = 1 << 7;
        const DOUBLE_UNDERLINE = 1 << 8;
        const CURLY_UNDERLINE  = 1 << 9;
        const OVERLINE      = 1 << 10;
    }
}

bitflags! {
    /// Cell-level flags that are orthogonal to SGR attributes.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct CellFlags: u8 {
        /// This cell is the leading (left) cell of a wide (2-column) character.
        const WIDE_CHAR = 1 << 0;
        /// This cell is the trailing (right) continuation of a wide character.
        /// Its content is meaningless; rendering uses the leading cell.
        const WIDE_CONTINUATION = 1 << 1;
    }
}

/// Color representation for terminal cells.
///
/// Supports the standard terminal color model hierarchy:
/// default → 16 named → 256 indexed → 24-bit RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Color {
    /// Terminal default (SGR 39 / SGR 49).
    #[default]
    Default,
    /// Named color index (0-15): standard 8 + bright 8.
    Named(u8),
    /// 256-color palette index (0-255).
    Indexed(u8),
    /// 24-bit true color.
    Rgb(u8, u8, u8),
}

/// SGR attributes for a cell: flags + foreground/background colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SgrAttrs {
    pub flags: SgrFlags,
    pub fg: Color,
    pub bg: Color,
    /// Underline color (SGR 58). `None` means use foreground.
    pub underline_color: Option<Color>,
}

impl SgrAttrs {
    /// Reset all attributes to default (SGR 0).
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Hyperlink identifier for OSC 8 links.
///
/// Zero means "no link". Non-zero values index into an external link registry
/// that maps IDs to URIs.
pub type HyperlinkId = u16;

/// A single cell in the terminal grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// The character content. A space for empty/erased cells.
    content: char,
    /// Display width of the content in terminal columns (1 or 2 for wide chars).
    width: u8,
    /// Cell-level flags (wide char, continuation, etc.).
    pub flags: CellFlags,
    /// SGR text attributes.
    pub attrs: SgrAttrs,
    /// Hyperlink ID (0 = no link).
    pub hyperlink: HyperlinkId,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            content: ' ',
            width: 1,
            flags: CellFlags::empty(),
            attrs: SgrAttrs::default(),
            hyperlink: 0,
        }
    }
}

impl Cell {
    /// Create a new cell with the given character and default attributes.
    pub fn new(ch: char) -> Self {
        Self {
            content: ch,
            width: 1,
            flags: CellFlags::empty(),
            attrs: SgrAttrs::default(),
            hyperlink: 0,
        }
    }

    /// Create a new cell with the given character, width, and attributes.
    pub fn with_attrs(ch: char, width: u8, attrs: SgrAttrs) -> Self {
        Self {
            content: ch,
            width,
            flags: CellFlags::empty(),
            attrs,
            hyperlink: 0,
        }
    }

    /// Create a wide (2-column) character cell.
    ///
    /// Returns `(leading, continuation)` pair. The leading cell holds the
    /// character; the continuation cell is a placeholder.
    pub fn wide(ch: char, attrs: SgrAttrs) -> (Self, Self) {
        let leading = Self {
            content: ch,
            width: 2,
            flags: CellFlags::WIDE_CHAR,
            attrs,
            hyperlink: 0,
        };
        let continuation = Self {
            content: ' ',
            width: 0,
            flags: CellFlags::WIDE_CONTINUATION,
            attrs,
            hyperlink: 0,
        };
        (leading, continuation)
    }

    /// The character content of this cell.
    pub fn content(&self) -> char {
        self.content
    }

    /// The display width in terminal columns.
    pub fn width(&self) -> u8 {
        self.width
    }

    /// Whether this cell is the leading half of a wide character.
    pub fn is_wide(&self) -> bool {
        self.flags.contains(CellFlags::WIDE_CHAR)
    }

    /// Whether this cell is a continuation (trailing half) of a wide character.
    pub fn is_wide_continuation(&self) -> bool {
        self.flags.contains(CellFlags::WIDE_CONTINUATION)
    }

    /// Set the character content and display width.
    pub fn set_content(&mut self, ch: char, width: u8) {
        self.content = ch;
        self.width = width;
        // Clear wide flags when replacing content.
        self.flags
            .remove(CellFlags::WIDE_CHAR | CellFlags::WIDE_CONTINUATION);
    }

    /// Reset this cell to a blank space with the given background attributes.
    ///
    /// Used by erase operations (ED, EL, ECH) which fill with the current
    /// background color but reset all other attributes.
    pub fn erase(&mut self, bg: Color) {
        self.content = ' ';
        self.width = 1;
        self.flags = CellFlags::empty();
        self.attrs = SgrAttrs {
            bg,
            ..SgrAttrs::default()
        };
        self.hyperlink = 0;
    }

    /// Reset this cell to a blank space with default attributes.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_cell_is_space() {
        let cell = Cell::default();
        assert_eq!(cell.content(), ' ');
        assert_eq!(cell.width(), 1);
        assert_eq!(cell.attrs, SgrAttrs::default());
        assert!(!cell.is_wide());
        assert!(!cell.is_wide_continuation());
        assert_eq!(cell.hyperlink, 0);
    }

    #[test]
    fn cell_new_has_default_attrs() {
        let cell = Cell::new('A');
        assert_eq!(cell.content(), 'A');
        assert_eq!(cell.attrs.flags, SgrFlags::empty());
        assert_eq!(cell.attrs.fg, Color::Default);
        assert_eq!(cell.attrs.bg, Color::Default);
    }

    #[test]
    fn cell_erase_clears_content_and_attrs() {
        let mut cell = Cell::with_attrs(
            'X',
            1,
            SgrAttrs {
                flags: SgrFlags::BOLD | SgrFlags::ITALIC,
                fg: Color::Named(1),
                bg: Color::Named(4),
                underline_color: None,
            },
        );
        cell.hyperlink = 42;
        cell.erase(Color::Named(2));
        assert_eq!(cell.content(), ' ');
        assert_eq!(cell.attrs.flags, SgrFlags::empty());
        assert_eq!(cell.attrs.fg, Color::Default);
        assert_eq!(cell.attrs.bg, Color::Named(2));
        assert_eq!(cell.hyperlink, 0);
    }

    #[test]
    fn wide_char_pair() {
        let attrs = SgrAttrs {
            flags: SgrFlags::BOLD,
            ..SgrAttrs::default()
        };
        let (lead, cont) = Cell::wide('\u{4E2D}', attrs); // '中'
        assert!(lead.is_wide());
        assert!(!lead.is_wide_continuation());
        assert_eq!(lead.width(), 2);
        assert_eq!(lead.content(), '中');

        assert!(!cont.is_wide());
        assert!(cont.is_wide_continuation());
        assert_eq!(cont.width(), 0);
    }

    #[test]
    fn set_content_clears_wide_flags() {
        let (mut lead, _) = Cell::wide('中', SgrAttrs::default());
        assert!(lead.is_wide());
        lead.set_content('A', 1);
        assert!(!lead.is_wide());
        assert!(!lead.is_wide_continuation());
    }

    #[test]
    fn erase_clears_wide_flags() {
        let (mut lead, _) = Cell::wide('中', SgrAttrs::default());
        lead.erase(Color::Default);
        assert!(!lead.is_wide());
    }

    #[test]
    fn sgr_attrs_reset() {
        let mut attrs = SgrAttrs {
            flags: SgrFlags::BOLD,
            fg: Color::Rgb(255, 0, 0),
            bg: Color::Indexed(42),
            underline_color: Some(Color::Named(3)),
        };
        attrs.reset();
        assert_eq!(attrs, SgrAttrs::default());
    }

    #[test]
    fn color_default() {
        assert_eq!(Color::default(), Color::Default);
    }

    #[test]
    fn cell_clear_resets_everything() {
        let mut cell = Cell::with_attrs(
            'Z',
            2,
            SgrAttrs {
                flags: SgrFlags::BOLD | SgrFlags::UNDERLINE,
                fg: Color::Rgb(1, 2, 3),
                bg: Color::Named(5),
                underline_color: Some(Color::Indexed(100)),
            },
        );
        cell.hyperlink = 99;
        cell.flags = CellFlags::WIDE_CHAR;
        cell.clear();
        assert_eq!(cell, Cell::default());
    }
}
