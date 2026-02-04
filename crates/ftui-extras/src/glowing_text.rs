#![forbid(unsafe_code)]

//! Animated text effects with glow, fade-in, and fade-out.
//!
//! This module provides widgets for displaying text with various animated
//! visual effects, useful for transitions, notifications, and visual flair.
//!
//! # Features
//!
//! - **Fade transitions**: Smooth fade-in and fade-out animations
//! - **Glow effects**: Configurable glow/bloom with color customization
//! - **Wave animation**: Text characters that oscillate
//! - **Typing effect**: Characters appearing one by one
//!
//! # Example
//!
//! ```rust,ignore
//! use ftui_extras::glowing_text::{GlowingText, TransitionOverlay};
//!
//! // Simple fade-in text
//! let text = GlowingText::new("Hello World")
//!     .glow_color(PackedRgba::rgb(100, 200, 255))
//!     .fade_in(0.5);  // Fade progress from 0.0 to 1.0
//!
//! // Full transition overlay with title and subtitle
//! let overlay = TransitionOverlay::new("Effect Name", "Description of what it does")
//!     .progress(transition_progress)  // 0.0 = invisible, 0.5 = peak, 1.0 = invisible
//!     .primary_color(accent_color);
//! ```

use ftui_core::geometry::Rect;
use ftui_render::cell::{Cell, CellAttrs, CellContent, PackedRgba, StyleFlags as CellStyleFlags};
use ftui_render::frame::Frame;
use ftui_text::{display_width, grapheme_width, graphemes};
use ftui_widgets::Widget;

// =============================================================================
// Color utilities for glow effects
// =============================================================================

/// Interpolate between two colors.
fn lerp_color(a: PackedRgba, b: PackedRgba, t: f64) -> PackedRgba {
    let t = t.clamp(0.0, 1.0);
    let r = (a.r() as f64 + (b.r() as f64 - a.r() as f64) * t) as u8;
    let g = (a.g() as f64 + (b.g() as f64 - a.g() as f64) * t) as u8;
    let b_val = (a.b() as f64 + (b.b() as f64 - a.b() as f64) * t) as u8;
    PackedRgba::rgb(r, g, b_val)
}

/// Apply alpha/brightness to a color.
fn apply_alpha(color: PackedRgba, alpha: f64) -> PackedRgba {
    let alpha = alpha.clamp(0.0, 1.0);
    PackedRgba::rgb(
        (color.r() as f64 * alpha) as u8,
        (color.g() as f64 * alpha) as u8,
        (color.b() as f64 * alpha) as u8,
    )
}

/// Create a glow color (lighter version of base).
fn glow_color(base: PackedRgba, intensity: f64) -> PackedRgba {
    let intensity = intensity.clamp(0.0, 1.0);
    let white = PackedRgba::rgb(255, 255, 255);
    lerp_color(base, white, intensity * 0.5)
}

// =============================================================================
// GlowingText - Single-line text with glow effect
// =============================================================================

/// A text widget with configurable glow and fade effects.
#[derive(Debug, Clone)]
pub struct GlowingText {
    text: String,
    base_color: PackedRgba,
    glow_color: PackedRgba,
    glow_intensity: f64,
    fade: f64,
    bold: bool,
}

impl GlowingText {
    /// Create a new glowing text widget.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            base_color: PackedRgba::rgb(255, 255, 255),
            glow_color: PackedRgba::rgb(100, 200, 255),
            glow_intensity: 0.0,
            fade: 1.0,
            bold: false,
        }
    }

    /// Set the base text color.
    pub fn color(mut self, color: PackedRgba) -> Self {
        self.base_color = color;
        self
    }

    /// Set the glow color (used for the glow effect).
    pub fn glow(mut self, color: PackedRgba) -> Self {
        self.glow_color = color;
        self
    }

    /// Set the glow intensity (0.0 = no glow, 1.0 = maximum glow).
    pub fn glow_intensity(mut self, intensity: f64) -> Self {
        self.glow_intensity = intensity.clamp(0.0, 1.0);
        self
    }

    /// Set the fade amount (0.0 = invisible, 1.0 = fully visible).
    pub fn fade(mut self, fade: f64) -> Self {
        self.fade = fade.clamp(0.0, 1.0);
        self
    }

    /// Make the text bold.
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// Calculate the effective color with glow and fade applied.
    fn effective_color(&self) -> PackedRgba {
        let base = if self.glow_intensity > 0.0 {
            let glowed = glow_color(self.base_color, self.glow_intensity);
            lerp_color(self.base_color, glowed, self.glow_intensity)
        } else {
            self.base_color
        };
        apply_alpha(base, self.fade)
    }

    /// Render to a frame at the specified position.
    pub fn render_at(&self, x: u16, y: u16, frame: &mut Frame) {
        if self.fade < 0.01 {
            return;
        }

        let color = self.effective_color();
        let mut flags = CellStyleFlags::empty();
        if self.bold {
            flags = flags.union(CellStyleFlags::BOLD);
        }
        let attrs = CellAttrs::new(flags, 0);

        let mut px = x;
        for grapheme in graphemes(self.text.as_str()) {
            let w = grapheme_width(grapheme);
            if w == 0 {
                continue;
            }
            let content = if w > 1 || grapheme.chars().count() > 1 {
                let id = frame.intern_with_width(grapheme, w as u8);
                CellContent::from_grapheme(id)
            } else if let Some(ch) = grapheme.chars().next() {
                CellContent::from_char(ch)
            } else {
                continue;
            };

            let mut cell = Cell::new(content);
            cell.fg = color;
            cell.attrs = attrs;
            frame.buffer.set(px, y, cell);

            px = px.saturating_add(w as u16);
        }
    }
}

impl Widget for GlowingText {
    fn render(self, area: Rect, frame: &mut Frame) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        self.render_at(area.x, area.y, frame);
    }
}

// =============================================================================
// TransitionOverlay - Full-screen fade-in/fade-out effect title overlay
// =============================================================================

/// A full-screen overlay for displaying transition text with fade effects.
///
/// Progress goes from 0.0 (invisible) to 0.5 (peak visibility) to 1.0 (invisible).
/// This creates a smooth fade-in then fade-out animation.
#[derive(Debug, Clone)]
pub struct TransitionOverlay {
    title: String,
    subtitle: String,
    progress: f64,
    primary_color: PackedRgba,
    secondary_color: PackedRgba,
    duration_ticks: u32,
}

impl TransitionOverlay {
    /// Create a new transition overlay with title and subtitle.
    pub fn new(title: impl Into<String>, subtitle: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            subtitle: subtitle.into(),
            progress: 0.0,
            primary_color: PackedRgba::rgb(255, 100, 200),
            secondary_color: PackedRgba::rgb(180, 180, 220),
            duration_ticks: 30,
        }
    }

    /// Set the transition progress (0.0 to 1.0).
    ///
    /// The fade follows a bell curve: 0.0 = invisible, 0.5 = peak, 1.0 = invisible.
    pub fn progress(mut self, progress: f64) -> Self {
        self.progress = progress.clamp(0.0, 1.0);
        self
    }

    /// Set the primary (title) color.
    pub fn primary_color(mut self, color: PackedRgba) -> Self {
        self.primary_color = color;
        self
    }

    /// Set the secondary (subtitle) color.
    pub fn secondary_color(mut self, color: PackedRgba) -> Self {
        self.secondary_color = color;
        self
    }

    /// Set the total duration in ticks.
    pub fn duration(mut self, ticks: u32) -> Self {
        self.duration_ticks = ticks;
        self
    }

    /// Calculate opacity from progress using a smooth bell curve.
    fn opacity(&self) -> f64 {
        // Sine curve for smooth fade-in and fade-out
        // p=0 -> 0, p=0.5 -> 1, p=1 -> 0
        (self.progress * std::f64::consts::PI).sin()
    }

    /// Calculate glow intensity (peaks slightly after opacity peak).
    fn glow_intensity(&self) -> f64 {
        // Glow peaks at around p=0.3 and p=0.7
        let t = self.progress * 2.0;
        if t <= 1.0 {
            t * 0.8
        } else {
            (2.0 - t) * 0.8
        }
    }

    /// Check if the overlay is visible (has non-zero opacity).
    pub fn is_visible(&self) -> bool {
        self.opacity() > 0.01
    }
}

impl Widget for TransitionOverlay {
    fn render(self, area: Rect, frame: &mut Frame) {
        let opacity = self.opacity();
        if opacity < 0.01 || area.width < 10 || area.height < 3 {
            return;
        }

        let glow = self.glow_intensity();

        // Center the title
        let title_len = display_width(&self.title) as u16;
        let title_x = area.x + area.width.saturating_sub(title_len) / 2;
        let title_y = area.y + area.height / 2;

        // Render title with glow
        let title_text = GlowingText::new(&self.title)
            .color(self.primary_color)
            .glow(self.primary_color)
            .glow_intensity(glow)
            .fade(opacity)
            .bold();
        title_text.render_at(title_x, title_y, frame);

        // Render subtitle below (if there's room and subtitle exists)
        if !self.subtitle.is_empty() && title_y + 1 < area.y + area.height {
            let subtitle_len = display_width(&self.subtitle) as u16;
            let subtitle_x = area.x + area.width.saturating_sub(subtitle_len) / 2;
            let subtitle_y = title_y + 1;

            let subtitle_text = GlowingText::new(&self.subtitle)
                .color(self.secondary_color)
                .glow(self.secondary_color)
                .glow_intensity(glow * 0.5)
                .fade(opacity * 0.9);
            subtitle_text.render_at(subtitle_x, subtitle_y, frame);
        }

        // Optional: render decorative glow "halo" around title
        if glow > 0.3 && title_y > 0 {
            let halo_chars = "~ ~ ~";
            let halo_len = display_width(halo_chars) as u16;
            let halo_x = area.x + area.width.saturating_sub(halo_len) / 2;

            // Above title
            if title_y > area.y {
                let halo_above = GlowingText::new(halo_chars)
                    .color(self.primary_color)
                    .fade(opacity * 0.3);
                halo_above.render_at(halo_x, title_y - 1, frame);
            }

            // Below subtitle
            let bottom_y = if self.subtitle.is_empty() {
                title_y + 1
            } else {
                title_y + 2
            };
            if bottom_y < area.y + area.height {
                let halo_below = GlowingText::new(halo_chars)
                    .color(self.primary_color)
                    .fade(opacity * 0.3);
                halo_below.render_at(halo_x, bottom_y, frame);
            }
        }
    }
}

// =============================================================================
// TransitionState - Helper for managing transition animations
// =============================================================================

/// Helper struct for managing transition animation state.
#[derive(Debug, Clone, Default)]
pub struct TransitionState {
    /// Current progress (0.0 to 1.0).
    progress: f64,
    /// Whether transition is active.
    active: bool,
    /// Speed of transition per tick.
    speed: f64,
    /// Title to display.
    title: String,
    /// Subtitle to display.
    subtitle: String,
    /// Primary color.
    color: PackedRgba,
}

impl TransitionState {
    /// Create a new transition state with default settings.
    pub fn new() -> Self {
        Self {
            progress: 0.0,
            active: false,
            speed: 0.05,
            title: String::new(),
            subtitle: String::new(),
            color: PackedRgba::rgb(255, 100, 200),
        }
    }

    /// Start a new transition with the given title and subtitle.
    pub fn start(&mut self, title: impl Into<String>, subtitle: impl Into<String>, color: PackedRgba) {
        self.title = title.into();
        self.subtitle = subtitle.into();
        self.color = color;
        self.progress = 0.0;
        self.active = true;
    }

    /// Set the transition speed (progress per tick, default 0.05).
    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed.clamp(0.01, 0.5);
    }

    /// Update the transition (call this every tick).
    pub fn tick(&mut self) {
        if self.active {
            self.progress += self.speed;
            if self.progress >= 1.0 {
                self.progress = 1.0;
                self.active = false;
            }
        }
    }

    /// Check if the transition is currently visible.
    pub fn is_visible(&self) -> bool {
        self.active || self.progress > 0.0 && self.progress < 1.0
    }

    /// Get a TransitionOverlay widget for the current state.
    pub fn overlay(&self) -> TransitionOverlay {
        TransitionOverlay::new(&self.title, &self.subtitle)
            .progress(self.progress)
            .primary_color(self.color)
    }

    /// Get the current progress.
    pub fn progress(&self) -> f64 {
        self.progress
    }

    /// Check if transition is active.
    pub fn is_active(&self) -> bool {
        self.active
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lerp_color() {
        let black = PackedRgba::rgb(0, 0, 0);
        let white = PackedRgba::rgb(255, 255, 255);

        let mid = lerp_color(black, white, 0.5);
        assert_eq!(mid.r(), 127);
        assert_eq!(mid.g(), 127);
        assert_eq!(mid.b(), 127);

        let zero = lerp_color(black, white, 0.0);
        assert_eq!(zero.r(), 0);

        let one = lerp_color(black, white, 1.0);
        assert_eq!(one.r(), 255);
    }

    #[test]
    fn test_apply_alpha() {
        let color = PackedRgba::rgb(200, 100, 50);

        let half = apply_alpha(color, 0.5);
        assert_eq!(half.r(), 100);
        assert_eq!(half.g(), 50);
        assert_eq!(half.b(), 25);

        let zero = apply_alpha(color, 0.0);
        assert_eq!(zero.r(), 0);
    }

    #[test]
    fn test_glowing_text_builder() {
        let text = GlowingText::new("test")
            .color(PackedRgba::rgb(255, 0, 0))
            .glow(PackedRgba::rgb(0, 255, 0))
            .glow_intensity(0.5)
            .fade(0.8)
            .bold();

        assert_eq!(text.text, "test");
        assert!(text.bold);
    }

    #[test]
    fn test_transition_overlay_opacity() {
        let overlay = TransitionOverlay::new("Title", "Subtitle");

        // At progress 0.0, opacity should be ~0
        let o0 = overlay.clone().progress(0.0);
        assert!(o0.opacity() < 0.01);

        // At progress 0.5, opacity should be ~1
        let o5 = overlay.clone().progress(0.5);
        assert!((o5.opacity() - 1.0).abs() < 0.01);

        // At progress 1.0, opacity should be ~0
        let o1 = overlay.progress(1.0);
        assert!(o1.opacity() < 0.01);
    }

    #[test]
    fn test_transition_state() {
        let mut state = TransitionState::new();
        assert!(!state.is_active());

        state.start("Test", "Description", PackedRgba::rgb(255, 100, 200));
        assert!(state.is_active());
        assert_eq!(state.progress(), 0.0);

        // Tick several times
        for _ in 0..10 {
            state.tick();
        }
        assert!(state.progress() > 0.0);
        assert!(state.progress() <= 1.0);

        // Tick until done
        for _ in 0..100 {
            state.tick();
        }
        assert!(!state.is_active());
        assert!((state.progress() - 1.0).abs() < 0.01);
    }
}
