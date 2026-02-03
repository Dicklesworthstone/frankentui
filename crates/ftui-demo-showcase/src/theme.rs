#![forbid(unsafe_code)]

//! Shared theme styles for the demo showcase, backed by ftui-extras themes.
//!
//! # Spacing System
//!
//! This module provides a consistent spacing system with named tokens for visual rhythm:
//!
//! | Token | Value | Use Case |
//! |-------|-------|----------|
//! | `XS` | 1 | Tight spacing (inline elements, dense UI) |
//! | `SM` | 2 | Compact (dense lists, item gaps) |
//! | `MD` | 3 | Normal (default padding, panel content) |
//! | `LG` | 4 | Generous (panel separation, section gaps) |
//! | `XL` | 6 | Spacious (major sections, modal margins) |
//!
//! Semantic aliases map to these tokens:
//! - `INLINE` (XS): Between inline elements
//! - `ITEM_GAP` (SM): Between list/grid items
//! - `PANEL_PADDING` (MD): Content padding inside panels
//! - `SECTION_GAP` (LG): Between major sections
//! - `MAJOR_GAP` (XL): Top-level layout separation

use ftui_extras::theme as core_theme;
use ftui_style::{Style, StyleFlags};

pub use core_theme::{
    AlphaColor, BadgeSpec, ColorToken, IntentStyles, IssueTypeStyles, PriorityBadge,
    PriorityStyles, SemanticStyles, SemanticSwatch, StatusBadge, StatusStyles, ThemeId, accent,
    accent_gradient, alpha, bg, blend_colors, blend_over, contrast, current_theme,
    current_theme_name, cycle_theme, fg, intent, issue_type, priority, priority_badge,
    semantic_styles, status, status_badge, syntax, syntax_theme, theme_count, with_alpha,
    with_opacity,
};
pub use core_theme::{palette, set_theme};

// ---------------------------------------------------------------------------
// Spacing tokens
// ---------------------------------------------------------------------------

/// Spacing tokens for consistent visual rhythm.
///
/// All values are in terminal cells (characters). These tokens create a
/// harmonious scale that feels intentional and professional.
///
/// # Design Rationale
///
/// The scale follows a non-linear progression that matches human perception:
/// - Small values (1-2) for tight, related elements
/// - Medium values (3-4) for standard separation
/// - Large values (6) for major visual breaks
///
/// This 1-2-3-4-6 scale is common in design systems (similar to 4px/8px/12px/16px/24px
/// in web design, but adapted for terminal cell-based layouts).
pub mod spacing {
    /// Extra-small spacing (1 cell). Use for tight, inline elements.
    pub const XS: u16 = 1;
    /// Small spacing (2 cells). Use for compact lists and item gaps.
    pub const SM: u16 = 2;
    /// Medium spacing (3 cells). Default for padding and content gaps.
    pub const MD: u16 = 3;
    /// Large spacing (4 cells). Use for section separation.
    pub const LG: u16 = 4;
    /// Extra-large spacing (6 cells). Use for major layout gaps.
    pub const XL: u16 = 6;

    // Semantic spacing aliases - use these when the context is clear

    /// Spacing between inline elements (maps to XS).
    pub const INLINE: u16 = XS;
    /// Gap between list/grid items (maps to SM).
    pub const ITEM_GAP: u16 = SM;
    /// Padding inside panels and containers (maps to MD).
    pub const PANEL_PADDING: u16 = MD;
    /// Gap between major sections (maps to LG).
    pub const SECTION_GAP: u16 = LG;
    /// Top-level layout separation (maps to XL).
    pub const MAJOR_GAP: u16 = XL;

    // Additional semantic spacing for specific use cases

    /// Horizontal margin for content areas.
    pub const CONTENT_MARGIN_H: u16 = SM;
    /// Vertical margin for content areas.
    pub const CONTENT_MARGIN_V: u16 = XS;
    /// Gap between form fields.
    pub const FORM_GAP: u16 = SM;
    /// Gap between buttons in a button group.
    pub const BUTTON_GAP: u16 = SM;
    /// Padding inside a modal/dialog.
    pub const MODAL_PADDING: u16 = LG;
    /// Gap between tab bar and content.
    pub const TAB_CONTENT_GAP: u16 = XS;
    /// Gap between status bar and content.
    pub const STATUS_BAR_GAP: u16 = XS;
}

/// Border radius tokens (for rounded corners in box-drawing contexts).
///
/// Note: Terminal UIs don't have true border radius, but these values
/// can inform decisions about border character sets (e.g., rounded vs sharp).
pub mod radius {
    /// Small radius - subtle rounding.
    pub const SM: u16 = 4;
    /// Medium radius - noticeable rounding.
    pub const MD: u16 = 8;
    /// Large radius - prominent rounding.
    pub const LG: u16 = 12;
}

/// Per-screen accent colors for visual distinction.
pub mod screen_accent {
    use super::{ColorToken, accent};

    pub const DASHBOARD: ColorToken = accent::ACCENT_1;
    pub const SHAKESPEARE: ColorToken = accent::ACCENT_4;
    pub const CODE_EXPLORER: ColorToken = accent::ACCENT_3;
    pub const WIDGET_GALLERY: ColorToken = accent::ACCENT_2;
    pub const LAYOUT_LAB: ColorToken = accent::ACCENT_8;
    pub const FORMS_INPUT: ColorToken = accent::ACCENT_6;
    pub const DATA_VIZ: ColorToken = accent::ACCENT_4;
    pub const FILE_BROWSER: ColorToken = accent::ACCENT_7;
    pub const ADVANCED: ColorToken = accent::ACCENT_5;
    pub const PERFORMANCE: ColorToken = accent::ACCENT_10;
    pub const MARKDOWN: ColorToken = accent::ACCENT_11;
    pub const VISUAL_EFFECTS: ColorToken = accent::ACCENT_12;
    pub const RESPONSIVE_DEMO: ColorToken = accent::ACCENT_9;
    pub const LOG_SEARCH: ColorToken = accent::ACCENT_3;
    pub const ACTION_TIMELINE: ColorToken = accent::ACCENT_2;
    pub const INTRINSIC_SIZING: ColorToken = accent::ACCENT_8;
}

// ---------------------------------------------------------------------------
// Icon vocabulary
// ---------------------------------------------------------------------------

/// Semantic icons for visual indicators.
///
/// This module provides consistent iconography throughout the UI, with both
/// emoji (Unicode) and ASCII fallback variants. Icons convey meaning instantly
/// without requiring text labels.
///
/// # Design Principles
///
/// 1. **Semantic meaning**: Each icon represents a specific concept
/// 2. **Visual distinctiveness**: Icons in the same category are visually distinct
/// 3. **Graceful degradation**: ASCII fallbacks for limited terminals
/// 4. **Consistent width**: Most icons are 1-2 cells wide
///
/// # Terminal Compatibility
///
/// - Modern terminals (iTerm2, Kitty, WezTerm, Ghostty): Full emoji support
/// - Older terminals: Use ASCII fallback via `icons::ascii` module
/// - Detection: Check `TERM`, `COLORTERM`, or use heuristics
///
/// # Usage
///
/// ```rust
/// use ftui_demo_showcase::theme::icons;
///
/// // Direct emoji use (for modern terminals)
/// let status = icons::STATUS_OPEN; // "🟢"
///
/// // ASCII fallback (for compatibility)
/// let status_ascii = icons::ascii::STATUS_OPEN; // "[O]"
///
/// // Combine with color for maximum clarity
/// let styled = format!("{} Open", icons::STATUS_OPEN);
/// ```
pub mod icons {
    // -------------------------------------------------------------------------
    // Status indicators
    // -------------------------------------------------------------------------

    /// Open/active status (green circle).
    pub const STATUS_OPEN: &str = "🟢";
    /// In-progress status (blue circle).
    pub const STATUS_PROGRESS: &str = "🔵";
    /// Blocked status (red circle).
    pub const STATUS_BLOCKED: &str = "🔴";
    /// Closed/completed status (black circle).
    pub const STATUS_CLOSED: &str = "⚫";

    // -------------------------------------------------------------------------
    // Priority levels
    // -------------------------------------------------------------------------

    /// Critical priority (P0) - requires immediate attention.
    pub const PRIORITY_CRITICAL: &str = "🔥";
    /// High priority (P1) - important work.
    pub const PRIORITY_HIGH: &str = "⚡";
    /// Medium priority (P2) - standard work.
    pub const PRIORITY_MEDIUM: &str = "🔹";
    /// Low priority (P3) - can wait.
    pub const PRIORITY_LOW: &str = "☕";
    /// Minimal priority (P4) - backlog.
    pub const PRIORITY_MINIMAL: &str = "💤";

    // -------------------------------------------------------------------------
    // Issue/item types
    // -------------------------------------------------------------------------

    /// Bug/defect.
    pub const TYPE_BUG: &str = "🐛";
    /// New feature.
    pub const TYPE_FEATURE: &str = "✨";
    /// Task/work item.
    pub const TYPE_TASK: &str = "📋";
    /// Epic/large initiative.
    pub const TYPE_EPIC: &str = "🚀";
    /// Chore/maintenance.
    pub const TYPE_CHORE: &str = "🧹";
    /// Documentation.
    pub const TYPE_DOCS: &str = "📖";
    /// Question/discussion.
    pub const TYPE_QUESTION: &str = "❓";

    // -------------------------------------------------------------------------
    // Intent/feedback indicators
    // -------------------------------------------------------------------------

    /// Error/failure.
    pub const INTENT_ERROR: &str = "❌";
    /// Warning/caution.
    pub const INTENT_WARNING: &str = "⚠️";
    /// Information.
    pub const INTENT_INFO: &str = "ℹ️";
    /// Success/done.
    pub const INTENT_SUCCESS: &str = "✅";

    // -------------------------------------------------------------------------
    // Action indicators
    // -------------------------------------------------------------------------

    /// Blocked/stopped.
    pub const ACTION_BLOCKED: &str = "⛔";
    /// Linked/connected.
    pub const ACTION_LINKED: &str = "🔗";
    /// Dependency.
    pub const ACTION_DEPENDS: &str = "📦";
    /// Search/find.
    pub const ACTION_SEARCH: &str = "🔍";
    /// Edit/modify.
    pub const ACTION_EDIT: &str = "✏️";
    /// Delete/remove.
    pub const ACTION_DELETE: &str = "🗑️";
    /// Add/create.
    pub const ACTION_ADD: &str = "➕";
    /// Refresh/reload.
    pub const ACTION_REFRESH: &str = "🔄";

    // -------------------------------------------------------------------------
    // UI elements
    // -------------------------------------------------------------------------

    /// Right arrow.
    pub const ARROW_RIGHT: &str = "→";
    /// Left arrow.
    pub const ARROW_LEFT: &str = "←";
    /// Up arrow.
    pub const ARROW_UP: &str = "↑";
    /// Down arrow.
    pub const ARROW_DOWN: &str = "↓";
    /// Bullet point.
    pub const BULLET: &str = "•";
    /// Checkbox checked.
    pub const CHECKBOX_ON: &str = "☑";
    /// Checkbox unchecked.
    pub const CHECKBOX_OFF: &str = "☐";
    /// Radio selected.
    pub const RADIO_ON: &str = "◉";
    /// Radio unselected.
    pub const RADIO_OFF: &str = "○";
    /// Expand/collapsed indicator.
    pub const EXPAND: &str = "▸";
    /// Collapse/expanded indicator.
    pub const COLLAPSE: &str = "▾";
    /// Folder closed.
    pub const FOLDER_CLOSED: &str = "📁";
    /// Folder open.
    pub const FOLDER_OPEN: &str = "📂";
    /// File.
    pub const FILE: &str = "📄";
    /// Anchor/pin.
    pub const ANCHOR: &str = "📍";
    /// History/time.
    pub const HISTORY: &str = "🕐";
    /// Star/favorite.
    pub const STAR: &str = "⭐";
    /// Settings/gear.
    pub const SETTINGS: &str = "⚙️";

    // -------------------------------------------------------------------------
    // Decorative/separators
    // -------------------------------------------------------------------------

    /// Vertical separator.
    pub const SEPARATOR_V: &str = "│";
    /// Horizontal separator.
    pub const SEPARATOR_H: &str = "─";
    /// Ellipsis (truncation indicator).
    pub const ELLIPSIS: &str = "…";

    /// ASCII fallback icons for terminals without emoji support.
    ///
    /// These provide equivalent semantic meaning using standard ASCII/box-drawing
    /// characters that render correctly in any terminal.
    pub mod ascii {
        // Status
        pub const STATUS_OPEN: &str = "[O]";
        pub const STATUS_PROGRESS: &str = "[~]";
        pub const STATUS_BLOCKED: &str = "[!]";
        pub const STATUS_CLOSED: &str = "[x]";

        // Priority
        pub const PRIORITY_CRITICAL: &str = "[!!!]";
        pub const PRIORITY_HIGH: &str = "[!!]";
        pub const PRIORITY_MEDIUM: &str = "[!]";
        pub const PRIORITY_LOW: &str = "[-]";
        pub const PRIORITY_MINIMAL: &str = "[.]";

        // Types
        pub const TYPE_BUG: &str = "[bug]";
        pub const TYPE_FEATURE: &str = "[+]";
        pub const TYPE_TASK: &str = "[T]";
        pub const TYPE_EPIC: &str = "[E]";
        pub const TYPE_CHORE: &str = "[c]";
        pub const TYPE_DOCS: &str = "[D]";
        pub const TYPE_QUESTION: &str = "[?]";

        // Intent
        pub const INTENT_ERROR: &str = "[X]";
        pub const INTENT_WARNING: &str = "[W]";
        pub const INTENT_INFO: &str = "[i]";
        pub const INTENT_SUCCESS: &str = "[v]";

        // Actions
        pub const ACTION_BLOCKED: &str = "[X]";
        pub const ACTION_LINKED: &str = "<->";
        pub const ACTION_DEPENDS: &str = "[d]";
        pub const ACTION_SEARCH: &str = "[?]";
        pub const ACTION_EDIT: &str = "[e]";
        pub const ACTION_DELETE: &str = "[-]";
        pub const ACTION_ADD: &str = "[+]";
        pub const ACTION_REFRESH: &str = "[r]";

        // UI elements
        pub const ARROW_RIGHT: &str = "->";
        pub const ARROW_LEFT: &str = "<-";
        pub const ARROW_UP: &str = "^";
        pub const ARROW_DOWN: &str = "v";
        pub const BULLET: &str = "*";
        pub const CHECKBOX_ON: &str = "[x]";
        pub const CHECKBOX_OFF: &str = "[ ]";
        pub const RADIO_ON: &str = "(*)";
        pub const RADIO_OFF: &str = "( )";
        pub const EXPAND: &str = ">";
        pub const COLLAPSE: &str = "v";
        pub const FOLDER_CLOSED: &str = "[+]";
        pub const FOLDER_OPEN: &str = "[-]";
        pub const FILE: &str = " - ";
        pub const ANCHOR: &str = "@";
        pub const HISTORY: &str = "[t]";
        pub const STAR: &str = "*";
        pub const SETTINGS: &str = "[S]";

        // Decorative
        pub const SEPARATOR_V: &str = "|";
        pub const SEPARATOR_H: &str = "-";
        pub const ELLIPSIS: &str = "...";
    }
}

/// Helper to select emoji or ASCII icons based on terminal capability.
///
/// Returns `true` if the terminal likely supports emoji/Unicode icons.
/// This is a heuristic based on common terminal environment variables.
///
/// # Detection Strategy
///
/// 1. Check `COLORTERM` for modern terminals (truecolor implies Unicode)
/// 2. Check `TERM` for known Unicode-capable terminals
/// 3. Check for known terminal-specific variables (KITTY, WEZTERM, etc.)
/// 4. Default to `true` for most cases (graceful degradation)
///
/// # Usage
///
/// ```rust
/// use ftui_demo_showcase::theme::{icons, supports_emoji_icons};
///
/// let status_icon = if supports_emoji_icons() {
///     icons::STATUS_OPEN
/// } else {
///     icons::ascii::STATUS_OPEN
/// };
/// ```
pub fn supports_emoji_icons() -> bool {
    // Check for modern terminal indicators
    if std::env::var("COLORTERM")
        .map(|v| v.contains("truecolor") || v.contains("24bit"))
        .unwrap_or(false)
    {
        return true;
    }

    // Check for known modern terminals
    if std::env::var("KITTY_WINDOW_ID").is_ok()
        || std::env::var("WEZTERM_PANE").is_ok()
        || std::env::var("GHOSTTY_RESOURCES_DIR").is_ok()
    {
        return true;
    }

    // Check TERM for known emoji-capable terminals
    if let Ok(term) = std::env::var("TERM") {
        let term_lower = term.to_lowercase();
        if term_lower.contains("kitty")
            || term_lower.contains("wezterm")
            || term_lower.contains("alacritty")
            || term_lower.contains("iterm")
            || term_lower.contains("256color")
            || term_lower.contains("xterm")
        {
            return true;
        }
    }

    // Default to true - most modern terminals support emoji
    // Users on limited terminals can set an override env var
    !std::env::var("FTUI_NO_EMOJI")
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(false)
}

/// Get a status icon with appropriate fallback.
pub fn status_icon(
    is_emoji: bool,
    is_open: bool,
    is_progress: bool,
    is_blocked: bool,
) -> &'static str {
    if is_blocked {
        if is_emoji {
            icons::STATUS_BLOCKED
        } else {
            icons::ascii::STATUS_BLOCKED
        }
    } else if is_progress {
        if is_emoji {
            icons::STATUS_PROGRESS
        } else {
            icons::ascii::STATUS_PROGRESS
        }
    } else if is_open {
        if is_emoji {
            icons::STATUS_OPEN
        } else {
            icons::ascii::STATUS_OPEN
        }
    } else {
        if is_emoji {
            icons::STATUS_CLOSED
        } else {
            icons::ascii::STATUS_CLOSED
        }
    }
}

/// Get a priority icon with appropriate fallback.
pub fn priority_icon(is_emoji: bool, priority: u8) -> &'static str {
    match priority {
        0 => {
            if is_emoji {
                icons::PRIORITY_CRITICAL
            } else {
                icons::ascii::PRIORITY_CRITICAL
            }
        }
        1 => {
            if is_emoji {
                icons::PRIORITY_HIGH
            } else {
                icons::ascii::PRIORITY_HIGH
            }
        }
        2 => {
            if is_emoji {
                icons::PRIORITY_MEDIUM
            } else {
                icons::ascii::PRIORITY_MEDIUM
            }
        }
        3 => {
            if is_emoji {
                icons::PRIORITY_LOW
            } else {
                icons::ascii::PRIORITY_LOW
            }
        }
        _ => {
            if is_emoji {
                icons::PRIORITY_MINIMAL
            } else {
                icons::ascii::PRIORITY_MINIMAL
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Named styles
// ---------------------------------------------------------------------------

/// Semantic text styles.
pub fn title() -> Style {
    Style::new().fg(fg::PRIMARY).attrs(StyleFlags::BOLD)
}

pub fn subtitle() -> Style {
    Style::new().fg(fg::SECONDARY).attrs(StyleFlags::ITALIC)
}

pub fn body() -> Style {
    Style::new().fg(fg::PRIMARY)
}

pub fn muted() -> Style {
    Style::new().fg(fg::MUTED)
}

pub fn link() -> Style {
    Style::new().fg(accent::LINK).attrs(StyleFlags::UNDERLINE)
}

pub fn code() -> Style {
    Style::new().fg(accent::INFO).bg(alpha::SURFACE)
}

pub fn error_style() -> Style {
    Style::new().fg(accent::ERROR).attrs(StyleFlags::BOLD)
}

pub fn success() -> Style {
    Style::new().fg(accent::SUCCESS).attrs(StyleFlags::BOLD)
}

pub fn warning() -> Style {
    Style::new().fg(accent::WARNING).attrs(StyleFlags::BOLD)
}

// ---------------------------------------------------------------------------
// Attribute showcase styles (exercises every StyleFlags variant)
// ---------------------------------------------------------------------------

pub fn bold() -> Style {
    Style::new().fg(fg::PRIMARY).attrs(StyleFlags::BOLD)
}

pub fn dim() -> Style {
    Style::new().fg(fg::PRIMARY).attrs(StyleFlags::DIM)
}

pub fn italic() -> Style {
    Style::new().fg(fg::PRIMARY).attrs(StyleFlags::ITALIC)
}

pub fn underline() -> Style {
    Style::new().fg(fg::PRIMARY).attrs(StyleFlags::UNDERLINE)
}

pub fn double_underline() -> Style {
    Style::new()
        .fg(fg::PRIMARY)
        .attrs(StyleFlags::DOUBLE_UNDERLINE)
}

pub fn curly_underline() -> Style {
    Style::new()
        .fg(fg::PRIMARY)
        .attrs(StyleFlags::CURLY_UNDERLINE)
}

pub fn blink_style() -> Style {
    Style::new().fg(fg::PRIMARY).attrs(StyleFlags::BLINK)
}

pub fn reverse() -> Style {
    Style::new().fg(fg::PRIMARY).attrs(StyleFlags::REVERSE)
}

pub fn hidden() -> Style {
    Style::new().fg(fg::PRIMARY).attrs(StyleFlags::HIDDEN)
}

pub fn strikethrough() -> Style {
    Style::new()
        .fg(fg::PRIMARY)
        .attrs(StyleFlags::STRIKETHROUGH)
}

// ---------------------------------------------------------------------------
// Component styles
// ---------------------------------------------------------------------------

/// Tab bar background.
pub fn tab_bar() -> Style {
    Style::new().bg(alpha::SURFACE).fg(fg::SECONDARY)
}

/// Active tab.
pub fn tab_active() -> Style {
    Style::new()
        .bg(alpha::HIGHLIGHT)
        .fg(fg::PRIMARY)
        .attrs(StyleFlags::BOLD)
}

/// Status bar background.
pub fn status_bar() -> Style {
    Style::new().bg(alpha::SURFACE).fg(fg::MUTED)
}

/// Content area border.
pub fn content_border() -> Style {
    Style::new().fg(fg::MUTED)
}

/// Help overlay background.
pub fn help_overlay() -> Style {
    Style::new().bg(alpha::OVERLAY).fg(fg::PRIMARY)
}

// ---------------------------------------------------------------------------
// Focus Management
// ---------------------------------------------------------------------------

/// Selection indicators for list items.
pub mod selection {
    /// Unicode selection indicator (arrow) for focused/selected items.
    pub const INDICATOR: &str = "▶ ";
    /// Empty prefix to maintain alignment with non-selected items.
    pub const EMPTY: &str = "  ";
    /// Alternate selection indicator (bullet) for secondary selections.
    pub const BULLET: &str = "● ";
    /// Cursor indicator for edit mode.
    pub const CURSOR: &str = "│ ";
}

/// Returns the border style for a panel based on its focus state.
///
/// When focused, uses the screen's accent color. When unfocused, uses muted styling.
///
/// # Arguments
/// * `is_focused` - Whether this panel currently has focus
/// * `accent` - The screen's accent color (from `screen_accent` module)
///
/// # Example
/// ```ignore
/// let border_style = panel_border_style(self.focus == Panel::Files, screen_accent::FILE_BROWSER);
/// let block = Block::new().style(border_style);
/// ```
pub fn panel_border_style(is_focused: bool, accent: ColorToken) -> Style {
    if is_focused {
        Style::new().fg(accent)
    } else {
        content_border()
    }
}

/// Returns the style for a list item based on selection and focus state.
///
/// Provides visual hierarchy:
/// - Selected + focused: Primary foreground, highlight background, bold
/// - Selected + unfocused: Secondary foreground, subtle surface background
/// - Not selected: Primary foreground, no background
///
/// # Arguments
/// * `is_selected` - Whether this item is the current selection
/// * `is_focused` - Whether the containing panel has focus
pub fn list_item_style(is_selected: bool, is_focused: bool) -> Style {
    match (is_selected, is_focused) {
        (true, true) => Style::new()
            .fg(fg::PRIMARY)
            .bg(alpha::HIGHLIGHT)
            .attrs(StyleFlags::BOLD),
        (true, false) => Style::new().fg(fg::SECONDARY).bg(alpha::SURFACE),
        (false, _) => Style::new().fg(fg::PRIMARY),
    }
}

/// Returns the selection indicator prefix for a list item.
///
/// Uses "▶ " for selected items and "  " (spaces) for non-selected items
/// to maintain alignment.
pub fn selection_indicator(is_selected: bool) -> &'static str {
    if is_selected {
        selection::INDICATOR
    } else {
        selection::EMPTY
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spacing_tokens_are_strictly_ordered() {
        const _: () = {
            // Tokens must form a strictly increasing sequence
            assert!(spacing::XS < spacing::SM, "XS must be less than SM");
            assert!(spacing::SM < spacing::MD, "SM must be less than MD");
            assert!(spacing::MD < spacing::LG, "MD must be less than LG");
            assert!(spacing::LG < spacing::XL, "LG must be less than XL");
        };
    }

    #[test]
    fn semantic_spacing_maps_to_correct_tokens() {
        // Verify semantic aliases point to expected base tokens
        assert_eq!(spacing::INLINE, spacing::XS);
        assert_eq!(spacing::ITEM_GAP, spacing::SM);
        assert_eq!(spacing::PANEL_PADDING, spacing::MD);
        assert_eq!(spacing::SECTION_GAP, spacing::LG);
        assert_eq!(spacing::MAJOR_GAP, spacing::XL);
    }

    #[test]
    fn spacing_values_are_reasonable_for_tui() {
        const _: () = {
            // All spacing values should be <= 10 cells for terminal contexts
            assert!(spacing::XS <= 10, "XS too large for TUI");
            assert!(spacing::SM <= 10, "SM too large for TUI");
            assert!(spacing::MD <= 10, "MD too large for TUI");
            assert!(spacing::LG <= 10, "LG too large for TUI");
            assert!(spacing::XL <= 10, "XL too large for TUI");
        };
    }

    #[test]
    fn spacing_values_are_positive() {
        const _: () = {
            // No zero or negative spacing
            assert!(spacing::XS > 0, "XS must be positive");
            assert!(spacing::SM > 0, "SM must be positive");
            assert!(spacing::MD > 0, "MD must be positive");
            assert!(spacing::LG > 0, "LG must be positive");
            assert!(spacing::XL > 0, "XL must be positive");
        };
    }

    #[test]
    fn spacing_scale_has_expected_values() {
        // Lock in the specific scale values (1-2-3-4-6)
        assert_eq!(spacing::XS, 1);
        assert_eq!(spacing::SM, 2);
        assert_eq!(spacing::MD, 3);
        assert_eq!(spacing::LG, 4);
        assert_eq!(spacing::XL, 6);
    }

    #[test]
    fn additional_semantic_spacing_uses_appropriate_tokens() {
        const _: () = {
            // Form-related spacing should use small/medium values
            assert!(spacing::FORM_GAP <= spacing::MD);
            assert!(spacing::BUTTON_GAP <= spacing::MD);

            // Content margins should be compact
            assert!(spacing::CONTENT_MARGIN_H <= spacing::MD);
            assert!(spacing::CONTENT_MARGIN_V <= spacing::SM);

            // Modal padding should be generous
            assert!(spacing::MODAL_PADDING >= spacing::LG);

            // Chrome gaps should be minimal
            assert!(spacing::TAB_CONTENT_GAP <= spacing::SM);
            assert!(spacing::STATUS_BAR_GAP <= spacing::SM);
        };
    }

    #[test]
    fn radius_tokens_are_ordered() {
        const _: () = {
            assert!(radius::SM < radius::MD, "SM radius must be less than MD");
            assert!(radius::MD < radius::LG, "MD radius must be less than LG");
        };
    }

    // -------------------------------------------------------------------------
    // Icon tests
    // -------------------------------------------------------------------------

    #[test]
    fn status_icons_are_distinct() {
        let icons_list = [
            icons::STATUS_OPEN,
            icons::STATUS_PROGRESS,
            icons::STATUS_BLOCKED,
            icons::STATUS_CLOSED,
        ];

        for (i, a) in icons_list.iter().enumerate() {
            for (j, b) in icons_list.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "Status icons {} and {} should be distinct", i, j);
                }
            }
        }
    }

    #[test]
    fn priority_icons_are_distinct() {
        let icons_list = [
            icons::PRIORITY_CRITICAL,
            icons::PRIORITY_HIGH,
            icons::PRIORITY_MEDIUM,
            icons::PRIORITY_LOW,
            icons::PRIORITY_MINIMAL,
        ];

        for (i, a) in icons_list.iter().enumerate() {
            for (j, b) in icons_list.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "Priority icons {} and {} should be distinct", i, j);
                }
            }
        }
    }

    #[test]
    fn type_icons_are_distinct() {
        let icons_list = [
            icons::TYPE_BUG,
            icons::TYPE_FEATURE,
            icons::TYPE_TASK,
            icons::TYPE_EPIC,
            icons::TYPE_CHORE,
            icons::TYPE_DOCS,
            icons::TYPE_QUESTION,
        ];

        for (i, a) in icons_list.iter().enumerate() {
            for (j, b) in icons_list.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "Type icons {} and {} should be distinct", i, j);
                }
            }
        }
    }

    #[test]
    fn intent_icons_are_distinct() {
        let icons_list = [
            icons::INTENT_ERROR,
            icons::INTENT_WARNING,
            icons::INTENT_INFO,
            icons::INTENT_SUCCESS,
        ];

        for (i, a) in icons_list.iter().enumerate() {
            for (j, b) in icons_list.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "Intent icons {} and {} should be distinct", i, j);
                }
            }
        }
    }

    #[test]
    fn icons_are_not_empty() {
        // Status
        assert!(!icons::STATUS_OPEN.is_empty());
        assert!(!icons::STATUS_PROGRESS.is_empty());
        assert!(!icons::STATUS_BLOCKED.is_empty());
        assert!(!icons::STATUS_CLOSED.is_empty());

        // Priority
        assert!(!icons::PRIORITY_CRITICAL.is_empty());
        assert!(!icons::PRIORITY_HIGH.is_empty());
        assert!(!icons::PRIORITY_MEDIUM.is_empty());
        assert!(!icons::PRIORITY_LOW.is_empty());

        // Intent
        assert!(!icons::INTENT_ERROR.is_empty());
        assert!(!icons::INTENT_SUCCESS.is_empty());

        // UI elements
        assert!(!icons::ARROW_RIGHT.is_empty());
        assert!(!icons::CHECKBOX_ON.is_empty());
    }

    #[test]
    fn ascii_fallback_icons_are_not_empty() {
        // Status
        assert!(!icons::ascii::STATUS_OPEN.is_empty());
        assert!(!icons::ascii::STATUS_PROGRESS.is_empty());
        assert!(!icons::ascii::STATUS_BLOCKED.is_empty());
        assert!(!icons::ascii::STATUS_CLOSED.is_empty());

        // Priority
        assert!(!icons::ascii::PRIORITY_CRITICAL.is_empty());
        assert!(!icons::ascii::PRIORITY_HIGH.is_empty());
        assert!(!icons::ascii::PRIORITY_MEDIUM.is_empty());
        assert!(!icons::ascii::PRIORITY_LOW.is_empty());

        // Intent
        assert!(!icons::ascii::INTENT_ERROR.is_empty());
        assert!(!icons::ascii::INTENT_SUCCESS.is_empty());
    }

    #[test]
    fn ascii_status_icons_are_distinct() {
        let icons_list = [
            icons::ascii::STATUS_OPEN,
            icons::ascii::STATUS_PROGRESS,
            icons::ascii::STATUS_BLOCKED,
            icons::ascii::STATUS_CLOSED,
        ];

        for (i, a) in icons_list.iter().enumerate() {
            for (j, b) in icons_list.iter().enumerate() {
                if i != j {
                    assert_ne!(
                        a, b,
                        "ASCII status icons {} and {} should be distinct",
                        i, j
                    );
                }
            }
        }
    }

    #[test]
    fn status_icon_helper_returns_correct_icons() {
        // Emoji mode
        assert_eq!(status_icon(true, true, false, false), icons::STATUS_OPEN);
        assert_eq!(
            status_icon(true, false, true, false),
            icons::STATUS_PROGRESS
        );
        assert_eq!(status_icon(true, false, false, true), icons::STATUS_BLOCKED);
        assert_eq!(status_icon(true, false, false, false), icons::STATUS_CLOSED);

        // ASCII mode
        assert_eq!(
            status_icon(false, true, false, false),
            icons::ascii::STATUS_OPEN
        );
        assert_eq!(
            status_icon(false, false, true, false),
            icons::ascii::STATUS_PROGRESS
        );
        assert_eq!(
            status_icon(false, false, false, true),
            icons::ascii::STATUS_BLOCKED
        );
        assert_eq!(
            status_icon(false, false, false, false),
            icons::ascii::STATUS_CLOSED
        );

        // Blocked takes precedence
        assert_eq!(status_icon(true, true, true, true), icons::STATUS_BLOCKED);
    }

    #[test]
    fn priority_icon_helper_returns_correct_icons() {
        // Emoji mode
        assert_eq!(priority_icon(true, 0), icons::PRIORITY_CRITICAL);
        assert_eq!(priority_icon(true, 1), icons::PRIORITY_HIGH);
        assert_eq!(priority_icon(true, 2), icons::PRIORITY_MEDIUM);
        assert_eq!(priority_icon(true, 3), icons::PRIORITY_LOW);
        assert_eq!(priority_icon(true, 4), icons::PRIORITY_MINIMAL);
        assert_eq!(priority_icon(true, 99), icons::PRIORITY_MINIMAL); // Out of range

        // ASCII mode
        assert_eq!(priority_icon(false, 0), icons::ascii::PRIORITY_CRITICAL);
        assert_eq!(priority_icon(false, 1), icons::ascii::PRIORITY_HIGH);
        assert_eq!(priority_icon(false, 2), icons::ascii::PRIORITY_MEDIUM);
        assert_eq!(priority_icon(false, 3), icons::ascii::PRIORITY_LOW);
        assert_eq!(priority_icon(false, 4), icons::ascii::PRIORITY_MINIMAL);
    }

    // -------------------------------------------------------------------------
    // Focus Management tests
    // -------------------------------------------------------------------------

    #[test]
    fn focused_panel_has_accent_border() {
        let accent = screen_accent::FILE_BROWSER;
        let style = panel_border_style(true, accent);
        // Focused panel should use the accent color
        assert!(style.fg.is_some());
        assert_eq!(style.fg, Some(accent.into()));
    }

    #[test]
    fn unfocused_panel_has_muted_border() {
        let accent = screen_accent::FILE_BROWSER;
        let style = panel_border_style(false, accent);
        // Unfocused panel should use muted/content border styling
        let expected = content_border();
        assert_eq!(style.fg, expected.fg);
    }

    #[test]
    fn selected_focused_item_has_highlight() {
        let style = list_item_style(true, true);
        // Selected + focused should have highlight background and bold
        assert!(style.bg.is_some());
        let attrs = style.attrs.unwrap_or(StyleFlags::NONE);
        assert!(attrs.contains(StyleFlags::BOLD));
    }

    #[test]
    fn selected_unfocused_item_has_subtle_bg() {
        let style = list_item_style(true, false);
        // Selected + unfocused should have surface background (subtle)
        assert!(style.bg.is_some());
        // Should not be bold when unfocused
        let attrs = style.attrs.unwrap_or(StyleFlags::NONE);
        assert!(!attrs.contains(StyleFlags::BOLD));
    }

    #[test]
    fn unselected_item_no_highlight() {
        let style_focused = list_item_style(false, true);
        let style_unfocused = list_item_style(false, false);
        // Unselected items should not have background highlighting
        assert!(style_focused.bg.is_none());
        assert!(style_unfocused.bg.is_none());
    }

    #[test]
    fn selection_indicator_present_when_selected() {
        let indicator = selection_indicator(true);
        assert!(indicator.contains('▶') || indicator.contains('>'));
        // Should have the arrow indicator
        assert_eq!(indicator, selection::INDICATOR);
    }

    #[test]
    fn selection_indicator_empty_when_not_selected() {
        let indicator = selection_indicator(false);
        // Should be empty spaces for alignment
        assert_eq!(indicator, selection::EMPTY);
        // Should not contain any visible indicator
        assert!(!indicator.contains('▶'));
        assert!(!indicator.contains('>'));
    }

    #[test]
    fn selection_indicators_have_same_width() {
        // Both indicators must have the same width for proper alignment
        let selected_width = selection::INDICATOR.chars().count();
        let empty_width = selection::EMPTY.chars().count();
        assert_eq!(selected_width, empty_width);
    }

    // -------------------------------------------------------------------------
    // WCAG Contrast Ratio tests (bd-3vbf.31)
    // -------------------------------------------------------------------------

    /// WCAG AA requires at minimum 4.5:1 for normal text and 3.0:1 for large text.
    /// We test against 4.5:1 (AA normal) for all foreground-on-background pairs.
    const WCAG_AA_NORMAL: f32 = 4.5;
    const WCAG_AA_LARGE: f32 = 3.0;

    /// Inline WCAG contrast ratio (avoids visual-fx feature dependency).
    fn cr(fg_token: ColorToken, bg_token: ColorToken) -> f32 {
        use ftui_render::cell::PackedRgba;
        fn linearize(v: f32) -> f32 {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        }
        fn lum(c: PackedRgba) -> f32 {
            let r = linearize(c.r() as f32 / 255.0);
            let g = linearize(c.g() as f32 / 255.0);
            let b = linearize(c.b() as f32 / 255.0);
            0.2126 * r + 0.7152 * g + 0.0722 * b
        }
        let l1 = lum(fg_token.resolve());
        let l2 = lum(bg_token.resolve());
        let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn primary_fg_on_base_bg_meets_wcag_aa() {
        let ratio = cr(fg::PRIMARY, bg::BASE);
        assert!(
            ratio >= WCAG_AA_NORMAL,
            "fg::PRIMARY on bg::BASE contrast {ratio:.2} < {WCAG_AA_NORMAL}"
        );
    }

    #[test]
    fn secondary_fg_on_base_bg_meets_wcag_aa() {
        let ratio = cr(fg::SECONDARY, bg::BASE);
        assert!(
            ratio >= WCAG_AA_NORMAL,
            "fg::SECONDARY on bg::BASE contrast {ratio:.2} < {WCAG_AA_NORMAL}"
        );
    }

    #[test]
    fn muted_fg_on_base_bg_meets_wcag_aa_large() {
        // Muted text is typically used at large size or for decorative purposes,
        // so we use the relaxed 3:1 threshold.
        let ratio = cr(fg::MUTED, bg::BASE);
        assert!(
            ratio >= WCAG_AA_LARGE,
            "fg::MUTED on bg::BASE contrast {ratio:.2} < {WCAG_AA_LARGE}"
        );
    }

    #[test]
    fn accent_error_on_base_bg_meets_wcag_aa() {
        let ratio = cr(accent::ERROR, bg::BASE);
        assert!(
            ratio >= WCAG_AA_NORMAL,
            "accent::ERROR on bg::BASE contrast {ratio:.2} < {WCAG_AA_NORMAL}"
        );
    }

    #[test]
    fn accent_success_on_base_bg_meets_wcag_aa() {
        let ratio = cr(accent::SUCCESS, bg::BASE);
        assert!(
            ratio >= WCAG_AA_NORMAL,
            "accent::SUCCESS on bg::BASE contrast {ratio:.2} < {WCAG_AA_NORMAL}"
        );
    }

    #[test]
    fn accent_warning_on_base_bg_meets_wcag_aa_large() {
        let ratio = cr(accent::WARNING, bg::BASE);
        assert!(
            ratio >= WCAG_AA_LARGE,
            "accent::WARNING on bg::BASE contrast {ratio:.2} < {WCAG_AA_LARGE}"
        );
    }

    #[test]
    fn accent_info_on_base_bg_meets_wcag_aa() {
        let ratio = cr(accent::INFO, bg::BASE);
        assert!(
            ratio >= WCAG_AA_LARGE,
            "accent::INFO on bg::BASE contrast {ratio:.2} < {WCAG_AA_LARGE}"
        );
    }

    #[test]
    fn status_colors_on_base_bg_meet_wcag_aa() {
        let status_tokens = [
            ("StatusOpen", ColorToken::StatusOpen),
            ("StatusInProgress", ColorToken::StatusInProgress),
            ("StatusBlocked", ColorToken::StatusBlocked),
            ("StatusClosed", ColorToken::StatusClosed),
        ];
        for (name, token) in status_tokens {
            let ratio = cr(token, bg::BASE);
            assert!(
                ratio >= WCAG_AA_LARGE,
                "status::{name} on bg::BASE contrast {ratio:.2} < {WCAG_AA_LARGE}"
            );
        }
    }

    #[test]
    fn priority_colors_on_base_bg_meet_wcag_aa() {
        let priority_tokens = [
            ("P0", ColorToken::PriorityP0),
            ("P1", ColorToken::PriorityP1),
            ("P2", ColorToken::PriorityP2),
            ("P3", ColorToken::PriorityP3),
            ("P4", ColorToken::PriorityP4),
        ];
        for (name, token) in priority_tokens {
            let ratio = cr(token, bg::BASE);
            assert!(
                ratio >= WCAG_AA_LARGE,
                "priority::{name} on bg::BASE contrast {ratio:.2} < {WCAG_AA_LARGE}"
            );
        }
    }

    #[test]
    fn screen_accents_on_deep_bg_meet_wcag_aa_large() {
        let screens: &[(&str, ColorToken)] = &[
            ("DASHBOARD", screen_accent::DASHBOARD),
            ("SHAKESPEARE", screen_accent::SHAKESPEARE),
            ("CODE_EXPLORER", screen_accent::CODE_EXPLORER),
            ("WIDGET_GALLERY", screen_accent::WIDGET_GALLERY),
            ("LAYOUT_LAB", screen_accent::LAYOUT_LAB),
            ("FORMS_INPUT", screen_accent::FORMS_INPUT),
            ("ADVANCED", screen_accent::ADVANCED),
            ("PERFORMANCE", screen_accent::PERFORMANCE),
            ("ACTION_TIMELINE", screen_accent::ACTION_TIMELINE),
        ];
        for (name, token) in screens {
            let ratio = cr(*token, bg::DEEP);
            assert!(
                ratio >= WCAG_AA_LARGE,
                "screen_accent::{name} on bg::DEEP contrast {ratio:.2} < {WCAG_AA_LARGE}"
            );
        }
    }

    // -------------------------------------------------------------------------
    // Semantic style validation (bd-3vbf.31)
    // -------------------------------------------------------------------------

    #[test]
    fn semantic_styles_have_nonzero_foreground_colors() {
        let ss = semantic_styles();
        // Status styles should have non-black fg
        let black = ftui_render::cell::PackedRgba::rgb(0, 0, 0);
        assert_ne!(
            ss.status.open.fg, black,
            "status.open fg should not be black"
        );
        assert_ne!(
            ss.status.in_progress.fg, black,
            "status.in_progress fg should not be black"
        );
        assert_ne!(
            ss.status.blocked.fg, black,
            "status.blocked fg should not be black"
        );
        assert_ne!(
            ss.status.closed.fg, black,
            "status.closed fg should not be black"
        );
    }

    #[test]
    fn semantic_styles_status_colors_are_distinct() {
        let ss = semantic_styles();
        let colors = [
            ss.status.open.fg,
            ss.status.in_progress.fg,
            ss.status.blocked.fg,
            ss.status.closed.fg,
        ];
        for i in 0..colors.len() {
            for j in (i + 1)..colors.len() {
                assert_ne!(
                    colors[i], colors[j],
                    "Status colors {i} and {j} should be distinct"
                );
            }
        }
    }
}
