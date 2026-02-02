//! Border styling primitives.

/// Border characters for drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BorderSet {
    /// Vertical border character.
    pub vertical: char,
    /// Horizontal border character.
    pub horizontal: char,
    /// Top-left corner character.
    pub top_left: char,
    /// Top-right corner character.
    pub top_right: char,
    /// Bottom-left corner character.
    pub bottom_left: char,
    /// Bottom-right corner character.
    pub bottom_right: char,
    /// Upward tee junction character.
    pub tee_up: char,
    /// Downward tee junction character.
    pub tee_down: char,
    /// Leftward tee junction character.
    pub tee_left: char,
    /// Rightward tee junction character.
    pub tee_right: char,
    /// Cross junction character.
    pub cross: char,
}

impl BorderSet {
    /// ASCII fallback border (+, -, |).
    pub const ASCII: Self = Self {
        vertical: '|',
        horizontal: '-',
        top_left: '+',
        top_right: '+',
        bottom_left: '+',
        bottom_right: '+',
        tee_up: '+',
        tee_down: '+',
        tee_left: '+',
        tee_right: '+',
        cross: '+',
    };

    /// Rounded corners (╭, ╮, ╯, ╰).
    pub const ROUNDED: Self = Self {
        vertical: '│',
        horizontal: '─',
        top_left: '╭',
        top_right: '╮',
        bottom_left: '╰',
        bottom_right: '╯',
        tee_up: '┴',
        tee_down: '┬',
        tee_left: '┤',
        tee_right: '├',
        cross: '┼',
    };

    /// Square single-line border.
    pub const SQUARE: Self = Self {
        vertical: '│',
        horizontal: '─',
        top_left: '┌',
        top_right: '┐',
        bottom_left: '└',
        bottom_right: '┘',
        tee_up: '┴',
        tee_down: '┬',
        tee_left: '┤',
        tee_right: '├',
        cross: '┼',
    };

    /// Double lines (║, ═).
    pub const DOUBLE: Self = Self {
        vertical: '║',
        horizontal: '═',
        top_left: '╔',
        top_right: '╗',
        bottom_left: '╚',
        bottom_right: '╝',
        tee_up: '╩',
        tee_down: '╦',
        tee_left: '╣',
        tee_right: '╠',
        cross: '╬',
    };

    /// Heavy lines (┃, ━).
    pub const HEAVY: Self = Self {
        vertical: '┃',
        horizontal: '━',
        top_left: '┏',
        top_right: '┓',
        bottom_left: '┗',
        bottom_right: '┛',
        tee_up: '┻',
        tee_down: '┳',
        tee_left: '┫',
        tee_right: '┣',
        cross: '╋',
    };
}

/// Border style presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BorderType {
    /// No border (but space reserved if Borders::ALL is set).
    #[default]
    Square,
    /// ASCII fallback border.
    Ascii,
    /// Single line border with rounded corners.
    Rounded,
    /// Double line border.
    Double,
    /// Heavy line border.
    Heavy,
    // TODO: Custom(BorderSet)
}

impl BorderType {
    /// Convert this border type to its corresponding border character set.
    pub fn to_border_set(&self) -> BorderSet {
        match self {
            BorderType::Square => BorderSet::SQUARE,
            BorderType::Ascii => BorderSet::ASCII,
            BorderType::Rounded => BorderSet::ROUNDED,
            BorderType::Double => BorderSet::DOUBLE,
            BorderType::Heavy => BorderSet::HEAVY,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_ascii_only() {
        let set = BorderSet::ASCII;
        let chars = [
            set.vertical,
            set.horizontal,
            set.top_left,
            set.top_right,
            set.bottom_left,
            set.bottom_right,
            set.tee_up,
            set.tee_down,
            set.tee_left,
            set.tee_right,
            set.cross,
        ];
        assert!(chars.iter().all(|c| c.is_ascii()));
    }

    #[test]
    fn square_has_box_drawing() {
        let set = BorderSet::SQUARE;
        assert_eq!(set.horizontal, '─');
        assert_eq!(set.vertical, '│');
        assert_eq!(set.cross, '┼');
    }

    #[test]
    fn rounded_has_round_corners() {
        let set = BorderSet::ROUNDED;
        assert_eq!(set.top_left, '╭');
        assert_eq!(set.top_right, '╮');
        assert_eq!(set.bottom_left, '╰');
        assert_eq!(set.bottom_right, '╯');
        assert_eq!(set.horizontal, '─');
        assert_eq!(set.vertical, '│');
    }

    #[test]
    fn double_has_double_lines() {
        let set = BorderSet::DOUBLE;
        assert_eq!(set.horizontal, '═');
        assert_eq!(set.vertical, '║');
        assert_eq!(set.top_left, '╔');
        assert_eq!(set.top_right, '╗');
        assert_eq!(set.bottom_left, '╚');
        assert_eq!(set.bottom_right, '╝');
        assert_eq!(set.cross, '╬');
    }

    #[test]
    fn heavy_has_heavy_lines() {
        let set = BorderSet::HEAVY;
        assert_eq!(set.horizontal, '━');
        assert_eq!(set.vertical, '┃');
        assert_eq!(set.top_left, '┏');
        assert_eq!(set.top_right, '┓');
        assert_eq!(set.bottom_left, '┗');
        assert_eq!(set.bottom_right, '┛');
        assert_eq!(set.cross, '╋');
    }

    #[test]
    fn all_border_sets_have_11_fields() {
        for set in [
            BorderSet::ASCII,
            BorderSet::ROUNDED,
            BorderSet::SQUARE,
            BorderSet::DOUBLE,
            BorderSet::HEAVY,
        ] {
            let chars = [
                set.vertical,
                set.horizontal,
                set.top_left,
                set.top_right,
                set.bottom_left,
                set.bottom_right,
                set.tee_up,
                set.tee_down,
                set.tee_left,
                set.tee_right,
                set.cross,
            ];
            assert_eq!(chars.len(), 11);
            // horizontal and vertical are distinct in all sets
            assert_ne!(set.horizontal, set.vertical);
        }
    }

    #[test]
    fn box_drawing_sets_have_distinct_corners() {
        // Non-ASCII sets use unique box drawing chars for each corner
        for set in [
            BorderSet::ROUNDED,
            BorderSet::SQUARE,
            BorderSet::DOUBLE,
            BorderSet::HEAVY,
        ] {
            let corners = [
                set.top_left,
                set.top_right,
                set.bottom_left,
                set.bottom_right,
            ];
            for (i, a) in corners.iter().enumerate() {
                for (j, b) in corners.iter().enumerate() {
                    if i != j {
                        assert_ne!(a, b, "corners {i} and {j} should differ");
                    }
                }
            }
        }
    }

    #[test]
    fn ascii_set_reuses_plus_for_junctions() {
        let set = BorderSet::ASCII;
        // ASCII uses '+' for all corners, tees, and cross
        assert_eq!(set.top_left, '+');
        assert_eq!(set.top_right, '+');
        assert_eq!(set.bottom_left, '+');
        assert_eq!(set.bottom_right, '+');
        assert_eq!(set.tee_up, '+');
        assert_eq!(set.tee_down, '+');
        assert_eq!(set.tee_left, '+');
        assert_eq!(set.tee_right, '+');
        assert_eq!(set.cross, '+');
    }

    #[test]
    fn border_type_to_border_set_roundtrip() {
        assert_eq!(BorderType::Square.to_border_set(), BorderSet::SQUARE);
        assert_eq!(BorderType::Ascii.to_border_set(), BorderSet::ASCII);
        assert_eq!(BorderType::Rounded.to_border_set(), BorderSet::ROUNDED);
        assert_eq!(BorderType::Double.to_border_set(), BorderSet::DOUBLE);
        assert_eq!(BorderType::Heavy.to_border_set(), BorderSet::HEAVY);
    }

    #[test]
    fn border_type_default_is_square() {
        assert_eq!(BorderType::default(), BorderType::Square);
    }

    #[test]
    fn borders_none_is_zero() {
        assert!(Borders::NONE.is_empty());
        assert_eq!(Borders::NONE.bits(), 0);
    }

    #[test]
    fn borders_all_contains_all_sides() {
        assert!(Borders::ALL.contains(Borders::TOP));
        assert!(Borders::ALL.contains(Borders::RIGHT));
        assert!(Borders::ALL.contains(Borders::BOTTOM));
        assert!(Borders::ALL.contains(Borders::LEFT));
    }

    #[test]
    fn borders_individual_bits_are_distinct() {
        let sides = [Borders::TOP, Borders::RIGHT, Borders::BOTTOM, Borders::LEFT];
        for (i, a) in sides.iter().enumerate() {
            for (j, b) in sides.iter().enumerate() {
                if i != j {
                    assert!(!a.contains(*b), "side {i} should not contain side {j}");
                }
            }
        }
    }

    #[test]
    fn borders_union_and_intersection() {
        let top_left = Borders::TOP | Borders::LEFT;
        assert!(top_left.contains(Borders::TOP));
        assert!(top_left.contains(Borders::LEFT));
        assert!(!top_left.contains(Borders::RIGHT));

        let top_right = Borders::TOP | Borders::RIGHT;
        let intersection = top_left & top_right;
        assert!(intersection.contains(Borders::TOP));
        assert!(!intersection.contains(Borders::LEFT));
        assert!(!intersection.contains(Borders::RIGHT));
    }

    #[test]
    fn borders_default_is_none() {
        assert_eq!(Borders::default(), Borders::NONE);
    }

    #[test]
    fn non_ascii_sets_have_no_ascii_chars() {
        for set in [
            BorderSet::ROUNDED,
            BorderSet::SQUARE,
            BorderSet::DOUBLE,
            BorderSet::HEAVY,
        ] {
            let chars = [
                set.vertical,
                set.horizontal,
                set.top_left,
                set.top_right,
                set.bottom_left,
                set.bottom_right,
                set.tee_up,
                set.tee_down,
                set.tee_left,
                set.tee_right,
                set.cross,
            ];
            assert!(
                chars.iter().all(|c| !c.is_ascii()),
                "non-ASCII border set should have no ASCII chars"
            );
        }
    }

    #[test]
    fn border_set_tees_are_consistent() {
        // For SQUARE: tees should share chars with edges
        let set = BorderSet::SQUARE;
        // tee_up (┴) connects vertical and horizontal lines going up
        // tee_down (┬) connects going down
        // They should all be distinct from each other and from corners
        let tees = [set.tee_up, set.tee_down, set.tee_left, set.tee_right];
        for (i, a) in tees.iter().enumerate() {
            for (j, b) in tees.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "tees {i} and {j} should differ");
                }
            }
        }
    }
}

bitflags::bitflags! {
    /// Bitflags for which borders to render.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Borders: u8 {
        /// No borders.
        const NONE   = 0b0000;
        /// Top border.
        const TOP    = 0b0001;
        /// Right border.
        const RIGHT  = 0b0010;
        /// Bottom border.
        const BOTTOM = 0b0100;
        /// Left border.
        const LEFT   = 0b1000;
        /// All four borders.
        const ALL    = Self::TOP.bits() | Self::RIGHT.bits() | Self::BOTTOM.bits() | Self::LEFT.bits();
    }
}
