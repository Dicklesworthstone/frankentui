#![forbid(unsafe_code)]

//! Theme system with built-in palettes and dynamic theme selection.
//!
//! This module provides a small set of coherent, high-contrast themes and
//! color tokens that resolve against the current theme at runtime.

use std::cell::Cell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

// Thread-local flag to track if current thread holds THEME_TEST_LOCK.
// Used for reentrant-style locking in set_theme() when called from within ScopedThemeLock.
thread_local! {
    static THEME_LOCK_HELD: Cell<bool> = const { Cell::new(false) };
}

#[cfg(feature = "syntax")]
use crate::syntax::HighlightTheme;
use ftui_render::cell::PackedRgba;
use ftui_style::Style;

/// Built-in theme identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemeId {
    /// Cyberpunk Aurora / Doodlestein Punk (default).
    CyberpunkAurora,
    /// JetBrains Darcula-inspired dark theme.
    Darcula,
    /// Sleek, modern light theme.
    LumenLight,
    /// Nordic-inspired low-contrast dark theme.
    NordicFrost,
    /// High contrast accessibility theme.
    HighContrast,
}

impl ThemeId {
    pub const ALL: [ThemeId; 5] = [
        ThemeId::CyberpunkAurora,
        ThemeId::Darcula,
        ThemeId::LumenLight,
        ThemeId::NordicFrost,
        ThemeId::HighContrast,
    ];

    /// Themes suitable for normal use (excludes accessibility-only themes).
    pub const STANDARD: [ThemeId; 4] = [
        ThemeId::CyberpunkAurora,
        ThemeId::Darcula,
        ThemeId::LumenLight,
        ThemeId::NordicFrost,
    ];

    pub const fn index(self) -> usize {
        match self {
            ThemeId::CyberpunkAurora => 0,
            ThemeId::Darcula => 1,
            ThemeId::LumenLight => 2,
            ThemeId::NordicFrost => 3,
            ThemeId::HighContrast => 4,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            ThemeId::CyberpunkAurora => "Cyberpunk Aurora",
            ThemeId::Darcula => "Darcula",
            ThemeId::LumenLight => "Lumen Light",
            ThemeId::NordicFrost => "Nordic Frost",
            ThemeId::HighContrast => "High Contrast",
        }
    }

    pub const fn next(self) -> Self {
        let idx = (self.index() + 1) % Self::ALL.len();
        Self::ALL[idx]
    }

    /// Get the next theme in the standard rotation (skipping accessibility-only themes).
    pub const fn next_non_accessibility(self) -> Self {
        let current_standard_idx = match self {
            ThemeId::CyberpunkAurora => 0,
            ThemeId::Darcula => 1,
            ThemeId::LumenLight => 2,
            ThemeId::NordicFrost => 3,
            ThemeId::HighContrast => 0, // HighContrast -> CyberpunkAurora
        };
        let next_idx = (current_standard_idx + 1) % Self::STANDARD.len();
        Self::STANDARD[next_idx]
    }

    pub const fn from_index(idx: usize) -> Self {
        Self::ALL[idx % Self::ALL.len()]
    }
}

/// Theme palette with semantic slots used throughout the UI.
#[derive(Debug, Clone, Copy)]
pub struct ThemePalette {
    pub bg_deep: PackedRgba,
    pub bg_base: PackedRgba,
    pub bg_surface: PackedRgba,
    pub bg_overlay: PackedRgba,
    pub bg_highlight: PackedRgba,
    pub fg_primary: PackedRgba,
    pub fg_secondary: PackedRgba,
    pub fg_muted: PackedRgba,
    pub fg_disabled: PackedRgba,
    pub accent_primary: PackedRgba,
    pub accent_secondary: PackedRgba,
    pub accent_success: PackedRgba,
    pub accent_warning: PackedRgba,
    pub accent_error: PackedRgba,
    pub accent_info: PackedRgba,
    pub accent_link: PackedRgba,
    pub accent_slots: [PackedRgba; 12],
    pub syntax_keyword: PackedRgba,
    pub syntax_string: PackedRgba,
    pub syntax_number: PackedRgba,
    pub syntax_comment: PackedRgba,
    pub syntax_function: PackedRgba,
    pub syntax_type: PackedRgba,
    pub syntax_operator: PackedRgba,
    pub syntax_punctuation: PackedRgba,
}

const THEMES: [ThemePalette; 5] = [
    ThemePalette {
        bg_deep: PackedRgba::rgb(10, 14, 20),
        bg_base: PackedRgba::rgb(26, 31, 41),
        bg_surface: PackedRgba::rgb(30, 36, 48),
        bg_overlay: PackedRgba::rgb(45, 55, 70),
        bg_highlight: PackedRgba::rgb(61, 79, 95),
        fg_primary: PackedRgba::rgb(179, 244, 255),
        fg_secondary: PackedRgba::rgb(199, 213, 224),
        fg_muted: PackedRgba::rgb(140, 160, 180),
        fg_disabled: PackedRgba::rgb(61, 79, 95),
        accent_primary: PackedRgba::rgb(0, 170, 255),
        accent_secondary: PackedRgba::rgb(255, 0, 255),
        accent_success: PackedRgba::rgb(110, 255, 200),
        accent_warning: PackedRgba::rgb(255, 229, 102),
        accent_error: PackedRgba::rgb(255, 60, 110),
        accent_info: PackedRgba::rgb(0, 255, 255),
        accent_link: PackedRgba::rgb(102, 204, 255),
        accent_slots: [
            PackedRgba::rgb(0, 170, 255),
            PackedRgba::rgb(255, 0, 255),
            PackedRgba::rgb(110, 255, 200),
            PackedRgba::rgb(255, 229, 102),
            PackedRgba::rgb(255, 60, 110),
            PackedRgba::rgb(0, 255, 255),
            PackedRgba::rgb(102, 204, 255),
            PackedRgba::rgb(255, 130, 175),
            PackedRgba::rgb(107, 255, 205),
            PackedRgba::rgb(255, 239, 153),
            PackedRgba::rgb(102, 255, 255),
            PackedRgba::rgb(255, 102, 255),
        ],
        syntax_keyword: PackedRgba::rgb(255, 102, 255),
        syntax_string: PackedRgba::rgb(110, 255, 200),
        syntax_number: PackedRgba::rgb(255, 229, 102),
        syntax_comment: PackedRgba::rgb(61, 79, 95),
        syntax_function: PackedRgba::rgb(0, 170, 255),
        syntax_type: PackedRgba::rgb(102, 255, 255),
        syntax_operator: PackedRgba::rgb(199, 213, 224),
        syntax_punctuation: PackedRgba::rgb(140, 160, 180),
    },
    ThemePalette {
        bg_deep: PackedRgba::rgb(43, 43, 43),
        bg_base: PackedRgba::rgb(50, 50, 50),
        bg_surface: PackedRgba::rgb(60, 63, 65),
        bg_overlay: PackedRgba::rgb(75, 80, 82),
        bg_highlight: PackedRgba::rgb(90, 96, 98),
        fg_primary: PackedRgba::rgb(169, 183, 198),
        fg_secondary: PackedRgba::rgb(146, 161, 177),
        fg_muted: PackedRgba::rgb(118, 132, 147), // Brightened for WCAG 3:1
        fg_disabled: PackedRgba::rgb(85, 90, 92),
        accent_primary: PackedRgba::rgb(104, 151, 187),
        accent_secondary: PackedRgba::rgb(152, 118, 170),
        accent_success: PackedRgba::rgb(130, 180, 110), // Brightened for WCAG 4.5:1
        accent_warning: PackedRgba::rgb(255, 198, 109),
        accent_error: PackedRgba::rgb(255, 115, 115), // Brightened for WCAG 4.5:1
        accent_info: PackedRgba::rgb(179, 212, 252),
        accent_link: PackedRgba::rgb(74, 136, 199),
        accent_slots: [
            PackedRgba::rgb(104, 151, 187),
            PackedRgba::rgb(152, 118, 170),
            PackedRgba::rgb(130, 180, 110), // Updated to match accent_success
            PackedRgba::rgb(255, 198, 109),
            PackedRgba::rgb(204, 120, 50),
            PackedRgba::rgb(191, 97, 106),
            PackedRgba::rgb(187, 181, 41),
            PackedRgba::rgb(100, 150, 180), // Brightened for WCAG 3:1 (LAYOUT_LAB)
            PackedRgba::rgb(149, 102, 71),
            PackedRgba::rgb(134, 138, 147),
            PackedRgba::rgb(161, 99, 158),
            PackedRgba::rgb(127, 140, 141),
        ],
        syntax_keyword: PackedRgba::rgb(204, 120, 50),
        syntax_string: PackedRgba::rgb(106, 135, 89),
        syntax_number: PackedRgba::rgb(104, 151, 187),
        syntax_comment: PackedRgba::rgb(128, 128, 128),
        syntax_function: PackedRgba::rgb(255, 198, 109),
        syntax_type: PackedRgba::rgb(152, 118, 170),
        syntax_operator: PackedRgba::rgb(169, 183, 198),
        syntax_punctuation: PackedRgba::rgb(134, 138, 147),
    },
    ThemePalette {
        bg_deep: PackedRgba::rgb(248, 249, 251),
        bg_base: PackedRgba::rgb(238, 241, 245),
        bg_surface: PackedRgba::rgb(230, 235, 241),
        bg_overlay: PackedRgba::rgb(220, 227, 236),
        bg_highlight: PackedRgba::rgb(208, 217, 228),
        fg_primary: PackedRgba::rgb(31, 41, 51),
        fg_secondary: PackedRgba::rgb(62, 76, 89),
        fg_muted: PackedRgba::rgb(123, 135, 148),
        fg_disabled: PackedRgba::rgb(160, 172, 181),
        accent_primary: PackedRgba::rgb(37, 99, 235),
        accent_secondary: PackedRgba::rgb(124, 58, 237),
        accent_success: PackedRgba::rgb(15, 120, 55), // Darkened for WCAG 4.5:1 on light bg
        accent_warning: PackedRgba::rgb(180, 83, 9),  // Darkened for WCAG 3:1 on light bg
        accent_error: PackedRgba::rgb(185, 28, 28),   // Darkened for WCAG 4.5:1 on light bg
        accent_info: PackedRgba::rgb(2, 132, 199),
        accent_link: PackedRgba::rgb(37, 99, 235),
        accent_slots: [
            PackedRgba::rgb(37, 99, 235),
            PackedRgba::rgb(124, 58, 237),
            PackedRgba::rgb(15, 120, 55), // Updated to match accent_success
            PackedRgba::rgb(180, 83, 9),  // Updated to match accent_warning
            PackedRgba::rgb(185, 28, 28), // Updated to match accent_error
            PackedRgba::rgb(2, 132, 199),
            PackedRgba::rgb(13, 148, 136), // Darkened teal for WCAG
            PackedRgba::rgb(190, 24, 93),  // Darkened pink for WCAG
            PackedRgba::rgb(79, 70, 229),  // Darkened indigo for WCAG
            PackedRgba::rgb(194, 65, 12),  // Darkened orange for WCAG
            PackedRgba::rgb(13, 148, 136), // Darkened emerald for WCAG
            PackedRgba::rgb(126, 34, 206), // Darkened purple for WCAG
        ],
        syntax_keyword: PackedRgba::rgb(124, 58, 237),
        syntax_string: PackedRgba::rgb(15, 120, 55), // Updated to match accent_success
        syntax_number: PackedRgba::rgb(217, 119, 6),
        syntax_comment: PackedRgba::rgb(154, 165, 177),
        syntax_function: PackedRgba::rgb(37, 99, 235),
        syntax_type: PackedRgba::rgb(14, 165, 233),
        syntax_operator: PackedRgba::rgb(71, 85, 105),
        syntax_punctuation: PackedRgba::rgb(100, 116, 139),
    },
    ThemePalette {
        bg_deep: PackedRgba::rgb(46, 52, 64),
        bg_base: PackedRgba::rgb(59, 66, 82),
        bg_surface: PackedRgba::rgb(67, 76, 94),
        bg_overlay: PackedRgba::rgb(76, 86, 106),
        bg_highlight: PackedRgba::rgb(94, 129, 172),
        fg_primary: PackedRgba::rgb(236, 239, 244),
        fg_secondary: PackedRgba::rgb(216, 222, 233),
        fg_muted: PackedRgba::rgb(163, 179, 194),
        fg_disabled: PackedRgba::rgb(123, 135, 148),
        accent_primary: PackedRgba::rgb(136, 192, 208),
        accent_secondary: PackedRgba::rgb(129, 161, 193),
        accent_success: PackedRgba::rgb(163, 190, 140),
        accent_warning: PackedRgba::rgb(235, 203, 139),
        accent_error: PackedRgba::rgb(240, 150, 160), // Brightened for WCAG 4.5:1
        accent_info: PackedRgba::rgb(143, 188, 187),
        accent_link: PackedRgba::rgb(136, 192, 208),
        accent_slots: [
            PackedRgba::rgb(136, 192, 208),
            PackedRgba::rgb(129, 161, 193),
            PackedRgba::rgb(163, 190, 140),
            PackedRgba::rgb(235, 203, 139),
            PackedRgba::rgb(240, 150, 160), // Updated to match accent_error
            PackedRgba::rgb(143, 188, 187),
            PackedRgba::rgb(180, 142, 173),
            PackedRgba::rgb(94, 129, 172),
            PackedRgba::rgb(208, 135, 112),
            PackedRgba::rgb(229, 233, 240),
            PackedRgba::rgb(216, 222, 233),
            PackedRgba::rgb(143, 188, 187),
        ],
        syntax_keyword: PackedRgba::rgb(129, 161, 193),
        syntax_string: PackedRgba::rgb(163, 190, 140),
        syntax_number: PackedRgba::rgb(180, 142, 173),
        syntax_comment: PackedRgba::rgb(97, 110, 136),
        syntax_function: PackedRgba::rgb(136, 192, 208),
        syntax_type: PackedRgba::rgb(143, 188, 187),
        syntax_operator: PackedRgba::rgb(216, 222, 233),
        syntax_punctuation: PackedRgba::rgb(229, 233, 240),
    },
    // High Contrast accessibility theme (WCAG AAA compliant)
    ThemePalette {
        bg_deep: PackedRgba::rgb(0, 0, 0),
        bg_base: PackedRgba::rgb(0, 0, 0),
        bg_surface: PackedRgba::rgb(20, 20, 20),
        bg_overlay: PackedRgba::rgb(40, 40, 40),
        bg_highlight: PackedRgba::rgb(80, 80, 80),
        fg_primary: PackedRgba::rgb(255, 255, 255),
        fg_secondary: PackedRgba::rgb(230, 230, 230),
        fg_muted: PackedRgba::rgb(180, 180, 180),
        fg_disabled: PackedRgba::rgb(120, 120, 120),
        accent_primary: PackedRgba::rgb(0, 255, 255),
        accent_secondary: PackedRgba::rgb(255, 255, 0),
        accent_success: PackedRgba::rgb(0, 255, 0),
        accent_warning: PackedRgba::rgb(255, 255, 0),
        accent_error: PackedRgba::rgb(255, 100, 100),
        accent_info: PackedRgba::rgb(100, 200, 255),
        accent_link: PackedRgba::rgb(100, 200, 255),
        accent_slots: [
            PackedRgba::rgb(0, 255, 255),
            PackedRgba::rgb(255, 255, 0),
            PackedRgba::rgb(0, 255, 0),
            PackedRgba::rgb(255, 165, 0),
            PackedRgba::rgb(255, 100, 100),
            PackedRgba::rgb(100, 200, 255),
            PackedRgba::rgb(255, 0, 255),
            PackedRgba::rgb(0, 255, 128),
            PackedRgba::rgb(255, 128, 0),
            PackedRgba::rgb(128, 255, 255),
            PackedRgba::rgb(255, 128, 255),
            PackedRgba::rgb(128, 255, 0),
        ],
        syntax_keyword: PackedRgba::rgb(255, 255, 0),
        syntax_string: PackedRgba::rgb(0, 255, 0),
        syntax_number: PackedRgba::rgb(255, 165, 0),
        syntax_comment: PackedRgba::rgb(128, 128, 128),
        syntax_function: PackedRgba::rgb(0, 255, 255),
        syntax_type: PackedRgba::rgb(255, 0, 255),
        syntax_operator: PackedRgba::rgb(255, 255, 255),
        syntax_punctuation: PackedRgba::rgb(200, 200, 200),
    },
];

static CURRENT_THEME: AtomicUsize = AtomicUsize::new(0);

/// Internal: set theme without acquiring the lock.
/// Used by `ScopedThemeLock::new()` which already holds the lock.
fn set_theme_internal(theme: ThemeId) {
    CURRENT_THEME.store(theme.index(), Ordering::Relaxed);
}

/// Set the active theme.
///
/// Acquires `THEME_TEST_LOCK` to serialize with parallel tests. If the current
/// thread already holds the lock (via `ScopedThemeLock`), sets the theme directly.
pub fn set_theme(theme: ThemeId) {
    // Check if current thread already holds the lock (reentrant case from ScopedThemeLock)
    let held = THEME_LOCK_HELD.with(|h| h.get());
    if held {
        // Current thread holds lock, set directly without re-acquiring
        set_theme_internal(theme);
    } else {
        // Acquire lock to serialize with other threads
        let _guard = THEME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_theme_internal(theme);
    }
}

/// Get the active theme.
pub fn current_theme() -> ThemeId {
    ThemeId::from_index(CURRENT_THEME.load(Ordering::Relaxed))
}

/// Get the active theme name.
pub fn current_theme_name() -> &'static str {
    current_theme().name()
}

/// Cycle to the next theme.
pub fn cycle_theme() -> ThemeId {
    let next = current_theme().next();
    set_theme(next);
    next
}

/// Return the palette for a theme.
pub fn palette(theme: ThemeId) -> &'static ThemePalette {
    &THEMES[theme.index()]
}

/// Return the current palette.
pub fn current_palette() -> &'static ThemePalette {
    palette(current_theme())
}

/// Return the total number of themes.
pub const fn theme_count() -> usize {
    ThemeId::ALL.len()
}

/// Mutex for serializing theme access in tests.
///
/// Used by `ScopedThemeLock` to prevent race conditions when multiple
/// tests set different themes concurrently.
static THEME_TEST_LOCK: Mutex<()> = Mutex::new(());

/// RAII guard for exclusive theme access during tests.
///
/// Acquires `THEME_TEST_LOCK`, sets the specified theme, and releases
/// the lock when dropped. This prevents race conditions in parallel tests
/// that read from the global theme state.
///
/// # Example
///
/// ```ignore
/// let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
/// // Theme is now CyberpunkAurora and other tests cannot change it
/// let checksum = render_to_checksum(&frame);
/// // Lock released when _guard is dropped
/// ```
pub struct ScopedThemeLock<'a> {
    _guard: MutexGuard<'a, ()>,
}

impl<'a> ScopedThemeLock<'a> {
    /// Create a new scoped theme lock, setting the specified theme.
    ///
    /// Blocks until the lock can be acquired. While held, other threads'
    /// calls to `set_theme()` or `ScopedThemeLock::new()` will block.
    #[must_use]
    pub fn new(theme: ThemeId) -> Self {
        let guard = THEME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Mark that current thread holds the lock (for reentrant set_theme calls)
        THEME_LOCK_HELD.with(|h| h.set(true));
        set_theme_internal(theme);
        Self { _guard: guard }
    }
}

impl Drop for ScopedThemeLock<'_> {
    fn drop(&mut self) {
        // Clear the thread-local flag when releasing the lock
        THEME_LOCK_HELD.with(|h| h.set(false));
    }
}

/// Token that resolves to a theme color at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorToken {
    BgDeep,
    BgBase,
    BgSurface,
    BgOverlay,
    BgHighlight,
    FgPrimary,
    FgSecondary,
    FgMuted,
    FgDisabled,
    AccentPrimary,
    AccentSecondary,
    AccentSuccess,
    AccentWarning,
    AccentError,
    AccentInfo,
    AccentLink,
    AccentSlot(usize),
    SyntaxKeyword,
    SyntaxString,
    SyntaxNumber,
    SyntaxComment,
    SyntaxFunction,
    SyntaxType,
    SyntaxOperator,
    SyntaxPunctuation,
    // Semantic colors (status / priority / issue type)
    StatusOpen,
    StatusInProgress,
    StatusBlocked,
    StatusClosed,
    PriorityP0,
    PriorityP1,
    PriorityP2,
    PriorityP3,
    PriorityP4,
    IssueBug,
    IssueFeature,
    IssueTask,
    IssueEpic,
}

impl ColorToken {
    pub fn resolve_in(self, palette: &ThemePalette) -> PackedRgba {
        match self {
            ColorToken::BgDeep => palette.bg_deep,
            ColorToken::BgBase => palette.bg_base,
            ColorToken::BgSurface => palette.bg_surface,
            ColorToken::BgOverlay => palette.bg_overlay,
            ColorToken::BgHighlight => palette.bg_highlight,
            ColorToken::FgPrimary => palette.fg_primary,
            ColorToken::FgSecondary => palette.fg_secondary,
            ColorToken::FgMuted => palette.fg_muted,
            ColorToken::FgDisabled => palette.fg_disabled,
            ColorToken::AccentPrimary => palette.accent_primary,
            ColorToken::AccentSecondary => palette.accent_secondary,
            ColorToken::AccentSuccess => palette.accent_success,
            ColorToken::AccentWarning => palette.accent_warning,
            ColorToken::AccentError => palette.accent_error,
            ColorToken::AccentInfo => palette.accent_info,
            ColorToken::AccentLink => palette.accent_link,
            ColorToken::AccentSlot(idx) => palette.accent_slots[idx % palette.accent_slots.len()],
            ColorToken::SyntaxKeyword => palette.syntax_keyword,
            ColorToken::SyntaxString => palette.syntax_string,
            ColorToken::SyntaxNumber => palette.syntax_number,
            ColorToken::SyntaxComment => palette.syntax_comment,
            ColorToken::SyntaxFunction => palette.syntax_function,
            ColorToken::SyntaxType => palette.syntax_type,
            ColorToken::SyntaxOperator => palette.syntax_operator,
            ColorToken::SyntaxPunctuation => palette.syntax_punctuation,
            ColorToken::StatusOpen => ensure_contrast(
                palette.accent_success,
                palette.bg_base,
                palette.fg_primary,
                palette.bg_deep,
            ),
            ColorToken::StatusInProgress => ensure_contrast(
                palette.accent_info,
                palette.bg_base,
                palette.fg_primary,
                palette.bg_deep,
            ),
            ColorToken::StatusBlocked => ensure_contrast(
                palette.accent_error,
                palette.bg_base,
                palette.fg_primary,
                palette.bg_deep,
            ),
            ColorToken::StatusClosed => ensure_contrast(
                palette.fg_muted,
                palette.bg_base,
                palette.fg_primary,
                palette.bg_deep,
            ),
            ColorToken::PriorityP0 => palette.accent_error,
            ColorToken::PriorityP1 => {
                blend_colors(palette.accent_warning, palette.accent_error, 0.6)
            }
            ColorToken::PriorityP2 => palette.accent_warning,
            ColorToken::PriorityP3 => palette.accent_info,
            ColorToken::PriorityP4 => palette.fg_muted,
            ColorToken::IssueBug => palette.accent_error,
            ColorToken::IssueFeature => palette.accent_secondary,
            ColorToken::IssueTask => palette.accent_primary,
            ColorToken::IssueEpic => palette.accent_warning,
        }
    }

    pub fn resolve(self) -> PackedRgba {
        self.resolve_in(current_palette())
    }
}

impl From<ColorToken> for PackedRgba {
    fn from(token: ColorToken) -> Self {
        token.resolve()
    }
}

/// A theme color with explicit alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AlphaColor {
    base: ColorToken,
    alpha: u8,
}

impl AlphaColor {
    pub const fn new(base: ColorToken, alpha: u8) -> Self {
        Self { base, alpha }
    }

    pub fn resolve(self) -> PackedRgba {
        let base = self.base.resolve();
        PackedRgba::rgba(base.r(), base.g(), base.b(), self.alpha)
    }
}

impl From<AlphaColor> for PackedRgba {
    fn from(token: AlphaColor) -> Self {
        token.resolve()
    }
}

/// Apply an explicit alpha to a theme token.
pub fn with_alpha(token: ColorToken, alpha: u8) -> PackedRgba {
    AlphaColor::new(token, alpha).resolve()
}

/// Apply a floating opacity to a theme token.
pub fn with_opacity(token: ColorToken, opacity: f32) -> PackedRgba {
    token.resolve().with_opacity(opacity)
}

/// Blend a themed overlay over a base color using source-over.
pub fn blend_over(overlay: ColorToken, base: ColorToken, opacity: f32) -> PackedRgba {
    overlay.resolve().with_opacity(opacity).over(base.resolve())
}

/// Blend raw colors using source-over.
pub fn blend_colors(overlay: PackedRgba, base: PackedRgba, opacity: f32) -> PackedRgba {
    overlay.with_opacity(opacity).over(base)
}

/// Sample a smooth, repeating gradient over the current theme's accent slots.
///
/// `t` is periodic with period 1.0 (i.e., `t=0.0` and `t=1.0` return the same color).
///
/// This is intended for visual polish (sparklines, animated accents, demo effects) while
/// staying coherent with the active theme.
pub fn accent_gradient(t: f64) -> PackedRgba {
    let slots = &current_palette().accent_slots;
    let t = t.rem_euclid(1.0);
    let t = t.clamp(0.0, 1.0);
    if slots.is_empty() {
        return accent::PRIMARY.resolve();
    }

    if slots.len() == 1 {
        return slots[0];
    }

    let max_idx = slots.len() - 1;
    let pos = t * max_idx as f64;
    let idx = (pos.floor() as usize).min(max_idx);
    let frac = pos - idx as f64;

    let a = slots[idx];
    let b = slots[(idx + 1).min(max_idx)];

    let r = (a.r() as f64 + (b.r() as f64 - a.r() as f64) * frac).round() as u8;
    let g = (a.g() as f64 + (b.g() as f64 - a.g() as f64) * frac).round() as u8;
    let b_val = (a.b() as f64 + (b.b() as f64 - a.b() as f64) * frac).round() as u8;
    PackedRgba::rgb(r, g, b_val)
}

fn ensure_contrast(
    fg: PackedRgba,
    bg: PackedRgba,
    light: PackedRgba,
    dark: PackedRgba,
) -> PackedRgba {
    let (light, dark) = if contrast::relative_luminance(light) >= contrast::relative_luminance(dark)
    {
        (light, dark)
    } else {
        (dark, light)
    };

    if contrast::meets_wcag_aa(fg, bg) {
        return fg;
    }

    let target = if contrast::relative_luminance(bg) < 0.5 {
        light
    } else {
        dark
    };

    let mut best = fg;
    let mut best_ratio = contrast::contrast_ratio(fg, bg);
    for step in 1..=10 {
        let t = step as f32 / 10.0;
        let candidate = blend_colors(target, fg, t);
        let ratio = contrast::contrast_ratio(candidate, bg);
        if ratio > best_ratio {
            best = candidate;
            best_ratio = ratio;
        }
        if ratio >= 4.5 {
            return candidate;
        }
    }

    best
}

const SEMANTIC_TINT_OPACITY: f32 = 0.18;

fn semantic_tint(token: ColorToken) -> PackedRgba {
    with_opacity(token, SEMANTIC_TINT_OPACITY)
}

fn semantic_text(token: ColorToken) -> PackedRgba {
    let base_bg = bg::BASE.resolve();
    let tint = semantic_tint(token);
    let composed = tint.over(base_bg);
    let candidates = [
        fg::PRIMARY.resolve(),
        fg::SECONDARY.resolve(),
        bg::DEEP.resolve(),
        PackedRgba::WHITE,
        PackedRgba::BLACK,
    ];
    let best = contrast::best_text_color(composed, &candidates);
    // Ensure WCAG AA compliance - fall back to black or white if needed
    if contrast::meets_wcag_aa(best, composed) {
        best
    } else if contrast::contrast_ratio(PackedRgba::BLACK, composed)
        >= contrast::contrast_ratio(PackedRgba::WHITE, composed)
    {
        PackedRgba::BLACK
    } else {
        PackedRgba::WHITE
    }
}

/// Background colors.
pub mod bg {
    use super::ColorToken;

    pub const DEEP: ColorToken = ColorToken::BgDeep;
    pub const BASE: ColorToken = ColorToken::BgBase;
    pub const SURFACE: ColorToken = ColorToken::BgSurface;
    pub const OVERLAY: ColorToken = ColorToken::BgOverlay;
    pub const HIGHLIGHT: ColorToken = ColorToken::BgHighlight;
}

/// Foreground / text colors.
pub mod fg {
    use super::ColorToken;

    pub const PRIMARY: ColorToken = ColorToken::FgPrimary;
    pub const SECONDARY: ColorToken = ColorToken::FgSecondary;
    pub const MUTED: ColorToken = ColorToken::FgMuted;
    pub const DISABLED: ColorToken = ColorToken::FgDisabled;
}

/// Accent / semantic colors.
pub mod accent {
    use super::ColorToken;

    pub const PRIMARY: ColorToken = ColorToken::AccentPrimary;
    pub const SECONDARY: ColorToken = ColorToken::AccentSecondary;
    pub const SUCCESS: ColorToken = ColorToken::AccentSuccess;
    pub const WARNING: ColorToken = ColorToken::AccentWarning;
    pub const ERROR: ColorToken = ColorToken::AccentError;
    pub const INFO: ColorToken = ColorToken::AccentInfo;
    pub const LINK: ColorToken = ColorToken::AccentLink;

    pub const ACCENT_1: ColorToken = ColorToken::AccentSlot(0);
    pub const ACCENT_2: ColorToken = ColorToken::AccentSlot(1);
    pub const ACCENT_3: ColorToken = ColorToken::AccentSlot(2);
    pub const ACCENT_4: ColorToken = ColorToken::AccentSlot(3);
    pub const ACCENT_5: ColorToken = ColorToken::AccentSlot(4);
    pub const ACCENT_6: ColorToken = ColorToken::AccentSlot(5);
    pub const ACCENT_7: ColorToken = ColorToken::AccentSlot(6);
    pub const ACCENT_8: ColorToken = ColorToken::AccentSlot(7);
    pub const ACCENT_9: ColorToken = ColorToken::AccentSlot(8);
    pub const ACCENT_10: ColorToken = ColorToken::AccentSlot(9);
    pub const ACCENT_11: ColorToken = ColorToken::AccentSlot(10);
    pub const ACCENT_12: ColorToken = ColorToken::AccentSlot(11);
}

/// Status colors (open / in-progress / blocked / closed).
pub mod status {
    use super::{ColorToken, PackedRgba, semantic_text, semantic_tint};

    pub const OPEN: ColorToken = ColorToken::StatusOpen;
    pub const IN_PROGRESS: ColorToken = ColorToken::StatusInProgress;
    pub const BLOCKED: ColorToken = ColorToken::StatusBlocked;
    pub const CLOSED: ColorToken = ColorToken::StatusClosed;

    pub fn open_bg() -> PackedRgba {
        semantic_tint(OPEN)
    }

    pub fn in_progress_bg() -> PackedRgba {
        semantic_tint(IN_PROGRESS)
    }

    pub fn blocked_bg() -> PackedRgba {
        semantic_tint(BLOCKED)
    }

    pub fn closed_bg() -> PackedRgba {
        semantic_tint(CLOSED)
    }

    pub fn open_text() -> PackedRgba {
        semantic_text(OPEN)
    }

    pub fn in_progress_text() -> PackedRgba {
        semantic_text(IN_PROGRESS)
    }

    pub fn blocked_text() -> PackedRgba {
        semantic_text(BLOCKED)
    }

    pub fn closed_text() -> PackedRgba {
        semantic_text(CLOSED)
    }
}

/// Priority colors (P0-P4).
pub mod priority {
    use super::{ColorToken, PackedRgba, semantic_text, semantic_tint};

    pub const P0: ColorToken = ColorToken::PriorityP0;
    pub const P1: ColorToken = ColorToken::PriorityP1;
    pub const P2: ColorToken = ColorToken::PriorityP2;
    pub const P3: ColorToken = ColorToken::PriorityP3;
    pub const P4: ColorToken = ColorToken::PriorityP4;

    pub fn p0_bg() -> PackedRgba {
        semantic_tint(P0)
    }

    pub fn p1_bg() -> PackedRgba {
        semantic_tint(P1)
    }

    pub fn p2_bg() -> PackedRgba {
        semantic_tint(P2)
    }

    pub fn p3_bg() -> PackedRgba {
        semantic_tint(P3)
    }

    pub fn p4_bg() -> PackedRgba {
        semantic_tint(P4)
    }

    pub fn p0_text() -> PackedRgba {
        semantic_text(P0)
    }

    pub fn p1_text() -> PackedRgba {
        semantic_text(P1)
    }

    pub fn p2_text() -> PackedRgba {
        semantic_text(P2)
    }

    pub fn p3_text() -> PackedRgba {
        semantic_text(P3)
    }

    pub fn p4_text() -> PackedRgba {
        semantic_text(P4)
    }
}

/// Issue type colors (bug / feature / task / epic).
pub mod issue_type {
    use super::{ColorToken, PackedRgba, semantic_text, semantic_tint};

    pub const BUG: ColorToken = ColorToken::IssueBug;
    pub const FEATURE: ColorToken = ColorToken::IssueFeature;
    pub const TASK: ColorToken = ColorToken::IssueTask;
    pub const EPIC: ColorToken = ColorToken::IssueEpic;

    pub fn bug_bg() -> PackedRgba {
        semantic_tint(BUG)
    }

    pub fn feature_bg() -> PackedRgba {
        semantic_tint(FEATURE)
    }

    pub fn task_bg() -> PackedRgba {
        semantic_tint(TASK)
    }

    pub fn epic_bg() -> PackedRgba {
        semantic_tint(EPIC)
    }

    pub fn bug_text() -> PackedRgba {
        semantic_text(BUG)
    }

    pub fn feature_text() -> PackedRgba {
        semantic_text(FEATURE)
    }

    pub fn task_text() -> PackedRgba {
        semantic_text(TASK)
    }

    pub fn epic_text() -> PackedRgba {
        semantic_text(EPIC)
    }
}

/// Intent colors (success / warning / info / error).
pub mod intent {
    use super::{ColorToken, PackedRgba, accent, semantic_text, semantic_tint};

    pub const SUCCESS: ColorToken = accent::SUCCESS;
    pub const WARNING: ColorToken = accent::WARNING;
    pub const INFO: ColorToken = accent::INFO;
    pub const ERROR: ColorToken = accent::ERROR;

    pub fn success_bg() -> PackedRgba {
        semantic_tint(SUCCESS)
    }

    pub fn warning_bg() -> PackedRgba {
        semantic_tint(WARNING)
    }

    pub fn info_bg() -> PackedRgba {
        semantic_tint(INFO)
    }

    pub fn error_bg() -> PackedRgba {
        semantic_tint(ERROR)
    }

    pub fn success_text() -> PackedRgba {
        semantic_text(SUCCESS)
    }

    pub fn warning_text() -> PackedRgba {
        semantic_text(WARNING)
    }

    pub fn info_text() -> PackedRgba {
        semantic_text(INFO)
    }

    pub fn error_text() -> PackedRgba {
        semantic_text(ERROR)
    }
}

/// Alpha-aware overlay colors.
pub mod alpha {
    use super::{AlphaColor, accent, bg};

    pub const SURFACE: AlphaColor = AlphaColor::new(bg::SURFACE, 220);
    pub const OVERLAY: AlphaColor = AlphaColor::new(bg::OVERLAY, 210);
    pub const HIGHLIGHT: AlphaColor = AlphaColor::new(bg::HIGHLIGHT, 200);

    pub const ACCENT_PRIMARY: AlphaColor = AlphaColor::new(accent::PRIMARY, 210);
    pub const ACCENT_SECONDARY: AlphaColor = AlphaColor::new(accent::SECONDARY, 200);
}

/// Syntax highlighting colors.
pub mod syntax {
    use super::ColorToken;

    pub const KEYWORD: ColorToken = ColorToken::SyntaxKeyword;
    pub const STRING: ColorToken = ColorToken::SyntaxString;
    pub const NUMBER: ColorToken = ColorToken::SyntaxNumber;
    pub const COMMENT: ColorToken = ColorToken::SyntaxComment;
    pub const FUNCTION: ColorToken = ColorToken::SyntaxFunction;
    pub const TYPE: ColorToken = ColorToken::SyntaxType;
    pub const OPERATOR: ColorToken = ColorToken::SyntaxOperator;
    pub const PUNCTUATION: ColorToken = ColorToken::SyntaxPunctuation;
}

/// Contrast utilities (WCAG AA).
pub mod contrast {
    use super::PackedRgba;

    const WCAG_AA_CONTRAST: f64 = 4.5;

    pub fn srgb_to_linear(c: f64) -> f64 {
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    pub fn relative_luminance(color: PackedRgba) -> f64 {
        let r = srgb_to_linear(color.r() as f64 / 255.0);
        let g = srgb_to_linear(color.g() as f64 / 255.0);
        let b = srgb_to_linear(color.b() as f64 / 255.0);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    pub fn contrast_ratio(fg: PackedRgba, bg: PackedRgba) -> f64 {
        let lum_fg = relative_luminance(fg);
        let lum_bg = relative_luminance(bg);
        let lighter = lum_fg.max(lum_bg);
        let darker = lum_fg.min(lum_bg);
        (lighter + 0.05) / (darker + 0.05)
    }

    pub fn meets_wcag_aa(fg: PackedRgba, bg: PackedRgba) -> bool {
        contrast_ratio(fg, bg) >= WCAG_AA_CONTRAST
    }

    pub fn best_text_color(bg: PackedRgba, candidates: &[PackedRgba]) -> PackedRgba {
        let mut best = candidates[0];
        let mut best_ratio = contrast_ratio(best, bg);
        for &candidate in candidates.iter().skip(1) {
            let ratio = contrast_ratio(candidate, bg);
            if ratio > best_ratio {
                best = candidate;
                best_ratio = ratio;
            }
        }
        best
    }
}

/// A semantic swatch with pre-computed styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SemanticSwatch {
    pub fg: PackedRgba,
    pub bg: PackedRgba,
    pub text: PackedRgba,
    pub fg_style: Style,
    pub badge_style: Style,
}

impl SemanticSwatch {
    fn new(fg: PackedRgba, bg: PackedRgba, text: PackedRgba) -> Self {
        Self {
            fg,
            bg,
            text,
            fg_style: Style::new().fg(fg),
            badge_style: Style::new().fg(text).bg(bg).bold(),
        }
    }

    fn from_token_in(
        token: ColorToken,
        palette: &ThemePalette,
        base_bg: PackedRgba,
        opacity: f32,
    ) -> Self {
        let fg = token.resolve_in(palette);
        let bg = fg.with_opacity(opacity);
        let composed = bg.over(base_bg);
        let candidates = [
            palette.fg_primary,
            palette.fg_secondary,
            palette.bg_deep,
            PackedRgba::WHITE,
            PackedRgba::BLACK,
        ];
        let text = contrast::best_text_color(composed, &candidates);
        Self::new(fg, bg, text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StatusStyles {
    pub open: SemanticSwatch,
    pub in_progress: SemanticSwatch,
    pub blocked: SemanticSwatch,
    pub closed: SemanticSwatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PriorityStyles {
    pub p0: SemanticSwatch,
    pub p1: SemanticSwatch,
    pub p2: SemanticSwatch,
    pub p3: SemanticSwatch,
    pub p4: SemanticSwatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IssueTypeStyles {
    pub bug: SemanticSwatch,
    pub feature: SemanticSwatch,
    pub task: SemanticSwatch,
    pub epic: SemanticSwatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IntentStyles {
    pub success: SemanticSwatch,
    pub warning: SemanticSwatch,
    pub info: SemanticSwatch,
    pub error: SemanticSwatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SemanticStyles {
    pub status: StatusStyles,
    pub priority: PriorityStyles,
    pub issue_type: IssueTypeStyles,
    pub intent: IntentStyles,
}

/// Semantic status badge variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusBadge {
    Open,
    InProgress,
    Blocked,
    Closed,
}

/// Semantic priority badge variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PriorityBadge {
    P0,
    P1,
    P2,
    P3,
    P4,
}

/// Label + style for a semantic badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BadgeSpec {
    pub label: &'static str,
    pub style: Style,
}

static SEMANTIC_STYLES_ALL: OnceLock<[SemanticStyles; ThemeId::ALL.len()]> = OnceLock::new();

fn semantic_styles_for(theme: ThemeId) -> SemanticStyles {
    let palette = palette(theme);
    let base_bg = palette.bg_base;
    let opacity = SEMANTIC_TINT_OPACITY;
    SemanticStyles {
        status: StatusStyles {
            open: SemanticSwatch::from_token_in(status::OPEN, palette, base_bg, opacity),
            in_progress: SemanticSwatch::from_token_in(
                status::IN_PROGRESS,
                palette,
                base_bg,
                opacity,
            ),
            blocked: SemanticSwatch::from_token_in(status::BLOCKED, palette, base_bg, opacity),
            closed: SemanticSwatch::from_token_in(status::CLOSED, palette, base_bg, opacity),
        },
        priority: PriorityStyles {
            p0: SemanticSwatch::from_token_in(priority::P0, palette, base_bg, opacity),
            p1: SemanticSwatch::from_token_in(priority::P1, palette, base_bg, opacity),
            p2: SemanticSwatch::from_token_in(priority::P2, palette, base_bg, opacity),
            p3: SemanticSwatch::from_token_in(priority::P3, palette, base_bg, opacity),
            p4: SemanticSwatch::from_token_in(priority::P4, palette, base_bg, opacity),
        },
        issue_type: IssueTypeStyles {
            bug: SemanticSwatch::from_token_in(issue_type::BUG, palette, base_bg, opacity),
            feature: SemanticSwatch::from_token_in(issue_type::FEATURE, palette, base_bg, opacity),
            task: SemanticSwatch::from_token_in(issue_type::TASK, palette, base_bg, opacity),
            epic: SemanticSwatch::from_token_in(issue_type::EPIC, palette, base_bg, opacity),
        },
        intent: IntentStyles {
            success: SemanticSwatch::from_token_in(intent::SUCCESS, palette, base_bg, opacity),
            warning: SemanticSwatch::from_token_in(intent::WARNING, palette, base_bg, opacity),
            info: SemanticSwatch::from_token_in(intent::INFO, palette, base_bg, opacity),
            error: SemanticSwatch::from_token_in(intent::ERROR, palette, base_bg, opacity),
        },
    }
}

/// Pre-compute semantic styles for the current theme.
pub fn semantic_styles() -> SemanticStyles {
    *semantic_styles_cached()
}

/// Build a semantic status badge (label + style) for the current theme.
#[must_use]
pub fn status_badge(status: StatusBadge) -> BadgeSpec {
    let styles = semantic_styles();
    match status {
        StatusBadge::Open => BadgeSpec {
            label: "OPEN",
            style: styles.status.open.badge_style,
        },
        StatusBadge::InProgress => BadgeSpec {
            label: "PROG",
            style: styles.status.in_progress.badge_style,
        },
        StatusBadge::Blocked => BadgeSpec {
            label: "BLKD",
            style: styles.status.blocked.badge_style,
        },
        StatusBadge::Closed => BadgeSpec {
            label: "DONE",
            style: styles.status.closed.badge_style,
        },
    }
}

/// Build a semantic priority badge (label + style) for the current theme.
#[must_use]
pub fn priority_badge(priority: PriorityBadge) -> BadgeSpec {
    let styles = semantic_styles();
    match priority {
        PriorityBadge::P0 => BadgeSpec {
            label: "P0",
            style: styles.priority.p0.badge_style,
        },
        PriorityBadge::P1 => BadgeSpec {
            label: "P1",
            style: styles.priority.p1.badge_style,
        },
        PriorityBadge::P2 => BadgeSpec {
            label: "P2",
            style: styles.priority.p2.badge_style,
        },
        PriorityBadge::P3 => BadgeSpec {
            label: "P3",
            style: styles.priority.p3.badge_style,
        },
        PriorityBadge::P4 => BadgeSpec {
            label: "P4",
            style: styles.priority.p4.badge_style,
        },
    }
}

/// Borrow pre-computed semantic styles for the current theme (cached per built-in theme).
pub fn semantic_styles_cached() -> &'static SemanticStyles {
    let all = SEMANTIC_STYLES_ALL.get_or_init(|| ThemeId::ALL.map(semantic_styles_for));
    &all[current_theme().index()]
}

/// Build a syntax highlight theme from the active palette.
#[cfg(feature = "syntax")]
pub fn syntax_theme() -> HighlightTheme {
    HighlightTheme {
        keyword: Style::new().fg(syntax::KEYWORD).bold(),
        keyword_control: Style::new().fg(syntax::KEYWORD),
        keyword_type: Style::new().fg(syntax::TYPE),
        keyword_modifier: Style::new().fg(syntax::KEYWORD),
        string: Style::new().fg(syntax::STRING),
        string_escape: Style::new().fg(accent::WARNING),
        number: Style::new().fg(syntax::NUMBER),
        boolean: Style::new().fg(syntax::NUMBER),
        identifier: Style::new().fg(fg::PRIMARY),
        type_name: Style::new().fg(syntax::TYPE),
        constant: Style::new().fg(syntax::NUMBER),
        function: Style::new().fg(syntax::FUNCTION),
        macro_name: Style::new().fg(accent::SECONDARY),
        comment: Style::new().fg(syntax::COMMENT).italic(),
        comment_block: Style::new().fg(syntax::COMMENT).italic(),
        comment_doc: Style::new().fg(syntax::COMMENT).italic(),
        operator: Style::new().fg(syntax::OPERATOR),
        punctuation: Style::new().fg(syntax::PUNCTUATION),
        delimiter: Style::new().fg(syntax::PUNCTUATION),
        attribute: Style::new().fg(accent::INFO),
        lifetime: Style::new().fg(accent::WARNING),
        label: Style::new().fg(accent::WARNING),
        heading: Style::new().fg(accent::PRIMARY).bold(),
        link: Style::new().fg(accent::LINK).underline(),
        emphasis: Style::new().italic(),
        whitespace: Style::new(),
        error: Style::new().fg(accent::ERROR).bold(),
        text: Style::new().fg(fg::PRIMARY),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_rotation_wraps() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        // CyberpunkAurora -> Darcula
        set_theme(ThemeId::CyberpunkAurora);
        assert_eq!(cycle_theme(), ThemeId::Darcula);
        // NordicFrost -> HighContrast
        set_theme(ThemeId::NordicFrost);
        assert_eq!(cycle_theme(), ThemeId::HighContrast);
        // HighContrast -> CyberpunkAurora (wraps)
        assert_eq!(cycle_theme(), ThemeId::CyberpunkAurora);
    }

    #[test]
    fn token_resolves_from_palette() {
        let _guard = ScopedThemeLock::new(ThemeId::Darcula);
        let color: PackedRgba = fg::PRIMARY.into();
        assert_eq!(color, palette(ThemeId::Darcula).fg_primary);
    }

    #[test]
    fn alpha_color_preserves_channel_and_alpha() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let color = AlphaColor::new(bg::BASE, 123).resolve();
        let base = current_palette().bg_base;
        assert_eq!(color.r(), base.r());
        assert_eq!(color.g(), base.g());
        assert_eq!(color.b(), base.b());
        assert_eq!(color.a(), 123);
    }

    #[test]
    fn blend_over_matches_packed_rgba() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let blended = blend_over(accent::PRIMARY, bg::BASE, 0.5);
        let expected = accent::PRIMARY
            .resolve()
            .with_opacity(0.5)
            .over(bg::BASE.resolve());
        assert_eq!(blended, expected);
    }

    #[test]
    fn accent_gradient_wraps() {
        for theme in ThemeId::ALL {
            let _guard = ScopedThemeLock::new(theme);
            assert_eq!(accent_gradient(0.0), accent_gradient(1.0));
            assert_eq!(accent_gradient(-1.0), accent_gradient(0.0));
        }
    }

    #[test]
    fn status_colors_have_valid_contrast() {
        for theme in ThemeId::ALL {
            let _guard = ScopedThemeLock::new(theme);
            let base = bg::BASE.resolve();
            let open = status::OPEN.resolve();
            let progress = status::IN_PROGRESS.resolve();
            let blocked = status::BLOCKED.resolve();
            let closed = status::CLOSED.resolve();
            assert!(
                contrast::contrast_ratio(open, base) >= 4.5,
                "OPEN contrast too low for {theme:?}"
            );
            assert!(
                contrast::contrast_ratio(progress, base) >= 4.5,
                "IN_PROGRESS contrast too low for {theme:?}"
            );
            assert!(
                contrast::contrast_ratio(blocked, base) >= 4.5,
                "BLOCKED contrast too low for {theme:?}"
            );
            assert!(
                contrast::contrast_ratio(closed, base) >= 4.5,
                "CLOSED contrast too low for {theme:?}"
            );
        }
    }

    #[test]
    fn priority_colors_distinct() {
        for theme in ThemeId::ALL {
            let _guard = ScopedThemeLock::new(theme);
            let colors = [
                priority::P0.resolve(),
                priority::P1.resolve(),
                priority::P2.resolve(),
                priority::P3.resolve(),
                priority::P4.resolve(),
            ];
            for i in 0..colors.len() {
                for j in (i + 1)..colors.len() {
                    assert_ne!(
                        colors[i], colors[j],
                        "Priority colors should be distinct for {theme:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn status_bg_variants_have_low_opacity() {
        for theme in ThemeId::ALL {
            let _guard = ScopedThemeLock::new(theme);
            assert!(status::open_bg().a() < 128);
            assert!(status::in_progress_bg().a() < 128);
            assert!(status::blocked_bg().a() < 128);
            assert!(status::closed_bg().a() < 128);
        }
    }

    #[test]
    fn status_badge_text_meets_contrast() {
        for theme in ThemeId::ALL {
            let _guard = ScopedThemeLock::new(theme);
            let base = bg::BASE.resolve();
            let bg_open = status::open_bg().over(base);
            let text_open = status::open_text();
            assert!(
                contrast::meets_wcag_aa(text_open, bg_open),
                "OPEN badge contrast too low for {theme:?}"
            );
        }
    }

    #[test]
    fn semantic_styles_build_valid_badge_styles() {
        for theme in ThemeId::ALL {
            let styles = semantic_styles_for(theme);
            let base_bg = palette(theme).bg_base;

            let swatches = [
                styles.status.open,
                styles.status.in_progress,
                styles.status.blocked,
                styles.status.closed,
                styles.priority.p0,
                styles.priority.p1,
                styles.priority.p2,
                styles.priority.p3,
                styles.priority.p4,
                styles.issue_type.bug,
                styles.issue_type.feature,
                styles.issue_type.task,
                styles.issue_type.epic,
                styles.intent.success,
                styles.intent.warning,
                styles.intent.info,
                styles.intent.error,
            ];

            for swatch in swatches {
                assert!(
                    swatch.fg_style.fg.is_some(),
                    "missing fg_style.fg for {theme:?}"
                );
                assert!(
                    swatch.badge_style.fg.is_some(),
                    "missing badge_style.fg for {theme:?}"
                );
                assert!(
                    swatch.badge_style.bg.is_some(),
                    "missing badge_style.bg for {theme:?}"
                );

                let badge_bg = swatch.bg.over(base_bg);
                assert!(
                    contrast::meets_wcag_aa(swatch.text, badge_bg),
                    "badge text contrast too low for {theme:?}"
                );

                assert_ne!(
                    swatch.badge_style.fg, swatch.badge_style.bg,
                    "badge fg/bg should differ for {theme:?}"
                );
            }
        }
    }

    #[test]
    fn status_badge_labels_are_distinct() {
        let labels = [
            status_badge(StatusBadge::Open).label,
            status_badge(StatusBadge::InProgress).label,
            status_badge(StatusBadge::Blocked).label,
            status_badge(StatusBadge::Closed).label,
        ];
        for i in 0..labels.len() {
            for j in (i + 1)..labels.len() {
                assert_ne!(
                    labels[i], labels[j],
                    "status badge labels should be distinct"
                );
            }
        }
    }

    #[test]
    fn priority_badge_labels_and_colors_are_distinct() {
        for theme in ThemeId::ALL {
            let _guard = ScopedThemeLock::new(theme);
            let badges = [
                priority_badge(PriorityBadge::P0),
                priority_badge(PriorityBadge::P1),
                priority_badge(PriorityBadge::P2),
                priority_badge(PriorityBadge::P3),
                priority_badge(PriorityBadge::P4),
            ];

            let labels: Vec<_> = badges.iter().map(|b| b.label).collect();
            for i in 0..labels.len() {
                for j in (i + 1)..labels.len() {
                    assert_ne!(
                        labels[i], labels[j],
                        "priority badge labels should be distinct"
                    );
                }
            }

            let bgs: Vec<_> = badges.iter().map(|b| b.style.bg).collect();
            for i in 0..bgs.len() {
                for j in (i + 1)..bgs.len() {
                    assert_ne!(bgs[i], bgs[j], "priority badge backgrounds should differ");
                }
            }
        }
    }
}
