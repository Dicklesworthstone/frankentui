#![forbid(unsafe_code)]

//! Inline validation error display widget.
//!
//! Displays validation errors near form fields with:
//! - Configurable error styling (default: red text with icon)
//! - Smooth appearance animation via opacity interpolation
//! - Screen reader accessibility (ARIA error association via semantic info)
//!
//! # Example
//!
//! ```ignore
//! use ftui_widgets::{ValidationErrorDisplay, ValidationErrorState};
//!
//! // Simple inline error
//! let error = ValidationErrorDisplay::new("This field is required");
//! let mut state = ValidationErrorState::default();
//! error.render(area, &mut frame, &mut state);
//!
//! // With custom styling
//! let error = ValidationErrorDisplay::new("Invalid email address")
//!     .with_icon("!")
//!     .with_style(Style::new().fg(PackedRgba::rgb(255, 100, 100)));
//! ```
//!
//! # Invariants (Alien Artifact)
//!
//! 1. **Icon presence**: Icon is always rendered when error is visible
//! 2. **Animation bounds**: Opacity is clamped to [0.0, 1.0]
//! 3. **Width calculation**: Total width = icon_width + 1 + message_width
//! 4. **Accessibility**: When rendered, screen readers can announce error text
//!
//! # Failure Modes
//!
//! | Scenario | Behavior |
//! |----------|----------|
//! | Empty message | Renders icon only |
//! | Zero-width area | No-op, state unchanged |
//! | Very narrow area | Truncates message with ellipsis |
//! | Animation overflow | Saturates at 0.0 or 1.0 |

use std::time::{Duration, Instant};

use ftui_core::geometry::Rect;
use ftui_render::cell::PackedRgba;
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_text::{display_width, grapheme_width};

use crate::{StatefulWidget, Widget, draw_text_span};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default error foreground color (red).
pub const ERROR_FG_DEFAULT: PackedRgba = PackedRgba::rgb(220, 60, 60);

/// Default error background color (dark red).
pub const ERROR_BG_DEFAULT: PackedRgba = PackedRgba::rgb(40, 0, 0);

/// Default error icon.
pub const ERROR_ICON_DEFAULT: &str = "⚠";

/// Default animation duration.
pub const ANIMATION_DURATION_MS: u64 = 150;

// ---------------------------------------------------------------------------
// ValidationErrorDisplay
// ---------------------------------------------------------------------------

/// A widget for displaying inline validation errors.
///
/// Renders an error message with an icon, styled for visibility and
/// accessibility. Supports smooth appearance/disappearance animations.
#[derive(Debug, Clone)]
pub struct ValidationErrorDisplay {
    /// The error message to display.
    message: String,
    /// Optional error code (for programmatic handling).
    error_code: Option<&'static str>,
    /// Icon to show before the message.
    icon: String,
    /// Style for the error text.
    style: Style,
    /// Style for the icon.
    icon_style: Style,
    /// Animation duration for appearance.
    animation_duration: Duration,
    /// Whether to show the message (vs icon only when narrow).
    show_message: bool,
}

impl Default for ValidationErrorDisplay {
    fn default() -> Self {
        Self {
            message: String::new(),
            error_code: None,
            icon: ERROR_ICON_DEFAULT.to_string(),
            style: Style::new().fg(ERROR_FG_DEFAULT),
            icon_style: Style::new().fg(ERROR_FG_DEFAULT),
            animation_duration: Duration::from_millis(ANIMATION_DURATION_MS),
            show_message: true,
        }
    }
}

impl ValidationErrorDisplay {
    /// Create a new validation error display with the given message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Default::default()
        }
    }

    /// Create from an error code and message.
    #[must_use]
    pub fn with_code(error_code: &'static str, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            error_code: Some(error_code),
            ..Default::default()
        }
    }

    /// Set a custom icon (default: "⚠").
    #[must_use]
    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = icon.into();
        self
    }

    /// Set the error text style.
    #[must_use]
    pub fn with_style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set the icon style.
    #[must_use]
    pub fn with_icon_style(mut self, style: Style) -> Self {
        self.icon_style = style;
        self
    }

    /// Set the animation duration.
    #[must_use]
    pub fn with_animation_duration(mut self, duration: Duration) -> Self {
        self.animation_duration = duration;
        self
    }

    /// Disable message display (icon only).
    #[must_use]
    pub fn icon_only(mut self) -> Self {
        self.show_message = false;
        self
    }

    /// Get the error message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Get the error code, if any.
    #[must_use = "use the error code (if any) for telemetry/diagnostics"]
    pub fn error_code(&self) -> Option<&'static str> {
        self.error_code
    }

    /// Calculate the minimum width needed to display the error.
    #[must_use]
    pub fn min_width(&self) -> u16 {
        let icon_width = display_width(self.icon.as_str()) as u16;
        if self.show_message && !self.message.is_empty() {
            let msg_width = display_width(self.message.as_str()) as u16;
            icon_width.saturating_add(1).saturating_add(msg_width)
        } else {
            icon_width
        }
    }
}

// ---------------------------------------------------------------------------
// ValidationErrorState
// ---------------------------------------------------------------------------

/// State for validation error animation and accessibility.
#[derive(Debug, Clone)]
pub struct ValidationErrorState {
    /// Whether the error is currently visible.
    visible: bool,
    /// Animation start time.
    animation_start: Option<Instant>,
    /// Current opacity (0.0 = hidden, 1.0 = fully visible).
    opacity: f32,
    /// Whether the error was just shown (for screen reader announcement).
    just_shown: bool,
    /// Unique ID for ARIA association.
    aria_id: u32,
}

impl Default for ValidationErrorState {
    fn default() -> Self {
        Self {
            visible: false,
            animation_start: None,
            opacity: 0.0,
            just_shown: false,
            aria_id: 0,
        }
    }
}

impl ValidationErrorState {
    /// Create a new state with the given ARIA ID.
    #[must_use]
    pub fn with_aria_id(mut self, id: u32) -> Self {
        self.aria_id = id;
        self
    }

    /// Show the error (triggers animation).
    pub fn show(&mut self) {
        if !self.visible {
            self.visible = true;
            self.animation_start = Some(Instant::now());
            self.just_shown = true;
        }
    }

    /// Hide the error (triggers fade-out animation).
    pub fn hide(&mut self) {
        if self.visible {
            self.visible = false;
            self.animation_start = Some(Instant::now());
        }
    }

    /// Set visibility directly (for immediate show/hide).
    pub fn set_visible(&mut self, visible: bool) {
        if visible {
            self.show();
        } else {
            self.hide();
        }
    }

    /// Check if the error is currently visible.
    #[inline]
    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Check if the error is fully visible (animation complete).
    #[must_use]
    pub fn is_fully_visible(&self) -> bool {
        self.visible && self.opacity >= 1.0
    }

    /// Get the current opacity.
    #[must_use]
    pub fn opacity(&self) -> f32 {
        self.opacity
    }

    /// Check and clear the "just shown" flag (for screen reader announcements).
    pub fn take_just_shown(&mut self) -> bool {
        std::mem::take(&mut self.just_shown)
    }

    /// Get the ARIA ID for accessibility association.
    #[must_use]
    pub fn aria_id(&self) -> u32 {
        self.aria_id
    }

    /// Update animation state. Call this each frame.
    pub fn tick(&mut self, animation_duration: Duration) {
        if let Some(start) = self.animation_start {
            let elapsed = start.elapsed();
            let progress = if animation_duration.is_zero() {
                1.0
            } else {
                (elapsed.as_secs_f32() / animation_duration.as_secs_f32()).clamp(0.0, 1.0)
            };

            if self.visible {
                // Fade in
                self.opacity = progress;
            } else {
                // Fade out
                self.opacity = 1.0 - progress;
            }

            // Animation complete
            if progress >= 1.0 {
                self.animation_start = None;
                self.opacity = if self.visible { 1.0 } else { 0.0 };
            }
        }
    }

    /// Check if an animation is currently in progress.
    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.animation_start.is_some()
    }
}

// ---------------------------------------------------------------------------
// Widget Implementation
// ---------------------------------------------------------------------------

impl StatefulWidget for ValidationErrorDisplay {
    type State = ValidationErrorState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "ValidationErrorDisplay",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        if area.is_empty() || area.height < 1 {
            return;
        }

        // Update animation
        state.tick(self.animation_duration);

        // Skip rendering if fully invisible
        if state.opacity <= 0.0 && !state.visible {
            return;
        }

        let deg = frame.buffer.degradation;

        // Calculate effective opacity for styling
        let effective_opacity = (state.opacity * 255.0) as u8;

        // Adjust style with opacity
        let icon_style = if deg.apply_styling() && effective_opacity < 255 {
            let fg = self.icon_style.fg.unwrap_or(ERROR_FG_DEFAULT);
            Style::new().fg(PackedRgba::rgba(fg.r(), fg.g(), fg.b(), effective_opacity))
        } else if deg.apply_styling() {
            self.icon_style
        } else {
            Style::default()
        };

        let text_style = if deg.apply_styling() && effective_opacity < 255 {
            let fg = self.style.fg.unwrap_or(ERROR_FG_DEFAULT);
            Style::new().fg(PackedRgba::rgba(fg.r(), fg.g(), fg.b(), effective_opacity))
        } else if deg.apply_styling() {
            self.style
        } else {
            Style::default()
        };

        // Draw icon
        let y = area.y;
        let mut x = area.x;
        let max_x = area.right();

        x = draw_text_span(frame, x, y, &self.icon, icon_style, max_x);

        // Draw space separator
        if x < max_x && self.show_message && !self.message.is_empty() {
            x = x.saturating_add(1);

            // Draw message (truncate with ellipsis if needed)
            let remaining_width = max_x.saturating_sub(x) as usize;
            let msg_width = display_width(self.message.as_str());

            if msg_width <= remaining_width {
                draw_text_span(frame, x, y, &self.message, text_style, max_x);
            } else if remaining_width >= 4 {
                // Truncate with ellipsis
                let mut truncated = String::new();
                let mut w = 0;
                let limit = remaining_width.saturating_sub(1); // Leave room for "…"

                for grapheme in unicode_segmentation::UnicodeSegmentation::graphemes(
                    self.message.as_str(),
                    true,
                ) {
                    let gw = grapheme_width(grapheme);
                    if w + gw > limit {
                        break;
                    }
                    truncated.push_str(grapheme);
                    w += gw;
                }
                truncated.push('…');

                draw_text_span(frame, x, y, &truncated, text_style, max_x);
            } else if remaining_width >= 1 {
                // Just ellipsis
                draw_text_span(frame, x, y, "…", text_style, max_x);
            }
        }
    }
}

impl Widget for ValidationErrorDisplay {
    fn render(&self, area: Rect, frame: &mut Frame) {
        let mut state = ValidationErrorState {
            visible: true,
            opacity: 1.0,
            ..Default::default()
        };
        StatefulWidget::render(self, area, frame, &mut state);
    }

    fn is_essential(&self) -> bool {
        // Errors are essential - users need to see them
        true
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    // -- Construction tests --

    #[test]
    fn new_creates_with_message() {
        let error = ValidationErrorDisplay::new("Required field");
        assert_eq!(error.message(), "Required field");
        assert_eq!(error.error_code(), None);
    }

    #[test]
    fn with_code_sets_error_code() {
        let error = ValidationErrorDisplay::with_code("required", "This field is required");
        assert_eq!(error.error_code(), Some("required"));
        assert_eq!(error.message(), "This field is required");
    }

    #[test]
    fn with_icon_overrides_default() {
        let error = ValidationErrorDisplay::new("Error").with_icon("!");
        assert_eq!(error.icon, "!");
    }

    #[test]
    fn icon_only_disables_message() {
        let error = ValidationErrorDisplay::new("Error").icon_only();
        assert!(!error.show_message);
    }

    #[test]
    fn default_uses_warning_icon() {
        let error = ValidationErrorDisplay::default();
        assert_eq!(error.icon, ERROR_ICON_DEFAULT);
    }

    // -- Min width calculation --

    #[test]
    fn min_width_icon_only() {
        let error = ValidationErrorDisplay::new("Error").icon_only();
        // Warning icon is 1 cell wide (may vary by font but assume 2 for emoji)
        let icon_width = display_width(ERROR_ICON_DEFAULT) as u16;
        assert_eq!(error.min_width(), icon_width);
    }

    #[test]
    fn min_width_with_message() {
        let error = ValidationErrorDisplay::new("Error");
        let icon_width = display_width(ERROR_ICON_DEFAULT) as u16;
        let msg_width = 5u16; // "Error"
        assert_eq!(error.min_width(), icon_width + 1 + msg_width);
    }

    #[test]
    fn min_width_empty_message() {
        let error = ValidationErrorDisplay::new("");
        let icon_width = display_width(ERROR_ICON_DEFAULT) as u16;
        assert_eq!(error.min_width(), icon_width);
    }

    // -- State tests --

    #[test]
    fn state_default_is_hidden() {
        let state = ValidationErrorState::default();
        assert!(!state.is_visible());
        assert_eq!(state.opacity(), 0.0);
    }

    #[test]
    fn show_sets_visible_and_starts_animation() {
        let mut state = ValidationErrorState::default();
        state.show();
        assert!(state.is_visible());
        assert!(state.is_animating());
        assert!(state.take_just_shown());
    }

    #[test]
    fn hide_clears_visible() {
        let mut state = ValidationErrorState::default();
        state.show();
        state.opacity = 1.0;
        state.animation_start = None;
        state.hide();
        assert!(!state.is_visible());
        assert!(state.is_animating());
    }

    #[test]
    fn show_twice_is_noop() {
        let mut state = ValidationErrorState::default();
        state.show();
        let start1 = state.animation_start;
        state.just_shown = false;
        state.show();
        assert_eq!(state.animation_start, start1);
        assert!(!state.just_shown); // Not re-triggered
    }

    #[test]
    fn take_just_shown_clears_flag() {
        let mut state = ValidationErrorState::default();
        state.show();
        assert!(state.take_just_shown());
        assert!(!state.take_just_shown());
    }

    #[test]
    fn tick_advances_opacity() {
        let mut state = ValidationErrorState::default();
        state.show();
        // Simulate time passing
        state.animation_start = Some(Instant::now() - Duration::from_millis(100));
        state.tick(Duration::from_millis(150));
        assert!(state.opacity > 0.0);
        assert!(state.opacity < 1.0);
    }

    #[test]
    fn tick_completes_animation() {
        let mut state = ValidationErrorState::default();
        state.show();
        state.animation_start = Some(Instant::now() - Duration::from_millis(200));
        state.tick(Duration::from_millis(150));
        assert_eq!(state.opacity, 1.0);
        assert!(!state.is_animating());
    }

    #[test]
    fn tick_fade_out() {
        let mut state = ValidationErrorState {
            visible: false,
            opacity: 1.0,
            animation_start: Some(Instant::now() - Duration::from_millis(75)),
            ..Default::default()
        };
        state.tick(Duration::from_millis(150));
        assert!(state.opacity < 1.0);
        assert!(state.opacity > 0.0);
    }

    #[test]
    fn is_fully_visible_requires_complete_animation() {
        let mut state = ValidationErrorState::default();
        state.show();
        assert!(!state.is_fully_visible());
        state.opacity = 1.0;
        state.animation_start = None;
        assert!(state.is_fully_visible());
    }

    #[test]
    fn aria_id_can_be_set() {
        let state = ValidationErrorState::default().with_aria_id(42);
        assert_eq!(state.aria_id(), 42);
    }

    // -- Rendering tests --

    #[test]
    fn render_draws_icon() {
        let error = ValidationErrorDisplay::new("Error");
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        Widget::render(&error, area, &mut frame);

        // Icon should be at position 0
        // Note: emoji width varies, but we check something was drawn
        let cell = frame.buffer.get(0, 0).unwrap();
        assert!(!cell.is_empty());
    }

    #[test]
    fn render_draws_message() {
        let error = ValidationErrorDisplay::new("Required").with_icon("!");
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        Widget::render(&error, area, &mut frame);

        // "!" at 0, space at 1, "Required" starts at 2
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('!'));
        assert_eq!(frame.buffer.get(2, 0).unwrap().content.as_char(), Some('R'));
    }

    #[test]
    fn render_truncates_long_message() {
        let error = ValidationErrorDisplay::new("This is a very long error message").with_icon("!");
        let area = Rect::new(0, 0, 12, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(12, 1, &mut pool);
        Widget::render(&error, area, &mut frame);

        // Should end with ellipsis somewhere
        let mut found_ellipsis = false;
        for x in 0..12 {
            if let Some(cell) = frame.buffer.get(x, 0)
                && cell.content.as_char() == Some('…')
            {
                found_ellipsis = true;
                break;
            }
        }
        assert!(found_ellipsis);
    }

    #[test]
    fn render_empty_area_is_noop() {
        let error = ValidationErrorDisplay::new("Error");
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        let mut state = ValidationErrorState::default();
        StatefulWidget::render(&error, area, &mut frame, &mut state);
        // Should not panic
    }

    #[test]
    fn render_hidden_state_draws_nothing() {
        let error = ValidationErrorDisplay::new("Error");
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        let mut state = ValidationErrorState::default(); // Not visible
        StatefulWidget::render(&error, area, &mut frame, &mut state);

        // Nothing should be drawn
        assert!(frame.buffer.get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn render_visible_state_draws_content() {
        let error = ValidationErrorDisplay::new("Error").with_icon("!");
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        let mut state = ValidationErrorState {
            visible: true,
            opacity: 1.0,
            ..Default::default()
        };
        StatefulWidget::render(&error, area, &mut frame, &mut state);

        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('!'));
    }

    #[test]
    fn render_icon_only_mode() {
        let error = ValidationErrorDisplay::new("This error should not appear")
            .with_icon("X")
            .icon_only();
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        Widget::render(&error, area, &mut frame);

        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('X'));
        // Position 1+ should be empty (no message)
        assert!(frame.buffer.get(1, 0).unwrap().is_empty());
    }

    #[test]
    fn is_essential_returns_true() {
        let error = ValidationErrorDisplay::new("Error");
        assert!(error.is_essential());
    }

    // -- Property tests --

    #[test]
    fn opacity_always_clamped() {
        let mut state = ValidationErrorState::default();
        state.show();
        state.animation_start = Some(Instant::now() - Duration::from_secs(10));
        state.tick(Duration::from_millis(100));
        assert!(state.opacity >= 0.0);
        assert!(state.opacity <= 1.0);
    }

    #[test]
    fn animation_duration_zero_is_immediate() {
        let mut state = ValidationErrorState::default();
        state.show();
        state.tick(Duration::ZERO);
        assert_eq!(state.opacity, 1.0);
        assert!(!state.is_animating());
    }

    // -- Style tests --

    #[test]
    fn style_is_applied_to_message() {
        let custom_style = Style::new().fg(PackedRgba::rgb(100, 200, 50));
        let error = ValidationErrorDisplay::new("Error")
            .with_icon("!")
            .with_style(custom_style);
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        Widget::render(&error, area, &mut frame);

        // Check message cell has custom fg color
        let cell = frame.buffer.get(2, 0).unwrap(); // 'R' of "Required"
        assert_eq!(
            cell.fg,
            PackedRgba::rgb(100, 200, 50),
            "message cell should use the custom fg color"
        );
    }

    #[test]
    fn icon_style_separate_from_message_style() {
        let error = ValidationErrorDisplay::new("Error")
            .with_icon("!")
            .with_icon_style(Style::new().fg(PackedRgba::rgb(255, 255, 0)))
            .with_style(Style::new().fg(PackedRgba::rgb(255, 0, 0)));
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        Widget::render(&error, area, &mut frame);

        let icon_cell = frame.buffer.get(0, 0).unwrap();
        let msg_cell = frame.buffer.get(2, 0).unwrap();
        // Icon should have yellow, message should have red
        assert_ne!(icon_cell.fg, msg_cell.fg);
    }
}
