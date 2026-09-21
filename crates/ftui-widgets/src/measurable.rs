//! Intrinsic sizing support for widgets.
//!
//! This module provides the [`MeasurableWidget`] trait for widgets that can report
//! their intrinsic dimensions, enabling content-aware layout like `Constraint::FitContent`.
//!
//! # Overview
//!
//! Not all widgets need intrinsic sizing—many simply fill whatever space they're given.
//! But some widgets have natural dimensions based on their content:
//!
//! - A [`Paragraph`](crate::paragraph::Paragraph) knows how wide its text is
//! - A [`Block`](crate::block::Block) knows its minimum border/padding requirements
//! - A [`List`](crate::list::List) knows how many items it contains
//!
//! # Size Constraints
//!
//! [`SizeConstraints`] captures the full sizing semantics:
//!
//! - **min**: Minimum size below which the widget clips or becomes unusable
//! - **preferred**: Size that best displays the content
//! - **max**: Maximum useful size (beyond this, extra space is wasted)
//!
//! # Example
//!
//! ```ignore
//! use ftui_core::geometry::Size;
//! use ftui_widgets::{MeasurableWidget, SizeConstraints, Widget};
//!
//! struct Label {
//!     text: String,
//! }
//!
//! impl MeasurableWidget for Label {
//!     fn measure(&self, _available: Size) -> SizeConstraints {
//!         let width = ftui_text::display_width(self.text.as_str()) as u16;
//!         SizeConstraints {
//!             min: Size::new(1, 1),           // At least show something
//!             preferred: Size::new(width, 1), // Ideal: full text on one line
//!             max: Some(Size::new(width, 1)), // No benefit from extra space
//!         }
//!     }
//!
//!     fn has_intrinsic_size(&self) -> bool {
//!         true // This widget's size depends on content
//!     }
//! }
//! ```
//!
//! # Invariants
//!
//! Implementations must maintain these invariants:
//!
//! 1. `min <= preferred <= max.unwrap_or(∞)` for both width and height
//! 2. `measure()` must be pure: same input → same output
//! 3. `measure()` should be O(content_length) worst case
//!
//! # Backwards Compatibility
//!
//! Widgets that don't implement `MeasurableWidget` explicitly get a default
//! implementation that returns `SizeConstraints::ZERO` and `has_intrinsic_size() = false`,
//! indicating they fill available space.

use ftui_core::geometry::Size;

/// Size constraints returned by measure operations.
///
/// Captures the full sizing semantics for a widget:
/// - **min**: Minimum usable size (content clips below this)
/// - **preferred**: Ideal size for content display
/// - **max**: Maximum useful size (no benefit beyond this)
///
/// # Invariants
///
/// The following must hold:
/// - `min.width <= preferred.width <= max.map_or(u16::MAX, |m| m.width)`
/// - `min.height <= preferred.height <= max.map_or(u16::MAX, |m| m.height)`
///
/// # Example
///
/// ```
/// use ftui_core::geometry::Size;
/// use ftui_widgets::SizeConstraints;
///
/// // A 10x3 text block with some flexibility
/// let constraints = SizeConstraints {
///     min: Size::new(5, 1),       // Can shrink to 5 chars, 1 line
///     preferred: Size::new(10, 3), // Ideal display
///     max: Some(Size::new(20, 5)), // No benefit beyond this
/// };
///
/// // Clamp an allocation to these constraints
/// let allocated = Size::new(8, 2);
/// let clamped = constraints.clamp(allocated);
/// assert_eq!(clamped, Size::new(8, 2)); // Within range, unchanged
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SizeConstraints {
    /// Minimum size below which the widget is unusable or clips content.
    pub min: Size,
    /// Preferred size that best displays content.
    pub preferred: Size,
    /// Maximum useful size. `None` means unbounded (widget can use all available space).
    pub max: Option<Size>,
}

impl SizeConstraints {
    /// Zero constraints (no minimum, no preferred, unbounded maximum).
    ///
    /// This is the default for widgets that fill available space.
    pub const ZERO: Self = Self {
        min: Size::ZERO,
        preferred: Size::ZERO,
        max: None,
    };

    /// Create constraints with exact sizing (min = preferred = max).
    ///
    /// Use this for widgets with a fixed, known size.
    #[inline]
    pub const fn exact(size: Size) -> Self {
        Self {
            min: size,
            preferred: size,
            max: Some(size),
        }
    }

    /// Create constraints with a minimum and preferred size, unbounded maximum.
    #[inline]
    pub const fn at_least(min: Size, preferred: Size) -> Self {
        Self {
            min,
            preferred,
            max: None,
        }
    }

    /// Clamp a given size to these constraints.
    ///
    /// The result will be:
    /// - At least `min.width` x `min.height`
    /// - At most `max.width` x `max.height` (if max is set)
    ///
    /// # Example
    ///
    /// ```
    /// use ftui_core::geometry::Size;
    /// use ftui_widgets::SizeConstraints;
    ///
    /// let c = SizeConstraints {
    ///     min: Size::new(5, 2),
    ///     preferred: Size::new(10, 5),
    ///     max: Some(Size::new(20, 10)),
    /// };
    ///
    /// // Below minimum
    /// assert_eq!(c.clamp(Size::new(3, 1)), Size::new(5, 2));
    ///
    /// // Within range
    /// assert_eq!(c.clamp(Size::new(15, 7)), Size::new(15, 7));
    ///
    /// // Above maximum
    /// assert_eq!(c.clamp(Size::new(30, 20)), Size::new(20, 10));
    /// ```
    pub fn clamp(&self, size: Size) -> Size {
        let max = self.max.unwrap_or(Size::MAX);

        // Use const-compatible clamping
        let width = if size.width < self.min.width {
            self.min.width
        } else if size.width > max.width {
            max.width
        } else {
            size.width
        };

        let height = if size.height < self.min.height {
            self.min.height
        } else if size.height > max.height {
            max.height
        } else {
            size.height
        };

        Size::new(width, height)
    }

    /// Check if these constraints are satisfied by the given size.
    ///
    /// Returns `true` if `size` is within the min/max bounds.
    #[inline]
    pub fn is_satisfied_by(&self, size: Size) -> bool {
        let max = self.max.unwrap_or(Size::MAX);
        size.width >= self.min.width
            && size.height >= self.min.height
            && size.width <= max.width
            && size.height <= max.height
    }

    /// Combine two constraints by taking the maximum minimums and minimum maximums.
    ///
    /// Useful when a widget has multiple children and needs to satisfy all constraints.
    pub fn intersect(&self, other: &SizeConstraints) -> SizeConstraints {
        let min_width = self.min.width.max(other.min.width);
        let min_height = self.min.height.max(other.min.height);

        let max = match (self.max, other.max) {
            (Some(a), Some(b)) => Some(Size::new(a.width.min(b.width), a.height.min(b.height))),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };

        // Preferred is the max of minimums clamped to max
        let preferred_width = self.preferred.width.max(other.preferred.width);
        let preferred_height = self.preferred.height.max(other.preferred.height);
        let preferred = Size::new(preferred_width, preferred_height);

        SizeConstraints {
            min: Size::new(min_width, min_height),
            preferred,
            max,
        }
    }
}

impl Default for SizeConstraints {
    fn default() -> Self {
        Self::ZERO
    }
}

/// A widget that can report its intrinsic dimensions.
///
/// Implement this trait for widgets whose size depends on their content.
/// Widgets that simply fill available space can use the default implementation.
///
/// # Semantics
///
/// - `measure(&self, available)` returns the size constraints given the available space
/// - `has_intrinsic_size()` returns `true` if measure() provides meaningful constraints
///
/// # Invariants
///
/// Implementations must ensure:
///
/// 1. **Monotonicity**: `min <= preferred <= max.unwrap_or(∞)`
/// 2. **Purity**: Same inputs produce identical outputs (no side effects)
/// 3. **Performance**: O(content_length) worst case
///
/// # Example
///
/// ```ignore
/// use ftui_core::geometry::Size;
/// use ftui_widgets::{MeasurableWidget, SizeConstraints};
///
/// struct Icon {
///     glyph: char,
/// }
///
/// impl MeasurableWidget for Icon {
///     fn measure(&self, _available: Size) -> SizeConstraints {
///         // Icons are always 1x1 (or 2x1 for wide chars)
///         let mut buf = [0u8; 4];
///         let glyph = self.glyph.encode_utf8(&mut buf);
///         let width = ftui_text::grapheme_width(glyph) as u16;
///         SizeConstraints::exact(Size::new(width, 1))
///     }
///
///     fn has_intrinsic_size(&self) -> bool {
///         true
///     }
/// }
/// ```
pub trait MeasurableWidget {
    /// Measure the widget given available space.
    ///
    /// # Arguments
    ///
    /// - `available`: Maximum space the widget could occupy. Use this for:
    ///   - Text wrapping calculations (wrap at available.width)
    ///   - Proportional sizing (e.g., "50% of available width")
    ///
    /// # Returns
    ///
    /// [`SizeConstraints`] describing the widget's min/preferred/max sizes.
    ///
    /// # Default Implementation
    ///
    /// Returns `SizeConstraints::ZERO`, indicating the widget fills available space.
    fn measure(&self, available: Size) -> SizeConstraints {
        let _ = available; // Suppress unused warning
        SizeConstraints::ZERO
    }

    /// Quick check: does this widget have content-dependent sizing?
    ///
    /// Widgets returning `false` can skip `measure()` calls when only chrome
    /// (borders, padding) matters. This is a performance optimization.
    ///
    /// # Returns
    ///
    /// - `true`: Widget size depends on content (call `measure()`)
    /// - `false`: Widget fills available space (skip `measure()`)
    ///
    /// # Default Implementation
    ///
    /// Returns `false` for backwards compatibility with existing widgets.
    fn has_intrinsic_size(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- SizeConstraints tests ---

    #[test]
    fn size_constraints_zero_is_default() {
        assert_eq!(SizeConstraints::default(), SizeConstraints::ZERO);
    }

    #[test]
    fn size_constraints_exact() {
        let c = SizeConstraints::exact(Size::new(10, 5));
        assert_eq!(c.min, Size::new(10, 5));
        assert_eq!(c.preferred, Size::new(10, 5));
        assert_eq!(c.max, Some(Size::new(10, 5)));
    }

    #[test]
    fn size_constraints_at_least() {
        let c = SizeConstraints::at_least(Size::new(5, 2), Size::new(10, 4));
        assert_eq!(c.min, Size::new(5, 2));
        assert_eq!(c.preferred, Size::new(10, 4));
        assert_eq!(c.max, None);
    }

    #[test]
    fn size_constraints_clamp_below_min() {
        let c = SizeConstraints {
            min: Size::new(5, 2),
            preferred: Size::new(10, 5),
            max: Some(Size::new(20, 10)),
        };
        assert_eq!(c.clamp(Size::new(3, 1)), Size::new(5, 2));
    }

    #[test]
    fn size_constraints_clamp_in_range() {
        let c = SizeConstraints {
            min: Size::new(5, 2),
            preferred: Size::new(10, 5),
            max: Some(Size::new(20, 10)),
        };
        assert_eq!(c.clamp(Size::new(15, 7)), Size::new(15, 7));
    }

    #[test]
    fn size_constraints_clamp_above_max() {
        let c = SizeConstraints {
            min: Size::new(5, 2),
            preferred: Size::new(10, 5),
            max: Some(Size::new(20, 10)),
        };
        assert_eq!(c.clamp(Size::new(30, 20)), Size::new(20, 10));
    }

    #[test]
    fn size_constraints_clamp_no_max() {
        let c = SizeConstraints {
            min: Size::new(5, 2),
            preferred: Size::new(10, 5),
            max: None,
        };
        // Without max, large values are preserved
        assert_eq!(c.clamp(Size::new(1000, 500)), Size::new(1000, 500));
        // But still clamped to min
        assert_eq!(c.clamp(Size::new(2, 1)), Size::new(5, 2));
    }

    #[test]
    fn size_constraints_is_satisfied_by() {
        let c = SizeConstraints {
            min: Size::new(5, 2),
            preferred: Size::new(10, 5),
            max: Some(Size::new(20, 10)),
        };

        assert!(c.is_satisfied_by(Size::new(10, 5)));
        assert!(c.is_satisfied_by(Size::new(5, 2))); // At min
        assert!(c.is_satisfied_by(Size::new(20, 10))); // At max

        assert!(!c.is_satisfied_by(Size::new(4, 2))); // Below min width
        assert!(!c.is_satisfied_by(Size::new(5, 1))); // Below min height
        assert!(!c.is_satisfied_by(Size::new(21, 10))); // Above max width
        assert!(!c.is_satisfied_by(Size::new(20, 11))); // Above max height
    }

    #[test]
    fn size_constraints_is_satisfied_by_no_max() {
        let c = SizeConstraints {
            min: Size::new(5, 2),
            preferred: Size::new(10, 5),
            max: None,
        };

        assert!(c.is_satisfied_by(Size::new(1000, 500))); // Any large size is fine
        assert!(!c.is_satisfied_by(Size::new(4, 2))); // Still respects min
    }

    #[test]
    fn size_constraints_intersect_both_bounded() {
        let a = SizeConstraints {
            min: Size::new(5, 2),
            preferred: Size::new(10, 5),
            max: Some(Size::new(20, 10)),
        };
        let b = SizeConstraints {
            min: Size::new(8, 3),
            preferred: Size::new(12, 6),
            max: Some(Size::new(15, 8)),
        };
        let c = a.intersect(&b);

        // Min is max of minimums
        assert_eq!(c.min, Size::new(8, 3));
        // Max is min of maximums
        assert_eq!(c.max, Some(Size::new(15, 8)));
        // Preferred is max of preferreds
        assert_eq!(c.preferred, Size::new(12, 6));
    }

    #[test]
    fn size_constraints_intersect_one_unbounded() {
        let bounded = SizeConstraints {
            min: Size::new(5, 2),
            preferred: Size::new(10, 5),
            max: Some(Size::new(20, 10)),
        };
        let unbounded = SizeConstraints {
            min: Size::new(8, 1),
            preferred: Size::new(15, 3),
            max: None,
        };
        let c = bounded.intersect(&unbounded);

        assert_eq!(c.min, Size::new(8, 2)); // Max of mins
        assert_eq!(c.max, Some(Size::new(20, 10))); // Bounded wins
        assert_eq!(c.preferred, Size::new(15, 5)); // Max of preferreds
    }

    #[test]
    fn size_constraints_intersect_both_unbounded() {
        let a = SizeConstraints::at_least(Size::new(5, 2), Size::new(10, 5));
        let b = SizeConstraints::at_least(Size::new(8, 3), Size::new(12, 6));
        let c = a.intersect(&b);

        assert_eq!(c.min, Size::new(8, 3));
        assert_eq!(c.max, None);
        assert_eq!(c.preferred, Size::new(12, 6));
    }

    // --- MeasurableWidget default implementation tests ---

    struct PlainWidget;

    impl MeasurableWidget for PlainWidget {}

    #[test]
    fn default_measure_returns_zero() {
        let widget = PlainWidget;
        assert_eq!(widget.measure(Size::MAX), SizeConstraints::ZERO);
    }

    #[test]
    fn default_has_no_intrinsic_size() {
        let widget = PlainWidget;
        assert!(!widget.has_intrinsic_size());
    }

    // --- Custom implementation tests ---

    struct FixedSizeWidget {
        width: u16,
        height: u16,
    }

    impl MeasurableWidget for FixedSizeWidget {
        fn measure(&self, _available: Size) -> SizeConstraints {
            SizeConstraints::exact(Size::new(self.width, self.height))
        }

        fn has_intrinsic_size(&self) -> bool {
            true
        }
    }

    #[test]
    fn custom_widget_measure() {
        let widget = FixedSizeWidget {
            width: 20,
            height: 5,
        };
        let c = widget.measure(Size::MAX);

        assert_eq!(c.min, Size::new(20, 5));
        assert_eq!(c.preferred, Size::new(20, 5));
        assert_eq!(c.max, Some(Size::new(20, 5)));
    }

    #[test]
    fn custom_widget_has_intrinsic_size() {
        let widget = FixedSizeWidget {
            width: 10,
            height: 3,
        };
        assert!(widget.has_intrinsic_size());
    }

    // --- Invariant tests (property-like) ---

    #[test]
    fn measure_is_pure_same_input_same_output() {
        let widget = FixedSizeWidget {
            width: 15,
            height: 4,
        };
        let available = Size::new(100, 50);

        let a = widget.measure(available);
        let b = widget.measure(available);

        assert_eq!(a, b, "measure() must be pure");
    }

    /// `min <= preferred <= max` is this trait's contract for every
    /// implementor, and it was only ever checked for `Paragraph` over ASCII -
    /// the two tests that stood here asserted it of a struct literal built to
    /// satisfy it. Measuring all of them found two breaches: a bordered,
    /// word-wrapped `Paragraph` at narrow widths (min 8, preferred 3), and a
    /// zero-width `MiniBar` (min 1 against preferred and max 0).
    #[test]
    fn every_implementor_keeps_min_le_preferred_le_max() {
        use crate::block::Block;
        use crate::borders::Borders;
        use crate::list::{List, ListItem};
        use crate::paragraph::Paragraph;
        use crate::progress::{MiniBar, ProgressBar};
        use crate::rule::Rule;
        use crate::sparkline::Sparkline;
        use crate::table::{Row, Table};
        use ftui_layout::Constraint;
        use ftui_text::{Text, WrapMode};

        let bordered = || Block::new().borders(Borders::ALL);
        let data = [1.0, 5.0, 2.0, 8.0];
        let no_data: [f64; 0] = [];
        let widgets: Vec<(&str, Box<dyn MeasurableWidget>)> = vec![
            ("Block", Box::new(Block::new())),
            (
                "Block bordered+titled",
                Box::new(bordered().title("A long block title")),
            ),
            ("Paragraph empty", Box::new(Paragraph::new(Text::raw("")))),
            (
                "Paragraph wide glyphs",
                Box::new(Paragraph::new(Text::raw("\u{65e5}\u{672c}\u{8a9e} text"))),
            ),
            (
                "Paragraph multiline",
                Box::new(Paragraph::new(Text::raw("a\nbb\nccc"))),
            ),
            (
                "Paragraph word-wrapped in a block",
                Box::new(
                    Paragraph::new(Text::raw("some words to wrap around"))
                        .wrap(WrapMode::Word)
                        .block(bordered().title("Title")),
                ),
            ),
            (
                // Char wrap's minimum is the widest grapheme (2 here), and at
                // one column the preferred width must be floored up to it.
                "Paragraph char-wrapped wide glyphs",
                Box::new(
                    Paragraph::new(Text::raw("\u{65e5}\u{672c}\u{8a9e}\u{65e5}\u{672c}"))
                        .wrap(WrapMode::Char),
                ),
            ),
            ("ProgressBar", Box::new(ProgressBar::new())),
            (
                "ProgressBar labelled",
                Box::new(ProgressBar::new().label("Downloading")),
            ),
            (
                "ProgressBar in a block",
                Box::new(ProgressBar::new().block(bordered())),
            ),
            ("MiniBar", Box::new(MiniBar::new(0.5, 10))),
            ("MiniBar zero width", Box::new(MiniBar::new(0.5, 0))),
            ("Sparkline", Box::new(Sparkline::new(&data))),
            ("Sparkline empty", Box::new(Sparkline::new(&no_data))),
            ("ListItem", Box::new(ListItem::new("item"))),
            ("ListItem empty", Box::new(ListItem::new(""))),
            ("List", Box::new(List::new(["one", "two", "three"]))),
            ("List empty", Box::new(List::new(Vec::<&str>::new()))),
            (
                "List in a block",
                Box::new(List::new(["x"]).block(bordered().title("Wide title"))),
            ),
            ("Rule", Box::new(Rule::new())),
            (
                "Rule titled",
                Box::new(Rule::new().title("A long rule title")),
            ),
            (
                "Table",
                Box::new(Table::new(
                    [Row::new(["a", "bb"]), Row::new(["ccc", "d"])],
                    [Constraint::Fixed(3), Constraint::Min(2)],
                )),
            ),
            (
                "Table with header",
                Box::new(
                    Table::new([Row::new(["a"])], [Constraint::Fixed(3)])
                        .header(Row::new(["Header"])),
                ),
            ),
            (
                "Table empty",
                Box::new(Table::new(Vec::<Row>::new(), Vec::<Constraint>::new())),
            ),
            (
                "Table in a block",
                Box::new(
                    Table::new([Row::new(["a"])], [Constraint::Fixed(1)])
                        .block(bordered().title("Title")),
                ),
            ),
        ];
        let sizes = [
            Size::new(0, 0),
            Size::new(1, 1),
            Size::new(3, 1),
            Size::new(5, 3),
            Size::new(10, 5),
            Size::new(80, 24),
            Size::MAX,
        ];

        let mut breaches = Vec::new();
        for (name, widget) in &widgets {
            for &available in &sizes {
                let c = widget.measure(available);
                let max = c.max.unwrap_or(Size::MAX);
                if c.min.width > c.preferred.width
                    || c.min.height > c.preferred.height
                    || c.preferred.width > max.width
                    || c.preferred.height > max.height
                {
                    breaches.push(format!(
                        "{name} at {}x{}: min {}x{}, preferred {}x{}, max {:?}",
                        available.width,
                        available.height,
                        c.min.width,
                        c.min.height,
                        c.preferred.width,
                        c.preferred.height,
                        c.max.map(|m| (m.width, m.height)),
                    ));
                }
            }
        }
        assert!(breaches.is_empty(), "{}", breaches.join("\n"));
    }

    // --- Property tests (proptest) ---

    mod property_tests {
        use super::*;
        use crate::paragraph::Paragraph;
        use ftui_text::Text;
        use proptest::prelude::*;

        fn size_strategy() -> impl Strategy<Value = Size> {
            (0u16..200, 0u16..100).prop_map(|(w, h)| Size::new(w, h))
        }

        fn text_strategy() -> impl Strategy<Value = String> {
            "[a-zA-Z0-9 ]{0,200}".prop_map(|s| s.to_string())
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            // Invariant: min <= preferred for both dimensions.
            #[test]
            fn paragraph_min_le_preferred(text in text_strategy(), available in size_strategy()) {
                let para = Paragraph::new(Text::raw(text));
                let c = para.measure(available);
                prop_assert!(c.min.width <= c.preferred.width,
                    "min.width {} > preferred.width {}", c.min.width, c.preferred.width);
                prop_assert!(c.min.height <= c.preferred.height,
                    "min.height {} > preferred.height {}", c.min.height, c.preferred.height);
            }

            // Invariant: preferred <= max when max is bounded.
            #[test]
            fn constraints_preferred_le_max(
                min_w in 0u16..50,
                min_h in 0u16..20,
                pref_w in 1u16..100,
                pref_h in 1u16..60,
                max_w in 1u16..150,
                max_h in 1u16..80,
                input in size_strategy(),
            ) {
                let min = Size::new(min_w, min_h);
                let preferred = Size::new(pref_w.max(min_w), pref_h.max(min_h));
                let max = Size::new(max_w.max(preferred.width), max_h.max(preferred.height));

                let c = SizeConstraints {
                    min,
                    preferred,
                    max: Some(max),
                };

                // Clamp should never exceed max.
                let clamped = c.clamp(input);
                prop_assert!(clamped.width <= max.width);
                prop_assert!(clamped.height <= max.height);

                // Preferred is always <= max.
                prop_assert!(c.preferred.width <= max.width);
                prop_assert!(c.preferred.height <= max.height);
            }

            // Invariant: measure() is pure for the same inputs.
            #[test]
            fn paragraph_measure_is_pure(text in text_strategy(), available in size_strategy()) {
                let para = Paragraph::new(Text::raw(text));
                let c1 = para.measure(available);
                let c2 = para.measure(available);
                prop_assert_eq!(c1, c2);
            }

            // Invariant: min size does not depend on available size.
            #[test]
            fn paragraph_min_constant(text in text_strategy(), a in size_strategy(), b in size_strategy()) {
                let para = Paragraph::new(Text::raw(text));
                let c1 = para.measure(a);
                let c2 = para.measure(b);
                prop_assert_eq!(c1.min, c2.min);
            }

            // Invariant: clamp is idempotent.
            #[test]
            fn clamp_is_idempotent(
                min_w in 0u16..50, min_h in 0u16..20,
                pref_w in 1u16..120, pref_h in 1u16..80,
                max_w in 1u16..200, max_h in 1u16..120,
                input in size_strategy(),
            ) {
                let min = Size::new(min_w, min_h);
                let preferred = Size::new(pref_w.max(min_w), pref_h.max(min_h));
                let max = Size::new(max_w.max(preferred.width), max_h.max(preferred.height));
                let c = SizeConstraints { min, preferred, max: Some(max) };

                let clamped = c.clamp(input);
                let clamped_again = c.clamp(clamped);
                prop_assert_eq!(clamped, clamped_again);
            }
        }
    }
}
