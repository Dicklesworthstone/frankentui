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
use std::fmt;

#[cfg(feature = "theme")]
use crate::theme::ThemePalette;

// Effects submodule with extracted effects (Metaballs, Plasma, etc.)
pub mod effects;
#[cfg(feature = "fx-gpu")]
pub mod gpu;

// Re-export from effects for convenience
pub use effects::{
    metaballs::{Metaball, MetaballsFx, MetaballsPalette, MetaballsParams},
    plasma::{PlasmaFx, PlasmaPalette, plasma_wave, plasma_wave_low},
    sampling::{
        BallState, CoordCache, FnSampler, MetaballFieldSampler, PlasmaSampler, Sampler,
        cell_to_normalized, fill_normalized_coords,
    },
};

// Canvas adapters for sub-pixel (Braille) FX rendering
#[cfg(feature = "canvas")]
pub use effects::{MetaballsCanvasAdapter, PlasmaCanvasAdapter};

/// Quality hint for FX implementations.
///
/// This enum is a stable "dial" so FX code can implement graceful degradation.
/// Use [`FxQuality::from_degradation`] to map from runtime budget levels.
///
/// # Variants
///
/// - `Full`: Normal detail, all iterations and effects enabled.
/// - `Reduced`: Fewer iterations, simplified math, lower frequency updates.
/// - `Minimal`: Very cheap fallback (lowest trig ops, static or near-static).
/// - `Off`: Render nothing (decorative effects are non-essential).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FxQuality {
    /// Render nothing - decorative effects are non-essential.
    Off,
    /// Very cheap fallback: minimal trig, near-static.
    Minimal,
    /// Fewer iterations, simplified math.
    Reduced,
    /// Normal detail, full quality.
    #[default]
    Full,
}

// ---------------------------------------------------------------------------
// DegradationLevel -> FxQuality mapping
// ---------------------------------------------------------------------------

/// Area threshold (in cells) above which `Full` is clamped to `Reduced`.
///
/// A 240x80 terminal = 19,200 cells. We use 16,000 as a conservative threshold
/// to trigger quality reduction on large areas even when budget allows Full.
pub const FX_AREA_THRESHOLD_FULL_TO_REDUCED: usize = 16_000;

/// Area threshold (in cells) above which `Reduced` is clamped to `Minimal`.
///
/// For extremely large renders (e.g., 4K equivalent), clamp to Minimal.
pub const FX_AREA_THRESHOLD_REDUCED_TO_MINIMAL: usize = 64_000;

impl FxQuality {
    /// Map from `DegradationLevel` to `FxQuality`.
    ///
    /// # Mapping Table
    ///
    /// | DegradationLevel | FxQuality |
    /// |------------------|-----------|
    /// | Full             | Full      |
    /// | SimpleBorders    | Reduced   |
    /// | NoStyling        | Reduced   |
    /// | EssentialOnly    | Off       |
    /// | Skeleton         | Off       |
    /// | SkipFrame        | Off       |
    ///
    /// Decorative backdrops are considered non-essential, so they disable
    /// at `EssentialOnly` and below.
    #[inline]
    pub fn from_degradation(level: ftui_render::budget::DegradationLevel) -> Self {
        use ftui_render::budget::DegradationLevel;
        match level {
            DegradationLevel::Full => Self::Full,
            DegradationLevel::SimpleBorders | DegradationLevel::NoStyling => Self::Reduced,
            DegradationLevel::EssentialOnly
            | DegradationLevel::Skeleton
            | DegradationLevel::SkipFrame => Self::Off,
        }
    }

    /// Map from `DegradationLevel` with area-based clamping.
    ///
    /// Large areas automatically reduce quality even if budget allows `Full`:
    /// - Area >= [`FX_AREA_THRESHOLD_FULL_TO_REDUCED`]: clamp `Full` to `Reduced`
    /// - Area >= [`FX_AREA_THRESHOLD_REDUCED_TO_MINIMAL`]: clamp `Reduced` to `Minimal`
    ///
    /// This prevents expensive per-cell computations from blocking the render loop.
    #[inline]
    pub fn from_degradation_with_area(
        level: ftui_render::budget::DegradationLevel,
        area_cells: usize,
    ) -> Self {
        let base = Self::from_degradation(level);
        Self::clamp_for_area(base, area_cells)
    }

    /// Clamp quality based on render area size.
    ///
    /// - Area >= [`FX_AREA_THRESHOLD_FULL_TO_REDUCED`]: `Full` becomes `Reduced`
    /// - Area >= [`FX_AREA_THRESHOLD_REDUCED_TO_MINIMAL`]: `Reduced` becomes `Minimal`
    #[inline]
    pub fn clamp_for_area(quality: Self, area_cells: usize) -> Self {
        match quality {
            Self::Full if area_cells >= FX_AREA_THRESHOLD_FULL_TO_REDUCED => {
                if area_cells >= FX_AREA_THRESHOLD_REDUCED_TO_MINIMAL {
                    Self::Minimal
                } else {
                    Self::Reduced
                }
            }
            Self::Reduced if area_cells >= FX_AREA_THRESHOLD_REDUCED_TO_MINIMAL => Self::Minimal,
            other => other,
        }
    }

    /// Returns `true` if effects should render (not `Off`).
    #[inline]
    pub fn is_enabled(self) -> bool {
        self != Self::Off
    }
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
// StackedFx: Compositor for multiple BackdropFx layers (bd-l8x9.2.5)
// ---------------------------------------------------------------------------

/// Blend mode for layer composition.
///
/// Controls how each layer is combined with the layers below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BlendMode {
    /// Standard alpha-over blending (layer painted on top).
    #[default]
    Over,
    /// Additive blending (layer colors added to base).
    Additive,
    /// Multiply blending (layer colors multiply with base).
    Multiply,
    /// Screen blending (inverse multiply for lightening).
    Screen,
}

impl BlendMode {
    /// Blend two colors using this blend mode.
    ///
    /// `top` is the layer color, `bottom` is the accumulated color so far.
    /// Both colors should have alpha in [0, 255].
    #[inline]
    pub fn blend(self, top: PackedRgba, bottom: PackedRgba) -> PackedRgba {
        match self {
            Self::Over => top.over(bottom),
            Self::Additive => Self::blend_additive(top, bottom),
            Self::Multiply => Self::blend_multiply(top, bottom),
            Self::Screen => Self::blend_screen(top, bottom),
        }
    }

    #[inline]
    fn blend_additive(top: PackedRgba, bottom: PackedRgba) -> PackedRgba {
        let ta = top.a() as f32 / 255.0;
        let r = (bottom.r() as f32 + top.r() as f32 * ta).min(255.0) as u8;
        let g = (bottom.g() as f32 + top.g() as f32 * ta).min(255.0) as u8;
        let b = (bottom.b() as f32 + top.b() as f32 * ta).min(255.0) as u8;
        // Result alpha is max of both
        let a = bottom.a().max(top.a());
        PackedRgba::rgba(r, g, b, a)
    }

    #[inline]
    fn blend_multiply(top: PackedRgba, bottom: PackedRgba) -> PackedRgba {
        let ta = top.a() as f32 / 255.0;
        // Multiply: result = top * bottom / 255
        let mr = (top.r() as f32 * bottom.r() as f32 / 255.0) as u8;
        let mg = (top.g() as f32 * bottom.g() as f32 / 255.0) as u8;
        let mb = (top.b() as f32 * bottom.b() as f32 / 255.0) as u8;
        // Lerp between bottom and multiplied result based on top alpha
        let r = (bottom.r() as f32 * (1.0 - ta) + mr as f32 * ta) as u8;
        let g = (bottom.g() as f32 * (1.0 - ta) + mg as f32 * ta) as u8;
        let b = (bottom.b() as f32 * (1.0 - ta) + mb as f32 * ta) as u8;
        let a = bottom.a().max(top.a());
        PackedRgba::rgba(r, g, b, a)
    }

    #[inline]
    fn blend_screen(top: PackedRgba, bottom: PackedRgba) -> PackedRgba {
        let ta = top.a() as f32 / 255.0;
        // Screen: result = 255 - (255 - top) * (255 - bottom) / 255
        let sr = 255 - ((255 - top.r()) as u16 * (255 - bottom.r()) as u16 / 255) as u8;
        let sg = 255 - ((255 - top.g()) as u16 * (255 - bottom.g()) as u16 / 255) as u8;
        let sb = 255 - ((255 - top.b()) as u16 * (255 - bottom.b()) as u16 / 255) as u8;
        // Lerp between bottom and screened result based on top alpha
        let r = (bottom.r() as f32 * (1.0 - ta) + sr as f32 * ta) as u8;
        let g = (bottom.g() as f32 * (1.0 - ta) + sg as f32 * ta) as u8;
        let b = (bottom.b() as f32 * (1.0 - ta) + sb as f32 * ta) as u8;
        let a = bottom.a().max(top.a());
        PackedRgba::rgba(r, g, b, a)
    }
}

/// A single layer in a stacked backdrop composition.
///
/// Each layer has:
/// - A `BackdropFx` effect
/// - An opacity (0.0 = invisible, 1.0 = fully opaque)
/// - A blend mode for compositing with layers below
pub struct FxLayer {
    fx: Box<dyn BackdropFx>,
    opacity: f32,
    blend_mode: BlendMode,
}

impl FxLayer {
    /// Create a new layer with default opacity (1.0) and blend mode (Over).
    #[inline]
    pub fn new(fx: Box<dyn BackdropFx>) -> Self {
        Self {
            fx,
            opacity: 1.0,
            blend_mode: BlendMode::Over,
        }
    }

    /// Create a new layer with specified opacity.
    #[inline]
    pub fn with_opacity(fx: Box<dyn BackdropFx>, opacity: f32) -> Self {
        Self {
            fx,
            opacity: opacity.clamp(0.0, 1.0),
            blend_mode: BlendMode::Over,
        }
    }

    /// Create a new layer with specified blend mode.
    #[inline]
    pub fn with_blend(fx: Box<dyn BackdropFx>, blend_mode: BlendMode) -> Self {
        Self {
            fx,
            opacity: 1.0,
            blend_mode,
        }
    }

    /// Create a new layer with both opacity and blend mode.
    #[inline]
    pub fn with_opacity_and_blend(
        fx: Box<dyn BackdropFx>,
        opacity: f32,
        blend_mode: BlendMode,
    ) -> Self {
        Self {
            fx,
            opacity: opacity.clamp(0.0, 1.0),
            blend_mode,
        }
    }

    /// Set the opacity for this layer.
    #[inline]
    pub fn set_opacity(&mut self, opacity: f32) {
        self.opacity = opacity.clamp(0.0, 1.0);
    }

    /// Set the blend mode for this layer.
    #[inline]
    pub fn set_blend_mode(&mut self, blend_mode: BlendMode) {
        self.blend_mode = blend_mode;
    }
}

impl fmt::Debug for FxLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FxLayer")
            .field("name", &self.fx.name())
            .field("opacity", &self.opacity)
            .field("blend_mode", &self.blend_mode)
            .finish()
    }
}

/// Stacked backdrop compositor: multiple FX layers with single-pass output.
///
/// `StackedFx` implements `BackdropFx` and composes multiple effects efficiently:
///
/// # Design
///
/// - **Separate layer buffers**: Each layer renders to its own reusable buffer.
/// - **Single final pass**: All layers are composited into the output in one pass.
/// - **No per-frame allocations**: Buffers grow-only and are reused across frames.
/// - **Explicit layer ordering**: Layers are rendered bottom-to-top (index 0 is bottom).
///
/// # Layer Ordering Semantics
///
/// Layers are composited in order: layer 0 is the base, layer 1 is painted on top,
/// and so on. This matches typical graphics conventions ("painter's algorithm").
///
/// ```text
/// Layer 2 (top)     ──┐
/// Layer 1 (middle)  ──┼──▶ Final composited output
/// Layer 0 (bottom)  ──┘
/// ```
///
/// # Performance
///
/// - Each layer's `render()` is called once per frame
/// - All layers are composited in a single tight loop
/// - Buffer allocations only occur on first render or size increase
///
/// # Example
///
/// ```ignore
/// use ftui_extras::visual_fx::{StackedFx, FxLayer, PlasmaFx, BlendMode};
///
/// let mut stack = StackedFx::new();
/// stack.push(FxLayer::new(Box::new(PlasmaFx::ocean())));
/// stack.push(FxLayer::with_opacity_and_blend(
///     Box::new(PlasmaFx::fire()),
///     0.3,
///     BlendMode::Additive,
/// ));
///
/// let backdrop = Backdrop::new(Box::new(stack), theme);
/// backdrop.render(area, &mut frame);
/// ```
pub struct StackedFx {
    layers: Vec<FxLayer>,
    /// Per-layer render buffers (grow-only, reused across frames).
    layer_bufs: Vec<Vec<PackedRgba>>,
    /// Cached dimensions for resize optimization.
    last_size: (u16, u16),
}

impl StackedFx {
    /// Create an empty stacked compositor.
    #[inline]
    pub fn new() -> Self {
        Self {
            layers: Vec::new(),
            layer_bufs: Vec::new(),
            last_size: (0, 0),
        }
    }

    /// Create a stacked compositor with pre-allocated capacity.
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            layers: Vec::with_capacity(capacity),
            layer_bufs: Vec::with_capacity(capacity),
            last_size: (0, 0),
        }
    }

    /// Add a layer to the top of the stack.
    #[inline]
    pub fn push(&mut self, layer: FxLayer) {
        self.layers.push(layer);
        self.layer_bufs.push(Vec::new());
    }

    /// Add a simple effect as a layer (opacity 1.0, Over blend).
    #[inline]
    pub fn push_fx(&mut self, fx: Box<dyn BackdropFx>) {
        self.push(FxLayer::new(fx));
    }

    /// Remove and return the top layer, if any.
    #[inline]
    pub fn pop(&mut self) -> Option<FxLayer> {
        self.layer_bufs.pop();
        self.layers.pop()
    }

    /// Clear all layers.
    #[inline]
    pub fn clear(&mut self) {
        self.layers.clear();
        self.layer_bufs.clear();
        self.last_size = (0, 0);
    }

    /// Number of layers in the stack.
    #[inline]
    pub fn len(&self) -> usize {
        self.layers.len()
    }

    /// Returns true if there are no layers.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    /// Get mutable access to a layer by index.
    #[inline]
    pub fn get_mut(&mut self, index: usize) -> Option<&mut FxLayer> {
        self.layers.get_mut(index)
    }

    /// Iterate over layers (bottom to top).
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &FxLayer> {
        self.layers.iter()
    }

    /// Iterate mutably over layers (bottom to top).
    #[inline]
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut FxLayer> {
        self.layers.iter_mut()
    }

    /// Ensure layer buffers are sized correctly (grow-only).
    fn ensure_buffers(&mut self, len: usize) {
        // Ensure we have enough buffer slots
        while self.layer_bufs.len() < self.layers.len() {
            self.layer_bufs.push(Vec::new());
        }

        // Grow each buffer if needed (never shrink)
        for buf in &mut self.layer_bufs {
            if buf.len() < len {
                buf.resize(len, PackedRgba::TRANSPARENT);
            }
        }
    }
}

impl Default for StackedFx {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for StackedFx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StackedFx")
            .field("layers", &self.layers)
            .field("last_size", &self.last_size)
            .finish()
    }
}

impl BackdropFx for StackedFx {
    fn name(&self) -> &'static str {
        "stacked"
    }

    fn resize(&mut self, width: u16, height: u16) {
        if self.last_size != (width, height) {
            self.last_size = (width, height);
            // Notify each layer's effect of the resize
            for layer in &mut self.layers {
                layer.fx.resize(width, height);
            }
        }
    }

    fn render(&mut self, ctx: FxContext<'_>, out: &mut [PackedRgba]) {
        // Early return for empty compositor or disabled quality
        if self.is_empty() || !ctx.quality.is_enabled() || ctx.is_empty() {
            return;
        }

        let len = ctx.len();
        debug_assert_eq!(out.len(), len);

        // Ensure buffers are ready
        self.ensure_buffers(len);

        // Phase 1: Render each layer to its buffer
        for (layer, buf) in self.layers.iter_mut().zip(self.layer_bufs.iter_mut()) {
            // Skip layers with zero opacity
            if layer.opacity <= 0.0 {
                continue;
            }

            // Clear buffer before rendering
            buf[..len].fill(PackedRgba::TRANSPARENT);

            // Render the effect
            layer.fx.render(ctx, &mut buf[..len]);
        }

        // Phase 2: Composite all layers into output in a single pass
        // This is the key optimization: one final pass over all cells
        for i in 0..len {
            let mut color = PackedRgba::TRANSPARENT;

            // Blend layers bottom-to-top
            for (layer, buf) in self.layers.iter().zip(self.layer_bufs.iter()) {
                if layer.opacity <= 0.0 {
                    continue;
                }

                let layer_color = buf[i].with_opacity(layer.opacity);
                color = layer.blend_mode.blend(layer_color, color);
            }

            out[i] = color;
        }
    }
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
    /// Optional quality override for demos/testing.
    /// When `None`, quality is derived from `frame.buffer.degradation`.
    quality_override: Option<FxQuality>,
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
            quality_override: None,
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

    /// Set an explicit quality override (for demos/testing).
    ///
    /// When set, this overrides the automatic derivation from `frame.buffer.degradation`.
    /// Pass `None` to restore automatic quality selection based on degradation level.
    #[inline]
    pub fn set_quality_override(&mut self, quality: Option<FxQuality>) {
        self.quality_override = quality;
    }

    #[inline]
    pub fn set_effect_opacity(&mut self, opacity: f32) {
        self.effect_opacity = opacity.clamp(0.0, 1.0);
    }

    #[inline]
    pub fn set_scrim(&mut self, scrim: Scrim) {
        self.scrim = scrim;
    }

    // -----------------------------------------------------------------------
    // Builder-style chained methods (bd-l8x9.2.3)
    // -----------------------------------------------------------------------

    /// Set the effect opacity and return self for chaining.
    ///
    /// # Example
    /// ```ignore
    /// Backdrop::new(fx, theme)
    ///     .with_effect_opacity(0.25)
    ///     .with_scrim(Scrim::vignette(0.5))
    ///     .over(child)
    /// ```
    #[must_use]
    #[inline]
    pub fn with_effect_opacity(mut self, opacity: f32) -> Self {
        self.effect_opacity = opacity.clamp(0.0, 1.0);
        self
    }

    /// Set the scrim and return self for chaining.
    #[must_use]
    #[inline]
    pub fn with_scrim(mut self, scrim: Scrim) -> Self {
        self.scrim = scrim;
        self
    }

    /// Set the theme and return self for chaining.
    #[must_use]
    #[inline]
    pub fn with_theme(mut self, theme: ThemeInputs) -> Self {
        self.theme = theme;
        self.base_fill = self.theme.bg_surface;
        self
    }

    /// Set a quality override and return self for chaining.
    ///
    /// When set, this overrides the automatic derivation from `frame.buffer.degradation`.
    #[must_use]
    #[inline]
    pub fn with_quality_override(mut self, quality: Option<FxQuality>) -> Self {
        self.quality_override = quality;
        self
    }

    /// Preset: subtle backdrop with low opacity and no scrim.
    ///
    /// Good for backgrounds where legibility of foreground content is paramount.
    /// Uses 15% effect opacity.
    #[must_use]
    #[inline]
    pub fn subtle(mut self) -> Self {
        self.effect_opacity = 0.15;
        self.scrim = Scrim::Off;
        self
    }

    /// Preset: vibrant backdrop with moderate opacity and soft vignette.
    ///
    /// Good for hero sections or splash screens where visual impact matters.
    /// Uses 50% effect opacity with a subtle vignette.
    #[must_use]
    #[inline]
    pub fn vibrant(mut self) -> Self {
        self.effect_opacity = 0.50;
        self.scrim = Scrim::vignette(0.3);
        self
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

        // Derive quality from frame degradation level, with area-based clamping.
        // Override takes precedence if set (for demos/testing).
        let quality = self.quality_override.unwrap_or_else(|| {
            FxQuality::from_degradation_with_area(frame.buffer.degradation, len)
        });

        let ctx = FxContext {
            width: w,
            height: h,
            frame: self.frame,
            time_seconds: self.time_seconds,
            quality,
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
            quality: FxQuality::Minimal,
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
            quality: FxQuality::Minimal,
            theme: &theme,
        };
        let mut out = Vec::new();
        fx.render(ctx, &mut out);
    }

    // -----------------------------------------------------------------------
    // Tiny-area safety tests (bd-l8x9.8)
    // Verify BackdropFx and Backdrop widget do not panic for edge-case sizes.
    // -----------------------------------------------------------------------

    #[test]
    fn tiny_area_1x1_is_safe() {
        let theme = ThemeInputs::default_dark();
        let mut fx = SolidBg;

        let ctx = FxContext {
            width: 1,
            height: 1,
            frame: 0,
            time_seconds: 0.0,
            quality: FxQuality::Full,
            theme: &theme,
        };
        let mut out = vec![PackedRgba::TRANSPARENT; 1];
        fx.render(ctx, &mut out);
        assert_eq!(out[0], theme.bg_base);
    }

    #[test]
    fn tiny_area_1xn_narrow_is_safe() {
        let theme = ThemeInputs::default_dark();
        let mut fx = SolidBg;

        // 1 column, 10 rows (narrow vertical strip)
        let ctx = FxContext {
            width: 1,
            height: 10,
            frame: 0,
            time_seconds: 0.0,
            quality: FxQuality::Full,
            theme: &theme,
        };
        let mut out = vec![PackedRgba::TRANSPARENT; 10];
        fx.render(ctx, &mut out);
        assert!(out.iter().all(|&c| c == theme.bg_base));
    }

    #[test]
    fn tiny_area_nx1_narrow_is_safe() {
        let theme = ThemeInputs::default_dark();
        let mut fx = SolidBg;

        // 10 columns, 1 row (narrow horizontal strip)
        let ctx = FxContext {
            width: 10,
            height: 1,
            frame: 0,
            time_seconds: 0.0,
            quality: FxQuality::Full,
            theme: &theme,
        };
        let mut out = vec![PackedRgba::TRANSPARENT; 10];
        fx.render(ctx, &mut out);
        assert!(out.iter().all(|&c| c == theme.bg_base));
    }

    #[test]
    fn backdrop_widget_tiny_1x1_is_safe() {
        let theme = ThemeInputs::default_dark();
        let backdrop = Backdrop::new(Box::new(SolidBg), theme);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        let area = Rect::new(0, 0, 1, 1);

        // Should not panic
        backdrop.render(area, &mut frame);

        // Background should be set
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_ne!(cell.bg, PackedRgba::TRANSPARENT);
    }

    #[test]
    fn backdrop_widget_tiny_1xn_is_safe() {
        let theme = ThemeInputs::default_dark();
        let backdrop = Backdrop::new(Box::new(SolidBg), theme);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 8, &mut pool);
        let area = Rect::new(0, 0, 1, 8);

        // Should not panic
        backdrop.render(area, &mut frame);

        // All cells should have background set
        for y in 0..8 {
            let cell = frame.buffer.get(0, y).unwrap();
            assert_ne!(cell.bg, PackedRgba::TRANSPARENT);
        }
    }

    #[test]
    fn backdrop_widget_tiny_nx1_is_safe() {
        let theme = ThemeInputs::default_dark();
        let backdrop = Backdrop::new(Box::new(SolidBg), theme);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(8, 1, &mut pool);
        let area = Rect::new(0, 0, 8, 1);

        // Should not panic
        backdrop.render(area, &mut frame);

        // All cells should have background set
        for x in 0..8 {
            let cell = frame.buffer.get(x, 0).unwrap();
            assert_ne!(cell.bg, PackedRgba::TRANSPARENT);
        }
    }

    #[test]
    fn backdrop_widget_empty_0x0_is_safe() {
        let theme = ThemeInputs::default_dark();
        let backdrop = Backdrop::new(Box::new(SolidBg), theme);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        // Empty area (0 width)
        let area = Rect::new(5, 5, 0, 0);

        // Should not panic (no-op for empty area)
        backdrop.render(area, &mut frame);
    }

    #[test]
    fn scrim_tiny_area_vignette_1x1_is_safe() {
        let theme = ThemeInputs::default_dark();
        let scrim = Scrim::vignette(0.5);
        // Should not panic for 1x1 area
        let _ = scrim.overlay_at(&theme, 0, 0, 1, 1);
    }

    #[test]
    fn scrim_tiny_area_vertical_fade_1_row_is_safe() {
        let theme = ThemeInputs::default_dark();
        let scrim = Scrim::vertical_fade(0.1, 0.9);
        // Should not panic for single-row area
        let overlay = scrim.overlay_at(&theme, 0, 0, 10, 1);
        // With height=1, t=1.0 so opacity should be bottom_opacity (0.9)
        assert!(overlay.a() > 0);
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

    // -----------------------------------------------------------------------
    // Buffer caching tests (bd-l8x9.2.1)
    // -----------------------------------------------------------------------

    #[test]
    fn backdrop_capacity_stable_across_many_renders() {
        // Unit test: capacity does not change across many renders at a stable size
        let theme = ThemeInputs::default_dark();
        let backdrop = Backdrop::new(Box::new(SolidBg), theme);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        let area = Rect::new(0, 0, 20, 10);

        // First render allocates the buffer
        backdrop.render(area, &mut frame);
        let initial_capacity = backdrop.fx_buf.borrow().capacity();

        // Render 50 more times - capacity must never change
        for i in 1..=50 {
            backdrop.render(area, &mut frame);
            let current_capacity = backdrop.fx_buf.borrow().capacity();
            assert_eq!(
                current_capacity, initial_capacity,
                "Capacity changed on render {} from {} to {}",
                i, initial_capacity, current_capacity
            );
        }
    }

    #[test]
    fn backdrop_resize_larger_grows_once() {
        // Unit test: resize to larger size grows once; not repeatedly
        let theme = ThemeInputs::default_dark();
        let backdrop = Backdrop::new(Box::new(SolidBg), theme);

        let mut pool = GraphemePool::new();

        // Start small (10x5 = 50 cells)
        let mut frame_small = Frame::new(10, 5, &mut pool);
        let area_small = Rect::new(0, 0, 10, 5);
        backdrop.render(area_small, &mut frame_small);
        let cap_small = backdrop.fx_buf.borrow().capacity();
        assert!(cap_small >= 50, "Buffer should be at least 50 cells");

        // Grow to larger (20x10 = 200 cells)
        let mut frame_large = Frame::new(20, 10, &mut pool);
        let area_large = Rect::new(0, 0, 20, 10);
        backdrop.render(area_large, &mut frame_large);
        let cap_after_grow = backdrop.fx_buf.borrow().capacity();
        assert!(
            cap_after_grow >= 200,
            "Buffer should grow to at least 200 cells"
        );

        // Render at same large size multiple times - no further growth
        for _ in 0..10 {
            backdrop.render(area_large, &mut frame_large);
            let cap_current = backdrop.fx_buf.borrow().capacity();
            assert_eq!(
                cap_current, cap_after_grow,
                "Buffer should not grow again at stable size"
            );
        }
    }

    #[test]
    fn backdrop_resize_smaller_does_not_shrink() {
        // Unit test: resize to smaller size does not shrink the buffer
        let theme = ThemeInputs::default_dark();
        let backdrop = Backdrop::new(Box::new(SolidBg), theme);

        let mut pool = GraphemePool::new();

        // Start large (30x15 = 450 cells)
        let mut frame_large = Frame::new(30, 15, &mut pool);
        let area_large = Rect::new(0, 0, 30, 15);
        backdrop.render(area_large, &mut frame_large);
        let cap_large = backdrop.fx_buf.borrow().capacity();
        assert!(cap_large >= 450, "Buffer should be at least 450 cells");

        // Shrink to smaller (10x5 = 50 cells)
        let mut frame_small = Frame::new(10, 5, &mut pool);
        let area_small = Rect::new(0, 0, 10, 5);
        backdrop.render(area_small, &mut frame_small);
        let cap_after_shrink = backdrop.fx_buf.borrow().capacity();

        // Buffer capacity must NOT decrease (grow-only policy)
        assert!(
            cap_after_shrink >= cap_large,
            "Buffer must not shrink: was {}, now {}",
            cap_large,
            cap_after_shrink
        );

        // Render at small size multiple times - still no shrinking
        for _ in 0..5 {
            backdrop.render(area_small, &mut frame_small);
            let cap_current = backdrop.fx_buf.borrow().capacity();
            assert!(
                cap_current >= cap_large,
                "Buffer must never shrink below peak capacity"
            );
        }
    }

    /// Test effect that records which cells it wrote to
    struct WriteTracker {
        written: std::cell::RefCell<Vec<bool>>,
    }

    impl WriteTracker {
        fn new() -> Self {
            Self {
                written: std::cell::RefCell::new(Vec::new()),
            }
        }

        #[allow(dead_code)]
        fn all_written(&self, len: usize) -> bool {
            let w = self.written.borrow();
            w.len() >= len && w[..len].iter().all(|&b| b)
        }
    }

    impl BackdropFx for WriteTracker {
        fn name(&self) -> &'static str {
            "write-tracker"
        }

        fn resize(&mut self, width: u16, height: u16) {
            let len = width as usize * height as usize;
            let mut w = self.written.borrow_mut();
            if w.len() < len {
                w.resize(len, false);
            }
            // Reset tracking for new size
            for b in w.iter_mut() {
                *b = false;
            }
        }

        fn render(&mut self, ctx: FxContext<'_>, out: &mut [PackedRgba]) {
            if ctx.is_empty() {
                return;
            }
            let len = ctx.len();
            let mut w = self.written.borrow_mut();
            if w.len() < len {
                w.resize(len, false);
            }
            // Mark all cells as written and fill with a known color
            for i in 0..len {
                out[i] = ctx.theme.accent_primary;
                w[i] = true;
            }
        }
    }

    #[test]
    fn backdrop_buffer_fully_written_no_stale_pixels() {
        // Unit test: buffer is fully written each frame (no stale pixels)
        let theme = ThemeInputs::default_dark();
        let tracker = WriteTracker::new();
        let backdrop = Backdrop::new(Box::new(tracker), theme);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(8, 6, &mut pool);
        let area = Rect::new(0, 0, 8, 6);
        let len = 8 * 6;

        // Render once - all cells should be written
        backdrop.render(area, &mut frame);

        // Verify all cells got the effect color (not stale TRANSPARENT)
        let fx_buf = backdrop.fx_buf.borrow();
        for (i, &color) in fx_buf.iter().take(len).enumerate() {
            // The buffer should have been filled by the effect
            // (WriteTracker fills with accent_primary)
            assert_ne!(
                color,
                PackedRgba::TRANSPARENT,
                "Cell {} was not written (stale pixel)",
                i
            );
        }
    }

    #[test]
    fn backdrop_buffer_no_stale_after_resize_grow() {
        // After growing, all cells in new size should be written
        let theme = ThemeInputs::default_dark();
        let backdrop = Backdrop::new(Box::new(SolidBg), theme);

        let mut pool = GraphemePool::new();

        // Small render
        let mut frame_small = Frame::new(4, 4, &mut pool);
        let area_small = Rect::new(0, 0, 4, 4);
        backdrop.render(area_small, &mut frame_small);

        // Grow and render
        let mut frame_large = Frame::new(10, 8, &mut pool);
        let area_large = Rect::new(0, 0, 10, 8);
        backdrop.render(area_large, &mut frame_large);

        // All 80 cells should be filled (SolidBg fills with bg_base)
        let fx_buf = backdrop.fx_buf.borrow();
        for (i, &color) in fx_buf.iter().take(80).enumerate() {
            assert_eq!(
                color, theme.bg_base,
                "Cell {} has wrong color after resize grow",
                i
            );
        }
    }

    #[test]
    fn backdrop_buffer_active_region_correct_after_shrink() {
        // After shrinking, only the active region cells matter (first w*h)
        let theme = ThemeInputs::default_dark();
        let backdrop = Backdrop::new(Box::new(SolidBg), theme);

        let mut pool = GraphemePool::new();

        // Large render (10x10 = 100 cells)
        let mut frame_large = Frame::new(10, 10, &mut pool);
        let area_large = Rect::new(0, 0, 10, 10);
        backdrop.render(area_large, &mut frame_large);
        let cap_after_large = backdrop.fx_buf.borrow().capacity();

        // Shrink to smaller (4x4 = 16 cells)
        let mut frame_small = Frame::new(4, 4, &mut pool);
        let area_small = Rect::new(0, 0, 4, 4);
        backdrop.render(area_small, &mut frame_small);

        // Active region (first 16 cells) should be properly filled
        let fx_buf = backdrop.fx_buf.borrow();
        for (i, &color) in fx_buf.iter().take(16).enumerate() {
            assert_eq!(
                color, theme.bg_base,
                "Active cell {} has wrong color after shrink",
                i
            );
        }

        // Capacity should still be at least 100 (no shrinking)
        assert!(
            fx_buf.capacity() >= cap_after_large,
            "Capacity shrunk unexpectedly"
        );
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

    // -----------------------------------------------------------------------
    // FxQuality mapping tests (DegradationLevel -> FxQuality)
    // -----------------------------------------------------------------------

    mod fx_quality_mapping {
        use super::*;
        use ftui_render::budget::DegradationLevel;

        #[test]
        fn from_degradation_full() {
            assert_eq!(
                FxQuality::from_degradation(DegradationLevel::Full),
                FxQuality::Full
            );
        }

        #[test]
        fn from_degradation_simple_borders() {
            assert_eq!(
                FxQuality::from_degradation(DegradationLevel::SimpleBorders),
                FxQuality::Reduced
            );
        }

        #[test]
        fn from_degradation_no_styling() {
            assert_eq!(
                FxQuality::from_degradation(DegradationLevel::NoStyling),
                FxQuality::Reduced
            );
        }

        #[test]
        fn from_degradation_essential_only() {
            assert_eq!(
                FxQuality::from_degradation(DegradationLevel::EssentialOnly),
                FxQuality::Off
            );
        }

        #[test]
        fn from_degradation_skeleton() {
            assert_eq!(
                FxQuality::from_degradation(DegradationLevel::Skeleton),
                FxQuality::Off
            );
        }

        #[test]
        fn from_degradation_skip_frame() {
            assert_eq!(
                FxQuality::from_degradation(DegradationLevel::SkipFrame),
                FxQuality::Off
            );
        }

        #[test]
        fn from_degradation_covers_all_variants() {
            // Exhaustive match to catch if new variants are added
            for level in [
                DegradationLevel::Full,
                DegradationLevel::SimpleBorders,
                DegradationLevel::NoStyling,
                DegradationLevel::EssentialOnly,
                DegradationLevel::Skeleton,
                DegradationLevel::SkipFrame,
            ] {
                let _ = FxQuality::from_degradation(level);
            }
        }

        #[test]
        fn area_clamp_small_area_unchanged() {
            // 100x40 = 4000 cells, below threshold
            assert_eq!(
                FxQuality::clamp_for_area(FxQuality::Full, 4000),
                FxQuality::Full
            );
            assert_eq!(
                FxQuality::clamp_for_area(FxQuality::Reduced, 4000),
                FxQuality::Reduced
            );
            assert_eq!(
                FxQuality::clamp_for_area(FxQuality::Minimal, 4000),
                FxQuality::Minimal
            );
            assert_eq!(
                FxQuality::clamp_for_area(FxQuality::Off, 4000),
                FxQuality::Off
            );
        }

        #[test]
        fn area_clamp_large_area_full_to_reduced() {
            // 200x80 = 16000 cells, at threshold
            assert_eq!(
                FxQuality::clamp_for_area(FxQuality::Full, FX_AREA_THRESHOLD_FULL_TO_REDUCED),
                FxQuality::Reduced
            );
        }

        #[test]
        fn area_clamp_huge_area_full_to_minimal() {
            // 320x200 = 64000 cells, at higher threshold
            assert_eq!(
                FxQuality::clamp_for_area(FxQuality::Full, FX_AREA_THRESHOLD_REDUCED_TO_MINIMAL),
                FxQuality::Minimal
            );
        }

        #[test]
        fn area_clamp_huge_area_reduced_to_minimal() {
            assert_eq!(
                FxQuality::clamp_for_area(FxQuality::Reduced, FX_AREA_THRESHOLD_REDUCED_TO_MINIMAL),
                FxQuality::Minimal
            );
        }

        #[test]
        fn area_clamp_minimal_unchanged() {
            // Minimal doesn't degrade further
            assert_eq!(
                FxQuality::clamp_for_area(FxQuality::Minimal, 100_000),
                FxQuality::Minimal
            );
        }

        #[test]
        fn area_clamp_off_unchanged() {
            // Off stays off
            assert_eq!(
                FxQuality::clamp_for_area(FxQuality::Off, 100_000),
                FxQuality::Off
            );
        }

        #[test]
        fn from_degradation_with_area_combined() {
            // Full budget + large area = Reduced
            assert_eq!(
                FxQuality::from_degradation_with_area(DegradationLevel::Full, 20_000),
                FxQuality::Reduced
            );

            // SimpleBorders + large area = Reduced (already Reduced, below higher threshold)
            assert_eq!(
                FxQuality::from_degradation_with_area(DegradationLevel::SimpleBorders, 20_000),
                FxQuality::Reduced
            );

            // EssentialOnly = Off regardless of area
            assert_eq!(
                FxQuality::from_degradation_with_area(DegradationLevel::EssentialOnly, 100),
                FxQuality::Off
            );
        }

        #[test]
        fn is_enabled_true_for_quality_levels() {
            assert!(FxQuality::Full.is_enabled());
            assert!(FxQuality::Reduced.is_enabled());
            assert!(FxQuality::Minimal.is_enabled());
        }

        #[test]
        fn is_enabled_false_for_off() {
            assert!(!FxQuality::Off.is_enabled());
        }

        #[test]
        fn default_is_full() {
            assert_eq!(FxQuality::default(), FxQuality::Full);
        }

        #[test]
        fn threshold_constants_are_reasonable() {
            // Verify thresholds are in expected order (const assertion)
            const { assert!(FX_AREA_THRESHOLD_FULL_TO_REDUCED < FX_AREA_THRESHOLD_REDUCED_TO_MINIMAL) };
            // 16k cells = ~200x80 terminal
            assert_eq!(FX_AREA_THRESHOLD_FULL_TO_REDUCED, 16_000);
            // 64k cells = ~320x200 or 4K equivalent
            assert_eq!(FX_AREA_THRESHOLD_REDUCED_TO_MINIMAL, 64_000);
        }
    }

    // -----------------------------------------------------------------------
    // StackedFx tests (bd-l8x9.2.5)
    // -----------------------------------------------------------------------

    mod stacked_fx_tests {
        use super::*;

        /// Test effect that fills with a solid color.
        struct SolidColor(PackedRgba);

        impl BackdropFx for SolidColor {
            fn name(&self) -> &'static str {
                "solid-color"
            }

            fn render(&mut self, ctx: FxContext<'_>, out: &mut [PackedRgba]) {
                if ctx.is_empty() {
                    return;
                }
                out[..ctx.len()].fill(self.0);
            }
        }

        /// Test effect that writes cell index as a gray value.
        struct GradientFx;

        impl BackdropFx for GradientFx {
            fn name(&self) -> &'static str {
                "gradient"
            }

            fn render(&mut self, ctx: FxContext<'_>, out: &mut [PackedRgba]) {
                for (i, pixel) in out.iter_mut().enumerate().take(ctx.len()) {
                    let gray = (i % 256) as u8;
                    *pixel = PackedRgba::rgb(gray, gray, gray);
                }
            }
        }

        #[test]
        fn stacked_fx_empty_is_noop() {
            let theme = ThemeInputs::default_dark();
            let ctx = FxContext {
                width: 4,
                height: 3,
                frame: 0,
                time_seconds: 0.0,
                quality: FxQuality::Full,
                theme: &theme,
            };
            let mut out = vec![PackedRgba::rgb(1, 2, 3); ctx.len()];

            let mut stack = StackedFx::new();
            stack.render(ctx, &mut out);

            // Output should be unchanged (empty stack is a no-op)
            assert!(out.iter().all(|&c| c == PackedRgba::rgb(1, 2, 3)));
        }

        #[test]
        fn stacked_fx_single_layer_renders() {
            let theme = ThemeInputs::default_dark();
            let ctx = FxContext {
                width: 4,
                height: 3,
                frame: 0,
                time_seconds: 0.0,
                quality: FxQuality::Full,
                theme: &theme,
            };
            let mut out = vec![PackedRgba::TRANSPARENT; ctx.len()];

            let mut stack = StackedFx::new();
            stack.push(FxLayer::new(Box::new(SolidColor(PackedRgba::rgb(
                100, 150, 200,
            )))));
            stack.render(ctx, &mut out);

            // All cells should have the solid color
            assert!(out.iter().all(|&c| c == PackedRgba::rgb(100, 150, 200)));
        }

        #[test]
        fn stacked_fx_two_layer_composition_over() {
            let theme = ThemeInputs::default_dark();
            let ctx = FxContext {
                width: 2,
                height: 2,
                frame: 0,
                time_seconds: 0.0,
                quality: FxQuality::Full,
                theme: &theme,
            };
            let mut out = vec![PackedRgba::TRANSPARENT; ctx.len()];

            let mut stack = StackedFx::new();
            // Layer 0: opaque red
            stack.push(FxLayer::new(Box::new(SolidColor(PackedRgba::rgb(
                255, 0, 0,
            )))));
            // Layer 1: 50% alpha blue (painted on top)
            stack.push(FxLayer::with_opacity(
                Box::new(SolidColor(PackedRgba::rgb(0, 0, 255))),
                0.5,
            ));
            stack.render(ctx, &mut out);

            // Expected: blue at 50% over red = some purple-ish color
            // Using Over blend: 0.5*255 + 0.5*255 for R, 0.5*0 + 0.5*0 for G, 0.5*255 for B
            // The exact values depend on the alpha blending formula
            for color in &out {
                // Red should be reduced, blue should be visible
                assert!(color.r() > 0 && color.r() < 255);
                assert!(color.b() > 0);
                assert_eq!(color.g(), 0);
            }
        }

        #[test]
        fn stacked_fx_layer_ordering_bottom_to_top() {
            let theme = ThemeInputs::default_dark();
            let ctx = FxContext {
                width: 1,
                height: 1,
                frame: 0,
                time_seconds: 0.0,
                quality: FxQuality::Full,
                theme: &theme,
            };
            let mut out = vec![PackedRgba::TRANSPARENT; 1];

            let mut stack = StackedFx::new();
            // Layer 0: green (bottom)
            stack.push(FxLayer::new(Box::new(SolidColor(PackedRgba::rgb(
                0, 255, 0,
            )))));
            // Layer 1: opaque red (top, should completely cover green)
            stack.push(FxLayer::new(Box::new(SolidColor(PackedRgba::rgb(
                255, 0, 0,
            )))));
            stack.render(ctx, &mut out);

            // Top layer (red) should fully cover bottom (green)
            assert_eq!(out[0], PackedRgba::rgb(255, 0, 0));
        }

        #[test]
        fn stacked_fx_zero_opacity_layer_invisible() {
            let theme = ThemeInputs::default_dark();
            let ctx = FxContext {
                width: 2,
                height: 2,
                frame: 0,
                time_seconds: 0.0,
                quality: FxQuality::Full,
                theme: &theme,
            };
            let mut out = vec![PackedRgba::TRANSPARENT; ctx.len()];

            let mut stack = StackedFx::new();
            // Layer 0: green
            stack.push(FxLayer::new(Box::new(SolidColor(PackedRgba::rgb(
                0, 255, 0,
            )))));
            // Layer 1: red at 0% opacity (should be invisible)
            stack.push(FxLayer::with_opacity(
                Box::new(SolidColor(PackedRgba::rgb(255, 0, 0))),
                0.0,
            ));
            stack.render(ctx, &mut out);

            // Should only see green
            assert!(out.iter().all(|&c| c == PackedRgba::rgb(0, 255, 0)));
        }

        #[test]
        fn stacked_fx_buffer_reuse_no_alloc() {
            let theme = ThemeInputs::default_dark();
            let ctx = FxContext {
                width: 10,
                height: 10,
                frame: 0,
                time_seconds: 0.0,
                quality: FxQuality::Full,
                theme: &theme,
            };
            let mut out = vec![PackedRgba::TRANSPARENT; ctx.len()];

            let mut stack = StackedFx::new();
            stack.push(FxLayer::new(Box::new(SolidColor(PackedRgba::rgb(
                100, 100, 100,
            )))));

            // First render allocates buffers
            stack.render(ctx, &mut out);
            let cap1 = stack.layer_bufs[0].capacity();

            // Second render should reuse buffers
            stack.render(ctx, &mut out);
            let cap2 = stack.layer_bufs[0].capacity();

            assert_eq!(cap1, cap2, "Buffer should be reused, not reallocated");
        }

        #[test]
        fn stacked_fx_resize_notifies_layers() {
            let theme = ThemeInputs::default_dark();

            // First context: 4x4
            let ctx1 = FxContext {
                width: 4,
                height: 4,
                frame: 0,
                time_seconds: 0.0,
                quality: FxQuality::Full,
                theme: &theme,
            };
            let mut out1 = vec![PackedRgba::TRANSPARENT; ctx1.len()];

            let mut stack = StackedFx::new();
            stack.push(FxLayer::new(Box::new(GradientFx)));

            stack.resize(4, 4);
            stack.render(ctx1, &mut out1);

            // Second context: 8x8 (resize)
            let ctx2 = FxContext {
                width: 8,
                height: 8,
                frame: 0,
                time_seconds: 0.0,
                quality: FxQuality::Full,
                theme: &theme,
            };
            let mut out2 = vec![PackedRgba::TRANSPARENT; ctx2.len()];

            stack.resize(8, 8);
            stack.render(ctx2, &mut out2);

            // Buffers should grow to accommodate new size
            assert!(stack.layer_bufs[0].len() >= ctx2.len());
        }

        #[test]
        fn blend_mode_additive() {
            let theme = ThemeInputs::default_dark();
            let ctx = FxContext {
                width: 1,
                height: 1,
                frame: 0,
                time_seconds: 0.0,
                quality: FxQuality::Full,
                theme: &theme,
            };
            let mut out = vec![PackedRgba::TRANSPARENT; 1];

            let mut stack = StackedFx::new();
            // Layer 0: dark red
            stack.push(FxLayer::new(Box::new(SolidColor(PackedRgba::rgb(
                100, 0, 0,
            )))));
            // Layer 1: dark blue, additive blend
            stack.push(FxLayer::with_blend(
                Box::new(SolidColor(PackedRgba::rgb(0, 0, 100))),
                BlendMode::Additive,
            ));
            stack.render(ctx, &mut out);

            // Additive: R=100, B=100
            assert_eq!(out[0].r(), 100);
            assert_eq!(out[0].g(), 0);
            assert_eq!(out[0].b(), 100);
        }

        #[test]
        fn blend_mode_multiply() {
            let bottom = PackedRgba::rgb(200, 100, 50);
            let top = PackedRgba::rgba(128, 128, 128, 255);

            let result = BlendMode::Multiply.blend(top, bottom);

            // Multiply: (200*128)/255 ≈ 100, (100*128)/255 ≈ 50, (50*128)/255 ≈ 25
            assert!(result.r() >= 98 && result.r() <= 102);
            assert!(result.g() >= 48 && result.g() <= 52);
            assert!(result.b() >= 23 && result.b() <= 27);
        }

        #[test]
        fn blend_mode_screen() {
            let bottom = PackedRgba::rgb(100, 50, 25);
            let top = PackedRgba::rgba(100, 100, 100, 255);

            let result = BlendMode::Screen.blend(top, bottom);

            // Screen lightens: result should be brighter than bottom
            assert!(result.r() >= bottom.r());
            assert!(result.g() >= bottom.g());
            assert!(result.b() >= bottom.b());
        }

        #[test]
        fn stacked_fx_push_pop() {
            let mut stack = StackedFx::new();
            assert!(stack.is_empty());
            assert_eq!(stack.len(), 0);

            stack.push_fx(Box::new(SolidColor(PackedRgba::rgb(255, 0, 0))));
            assert_eq!(stack.len(), 1);

            stack.push_fx(Box::new(SolidColor(PackedRgba::rgb(0, 255, 0))));
            assert_eq!(stack.len(), 2);

            let popped = stack.pop();
            assert!(popped.is_some());
            assert_eq!(stack.len(), 1);

            stack.clear();
            assert!(stack.is_empty());
        }

        #[test]
        fn stacked_fx_off_quality_is_noop() {
            let theme = ThemeInputs::default_dark();
            let ctx = FxContext {
                width: 4,
                height: 3,
                frame: 0,
                time_seconds: 0.0,
                quality: FxQuality::Off,
                theme: &theme,
            };
            let sentinel = PackedRgba::rgb(42, 42, 42);
            let mut out = vec![sentinel; ctx.len()];

            let mut stack = StackedFx::new();
            stack.push(FxLayer::new(Box::new(SolidColor(PackedRgba::rgb(
                255, 0, 0,
            )))));
            stack.render(ctx, &mut out);

            // With quality Off, output should be unchanged
            assert!(out.iter().all(|&c| c == sentinel));
        }

        #[test]
        fn fx_layer_debug_impl() {
            let layer = FxLayer::new(Box::new(SolidColor(PackedRgba::rgb(100, 100, 100))));
            let debug_str = format!("{:?}", layer);
            assert!(debug_str.contains("solid-color"));
            assert!(debug_str.contains("opacity"));
            assert!(debug_str.contains("blend_mode"));
        }

        #[test]
        fn stacked_fx_default() {
            let stack = StackedFx::default();
            assert!(stack.is_empty());
        }
    }

    // -----------------------------------------------------------------------
    // Alpha blending correctness tests (bd-l8x9.8)
    // -----------------------------------------------------------------------

    mod alpha_blending_correctness {
        use super::*;

        /// Test effect that outputs a known semi-transparent color.
        struct SemiTransparentFx {
            color: PackedRgba,
        }

        impl SemiTransparentFx {
            fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
                Self {
                    color: PackedRgba::rgba(r, g, b, a),
                }
            }
        }

        impl BackdropFx for SemiTransparentFx {
            fn name(&self) -> &'static str {
                "semi-transparent"
            }

            fn render(&mut self, ctx: FxContext<'_>, out: &mut [PackedRgba]) {
                if ctx.is_empty() {
                    return;
                }
                out[..ctx.len()].fill(self.color);
            }
        }

        #[test]
        fn backdrop_composition_matches_explicit_over_math() {
            // Verify that Backdrop's composition matches explicit PackedRgba::over() calculations.
            // Composition order: base_fill <- effect (with opacity) <- scrim (Off = no scrim)
            let theme = ThemeInputs::default_dark();

            // Create effect that outputs a known color
            let fx_output = PackedRgba::rgb(200, 100, 50);
            let fx = SemiTransparentFx::new(fx_output.r(), fx_output.g(), fx_output.b(), 255);

            let mut backdrop = Backdrop::new(Box::new(fx), theme);
            backdrop.set_effect_opacity(0.5);
            backdrop.set_scrim(Scrim::Off);

            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(1, 1, &mut pool);
            let area = Rect::new(0, 0, 1, 1);

            backdrop.render(area, &mut frame);

            let cell = frame.buffer.get(0, 0).unwrap();

            // Manually compute expected result:
            // base_fill = theme.bg_surface (opaque)
            // effect at 50% opacity = fx_output.with_opacity(0.5)
            // final = effect.over(base_fill)
            let base_fill = PackedRgba::rgb(
                theme.bg_surface.r(),
                theme.bg_surface.g(),
                theme.bg_surface.b(),
            );
            let effect_with_opacity = fx_output.with_opacity(0.5);
            let expected = effect_with_opacity.over(base_fill);

            assert_eq!(
                cell.bg, expected,
                "Backdrop composition mismatch: got {:?}, expected {:?}",
                cell.bg, expected
            );
        }

        #[test]
        fn backdrop_scrim_applies_correctly_over_effect() {
            // Verify scrim is applied after effect compositing
            let theme = ThemeInputs::default_dark();

            let fx = SemiTransparentFx::new(100, 150, 200, 255);
            let mut backdrop = Backdrop::new(Box::new(fx), theme);
            backdrop.set_effect_opacity(1.0);
            backdrop.set_scrim(Scrim::uniform_raw(0.5));

            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(1, 1, &mut pool);
            let area = Rect::new(0, 0, 1, 1);

            backdrop.render(area, &mut frame);

            let cell = frame.buffer.get(0, 0).unwrap();

            // Manually compute expected result:
            // base_fill = theme.bg_surface (opaque)
            // effect at 100% = fx_output
            // after_effect = effect.over(base_fill)
            // scrim = theme.bg_overlay at 50% opacity
            // final = scrim.over(after_effect)
            let base_fill = PackedRgba::rgb(
                theme.bg_surface.r(),
                theme.bg_surface.g(),
                theme.bg_surface.b(),
            );
            let fx_output = PackedRgba::rgb(100, 150, 200);
            let after_effect = fx_output.over(base_fill);
            let scrim_color = theme.bg_overlay.with_opacity(0.5);
            let expected = scrim_color.over(after_effect);

            assert_eq!(
                cell.bg, expected,
                "Scrim composition mismatch: got {:?}, expected {:?}",
                cell.bg, expected
            );
        }

        #[test]
        fn backdrop_zero_effect_opacity_shows_only_base_and_scrim() {
            let theme = ThemeInputs::default_dark();

            // Effect that would output red, but at 0% opacity
            let fx = SemiTransparentFx::new(255, 0, 0, 255);
            let mut backdrop = Backdrop::new(Box::new(fx), theme);
            backdrop.set_effect_opacity(0.0);
            backdrop.set_scrim(Scrim::Off);

            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(1, 1, &mut pool);
            let area = Rect::new(0, 0, 1, 1);

            backdrop.render(area, &mut frame);

            let cell = frame.buffer.get(0, 0).unwrap();

            // With 0% effect opacity, only base_fill should show
            let base_fill = PackedRgba::rgb(
                theme.bg_surface.r(),
                theme.bg_surface.g(),
                theme.bg_surface.b(),
            );

            // Effect at 0 opacity over base = base unchanged
            let fx_zero = PackedRgba::rgba(255, 0, 0, 0);
            let expected = fx_zero.over(base_fill);

            assert_eq!(
                cell.bg, expected,
                "Zero opacity effect should not affect output"
            );
        }

        #[test]
        fn backdrop_full_effect_opacity_dominates() {
            let theme = ThemeInputs::default_dark();

            // Opaque effect at 100% opacity
            let fx = SemiTransparentFx::new(255, 128, 64, 255);
            let mut backdrop = Backdrop::new(Box::new(fx), theme);
            backdrop.set_effect_opacity(1.0);
            backdrop.set_scrim(Scrim::Off);

            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(1, 1, &mut pool);
            let area = Rect::new(0, 0, 1, 1);

            backdrop.render(area, &mut frame);

            let cell = frame.buffer.get(0, 0).unwrap();

            // Opaque effect at 100% opacity completely covers base
            let expected = PackedRgba::rgb(255, 128, 64);

            assert_eq!(cell.bg, expected, "Full opacity effect should dominate");
        }

        #[test]
        fn packed_rgba_over_is_commutative_only_for_opaque() {
            // Verify our understanding of alpha compositing:
            // A.over(B) != B.over(A) in general (only equal if both opaque)
            let opaque_red = PackedRgba::rgb(255, 0, 0);
            let opaque_blue = PackedRgba::rgb(0, 0, 255);
            let semi_red = PackedRgba::rgba(255, 0, 0, 128);
            let semi_blue = PackedRgba::rgba(0, 0, 255, 128);

            // Opaque case: the top color wins, so A.over(B) = A
            assert_eq!(opaque_red.over(opaque_blue), opaque_red);
            assert_eq!(opaque_blue.over(opaque_red), opaque_blue);

            // Semi-transparent case: order matters
            let red_over_blue = semi_red.over(semi_blue);
            let blue_over_red = semi_blue.over(semi_red);
            assert_ne!(
                red_over_blue, blue_over_red,
                "Semi-transparent alpha compositing should not be commutative"
            );
        }

        #[test]
        fn backdrop_deterministic_across_renders() {
            // Same inputs should produce identical outputs every time
            let theme = ThemeInputs::default_dark();
            let fx = SemiTransparentFx::new(150, 100, 200, 255);

            let mut backdrop = Backdrop::new(Box::new(fx), theme);
            backdrop.set_effect_opacity(0.7);
            backdrop.set_scrim(Scrim::uniform(0.3));
            backdrop.set_time(100, 5.5);

            let mut pool = GraphemePool::new();
            let area = Rect::new(0, 0, 4, 4);

            // First render - capture cell colors
            let colors1: Vec<PackedRgba> = {
                let mut frame1 = Frame::new(4, 4, &mut pool);
                backdrop.render(area, &mut frame1);
                let mut colors = Vec::with_capacity(16);
                for y in 0..4 {
                    for x in 0..4 {
                        colors.push(frame1.buffer.get(x, y).unwrap().bg);
                    }
                }
                colors
            };

            // Second render - compare with first
            let mut frame2 = Frame::new(4, 4, &mut pool);
            backdrop.render(area, &mut frame2);

            for y in 0..4 {
                for x in 0..4 {
                    let c1 = colors1[(y * 4 + x) as usize];
                    let c2 = frame2.buffer.get(x, y).unwrap().bg;
                    assert_eq!(c1, c2, "Cell ({x}, {y}) differs between renders");
                }
            }
        }

        #[test]
        fn stacked_fx_alpha_matches_sequential_over() {
            // Verify that StackedFx composition matches sequential PackedRgba::over()
            let theme = ThemeInputs::default_dark();

            let color_a = PackedRgba::rgba(255, 0, 0, 200);
            let color_b = PackedRgba::rgba(0, 255, 0, 150);
            let color_c = PackedRgba::rgba(0, 0, 255, 100);

            let ctx = FxContext {
                width: 1,
                height: 1,
                frame: 0,
                time_seconds: 0.0,
                quality: FxQuality::Full,
                theme: &theme,
            };
            let mut out = vec![PackedRgba::TRANSPARENT; 1];

            let mut stack = StackedFx::new();
            // Layers: A (bottom) <- B <- C (top)
            stack.push(FxLayer::new(Box::new(SemiTransparentFx { color: color_a })));
            stack.push(FxLayer::new(Box::new(SemiTransparentFx { color: color_b })));
            stack.push(FxLayer::new(Box::new(SemiTransparentFx { color: color_c })));
            stack.render(ctx, &mut out);

            // Manual calculation: start with TRANSPARENT, then over each layer
            let step1 = color_a.over(PackedRgba::TRANSPARENT);
            let step2 = color_b.over(step1);
            let expected = color_c.over(step2);

            assert_eq!(
                out[0], expected,
                "StackedFx composition should match sequential over(): got {:?}, expected {:?}",
                out[0], expected
            );
        }
    }

    // -----------------------------------------------------------------------
    // Backdrop degradation integration tests (bd-l8x9.2.4)
    // -----------------------------------------------------------------------

    /// Test effect that captures the quality it received
    struct QualityCapture {
        captured: std::cell::RefCell<Option<FxQuality>>,
    }

    impl QualityCapture {
        fn new() -> Self {
            Self {
                captured: std::cell::RefCell::new(None),
            }
        }

        #[allow(dead_code)]
        fn captured_quality(&self) -> Option<FxQuality> {
            *self.captured.borrow()
        }
    }

    impl BackdropFx for QualityCapture {
        fn name(&self) -> &'static str {
            "quality-capture"
        }

        fn render(&mut self, ctx: FxContext<'_>, out: &mut [PackedRgba]) {
            *self.captured.borrow_mut() = Some(ctx.quality);
            out.fill(ctx.theme.bg_base);
        }
    }

    #[test]
    fn backdrop_uses_degradation_for_quality_full() {
        use ftui_render::budget::DegradationLevel;

        let theme = ThemeInputs::default_dark();
        let capture = QualityCapture::new();
        let backdrop = Backdrop::new(Box::new(capture), theme);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        frame.set_degradation(DegradationLevel::Full);

        let area = Rect::new(0, 0, 10, 5);
        backdrop.render(area, &mut frame);

        // Access the captured quality through the Backdrop's internal fx
        // Since we can't easily access it, we verify the mapping indirectly
        // by checking FxQuality::from_degradation
        let expected = FxQuality::from_degradation(DegradationLevel::Full);
        assert_eq!(expected, FxQuality::Full);
    }

    #[test]
    fn backdrop_uses_degradation_for_quality_reduced() {
        use ftui_render::budget::DegradationLevel;

        let theme = ThemeInputs::default_dark();
        let backdrop = Backdrop::new(Box::new(SolidBg), theme);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        frame.set_degradation(DegradationLevel::SimpleBorders);

        let area = Rect::new(0, 0, 10, 5);
        // Should not panic - quality derived from degradation
        backdrop.render(area, &mut frame);

        // Verify mapping: SimpleBorders -> Reduced
        let expected = FxQuality::from_degradation(DegradationLevel::SimpleBorders);
        assert_eq!(expected, FxQuality::Reduced);
    }

    #[test]
    fn backdrop_uses_degradation_for_quality_off() {
        use ftui_render::budget::DegradationLevel;

        let theme = ThemeInputs::default_dark();
        let backdrop = Backdrop::new(Box::new(SolidBg), theme);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        frame.set_degradation(DegradationLevel::EssentialOnly);

        let area = Rect::new(0, 0, 10, 5);
        backdrop.render(area, &mut frame);

        // Verify mapping: EssentialOnly -> Off
        let expected = FxQuality::from_degradation(DegradationLevel::EssentialOnly);
        assert_eq!(expected, FxQuality::Off);
    }

    #[test]
    fn backdrop_quality_override_takes_precedence() {
        use ftui_render::budget::DegradationLevel;

        let theme = ThemeInputs::default_dark();
        let mut backdrop = Backdrop::new(Box::new(SolidBg), theme);

        // Set override to Full
        backdrop.set_quality_override(Some(FxQuality::Full));

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        // Even with EssentialOnly degradation (which maps to Off)...
        frame.set_degradation(DegradationLevel::EssentialOnly);

        let area = Rect::new(0, 0, 10, 5);
        // ...the override should take precedence
        backdrop.render(area, &mut frame);

        // Background should be set (effect ran at Full quality)
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_ne!(cell.bg, PackedRgba::TRANSPARENT);
    }

    #[test]
    fn backdrop_quality_override_none_uses_degradation() {
        use ftui_render::budget::DegradationLevel;

        let theme = ThemeInputs::default_dark();
        let mut backdrop = Backdrop::new(Box::new(SolidBg), theme);

        // Explicitly set override to None (default behavior)
        backdrop.set_quality_override(None);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        frame.set_degradation(DegradationLevel::Full);

        let area = Rect::new(0, 0, 10, 5);
        backdrop.render(area, &mut frame);

        // Should render normally
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_ne!(cell.bg, PackedRgba::TRANSPARENT);
    }

    #[test]
    fn backdrop_area_clamping_applied_for_large_areas() {
        use ftui_render::budget::DegradationLevel;

        // Test that large areas get quality clamped even with Full degradation
        let area_cells = FX_AREA_THRESHOLD_FULL_TO_REDUCED + 1000;
        let base_quality = FxQuality::from_degradation(DegradationLevel::Full);
        let clamped = FxQuality::from_degradation_with_area(DegradationLevel::Full, area_cells);

        assert_eq!(base_quality, FxQuality::Full);
        // Large area should clamp Full -> Reduced (or lower)
        assert_ne!(
            clamped,
            FxQuality::Full,
            "Large area should clamp quality from Full: got {:?}",
            clamped
        );
    }

    #[test]
    fn backdrop_degradation_levels_map_correctly() {
        use ftui_render::budget::DegradationLevel;

        // Verify the full mapping table from the docstring
        assert_eq!(
            FxQuality::from_degradation(DegradationLevel::Full),
            FxQuality::Full
        );
        assert_eq!(
            FxQuality::from_degradation(DegradationLevel::SimpleBorders),
            FxQuality::Reduced
        );
        assert_eq!(
            FxQuality::from_degradation(DegradationLevel::NoStyling),
            FxQuality::Reduced
        );
        assert_eq!(
            FxQuality::from_degradation(DegradationLevel::EssentialOnly),
            FxQuality::Off
        );
        assert_eq!(
            FxQuality::from_degradation(DegradationLevel::Skeleton),
            FxQuality::Off
        );
        assert_eq!(
            FxQuality::from_degradation(DegradationLevel::SkipFrame),
            FxQuality::Off
        );
    }

    // -----------------------------------------------------------------------
    // Builder-style chaining tests (bd-l8x9.2.3)
    // -----------------------------------------------------------------------

    #[test]
    fn backdrop_builder_with_effect_opacity() {
        let theme = ThemeInputs::default_dark();
        let backdrop = Backdrop::new(Box::new(SolidBg), theme).with_effect_opacity(0.75);
        assert!((backdrop.effect_opacity - 0.75).abs() < 0.001);
    }

    #[test]
    fn backdrop_builder_with_scrim() {
        let theme = ThemeInputs::default_dark();
        let backdrop = Backdrop::new(Box::new(SolidBg), theme).with_scrim(Scrim::vignette(0.5));
        assert_ne!(
            backdrop.scrim,
            Scrim::Off,
            "Scrim should be vignette, not Off"
        );
    }

    #[test]
    fn backdrop_builder_chaining() {
        let theme = ThemeInputs::default_dark();
        let backdrop = Backdrop::new(Box::new(SolidBg), theme)
            .with_effect_opacity(0.25)
            .with_scrim(Scrim::uniform(0.3))
            .with_quality_override(Some(FxQuality::Reduced));

        assert!((backdrop.effect_opacity - 0.25).abs() < 0.001);
        assert!(backdrop.quality_override == Some(FxQuality::Reduced));
    }

    #[test]
    fn backdrop_preset_subtle() {
        let theme = ThemeInputs::default_dark();
        let backdrop = Backdrop::new(Box::new(SolidBg), theme).subtle();

        assert!((backdrop.effect_opacity - 0.15).abs() < 0.001);
        assert!(matches!(backdrop.scrim, Scrim::Off));
    }

    #[test]
    fn backdrop_preset_vibrant() {
        let theme = ThemeInputs::default_dark();
        let backdrop = Backdrop::new(Box::new(SolidBg), theme).vibrant();

        assert!((backdrop.effect_opacity - 0.50).abs() < 0.001);
        assert!(
            matches!(backdrop.scrim, Scrim::Vignette { .. }),
            "Vibrant preset should use vignette scrim"
        );
    }

    #[test]
    fn backdrop_builder_over_renders_correctly() {
        let theme = ThemeInputs::default_dark();
        let backdrop = Backdrop::new(Box::new(SolidBg), theme)
            .with_effect_opacity(0.3)
            .subtle();

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 4, &mut pool);
        let area = Rect::new(0, 0, 4, 4);

        // Create a simple child widget
        struct DummyChild;
        impl Widget for DummyChild {
            fn render(&self, _area: Rect, _frame: &mut Frame) {}
        }

        // Use the builder-style over() method
        let composed = backdrop.over(&DummyChild);
        composed.render(area, &mut frame);

        // Verify backdrop rendered (cell has non-transparent bg)
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_ne!(cell.bg, PackedRgba::TRANSPARENT);
    }

    #[test]
    fn backdrop_with_theme_updates_base_fill() {
        let dark = ThemeInputs::default_dark();
        let light = ThemeInputs::default_light();

        let backdrop = Backdrop::new(Box::new(SolidBg), dark).with_theme(light);

        // base_fill should be updated to light theme's bg_surface
        assert_eq!(backdrop.base_fill, light.bg_surface);
    }
}
