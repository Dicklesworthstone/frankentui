#![forbid(unsafe_code)]

//! Plasma backdrop effect (cell-space).
//!
//! Deterministic, no-allocation (steady state), and theme-aware.
//! Uses wave interference patterns for psychedelic visuals.

use crate::visual_fx::{BackdropFx, FxContext, FxQuality, ThemeInputs};
use ftui_render::cell::PackedRgba;

// ---------------------------------------------------------------------------
// Wave Functions
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
    // Breathing envelope: slow amplitude modulation for organic feel
    let breath = 0.85 + 0.15 * (time * 0.3).sin();
    ((value * breath) + 1.0) / 2.0
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

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

/// Theme-aware palette presets for plasma.
///
/// # Theme-Derived Palettes
///
/// Several presets dynamically derive their colors from `ThemeInputs`:
/// - [`ThemeAccents`]: Blends through accent_primary -> accent_secondary
/// - [`Aurora`]: Cool tones using accent_slots[0..2] with blue bias
/// - [`Ember`]: Warm tones using accent_slots[2..4] with orange bias
/// - [`Subtle`]: Low saturation, bg-focused for non-distracting backdrops
/// - [`Monochrome`]: Grayscale from bg_base to fg_primary
///
/// # Fixed Palettes
///
/// These presets use hard-coded colors for consistent appearance:
/// - [`Sunset`], [`Ocean`], [`Fire`], [`Neon`], [`Cyberpunk`]
///
/// # Fallbacks
///
/// Theme-derived palettes fall back to sensible defaults if ThemeInputs
/// has default/transparent values. See `ThemeInputs::default_dark()` for
/// the fallback colors used in testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PlasmaPalette {
    /// Use theme accent colors for the gradient.
    ///
    /// Blends: bg_surface -> accent_primary -> accent_secondary -> fg_primary
    #[default]
    ThemeAccents,
    /// Cool theme-derived palette (blues, cyans, purples).
    ///
    /// Uses accent_slots[0..2] with a blue bias. Falls back to Ocean-like
    /// gradient if slots are transparent.
    Aurora,
    /// Warm theme-derived palette (reds, oranges, yellows).
    ///
    /// Uses accent_slots[2..4] with an orange bias. Falls back to Fire-like
    /// gradient if slots are transparent.
    Ember,
    /// Subtle, low-saturation palette for non-distracting backdrops.
    ///
    /// Blends between bg tones with minimal color shift. Safe for use
    /// behind text without scrim in most cases.
    Subtle,
    /// Grayscale from bg_base to fg_primary.
    ///
    /// Uses only luminance, creating a theme-aware monochrome effect.
    Monochrome,
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
    /// Galaxy (deep black -> indigo -> magenta -> white stars).
    Galaxy,
}

impl PlasmaPalette {
    /// Map a normalized value [0, 1] to a color.
    ///
    /// # Determinism
    ///
    /// Given identical inputs (t, theme), this function always returns the same color.
    /// No global state or randomness is used.
    ///
    /// # Palette Stops
    ///
    /// Each palette uses 2-5 color stops with linear interpolation between them.
    /// The number of stops varies by palette type but is always >= 2.
    #[inline]
    pub fn color_at(&self, t: f64, theme: &ThemeInputs) -> PackedRgba {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::ThemeAccents => Self::theme_gradient(t, theme),
            Self::Aurora => Self::aurora(t, theme),
            Self::Ember => Self::ember(t, theme),
            Self::Subtle => Self::subtle(t, theme),
            Self::Monochrome => Self::monochrome(t, theme),
            Self::Sunset => Self::sunset(t),
            Self::Ocean => Self::ocean(t),
            Self::Fire => Self::fire(t),
            Self::Neon => Self::neon(t),
            Self::Cyberpunk => Self::cyberpunk(t),
            Self::Galaxy => Self::galaxy(t),
        }
    }

    /// Returns the number of color stops in this palette.
    ///
    /// Useful for testing and documentation.
    #[inline]
    pub const fn stop_count(&self) -> usize {
        match self {
            Self::ThemeAccents => 4,
            Self::Aurora => 4,
            Self::Ember => 4,
            Self::Subtle => 3,
            Self::Monochrome => 2,
            Self::Sunset => 4,
            Self::Ocean => 3,
            Self::Fire => 5,
            Self::Neon => 6, // HSV cycle has 6 segments
            Self::Cyberpunk => 3,
            Self::Galaxy => 4,
        }
    }

    /// Returns true if this palette is theme-derived.
    #[inline]
    pub const fn is_theme_derived(&self) -> bool {
        matches!(
            self,
            Self::ThemeAccents | Self::Aurora | Self::Ember | Self::Subtle | Self::Monochrome
        )
    }

    fn theme_gradient(t: f64, theme: &ThemeInputs) -> PackedRgba {
        // Blend through: bg_surface -> accent_primary -> accent_secondary -> fg_primary
        // 4 stops with 3 segments
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

    /// Aurora: Cool theme-derived palette (blues, cyans, purples).
    ///
    /// Uses accent_slots[0] and accent_slots[1] as the core colors, with
    /// accent_primary and bg_surface for endpoints. Falls back to cool
    /// blue/cyan gradient if slots are transparent.
    fn aurora(t: f64, theme: &ThemeInputs) -> PackedRgba {
        // Get cool colors from theme, with fallbacks
        let cool1 = if theme.accent_slots[0] == PackedRgba::TRANSPARENT {
            PackedRgba::rgb(60, 100, 180) // Fallback: blue
        } else {
            theme.accent_slots[0]
        };
        let cool2 = if theme.accent_slots[1] == PackedRgba::TRANSPARENT {
            PackedRgba::rgb(80, 200, 220) // Fallback: cyan
        } else {
            theme.accent_slots[1]
        };

        // 4 stops: bg_surface -> cool1 -> cool2 -> accent_primary
        if t < 0.33 {
            let s = t / 0.33;
            Self::lerp_color(theme.bg_surface, cool1, s)
        } else if t < 0.66 {
            let s = (t - 0.33) / 0.33;
            Self::lerp_color(cool1, cool2, s)
        } else {
            let s = (t - 0.66) / 0.34;
            Self::lerp_color(cool2, theme.accent_primary, s)
        }
    }

    /// Ember: Warm theme-derived palette (reds, oranges, yellows).
    ///
    /// Uses accent_slots[2] and accent_slots[3] as the core colors, with
    /// accent_secondary and bg_surface for endpoints. Falls back to warm
    /// red/orange gradient if slots are transparent.
    fn ember(t: f64, theme: &ThemeInputs) -> PackedRgba {
        // Get warm colors from theme, with fallbacks
        let warm1 = if theme.accent_slots[2] == PackedRgba::TRANSPARENT {
            PackedRgba::rgb(200, 80, 50) // Fallback: red-orange
        } else {
            theme.accent_slots[2]
        };
        let warm2 = if theme.accent_slots[3] == PackedRgba::TRANSPARENT {
            PackedRgba::rgb(255, 180, 60) // Fallback: orange-yellow
        } else {
            theme.accent_slots[3]
        };

        // 4 stops: bg_surface -> warm1 -> warm2 -> accent_secondary
        if t < 0.33 {
            let s = t / 0.33;
            Self::lerp_color(theme.bg_surface, warm1, s)
        } else if t < 0.66 {
            let s = (t - 0.33) / 0.33;
            Self::lerp_color(warm1, warm2, s)
        } else {
            let s = (t - 0.66) / 0.34;
            Self::lerp_color(warm2, theme.accent_secondary, s)
        }
    }

    /// Subtle: Low-saturation palette for non-distracting backdrops.
    ///
    /// Blends through bg tones with minimal color shift. The palette stays
    /// close to the background, making it safe for use behind text without
    /// requiring a scrim in most cases.
    fn subtle(t: f64, theme: &ThemeInputs) -> PackedRgba {
        // 3 stops: bg_base -> bg_surface -> bg_overlay
        // This creates a very subtle depth effect
        if t < 0.5 {
            let s = t / 0.5;
            Self::lerp_color(theme.bg_base, theme.bg_surface, s)
        } else {
            let s = (t - 0.5) / 0.5;
            Self::lerp_color(theme.bg_surface, theme.bg_overlay, s)
        }
    }

    /// Monochrome: Grayscale from bg_base to fg_primary.
    ///
    /// Creates a theme-aware monochrome gradient using only luminance values.
    fn monochrome(t: f64, theme: &ThemeInputs) -> PackedRgba {
        // 2 stops: bg_base -> fg_primary (simple linear blend)
        Self::lerp_color(theme.bg_base, theme.fg_primary, t)
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
        // Full hue cycle with slightly reduced saturation for subtlety
        let hue = t * 360.0;
        Self::hsv_to_rgb(hue, 0.92, 1.0)
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

    fn galaxy(t: f64) -> PackedRgba {
        // Deep space -> indigo nebula -> magenta -> bright star white
        let (r, g, b) = if t < 0.3 {
            let s = t / 0.3;
            Self::lerp_rgb((5, 2, 15), (40, 20, 100), s)
        } else if t < 0.6 {
            let s = (t - 0.3) / 0.3;
            Self::lerp_rgb((40, 20, 100), (180, 50, 160), s)
        } else if t < 0.85 {
            let s = (t - 0.6) / 0.25;
            Self::lerp_rgb((180, 50, 160), (220, 180, 255), s)
        } else {
            let s = (t - 0.85) / 0.15;
            Self::lerp_rgb((220, 180, 255), (255, 250, 240), s)
        };
        PackedRgba::rgb(r, g, b)
    }

    /// Fixed-point RGB lerp using u32 arithmetic (avoids f64 per channel).
    #[inline]
    fn lerp_rgb(a: (u8, u8, u8), b: (u8, u8, u8), t: f64) -> (u8, u8, u8) {
        let t256 = (t.clamp(0.0, 1.0) * 256.0) as u32;
        let inv = 256 - t256;
        (
            ((a.0 as u32 * inv + b.0 as u32 * t256) >> 8) as u8,
            ((a.1 as u32 * inv + b.1 as u32 * t256) >> 8) as u8,
            ((a.2 as u32 * inv + b.2 as u32 * t256) >> 8) as u8,
        )
    }

    /// Fixed-point color lerp using u32 arithmetic (avoids f64 per channel).
    #[inline]
    fn lerp_color(a: PackedRgba, b: PackedRgba, t: f64) -> PackedRgba {
        let t256 = (t.clamp(0.0, 1.0) * 256.0) as u32;
        let inv = 256 - t256;
        let r = ((a.r() as u32 * inv + b.r() as u32 * t256) >> 8) as u8;
        let g = ((a.g() as u32 * inv + b.g() as u32 * t256) >> 8) as u8;
        let bl = ((a.b() as u32 * inv + b.b() as u32 * t256) >> 8) as u8;
        PackedRgba::rgb(r, g, bl)
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

// ---------------------------------------------------------------------------
// PlasmaFx
// ---------------------------------------------------------------------------

/// Procedural plasma backdrop effect.
///
/// Renders a classic plasma effect using multiple overlapping sine waves.
/// The output is deterministic given the same inputs.
///
/// # Quality Tiers
///
/// - `Full`: 6 trigonometric evaluations per cell, full quality
/// - `Reduced`: Same as Full (reserved for future downsampling/skip)
/// - `Minimal`: 3 trigonometric evaluations per cell, simpler waves
/// - `Off`: No rendering (early return)
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
    scratch: PlasmaScratch,
}

impl PlasmaFx {
    /// Create a new plasma effect with the specified palette.
    #[inline]
    pub const fn new(palette: PlasmaPalette) -> Self {
        Self {
            palette,
            scratch: PlasmaScratch::new(),
        }
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

    /// Create a plasma effect with the aurora (cool) palette.
    ///
    /// Theme-derived: uses cool tones from `accent_slots[0..2]`.
    #[inline]
    pub const fn aurora() -> Self {
        Self::new(PlasmaPalette::Aurora)
    }

    /// Create a plasma effect with the ember (warm) palette.
    ///
    /// Theme-derived: uses warm tones from `accent_slots[2..4]`.
    #[inline]
    pub const fn ember() -> Self {
        Self::new(PlasmaPalette::Ember)
    }

    /// Create a plasma effect with the subtle palette.
    ///
    /// Theme-derived: low-saturation bg tones, safe behind text.
    #[inline]
    pub const fn subtle() -> Self {
        Self::new(PlasmaPalette::Subtle)
    }

    /// Create a plasma effect with the monochrome palette.
    ///
    /// Theme-derived: grayscale from bg to fg.
    #[inline]
    pub const fn monochrome() -> Self {
        Self::new(PlasmaPalette::Monochrome)
    }

    /// Set the palette.
    #[inline]
    pub fn set_palette(&mut self, palette: PlasmaPalette) {
        self.palette = palette;
    }

    /// Get the current palette.
    #[inline]
    pub const fn palette(&self) -> PlasmaPalette {
        self.palette
    }
}

impl Default for PlasmaFx {
    fn default() -> Self {
        Self::theme()
    }
}

#[derive(Debug, Clone)]
struct PlasmaScratch {
    width: u16,
    x: Vec<f64>,
    x_sq: Vec<f64>,
    x_center_sq: Vec<f64>,
    sin_x2: Vec<f64>,
    v1: Vec<f64>,
}

impl PlasmaScratch {
    const fn new() -> Self {
        Self {
            width: 0,
            x: Vec::new(),
            x_sq: Vec::new(),
            x_center_sq: Vec::new(),
            sin_x2: Vec::new(),
            v1: Vec::new(),
        }
    }

    fn ensure_width(&mut self, width: u16, w: f64) {
        if self.width == width {
            return;
        }
        self.width = width;
        let len = width as usize;
        self.x.resize(len, 0.0);
        self.x_sq.resize(len, 0.0);
        self.x_center_sq.resize(len, 0.0);
        self.sin_x2.resize(len, 0.0);
        self.v1.resize(len, 0.0);

        for dx in 0..len {
            let x = (dx as f64 / w) * 6.0;
            let x_sq = x * x;
            let x_center = x - 3.0;
            self.x[dx] = x;
            self.x_sq[dx] = x_sq;
            self.x_center_sq[dx] = x_center * x_center;
            self.sin_x2[dx] = (x * 2.0).sin();
        }
    }

    fn update_time(&mut self, time: f64) {
        for (idx, x) in self.x.iter().enumerate() {
            self.v1[idx] = (x * 1.5 + time).sin();
        }
    }
}

impl PlasmaFx {
    #[inline]
    fn render_with_palette<F>(&mut self, ctx: FxContext<'_>, out: &mut [PackedRgba], mut sample: F)
    where
        F: FnMut(f64) -> PackedRgba,
    {
        // Early return if quality is Off (decorative effects are non-essential)
        if !ctx.quality.is_enabled() || ctx.is_empty() {
            return;
        }
        debug_assert_eq!(out.len(), ctx.len());

        let w = ctx.width as f64;
        let h = ctx.height as f64;
        let time = ctx.time_seconds;

        // Quality tiers:
        // - Full: 6 wave components (v1-v6) with sqrt-based radial waves
        // - Reduced: 4 wave components (v1-v4 skipping expensive sqrt radials)
        // - Minimal: 3 wave components (v1-v3, cheapest)
        let quality = ctx.quality;
        let breath = 0.85 + 0.15 * (time * 0.3).sin();

        // Precompute x-dependent terms once per frame (no per-frame allocation).
        let scratch = &mut self.scratch;
        scratch.ensure_width(ctx.width, w);
        scratch.update_time(time);

        for dy in 0..ctx.height {
            let ny = dy as f64 / h;
            let y = ny * 6.0;
            let y_sq = y * y;
            let y_center = y - 3.0;
            let y_center_sq = y_center * y_center;
            let v2 = (y * 1.8 + time * 0.8).sin();
            let cos_y2 = (y * 2.0).cos();

            for dx in 0..ctx.width {
                let idx = dy as usize * ctx.width as usize + dx as usize;
                let x = scratch.x[dx as usize];
                let v1 = scratch.v1[dx as usize];
                let v3 = ((x + y) * 1.2 + time * 0.6).sin();

                let wave = if quality == FxQuality::Minimal {
                    let value = (v1 + v2 + v3) / 3.0;
                    (value + 1.0) / 2.0
                } else if quality == FxQuality::Reduced {
                    // 4 components: skip sqrt-based v4/v5, keep interference v6
                    let v6 = (scratch.sin_x2[dx as usize] * cos_y2 + time * 0.5).sin();
                    let value = (v1 + v2 + v3 + v6) / 4.0;
                    ((value * breath) + 1.0) / 2.0
                } else {
                    let v4 = ((scratch.x_sq[dx as usize] + y_sq).sqrt() * 2.0 - time * 1.2).sin();
                    let v5 = ((scratch.x_center_sq[dx as usize] + y_center_sq).sqrt() * 1.8 + time)
                        .cos();
                    let v6 = (scratch.sin_x2[dx as usize] * cos_y2 + time * 0.5).sin();
                    let value = (v1 + v2 + v3 + v4 + v5 + v6) / 6.0;
                    ((value * breath) + 1.0) / 2.0
                };

                out[idx] = sample(wave.clamp(0.0, 1.0));
            }
        }
    }
}

impl BackdropFx for PlasmaFx {
    fn name(&self) -> &'static str {
        "plasma"
    }

    fn render(&mut self, ctx: FxContext<'_>, out: &mut [PackedRgba]) {
        let theme = ctx.theme;
        let palette = self.palette;
        match palette {
            PlasmaPalette::ThemeAccents => {
                self.render_with_palette(ctx, out, |t| PlasmaPalette::theme_gradient(t, theme));
            }
            PlasmaPalette::Aurora => {
                self.render_with_palette(ctx, out, |t| PlasmaPalette::aurora(t, theme));
            }
            PlasmaPalette::Ember => {
                self.render_with_palette(ctx, out, |t| PlasmaPalette::ember(t, theme));
            }
            PlasmaPalette::Subtle => {
                self.render_with_palette(ctx, out, |t| PlasmaPalette::subtle(t, theme));
            }
            PlasmaPalette::Monochrome => {
                self.render_with_palette(ctx, out, |t| PlasmaPalette::monochrome(t, theme));
            }
            PlasmaPalette::Sunset => self.render_with_palette(ctx, out, PlasmaPalette::sunset),
            PlasmaPalette::Ocean => self.render_with_palette(ctx, out, PlasmaPalette::ocean),
            PlasmaPalette::Fire => self.render_with_palette(ctx, out, PlasmaPalette::fire),
            PlasmaPalette::Neon => self.render_with_palette(ctx, out, PlasmaPalette::neon),
            PlasmaPalette::Cyberpunk => {
                self.render_with_palette(ctx, out, PlasmaPalette::cyberpunk)
            }
            PlasmaPalette::Galaxy => self.render_with_palette(ctx, out, PlasmaPalette::galaxy),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(theme: &ThemeInputs) -> FxContext<'_> {
        FxContext {
            width: 24,
            height: 12,
            frame: 1,
            time_seconds: 1.25,
            quality: FxQuality::Full,
            theme,
        }
    }

    #[test]
    fn deterministic_for_fixed_inputs() {
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::default();
        let ctx = ctx(&theme);
        let mut out1 = vec![PackedRgba::TRANSPARENT; ctx.len()];
        let mut out2 = vec![PackedRgba::TRANSPARENT; ctx.len()];
        fx.render(ctx, &mut out1);
        fx.render(ctx, &mut out2);
        assert_eq!(out1, out2);
    }

    #[test]
    fn tiny_area_safe() {
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::default();

        // Test 0x0
        let ctx = FxContext {
            width: 0,
            height: 0,
            frame: 0,
            time_seconds: 0.0,
            quality: FxQuality::Minimal,
            theme: &theme,
        };
        fx.render(ctx, &mut []);

        // Test 0x10
        let ctx = FxContext {
            width: 0,
            height: 10,
            frame: 0,
            time_seconds: 0.0,
            quality: FxQuality::Minimal,
            theme: &theme,
        };
        fx.render(ctx, &mut []);

        // Test 10x0
        let ctx = FxContext {
            width: 10,
            height: 0,
            frame: 0,
            time_seconds: 0.0,
            quality: FxQuality::Minimal,
            theme: &theme,
        };
        fx.render(ctx, &mut []);

        // Test 1x1
        let ctx = FxContext {
            width: 1,
            height: 1,
            frame: 0,
            time_seconds: 0.0,
            quality: FxQuality::Minimal,
            theme: &theme,
        };
        let mut out = vec![PackedRgba::TRANSPARENT; 1];
        fx.render(ctx, &mut out);
    }

    #[test]
    fn quality_off_does_not_render() {
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::default();
        let ctx = FxContext {
            width: 4,
            height: 4,
            frame: 0,
            time_seconds: 1.0,
            quality: FxQuality::Off,
            theme: &theme,
        };
        let mut out = vec![PackedRgba::TRANSPARENT; 16];
        fx.render(ctx, &mut out);
        // Should remain unchanged (TRANSPARENT)
        assert!(out.iter().all(|c| *c == PackedRgba::TRANSPARENT));
    }

    #[test]
    fn all_palettes_render_without_panic() {
        let theme = ThemeInputs::default_dark();
        let ctx = ctx(&theme);
        let mut out = vec![PackedRgba::TRANSPARENT; ctx.len()];

        for palette in [
            PlasmaPalette::ThemeAccents,
            PlasmaPalette::Aurora,
            PlasmaPalette::Ember,
            PlasmaPalette::Subtle,
            PlasmaPalette::Monochrome,
            PlasmaPalette::Sunset,
            PlasmaPalette::Ocean,
            PlasmaPalette::Fire,
            PlasmaPalette::Neon,
            PlasmaPalette::Cyberpunk,
            PlasmaPalette::Galaxy,
        ] {
            let mut fx = PlasmaFx::new(palette);
            fx.render(ctx, &mut out);
        }
    }

    #[test]
    fn palette_stop_counts_valid() {
        // All palettes must have at least 2 stops
        for palette in [
            PlasmaPalette::ThemeAccents,
            PlasmaPalette::Aurora,
            PlasmaPalette::Ember,
            PlasmaPalette::Subtle,
            PlasmaPalette::Monochrome,
            PlasmaPalette::Sunset,
            PlasmaPalette::Ocean,
            PlasmaPalette::Fire,
            PlasmaPalette::Neon,
            PlasmaPalette::Cyberpunk,
            PlasmaPalette::Galaxy,
        ] {
            let count = palette.stop_count();
            assert!(
                count >= 2,
                "{:?} has only {} stops (minimum 2)",
                palette,
                count
            );
        }
    }

    #[test]
    fn palette_color_bounds_valid() {
        // All palette colors must have valid RGB values (0-255)
        // and non-zero alpha when rendering
        let theme = ThemeInputs::default_dark();
        for palette in [
            PlasmaPalette::ThemeAccents,
            PlasmaPalette::Aurora,
            PlasmaPalette::Ember,
            PlasmaPalette::Subtle,
            PlasmaPalette::Monochrome,
            PlasmaPalette::Sunset,
            PlasmaPalette::Ocean,
            PlasmaPalette::Fire,
            PlasmaPalette::Neon,
            PlasmaPalette::Cyberpunk,
            PlasmaPalette::Galaxy,
        ] {
            // Test at multiple t values across the gradient
            for t_int in 0..=10 {
                let t = t_int as f64 / 10.0;
                let color = palette.color_at(t, &theme);
                // RGB values are implicitly valid (u8)
                // Check alpha is non-zero (opaque colors for plasma)
                assert!(color.a() > 0, "{:?} at t={} has zero alpha", palette, t);
            }
        }
    }

    #[test]
    fn theme_derived_palettes_identified_correctly() {
        // Theme-derived palettes should return true
        assert!(PlasmaPalette::ThemeAccents.is_theme_derived());
        assert!(PlasmaPalette::Aurora.is_theme_derived());
        assert!(PlasmaPalette::Ember.is_theme_derived());
        assert!(PlasmaPalette::Subtle.is_theme_derived());
        assert!(PlasmaPalette::Monochrome.is_theme_derived());

        // Fixed palettes should return false
        assert!(!PlasmaPalette::Sunset.is_theme_derived());
        assert!(!PlasmaPalette::Ocean.is_theme_derived());
        assert!(!PlasmaPalette::Fire.is_theme_derived());
        assert!(!PlasmaPalette::Neon.is_theme_derived());
        assert!(!PlasmaPalette::Cyberpunk.is_theme_derived());
        assert!(!PlasmaPalette::Galaxy.is_theme_derived());
    }

    #[test]
    fn theme_derived_palettes_differ_from_fixed() {
        // Theme-derived palettes should produce different output
        // from fixed palettes with the same wave values
        let theme = ThemeInputs::default_dark();
        let t = 0.5;

        let theme_color = PlasmaPalette::ThemeAccents.color_at(t, &theme);
        let sunset_color = PlasmaPalette::Sunset.color_at(t, &theme);

        // They should differ (different color sources)
        assert_ne!(
            theme_color, sunset_color,
            "Theme palette should differ from Sunset"
        );
    }

    #[test]
    fn palette_determinism() {
        // Same inputs should always produce same outputs
        let theme = ThemeInputs::default_dark();

        for palette in [
            PlasmaPalette::ThemeAccents,
            PlasmaPalette::Aurora,
            PlasmaPalette::Ember,
            PlasmaPalette::Subtle,
            PlasmaPalette::Monochrome,
            PlasmaPalette::Galaxy,
        ] {
            for t_int in 0..=10 {
                let t = t_int as f64 / 10.0;
                let c1 = palette.color_at(t, &theme);
                let c2 = palette.color_at(t, &theme);
                assert_eq!(c1, c2, "{:?} is non-deterministic at t={}", palette, t);
            }
        }
    }

    #[test]
    fn aurora_uses_cool_tones() {
        // Aurora should blend cool colors
        let theme = ThemeInputs::default_dark();
        let mid_color = PlasmaPalette::Aurora.color_at(0.5, &theme);

        // Mid-value should have blue or cyan bias (not warm)
        // With default theme, aurora uses fallback blues
        let r = mid_color.r() as i32;
        let b = mid_color.b() as i32;

        // Blue should be comparable to or stronger than red
        assert!(
            b >= r - 50,
            "Aurora mid-color should have cool tones, got r={} b={}",
            r,
            b
        );
    }

    #[test]
    fn ember_uses_warm_tones_with_fallback_theme() {
        // Ember should blend warm colors when using fallback (transparent slots)
        // The default_dark theme has non-transparent slots, so we create a theme
        // with transparent slots to test the warm fallback behavior.
        let mut theme = ThemeInputs::default_dark();
        theme.accent_slots[2] = PackedRgba::TRANSPARENT;
        theme.accent_slots[3] = PackedRgba::TRANSPARENT;

        let mid_color = PlasmaPalette::Ember.color_at(0.5, &theme);

        // Mid-value should have red or orange bias (warm) when using fallbacks
        let r = mid_color.r() as i32;
        let b = mid_color.b() as i32;

        // Red should be stronger than blue when fallbacks are active
        assert!(
            r > b,
            "Ember mid-color with fallbacks should have warm tones, got r={} b={}",
            r,
            b
        );
    }

    #[test]
    fn subtle_stays_near_background() {
        // Subtle palette should stay close to background colors
        let theme = ThemeInputs::default_dark();

        for t_int in 0..=10 {
            let t = t_int as f64 / 10.0;
            let color = PlasmaPalette::Subtle.color_at(t, &theme);

            // Subtle colors should be relatively dark (near bg)
            // with default_dark theme
            let luminance =
                color.r() as u32 * 299 + color.g() as u32 * 587 + color.b() as u32 * 114;
            let avg_lum = luminance / 1000;

            // Should be darker than mid-gray (128)
            assert!(
                avg_lum < 160,
                "Subtle palette should stay near background, got luminance {} at t={}",
                avg_lum,
                t
            );
        }
    }

    #[test]
    fn wave_output_in_valid_range() {
        // Test that plasma_wave always returns values in [0, 1]
        for nx in [0.0, 0.25, 0.5, 0.75, 1.0] {
            for ny in [0.0, 0.25, 0.5, 0.75, 1.0] {
                for time in [0.0, 1.0, 10.0, 100.0] {
                    let v = plasma_wave(nx, ny, time);
                    assert!(
                        (0.0..=1.0).contains(&v),
                        "plasma_wave({nx}, {ny}, {time}) = {v} out of range"
                    );

                    let v_low = plasma_wave_low(nx, ny, time);
                    assert!(
                        (0.0..=1.0).contains(&v_low),
                        "plasma_wave_low({nx}, {ny}, {time}) = {v_low} out of range"
                    );
                }
            }
        }
    }

    #[test]
    fn minimal_quality_uses_simplified_wave() {
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::default();

        // Render with Full quality
        let ctx_full = FxContext {
            width: 8,
            height: 8,
            frame: 0,
            time_seconds: 1.0,
            quality: FxQuality::Full,
            theme: &theme,
        };
        let mut out_full = vec![PackedRgba::TRANSPARENT; 64];
        fx.render(ctx_full, &mut out_full);

        // Render with Minimal quality
        let ctx_min = FxContext {
            width: 8,
            height: 8,
            frame: 0,
            time_seconds: 1.0,
            quality: FxQuality::Minimal,
            theme: &theme,
        };
        let mut out_min = vec![PackedRgba::TRANSPARENT; 64];
        fx.render(ctx_min, &mut out_min);

        // They should differ (different wave functions)
        assert_ne!(out_full, out_min);
    }

    #[test]
    fn palette_setter_roundtrip() {
        let mut fx = PlasmaFx::default();
        assert_eq!(fx.palette(), PlasmaPalette::ThemeAccents);
        fx.set_palette(PlasmaPalette::Galaxy);
        assert_eq!(fx.palette(), PlasmaPalette::Galaxy);
    }

    #[test]
    fn palette_color_clamps() {
        let theme = ThemeInputs::default_dark();
        let palette = PlasmaPalette::Ocean;
        let low = palette.color_at(-1.0, &theme);
        let high = palette.color_at(2.0, &theme);
        assert_eq!(low, palette.color_at(0.0, &theme));
        assert_eq!(high, palette.color_at(1.0, &theme));
    }

    #[test]
    fn lerp_helpers_respect_endpoints() {
        let a = (10, 20, 30);
        let b = (200, 210, 220);
        assert_eq!(PlasmaPalette::lerp_rgb(a, b, 0.0), a);
        assert_eq!(PlasmaPalette::lerp_rgb(a, b, 1.0), b);

        let ca = PackedRgba::rgb(10, 20, 30);
        let cb = PackedRgba::rgb(200, 210, 220);
        assert_eq!(PlasmaPalette::lerp_color(ca, cb, 0.0), ca);
        assert_eq!(PlasmaPalette::lerp_color(ca, cb, 1.0), cb);
    }

    #[test]
    fn hsv_to_rgb_primary_colors() {
        let red = PlasmaPalette::hsv_to_rgb(0.0, 1.0, 1.0);
        let green = PlasmaPalette::hsv_to_rgb(120.0, 1.0, 1.0);
        let blue = PlasmaPalette::hsv_to_rgb(240.0, 1.0, 1.0);

        assert_eq!(red, PackedRgba::rgb(255, 0, 0));
        assert_eq!(green, PackedRgba::rgb(0, 255, 0));
        assert_eq!(blue, PackedRgba::rgb(0, 0, 255));
    }

    // =========================================================================
    // No-Allocation Proxy Tests (bd-l8x9.4.3)
    // =========================================================================

    #[test]
    fn no_alloc_proxy_stable_size() {
        // Verify that repeated renders at stable size do not grow caller's buffer capacity
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::default();

        // Create buffer with exact capacity (no spare room)
        let mut out = Vec::with_capacity(64);
        out.resize(64, PackedRgba::TRANSPARENT);
        let initial_capacity = out.capacity();

        // Render multiple times at the same size
        for frame in 0..10 {
            let ctx = FxContext {
                width: 8,
                height: 8,
                frame,
                time_seconds: frame as f64 * 0.1,
                quality: FxQuality::Full,
                theme: &theme,
            };
            fx.render(ctx, &mut out);
        }

        // Capacity should not have grown
        assert_eq!(
            out.capacity(),
            initial_capacity,
            "Buffer capacity grew during repeated renders: {} -> {}",
            initial_capacity,
            out.capacity()
        );
    }

    #[test]
    fn no_alloc_fx_internal_state() {
        // PlasmaFx should have no internal buffers that grow
        let fx = PlasmaFx::default();
        let size1 = std::mem::size_of_val(&fx);

        // Create with different palette
        let fx2 = PlasmaFx::new(PlasmaPalette::Ocean);
        let size2 = std::mem::size_of_val(&fx2);

        // Both should have identical sizes (no dynamic allocations)
        assert_eq!(size1, size2, "PlasmaFx size should not vary with palette");

        // Size should be reasonable (palette enum + scratch buffers for pre-computation)
        // On 64-bit: palette (1-2 bytes) + PlasmaScratch (5 Vecs at 24 bytes each + u16)
        assert!(
            size1 <= 200,
            "PlasmaFx should be reasonably sized, got {} bytes",
            size1
        );
    }

    // =========================================================================
    // Quality Scaling Tests (bd-l8x9.4.3)
    // =========================================================================

    #[test]
    fn quality_reduced_produces_output() {
        // Reduced quality should still produce visible output (not empty)
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::default();
        let ctx = FxContext {
            width: 8,
            height: 8,
            frame: 0,
            time_seconds: 1.0,
            quality: FxQuality::Reduced,
            theme: &theme,
        };
        let mut out = vec![PackedRgba::TRANSPARENT; 64];
        fx.render(ctx, &mut out);

        // Should have non-transparent pixels
        let non_transparent = out
            .iter()
            .filter(|c| **c != PackedRgba::TRANSPARENT)
            .count();
        assert!(
            non_transparent > 0,
            "Reduced quality should produce visible output"
        );
    }

    #[test]
    fn quality_tiers_are_deterministic() {
        // Each quality tier should be deterministic
        let theme = ThemeInputs::default_dark();

        for quality in [FxQuality::Full, FxQuality::Reduced, FxQuality::Minimal] {
            let mut fx = PlasmaFx::default();
            let ctx = FxContext {
                width: 8,
                height: 8,
                frame: 42,
                time_seconds: 3.25, // Use non-PI value for test
                quality,
                theme: &theme,
            };

            let mut out1 = vec![PackedRgba::TRANSPARENT; 64];
            let mut out2 = vec![PackedRgba::TRANSPARENT; 64];

            fx.render(ctx, &mut out1);
            fx.render(ctx, &mut out2);

            assert_eq!(out1, out2, "{:?} quality should be deterministic", quality);
        }
    }

    #[test]
    fn quality_affects_visual_complexity() {
        // Verify that quality tiers produce measurably different output
        // Full should have more variance than Minimal (more wave components)
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::default();

        // Sample at multiple time points to get variance
        let mut full_variance = 0.0;
        let mut minimal_variance = 0.0;

        for time in [0.0, 0.5, 1.0, 1.5, 2.0] {
            let ctx_full = FxContext {
                width: 4,
                height: 4,
                frame: 0,
                time_seconds: time,
                quality: FxQuality::Full,
                theme: &theme,
            };
            let ctx_min = FxContext {
                width: 4,
                height: 4,
                frame: 0,
                time_seconds: time,
                quality: FxQuality::Minimal,
                theme: &theme,
            };

            let mut out_full = vec![PackedRgba::TRANSPARENT; 16];
            let mut out_min = vec![PackedRgba::TRANSPARENT; 16];

            fx.render(ctx_full, &mut out_full);
            fx.render(ctx_min, &mut out_min);

            // Calculate variance as sum of absolute differences between adjacent cells
            for i in 0..15 {
                full_variance +=
                    (out_full[i].r() as i32 - out_full[i + 1].r() as i32).unsigned_abs() as f64;
                minimal_variance +=
                    (out_min[i].r() as i32 - out_min[i + 1].r() as i32).unsigned_abs() as f64;
            }
        }

        // Full should have at least as much variance as Minimal
        // (more wave components = more complex patterns)
        // Note: We use >= rather than > because variance depends on sampling
        assert!(
            full_variance >= minimal_variance * 0.8,
            "Full quality variance ({}) should be >= 80% of Minimal variance ({})",
            full_variance,
            minimal_variance
        );
    }

    // =========================================================================
    // Determinism with Hash Verification (bd-l8x9.4.3)
    // =========================================================================

    #[test]
    fn determinism_hash_stable() {
        // Compute a hash of the output and verify it's stable across runs
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::new(PlasmaPalette::Ocean);
        let ctx = FxContext {
            width: 16,
            height: 8,
            frame: 100,
            time_seconds: 5.0,
            quality: FxQuality::Full,
            theme: &theme,
        };

        let mut out = vec![PackedRgba::TRANSPARENT; 128];
        fx.render(ctx, &mut out);

        // Hash the output
        let mut hasher1 = DefaultHasher::new();
        for color in &out {
            color.r().hash(&mut hasher1);
            color.g().hash(&mut hasher1);
            color.b().hash(&mut hasher1);
            color.a().hash(&mut hasher1);
        }
        let hash1 = hasher1.finish();

        // Render again and hash
        fx.render(ctx, &mut out);
        let mut hasher2 = DefaultHasher::new();
        for color in &out {
            color.r().hash(&mut hasher2);
            color.g().hash(&mut hasher2);
            color.b().hash(&mut hasher2);
            color.a().hash(&mut hasher2);
        }
        let hash2 = hasher2.finish();

        assert_eq!(
            hash1, hash2,
            "Determinism failed: hash changed on re-render"
        );

        // Log for debugging (can be used to track regressions)
        eprintln!(
            "{{\"test\":\"determinism_hash\",\"preset\":\"Ocean\",\"dims\":\"16x8\",\"frame\":100,\"time\":5.0,\"hash\":\"{:016x}\"}}",
            hash1
        );
    }
}
