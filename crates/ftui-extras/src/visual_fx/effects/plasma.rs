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
    height: u16,
    // Per-column geometry bases (computed once per resize).
    x_v1_sin: Vec<f64>,
    x_v1_cos: Vec<f64>,
    // Per-row geometry bases (computed once per resize).
    y_v2_sin: Vec<f64>,
    y_v2_cos: Vec<f64>,
    // Per-pixel diagonal basis for v3 (computed once per resize).
    diag_sin: Vec<f64>,
    diag_cos: Vec<f64>,
    // Per-pixel geometry bases for full quality (computed once per resize).
    radial_center_sin: Vec<f64>,
    radial_center_cos: Vec<f64>,
    radial_offset_sin: Vec<f64>,
    radial_offset_cos: Vec<f64>,
    interference_sin: Vec<f64>,
    interference_cos: Vec<f64>,
    // Per-frame scratch.
    v1_frame: Vec<f64>,
    v2_frame: Vec<f64>,
}

impl PlasmaScratch {
    const fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            x_v1_sin: Vec::new(),
            x_v1_cos: Vec::new(),
            y_v2_sin: Vec::new(),
            y_v2_cos: Vec::new(),
            diag_sin: Vec::new(),
            diag_cos: Vec::new(),
            radial_center_sin: Vec::new(),
            radial_center_cos: Vec::new(),
            radial_offset_sin: Vec::new(),
            radial_offset_cos: Vec::new(),
            interference_sin: Vec::new(),
            interference_cos: Vec::new(),
            v1_frame: Vec::new(),
            v2_frame: Vec::new(),
        }
    }

    fn ensure_geometry(&mut self, width: u16, height: u16, w: f64, h: f64) {
        if self.width == width && self.height == height {
            return;
        }
        self.width = width;
        self.height = height;
        let w_len = width as usize;
        let h_len = height as usize;

        // Per-column bases.
        self.x_v1_sin.resize(w_len, 0.0);
        self.x_v1_cos.resize(w_len, 0.0);

        let mut x_coords = vec![0.0f64; w_len];
        let mut sin_x2 = vec![0.0f64; w_len];
        let mut x_sq = vec![0.0f64; w_len];
        let mut x_center_sq = vec![0.0f64; w_len];
        let mut x_diag_sin = vec![0.0f64; w_len];
        let mut x_diag_cos = vec![0.0f64; w_len];

        for dx in 0..w_len {
            let x = (dx as f64 / w) * 6.0;
            x_coords[dx] = x;
            x_sq[dx] = x * x;
            let x_center = x - 3.0;
            x_center_sq[dx] = x_center * x_center;
            sin_x2[dx] = (x * 2.0).sin();

            let (s1, c1) = (x * 1.5).sin_cos();
            self.x_v1_sin[dx] = s1;
            self.x_v1_cos[dx] = c1;
            let (sd, cd) = (x * 1.2).sin_cos();
            x_diag_sin[dx] = sd;
            x_diag_cos[dx] = cd;
        }

        // Per-row bases.
        self.y_v2_sin.resize(h_len, 0.0);
        self.y_v2_cos.resize(h_len, 0.0);

        let mut y_coords = vec![0.0f64; h_len];
        let mut cos_y2 = vec![0.0f64; h_len];
        let mut y_diag_sin = vec![0.0f64; h_len];
        let mut y_diag_cos = vec![0.0f64; h_len];

        for dy in 0..h_len {
            let y = (dy as f64 / h) * 6.0;
            y_coords[dy] = y;
            cos_y2[dy] = (y * 2.0).cos();

            let (s2, c2) = (y * 1.8).sin_cos();
            self.y_v2_sin[dy] = s2;
            self.y_v2_cos[dy] = c2;
            let (sd, cd) = (y * 1.2).sin_cos();
            y_diag_sin[dy] = sd;
            y_diag_cos[dy] = cd;
        }

        // Per-pixel bases for full quality.
        let total = w_len.saturating_mul(h_len);
        self.radial_center_sin.resize(total, 0.0);
        self.radial_center_cos.resize(total, 0.0);
        self.radial_offset_sin.resize(total, 0.0);
        self.radial_offset_cos.resize(total, 0.0);
        self.interference_sin.resize(total, 0.0);
        self.interference_cos.resize(total, 0.0);
        self.diag_sin.resize(total, 0.0);
        self.diag_cos.resize(total, 0.0);

        for dy in 0..h_len {
            let y = y_coords[dy];
            let y_sq = y * y;
            let y_center = y - 3.0;
            let y_center_sq = y_center * y_center;
            let cy2 = cos_y2[dy];
            let diag_y_sin = y_diag_sin[dy];
            let diag_y_cos = y_diag_cos[dy];
            let row_offset = dy * w_len;

            for dx in 0..w_len {
                let idx = row_offset + dx;

                let diag_x_sin = x_diag_sin[dx];
                let diag_x_cos = x_diag_cos[dx];
                self.diag_sin[idx] = diag_x_sin * diag_y_cos + diag_x_cos * diag_y_sin;
                self.diag_cos[idx] = diag_x_cos * diag_y_cos - diag_x_sin * diag_y_sin;

                let radial_center = (x_sq[dx] + y_sq).sqrt() * 2.0;
                let radial_offset = (x_center_sq[dx] + y_center_sq).sqrt() * 1.8;
                let (sc, cc) = radial_center.sin_cos();
                self.radial_center_sin[idx] = sc;
                self.radial_center_cos[idx] = cc;
                let (so, co) = radial_offset.sin_cos();
                self.radial_offset_sin[idx] = so;
                self.radial_offset_cos[idx] = co;

                let base = sin_x2[dx] * cy2;
                let (sb, cb) = base.sin_cos();
                self.interference_sin[idx] = sb;
                self.interference_cos[idx] = cb;
            }
        }

        // Per-frame scratch buffers.
        self.v1_frame.resize(w_len, 0.0);
        self.v2_frame.resize(h_len, 0.0);
    }
}

impl PlasmaFx {
    #[inline]
    fn render_with_palette<F>(&mut self, ctx: FxContext<'_>, out: &mut [PackedRgba], mut sample: F)
    where
        F: FnMut(f64) -> PackedRgba,
    {
        if !ctx.quality.is_enabled() || ctx.is_empty() {
            return;
        }
        if out.len() != ctx.len() {
            return;
        }

        let w = ctx.width as f64;
        let h = ctx.height as f64;
        let time = ctx.time_seconds;
        let quality = ctx.quality;

        let scratch = &mut self.scratch;
        scratch.ensure_geometry(ctx.width, ctx.height, w, h);

        // Per-frame time sin/cos pairs (replaces per-pixel trig).
        let (sin_t1, cos_t1) = time.sin_cos();
        let (sin_t2, cos_t2) = (time * 0.8).sin_cos();
        let (sin_t3, cos_t3) = (time * 0.6).sin_cos();
        let (sin_t4, cos_t4) = (time * 1.2).sin_cos();
        let sin_time = sin_t1;
        let cos_time = cos_t1;
        let (sin_t6, cos_t6) = (time * 0.5).sin_cos();
        let breath = 0.85 + 0.15 * (time * 0.3).sin();

        let ww = ctx.width as usize;
        let hh = ctx.height as usize;

        // Pre-compute per-column v1 and per-row v2 via sin(a+b) identity.
        for dx in 0..ww {
            scratch.v1_frame[dx] = scratch.x_v1_sin[dx] * cos_t1 + scratch.x_v1_cos[dx] * sin_t1;
        }
        for dy in 0..hh {
            scratch.v2_frame[dy] = scratch.y_v2_sin[dy] * cos_t2 + scratch.y_v2_cos[dy] * sin_t2;
        }
        let v1_frame = &scratch.v1_frame;
        let diag_sin = &scratch.diag_sin;
        let diag_cos = &scratch.diag_cos;

        // Quality-hoisted loops: zero sin/cos per pixel.
        match quality {
            FxQuality::Full => {
                for dy in 0..hh {
                    let v2 = scratch.v2_frame[dy];
                    let row_offset = dy * ww;
                    let diag_sin_row = &diag_sin[row_offset..row_offset + ww];
                    let diag_cos_row = &diag_cos[row_offset..row_offset + ww];
                    let radial_center_sin_row =
                        &scratch.radial_center_sin[row_offset..row_offset + ww];
                    let radial_center_cos_row =
                        &scratch.radial_center_cos[row_offset..row_offset + ww];
                    let radial_offset_sin_row =
                        &scratch.radial_offset_sin[row_offset..row_offset + ww];
                    let radial_offset_cos_row =
                        &scratch.radial_offset_cos[row_offset..row_offset + ww];
                    let interference_sin_row =
                        &scratch.interference_sin[row_offset..row_offset + ww];
                    let interference_cos_row =
                        &scratch.interference_cos[row_offset..row_offset + ww];
                    let out_row = &mut out[row_offset..row_offset + ww];
                    for dx in 0..ww {
                        let v1 = v1_frame[dx];
                        let v3 = diag_sin_row[dx] * cos_t3 + diag_cos_row[dx] * sin_t3;
                        let v4 =
                            radial_center_sin_row[dx] * cos_t4 - radial_center_cos_row[dx] * sin_t4;
                        let v5 = radial_offset_cos_row[dx] * cos_time
                            - radial_offset_sin_row[dx] * sin_time;
                        let v6 =
                            interference_sin_row[dx] * cos_t6 + interference_cos_row[dx] * sin_t6;
                        let value = (v1 + v2 + v3 + v4 + v5 + v6) / 6.0;
                        let wave = ((value * breath) + 1.0) / 2.0;
                        out_row[dx] = sample(wave.clamp(0.0, 1.0));
                    }
                }
            }
            FxQuality::Reduced => {
                for dy in 0..hh {
                    let v2 = scratch.v2_frame[dy];
                    let row_offset = dy * ww;
                    let diag_sin_row = &diag_sin[row_offset..row_offset + ww];
                    let diag_cos_row = &diag_cos[row_offset..row_offset + ww];
                    let interference_sin_row =
                        &scratch.interference_sin[row_offset..row_offset + ww];
                    let interference_cos_row =
                        &scratch.interference_cos[row_offset..row_offset + ww];
                    let out_row = &mut out[row_offset..row_offset + ww];
                    for dx in 0..ww {
                        let v1 = v1_frame[dx];
                        let v3 = diag_sin_row[dx] * cos_t3 + diag_cos_row[dx] * sin_t3;
                        let v6 =
                            interference_sin_row[dx] * cos_t6 + interference_cos_row[dx] * sin_t6;
                        let value = (v1 + v2 + v3 + v6) / 4.0;
                        let wave = ((value * breath) + 1.0) / 2.0;
                        out_row[dx] = sample(wave.clamp(0.0, 1.0));
                    }
                }
            }
            FxQuality::Minimal => {
                for dy in 0..hh {
                    let v2 = scratch.v2_frame[dy];
                    let row_offset = dy * ww;
                    let diag_sin_row = &diag_sin[row_offset..row_offset + ww];
                    let diag_cos_row = &diag_cos[row_offset..row_offset + ww];
                    let out_row = &mut out[row_offset..row_offset + ww];
                    for dx in 0..ww {
                        let v1 = v1_frame[dx];
                        let v3 = diag_sin_row[dx] * cos_t3 + diag_cos_row[dx] * sin_t3;
                        let value = (v1 + v2 + v3) / 3.0;
                        let wave = (value + 1.0) / 2.0;
                        out_row[dx] = sample(wave.clamp(0.0, 1.0));
                    }
                }
            }
            FxQuality::Off => {}
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
                // Precompute theme stops once (avoid per-pixel theme deref).
                let bg_surface = theme.bg_surface;
                let accent_primary = theme.accent_primary;
                let accent_secondary = theme.accent_secondary;
                let fg_primary = theme.fg_primary;
                self.render_with_palette(ctx, out, |t| {
                    if t < 0.33 {
                        let s = t / 0.33;
                        PlasmaPalette::lerp_color(bg_surface, accent_primary, s)
                    } else if t < 0.66 {
                        let s = (t - 0.33) / 0.33;
                        PlasmaPalette::lerp_color(accent_primary, accent_secondary, s)
                    } else {
                        let s = (t - 0.66) / 0.34;
                        PlasmaPalette::lerp_color(accent_secondary, fg_primary, s)
                    }
                });
            }
            PlasmaPalette::Aurora => {
                // Precompute fallbacks/stops once (avoid per-pixel slot checks).
                let bg_surface = theme.bg_surface;
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
                let accent_primary = theme.accent_primary;
                self.render_with_palette(ctx, out, |t| {
                    if t < 0.33 {
                        let s = t / 0.33;
                        PlasmaPalette::lerp_color(bg_surface, cool1, s)
                    } else if t < 0.66 {
                        let s = (t - 0.33) / 0.33;
                        PlasmaPalette::lerp_color(cool1, cool2, s)
                    } else {
                        let s = (t - 0.66) / 0.34;
                        PlasmaPalette::lerp_color(cool2, accent_primary, s)
                    }
                });
            }
            PlasmaPalette::Ember => {
                // Precompute fallbacks/stops once (avoid per-pixel slot checks).
                let bg_surface = theme.bg_surface;
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
                let accent_secondary = theme.accent_secondary;
                self.render_with_palette(ctx, out, |t| {
                    if t < 0.33 {
                        let s = t / 0.33;
                        PlasmaPalette::lerp_color(bg_surface, warm1, s)
                    } else if t < 0.66 {
                        let s = (t - 0.33) / 0.33;
                        PlasmaPalette::lerp_color(warm1, warm2, s)
                    } else {
                        let s = (t - 0.66) / 0.34;
                        PlasmaPalette::lerp_color(warm2, accent_secondary, s)
                    }
                });
            }
            PlasmaPalette::Subtle => {
                // Precompute theme stops once (avoid per-pixel theme deref).
                let bg_base = theme.bg_base;
                let bg_surface = theme.bg_surface;
                let bg_overlay = theme.bg_overlay;
                self.render_with_palette(ctx, out, |t| {
                    if t < 0.5 {
                        let s = t / 0.5;
                        PlasmaPalette::lerp_color(bg_base, bg_surface, s)
                    } else {
                        let s = (t - 0.5) / 0.5;
                        PlasmaPalette::lerp_color(bg_surface, bg_overlay, s)
                    }
                });
            }
            PlasmaPalette::Monochrome => {
                // Precompute theme stops once (avoid per-pixel theme deref).
                let bg_base = theme.bg_base;
                let fg_primary = theme.fg_primary;
                self.render_with_palette(ctx, out, |t| {
                    PlasmaPalette::lerp_color(bg_base, fg_primary, t)
                });
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
    fn full_quality_matches_reference_wave_formula() {
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::new(PlasmaPalette::Sunset);
        let ctx = FxContext {
            width: 17,
            height: 9,
            frame: 7,
            time_seconds: 1.2345,
            quality: FxQuality::Full,
            theme: &theme,
        };
        let mut out = vec![PackedRgba::TRANSPARENT; ctx.len()];
        fx.render(ctx, &mut out);

        let w = ctx.width as f64;
        let h = ctx.height as f64;
        for dy in 0..ctx.height {
            for dx in 0..ctx.width {
                let idx = dy as usize * ctx.width as usize + dx as usize;
                let nx = dx as f64 / w;
                let ny = dy as f64 / h;
                let expected =
                    PlasmaPalette::sunset(plasma_wave(nx, ny, ctx.time_seconds).clamp(0.0, 1.0));
                assert_eq!(
                    out[idx], expected,
                    "mismatch at ({dx}, {dy}) with t={}",
                    ctx.time_seconds
                );
            }
        }
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
    fn length_mismatch_is_ignored_without_mutating_output() {
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::default();
        let ctx = ctx(&theme);
        let sentinel = PackedRgba::rgb(1, 2, 3);
        let mut out = vec![sentinel; ctx.len().saturating_sub(1)];

        fx.render(ctx, &mut out);

        assert!(out.iter().all(|px| *px == sentinel));
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
        // On 64-bit: palette (1-2 bytes) + PlasmaScratch (16 Vecs at 24 bytes each + 2×u16)
        // after sin/cos decomposition optimization
        assert!(
            size1 <= 408,
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
            "Determinism failed: hash changed on re-render (hash1={hash1:016x}, hash2={hash2:016x})"
        );
    }

    #[test]
    fn plasma_wave_output_in_0_1() {
        for i in 0..100 {
            let nx = i as f64 / 100.0;
            let ny = (100 - i) as f64 / 100.0;
            let v = plasma_wave(nx, ny, i as f64 * 0.1);
            assert!((0.0..=1.0).contains(&v), "plasma_wave out of [0,1]: {v}");
        }
    }

    #[test]
    fn plasma_wave_low_output_in_0_1() {
        for i in 0..100 {
            let nx = i as f64 / 100.0;
            let ny = (100 - i) as f64 / 100.0;
            let v = plasma_wave_low(nx, ny, i as f64 * 0.1);
            assert!(
                (0.0..=1.0).contains(&v),
                "plasma_wave_low out of [0,1]: {v}"
            );
        }
    }

    #[test]
    fn plasma_wave_deterministic() {
        let a = plasma_wave(0.5, 0.5, 1.0);
        let b = plasma_wave(0.5, 0.5, 1.0);
        assert_eq!(a, b);
    }

    #[test]
    fn factory_methods_set_correct_palette() {
        assert!(matches!(
            PlasmaFx::theme().palette(),
            PlasmaPalette::ThemeAccents
        ));
        assert!(matches!(
            PlasmaFx::sunset().palette(),
            PlasmaPalette::Sunset
        ));
        assert!(matches!(PlasmaFx::ocean().palette(), PlasmaPalette::Ocean));
        assert!(matches!(PlasmaFx::fire().palette(), PlasmaPalette::Fire));
        assert!(matches!(PlasmaFx::neon().palette(), PlasmaPalette::Neon));
    }

    #[test]
    fn factory_methods_remaining_palettes() {
        assert!(matches!(
            PlasmaFx::cyberpunk().palette(),
            PlasmaPalette::Cyberpunk
        ));
        assert!(matches!(
            PlasmaFx::aurora().palette(),
            PlasmaPalette::Aurora
        ));
        assert!(matches!(PlasmaFx::ember().palette(), PlasmaPalette::Ember));
        assert!(matches!(
            PlasmaFx::subtle().palette(),
            PlasmaPalette::Subtle
        ));
        assert!(matches!(
            PlasmaFx::monochrome().palette(),
            PlasmaPalette::Monochrome
        ));
    }

    #[test]
    fn fx_name_returns_plasma() {
        let fx = PlasmaFx::default();
        assert_eq!(fx.name(), "plasma");
    }

    #[test]
    fn fx_default_is_theme_accents() {
        let fx = PlasmaFx::default();
        assert_eq!(fx.palette(), PlasmaPalette::ThemeAccents);
    }

    #[test]
    fn palette_default_is_theme_accents() {
        let palette = PlasmaPalette::default();
        assert_eq!(palette, PlasmaPalette::ThemeAccents);
    }

    #[test]
    fn galaxy_palette_renders_valid_colors() {
        let theme = ThemeInputs::default_dark();
        for i in 0..=10 {
            let t = i as f64 / 10.0;
            let color = PlasmaPalette::Galaxy.color_at(t, &theme);
            // Just verify we get valid RGB values (no panics)
            let _ = (color.r(), color.g(), color.b());
        }
    }

    #[test]
    fn cyberpunk_palette_renders_valid_colors() {
        let theme = ThemeInputs::default_dark();
        let start = PlasmaPalette::Cyberpunk.color_at(0.0, &theme);
        let end = PlasmaPalette::Cyberpunk.color_at(1.0, &theme);
        // Cyberpunk starts with hot pink (high R)
        assert!(start.r() > 200);
        // Cyberpunk ends with cyan (high B and G)
        assert!(end.b() > 200);
    }

    #[test]
    fn plasma_wave_low_deterministic() {
        let a = plasma_wave_low(0.5, 0.5, 1.0);
        let b = plasma_wave_low(0.5, 0.5, 1.0);
        assert_eq!(a, b);
    }

    #[test]
    fn monochrome_palette_endpoints() {
        let theme = ThemeInputs::default_dark();
        let at_zero = PlasmaPalette::Monochrome.color_at(0.0, &theme);
        let at_one = PlasmaPalette::Monochrome.color_at(1.0, &theme);
        // At t=0, should match bg_base
        assert_eq!(at_zero.r(), theme.bg_base.r());
        assert_eq!(at_zero.g(), theme.bg_base.g());
        // At t=1, should match fg_primary
        assert_eq!(at_one.r(), theme.fg_primary.r());
        assert_eq!(at_one.g(), theme.fg_primary.g());
    }

    #[test]
    fn hsv_to_rgb_green_and_blue() {
        let green = PlasmaPalette::hsv_to_rgb(120.0, 1.0, 1.0);
        assert!(green.g() > 200);
        assert!(green.r() < 10);
        assert!(green.b() < 10);

        let blue = PlasmaPalette::hsv_to_rgb(240.0, 1.0, 1.0);
        assert!(blue.b() > 200);
        assert!(blue.r() < 10);
        assert!(blue.g() < 10);
    }

    #[test]
    fn palette_is_theme_derived_correct() {
        assert!(!PlasmaPalette::Sunset.is_theme_derived());
        assert!(!PlasmaPalette::Ocean.is_theme_derived());
        assert!(!PlasmaPalette::Fire.is_theme_derived());
        assert!(!PlasmaPalette::Neon.is_theme_derived());
        assert!(!PlasmaPalette::Cyberpunk.is_theme_derived());
        assert!(!PlasmaPalette::Galaxy.is_theme_derived());
    }

    // =========================================================================
    // Reduced Quality Reference Formula (bd-50ltp)
    // =========================================================================

    #[test]
    fn reduced_quality_matches_4_component_formula() {
        // Reduced quality uses v1, v2, v3, v6 (4 components).
        // Verify the rendered output matches the expected formula.
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::new(PlasmaPalette::Sunset);
        let ctx = FxContext {
            width: 11,
            height: 7,
            frame: 3,
            time_seconds: 2.345,
            quality: FxQuality::Reduced,
            theme: &theme,
        };
        let mut out = vec![PackedRgba::TRANSPARENT; ctx.len()];
        fx.render(ctx, &mut out);

        let w = ctx.width as f64;
        let h = ctx.height as f64;
        let time = ctx.time_seconds;
        let breath = 0.85 + 0.15 * (time * 0.3).sin();

        for dy in 0..ctx.height {
            for dx in 0..ctx.width {
                let idx = dy as usize * ctx.width as usize + dx as usize;
                let x = (dx as f64 / w) * 6.0;
                let y = (dy as f64 / h) * 6.0;

                let v1 = (x * 1.5 + time).sin();
                let v2 = (y * 1.8 + time * 0.8).sin();
                let v3 = ((x + y) * 1.2 + time * 0.6).sin();
                let v6 = ((x * 2.0).sin() * (y * 2.0).cos() + time * 0.5).sin();
                let value = (v1 + v2 + v3 + v6) / 4.0;
                let wave = ((value * breath) + 1.0) / 2.0;
                let expected = PlasmaPalette::sunset(wave.clamp(0.0, 1.0));

                assert_eq!(
                    out[idx], expected,
                    "reduced quality mismatch at ({dx}, {dy})"
                );
            }
        }
    }

    #[test]
    fn reduced_quality_differs_from_full_and_minimal() {
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::new(PlasmaPalette::Ocean);
        let base = FxContext {
            width: 10,
            height: 6,
            frame: 5,
            time_seconds: 1.5,
            quality: FxQuality::Full,
            theme: &theme,
        };

        let mut out_full = vec![PackedRgba::TRANSPARENT; base.len()];
        fx.render(base, &mut out_full);

        let ctx_reduced = FxContext {
            quality: FxQuality::Reduced,
            ..base
        };
        let mut out_reduced = vec![PackedRgba::TRANSPARENT; base.len()];
        fx.render(ctx_reduced, &mut out_reduced);

        let ctx_minimal = FxContext {
            quality: FxQuality::Minimal,
            ..base
        };
        let mut out_minimal = vec![PackedRgba::TRANSPARENT; base.len()];
        fx.render(ctx_minimal, &mut out_minimal);

        assert_ne!(out_full, out_reduced, "Full should differ from Reduced");
        assert_ne!(
            out_reduced, out_minimal,
            "Reduced should differ from Minimal"
        );
    }

    // =========================================================================
    // HSV Edge Cases (bd-50ltp)
    // =========================================================================

    #[test]
    fn hsv_secondary_colors() {
        // Yellow = 60 degrees
        let yellow = PlasmaPalette::hsv_to_rgb(60.0, 1.0, 1.0);
        assert!(
            yellow.r() > 200,
            "Yellow should have high R: {}",
            yellow.r()
        );
        assert!(
            yellow.g() > 200,
            "Yellow should have high G: {}",
            yellow.g()
        );
        assert!(yellow.b() < 30, "Yellow should have low B: {}", yellow.b());

        // Cyan = 180 degrees
        let cyan = PlasmaPalette::hsv_to_rgb(180.0, 1.0, 1.0);
        assert!(cyan.r() < 10, "Cyan should have low R: {}", cyan.r());
        assert!(cyan.g() > 200, "Cyan should have high G: {}", cyan.g());
        assert!(cyan.b() > 200, "Cyan should have high B: {}", cyan.b());

        // Magenta = 300 degrees
        let magenta = PlasmaPalette::hsv_to_rgb(300.0, 1.0, 1.0);
        assert!(
            magenta.r() > 200,
            "Magenta should have high R: {}",
            magenta.r()
        );
        assert!(
            magenta.g() < 10,
            "Magenta should have low G: {}",
            magenta.g()
        );
        assert!(
            magenta.b() > 200,
            "Magenta should have high B: {}",
            magenta.b()
        );
    }

    #[test]
    fn hsv_hue_wrapping_at_360() {
        // Hue 360 should wrap to the same as hue 0 (red)
        let at_0 = PlasmaPalette::hsv_to_rgb(0.0, 1.0, 1.0);
        let at_360 = PlasmaPalette::hsv_to_rgb(360.0, 1.0, 1.0);
        assert_eq!(at_0, at_360, "360 degrees should wrap to 0 degrees");

        // Hue 720 should also wrap to red
        let at_720 = PlasmaPalette::hsv_to_rgb(720.0, 1.0, 1.0);
        assert_eq!(at_0, at_720, "720 degrees should wrap to 0 degrees");
    }

    #[test]
    fn hsv_zero_saturation_gives_gray() {
        // With S=0, all hues should produce the same gray
        let gray1 = PlasmaPalette::hsv_to_rgb(0.0, 0.0, 0.5);
        let gray2 = PlasmaPalette::hsv_to_rgb(120.0, 0.0, 0.5);
        let gray3 = PlasmaPalette::hsv_to_rgb(240.0, 0.0, 0.5);
        assert_eq!(gray1, gray2, "S=0 should produce identical grays");
        assert_eq!(gray2, gray3, "S=0 should produce identical grays");
        // All channels should be equal (gray)
        assert_eq!(gray1.r(), gray1.g());
        assert_eq!(gray1.g(), gray1.b());
    }

    #[test]
    fn hsv_zero_value_gives_black() {
        // V=0 should give black regardless of hue or saturation
        let black1 = PlasmaPalette::hsv_to_rgb(0.0, 1.0, 0.0);
        let black2 = PlasmaPalette::hsv_to_rgb(180.0, 0.5, 0.0);
        assert_eq!(black1, PackedRgba::rgb(0, 0, 0));
        assert_eq!(black2, PackedRgba::rgb(0, 0, 0));
    }

    #[test]
    fn hsv_full_saturation_full_value_covers_all_sextants() {
        // Walk through all 6 sextants of the HSV color wheel
        let hues = [0.0, 60.0, 120.0, 180.0, 240.0, 300.0];
        let mut previous = PackedRgba::rgb(0, 0, 0);
        for (i, &hue) in hues.iter().enumerate() {
            let color = PlasmaPalette::hsv_to_rgb(hue, 1.0, 1.0);
            if i > 0 {
                assert_ne!(
                    color, previous,
                    "HSV sextant at hue={hue} should differ from previous"
                );
            }
            previous = color;
        }
    }

    // =========================================================================
    // Scratch Geometry Caching (bd-50ltp)
    // =========================================================================

    #[test]
    fn scratch_geometry_cached_on_same_dimensions() {
        // Rendering at the same size twice should reuse geometry (no recompute).
        // Verify by checking determinism and that output is identical.
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::new(PlasmaPalette::Sunset);
        let ctx = FxContext {
            width: 12,
            height: 8,
            frame: 0,
            time_seconds: 1.0,
            quality: FxQuality::Full,
            theme: &theme,
        };

        let mut out1 = vec![PackedRgba::TRANSPARENT; ctx.len()];
        fx.render(ctx, &mut out1);

        // Same dimensions, different frame/time
        let ctx2 = FxContext {
            frame: 1,
            time_seconds: 2.0,
            ..ctx
        };
        let mut out2 = vec![PackedRgba::TRANSPARENT; ctx2.len()];
        fx.render(ctx2, &mut out2);

        // Outputs should differ (different time) but both should be valid
        assert_ne!(
            out1, out2,
            "Different times should produce different output"
        );

        // Re-render at original time to verify scratch wasn't corrupted
        let mut out3 = vec![PackedRgba::TRANSPARENT; ctx.len()];
        fx.render(ctx, &mut out3);
        assert_eq!(out1, out3, "Re-render at same params should match original");
    }

    #[test]
    fn scratch_geometry_recomputes_on_resize() {
        // Rendering at different sizes should produce different outputs because
        // geometry is recomputed for new dimensions.
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::new(PlasmaPalette::Sunset);

        let ctx_small = FxContext {
            width: 8,
            height: 4,
            frame: 0,
            time_seconds: 1.0,
            quality: FxQuality::Full,
            theme: &theme,
        };
        let mut out_small = vec![PackedRgba::TRANSPARENT; ctx_small.len()];
        fx.render(ctx_small, &mut out_small);

        let ctx_large = FxContext {
            width: 16,
            height: 8,
            frame: 0,
            time_seconds: 1.0,
            quality: FxQuality::Full,
            theme: &theme,
        };
        let mut out_large = vec![PackedRgba::TRANSPARENT; ctx_large.len()];
        fx.render(ctx_large, &mut out_large);

        // Different sizes, so output arrays differ in length at minimum
        assert_ne!(out_small.len(), out_large.len());

        // Re-render small to verify geometry was recomputed properly
        let mut out_small2 = vec![PackedRgba::TRANSPARENT; ctx_small.len()];
        fx.render(ctx_small, &mut out_small2);
        assert_eq!(
            out_small, out_small2,
            "Switching back to original size should produce same output"
        );
    }

    // =========================================================================
    // Palette Segment Boundary Tests (bd-50ltp)
    // =========================================================================

    #[test]
    fn fire_palette_segment_boundaries() {
        // Fire has 5 segments at boundaries: 0.0, 0.2, 0.4, 0.6, 0.8, 1.0
        let theme = ThemeInputs::default_dark();
        let fire = PlasmaPalette::Fire;

        // Start should be near black
        let start = fire.color_at(0.0, &theme);
        assert!(
            start.r() < 10 && start.g() < 10 && start.b() < 10,
            "Fire start should be near black: ({}, {}, {})",
            start.r(),
            start.g(),
            start.b()
        );

        // End should be near white-yellow
        let end = fire.color_at(1.0, &theme);
        assert!(
            end.r() > 200 && end.g() > 200 && end.b() > 150,
            "Fire end should be light: ({}, {}, {})",
            end.r(),
            end.g(),
            end.b()
        );

        // Monotonically increasing brightness across segments
        let mut prev_lum = 0u32;
        for i in 0..=10 {
            let t = i as f64 / 10.0;
            let c = fire.color_at(t, &theme);
            let lum = c.r() as u32 * 299 + c.g() as u32 * 587 + c.b() as u32 * 114;
            assert!(
                lum >= prev_lum,
                "Fire brightness should increase: at t={t}, lum={lum} < prev={prev_lum}"
            );
            prev_lum = lum;
        }
    }

    #[test]
    fn galaxy_palette_segment_boundaries() {
        // Galaxy: deep space -> indigo -> magenta -> light -> white-ish
        // 4 segments at: 0.0, 0.3, 0.6, 0.85, 1.0
        let theme = ThemeInputs::default_dark();
        let galaxy = PlasmaPalette::Galaxy;

        // Start should be very dark
        let start = galaxy.color_at(0.0, &theme);
        let start_lum = start.r() as u32 + start.g() as u32 + start.b() as u32;
        assert!(
            start_lum < 30,
            "Galaxy start should be near-black: lum={}",
            start_lum
        );

        // End should be bright
        let end = galaxy.color_at(1.0, &theme);
        let end_lum = end.r() as u32 + end.g() as u32 + end.b() as u32;
        assert!(
            end_lum > 600,
            "Galaxy end should be bright: lum={}",
            end_lum
        );

        // Mid-point should have a purple/magenta hue
        let mid = galaxy.color_at(0.6, &theme);
        assert!(
            mid.r() > mid.g(),
            "Galaxy mid should have magenta (R > G): r={}, g={}",
            mid.r(),
            mid.g()
        );
    }

    #[test]
    fn neon_palette_full_hue_cycle() {
        // Neon maps t in [0,1] to the full HSV hue cycle [0, 360].
        // At 6 evenly spaced points, we should see R, Y, G, C, B, M.
        let theme = ThemeInputs::default_dark();
        let neon = PlasmaPalette::Neon;

        // t=0 ~ red, t=1/6 ~ yellow, t=2/6 ~ green, etc.
        let red = neon.color_at(0.0, &theme);
        assert!(
            red.r() > 200 && red.g() < 50,
            "Neon at t=0 should be red-ish"
        );

        let green = neon.color_at(1.0 / 3.0, &theme);
        assert!(
            green.g() > 200 && green.r() < 50,
            "Neon at t=1/3 should be green-ish: r={} g={}",
            green.r(),
            green.g()
        );

        let blue = neon.color_at(2.0 / 3.0, &theme);
        assert!(
            blue.b() > 200 && blue.g() < 50,
            "Neon at t=2/3 should be blue-ish: g={} b={}",
            blue.g(),
            blue.b()
        );
    }

    #[test]
    fn sunset_palette_endpoint_colors() {
        let theme = ThemeInputs::default_dark();
        let sunset = PlasmaPalette::Sunset;

        // Start: deep purple (high R and B, low G)
        let start = sunset.color_at(0.0, &theme);
        assert!(
            start.b() > start.g(),
            "Sunset start should be purple-ish: g={} b={}",
            start.g(),
            start.b()
        );

        // End: yellow (high R and G)
        let end = sunset.color_at(1.0, &theme);
        assert!(end.r() > 200, "Sunset end should have high R");
        assert!(end.g() > 200, "Sunset end should have high G");
    }

    #[test]
    fn ocean_palette_endpoint_colors() {
        let theme = ThemeInputs::default_dark();
        let ocean = PlasmaPalette::Ocean;

        // Start: deep blue
        let start = ocean.color_at(0.0, &theme);
        assert!(
            start.b() > start.r() && start.b() > start.g(),
            "Ocean start should be blue: r={} g={} b={}",
            start.r(),
            start.g(),
            start.b()
        );

        // End: seafoam (high G, moderate B, some R)
        let end = ocean.color_at(1.0, &theme);
        assert!(
            end.g() > 200,
            "Ocean end should be seafoam with high G: g={}",
            end.g()
        );
    }

    // =========================================================================
    // Lerp Midpoint Accuracy (bd-50ltp)
    // =========================================================================

    #[test]
    fn lerp_rgb_midpoint_accuracy() {
        // At t=0.5, the result should be near the average of the endpoints
        let a = (0, 0, 0);
        let b = (200, 100, 50);
        let mid = PlasmaPalette::lerp_rgb(a, b, 0.5);
        // Allow +/- 1 for fixed-point rounding
        assert!((mid.0 as i32 - 100).abs() <= 1, "R midpoint: {}", mid.0);
        assert!((mid.1 as i32 - 50).abs() <= 1, "G midpoint: {}", mid.1);
        assert!((mid.2 as i32 - 25).abs() <= 1, "B midpoint: {}", mid.2);
    }

    #[test]
    fn lerp_color_midpoint_accuracy() {
        let a = PackedRgba::rgb(0, 0, 0);
        let b = PackedRgba::rgb(200, 100, 50);
        let mid = PlasmaPalette::lerp_color(a, b, 0.5);
        assert!((mid.r() as i32 - 100).abs() <= 1, "R midpoint: {}", mid.r());
        assert!((mid.g() as i32 - 50).abs() <= 1, "G midpoint: {}", mid.g());
        assert!((mid.b() as i32 - 25).abs() <= 1, "B midpoint: {}", mid.b());
    }

    #[test]
    fn lerp_rgb_clamping_at_boundaries() {
        // t values beyond [0, 1] should be clamped
        let a = (50, 100, 150);
        let b = (200, 210, 220);
        let below = PlasmaPalette::lerp_rgb(a, b, -1.0);
        let above = PlasmaPalette::lerp_rgb(a, b, 2.0);
        assert_eq!(below, a, "t < 0 should clamp to start");
        assert_eq!(above, b, "t > 1 should clamp to end");
    }

    // =========================================================================
    // Breathing Envelope (bd-50ltp)
    // =========================================================================

    #[test]
    fn breathing_envelope_modulates_wave() {
        // The breathing envelope is: 0.85 + 0.15 * sin(time * 0.3)
        // At time=0, sin(0)=0 => breath=0.85
        // At time=pi/(2*0.3)~=5.236, sin(pi/2)=1 => breath=1.0
        // Verify that outputs differ between these time points.
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::new(PlasmaPalette::Sunset);

        let ctx1 = FxContext {
            width: 6,
            height: 4,
            frame: 0,
            time_seconds: 0.0, // breath = 0.85
            quality: FxQuality::Full,
            theme: &theme,
        };
        let mut out1 = vec![PackedRgba::TRANSPARENT; ctx1.len()];
        fx.render(ctx1, &mut out1);

        let ctx2 = FxContext {
            time_seconds: std::f64::consts::FRAC_PI_2 / 0.3, // breath = 1.0
            ..ctx1
        };
        let mut out2 = vec![PackedRgba::TRANSPARENT; ctx2.len()];
        fx.render(ctx2, &mut out2);

        // Different breath values should produce different outputs
        assert_ne!(
            out1, out2,
            "Different breathing phases should produce different output"
        );
    }

    // =========================================================================
    // Extreme Time Values (bd-50ltp)
    // =========================================================================

    #[test]
    fn extreme_time_values_do_not_panic() {
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::default();
        let mut out = vec![PackedRgba::TRANSPARENT; 64];

        for &time in &[0.0, 0.001, 100.0, 10_000.0, 1e6, f64::MIN_POSITIVE] {
            let ctx = FxContext {
                width: 8,
                height: 8,
                frame: 0,
                time_seconds: time,
                quality: FxQuality::Full,
                theme: &theme,
            };
            fx.render(ctx, &mut out);
            // Should not panic and should produce some non-transparent pixels
            let filled = out
                .iter()
                .filter(|c| **c != PackedRgba::TRANSPARENT)
                .count();
            assert!(filled > 0, "time={time}: should produce output");
        }
    }

    #[test]
    fn wave_extreme_inputs_in_valid_range() {
        // Test plasma_wave with extreme but valid inputs
        for &time in &[0.0, 1e3, 1e6] {
            for &nx in &[0.0, 0.5, 1.0] {
                for &ny in &[0.0, 0.5, 1.0] {
                    let v = plasma_wave(nx, ny, time);
                    assert!(
                        (0.0..=1.0).contains(&v),
                        "plasma_wave({nx}, {ny}, {time}) = {v} out of [0,1]"
                    );
                    let v_low = plasma_wave_low(nx, ny, time);
                    assert!(
                        (0.0..=1.0).contains(&v_low),
                        "plasma_wave_low({nx}, {ny}, {time}) = {v_low} out of [0,1]"
                    );
                }
            }
        }
    }

    // =========================================================================
    // Aurora/Ember with Custom Theme Slots (bd-50ltp)
    // =========================================================================

    #[test]
    fn aurora_uses_custom_theme_slots_when_non_transparent() {
        // Create a theme with custom accent_slots to verify the non-fallback path
        let mut theme = ThemeInputs::default_dark();
        theme.accent_slots[0] = PackedRgba::rgb(100, 50, 200); // Custom purple
        theme.accent_slots[1] = PackedRgba::rgb(50, 255, 100); // Custom green

        let aurora_custom = PlasmaPalette::Aurora.color_at(0.5, &theme);

        // Now test with transparent slots (fallback path)
        let mut theme_fallback = ThemeInputs::default_dark();
        theme_fallback.accent_slots[0] = PackedRgba::TRANSPARENT;
        theme_fallback.accent_slots[1] = PackedRgba::TRANSPARENT;

        let aurora_fallback = PlasmaPalette::Aurora.color_at(0.5, &theme_fallback);

        // Custom and fallback should produce different colors at the same t
        assert_ne!(
            aurora_custom, aurora_fallback,
            "Aurora with custom slots should differ from fallback"
        );
    }

    #[test]
    fn ember_uses_custom_theme_slots_when_non_transparent() {
        let mut theme = ThemeInputs::default_dark();
        theme.accent_slots[2] = PackedRgba::rgb(255, 0, 0); // Custom red
        theme.accent_slots[3] = PackedRgba::rgb(255, 255, 0); // Custom yellow

        let ember_custom = PlasmaPalette::Ember.color_at(0.5, &theme);

        let mut theme_fallback = ThemeInputs::default_dark();
        theme_fallback.accent_slots[2] = PackedRgba::TRANSPARENT;
        theme_fallback.accent_slots[3] = PackedRgba::TRANSPARENT;

        let ember_fallback = PlasmaPalette::Ember.color_at(0.5, &theme_fallback);

        assert_ne!(
            ember_custom, ember_fallback,
            "Ember with custom slots should differ from fallback"
        );
    }

    // =========================================================================
    // PlasmaFx Clone and Debug (bd-50ltp)
    // =========================================================================

    #[test]
    fn plasma_fx_clone_produces_identical_output() {
        let theme = ThemeInputs::default_dark();
        let mut fx1 = PlasmaFx::new(PlasmaPalette::Cyberpunk);

        // Warm up the scratch buffer
        let ctx = FxContext {
            width: 10,
            height: 6,
            frame: 0,
            time_seconds: 1.0,
            quality: FxQuality::Full,
            theme: &theme,
        };
        let mut out1 = vec![PackedRgba::TRANSPARENT; ctx.len()];
        fx1.render(ctx, &mut out1);

        // Clone and render again
        let mut fx2 = fx1.clone();
        let mut out2 = vec![PackedRgba::TRANSPARENT; ctx.len()];
        fx2.render(ctx, &mut out2);

        assert_eq!(out1, out2, "Clone should produce identical render output");
    }

    #[test]
    fn plasma_fx_debug_includes_palette() {
        let fx = PlasmaFx::new(PlasmaPalette::Galaxy);
        let debug = format!("{fx:?}");
        assert!(
            debug.contains("Galaxy"),
            "Debug output should mention palette: {debug}"
        );
    }

    // =========================================================================
    // Minimal Quality Reference Formula (bd-50ltp)
    // =========================================================================

    #[test]
    fn minimal_quality_matches_3_component_formula() {
        // Minimal quality uses v1, v2, v3 (3 components) without breathing envelope.
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::new(PlasmaPalette::Ocean);
        let ctx = FxContext {
            width: 9,
            height: 5,
            frame: 2,
            time_seconds: 0.789,
            quality: FxQuality::Minimal,
            theme: &theme,
        };
        let mut out = vec![PackedRgba::TRANSPARENT; ctx.len()];
        fx.render(ctx, &mut out);

        let w = ctx.width as f64;
        let h = ctx.height as f64;
        let time = ctx.time_seconds;

        for dy in 0..ctx.height {
            for dx in 0..ctx.width {
                let idx = dy as usize * ctx.width as usize + dx as usize;
                let nx = dx as f64 / w;
                let ny = dy as f64 / h;
                let expected = PlasmaPalette::ocean(plasma_wave_low(nx, ny, time).clamp(0.0, 1.0));

                assert_eq!(
                    out[idx], expected,
                    "minimal quality mismatch at ({dx}, {dy})"
                );
            }
        }
    }

    // =========================================================================
    // Lerp Edge Cases (bd-17wlv)
    // =========================================================================

    #[test]
    fn lerp_rgb_same_values_returns_identity() {
        let a = (128, 64, 200);
        let mid = PlasmaPalette::lerp_rgb(a, a, 0.5);
        // With same endpoints, any t should return (approximately) the same value.
        // Allow +/- 1 for fixed-point rounding.
        assert!((mid.0 as i32 - a.0 as i32).abs() <= 1);
        assert!((mid.1 as i32 - a.1 as i32).abs() <= 1);
        assert!((mid.2 as i32 - a.2 as i32).abs() <= 1);
    }

    #[test]
    fn lerp_color_same_values_returns_identity() {
        let c = PackedRgba::rgb(77, 155, 233);
        let mid = PlasmaPalette::lerp_color(c, c, 0.5);
        assert!((mid.r() as i32 - c.r() as i32).abs() <= 1);
        assert!((mid.g() as i32 - c.g() as i32).abs() <= 1);
        assert!((mid.b() as i32 - c.b() as i32).abs() <= 1);
    }

    #[test]
    fn lerp_rgb_max_values() {
        let white = (255, 255, 255);
        let black = (0, 0, 0);
        let mid = PlasmaPalette::lerp_rgb(black, white, 0.5);
        // Should be near 127-128
        assert!((mid.0 as i32 - 127).abs() <= 1);
        assert!((mid.1 as i32 - 127).abs() <= 1);
        assert!((mid.2 as i32 - 127).abs() <= 1);

        // White to white should stay white
        let ww = PlasmaPalette::lerp_rgb(white, white, 0.5);
        assert!((ww.0 as i32 - 255).abs() <= 1);
    }

    #[test]
    fn lerp_rgb_quarter_and_three_quarter() {
        let a = (0, 0, 0);
        let b = (200, 100, 40);
        let q = PlasmaPalette::lerp_rgb(a, b, 0.25);
        let tq = PlasmaPalette::lerp_rgb(a, b, 0.75);
        // Quarter should be approximately 50, 25, 10
        assert!((q.0 as i32 - 50).abs() <= 2);
        assert!((q.1 as i32 - 25).abs() <= 2);
        // Three-quarter should be approximately 150, 75, 30
        assert!((tq.0 as i32 - 150).abs() <= 2);
        assert!((tq.1 as i32 - 75).abs() <= 2);
    }

    // =========================================================================
    // HSV Intermediate Values (bd-17wlv)
    // =========================================================================

    #[test]
    fn hsv_partial_saturation_desaturates() {
        // At S=0.5, V=1.0, red hue should have less vivid red
        let full_sat = PlasmaPalette::hsv_to_rgb(0.0, 1.0, 1.0);
        let half_sat = PlasmaPalette::hsv_to_rgb(0.0, 0.5, 1.0);
        // Half saturation should have higher G and B than full saturation
        assert!(
            half_sat.g() > full_sat.g(),
            "Half saturation should desaturate: half.g={} full.g={}",
            half_sat.g(),
            full_sat.g()
        );
        assert!(
            half_sat.b() > full_sat.b(),
            "Half saturation should desaturate: half.b={} full.b={}",
            half_sat.b(),
            full_sat.b()
        );
        // R should stay at 255 (V=1.0)
        assert_eq!(half_sat.r(), 255);
    }

    #[test]
    fn hsv_at_exact_sextant_edges() {
        // Test at exact boundaries: 60, 120, 180, 240, 300
        // Each should produce the well-known secondary colors.
        let at_59 = PlasmaPalette::hsv_to_rgb(59.9, 1.0, 1.0);
        let at_60 = PlasmaPalette::hsv_to_rgb(60.0, 1.0, 1.0);
        // Both near yellow — should be close
        assert!(
            (at_59.r() as i32 - at_60.r() as i32).abs() <= 5,
            "Sextant edge should be continuous"
        );

        let at_179 = PlasmaPalette::hsv_to_rgb(179.9, 1.0, 1.0);
        let at_180 = PlasmaPalette::hsv_to_rgb(180.0, 1.0, 1.0);
        assert!(
            (at_179.g() as i32 - at_180.g() as i32).abs() <= 5,
            "Sextant edge 180 should be continuous"
        );
    }

    #[test]
    fn hsv_partial_value_dims_color() {
        // V=0.5 should produce half-brightness
        let bright = PlasmaPalette::hsv_to_rgb(0.0, 1.0, 1.0);
        let dim = PlasmaPalette::hsv_to_rgb(0.0, 1.0, 0.5);
        // Dim red should be approximately half the bright red
        assert!(
            dim.r() < bright.r(),
            "V=0.5 should be dimmer: dim.r={} bright.r={}",
            dim.r(),
            bright.r()
        );
        assert!((dim.r() as i32 - 127).abs() <= 2);
    }

    // =========================================================================
    // Wave Function Properties (bd-17wlv)
    // =========================================================================

    #[test]
    fn plasma_wave_differs_from_plasma_wave_low() {
        // Full and low wave functions use different numbers of components,
        // so they should generally produce different values.
        let mut any_differ = false;
        for i in 0..20 {
            let nx = i as f64 / 20.0;
            let ny = (20 - i) as f64 / 20.0;
            let full = plasma_wave(nx, ny, 1.0);
            let low = plasma_wave_low(nx, ny, 1.0);
            if (full - low).abs() > 1e-10 {
                any_differ = true;
                break;
            }
        }
        assert!(
            any_differ,
            "plasma_wave and plasma_wave_low should differ for at least some inputs"
        );
    }

    #[test]
    fn plasma_wave_at_time_zero() {
        // At time=0, the breathing envelope is 0.85 + 0.15*sin(0) = 0.85.
        // Verify this affects the output range.
        let v = plasma_wave(0.5, 0.5, 0.0);
        assert!((0.0..=1.0).contains(&v));
        // With breath=0.85, the wave is compressed: ((value * 0.85) + 1.0) / 2.0
        // Range is [((-1*0.85)+1)/2, ((1*0.85)+1)/2] = [0.075, 0.925]
        assert!(
            (0.05..=0.95).contains(&v),
            "At time=0, breath=0.85 constrains range: {v}"
        );
    }

    #[test]
    fn wave_continuity_nearby_inputs_similar() {
        // Nearby spatial inputs should produce nearby outputs (wave is smooth).
        let time = 1.0;
        let base = plasma_wave(0.5, 0.5, time);
        let nearby = plasma_wave(0.501, 0.501, time);
        let diff = (base - nearby).abs();
        // The wave function is continuous, so small input changes → small output changes.
        assert!(
            diff < 0.05,
            "Nearby inputs should produce similar values: base={base} nearby={nearby} diff={diff}"
        );
    }

    #[test]
    fn wave_varies_over_time() {
        // The wave should not be constant over time at a fixed spatial point.
        let v0 = plasma_wave(0.5, 0.5, 0.0);
        let v1 = plasma_wave(0.5, 0.5, 1.0);
        let v2 = plasma_wave(0.5, 0.5, 2.0);
        // At least one pair should differ
        assert!(
            v0 != v1 || v1 != v2,
            "Wave should vary over time: v0={v0} v1={v1} v2={v2}"
        );
    }

    // =========================================================================
    // Theme Gradient Segment Boundaries (bd-17wlv)
    // =========================================================================

    #[test]
    fn theme_gradient_exact_boundaries() {
        // At t=0.0, should be near bg_surface. At t=1.0, should be near fg_primary.
        // Allow +/- 1 for fixed-point lerp rounding at endpoints.
        let theme = ThemeInputs::default_dark();

        let at_0 = PlasmaPalette::ThemeAccents.color_at(0.0, &theme);
        assert!((at_0.r() as i32 - theme.bg_surface.r() as i32).abs() <= 1);
        assert!((at_0.g() as i32 - theme.bg_surface.g() as i32).abs() <= 1);

        let at_1 = PlasmaPalette::ThemeAccents.color_at(1.0, &theme);
        assert!((at_1.r() as i32 - theme.fg_primary.r() as i32).abs() <= 1);
        assert!((at_1.g() as i32 - theme.fg_primary.g() as i32).abs() <= 1);
    }

    #[test]
    fn theme_gradient_continuity_at_segment_edges() {
        // Values just below and above segment edges should be close.
        let theme = ThemeInputs::default_dark();
        let ta = PlasmaPalette::ThemeAccents;

        let just_below_033 = ta.color_at(0.329, &theme);
        let just_above_033 = ta.color_at(0.331, &theme);
        // Should be very close (continuity at the boundary)
        assert!(
            (just_below_033.r() as i32 - just_above_033.r() as i32).abs() <= 5,
            "Segment boundary at 0.33 should be continuous: {} vs {}",
            just_below_033.r(),
            just_above_033.r()
        );

        let just_below_066 = ta.color_at(0.659, &theme);
        let just_above_066 = ta.color_at(0.661, &theme);
        assert!(
            (just_below_066.r() as i32 - just_above_066.r() as i32).abs() <= 5,
            "Segment boundary at 0.66 should be continuous: {} vs {}",
            just_below_066.r(),
            just_above_066.r()
        );
    }

    // =========================================================================
    // Palette Midpoint Transitions (bd-17wlv)
    // =========================================================================

    #[test]
    fn cyberpunk_midpoint_is_purple() {
        let theme = ThemeInputs::default_dark();
        let mid = PlasmaPalette::Cyberpunk.color_at(0.5, &theme);
        // At t=0.5, it's the boundary between hot pink->purple and purple->cyan
        // Should be at or near the purple stop (150, 50, 200)
        assert!(
            mid.r() > 100 && mid.b() > 100,
            "Cyberpunk midpoint should be purple-ish: r={} g={} b={}",
            mid.r(),
            mid.g(),
            mid.b()
        );
    }

    #[test]
    fn sunset_midpoint_is_pinkish_orange() {
        let theme = ThemeInputs::default_dark();
        let mid = PlasmaPalette::Sunset.color_at(0.5, &theme);
        // At t=0.5, between hot pink and orange
        assert!(
            mid.r() > 200,
            "Sunset midpoint should have high R: r={}",
            mid.r()
        );
    }

    #[test]
    fn ocean_midpoint_is_cyan() {
        let theme = ThemeInputs::default_dark();
        let mid = PlasmaPalette::Ocean.color_at(0.5, &theme);
        // At t=0.5, exactly at the cyan stop (30, 180, 220)
        assert!(
            mid.g() > 100 && mid.b() > 150,
            "Ocean midpoint should be cyan: r={} g={} b={}",
            mid.r(),
            mid.g(),
            mid.b()
        );
    }

    // =========================================================================
    // Fire Palette Intermediate Segments (bd-17wlv)
    // =========================================================================

    #[test]
    fn fire_intermediate_segments_color_progression() {
        let theme = ThemeInputs::default_dark();
        let fire = PlasmaPalette::Fire;

        // t=0.1 should be in the first segment (black → dark red): low R
        let early = fire.color_at(0.1, &theme);
        assert!(
            early.r() < 80 && early.g() < 20,
            "Fire at 0.1 should be very dark: r={} g={}",
            early.r(),
            early.g()
        );

        // t=0.3 should be in second segment (dark red → orange): moderate R
        let mid_early = fire.color_at(0.3, &theme);
        assert!(
            mid_early.r() > early.r(),
            "Fire at 0.3 should be brighter than 0.1: {} vs {}",
            mid_early.r(),
            early.r()
        );

        // t=0.5 should be in third segment (orange → yellow-ish): higher R, some G
        let mid = fire.color_at(0.5, &theme);
        assert!(
            mid.r() > 150,
            "Fire at 0.5 should have strong R: {}",
            mid.r()
        );

        // t=0.7 should be in fourth segment: high R, notable G
        let mid_late = fire.color_at(0.7, &theme);
        assert!(
            mid_late.g() > mid.g(),
            "Fire at 0.7 should have more G: {} vs {}",
            mid_late.g(),
            mid.g()
        );
    }

    // =========================================================================
    // Galaxy 4th Segment (bd-17wlv)
    // =========================================================================

    #[test]
    fn galaxy_fourth_segment_bright_stars() {
        // Galaxy segment 4 is t=0.85..1.0: light lavender → warm white
        let theme = ThemeInputs::default_dark();
        let galaxy = PlasmaPalette::Galaxy;

        let at_085 = galaxy.color_at(0.85, &theme);
        let at_092 = galaxy.color_at(0.92, &theme);
        let at_100 = galaxy.color_at(1.0, &theme);

        // All should be bright
        let lum_085 = at_085.r() as u32 + at_085.g() as u32 + at_085.b() as u32;
        let lum_100 = at_100.r() as u32 + at_100.g() as u32 + at_100.b() as u32;
        assert!(lum_085 > 400, "Galaxy at 0.85 should be bright: {lum_085}");
        assert!(lum_100 > lum_085, "Galaxy should get brighter toward 1.0");

        // Intermediate should be between endpoints
        assert!(
            at_092.r() >= at_085.r() && at_092.r() <= at_100.r(),
            "Galaxy 4th segment R should be monotonic"
        );
    }

    // =========================================================================
    // Render Edge Cases (bd-17wlv)
    // =========================================================================

    #[test]
    fn render_single_column() {
        // Width=1 should render without panic
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::new(PlasmaPalette::Ocean);
        let ctx = FxContext {
            width: 1,
            height: 10,
            frame: 0,
            time_seconds: 1.0,
            quality: FxQuality::Full,
            theme: &theme,
        };
        let mut out = vec![PackedRgba::TRANSPARENT; ctx.len()];
        fx.render(ctx, &mut out);
        let filled = out
            .iter()
            .filter(|c| **c != PackedRgba::TRANSPARENT)
            .count();
        assert!(filled > 0, "Single column should produce output");
    }

    #[test]
    fn render_single_row() {
        // Height=1 should render without panic
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::new(PlasmaPalette::Fire);
        let ctx = FxContext {
            width: 20,
            height: 1,
            frame: 0,
            time_seconds: 1.0,
            quality: FxQuality::Full,
            theme: &theme,
        };
        let mut out = vec![PackedRgba::TRANSPARENT; ctx.len()];
        fx.render(ctx, &mut out);
        let filled = out
            .iter()
            .filter(|c| **c != PackedRgba::TRANSPARENT)
            .count();
        assert!(filled > 0, "Single row should produce output");
    }

    #[test]
    fn render_large_dimensions() {
        // Stress test: 100x60 should work without panic or excessive memory
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::new(PlasmaPalette::Sunset);
        let ctx = FxContext {
            width: 100,
            height: 60,
            frame: 0,
            time_seconds: 1.5,
            quality: FxQuality::Full,
            theme: &theme,
        };
        let mut out = vec![PackedRgba::TRANSPARENT; ctx.len()];
        fx.render(ctx, &mut out);
        let filled = out
            .iter()
            .filter(|c| **c != PackedRgba::TRANSPARENT)
            .count();
        assert_eq!(filled, 6000, "All 6000 pixels should be filled");
    }

    #[test]
    fn render_at_time_zero() {
        // Time=0 is a common initial state; verify it works for all quality tiers
        let theme = ThemeInputs::default_dark();
        for quality in [FxQuality::Full, FxQuality::Reduced, FxQuality::Minimal] {
            let mut fx = PlasmaFx::new(PlasmaPalette::ThemeAccents);
            let ctx = FxContext {
                width: 8,
                height: 8,
                frame: 0,
                time_seconds: 0.0,
                quality,
                theme: &theme,
            };
            let mut out = vec![PackedRgba::TRANSPARENT; 64];
            fx.render(ctx, &mut out);
            let filled = out
                .iter()
                .filter(|c| **c != PackedRgba::TRANSPARENT)
                .count();
            assert!(filled > 0, "{:?} at time=0 should produce output", quality);
        }
    }

    // =========================================================================
    // Palette Switching (bd-17wlv)
    // =========================================================================

    #[test]
    fn set_palette_changes_render_output() {
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::new(PlasmaPalette::Sunset);
        let ctx = FxContext {
            width: 8,
            height: 8,
            frame: 0,
            time_seconds: 1.0,
            quality: FxQuality::Full,
            theme: &theme,
        };

        let mut out_sunset = vec![PackedRgba::TRANSPARENT; 64];
        fx.render(ctx, &mut out_sunset);

        fx.set_palette(PlasmaPalette::Ocean);
        let mut out_ocean = vec![PackedRgba::TRANSPARENT; 64];
        fx.render(ctx, &mut out_ocean);

        assert_ne!(
            out_sunset, out_ocean,
            "Different palettes should produce different output"
        );
    }

    // =========================================================================
    // Multiple Successive Resizes (bd-17wlv)
    // =========================================================================

    #[test]
    fn scratch_handles_multiple_resizes() {
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::new(PlasmaPalette::Sunset);

        // Cycle through multiple sizes
        let sizes = [(4, 4), (8, 6), (3, 10), (16, 2), (4, 4)];
        for &(w, h) in &sizes {
            let ctx = FxContext {
                width: w,
                height: h,
                frame: 0,
                time_seconds: 1.0,
                quality: FxQuality::Full,
                theme: &theme,
            };
            let mut out = vec![PackedRgba::TRANSPARENT; ctx.len()];
            fx.render(ctx, &mut out);
            let filled = out
                .iter()
                .filter(|c| **c != PackedRgba::TRANSPARENT)
                .count();
            assert_eq!(
                filled,
                (w * h) as usize,
                "All pixels should be filled at {w}x{h}"
            );
        }
    }

    #[test]
    fn scratch_resize_preserves_determinism() {
        // After resizing, re-rendering at original size should still match.
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::new(PlasmaPalette::Neon);

        let ctx_orig = FxContext {
            width: 6,
            height: 4,
            frame: 0,
            time_seconds: 2.0,
            quality: FxQuality::Full,
            theme: &theme,
        };
        let mut out_orig = vec![PackedRgba::TRANSPARENT; ctx_orig.len()];
        fx.render(ctx_orig, &mut out_orig);

        // Resize to different size
        let ctx_big = FxContext {
            width: 20,
            height: 15,
            frame: 0,
            time_seconds: 2.0,
            quality: FxQuality::Full,
            theme: &theme,
        };
        let mut out_big = vec![PackedRgba::TRANSPARENT; ctx_big.len()];
        fx.render(ctx_big, &mut out_big);

        // Back to original
        let mut out_again = vec![PackedRgba::TRANSPARENT; ctx_orig.len()];
        fx.render(ctx_orig, &mut out_again);
        assert_eq!(
            out_orig, out_again,
            "Returning to original size should produce same output"
        );
    }

    // =========================================================================
    // Individual Fixed Palette Render (bd-17wlv)
    // =========================================================================

    #[test]
    fn each_fixed_palette_produces_distinct_output() {
        // Each fixed palette should produce visually distinct output.
        let theme = ThemeInputs::default_dark();
        let fixed_palettes = [
            PlasmaPalette::Sunset,
            PlasmaPalette::Ocean,
            PlasmaPalette::Fire,
            PlasmaPalette::Neon,
            PlasmaPalette::Cyberpunk,
            PlasmaPalette::Galaxy,
        ];

        let mut outputs = Vec::new();
        for palette in &fixed_palettes {
            let mut fx = PlasmaFx::new(*palette);
            let ctx = FxContext {
                width: 8,
                height: 8,
                frame: 0,
                time_seconds: 1.0,
                quality: FxQuality::Full,
                theme: &theme,
            };
            let mut out = vec![PackedRgba::TRANSPARENT; 64];
            fx.render(ctx, &mut out);
            outputs.push(out);
        }

        // Every pair should differ
        for i in 0..outputs.len() {
            for j in (i + 1)..outputs.len() {
                assert_ne!(
                    outputs[i], outputs[j],
                    "{:?} and {:?} should produce different output",
                    fixed_palettes[i], fixed_palettes[j]
                );
            }
        }
    }

    #[test]
    fn each_theme_palette_produces_distinct_output() {
        // Each theme-derived palette should produce distinct output.
        let theme = ThemeInputs::default_dark();
        let theme_palettes = [
            PlasmaPalette::ThemeAccents,
            PlasmaPalette::Aurora,
            PlasmaPalette::Ember,
            PlasmaPalette::Subtle,
            PlasmaPalette::Monochrome,
        ];

        let mut outputs = Vec::new();
        for palette in &theme_palettes {
            let mut fx = PlasmaFx::new(*palette);
            let ctx = FxContext {
                width: 8,
                height: 8,
                frame: 0,
                time_seconds: 1.0,
                quality: FxQuality::Full,
                theme: &theme,
            };
            let mut out = vec![PackedRgba::TRANSPARENT; 64];
            fx.render(ctx, &mut out);
            outputs.push(out);
        }

        for i in 0..outputs.len() {
            for j in (i + 1)..outputs.len() {
                assert_ne!(
                    outputs[i], outputs[j],
                    "{:?} and {:?} should produce different output",
                    theme_palettes[i], theme_palettes[j]
                );
            }
        }
    }

    // =========================================================================
    // Monochrome Gradient Linearity (bd-17wlv)
    // =========================================================================

    #[test]
    fn monochrome_midpoint_is_average() {
        let theme = ThemeInputs::default_dark();
        let mid = PlasmaPalette::Monochrome.color_at(0.5, &theme);
        let expected_r = (theme.bg_base.r() as i32 + theme.fg_primary.r() as i32) / 2;
        let expected_g = (theme.bg_base.g() as i32 + theme.fg_primary.g() as i32) / 2;
        // Allow +/- 1 for fixed-point rounding
        assert!(
            (mid.r() as i32 - expected_r).abs() <= 1,
            "Monochrome midpoint R: got {} expected ~{}",
            mid.r(),
            expected_r
        );
        assert!(
            (mid.g() as i32 - expected_g).abs() <= 1,
            "Monochrome midpoint G: got {} expected ~{}",
            mid.g(),
            expected_g
        );
    }

    #[test]
    fn monochrome_is_linear_blend_of_bg_and_fg() {
        // Monochrome linearly blends bg_base → fg_primary.
        // Verify the blend is monotonic across all channels.
        let theme = ThemeInputs::default_dark();
        let at_0 = PlasmaPalette::Monochrome.color_at(0.0, &theme);
        let at_05 = PlasmaPalette::Monochrome.color_at(0.5, &theme);
        let at_1 = PlasmaPalette::Monochrome.color_at(1.0, &theme);

        // Midpoint should be between endpoints (or equal) for each channel.
        for (ch_name, lo, mid, hi) in [
            ("R", at_0.r(), at_05.r(), at_1.r()),
            ("G", at_0.g(), at_05.g(), at_1.g()),
            ("B", at_0.b(), at_05.b(), at_1.b()),
        ] {
            let (min_ep, max_ep) = (lo.min(hi), lo.max(hi));
            // Allow +/- 1 for rounding
            assert!(
                (mid as i32) >= (min_ep as i32 - 1) && (mid as i32) <= (max_ep as i32 + 1),
                "Monochrome midpoint {ch_name}={mid} should be between {min_ep} and {max_ep}"
            );
        }
    }

    // =========================================================================
    // Subtle Palette with Different Themes (bd-17wlv)
    // =========================================================================

    #[test]
    fn subtle_output_changes_with_theme() {
        let dark_theme = ThemeInputs::default_dark();
        let dark_mid = PlasmaPalette::Subtle.color_at(0.5, &dark_theme);

        // Create a lighter theme by modifying bg colors
        let mut light_theme = ThemeInputs::default_dark();
        light_theme.bg_base = PackedRgba::rgb(240, 240, 240);
        light_theme.bg_surface = PackedRgba::rgb(250, 250, 250);
        light_theme.bg_overlay = PackedRgba::rgb(230, 230, 235);
        let light_mid = PlasmaPalette::Subtle.color_at(0.5, &light_theme);

        assert_ne!(dark_mid, light_mid, "Subtle palette should adapt to theme");
        // Light theme version should be brighter
        assert!(
            light_mid.r() > dark_mid.r(),
            "Light subtle should be brighter: light.r={} dark.r={}",
            light_mid.r(),
            dark_mid.r()
        );
    }

    // =========================================================================
    // Neon Palette Intermediate Values (bd-17wlv)
    // =========================================================================

    #[test]
    fn neon_at_intermediate_hues() {
        let theme = ThemeInputs::default_dark();
        let neon = PlasmaPalette::Neon;

        // t=1/6 ≈ yellow (hue=60)
        let yellow = neon.color_at(1.0 / 6.0, &theme);
        assert!(
            yellow.r() > 200 && yellow.g() > 200 && yellow.b() < 50,
            "Neon at t=1/6 should be yellow: r={} g={} b={}",
            yellow.r(),
            yellow.g(),
            yellow.b()
        );

        // t=1/2 ≈ cyan (hue=180)
        let cyan = neon.color_at(0.5, &theme);
        assert!(
            cyan.r() < 30 && cyan.g() > 200 && cyan.b() > 200,
            "Neon at t=0.5 should be cyan: r={} g={} b={}",
            cyan.r(),
            cyan.g(),
            cyan.b()
        );

        // t=5/6 ≈ magenta (hue=300)
        let magenta = neon.color_at(5.0 / 6.0, &theme);
        assert!(
            magenta.r() > 200 && magenta.g() < 30 && magenta.b() > 200,
            "Neon at t=5/6 should be magenta: r={} g={} b={}",
            magenta.r(),
            magenta.g(),
            magenta.b()
        );
    }

    // =========================================================================
    // Reduced Quality Render Paths (bd-17wlv)
    // =========================================================================

    #[test]
    fn reduced_quality_renders_each_palette() {
        // Reduced quality has a distinct render path; verify it works for all palettes.
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
            let mut fx = PlasmaFx::new(palette);
            let ctx = FxContext {
                width: 6,
                height: 4,
                frame: 0,
                time_seconds: 1.5,
                quality: FxQuality::Reduced,
                theme: &theme,
            };
            let mut out = vec![PackedRgba::TRANSPARENT; ctx.len()];
            fx.render(ctx, &mut out);
            let filled = out
                .iter()
                .filter(|c| **c != PackedRgba::TRANSPARENT)
                .count();
            assert!(
                filled > 0,
                "{:?} Reduced quality should produce output",
                palette
            );
        }
    }

    #[test]
    fn minimal_quality_renders_each_palette() {
        // Minimal quality has a distinct render path; verify it works for all palettes.
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
            let mut fx = PlasmaFx::new(palette);
            let ctx = FxContext {
                width: 6,
                height: 4,
                frame: 0,
                time_seconds: 1.5,
                quality: FxQuality::Minimal,
                theme: &theme,
            };
            let mut out = vec![PackedRgba::TRANSPARENT; ctx.len()];
            fx.render(ctx, &mut out);
            let filled = out
                .iter()
                .filter(|c| **c != PackedRgba::TRANSPARENT)
                .count();
            assert!(
                filled > 0,
                "{:?} Minimal quality should produce output",
                palette
            );
        }
    }

    // =========================================================================
    // Aurora Fallback Path Completeness (bd-17wlv)
    // =========================================================================

    #[test]
    fn aurora_fallback_produces_cool_gradient() {
        // With transparent accent_slots, Aurora should fall back to blue/cyan.
        let mut theme = ThemeInputs::default_dark();
        theme.accent_slots[0] = PackedRgba::TRANSPARENT;
        theme.accent_slots[1] = PackedRgba::TRANSPARENT;

        // Sample across the full range
        let mut seen_blue_bias = false;
        for i in 1..10 {
            let t = i as f64 / 10.0;
            let c = PlasmaPalette::Aurora.color_at(t, &theme);
            if c.b() > c.r() {
                seen_blue_bias = true;
            }
        }
        assert!(
            seen_blue_bias,
            "Aurora fallback should have at least some cool/blue tones"
        );
    }

    // =========================================================================
    // PlasmaFx Eq and Hash Derive (bd-17wlv)
    // =========================================================================

    #[test]
    fn palette_eq_and_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(PlasmaPalette::Sunset);
        set.insert(PlasmaPalette::Ocean);
        set.insert(PlasmaPalette::Sunset); // duplicate
        assert_eq!(set.len(), 2, "HashSet should deduplicate palettes");
    }

    #[test]
    fn palette_copy() {
        let p = PlasmaPalette::Galaxy;
        let q = p; // Copy
        assert_eq!(p, q);
    }

    // =========================================================================
    // Negative Time Values (bd-17wlv)
    // =========================================================================

    #[test]
    fn plasma_wave_negative_time() {
        // Negative time is valid (e.g., rewinding); wave should still be in [0,1].
        for &t in &[-1.0, -10.0, -100.0, -1e4] {
            let v = plasma_wave(0.5, 0.5, t);
            assert!(
                (0.0..=1.0).contains(&v),
                "plasma_wave at time={t} should be in [0,1], got {v}"
            );
            let vl = plasma_wave_low(0.5, 0.5, t);
            assert!(
                (0.0..=1.0).contains(&vl),
                "plasma_wave_low at time={t} should be in [0,1], got {vl}"
            );
        }
    }

    #[test]
    fn render_negative_time_produces_output() {
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::new(PlasmaPalette::Ocean);
        let ctx = FxContext {
            width: 8,
            height: 8,
            frame: 0,
            time_seconds: -5.0,
            quality: FxQuality::Full,
            theme: &theme,
        };
        let mut out = vec![PackedRgba::TRANSPARENT; 64];
        fx.render(ctx, &mut out);
        let filled = out
            .iter()
            .filter(|c| **c != PackedRgba::TRANSPARENT)
            .count();
        assert!(filled > 0, "Negative time should still produce output");
    }

    // =========================================================================
    // NaN / Infinity Edge Cases (bd-17wlv)
    // =========================================================================

    #[test]
    fn render_nan_time_does_not_panic() {
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::new(PlasmaPalette::Sunset);
        let ctx = FxContext {
            width: 4,
            height: 4,
            frame: 0,
            time_seconds: f64::NAN,
            quality: FxQuality::Full,
            theme: &theme,
        };
        let mut out = vec![PackedRgba::TRANSPARENT; 16];
        // Should not panic. Output may be garbage but must not crash.
        fx.render(ctx, &mut out);
    }

    #[test]
    fn render_infinity_time_does_not_panic() {
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::new(PlasmaPalette::Fire);
        for &t in &[f64::INFINITY, f64::NEG_INFINITY] {
            let ctx = FxContext {
                width: 4,
                height: 4,
                frame: 0,
                time_seconds: t,
                quality: FxQuality::Full,
                theme: &theme,
            };
            let mut out = vec![PackedRgba::TRANSPARENT; 16];
            fx.render(ctx, &mut out);
        }
    }

    // =========================================================================
    // render_with_palette Early Returns (bd-17wlv)
    // =========================================================================

    #[test]
    fn render_quality_off_leaves_output_untouched() {
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::new(PlasmaPalette::Ocean);
        let sentinel = PackedRgba::rgb(42, 42, 42);
        let ctx = FxContext {
            width: 4,
            height: 4,
            frame: 0,
            time_seconds: 1.0,
            quality: FxQuality::Off,
            theme: &theme,
        };
        let mut out = vec![sentinel; 16];
        fx.render(ctx, &mut out);
        // Output should be completely untouched.
        assert!(
            out.iter().all(|c| *c == sentinel),
            "FxQuality::Off should not modify the output buffer"
        );
    }

    #[test]
    fn render_mismatched_buffer_leaves_output_untouched() {
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::new(PlasmaPalette::Sunset);
        let sentinel = PackedRgba::rgb(99, 99, 99);
        let ctx = FxContext {
            width: 4,
            height: 4,
            frame: 0,
            time_seconds: 1.0,
            quality: FxQuality::Full,
            theme: &theme,
        };
        // Buffer is 10 elements but ctx expects 16 — should early-return.
        let mut out = vec![sentinel; 10];
        fx.render(ctx, &mut out);
        assert!(
            out.iter().all(|c| *c == sentinel),
            "Mismatched buffer should not be modified"
        );
    }

    // =========================================================================
    // BackdropFx::render() Consistency with color_at() (bd-17wlv)
    // =========================================================================

    #[test]
    fn backdrop_render_matches_color_at_for_all_palettes() {
        // The inlined palette closures in BackdropFx::render() must produce
        // the same colors as PlasmaPalette::color_at() for the same wave values.
        let theme = ThemeInputs::default_dark();
        let all_palettes = [
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
        ];

        let w: u16 = 10;
        let h: u16 = 6;
        let time = 1.5;

        for palette in all_palettes {
            let mut fx = PlasmaFx::new(palette);
            let ctx = FxContext {
                width: w,
                height: h,
                frame: 0,
                time_seconds: time,
                quality: FxQuality::Full,
                theme: &theme,
            };
            let mut out = vec![PackedRgba::TRANSPARENT; ctx.len()];
            fx.render(ctx, &mut out);

            // Verify every pixel matches the reference path.
            for dy in 0..h {
                for dx in 0..w {
                    let idx = dy as usize * w as usize + dx as usize;
                    let nx = dx as f64 / w as f64;
                    let ny = dy as f64 / h as f64;
                    let wave = plasma_wave(nx, ny, time).clamp(0.0, 1.0);
                    let expected = palette.color_at(wave, &theme);
                    assert_eq!(
                        out[idx], expected,
                        "{:?} mismatch at ({dx},{dy}): wave={wave:.4}",
                        palette
                    );
                }
            }
        }
    }

    // =========================================================================
    // Ember Fallback Path (bd-17wlv)
    // =========================================================================

    #[test]
    fn ember_fallback_produces_warm_gradient() {
        // With transparent accent_slots[2..4], Ember should fall back to red/orange.
        let mut theme = ThemeInputs::default_dark();
        theme.accent_slots[2] = PackedRgba::TRANSPARENT;
        theme.accent_slots[3] = PackedRgba::TRANSPARENT;

        let mut seen_red_bias = false;
        for i in 1..10 {
            let t = i as f64 / 10.0;
            let c = PlasmaPalette::Ember.color_at(t, &theme);
            if c.r() > c.b() {
                seen_red_bias = true;
            }
        }
        assert!(
            seen_red_bias,
            "Ember fallback should have at least some warm/red tones"
        );
    }

    // =========================================================================
    // color_at Clamping (bd-17wlv)
    // =========================================================================

    #[test]
    fn color_at_clamps_out_of_range_t() {
        let theme = ThemeInputs::default_dark();
        // Negative and >1.0 should be clamped to endpoints.
        for palette in [
            PlasmaPalette::Sunset,
            PlasmaPalette::Ocean,
            PlasmaPalette::Fire,
            PlasmaPalette::Neon,
            PlasmaPalette::ThemeAccents,
        ] {
            let at_neg = palette.color_at(-1.0, &theme);
            let at_0 = palette.color_at(0.0, &theme);
            assert_eq!(
                at_neg, at_0,
                "{:?}: color_at(-1.0) should equal color_at(0.0)",
                palette
            );

            let at_2 = palette.color_at(2.0, &theme);
            let at_1 = palette.color_at(1.0, &theme);
            assert_eq!(
                at_2, at_1,
                "{:?}: color_at(2.0) should equal color_at(1.0)",
                palette
            );
        }
    }

    // =========================================================================
    // Scratch Capacity Stability (bd-17wlv)
    // =========================================================================

    #[test]
    fn scratch_capacity_stable_across_same_size_renders() {
        let theme = ThemeInputs::default_dark();
        let mut fx = PlasmaFx::new(PlasmaPalette::Galaxy);
        let mut out = vec![PackedRgba::TRANSPARENT; 48];

        // Warm up.
        let ctx = FxContext {
            width: 8,
            height: 6,
            frame: 0,
            time_seconds: 0.0,
            quality: FxQuality::Full,
            theme: &theme,
        };
        fx.render(ctx, &mut out);

        // Clone to capture scratch state after warm-up.
        let fx_snapshot = fx.clone();

        // Render 10 more times at same size.
        for i in 1..=10 {
            let ctx = FxContext {
                width: 8,
                height: 6,
                frame: i,
                time_seconds: i as f64 * 0.05,
                quality: FxQuality::Full,
                theme: &theme,
            };
            fx.render(ctx, &mut out);
        }

        // Scratch dimensions should match.
        assert_eq!(fx.scratch.width, fx_snapshot.scratch.width);
        assert_eq!(fx.scratch.height, fx_snapshot.scratch.height);
        // Vec lengths should be identical (no growth).
        assert_eq!(
            fx.scratch.x_v1_sin.len(),
            fx_snapshot.scratch.x_v1_sin.len()
        );
        assert_eq!(
            fx.scratch.radial_center_sin.len(),
            fx_snapshot.scratch.radial_center_sin.len()
        );
    }

    // =========================================================================
    // Reduced Quality Uses Fewer Components (bd-17wlv)
    // =========================================================================

    #[test]
    fn reduced_quality_differs_from_full() {
        // Reduced uses 4 components (v1+v2+v3+v6), Full uses 6.
        // At non-trivial sizes, the outputs should differ.
        let theme = ThemeInputs::default_dark();

        let run = |quality: FxQuality| -> Vec<PackedRgba> {
            let mut fx = PlasmaFx::new(PlasmaPalette::Sunset);
            let ctx = FxContext {
                width: 8,
                height: 8,
                frame: 0,
                time_seconds: 2.0,
                quality,
                theme: &theme,
            };
            let mut out = vec![PackedRgba::TRANSPARENT; 64];
            fx.render(ctx, &mut out);
            out
        };

        let full = run(FxQuality::Full);
        let reduced = run(FxQuality::Reduced);
        let minimal = run(FxQuality::Minimal);

        assert_ne!(full, reduced, "Full and Reduced should differ");
        assert_ne!(full, minimal, "Full and Minimal should differ");
        assert_ne!(reduced, minimal, "Reduced and Minimal should differ");
    }
}
