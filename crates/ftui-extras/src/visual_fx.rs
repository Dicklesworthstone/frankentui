#![forbid(unsafe_code)]

//! Visual FX primitives (feature-gated).
//!
//! This module defines the stable core types used by higher-level visual FX:
//! - background-only "backdrop" effects
//! - optional quality tiers
//! - theme input plumbing (resolved theme colors)
//!
//! Design goals:
//! - **Deterministic**: given the same inputs, output should be identical.
//! - **No per-frame allocations required**: effects should reuse internal buffers.
//! - **Tiny-area safe**: width/height may be zero; must not panic.
//!
//! # Theme Boundary
//!
//! `ThemeInputs` is the **sole theme boundary** for FX modules. Visual effects
//! consume only this struct and never perform global theme lookups. Conversions
//! from the theme systems (`ThemePalette`, `ResolvedTheme`) are explicit and
//! cacheable at the app/screen level.

use ftui_core::geometry::Rect;
use ftui_render::cell::PackedRgba;
use ftui_render::frame::Frame;
use ftui_widgets::Widget;
use std::cell::RefCell;

#[cfg(feature = "theme")]
use crate::theme::ThemePalette;

/// Quality hint for FX implementations.
///
/// The mapping from runtime degradation/budgets is handled elsewhere; this enum is
/// a stable "dial" so FX code can implement graceful degradation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FxQuality {
    Low,
    Medium,
    High,
}

/// Resolved theme inputs for FX.
///
/// This is the **sole theme boundary** for visual FX modules. Effects consume only
/// this struct and never perform global theme lookups. This keeps FX code free of
/// cyclic dependencies and makes theme resolution explicit and cacheable.
///
/// # Design
///
/// - **Data-only**: No methods that access global theme state.
/// - **Small and cheap**: Pass by reference; fits in a few cache lines.
/// - **Opaque backgrounds**: `bg_base` and `bg_surface` should be opaque so Backdrop
///   output is deterministic regardless of existing buffer state.
/// - **Sufficient for FX**: Contains all slots needed by Metaballs/Plasma without
///   hardcoding demo palettes.
///
/// # Conversions
///
/// Explicit `From` implementations exist for:
/// - `ThemePalette` (ftui-extras theme system)
/// - `ResolvedTheme` (ftui-style theme system) - requires `ftui-style` dep
///
/// Conversions are cacheable at the app/screen level (recompute on theme change).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ThemeInputs {
    /// Opaque base background color (deepest layer).
    pub bg_base: PackedRgba,
    /// Opaque surface background (cards, panels).
    pub bg_surface: PackedRgba,
    /// Overlay/scrim color (used for legibility layers).
    pub bg_overlay: PackedRgba,
    /// Primary foreground/text color.
    pub fg_primary: PackedRgba,
    /// Muted foreground (secondary text, disabled states).
    pub fg_muted: PackedRgba,
    /// Primary accent color.
    pub accent_primary: PackedRgba,
    /// Secondary accent color.
    pub accent_secondary: PackedRgba,
    /// Additional accent slots for palettes/presets (keep small).
    pub accent_slots: [PackedRgba; 4],
}

impl ThemeInputs {
    /// Create a new `ThemeInputs` with all slots specified.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        bg_base: PackedRgba,
        bg_surface: PackedRgba,
        bg_overlay: PackedRgba,
        fg_primary: PackedRgba,
        fg_muted: PackedRgba,
        accent_primary: PackedRgba,
        accent_secondary: PackedRgba,
        accent_slots: [PackedRgba; 4],
    ) -> Self {
        Self {
            bg_base,
            bg_surface,
            bg_overlay,
            fg_primary,
            fg_muted,
            accent_primary,
            accent_secondary,
            accent_slots,
        }
    }

    /// Sensible defaults: dark base, light foreground, neutral accents.
    ///
    /// Use this for fallback/testing when no theme is available.
    #[inline]
    pub const fn default_dark() -> Self {
        Self {
            bg_base: PackedRgba::rgb(26, 31, 41),
            bg_surface: PackedRgba::rgb(30, 36, 48),
            bg_overlay: PackedRgba::rgba(45, 55, 70, 180),
            fg_primary: PackedRgba::rgb(179, 244, 255),
            fg_muted: PackedRgba::rgb(127, 147, 166),
            accent_primary: PackedRgba::rgb(0, 170, 255),
            accent_secondary: PackedRgba::rgb(255, 0, 255),
            accent_slots: [
                PackedRgba::rgb(57, 255, 180),
                PackedRgba::rgb(255, 229, 102),
                PackedRgba::rgb(255, 51, 102),
                PackedRgba::rgb(0, 255, 255),
            ],
        }
    }

    /// Light theme defaults for testing.
    #[inline]
    pub const fn default_light() -> Self {
        Self {
            bg_base: PackedRgba::rgb(238, 241, 245),
            bg_surface: PackedRgba::rgb(230, 235, 241),
            bg_overlay: PackedRgba::rgba(220, 227, 236, 200),
            fg_primary: PackedRgba::rgb(31, 41, 51),
            fg_muted: PackedRgba::rgb(123, 135, 148),
            accent_primary: PackedRgba::rgb(37, 99, 235),
            accent_secondary: PackedRgba::rgb(124, 58, 237),
            accent_slots: [
                PackedRgba::rgb(22, 163, 74),
                PackedRgba::rgb(245, 158, 11),
                PackedRgba::rgb(220, 38, 38),
                PackedRgba::rgb(14, 165, 233),
            ],
        }
    }
}

impl Default for ThemeInputs {
    fn default() -> Self {
        Self::default_dark()
    }
}

// ---------------------------------------------------------------------------
// Conversion: ftui_extras::theme::ThemePalette -> ThemeInputs (requires "theme")
// ---------------------------------------------------------------------------

#[cfg(feature = "theme")]
impl From<&ThemePalette> for ThemeInputs {
    fn from(palette: &ThemePalette) -> Self {
        Self {
            bg_base: palette.bg_base,
            bg_surface: palette.bg_surface,
            bg_overlay: palette.bg_overlay,
            fg_primary: palette.fg_primary,
            fg_muted: palette.fg_muted,
            accent_primary: palette.accent_primary,
            accent_secondary: palette.accent_secondary,
            accent_slots: [
                palette.accent_slots[0],
                palette.accent_slots[1],
                palette.accent_slots[2],
                palette.accent_slots[3],
            ],
        }
    }
}

#[cfg(feature = "theme")]
impl From<ThemePalette> for ThemeInputs {
    fn from(palette: ThemePalette) -> Self {
        Self::from(&palette)
    }
}

// ---------------------------------------------------------------------------
// Conversion: ftui_style::theme::ResolvedTheme -> ThemeInputs
// ---------------------------------------------------------------------------

/// Convert an `ftui_style::color::Color` to `PackedRgba`.
///
/// This always produces an opaque color (alpha = 255).
fn color_to_packed(color: ftui_style::color::Color) -> PackedRgba {
    let rgb = color.to_rgb();
    PackedRgba::rgb(rgb.r, rgb.g, rgb.b)
}

impl From<ftui_style::theme::ResolvedTheme> for ThemeInputs {
    /// Convert from `ftui_style::theme::ResolvedTheme`.
    ///
    /// Maps semantic slots as follows:
    /// - `background` -> `bg_base`
    /// - `surface` -> `bg_surface`
    /// - `overlay` -> `bg_overlay`
    /// - `text` -> `fg_primary`
    /// - `text_muted` -> `fg_muted`
    /// - `primary` -> `accent_primary`
    /// - `secondary` -> `accent_secondary`
    /// - `accent`, `success`, `warning`, `error` -> `accent_slots[0..4]`
    fn from(theme: ftui_style::theme::ResolvedTheme) -> Self {
        Self {
            bg_base: color_to_packed(theme.background),
            bg_surface: color_to_packed(theme.surface),
            bg_overlay: color_to_packed(theme.overlay),
            fg_primary: color_to_packed(theme.text),
            fg_muted: color_to_packed(theme.text_muted),
            accent_primary: color_to_packed(theme.primary),
            accent_secondary: color_to_packed(theme.secondary),
            accent_slots: [
                color_to_packed(theme.accent),
                color_to_packed(theme.success),
                color_to_packed(theme.warning),
                color_to_packed(theme.error),
            ],
        }
    }
}

impl From<&ftui_style::theme::ResolvedTheme> for ThemeInputs {
    fn from(theme: &ftui_style::theme::ResolvedTheme) -> Self {
        Self::from(*theme)
    }
}

/// Call-site provided render context.
///
/// `BackdropFx` renders into a caller-owned `out` buffer using a row-major layout:
/// `out[(y * width + x)]` for 0 <= x < width, 0 <= y < height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FxContext<'a> {
    pub width: u16,
    pub height: u16,
    pub frame: u64,
    pub time_seconds: f64,
    pub quality: FxQuality,
    pub theme: &'a ThemeInputs,
}

impl<'a> FxContext<'a> {
    #[inline]
    pub const fn len(&self) -> usize {
        self.width as usize * self.height as usize
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

// ---------------------------------------------------------------------------
// Contrast + opacity helpers
// ---------------------------------------------------------------------------

/// Minimum safe scrim opacity for bounded modes.
pub const SCRIM_OPACITY_MIN: f32 = 0.05;
/// Maximum safe scrim opacity for bounded modes.
pub const SCRIM_OPACITY_MAX: f32 = 0.85;

/// Clamp a scrim opacity into safe bounds.
#[inline]
pub fn clamp_scrim_opacity(opacity: f32) -> f32 {
    opacity.clamp(SCRIM_OPACITY_MIN, SCRIM_OPACITY_MAX)
}

#[inline]
fn clamp_opacity(opacity: f32) -> f32 {
    opacity.clamp(0.0, 1.0)
}

#[inline]
fn linearize_srgb(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Relative luminance in [0.0, 1.0] (sRGB, WCAG).
#[inline]
pub fn luminance(color: PackedRgba) -> f32 {
    let r = linearize_srgb(color.r() as f32 / 255.0);
    let g = linearize_srgb(color.g() as f32 / 255.0);
    let b = linearize_srgb(color.b() as f32 / 255.0);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Contrast ratio between two colors (>= 1.0).
#[inline]
pub fn contrast_ratio(fg: PackedRgba, bg: PackedRgba) -> f32 {
    let l1 = luminance(fg);
    let l2 = luminance(bg);
    let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
}

/// Background-only effect that renders into a caller-owned pixel buffer.
///
/// Invariants:
/// - Implementations must tolerate `width == 0` or `height == 0` (no panic).
/// - `out.len()` is expected to equal `ctx.width * ctx.height`. Implementations may
///   debug-assert this but should not rely on it for safety.
/// - Implementations should avoid per-frame allocations; reuse internal state.
pub trait BackdropFx {
    /// Human-readable name (used for debugging / UI).
    fn name(&self) -> &'static str;

    /// Optional resize hook so effects can (re)allocate caches deterministically.
    fn resize(&mut self, _width: u16, _height: u16) {}

    /// Render into `out` (row-major, width*height).
    fn render(&mut self, ctx: FxContext<'_>, out: &mut [PackedRgba]);
}

// ---------------------------------------------------------------------------
// Backdrop widget: effect buffer + composition + scrim
// ---------------------------------------------------------------------------

/// Optional scrim overlay to improve foreground legibility over a moving backdrop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scrim {
    Off,
    /// Uniform overlay using `ThemeInputs.bg_overlay` (or a custom color).
    Uniform {
        opacity: ScrimOpacity,
        color: Option<PackedRgba>,
    },
    /// Vertical fade from `top_opacity` to `bottom_opacity`.
    VerticalFade {
        top_opacity: ScrimOpacity,
        bottom_opacity: ScrimOpacity,
        color: Option<PackedRgba>,
    },
    /// Darken edges more than the center.
    Vignette {
        strength: ScrimOpacity,
        color: Option<PackedRgba>,
    },
}

impl Scrim {
    /// Uniform scrim using the theme overlay color (bounded opacity).
    pub fn uniform(opacity: f32) -> Self {
        Self::Uniform {
            opacity: ScrimOpacity::bounded(opacity),
            color: None,
        }
    }

    /// Uniform scrim using the theme overlay color (unbounded opacity).
    pub fn uniform_raw(opacity: f32) -> Self {
        Self::Uniform {
            opacity: ScrimOpacity::raw(opacity),
            color: None,
        }
    }

    /// Uniform scrim using a custom color (bounded opacity).
    pub fn uniform_color(color: PackedRgba, opacity: f32) -> Self {
        Self::Uniform {
            opacity: ScrimOpacity::bounded(opacity),
            color: Some(color),
        }
    }

    /// Uniform scrim using a custom color (unbounded opacity).
    pub fn uniform_color_raw(color: PackedRgba, opacity: f32) -> Self {
        Self::Uniform {
            opacity: ScrimOpacity::raw(opacity),
            color: Some(color),
        }
    }

    /// Vertical fade scrim using the theme overlay color (bounded opacity).
    pub fn vertical_fade(top_opacity: f32, bottom_opacity: f32) -> Self {
        Self::VerticalFade {
            top_opacity: ScrimOpacity::bounded(top_opacity),
            bottom_opacity: ScrimOpacity::bounded(bottom_opacity),
            color: None,
        }
    }

    /// Vertical fade scrim using a custom color (bounded opacity).
    pub fn vertical_fade_color(color: PackedRgba, top_opacity: f32, bottom_opacity: f32) -> Self {
        Self::VerticalFade {
            top_opacity: ScrimOpacity::bounded(top_opacity),
            bottom_opacity: ScrimOpacity::bounded(bottom_opacity),
            color: Some(color),
        }
    }

    /// Vignette scrim using the theme overlay color (bounded strength).
    pub fn vignette(strength: f32) -> Self {
        Self::Vignette {
            strength: ScrimOpacity::bounded(strength),
            color: None,
        }
    }

    /// Vignette scrim using a custom color (bounded strength).
    pub fn vignette_color(color: PackedRgba, strength: f32) -> Self {
        Self::Vignette {
            strength: ScrimOpacity::bounded(strength),
            color: Some(color),
        }
    }

    /// Default scrim preset for text-heavy panels.
    pub fn text_panel_default() -> Self {
        Self::vertical_fade(0.12, 0.35)
    }

    fn color_or_theme(color: Option<PackedRgba>, theme: &ThemeInputs) -> PackedRgba {
        color.unwrap_or(theme.bg_overlay)
    }

    #[inline]
    fn lerp(a: f32, b: f32, t: f32) -> f32 {
        a + (b - a) * t
    }

    fn overlay_at(self, theme: &ThemeInputs, x: u16, y: u16, w: u16, h: u16) -> PackedRgba {
        match self {
            Scrim::Off => PackedRgba::TRANSPARENT,
            Scrim::Uniform { opacity, color } => {
                let opacity = opacity.resolve();
                Self::color_or_theme(color, theme).with_opacity(opacity)
            }
            Scrim::VerticalFade {
                top_opacity,
                bottom_opacity,
                color,
            } => {
                let top = top_opacity.resolve();
                let bottom = bottom_opacity.resolve();
                let t = if h <= 1 {
                    1.0
                } else {
                    y as f32 / (h as f32 - 1.0)
                };
                let opacity = Self::lerp(top, bottom, t).clamp(0.0, 1.0);
                Self::color_or_theme(color, theme).with_opacity(opacity)
            }
            Scrim::Vignette { strength, color } => {
                let strength = strength.resolve();
                if w <= 1 || h <= 1 {
                    return Self::color_or_theme(color, theme).with_opacity(strength);
                }

                // Normalized distance to center in [0, 1].
                let cx = (w as f64 - 1.0) * 0.5;
                let cy = (h as f64 - 1.0) * 0.5;
                let dx = (x as f64 - cx) / cx;
                let dy = (y as f64 - cy) / cy;
                let r = (dx * dx + dy * dy).sqrt().clamp(0.0, 1.0);

                // Smoothstep-ish curve to avoid a harsh ring.
                let t = r * r * (3.0 - 2.0 * r);
                let opacity = (strength as f64 * t) as f32;
                Self::color_or_theme(color, theme).with_opacity(opacity)
            }
        }
    }
}

/// Scrim opacity with explicit clamp mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrimOpacity {
    value: f32,
    clamp: ScrimClamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrimClamp {
    /// Clamp into safe bounds (prevents accidental extremes).
    Bounded,
    /// Clamp only to [0, 1] (explicit extremes allowed).
    Unbounded,
}

impl ScrimOpacity {
    pub const fn bounded(value: f32) -> Self {
        Self {
            value,
            clamp: ScrimClamp::Bounded,
        }
    }

    pub const fn raw(value: f32) -> Self {
        Self {
            value,
            clamp: ScrimClamp::Unbounded,
        }
    }

    fn resolve(self) -> f32 {
        match self.clamp {
            ScrimClamp::Bounded => clamp_scrim_opacity(self.value),
            ScrimClamp::Unbounded => clamp_opacity(self.value),
        }
    }
}

/// Backdrop widget: renders a [`BackdropFx`] into **cell backgrounds only**.
///
/// The Backdrop:
/// - never writes glyph content (preserves `cell.content`)
/// - uses an opaque base fill so results are deterministic regardless of prior buffer state
/// - owns/reuses an internal effect buffer (grow-only)
pub struct Backdrop {
    fx: RefCell<Box<dyn BackdropFx>>,
    fx_buf: RefCell<Vec<PackedRgba>>,
    last_size: RefCell<(u16, u16)>,

    theme: ThemeInputs,
    base_fill: PackedRgba,
    effect_opacity: f32,
    scrim: Scrim,
    quality: FxQuality,
    frame: u64,
    time_seconds: f64,
}

impl Backdrop {
    pub fn new(fx: Box<dyn BackdropFx>, theme: ThemeInputs) -> Self {
        let base_fill = theme.bg_surface;
        Self {
            fx: RefCell::new(fx),
            fx_buf: RefCell::new(Vec::new()),
            last_size: RefCell::new((0, 0)),
            theme,
            base_fill,
            effect_opacity: 0.35,
            scrim: Scrim::Off,
            quality: FxQuality::High,
            frame: 0,
            time_seconds: 0.0,
        }
    }

    #[inline]
    pub fn set_theme(&mut self, theme: ThemeInputs) {
        self.theme = theme;
        self.base_fill = self.theme.bg_surface;
    }

    #[inline]
    pub fn set_time(&mut self, frame: u64, time_seconds: f64) {
        self.frame = frame;
        self.time_seconds = time_seconds;
    }

    #[inline]
    pub fn set_quality(&mut self, quality: FxQuality) {
        self.quality = quality;
    }

    #[inline]
    pub fn set_effect_opacity(&mut self, opacity: f32) {
        self.effect_opacity = opacity.clamp(0.0, 1.0);
    }

    #[inline]
    pub fn set_scrim(&mut self, scrim: Scrim) {
        self.scrim = scrim;
    }

    fn base_fill_opaque(&self) -> PackedRgba {
        PackedRgba::rgb(self.base_fill.r(), self.base_fill.g(), self.base_fill.b())
    }

    /// Render `self` and then `child` in the same area.
    ///
    /// This is the simplest way to layer "markdown over animated background"
    /// without introducing any new layout semantics.
    #[inline]
    pub fn render_with<W: Widget + ?Sized>(&self, area: Rect, frame: &mut Frame, child: &W) {
        self.render(area, frame);
        child.render(area, frame);
    }

    /// Return a composable wrapper that renders `self` first, then `child`.
    #[inline]
    pub fn over<'a, W: Widget + ?Sized>(&'a self, child: &'a W) -> WithBackdrop<'a, Backdrop, W> {
        WithBackdrop::new(self, child)
    }
}

/// Render a backdrop widget, then a child widget, in the same area.
pub struct WithBackdrop<'a, B: Widget + ?Sized, W: Widget + ?Sized> {
    backdrop: &'a B,
    child: &'a W,
}

impl<'a, B: Widget + ?Sized, W: Widget + ?Sized> WithBackdrop<'a, B, W> {
    #[inline]
    pub const fn new(backdrop: &'a B, child: &'a W) -> Self {
        Self { backdrop, child }
    }
}

impl<B: Widget + ?Sized, W: Widget + ?Sized> Widget for WithBackdrop<'_, B, W> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        self.backdrop.render(area, frame);
        self.child.render(area, frame);
    }
}

impl Widget for Backdrop {
    fn render(&self, area: Rect, frame: &mut Frame) {
        let clipped = frame.buffer.current_scissor().intersection(&area);
        if clipped.is_empty() {
            return;
        }

        let w = clipped.width;
        let h = clipped.height;
        let len = w as usize * h as usize;

        // Grow-only buffer; never shrink.
        {
            let mut buf = self.fx_buf.borrow_mut();
            if buf.len() < len {
                buf.resize(len, PackedRgba::TRANSPARENT);
            }
            buf[..len].fill(PackedRgba::TRANSPARENT);
        }

        // Resize hook for effects that cache by dims.
        {
            let mut last = self.last_size.borrow_mut();
            if *last != (w, h) {
                self.fx.borrow_mut().resize(w, h);
                *last = (w, h);
            }
        }

        let ctx = FxContext {
            width: w,
            height: h,
            frame: self.frame,
            time_seconds: self.time_seconds,
            quality: self.quality,
            theme: &self.theme,
        };

        // Run the effect.
        {
            let mut fx = self.fx.borrow_mut();
            let mut buf = self.fx_buf.borrow_mut();
            fx.render(ctx, &mut buf[..len]);
        }

        let base = self.base_fill_opaque();
        let fx_opacity = self.effect_opacity.clamp(0.0, 1.0);
        let region_opacity = frame.buffer.current_opacity().clamp(0.0, 1.0);

        let buf = self.fx_buf.borrow();
        for dy in 0..h {
            for dx in 0..w {
                let idx = dy as usize * w as usize + dx as usize;
                let fx_color = buf[idx].with_opacity(fx_opacity);
                let mut bg = fx_color.over(base);
                bg = self.scrim.overlay_at(&self.theme, dx, dy, w, h).over(bg);

                if let Some(cell) = frame.buffer.get_mut(clipped.x + dx, clipped.y + dy) {
                    if region_opacity < 1.0 {
                        cell.bg = bg.with_opacity(region_opacity).over(cell.bg);
                    } else {
                        cell.bg = bg;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// PlasmaFx: Wave-based procedural background
// ---------------------------------------------------------------------------

/// Compute plasma wave value for a single cell.
///
/// Returns a value in `[0.0, 1.0]` representing the wave intensity at the given
/// normalized coordinates `(nx, ny)` where both are in `[0.0, 1.0]`.
///
/// The wave equation uses 6 trigonometric terms:
/// - v1: horizontal wave
/// - v2: vertical wave (slightly offset phase)
/// - v3: diagonal wave
/// - v4: radial wave from center
/// - v5: radial wave with offset center
/// - v6: interference pattern
///
/// # Determinism
///
/// Given identical inputs, this function always returns the same output.
/// No global state or randomness is used.
#[inline]
pub fn plasma_wave(nx: f64, ny: f64, time: f64) -> f64 {
    // Scale normalized coords to wave-space (matches original PlasmaState)
    let x = nx * 6.0;
    let y = ny * 6.0;

    // 6 wave components
    let v1 = (x * 1.5 + time).sin();
    let v2 = (y * 1.8 + time * 0.8).sin();
    let v3 = ((x + y) * 1.2 + time * 0.6).sin();
    let v4 = ((x * x + y * y).sqrt() * 2.0 - time * 1.2).sin();
    let v5 = (((x - 3.0).powi(2) + (y - 3.0).powi(2)).sqrt() * 1.8 + time).cos();
    let v6 = ((x * 2.0).sin() * (y * 2.0).cos() + time * 0.5).sin();

    // Average and normalize from [-1, 1] to [0, 1]
    let value = (v1 + v2 + v3 + v4 + v5 + v6) / 6.0;
    (value + 1.0) / 2.0
}

/// Simplified plasma wave for low-quality rendering.
///
/// Uses only 3 wave components (cheaper but still visually interesting).
#[inline]
pub fn plasma_wave_low(nx: f64, ny: f64, time: f64) -> f64 {
    let x = nx * 6.0;
    let y = ny * 6.0;

    // 3 simplified wave components
    let v1 = (x * 1.5 + time).sin();
    let v2 = (y * 1.8 + time * 0.8).sin();
    let v3 = ((x + y) * 1.2 + time * 0.6).sin();

    let value = (v1 + v2 + v3) / 3.0;
    (value + 1.0) / 2.0
}

/// Palette mode for plasma coloring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PlasmaPalette {
    /// Use theme accent colors for the gradient.
    #[default]
    ThemeAccents,
    /// Classic sunset gradient (purple -> pink -> orange -> yellow).
    Sunset,
    /// Ocean gradient (deep blue -> cyan -> seafoam).
    Ocean,
    /// Fire gradient (black -> red -> orange -> yellow -> white).
    Fire,
    /// Neon rainbow (full hue cycle).
    Neon,
    /// Cyberpunk (hot pink -> purple -> cyan).
    Cyberpunk,
}

impl PlasmaPalette {
    /// Map a normalized value [0, 1] to a color.
    pub fn color_at(&self, t: f64, theme: &ThemeInputs) -> PackedRgba {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::ThemeAccents => Self::theme_gradient(t, theme),
            Self::Sunset => Self::sunset(t),
            Self::Ocean => Self::ocean(t),
            Self::Fire => Self::fire(t),
            Self::Neon => Self::neon(t),
            Self::Cyberpunk => Self::cyberpunk(t),
        }
    }

    fn theme_gradient(t: f64, theme: &ThemeInputs) -> PackedRgba {
        // Blend through: bg_base -> accent_primary -> accent_secondary -> fg_primary
        if t < 0.33 {
            let s = t / 0.33;
            Self::lerp_color(theme.bg_surface, theme.accent_primary, s)
        } else if t < 0.66 {
            let s = (t - 0.33) / 0.33;
            Self::lerp_color(theme.accent_primary, theme.accent_secondary, s)
        } else {
            let s = (t - 0.66) / 0.34;
            Self::lerp_color(theme.accent_secondary, theme.fg_primary, s)
        }
    }

    fn sunset(t: f64) -> PackedRgba {
        // Deep purple -> hot pink -> orange -> yellow
        let (r, g, b) = if t < 0.33 {
            let s = t / 0.33;
            Self::lerp_rgb((80, 20, 120), (255, 50, 120), s)
        } else if t < 0.66 {
            let s = (t - 0.33) / 0.33;
            Self::lerp_rgb((255, 50, 120), (255, 150, 50), s)
        } else {
            let s = (t - 0.66) / 0.34;
            Self::lerp_rgb((255, 150, 50), (255, 255, 150), s)
        };
        PackedRgba::rgb(r, g, b)
    }

    fn ocean(t: f64) -> PackedRgba {
        // Deep blue -> cyan -> seafoam
        let (r, g, b) = if t < 0.5 {
            let s = t / 0.5;
            Self::lerp_rgb((10, 30, 100), (30, 180, 220), s)
        } else {
            let s = (t - 0.5) / 0.5;
            Self::lerp_rgb((30, 180, 220), (150, 255, 200), s)
        };
        PackedRgba::rgb(r, g, b)
    }

    fn fire(t: f64) -> PackedRgba {
        // Black -> dark red -> orange -> yellow -> white
        let (r, g, b) = if t < 0.2 {
            let s = t / 0.2;
            Self::lerp_rgb((0, 0, 0), (80, 10, 0), s)
        } else if t < 0.4 {
            let s = (t - 0.2) / 0.2;
            Self::lerp_rgb((80, 10, 0), (200, 50, 0), s)
        } else if t < 0.6 {
            let s = (t - 0.4) / 0.2;
            Self::lerp_rgb((200, 50, 0), (255, 150, 20), s)
        } else if t < 0.8 {
            let s = (t - 0.6) / 0.2;
            Self::lerp_rgb((255, 150, 20), (255, 230, 100), s)
        } else {
            let s = (t - 0.8) / 0.2;
            Self::lerp_rgb((255, 230, 100), (255, 255, 220), s)
        };
        PackedRgba::rgb(r, g, b)
    }

    fn neon(t: f64) -> PackedRgba {
        // Full hue cycle
        let hue = t * 360.0;
        Self::hsv_to_rgb(hue, 1.0, 1.0)
    }

    fn cyberpunk(t: f64) -> PackedRgba {
        // Hot pink -> purple -> cyan
        let (r, g, b) = if t < 0.5 {
            let s = t / 0.5;
            Self::lerp_rgb((255, 20, 150), (150, 50, 200), s)
        } else {
            let s = (t - 0.5) / 0.5;
            Self::lerp_rgb((150, 50, 200), (50, 220, 255), s)
        };
        PackedRgba::rgb(r, g, b)
    }

    #[inline]
    fn lerp_rgb(a: (u8, u8, u8), b: (u8, u8, u8), t: f64) -> (u8, u8, u8) {
        (
            (a.0 as f64 + (b.0 as f64 - a.0 as f64) * t) as u8,
            (a.1 as f64 + (b.1 as f64 - a.1 as f64) * t) as u8,
            (a.2 as f64 + (b.2 as f64 - a.2 as f64) * t) as u8,
        )
    }

    #[inline]
    fn lerp_color(a: PackedRgba, b: PackedRgba, t: f64) -> PackedRgba {
        let (r, g, blue) = Self::lerp_rgb((a.r(), a.g(), a.b()), (b.r(), b.g(), b.b()), t);
        PackedRgba::rgb(r, g, blue)
    }

    #[inline]
    fn hsv_to_rgb(h: f64, s: f64, v: f64) -> PackedRgba {
        let h = h % 360.0;
        let c = v * s;
        let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
        let m = v - c;

        let (r, g, b) = if h < 60.0 {
            (c, x, 0.0)
        } else if h < 120.0 {
            (x, c, 0.0)
        } else if h < 180.0 {
            (0.0, c, x)
        } else if h < 240.0 {
            (0.0, x, c)
        } else if h < 300.0 {
            (x, 0.0, c)
        } else {
            (c, 0.0, x)
        };

        PackedRgba::rgb(
            ((r + m) * 255.0) as u8,
            ((g + m) * 255.0) as u8,
            ((b + m) * 255.0) as u8,
        )
    }
}

/// Procedural plasma backdrop effect.
///
/// Renders a classic plasma effect using multiple overlapping sine waves.
/// The output is deterministic given the same inputs.
///
/// # Quality Tiers
///
/// - `High`: 6 trigonometric evaluations per cell, full quality
/// - `Medium`: Same as High (reserved for future downsampling)
/// - `Low`: 3 trigonometric evaluations per cell, simpler waves
///
/// # Example
///
/// ```ignore
/// let plasma = PlasmaFx::new(PlasmaPalette::Ocean);
/// let backdrop = Backdrop::new(Box::new(plasma), theme);
/// backdrop.render(area, &mut frame);
/// ```
#[derive(Debug, Clone)]
pub struct PlasmaFx {
    palette: PlasmaPalette,
}

impl PlasmaFx {
    /// Create a new plasma effect with the specified palette.
    #[inline]
    pub const fn new(palette: PlasmaPalette) -> Self {
        Self { palette }
    }

    /// Create a plasma effect using theme colors.
    #[inline]
    pub const fn theme() -> Self {
        Self::new(PlasmaPalette::ThemeAccents)
    }

    /// Create a plasma effect with the sunset palette.
    #[inline]
    pub const fn sunset() -> Self {
        Self::new(PlasmaPalette::Sunset)
    }

    /// Create a plasma effect with the ocean palette.
    #[inline]
    pub const fn ocean() -> Self {
        Self::new(PlasmaPalette::Ocean)
    }

    /// Create a plasma effect with the fire palette.
    #[inline]
    pub const fn fire() -> Self {
        Self::new(PlasmaPalette::Fire)
    }

    /// Create a plasma effect with the neon palette.
    #[inline]
    pub const fn neon() -> Self {
        Self::new(PlasmaPalette::Neon)
    }

    /// Create a plasma effect with the cyberpunk palette.
    #[inline]
    pub const fn cyberpunk() -> Self {
        Self::new(PlasmaPalette::Cyberpunk)
    }

    /// Set the palette.
    #[inline]
    pub fn set_palette(&mut self, palette: PlasmaPalette) {
        self.palette = palette;
    }
}

impl BackdropFx for PlasmaFx {
    fn name(&self) -> &'static str {
        "plasma"
    }

    fn render(&mut self, ctx: FxContext<'_>, out: &mut [PackedRgba]) {
        if ctx.is_empty() {
            return;
        }
        debug_assert_eq!(out.len(), ctx.len());

        let w = ctx.width as f64;
        let h = ctx.height as f64;
        let time = ctx.time_seconds;
        let use_low_quality = ctx.quality == FxQuality::Low;

        for dy in 0..ctx.height {
            for dx in 0..ctx.width {
                let idx = dy as usize * ctx.width as usize + dx as usize;

                // Normalized coordinates [0, 1]
                let nx = dx as f64 / w;
                let ny = dy as f64 / h;

                // Compute wave value based on quality
                let wave = if use_low_quality {
                    plasma_wave_low(nx, ny, time)
                } else {
                    plasma_wave(nx, ny, time)
                };

                out[idx] = self.palette.color_at(wave, ctx.theme);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::Cell;
    use ftui_render::grapheme_pool::GraphemePool;

    struct SolidBg;

    impl BackdropFx for SolidBg {
        fn name(&self) -> &'static str {
            "solid-bg"
        }

        fn render(&mut self, ctx: FxContext<'_>, out: &mut [PackedRgba]) {
            if ctx.width == 0 || ctx.height == 0 {
                return;
            }
            debug_assert_eq!(out.len(), ctx.len());
            out.fill(ctx.theme.bg_base);
        }
    }

    #[test]
    fn smoke_backdrop_fx_renders_without_panicking() {
        let theme = ThemeInputs::default_dark();
        let ctx = FxContext {
            width: 4,
            height: 3,
            frame: 0,
            time_seconds: 0.0,
            quality: FxQuality::Low,
            theme: &theme,
        };
        let mut out = vec![PackedRgba::TRANSPARENT; ctx.len()];

        let mut fx = SolidBg;
        fx.render(ctx, &mut out);

        assert!(out.iter().all(|&c| c == theme.bg_base));
    }

    #[test]
    fn tiny_area_is_safe() {
        let theme = ThemeInputs::default_dark();
        let mut fx = SolidBg;

        let ctx = FxContext {
            width: 0,
            height: 0,
            frame: 0,
            time_seconds: 0.0,
            quality: FxQuality::Low,
            theme: &theme,
        };
        let mut out = Vec::new();
        fx.render(ctx, &mut out);
    }

    #[test]
    fn theme_inputs_has_opaque_backgrounds() {
        let theme = ThemeInputs::default_dark();
        assert_eq!(theme.bg_base.a(), 255, "bg_base should be opaque");
        assert_eq!(theme.bg_surface.a(), 255, "bg_surface should be opaque");
        // bg_overlay can have alpha for scrim effects
    }

    #[test]
    fn default_dark_and_light_differ() {
        let dark = ThemeInputs::default_dark();
        let light = ThemeInputs::default_light();
        assert_ne!(dark.bg_base, light.bg_base);
        assert_ne!(dark.fg_primary, light.fg_primary);
    }

    #[test]
    fn theme_inputs_default_equals_default_dark() {
        assert_eq!(ThemeInputs::default(), ThemeInputs::default_dark());
    }

    // -----------------------------------------------------------------------
    // Tests for From<ThemePalette> conversion (requires "theme" feature)
    // -----------------------------------------------------------------------

    #[cfg(feature = "theme")]
    mod palette_conversion {
        use super::*;
        use crate::theme::{ThemeId, palette};

        #[test]
        fn theme_inputs_from_palette_is_deterministic() {
            let palette = palette(ThemeId::CyberpunkAurora);
            let inputs1 = ThemeInputs::from(palette);
            let inputs2 = ThemeInputs::from(palette);
            assert_eq!(inputs1, inputs2);
        }

        #[test]
        fn theme_inputs_from_all_palettes() {
            for id in ThemeId::ALL {
                let palette = palette(id);
                let inputs = ThemeInputs::from(palette);
                // Verify backgrounds are opaque
                assert_eq!(inputs.bg_base.a(), 255, "bg_base opaque for {:?}", id);
                assert_eq!(inputs.bg_surface.a(), 255, "bg_surface opaque for {:?}", id);
                // Verify accents are populated
                assert_ne!(inputs.accent_primary, PackedRgba::TRANSPARENT);
                assert_ne!(inputs.accent_secondary, PackedRgba::TRANSPARENT);
            }
        }

        #[test]
        fn conversion_from_ref_and_value_match() {
            let palette = palette(ThemeId::Darcula);
            let from_ref = ThemeInputs::from(palette); // palette is already &ThemePalette
            let from_val = ThemeInputs::from(*palette); // dereference to test From<ThemePalette>
            assert_eq!(from_ref, from_val);
        }
    }

    // -----------------------------------------------------------------------
    // Tests for From<ResolvedTheme> conversion (ftui_style)
    // -----------------------------------------------------------------------

    mod resolved_theme_conversion {
        use super::*;
        use ftui_style::theme::themes;

        #[test]
        fn theme_inputs_from_resolved_theme_is_deterministic() {
            let resolved = themes::dark().resolve(true);
            let inputs1 = ThemeInputs::from(resolved);
            let inputs2 = ThemeInputs::from(resolved);
            assert_eq!(inputs1, inputs2);
        }

        #[test]
        fn theme_inputs_from_resolved_theme_dark() {
            let resolved = themes::dark().resolve(true);
            let inputs = ThemeInputs::from(resolved);
            // Verify backgrounds are opaque
            assert_eq!(inputs.bg_base.a(), 255, "bg_base should be opaque");
            assert_eq!(inputs.bg_surface.a(), 255, "bg_surface should be opaque");
            assert_eq!(inputs.bg_overlay.a(), 255, "bg_overlay should be opaque");
            // Verify foregrounds are populated
            assert_ne!(inputs.fg_primary, PackedRgba::TRANSPARENT);
            assert_ne!(inputs.fg_muted, PackedRgba::TRANSPARENT);
        }

        #[test]
        fn theme_inputs_from_resolved_theme_light() {
            let resolved = themes::light().resolve(false);
            let inputs = ThemeInputs::from(resolved);
            // Verify it produces different colors than dark
            let dark_inputs = ThemeInputs::from(themes::dark().resolve(true));
            assert_ne!(inputs.bg_base, dark_inputs.bg_base);
        }

        #[test]
        fn theme_inputs_from_all_preset_themes() {
            for (name, theme) in [
                ("dark", themes::dark()),
                ("light", themes::light()),
                ("nord", themes::nord()),
                ("dracula", themes::dracula()),
                ("solarized_dark", themes::solarized_dark()),
                ("solarized_light", themes::solarized_light()),
                ("monokai", themes::monokai()),
            ] {
                let resolved = theme.resolve(true);
                let inputs = ThemeInputs::from(resolved);
                // All backgrounds should be opaque
                assert_eq!(inputs.bg_base.a(), 255, "bg_base opaque for {}", name);
                assert_eq!(inputs.bg_surface.a(), 255, "bg_surface opaque for {}", name);
            }
        }

        #[test]
        fn conversion_from_ref_and_value_match() {
            let resolved = themes::dark().resolve(true);
            let from_ref = ThemeInputs::from(&resolved);
            let from_val = ThemeInputs::from(resolved);
            assert_eq!(from_ref, from_val);
        }

        #[test]
        fn color_to_packed_produces_opaque() {
            use ftui_style::color::Color;
            let color = Color::rgb(100, 150, 200);
            let packed = super::super::color_to_packed(color);
            assert_eq!(packed.r(), 100);
            assert_eq!(packed.g(), 150);
            assert_eq!(packed.b(), 200);
            assert_eq!(packed.a(), 255);
        }

        #[test]
        fn accent_slots_populated_from_semantic_colors() {
            let resolved = themes::dark().resolve(true);
            let inputs = ThemeInputs::from(resolved);
            // accent_slots[0] is theme.accent
            // accent_slots[1] is theme.success
            // accent_slots[2] is theme.warning
            // accent_slots[3] is theme.error
            for slot in &inputs.accent_slots {
                assert_ne!(*slot, PackedRgba::TRANSPARENT);
            }
        }
    }

    #[test]
    fn backdrop_preserves_glyph_content() {
        let theme = ThemeInputs::default_dark();
        let backdrop = Backdrop::new(Box::new(SolidBg), theme);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 2, &mut pool);
        let area = Rect::new(0, 0, 4, 2);

        // Seed some glyphs (Backdrop must not erase them).
        frame.buffer.set(
            1,
            0,
            Cell::default()
                .with_char('A')
                .with_bg(PackedRgba::rgb(1, 2, 3)),
        );
        frame.buffer.set(
            2,
            1,
            Cell::default()
                .with_char('Z')
                .with_bg(PackedRgba::rgb(4, 5, 6)),
        );

        backdrop.render(area, &mut frame);

        assert_eq!(frame.buffer.get(1, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(frame.buffer.get(2, 1).unwrap().content.as_char(), Some('Z'));
    }

    #[test]
    fn backdrop_reuses_internal_buffer_for_same_size() {
        let theme = ThemeInputs::default_dark();
        let backdrop = Backdrop::new(Box::new(SolidBg), theme);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 4, &mut pool);
        let area = Rect::new(0, 0, 10, 4);

        backdrop.render(area, &mut frame);
        let cap1 = backdrop.fx_buf.borrow().capacity();
        backdrop.render(area, &mut frame);
        let cap2 = backdrop.fx_buf.borrow().capacity();

        assert_eq!(cap1, cap2);
    }

    struct WriteChar(char);

    impl Widget for WriteChar {
        fn render(&self, area: Rect, frame: &mut Frame) {
            if area.is_empty() {
                return;
            }
            frame.buffer.set(
                area.x,
                area.y,
                Cell::default()
                    .with_char(self.0)
                    .with_bg(PackedRgba::TRANSPARENT),
            );
        }
    }

    #[test]
    fn with_backdrop_renders_child_over_backdrop() {
        let theme = ThemeInputs::default_dark();
        let mut backdrop = Backdrop::new(Box::new(SolidBg), theme);
        backdrop.set_effect_opacity(1.0);

        let child = WriteChar('X');

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        let area = Rect::new(0, 0, 1, 1);

        let composed = WithBackdrop::new(&backdrop, &child);
        composed.render(area, &mut frame);

        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('X'));
        assert_eq!(cell.bg, theme.bg_base);
    }

    #[test]
    fn backdrop_render_with_is_equivalent_to_with_backdrop() {
        let theme = ThemeInputs::default_dark();
        let mut backdrop = Backdrop::new(Box::new(SolidBg), theme);
        backdrop.set_effect_opacity(1.0);

        let child = WriteChar('Y');

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        let area = Rect::new(0, 0, 1, 1);

        backdrop.render_with(area, &mut frame, &child);

        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('Y'));
        assert_eq!(cell.bg, theme.bg_base);
    }

    #[test]
    fn clamp_scrim_opacity_bounds() {
        assert_eq!(clamp_scrim_opacity(-1.0), SCRIM_OPACITY_MIN);
        assert_eq!(clamp_scrim_opacity(2.0), SCRIM_OPACITY_MAX);
        assert_eq!(clamp_scrim_opacity(0.4), 0.4);
    }

    #[test]
    fn luminance_black_white() {
        let black = PackedRgba::rgb(0, 0, 0);
        let white = PackedRgba::rgb(255, 255, 255);
        let l_black = luminance(black);
        let l_white = luminance(white);
        assert!(l_black <= 0.0001);
        assert!(l_white >= 0.999);
    }

    #[test]
    fn contrast_ratio_black_white_is_high() {
        let black = PackedRgba::rgb(0, 0, 0);
        let white = PackedRgba::rgb(255, 255, 255);
        let ratio = contrast_ratio(white, black);
        assert!(ratio > 20.0);
    }

    #[test]
    fn scrim_uniform_bounded_clamps_to_min() {
        let theme = ThemeInputs::default_dark();
        let scrim = Scrim::uniform(0.0);
        let overlay = scrim.overlay_at(&theme, 0, 0, 4, 4);
        let expected = theme.bg_overlay.with_opacity(SCRIM_OPACITY_MIN);
        assert_eq!(overlay, expected);
    }

    #[test]
    fn scrim_uniform_raw_allows_zero() {
        let theme = ThemeInputs::default_dark();
        let scrim = Scrim::uniform_raw(0.0);
        let overlay = scrim.overlay_at(&theme, 0, 0, 4, 4);
        assert_eq!(overlay.a(), 0);
        assert_eq!(overlay.r(), theme.bg_overlay.r());
        assert_eq!(overlay.g(), theme.bg_overlay.g());
        assert_eq!(overlay.b(), theme.bg_overlay.b());
    }

    #[test]
    fn scrim_vertical_fade_interpolates() {
        let theme = ThemeInputs::default_dark();
        let scrim = Scrim::vertical_fade(0.1, 0.5);
        let top = scrim.overlay_at(&theme, 0, 0, 1, 3);
        let mid = scrim.overlay_at(&theme, 0, 1, 1, 3);
        let bottom = scrim.overlay_at(&theme, 0, 2, 1, 3);

        let top_expected = theme.bg_overlay.with_opacity(0.1);
        let mid_expected = theme.bg_overlay.with_opacity(0.3);
        let bottom_expected = theme.bg_overlay.with_opacity(0.5);

        assert_eq!(top, top_expected);
        assert_eq!(mid, mid_expected);
        assert_eq!(bottom, bottom_expected);
    }

    #[test]
    fn scrim_vignette_edges_are_darker() {
        let theme = ThemeInputs::default_dark();
        let scrim = Scrim::vignette(0.6);
        let center = scrim.overlay_at(&theme, 2, 2, 5, 5).a();
        let edge = scrim.overlay_at(&theme, 0, 0, 5, 5).a();
        assert!(edge >= center);
    }
}
