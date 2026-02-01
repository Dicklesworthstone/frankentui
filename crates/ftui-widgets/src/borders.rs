//! Border styling primitives.

/// Border characters for drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BorderSet {
    pub vertical: char,
    pub horizontal: char,
    pub top_left: char,
    pub top_right: char,
    pub bottom_left: char,
    pub bottom_right: char,
}

impl BorderSet {
    /// Plain ASCII border (|, -).
    pub const PLAIN: Self = Self {
        vertical: '│',
        horizontal: '─',
        top_left: '┌',
        top_right: '┐',
        bottom_left: '└',
        bottom_right: '┘',
    };

    /// Rounded corners (╭, ╮, ╯, ╰).
    pub const ROUNDED: Self = Self {
        vertical: '│',
        horizontal: '─',
        top_left: '╭',
        top_right: '╮',
        bottom_left: '╰',
        bottom_right: '╯',
    };

    /// Double lines (║, ═).
    pub const DOUBLE: Self = Self {
        vertical: '║',
        horizontal: '═',
        top_left: '╔',
        top_right: '╗',
        bottom_left: '╚',
        bottom_right: '╝',
    };

    /// Thick lines (┃, ━).
    pub const THICK: Self = Self {
        vertical: '┃',
        horizontal: '━',
        top_left: '┏',
        top_right: '┓',
        bottom_left: '┗',
        bottom_right: '┛',
    };
}

/// Border style presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BorderType {
    /// No border (but space reserved if Borders::ALL is set).
    #[default]
    Plain,
    /// Single line border with rounded corners.
    Rounded,
    /// Double line border.
    Double,
    /// Thick line border.
    Thick,
    // TODO: Custom(BorderSet)
}

impl BorderType {
    pub fn to_border_set(&self) -> BorderSet {
        match self {
            BorderType::Plain => BorderSet::PLAIN,
            BorderType::Rounded => BorderSet::ROUNDED,
            BorderType::Double => BorderSet::DOUBLE,
            BorderType::Thick => BorderSet::THICK,
        }
    }
}

bitflags::bitflags! {
    /// Bitflags for which borders to render.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Borders: u8 {
        const NONE   = 0b0000;
        const TOP    = 0b0001;
        const RIGHT  = 0b0010;
        const BOTTOM = 0b0100;
        const LEFT   = 0b1000;
        const ALL    = Self::TOP.bits() | Self::RIGHT.bits() | Self::BOTTOM.bits() | Self::LEFT.bits();
    }
}
