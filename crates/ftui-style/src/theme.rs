#![forbid(unsafe_code)]

//! Theme system with semantic color slots.
//!
//! A Theme provides semantic color slots that map to actual colors. This enables
//! consistent styling and easy theme switching (light/dark mode, custom themes).
//!
//! # Example
//! ```
//! use ftui_style::theme::{Theme, AdaptiveColor};
//! use ftui_style::color::Color;
//!
//! // Use the default dark theme
//! let theme = Theme::default();
//! let text_color = theme.text.resolve(true); // true = dark mode
//!
//! // Create a custom theme
//! let custom = Theme::builder()
//!     .text(Color::rgb(200, 200, 200))
//!     .background(Color::rgb(20, 20, 20))
//!     .build();
//! ```

use crate::color::Color;
use std::env;

/// An adaptive color that can change based on light/dark mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdaptiveColor {
    /// A fixed color that doesn't change with mode.
    Fixed(Color),
    /// A color that adapts to light/dark mode.
    Adaptive {
        /// Color to use in light mode.
        light: Color,
        /// Color to use in dark mode.
        dark: Color,
    },
}

impl AdaptiveColor {
    /// Create a fixed color.
    #[inline]
    pub const fn fixed(color: Color) -> Self {
        Self::Fixed(color)
    }

    /// Create an adaptive color with light/dark variants.
    #[inline]
    pub const fn adaptive(light: Color, dark: Color) -> Self {
        Self::Adaptive { light, dark }
    }

    /// Resolve the color based on the current mode.
    ///
    /// # Arguments
    /// * `is_dark` - true for dark mode, false for light mode
    #[inline]
    pub const fn resolve(&self, is_dark: bool) -> Color {
        match self {
            Self::Fixed(c) => *c,
            Self::Adaptive { light, dark } => {
                if is_dark {
                    *dark
                } else {
                    *light
                }
            }
        }
    }

    /// Check if this color adapts to mode.
    #[inline]
    pub const fn is_adaptive(&self) -> bool {
        matches!(self, Self::Adaptive { .. })
    }
}

impl Default for AdaptiveColor {
    fn default() -> Self {
        Self::Fixed(Color::rgb(128, 128, 128))
    }
}

impl From<Color> for AdaptiveColor {
    fn from(color: Color) -> Self {
        Self::Fixed(color)
    }
}

/// A theme with semantic color slots.
///
/// Themes provide consistent styling across an application by mapping
/// semantic names (like "error" or "primary") to actual colors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    // Primary UI colors
    /// Primary accent color (e.g., buttons, highlights).
    pub primary: AdaptiveColor,
    /// Secondary accent color.
    pub secondary: AdaptiveColor,
    /// Tertiary accent color.
    pub accent: AdaptiveColor,

    // Backgrounds
    /// Main background color.
    pub background: AdaptiveColor,
    /// Surface color (cards, panels).
    pub surface: AdaptiveColor,
    /// Overlay color (dialogs, dropdowns).
    pub overlay: AdaptiveColor,

    // Text
    /// Primary text color.
    pub text: AdaptiveColor,
    /// Muted text color.
    pub text_muted: AdaptiveColor,
    /// Subtle text color (hints, placeholders).
    pub text_subtle: AdaptiveColor,

    // Semantic colors
    /// Success color (green).
    pub success: AdaptiveColor,
    /// Warning color (yellow/orange).
    pub warning: AdaptiveColor,
    /// Error color (red).
    pub error: AdaptiveColor,
    /// Info color (blue).
    pub info: AdaptiveColor,

    // Borders
    /// Default border color.
    pub border: AdaptiveColor,
    /// Focused element border.
    pub border_focused: AdaptiveColor,

    // Selection
    /// Selection background.
    pub selection_bg: AdaptiveColor,
    /// Selection foreground.
    pub selection_fg: AdaptiveColor,

    // Scrollbar
    /// Scrollbar track color.
    pub scrollbar_track: AdaptiveColor,
    /// Scrollbar thumb color.
    pub scrollbar_thumb: AdaptiveColor,
}

impl Default for Theme {
    fn default() -> Self {
        themes::dark()
    }
}

impl Theme {
    /// Create a new theme builder.
    pub fn builder() -> ThemeBuilder {
        ThemeBuilder::new()
    }

    /// Detect whether dark mode should be used.
    ///
    /// Detection heuristics:
    /// 1. Check COLORFGBG environment variable
    /// 2. Default to dark mode (most terminals are dark)
    ///
    /// Note: OSC 11 background query would be more accurate but requires
    /// terminal interaction which isn't always safe or fast.
    #[must_use]
    pub fn detect_dark_mode() -> bool {
        Self::detect_dark_mode_from_colorfgbg(env::var("COLORFGBG").ok().as_deref())
    }

    fn detect_dark_mode_from_colorfgbg(colorfgbg: Option<&str>) -> bool {
        // COLORFGBG format: "fg;bg" where values are ANSI color indices
        // Common light terminals use bg=15 (white), dark use bg=0 (black)
        if let Some(colorfgbg) = colorfgbg
            && let Some(bg_part) = colorfgbg.split(';').next_back()
            && let Ok(bg) = bg_part.trim().parse::<u8>()
        {
            // High ANSI indices (7, 15) typically mean light background
            return bg != 7 && bg != 15;
        }

        // Default to dark mode (most common for terminals)
        true
    }

    /// Create a resolved copy of this theme for a specific mode.
    ///
    /// This flattens all adaptive colors to fixed colors based on the mode.
    #[must_use]
    pub fn resolve(&self, is_dark: bool) -> ResolvedTheme {
        ResolvedTheme {
            primary: self.primary.resolve(is_dark),
            secondary: self.secondary.resolve(is_dark),
            accent: self.accent.resolve(is_dark),
            background: self.background.resolve(is_dark),
            surface: self.surface.resolve(is_dark),
            overlay: self.overlay.resolve(is_dark),
            text: self.text.resolve(is_dark),
            text_muted: self.text_muted.resolve(is_dark),
            text_subtle: self.text_subtle.resolve(is_dark),
            success: self.success.resolve(is_dark),
            warning: self.warning.resolve(is_dark),
            error: self.error.resolve(is_dark),
            info: self.info.resolve(is_dark),
            border: self.border.resolve(is_dark),
            border_focused: self.border_focused.resolve(is_dark),
            selection_bg: self.selection_bg.resolve(is_dark),
            selection_fg: self.selection_fg.resolve(is_dark),
            scrollbar_track: self.scrollbar_track.resolve(is_dark),
            scrollbar_thumb: self.scrollbar_thumb.resolve(is_dark),
        }
    }
}

/// A theme with all colors resolved to fixed values.
///
/// This is the result of calling `Theme::resolve()` with a specific mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedTheme {
    /// Primary accent color.
    pub primary: Color,
    /// Secondary accent color.
    pub secondary: Color,
    /// Tertiary accent color.
    pub accent: Color,
    /// Main background color.
    pub background: Color,
    /// Surface color (cards, panels).
    pub surface: Color,
    /// Overlay color (dialogs, dropdowns).
    pub overlay: Color,
    /// Primary text color.
    pub text: Color,
    /// Muted text color.
    pub text_muted: Color,
    /// Subtle text color (hints, placeholders).
    pub text_subtle: Color,
    /// Success color.
    pub success: Color,
    /// Warning color.
    pub warning: Color,
    /// Error color.
    pub error: Color,
    /// Info color.
    pub info: Color,
    /// Default border color.
    pub border: Color,
    /// Focused element border.
    pub border_focused: Color,
    /// Selection background.
    pub selection_bg: Color,
    /// Selection foreground.
    pub selection_fg: Color,
    /// Scrollbar track color.
    pub scrollbar_track: Color,
    /// Scrollbar thumb color.
    pub scrollbar_thumb: Color,
}

/// Builder for creating custom themes.
#[derive(Debug, Clone)]
#[must_use]
pub struct ThemeBuilder {
    theme: Theme,
}

impl ThemeBuilder {
    /// Create a new builder starting from the default dark theme.
    pub fn new() -> Self {
        Self {
            theme: themes::dark(),
        }
    }

    /// Start from a base theme.
    pub fn from_theme(theme: Theme) -> Self {
        Self { theme }
    }

    /// Set the primary color.
    pub fn primary(mut self, color: impl Into<AdaptiveColor>) -> Self {
        self.theme.primary = color.into();
        self
    }

    /// Set the secondary color.
    pub fn secondary(mut self, color: impl Into<AdaptiveColor>) -> Self {
        self.theme.secondary = color.into();
        self
    }

    /// Set the accent color.
    pub fn accent(mut self, color: impl Into<AdaptiveColor>) -> Self {
        self.theme.accent = color.into();
        self
    }

    /// Set the background color.
    pub fn background(mut self, color: impl Into<AdaptiveColor>) -> Self {
        self.theme.background = color.into();
        self
    }

    /// Set the surface color.
    pub fn surface(mut self, color: impl Into<AdaptiveColor>) -> Self {
        self.theme.surface = color.into();
        self
    }

    /// Set the overlay color.
    pub fn overlay(mut self, color: impl Into<AdaptiveColor>) -> Self {
        self.theme.overlay = color.into();
        self
    }

    /// Set the text color.
    pub fn text(mut self, color: impl Into<AdaptiveColor>) -> Self {
        self.theme.text = color.into();
        self
    }

    /// Set the muted text color.
    pub fn text_muted(mut self, color: impl Into<AdaptiveColor>) -> Self {
        self.theme.text_muted = color.into();
        self
    }

    /// Set the subtle text color.
    pub fn text_subtle(mut self, color: impl Into<AdaptiveColor>) -> Self {
        self.theme.text_subtle = color.into();
        self
    }

    /// Set the success color.
    pub fn success(mut self, color: impl Into<AdaptiveColor>) -> Self {
        self.theme.success = color.into();
        self
    }

    /// Set the warning color.
    pub fn warning(mut self, color: impl Into<AdaptiveColor>) -> Self {
        self.theme.warning = color.into();
        self
    }

    /// Set the error color.
    pub fn error(mut self, color: impl Into<AdaptiveColor>) -> Self {
        self.theme.error = color.into();
        self
    }

    /// Set the info color.
    pub fn info(mut self, color: impl Into<AdaptiveColor>) -> Self {
        self.theme.info = color.into();
        self
    }

    /// Set the border color.
    pub fn border(mut self, color: impl Into<AdaptiveColor>) -> Self {
        self.theme.border = color.into();
        self
    }

    /// Set the focused border color.
    pub fn border_focused(mut self, color: impl Into<AdaptiveColor>) -> Self {
        self.theme.border_focused = color.into();
        self
    }

    /// Set the selection background color.
    pub fn selection_bg(mut self, color: impl Into<AdaptiveColor>) -> Self {
        self.theme.selection_bg = color.into();
        self
    }

    /// Set the selection foreground color.
    pub fn selection_fg(mut self, color: impl Into<AdaptiveColor>) -> Self {
        self.theme.selection_fg = color.into();
        self
    }

    /// Set the scrollbar track color.
    pub fn scrollbar_track(mut self, color: impl Into<AdaptiveColor>) -> Self {
        self.theme.scrollbar_track = color.into();
        self
    }

    /// Set the scrollbar thumb color.
    pub fn scrollbar_thumb(mut self, color: impl Into<AdaptiveColor>) -> Self {
        self.theme.scrollbar_thumb = color.into();
        self
    }

    /// Build the theme.
    pub fn build(self) -> Theme {
        self.theme
    }
}

impl Default for ThemeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Built-in theme presets.
pub mod themes {
    use super::*;

    /// Default sensible theme (dark mode).
    #[must_use]
    pub fn default() -> Theme {
        dark()
    }

    /// Dark theme.
    #[must_use]
    pub fn dark() -> Theme {
        Theme {
            primary: AdaptiveColor::fixed(Color::rgb(88, 166, 255)), // Blue
            secondary: AdaptiveColor::fixed(Color::rgb(163, 113, 247)), // Purple
            accent: AdaptiveColor::fixed(Color::rgb(255, 123, 114)), // Coral

            background: AdaptiveColor::fixed(Color::rgb(22, 27, 34)), // Dark gray
            surface: AdaptiveColor::fixed(Color::rgb(33, 38, 45)),    // Slightly lighter
            overlay: AdaptiveColor::fixed(Color::rgb(48, 54, 61)),    // Even lighter

            text: AdaptiveColor::fixed(Color::rgb(230, 237, 243)), // Bright
            text_muted: AdaptiveColor::fixed(Color::rgb(139, 148, 158)), // Gray
            text_subtle: AdaptiveColor::fixed(Color::rgb(110, 118, 129)), // Darker gray

            success: AdaptiveColor::fixed(Color::rgb(63, 185, 80)), // Green
            warning: AdaptiveColor::fixed(Color::rgb(210, 153, 34)), // Yellow
            error: AdaptiveColor::fixed(Color::rgb(248, 81, 73)),   // Red
            info: AdaptiveColor::fixed(Color::rgb(88, 166, 255)),   // Blue

            border: AdaptiveColor::fixed(Color::rgb(48, 54, 61)), // Subtle
            border_focused: AdaptiveColor::fixed(Color::rgb(88, 166, 255)), // Accent

            selection_bg: AdaptiveColor::fixed(Color::rgb(56, 139, 253)), // Blue
            selection_fg: AdaptiveColor::fixed(Color::rgb(255, 255, 255)), // White

            scrollbar_track: AdaptiveColor::fixed(Color::rgb(33, 38, 45)),
            scrollbar_thumb: AdaptiveColor::fixed(Color::rgb(72, 79, 88)),
        }
    }

    /// Light theme.
    #[must_use]
    pub fn light() -> Theme {
        Theme {
            primary: AdaptiveColor::fixed(Color::rgb(9, 105, 218)), // Blue
            secondary: AdaptiveColor::fixed(Color::rgb(130, 80, 223)), // Purple
            accent: AdaptiveColor::fixed(Color::rgb(207, 34, 46)),  // Red

            background: AdaptiveColor::fixed(Color::rgb(255, 255, 255)), // White
            surface: AdaptiveColor::fixed(Color::rgb(246, 248, 250)),    // Light gray
            overlay: AdaptiveColor::fixed(Color::rgb(255, 255, 255)),    // White

            text: AdaptiveColor::fixed(Color::rgb(31, 35, 40)), // Dark
            text_muted: AdaptiveColor::fixed(Color::rgb(87, 96, 106)), // Gray
            text_subtle: AdaptiveColor::fixed(Color::rgb(140, 149, 159)), // Light gray

            success: AdaptiveColor::fixed(Color::rgb(26, 127, 55)), // Green
            warning: AdaptiveColor::fixed(Color::rgb(158, 106, 3)), // Yellow
            error: AdaptiveColor::fixed(Color::rgb(207, 34, 46)),   // Red
            info: AdaptiveColor::fixed(Color::rgb(9, 105, 218)),    // Blue

            border: AdaptiveColor::fixed(Color::rgb(208, 215, 222)), // Light gray
            border_focused: AdaptiveColor::fixed(Color::rgb(9, 105, 218)), // Accent

            selection_bg: AdaptiveColor::fixed(Color::rgb(221, 244, 255)), // Light blue
            selection_fg: AdaptiveColor::fixed(Color::rgb(31, 35, 40)),    // Dark

            scrollbar_track: AdaptiveColor::fixed(Color::rgb(246, 248, 250)),
            scrollbar_thumb: AdaptiveColor::fixed(Color::rgb(175, 184, 193)),
        }
    }

    /// Nord color scheme (dark variant).
    #[must_use]
    pub fn nord() -> Theme {
        Theme {
            primary: AdaptiveColor::fixed(Color::rgb(136, 192, 208)), // Nord8 (frost)
            secondary: AdaptiveColor::fixed(Color::rgb(180, 142, 173)), // Nord15 (purple)
            accent: AdaptiveColor::fixed(Color::rgb(191, 97, 106)),   // Nord11 (aurora red)

            background: AdaptiveColor::fixed(Color::rgb(46, 52, 64)), // Nord0
            surface: AdaptiveColor::fixed(Color::rgb(59, 66, 82)),    // Nord1
            overlay: AdaptiveColor::fixed(Color::rgb(67, 76, 94)),    // Nord2

            text: AdaptiveColor::fixed(Color::rgb(236, 239, 244)), // Nord6
            text_muted: AdaptiveColor::fixed(Color::rgb(216, 222, 233)), // Nord4
            text_subtle: AdaptiveColor::fixed(Color::rgb(129, 161, 193)), // Nord9

            success: AdaptiveColor::fixed(Color::rgb(163, 190, 140)), // Nord14 (green)
            warning: AdaptiveColor::fixed(Color::rgb(235, 203, 139)), // Nord13 (yellow)
            error: AdaptiveColor::fixed(Color::rgb(191, 97, 106)),    // Nord11 (red)
            info: AdaptiveColor::fixed(Color::rgb(129, 161, 193)),    // Nord9 (blue)

            border: AdaptiveColor::fixed(Color::rgb(76, 86, 106)), // Nord3
            border_focused: AdaptiveColor::fixed(Color::rgb(136, 192, 208)), // Nord8

            selection_bg: AdaptiveColor::fixed(Color::rgb(76, 86, 106)), // Nord3
            selection_fg: AdaptiveColor::fixed(Color::rgb(236, 239, 244)), // Nord6

            scrollbar_track: AdaptiveColor::fixed(Color::rgb(59, 66, 82)),
            scrollbar_thumb: AdaptiveColor::fixed(Color::rgb(76, 86, 106)),
        }
    }

    /// Dracula color scheme.
    #[must_use]
    pub fn dracula() -> Theme {
        Theme {
            primary: AdaptiveColor::fixed(Color::rgb(189, 147, 249)), // Purple
            secondary: AdaptiveColor::fixed(Color::rgb(255, 121, 198)), // Pink
            accent: AdaptiveColor::fixed(Color::rgb(139, 233, 253)),  // Cyan

            background: AdaptiveColor::fixed(Color::rgb(40, 42, 54)), // Background
            surface: AdaptiveColor::fixed(Color::rgb(68, 71, 90)),    // Current line
            overlay: AdaptiveColor::fixed(Color::rgb(68, 71, 90)),    // Current line

            text: AdaptiveColor::fixed(Color::rgb(248, 248, 242)), // Foreground
            text_muted: AdaptiveColor::fixed(Color::rgb(188, 188, 188)), // Lighter
            text_subtle: AdaptiveColor::fixed(Color::rgb(98, 114, 164)), // Comment

            success: AdaptiveColor::fixed(Color::rgb(80, 250, 123)), // Green
            warning: AdaptiveColor::fixed(Color::rgb(255, 184, 108)), // Orange
            error: AdaptiveColor::fixed(Color::rgb(255, 85, 85)),    // Red
            info: AdaptiveColor::fixed(Color::rgb(139, 233, 253)),   // Cyan

            border: AdaptiveColor::fixed(Color::rgb(68, 71, 90)), // Current line
            border_focused: AdaptiveColor::fixed(Color::rgb(189, 147, 249)), // Purple

            selection_bg: AdaptiveColor::fixed(Color::rgb(68, 71, 90)), // Current line
            selection_fg: AdaptiveColor::fixed(Color::rgb(248, 248, 242)), // Foreground

            scrollbar_track: AdaptiveColor::fixed(Color::rgb(40, 42, 54)),
            scrollbar_thumb: AdaptiveColor::fixed(Color::rgb(68, 71, 90)),
        }
    }

    /// Solarized Dark color scheme.
    #[must_use]
    pub fn solarized_dark() -> Theme {
        Theme {
            primary: AdaptiveColor::fixed(Color::rgb(38, 139, 210)), // Blue
            secondary: AdaptiveColor::fixed(Color::rgb(108, 113, 196)), // Violet
            accent: AdaptiveColor::fixed(Color::rgb(203, 75, 22)),   // Orange

            background: AdaptiveColor::fixed(Color::rgb(0, 43, 54)), // Base03
            surface: AdaptiveColor::fixed(Color::rgb(7, 54, 66)),    // Base02
            overlay: AdaptiveColor::fixed(Color::rgb(7, 54, 66)),    // Base02

            text: AdaptiveColor::fixed(Color::rgb(131, 148, 150)), // Base0
            text_muted: AdaptiveColor::fixed(Color::rgb(101, 123, 131)), // Base00
            text_subtle: AdaptiveColor::fixed(Color::rgb(88, 110, 117)), // Base01

            success: AdaptiveColor::fixed(Color::rgb(133, 153, 0)), // Green
            warning: AdaptiveColor::fixed(Color::rgb(181, 137, 0)), // Yellow
            error: AdaptiveColor::fixed(Color::rgb(220, 50, 47)),   // Red
            info: AdaptiveColor::fixed(Color::rgb(38, 139, 210)),   // Blue

            border: AdaptiveColor::fixed(Color::rgb(7, 54, 66)), // Base02
            border_focused: AdaptiveColor::fixed(Color::rgb(38, 139, 210)), // Blue

            selection_bg: AdaptiveColor::fixed(Color::rgb(7, 54, 66)), // Base02
            selection_fg: AdaptiveColor::fixed(Color::rgb(147, 161, 161)), // Base1

            scrollbar_track: AdaptiveColor::fixed(Color::rgb(0, 43, 54)),
            scrollbar_thumb: AdaptiveColor::fixed(Color::rgb(7, 54, 66)),
        }
    }

    /// Solarized Light color scheme.
    #[must_use]
    pub fn solarized_light() -> Theme {
        Theme {
            primary: AdaptiveColor::fixed(Color::rgb(38, 139, 210)), // Blue
            secondary: AdaptiveColor::fixed(Color::rgb(108, 113, 196)), // Violet
            accent: AdaptiveColor::fixed(Color::rgb(203, 75, 22)),   // Orange

            background: AdaptiveColor::fixed(Color::rgb(253, 246, 227)), // Base3
            surface: AdaptiveColor::fixed(Color::rgb(238, 232, 213)),    // Base2
            overlay: AdaptiveColor::fixed(Color::rgb(253, 246, 227)),    // Base3

            text: AdaptiveColor::fixed(Color::rgb(101, 123, 131)), // Base00
            text_muted: AdaptiveColor::fixed(Color::rgb(88, 110, 117)), // Base01
            text_subtle: AdaptiveColor::fixed(Color::rgb(147, 161, 161)), // Base1

            success: AdaptiveColor::fixed(Color::rgb(133, 153, 0)), // Green
            warning: AdaptiveColor::fixed(Color::rgb(181, 137, 0)), // Yellow
            error: AdaptiveColor::fixed(Color::rgb(220, 50, 47)),   // Red
            info: AdaptiveColor::fixed(Color::rgb(38, 139, 210)),   // Blue

            border: AdaptiveColor::fixed(Color::rgb(238, 232, 213)), // Base2
            border_focused: AdaptiveColor::fixed(Color::rgb(38, 139, 210)), // Blue

            selection_bg: AdaptiveColor::fixed(Color::rgb(238, 232, 213)), // Base2
            selection_fg: AdaptiveColor::fixed(Color::rgb(88, 110, 117)),  // Base01

            scrollbar_track: AdaptiveColor::fixed(Color::rgb(253, 246, 227)),
            scrollbar_thumb: AdaptiveColor::fixed(Color::rgb(238, 232, 213)),
        }
    }

    /// Monokai color scheme.
    #[must_use]
    pub fn monokai() -> Theme {
        Theme {
            primary: AdaptiveColor::fixed(Color::rgb(102, 217, 239)), // Cyan
            secondary: AdaptiveColor::fixed(Color::rgb(174, 129, 255)), // Purple
            accent: AdaptiveColor::fixed(Color::rgb(249, 38, 114)),   // Pink

            background: AdaptiveColor::fixed(Color::rgb(39, 40, 34)), // Background
            surface: AdaptiveColor::fixed(Color::rgb(60, 61, 54)),    // Lighter
            overlay: AdaptiveColor::fixed(Color::rgb(60, 61, 54)),    // Lighter

            text: AdaptiveColor::fixed(Color::rgb(248, 248, 242)), // Foreground
            text_muted: AdaptiveColor::fixed(Color::rgb(189, 189, 189)), // Gray
            text_subtle: AdaptiveColor::fixed(Color::rgb(117, 113, 94)), // Comment

            success: AdaptiveColor::fixed(Color::rgb(166, 226, 46)), // Green
            warning: AdaptiveColor::fixed(Color::rgb(230, 219, 116)), // Yellow
            error: AdaptiveColor::fixed(Color::rgb(249, 38, 114)),   // Pink/red
            info: AdaptiveColor::fixed(Color::rgb(102, 217, 239)),   // Cyan

            border: AdaptiveColor::fixed(Color::rgb(60, 61, 54)), // Lighter bg
            border_focused: AdaptiveColor::fixed(Color::rgb(102, 217, 239)), // Cyan

            selection_bg: AdaptiveColor::fixed(Color::rgb(73, 72, 62)), // Selection
            selection_fg: AdaptiveColor::fixed(Color::rgb(248, 248, 242)), // Foreground

            scrollbar_track: AdaptiveColor::fixed(Color::rgb(39, 40, 34)),
            scrollbar_thumb: AdaptiveColor::fixed(Color::rgb(60, 61, 54)),
        }
    }

    /// Doom color scheme (High contrast, Industrial).
    #[must_use]
    pub fn doom() -> Theme {
        Theme {
            primary: AdaptiveColor::fixed(Color::rgb(178, 34, 34)), // Firebrick
            secondary: AdaptiveColor::fixed(Color::rgb(50, 205, 50)), // LimeGreen
            accent: AdaptiveColor::fixed(Color::rgb(255, 255, 0)), // Yellow (Ammo)

            background: AdaptiveColor::fixed(Color::rgb(26, 26, 26)), // Dark Grey
            surface: AdaptiveColor::fixed(Color::rgb(47, 47, 47)),    // Gunmetal
            overlay: AdaptiveColor::fixed(Color::rgb(64, 64, 64)),    // Dim Grey

            text: AdaptiveColor::fixed(Color::rgb(211, 211, 211)), // Light Grey
            text_muted: AdaptiveColor::fixed(Color::rgb(128, 128, 128)), // Grey
            text_subtle: AdaptiveColor::fixed(Color::rgb(105, 105, 105)), // Dim Grey

            success: AdaptiveColor::fixed(Color::rgb(50, 205, 50)), // Green
            warning: AdaptiveColor::fixed(Color::rgb(255, 215, 0)), // Gold
            error: AdaptiveColor::fixed(Color::rgb(139, 0, 0)),     // Dark Red
            info: AdaptiveColor::fixed(Color::rgb(65, 105, 225)),   // Royal Blue

            border: AdaptiveColor::fixed(Color::rgb(105, 105, 105)), // Dim Grey
            border_focused: AdaptiveColor::fixed(Color::rgb(178, 34, 34)), // Firebrick

            selection_bg: AdaptiveColor::fixed(Color::rgb(139, 0, 0)), // Dark Red
            selection_fg: AdaptiveColor::fixed(Color::rgb(255, 255, 255)), // White

            scrollbar_track: AdaptiveColor::fixed(Color::rgb(26, 26, 26)),
            scrollbar_thumb: AdaptiveColor::fixed(Color::rgb(178, 34, 34)),
        }
    }

    /// Quake color scheme (Earth tones, Medieval).
    #[must_use]
    pub fn quake() -> Theme {
        Theme {
            primary: AdaptiveColor::fixed(Color::rgb(139, 69, 19)), // SaddleBrown
            secondary: AdaptiveColor::fixed(Color::rgb(85, 107, 47)), // DarkOliveGreen
            accent: AdaptiveColor::fixed(Color::rgb(205, 133, 63)),   // Peru

            background: AdaptiveColor::fixed(Color::rgb(28, 28, 28)), // Very Dark Grey
            surface: AdaptiveColor::fixed(Color::rgb(46, 39, 34)),    // Deep Brown/Grey
            overlay: AdaptiveColor::fixed(Color::rgb(62, 54, 48)),    // Lighter Brown/Grey

            text: AdaptiveColor::fixed(Color::rgb(210, 180, 140)), // Tan
            text_muted: AdaptiveColor::fixed(Color::rgb(139, 115, 85)), // Dark Tan
            text_subtle: AdaptiveColor::fixed(Color::rgb(101, 84, 61)), // Deep Tan

            success: AdaptiveColor::fixed(Color::rgb(85, 107, 47)), // Olive
            warning: AdaptiveColor::fixed(Color::rgb(210, 105, 30)), // Chocolate
            error: AdaptiveColor::fixed(Color::rgb(128, 0, 0)),     // Maroon
            info: AdaptiveColor::fixed(Color::rgb(70, 130, 180)),   // SteelBlue

            border: AdaptiveColor::fixed(Color::rgb(93, 64, 55)), // Rusty Brown
            border_focused: AdaptiveColor::fixed(Color::rgb(205, 133, 63)), // Peru

            selection_bg: AdaptiveColor::fixed(Color::rgb(139, 69, 19)), // SaddleBrown
            selection_fg: AdaptiveColor::fixed(Color::rgb(255, 222, 173)), // NavajoWhite

            scrollbar_track: AdaptiveColor::fixed(Color::rgb(28, 28, 28)),
            scrollbar_thumb: AdaptiveColor::fixed(Color::rgb(139, 69, 19)),
        }
    }
}

// ============================================================================
// SharedResolvedTheme — ArcSwap-backed concurrent access (bd-3l9qr.2)
// ============================================================================

/// Wait-free shared resolved theme for concurrent read/write.
///
/// Wraps a [`ResolvedTheme`] in an [`arc_swap::ArcSwap`] so that the render
/// thread can read theme colors without locking while the main thread updates
/// them on theme switch or mode change.
///
/// # Example
///
/// ```
/// use ftui_style::theme::{Theme, ResolvedTheme, SharedResolvedTheme};
///
/// let theme = Theme::default();
/// let resolved = theme.resolve(true); // dark mode
/// let shared = SharedResolvedTheme::new(resolved);
///
/// // Wait-free read from render thread
/// let current = shared.load();
/// assert_eq!(current.primary, resolved.primary);
///
/// // Update on theme switch
/// let light = theme.resolve(false);
/// shared.store(light);
/// ```
pub struct SharedResolvedTheme {
    inner: arc_swap::ArcSwap<ResolvedTheme>,
}

impl SharedResolvedTheme {
    /// Create shared theme from an initial resolved theme.
    pub fn new(theme: ResolvedTheme) -> Self {
        Self {
            inner: arc_swap::ArcSwap::from_pointee(theme),
        }
    }

    /// Wait-free read of current resolved theme.
    #[inline]
    pub fn load(&self) -> ResolvedTheme {
        let guard = self.inner.load();
        **guard
    }

    /// Atomically replace the resolved theme (e.g., on theme switch or mode change).
    #[inline]
    pub fn store(&self, theme: ResolvedTheme) {
        self.inner.store(std::sync::Arc::new(theme));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_color_fixed() {
        let color = AdaptiveColor::fixed(Color::rgb(255, 0, 0));
        assert_eq!(color.resolve(true), Color::rgb(255, 0, 0));
        assert_eq!(color.resolve(false), Color::rgb(255, 0, 0));
        assert!(!color.is_adaptive());
    }

    #[test]
    fn adaptive_color_adaptive() {
        let color = AdaptiveColor::adaptive(
            Color::rgb(255, 255, 255), // light
            Color::rgb(0, 0, 0),       // dark
        );
        assert_eq!(color.resolve(true), Color::rgb(0, 0, 0)); // dark
        assert_eq!(color.resolve(false), Color::rgb(255, 255, 255)); // light
        assert!(color.is_adaptive());
    }

    #[test]
    fn theme_default_is_dark() {
        let theme = Theme::default();
        // Dark themes typically have dark backgrounds
        let bg = theme.background.resolve(true);
        if let Color::Rgb(rgb) = bg {
            // Background should be dark
            assert!(rgb.luminance_u8() < 50);
        }
    }

    #[test]
    fn theme_light_has_light_background() {
        let theme = themes::light();
        let bg = theme.background.resolve(false);
        if let Color::Rgb(rgb) = bg {
            // Light background
            assert!(rgb.luminance_u8() > 200);
        }
    }

    #[test]
    fn theme_has_all_slots() {
        let theme = Theme::default();
        // Just verify all slots exist and resolve without panic
        let _ = theme.primary.resolve(true);
        let _ = theme.secondary.resolve(true);
        let _ = theme.accent.resolve(true);
        let _ = theme.background.resolve(true);
        let _ = theme.surface.resolve(true);
        let _ = theme.overlay.resolve(true);
        let _ = theme.text.resolve(true);
        let _ = theme.text_muted.resolve(true);
        let _ = theme.text_subtle.resolve(true);
        let _ = theme.success.resolve(true);
        let _ = theme.warning.resolve(true);
        let _ = theme.error.resolve(true);
        let _ = theme.info.resolve(true);
        let _ = theme.border.resolve(true);
        let _ = theme.border_focused.resolve(true);
        let _ = theme.selection_bg.resolve(true);
        let _ = theme.selection_fg.resolve(true);
        let _ = theme.scrollbar_track.resolve(true);
        let _ = theme.scrollbar_thumb.resolve(true);
    }

    #[test]
    fn theme_builder_works() {
        let theme = Theme::builder()
            .primary(Color::rgb(255, 0, 0))
            .background(Color::rgb(0, 0, 0))
            .build();

        assert_eq!(theme.primary.resolve(true), Color::rgb(255, 0, 0));
        assert_eq!(theme.background.resolve(true), Color::rgb(0, 0, 0));
    }

    #[test]
    fn theme_resolve_flattens() {
        let theme = themes::dark();
        let resolved = theme.resolve(true);

        // All colors should be the same as resolving individually
        assert_eq!(resolved.primary, theme.primary.resolve(true));
        assert_eq!(resolved.text, theme.text.resolve(true));
        assert_eq!(resolved.background, theme.background.resolve(true));
    }

    #[test]
    fn all_presets_exist() {
        let _ = themes::default();
        let _ = themes::dark();
        let _ = themes::light();
        let _ = themes::nord();
        let _ = themes::dracula();
        let _ = themes::solarized_dark();
        let _ = themes::solarized_light();
        let _ = themes::monokai();
    }

    #[test]
    fn presets_have_different_colors() {
        let dark = themes::dark();
        let light = themes::light();
        let nord = themes::nord();

        // Different themes should have different backgrounds
        assert_ne!(
            dark.background.resolve(true),
            light.background.resolve(false)
        );
        assert_ne!(dark.background.resolve(true), nord.background.resolve(true));
    }

    #[test]
    fn detect_dark_mode_returns_bool() {
        // Just verify it doesn't panic
        let _ = Theme::detect_dark_mode();
    }

    #[test]
    fn color_converts_to_adaptive() {
        let color = Color::rgb(100, 150, 200);
        let adaptive: AdaptiveColor = color.into();
        assert_eq!(adaptive.resolve(true), color);
        assert_eq!(adaptive.resolve(false), color);
    }

    #[test]
    fn builder_from_theme() {
        let base = themes::nord();
        let modified = ThemeBuilder::from_theme(base.clone())
            .primary(Color::rgb(255, 0, 0))
            .build();

        // Modified primary
        assert_eq!(modified.primary.resolve(true), Color::rgb(255, 0, 0));
        // Unchanged secondary (from nord)
        assert_eq!(modified.secondary, base.secondary);
    }

    // Count semantic slots to verify we have 15+
    #[test]
    fn has_at_least_15_semantic_slots() {
        let theme = Theme::default();
        let slot_count = 19; // Counting from the struct definition
        assert!(slot_count >= 15);

        // Verify by accessing each slot
        let _slots = [
            &theme.primary,
            &theme.secondary,
            &theme.accent,
            &theme.background,
            &theme.surface,
            &theme.overlay,
            &theme.text,
            &theme.text_muted,
            &theme.text_subtle,
            &theme.success,
            &theme.warning,
            &theme.error,
            &theme.info,
            &theme.border,
            &theme.border_focused,
            &theme.selection_bg,
            &theme.selection_fg,
            &theme.scrollbar_track,
            &theme.scrollbar_thumb,
        ];
    }

    #[test]
    fn adaptive_color_default_is_gray() {
        let color = AdaptiveColor::default();
        assert!(!color.is_adaptive());
        assert_eq!(color.resolve(true), Color::rgb(128, 128, 128));
        assert_eq!(color.resolve(false), Color::rgb(128, 128, 128));
    }

    #[test]
    fn theme_builder_default() {
        let builder = ThemeBuilder::default();
        let theme = builder.build();
        // Default builder starts from dark theme
        assert_eq!(theme, themes::dark());
    }

    #[test]
    fn resolved_theme_has_all_19_slots() {
        let theme = themes::dark();
        let resolved = theme.resolve(true);
        // Just verify all slots are accessible without panic
        let _colors = [
            resolved.primary,
            resolved.secondary,
            resolved.accent,
            resolved.background,
            resolved.surface,
            resolved.overlay,
            resolved.text,
            resolved.text_muted,
            resolved.text_subtle,
            resolved.success,
            resolved.warning,
            resolved.error,
            resolved.info,
            resolved.border,
            resolved.border_focused,
            resolved.selection_bg,
            resolved.selection_fg,
            resolved.scrollbar_track,
            resolved.scrollbar_thumb,
        ];
    }

    #[test]
    fn dark_and_light_resolve_differently() {
        let theme = Theme {
            text: AdaptiveColor::adaptive(Color::rgb(0, 0, 0), Color::rgb(255, 255, 255)),
            ..themes::dark()
        };
        let dark_resolved = theme.resolve(true);
        let light_resolved = theme.resolve(false);
        assert_ne!(dark_resolved.text, light_resolved.text);
        assert_eq!(dark_resolved.text, Color::rgb(255, 255, 255));
        assert_eq!(light_resolved.text, Color::rgb(0, 0, 0));
    }

    #[test]
    fn all_dark_presets_have_dark_backgrounds() {
        for (name, theme) in [
            ("dark", themes::dark()),
            ("nord", themes::nord()),
            ("dracula", themes::dracula()),
            ("solarized_dark", themes::solarized_dark()),
            ("monokai", themes::monokai()),
        ] {
            let bg = theme.background.resolve(true);
            if let Color::Rgb(rgb) = bg {
                assert!(
                    rgb.luminance_u8() < 100,
                    "{name} background too bright: {}",
                    rgb.luminance_u8()
                );
            }
        }
    }

    #[test]
    fn all_light_presets_have_light_backgrounds() {
        for (name, theme) in [
            ("light", themes::light()),
            ("solarized_light", themes::solarized_light()),
        ] {
            let bg = theme.background.resolve(false);
            if let Color::Rgb(rgb) = bg {
                assert!(
                    rgb.luminance_u8() > 150,
                    "{name} background too dark: {}",
                    rgb.luminance_u8()
                );
            }
        }
    }

    #[test]
    fn theme_default_equals_dark() {
        assert_eq!(Theme::default(), themes::dark());
        assert_eq!(themes::default(), themes::dark());
    }

    #[test]
    fn builder_all_setters_chain() {
        let theme = Theme::builder()
            .primary(Color::rgb(1, 0, 0))
            .secondary(Color::rgb(2, 0, 0))
            .accent(Color::rgb(3, 0, 0))
            .background(Color::rgb(4, 0, 0))
            .surface(Color::rgb(5, 0, 0))
            .overlay(Color::rgb(6, 0, 0))
            .text(Color::rgb(7, 0, 0))
            .text_muted(Color::rgb(8, 0, 0))
            .text_subtle(Color::rgb(9, 0, 0))
            .success(Color::rgb(10, 0, 0))
            .warning(Color::rgb(11, 0, 0))
            .error(Color::rgb(12, 0, 0))
            .info(Color::rgb(13, 0, 0))
            .border(Color::rgb(14, 0, 0))
            .border_focused(Color::rgb(15, 0, 0))
            .selection_bg(Color::rgb(16, 0, 0))
            .selection_fg(Color::rgb(17, 0, 0))
            .scrollbar_track(Color::rgb(18, 0, 0))
            .scrollbar_thumb(Color::rgb(19, 0, 0))
            .build();
        assert_eq!(theme.primary.resolve(true), Color::rgb(1, 0, 0));
        assert_eq!(theme.scrollbar_thumb.resolve(true), Color::rgb(19, 0, 0));
    }

    #[test]
    fn resolved_theme_is_copy() {
        let theme = themes::dark();
        let resolved = theme.resolve(true);
        let copy = resolved;
        assert_eq!(resolved, copy);
    }

    #[test]
    fn detect_dark_mode_with_colorfgbg_dark() {
        // COLORFGBG "0;0" means fg=0 bg=0 (black bg = dark mode)
        let result = Theme::detect_dark_mode_from_colorfgbg(Some("0;0"));
        assert!(result, "bg=0 should be dark mode");
    }

    #[test]
    fn detect_dark_mode_with_colorfgbg_light_15() {
        // COLORFGBG "0;15" means bg=15 (white = light mode)
        let result = Theme::detect_dark_mode_from_colorfgbg(Some("0;15"));
        assert!(!result, "bg=15 should be light mode");
    }

    #[test]
    fn detect_dark_mode_with_colorfgbg_light_7() {
        // COLORFGBG "0;7" means bg=7 (silver = light mode)
        let result = Theme::detect_dark_mode_from_colorfgbg(Some("0;7"));
        assert!(!result, "bg=7 should be light mode");
    }

    #[test]
    fn detect_dark_mode_without_env_defaults_dark() {
        let result = Theme::detect_dark_mode_from_colorfgbg(None);
        assert!(result, "missing COLORFGBG should default to dark");
    }

    #[test]
    fn detect_dark_mode_with_empty_string() {
        let result = Theme::detect_dark_mode_from_colorfgbg(Some(""));
        assert!(result, "empty COLORFGBG should default to dark");
    }

    #[test]
    fn detect_dark_mode_with_no_semicolon() {
        let result = Theme::detect_dark_mode_from_colorfgbg(Some("0"));
        assert!(result, "COLORFGBG without semicolon should default to dark");
    }

    #[test]
    fn detect_dark_mode_with_multiple_semicolons() {
        // Some terminals use "fg;bg;..." format
        let result = Theme::detect_dark_mode_from_colorfgbg(Some("0;0;extra"));
        assert!(result, "COLORFGBG with extra parts should use last as bg");
    }

    #[test]
    fn detect_dark_mode_with_whitespace() {
        let result = Theme::detect_dark_mode_from_colorfgbg(Some("0; 15 "));
        assert!(!result, "COLORFGBG with whitespace should parse correctly");
    }

    #[test]
    fn detect_dark_mode_with_invalid_number() {
        let result = Theme::detect_dark_mode_from_colorfgbg(Some("0;abc"));
        assert!(
            result,
            "COLORFGBG with invalid number should default to dark"
        );
    }

    #[test]
    fn theme_clone_produces_equal_theme() {
        let theme = themes::nord();
        let cloned = theme.clone();
        assert_eq!(theme, cloned);
    }

    #[test]
    fn theme_equality_different_themes() {
        let dark = themes::dark();
        let light = themes::light();
        assert_ne!(dark, light);
    }

    #[test]
    fn resolved_theme_different_modes_differ() {
        // Create a theme with adaptive colors
        let theme = Theme {
            text: AdaptiveColor::adaptive(Color::rgb(0, 0, 0), Color::rgb(255, 255, 255)),
            background: AdaptiveColor::adaptive(Color::rgb(255, 255, 255), Color::rgb(0, 0, 0)),
            ..themes::dark()
        };
        let dark_resolved = theme.resolve(true);
        let light_resolved = theme.resolve(false);
        assert_ne!(dark_resolved, light_resolved);
    }

    #[test]
    fn resolved_theme_equality_same_mode() {
        let theme = themes::dark();
        let resolved1 = theme.resolve(true);
        let resolved2 = theme.resolve(true);
        assert_eq!(resolved1, resolved2);
    }

    #[test]
    fn preset_nord_has_characteristic_colors() {
        let nord = themes::nord();
        // Nord8 frost blue is the primary color
        let primary = nord.primary.resolve(true);
        if let Color::Rgb(rgb) = primary {
            assert!(rgb.b > rgb.r, "Nord primary should be bluish");
        }
    }

    #[test]
    fn preset_dracula_has_characteristic_colors() {
        let dracula = themes::dracula();
        // Dracula primary is purple
        let primary = dracula.primary.resolve(true);
        if let Color::Rgb(rgb) = primary {
            assert!(
                rgb.r > 100 && rgb.b > 200,
                "Dracula primary should be purple"
            );
        }
    }

    #[test]
    fn preset_monokai_has_characteristic_colors() {
        let monokai = themes::monokai();
        // Monokai primary is cyan
        let primary = monokai.primary.resolve(true);
        if let Color::Rgb(rgb) = primary {
            assert!(rgb.g > 200 && rgb.b > 200, "Monokai primary should be cyan");
        }
    }

    #[test]
    fn preset_solarized_dark_and_light_share_accent_colors() {
        let sol_dark = themes::solarized_dark();
        let sol_light = themes::solarized_light();
        // Solarized uses same accent colors in both modes
        assert_eq!(
            sol_dark.primary.resolve(true),
            sol_light.primary.resolve(true),
            "Solarized dark and light should share primary accent"
        );
    }

    #[test]
    fn builder_accepts_adaptive_color_directly() {
        let adaptive = AdaptiveColor::adaptive(Color::rgb(0, 0, 0), Color::rgb(255, 255, 255));
        let theme = Theme::builder().text(adaptive).build();
        assert!(theme.text.is_adaptive());
    }

    #[test]
    fn all_presets_have_distinct_error_colors_from_info() {
        for (name, theme) in [
            ("dark", themes::dark()),
            ("light", themes::light()),
            ("nord", themes::nord()),
            ("dracula", themes::dracula()),
            ("solarized_dark", themes::solarized_dark()),
            ("monokai", themes::monokai()),
        ] {
            let error = theme.error.resolve(true);
            let info = theme.info.resolve(true);
            assert_ne!(
                error, info,
                "{name} should have distinct error and info colors"
            );
        }
    }

    #[test]
    fn adaptive_color_debug_impl() {
        let fixed = AdaptiveColor::fixed(Color::rgb(255, 0, 0));
        let adaptive = AdaptiveColor::adaptive(Color::rgb(0, 0, 0), Color::rgb(255, 255, 255));
        // Just verify Debug doesn't panic
        let _ = format!("{:?}", fixed);
        let _ = format!("{:?}", adaptive);
    }

    #[test]
    fn theme_debug_impl() {
        let theme = themes::dark();
        // Just verify Debug doesn't panic and contains something useful
        let debug = format!("{:?}", theme);
        assert!(debug.contains("Theme"));
    }

    #[test]
    fn resolved_theme_debug_impl() {
        let resolved = themes::dark().resolve(true);
        let debug = format!("{:?}", resolved);
        assert!(debug.contains("ResolvedTheme"));
    }

    // ====== SharedResolvedTheme tests (bd-3l9qr.2) ======

    #[test]
    fn shared_theme_load_returns_initial() {
        let dark = themes::dark().resolve(true);
        let shared = SharedResolvedTheme::new(dark);
        assert_eq!(shared.load(), dark);
    }

    #[test]
    fn shared_theme_store_replaces_value() {
        let original = themes::dark().resolve(true);
        // Build a clearly different theme by mutating a field.
        let mut updated = original;
        updated.primary = Color::rgb(0, 0, 0);
        assert_ne!(original.primary, updated.primary);

        let shared = SharedResolvedTheme::new(original);
        shared.store(updated);
        assert_eq!(shared.load(), updated);
        assert_ne!(shared.load(), original);
    }

    #[test]
    fn shared_theme_concurrent_read_write() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let dark = themes::dark().resolve(true);
        let light = themes::dark().resolve(false);
        let shared = Arc::new(SharedResolvedTheme::new(dark));
        let barrier = Arc::new(Barrier::new(5));

        let readers: Vec<_> = (0..4)
            .map(|_| {
                let s = Arc::clone(&shared);
                let b = Arc::clone(&barrier);
                let dark_copy = dark;
                let light_copy = light;
                thread::spawn(move || {
                    b.wait();
                    for _ in 0..10_000 {
                        let theme = s.load();
                        // Must be one of the two valid themes (no torn reads).
                        assert!(
                            theme == dark_copy || theme == light_copy,
                            "torn read detected"
                        );
                    }
                })
            })
            .collect();

        let writer = {
            let s = Arc::clone(&shared);
            let b = Arc::clone(&barrier);
            thread::spawn(move || {
                b.wait();
                for i in 0..1_000 {
                    if i % 2 == 0 {
                        s.store(light);
                    } else {
                        s.store(dark);
                    }
                }
            })
        };

        writer.join().unwrap();
        for h in readers {
            h.join().unwrap();
        }
    }
}
