#![forbid(unsafe_code)]

//! Focus indicator styling for visually identifying the focused widget.
//!
//! [`FocusIndicator`] describes how a focused widget should render its
//! focus cue. Widgets query the indicator to apply a focus style overlay,
//! border highlight, or underline to their focused state.
//!
//! # Usage
//!
//! ```rust
//! use ftui_widgets::focus::FocusIndicator;
//! use ftui_style::Style;
//!
//! // Default: reverse video on focused element
//! let indicator = FocusIndicator::default();
//!
//! // Custom: blue underline with bold
//! let indicator = FocusIndicator::underline()
//!     .with_style(Style::new().bold());
//! ```

use ftui_style::Style;

/// The kind of visual cue used to indicate focus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FocusIndicatorKind {
    /// Apply a style overlay (e.g., reverse video) to the focused widget.
    #[default]
    StyleOverlay,
    /// Draw an underline on the focused widget's content.
    Underline,
    /// Highlight the border of the focused widget's block.
    Border,
    /// No visual indicator (focus is tracked but not shown).
    None,
}

/// Configuration for how focused widgets render their focus cue.
///
/// Combines a [`FocusIndicatorKind`] (what to draw) with a [`Style`]
/// (how to draw it). Widgets call [`style`](FocusIndicator::style) to
/// get the overlay style and [`kind`](FocusIndicator::kind) to decide
/// the rendering strategy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FocusIndicator {
    kind: FocusIndicatorKind,
    style: Style,
}

impl Default for FocusIndicator {
    /// Default focus indicator: reverse video overlay.
    fn default() -> Self {
        Self {
            kind: FocusIndicatorKind::StyleOverlay,
            style: Style::new().reverse(),
        }
    }
}

impl FocusIndicator {
    /// Create a focus indicator with a custom style overlay.
    #[must_use]
    pub fn style_overlay(style: Style) -> Self {
        Self {
            kind: FocusIndicatorKind::StyleOverlay,
            style,
        }
    }

    /// Create a focus indicator that underlines focused content.
    #[must_use]
    pub fn underline() -> Self {
        Self {
            kind: FocusIndicatorKind::Underline,
            style: Style::new().underline(),
        }
    }

    /// Create a focus indicator that highlights the widget border.
    #[must_use]
    pub fn border() -> Self {
        Self {
            kind: FocusIndicatorKind::Border,
            style: Style::new().bold(),
        }
    }

    /// Create a focus indicator with no visual cue.
    #[must_use]
    pub fn none() -> Self {
        Self {
            kind: FocusIndicatorKind::None,
            style: Style::new(),
        }
    }

    /// Set the style for this indicator.
    #[must_use]
    pub fn with_style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set the kind of indicator.
    #[must_use]
    pub fn with_kind(mut self, kind: FocusIndicatorKind) -> Self {
        self.kind = kind;
        self
    }

    /// Get the indicator kind.
    #[inline]
    #[must_use]
    pub fn kind(&self) -> FocusIndicatorKind {
        self.kind
    }

    /// Get the focus style to apply.
    #[inline]
    #[must_use]
    pub fn style(&self) -> Style {
        self.style
    }

    /// Check if this indicator has a visible focus cue.
    #[inline]
    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.kind != FocusIndicatorKind::None
    }

    /// Apply the focus style as an overlay on the given base style.
    ///
    /// The focus style's set properties override the base; unset
    /// properties fall through from the base.
    #[must_use]
    pub fn apply_to(&self, base: Style) -> Style {
        if self.kind == FocusIndicatorKind::None {
            return base;
        }
        self.style.merge(&base)
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::PackedRgba;

    #[test]
    fn default_is_reverse_overlay() {
        let ind = FocusIndicator::default();
        assert_eq!(ind.kind(), FocusIndicatorKind::StyleOverlay);
        assert!(ind.is_visible());
    }

    #[test]
    fn underline_indicator() {
        let ind = FocusIndicator::underline();
        assert_eq!(ind.kind(), FocusIndicatorKind::Underline);
        assert!(ind.is_visible());
    }

    #[test]
    fn border_indicator() {
        let ind = FocusIndicator::border();
        assert_eq!(ind.kind(), FocusIndicatorKind::Border);
        assert!(ind.is_visible());
    }

    #[test]
    fn none_indicator_not_visible() {
        let ind = FocusIndicator::none();
        assert_eq!(ind.kind(), FocusIndicatorKind::None);
        assert!(!ind.is_visible());
    }

    #[test]
    fn with_style_builder() {
        let style = Style::new().bold().italic();
        let ind = FocusIndicator::underline().with_style(style);
        assert_eq!(ind.style(), style);
        assert_eq!(ind.kind(), FocusIndicatorKind::Underline);
    }

    #[test]
    fn with_kind_builder() {
        let ind = FocusIndicator::default().with_kind(FocusIndicatorKind::Border);
        assert_eq!(ind.kind(), FocusIndicatorKind::Border);
    }

    #[test]
    fn apply_to_merges_styles() {
        let base = Style::new().fg(PackedRgba::rgb(255, 0, 0));
        let ind = FocusIndicator::style_overlay(Style::new().bold());
        let result = ind.apply_to(base);
        // Should have fg from base and bold from focus
        assert_eq!(result.fg, Some(PackedRgba::rgb(255, 0, 0)));
    }

    #[test]
    fn apply_to_none_returns_base() {
        let base = Style::new().fg(PackedRgba::rgb(255, 0, 0)).bold();
        let ind = FocusIndicator::none();
        let result = ind.apply_to(base);
        assert_eq!(result, base);
    }

    #[test]
    fn style_overlay_constructor() {
        let style = Style::new().italic();
        let ind = FocusIndicator::style_overlay(style);
        assert_eq!(ind.kind(), FocusIndicatorKind::StyleOverlay);
        assert_eq!(ind.style(), style);
    }
}
